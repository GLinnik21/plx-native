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
    let a = ServerId::from_raw(40);
    let b = ServerId::from_raw(41);
    assert!(plx_plex::plex::client_for(a).is_none() && plx_plex::plex::client_for(b).is_none());

    apply_plan(&mut ps, 
        Plan {
            sid: a,
            machine_id: "MACHINE-A".into(),
            ..Default::default()
        },
        "rk-a",
    );
    assert_eq!(
        ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), a, "rk-a").machine_id,
        "MACHINE-A",
        "its own server reuses it"
    );
    assert_eq!(
        ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), b, "rk-b").machine_id,
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
    assert_eq!(ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), a, "rk-a").machine_id, "MACHINE-A");
    assert_eq!(ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), b, "rk-b").machine_id, "");
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
    let a = plx_plex::plex::register_for_test(
        "route-test-A",
        "127.0.0.1",
        pa,
        "tok-a",
        "cid-route-test",
    );
    let b = plx_plex::plex::register_for_test(
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
    assert!(plx_plex::plex::set_current(a));
    assert_eq!(
        cur_sid(&ps),
        b,
        "what is PLAYING does not move when the browsed server does"
    );

    let lease_b = begin_timeline_reporting(&ps).expect("B timeline lease");
    assert!(report_timeline(
        &lease_b,
        plx_plex::plex::TimelineState::Playing,
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
        plx_plex::plex::TimelineState::Stopped,
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
    plx_plex::plex::reset_servers_for_test();
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
    let old_sid = plx_plex::plex::register_for_test(
        "timeline-stop-old",
        "127.0.0.1",
        old_port,
        "old-token",
        "timeline-client",
    );
    let new_sid = plx_plex::plex::register_for_test(
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
        let sent = report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 1_000, 20_000);
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
    plx_plex::plex::reset_servers_for_test();
    reset_session(&mut ps);
    install_active_encoder("");
    reset_player_control_for_test(&ps);
}

/// The refetch gate waits for the resume point to be committed, not for the whole stop worker.
/// The worker posts `stopped`, THEN asks the server to stop the encoder (a second round trip that
/// a slow PMS holds for ~17 s). `timeline_stop_landed` must read true between the two while
/// `scrobble_settled` (the whole worker) still reads false.
#[test]
fn timeline_stop_landed_reads_true_once_stopped_was_posted_while_the_encoder_stop_is_held() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::mpsc;
    use std::time::Duration;

    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);

    // One connection per request, in order: the 1st (the `stopped` timeline) waits on `gate_stopped`,
    // the 2nd (the encoder stop) on `gate_encoder`. Every request line is reported on `seen`.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    let (seen_tx, seen_rx) = mpsc::channel::<String>();
    let (gate_stopped_tx, gate_stopped_rx) = mpsc::channel::<()>();
    let (gate_encoder_tx, gate_encoder_rx) = mpsc::channel::<()>();
    let server = std::thread::spawn(move || {
        for gate in [gate_stopped_rx, gate_encoder_rx] {
            let Some(Ok(mut socket)) = listener.incoming().next() else { return };
            let mut line = String::new();
            let _ = BufReader::new(&socket).read_line(&mut line);
            let _ = seen_tx.send(line);
            let _ = gate.recv();
            let _ = socket.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let sid = plx_plex::plex::register_for_test(
        "timeline-stop-landed",
        "127.0.0.1",
        port,
        "tok",
        "timeline-client",
    );
    apply_plan(
        &mut ps,
        Plan {
            sid,
            sess: "logical-landed".into(),
            ..Default::default()
        },
        "rk-landed",
    );
    install_active_encoder("wire-landed");

    scrobble_stop(&mut ps, Some(("rk-landed".into(), 11_000, 20_000)), None);
    let first = seen_rx.recv_timeout(Duration::from_secs(5)).expect("no stopped POST");
    assert!(first.contains("state=stopped"), "first request was not stopped: {first}");
    assert!(
        !timeline_stop_landed() && !scrobble_settled(),
        "premise: the stop is announced and the worker has not finished"
    );

    gate_stopped_tx.send(()).unwrap();
    let second = seen_rx.recv_timeout(Duration::from_secs(5)).expect("no encoder stop");
    assert!(second.contains("/transcode/universal/stop"), "second request: {second}");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !timeline_stop_landed() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        timeline_stop_landed(),
        "the stopped POST was answered, so the resume point is committed"
    );
    assert!(
        !scrobble_settled(),
        "the worker is still inside the held encoder stop"
    );

    gate_encoder_tx.send(()).unwrap();
    drain_scrobble();
    assert!(scrobble_settled() && timeline_stop_landed());
    server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
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
    let _g = plx_base::testlock::serial();
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
    apply_plan(&mut ps, 
        Plan {
            sid: ServerId::from_raw(0),
            url: "https://example.invalid/old.mkv".into(),
            sess: "logical-old".into(),
            pq_id: "pq-old".into(),
            pq_item_id: "pqi-old".into(),
            audio: Some(CarriedAudio::named(7, -1)),
            sub_sid: 9,
            ..Default::default()
        },
        "rk-old",
    );
    install_active_encoder("wire-old");
    let old = begin_timeline_reporting(&ps).expect("old reporter");
    let before = timeline_snapshot(&old, plx_plex::plex::TimelineState::Playing, 1_000, 2_000)
        .expect("old projection");
    assert_eq!(before.rating_key, "rk-old");
    assert_eq!(before.session, "wire-old");
    assert_eq!((before.audio_stream_id, before.subtitle_stream_id), (7, 9));

    begin_engine_teardown(true);
    assert!(
        timeline_snapshot(&old, plx_plex::plex::TimelineState::Playing, 1_500, 2_000).is_none(),
        "an old reporter must not sample any field after its Engine is retired"
    );
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
}

/// Field report, 0.7.0 prep: an autoplayed trailer whose Original open failed was re-opened as an
/// HLS transcode, and that engine posted `timeline playing t=…/178s` every ten seconds — watch
/// state written to the viewer's account by a trailer they never chose to play. The failure path
/// had cleared the session's preview flag while the abandoned Load still held the engine, so the
/// reload that followed started a reporter. What a session may write is a property of the REQUEST
/// that produced it, and no teardown bookkeeping may turn a preview into a playback.
#[test]
fn a_session_resolved_for_a_preview_never_reports_a_timeline() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = plx_base::testlock::serial();
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
    ps.request = Some(PlaybackRequest {
        sid: ServerId::from_raw(0),
        rk: "trailer".into(),
        part: "/library/parts/1/file.mp4".into(),
        vcodec: "h264".into(),
        acodec: "aac".into(),
        title: "Trailer".into(),
        ctx: plx_data::metadata::TRAILER_CONTEXT.into(),
        preview: true,
        version: None,
        show_rk: String::new(),
    });
    apply_plan(
        &mut ps,
        Plan {
            sid: ServerId::from_raw(0),
            url: "https://example.invalid/trailer.mp4".into(),
            sess: "logical-trailer".into(),
            ..Default::default()
        },
        "trailer",
    );
    assert!(is_preview(&ps));
    assert!(preview_request(&ps));
    assert!(begin_timeline_reporting(&ps).is_none(), "a preview has no timeline");
    clear_preview(&mut ps);
    assert!(preview_request(&ps), "clearing the preview flag must not make it a playback");
    assert!(
        begin_timeline_reporting(&ps).is_none(),
        "a session resolved for a preview must never report a timeline"
    );
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
}

/// **A film pressed after a trailer is a film from the press on.** `resolved_as_preview` is fixed
/// at a landing, so a film whose resolve never lands (a refused worker spawn, a superseded
/// request) left the PREVIOUS trailer's flag standing: the failure read-out of the film was then
/// reported as a preview, with the preview's origin, fingerprint and flags. The request itself
/// retires the flag.
#[test]
fn a_film_request_retires_the_previous_trailers_preview_mark() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    resolve_as_preview_for_test(&mut ps);
    assert!(preview_request(&ps), "the trailer's session is marked");

    assert!(request_play(
        &mut ps, &mut meta, plx_platform::devcaps::caps(), ServerId::from_raw(0), "film", "/library/parts/2/file.mkv", "h264", "aac", "Film", "", None, "",
    ));
    assert!(!preview_request(&ps), "a film request is not a preview, whatever the last landing was");

    cancel_play(&mut ps);
    let _ = wait_resolve_workers_for_test(std::time::Duration::from_secs(10));
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
}

/// The record a stop leaves for the app: position from the final report, parentage and the
/// "was it watched when the play began" from the item's play-queue row (fetched at play start), and
/// `was_watched = true` when no row can be found (an unknown must never fan a scrobble out). With no
/// client the POST cannot be taken, and the record still arrives, as not taken.
#[test]
fn a_stop_leaves_a_record_with_the_queue_rows_parentage_and_prior_watched_state() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    let _ = take_landed_stops();
    ps.queue = vec![plx_plex::plex::QueueRow {
        item_id: 11, rk: "7".into(), show_rk: "S".into(), season: 2, watched: false, ..Default::default()
    }];
    ps.pq_item_id = "11".into();
    scrobble_stop(&mut ps, Some(("7".into(), 30_000, 60_000)), None);
    drain_scrobble();
    ps.pq_item_id = "99".into();
    scrobble_stop(&mut ps, Some(("8".into(), 1_000, 60_000)), None);
    drain_scrobble();
    let stops = take_landed_stops();
    assert_eq!(stops.len(), 2, "both stops arrive, oldest first");
    let first = &stops[0];
    assert_eq!(first.rk.as_str(), "7");
    assert_eq!((first.pos_ms, first.dur_ms, first.ok, first.was_watched), (30_000, 60_000, false, false));
    assert!(stops[1].was_watched, "no queue row for the item: read as watched, never fanned out");
    assert!(!stops_owed());
    reset_player_control_for_test(&ps);
}

/// A loopback PMS that answers every request with an empty 200 and keeps each request line, in the
/// order they arrived: what a stop's worker sent, as the server saw it.
struct RecordingServer {
    port: i32,
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl RecordingServer {
    fn start() -> Self {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().unwrap().port() as i32;
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let done = Arc::new(AtomicBool::new(false));
        let thread = {
            let (seen, done) = (seen.clone(), done.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut socket, _)) => {
                            let _ = socket.set_nonblocking(false);
                            let _ = socket.set_read_timeout(Some(Duration::from_secs(5)));
                            let mut line = String::new();
                            let _ = BufReader::new(&socket).read_line(&mut line);
                            seen.lock().unwrap().push(line);
                            let _ = socket.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            );
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(2)),
                    }
                }
            })
        };
        Self { port, seen, done, thread }
    }

    /// Every request line so far.
    fn lines(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn stop(self) {
        self.done.store(true, std::sync::atomic::Ordering::SeqCst);
        self.thread.join().unwrap();
    }
}

/// **A stop that finished the item marks it watched, after its `stopped` report.** A server only
/// marks an item played itself when the stop lands past its own threshold (PMS's is about 90 %),
/// and long credits start before that, so Up Next and Skip on a `final` credits marker set
/// [`PlaybackSession::finished`] and a stop BELOW the threshold scrobbles the item once the
/// `stopped` POST is in. The record the app learns the stop from then reads as the end of the
/// item, before its read-back says the server's. The flag is taken by the stop that reads it: the
/// next stop, which the viewer did not finish, posts `stopped` alone.
#[test]
fn a_stop_that_finished_the_item_scrobbles_it_after_the_stopped_report() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    let _ = take_landed_stops();

    // Every request line a stop's worker sends, in order. The server answers each with an empty 200.
    let server = RecordingServer::start();
    let sid = plx_plex::plex::register_for_test("finished-stop", "127.0.0.1", server.port, "tok", "finished-client");
    apply_plan(
        &mut ps,
        Plan {
            sid,
            sess: "logical-finished".into(),
            ..Default::default()
        },
        "7",
    );

    // The viewer left item 7 for the next episode at 30 s of 60 s.
    ps.finished = true;
    scrobble_stop(&mut ps, Some(("7".into(), 30_000, 60_000)), None);
    drain_scrobble();
    assert!(!ps.finished, "the stop took the flag, so it cannot mark the next item");
    {
        let lines = server.lines();
        assert_eq!(lines.len(), 2, "the stopped report and the scrobble, nothing else: {lines:?}");
        assert!(lines[0].contains("state=stopped"), "`stopped` goes first: {}", lines[0]);
        assert!(lines[1].contains("/:/scrobble?key=7"), "then the scrobble: {}", lines[1]);
    }
    let stops = take_landed_stops();
    assert_eq!(stops.len(), 1);
    assert_eq!(
        (stops[0].pos_ms, stops[0].dur_ms, stops[0].ok),
        (60_000, 60_000, true),
        "the app is told the item ended, not where the viewer stopped"
    );

    // The next stop was not a finish: `stopped` alone, and the position is the viewer's.
    scrobble_stop(&mut ps, Some(("8".into(), 30_000, 60_000)), None);
    drain_scrobble();
    {
        let lines = server.lines();
        assert_eq!(lines.len(), 3, "only the `stopped` report: {lines:?}");
        assert!(lines[2].contains("state=stopped"), "{}", lines[2]);
    }
    let stops = take_landed_stops();
    assert_eq!((stops[0].pos_ms, stops[0].dur_ms), (30_000, 60_000));

    server.stop();
    plx_plex::plex::reset_servers_for_test();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
}

/// **A stop past the server's own threshold is not scrobbled a second time.** Up Next only appears
/// at the final credits marker, and the end of the stream and the countdown's expiry reach it too,
/// so a stop with `finished` set often lands past ~90 %: the server has already counted the view
/// from that report, and `/:/scrobble` would add one more (`docs/pms-api.md` §6, "Measured live
/// 2026-10-10"). Such a stop posts `stopped` alone, tells the app where the viewer stopped, and
/// still takes the flag.
#[test]
fn a_finishing_stop_past_the_servers_threshold_posts_stopped_alone() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    let _ = take_landed_stops();

    let server = RecordingServer::start();
    let sid = plx_plex::plex::register_for_test("finished-late", "127.0.0.1", server.port, "tok", "finished-late-client");
    apply_plan(
        &mut ps,
        Plan {
            sid,
            sess: "logical-finished-late".into(),
            ..Default::default()
        },
        "7",
    );

    // 59 s of 60 s is past 90 %: the server marks item 7 played from this stop itself.
    ps.finished = true;
    scrobble_stop(&mut ps, Some(("7".into(), 59_000, 60_000)), None);
    drain_scrobble();
    assert!(!ps.finished, "the stop took the flag even though it did not scrobble");
    {
        let lines = server.lines();
        assert_eq!(lines.len(), 1, "only the `stopped` report, no second view: {lines:?}");
        assert!(lines[0].contains("state=stopped"), "{}", lines[0]);
    }
    let stops = take_landed_stops();
    assert_eq!(stops.len(), 1);
    assert_eq!(
        (stops[0].pos_ms, stops[0].dur_ms, stops[0].ok),
        (59_000, 60_000, true),
        "the record keeps the viewer's position; the server's own threshold made it watched"
    );

    server.stop();
    plx_plex::plex::reset_servers_for_test();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
}

/// A `finished` flag no stop took (Up Next with nothing playing to stop) must not mark the item
/// the next request starts: every playback request clears it.
#[test]
fn a_playback_request_clears_a_finished_flag_no_stop_took() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let mut meta = plx_data::stores::metadata::MetadataStore::default();
    ps.finished = true;

    assert!(request_play(
        &mut ps, &mut meta, plx_platform::devcaps::caps(), ServerId::from_raw(0), "film", "/library/parts/2/file.mkv", "h264", "aac", "Film", "", None, "",
    ));
    assert!(!ps.finished, "the request retired the flag");

    cancel_play(&mut ps);
    let _ = wait_resolve_workers_for_test(std::time::Duration::from_secs(10));
    reset_player_control_for_test(&ps);
    reset_session(&mut ps);
}

/// **A stop never outlives the identity it was made under.** A `stopped` POST stalled on a slow
/// server can land after a profile switch; its record would then patch, read back and fan out on the
/// NEW profile. `retire_pending_stops` (called where every store is reset) drops both the record
/// already in the mailbox and the one a worker posts afterwards, and leaves nothing owed.
#[test]
fn a_stop_posted_after_the_identity_changed_is_dropped() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    let _ = take_landed_stops();
    let rec = |rk: &str| plx_data::viewstate::StopRecord {
        sid: ServerId::from_raw(0), rk: rk.into(),
        pos_ms: 30_000, dur_ms: 60_000, ok: false, was_watched: false,
    };
    let mut landed_before = PendingStop::new(rec("1"));
    let mut stalled = PendingStop::new(rec("2"));
    landed_before.post(true);
    assert!(stops_owed());

    retire_pending_stops();
    assert!(take_landed_stops().is_empty(), "the posted record was emptied with the mailbox");
    stalled.post(true); // the stalled worker's POST returns after the switch
    assert!(take_landed_stops().is_empty(), "the late record is another identity's");
    assert!(!stops_owed(), "a dropped record is no longer owed");

    let mut fresh = PendingStop::new(rec("3"));
    fresh.post(true);
    assert_eq!(take_landed_stops().len(), 1, "a stop made after the switch lands as ever");
    reset_player_control_for_test(&ps);
}

/// Test-only: stand up a playing item on a recording PMS, with `first` as the live HLS encoder.
/// Returns the PMS's request lines and the reporter's lease; the caller holds the serial guard.
fn playing_on_recording_pms(
    ps: &mut crate::route::PlaybackSession,
    first: &str,
) -> (std::sync::mpsc::Receiver<String>, TimelineLease) {
    drain_scrobble();
    reset_session(ps);
    reset_player_control_for_test(ps);
    // `max` is a ceiling (the stub gives up after ten idle seconds), not an expectation.
    let (port, rx, _server) = recording_pms(8);
    (rx, playing_on_port(ps, first, port))
}

/// The item `playing_on_recording_pms` plays, on the PMS at `port`: registered, planned, `first`
/// installed as the live HLS encoder, and a reporter lease taken.
fn playing_on_port(ps: &mut crate::route::PlaybackSession, first: &str, port: i32) -> TimelineLease {
    let sid = plx_plex::plex::register_for_test(
        "timeline-retired",
        "127.0.0.1",
        port,
        "tok",
        "timeline-client",
    );
    apply_plan(
        ps,
        Plan {
            sid,
            url: "https://example.invalid/auto.m3u8".into(),
            sess: "logical-1236".into(),
            pq_id: "pq-1236".into(),
            pq_item_id: "pqi-1236".into(),
            ..Default::default()
        },
        "rk-1236",
    );
    install_active_hls(first, "http://fixture.invalid/first.m3u8", crate::abr::Rung::P720);
    begin_timeline_reporting(ps).expect("timeline lease")
}

/// What the stop worker leaves in `PlayerControl` once it is done: the last reported id.
fn reported_timeline_left_behind() -> Option<ReportedTimeline> {
    PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).reported_timeline.clone()
}

/// Test-only: commit `id` as the live HLS encoder through the door an automatic quality switch
/// uses, so the route epoch moves the way it does in production.
fn commit_hls_encoder(id: &str) {
    replace_active_hls_for(
        &worker_ticket(),
        id,
        "http://fixture.invalid/next.m3u8",
        crate::abr::Rung::P1080,
        None,
    )
    .expect("the ABR commit is current");
}

/// Every request line the PMS gets until it has been quiet for 300 ms, oldest first. The caller
/// compares the whole list, so a missing report and an extra one both read as a wrong sequence.
fn requests_until_quiet(rx: &std::sync::mpsc::Receiver<String>) -> Vec<String> {
    use std::time::Duration;
    let mut lines = vec![rx.recv_timeout(Duration::from_secs(5)).expect("no request reached the PMS")];
    while let Ok(line) = rx.recv_timeout(Duration::from_millis(300)) {
        lines.push(line);
    }
    lines
}

#[test]
fn a_termination_on_the_answer_ends_the_reporter_and_keeps_the_server_s_reason() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    let (port, rx, _server) = recording_pms_answering(
        8,
        r#"{"MediaContainer":{"size":0,"terminationCode":2006,"terminationText":"stopped by the admin"}}"#,
    );
    let lease = playing_on_port(&mut ps, "logical-stop-abr-1", port);
    assert!(
        !report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000),
        "the reporter ends here, so no more `playing` reaches the server"
    );
    let lines = requests_until_quiet(&rx);
    let shape = crate::player::error_now(&ps);
    leave_recording_pms_case(&mut ps);
    crate::player::SHARED.reset_session();
    assert_eq!(lines.len(), 2, "the report, then the stop that clears the row: {lines:#?}");
    assert!(lines[0].contains("state=playing"), "{}", lines[0]);
    assert!(lines[1].contains("state=stopped") && lines[1].contains("continuing=1"), "{}", lines[1]);
    assert_eq!(shape.kind, crate::player::FailureKind::ServerStopped);
    assert_eq!(shape.detail, "stopped by the admin");
}

#[test]
fn server_stop_is_visible_while_dashboard_cleanup_waits_and_retires_the_row_once() {
    use std::time::Duration;

    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    crate::player::SHARED.reset_session();
    let (port, rx, release, _server) = held_pms_answering(
        2,
        r#"{"MediaContainer":{"terminationCode":2006,"terminationText":"stopped by the admin"}}"#,
    );
    let lease = playing_on_port(&mut ps, "logical-1236-abr-1", port);
    let reporter = std::thread::spawn(move || {
        report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000)
    });
    let first = rx.recv_timeout(Duration::from_secs(5)).expect("playing(A)");
    let parked = rx.recv_timeout(Duration::from_secs(5)).expect("continuing stop(A)");
    let shape = crate::player::error_now(&ps);
    commit_hls_encoder("logical-1236-abr-2");

    release.send(()).unwrap();
    assert!(!reporter.join().unwrap());
    let replacement = begin_timeline_reporting(&ps).expect("replacement reporter lease");
    let replacement_sent = report_timeline(&replacement, plx_plex::plex::TimelineState::Playing, 20_000, 60_000);
    begin_engine_teardown(false);
    scrobble_stop(&mut ps, Some(("rk-1236".into(), 10_000, 60_000)), None);
    drain_scrobble();
    let remaining = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    crate::player::SHARED.reset_session();

    assert!(first.contains("state=playing"));
    assert!(parked.contains("state=stopped") && parked.contains("continuing=1"));
    assert_eq!(shape.kind, crate::player::FailureKind::ServerStopped);
    assert_eq!(shape.detail, "stopped by the admin");
    assert!(!replacement_sent, "the replacement reporter must send no playing report");
    assert_eq!(remaining.len(), 2, "only the final stopped and the encoder stop: {remaining:#?}");
    assert!(remaining[0].contains("state=stopped") && !remaining[0].contains("continuing=1"));
    assert!(remaining[0].contains("X-Plex-Session-Identifier=logical-1236-abr-2"));
    assert!(remaining[1].contains("/transcode/universal/stop"));
}

fn leave_recording_pms_case(ps: &mut crate::route::PlaybackSession) {
    plx_plex::plex::reset_servers_for_test();
    reset_session(ps);
    install_active_encoder("");
    reset_player_control_for_test(ps);
}

/// Issue #1236: an automatic quality switch replaces the PMS encoder, and every id that ever
/// reported `playing` kept a "playing" dashboard row for minutes after its last report (one
/// playback showed up as 13). The periodic report moves to the new id; the old one must be told
/// it stopped, as Plex's own TV client does before it restarts a stream: `state=stopped` with
/// `continuing=1`, at the position the playback has reached, BEFORE the new id's first report.
#[test]
fn a_retired_encoder_that_reported_gets_a_continuing_stopped_before_the_next_report() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));

    commit_hls_encoder("logical-1236-abr-2");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 20_000, 60_000));

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 3, "playing(A), stopped(A), playing(B) expected: {lines:#?}");
    assert!(
        lines[0].contains("state=playing")
            && lines[0].contains("X-Plex-Session-Identifier=logical-1236-abr-1"),
        "the first report is A's playing: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("state=stopped")
            && lines[1].contains("continuing=1")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1")
            && lines[1].contains("ratingKey=rk-1236")
            && lines[1].contains("time=20000")
            && lines[1].contains("playQueueID=pq-1236"),
        "before B reports, A is told it stopped, continuing, at the new position: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("state=playing")
            && !lines[2].contains("continuing")
            && lines[2].contains("X-Plex-Session-Identifier=logical-1236-abr-2"),
        "B's report follows, as an ordinary playing: {}",
        lines[2]
    );
}

/// The whole decision, pure: the previous report's id against the id about to go out.
#[test]
fn only_an_id_that_reported_and_was_left_behind_is_retired() {
    // first report: nothing was reported, so nothing is retired
    assert_eq!(retired_report_session(None, "a"), None);
    // the same id again: still the reporting identity
    assert_eq!(retired_report_session(Some("a"), "a"), None);
    // the identity moved: the id it left is the one to stop, never the new one
    assert_eq!(retired_report_session(Some("a"), "b"), Some("a"));
    // an empty id names no session on the wire, so it is never stopped
    assert_eq!(retired_report_session(Some(""), "b"), None);
    // a direct play's logical id retired by the HLS fallback's encoder, and back
    assert_eq!(retired_report_session(Some("logical"), "logical-abr-1"), Some("logical"));
    assert_eq!(retired_report_session(Some("logical-abr-1"), "logical"), Some("logical-abr-1"));
}

/// An ordinary playback sends ordinary reports: the same id every tick means no `stopped` and no
/// `continuing` anywhere, so a server that never saw the retirement sees the same requests as ever.
#[test]
fn reports_under_one_id_never_retire_anything() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Paused, 20_000, 60_000));
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 30_000, 60_000));

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 3, "one report per call: {lines:#?}");
    for line in &lines {
        assert!(
            !line.contains("state=stopped") && !line.contains("continuing"),
            "nothing was retired: {line}"
        );
        assert!(line.contains("X-Plex-Session-Identifier=logical-1236-abr-1"), "{line}");
    }
}

/// **A `stopped` is only for an id that reported.** An automatic switch can mint and commit an
/// encoder and replace it again before the next tick (and a candidate that is never committed
/// never becomes the reporting identity at all); PMS has no row for such an id, and a `stopped`
/// naming one would be a write about a session this playback never announced. Only the id the
/// periodic report last went out under is retired, once.
#[test]
fn an_encoder_that_never_reported_is_never_stopped() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));

    // Two commits land between ticks: the first one is never the reporting identity.
    commit_hls_encoder("logical-1236-abr-2");
    commit_hls_encoder("logical-1236-abr-3");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 20_000, 60_000));

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 3, "playing(abr-1), stopped(abr-1), playing(abr-3): {lines:#?}");
    assert!(
        lines[1].contains("state=stopped")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1"),
        "the stopped is the reported id's: {}",
        lines[1]
    );
    assert!(
        lines.iter().all(|l| !l.contains("logical-1236-abr-2")),
        "abr-2 never reported, it must not be named: {lines:#?}"
    );
}

/// A quality change or a transcode seek reloads the Engine, which starts a NEW reporter for the
/// same playback, after `begin_engine_teardown(true)` has retired the old one's lease. The id the
/// old reporter last reported under is the one the reload retired, so the new reporter's first
/// report must still stop it.
#[test]
fn a_reloaded_engine_still_retires_the_id_its_predecessor_reported() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, old_lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&old_lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));

    commit_hls_encoder("logical-1236-abr-2");
    begin_engine_teardown(true);
    assert!(
        !report_timeline(&old_lease, plx_plex::plex::TimelineState::Playing, 15_000, 60_000),
        "premise: the reload retired the old Engine's lease"
    );
    let new_lease = begin_timeline_reporting(&ps).expect("the reload's lease");
    assert!(report_timeline(&new_lease, plx_plex::plex::TimelineState::Playing, 20_000, 60_000));

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 3, "playing(abr-1), stopped(abr-1), playing(abr-2): {lines:#?}");
    assert!(
        lines[1].contains("state=stopped")
            && lines[1].contains("continuing=1")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1"),
        "the reload's first report retires its predecessor's id: {}",
        lines[1]
    );
}

#[test]
fn a_different_item_inherits_no_reported_id() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));

    // The next item starts without a stop in between (the projection is republished for it).
    let sid = cur_sid(&ps);
    apply_plan(
        &mut ps,
        Plan { sid, url: "https://example.invalid/next.m3u8".into(), sess: "logical-next".into(), ..Default::default() },
        "rk-next",
    );
    install_active_hls("logical-next-abr-1", "http://fixture.invalid/n.m3u8", crate::abr::Rung::P720);
    let next = begin_timeline_reporting(&ps).expect("the next item's lease");
    assert!(report_timeline(&next, plx_plex::plex::TimelineState::Playing, 5_000, 60_000));

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 2, "playing(first), playing(next): {lines:#?}");
    assert!(
        lines[1].contains("ratingKey=rk-next") && !lines[1].contains("state=stopped"),
        "a different item's first report stops nothing: {}",
        lines[1]
    );
}

/// The final teardown: when the encoder changed after the last periodic report, the last reported
/// id must still be told `stopped` (before the final one, which commits the resume point), and the
/// encoder stop follows as ever. With the id unchanged there is nothing extra to send.
#[test]
fn the_final_stop_also_retires_the_last_reported_id_when_it_moved_on() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));
    commit_hls_encoder("logical-1236-abr-2");

    scrobble_stop(&mut ps, Some(("rk-1236".into(), 30_000, 60_000)), None);
    drain_scrobble();

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(
        lines.len(),
        4,
        "playing(abr-1), stopped(abr-1), stopped(abr-2), encoder stop: {lines:#?}"
    );
    assert!(
        lines[1].contains("state=stopped")
            && lines[1].contains("continuing=1")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1")
            && lines[1].contains("time=30000"),
        "the reported id is retired first: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("state=stopped")
            && !lines[2].contains("continuing")
            && lines[2].contains("X-Plex-Session-Identifier=logical-1236-abr-2"),
        "then the final stopped, an ordinary one: {}",
        lines[2]
    );
    assert!(lines[3].contains("/transcode/universal/stop"), "{}", lines[3]);
}

#[test]
fn the_final_stop_under_the_reported_id_sends_no_extra_stopped() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));

    scrobble_stop(&mut ps, Some(("rk-1236".into(), 30_000, 60_000)), None);
    drain_scrobble();

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 3, "playing, the final stopped, the encoder stop: {lines:#?}");
    assert!(
        lines[1].contains("state=stopped")
            && !lines[1].contains("continuing")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1"),
        "{}",
        lines[1]
    );
}

/// The order a real stop runs in (`player::engine::teardown`): the route's teardown retires the
/// Engine and the reporter's projection FIRST, and only then, after the media threads are joined,
/// does `scrobble_stop` build the stop worker. The id the reporter last used has to survive the
/// first to reach the second, or the final stop never names it.
#[test]
fn the_final_stop_retires_the_last_reported_id_after_the_engine_teardown_ran_first() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));
    commit_hls_encoder("logical-1236-abr-2");

    begin_engine_teardown(false);
    scrobble_stop(&mut ps, Some(("rk-1236".into(), 30_000, 60_000)), None);
    drain_scrobble();

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(
        lines.len(),
        4,
        "playing(abr-1), stopped(abr-1), stopped(abr-2), encoder stop: {lines:#?}"
    );
    assert!(
        lines[1].contains("state=stopped")
            && lines[1].contains("continuing=1")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1"),
        "the reported id is retired first: {}",
        lines[1]
    );
}

/// A failed Engine (like a failed start or a failed Original trial) clears the timeline
/// projection, and the Engine that follows it (Try Again, or a rollback's rebase onto a new
/// encoder) reports from a fresh one. The id the playback had been reporting under must survive
/// that. `fail_current_engine` is the real edge; the same item is then played again.
#[test]
fn a_cleared_projection_does_not_forget_the_id_the_playback_reported_under() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));

    fail_current_engine();
    assert!(
        PLAYER_CONTROL.lock().unwrap_or_else(|e| e.into_inner()).timeline.is_none(),
        "premise: the failure cleared the projection"
    );
    let sid = cur_sid(&ps);
    apply_plan(
        &mut ps,
        Plan {
            sid,
            url: "https://example.invalid/auto.m3u8".into(),
            sess: "logical-1236".into(),
            pq_id: "pq-1236".into(),
            pq_item_id: "pqi-1236".into(),
            ..Default::default()
        },
        "rk-1236",
    );
    install_active_hls("logical-1236-abr-2", "http://fixture.invalid/second.m3u8", crate::abr::Rung::P1080);
    let rebuilt = begin_timeline_reporting(&ps).expect("the rebuilt Engine's lease");
    assert!(report_timeline(&rebuilt, plx_plex::plex::TimelineState::Playing, 20_000, 60_000));

    let lines = requests_until_quiet(&rx);
    leave_recording_pms_case(&mut ps);
    assert_eq!(lines.len(), 3, "playing(abr-1), stopped(abr-1), playing(abr-2): {lines:#?}");
    assert!(
        lines[1].contains("state=stopped")
            && lines[1].contains("continuing=1")
            && lines[1].contains("X-Plex-Session-Identifier=logical-1236-abr-1"),
        "the rebuilt reporter retires the id the cleared projection had reported: {}",
        lines[1]
    );
}

/// **The stop worker takes the reported id AFTER the reporter's last POST, not before.** On a real
/// stop `begin_engine_teardown` bumps the epoch, `engine::teardown` only signals the reporter and
/// hands its handle to `scrobble_stop` unjoined, and the worker joins it. A tick already past its
/// epoch check is not stopped by the bump: it finishes its POSTs and records its id. The encoder
/// moved A to B and the viewer pressed BACK while tick N was sending `stopped(A)` + `playing(B)`:
/// a take on the main thread, before that join, saw A and sent `stopped(A)` a second time, then
/// left B behind for the next playback of the item to retire.
#[test]
fn a_tick_in_flight_at_the_stop_is_not_retired_twice_and_leaves_no_record() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    // Request 1 is `playing(A)`; request 2 is the tick's `stopped(A)`, which the stub parks.
    let (port, rx, release, _server) = held_pms(2);
    let lease = playing_on_port(&mut ps, "logical-1236-abr-1", port);
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));
    commit_hls_encoder("logical-1236-abr-2");

    let reporter = std::thread::spawn(move || {
        assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 20_000, 60_000));
    });
    let first = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("playing(A)");
    let parked = rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the tick's stopped(A)");
    assert!(first.contains("state=playing"), "{first}");
    assert!(
        parked.contains("state=stopped") && parked.contains("logical-1236-abr-1"),
        "premise: the tick is parked mid-POST, holding the effect lane: {parked}"
    );

    // The stop, in the order `engine::teardown` runs it, with the tick's thread as the reporter.
    begin_engine_teardown(false);
    scrobble_stop(&mut ps, Some(("rk-1236".into(), 30_000, 60_000)), Some(reporter));
    release.send(()).unwrap();
    drain_scrobble();

    let mut lines = vec![first, parked];
    lines.extend(requests_until_quiet(&rx));
    let left = reported_timeline_left_behind();
    leave_recording_pms_case(&mut ps);
    let retired_a = lines
        .iter()
        .filter(|l| l.contains("state=stopped") && l.contains("continuing=1"))
        .count();
    assert_eq!(
        retired_a, 1,
        "A is retired exactly once, by the tick that moved off it: {lines:#?}"
    );
    assert_eq!(
        lines.len(),
        5,
        "playing(A), stopped(A), playing(B), the final stopped(B), the encoder stop: {lines:#?}"
    );
    assert!(
        lines[3].contains("state=stopped")
            && !lines[3].contains("continuing")
            && lines[3].contains("X-Plex-Session-Identifier=logical-1236-abr-2"),
        "the final stopped goes out under B: {}",
        lines[3]
    );
    assert_eq!(left, None, "the stop worker consumed the record the late tick wrote");
}

/// A reporter cut loose by a reload (its handle dropped) can still have a tick in flight when the
/// Engine-less stop runs with nothing to post. That stop has no worker to take the record, so it
/// must not leave one behind, and must not wait on the tick either: it hands the take to a worker.
#[test]
fn a_stop_with_nothing_to_post_still_leaves_no_record_behind_a_tick_in_flight() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    drain_scrobble();
    reset_session(&mut ps);
    reset_player_control_for_test(&ps);
    let (port, rx, release, _server) = held_pms(2);
    let lease = playing_on_port(&mut ps, "logical-1236-abr-1", port);
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));
    commit_hls_encoder("logical-1236-abr-2");

    let tick = std::thread::spawn(move || {
        report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 20_000, 60_000)
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("playing(A)");
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("the tick's stopped(A), parked");

    // The reload cut the reporter loose; the new start failed and Back is pressed with no encoder.
    begin_engine_teardown(true);
    install_active_encoder("");
    scrobble_stop(&mut ps, None, None);
    release.send(()).unwrap();
    assert!(tick.join().expect("the tick returns"), "the tick completed its POSTs");
    drain_scrobble();

    let left = reported_timeline_left_behind();
    leave_recording_pms_case(&mut ps);
    assert_eq!(left, None, "no worker, or a worker that ran before the tick's note, leaves it");
}

/// With no reporter alive the stop has nothing to wait for and spawns no worker, but the record a
/// reporter left must still go with the playback it belonged to.
#[test]
fn a_stop_that_spawns_no_worker_still_consumes_the_record() {
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (_rx, lease) = playing_on_recording_pms(&mut ps, "logical-1236-abr-1");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 10_000, 60_000));
    assert!(reported_timeline_left_behind().is_some(), "premise: the report was recorded");

    install_active_encoder("");
    begin_engine_teardown(false);
    scrobble_stop(&mut ps, None, None);
    drain_scrobble();

    let left = reported_timeline_left_behind();
    leave_recording_pms_case(&mut ps);
    assert_eq!(left, None);
}
