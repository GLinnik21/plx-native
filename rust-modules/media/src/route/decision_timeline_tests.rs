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
        live: None,
        version: None,
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
        &mut ps, &mut meta, plx_platform::devcaps::caps(), ServerId::from_raw(0), "film", "/library/parts/2/file.mkv", "h264", "aac", "Film", "", None,
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
        &mut ps, &mut meta, plx_platform::devcaps::caps(), ServerId::from_raw(0), "film", "/library/parts/2/file.mkv", "h264", "aac", "Film", "", None,
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

// The official client's Live TV sequence (`docs/pms-api.md` §10), as a loopback server answers it.
// The tune's body nests the session the way the server log names its levels; it was not captured.
const LIVE_TUNED: &[u8] = br#"{"MediaContainer":{"size":1,"MediaSubscription":[{"id":112,
    "MediaGrabOperation":[{"Metadata":{"key":"/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2",
    "ratingKey":"355108","type":"episode"}}]}]}}"#;
const LIVE_DECIDED: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"videoCodec":"h264","audioCodec":"aac",
    "Part":[{"key":"/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/cid-live/index.m3u8?offset=-1.000000",
    "decision":"directplay"}]}]}]}}"#;

/// A loopback PMS for the live sequence: answers the tune and the decision with their shapes and
/// everything else with an empty container, handing each request line down the channel.
fn live_pms(decision_refused: bool) -> (i32, std::sync::mpsc::Receiver<String>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let first = drain_http(&mut socket);
                    let decision = first.starts_with("GET /video/:/transcode/universal/decision?");
                    if decision && decision_refused {
                        // PMS 1.43.3 on the TV, 2026-10-21: "unable to find a matching profile".
                        write_status(&mut socket, 400);
                    } else {
                        let body = if first.starts_with("POST /livetv/dvrs/7/channels/C101.242.ersatztv.org/tune") {
                            LIVE_TUNED
                        } else if decision {
                            LIVE_DECIDED
                        } else {
                            EMPTY_MC
                        };
                        write_json(&mut socket, body);
                    }
                    if tx.send(first).is_err() {
                        return;
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(4)),
            }
        }
    });
    (port, rx)
}

#[test]
fn a_live_channel_tunes_then_reports_under_its_session_and_its_stop_frees_the_tuner() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx) = live_pms(false);
    let sid = plx_plex::plex::register_for_test("route-live", "127.0.0.1", port, "tok-live", "cid-live");
    let mut env = ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "");
    env.live = Some(LiveTune { dvr: "7".into(), channel: "C101.242.ersatztv.org".into() });
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "", "", "", "", &env);
    let next = || rx.recv_timeout(Duration::from_secs(5)).expect("a request");

    let tune = next();
    assert!(tune.starts_with("POST /livetv/dvrs/7/channels/C101.242.ersatztv.org/tune?"), "{tune}");
    assert_eq!(
        query_param(&tune, "X-Plex-Session-Identifier"),
        Some(plan.sess.as_str()),
        "PMS refuses a tune without a session id (400), and it is the one the playback reports under"
    );
    assert_eq!(query_param(&tune, "X-Plex-Client-Identifier"), Some("cid-live"));
    let decision = next();
    assert_eq!(query_param(&decision, "path"), Some("%2Flivetv%2Fsessions%2F9b9fe727-0000-46db-b94c-ef512e3e61d2"));
    assert_eq!(query_param(&decision, "directPlay"), Some("1"));
    assert_eq!(query_param(&decision, "hasMDE"), Some("1"), "the client's own decision is what PMS calls a direct play");
    assert_eq!(query_param(&decision, "subtitles"), Some("none"));
    assert_eq!(query_param(&decision, "X-Plex-Client-Profile-Name"), Some("Generic"), "PMS 400s a profile-less decision");
    let profile = query_param(&decision, "X-Plex-Client-Profile-Extra").expect("a profile augmentation");
    assert!(profile.contains("mpegts") && !profile.contains("direct-play"),
        "an HLS target and no direct-play profile, or PMS decides a conversion: {profile}");
    assert_eq!(query_param(&decision, "session"), Some(plan.sess.as_str()), "the decision and the URL share one session");

    let live = plan.live.clone().expect("the plan names the recording");
    assert_eq!((live.key.as_str(), live.rating_key.as_str()), ("/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2", "355108"));
    assert!(plan.url.contains("/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/cid-live/index.m3u8?offset=-1.000000&"), "{}", plan.url);
    assert!(plan.url.contains("X-Plex-Token=tok-live") && plan.url.contains(&format!("X-Plex-Session-Identifier={}", plan.sess)));
    assert_eq!((plan.vcodec.as_str(), plan.acodec.as_str()), ("h264", "aac"));
    assert!(plan.tsession.is_empty() && plan.pq_id.is_empty(), "a direct play with no PlayQueue");

    apply_plan(&mut ps, plan, "");
    assert!(is_live(&ps) && live_playback());
    let lease = begin_timeline_reporting(&ps).expect("a live play reports without a ratingKey of its own");
    assert!(report_timeline(&lease, plx_plex::plex::TimelineState::Playing, 12_000, 12_000));
    let playing = next();
    assert!(playing.starts_with("POST /:/timeline?key=%2Flivetv%2Fsessions%2F9b9fe727-0000-46db-b94c-ef512e3e61d2&ratingKey=355108&state=playing&time=0&duration=0&playbackTime=12000"), "{playing}");

    scrobble_stop(&mut ps, None, None);
    drain_scrobble();
    let stopped = next();
    assert!(stopped.starts_with("POST /:/timeline?key=%2Flivetv%2Fsessions%2F9b9fe727-0000-46db-b94c-ef512e3e61d2"), "{stopped}");
    assert_eq!(query_param(&stopped, "state"), Some("stopped"));
    assert_eq!(query_param(&stopped, "continuing"), Some("0"));
    assert!(!is_live(&ps) && !live_playback(), "the stop retires the session");
}

#[test]
fn a_refused_live_decision_still_plays_the_session_s_own_playlist() {
    use std::time::Duration;
    let mut ps = crate::route::PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    let (port, rx) = live_pms(true);
    let sid = plx_plex::plex::register_for_test("route-live-400", "127.0.0.1", port, "tok-live", "cid-live");
    let mut env = ResolveEnv::snapshot(&ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "");
    env.live = Some(LiveTune { dvr: "7".into(), channel: "C101.242.ersatztv.org".into() });
    let plan = build_stream(&plx_base::task::OffFrame::for_test(), "", "", "", "", &env);
    for _ in 0..2 {
        rx.recv_timeout(Duration::from_secs(5)).expect("the tune, then the refused decision");
    }
    assert!(plan.live.is_some(), "the recording is still the plan's to stop");
    let own = format!("/livetv/sessions/9b9fe727-0000-46db-b94c-ef512e3e61d2/{}/index.m3u8?offset=-1.000000&", plan.sess);
    assert!(plan.url.contains(&own), "the consumer is named after the tune's session id: {}", plan.url);
    assert_eq!((plan.vcodec.as_str(), plan.acodec.as_str()), ("h264", "aac"));
}

#[test]
fn a_live_plan_abandoned_after_its_tune_still_stops_the_recording() {
    let live = plx_plex::plex::livetv::LiveSession { key: "/livetv/sessions/abcdef01".into(), rating_key: String::new() };
    let tuned_only = Plan { sid: ServerId::from_raw(3), live: Some(live.clone()), ..Default::default() };
    let resources = abandoned_plan_resources(&tuned_only).expect("a tuned session is a server object to retire");
    assert_eq!(resources.live, Some((live.clone(), String::new())));
    let decided = Plan { sess: "s1".into(), live: Some(live.clone()), ..Default::default() };
    let resources = abandoned_plan_resources(&decided).unwrap();
    assert_eq!((resources.identities, resources.live), (vec!["s1".to_string()], Some((live, "s1".to_string()))));
    assert!(abandoned_plan_resources(&Plan::default()).is_none());
}
