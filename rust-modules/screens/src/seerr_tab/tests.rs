//! The Seerr tab (docs/seerr.md), stepped with a host whose views carry a real Seerr store fed
//! through the store's own job path (a scripted seam, no network): the two sections and the header
//! that switches them, the request rows and the order they keep, Discover's filter and its further
//! pages, the read-outs, the poll while a request moves, and what each press asks.
use super::*;
use crate::registry::{AppMsg, PageMemory};
use plx_data::search::view::SearchView;
use plx_data::seerr::{
    Availability, MediaRequest, Page as Listing, RequestStatus, SeerrError, SeerrIo, SeerrSession, SeerrUser, Title,
    TitleDetail,
};
use plx_data::stores::StoreJob;
use plx_data::watchlist::WatchlistInput;
use plx_machine::machine::{FocusRead, Host, InstanceId, MachineId, PressRead, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::ScreenArg;
use std::sync::{Arc, Mutex};

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

fn title(kind: MediaKind, tmdb: i64, status: Availability) -> Title {
    Title { kind: Some(kind), tmdb, title: format!("s{tmdb:08x}"), poster: Some("/p.jpg".into()), status, ..Default::default() }
}

fn request(id: i64, tmdb: i64, status: RequestStatus) -> MediaRequest {
    MediaRequest { id, kind: MediaKind::Movie, tmdb, status, availability: Availability::Pending, seasons: Vec::new(),
        requested_by: Some("s0000a1ex".into()), modified_by: None, created: None, updated: None, downloads: Vec::new() }
}

/// Signs in as `permissions`, and answers each op from what it holds now (a test may change it
/// between reads); `fail` fails every call.
#[derive(Default)]
struct Script {
    /// Discover row pages: `(row, page)` to its titles; the row's page count is the highest page held.
    pages: Mutex<HashMap<(DiscoverRow, u32), Vec<Title>>>,
    list: Mutex<Vec<MediaRequest>>,
    /// The request list's page size; 0 answers the whole list as one page.
    page_size: usize,
    permissions: u64,
    fail: bool,
}
impl SeerrIo for Script {
    fn sign_in(&self, _: &str, _: &str) -> Result<(SeerrSession, SeerrUser), SeerrError> {
        Ok((SeerrSession::for_test("connect.sid=x"), SeerrUser { id: 1, name: "s00000001".into(), permissions: self.permissions }))
    }
    fn run(&self, _: &str, _: &SeerrSession, _: &SeerrUser, op: &SeerrOp) -> Result<SeerrData, SeerrError> {
        if self.fail {
            return Err(SeerrError::Transport);
        }
        Ok(match op {
            SeerrOp::Discover { row, page } => {
                let pages = self.pages.lock().unwrap();
                let count = pages.keys().filter(|(r, _)| r == row).map(|(_, p)| *p).max().unwrap_or(1);
                SeerrData::Titles(Listing { items: pages.get(&(*row, *page)).cloned().unwrap_or_default(), page: *page, pages: count })
            }
            SeerrOp::Requests { page } => {
                let list = self.list.lock().unwrap();
                let size = if self.page_size == 0 { list.len().max(1) } else { self.page_size };
                let pages = list.len().div_ceil(size).max(1) as u32;
                let items = list.iter().skip((*page as usize - 1) * size).take(size).cloned().collect();
                SeerrData::Requests(Listing { items, page: *page, pages })
            }
            SeerrOp::Detail { kind, tmdb } =>
                SeerrData::Detail(TitleDetail { title: title(*kind, *tmdb, Availability::Unknown), ..Default::default() }),
            other => panic!("the Seerr tab asked for {other:?}"),
        })
    }
}

struct Fixture {
    search: plx_data::stores::search::SearchSnapshot,
    seerr: SeerrStore,
    script: Arc<Script>,
    input: WatchlistInput,
    measure: FixtureMeasure,
    ms: u32,
}

impl Fixture {
    fn new(script: Script) -> Self {
        let script = Arc::new(script);
        let mut seerr = SeerrStore::with_io(script.clone());
        seerr.set_address("http://127.0.0.1:5055");
        Self { search: plx_data::stores::search::SearchStore::default().snapshot(), seerr, script,
            input: WatchlistInput::for_test(1), measure: FixtureMeasure, ms: 1_000 }
    }

    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, SeerrHost> {
        Cx { views: (self.search.view(), &self.seerr), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &self.measure,
            focus: FocusRead { current: focus, ..Default::default() }, press: PressRead::default(),
            owner: plx_machine::machine::InputOwner::Entry(ENTRY) }
    }

    fn step_at(&self, screen: &mut SeerrScreen, ev: ScreenEvent<SeerrHost>, focus: Option<FocusKey<u32>>) -> Vec<Stamped<SeerrHost>> {
        let cx = self.cx(focus);
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
        Machine::<SeerrHost>::step(screen, &ev, &cx, &mut fx);
        out
    }

    fn step(&self, screen: &mut SeerrScreen, ev: ScreenEvent<SeerrHost>) -> Vec<Stamped<SeerrHost>> {
        self.step_at(screen, ev, None)
    }

    fn tick(&mut self, screen: &mut SeerrScreen, ms: u32) -> Vec<Stamped<SeerrHost>> {
        self.ms = self.ms.wrapping_add(ms);
        self.step(screen, ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }))
    }

    /// Perform every Seerr command in `out` as the Bridge would, run the jobs inline and land them.
    fn serve(&mut self, out: &[Stamped<SeerrHost>]) -> Vec<SeerrCmd> {
        let mut asked = Vec::new();
        for stamped in out {
            if let Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(cmd))) = &stamped.fx {
                asked.push(cmd.clone());
                self.seerr.run(cmd.clone(), &self.input);
            }
        }
        for job in self.seerr.take_jobs() {
            let StoreJob::Seerr(job, port) = job else { unreachable!() };
            plx_data::stores::seerr::finish_seerr_job(&port, job.addr, job.req);
        }
        self.seerr.pump(self.input.profile());
        asked
    }

    /// Serve what the page asks until it asks nothing more.
    fn settle(&mut self, screen: &mut SeerrScreen) {
        for _ in 0..8 {
            let out = self.tick(screen, 16);
            if self.serve(&out).is_empty() {
                break;
            }
        }
        self.tick(screen, 16);
    }

    fn mounted(&mut self) -> SeerrScreen {
        let mut screen = SeerrScreen::new(ENTRY);
        let out = self.step(&mut screen, ScreenEvent::Mount);
        self.serve(&out);
        self.settle(&mut screen);
        screen
    }

    fn press(&mut self, screen: &mut SeerrScreen, elem: u32) -> Vec<Stamped<SeerrHost>> {
        let out = self.step(screen, ScreenEvent::Activate(elem));
        self.serve(&out);
        self.settle(screen);
        out
    }
}

fn slots(cmds: &[SeerrCmd]) -> Vec<Slot> {
    cmds.iter().map(|cmd| match cmd {
        SeerrCmd::Ask { slot, .. } | SeerrCmd::Refresh { slot, .. } => slot.clone(),
    }).collect()
}

fn refreshes(out: &[Stamped<SeerrHost>]) -> usize {
    out.iter().filter(|s| matches!(&s.fx, Fx::App(AppFx::Store(_, StoreCmd::Seerr(SeerrCmd::Refresh { .. }))))).count()
}

/// The one content request in `out`, named as the tests compare it.
fn one_request(out: &[Stamped<SeerrHost>]) -> String {
    let reqs: Vec<&ContentReq> = out.iter().filter_map(|s| match &s.fx { Fx::App(AppFx::Content(req)) => Some(req), _ => None }).collect();
    match reqs.as_slice() {
        [ContentReq::Push(ContentArg::SeerrTitle { kind, tmdb })] => format!("title {kind:?} {tmdb}"),
        [ContentReq::Tab(tab)] => format!("tab {tab:?}"),
        [ContentReq::Account] => "account".into(),
        [ContentReq::ToHome] => "home".into(),
        [] => "nothing".into(),
        more => format!("{} other requests", more.len()),
    }
}

fn keys(screen: &SeerrScreen, cx: &Cx<'_, SeerrHost>) -> Vec<Sec> {
    let mut sections = Vec::new();
    StackPage::<SeerrHost>::sections(&screen.page, cx, &mut sections);
    sections.iter().map(|s| s.key).collect()
}

#[test]
fn every_group_is_its_own() {
    let mut groups: Vec<GroupId> = vec![HEADER_GROUP, STATUS_GROUP, PENDING_GROUP];
    groups.extend(discover::ROWS.iter().map(|r| shelf_group(*r)));
    groups.extend((0..plx_plex::plex::seerr::REQUEST_PAGE as usize).map(row_group));
    for (i, g) in groups.iter().enumerate() {
        assert!(!groups[i + 1..].contains(g), "group {g:?} is taken twice");
    }
    for (i, row) in discover::ROWS.iter().enumerate() {
        assert_eq!(discover::ordinal(*row), i);
    }
}

#[test]
fn mounting_asks_the_list_and_every_rows_first_page() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new(Script::default());
    let mut screen = SeerrScreen::new(ENTRY);
    let out = fixture.step(&mut screen, ScreenEvent::Mount);
    let mut want = vec![Slot::Requests(1)];
    want.extend(discover::ROWS.iter().map(|row| Slot::Discover(*row, 1)));
    assert_eq!(slots(&fixture.serve(&out)), want);
    assert_eq!(screen.section(), Section::Requests, "the page opens on Requests");
}

#[test]
fn every_request_is_a_row_in_the_lists_order_and_its_title_arrives_with_its_read() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval), request(2, 32, RequestStatus::Approved)];
    let mut fixture = Fixture::new(script);
    let screen = fixture.mounted();
    assert_eq!(screen.fill(), Fill::Shown);
    let rows: Vec<(i64, String)> = screen.rows().map(|r| (r.id, r.title.clone())).collect();
    assert_eq!(rows, [(1, "s0000001f".into()), (2, "s00000020".into())]);
    let cx = fixture.cx(None);
    assert_eq!(keys(&screen, &cx), [Sec::Head, Sec::Request(screen.row_elem(0).unwrap()),
        Sec::Request(screen.row_elem(1).unwrap()), Sec::Status], "one section a row, under the header");
    assert_eq!(screen.titles_asked.len(), 2, "one title read per request, not one a frame");
}

#[test]
fn a_refresh_updates_rows_in_place_and_puts_a_new_request_on_top() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval), request(2, 32, RequestStatus::Approved)];
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    let (first, second) = (screen.row_elem(0).unwrap(), screen.row_elem(1).unwrap());
    // The server re-sorts (2 changed last), approves 1 and gains 3.
    *fixture.script.list.lock().unwrap() = vec![request(3, 33, RequestStatus::PendingApproval),
        request(2, 32, RequestStatus::Approved), request(1, 31, RequestStatus::Approved)];
    let out = fixture.step(&mut screen, ScreenEvent::Uncover);
    fixture.serve(&out);
    fixture.settle(&mut screen);
    let ids: Vec<i64> = screen.rows().map(|r| r.id).collect();
    assert_eq!(ids, [3, 1, 2], "the new one on top, the others where they stood");
    assert_eq!((screen.row_elem(1), screen.row_elem(2)), (Some(first), Some(second)), "each row keeps its element");
    assert_eq!(screen.rows().nth(1).unwrap().chip, msg::seerr_chip_approved(), "updated in place");
}

#[test]
fn the_header_switches_sections_in_place_and_its_stops_follow_the_section() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    script.pages.lock().unwrap().insert((DiscoverRow::Trending, 1), vec![title(MediaKind::Tv, 22, Availability::Unknown)]);
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval)];
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    let elems = |s: &SeerrScreen| (0..3).filter_map(|n| StackPage::<SeerrHost>::plain_elem(&s.page, Sec::Head, n)).collect::<Vec<_>>();
    assert_eq!(elems(&screen), [REQUESTS_ELEM, DISCOVER_ELEM]);
    assert_eq!(screen.page.header_row(&FixtureMeasure).elems(), elems(&screen), "the row lays out the stops the page walks");
    fixture.press(&mut screen, DISCOVER_ELEM);
    assert_eq!(screen.section(), Section::Discover);
    assert_eq!(elems(&screen), [REQUESTS_ELEM, DISCOVER_ELEM, AVAILABLE_ELEM]);
    assert_eq!(screen.page.header_row(&FixtureMeasure).elems(), elems(&screen));
    let cx = fixture.cx(None);
    assert_eq!(keys(&screen, &cx), [Sec::Head, Sec::Shelf(DiscoverRow::Trending), Sec::Status]);
    let out = fixture.press(&mut screen, REQUESTS_ELEM);
    assert_eq!(screen.section(), Section::Requests);
    assert_eq!(refreshes(&out), 1, "choosing Requests reads the list again");
    assert_eq!(StackPage::<SeerrHost>::plain_step(&screen.page, Sec::Head, 0, Dir::Right), Some(1));
    assert_eq!(StackPage::<SeerrHost>::plain_step(&screen.page, Sec::Head, 1, Dir::Right), None, "no wrap");
}

#[test]
fn discover_leaves_out_what_the_server_holds_until_it_is_asked_to_show_it() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    script.pages.lock().unwrap().insert((DiscoverRow::Trending, 1), vec![
        title(MediaKind::Movie, 21, Availability::Available),
        title(MediaKind::Tv, 22, Availability::PartlyAvailable),
        title(MediaKind::Movie, 23, Availability::Unknown),
    ]);
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    fixture.press(&mut screen, DISCOVER_ELEM);
    let ids = |s: &SeerrScreen| s.shelf(DiscoverRow::Trending).iter().map(|c| c.tmdb).collect::<Vec<_>>();
    assert_eq!(ids(&screen), [22, 23], "a partly available show still has seasons to ask for");
    fixture.press(&mut screen, AVAILABLE_ELEM);
    assert_eq!(ids(&screen), [21, 22, 23]);
    fixture.press(&mut screen, AVAILABLE_ELEM);
    assert_eq!(ids(&screen), [22, 23]);
}

#[test]
fn a_row_the_filter_thins_reads_on_by_itself_and_leaves_the_rest_to_the_walk() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    {
        let mut pages = script.pages.lock().unwrap();
        for page in 1..=5u32 {
            let mut titles: Vec<Title> = (0..8).map(|i| title(MediaKind::Movie, i64::from(page * 100 + i), Availability::Available)).collect();
            titles.push(title(MediaKind::Movie, i64::from(page * 100 + 50), Availability::Unknown));
            pages.insert((DiscoverRow::PopularMovies, page), titles);
        }
    }
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    fixture.press(&mut screen, DISCOVER_ELEM);
    let shown: Vec<i64> = screen.shelf(DiscoverRow::PopularMovies).iter().map(|c| c.tmdb).collect();
    assert_eq!(shown, [150, 250, 350], "one new title a page, read by itself up to its fill");
    let row = DiscoverRow::PopularMovies;
    assert!(fixture.seerr.asked(&Slot::Discover(row, discover::MAX_FILL_PAGES + 1)).is_none(), "past that, the walk asks");
    let cx = fixture.cx(None);
    let cards = StackPage::<SeerrHost>::cards(&screen.page, &cx, Sec::Shelf(row)).expect("the shelf has cards");
    assert!(plx_ui::cards::CardSource::<SeerrHost>::more(&cards), "the shelf says there is more to walk into");
    assert_eq!(plx_ui::cards::CardSource::<SeerrHost>::page_epoch(&cards), discover::MAX_FILL_PAGES);
}

#[test]
fn the_lists_next_page_is_read_as_focus_nears_its_end() {
    let _serial = plx_base::testlock::serial();
    let script = Script { page_size: 5, ..Default::default() };
    *script.list.lock().unwrap() = (1..=12).map(|i| request(i, 100 + i, RequestStatus::PendingApproval)).collect();
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    assert_eq!(screen.rows().count(), 5, "the first page");
    let early = FocusKey { entry: ENTRY, elem: screen.row_elem(0).unwrap() };
    let out = fixture.step_at(&mut screen, ScreenEvent::Tick(Tick { ms: 2_000, dt_us: 16_667 }), Some(early));
    assert!(slots(&fixture.serve(&out)).is_empty(), "nothing more while focus is at the top");
    for _ in 0..3 {
        let last = FocusKey { entry: ENTRY, elem: screen.row_elem(screen.rows().count() - 1).unwrap() };
        let out = fixture.step_at(&mut screen, ScreenEvent::Tick(Tick { ms: 2_000, dt_us: 16_667 }), Some(last));
        fixture.serve(&out);
        fixture.settle(&mut screen);
    }
    let ids: Vec<i64> = screen.rows().map(|r| r.id).collect();
    assert_eq!(ids, (1..=12).collect::<Vec<_>>(), "every request is reachable, in the list's order");
}

#[test]
fn a_row_or_a_card_opens_its_title() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    script.pages.lock().unwrap().insert((DiscoverRow::Trending, 1), vec![title(MediaKind::Tv, 22, Availability::Unknown)]);
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval)];
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    let row = screen.row_elem(0).unwrap();
    assert_eq!(one_request(&fixture.step(&mut screen, ScreenEvent::Activate(row))), "title Movie 31");
    fixture.press(&mut screen, DISCOVER_ELEM);
    let card = screen.card_elem(DiscoverRow::Trending, 0).unwrap();
    assert_eq!(one_request(&fixture.step(&mut screen, ScreenEvent::Activate(card))), "title Tv 22");
}

#[test]
fn the_strip_leaves_by_request_and_the_seerr_pill_is_where_the_page_already_is() {
    use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_SEARCH, TAB_KEY_SEERR, TAB_KEY_WATCHLIST};
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new(Script::default());
    let mut screen = SeerrScreen::new(ENTRY);
    for (key, want) in [(TAB_KEY_HOME, "tab Home"), (TAB_KEY_SEARCH, "tab Search"), (TAB_KEY_WATCHLIST, "tab Watchlist"),
        (TAB_KEY_CHIP, "account")] {
        let out = fixture.step(&mut screen, ScreenEvent::Activate(key));
        assert_eq!(one_request(&out), want, "strip key {key:#x}");
    }
    let out = fixture.step(&mut screen, ScreenEvent::Activate(TAB_KEY_SEERR));
    assert_eq!(one_request(&out), "nothing", "the page's own pill asks for nothing");
}

#[test]
fn back_from_the_top_leaves_for_home() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new(Script::default());
    let mut screen = SeerrScreen::new(ENTRY);
    let back = InputEvent { at: Tick { ms: 0, dt_us: 0 }, source: plx_machine::machine::Source::Script,
        kind: InputKind::Key { key: Key::Back, sym: 0, wcode: 0, edge: Edge::Down, at_edge: false } };
    let out = fixture.step(&mut screen, ScreenEvent::Input(back));
    assert_eq!(one_request(&out), "home");
}

#[test]
fn the_strip_links_down_through_the_header_into_the_first_row() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval)];
    let mut fixture = Fixture::new(script);
    let screen = fixture.mounted();
    let mut links = Vec::new();
    Screen::<SeerrHost>::links(&screen, &mut links);
    let strip = plx_ui::containers::tabs::STRIP;
    assert!(links.iter().any(|l| l.from == strip && l.dir == Dir::Down && l.to == HEADER_GROUP), "{links:?}");
    assert!(links.iter().any(|l| l.from == HEADER_GROUP && l.dir == Dir::Down && l.to == row_group(0)), "{links:?}");
}

#[test]
fn each_section_has_its_own_read_out_when_it_has_nothing() {
    let _serial = plx_base::testlock::serial();
    let mut empty = Fixture::new(Script::default());
    let mut screen = empty.mounted();
    assert_eq!((screen.fill(), screen.page.read_out()), (Fill::Empty, Some(msg::seerr_requests_empty())));
    empty.press(&mut screen, DISCOVER_ELEM);
    assert_eq!((screen.fill(), screen.page.read_out()), (Fill::Empty, Some(msg::seerr_discover_empty())));
    let failed = Fixture::new(Script { fail: true, ..Default::default() }).mounted();
    assert_eq!((failed.fill(), failed.page.read_out()), (Fill::Failed, Some(msg::seerr_title_unreachable())));
    let loading = SeerrScreen::new(ENTRY);
    assert_eq!((loading.fill(), loading.page.read_out()), (Fill::Loading, None));
}

#[test]
fn the_list_is_read_again_while_a_request_moves_and_only_then() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval)];
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    assert_eq!(refreshes(&fixture.tick(&mut screen, REFRESH_MS)), 0, "nothing on its way: no poll");
    *fixture.script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::Approved)];
    let out = fixture.step(&mut screen, ScreenEvent::Uncover);
    fixture.serve(&out);
    fixture.settle(&mut screen);
    assert!(screen.rows().next().unwrap().moving);
    assert_eq!(refreshes(&fixture.tick(&mut screen, REFRESH_MS / 2)), 0, "not before its time");
    let out = fixture.tick(&mut screen, REFRESH_MS / 2);
    assert_eq!(refreshes(&out), 1);
    fixture.serve(&out);
    fixture.step(&mut screen, ScreenEvent::Cover);
    assert_eq!(refreshes(&fixture.tick(&mut screen, REFRESH_MS)), 0, "a covered page does not poll");
    fixture.press(&mut screen, DISCOVER_ELEM);
    fixture.step(&mut screen, ScreenEvent::Uncover);
    assert_eq!(refreshes(&fixture.tick(&mut screen, 2 * REFRESH_MS)), 0, "nor does Discover");
}

#[test]
fn coming_back_reads_the_list_again_and_keeps_the_rows_shown_meanwhile() {
    let _serial = plx_base::testlock::serial();
    let script = Script::default();
    *script.list.lock().unwrap() = vec![request(1, 31, RequestStatus::PendingApproval)];
    let mut fixture = Fixture::new(script);
    let mut screen = fixture.mounted();
    let out = fixture.step(&mut screen, ScreenEvent::Uncover);
    assert_eq!(refreshes(&out), 1, "a request made on a title page shows on the way back");
    for stamped in &out {
        if let Fx::App(AppFx::Store(_, StoreCmd::Seerr(cmd))) = &stamped.fx {
            fixture.seerr.run(cmd.clone(), &fixture.input);
        }
    }
    fixture.tick(&mut screen, 16);
    assert_eq!(screen.rows().count(), 1, "the rows stay while the list is read again");
}
