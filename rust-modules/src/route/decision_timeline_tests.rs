//! Machine-id cache scoping and playback timeline/scrobble lease tests.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;

/// The `/identity` cache is keyed to the server that taught it. It feeds
/// `uri=server://{machineIdentifier}/…` on the PlayQueue POST, so one cached globally is
/// server A's fingerprint sent to server B — naming a machine B has never heard of, on a POST
/// that is best-effort and therefore fails silently.
#[test]
fn the_machine_id_cache_is_scoped_to_the_server_that_taught_it() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let a = ServerId::from_raw((crate::plex::MAX_SERVERS - 2) as u16);
    let b = ServerId::from_raw((crate::plex::MAX_SERVERS - 1) as u16);
    assert!(crate::plex::client_for(a).is_none() && crate::plex::client_for(b).is_none());

    apply_plan(&mut ps, 
        Plan {
            sid: a,
            machine_id: "MACHINE-A".into(),
            ..Default::default()
        },
        "rk-a",
    );
    assert_eq!(
        ResolveEnv::snapshot(&ps, crate::metadata::MetadataView::new(), a, "rk-a").machine_id,
        "MACHINE-A",
        "its own server reuses it"
    );
    assert_eq!(
        ResolveEnv::snapshot(&ps, crate::metadata::MetadataView::new(), b, "rk-b").machine_id,
        "",
        "another server must re-ask rather than inherit A's fingerprint"
    );

    // …and an empty `machine_id` means "leave the cache alone", not "the cache is now B's"
    apply_plan(&mut ps, 
        Plan {
            sid: b,
            ..Default::default()
        },
        "rk-b",
    );
    assert_eq!(ResolveEnv::snapshot(&ps, crate::metadata::MetadataView::new(), a, "rk-a").machine_id, "MACHINE-A");
    assert_eq!(ResolveEnv::snapshot(&ps, crate::metadata::MetadataView::new(), b, "rk-b").machine_id, "");
}

/// **The one that had to ship in the same commit as `cur_sid`.** The `/:/timeline` report runs
/// on a worker every ten seconds and used to read the current server fresh on each tick — the
/// only place in the playback path with no capture at all. Split from the rest, the app
/// resolves correctly, plays correctly, and quietly writes the resume point of a friend's film
/// onto your own server for as long as it plays.
///
/// Two servers on loopback, the item playing from B, the user browsing A: the POST must land on
/// B. The closing report to A is the control — it proves the two stubs are distinguishable, so
/// "A heard nothing" is a fact about the routing and not about a listener that never worked.
#[test]
#[cfg(feature = "devtriggers")]
fn the_timeline_reaches_the_server_the_item_came_from_not_the_current_one() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::time::Duration;
    let _g = fresh_registry(&mut ps);
    let (pa, rx_a, ha) = stub_pms();
    let (pb, rx_b, hb) = stub_pms();
    // `register_for_test`, not the public `register`: the latter resolves the device id through
    // `session::load`, which mints and PERSISTS a uuid on a host that has no session file.
    let a = crate::plex::register_for_test(
        "route-test-A",
        "127.0.0.1",
        pa,
        "tok-a",
        "cid-route-test",
    );
    let b = crate::plex::register_for_test(
        "route-test-B",
        "127.0.0.1",
        pb,
        "tok-b",
        "cid-route-test",
    );
    assert_ne!(a, b, "two servers, two slots");

    // an item from B starts playing, then the user walks back to their OWN server's Home
    apply_plan(&mut ps, 
        Plan {
            sid: b,
            url: "https://example.invalid/b.mkv".into(),
            sess: "timeline-session-b".into(),
            ..Default::default()
        },
        "rk-b",
    );
    assert!(crate::plex::set_current(a));
    assert_eq!(
        cur_sid(&ps),
        b,
        "what is PLAYING does not move when the browsed server does"
    );

    let lease_b = begin_timeline_reporting(&ps).expect("B timeline lease");
    assert!(report_timeline(
        &lease_b,
        crate::plex::TimelineState::Playing,
        1_000,
        2_000,
    ));
    let got = rx_b
        .recv_timeout(Duration::from_secs(5))
        .expect("B never received the report");
    assert!(
        got.contains("ratingKey=rk-b"),
        "B got something else: {got}"
    );
    assert!(
        rx_a.recv_timeout(Duration::from_millis(300)).is_err(),
        "the current server must not receive another server's progress"
    );

    // control: a complete A projection reaches A, so the assertion above is about routing.
    apply_plan(&mut ps, 
        Plan {
            sid: a,
            url: "https://example.invalid/a.mkv".into(),
            sess: "timeline-session-a".into(),
            ..Default::default()
        },
        "rk-a",
    );
    let lease_a = begin_timeline_reporting(&ps).expect("A timeline lease");
    assert!(report_timeline(
        &lease_a,
        crate::plex::TimelineState::Stopped,
        0,
        2_000,
    ));
    let got = rx_a
        .recv_timeout(Duration::from_secs(5))
        .expect("A never received its own report");
    assert!(
        got.contains("ratingKey=rk-a"),
        "A got something else: {got}"
    );

    ha.join().unwrap();
    hb.join().unwrap();
    // Hand the table back empty. Both stubs' ports close as this returns, so anything left
    // registered is a client that answers nothing — and `CURRENT` still points at one of them.
    // The session is idled with it for the same reason, one level up: it is still holding `b`
    // as the playing server, i.e. a `ServerId` into the table being emptied.
    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
}

#[test]
#[cfg(feature = "devtriggers")]
fn replacement_timeline_waits_for_the_announced_old_stop_boundary() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    use std::time::Duration;

    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    let (order_tx, order_rx) = std::sync::mpsc::channel();
    let (old_port, old_server) = ordered_stub_pms("old", order_tx.clone());
    let (new_port, new_server) = ordered_stub_pms("new", order_tx);
    let old_sid = crate::plex::register_for_test(
        "timeline-stop-old",
        "127.0.0.1",
        old_port,
        "old-token",
        "timeline-client",
    );
    let new_sid = crate::plex::register_for_test(
        "timeline-stop-new",
        "127.0.0.1",
        new_port,
        "new-token",
        "timeline-client",
    );
    apply_plan(&mut ps, 
        Plan {
            sid: old_sid,
            sess: "logical-old".into(),
            ..Default::default()
        },
        "rk-old-stop",
    );
    // The test isolates timeline ordering; avoid adding a second transcode-stop request to the
    // one-shot old PMS after its stopped report.
    install_active_encoder("");

    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    struct ReleaseOnDrop(Option<std::sync::mpsc::Sender<()>>);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            if let Some(tx) = self.0.take() {
                let _ = tx.send(());
            }
        }
    }
    let mut release = ReleaseOnDrop(Some(release_tx));
    let old_reporter = std::thread::spawn(move || {
        let _ = release_rx.recv();
    });
    scrobble_stop(&mut ps, 
        Some(("rk-old-stop".into(), 11_000, 20_000)),
        Some(old_reporter),
    );

    apply_plan(&mut ps, 
        Plan {
            sid: new_sid,
            url: "https://example.invalid/new.mkv".into(),
            sess: "logical-new".into(),
            ..Default::default()
        },
        "rk-new-playing",
    );
    let lease = begin_timeline_reporting(&ps).expect("replacement reporter lease");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let replacement = std::thread::spawn(move || {
        let sent = report_timeline(&lease, crate::plex::TimelineState::Playing, 1_000, 20_000);
        let _ = done_tx.send(sent);
    });

    assert!(
        order_rx.recv_timeout(Duration::from_millis(250)).is_err(),
        "replacement playing escaped before the old reporter/stop boundary",
    );
    release.0.take().unwrap().send(()).unwrap();
    let (first_label, old) = order_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("old stopped report never reached PMS");
    assert_eq!(
        first_label, "old",
        "replacement report arrived before stopped"
    );
    assert!(
        old.contains("ratingKey=rk-old-stop"),
        "wrong old report: {old}"
    );
    assert!(
        old.contains("state=stopped"),
        "old report was not stopped: {old}"
    );
    let (second_label, new) = order_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("replacement playing report never reached PMS");
    assert_eq!(second_label, "new", "old server received an extra request");
    assert!(
        new.contains("ratingKey=rk-new-playing"),
        "wrong new report: {new}"
    );
    assert!(
        new.contains("state=playing"),
        "new report was not playing: {new}"
    );
    assert_eq!(done_rx.recv_timeout(Duration::from_secs(1)), Ok(true));

    replacement.join().unwrap();
    drain_scrobble();
    old_server.join().unwrap();
    new_server.join().unwrap();
    crate::plex::reset_servers_for_test();
    reset_session(&mut ps);
    install_active_encoder("");
    reset_player_control_for_test(&ps);
}

#[test]
fn every_concurrent_scrobble_drain_waits_for_the_same_taken_handle() {
    use std::time::Duration;

    let join = std::sync::Arc::new(ScrobbleJoin::new());
    let generation = join.reserve();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ = release_rx.recv();
    });
    join.install(generation, worker);

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let first_join = join.clone();
    let first_done = done_tx.clone();
    let first = std::thread::spawn(move || {
        first_join.drain();
        let _ = first_done.send(1);
    });
    let second_join = join.clone();
    let second = std::thread::spawn(move || {
        second_join.drain();
        let _ = done_tx.send(2);
    });

    assert!(
        done_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "a drainer escaped after another thread took the JoinHandle",
    );
    release_tx.send(()).unwrap();
    let mut completed = [
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
    ];
    completed.sort_unstable();
    assert_eq!(completed, [1, 2]);
    first.join().unwrap();
    second.join().unwrap();
}

#[test]
fn timeline_lease_cannot_cross_engine_teardown() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = crate::testlock::serial();
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
    apply_plan(&mut ps, 
        Plan {
            sid: ServerId::from_raw(0),
            url: "https://example.invalid/old.mkv".into(),
            sess: "logical-old".into(),
            pq_id: "pq-old".into(),
            pq_item_id: "pqi-old".into(),
            audio_sid: 7,
            sub_sid: 9,
            ..Default::default()
        },
        "rk-old",
    );
    install_active_encoder("wire-old");
    let old = begin_timeline_reporting(&ps).expect("old reporter");
    let before = timeline_snapshot(&old, crate::plex::TimelineState::Playing, 1_000, 2_000)
        .expect("old projection");
    assert_eq!(before.rating_key, "rk-old");
    assert_eq!(before.session, "wire-old");
    assert_eq!((before.audio_stream_id, before.subtitle_stream_id), (7, 9));

    begin_engine_teardown(true);
    assert!(
        timeline_snapshot(&old, crate::plex::TimelineState::Playing, 1_500, 2_000).is_none(),
        "an old reporter must not sample any field after its Engine is retired"
    );
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
}
