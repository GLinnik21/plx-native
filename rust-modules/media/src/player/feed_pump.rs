//! Physical buffer-feed tick operations. The common pump owns route/recovery decisions and
//! maps these native results to its attempt trace.

use super::engine::Engine;
use super::feed_resources::FeedResources;
use super::{ACB_OK, SHARED, TX};
use super::shared::Stage;
use super::shared::{HlsPauseCompletion, HlsPrimeKind, HlsSeekPause};
use super::SeekMode;
use plx_base::task::MainThread;
use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use std::time::Duration;

pub(super) struct LoadGate {
    pub native_epoch: u32,
    pub verdict: LoadGateVerdict,
}

pub(super) enum LoadGateVerdict {
    Returned,
    Deferred,
    TimedOut(Duration),
    LostEpoch,
}

/// The native Load's issued→returned gate. The worker's epoch, not a later route choice,
/// authorizes the result. A timeout publishes the existing callback failure latches and lets
/// the common pump attach the appropriate trace class to its current route attempt.
pub(super) fn load_gate(epoch: u32, budget: Duration) -> LoadGate {
    if SHARED.native_load_returned(epoch) {
        return LoadGate { native_epoch: epoch, verdict: LoadGateVerdict::Returned };
    }
    if SHARED.native_load_deferred_logged_epoch.swap(epoch, Relaxed) != epoch {
        super::log("native: loadCompleted while Load in flight — deferring");
    }
    let verdict = match SHARED.native_load_elapsed(epoch) {
        Some(elapsed) if elapsed >= budget => {
            super::log(&format!(
                "native: Load did not return within {}s — publishing load_failed",
                budget.as_secs()
            ));
            SHARED.load_failed.store(true, Release);
            SHARED.load_timed_out.store(true, Release);
            LoadGateVerdict::TimedOut(elapsed)
        }
        Some(_) => LoadGateVerdict::Deferred,
        // A synchronous UnloadCompleted can retire the native epoch while Load is deferred.
        // The gate can never open then, so surface the same failure instead of waiting forever.
        None => {
            super::log(
                "native: Load epoch left Active while loadCompleted was still deferred — \
                 publishing load_failed",
            );
            SHARED.load_failed.store(true, Release);
            SHARED.load_timed_out.store(true, Release);
            LoadGateVerdict::LostEpoch
        }
    };
    LoadGate { native_epoch: epoch, verdict }
}

/// Whether native construction is callable yet. A blocked Load exposes its exact epoch only
/// while the live non-preview Engine still owes the Load budget check.
pub(super) enum CtorGate {
    Ready,
    Awaiting { budget_epoch: Option<u32> },
}

pub(super) fn ctor_gate(feed: &mut FeedResources, mt: &MainThread) -> CtorGate {
    let Some(eng) = feed.engine() else { return CtorGate::Awaiting { budget_epoch: None }; };
    if unsafe { super::sink().ready(mt) } != 0 { return CtorGate::Ready; }
    CtorGate::Awaiting {
        budget_epoch: (eng.stage == Stage::Loading && !eng.preview_abandon).then_some(eng.native_epoch),
    }
}

/// Native seek reopen watchdog. A closed feed gate cannot make progress, so its clock is reset
/// without consuming the route owner's pending target.
pub(super) fn stuck_seek_due(feed: &mut FeedResources, now: u32, feed_allowed: bool,
    route_flight: bool, budget_ms: u32) -> bool {
    let Some(eng) = feed.engine() else { return false; };
    if eng.flushed && eng.rebase_pending && !feed_allowed {
        eng.seek_armed_at = now;
    }
    eng.flushed && eng.rebase_pending
        && now.wrapping_sub(eng.seek_armed_at) > budget_ms
        && !route_flight
}

pub(super) enum StuckSeekAction { Retried(i32), Reload }

/// Re-arm only the native demux target after common has committed any newer route seek.
pub(super) fn retry_stuck_seek(feed: &mut FeedResources, now: u32, target_ns: i64,
    can_retry: bool) -> StuckSeekAction {
    let eng = feed.engine().expect("stuck seek belongs to live feed");
    if eng.seek_retries < 2 && can_retry {
        eng.seek_retries += 1;
        SHARED.seek_to_ns.store(target_ns, Release);
        SHARED.seek_target_ns.store(target_ns, Relaxed);
        eng.seek_armed_at = now;
        StuckSeekAction::Retried(eng.seek_retries)
    } else {
        StuckSeekAction::Reload
    }
}

/// Eligibility for applying a pending direct-play/transcode seek on this physical feed.
pub(super) fn pending_seek_ready(feed: &FeedResources, seek_reloads: bool) -> bool {
    feed.pending_seek_ready(seek_reloads)
}

pub(super) enum SeekPause { Ready, Refused, Stale, Busy }

/// One native Pause transition before an in-place seek. The common route owner chooses any
/// fallback reload from this typed verdict.
pub(super) fn prepare_seek_pause(mt: &MainThread) -> SeekPause {
    match SHARED.prepare_seek_pause() {
        Some(HlsSeekPause::AlreadyHeld) => SeekPause::Ready,
        Some(HlsSeekPause::Issue(token)) => {
            let accepted = unsafe { super::sink().pause(mt) } != 0;
            match SHARED.complete_seek_pause(token, accepted) {
                HlsPauseCompletion::Accepted => SeekPause::Ready,
                HlsPauseCompletion::Refused => SeekPause::Refused,
                HlsPauseCompletion::Stale => SeekPause::Stale,
            }
        }
        None => SeekPause::Busy,
    }
}

pub(super) fn begin_seek_discontinuity(feed: &FeedResources) -> bool {
    feed.begin_seek_discontinuity()
}

/// Place the webOS 5+ exported window at the native Load's completion, before decoded frames.
/// `src` is the coded size, not the 1080p canvas. The coded size becomes known later; the caller
/// re-places once the demuxer publishes it. Record the place result for diagnostics: before this
/// mirror, the panel falsely said "not placed" throughout a successful exported-window session.
pub(super) fn place_exported(mt: &MainThread, eng: &mut Engine) {
    if super::sink().window_mode() != super::VP_EXPORTED { return; }
    let (w, h) = SHARED.video_raster();
    let src = if w > 0 && h > 0 { (w, h) } else { (1920, 1080) };
    let rv = unsafe { super::sink().place_window(mt, src.0, src.1, 0, 0, 1920, 1080) };
    eng.placed_src = src;
    SHARED.dg_place_rv.store(rv, Relaxed);
    SHARED.dg_placed_w.store(src.0, Relaxed);
    SHARED.dg_placed_h.store(src.1, Relaxed);
    super::log(&format!(
        "vplane: exported window placed src={}x{} rv={rv}",
        src.0, src.1
    ));
}

#[derive(Default)]
pub(super) struct PausedSeekStep {
    pub finish: bool,
    #[cfg(test)]
    pub mid_seek_claimed: bool,
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
///   [`super::finish_paused_seek`] (on the ACB route only once the plane is bound and a tick has
///   followed the bind: [`plane_shows_the_picture`]). A refused re-pause leaves the preroll and its ownership in
///   place, so the next tick retries; the film plays only for that long.
///
/// `playpos + 15 s >= target` is the guard the loop's old re-pause had: a presented frame far
/// before the target is not the landing. Runs before `pa` is split, because the re-pause takes the
/// whole adapter.
pub(super) fn paused_seek_step(
    feed: &mut FeedResources,
    now: u32,
    route_flight: bool,
    claim_owns_pause: bool,
) -> PausedSeekStep {
    let mut step = PausedSeekStep::default();
    if TX.paused.load(Acquire)
        && !TX.seek_preroll_active()
        && SHARED.seeking.load(Relaxed)
        && !route_flight
        && !claim_owns_pause
    {
        let target = SHARED.seek_target_ns.load(Relaxed);
        let target = if target >= 0 { target } else { SHARED.playpos_ns.load(Relaxed) };
        // Only a seek the pump already DISPATCHED and that is still in flight: no newer tap waiting
        // (`request_seek` sets `SHARED.seeking` and `TX.seek_to_ns` together, ahead of the
        // dispatcher, and the dispatcher's `own_seek_under_pause` owns THAT seek at its own target),
        // and the Engine flushed or awaiting its keyframe's rebase.
        if let Some(eng) = feed.engine().filter(|e| TX.seek_to_ns.load(Relaxed) < 0 && (e.flushed || e.rebase_pending)) {
            // The Pause marked the clock the seek prime owns as the viewer's, which fences the
            // prime's Play: give the hold back to the prime first (it Plays one frame, then
            // `finish_paused_seek` re-pauses).
            if SHARED.hand_hold_to_seek_prime() && TX.own_paused_seek(target) {
                eng.seek_armed_at = now;
                #[cfg(test)]
                #[cfg(test)]
                { step.mid_seek_claimed = true; }
                super::log("seek: viewer paused mid-seek; the pump opened the preroll");
            }
        }
    }
    // A session with no picture under a viewer Pause. A reload keeps the Pause and zeroes `frames`,
    // the fresh Engine arms an Initial hold the Pause marks user-held, and the feed gate
    // (`TX.feed_allowed`) stays shut: no keyframe is fed, nothing is ever presented, and the HUD
    // reads Buffering over a blank plane until Play. Derived from the state, so it covers every
    // route that reloads (retranscode, adaptive, native audio, Original, recovery landings, a
    // Pause pressed during a claim flight or before the first frame) without a call at each site:
    // open the same bounded preroll a paused seek gets. Once a frame is presented `frames >= 1`
    // closes this branch for good and the landed check below re-pauses.
    if TX.paused.load(Acquire)
        && !TX.seek_preroll_active()
        && !claim_owns_pause
        && !route_flight
        && TX.seek_to_ns.load(Relaxed) < 0
        && SHARED.frames.load(Relaxed) == 0
        // A prime still owed: only then does the seek hold hand back a Play. Once the prime's Play
        // has fired (`prime_play` cleared, the clock Running) a Pause before the first position
        // update is a plain `Held(User)`; converting it would clear `user_held` with nothing left
        // to Play, and the viewer's Play would then find no hold to release.
        && feed.engine().is_some_and(|e| e.prime_play)
        && SHARED.hand_hold_to_seek_prime()
    {
        // Owned at 0, not at `playpos_ns`: `frames` is per-session (zeroed by every reset), so the
        // first presented frame IS this session's landing, while `playpos_ns` is the PRE-reload
        // playhead (a reload does not rewrite it). A target taken from it makes the `+ 15 s >=
        // target` guard fail for a reload that lands behind the old playhead, and the film then
        // plays on under a Paused transport.
        if TX.own_paused_seek(0) {
            super::log("reload: viewer paused with no picture yet; the pump opened the preroll");
        }
    }
    let Some(target) = TX.owned_paused_seek() else { return step };
    let Some(eng) = feed.engine() else { return step };
    let landed = !eng.flushed
        && !eng.rebase_pending
        && TX.seek_to_ns.load(Relaxed) < 0
        && !route_flight
        && SHARED.frames.load(Relaxed) >= 1
        && plane_shows_the_picture(eng)
        && SHARED.playpos_ns.load(Relaxed) + 15 * 1_000_000_000 >= target;
    if landed {
        step.finish = true;
    }
    step
}

/// Is a picture the Engine has presented actually on the panel? Without ACB (webOS 5+) the sink is
/// placed at Load, so any presented tick is. On the ACB route (webOS 4) the video plane connects
/// only through `setMediaVideoData` + `acb_start` (the `Stage::Streaming` transition below), and a
/// tick counted before that is a picture the plane never showed. A paused preroll re-paused on such
/// a tick leaves a black panel (device-measured: a VIDEO-only capture read "no signal" until
/// Resume), because the pause stops the ticks the bind is waiting on. So the landing needs the
/// bind complete AND one tick after it (`Engine::bound_at_frames`).
fn plane_shows_the_picture(eng: &Engine) -> bool {
    if !ACB_OK.load(Relaxed) {
        return true;
    }
    eng.stage >= Stage::Streaming && eng.bound_at_frames.is_none_or(|at| SHARED.frames.load(Relaxed) != at)
}


pub(super) fn publish_diag(
    feed: &mut FeedResources,
    now: u32,
    last_frames: &std::sync::atomic::AtomicI32,
) {
    let Some(eng) = feed.engine() else { return; };
    SHARED.dg_stage.store(eng.stage as u8, Relaxed);
    // Nobody is looking: skip it. `aq_bytes` takes each queue's pthread mutex, and the read-out
    // samples at 2 Hz, so publishing at 60 Hz is 30x more often than anything can observe. Costs
    // no freshness — the loop order is pump → `Diagnostics::update` → `Diagnostics::draw`
    // (`app/run.rs`), so the frame the panel
    // is switched on has already republished.
    if !super::DIAG_READOUT_ON.load(Relaxed) {
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
    // photograph has no time axis; stamping here rather than in `app::diagnostics` is what makes the
    // clock measure the STALL rather than how long the panel has been open.
    let f = SHARED.frames.load(Relaxed);
    if last_frames.swap(f, Relaxed) != f {
        SHARED.dg_frame_at.store(now, Relaxed);
    }
}

/// **`plxnative-refuse601` (dev builds): the pipeline's `type=18 num=601` after the Load returned,
/// once per request.** The refusal webOS 10.3.1 gives a stream it cannot allocate a decoder for
/// (#695, DTS), reproduced on a set that would take the stream: once the Load has returned and
/// before any picture, it publishes through [`super::sf_on_event`], the entry the firmware calls, so
/// the callback's own latches (`dg_cb_err_num`, `load_failed`) and the failed-open ladder see exactly
/// what a real refusal gives them.
///
/// The read is a `devtrig::flag` (a `stat`), and a latched flag could not be armed per test: a
/// process-wide latch hands whichever test ran first its answer for all of them. So the cheap
/// atomic guards come first and the file is read only inside the open window (after the Load
/// returned, before a picture, and for a request not yet refused), never on a playing frame. The
/// latch is written only after the flag holds, so an unarmed request is never marked refused.
pub(super) fn inject_refuse601(
    feed: &mut FeedResources,
    generation: u32,
    preview: bool,
    refused_for: &std::sync::atomic::AtomicU32,
) {
    let Some(eng) = feed.engine() else { return; };
    if generation == 0
        || preview
        || !SHARED.native_load_returned(eng.native_epoch)
        || SHARED.seen_frame.load(Relaxed)
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
    super::log("devtrig refuse601: publishing type=18 num=601 after the Load");
    super::sf_on_event(eng.native_epoch, 18, 601, std::ptr::null());
}

/// The native surround audio watchdog ([`super::native_stall`]): the pipeline took the Load and a few
/// access units of a native Opus track with more than two channels, then stopped
/// accepting with both our queues full and no picture. True when it fired, with the refusal flag
/// raised: the caller hands the audio to the failed-open ladder, whose named write is the learner
/// ([`confirm_refusal`]).
pub(super) fn native_audio_stall_fired(feed: &mut FeedResources, now: u32) -> bool {
    let Some(eng) = feed.engine() else { return false; };
    if eng.preview_abandon { return false; }
    use super::native_stall::{Probe, Step, NATIVE_AUDIO_STALL_MS, SIZES, WATCH};
    let fed = crate::ff::native_audio::fed();
    let queue = |q: &Option<Box<crate::aq::AuQueue>>| q.as_ref().map_or(0, |q| crate::aq::aq_bytes(&**q as *const _ as *mut _) as i64);
    let (qa, qv) = (queue(&eng.aq_audio), queue(&eng.aq_video));
    let accepted = super::engine::fed_totals().1;
    let probe = Probe {
        now,
        channels: fed.map_or(0, |(_, c)| c),
        load_returned: SHARED.native_load_returned(eng.native_epoch),
        frames: SHARED.frames.load(Relaxed),
        accepted,
        audio_queue_bytes: qa,
        video_queue_bytes: qv,
    };
    match WATCH.lock().unwrap_or_else(|e| e.into_inner()).step(&probe) {
        Step::Quiet => false,
        Step::Armed => {
            if let Some((kind, channels)) = fed {
                super::log(&format!(
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
            super::log(&format!(
                "native audio: STALL, no picture: {accepted} AUs accepted, none for {stalled_ms} ms (watching for {armed_ms} ms), \
                 audio queue {qa} B, video queue {qv} B, fed packet sizes {sizes}"
            ));
            SHARED.native_audio_refused.store(true, Release);
            true
        }
    }
}

/// The one event-log line for an era-chosen sink counter that never arrived in a played session
/// ([`super::era_callbacks`]). It only reads counters and logs; nothing branches on it.
pub(super) fn note_missing_era_callback(feed: &mut FeedResources) {
    let Some(eng) = feed.engine() else { return; };
    if eng.preview_abandon { return; }
    use super::era_callbacks::{line, Probe, Step};
    let major = plx_platform::tv::device::info().major;
    let probe = Probe {
        ticks: SHARED.dg_pos_ticks.load(Relaxed),
        displayed: SHARED.dg_sink_displayed_cb.load(Relaxed),
        other_numbering: SHARED.dg_sink_other_cb.load(Relaxed),
        major,
        expected_type: super::displayed_counter_type(major),
    };
    if let Step::Missing(m) = eng.era_watch.step(&probe) {
        super::log(&line(&m));
    }
}

/// Apply native flush and demux rebase only after the route owner committed the seek.
pub(super) fn apply_inplace_seek_pre(feed: &mut FeedResources, mt: &MainThread,
    t: i64, mode: SeekMode, coalesced: u32) {
    let eng = feed.engine().expect("in-place seek belongs to live feed");
        // `NativeSeek` does not flush here: LG's public `Seek` flushes the pipeline itself when
        // `feed_stream` calls it on the landed keyframe, and a `flush()` is itself a seek to 0, so
        // flushing after (or instead of re-anchoring) leaves the stale segment at 0 (#568).
        if mode != SeekMode::NativeSeek {
            unsafe {
                super::sink().flush(mt); // drop decoded/queued frames only after the clock is really held
            }
        }
        super::engine::drain_aq(eng);
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
        // A seek armed under a viewer Pause: the pump opens the preroll and closes it on the landing.
}

/// Finish the physical seek arm after common has opened any viewer-paused preroll.
pub(super) fn apply_inplace_seek_post(feed: &mut FeedResources, t: i64, now: u32) {
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
        eng.prime_play = true; // paused above; Play once PRIME_NS is buffered (no fast-forward)
                               // "no post-seek frame presented yet" — the feed-ahead throttle feeds freely until the first
                               // real presented pts lands, instead of comparing the new fed pts against the STALE pre-seek
                               // presented position (which would wrongly break feeding on a forward in-place seek).
        SHARED.pres_fed.store(super::engine::PRES_NONE, Relaxed);
        eng.feed_pace = super::engine::FeedPace::default(); // the run-on belongs to the pre-seek report
        SHARED.frames.store(0, Relaxed); // count only POST-seek frames (rebind + resume re-pause gate)
                                         // …and forget the last presentation stamp with it. The plane goes dark across a seek by
                                         // design, so the first frame after one would otherwise post a gap the size of the seek and
                                         // read as a stutter (`Shared`'s `dg_vpres_*` block).
        SHARED.dg_vpres_at.store(0, Relaxed);
        SHARED.playpos_ns.store(t, Relaxed); // displayed position jumps; wall clock takes over
}

pub(super) fn prime_before_play(rebase_pending: bool, segmented_hls: bool) -> bool {
    rebase_pending || segmented_hls
}

pub(super) enum LoadTimeout { Elapsed(Duration), LostEpoch }
#[derive(Default)]
pub(super) struct LoadTick {
    pub timeout: Option<LoadTimeout>,
    pub speed_fallback: Option<&'static str>,
}

/// Physical Load/Play transition. Route and report side effects are returned as addressed facts
/// for the common pump to apply before ACB bind and feeding.
pub(super) fn tick_load(feed: &mut FeedResources, mt: &MainThread, now: u32,
    segmented_hls: bool, budget: Duration, speed_deadline_ms: u32) -> LoadTick {
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
    // issue #74 D.1: the gate below requires `SHARED.native_load_returned(eng.native_epoch)`
    // BEFORE this arm may even poll `sf_is_load_completed` — that poll is itself a concurrent
    // Starfish call, and on v0.6.0 it was the FIRST thing this arm did while `sf_load` was still
    // executing (synchronously) on the load thread. See `src/starfish.c`'s `g_load_returned` for
    // the matching seam-side half; `threads::load_thread` flips the Rust side right after the
    // real, blocking `sf_load` call returns. Gated on `!eng.preview_abandon` too, same as the
    // completion arm below: an abandoned trailer-preview Load must not drive any of this — no
    // deferral logging, no budget clock, nothing that reacts to it as if it were live. That is
    // why the guard is checked BEFORE `native_load_gate_ready` is even called: the helper is
    // where the deferral logging and D.1.4 budget/telemetry actually live (see its own doc).
    if eng.stage == Stage::Loading
        && !eng.preview_abandon
        && gate_ready
        && (SHARED.load_completed.load(Relaxed) || unsafe { super::sink().is_load_completed(mt) } != 0)
    {
        // "native: Load returned after Nms" is logged unconditionally at the gate transition
        // itself (`threads::load_thread`, right where `mark_native_load_returned` flips it), not
        // here — see `load-returned-log-is-conditional-on-loadcompleted-and-unbounded-after`. It
        // used to live in this arm, which meant it only ever appeared paired with the deferral
        // line above, and never at all on the path this fix is most likely to be read on: Load
        // eventually returns but `loadCompleted` goes quiet, so this arm never runs.
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
        if super::sink().window_mode() == super::VP_EXPORTED {
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
        if prime_before_play(eng.rebase_pending, segmented_hls) {
            eng.prime_play = true;
            super::log("SMP loadCompleted (priming before Play)");
        } else {
            let generation = SHARED.hls_candidate_generation.load(Acquire);
            let recovery = SHARED.hls_recovery();
            if let Some((token, _)) =
                SHARED.reserve_hls_prime_play(HlsPrimeKind::Fresh, generation, recovery)
            {
                let accepted = unsafe { super::sink().play(mt) } != 0;
                match SHARED.complete_hls_prime_play(token, accepted) {
                    super::shared::HlsPlayCompletion::Accepted { resume_acb } => {
                        if resume_acb {
                            super::acb_mirror_playstate_at(mt, eng.stage, true);
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
    } else if eng.stage == Stage::Loading
        && !eng.preview_abandon
        && SHARED.native_load_returned(eng.native_epoch)
    {
        // Load HAS returned here (the first arm's `native_load_gate_ready` failed) and
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
        match SHARED.native_load_returned_elapsed(eng.native_epoch) {
            Some(elapsed) if elapsed >= budget => {
                super::log(&format!(
                    "native: loadCompleted did not arrive within {}s after Load returned — \
                     publishing load_failed",
                    budget.as_secs()
                ));
                SHARED.load_failed.store(true, Release);
                SHARED.load_timed_out.store(true, Release);
                notice.timeout = Some(LoadTimeout::Elapsed(elapsed));
            }
            Some(_) => {}
            // Same rationale as the `None` arm above: losing `Active` (a synchronous
            // UnloadCompleted for this epoch) while still waiting on `loadCompleted` must fire
            // the timeout rather than silently going quiet forever.
            None => {
                super::log(
                    "native: Load epoch left Active while still waiting on loadCompleted after \
                     Load returned — publishing load_failed",
                );
                SHARED.load_failed.store(true, Release);
                SHARED.load_timed_out.store(true, Release);
                notice.timeout = Some(LoadTimeout::LostEpoch);
            }
        }
    }

    // ---------- playback speed: the safety net ----------
    // A speed session's Load declared PCM. The pipeline reports what it set up (audio-info
    // callback, `player::note_speed_audio_report`); a report that is not the declared 48 kHz
    // stereo, or none within speed_deadline_ms of loadCompleted, means this television
    // does not take app-fed PCM the way the 3.9.3 set does — fall back to 1x rather than play
    // silence or a drifting sound. Video-only Loads (the `noaudio` trigger) have no audio to judge.
    if super::applied_rate_permille() != super::RATE_NORMAL
        && eng.stage >= Stage::Playing
        && !super::no_audio_armed()
        && TX.seek_to_ns.load(Relaxed) < 0
    {
        match SHARED.speed_audio_verdict.load(Relaxed) {
            super::SPEED_AUDIO_BAD => {
                notice.speed_fallback = Some("the pipeline set up different audio than the PCM declared");
            }
            super::SPEED_AUDIO_UNKNOWN
                if now.wrapping_sub(SHARED.dg_load_at.load(Relaxed)) > speed_deadline_ms =>
            {
                notice.speed_fallback = Some("the pipeline never reported its audio");
            }
            _ => {}
        }
    }

    notice
}
