//! Physical feed's cross-thread transport, callback and clock state. Every field
//! replaces a `volatile` global from playback.c and is an atomic or a Mutex (never a bare value).
//! Common route ownership is separate. The feed's process-lifetime owners, [`FEED_SHARED`] and
//! [`FEED_TX`], outlive every start/stop cycle and are *reset*, never freed. Native callbacks
//! carry the exact `Load` epoch: retirement drains an event already inside that epoch and rejects
//! every later event, so stable storage is not mistaken for permission to mutate the next
//! playback.
use plx_net::stream::HttpStream;
use crate::native_gate::NativeGate;
pub use crate::native_gate::{EventScope, NativeEventClass};
use std::ffi::CString;
use std::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering,
};
use std::sync::{Condvar, Mutex};

pub use plx_playback_contract::readout::RefusalPath;
use plx_playback_contract::feed::ResetScope;
use std::time::{Duration, Instant};

/// Physical 1x transport rate and its unknown speed-audio readout.
pub use plx_playback_contract::RATE_NORMAL;
pub const SPEED_AUDIO_UNKNOWN: u8 = 0;
/// The pipeline reported the declared 48 kHz stereo for a speed session.
pub const SPEED_AUDIO_OK: u8 = 1;
/// The pipeline reported something else than the declared 48 kHz stereo.
pub const SPEED_AUDIO_BAD: u8 = 2;

/// Exact conservation certificate accumulated while the native media clock is held.
///
/// For completed acquisitions `(A_i, D_i)`, `debt` is `Σ(A_i-D_i)` and `runway` is
/// `max_i(P_{i-1}+A_i)`, where `P` is that running debt.  A recovery epoch may resume only after
/// at least one complete segment, once the debt has closed and the playable reserve covers the
/// largest prefix cost.  Unlike the ABR acquisition bag, this state starts at each actual pause
/// and is discarded at Play, so an old slow segment can never raise the next pause's floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlsRecoveryEpoch {
    pub active: bool,
    pub completed: u64,
    pub debt_us: i64,
    pub runway_us: i64,
}

impl HlsRecoveryEpoch {
    const fn idle() -> Self {
        Self {
            active: false,
            completed: 0,
            debt_us: 0,
            runway_us: 0,
        }
    }

    fn begin(&mut self) {
        *self = Self {
            active: true,
            ..Self::idle()
        };
    }

    fn observe(&mut self, acquisition_us: u64, media: std::time::Duration) {
        if !self.active {
            return;
        }
        let acquisition_us = i64::try_from(acquisition_us).unwrap_or(i64::MAX);
        let media_us = i64::try_from(media.as_micros()).unwrap_or(i64::MAX);
        self.runway_us = self
            .runway_us
            .max(self.debt_us.saturating_add(acquisition_us));
        self.debt_us = self
            .debt_us
            .saturating_add(acquisition_us)
            .saturating_sub(media_us);
        self.completed = self.completed.saturating_add(1);
    }

    pub fn ready(self, playable_ns: i64) -> bool {
        self.active
            && self.completed > 0
            && self.debt_us <= 0
            && playable_ns.max(0) / 1_000 >= self.runway_us
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClockHold {
    /// A freshly loaded pipeline has not been started yet.
    Initial,
    /// An in-place seek paused the old timeline and is priming the new one.
    Seek,
    /// ABR stopped the clock at a measured starvation boundary.
    Rebuffer,
    /// The viewer explicitly paused a running clock.
    User,
    /// The viewer resumed a queued stream; feeding is open, but the physical clock remains held
    /// until both decoder lanes and the measured runway are ready.
    ResumePrime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsPrimeKind {
    Fresh,
    Rebuffer,
    Resume,
}

impl ClockHold {
    fn prime_kind(self) -> Option<HlsPrimeKind> {
        match self {
            Self::Initial | Self::Seek => Some(HlsPrimeKind::Fresh),
            Self::Rebuffer => Some(HlsPrimeKind::Rebuffer),
            Self::ResumePrime => Some(HlsPrimeKind::Resume),
            Self::User => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsClockPhase {
    Running,
    PauseIssuing {
        token: u64,
        hold: ClockHold,
    },
    Held {
        hold: ClockHold,
    },
    PlayIssuing {
        token: u64,
        hold: ClockHold,
        resume_acb: bool,
    },
    Stopping,
}

#[derive(Clone, Copy, Debug)]
struct HlsClockState {
    phase: HlsClockPhase,
    next_token: u64,
    recovery: HlsRecoveryEpoch,
    /// Accepted user Pause ownership. This is intentionally inside the actuator mutex: the
    /// transport atomic is a hot-path mirror, not a second authority over whether Play may issue.
    user_held: bool,
    /// Monotone identity of accepted user clock transitions.  `Running` is not sufficient state:
    /// a complete Pause -> Resume may occur while a worker is blocked and return to the same
    /// shape.  Every automatic lease names the sequence it observed and the mutex validates that
    /// exact boundary before granting ownership.
    user_sequence: u64,
    /// User Resume was accepted while an internal/candidate hold still owned the native clock.
    /// The eventual Play must also resume ACB; losing that second half produces video/audio drift.
    resume_acb_pending: bool,
    /// Exactly one automatic actuator may own the boundary. Network/native I/O runs outside the
    /// mutex, but its lease token stays here through commit/rollback so another actuator cannot
    /// pass the same precondition concurrently.
    automatic: Option<HlsAutomaticOwner>,
}

impl HlsClockState {
    const fn new() -> Self {
        Self {
            phase: HlsClockPhase::Running,
            next_token: 0,
            recovery: HlsRecoveryEpoch::idle(),
            user_held: false,
            user_sequence: 0,
            resume_acb_pending: false,
            automatic: None,
        }
    }

    fn token(&mut self) -> u64 {
        self.next_token = self
            .next_token
            .checked_add(1)
            .expect("HLS clock transition token space exhausted");
        self.next_token
    }

    fn advance_user_sequence(&mut self) {
        self.user_sequence = self
            .user_sequence
            .checked_add(1)
            .expect("HLS user clock sequence exhausted");
    }
}

/// Why an HLS clock fence was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsClockFenceError {
    Stopping,
    Overlap,
    Exhausted,
    Superseded,
}

/// Which automatic quality transition owns the HLS clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsAutomaticTransition {
    QualityUp,
    QualityDown,
    Original,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HlsAutomaticPhase {
    Working,
    CommitAuthorized,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HlsAutomaticOwner {
    token: u64,
    kind: HlsAutomaticTransition,
    phase: HlsAutomaticPhase,
    user_sequence: u64,
}

// The HLS pause handshake's outcome enums and the feed `Stage` are contract vocabulary
// (`plx_playback_contract::feed`); the old paths stay for this crate's own code and for media's
// `player::shared` glob.
pub use plx_playback_contract::feed::{
    HlsPauseCompletion, HlsPlayCompletion, HlsUserPause, HlsUserResume, Stage,
};

/// What reserving the native Pause for an in-place seek decided: a Pause to issue under the
/// clock's token, or the hold that is already in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HlsSeekPause {
    Issue(u64),
    AlreadyHeld,
}

pub use plx_playback_contract::readout::PlaybackState;

pub struct Shared {
    /// Synchronized lifecycle/provenance for callbacks from the current native `Load`.
    native_session: NativeGate,
    // library callback thread (K) -> main (M)
    pub playpos_ns: AtomicI64, // g_playpos_ns
    /// Last content position from a native type=0 callback. Unlike playpos_ns, seek and HUD
    /// code never writes this mailbox. dg_pos_ticks publishes each new sample.
    pub reported_playpos_ns: AtomicI64,
    // the presented frame's fed PTS (0-based, raw `num` from the type=0 callback). The feed
    // loop throttles on max_fed_pts - pres_fed so it stays ~MAX_FEED_AHEAD ahead of the
    // decoder (feeding further overfills the 4K HEVC DPB/CPB and stalls the sink).
    pub pres_fed: AtomicI64,
    pub frames: AtomicI32, // bf_frames
    /// **Has this SESSION ever put a picture on the panel?** Set by the frame-presented callback
    /// beside `frames`, cleared ONLY by [`Shared::reset_session`] — so it survives a seek, which
    /// `frames` deliberately does not: `pump` zeroes `frames` as *part of applying* a seek, which
    /// makes "we have never shown a frame" and "we just seeked" indistinguishable through it. The
    /// HUD's one rule for which surface owns the "pipeline is working" read-out keys on this bit;
    /// see `appkit::player_hud::busy_surface`. Monotone within a session (false→true only), which is
    /// what lets two readers in one frame sample it independently without disagreeing.
    pub seen_frame: AtomicBool,
    /// **Has the video sink counted a displayed frame since the plane was last bound?** Set by the
    /// displayed-frame counter callback (`sink_counter_kind`) when its `num` is above zero, cleared
    /// when the ACB plane bind completes ([`Shared::forget_displayed_before_bind`]) and by every
    /// session reset, so a count taken before the bind, or by an earlier session, never stands for
    /// a picture on the panel. Read only through [`Shared::displayed_after_bind`] (it is what the
    /// Feed driver's `Observation::Presented` waits for); unlike the `dg_*` mirrors, a decision
    /// branches on it.
    sink_displayed_live: AtomicBool,
    // A positive callback sequence, so an in-place seek can exclude callbacks before Play.
    pub(crate) sink_displayed_sequence: AtomicU32,
    /// **The raw pts of the first audio AU the demuxer pushed since its last open or seek**.
    /// Written by `ff::demux` BEFORE that push, so the feed thread that pops the AU
    /// sees it. The audio lane uses it to tell the real start of a late audio track (an MKV
    /// audio `Delay` of several seconds) from stale pre-seek audio, which looks the same — far
    /// ahead of the video — but never carries this pts (see the physical audio feed lane). `i64::MIN`
    /// before one, since a real pts may be negative.
    pub first_audio_pts: AtomicI64,
    pub load_completed: AtomicBool,          // bf_loaded signal
    pub media_id: Mutex<Option<CString>>,    // bf_mediaId (captured once)
    pub source_info: Mutex<Option<Vec<u8>>>, // sourceInfoRaw, VERBATIM incl NUL
    /// What the demuxer read of a VP9 stream's colour (PQ tag, mastering display, light level). Set
    /// only for VP9, cleared per session; `pump` writes it into `source_info` before ACB is sent it.
    pub container_hdr: Mutex<Option<plx_playback_contract::video::ContainerHdr>>,

    // main/pump (M) -> library callback thread (K)
    pub pts_shift: AtomicI64, // g_pts_shift
    // content-time offset added to the displayed position: a transcode SEEK restarts the
    // stream 0-based (its PTS loses the seek offset), so playpos = fed - pts_shift + this.
    // 0 for direct-play (file PTS already IS content time) and for the initial transcode.
    pub disp_base: AtomicI64,

    // main/pump (M) -> demux (D)
    // The demux seek target in content-ns (-1 = none). The pump publishes it and the demux
    // thread consumes it with an av_seek_frame between two reads — it is the ENTIRE direct-play
    // seek mechanism, and also carries the resume offset armed before the first read. Nothing
    // interrupts the demuxer to make this happen; a `seek_byte` reopen trigger and a `next_url`
    // used to sit alongside it for that, and neither could ever fire (see ff.rs).
    pub seek_to_ns: AtomicI64,
    // the in-place seek's target content-ns, for the feed's rebase guard (drop stale drifted
    // keyframes). Distinct from seek_to_ns, which the demuxer consumes as it seeks. -1 = none.
    pub seek_target_ns: AtomicI64,
    // UI loading state: true from a seek request until playback resumes at the new position (prime→
    // Play). The HUD shows a spinner + freezes the playhead at `seek_display_ns` so it doesn't
    // wobble through the seek/rebase. -1 display = not loading.
    pub seeking: AtomicBool,
    pub seek_display_ns: AtomicI64,
    // The seek epoch the player's newest `FeedCommand::Seek` carried (0 = none this session), and
    // the newest one whose playback has resumed (published where `seeking` is cleared by the
    // landing). The player derives its own `seeking` from the epoch it requested and this one.
    pub seek_epoch: AtomicU32,
    pub seek_landed_epoch: AtomicU32,
    // The feed's own request to seek again: a refused native `Seek` (the in-place downgrade in
    // `native_executor`) counts it in `seek_requeue_epoch` (0 = none this session) and publishes
    // where in `seek_requeue_target`. The player reads both through `FeedRaw::seek_requeue` and
    // turns a sample whose epoch is new into one pending seek.
    pub seek_requeue_epoch: AtomicU32,
    pub seek_requeue_target: AtomicI64,
    // NATIVE audio-track switch (direct-play, no transcode): the 0-based audio stream index
    // to feed. `desired` (-1 = av_find_best_stream) is read by the demuxer to pick the Nth audio
    // stream and PERSISTS across seeks/reloads (reset only on a new item, not in reset_session).
    // The synchronized route controller owns whether a reload is pending; keeping a second atomic
    // mailbox here used to let reset_session erase the request halfway through that reload.
    pub desired_audio_idx: AtomicI32,
    // The speed THIS session's Load plays at (`player::session_rate_for`, published by
    // `start_bufferfeed` before the demuxer spawns): what the timeline conversion
    // (`to_sink_ns`) and the demuxer's audio decode read. Per session: reset_session puts
    // it back to 1x.
    pub applied_rate_permille: AtomicI32,
    // What the pipeline reported setting up for THIS speed session's audio
    // (`player::note_speed_audio_report`); per session.
    pub speed_audio_verdict: AtomicU8,
    // demux (D) -> main (M): the producer died before publishing a duration, so the EOS path
    // (which needs `duration_ns > 0`) can never fire and the player would sit on a black screen
    // forever. The pump turns this into `PlaybackState::Error` so the HUD can say so.
    pub demux_failed: AtomicBool,
    /// The media transport failed after open, possibly after frames were already presented.
    /// Unlike `demux_failed`, this is not gated on a zero-frame start: a truncated live transfer
    /// is an error rather than a successful early EOF.
    pub demux_io_failed: AtomicBool,
    /// WHY the demuxer found nothing to feed, when the answer is "the stream itself": the server
    /// delivered audio streams and no video stream. Issue #22's whole shape — a transcode target
    /// the server cannot honour makes PMS drop the video track, and `ff: no video stream` alone
    /// sent the reviewer on a full server-side investigation the app could have named in one
    /// line. Set by the demux thread beside `demux_failed`; read on the main thread, which
    /// combines it with `route::is_transcoding()` to word the error (an audio-only TRANSCODE is
    /// the server's doing; an audio-only FILE is the file's).
    pub demux_no_video: AtomicBool,
    /// `sf_load` returned 0 — the pipeline REFUSED the Load payload.
    ///
    /// Without this the pump has no way to learn it: `load_thread` logged the result and dropped
    /// it, `loadCompleted` never arrives, and `Stage::Loading` is never left — so the player sits
    /// on a spinner forever with no error and no timeout. A rejected payload is the most likely
    /// shape of a webOS-5-specific failure (a key the newer pipeline will not accept), which makes
    /// this exactly the case that must be visible.
    pub load_failed: AtomicBool,
    /// issue #74 D.1.4: set alongside `load_failed` ONLY by the two `NATIVE_LOAD_BUDGET` expiry
    /// arms in `pump.rs` — never by a synchronous Load refusal (`sf_load` returning 0) or a
    /// `type=18` pipeline refusal, both of which set `load_failed` alone. This is what lets
    /// `error_shape` tell a k5lp-style hang apart from an ordinary firmware refusal on the wire,
    /// as `FailureKind::LoadTimeout` rather than the generic `FailureKind::TvPipeline`.
    pub load_timed_out: AtomicBool,
    /// The pipeline took the video decoder back from an ESTABLISHED session: `type=18 num=600`
    /// (`"VDEC resource is released by policy action"`) after a picture. Measured on the dev set
    /// (webOS 4, 2026-10-13): a direct play left paused until the screensaver came on got this
    /// with NO `LIFECYCLE: background`, and every later Play was refused for good. Set on the
    /// callback thread. It is a COUNT of the revocations this session (zeroed by the session's reset),
    /// not a flag: the player's readout holds how many it has taken, and the app loop parks the
    /// session exactly as an app switch does for each new one (rebuilding it on the viewer's next press).
    pub decoder_revocations: AtomicU32,
    /// The pipeline refused the Load with `type=18 num=200` (`Audio Codec Not Supported`) before any
    /// picture while the session fed its audio natively (`ff::native_audio::fed`). Set on the
    /// callback thread; the pump turns it into a learned refusal and a recovery flight that has the
    /// server convert that audio (the machine's `ConfirmRefusal`, run by the common pump's `confirm_refusal`).
    /// A COUNT of the refusals this session (the callback and the stall watchdog each add one),
    /// taken by the player's readout like [`Self::decoder_revocations`].
    pub native_audio_refusals: AtomicU32,
    /// issue #74 D.1: the epoch this session last logged "native: loadCompleted while Load in
    /// flight — deferring" for — 0 = never. `epoch` is monotonic and never 0 for a real session
    /// (the native gate refuses exhaustion instead of wrapping), so comparing against this is enough
    /// to log the deferral exactly once per session without needing an `Engine` field: the pump's
    /// `loadCompleted` arm can be entered many times while `Load` is in flight, and only the
    /// first should log.
    pub native_load_deferred_logged_epoch: AtomicU32,
    /// issue #74 D.1: the elapsed milliseconds `mark_native_load_returned` measured at the moment
    /// it flipped `load_call` to `Returned` — captured there (by `native_load::load_thread`) rather
    /// than re-derived on a later pump tick, since a later tick would over-count by however long
    /// the pump took to run again. `pump.rs` reads this once, for the "native: Load returned
    /// after Nms" log, exactly when it observes the deferral epoch marker above matching its own.
    pub native_load_elapsed_ms: AtomicU64,

    /// The main demuxer's FIRST video keyframe of this engine run (stream time + a payload
    /// fingerprint), published once for the side subtitle reader's clock mapping and cleared when
    /// the demuxer starts. `None` until that packet is read.
    pub side_anchor: Mutex<Option<plx_playback_contract::subtitle::Anchor>>,
    /// The side subtitle reader owns this playback's subtitles: `ff::demux` then neither enumerates
    /// the stream's own subtitle tracks nor begins an ASS source, which the reader's would replace.
    pub side_subs_owner: AtomicBool,

    // demux (D) -> main (M)
    pub file_size: AtomicI64, // g_file_size
    /// The DECODED FRAME SIZE, published by the demuxer once the video stream is known.
    ///
    /// Exists for the webOS 5+ exported window, whose `SDL_webOSSetExportedWindow(id, src, dst)`
    /// wants the frame you are FEEDING as `src` and the on-screen rect as `dst` — the pair is what
    /// expresses scaling. webOS 4's `AcbAPI_setDisplayWindow` takes only a destination, so nothing
    /// needed this before and the exported path was written passing the authoring canvas (1920x1080)
    /// for both. That is wrong for every 4K direct play, which is most of them.
    ///
    /// 0 until the demuxer has opened the stream; readers must treat 0 as "not known yet".
    pub video_w: AtomicI32,
    pub video_h: AtomicI32,
    /// Coherent `{w,h}` publication for actuator consumers. The individual fields above remain
    /// diagnostic mirrors; reading them independently can manufacture a raster no stream owned.
    video_raster: AtomicU64,
    /// Coherent display aspect from sourceInfo, including non-square pixels. Zero
    /// until reported; overlays temporarily fall back to the demuxer's coded raster.
    pub video_aspect: AtomicU64,
    /// Decoder-reported source frame rate in thousandths of a frame per second. 0 means the
    /// sourceInfo callback has not supplied one; unlike `FRAMEREADY`, this is stream metadata and
    /// therefore does not mistake the firmware's ~5 Hz position tick for video cadence.
    pub video_fps_milli: AtomicI64,
    pub duration_ns: AtomicI64, // was g_mkv.duration_ns (published)
    /// Latest normalized timestamps successfully enqueued for each elementary stream. HLS writes
    /// its segment-normalized zero-based timeline; progressive Original writes absolute movie
    /// PTS. Their consumers apply the matching display-base rule. These are content-time facts
    /// (not queue byte counts); `-1` means the lane has not produced an AU in this session/seek.
    pub hls_video_tail_ns: AtomicI64,
    pub hls_audio_tail_ns: AtomicI64,
    /// Whether the active HLS segment declared an audio stream that this session feeds. The prime
    /// gate must not reinterpret a temporarily empty audio lane as a genuinely video-only file.
    pub hls_audio_expected: AtomicBool,
    /// Retrospective replay reserve used only at a fresh HLS Load/seek boundary. Viewer Resume
    /// requires the balanced decoder prime; automatic runtime rebuffer uses the pause-local
    /// `hls_recovery` certificate below; carrying this history into every controller-created hold
    /// was the source of repeated freeze/catch-up on an otherwise stable stream. Zero leaves the
    /// decoder's ordinary prime threshold in charge.
    pub hls_prime_runway_ms: AtomicI64,
    /// Demux -> main request to stop the internal media clock before a projected fetch overrun
    /// turns into freeze/catch-up. This is not the user's pause state; feeding continues.
    pub hls_rebuffer_requested: AtomicBool,
    /// Video-tail generation captured when `hls_rebuffer_requested` was raised. If a completed
    /// segment advances the tail before the main thread consumes the request, the condition it
    /// described has already cleared and the request is stale.
    pub hls_rebuffer_request_tail_ns: AtomicI64,
    /// Derived hot-path mirror of the synchronized clock state below: Starfish is internally
    /// paused and waiting for measured runway. Production transitions write it only while holding
    /// `hls_clock`; workers which merely need a deadline predicate can read the atomic cheaply.
    pub hls_rebuffering: AtomicBool,
    /// Monotone proof that an internal hold happened, even if Pause→Play completes entirely while
    /// a network operation is blocked and both of its boolean snapshots therefore read false.
    pub hls_internal_hold_epoch: AtomicU64,
    /// One synchronized authority for native Pause/Play issuance, the pause-local conservation
    /// certificate and candidate/trial exclusion. FFI is called outside this mutex through a
    /// reserve/complete token protocol; `Condvar` lets the demux worker linearize after a very
    /// short in-flight Play instead of treating that ordinary ordering as a fatal overlap.
    hls_clock: Mutex<HlsClockState>,
    hls_clock_changed: Condvar,
    /// Retrospective reserve retained by an exploratory candidate's own deadline. Its non-negative
    /// presence also prevents an already-held clock from restarting mid-transaction; it is not a
    /// continuously armed pause threshold. `-1` means no trial.
    pub hls_trial_reserve_ms: AtomicI64,
    /// Seqlock generation for candidate media/ownership publication. Even values are stable; a
    /// worker CASes even->odd before its first candidate AU, then either publishes new ownership or
    /// realigns the old cursor. It publishes the matching recovery state before advancing
    /// odd->next even. The main-thread prime gate accepts only an unchanged even value around its
    /// whole tails+epoch snapshot, preventing false->true->false ABA from mixing candidate media
    /// with the previous rung's certificate. This is not a timer or threshold.
    pub hls_candidate_generation: AtomicU64,
    /// The pipeline reported end-of-stream: `type=28` (0x1c, `STATE_UPDATE__ENDOFSTREAM`, the same
    /// number on every webOS since the renumbering starts above it), `str=true`, after a picture.
    /// It is the pipeline's own word that it presented the last of what it was given, which the
    /// `ended` arithmetic cannot always reach: on the C1 (2026-10-20) a film whose audio ran 3 s
    /// past its last video frame stopped its clock at 5894 s of 5897 s, refused the audio after
    /// the EOS, and sat on the last frame for good. Cleared with the session and by a seek.
    pub pipeline_eos: AtomicBool,

    // close-to-interrupt handle: raw ptr to the Engine-owned HttpStream box, so
    // the pump/teardown can close(fd) to unblock a blocked recv. The box outlives
    // the worker threads (Engine drops after join), so the ptr stays valid.
    pub hs_ptr: AtomicPtr<HttpStream>,

    // ---- diagnostics mirror (`app::diagnostics`) -------------------------------------------------
    // Values the render path (and the opted-in handled-error snapshot) need that live on the
    // Engine, republished from the pump.
    // The render path holds no `&mut PlayerAdapter` and so cannot reach the `Engine` at all (when
    // the slot was a `static mut` it could, and a second live borrow was instant UB) — so it reads
    // these instead. Eighteen of them (`FeedDiag`: `dg_place_rv` ... `video_fps_milli`) reach the
    // read-out as the pump's sample, `FeedRaw::diag`, and the demux's `dg_abr_*` rows reach it as
    // `FeedRaw::abr`; the handled-error snapshot's `dg_cb_err_num` is still a direct mailbox load.
    //
    // STRICTLY ONE-WAY: written by the pump and seam, read only for observation. Nothing in the
    // playback state machine may ever branch on one, or a diagnostic becomes load-bearing.
    /// `Stage` as u8 — where the bind/play sequence has got to. Unlike the expensive queue
    /// mirrors below, this scalar is always current enough for a terminal error report even when
    /// Stats for Nerds is closed.
    pub dg_stage: AtomicU8,
    /// Starfish callbacks seen this session. A count of 0 with a completed Load is the pipeline
    /// never speaking to us — the sharpest single symptom there is. The TYPE of the last callback
    /// is deliberately not kept: the numbering shifts between webOS 4 and 5+ above 0x1c, so a
    /// displayed type would be a confident lie on the firmware we cannot test. `dg_cb_err` latches
    /// the one type that means the same on both.
    pub dg_cb_count: AtomicU32,
    /// What the callback accounting path reads, all per session: position ticks (`type=0`),
    /// callbacks `sink_counter_kind` named as the displayed-frame counter, and callbacks numbered
    /// 46 to 49 that it did not name (the other era's numbers). Unlike `frames`, a seek leaves them.
    pub dg_pos_ticks: AtomicU32,
    pub dg_sink_displayed_cb: AtomicU32,
    pub dg_sink_other_cb: AtomicU32,
    /// Why the VIDEO feeder is where it is, taken at each of `feed_stream`'s exit points.
    /// 0 none yet · 1 accepting · 2 BufferFull · 3 refused · 4 waiting for a frame (feed-ahead
    /// throttle) · 5 queue empty.
    ///
    /// Supersedes a bare last-reply byte, which says strictly less: "BufferFull" and "the throttle
    /// held us back" and "there was nothing to send" are three different faults with three
    /// different fixes, and a reply byte cannot tell them apart because the last two never reach a
    /// `Feed()` call at all. `queue empty` vs `BufferFull` is what splits a dead PRODUCER from a
    /// dead SINK.
    pub dg_feed_state: AtomicU8,
    /// The first Starfish error callback (`ty == 18`) of this session, and the callback INDEX it
    /// arrived at. Sticky: a later healthy callback must not erase the error that explains the
    /// session, and the index says whether it was immediate or after a long healthy run.
    pub dg_cb_err: AtomicI32,
    pub dg_cb_err_at: AtomicU32,
    /// The firmware's own error NUMBER (`num`) of that same first `ty == 18` callback — the
    /// `601` of `type=18 num=601 Resource Allocation Error`. Latched together with `dg_cb_err`
    /// (first one wins, so a later callback cannot overwrite the explanation) and cleared where
    /// that clears. Already clamped to the `i32` range: it is the one number a handled `tv_pipeline`
    /// report carries about the refusal, never the callback's string. A SYNCHRONOUS Load refusal
    /// (`sf_load` returning 0) has no number to latch — Starfish's `Load` hands back a bare
    /// success flag, and the other refusals inside `sf_load` are the seam's own lifecycle gates —
    /// so a session that failed that way leaves this at 0 ("none recorded").
    pub dg_cb_err_num: AtomicI32,
    /// HOW the Load was refused, as a [`RefusalPath`] code: written by [`Self::publish_native_load_failure`]
    /// (`Sync`) and by `sf_on_event`'s refusal-before-any-picture arm (`Async`), first writer wins,
    /// cleared by [`Self::reset_session`] with the other per-play failure state. Read by the
    /// `playback.failed` event, which `dg_cb_err_num` alone cannot serve: a synchronous refusal
    /// leaves that at 0 and so does a session that never failed.
    pub refusal_path: AtomicU8,
    /// The media GET's HTTP status and the bytes actually delivered to the demuxer. Written by the
    /// DEMUX thread — the third writer of this block. 0/0 = no connection was ever made, which is
    /// a different failure from a connection that answered 401.
    pub dg_http_status: AtomicI32,
    pub dg_net_rx: AtomicI64,
    /// WHY the media transfer died, when it was a curl transfer dying mid-body (issue #504): the
    /// libcurl code (0 = none recorded), the transfer's age in ms (-1 = unknown) and whether the
    /// reopen that follows a failed read was refused as well. Written by the demux thread beside
    /// `demux_io_failed` (`ff::frame_read_failed`), read by the handled playback error as a
    /// class and an age bucket — never as these numbers.
    pub dg_transport_rc: AtomicI32,
    pub dg_transport_age_ms: AtomicI64,
    pub dg_transport_reopen_failed: AtomicBool,
    /// WHICH demuxer exit failed the session (`DemuxExitClass::latch`; 0 = none recorded). Written
    /// by `note_demux_exit` / `fail_demux`, FIRST WINS: the first cause is the real one, and the
    /// teardown cascade that follows it must not overwrite it. Read by the handled playback error
    /// as a class. Cleared per session beside the other `dg_*` latches.
    pub dg_demux_exit: AtomicU8,
    /// SDL ticks when `loadCompleted` landed, and when `frames` last CHANGED. A photograph has no
    /// time axis: "Load completed, 0 frames" is innocent at 2 s and damning at 4 minutes, and the
    /// panel cannot tell the difference without these. Stamped in the pump, never in `app::diagnostics` —
    /// a stats-local timer would start when the panel is OPENED, so a four-minute hang would
    /// photograph as twelve seconds.
    pub dg_load_at: AtomicU32,
    pub dg_frame_at: AtomicU32,
    /// **The video plane's own cadence** — the counters behind the heartbeat's `vtick=`/`vgap=`.
    ///
    /// They exist because every other frame number in this app is about the GRAPHICS plane. `fps=`
    /// counts our GL swaps and `worstframe=` times our own draw, and both stay a flat 60/0.5 ms
    /// through playback that visibly stutters: the decoded picture is composited by the TV on a
    /// plane we never touch. The one place the pipeline tells us it put a frame on that plane is
    /// the `ty == 0` callback, so that is where these are stamped.
    ///
    /// Written on the LIBRARY thread (`sf_on_event`), sampled on the main thread into
    /// `FeedRaw::{vpres_count, vpres_gap_sum_ms, vpres_gap_count}` (the heartbeat differences two
    /// samples of them) — hence monotonic COUNTS rather than a delta of [`frames`](Self::frames),
    /// which a seek resets underneath a reader. Nothing drains them: `dg_vpres_gap_sum` (ms,
    /// wrapping) over `dg_vpres_gap_ct` is the mean gap between position callbacks over whatever
    /// span two samples cover, and the session reset zeroes all four. The gap count is not the
    /// callback count: a callback with no stamp before it adds a tick and no gap.
    ///
    /// `dg_vpres_at` is the previous presentation's stamp and MUST be zeroed wherever `frames` is
    /// (session reset, and the post-seek re-count in the pump): the pipeline legitimately shows
    /// nothing across a seek, and a gap measured over that pause is the harness reporting the seek
    /// as a stutter. A Pause or hold does NOT clear it (the clock-silence watchdog,
    /// `native_clock_silence_ms`, reads its age), so the first callback after a hold posts the
    /// hold's length: one spike in the line that holds it, which the mean over the next line forgets.
    pub dg_vpres_ct: AtomicU32,
    pub dg_vpres_at: AtomicU32,
    pub dg_vpres_gap_sum: AtomicU32,
    pub dg_vpres_gap_ct: AtomicU32,
    /// Bytes queued in each AU lane at the last tick, against `engine::aq_caps`.
    pub dg_aq_video: AtomicI64,
    pub dg_aq_audio: AtomicI64,
    /// High-water fed PTS per lane. Their DIFFERENCE is the sharpest instantaneous answer to
    /// "video plays but there is no sound": a snapshot of the fed COUNTS cannot see a lane that
    /// stopped advancing, because a large total stays large. A skew that grows without bound is
    /// the audio lane starving; a skew near zero says both lanes are keeping up and the fault is
    /// downstream of us.
    pub dg_fed_v_pts: AtomicI64,
    pub dg_fed_a_pts: AtomicI64,
    /// The codec strings this session actually put in the Starfish Load payload, as small codes
    /// (0 = none/not built; video 1 = H264, 2 = H265, 3 = AV1, 4 = VP9; audio 1 = AC3, 2 = AC3 PLUS, 3 = AAC).
    ///
    /// Codes rather than strings so they need no lock on the render path — and the PAYLOAD's view
    /// rather than a re-derivation, because the whole class of bug here is the payload disagreeing
    /// with the stream. `dg_load_a == 0` is `needAudio:false`, which is a complete answer to
    /// "there is no sound": the pipeline was never asked for any.
    pub dg_load_v: AtomicU8,
    pub dg_load_a: AtomicU8,
    /// The demuxer's conditional-probe answers, in `dg_load_v` / `dg_load_a`'s own code spaces
    /// (0 = no disagreement): when the bytes it opened disagree with the Load's declared codec, it
    /// refuses the stream AND records what the file really carries, so the pump's one-shot probe
    /// retry can reload with the corrected declaration. Written ONLY on that failure path — a
    /// healthy open never touches either field, so the happy path pays nothing — and cleared with
    /// `demux_failed` in the session reset.
    pub probe_actual_v: AtomicU8,
    pub probe_actual_a: AtomicU8,
    /// Auto-quality facts written by the demux worker and read through `FeedRaw::abr`.
    /// Mode: 0 inactive, 1 progressive Original watchdog, 2 fixed-session HLS controller. The
    /// adaptive worker's own measurements and decisions are not mailboxes: it ships them to the
    /// player as an `AbrReadout` (`media/src/adaptive.rs`).
    pub dg_abr_mode: AtomicU8,
    /// Current Original source requirement or active HLS rung, in kbit/s.
    pub dg_abr_kbps: AtomicI64,
    /// The current HLS master declaration, an observation of what PMS actually emitted;
    /// `dg_abr_kbps` is the requested ceiling and must never be presented as this.
    pub dg_abr_declared_kbps: AtomicI64,
    /// `vp_place` return, and the size it was called with. `i32::MIN` = never called.
    pub dg_place_rv: AtomicI32,
    pub dg_placed_w: AtomicI32,
    pub dg_placed_h: AtomicI32,
}

/// Native mailbox and gate used by a feed executor.
pub type PhysicalShared = Shared;

/// **The Feed backend's process-lifetime native mailbox and gate.** The common player's own
/// `SHARED` (subtitles, container track names, the audio-pick reason) holds none of it.
///
/// It is a static because its readers cannot be handed a reference: `sf_on_event(epoch, ty, num,
/// s)` is a `#[no_mangle]` firmware callback with no user-data pointer, and the demux, Load and
/// subtitle workers reach it off the main thread. What can be handed is: [`FeedResources`],
/// [`native_executor`] and [`native_release`] take a `&'static PhysicalShared`, and the adapter
/// passes this one in. Media code that still names it directly (`pump`, `feed_pump`,
/// `demux_session`, the callbacks) spells the path out, so `grep FEED_SHARED` finds every user.
///
/// [`FeedResources`]: crate::feed_resources::FeedResources
/// [`native_executor`]: crate::native_executor
/// [`native_release`]: crate::native_release
pub static FEED_SHARED: PhysicalShared = Shared::new();

impl Shared {
    /// Record WHICH demuxer exit failed the session, without raising `demux_failed` (an exit that
    /// reaches the shared "produced no access units" tail through a plain `break`). FIRST WINS: a
    /// compare-exchange from 0, so the real cause is kept and any later cascade is ignored.
    pub fn note_demux_exit_latch(&self, exit_latch: u8) {
        let _ = self
            .dg_demux_exit
            .compare_exchange(0, exit_latch, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// Raise `demux_failed` AND name the exit that raised it (first wins, see `note_demux_exit`).
    /// The one way the demuxer or the Load builder fails the session through that flag.
    pub fn fail_demux_latch(&self, exit_latch: u8) {
        self.note_demux_exit_latch(exit_latch);
        self.demux_failed.store(true, Ordering::Release);
    }

    pub const fn new() -> Self {
        Shared {
            native_session: NativeGate::new(),
            dg_stage: AtomicU8::new(0),
            dg_cb_count: AtomicU32::new(0),
            dg_pos_ticks: AtomicU32::new(0),
            dg_sink_displayed_cb: AtomicU32::new(0),
            dg_sink_other_cb: AtomicU32::new(0),
            dg_feed_state: AtomicU8::new(0),
            dg_cb_err: AtomicI32::new(0),
            dg_cb_err_at: AtomicU32::new(0),
            dg_cb_err_num: AtomicI32::new(0),
            refusal_path: AtomicU8::new(RefusalPath::None as u8),
            dg_http_status: AtomicI32::new(0),
            dg_transport_rc: AtomicI32::new(0),
            dg_transport_age_ms: AtomicI64::new(-1),
            dg_transport_reopen_failed: AtomicBool::new(false),
            dg_demux_exit: AtomicU8::new(0),
            dg_net_rx: AtomicI64::new(0),
            side_anchor: Mutex::new(None),
            side_subs_owner: AtomicBool::new(false),
            dg_load_at: AtomicU32::new(0),
            dg_frame_at: AtomicU32::new(0),
            dg_vpres_ct: AtomicU32::new(0),
            dg_vpres_at: AtomicU32::new(0),
            dg_vpres_gap_sum: AtomicU32::new(0),
            dg_vpres_gap_ct: AtomicU32::new(0),
            dg_aq_video: AtomicI64::new(0),
            dg_aq_audio: AtomicI64::new(0),
            dg_fed_v_pts: AtomicI64::new(0),
            dg_fed_a_pts: AtomicI64::new(0),
            dg_load_v: AtomicU8::new(0),
            dg_load_a: AtomicU8::new(0),
            probe_actual_v: AtomicU8::new(0),
            probe_actual_a: AtomicU8::new(0),
            dg_abr_mode: AtomicU8::new(0),
            dg_abr_kbps: AtomicI64::new(0),
            dg_abr_declared_kbps: AtomicI64::new(-1),
            dg_place_rv: AtomicI32::new(i32::MIN),
            dg_placed_w: AtomicI32::new(0),
            dg_placed_h: AtomicI32::new(0),
            playpos_ns: AtomicI64::new(0),
            reported_playpos_ns: AtomicI64::new(0),
            pres_fed: AtomicI64::new(0),
            frames: AtomicI32::new(0),
            seen_frame: AtomicBool::new(false),
            sink_displayed_live: AtomicBool::new(false),
            sink_displayed_sequence: AtomicU32::new(0),
            first_audio_pts: AtomicI64::new(i64::MIN),
            load_completed: AtomicBool::new(false),
            media_id: Mutex::new(None),
            source_info: Mutex::new(None),
            container_hdr: Mutex::new(None),
            pts_shift: AtomicI64::new(0),
            disp_base: AtomicI64::new(0),
            seek_to_ns: AtomicI64::new(-1),
            seek_target_ns: AtomicI64::new(-1),
            seeking: AtomicBool::new(false),
            seek_display_ns: AtomicI64::new(-1),
            seek_epoch: AtomicU32::new(0),
            seek_landed_epoch: AtomicU32::new(0),
            seek_requeue_epoch: AtomicU32::new(0),
            seek_requeue_target: AtomicI64::new(0),
            desired_audio_idx: AtomicI32::new(-1),
            applied_rate_permille: AtomicI32::new(RATE_NORMAL),
            speed_audio_verdict: AtomicU8::new(0),
            demux_failed: AtomicBool::new(false),
            demux_io_failed: AtomicBool::new(false),
            demux_no_video: AtomicBool::new(false),
            load_failed: AtomicBool::new(false),
            load_timed_out: AtomicBool::new(false),
            decoder_revocations: AtomicU32::new(0),
            native_audio_refusals: AtomicU32::new(0),
            native_load_deferred_logged_epoch: AtomicU32::new(0),
            native_load_elapsed_ms: AtomicU64::new(0),
            file_size: AtomicI64::new(0),
            video_w: AtomicI32::new(0),
            video_h: AtomicI32::new(0),
            video_raster: AtomicU64::new(0),
            video_aspect: AtomicU64::new(0),
            video_fps_milli: AtomicI64::new(0),
            duration_ns: AtomicI64::new(0),
            hls_video_tail_ns: AtomicI64::new(-1),
            hls_audio_tail_ns: AtomicI64::new(-1),
            hls_audio_expected: AtomicBool::new(false),
            hls_prime_runway_ms: AtomicI64::new(0),
            hls_rebuffer_requested: AtomicBool::new(false),
            hls_rebuffer_request_tail_ns: AtomicI64::new(-1),
            hls_rebuffering: AtomicBool::new(false),
            hls_internal_hold_epoch: AtomicU64::new(0),
            hls_clock: Mutex::new(HlsClockState::new()),
            hls_clock_changed: Condvar::new(),
            hls_trial_reserve_ms: AtomicI64::new(-1),
            hls_candidate_generation: AtomicU64::new(0),
            pipeline_eos: AtomicBool::new(false),
            hs_ptr: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    /// Open one native epoch; the feed gate owns its clock and callback admission.
    pub fn begin_native_session(&self) -> Option<u32> {
        self.native_session.begin_native_session()
    }

    /// Publish the Load-returned gate before the addressed receipt enters the inbox.
    pub fn mark_native_load_returned(&self, epoch: u32, pcm_lead_ns: Option<i64>) -> Option<Duration> {
        self.native_session.mark_native_load_returned(epoch, pcm_lead_ns)
    }

    /// Publish synchronous refusal while the exact active epoch holds the callback/reset barrier.
    pub fn publish_native_load_failure(&self, epoch: u32) -> bool {
        self.native_session.with_active_epoch(epoch, || {
            self.note_refusal_path(RefusalPath::Sync);
            self.load_failed.store(true, Ordering::Release);
        })
    }

    /// Record how this session's Load was refused. The first path wins: a later callback must not
    /// rewrite the account of the refusal that ended the open.
    pub fn note_refusal_path(&self, path: RefusalPath) {
        let _ = self.refusal_path.compare_exchange(
            RefusalPath::None as u8,
            path as u8,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }

    /// The refusal path recorded for this session, [`RefusalPath::None`] when there is none.
    pub fn refusal_path(&self) -> RefusalPath {
        RefusalPath::from_u8(self.refusal_path.load(Ordering::Relaxed))
    }

    pub fn native_load_returned(&self, epoch: u32) -> bool {
        self.native_session.native_load_returned(epoch)
    }

    pub fn native_load_pcm_lead(&self, epoch: u32) -> Option<i64> {
        self.native_session.native_load_pcm_lead(epoch)
    }

    pub fn native_load_returned_elapsed(&self, epoch: u32) -> Option<Duration> {
        self.native_session.native_load_returned_elapsed(epoch)
    }

    pub fn native_load_elapsed(&self, epoch: u32) -> Option<Duration> {
        self.native_session.native_load_elapsed(epoch)
    }

    #[cfg(all(feature = "test-support", feature = "hostsim"))]
    pub fn test_backdate_native_load_issued(&self, epoch: u32, by: Duration) -> bool {
        self.native_session.test_backdate_native_load_issued(epoch, by)
    }

    #[cfg(all(feature = "test-support", feature = "hostsim"))]
    pub fn test_backdate_native_load_returned(&self, epoch: u32, by: Duration) -> bool {
        self.native_session.test_backdate_native_load_returned(epoch, by)
    }

    pub fn retire_native_session(&self, epoch: u32) -> bool {
        self.native_session.retire_native_session(epoch)
    }

    pub fn begin_native_release(&self, epoch: u32) -> bool {
        self.native_session.begin_native_release(epoch)
    }

    pub fn abandon_native_session(&self, epoch: u32) -> bool {
        self.native_session.abandon_native_session(epoch)
    }

    pub fn native_releasing(&self) -> bool {
        self.native_session.native_releasing()
    }

    /// The pipeline took the video decoder back: one more revocation this session.
    pub fn raise_decoder_revoked(&self) {
        self.decoder_revocations.fetch_add(1, Ordering::Release);
    }

    /// The pipeline refused natively fed audio (the callback, or the stall watchdog): one more refusal.
    pub fn raise_native_audio_refused(&self) {
        self.native_audio_refusals.fetch_add(1, Ordering::Release);
    }

    pub fn native_load_abandoned(&self) -> bool {
        self.native_session.native_load_abandoned()
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "hostsim"))]
    pub fn test_force_native_idle(&self) {
        self.native_session.test_force_native_idle();
    }

    pub fn native_unload_completed(&self, epoch: u32) -> bool {
        self.native_session.native_unload_completed(epoch)
    }

    pub fn begin_native_media_discontinuity(&self, epoch: u32) -> bool {
        self.native_session.begin_native_media_discontinuity(epoch)
    }

    #[cfg(feature = "test-support")]
    pub fn arm_native_presentations(&self, epoch: u32) -> bool {
        self.native_session.arm_native_presentations(epoch)
    }

    pub fn begin_native_presentation_probe(&self, epoch: u32) -> bool {
        self.native_session.begin_native_presentation_probe(epoch)
    }

    pub fn commit_native_presentation_probe(&self, epoch: u32, replay: impl FnOnce(i64)) -> bool {
        self.native_session.commit_native_presentation_probe(epoch, replay)
    }

    pub fn reject_native_presentation_probe(&self, epoch: u32) -> bool {
        self.native_session.reject_native_presentation_probe(epoch)
    }

    /// Sample or publish one demux observer's facts under the existing retirement barrier.
    /// The closure must contain only bounded state work: no IO, native calls, or nested epoch gate.
    pub fn with_active_demux_epoch<R>(&self, epoch: u32, read: impl FnOnce() -> R) -> Option<R> {
        let mut result = None;
        self.native_session.with_active_epoch(epoch, || result = Some(read()));
        result
    }

    pub fn with_native_session<R>(
        &self,
        epoch: u32,
        class: NativeEventClass,
        num: i64,
        event: impl FnOnce() -> R,
    ) -> Option<R> {
        self.native_session.with_native_session(epoch, class, num, event)
    }

    pub fn with_native_session_scoped<R>(
        &self,
        epoch: u32,
        class: NativeEventClass,
        num: i64,
        event: impl FnOnce(EventScope) -> R,
    ) -> Option<R> {
        self.native_session.with_native_session_scoped(epoch, class, num, event)
    }
    /// Library thread, inside the epoch barrier: the displayed-frame counter reported `num`
    /// frames. Only a positive count is a displayed frame; `0` is the sink saying it has shown
    /// nothing yet. (The callback census, `dg_sink_displayed_cb`, counts every callback and is
    /// written beside this call by `sf_event::note_sink_counter`.)
    pub fn note_displayed_frames(&self, num: i64) {
        if num > 0 {
            self.sink_displayed_live.store(true, Ordering::Relaxed);
            self.sink_displayed_sequence.fetch_add(1, Ordering::Release);
        }
    }

    /// Main thread, when the ACB plane bind completes: a displayed count taken until now was
    /// taken before the plane was connected, so it is not a picture on the panel.
    pub fn forget_displayed_before_bind(&self) {
        self.sink_displayed_live.store(false, Ordering::Relaxed);
    }

    /// Has the sink counted a displayed frame since the plane bind (or since the session began,
    /// where there is no bind to wait for)?
    pub fn displayed_after_bind(&self) -> bool {
        self.sink_displayed_live.load(Ordering::Relaxed)
    }

    pub fn publish_video_raster(&self, width: i32, height: i32) {
        let packed = (u64::from(width as u32) << 32) | u64::from(height as u32);
        self.video_w.store(width, Ordering::Relaxed);
        self.video_h.store(height, Ordering::Relaxed);
        self.video_raster.store(packed, Ordering::Release);
    }

    /// Arm the demuxer to open+seek to `target_ns` on the NEXT Load, displaying honest content
    /// time (`FeedCommand::ArmOpen`). The player passes disp_base=0 and, via start_bufferfeed,
    /// rebase_pending=true, so feed_stream rebases the landed keyframe K to fed-pts 0 and the
    /// presented position reads as num+K = content time; `playpos_ns` gives instant HUD feedback
    /// until frames land. The values arrive clamped (`player::arm_open`). Sent BEFORE
    /// start_bufferfeed (resume) or via reload_at (mid-play seek). Main thread only, and it takes no
    /// `MainThread` token: it publishes to this struct and reaches no slot.
    pub fn arm_open(&self, target_ns: i64, disp_base: i64, playpos_ns: i64) {
        self.seek_to_ns.store(target_ns, Ordering::Release);
        self.disp_base.store(disp_base, Ordering::Relaxed);
        self.playpos_ns.store(playpos_ns, Ordering::Relaxed);
    }

    /// A transcode restarted at a content offset (`FeedCommand::Rebase`): the published position
    /// carries `disp_base` from the first frame and `playpos_ns` gives instant HUD feedback until
    /// frames land. No seek is armed (the encode is 0-based at the offset), and `pts_shift` is the
    /// video lane's alone. Main thread only, like [`Self::arm_open`].
    pub fn rebase(&self, playpos_ns: i64, disp_base: i64) {
        self.disp_base.store(disp_base, Ordering::Relaxed);
        self.playpos_ns.store(playpos_ns, Ordering::Relaxed);
    }

    /// The audio stream the demuxer feeds from its next container open (`FeedCommand::SelectAudio`;
    /// `-1` feeds the file's own default). Not cleared by a session reset: the player's choice
    /// outlives a reload and is sent again before every start.
    pub fn select_audio(&self, idx: i32) {
        self.desired_audio_idx.store(idx, Ordering::Relaxed);
    }

    /// Publish the display aspect the sourceInfo declared, as a reduced `(num, den)`. Packed
    /// `(num << 32) | den` in one word so a reader never sees half of a pair.
    pub fn publish_video_aspect(&self, num: u32, den: u32) {
        self.video_aspect.store((u64::from(num) << 32) | u64::from(den), Ordering::Release);
    }

    /// The published display aspect, `(0, 0)` until one is.
    pub fn video_aspect_pair(&self) -> (u32, u32) {
        let packed = self.video_aspect.load(Ordering::Acquire);
        ((packed >> 32) as u32, packed as u32)
    }

    pub fn video_raster(&self) -> (i32, i32) {
        let packed = self.video_raster.load(Ordering::Acquire);
        ((packed >> 32) as u32 as i32, packed as u32 as i32)
    }

    /// [`Self::reset_session`] for a RELOAD of the same item — everything a fresh Load must not
    /// inherit is cleared; file facts survive: duration and bounded, already-read ASS sources.
    /// The demuxer re-publishes duration when it reopens, a few hundred ms
    /// later, and until then a zero here is a playbar drawn at position ÷ 0 (`engine::teardown`
    /// has the account).
    pub fn reset_session_for_reload(&self) {
        self.reset_scoped(ResetScope::Reload, &mut || {});
    }

    /// Everything here describes one engine's session and must not outlive it. (The Original
    /// failure, which outlives an engine reload, is the player's readout, not a mailbox.)
    pub fn reset_session(&self) {
        self.reset_scoped(ResetScope::Stop, &mut || {});
    }

    /// The session reset of [`plx_playback_contract::feed::FeedStop::reset`]: the mirrors and `common` inside the
    /// callback/reset barrier, and for a reload the duration the demuxer published kept.
    pub fn reset_scoped(&self, scope: ResetScope, common: &mut dyn FnMut()) {
        let duration_ns = self.duration_ns.load(Ordering::Relaxed);
        self.reset_session_with(|| common());
        if scope == ResetScope::Reload {
            self.duration_ns.store(duration_ns, Ordering::Relaxed);
        }
    }

    /// Keep common resource retirement inside the existing callback/reset barrier.
    pub fn reset_session_with(&self, reset_common: impl FnOnce()) {
        self.native_session.reset_session_with(|| {
            self.reset_session_mirrors();
            reset_common();
        });
    }

    fn reset_session_mirrors(&self) {
        // the diagnostics mirror is per-session too — a stale bind outcome from the last item is
        // exactly the misleading answer the read-out exists to avoid
        self.dg_stage.store(0, Ordering::Relaxed);
        self.dg_cb_count.store(0, Ordering::Relaxed);
        self.dg_pos_ticks.store(0, Ordering::Relaxed);
        self.reported_playpos_ns.store(0, Ordering::Relaxed);
        self.dg_sink_displayed_cb.store(0, Ordering::Relaxed);
        self.dg_sink_other_cb.store(0, Ordering::Relaxed);
        self.dg_feed_state.store(0, Ordering::Relaxed);
        // A latched error MUST be cleared here, or one bad session paints every later healthy
        // playback red for the life of the process.
        self.dg_cb_err.store(0, Ordering::Relaxed);
        self.dg_cb_err_at.store(0, Ordering::Relaxed);
        self.dg_cb_err_num.store(0, Ordering::Relaxed);
        self.refusal_path.store(RefusalPath::None as u8, Ordering::Relaxed);
        self.dg_http_status.store(0, Ordering::Relaxed);
        self.dg_transport_rc.store(0, Ordering::Relaxed);
        self.dg_transport_age_ms.store(-1, Ordering::Relaxed);
        self.dg_transport_reopen_failed.store(false, Ordering::Relaxed);
        self.dg_demux_exit.store(0, Ordering::Relaxed);
        self.dg_net_rx.store(0, Ordering::Relaxed);
        self.dg_load_at.store(0, Ordering::Relaxed);
        self.dg_frame_at.store(0, Ordering::Relaxed);
        self.dg_vpres_ct.store(0, Ordering::Relaxed);
        self.dg_vpres_at.store(0, Ordering::Relaxed);
        self.dg_vpres_gap_sum.store(0, Ordering::Relaxed);
        self.dg_vpres_gap_ct.store(0, Ordering::Relaxed);
        self.dg_aq_video.store(0, Ordering::Relaxed);
        self.dg_aq_audio.store(0, Ordering::Relaxed);
        self.dg_fed_v_pts.store(0, Ordering::Relaxed);
        self.dg_fed_a_pts.store(0, Ordering::Relaxed);
        self.dg_load_v.store(0, Ordering::Relaxed);
        self.dg_load_a.store(0, Ordering::Relaxed);
        self.probe_actual_v.store(0, Ordering::Relaxed);
        self.probe_actual_a.store(0, Ordering::Relaxed);
        self.dg_abr_mode.store(0, Ordering::Relaxed);
        self.dg_abr_kbps.store(0, Ordering::Relaxed);
        self.dg_abr_declared_kbps.store(-1, Ordering::Relaxed);
        self.dg_place_rv.store(i32::MIN, Ordering::Relaxed);
        self.dg_placed_w.store(0, Ordering::Relaxed);
        self.dg_placed_h.store(0, Ordering::Relaxed);
        self.playpos_ns.store(0, Ordering::Relaxed);
        self.pres_fed.store(0, Ordering::Relaxed);
        self.frames.store(0, Ordering::Relaxed);
        // a fresh session has shown nothing yet — keep this adjacent to `frames` in all three
        // places (declaration, `new`, here): a reload that forgot the bit would silently suppress
        // the centred read-out for the rest of the app's life.
        self.seen_frame.store(false, Ordering::Relaxed);
        self.sink_displayed_live.store(false, Ordering::Relaxed);
        self.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
        self.load_completed.store(false, Ordering::Relaxed);
        // issue #74 D.1: both are only ever cleared by the loadCompleted arm that consumes them
        // (pump.rs), so a session torn down after logging the deferral but before that arm runs
        // would otherwise leave them set — a later session's "native: Load returned after Nms"
        // could then report a PREVIOUS session's duration, or skip its own deferral log because
        // the epoch marker still matched. Clear both here, unconditionally, like every other
        // per-session diagnostic above.
        self.native_load_deferred_logged_epoch
            .store(0, Ordering::Relaxed);
        self.native_load_elapsed_ms.store(0, Ordering::Relaxed);
        *self.media_id.lock().unwrap() = None;
        *self.source_info.lock().unwrap() = None;
        *self.container_hdr.lock().unwrap() = None;
        self.pts_shift.store(0, Ordering::Relaxed);
        self.disp_base.store(0, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
        self.seek_target_ns.store(-1, Ordering::Relaxed);
        self.seeking.store(false, Ordering::Relaxed);
        self.seek_display_ns.store(-1, Ordering::Relaxed);
        self.seek_epoch.store(0, Ordering::Relaxed);
        self.seek_landed_epoch.store(0, Ordering::Relaxed);
        self.seek_requeue_epoch.store(0, Ordering::Relaxed);
        self.seek_requeue_target.store(0, Ordering::Relaxed);
        // NB: desired_audio_idx is NOT reset here — it persists across seeks/reloads so a
        // native audio-track choice survives seeking. It is reset on a new item (route).
        // The viewer's offered rate lives in the session's readout and persists likewise; the
        // APPLIED rate is the pipeline's, and a fresh one is 1x.
        self.applied_rate_permille.store(RATE_NORMAL, Ordering::Relaxed);
        self.speed_audio_verdict.store(SPEED_AUDIO_UNKNOWN, Ordering::Relaxed);
        self.demux_failed.store(false, Ordering::Relaxed);
        self.demux_io_failed.store(false, Ordering::Relaxed);
        self.demux_no_video.store(false, Ordering::Relaxed);
        self.load_failed.store(false, Ordering::Relaxed);
        self.load_timed_out.store(false, Ordering::Relaxed);
        self.decoder_revocations.store(0, Ordering::Relaxed);
        self.native_audio_refusals.store(0, Ordering::Relaxed);
        self.file_size.store(0, Ordering::Relaxed);
        self.video_w.store(0, Ordering::Relaxed);
        self.video_h.store(0, Ordering::Relaxed);
        self.video_raster.store(0, Ordering::Release);
        self.video_aspect.store(0, Ordering::Release);
        self.video_fps_milli.store(0, Ordering::Relaxed);
        self.duration_ns.store(0, Ordering::Relaxed);
        self.hls_video_tail_ns.store(-1, Ordering::Relaxed);
        self.hls_audio_tail_ns.store(-1, Ordering::Relaxed);
        self.hls_audio_expected.store(false, Ordering::Relaxed);
        self.hls_prime_runway_ms.store(0, Ordering::Relaxed);
        self.hls_rebuffer_requested.store(false, Ordering::Relaxed);
        self.hls_rebuffer_request_tail_ns
            .store(-1, Ordering::Relaxed);
        self.reset_hls_clock();
        self.pipeline_eos.store(false, Ordering::Relaxed);
        self.hs_ptr.store(std::ptr::null_mut(), Ordering::Release);
    }

    /// Start (or restart after a rung commit) the proof for the media that will release this hold.
    #[cfg(all(any(test, feature = "test-support"), feature = "hostsim"))]
    pub fn begin_hls_recovery(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.recovery.begin();
        // Compatibility for the existing direct state-machine tests: production enters Held via
        // `complete_hls_rebuffer_pause`, while those tests construct the same boundary explicitly.
        if matches!(
            clock.phase,
            HlsClockPhase::Running
                | HlsClockPhase::Held {
                    hold: ClockHold::Initial,
                }
        ) {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            };
        }
    }

    /// Credit exactly one fully downloaded, validated and enqueued segment from the active rung.
    pub fn observe_hls_recovery(&self, acquisition_us: u64, media: std::time::Duration) {
        self.hls_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recovery
            .observe(acquisition_us, media);
    }

    pub fn hls_recovery(&self) -> HlsRecoveryEpoch {
        self.hls_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .recovery
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "hostsim"))]
    pub fn finish_hls_recovery(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.recovery = HlsRecoveryEpoch::idle();
        // The older focused tests construct an internal hold by publishing the mirrors directly.
        // Restore the synchronized half as well, otherwise a refused-Play case leaves `Held` behind
        // and whichever serial test happens to run next cannot issue its initial Play.
        if !self.hls_rebuffering.load(Ordering::Acquire)
            && !matches!(
                clock.phase,
                HlsClockPhase::PlayIssuing { .. } | HlsClockPhase::Stopping
            )
        {
            clock.phase = HlsClockPhase::Running;
            self.hls_clock_changed.notify_all();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn reset_hls_clock_for_test(&self) {
        self.reset_hls_clock();
    }

    /// Put a newly constructed native pipeline into its real pre-Play state. This happens before
    /// either worker is spawned, so neither an ABR trial nor a user command can mistake "loaded but
    /// not started" for a running clock. `user_held` is the durable viewer intent carried across an
    /// engine reload; `seek_preroll` means that intent is temporarily allowed to decode one frame.
    pub fn arm_initial_clock_hold(&self, user_held: bool, seek_preroll: bool) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase != HlsClockPhase::Running
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
        {
            return false;
        }
        clock.phase = HlsClockPhase::Held {
            hold: ClockHold::Initial,
        };
        clock.user_held = user_held && !seek_preroll;
        clock.resume_acb_pending = user_held && seek_preroll;
        true
    }

    /// A start that armed the initial hold but failed before installing an Engine owns no native
    /// clock. Invalidate every candidate/trial fence and return to the no-session state so retrying
    /// the same item cannot inherit `Held` forever.
    pub fn cancel_clock_session_start(&self) {
        self.reset_hls_clock();
    }

    /// Whether the native clock is held for the VIEWER's pause, which fences the start's first
    /// Play behind a prime (`reserve_hls_prime_play`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn clock_user_held_for_test(&self) -> bool {
        self.hls_clock.lock().unwrap_or_else(|e| e.into_inner()).user_held
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "hostsim"))]
    pub fn ensure_initial_clock_hold_for_test(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase == HlsClockPhase::Running {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::Initial,
            };
        }
    }

    /// Reserve an internal Pause and start accepting recovery segments BEFORE the FFI call. A
    /// segment committed in the call window is retained if Pause succeeds and discarded if it is
    /// refused, so the first useful recovery object can no longer disappear between two flags.
    pub fn prepare_hls_rebuffer_pause(&self) -> Option<u64> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        // Trial and Pause spend the same reserve. Whichever reserves this mutex first owns it; the
        // loser retries after the transaction settles instead of creating two physical clocks.
        if clock.phase != HlsClockPhase::Running
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        {
            return None;
        }
        let token = clock.token();
        clock.recovery.begin();
        clock.phase = HlsClockPhase::PauseIssuing {
            token,
            hold: ClockHold::Rebuffer,
        };
        Some(token)
    }

    pub fn complete_hls_rebuffer_pause(
        &self,
        token: u64,
        accepted: bool,
    ) -> HlsPauseCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase
            != (HlsClockPhase::PauseIssuing {
                token,
                hold: ClockHold::Rebuffer,
            })
        {
            return HlsPauseCompletion::Stale;
        }
        if accepted {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            };
            self.hls_rebuffering.store(true, Ordering::Release);
            self.hls_internal_hold_epoch.fetch_add(1, Ordering::AcqRel);
        } else {
            clock.phase = HlsClockPhase::Running;
            clock.recovery = HlsRecoveryEpoch::idle();
        }
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPauseCompletion::Accepted
        } else {
            HlsPauseCompletion::Refused
        }
    }

    /// Reserve a viewer Pause through the same actuator protocol as automatic rebuffering. If the
    /// internal controller already owns a physical hold, only the orthogonal user hold is added and
    /// no redundant native Pause is issued.
    pub fn prepare_hls_user_pause(&self) -> Option<HlsUserPause> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase == HlsClockPhase::Stopping {
            return None;
        }
        if clock.user_held {
            return Some(HlsUserPause::AlreadyHeld);
        }
        if matches!(clock.phase, HlsClockPhase::Held { .. }) {
            clock.user_held = true;
            clock.resume_acb_pending = false;
            clock.advance_user_sequence();
            return Some(HlsUserPause::AlreadyHeld);
        }
        if clock.phase != HlsClockPhase::Running {
            return None;
        }
        let token = clock.token();
        clock.phase = HlsClockPhase::PauseIssuing {
            token,
            hold: ClockHold::User,
        };
        Some(HlsUserPause::Issue(token))
    }

    pub fn complete_hls_user_pause(&self, token: u64, accepted: bool) -> HlsPauseCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase
            != (HlsClockPhase::PauseIssuing {
                token,
                hold: ClockHold::User,
            })
        {
            return HlsPauseCompletion::Stale;
        }
        if accepted {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::User,
            };
            clock.user_held = true;
            clock.resume_acb_pending = false;
            clock.advance_user_sequence();
        } else {
            clock.phase = HlsClockPhase::Running;
        }
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPauseCompletion::Accepted
        } else {
            HlsPauseCompletion::Refused
        }
    }

    /// Reserve the native Pause that makes an in-place seek safe. A seek requested from an
    /// existing user hold reuses that physical Pause, but temporarily transfers ownership to the
    /// seek prime so exactly one landed frame may be decoded before the user hold is restored.
    pub fn prepare_seek_pause(&self) -> Option<HlsSeekPause> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        match clock.phase {
            HlsClockPhase::Running => {
                let token = clock.token();
                clock.phase = HlsClockPhase::PauseIssuing {
                    token,
                    hold: ClockHold::Seek,
                };
                Some(HlsSeekPause::Issue(token))
            }
            HlsClockPhase::Held {
                hold:
                    ClockHold::User | ClockHold::Initial | ClockHold::Seek | ClockHold::ResumePrime,
            } => {
                if clock.user_held {
                    clock.user_held = false;
                    clock.resume_acb_pending = true;
                }
                clock.phase = HlsClockPhase::Held {
                    hold: ClockHold::Seek,
                };
                Some(HlsSeekPause::AlreadyHeld)
            }
            HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            }
            | HlsClockPhase::PauseIssuing { .. }
            | HlsClockPhase::PlayIssuing { .. }
            | HlsClockPhase::Stopping => None,
        }
    }

    /// A viewer Pause that lands on a clock a seek or an Initial hold already owns marks it
    /// `user_held`, and `reserve_hls_prime_play` refuses a Play on a user-held clock: the keyframe
    /// would be fed through the preroll's open gate and never be allowed to present. When the pump
    /// opens a preroll for such a Pause it hands the hold back to the seek prime exactly as
    /// [`Self::prepare_seek_pause`] does for a seek dispatched from a user hold. `false` when the
    /// clock is not held (transitioning, running, rebuffering): the caller tries again next tick.
    pub fn hand_hold_to_seek_prime(&self) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        let HlsClockPhase::Held {
            hold: ClockHold::User | ClockHold::Initial | ClockHold::Seek | ClockHold::ResumePrime,
        } = clock.phase
        else {
            return false;
        };
        if clock.user_held {
            clock.user_held = false;
            clock.resume_acb_pending = true;
        }
        clock.phase = HlsClockPhase::Held { hold: ClockHold::Seek };
        true
    }

    pub fn complete_seek_pause(&self, token: u64, accepted: bool) -> HlsPauseCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase
            != (HlsClockPhase::PauseIssuing {
                token,
                hold: ClockHold::Seek,
            })
        {
            return HlsPauseCompletion::Stale;
        }
        clock.phase = if accepted {
            HlsClockPhase::Held {
                hold: ClockHold::Seek,
            }
        } else {
            HlsClockPhase::Running
        };
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPauseCompletion::Accepted
        } else {
            HlsPauseCompletion::Refused
        }
    }

    /// Accept the viewer's Resume intent. Queued streams transfer an ordinary user hold to an
    /// explicit resume-prime hold: the caller opens feeding, while `try_prime` remains the sole
    /// owner of the eventual Play + ACB Resume. A candidate publication, seek or internal recovery
    /// already holding the clock keeps its own certificate and only records the deferred ACB half.
    pub fn prepare_hls_user_resume(&self, queued_stream: bool) -> Option<HlsUserResume> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase == HlsClockPhase::Stopping || !clock.user_held {
            return None;
        }
        let HlsClockPhase::Held { hold } = clock.phase else {
            return None;
        };
        if hold != ClockHold::User {
            clock.user_held = false;
            clock.resume_acb_pending = true;
            clock.advance_user_sequence();
            return Some(HlsUserResume::Deferred);
        }
        if queued_stream {
            clock.phase = HlsClockPhase::Held {
                hold: ClockHold::ResumePrime,
            };
            clock.user_held = false;
            clock.resume_acb_pending = true;
            clock.advance_user_sequence();
            return Some(HlsUserResume::Prime);
        }
        let token = clock.token();
        clock.phase = HlsClockPhase::PlayIssuing {
            token,
            hold,
            resume_acb: true,
        };
        Some(HlsUserResume::Issue(token))
    }

    /// Snapshot which media certificate currently owns a held clock. The mutex phase is the
    /// authority; `hls_rebuffering` remains only a worker/diagnostics mirror and cannot classify a
    /// ResumePrime without racing a user transition.
    pub fn hls_prime_kind(&self) -> Option<HlsPrimeKind> {
        let clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        let HlsClockPhase::Held { hold } = clock.phase else {
            return None;
        };
        hold.prime_kind()
    }

    /// Final check-and-reserve for `sf_play`. Candidate/trial arm uses the same mutex, eliminating
    /// the old check/use gap between the last seqlock read and the native Play call.
    pub fn reserve_hls_prime_play(
        &self,
        expected_kind: HlsPrimeKind,
        expected_candidate: u64,
        expected_recovery: HlsRecoveryEpoch,
    ) -> Option<(u64, HlsRecoveryEpoch)> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
            || clock.automatic.is_some()
            || self.hls_candidate_generation.load(Ordering::Acquire) != expected_candidate
            || expected_candidate & 1 != 0
            || (expected_kind == HlsPrimeKind::Rebuffer && clock.recovery != expected_recovery)
            || clock.user_held
        {
            return None;
        }
        let HlsClockPhase::Held { hold } = clock.phase else {
            return None;
        };
        if hold.prime_kind() != Some(expected_kind) {
            return None;
        }
        let token = clock.token();
        let recovery = clock.recovery;
        let resume_acb = clock.resume_acb_pending;
        clock.phase = HlsClockPhase::PlayIssuing {
            token,
            hold,
            resume_acb,
        };
        Some((token, recovery))
    }

    pub fn complete_hls_prime_play(&self, token: u64, accepted: bool) -> HlsPlayCompletion {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        let HlsClockPhase::PlayIssuing {
            token: active,
            hold,
            resume_acb,
        } = clock.phase
        else {
            return HlsPlayCompletion::Stale;
        };
        if active != token {
            return HlsPlayCompletion::Stale;
        }
        if accepted {
            clock.phase = HlsClockPhase::Running;
            clock.recovery = HlsRecoveryEpoch::idle();
            clock.user_held = false;
            clock.resume_acb_pending = false;
            if hold == ClockHold::User {
                clock.advance_user_sequence();
            }
            if hold == ClockHold::Rebuffer {
                self.hls_rebuffering.store(false, Ordering::Release);
            }
        } else {
            clock.phase = HlsClockPhase::Held { hold };
            if hold == ClockHold::User {
                clock.user_held = true;
            };
        }
        self.hls_clock_changed.notify_all();
        if accepted {
            HlsPlayCompletion::Accepted { resume_acb }
        } else {
            HlsPlayCompletion::Refused
        }
    }

    pub fn begin_hls_candidate_transition(
        &self,
        expected_automatic: Option<u64>,
    ) -> Result<u64, HlsClockFenceError> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        while matches!(clock.phase, HlsClockPhase::PlayIssuing { .. }) {
            clock = self
                .hls_clock_changed
                .wait(clock)
                .unwrap_or_else(|e| e.into_inner());
        }
        if clock.phase == HlsClockPhase::Stopping {
            return Err(HlsClockFenceError::Stopping);
        }
        if let Some(expected) = expected_automatic {
            let Some(owner) = clock.automatic else {
                return Err(HlsClockFenceError::Superseded);
            };
            let phase_ok = match owner.kind {
                HlsAutomaticTransition::QualityUp | HlsAutomaticTransition::Original => {
                    clock.phase == HlsClockPhase::Running
                        && !self.hls_rebuffer_requested.load(Ordering::Acquire)
                }
                HlsAutomaticTransition::QualityDown => matches!(
                    clock.phase,
                    HlsClockPhase::Running
                        | HlsClockPhase::PauseIssuing {
                            hold: ClockHold::Rebuffer,
                            ..
                        }
                        | HlsClockPhase::Held {
                            hold: ClockHold::Rebuffer,
                        }
                ),
            };
            if owner.token != expected
                || owner.phase != HlsAutomaticPhase::Working
                || owner.user_sequence != clock.user_sequence
                || clock.user_held
                || !phase_ok
            {
                return Err(HlsClockFenceError::Superseded);
            }
            clock
                .automatic
                .as_mut()
                .expect("the validated automatic owner remains present")
                .phase = HlsAutomaticPhase::CommitAuthorized;
        }
        let stable = self.hls_candidate_generation.load(Ordering::Acquire);
        if stable & 1 != 0 {
            return Err(HlsClockFenceError::Overlap);
        }
        let generation = stable.checked_add(1).ok_or(HlsClockFenceError::Exhausted)?;
        self.hls_candidate_generation
            .compare_exchange(stable, generation, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| generation)
            .map_err(|_| HlsClockFenceError::Overlap)
    }

    pub fn settle_hls_candidate_transition(&self, generation: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        // The mutex phase is authoritative. During PauseIssuing the diagnostics mirror is still
        // false, but the recovery epoch already exists and a rung publication must rebase it.
        if matches!(
            clock.phase,
            HlsClockPhase::PauseIssuing {
                hold: ClockHold::Rebuffer,
                ..
            } | HlsClockPhase::Held {
                hold: ClockHold::Rebuffer,
            }
        ) {
            clock.recovery.begin();
        }
        let Some(settled) = generation.checked_add(1) else {
            // Remain odd/fenced forever rather than letting an ancient generation become current.
            return false;
        };
        let accepted = self
            .hls_candidate_generation
            .compare_exchange(generation, settled, Ordering::Release, Ordering::Acquire)
            .is_ok();
        if accepted {
            self.hls_clock_changed.notify_all();
        }
        accepted
    }

    pub fn hls_user_sequence(&self) -> u64 {
        self.hls_clock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .user_sequence
    }

    pub fn arm_hls_trial_at(
        &self,
        runway_ms: i64,
        expected_user_sequence: u64,
    ) -> Option<u64> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.phase != HlsClockPhase::Running
            || clock.user_held
            || clock.user_sequence != expected_user_sequence
            || clock.automatic.is_some()
            || self.hls_rebuffer_requested.load(Ordering::Acquire)
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
        {
            return None;
        }
        let token = clock.token();
        clock.automatic = Some(HlsAutomaticOwner {
            token,
            kind: HlsAutomaticTransition::QualityUp,
            phase: HlsAutomaticPhase::Working,
            user_sequence: clock.user_sequence,
        });
        self.hls_trial_reserve_ms
            .store(runway_ms.max(0), Ordering::Release);
        Some(token)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn arm_hls_trial(&self, runway_ms: i64) -> Option<u64> {
        let sequence = self.hls_user_sequence();
        self.arm_hls_trial_at(runway_ms, sequence)
    }

    pub fn finish_hls_trial(&self, token: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(
            clock.automatic,
            Some(HlsAutomaticOwner {
                token: active,
                kind: HlsAutomaticTransition::QualityUp,
                ..
            }) if active == token
        ) {
            return false;
        }
        clock.automatic = None;
        self.hls_trial_reserve_ms.store(-1, Ordering::Release);
        self.hls_clock_changed.notify_all();
        true
    }

    pub fn begin_hls_automatic_transition_at(
        &self,
        kind: HlsAutomaticTransition,
        expected_user_sequence: u64,
    ) -> Option<u64> {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.automatic.is_some()
            || clock.user_held
            || clock.user_sequence != expected_user_sequence
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        {
            return None;
        }
        let phase_ok = match kind {
            HlsAutomaticTransition::QualityUp | HlsAutomaticTransition::Original => {
                clock.phase == HlsClockPhase::Running
                    && !self.hls_rebuffer_requested.load(Ordering::Acquire)
            }
            HlsAutomaticTransition::QualityDown => matches!(
                clock.phase,
                HlsClockPhase::Running
                    | HlsClockPhase::PauseIssuing {
                        hold: ClockHold::Rebuffer,
                        ..
                    }
                    | HlsClockPhase::Held {
                        hold: ClockHold::Rebuffer,
                    }
            ),
        };
        if !phase_ok {
            return None;
        }
        let token = clock.token();
        clock.automatic = Some(HlsAutomaticOwner {
            token,
            kind,
            phase: HlsAutomaticPhase::Working,
            user_sequence: clock.user_sequence,
        });
        Some(token)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn begin_hls_automatic_transition(
        &self,
        kind: HlsAutomaticTransition,
    ) -> Option<u64> {
        let sequence = self.hls_user_sequence();
        self.begin_hls_automatic_transition_at(kind, sequence)
    }

    pub fn authorize_hls_automatic_transition_commit(&self, token: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.user_held || clock.phase != HlsClockPhase::Running {
            return false;
        }
        let user_sequence = clock.user_sequence;
        let Some(owner) = clock.automatic.as_mut() else {
            return false;
        };
        if owner.token != token
            || owner.phase != HlsAutomaticPhase::Working
            || owner.user_sequence != user_sequence
        {
            return false;
        }
        owner.phase = HlsAutomaticPhase::CommitAuthorized;
        true
    }

    pub fn finish_hls_automatic_transition(&self, token: u64) -> bool {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if !matches!(clock.automatic, Some(HlsAutomaticOwner { token: active, .. }) if active == token)
        {
            return false;
        }
        clock.automatic = None;
        self.hls_clock_changed.notify_all();
        true
    }

    /// One synchronized authority check for every automatic actuator. Diagnostics atomics are
    /// mirrors; they cannot decide whether a user hold, native clock transition, candidate
    /// publication, or another private trial owns the boundary.
    pub fn hls_automatic_transition_permitted(&self, kind: HlsAutomaticTransition) -> bool {
        let clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        if clock.user_held
            || clock.automatic.is_some()
            || self.hls_candidate_generation.load(Ordering::Acquire) & 1 != 0
            || self.hls_trial_reserve_ms.load(Ordering::Acquire) >= 0
        {
            return false;
        }
        match kind {
            HlsAutomaticTransition::QualityUp | HlsAutomaticTransition::Original => {
                clock.phase == HlsClockPhase::Running
                    && !self.hls_rebuffer_requested.load(Ordering::Acquire)
            }
            HlsAutomaticTransition::QualityDown => matches!(
                clock.phase,
                HlsClockPhase::Running
                    | HlsClockPhase::PauseIssuing {
                        hold: ClockHold::Rebuffer,
                        ..
                    }
                    | HlsClockPhase::Held {
                        hold: ClockHold::Rebuffer,
                    }
            ),
        }
    }

    pub fn stop_hls_clock(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        clock.phase = HlsClockPhase::Stopping;
        clock.recovery = HlsRecoveryEpoch::idle();
        clock.user_held = false;
        clock.resume_acb_pending = false;
        clock.automatic = None;
        self.hls_rebuffering.store(false, Ordering::Release);
        self.hls_clock_changed.notify_all();
    }

    fn reset_hls_clock(&self) {
        let mut clock = self.hls_clock.lock().unwrap_or_else(|e| e.into_inner());
        // Session identity changes, token identity does not restart. A worker from the retired
        // session may still drop its RAII lease after reset; preserving this counter makes that
        // completion stale instead of letting token 1 clear the next session's token 1.
        let next_token = clock.next_token;
        let user_sequence = clock.user_sequence;
        *clock = HlsClockState::new();
        clock.next_token = next_token;
        clock.user_sequence = user_sequence;
        self.hls_rebuffering.store(false, Ordering::Relaxed);
        self.hls_internal_hold_epoch.fetch_add(1, Ordering::AcqRel);
        self.hls_trial_reserve_ms.store(-1, Ordering::Relaxed);
        // Session reset itself invalidates every prime snapshot. Preserve monotonicity rather than
        // writing zero: a reader spanning teardown must observe a different even generation.
        // The same read-modify-write loop `fetch_update` runs internally; see
        // `player::report::requested` for why it is spelled out rather than called.
        let mut current = self.hls_candidate_generation.load(Ordering::Acquire);
        loop {
            let next = if current & 1 == 0 {
                current.checked_add(2).unwrap_or(u64::MAX)
            } else {
                current.checked_add(1).unwrap_or(u64::MAX)
            };
            match self.hls_candidate_generation.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
        self.hls_clock_changed.notify_all();
    }
}

/// `Transport::preroll_volume` while the sink is not muted (a saved volume is 0..=100).
const PREROLL_VOLUME_NONE: i32 = i32::MIN;

/// UI-facing transport state. Main-thread-only in practice (plex_run + pump +
/// player_hud all run on M), but exposed as atomics so the app's per-frame loop reads it
/// with plain .load()/.store(); the HUD's draw path sees it only through the `HudModel` that
/// loop fills. Replaces the #[no_mangle] transport globals.
pub struct Transport {
    pub started: AtomicBool, // bf_started
    pub paused: AtomicBool,  // pl_paused
    /// Monotone duration of native-accepted user Pause. The bool alone loses a Pause->Resume
    /// interval that begins and ends while the demuxer is blocked, so bounded recovery work uses
    /// this synchronized clock to subtract exactly those intervals from elapsed time.
    pause_clock: Mutex<UserPauseClock>,
    /// The viewer remains logically paused while a seek is allowed to feed, Play one landed frame,
    /// then re-pause. Keeping this separate prevents the UI/user intent from lying during preroll.
    seek_preroll: AtomicBool,
    /// The seek preroll above was opened by the PUMP, not by a caller, and the pump closes it: the
    /// target (ns) the seek was dispatched at, `-1` when the preroll is absent or a caller's.
    /// Transport-wide on purpose: a reload (`reset_for_reload`) replaces the Engine but keeps the
    /// viewer's Pause and this ownership, so the landing is settled on whichever Engine is live.
    seek_preroll_owned: AtomicI64,
    /// The sink volume the paused preroll's Play window muted (`player::mute_for_preroll_on`) saved for
    /// its restore, `PREROLL_VOLUME_NONE` when the pipeline is not muted. The volume is PER PIPELINE:
    /// a stop unloads it (`reset` forgets this with it) and a reload's fresh Load starts at the
    /// default again, so only a still-live pipeline ever needs the restore
    /// (`player::restore_preroll_volume`: resume, the accepted re-pause, and the pump's per-tick sweep).
    preroll_volume: AtomicI32,
    /// The feed's copy of the pending seek (-1 = none). It gates feeding while a seek waits (no AU
    /// is fed with one armed) and is written ONLY by commands: `Seek` and `SeekRequeued` arm it,
    /// `SeekTaken` clears it, and the native-Seek-refused downgrade re-arms it itself. The
    /// player's own pending target, and how many requests it folded, are its readout.
    pub seek_to_ns: AtomicI64,
}

/// The instant the user-pause clock reads for a transition or a sample. `Instant::now()`, except
/// that a host test may pin it for its own thread (`pinned_pause_clock`) so a Pause->Resume
/// cycle can be given an exact length instead of being slept through.
fn pause_clock_now() -> Instant {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(pinned) = pinned_pause_clock::read() {
        return pinned;
    }
    Instant::now()
}

/// A per-thread virtual reading for `pause_clock_now`: time moves only when `advance` is called.
/// Dropping the guard restores the real clock for the thread.
#[cfg(any(test, feature = "test-support"))]
pub mod pinned_pause_clock {
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    thread_local! {
        static PINNED: Cell<Option<Instant>> = const { Cell::new(None) };
    }

    pub(super) fn read() -> Option<Instant> { PINNED.with(Cell::get) }

    pub struct Pinned;

    impl Pinned {
        /// Pin this thread's pause clock at the real time now.
        pub fn now() -> Self {
            PINNED.with(|pinned| pinned.set(Some(Instant::now())));
            Pinned
        }

        pub fn advance(&self, by: Duration) {
            PINNED.with(|pinned| pinned.set(pinned.get().map(|at| at + by)));
        }
    }

    impl Drop for Pinned {
        fn drop(&mut self) { PINNED.with(|pinned| pinned.set(None)); }
    }
}

#[derive(Clone, Copy)]
struct UserPauseClock {
    completed: Duration,
    active_since: Option<Instant>,
    sequence: u64,
}

impl UserPauseClock {
    const fn new() -> Self {
        Self {
            completed: Duration::ZERO,
            active_since: None,
            sequence: 0,
        }
    }

    fn elapsed_at(self, now: Instant) -> Duration {
        self.completed.saturating_add(
            self.active_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since)),
        )
    }
}

/// The accepted-transport state a worker samples: the monotone sequence of Pause/Resume pairs and
/// the cumulative time spent paused.
#[derive(Clone, Copy, Debug)]
pub struct UserPauseSnapshot {
    pub now: std::time::Instant,
    pub elapsed: Duration,
    pub paused: bool,
    pub sequence: u64,
}

/// Per-worker cursor over accepted transport events. A complete Pause→Resume may happen while a
/// worker is blocked in native queue/network I/O; consuming the monotone sequence plus cumulative
/// duration makes that interval observable even when both surrounding boolean samples are false.
#[derive(Clone, Copy, Debug)]
pub struct UserPauseCursor {
    sequence: u64,
    elapsed: Duration,
}

impl UserPauseCursor {
    pub fn new(snapshot: UserPauseSnapshot) -> Self {
        Self {
            sequence: snapshot.sequence,
            elapsed: snapshot.elapsed,
        }
    }

    pub fn consume_completed(&mut self, snapshot: UserPauseSnapshot) -> Option<Duration> {
        if snapshot.paused || snapshot.sequence == self.sequence {
            return None;
        }
        let elapsed = snapshot.elapsed.saturating_sub(self.elapsed);
        self.sequence = snapshot.sequence;
        self.elapsed = snapshot.elapsed;
        (!elapsed.is_zero()).then_some(elapsed)
    }
}

/// **The Feed backend's process-lifetime transport** (pause, seek intent, preroll). The common
/// player's own `TX` holds only what the player screen publishes (`hud_until`, `scrub_ns`).
/// A static for [`FEED_SHARED`]'s reason: the app loop, the pump and the HUD read it with no
/// executor handle in reach.
pub static FEED_TX: Transport = Transport::new();

/// Monotonic milliseconds and microseconds, on the one process clock the player shares (`plx_base::clock`).
pub use plx_base::clock::{vclock_ms, vclock_us};

/// **Film time to the pipeline's timeline.** At speed `r` the pipeline still plays at its own 1x,
/// so every timestamp the app hands it — a fed AU, a seek or decode position — is divided by `r`;
/// the frames then arrive `r` times closer together and the picture runs `r` times faster.
/// Identity at 1x, which is every playback that has not picked a speed.
pub fn to_sink_ns(ns: i64) -> i64 {
    scale_ns(ns, RATE_NORMAL, FEED_SHARED.applied_rate_permille.load(Ordering::Relaxed))
}
/// The inverse, for every timestamp the pipeline reports back (a presented frame).
pub fn from_sink_ns(ns: i64) -> i64 {
    scale_ns(ns, FEED_SHARED.applied_rate_permille.load(Ordering::Relaxed), RATE_NORMAL)
}
fn scale_ns(ns: i64, num: i32, den: i32) -> i64 {
    if num == den || den <= 0 {
        return ns;
    }
    (ns as i128 * num as i128 / den as i128) as i64
}


impl Transport {
    pub const fn new() -> Self {
        Transport {
            started: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            pause_clock: Mutex::new(UserPauseClock::new()),
            seek_preroll: AtomicBool::new(false),
            seek_preroll_owned: AtomicI64::new(-1),
            preroll_volume: AtomicI32::new(PREROLL_VOLUME_NONE),
            seek_to_ns: AtomicI64::new(-1),
        }
    }

    /// Publish a committed physical transport transition to both the feed gate and the exact
    /// pause clock. An active object reaches this only after native acceptance; before a new
    /// object exists, the common foreground owner may seed its saved paused viewer goal as the
    /// initial hold. Refused native transitions never enter either state.
    pub fn commit_paused(&self, paused: bool) {
        let mut clock = self.pause_clock.lock().unwrap_or_else(|e| e.into_inner());
        // The timestamp belongs to the mutex-order transition. Sampling it before `lock` lets a
        // concurrent clock snapshot linearize first while this transition carries an earlier
        // time, manufacturing paused duration before the transaction's own W0.
        let now = pause_clock_now();
        match (clock.active_since, paused) {
            (None, true) => {
                clock.active_since = Some(now);
                clock.sequence = clock
                    .sequence
                    .checked_add(1)
                    .expect("accepted Pause sequence exhausted");
            }
            (Some(since), false) => {
                clock.completed = clock
                    .completed
                    .saturating_add(now.saturating_duration_since(since));
                clock.active_since = None;
                clock.sequence = clock
                    .sequence
                    .checked_add(1)
                    .expect("accepted Pause sequence exhausted");
            }
            _ => {}
        }
        self.paused.store(paused, Ordering::Release);
    }

    /// One synchronized sample for an unpaused-elapsed transaction clock. `Instant` and the
    /// cumulative accepted-Pause duration are observed under the transition mutex, so a complete
    /// Pause->Resume inside a blocked network call can never disappear between two bool reads.
    pub fn pause_clock_sample(&self) -> (Instant, Duration) {
        let sample = self.pause_state_sample();
        (sample.now, sample.elapsed)
    }

    pub fn pause_state_sample(&self) -> UserPauseSnapshot {
        let clock = self.pause_clock.lock().unwrap_or_else(|e| e.into_inner());
        let now = pause_clock_now();
        UserPauseSnapshot {
            now,
            elapsed: clock.elapsed_at(now),
            paused: clock.active_since.is_some(),
            sequence: clock.sequence,
        }
    }

    pub fn begin_paused_seek(&self) -> bool {
        if !self.paused.load(Ordering::Acquire) {
            return false;
        }
        self.seek_preroll.store(true, Ordering::Release);
        true
    }

    pub fn seek_preroll_active(&self) -> bool {
        self.seek_preroll.load(Ordering::Acquire)
    }

    /// Feeding is an actuator concern, not the user-visible pause intent. A paused seek is the one
    /// bounded exception: it must decode a landed frame while the transport still says Paused.
    pub fn feed_allowed(&self) -> bool {
        !self.paused.load(Ordering::Acquire) || self.seek_preroll_active()
    }

    /// Open the paused-seek preroll for a seek the PUMP dispatched under a viewer Pause, and take
    /// ownership of closing it (`pump::settle_paused_seek`). A preroll a caller already opened (the
    /// foreground restore) stays the caller's: `false`. Re-opening an owned one just retargets it
    /// (a coalesced tap). Not paused: nothing to hold, `false`.
    pub fn own_paused_seek(&self, target_ns: i64) -> bool {
        if self.seek_preroll_owned.load(Ordering::Acquire) >= 0 {
            self.seek_preroll_owned.store(target_ns.max(0), Ordering::Release);
            return true;
        }
        if self.seek_preroll_active() || !self.begin_paused_seek() {
            return false;
        }
        self.seek_preroll_owned.store(target_ns.max(0), Ordering::Release);
        true
    }

    /// The target of the preroll the pump owns, if it owns one.
    pub fn owned_paused_seek(&self) -> Option<i64> {
        let target = self.seek_preroll_owned.load(Ordering::Acquire);
        (target >= 0 && self.seek_preroll_active()).then_some(target)
    }

    /// Closing the preroll ends any ownership of it, so a failed re-pause (which leaves the preroll
    /// open) keeps the pump retrying, and every other closer (Resume, an abandoned seek, a stop)
    /// drops it without the pump re-pausing a film the viewer has since released.
    pub fn finish_seek_preroll(&self) {
        self.seek_preroll.store(false, Ordering::Release);
        self.seek_preroll_owned.store(-1, Ordering::Release);
    }

    /// The paused preroll muted the sink; `saved` (0..=100) is what [`take_preroll_volume`]
    /// returns. A second mute (a reload's fresh pipeline inside one open preroll) keeps the first
    /// saved value (always the pipeline default today: the volume is never read back).
    ///
    /// [`take_preroll_volume`]: Transport::take_preroll_volume
    pub fn note_preroll_muted(&self, saved: i32) {
        let _ = self.preroll_volume.compare_exchange(
            PREROLL_VOLUME_NONE,
            saved.clamp(0, 100),
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Whether a mute is outstanding.
    pub fn preroll_muted(&self) -> bool {
        self.preroll_volume.load(Ordering::Acquire) != PREROLL_VOLUME_NONE
    }

    /// Take the volume to restore, clearing the mute. `None` when nothing is muted.
    pub fn take_preroll_volume(&self) -> Option<i32> {
        match self.preroll_volume.swap(PREROLL_VOLUME_NONE, Ordering::AcqRel) {
            PREROLL_VOLUME_NONE => None,
            v => Some(v),
        }
    }

    /// An engine reload is not a new playback and must not erase viewer Pause or the paused-seek
    /// handoff. It only retires engine-local transport mailboxes.
    ///
    /// **`scrub_ns` is NOT among them any more** (restructure §2.3, phase 9). The scrub preview is
    /// the `PlayerScreen`'s own `scrub.ns` and this atomic is the PUBLISHED copy of it, so a store
    /// here was a second owner writing over the first — and it was redundant besides: every seek
    /// that can cause a reload is asked for by that screen, which retires its own preview in the
    /// same step (`commit_scrub`; through phase 12 `app::playback::commit_seek` did it instead,
    /// which was the same cross-owner write one level down). The one reload nobody asks for (the
    /// OS taking the screen) delivers `PlayerScreen::transport_reset(true)` from the loop's
    /// background arm.
    pub fn reset_for_reload(&self) {
        self.started.store(false, Ordering::Relaxed);
        self.seek_to_ns.store(-1, Ordering::Relaxed);
    }

    /// reset on stop (mirrors the transport tail of stop_bufferfeed).
    pub fn reset(&self) {
        self.started.store(false, Ordering::Relaxed);
        *self.pause_clock.lock().unwrap_or_else(|e| e.into_inner()) = UserPauseClock::new();
        self.paused.store(false, Ordering::Release);
        self.seek_preroll.store(false, Ordering::Relaxed);
        self.seek_preroll_owned.store(-1, Ordering::Relaxed);
        // The pipeline a mute belonged to is unloaded with the stop; the next one starts unmuted.
        self.preroll_volume.store(PREROLL_VOLUME_NONE, Ordering::Relaxed);
        // …and not `scrub_ns`, for `reset_for_reload`'s reason: a new session is a fresh
        // `PlayerScreen` (or, on an auto-advance chain, `start_playback`'s in-place reset of the
        // live one), and both publish `Scrub::IDLE` themselves.
        self.seek_to_ns.store(-1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demux_observer_cannot_publish_or_sample_a_successors_native_epoch() {
        let shared = Shared::new();
        let first = shared.begin_native_session().expect("first native epoch");
        assert_eq!(shared.with_active_demux_epoch(first, || {
            shared.playpos_ns.store(17, Ordering::Relaxed);
            17
        }), Some(17));
        assert!(shared.retire_native_session(first));
        let second = shared.begin_native_session().expect("successor native epoch");
        shared.playpos_ns.store(29, Ordering::Relaxed);
        assert_eq!(shared.with_active_demux_epoch(first, || {
            shared.playpos_ns.store(99, Ordering::Relaxed);
            99
        }), None);
        assert_eq!(shared.with_active_demux_epoch(first, || {
            shared.playpos_ns.load(Ordering::Relaxed)
        }), None);
        assert_eq!(shared.with_active_demux_epoch(second, || {
            shared.playpos_ns.load(Ordering::Relaxed)
        }), Some(29));
        assert!(shared.retire_native_session(second));
    }

    #[test]
    fn a_worker_consumes_a_complete_hidden_pause_cycle_as_one_event() {
        let transport = Transport::new();
        let mut cursor = UserPauseCursor::new(transport.pause_state_sample());
        transport.commit_paused(true);
        std::thread::sleep(Duration::from_millis(2));
        transport.commit_paused(false);

        let after = transport.pause_state_sample();
        assert!(
            !after.paused,
            "the surrounding boolean samples are both false"
        );
        assert_eq!(after.sequence, 2);
        assert!(
            cursor
                .consume_completed(after)
                .is_some_and(|elapsed| elapsed >= Duration::from_millis(1)),
            "the accepted interval must survive a blocked worker",
        );
        assert_eq!(
            cursor.consume_completed(after),
            None,
            "an event is consumed exactly once"
        );
    }

    #[test]
    fn user_hold_is_inside_the_automatic_transition_authority() {
        let shared = Shared::new();
        assert!(shared.hls_automatic_transition_permitted(HlsAutomaticTransition::Original));
        let HlsUserPause::Issue(token) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("running clock must issue the user Pause");
        };
        assert_eq!(
            shared.complete_hls_user_pause(token, true),
            HlsPauseCompletion::Accepted,
        );
        assert!(!shared.hls_automatic_transition_permitted(HlsAutomaticTransition::Original));
        assert!(!shared.hls_automatic_transition_permitted(HlsAutomaticTransition::QualityDown));
    }

    #[test]
    fn exactly_one_automatic_actuator_owns_a_boundary() {
        let shared = Shared::new();
        let original = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::Original)
            .expect("running playback grants the first automatic lease");
        assert!(
            shared
                .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
                .is_none(),
            "a second actuator cannot pass the same boundary while Original owns it",
        );
        assert!(shared.authorize_hls_automatic_transition_commit(original));
        assert!(
            !shared.authorize_hls_automatic_transition_commit(original),
            "commit authorization is one explicit edge, not a repeatable predicate",
        );
        assert!(shared.finish_hls_automatic_transition(original));
        let down = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("dropping the former owner releases the next explicit transition");
        assert!(shared.finish_hls_automatic_transition(down));
    }

    #[test]
    fn a_complete_pause_resume_cycle_invalidates_an_earlier_automatic_boundary() {
        let shared = Shared::new();
        let observed_user_sequence = shared.hls_user_sequence();
        let HlsUserPause::Issue(pause) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("running clock must issue Pause");
        };
        assert_eq!(
            shared.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        let HlsUserResume::Issue(play) = shared.prepare_hls_user_resume(false).unwrap() else {
            panic!("a held non-queued clock must issue Play");
        };
        assert!(matches!(
            shared.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { .. }
        ));

        assert!(
            shared
                .begin_hls_automatic_transition_at(
                    HlsAutomaticTransition::Original,
                    observed_user_sequence,
                )
                .is_none(),
            "the same Running/!held shape is a different boundary after Pause->Resume",
        );
    }

    #[test]
    fn a_retired_trial_cannot_release_the_next_sessions_lease() {
        let shared = Shared::new();
        let retired = shared
            .arm_hls_trial(1_000)
            .expect("first session grants its trial");
        shared.reset_hls_clock_for_test();
        let current = shared
            .arm_hls_trial(2_000)
            .expect("next session grants an independent trial");
        assert_ne!(retired, current);
        assert!(
            !shared.finish_hls_trial(retired),
            "the retired RAII drop has no authority in the new session",
        );
        assert_eq!(shared.hls_trial_reserve_ms.load(Ordering::Acquire), 2_000);
        assert!(shared.finish_hls_trial(current));
    }

    #[test]
    fn a_retired_actuator_cannot_publish_candidate_media_into_the_next_session() {
        let shared = Shared::new();
        let retired = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("first session grants its downshift");
        shared.reset_hls_clock_for_test();
        let current = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("next session grants its own downshift");
        assert_eq!(
            shared.begin_hls_candidate_transition(Some(retired)),
            Err(HlsClockFenceError::Superseded),
        );
        let candidate = shared
            .begin_hls_candidate_transition(Some(current))
            .expect("only the current actuator may enter publication");
        assert!(shared.settle_hls_candidate_transition(candidate));
        assert!(shared.finish_hls_automatic_transition(current));
    }

    #[test]
    fn internal_reprime_waits_for_the_recovery_actuator_transaction() {
        let shared = Shared::new();
        let down = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityDown)
            .expect("running clock grants the recovery actuator");
        let pause = shared
            .prepare_hls_rebuffer_pause()
            .expect("the internal clock can enter its recovery hold");
        assert_eq!(
            shared.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        shared.observe_hls_recovery(500_000, Duration::from_secs(2));
        let generation = shared.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            shared
                .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, shared.hls_recovery())
                .is_none(),
            "the old actuator may not restart while its replacement owns the boundary",
        );
        assert!(shared.finish_hls_automatic_transition(down));
        assert!(shared
            .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, shared.hls_recovery())
            .is_some());
    }

    #[test]
    fn a_user_pause_wins_over_an_inflight_original_completion() {
        let shared = Shared::new();
        let original = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::Original)
            .expect("running playback grants an Original lease");
        let HlsUserPause::Issue(pause) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("the orthogonal user transition must remain available");
        };
        assert_eq!(
            shared.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        assert!(
            !shared.authorize_hls_automatic_transition_commit(original),
            "a late network result is evidence only after the viewer owns the clock",
        );
        assert!(shared.finish_hls_automatic_transition(original));
    }

    #[test]
    fn a_user_clock_transition_revokes_inflight_quality_publication() {
        let shared = Shared::new();
        let up = shared
            .begin_hls_automatic_transition(HlsAutomaticTransition::QualityUp)
            .expect("running playback grants the upshift lease");
        let HlsUserPause::Issue(pause) = shared.prepare_hls_user_pause().unwrap() else {
            panic!("the user Pause remains orthogonal while candidate I/O runs");
        };
        assert_eq!(
            shared.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted,
        );
        assert_eq!(
            shared.begin_hls_candidate_transition(Some(up)),
            Err(HlsClockFenceError::Superseded),
            "private candidate bytes may not publish after the viewer changed the clock",
        );
        assert!(shared.finish_hls_automatic_transition(up));
    }

    #[test]
    fn raster_readers_never_observe_a_pair_no_stream_published() {
        let shared = Shared::new();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..20_000 {
                    shared.publish_video_raster(1_280, 720);
                    shared.publish_video_raster(1_920, 1_080);
                }
            });
            for _ in 0..20_000 {
                assert!(matches!(
                    shared.video_raster(),
                    (0, 0) | (1_280, 720) | (1_920, 1_080)
                ));
            }
        });
    }

    #[test]
    fn recovery_closes_the_actual_pause_epoch_not_a_permutation_of_history() {
        let mut epoch = HlsRecoveryEpoch::idle();
        epoch.begin();
        epoch.observe(2_500_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.debt_us, 500_000);
        assert!(
            !epoch.ready(20_000_000_000),
            "reserve alone cannot erase an open acquisition debt"
        );

        epoch.observe(1_500_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.debt_us, 0);
        assert_eq!(epoch.runway_us, 2_500_000);
        assert!(
            !epoch.ready(2_499_999_999),
            "one nanosecond below the measured prefix is not enough"
        );
        assert!(epoch.ready(2_500_000_000));
    }

    #[test]
    fn a_new_hold_carries_no_debt_from_an_old_slow_segment() {
        let mut epoch = HlsRecoveryEpoch::idle();
        epoch.begin();
        epoch.observe(9_000_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.debt_us, 7_000_000);

        epoch.begin();
        epoch.observe(500_000, std::time::Duration::from_secs(2));
        assert_eq!(epoch.completed, 1);
        assert_eq!(epoch.debt_us, -1_500_000);
        assert_eq!(epoch.runway_us, 500_000);
        assert!(epoch.ready(500_000_000));
    }

    #[test]
    fn initial_prime_is_an_explicit_hold_that_rejects_trials_and_fences_user_resume() {
        let s = Shared::new();
        assert!(s.arm_initial_clock_hold(false, false));
        assert!(
            s.arm_hls_trial(1_000).is_none(),
            "an up-trial cannot spend reserve before the first native Play"
        );
        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred)
        );

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
            .expect("the media-prime lane, not user Resume, owns the first Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
        assert!(
            s.arm_hls_trial(1_000).is_some(),
            "trials are admitted once the clock is Running"
        );
    }

    #[test]
    fn foreground_play_releases_both_the_pause_mirror_and_initial_user_hold() {
        let s = Shared::new();
        assert!(s.arm_initial_clock_hold(true, false));
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred),
        );
        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_some(),
            "the foreground Play event must clear the clock reducer's user hold, not only TX",
        );
    }

    #[test]
    fn a_user_pause_before_progressive_load_completed_fences_the_initial_play() {
        let s = Shared::new();
        assert!(s.arm_initial_clock_hold(false, false));
        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_none(),
            "the progressive load-complete fast path must not Play through viewer Pause",
        );

        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred)
        );
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_some(),
            "the same initial hold must become playable after the viewer resumes",
        );
    }

    #[test]
    fn a_stream_resume_does_not_issue_play_before_feed_reopens() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );

        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Prime),
            "a queued stream must reopen feeding and earn a balanced prime before native Play",
        );
        assert_eq!(s.hls_prime_kind(), Some(HlsPrimeKind::Resume));
    }

    #[test]
    fn a_second_pause_during_resume_prime_fences_play_until_the_second_resume() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert_eq!(s.prepare_hls_user_resume(true), Some(HlsUserResume::Prime));
        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Resume, generation, s.hls_recovery())
                .is_none(),
            "the second viewer Pause must fence a ready ResumePrime",
        );
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred),
            "the existing ResumePrime certificate survives the second viewer Resume",
        );
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Resume, generation, s.hls_recovery())
            .expect("the second Resume releases the same prime once its media is ready");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
    }

    #[test]
    fn a_local_sample_without_stream_queues_keeps_immediate_resume() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert!(matches!(
            s.prepare_hls_user_resume(false),
            Some(HlsUserResume::Issue(_))
        ));
    }

    #[test]
    fn a_seek_transfers_resume_prime_to_the_fresh_timeline_certificate() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert_eq!(s.prepare_hls_user_resume(true), Some(HlsUserResume::Prime));
        assert_eq!(s.prepare_seek_pause(), Some(HlsSeekPause::AlreadyHeld));
        assert_eq!(s.hls_prime_kind(), Some(HlsPrimeKind::Fresh));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Resume, generation, s.hls_recovery())
                .is_none(),
            "a stale ResumePrime snapshot cannot Play through a newer seek",
        );
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
                .is_some(),
            "the seek's fresh-timeline certificate now owns Play",
        );
    }

    #[test]
    fn a_paused_non_seek_reload_stays_held_but_a_seek_preroll_may_decode_its_still() {
        let ordinary_reload = Shared::new();
        assert!(ordinary_reload.arm_initial_clock_hold(true, false));
        let ordinary_generation = ordinary_reload
            .hls_candidate_generation
            .load(Ordering::Acquire);
        assert!(
            ordinary_reload
                .reserve_hls_prime_play(
                    HlsPrimeKind::Fresh,
                    ordinary_generation,
                    ordinary_reload.hls_recovery(),
                )
                .is_none(),
            "preserving Pause across a non-seek reload must not authorize native Play",
        );

        let seek_reload = Shared::new();
        assert!(seek_reload.arm_initial_clock_hold(true, true));
        let seek_generation = seek_reload.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = seek_reload
            .reserve_hls_prime_play(
                HlsPrimeKind::Fresh,
                seek_generation,
                seek_reload.hls_recovery(),
            )
            .expect("the explicit one-frame seek preroll owns this Play");
        assert_eq!(
            seek_reload.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true },
            "the preroll Play must carry the deferred ACB resume",
        );
    }

    #[test]
    fn trial_and_rebuffer_pause_linearize_under_one_mutex() {
        let trial_first = Shared::new();
        let trial = trial_first
            .arm_hls_trial(1_000)
            .expect("running clock grants the trial");
        assert!(
            trial_first.prepare_hls_rebuffer_pause().is_none(),
            "a reserved trial makes Pause retry"
        );
        assert!(trial_first.finish_hls_trial(trial));
        assert!(trial_first.prepare_hls_rebuffer_pause().is_some());

        let pause_first = Shared::new();
        let _pause = pause_first
            .prepare_hls_rebuffer_pause()
            .expect("Pause wins the reservation");
        assert!(
            pause_first.arm_hls_trial(1_000).is_none(),
            "a reserved Pause makes the up-trial reject"
        );
    }

    #[test]
    fn candidate_settlement_rebases_recovery_while_pause_is_still_issuing() {
        let s = Shared::new();
        let pause = s.prepare_hls_rebuffer_pause().expect("reserve Pause");
        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        assert_eq!(s.hls_recovery().completed, 1);

        let candidate = s
            .begin_hls_candidate_transition(None)
            .expect("arm down candidate");
        assert!(s.settle_hls_candidate_transition(candidate));
        assert_eq!(
            s.hls_recovery().completed,
            0,
            "new-rung media needs a fresh recovery certificate even before Pause returns"
        );
        assert_eq!(
            s.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
    }

    #[test]
    fn a_paused_seek_reuses_the_physical_hold_and_resumes_acb_only_at_prime() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        assert_eq!(s.prepare_seek_pause(), Some(HlsSeekPause::AlreadyHeld));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, s.hls_recovery())
            .expect("seek prime owns the reused hold");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
    }

    /// The segment completion callback can win the tiny race after Starfish has accepted Pause but
    /// before the main thread has returned from the FFI call. Recovery must therefore be armed by
    /// the reservation, not by the completion half of that call.
    #[test]
    fn a_segment_completed_while_pause_is_issuing_belongs_to_that_hold() {
        let s = Shared::new();
        let token = s
            .prepare_hls_rebuffer_pause()
            .expect("the stable clock reserves Pause");

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        assert_eq!(
            s.complete_hls_rebuffer_pause(token, true),
            HlsPauseCompletion::Accepted
        );

        let recovery = s.hls_recovery();
        assert_eq!(recovery.completed, 1);
        assert!(recovery.ready(500_000_000));
    }

    #[test]
    fn a_segment_seen_during_a_refused_pause_is_discarded() {
        let s = Shared::new();
        let token = s
            .prepare_hls_rebuffer_pause()
            .expect("the stable clock reserves Pause");

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        assert_eq!(
            s.complete_hls_rebuffer_pause(token, false),
            HlsPauseCompletion::Refused
        );

        assert_eq!(s.hls_recovery(), HlsRecoveryEpoch::idle());
        assert!(!s.hls_rebuffering.load(Ordering::Acquire));
    }

    /// A bool snapshot loses a complete Pause -> Play cycle inside a blocked network read. The
    /// monotone epoch is the durable observation used by the deadline transaction instead.
    #[test]
    fn an_accepted_internal_hold_remains_observable_after_play() {
        let s = Shared::new();
        let before = s.hls_internal_hold_epoch.load(Ordering::Acquire);
        let pause = s.prepare_hls_rebuffer_pause().expect("reserve Pause");
        assert_eq!(
            s.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        let held = s.hls_internal_hold_epoch.load(Ordering::Acquire);
        assert_ne!(held, before);

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let recovery = s.hls_recovery();
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, recovery)
            .expect("the ready held clock reserves Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: false }
        );

        assert!(!s.hls_rebuffering.load(Ordering::Acquire));
        assert_eq!(s.hls_internal_hold_epoch.load(Ordering::Acquire), held);
    }

    #[test]
    fn user_pause_and_internal_rebuffer_are_orthogonal_holds() {
        let s = Shared::new();
        let pause = s
            .prepare_hls_rebuffer_pause()
            .expect("reserve internal Pause");
        assert_eq!(
            s.complete_hls_rebuffer_pause(pause, true),
            HlsPauseCompletion::Accepted
        );

        assert_eq!(s.prepare_hls_user_pause(), Some(HlsUserPause::AlreadyHeld));
        assert_eq!(
            s.prepare_hls_user_resume(true),
            Some(HlsUserResume::Deferred)
        );

        s.observe_hls_recovery(500_000, Duration::from_secs(2));
        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let recovery = s.hls_recovery();
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Rebuffer, generation, recovery)
            .expect("the internal certificate owns eventual Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true },
            "the eventual internal Play must also finish the viewer's deferred ACB Resume",
        );
    }

    #[test]
    fn candidate_publication_defers_user_resume_until_it_settles() {
        let s = Shared::new();
        let pause = match s.prepare_hls_user_pause() {
            Some(HlsUserPause::Issue(token)) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            s.complete_hls_user_pause(pause, true),
            HlsPauseCompletion::Accepted
        );
        let candidate = s
            .begin_hls_candidate_transition(None)
            .expect("arm candidate");

        assert_eq!(s.prepare_hls_user_resume(true), Some(HlsUserResume::Prime));
        let recovery = s.hls_recovery();
        assert!(
            s.reserve_hls_prime_play(HlsPrimeKind::Resume, candidate, recovery)
                .is_none(),
            "an active candidate generation must fence ResumePrime",
        );
        assert!(s.settle_hls_candidate_transition(candidate));

        let generation = s.hls_candidate_generation.load(Ordering::Acquire);
        let (play, _) = s
            .reserve_hls_prime_play(HlsPrimeKind::Resume, generation, recovery)
            .expect("settlement releases the pending user Play");
        assert_eq!(
            s.complete_hls_prime_play(play, true),
            HlsPlayCompletion::Accepted { resume_acb: true }
        );
    }

    /// The one invariant that cannot be recovered from if it breaks, and breaks SILENTLY: a session
    /// boundary must forget this session's picture. `seen_frame` is set once, off-thread, and is
    /// never cleared anywhere else — so a `reset_session` that dropped it would leave it true for
    /// the rest of the process's life, and every subsequent cold start would show only the 12px
    /// transport mark instead of the centred read-out. Graded together with `frames`, because the
    /// two are only meaningful as a pair (see `Shared::seen_frame`).
    #[test]
    fn reset_session_forgets_this_sessions_picture() {
        let s = Shared::new();
        assert!(
            !s.seen_frame.load(Ordering::Relaxed),
            "a fresh session has shown nothing"
        );
        s.seen_frame.store(true, Ordering::Relaxed);
        s.frames.store(9, Ordering::Relaxed);
        s.demux_io_failed.store(true, Ordering::Relaxed);
        s.dg_transport_rc.store(56, Ordering::Relaxed);
        s.dg_transport_age_ms.store(60_000, Ordering::Relaxed);
        s.dg_transport_reopen_failed.store(true, Ordering::Relaxed);
        s.dg_abr_mode.store(2, Ordering::Relaxed);
        s.dg_abr_kbps.store(2_000, Ordering::Relaxed);
        s.dg_abr_declared_kbps.store(1_459, Ordering::Relaxed);
        s.reset_session();
        assert!(
            !s.seen_frame.load(Ordering::Relaxed),
            "a reload/stop blanks the plane — say so"
        );
        assert_eq!(
            s.frames.load(Ordering::Relaxed),
            0,
            "and the two must be reset together"
        );
        assert!(
            !s.demux_io_failed.load(Ordering::Relaxed),
            "a new session must not inherit an I/O failure"
        );
        assert_eq!(
            (
                s.dg_transport_rc.load(Ordering::Relaxed),
                s.dg_transport_age_ms.load(Ordering::Relaxed),
                s.dg_transport_reopen_failed.load(Ordering::Relaxed),
            ),
            (0, -1, false),
            "nor the cause of the last session's transfer failure"
        );
        assert_eq!(s.dg_abr_mode.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_kbps.load(Ordering::Relaxed), 0);
        assert_eq!(s.dg_abr_declared_kbps.load(Ordering::Relaxed), -1);
    }
}

#[cfg(test)]
mod sink_time_tests {
    use super::*;
    use std::sync::atomic::Ordering::Relaxed;

    /// The timeline conversion: at 1.5x a fed timestamp is two thirds of the film time it stands
    /// for, the presented one comes back as film time, and at 1x nothing changes.
    #[test]
    fn the_pipeline_timeline_is_film_time_divided_by_the_speed() {
        let _serial = plx_base::testlock::serial();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED.applied_rate_permille.store(RATE_NORMAL, Relaxed);
            }
        }
        let _restore = Restore;
        FEED_SHARED.applied_rate_permille.store(RATE_NORMAL, Relaxed);
        assert_eq!(to_sink_ns(7_000_000_000), 7_000_000_000);
        FEED_SHARED.applied_rate_permille.store(1500, Relaxed);
        assert_eq!(to_sink_ns(3_000_000_000), 2_000_000_000);
        assert_eq!(from_sink_ns(2_000_000_000), 3_000_000_000);
        FEED_SHARED.applied_rate_permille.store(500, Relaxed);
        assert_eq!(to_sink_ns(3_000_000_000), 6_000_000_000, "half speed spreads frames out");
        // a long film at a fractional speed survives the round trip within a nanosecond
        FEED_SHARED.applied_rate_permille.store(1750, Relaxed);
        let t = 9_876_543_210_123;
        assert!((from_sink_ns(to_sink_ns(t)) - t).abs() <= 2);
    }
}
