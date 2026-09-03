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
//! It reports [`VP_EXPORTED`](super::VP_EXPORTED), the webOS 5+ path, because that one binds video
//! through an exported window id and never touches ACB — so the whole ACB stage sequence stays
//! skipped rather than faked.

use std::os::raw::{c_char, c_int, c_long, c_uint};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering::Relaxed};
#[cfg(test)]
use std::sync::atomic::{AtomicI32, AtomicU64};

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
#[cfg(test)]
pub(super) static FORCE_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
/// Force the next host `Play` calls to report a seam result. `i32::MIN` means normal clocksink
/// behaviour. This makes the state-machine invariant testable without teaching production code a
/// failure-injection branch.
#[cfg(test)]
pub(super) static FORCE_PLAY_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);
#[cfg(test)]
pub(super) static PLAY_CALLS: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
pub(super) static FORCE_PAUSE_RESULT: AtomicI32 = AtomicI32::new(i32::MIN);

/// Is the clock sink armed? Read once, at the first seam call, and latched.
///
/// A trigger rather than a build feature so one binary does both: `make sim` keeps landing on the
/// failure read-out (which is what the UI work wants to see) unless a session explicitly asks for
/// a pipeline run.
fn enabled() -> bool {
    #[cfg(test)]
    if FORCE_ENABLED.load(Relaxed) {
        return true;
    }
    static ONCE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ONCE.get_or_init(|| {
        let on = crate::dev::flag("clocksink");
        if on {
            crate::log(
                "clocksink: ARMED — AUs are accepted and discarded, and the presentation clock \
                 advances at real time. NOTHING IS DECODED and no number from this run is a \
                 device measurement.",
            );
        }
        on
    })
}

/// **This file is included as `player::ffi::sys`, so `super` is `ffi` and not `player`.** These two
/// hop the extra level once, here, instead of at eight call sites where the reader would have to
/// count colons to see which module is meant.
fn now_ms() -> i64 {
    i64::from(super::super::vclock_ms())
}
fn report_position(pts_ns: i64) {
    let epoch = ACTIVE_EPOCH.load(Relaxed);
    if epoch != 0 && !CALLBACK_GATE_RETIRED.load(Relaxed) {
        CALLBACK_INTERCEPTS.fetch_add(1, Relaxed);
        super::super::sf_on_event(epoch, 0, pts_ns, std::ptr::null());
    }
}

/// Everything the sink remembers. One session's worth; `sf_unload` clears it.
struct Clock;

static LOADED: AtomicBool = AtomicBool::new(false);
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

#[cfg(test)]
pub(super) fn force_callback_intercepts_for_test(value: u32) {
    CALLBACK_INTERCEPTS.store(value, Relaxed);
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
    OBJECT_READY.store(false, Relaxed);
    LOADED.store(false, Relaxed);
    Clock::rewind();
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
        let free = base.saturating_add(elapsed.saturating_mul(1_000_000));
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

    /// Rewind to the start of a fresh stream. A seek on this path is a fresh `Load`
    /// (`INPLACE_SEEK_OK` is false under `VP_EXPORTED`), so the fed timeline restarts at zero.
    fn rewind() {
        PLAYING.store(false, Relaxed);
        BASE_NS.store(0, Relaxed);
        FED_MAX_NS.store(i64::MIN, Relaxed);
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
                if !LOADED.load(Relaxed) {
                    continue;
                }
                if PLAYING.load(Relaxed) {
                    report_position(Clock::position_ns());
                }
            });
        if spawned.is_err() {
            TICKING.store(false, Relaxed);
            crate::log("clocksink: could not spawn the position thread; no position will report");
        }
    }
}

pub(super) unsafe fn sf_load(_payload: *const c_char, epoch: u32) -> c_int {
    if !enabled() || epoch == 0 || OBJECT_READY.load(Relaxed) || LIFECYCLE_BLOCKED.load(Relaxed) {
        return 0; // "pipeline could not be constructed" — the engine's existing failure path
    }
    Clock::rewind();
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
            super::super::sf_on_event(epoch, ty, num, s.as_ptr());
        }
        return 1;
    }
    // The engine waits on `SHARED.load_completed`, which is set by parsing a callback STRING. Go
    // through the real callback rather than setting the flag, so the parse is exercised. Type 2 is
    // benign: the harness greps `smp_cb type=18` for a playback error and this is not one.
    CALLBACK_INTERCEPTS.fetch_add(1, Relaxed);
    super::super::sf_on_event(epoch, 2, 0, c"{\"loadCompleted\":true}".as_ptr());
    1
}

/// How many more host Loads `plxnative-refuseload[=N]` still refuses. Read once (a trigger is a
/// boot-time fact like every other), counted down per `sf_load`; `i64::MAX` for the bare flag.
static REFUSALS_LEFT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);
fn take_refusal() -> bool {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let n = match crate::dev::read("refuseload") {
            None => 0,
            Some(v) if v.trim().is_empty() => i64::MAX,
            Some(v) => v.trim().parse::<i64>().unwrap_or(0),
        };
        if n > 0 {
            crate::log(&format!(
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
pub(super) unsafe fn sf_ready() -> c_int {
    c_int::from(enabled() && OBJECT_READY.load(Relaxed))
}
pub(super) unsafe fn sf_is_load_completed() -> c_int {
    c_int::from(enabled() && LOADED.load(Relaxed))
}
pub(super) unsafe fn sf_play() -> c_int {
    #[cfg(test)]
    {
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
pub(super) unsafe fn sf_pause() -> c_int {
    #[cfg(test)]
    {
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
pub(super) unsafe fn sf_flush() -> c_int {
    if !enabled() {
        return 0;
    }
    Clock::rewind();
    1
}
pub(super) unsafe fn sf_push_eos() -> c_int {
    c_int::from(enabled())
}
pub(super) unsafe fn sf_set_time_to_decode(_position_ns: i64) -> c_int {
    c_int::from(enabled())
}
pub(super) unsafe fn sf_set_content_info(_position_ns: i64) -> c_int {
    c_int::from(enabled())
}
pub(super) unsafe fn sf_send_segment() -> c_int {
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
pub(super) unsafe fn sf_feed(_p: *const u8, _size: c_uint, pts: i64, es_data: c_int) -> c_char {
    if !enabled() {
        return FEED_ERROR;
    }
    if es_data == 1 {
        FED_MAX_NS.fetch_max(pts, Relaxed);
    }
    FEED_OK
}
pub(super) unsafe fn sf_unload() {
    let epoch = ACTIVE_EPOCH.load(Relaxed);
    if epoch != 0 && OBJECT_READY.load(Relaxed) {
        // Firmware emits this synthetic lifecycle callback before Unload returns. It bypasses the
        // callbackFunctionHook interposer, so it does not contribute to CALLBACK_INTERCEPTS.
        NATIVE_UNLOAD_COMPLETED.store(true, Relaxed);
        super::super::sf_on_event(epoch, 23, 0, std::ptr::null());
    }
    LOADED.store(false, Relaxed);
    Clock::rewind();
}
pub(super) unsafe fn sf_callback_gate_retire() -> c_int {
    CALLBACK_GATE_RETIRED.store(true, Relaxed);
    c_int::from(
        OBJECT_READY.load(Relaxed)
            && !LIFECYCLE_BLOCKED.load(Relaxed)
            && CALLBACK_INTERCEPTS.load(Relaxed) != 0,
    )
}
pub(super) unsafe fn sf_callback_intercepts() -> c_uint {
    CALLBACK_INTERCEPTS.load(Relaxed)
}
pub(super) unsafe fn sf_destroy() -> c_int {
    let safe = OBJECT_READY.load(Relaxed)
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
    1
}
pub(super) unsafe fn sf_quarantine() {
    CALLBACK_GATE_RETIRED.store(true, Relaxed);
    LIFECYCLE_BLOCKED.store(true, Relaxed);
    OBJECT_READY.store(false, Relaxed);
    LOADED.store(false, Relaxed);
    Clock::rewind();
}

/// `VP_NONE` — "video cannot be displayed, but the app still runs", which is precisely the
/// simulator's situation and an existing, handled television state.
///
/// With the clock sink armed this becomes `VP_EXPORTED`, the webOS 5+ path: video binds through an
/// exported window id and ACB is never touched, so the engine's ACB stage sequence stays SKIPPED
/// rather than faked. Faking ACB would mean modelling a bind order this file cannot verify.
pub(super) unsafe fn vp_mode() -> c_int {
    if enabled() {
        super::VP_EXPORTED
    } else {
        super::VP_NONE
    }
}
pub(super) unsafe fn vp_create_window() -> *const c_char {
    if enabled() {
        c"clocksink-window".as_ptr()
    } else {
        std::ptr::null()
    }
}
/// Never NUL — contracted to return a valid string even when no window exists, and `ui::stats`
/// reads it unconditionally.
pub(super) unsafe fn vp_window_id() -> *const c_char {
    if enabled() {
        c"clocksink-window".as_ptr()
    } else {
        c"".as_ptr()
    }
}
pub(super) unsafe fn vp_place(
    _src_w: c_int,
    _src_h: c_int,
    _dst_x: c_int,
    _dst_y: c_int,
    _dst_w: c_int,
    _dst_h: c_int,
) -> c_int {
    c_int::from(enabled())
}
pub(super) unsafe fn vp_destroy_window() {}

pub(super) unsafe fn acb_create(_app_id: *const c_char, _player_type: c_int) -> c_long {
    0 // 0 = failed, per starfish.h. Unreached under VP_EXPORTED, and not faked for the same reason.
}
pub(super) unsafe fn acb_bind(_media_id: *const c_char) {}
pub(super) unsafe fn acb_send_video_data(_source_info: *const c_char) -> c_int {
    -1 // -1 = rejected, per starfish.h
}
pub(super) unsafe fn acb_send_atmos(_media_id: *const c_char) -> c_int {
    0 // 0 = no ACB / no symbol, which is exactly the host's situation
}
pub(super) unsafe fn acb_start(_x: c_long, _y: c_long, _w: c_long, _h: c_long) {}
pub(super) unsafe fn acb_unload() {}
pub(super) unsafe fn acb_pause() {}
pub(super) unsafe fn acb_resume() {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The seam state is process-global and the engine's hostsim tests drive the same atomics.
    /// Use the crate-wide lock rather than a module-local mutex: two different locks made both
    /// suites individually serial while still allowing them to overwrite `FED_MAX_NS` together.
    fn lock() -> std::sync::MutexGuard<'static, ()> {
        crate::testlock::serial()
    }

    fn fresh() {
        Clock::rewind();
        FED_MAX_NS.store(i64::MIN, Relaxed);
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
}
