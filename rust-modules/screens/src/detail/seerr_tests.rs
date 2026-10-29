//! A show's missing seasons on its Detail page (docs/seerr.md): when the page asks the household's
//! Seerr server, which face the request disc wears for each answer, and what a press asks. The store
//! is a real `SeerrStore` fed through its own job path over a scripted seam (no network), published
//! to the page through the test host's `MetadataLike::seerr`.
use super::*;
use plx_data::seerr::{Availability, Season, SeerrIo, SeerrSession, SeerrUser, Title, TitleDetail};
use plx_data::stores::StoreJob;
use plx_data::watchlist::WatchlistInput;
use std::sync::{Arc, Mutex};

const TMDB: i64 = 1399;

/// Signs in, answers a show's detail with `seasons`, and a request with `request`.
struct Script {
    seasons: Vec<Availability>,
    request: Mutex<Result<SeerrData, SeerrError>>,
}

impl SeerrIo for Script {
    fn sign_in(&self, _: &str, _: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        Ok((SeerrSession::for_test("connect.sid=x"), SeerrUser { id: 1, name: "s00000001".into() }))
    }
    fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        match op {
            SeerrOp::Detail { kind: MediaKind::Tv, tmdb } => Ok(SeerrData::Detail(TitleDetail {
                title: Title { kind: Some(MediaKind::Tv), tmdb: *tmdb, ..Default::default() },
                seasons: self.seasons.iter().enumerate()
                    .map(|(i, &status)| Season { number: i as i32 + 1, episodes: Some(10), status }).collect(),
            })),
            SeerrOp::Request { .. } => self.request.lock().unwrap().clone(),
            other => panic!("the Detail page asked for {other:?}"),
        }
    }
}

/// The test host's Seerr store: one with `script` at an address, or none at all.
fn seerr_at(script: Option<Script>) {
    *test_seerr() = script.map(|script| {
        let mut store = SeerrStore::with_io(Arc::new(script));
        store.set_address("http://127.0.0.1:5055");
        store
    });
}

fn show(tmdb: Option<i64>) -> Detail {
    Detail { tmdb, ..detail(ServerId::UNSET, "show") }
}

fn seerr_asks(effects: &[plx_machine::machine::Stamped<TestHost>]) -> Vec<(Slot, SeerrOp)> {
    effects.iter().filter_map(|s| match &s.fx {
        Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))) => Some((slot.clone(), op.clone())),
        _ => None,
    }).collect()
}

/// Perform every Seerr ask in `effects` as the Bridge would, run the jobs inline and land them.
fn serve(effects: &[plx_machine::machine::Stamped<TestHost>]) {
    let input = WatchlistInput::for_test(1);
    let store = test_seerr().as_mut().expect("a store to serve");
    for (slot, op) in seerr_asks(effects) {
        store.ask(slot, op, &input);
    }
    for job in store.take_jobs() {
        let StoreJob::Seerr(job, port) = job else { unreachable!() };
        plx_data::stores::seerr::finish_seerr_job(&port, job.addr, job.req);
    }
    store.pump(input.profile());
}

/// Open the show's page with Seerr answering `seasons`, and serve what it asks.
fn opened(guard: &plx_base::testlock::Serial, seasons: &[Availability], request: Result<SeerrData, SeerrError>) -> DetailScreen {
    seerr_at(Some(Script { seasons: seasons.to_vec(), request: Mutex::new(request) }));
    let mut screen = bare(guard, ServerId::UNSET, "show");
    let fx = tick_page(&mut screen, None, 16);
    serve(&fx);
    tick_page(&mut screen, None, 32);
    screen
}

fn request_of(screen: &DetailScreen) -> Option<hero::RequestFace> {
    screen.hero_set(test_store().view()).request
}

/// A show whose server sent its TMDB id is asked about ONCE per page, and only while a Seerr
/// address is set. A movie, and a show without the id, are never asked about.
#[test]
fn a_show_with_a_tmdb_id_is_asked_about_once_and_only_with_an_address() {
    let guard = install(show(Some(TMDB)));
    seerr_at(Some(Script { seasons: vec![], request: Mutex::new(Ok(SeerrData::Requested)) }));
    let mut screen = bare(&guard, ServerId::UNSET, "show");
    assert_eq!(seerr_asks(&tick_page(&mut screen, None, 16)),
        [(Slot::Detail(MediaKind::Tv, TMDB), SeerrOp::Detail { kind: MediaKind::Tv, tmdb: TMDB })]);
    assert!(seerr_asks(&tick_page(&mut screen, None, 32)).is_empty(), "asked once per page");
    let mut again = bare(&guard, ServerId::UNSET, "show");
    assert_eq!(seerr_asks(&tick_page(&mut again, None, 16)).len(), 1, "a new page asks afresh");

    seerr_at(None);
    let mut unset = bare(&guard, ServerId::UNSET, "show");
    assert!(seerr_asks(&tick_page(&mut unset, None, 16)).is_empty(), "no store, no ask");
    *test_seerr() = Some(SeerrStore::with_io(Arc::new(Script { seasons: vec![], request: Mutex::new(Ok(SeerrData::Requested)) })));
    let mut no_address = bare(&guard, ServerId::UNSET, "show");
    assert!(seerr_asks(&tick_page(&mut no_address, None, 16)).is_empty(), "no address, no ask");
    assert_eq!(request_of(&no_address), None);
    drop(guard);

    seerr_at(Some(Script { seasons: vec![], request: Mutex::new(Ok(SeerrData::Requested)) }));
    let guard = install(show(None));
    let mut without_id = bare(&guard, ServerId::UNSET, "show");
    assert!(seerr_asks(&tick_page(&mut without_id, None, 16)).is_empty(), "a show with no TMDB id");
    drop(guard);
    let guard = install(Detail { tmdb: Some(TMDB), ..movie_with_guid(WL_GUID) });
    let mut movie = bare(&guard, ServerId::UNSET, "movie");
    assert!(seerr_asks(&tick_page(&mut movie, None, 16)).is_empty(), "a movie has no seasons to request");
    seerr_at(None);
}

/// Seasons nobody holds or asked for put the disc LAST in the row, on its Ask face; a show the
/// server holds or was asked for whole has no disc, and neither does one Seerr could not be asked about.
#[test]
fn missing_seasons_put_the_request_disc_last_and_nothing_missing_puts_none() {
    use Availability::*;
    let guard = install(show(Some(TMDB)));
    let screen = opened(&guard, &[Available, Unknown, Unknown], Ok(SeerrData::Requested));
    assert_eq!(request_of(&screen), Some(hero::RequestFace::Ask));
    let set = screen.hero_set(test_store().view());
    let (ctls, n) = hero::hero_ctls(set);
    assert_eq!(ctls[n - 1], hero::HeroCtl::Request, "the request disc is last");
    assert_eq!(hero::disc_verb(hero::HeroCtl::Request, set, false),
        Some((4, plx_platform::i18n::msg::seerr_title_request_seasons_c())));

    for whole in [&[Available, Available][..], &[Available, Pending, Processing][..]] {
        let screen = opened(&guard, whole, Ok(SeerrData::Requested));
        assert_eq!(request_of(&screen), None, "{whole:?}");
    }
    seerr_at(None);
}

/// A press asks for every missing season in one request; the store's answer turns the face, and a
/// sent request offers nothing more to press.
#[test]
fn a_press_requests_every_missing_season_and_the_answer_turns_the_face() {
    use Availability::*;
    let guard = install(show(Some(TMDB)));
    let mut screen = opened(&guard, &[Available, Unknown, Pending, Unknown], Ok(SeerrData::Requested));
    let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_REQUEST), Some(hero::ELEM_REQUEST));
    assert_eq!(seerr_asks(&fx), [(Slot::Request(MediaKind::Tv, TMDB),
        SeerrOp::Request { kind: MediaKind::Tv, tmdb: TMDB, seasons: vec![2, 4] })]);
    let input = WatchlistInput::for_test(1);
    let store = test_seerr().as_mut().unwrap();
    let (slot, op) = seerr_asks(&fx).remove(0);
    store.ask(slot, op, &input);
    tick_page(&mut screen, None, 48);
    assert_eq!(request_of(&screen), Some(hero::RequestFace::Sending));
    let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_REQUEST), Some(hero::ELEM_REQUEST));
    assert!(seerr_asks(&fx).is_empty(), "a request on its way is not sent twice");

    serve(&[]);
    tick_page(&mut screen, None, 64);
    assert_eq!(request_of(&screen), Some(hero::RequestFace::Sent));
    let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_REQUEST), Some(hero::ELEM_REQUEST));
    assert!(seerr_asks(&fx).is_empty(), "a sent request offers nothing more");
    seerr_at(None);
}

/// A refused request says why on the disc: somebody asked first (nothing to press), the account
/// may not (a press tries again), or no answer (a press tries again).
#[test]
fn a_refused_request_says_why_and_only_a_retryable_one_presses_again() {
    use hero::RequestFace as Face;
    let guard = install(show(Some(TMDB)));
    for (error, face) in [
        (SeerrError::AlreadyRequested, Face::AlreadyAsked),
        (SeerrError::Forbidden, Face::Refused),
        (SeerrError::Transport, Face::Failed),
    ] {
        let mut screen = opened(&guard, &[Availability::Unknown], Err(error.clone()));
        let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_REQUEST), Some(hero::ELEM_REQUEST));
        serve(&fx);
        tick_page(&mut screen, None, 48);
        assert_eq!(request_of(&screen), Some(face), "{error:?}");
        let mark = if face == Face::AlreadyAsked { plx_ui::icons::Icon::Check } else { plx_ui::icons::Icon::Plus };
        assert_eq!(face.icon(), mark, "{error:?}");
        let (_, fx) = step(&mut screen, &ScreenEvent::Activate(hero::ELEM_REQUEST), Some(hero::ELEM_REQUEST));
        assert_eq!(seerr_asks(&fx).len(), usize::from(face.presses()), "{error:?}");
    }
    seerr_at(None);
}
