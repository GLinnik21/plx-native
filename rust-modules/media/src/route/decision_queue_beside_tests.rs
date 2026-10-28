//! **The play queue is asked for BESIDE the rest of a playback start** (`plan::build_stream`),
//! and lands in the plan on every exit.
//!
//! The ordinary loopback PMS (`plan_pms`) answers one connection at a time and records arrival
//! order, so it cannot tell "asked beside" from "asked first": both answer. This one serves each
//! connection on a thread of its own and holds the `POST /playQueues` answer, so a scenario can
//! only finish if the queue request was still OPEN when the rest of the plan asked its questions.
use super::test_support::*;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The queue the loopback PMS hands out: ids the plan must carry.
const QUEUE_BODY: &[u8] = br#"{"MediaContainer":{"size":0,"playQueueID":4242,"playQueueSelectedItemID":9001,"playQueueTotalCount":1,"playQueueSelectedItemOffset":0}}"#;

/// How long the queue's answer is held.
#[derive(Clone, Copy)]
enum Hold {
    /// Until `/decision` has been requested (at most [`GATE`]): the answer exists only if the two
    /// requests were open together.
    UntilDecision,
    /// For a fixed time, whatever else is asked: long enough for an early exit to return first.
    For(Duration),
}

/// The longest a held queue answer waits for `/decision` before giving up and answering.
const GATE: Duration = Duration::from_secs(3);

struct Pms {
    port: i32,
    /// `/decision` had been requested by the time the queue's answer was released.
    decision_came_first: Arc<AtomicBool>,
    server: std::thread::JoinHandle<Vec<String>>,
}

/// A loopback PMS that serves `n` requests, each on its own thread, and holds the queue's answer.
fn queue_pms(n: usize, hold: Hold, mde: &'static [u8]) -> Pms {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().unwrap().port() as i32;
    let decision_came_first = Arc::new(AtomicBool::new(false));
    let verdict = Arc::clone(&decision_came_first);
    let server = std::thread::spawn(move || {
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let decision_seen = Arc::new(AtomicBool::new(false));
        let mut handlers = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while handlers.len() < n && Instant::now() < deadline {
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let (seen, decision_seen, verdict) =
                        (Arc::clone(&seen), Arc::clone(&decision_seen), Arc::clone(&verdict));
                    handlers.push(std::thread::spawn(move || {
                        let first = drain_http(&mut socket);
                        seen.lock().unwrap().push(first.clone());
                        if first.starts_with("POST /playQueues") {
                            match hold {
                                Hold::UntilDecision => {
                                    let until = Instant::now() + GATE;
                                    while !decision_seen.load(Ordering::SeqCst) && Instant::now() < until {
                                        std::thread::sleep(Duration::from_millis(5));
                                    }
                                    verdict.store(decision_seen.load(Ordering::SeqCst), Ordering::SeqCst);
                                }
                                Hold::For(wait) => std::thread::sleep(wait),
                            }
                            write_json(&mut socket, QUEUE_BODY);
                        } else if first.contains("/decision?") {
                            decision_seen.store(true, Ordering::SeqCst);
                            write_json(&mut socket, mde);
                        } else {
                            write_json(&mut socket, EMPTY_MC);
                        }
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(4)),
                Err(e) => panic!("accept: {e}"),
            }
        }
        for h in handlers {
            h.join().expect("a request handler");
        }
        let requests = seen.lock().unwrap().clone();
        assert_eq!(requests.len(), n, "the scenario makes {n} requests: {requests:#?}");
        requests
    });
    Pms { port, decision_came_first, server }
}

/// Resolve a 4K movie that is already loaded (no item GET) against `pms`.
fn resolve(ps: &PlaybackSession, pms: &Pms, mode: DirectPlayMode) -> Plan {
    let sid = plx_plex::plex::register_for_test("queue-beside", "127.0.0.1", pms.port, "token", "queue-client");
    let mut env = ResolveEnv::snapshot(ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, "rk-4k");
    env.direct_play_mode = mode;
    env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
    let plan = build_stream(
        &plx_base::task::OffFrame::for_test(),
        "rk-4k",
        "/library/parts/36013/1/file.mkv",
        "hevc",
        "eac3",
        &env,
    );
    plan
}

/// The queue request and `/decision` are open at the same time. A resolve that asked the queue
/// FIRST and the decision after it would hold the queue's answer for the whole gate (nothing asks
/// `/decision` until it returns), release it unaided, and fail this.
#[test]
#[cfg(feature = "devtriggers")]
fn the_play_queue_and_the_decision_are_open_at_once() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let pms = queue_pms(2, Hold::UntilDecision, MDE_DIRECTPLAY);
    let plan = resolve(&ps, &pms, DirectPlayMode::Auto);
    let overlapped = pms.decision_came_first.load(Ordering::SeqCst);
    pms.server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
    assert!(overlapped, "the queue was answered before anything asked `/decision`: the two ran one after the other");
    assert!(plan.url.contains("/library/parts/36013/"), "{}", plan.url);
    assert_eq!((plan.pq_id.as_str(), plan.pq_item_id.as_str()), ("4242", "9001"), "the queue lands on the success path");
}

/// An exit that returns the plan early (here the server's refusal under Force Direct Play) still
/// waits for the queue it asked for and lands it, though the queue's answer comes long after the
/// refusal was known. A queue left running behind a plan built without it would leave these empty.
#[test]
#[cfg(feature = "devtriggers")]
fn an_early_exit_still_lands_the_queue() {
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    assert!(plx_net::net::global_init());
    let pms = queue_pms(2, Hold::For(Duration::from_millis(400)), MDE_TRANSCODE);
    let started = Instant::now();
    let plan = resolve(&ps, &pms, DirectPlayMode::Forced);
    let waited = started.elapsed();
    pms.server.join().unwrap();
    plx_plex::plex::reset_servers_for_test();
    assert!(plan.url.is_empty(), "the refusal is an early exit: {}", plan.url);
    assert!(plan.verdict.as_ref().is_some_and(|v| v.text().contains("Force Direct Play is on")));
    assert!(waited >= Duration::from_millis(350), "build_stream returned after {waited:?}, before the queue answered");
    assert_eq!((plan.pq_id.as_str(), plan.pq_item_id.as_str()), ("4242", "9001"), "the queue lands on an early exit too");
}
