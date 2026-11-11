//! Physical buffer-feed tick operations. The common pump owns route/recovery decisions and
//! maps these native results to its attempt trace.

use crate::feed_resources::FeedResources;
use crate::feed_state::{Engine, FeedPace, Source, PRES_NONE};
use crate::native_executor::{
    arm_live_clock_prime, drain_aq, feed_both_lanes, feed_sample, fed_totals, play_initial_now,
    NativeHooks,
};
use crate::readout::{playable_buffer_ms, runtime_failure};
use crate::seek::SeekMode;
use crate::shared::{FEED_SHARED, FEED_TX, SPEED_AUDIO_BAD, SPEED_AUDIO_OK};
use crate::shared::Stage;
use crate::shared::{
    HlsPauseCompletion, HlsPlayCompletion, HlsSeekPause, HlsUserPause, HlsUserResume,
};
use plx_base::eventlog::log;
use plx_base::task::MainThread;
use plx_playback_contract::feed::{
    DemuxAbr, FailureFlag, FailureFlags, FeedDiag, FeedFacts, FeedRaw, LoadRaw, SeekRequeue,
    DvWithdrawal, UserPauseOutcome, UserResumeOutcome,
};
use plx_playback_contract::readout::{AttemptFacts, RuntimeFailure};
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::os::raw::c_char;
use std::time::Duration;

// The verdict types a tick returns are contract vocabulary (`plx_playback_contract::feed`); the
// old paths stay for this crate's own code.
pub use plx_playback_contract::feed::{
    CtorGate, LoadTick, LoadTimeout, PausedSeekStep, PresentationTick, SeekPause, StuckSeekAction,
};

pub struct LoadGate {
    pub native_epoch: u32,
    pub verdict: LoadGateVerdict,
}

pub enum LoadGateVerdict {
    Returned,
    Deferred,
    TimedOut(Duration),
    LostEpoch,
}

/// issue #74 D.1.4: the longest the `loadCompleted` arm will wait for `sf_load` to actually
/// return before giving up and publishing `load_failed`. Kodi's own precedent
/// (`MediaPipelineWebOS.cpp:795-801`) is a 1s `wait_for` then `PLAYER_ABORT`; this budget is
/// deliberately far larger because a real `Load` on the dev set measures in the HUNDREDS OF
/// MILLISECONDS for a cold pipeline (investigation.md §A.0's own citation), and the point of this
/// timeout is to catch a genuine hang — the k5lp `DirectVoInit` block investigation.md §B
/// describes — not to interrupt an ordinary slow one. It exists so a Load that never returns
/// becomes a failure read-out instead of a permanent Connecting spinner.
///
/// Its clock is the wall clock from `begin_native_session`, so it counts the Load thread's
/// sound-output read in front of `Load`; the Load-timing log and telemetry do not (`native_load::load_alone`).
/// The feed owns it: the player is told only that it ran out ([`LoadTimeout`]).
pub const NATIVE_LOAD_BUDGET: Duration = Duration::from_secs(20);

/// The native Load's issued→returned gate. The worker's epoch, not a later route choice,
/// authorizes the result. A timeout publishes the existing callback failure latches and lets
/// the common pump attach the appropriate trace class to its current route attempt.
pub fn load_gate(epoch: u32, budget: Duration) -> LoadGate {
    if FEED_SHARED.native_load_returned(epoch) {
        return LoadGate { native_epoch: epoch, verdict: LoadGateVerdict::Returned };
    }
    if FEED_SHARED.native_load_deferred_logged_epoch.swap(epoch, Relaxed) != epoch {
        log("native: loadCompleted while Load in flight — deferring");
    }
    let verdict = match FEED_SHARED.native_load_elapsed(epoch) {
        Some(elapsed) if elapsed >= budget => {
            log(&format!(
                "native: Load did not return within {}s — publishing load_failed",
                budget.as_secs()
            ));
            FEED_SHARED.load_failed.store(true, Release);
            FEED_SHARED.load_timed_out.store(true, Release);
            LoadGateVerdict::TimedOut(elapsed)
        }
        Some(_) => LoadGateVerdict::Deferred,
        // A synchronous UnloadCompleted can retire the native epoch while Load is deferred.
        // The gate can never open then, so surface the same failure instead of waiting forever.
        None => {
            log(
                "native: Load epoch left Active while loadCompleted was still deferred — \
                 publishing load_failed",
            );
            FEED_SHARED.load_failed.store(true, Release);
            FEED_SHARED.load_timed_out.store(true, Release);
            LoadGateVerdict::LostEpoch
        }
    };
    LoadGate { native_epoch: epoch, verdict }
}


pub fn ctor_gate(feed: &mut FeedResources, mt: &MainThread) -> CtorGate {
    let hooks = feed.hooks();
    let Some(eng) = feed.engine() else { return CtorGate::Awaiting { timeout: None }; };
    if unsafe { (hooks.feed.sink)().ready(mt) } != 0 { return CtorGate::Ready; }
    // A constructor still in flight owes the Load budget check while its Load is live; the verdict
    // is the feed's, on the feed's clock, and the player only attributes a timeout.
    let timeout = (eng.stage == Stage::Loading && !eng.preview_abandon)
        .then(|| match load_gate(eng.native_epoch, NATIVE_LOAD_BUDGET).verdict {
            LoadGateVerdict::Returned | LoadGateVerdict::Deferred => None,
            LoadGateVerdict::TimedOut(elapsed) => Some(LoadTimeout::Elapsed(elapsed)),
            // The epoch retired before this poll could measure its elapsed time.
            LoadGateVerdict::LostEpoch => Some(LoadTimeout::LostEpoch),
        })
        .flatten();
    CtorGate::Awaiting { timeout }
}

/// The reopen budget measures the demux/keyframe phase. Once Play accepts a Segment seek,
/// give the sink its own budget to display on the installed timeline.
pub fn stuck_seek_due(feed: &mut FeedResources, now: u32, feed_allowed: bool,
    route_flight: bool, budget_ms: u32) -> bool {
    let Some(eng) = feed.engine() else { return false; };
    if let Some(landing) = eng.segment_landing.as_mut() {
        if let Some(play) = landing.play.as_mut() {
            if landing.epoch != FEED_SHARED.seek_epoch.load(Relaxed) {
                eng.segment_landing = None;
                return false;
            }
            let clock_advanced = FEED_SHARED.dg_pos_ticks.load(Acquire) != play.position_tick;
            let position = FEED_SHARED.pres_fed.load(Relaxed);
            let displayed = FEED_SHARED.sink_displayed_sequence.load(Acquire) != play.displayed;
            // Compare the reported pipeline position to the accepted video, in fed time.
            // The requested position can be several seconds beyond the landing keyframe.
            let video_runway = eng.seek_base_pts..=eng.max_fed_video_pts;
            if clock_advanced && video_runway.contains(&position) && displayed {
                FEED_SHARED.seek_landed_epoch.store(landing.epoch, Release);
                eng.segment_landing = None;
                return false;
            }
            if !feed_allowed || route_flight || eng.dv_start.landing_held() {
                play.waiting_since = None;
                return false;
            }
            const SEGMENT_PRESENTATION_MS: u32 = 4_000;
            return now.wrapping_sub(*play.waiting_since.get_or_insert(now)) >= SEGMENT_PRESENTATION_MS;
        }
    }
    if eng.flushed && eng.rebase_pending && !feed_allowed {
        eng.seek_armed_at = now;
    }
    eng.flushed && eng.rebase_pending
        && now.wrapping_sub(eng.seek_armed_at) > budget_ms
        && !route_flight
        && !eng.dv_start.landing_held() // the Dolby Vision guard holds this landing and reloads it itself
}

/// Retry an unanchored reopen. When the segment was already primed, a new Load owns recovery.
pub fn retry_stuck_seek(feed: &mut FeedResources, now: u32, target_ns: i64,
    can_retry: bool) -> StuckSeekAction {
    let eng = feed.engine().expect("stuck seek belongs to live feed");
    if eng.segment_landing.as_ref().is_some_and(|seek| seek.play.is_some()) {
        eng.segment_landing = None;
        return StuckSeekAction::Reload;
    }
    if eng.seek_retries < 2 && can_retry {
        if let Some(landing) = eng.segment_landing.as_mut() {
            landing.epoch = FEED_SHARED.seek_epoch.load(Relaxed);
        }
        eng.seek_retries += 1;
        FEED_SHARED.seek_to_ns.store(target_ns, Release);
        FEED_SHARED.seek_target_ns.store(target_ns, Relaxed);
        eng.seek_armed_at = now;
        StuckSeekAction::Retried(eng.seek_retries)
    } else {
        StuckSeekAction::Reload
    }
}

/// Eligibility for applying a pending direct-play/transcode seek on this physical feed.
pub fn pending_seek_ready(feed: &FeedResources, seek_reloads: bool) -> bool {
    feed.pending_seek_ready(seek_reloads)
}

/// One native Pause transition before an in-place seek. The common route owner chooses any
/// fallback reload from this typed verdict.
pub fn prepare_seek_pause(feed: &mut FeedResources, mt: &MainThread) -> SeekPause {
    let hooks = feed.hooks();
    match FEED_SHARED.prepare_seek_pause() {
        Some(HlsSeekPause::AlreadyHeld) => SeekPause::Ready,
        Some(HlsSeekPause::Issue(token)) => {
            let accepted = unsafe { (hooks.feed.sink)().pause(mt) } != 0;
            match FEED_SHARED.complete_seek_pause(token, accepted) {
                HlsPauseCompletion::Accepted => SeekPause::Ready,
                HlsPauseCompletion::Refused => SeekPause::Refused,
                HlsPauseCompletion::Stale => SeekPause::Stale,
            }
        }
        None => SeekPause::Busy,
    }
}

pub fn begin_seek_discontinuity(feed: &mut FeedResources) -> bool {
    feed.begin_seek_discontinuity()
}

/// Place the webOS 5+ exported window at the native Load's completion, before decoded frames.
/// `src` is the coded size, not the 1080p canvas. The coded size becomes known later; the caller
/// re-places once the demuxer publishes it. Record the place result for diagnostics: before this
/// mirror, the panel falsely said "not placed" throughout a successful exported-window session.
pub fn place_exported(mt: &MainThread, eng: &mut Engine, hooks: &NativeHooks) {
    if (hooks.feed.sink)().window_mode() != plx_platform::tv::sink::VP_EXPORTED { return; }
    let (w, h) = FEED_SHARED.video_raster();
    let src = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    let rv = unsafe { (hooks.feed.sink)().place_window(mt, src.0, src.1, 0, 0, 1920, 1080) };
    eng.placed_src = src;
    FEED_SHARED.dg_place_rv.store(rv, Relaxed);
    FEED_SHARED.dg_placed_w.store(src.0, Relaxed);
    FEED_SHARED.dg_placed_h.store(src.1, Relaxed);
    log(&format!(
        "vplane: exported window placed src={}x{} rv={rv}",
        src.0, src.1
    ));
}

/// The pump's half of "a paused film stays paused across a scrub", run at the head of every tick:
///
/// * a viewer Pause pressed AFTER a playing seek was dispatched shut the feed gate under it, so
///   the pump opens its preroll now (and restarts the stuck watchdog's clock from the open);
/// * a session with no picture yet under a viewer Pause (any reload, or a Pause before the first
///   frame) gets the same preroll, so it can present one frame (see the branch below);
/// * once the preroll the pump owns has LANDED on whichever Engine is live (in place: the keyframe
///   re-anchored; a reload or transcode landing: the fresh session re-anchored, and `frames` —
///   zeroed by the arm and by every session reset — counts a POST-seek presentation), re-pause with
///   `finish_paused_seek` (the common player's) (on the ACB route only once the plane is bound and a tick has
///   followed the bind: [`plane_shows_the_picture`]). A refused re-pause leaves the preroll and its ownership in
///   place, so the next tick retries; the film plays only for that long.
///
/// `playpos + 15 s >= target` is the guard the loop's old re-pause had: a presented frame far
/// before the target is not the landing. Runs before `pa` is split, because the re-pause takes the
/// whole adapter.
pub fn paused_seek_step(
    feed: &mut FeedResources,
    now: u32,
    route_flight: bool,
    claim_owns_pause: bool,
) -> PausedSeekStep {
    let hooks = feed.hooks();
    let mut step = PausedSeekStep::default();
    if FEED_TX.paused.load(Acquire)
        && !FEED_TX.seek_preroll_active()
        && FEED_SHARED.seeking.load(Relaxed)
        && !route_flight
        && !claim_owns_pause
    {
        let target = FEED_SHARED.seek_target_ns.load(Relaxed);
        let target = if target >= 0 { target } else { FEED_SHARED.playpos_ns.load(Relaxed) };
        // Only a seek the pump already DISPATCHED and that is still in flight: no newer tap waiting
        // (`request_seek` sets `FEED_SHARED.seeking` and `FEED_TX.seek_to_ns` together, ahead of the
        // dispatcher, and the dispatcher's `own_seek_under_pause` owns THAT seek at its own target),
        // and the Engine flushed or awaiting its keyframe's rebase.
        if let Some(eng) = feed.engine().filter(|e| FEED_TX.seek_to_ns.load(Relaxed) < 0 && (e.flushed || e.rebase_pending)) {
            // The Pause marked the clock the seek prime owns as the viewer's, which fences the
            // prime's Play: give the hold back to the prime first (it Plays one frame, then
            // `finish_paused_seek` re-pauses).
            if FEED_SHARED.hand_hold_to_seek_prime() && FEED_TX.own_paused_seek(target) {
                eng.seek_armed_at = now;
                step.mid_seek_claimed = true;
                step.preroll_opened = true;
                log("seek: viewer paused mid-seek; the pump opened the preroll");
            }
        }
    }
    // A session with no picture under a viewer Pause. A reload keeps the Pause and zeroes `frames`,
    // the fresh Engine arms an Initial hold the Pause marks user-held, and the feed gate
    // (`FEED_TX.feed_allowed`) stays shut: no keyframe is fed, nothing is ever presented, and the HUD
    // reads Buffering over a blank plane until Play. Derived from the state, so it covers every
    // route that reloads (retranscode, adaptive, native audio, Original, recovery landings, a
    // Pause pressed during a claim flight or before the first frame) without a call at each site:
    // open the same bounded preroll a paused seek gets. Once a frame is presented `frames >= 1`
    // closes this branch for good and the landed check below re-pauses.
    if FEED_TX.paused.load(Acquire)
        && !FEED_TX.seek_preroll_active()
        && !claim_owns_pause
        && !route_flight
        && FEED_TX.seek_to_ns.load(Relaxed) < 0
        && FEED_SHARED.frames.load(Relaxed) == 0
        // A prime still owed: only then does the seek hold hand back a Play. Once the prime's Play
        // has fired (`prime_play` cleared, the clock Running) a Pause before the first position
        // update is a plain `Held(User)`; converting it would clear `user_held` with nothing left
        // to Play, and the viewer's Play would then find no hold to release.
        && feed.engine().is_some_and(|e| e.prime_play)
        && FEED_SHARED.hand_hold_to_seek_prime()
    {
        // Owned at 0, not at `playpos_ns`: `frames` is per-session (zeroed by every reset), so the
        // first presented frame IS this session's landing, while `playpos_ns` is the PRE-reload
        // playhead (a reload does not rewrite it). A target taken from it makes the `+ 15 s >=
        // target` guard fail for a reload that lands behind the old playhead, and the film then
        // plays on under a Paused transport.
        if FEED_TX.own_paused_seek(0) {
            step.preroll_opened = true;
            log("reload: viewer paused with no picture yet; the pump opened the preroll");
        }
    }
    let Some(target) = FEED_TX.owned_paused_seek() else { return step };
    let Some(eng) = feed.engine() else { return step };
    let landed = !eng.flushed
        && !eng.rebase_pending
        && eng.segment_landing.is_none()
        && FEED_TX.seek_to_ns.load(Relaxed) < 0
        && !route_flight
        && FEED_SHARED.frames.load(Relaxed) >= 1
        && plane_shows_the_picture(eng, hooks)
        && FEED_SHARED.playpos_ns.load(Relaxed) + 15 * 1_000_000_000 >= target;
    if landed {
        step.finish = true;
    }
    step
}

/// The viewer's Pause, end to end: the clock transition, the native Pause through the
/// composition's sink, and the transport commit at the same accepted boundary. Publishing the feed
/// gate here, with the native acceptance, is what keeps a deadline transaction from charging
/// accepted Pause time as active playback. The player mirrors the ACB playstate after the answer.
pub fn user_pause(feed: &mut FeedResources, mt: &MainThread) -> UserPauseOutcome {
    match FEED_SHARED.prepare_hls_user_pause() {
        Some(HlsUserPause::AlreadyHeld) => {
            FEED_TX.commit_paused(true);
            UserPauseOutcome::AlreadyHeld
        }
        Some(HlsUserPause::Issue(token)) => {
            // SAFETY: the main thread, through the composition's fenced sink.
            let accepted = unsafe { (feed.hooks().feed.sink)().pause(mt) } != 0;
            let completion = FEED_SHARED.complete_hls_user_pause(token, accepted);
            if completion == HlsPauseCompletion::Accepted {
                FEED_TX.commit_paused(true);
            }
            UserPauseOutcome::Issued(completion)
        }
        None => UserPauseOutcome::Deferred,
    }
}

/// The viewer's Resume, end to end: the mirror of [`user_pause`]. A paused-seek preroll that is
/// open is closed with the commit, so the Play below it never runs against a gate it holds shut.
pub fn user_resume(feed: &mut FeedResources, mt: &MainThread) -> UserResumeOutcome {
    let queued_stream = feed.uses_stream_queues();
    let close_preroll_and_commit = || {
        if FEED_TX.seek_preroll_active() {
            FEED_TX.finish_seek_preroll();
        }
        FEED_TX.commit_paused(false);
    };
    match FEED_SHARED.prepare_hls_user_resume(queued_stream) {
        Some(HlsUserResume::Deferred) => {
            // The retranscode's fresh Load arms the initial prime (a non-User clock hold,
            // prime_play latched) but nothing else will fire it: the landing issues this
            // Resume, never the runner's Operation::Play, so startup_play_commanded stays
            // false and try_prime's startup gate would block the post-Load retry forever
            // (observed live: BufferFull at the native cap, pos_ms=0).
            // prepare_hls_user_resume just cleared user_held and set resume_acb_pending; this
            // Resume IS the session's startup play command — mark it as commanded, then fire
            // the initial prime. If the physical Play lands before the Load worker has called
            // Starfish's Load it is Refused and the prime stays armed (Held{Initial});
            // try_prime is now un-gated and issues the Play once loadCompleted has passed
            // and both lanes are primed. Only the Fresh-kind holds (Initial, Seek) accept
            // play_initial_now; Rebuffer/ResumePrime falls through to Fenced.
            let hooks = feed.hooks();
            let fired = feed.engine().is_some_and(|eng| {
                eng.startup_play_commanded = true;
                matches!(
                    crate::native_executor::play_initial_now(mt, eng, &hooks.feed),
                    Some(true)
                )
            });
            close_preroll_and_commit();
            if fired {
                UserResumeOutcome::Played { resume_acb: true }
            } else {
                UserResumeOutcome::Fenced
            }
        }
        Some(HlsUserResume::Prime) => {
            let Some(engine) = feed.engine() else { return UserResumeOutcome::PrimeLost };
            crate::native_executor::arm_live_clock_prime(engine);
            close_preroll_and_commit();
            UserResumeOutcome::Priming
        }
        Some(HlsUserResume::Issue(token)) => {
            // SAFETY: the main thread, through the composition's fenced sink.
            let accepted = unsafe { (feed.hooks().feed.sink)().play(mt) } != 0;
            match FEED_SHARED.complete_hls_prime_play(token, accepted) {
                HlsPlayCompletion::Accepted { resume_acb } => {
                    close_preroll_and_commit();
                    UserResumeOutcome::Played { resume_acb }
                }
                HlsPlayCompletion::Refused => UserResumeOutcome::PlayRefused,
                HlsPlayCompletion::Stale => UserResumeOutcome::PlayStale,
            }
        }
        None if FEED_TX.seek_preroll_active() => {
            close_preroll_and_commit();
            UserResumeOutcome::PrerollCancelled
        }
        None => UserResumeOutcome::NoHold,
    }
}

/// Is a picture the Engine has presented actually on the panel? Without ACB (webOS 5+) the sink is
/// placed at Load, so any presented tick is. On the ACB route (webOS 4) the video plane connects
/// only through `setMediaVideoData` + `acb_start` (the `Stage::Streaming` transition below), and a
/// tick counted before that is a picture the plane never showed. A paused preroll re-paused on such
/// a tick leaves a black panel (device-measured: a VIDEO-only capture read "no signal" until
/// Resume), because the pause stops the ticks the bind is waiting on. So the landing needs the
/// bind complete AND one tick after it (`Engine::bound_at_frames`).
pub(crate) fn plane_shows_the_picture(eng: &Engine, hooks: &NativeHooks) -> bool {
    plane_shows(hooks.release.plane.handle(), eng.stage, eng.bound_at_frames, eng.shared.frames.load(Relaxed))
}

/// The pure rule of [`plane_shows_the_picture`], over the facts it reads.
pub(crate) fn plane_shows(acb_plane: bool, stage: Stage, bound_at_frames: Option<i32>, frames: i32) -> bool {
    if !acb_plane {
        return true;
    }
    stage >= Stage::Streaming && bound_at_frames.is_none_or(|at| frames != at)
}

/// **Is a picture on the panel, as far as this process can know?** The Feed driver's grounds for
/// `Observation::Presented`: [`plane_shows`] (on the ACB route the bind is complete and one
/// position tick has followed it) AND the video sink's displayed-frame counter has reported a
/// positive count since that bind ([`crate::shared::Shared::displayed_after_bind`]; with no bind
/// to wait for on webOS 5+, since the session began). A position tick alone (type 0 `FRAMEREADY`)
/// is never enough: it is a 5 Hz clock sample, and one counted before the plane bind left the
/// panel black on the device. Necessary-condition evidence, not photon proof; a firmware that
/// never delivers the counter (`era callbacks: no displayed-frame counter`) never presents.
pub(crate) fn picture_on_panel(
    shared: &crate::shared::Shared, acb_plane: bool, stage: Stage, bound_at_frames: Option<i32>,
) -> bool {
    plane_shows(acb_plane, stage, bound_at_frames, shared.frames.load(Relaxed))
        && shared.displayed_after_bind()
}


pub fn publish_diag(
    feed: &mut FeedResources,
    now: u32,
    last_frames: &std::sync::atomic::AtomicI32,
    readout_on: bool,
) {
    let Some(eng) = feed.engine() else { return; };
    FEED_SHARED.dg_stage.store(eng.stage as u8, Relaxed);
    // Nobody is looking: skip it. `aq_bytes` takes each queue's pthread mutex, and the read-out
    // samples at 2 Hz, so publishing at 60 Hz is 30x more often than anything can observe. Costs
    // no freshness — the loop order is pump → `Diagnostics::update` → `Diagnostics::draw`
    // (`app/run.rs`), so the frame the panel
    // is switched on has already republished.
    if !readout_on {
        return;
    }
    let qv = eng.aq_video.as_ref().map_or(0, |q| {
        crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64
    });
    let qa = eng.aq_audio.as_ref().map_or(0, |q| {
        crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64
    });
    FEED_SHARED.dg_aq_video.store(qv, Relaxed);
    FEED_SHARED.dg_aq_audio.store(qa, Relaxed);
    FEED_SHARED.dg_fed_v_pts.store(eng.max_fed_video_pts, Relaxed);
    FEED_SHARED.dg_fed_a_pts.store(eng.max_fed_audio_pts, Relaxed);
    // Stamp when the frame count MOVES. The panel needs "how long has it been stuck" and a
    // photograph has no time axis; stamping here rather than in `app::diagnostics` is what makes the
    // clock measure the STALL rather than how long the panel has been open.
    let f = FEED_SHARED.frames.load(Relaxed);
    if last_frames.swap(f, Relaxed) != f {
        FEED_SHARED.dg_frame_at.store(now, Relaxed);
    }
}

/// **`plxnative-refuse601` (dev builds): the pipeline's `type=18 num=601` after the Load returned,
/// once per request.** The refusal webOS 10.3.1 gives a stream it cannot allocate a decoder for
/// (#695, DTS), reproduced on a set that would take the stream: once the Load has returned and
/// before any picture, it publishes through [`crate::sf_event::sf_on_event`], the entry the firmware calls, so
/// the callback's own latches (`dg_cb_err_num`, `load_failed`) and the failed-open ladder see exactly
/// what a real refusal gives them.
///
/// The read is a `devtrig::flag` (a `stat`), and a latched flag could not be armed per test: a
/// process-wide latch hands whichever test ran first its answer for all of them. So the cheap
/// atomic guards come first and the file is read only inside the open window (after the Load
/// returned, before a picture, and for a request not yet refused), never on a playing frame. The
/// latch is written only after the flag holds, so an unarmed request is never marked refused.
pub fn inject_refuse601(
    feed: &mut FeedResources,
    generation: u32,
    preview: bool,
    refused_for: &std::sync::atomic::AtomicU32,
) {
    let Some(eng) = feed.engine() else { return; };
    if generation == 0
        || preview
        || !FEED_SHARED.native_load_returned(eng.native_epoch)
        || FEED_SHARED.seen_frame.load(Relaxed)
        || refused_for.load(Acquire) == generation
    {
        return;
    }
    if !plx_base::devtrig::flag("refuse601") {
        return;
    }
    if refused_for.swap(generation, std::sync::atomic::Ordering::AcqRel) == generation {
        return;
    }
    #[cfg(feature = "devtriggers")]
    log("devtrig refuse601: publishing type=18 num=601 after the Load");
    crate::sf_event::sf_on_event(eng.native_epoch, 18, 601, std::ptr::null());
}

/// The Dolby Vision landing guard's work for this frame ([`crate::dv_start`], #1166): the withdrawal
/// the feed path raised at a landing it predicted a skip for, else the watch's verdict on this tick's
/// [`Probe`](crate::dv_start::Probe). A report-only guard's silence is logged here and answers `None`.
///
/// A flight owns the route's reload (`route_flight`), so while one is out a pending withdrawal waits
/// and the watch's clock does not run: the first frame after it clears takes the withdrawal, at the
/// target the player reads THEN.
pub fn dv_landing_due(feed: &mut FeedResources, now: u32, route_flight: bool) -> Option<DvWithdrawal> {
    use crate::dv_start::{Mode, Probe, Step};
    let eng = feed.engine()?;
    if eng.dv_start.mode() == Mode::Inert {
        return None;
    }
    if !route_flight {
        if let Some(w) = eng.dv_start.take_withdrawal() {
            return Some(w.to_contract());
        }
    }
    let probe = Probe {
        now,
        landed: !eng.rebase_pending,
        prime_released: !eng.prime_play,
        accepted_ns: eng.max_fed_video_pts - eng.seek_base_pts,
        feed_open: FEED_TX.feed_allowed(),
        // The viewer's clock runs (or a paused-seek preroll is open): the `paused && !preroll` test the
        // Play/pause ladder uses. A flight holds presentation, so it is not a running clock.
        clock_running: !route_flight && (!FEED_TX.paused.load(Acquire) || FEED_TX.seek_preroll_active()),
        frames: FEED_SHARED.frames.load(Relaxed),
    };
    match eng.dv_start.step(&probe) {
        Step::Quiet => None,
        Step::Report(r) => {
            log(&r.line());
            None
        }
        Step::Withdraw(w) => Some(w.to_contract()),
    }
}

/// The route refused the withdrawal [`dv_landing_due`] handed over: the guard stands down and the
/// held landing feeds.
pub fn dv_stand_down(feed: &mut FeedResources) {
    if let Some(eng) = feed.engine() {
        eng.dv_start.stand_down();
    }
}

/// The native surround audio watchdog ([`crate::native_stall`]): the pipeline took the Load and a few
/// access units of a native Opus track with more than two channels, then stopped
/// accepting with both our queues full and no picture. True when it fired, with the refusal flag
/// raised: the caller hands the audio to the failed-open ladder, whose named write is the learner
/// ([`confirm_refusal`]).
pub fn native_audio_stall_fired(feed: &mut FeedResources, now: u32) -> bool {
    let Some(eng) = feed.engine() else { return false; };
    if eng.preview_abandon { return false; }
    use crate::native_stall::{Probe, Step, NATIVE_AUDIO_STALL_MS, SIZES, WATCH};
    let fed = crate::ff::native_audio::fed();
    let queue = |q: &Option<Box<crate::aq::AuQueue>>| q.as_ref().map_or(0, |q| crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64);
    let (qa, qv) = (queue(&eng.aq_audio), queue(&eng.aq_video));
    let accepted = fed_totals().1;
    let probe = Probe {
        now,
        channels: fed.map_or(0, |(_, c)| c),
        load_returned: FEED_SHARED.native_load_returned(eng.native_epoch),
        frames: FEED_SHARED.frames.load(Relaxed),
        accepted,
        audio_queue_bytes: qa,
        video_queue_bytes: qv,
        landing_held: eng.dv_start.landing_held(),
    };
    match WATCH.lock().unwrap_or_else(|e| e.into_inner()).step(&probe) {
        Step::Quiet => false,
        Step::Armed => {
            if let Some((kind, channels)) = fed {
                log(&format!(
                    "native audio: watching {} with {channels} channels ({} accepted AUs, audio queue {qa} B, video queue {qv} B); \
                     fires if nothing is accepted for {} ms with no picture",
                    kind.ffmpeg_name(),
                    accepted,
                    NATIVE_AUDIO_STALL_MS
                ));
            }
            false
        }
        Step::Fire { armed_ms, stalled_ms, accepted } => {
            let sizes = SIZES.lock().unwrap_or_else(|e| e.into_inner()).summary().unwrap_or_else(|| "none".into());
            log(&format!(
                "native audio: STALL, no picture: {accepted} AUs accepted, none for {stalled_ms} ms (watching for {armed_ms} ms), \
                 audio queue {qa} B, video queue {qv} B, fed packet sizes {sizes}"
            ));
            FEED_SHARED.raise_native_audio_refused();
            true
        }
    }
}

/// The one event-log line for an era-chosen sink counter that never arrived in a played session
/// ([`crate::era_callbacks`]). It only reads counters and logs; nothing branches on it.
pub fn note_missing_era_callback(feed: &mut FeedResources) {
    let Some(eng) = feed.engine() else { return; };
    if eng.preview_abandon { return; }
    use crate::era_callbacks::{line, Probe, Step};
    let major = plx_platform::tv::device::info().major;
    let probe = Probe {
        ticks: FEED_SHARED.dg_pos_ticks.load(Relaxed),
        displayed: FEED_SHARED.dg_sink_displayed_cb.load(Relaxed),
        other_numbering: FEED_SHARED.dg_sink_other_cb.load(Relaxed),
        major,
        expected_type: crate::sf_event::displayed_counter_type(major),
    };
    if let Step::Missing(m) = eng.era_watch.step(&probe) {
        log(&line(&m));
    }
}

/// Apply native flush and demux rebase only after the route owner committed the seek.
pub fn apply_inplace_seek_pre(feed: &mut FeedResources, mt: &MainThread,
    t: i64, mode: SeekMode, coalesced: u32) {
    let hooks = feed.hooks();
    let eng = feed.engine().expect("in-place seek belongs to live feed");
        eng.segment_landing = (mode == SeekMode::Segment).then(|| crate::feed_state::SegmentLanding {
            epoch: FEED_SHARED.seek_epoch.load(Relaxed),
            play: None,
        });
        // `NativeSeek` does not flush here: LG's public `Seek` flushes the pipeline itself when
        // `feed_stream` calls it on the landed keyframe, and a `flush()` is itself a seek to 0, so
        // flushing after (or instead of re-anchoring) leaves the stale segment at 0 (#568).
        if mode != SeekMode::NativeSeek {
            unsafe {
                (hooks.feed.sink)().flush(mt); // drop decoded/queued frames only after the clock is really held
            }
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
        FEED_SHARED.seek_to_ns.store(t, Release); // the demux thread's av_seek target
        FEED_SHARED.seek_target_ns.store(t, Relaxed); // rebase guard: reject stale drifted keyframes
        FEED_SHARED.disp_base.store(0, Relaxed);
        log(&format!(
            "seek(in-place): av_seek t={t} coalesced={coalesced}"
        ));
        // zero-base the fed timeline on the first post-seek keyframe (feed_stream), so it
        // presents against the flush-reset clock immediately — no catch-up freeze
        eng.rebase_pending = true;
        eng.rebase_drops = 0;
        // A seek armed under a viewer Pause: the pump opens the preroll and closes it on the landing.
}

/// Finish the physical seek arm after common has opened any viewer-paused preroll.
pub fn apply_inplace_seek_post(feed: &mut FeedResources, t: i64, now: u32) {
    let eng = feed.engine().expect("in-place seek belongs to live feed");
        eng.seek_armed_at = now; // stuck-watchdog start (see the coalesce/escape above)
        eng.seek_retries = 0;
        eng.max_fed_video_pts = 0;
        eng.max_fed_audio_pts = 0;
        eng.audio_start_fp = None;
        // Drop any AU held from BEFORE the seek (the per-lane BufferFull retry). drain_aq cleared the
        // QUEUES but not these pending AUs; feeding a stale pre-seek AU after the seek re-bases it to
        // the NEW pts_shift → a big A/V desync — the stale audio pts advances max_fed_audio_pts, then
        // the STALE_BACKJUMP guard drops the fresh post-seek audio as "too far back" → no sound. The
        // reopened stream supplies fresh post-seek AUs.
        eng.pending_video = None;
        eng.pending_audio = None;
        eng.dv_start.rearm(Some(t)); // a fresh landing for the Dolby Vision guard (`crate::dv_start`)
        eng.prime_play = true; // paused above; Play once PRIME_NS is buffered (no fast-forward)
                               // "no post-seek frame presented yet" — the feed-ahead throttle feeds freely until the first
                               // real presented pts lands, instead of comparing the new fed pts against the STALE pre-seek
                               // presented position (which would wrongly break feeding on a forward in-place seek).
        FEED_SHARED.pres_fed.store(PRES_NONE, Relaxed);
        eng.feed_pace = FeedPace::default(); // the run-on belongs to the pre-seek report
        FEED_SHARED.frames.store(0, Relaxed); // count only POST-seek frames (rebind + resume re-pause gate)
                                         // …and forget the last presentation stamp with it. The plane goes dark across a seek by
                                         // design, so the first frame after one would otherwise post a gap the size of the seek and
                                         // read as a stutter (`Shared`'s `dg_vpres_*` block).
        FEED_SHARED.dg_vpres_at.store(0, Relaxed);
        FEED_SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
}

pub fn prime_before_play(rebase_pending: bool, segmented_hls: bool) -> bool {
    rebase_pending || segmented_hls
}

/// Physical Load/Play transition. Route and report side effects are returned as addressed facts
/// for the common pump to apply before ACB bind and feeding.
pub fn tick_load(feed: &mut FeedResources, mt: &MainThread, now: u32,
    segmented_hls: bool, speed_deadline_ms: u32, no_audio: bool) -> LoadTick {
    let budget = NATIVE_LOAD_BUDGET;
    let hooks = feed.hooks();
    let eng = feed.engine().expect("Load tick belongs to live feed");
    let mut notice = LoadTick::default();
    let gate_ready = if eng.stage == Stage::Loading && !eng.preview_abandon {
        match load_gate(eng.native_epoch, budget).verdict {
            LoadGateVerdict::Returned => true,
            LoadGateVerdict::Deferred => false,
            LoadGateVerdict::TimedOut(elapsed) => {
                notice.timeout = Some(LoadTimeout::Elapsed(elapsed));
                false
            }
            LoadGateVerdict::LostEpoch => {
                notice.timeout = Some(LoadTimeout::LostEpoch);
                false
            }
        }
    } else { false };
    // ---------- load -> Play (decode fed frames as soon as loaded) ----------
    // issue #74 D.1: the gate below requires `FEED_SHARED.native_load_returned(eng.native_epoch)`
    // BEFORE this arm may even poll `sf_is_load_completed` — that poll is itself a concurrent
    // Starfish call, and on v0.6.0 it was the FIRST thing this arm did while `sf_load` was still
    // executing (synchronously) on the load thread. See `src/starfish.c`'s `g_load_returned` for
    // the matching seam-side half; `native_load::load_thread` flips the Rust side right after the
    // real, blocking `sf_load` call returns. Gated on `!eng.preview_abandon` too, same as the
    // completion arm below: an abandoned trailer-preview Load must not drive any of this — no
    // deferral logging, no budget clock, nothing that reacts to it as if it were live. That is
    // why the guard is checked BEFORE [`load_gate`] is even called: the gate is
    // where the deferral logging and the D.1.4 budget actually live (see its own doc).
    if eng.stage == Stage::Loading
        && !eng.preview_abandon
        && gate_ready
        && (FEED_SHARED.load_completed.load(Relaxed) || unsafe { (hooks.feed.sink)().is_load_completed(mt) } != 0)
    {
        // "native: Load returned after Nms" is logged unconditionally at the gate transition
        // itself (`native_load::load_thread`, right where `mark_native_load_returned` flips it), not
        // here — see `load-returned-log-is-conditional-on-loadcompleted-and-unbounded-after`. It
        // used to live in this arm, which meant it only ever appeared paired with the deferral
        // line above, and never at all on the path this fix is most likely to be read on: Load
        // eventually returns but `loadCompleted` goes quiet, so this arm never runs.
        FEED_SHARED.load_completed.store(true, Relaxed);
        // when it completed, so the panel can say how long we have been waiting for a frame since
        FEED_SHARED.dg_load_at.store(now, Relaxed);
        log("SMP loadCompleted");
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
        place_exported(mt, eng, hooks);
        if (hooks.feed.sink)().window_mode() == plx_platform::tv::sink::VP_EXPORTED {
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
        if eng.startup_controlled {
            // The common runner owns the first Play. A progressive source is ready to issue it
            // immediately; a seek or segmented stream still waits for the existing prime proof.
            eng.startup_immediate_play = !prime_before_play(eng.rebase_pending, segmented_hls);
            eng.prime_play = true;
            if !eng.startup_immediate_play {
                log("SMP loadCompleted (priming before Play)");
            }
        } else if prime_before_play(eng.rebase_pending, segmented_hls) {
            eng.prime_play = true;
            log("SMP loadCompleted (priming before Play)");
        } else {
            let _ = play_initial_now(mt, eng, &hooks.feed);
        }
    } else if eng.stage == Stage::Loading
        && !eng.preview_abandon
        && FEED_SHARED.native_load_returned(eng.native_epoch)
    {
        // Load HAS returned here (the first arm's `load_gate` did not return) and
        // `loadCompleted` has NOT arrived (the previous arm's condition also failed) — the other
        // half of issue #74 D.1's budget, and the one `load-returned-log-is-conditional-on-
        // loadcompleted-and-unbounded-after` found unbounded: on v0.6.0 and unchanged until now,
        // a Load that returns ok=1 and then never signals `loadCompleted` left the player parked
        // in Connecting forever, with no counterpart to the D.1.4 timeout above. Gated on
        // `!eng.preview_abandon` too: an abandoned trailer-preview Load has nobody watching a
        // failure read-out for, and must not publish `load_failed`/`load_timed_out` either.
        // Budget it from
        // the moment Load RETURNED, not from when it was ISSUED — `native_load_elapsed` (used
        // above) never resets, so a Load that used most of its own budget getting here would
        // otherwise leave this second wait almost none of its own.
        match FEED_SHARED.native_load_returned_elapsed(eng.native_epoch) {
            Some(elapsed) if elapsed >= budget => {
                log(&format!(
                    "native: loadCompleted did not arrive within {}s after Load returned — \
                     publishing load_failed",
                    budget.as_secs()
                ));
                FEED_SHARED.load_failed.store(true, Release);
                FEED_SHARED.load_timed_out.store(true, Release);
                notice.timeout = Some(LoadTimeout::Elapsed(elapsed));
            }
            Some(_) => {}
            // Same rationale as the `None` arm above: losing `Active` (a synchronous
            // UnloadCompleted for this epoch) while still waiting on `loadCompleted` must fire
            // the timeout rather than silently going quiet forever.
            None => {
                log(
                    "native: Load epoch left Active while still waiting on loadCompleted after \
                     Load returned — publishing load_failed",
                );
                FEED_SHARED.load_failed.store(true, Release);
                FEED_SHARED.load_timed_out.store(true, Release);
                notice.timeout = Some(LoadTimeout::LostEpoch);
            }
        }
    }

    // ---------- playback speed: the safety net ----------
    // A speed session's Load declared PCM. The pipeline reports what it set up (audio-info
    // callback, `crate::sf_event`); a report that is not the declared 48 kHz
    // stereo, or none within speed_deadline_ms of loadCompleted, means this television
    // does not take app-fed PCM the way the 3.9.3 set does — fall back to 1x rather than play
    // silence or a drifting sound. Video-only Loads (the `noaudio` trigger) have no audio to judge.
    if FEED_SHARED.applied_rate_permille.load(Relaxed) != crate::shared::RATE_NORMAL
        && eng.stage >= Stage::Playing
        && !no_audio
        && FEED_TX.seek_to_ns.load(Relaxed) < 0
    {
        match FEED_SHARED.speed_audio_verdict.load(Relaxed) {
            crate::shared::SPEED_AUDIO_BAD => {
                notice.speed_fallback = Some("the pipeline set up different audio than the PCM declared");
            }
            crate::shared::SPEED_AUDIO_UNKNOWN
                if now.wrapping_sub(FEED_SHARED.dg_load_at.load(Relaxed)) > speed_deadline_ms =>
            {
                notice.speed_fallback = Some("the pipeline never reported its audio");
            }
            _ => {}
        }
    }

    notice
}

/// Playable HLS reserve in the same content-time coordinates as the demux controller. The main
/// thread needs this read for the exact depleted-buffer fallback and for rebuffer diagnostics;
/// ordinary ABR decisions remain on the worker.
pub fn hls_buffered_ms() -> Option<i64> {
    let video = FEED_SHARED.hls_video_tail_ns.load(Acquire);
    if video < 0 {
        return None;
    }
    let audio = FEED_SHARED.hls_audio_tail_ns.load(Acquire);
    let tail = if audio >= 0 { video.min(audio) } else { video };
    let display_base = FEED_SHARED.disp_base.load(Relaxed).max(0);
    Some(
        tail.saturating_add(display_base)
            .saturating_sub(FEED_SHARED.playpos_ns.load(Relaxed).max(0))
            .max(0)
            / 1_000_000,
    )
}

/// The measured period of LG's native position callback, in milliseconds.
///
/// **Measured, not chosen.** The pipeline emits `PF_EVENT_TYPE_FRAMEREADY` at 5 Hz and the
/// device heartbeat reads `vtick=5 vgap=201ms` — unvarying across codec, raster and container,
/// on clean and on visibly stuttering playback alike (`crate::sf_event::sf_on_event`, measured
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
pub const NATIVE_CLOCK_STOPPED_MS: u32 = NATIVE_POSITION_PERIOD_MS * NATIVE_CLOCK_SILENT_PERIODS;

/// When the silence observation was last restarted, in [`crate::shared::vclock_ms`] milliseconds.
///
/// Every state that legitimately stops the native clock — user Pause, an internal rebuffer hold,
/// a stage below `Playing`, a route that is not segmented HLS — restarts it, because otherwise a
/// viewer who paused for a minute would resume straight into an internal hold. Main-thread only;
/// an atomic merely to avoid a `static mut`.
pub static CLOCK_WATCH_SINCE_MS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Everything that stops the clock on purpose restarts the silence observation with it.
fn restart_native_clock_watch() {
    CLOCK_WATCH_SINCE_MS.store(crate::shared::vclock_ms(), Relaxed);
}

/// Milliseconds since the native position callback last fired, or `None` if this seek/session
/// epoch has not seen one at all.
///
/// `dg_vpres_at` is zeroed wherever `frames` is — session reset and the post-seek re-count in
/// this file — so a seek starts with no observation rather than with a gap the size of the seek.
/// The [`CLOCK_WATCH_SINCE_MS`] floor does the same for the deliberate holds, which do not clear
/// the stamp.
pub fn native_clock_silence_ms() -> Option<u32> {
    let last_tick = FEED_SHARED.dg_vpres_at.load(Relaxed);
    if last_tick == 0 {
        return None;
    }
    let since = last_tick.max(CLOCK_WATCH_SINCE_MS.load(Relaxed));
    Some(crate::shared::vclock_ms().saturating_sub(since))
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
/// `FEED_TX.paused` and restarts the observation — and an internal hold cannot re-arm, because the
/// hold sets `prime_play`, which returns at the same place.
pub fn native_clock_stopped(silence_ms: Option<u32>, buffered_ms: Option<i64>) -> bool {
    let (Some(silence_ms), Some(buffered_ms)) = (silence_ms, buffered_ms) else {
        return false;
    };
    silence_ms >= NATIVE_CLOCK_STOPPED_MS && buffered_ms <= i64::from(silence_ms)
}

pub fn should_begin_hls_rebuffer(
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

pub fn rebuffer_request_is_current(
    requested: bool,
    requested_tail_ns: i64,
    current_tail_ns: i64,
) -> bool {
    requested && (requested_tail_ns < 0 || current_tail_ns <= requested_tail_ns)
}

/// Stop the INTERNAL A/V clock at a physical reserve boundary while leaving demux and feeding
/// alive. This is intentionally not `FEED_TX.paused`: the viewer did not pause, and the whole point is
/// to keep acquiring until `try_prime` sees enough measured runway to resume smoothly.
fn maybe_begin_hls_rebuffer(segmented_hls: bool, mt: &MainThread, eng: &mut Engine, hooks: &NativeHooks) {
    if !segmented_hls
        || eng.stage < Stage::Playing
        || eng.prime_play
        || FEED_TX.paused.load(Relaxed)
    {
        // Each of these legitimately stops the native clock, so none of them may be read later as
        // a stalled one: a viewer paused for a minute must resume into playback, not into an
        // instant internal hold, and the tick after this hold releases must start counting again.
        restart_native_clock_watch();
        return;
    }
    // Consume atomically: a worker request racing this tick must either be the value returned here
    // or remain set for the next tick, never be erased by a later unconditional store.
    let requested = FEED_SHARED.hls_rebuffer_requested.swap(false, Acquire);
    let requested_tail_ns = FEED_SHARED.hls_rebuffer_request_tail_ns.load(Acquire);
    let current_tail_ns = FEED_SHARED.hls_video_tail_ns.load(Acquire);
    let requested = rebuffer_request_is_current(requested, requested_tail_ns, current_tail_ns);
    let trial_reserve_ms = FEED_SHARED.hls_trial_reserve_ms.load(Acquire);
    let runtime_runway_ms = FEED_SHARED.hls_prime_runway_ms.load(Acquire);
    let buffered_ms = hls_buffered_ms();
    let silence_ms = native_clock_silence_ms();
    let clock_stopped = native_clock_stopped(silence_ms, buffered_ms);
    // A silent clock that does NOT become a hold is the case this gate was blind to for 82 s on
    // the device; say why, once per silent second, so a device log can answer it.
    if silence_ms.is_some_and(|ms| ms >= NATIVE_CLOCK_STOPPED_MS) && !clock_stopped {
        static LAST_SILENT_LINE_MS: std::sync::atomic::AtomicU32 =
            std::sync::atomic::AtomicU32::new(0);
        let now = crate::shared::vclock_ms();
        if now.wrapping_sub(LAST_SILENT_LINE_MS.load(Relaxed)) >= 1_000 {
            LAST_SILENT_LINE_MS.store(now, Relaxed);
            log(&format!(
                "hls: clock silent={}ms buf={}ms vtail={} atail={} disp_base={} pos={} \
                 requested={} stage_playing=1",
                silence_ms.unwrap_or(0),
                buffered_ms.map_or(-1, |ms| ms),
                FEED_SHARED.hls_video_tail_ns.load(Acquire),
                FEED_SHARED.hls_audio_tail_ns.load(Acquire),
                FEED_SHARED.disp_base.load(Relaxed),
                FEED_SHARED.playpos_ns.load(Relaxed),
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
    let Some(pause_token) = FEED_SHARED.prepare_hls_rebuffer_pause() else {
        if requested {
            FEED_SHARED.hls_rebuffer_requested.store(true, Release);
        }
        log("hls: auto-rebuffer pause deferred by clock transition; will retry");
        return;
    };
    let paused = unsafe { (hooks.feed.sink)().pause(mt) };
    match FEED_SHARED.complete_hls_rebuffer_pause(pause_token, paused != 0) {
        HlsPauseCompletion::Accepted => {}
        HlsPauseCompletion::Refused => {
            if requested {
                FEED_SHARED.hls_rebuffer_requested.store(true, Release);
            }
            log("hls: auto-rebuffer pause refused by Starfish; will retry");
            return;
        }
        HlsPauseCompletion::Stale => {
            if requested {
                FEED_SHARED.hls_rebuffer_requested.store(true, Release);
            }
            log("hls: auto-rebuffer pause result was stale; will retry");
            return;
        }
    }
    arm_live_clock_prime(eng);
    // The pause-local proof was armed before sf_pause, so a completed segment in the native-call
    // window is already part of this hold instead of being silently discarded.
    // `silent=` is the only field that separates the two ways this hold is reached: an exact
    // arithmetic zero, or a presentation clock that stopped with a remnant it will never spend.
    log(&format!(
        "hls: auto-rebuffer pause buf={}ms trial_reserve={}ms runway={}ms silent={}ms",
        buffered_ms.unwrap_or(-1),
        trial_reserve_ms,
        runtime_runway_ms,
        silence_ms.map_or(-1, i64::from),
    ));
}


/// Ticks `FEED_SHARED.frames` must have reached before the ACB bind (`setMediaVideoData`) may be sent.
/// The pipeline's sink is bound to MAIN once the decoder produces output (the order the original
/// C engine documented: "once the decoder is producing frames so the videosink is bound to
/// MAIN"), which is why a normal start waits for a second tick (about 400 ms of playback). While
/// a paused preroll is open the pipeline is playing ONE picture for the viewer and every tick it
/// waits is a tick of audible playback, so one tick (the decoder demonstrably running) is enough.
fn acb_bind_frames_needed(preroll_open: bool) -> i32 {
    if preroll_open {
        1
    } else {
        2
    }
}


/// Bind the native plane, hold a depleted segmented feed, and send available access units.
/// Common route policy supplies immutable source facts and receives only the state needed for
/// its HUD and terminal decision after physical work completes.
pub fn tick_presentation(feed: &mut FeedResources, mt: &MainThread,
    segmented_hls: bool, immersive: bool) -> PresentationTick {
    let hooks = feed.hooks();
    let eng = feed.engine().expect("presentation tick belongs to live feed");
    let stream = matches!(eng.source, Source::Stream);
    // ---------- ACB bind, Kodi/ss4s order: setSinkType(MAIN)+setMediaId+setState(LOADED) ----------
    if eng.stage == Stage::Playing && hooks.release.plane.handle() {
        let id = FEED_SHARED.media_id.lock().unwrap().clone();
        // One binding at a time, across backends: a bind waits for the previous release to
        // be proven (the firmware's completion callback), pumping the plane owner each tick it
        // waits, and goes ahead anyway once that release has failed or timed out (`Rebind`), so a
        // firmware that never completes still gets its picture. A deferred tick leaves the stage
        // at Playing, exactly as a media id that has not arrived yet does.
        let granted = id.as_ref().is_some_and(|id| {
            use plx_platform::tv::plane::{BindPolicy, BindVerdict, PlaneHandle};
            matches!(hooks.release.plane.bind((hooks.feed.sink)(), mt, PlaneHandle::Feed, BindPolicy::Rebind,
                &id.to_string_lossy()),
                BindVerdict::Granted)
        });
        if let Some(id) = id.filter(|_| granted) {
            unsafe { (hooks.feed.sink)().plane_bind(mt, id.as_ptr()) };
            log(&format!("SMP ACB bound id={}", id.to_string_lossy()));
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
            // now readable rather than remembered (see `acb_send_atmos` in `src/video_plane.c` for
            // the addresses). The rule's stated consequence, `SOUND_ERROR_019`, is a literal that
            // exists in NO library on this device.
            //
            // Ran behind `/tmp/plxnative-atmosacb` first, then measured, then made the default:
            // `rv=1` accepted, 1600 audio AUs fed with `reply=O` and no error of any kind, and the
            // television's own read-out — "Dolby Vision / Dolby Atmos", both lines — photographed
            // in a DISPLAY capture at 11 s. `/tmp/plxnative-noatmosacb` is the way back out.
            if immersive && !plx_base::devtrig::flag("noatmosacb") {
                let rv = unsafe { (hooks.feed.sink)().plane_send_atmos(mt, id.as_ptr()) };
                log(&format!("atmos: acb setMediaAudioData rv={rv}"));
            }
            eng.stage = Stage::Bound;
        }
    }

    // ---------- send the WHOLE sourceInfo envelope once frames flow (verbatim, except a
    // container-PQ VP9 stream, see below), then
    // window + PLAYING (setMediaVideoData -> setDisplayWindow -> setState PLAYING) ----------
    if let Some(at) = eng.bound_at_frames {
        // The tick the landing owes after the bind has arrived (or `frames` was reset since).
        if FEED_SHARED.frames.load(Relaxed) != at {
            eng.bound_at_frames = None;
        }
    }
    if eng.stage == Stage::Bound
        && !eng.video_info_sent
        && FEED_SHARED.frames.load(Relaxed) >= acb_bind_frames_needed(FEED_TX.seek_preroll_active())
    {
        let bytes = FEED_SHARED.source_info.lock().unwrap().clone();
        if let Some(mut bytes) = bytes {
            // A VP9 stream's PQ lives in the container, which the pipeline never sees, so its own
            // envelope says "hdrType":"none". Say HDR10 here when the demuxer found PQ (see
            // `vp9_hdr`). `/tmp/plxnative-novp9hdr` sends the envelope as the pipeline made it.
            let container_hdr = *FEED_SHARED.container_hdr.lock().unwrap();
            if let Some(h) = container_hdr {
                if !plx_base::devtrig::flag("novp9hdr") {
                    if let Some(rewritten) = crate::vp9_hdr::rewrite_source_info(&bytes, &h) {
                        log(&crate::vp9_hdr::log_line(&h));
                        bytes = rewritten;
                    }
                }
            }
            let rv = unsafe { (hooks.feed.sink)().plane_send_video_data(mt, bytes.as_ptr() as *const c_char) };
            log(&format!(
                "setMediaVideoData rv={rv} frames={}",
                FEED_SHARED.frames.load(Relaxed)
            ));
            if rv != -1 {
                // -1 = client-side isJsonError reject; else accepted
                eng.video_info_sent = true;
                unsafe { (hooks.feed.sink)().plane_start(mt, 0, 0, 1920, 1080) };
                eng.stage = Stage::Streaming;
                eng.bound_at_frames = Some(FEED_SHARED.frames.load(Relaxed));
                // A displayed count taken before this point was taken against an unconnected plane.
                eng.shared.forget_displayed_before_bind();
                // `acb_start` sets ACB PLAYING. Under a pipeline that is really paused (a late
                // tick bound the plane after the re-pause; an open preroll is a PLAYING pipeline,
                // and its re-pause mirrors the pause itself) ACB must follow it, or it is left
                // PLAYING over a paused pipeline and nothing ever tells it otherwise.
                if FEED_TX.paused.load(Acquire) && !FEED_TX.seek_preroll_active() {
                    unsafe { (hooks.feed.sink)().plane_pause(mt) };
                    log("setMediaVideoData sent under a paused pipeline → ACB PAUSED");
                }
                log("setMediaVideoData sent → window+PLAYING");
            }
        }
    }

    // ---------- webOS 5+ (VP_EXPORTED): correct the placement if the real frame size only
    // became known after Load. The demuxer publishes the coded size when it opens the stream,
    // which races the load thread — so the placement above may have used the fallback. Re-placing
    // costs one call, and Kodi re-places on every render-area change anyway.
    // ----------
    if eng.stage >= Stage::Playing && eng.placed_src != (0, 0) {
        let now = FEED_SHARED.video_raster();
        if now.0 > 0 && now.1 > 0 && now != eng.placed_src {
            place_exported(mt, eng, hooks);
        }
    }

    // ---------- feed AUs after the initial Load has reached Playing. The native clock may itself
    // be Running or Paused: runtime rebuffer and ResumePrime deliberately fill both lanes before
    // issuing Play. NOT while a seek is armed: on a resume the seek is armed before PLAYING, so
    // feeding first would present the file start for a frame before the seek repositions — a
    // visible jump. ----------
    maybe_begin_hls_rebuffer(segmented_hls, mt, eng, hooks);
    if eng.stage >= Stage::Playing && FEED_TX.feed_allowed() && FEED_TX.seek_to_ns.load(Relaxed) < 0 {
        if stream {
            // Two-lane feed, then the prime attempt — the ordering and the reason both live in
            // `feed_both_lanes`, so this call site cannot drift from them.
            plx_base::diag::spans::span("feed", || feed_both_lanes(mt, eng, &FEED_TX, &hooks.feed));
        } else {
            plx_base::diag::spans::span("feed", || feed_sample(mt, eng, hooks.feed.sink));
        }
    }

    PresentationTick { stage: eng.stage, eos_pushed: eng.eos_pushed }
}

/// A seek was requested (`FeedCommand::Seek`): the stores the player's `request_seek` used to make
/// itself, in the same order. The target is published before a worker can observe the seek flag or
/// the request. The feed's copy of the pending target only ever holds the newest one; the player's
/// readout counts how many requests it folded (`coalesced=`).
pub fn seek_requested(epoch: u32, target_ns: i64, display_ns: i64) {
    FEED_SHARED.pipeline_eos.store(false, Relaxed);
    FEED_SHARED.seek_display_ns.store(display_ns, Relaxed);
    FEED_SHARED.seek_epoch.store(epoch, Relaxed);
    FEED_SHARED.seeking.store(true, Release);
    FEED_TX.seek_to_ns.store(target_ns, Relaxed);
}

/// The player took the pending seek (`FeedCommand::SeekTaken`): the feed's copy goes with the
/// player's, unconditionally, so a target the player no longer holds can never leave the feed gate
/// shut (an armed copy blocks every AU, and the transcode path takes it a frame before its flight
/// lands). The store is the one the pump's own take made.
pub fn seek_taken() {
    FEED_TX.seek_to_ns.store(-1, Relaxed);
}

/// The player put a taken seek back (`FeedCommand::SeekRequeued`): only the feed's copy of the
/// pending target, with the release the pump's own re-arm used. Nothing was requested again, so the
/// in-flight flag, the display target and the pipeline's end of stream are not touched.
pub fn seek_requeued(target_ns: i64) {
    FEED_TX.seek_to_ns.store(target_ns, Release);
}

/// The next open starts at `target_ns` (`FeedCommand::ArmOpen`).
pub fn arm_open(target_ns: i64, disp_base: i64, playpos_ns: i64) {
    FEED_SHARED.arm_open(target_ns, disp_base, playpos_ns);
}

/// A transcode restarted at a content offset (`FeedCommand::Rebase`).
pub fn rebase(playpos_ns: i64, disp_base: i64) {
    FEED_SHARED.rebase(playpos_ns, disp_base);
}

/// The player's audio choice (`FeedCommand::SelectAudio`): what the demuxer feeds from its next open.
pub fn select_audio(idx: i32) {
    FEED_SHARED.select_audio(idx);
}

/// The seek was given up on (`FeedCommand::AbandonSeek`): the spinner and the frozen playhead end
/// with it. `seek_display_ns` goes back to `-1`, the HUD's "no seek target".
pub fn seek_abandoned() {
    FEED_SHARED.seeking.store(false, Relaxed);
    FEED_SHARED.seek_display_ns.store(-1, Relaxed);
}

/// One sample of the physical feed callback mailboxes without route or session access: the sparse
/// facts and the raw ones, for the player to ingest addressed to the attempt it installed. The
/// caller is `FeedBackend::sample`, which answers only for a live session and adds the Engine's
/// Load-side facts (`FeedRaw::load`, [`LoadRaw::IDLE`] here); a feed with no session answers its
/// idle mailboxes.
///
/// The failure flags are loaded ONCE, first, and both halves are built from that one load: a
/// worker publishes the code that explains a failure (`dg_cb_err_num`, `dg_http_status`) before the
/// `Release` store of the flag, so a reader that loads the flag with `Acquire` and the code after it
/// can never see a flag without its code, and the mapped and raw halves never disagree about it.
pub fn native_feed_facts() -> FeedFacts {
    let failure = failure_sample();
    FeedFacts { attempt: attempt_facts(failure), raw: native_feed_raw(failure) }
}

/// A feed's failure flags and what explains them, in the only order that is consistent: the flags
/// (`Acquire`, pairing with their writers' `Release`), then the codes they were published after.
#[derive(Clone, Copy)]
struct FailureSample {
    flags: FailureFlags,
    cb_err_num: i32,
    http_status: i32,
    probe_actual_video: u8,
    probe_actual_audio: u8,
    native_audio_refusals: u32,
}

fn failure_sample() -> FailureSample {
    let flags = FailureFlags {
        load_failed: FEED_SHARED.load_failed.load(Acquire),
        load_timed_out: FEED_SHARED.load_timed_out.load(Acquire),
        demux_failed: FEED_SHARED.demux_failed.load(Acquire),
        demux_io_failed: FEED_SHARED.demux_io_failed.load(Acquire),
    };
    FailureSample {
        flags,
        cb_err_num: FEED_SHARED.dg_cb_err_num.load(Relaxed),
        http_status: FEED_SHARED.dg_http_status.load(Relaxed),
        probe_actual_video: FEED_SHARED.probe_actual_v.load(Relaxed),
        probe_actual_audio: FEED_SHARED.probe_actual_a.load(Relaxed),
        native_audio_refusals: FEED_SHARED.native_audio_refusals.load(Acquire),
    }
}

/// The values whose sparse `AttemptFacts` form would lose what a decision reads (the contract's
/// `feed` module audits each member). Plain loads of the mailboxes, in the orderings their writers
/// publish with; the failure flags and their companions are `failure`'s.
fn native_feed_raw(failure: FailureSample) -> FeedRaw {
    FeedRaw {
        playpos_ns: FEED_SHARED.playpos_ns.load(Relaxed),
        frames: FEED_SHARED.frames.load(Relaxed),
        flags: failure.flags,
        decoder_revocations: FEED_SHARED.decoder_revocations.load(Acquire),
        native_audio_refusals: failure.native_audio_refusals,
        file_size: FEED_SHARED.file_size.load(Relaxed),
        applied_rate_permille: FEED_SHARED.applied_rate_permille.load(Relaxed),
        speed_audio_ok: match FEED_SHARED.speed_audio_verdict.load(Relaxed) {
            SPEED_AUDIO_OK => Some(true),
            SPEED_AUDIO_BAD => Some(false),
            _ => None,
        },
        vpres_count: FEED_SHARED.dg_vpres_ct.load(Relaxed),
        pts_shift: FEED_SHARED.pts_shift.load(Relaxed),
        pipeline_eos: FEED_SHARED.pipeline_eos.load(Relaxed),
        seek_landed_epoch: FEED_SHARED.seek_landed_epoch.load(Acquire),
        seek_target_ns: Some(FEED_SHARED.seek_target_ns.load(Relaxed)).filter(|&t| t >= 0),
        // The epoch is the publication point of the requeue: its target is stored first.
        seek_requeue: {
            let epoch = FEED_SHARED.seek_requeue_epoch.load(Acquire);
            SeekRequeue { epoch, target_ns: FEED_SHARED.seek_requeue_target.load(Relaxed) }
        },
        cb_err_num: failure.cb_err_num,
        http_status: failure.http_status,
        probe_actual_video: failure.probe_actual_video,
        probe_actual_audio: failure.probe_actual_audio,
        diag: native_feed_diag(),
        vpres_gap_sum_ms: FEED_SHARED.dg_vpres_gap_sum.load(Relaxed),
        vpres_gap_count: FEED_SHARED.dg_vpres_gap_ct.load(Relaxed),
        disp_base: FEED_SHARED.disp_base.load(Relaxed),
        // The Engine's own fields are not statics: `FeedBackend::sample` fills this half.
        load: LoadRaw::IDLE,
        abr: DemuxAbr {
            mode: FEED_SHARED.dg_abr_mode.load(Relaxed),
            kbps: FEED_SHARED.dg_abr_kbps.load(Relaxed),
            declared_kbps: FEED_SHARED.dg_abr_declared_kbps.load(Relaxed),
        },
        side_reader_active: crate::side_reader::active(),
        side_failures: crate::side_reader::failures(),
        seek_mode: crate::seek::seek_mode(),
        side_exact_delta_ns: crate::side_reader::exact_delta_ns(),
        native_audio: crate::ff::native_audio::fed()
            .map(|(kind, channels)| plx_playback_contract::feed::NativeAudio { codec: kind.ffmpeg_name(), channels }),
    }
}

/// The feed-written values the Stats-for-nerds read-out shows ([`FeedDiag`]): plain `Relaxed` loads
/// of the mailboxes the read-out used to read itself, each by its own name. Their writers are the
/// pump's publish ([`publish_diag`]), the placement, the Load's payload, the pipeline's callbacks
/// and the demux's byte counter; the session's reset returns every one to [`FeedDiag::IDLE`].
fn native_feed_diag() -> FeedDiag {
    let (fed_v, fed_a) = fed_totals();
    FeedDiag {
        place_rv: FEED_SHARED.dg_place_rv.load(Relaxed),
        placed_w: FEED_SHARED.dg_placed_w.load(Relaxed),
        placed_h: FEED_SHARED.dg_placed_h.load(Relaxed),
        load_completed: FEED_SHARED.load_completed.load(Relaxed),
        cb_count: FEED_SHARED.dg_cb_count.load(Relaxed),
        aq_video: FEED_SHARED.dg_aq_video.load(Relaxed),
        aq_audio: FEED_SHARED.dg_aq_audio.load(Relaxed),
        fed_v_pts: FEED_SHARED.dg_fed_v_pts.load(Relaxed),
        fed_a_pts: FEED_SHARED.dg_fed_a_pts.load(Relaxed),
        load_v: FEED_SHARED.dg_load_v.load(Relaxed),
        load_a: FEED_SHARED.dg_load_a.load(Relaxed),
        feed_state: FEED_SHARED.dg_feed_state.load(Relaxed),
        cb_err: FEED_SHARED.dg_cb_err.load(Relaxed),
        cb_err_at: FEED_SHARED.dg_cb_err_at.load(Relaxed),
        net_rx: FEED_SHARED.dg_net_rx.load(Relaxed),
        load_at: FEED_SHARED.dg_load_at.load(Relaxed),
        frame_at: FEED_SHARED.dg_frame_at.load(Relaxed),
        video_fps_milli: FEED_SHARED.video_fps_milli.load(Relaxed),
        fed_v,
        fed_a,
        pushed_any: crate::ff::pushed_any(),
    }
}

/// The player noted a failure ([`plx_playback_contract::feed::FeedCommand::NoteFailure`]): the flag
/// goes up in the feed's own mailbox, with the `Release` the callbacks' stores use, so it comes back
/// through the next [`native_feed_facts`] as any callback's would.
pub fn note_failure(flag: FailureFlag) {
    let cell = match flag {
        FailureFlag::LoadFailed => &FEED_SHARED.load_failed,
        FailureFlag::LoadTimedOut => &FEED_SHARED.load_timed_out,
        FailureFlag::DemuxFailed => &FEED_SHARED.demux_failed,
        FailureFlag::DemuxIoFailed => &FEED_SHARED.demux_io_failed,
    };
    cell.store(true, Release);
}

/// The native stop retains the exact Engine while its workers join, so it may capture one last
/// addressed mailbox snapshot after they finish and before teardown clears the process mailboxes.
pub fn native_attempt_facts() -> plx_playback_contract::readout::AttemptFacts {
    attempt_facts(failure_sample())
}

fn attempt_facts(failure_sample: FailureSample) -> plx_playback_contract::readout::AttemptFacts {
    let reported_frame = FEED_SHARED.seen_frame.load(Relaxed);
    let position = FEED_SHARED.playpos_ns.load(Relaxed);
    let duration = FEED_SHARED.duration_ns.load(Relaxed);
    let (width, height) = FEED_SHARED.video_raster();
    let (aspect_num, aspect_den) = FEED_SHARED.video_aspect_pair();
    let stage = FEED_SHARED.dg_stage.load(Relaxed);
    let pipeline_error = failure_sample.cb_err_num;
    let http_status = failure_sample.http_status;
    let demux_exit = FEED_SHARED.dg_demux_exit.load(Relaxed);
    let declared_kbps = FEED_SHARED.dg_abr_declared_kbps.load(Relaxed);
    let transport_rc = FEED_SHARED.dg_transport_rc.load(Relaxed);
    let transfer_age_ms = FEED_SHARED.dg_transport_age_ms.load(Relaxed);
    let flags = failure_sample.flags;
    let failure = runtime_failure(
        flags.demux_failed, flags.demux_io_failed, flags.load_failed, flags.load_timed_out,
    );
    let requested_kbps = FEED_SHARED.dg_abr_kbps.load(Relaxed);
    let facts = AttemptFacts {
        started: Some(FEED_TX.started.load(Relaxed)),
        playing_observed: None,
        ended: None, // the player's readout owns the end of stream (`PlayerReadout::ended`)
        seek_pending_ns: None, // the pending seek and its display target are the player's own (`PlayerReadout`)
        rebuffering: Some(FEED_SHARED.hls_rebuffering.load(Relaxed)),
        seek_display_ns: None,
        status: None, // ... and the derived UI state (`PlayerReadout::state`)
        requested_kbps: (requested_kbps > 0).then_some(requested_kbps),
        refusal_path: Some(FEED_SHARED.refusal_path()),
        runtime_failure: (failure != RuntimeFailure::Unknown).then_some(failure),
        no_video: FEED_SHARED.demux_no_video.load(Relaxed).then_some(true),
        // a reset zeroes the playhead; before the first frame only a seeded open (`arm_open`,
        // `rebase`) or a seek moves it, so a non-zero value is a target the HUD should show
        position_ns: (reported_frame || position > 0).then_some(position.max(0)),
        duration_ns: (duration > 0).then_some(duration),
        playable_buffer_ms: playable_buffer_ms(
            FEED_SHARED.hls_video_tail_ns.load(Relaxed),
            FEED_SHARED.hls_audio_tail_ns.load(Relaxed),
            FEED_SHARED.dg_load_a.load(Relaxed) != 0,
            FEED_SHARED.disp_base.load(Relaxed),
            position,
        ),
        reported_frame: reported_frame.then_some(true),
        seen_frame: Some(reported_frame),
        frames: reported_frame.then_some(FEED_SHARED.frames.load(Relaxed)),
        raster: (width > 0 && height > 0).then_some((width, height)),
        display_aspect: (aspect_num > 0 && aspect_den > 0).then_some((aspect_num, aspect_den)),
        pipeline_stage: (stage > 0).then_some(stage),
        pipeline_error: (pipeline_error != 0).then_some(pipeline_error),
        http_status: (http_status > 0).then_some(http_status),
        demux_exit: (demux_exit != 0).then_some(demux_exit),
        declared_kbps: (declared_kbps > 0).then_some(declared_kbps),
        // The adaptive worker's measurements are not the feed's: the player fills these two from
        // its readout (`AbrReadout`), which the worker ships.
        media_kbps: None,
        abr_buffer_ms: None,
        transport_rc: (transport_rc != 0).then_some(transport_rc),
        transfer_age_ms: (transfer_age_ms > 0).then_some(transfer_age_ms),
        transport_reopen_failed: FEED_SHARED.dg_transport_reopen_failed.load(Relaxed).then_some(true),
        // The teardown's own ingestion records it (`NativeStop::load_abandoned`): no live session
        // has a parked Load.
        load_abandoned: None,
    };
    facts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reload's seeded target is the attempt's position until its first frame; the HUD drew
    /// none as 0:00.
    #[test]
    fn a_seeded_open_reports_its_target_before_the_first_frame() {
        let _serial = plx_base::testlock::serial();
        FEED_SHARED.reset_session();
        assert_eq!(native_attempt_facts().position_ns, None, "a fresh session has no position yet");

        rebase(120_000_000_000, 120_000_000_000);
        assert_eq!(native_attempt_facts().position_ns, Some(120_000_000_000),
            "a transcode restarted at 120 s reads 120 s until its first frame");

        FEED_SHARED.reset_session_for_reload();
        assert_eq!(native_attempt_facts().position_ns, None, "a reload forgets the old seed");
        arm_open(90_000_000_000, 0, 90_000_000_000);
        assert_eq!(native_attempt_facts().position_ns, Some(90_000_000_000),
            "a reload armed at 90 s reads 90 s until its first frame");
        FEED_SHARED.reset_session();
    }
}
