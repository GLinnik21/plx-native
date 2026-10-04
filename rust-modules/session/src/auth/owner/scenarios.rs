//! Session-owner scenarios the application's tests drive.
//!
//! These were helpers of `owner`'s own `mod tests`; four of them are the session halves of the
//! roster-art scenarios that `app/session_roster_art_tests.rs` runs and then grades against the
//! poster that sits above this layer. A dependent crate's tests cannot reach a `cfg(test)` module,
//! so they live here, behind `test-support`, with the helper chain they build on. Nothing in this
//! file exists in a shipping build.

use super::*;

pub struct OwnerHost;
impl plx_machine::machine::Host for OwnerHost {
    type Arg = plx_machine::machine::BareArg;
    type Fx = SessionFx;
    type Msg = SessionEvent;
    type Elem = u32;
    type Views<'a> = SessionRead<'a>;
    type Init = SessionInit;
    type Memory = ();
}
impl SessionHost for OwnerHost {
    fn session_effect(effect: SessionFx) -> SessionFx { effect }
}

pub fn step(owner: &mut SessionMachine, event: SessionEvent) -> Vec<SessionFx> {
    use plx_machine::machine::{Cx, Effects, Fx, InputOwner, EntryId, Machine, Tick};
    let publication = owner.publication();
    let cx = Cx::<OwnerHost> { views: publication.read(), tick: Tick::default(),
        measure: &plx_machine::machine::BareMeasure, press: Default::default(),
        focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
    let mut present = plx_machine::present::Present::new();
    let mut effects = Vec::new();
    owner.step(&event, &cx, &mut Effects::new(&mut effects, MachineId::Session, &mut present));
    effects.into_iter().map(|effect| match effect.fx {
        Fx::App(effect) => effect,
        _ => panic!("Session emitted a non-domain effect"),
    }).collect()
}

pub fn roster_refresh_fixture(profile_uuid: &str,
    home_users: Vec<plx_plex::plex::session::HomeUserRef>) -> SessionMachine {
    let source = plx_plex::plex::session::SourceRef {
        machine_id: "profile-machine".into(), name: "Profile server".into(), owned: true,
        token: "profile-server-token".into(), address: "10.0.0.8".into(), port: 32400,
        origin_url: "https://10-0-0-8.example.plex.direct:32400".into(),
        tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default()
    };
    let user = UserRef { uuid: profile_uuid.into(), title: "Seated profile".into(),
        token: "profile-server-token".into(), ..Default::default() };
    let mut persisted = PersistedSession {
        client_id: "synthetic-client".into(), account_token: "account-token".into(),
        server: super::super::server_ref(&source), user: user.clone(),
        home_users, sources: vec![source.clone()], ..Default::default()
    };
    if !profile_uuid.is_empty() {
        persisted.profiles.push(plx_plex::plex::session::ProfileCreds {
            uuid: profile_uuid.into(), user, server: persisted.server.clone(),
            sources: vec![source], pin: None, extensions: Default::default(),
        });
    }
    SessionMachine::from_init(SessionInit::captured(persisted))
}

/// The who's-watching picker path: `Ready` seats the picked profile and commits its roster as
/// a `Switch` (the one revoke this change of identity owes), Home is drawn, and only then does
/// the same request's secondary `ProfileRoster` land — for the profile that is ALREADY seated
/// and installed. Returns the owner after the `Ready` commit (its registry executed), the
/// request, its epoch and the seated profile's primary source.
pub fn picker_switch_seated() -> (SessionMachine, u32, u64, plx_plex::plex::session::SourceRef) {
    let primary = plx_plex::plex::session::SourceRef {
        machine_id: "a".into(), name: "Primary A".into(), owned: true,
        token: "kid-a-token".into(), address: "10.0.0.8".into(), port: 32400,
        origin_url: "https://10-0-0-8.example.plex.direct:32400".into(),
        tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default()
    };
    let user = UserRef { uuid: "u-kid".into(), title: "Kid".into(),
        token: primary.token.clone(), ..Default::default() };
    let persisted = PersistedSession {
        client_id: "synthetic-client".into(), account_token: "account-token".into(),
        server: super::super::server_ref(&primary), user: UserRef { uuid: "u-admin".into(),
            token: "admin-token".into(), ..Default::default() },
        home_users: vec![plx_plex::plex::session::HomeUserRef { uuid: "u-kid".into(),
            title: "Kid".into(), admin: false, ..Default::default() }],
        sources: vec![primary.clone()], ..Default::default()
    };
    let mut init = SessionInit::captured(persisted);
    init.phase = Phase::Switching;
    init.users = vec![UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() }];
    let mut owner = SessionMachine::from_init(init);
    let req = owner.allocate(SessionOp::ProfileSwitch, None).unwrap();
    owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
    let epoch = owner.state.epoch;
    let expected = super::super::SessionIdentity::of(&owner.state.persisted);
    let ready = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
        key: SessionWorkKey { epoch, op: SessionOp::ProfileSwitch }, admission: AdmissionId(req),
        arrival: 1, terminal: false, lifecycle: None,
        outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ProfileSwitch(
            super::super::ProfileSwitchProgress { epoch, expected,
                outcome: super::super::ProfileSwitchOutcomeProgress::Ready {
                    delta: super::super::ProfileDelta {
                        server: super::super::server_ref(&primary),
                        sources: vec![primary.clone()], user: user.clone(), cache: None,
                    },
                    probes: Vec::new(),
                },
            }))),
    };
    let effects = step(&mut owner, SessionEvent::Result(ready));
    let (reply, plan) = effects.iter().find_map(|effect| match effect {
        SessionFx::Commit { req, epoch, arrival, plan } => Some((CommitReply {
            req: *req, epoch: *epoch, arrival: *arrival, admission: CommitAdmission::RegistryOnly,
        }, plan.clone())),
        _ => None,
    }).expect("the profile switch must commit");
    assert!(matches!(&plan.registry[0], RegistryPlan::Install { commit: RosterCommit::Switch, .. }),
        "seating a different profile is a switch");
    for p in &plan.registry {
        assert!(super::super::execute_session_registry(p, "synthetic-client"));
    }
    step(&mut owner, SessionEvent::Commit(reply));
    assert_eq!(owner.state.persisted.user.uuid, "u-kid");
    (owner, req, epoch, primary)
}

pub fn late_profile_roster(owner: &mut SessionMachine, req: u32, epoch: u64,
    primary: &plx_plex::plex::session::SourceRef, expected: super::super::SessionIdentity) -> CommitPlan {
    let resources = vec![plx_plex::plex::account::Resource { name: primary.name.clone(),
        client_identifier: primary.machine_id.clone(), provides: "server".into(),
        owned: true, access_token: primary.token.clone(), ..Default::default() }];
    let roster = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
        key: SessionWorkKey { epoch, op: SessionOp::ProfileSwitch }, admission: AdmissionId(req),
        arrival: 2, terminal: true, lifecycle: None,
        outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ProfileRoster(
            super::super::ProfileRosterProgress { epoch, expected,
                resources, reached: vec![primary.clone()], probes: Vec::new() }))),
    };
    let effects = step(owner, SessionEvent::Result(roster));
    effects.iter().find_map(|effect| match effect {
        SessionFx::Commit { plan, .. } => Some(plan.clone()), _ => None,
    }).expect("the late profile roster must commit")
}

/// The session half of **Blink C** (`a_late_profile_roster_for_the_seated_profile_keeps_resident_art`,
/// which lives in `app/session_roster_art_tests.rs` beside the poster it grades): the owner
/// seats a picked profile, then takes the switch's own late `ProfileRoster` for that same
/// profile. Returns the seated server's slot and the plan that late roster commits. The caller
/// holds [`plx_base::testlock::serial`] and has reset the server table and the grants.
pub fn late_roster_of_the_seated_profile() -> (plx_plex::plex::ServerId, CommitPlan) {
    let (mut owner, req, epoch, primary) = picker_switch_seated();
    let sid = plx_plex::plex::id_of_machine("a").expect("the switch installed the seated server");
    let seated = super::super::SessionIdentity::of(&owner.state.persisted);
    let plan = late_profile_roster(&mut owner, req, epoch, &primary, seated);
    (sid, plan)
}

/// The session half of **Blink B** (`an_admin_boot_refresh_of_the_seated_profile_keeps_resident_art`,
/// which lives in `app/session_roster_art_tests.rs` beside the poster it grades): the stored
/// session's registry is installed, then discovery reaches the same server and user under
/// plex.tv's current `grant`. Returns the stored server's slot and the plan the refresh
/// commits. The caller holds [`plx_base::testlock::serial`] and has reset the server table and the
/// grants.
pub fn admin_boot_refresh_of_the_seated_profile(grant: &str) -> (plx_plex::plex::ServerId, CommitPlan) {
    let users = vec![plx_plex::plex::session::HomeUserRef { id: 1, uuid: "u-owner".into(),
        title: "Owner".into(), admin: true, ..Default::default() }];
    let mut owner = roster_refresh_fixture("u-owner", users);
    // The stored-session boot: the persisted roster, registered before any discovery.
    let stored = owner.state.persisted.sources.clone();
    assert!(super::super::execute_session_registry(&RegistryPlan::Install {
        sources: stored.clone(), primary: None, commit: RosterCommit::Merge,
    }, "synthetic-client"));
    let sid = plx_plex::plex::id_of_machine("profile-machine").expect("the stored server registered");

    // Discovery reaches the same server, same address, same user — under plex.tv's grant.
    let mut reached = stored[0].clone();
    reached.token = grant.into();
    let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
    owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
    let epoch = owner.state.epoch;
    let expected = super::super::SessionIdentity::of(&owner.state.persisted);
    let resources = vec![plx_plex::plex::account::Resource { name: reached.name.clone(),
        client_identifier: reached.machine_id.clone(), provides: "server".into(), owned: true,
        access_token: reached.token.clone(), ..Default::default() }];
    let envelope = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
        key: SessionWorkKey { epoch, op: SessionOp::ServerRoster }, admission: AdmissionId(req),
        arrival: 1, terminal: true, lifecycle: None,
        outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
            super::super::ServerRosterProgress { epoch, expected,
                outcome: super::super::ServerRosterOutcome::Reconcile {
                    resources, found: vec![reached.clone()],
                    admitted_machine_id: reached.machine_id.clone(), household: vec![1],
                    settled: Vec::new(),
                },
            }))),
    };
    let effects = step(&mut owner, SessionEvent::Result(envelope));
    let plan = effects.iter().find_map(|effect| match effect {
        SessionFx::Commit { plan, .. } => Some(plan.clone()), _ => None,
    }).expect("a rotated grant is persisted");
    (sid, plan)
}

/// The seated profile is the Home ADMIN, but the account signed in on this television is a
/// non-managed MEMBER of that Home (their own plex.tv account, switched to the admin's tile).
/// The roster refresh lists `/resources` with the member's `account_token`, so every token it
/// carries is the MEMBER's grant — the admin's own server comes back `owned: false`. The stored
/// registry (the admin's grants) is live; returns the owner, the admin server's slot and the
/// member's view of it.
pub fn member_account_on_admin_seat() -> (SessionMachine, plx_plex::plex::ServerId, plx_plex::plex::session::SourceRef) {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::grant::reset_for_test();
    let users = vec![
        plx_plex::plex::session::HomeUserRef { id: 1, uuid: "u-admin".into(), title: "Admin".into(),
            admin: true, ..Default::default() },
        plx_plex::plex::session::HomeUserRef { id: 2, uuid: "u-member".into(), title: "Member".into(),
            ..Default::default() },
    ];
    let owner = roster_refresh_fixture("u-admin", users);
    assert!(owner.state.persisted.active_profile_is_admin());
    let stored = owner.state.persisted.sources.clone();
    assert!(stored[0].owned, "the admin's roster calls the admin's server owned");
    assert!(super::super::execute_session_registry(&RegistryPlan::Install {
        sources: stored.clone(), primary: None, commit: RosterCommit::Merge,
    }, "synthetic-client"));
    let sid = plx_plex::plex::id_of_machine("profile-machine").expect("the stored server registered");
    let mut members_view = stored[0].clone();
    members_view.token = "member-grant-for-the-admins-server".into();
    members_view.owned = false;
    (owner, sid, members_view)
}

/// The session half of the review finding on the Refresh commit
/// (`a_refresh_under_another_accounts_token_does_not_keep_the_seated_profiles_art`, which
/// lives in `app/session_roster_art_tests.rs` beside the poster it grades): `admin` is not
/// "the account holder". The terminal reconcile of [`member_account_on_admin_seat`] installs
/// the member's grants over the admin's live tokens. Returns the admin server's slot and the
/// plan that reconcile commits. The caller holds [`plx_base::testlock::serial`].
pub fn refresh_under_another_accounts_token() -> (plx_plex::plex::ServerId, CommitPlan) {
    let (mut owner, sid, members_view) = member_account_on_admin_seat();
    let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
    owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
    let epoch = owner.state.epoch;
    let expected = super::super::SessionIdentity::of(&owner.state.persisted);
    let resources = vec![plx_plex::plex::account::Resource { name: members_view.name.clone(),
        client_identifier: members_view.machine_id.clone(), provides: "server".into(), owned: false,
        access_token: members_view.token.clone(), ..Default::default() }];
    let envelope = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
        key: SessionWorkKey { epoch, op: SessionOp::ServerRoster }, admission: AdmissionId(req),
        arrival: 1, terminal: true, lifecycle: None,
        outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
            super::super::ServerRosterProgress { epoch, expected,
                outcome: super::super::ServerRosterOutcome::Reconcile {
                    resources, found: vec![members_view.clone()],
                    admitted_machine_id: members_view.machine_id.clone(), household: vec![1, 2],
                    settled: Vec::new(),
                },
            }))),
    };
    let effects = step(&mut owner, SessionEvent::Result(envelope));
    let plan = effects.iter().find_map(|effect| match effect {
        SessionFx::Commit { plan, .. } => Some(plan.clone()), _ => None,
    }).expect("the changed roster commits");
    (sid, plan)
}

/// The session half of the same gap one observation earlier
/// (`an_activation_under_another_accounts_token_does_not_keep_the_seated_profiles_art`, which
/// lives in `app/session_roster_art_tests.rs` beside the poster it grades): the roster worker's
/// `Activate` progress for the admin's server, carrying the member's grant, re-tokens the
/// admin's live slot in place. Returns that slot and the plan the activation commits. The
/// caller holds [`plx_base::testlock::serial`].
pub fn activation_under_another_accounts_token() -> (plx_plex::plex::ServerId, CommitPlan) {
    let (mut owner, sid, members_view) = member_account_on_admin_seat();
    let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
    owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
    let epoch = owner.state.epoch;
    let expected = super::super::SessionIdentity::of(&owner.state.persisted);
    let activate = SessionEnvelope {
        addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
        key: SessionWorkKey { epoch, op: SessionOp::ServerRoster },
        admission: AdmissionId(req), arrival: 1, terminal: false, lifecycle: None,
        outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Registry(
            super::super::RegistryProgress::Activate {
                epoch, expected: Some(expected),
                candidate: super::super::CandidateActivation {
                    machine_id: members_view.machine_id.clone(), token: members_view.token.clone(),
                    name: members_view.name.clone(), credit: String::new(), owned: false,
                    home: true, owner_id: 1, origin: members_view.origin().unwrap(),
                    address: members_view.address.clone(), location: plx_plex::plex::probe::Location::Local,
                    ipv6: false,
                },
            }))),
    };
    let effects = step(&mut owner, SessionEvent::Result(activate));
    let plan = effects.iter().find_map(|effect| match effect {
        SessionFx::Commit { plan, .. } => Some(plan.clone()), _ => None,
    }).expect("the activation reaches the commit boundary");
    (sid, plan)
}
