//! **The Requests tab** (docs/seerr.md): what the household's Seerr server suggests and what this
//! account has asked it for, as rows of the shared Request card (`registry::seerr_cards`), on the
//! strip only while a Seerr address is set.
//!
//! The page is a `plx_ui::cards::Stack`: a top band that keeps the first row under the strip, one
//! shelf per [`Row`] that has cards, and a full-screen read-out when nothing came. It reads the
//! Seerr store's answers (`SearchLike::seerr`) and asks through `StoreCmd::Seerr`: every row when
//! it mounts, "My requests" again whenever it is uncovered (a request made on a title page shows on
//! the way back), and the title of each of the first [`MINE_SHOWN`] requests, because Seerr's
//! request list names no title, only its TMDB id.
//!
//! **Routing.** A screen names no sibling: a card press is a push of `ContentArg::SeerrTitle`; the
//! strip's pills and chip are `ContentReq::Tab` / `ContentReq::Account`, and BACK from the top of
//! the page is `ContentReq::ToHome`, which the application performs as the Watchlist's BACK.
use std::collections::HashMap;
use std::ffi::{CStr, CString};

use plx_data::seerr::{DiscoverRow, MediaKind, SeerrData, SeerrOp};
use plx_data::stores::seerr::{Answer, SeerrCmd, SeerrStore, Slot};
use plx_data::stores::{StoreCmd, StoreId};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, Enter, EntryId, FocusKey, FocusTarget, Fx, GroupId, Handled, InputEvent,
    InputKind, Key, LogicalState, Machine,
};
use plx_machine::present::Provenance;
use plx_platform::i18n::msg;
use plx_ui::cards::{CardEvent, Kind, RowStyle, SectionSpec, Stack, StackEvent, StackPage};
use plx_ui::consts::*;
use plx_ui::label::{HAlign, Label, VAlign};
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::screen::{DrawFrame, FocusSource, Focusable, HitSource, Link, RenderStrategy, Screen, ScreenEvent};
use plx_ui::widgets::{StatusKind, StatusOverlay};
use plx_ui::{theme, Rect};

use crate::registry::seerr_cards::{RequestCard, RequestCards};
use crate::registry::{AppFx, ContentArg, ContentReq, HomeTab, SearchLike};

/// One row of the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Row {
    /// What this account asked the server for.
    Mine,
    Discover(DiscoverRow),
}

/// The rows, top to bottom.
pub const ROWS: [Row; 6] = [
    Row::Mine,
    Row::Discover(DiscoverRow::Trending),
    Row::Discover(DiscoverRow::PopularMovies),
    Row::Discover(DiscoverRow::PopularTv),
    Row::Discover(DiscoverRow::UpcomingMovies),
    Row::Discover(DiscoverRow::UpcomingTv),
];

impl Row {
    fn ordinal(self) -> usize {
        match self {
            Row::Mine => 0,
            Row::Discover(DiscoverRow::Trending) => 1,
            Row::Discover(DiscoverRow::PopularMovies) => 2,
            Row::Discover(DiscoverRow::PopularTv) => 3,
            Row::Discover(DiscoverRow::UpcomingMovies) => 4,
            Row::Discover(DiscoverRow::UpcomingTv) => 5,
        }
    }

    fn group(self) -> GroupId {
        GroupId(1 + self.ordinal() as u32)
    }

    fn slot(self) -> Slot {
        match self {
            Row::Mine => Slot::Requests,
            Row::Discover(row) => Slot::Discover(row, 1),
        }
    }

    fn op(self) -> SeerrOp {
        match self {
            Row::Mine => SeerrOp::Requests { page: 1 },
            Row::Discover(row) => SeerrOp::Discover { row, page: 1 },
        }
    }

    fn heading(self) -> &'static CStr {
        match self {
            Row::Mine => msg::requests_row_mine_c(),
            Row::Discover(DiscoverRow::Trending) => msg::requests_row_trending_c(),
            Row::Discover(DiscoverRow::PopularMovies) => msg::requests_row_popular_movies_c(),
            Row::Discover(DiscoverRow::PopularTv) => msg::requests_row_popular_tv_c(),
            Row::Discover(DiscoverRow::UpcomingMovies) => msg::requests_row_upcoming_movies_c(),
            Row::Discover(DiscoverRow::UpcomingTv) => msg::requests_row_upcoming_tv_c(),
        }
    }
}

/// How many of this account's requests "My requests" shows, newest first: each one is a title read
/// of its own, so the row stops at the first ones rather than reading the whole list.
pub const MINE_SHOWN: usize = 12;

/// A shelf's heading band: the Person page's (title rung and the gap to the cards).
const SHELF_HEADING: f32 = TITLE_DY + CARD_DY;
/// The top band: the first row's cards stand where the Library's and the Watchlist's grids start.
const TOP: f32 = plx_ui::page_header::GRID_TOP - SHELF_HEADING;
static SHELF_STYLE: RowStyle = RowStyle::HOME;

const TOP_GROUP: GroupId = GroupId(0);
const STATUS_GROUP: GroupId = GroupId(7);
/// The rows' place while nothing has landed (`PageRegions::declare_pending`).
const PENDING_GROUP: GroupId = GroupId(8);
/// The first card's element; cards count up from here, never reused within a page.
const FIRST_CARD_ELEM: u32 = 0x1000;

/// The page's sections, in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    Top,
    Row(Row),
    Status,
}

/// Where the page stands as a whole, which decides the read-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Something is still on its way and no row has a card.
    Loading,
    /// At least one row has cards.
    Shown,
    /// Every row answered, and none has a card.
    Empty,
    /// Every row failed.
    Failed,
}

/// One shelf's cards and their elements.
#[derive(Default)]
struct Shelf {
    cards: Vec<RequestCard>,
    elems: Vec<u32>,
}

/// The cards `row` shows, from what the store holds.
fn row_cards(row: Row, seerr: &SeerrStore) -> Vec<RequestCard> {
    let answer = |slot: &Slot| seerr.answer(slot);
    match row {
        Row::Mine => match answer(&Slot::Requests) {
            Some(Answer::Ready(SeerrData::Requests(page))) => page
                .items
                .iter()
                .take(MINE_SHOWN)
                .filter_map(|r| match answer(&Slot::Detail(r.kind, r.tmdb)) {
                    Some(Answer::Ready(SeerrData::Detail(detail))) => RequestCard::of(&detail.title),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        Row::Discover(_) => match answer(&row.slot()) {
            Some(Answer::Ready(SeerrData::Titles(page))) => page.items.iter().filter_map(RequestCard::of).collect(),
            _ => Vec::new(),
        },
    }
}

/// The requests "My requests" shows whose titles have not been asked for yet.
fn titles_owed(seerr: &SeerrStore, asked: &[(MediaKind, i64)]) -> Vec<(MediaKind, i64)> {
    let Some(Answer::Ready(SeerrData::Requests(page))) = seerr.answer(&Slot::Requests) else { return Vec::new() };
    page.items
        .iter()
        .take(MINE_SHOWN)
        .map(|r| (r.kind, r.tmdb))
        .filter(|id| !asked.contains(id) && seerr.asked(&Slot::Detail(id.0, id.1)).is_none())
        .collect()
}

/// The page's state as the stack reads it.
struct Page {
    entry: EntryId,
    shelves: [Shelf; 6],
    /// Each card's element by its row and title: a title keeps its element while it stays shown.
    keys: HashMap<(Row, MediaKind, i64), u32>,
    next: u32,
    /// The groups of the rows that have cards, top to bottom: the content of [`Page::regions`].
    content: Vec<GroupId>,
    fill: Fill,
    rev: u64,
}

impl Page {
    fn new(entry: EntryId) -> Self {
        Self {
            entry,
            shelves: Default::default(),
            keys: HashMap::new(),
            next: FIRST_CARD_ELEM,
            content: Vec::new(),
            fill: Fill::Loading,
            rev: 0,
        }
    }

    fn intern(&mut self, row: Row, kind: MediaKind, tmdb: i64) -> u32 {
        let next = &mut self.next;
        *self.keys.entry((row, kind, tmdb)).or_insert_with(|| {
            let elem = *next;
            *next += 1;
            elem
        })
    }

    /// Bring every shelf up to the store. Answers whether anything the stack reads moved.
    fn sync(&mut self, seerr: Option<&SeerrStore>) -> bool {
        let mut changed = false;
        for row in ROWS {
            let cards = seerr.map(|s| row_cards(row, s)).unwrap_or_default();
            if cards == self.shelves[row.ordinal()].cards {
                continue;
            }
            let elems = cards.iter().map(|c| self.intern(row, c.kind, c.tmdb)).collect();
            self.shelves[row.ordinal()] = Shelf { cards, elems };
            changed = true;
        }
        let fill = self.fill_of(seerr);
        changed |= fill != self.fill;
        self.fill = fill;
        if changed {
            self.content = ROWS.iter().filter(|r| !self.shelves[r.ordinal()].cards.is_empty()).map(|r| r.group()).collect();
            self.rev += 1;
        }
        changed
    }

    fn fill_of(&self, seerr: Option<&SeerrStore>) -> Fill {
        if self.shelves.iter().any(|s| !s.cards.is_empty()) {
            return Fill::Shown;
        }
        let Some(seerr) = seerr else { return Fill::Loading };
        let answers: Vec<Option<&Answer>> = ROWS.iter().map(|r| seerr.answer(&r.slot())).collect();
        if answers.iter().all(|a| matches!(a, Some(Answer::Failed(_)))) {
            Fill::Failed
        } else if answers.iter().all(|a| matches!(a, Some(Answer::Ready(_) | Answer::Failed(_)))) && !self.titles_pending(seerr) {
            Fill::Empty
        } else {
            Fill::Loading
        }
    }

    /// A title "My requests" waits for is still on its way.
    fn titles_pending(&self, seerr: &SeerrStore) -> bool {
        let Some(Answer::Ready(SeerrData::Requests(page))) = seerr.answer(&Slot::Requests) else { return false };
        page.items.iter().take(MINE_SHOWN).any(|r| !matches!(seerr.answer(&Slot::Detail(r.kind, r.tmdb)),
            Some(Answer::Ready(_) | Answer::Failed(_))))
    }

    fn regions(&self) -> PageRegions<'_> {
        PageRegions {
            header: None,
            content: &self.content,
            rail: None,
            back_walks_header: false,
            arrival: Arrival::Content,
            pending: (self.fill == Fill::Loading).then_some(PENDING_GROUP),
        }
    }

    /// The card `elem` stands for.
    fn card_of(&self, elem: u32) -> Option<&RequestCard> {
        self.shelves.iter().find_map(|s| s.elems.iter().position(|e| *e == elem).and_then(|i| s.cards.get(i)))
    }

    fn read_out(&self) -> Option<&'static str> {
        match self.fill {
            Fill::Failed => Some(msg::seerr_title_unreachable()),
            Fill::Empty => Some(msg::requests_empty()),
            Fill::Loading | Fill::Shown => None,
        }
    }
}

/// A household server whose photo proxy fetches the TMDB posters (the one Search's Request row
/// reads).
fn art_server<H: SearchLike>(cx: &Cx<'_, H>) -> Option<u16> {
    H::search(cx).scope().sources().iter().find(|source| source.household).map(|source| source.sid.raw())
}

impl<H: SearchLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = RequestCards<'a>;

    fn revision(&self, _cx: &Cx<'_, H>) -> u64 {
        self.rev
    }

    fn sections(&self, _cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Top, Kind::Custom { height: TOP, focusable: false }, TOP_GROUP));
        for row in ROWS {
            if self.shelves[row.ordinal()].cards.is_empty() {
                continue;
            }
            out.push(
                SectionSpec::new(Sec::Row(row), Kind::Shelf { style: &SHELF_STYLE, heading: SHELF_HEADING }, row.group())
                    .seated(plx_ui::page_regions::CONTENT_HEAD_SEAT),
            );
        }
        out.push(SectionSpec::new(Sec::Status, Kind::Overlay { rect: Rect::FULL, focusable: false }, STATUS_GROUP).ranked(1));
    }

    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        out.extend(ROWS.iter().map(|r| Sec::Row(*r)));
    }

    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<RequestCards<'a>> {
        let Sec::Row(row) = k else { return None };
        let shelf = &self.shelves[row.ordinal()];
        (!shelf.cards.is_empty()).then(|| RequestCards { elems: &shelf.elems, cards: &shelf.cards, art_server: art_server(cx) })
    }

    /// A Seerr title has no item menu: a hold is not taught here.
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Sec, _elem: &u32) -> bool {
        false
    }

    fn shelf_foot(&self, _cx: &Cx<'_, H>, _k: Sec) -> f32 {
        UNDER_LABEL_AIR
    }

    fn extra_groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<plx_ui::screen::GroupSpec>) {
        self.regions().declare_pending(out);
    }

    fn draw_heading(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, lift: f32) {
        if let Sec::Row(row) = k {
            let p = f.painter.alpha(f.page_alpha);
            plx_ui::cards::draw_heading_c(p, row.heading(), c"", c"", MARGIN_X, r.y - lift, 1.0, f.measure);
        }
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, _r: Rect, _scroll: f32) {
        let (Sec::Status, Some(text)) = (k, self.read_out()) else { return };
        let Ok(text) = CString::new(text) else { return };
        let (sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
        let band = Rect::new(0.0, StatusOverlay::FULL_ANCHOR_TOP, SCR_W, sz as f32);
        let label = Label::new(text.as_ptr(), sz, ink).h(HAlign::Center).v(VAlign::CapTop);
        if bold { label.bold() } else { label }.draw(f.painter.alpha(f.page_alpha), band);
    }
}

/// The Requests page.
pub struct RequestsScreen {
    page: Page,
    stack: Stack<Sec>,
    /// The titles of "My requests" this page has asked for.
    titles_asked: Vec<(MediaKind, i64)>,
    /// The page's own arrival seat while nobody has chosen another (the Watchlist's rule): the group
    /// it last seated (`Some(None)`: nothing, while the rows are on their way), `None` once the
    /// reader acts, the page is restored, or a row holds it.
    seat: Option<Option<GroupId>>,
    /// Seats this page asked the engine for whose `Enter` has not come back yet.
    own_enters: u8,
    /// How many of [`own_enters`](Self::own_enters) were queued before the reader acted.
    stale_enters: u8,
}

impl RequestsScreen {
    /// The heartbeat word, byte-identical to the route word (`app/words.rs`).
    pub const WORD: &'static str = "requests";

    pub const SHAPE: &'static str = "RequestsScreen{entry:u32,next:u32,fill:u32,rows:[[{tv:bool,tmdb:u64,elem:u32}];6],titles_asked:[{tv:bool,tmdb:u64}],seat:Option<Option<u32>>,own_enters:u8,stale_enters:u8,stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Shelf]}}";

    pub fn new(entry: EntryId) -> Self {
        let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
        Self {
            page: Page::new(entry),
            stack: Stack::new(entry).reveal_on_move(true).clipped(Rect::new(0.0, floor, SCR_W, SCR_H - floor)),
            titles_asked: Vec::new(),
            seat: None,
            own_enters: 0,
            stale_enters: 0,
        }
    }

    /// Where the page stands as a whole.
    pub fn fill(&self) -> Fill {
        self.page.fill
    }

    /// The cards row `row` shows.
    pub fn row(&self, row: Row) -> &[RequestCard] {
        &self.page.shelves[row.ordinal()].cards
    }

    /// The element of card `i` of `row`.
    pub fn elem_at(&self, row: Row, i: usize) -> Option<u32> {
        self.page.shelves[row.ordinal()].elems.get(i).copied()
    }

    fn ask<H: SearchLike>(slot: Slot, op: SeerrOp, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))));
    }

    fn ask_rows<H: SearchLike>(fx: &mut Effects<'_, H>) {
        for row in ROWS {
            Self::ask(row.slot(), row.op(), fx);
        }
    }

    /// Bring the page up to the store, and ask for the titles "My requests" still lacks.
    fn sync<H: SearchLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let seerr = H::seerr(cx);
        if let Some(seerr) = seerr {
            for (kind, tmdb) in titles_owed(seerr, &self.titles_asked) {
                self.titles_asked.push((kind, tmdb));
                Self::ask(Slot::Detail(kind, tmdb), SeerrOp::Detail { kind, tmdb }, fx);
            }
        }
        if self.page.sync(seerr) {
            fx.invalidate(Provenance::Input);
        }
    }

    /// Send `req` to the application (a screen names no sibling).
    fn request<H: SearchLike>(req: ContentReq, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Content(req)));
        fx.invalidate(Provenance::Input);
    }

    /// **Seat the arrival on the first row**, or nothing while the rows are on their way
    /// (`PageRegions::unseat`), as the Watchlist does.
    fn seat_first<H: SearchLike>(&mut self, fx: &mut Effects<'_, H>) {
        let regions = self.page.regions();
        let want = regions.first_seat();
        let target = want.map(FocusTarget::FirstInGroupAnimated).or_else(|| regions.unseat());
        self.seat = want.is_none().then_some(want);
        if let Some(target) = target {
            self.seat_own(target, fx);
        }
    }

    /// Re-seat the arrival when what the page can offer has moved, while it is still the page's own.
    fn follow_seat<H: SearchLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(seated) = self.seat else { return };
        if seated != self.page.regions().first_seat() {
            self.seat_first(fx);
        }
    }

    fn seat_own<H: SearchLike>(&mut self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        self.own_enters = self.own_enters.saturating_add(1);
        fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    /// **BACK**, decided by [`PageRegions::back`]: a scrolled page returns to its first row, and from
    /// the top it leaves for Home.
    fn back<H: SearchLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let focus = cx.focus.current.filter(|key| key.entry == self.page.entry);
        let regions = self.page.regions();
        let at = focus.and_then(|key| regions.region_of(key.elem, Focusable::<H>::group_of(self, &key.elem, cx)));
        let first = self.page.content.first().copied();
        match regions.back(at, self.stack.target() > 0.5) {
            BackStep::Top | BackStep::Content => match first {
                Some(group) => self.seat_own(FocusTarget::FirstInGroupAnimated(group), fx),
                None => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            },
            BackStep::Header | BackStep::Nav => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            BackStep::Leave => Self::request(ContentReq::ToHome, fx),
        }
        Handled::Yes
    }

    /// A press on the shared strip: the other tabs and the chip are requests; the Requests pill is
    /// the page the user is already on.
    fn strip_press(elem: u32) -> Option<Option<ContentReq>> {
        use plx_ui::widgets::{
            TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_REQUESTS, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST,
        };
        match elem {
            TAB_KEY_HOME => Some(Some(ContentReq::Tab(HomeTab::Home))),
            TAB_KEY_MOVIES => Some(Some(ContentReq::Tab(HomeTab::Movies))),
            TAB_KEY_SHOWS => Some(Some(ContentReq::Tab(HomeTab::Shows))),
            TAB_KEY_SEARCH => Some(Some(ContentReq::Tab(HomeTab::Search))),
            TAB_KEY_WATCHLIST => Some(Some(ContentReq::Tab(HomeTab::Watchlist))),
            TAB_KEY_CHIP => Some(Some(ContentReq::Account)),
            TAB_KEY_REQUESTS => Some(None),
            _ => None,
        }
    }

    /// An element activated (a pointer's click, a committed press): a card opens its title, and
    /// the strip's pills and chip are requests.
    fn activate<H: SearchLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) -> Handled {
        if self.page.card_of(elem).is_some() {
            self.open(elem, fx);
            return Handled::Yes;
        }
        match Self::strip_press(elem) {
            Some(Some(req)) => Self::request(req, fx),
            Some(None) => {}
            None => return Handled::No,
        }
        Handled::Yes
    }

    /// A card pressed: its title's page.
    fn open<H: SearchLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        if let Some(card) = self.page.card_of(elem) {
            let arg = ContentArg::SeerrTitle { kind: card.kind, tmdb: card.tmdb };
            Self::request(ContentReq::Push(arg), fx);
        }
    }

    fn view<H: SearchLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }
}

plx_ui::focusable_via_view!(RequestsScreen, H: [SearchLike], view);

impl LogicalState for RequestsScreen {
    fn write(&self, c: &mut Canon) {
        let page = &self.page;
        c.u32(page.entry.0).u32(page.next).u32(page.fill as u32);
        for shelf in &page.shelves {
            c.seq(shelf.cards.len());
            for (card, elem) in shelf.cards.iter().zip(&shelf.elems) {
                c.bool(card.kind == MediaKind::Tv).u64(card.tmdb as u64).u32(*elem);
            }
        }
        c.seq(self.titles_asked.len());
        for (kind, tmdb) in &self.titles_asked {
            c.bool(*kind == MediaKind::Tv).u64(*tmdb as u64);
        }
        c.option(self.seat, |c, seat| {
            c.option(seat, |c, group| {
                c.u32(group.0);
            });
        });
        c.u32(self.own_enters as u32).u32(self.stale_enters as u32);
        self.stack.write(c);
    }

    fn probe(&self, out: &mut String) {
        let rows: Vec<String> = self.page.shelves.iter().map(|s| s.cards.len().to_string()).collect();
        out.push_str(&format!("requests fill={:?} rows={}", self.page.fill, rows.join(",")));
    }
}

impl<H: SearchLike> Machine<H> for RequestsScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(_, card)) => Some(card),
            _ => None,
        };
        // The reader acting at the seat makes it theirs, claimed on the event (the Watchlist's rule).
        if let ScreenEvent::Input(InputEvent { kind, .. }) = ev {
            if matches!(kind, InputKind::Key { edge: Edge::Down, .. } | InputKind::Pointer { .. }
                | InputKind::Click { .. } | InputKind::Drag { .. } | InputKind::Wheel { .. }) {
                self.seat = None;
                self.stale_enters = self.own_enters;
            }
        }
        match ev {
            ScreenEvent::Mount => {
                Self::ask_rows(fx);
                Handled::Yes
            }
            ScreenEvent::Tick(_) => {
                self.sync(cx, fx);
                self.follow_seat(fx);
                Handled::Yes
            }
            ScreenEvent::Enter(enter) => {
                match enter {
                    Enter::Fresh { .. } if self.own_enters > 0 => {
                        self.own_enters -= 1;
                        if self.stale_enters > 0 {
                            // The reader moved after this seat was asked for: put their focus back.
                            self.stale_enters -= 1;
                            if let Some(held) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                                self.seat_own(FocusTarget::Elem(held), fx);
                            }
                        }
                    }
                    Enter::Fresh { .. } => self.seat_first(fx),
                    Enter::Restored => {
                        self.seat = None;
                        self.stale_enters = self.own_enters;
                    }
                }
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                // A request made on a title page shows on the way back.
                Self::ask(Row::Mine.slot(), Row::Mine.op(), fx);
                self.titles_asked.clear();
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            // The strip is a `Control` group, so OK on a pill commits a press that arrives here.
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    self.open(elem, fx);
                } else if let Some(key) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                    self.activate(key.elem, fx);
                }
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                self.back(cx, fx)
            }
            _ => Handled::No,
        }
    }
}

impl<H: SearchLike> Screen<H> for RequestsScreen {
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        self.stack.view(&self.page).redraw_focused(f, focus);
    }
    fn opener_departing(&self, key: FocusKey<u32>) -> bool {
        self.stack.view(&self.page).opener_departing::<H>(key)
    }
    fn name(&self) -> &'static str {
        Self::WORD
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _budget: &mut plx_ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        self.stack.view(&self.page).paint(f);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    /// The bar and the rows are one column (`PageRegions::links`).
    fn links(&self, out: &mut Vec<Link>) {
        self.page.regions().links(out);
    }
    fn holds_arrival(&self, dir: plx_ui::screen::Dir) -> bool {
        self.page.regions().holds(dir)
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
#[path = "requests_tests.rs"]
mod tests;
