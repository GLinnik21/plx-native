//! Common native route-start orchestration. The route owns the transaction and exact attempt;
//! the feed constructor only runs after admission and source preparation.

use super::{adapter::PlayerAdapter, engine, SHARED};
#[cfg(any(test, feature = "test-support"))]
use super::TX;
use std::sync::atomic::Ordering;
use plx_base::eventlog::log;

/// Start one native Engine and settle any route candidate which was prepared before (or, for a
/// dev fixture, during) this call.  A pre-existing Engine is a successful no-op only when there is
/// no candidate waiting for a *new* Load; otherwise accepting it would falsely settle the new
/// route with the old decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferfeedStartOutcome {
    AlreadyRunning,
    Launched(crate::route::RouteStartAttempt),
    /// The previous object's `Unload` is still running: the start is queued in the release
    /// machine and runs the frame its tail has run (or fails typed at the deadline). Accepted:
    /// the route stays on the Player and the HUD shows Connecting.
    Deferred,
    Failed,
}

impl BufferfeedStartOutcome {
    pub fn accepted(self) -> bool {
        !matches!(self, Self::Failed)
    }
}

pub fn start_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    let outcome = start_bufferfeed_tracked(ps, pa);
    ps.jail_load_blocked || outcome.accepted()
}

/// Start the native Engine while retaining the identity of the exact asynchronous `sf_load`.
/// Foreground recovery uses this to wait for the media-thread result instead of treating thread
/// creation as proof that the television accepted the payload.
pub fn start_bufferfeed_tracked(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> BufferfeedStartOutcome {
    if plx_platform::tv::sandbox::blocks_native_video() {
        ps.jail_load_blocked = true;
        log("start_bufferfeed: refusing native video; affected chassis cannot read /dev/rtkmem");
        if let Some(ticket) = crate::route::begin_route_start() {
            let _ = crate::route::abort_route_start(ticket, crate::route::RouteStartResult::StartFailed);
        }
        return BufferfeedStartOutcome::Failed;
    }
    ps.jail_load_blocked = false;
    ps.native_start_refused = None;
    // A fresh play (not a `reload_at`, which reaches `start_bufferfeed_for` directly) is a new
    // playback: it gets to try the native Seek again.
    if !pa.is_live() {
        super::set_native_seek_refused(false);
    }
    // A URI-mode probe session (dev trigger `plxnative-uriprobe`) holds the route-start attempt and
    // the exported window without an Engine in the slot. Active double-start stays a no-op;
    // closing or quarantined ownership refuses a new start until actual release.
    #[cfg(feature = "devtriggers")]
    if pa.uri_live() {
        log("start_bufferfeed: a URI-mode registration still owns its resources");
        if pa.uri_closing() {
            if let Some(existing) = pa.uri_attempt() {
                if let crate::route::LiveEngineStartRelation::Conflict(transaction) = crate::route::classify_live_engine_start(existing) {
                    let _ = crate::route::abort_route_start(transaction, crate::route::RouteStartResult::StartFailed);
                }
            }
            return BufferfeedStartOutcome::Failed;
        }
        return BufferfeedStartOutcome::AlreadyRunning;
    }
    if let Some(existing) = pa.active_feed_attempt() {
        match crate::route::classify_live_engine_start(existing) {
            crate::route::LiveEngineStartRelation::CurrentAttempt => {
                // The exact Load is already in flight. Return its identity so foreground can
                // observe that same attempt instead of treating an idempotent rediscovery as an
                // untracked Engine and entering an endless AlreadyRunning retry.
                log("start_bufferfeed: following already-running Load attempt");
                return BufferfeedStartOutcome::Launched(existing);
            }
            crate::route::LiveEngineStartRelation::NoPendingRoute => {
                log("start_bufferfeed: already running (no-op)");
                return BufferfeedStartOutcome::AlreadyRunning;
            }
            crate::route::LiveEngineStartRelation::Conflict(ticket) => {
                log("start_bufferfeed: live Engine belongs to another Load attempt");
                let _ = crate::route::abort_route_start(
                    ticket,
                    crate::route::RouteStartResult::StartFailed,
                );
                return BufferfeedStartOutcome::Failed;
            }
        }
    }
    let Some(route_start) = crate::route::begin_route_start() else {
        log(&format!(
            "start_bufferfeed: route reducer refused a start owner (phase={})",
            crate::route::control_phase_label()
        ));
        return BufferfeedStartOutcome::Failed;
    };
    let outcome = start_bufferfeed_for(ps, pa, route_start);
    // The refusal for an outstanding abandoned Load builds no Engine, so no pump will ever publish
    // why the player is empty. Record it where `player::state` derives the read-out from; the start
    // itself stays refused.
    if outcome == BufferfeedStartOutcome::Failed && pa.has_abandoned_load() {
        ps.native_start_refused = Some(crate::route::StartRefusal::LoadTimeout);
    }
    outcome
}

pub(super) fn start_bufferfeed_for(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: crate::route::RouteStartTransaction,
) -> BufferfeedStartOutcome {
    #[cfg(feature = "devtriggers")]
    if pa.uri_live() {
        // Reload/foreground starts reach this seam directly too. URI ownership never queues one.
        let _ = crate::route::abort_route_start(route_start, crate::route::RouteStartResult::StartFailed);
        return BufferfeedStartOutcome::Failed;
    }
    // The last real stop's object may still be inside its off-frame `Unload`. A tail that is ready
    // runs now; one that is not defers this start into the release machine (never a blocking join:
    // PLX-NATIVE-1Q), which starts it the frame the tail has run or refuses it at the deadline.
    if pa.release_busy() {
        engine::poll_release(ps, pa, std::time::Instant::now());
    }
    let mut fx = Vec::new();
    pa.release_step(
        &super::release::ReleaseEvent::StartRequested(super::release::QueuedStart { route_start }),
        &mut fx,
    );
    let mut go = false;
    let mut refused = false;
    for effect in fx {
        match effect {
            super::release::ReleaseEffect::Start(_) => go = true,
            super::release::ReleaseEffect::RefuseStart(q) => {
                refuse_start_for_release(ps, q);
                refused = true;
            }
            super::release::ReleaseEffect::AbortSuperseded(q) => {
                pa.settle_preview_admission(false, false);
                pa.queued_start_dropped();
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            _ => {}
        }
    }
    if refused {
        return BufferfeedStartOutcome::Failed;
    }
    if !go {
        log("start_bufferfeed: the previous Unload is still running; start deferred");
        return BufferfeedStartOutcome::Deferred;
    }
    match start_bufferfeed_inner(ps, pa, route_start) {
        Ok(attempt) => {
            // Every start funnels through here (fresh, reload and the rollback's reload alike), so
            // the refusal's read-out ends at the first start that is NOT refused. Clearing it only
            // in `start_bufferfeed_tracked` left a reload landing after a refused fresh start
            // (the foreground restore's rollback) with an Error over the video it began.
            ps.native_start_refused = None;
            // The request's failed-open ladder rules on this Engine's open too: a Load it did not
            // ask for (the foreground restore after a Home during a recovery flight or over the
            // read-out) takes it out of `Flight` or `Failed`. The recovery landing's own reload has
            // already moved it (`FlightLanded`), so this is a no-op there.
            let _ = ps.open_recovery().step(&super::recovery::RecoveryEvent::LoadStarted, &mut Vec::new());
            BufferfeedStartOutcome::Launched(attempt)
        }
        Err(result) => {
            let _ = crate::route::abort_route_start(route_start, result);
            BufferfeedStartOutcome::Failed
        }
    }
}


pub(super) fn start_bufferfeed_inner(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: crate::route::RouteStartTransaction,
) -> Result<crate::route::RouteStartAttempt, crate::route::RouteStartResult> {
    // The previous object's release is finished or the start was deferred before this point
    // (`start_bufferfeed_for`), so the epoch is retired and the object destroyed already.
    // A timed-out Load from an earlier Engine may still own the one native object the C seam
    // holds. Release it now if it has returned; otherwise refuse before anything below (the
    // exported window in particular) can touch state that object is still using.
    if !engine::begin_native_open(pa) {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    // Guard a double-start: overwriting a live ENGINE slot would DROP the running
    // Engine, detaching its worker threads and freeing the hs/aq boxes those
    // threads still hold raw ptrs into -> use-after-free. If already running, no-op.
    // (Reachable via a PLAY key landing in the WILL->DID foreground window.)
    // Per-SESSION, not per-boot: both the log's every-100th cadence and the diagnostics read-out
    // mean "this playback", and a count carried in from the last item answers neither question.

    let prepared = super::feed_open::prepare_source(ps, route_start)?;
    let url = prepared.url;
    let sample = prepared.sample;
    let is_h265 = prepared.is_h265;
    let stream = sample.is_none();
    // dev: /tmp/plxnative-uriprobe — play this URL through `com.webos.media` (URI mode) instead of
    // building an Engine, to learn what webOS 5+ does with it (issue #1064). Taken AFTER the route
    // is prepared and BEFORE anything native is touched (the clock and session guards, the exported
    // window, the payload), and never for a preview or a local sample: the probe needs a real stream
    // URL and the foreground play which owns the screen. The content names the variant; text which
    // names none leaves the probe unarmed and the play normal rather than running a variant nobody
    // asked for.
    #[cfg(feature = "devtriggers")]
    if stream && !crate::route::is_preview(ps) {
        if let Some(text) = plx_base::devtrig::read("uriprobe") {
            match super::uriprobe::Variant::from_trigger(&text) {
                Some(variant) => {
                    // The probe takes no native session of its own, but its window verbs reach the
                    // same seam, so it refuses like an Engine does while an earlier session still
                    // owns the object (a parked timed-out Load): the tester plays again.
                    if engine::native_slot_available().is_none() {
                        log("uriprobe: the native object is still owned by an earlier session: probe NOT started");
                        return Err(crate::route::RouteStartResult::StartFailed);
                    }
                    return super::uriprobe_drive::start(ps, pa, route_start, &url, variant);
                }
                None => log("uriprobe: the trigger file names no variant (empty, load, late, none or drive): NOT armed, playing normally"),
            }
        }
    }
    // The prepared route has finished choosing the stream. Freeze the values the native Load,
    // payload and demux worker will use before any of those physical resources are constructed.
    let open = stream.then(|| super::feed_open::FeedOpenRequest::capture(ps, url));
    // The route reducer alone mints the physical attempt. The native constructor consumes this
    // identity; no backend-owned logical counter can alias a later route.
    let Some(route_attempt) = crate::route::claim_route_start_attempt(route_start) else {
        log("start_bufferfeed: prepared route lost before native construction");
        return Err(crate::route::RouteStartResult::StartFailed);
    };
    ps.begin_attempt_view(route_attempt, super::attempt_view::Backend::Feed);
    let session_rate = if stream { super::session_rate_for(ps) } else { super::RATE_NORMAL };
    let trace_generation = open.as_ref().map_or_else(
        crate::route::playback_trace_generation,
        |request| request.trace_generation,
    );
    engine::open_prepared(pa, route_attempt, open, sample, is_h265, session_rate, trace_generation)?;
    if stream { pa.start_timeline(ps, route_attempt); }
    Ok(route_attempt)
}


/// What one pass of the effects did.
#[derive(Default)]
pub(super) struct ReleasePass {
    pub(super) tail_ran: bool,
    pub(super) exit: Option<super::release::ReleaseEffect>,
}


pub(super) fn run_release_effects(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    fx: Vec<super::release::ReleaseEffect>,
    mut joined: Option<engine::PendingRelease>,
    may_start: bool,
) -> ReleasePass {
    use super::release::ReleaseEffect as E;
    let mut pass = ReleasePass::default();
    for effect in fx {
        match effect {
            E::RunTail { epoch } => {
                if let Some(p) = joined.take() {
                    debug_assert_eq!(p.epoch(), epoch);
                    engine::run_release_tail(pa.mt(), p);
                    pass.tail_ran = true;
                }
            }
            E::Start(q) if may_start => {
                let launched = match start_bufferfeed_for(ps, pa, q.route_start) {
                    BufferfeedStartOutcome::Launched(attempt) => Some(attempt),
                    _ => None,
                };
                pa.settle_preview_admission(launched.is_some(), true);
                pa.queued_start_ran(launched);
            }
            // The exit does not start playback; the transaction is settled instead.
            E::Start(q) => {
                pa.settle_preview_admission(false, false);
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            E::AbortSuperseded(q) => {
                pa.settle_preview_admission(false, false);
                pa.queued_start_dropped();
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            E::RefuseStart(q) => {
                refuse_start_for_release(ps, q);
                pa.settle_preview_admission(false, true);
                pa.queued_start_ran(None);
            }
            exit @ (E::ExitClean | E::ExitWaiting | E::ExitWorkerInFirmware) => pass.exit = Some(exit),
        }
    }
    pass
}

/// The deadline passed over a start: settle its transaction as failed and record where
/// `player::state` derives the `release_timeout` read-out from. No Engine, no second object.
pub(super) fn refuse_start_for_release(ps: &mut crate::route::PlaybackSession, q: super::release::QueuedStart) {
    log("native lifecycle: the previous Unload is overdue (refusing native start)");
    let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
    ps.native_start_refused = Some(crate::route::StartRefusal::ReleaseTimeout);
}


/// Synchronous result of replacing the current native Engine.  `Started` means the new Engine owns
/// a Load; the other states are explicit effect outcomes for the route reducer, not log-only
/// failures which leave an `OriginalTrial` waiting forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "a reload is an external state-machine effect and its result must be settled"]
pub enum ReloadOutcome {
    Started,
    NoRoute,
    StartFailed,
}

/// Settle the route-start transaction a reload effect promised but can no longer perform.
///
/// **Observe rather than reserve.** `begin_route_start` MINTS a transaction from a reducer which
/// owns none — from `Stable`, and, since a completed stop now publishes `Idle`
/// ([`crate::route::finish_engine_teardown`]), from `Idle` too — and aborting a transaction no
/// caller ever used would publish `Failed` for a route that never had a `Load` to lose. Every
/// phase that really does own one (`Preparing`/`Prepared`/`Starting`, ordinary or Original) is
/// exactly the set `pending_route_start` returns without minting, so nothing leaks.
fn settle_missing_route_start() {
    if let Some(ticket) = crate::route::pending_route_start() {
        let _ = crate::route::abort_route_start(ticket, crate::route::RouteStartResult::NoRoute);
    }
}

fn prepare_reload_transaction() -> Option<crate::route::RouteStartTransaction> {
    let ticket = crate::route::begin_route_start()?;
    if crate::route::prepare_route_start(ticket) {
        Some(ticket)
    } else {
        log("reload: route start transaction was superseded before teardown");
        None
    }
}

fn reload_start_outcome(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    ticket: crate::route::RouteStartTransaction,
) -> ReloadOutcome {
    if start_bufferfeed_for(ps, pa, ticket).accepted() {
        ReloadOutcome::Started
    } else {
        ReloadOutcome::StartFailed
    }
}


fn reload_transcode_start(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) -> Option<BufferfeedStartOutcome> {
    let ticket = prepare_reload_transaction()?;
    log(&format!(
        "reload_transcode: fresh Load at offset {}s",
        offset_ns / 1_000_000_000
    ));
    engine::teardown(ps, pa, true); // keep the session; reload mode
    SHARED.disp_base.store(offset_ns, Ordering::Relaxed); // transcode is 0-based at content=offset
    SHARED.playpos_ns.store(offset_ns, Ordering::Relaxed);
    Some(start_bufferfeed_for(ps, pa, ticket))
}

/// Every target `reload_at` was asked for (test probe, #568): proves a refused native Seek reloads
/// exactly once, at the user's target.
#[cfg(any(test, feature = "test-support"))]
pub(super) static RELOAD_AT_LOG: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// Whether the paused-seek preroll was open as each `reload_at` / `reload_transcode` was entered,
/// before its teardown (test probe): the reload keeps it, so what the fresh session's feed gate and
/// prime see is whatever the dispatcher left open here.
#[cfg(any(test, feature = "test-support"))]
pub(super) static RELOAD_PREROLL_LOG: std::sync::Mutex<Vec<bool>> = std::sync::Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
fn note_reload_preroll() {
    RELOAD_PREROLL_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(TX.seek_preroll_active());
}

pub fn reload_at(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, target_ns: i64) -> ReloadOutcome {
    #[cfg(any(test, feature = "test-support"))]
    {
        RELOAD_AT_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(target_ns);
        note_reload_preroll();
    }
    if crate::route::url(ps).is_empty() {
        log("reload_at: no url (ignored)");
        settle_missing_route_start();
        return ReloadOutcome::NoRoute;
    }
    let Some(ticket) = prepare_reload_transaction() else {
        return ReloadOutcome::StartFailed;
    };
    log(&format!(
        "reload_at: fresh Load at {}s",
        target_ns / 1_000_000_000
    ));
    engine::teardown(ps, pa, true); // reload mode: preserve the session (no url-clear / stop-scrobble)
    engine::arm_seek(target_ns);
    reload_start_outcome(ps, pa, ticket)
}

/// NATIVE audio-track switch (direct-play, NO transcode): select the Nth audio stream from the
/// same MKV and reload the direct-play pipeline at the current position (route::stream_acodec
/// was already set to the chosen track's codec, so the fresh Load configures the right audio
/// decoder). desired_audio_idx persists across the reload, so the demuxer keeps feeding the
/// chosen stream and the choice survives later seeks.
pub fn switch_audio_native(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, audio_idx: i32, pos_ns: i64) -> ReloadOutcome {
    SHARED.desired_audio_idx.store(audio_idx, Ordering::Relaxed);
    log(&format!(
        "switch_audio_native: audio_idx={audio_idx} at {}s",
        pos_ns / 1_000_000_000
    ));
    reload_at(ps, pa, pos_ns) // fresh direct-play Load at the current position, new audio stream
}

/// Reload the pipeline for a MODE/CODEC change — an audio-track switch on a direct-play HEVC
/// item forces a transcode (H264/AC3), so the pipeline must be re-Loaded with the H264 payload
/// (feeding H264 into the H265-configured pipeline stalls). Unlike reload_at, the transcode
/// start.mkv is already 0-based at `&offset`, so no av_seek — just set disp_base to the offset.
/// route::retranscode has already set the URL + session + STREAM_VCODEC=h264 before this call.
pub fn reload_transcode(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) -> ReloadOutcome {
    #[cfg(any(test, feature = "test-support"))]
    note_reload_preroll();
    if crate::route::url(ps).is_empty() {
        // The other abandonment path with the same hole: `request_seek` armed the spinner and
        // nothing here would ever disarm it. See `player::abandon_seek`.
        crate::player::abandon_seek();
        log("reload_transcode: no url (ignored)");
        settle_missing_route_start();
        return ReloadOutcome::NoRoute;
    }
    match reload_transcode_start(ps, pa, offset_ns) {
        Some(outcome) if outcome.accepted() => ReloadOutcome::Started,
        Some(BufferfeedStartOutcome::Failed) | None => ReloadOutcome::StartFailed,
        // `start_bufferfeed_for` always creates a fresh attempt, but keep the conversion total if
        // that implementation changes.
        Some(BufferfeedStartOutcome::AlreadyRunning) => ReloadOutcome::Started,
        Some(BufferfeedStartOutcome::Launched(_) | BufferfeedStartOutcome::Deferred) => {
            unreachable!("accepted handled above")
        }
    }
}

/// The same reload edge as [`reload_transcode`], retaining the exact physical Load token for the
/// foreground state machine. Used only after a synchronous Original-open failure has restored its
/// HLS candidate; ordinary callers keep the coarser [`ReloadOutcome`].
pub fn reload_transcode_tracked(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) -> BufferfeedStartOutcome {
    if crate::route::url(ps).is_empty() {
        crate::player::abandon_seek();
        log("reload_transcode_tracked: no url (ignored)");
        settle_missing_route_start();
        return BufferfeedStartOutcome::Failed;
    }
    reload_transcode_start(ps, pa, offset_ns).unwrap_or(BufferfeedStartOutcome::Failed)
}

/// Stop playback: unblock+join threads, reset all state so a fresh start_bufferfeed() can
/// restart, and release the pipeline and the video plane. With a ready object the native Unload is
/// handed to the `sf-unload` worker and the destroy / plane release finish later in
/// [`poll_release`] (`PendingRelease`), so the object may still exist when this returns; anything
/// that needs it gone is queued behind it in the release machine (`Deferred`), never waited for.
pub fn stop_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    engine::teardown(ps, pa, false);
}

/// Suspend for an app-switch: tear the pipeline down (webOS reclaims the video plane while we're
/// backgrounded) but PRESERVE the playback session — keep the URL + transcode session, and
/// don't scrobble "stopped". This makes the foreground restore a clean same-item reload
/// (begin_resume + start_bufferfeed), the known-good path, instead of resurrecting a stopped session.
pub fn suspend_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    engine::teardown(ps, pa, true);
}

