//! **The Seerr tab** (docs/seerr.md): the household's Seerr server in two sections under one header
//! row, on the strip only while a Seerr address is set.
//!
//! - **Requests**: the request list, everybody's for a user the server shows them to and the user's
//!   own otherwise, the most recently changed first: one [`plx_ui::media_row::MediaRow`] each, with
//!   who asked, when, what became of it and the download while it moves ([`rows`]). A row opens its
//!   title's Seerr page.
//! - **Discover**: the server's suggestion rows (Trending, Popular, Upcoming) as shelves of the
//!   shared Request card, with the titles already on the server left out until the header's
//!   "Available" control shows them ([`discover`]).
//!
//! The page is a `plx_ui::cards::Stack`: the header block, then the section on view (a request row
//! is a section of its own, one element tall, so the stack reveals the focused row as it reveals a
//! shelf), and a full-screen read-out when the section has nothing. It reads the Seerr store
//! (`SearchLike::seerr`) and asks through `StoreCmd::Seerr`: both sections' first reads when it
//! mounts, each request's title (the list names none), Discover's further pages while a filtered
//! row runs short, and the list again whenever the page is uncovered or Requests is chosen, and
//! every [`REFRESH_MS`] while a request on view is still on its way (a refresh keeps the rows shown
//! until the new answer lands, `SeerrCmd::Refresh`).
//!
//! **Order holds.** Rows keep the order the list first came in while the page stands: a refresh
//! updates them in place, a request new to the page goes on top (or, from a further page, after the
//! row the server puts before it), so a refresh never moves the row under the reader's focus
//! because the server re-sorted the list.
//!
//! **Routing.** A screen names no sibling: a card or row press is a push of
//! `ContentArg::SeerrTitle`; the strip's pills and chip are `ContentReq::Tab` / `ContentReq::Account`,
//! and BACK from the top of the page is `ContentReq::ToHome`, which the application performs as the
//! Watchlist's BACK.
pub mod discover;
pub mod header;
pub mod rows;

use std::collections::HashMap;
use std::ffi::CString;

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
use plx_ui::media_row::{MediaRow, Poster};
use plx_ui::page_header::SharedRow;
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::screen::{
    AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Link,
    RenderStrategy, Screen, ScreenEvent, Seat,
};
use plx_ui::widgets::{CtlPop, StatusKind, StatusOverlay, TabStrip};
use plx_ui::{theme, Rect};

use crate::registry::seerr_cards::{RequestCard, RequestCards};
use crate::registry::{AppFx, ContentArg, ContentReq, HomeTab, SearchLike};
use rows::RowView;

/// The section on view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Section {
    Requests,
    Discover,
}

/// The header's stops: the two sections, and Discover's "Available" control.
pub const REQUESTS_ELEM: u32 = 0x10;
pub const DISCOVER_ELEM: u32 = 0x11;
pub const AVAILABLE_ELEM: u32 = 0x12;
/// The first row's or card's element; they count up from here, never reused within a page.
const FIRST_ELEM: u32 = 0x1000;

const HEADER_GROUP: GroupId = GroupId(1);
const STATUS_GROUP: GroupId = GroupId(2);
/// The section's place while its first read is on its way (`PageRegions::declare_pending`).
const PENDING_GROUP: GroupId = GroupId(3);
/// Discover's shelves: `FIRST_SHELF_GROUP + ordinal`.
const FIRST_SHELF_GROUP: u32 = 4;
/// Request row `i`'s group: `FIRST_ROW_GROUP + i`.
const FIRST_ROW_GROUP: u32 = 16;

/// How often the list is read again while a request on view is still on its way: Seerr's own
/// download sync runs every minute, so half that sees each change.
pub const REFRESH_MS: u32 = 30_000;

/// A shelf's heading band: the Person page's (title rung and the gap to the cards).
const SHELF_HEADING: f32 = TITLE_DY + CARD_DY;
static SHELF_STYLE: RowStyle = RowStyle::HOME;
/// The header block: the row and the air under it; the first row or shelf stands at its foot, where
/// the Library's and the Watchlist's grids start.
const HEAD_H: f32 = plx_ui::page_header::GRID_TOP;
/// A request row's box: its poster's left edge on the page's text edge, its right edge the header's.
const ROW_X: f32 = plx_ui::page_header::TEXT_X0 - plx_ui::media_row::PAD;
const ROW_W: f32 = plx_ui::page_header::TEXT_X1 + plx_ui::media_row::PAD - ROW_X;
/// One row and the air under it.
const ROW_PITCH: f32 = plx_ui::media_row::ROW_H + theme::space::XS;

/// The page's sections, in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    Head,
    /// One request row, by its element.
    Request(u32),
    Shelf(DiscoverRow),
    Status,
}

/// Where the section on view stands as a whole, which decides the read-out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// Something is still on its way and nothing is shown.
    Loading,
    Shown,
    /// Everything answered, and nothing is left to show.
    Empty,
    Failed,
}

/// What a row or card element stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Ident {
    Request(i64),
    Card(DiscoverRow, MediaKind, i64),
}

/// One request row on view.
#[derive(Clone, Debug, PartialEq)]
struct ListRow {
    elem: u32,
    view: RowView,
}

/// One shelf's cards and their elements, and how far through the server's pages it has read.
#[derive(Default)]
struct Shelf {
    cards: Vec<RequestCard>,
    elems: Vec<u32>,
    read: u32,
    more: bool,
}

/// The page's state as the stack reads it.
struct Page {
    entry: EntryId,
    section: Section,
    show_available: bool,
    sees_all: bool,
    list: Vec<ListRow>,
    shelves: [Shelf; discover::ROWS.len()],
    keys: HashMap<Ident, u32>,
    next: u32,
    /// The groups of what the section on view shows, top to bottom: the content of [`Page::regions`].
    content: Vec<GroupId>,
    fill: Fill,
    rev: u64,
    /// The header row's motion (the shared `PageHeader` motion, held by the page that wears it).
    header_capsules: TabStrip,
    header_pop: CtlPop<1>,
    header_rows: SharedRow<header::Row>,
}

impl Page {
    fn new(entry: EntryId) -> Self {
        Self {
            entry,
            section: Section::Requests,
            show_available: false,
            sees_all: false,
            list: Vec::new(),
            shelves: Default::default(),
            keys: HashMap::new(),
            next: FIRST_ELEM,
            content: Vec::new(),
            fill: Fill::Loading,
            rev: 0,
            header_capsules: TabStrip::new(),
            header_pop: CtlPop::new(),
            header_rows: SharedRow::new(),
        }
    }

    fn intern(&mut self, ident: Ident) -> u32 {
        let next = &mut self.next;
        *self.keys.entry(ident).or_insert_with(|| {
            let elem = *next;
            *next += 1;
            elem
        })
    }

    /// The header row for the section, the toggle and the list's scope as they are now: the one held
    /// unless one of them (or the locale) changed.
    fn header_row(&self, measure: &dyn plx_machine::machine::Measure) -> std::rc::Rc<header::Row> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (std::ptr::from_ref(plx_platform::i18n::current()) as usize).hash(&mut h);
        (self.section, self.show_available, self.sees_all).hash(&mut h);
        self.header_rows.get(h.finish(), || header::Row::new(measure, self.section, self.show_available, self.sees_all))
    }

    /// The header's stops, left to right: the two sections, and on Discover its "Available" control
    /// ([`header::Row`] lays out the same ones).
    fn header_elems(&self) -> &'static [u32] {
        match self.section {
            Section::Requests => &[REQUESTS_ELEM, DISCOVER_ELEM],
            Section::Discover => &[REQUESTS_ELEM, DISCOVER_ELEM, AVAILABLE_ELEM],
        }
    }

    /// Bring the list and the shelves up to the store. Answers whether anything the stack reads moved.
    fn sync(&mut self, seerr: Option<&SeerrStore>) -> bool {
        let mut changed = false;
        if let Some(seerr) = seerr {
            changed |= self.sync_list(seerr);
            for (ordinal, row) in discover::ROWS.into_iter().enumerate() {
                let discover::Row { cards, read, more } = discover::row(seerr, row, self.show_available);
                let shelf = &self.shelves[ordinal];
                if cards == shelf.cards && (read, more) == (shelf.read, shelf.more) {
                    continue;
                }
                let elems = cards.iter().map(|c| self.intern(Ident::Card(row, c.kind, c.tmdb))).collect();
                self.shelves[ordinal] = Shelf { cards, elems, read, more };
                changed = true;
            }
            if seerr.sees_all_requests() != self.sees_all {
                self.sees_all = seerr.sees_all_requests();
                changed = true;
            }
        }
        let fill = self.fill_of(seerr);
        changed |= fill != self.fill;
        self.fill = fill;
        if changed {
            self.relayout();
        }
        changed
    }

    /// The list's rows: in the order the page first showed them, a request new to the page at the
    /// top, one that left the list gone (module doc).
    fn sync_list(&mut self, seerr: &SeerrStore) -> bool {
        let pages = request_pages(seerr);
        if pages.read == 0 {
            return false;
        }
        let mut views: Vec<RowView> = Vec::new();
        for r in requests(seerr) {
            if !views.iter().any(|v| v.id == r.id) {
                views.push(rows::row_view(r, seerr.title_detail(r.kind, r.tmdb)));
            }
        }
        // A row new to the page stands after the shown row the server puts just before it, or on
        // top when the server puts it before every shown one (a new request); a shown row keeps
        // its place and its element.
        let shown = |id: i64| self.list.iter().any(|row| row.view.id == id);
        let (mut top, mut after): (Vec<RowView>, HashMap<i64, Vec<RowView>>) = (Vec::new(), HashMap::new());
        let mut before: Option<i64> = None;
        for view in &views {
            match before {
                _ if shown(view.id) => before = Some(view.id),
                None => top.push(view.clone()),
                Some(id) => after.entry(id).or_default().push(view.clone()),
            }
        }
        let mut list: Vec<ListRow> = top.into_iter().map(|view| ListRow { elem: 0, view }).collect();
        for row in &self.list {
            let Some(view) = views.iter().find(|v| v.id == row.view.id) else { continue };
            list.push(ListRow { elem: row.elem, view: view.clone() });
            list.extend(after.remove(&row.view.id).into_iter().flatten().map(|view| ListRow { elem: 0, view }));
        }
        for row in &mut list {
            if row.elem == 0 {
                row.elem = self.intern(Ident::Request(row.view.id));
            }
        }
        if list == self.list {
            return false;
        }
        self.list = list;
        true
    }

    fn relayout(&mut self) {
        self.content = match self.section {
            Section::Requests => (0..self.list.len()).map(row_group).collect(),
            Section::Discover => discover::ROWS.iter().enumerate()
                .filter(|(ordinal, _)| !self.shelves[*ordinal].cards.is_empty())
                .map(|(_, row)| shelf_group(*row)).collect(),
        };
        self.rev += 1;
    }

    fn fill_of(&self, seerr: Option<&SeerrStore>) -> Fill {
        let shown = match self.section {
            Section::Requests => !self.list.is_empty(),
            Section::Discover => self.shelves.iter().any(|s| !s.cards.is_empty()),
        };
        if shown {
            return Fill::Shown;
        }
        let Some(seerr) = seerr else { return Fill::Loading };
        let answers: Vec<Option<&Answer>> = match self.section {
            Section::Requests => vec![seerr.answer(&Slot::Requests(1))],
            Section::Discover => discover::ROWS.iter().map(|row| seerr.answer(&Slot::Discover(*row, 1))).collect(),
        };
        let more = self.section == Section::Discover
            && discover::ROWS.iter().any(|row| discover::row(seerr, *row, self.show_available).fill_page().is_some());
        if answers.iter().all(|a| matches!(a, Some(Answer::Failed(_)))) {
            Fill::Failed
        } else if answers.iter().all(|a| matches!(a, Some(Answer::Ready(_) | Answer::Failed(_)))) && !more {
            Fill::Empty
        } else {
            Fill::Loading
        }
    }

    fn regions(&self) -> PageRegions<'_> {
        PageRegions {
            header: Some(HEADER_GROUP),
            content: &self.content,
            rail: None,
            back_walks_header: false,
            arrival: Arrival::Content,
            pending: (self.fill == Fill::Loading).then_some(PENDING_GROUP),
        }
    }

    fn row_of(&self, elem: u32) -> Option<&ListRow> {
        self.list.iter().find(|row| row.elem == elem)
    }

    /// The card `elem` stands for.
    fn card_of(&self, elem: u32) -> Option<&RequestCard> {
        self.shelves.iter().find_map(|s| s.elems.iter().position(|e| *e == elem).and_then(|i| s.cards.get(i)))
    }

    fn read_out(&self) -> Option<&'static str> {
        match (self.fill, self.section) {
            (Fill::Failed, _) => Some(msg::seerr_title_unreachable()),
            (Fill::Empty, Section::Requests) => Some(msg::seerr_requests_empty()),
            (Fill::Empty, Section::Discover) => Some(msg::seerr_discover_empty()),
            (Fill::Loading | Fill::Shown, _) => None,
        }
    }
}

/// How far through the request list's pages the store has read: the leading run of landed pages,
/// and whether the server has more.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RequestPages {
    read: u32,
    more: bool,
}

fn request_pages(seerr: &SeerrStore) -> RequestPages {
    let mut out = RequestPages::default();
    while let Some(Answer::Ready(SeerrData::Requests(page))) = seerr.answer(&Slot::Requests(out.read + 1)) {
        out.read += 1;
        out.more = out.read < page.pages;
    }
    out
}

/// Every request on the pages read so far, in the server's order (a request the server moved to a
/// later page between two reads may come twice; the caller keeps the first).
fn requests(seerr: &SeerrStore) -> impl Iterator<Item = &plx_data::seerr::MediaRequest> {
    (1..=request_pages(seerr).read).filter_map(|page| match seerr.answer(&Slot::Requests(page)) {
        Some(Answer::Ready(SeerrData::Requests(page))) => Some(page.items.iter()),
        _ => None,
    }).flatten()
}

/// How close to the list's end focus may stand before its next page is read.
const NEAR_END: usize = 3;

fn row_group(i: usize) -> GroupId {
    GroupId(FIRST_ROW_GROUP + i as u32)
}

fn shelf_group(row: DiscoverRow) -> GroupId {
    GroupId(FIRST_SHELF_GROUP + discover::ordinal(row) as u32)
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
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: HEAD_H, focusable: true }, HEADER_GROUP).ranked(1));
        match self.section {
            Section::Requests => {
                for (i, row) in self.list.iter().enumerate() {
                    out.push(SectionSpec::new(Sec::Request(row.elem), Kind::Custom { height: ROW_PITCH, focusable: true },
                        row_group(i)));
                }
            }
            Section::Discover => {
                for (ordinal, row) in discover::ROWS.into_iter().enumerate() {
                    if self.shelves[ordinal].cards.is_empty() {
                        continue;
                    }
                    out.push(SectionSpec::new(Sec::Shelf(row), Kind::Shelf { style: &SHELF_STYLE, heading: SHELF_HEADING },
                        shelf_group(row)).seated(plx_ui::page_regions::CONTENT_HEAD_SEAT));
                }
            }
        }
        out.push(SectionSpec::new(Sec::Status, Kind::Overlay { rect: Rect::FULL, focusable: false }, STATUS_GROUP).ranked(2));
    }

    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        match self.section {
            Section::Requests => out.extend(self.list.iter().map(|row| Sec::Request(row.elem))),
            Section::Discover => out.extend(discover::ROWS.iter().map(|row| Sec::Shelf(*row))),
        }
        out.push(Sec::Head);
    }

    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<RequestCards<'a>> {
        let Sec::Shelf(row) = k else { return None };
        let shelf = &self.shelves[discover::ordinal(row)];
        (!shelf.cards.is_empty()).then(|| RequestCards {
            elems: &shelf.elems, cards: &shelf.cards, art_server: art_server(cx), more: shelf.more, epoch: shelf.read,
        })
    }

    /// A Seerr title has no item menu: a hold is not taught here.
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Sec, _elem: &u32) -> bool {
        false
    }

    fn elem_of(&self, k: Sec) -> Option<u32> {
        match k {
            Sec::Request(elem) => Some(elem),
            _ => None,
        }
    }

    fn plain_len(&self, k: Sec) -> usize {
        match k {
            Sec::Head => self.header_elems().len(),
            Sec::Request(_) => 1,
            Sec::Shelf(_) | Sec::Status => 0,
        }
    }

    fn plain_elem(&self, k: Sec, n: usize) -> Option<u32> {
        match k {
            Sec::Head => self.header_elems().get(n).copied(),
            _ if n == 0 => StackPage::<H>::elem_of(self, k),
            _ => None,
        }
    }

    /// Left and Right walk the header's stops; nothing else holds more than one.
    fn plain_step(&self, k: Sec, at: usize, dir: Dir) -> Option<usize> {
        match (k, dir) {
            (Sec::Head, Dir::Left) => at.checked_sub(1),
            (Sec::Head, Dir::Right) => Some(at + 1).filter(|n| *n < StackPage::<H>::plain_len(self, k)),
            _ => None,
        }
    }

    fn focus_rect(&self, _cx: &Cx<'_, H>, k: Sec, section: Rect) -> Rect {
        match k {
            Sec::Request(_) => Rect::new(ROW_X, section.y, ROW_W, plx_ui::media_row::ROW_H),
            _ => section,
        }
    }

    fn element_rect(&self, cx: &Cx<'_, H>, k: Sec, n: usize, section: Rect) -> Rect {
        let plate = (k == Sec::Head)
            .then(|| self.header_elems().get(n))
            .flatten()
            .and_then(|elem| self.header_row(cx.measure).plate(*elem));
        match plate {
            Some(plate) => Rect::new(plate.x, plate.y + section.y, plate.w, plate.h),
            None => StackPage::<H>::focus_rect(self, cx, k, section),
        }
    }

    /// The header section's group is the shared row's (`HeaderFocus::group_spec`); a request row is
    /// a column of one, Up and Down the page's links.
    fn plain_group(&self, cx: &Cx<'_, H>, k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        let header = (k == Sec::Head).then(|| self.header_row(cx.measure));
        match header.as_ref().and_then(|row| row.focus(self.entry, id).group_spec()) {
            Some(spec) => GroupSpec { extent, ..spec },
            None => GroupSpec {
                id,
                kind: GroupKind::Free,
                seat: Seat::First,
                reachable: AxisMask::VERTICAL,
                edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                extent,
                len: 1,
                elem: ElemKind::Bare,
            },
        }
    }

    fn shelf_foot(&self, _cx: &Cx<'_, H>, _k: Sec) -> f32 {
        UNDER_LABEL_AIR
    }

    fn extra_groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        self.regions().declare_pending(out);
    }

    fn draw_heading(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, lift: f32) {
        if let Sec::Shelf(row) = k {
            let p = f.painter.alpha(f.page_alpha);
            plx_ui::cards::draw_heading_c(p, discover::heading(row), c"", c"", MARGIN_X, r.y - lift, 1.0, f.measure);
        }
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, _scroll: f32) {
        let p = f.painter.alpha(f.page_alpha);
        match k {
            Sec::Head => {
                let pop = self.header_pop.scale_with(0, f.press.scale);
                self.header_row(f.measure).draw(p, r.y, &self.header_capsules, pop);
            }
            Sec::Request(elem) => {
                let Some(row) = self.row_of(elem) else { return };
                let focused = f.cx.focus.current == Some(FocusKey { entry: self.entry, elem });
                let server = art_server(f.cx);
                let view = &row.view;
                MediaRow {
                    title: &view.title,
                    lines: [&view.lines[0], &view.lines[1], &view.lines[2]],
                    chip: view.chip,
                    progress: view.progress,
                    poster: view.poster.as_deref().zip(server).map(|(url, server)| Poster { server, url }),
                    focused,
                }
                .draw(p, Rect::new(ROW_X, r.y, ROW_W, plx_ui::media_row::ROW_H), f.measure);
            }
            Sec::Status => {
                let Some(text) = self.read_out() else { return };
                let Ok(text) = CString::new(text) else { return };
                let (sz, bold, ink) = StatusOverlay::verdict_face(StatusKind::Failed);
                let band = Rect::new(0.0, StatusOverlay::FULL_ANCHOR_TOP, SCR_W, sz as f32);
                let label = Label::new(text.as_ptr(), sz, ink).h(HAlign::Center).v(VAlign::CapTop);
                if bold { label.bold() } else { label }.draw(p, band);
            }
            Sec::Shelf(_) => {}
        }
    }
}

/// The Seerr tab.
pub struct SeerrScreen {
    page: Page,
    stack: Stack<Sec>,
    /// The titles this page has asked for (each request's, read by its own `Detail`).
    titles_asked: Vec<(MediaKind, i64)>,
    /// When the list was last asked, by the page's `Tick` clock; `None` before the first tick.
    asked_at: Option<u32>,
    /// A surface or a pushed page stands over this one: it does not poll.
    covered: bool,
    /// The page's own arrival seat while nobody has chosen another (the Watchlist's rule): the group
    /// it last seated (`Some(None)`: nothing, while the section is on its way), `None` once the
    /// reader acts, the page is restored, or a row holds it.
    seat: Option<Option<GroupId>>,
    /// Seats this page asked the engine for whose `Enter` has not come back yet.
    own_enters: u8,
    /// How many of [`own_enters`](Self::own_enters) were queued before the reader acted.
    stale_enters: u8,
}

impl SeerrScreen {
    /// The heartbeat word, byte-identical to the route word (`app/words.rs`).
    pub const WORD: &'static str = "seerr";

    pub const SHAPE: &'static str = "SeerrScreen{entry:u32,section:u32,show_available:bool,sees_all:bool,next:u32,fill:u32,list:[{elem:u32,id:u64,tv:bool,tmdb:u64,chip:str,pending:bool,moving:bool}],shelves:[[{tv:bool,tmdb:u64,elem:u32}];5],titles_asked:[{tv:bool,tmdb:u64}],asked_at:Option<u32>,covered:bool,seat:Option<Option<u32>>,own_enters:u8,stale_enters:u8,header_capsules:TabStrip{sel:Capsule{x:Spring{pos:f32,vel:f32},w:Spring{pos:f32,vel:f32},a:Spring{pos:f32,vel:f32},at:u32},foc:Capsule{x:Spring{pos:f32,vel:f32},w:Spring{pos:f32,vel:f32},a:Spring{pos:f32,vel:f32},at:u32}},header_pop:{sp:[Spring{pos:f32,vel:f32};1],focused:Option<u32>},stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Shelf]}}";

    pub fn new(entry: EntryId) -> Self {
        let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
        Self {
            page: Page::new(entry),
            stack: Stack::new(entry).reveal_on_move(true).clipped(Rect::new(0.0, floor, SCR_W, SCR_H - floor)),
            titles_asked: Vec::new(),
            asked_at: None,
            covered: false,
            seat: None,
            own_enters: 0,
            stale_enters: 0,
        }
    }

    /// The section on view.
    pub fn section(&self) -> Section {
        self.page.section
    }

    /// Where the section on view stands as a whole.
    pub fn fill(&self) -> Fill {
        self.page.fill
    }

    /// The request rows, top to bottom.
    pub fn rows(&self) -> impl Iterator<Item = &RowView> {
        self.page.list.iter().map(|row| &row.view)
    }

    /// The element of request row `i`.
    pub fn row_elem(&self, i: usize) -> Option<u32> {
        self.page.list.get(i).map(|row| row.elem)
    }

    /// The cards Discover shelf `row` shows.
    pub fn shelf(&self, row: DiscoverRow) -> &[RequestCard] {
        &self.page.shelves[discover::ordinal(row)].cards
    }

    /// The element of card `i` of `row`.
    pub fn card_elem(&self, row: DiscoverRow, i: usize) -> Option<u32> {
        self.page.shelves[discover::ordinal(row)].elems.get(i).copied()
    }

    fn ask<H: SearchLike>(slot: Slot, op: SeerrOp, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Ask { slot, op }))));
    }

    /// Read the list's pages again, keeping the rows shown until the answers land.
    fn refresh<H: SearchLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.asked_at = Some(cx.tick.ms);
        let read = H::seerr(cx).map_or(0, |seerr| request_pages(seerr).read).max(1);
        for page in 1..=read {
            let (slot, op) = (Slot::Requests(page), SeerrOp::Requests { page });
            fx.push(Fx::App(AppFx::Store(StoreId::Seerr, StoreCmd::Seerr(SeerrCmd::Refresh { slot, op }))));
        }
    }

    /// Ask for `row`'s `page` unless it was asked already.
    fn ask_page<H: SearchLike>(seerr: &SeerrStore, row: DiscoverRow, page: u32, fx: &mut Effects<'_, H>) {
        if seerr.asked(&Slot::Discover(row, page)).is_none() {
            Self::ask(Slot::Discover(row, page), SeerrOp::Discover { row, page }, fx);
        }
    }

    fn ask_all<H: SearchLike>(fx: &mut Effects<'_, H>) {
        Self::ask(Slot::Requests(1), SeerrOp::Requests { page: 1 }, fx);
        for row in discover::ROWS {
            Self::ask(Slot::Discover(row, 1), SeerrOp::Discover { row, page: 1 }, fx);
        }
    }

    /// Bring the page up to the store: ask for the titles the rows still lack and the pages a
    /// filtered shelf runs short of, and poll the list while a request on view is on its way.
    fn sync<H: SearchLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let seerr = H::seerr(cx);
        if let Some(seerr) = seerr {
            for r in requests(seerr) {
                let id = (r.kind, r.tmdb);
                if self.titles_asked.contains(&id) || seerr.asked(&Slot::Detail(r.kind, r.tmdb)).is_some() {
                    continue;
                }
                self.titles_asked.push(id);
                Self::ask(Slot::Detail(r.kind, r.tmdb), SeerrOp::Detail { kind: r.kind, tmdb: r.tmdb }, fx);
            }
            // The list's next page, once focus walks near its end: every request stays reachable.
            let pages = request_pages(seerr);
            let at = cx.focus.current.filter(|key| key.entry == self.page.entry)
                .and_then(|key| self.page.list.iter().position(|row| row.elem == key.elem));
            let near_end = at.is_some_and(|i| i + NEAR_END >= self.page.list.len());
            if self.page.section == Section::Requests && pages.more && near_end
                && seerr.asked(&Slot::Requests(pages.read + 1)).is_none()
            {
                let page = pages.read + 1;
                Self::ask(Slot::Requests(page), SeerrOp::Requests { page }, fx);
            }
            if self.page.section == Section::Discover {
                for row in discover::ROWS {
                    if let Some(page) = discover::row(seerr, row, self.page.show_available).fill_page() {
                        Self::ask_page(seerr, row, page, fx);
                    }
                }
            }
        }
        if self.page.sync(seerr) {
            self.stack.reveal(&self.page, cx);
            fx.invalidate(Provenance::Input);
        }
        let ms = cx.tick.ms;
        let due = self.asked_at.is_none_or(|at| ms.wrapping_sub(at) >= REFRESH_MS);
        if !self.covered && self.page.section == Section::Requests && due && self.page.list.iter().any(|row| row.view.moving) {
            self.refresh(cx, fx);
        }
        if self.asked_at.is_none() {
            self.asked_at = Some(ms);
        }
    }

    /// Send `req` to the application (a screen names no sibling).
    fn request<H: SearchLike>(req: ContentReq, fx: &mut Effects<'_, H>) {
        fx.push(Fx::App(AppFx::Content(req)));
        fx.invalidate(Provenance::Input);
    }

    /// **Seat the arrival on the section's first row or shelf**, or nothing while it is on its way
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
                None => self.seat_own(FocusTarget::ContainerGroup(HEADER_GROUP), fx),
            },
            BackStep::Header => self.seat_own(FocusTarget::ContainerGroup(HEADER_GROUP), fx),
            BackStep::Nav => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            BackStep::Leave => Self::request(ContentReq::ToHome, fx),
        }
        Handled::Yes
    }

    /// A press on the shared strip: the other tabs and the chip are requests; the Seerr pill is the
    /// page the user is already on.
    fn strip_press(elem: u32) -> Option<Option<ContentReq>> {
        use plx_ui::widgets::{
            TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SEERR, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST,
        };
        match elem {
            TAB_KEY_HOME => Some(Some(ContentReq::Tab(HomeTab::Home))),
            TAB_KEY_MOVIES => Some(Some(ContentReq::Tab(HomeTab::Movies))),
            TAB_KEY_SHOWS => Some(Some(ContentReq::Tab(HomeTab::Shows))),
            TAB_KEY_SEARCH => Some(Some(ContentReq::Tab(HomeTab::Search))),
            TAB_KEY_WATCHLIST => Some(Some(ContentReq::Tab(HomeTab::Watchlist))),
            TAB_KEY_CHIP => Some(Some(ContentReq::Account)),
            TAB_KEY_SEERR => Some(None),
            _ => None,
        }
    }

    /// Choose `section`: the header's plate moves to it and the content below is its own. Choosing
    /// Requests reads the list again.
    fn choose<H: SearchLike>(&mut self, section: Section, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        if section == Section::Requests {
            self.refresh(cx, fx);
        }
        if self.page.section != section {
            self.page.section = section;
            self.page.fill = self.page.fill_of(H::seerr(cx));
            self.page.relayout();
            fx.invalidate(Provenance::Input);
        }
    }

    /// An element activated (a pointer's click, a committed press).
    fn activate<H: SearchLike>(&mut self, elem: u32, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match elem {
            REQUESTS_ELEM => self.choose(Section::Requests, cx, fx),
            DISCOVER_ELEM => self.choose(Section::Discover, cx, fx),
            AVAILABLE_ELEM => {
                self.page.show_available = !self.page.show_available;
                self.page.sync(H::seerr(cx));
                self.page.relayout();
                fx.invalidate(Provenance::Input);
            }
            _ if self.page.card_of(elem).is_some() || self.page.row_of(elem).is_some() => self.open(elem, fx),
            _ => match Self::strip_press(elem) {
                Some(Some(req)) => Self::request(req, fx),
                Some(None) => {}
                None => return Handled::No,
            },
        }
        Handled::Yes
    }

    /// A card or a row pressed: its title's page.
    fn open<H: SearchLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) {
        let title = match (self.page.card_of(elem), self.page.row_of(elem)) {
            (Some(card), _) => Some((card.kind, card.tmdb)),
            (_, Some(row)) => row.view.kind.map(|kind| (kind, row.view.tmdb)),
            _ => None,
        };
        if let Some((kind, tmdb)) = title {
            Self::request(ContentReq::Push(ContentArg::SeerrTitle { kind, tmdb }), fx);
        }
    }

    fn view<H: SearchLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }
}

plx_ui::focusable_via_view!(SeerrScreen, H: [SearchLike], view);

impl LogicalState for SeerrScreen {
    fn write(&self, c: &mut Canon) {
        let page = &self.page;
        c.u32(page.entry.0).u32(page.section as u32).bool(page.show_available).bool(page.sees_all).u32(page.next)
            .u32(page.fill as u32);
        c.seq(page.list.len());
        for row in &page.list {
            let v = &row.view;
            c.u32(row.elem).u64(v.id as u64).bool(v.kind == Some(MediaKind::Tv)).u64(v.tmdb as u64).str(v.chip)
                .bool(v.pending).bool(v.moving);
        }
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
        c.option(self.asked_at, |c, at| {
            c.u32(at);
        });
        c.bool(self.covered);
        c.option(self.seat, |c, seat| {
            c.option(seat, |c, group| {
                c.u32(group.0);
            });
        });
        c.u32(self.own_enters as u32).u32(self.stale_enters as u32);
        page.header_capsules.write_motion(c);
        page.header_pop.write_motion(c);
        self.stack.write(c);
    }

    fn probe(&self, out: &mut String) {
        let shelves: Vec<String> = self.page.shelves.iter().map(|s| s.cards.len().to_string()).collect();
        out.push_str(&format!("seerr section={:?} fill={:?} rows={} shelves={}", self.page.section, self.page.fill,
            self.page.list.len(), shelves.join(",")));
    }
}

impl<H: SearchLike> Machine<H> for SeerrScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(Sec::Shelf(row), CardEvent::Want(_))) => {
                // The reader walked near a shelf's end: its next page (`discover` module doc).
                let shelf = &self.page.shelves[discover::ordinal(row)];
                if let (Some(seerr), true) = (H::seerr(cx), shelf.more) {
                    Self::ask_page(seerr, row, shelf.read + 1, fx);
                }
                None
            }
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
                Self::ask_all(fx);
                Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.sync(cx, fx);
                self.page.header_row(cx.measure).step(self.page.entry, cx.focus.current, &mut self.page.header_capsules,
                    &mut self.page.header_pop, t.dt());
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
            ScreenEvent::Cover => {
                self.covered = true;
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                // A request made, approved or declined on a page over this one shows on the way back.
                self.covered = false;
                self.refresh(cx, fx);
                self.titles_asked.clear();
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, cx, fx),
            // The strip and the header are `Control` groups, so OK on a pill commits a press that
            // arrives here; a request row's press arrives the same way.
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    self.open(elem, fx);
                } else if let Some(key) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                    self.activate(key.elem, cx, fx);
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

impl<H: SearchLike> Screen<H> for SeerrScreen {
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
    /// The bar, the header and the section's rows or shelves are one column (`PageRegions::links`).
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
#[path = "tests.rs"]
mod tests;
