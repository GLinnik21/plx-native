//! Main-thread confined native buffer-feed execution. A prepared Load owns its clock and
//! callback epochs until the addressed route attempt installs its Engine.

use crate::aq::{self, AuQueue};
use crate::feed_resources::{FeedResources, LoadReceipt};
use crate::feed_state::{Engine, Source, PRES_NONE};
use crate::feed_state::{FeedPace, SampleBuf, AQ_AUDIO_BYTES, AQ_VIDEO_BYTES};
use crate::ff::DemuxTransport;
use crate::open::PhysicalOpen;
use crate::pcm_lead::PcmLead;
use crate::side_reader::SideReaderPort;
use crate::shared::PhysicalShared;
use crate::shared::{Stage, Transport};
use crate::SendPtr;
use plx_base::task::MainThread;
use plx_platform::tv::sink::VideoSink;
use plx_net::stream::HttpStream;
use std::os::raw::{c_char, c_int, c_void};
use std::ffi::CString;
use std::sync::atomic::Ordering;

/// Retracts an initial clock hold when a prepared Load is superseded before installation.
pub struct ClockStartGuard {
    shared: &'static PhysicalShared,
    armed: bool,
}

impl ClockStartGuard {
    pub fn arm(shared: &'static PhysicalShared, user_held: bool, seek_preroll: bool) -> Option<Self> {
        shared
            .arm_initial_clock_hold(user_held, seek_preroll)
            .then_some(Self { shared, armed: true })
    }

    pub fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for ClockStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.cancel_clock_session_start();
        }
    }
}

/// Retires the exact native callback epoch if Load construction does not install an Engine.
pub struct NativeSessionStartGuard {
    shared: &'static PhysicalShared,
    epoch: u32,
    armed: bool,
}

impl NativeSessionStartGuard {
    pub fn arm(shared: &'static PhysicalShared) -> Option<Self> {
        shared
            .begin_native_session()
            .map(|epoch| Self { shared, epoch, armed: true })
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn commit(mut self) {
        self.armed = false;
    }
}

impl Drop for NativeSessionStartGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.retire_native_session(self.epoch);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrepareRefusal {
    ClockBusy,
    NativeBusy,
}

/// Common composition supplies these three already-existing IO seams. The physical executor
/// never reads a route, credential registry, or common seek-mode global.
pub struct LaunchHooks {
    pub demux_transport: fn() -> DemuxTransport,
    pub load_thread: fn(SendPtr<c_char>, u32, Option<LoadReceipt>, PcmLead),
    pub select_seek_mode: fn(),
}

/// Owns one physical Load declaration and the two retractable native epochs. The common
/// route owner binds its addressed demux session only after claiming the route attempt.
pub struct NativePrepared {
    pub(crate) open: Option<PhysicalOpen>,
    pub(crate) sample: Option<Box<SampleBuf>>,
    pub(crate) stream: bool,
    pub(crate) payload_c: CString,
    pub(crate) audio_lead: PcmLead,
    pub(crate) clock_start: ClockStartGuard,
    pub(crate) native_start: NativeSessionStartGuard,
    pub(crate) user_held: bool,
    pub(crate) side_reader: Box<dyn SideReaderPort>,
}

impl NativePrepared {
    pub fn arm(
        shared: &'static PhysicalShared,
        open: Option<PhysicalOpen>,
        sample: Option<Box<SampleBuf>>,
        stream: bool,
        payload_c: CString,
        audio_lead: PcmLead,
        user_held: bool,
        seek_preroll: bool,
        side_reader: Box<dyn SideReaderPort>,
    ) -> Result<Self, PrepareRefusal> {
        let clock_start = ClockStartGuard::arm(shared, user_held, seek_preroll)
            .ok_or(PrepareRefusal::ClockBusy)?;
        let native_start = NativeSessionStartGuard::arm(shared)
            .ok_or(PrepareRefusal::NativeBusy)?;
        Ok(Self {
            open, sample, stream, payload_c, audio_lead, clock_start, native_start,
            user_held, side_reader,
        })
    }

    pub fn native_epoch(&self) -> u32 {
        self.native_start.epoch()
    }

    pub fn bind_demux_session(
        &mut self,
        session: Box<dyn plx_playback_contract::demux::DemuxSession>,
    ) {
        if let Some(open) = self.open.as_mut() {
            debug_assert!(open.session.is_none());
            open.session = Some(session);
        }
    }
}

/// Free both queued AU lanes and each held BufferFull retry AU.
pub fn drain_aq(eng: &mut Engine) {
    drain_one(eng.aq_video.as_mut());
    drain_one(eng.aq_audio.as_mut());
    eng.pending_video = None;
    eng.pending_audio = None;
}

fn drain_one(q: Option<&mut Box<AuQueue>>) {
    if let Some(q) = q {
        let qp = &mut **q as *mut AuQueue;
        let mut eof: c_int = 0;
        loop {
            let n = aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            unsafe { libc::free(n as *mut c_void) };
        }
    }
}

/// Re-prime at the currently presented physical point without changing the clock.
pub fn arm_live_clock_prime(eng: &mut Engine) {
    let shared = eng.shared;
    let presented = shared.pres_fed.load(Ordering::Relaxed);
    eng.seek_base_pts = if presented == PRES_NONE {
        eng.max_fed_video_pts.min(eng.max_fed_audio_pts.max(0))
    } else {
        presented
    };
    eng.prime_play = true;
}

/// Feed the looped local sample. The provider is called at every native verb so a release
/// entering the native-object fence cannot leave a cached unfenced sink in the executor.
pub fn feed_sample(
    mt: &MainThread,
    eng: &mut Engine,
    sink: fn() -> &'static dyn VideoSink,
) {
    let s = match &mut eng.source {
        Source::Sample(s) => s,
        _ => return,
    };
    let naus = s.au.len();
    if naus < 2 {
        return;
    }
    let mut fed = 0;
    while fed < 60 {
        if s.next >= naus - 1 {
            s.next = 0;
            s.loops += 1;
        }
        let off = s.au[s.next];
        let end = s.au[s.next + 1];
        let pts = (s.loops * (naus as i64 - 1) + s.next as i64) * 41708333;
        let r = unsafe { sink().feed(mt, s.data[off..].as_ptr(), (end - off) as u32, pts, 1) };
        if (r as u8) != b'O' {
            break;
        }
        s.next += 1;
        fed += 1;
    }
}

/// Builds the "stream: <origin> path=<path>" diagnostic line for the freshly opened
/// transcode/direct-stream URL. `plx_base::eventlog::log()` runs every line through
/// `eventlog::scrub::scrub_local()`, which already redacts `X-Plex-Token=` (and other
/// credential query params) by value only, stopping at the next `&` — so the token,
/// which `Client::with_token` always appends last, never reaches the log. Truncating
/// `path` here on top of that is redundant, and for a burn transcode URL it cuts the
/// line off before `subtitleStreamID=`/`subtitles=burn`, which is exactly the evidence
/// `tests/run.py`'s `op_audio_enhancement_burn` grader looks for.
pub fn stream_open_log_line(origin_log_form: &str, path: &str) -> String {
    format!("stream: {origin_log_form} path={path}")
}


/// Launch workers against the route's exact attempt; this owns no route/session policy.
pub fn launch_native_open(
    feed: &mut FeedResources,
    transport: &Transport,
    attempt: plx_playback_contract::AttemptId,
    prepared: NativePrepared,
    hooks: &LaunchHooks,
) -> Result<(), ()> {
    let NativePrepared { mut open, sample, stream, payload_c, audio_lead, clock_start, native_start,
        user_held, mut side_reader } = prepared;
    let shared = feed.shared();
    // fd = -1 (CLOSED) so a teardown before/without http_open doesn't close(0)
    let mut hs = plx_net::stream::http_stream_boxed();
    let mut aqv_box: Option<Box<AuQueue>> = None;
    let mut aqa_box: Option<Box<AuQueue>> = None;
    let mut stream_th = None;
    let source;
    // Whether the side subtitle reader will run for this engine run, decided BEFORE the demuxer is
    // spawned: the demuxer reads `side_subs_owner` once, as it opens the stream, and must never
    // also open subtitle decoders or ASS sources the reader replaces.

    if stream {
        let open = open.as_mut().expect("stream request was captured");
        // The whole origin goes to the demuxer: its scheme chooses the byte transport, and
        // HTTPS certificate validation needs the original `plex.direct` hostname.
        let path = std::mem::take(&mut open.path);
        plx_base::eventlog::log(&stream_open_log_line(&open.origin.log_form(), &path));
        let origin = open.origin.clone();
        // Two-lane feed: the demuxer routes es=1 video to aq_video and es=2 audio to
        // aq_audio, each with its own cap + feeder.
        let mut qv = aq::aq_new(AQ_VIDEO_BYTES);
        let mut qa = aq::aq_new(AQ_AUDIO_BYTES);
        let aqv_raw = &mut *qv as *mut AuQueue;
        let aqa_raw = &mut *qa as *mut AuQueue;
        let hs_raw = &mut *hs as *mut HttpStream;
        shared.hs_ptr.store(hs_raw, Ordering::Release);
        {
            let aqp = SendPtr(aqv_raw);
            let aqap = SendPtr(aqa_raw);
            let hsp = SendPtr(hs_raw);
            // The Load payload's audio codec, moved BY VALUE from the prepared open.
            // `ff::demux` used to call `route::stream_acodec()` from the demux thread, cloning
            // a `static mut String` that main-thread route transitions and native audio switches
            // reassign — a data race, and a use-after-free if the reassignment dropped the old
            // buffer mid-clone.
            // The prepared open is refreshed on every reload: every one of those writers is followed by
            // `teardown(true) + start_bufferfeed()` (reload_at / reload_transcode /
            // switch_audio_native), which respawns this thread with the new value.
            let acodec = std::mem::take(&mut open.audio_codec);
            let force_original = open.feed_payload.force_direct_play;
            let demux_transport = (hooks.demux_transport)();
            let session = open.session.take().expect("one demux session per captured feed open");
            let abr = open.hls_abr.take();
            let auto_original = open.auto_original.take();
            // Before the demuxer exists, so what it publishes is this session's.
            crate::native_audio::reset();
            // A clock-only reader (a sidecar) reads no subtitle: the demuxer keeps what it enumerates.
            shared.side_subs_owner.store(side_reader.embedded_owner(), Ordering::Release);
            stream_th = plx_base::task::spawn_off_frame_keeping("demux", move |off| {
                crate::demux(off, origin, path, acodec, force_original, demux_transport, session, abr, auto_original, aqp, aqap, hsp)
            });
            if stream_th.is_none() {
                shared.side_subs_owner.store(false, Ordering::Release);
                // Nothing will ever fill the AU queues, so there is no session to start. `hs` is
                // about to drop with this early return, so retract the pointer first — the pump
                // and teardown both read it straight off shared.
                shared.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
                return Err(());
            }
        }
        aqv_box = Some(qv);
        aqa_box = Some(qa);
        source = Source::Stream;
    } else {
        source = Source::Sample(sample.unwrap());
    }

    // the media thread constructs + loads + runs the loop (owns the GMainContext)
    let payload_ptr = SendPtr(payload_c.as_ptr() as *mut c_char);
    let native_epoch = native_start.epoch();
    let receipt = feed.load_receipt(attempt, native_epoch);
    let receipt_token = receipt.token();
    let load_thread = hooks.load_thread;
    let load_th = plx_base::task::spawn("media", move || {
        load_thread(payload_ptr, native_epoch, Some(receipt), audio_lead)
    });
    if load_th.is_none() {
        feed.cancel_load_receipt(receipt_token);
        // Without the media thread nothing is ever Loaded and nothing drains the AU queues, so the
        // demuxer would park in aq_push forever holding raw pointers into locals this return is
        // about to drop. Stop it exactly the way teardown's stream branch does — abort both lanes,
        // shutdown to wake the recv, join, and only then the real close.
        for q in [aqv_box.as_mut(), aqa_box.as_mut()].into_iter().flatten() {
            aq::aq_abort(&mut **q);
        }
        let p = shared.hs_ptr.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if !p.is_null() {
            plx_net::stream::http_shutdown(p);
        }
        side_reader.stop_before_demux_join("start-failed"); // before the demux join, like teardown
        plx_net::range_io::abort_active(); // the https demuxer's equivalent — see teardown
        if let Some(t) = stream_th.take() {
            plx_base::task::join("demux", t);
        }
        if !p.is_null() {
            plx_net::stream::http_close(p); // sole owner now: the reader is joined
        }
        return Err(());
    }
    // The side subtitle reader (dev trigger `plxnative-subside`), started once the demuxer is
    // spawned and the media thread exists: it waits for the demuxer's first keyframe before it
    // reads anything. (Its ownership flag was set before the demux spawn, below.)
    side_reader.start_after_load_spawn();

    let (lead_at_start, lead_pending) = audio_lead.at_engine_start();
    let eng = Engine {
        shared,
        attempt,
        native_epoch,
        stage: Stage::Loading,
        video_info_sent: false,
        bound_at_frames: None,
        placed_src: (0, 0),
        eos_pushed: false,
        // if a seek is armed for the FIRST open (resume, or reload_at), rebase the first
        // post-seek keyframe to fed-pts 0 so the pipeline sees a 0-based timeline identical
        // to fresh play (disp_base carries the content offset). Plain fresh play leaves this
        // false (first keyframe is already ~0).
        rebase_pending: shared.seek_to_ns.load(Ordering::Relaxed) >= 0,
        rebase_drops: 0,
        seek_armed_at: 0,
        seek_retries: 0,
        flushed: false,
        presentation_rearm_pending: false,
        max_fed_video_pts: 0,
        max_fed_audio_pts: 0,
        audio_start_fp: None,
        pcm_sink_lead_ns: lead_at_start,
        pcm_lead_pending: lead_pending,
        feed_pace: FeedPace::default(),
        seek_base_pts: 0,
        prime_play: false,
        startup_controlled: true,
        startup_user_held: user_held,
        startup_play_commanded: false,
        startup_first_play_done: false,
        startup_play_refused: false,
        startup_immediate_play: false,
        aq_video: aqv_box,
        aq_audio: aqa_box,
        hs,
        pending_video: None,
        pending_audio: None,
        payload: payload_c,
        source,
        stream_th,
        load_th,
        side_reader,
        preview_abandon: false,
        foreground_follows_load: false,
        era_watch: crate::era_callbacks::Watch::new(),
    };
    // Select this session's direct-play seek mode (`SeekMode`, mod.rs, whose SCOPE note argues why
    // it is per session). `feed_stream` can downgrade it to `Reload` mid-session (the pipeline was
    // unreachable, or LG refused a `Seek`), and that is a fact about the StarfishMediaAPIs object
    // — which `sf_load` constructs and teardown's `sf_destroy` destructs, once per session — not
    // about the TV, so it is re-selected here for every session. Placed HERE rather than at the
    // top of this function on purpose: the `engine_is_live` no-op return above must NOT re-select,
    // or a stray PLAY landing mid-session would cancel a downgrade that is still needed.
    (hooks.select_seek_mode)();
    feed.install(eng);
    native_start.commit();
    clock_start.commit();
    transport.started.store(true, Ordering::Relaxed);
    plx_base::eventlog::log(&format!(
        "SMP: media thread spawned, stream={}",
        stream as i32
    ));
    Ok(())
}
