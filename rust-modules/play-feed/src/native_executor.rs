//! Main-thread confined native buffer-feed execution. A prepared Load owns its clock and
//! callback epochs until the addressed route attempt installs its Engine.

use crate::aq::{self, AuQueue};
use crate::feed_resources::{FeedResources, LoadReceipt};
use crate::feed_state::{AbandonedLoad, AuBox, Engine, PendingRelease, Source};
use crate::feed_state::{FeedPace, SampleBuf, AQ_AUDIO_BYTES, AQ_VIDEO_BYTES};
use crate::ff::DemuxTransport;
use crate::open::PhysicalOpen;
use crate::pcm_lead::{pcm_lead_plan, PcmLead};
use crate::seek::SeekMode;
use crate::side_reader::SideReaderPort;
use crate::shared::PhysicalShared;
use crate::shared::{HlsPlayCompletion, HlsPrimeKind, Stage, Transport};
use crate::SendPtr;
use plx_base::task::MainThread;
use plx_platform::tv::sink::VideoSink;
use plx_platform::tv::sink::UnloadTicket;
use plx_platform::devcaps::AudioStack;
use plx_net::stream::HttpStream;
use std::os::raw::{c_char, c_int, c_void};
use std::ffi::CString;
use std::sync::atomic::{AtomicI64, Ordering};

static VTOT: AtomicI64 = AtomicI64::new(0);
static ATOT: AtomicI64 = AtomicI64::new(0);
static VATT: AtomicI64 = AtomicI64::new(0);
static AATT: AtomicI64 = AtomicI64::new(0);

pub fn fed_totals() -> (i64, i64) {
    (VTOT.load(Ordering::Relaxed), ATOT.load(Ordering::Relaxed))
}

/// A test's stand-in for a live session's counters (the diag sample reads them like any other).
#[cfg(any(test, feature = "test-support"))]
pub fn store_fed_totals(video: i64, audio: i64) {
    VTOT.store(video, Ordering::Relaxed);
    ATOT.store(audio, Ordering::Relaxed);
}

pub fn reset_fed_counters() {
    VTOT.store(0, Ordering::Relaxed);
    ATOT.store(0, Ordering::Relaxed);
    VATT.store(0, Ordering::Relaxed);
    AATT.store(0, Ordering::Relaxed);
}

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
    Refused,
    ClockBusy,
    NativeBusy,
}

/// Common composition supplies these three already-existing IO seams. The physical executor
/// never reads a route or credential registry; `sink` is the fenced display sink the seek mode is
/// selected from ([`crate::seek::select_seek_mode_for_load`]).
pub struct LaunchHooks {
    pub demux_transport: fn() -> DemuxTransport,
    pub load_thread: fn(SendPtr<c_char>, u32, Option<LoadReceipt>, PcmLead),
    pub sink: fn() -> &'static dyn VideoSink,
}

/// Captured native Load inputs and the installed display sink's window event.
pub struct PrepareHooks {
    pub sink: fn() -> &'static dyn VideoSink,
    pub on_exported_window_create: fn(),
    pub load_refusal_latch: u8,
    /// The common caption runner the side subtitle reader's thread calls back into: what each
    /// open's reader and every `FeedCommand::SyncSideReader` run over.
    pub side_reader_job: &'static dyn plx_playback_contract::subtitle::SideReaderJobPort,
}

/// Native release uses the installed raw sink while the public sink is fenced for other verbs.
/// Each call asks the provider afresh; no native object pointer survives across transitions.
/// `Copy`, so a per-owner override (the unload spawner of a `FeedResources`) is a struct update.
#[derive(Clone, Copy)]
pub struct ReleaseHooks {
    pub raw_sink: fn() -> &'static dyn VideoSink,
    /// The process's plane owner: whether this device's plane is ACB, and the one that records
    /// (and never waits on) the release's `setState(UNLOADED)`.
    pub plane: &'static plx_platform::tv::plane::PlaneOwner,
    pub on_release_step: fn(crate::native_release::ReleaseStep),
    pub on_exported_window_destroy: fn(),
    pub spawn_unload: fn(UnloadTicket) -> Option<std::thread::JoinHandle<()>>,
}

/// Main-thread physical Feed verbs with only the few common observations they publish.
/// The sink provider runs at every native call, including reentrant callbacks and release fences.
pub struct FeedHooks {
    pub sink: fn() -> &'static dyn VideoSink,
    pub mirror_playstate: fn(&MainThread, Stage, bool),
    pub mute_for_preroll: fn(&MainThread),
    pub feed_clock_us: fn() -> u64,
}

/// Everything the common composition lends the physical executor, as one value. The composition
/// builds it once as a `const`; the owner of the native session (`FeedResources`) holds the
/// `&'static` reference, so no process-global registry carries it and every caller that reaches
/// the executor through the owner reaches the same hooks. Each executor entry point still takes the
/// one part it uses (`&hooks.feed`, `&hooks.release`, ...).
pub struct NativeHooks {
    pub prepare: PrepareHooks,
    pub launch: LaunchHooks,
    pub feed: FeedHooks,
    pub release: ReleaseHooks,
}

/// One accepted native audio packet's size, while the stall watchdog's evidence window is open and
/// the feed is a native audio feed; the line it returns is logged once, with the first AUs' sizes.
fn record_native_audio_packet_size(len: u32) -> Option<String> {
    let mut sizes = crate::native_stall::SIZES.lock().unwrap_or_else(|e| e.into_inner());
    if sizes.open() && crate::ff::native_audio::fed().is_some() {
        sizes.record(len)
    } else {
        None
    }
}

fn to_sink_ns(ns: i64, shared: &PhysicalShared) -> i64 {
    let rate = shared.applied_rate_permille.load(Ordering::Relaxed);
    to_sink_ns_at_rate(ns, rate)
}

fn to_sink_ns_at_rate(ns: i64, rate: i32) -> i64 {
    if rate <= 0 || rate == crate::shared::RATE_NORMAL {
        ns
    } else {
        (ns as i128 * i128::from(crate::shared::RATE_NORMAL) / i128::from(rate)) as i64
    }
}

struct SinkReleasePort<'a>(&'a ReleaseHooks);

impl crate::native_release::NativeReleasePort for SinkReleasePort<'_> {
    fn callback_gate_retire(&self, mt: &MainThread) -> bool {
        unsafe { (self.0.raw_sink)().callback_gate_retire(mt) != 0 }
    }

    fn callback_intercepts(&self, mt: &MainThread) -> u32 {
        unsafe { (self.0.raw_sink)().callback_intercepts(mt) }
    }

    /// Recorded in the plane owner so a later bind, from either backend, can wait for the
    /// firmware's completion; this release itself never blocks on it.
    fn plane_unload(&self, mt: &MainThread) {
        self.0.plane.unload((self.0.raw_sink)(), mt);
    }

    fn destroy(&self, mt: &MainThread) -> bool {
        unsafe { (self.0.raw_sink)().destroy(mt) != 0 }
    }

    fn quarantine(&self, mt: &MainThread) {
        unsafe { (self.0.raw_sink)().quarantine(mt) };
    }
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
    /// The route's half of the Dolby Vision landing guard, which the Engine's construction turns into
    /// its [`crate::dv_start::Guard`].
    pub(crate) dv_landing: plx_playback_contract::feed::DvLandingPlan,
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
        dv_landing: plx_playback_contract::feed::DvLandingPlan,
    ) -> Result<Self, PrepareRefusal> {
        let clock_start = ClockStartGuard::arm(shared, user_held, seek_preroll).ok_or_else(|| {
            plx_base::eventlog::log("start_bufferfeed: clock state is not idle (refusing overlapping start)");
            PrepareRefusal::ClockBusy
        })?;
        let native_start = NativeSessionStartGuard::arm(shared).ok_or_else(|| {
            plx_base::eventlog::log("start_bufferfeed: native session is not idle (refusing overlapping Load)");
            PrepareRefusal::NativeBusy
        })?;
        Ok(Self {
            open, sample, stream, payload_c, audio_lead, clock_start, native_start,
            user_held, side_reader, dv_landing,
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
        user_held, mut side_reader, dv_landing } = prepared;
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
            let demux_transport = (hooks.demux_transport)();
            let session = open.session.take().expect("one demux session per captured feed open");
            let abr = open.hls_abr.take();
            let auto_original = open.auto_original.take();
            // Before the demuxer exists, so what it publishes is this session's.
            crate::native_audio::reset();
            // A clock-only reader (a sidecar) reads no subtitle: the demuxer keeps what it enumerates.
            shared.side_subs_owner.store(side_reader.embedded_owner(), Ordering::Release);
            stream_th = plx_base::task::spawn_off_frame_keeping("demux", move |off| {
                crate::demux(off, origin, path, acodec, demux_transport, session, abr, auto_original, aqp, aqap, hsp)
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
        segment_landing: None,
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
        era_watch: crate::era_callbacks::Watch::new(),
        // The seek armed for this first open (a resume, or `reload_at`) is the landing the guard is armed with.
        dv_start: crate::dv_start::Guard::from_plan(dv_landing, {
            let armed = shared.seek_to_ns.load(Ordering::Relaxed);
            (armed >= 0).then_some(armed)
        }),
    };
    // Select this session's direct-play seek mode (`SeekMode`, seek.rs, whose SCOPE note argues why
    // it is per session). `feed_stream` can downgrade it to `Reload` mid-session (the pipeline was
    // unreachable, or LG refused a `Seek`), and that is a fact about the StarfishMediaAPIs object
    // — which `sf_load` constructs and teardown's `sf_destroy` destructs, once per session — not
    // about the TV, so it is re-selected here for every session. Placed HERE rather than at the
    // top of this function on purpose: the `engine_is_live` no-op return above must NOT re-select,
    // or a stray PLAY landing mid-session would cancel a downgrade that is still needed.
    crate::seek::select_seek_mode_for_load((hooks.sink)());
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

/// Unload and retire the native object of `epoch`, then destroy it only with complete evidence.
/// The engine's own teardown and the abandoned-Load release share this so the order below is one
/// definition.
/// **Where a teardown's frame goes, one step at a time.** Each step's wall time is appended as
/// `label:ms` and the whole run is logged as ONE `teardown spans:` line at the end, so a slow BACK
/// frame names the call that held the main thread instead of being inferred from a sched trace.
/// Measuring costs two `Instant::now()` per step; the line is written once per teardown.
pub struct TeardownSpans {
    t0: std::time::Instant,
    last: std::time::Instant,
    out: String,
}

impl TeardownSpans {
    pub fn new() -> Self {
        let now = std::time::Instant::now();
        Self { t0: now, last: now, out: String::new() }
    }

    /// Close the step that ran since the previous mark under `label`.
    pub fn mark(&mut self, label: &str) {
        use std::fmt::Write as _;
        let now = std::time::Instant::now();
        let ms = now.duration_since(self.last).as_secs_f64() * 1000.0;
        self.last = now;
        let _ = write!(self.out, "{}{label}:{ms:.1}", if self.out.is_empty() { "" } else { "," });
    }

    pub fn emit(&self, what: &str) {
        plx_base::eventlog::log(&format!(
            "teardown spans: {what} total={:.1} {}",
            self.t0.elapsed().as_secs_f64() * 1000.0,
            self.out,
        ));
    }
}

/// Release an abandoned Load only after its worker has returned, so the payload and native
/// object remain alive for every byte the worker can still read.
pub fn reap_abandoned_load_resource(
    feed: &mut FeedResources,
    mt: &MainThread,
    hooks: &ReleaseHooks,
) -> bool {
    let finished = match feed.abandoned_load_mut() {
        Some(parked) => parked.load_th.is_finished(),
        None => return false,
    };
    if !finished {
        return false;
    }
    let parked = feed.take_abandoned_load().expect("checked above");
    plx_base::task::join("media", parked.load_th);
    plx_base::eventlog::log(&format!(
        "native lifecycle: abandoned Load epoch={} returned {}ms after teardown; releasing it",
        parked.epoch,
        parked.abandoned_at.elapsed().as_millis(),
    ));
    let status = release_native_object_timed(mt, parked.epoch, &mut TeardownSpans::new(), feed.shared(), hooks);
    feed.note_reaped_release(parked.attempt, status);
    true
}

/// Everything a native open does before it prepares anything: release an abandoned Load that has
/// returned, refuse (`false`) while one has not (it still owns the one native object the C seam
/// holds), and zero the per-session fed counters and the stall watchdog's window.
pub fn begin_native_open(feed: &mut FeedResources, mt: &MainThread) -> bool {
    reap_abandoned_load_resource(feed, mt, &feed.hooks().release);
    if feed.has_abandoned_load() {
        plx_base::eventlog::log(
            "start_bufferfeed: an abandoned Load has not returned yet (refusing native start)");
        return false;
    }
    reset_fed_counters();
    crate::native_stall::reset();
    true
}

/// Start an off-frame Unload or complete it synchronously when the object cannot be handed off.
pub fn release_off_frame(
    mt: &MainThread,
    epoch: u32,
    sp: &mut TeardownSpans,
    status: &mut plx_playback_contract::ReleaseStatus,
    shared: &'static PhysicalShared,
    hooks: &ReleaseHooks,
) -> Option<std::thread::JoinHandle<()>> {
    if unsafe { (hooks.raw_sink)().ready(mt) } == 0 || !shared.begin_native_release(epoch) {
        *status = release_native_object_timed(mt, epoch, sp, shared, hooks);
        return None;
    }
    let th = (hooks.spawn_unload)(UnloadTicket::issue(mt));
    sp.mark("unload_handoff");
    if th.is_none() {
        plx_base::eventlog::log(
            "native lifecycle: no thread for the off-frame Unload; unloading on the main thread",
        );
        unsafe { (hooks.raw_sink)().unload(&UnloadTicket::issue(mt)) };
        sp.mark("sf_unload");
        *status = finish_unloaded_object(mt, epoch, sp, shared, hooks);
        destroy_native_window(mt, sp, hooks);
    }
    th
}

/// Main-thread tail of a finished off-frame Unload.
pub fn run_release_tail(
    mt: &MainThread,
    p: PendingRelease,
    shared: &'static PhysicalShared,
    hooks: &ReleaseHooks,
) -> plx_playback_contract::ReleaseStatus {
    let mut sp = TeardownSpans::new();
    sp.mark("join");
    let status = finish_unloaded_object(mt, p.epoch, &mut sp, shared, hooks);
    destroy_native_window(mt, &mut sp, hooks);
    sp.emit("release");
    plx_base::eventlog::log(&format!(
        "native lifecycle: off-frame Unload epoch={} released {} ms after the stop",
        p.epoch,
        p.handed_off_at.elapsed().as_millis(),
    ));
    status
}

/// Unload, close the C callback gate, retire the Rust epoch, then destroy or quarantine.
pub fn release_native_object_timed(
    mt: &MainThread,
    epoch: u32,
    sp: &mut TeardownSpans,
    shared: &'static PhysicalShared,
    hooks: &ReleaseHooks,
) -> plx_playback_contract::ReleaseStatus {
    let status = if unsafe { (hooks.raw_sink)().ready(mt) } != 0 {
        sp.mark("pre_unload");
        unsafe { (hooks.raw_sink)().unload(&UnloadTicket::issue(mt)) };
        sp.mark("sf_unload");
        finish_unloaded_object(mt, epoch, sp, shared, hooks)
    } else {
        let no_object = (hooks.raw_sink)().no_object_proof(mt);
        let retired = shared.retire_native_session(epoch);
        if !retired {
            plx_base::eventlog::log(&format!(
                "native lifecycle: no dispatchable object and epoch={epoch} was already retired",
            ));
        }
        if retired && no_object {
            plx_playback_contract::ReleaseStatus::Released
        } else {
            plx_playback_contract::ReleaseStatus::Quarantined
        }
    };
    destroy_native_window(mt, sp, hooks);
    status
}

fn finish_unloaded_object(
    mt: &MainThread,
    epoch: u32,
    sp: &mut TeardownSpans,
    shared: &'static PhysicalShared,
    hooks: &ReleaseHooks,
) -> plx_playback_contract::ReleaseStatus {
    use crate::native_release::ReleaseStep;
    let mut status = plx_playback_contract::ReleaseStatus::Quarantined;
    crate::native_release::finish_unloaded_object(
        &SinkReleasePort(hooks),
        shared,
        mt,
        epoch,
        || hooks.plane.handle(),
        |label| sp.mark(label),
        |step| {
            if step == ReleaseStep::NativeDestroyed {
                status = plx_playback_contract::ReleaseStatus::Released;
            }
            (hooks.on_release_step)(step);
        },
    );
    status
}

fn destroy_native_window(mt: &MainThread, sp: &mut TeardownSpans, hooks: &ReleaseHooks) {
    unsafe { (hooks.raw_sink)().destroy_window(mt) };
    sp.mark("destroy_window");
    if (hooks.raw_sink)().window_mode() == plx_platform::tv::sink::VP_EXPORTED {
        (hooks.on_exported_window_destroy)();
    }
}



/// Owns a removed Engine through worker joins, native release and queue destruction. Common
/// route/report work may run between its phases without receiving the Engine or native handles.
pub(crate) struct NativeStop {
    eng: Engine,
    stream: bool,
    spans: TeardownSpans,
    abandoned_load: Option<std::thread::JoinHandle<()>>,
    unload_th: Option<std::thread::JoinHandle<()>>,
    release_status: plx_playback_contract::ReleaseStatus,
    /// The logical playback ended under a kept session (`FeedStop::stop_begin`'s `terminal`): a
    /// Load still in the native call is parked, and the object is released off the frame, as for
    /// a real stop, though `for_reload` says the session stays.
    terminal: bool,
}

impl NativeStop {
    pub fn take(feed: &mut FeedResources, terminal: bool) -> Self {
        let eng = feed.take().expect("main-thread live check and take are one transaction");
        feed.retire_load_attempt(eng.attempt);
        let stream = matches!(eng.source, Source::Stream { .. });
        Self { eng, stream, spans: TeardownSpans::new(), abandoned_load: None, unload_th: None,
            release_status: plx_playback_contract::ReleaseStatus::Closing, terminal }
    }

    /// The attempt whose Engine this stop owns.
    pub fn attempt(&self) -> plx_playback_contract::AttemptId { self.eng.attempt }

    /// The side reader must stop before the paired demux reader is joined.
    pub fn stop_side_reader(&mut self, for_reload: bool) {
        self.spans.mark("begin");
        self.eng.side_reader.stop_before_demux_join(if for_reload { "reload" } else { "stop" });
        self.spans.mark("subside");
    }

    /// Wake both transports and queues before either worker join; close the socket only after
    /// the demux reader no longer owns its fd. A timed-out Load remains parked and unjoined.
    pub fn unblock_join_close(&mut self) {
        let shared = self.eng.shared;
        if self.stream {
            for q in [self.eng.aq_video.as_mut(), self.eng.aq_audio.as_mut()].into_iter().flatten() {
                aq::aq_abort(&mut **q);
            }
            let p = shared.hs_ptr.load(Ordering::Acquire);
            if !p.is_null() { plx_net::stream::http_shutdown(p); }
            plx_net::range_io::abort_active();
        }
        self.spans.mark("unblock");
        if let Some(thread) = self.eng.stream_th.take() {
            plx_base::task::join("demux", thread);
        }
        self.spans.mark("join_demux");
        self.abandoned_load = match self.eng.load_th.take() {
            Some(thread) if !thread.is_finished()
                && (shared.load_timed_out.load(Ordering::Acquire) || self.terminal) => Some(thread),
            Some(thread) => {
                plx_base::task::join("media", thread);
                None
            }
            None => None,
        };
        self.spans.mark("join_media");
        if self.stream {
            let p = shared.hs_ptr.load(Ordering::Acquire);
            if !p.is_null() { plx_net::stream::http_close(p); }
        }
        self.spans.mark("http_close");
    }

    pub fn report_dispatched(&mut self) { self.spans.mark("scrobble"); }

    /// Whether the unblock just found a Load past its budget still inside the native call and
    /// parked it: the release that follows hands it to the abandoned-Load release. Known from
    /// [`Self::unblock_join_close`] on, before the native phase says so.
    pub fn load_abandoned(&self) -> bool { self.abandoned_load.is_some() }

    pub fn release_status(&self) -> plx_playback_contract::ReleaseStatus {
        self.release_status
    }

    /// Release only after common reporting has handed its final write to the off-frame worker.
    pub fn release_and_drain(&mut self, mt: &MainThread,
        feed: &mut FeedResources, for_reload: bool, hooks: &ReleaseHooks) {
        let shared = self.eng.shared;
        if let Some(load_th) = self.abandoned_load.take() {
            if !shared.abandon_native_session(self.eng.native_epoch) {
                plx_base::eventlog::log(&format!(
                    "native lifecycle: abandoned Load epoch={} no longer owned the Active phase",
                    self.eng.native_epoch,
                ));
            }
            plx_base::eventlog::log(&format!(
                "native lifecycle: Load epoch={} still in flight at teardown — not joining; the \
                 object stays parked (and native playback refused) until Load returns",
                self.eng.native_epoch,
            ));
            feed.park_abandoned_load(AbandonedLoad {
                attempt: self.eng.attempt,
                epoch: self.eng.native_epoch,
                load_th,
                _payload: std::mem::take(&mut self.eng.payload),
                abandoned_at: std::time::Instant::now(),
            });
        } else if !for_reload || self.terminal {
            // A per-owner spawner (a test's, or a composition's) replaces the default task start.
            let hooks = ReleaseHooks {
                spawn_unload: feed.unload_spawner().unwrap_or(hooks.spawn_unload),
                ..*hooks
            };
            self.unload_th = release_off_frame(mt, self.eng.native_epoch, &mut self.spans,
                &mut self.release_status, feed.shared(), &hooks);
        } else {
            self.release_status = release_native_object_timed(
                mt, self.eng.native_epoch, &mut self.spans, feed.shared(), hooks);
        }
        if self.stream {
            drain_aq(&mut self.eng);
            for q in [self.eng.aq_video.as_mut(), self.eng.aq_audio.as_mut()].into_iter().flatten() {
                aq::aq_destroy(&mut **q);
            }
        }
        self.spans.mark("drain_aq");
    }

    /// Keep the Engine and payload alive under an off-frame Unload; otherwise drop only after
    /// every worker has joined and the queues are destroyed.
    pub fn finish(mut self, for_reload: bool) -> Option<PendingRelease> {
        self.spans.mark("reset");
        self.spans.emit(if for_reload { "reload" } else { "stop" });
        if let Some(unload_th) = self.unload_th.take() {
            plx_base::eventlog::log(&format!(
                "stop_bufferfeed: torn down (native Unload epoch={} continues off-frame)",
                self.eng.native_epoch,
            ));
            Some(PendingRelease {
                epoch: self.eng.native_epoch,
                unload_th: Some(unload_th),
                handed_off_at: std::time::Instant::now(),
                _engine: self.eng,
            })
        } else {
            plx_base::eventlog::log("stop_bufferfeed: torn down");
            None
        }
    }
}

/// feed streamed AUs from the demux queue; hold the current AU across ticks on
/// BufferFull (backpressure); zero-base the fed timeline on the first post-seek
/// keyframe; drop stale AUs past the B-frame reorder distance.
/// prime-then-play buffer depth: how much of the post-seek stream to buffer (paused) before
/// starting the clock. Enough to cover the pipeline's decode latency so the first frame is ready.
pub const PRIME_NS: i64 = 700_000_000;
// Prime the AUDIO lane too before starting the (audioSync master) clock — else a rapid-seek drain
// can start Play on an empty audio queue and leave audio silent until the next seek. Fallback:
// start anyway once video buffers PRIME_VIDEO_MAX_NS without audio (audioless / briefly starved),
// so a genuinely audioless region can't hang.
pub const PRIME_AUDIO_NS: i64 = 300_000_000;
pub use crate::feed_state::PRIME_VIDEO_MAX_NS;
// Feed-ahead throttle (Kodi-parity): keep the VIDEO lane at most this far ahead of the presented
// position (FEED_SHARED.pres_fed) instead of feeding greedily to BufferFull. Bounding the buffer to
// ~1.6s (was ~10-20s: aq 6MB + the pipeline's own ~8MB) makes seeks flush far less, keeps the
// clock from running ahead, and cuts latency. AUDIO gets a looser bound so it can ride slightly
// ahead (audio buffer is cheap and it's the master clock) without unbounded race on odd muxes.
pub use crate::feed_state::MAX_FEED_AHEAD_NS;
pub use plx_playback_contract::feed::AUDIO_SLACK_NS;
// A fed pts this far below a lane's high-water is a stale pre-seek AU (past the B-frame reorder
// distance) → drop it rather than feed a backward jump.
pub const STALE_BACKJUMP_NS: i64 = 2_000_000_000;
// In-place-seek rebase guard: a first-keyframe this far from the seek target is a stale pre-reopen
// frame from the drifted read position, not the real post-seek keyframe. av_seek(BACKWARD) lands
// AT/BEFORE the target, so a valid anchor is never ahead and is at most ~one GOP behind — hence a
// tight AHEAD bound and a looser BEHIND bound (to tolerate sparse keyframes). Capped by MAX_REBASE_DROPS.
pub const SEEK_STALE_AHEAD_NS: i64 = 6_000_000_000;
pub const SEEK_STALE_BEHIND_NS: i64 = 30_000_000_000;
pub const MAX_REBASE_DROPS: i32 = 240;
// Audio counterpart of the rebase guard: an audio AU this far AHEAD of the video high-water is a
// stale drifted frame (the demuxer's pre-reopen audio after a seek). Feeding it poisons
// max_fed_audio_pts (and then the backjump guard drops all LEGIT audio until the demux catches
// up to the poisoned mark → seconds of silence). Legit audio lead is bounded by the throttle to
// ~MAX_FEED_AHEAD+AUDIO_SLACK (3.6s), so 5s has margin. It was 15s, and a stuck-retry seek fed
// stale audio 14.9s ahead — 13s of user-audible silence after a rapid 10s-back tap burst.
pub const AUDIO_STALE_AHEAD_NS: i64 = 5_000_000_000;
/// How far past the start of its timeline the audio lane may feed whatever the clock says.
///
/// **A track whose audio starts late must still reach Starfish before the clock can move.** An
/// MKV audio `Delay` (an 11.008 s E-AC-3 track on a WEB-DL, 2026-10) puts the first audio AU
/// seconds past the first video one. Starfish's clock waits for audio, so `pres_fed` sits at 0
/// and the video lane stops [`MAX_FEED_AHEAD_NS`] in — and the audio AU, far past both, was
/// dropped as stale by [`AUDIO_STALE_AHEAD_NS`] and would have been held by the throttle anyway.
/// Nothing moved again: the picture buffered forever, while a resume past the gap played. The
/// AU the demuxer marked as its first since the open or seek is the real start, never stale
/// pre-seek audio (that was pushed before the mark), so it and the next 2 s are fed regardless.
/// 2 s is under the throttle's own audio lead ([`MAX_FEED_AHEAD_NS`] + [`AUDIO_SLACK_NS`]), so a
/// track that starts on time is fed exactly as before.
///
/// **The demuxer has to be able to READ that much audio**, which for a late track means with the
/// video lane already past its base cap and nobody draining it (no clock yet): `ff::demux` keeps
/// the video lane's pre-audio headroom until the audio lane has been pushed this much plus a
/// margin (`ff::LEAD_IN_MARGIN_NS`), and only then releases it as the consumer drains.
pub use crate::feed_state::AUDIO_LEAD_IN_NS;
// The lead-in is only sound between these two: under the throttle's own audio lead (a track that
// starts on time is fed exactly as before), and over the audio the clock is primed with (else
// "ahead of the clock" would not be enough audio to start it).
const _: () = assert!(AUDIO_LEAD_IN_NS < MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS);
const _: () = assert!(AUDIO_LEAD_IN_NS > PRIME_AUDIO_NS);
// Sentinel for FEED_SHARED.pres_fed meaning "no post-seek frame has presented yet" — the feed-ahead
// throttle treats it as feed-freely (don't compare the new fed pts against a stale pre-seek
// presented position). Set on a seek; the first presented frame overwrites it with a real pts.
pub use crate::feed_state::PRES_NONE;


/// The most wall-clock time ONE lane may spend feeding in one tick, in µs, once it has fed an AU
/// **and its lead is healthy** (see [`lane_starved`]).
///
/// `Feed()` blocks on the frame thread, 0.3–0.7 ms per AU on the TV. In steady state a lane feeds
/// one or two AUs a frame and never comes near this. The throttle does not apply while priming,
/// though, and a post-prime lane is a full lead behind, so on 2026-10-01 the first frames of
/// every playback fed the whole demuxed backlog at once: `FRAMEDROP total=89.1 … sfvx62:19.4,
/// sfax120:50.8` (62 video and 120 audio AUs in one frame), then `sfax53:38.3` the frame after,
/// with the player's HUD and track menu on screen. A seek re-primes the same way. Two lanes at
/// this slice cost at most ~6 ms of a 16.7 ms frame plus one AU's overrun each.
/// The slice is per LANE, so the video lane cannot spend the audio lane's share. The old greedy
/// video lane did exactly that: it fed 2.5 s of video before audio got its turn, and the prime
/// then fell through to the video-only escape (`primed: v=2541ms a=0ms`).
///
/// **The slice spreads a BACKLOG; it must never throttle a lane that is short of media.** A
/// lane below its low-water mark ([`lane_starved`]) is exempt: while priming, that is until the
/// lane holds its prime depth ([`PRIME_NS`] / [`PRIME_AUDIO_NS`], 17–42 AUs) — otherwise a slow
/// `Feed()` (3 ms or more, so one AU a lane a tick) stretches time-to-first-frame by that many
/// ticks — and once playing, until it is [`FEED_LOW_WATER_NS`] ahead of the playhead — otherwise
/// at the 50 Hz present rate 60 fps content drains its lead. The exemption is bounded by the
/// low-water mark itself, so the bulk of a backlog is still fed a slice a tick, and the prime
/// finishes on the tick its depth is reached, one tick later at most than a greedy feed would.
pub const FEED_LANE_SLICE_US: u64 = 3_000;

/// A playing lane whose lead over the presented position is below this is exempt from
/// [`FEED_LANE_SLICE_US`]. 250 ms is [`FEED_PACE_CAP_MS`]: the position reports arrive every
/// ~200 ms, so a lead under it can be gone before the next report says so, and it is ~6 AUs of
/// 24 fps video — a handful of `Feed()` calls, bounded cost, never a backlog.
pub const FEED_LOW_WATER_NS: i64 = 250_000_000;

/// Whether a lane is short of media, and so must be fed past its slice.
///
/// While priming (`priming`) the lane is short until it holds `prime_ns` past the seek base;
/// otherwise it is short when its lead over the presented position `pres` ([`PRES_NONE`]: nothing
/// presented, no lead to judge, not short) is under [`FEED_LOW_WATER_NS`].
fn lane_starved(priming: bool, max_fed: i64, seek_base: i64, prime_ns: i64, pres: i64) -> bool {
    if priming {
        max_fed - seek_base < prime_ns
    } else {
        pres != PRES_NONE && max_fed - pres < FEED_LOW_WATER_NS
    }
}


/// One lane's share of a tick's feeding: [`FEED_LANE_SLICE_US`] from the lane's start.
struct FeedSlice {
    start: u64,
}

impl FeedSlice {
    fn begin(clock_us: fn() -> u64) -> Self {
        Self { start: clock_us() }
    }

    /// Whether the lane should stop before its next AU. A lane that has fed nothing yet always
    /// gets one AU, so the backlog drains on every tick, however slow the sink is; a `starved`
    /// lane ([`lane_starved`]) is never spent.
    fn spent(&self, fed: usize, starved: bool, clock_us: fn() -> u64) -> bool {
        fed > 0 && !starved && clock_us().saturating_sub(self.start) >= FEED_LANE_SLICE_US
    }
}



/// Nanoseconds added to every fed VIDEO PTS — **a one-tick rounding repair for LG's Dolby Vision
/// display-management lookup**, and inert everywhere else.
///
/// The fault is arithmetic and it is entirely on the television's side; this is the only lever we
/// have on it. `gstdualsequencer.c:606` (DWARF-confirmed, `libgstdualsequencer.so` 0x25b0–0x25e0)
/// keys the LUT entry it hands the display firmware with a DOUBLE truncation of the buffer's
/// nanosecond PTS:
///
/// ```text
/// ulTimeStamp = trunc(trunc(pts_ns) * 9 / 100000)      // ns -> 90 kHz ticks
/// ```
///
/// and `DOVI_SWSync_SetDoviLUTnMap` (`libkadaptor` 0xe30e8) then scans all 95 slots for **exact
/// 32-bit equality** — no tolerance, no nearest match. At 24000/1001 fps neither unit is exact:
/// a frame time is a whole number of nanoseconds only every **3rd** frame and a whole number of
/// 90 kHz ticks only every **4th**, so on `n ≡ 4, 8 (mod 12)` — and on no other frame — the two
/// truncations disagree by exactly one tick, the lookup misses, and the panel reuses the previous
/// frame's tone mapping. `lcm(3,4) = 12` frames is **0.5005 s**, which is the period of the
/// stutter as seen.
///
/// Measured on this set, uninstrumented, 85 s of Profile 5: 340 misses, and **340 of 340** equal
/// the double-truncated key while **0 of 340** equal the exact tick value — residues `{4: 170,
/// 8: 170}` and nothing else. It is a derivation that reproduces the data with no free parameter,
/// not a fit.
///
/// The repair is to hand the pipeline a PTS whose truncation lands in the right bin. The loss is
/// the fractional nanosecond FFmpeg's rescale already dropped, so it is strictly less than 1 ns,
/// and **one** nanosecond recovers it. The upper bound is `100000/9 ≈ 11111 ns` — beyond that an
/// already-correct frame would be pushed a tick the other way — so 1 sits at the safe end of a
/// wide range. As an A/V offset it is nothing: 1 ns against a 41.7 ms frame.
///
/// **Video only.** The key is computed from the video buffer's own PTS; the audio lane never
/// reaches this code and shifting it would be a skew for no reason.
///
/// **THE SIGN WAS BACKWARDS, AND −1 IS THE FIX.** Read the history below for what was tried; the
/// short version is that the model was right about the mechanism, wrong about which side rounds,
/// and the device settled it. Measured with LG's own level-2 KADP logging armed mid-playback
/// (`tools/logmprobe`): for **38 of 40** misses the key written into the LUT ring is exactly the
/// key the display firmware requested **plus one**. The ring is a tick HIGH, so the fed PTS goes
/// DOWN. Alternating unseeked legs, same title, same binary:
///
/// ```text
///   nudge = -1   misses 1        nudge = 0   misses 81
///   nudge = -1   misses 1        nudge = 0   misses 81
/// ```
///
/// Reproducible, 81:1, and the arithmetic that predicts −1 also predicted +1 would help; +1 was
/// measured at 163/165 against 164 for zero — i.e. inert. **Trust the measurement here, not the
/// derivation**: our fed PTS is not passed through, the pipeline re-timestamps by NEAREST-rounding
/// on the 1001/24000 lattice (measured: alternating −0.333/+0.333 ns against the exact rational),
/// so exactly how one nanosecond on our side moves a tick on theirs is not something this comment
/// can honestly claim to model. What it can claim is 81:1, twice, with the scene controlled by
/// alternation.
///
/// # The history, kept because three of its steps were wrong and each cost a run
///
/// **AND THE DEVICE REFUTED THE FIRST ATTEMPT, which is why the default was 0 for a while.** The one step
/// the disassembly could not settle was whether the OTHER side of that exact-equality comparison
/// moves with us. It does. Controlled A/B, same title, same seek to 900 s, same 45 s window, only
/// this value differing: **nudge 0 → 118 misses, nudge 1 → 230**. Worse, not fixed. A constant
/// offset cannot repair a *relative* truncation difference when both sides derive from the same
/// fed timestamp — which is now measured rather than assumed, and which also retires the tidiest
/// explanation this investigation has produced.
///
/// What survives the refutation is the arithmetic, and it is not small: 340 of 340 misses in the
/// unseeked run equal the double-truncated key and 0 of 340 equal the exact tick value, residues
/// `{4, 8}` mod 12 and nothing else. So the key IS computed that way and the comparison IS exact.
/// What is now open is why the slot the firmware asks for — using, we measured, dualsequencer's
/// own key — is not in the ring. That points at pairing or at slot lifetime, not at rounding.
///
/// `/tmp/plxnative-ptsnudge=<ns>` is kept because it is the instrument that produced that result
/// and the next candidate value is one run away. Anything from 1 to ~11110 is in range; beyond
/// that an already-correct frame is pushed a tick the other way.
///
/// It is also how the PCM lead (#1086) was first measured: `ptsnudge=-390000000` (the VIDEO 390 ms
/// earlier) read -0.017 to -0.041 s on a PCM Load, against -0.377 s with no correction. The correction
/// ships on the audio side instead (`PCM_SINK_LEAD_NS`), so the two are not to be combined in a
/// run: the video's would be taken twice.
fn pts_nudge_ns() -> i64 {
    const DEFAULT: i64 = -1;
    static NUDGE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(i64::MIN);
    let v = NUDGE.load(Ordering::Relaxed);
    if v != i64::MIN {
        return v;
    }
    // Latched at the first feed rather than read per AU: this is the hottest path in the app and
    // the trigger surface is a filesystem open. Same shape as every other `devtrig::` read here.
    let v = plx_base::devtrig::read("ptsnudge")
        .and_then(|s| s.trim().parse::<i64>().ok())
        .unwrap_or(DEFAULT);
    NUDGE.store(v, Ordering::Relaxed);
    if v != DEFAULT {
        plx_base::eventlog::log(&format!(
            "ptsnudge: video PTS nudge overridden to {v} ns (default {DEFAULT})"
        ));
    }
    v
}

/// The audio lane's sink pts: content time `fp` moved onto the pipeline's timeline
/// ([`to_sink_ns`]) plus the Load's `pcm_lead_ns`. The lead is real time the set takes, so it
/// is added AFTER the speed division: at 1.5x a 400 ms lead is still 400 ms, not 267. PURE.
pub fn audio_sink_pts(fp: i64, pcm_lead_ns: i64, rate: i32) -> i64 {
    to_sink_ns_at_rate(fp, rate).saturating_add(pcm_lead_ns).max(0)
}



/// Take the output's lead from the Load's return, once, before the first audio unit of this Load is
/// fed. `false` while the Load has not returned (the audio waits; nothing can be fed before it
/// anyway). A lead that was settled on the frame thread has nothing to take.
pub fn settle_pcm_lead(eng: &mut Engine) -> bool {
    let shared = eng.shared;
    if !eng.pcm_lead_pending {
        return true;
    }
    let Some(ns) = shared.native_load_pcm_lead(eng.native_epoch) else {
        return false;
    };
    eng.pcm_sink_lead_ns = ns;
    eng.pcm_lead_pending = false;
    true
}

pub fn hls_candidate_snapshot_stable(start: u64, end: u64) -> bool {
    start & 1 == 0 && start == end
}



/// Execute the first immediate Play only after the common startup runner has commanded it.
/// The same clock reservation and completion rule is used by the progressive legacy fixture.
/// `Some(false)` is a native refusal; `None` means the clock gate deferred the verb.
pub fn play_initial_now(mt: &MainThread, eng: &mut Engine, hooks: &FeedHooks) -> Option<bool> {
    let shared = eng.shared;
    let generation = shared.hls_candidate_generation.load(Ordering::Acquire);
    let recovery = shared.hls_recovery();
    if let Some((token, _)) =
        shared.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, recovery)
    {
        let accepted = unsafe { (hooks.sink)().play(mt) } != 0;
        match shared.complete_hls_prime_play(token, accepted) {
            HlsPlayCompletion::Accepted { resume_acb } => {
                if resume_acb {
                    (hooks.mirror_playstate)(mt, eng.stage, true);
                }
                eng.prime_play = false;
                plx_base::eventlog::log("SMP Play");
                return Some(true);
            }
            HlsPlayCompletion::Refused => {
                eng.prime_play = true;
                plx_base::eventlog::log("SMP Play refused; priming for retry");
                return Some(false);
            }
            HlsPlayCompletion::Stale => {
                eng.prime_play = true;
                plx_base::eventlog::log("SMP Play result stale; priming for retry");
            }
        }
    } else {
        eng.prime_play = true;
        plx_base::eventlog::log("SMP Play fenced; priming before retry");
    }
    None
}

/// **Start the clock once both lanes are buffered past the seek base** — video to [`PRIME_NS`]
/// AND audio to [`PRIME_AUDIO_NS`], with a video-only escape at [`PRIME_VIDEO_MAX_NS`] so an
/// audioless or briefly-starved stream still starts.
///
/// **This is called from TWO places, and the second one is the whole point.** It used to live
/// inline in [`feed_stream`]'s per-AU loop and nowhere else, which made it unreachable in exactly
/// the case that matters. A pump tick runs the video lane and then the audio lane, so when a whole
/// HLS segment lands at once the video lane drains all of it while `abuf` is still zero — the
/// `abuf` arm cannot be true yet, and a 2-second segment never reaches the 2500 ms video-only
/// escape. The audio lane then fills, but evaluating nothing. On every later tick `aq_pop` returns
/// null and the loop breaks before reaching the check, so Play ends up waiting on a video AU that
/// only the next segment can supply. On 2026-08-29 that held a viewer's picture for 94 seconds
/// with both lanes holding a playable buffer the entire time.
///
/// Calling it again at the end of [`feed_audio_lane`] closes that hole at the only moment both
/// lanes' extents are known. It is idempotent — `prime_play` latches false on the first success —
/// so the in-loop call is kept for the case it already served: a direct-play stream that reaches
/// the threshold mid-lane should start on that AU, not a tick later.
pub fn try_prime(mt: &MainThread, eng: &mut Engine, transport: &Transport, hooks: &FeedHooks) {
    let shared = eng.shared;
    if !eng.prime_play {
        return;
    }
    // A paused reload owns a bounded first-picture preroll, not a viewer Play. The pump
    // re-pauses it after presentation; waiting for the viewer's command here strands that
    // preroll over a blank plane. Every other initial prime still waits for common startup.
    if eng.startup_controlled && !eng.startup_play_commanded && !transport.seek_preroll_active() {
        return;
    }
    let Some(prime_kind) = shared.hls_prime_kind() else {
        return;
    };
    // If the clock is already held, do not restart it in the middle of an exploratory transaction.
    // The transaction's own deadline is consuming its discretionary B-max(R,D) budget; starting
    // the playhead too would spend that balance a second time. This marker never pauses a running
    // clock — it only delays release of an existing prime hold until commit/reject. Dropping the
    // trial marker releases this gate, and the next ordinary pump tick retries the prime.
    if shared.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0 {
        return;
    }
    let candidate_generation = shared.hls_candidate_generation.load(Ordering::Acquire);
    if !hls_candidate_snapshot_stable(candidate_generation, candidate_generation) {
        return;
    }
    let accepted_vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
    let accepted_abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
    let observed_runway_ns = shared
        .hls_prime_runway_ms
        .load(Ordering::Relaxed)
        .max(0)
        .saturating_mul(1_000_000);
    // During an internal hold or a user ResumePrime, Starfish may stop accepting AUs while its own
    // source buffers are full. The already-demuxed application queues behind it are still
    // guaranteed playable media: they have passed transport, completeness, demux and timestamp
    // validation, and feeding is open while the clock remains held. Requiring the entire observed
    // runway to be Starfish-accepted can therefore deadlock at the native buffer cap with fuller
    // application queues behind it.
    //
    // Decoder prime remains an accepted-data condition. Only a continuation boundary (automatic
    // rebuffer or ResumePrime) reads the full pipeline high-water. Initial Load/seek remains
    // accepted-only because it is a fresh clock boundary whose old queued tail is not proof about
    // the newly loaded/landed timeline.
    let queued_runway = matches!(prime_kind, HlsPrimeKind::Rebuffer | HlsPrimeKind::Resume);
    let shift = shared.pts_shift.load(Ordering::Relaxed);
    let queued_video = shared.hls_video_tail_ns.load(Ordering::Acquire);
    let queued_audio = shared.hls_audio_tail_ns.load(Ordering::Acquire);
    let playable_video_high = if queued_runway && queued_video >= 0 {
        eng.max_fed_video_pts
            .max(queued_video.saturating_add(shift))
    } else {
        eng.max_fed_video_pts
    };
    let playable_audio_high = if queued_runway && queued_audio >= 0 {
        eng.max_fed_audio_pts
            .max(queued_audio.saturating_add(shift))
    } else {
        eng.max_fed_audio_pts
    };
    let playable_vbuf = playable_video_high - eng.seek_base_pts;
    let playable_abuf = playable_audio_high - eng.seek_base_pts;
    // An A/V stream requires both lanes even while audio is temporarily empty. Only a segment the
    // demuxer proved audioless may use the video-only escape; `abuf <= 0` alone is starvation, not
    // evidence that no audio exists.
    let audio_expected = shared.hls_audio_expected.load(Ordering::Acquire) || playable_abuf > 0;
    let decoder_ready = if audio_expected {
        accepted_vbuf >= PRIME_NS && accepted_abuf >= PRIME_AUDIO_NS
    } else {
        accepted_vbuf >= PRIME_VIDEO_MAX_NS
    };
    let recovery = shared.hls_recovery();
    // A downshift has no discretionary trial reserve, yet it can queue candidate AUs while an
    // internal hold is active. Validate one seqlock generation around the ENTIRE tail/decoder/
    // recovery snapshot: an odd or changed value means candidate publication overlapped it. A
    // bool cannot prove this because a complete false->true->false transition is an ABA. Re-read
    // the up-trial gate at the same final boundary so neither transaction type has a check/use gap.
    if shared.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        || !hls_candidate_snapshot_stable(
            candidate_generation,
            shared.hls_candidate_generation.load(Ordering::Acquire),
        )
    {
        return;
    }
    let balanced_runway_ready = if audio_expected {
        playable_vbuf >= observed_runway_ns && playable_abuf >= observed_runway_ns
    } else {
        playable_vbuf >= observed_runway_ns
    };
    let boundary_ready = match prime_kind {
        HlsPrimeKind::Rebuffer => {
            let playable = if audio_expected {
                playable_vbuf.min(playable_abuf)
            } else {
                playable_vbuf
            };
            recovery.ready(playable)
        }
        HlsPrimeKind::Fresh => balanced_runway_ready,
        // A viewer Resume continues an existing timeline, not a replay of every acquisition
        // since the rung was selected. That historical worst-permutation reserve can exceed
        // native + AU queue capacity (#1216); no further media can arrive until Play. Keep the
        // accepted A/V decoder prime below, but do not make user intent repay historical stress.
        // An automatic rebuffer remains Rebuffer even across user Pause/Resume and must still
        // close its own pause-local recovery epoch above.
        HlsPrimeKind::Resume => true,
    };
    if decoder_ready && boundary_ready && transport.feed_allowed() {
        let Some((play_token, recovery)) =
            shared.reserve_hls_prime_play(prime_kind, candidate_generation, recovery)
        else {
            return;
        };
        // A paused preroll plays one picture for a viewer who is paused: keep that window silent.
        if transport.seek_preroll_active() {
            (hooks.mute_for_preroll)(mt);
            // The mute is the feed's own record (the restore gives back what was saved, which is
            // always the pipeline default: the volume is never read back).
            transport.note_preroll_muted(plx_platform::tv::sink::PIPELINE_DEFAULT_VOLUME);
        }
        // Snapshot before the call so a callback delivered synchronously by Play counts, but
        // one queued during Flush/Segment does not establish the new clock by itself.
        let landing = eng.segment_landing.as_mut().filter(|seek| seek.play.is_none());
        let baseline = landing.is_some().then(|| crate::feed_state::SegmentPlay {
            position_tick: shared.dg_pos_ticks.load(Ordering::Acquire),
            displayed: shared.sink_displayed_sequence.load(Ordering::Acquire),
            waiting_since: None,
        });
        let played = unsafe { (hooks.sink)().play(mt) };
        match shared.complete_hls_prime_play(play_token, played != 0) {
            HlsPlayCompletion::Accepted { resume_acb } => {
                if resume_acb {
                    (hooks.mirror_playstate)(mt, eng.stage, true);
                }
                eng.prime_play = false;
                shared.seeking.store(false, Ordering::Relaxed); // playback resumed at the new position → HUD spinner off
                if let Some(seek) = landing {
                    seek.play = baseline;
                } else if eng.segment_landing.is_none() {
                    shared.seek_landed_epoch.store(shared.seek_epoch.load(Ordering::Relaxed), Ordering::Release);
                }
                if prime_kind == HlsPrimeKind::Rebuffer {
                    plx_base::eventlog::log(&format!(
                        "primed: v={}ms a={}ms recovery_n={} debt={}ms runway={}ms -> Play",
                        playable_vbuf / 1_000_000,
                        playable_abuf / 1_000_000,
                        recovery.completed,
                        recovery.debt_us / 1_000,
                        recovery.runway_us / 1_000,
                    ));
                } else {
                    plx_base::eventlog::log(&format!(
                        "primed: v={}ms a={}ms runway={}ms -> Play",
                        playable_vbuf / 1_000_000,
                        playable_abuf / 1_000_000,
                        observed_runway_ns / 1_000_000,
                    ));
                }
            }
            HlsPlayCompletion::Refused => {
                if eng.startup_controlled && !eng.startup_first_play_done {
                    eng.startup_play_refused = true;
                    eng.startup_play_commanded = false;
                    plx_base::eventlog::log("primed: Starfish refused initial Play; common startup must settle it");
                } else {
                    plx_base::eventlog::log("primed: Starfish refused Play; keeping the clock held and retrying");
                }
            }
            HlsPlayCompletion::Stale => {
                plx_base::eventlog::log("primed: Play result lost its clock token; teardown/transition won");
            }
        }
    }
}

/// **One tick's feeding, as a unit: video lane, audio lane, then the prime attempt.**
///
/// The order is load-bearing and predates this function — the VIDEO lane owns the seek rebase, so
/// it must clear `rebase_pending` and publish `pts_shift` before the audio lane reads them in the
/// same tick. What is new is the third step.
///
/// [`try_prime`] is called HERE rather than at the end of [`feed_audio_lane`] because that
/// function returns early on `rebase_pending`, and a tail call would then be skipped on exactly
/// the ticks a reload passes through.
///
/// **The first version of this doc justified the placement with the audioless case, and that
/// argument is FALSE.** Every `Source::Stream` allocates BOTH queues whether or not audio is
/// produced, so `aq_audio == None` is not a state a real stream reaches; and with no audio a
/// 2-second segment cannot satisfy the `PRIME_VIDEO_MAX_NS` escape from this call either, so the
/// placement neither helps nor harms it. The reason for the wrapper is that one function should
/// own what a tick does to the lanes — not that it rescues a case it never sees.
/// **The two lane feeders are PRIVATE so that this is the only way in.** A review found the hole:
/// with them `pub`, reverting `pump` to two separate lane calls restored the livelock while
/// the regression test — which calls this wrapper — stayed green. Private, that revert does not
/// compile.
pub fn feed_both_lanes(mt: &MainThread, eng: &mut Engine, transport: &Transport, hooks: &FeedHooks) {
    feed_stream(mt, eng, transport, hooks);
    feed_audio_lane(mt, eng, hooks);
    try_prime(mt, eng, transport, hooks);
}

/// Meet the landing keyframe (`kf_pts`, the access unit's `key`) with the Dolby Vision guard. True when
/// the keyframe must be held back: a withdrawal is pending and the player's driver reloads without the
/// node. A report-only guard logs its line here and the keyframe feeds as before.
fn meet_dv_landing(eng: &mut Engine, kf_pts: i64, key: c_int) -> bool {
    use crate::dv_start::{Landing, Step};
    match eng.dv_start.land(&Landing { kf_pts, key, in_place: eng.flushed }) {
        Step::Quiet => false,
        Step::Report(r) => {
            plx_base::eventlog::log(&r.line());
            false
        }
        Step::Withdraw(_) => true,
    }
}

fn feed_stream(mt: &MainThread, eng: &mut Engine, transport: &Transport, hooks: &FeedHooks) {
    if eng.dv_start.landing_held() {
        return; // the landing keyframe is held until the player's driver reloads without the node
    }
    let shared = eng.shared;
    let qp = match eng.aq_video.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    let slice = FeedSlice::begin(hooks.feed_clock_us);
    let mut fed = 0;
    while fed < 120 {
        // Feed each AU, throttled to ~MAX_FEED_AHEAD_NS ahead of the presented position per lane
        // (see the throttle below) rather than greedily to BufferFull.
        if eng.pending_video.is_none() {
            let mut eof: c_int = 0;
            let n = aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                // true EOF (producer done + video lane drained): signal end-of-stream ONCE so the
                // pipeline drains its last frames instead of hanging on them (Kodi keys EOS to the
                // video drain). Keyed on the video lane only.
                if eof != 0 && !eng.eos_pushed && eng.stage >= Stage::Streaming {
                    unsafe { (hooks.sink)().push_eos(mt) };
                    eng.eos_pushed = true;
                    plx_base::eventlog::log("EOS pushed at true EOF");
                }
                // Nothing to send. THE case the read-out's Feed row exists to name: a dead
                // PRODUCER, which every other field on the panel cannot tell from a dead sink.
                shared.dg_feed_state.store(5, Ordering::Relaxed);
                break;
            }
            eng.pending_video = Some(AuBox(n));
        }
        let n = eng.pending_video.as_ref().unwrap().0;
        let (es, key, pts, len, data) = unsafe { aq::au_fields(n) };
        // An Engine started with no seek armed has no rebase to carry its landing: its first video
        // keyframe is the one the Dolby Vision guard asks about ([`crate::dv_start`]).
        if !eng.rebase_pending && es == 1 && key != 0 && eng.dv_start.awaiting_landing() && meet_dv_landing(eng, pts, key) {
            break;
        }
        if eng.rebase_pending {
            if es == 1 && key != 0 {
                let completing_in_place_seek = eng.flushed;
                let mut segment_installed = false;
                // In-place seek: drop a keyframe that landed well AHEAD of the target — it's a stale
                // frame from the pre-flush read position (the reopen+av_seek hasn't taken effect yet),
                // not the real post-seek keyframe. Capped so a failed av_seek can't hang the rebase.
                if eng.flushed {
                    let target = shared.seek_target_ns.load(Ordering::Relaxed);
                    let stale = target >= 0
                        && (pts - target > SEEK_STALE_AHEAD_NS
                            || target - pts > SEEK_STALE_BEHIND_NS);
                    if stale && eng.rebase_drops < MAX_REBASE_DROPS {
                        eng.rebase_drops += 1;
                        if eng.rebase_drops == 1 {
                            plx_base::eventlog::log(&format!(
                                "rebase: dropping stale kf pts={}s (target {}s)",
                                pts / 1_000_000_000,
                                target / 1_000_000_000
                            ));
                        }
                        eng.pending_video = None;
                        continue;
                    }
                }
                // The landing, as the Dolby Vision guard meets it: AFTER the stale-keyframe drop (a
                // stale keyframe is no landing) and BEFORE any `Seek`, `setTimeToDecode` or `pts_shift`
                // write, so a withdrawn landing leaves the pipeline untouched. Not for a flushed
                // keyframe in `Reload` mode: it anchors nothing and the pump's own reload is on its way.
                if !(eng.flushed && crate::seek::seek_mode() == SeekMode::Reload) && meet_dv_landing(eng, pts, key) {
                    break;
                }
                if eng.flushed && crate::seek::seek_mode() == SeekMode::NativeSeek {
                    // NATIVE in-place seek (#568): feed the REAL content PTS (pts_shift=0,
                    // disp_base=0 -> playpos = presented real pts = content time, as in the
                    // Segment branch below) and anchor the pipeline with LG's public
                    // `StarfishMediaAPIs::Seek(millis)`. Seek is a flushing `gst_element_seek`
                    // whose new segment starts at EXACTLY the millis passed, so the PTS fed from
                    // here on is on that timeline by construction; the pump skipped its own
                    // `flush()` for this mode and nothing may flush after this (a `flush()` is
                    // itself a seek to 0 and re-creates the stale-segment stall). `feed_stream`
                    // runs on the main thread and is the only Feed caller, and this keyframe is
                    // not fed yet, so Seek cannot race a Feed. Never a negative millis: Seek
                    // refuses it, and a negative PTS is unparseable by Feed.
                    shared.pts_shift.store(0, Ordering::Relaxed);
                    let ms = to_sink_ns(pts.max(0), shared) / 1_000_000;
                    let rv = unsafe { (hooks.sink)().seek(mt, ms) };
                    plx_base::eventlog::log(&format!("in-place seek(native): Seek({ms}) rv={rv}"));
                    if rv == 0 {
                        // A refused Seek can leave the pipeline refusing every Feed. Drop the rest
                        // of THIS SESSION's seeks to the reload path and redo THIS one as a
                        // reload at the user's target (the pump reads the mailbox next tick;
                        // `reload_at` REPLACES the engine). `eng.flushed`/`rebase_pending` stay
                        // set so the audio lane keeps holding and no AU reaches a pipeline that
                        // is rejecting Feed.
                        crate::seek::set_seek_mode(SeekMode::Reload);
                        // Latched across the reload this very seek is about to do, so the fresh
                        // session's `select_seek_mode` does not offer the same firmware another
                        // refused Seek on every later seek of this playback.
                        crate::seek::set_native_seek_refused(true);
                        plx_base::eventlog::log(
                            "in-place seek DOWNGRADE: native Seek refused -> reload-per-seek for \
                             the rest of this playback",
                        );
                        let tgt = shared.seek_target_ns.load(Ordering::Relaxed).max(0);
                        // Tell the player (it adopts a new epoch as ONE pending seek, which the
                        // pump takes and reloads at), and keep the feed's own copy armed: it is
                        // what holds the AU gate shut while the pipeline refuses Feed.
                        shared.seek_requeue_target.store(tgt, Ordering::Relaxed);
                        shared.seek_requeue_epoch.fetch_add(1, Ordering::Release);
                        transport.seek_to_ns.store(tgt, Ordering::Release);
                        eng.pending_video = None;
                        break;
                    }
                    segment_installed = true;
                    eng.flushed = false;
                } else if eng.flushed && crate::seek::seek_mode() != SeekMode::Segment {
                    // Flushed but not in a mode that anchors in place: a refused native `Seek`
                    // downgraded the session to `Reload` and the pump's reload (`reload_at`)
                    // is on its way. The private Segment path (`sf_pipeline`'s decompile-derived
                    // offsets, proven on webOS 4.5 only) must NEVER run here, so this keyframe
                    // anchors nothing: drop it and leave `flushed`/`rebase_pending` set, which
                    // keeps the audio lane holding until the reload replaces the engine.
                    eng.pending_video = None;
                    break;
                } else if eng.flushed {
                    // Kodi IN-PLACE seek (exact): feed the REAL content PTS (no rebase), tell the
                    // pipeline the real decode position, then inject a fresh GStreamer SEGMENT —
                    // this re-anchors the sink WITHOUT a reload/decoder re-init. disp_base=0 +
                    // pts_shift=0 → playpos = presented real pts = content time.
                    shared.pts_shift.store(0, Ordering::Relaxed);
                    let ok = unsafe { (hooks.sink)().set_time_to_decode(mt, to_sink_ns(pts, shared)) };
                    // setTimeToDecode returns 0 on webOS<11 (it needs PausedState); fall back to
                    // the content-info path (loadSpi_getInfo + setContentInfo(ptsToDecode)), which
                    // re-anchors the decode position while Playing. Then always inject the fresh
                    // GStreamer SEGMENT so the sink re-bases instead of stalling.
                    let ci = if ok == 0 {
                        unsafe { (hooks.sink)().set_content_info(mt, to_sink_ns(pts, shared)) }
                    } else {
                        1
                    };
                    let seg = unsafe { (hooks.sink)().send_segment(mt) };
                    segment_installed = seg != 0;
                    plx_base::eventlog::log(&format!("in-place seek: setTimeToDecode({pts}) rv={ok} setContentInfo={ci} sendSegment={seg}"));
                    if seg == 0 {
                        // The pipeline ptr wasn't reachable, so NO fresh GStreamer segment was
                        // injected and THIS seek's buffers are scheduled against the pre-seek
                        // segment — the stale-segment state that stops the sink draining and
                        // fills ~14.7 MB of upstream buffers in ~48 s (permanent BufferFull +
                        // "Playing error"; see reload_at's decompiled ground truth). So drop the
                        // rest of THIS SESSION's seeks to the reload path, which rebuilds the
                        // segment by construction; start_bufferfeed re-arms the flag for the next
                        // session (the SCOPE note in mod.rs argues why that is the right scope).
                        // Logged loudly because this is the single most consequential state
                        // change in the seek path and it used to be invisible: instant seeks
                        // silently became multi-second reloads with nothing in the event log
                        // naming the cause — only the `sendSegment=0` above, whose consequence
                        // was not stated anywhere.
                        crate::seek::set_seek_mode(SeekMode::Reload);
                        plx_base::eventlog::log(
                            "in-place seek DOWNGRADE: sendSegment=0 (CustomPipeline unreachable) \
                             -> reload-per-seek for the rest of this session",
                        );
                    }
                    eng.flushed = false;
                } else {
                    // reload / initial-resume seek: rebase the landed keyframe to fed-pts 0 (the
                    // fresh Load's pipeline expects a 0-based feed; disp_base carries the offset).
                    shared.pts_shift.store(-pts, Ordering::Relaxed);
                }
                eng.rebase_pending = false; // releases the AUDIO lane (which holds until this clears)
                eng.seek_base_pts = pts + shared.pts_shift.load(Ordering::Relaxed); // fed-pts base
                                                                                    // Installing a SEGMENT is not acceptance of the AU which establishes it. Keep
                                                                                    // callbacks fenced across BufferFull/error and arm only below, after this exact
                                                                                    // retained keyframe has returned `O` from sf_feed.
                eng.presentation_rearm_pending = completing_in_place_seek && segment_installed;
                plx_base::eventlog::log(&format!(
                    "rebase: first post-seek keyframe pts={pts} -> pts_shift={} irap={}",
                    shared.pts_shift.load(Ordering::Relaxed),
                    aq::hevc_irap_not_idr(key).map_or_else(|| "IDR".to_string(), aq::hevc_irap_label),
                ));
            } else {
                eng.pending_video = None; // drop pre-keyframe AUs
                continue;
            }
        }
        let mut fp = pts + shared.pts_shift.load(Ordering::Relaxed) + pts_nudge_ns();
        if fp < eng.max_fed_video_pts - STALE_BACKJUMP_NS {
            eng.pending_video = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        // Feed-ahead throttle: don't feed an AU that's already more than its lane's budget ahead
        // of the presented position — keep it pending and retry once the pipeline presents more.
        // Skipped while priming (to reach PRIME_NS before Play; the per-tick slice below is waived
        // until the lane holds its prime depth, then spreads the rest). Each lane's queue is
        // pts-ordered, so if the head is over budget everything behind it is too; breaking is right.
        let pres = eng.feed_pace.reference_at(
            shared.pres_fed.load(Ordering::Relaxed),
            crate::shared::vclock_ms(),
            shared.applied_rate_permille.load(Ordering::Relaxed),
        );
        if !eng.prime_play {
            let budget = if es == 1 {
                MAX_FEED_AHEAD_NS
            } else {
                MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS
            };
            if pres != PRES_NONE && fp - pres > budget {
                shared.dg_feed_state.store(4, Ordering::Relaxed); // waiting for a frame
                break;
            }
        }
        // Out of this tick's slice: the AU stays pending, as on a throttle stop, for the next tick.
        // A lane short of media is exempt: it is priming or its lead is nearly gone.
        let starved = lane_starved(
            eng.prime_play,
            eng.max_fed_video_pts,
            eng.seek_base_pts,
            PRIME_NS,
            pres,
        );
        if slice.spent(fed, starved, hooks.feed_clock_us) {
            break;
        }
        let presentation_probe = if eng.presentation_rearm_pending {
            let armed = shared.begin_native_presentation_probe(eng.native_epoch);
            if !armed {
                plx_base::eventlog::log("rebase: native presentation epoch retired before post-seek Feed");
            }
            armed
        } else {
            false
        };
        let sink_pts = to_sink_ns(fp, shared);
        let r = plx_base::diag::spans::span("sfv", || unsafe { (hooks.sink)().feed(mt, data, len as u32, sink_pts, es) });
        if presentation_probe {
            if (r as u8) == b'O' {
                if !shared.commit_native_presentation_probe(eng.native_epoch, |num| {
                    crate::sf_event::sf_on_event_inner(0, num, std::ptr::null(), crate::shared::EventScope::Live)
                }) {
                    plx_base::eventlog::log("rebase: native presentation probe lost its epoch after accepted Feed");
                }
            } else if !shared.reject_native_presentation_probe(eng.native_epoch) {
                plx_base::eventlog::log("rebase: native presentation probe lost its epoch after rejected Feed");
            }
        }
        // **Only an ACCEPTED feed advances the mark.** `max_fed_*_pts` is the high-water of what
        // the PIPELINE holds and `try_prime` starts the clock off it, so counting a rejected AU
        // primes against media the decoder never received. This advanced before the reply was read
        // for as long as the check lived inside this loop — where it was video-only, and one
        // frame early. `feed_both_lanes` made the same slip reachable from the AUDIO lane, which
        // the old check could not prime from at all, so the latent sloppiness became a defect.
        if (r as u8) == b'O' && fp > eng.max_fed_video_pts {
            eng.max_fed_video_pts = fp;
        }
        // prime-then-play: once PRIME_NS of the fresh (post-seek/resume) stream is buffered,
        // start the clock. The pipeline was paused through the reopen gap, so it now presents
        // from the seek point in A/V sync instead of fast-forwarding to a clock that ran ahead.
        // Start the clock once BOTH lanes are buffered past the seek base: video to PRIME_NS AND
        // audio to PRIME_AUDIO_NS. Priming on video ALONE started the audioSync MASTER clock with
        // an empty audio queue, so a rapid-seek drain could leave audio silent until the next seek.
        // The video-buffer fallback still starts an audioless/briefly-starved stream (no hang).
        try_prime(mt, eng, transport, hooks);
        // The log cadence counts ATTEMPTS (its `reply=` field is the only record of a rejected
        // feed, and tests/run.py greps `feed v#`), so it keeps its own counter. VTOT counts what
        // was ACCEPTED — see below.
        if es == 1 {
            let v = VATT.fetch_add(1, Ordering::Relaxed) + 1;
            if v <= 4 || v % 100 == 0 {
                let qb = aq::aq_bytes(qp);
                plx_base::eventlog::log(&format!(
                    "feed v#{v} sz={len} fed={fp} reply={} qbytes={qb}",
                    r as u8 as char
                ));
            }
        }
        if (r as u8) != b'O' {
            // 'B' BufferFull -> keep pending, retry next tick (VIDEO lane only)
            shared
                .dg_feed_state
                .store(if (r as u8) == b'B' { 2 } else { 3 }, Ordering::Relaxed);
            break;
        }
        if eng.presentation_rearm_pending {
            eng.presentation_rearm_pending = false;
        }
        shared.dg_feed_state.store(1, Ordering::Relaxed); // accepting
                                                          // COUNTED HERE, below the reply test, so `Fed` means AUs the pipeline TOOK. Counted above
                                                          // it, a permanently-full sink re-offered and re-counted the same retained AU every tick,
                                                          // and the read-out showed a brisk feed rate through a stall.
        if es == 1 {
            VTOT.fetch_add(1, Ordering::Relaxed);
        }
        eng.pending_video = None;
        fed += 1;
    }
}

/// Whether an audio AU at fed pts `fp` is in its timeline's lead-in: it is the AU the demuxer
/// marked as the first since its open or seek (`opens`), or lies within [`AUDIO_LEAD_IN_NS`] past
/// the one that was accepted (`start`). Lead-in audio is fed ahead of the clock: neither the
/// stale-ahead drop nor the feed-ahead throttle applies to it.
pub fn audio_lead_in(fp: i64, opens: bool, start: Option<i64>) -> bool {
    opens || start.is_some_and(|s| fp >= s && fp - s < AUDIO_LEAD_IN_NS)
}

/// AUDIO lane feeder (two-lane ff path only). Independent of the video lane: its own queue, its
/// own fed-pts high-water, its own BufferFull retry — so a video BufferFull never starves audio.
/// HOLDS while a seek rebase is pending (the VIDEO lane sets pts_shift on its first post-seek
/// keyframe; feeding audio before that would use a stale shift → A/V desync). No prime/Play here —
/// only the video lane starts the clock. Called AFTER feed_stream each tick, so a same-tick rebase
/// is already visible.
fn feed_audio_lane(mt: &MainThread, eng: &mut Engine, hooks: &FeedHooks) {
    let shared = eng.shared;
    if eng.rebase_pending || eng.dv_start.landing_held() {
        return; // wait for the video lane to publish pts_shift (or for the withdrawal's reload)
    }
    if !settle_pcm_lead(eng) {
        return; // the output's lead is not here yet: stamping with a guess would move later
    }
    let qp = match eng.aq_audio.as_mut() {
        Some(q) => &mut **q as *mut AuQueue,
        None => return,
    };
    // hoisted out of the loop: pts_shift is stable once rebase clears (only the video lane's rebase
    // arm writes it, on this same thread), and one pres_fed sample per tick is plenty against the
    // multi-second audio budget.
    let shift = shared.pts_shift.load(Ordering::Relaxed);
    let pres = eng.feed_pace.reference_at(
        shared.pres_fed.load(Ordering::Relaxed),
        crate::shared::vclock_ms(),
        shared.applied_rate_permille.load(Ordering::Relaxed),
    );
    let slice = FeedSlice::begin(hooks.feed_clock_us);
    let mut fed = 0;
    while fed < 120 {
        if eng.pending_audio.is_none() {
            let mut eof: c_int = 0;
            let n = aq::aq_pop(qp, &mut eof);
            if n.is_null() {
                break;
            }
            eng.pending_audio = Some(AuBox(n));
        }
        let n = eng.pending_audio.as_ref().unwrap().0;
        let (es, _key, pts, len, data) = unsafe { aq::au_fields(n) };
        let mut fp = pts + shift;
        let first_pts = shared.first_audio_pts.load(Ordering::Acquire);
        let opens = first_pts != i64::MIN && pts == first_pts;
        let lead_in = audio_lead_in(fp, opens, eng.audio_start_fp);
        // Stale drifted audio from before a seek's reopen: far AHEAD of the freshly-anchored video.
        // Drop it (else it poisons max_fed_audio_pts and stalls the audio clock → playback sticks).
        // A late-starting track is far ahead too, legitimately, so it is measured from its own
        // start as well as from the video (see AUDIO_LEAD_IN_NS).
        let stale_ref = eng.max_fed_video_pts.max(eng.audio_start_fp.unwrap_or(i64::MIN));
        if !lead_in && eng.max_fed_video_pts > 0 && fp > stale_ref + AUDIO_STALE_AHEAD_NS {
            eng.pending_audio = None;
            continue;
        }
        if fp < eng.max_fed_audio_pts - STALE_BACKJUMP_NS {
            eng.pending_audio = None; // stale (a big backward jump)
            continue;
        }
        if fp < 0 {
            fp = 0;
        }
        if !eng.prime_play
            && !lead_in
            && pres != PRES_NONE
            && fp - pres > MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS
        {
            break;
        }
        let starved = lane_starved(eng.prime_play, eng.max_fed_audio_pts, eng.seek_base_pts, PRIME_AUDIO_NS, pres);
        if slice.spent(fed, starved, hooks.feed_clock_us) {
            break;
        }
        let sink_pts = audio_sink_pts(fp, eng.pcm_sink_lead_ns, shared.applied_rate_permille.load(Ordering::Relaxed));
        let r = plx_base::diag::spans::span("sfa", || unsafe { (hooks.sink)().feed(mt, data, len as u32, sink_pts, es) });
        // Accepted only — see the video lane's note.
        if (r as u8) == b'O' && fp > eng.max_fed_audio_pts {
            eng.max_fed_audio_pts = fp;
        }
        if (r as u8) == b'O' && opens {
            if eng.audio_start_fp.is_none() && fp - eng.max_fed_video_pts > MAX_FEED_AHEAD_NS {
                plx_base::eventlog::log(&format!(
                    "feed: audio starts {}ms after the video fed so far; fed ahead of the clock",
                    (fp - eng.max_fed_video_pts) / 1_000_000
                ));
            }
            eng.audio_start_fp = Some(fp);
        }
        let a = AATT.fetch_add(1, Ordering::Relaxed) + 1;
        if a <= 4 || a % 200 == 0 {
            let qb = aq::aq_bytes(qp);
            plx_base::eventlog::log(&format!(
                "feed a#{a} sz={len} fed={fp} reply={} qbytes={qb}",
                r as u8 as char
            ));
        }
        if (r as u8) == b'O' {
            ATOT.fetch_add(1, Ordering::Relaxed); // accepted, not attempted — see feed_stream
            if let Some(line) = record_native_audio_packet_size(len as u32) {
                plx_base::eventlog::log(&format!("native audio: packet sizes of the first AUs fed: {line}"));
            }
        }
        if (r as u8) != b'O' {
            break; // 'B' BufferFull -> keep pending, retry next tick (AUDIO lane only)
        }
        eng.pending_audio = None;
        fed += 1;
    }
}

/// The sink envelope for THIS Load, from the route (main thread), with the dev override applied.
pub fn sink_envelope_now(facts: &crate::PayloadFacts, video: crate::LoadVideo, rate: i32) -> crate::SinkEnvelope {
    if let Some(spec) = plx_base::devtrig::read("sinkmax") {
        if let Some(env) = crate::parse_sinkmax(&spec) {
            #[cfg(feature = "devtriggers")]
            plx_base::eventlog::log(&format!(
                "sinkmax: envelope OVERRIDDEN to {}x{}@{} by /tmp/plxnative-sinkmax",
                env.w, env.h, env.fps
            ));
            return env;
        }
        plx_base::eventlog::log(&format!("sinkmax: unparseable spec {spec:?} — ignored"));
    }
    crate::sink_envelope(
        video,
        facts.max_raster,
        crate::fed_fps(facts.fps, rate),
        &plx_platform::devcaps::caps(),
        plx_platform::devcaps::measured() && !facts.force_direct_play,
    )
}

/// The audio name and diagnostic code a Load declares for `codec`: PCM whenever the app decodes the
/// stream itself (`pcm` is the shape supplied by the route), whatever the codec
/// is — DTS is `DTS` only when the set's own decoder takes it — else [`audio_payload_codec`]'s.
/// PURE.
pub fn audio_declaration(codec: &str, pcm: bool) -> Option<(&'static str, u8)> {
    if pcm {
        return Some(("PCM", 5));
    }
    crate::audio_payload_codec(codec)
}


/// What a Load declares for its audio, and the plan for the sink lead that follows from it: ONE
/// value, so the payload's `"audio"` name and the Engine's lead cannot disagree. PURE.
pub struct LoadAudio {
    /// The LG-side name the payload carries; `"PCM"` for app-decoded audio.
    pub name: &'static str,
    /// The `dg_load_a` diagnostics code for the codec (`audio_payload_codec`).
    pub diagnostic: u8,
    /// [`pcm_lead_plan`] of `name` on the set's stack.
    pub lead: PcmLead,
}

/// [`LoadAudio`] for a track of `codec` at `session_rate` (permille); `None` only at 1x, when the pipeline has
/// no name for the codec and the app does not decode it. A speed session decodes and stretches the sound in the app
/// (`ff::SpeedDecoder`), so at any speed but 1x every codec declares `"PCM"`; so does a track the app
/// decodes itself at 1x (`app_pcm`, the route's declared shape: DTS under the
/// lossy gate, FLAC and the like), whatever its codec, and that name is what the lead plan is made
/// from, so every Load that declares `"audio":"PCM"` is planned a lead. `lead_override_ms` is
/// the dev trigger's text and `stack` the set's audio stack ([`plx_platform::devcaps::audio_stack`]).
pub fn load_audio(
    codec: &str, session_rate: i32, app_pcm: bool, lead_override_ms: Option<&str>,
    stack: AudioStack,
) -> Option<LoadAudio> {
    let declared = audio_declaration(codec, app_pcm);
    // A speed session declares PCM whatever the codec is called, so it needs no payload name for it
    // (`mp3`, `vorbis` and `truehd` have none): its diagnostic code stays the codec's when it has one.
    let (name, diagnostic) = if session_rate != crate::shared::RATE_NORMAL {
        ("PCM", declared.map_or(5, |(_, code)| code))
    } else {
        declared?
    };
    Some(LoadAudio { name, diagnostic, lead: pcm_lead_plan(name, lead_override_ms, stack) })
}



pub fn build_av_payload_from_facts(
    facts: &crate::PayloadFacts, video: &str, audio: &str, env: crate::SinkEnvelope, rate: i32,
    options: crate::PayloadOptions,
) -> String {
    let built = crate::build_av_payload(
        facts, video, audio, env, rate, options,
    );
    for line in built.notices {
        plx_base::eventlog::log(&line);
    }
    built.json
}



/// `"appId":"<this install's id>"` — the payload key, and the anchor `with_window_id` splices onto.
///
/// One expression, called from both places, so the two cannot disagree.
pub fn app_id_key() -> String {
    format!(r#""appId":"{}""#, plx_base::paths::app_id())
}

/// Substitute the `@APPID@` placeholder with the id of the install this process actually is.
///
/// Exactly once per payload, asserted at build time: a `replace` that matched twice would leave a
/// second `appId` key for `with_window_id` to splice a duplicate `windowId` onto, and one that
/// matched nothing would hand LG's JSON parser the placeholder as a literal app id. Both are
/// silent — the pipeline reports no error for either — which is why this is graded in the host
/// suite rather than left to be noticed on a television.
pub fn with_app_id(p: &str) -> String {
    p.replace("@APPID@", plx_base::paths::app_id())
}

/// Create the exported window and splice its id into the Load payload — the webOS 5+ binding, in
/// one place.
///
/// **Session-scoped, and that is the point.** The window is created HERE rather than at boot
/// because `teardown` destroys it, and the two ends have to sit at the same level: created once at
/// boot and destroyed per session, the second play would splice nothing and show no picture, in
/// silence. Creating it beside its only consumer also makes the "must exist before Load" ordering
/// locally visible instead of a promise made across two files.
///
/// On webOS 5 this one string IS the video-plane binding: the compositor assigned it to the window
/// we just created, and the pipeline imports that window by name and punches through to it.
/// Everything else about the BUFFERSTREAM payload is unchanged between the eras — which is what
/// both reference implementations show (ss4s compiles the same payload builder for webOS 3, 4 and
/// 5 and swaps only the resource file; Kodi adds this key and nothing else).
///
/// Inserted after `"appId":"…"` because that is a stable sibling inside `option` in every payload
/// variant here — via [`app_id_key`], so the anchor cannot drift from what `with_app_id` composed.
/// A no-op on every webOS 4.x set, where `vp_create_window` returns NULL.
pub fn with_window_id(mt: &MainThread, p: &str, hooks: &PrepareHooks) -> String {
    if (hooks.sink)().window_mode() != plx_platform::tv::sink::VP_EXPORTED {
        return p.to_string();
    }
    let id = unsafe { (hooks.sink)().create_window(mt) };
    (hooks.on_exported_window_create)();
    if id.is_null() {
        plx_base::eventlog::log("windowId: no exported window — video will not bind");
        return p.to_string();
    }
    let id = unsafe { std::ffi::CStr::from_ptr(id) }.to_string_lossy();
    let anchor = app_id_key();
    if !p.contains(&anchor) {
        plx_base::eventlog::log("windowId: payload has no appId anchor — NOT spliced; video will not bind");
        return p.to_string();
    }
    plx_base::eventlog::log(&format!(
        "vplane: exported windowId={id} spliced into the Load payload"
    ));
    p.replace(&anchor, &format!(r#"{anchor},"windowId":"{id}""#))
}



/// Resolve the physical Load payload and arm its retractable clock/native epochs. The
/// already-selected route supplies only frozen source facts; no route or Plex state is read here.
pub fn prepare_native_open(
    mt: &MainThread,
    shared: &'static PhysicalShared,
    user_held: bool,
    seek_preroll: bool,
    open: Option<PhysicalOpen>,
    sample: Option<Box<SampleBuf>>,
    is_h265: bool,
    session_rate: i32,
    side_reader: Box<dyn SideReaderPort>,
    no_audio: bool,
    dv_landing: plx_playback_contract::feed::DvLandingPlan,
    hooks: &PrepareHooks,
) -> Result<NativePrepared, PrepareRefusal> {
    let stream = sample.is_none();
    // For a streamed direct-play/transcode, pick the Load codecs from the item: video H264 vs
    // H265 (native HEVC direct-play), audio AC3/EAC3/AAC/DTS. (The local sample paths keep their
    // fixed payloads.)
    // dev A/B: /tmp/plxnative-noaudio feeds video only (needAudio:false + skip es=2) to isolate
    // whether the audio ES (E-AC3/Atmos) is what stalls the sink on 4K HEVC.
    crate::set_feed_audio(!no_audio);
    let stream_payload;
    // Every arm below assigns this — the static payloads through `static_envelope`, the streamed
    // one through `sink_envelope_now` — so a new arm that forgets it fails to compile.
    let sink_env: crate::SinkEnvelope;
    // The LG-side audio name the payload ended up carrying, hoisted so the `load:` line below can
    // report it. "-" is the video-only case, where there is no audio ES to name.
    let mut audio_declared: &str = "-";
    // ...and the plan for the sink lead that declaration carries (`load_audio`), none for a
    // compressed or absent track.
    let mut audio_lead = PcmLead::Off;
    // ...and the video name beside it, for the same reason and to keep the codec->name mapping
    // written ONCE (`LoadVideo`): the `load:` line below would otherwise re-read the route and
    // re-derive it.
    let mut video_declared: &str = "-";
    // Playback speed for THIS Load (`player::session_rate_for`), published before the demuxer is
    // spawned: it reads it to decide whether to decode the audio, and every timestamp crossing to
    // the pipeline is scaled by it (`shared::to_sink_ns`). After any `reset_session`, which puts
    // it back to 1x.
    shared.applied_rate_permille.store(session_rate, Ordering::Relaxed);
    if session_rate != crate::shared::RATE_NORMAL {
        plx_base::eventlog::log(&format!("speed: this Load plays at {session_rate} (audio decoded in the app, fed as PCM)"));
    }
    let payload_str: &str = if stream {
        let open = open.as_ref().expect("stream request was captured");
        let stream_vcodec = &open.video_codec;
        let Some(lv) = crate::LoadVideo::of_stream(stream_vcodec) else {
            plx_base::eventlog::log(&format!(
                "start_bufferfeed: the pipeline cannot feed video codec {stream_vcodec:?}; refusing the Load instead of declaring it H.264"
            ));
            shared.fail_demux_latch(hooks.load_refusal_latch);
            return Err(PrepareRefusal::Refused);
        };
        if crate::load_video_refused(lv, &plx_platform::devcaps::caps()) {
            plx_base::eventlog::log(&format!(
                "start_bufferfeed: {} stream but the device table lists no such decoder; refusing the Load",
                lv.payload_name()
            ));
            shared.fail_demux_latch(hooks.load_refusal_latch);
            return Err(PrepareRefusal::Refused);
        }
        video_declared = lv.payload_name();
        // Record what the payload ACTUALLY says, for the diagnostics read-out — including the
        // video-only case, where `dg_load_a == 0` is the whole explanation for silence.
        shared.dg_load_v.store(lv.diag_code(), Ordering::Relaxed);
        if no_audio {
            shared.dg_load_a.store(0, Ordering::Relaxed);
            // A static payload declares its own envelope; say so on the `load:` line.
            sink_env = crate::static_envelope(lv);
            match lv {
                crate::LoadVideo::H264 => crate::PAYLOAD_V,
                crate::LoadVideo::H265 => crate::PAYLOAD_H265,
                crate::LoadVideo::Av1 | crate::LoadVideo::Vp9 => {
                    stream_payload = crate::renamed_static_payload(lv);
                    stream_payload.as_str()
                }
            }
        } else {
            // LG's pipeline names E-AC3 "AC3 PLUS" (Dolby Digital Plus), NOT "EAC3" — the
            // wrong string leaves the audio ES unconfigured, and with audioSync the video
            // sink slaves to the dead audio clock and stalls (verified: video-only plays).
            let codec = &open.audio_codec;
            // App-decoded audio: the Load must state the PCM shape, and the demuxer must produce
            // exactly that. Missing facts refuse the Load — a guessed rate plays at the wrong pitch.
            let pcm = match crate::app_pcm_for_rate(session_rate, open.feed_payload.pcm.clone()) {
                Some(Ok(f)) => Some(f),
                Some(Err(why)) => {
                    plx_base::eventlog::log(&format!("start_bufferfeed: PCM audio refused — {why}"));
                    shared.demux_failed.store(true, Ordering::Release);
                    return Err(PrepareRefusal::Refused);
                }
                None => None,
            };
            // Dev trigger `pcmavoffset=<ms>` (debug builds, read per Load): how the lead is measured
            // on an output the default was not filmed on.
            let lead_override = plx_base::devtrig::read("pcmavoffset");
            let Some(audio) = load_audio(
                &codec, session_rate, pcm.is_some(), lead_override.as_deref(), plx_platform::devcaps::audio_stack(),
            ) else {
                plx_base::eventlog::log(&format!("start_bufferfeed: unsupported audio codec {codec:?}; refusing incorrect Load declaration"));
                shared.fail_demux_latch(hooks.load_refusal_latch);
                return Err(PrepareRefusal::Refused);
            };
            crate::set_pcm_out(
                pcm.map(|f| crate::ff_pcm::PcmOut {
                    rate: f.sample_rate, bits: f.bits, channels: f.channels, source_rate: f.source_rate,
                }),
                pcm.is_some() && open.pcm_is_lossy,
            );
            if let Some(f) = pcm {
                plx_base::eventlog::log(&crate::pcm_decode_line(&codec, &f));
            }
            shared.dg_load_a.store(audio.diagnostic, Ordering::Relaxed);
            // App-decoded audio (`pcm`) and a speed session (`ff::SpeedDecoder`) both declare PCM;
            // `load_audio` has already put that into the declared name and the lead plan with it.
            let ac = audio.name;
            audio_declared = ac;
            audio_lead = audio.lead;
            // A lead that follows the sound output is read and logged by the Load thread, which may
            // block on the bus (`resolve` asks nothing for the other plans); the dev override, a
            // stack whose lead is the same on every output (`Fixed`) and a stack with no lead
            // (`NoLead`) are settled here and say so now.
            if let Some(line) = audio_lead.start_log_line() {
                plx_base::eventlog::log(&line);
            }
            // The sink envelope — `adaptiveStreaming`'s maxWidth/maxHeight/maxFrameRate — used
            // to be the panel max (4K60) for every codec and every source, on the reasoning that
            // the pipeline reads the true dims from the SPS and the block is "just a ceiling".
            // True on the dev set (webOS 4.10); MEASURED FALSE on 10.3.1, which allocates against
            // the declaration and refuses every H.264 Load at 4K60 (`smp_cb type=18 num=601`) —
            // i.e. every server transcode. `sink_envelope` is the rule now, and it is deliberately
            // no wider than what was measured: `docs/webos10-resource-allocation.md`.
            // `lv` is the STREAM's declared codec; `is_h265` above belongs to the local-sample
            // path and is false here for every streamed HEVC playback (device-measured 2026-09-03:
            // three HEVC cells declared FHD before this read the right flag).
            sink_env = sink_envelope_now(&open.feed_payload, lv, session_rate);
            let av = build_av_payload_from_facts(&open.feed_payload, lv.payload_name(), ac, sink_env, session_rate, open.payload_options);
            stream_payload = if ac == "OPUS" {
                let channels = open.audio_channels;
                let Some(p) = crate::with_opus_info(&av, channels) else {
                    plx_base::eventlog::log(&format!("start_bufferfeed: no Opus layout for {channels} channel(s); refusing the Load"));
                    shared.demux_failed.store(true, Ordering::Release);
                    return Err(PrepareRefusal::Refused);
                };
                plx_base::eventlog::log(&format!("start_bufferfeed: opusInfo channels={channels}"));
                p
            } else {
                av
            };
            &stream_payload
        }
    } else if is_h265 {
        sink_env = crate::static_envelope(crate::LoadVideo::H265);
        crate::PAYLOAD_H265
    } else {
        sink_env = crate::static_envelope(crate::LoadVideo::H264);
        crate::PAYLOAD_V
    };
    // The windowId splice happens HERE, at the single point every payload variant passes through,
    // rather than inside build_av_payload — three of the four variants are static strings that
    // never see the builder, and a binding that works only for streamed A/V would be the kind of
    // bug that reproduces on some content and not others.
    // ...and the appId substitution happens here for the same reason, and BEFORE the splice —
    // `with_window_id`'s anchor is the composed `"appId":"<id>"`, so the placeholder has to be
    // gone by then.
    let payload_c = std::ffi::CString::new(with_window_id(mt, &with_app_id(payload_str), hooks)).unwrap();
    if stream {
        let open = open.as_ref().expect("stream request was captured");
        // What the payload ACTUALLY declared, once, in the event log. Not test scaffolding: until
        // this line, the only answer to "what did we tell the television this stream was" lived in
        // the on-screen diagnostics read-out (`dg_load_v`/`dg_load_a` above), i.e. nowhere a log
        // could settle it — and the `"AC3 PLUS"` renaming a few lines up is exactly the kind of
        // thing a log should be able to settle, since getting it wrong leaves the audio ES
        // unconfigured and stalls the video sink through audioSync. Streamed only: the two static
        // sample payloads declare nothing chosen and would be claiming a declaration they never
        // consulted. Carries no URL and no token, and costs one line per playback.
        let dv = open.original_dv;
        plx_base::eventlog::log(&format!(
            "load: v={} a={:?} fps={:.3} dv=present:{} P{}/{} el:{} atmos:{} max={}x{}@{}",
            video_declared,
            audio_declared,
            open.feed_payload.fps,
            dv.present as i32,
            dv.profile,
            dv.bl_compat,
            dv.el_present as i32,
            open.feed_payload.immersive as i32,
            sink_env.w,
            sink_env.h,
            sink_env.fps
        ));
    }

    NativePrepared::arm(
        shared, open, sample, stream, payload_c, audio_lead,
        user_held, seek_preroll, side_reader, dv_landing,
    )
}

#[cfg(all(test, feature = "hostsim"))]
mod native_lifecycle_host_seam_tests {
    use super::*;
    use crate::native_release::ReleaseStep;
    use crate::shared::FEED_SHARED;

    fn test_sink() -> &'static dyn VideoSink { crate::ffi_host::test_sink::sink() }

    /// The release verbs against the simulator's sink and a plane of the test's own. The common
    /// observations the composition records (telemetry crumbs) are not the executor's, so they are
    /// no-ops here; `plx_media`'s tests keep the ones that read them.
    fn release_hooks() -> ReleaseHooks {
        static PLANE: plx_platform::tv::plane::PlaneOwner = plx_platform::tv::plane::PlaneOwner::new();
        fn no_step(_: ReleaseStep) {}
        fn no_window() {}
        fn no_unload(_: UnloadTicket) -> Option<std::thread::JoinHandle<()>> { None }
        ReleaseHooks {
            raw_sink: test_sink,
            plane: &PLANE,
            on_release_step: no_step,
            on_exported_window_destroy: no_window,
            spawn_unload: no_unload,
        }
    }

    struct FreshProcess;

    impl Drop for FreshProcess {
        fn drop(&mut self) {
            crate::ffi_host::reset_native_lifecycle_for_test();
            crate::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.reset_session();
        }
    }

    fn loaded_epoch(mt: &MainThread) -> u32 {
        let epoch = FEED_SHARED.begin_native_session().expect("native epoch");
        assert_eq!(unsafe { test_sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert_eq!(unsafe { test_sink().ready(mt) }, 1);
        epoch
    }

    #[test]
    fn host_seam_destroys_only_with_evidence_and_latches_quarantine() {
        let _serial = plx_base::testlock::serial();
        crate::ffi_host::reset_native_lifecycle_for_test();
        crate::ffi_host::force_clocksink_for_test(true);
        FEED_SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let mt = unsafe { MainThread::assume() };

        let first = FEED_SHARED.begin_native_session().expect("first native epoch");
        assert_eq!(unsafe { test_sink().load(c"{}".as_ptr(), first) }, 1);
        assert!(unsafe { test_sink().callback_intercepts(&mt) } > 0);
        unsafe { test_sink().unload(&plx_platform::tv::sink::UnloadTicket::issue(&mt)) };
        assert!(FEED_SHARED.native_unload_completed(first));
        assert_eq!(unsafe { test_sink().callback_gate_retire(&mt) }, 1);
        assert!(FEED_SHARED.retire_native_session(first));
        assert_eq!(unsafe { test_sink().destroy(&mt) }, 1);
        assert_eq!(unsafe { test_sink().ready(&mt) }, 0);

        FEED_SHARED.reset_session();
        let unproven = FEED_SHARED
            .begin_native_session()
            .expect("unproven native epoch");
        assert_eq!(unsafe { test_sink().load(c"{}".as_ptr(), unproven) }, 1);
        crate::ffi_host::force_callback_intercepts_for_test(0);
        unsafe { test_sink().unload(&plx_platform::tv::sink::UnloadTicket::issue(&mt)) };
        assert_eq!(unsafe { test_sink().callback_gate_retire(&mt) }, 0);
        assert!(FEED_SHARED.retire_native_session(unproven));
        unsafe { test_sink().quarantine(&mt) };

        FEED_SHARED.reset_session();
        let refused = FEED_SHARED.begin_native_session().expect("refused native epoch");
        assert_eq!(
            unsafe { test_sink().load(c"{}".as_ptr(), refused) },
            0,
            "quarantine must remain a process-long refusal",
        );
        assert!(FEED_SHARED.retire_native_session(refused));
    }


    #[test]
    fn a_refused_load_with_no_constructed_object_releases_its_epoch() {
        let _serial = plx_base::testlock::serial();
        crate::ffi_host::reset_native_lifecycle_for_test();
        crate::ffi_host::force_clocksink_for_test(false);
        FEED_SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let mt = unsafe { MainThread::assume() };
        let epoch = FEED_SHARED.begin_native_session().expect("native epoch");
        assert_eq!(unsafe { test_sink().load(c"{}".as_ptr(), epoch) }, 0);
        assert_eq!(unsafe { test_sink().ready(&mt) }, 0);
        assert_eq!(release_native_object_timed(&mt, epoch, &mut TeardownSpans::new(), &FEED_SHARED, &release_hooks()),
            plx_playback_contract::ReleaseStatus::Released,
            "the joined host Load refused before constructing any native object");
        let next = FEED_SHARED.begin_native_session()
            .expect("an explicitly empty native seam must not quarantine subsequent attempts");
        assert!(FEED_SHARED.retire_native_session(next));
    }


    #[test]
    fn ready_zero_with_an_allocated_epoch_is_still_uncertain() {
        let _serial = plx_base::testlock::serial();
        crate::ffi_host::reset_native_lifecycle_for_test();
        crate::ffi_host::force_clocksink_for_test(true);
        FEED_SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        // Hide dispatchability while retaining the constructed object's callback epoch.
        crate::ffi_host::force_object_ready_for_test(false);
        assert_eq!(unsafe { test_sink().ready(&mt) }, 0);
        assert_eq!(release_native_object_timed(&mt, epoch, &mut TeardownSpans::new(), &FEED_SHARED, &release_hooks()),
            plx_playback_contract::ReleaseStatus::Quarantined,
            "ready=0 alone cannot authorize release of an allocated object");
    }


    #[test]
    fn a_blocked_host_lifecycle_never_becomes_no_object_proof() {
        let _serial = plx_base::testlock::serial();
        crate::ffi_host::reset_native_lifecycle_for_test();
        crate::ffi_host::force_clocksink_for_test(true);
        FEED_SHARED.reset_session();
        let _fresh_process = FreshProcess;
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        unsafe { test_sink().quarantine(&mt) };
        assert!(crate::ffi_host::lifecycle_blocked_for_test());
        assert_eq!(unsafe { test_sink().ready(&mt) }, 0);
        assert_eq!(release_native_object_timed(&mt, epoch, &mut TeardownSpans::new(), &FEED_SHARED, &release_hooks()),
            plx_playback_contract::ReleaseStatus::Quarantined);
    }
}
