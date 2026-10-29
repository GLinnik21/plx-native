//! The Requests tab (docs/seerr.md), stepped with a host whose views carry a real Seerr store fed
//! through the store's own job path (a scripted seam, no network): what each row shows, the titles
//! "My requests" reads, the read-outs, the column the strip links into, and what each press asks.
use super::*;
use crate::registry::{AppMsg, PageMemory};
use plx_data::search::view::SearchView;
use plx_data::seerr::{Availability, MyRequest, Page as Listing, RequestStatus, SeerrError, SeerrIo, SeerrSession,
    SeerrUser, Title, TitleDetail};
use plx_data::stores::StoreJob;
use plx_data::watchlist::WatchlistInput;
use plx_machine::machine::{FocusRead, Host, InstanceId, MachineId, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::{Dir, ScreenArg};
use std::sync::Arc;

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> plx_machine::machine::Chrome { plx_machine::machine::Chrome::TabBar }
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

const ENTRY: EntryId = EntryId(80);
const INSTANCE: InstanceId = InstanceId(81);

fn title(kind: MediaKind, tmdb: i64) -> Title {
    Title { kind: Some(kind), tmdb, title: format!("s{tmdb:08x}"), poster: Some("/p.jpg".into()), ..Default::default() }
}

fn request(kind: MediaKind, tmdb: i64) -> MyRequest {
    MyRequest { id: tmdb, kind, tmdb, status: RequestStatus::PendingApproval, availability: Availability::Pending,
        seasons: Vec::new() }
}

/// Signs in, and answers each op from what it was built with; `fail` fails every one.
#[derive(Default)]
struct Script {
    trending: Vec<Title>,
    mine: Vec<MyRequest>,
    fail: bool,
}
impl SeerrIo for Script {
    fn sign_in(&self, _: &str, _: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        Ok((SeerrSession::for_test("connect.sid=x"), SeerrUser { id: 1, name: "s00000001".into(), permissions: 0 }))
    }
    fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        if self.fail {
            return Err(SeerrError::Transport);
        }
        let page = |items: Vec<Title>| SeerrData::Titles(Listing { items, page: 1, pages: 1 });
        Ok(match op {
            SeerrOp::Discover { row: DiscoverRow::Trending, .. } => page(self.trending.clone()),
            SeerrOp::Discover { .. } => page(Vec::new()),
            SeerrOp::MyRequests { .. } => SeerrData::Requests(Listing { items: self.mine.clone(), page: 1, pages: 1 }),
            SeerrOp::Detail { kind, tmdb } => SeerrData::Detail(TitleDetail { title: title(*kind, *tmdb), ..Default::default() }),
            other => panic!("the Requests page asked for {other:?}"),
        })
    }
}

struct Fixture {
    search: plx_data::stores::search::SearchSnapshot,
    seerr: SeerrStore,
    input: WatchlistInput,
    measure: FixtureMeasure,
}

impl Fixture {
    fn new(script: Script) -> Self {
        let mut seerr = SeerrStore::with_io(Arc::new(script));
        seerr.set_address("http://127.0.0.1:5055");
        Self { search: plx_data::stores::search::SearchStore::default().snapshot(), seerr,
            input: WatchlistInput::for_test(1), measure: FixtureMeasure }
    }

    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, SeerrHost> {
        Cx { views: (self.search.view(), &self.seerr), tick: Tick::default(), measure: &self.measure,
            focus: FocusRead { current: focus, ..Default::default() }, press: PressRead::default(),
            owner: plx_machine::machine::InputOwner::Entry(ENTRY) }
    }

    fn step(&self, screen: &mut RequestsScreen, ev: ScreenEvent<SeerrHost>) -> Vec<Stamped<SeerrHost>> {
        let cx = self.cx(None);
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
        Machine::<SeerrHost>::step(screen, &ev, &cx, &mut fx);
        out
    }

    /// Perform every Seerr ask in `out` as the Bridge would, run the jobs inline and land them.
    fn serve(&mut self, out: &[Stamped<SeerrHost>]) -> Vec<Slot> {
        let mut asked = Vec::new();
        for stamped in out {
            if let Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))) = &stamped.fx {
                asked.push(slot.clone());
                self.seerr.ask(slot.clone(), op.clone(), &self.input);
            }
        }
        for job in self.seerr.take_jobs() {
            let StoreJob::Seerr(job, port) = job else { unreachable!() };
            plx_data::stores::seerr::finish_seerr_job(&port, job.addr, job.req);
        }
        self.seerr.pump(self.input.profile());
        asked
    }

    /// Mount the page and serve what it asks until it asks nothing more.
    fn mounted(&mut self) -> RequestsScreen {
        let mut screen = RequestsScreen::new(ENTRY);
        let out = self.step(&mut screen, ScreenEvent::Mount);
        self.serve(&out);
        for _ in 0..4 {
            let out = self.step(&mut screen, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
            if self.serve(&out).is_empty() {
                break;
            }
        }
        self.step(&mut screen, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
        screen
    }
}

fn content_reqs(out: &[Stamped<SeerrHost>]) -> Vec<&ContentReq> {
    out.iter().filter_map(|s| match &s.fx { Fx::App(AppFx::Content(req)) => Some(req), _ => None }).collect()
}

/// The one content request in `out`, named as the tests compare it.
fn one_request(out: &[Stamped<SeerrHost>]) -> String {
    match content_reqs(out).as_slice() {
        [ContentReq::Push(ContentArg::SeerrTitle { kind, tmdb })] => format!("title {kind:?} {tmdb}"),
        [ContentReq::Tab(tab)] => format!("tab {tab:?}"),
        [ContentReq::Account] => "account".into(),
        [ContentReq::ToHome] => "home".into(),
        [] => "nothing".into(),
        more => format!("{} other requests", more.len()),
    }
}

#[test]
fn every_row_has_its_own_group_and_slot() {
    let groups: Vec<GroupId> = ROWS.iter().map(|r| r.group()).collect();
    for (i, g) in groups.iter().enumerate() {
        assert!(!groups[i + 1..].contains(g), "row {i} shares a group");
        assert!(![TOP_GROUP, STATUS_GROUP, PENDING_GROUP].contains(g), "row {i} takes a fixed group");
    }
    for (i, r) in ROWS.iter().enumerate() {
        assert_eq!(r.ordinal(), i, "ROWS is in ordinal order");
        assert!(!ROWS[i + 1..].iter().any(|o| o.slot() == r.slot()), "row {i} shares a slot");
    }
}

#[test]
fn mounting_asks_every_row_once() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new(Script::default());
    let mut screen = RequestsScreen::new(ENTRY);
    let out = fixture.step(&mut screen, ScreenEvent::Mount);
    let asked: Vec<Slot> = out.iter().filter_map(|s| match &s.fx {
        Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, .. }))) => Some(slot.clone()),
        _ => None,
    }).collect();
    assert_eq!(asked, ROWS.iter().map(|r| r.slot()).collect::<Vec<_>>());
}

#[test]
fn my_requests_show_their_titles_once_each_title_is_read() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new(Script {
        trending: vec![title(MediaKind::Movie, 21), title(MediaKind::Tv, 22)],
        mine: vec![request(MediaKind::Tv, 31), request(MediaKind::Movie, 32)],
        ..Default::default()
    });
    let screen = fixture.mounted();
    assert_eq!(screen.fill(), Fill::Shown);
    let ids = |row| screen.row(row).iter().map(|c| (c.kind, c.tmdb)).collect::<Vec<_>>();
    assert_eq!(ids(Row::Mine), [(MediaKind::Tv, 31), (MediaKind::Movie, 32)],
        "each request is drawn by the title its own read brought back, in the list's order");
    assert_eq!(ids(Row::Discover(DiscoverRow::Trending)), [(MediaKind::Movie, 21), (MediaKind::Tv, 22)]);
    assert!(screen.row(Row::Discover(DiscoverRow::PopularMovies)).is_empty());
    assert_eq!(screen.titles_asked.len(), 2, "one title read per request, not one a frame");
    let mut sections = Vec::new();
    StackPage::<SeerrHost>::sections(&screen.page, &fixture.cx(None), &mut sections);
    let rows: Vec<Sec> = sections.iter().map(|s| s.key).filter(|k| matches!(k, Sec::Row(_))).collect();
    assert_eq!(rows, [Sec::Row(Row::Mine), Sec::Row(Row::Discover(DiscoverRow::Trending))],
        "a row with no cards is no section");
}

#[test]
fn my_requests_stop_at_the_first_few() {
    let _serial = plx_base::testlock::serial();
    let mine = (0..MINE_SHOWN as i64 + 5).map(|i| request(MediaKind::Movie, 100 + i)).collect();
    let mut fixture = Fixture::new(Script { mine, ..Default::default() });
    let screen = fixture.mounted();
    assert_eq!(screen.row(Row::Mine).len(), MINE_SHOWN);
    assert_eq!(screen.titles_asked.len(), MINE_SHOWN, "no title read past the row's end");
}

#[test]
fn a_card_keeps_its_element_while_its_row_stays_and_a_press_opens_its_title() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new(Script { trending: vec![title(MediaKind::Tv, 22)], ..Default::default() });
    let mut screen = fixture.mounted();
    let row = Row::Discover(DiscoverRow::Trending);
    let elem = screen.elem_at(row, 0).expect("the card has an element");
    fixture.step(&mut screen, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }));
    assert_eq!(screen.elem_at(row, 0), Some(elem), "a frame with nothing new keeps the element");
    let out = fixture.step(&mut screen, ScreenEvent::Activate(elem));
    assert_eq!(one_request(&out), "title Tv 22");
}

#[test]
fn the_strip_leaves_by_request_and_the_requests_pill_is_where_the_page_already_is() {
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_REQUESTS, TAB_KEY_SEARCH, TAB_KEY_WATCHLIST};
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new(Script::default());
    let mut screen = RequestsScreen::new(ENTRY);
    for (key, want) in [(TAB_KEY_HOME, "tab Home"), (TAB_KEY_SEARCH, "tab Search"), (TAB_KEY_WATCHLIST, "tab Watchlist"),
        (TAB_KEY_CHIP, "account")] {
        let out = fixture.step(&mut screen, ScreenEvent::Activate(key));
        assert_eq!(one_request(&out), want, "strip key {key:#x}");
    }
    let out = fixture.step(&mut screen, ScreenEvent::Activate(TAB_KEY_REQUESTS));
    assert_eq!(one_request(&out), "nothing", "the page's own pill asks for nothing");
}

#[test]
fn back_from_the_top_leaves_for_home() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new(Script::default());
    let mut screen = RequestsScreen::new(ENTRY);
    let back = InputEvent { at: Tick { ms: 0, dt_us: 0 }, source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } };
    let out = fixture.step(&mut screen, ScreenEvent::Input(back));
    assert_eq!(one_request(&out), "home");
}

#[test]
fn the_strip_links_down_into_the_first_row_with_cards() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new(Script { trending: vec![title(MediaKind::Movie, 21)], ..Default::default() });
    let screen = fixture.mounted();
    let mut links = Vec::new();
    Screen::<SeerrHost>::links(&screen, &mut links);
    let trending = Row::Discover(DiscoverRow::Trending).group();
    assert!(links.iter().any(|l| l.from == plx_ui::containers::tabs::STRIP && l.dir == Dir::Down && l.to == trending),
        "with no requests, DOWN from the strip reaches Trending: {links:?}");
}

#[test]
fn nothing_to_show_and_nothing_reachable_have_their_own_read_outs() {
    let _serial = plx_base::testlock::serial();
    let empty = Fixture::new(Script::default()).mounted();
    assert_eq!((empty.fill(), empty.page.read_out()), (Fill::Empty, Some(msg::requests_empty())));
    let failed = Fixture::new(Script { fail: true, ..Default::default() }).mounted();
    assert_eq!((failed.fill(), failed.page.read_out()), (Fill::Failed, Some(msg::seerr_title_unreachable())));
    let loading = RequestsScreen::new(ENTRY);
    assert_eq!((loading.fill(), loading.page.read_out()), (Fill::Loading, None));
}

#[test]
fn coming_back_reads_my_requests_again() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new(Script { mine: vec![request(MediaKind::Movie, 32)], ..Default::default() });
    let mut screen = fixture.mounted();
    let out = fixture.step(&mut screen, ScreenEvent::Uncover);
    assert_eq!(fixture.serve(&out), [Slot::MyRequests], "a request made on a title page shows on the way back");
}
