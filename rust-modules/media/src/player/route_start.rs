//! Common native route-start orchestration. The route owns the transaction and exact attempt;
//! the feed constructor only runs after admission and source preparation.

use super::{adapter::PlayerAdapter, engine, SHARED, TX};
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
        poll_release(ps, pa, std::time::Instant::now());
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
    if !pa.begin_native_open() {
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
    let side_reader = Box::new(super::feed_open::SideReaderOpen::capture(ps, stream));
    let open = stream.then(|| super::feed_open::FeedOpenRequest::capture(ps, url));
    if open.as_ref().is_some_and(|request| !request.credential_transport_allowed()) {
        plx_base::eventlog::log("stream: refused insecure credential transport");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    // Payload and native callback guards are resolved before the route mints an attempt, as in
    // the original Load path. A refusal here remains a Prepared transaction until the caller
    // aborts it; a later supersession drops both guards without launching a worker.
    let session_rate = if stream { super::session_rate_for(ps) } else { super::RATE_NORMAL };
    let trace_generation = crate::route::playback_trace_generation();
    let mut prepared_native = engine::prepare_native_open(pa.mt(), pa.feed.shared(),
        TX.paused.load(Ordering::Acquire), TX.seek_preroll_active(),
        open.map(super::feed_open::FeedOpenRequest::into_physical), sample, is_h265, session_rate, side_reader)
        .map_err(|_| crate::route::RouteStartResult::StartFailed)?;
    // The route reducer alone mints the physical attempt. The native constructor consumes this
    // identity; no backend-owned logical counter can alias a later route.
    let Some(route_attempt) = crate::route::claim_route_start_attempt(route_start) else {
        log("start_bufferfeed: prepared route lost before native construction");
        return Err(crate::route::RouteStartResult::StartFailed);
    };
    let attempt = route_attempt.contract_id();
    let force_original = prepared_native.force_original();
    prepared_native.bind_demux_session(attempt);
    ps.begin_attempt_view(route_attempt, super::attempt_view::Backend::Feed);
    let source = pa.next_source_key();
    pa.stage_native_feed(attempt, source, prepared_native)
        .map_err(|_| crate::route::RouteStartResult::StartFailed)?;
    let fidelity = if force_original {
        plx_playback_contract::Fidelity::Exact
    } else {
        plx_playback_contract::Fidelity::Compatible
    };
    let backend_at = pa.backend_tick();
    let result = pa.backend_send_at(
        plx_playback_contract::Intent::Start {
            backend: plx_playback_contract::BackendKind::Feed,
            attempt,
            source,
            requirements: plx_playback_contract::Requirements {
                source: plx_playback_contract::SourceKind::PacketFeed,
                video: fidelity,
                audio: fidelity,
                // Route preparation has already selected the feed's streams. The native
                // driver has no separate track command to confirm during startup.
                audio_track: plx_playback_contract::TrackChoice::Unspecified,
                subtitle_track: plx_playback_contract::TrackChoice::Unspecified,
            },
            // Resume/seek rebasing is already encoded in the prepared physical source.
            initial: plx_playback_contract::InitialPlayback {
                position: None,
                paused: TX.paused.load(Ordering::Acquire),
            },
        },
        if crate::route::is_preview(ps) {
            super::attempt_metrics::Origin::Preview
        } else {
            super::attempt_metrics::Origin::Fresh
        },
        backend_at,
    );
    if result.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { attempt: id, .. }
        | plx_playback_contract::Fx::CommandRefused { attempt: id, .. } if *id == attempt
    )) {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    pa.bind_feed_attempt(route_attempt, trace_generation);
    // The app gates its first pump on this readout. Publish after physical start and exact
    // attempt binding, before the caller returns to that gate.
    pa.publish_feed_attempt_view(ps);
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
                    let attempt = p.attempt();
                    engine::run_release_tail(pa.mt(), p);
                    pa.note_backend_feed_release(attempt, plx_playback_contract::ReleaseStatus::Released);
                    let at = pa.backend_tick();
                    let _ = pa.backend_poll_at(at);
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
    teardown(ps, pa, true); // keep the session; reload mode
    SHARED.disp_base.store(offset_ns, Ordering::Relaxed); // transcode is 0-based at content=offset
    SHARED.playpos_ns.store(offset_ns, Ordering::Relaxed);
    Some(start_bufferfeed_for(ps, pa, ticket))
}

/// Direct-play seek = tear down the pipeline and start a FRESH Load at `target_ns`. The old
/// flush()+refeed path left a STALE GStreamer segment (decompiled ground truth: the no-arg
/// StarfishMediaAPIs::flush() → CustomPipeline::flush() is a degenerate gst_element_seek to
/// GST_CLOCK_TIME_NONE with NO FLUSH_START/STOP and NO fresh SEGMENT; the HW sink/decoder
/// only re-anchor their segment/basetime on a real SEGMENT/FLUSH event). Post-seek buffers
/// were then scheduled against the pre-seek segment, the sink stopped draining, and the fixed
/// ~14.7 MB of upstream buffers filled in ~48 s → permanent BufferFull + "Playing error". A
/// fresh Load re-establishes a correct segment by construction — the known-good fresh-play
/// path, which never wedges. Heavier than a flush (a ~1 s re-preroll) but correct.
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
    teardown(ps, pa, true); // reload mode: preserve the session (no url-clear / stop-scrobble)
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
    teardown(ps, pa, false);
}

/// Suspend for an app-switch: tear the pipeline down (webOS reclaims the video plane while we're
/// backgrounded) but PRESERVE the playback session — keep the URL + transcode session, and
/// don't scrobble "stopped". This makes the foreground restore a clean same-item reload
/// (begin_resume + start_bufferfeed), the known-good path, instead of resurrecting a stopped session.
pub fn suspend_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    teardown(ps, pa, true);
}


/// Resume/seek AT the first Load. A direct-play item seeks the demuxer (av_seek via arm_seek).
/// A TRANSCODE item's stream is 0-based and NOT seekable (no byte-index, Content-Length=-1), so
/// av_seek fails — instead restart the encode at `&offset=secs` and display content time via
/// disp_base. Call BEFORE start_bufferfeed, AFTER route::play_movie has run the decision (so the
/// transcode session + flavor are set). Used for viewOffset resume.
///
/// **There is one entry point, [`begin_resume`] / [`land_resume`], and it is a flight** (the cold
/// resume of a freshly resolved play, and the foreground restore of a parked one): a transcode's
/// rebuild at the saved position is planned on the frame thread and its PMS half runs on a worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "resume preparation is an external route effect and must be settled"]
pub enum ResumeOutcome {
    Prepared,
    NoRoute,
    RebuildRejected,
}

/// What [`begin_resume`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use = "a pending resume owes a start when it lands"]
pub enum ResumeStart {
    /// Decided on the spot (no rebuild is owed): the caller starts the Load (or fails the start)
    /// now.
    Settled(ResumeOutcome),
    /// A flight owns the rebuild at the saved position (`route::dispatch_resume_rebase`): the start
    /// stays owed, `player::state` answers `Resolving` (via `route::resume_flight_outstanding`), and [`land_resume`] delivers the
    /// outcome the caller starts the Load on.
    Pending,
}

/// The part of a resume that never reaches PMS.
enum ResumePrelude {
    Settled(ResumeOutcome),
    /// A transcode with a saved position: its encode must restart at the offset.
    Rebuild,
}

fn resume_prelude(ps: &mut crate::route::PlaybackSession, resume_ns: i64) -> ResumePrelude {
    if resume_ns <= 0 {
        return ResumePrelude::Settled(ResumeOutcome::Prepared);
    }
    if crate::route::url(ps).is_empty() {
        return ResumePrelude::Settled(ResumeOutcome::NoRoute);
    }
    if !crate::route::is_transcoding(ps) {
        engine::arm_seek(resume_ns); // direct-play: av_seek the file at the first open
        return ResumePrelude::Settled(ResumeOutcome::Prepared);
    }
    ResumePrelude::Rebuild
}

/// The transcode half of a resume, once its encode restarts at the offset (0-based): `disp_base`
/// carries the offset so the HUD reads content time.
fn note_transcode_resumed(resume_ns: i64) {
    SHARED.disp_base.store(resume_ns, Ordering::Relaxed);
    SHARED.playpos_ns.store(resume_ns, Ordering::Relaxed);
    log(&format!(
        "resume(transcode): restart at offset {}s",
        resume_ns / 1_000_000_000
    ));
}

/// Prepare a resume at `resume_ns`: a cold start (a play whose plan just landed) or a foreground
/// restore (the parked session's start transaction is `Stable`/`Prepared`/`Failed`, or an Original
/// trial's own `Prepared` one for a session suspended mid-trial, which the flight flies over without
/// touching the trial's rollback; an `Idle` one has no URL and answers `NoRoute` here, never
/// reaching the rebuild). No Engine exists. Everything but a transcode settles on the spot; a transcode's
/// rebuild at the saved position is a FLIGHT — planned here on the frame thread, the PMS half on a
/// worker — and answers [`ResumeStart::Pending`]. The frame never reaches the server.
pub fn begin_resume(ps: &mut crate::route::PlaybackSession, resume_ns: i64) -> ResumeStart {
    match resume_prelude(ps, resume_ns) {
        ResumePrelude::Settled(outcome) => ResumeStart::Settled(outcome),
        ResumePrelude::Rebuild => match crate::route::dispatch_resume_rebase(ps, resume_ns) {
            crate::route::RecoveryDispatch::Flying { .. } => ResumeStart::Pending,
            // Nothing to rebuild or the worker could not start: a refusal.
            crate::route::RecoveryDispatch::Refused => ResumeStart::Settled(ResumeOutcome::RebuildRejected),
        },
    }
}

/// Drain a pending resume's landing, once a frame while [`crate::route::resume_flight_outstanding`].
/// `None` while the worker is still running, or when the flight went stale (a Back, a teardown, a
/// newer play request: the landing is discarded and its replacement stopped — here, by the stale
/// reap this runs first, since a flight with no Engine has no pump to do it — and whoever ended the
/// flight owns what happens next). `Some` is the outcome of the rebuild: the
/// route is installed and `Prepared` (the caller starts the Load), or the rebuild was refused and
/// the transaction is `Failed`.
pub fn land_resume(ps: &mut crate::route::PlaybackSession) -> Option<ResumeOutcome> {
    crate::route::discard_stale_flight_landing();
    if !crate::route::resume_flight_outstanding() {
        return None;
    }
    let landing = crate::route::take_ready_recovery_flight(ps)?;
    Some(match landing.verdict {
        crate::route::RecoveryVerdict::Install => {
            note_transcode_resumed(landing.offset_ns);
            ResumeOutcome::Prepared
        }
        crate::route::RecoveryVerdict::Refused => ResumeOutcome::RebuildRejected,
    })
}

/// **The runner of [`super::release::NativeRelease`].** Once a frame (and at the head of a start):
/// join the `Unload` worker only if it has returned (so the join cannot block), feed that and the
/// clock to the machine, and execute what it asks: the main-thread tail, the start that was queued
/// behind the release, or the typed refusal once the release is overdue. `true` when a tail ran.
pub fn poll_release(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    now: std::time::Instant,
) -> bool {
    let (fx, joined) = drive_release(pa, now, None);
    super::route_start::run_release_effects(ps, pa, fx, joined, true).tail_ran
}

/// [`poll_release`] at the wall clock: the per-frame entry `app::run` calls.
pub fn poll_release_frame(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    poll_release(ps, pa, std::time::Instant::now())
}

/// Reap one completed native Load parked by an earlier attempt, on the app's main thread.
pub fn reap_abandoned_load(pa: &mut super::adapter::PlayerAdapter) -> bool {
    pa.reap_abandoned_load()
}

/// Join a returned worker, then step the machine with its return, the clock and (at the exit) the
/// exit request. Returns the effects and the joined release, whose tail the effects may run.
fn drive_release(
    pa: &mut super::adapter::PlayerAdapter,
    now: std::time::Instant,
    exit: Option<std::time::Instant>,
) -> (Vec<super::release::ReleaseEffect>, Option<engine::PendingRelease>) {
    use super::release::ReleaseEvent as V;
    let mut fx = Vec::new();
    let joined = pa.take_finished_release().map(|mut p| {
        p.join_returned_worker();
        pa.release_step(&V::UnloadReturned { epoch: p.epoch() }, &mut fx);
        p
    });
    pa.release_step(&V::Tick { now }, &mut fx);
    if let Some(at) = exit {
        pa.release_step(&V::Exit { now: at }, &mut fx);
    }
    (fx, joined)
}

/// Whether the process may tear the display down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeExit {
    Clean,
    /// Native ownership is unresolved: a worker may still be inside firmware, or a URI
    /// registration may remain quarantined after its worker ended. Skip `SDL_Quit` and the
    /// Wayland window destroy until the OS reclaims that ownership.
    WorkerInFirmware,
}

/// The native half of `app::run::shutdown`, after the final stop. Waits for the native Unload or
/// URI registration release up to the release deadline, then preserves unresolved display ownership.
pub fn exit_native(pa: &mut super::adapter::PlayerAdapter) -> NativeExit {
    exit_native_with(pa, std::time::Instant::now, || std::thread::sleep(std::time::Duration::from_millis(2)))
}

pub(super) fn exit_native_with(
    pa: &mut super::adapter::PlayerAdapter,
    mut clock: impl FnMut() -> std::time::Instant,
    mut idle: impl FnMut(),
) -> NativeExit {
    #[cfg(feature = "devtriggers")]
    if pa.uri_live() {
        let started = clock();
        let mut ps = crate::route::PlaybackSession::IDLE;
        super::uriprobe_drive::stop(&mut ps, pa);
        while pa.uri_live() {
            super::uriprobe_drive::drive(&mut ps, pa);
            if !pa.uri_live() { break; }
            if pa.uri_acb_exit_quarantine() || clock().saturating_duration_since(started) >= super::release::RELEASE_DEADLINE {
                log("exit: URI ownership unresolved; the display is left to the OS");
                if let Some(session) = pa.take_uri() { std::mem::forget(session); }
                leak_parked_for_exit(pa);
                return NativeExit::WorkerInFirmware;
            }
            idle();
        }
    }
    reap_abandoned_load(pa);
    if pa.has_abandoned_load() {
        log("exit: a timed-out Load is still inside firmware; the display is left to the OS");
        leak_parked_for_exit(pa);
        return NativeExit::WorkerInFirmware;
    }
    let mut ps = crate::route::PlaybackSession::IDLE;
    loop {
        let now = clock();
        let (fx, joined) = drive_release(pa, now, Some(now));
        match super::route_start::run_release_effects(&mut ps, pa, fx, joined, false).exit {
            Some(super::release::ReleaseEffect::ExitClean) => return NativeExit::Clean,
            Some(super::release::ReleaseEffect::ExitWorkerInFirmware) => {
                log("exit: the native Unload is overdue; the display is left to the OS");
                leak_parked_for_exit(pa);
                return NativeExit::WorkerInFirmware;
            }
            _ => idle(),
        }
    }
}

/// **A worker still in firmware reads memory its owner is about to drop.** The caller skips
/// `SDL_Quit` and `app::run_and_shutdown` ends the process with `_exit` (no `App` drop, no libc
/// finalizers), but the stack above it may still unwind, so the parked
/// [`PendingRelease`] (its Engine: the Load payload Starfish holds a raw pointer into, the
/// handle set, the AU queues) and the parked [`AbandonedLoad`] (the payload its `media` thread
/// reads) would be freed under the live call. They are leaked instead, for the life of the process
/// (`release.rs`: the `_exit` that follows skips their destructors). Only `exit_native` calls this, and only on that outcome.
fn leak_parked_for_exit(pa: &mut super::adapter::PlayerAdapter) {
    if let Some(p) = pa.take_pending_release_any() {
        std::mem::forget(p);
    }
    if let Some(l) = pa.take_abandoned_load() {
        std::mem::forget(l);
    }
}

/// Test helper: finish the pending release the way the exit does, with no deadline concerns.
#[cfg(all(test, feature = "hostsim"))]
pub(crate) fn finish_release_for_test(pa: &mut super::adapter::PlayerAdapter) {
    let mut ps = crate::route::PlaybackSession::IDLE;
    while pa.release_busy() {
        let (fx, joined) = drive_release(pa, std::time::Instant::now(), None);
        let _ = super::route_start::run_release_effects(&mut ps, pa, fx, joined, false);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// **A terminal playback error lets go of the pipeline.** The failure read-out stands on the
/// session, but an Engine left loaded behind it keeps the native pipeline and the video plane,
/// which the television reads as playback still going: its screensaver never starts.
///
/// Releases them the frame after the Engine failed, the same teardown an app switch uses
/// ([`suspend_bufferfeed`]: the session, URL and transcode session stay, nothing is reported as
/// stopped). What the read-out and its actions need survives it, in the session: the cause
/// ([`crate::route::note_engine_failure`], so `state()` and `error_now` still answer) and the
/// playhead a retry resumes from (the unpresented-resume slot `retry_failed_playback` reads).
/// Try again, Play automatically and a quality pick start a new playback from scratch, and Back
/// ends the session with a real stop, so none of them needs the loaded pipeline.
/// Returns whether an Engine was released.
pub fn release_failed_engine(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    if !engine::failed_engine_release_armed() {
        return false;
    }
    // Armed by an Engine that has since been replaced (an automatic retry started a new playback
    // in the same frame): not this Engine's end, and nothing left to release.
    if !pa.is_live() {
        engine::clear_failed_engine_release_arm();
        return false;
    }
    // Not this Engine's end: a preview, one a recovery is still flying for, or one that never put
    // a picture up (a Load that failed, an open that never produced a frame) and so holds nothing
    // the television mistakes for playback.
    if crate::route::is_preview(ps)
        || SHARED.pb_state.load(Ordering::Relaxed) != super::shared::PlaybackState::Error as u8
        || SHARED.frames.load(Ordering::Relaxed) == 0
        || crate::route::recovery_flight_outstanding()
        || crate::route::engineless_flight_outstanding()
    {
        return false;
    }
    engine::clear_failed_engine_release_arm();
    let cause = super::live_runtime_failure(ps);
    crate::route::note_engine_failure(ps, cause, super::intended_pos_ns(ps));
    log("player: the failed Engine's pipeline is released under the failure read-out");
    teardown(ps, pa, true);
    true
}

/// Retire a failed foreground Engine only if it still owns the exact Load the foreground reducer
/// observed. A later route action may have replaced it between `player::pump` and the app poll;
/// tearing down unconditionally there would destroy the healthy replacement.
pub fn suspend_bufferfeed_if_attempt(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    attempt: crate::route::RouteStartAttempt,
) -> bool {
    let owns_attempt = pa.active_feed_attempt() == Some(attempt);
    if owns_attempt {
        teardown(ps, pa, true);
    }
    owns_attempt
}

/// The foreground lifecycle follows `attempt` (it launched that Load, or took over following it):
/// from now on the Load call's own synchronous refusal is the lifecycle's to settle
/// (`PlayerAdapter::mark_foreground_follows_feed_load`). True when the live Engine owns that exact attempt; a
/// replaced Engine (a different attempt) is not marked.
pub fn foreground_follows_load(pa: &mut super::adapter::PlayerAdapter, attempt: crate::route::RouteStartAttempt) -> bool {
    pa.mark_foreground_follows_feed_load(attempt)
}

/// Common teardown owns the route fence, final report and scrobble ordering. The opaque native
/// stop owns the Engine, worker joins, transport close, native release and AU destruction.
pub(super) fn teardown(ps: &mut crate::route::PlaybackSession, pa: &mut PlayerAdapter,
    for_reload: bool) {
    engine::clear_failed_engine_release_arm();
    if let Some(route_attempt) = pa.active_feed_attempt() {
        let attempt = route_attempt.contract_id();
        let at = pa.backend_tick();
        let _ = pa.backend_send_at(
            plx_playback_contract::Intent::Stop {
                backend: plx_playback_contract::BackendKind::Feed,
                attempt,
                reason: if for_reload {
                    plx_playback_contract::StopReason::Replace
                } else {
                    plx_playback_contract::StopReason::Stop
                },
            },
            super::attempt_metrics::Origin::Fresh,
            at,
        );
        let _ = pa.take_backend_feed_stop();
    }
    if !begin_teardown(ps, pa, for_reload) { return; }
    // An abandoned preview Load is never joined on the frame while native Load is in flight.
    if crate::route::is_preview(ps) {
        if let Some((loading, finished)) = pa.preview_load_join_state() {
            if super::preview::defer_media_join(loading, finished) {
                pa.abandon_preview_load();
                return;
            }
        }
    }
    begin_feed_teardown(for_reload);
    SHARED.stop_hls_clock();
    let route_attempt = pa.active_feed_attempt().expect("live feed has a route owner");
    let mut native = pa.take_native_stop();
    let final_report = final_report(ps, for_reload);
    native.stop_side_reader(for_reload);
    let mut report_th = stop_feed_timeline(pa, route_attempt);
    native.unblock_join_close();
    let mut terminal_facts = native.terminal_facts();
    terminal_facts.started = Some(false);
    let _ = ps.ingest_attempt_facts(route_attempt, crate::player::attempt_view::Backend::Feed, terminal_facts);
    after_physical_close(ps, for_reload, final_report, &mut report_th);
    native.report_dispatched();
    pa.release_native_stop(&mut native, for_reload);
    // A reload preserves the file's duration and viewer transport, then restarts the same item.
    // A real stop retires both along with the current claim hold.
    if for_reload {
        SHARED.reset_session_for_reload();
        TX.reset_for_reload();
    } else {
        SHARED.reset_session();
        TX.reset();
        super::claim_hold::clear();
    }
    finish_feed_teardown(ps, for_reload, report_th);
    pa.finish_native_stop(native, for_reload);
    pa.note_backend_feed_release(route_attempt.contract_id(), if pa.release_busy() {
        plx_playback_contract::ReleaseStatus::Closing
    } else {
        plx_playback_contract::ReleaseStatus::Released
    });
    let at = pa.backend_tick();
    let _ = pa.backend_poll_at(at);
}


/// Settle queued and engine-less playback ownership before a physical stop.
pub(super) fn begin_teardown(ps: &mut crate::route::PlaybackSession, pa: &mut PlayerAdapter, for_reload: bool) -> bool {
    // A URI-mode probe session (dev trigger `plxnative-uriprobe`) ends with the playback it stands
    // in for, whatever ended it (Back, an app switch, the next Play): its machine settles a start
    // still pending and requests unload/close; the adapter retains its window until release. It owns no Engine, so the
    // Engine-less ritual below then finishes the stop (the transcode session, the route) as it does
    // for a start which failed before an Engine existed.
    #[cfg(feature = "devtriggers")]
    super::uriprobe_drive::stop(ps, pa);
    // A start still queued behind the previous object's `Unload` is the playback this stop ends.
    // Always sent: the machine owns whether anything is queued (a no-op otherwise), and no copy of
    // that fact in the session can go stale across a landing that replaced the session.
    {
        let mut fx = Vec::new();
        pa.release_step(&super::release::ReleaseEvent::CancelQueued, &mut fx);
        let _ = run_release_effects(ps, pa, fx, None, false);
    }
    // A prepared route whose native start failed has no Engine but still owns its exact PMS
    // resource and Session projection. A real stop must retire that ownership exactly once;
    // returning here used to leak the candidate forever while the reducer remained Failed.
    if !pa.is_live() {
        // An Engine-less flight (a resume, or a rollback no Engine waits on) has no Engine to take down with it, and an app-switch suspend
        // keeps the session for the foreground restore, which reserves its own start transaction:
        // drop the flight as a live Engine's suspend would (`Stable`, landing discarded, only its
        // replacement stopped). A full stop below clears it itself.
        if for_reload && crate::route::engineless_flight_outstanding() {
            crate::route::begin_engine_teardown(true);
        }
        if !for_reload {
            // A real stop under the failure read-out whose Engine was released
            // ([`release_failed_engine`]) ends the playback the Engine would have ended: the
            // position it stopped at is the one the server keeps.
            let mut final_report = None;
            if crate::route::engine_failure(ps).is_some() {
                let dur = SHARED.duration_ns.load(Ordering::Relaxed);
                let pos = crate::route::unpresented_resume_ns(ps);
                crate::player::report::ended(ps, pos, dur);
                let rk = crate::route::cur_rk(ps);
                final_report = (!rk.is_empty() && dur > 0).then(|| (rk, pos / 1_000_000, dur / 1_000_000));
            }
            crate::route::scrobble_stop(ps, final_report, None);
            crate::route::begin_engine_teardown(false);
            crate::route::drop_original_recovery(ps);
            crate::route::clear_url(ps);
            SHARED.reset_session();
            TX.reset();
            mark_attempt_stopped(ps);
            super::claim_hold::clear();
            // Nothing here is asynchronous, so the fence `begin_engine_teardown` just raised is
            // already spent. Publish the completed stop rather than latching `Stopping` — see
            // `route::finish_engine_teardown`.
            crate::route::finish_engine_teardown();
        }
        return false;
    }
    true
}

/// A logical stop leaves the last addressed evidence available for the next report tick. The
/// next physical start calls `begin_attempt_view` and clears it to Unknown for the new attempt.
fn mark_attempt_stopped(ps: &mut crate::route::PlaybackSession) {
    let Some(attempt) = ps.attempt_view().attempt() else { return; };
    let mut facts = *ps.attempt_view().facts();
    facts.started = Some(false);
    let _ = ps.ingest_attempt_facts(attempt, crate::player::attempt_view::Backend::Feed, facts);
}

/// Capture final reporting facts before native teardown resets position and duration.
pub(super) fn final_report(ps: &mut crate::route::PlaybackSession, for_reload: bool) -> Option<(String, i64, i64)> {
    // capture the final-position report BEFORE teardown zeroes playpos/duration (a reload is
    // not a stop — don't scrobble "stopped", it would falsely pause/mark-watched the item)
    // **The one place a playback really ENDS.** `for_reload` covers a direct-play seek reload, an
    // ABR rung change and an app-switch suspend — each of which destroys an ENGINE and keeps the
    // playback — so reporting an ending on any of them would turn the completion rate into a
    // measure of how often people scrub. Read before the teardown below zeroes both values, for
    // the same reason `final_report` is.
    if !for_reload {
        crate::player::report::ended(
            ps,
            SHARED.playpos_ns.load(Ordering::Relaxed),
            SHARED.duration_ns.load(Ordering::Relaxed),
        );
    }
    if for_reload {
        None
    } else {
        let rk = crate::route::cur_rk(ps);
        let dur = SHARED.duration_ns.load(Ordering::Relaxed);
        if !rk.is_empty() && dur > 0 {
            Some((
                rk,
                SHARED.playpos_ns.load(Ordering::Relaxed) / 1_000_000,
                dur / 1_000_000,
            ))
        } else {
            None
        }
    }
}

/// Finish the route's final report after the demux reader has joined and its transport is closed.
/// The reporter handle stays owned until scrobble_stop orders its last playing write before stop.
pub(super) fn after_physical_close(
    ps: &mut crate::route::PlaybackSession,
    for_reload: bool,
    final_report: Option<(String, i64, i64)>,
    report_th: &mut Option<std::thread::JoinHandle<()>>,
) {
    if !for_reload {
        crate::route::scrobble_stop(ps, final_report, report_th.take());
    }
}

/// Fence late route workers before any physical worker is interrupted.
pub(super) fn begin_feed_teardown(for_reload: bool) {
    crate::route::begin_engine_teardown(for_reload);
}

/// Stop the exact reporter before the feed workers are unblocked; keep its handle for the
/// final scrobble ordering decision after those workers have joined.
pub(super) fn stop_feed_timeline(
    pa: &mut PlayerAdapter,
    attempt: crate::route::RouteStartAttempt,
) -> Option<std::thread::JoinHandle<()>> {
    pa.stop_timeline(attempt)
}

/// Finish route ownership only after physical release and the shared facts have reset.
pub(super) fn finish_feed_teardown(
    ps: &mut crate::route::PlaybackSession,
    for_reload: bool,
    report_th: Option<std::thread::JoinHandle<()>>,
) {
    if !for_reload {
        // **The carried link estimate dies with the PLAYBACK, not with the engine** (I8). A reload
        // is the same item on the same link at a new position — a seek, a quality pick, an
        // app-switch resume — and re-measuring the link from nothing across one is what made every
        // skip re-ramp the ladder for ten to twenty seconds.
        SHARED.clear_abr_seed();
        // The failure is retained across the rollback reload only; a new playback must not carry
        // another item's PMS response into its diagnostics.
        SHARED.clear_abr_failure();
        // A deferred Original recovery outlives nothing: the playback it belonged to is over, so
        // there is no frame left that could confirm it and no route left to roll back to. The
        // encoder it was holding is still running on the server, which is why this RETIRES rather
        // than forgets — see `route::drop_original_recovery`.
        crate::route::drop_original_recovery(ps);
        crate::route::clear_url(ps); // the transcode stop rode out with `scrobble_stop` above
    } else if let Some(t) = report_th {
        // A reload posts no `stopped`, so there is nothing to order against: detach. Its own
        // ReportStop is already set, so it exits after its current POST and cannot be revived by
        // the next session — which is exactly what the shared flag could not guarantee.
        drop(t);
    }
    // Every worker `begin_engine_teardown` fenced is joined and the URL is gone: the stop is
    // COMPLETE as far as the route is concerned, so retire the fence too. The native object may
    // still be inside its off-frame `Unload`; that is the adapter's fence (`PendingRelease`), not
    // the route's, and every native start finishes it first. A reload keeps its own phase
    // (`Prepared`/`OriginalTrial`), which is the transaction the caller is about to start.
    if !for_reload {
        crate::route::finish_engine_teardown();
    }
}

#[cfg(test)]
mod readout_stop_tests {
    use super::*;

    #[test]
    fn logical_stop_retains_failure_evidence_until_next_attempt() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let attempt = crate::route::RouteStartAttempt::fixture();
        ps.begin_attempt_view(attempt, super::super::attempt_view::Backend::Feed);
        assert!(ps.ingest_attempt_facts(attempt, crate::player::attempt_view::Backend::Feed, super::super::attempt_view::AttemptFacts {
            started: Some(true),
            runtime_failure: Some(super::super::attempt_view::RuntimeFailure::TvPipeline),
            ..Default::default()
        }));

        mark_attempt_stopped(&mut ps);
        assert_eq!(ps.attempt_view().facts().started, Some(false));
        assert!(matches!(ps.attempt_view().facts().runtime_failure,
            Some(super::super::attempt_view::RuntimeFailure::TvPipeline)));

        ps.begin_attempt_view(attempt, super::super::attempt_view::Backend::Feed);
        assert_eq!(ps.attempt_view().facts().started, None);
        assert!(ps.attempt_view().facts().runtime_failure.is_none());
    }
}
