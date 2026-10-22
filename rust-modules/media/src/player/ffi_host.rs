//! The `hostsim` stand-in for `src/starfish.c` — the television's media seam, absent.
//!
//! This file has two behaviours and the default is still the honest failure.
//!
//! # Default: nothing decodes, and it says so immediately
//!
//! The simulator has no StarfishMediaAPIs, no `libAcbAPI`, and no hardware video plane. Every verb
//! reports the same failure the seam itself reports when a television has no usable video path
//! (`vp_mode() == VP_NONE`, `sf_load` returning 0), which is a state the engine, the pump and the
//! HUD already handle — a real firmware can be in it, and issue #22 was exactly that. So pressing
//! Play in the simulator lands on the app's genuine full-screen failure read-out rather than on a
//! hang or a panic.
//!
//! **The alternative was worse.** Stubbing these as no-ops that return SUCCESS would have the pump
//! wait forever for frames that never arrive, and a simulator that silently hangs on Play teaches
//! an agent that playback is broken when it is merely absent. Failing honestly and immediately is
//! the whole contract of that path.
//!
//! # Opt-in: a REFUSED Load (`plxnative-refuseload[=N]`)
//!
//! With the sink armed, refuse the first N Loads (bare flag: every Load) the way webOS 10.3.1
//! refused the 4K60 H.264 envelope — asynchronously, by callback, after `Load()` returned ok=1.
//! See `sf_load`. It exists so the failure read-out that refusal reaches can be driven and
//! screenshotted on a Mac, and so a rollback that reloads can be made to fail a second time.
//!
//! # Opt-in: the CLOCK SINK (`plxnative-clocksink`)
//!
//! **It accepts access units, throws them away, and advances a presentation clock at real time.**
//! Nothing decodes and nothing is displayed. What it makes runnable on a Mac is everything
//! upstream of the decoder: the PMS decision, HLS parsing, the AVIO transport, `ff.rs`'s demux, the
//! AU queues and their byte-cap backpressure, the feed-ahead throttle, the ABR controller's
//! transactions, seek and PTS rebase.
//!
//! **Why that is worth having, stated as what it can and cannot decide.** The reachable reserve is
//! `B_max = lead + queue_bytes / rate`, and every term in it is OURS — `AQ_VIDEO_BYTES`,
//! `AQ_AUDIO_BYTES`, `MAX_FEED_AHEAD_NS`, `AUDIO_SLACK_NS` — not Starfish's. `abr/sim.rs` models
//! that analytically and has never once been checked against a running pipeline; this is the only
//! way to check it without a television. It can also answer whether the queues drain as modelled,
//! whether a rung transaction costs what `abr: tx` says, and whether backpressure behaves.
//!
//! It cannot answer anything about LG's decoder: resource-allocation refusals, the ACB video-plane
//! bind, the Load payload's Dolby declaration, `SOUND_ERROR_019`, frame pacing, or which codecs
//! the panel takes. A Mac decodes things that television will not and the reverse, so "it played
//! here" transfers nothing. **Every heartbeat already carries `sim=1`; nothing measured through
//! this sink is a device measurement, and a number taken here must never be quoted as one.**
//!
//! **Why a clock and not AVFoundation.** `AVSampleBufferDisplayLayer` is a genuinely close
//! analogue of Starfish's buffer-feed mode and would decode for real — but the questions it would
//! newly answer are about Apple's decoder, and the ones that break this app are about LG's. The
//! pixels are not the uncertainty. This is ~1% of the work for the part that is.
//!
//! **It also ends a film the way the pipeline does.** After `PushEOS`, once the clock has played
//! out to the last video frame it was fed, the sink sends `type=28 num=1 str=true` through the real
//! callback ([`Clock::tick`]), so a film whose audio outlasts its video ends here as on the
//! television instead of hanging at the video tail. Cleared wherever the fed timeline restarts.
//!
//! It reports [`VP_EXPORTED`](super::VP_EXPORTED), the webOS 5+ path, because that one binds video
//! through an exported window id and never touches ACB — so the whole ACB stage sequence stays
//! skipped rather than faked.
//!
//! # How it is reached
//!
//! The verbs below are private; [`HostSink`] is the simulator's own `tv::sink::VideoSink`, one
//! method per verb, installed by `port.rs` for the `hostsim` build and returned directly by
//! `player::sink()` in the hostsim tests. The `*_for_test` hooks stay here, next to the statics
//! they touch, and the tests reach them as `crate::player::ffi_host::<hook>`.

use plx_base::task::MainThread;
use plx_platform::tv::sink::VideoSink;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_long, c_uint};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::Relaxed};
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicI32, AtomicU64};
#[cfg(any(test, feature = "test-support"))]
use std::sync::{Condvar, Mutex};

/// `sf_feed`'s rejection code. `starfish.h` documents the three replies as `'O'` (ok), `'B'`
/// (BufferFull) and `'e'` (error); the pump treats `'B'` as backpressure and retries forever, so
/// returning it here would spin. `'e'` is the one that terminates.
const FEED_ERROR: c_char = b'e' as c_char;
/// `sf_feed`'s acceptance code.
const FEED_OK: c_char = b'O' as c_char;

/// How often the sink reports a position, milliseconds.
///
/// **Measured from the television, not chosen.** LG's pipeline emits its position callback at
/// **5 Hz** — `vtick=5 vgap=201ms`, unvarying across every Profile 5 run — and the app's
/// feed-ahead throttle is built against exactly that granularity (200 ms against a 1.6 s budget).
/// Ticking faster here would give the host a smoother position than any television produces and
/// would hide a throttle bug that a device would show.
const TICK_MS: u64 = 200;

/// **Test-only arming, deliberately NOT the trigger file.** `enabled()` latches in a `OnceLock`
/// and the runtime root it consults latches in another, both process-wide — so a unit test that
/// armed the sink by writing `plxnative-clocksink` would pass alone and fail in a full run,
/// depending on which test touched a root first. It would also have to write into the same
/// directory a real simulator reads. This overrides the answer directly instead, so the test is
/// deterministic and leaves no file behind.
#[cfg(any(test, feature = "test-support"))]
pub(super) static FORCE_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Force the next host `Play` calls to report a seam result. `i32::MIN` means normal clocksink
/// behaviour. This makes the state-machine invariant testable without teaching production code a
/// failure-injection branch.
#[cfg(any(test, feature = "test-support"))]
pub(super) static FORCE_PLAY_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);
#[cfg(any(test, feature = "test-support"))]
pub(super) static PLAY_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(any(test, feature = "test-support"))]
pub(super) static FORCE_PAUSE_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);
/// The host pipeline's volume (`setVolume`); a fresh pipeline starts at `PIPELINE_DEFAULT_VOLUME`.
#[cfg(any(test, feature = "test-support"))]
pub(super) static HOST_VOLUME: AtomicI32 = AtomicI32::new(plx_platform::tv::sink::PIPELINE_DEFAULT_VOLUME);
/// Every Play / Pause / `set_volume(n)` the sink saw, in order, as `"play"`, `"pause"`,
/// `"volume:n"`: what a test reads to prove the paused preroll mutes BEFORE its Play and restores
/// AFTER its re-pause. Cleared by [`reset_volume_probe_for_test`].
#[cfg(any(test, feature = "test-support"))]
pub(super) static VERB_ORDER_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
#[cfg(any(test, feature = "test-support"))]
fn note_verb(verb: String) {
    VERB_ORDER_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(verb);
}

/// ACB seam probes (host has no libAcbAPI): `setMediaVideoData` answers -1 (rejected) unless a
/// test arms [`ACB_VIDEO_DATA_ACCEPT`], and every `acb_start`/`acb_pause`/`acb_resume` is counted,
/// so a test can read the order and number of plane verbs the pump issued.
#[cfg(test)]
pub(super) static ACB_VIDEO_DATA_ACCEPT: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(super) static ACB_VIDEO_DATA_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ACB_START_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ACB_PAUSE_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static ACB_RESUME_CALLS: AtomicU64 = AtomicU64::new(0);
/// `(setMediaVideoData, acb_start, acb_pause, acb_resume)` call counts. Test-only.
#[cfg(test)]
pub(super) fn acb_calls_for_test() -> (u64, u64, u64, u64) {
    (
        ACB_VIDEO_DATA_CALLS.load(Relaxed),
        ACB_START_CALLS.load(Relaxed),
        ACB_PAUSE_CALLS.load(Relaxed),
        ACB_RESUME_CALLS.load(Relaxed),
    )
}
/// Zero the ACB counters and choose whether `setMediaVideoData` is accepted. Test-only.
#[cfg(test)]
pub(super) fn reset_acb_for_test(accept_video_data: bool) {
    ACB_VIDEO_DATA_ACCEPT.store(accept_video_data, Relaxed);
    for c in [&ACB_VIDEO_DATA_CALLS, &ACB_START_CALLS, &ACB_PAUSE_CALLS, &ACB_RESUME_CALLS] {
        c.store(0, Relaxed);
    }
}

/// Every millis `sf_seek_ms` was called with, in order (#568): what a test reads to prove a
/// direct-play seek went through LG's public `Seek` and landed where the keyframe did.
#[cfg(any(test, feature = "test-support"))]
pub(super) static SEEK_LOG: Mutex<Vec<i64>> = Mutex::new(Vec::new());
/// Forced `sf_seek_ms` answer; `i32::MIN` means "behave". `0` is LG refusing the seek.
#[cfg(any(test, feature = "test-support"))]
pub(super) static FORCE_SEEK_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);
/// How many times the sink was flushed: a native seek must not flush (Seek flushes itself, and a
/// flush after it re-anchors the segment at 0).
#[cfg(any(test, feature = "test-support"))]
pub(super) static FLUSH_CALLS: AtomicU64 = AtomicU64::new(0);
/// How many times the Segment path's private-offset verbs (`setTimeToDecode`, `setContentInfo`,
/// `sendSegmentEvent`) were called: on `VP_EXPORTED` they must never be, whatever the seek's fate.
#[cfg(any(test, feature = "test-support"))]
pub(super) static SEGMENT_CALLS: AtomicU64 = AtomicU64::new(0);

/// issue #74 D.1 test seam — real (v0.6.0) `sf_load` publishes `SMP_READY(1)` and then calls the
/// blocking, synchronous Load, so there is a real window during which every OTHER seam verb is
/// dispatchable but must not be dispatched. The unmodified host `sf_load` above has no such
/// window (it emits `loadCompleted` and returns `1` in the same call), so these controls carve
/// one open for the regression test: `LOAD_IN_FLIGHT` is true from the moment `sf_load` emits
/// `loadCompleted` until it actually returns, `HOLD_LOAD` is what makes it block there on demand,
/// and `IN_FLIGHT_CALLS`/`IN_FLIGHT_VERB` are what let a test PROVE nothing reached the seam
/// during that window (this project's silent-instrument rule — see `note_dispatch`).
#[cfg(any(test, feature = "test-support"))]
pub(super) static LOAD_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
/// How many seam verbs were dispatched while [`LOAD_IN_FLIGHT`] was true. Zero across a whole
/// held Load is the primary assertion `nothing_reaches_the_seam_while_load_is_still_in_flight`
/// makes.
#[cfg(any(test, feature = "test-support"))]
pub(super) static IN_FLIGHT_CALLS: AtomicU32 = AtomicU32::new(0);
/// The FIRST verb name recorded while [`LOAD_IN_FLIGHT`] was true — for the failure message, so a
/// red run says exactly what reached the seam rather than only that something did.
#[cfg(any(test, feature = "test-support"))]
pub(super) static IN_FLIGHT_VERB: Mutex<Option<&'static str>> = Mutex::new(None);
/// Bumped at the top of every host verb the real seam routes through `sf_ready_object()`. Records
/// only while `LOAD_IN_FLIGHT` is true — a call before the window opens or after it closes is
/// exactly what the fix is supposed to allow.
#[cfg(any(test, feature = "test-support"))]
fn note_dispatch(verb: &'static str) {
    if LOAD_IN_FLIGHT.load(Relaxed) {
        IN_FLIGHT_CALLS.fetch_add(1, Relaxed);
        let mut slot = IN_FLIGHT_VERB.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(verb);
        }
    }
}
/// Makes the host `sf_load` block (after emitting `loadCompleted`, so `LOAD_IN_FLIGHT` is already
/// true) until [`release_load_for_test`] is called — the only way to get a REAL two-thread window
/// open on the host, rather than a single-threaded simulation of one.
#[cfg(any(test, feature = "test-support"))]
pub(super) static HOLD_LOAD: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());
/// Arm the hold BEFORE spawning `threads::load_thread`, so its `sf_load` call blocks once it
/// reaches the wait above. A test that never calls this gets the unmodified, non-blocking
/// `sf_load` — this is opt-in per test, not a global change in behaviour.
#[cfg(test)]
pub(super) fn hold_load_for_test() {
    let mut held = HOLD_LOAD.0.lock().unwrap_or_else(|e| e.into_inner());
    *held = true;
}
#[cfg(test)]
pub(super) fn release_load_for_test() {
    let mut held = HOLD_LOAD.0.lock().unwrap_or_else(|e| e.into_inner());
    *held = false;
    HOLD_LOAD.1.notify_all();
}
/// How long the host `sf_unload` takes, in ms — the device's `StarfishMediaAPIs::Unload` blocks
/// for 110-150 ms (`engine::PendingRelease`), and a test of the frame that pays for it needs a
/// sink that takes measurable time too. 0 (the default) is the unmodified, instant Unload.
#[cfg(test)]
static UNLOAD_DELAY_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(test)]
pub(super) fn delay_unload_for_test(ms: u64) {
    UNLOAD_DELAY_MS.store(ms, Relaxed);
}
#[cfg(test)]
pub(super) fn in_flight_calls_for_test() -> u32 {
    IN_FLIGHT_CALLS.load(Relaxed)
}
#[cfg(test)]
pub(super) fn in_flight_verb_for_test() -> Option<&'static str> {
    *IN_FLIGHT_VERB.lock().unwrap_or_else(|e| e.into_inner())
}
/// Force [`LOAD_IN_FLIGHT`] directly, for a test that simulates the window without a real
/// second thread (`the_gate_is_epoch_scoped`) — see that test for why it still needs this even
/// though it never calls `sf_load`.
#[cfg(test)]
pub(super) fn set_load_in_flight_for_test(on: bool) {
    LOAD_IN_FLIGHT.store(on, Relaxed);
}
/// Whether the host `sf_load` is currently inside its in-flight window (`loadCompleted` emitted,
/// not yet returned).
#[cfg(test)]
pub(super) fn load_in_flight_for_test() -> bool {
    LOAD_IN_FLIGHT.load(Relaxed)
}
/// **Test-only: force the host clock sink on.** The override reaches tests through a hook here
/// rather than by widening the visibility of [`FORCE_ENABLED`].
#[cfg(test)]
pub(super) fn force_clocksink_for_test(on: bool) {
    FORCE_ENABLED.store(on, Relaxed);
}
/// Forget the seek probes (#568) and optionally force LG's `Seek` answer (`Some(0)` = refuse).
#[cfg(test)]
pub(super) fn reset_seek_probe_for_test(force: Option<c_int>) {
    SEEK_LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
    FORCE_SEEK_RESULT.store(force.unwrap_or(i32::MIN), Relaxed);
    FLUSH_CALLS.store(0, Relaxed);
    SEGMENT_CALLS.store(0, Relaxed);
}
/// The millis every `Seek` was called with since [`reset_seek_probe_for_test`].
#[cfg(test)]
pub(super) fn seek_log_for_test() -> Vec<i64> {
    SEEK_LOG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
/// How many private-offset Segment verbs ran since [`reset_seek_probe_for_test`].
#[cfg(test)]
pub(super) fn segment_calls_for_test() -> u64 {
    SEGMENT_CALLS.load(Relaxed)
}
/// How many times the sink was flushed since [`reset_seek_probe_for_test`].
#[cfg(test)]
pub(super) fn flush_calls_for_test() -> u64 {
    FLUSH_CALLS.load(Relaxed)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn force_play_result_for_test(result: Option<c_int>) {
    FORCE_PLAY_RESULT.store(result.unwrap_or(i32::MIN), Relaxed);
}
/// Forget the verb order and put the host pipeline back at its default volume of 100.
#[cfg(test)]
pub(super) fn reset_volume_probe_for_test() {
    VERB_ORDER_LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
    HOST_VOLUME.store(100, Relaxed);
}
/// The Play / Pause / `volume:n` verbs since [`reset_volume_probe_for_test`], in order.
#[cfg(test)]
pub(super) fn verb_order_for_test() -> Vec<String> {
    VERB_ORDER_LOG.lock().unwrap_or_else(|e| e.into_inner()).clone()
}
/// The host pipeline's current volume.
#[cfg(test)]
pub(super) fn host_volume_for_test() -> i32 {
    HOST_VOLUME.load(Relaxed)
}
#[cfg(test)]
pub(super) fn play_calls_for_test() -> u64 {
    PLAY_CALLS.load(Relaxed)
}
#[cfg(any(test, feature = "test-support"))]
pub(super) fn force_pause_result_for_test(result: Option<c_int>) {
    FORCE_PAUSE_RESULT.store(result.unwrap_or(i32::MIN), Relaxed);
}

/// Is the clock sink armed? Read once, at the first seam call, and latched.
///
/// A trigger rather than a build feature so one binary does both: `make sim` keeps landing on the
/// failure read-out (which is what the UI work wants to see) unless a session explicitly asks for
/// a pipeline run.
fn enabled() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    if FORCE_ENABLED.load(Relaxed) {
        return true;
    }
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        let on = plx_base::devtrig::flag("clocksink");
        if on {
            plx_base::eventlog::log(
                "clocksink: ARMED — AUs are accepted and discarded, and the presentation clock \
                 advances at real time. NOTHING IS DECODED and no number from this run is a \
                 device measurement.",
            );
        }
        on
    })
}

fn now_ms() -> i64 {
    #[cfg(test)]
    if let Some(pinned) = pinned_clock::read() {
        return pinned;
    }
    i64::from(super::vclock_ms())
}

/// A per-thread virtual reading for [`now_ms`], so a test that grades the clock's POSITION is not
/// graded on how many real milliseconds its own statements took. The sink clock adds the wall time
/// since it last started running, in whole milliseconds, so a test that starts it at an exact
/// offset and pauses it one statement later reads that offset only when no millisecond boundary
/// falls between the two: a loaded runner makes one fall there often enough to be a flake
/// (`claim_hold`'s "stopped AT the claim offset" read 1 ms high). Pinned, time moves only when
/// [`pinned_clock::Pinned::advance`] says so; dropping the guard restores the real clock. The
/// pin starts at the real reading, so it shares the ticker thread's time base.
#[cfg(test)]
pub(super) mod pinned_clock {
    use std::cell::Cell;

    thread_local! {
        static PINNED: Cell<Option<i64>> = const { Cell::new(None) };
    }

    pub(super) fn read() -> Option<i64> {
        PINNED.with(Cell::get)
    }

    pub(crate) struct Pinned;

    impl Pinned {
        /// Pin this thread's sink clock at the real time now.
        pub(crate) fn now() -> Self {
            PINNED.with(|p| p.set(Some(i64::from(super::super::vclock_ms()))));
            Pinned
        }

        pub(crate) fn advance_ms(&self, by: i64) {
            PINNED.with(|p| p.set(p.get().map(|at| at + by)));
        }
    }

    impl Drop for Pinned {
        fn drop(&mut self) {
            PINNED.with(|p| p.set(None));
        }
    }
}
/// The rebase `sf_on_event` applies to a reported position (`playpos = fed - pts_shift +
/// disp_base`), inverted: the fed PTS at which the app will read `media_ns`.
fn media_to_fed(media_ns: i64) -> i64 {
    let shared = &super::SHARED;
    media_ns
        .saturating_add(shared.pts_shift.load(Relaxed))
        .saturating_sub(shared.disp_base.load(Relaxed))
}

/// Stop the running clock EXACTLY at a movie position, or lift the stop (`None`). Returns whether
/// a stop was armed: one the playhead has already passed is refused, because honouring it would
/// run the clock backwards.
///
/// **Why it exists: the documentation's player figure** (`autopause=at=<ms>`). A pause issued when
/// the playhead has reached a position is accepted some tens of milliseconds later, and how many
/// moves with host scheduling, across a 24 fps frame boundary as often as not: a different frame,
/// a different knob position, every run. Stopped here, the clock waits on the one position the
/// pause is aimed at, the pause then freezes it there (`hold` reads the clamped position), and
/// the picture `sim_video` shows, the clock readout and the playhead are identical from run to
/// run. A stall of the sink's own making, like the fed-ceiling clamp above: the app sees a
/// pipeline that has not presented past that point yet, which is a shape it already handles.
pub fn stop_clock_at(media_ns: Option<i64>) -> bool {
    let Some(media_ns) = media_ns else {
        STOP_AT_MEDIA_NS.store(i64::MAX, Relaxed);
        return false;
    };
    if Clock::position_ns() > media_to_fed(media_ns) {
        return false;
    }
    STOP_AT_MEDIA_NS.store(media_ns, Relaxed);
    true
}

/// Deliver one pipeline event through the real callback, as LG's library thread would: into the
/// current `Load`'s epoch, and only while that `Load`'s callback gate is open.
fn report_event(ty: c_int, num: i64, text: *const c_char) {
    let epoch = ACTIVE_EPOCH.load(Relaxed);
    if epoch != 0 && !CALLBACK_GATE_RETIRED.load(Relaxed) {
        CALLBACK_INTERCEPTS.fetch_add(1, Relaxed);
        super::sf_on_event(epoch, ty, num, text);
    }
}

fn report_position(pts_ns: i64) {
    report_event(0, pts_ns, std::ptr::null());
}

/// How many times the sink decided to report the pipeline's end-of-stream. Test-only: it counts
/// the decision, so a unit test can grade it without an open callback gate.
#[cfg(test)]
static EOS_REPORTS: AtomicU64 = AtomicU64::new(0);

/// The pipeline's end-of-stream: `type=28 num=1 str=true` (`PIPELINE_END_OF_STREAM`).
fn report_end_of_stream() {
    #[cfg(test)]
    EOS_REPORTS.fetch_add(1, Relaxed);
    report_event(28, 1, c"true".as_ptr());
}

/// Everything the sink remembers. One session's worth; `sf_unload` clears it.
struct Clock;

static LOADED: AtomicBool = AtomicBool::new(false);
#[cfg(any(test, feature = "test-support"))]
static FORCE_DESTROY_REFUSAL: AtomicBool = AtomicBool::new(false);
/// A constructed Starfish object survives `Unload` until the separately gated destructor.
static OBJECT_READY: AtomicBool = AtomicBool::new(false);
/// Callback context installed by the current host `Load`, mirroring firmware's `void *ctx`.
static ACTIVE_EPOCH: AtomicU32 = AtomicU32::new(0);
static CALLBACK_INTERCEPTS: AtomicU32 = AtomicU32::new(0);
static CALLBACK_GATE_RETIRED: AtomicBool = AtomicBool::new(false);
static NATIVE_UNLOAD_COMPLETED: AtomicBool = AtomicBool::new(false);
static LIFECYCLE_BLOCKED: AtomicBool = AtomicBool::new(false);
static PLAYING: AtomicBool = AtomicBool::new(false);
static TICKING: AtomicBool = AtomicBool::new(false);
/// Presentation position at the moment the clock last started running, in FED-PTS space.
static BASE_NS: AtomicI64 = AtomicI64::new(0);
/// `SDL_GetTicks`-equivalent wall clock at that same moment.
static RESUMED_AT_MS: AtomicI64 = AtomicI64::new(0);
/// The highest video PTS ever handed to [`sf_feed`]. The clock may not run past it.
static FED_MAX_NS: AtomicI64 = AtomicI64::new(i64::MIN);
/// A MOVIE position the clock may not run past either (`i64::MAX` = none): [`stop_clock_at`].
static STOP_AT_MEDIA_NS: AtomicI64 = AtomicI64::new(i64::MAX);
/// **`PushEOS` has been called and the clock has not yet played out to the end of what was fed.**
/// The pipeline reports its end-of-stream only after it has presented everything it holds, so the
/// sink latches the push and [`Clock::tick`] sends `type=28` once the position reaches
/// [`FED_MAX_NS`]. One per pushed EOS; cleared wherever the fed timeline restarts
/// ([`Clock::rewind`], [`sf_seek_ms`]).
static EOS_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
pub(super) fn force_callback_intercepts_for_test(value: u32) {
    CALLBACK_INTERCEPTS.store(value, Relaxed);
}
/// Force `OBJECT_READY` directly, without a real `sf_load` call. `sf_load`'s host stub
/// unconditionally emits the `loadCompleted` callback before it can ever return (see its own
/// doc), so it cannot represent "Load constructed the pipeline but `loadCompleted` never
/// arrives" — the exact state
/// `native_load_returned_loadcompleted_never_arrives_fires_load_failed` needs. This flips only
/// the half `sf_ready()` reads, leaving `LOADED` (and so `sf_is_load_completed()`) at its
/// default `false`.
/// Whether the host object is quarantined (`LIFECYCLE_BLOCKED`) — separates a D1 release from a
/// quarantine, which both leave `sf_ready()` at 0.
#[cfg(test)]
pub(super) fn lifecycle_blocked_for_test() -> bool {
    LIFECYCLE_BLOCKED.load(Relaxed)
}
/// Make the seam's own D1 evidence disagree with Rust's: `sf_destroy` answers 0 and latches the
/// quarantine even though every proof Rust holds says Destroy. Cleared by
/// [`reset_native_lifecycle_for_test`].
#[cfg(test)]
pub(super) fn force_destroy_refusal_for_test(on: bool) {
    FORCE_DESTROY_REFUSAL.store(on, Relaxed);
}
#[cfg(test)]
pub(super) fn force_object_ready_for_test(on: bool) {
    OBJECT_READY.store(on, Relaxed);
}

/// A fresh host test process without paying for a process per case. Production has deliberately no
/// equivalent: once a real object is quarantined, clearing this latch would reintroduce reuse.
#[cfg(test)]
pub(super) fn reset_native_lifecycle_for_test() {
    ACTIVE_EPOCH.store(0, Relaxed);
    CALLBACK_INTERCEPTS.store(0, Relaxed);
    CALLBACK_GATE_RETIRED.store(false, Relaxed);
    NATIVE_UNLOAD_COMPLETED.store(false, Relaxed);
    LIFECYCLE_BLOCKED.store(false, Relaxed);
    FORCE_DESTROY_REFUSAL.store(false, Relaxed);
    OBJECT_READY.store(false, Relaxed);
    LOADED.store(false, Relaxed);
    LOAD_IN_FLIGHT.store(false, Relaxed);
    IN_FLIGHT_CALLS.store(0, Relaxed);
    *IN_FLIGHT_VERB.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *HOLD_LOAD.0.lock().unwrap_or_else(|e| e.into_inner()) = false;
    HOLD_LOAD.1.notify_all();
    UNLOAD_DELAY_MS.store(0, Relaxed);
    Clock::rewind();
}

/// Start the sink clock running at `base_ns` in the fed-PTS space with plenty of fed media ahead,
/// so a test can watch whether it advances. Test-only: production starts it through `sf_play`.
#[cfg(test)]
pub(super) fn clock_run_for_test(base_ns: i64) {
    Clock::rewind();
    BASE_NS.store(base_ns, Relaxed);
    FED_MAX_NS.store(i64::MAX / 2, Relaxed);
    RESUMED_AT_MS.store(now_ms(), Relaxed);
    PLAYING.store(true, Relaxed);
}
/// The sink clock's position and whether it is running. Test-only.
#[cfg(test)]
pub(super) fn clock_state_for_test() -> (i64, bool) {
    (Clock::position_ns(), PLAYING.load(Relaxed))
}
/// Take one sink tick (what the `clocksink` thread does every [`TICK_MS`]) on the calling thread.
/// Test-only.
#[cfg(test)]
pub(super) fn tick_for_test() {
    Clock::tick();
}
/// Run the clock `ms` of wall time forward while it is playing, for EVERY thread that reads it:
/// the `clocksink` thread is not the test's, so a per-thread pin ([`pinned_clock`]) would leave it
/// reporting the real time. Backdates the resume stamp, which is all `position_ns` adds elapsed
/// time to. Test-only.
#[cfg(test)]
pub(super) fn advance_clock_for_test(ms: i64) {
    RESUMED_AT_MS.fetch_sub(ms, Relaxed);
}

impl Clock {
    /// Where the sink claims to be presenting, in the fed-PTS timeline `sf_on_event` expects.
    ///
    /// **Clamped to what has actually been fed**, which is the property that makes the whole thing
    /// a plant rather than a fiction: a starved pipeline stops advancing, the reserve stops
    /// draining at the reader, and the app sees the same shape a real underrun produces. An
    /// unclamped clock would run away from the data and report healthy playback through a stall.
    fn position_ns() -> i64 {
        let base = BASE_NS.load(Relaxed);
        let fed = FED_MAX_NS.load(Relaxed);
        let elapsed = if PLAYING.load(Relaxed) {
            (now_ms() - RESUMED_AT_MS.load(Relaxed)).max(0)
        } else {
            0
        };
        let mut free = base.saturating_add(elapsed.saturating_mul(1_000_000));
        let stop = STOP_AT_MEDIA_NS.load(Relaxed);
        if stop != i64::MAX {
            free = free.min(media_to_fed(stop));
        }
        if fed == i64::MIN {
            base
        } else {
            free.min(fed)
        }
    }

    /// Freeze the clock where it is. Idempotent.
    ///
    /// **Read the position BEFORE flipping the flag.** `position_ns` adds elapsed time only while
    /// `PLAYING`, so `swap(false)` then read returns the base unchanged and every pause silently
    /// rewinds the clock to wherever it last resumed. That is what the first version of this did,
    /// and `holding_then_resuming_does_not_lose_or_gain_position` is the test that found it.
    fn hold() {
        let at = Self::position_ns();
        if PLAYING.swap(false, Relaxed) {
            BASE_NS.store(at, Relaxed);
        }
    }

    /// Run the clock from wherever it was held. Idempotent.
    fn resume() {
        if !PLAYING.swap(true, Relaxed) {
            RESUMED_AT_MS.store(now_ms(), Relaxed);
        }
        Self::start_ticker();
    }

    /// Rewind to the start of a fresh stream (a fresh `Load`, or `sf_flush`, which is itself a seek
    /// to 0), so the fed timeline restarts at zero. A native `Seek` does not come here: it
    /// re-anchors at the millis it was given (`sf_seek_ms`).
    fn rewind() {
        PLAYING.store(false, Relaxed);
        BASE_NS.store(0, Relaxed);
        FED_MAX_NS.store(i64::MIN, Relaxed);
        EOS_PENDING.store(false, Relaxed);
    }

    /// What the sink reports every [`TICK_MS`] while a loaded session is running: the position,
    /// and, once an end-of-stream has been pushed and the clock has played out to the last video
    /// frame it was fed, the pipeline's end-of-stream.
    ///
    /// **Why the clock reaching [`FED_MAX_NS`] is the pipeline's end.** The clock is clamped to the
    /// highest VIDEO pts fed, which is where a real pipeline's presentation stops once its last
    /// frame is out. A film whose audio outlasts its video leaves the position there for good, and
    /// the arithmetic the pump falls back on (position within `EOS_TAIL_NS` of the container's
    /// duration, the longest track's) never holds. What ends such a film is the pipeline saying so:
    /// `smp_cb type=28 num=1 str=true`, sent after the last video frame was presented and with the
    /// clock still at that frame. Measured twice, with the same order both times: a contributor's
    /// LG C1 on webOS 6.5.3 (the clock stopped at 5894 s of 5897 s, #865) and the dev TV on webOS
    /// 4.10.2 (ACB route; an H.264 + AC3 clip whose video ends at 20 s and audio at 23 s: `EOS
    /// pushed at true EOF`, the clock stuck at `pos=19s`, then `type=28`). Until this existed the
    /// simulator reproduced the stall at the video tail but never the `type=28` that ends the
    /// film, so #865's `pipeline_eos` arm could not fire here.
    ///
    /// A paused clock reports nothing, as a paused pipeline does not finish. It is sent through
    /// [`super::sf_on_event`], like the position, and sets nothing in `SHARED` itself.
    ///
    /// A function of its own so a test can take a tick on its own thread ([`tick_for_test`])
    /// instead of waiting for the ticker's wall-clock sleep.
    fn tick() {
        if !LOADED.load(Relaxed) || !PLAYING.load(Relaxed) {
            return;
        }
        let at = Clock::position_ns();
        report_position(at);
        let fed = FED_MAX_NS.load(Relaxed);
        if fed != i64::MIN && at >= fed && EOS_PENDING.swap(false, Relaxed) {
            report_end_of_stream();
        }
    }

    /// One thread for the process, reporting position at the device's own cadence.
    ///
    /// It calls [`super::sf_on_event`] — the same `#[no_mangle]` entry point LG's library thread
    /// calls, with the same arguments — rather than writing `SHARED` directly. That is deliberate:
    /// the mapping from a fed PTS to `playpos_ns` (the `pts_shift`/`disp_base` rebase) lives in
    /// that callback, and a sink that bypassed it would be testing a path the television does not
    /// take.
    fn start_ticker() {
        if TICKING.swap(true, Relaxed) {
            return;
        }
        let spawned = std::thread::Builder::new()
            .name("clocksink".into())
            .spawn(|| loop {
                std::thread::sleep(std::time::Duration::from_millis(TICK_MS));
                Clock::tick();
            });
        if spawned.is_err() {
            TICKING.store(false, Relaxed);
            plx_base::eventlog::log("clocksink: could not spawn the position thread; no position will report");
        }
    }
}

unsafe fn sf_load(payload: *const c_char, epoch: u32) -> c_int {
    if !enabled() || epoch == 0 || OBJECT_READY.load(Relaxed) || LIFECYCLE_BLOCKED.load(Relaxed) {
        return 0; // "pipeline could not be constructed" — the engine's existing failure path
    }
    Clock::rewind();
    // `plxnative-simvideo`: the screenshot picture (`player::sim_video`) learns the codec here.
    if !payload.is_null() {
        let payload = std::ffi::CStr::from_ptr(payload).to_string_lossy();
        crate::player::sim_video::load(&payload, Clock::position_ns);
    }
    ACTIVE_EPOCH.store(epoch, Relaxed);
    CALLBACK_INTERCEPTS.store(0, Relaxed);
    CALLBACK_GATE_RETIRED.store(false, Relaxed);
    NATIVE_UNLOAD_COMPLETED.store(false, Relaxed);
    OBJECT_READY.store(true, Relaxed);
    LOADED.store(true, Relaxed);
    if take_refusal() {
        // **`plxnative-refuseload[=N]`: the webOS 10.3.1 refusal, on a Mac.** The sequence is the
        // lab set's own (`docs/webos10-lab-report.md` §3.2): the pipeline acknowledges, echoes the
        // sink envelope it was handed as `type=5`, and then refuses with `type=18 num=601` —
        // AFTER `Load()` has returned ok=1. That asynchronous shape is what left the app on a black
        // screen for 70 s with a healthy-looking state machine, and it is the one no other host
        // path can produce: `LIFECYCLE_BLOCKED` makes `sf_load` return 0, which is the synchronous
        // refusal and a different code path. N counts Loads refused before the sink behaves again
        // (absent = every Load), so a rollback that reloads can be made to succeed or to fail too.
        for (ty, num, s) in [
            (13, 1, c""),
            (14, 0, c"1"),
            (
                5,
                0,
                c"0 video/x-h264 (null) (null) 3840 2160 (null) 60.000000 0 0 0",
            ),
            (15, 0, c"1"),
            (8, 0, c"audio/mpeg"),
            (18, 601, c"Resource Allocation Error"),
        ] {
            CALLBACK_INTERCEPTS.fetch_add(1, Relaxed);
            super::sf_on_event(epoch, ty, num, s.as_ptr());
        }
        return 1;
    }
    // The engine waits on `SHARED.load_completed`, which is set by parsing a callback STRING. Go
    // through the real callback rather than setting the flag, so the parse is exercised. Type 2 is
    // benign: the harness greps `smp_cb type=18` for a playback error and this is not one.
    CALLBACK_INTERCEPTS.fetch_add(1, Relaxed);
    super::sf_on_event(epoch, 2, 0, c"{\"loadCompleted\":true}".as_ptr());
    // issue #74 D.1 test seam: on the real (v0.6.0) seam this is the exact window between
    // `SMP_SET_READY(1)` and the real, blocking Load call returning — `loadCompleted` has
    // already fired (LG's pipeline can signal it from inside `Load()`) but the call has not come
    // back yet. The unmodified host stub has no such window at all; `HOLD_LOAD` opens one only
    // when a test has armed it (`hold_load_for_test`), so every other host test's `sf_load` is
    // unaffected.
    #[cfg(any(test, feature = "test-support"))]
    {
        LOAD_IN_FLIGHT.store(true, Relaxed);
        let mut held = HOLD_LOAD.0.lock().unwrap_or_else(|e| e.into_inner());
        while *held {
            held = HOLD_LOAD.1.wait(held).unwrap_or_else(|e| e.into_inner());
        }
        drop(held);
        LOAD_IN_FLIGHT.store(false, Relaxed);
    }
    1
}

/// How many more host Loads `plxnative-refuseload[=N]` still refuses. Read once (a trigger is a
/// boot-time fact like every other), counted down per `sf_load`; `i64::MAX` for the bare flag.
static REFUSALS_LEFT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
fn take_refusal() -> bool {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let n = match plx_base::devtrig::read("refuseload") {
            None => 0,
            Some(v) if v.trim().is_empty() => i64::MAX,
            Some(v) => v.trim().parse::<i64>().unwrap_or(0),
        };
        if n > 0 {
            plx_base::eventlog::log(&format!(
                "clocksink: plxnative-refuseload armed — the next {} Load(s) get the webOS 10.3.1 \
                 type=18 num=601 refusal after Load() returns ok=1",
                if n == i64::MAX { "∞".to_string() } else { n.to_string() }
            ));
        }
        REFUSALS_LEFT.store(n, Relaxed);
    });
    loop {
        let left = REFUSALS_LEFT.load(Relaxed);
        if left <= 0 {
            return false;
        }
        let next = if left == i64::MAX { left } else { left - 1 };
        if REFUSALS_LEFT
            .compare_exchange(left, next, Relaxed, Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}
unsafe fn sf_ready() -> c_int {
    c_int::from(enabled() && OBJECT_READY.load(Relaxed))
}
unsafe fn sf_is_load_completed() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_is_load_completed");
    c_int::from(enabled() && LOADED.load(Relaxed))
}
unsafe fn sf_play() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_play");
        note_verb("play".into());
        PLAY_CALLS.fetch_add(1, Relaxed);
        let forced = FORCE_PLAY_RESULT.load(Relaxed);
        if forced != i32::MIN {
            return forced;
        }
    }
    if !enabled() {
        return 0;
    }
    Clock::resume();
    1
}
unsafe fn sf_pause() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_pause");
        note_verb("pause".into());
        let forced = FORCE_PAUSE_RESULT.load(Relaxed);
        if forced != i32::MIN {
            return forced;
        }
    }
    if !enabled() {
        return 0;
    }
    Clock::hold();
    1
}
unsafe fn sf_set_volume(volume: c_int) -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_set_volume");
        note_verb(format!("volume:{volume}"));
        HOST_VOLUME.store(volume, Relaxed);
    }
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = volume;
    c_int::from(enabled())
}
unsafe fn sf_flush() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_flush");
        FLUSH_CALLS.fetch_add(1, Relaxed);
    }
    if !enabled() {
        return 0;
    }
    Clock::rewind();
    crate::player::sim_video::flush();
    1
}
/// LG's public `Seek(millis)`: refuses a negative target, otherwise flushes the pipeline and
/// re-anchors the segment (and so the fed-PTS clock) at the millis passed, paused, with nothing fed.
/// `FORCE_SEEK_RESULT` injects LG's refusal.
unsafe fn sf_seek_ms(ms: i64) -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_seek_ms");
        SEEK_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(ms);
        let forced = FORCE_SEEK_RESULT.load(Relaxed);
        if forced != i32::MIN {
            return forced;
        }
    }
    if !enabled() || ms < 0 {
        return 0;
    }
    PLAYING.store(false, Relaxed);
    BASE_NS.store(ms.saturating_mul(1_000_000), Relaxed);
    FED_MAX_NS.store(i64::MIN, Relaxed);
    EOS_PENDING.store(false, Relaxed);
    crate::player::sim_video::flush();
    1
}
/// `PushEOS`: the engine has fed the last access unit. The pipeline answers with its own
/// end-of-stream once it has presented them all, which [`Clock::tick`] reproduces.
unsafe fn sf_push_eos() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_push_eos");
    if !enabled() {
        return 0;
    }
    EOS_PENDING.store(true, Relaxed);
    1
}
unsafe fn sf_set_time_to_decode(_position_ns: i64) -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_set_time_to_decode");
        SEGMENT_CALLS.fetch_add(1, Relaxed);
    }
    c_int::from(enabled())
}
unsafe fn sf_set_content_info(_position_ns: i64) -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_set_content_info");
        SEGMENT_CALLS.fetch_add(1, Relaxed);
    }
    c_int::from(enabled())
}
unsafe fn sf_send_segment() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    {
        note_dispatch("sf_send_segment");
        SEGMENT_CALLS.fetch_add(1, Relaxed);
    }
    c_int::from(enabled())
}

/// Accept the AU, discard it, and remember how far the VIDEO lane has been fed.
///
/// `es_data == 1` is video (`engine.rs`'s `es`), and only video moves the clock's ceiling: the
/// audio lane is allowed to run `AUDIO_SLACK_NS` ahead by design, so clamping to it would let the
/// clock outrun the pictures it claims to be presenting.
///
/// Always `'O'`. `'B'` (BufferFull) is the real pipeline's own backpressure, and the sink has no
/// buffer to fill — the app's backpressure is upstream, in the AU queues' byte caps and the
/// feed-ahead throttle, and those are exactly what this exists to exercise. Returning `'B'` here
/// would add a second, fictional one.
unsafe fn sf_feed(p: *const u8, size: c_uint, pts: i64, es_data: c_int) -> c_char {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_feed");
    if !enabled() {
        return FEED_ERROR;
    }
    if es_data == 1 {
        FED_MAX_NS.fetch_max(pts, Relaxed);
        if !p.is_null() && size > 0 {
            crate::player::sim_video::feed(std::slice::from_raw_parts(p, size as usize), pts);
        }
    }
    FEED_OK
}
unsafe fn sf_unload() {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_unload");
    #[cfg(test)]
    {
        let ms = UNLOAD_DELAY_MS.load(Relaxed);
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms));
        }
    }
    let epoch = ACTIVE_EPOCH.load(Relaxed);
    if epoch != 0 && OBJECT_READY.load(Relaxed) {
        // Firmware emits this synthetic lifecycle callback before Unload returns. It bypasses the
        // callbackFunctionHook interposer, so it does not contribute to CALLBACK_INTERCEPTS.
        NATIVE_UNLOAD_COMPLETED.store(true, Relaxed);
        super::sf_on_event(epoch, 23, 0, std::ptr::null());
    }
    LOADED.store(false, Relaxed);
    Clock::rewind();
    crate::player::sim_video::stop();
}
unsafe fn sf_callback_gate_retire() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_callback_gate_retire");
    CALLBACK_GATE_RETIRED.store(true, Relaxed);
    c_int::from(
        OBJECT_READY.load(Relaxed)
            && !LIFECYCLE_BLOCKED.load(Relaxed)
            && CALLBACK_INTERCEPTS.load(Relaxed) != 0,
    )
}
unsafe fn sf_callback_intercepts() -> c_uint {
    CALLBACK_INTERCEPTS.load(Relaxed)
}
unsafe fn sf_destroy() -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_destroy");
    #[cfg(any(test, feature = "test-support"))]
    let refused = FORCE_DESTROY_REFUSAL.load(Relaxed);
    #[cfg(not(any(test, feature = "test-support")))]
    let refused = false;
    let safe = !refused
        && OBJECT_READY.load(Relaxed)
        && CALLBACK_GATE_RETIRED.load(Relaxed)
        && CALLBACK_INTERCEPTS.load(Relaxed) != 0
        && NATIVE_UNLOAD_COMPLETED.load(Relaxed)
        && !LIFECYCLE_BLOCKED.load(Relaxed);
    if !safe {
        sf_quarantine();
        return 0;
    }
    ACTIVE_EPOCH.store(0, Relaxed);
    OBJECT_READY.store(false, Relaxed);
    LOADED.store(false, Relaxed);
    Clock::rewind();
    crate::player::sim_video::stop();
    1
}
unsafe fn sf_quarantine() {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("sf_quarantine");
    CALLBACK_GATE_RETIRED.store(true, Relaxed);
    LIFECYCLE_BLOCKED.store(true, Relaxed);
    OBJECT_READY.store(false, Relaxed);
    LOADED.store(false, Relaxed);
    Clock::rewind();
    crate::player::sim_video::stop();
}

/// `VP_NONE` — "video cannot be displayed, but the app still runs", which is precisely the
/// simulator's situation and an existing, handled television state.
///
/// With the clock sink armed this becomes `VP_EXPORTED`, the webOS 5+ path: video binds through an
/// exported window id and ACB is never touched, so the engine's ACB stage sequence stays SKIPPED
/// rather than faked. Faking ACB would mean modelling a bind order this file cannot verify.
unsafe fn vp_mode() -> c_int {
    if enabled() {
        plx_platform::tv::sink::VP_EXPORTED
    } else {
        plx_platform::tv::sink::VP_NONE
    }
}
unsafe fn vp_create_window() -> *const c_char {
    if enabled() {
        c"clocksink-window".as_ptr()
    } else {
        std::ptr::null()
    }
}
/// Never NUL — contracted to return a valid string even when no window exists, and `ui::stats`
/// reads it unconditionally.
unsafe fn vp_window_id() -> *const c_char {
    if enabled() {
        c"clocksink-window".as_ptr()
    } else {
        c"".as_ptr()
    }
}
unsafe fn vp_place(
    _src_w: c_int,
    _src_h: c_int,
    _dst_x: c_int,
    _dst_y: c_int,
    _dst_w: c_int,
    _dst_h: c_int,
) -> c_int {
    c_int::from(enabled())
}
unsafe fn vp_destroy_window() {}

unsafe fn acb_create(_app_id: *const c_char, _player_type: c_int) -> c_long {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_create");
    0 // 0 = failed, per starfish.h. Unreached under VP_EXPORTED, and not faked for the same reason.
}
unsafe fn acb_bind(_media_id: *const c_char) {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_bind");
}
unsafe fn acb_send_video_data(_source_info: *const c_char) -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_send_video_data");
    #[cfg(test)]
    {
        ACB_VIDEO_DATA_CALLS.fetch_add(1, Relaxed);
        if ACB_VIDEO_DATA_ACCEPT.load(Relaxed) {
            return 0;
        }
    }
    -1 // -1 = rejected, per starfish.h
}
unsafe fn acb_send_atmos(_media_id: *const c_char) -> c_int {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_send_atmos");
    0 // 0 = no ACB / no symbol, which is exactly the host's situation
}
unsafe fn acb_start(_x: c_long, _y: c_long, _w: c_long, _h: c_long) {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_start");
    #[cfg(test)]
    ACB_START_CALLS.fetch_add(1, Relaxed);
}
unsafe fn acb_unload() {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_unload");
}
unsafe fn acb_pause() {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_pause");
    #[cfg(test)]
    ACB_PAUSE_CALLS.fetch_add(1, Relaxed);
}
unsafe fn acb_resume() {
    #[cfg(any(test, feature = "test-support"))]
    note_dispatch("acb_resume");
    #[cfg(test)]
    ACB_RESUME_CALLS.fetch_add(1, Relaxed);
}

/// The simulator's [`VideoSink`]: every method is the verb of the same name above.
pub struct HostSink;

impl VideoSink for HostSink {
    unsafe fn load(&self, payload: *const c_char, epoch: u32) -> c_int {
        sf_load(payload, epoch)
    }
    unsafe fn ready(&self, _: &MainThread) -> c_int {
        sf_ready()
    }
    unsafe fn is_load_completed(&self, _: &MainThread) -> c_int {
        sf_is_load_completed()
    }
    unsafe fn play(&self, _: &MainThread) -> c_int {
        sf_play()
    }
    unsafe fn pause(&self, _: &MainThread) -> c_int {
        sf_pause()
    }
    unsafe fn set_volume(&self, _: &MainThread, volume: c_int) -> c_int {
        sf_set_volume(volume)
    }
    unsafe fn flush(&self, _: &MainThread) -> c_int {
        sf_flush()
    }
    unsafe fn push_eos(&self, _: &MainThread) -> c_int {
        sf_push_eos()
    }
    unsafe fn set_time_to_decode(&self, _: &MainThread, position_ns: i64) -> c_int {
        sf_set_time_to_decode(position_ns)
    }
    unsafe fn set_content_info(&self, _: &MainThread, position_ns: i64) -> c_int {
        sf_set_content_info(position_ns)
    }
    unsafe fn send_segment(&self, _: &MainThread) -> c_int {
        sf_send_segment()
    }
    unsafe fn seek(&self, _: &MainThread, ms: i64) -> c_int {
        sf_seek_ms(ms)
    }
    fn feed_abi_is_cow(&self) -> bool {
        false
    }
    unsafe fn feed(
        &self,
        _: &MainThread,
        p: *const u8,
        size: c_uint,
        pts: i64,
        es_data: c_int,
    ) -> c_char {
        sf_feed(p, size, pts, es_data)
    }
    unsafe fn unload(&self, _: &plx_platform::tv::sink::UnloadTicket) {
        sf_unload()
    }
    unsafe fn callback_gate_retire(&self, _: &MainThread) -> c_int {
        sf_callback_gate_retire()
    }
    unsafe fn callback_intercepts(&self, _: &MainThread) -> u32 {
        sf_callback_intercepts()
    }
    unsafe fn destroy(&self, _: &MainThread) -> c_int {
        sf_destroy()
    }
    unsafe fn quarantine(&self, _: &MainThread) {
        sf_quarantine()
    }
    fn window_mode(&self) -> c_int {
        unsafe { vp_mode() }
    }
    fn window_id(&self) -> &'static CStr {
        // SAFETY: `vp_window_id` is never null and returns a static, NUL-terminated literal.
        unsafe { CStr::from_ptr(vp_window_id()) }
    }
    unsafe fn create_window(&self, _: &MainThread) -> *const c_char {
        vp_create_window()
    }
    #[allow(clippy::too_many_arguments)]
    unsafe fn place_window(
        &self,
        _: &MainThread,
        src_w: c_int,
        src_h: c_int,
        dst_x: c_int,
        dst_y: c_int,
        dst_w: c_int,
        dst_h: c_int,
    ) -> c_int {
        vp_place(src_w, src_h, dst_x, dst_y, dst_w, dst_h)
    }
    unsafe fn destroy_window(&self, _: &MainThread) {
        vp_destroy_window()
    }
    unsafe fn plane_create(
        &self,
        _: &MainThread,
        app_id: *const c_char,
        player_type: c_int,
    ) -> c_long {
        acb_create(app_id, player_type)
    }
    unsafe fn plane_bind(&self, _: &MainThread, media_id: *const c_char) {
        acb_bind(media_id)
    }
    unsafe fn plane_send_video_data(&self, _: &MainThread, source_info: *const c_char) -> c_int {
        acb_send_video_data(source_info)
    }
    unsafe fn plane_send_atmos(&self, _: &MainThread, media_id: *const c_char) -> c_int {
        acb_send_atmos(media_id)
    }
    unsafe fn plane_start(&self, _: &MainThread, x: c_long, y: c_long, w: c_long, h: c_long) {
        acb_start(x, y, w, h)
    }
    unsafe fn plane_unload(&self, _: &MainThread) {
        acb_unload()
    }
    unsafe fn plane_pause(&self, _: &MainThread) {
        acb_pause()
    }
    unsafe fn plane_resume(&self, _: &MainThread) {
        acb_resume()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam state is process-global and the engine's hostsim tests drive the same atomics.
    /// Use the crate-wide lock rather than a module-local mutex: two different locks made both
    /// suites individually serial while still allowing them to overwrite `FED_MAX_NS` together.
    fn lock() -> plx_base::testlock::Serial {
        plx_base::testlock::serial()
    }

    fn fresh() {
        Clock::rewind();
        FED_MAX_NS.store(i64::MIN, Relaxed);
    }

    #[test]
    fn a_media_stop_parks_the_running_clock_exactly_on_it_and_a_pause_keeps_it_there() {
        let _g = lock();
        fresh();
        // The fed timeline is rebased onto the movie's: playpos = fed - pts_shift + disp_base.
        let shared = &super::super::SHARED;
        shared.pts_shift.store(2_000_000_000, Relaxed);
        shared.disp_base.store(435_000_000_000, Relaxed);
        FED_MAX_NS.store(600_000_000_000, Relaxed);
        BASE_NS.store(2_000_000_000, Relaxed);
        RESUMED_AT_MS.store(now_ms(), Relaxed);
        PLAYING.store(true, Relaxed);
        assert!(stop_clock_at(Some(446_020_000_000)), "11 s ahead of the playhead: armed");
        // A minute of wall time later the clock has run into the stop and waits on it, on the
        // exact fed PTS the movie position maps to, not wherever the wall clock got to.
        RESUMED_AT_MS.store(now_ms() - 60_000, Relaxed);
        assert_eq!(Clock::position_ns(), 13_020_000_000);
        Clock::hold();
        assert_eq!(BASE_NS.load(Relaxed), 13_020_000_000, "the pause keeps the stop's position");
        stop_clock_at(None);
        assert_eq!(Clock::position_ns(), 13_020_000_000, "lifting it does not move a held clock");
        // A stop the playhead has already passed is refused: the clock would otherwise jump back.
        assert!(!stop_clock_at(Some(440_000_000_000)));
        assert_eq!(Clock::position_ns(), 13_020_000_000);
        shared.pts_shift.store(0, Relaxed);
        shared.disp_base.store(0, Relaxed);
    }

    #[test]
    fn a_held_clock_does_not_advance() {
        let _g = lock();
        fresh();
        FED_MAX_NS.store(60_000_000_000, Relaxed);
        BASE_NS.store(5_000_000_000, Relaxed);
        assert_eq!(Clock::position_ns(), 5_000_000_000);
    }

    #[test]
    fn the_clock_never_runs_past_what_has_been_fed() {
        let _g = lock();
        fresh();
        // Playing, started a long time ago, but only 2 s of video ever fed: the sink must report
        // 2 s and not the elapsed wall time. This is the whole difference between a plant and a
        // fiction — an unclamped clock reports healthy playback straight through a starved
        // pipeline, which is the one failure the sink exists to make visible.
        BASE_NS.store(0, Relaxed);
        FED_MAX_NS.store(2_000_000_000, Relaxed);
        RESUMED_AT_MS.store(now_ms() - 60_000, Relaxed);
        PLAYING.store(true, Relaxed);
        assert_eq!(Clock::position_ns(), 2_000_000_000);
        PLAYING.store(false, Relaxed);
    }

    #[test]
    fn holding_then_resuming_does_not_lose_or_gain_position() {
        let _g = lock();
        fresh();
        FED_MAX_NS.store(600_000_000_000, Relaxed);
        BASE_NS.store(0, Relaxed);
        RESUMED_AT_MS.store(now_ms() - 1_000, Relaxed);
        PLAYING.store(true, Relaxed);
        Clock::hold();
        let held = BASE_NS.load(Relaxed);
        assert!(
            held >= 1_000_000_000,
            "a second of wall time is a second of media, got {held}ns"
        );
        assert_eq!(
            Clock::position_ns(),
            held,
            "a held clock reports where it stopped"
        );
        Clock::hold();
        assert_eq!(
            BASE_NS.load(Relaxed),
            held,
            "holding twice must not advance it again"
        );
    }

    #[test]
    fn a_rewind_forgets_both_the_position_and_the_fed_ceiling() {
        let _g = lock();
        fresh();
        FED_MAX_NS.store(9_000_000_000, Relaxed);
        BASE_NS.store(9_000_000_000, Relaxed);
        Clock::rewind();
        assert_eq!(BASE_NS.load(Relaxed), 0);
        assert_eq!(FED_MAX_NS.load(Relaxed), i64::MIN);
        // Nothing fed yet: the clock reports the base rather than running free from it.
        assert_eq!(Clock::position_ns(), 0);
    }

    #[test]
    fn only_the_video_lane_raises_the_ceiling() {
        let _g = lock();
        fresh();
        // Audio is permitted to run AUDIO_SLACK_NS ahead of video by design, so a clock clamped to
        // the audio lane would outrun the pictures it claims to present.
        unsafe {
            sf_feed(std::ptr::null(), 0, 8_000_000_000, 0);
        }
        assert_eq!(
            FED_MAX_NS.load(Relaxed),
            i64::MIN,
            "audio must not move the ceiling"
        );
    }

    /// A loaded, running clock that has played out everything it was fed (20 s of video).
    fn at_the_tail() {
        fresh();
        force_clocksink_for_test(true);
        run_to_the_tail();
    }

    /// The clock state `at_the_tail` ends in, WITHOUT clearing a latch a test is grading.
    fn run_to_the_tail() {
        LOADED.store(true, Relaxed);
        FED_MAX_NS.store(20_000_000_000, Relaxed);
        BASE_NS.store(0, Relaxed);
        RESUMED_AT_MS.store(now_ms() - 60_000, Relaxed);
        PLAYING.store(true, Relaxed);
        EOS_REPORTS.store(0, Relaxed);
    }

    fn reports_after(ticks: u32) -> u64 {
        for _ in 0..ticks {
            Clock::tick();
        }
        EOS_REPORTS.load(Relaxed)
    }

    fn disarm() {
        reset_native_lifecycle_for_test();
        force_clocksink_for_test(false);
    }

    #[test]
    fn the_sink_reports_the_end_of_stream_only_after_one_is_pushed_and_once_per_push() {
        let _g = lock();
        at_the_tail();
        assert_eq!(reports_after(3), 0, "nothing is sent before an EOS is pushed, tail or not");
        assert_eq!(unsafe { sf_push_eos() }, 1);
        assert_eq!(reports_after(1), 1, "the clock is at the last fed frame: the pipeline is done");
        assert_eq!(reports_after(5), 1, "once per pushed EOS");
        assert_eq!(unsafe { sf_push_eos() }, 1);
        assert_eq!(reports_after(1), 2, "a second push is a second end");
        disarm();
    }

    #[test]
    fn a_pushed_end_of_stream_waits_for_the_clock_to_play_out() {
        let _g = lock();
        at_the_tail();
        RESUMED_AT_MS.store(now_ms() - 5_000, Relaxed); // 5 s in, 15 s of fed video still to play
        unsafe { sf_push_eos() };
        assert_eq!(reports_after(2), 0, "the pipeline has not presented what it holds");
        RESUMED_AT_MS.fetch_sub(20_000, Relaxed);
        assert_eq!(reports_after(1), 1);
        disarm();
    }

    #[test]
    fn a_paused_clock_does_not_finish() {
        let _g = lock();
        at_the_tail();
        unsafe { sf_push_eos() };
        Clock::hold();
        assert_eq!(reports_after(3), 0, "a paused pipeline does not reach its end");
        Clock::resume();
        assert_eq!(reports_after(1), 1, "it finishes when it is resumed");
        disarm();
    }

    #[test]
    fn a_pushed_end_of_stream_does_not_survive_the_fed_timeline_restarting() {
        let _g = lock();
        let restarts: [(&str, fn()); 4] = [
            ("rewind", Clock::rewind),
            ("flush", || {
                unsafe { sf_flush() };
            }),
            ("seek", || {
                unsafe { sf_seek_ms(0) };
            }),
            ("unload", || unsafe { sf_unload() }),
        ];
        for (what, restart) in restarts {
            at_the_tail();
            unsafe { sf_push_eos() };
            restart();
            run_to_the_tail(); // the same clock a new timeline would reach, with the latch as it was left
            assert_eq!(reports_after(2), 0, "a pushed EOS survived a {what}");
        }
        disarm();
    }

    /// `sf_unload` is one of the nine verbs the real seam routes through `sf_ready_object()`
    /// (`grep -n 'sf_ready_object()' src/starfish.c`), so the in-flight instrument must see it
    /// too — otherwise an Unload reaching the seam mid-Load (the one verb that would destroy the
    /// object the Load call is still inside) would score zero dispatches and
    /// `nothing_reaches_the_seam_while_load_is_still_in_flight` would pass having proven nothing
    /// about that verb.
    #[test]
    fn sf_unload_is_visible_to_the_in_flight_instrument() {
        let _g = lock();
        fresh();
        IN_FLIGHT_CALLS.store(0, Relaxed);
        *IN_FLIGHT_VERB.lock().unwrap_or_else(|e| e.into_inner()) = None;
        LOAD_IN_FLIGHT.store(true, Relaxed);
        unsafe {
            sf_unload();
        }
        LOAD_IN_FLIGHT.store(false, Relaxed);
        assert_eq!(
            IN_FLIGHT_CALLS.load(Relaxed),
            1,
            "sf_unload must bump IN_FLIGHT_CALLS like every other seam verb sf_ready_object() \
             gates, or the primary in-flight test's zero-dispatch assertion cannot see this verb"
        );
        assert_eq!(
            *IN_FLIGHT_VERB.lock().unwrap_or_else(|e| e.into_inner()),
            Some("sf_unload")
        );
    }
}
