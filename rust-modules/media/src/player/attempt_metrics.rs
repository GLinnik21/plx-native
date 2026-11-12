#![deny(clippy::wildcard_enum_match_arm)]
//! Pure, local playback-attempt measurements. A logical request may try several physical
//! backends; their identities come from the route owner. This reducer neither mints an attempt
//! nor reads a clock, player registry, recorder, consent setting, URL, or user content. A caller
//! injects monotonic ticks. The production adapter consumes the typed effects into a bounded
//! local notice window; external error reporting retains its separate consent policy.

/// Retained physical summaries per logical request. Older summaries are discarded, while the
/// request's aggregate counts and durations continue saturating. This bounds recorder payloads.
const MAX_ATTEMPT_HISTORY: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tick { Known(u64), Unknown }
impl Tick {
    pub const fn from_millis(ms: u64) -> Self { Self::Known(ms) }
    pub const fn as_millis(self) -> Option<u64> {
        match self { Self::Known(ms) => Some(ms), Self::Unknown => None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend { Uri, Feed, Unknown }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin { Fresh, UserRetry, QualityChange, ViewerReconfigure, AutomaticHandoff, Recovery, ForegroundResume, Preview }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage { Planning, Open, Decode, Presentation, Release, Unknown }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause { Refused, Unsupported, Transport, Native, Timeout, Exhausted, Unknown }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failure { pub stage: Stage, pub cause: Cause }

/// These observations are sparse. Only `PicturePresented` confirms a picture; firmware command
/// acknowledgement, backend Playing and advancing content time have separate evidence slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence { CommandAcknowledged, PlaybackObserved, ClockAdvanced, PicturePresented }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SeenEvidence {
    pub command_acknowledged: bool,
    pub playback_observed: bool,
    pub clock_advanced: bool,
    pub picture_presented: bool,
}


/// What the viewer or backend is doing during the next interval. Unknown and intentional
/// pause/seek/hold time is never charged to involuntary rebuffer or handoff stall.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Condition { Playing, Rebuffering, Paused, Seeking, Held, Unknown }
impl Condition {
    fn eligible(self) -> bool { matches!(self, Self::Playing | Self::Rebuffering) }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttemptOutcome { Ended, Failed(Failure), Cancelled, Unconfirmed, Superseded }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestOutcome { Ended, Failed(Failure), Cancelled, Unconfirmed, Replaced }

/// Every answer names both the logical request and physical attempt, except request lifecycle
/// events. The route owner must supply identities and the tick; this model supplies neither.
pub enum Event<R, A> {
    RequestStarted { request: R, origin: Origin, at: Tick },
    AttemptStarted { request: R, attempt: A, backend: Backend, origin: Origin, at: Tick },
    Evidence { request: R, attempt: A, evidence: Evidence, at: Tick },
    Condition { request: R, attempt: A, condition: Condition, at: Tick },
    /// Recovery was accepted while the failed physical attempt still owns the route. The next
    /// attempt has no ID yet; begin the gap before native release and source resolution.
    HandoffBegun { request: R, from: A, at: Tick },
    Handoff { request: R, from: A, to: A, at: Tick },
    AttemptFinished { request: R, attempt: A, outcome: AttemptOutcome, at: Tick },
    RequestFinished { request: R, outcome: RequestOutcome, at: Tick },
}
impl<R, A> Event<R, A> {
    fn at(&self) -> Tick {
        match self {
            Self::RequestStarted { at, .. } | Self::AttemptStarted { at, .. }
            | Self::Evidence { at, .. } | Self::Condition { at, .. }
            | Self::HandoffBegun { at, .. } | Self::Handoff { at, .. }
            | Self::AttemptFinished { at, .. }
            | Self::RequestFinished { at, .. } => *at,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttemptSummary<A> {
    pub id: A,
    pub backend: Backend,
    pub origin: Origin,
    pub started_at: Tick,
    pub first_picture_at: Option<Tick>,
    pub startup_ms: Option<u64>,
    pub evidence: SeenEvidence,
    pub outcome: Option<AttemptOutcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestSummary<R, A> {
    pub id: R,
    pub origin: Origin,
    pub started_at: Tick,
    pub first_picture_at: Option<Tick>,
    /// Known, continuous time from logical request to its first confirmed picture.
    pub startup_ms: Option<u64>,
    pub outcome: Option<RequestOutcome>,
    pub attempt_count: u32,
    pub recent_attempts: Vec<AttemptSummary<A>>,
    pub current_attempt: Option<AttemptSummary<A>>,
    pub rebuffer_count: u32,
    pub rebuffer_ms: u64,
    pub handoff_count: u32,
    pub handoff_gap_ms: u64,
    /// Union of rebuffer and handoff intervals, counted once when they overlap.
    pub involuntary_stall_ms: u64,
    /// Accepted events without a monotonic tick. Their duration is excluded, not inferred.
    pub unknown_time_events: u32,
}

/// What one discrete record reports. Closed and local: it is never serialized, and it adds no
/// variant to `Backend`, `Stage` or `Origin`, whose domains the consent preview lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Incident {
    /// A physical attempt failed at the record's stage (`Open` is a failed start).
    Failed(Cause),
    /// An involuntary rebuffer began after the request's first confirmed picture.
    Rebuffer,
    /// The route owner accepted a switch away from this attempt's backend.
    Handoff,
}

/// One occurrence of an [`Incident`], addressed to its logical request and physical attempt.
/// `stage` is only what the evidence proves: a handoff or rebuffer whose attempt has shown no
/// evidence reports `Stage::Unknown` rather than a guessed default.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record<R, A> {
    pub request: R,
    pub attempt: A,
    pub backend: Backend,
    pub stage: Stage,
    pub incident: Incident,
    pub at: Tick,
}

/// Typed local results. The runner decides whether a terminal failure becomes an existing
/// consented handled report. A recovered refusal is a different observation, emitted only when
/// a later physical attempt presents its first picture. An `Incident` is the per-event record:
/// exactly one per accepted failure, rebuffer episode and handoff. Start rejections never reach
/// this reducer (no attempt exists) and release failures outlive the logical request; both are
/// recorded by the physical-lifecycle owner (`backend_metrics`), not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fx<R, A> {
    Incident(Record<R, A>),
    Finished(RequestSummary<R, A>),
    HandledError { request: R, attempt: Option<A>, backend: Backend, failure: Failure },
    RecoveredRefusal { request: R, refused_attempt: A, recovered_attempt: A, refused_backend: Backend },
}

#[derive(Clone, Copy)]
struct Handoff<A> { from: A }

impl<A: Copy> AttemptSummary<A> {
    /// The stage the attempt's own evidence proves. No evidence stays `Unknown`.
    fn proven_stage(&self) -> Stage {
        if self.evidence.picture_presented { Stage::Decode }
        else if self.evidence.playback_observed || self.evidence.command_acknowledged
            || self.evidence.clock_advanced { Stage::Presentation }
        else { Stage::Unknown }
    }

    fn record<R>(&self, request: R, stage: Stage, incident: Incident, at: Tick) -> Record<R, A> {
        Record { request, attempt: self.id, backend: self.backend, stage, incident, at }
    }
}

/// A single request is enough: the route owner serializes logical playback requests. The
/// caller may use its existing playback-level `plx_playback_contract::AttemptId` and a
/// route-owned physical identity such as `crate::route::RouteStartAttempt`; the reducer creates
/// neither. IDs must increase in the owner's canonical order. Persisted high-water marks reject
/// replay even after bounded history evicts an old summary. Completed summaries leave as effects.
/// IDs are data, not mutable capabilities. This closed bound keeps borrowed summaries inert.
pub trait Identity: Copy + Ord + identity::Sealed {}
mod identity {
    pub trait Sealed {}
    impl Sealed for u64 {}
    impl Sealed for super::super::backend::LogicalRequestId {}
    impl Sealed for plx_playback_contract::AttemptId {}
}
impl Identity for u64 {}
impl Identity for super::backend::LogicalRequestId {}
impl Identity for plx_playback_contract::AttemptId {}
pub struct AttemptMetrics<R: Identity, A: Identity> {
    request: Option<RequestSummary<R, A>>,
    last_request_id: Option<R>,
    last_attempt_id: Option<A>,
    condition: Condition,
    last_known_tick: Option<u64>,
    time_anchor: Option<u64>,
    request_start_clock_known: bool,
    attempt_start_clock_known: bool,
    handoff: Option<Handoff<A>>,
    last_handoff_to: Option<A>,
    pending_refusal: Option<(A, Backend)>,
}

impl<R: Identity, A: Identity> Default for AttemptMetrics<R, A> {
    fn default() -> Self { Self::new() }
}

impl<R: Identity, A: Identity> AttemptMetrics<R, A> {
    pub const fn new() -> Self {
        Self {
            request: None, last_request_id: None, last_attempt_id: None,
            condition: Condition::Unknown, last_known_tick: None,
            time_anchor: None, request_start_clock_known: false,
            attempt_start_clock_known: false,
            handoff: None, last_handoff_to: None, pending_refusal: None,
        }
    }

    pub fn snapshot(&self) -> Option<&RequestSummary<R, A>> { self.request.as_ref() }

    /// The stage a handoff leaves: the source attempt's own failure stage, else what its
    /// evidence proves. A source that neither failed nor showed evidence stays `Unknown`.
    fn handoff_stage(source: &AttemptSummary<A>) -> Stage {
        match source.outcome {
            Some(AttemptOutcome::Failed(Failure { stage, .. })) => stage,
            _ => source.proven_stage(),
        }
    }

    pub fn step(&mut self, event: Event<R, A>, effects: &mut Vec<Fx<R, A>>) {
        self.transition(event, effects);
    }


    fn transition(&mut self, event: Event<R, A>, effects: &mut Vec<Fx<R, A>>) {
        // Address validation precedes time accounting. A late answer must not extend the
        // newer request's stall merely because it carries a later timestamp.
        let admitted = match &event {
            Event::RequestStarted { request, .. } => self.last_request_id.is_none_or(|last| *request > last),
            Event::RequestFinished { request, .. } => {
                self.request.as_ref().is_some_and(|r| r.id == *request && r.outcome.is_none())
            }
            Event::AttemptStarted { request, attempt, .. } => self.request.as_ref().is_some_and(|r| {
                r.id == *request
                    && r.outcome.is_none()
                    && self.last_attempt_id.is_none_or(|last| *attempt > last)
                    && !r.current_attempt.is_some_and(|a| a.id == *attempt)
                    && !r.recent_attempts.iter().any(|a| a.id == *attempt)
            }),
            Event::Evidence { request, attempt, .. }
            | Event::Condition { request, attempt, .. }
            | Event::AttemptFinished { request, attempt, .. } => self.request.as_ref().is_some_and(|r| {
                r.id == *request
                    && r.outcome.is_none()
                    && r.current_attempt.is_some_and(|a| a.id == *attempt && a.outcome.is_none())
            }),
            Event::HandoffBegun { request, from, .. } => self.request.as_ref().is_some_and(|r| {
                r.id == *request
                    && r.outcome.is_none()
                    && r.current_attempt.is_some_and(|a| a.id == *from)
                    && self.handoff.is_none()
            }),
            Event::Handoff { request, from, to, .. } => self.request.as_ref().is_some_and(|r| {
                r.id == *request
                    && r.outcome.is_none()
                    && to > from
                    && r.current_attempt.is_some_and(|a| a.id == *from)
                    && self.handoff.is_none()
                    && self.last_handoff_to != Some(*to)
            }),
        };
        if !admitted {
            return;
        }
        let at = event.at();
        if let Tick::Known(to) = at {
            if self.last_known_tick.is_some_and(|last| to < last) {
                return;
            }
            let elapsed = self.time_anchor.map_or(0, |from| to - from);
            if let Some(request) = self.request.as_mut().filter(|r| r.outcome.is_none()) {
                let eligible = self.condition.eligible();
                let rebuffer = eligible && self.condition == Condition::Rebuffering && request.first_picture_at.is_some();
                let handoff = eligible && self.handoff.is_some();
                if rebuffer {
                    request.rebuffer_ms = request.rebuffer_ms.saturating_add(elapsed);
                }
                if handoff {
                    request.handoff_gap_ms = request.handoff_gap_ms.saturating_add(elapsed);
                }
                if rebuffer || handoff {
                    request.involuntary_stall_ms = request.involuntary_stall_ms.saturating_add(elapsed);
                }
            }
            self.last_known_tick = Some(to);
            self.time_anchor = Some(to);
        } else {
            // An unavailable clock breaks the interval. The next known tick starts a new
            // anchor; no duration is invented for the unknown span.
            self.time_anchor = None;
            if let Some(request) = self.request.as_mut().filter(|r| r.outcome.is_none()) {
                request.unknown_time_events = request.unknown_time_events.saturating_add(1);
            }
            self.request_start_clock_known = false;
            self.attempt_start_clock_known = false;
        }
        let finish = match &event {
            Event::RequestStarted { .. } => Some(RequestOutcome::Replaced),
            Event::RequestFinished { outcome, .. } => Some(*outcome),
            Event::AttemptStarted { .. }
            | Event::Evidence { .. }
            | Event::Condition { .. }
            | Event::HandoffBegun { .. }
            | Event::Handoff { .. }
            | Event::AttemptFinished { .. } => None,
        };
        if let Some(outcome) = finish {
            if let Some(request) = self.request.as_mut().filter(|r| r.outcome.is_none()) {
                // An End without independent picture evidence is an unconfirmed playback, never a
                // successful completion. Explicit failure and cancellation retain their own classes.
                let outcome = if outcome == RequestOutcome::Ended && request.first_picture_at.is_none() {
                    RequestOutcome::Unconfirmed
                } else {
                    outcome
                };
                if let Some(attempt) = request.current_attempt.as_mut() {
                    if attempt.outcome.is_none() {
                        attempt.outcome = Some(match outcome {
                            RequestOutcome::Ended if attempt.first_picture_at.is_some() => AttemptOutcome::Ended,
                            RequestOutcome::Ended => AttemptOutcome::Unconfirmed,
                            RequestOutcome::Failed(failure) => AttemptOutcome::Failed(failure),
                            RequestOutcome::Cancelled => AttemptOutcome::Cancelled,
                            RequestOutcome::Unconfirmed => AttemptOutcome::Unconfirmed,
                            RequestOutcome::Replaced => AttemptOutcome::Superseded,
                        });
                    }
                }
                request.outcome = Some(outcome);
                if let RequestOutcome::Failed(failure) = outcome {
                    effects.push(Fx::HandledError {
                        request: request.id,
                        attempt: request.current_attempt.map(|a| a.id),
                        backend: request.current_attempt.map_or(Backend::Unknown, |a| a.backend),
                        failure,
                    });
                }
                effects.push(Fx::Finished(request.clone()));
                self.handoff = None;
                self.pending_refusal = None;
            }
        }
        match event {
            Event::RequestStarted { request, origin, at } => {
                if self.request.as_ref().is_some_and(|old| old.id == request) {
                    return;
                }
                self.request = Some(RequestSummary {
                    id: request,
                    origin,
                    started_at: at,
                    first_picture_at: None,
                    startup_ms: None,
                    outcome: None,
                    attempt_count: 0,
                    recent_attempts: Vec::new(),
                    current_attempt: None,
                    rebuffer_count: 0,
                    rebuffer_ms: 0,
                    handoff_count: 0,
                    handoff_gap_ms: 0,
                    involuntary_stall_ms: 0,
                    unknown_time_events: u32::from(at == Tick::Unknown),
                });
                self.last_request_id = Some(request);
                self.last_attempt_id = None;
                self.condition = Condition::Unknown;
                self.time_anchor = at.as_millis();
                if let Some(ms) = at.as_millis() {
                    self.last_known_tick = Some(ms);
                }
                self.request_start_clock_known = at.as_millis().is_some();
                self.attempt_start_clock_known = false;
                self.handoff = None;
                self.last_handoff_to = None;
                self.pending_refusal = None;
            }
            Event::RequestFinished { .. } => {}
            Event::AttemptStarted { request, attempt, backend, origin, at } => {
                let Some(r) = self.request.as_mut().filter(|r| r.id == request && r.outcome.is_none()) else { return };
                if r.current_attempt.is_some_and(|a| a.id == attempt) || r.recent_attempts.iter().any(|a| a.id == attempt) {
                    return;
                }
                if let Some(mut attempt) = r.current_attempt.take() {
                    if attempt.outcome.is_none() {
                        attempt.outcome = Some(if attempt.first_picture_at.is_some() {
                            AttemptOutcome::Superseded
                        } else {
                            AttemptOutcome::Unconfirmed
                        });
                    }
                    if r.recent_attempts.len() == MAX_ATTEMPT_HISTORY {
                        r.recent_attempts.remove(0);
                    }
                    r.recent_attempts.push(attempt);
                }
                r.attempt_count = r.attempt_count.saturating_add(1);
                r.current_attempt = Some(AttemptSummary {
                    id: attempt,
                    backend,
                    origin,
                    started_at: at,
                    first_picture_at: None,
                    startup_ms: None,
                    evidence: SeenEvidence::default(),
                    outcome: None,
                });
                self.last_attempt_id = Some(attempt);
                self.attempt_start_clock_known = at.as_millis().is_some();
            }
            Event::Evidence { request, attempt, evidence, at } => {
                let Some(r) = self.request.as_mut().filter(|r| r.id == request && r.outcome.is_none()) else { return };
                let Some(current) = r.current_attempt.as_mut().filter(|a| a.id == attempt && a.outcome.is_none()) else { return };
                match evidence {
                    Evidence::CommandAcknowledged => current.evidence.command_acknowledged = true,
                    Evidence::PlaybackObserved => current.evidence.playback_observed = true,
                    Evidence::ClockAdvanced => current.evidence.clock_advanced = true,
                    Evidence::PicturePresented => current.evidence.picture_presented = true,
                }
                if evidence != Evidence::PicturePresented || current.first_picture_at.is_some() {
                    return;
                }
                current.first_picture_at = Some(at);
                if self.attempt_start_clock_known {
                    current.startup_ms =
                        current.started_at.as_millis().zip(at.as_millis()).map(|(start, end)| end.saturating_sub(start));
                }
                let first_for_request = r.first_picture_at.is_none();
                if first_for_request {
                    r.first_picture_at = Some(at);
                    if self.request_start_clock_known {
                        r.startup_ms = r.started_at.as_millis().zip(at.as_millis()).map(|(start, end)| end.saturating_sub(start));
                    }
                }
                // Startup buffering becomes rebuffering only after the first picture; an
                // already-counted episode stays one episode across a backend handoff.
                if first_for_request && self.condition == Condition::Rebuffering {
                    r.rebuffer_count = r.rebuffer_count.saturating_add(1);
                    if let Some(a) = r.current_attempt {
                        effects.push(Fx::Incident(a.record(request, a.proven_stage(), Incident::Rebuffer, at)));
                    }
                }
                // A failed first target may be followed by another physical try. The gap
                // ends at the first successor picture, not only the original target's.
                if self.handoff.is_some_and(|h| h.from != attempt) {
                    self.handoff = None;
                }
                if let Some((refused_attempt, backend)) = self.pending_refusal.take().filter(|(old, _)| *old != attempt) {
                    effects.push(Fx::RecoveredRefusal {
                        request,
                        refused_attempt,
                        recovered_attempt: attempt,
                        refused_backend: backend,
                    });
                }
            }
            Event::Condition { request, attempt, condition, at } => {
                let Some(r) = self.request.as_mut().filter(|r| r.id == request && r.outcome.is_none()) else { return };
                let Some(current) = r.current_attempt.filter(|a| a.id == attempt && a.outcome.is_none()) else { return };
                if condition == Condition::Rebuffering && self.condition != Condition::Rebuffering && r.first_picture_at.is_some()
                {
                    r.rebuffer_count = r.rebuffer_count.saturating_add(1);
                    effects.push(Fx::Incident(current.record(request, current.proven_stage(), Incident::Rebuffer, at)));
                }
                self.condition = condition;
            }
            Event::HandoffBegun { request, from, at } => {
                let Some(r) = self.request.as_mut().filter(|r| r.id == request && r.outcome.is_none()) else { return };
                let Some(source) = r.current_attempt.filter(|a| a.id == from) else { return };
                if self.handoff.is_some() {
                    return;
                }
                r.handoff_count = r.handoff_count.saturating_add(1);
                self.handoff = Some(Handoff { from });
                effects.push(Fx::Incident(source.record(request, Self::handoff_stage(&source), Incident::Handoff, at)));
            }
            Event::Handoff { request, from, to, at } => {
                let Some(r) = self.request.as_mut().filter(|r| r.id == request && r.outcome.is_none()) else { return };
                let Some(source) = r.current_attempt.filter(|a| a.id == from) else { return };
                if to <= from || self.handoff.is_some() || self.last_handoff_to == Some(to) {
                    return;
                }
                r.handoff_count = r.handoff_count.saturating_add(1);
                self.handoff = Some(Handoff { from });
                self.last_handoff_to = Some(to);
                effects.push(Fx::Incident(source.record(request, Self::handoff_stage(&source), Incident::Handoff, at)));
            }
            Event::AttemptFinished { request, attempt, outcome, at } => {
                let Some(r) = self.request.as_mut().filter(|r| r.id == request && r.outcome.is_none()) else { return };
                let Some(current) = r.current_attempt.as_mut().filter(|a| a.id == attempt && a.outcome.is_none()) else { return };
                current.outcome = Some(if outcome == AttemptOutcome::Ended && current.first_picture_at.is_none() {
                    AttemptOutcome::Unconfirmed
                } else {
                    outcome
                });
                if let AttemptOutcome::Failed(Failure { stage, cause }) = outcome {
                    if stage != Stage::Release {
                        effects.push(Fx::Incident(current.record(request, stage, Incident::Failed(cause), at)));
                    }
                }
                if matches!(outcome, AttemptOutcome::Failed(Failure { cause: Cause::Refused, .. })) {
                    self.pending_refusal = Some((attempt, current.backend));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type Model = AttemptMetrics<u64, u64>;
    fn t(ms: u64) -> Tick { Tick::from_millis(ms) }
    #[test]
    fn rejected_events_preserve_every_field_and_the_next_interval() {
        let mut m = Model::new();
        start(&mut m, 1, 10);
        send(&mut m, picture(1, 10, 10));
        send(&mut m, condition(1, 10, 20, Condition::Rebuffering));
        send(&mut m, Event::HandoffBegun { request: 1, from: 10, at: t(30) });
        let before = (m.request.clone(), m.last_request_id, m.last_attempt_id, m.condition,
            m.last_known_tick, m.time_anchor, m.request_start_clock_known, m.attempt_start_clock_known,
            m.handoff.map(|h| h.from), m.last_handoff_to, m.pending_refusal);
        for ev in [
            Event::Evidence { request: 9, attempt: 10, evidence: Evidence::PicturePresented, at: Tick::Unknown },
            Event::AttemptStarted { request: 1, attempt: 9, backend: Backend::Feed, origin: Origin::Recovery, at: t(100) },
            condition(1, 10, 5, Condition::Paused),
        ] {
            assert!(send(&mut m, ev).is_empty());
            assert_eq!((m.request.clone(), m.last_request_id, m.last_attempt_id, m.condition,
                m.last_known_tick, m.time_anchor, m.request_start_clock_known, m.attempt_start_clock_known,
                m.handoff.map(|h| h.from), m.last_handoff_to, m.pending_refusal), before);
        }
        assert!(send(&mut m, condition(1, 10, 50, Condition::Playing)).is_empty());
        let s = m.snapshot().unwrap();
        assert_eq!((s.rebuffer_count, s.rebuffer_ms, s.handoff_count, s.handoff_gap_ms, s.involuntary_stall_ms), (1, 30, 1, 20, 30));
    }

    #[test]
    fn terminal_effects_follow_accounting_and_precede_replacement() {
        let mut m = Model::new();
        start(&mut m, 1, 10);
        send(&mut m, picture(1, 10, 10));
        send(&mut m, condition(1, 10, 20, Condition::Rebuffering));
        let fx = send(&mut m, Event::RequestStarted { request: 2, origin: Origin::UserRetry, at: t(50) });
        let [Fx::Finished(old)] = fx.as_slice() else { panic!("replacement must finish the old request once") };
        assert_eq!(old.outcome, Some(RequestOutcome::Replaced));
        assert_eq!((old.rebuffer_ms, old.involuntary_stall_ms), (30, 30));
        assert_eq!(old.current_attempt.unwrap().outcome, Some(AttemptOutcome::Superseded));
        assert_eq!(m.snapshot().unwrap().id, 2);
        assert_eq!(m.snapshot().unwrap().attempt_count, 0);
        assert_eq!((m.last_attempt_id, m.condition, m.time_anchor), (None, Condition::Unknown, Some(50)));
        send(&mut m, Event::AttemptStarted { request: 2, attempt: 20, backend: Backend::Feed, origin: Origin::UserRetry, at: t(60) });
        let failure = Failure { stage: Stage::Open, cause: Cause::Refused };
        let fx = send(&mut m, Event::RequestFinished { request: 2, outcome: RequestOutcome::Failed(failure), at: t(70) });
        assert_eq!(fx, vec![
            Fx::HandledError { request: 2, attempt: Some(20), backend: Backend::Feed, failure },
            Fx::Finished(m.snapshot().unwrap().clone()),
        ]);
        assert!(m.handoff.is_none() && m.pending_refusal.is_none());
    }

    fn start(model: &mut Model, request: u64, attempt: u64) {
        let mut effects = Vec::new();
        model.step(Event::RequestStarted { request, origin: Origin::Fresh, at: t(0) }, &mut effects);
        model.step(Event::AttemptStarted { request, attempt, backend: Backend::Uri, origin: Origin::Fresh, at: t(0) }, &mut effects);
        assert!(effects.is_empty());
    }
    fn send(model: &mut Model, event: Event<u64, u64>) -> Vec<Fx<u64, u64>> {
        let mut effects = Vec::new();
        model.step(event, &mut effects);
        effects
    }
    fn picture(request: u64, attempt: u64, at: u64) -> Event<u64, u64> {
        Event::Evidence { request, attempt, evidence: Evidence::PicturePresented, at: t(at) }
    }
    fn condition(request: u64, attempt: u64, at: u64, value: Condition) -> Event<u64, u64> {
        Event::Condition { request, attempt, condition: value, at: t(at) }
    }

    #[test]
    fn stale_physical_reply_cannot_confirm_picture_or_advance_new_attempt_time() {
        let mut model = Model::new();
        start(&mut model, 1, 11);
        send(&mut model, picture(1, 11, 10));
        send(&mut model, condition(1, 11, 20, Condition::Rebuffering));
        send(&mut model, Event::AttemptStarted {
            request: 1, attempt: 12, backend: Backend::Feed, origin: Origin::Recovery, at: t(30),
        });
        send(&mut model, picture(1, 11, 100)); // late from the retired URI attempt
        send(&mut model, picture(1, 12, 40));
        let s = model.snapshot().unwrap();
        assert_eq!(s.first_picture_at, Some(t(10)));
        assert_eq!(s.current_attempt.unwrap().first_picture_at, Some(t(40)));
        assert_eq!(s.rebuffer_ms, 20, "the stale reply at 100 ms charged no time");
        assert_eq!(s.recent_attempts[0].outcome, Some(AttemptOutcome::Superseded));
    }

    #[test]
    fn intentional_and_unknown_intervals_do_not_become_rebuffer_time() {
        let mut model = Model::new();
        start(&mut model, 2, 21);
        send(&mut model, picture(2, 21, 10));
        for (at, state) in [
            (20, Condition::Rebuffering), (30, Condition::Paused), (40, Condition::Seeking),
            (50, Condition::Held), (60, Condition::Unknown), (70, Condition::Rebuffering),
            (80, Condition::Playing),
        ] { send(&mut model, condition(2, 21, at, state)); }
        send(&mut model, condition(2, 21, 90, Condition::Rebuffering));
        send(&mut model, Event::Condition { request: 2, attempt: 21, condition: Condition::Rebuffering, at: Tick::Unknown });
        send(&mut model, condition(2, 21, 100, Condition::Playing));
        let s = model.snapshot().unwrap();
        assert_eq!(s.rebuffer_count, 3);
        assert_eq!(s.rebuffer_ms, 20);
        assert_eq!(s.involuntary_stall_ms, 20);
    }

    #[test]
    fn handoff_gap_counts_once_and_overlapping_rebuffer_is_a_union() {
        let mut model = Model::new();
        start(&mut model, 3, 31);
        send(&mut model, picture(3, 31, 10));
        send(&mut model, condition(3, 31, 20, Condition::Rebuffering));
        send(&mut model, Event::Handoff { request: 3, from: 31, to: 32, at: t(30) });
        send(&mut model, Event::Handoff { request: 3, from: 31, to: 32, at: t(31) });
        send(&mut model, Event::AttemptStarted {
            request: 3, attempt: 32, backend: Backend::Feed, origin: Origin::AutomaticHandoff, at: t(35),
        });
        send(&mut model, picture(3, 32, 50));
        send(&mut model, condition(3, 32, 60, Condition::Playing));
        let s = model.snapshot().unwrap();
        assert_eq!(s.handoff_count, 1);
        assert_eq!(s.handoff_gap_ms, 20);
        assert_eq!(s.rebuffer_ms, 40);
        assert_eq!(s.involuntary_stall_ms, 40, "the 20 ms overlap is charged once");
    }

    #[test]
    fn automatic_handoff_gap_includes_release_and_resolve_after_recovery_decision() {
        let mut model = Model::new();
        start(&mut model, 6, 61);
        send(&mut model, picture(6, 61, 10));
        send(&mut model, condition(6, 61, 10, Condition::Playing));
        let recovery_decided_at = 20;
        send(&mut model, Event::AttemptFinished { request: 6, attempt: 61,
            outcome: AttemptOutcome::Failed(Failure { stage: Stage::Decode, cause: Cause::Native }),
            at: t(recovery_decided_at) });
        // Native Closing and source resolution occupy 20..40; the physical successor has no
        // AttemptId until after that work. Duplicates and a stale request cannot move the clock.
        send(&mut model, Event::HandoffBegun { request: 6, from: 61,
            at: t(recovery_decided_at) });
        send(&mut model, Event::HandoffBegun { request: 6, from: 61, at: t(21) });
        send(&mut model, Event::HandoffBegun { request: 5, from: 61, at: t(100) });
        send(&mut model, Event::Handoff { request: 6, from: 61, to: 62, at: t(40) });
        send(&mut model, Event::AttemptStarted { request: 6, attempt: 62,
            backend: Backend::Feed, origin: Origin::AutomaticHandoff, at: t(40) });
        send(&mut model, picture(6, 62, 60));
        let summary = model.snapshot().unwrap();
        assert_eq!(summary.handoff_count, 1);
        assert_eq!(summary.handoff_gap_ms, 60 - recovery_decided_at,
            "the visible gap begins when recovery is chosen, before native release");
        assert_eq!(summary.current_attempt.unwrap().first_picture_at, Some(t(60)));
    }

    #[test]
    fn failed_handoff_target_keeps_one_gap_until_a_later_successor_picture() {
        let mut model = Model::new();
        start(&mut model, 9, 91);
        send(&mut model, picture(9, 91, 10));
        send(&mut model, condition(9, 91, 10, Condition::Playing));
        send(&mut model, Event::Handoff { request: 9, from: 91, to: 92, at: t(20) });
        send(&mut model, Event::AttemptStarted {
            request: 9, attempt: 92, backend: Backend::Feed,
            origin: Origin::AutomaticHandoff, at: t(25),
        });
        send(&mut model, Event::AttemptFinished {
            request: 9, attempt: 92,
            outcome: AttemptOutcome::Failed(Failure { stage: Stage::Open, cause: Cause::Transport }),
            at: t(30),
        });
        send(&mut model, Event::AttemptStarted {
            request: 9, attempt: 93, backend: Backend::Feed, origin: Origin::Recovery, at: t(35),
        });
        send(&mut model, picture(9, 93, 40));
        let s = model.snapshot().unwrap();
        assert_eq!(s.handoff_count, 1);
        assert_eq!(s.handoff_gap_ms, 20);
    }

    #[test]
    fn refusal_recovery_needs_picture_and_terminal_handled_error_emits_once() {
        let mut model = Model::new();
        start(&mut model, 4, 41);
        let refusal = Failure { stage: Stage::Open, cause: Cause::Refused };
        send(&mut model, Event::AttemptFinished { request: 4, attempt: 41, outcome: AttemptOutcome::Failed(refusal), at: t(10) });
        send(&mut model, Event::Handoff { request: 4, from: 41, to: 42, at: t(11) });
        send(&mut model, Event::AttemptStarted {
            request: 4, attempt: 42, backend: Backend::Feed, origin: Origin::Recovery, at: t(12),
        });
        for (at, evidence) in [(13, Evidence::CommandAcknowledged), (14, Evidence::PlaybackObserved), (15, Evidence::ClockAdvanced)] {
            assert!(send(&mut model, Event::Evidence { request: 4, attempt: 42, evidence, at: t(at) }).is_empty());
        }
        assert!(model.snapshot().unwrap().first_picture_at.is_none());
        let recovered = send(&mut model, picture(4, 42, 20));
        assert_eq!(recovered.as_slice(), &[Fx::RecoveredRefusal {
            request: 4, refused_attempt: 41, recovered_attempt: 42, refused_backend: Backend::Uri,
        }]);
        assert!(send(&mut model, picture(4, 42, 21)).is_empty());
        let failure = Failure { stage: Stage::Decode, cause: Cause::Native };
        let terminal = send(&mut model, Event::RequestFinished { request: 4, outcome: RequestOutcome::Failed(failure), at: t(30) });
        assert!(matches!(terminal.as_slice(), [Fx::HandledError { failure: f, .. }, Fx::Finished(_)] if *f == failure));
        assert!(send(&mut model, Event::RequestFinished { request: 4, outcome: RequestOutcome::Failed(failure), at: t(31) }).is_empty());
    }

    #[test]
    fn cancellation_and_unconfirmed_end_are_distinct_from_failure() {
        let mut model = Model::new();
        start(&mut model, 5, 51);
        let end = send(&mut model, Event::RequestFinished { request: 5, outcome: RequestOutcome::Ended, at: t(10) });
        assert!(matches!(end.as_slice(), [Fx::Finished(s)] if s.outcome == Some(RequestOutcome::Unconfirmed)));
        send(&mut model, Event::RequestStarted { request: 6, origin: Origin::Fresh, at: t(20) });
        send(&mut model, Event::AttemptStarted { request: 6, attempt: 61, backend: Backend::Feed, origin: Origin::Fresh, at: t(20) });
        let cancel = send(&mut model, Event::RequestFinished { request: 6, outcome: RequestOutcome::Cancelled, at: t(25) });
        assert!(matches!(cancel.as_slice(), [Fx::Finished(s)] if s.outcome == Some(RequestOutcome::Cancelled)));
    }

    #[test]
    fn unknown_startup_time_stays_unknown_after_a_real_picture() {
        let mut model = Model::new();
        start(&mut model, 8, 81);
        send(&mut model, Event::Evidence {
            request: 8, attempt: 81, evidence: Evidence::ClockAdvanced, at: t(5),
        });
        send(&mut model, Event::Condition {
            request: 8, attempt: 81, condition: Condition::Unknown, at: Tick::Unknown,
        });
        send(&mut model, picture(8, 81, 20));
        let s = model.snapshot().unwrap();
        assert_eq!(s.first_picture_at, Some(t(20)));
        assert_eq!(s.startup_ms, None, "no duration spans an unknown tick");
        assert_eq!(s.current_attempt.unwrap().startup_ms, None);
        assert_eq!(s.unknown_time_events, 1, "the missing time remains visible in the summary");
    }

    #[test]
    fn old_attempt_history_is_bounded_without_losing_aggregate_count() {
        let mut model = Model::new();
        start(&mut model, 7, 1);
        for attempt in 2..=12 {
            send(&mut model, Event::AttemptStarted {
                request: 7, attempt, backend: Backend::Feed, origin: Origin::Recovery, at: t(attempt),
            });
        }
        let s = model.snapshot().unwrap();
        assert_eq!(s.attempt_count, 12);
        assert_eq!(s.recent_attempts.len(), MAX_ATTEMPT_HISTORY);
        assert_eq!(s.recent_attempts[0].id, 4);
        assert_eq!(s.current_attempt.unwrap().id, 12);
        send(&mut model, Event::AttemptStarted {
            request: 7, attempt: 1, backend: Backend::Uri, origin: Origin::Recovery, at: t(100),
        });
        assert_eq!(model.snapshot().unwrap().attempt_count, 12, "eviction cannot reopen old physical ID 1");
    }

    #[test]
    fn completed_logical_request_cannot_replay_after_a_newer_one() {
        let mut model = Model::new();
        start(&mut model, 10, 101);
        let failure = Failure { stage: Stage::Open, cause: Cause::Timeout };
        let first = send(&mut model, Event::RequestFinished {
            request: 10, outcome: RequestOutcome::Failed(failure), at: t(10),
        });
        assert!(matches!(first.as_slice(), [Fx::HandledError { .. }, Fx::Finished(_)]));
        send(&mut model, Event::RequestStarted { request: 11, origin: Origin::Fresh, at: t(20) });
        let replay = send(&mut model, Event::RequestStarted { request: 10, origin: Origin::Fresh, at: t(30) });
        assert!(replay.is_empty());
        assert_eq!(model.snapshot().unwrap().id, 11);
    }

    #[test]
    fn physical_end_without_picture_is_unconfirmed() {
        let mut model = Model::new();
        start(&mut model, 12, 121);
        send(&mut model, Event::AttemptFinished {
            request: 12, attempt: 121, outcome: AttemptOutcome::Ended, at: t(5),
        });
        assert_eq!(model.snapshot().unwrap().current_attempt.unwrap().outcome, Some(AttemptOutcome::Unconfirmed));
    }

    fn record(attempt: u64, stage: Stage, incident: Incident, at: u64) -> Fx<u64, u64> {
        Fx::Incident(Record { request: 1, attempt, backend: Backend::Uri, stage, incident, at: t(at) })
    }

    #[test]
    fn a_handoff_is_one_incident_and_its_stage_is_only_what_the_source_proves() {
        let mut model = Model::new();
        start(&mut model, 1, 11);
        // The source has neither failed nor shown any evidence: the stage stays Unknown.
        assert_eq!(send(&mut model, Event::HandoffBegun { request: 1, from: 11, at: t(5) }),
            [record(11, Stage::Unknown, Incident::Handoff, 5)]);
        assert!(send(&mut model, Event::HandoffBegun { request: 1, from: 11, at: t(6) }).is_empty());
        assert!(send(&mut model, Event::Handoff { request: 1, from: 11, to: 12, at: t(7) }).is_empty(),
            "the successor Start completes the begun handoff and is not a second one");
        assert_eq!(model.snapshot().unwrap().handoff_count, 1);
    }

    #[test]
    fn a_failed_attempt_is_one_incident_at_its_stage_and_release_faults_are_not_recorded_here() {
        let mut model = Model::new();
        start(&mut model, 1, 11);
        let open = Failure { stage: Stage::Open, cause: Cause::Timeout };
        assert_eq!(send(&mut model, Event::AttemptFinished { request: 1, attempt: 11,
            outcome: AttemptOutcome::Failed(open), at: t(4) }),
            [record(11, Stage::Open, Incident::Failed(Cause::Timeout), 4)]);
        assert!(send(&mut model, Event::AttemptFinished { request: 1, attempt: 11,
            outcome: AttemptOutcome::Failed(open), at: t(5) }).is_empty(), "a finished attempt answers once");
        send(&mut model, Event::AttemptStarted { request: 1, attempt: 12, backend: Backend::Uri,
            origin: Origin::Recovery, at: t(6) });
        // The release of a native resource outlives the request; the physical owner records it.
        assert!(send(&mut model, Event::AttemptFinished { request: 1, attempt: 12,
            outcome: AttemptOutcome::Failed(Failure { stage: Stage::Release, cause: Cause::Native }),
            at: t(7) }).is_empty());
    }

    #[test]
    fn a_rebuffer_reports_the_stage_its_attempt_evidence_proves() {
        let mut model = Model::new();
        start(&mut model, 1, 11);
        send(&mut model, picture(1, 11, 10));
        assert_eq!(send(&mut model, condition(1, 11, 20, Condition::Rebuffering)),
            [record(11, Stage::Decode, Incident::Rebuffer, 20)]);
        assert!(send(&mut model, condition(1, 11, 25, Condition::Rebuffering)).is_empty());
        // A second attempt that has shown nothing yet reports Unknown, not a borrowed stage.
        send(&mut model, condition(1, 11, 30, Condition::Playing));
        send(&mut model, Event::AttemptStarted { request: 1, attempt: 12, backend: Backend::Feed,
            origin: Origin::Recovery, at: t(31) });
        let fx = send(&mut model, Event::Condition { request: 1, attempt: 12,
            condition: Condition::Rebuffering, at: t(40) });
        assert!(matches!(fx.as_slice(), [Fx::Incident(Record { attempt: 12, backend: Backend::Feed,
            stage: Stage::Unknown, incident: Incident::Rebuffer, .. })]));
    }
}
