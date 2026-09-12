//! Picker regression test slice; production and old-test removal remain core-owned.
//! Use the existing Bridge/owner/dispatcher and explicit resource fixtures; no global controller.

use super::*;
use crate::auth::owner::ReplyTo;
use crate::auth::{Phase, Picker, SessionCmd, SessionInit};
use crate::plex::session::{self, HomeUserRef, ServerRef, Session, SourceRef, UserRef};

fn stored(protected: bool) -> Session {
    let uuid = if protected { "adult" } else { "kid" };
    Session {
        client_id: "synthetic-picker-client".into(),
        account_token: "synthetic-owner-token".into(),
        server: ServerRef {
            machine_id: "synthetic-picker-server".into(),
            address: "127.0.0.1".into(),
            port: 32400,
            origin_url: "http://127.0.0.1:32400".into(),
            token: "synthetic-server-token".into(),
            ..Default::default()
        },
        user: UserRef {
            uuid: uuid.into(),
            token: "synthetic-profile-token".into(),
            ..Default::default()
        },
        sources: vec![SourceRef {
            machine_id: "synthetic-picker-server".into(),
            address: "127.0.0.1".into(),
            port: 32400,
            origin_url: "http://127.0.0.1:32400".into(),
            token: "synthetic-server-token".into(),
            owned: true,
            ..Default::default()
        }],
        home_users: vec![HomeUserRef {
            uuid: uuid.into(),
            protected,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn rig(session: Session) -> Bridge {
    let mut init = SessionInit::captured(session);
    init.epoch = u64::from(u32::MAX) + 91;
    Bridge::for_session_test(init)
}

fn command(rig: &mut Bridge, d: &mut Dispatcher<AppHost>, cmd: SessionCmd) {
    execute_session_command(d, cmd);
    d.frame_with(
        rig,
        Tick::default(),
        Vec::new(),
        Vec::new(),
        &mut NoTap,
        false,
    );
}

fn back() -> SessionCmd {
    SessionCmd::BackAtRoot {
        reply: ReplyTo {
            instance: 81,
            correlation: 1,
        },
    }
}

fn refused_back(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) {
    assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
    assert!(!rig.session.snapshot_init().apply_pending);
    assert!(rig.take_session_ready().is_none());
    let before = rig.session.subhash();
    let publication = rig.session.publication();
    let resources = {
        let r = rig.session_adapter.fixture_resources();
        assert!(
            r.root_press_available,
            "BACK must reach the owner, not a cooldown refusal"
        );
        serde_json::json!({"disk": r.disk, "registry": r.registry_writes})
    };
    command(rig, d, back());
    assert_eq!(
        rig.session_adapter.fixture_resources().back_results,
        [false]
    );
    assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
    assert!(!rig.session.snapshot_init().apply_pending);
    assert_eq!(rig.session.subhash(), before);
    assert!(std::sync::Arc::ptr_eq(
        &publication,
        &rig.session.publication()
    ));
    let r = rig.session_adapter.fixture_resources();
    assert_eq!(
        serde_json::json!({"disk": r.disk, "registry": r.registry_writes}),
        resources
    );
    command(rig, d, SessionCmd::TakeReady);
    assert!(
        rig.take_session_ready().is_none(),
        "refusal cannot arm credentials for handoff"
    );
}

#[test]
fn back_out_of_the_boot_picker_refuses_a_pin_protected_profile_and_nothing_else() {
    // Pure may_resume/default/reason-string assertions remain in auth.rs's legacy bodies;
    // these are actual owner commands and resource handoffs, not evidence about log output.
    for protected in [true, false] {
        let saved = stored(protected);
        let mut rig = rig(saved.clone());
        let mut d = Dispatcher::<AppHost>::new();
        command(&mut rig, &mut d, SessionCmd::StartSwitch(Picker::Boot));
        assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
        if protected {
            refused_back(&mut rig, &mut d);
        } else {
            assert!(rig.session_adapter.fixture_resources().root_press_available);
            command(&mut rig, &mut d, back());
            assert_eq!(rig.session_adapter.fixture_resources().back_results, [true]);
            assert_eq!(rig.auth_read().0.phase, Phase::Ready);
            assert!(rig.session.snapshot_init().apply_pending);
            assert!(
                rig.take_session_ready().is_none(),
                "TakeReady owns the resource handoff"
            );
            command(&mut rig, &mut d, SessionCmd::TakeReady);
            let ready = rig.take_session_ready().expect("usable one-shot credentials");
            assert_eq!(ready.token, saved.pms_token());
            assert_eq!(ready.origin.host(), "127.0.0.1");
            assert_eq!(ready.origin.port(), 32400);
            assert!(rig.take_session_ready().is_none());
            assert!(!rig.session.snapshot_init().apply_pending);
            let resources = rig.session_adapter.fixture_resources();
            assert_eq!(resources.disk.user.uuid, saved.user.uuid);
            assert_eq!(
                resources
                    .profile
                    .as_ref()
                    .unwrap()
                    .profile
                    .as_ref()
                    .unwrap()
                    .uuid,
                saved.user.uuid
            );
            assert!(!resources.registry_writes.is_empty());
        }
    }

    // The old no-session case starts with an already raised picker. StartSwitch on a signed-out
    // owner instead correctly fails sign-in; construct that initial screen cut only once here.
    let mut init = SessionInit::captured(Session::default());
    init.phase = Phase::Profiles;
    init.picker = Picker::ChangeProfile;
    let mut empty = Bridge::for_session_test(init);
    refused_back(&mut empty, &mut Dispatcher::<AppHost>::new());

    for picker in [Picker::Boot, Picker::SignedIn] {
        let mut unchosen = stored(true);
        unchosen.user = UserRef::default();
        assert!(
            !unchosen.pms_token().is_empty(),
            "abandoned sign-in still holds owner credentials"
        );
        let mut rig = rig(unchosen);
        let mut d = Dispatcher::<AppHost>::new();
        command(&mut rig, &mut d, SessionCmd::StartSwitch(picker));
        refused_back(&mut rig, &mut d);
    }
}

struct ResourceCleanup<'a>(&'a crate::task::MainThread);
impl Drop for ResourceCleanup<'_> {
    fn drop(&mut self) {
        crate::plex::reset_servers_for_test();
        session::ProfilePublisher::new(self.0).publish(None, 0);
    }
}

fn live_detachment() {
    let _lock = crate::testlock::serial();
    let mt = unsafe { crate::task::MainThread::assume() };
    let tmp = session::TempSession::new("picker-owner-detachment");
    let _cleanup = ResourceCleanup(&mt);
    tmp.assert_only_target();
    crate::plex::reset_servers_for_test();
    let saved = stored(true);
    session::save(&saved);
    let disk = std::fs::read(tmp.path()).unwrap();
    let mut rig = rig(saved);
    rig.session_adapter =
        super::super::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
    let mut d = Dispatcher::<AppHost>::new();
    command(&mut rig, &mut d, SessionCmd::ResumeStored);
    assert_eq!(rig.auth_read().0.phase, Phase::Ready);
    assert!(rig.take_session_ready().is_some());
    assert!(rig.take_session_ready().is_none());
    assert_eq!(session::current().unwrap().uuid, "adult");
    assert_eq!(
        session::current_gen(),
        rig.session.snapshot_init().profile_scope.0
    );
    command(
        &mut rig,
        &mut d,
        SessionCmd::StartSwitch(Picker::ChangeProfile),
    );
    assert_eq!(rig.auth_read().0.phase, Phase::Profiles);
    assert!(rig.session.snapshot_init().active_profile.is_none());
    assert!(
        session::current().is_none(),
        "real publication detaches before any BACK"
    );
    assert_eq!(
        session::current_gen(),
        rig.session.snapshot_init().profile_scope.0
    );
    assert!(!rig.session.snapshot_init().apply_pending);
    assert!(rig.take_session_ready().is_none());
    assert_eq!(
        std::fs::read(tmp.path()).unwrap(),
        disk,
        "detachment is not credential deletion"
    );
    tmp.assert_only_target();
    // No live BACK: that would invoke the platform Home adapter. The independent fixture traces
    // below exercise BACK and its actual reply; this trace proves the native publication cut.
    // Auxiliary roster work is refused by this frozen adapter; no worker/network is launched.
}

#[test]
fn change_profile_then_back_cannot_restore_the_protected_profile_it_left() {
    for protected in [true, false] {
        let saved = stored(protected);
        assert_eq!(saved.active_profile_is_protected(), protected);
        let mut rig = rig(saved.clone());
        let mut d = Dispatcher::<AppHost>::new();
        command(&mut rig, &mut d, SessionCmd::ResumeStored);
        assert_eq!(rig.auth_read().0.phase, Phase::Ready);
        assert!(rig.take_session_ready().is_some());
        assert!(rig.take_session_ready().is_none());
        assert_eq!(
            rig.session
                .snapshot_init()
                .active_profile
                .as_ref()
                .unwrap()
                .uuid,
            saved.user.uuid
        );
        assert_eq!(
            rig.session_adapter
                .fixture_resources()
                .profile
                .as_ref()
                .unwrap()
                .profile
                .as_ref()
                .unwrap()
                .uuid,
            saved.user.uuid
        );
        command(
            &mut rig,
            &mut d,
            SessionCmd::StartSwitch(Picker::ChangeProfile),
        );
        assert!(rig.session.snapshot_init().active_profile.is_none());
        assert!(rig.session.publication().profile.is_none());
        assert!(rig
            .session_adapter
            .fixture_resources()
            .profile
            .as_ref()
            .unwrap()
            .profile
            .is_none());
        refused_back(&mut rig, &mut d);
        assert!(rig
            .session_adapter
            .fixture_resources()
            .profile
            .as_ref()
            .unwrap()
            .profile
            .is_none());
    }
    live_detachment();
}
