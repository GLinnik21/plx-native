//! Synthetic baselines for the EXISTING owner, not a test-only Attempt implementation.
use super::*;
use super::scenarios::step;
use super::super::{LoginProgress, observation::Observation};
use plx_machine::machine::RequestId;
use plx_plex::plex::session::async_persistence::{CompletionOutcome, Operation, PersistOutcome,
    PersistenceCompletion};

// Diagnostics deliberately exclude effect payloads, which may contain credentials.
fn safe_effect_diagnostics(effects: &[SessionFx]) -> Vec<String> {
    effects.iter().map(|effect| {
        let kind = std::mem::discriminant(effect);
        match effect {
            SessionFx::Cancel { requests, epoch } =>
                format!("{kind:?} requests={requests:?} epoch={epoch}"),
            SessionFx::Work { req, key, .. } =>
                format!("{kind:?} req={req} epoch={}", key.epoch),
            SessionFx::Commit { req, epoch, arrival, .. } =>
                format!("{kind:?} req={req} epoch={epoch} arrival={arrival}"),
            SessionFx::Erase { req, epoch, .. } =>
                format!("{kind:?} req={req} epoch={epoch}"),
            SessionFx::Retire { req } => format!("{kind:?} req={req}"),
            SessionFx::BackReply { to, resumed } =>
                format!("{kind:?} instance={} correlation={} resumed={resumed}",
                    to.instance, to.correlation),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Ready { .. }
            | SessionFx::Capture { .. } | SessionFx::PublishProfile(_) | SessionFx::Coordinator(_)
            | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => format!("{kind:?}"),
        }
    }).collect()
}

// Capture only the explicit synthetic probe fixtures, never global session state.
fn emit_legacy_v5_artifact(case: &str, init: &SessionInit) {
    let Some(dir) = std::env::var_os("PLX_SESSION_V5_GOLDEN_OUT") else { return };
    let (bytes_name, manifest_name) = match case {
        "boot-account" => ("boot-account.json", "boot-account.manifest.json"),
        "boot-devpms" => ("boot-devpms.json", "boot-devpms.manifest.json"),
        "live" => ("live.json", "live.manifest.json"),
        "nested" => ("nested.json", "nested.manifest.json"),
        _ => panic!("unknown synthetic legacy V5 case"),
    };
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir).expect("create synthetic golden output directory");
    let bytes = serde_json::to_vec(init).expect("serialize synthetic legacy V5 case");
    let manifest = serde_json::json!({
        "schema": "legacy-v5", "case": case, "hash": init.hash(),
        "bytes_path": bytes_name, "synthetic": true
    });
    std::fs::write(dir.join(bytes_name), bytes).expect("write synthetic golden bytes");
    std::fs::write(dir.join(manifest_name), serde_json::to_vec(&manifest).unwrap())
        .expect("write synthetic golden manifest");
}

fn captured() -> SessionInit {
    SessionInit::captured(PersistedSession { client_id: "synthetic-client".into(), ..Default::default() })
}
fn local() -> SessionInit {
    let mut s = captured().persisted;
    s.account_token = "synthetic-account".into();
    s.server.address = "127.0.0.1".into();
    s.server.port = 32400;
    s.server.token = "synthetic-pms".into();
    s.user.token = "synthetic-pms".into();
    SessionInit::captured(s)
}
fn command(owner: &mut SessionMachine, command: Command) -> Vec<SessionFx> {
    step(owner, SessionEvent::Command(command))
}
fn envelope(owner: &SessionMachine, req: u32, arrival: u64, terminal: bool,
    data: Observation) -> SessionEnvelope {
    SessionEnvelope { addr: Addr { to: MachineId::Session, req: RequestId(req) },
        key: owner.common.pending[&req].key, admission: AdmissionId(req), arrival, terminal,
        lifecycle: None, outcome: SessionArrival::Data(Arc::new(data)) }
}
fn login(owner: &mut SessionMachine, arrival: u64, progress: LoginProgress, terminal: bool) -> SessionEnvelope {
    let e = envelope(owner, owner.common.next_req, arrival, terminal, Observation::Login(progress));
    let effects = step(owner, SessionEvent::Result(e.clone()));
    assert!(matches!(effects.as_slice(), [SessionFx::Acknowledge(r)] if r == &vec![Receipt::of(&e)]));
    e
}
fn start(owner: &mut SessionMachine) -> u64 {
    let effects = command(owner, Command::StartLogin);
    assert!(matches!(effects.as_slice(), [SessionFx::Cancel { requests, epoch: 2 },
        SessionFx::Coordinator(CoordinatorAction::SignInStarted),
        SessionFx::Work { req: 1, key: SessionWorkKey { epoch: 2, op: SessionOp::Login },
            admission: AdmissionId(1), input: SessionWork::Login { client_id } }]
        if requests.is_empty() && client_id == "synthetic-client"));
    assert_eq!(owner.read().0.phase, Phase::Creating);
    owner.common.epoch
}
fn waiting(owner: &mut SessionMachine) -> u64 {
    let epoch = start(owner);
    login(owner, 1, LoginProgress::CodeReady { epoch, code: "TEST".into(), qr_png: vec![1, 2, 3] }, false);
    epoch
}
fn commit_reply(effects: &[SessionFx], admission: CommitAdmission) -> CommitReply {
    let [SessionFx::Commit { req, epoch, arrival, .. }] = effects else { panic!("exactly one commit required") };
    CommitReply { req: *req, epoch: *epoch, arrival: *arrival, admission }
}
fn durable() -> CompletionOutcome {
    CompletionOutcome::Durable(Operation::Write {
        outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None })
}

#[test]
fn arbitrary_legacy_phases_and_erasing_roundtrip_without_normalizing_residue() {
    for phase in [Phase::Idle, Phase::Creating, Phase::Waiting, Phase::Discovering, Phase::Profiles,
        Phase::Switching, Phase::Ready, Phase::Error, Phase::Deleted] {
        for erasing in [false, true] {
            let mut init = captured();
            init.phase = phase;
            init.pending_erase = erasing.then_some((73, true));
            init.picker = Picker::ChangeProfile;
            init.pin_code = "synthetic-code".into();
            init.qr_png = vec![1, 2, 3];
            init.qr_gen = 9;
            init.code_replaced = true;
            init.link_trouble = true;
            init.users = vec![UserTile { uuid: "synthetic-retained".into(), ..Default::default() }];
            init.error = "synthetic-error".into();
            init.pin_denied = true;
            init.signin_active = true;
            init.authorized_in_flow = true;
            init.persistence_warning_answered = true;
            init.apply_pending = true;
            init.signin_account = Some("Synthetic Hidden Name".into());
            let mut owner = SessionMachine::from_init(init.clone());
            assert_eq!(owner.read().0.phase, phase);
            let restored = owner.snapshot_init();
            assert_eq!(serde_json::to_value(&restored).unwrap(), serde_json::to_value(&init).unwrap());
            assert_eq!(restored.signin_account, init.signin_account);
            let wire = serde_json::to_value(&restored).unwrap();
            assert!(wire.get("signin_account").is_none());
            let decoded: SessionInit = serde_json::from_value(wire).unwrap();
            assert!(decoded.signin_account.is_none());
            assert_eq!(decoded.hash(), init.hash());
            assert_eq!(owner.subhash(), init.hash());
            assert!(command(&mut owner, Command::DismissPinError).is_empty());
            init.pin_denied = false;
            let after = owner.snapshot_init();
            assert_eq!(serde_json::to_value(&after).unwrap(), serde_json::to_value(&init).unwrap());
            assert_eq!(after.signin_account, init.signin_account);
            assert_eq!(owner.subhash(), init.hash());
        }
    }
}

#[test]
fn qr_waiting_replacement_discovery_and_duplicate_receipts() {
    let mut owner = SessionMachine::from_init(captured());
    let epoch = waiting(&mut owner);
    let retained = owner.publication();
    let code = envelope(&owner, 1, 1, false, Observation::Login(LoginProgress::CodeReady {
        epoch, code: "TEST".into(), qr_png: vec![1, 2, 3] }));
    let fx = step(&mut owner, SessionEvent::Result(code.clone()));
    assert!(matches!(fx.as_slice(), [SessionFx::Acknowledge(r)] if r == &vec![Receipt::of(&code)]));
    assert!(Arc::ptr_eq(&retained, &owner.publication()));
    login(&mut owner, 2, LoginProgress::CodeReplacing { epoch }, false);
    assert_eq!(owner.read().0.phase, Phase::Creating);
    assert!(owner.read().0.code.is_empty());
    assert!(owner.read().0.code_replaced);
    login(&mut owner, 3, LoginProgress::CodeReady { epoch, code: "NEXT".into(), qr_png: vec![4] }, false);
    assert_eq!(owner.read().0.qr_generation, 2);
    login(&mut owner, 4, LoginProgress::Authorized { epoch, token: "synthetic-new-account".into() }, false);
    assert_eq!(owner.read().0.phase, Phase::Discovering);
    assert!(owner.state.projected_authorization());
    assert_eq!(owner.common.pending[&1].expected.account_token, "synthetic-new-account");
    assert_eq!(&*retained.code, "TEST");
    assert_eq!(&*retained.png, &[1, 2, 3]);
    // Authorization does NOT clear the old QR payload in today's flat state.
    assert_eq!(&*owner.read().0.code, "NEXT");
}

#[test]
fn discovery_failure_private_account_is_neither_wire_nor_hash_authority() {
    let mut owner = SessionMachine::from_init(captured());
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-account".into() }, false);
    let failed = envelope(&owner, 1, 3, true, Observation::Login(LoginProgress::Failed {
        epoch, message: plx_platform::i18n::msg::browse_auth_no_servers().into(),
        incident: IncidentContext::internal(InternalClass::WorkerDropped), plaintext: None,
        account: Some("Synthetic Person".into()) }));
    let fx = step(&mut owner, SessionEvent::Result(failed.clone()));
    assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::SignInFailed {
        phase: Phase::Discovering }), SessionFx::Acknowledge(r)] if r == &vec![Receipt::of(&failed)]));
    assert_eq!(owner.read().0.phase, Phase::Error);
    assert_eq!(owner.read().0.account.as_deref(), Some("Synthetic Person"));
    let init = owner.snapshot_init();
    let bytes = serde_json::to_vec(&init).unwrap();
    assert!(!String::from_utf8(bytes.clone()).unwrap().contains("Synthetic Person"));
    let decoded: SessionInit = serde_json::from_slice(&bytes).unwrap();
    assert!(decoded.signin_account.is_none());
    assert_eq!(init.hash(), decoded.hash());
    assert_eq!(bytes, serde_json::to_vec(&decoded).unwrap());
    assert!(SessionMachine::from_init(decoded).read().0.account.is_none());
    let fx = command(&mut owner, Command::Retry);
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { .. },
        SessionFx::Coordinator(CoordinatorAction::SignInStarted),
        SessionFx::Work { key: SessionWorkKey { op: SessionOp::Rediscover, .. }, .. }]));
    assert_eq!(owner.read().0.phase, Phase::Discovering);
    assert!(owner.read().0.account.is_none());
}

#[test]
fn back_cancels_login_but_only_resumes_committed_credentials() {
    // Boot BACK requires a positively identified, unprotected stored profile.
    // An empty UUID is protected (Session::active_profile_is_protected), even with a PMS token.
    let mut saved = local().persisted;
    saved.user.uuid = "synthetic-saved-profile".into();
    saved.home_users = vec![plx_plex::plex::session::HomeUserRef {
        uuid: saved.user.uuid.clone(), protected: false, ..Default::default() }];
    let mut owner = SessionMachine::from_init(SessionInit::captured(saved));
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-provisional".into() }, false);
    let reply = ReplyTo { instance: 4, correlation: 9 };
    let fx = command(&mut owner, Command::BackAtRoot { reply });
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { requests, epoch: 3 },
        SessionFx::Coordinator(CoordinatorAction::SignInCancelled),
        SessionFx::BackReply { to: ReplyTo { instance: 4, correlation: 9 }, resumed: true }]
        if requests == &vec![1]), "BACK effects: {:?}", safe_effect_diagnostics(&fx));
    assert_eq!(owner.common.persisted.account_token, "synthetic-account");
    assert_eq!(owner.common.persisted.user.uuid, "synthetic-saved-profile");
    assert_eq!(owner.common.persisted.server.token, "synthetic-pms");
    assert_eq!(owner.read().0.phase, Phase::Ready);
    assert!(owner.state.handoff_pending());
    assert!(owner.read().0.code.is_empty());
    assert!(owner.read().0.png.is_empty());
    assert_eq!(owner.read().0.qr_generation, 0);
    let fx = command(&mut owner, Command::TakeReady);
    let reply = commit_reply(&fx, CommitAdmission::RegistryOnly);
    let fx = step(&mut owner, SessionEvent::Commit(reply));
    assert!(matches!(fx.as_slice(), [SessionFx::PublishProfile(_), SessionFx::Ready { .. }, SessionFx::Retire { .. }]));
    assert!(step(&mut owner, SessionEvent::Commit(reply)).is_empty());
}

#[test]
fn root_back_without_saved_authority_is_an_addressed_negative_only() {
    let mut owner = SessionMachine::from_init(captured());
    waiting(&mut owner);
    let publication = owner.publication();
    let fx = command(&mut owner, Command::BackAtRoot { reply: ReplyTo { instance: 8, correlation: 7 } });
    assert!(matches!(fx.as_slice(), [SessionFx::BackReply {
        to: ReplyTo { instance: 8, correlation: 7 }, resumed: false }]));
    assert!(Arc::ptr_eq(&publication, &owner.publication()));
}

#[test]
fn erase_is_ordered_and_stale_completions_do_not_finish_it() {
    let mut owner = SessionMachine::from_init(captured());
    waiting(&mut owner);
    let fx = command(&mut owner, Command::EraseLocal);
    assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::CloseTelemetry),
        SessionFx::Cancel { requests, epoch: 3 }, SessionFx::PublishProfile(_),
        SessionFx::Erase { req: 1, epoch: 3, all_local: true }] if requests == &vec![1]));
    // erase() clears presentation immediately but keeps Waiting until erased().
    assert_eq!(owner.read().0.phase, Phase::Waiting);
    assert!(owner.read().0.code.is_empty());
    assert!(owner.read().0.png.is_empty());
    assert_eq!(owner.read().0.qr_generation, 0);
    assert!(owner.state.picker_data().users.is_empty());
    assert!(owner.state.picker_data().error.is_empty());
    assert!(!owner.state.handoff_pending());
    assert!(owner.common.persisted.account_token.is_empty());
    assert!(command(&mut owner, Command::StartLogin).is_empty());
    assert!(step(&mut owner, SessionEvent::Erased { epoch: 2, leftovers: 99 }).is_empty());
    assert!(owner.state.pending_erase().is_some());
    let fx = step(&mut owner, SessionEvent::Erased { epoch: 3, leftovers: 2 });
    assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::LocalDataErased)]));
    assert_eq!(owner.read().0.phase, Phase::Deleted);
    assert_eq!(owner.read().0.delete_leftovers, 2);
}

#[test]
fn signout_completion_starts_one_new_signin_after_erase_not_before() {
    let mut owner = SessionMachine::from_init(captured());
    let fx = command(&mut owner, Command::SignOut);
    assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::CloseTelemetry),
        SessionFx::Cancel { epoch: 2, .. }, SessionFx::PublishProfile(_),
        SessionFx::Erase { epoch: 2, all_local: false, .. }]));
    let fx = step(&mut owner, SessionEvent::Erased { epoch: 2, leftovers: 0 });
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { epoch: 3, .. },
        SessionFx::Coordinator(CoordinatorAction::SignInStarted),
        SessionFx::Capture { req: 1, epoch: 3, request: SessionReadRequest::LoginClientId }]));
    assert!(step(&mut owner, SessionEvent::Erased { epoch: 2, leftovers: 0 }).is_empty());
}

fn picker() -> SessionMachine {
    let mut init = local();
    init.persisted.home_users = vec![plx_plex::plex::session::HomeUserRef {
        uuid: "synthetic-other".into(), title: "Other".into(), ..Default::default() }];
    // Re-capture, so committed credentials agree with the actual boot input.
    let mut owner = SessionMachine::from_init(SessionInit::captured(init.persisted));
    let fx = command(&mut owner, Command::StartSwitch(Picker::ChangeProfile));
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { .. }, SessionFx::PublishProfile(_),
        SessionFx::Commit { .. }, SessionFx::Work { input: SessionWork::ServerRoster { .. }, .. },
        SessionFx::Work { input: SessionWork::HomeRoster { .. }, .. }]));
    let reply = fx.iter().find_map(|f| match f { SessionFx::Commit { req, epoch, arrival, .. } =>
        Some(CommitReply { req: *req, epoch: *epoch, arrival: *arrival, admission: CommitAdmission::RegistryOnly }),
        SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. } | SessionFx::Ready { .. }
        | SessionFx::Capture { .. } | SessionFx::Work { .. } | SessionFx::Cancel { .. }
        | SessionFx::PublishProfile(_) | SessionFx::Erase { .. } | SessionFx::Coordinator(_)
        | SessionFx::RestartReply { .. } | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
        | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
        | SessionFx::RefreshCache { .. } => None }).unwrap();
    let settled = step(&mut owner, SessionEvent::Commit(reply));
    assert!(matches!(settled.as_slice(), [SessionFx::Retire { .. }]));
    owner
}
#[test]
fn profiles_selection_capture_pin_failure_and_root_back() {
    let mut owner = picker();
    assert_eq!(owner.read().0.phase, Phase::Profiles);
    let reply = ReplyTo { instance: 5, correlation: 6 };
    let fx = command(&mut owner, Command::SelectProfileWithReply { index: 0, pin: Some("0000".into()), reply });
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { .. },
        SessionFx::Capture { request: SessionReadRequest::ProfilePolicy, .. },
        SessionFx::SelectionReply { to: ReplyTo { instance: 5, correlation: 6 }, accepted: true, .. }]));
    assert_eq!(owner.read().0.phase, Phase::Switching);
    let req = owner.common.next_req;
    let epoch = owner.common.epoch;
    let fx = step(&mut owner, SessionEvent::Read(SessionReadReply {
        addr: Addr { to: MachineId::Session, req: RequestId(req) }, epoch,
        value: SessionReadValue::ProfilePolicy { recently_unreachable: false } }));
    assert!(matches!(fx.as_slice(), [SessionFx::Work { req: r, input: SessionWork::ProfileSwitch { pin: Some(p), .. }, .. }]
        if *r == req && p == "0000"));
    let e = envelope(&owner, req, 1, true, Observation::ProfileSwitch(super::super::ProfileSwitchProgress {
        epoch, expected: super::super::SessionIdentity::of(&owner.common.persisted),
        outcome: super::super::ProfileSwitchOutcomeProgress::Failed { error: "synthetic-pin-denied".into(), pin_denied: true } }));
    let fx = step(&mut owner, SessionEvent::Result(e.clone()));
    assert!(matches!(fx.as_slice(), [SessionFx::Retire { req: r }, SessionFx::Acknowledge(rs)]
        if *r == req && rs == &vec![Receipt::of(&e)]));
    assert_eq!(owner.read().0.phase, Phase::Profiles);
    assert!(owner.read().0.pin_denied);
    assert!(command(&mut owner, Command::DismissPinError).is_empty());
    assert!(!owner.read().0.pin_denied);
    let fx = command(&mut owner, Command::BackAtRoot { reply });
    assert!(matches!(fx.as_slice(), [SessionFx::BackReply { resumed: false, .. }]));
}

#[test]
fn superseding_a_pending_commit_revokes_permit_and_returns_owned_credit_once() {
    let mut owner = SessionMachine::from_init(captured());
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-account".into() }, false);
    let e = envelope(&owner, 1, 3, true, Observation::Login(LoginProgress::SignedIn {
        epoch, server: local().persisted.server, sources: Vec::new(), users: Vec::new() }));
    let fx = step(&mut owner, SessionEvent::Result(e.clone()));
    let old = commit_reply(&fx, CommitAdmission::RegistryOnly);
    assert!(owner.commit_permit(old.req, old.epoch, old.arrival).is_some());
    assert_eq!(owner.read().0.phase, Phase::Discovering);
    let fx = command(&mut owner, Command::StartLogin);
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { epoch: 2, .. }, SessionFx::Acknowledge(rs),
        SessionFx::Cancel { epoch: 3, .. }, SessionFx::Coordinator(CoordinatorAction::SignInStarted),
        SessionFx::Work { .. }] if rs == &vec![Receipt::of(&e)]));
    assert!(owner.commit_permit(old.req, old.epoch, old.arrival).is_none());
    assert!(step(&mut owner, SessionEvent::Commit(old)).is_empty());
    assert_eq!(owner.read().0.phase, Phase::Creating);
}

#[test]
fn stored_ready_admission_is_not_durability_and_background_is_not_saved_login() {
    let mut owner = SessionMachine::from_init(local());
    let fx = command(&mut owner, Command::ResumeStored);
    let reply = commit_reply(&fx, CommitAdmission::RegistryOnly);
    let fx = step(&mut owner, SessionEvent::Commit(reply));
    assert!(matches!(fx.as_slice(), [SessionFx::PublishProfile(_), SessionFx::Ready { .. }, SessionFx::Retire { .. }]));
    assert_eq!(owner.read().0.phase, Phase::Ready);
    assert!(!owner.common.commit_phase.admitted);
    assert!(!owner.common.commit_phase.durable);
    // Actual supported refresh command, with an ordinary synthetic HomeRoster result.
    let mut owner = picker();
    let req = owner.common.pending.iter().find(|(_, p)| p.key.op == SessionOp::HomeRoster).unwrap().0.to_owned();
    let epoch = owner.common.epoch;
    let e = envelope(&owner, req, 1, true, Observation::HomeRoster(super::super::HomeRosterProgress {
        epoch, expected: super::super::SessionIdentity::of(&owner.common.persisted),
        users: Some(vec![UserTile { uuid: "synthetic-other".into(), ..Default::default() }]) }));
    let fx = step(&mut owner, SessionEvent::Result(e.clone()));
    let reply = commit_reply(&fx, CommitAdmission::Admitted { revision: 7, purpose: PersistencePurpose::Background });
    let fx = step(&mut owner, SessionEvent::Commit(reply));
    assert!(matches!(fx.as_slice(), [SessionFx::Retire { .. }, SessionFx::Acknowledge(rs)] if rs == &vec![Receipt::of(&e)]));
    assert!(owner.common.commit_phase.admitted && !owner.common.commit_phase.durable);
    let completion = PersistenceCompletion { req, epoch, arrival: 1, revision: 7,
        purpose: PersistencePurpose::Background, outcome: durable() };
    assert!(step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { revision: 8, ..completion })).is_empty());
    assert!(!owner.common.commit_phase.durable);
    assert!(step(&mut owner, SessionEvent::Persistence(completion)).is_empty());
    assert!(owner.common.commit_phase.durable);
    assert!(!owner.common.commit_phase.proves_saved_login);
}

#[test]
fn superseded_durable_disk_evidence_never_seats_the_superseded_identity() {
    let mut owner = SessionMachine::from_init(captured());
    let before = owner.common.disk_identity.clone();
    let after = Identity { account_token: "synthetic-superseded".into(), ..before.clone() };
    let retained = owner.publication();
    assert!(step(&mut owner, SessionEvent::DiskWrite { before: before.clone(), after: after.clone(), outcome: durable() }).is_empty());
    assert!(owner.common.disk_identity == after);
    assert_eq!(owner.common.persisted.account_token, "");
    assert!(Arc::ptr_eq(&retained, &owner.publication()));
    assert!(step(&mut owner, SessionEvent::DiskWrite { before, after, outcome: durable() }).is_empty());
}

// Independent V5 boot-field census, deliberately NOT `SessionInit::write` or a parallel machine.
// Freeze the ordering/tags of the supported pre-effect boot, including all absent option markers.
fn frozen_v5_boot_hash(s: &SessionInit) -> u64 {
    let mut w = Canon::new();
    w.str(&s.disk_identity.client_id).str(&s.disk_identity.account_token).str(&s.disk_identity.profile_uuid);
    match &s.authority {
        BootstrapAuthority::Account { extras } => { w.u8(0); write_sources(&mut w, extras); }
        BootstrapAuthority::DevPms { primary, extras } => { w.u8(1); write_server(&mut w, primary); write_sources(&mut w, extras); }
    }
    w.u8(0).u8(0);
    write_persisted(&mut w, &s.persisted);
    write_persisted(&mut w, &s.committed_credentials.merge_into(&PersistedSession::default()));
    w.str("").seq(0).seq(0).str("").bool(false).bool(false).bool(false).bool(false).bool(false)
        .u64(0).u64(0).u64(1).u32(0).u64(0).u32(0);
    w.option(None::<u8>, |_, _| {}).seq(0); // active profile, pending
    w.option(None::<u8>, |_, _| {}).bool(false).bool(false).bool(false); // purpose, commit phase
    w.option(None::<u8>, |_, _| {}).option(None::<u8>, |_, _| {}).bool(false); // warning, held, answered
    w.option(None::<u8>, |_, _| {}).option(None::<u8>, |_, _| {}); // pending commit, erase
    w.bool(false).seq(0).option(None::<u8>, |_, _| {}).seq(0).u32(0).bool(false);
    w.option(None::<u8>, |_, _| {}); // switch-refused; plaintext/retry append NOTHING when absent
    w.finish()
}
#[test]
fn v5_boot_roundtrip_and_frozen_canonical_field_order() {
    let saved = local().persisted;
    let primary = saved.server.clone();
    for init in [captured(), SessionInit::captured_boot(saved, Some(primary), Vec::new())] {
        assert_eq!(init.phase, Phase::Idle);
        assert_eq!(init.hash(), frozen_v5_boot_hash(&init));
        // LEGACY GOLDEN PROBE: coordinator freezes these prior-owner values, never the replacement's.
        let (authority_label, case, expected_hash) = match &init.authority {
            BootstrapAuthority::Account { .. } => ("Account", "boot-account", 276035472506033726),
            BootstrapAuthority::DevPms { .. } => ("DevPms", "boot-devpms", 13167250867475899844),
        };
        assert_eq!(init.hash(), expected_hash);
        emit_legacy_v5_artifact(case, &init);
        eprintln!("LEGACY_V5_BOOT {} HASH={}", authority_label, init.hash());
        let bytes = serde_json::to_vec(&init).unwrap();
        let expected: &[u8] = match case {
            "boot-account" => include_bytes!("fixtures/legacy-v5/boot-account.json"),
            "boot-devpms" => include_bytes!("fixtures/legacy-v5/boot-devpms.json"),
            _ => unreachable!(),
        };
        assert!(bytes.as_slice() == expected, "legacy V5 bytes differ for {case}");
        let decoded: SessionInit = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&decoded).unwrap());
        assert_eq!(init.hash(), decoded.hash());
        assert!(decoded.signin_account.is_none());
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("signin_account").is_none());
        assert!(value.get("discovery_retry").is_none());
        assert_eq!(value["phase"], "Idle");
        assert_eq!(value["picker"], "Boot");
        assert_eq!(value["pin_code"], "");
        assert_eq!(value["qr_png"], serde_json::json!([]));
        assert_eq!(value["pending"], serde_json::json!({}));
        assert_eq!(value["epoch"], 1);
        assert_eq!(SessionMachine::from_init(decoded).subhash(), init.hash());
    }
}

#[test]
fn profile_seating_revokes_once_and_late_roster_is_same_identity_refresh() {
    let mut owner = picker();
    command(&mut owner, Command::SelectProfile { index: 0, pin: None });
    let req = owner.common.next_req;
    let epoch = owner.common.epoch;
    step(&mut owner, SessionEvent::Read(SessionReadReply {
        addr: Addr { to: MachineId::Session, req: RequestId(req) }, epoch,
        value: SessionReadValue::ProfilePolicy { recently_unreachable: false } }));
    let primary = plx_plex::plex::session::SourceRef {
        machine_id: "synthetic-server".into(), address: "127.0.0.1".into(), port: 32400,
        token: "synthetic-profile-token".into(), owned: true, ..Default::default() };
    let e = envelope(&owner, req, 1, false, Observation::ProfileSwitch(super::super::ProfileSwitchProgress {
        epoch, expected: super::super::SessionIdentity::of(&owner.common.persisted),
        outcome: super::super::ProfileSwitchOutcomeProgress::Ready {
            delta: super::super::ProfileDelta { server: super::super::server_ref(&primary),
                sources: vec![primary.clone()], user: UserRef { uuid: "synthetic-other".into(),
                    token: primary.token.clone(), ..Default::default() }, cache: None }, probes: Vec::new() } }));
    let fx = step(&mut owner, SessionEvent::Result(e.clone()));
    assert!(matches!(fx.as_slice(), [SessionFx::Commit { plan, .. }]
        if matches!(plan.registry.as_slice(), [RegistryPlan::Install { commit: RosterCommit::Switch, .. }])));
    let reply = commit_reply(&fx, CommitAdmission::RegistryOnly);
    let fx = step(&mut owner, SessionEvent::Commit(reply));
    assert!(matches!(fx.as_slice(), [SessionFx::Acknowledge(rs)] if rs == &vec![Receipt::of(&e)]));
    assert_eq!(owner.read().0.phase, Phase::Ready);
    assert!(owner.common.pending[&req].phase == StreamPhase::ProfileSeated);
    let expected = super::super::SessionIdentity::of(&owner.common.persisted);
    let plan = scenarios::late_profile_roster(&mut owner, req, epoch, &primary, expected);
    assert!(matches!(plan.registry.as_slice(), [RegistryPlan::Install {
        commit: RosterCommit::Refresh { same_identity: true }, .. }]));
    // No registry executor is run: the exact value is the owner-level revocation contract.
}

#[test]
fn failure_readout_survives_resuming_back_erase_and_deleted() {
    let mut saved = local().persisted;
    saved.user.uuid = "synthetic-saved-profile".into();
    saved.home_users = vec![plx_plex::plex::session::HomeUserRef {
        uuid: saved.user.uuid.clone(), protected: false, ..Default::default() }];
    let mut owner = SessionMachine::from_init(SessionInit::captured(saved));
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-provisional".into() }, false);
    let verdict = super::super::PlaintextVerdict {
        machine_id: "synthetic-lan".into(), name: "Synthetic LAN".into(), shared_by: String::new(),
        eligibility: plx_plex::plex::probe::PlaintextEligibility::Eligible,
        choice: plx_plex::plex::session::PlaintextChoice::Undecided };
    let failed = envelope(&owner, 1, 3, true, Observation::Login(LoginProgress::Failed {
        epoch, message: "synthetic-insecure-only".into(),
        incident: IncidentContext::internal(InternalClass::WorkerDropped),
        plaintext: Some(verdict), account: Some("Synthetic Person".into()) }));
    let fx = step(&mut owner, SessionEvent::Result(failed.clone()));
    assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::SignInFailed {
        phase: Phase::Discovering }), SessionFx::Acknowledge(rs)] if rs == &vec![Receipt::of(&failed)]), "{:?}", safe_effect_diagnostics(&fx));
    let readout = serde_json::to_vec(&owner.state.plaintext()).unwrap();
    let fx = command(&mut owner, Command::BackAtRoot { reply: ReplyTo { instance: 1, correlation: 2 } });
    // Terminal Failed already retired login; signin_active was consumed by SignInFailed.
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { requests, epoch: 3 },
        SessionFx::BackReply { resumed: true, .. }] if requests.is_empty()), "{:?}", safe_effect_diagnostics(&fx));
    assert_eq!(owner.read().0.phase, Phase::Ready);
    assert_eq!(owner.common.persisted.account_token, "synthetic-account");
    for phase in [Phase::Ready, Phase::Ready, Phase::Deleted] {
        assert_eq!(owner.read().0.phase, phase);
        assert_eq!(serde_json::to_vec(&owner.state.plaintext()).unwrap(), readout);
        assert_eq!(owner.state.signin_account().as_deref(), Some("Synthetic Person"));
        // readout_account:845-847 hides the retained private name outside the no-server Error.
        assert!(owner.read().0.account.is_none());
        if owner.state.pending_erase().is_some() {
            let fx = step(&mut owner, SessionEvent::Erased { epoch: 4, leftovers: 1 });
            assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::LocalDataErased)]), "{:?}", safe_effect_diagnostics(&fx));
        } else if phase == Phase::Ready {
            let fx = command(&mut owner, Command::EraseLocal);
            assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::CloseTelemetry),
                SessionFx::Cancel { requests, epoch: 4 }, SessionFx::PublishProfile(_),
                SessionFx::Erase { epoch: 4, all_local: true, .. }] if requests.is_empty()), "{:?}", safe_effect_diagnostics(&fx));
            assert!(owner.read().0.code.is_empty());
            assert!(!owner.state.handoff_pending());
        }
    }
}

#[test]
fn discovery_retry_is_live_restorable_but_epoch_advance_clears_it() {
    let mut owner = SessionMachine::from_init(captured());
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-account".into() }, false);
    login(&mut owner, 3, LoginProgress::DiscoveryTrouble { epoch,
        progress: super::super::DiscoveryRetryProgress {
            run: super::super::DiscoveryRetryRun::Resources, misses: 2, elapsed_ms: 1234 } }, false);
    let init = owner.snapshot_init();
    let mut restored = SessionMachine::from_init(init.clone());
    assert_eq!(restored.subhash(), init.hash());
    assert_eq!(restored.state.discovery_retry().unwrap().elapsed_ms, 1234);
    let fx = command(&mut restored, Command::EraseLocal);
    assert!(matches!(fx.as_slice(), [SessionFx::Coordinator(CoordinatorAction::CloseTelemetry),
        SessionFx::Cancel { epoch: 3, .. }, SessionFx::PublishProfile(_), SessionFx::Erase { epoch: 3, .. }]), "{:?}", safe_effect_diagnostics(&fx));
    // Important source correction: advance_epoch:1789 explicitly clears this anchor.
    assert!(restored.state.discovery_retry().is_none());
    assert_eq!(restored.read().0.phase, Phase::Discovering);
    step(&mut restored, SessionEvent::Erased { epoch: 3, leftovers: 0 });
    assert_eq!(restored.read().0.phase, Phase::Deleted);
    assert!(restored.state.discovery_retry().is_none());
    assert!(serde_json::to_vec(&init).unwrap().as_slice() == include_bytes!("fixtures/legacy-v5/live.json"),
        "legacy V5 bytes differ for live");
    assert_eq!(init.hash(), 18001482832323156010);
    emit_legacy_v5_artifact("live", &init);
    eprintln!("LEGACY_V5_LIVE HASH={}", init.hash());
}

#[test]
fn fresh_final_warning_holds_publication_until_addressed_continue() {
    use plx_plex::plex::session::async_persistence::Failure;
    let mut owner = SessionMachine::from_init(captured());
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-account".into() }, false);
    let e = envelope(&owner, 1, 3, true, Observation::Login(LoginProgress::SignedIn {
        epoch, server: local().persisted.server, sources: Vec::new(),
        users: vec![UserTile { uuid: "synthetic-only".into(), ..Default::default() }] }));
    let fx = step(&mut owner, SessionEvent::Result(e));
    assert!(matches!(fx.as_slice(), [SessionFx::Commit { plan, .. }]
        if plan.authority == plx_plex::plex::session::SaveAuthority::FreshReauthentication), "{:?}", safe_effect_diagnostics(&fx));
    let reply = commit_reply(&fx, CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery });
    let fx = step(&mut owner, SessionEvent::Commit(reply));
    assert!(!fx.iter().any(|f| matches!(f, SessionFx::PublishProfile(_) | SessionFx::Ready { .. })), "{:?}", safe_effect_diagnostics(&fx));
    assert!(owner.state.projected_authorization());
    assert!(step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req: reply.req,
        epoch, arrival: 3, revision: 1, purpose: PersistencePurpose::Discovery, outcome: durable() })).is_empty());
    let fx = command(&mut owner, Command::TakeReady);
    assert!(matches!(fx.as_slice(), [SessionFx::Commit { plan, .. }]
        if plan.authority == plx_plex::plex::session::SaveAuthority::FreshReauthentication), "{:?}", safe_effect_diagnostics(&fx));
    assert!(!owner.state.projected_authorization());
    let reply = commit_reply(&fx, CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final });
    let fx = step(&mut owner, SessionEvent::Commit(reply));
    assert!(matches!(fx.as_slice(), [SessionFx::Retire { req }] if *req == reply.req), "{:?}", safe_effect_diagnostics(&fx));
    assert!(owner.common.held_handoff.is_some());
    let completion = PersistenceCompletion { req: reply.req, epoch, arrival: 0, revision: 2,
        purpose: PersistencePurpose::Final,
        outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
    assert!(step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { revision: 3, ..completion })).is_empty());
    assert!(owner.common.persistence_warning.is_none());
    assert!(step(&mut owner, SessionEvent::Persistence(completion)).is_empty());
    let key = owner.common.persistence_warning.unwrap().key;
    let fx = command(&mut owner, Command::BackAtRoot { reply: ReplyTo { instance: 2, correlation: 3 } });
    assert!(matches!(fx.as_slice(), [SessionFx::BackReply { resumed: false, .. }]), "{:?}", safe_effect_diagnostics(&fx));
    assert!(command(&mut owner, Command::TakeReady).is_empty());
    let fx = command(&mut owner, Command::AcknowledgePersistenceWarning { key });
    assert!(matches!(fx.as_slice(), [SessionFx::PublishProfile(_), SessionFx::Ready { .. }]), "{:?}", safe_effect_diagnostics(&fx));
    assert!(owner.common.held_handoff.is_none());
    assert!(!owner.common.commit_phase.proves_saved_login);
    assert!(command(&mut owner, Command::AcknowledgePersistenceWarning { key }).is_empty());
}

#[test]
fn busy_commit_retains_fifo_and_supersession_returns_each_credit_once() {
    let mut owner = SessionMachine::from_init(captured());
    let epoch = waiting(&mut owner);
    login(&mut owner, 2, LoginProgress::Authorized { epoch, token: "synthetic-account".into() }, false);
    let e = envelope(&owner, 1, 3, true, Observation::Login(LoginProgress::SignedIn {
        epoch, server: local().persisted.server, sources: Vec::new(), users: Vec::new() }));
    let fx = step(&mut owner, SessionEvent::Result(e.clone()));
    let old = commit_reply(&fx, CommitAdmission::RegistryOnly);
    let second = envelope(&owner, 1, 4, false, Observation::Login(LoginProgress::CodeReplacing { epoch }));
    let third = envelope(&owner, 1, 5, false, Observation::Login(LoginProgress::CodeReplacing { epoch }));
    for record in [&second, &third, &second] {
        assert!(step(&mut owner, SessionEvent::Result(record.clone())).is_empty());
    }
    assert!(owner.common.inbox.iter().map(Receipt::of).collect::<Vec<_>>() ==
        vec![Receipt::of(&second), Receipt::of(&third)]);
    assert!(!owner.common.pump_pending);
    assert_eq!(owner.common.pending[&1].last_arrival, Some(3));
    let fx = command(&mut owner, Command::StartLogin);
    // discard_owned_envelopes drains FIFO first, then returns the commit's owned receipt.
    assert!(matches!(fx.as_slice(), [SessionFx::Cancel { epoch: 2, .. }, SessionFx::Acknowledge(rs),
        SessionFx::Cancel { epoch: 3, .. }, SessionFx::Coordinator(CoordinatorAction::SignInStarted),
        SessionFx::Work { .. }] if rs == &vec![Receipt::of(&second), Receipt::of(&third), Receipt::of(&e)]), "{:?}", safe_effect_diagnostics(&fx));
    assert!(owner.common.inbox.is_empty());
    assert!(owner.commit_permit(old.req, old.epoch, old.arrival).is_none());
    assert!(step(&mut owner, SessionEvent::Commit(old)).is_empty());
}

#[test]
fn nested_v5_prior_writer_golden_probe() {
    let owner = scenarios::roster_refresh_fixture("synthetic-profile", vec![
        plx_plex::plex::session::HomeUserRef { uuid: "synthetic-profile".into(),
            title: "Synthetic Profile".into(), protected: false, admin: true, ..Default::default() }]);
    let init = owner.snapshot_init();
    assert!(!init.persisted.sources.is_empty());
    assert!(!init.persisted.profiles.is_empty());
    let bytes = serde_json::to_vec(&init).unwrap();
    assert!(bytes.as_slice() == include_bytes!("fixtures/legacy-v5/nested.json"),
        "legacy V5 bytes differ for nested");
    let decoded: SessionInit = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(init.hash(), decoded.hash());
    assert_eq!(bytes, serde_json::to_vec(&decoded).unwrap());
    // Capture complete prior-writer bytes before any replacement, not a generated expectation.
    assert_eq!(init.hash(), 776259573816388212);
    emit_legacy_v5_artifact("nested", &init);
    eprintln!("LEGACY_V5_NESTED HASH={}", init.hash());
    let mut private = init.clone();
    private.signin_account = Some("Synthetic Hidden Name".into());
    assert_eq!(private.hash(), init.hash());
    assert_eq!(serde_json::to_vec(&private).unwrap(), serde_json::to_vec(&init).unwrap());
    assert_eq!(SessionMachine::from_init(private).snapshot_init().signin_account.as_deref(),
        Some("Synthetic Hidden Name"));
}

#[test]
fn restored_accepted_discovery_keeps_stream_authority_and_matching_retry_fence() {
    let mut original = SessionMachine::from_init(captured());
    let epoch = waiting(&mut original);
    let req = original.common.next_req;
    let key = original.common.pending[&req].key;
    let admission = AdmissionReply { addr: Addr { to: MachineId::Session, req: RequestId(req) },
        key, correlation: AdmissionId(req), accepted: true };
    assert!(step(&mut original, SessionEvent::Admission(admission)).is_empty());
    assert!(original.common.pending[&req].admission == AdmissionState::Accepted(AdmissionId(req)));
    login(&mut original, 2, LoginProgress::Authorized { epoch, token: "synthetic-account".into() }, false);
    login(&mut original, 3, LoginProgress::DiscoveryTrouble { epoch,
        progress: super::super::DiscoveryRetryProgress {
            run: super::super::DiscoveryRetryRun::Resources, misses: 2, elapsed_ms: 1234 } }, false);
    let mut restored = SessionMachine::from_init(original.snapshot_init());
    assert_eq!(original.subhash(), restored.subhash());
    assert_eq!(restored.common.pending[&req].last_arrival, Some(3));
    assert!(restored.common.pending[&req].admission == AdmissionState::Accepted(AdmissionId(req)));
    for (arrival, run) in [(4, super::super::DiscoveryRetryRun::HomeUsers),
        (5, super::super::DiscoveryRetryRun::Resources)] {
        let record = envelope(&original, req, arrival, false,
            Observation::Login(LoginProgress::DiscoveryRetrySettled { epoch, run }));
        let original_fx = step(&mut original, SessionEvent::Result(record.clone()));
        let restored_fx = step(&mut restored, SessionEvent::Result(record));
        assert_eq!(serde_json::to_value(original_fx).unwrap(), serde_json::to_value(restored_fx).unwrap());
        assert_eq!(serde_json::to_value(original.snapshot_init()).unwrap(),
            serde_json::to_value(restored.snapshot_init()).unwrap());
        assert_eq!(original.subhash(), restored.subhash());
        assert_eq!(restored.read().0.discovery_retry.is_some(), arrival == 4);
    }
}

#[test]
fn restored_empty_waiting_and_ready_do_not_invent_work_or_handoff() {
    for phase in [Phase::Waiting, Phase::Ready] {
        let mut init = captured();
        init.phase = phase;
        init.signin_account = Some("Synthetic Hidden Name".into());
        let mut owner = SessionMachine::from_init(init.clone());
        let previous = owner.publication();
        assert!(command(&mut owner, Command::TakeReady).is_empty());
        assert!(Arc::ptr_eq(&previous, &owner.publication()));
        let after = owner.snapshot_init();
        assert_eq!(serde_json::to_value(&after).unwrap(), serde_json::to_value(&init).unwrap());
        assert_eq!(after.signin_account, init.signin_account);
        assert_eq!(owner.subhash(), init.hash());
    }
}
