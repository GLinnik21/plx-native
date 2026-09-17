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

/// **TV 2026-09-17: first-run consent and the who's-watching picker remount each other forever.**
///
/// Fresh install, consent never answered, QR sign-in onto a three-user Plex Home. The log then
/// alternated `coldopen screen=settings` / `coldopen screen=profiles` about once per 1–2 frames
/// for as long as the picker phase lasted. This drives the loop's own per-frame routing for
/// `auth::Phase::Profiles` (`run.rs`: `nav_root_if_unsettled(Profiles)`, then `maybe_ask_consent`
/// ONLY once Profiles is the settled top, parked AFTER the dispatcher frame exactly as the loop
/// parks them) over the real `Bridge` + `Dispatcher`.
///
/// The fix has two independent halves, both exercised here: `NavOp::Root` now truly replaces the
/// stack (so the never-retired Login root stops sitting under every later mint) and
/// `NavStack::request` drops an exactly-redundant `Root` before it touches `pending` or the
/// transition (so the per-frame re-ask cannot keep re-kicking a `PageDip` that never settles) —
/// and the follower asks for the consent surface only once Profiles has actually landed, never
/// while Login is still the entry the `Root` above is retiring.
///
/// Expected: the picker mounts once, the consent surface mounts once, and neither unmounts.
///
/// `hostsim`-only, and this is a correctness gate rather than a convenience: `paths::ENV_STEERABLE`
/// is `cfg!(feature = "hostsim")`, so `PLXNATIVE_RUNTIME_DIR` is only honoured in that build; off
/// it, `dev::any_trigger_present()` scans the literal shared `/tmp` regardless of the env var this
/// test sets, and `make check`'s first (non-`hostsim`) pass grades this test against whatever
/// stray `plxnative-*` file another process or an earlier run left there instead of a private root.
#[cfg(feature = "hostsim")]
#[test]
fn first_run_consent_over_the_picker_does_not_flip_mounts_every_frame() {
    use super::test_support::tick;
    let _g = crate::testlock::serial();
    let saved_consent = crate::telemetry::consent::current();
    crate::telemetry::consent::install(crate::telemetry::consent::Consent::default());
    // Any `plxnative-*` file in the runtime root suppresses the question (`dev::any_trigger_present`);
    // run under `--features hostsim` with a private `PLXNATIVE_RUNTIME_DIR`, as `make check` does.
    assert!(!crate::dev::any_trigger_present(), "a stray trigger in {:?} suppresses the consent question",
        crate::paths::runtime_dir());
    assert!(crate::dev::scenarios::consent_override().is_none());

    let mut account = stored(true);
    account.user = UserRef::default();
    account.home_users = ["owner", "adult", "kid"]
        .iter()
        .map(|u| HomeUserRef { uuid: (*u).into(), ..Default::default() })
        .collect();
    let mut rig = rig(account);
    // Production's dispatcher (`app/boot.rs`): the PageDip route transition, not the tests' cut.
    let mut d = Dispatcher::<AppHost>::with_transition(Box::new(
        crate::ui::containers::transition::PageDip::new(),
    ));

    // The QR screen was the root before the sign-in completed.
    super::nav_root(&mut d, AppArg::Login);
    for i in 0..20 { super::frame(&mut d, &mut rig, tick(i), vec![]); }
    assert_eq!(d.nav.tabs.stack.entries.len(), 1, "Login is the settled root before the sign-in lands");
    execute_session_command(&mut d, SessionCmd::StartSwitch(Picker::SignedIn));
    super::frame(&mut d, &mut rig, tick(20), vec![]);
    assert_eq!(rig.auth_read().0.phase, Phase::Profiles, "the sign-in raised the picker phase");

    let mut mounts: Vec<(u32, &'static str)> = Vec::new();
    // Only the picker/consent family: Login's own single, legitimate Unmount — the `Root` above
    // truly replacing it (item 1's fix) retires the never-answered-for QR root exactly once — is
    // a real navigation event, not a recurrence of the bug, so it is deliberately not counted
    // here (the doc above promises "neither [picker nor consent] unmounts", not "nothing does").
    let mut watched: std::collections::HashSet<InstanceId> = Default::default();
    let mut unmounts = 0usize;
    let mut cold: Vec<String> = Vec::new();
    for i in 21..=140u32 {
        // run.rs ~199/212: the dispatcher frame (it commits what the previous iteration parked).
        let (_, report) = super::frame(&mut d, &mut rig, tick(i), vec![]);
        for (id, name) in &report.mounted {
            mounts.push((i, *name));
            if matches!(*name, "profiles" | "settings") {
                watched.insert(*id);
            }
        }
        unmounts += report.unmounted.iter().filter(|id| watched.contains(id)).count();
        cold.extend(d.take_cold_open_lines());
        // run.rs ~1654-1722: the route follower, parked for the NEXT frame's commit.
        if matches!(d.top_arg(), Some(AppArg::Login | AppArg::Profiles)) {
            assert!(rig.take_session_ready().is_none());
            if matches!(rig.auth_read().0.phase, Phase::Profiles | Phase::Switching) {
                super::nav_root_if_unsettled(&mut d, AppArg::Profiles);
                // Only once Profiles is the SETTLED top — never while Login is still fading out
                // underneath it (see the fix's doc above).
                if super::top_settled_on(&d, &AppArg::Profiles) {
                    super::super::input::maybe_ask_consent(&mut d);
                }
            }
        }
    }

    if let Some(c) = saved_consent {
        crate::telemetry::consent::install(c);
    }
    // The first-run consent surface is a Settings-family screen: its `Screen::name()` is "settings".
    let named = |w: &str| mounts.iter().filter(|(_, n)| *n == w).count();
    assert_eq!(
        (named("profiles"), named("settings"), unmounts),
        (1, 1, 0),
        "(frame, screen) mounts {mounts:?}; {unmounts} unmounts; coldopen lines {cold:?}",
    );
    // The perpetual dip is invisible to the mount count above (the redundant `Root` used to keep
    // re-kicking `PageDip`'s Out ramp forever without ever re-minting anything) — 140 frames is
    // long past its 210 ms schedule, so anything short of 1.0 here means it is still cycling.
    assert_eq!(d.nav.tabs.stack.page_alpha(), 1.0, "the page transition actually settled");
}

/// **Login-phase twin: the same per-frame follower over the QR screen never re-mints it either.**
///
/// The picker test above exercises the `Phase::Profiles | Phase::Switching` arm of `run.rs`'s
/// per-frame follower (~1697); this exercises the OTHER arms behind the same
/// `if matches!(app.route(), AppArg::Login | AppArg::Profiles)` guard while auth sits in
/// `Phase::Waiting` (the QR flow, before any sign-in lands): the `persistence_warning` branch
/// (~1691) and the default `_ => nav_root_if_unsettled(Login)` arm (~1719/1722). Both are
/// per-frame `Root(Login)` followers of exactly the shape that broke on the Profiles side —
/// there is just no covering surface here to make a perpetual dip visible as a remount, so this
/// grades it directly off the mount/unmount counts and the settled alpha instead.
///
/// Expected: Login mounts exactly once, over the whole run, and never unmounts.
#[test]
fn login_phase_follower_settles_and_does_not_recycle_the_qr_screen() {
    use super::test_support::tick;
    let _g = crate::testlock::serial();

    let mut init = SessionInit::captured(Session::default());
    init.phase = Phase::Waiting;
    let mut rig = Bridge::for_session_test(init);
    // Production's dispatcher (`app/boot.rs`): the PageDip route transition, not the tests' cut.
    let mut d = Dispatcher::<AppHost>::with_transition(Box::new(
        crate::ui::containers::transition::PageDip::new(),
    ));

    // `app/boot.rs`: the QR screen is minted as the root once, before the loop's first iteration.
    super::nav_root(&mut d, AppArg::Login);

    let mut mounts: Vec<(u32, &'static str)> = Vec::new();
    let mut unmounts = 0usize;
    for i in 0..140u32 {
        // run.rs ~199/212: the dispatcher frame (it commits what the previous iteration parked).
        let (_, report) = super::frame(&mut d, &mut rig, tick(i), vec![]);
        mounts.extend(report.mounted.iter().map(|(_, n)| (i, *n)));
        unmounts += report.unmounted.len();
        // run.rs ~1654-1722: the route follower, parked for the NEXT frame's commit.
        if matches!(d.top_arg(), Some(AppArg::Login | AppArg::Profiles)) {
            assert!(rig.take_session_ready().is_none());
            if rig.auth_read().0.persistence_warning.is_some() {
                super::nav_root_if_unsettled(&mut d, AppArg::Login);
            } else {
                match rig.auth_read().0.phase {
                    Phase::Ready => {}
                    Phase::Profiles | Phase::Switching => {
                        unreachable!("Phase::Waiting never advances on its own in this fixture")
                    }
                    _ => super::nav_root_if_unsettled(&mut d, AppArg::Login),
                }
            }
        }
    }

    let named = |w: &str| mounts.iter().filter(|(_, n)| *n == w).count();
    assert_eq!(
        (named("login"), unmounts),
        (1, 0),
        "(frame, screen) mounts {mounts:?}; {unmounts} unmounts",
    );
    assert_eq!(d.nav.tabs.stack.page_alpha(), 1.0, "the page transition actually settled");
}
