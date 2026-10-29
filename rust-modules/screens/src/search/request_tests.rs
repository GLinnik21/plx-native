//! The Request row (docs/seerr.md), stepped with a host whose views carry a real Seerr store fed
//! through the store's own job path (a canned seam, no network): what the row shows, for which
//! query, where it sits in the focus graph, and what OK on a card asks for.
use super::*;
use crate::registry::AppMsg;
use plx_data::search::view::SearchView;
use plx_data::seerr::{Availability, MediaKind, Page, SeerrData, SeerrError, SeerrIo, SeerrOp, SeerrSession, SeerrUser, Title};
use plx_data::stores::seerr::{Slot, SeerrStore};
use plx_data::stores::StoreJob;
use plx_data::watchlist::WatchlistInput;
use plx_machine::machine::{FocusRead, Host, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::ScreenArg;
use std::sync::Arc;

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> plx_machine::machine::Chrome { plx_machine::machine::Chrome::None }
    fn id(&self) -> plx_machine::machine::ScreenId { plx_machine::machine::ScreenId(1) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, _: &Self) -> bool { true }
}

struct SeerrHost;
impl Host for SeerrHost {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = (SearchView<'a>, &'a SeerrStore);
    type Init = Arg;
    type Memory = PageMemory;
}
impl SearchLike for SeerrHost {
    fn search<'a>(cx: &Cx<'a, Self>) -> SearchView<'a> { cx.views.0 }
    fn seerr<'a>(cx: &Cx<'a, Self>) -> Option<&'a SeerrStore> { Some(cx.views.1) }
}

const ENTRY: EntryId = EntryId(64);
const INSTANCE: InstanceId = InstanceId(65);

/// Signs in, and answers every search with `titles`.
struct Canned(Vec<Title>);
impl SeerrIo for Canned {
    fn sign_in(&self, _: &str, _: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        Ok((SeerrSession::for_test("connect.sid=x"), SeerrUser { id: 1, name: "s00000001".into() }))
    }
    fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, _: &SeerrOp) -> Result<SeerrData, SeerrError> {
        Ok(SeerrData::Titles(Page { items: self.0.clone(), page: 1, pages: 1 }))
    }
}

fn title(kind: MediaKind, tmdb: i64, status: Availability) -> Title {
    Title { kind: Some(kind), tmdb, title: format!("s{tmdb:08x}"), status, poster: Some("/p.jpg".into()), ..Default::default() }
}

/// A Seerr store that answered `query` with `titles`, through its own job path.
fn answered(query: &str, titles: Vec<Title>) -> SeerrStore {
    let mut store = SeerrStore::with_io(Arc::new(Canned(titles)));
    store.set_address("http://127.0.0.1:5055");
    let input = WatchlistInput::for_test(1);
    store.ask(Slot::Search, SeerrOp::Search { query: query.into(), page: 1 }, &input);
    for job in store.take_jobs() {
        let StoreJob::Seerr(job, port) = job else { unreachable!() };
        plx_data::stores::seerr::finish_seerr_job(&port, job.addr, job.req);
    }
    store.pump(input.profile());
    store
}

struct Fixture {
    search: plx_data::stores::search::SearchSnapshot,
    seerr: SeerrStore,
    measure: FixtureMeasure,
}

impl Fixture {
    fn new(query: &str, seerr: SeerrStore) -> Self {
        let mut store = plx_data::stores::search::SearchStore::default();
        store.run(SearchCmd::SetQuery(query.into()));
        let search = store.snapshot();
        Self { search, seerr, measure: FixtureMeasure }
    }
    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, SeerrHost> {
        Cx { views: (self.search.view(), &self.seerr), tick: Tick::default(), measure: &self.measure,
            focus: FocusRead { current: focus, ..Default::default() }, press: PressRead::default(),
            owner: InputOwner::Entry(ENTRY) }
    }
    fn screen(&self) -> SearchScreen {
        let mut screen = SearchScreen::new(ENTRY, INSTANCE);
        self.step(&mut screen, ScreenEvent::Mount);
        self.step(&mut screen, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
        screen
    }
    fn step(&self, screen: &mut SearchScreen, ev: ScreenEvent<SeerrHost>) -> Vec<Stamped<SeerrHost>> {
        let cx = self.cx(Some(FocusKey { entry: ENTRY, elem: FIELD }));
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
        Machine::<SeerrHost>::step(screen, &ev, &cx, &mut fx);
        out
    }
}

#[test]
fn the_row_shows_what_seerr_found_for_this_query_less_what_the_household_holds() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new("wallace", answered("wallace", vec![
        title(MediaKind::Movie, 11, Availability::Unknown),
        title(MediaKind::Tv, 12, Availability::Pending),
        title(MediaKind::Movie, 13, Availability::Available),
        title(MediaKind::Tv, 14, Availability::PartlyAvailable),
    ]));
    let screen = fixture.screen();
    let shown: Vec<(MediaKind, i64)> = screen.request.iter().map(|card| (card.kind, card.tmdb)).collect();
    assert_eq!(shown, [(MediaKind::Movie, 11), (MediaKind::Tv, 12), (MediaKind::Tv, 14)],
        "a title already on the household's servers is the result rows' to show");
    assert_eq!(screen.request_elems.len(), 3);
    assert_eq!(screen.request[0].poster.as_deref(), Some("https://image.tmdb.org/t/p/w342/p.jpg"));
    let mut sections = Vec::new();
    StackPage::<SeerrHost>::sections(&screen, &fixture.cx(None), &mut sections);
    assert!(sections.iter().any(|s| matches!(s.key, Sec::Request(_))), "the row is a section of the page");
}

#[test]
fn an_answer_for_another_term_is_not_shown() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new("gromit", answered("wallace", vec![title(MediaKind::Movie, 11, Availability::Unknown)]));
    let screen = fixture.screen();
    assert!(screen.request.is_empty() && !screen.has_request());
}

#[test]
fn the_row_stands_under_the_field_when_the_servers_found_nothing_and_ok_opens_the_title() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new("wallace", answered("wallace", vec![title(MediaKind::Tv, 12, Availability::Unknown)]));
    let mut screen = fixture.screen();
    let mut links = Vec::new();
    Screen::<SeerrHost>::links(&screen, &mut links);
    assert!(links.iter().any(|l| l.from == FIELD_GROUP && l.dir == Dir::Down && l.to == REQUEST_GROUP),
        "with no result rows, DOWN from the field reaches the Request row");
    let elem = screen.request_elems[0];
    let out = fixture.step(&mut screen, ScreenEvent::Activate(elem));
    assert!(out.iter().any(|s| matches!(&s.fx,
        Fx::App(AppFx::Search(SearchReq::SeerrTitle { kind: MediaKind::Tv, tmdb: 12 })))),
        "OK on a card opens that title's Seerr page");
}

#[test]
fn with_no_seerr_the_page_has_no_request_row() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new("wallace", SeerrStore::default());
    let screen = fixture.screen();
    assert!(!screen.has_request());
    let mut sections = Vec::new();
    StackPage::<SeerrHost>::sections(&screen, &fixture.cx(None), &mut sections);
    assert!(!sections.iter().any(|s| matches!(s.key, Sec::Request(_))));
}
