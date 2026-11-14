//! **A fresh play of an episode lands a queue that has the episodes BEFORE it**, so Previous has
//! somewhere to go (`queue_neighbors`, the HUD's Previous disc).
//!
//! The loopback PMS here answers `POST /playQueues` the way the two documented request shapes
//! differ, and the test grades what the app asked for, not a canned body:
//!
//! * an EPISODE uri with `continuous=1` (what the app used to send for every play) returns that
//!   episode and the ones AFTER it, `playQueueSelectedItemOffset: 0` (docs/pms-api.md, "Up Next":
//!   the queue is the show's remaining episodes);
//! * the SHOW uri with `key=/library/metadata/{episode}` returns the show's episodes in order with
//!   the keyed one selected (docs/plex-openapi.json, `POST /playQueues`: "`key`: the key of the
//!   first item to play"; python-plexapi's `PlayQueue.create(show, startItem=episode)` sends the
//!   same two params).
//!
//! A server that ignores `key` and answers the show's own row, or the wrong selection, must never
//! cost the viewer their Up Next: the play falls back to the episode request.
use super::test_support::*;
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SHOW: &str = "50";
/// The season's episodes: (ratingKey, playQueueItemID in the queue that holds them all).
const SEASON: [(&str, i64); 4] = [("101", 7001), ("102", 7002), ("103", 7003), ("104", 7004)];

fn episode_row(rk: &str, item_id: i64, index: usize) -> String {
    format!(
        r#"{{"ratingKey":"{rk}","playQueueItemID":{item_id},"type":"episode","title":"E{index}",
        "grandparentRatingKey":"{SHOW}","grandparentTitle":"Show","parentIndex":1,"index":{index},
        "duration":1200000,"Media":[{{"videoCodec":"h264","audioCodec":"aac",
        "Part":[{{"key":"/library/parts/{rk}/file.mkv"}}]}}]}}"#
    )
}

/// A `/playQueues` answer holding `episodes` of the season (their positions in it), `selected` of
/// them being the one the server selected.
fn queue_answer(from: usize, selected: &str) -> Vec<u8> {
    let rows: Vec<String> = SEASON[from..]
        .iter()
        .enumerate()
        .map(|(n, (rk, id))| episode_row(rk, *id, from + n + 1))
        .collect();
    let at = SEASON[from..].iter().position(|(rk, _)| *rk == selected).unwrap_or(0);
    format!(
        r#"{{"MediaContainer":{{"size":{},"playQueueID":88,"playQueueSelectedItemID":{},
        "playQueueSelectedItemOffset":{at},"playQueueTotalCount":{},"Metadata":[{}]}}}}"#,
        rows.len(),
        SEASON[from + at].1,
        rows.len(),
        rows.join(","),
    )
    .into_bytes()
}

/// How the loopback server treats the SHOW-based request.
#[derive(Clone, Copy, PartialEq)]
enum ShowQueue {
    /// Honours `key`: the whole season, the keyed episode selected.
    Honoured,
    /// Ignores `key` and answers a queue that starts at the first episode, selected on it.
    KeyIgnored,
}

/// What the server answers to one request line.
fn answer(line: &str, show_queue: ShowQueue) -> Vec<u8> {
    if line.contains("/decision?") {
        return MDE_DIRECTPLAY.to_vec();
    }
    if !line.starts_with("POST /playQueues") {
        return EMPTY_MC.to_vec();
    }
    let uri = query_param(line, "uri").map(|u| u.replace("%2F", "/").replace("%3A", ":")).unwrap_or_default();
    let ask = uri.rsplit('/').next().unwrap_or_default().to_owned();
    if ask == SHOW {
        let key = query_param(line, "key").map(|k| k.replace("%2F", "/")).unwrap_or_default();
        let wanted = key.rsplit('/').next().unwrap_or_default();
        return match show_queue {
            ShowQueue::Honoured => queue_answer(0, wanted),
            ShowQueue::KeyIgnored => queue_answer(0, SEASON[0].0),
        };
    }
    // An episode uri: the episode and the ones after it, selected on the first.
    let from = SEASON.iter().position(|(rk, _)| *rk == ask).unwrap_or(0);
    queue_answer(from, &ask)
}

struct Pms {
    port: i32,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Pms {
    fn serving(show_queue: ShowQueue) -> Pms {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().unwrap().port() as i32;
        let requests = Arc::new(Mutex::new(Vec::<String>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (log, stopping) = (Arc::clone(&requests), Arc::clone(&stop));
        let worker = std::thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                match plx_base::testnet::accept(&listener) {
                    Ok((mut socket, _)) => {
                        socket.set_nonblocking(false).expect("blocking");
                        let line = drain_http(&mut socket);
                        log.lock().unwrap().push(line.clone());
                        write_json(&mut socket, &answer(&line, show_queue));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            }
        });
        Pms { port, requests, stop, worker: Some(worker) }
    }

    fn queue_posts(&self) -> Vec<String> {
        self.requests.lock().unwrap().iter().filter(|l| l.starts_with("POST /playQueues")).cloned().collect()
    }

    /// How many window reads (`GET /playQueues/{id}`) the server has been asked for.
    fn window_reads(&self) -> usize {
        self.requests.lock().unwrap().iter().filter(|l| l.starts_with("GET /playQueues/")).count()
    }
}

impl Drop for Pms {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        plx_plex::plex::reset_servers_for_test();
    }
}

/// The process-global registry guard, the session and the loopback server of one test. Field order
/// is drop order: the server resets the registry, so the guard that licenses that write outlives it.
struct Fx {
    pms: Pms,
    ps: PlaybackSession,
    _guard: plx_base::testlock::Serial,
}

impl Fx {
    fn new(show_queue: ShowQueue) -> Fx {
        let mut ps = PlaybackSession::IDLE;
        let guard = fresh_registry(&mut ps);
        Fx { pms: Pms::serving(show_queue), ps, _guard: guard }
    }

    /// A fresh play of episode `rk` of show `show_rk` (`""`: no show), resolved against the server:
    /// the plan the start would land.
    fn play(&self, rk: &str, show_rk: &str) -> Plan {
        assert!(plx_net::net::global_init());
        let sid = plx_plex::plex::register_for_test("predecessor-queue", "127.0.0.1", self.pms.port, "token", "queue-client");
        let mut env = ResolveEnv::snapshot(&self.ps, plx_data::stores::metadata::MetadataStore::default().view(), sid, rk);
        env.cached_item = Some(fourk_item(sid, vec![eac3_track()]));
        env.queue_show_rk = show_rk.into();
        build_stream(
            &plx_base::task::OffFrame::for_test(),
            rk,
            "/library/parts/36013/1/file.mkv",
            "hevc",
            "eac3",
            &env,
        )
    }
}

/// The ratingKeys of a landed queue, and where the playing episode sits in it.
fn shape(plan: &Plan, playing: &str) -> (Vec<String>, Option<usize>) {
    let rks = plan.queue.iter().map(|r| r.rk.clone()).collect();
    let at = plx_plex::plex::queue_index_of(&plan.queue, plan.pq_item_id.parse().unwrap_or(0), plan.sid, playing);
    (rks, at)
}

/// **The regression.** Playing episode 102 fresh must land a queue with 101 before it: the HUD's
/// Previous is `index > 0` of exactly these rows (`queue_neighbors`). Before the fix the play
/// asked for the episode alone and the landed queue began at the playing row, so Previous was
/// enabled only as a restart of the current episode.
#[test]
#[cfg(feature = "devtriggers")]
fn a_fresh_play_of_a_middle_episode_lands_the_episodes_before_it() {
    let fx = Fx::new(ShowQueue::Honoured);
    let plan = fx.play("102", SHOW);
    let (rks, at) = shape(&plan, "102");
    assert_eq!(rks, ["101", "102", "103", "104"], "{:?}", fx.pms.queue_posts());
    assert_eq!(at, Some(1), "the playing row, with a predecessor");
    assert_eq!(plan.pq_id, "88");
    assert_eq!(plan.pq_item_id, "7002", "the timeline reports the keyed row's own queue item id");
    assert_eq!(plan.up_next.as_ref().map(|u| u.rk.as_str()), Some("103"), "Up Next is the row after it");
    let posts = fx.pms.queue_posts();
    assert_eq!(posts.len(), 1, "one queue was created: {posts:?}");
    let decoded = |name: &str| query_param(&posts[0], name).map(|v| v.replace("%2F", "/").replace("%3A", ":"));
    assert!(decoded("uri").is_some_and(|u| u.ends_with("/library/metadata/50")), "the SHOW is the uri: {posts:?}");
    assert_eq!(decoded("key").as_deref(), Some("/library/metadata/102"), "the episode is the key: {posts:?}");
    assert!(!posts[0].contains("continuous"), "the queue already spans the show, before and after: {posts:?}");
}

/// **A server that ignores `key` costs the viewer nothing.** The show-based answer selects the
/// wrong row, so it is not believed: the play asks for the episode alone, as it always did, and its
/// timeline ids, queue and Up Next are the ones that request returns.
#[test]
#[cfg(feature = "devtriggers")]
fn a_server_that_ignores_the_key_falls_back_to_the_episode_request() {
    let fx = Fx::new(ShowQueue::KeyIgnored);
    let plan = fx.play("102", SHOW);
    let (rks, at) = shape(&plan, "102");
    assert_eq!(rks, ["102", "103", "104"], "{:?}", fx.pms.queue_posts());
    assert_eq!(at, Some(0), "no predecessor, but a coherent queue");
    assert_eq!(plan.pq_item_id, "7002");
    assert_eq!(plan.up_next.as_ref().map(|u| u.rk.as_str()), Some("103"));
    let posts = fx.pms.queue_posts();
    assert_eq!(posts.len(), 2, "the show-based ask, then the episode ask: {posts:?}");
    assert!(posts[0].contains("key="), "{posts:?}");
    assert!(!posts[1].contains("key=") && posts[1].contains("continuous=1"), "{posts:?}");
}

/// **A play that names no show asks for the episode alone**, exactly the request every play sent
/// before: a movie, a trailer and an extra have no show, and an extra's `continuous` stays off.
#[test]
#[cfg(feature = "devtriggers")]
fn a_play_with_no_show_asks_for_the_item_alone() {
    let fx = Fx::new(ShowQueue::Honoured);
    let plan = fx.play("102", "");
    let (rks, at) = shape(&plan, "102");
    assert_eq!(rks, ["102", "103", "104"], "{:?}", fx.pms.queue_posts());
    assert_eq!(at, Some(0));
    let posts = fx.pms.queue_posts();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert!(!posts[0].contains("key="), "{posts:?}");
    assert!(posts[0].contains("continuous=1"), "{posts:?}");
}

/// **The first episode of the show has nothing before it**, so its Previous stays disabled: the
/// show-keyed queue starts on it.
#[test]
#[cfg(feature = "devtriggers")]
fn the_first_episode_of_the_show_lands_no_predecessor() {
    let fx = Fx::new(ShowQueue::Honoured);
    let plan = fx.play("101", SHOW);
    let (rks, at) = shape(&plan, "101");
    assert_eq!(rks, ["101", "102", "103", "104"], "{:?}", fx.pms.queue_posts());
    assert_eq!(at, Some(0), "the first row: queue_neighbors reports no previous");
}

/// Four queue rows of the season (item ids 7001..=7004) on `sid`.
fn season_rows(sid: plx_plex::plex::ServerId) -> Vec<plx_plex::plex::QueueRow> {
    SEASON
        .iter()
        .map(|(rk, id)| plx_plex::plex::QueueRow { item_id: *id, sid, rk: (*rk).to_owned(), ..Default::default() })
        .collect()
}

/// **A handoff's continuation is captured AFTER the outgoing stop is announced, and carries that
/// count**: the handoff stops the episode it leaves before it asks for the next one, so the window
/// re-read ([`the_window_reread_waits_for_the_outgoing_stop_within_its_bound`]) waits for it. A
/// `capture` that recorded no stop (`0`, what a continuation built from rows alone holds) would
/// read the rows while that report is still on its way, and Previous-then-Next would resume the
/// episode just left from the position it had before the viewer left it.
#[test]
#[cfg(feature = "devtriggers")]
fn a_captured_continuation_waits_for_the_stop_announced_before_it() {
    let mut ps = PlaybackSession::IDLE;
    let _guard = fresh_registry(&mut ps);
    let rows = season_rows(plx_plex::plex::ServerId::from_raw(1));
    ps.queue = rows.clone();
    ps.pq_id = "88".into();
    let outgoing = TIMELINE_STOP_FENCE.announce();
    let announced = TIMELINE_STOP_FENCE.announced();
    let continuation = crate::route::QueueContinuation::capture(&ps, &rows[0]).expect("the row is in the queue");
    outgoing.finish();
    assert!(
        continuation.stop() >= announced && announced > 0,
        "captured after the announce, it waits for it: {} < {announced}",
        continuation.stop()
    );
}

/// **A handoff re-reads the queue window only after the episode it leaves has reported where it
/// was**, and never waits longer than its bound. The rows carry each episode's `viewOffset`: read
/// before the outgoing `stopped` report lands, the episode just left would resume from the position
/// it had before the viewer left it when stepped back to (Previous, then Next).
#[test]
#[cfg(feature = "devtriggers")]
fn the_window_reread_waits_for_the_outgoing_stop_within_its_bound() {
    let fx = Fx::new(ShowQueue::Honoured);
    assert!(plx_net::net::global_init());
    let sid = plx_plex::plex::register_for_test("predecessor-queue", "127.0.0.1", fx.pms.port, "token", "queue-client");
    let client = plx_plex::plex::client_for(sid).expect("the registered server");
    let rows = season_rows(sid);
    let continuation = crate::route::QueueContinuation::from_queue("88".into(), rows.clone(), &rows[1])
        .expect("the selected row is in the queue");

    // The outgoing episode's stop is announced and still on its way.
    let stop = TIMELINE_STOP_FENCE.announce();
    let waiting = continuation.clone().awaiting(TIMELINE_STOP_FENCE.announced());
    // The clock starts HERE, before the reader exists and so before the sleep below can begin: a
    // reader the scheduler starts late (a loaded runner) must not shorten what this measures. Taken
    // inside the reader, a late start put its clock after the sleep's, and a read that did wait for
    // the stop measured under 250 ms.
    let started = std::time::Instant::now();
    let read = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let _ = waiting.refreshed_within(client, Duration::from_secs(20));
            started.elapsed()
        });
        std::thread::sleep(Duration::from_millis(250));
        assert_eq!(fx.pms.window_reads(), 0, "the window was read while the outgoing stop was in flight");
        stop.finish();
        reader.join().expect("the reader")
    });
    assert!(read >= Duration::from_millis(250), "it waited for the stop: {read:?}");
    assert_eq!(fx.pms.window_reads(), 1, "and read the window once it landed");

    // A stop that never lands costs the start the bound and nothing more.
    let stuck = TIMELINE_STOP_FENCE.announce();
    let waiting = continuation.awaiting(TIMELINE_STOP_FENCE.announced());
    let started = std::time::Instant::now();
    let _ = waiting.refreshed_within(client, Duration::from_millis(120));
    let waited = started.elapsed();
    drop(stuck);
    assert!(waited >= Duration::from_millis(120) && waited < Duration::from_secs(10), "{waited:?}");
    assert_eq!(fx.pms.window_reads(), 2, "the window is still read once the bound passed");
}
