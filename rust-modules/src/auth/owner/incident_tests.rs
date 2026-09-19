//! The onboarding incident offer, driven through the owner's own step: a producer's failure, the
//! permission decision as a command, the person's answer, the adapter's delivery. Synthetic
//! values only.

use super::super::*;
use super::*;
use crate::auth::LoginProgress;
use crate::telemetry::incident::{IncidentContext, IncidentKind, InternalClass, LinkClass};

struct OwnerHost;
impl crate::ui::machine::Host for OwnerHost {
    type Arg = crate::ui::fixture::FixtureArg;
    type Fx = SessionFx;
    type Msg = SessionEvent;
    type Elem = u32;
    type Views<'a> = SessionRead<'a>;
    type Init = SessionInit;
    type Memory = ();
}
impl SessionHost for OwnerHost {
    fn session_effect(effect: SessionFx) -> SessionFx {
        effect
    }
}

fn step(owner: &mut SessionMachine, event: SessionEvent) -> Vec<SessionFx> {
    use crate::ui::machine::{Cx, Effects, EntryId, Fx, InputOwner, Machine, Tick};
    let publication = owner.publication();
    let cx = Cx::<OwnerHost> {
        views: publication.read(),
        tick: Tick::default(),
        measure: &crate::ui::fixture::FixtureMeasure,
        press: Default::default(),
        focus: Default::default(),
        owner: InputOwner::Entry(EntryId(0)),
    };
    let mut present = crate::ui::present::Present::new();
    let mut effects = Vec::new();
    owner.step(&event, &cx, &mut Effects::new(&mut effects, MachineId::Session, &mut present));
    effects
        .into_iter()
        .map(|effect| match effect.fx {
            Fx::App(effect) => effect,
            _ => panic!("Session emitted a non-domain effect"),
        })
        .collect()
}

fn command(owner: &mut SessionMachine, command: Command) -> Vec<SessionFx> {
    step(owner, SessionEvent::Command(command))
}

/// A sign-in request's observation, as the adapter would deliver it.
fn observe(owner: &mut SessionMachine, progress: LoginProgress, terminal: bool) -> Vec<SessionFx> {
    let req = signin_request(owner);
    let pending = &owner.state.pending[&req];
    let arrival = pending.last_arrival.map_or(1, |a| a + 1);
    let envelope = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
        key: pending.key,
        admission: AdmissionId(req),
        arrival,
        terminal,
        lifecycle: None,
        outcome: SessionArrival::Data(Arc::new(super::super::super::observation::Observation::Login(progress))),
    };
    step(owner, SessionEvent::Result(envelope))
}

fn dns() -> crate::net::RequestFailure {
    crate::net::RequestFailure {
        cause: crate::net::RequestError::Transport,
        status: None,
        body_limit: None,
        curl_rc: Some(6),
    }
}

/// Fixed timestamps, so a context compares equal to itself across calls.
fn pin_create() -> IncidentContext {
    IncidentContext {
        occurred_at_ms: 1,
        ..IncidentContext::new(IncidentKind::PinCreate, Some(Err(dns()))).with_link_state(0, None, 1)
    }
}

fn stalled() -> IncidentContext {
    IncidentContext {
        occurred_at_ms: 2,
        ..IncidentContext::new(IncidentKind::LinkStalled, Some(Err(dns())))
            .with_link_state(2, Some(std::time::Duration::from_secs(6)), 1)
    }
}

/// A sign-in on its way: the QR flow started, a login request in flight.
fn signing_in() -> SessionMachine {
    let mut owner = SessionMachine::from_init(SessionInit::captured(PersistedSession {
        client_id: "synthetic-client".into(),
        ..Default::default()
    }));
    command(&mut owner, Command::StartLogin);
    owner
}

/// The flow's worker failed with `context`.
fn fail(owner: &mut SessionMachine, context: IncidentContext) -> Vec<SessionFx> {
    let epoch = owner.state.epoch;
    observe(
        owner,
        LoginProgress::Failed { epoch, message: "synthetic failure".into(), incident: context },
        true,
    )
}

fn offer(owner: &SessionMachine) -> Option<IncidentOffer> {
    owner.read().0.incident.clone()
}

fn resolve(owner: &mut SessionMachine, permission: Permission, revision: u32) -> Vec<SessionFx> {
    let id = offer(owner).expect("an incident is held").id;
    command(owner, Command::ResolveIncident { id, permission, revision })
}

fn incident_effects(effects: &[SessionFx]) -> Vec<(IncidentLane, IncidentReport)> {
    effects
        .iter()
        .filter_map(|fx| match fx {
            SessionFx::Incident { lane, report, .. } => Some((*lane, *report)),
            _ => None,
        })
        .collect()
}

// ---- (a) -----------------------------------------------------------------------------------------

#[test]
fn a_pin_create_failure_with_unset_consent_publishes_an_offered_incident() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    assert_eq!(owner.read().0.phase, Phase::Error);
    let held = offer(&owner).expect("the failure is held as an incident");
    assert_eq!(held.state, IncidentState::Pending, "nothing is offered before permission is known");
    assert_eq!(held.key.kind, IncidentKind::PinCreate);
    assert_eq!(held.key.link, LinkClass::Dns);
    assert_eq!(held.context, Some(pin_create()), "the closed evidence, as the producer built it");

    let effects = resolve(&mut owner, Permission::NotDetermined, 3);
    assert!(incident_effects(&effects).is_empty(), "an offer sends nothing by itself");
    assert_eq!(offer(&owner).unwrap().state, IncidentState::Offered { revision: 3 });
}

// ---- (c) -----------------------------------------------------------------------------------------

#[test]
fn a_second_failure_after_try_again_is_not_offered_again_after_not_now() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    let first = offer(&owner).unwrap();
    assert!(!command(&mut owner, Command::DeclineIncident { id: first.id }).iter().any(|fx| matches!(fx, SessionFx::Incident { .. })));
    assert_eq!(offer(&owner).unwrap().state, IncidentState::NotNow);

    // Try again: a new epoch, a new request — and the same failure over the same link.
    command(&mut owner, Command::Retry);
    assert_eq!(offer(&owner).map(|o| o.id), Some(first.id), "Try again does not forget the answer");
    fail(&mut owner, pin_create());
    let after = offer(&owner).unwrap();
    assert_eq!(after.id, first.id, "the same (flow, kind, link) raises nothing new this launch");
    assert_eq!(after.state, IncidentState::NotNow, "and so is not offered again");
}

// ---- (d) -----------------------------------------------------------------------------------------

#[test]
fn declined_retains_nothing_and_offers_nothing() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    let effects = resolve(&mut owner, Permission::Declined, 1);
    assert!(incident_effects(&effects).is_empty());
    let held = offer(&owner).unwrap();
    assert_eq!(held.state, IncidentState::Dropped);
    assert_eq!(held.context, None, "a No keeps no evidence");

    // Details → Send report still works, built at press time from the key alone.
    let effects = command(&mut owner, Command::ReportIncident { id: held.id });
    assert_eq!(incident_effects(&effects), vec![(IncidentLane::OneOff, IncidentReport::AtPress(held.key))]);
    assert_eq!(offer(&owner).unwrap().state, IncidentState::Sending);
}

// ---- (e) -----------------------------------------------------------------------------------------

#[test]
fn granted_sends_the_standing_report_and_offers_nothing() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    let effects = resolve(&mut owner, Permission::Granted, 1);
    assert_eq!(incident_effects(&effects), vec![(IncidentLane::Standing, IncidentReport::Retained(pin_create()))]);
    let held = offer(&owner).unwrap();
    assert_eq!(held.state, IncidentState::AutoSending, "never Offered");

    step(&mut owner, SessionEvent::IncidentReported { id: held.id, delivery: IncidentDelivery::Standing { receipt: Some("synthetic-standing".into()) } });
    assert_eq!(
        offer(&owner).unwrap().state,
        IncidentState::Queued { receipt: "synthetic-standing".into() },
        "a standing report shows its Report ID too"
    );
}

// ---- (f) -----------------------------------------------------------------------------------------

#[test]
fn a_stalled_wait_is_never_sent_standing() {
    let mut owner = signing_in();
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::LinkTrouble { epoch, trouble: Some(stalled()) }, false);
    assert!(owner.read().0.link_trouble, "the wait says the link is down");
    let held = offer(&owner).expect("a stalled wait raises an incident");
    assert_eq!(held.key.kind, IncidentKind::LinkStalled);

    let effects = resolve(&mut owner, Permission::Granted, 1);
    assert!(incident_effects(&effects).is_empty(), "Granted does not cover a stalled wait");
    assert_eq!(offer(&owner).unwrap().state, IncidentState::OnRequest { revision: 1 }, "it is offered behind Details instead");

    observe(&mut owner, LoginProgress::LinkTrouble { epoch, trouble: None }, false);
    assert!(!owner.read().0.link_trouble, "an answer clears the status line");
    assert_eq!(offer(&owner).unwrap().id, held.id, "but not the offer");
}

/// The QR code is still on screen during a stall, possibly mid-scan: no permission state may put
/// the alert over it. The offer lives behind *Details*, and a press there still sends a one-off.
#[test]
fn a_stalled_wait_is_offered_through_details_only_never_by_the_alert() {
    for permission in [Permission::NotDetermined, Permission::Granted] {
        let mut owner = signing_in();
        let epoch = owner.state.epoch;
        observe(&mut owner, LoginProgress::LinkTrouble { epoch, trouble: Some(stalled()) }, false);
        let effects = resolve(&mut owner, permission, 1);
        assert!(incident_effects(&effects).is_empty(), "{permission:?}: a stall is never sent without a press");
        let held = offer(&owner).unwrap();
        assert!(
            !matches!(held.state, IncidentState::Offered { .. }),
            "{permission:?}: a stall must not reach Offered (the alert), got {:?}",
            held.state
        );
        assert_eq!(held.state, IncidentState::OnRequest { revision: 1 });
        assert_eq!(held.context, Some(stalled()), "the evidence is kept for a press");

        // Details → Send report.
        let effects = command(&mut owner, Command::ReportIncident { id: held.id });
        assert_eq!(incident_effects(&effects), vec![(IncidentLane::OneOff, IncidentReport::Retained(stalled()))]);
        assert_eq!(offer(&owner).unwrap().state, IncidentState::Sending);
    }
}

#[test]
fn a_later_no_forgets_a_stall_offered_behind_details() {
    let mut owner = signing_in();
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::LinkTrouble { epoch, trouble: Some(stalled()) }, false);
    resolve(&mut owner, Permission::NotDetermined, 1);
    assert!(resolve(&mut owner, Permission::NotDetermined, 1).is_empty(), "same revision: nothing to redo");
    resolve(&mut owner, Permission::Declined, 2);
    let held = offer(&owner).unwrap();
    assert_eq!(held.state, IncidentState::Dropped);
    assert_eq!(held.context, None, "a No keeps no evidence");
}

// ---- the rest of the machine ---------------------------------------------------------------------

#[test]
fn the_offer_is_re_resolved_when_the_decision_changes() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    let effects = resolve(&mut owner, Permission::Granted, 2);
    assert_eq!(incident_effects(&effects).len(), 1, "a Yes at the onboarding scope sends it");
    assert_eq!(offer(&owner).unwrap().state, IncidentState::AutoSending);
}

#[test]
fn send_report_carries_a_receipt_or_says_it_failed() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    let id = offer(&owner).unwrap().id;
    let effects = command(&mut owner, Command::ReportIncident { id });
    assert_eq!(incident_effects(&effects), vec![(IncidentLane::OneOff, IncidentReport::Retained(pin_create()))]);
    assert!(command(&mut owner, Command::ReportIncident { id }).is_empty(), "one press, one report");
    step(&mut owner, SessionEvent::IncidentReported { id, delivery: IncidentDelivery::OneOff { receipt: None } });
    assert_eq!(offer(&owner).unwrap().state, IncidentState::Failed);
    command(&mut owner, Command::ReportIncident { id });
    step(&mut owner, SessionEvent::IncidentReported {
        id,
        delivery: IncidentDelivery::OneOff { receipt: Some("synthetic-receipt".into()) },
    });
    assert_eq!(offer(&owner).unwrap().state, IncidentState::Queued { receipt: "synthetic-receipt".into() });
}

#[test]
fn a_newer_incident_supersedes_the_held_one_and_fences_its_reply() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    let old = offer(&owner).unwrap();
    command(&mut owner, Command::ReportIncident { id: old.id });
    command(&mut owner, Command::Retry);
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::LinkTrouble { epoch, trouble: Some(stalled()) }, false);
    let new = offer(&owner).unwrap();
    assert_ne!(new.id, old.id);
    assert_eq!(new.state, IncidentState::Pending);
    assert!(
        step(&mut owner, SessionEvent::IncidentReported { id: old.id, delivery: IncidentDelivery::OneOff { receipt: None } })
            .is_empty()
    );
    assert_eq!(offer(&owner).unwrap(), new, "a reply for the superseded offer changes nothing");
}

#[test]
fn sign_out_forgets_the_incident_and_what_this_launch_has_seen() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    command(&mut owner, Command::SignOut);
    assert_eq!(offer(&owner), None);
    assert!(owner.state.incidents_seen.is_empty());
}

#[test]
fn a_failure_keeps_an_offer_the_wait_raised() {
    let mut owner = signing_in();
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::LinkTrouble { epoch, trouble: Some(stalled()) }, false);
    resolve(&mut owner, Permission::NotDetermined, 1);
    assert!(matches!(offer(&owner).unwrap().state, IncidentState::OnRequest { .. }));
    // The code then runs out: a different kind, so it supersedes — but the resolved stall is not
    // lost to `fail_login` on the way; it is replaced by a newer incident.
    let expired = IncidentContext::new(IncidentKind::PinExpired, None).with_link_state(0, None, 4);
    fail(&mut owner, expired);
    let now = offer(&owner).unwrap();
    assert_eq!(now.key.kind, IncidentKind::PinExpired);
    assert!(!owner.read().0.link_trouble, "the read-out replaces the wait's status line");
}

// ---- review round 2026-09-19 -------------------------------------------------------------------

/// The sign-in request in flight — a QR sign-in, or the discovery-only retry of an authorized one.
fn signin_request(owner: &SessionMachine) -> u32 {
    *owner
        .state
        .pending
        .iter()
        .find(|(_, p)| matches!(p.key.op, SessionOp::Login | SessionOp::Rediscover))
        .map(|(req, _)| req)
        .expect("a sign-in request is in flight")
}

/// A terminal non-data arrival for the sign-in request: the worker was refused or went away.
fn arrive(owner: &mut SessionMachine, outcome: SessionArrival) -> Vec<SessionFx> {
    let req = signin_request(owner);
    let pending = &owner.state.pending[&req];
    let envelope = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
        key: pending.key,
        admission: AdmissionId(req),
        arrival: pending.last_arrival.map_or(1, |a| a + 1),
        terminal: true,
        lifecycle: None,
        outcome,
    };
    step(owner, SessionEvent::Result(envelope))
}

/// A PinCreate the person answered Not now, then Try again.
fn after_a_declined_pin_create() -> (SessionMachine, IncidentOffer) {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    let first = offer(&owner).unwrap();
    command(&mut owner, Command::DeclineIncident { id: first.id });
    command(&mut owner, Command::Retry);
    (owner, first)
}

/// **A failure that ends the sign-in without a worker's incident offers ITS OWN report**, never
/// the previous failure's. A dropped or refused worker used to reach the read-out through
/// `fail_login` alone, which left the PinCreate offer standing under a caption about something
/// else — its Details, its support code and its Send report all about the earlier failure.
#[test]
fn a_dropped_worker_offers_its_own_failure_not_the_previous_one() {
    let (mut owner, first) = after_a_declined_pin_create();
    arrive(&mut owner, SessionArrival::Dropped);
    assert_eq!(owner.read().0.phase, Phase::Error);
    let now = offer(&owner).expect("the read-out has a report to offer");
    assert_ne!(now.id, first.id, "the read-out offered the PinCreate failure's report");
    assert_ne!(now.key.kind, IncidentKind::PinCreate);
    assert_eq!(now.key.kind, IncidentKind::Internal(InternalClass::WorkerDropped));
    assert_eq!(now.state, IncidentState::Pending, "a new failure: asked about in its own right");
    assert_eq!(now.context, Some(IncidentContext::internal(InternalClass::WorkerDropped)));
}

#[test]
fn a_refused_worker_and_a_refused_admission_offer_their_own_failure() {
    let (mut owner, _) = after_a_declined_pin_create();
    arrive(&mut owner, SessionArrival::Refused);
    assert_eq!(offer(&owner).unwrap().key.kind, IncidentKind::Internal(InternalClass::WorkerRefused));

    let (mut owner, _) = after_a_declined_pin_create();
    let req = signin_request(&owner);
    let key = owner.state.pending[&req].key;
    step(&mut owner, SessionEvent::Admission(AdmissionReply {
        addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
        key,
        correlation: AdmissionId(req),
        accepted: false,
    }));
    assert_eq!(owner.read().0.phase, Phase::Error);
    assert_eq!(offer(&owner).unwrap().key.kind, IncidentKind::Internal(InternalClass::AdmissionRefused));
}

#[test]
fn a_sign_in_whose_commit_is_refused_offers_its_own_failure() {
    let (mut owner, _) = after_a_declined_pin_create();
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::Authorized { epoch, token: "synthetic-token".into() }, false);
    let effects = observe(&mut owner, LoginProgress::SignedIn {
        epoch,
        server: Default::default(),
        sources: Vec::new(),
        users: vec![crate::auth::UserTile { title: "Only user".into(), ..Default::default() }],
    }, true);
    let (req, arrival) = effects.iter().find_map(|fx| match fx {
        SessionFx::Commit { req, arrival, .. } => Some((*req, *arrival)),
        _ => None,
    }).expect("the sign-in is committed");
    step(&mut owner, SessionEvent::Commit(CommitReply {
        req,
        epoch,
        arrival,
        admission: CommitAdmission::Rejected {
            revision: None,
            rejection: crate::plex::session::async_persistence::RejectionKind::Capacity,
        },
    }));
    assert_eq!(owner.read().0.phase, Phase::Error);
    assert_eq!(offer(&owner).unwrap().key.kind, IncidentKind::Internal(InternalClass::CommitRefused));
}

/// A failure this launch already asked about, coming back after a DIFFERENT one, is raised again
/// — its read-out needs its own Details — but never asked about twice: it is resolved behind
/// Details only, for every decision that would have put the alert up or sent it unasked.
#[test]
fn a_seen_failure_after_another_is_offered_again_behind_details_only() {
    for permission in [Permission::NotDetermined, Permission::Granted] {
        let (mut owner, first) = after_a_declined_pin_create();
        arrive(&mut owner, SessionArrival::Dropped);
        resolve(&mut owner, Permission::NotDetermined, 1);
        let dropped = offer(&owner).unwrap();
        command(&mut owner, Command::DeclineIncident { id: dropped.id });
        command(&mut owner, Command::Retry);
        fail(&mut owner, pin_create());
        let again = offer(&owner).unwrap();
        assert_eq!(again.key, first.key, "the read-out is about the PinCreate again");
        assert_ne!(again.id, dropped.id);
        let effects = resolve(&mut owner, permission, 1);
        assert!(incident_effects(&effects).is_empty(), "{permission:?}: sent unasked");
        assert_eq!(offer(&owner).unwrap().state, IncidentState::OnRequest { revision: 1 }, "{permission:?}");
        // A later decision may drop it, never escalate it to the alert.
        resolve(&mut owner, Permission::NotDetermined, 2);
        assert_eq!(offer(&owner).unwrap().state, IncidentState::OnRequest { revision: 2 });
        resolve(&mut owner, Permission::Declined, 3);
        assert_eq!(offer(&owner).unwrap().state, IncidentState::Dropped);
    }
}

/// An ending the onboarding report does not cover retires the held offer instead of explaining
/// itself with it.
#[test]
fn an_uncovered_ending_retires_the_held_offer() {
    let (mut owner, _) = after_a_declined_pin_create();
    owner.fail_login("synthetic uncovered ending", None, &mut |_| {});
    assert_eq!(owner.state.incident, None);
}

/// **Try again after an authorized discovery failed asks nothing new.** The retry is a
/// discovery-only pass (`SessionOp::Rediscover`), and when it fails the same way it reports the
/// same incident (`auth::discovery_failure`), so the Not now stands.
#[test]
fn a_discovery_retry_that_fails_the_same_way_is_not_asked_about_again() {
    let mut owner = signing_in();
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::Authorized { epoch, token: "synthetic-token".into() }, false);
    let silent = IncidentContext {
        occurred_at_ms: 3,
        ..IncidentContext::new(IncidentKind::Discovery(crate::telemetry::incident::DiscoveryClass::Silent), Some(Err(dns())))
    };
    fail(&mut owner, silent);
    resolve(&mut owner, Permission::NotDetermined, 1);
    let first = offer(&owner).unwrap();
    command(&mut owner, Command::DeclineIncident { id: first.id });
    command(&mut owner, Command::Retry);
    let req = signin_request(&owner);
    assert!(owner.state.pending[&req].key.op == SessionOp::Rediscover, "the retry re-runs discovery only");
    fail(&mut owner, silent);
    let after = offer(&owner).unwrap();
    assert_eq!((after.id, after.state), (first.id, IncidentState::NotNow), "the Not now stands");
}

/// **Try again after plex.tv refused the account token is a new QR sign-in.** An
/// [`IncidentKind::Authorization`] failure means `/resources` answered 401/403 to the token the
/// pin had just yielded; a discovery-only retry would hand plex.tv that same refused token again
/// and could never recover. The retry mints a new code — and, being the same failure, the Not now
/// still stands when it fails that way again.
#[test]
fn try_again_after_the_token_was_refused_starts_a_new_sign_in() {
    let mut owner = signing_in();
    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::Authorized { epoch, token: "synthetic-token".into() }, false);
    let refused = IncidentContext {
        occurred_at_ms: 4,
        ..IncidentContext::new(IncidentKind::Authorization, Some(Ok(401)))
    };
    fail(&mut owner, refused);
    resolve(&mut owner, Permission::NotDetermined, 1);
    let first = offer(&owner).unwrap();
    command(&mut owner, Command::DeclineIncident { id: first.id });
    let effects = command(&mut owner, Command::Retry);
    let req = signin_request(&owner);
    assert!(owner.state.pending[&req].key.op == SessionOp::Login, "the retry reused the refused token");
    assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Work { input: SessionWork::Login { .. }, .. })),
        "the retry asks for a new code");
    assert_eq!(owner.read().0.phase, Phase::Creating);

    let epoch = owner.state.epoch;
    observe(&mut owner, LoginProgress::Authorized { epoch, token: "synthetic-token-2".into() }, false);
    fail(&mut owner, refused);
    let after = offer(&owner).unwrap();
    assert_eq!((after.id, after.state), (first.id, IncidentState::NotNow), "the Not now stands");
}

/// **A one-off that did not get through can be sent again.** The lane hands back a receipt as
/// soon as it takes the report, but when the spool could not take it the bounded direct fallback
/// is still on the network; if that then fails, the offer must go back to a state Send report is
/// accepted in — the screen saying "couldn't be sent" over a Details with no Send report was the
/// defect. Fenced by the receipt as well as the id: only the report that failed is re-opened.
#[test]
fn a_one_off_whose_direct_fallback_failed_can_be_sent_again() {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    resolve(&mut owner, Permission::NotDetermined, 1);
    let id = offer(&owner).unwrap().id;
    command(&mut owner, Command::ReportIncident { id });
    step(&mut owner, SessionEvent::IncidentReported {
        id,
        delivery: IncidentDelivery::OneOff { receipt: Some("receipt-1".into()) },
    });
    let sent = IncidentState::Queued { receipt: "receipt-1".into() };
    assert_eq!(offer(&owner).unwrap().state, sent);

    // Another report's failure, or another offer's, re-opens nothing.
    let other = IncidentDelivery::Undelivered { receipt: "receipt-0".into() };
    assert!(step(&mut owner, SessionEvent::IncidentReported { id, delivery: other }).is_empty());
    let wrong_id = IncidentDelivery::Undelivered { receipt: "receipt-1".into() };
    assert!(step(&mut owner, SessionEvent::IncidentReported { id: id + 1, delivery: wrong_id }).is_empty());
    assert_eq!(offer(&owner).unwrap().state, sent);

    step(&mut owner, SessionEvent::IncidentReported {
        id,
        delivery: IncidentDelivery::Undelivered { receipt: "receipt-1".into() },
    });
    let held = offer(&owner).unwrap();
    assert_eq!(held.state, IncidentState::Failed, "the fallback's failure reaches the offer");
    assert!(held.sendable(), "and Send report is offered again");
    let effects = command(&mut owner, Command::ReportIncident { id });
    assert_eq!(incident_effects(&effects), vec![(IncidentLane::OneOff, IncidentReport::Retained(pin_create()))]);
}

// ---- delivery: queued is not delivered -----------------------------------------------------------

fn reported(owner: &mut SessionMachine, id: u32, delivery: IncidentDelivery) {
    step(owner, SessionEvent::IncidentReported { id, delivery });
}

/// A one-off or a standing report the lane took, as the owner then holds it.
fn queued(lane: IncidentLane) -> (SessionMachine, u32) {
    let mut owner = signing_in();
    fail(&mut owner, pin_create());
    let receipt = Some("receipt-1".to_string());
    let id = match lane {
        IncidentLane::OneOff => {
            resolve(&mut owner, Permission::NotDetermined, 1);
            let id = offer(&owner).unwrap().id;
            command(&mut owner, Command::ReportIncident { id });
            reported(&mut owner, id, IncidentDelivery::OneOff { receipt });
            id
        }
        IncidentLane::Standing => {
            resolve(&mut owner, Permission::Granted, 1);
            let id = offer(&owner).unwrap().id;
            reported(&mut owner, id, IncidentDelivery::Standing { receipt });
            id
        }
    };
    (owner, id)
}

/// **(a)/(d) A report the lane TOOK is queued, not delivered** — on both lanes.
#[test]
fn a_report_the_lane_took_is_queued_not_delivered() {
    for lane in [IncidentLane::OneOff, IncidentLane::Standing] {
        let (owner, _) = queued(lane);
        assert_eq!(offer(&owner).unwrap().state, IncidentState::Queued { receipt: "receipt-1".into() }, "{lane:?}");
    }
}

/// **(b)/(d) Only a server's acceptance of THAT report makes it delivered**; a held one is saved
/// first and still becomes delivered when a later flush gets through. Fenced by id and receipt.
#[test]
fn a_2xx_for_the_shown_receipt_moves_it_to_delivered() {
    for lane in [IncidentLane::OneOff, IncidentLane::Standing] {
        let (mut owner, id) = queued(lane);
        let delivered = |r: &str| IncidentDelivery::Delivered { receipt: r.into() };
        step(&mut owner, SessionEvent::IncidentReported { id, delivery: delivered("receipt-0") });
        step(&mut owner, SessionEvent::IncidentReported { id: id + 1, delivery: delivered("receipt-1") });
        assert_eq!(offer(&owner).unwrap().state, IncidentState::Queued { receipt: "receipt-1".into() }, "{lane:?}: fenced");

        step(&mut owner, SessionEvent::IncidentReported { id, delivery: IncidentDelivery::Held { receipt: "receipt-1".into() } });
        assert_eq!(offer(&owner).unwrap().state, IncidentState::Saved { receipt: "receipt-1".into() }, "{lane:?}");
        assert!(!offer(&owner).unwrap().sendable(), "a saved report is still going to be sent");

        step(&mut owner, SessionEvent::IncidentReported { id, delivery: delivered("receipt-1") });
        assert_eq!(offer(&owner).unwrap().state, IncidentState::Delivered { receipt: "receipt-1".into() }, "{lane:?}");
        assert!(!offer(&owner).unwrap().sendable());
    }
}

/// **(c)/(d) A report dropped after all — queued or saved — is Failed, and re-sendable.**
#[test]
fn a_discarded_report_fails_and_can_be_sent_again() {
    for lane in [IncidentLane::OneOff, IncidentLane::Standing] {
        for saved_first in [false, true] {
            let (mut owner, id) = queued(lane);
            if saved_first {
                step(&mut owner, SessionEvent::IncidentReported { id, delivery: IncidentDelivery::Held { receipt: "receipt-1".into() } });
            }
            step(&mut owner, SessionEvent::IncidentReported { id, delivery: IncidentDelivery::Undelivered { receipt: "receipt-1".into() } });
            let held = offer(&owner).unwrap();
            assert_eq!(held.state, IncidentState::Failed, "{lane:?} saved_first={saved_first}");
            assert!(held.sendable());
            let effects = command(&mut owner, Command::ReportIncident { id });
            assert_eq!(incident_effects(&effects), vec![(IncidentLane::OneOff, IncidentReport::Retained(pin_create()))]);
        }
    }
}
