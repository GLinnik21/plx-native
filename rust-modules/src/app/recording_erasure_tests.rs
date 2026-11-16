//! Real Session/Bridge erasure ACK and private files; unrelated host sweeps are substituted.
use super::*;
use crate::app::{bootstrap, recorder, run};
use plx_plex::plex::session::{self, Session};

#[test]
fn owned_recording_files_are_erased_after_quiescence_and_leftovers_are_acked() {
    let _serial = plx_base::testlock::serial();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    for partial in [false, true] {
        let temp = session::TempSession::new(if partial {
            "recording-erase-partial"
        } else {
            "recording-erase"
        });
        temp.assert_only_target();
        let root = temp.path().parent().unwrap().to_path_buf();
        let outside = root.join("unrelated-recording");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"synthetic sentinel").unwrap();
        let recording = root.join("plxnative-recordings");
        let sink = plx_ui::rec::DirSink::create(&recording.join("latest")).unwrap();
        let initial = bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
        let mut rec = recorder::Recplay::recording_with_sink(&initial, Box::new(sink)).unwrap();
        rec.tick(0, 0.0); // deliberately still buffered when the confirmed command arrives
        std::os::unix::fs::symlink(&outside, recording.join("external-link")).unwrap();
        std::os::unix::fs::symlink(outside.join("sentinel"), root.join("plxnative-app-init"))
            .unwrap();
        std::fs::write(root.join("plxnative-recplay"), outside.to_str().unwrap()).unwrap();
        if partial {
            std::fs::create_dir(root.join("plxnative-rec")).unwrap();
            std::fs::write(
                root.join("plxnative-rec/keep"),
                b"unexpected control directory",
            )
            .unwrap();
        } else {
            std::fs::write(root.join("plxnative-rec"), b"").unwrap();
        }

        let mut bridge = Bridge::for_session_test(plx_session::auth::SessionInit::captured(Session {
            client_id: "synthetic-erasure".into(),
            ..Default::default()
        }));
        rec.arm_landgate(bridge.landgate());
        bridge.session_adapter =
            super::super::adapters::session::SessionAdapter::live_recording_resources_for_test(
                &mt,
                root.clone(),
            );
        let mut pages = Dispatcher::<AppHost>::new();
        assert!(run::request_local_erasure(
            &mut rec,
            &mut bridge,
            &mut pages
        ));
        assert!(matches!(rec, recorder::Recplay::Off));
        assert!(
            recording.exists(),
            "quiescence precedes the queued Session resource effect"
        );
        assert!(
            temp.path().exists(),
            "enqueue does not perform Session erasure"
        );
        pages.frame_with(
            &mut bridge,
            Tick::default(),
            Vec::new(),
            Vec::new(),
            &mut rec,
            false,
        );
        bridge.settle_session_io_for_test(&mut pages);
        assert_eq!(bridge.auth_read().0.phase, plx_session::auth::Phase::Deleted);
        assert_eq!(bridge.auth_read().0.delete_leftovers, usize::from(partial));
        assert!(bridge
            .take_observed_loop_commands()
            .iter()
            .any(|req| matches!(req, LoopReq::LocalDataErased)));
        run::finish_local_erasure(&bridge, &mut pages);
        pages.frame_with(
            &mut bridge,
            Tick::default(),
            Vec::new(),
            Vec::new(),
            &mut rec,
            false,
        );
        assert!(
            matches!(pages.top_arg(), Some(AppArg::Login)),
            "real completion handler returns to login despite leftovers"
        );
        assert!(!temp.path().exists());
        assert!(!recording.exists());
        assert!(std::fs::symlink_metadata(root.join("plxnative-app-init")).is_err());
        assert!(!root.join("plxnative-recplay").exists());
        assert_eq!(
            std::fs::read(outside.join("sentinel")).unwrap(),
            b"synthetic sentinel"
        );
        rec.tick(1, 0.016);
        rec.end_frame(&|| 0);
        assert!(!rec.finish(plx_machine::landgate::fixture_gate()));
        assert!(
            !recording.exists(),
            "subsequent frames and shutdown cannot recreate erased data"
        );
        assert_eq!(
            plx_ui::rec::erase_owned_artifacts(&root, crate::app::input::remove_or_prove_absent).len(),
            usize::from(partial),
            "missing files are idempotent; an unremoved control directory remains reported"
        );
        session::ProfilePublisher::new(&mt).publish(None, 0, String::new());
        plx_plex::plex::reset_servers_for_test();
    }
}

#[test]
fn ordered_erasure_retires_the_single_owned_recorder_before_session_io() {
    let _serial = plx_base::testlock::serial();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let temp = session::TempSession::new("ordered-recording-erasure");
    temp.assert_only_target();
    let root = temp.path().parent().unwrap().to_path_buf();
    let recording = root.join("plxnative-recordings");
    let sink = plx_ui::rec::DirSink::create(&recording.join("latest")).unwrap();
    let initial = bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let mut rec = recorder::Recplay::recording_with_sink(&initial, Box::new(sink)).unwrap();
    rec.tick(0, 0.0);
    let mut boot_initial = Some(initial);
    let mut bridge = Bridge::for_session_test(plx_session::auth::SessionInit::captured(Session {
        client_id: "synthetic-ordered-erasure".into(), ..Default::default()
    }));
    rec.arm_landgate(bridge.landgate());
    bridge.session_adapter = super::super::adapters::session::SessionAdapter::live_recording_resources_for_test(&mt, root);
    let mut pages = Dispatcher::<AppHost>::new();
    pages.emit(MachineId::Nav, Fx::App(AppFx::Loop(LoopReq::DeleteAllLocalData)));
    let mut owned = super::test_support::TestCommandResources::new();
    let mut resources = owned.borrow();
    resources.rec = &mut rec;
    resources.boot_initial = &mut boot_initial;
    super::frame_with_commands(&mut pages, &mut bridge, &mut resources, Tick::default(), vec![], &mut plx_ui::dispatch::NoTap);
    assert!(matches!(rec, recorder::Recplay::Off));
    assert!(boot_initial.is_none());
    assert_eq!(bridge.screen_command_receipts[0].2, plx_machine::machine::AppCommandOutcome::Completed);
    bridge.settle_session_io_for_test(&mut pages);
    assert_eq!(bridge.auth_read().0.phase, plx_session::auth::Phase::Deleted);
    assert!(!recording.exists());
    assert!(!temp.path().exists());
    assert!(matches!(pages.top_arg(), Some(AppArg::Login)));
    session::ProfilePublisher::new(&mt).publish(None, 0, String::new());
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn replay_cannot_authorize_erasure_of_a_recording_target() {
    let _serial = plx_base::testlock::serial();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let temp = session::TempSession::new("replay-no-erasure");
    temp.assert_only_target();
    let before = std::fs::read(temp.path()).unwrap();
    let initial = bootstrap::Initial::synthetic_home(17, 32517, None).unwrap();
    let mut bridge = Bridge::controlled_home(|| 0, &initial, &mt, true);
    let mut pages = Dispatcher::<AppHost>::new();
    let mut rec = recorder::Recplay::Off;
    assert!(!run::request_local_erasure(
        &mut rec,
        &mut bridge,
        &mut pages
    ));
    assert!(std::fs::read(temp.path()).unwrap() == before);
    assert!(bridge.session.snapshot_init().pending_erase.is_none());
}
