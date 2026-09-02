//! player::pump — the main-thread pump (was bufferfeed_pump). Runs each frame from
//! plex_run: the pending-seek handler, the ACB-bind state machine (Stage), and the
//! feed dispatch. All ACB/Starfish control calls happen here on the main thread.
use super::engine::{
    arm_live_clock_prime, drain_aq, engine, feed_both_lanes, feed_sample, Engine, Source,
};
use super::shared::{HlsPauseCompletion, HlsPrimeKind, HlsSeekPause, Stage};
use super::{ffi, ACB_OK, SHARED, TX};
use crate::task::MainThread;
use std::os::raw::c_char;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

/// Publish the one value the HUD renders from. Pure derivation off signals the workers already
/// maintain — no new cross-thread plumbing, and it runs on every path out of `pump` (including
/// the early returns) so the state can never go stale behind a spinner that never resolves.
fn set_state(s: super::shared::PlaybackState) {
    SHARED.pb_state.store(s as u8, Relaxed);
}

/// How many seek requests this pump seek MERGED — i.e. requests that arrived while an earlier
/// seek was still resolving and so never got a seek of their own. Reset as it's read, so each
/// logged seek reports only the requests it consumed.
///
/// The count is what makes coalescing observable. `seek_to_ns` only ever holds the newest target,
/// so a burst of six taps and a single tap leave identical state behind; and the burst is applied
/// by whichever of two paths wins a race with the stuck-watchdog, which log different lines.
/// Counting either line therefore measures PMS latency, not merging — `coalesced=` measures
/// merging directly, and both paths report it.
fn take_coalesced() -> u32 {
    TX.seek_reqs.swap(0, Relaxed).saturating_sub(1)
}

fn prime_before_play(rebase_pending: bool, segmented_hls: bool) -> bool {
    rebase_pending || segmented_hls
}

/// Playable HLS reserve in the same content-time coordinates as the demux controller. The main
/// thread needs this read for the exact depleted-buffer fallback and for rebuffer diagnostics;
/// ordinary ABR decisions remain on the worker.
fn hls_buffered_ms() -> Option<i64> {
    let video = SHARED.hls_video_tail_ns.load(Acquire);
    if video < 0 {
        return None;
    }
    let audio = SHARED.hls_audio_tail_ns.load(Acquire);
    let tail = if audio >= 0 { video.min(audio) } else { video };
    let display_base = SHARED.disp_base.load(Relaxed).max(0);
    Some(
        tail.saturating_add(display_base)
            .saturating_sub(SHARED.playpos_ns.load(Relaxed).max(0))
            .max(0)
            / 1_000_000,
    )
}

/// The measured period of LG's native position callback, in milliseconds.
///
/// **Measured, not chosen.** The pipeline emits `PF_EVENT_TYPE_FRAMEREADY` at 5 Hz and the
/// device heartbeat reads `vtick=5 vgap=201ms` — unvarying across codec, raster and container,
/// on clean and on visibly stuttering playback alike (`player::sf_on_event`, measured
/// 2026-08-21). 201 is that measurement rather than the nominal 200.
const NATIVE_POSITION_PERIOD_MS: u32 = 201;

/// How many consecutive silent periods make the presentation clock physically STOPPED rather
/// than merely late.
///
/// Five, so the shortest silence this reports is 1005 ms. That is far outside anything
/// scheduling jitter can produce against a cadence measured to the millisecond, and it costs the
/// viewer about a second before a stalled stream can be acted on — against the 82 s a completing
/// fetch cost on the device (`pipe_abr_down_collapse`, 2026-09-02).
const NATIVE_CLOCK_SILENT_PERIODS: u32 = 5;

/// The silence that means the native clock has stopped: [`NATIVE_CLOCK_SILENT_PERIODS`] periods
/// of [`NATIVE_POSITION_PERIOD_MS`].
const NATIVE_CLOCK_STOPPED_MS: u32 = NATIVE_POSITION_PERIOD_MS * NATIVE_CLOCK_SILENT_PERIODS;

/// When the silence observation was last restarted, in [`super::vclock_ms`] milliseconds.
///
/// Every state that legitimately stops the native clock — user Pause, an internal rebuffer hold,
/// a stage below `Playing`, a route that is not segmented HLS — restarts it, because otherwise a
/// viewer who paused for a minute would resume straight into an internal hold. Main-thread only;
/// an atomic merely to avoid a `static mut`.
static CLOCK_WATCH_SINCE_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Everything that stops the clock on purpose restarts the silence observation with it.
fn restart_native_clock_watch() {
    CLOCK_WATCH_SINCE_MS.store(super::vclock_ms(), Relaxed);
}

/// Milliseconds since the native position callback last fired, or `None` if this seek/session
/// epoch has not seen one at all.
///
/// `dg_vpres_at` is zeroed wherever `frames` is — session reset and the post-seek re-count in
/// this file — so a seek starts with no observation rather than with a gap the size of the seek.
/// The [`CLOCK_WATCH_SINCE_MS`] floor does the same for the deliberate holds, which do not clear
/// the stamp.
fn native_clock_silence_ms() -> Option<u32> {
    let last_tick = SHARED.dg_vpres_at.load(Relaxed);
    if last_tick == 0 {
        return None;
    }
    let since = last_tick.max(CLOCK_WATCH_SINCE_MS.load(Relaxed));
    Some(super::vclock_ms().saturating_sub(since))
}

/// **The terminal reserve boundary as a PHYSICAL fact rather than an arithmetic one.**
///
/// `hls_buffered_ms` is content-time bookkeeping: the demuxed tail minus the playhead. It reaches
/// exactly zero only if the playhead reaches the tail, and on this pipeline it does not. The last
/// fed access units cannot be presented until the ones behind them arrive, so when acquisition
/// stops the position callback stops with the playhead parked a few hundred milliseconds short,
/// and that remnant then never moves again. Device-measured 2026-09-02
/// (`pipe_abr_down_collapse`): the callback went silent at `pos=29s` with the demuxed tail at
/// ~29.96 s and stayed silent for 77 s, so neither `buffered_ms == Some(0)` here nor the worker's
/// `spent >= reserve_ms_at_start` in [`crate::ff`]'s `StallGuard` could ever be reached, and a
/// 20000 kbps object ran to completion on a 500 kbps link before any rung could be decided.
///
/// The clock's own silence is the observation both of those were standing in for. If the clock
/// had been running through it, it would have presented `silence_ms` of media and said so five
/// times a second; a reserve no larger than the silence has therefore already been outlasted, and
/// `B = 0` is true within the only resolution the arithmetic can offer.
///
/// **The inequality is one-sided on purpose.** A large surviving reserve is not a starvation
/// boundary, so a stream holding 5.6 s whose callback is 1.2 s late keeps playing and only a
/// reserve the stopped clock has already outlasted counts. An unknowable reserve (`None`, the
/// audio lane silent after an open or a seek) holds nothing, exactly as it arms no `StallGuard`.
/// User Pause never reaches here — [`maybe_begin_hls_rebuffer`] returns before this on
/// `TX.paused` and restarts the observation — and an internal hold cannot re-arm, because the
/// hold sets `prime_play`, which returns at the same place.
fn native_clock_stopped(silence_ms: Option<u32>, buffered_ms: Option<i64>) -> bool {
    let (Some(silence_ms), Some(buffered_ms)) = (silence_ms, buffered_ms) else {
        return false;
    };
    silence_ms >= NATIVE_CLOCK_STOPPED_MS && buffered_ms <= i64::from(silence_ms)
}

fn should_begin_hls_rebuffer(
    requested: bool,
    _trial_reserve_ms: i64,
    _runtime_runway_ms: i64,
    buffered_ms: Option<i64>,
    native_clock_stopped: bool,
) -> bool {
    // `runtime_runway_ms` is a boundary requirement: it says how much reserve must be present
    // when a clock starts, not how much of the same acquisition remains at every later pump tick.
    // Comparing a decreasing mid-fetch B with the original full R makes a sustainable stream
    // pause shortly before virtually every segment lands. The reserve retained by a bounded
    // candidate is a boundary balance too: `exploration_budget_ms` gives the transaction only
    // B-max(R,D), and its deadline is the actuator that enforces that spend. Treating R as a live
    // main-thread stop arm duplicates the deadline and, on the device, latched seconds of stopped
    // playback after 136 ms of harmless measurement/clock skew (B=5167, R=5303).
    //
    // Only the in-flight response, which can see its current byte progress, may request an early
    // hold. Actual B=0 remains the coefficient-free terminal condition when no response byte has
    // arrived. Keep the boundary arguments in this pure function so the regression exercises the
    // exact values the pump samples and so the diagnostic line below can continue publishing them.
    // `native_clock_stopped` is the same coefficient-free `B = 0` observation, taken from the
    // pipeline's own cadence instead of from content-time arithmetic that cannot reach zero. See
    // its doc comment for why the arithmetic form is not merely imprecise here but unreachable.
    requested || buffered_ms == Some(0) || native_clock_stopped
}

fn rebuffer_request_is_current(
    requested: bool,
    requested_tail_ns: i64,
    current_tail_ns: i64,
) -> bool {
    requested && (requested_tail_ns < 0 || current_tail_ns <= requested_tail_ns)
}

/// Stop the INTERNAL A/V clock at a physical reserve boundary while leaving demux and feeding
/// alive. This is intentionally not `TX.paused`: the viewer did not pause, and the whole point is
/// to keep acquiring until `try_prime` sees enough measured runway to resume smoothly.
fn maybe_begin_hls_rebuffer(mt: &MainThread, eng: &mut Engine) {
    if !crate::route::is_segmented_hls()
        || eng.stage < Stage::Playing
        || eng.prime_play
        || TX.paused.load(Relaxed)
    {
        // Each of these legitimately stops the native clock, so none of them may be read later as
        // a stalled one: a viewer paused for a minute must resume into playback, not into an
        // instant internal hold, and the tick after this hold releases must start counting again.
        restart_native_clock_watch();
        return;
    }
    // Consume atomically: a worker request racing this tick must either be the value returned here
    // or remain set for the next tick, never be erased by a later unconditional store.
    let requested = SHARED.hls_rebuffer_requested.swap(false, Acquire);
    let requested_tail_ns = SHARED.hls_rebuffer_request_tail_ns.load(Acquire);
    let current_tail_ns = SHARED.hls_video_tail_ns.load(Acquire);
    let requested = rebuffer_request_is_current(requested, requested_tail_ns, current_tail_ns);
    let trial_reserve_ms = SHARED.hls_trial_reserve_ms.load(Acquire);
    let runtime_runway_ms = SHARED.hls_prime_runway_ms.load(Acquire);
    let buffered_ms = hls_buffered_ms();
    let silence_ms = native_clock_silence_ms();
    let clock_stopped = native_clock_stopped(silence_ms, buffered_ms);
    // A silent clock that does NOT become a hold is the case this gate was blind to for 82 s on
    // the device; say why, once per silent second, so a device log can answer it.
    if silence_ms.is_some_and(|ms| ms >= NATIVE_CLOCK_STOPPED_MS) && !clock_stopped {
        static LAST_SILENT_LINE_MS: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);
        let now = super::vclock_ms();
        if now.wrapping_sub(LAST_SILENT_LINE_MS.load(Relaxed)) >= 1_000 {
            LAST_SILENT_LINE_MS.store(now, Relaxed);
            super::log(&format!(
                "hls: clock silent={}ms buf={}ms vtail={} atail={} disp_base={} pos={} \
                 requested={} stage_playing=1",
                silence_ms.unwrap_or(0),
                buffered_ms.map_or(-1, |ms| ms),
                SHARED.hls_video_tail_ns.load(Acquire),
                SHARED.hls_audio_tail_ns.load(Acquire),
                SHARED.disp_base.load(Relaxed),
                SHARED.playpos_ns.load(Relaxed),
                requested,
            ));
        }
    }
    if !should_begin_hls_rebuffer(
        requested,
        trial_reserve_ms,
        runtime_runway_ms,
        buffered_ms,
        clock_stopped,
    ) {
        return;
    }
    let Some(pause_token) = SHARED.prepare_hls_rebuffer_pause() else {
        if requested {
            SHARED.hls_rebuffer_requested.store(true, Release);
        }
        super::log("hls: auto-rebuffer pause deferred by clock transition; will retry");
        return;
    };
    let paused = unsafe { ffi::sf_pause(mt) };
    match SHARED.complete_hls_rebuffer_pause(pause_token, paused != 0) {
        HlsPauseCompletion::Accepted => {}
        HlsPauseCompletion::Refused => {
            if requested {
                SHARED.hls_rebuffer_requested.store(true, Release);
            }
            super::log("hls: auto-rebuffer pause refused by Starfish; will retry");
            return;
        }
        HlsPauseCompletion::Stale => {
            if requested {
                SHARED.hls_rebuffer_requested.store(true, Release);
            }
            super::log("hls: auto-rebuffer pause result was stale; will retry");
            return;
        }
    }
    arm_live_clock_prime(eng);
    // The pause-local proof was armed before sf_pause, so a completed segment in the native-call
    // window is already part of this hold instead of being silently discarded.
    // `silent=` is the only field that separates the two ways this hold is reached: an exact
    // arithmetic zero, or a presentation clock that stopped with a remnant it will never spend.
    super::log(&format!(
        "hls: auto-rebuffer pause buf={}ms trial_reserve={}ms runway={}ms silent={}ms",
        buffered_ms.unwrap_or(-1),
        trial_reserve_ms,
        runtime_runway_ms,
        silence_ms.map_or(-1, i64::from),
    ));
}

/// Place the webOS 5+ exported window: source frame -> on-screen rect.
///
/// No-op on the ACB path. `src` is the frame being FED and `dst` where it lands, and the pair is
/// what expresses scaling — so a 4K direct play must not be described as 1080p, which passing the
/// authoring canvas for both would do. The coded size comes from the demuxer, the only thing that
/// knows it for certain; before it has published, the canvas is the least-wrong fallback and the
/// caller re-places once the real one arrives.
fn place_exported(mt: &MainThread, eng: &mut super::engine::Engine) {
    if ffi::vp_mode() != ffi::VP_EXPORTED {
        return;
    }
    let (w, h) = SHARED.video_raster();
    let src = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    let rv = unsafe { ffi::vp_place(mt, src.0, src.1, 0, 0, 1920, 1080) };
    eng.placed_src = src;
    // RECORD it, do not merely log it. These three fields had no writer anywhere in the tree, so
    // `dg_place_rv` sat at its `i32::MIN` "never called" sentinel for the life of the process and
    // the diagnostics read-out rendered `Placed: not placed` in DANGER bold on every webOS 5+ set
    // — a fabricated fault, on the one firmware family nobody here can test, pointing every reader
    // at the video plane. The dev TV takes the ACB path, so no amount of looking at it would have
    // caught this.
    SHARED.dg_place_rv.store(rv, Relaxed);
    SHARED.dg_placed_w.store(src.0, Relaxed);
    SHARED.dg_placed_h.store(src.1, Relaxed);
    super::log(&format!(
        "vplane: exported window placed src={}x{} rv={rv}",
        src.0, src.1
    ));
}

/// Mirror Engine-confined observations into `Shared` for diagnostics and handled-error context.
///
/// The read-out cannot call `engine(&MainThread)` itself — that hands out a `&'static mut` to a
/// `static mut`, and the draw runs inside a frame where the pump's borrow may still be live, so a
/// second one is instant UB. This is the only bridge, it is one-way, and nothing in the playback
/// state machine may read these fields back.
///
/// The stage is a cheap scalar and is always published so an opted-in terminal report does not
/// depend on whether Stats for Nerds happened to be open. `aq_bytes` takes each queue's pthread
/// mutex, which is why the rest is sampled HERE, once per visible diagnostics tick, and never from
/// a draw.
/// Last `frames` we saw, so a CHANGE can be stamped. A decrease counts: `frames` is seek-scoped
/// and the pump zeroes it applying a seek, which is motion, not a freeze.
static LAST_FRAMES: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

fn publish_diag(eng: &Engine, now: u32) {
    SHARED.dg_stage.store(eng.stage as u8, Relaxed);
    // Nobody is looking: skip it. `aq_bytes` takes each queue's pthread mutex, and the read-out
    // samples at 2 Hz, so publishing at 60 Hz is 30x more often than anything can observe. Costs
    // no freshness — the loop order is pump → stats::update → stats::draw, so the frame the panel
    // is switched on has already republished.
    if !crate::ui::stats::enabled() {
        return;
    }
    let qv = eng.aq_video.as_ref().map_or(0, |q| {
        crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64
    });
    let qa = eng.aq_audio.as_ref().map_or(0, |q| {
        crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64
    });
    SHARED.dg_aq_video.store(qv, Relaxed);
    SHARED.dg_aq_audio.store(qa, Relaxed);
    SHARED.dg_fed_v_pts.store(eng.max_fed_video_pts, Relaxed);
    SHARED.dg_fed_a_pts.store(eng.max_fed_audio_pts, Relaxed);
    // Stamp when the frame count MOVES. The panel needs "how long has it been stuck" and a
    // photograph has no time axis; stamping here rather than in `ui::stats` is what makes the
    // clock measure the STALL rather than how long the panel has been open.
    let f = SHARED.frames.load(Relaxed);
    if LAST_FRAMES.swap(f, Relaxed) != f {
        SHARED.dg_frame_at.store(now, Relaxed);
    }
}

/// The failed half of the deferred Original recovery: put the old route back and say where to
/// resume it, in nanoseconds. `None` means there was no recovery in flight, and the caller's
/// failure means what it always did.
///
/// The three failure gates below all route through here rather than one of them, because the three
/// ways a source can refuse to open are not distinguishable from the outside and all three cost
/// the same thing: `demux_io_failed` is the transfer dying, `demux_failed` with no frames is the
/// open being rejected, and `load_failed` is the pipeline refusing the payload the source implied.
/// The device case was the first; a 404 or a codec the set will not take are the other two.
enum OriginalRollbackPreparation {
    NotPending,
    RebaseFailed,
    Prepared(crate::route::OriginalRollback),
}

fn prepare_failed_original_rollback(status: i32) -> OriginalRollbackPreparation {
    let Some(rollback) = crate::route::rollback_original_recovery() else {
        return OriginalRollbackPreparation::NotPending;
    };
    let secs = rollback.offset_ns / 1_000_000_000;
    if status >= 400 {
        super::note_original_failure(super::ABR_FAILURE_ORIGINAL_HTTP, status);
    } else {
        // HTTP 200 followed by demux/Load failure is meaningfully different from no response.
        super::note_original_failure(super::ABR_FAILURE_ORIGINAL_OPEN, status);
    }
    // The held rollback resource is still alive, but its old start URL begins at the boundary
    // where that worker was created. Register a fresh physical HLS session at the recovery
    // position before reloading it; reopening the saved URL pairs a new display base with old media
    // and makes the picture jump backwards while the clock claims it did not.
    if crate::route::transcode_seek(secs).is_none() {
        super::log("abr: restored HLS encoder but could not rebase it to the recovery position");
        return OriginalRollbackPreparation::RebaseFailed;
    }
    super::report::note_delivery_requested_for(
        crate::route::playback_trace_generation(),
        super::report::DeliveryClass::Hls,
        super::report::QualityClass::Unknown,
        super::report::DeliveryReason::OriginalOpenRollback,
    );
    OriginalRollbackPreparation::Prepared(rollback)
}

fn recover_from_failed_original() -> Option<crate::route::OriginalRollback> {
    // Capture before `reload_transcode` clears the engine-scoped HTTP mirror. This sticky pair is
    // the reason a successful HLS rollback can still explain on screen why Original was refused.
    let status = SHARED.dg_http_status.load(Relaxed);
    match prepare_failed_original_rollback(status) {
        OriginalRollbackPreparation::Prepared(rollback) => Some(rollback),
        OriginalRollbackPreparation::NotPending | OriginalRollbackPreparation::RebaseFailed => None,
    }
}

/// Foreground can observe a synchronous construction failure before an Engine exists for
/// [`pump`] to inspect. Take the same typed Original -> restored-HLS edge used by asynchronous
/// open failures and return the exact replacement Load token to the foreground lifecycle.
pub(crate) enum ForegroundOriginalRecovery {
    NotOriginal,
    Tracking(crate::route::RouteStartAttempt),
    /// PMS preparation succeeded and the HLS candidate remains retryable, but native Engine
    /// construction failed before an asynchronous Load could be observed.
    RetryPrepared,
    /// Rollback consumed the Original snapshot but could not rebuild/rebase a truthful HLS route.
    Terminal,
}

pub(crate) fn recover_failed_foreground_original(mt: &MainThread) -> ForegroundOriginalRecovery {
    // No HTTP request necessarily happened: do not inherit the previous Engine's sticky status.
    let rollback = match prepare_failed_original_rollback(0) {
        OriginalRollbackPreparation::NotPending => {
            return ForegroundOriginalRecovery::NotOriginal;
        }
        OriginalRollbackPreparation::RebaseFailed => {
            set_state(super::shared::PlaybackState::Error);
            return ForegroundOriginalRecovery::Terminal;
        }
        OriginalRollbackPreparation::Prepared(rollback) => rollback,
    };
    match super::engine::reload_transcode_tracked(mt, rollback.offset_ns) {
        super::engine::BufferfeedStartOutcome::Launched(attempt) => {
            ForegroundOriginalRecovery::Tracking(attempt)
        }
        super::engine::BufferfeedStartOutcome::Failed => ForegroundOriginalRecovery::RetryPrepared,
        super::engine::BufferfeedStartOutcome::AlreadyRunning => {
            set_state(super::shared::PlaybackState::Error);
            ForegroundOriginalRecovery::Terminal
        }
    }
}

/// Which recovery owns a source/pipeline open failure. Pure so the ordering cannot regress while
/// the device-only reload calls remain outside host tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenFailureAction {
    /// HLS→Original had a live encoder held specifically for this outcome.
    RollbackPendingOriginal,
    /// Cold Auto Original never produced a frame; build the HLS contingency bootstrap retained.
    StartAutoHls,
    /// Manual Original, a post-frame interruption, or no viable route.
    Error,
}

fn open_failure_action(
    pending_original: bool,
    auto_original: bool,
    no_frames: bool,
) -> OpenFailureAction {
    if pending_original {
        OpenFailureAction::RollbackPendingOriginal
    } else if auto_original && no_frames {
        OpenFailureAction::StartAutoHls
    } else {
        OpenFailureAction::Error
    }
}

/// Recover either kind of failed Original open and return the reload position in nanoseconds.
/// Pending HLS rollback has priority; a failed rollback is terminal rather than silently starting
/// a third transaction on route state whose encoder restore already failed.
fn recover_failed_source_route() -> Option<crate::route::OriginalRollback> {
    let action = open_failure_action(
        crate::route::original_recovery_pending(),
        crate::route::auto_original_watch().is_some(),
        SHARED.frames.load(Relaxed) == 0,
    );
    match action {
        OpenFailureAction::RollbackPendingOriginal => recover_from_failed_original(),
        OpenFailureAction::StartAutoHls => {
            let status = SHARED.dg_http_status.load(Relaxed);
            if status >= 400 {
                super::note_original_failure(super::ABR_FAILURE_ORIGINAL_HTTP, status);
            } else {
                super::note_original_failure(super::ABR_FAILURE_ORIGINAL_OPEN, status);
            }
            let secs = (SHARED.playpos_ns.load(Relaxed) / 1_000_000_000).max(0);
            crate::route::fallback_unopened_auto_to_hls(secs)?;
            Some(crate::route::OriginalRollback::without_deferred(
                secs * 1_000_000_000,
            ))
        }
        OpenFailureAction::Error => None,
    }
}

/// Settle a synchronous native reload effect.  A failed `start_bufferfeed` used to be only a log:
/// callers had already advanced the route reducer and then returned, leaving no Engine to publish
/// the failure and (for Original) an `OriginalTrial` which could never receive its first frame.
fn settle_reload(outcome: super::engine::ReloadOutcome, operation: &str) -> bool {
    if outcome == super::engine::ReloadOutcome::Started {
        return true;
    }
    super::log(&format!(
        "route transition: {operation} failed synchronously ({outcome:?})",
    ));
    set_state(super::shared::PlaybackState::Error);
    false
}

/// Start the native half of an HLS→Original transaction.  The reducer already owns a complete HLS
/// rollback snapshot; a synchronous Load construction failure is the same typed rejection as an
/// HTTP/demux/Load callback failure and must cross that rollback edge immediately.
fn start_original_trial_reload(
    mt: &MainThread,
    reload: crate::route::AutoOriginalReload,
    position_ns: i64,
) -> bool {
    let outcome = match reload {
        crate::route::AutoOriginalReload::Direct => super::engine::reload_at(mt, position_ns),
        crate::route::AutoOriginalReload::Remux => super::engine::reload_transcode(mt, position_ns),
    };
    if outcome == super::engine::ReloadOutcome::Started {
        return true;
    }
    super::log(&format!(
        "abr: Original trial could not start ({outcome:?}); taking explicit rollback edge",
    ));
    let Some(rollback) = recover_from_failed_original() else {
        set_state(super::shared::PlaybackState::Error);
        return false;
    };
    let restored = super::engine::reload_transcode(mt, rollback.offset_ns);
    if restored == super::engine::ReloadOutcome::Started {
        true
    } else {
        super::log(&format!("abr: HLS rollback could not start ({restored:?})",));
        set_state(super::shared::PlaybackState::Error);
        false
    }
}

pub(crate) fn pump(mt: &MainThread, now: u32) {
    use super::shared::PlaybackState;
    let eng = match engine(mt) {
        Some(e) => e,
        None => {
            set_state(PlaybackState::Idle);
            return;
        }
    };
    // Republish the Engine-confined values the diagnostics read-out needs, BEFORE the early
    // returns below. Placed here, immediately after the borrow, for two reasons: it is the one
    // spot that cannot be forgotten as the pump grows, and it therefore keeps reporting through
    // the `sf_ready` and producer-died bail-outs — which is precisely when a stalled user is
    // looking at the panel. See `Shared`'s diagnostics-mirror block for the one-way rule.
    publish_diag(eng, now);
    // The media thread may have returned from sf_load immediately, but it is not allowed to make
    // the route Stable before this Engine exists in the main-thread slot. Drain its exact-token
    // result here, after installation and before any worker publication can be accepted.
    crate::route::drain_route_start_results();
    // `sf_load == 0` may leave no callable object, so this must precede the sf_ready wait below;
    // otherwise the pump returns Connecting forever and never consumes the explicit failure.
    if SHARED.load_failed.load(Acquire) {
        if let Some(rollback) = recover_failed_source_route() {
            let started = settle_reload(
                super::engine::reload_transcode(mt, rollback.offset_ns),
                "HLS rollback after native Load failure",
            );
            let _ = started;
            return;
        }
        crate::route::fail_current_engine();
        set_state(PlaybackState::Error);
        return;
    }
    // wait for the media-thread ctor
    if unsafe { ffi::sf_ready(mt) } == 0 {
        set_state(PlaybackState::Connecting);
        return;
    }
    // ---------- the Original recovery, once it has to answer for itself ----------
    //
    // `recover_auto_to_original` deferred two irreversible things — the server-side stop and the
    // old route — because the probes that authorised the switch are a claim about a byte range
    // fetched seconds ago, not about the fresh open the reload above just performed. Here is where
    // that open is graded.
    //
    // **Decoded frames, not `loadCompleted`.** A completed Load is the pipeline accepting a
    // PAYLOAD DECLARATION; it says nothing about the source delivering. `frames` is zeroed by
    // `teardown(for_reload=true)` on the way into this reload (`SHARED::reset_session`), so a
    // non-zero count here belongs to the new source and to nothing else.
    if crate::route::original_recovery_pending() && SHARED.frames.load(Relaxed) > 0 {
        crate::route::confirm_original_recovery();
    }
    if SHARED.frames.load(Relaxed) > 0 {
        crate::route::confirm_resume_presented();
    }

    // The producer died before publishing a duration: the EOS path is gated on `duration_ns > 0`
    // so it can NEVER fire, and the player used to sit on a black screen forever with no error
    // and no exit. Surface it instead — BACK is already the escape.
    //
    // **Unless there is a way back**, which is the whole of the deferral above. Device,
    // 2026-08-29: the viewer asked for Original by hand on a film that was playing, the source URL
    // failed the way the same server's Original probe had failed forty seconds earlier, and this
    // line raised the failure read-out on a stream that had been working. A failed recovery is
    // worth a second reload; it is not worth the playback.
    // A worker may publish a more specific terminal code immediately before this generic Release
    // flag. Acquire is the matching hand-off; `error_now` can then report the transaction cause
    // instead of racing it into the generic producer bucket.
    if SHARED.demux_io_failed.load(Acquire) {
        if let Some(rollback) = recover_failed_source_route() {
            let started = settle_reload(
                super::engine::reload_transcode(mt, rollback.offset_ns),
                "HLS rollback after source I/O failure",
            );
            let _ = started;
            return;
        }
        crate::route::fail_current_engine();
        set_state(PlaybackState::Error);
        return;
    }
    if SHARED.demux_failed.load(Acquire) && SHARED.frames.load(Relaxed) == 0 {
        if let Some(rollback) = recover_failed_source_route() {
            let started = settle_reload(
                super::engine::reload_transcode(mt, rollback.offset_ns),
                "HLS rollback after demux failure",
            );
            let _ = started;
            return;
        }
        crate::route::fail_current_engine();
        set_state(PlaybackState::Error);
        return;
    }
    let stream = matches!(eng.source, Source::Stream);

    // ---------- one synchronized route/action state machine ----------
    //
    // User quality/track changes survive pre-roll and outrank automatic ABR. A simultaneous seek
    // is not a competing mailbox: it supplies this action's target, so the two become ONE reload
    // instead of either resetting the seek or rebuilding at the position the viewer already left.
    if stream && eng.stage >= Stage::Playing && !(eng.flushed && eng.rebase_pending) {
        if let Some(action) = crate::route::claim_route_action() {
            let pending_seek = TX.seek_to_ns.load(Relaxed);
            let current_pos = SHARED.playpos_ns.load(Relaxed).max(0);
            let user_target = if pending_seek >= 0 {
                pending_seek
            } else {
                current_pos
            };
            match action.intent.clone() {
                crate::route::RouteIntent::User(intent) => match intent {
                    crate::route::UserRouteIntent::Retranscode => {
                        let secs = user_target / 1_000_000_000;
                        if crate::route::retranscode_for(&action.ticket, secs).is_some() {
                            crate::route::finish_route_action(
                                &action,
                                crate::route::RouteApplyResult::Prepared,
                            );
                            if pending_seek >= 0 {
                                crate::route::commit_user_seek();
                            }
                            super::log(&format!(
                                "route transition: user retranscode at {secs}s{}",
                                if pending_seek >= 0 { " + seek" } else { "" },
                            ));
                            settle_reload(
                                super::engine::reload_transcode(mt, user_target),
                                "user retranscode reload",
                            );
                            return;
                        }
                        crate::route::finish_route_action(
                            &action,
                            crate::route::RouteApplyResult::Rejected,
                        );
                        super::log("route transition: user retranscode was rejected; current stream retained");
                    }
                    crate::route::UserRouteIntent::NativeAudioReload => {
                        let idx = SHARED.desired_audio_idx.load(Relaxed);
                        crate::route::finish_route_action(
                            &action,
                            crate::route::RouteApplyResult::Prepared,
                        );
                        if pending_seek >= 0 {
                            crate::route::commit_user_seek();
                        }
                        super::log(&format!(
                            "route transition: native audio idx={idx} at {}s{}",
                            user_target / 1_000_000_000,
                            if pending_seek >= 0 { " + seek" } else { "" },
                        ));
                        settle_reload(
                            super::engine::switch_audio_native(mt, idx, user_target),
                            "native audio reload",
                        );
                        return;
                    }
                    crate::route::UserRouteIntent::AdaptiveReload => {
                        if crate::route::is_transcoding() {
                            let secs = user_target / 1_000_000_000;
                            if crate::route::transcode_seek(secs).is_some() {
                                crate::route::finish_route_action(
                                    &action,
                                    crate::route::RouteApplyResult::Prepared,
                                );
                                if pending_seek >= 0 {
                                    crate::route::commit_user_seek();
                                }
                                super::log(&format!(
                                    "route transition: adaptive worker reload at {secs}s{}",
                                    if pending_seek >= 0 { " + seek" } else { "" },
                                ));
                                settle_reload(
                                    super::engine::reload_transcode(mt, user_target),
                                    "adaptive HLS reload",
                                );
                                return;
                            }
                            crate::route::finish_route_action(
                                &action,
                                crate::route::RouteApplyResult::Rejected,
                            );
                            super::log("route transition: adaptive transcode reload was rejected");
                        } else {
                            crate::route::finish_route_action(
                                &action,
                                crate::route::RouteApplyResult::Prepared,
                            );
                            if pending_seek >= 0 {
                                crate::route::commit_user_seek();
                            }
                            super::log(&format!(
                                "route transition: adaptive direct reload at {}s{}",
                                user_target / 1_000_000_000,
                                if pending_seek >= 0 { " + seek" } else { "" },
                            ));
                            settle_reload(
                                super::engine::reload_at(mt, user_target),
                                "adaptive direct reload",
                            );
                            return;
                        }
                    }
                    crate::route::UserRouteIntent::RecoverOriginal => {
                        let secs = user_target / 1_000_000_000;
                        match crate::route::recover_auto_to_original_for(
                            &action.ticket,
                            secs,
                            false,
                        ) {
                            Some(reload) => {
                                crate::route::commit_user_seek();
                                start_original_trial_reload(mt, reload, user_target);
                                return;
                            }
                            None => {
                                crate::route::finish_route_action(
                                    &action,
                                    crate::route::RouteApplyResult::Rejected,
                                );
                                super::log("route transition: manual Original was rejected; current stream retained");
                            }
                        }
                    }
                },
                crate::route::RouteIntent::Automatic(intent) => {
                    match intent {
                        crate::route::AutomaticRouteIntent::OriginalToHls {
                            ticket,
                            conservative_kbps,
                            position_ns,
                        } => {
                            let position_ns = position_ns.max(0);
                            let secs = position_ns / 1_000_000_000;
                            if ticket != action.ticket {
                                crate::route::finish_route_action(
                                    &action,
                                    crate::route::RouteApplyResult::Cancelled,
                                );
                                super::log("auto: discarded fallback action whose ticket changed before claim");
                                return;
                            }
                            if crate::route::fallback_auto_to_hls_for(
                                &action.ticket,
                                conservative_kbps,
                                secs,
                            )
                            .is_some()
                            {
                                crate::route::finish_route_action(
                                    &action,
                                    crate::route::RouteApplyResult::Prepared,
                                );
                                crate::route::commit_user_seek();
                                settle_reload(
                                    super::engine::reload_transcode(mt, position_ns),
                                    "automatic Original-to-HLS reload",
                                );
                                return;
                            }
                            crate::route::finish_route_action(
                                &action,
                                crate::route::RouteApplyResult::Rejected,
                            );
                            super::log("auto: synchronized Original fallback could not build HLS");
                            SHARED.demux_io_failed.store(true, Relaxed);
                        }
                        crate::route::AutomaticRouteIntent::HlsToOriginal {
                            ticket,
                            evidence_kbps: _,
                            position_ns,
                        } => {
                            let position_ns = position_ns.max(0);
                            let secs = position_ns / 1_000_000_000;
                            if ticket != action.ticket {
                                crate::route::finish_route_action(
                                    &action,
                                    crate::route::RouteApplyResult::Cancelled,
                                );
                                super::log("auto: discarded Original action whose ticket changed before claim");
                                return;
                            }
                            match crate::route::recover_auto_to_original_for(
                                &action.ticket,
                                secs,
                                true,
                            ) {
                                Some(reload) => {
                                    crate::route::commit_user_seek();
                                    start_original_trial_reload(mt, reload, position_ns);
                                    return;
                                }
                                None => {
                                    crate::route::finish_route_action(
                                        &action,
                                        crate::route::RouteApplyResult::Rejected,
                                    );
                                    // The HLS worker stopped only to hand this action to the main
                                    // thread. A refused/lost Original decision leaves its route fully
                                    // intact, so rebuild that exact HLS Engine instead of converting a
                                    // recoverable probe failure into the terminal playback screen.
                                    super::log(
                                        "auto: Original recovery was rejected; reopening retained HLS",
                                    );
                                    crate::route::commit_user_seek();
                                    settle_reload(
                                        super::engine::reload_transcode(mt, position_ns),
                                        "retained HLS reopen after rejected Original",
                                    );
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // ---------- an in-place seek is still resolving (flushed, not yet re-anchored): COALESCE.
    // Firing another now would race the demux reopen (rapid tap-seeks) and the rebase guard would
    // then eat everything → stuck. Hold the latest requested target (it stays in TX.seek_to_ns) and
    // process it once this seek anchors. If it hasn't anchored within SEEK_STUCK_MS the reopen was
    // lost — first RETRY the reopen cheaply (re-interrupt the demuxer + re-arm), and only fall back
    // to a full (slow) reload if the retries also fail.
    // SEEK_STUCK_MS must comfortably exceed a real reopen+av_seek+first-keyframe on 4K HEVC over
    // PMS (~700ms): the retry closes the demux socket, so a premature watchdog KILLS an open that
    // was about to succeed and restarts it — at 500ms a rapid tap-burst self-DoS'd into the
    // reload fallback every time (caught by the seek_rapid harness cases).
    const SEEK_STUCK_MS: u32 = 1200;
    if eng.flushed && eng.rebase_pending && now.wrapping_sub(eng.seek_armed_at) > SEEK_STUCK_MS {
        // Adopt the NEWEST coalesced target if later taps landed while this seek was resolving
        // (TX.seek_to_ns holds the latest request): retrying/reloading at the original armed
        // target would land where the user already tapped away from, then seek AGAIN.
        let pending = TX.seek_to_ns.swap(-1, Relaxed);
        let tgt = if pending >= 0 {
            pending
        } else {
            SHARED.seek_target_ns.load(Relaxed)
        }
        .max(0);
        if pending >= 0 {
            // The coalesced tap is a distinct desired timeline. It used to replace the demux
            // target below without crossing the reducer boundary, leaving `pending_seek_ns`
            // permanently Busy and old ABR evidence authorized against the new bytes.
            crate::route::commit_user_seek();
        }
        if eng.seek_retries < 2 {
            eng.seek_retries += 1;
            SHARED.seek_to_ns.store(tgt, Release); // re-publish; the demux thread seeks on it
            SHARED.seek_target_ns.store(tgt, Relaxed); // rebase guard keys on the retried target
            eng.seek_armed_at = now;
            super::log(&format!(
                "seek: in-place stuck → retry reopen at {}s (#{}) coalesced={}",
                tgt / 1_000_000_000,
                eng.seek_retries,
                take_coalesced()
            ));
        } else {
            super::log(&format!(
                "seek: in-place stuck → reload at {}s",
                tgt / 1_000_000_000
            ));
            settle_reload(
                super::engine::reload_at(mt, tgt),
                "stuck in-place seek reload",
            ); // REPLACES the engine — eng dangles, return
            return;
        }
    }
    // ---------- pending seek: flush, drop queued AUs, re-point the demux ----------
    let t = TX.seek_to_ns.load(Relaxed);
    // A live transcode has NO Content-Length (file_size stays -1), so gate it on duration
    // only and seek by RESTARTING the transcode at a time &offset (below). Direct-play
    // seeks in place via av_seek.
    let is_transcode = crate::route::is_transcoding();
    if stream
        && t >= 0
        && !crate::route::original_recovery_pending()
        && !(eng.flushed && eng.rebase_pending) // coalesce: don't stack in-place seeks
        && eng.stage >= Stage::Playing
        && SHARED.duration_ns.load(Relaxed) > 0
        && (is_transcode || SHARED.file_size.load(Relaxed) > 0)
    {
        TX.seek_to_ns.store(-1, Relaxed);
        let coalesced = take_coalesced(); // read here so BOTH branches consume this seek's requests
        let t = t.max(0);
        // TRANSCODE seek: restart the encode at the new &offset and do a FULL RELOAD (fresh Load).
        // A flush()+refeed of the new start.mkv left a STALE GStreamer segment → visual artifacts on
        // the jump (the same class of bug the in-place seek cured for direct-play; a transcode can't
        // use the in-place path — the stream content itself changes at the new offset). A fresh Load
        // rebuilds the segment by construction. reload_transcode REPLACES the ENGINE, so `eng`
        // dangles after it — return immediately and let the next pump() tick drive the fresh engine.
        if is_transcode {
            let secs = t / 1_000_000_000;
            if crate::route::transcode_seek(secs).is_some() {
                crate::route::commit_user_seek();
                settle_reload(
                    super::engine::reload_transcode(mt, t),
                    "transcode seek reload",
                );
            } else {
                // Give up on THIS seek and say so, or the spinner and the frozen playhead outlive
                // the playback — see `player::abandon_seek`. The engine is untouched here (no
                // flush has happened), so the stream itself carries on from where it was.
                super::abandon_seek();
                super::log("seek(transcode): rebuild failed");
            }
            return;
        }
        // DIRECT-PLAY seek: Kodi IN-PLACE seek — flush + av_seek the demuxer, and
        // (in feed_stream, on the first post-flush keyframe) setTimeToDecode + sendSegmentEvent
        // to re-anchor the GStreamer segment WITHOUT a reload/decoder re-init (no A/V-resync
        // glitch). A plain flush()+refeed with NO fresh segment left a STALE GStreamer segment
        // → the HW sink stops draining ~48 s later → permanent BufferFull + "Playing error"
        // (root-caused by decompiling libpipeline/lxvideosink; see engine::reload_at). Falls
        // back to reload-per-seek if the pipeline isn't reachable (INPLACE_SEEK_OK cleared by
        // feed_stream when sendSegmentEvent can't find it). reload_at REPLACES the ENGINE, so
        // `eng` dangles after it: return immediately.
        if !super::INPLACE_SEEK_OK.load(Relaxed) {
            crate::route::commit_user_seek();
            settle_reload(super::engine::reload_at(mt, t), "direct seek reload");
            return;
        }
        // Pause, user-held seek reuse and eventual prime all share one actuator state. In
        // particular a seek from Paused must not issue a redundant native Pause, and a result from
        // an older transition must not authorize flushing this session's queues.
        match SHARED.prepare_seek_pause() {
            Some(HlsSeekPause::AlreadyHeld) => {}
            Some(HlsSeekPause::Issue(token)) => {
                let accepted = unsafe { ffi::sf_pause(mt) } != 0;
                match SHARED.complete_seek_pause(token, accepted) {
                    HlsPauseCompletion::Accepted => {}
                    HlsPauseCompletion::Refused => {
                        super::log("seek(in-place): Starfish refused Pause → reload fallback");
                        crate::route::commit_user_seek();
                        settle_reload(
                            super::engine::reload_at(mt, t),
                            "seek reload after native Pause refusal",
                        );
                        return;
                    }
                    HlsPauseCompletion::Stale => {
                        super::log("seek(in-place): stale Pause result → reload fallback");
                        crate::route::commit_user_seek();
                        settle_reload(
                            super::engine::reload_at(mt, t),
                            "seek reload after stale Pause",
                        );
                        return;
                    }
                }
            }
            None => {
                // The main-thread transition in flight will settle before the next pump tick. Keep
                // the newest seek mailbox intact rather than consuming it into no operation.
                TX.seek_to_ns.store(t, Release);
                TX.seek_reqs.store(1, Release);
                super::log("seek(in-place): clock transition busy; retrying");
                return;
            }
        }
        if !SHARED.begin_native_media_discontinuity(eng.native_epoch) {
            crate::route::commit_user_seek();
            super::log("seek(in-place): native callback epoch retired → reload fallback");
            settle_reload(
                super::engine::reload_at(mt, t),
                "seek reload after retired native epoch",
            );
            return;
        }
        // This is the applied media boundary: everything after it belongs to the new playhead.
        // Advancing earlier would strand the live worker when PMS/native refused; advancing later
        // would let pre-seek ABR evidence publish while queues are already being reset.
        crate::route::commit_user_seek();
        unsafe {
            ffi::sf_flush(mt); // drop decoded/queued frames only after the clock is really held
        }
        drain_aq(eng);
        // in-place direct-play seek: publish the target and let the DEMUX THREAD av_seek_frame
        // between two reads (ff.rs's inner loop). Nothing here interrupts the socket: the pump
        // used to shutdown(2) it to break the read so the outer loop would reopen+seek, but our
        // AVIO is seekable, so libavformat just healed the broken read through `seek_cb` and read
        // on — no reopen, no seek, and every direct-play seek escalated to a reload. See ff.rs.
        // eng.flushed → feed_stream fires setTimeToDecode+sendSegmentEvent on the first post-seek
        // keyframe. disp_base=0 (rebase to the landed keyframe).
        eng.flushed = true;
        eng.presentation_rearm_pending = false;
        SHARED.seek_to_ns.store(t, Release); // the demux thread's av_seek target
        SHARED.seek_target_ns.store(t, Relaxed); // rebase guard: reject stale drifted keyframes
        SHARED.disp_base.store(0, Relaxed);
        super::log(&format!(
            "seek(in-place): av_seek t={t} coalesced={coalesced}"
        ));
        // zero-base the fed timeline on the first post-seek keyframe (feed_stream), so it
        // presents against the flush-reset clock immediately — no catch-up freeze
        eng.rebase_pending = true;
        eng.rebase_drops = 0;
        eng.seek_armed_at = now; // stuck-watchdog start (see the coalesce/escape above)
        eng.seek_retries = 0;
        eng.max_fed_video_pts = 0;
        eng.max_fed_audio_pts = 0;
        // Drop any AU held from BEFORE the seek (the per-lane BufferFull retry). drain_aq cleared the
        // QUEUES but not these pending AUs; feeding a stale pre-seek AU after the seek re-bases it to
        // the NEW pts_shift → a big A/V desync — the stale audio pts advances max_fed_audio_pts, then
        // the STALE_BACKJUMP guard drops the fresh post-seek audio as "too far back" → no sound. The
        // reopened stream supplies fresh post-seek AUs.
        eng.pending_video = None;
        eng.pending_audio = None;
        eng.prime_play = true; // paused above; Play once PRIME_NS is buffered (no fast-forward)
                               // "no post-seek frame presented yet" — the feed-ahead throttle feeds freely until the first
                               // real presented pts lands, instead of comparing the new fed pts against the STALE pre-seek
                               // presented position (which would wrongly break feeding on a forward in-place seek).
        SHARED.pres_fed.store(super::engine::PRES_NONE, Relaxed);
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (rebind + resume re-pause gate)
                                         // …and forget the last presentation stamp with it. The plane goes dark across a seek by
                                         // design, so the first frame after one would otherwise post a gap the size of the seek and
                                         // read as a stutter (`Shared`'s `dg_vpres_*` block).
        SHARED.dg_vpres_at.store(0, Relaxed);
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
    }

    // ---------- load -> Play (decode fed frames as soon as loaded) ----------
    if eng.stage == Stage::Loading
        && (SHARED.load_completed.load(Relaxed) || unsafe { ffi::sf_is_load_completed(mt) } != 0)
    {
        SHARED.load_completed.store(true, Relaxed);
        // when it completed, so the panel can say how long we have been waiting for a frame since
        SHARED.dg_load_at.store(now, Relaxed);
        super::log("SMP loadCompleted");
        eng.stage = Stage::Playing;
        // webOS 5+: place the exported window HERE, the instant Load reports success — which is
        // exactly where ss4s does it (smp_player.c calls StarfishResourcePostLoad synchronously
        // on the line after StarfishMediaAPIs_load returns true).
        //
        // It used to wait for `frames >= 2`, copied from the ACB block below, and that is a
        // DEADLOCK on this path: the pipeline does not emit frames until its sink is bound, and
        // the sink is not placed until frames arrive. The symptom is a player stuck in buffering
        // forever, which is what webOS 6 and 10 reported. The gate is right for ACB and only for
        // ACB — `setMediaVideoData` there needs the sourceInfo envelope, which cannot exist until
        // the pipeline has produced something. The exported window has no such dependency: the
        // BINDING already happened via `option.windowId` in the Load payload, and this call is
        // pure geometry.
        place_exported(mt, eng);
        if ffi::vp_mode() == ffi::VP_EXPORTED {
            // Playing -> Streaming directly: there is no bind sequence to sit in the middle of.
            // The transition is load-bearing beyond bookkeeping — `pushEOS` is gated on
            // `>= Streaming`, so without it the last frames never drain and Up Next never fires.
            eng.stage = Stage::Streaming;
        }
        // A fresh Load for a seek/resume primes before Play so the clock does not free-run through
        // the demux reopen gap. Segmented HLS needs the same gate even at offset zero: its video
        // and AAC arrive through independent queues, and starting Starfish's audio-master clock
        // before AAC is present produced silent initial playback that recovered only after a seek
        // (the seek path already primed both lanes). Progressive initial play keeps its proven
        // immediate-start behavior.
        if prime_before_play(eng.rebase_pending, crate::route::is_segmented_hls()) {
            eng.prime_play = true;
            super::log("SMP loadCompleted (priming before Play)");
        } else {
            let generation = SHARED.hls_candidate_generation.load(Acquire);
            let recovery = SHARED.hls_recovery();
            if let Some((token, _)) =
                SHARED.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, recovery)
            {
                let accepted = unsafe { ffi::sf_play(mt) } != 0;
                match SHARED.complete_hls_prime_play(token, accepted) {
                    super::shared::HlsPlayCompletion::Accepted { resume_acb } => {
                        if resume_acb {
                            super::acb_mirror_playstate(mt, true);
                        }
                        super::log("SMP Play");
                    }
                    super::shared::HlsPlayCompletion::Refused => {
                        eng.prime_play = true;
                        super::log("SMP Play refused; priming for retry");
                    }
                    super::shared::HlsPlayCompletion::Stale => {
                        eng.prime_play = true;
                        super::log("SMP Play result stale; priming for retry");
                    }
                }
            } else {
                eng.prime_play = true;
                super::log("SMP Play fenced; priming before retry");
            }
        }
    }

    // ---------- ACB bind, Kodi/ss4s order: setSinkType(MAIN)+setMediaId+setState(LOADED) ----------
    if eng.stage == Stage::Playing && ACB_OK.load(Relaxed) {
        let id = SHARED.media_id.lock().unwrap().clone();
        if let Some(id) = id {
            unsafe { ffi::acb_bind(mt, id.as_ptr()) };
            super::log(&format!("SMP ACB bound id={}", id.to_string_lossy()));
            // **Dolby Atmos, to ACB, exactly here** — this is where LG's own client fires it
            // (`libcbe` 0x1b98d78, on LOADCOMPLETED): after setMediaId, and BEFORE the
            // frame-gated setMediaVideoData below. It needs no decoded frame and reads no
            // state; `context` is the `id` we have in hand, which is the whole reason the call
            // exists rather than a forward of the pipeline's own AUDIO_INFO callback (that
            // string carries `track` and `dualMono` and no context). See `acb_send_atmos`.
            //
            // **This looks like it breaks the "never feed audio to ACB" rule and it does not** —
            // the rule is about the audio ELEMENTARY STREAM, which the pipeline owns and which we
            // still never hand ACB. This is a two-key metadata descriptor, and the distinction is
            // now readable rather than remembered (see `acb_send_atmos` in `src/starfish.c` for
            // the addresses). The rule's stated consequence, `SOUND_ERROR_019`, is a literal that
            // exists in NO library on this device.
            //
            // Ran behind `/tmp/plxnative-atmosacb` first, then measured, then made the default:
            // `rv=1` accepted, 1600 audio AUs fed with `reply=O` and no error of any kind, and the
            // television's own read-out — "Dolby Vision / Dolby Atmos", both lines — photographed
            // in a DISPLAY capture at 11 s. `/tmp/plxnative-noatmosacb` is the way back out.
            if crate::route::stream_immersive() && !crate::dev::flag("noatmosacb") {
                let rv = unsafe { ffi::acb_send_atmos(mt, id.as_ptr()) };
                super::log(&format!("atmos: acb setMediaAudioData rv={rv}"));
            }
            eng.stage = Stage::Bound;
        }
    }

    // ---------- send the WHOLE sourceInfo envelope VERBATIM, once frames flow, then
    // window + PLAYING (setMediaVideoData -> setDisplayWindow -> setState PLAYING) ----------
    if eng.stage == Stage::Bound && !eng.video_info_sent && SHARED.frames.load(Relaxed) >= 2 {
        let bytes = SHARED.source_info.lock().unwrap().clone();
        if let Some(bytes) = bytes {
            let rv = unsafe { ffi::acb_send_video_data(mt, bytes.as_ptr() as *const c_char) };
            super::log(&format!(
                "setMediaVideoData rv={rv} frames={}",
                SHARED.frames.load(Relaxed)
            ));
            if rv != -1 {
                // -1 = client-side isJsonError reject; else accepted
                eng.video_info_sent = true;
                unsafe { ffi::acb_start(mt, 0, 0, 1920, 1080) };
                eng.stage = Stage::Streaming;
                super::log("setMediaVideoData sent → window+PLAYING");
            }
        }
    }

    // ---------- webOS 5+ (VP_EXPORTED): correct the placement if the real frame size only
    // became known after Load. The demuxer publishes the coded size when it opens the stream,
    // which races the load thread — so the placement above may have used the fallback. Re-placing
    // costs one call, and Kodi re-places on every render-area change anyway.
    // ----------
    if eng.stage >= Stage::Playing && eng.placed_src != (0, 0) {
        let now = SHARED.video_raster();
        if now.0 > 0 && now.1 > 0 && now != eng.placed_src {
            place_exported(mt, eng);
        }
    }

    // ---------- feed AUs after the initial Load has reached Playing. The native clock may itself
    // be Running or Paused: runtime rebuffer and ResumePrime deliberately fill both lanes before
    // issuing Play. NOT while a seek is armed: on a resume the seek is armed before PLAYING, so
    // feeding first would present the file start for a frame before the seek repositions — a
    // visible jump. ----------
    maybe_begin_hls_rebuffer(mt, eng);
    if eng.stage >= Stage::Playing && TX.feed_allowed() && TX.seek_to_ns.load(Relaxed) < 0 {
        if stream {
            // Two-lane feed, then the prime attempt — the ordering and the reason both live in
            // `feed_both_lanes`, so this call site cannot drift from them.
            feed_both_lanes(mt, eng);
        } else {
            feed_sample(mt, eng);
        }
    }

    // ---------- end-of-stream: the producer hit file EOF (eos_pushed → all AUs to the end were
    // fed) and the pipeline has now played out to within EOS_TAIL of the duration. Mark ended so
    // app.rs tears the player down at the credits instead of freezing on the last frame. Paused
    // at the end stays paused (playpos won't climb) — correct; it fires when resumed. Any seek
    // clears the flag (request_seek), so seeking back from the end doesn't re-trigger. ----------
    // ---------- publish the derived UI state (see PlaybackState's doc) ----------
    // Order matters: a seek in flight outranks "we have frames", because the frames on the panel
    // are the PRE-seek ones and the HUD must show the target, not them.
    set_state(if SHARED.seeking.load(Relaxed) {
        PlaybackState::Seeking
    } else if eng.stage == Stage::Loading {
        PlaybackState::Connecting
    } else if SHARED.hls_rebuffering.load(Relaxed) || SHARED.frames.load(Relaxed) == 0 {
        PlaybackState::Buffering
    } else {
        PlaybackState::Playing
    });

    const EOS_TAIL_NS: i64 = 1_000_000_000;
    if eng.eos_pushed && !SHARED.ended.load(Relaxed) {
        let dur = SHARED.duration_ns.load(Relaxed);
        let pos = SHARED.playpos_ns.load(Relaxed);
        if dur > 0 && pos >= dur - EOS_TAIL_NS {
            SHARED.ended.store(true, Relaxed);
            super::log(&format!(
                "EOS reached: playpos={}s/{}s → ended",
                pos / 1_000_000_000,
                dur / 1_000_000_000
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        hls_buffered_ms, native_clock_silence_ms, native_clock_stopped, open_failure_action,
        prime_before_play, rebuffer_request_is_current, should_begin_hls_rebuffer,
        OpenFailureAction, CLOCK_WATCH_SINCE_MS, NATIVE_CLOCK_STOPPED_MS, SHARED,
    };
    use std::sync::atomic::Ordering::{Relaxed, Release};

    /// The device's measured position-callback period, so the replay below advances in the same
    /// steps the television reports in (`vtick=5 vgap=201ms`; `player::sf_on_event`).
    const POSITION_PERIOD_MS: u32 = 201;

    /// Put the three atomics `hls_buffered_ms` samples back where a fresh session leaves them, so
    /// a replay cannot leak a frozen playhead into another test holding the same serial lock.
    fn clear_hls_reserve_sample() {
        SHARED.hls_video_tail_ns.store(-1, Release);
        SHARED.hls_audio_tail_ns.store(-1, Release);
        SHARED.disp_base.store(0, Relaxed);
        SHARED.playpos_ns.store(0, Relaxed);
    }

    /// **Regression: `pipe_abr_down_collapse` on the television, 2026-09-02.** A 40000 kbps link
    /// collapsed to 500 kbps at t=25 s while a 20000 kbps rung was active. The controller's
    /// descent was correct once it could run, but it could not run for 82 s, and the case failed
    /// its `pos_climb` gate with 38 s of progress in a 120 s run.
    ///
    /// What stopped it is the terminal boundary being defined arithmetically. Segment 9 was
    /// demuxed to a video tail of ~29.96 s; the pipeline then stopped reporting a position
    /// altogether (`play=0pm vtick=0 vgap=0ms`, log line 248, holding `pos=29s` for the next
    /// 77 s) with the playhead parked SHORT of that tail, because the last fed access units
    /// cannot be presented until the ones behind them arrive. So `hls_buffered_ms` stuck at a
    /// small positive remnant forever, `should_begin_hls_rebuffer`'s `buffered_ms == Some(0)`
    /// never became true, no `hls: auto-rebuffer pause` was ever written, and the worker's
    /// `StallGuard` — which reads that same hold as `terminal_hold_started` — never abandoned the
    /// fetch. The 5.17 MB object therefore ran to completion on a 500 kbps link
    /// (`open_probe_ms=82431 total_ms=82682`, log line 323) before any rung decision existed.
    ///
    /// The physical fact the arithmetic cannot see is that the clock has STOPPED. This replays
    /// the numbers the log carries and asserts the hold begins about a second after it stops,
    /// not when the fetch completes.
    #[test]
    fn a_stopped_native_clock_reaches_the_terminal_boundary_the_arithmetic_never_will() {
        let _serial = crate::testlock::serial();
        // Segment 9 demuxed: 48 video AUs at 24 fps and 94 AAC frames, ending the tenth 2 s
        // segment of the stream (`hls: segment=9 ... v=48 a=94`, log line 235).
        SHARED.hls_video_tail_ns.store(29_958_000_000, Release);
        SHARED.hls_audio_tail_ns.store(29_980_000_000, Release);
        SHARED.disp_base.store(0, Relaxed);
        // The playhead then froze. The log prints whole seconds, so the exact remnant is not
        // recoverable from it; what the log does prove is that it never reached zero, because the
        // auto-rebuffer line that an exact zero writes was never written in those 77 s.
        SHARED.playpos_ns.store(29_700_000_000, Relaxed);
        let buffered_ms = hls_buffered_ms();
        // Clear the shared sample BEFORE the first assertion: `testlock::serial()` recovers a
        // poisoned lock, so a panic past this point would otherwise leak a frozen playhead into
        // every later serial test (see the test-suite pollution note in `lib.rs`).
        clear_hls_reserve_sample();
        assert_eq!(
            buffered_ms,
            Some(258),
            "the arithmetic reserve the main thread samples is positive and stays there"
        );

        // Walk the silence forward one position-callback period at a time and find the first tick
        // at which the main thread holds the clock. That tick is also what the worker's
        // `StallGuard` reads as `terminal_hold_started`, so it is when the oversized fetch is
        // abandoned and a cheaper rung becomes decidable.
        let mut held_at_ms: Option<u32> = None;
        let mut silence_ms = 0u32;
        while silence_ms <= 82_682 {
            let stopped = native_clock_stopped(Some(silence_ms), buffered_ms);
            if should_begin_hls_rebuffer(false, -1, 5_668, buffered_ms, stopped) {
                held_at_ms = Some(silence_ms);
                break;
            }
            silence_ms += POSITION_PERIOD_MS;
        }
        assert!(
            held_at_ms.is_some_and(|ms| ms <= 1_500),
            "a presentation clock that has physically stopped is the terminal reserve boundary:              expected the hold within 1500 ms of the last position callback, got {held_at_ms:?}              (the device waited 82682 ms for the fetch to complete instead)"
        );
    }

    #[test]
    fn a_failed_original_open_prefers_rollback_then_cold_auto_before_error() {
        use OpenFailureAction::{Error, RollbackPendingOriginal, StartAutoHls};

        assert_eq!(
            open_failure_action(true, true, true),
            RollbackPendingOriginal
        );
        assert_eq!(open_failure_action(false, true, true), StartAutoHls);
        assert_eq!(
            open_failure_action(false, true, false),
            Error,
            "a post-frame failure is not a cold open"
        );
        assert_eq!(
            open_failure_action(false, false, true),
            Error,
            "manual Original stays the user's fixed choice"
        );
    }

    #[test]
    fn segmented_hls_primes_both_lanes_even_without_a_seek() {
        assert!(prime_before_play(false, true));
        assert!(prime_before_play(true, true));
        assert!(prime_before_play(true, false));
        assert!(!prime_before_play(false, false));
    }

    #[test]
    fn boundary_reserves_are_resume_gates_not_mid_acquisition_pause_triggers() {
        assert!(
            !should_begin_hls_rebuffer(false, -1, 3_000, Some(2_999), false),
            "a full acquisition runway cannot be compared to a partially spent buffer mid-fetch",
        );
        assert!(
            should_begin_hls_rebuffer(true, -1, 3_000, Some(2_999), false),
            "the in-flight fetch may still request a controlled hold from its live progress",
        );
        assert!(
            !should_begin_hls_rebuffer(false, 5_303, 5_303, Some(5_167), false),
            "a bounded candidate that reaches its deadline must finish instead of latching a \
             multi-second playback hold at the retrospective replay balance",
        );
        assert!(
            should_begin_hls_rebuffer(false, -1, 3_000, Some(0), false),
            "physical depletion must hold the clock even when a fetch has no measurable prefix",
        );
        assert!(
            should_begin_hls_rebuffer(false, -1, 3_000, Some(258), true),
            "a presentation clock that has stopped is that same physical depletion, observed \
             where the content-time arithmetic cannot reach zero",
        );
    }

    /// The stopped-clock observation is one-sided: it must fire on the device's remnant and must
    /// not fire on a healthy reserve whose callback is merely late.
    #[test]
    fn only_a_reserve_the_stopped_clock_has_outlasted_is_a_terminal_boundary() {
        assert_eq!(NATIVE_CLOCK_STOPPED_MS, 1_005, "five 201 ms position periods");
        assert!(
            !native_clock_stopped(Some(NATIVE_CLOCK_STOPPED_MS - 1), Some(258)),
            "four and a bit periods of silence is jitter, not a stopped clock",
        );
        assert!(
            native_clock_stopped(Some(NATIVE_CLOCK_STOPPED_MS), Some(258)),
            "the device's remnant is smaller than the silence that has already outlasted it",
        );
        assert!(
            !native_clock_stopped(Some(1_200), Some(5_668)),
            "a stream holding 5.6 s of reserve is not starving because one callback was late",
        );
        assert!(
            native_clock_stopped(Some(5_668), Some(5_668)),
            "silence long enough to have presented the whole reserve is the same boundary",
        );
        assert!(
            !native_clock_stopped(None, Some(0)),
            "an epoch with no position callback yet has nothing to call stopped",
        );
        assert!(
            !native_clock_stopped(Some(82_682), None),
            "an unknowable reserve holds nothing, exactly as it arms no StallGuard",
        );
    }

    /// A seek and a session reset both zero `dg_vpres_at`, so the silence must read as absent
    /// rather than as a gap the size of the seek.
    #[test]
    fn a_seek_leaves_no_silence_observation_behind() {
        let _serial = crate::testlock::serial();
        let restore = SHARED.dg_vpres_at.swap(0, Relaxed);
        assert_eq!(
            native_clock_silence_ms(),
            None,
            "a cleared presentation stamp is no observation, not an infinite one",
        );
        // And a deliberate hold floors the observation at the moment it was restarted, so a long
        // user Pause cannot be read as a stall on the first tick after Resume.
        SHARED.dg_vpres_at.store(1, Relaxed);
        CLOCK_WATCH_SINCE_MS.store(super::super::vclock_ms(), Relaxed);
        assert!(
            native_clock_silence_ms().is_some_and(|ms| ms < NATIVE_CLOCK_STOPPED_MS),
            "a just-restarted watch cannot already be a stopped clock",
        );
        CLOCK_WATCH_SINCE_MS.store(0, Relaxed);
        SHARED.dg_vpres_at.store(restore, Relaxed);
    }

    #[test]
    fn media_completed_after_a_hold_request_makes_that_request_stale() {
        assert!(rebuffer_request_is_current(true, 10_000, 10_000));
        assert!(
            !rebuffer_request_is_current(true, 10_000, 12_000),
            "a completed and feedable segment removed the underflow the old request described",
        );
        assert!(!rebuffer_request_is_current(false, 10_000, 10_000));
    }
}
