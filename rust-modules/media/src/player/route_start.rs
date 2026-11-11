//! Common native route-start orchestration. The route owns the transaction and exact attempt;
//! the feed constructor only runs after admission and source preparation.

#[cfg(test)]
use plx_play_feed::shared::FEED_TX;
use super::adapter::PlayerAdapter;
use plx_playback_contract::feed::ResetScope;
use super::{engine, SHARED};
use plx_base::eventlog::log;

/// Start one native Engine and settle any route candidate which was prepared before (or, for a
/// dev fixture, during) this call.  A pre-existing Engine is a successful no-op only when there is
/// no candidate waiting for a *new* Load; otherwise accepting it would falsely settle the new
/// route with the old decoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferfeedStartOutcome {
    AlreadyRunning,
    Launched(crate::route::RouteStartAttempt),
    /// The previous Feed `Unload` or URI registration is still releasing: the start is queued
    /// in the release machine and runs after exact release (or fails at the deadline). Accepted:
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

/// Start the qualified route through the common attempt owner, regardless of physical backend.
pub fn start_playback(ps: &mut crate::route::PlaybackSession, pa: &mut PlayerAdapter) -> bool {
    start_bufferfeed(ps, pa)
}

/// Preserve the exact physical attempt for foreground and release correlation.
pub fn start_playback_tracked(ps: &mut crate::route::PlaybackSession, pa: &mut PlayerAdapter)
    -> BufferfeedStartOutcome {
    start_bufferfeed_tracked(ps, pa)
}

/// Start the native Engine while retaining the identity of the exact asynchronous `sf_load`.
/// Foreground recovery uses this to wait for the media-thread result instead of treating thread
/// creation as proof that the television accepted the payload.
pub fn start_bufferfeed_tracked(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> BufferfeedStartOutcome {
    if super::accept_server_stop(ps) {
        return BufferfeedStartOutcome::Failed;
    }
    pa.refresh_uri_qualification(ps);
    let uri_chosen = matches!(ps.backend_offer(),
        crate::route::backend_policy::Selection::Open(offer)
            if offer.backend == plx_playback_contract::BackendKind::Uri);
    if !uri_chosen && plx_platform::tv::sandbox::blocks_native_video() {
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
        pa.feed_command(plx_playback_contract::feed::FeedCommand::BeginPlayback);
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
    if let Some(existing) = pa.active_uri_attempt() {
        if pa.backend_snapshot().phase == plx_playback_contract::Phase::Quarantined {
            if let crate::route::LiveEngineStartRelation::Conflict(ticket) =
                crate::route::classify_live_engine_start(existing) {
                let _ = crate::route::abort_route_start(ticket, crate::route::RouteStartResult::StartFailed);
            }
            return BufferfeedStartOutcome::Failed;
        }
        // A closing URI still owns its native session, but the logical successor may reserve
        // the one route transaction behind its addressed release. No URL escrow is taken here.
        if pa.backend_snapshot().phase != plx_playback_contract::Phase::Releasing {
        match crate::route::classify_live_engine_start(existing) {
            crate::route::LiveEngineStartRelation::CurrentAttempt =>
                return BufferfeedStartOutcome::Launched(existing),
            crate::route::LiveEngineStartRelation::NoPendingRoute =>
                return BufferfeedStartOutcome::AlreadyRunning,
            crate::route::LiveEngineStartRelation::Conflict(ticket) => {
                // A newly resolved route replaces this URI attempt. Retire its exact native
                // owner first, then reserve the existing route transaction in the shared
                // release queue; the successor must not open while close is only pending.
                let at = pa.backend_tick();
                let stopped = pa.backend_send_at(ps, plx_playback_contract::Intent::Stop {
                    backend: plx_playback_contract::BackendKind::Uri,
                    attempt: existing.contract_id(),
                    reason: plx_playback_contract::StopReason::Replace,
                }, super::attempt_metrics::Origin::Fresh, at);
                let addressed = stopped.controls.iter().any(|sent|
                    sent.backend == plx_playback_contract::BackendKind::Uri
                        && sent.attempt == existing.contract_id()
                        && sent.operation == plx_playback_contract::Operation::Stop(
                            plx_playback_contract::StopReason::Replace));
                let refused = stopped.playback.iter().any(|notice| matches!(notice,
                    plx_playback_contract::Fx::Rejected {
                        backend: plx_playback_contract::BackendKind::Uri, attempt, ..
                    } | plx_playback_contract::Fx::CommandRefused {
                        backend: plx_playback_contract::BackendKind::Uri, attempt,
                        operation: plx_playback_contract::Operation::Stop(_), ..
                    } if *attempt == existing.contract_id()));
                if !addressed || refused || !pa.release_busy() {
                    let _ = crate::route::abort_route_start(
                        ticket, crate::route::RouteStartResult::StartFailed);
                    return BufferfeedStartOutcome::Failed;
                }
                return start_bufferfeed_for(ps, pa, ticket);
            }
        }
        }
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
    if super::accept_server_stop(ps) {
        let _ = crate::route::abort_route_start(route_start, crate::route::RouteStartResult::StartFailed);
        return BufferfeedStartOutcome::Failed;
    }
    #[cfg(feature = "devtriggers")]
    if pa.uri_live() {
        // Reload/foreground starts reach this seam directly too. The diagnostic probe's
        // registration is separate from the product URI release queue.
        let _ = crate::route::abort_route_start(route_start, crate::route::RouteStartResult::StartFailed);
        return BufferfeedStartOutcome::Failed;
    }
    // The last real stop's object may still be inside its off-frame `Unload`. A tail that is ready
    // runs now; one that is not defers this start into the release machine (never a blocking join:
    // PLX-NATIVE-1Q), which starts it the frame the tail has run or refuses it at the deadline.
    if pa.release_busy() && pa.active_uri_attempt().is_none() {
        poll_release(ps, pa, std::time::Instant::now());
    }
    let mut fx = Vec::new();
    pa.release_step(
        ps,
        &super::release::ReleaseEvent::StartRequested(super::release::QueuedStart { route_start }),
        &mut fx,
    );
    let mut go = false;
    let mut refused = false;
    for effect in fx {
        match effect {
            super::release::ReleaseEffect::Start(_) => go = true,
            super::release::ReleaseEffect::RefuseStart(q) => {
                refuse_start_for_release(ps, pa, q);
                refused = true;
            }
            super::release::ReleaseEffect::AbortSuperseded(q) => {
                pa.settle_preview_admission(false, false);
                pa.discard_qualified_uri_for(ps, q.route_start);
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
        log("start_playback: the previous native owner is still releasing; start deferred");
        return BufferfeedStartOutcome::Deferred;
    }
    pa.refresh_uri_qualification(ps);
    let start = if matches!(ps.backend_offer(),
        crate::route::backend_policy::Selection::Open(offer)
            if offer.backend == plx_playback_contract::BackendKind::Uri) {
        start_qualified_uri_inner(ps, pa, route_start)
    } else if !ps.backends().feed {
        // No Feed composed: there is no physical open to resolve a source for.
        Err(crate::route::RouteStartResult::StartFailed)
    } else {
        start_bufferfeed_inner(ps, pa, route_start)
    };
    match start {
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

/// A source-specific qualified URI offer uses the same route claim and common attempt runner as
/// Feed. The caller supplied the owned URL and exact native track map; no MDE or device codec
/// declaration can create this offer on its own.
fn start_qualified_uri_inner(ps: &mut crate::route::PlaybackSession, pa: &mut PlayerAdapter,
    route_start: crate::route::RouteStartTransaction)
    -> Result<crate::route::RouteStartAttempt, crate::route::RouteStartResult> {
    use crate::route::backend_policy::Selection;
    let fail = crate::route::RouteStartResult::StartFailed;
    if !crate::route::prepare_route_start(route_start) { return Err(fail); }
    let route_attempt = crate::route::claim_route_start_attempt(route_start).ok_or(fail)?;
    let offer = pa.take_qualified_uri(ps, route_start).ok_or(fail)?;
    if !matches!(ps.backend_offer(), Selection::Open(choice)
        if choice.backend == plx_playback_contract::BackendKind::Uri
            && choice.id == offer.offer_id) {
        ps.qualify_backend_offer(plx_playback_contract::BackendKind::Uri,
            offer.offer_id, crate::route::backend_policy::Qualification::Unknown);
        return Err(fail);
    }
    let attempt = route_attempt.contract_id();
    let source = match pa.stage_uri_source(attempt, offer.source) {
        Ok(source) => source,
        Err(_) => {
            ps.qualify_backend_offer(plx_playback_contract::BackendKind::Uri,
                offer.offer_id, crate::route::backend_policy::Qualification::Unknown);
            return Err(fail);
        }
    };
    pa.sync_request_metrics(ps);
    let at = pa.backend_tick();
    let result = pa.backend_send_at(ps, plx_playback_contract::Intent::Start {
        backend: plx_playback_contract::BackendKind::Uri,
        attempt,
        source,
        requirements: offer.requirements,
        initial: offer.initial,
    }, ps.attempt_origin(), at);
    if result.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { attempt: id, .. }
        | plx_playback_contract::Fx::CommandRefused { attempt: id, .. } if *id == attempt
    )) {
        ps.qualify_backend_offer(plx_playback_contract::BackendKind::Uri,
            offer.offer_id, crate::route::backend_policy::Qualification::Unknown);
        return Err(fail);
    }
    assert!(ps.backend_offer_admitted(plx_playback_contract::BackendKind::Uri,
        offer.offer_id), "qualified URI offer must remain selected on the main thread");
    ps.clear_reconfigure_pause_goal();
    ps.begin_attempt_view(route_attempt, super::attempt_view::Backend::Uri);
    let mut facts = *ps.attempt_view().facts();
    facts.duration_ns = Some(offer.pms_duration_ns);
    facts.ended = Some(false);
    facts.seek_display_ns = offer.initial.position.and_then(|position|
        position.as_millis().checked_mul(1_000_000))
        .and_then(|ns| i64::try_from(ns).ok());
    let _ = ps.ingest_attempt_facts(route_attempt, super::attempt_view::Backend::Uri, facts);
    pa.bind_uri_attempt(route_attempt, offer.offer_id);
    pa.start_timeline(ps, route_attempt);
    Ok(route_attempt)
}


pub(super) fn start_bufferfeed_inner(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: crate::route::RouteStartTransaction,
) -> Result<crate::route::RouteStartAttempt, crate::route::RouteStartResult> {
    restoring_offered_rate_on_failure(ps, |ps| {
        restoring_pause_on_failure(ps, pa, |ps, pa| start_bufferfeed_prepared(ps, pa, route_start))
    })
}

/// A REAL stop of the transport (a reload does not stop it): the readout's `TransportStopped`. The
/// feed's own `Transport::reset` runs in `FeedControl::reset(ResetScope::Stop, ..)`, called just
/// before, so the Pause, the preroll and the owed re-pause go from both copies at once.
pub(super) fn stop_transport(ps: &mut crate::route::PlaybackSession) {
    ps.step_readout(super::readout::ReadoutEvent::TransportStopped);
}

/// Run a Feed start that publishes the viewer's pause goal to the Feed's transport, and put the
/// pause the transport held back if the start fails: a candidate that was refused, or lost its
/// route, leaves the outgoing attempt's pause exactly as found. (A guard that drops cannot reach
/// the session and the adapter that carry the event and the command, so the rollback is a wrapper,
/// like the offered rate's.)
fn restoring_pause_on_failure<T, E>(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    start: impl FnOnce(&mut crate::route::PlaybackSession, &mut super::adapter::PlayerAdapter)
        -> Result<T, E>,
) -> Result<T, E> {
    let before = ps.readout().paused();
    let started = start(ps, pa);
    if started.is_err() && ps.readout().paused() != before {
        super::commit_paused(ps, pa, before);
    }
    started
}

/// Run a Feed start that publishes the session's offered rate to its native Load, and put the
/// rate the readout held back if the start fails: a failed candidate cannot change the outgoing
/// attempt's offer, and a refusal anywhere between the publish and the commit leaves it as found.
fn restoring_offered_rate_on_failure<T, E>(
    ps: &mut crate::route::PlaybackSession,
    start: impl FnOnce(&mut crate::route::PlaybackSession) -> Result<T, E>,
) -> Result<T, E> {
    let before = ps.readout().play_rate_permille();
    let started = start(ps);
    if started.is_err() {
        ps.step_readout(super::readout::ReadoutEvent::RateSelected { permille: before });
    }
    started
}

fn start_bufferfeed_prepared(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    route_start: crate::route::RouteStartTransaction,
) -> Result<crate::route::RouteStartAttempt, crate::route::RouteStartResult> {
    // The source first: a route plan qualified its recipe already, and a source no plan resolved (a
    // dev trigger's) is qualified by being resolved. Nothing native is touched until the
    // offer check below has passed.
    let resolved = super::feed_open::resolve_source(ps, pa)?;
    if !ps.backend_planned_feed_allowed() {
        log("start_bufferfeed: no qualified feed offer for this resolved route");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let offer = ps.planned_feed_offer().expect("qualified feed has a recipe");
    // The previous object's release is finished or the start was deferred before this point
    // (`start_bufferfeed_for`), so the epoch is retired and the object destroyed already.
    // A timed-out Load from an earlier Engine may still own the one native object the C seam
    // holds. Release it now if it has returned; otherwise refuse before anything below (the
    // exported window in particular) can touch state that object is still using.
    if !pa.open_begin() {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    // `open_begin` can reap a previously abandoned Load. Admit its exact D1/quarantine
    // verdict before asking the common runner to admit another attempt.
    let at = pa.backend_tick();
    let _ = pa.backend_poll_at(ps, at);
    // Guard a double-start: overwriting a live ENGINE slot would DROP the running
    // Engine, detaching its worker threads and freeing the hs/aq boxes those
    // threads still hold raw ptrs into -> use-after-free. If already running, no-op.
    // (Reachable via a PLAY key landing in the WILL->DID foreground window.)
    // Per-SESSION, not per-boot: both the log's every-100th cadence and the diagnostics read-out
    // mean "this playback", and a count carried in from the last item answers neither question.

    if !crate::route::prepare_route_start(route_start) {
        log("start_bufferfeed: prepared route no longer owns the transaction");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let url = resolved.url;
    let sample = resolved.sample;
    let is_h265 = resolved.is_h265;
    let stream = sample.is_none();
    // dev: /tmp/plxnative-uriprobe — play this URL through `com.webos.media` (URI mode) instead of
    // building an Engine, to learn what webOS 5+ does with it (issue #1064). Taken AFTER the route
    // is prepared and BEFORE anything native is touched (the clock and session guards, the exported
    // window, the payload), and never for a preview or a local sample: the probe needs a real stream
    // URL and the foreground play which owns the screen. The content names the variant, then
    // optionally the native ABR rates (`bpsstart=N bpsmax=N bpsmin=N`); text which names none
    // leaves the probe unarmed and the play normal rather than running a variant nobody asked for.
    #[cfg(feature = "devtriggers")]
    if stream && !crate::route::is_preview(ps) && pa.uri_probe_installed() {
        if let Some(text) = plx_base::devtrig::read("uriprobe") {
            match pa.uri_probe_armed(&text) {
                Some(launch) => {
                    // The probe takes no native session of its own, but its window verbs reach the
                    // same seam, so it refuses like an Engine does while an earlier session still
                    // owns the object (a parked timed-out Load): the tester plays again.
                    if !pa.uri_probe_native_slot_available() {
                        log("uriprobe: the native object is still owned by an earlier session: probe NOT started");
                        return Err(crate::route::RouteStartResult::StartFailed);
                    }
                    return super::uriprobe_drive::start(ps, pa, route_start, &url, launch);
                }
                None => log("uriprobe: the trigger file names no variant (empty, load, late, none or drive, then optional bpsstart=N bpsmax=N bpsmin=N from 1 to 1000000000): NOT armed, playing normally"),
            }
        }
    }
    // The prepared route has finished choosing the stream. Freeze the values the native Load,
    // payload and demux worker will use before any of those physical resources are constructed.
    let side_reader = super::feed_open::side_reader_request(ps, stream);
    let mut open = stream.then(|| super::feed_open::FeedOpenRequest::capture(ps, url));
    if open.as_ref().is_some_and(|request| !request.credential_transport_allowed()) {
        plx_base::eventlog::log("stream: refused insecure credential transport");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let abr_inbox = open.as_mut().map(super::feed_open::FeedOpenRequest::take_readouts);
    // Payload and native callback guards are resolved before the route mints an attempt, as in
    // the original Load path. A refusal here remains a Prepared transaction until the caller
    // aborts it; a later supersession drops both guards without launching a worker.
    let session_rate = if stream { feed_start_rate(ps)? } else { super::RATE_NORMAL };
    // The viewer's goal: a reconfigure or a claim landing records it, else the transport's own
    // state. The readout commit and the plan's Play decision follow this, so a claim landing of a
    // playing viewer starts the fresh stream unpaused (the plan issues Play, which is the
    // `ResumeInitial` that opens the feed gate and lets the Initial prime fire).
    let start_paused = feed_start_paused(ps);
    // The physical hold: the feed gate's state before this start (captured before the commit
    // below mutates it). A claim landing paused the transport for its flight, so even a playing
    // viewer's fresh stream is held here for its Initial prime, though its goal is to play.
    let physical_hold = ps.readout().paused();
    // Publish the session's offered rate and the pause goal to the native Load. A refused start
    // puts both back (the wrappers around this fn): a failed candidate cannot change the outgoing
    // attempt's offer or its Feed mailbox.
    ps.step_readout(super::readout::ReadoutEvent::RateSelected { permille: session_rate });
    if ps.readout().paused() != start_paused {
        super::commit_paused(ps, pa, start_paused);
    }
    let trace_generation = crate::route::playback_trace_generation();
    // A native refusal is mapped here, by the common owner: the executor logs why, and the route
    // only learns that the start failed.
    let native_epoch = pa.open_prepare(plx_playback_contract::feed::OpenRequest {
        start_paused: physical_hold,
        seek_preroll: ps.readout().seek_preroll(),
        stream: open.map(super::feed_open::FeedOpenRequest::into_stream),
        sample,
        is_h265,
        session_rate,
        requested_rate: super::play_rate_permille(ps),
        no_audio: super::no_audio_armed(),
        side_reader,
        // A local sample's static payload carries no node, so only a stream arms the guard.
        dv_landing: if stream {
            super::feed_open::dv_landing_plan(ps)
        } else {
            plx_playback_contract::feed::DvLandingPlan::INERT
        },
    }).map_err(|_| crate::route::RouteStartResult::StartFailed)?;
    // The route reducer alone mints the physical attempt. The native constructor consumes this
    // identity; no backend-owned logical counter can alias a later route.
    let Some(route_attempt) = crate::route::claim_route_start_attempt(route_start) else {
        log("start_bufferfeed: prepared route lost before native construction");
        // The parked open owns the clock and epoch guards; a later start needs them idle.
        pa.open_abort();
        return Err(crate::route::RouteStartResult::StartFailed);
    };
    let attempt = route_attempt.contract_id();
    let (video_fidelity, audio_fidelity) = ps.planned_feed_fidelity()
        .expect("qualified feed has fidelity requirements");
    let Some(session) = pa.capture_demux_session(attempt, native_epoch) else {
        // No Feed composition (the open above was refused on one already): nothing to stage.
        pa.open_abort();
        return Err(crate::route::RouteStartResult::StartFailed);
    };
    ps.begin_attempt_view(route_attempt, super::attempt_view::Backend::Feed);
    let source = pa.next_source_key();
    if pa.open_stage(attempt, source, session).is_err() {
        // The stage consumed the parked open on its refusal; abort is the explicit no-op that
        // keeps the pairing with `open_prepare` whole.
        pa.open_abort();
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    if let Some(inbox) = abr_inbox {
        pa.bind_abr_inbox(route_attempt, inbox);
    }
    // The demuxer spawns inside the Start below and reads the feed's audio copy as it opens the
    // container. Every writer of the choice that has no adapter in hand (the route's landing, a
    // retry's reset, a displaced pick) stepped the session's readout; this is where it reaches the feed.
    super::publish_audio_choice(ps, pa);
    pa.sync_request_metrics(ps);
    let backend_at = pa.backend_tick();
    let result = pa.backend_send_at(
        ps,
        plx_playback_contract::Intent::Start {
            backend: plx_playback_contract::BackendKind::Feed,
            attempt,
            source,
            requirements: plx_playback_contract::Requirements {
                source: plx_playback_contract::SourceKind::PacketFeed,
                video: video_fidelity,
                audio: audio_fidelity,
                // Route preparation has already selected the feed's streams. The native
                // driver has no separate track command to confirm during startup.
                audio_track: plx_playback_contract::TrackChoice::Unspecified,
                subtitle_track: plx_playback_contract::TrackChoice::Unspecified,
            },
            // Resume/seek rebasing is already encoded in the prepared physical source.
            initial: plx_playback_contract::InitialPlayback {
                position: None,
                paused: start_paused,
            },
        },
        if crate::route::is_preview(ps) {
            super::attempt_metrics::Origin::Preview
        } else {
            ps.attempt_origin()
        },
        backend_at,
    );
    if result.playback.iter().any(|fx| matches!(fx,
        plx_playback_contract::Fx::Rejected { attempt: id, .. }
        | plx_playback_contract::Fx::CommandRefused { attempt: id, .. } if *id == attempt
    )) {
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    let admitted = ps.backend_offer_admitted(plx_playback_contract::BackendKind::Feed, offer);
    assert!(admitted, "the prechecked feed offer must remain admissible on the main thread");
    ps.clear_reconfigure_pause_goal();
    pa.bind_feed_attempt(route_attempt, trace_generation);
    // The app gates its first pump on this readout. Publish after physical start and exact
    // attempt binding, before the caller returns to that gate.
    pa.publish_feed_attempt_view(ps);
    if stream { pa.start_timeline(ps, route_attempt); }
    Ok(route_attempt)
}

/// Select the physical rate for the prepared Feed source. Route start is the boundary where
/// the session's accepted speed is applied to a new native Load.
fn feed_start_rate(ps: &mut crate::route::PlaybackSession)
    -> Result<i32, crate::route::RouteStartResult> {
    let want = ps.requested_rate_permille();
    if want != super::RATE_NORMAL && !super::feed_speed_offer(ps).contains(&want) {
        log("start_bufferfeed: requested playback speed is unavailable for this Feed source");
        return Err(crate::route::RouteStartResult::StartFailed);
    }
    // The physical payload still consumes the offered rate. Publish the session's
    // accepted value before that preflight; a refused native start puts the old offer back
    // ([`restoring_offered_rate_on_failure`]).
    ps.step_readout(super::readout::ReadoutEvent::RateSelected { permille: want });
    let rate = super::session_rate_for(ps);
    debug_assert_eq!(rate, want, "the prechecked Feed rate must be applied exactly");
    Ok(rate)
}

/// Capture one pause goal for both the physical hold and common Start intent.
fn feed_start_paused(ps: &crate::route::PlaybackSession) -> bool {
    ps.reconfigure_pause_goal().unwrap_or_else(|| ps.readout().paused())
}

/// What one pass of the effects did.
#[derive(Default)]
pub(super) struct ReleasePass {
    pub(super) tail_ran: bool,
    pub(super) exit: Option<super::release::ReleaseEffect>,
}

/// Execute the existing release machine's queued route transaction after the addressed URI
/// `Released` verdict. A raw backend poll (`backend_poll_at`) only steps the machine; the route
/// start waits for the session poll to run these effects, and the adapter carries only these
/// effects between those two steps, never a second queue.
pub(super) fn run_uri_queued_effects(ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter, fx: Vec<super::release::ReleaseEffect>) {
    use super::release::ReleaseEffect as E;
    for effect in fx {
        match effect {
            E::Start(q) => {
                let launched = match start_bufferfeed_for(ps, pa, q.route_start) {
                    BufferfeedStartOutcome::Launched(attempt) => Some(attempt),
                    _ => None,
                };
                pa.settle_preview_admission(launched.is_some(), true);
                pa.queued_start_ran(launched);
            }
            E::AbortSuperseded(q) => {
                pa.settle_preview_admission(false, false);
                pa.discard_qualified_uri_for(ps, q.route_start);
                pa.queued_start_dropped();
                let _ = crate::route::abort_route_start(q.route_start,
                    crate::route::RouteStartResult::StartFailed);
            }
            E::RefuseStart(q) => {
                refuse_start_for_release(ps, pa, q);
                pa.settle_preview_admission(false, true);
                pa.queued_start_ran(None);
            }
            E::RunTail { .. } | E::ExitClean | E::ExitWaiting | E::ExitWorkerInFirmware => {
                debug_assert!(false, "URI release produced a native Feed tail/exit effect");
            }
        }
    }
}


pub(super) fn run_release_effects(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    fx: Vec<super::release::ReleaseEffect>,
    may_start: bool,
) -> ReleasePass {
    use super::release::ReleaseEffect as E;
    let mut pass = ReleasePass::default();
    for effect in fx {
        match effect {
            E::RunTail { epoch } => {
                if pa.run_release_tail(epoch) {
                    let at = pa.backend_tick();
                    let _ = pa.backend_poll_at(ps, at);
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
                pa.discard_qualified_uri_for(ps, q.route_start);
                pa.queued_start_dropped();
                let _ = crate::route::abort_route_start(q.route_start, crate::route::RouteStartResult::StartFailed);
            }
            E::RefuseStart(q) => {
                refuse_start_for_release(ps, pa, q);
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
pub(super) fn refuse_start_for_release(ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter, q: super::release::QueuedStart) {
    log("native lifecycle: the previous release is overdue (refusing native start)");
    pa.discard_qualified_uri_for(ps, q.route_start);
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
    teardown(ps, pa, true, plx_playback_contract::StopReason::Replace); // keep the session; reload mode
    super::rebase_transcode(ps, pa, offset_ns); // transcode is 0-based at content=offset
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
#[cfg(test)]
pub(super) static RELOAD_AT_LOG: std::sync::Mutex<Vec<i64>> = std::sync::Mutex::new(Vec::new());

/// Whether the paused-seek preroll was open as each `reload_at` / `reload_transcode` was entered,
/// before its teardown (test probe): the reload keeps it, so what the fresh session's feed gate and
/// prime see is whatever the dispatcher left open here.
#[cfg(test)]
pub(super) static RELOAD_PREROLL_LOG: std::sync::Mutex<Vec<bool>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn note_reload_preroll() {
    RELOAD_PREROLL_LOG.lock().unwrap_or_else(|e| e.into_inner()).push(FEED_TX.seek_preroll_active());
}

pub fn reload_at(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, target_ns: i64) -> ReloadOutcome {
    #[cfg(test)]
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
    let reload_is_seek = ps.readout().unconfirmed_seek_target().is_some();
    teardown(ps, pa, true, plx_playback_contract::StopReason::Replace); // reload mode: preserve the session (no url-clear / stop-scrobble)
    if reload_is_seek {
        ps.step_readout(super::readout::ReadoutEvent::ReloadSeek { target_ns });
    }
    super::arm_open(pa, target_ns);
    reload_start_outcome(ps, pa, ticket)
}

/// NATIVE audio-track switch (direct-play, NO transcode): select the Nth audio stream from the
/// same MKV and reload the direct-play pipeline at the current position (route::stream_acodec
/// was already set to the chosen track's codec, so the fresh Load configures the right audio
/// decoder). desired_audio_idx persists across the reload, so the demuxer keeps feeding the
/// chosen stream and the choice survives later seeks.
pub fn switch_audio_native(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, audio_idx: i32, pos_ns: i64) -> ReloadOutcome {
    ps.step_readout(super::readout::ReadoutEvent::AudioSelected { idx: audio_idx });
    super::publish_audio_choice(ps, pa);
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
    #[cfg(test)]
    note_reload_preroll();
    if crate::route::url(ps).is_empty() {
        // The other abandonment path with the same hole: `request_seek` armed the spinner and
        // nothing here would ever disarm it. See `player::abandon_seek`.
        crate::player::abandon_seek(ps, pa);
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
        crate::player::abandon_seek(ps, pa);
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
    super::accept_server_stop(ps);
    teardown(ps, pa, false, plx_playback_contract::StopReason::Stop);
}

/// A retry or queue handoff may replace the outgoing playback only while it owns that decision.
pub fn stop_for_replacement(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    if retire_if_server_stopped(ps, pa) {
        return false;
    }
    teardown(ps, pa, false, plx_playback_contract::StopReason::Stop);
    true
}

/// The per-frame door of a server stop, for whichever backend plays: when the server has ended
/// this logical playback, retire it ([`super::accept_server_stop`]) and release what plays it.
pub fn retire_if_server_stopped(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    if !super::accept_server_stop(ps) {
        return false;
    }
    release_server_stopped_engine(ps, pa);
    true
}

/// A server stop can retire a replacement Engine before it ever presents a frame, and a URI
/// attempt the same. The session stays for the read-out (a reload-shaped teardown), but the
/// release is the terminal kind: off the frame, with a Load still in the native call parked.
pub(super) fn release_server_stopped_engine(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
) {
    if pa.is_live() || pa.active_uri_attempt().is_some() || (ps.backends().feed && ps.readout().start_queued()) {
        super::abandon_seek(ps, pa);
        teardown(ps, pa, true, plx_playback_contract::StopReason::Replace);
    }
}

/// Suspend for an app-switch: tear the pipeline down (webOS reclaims the video plane while we're
/// backgrounded) but PRESERVE the playback session — keep the URL + transcode session, and
/// don't scrobble "stopped". This makes the foreground restore a clean same-item reload
/// (begin_resume + start_bufferfeed), the known-good path, instead of resurrecting a stopped session.
pub fn suspend_bufferfeed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    teardown(ps, pa, true, plx_playback_contract::StopReason::Suspend);
}


/// Resume/seek AT the first Load. A direct-play item seeks the demuxer (av_seek via `ArmOpen`).
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

fn resume_prelude(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    resume_ns: i64,
) -> ResumePrelude {
    if resume_ns <= 0 {
        return ResumePrelude::Settled(ResumeOutcome::Prepared);
    }
    if crate::route::url(ps).is_empty() {
        return ResumePrelude::Settled(ResumeOutcome::NoRoute);
    }
    if !crate::route::is_transcoding(ps) {
        super::arm_open(pa, resume_ns); // direct-play: av_seek the file at the first open
        return ResumePrelude::Settled(ResumeOutcome::Prepared);
    }
    ResumePrelude::Rebuild
}

/// The transcode half of a resume, once its encode restarts at the offset (0-based): `disp_base`
/// carries the offset so the HUD reads content time.
fn note_transcode_resumed(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, resume_ns: i64) {
    super::rebase_transcode(ps, pa, resume_ns);
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
pub fn begin_resume(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    resume_ns: i64,
) -> ResumeStart {
    if super::accept_server_stop(ps) {
        return ResumeStart::Settled(ResumeOutcome::RebuildRejected);
    }
    match resume_prelude(ps, pa, resume_ns) {
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
pub fn land_resume(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
) -> Option<ResumeOutcome> {
    if super::accept_server_stop(ps) {
        return None;
    }
    crate::route::discard_stale_flight_landing();
    if !crate::route::resume_flight_outstanding() {
        return None;
    }
    let landing = crate::route::take_ready_recovery_flight(ps)?;
    Some(match landing.verdict {
        crate::route::RecoveryVerdict::Install => {
            if ps.backends().feed {
                note_transcode_resumed(ps, pa, landing.offset_ns);
            }
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
    let fx = drive_release(ps, pa, now, None);
    super::route_start::run_release_effects(ps, pa, fx, true).tail_ran
}

/// [`poll_release`] at the wall clock: the per-frame entry `app::run` calls.
pub fn poll_release_frame(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> bool {
    poll_release(ps, pa, std::time::Instant::now())
}

/// Reap one completed native Load parked by an earlier attempt, on the app's main thread.
pub fn reap_abandoned_load(pa: &mut super::adapter::PlayerAdapter) -> bool {
    pa.reap_abandoned_load()
}

/// Step the release machine with a returned worker (the Feed has joined it and keeps the release
/// for its tail), the clock and (at the exit) the exit request. Returns the effects, one of which
/// may be that tail.
fn drive_release(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    now: std::time::Instant,
    exit: Option<std::time::Instant>,
) -> Vec<super::release::ReleaseEffect> {
    use super::release::ReleaseEvent as V;
    let mut fx = Vec::new();
    if let Some(epoch) = pa.take_returned_release() {
        pa.release_step(ps, &V::UnloadReturned { epoch }, &mut fx);
    }
    pa.release_step(ps, &V::Tick { now }, &mut fx);
    if let Some(at) = exit {
        pa.release_step(ps, &V::Exit { now: at }, &mut fx);
    }
    fx
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
pub fn exit_native(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> NativeExit {
    exit_native_with(ps, pa, std::time::Instant::now, || std::thread::sleep(std::time::Duration::from_millis(2)))
}

pub fn exit_native_with(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    _clock: impl FnMut() -> std::time::Instant,
    _idle: impl FnMut(),
) -> NativeExit {
    let mut clock = _clock;
    let mut idle = _idle;
    // Exit cannot launch the successor a release was holding. Retire its exact route ticket
    // before waiting for the old native owner; no URL escrow or encoder may be started later.
    let mut queued_fx = Vec::new();
    pa.release_step(ps, &super::release::ReleaseEvent::CancelQueued, &mut queued_fx);
    queued_fx.extend(pa.take_release_effects());
    for effect in queued_fx {
        if let super::release::ReleaseEffect::AbortSuperseded(q)
            | super::release::ReleaseEffect::Start(q)
            | super::release::ReleaseEffect::RefuseStart(q) = effect {
            pa.settle_preview_admission(false, false);
            pa.drop_qualified_uri_for_exit(q.route_start);
            pa.queued_start_dropped();
            let _ = crate::route::abort_route_start(q.route_start,
                crate::route::RouteStartResult::StartFailed);
        }
    }
    #[cfg(feature = "devtriggers")]
    if pa.uri_live() {
        let started = clock();
        super::uriprobe_drive::stop(ps, pa);
        while pa.uri_live() {
            super::uriprobe_drive::drive(ps, pa);
            if !pa.uri_live() { break; }
            if pa.uri_acb_exit_quarantine() || clock().saturating_duration_since(started) >= super::release::RELEASE_DEADLINE {
                log("exit: URI ownership unresolved; the display is left to the OS");
                if let Some(session) = pa.take_uri() { std::mem::forget(session); }
                pa.leak_for_exit();
                return NativeExit::WorkerInFirmware;
            }
            idle();
        }
    }
    if pa.uri_backend_exit_pending() {
        if let Some(attempt) = pa.active_uri_attempt() {
            if !matches!(pa.backend_snapshot().phase,
                plx_playback_contract::Phase::Releasing | plx_playback_contract::Phase::Quarantined) {
                let at = pa.backend_tick();
                let _ = pa.backend_send_at(ps, plx_playback_contract::Intent::Stop {
                    backend: plx_playback_contract::BackendKind::Uri,
                    attempt: attempt.contract_id(),
                    reason: plx_playback_contract::StopReason::Exit,
                }, super::attempt_metrics::Origin::Fresh, at);
            }
        }
        let started = clock();
        while pa.uri_backend_exit_pending() {
            let at = pa.backend_tick();
            let _ = pa.backend_poll_at(ps, at);
            if !pa.uri_backend_exit_pending() { break; }
            if clock().saturating_duration_since(started) >= super::release::RELEASE_DEADLINE {
                log("exit: URI ownership unresolved; the display is left to the OS");
                pa.park_uri_backend_for_exit();
                pa.leak_for_exit();
                return NativeExit::WorkerInFirmware;
            }
            idle();
        }
    }
    // A stop that deferred leaves a preview whose Load had not returned installed: the release
    // machine owes nothing for it, so the loop below would call the display clean over a Load still
    // inside firmware. Wait for the Load as the exit waits for the URI driver and the release; when
    // it returns, run the deferred stop and let the loop wait for its Unload. The Engine is leaked
    // only at the deadline, never dropped under the thread that reads its payload.
    if pa.is_live() {
        let started = clock();
        loop {
            if !pa.defer_preview_join() {
                stop_bufferfeed(ps, pa);
                break;
            }
            if clock().saturating_duration_since(started) >= super::release::RELEASE_DEADLINE {
                log("exit: a preview's Load is still inside firmware; the display is left to the OS");
                pa.leak_for_exit();
                return NativeExit::WorkerInFirmware;
            }
            idle();
        }
    }
    reap_abandoned_load(pa);
    if pa.has_abandoned_load() {
        log("exit: a timed-out Load is still inside firmware; the display is left to the OS");
        pa.leak_for_exit();
        return NativeExit::WorkerInFirmware;
    }
    // No Feed composed: the URI waits above are the whole exit.
    if !ps.backends().feed {
        return NativeExit::Clean;
    }
    loop {
        let now = clock();
        let fx = drive_release(ps, pa, now, Some(now));
        match super::route_start::run_release_effects(ps, pa, fx, false).exit {
            Some(super::release::ReleaseEffect::ExitClean) => return NativeExit::Clean,
            Some(super::release::ReleaseEffect::ExitWorkerInFirmware) => {
                log("exit: the native Unload is overdue; the display is left to the OS");
                pa.leak_for_exit();
                return NativeExit::WorkerInFirmware;
            }
            _ => idle(),
        }
    }
}

/// Test helper: finish the pending release the way the exit does, with no deadline concerns.
#[cfg(all(test, feature = "hostsim"))]
pub(crate) fn finish_release_for_test(pa: &mut super::adapter::PlayerAdapter) {
    let mut ps = crate::route::PlaybackSession::IDLE;
    while pa.release_busy() {
        let fx = drive_release(&mut ps, pa, std::time::Instant::now(), None);
        let _ = super::route_start::run_release_effects(&mut ps, pa, fx, false);
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
        || ps.readout().state() != plx_playback_contract::readout::PlaybackState::Error
        || ps.readout().raw().frames == 0
        || crate::route::recovery_flight_outstanding()
        || crate::route::engineless_flight_outstanding()
    {
        return false;
    }
    engine::clear_failed_engine_release_arm();
    let cause = super::live_runtime_failure(ps);
    crate::route::note_engine_failure(ps, cause, super::intended_pos_ns(ps));
    log("player: the failed Engine's pipeline is released under the failure read-out");
    teardown(ps, pa, true, plx_playback_contract::StopReason::Replace);
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
        teardown(ps, pa, true, plx_playback_contract::StopReason::Suspend);
    }
    owns_attempt
}

/// Retire only the failed physical start the foreground is following. A replacement that won
/// the route between the frame tick and this callback keeps its native owner and readout.
pub fn suspend_playback_if_attempt(ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    attempt: crate::route::RouteStartAttempt) -> bool {
    if pa.active_uri_attempt() == Some(attempt) {
        teardown(ps, pa, true, plx_playback_contract::StopReason::Suspend);
        return true;
    }
    suspend_bufferfeed_if_attempt(ps, pa, attempt)
}

/// The foreground lifecycle follows `attempt` (it launched that Load, or took over following it):
/// from now on the Load call's own synchronous refusal is the lifecycle's to settle
/// (`PlayerAdapter::mark_foreground_follows_feed_load`). True when the adapter has bound that exact
/// attempt; a replaced one (a different attempt) is not marked.
pub fn foreground_follows_load(pa: &mut super::adapter::PlayerAdapter, attempt: crate::route::RouteStartAttempt) -> bool {
    pa.mark_foreground_follows_feed_load(attempt)
}

/// Common teardown owns the route fence, final report and scrobble ordering. The opaque native
/// stop owns the Engine, worker joins, transport close, native release and AU destruction.
pub(super) fn teardown(ps: &mut crate::route::PlaybackSession, pa: &mut PlayerAdapter,
    for_reload: bool, reason: plx_playback_contract::StopReason) {
    engine::clear_failed_engine_release_arm();
    pa.cancel_uri_recovery(ps);
    if let Some(route_attempt) = pa.active_uri_attempt() {
        let at = pa.backend_tick();
        let _ = pa.backend_send_at(ps, plx_playback_contract::Intent::Stop {
            backend: plx_playback_contract::BackendKind::Uri,
            attempt: route_attempt.contract_id(), reason,
        }, super::attempt_metrics::Origin::Fresh, at);
        let _ = begin_teardown(ps, pa, for_reload);
        let at = pa.backend_tick();
        let _ = pa.backend_poll_session_at(ps, at);
        return;
    }
    if let Some(route_attempt) = pa.active_feed_attempt() {
        let attempt = route_attempt.contract_id();
        let at = pa.backend_tick();
        let _ = pa.backend_send_at(
            ps,
            plx_playback_contract::Intent::Stop {
                backend: plx_playback_contract::BackendKind::Feed,
                attempt,
                reason,
            },
            super::attempt_metrics::Origin::Fresh,
            at,
        );
    }
    // The server ended this playback: the stop keeps the session for the read-out (`for_reload`), but
    // the Feed releases it as a terminal one (off the frame, an in-flight Load parked).
    let server_stopped = super::server_stop_applies(ps);
    if !begin_teardown(ps, pa, for_reload) { return; }
    {
    // An abandoned preview Load is never joined on the frame while native Load is in flight.
    if crate::route::is_preview(ps) && pa.defer_preview_join() {
        return;
    }
    begin_feed_teardown(for_reload);
    let route_attempt = pa.active_feed_attempt().expect("live feed has a route owner");
    // The outgoing attempt's last position, sampled while it is still the addressed one: the stop
    // retires the address and the reset after it idles the readout.
    pa.stop_begin(ps, server_stopped);
    let final_report = final_report(ps, for_reload);
    pa.stop_side_reader(for_reload);
    let mut report_th = stop_feed_timeline(pa, route_attempt);
    let unblocked = pa.stop_unblock();
    // The adaptive worker is joined: take its last report in (the carried link estimate a reload
    // hands on, the Original failure its probe noted, and the two members the terminal facts
    // carry), then close its channel. The reset below keeps the failure; a real stop clears it last.
    pa.drain_abr(ps);
    pa.close_abr_inbox();
    let mut terminal_facts = unblocked.attempt;
    ps.readout().fill_abr_facts(&mut terminal_facts);
    terminal_facts.started = Some(false);
    // The Load this teardown is about to park (its budget had expired) is a fact of THIS attempt: the
    // native phase says so only after the release below and outlives the session reset, which the
    // readout does not. A reload refused for that Load reads it from here (`pump::settle_reload`).
    terminal_facts.load_abandoned = Some(unblocked.load_abandoned);
    let _ = ps.ingest_attempt_facts(route_attempt, crate::player::attempt_view::Backend::Feed, terminal_facts);
    after_physical_close(ps, for_reload, final_report, &mut report_th);
    pa.stop_report_dispatched();
    pa.stop_release(for_reload);
    // A reload preserves the file's duration and viewer transport, then restarts the same item.
    // A real stop retires both along with the current claim hold.
    // Every reset of the Feed's session also retires the readout's view of it (`SessionReset`). A
    // real stop ends the playback and its end of stream with it: left standing, the finished play's
    // `ended` is what a replay finds while its start is deferred behind this Unload, the loop's
    // end-of-stream rule exits the replay's player page and that stop cancels the start.
    if for_reload {
        pa.reset_session(ResetScope::Reload, &mut || SHARED.reset_session_for_reload());
    } else {
        pa.reset_session(ResetScope::Stop, &mut || SHARED.reset_session());
        stop_transport(ps);
        super::claim_hold::clear();
    }
    ps.step_readout(super::readout::ReadoutEvent::SessionReset);
    finish_feed_teardown(ps, for_reload, report_th);
    pa.stop_finish(ps, for_reload);
    let at = pa.backend_tick();
    let _ = pa.backend_poll_at(ps, at);
    }
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
        pa.release_step(ps, &super::release::ReleaseEvent::CancelQueued, &mut fx);
        if ps.backends().feed {
            let _ = run_release_effects(ps, pa, fx, false);
        } else {
            run_uri_queued_effects(ps, pa, fx);
        }
    }
    // A prepared route whose native start failed has no Engine but still owns its exact PMS
    // resource and Session projection. A real stop must retire that ownership exactly once;
    // returning here used to leak the candidate forever while the reducer remained Failed.
    let feed_live = pa.is_live();
    if !feed_live {
        let uri_attempt = (ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri))
            .then(|| ps.attempt_view().attempt()).flatten();
        let mut uri_report_th = uri_attempt.and_then(|attempt| pa.stop_timeline(attempt));
        // An Engine-less flight (a resume, or a rollback no Engine waits on) has no Engine to take down with it, and an app-switch suspend
        // keeps the session for the foreground restore, which reserves its own start transaction:
        // drop the flight as a live Engine's suspend would (`Stable`, landing discarded, only its
        // replacement stopped). A full stop below clears it itself.
        if for_reload && (crate::route::engineless_flight_outstanding()
            || ps.attempt_view().backend() == Some(super::attempt_view::Backend::Uri)) {
            crate::route::begin_engine_teardown(true);
        }
        if !for_reload {
            // A seek reload can still be queued, and a failed Engine may already be released.
            // The session owns the final position in both cases, even without a live Engine.
            let feed_seek = uri_attempt.is_none()
                .then(|| ps.readout().unconfirmed_seek_target()).flatten();
            let mut final_report = None;
            if uri_attempt.is_some() && pa.finalize_uri_once() {
                let facts = ps.attempt_view().facts();
                let pos = final_position_ns(ps, facts.position_ns);
                let dur = facts.duration_ns.unwrap_or(0);
                crate::player::report::ended(ps, pos, dur);
                let rk = crate::route::cur_rk(ps);
                final_report = (!rk.is_empty() && dur > 0)
                    .then(|| (rk, pos / 1_000_000, dur / 1_000_000));
            } else if ps.backends().feed
                && (feed_seek.is_some() || crate::route::engine_failure(ps).is_some()) {
                let dur = super::duration_ns(ps).unwrap_or(0);
                let pos = final_position_ns(ps, feed_seek);
                crate::player::report::ended(ps, pos, dur);
                let rk = crate::route::cur_rk(ps);
                final_report = (!rk.is_empty() && dur > 0).then(|| (rk, pos / 1_000_000, dur / 1_000_000));
            }
            crate::route::scrobble_stop(ps, final_report, uri_report_th.take());
            crate::route::begin_engine_teardown(false);
            crate::route::drop_original_recovery(ps);
            crate::route::retire_stopped_playback(ps);
            pa.reset_session(ResetScope::Stop, &mut || SHARED.reset_session());
            stop_transport(ps);
            ps.step_readout(super::readout::ReadoutEvent::SessionReset);
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
    let Some(backend) = ps.attempt_view().backend() else { return; };
    let mut facts = *ps.attempt_view().facts();
    facts.started = Some(false);
    if backend == super::attempt_view::Backend::Uri && facts.runtime_failure.is_none() {
        facts.status = Some(plx_playback_contract::readout::PlaybackState::Idle);
    }
    let _ = ps.ingest_attempt_facts(attempt, backend, facts);
}

/// The position a playback's final report names: the one the server's terminating report carried when
/// the server ended it, else the last sample (`observed`, or where an unpresented attempt resumes).
fn final_position_ns(ps: &crate::route::PlaybackSession, observed: Option<i64>) -> i64 {
    if crate::route::engine_failure(ps) == Some(super::RuntimeFailure::ServerStopped) {
        return crate::route::unpresented_resume_ns(ps);
    }
    observed.unwrap_or_else(|| crate::route::unpresented_resume_ns(ps))
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
    // The duration is the session's last sample of the live attempt (the pump's, taken at the
    // frame's start and end), which is the container's duration for the whole of a playback.
    let dur = super::duration_ns(ps).unwrap_or(0);
    let readout = ps.readout();
    let pos_ns = final_position_ns(ps, readout.unconfirmed_seek_target().or(Some(readout.raw().playpos_ns)));
    if !for_reload {
        crate::player::report::ended(ps, pos_ns, dur);
    }
    if for_reload {
        None
    } else {
        let rk = crate::route::cur_rk(ps);
        if !rk.is_empty() && dur > 0 {
            Some((
                rk,
                pos_ns / 1_000_000,
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
    // The worker has stopped, so a reload hands route the last estimate it published, which the
    // replacement controller starts from (I8); a stop carries none.
    if for_reload {
        ps.carry_readout_seed();
    } else {
        ps.observe_link_seed(None);
    }
    if !for_reload {
        // **The carried link estimate dies with the PLAYBACK, not with the engine** (I8). A reload
        // is the same item on the same link at a new position — a seek, a quality pick, an
        // app-switch resume — and re-measuring the link from nothing across one is what made every
        // skip re-ramp the ladder for ten to twenty seconds.
        ps.step_readout(super::readout::ReadoutEvent::AbrSeed(None));
        // The failure is retained across the rollback reload only; a new playback must not carry
        // another item's PMS response into its diagnostics. Its own event, not the session's reset,
        // which a reload steps too.
        ps.step_readout(super::readout::ReadoutEvent::OriginalFailure(
            plx_playback_contract::adaptive::FailureEdit::Cleared,
        ));
        // A deferred Original recovery outlives nothing: the playback it belonged to is over, so
        // there is no frame left that could confirm it and no route left to roll back to. The
        // encoder it was holding is still running on the server, which is why this RETIRES rather
        // than forgets — see `route::drop_original_recovery`.
        crate::route::drop_original_recovery(ps);
        crate::route::retire_stopped_playback(ps); // the transcode stop rode out with `scrobble_stop` above
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

#[cfg(test)]
mod feed_start_controls_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn a_feed_start_uses_the_session_rate_after_a_uri_attempt() {
        let _serial = plx_base::testlock::serial();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) { crate::ff::force_speed_decoder_for_test(false); }
        }
        let _restore = Restore;
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::ff::force_speed_decoder_for_test(true);
        ps.accept_requested_rate(1500);
        ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(),
            super::super::attempt_view::Backend::Uri);
        // The previous Feed attempt or URI-to-Feed handoff left the offered rate at 1x.
        assert_eq!(ps.readout().play_rate_permille(), super::super::RATE_NORMAL);
        assert!(super::super::speed_offer(&ps).is_empty(),
            "the current URI UI must not offer Feed speed controls");
        assert!(super::super::feed_speed_offer(&ps).contains(&1500),
            "the resolved Feed candidate can satisfy 1.5x");

        assert_eq!(feed_start_rate(&mut ps).expect("Feed rate is available"), 1500,
            "the accepted session rate must configure the next physical Feed Load");
        assert_eq!(ps.readout().play_rate_permille(), 1500,
            "the physical payload preflight must see the same requested rate");
    }

    #[test]
    fn a_reconfigured_feed_start_captures_the_viewer_pause_goal() {
        let _serial = plx_base::testlock::serial();
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) { FEED_TX.commit_paused(self.0); }
        }
        let _restore = Restore(FEED_TX.paused.load(Ordering::Acquire));
        let mut ps = crate::route::PlaybackSession::IDLE;

        // The outgoing URI owned the viewer's Pause while the Feed mailbox was idle.
        FEED_TX.commit_paused(false);
        ps.set_reconfigure_pause_goal(true);
        assert!(feed_start_paused(&ps),
            "the native Load hold and InitialPlayback must both be Paused");

        // An old Feed hold must not pause the newly requested Playing attempt.
        FEED_TX.commit_paused(true);
        ps.set_reconfigure_pause_goal(false);
        assert!(!feed_start_paused(&ps),
            "the new Feed attempt must not inherit an old physical Pause");
    }

    /// A start that fails after it published the session's rate to the native Load leaves the
    /// outgoing offer as it found it; one that succeeds keeps the rate it published.
    #[test]
    fn a_failed_feed_start_puts_the_offered_rate_back_and_a_started_one_keeps_it() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        ps.step_readout(super::super::readout::ReadoutEvent::RateSelected { permille: 1250 });
        let publish = |ps: &mut crate::route::PlaybackSession| {
            ps.step_readout(super::super::readout::ReadoutEvent::RateSelected { permille: 1500 });
        };

        let failed: Result<(), ()> = restoring_offered_rate_on_failure(&mut ps, |ps| {
            publish(ps);
            assert_eq!(ps.readout().play_rate_permille(), 1500, "published before the refusal");
            Err(())
        });
        assert!(failed.is_err());
        assert_eq!(ps.readout().play_rate_permille(), 1250, "a refused candidate leaves the offer as found");

        let started: Result<(), ()> = restoring_offered_rate_on_failure(&mut ps, |ps| { publish(ps); Ok(()) });
        assert!(started.is_ok());
        assert_eq!(ps.readout().play_rate_permille(), 1500, "a started candidate keeps the rate it published");
    }

    /// A start that fails after it published the viewer's pause goal to the Feed's transport leaves
    /// the outgoing attempt's pause as it found it; one that succeeds keeps the goal it published.
    #[test]
    fn a_failed_feed_start_puts_the_pause_back_and_a_started_one_keeps_it() {
        let _serial = plx_base::testlock::serial();
        struct Restore(bool);
        impl Drop for Restore {
            fn drop(&mut self) { FEED_TX.commit_paused(self.0); }
        }
        let _restore = Restore(FEED_TX.paused.load(Ordering::Acquire));
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        FEED_TX.commit_paused(false);
        let mut ps = crate::route::PlaybackSession::IDLE;
        let publish = |ps: &mut crate::route::PlaybackSession,
            pa: &mut crate::player::adapter::PlayerAdapter| {
            crate::player::commit_paused(ps, pa, true);
            assert!(FEED_TX.paused.load(Ordering::Acquire), "published before the refusal");
            assert!(ps.readout().paused(), "and the readout's copy with it");
        };

        let failed: Result<(), ()> = restoring_pause_on_failure(&mut ps, &mut pa, |ps, pa| { publish(ps, pa); Err(()) });
        assert!(failed.is_err());
        assert!(!FEED_TX.paused.load(Ordering::Acquire), "a refused candidate leaves the pause as found");
        assert!(!ps.readout().paused(), "in both copies");

        let started: Result<(), ()> = restoring_pause_on_failure(&mut ps, &mut pa, |ps, pa| { publish(ps, pa); Ok(()) });
        assert!(started.is_ok());
        assert!(FEED_TX.paused.load(Ordering::Acquire), "a started candidate keeps the pause it published");
        assert!(ps.readout().paused());
    }

    #[test]
    fn an_unavailable_feed_rate_refuses_without_changing_the_physical_mailbox() {
        let _serial = plx_base::testlock::serial();
        struct Restore;
        impl Drop for Restore {
            fn drop(&mut self) { crate::ff::force_speed_decoder_for_test(false); }
        }
        let _restore = Restore;
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::ff::force_speed_decoder_for_test(true);
        ps.accept_requested_rate(1500);
        // The set already refused a PCM session, so the offer holds no speed above 1x.
        ps.step_readout(super::super::readout::ReadoutEvent::SpeedRefused);

        assert!(feed_start_rate(&mut ps).is_err(),
            "a requested speed the offer does not hold is refused, never reinterpreted as 1x");
        assert_eq!(ps.readout().play_rate_permille(), super::super::RATE_NORMAL);
        assert_eq!(ps.requested_rate_permille(), 1500,
            "a refused candidate does not alter the viewer's logical rate goal");
    }
}
