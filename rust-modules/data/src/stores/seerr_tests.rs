//! The Seerr store over a scripted seam, its jobs run inline: what a slot reads before and after
//! its answer, the connection line, a superseded ask, a refused job, a new address and a new profile.
use super::*;
use crate::seerr::{Page, Season, Title};
use std::sync::Mutex;

/// Signs in with `sign_in` every time and answers `run` with the next reply.
struct Script {
    sign_in: Mutex<Result<(SeerrSession, SeerrUser), SeerrError>>,
    runs: Mutex<Vec<Result<SeerrData, SeerrError>>>,
    calls: Mutex<Vec<String>>,
}

impl Script {
    fn new(sign_in: Result<(SeerrSession, SeerrUser), SeerrError>, runs: Vec<Result<SeerrData, SeerrError>>) -> Arc<Self> {
        Arc::new(Self { sign_in: Mutex::new(sign_in), runs: Mutex::new(runs), calls: Mutex::new(Vec::new()) })
    }
}

impl SeerrIo for Script {
    fn sign_in(&self, address: &str, _: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        self.calls.lock().unwrap().push(format!("sign_in {address}"));
        self.sign_in.lock().unwrap().clone()
    }
    fn run(&self, address: &str, _: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        self.calls.lock().unwrap().push(format!("run {address} {op:?}"));
        self.runs.lock().unwrap().remove(0)
    }
}

fn signed_in() -> Result<(SeerrSession, SeerrUser), SeerrError> {
    Ok((SeerrSession::for_test("connect.sid=x"), SeerrUser { id: 4, name: "s00000004".into(), permissions: 0 }))
}

fn store(io: &Arc<Script>) -> SeerrStore {
    let mut store = SeerrStore::with_io(io.clone());
    store.set_address("http://127.0.0.1:5055");
    store
}

/// Run every emitted job inline, as the runner would on its worker.
fn run_jobs(store: &mut SeerrStore) -> usize {
    let jobs = store.take_jobs();
    let n = jobs.len();
    for job in jobs {
        match job {
            StoreJob::Seerr(job, port) => finish_seerr_job(&port, job.addr, job.req),
            other => panic!("not a Seerr job: {other:?}"),
        }
    }
    n
}

fn titles(n: usize) -> SeerrData {
    SeerrData::Titles(Page { items: (0..n).map(|i| Title { tmdb: i as i64 + 1, ..Default::default() }).collect(), page: 1, pages: 1 })
}

fn search(q: &str) -> SeerrOp {
    SeerrOp::Search { query: q.into(), page: 1 }
}

#[test]
fn no_address_is_off_and_an_ask_fails_without_a_job() {
    let io = Script::new(signed_in(), vec![]);
    let mut store = SeerrStore::with_io(io.clone());
    assert_eq!((store.connection(), store.configured()), (&Connection::Off, false));
    store.ask(Slot::Search, search("q"), &WatchlistInput::for_test(1));
    assert_eq!(store.answer(&Slot::Search), Some(&Answer::Failed(SeerrError::BadAddress)));
    assert!(!store.has_jobs());
}

#[test]
fn nobody_to_sign_in_as_asks_nothing() {
    let io = Script::new(signed_in(), vec![]);
    let mut store = store(&io);
    store.connect(&WatchlistInput::signed_out_for_test(1));
    assert_eq!(store.connection(), &Connection::Idle);
    assert!(!store.has_jobs(), "no credential, no request");
}

#[test]
fn a_profile_is_owed_one_sign_in_and_not_one_a_frame() {
    let io = Script::new(Err(SeerrError::Forbidden), vec![]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    assert!(store.needs_connect(input.profile()));
    store.connect(&input);
    assert!(!store.needs_connect(input.profile()), "asked: not owed again while it runs");
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.connection(), &Connection::NotAllowed);
    assert!(!store.needs_connect(input.profile()), "a refusal is Settings' to retry, not the frame's");
    let unsigned = WatchlistInput::signed_out_for_test(1);
    let mut other = self::store(&io);
    other.connect(&unsigned);
    assert!(!other.needs_connect(unsigned.profile()), "no account: asked once, not every frame");
    store.set_address("http://127.0.0.1:5056");
    assert!(store.needs_connect(input.profile()), "a new address is owed a sign-in");
}

#[test]
fn connect_signs_in_and_names_the_user() {
    let io = Script::new(signed_in(), vec![]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.connect(&input);
    assert_eq!(store.connection(), &Connection::Connecting);
    assert_eq!(run_jobs(&mut store), 1);
    assert!(store.pump(input.profile()));
    assert_eq!(store.connection(), &Connection::Connected { name: "s00000004".into() });
    assert_eq!(*io.calls.lock().unwrap(), ["sign_in http://127.0.0.1:5055"]);
}

#[test]
fn a_refused_or_unreachable_sign_in_says_so() {
    for (e, want) in [(SeerrError::Forbidden, Connection::NotAllowed), (SeerrError::Transport, Connection::Unreachable),
        (SeerrError::BadAddress, Connection::BadAddress), (SeerrError::Other(502), Connection::Unreachable)] {
        let io = Script::new(Err(e.clone()), vec![]);
        let mut store = store(&io);
        let input = WatchlistInput::for_test(1);
        store.connect(&input);
        run_jobs(&mut store);
        store.pump(input.profile());
        assert_eq!(store.connection(), &want, "{e:?}");
        assert_eq!(store.answer(&Slot::Connect), Some(&Answer::Failed(e)));
    }
}

#[test]
fn an_ask_signs_in_once_and_later_asks_reuse_the_session() {
    let io = Script::new(signed_in(), vec![Ok(titles(2)), Ok(titles(3))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, search("a"), &input);
    assert_eq!(store.answer(&Slot::Search), Some(&Answer::Loading));
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Search), Some(&Answer::Ready(titles(2))));
    store.ask(Slot::Search, search("ab"), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Search), Some(&Answer::Ready(titles(3))));
    let calls = io.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|c| c.starts_with("sign_in")).count(), 1, "{calls:?}");
}

#[test]
fn a_failed_call_with_a_good_session_keeps_the_connection() {
    let io = Script::new(signed_in(), vec![Ok(titles(1)), Err(SeerrError::Forbidden)]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, search("a"), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    store.ask(Slot::Request(MediaKind::Movie, 7), SeerrOp::Request { kind: MediaKind::Movie, tmdb: 7, seasons: vec![],
        options: crate::seerr::RequestOptions::default() }, &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Request(MediaKind::Movie, 7)), Some(&Answer::Failed(SeerrError::Forbidden)));
    assert!(matches!(store.connection(), Connection::Connected { .. }), "a refused request is not a lost connection");
}

#[test]
fn only_a_slots_latest_ask_is_answered() {
    let io = Script::new(signed_in(), vec![Ok(titles(1)), Ok(titles(2))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, search("a"), &input);
    let first = store.take_jobs();
    store.ask(Slot::Search, search("ab"), &input);
    let second = store.take_jobs();
    for job in second.into_iter().chain(first) {
        let StoreJob::Seerr(job, port) = job else { unreachable!() };
        finish_seerr_job(&port, job.addr, job.req);
    }
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Search), Some(&Answer::Ready(titles(1))),
        "the latest ask ran first and took the first reply; the superseded one's landing was dropped");
    assert!(!store.pump(input.profile()));
}

#[test]
fn a_refused_job_is_asked_again_after_the_wait() {
    let io = Script::new(signed_in(), vec![Ok(titles(4))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Discover(DiscoverRow::Trending, 1), SeerrOp::Discover { row: DiscoverRow::Trending, page: 1 }, &input);
    let StoreJob::Seerr(job, port) = store.take_jobs().remove(0) else { unreachable!() };
    port.refused(job.addr);
    assert!(!store.pump(input.profile()), "a refusal changes nothing a surface reads");
    // The pump that took the refusal starts the wait; each later pump counts one frame of it.
    for _ in 0..RETRY_FRAMES - 1 {
        store.pump(input.profile());
    }
    assert!(!store.has_jobs(), "not before the wait is out");
    store.pump(input.profile());
    assert_eq!(run_jobs(&mut store), 1, "asked again, once");
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Discover(DiscoverRow::Trending, 1)), Some(&Answer::Ready(titles(4))));
}

#[test]
fn a_new_address_forgets_the_old_ones_answers_and_session() {
    let io = Script::new(signed_in(), vec![Ok(titles(1)), Ok(titles(2))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, search("a"), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    store.ask(Slot::Search, search("b"), &input);
    let stale = store.take_jobs();
    store.set_address("https://seerr.example.com");
    assert_eq!((store.connection(), store.answer(&Slot::Search)), (&Connection::Idle, None));
    for job in stale {
        let StoreJob::Seerr(job, port) = job else { unreachable!() };
        finish_seerr_job(&port, job.addr, job.req);
    }
    assert!(!store.pump(input.profile()), "the old address's answer lands into nothing");
    store.ask(Slot::Search, search("c"), &input);
    run_jobs(&mut store);
    assert!(io.calls.lock().unwrap().iter().any(|c| c == "sign_in https://seerr.example.com"), "signed in at the new one");
    store.set_address("");
    assert_eq!(store.connection(), &Connection::Off);
}

#[test]
fn another_profile_forgets_the_session_and_its_landings() {
    let io = Script::new(signed_in(), vec![Ok(titles(1)), Ok(titles(2))]);
    let mut store = store(&io);
    let first = WatchlistInput::for_test(1);
    store.ask(Slot::Requests(1), SeerrOp::Requests { page: 1 }, &first);
    run_jobs(&mut store);
    let second = WatchlistInput::for_test(2);
    assert!(store.pump(second.profile()), "the landing is the old profile's");
    assert_eq!((store.connection(), store.answer(&Slot::Requests(1))), (&Connection::Idle, None));
    store.ask(Slot::Requests(1), SeerrOp::Requests { page: 1 }, &second);
    run_jobs(&mut store);
    store.pump(second.profile());
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Ready(titles(2))));
    assert_eq!(io.calls.lock().unwrap().iter().filter(|c| c.starts_with("sign_in")).count(), 2,
        "the new profile signs in for itself");
}

#[test]
fn a_refresh_keeps_the_shown_answer_until_the_new_one_lands_and_when_it_fails() {
    let io = Script::new(signed_in(), vec![Ok(titles(1)), Ok(titles(2)), Err(SeerrError::Transport)]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    let op = SeerrOp::Requests { page: 1 };
    store.run(SeerrCmd::Refresh { slot: Slot::Requests(1), op: op.clone() }, &input);
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Loading), "nothing shown yet: a plain ask");
    run_jobs(&mut store);
    store.pump(input.profile());
    store.run(SeerrCmd::Refresh { slot: Slot::Requests(1), op: op.clone() }, &input);
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Ready(titles(1))), "kept while the refresh runs");
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Ready(titles(2))), "the new answer replaces it");
    store.refresh(Slot::Requests(1), op.clone(), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Ready(titles(2))), "a failed refresh keeps it");
}

#[test]
fn a_plain_ask_over_a_refresh_in_flight_shows_its_own_outcome() {
    let io = Script::new(signed_in(), vec![Ok(titles(1)), Err(SeerrError::Transport)]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    let op = SeerrOp::Requests { page: 1 };
    store.ask(Slot::Requests(1), op.clone(), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    store.refresh(Slot::Requests(1), op.clone(), &input);
    store.ask(Slot::Requests(1), op, &input);
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Loading));
    assert_eq!(run_jobs(&mut store), 2);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Requests(1)), Some(&Answer::Failed(SeerrError::Transport)),
        "the ask is no refresh: its failure is shown");
}

#[test]
fn the_signed_in_users_permissions_say_who_decides_and_who_sees_everything() {
    let mut store = store(&Script::new(signed_in(), vec![Ok(SeerrData::Connected)]));
    assert!(!store.may_manage_requests() && !store.sees_all_requests(), "nobody signed in");
    let manager = Ok((SeerrSession::for_test("c"), SeerrUser { id: 1, name: String::new(), permissions: 16 }));
    let io = Script::new(manager, vec![]);
    store = SeerrStore::with_io(io);
    store.set_address("http://127.0.0.1:5055");
    let input = WatchlistInput::for_test(1);
    store.connect(&input);
    run_jobs(&mut store);
    store.pump(input.profile());
    assert!(store.may_manage_requests() && store.sees_all_requests());
}

#[test]
fn a_job_that_panics_still_answers() {
    struct Panics;
    impl SeerrIo for Panics {
        fn sign_in(&self, _: &str, _: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
            panic!("synthetic")
        }
        fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, _: &SeerrOp) -> Result<SeerrData, SeerrError> {
            unreachable!()
        }
    }
    let mut store = SeerrStore::with_io(Arc::new(Panics));
    store.set_address("http://127.0.0.1:5055");
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, search("a"), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    assert_eq!(store.answer(&Slot::Search), Some(&Answer::Failed(SeerrError::Transport)));
}

#[test]
fn a_typed_in_ask_waits_for_the_term_to_settle_and_a_repeat_is_nothing_new() {
    let io = Script::new(signed_in(), vec![Ok(titles(1))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.want(Slot::Search, search("a"));
    for _ in 0..SETTLE_FRAMES - 1 {
        assert!(store.take_due().is_none(), "not before the term has stood");
    }
    store.want(Slot::Search, search("ab"));
    assert!(store.take_due().is_none(), "a new term starts the wait again");
    for _ in 0..SETTLE_FRAMES - 2 {
        assert!(store.take_due().is_none());
    }
    let (slot, op) = store.take_due().expect("settled");
    assert_eq!((slot.clone(), op.clone()), (Slot::Search, search("ab")));
    store.ask(slot, op, &input);
    assert_eq!(store.asked(&Slot::Search), Some(&search("ab")));
    store.want(Slot::Search, search("ab"));
    for _ in 0..SETTLE_FRAMES * 2 {
        assert!(store.take_due().is_none(), "the term already asked is not asked again");
    }
}

#[test]
fn unwanting_a_slot_forgets_its_wait_its_ask_and_its_answer() {
    let io = Script::new(signed_in(), vec![Ok(titles(1))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, search("a"), &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    store.want(Slot::Search, search("b"));
    store.unwant(&Slot::Search);
    assert_eq!((store.answer(&Slot::Search), store.asked(&Slot::Search)), (None, None));
    for _ in 0..SETTLE_FRAMES * 2 {
        assert!(store.take_due().is_none());
    }
}

fn movie(status: Availability) -> TitleDetail {
    TitleDetail { title: Title { kind: Some(MediaKind::Movie), tmdb: 1, status, ..Default::default() }, ..Default::default() }
}

fn show(statuses: &[Availability]) -> TitleDetail {
    TitleDetail {
        title: Title { kind: Some(MediaKind::Tv), tmdb: 2, status: Availability::PartlyAvailable, ..Default::default() },
        seasons: statuses.iter().enumerate()
            .map(|(i, &status)| Season { number: i as i32 + 1, episodes: Some(8), status }).collect(),
        ..Default::default()
    }
}

#[test]
fn a_movie_nobody_asked_for_can_be_requested_and_nothing_else_can() {
    assert_eq!(offer(&movie(Availability::Unknown), None), Offer::Request(vec![]));
    for status in [Availability::Pending, Availability::Processing, Availability::Available, Availability::Blocked] {
        assert_eq!(offer(&movie(status), None), Offer::Nothing, "{status:?}");
    }
}

#[test]
fn a_show_offers_only_the_seasons_nobody_holds_or_asked_for() {
    let detail = show(&[Availability::Available, Availability::Unknown, Availability::Pending, Availability::Unknown]);
    assert_eq!(offer(&detail, None), Offer::Request(vec![2, 4]));
    assert_eq!(offer(&show(&[Availability::Available, Availability::Processing]), None), Offer::Nothing);
}

#[test]
fn a_request_in_flight_or_done_takes_the_offer_and_a_failed_one_offers_it_again() {
    let detail = movie(Availability::Unknown);
    assert_eq!(offer(&detail, Some(&Answer::Loading)), Offer::Sending);
    assert_eq!(offer(&detail, Some(&Answer::Ready(SeerrData::Requested))), Offer::Nothing);
    assert_eq!(offer(&detail, Some(&Answer::Failed(SeerrError::Transport))), Offer::Request(vec![]));
}

/// The request menu opens only for a user Seerr lets choose where a request goes, and reads the
/// servers and one server's options from their own slots.
#[test]
fn the_menu_reads_the_users_permission_and_the_servers_and_options_slots() {
    use crate::seerr::{ArrOptions, ArrServer, QualityProfile};
    let advanced = Ok((SeerrSession::for_test("connect.sid=x"),
        SeerrUser { id: 4, name: "s00000004".into(), permissions: plx_plex::plex::seerr::PERMISSION_REQUEST_ADVANCED }));
    let servers = vec![ArrServer { id: 0, name: "s00000001".into(), is_default: true, ..Default::default() }];
    let options = ArrOptions { profiles: vec![QualityProfile { id: 4, name: "s00000002".into() }], folders: vec![] };
    let io = Script::new(advanced, vec![Ok(SeerrData::Servers(servers.clone())), Ok(SeerrData::ServerOptions(options.clone()))]);
    let mut store = store(&io);
    let input = WatchlistInput::for_test(1);
    assert!(!store.may_choose_options(), "nobody is signed in yet");
    store.ask(Slot::Servers(MediaKind::Tv), SeerrOp::Servers { kind: MediaKind::Tv }, &input);
    store.ask(Slot::ServerOptions(MediaKind::Tv, 0), SeerrOp::ServerOptions { kind: MediaKind::Tv, server: 0 }, &input);
    run_jobs(&mut store);
    store.pump(input.profile());
    assert!(store.may_choose_options());
    assert_eq!(store.servers(MediaKind::Tv), Some(&servers[..]));
    assert_eq!(store.servers(MediaKind::Movie), None, "a movie's servers are another slot");
    assert_eq!(store.server_options(MediaKind::Tv, 0), Some(&options));

    let io = Script::new(signed_in(), vec![Ok(titles(0))]);
    let mut plain = self::store(&io);
    plain.ask(Slot::Search, search("a"), &input);
    run_jobs(&mut plain);
    plain.pump(input.profile());
    assert!(!plain.may_choose_options(), "a user with Request alone sends straight away");
}
