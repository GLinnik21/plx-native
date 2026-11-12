//! Main-thread-confined native feed resources and feed pacing state.
//! Common route ownership stays in the player coordinator.

use crate::aq::{AuNode, AuQueue};
use crate::shared::{PhysicalShared, Stage};
use plx_net::stream::HttpStream;
use std::os::raw::{c_int, c_void};
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::Ordering;

// The queue caps and the feed-ahead leads are contract vocabulary (the player's ABR plant and
// read-out name them without naming this crate); the old paths stay for this crate's own code.
pub use plx_playback_contract::feed::{
    aq_caps, AQ_AUDIO_BYTES, AQ_VIDEO_BYTES, MAX_FEED_AHEAD_NS, PRES_NONE,
};

/// Post-seek video depth allowed before an absent audio lane stops blocking Play.
pub const PRIME_VIDEO_MAX_NS: i64 = 2_500_000_000;
/// Audio lead-in read and feed window when a track begins late.
pub const AUDIO_LEAD_IN_NS: i64 = 2_000_000_000;

/// The longest the throttle's reference runs on past the last position report, in ms. The
/// pipeline reports every ~201 ms, so against this cap the run-on bridges a report up to ~49 ms
/// late (not a missed one); a paused or stalled clock stops changing `pres_fed`, and the
/// reference then stops this far ahead of it instead of opening the lane without bound.
pub const FEED_PACE_CAP_MS: u32 = 250;

/// **The feed-ahead throttle's reference: the presented position, run on at 1x between reports.**
///
/// `FEED_SHARED.pres_fed` moves in ~200 ms steps (the pipeline's 5 Hz position callback). Measured
/// against that step function, the throttle stayed shut for eleven frames and then opened by
/// 200 ms of media at once, so every fifth of a second one frame fed ~5 video and ~7 audio AUs to
/// Starfish, and `Feed()` blocks: on the TV with the track menu open, `FRAMEDROP … results=14.6`
/// with `feed:14.4` inside it, 4–15 ms on every 12th frame. Run on from the last report at 1x,
/// the same budget opens a frame's worth of media per frame, so the same AUs reach
/// Starfish one or two per frame. The bound is the same lead,
/// `MAX_FEED_AHEAD_NS`, past the ESTIMATED playhead; the reserve can therefore exceed the raw
/// report's by at most [`FEED_PACE_CAP_MS`], and that cap stops the estimate when the reports
/// stop changing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedPace {
    /// The last distinct `pres_fed` seen, and when it was first seen (player clock, ms).
    last: Option<(i64, u32)>,
}

impl FeedPace {
    /// The position to throttle against at `now_ms`, given the current raw `pres_fed`.
    /// [`PRES_NONE`] passes through untouched (feed freely) and forgets the last report.
    pub fn reference(&mut self, pres: i64, now_ms: u32) -> i64 {
        self.reference_at(pres, now_ms, crate::shared::RATE_NORMAL)
    }

    /// [`Self::reference`] for a pipeline playing at `rate_permille` (1500 = 1.5x): between two
    /// reports the playhead moves that much media per wall-clock ms, so the run-on scales with it.
    /// Run on at 1x under a faster pipeline, the estimate would trail the real playhead and the
    /// lane would feed less lead than the throttle means to keep.
    pub fn reference_at(&mut self, pres: i64, now_ms: u32, rate_permille: i32) -> i64 {
        if pres == PRES_NONE {
            self.last = None;
            return pres;
        }
        let at = match self.last {
            Some((p, at)) if p == pres => at,
            _ => {
                self.last = Some((pres, now_ms));
                now_ms
            }
        };
        let run_on = now_ms.saturating_sub(at).min(FEED_PACE_CAP_MS) as i64;
        pres.saturating_add(run_on * 1_000 * i64::from(rate_permille.max(0)))
    }
}


pub use plx_playback_contract::feed::SampleBuf;

pub enum Source {
    Stream, // host/port/path are consumed by the demux thread at spawn
    Sample(Box<SampleBuf>),
}

/// owns a popped au_node; frees on drop (paired with the malloc in aq_push).
pub struct AuBox(pub *mut AuNode);
impl Drop for AuBox {
    fn drop(&mut self) {
        unsafe { libc::free(self.0 as *mut c_void) }
    }
}

/// A Segment seek remains unresolved after Play until the sink and clock both follow its AUs.
pub struct SegmentLanding {
    pub(crate) epoch: u32,
    pub(crate) play: Option<SegmentPlay>,
}

pub(crate) struct SegmentPlay {
    pub position_tick: u32,
    pub displayed: u32,
    pub waiting_since: Option<u32>,
}

/// MAIN-THREAD-CONFINED. No worker thread ever names an Engine field.
pub struct Engine {
    /// Physical callback mailbox for this feed executor; common owns its lifetime and reset.
    pub shared: &'static PhysicalShared,
    /// Exact reducer attempt which constructed this native object. A duplicate start for this
    /// attempt is harmless; a different Prepared transaction must never be mistaken for it.
    pub attempt: plx_playback_contract::AttemptId,
    /// Firmware callback context for this exact native `Load`.
    pub native_epoch: u32,
    pub stage: Stage,
    pub video_info_sent: bool, // videoInfoSent
    /// ACB route: `FEED_SHARED.frames` when the plane bind completed (`Stage::Streaming`), until a tick
    /// beyond it is seen. A paused preroll re-pauses only after that tick: a picture presented
    /// before the bind is not on the panel.
    pub bound_at_frames: Option<i32>,
    /// webOS 5+ only: the source size the exported window was last placed with, so a corrective
    /// re-place happens exactly once if the demuxer publishes the real one later. 0 = never placed.
    pub placed_src: (c_int, c_int),
    pub eos_pushed: bool, // Kodi VIDEO_DRAIN: pushEOS() sent once at true EOF
    pub rebase_pending: bool, // g_rebase_pending
    // In-place seek only: keyframes this far AHEAD of the seek target are stale frames the demuxer
    // produced from its pre-flush read position before the reopen+av_seek won the race (playback
    // drifts forward during a long scrub) — reject them so the rebase anchors on the REAL post-seek
    // keyframe, not the drifted one. `rebase_drops` caps the rejections so a sparse-keyframe file or
    // a genuinely-failed av_seek can't hang the rebase.
    pub rebase_drops: i32,
    pub seek_armed_at: u32, // SDL-ticks when the current in-place seek was armed (stuck watchdog)
    pub seek_retries: i32,  // cheap reopen-retries attempted before escalating to a full reload
    pub segment_landing: Option<SegmentLanding>,
    pub flushed: bool,      // Kodi m_flushed: set on an in-place seek flush; the first
    // post-flush keyframe triggers setTimeToDecode + sendSegmentEvent (the fresh GStreamer
    // segment a bare flush() omits), then clears this.
    /// The post-seek keyframe has installed its segment but has not yet been accepted by
    /// Starfish. Native presentation callbacks remain fenced until that exact AU returns `O`.
    pub presentation_rearm_pending: bool,
    pub max_fed_video_pts: i64, // high-water fed pts, VIDEO lane (g_max_fed_pts)
    pub max_fed_audio_pts: i64, // high-water fed pts, AUDIO lane (two-lane feed)
    /// Fed pts of the AU that opened this timeline's audio — the one the demuxer marked as its
    /// first since the open or seek (`FEED_SHARED.first_audio_pts`) — once Starfish accepted it.
    /// `None` until then; reset with `max_fed_audio_pts`. See `audio_lead_in`.
    pub audio_start_fp: Option<i64>,
    /// Audio AUs the stale-ahead guard dropped since this open or seek (`audio_stale_ahead`);
    /// reset with `max_fed_audio_pts`. Counts what the `a#` feed counter cannot (it counts
    /// accepted AUs), and paces the event-log line that reports the drops.
    pub audio_stale_dropped: u32,
    /// Added to the sink pts of every audio AU of THIS Load (`audio_sink_pts`): the active sound
    /// output's lead on a `liblxa` or 2018 stack, the one lead of the `lgaudiosink` stack
    /// (`pcm_output_lead`), or the dev override, when the Load declared `"audio":"PCM"`, 0 for a
    /// compressed Load and for a stack with no lead. Chosen once per Load, from the plan
    /// `load_audio` made with the declared audio (`PcmLead`); an in-place seek keeps it.
    pub pcm_sink_lead_ns: i64,
    /// The lead follows the sound output, which only the Load thread may read (the frame thread
    /// may not wait on the bus): `pcm_sink_lead_ns` is not final until `settle_pcm_lead` has
    /// taken it from the Load's return, and no audio is fed before.
    pub pcm_lead_pending: bool,
    /// The feed-ahead throttle's reference position, run on between the pipeline's 5 Hz
    /// position reports (see [`FeedPace`]).
    pub feed_pace: FeedPace,
    pub seek_base_pts: i64,     // fed pts of the first post-seek keyframe (prime measures buffer
    // depth as max_fed_video_pts - seek_base_pts, since the in-place seek feeds REAL pts, not 0-based)
    // prime-then-play: after a seek/resume the pipeline is PAUSED and data is buffered before
    // Play, so the clock doesn't free-run through the demux reopen / transcode-restart gap (that
    // gap is what makes video "fast-forward" to catch the audio clock on resume). feed_stream
    // fires Play once max_fed_pts reaches PRIME_NS.
    pub prime_play: bool,
    /// Common startup owns the first Play. Later seek/rebuffer primes retain their physical
    /// clock protocol after that command has been admitted.
    pub startup_controlled: bool,
    /// Viewer hold captured before this physical Load; first common Play must resume it.
    pub startup_user_held: bool,
    pub startup_play_commanded: bool,
    /// Physical first Play was accepted; later seek/rebuffer primes are independent.
    pub startup_first_play_done: bool,
    /// One refused first Play is reported against its common command on the next driver poll.
    pub startup_play_refused: bool,
    /// Progressive initial playback kept its immediate Play boundary after LoadCompleted.
    pub startup_immediate_play: bool,
    // Two-lane feed (Kodi m_messageQueueVideo/Audio): the demuxer routes es=1 video to aq_video
    // and es=2 audio to aq_audio; each lane is fed independently so a video BufferFull can't stall
    // the audio lane (the audioSync master clock). Both are allocated for a stream. (None only
    // pre-start and on the local-sample source.)
    pub aq_video: Option<Box<AuQueue>>, // g_aq (M owns; ptr handed to D)
    pub aq_audio: Option<Box<AuQueue>>, // audio lane
    // hs/payload are RAII: held alive for the workers (which hold raw ptrs into
    // them) and freed only after join — never read back through the field.
    #[allow(dead_code)]
    pub hs: Box<HttpStream>, // demux socket (M owns; D uses via raw ptr)
    pub pending_video: Option<AuBox>, // bf_pending, VIDEO lane (held across BufferFull)
    pub pending_audio: Option<AuBox>, // bf_pending, AUDIO lane
    #[allow(dead_code)]
    pub payload: std::ffi::CString, // bf_payload (kept alive for the session)
    pub source: Source,
    pub stream_th: Option<std::thread::JoinHandle<()>>,
    pub load_th: Option<std::thread::JoinHandle<()>>,
    /// Attempt-scoped common reader IO; its stop runs before the paired demux join.
    pub side_reader: Box<dyn crate::side_reader::SideReaderPort>,
    /// Preview Load that must land unbound. The media join waits until the thread has returned.
    pub preview_abandon: bool,
    /// Whether this session's era-chosen sink counter has shown up (the callback-era watch).
    pub era_watch: crate::era_callbacks::Watch,
    /// The Dolby Vision landing guard ([`crate::dv_start`]), re-armed at every landing.
    pub dv_start: crate::dv_start::Guard,
}

impl Engine {
    pub fn uses_stream_queues(&self) -> bool {
        matches!(self.source, Source::Stream)
    }
}

/// Test builds only: how many Engines have dropped (the exit's leak is observed by it staying put).
#[cfg(any(test, feature = "test-support"))]
static ENGINES_DROPPED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Test builds only: an Engine dropped while it still holds its demuxer's handle was never torn
/// down, so that thread may still be writing into `aq_video`/`aq_audio`, which this drop frees.
/// `teardown` always takes the handle and joins it first. The hostsim rigs used to drop a
/// reloaded Engine straight out of their `PlayerAdapter`: the demuxer's `aq_set_eof` then landed
/// in freed memory, a heap corruption that surfaced later as a crash in some unrelated free.
#[cfg(any(test, feature = "test-support"))]
impl Drop for Engine {
    fn drop(&mut self) {
        ENGINES_DROPPED.fetch_add(1, Ordering::SeqCst);
        if self.stream_th.is_some() && !std::thread::panicking() {
            panic!("an Engine was dropped with its demuxer still owned: stop it with teardown first");
        }
    }
}


#[cfg(any(test, feature = "test-support"))]
pub fn engines_dropped_for_test() -> usize {
    ENGINES_DROPPED.load(Ordering::SeqCst)
}

pub struct AbandonedLoad {
    pub attempt: plx_playback_contract::AttemptId,
    pub epoch: u32,
    pub load_th: std::thread::JoinHandle<()>,
    /// The Load payload `sf_load` was handed as a raw pointer. Must outlive the thread.
    pub _payload: std::ffi::CString,
    pub abandoned_at: std::time::Instant,
}

pub struct PendingRelease {
    pub epoch: u32,
    /// Taken by the join ([`join_finished_release`]); `None` afterwards.
    pub unload_th: Option<std::thread::JoinHandle<()>>,
    pub handed_off_at: std::time::Instant,
    /// The Engine the object belonged to. It used to drop at the end of `teardown`, after the
    /// release; it still drops after it (the Load payload in particular outlives the Unload).
    pub _engine: Engine,
}

impl PendingRelease {
    pub fn attempt(&self) -> plx_playback_contract::AttemptId {
        self._engine.attempt
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn handed_off_at(&self) -> std::time::Instant {
        self.handed_off_at
    }

    /// The `Unload` has returned, so joining its worker cannot block.
    pub fn is_finished(&self) -> bool {
        self.unload_th.as_ref().is_none_or(|t| t.is_finished())
    }

    /// The release machine has observed this worker return; joining it cannot block the frame.
    pub fn join_returned_worker(&mut self) {
        if let Some(thread) = self.unload_th.take() {
            plx_base::task::join("sf-unload", thread);
        }
    }
}

#[cfg(test)]
mod feed_pace_tests {
    use super::{FeedPace, FEED_PACE_CAP_MS, MAX_FEED_AHEAD_NS, PRES_NONE};

    const MS: i64 = 1_000_000;

    /// Video AUs (24 fps, 41.7 ms apart) the throttle admits at each 60 Hz frame of a 1x clock
    /// whose position reports arrive every 200 ms — with `reference` as the throttle's position.
    fn admitted_per_frame(reference: impl Fn(&mut FeedPace, i64, u32) -> i64) -> Vec<u32> {
        let mut pace = FeedPace::default();
        let au_ns = 41_708_333i64;
        let mut next_pts = MAX_FEED_AHEAD_NS; // the lane starts full
        let mut out = Vec::new();
        for frame in 0..120u32 {
            let now = 1_000 + frame * 1000 / 60;
            let true_pos = (now as i64 - 1_000) * MS;
            let pres = true_pos - true_pos % (200 * MS); // the 5 Hz step function
            let refpos = reference(&mut pace, pres, now);
            let mut n = 0;
            while next_pts - refpos <= MAX_FEED_AHEAD_NS {
                next_pts += au_ns;
                n += 1;
            }
            out.push(n);
        }
        out
    }

    /// **Regression: the 5 Hz feed burst.** Against the raw report the lane opens 200 ms at a
    /// time: ~5 AUs land on one frame and none on the eleven around it. Run on at 1x it never
    /// admits more than one video AU a frame, and admits the same media over the same time.
    #[test]
    fn the_reference_spreads_the_five_hertz_burst_across_frames() {
        let raw = admitted_per_frame(|_, pres, _| pres);
        let paced = admitted_per_frame(|p, pres, now| p.reference(pres, now));
        assert!(raw.iter().max().copied().unwrap_or(0) >= 4, "the raw step feeds in bursts: {raw:?}");
        assert!(paced.iter().all(|&n| n <= 1), "paced: at most one AU a frame: {paced:?}");
        let (r, p): (u32, u32) = (raw.iter().sum(), paced.iter().sum());
        assert!(p.abs_diff(r) <= 6, "same media over the same two seconds: raw {r}, paced {p}");
    }

    /// A `now` BEFORE the stamp of the last report (the throttle read its clock earlier than the
    /// report was first seen) is no run-on, not a full-cap one.
    #[test]
    fn a_clock_behind_the_report_stamp_does_not_run_on() {
        let mut pace = FeedPace::default();
        assert_eq!(pace.reference(5_000 * MS, 1_000), 5_000 * MS);
        assert_eq!(pace.reference(5_000 * MS, 900), 5_000 * MS);
    }

    /// A clock that stops reporting new positions (paused, stalled) opens the lane by at most
    /// the cap, never without bound; `PRES_NONE` passes through and forgets the last report.
    #[test]
    fn a_frozen_report_runs_on_at_most_the_cap_and_none_passes_through() {
        let mut pace = FeedPace::default();
        assert_eq!(pace.reference(5_000 * MS, 100), 5_000 * MS);
        assert_eq!(pace.reference(5_000 * MS, 150), 5_050 * MS);
        assert_eq!(pace.reference(5_000 * MS, 100_000), 5_000 * MS + FEED_PACE_CAP_MS as i64 * MS);
        assert_eq!(pace.reference(PRES_NONE, 100_001), PRES_NONE);
        assert_eq!(pace.reference(5_000 * MS, 100_002), 5_000 * MS, "a fresh report restarts the run-on");
    }

    /// At 1.5x the playhead moves 1.5 ms of media per wall-clock ms between reports, so the
    /// estimate runs on that much faster; capped by the same wall-clock bound.
    #[test]
    fn the_run_on_follows_the_pipeline_rate() {
        let mut pace = FeedPace::default();
        assert_eq!(pace.reference_at(5_000 * MS, 100, 1500), 5_000 * MS);
        assert_eq!(pace.reference_at(5_000 * MS, 150, 1500), 5_075 * MS);
        assert_eq!(pace.reference_at(5_000 * MS, 200, 500), 5_050 * MS, "half speed runs on at half");
        assert_eq!(
            pace.reference_at(5_000 * MS, 100_000, 2000),
            5_000 * MS + 2 * FEED_PACE_CAP_MS as i64 * MS,
            "the cap is wall-clock time; at 2x it covers twice the media"
        );
        assert_eq!(
            FeedPace::default().reference(5_000 * MS, 100),
            FeedPace::default().reference_at(5_000 * MS, 100, plx_playback_contract::RATE_NORMAL)
        );
    }
}
