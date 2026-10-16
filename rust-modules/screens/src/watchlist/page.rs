//! **The Watchlist page** (plan §2.4 W45; boards Watchlist, WatchlistRemoved, TileStates): the
//! header, the six-column 250 x 375 grid of titles, the ground keyed to the focused title and the
//! focus caption.
//!
//! The page is a `plx_ui::cards::Stack` of three sections, like the Collection page: the head (the
//! title band and its one focus element, the Sort & Filter pill), the grid, and the full-screen
//! read-outs. Unlike Collection it is not windowed: the app hands it the WHOLE snapshot, already
//! ordered and filtered by the options ([`WatchlistIn::Rows`]), so there are no holes and no paging.
//!
//! **Routing.** A screen names no sibling and the registry has no Watchlist request yet, so a press
//! becomes either an effect that already exists (OK on a matched title is a Detail push, BACK is a
//! `ContentReq::Back`) or an entry in the page's outbox ([`WatchlistScreen::take_out`]) that the
//! app drains after each step. What leaves through the outbox is exactly what has no registry
//! spelling yet: the catalog page of an unmatched title (TODO(W6): `ContentArg::Catalog`), the
//! Sort & Filter menu and the hold menu.
use std::collections::HashMap;

use plx_data::watchlist::model::{Kind as WatchKind, ServerMatch, TileState, WatchRow};
use plx_machine::machine::{
    Canon, Cx, Edge, Effects, EntryId, FocusKey, GroupId, Handled, InputEvent, InputKind, Key, LogicalState,
    Machine, Tick,
};
use plx_machine::present::Provenance;
use plx_ui::cards::{
    self as ui_cards, CardEvent, CardSource, GridSpec, Kind, SectionSpec, Stack, StackEvent, StackPage, Tile,
    TileLabel,
};
use plx_ui::consts::*;
use plx_ui::screen::{
    AxisMask, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Link,
    RenderStrategy, Screen, ScreenEvent, Seat,
};
use plx_ui::theme;
use plx_ui::tile::{Resume, TileFacts, TileKind};
use plx_ui::widgets::{AmbientWash, Art, PageGround};
use plx_ui::{Painter, Rect};

use super::caption;
use super::header::{draw_header, HeaderFace};
use super::options::{ShowFilter, TypeFilter, WatchlistOptionsState};
use super::readout::{draw_readout, Readout};
use super::tiles;
use crate::registry::{AppFx, ContentArg, ContentLike, ContentReq};

/// One row of the snapshot the page draws: the stored row, its committed tile state and the
/// matched copy a press opens (if any). Built by the app from `WatchlistView`.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchTile {
    pub row: WatchRow,
    pub state: TileState,
    pub copy: Option<ServerMatch>,
}

/// What the matched PMS item lends a tile: the facts the Watchlist's own row does not carry (a
/// discover row has no server art, no watch state and no UltraBlur colours, plan §1.6). The app
/// sends these per guid as it learns them ([`WatchlistIn::Facts`]); a tile without them draws the
/// skeleton (Q9) and the flat ground.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MatchedFacts {
    /// The matched item's poster path on its server. Empty: the page derives
    /// `/library/metadata/<ratingKey>/thumb` for a playable copy.
    pub thumb: String,
    /// The matched item is finished (the watched tick).
    pub watched: bool,
    /// The matched item is part-way through (the resume bar), 0..1.
    pub resume: Option<f32>,
    /// The matched item's `UltraBlurColors` corners: the ground the focused title keys.
    pub blur: Option<[[f32; 3]; 4]>,
}

/// Where the list stands, which decides the read-out (plan §1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListState {
    /// The first read has not landed: no tiles and no read-out.
    Loading,
    /// The list landed; `total` is the whole list before the options narrowed it, so an empty
    /// snapshot is "your watchlist is empty" at 0 and "nothing matches" above it.
    Ready { total: usize },
    /// Q2: a managed profile without a plex.tv token has no list to read.
    Unavailable,
}

/// What the page asks the app to do. The app owns routing (a screen names no sibling).
#[derive(Clone, Debug, PartialEq)]
pub enum WatchlistOut {
    /// OK on a tile with a reachable matched copy: the Detail page of that (server, ratingKey).
    OpenDetail(ServerMatch),
    /// OK on a tile with no playable copy: the catalog page of this guid.
    /// TODO(W6): the app has no `ContentArg::Catalog` yet; until W6 lands the app drops this.
    OpenCatalog(String),
    /// Hold on the tile at `index` (in the shown order).
    Hold { index: usize, guid: String, state: TileState, copy: Option<ServerMatch> },
    /// OK on the Sort & Filter pill.
    OpenOptions,
    /// BACK.
    Back,
}

/// What the app tells the page.
#[derive(Clone, Debug, PartialEq)]
pub enum WatchlistIn {
    /// A new snapshot (already ordered and filtered by the options).
    Rows(Vec<WatchTile>),
    /// This guid left the list: the DS4 landing runs and focus moves to the next title.
    Removed(String),
    /// The Sort & Filter value changed.
    Options(WatchlistOptionsState),
    /// Where the list stands (loading, landed with a total, unavailable).
    List(ListState),
    /// What the matched PMS item lends the tile of `guid`.
    Facts(String, MatchedFacts),
}

// ── Geometry: `Watchlist.dc.html`, a 1920 x 1080 stage.

/// The grid's first row of posters (`top: 282px`).
pub const GRID_TOP: f32 = 282.0;
/// The content edge a scrolled row snaps to, and where the header is gone: the same 96 on every
/// side as every other card page.
const CONTENT_TOP: f32 = MARGIN_X;
/// The Sort & Filter pill (`right:150px; top:169px; height:54px`, 26 px text, a 20 px chevron).
const PILL_RIGHT: f32 = 150.0;
const PILL_TOP: f32 = 169.0;
const PILL_H: f32 = 54.0;
const PILL_PAD_LEAD: f32 = 24.0;
const PILL_PAD_TRAIL: f32 = 22.0;
const PILL_GAP: f32 = 10.0;
const PILL_CHEVRON: f32 = 20.0;

/// The pill's focus element, below the card keys.
pub const PILL_ELEM: u32 = 1;
/// The first card's element; cards count up from here, never reused within a page.
const FIRST_CARD_ELEM: u32 = 0x1000;

const GRID_GROUP: GroupId = GroupId(0);
const HEADER_GROUP: GroupId = GroupId(1);
const STATUS_GROUP: GroupId = GroupId(2);

/// The page's sections, in document order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    /// The title band and the Sort & Filter pill (one focus element).
    Head,
    /// The title grid.
    Items,
    /// The full-screen read-outs (not focusable).
    Status,
}

/// How much of the header is left at `scroll`: all of it at the top, none once the first row has
/// risen to the content edge, so a scrolled page leaves no remnant of the head above its rows.
fn head_alpha(scroll: f32) -> f32 {
    (1.0 - scroll / (GRID_TOP - CONTENT_TOP)).clamp(0.0, 1.0)
}

/// The pill's rect in the page's document space.
/// TODO(W45H): `header` owns the pill's paint; when it exports its rect this delegates to it.
fn pill_rect(measure: &dyn plx_machine::machine::Measure, summary: &str) -> Rect {
    let text = plx_platform::i18n::msg::watchlist_page_sort_filter_summary(summary);
    let w = PILL_PAD_LEAD + measure.width_str(&text, theme::size::LABEL, false) + PILL_GAP + PILL_CHEVRON + PILL_PAD_TRAIL;
    Rect::new(SCR_W - PILL_RIGHT - w, PILL_TOP, w, PILL_H)
}

/// One title's derived drawing facts, rebuilt when the snapshot or the facts move.
struct Cell {
    elem: u32,
    src: u16,
    thumb: String,
    watched: bool,
    resume: Option<f32>,
    blur: Option<[[f32; 3]; 4]>,
}

/// Guid to element: a title keeps its key while it stays on the list, and a key is never reused.
#[derive(Default)]
struct Keys {
    next: u32,
    by_guid: HashMap<String, u32>,
}

impl Keys {
    fn new() -> Self {
        Self { next: FIRST_CARD_ELEM, by_guid: HashMap::new() }
    }

    /// The elements of `tiles`, in order; keys of guids no longer present are released.
    fn assign(&mut self, tiles: &[WatchTile]) -> Vec<u32> {
        let mut kept = HashMap::with_capacity(tiles.len());
        let elems = tiles
            .iter()
            .map(|t| {
                let elem = self.by_guid.get(&t.row.guid).copied().unwrap_or_else(|| {
                    // A billion keys is far beyond any session: this is the loud failure of an
                    // impossible state, not a limit on the list.
                    assert!(self.next < plx_ui::geom::NAMESPACE_FLOOR, "watchlist element-key space exhausted");
                    self.next += 1;
                    self.next - 1
                });
                kept.insert(t.row.guid.clone(), elem);
                elem
            })
            .collect();
        self.by_guid = kept;
        elems
    }
}

/// The page's content and the state its sections read. The `Stack` is the screen's other field so
/// that `self.stack.on(&self.page, ..)` borrows the two apart.
struct Page {
    entry: EntryId,
    tiles: Vec<WatchTile>,
    options: WatchlistOptionsState,
    list: ListState,
    facts: HashMap<String, MatchedFacts>,
    keys: Keys,
    /// Parallel to `tiles`. Derived, not logical state.
    cells: Vec<Cell>,
    /// Element to index in `tiles`. Derived, not logical state.
    index: HashMap<u32, usize>,
    /// Moves whenever anything the sections read moves (`StackPage::revision`).
    rev: u64,
}

impl Page {
    fn touch(&mut self) {
        self.rev += 1;
    }

    /// Re-derive the cells after the snapshot or the facts moved.
    fn rebuild(&mut self) {
        let elems = self.keys.assign(&self.tiles);
        self.cells.clear();
        self.index.clear();
        for (i, (t, elem)) in self.tiles.iter().zip(elems).enumerate() {
            let facts = self.facts.get(&t.row.guid);
            // Only a playable copy has a poster of its own to fetch; an offline server's art stays
            // whatever the app handed over, and anything else draws the skeleton (Q9).
            let playable = matches!(t.state, TileState::Available | TileState::ComingSoonMatched);
            let thumb = match (facts.map(|f| f.thumb.as_str()).filter(|s| !s.is_empty()), t.copy) {
                (Some(path), _) => path.to_string(),
                (None, Some(copy)) if playable => format!("/library/metadata/{}/thumb", copy.rating_key),
                _ => String::new(),
            };
            // One corner mark per tile: an unplayable mark replaces the watched tick.
            let watched = facts.is_some_and(|f| f.watched) && !tiles::owns_corner(t.state);
            self.cells.push(Cell {
                elem,
                src: t.copy.map_or(0, |c| c.server_id.raw()),
                thumb,
                watched,
                resume: facts.and_then(|f| f.resume),
                blur: facts.and_then(|f| f.blur),
            });
            self.index.insert(elem, i);
        }
        self.touch();
    }

    fn readout(&self) -> Option<Readout> {
        match self.list {
            ListState::Unavailable => Some(Readout::NoToken),
            ListState::Ready { total } if self.tiles.is_empty() => {
                Some(if total == 0 { Readout::Empty } else { Readout::Filtered })
            }
            _ => None,
        }
    }

    /// Q7: the count beside the title: the titles shown, once the list has landed.
    fn count(&self) -> Option<usize> {
        matches!(self.list, ListState::Ready { .. }).then_some(self.tiles.len())
    }
}

/// The grid's content (`CardSource`): the snapshot's titles and what was derived for them.
struct Titles<'a> {
    page: &'a Page,
}

impl<H: ContentLike> CardSource<H> for Titles<'_> {
    fn len(&self) -> usize {
        self.page.cells.len()
    }
    fn elem(&self, i: usize) -> u32 {
        self.page.cells[i].elem
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.page.index.get(e).copied()
    }
    fn art(&self, i: usize) -> Art<'_> {
        let (t, c) = (&self.page.tiles[i], &self.page.cells[i]);
        Art::Poster(Some(TileFacts {
            owner: c.elem as usize,
            src: c.src,
            kind: match t.row.kind {
                WatchKind::Movie => TileKind::Movie,
                WatchKind::Show => TileKind::Show,
            },
            title: &t.row.title,
            thumb: &c.thumb,
            year: t.row.year.unwrap_or(0),
            resume: c.resume.map(|frac| Resume { frac, left_ms: 0 }),
            watched: c.watched,
            unwatched: !c.watched && c.resume.is_none(),
            blur: c.blur,
            ..Default::default()
        }))
    }
    fn label(&self, i: usize) -> TileLabel {
        let t = &self.page.tiles[i];
        TileLabel::titled(&t.row.title, &caption::caption(&t.row, t.state))
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.page.cells[i].resume
    }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, _measure: &dyn plx_machine::machine::Measure) {
        tiles::paint(p, self.page.tiles[i].state, tile);
    }
}

impl<H: ContentLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = Titles<'a>;

    fn revision(&self, _cx: &Cx<'_, H>) -> u64 {
        self.rev
    }

    fn sections(&self, _cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: GRID_TOP, focusable: true }, HEADER_GROUP).ranked(1));
        // DS4: a removal plays (survivors spring to their cells) instead of cutting.
        out.push(SectionSpec::new(
            Sec::Items,
            Kind::Grid { spec: GridSpec::new(GRID_TOP, CONTENT_TOP).animate_landings(true) },
            GRID_GROUP,
        ));
        out.push(SectionSpec::new(Sec::Status, Kind::Overlay { rect: Rect::FULL, focusable: false }, STATUS_GROUP).ranked(2));
    }

    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        out.extend([Sec::Items, Sec::Head]);
    }

    fn cards<'a>(&'a self, _cx: &'a Cx<'_, H>, k: Sec) -> Option<Titles<'a>> {
        (k == Sec::Items && !self.cells.is_empty()).then_some(Titles { page: self })
    }

    fn elem_of(&self, k: Sec) -> Option<u32> {
        (k == Sec::Head).then_some(PILL_ELEM)
    }

    /// Every title has a hold menu (at least Remove), so the hold hint shows on every card.
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Sec, _elem: &u32) -> bool {
        true
    }

    fn focus_rect(&self, cx: &Cx<'_, H>, k: Sec, section: Rect) -> Rect {
        match k {
            Sec::Head => {
                let r = pill_rect(cx.measure, &self.options.summary());
                Rect::new(r.x, r.y + section.y, r.w, r.h)
            }
            Sec::Items | Sec::Status => section,
        }
    }

    fn plain_group(&self, _cx: &Cx<'_, H>, _k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        GroupSpec {
            id,
            kind: GroupKind::Free,
            seat: Seat::First,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
            extent,
            len: 1,
            elem: ElemKind::Bare,
        }
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, _r: Rect, scroll: f32) {
        let p = f.painter.alpha(f.page_alpha);
        match k {
            Sec::Head => {
                let p = p.alpha(head_alpha(scroll));
                let summary = self.options.summary();
                let on = f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == PILL_ELEM);
                draw_header(
                    p,
                    Rect::new(0.0, -scroll, SCR_W, GRID_TOP),
                    &HeaderFace {
                        title: plx_platform::i18n::msg::watchlist_page_title(),
                        count: self.count(),
                        summary: &summary,
                        pill_focused: on,
                    },
                );
            }
            Sec::Status => {
                if let Some(readout) = self.readout() {
                    draw_readout(readout, p, Rect::FULL);
                }
            }
            Sec::Items => {}
        }
    }
}

pub struct WatchlistScreen {
    page: Page,
    /// The page's layout, one scroll, groups, reconcile and the grid (`ui::cards::Stack`).
    stack: Stack<Sec>,
    ground: PageGround,
    ground_seeded: bool,
    /// The focused title's index as the engine last told us (the last one while the pill holds
    /// focus): the screen keeps no focus of its own, this mirrors the engine's for `press`.
    focus: usize,
    /// What the page asked of the app that has no effect spelling yet; drained by
    /// [`take_out`](Self::take_out).
    outbox: Vec<WatchlistOut>,
}

impl WatchlistScreen {
    /// The heartbeat word, byte-identical to the route word (`tests/manifest.json` selects on it).
    pub const WORD: &'static str = "watchlist";

    pub const SHAPE: &'static str = "WatchlistScreen{entry:u32,list:{tag:u32,total:u64},options:{type:u32,sort:str,descending:bool,show:u32},next_elem:u32,tiles:[{guid:str,state:u32,elem:u32}],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Grid{bands,pop,landing}]}}";

    /// A page over `rows` (a snapshot already ordered and filtered by `options`), landed. The
    /// entry is unset: mount with [`at_entry`](Self::at_entry).
    pub fn new(rows: Vec<WatchTile>, options: WatchlistOptionsState) -> Self {
        let entry = EntryId(0);
        let total = rows.len();
        let mut page = Page {
            entry,
            tiles: rows,
            options,
            list: ListState::Ready { total },
            facts: HashMap::new(),
            keys: Keys::new(),
            cells: Vec::new(),
            index: HashMap::new(),
            rev: 0,
        };
        page.rebuild();
        Self {
            page,
            // A page that leaves focus (a menu over it, a pointer gone) rests at the top.
            stack: Stack::new(entry).hold_hint(plx_ui::hold_hint::Kind::Collection).home_when_unfocused(true),
            ground: PageGround::new(),
            ground_seeded: false,
            focus: 0,
            outbox: Vec::new(),
        }
    }

    /// The same page mounted as `entry`.
    pub fn at_entry(mut self, entry: EntryId) -> Self {
        self.page.entry = entry;
        self.stack = Stack::new(entry).hold_hint(plx_ui::hold_hint::Kind::Collection).home_when_unfocused(true);
        self
    }

    pub fn tiles(&self) -> &[WatchTile] {
        &self.page.tiles
    }

    pub fn options(&self) -> &WatchlistOptionsState {
        &self.page.options
    }

    pub fn list(&self) -> ListState {
        self.page.list
    }

    /// The read-out the page shows instead of a grid, if any.
    pub fn readout(&self) -> Option<Readout> {
        self.page.readout()
    }

    /// Q7: the count beside the title.
    pub fn count(&self) -> Option<usize> {
        self.page.count()
    }

    /// The focused tile's index in the shown order, if there is one.
    pub fn focused(&self) -> Option<usize> {
        (self.focus < self.page.tiles.len()).then_some(self.focus)
    }

    /// The element of the tile at `index`.
    pub fn elem_at(&self, index: usize) -> Option<u32> {
        self.page.cells.get(index).map(|c| c.elem)
    }

    /// The tile showing element `elem`.
    pub fn index_of(&self, elem: u32) -> Option<usize> {
        self.page.index.get(&elem).copied()
    }

    /// The Sort & Filter pill's rect in the page's document space.
    pub fn pill_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        pill_rect(measure, &self.page.options.summary())
    }

    /// Take an input. `Rows` and `Options` replace the state; `Removed` drops the tile, leaving
    /// the engine's reconcile to put focus on the title that took its index (the next one), and
    /// the grid's landing to play the survivors into their cells (DS4).
    pub fn input(&mut self, ev: WatchlistIn) {
        match ev {
            WatchlistIn::Rows(rows) => {
                self.page.tiles = rows;
                self.page.rebuild();
            }
            WatchlistIn::Options(options) => {
                self.page.options = options;
                self.page.touch();
            }
            WatchlistIn::Removed(guid) => {
                let before = self.page.tiles.len();
                self.page.tiles.retain(|t| t.row.guid != guid);
                self.page.facts.remove(&guid);
                if let ListState::Ready { total } = &mut self.page.list {
                    if self.page.tiles.len() < before {
                        *total = total.saturating_sub(1);
                    }
                }
                self.page.rebuild();
            }
            WatchlistIn::List(list) => {
                self.page.list = list;
                self.page.touch();
            }
            WatchlistIn::Facts(guid, facts) => {
                self.page.facts.insert(guid, facts);
                self.page.rebuild();
            }
        }
        self.focus = self.focus.min(self.page.tiles.len().saturating_sub(1));
    }

    /// OK on the tile at `index`, per the plan §1.9 table. Reads the drawn, committed state.
    pub fn press_at(&self, index: usize) -> Option<WatchlistOut> {
        let t = self.page.tiles.get(index)?;
        Some(match (t.state, t.copy) {
            // Q6 (default): coming soon AND matched keeps the clock and stays playable.
            (TileState::Available | TileState::ComingSoonMatched, Some(copy)) => WatchlistOut::OpenDetail(copy),
            // TODO(W6): the catalog page; the app drops this until `ContentArg::Catalog` exists.
            _ => WatchlistOut::OpenCatalog(t.row.guid.clone()),
        })
    }

    /// Hold on the tile at `index`.
    pub fn hold_at(&self, index: usize) -> Option<WatchlistOut> {
        let t = self.page.tiles.get(index)?;
        Some(WatchlistOut::Hold { index, guid: t.row.guid.clone(), state: t.state, copy: t.copy })
    }

    /// OK on the focused tile.
    pub fn press(&self) -> Option<WatchlistOut> {
        self.press_at(self.focused()?)
    }

    /// Hold on the focused tile.
    pub fn hold(&self) -> Option<WatchlistOut> {
        self.hold_at(self.focused()?)
    }

    /// What the page asked of the app since the last call, oldest first. The app drains this after
    /// every step it gives the page.
    pub fn take_out(&mut self) -> Vec<WatchlistOut> {
        std::mem::take(&mut self.outbox)
    }

    /// The ground the focused title keys, as a target: its matched item's UltraBlur colours, else
    /// the flat app surface. A discover row carries no colours of its own (plan §1.6), so the
    /// matched PMS item's are the only source.
    fn ground_target(&self) -> [[f32; 4]; 4] {
        self.page
            .cells
            .get(self.focus)
            .and_then(|c| c.blur)
            .map_or([theme::SURFACE_APP; 4], |blur| AmbientWash::keyed(blur, PageGround::CARD_W))
    }

    fn tick<H: ContentLike>(&mut self, t: Tick, cx: &Cx<'_, H>) {
        if let Some(i) = cx
            .focus
            .current
            .filter(|key| key.entry == self.page.entry)
            .and_then(|key| self.page.index.get(&key.elem).copied())
        {
            self.focus = i;
        }
        let target = self.ground_target();
        if self.ground_seeded {
            self.ground.key_target(target, t.dt());
        } else {
            self.ground.jump_target(target);
            self.ground_seeded = true;
        }
    }

    /// Send `out` where it goes: an existing effect, or the outbox.
    fn route<H: ContentLike>(&mut self, out: WatchlistOut, fx: &mut Effects<'_, H>) {
        match out {
            WatchlistOut::OpenDetail(m) => fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Push(
                ContentArg::Detail { sid: m.server_id, rk: m.rating_key.to_string() },
            )))),
            WatchlistOut::Back => fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Back))),
            other => {
                self.outbox.push(other);
                fx.invalidate(Provenance::Input);
            }
        }
    }

    /// The page's `Focusable` (see `focusable_via_view!` below): the stack's view over the page.
    fn view<H: ContentLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    /// Test support: register the stops the grid draws (`Grid::record_stops`).
    #[cfg(test)]
    pub(crate) fn record_stops<H: ContentLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        self.stack.view(&self.page).record_stops(f);
    }

    /// Test support: the scale card `elem` is drawn at.
    #[cfg(test)]
    pub(crate) fn scale_of<H: ContentLike>(&self, cx: &Cx<'_, H>, elem: &u32) -> Option<f32> {
        self.stack.view(&self.page).scale_of(cx, elem)
    }

    /// Test support: the page scroll.
    #[cfg(test)]
    pub(crate) fn scroll(&self) -> f32 {
        self.stack.scroll()
    }

    /// Test support: the ground colours the focused title keys (before the wash eases to them).
    #[cfg(test)]
    pub(crate) fn ground_goal(&self) -> [[f32; 4]; 4] {
        self.ground_target()
    }

    /// Test support: set the mirrored focus index, as a tick would from the engine's focus.
    #[cfg(test)]
    pub(crate) fn set_focus_index(&mut self, i: usize) {
        self.focus = i;
    }
}

plx_ui::focusable_via_view!(WatchlistScreen, H: [ContentLike], view);

impl LogicalState for WatchlistScreen {
    fn write(&self, c: &mut Canon) {
        let page = &self.page;
        c.u32(page.entry.0).u32(page.keys.next);
        match page.list {
            ListState::Loading => c.u32(0).u64(0),
            ListState::Ready { total } => c.u32(1).u64(total as u64),
            ListState::Unavailable => c.u32(2).u64(0),
        };
        let o = &page.options;
        c.u32(match o.type_ {
            TypeFilter::All => 0,
            TypeFilter::Movies => 1,
            TypeFilter::Shows => 2,
        })
        .str(&format!("{:?}", o.sort))
        .bool(o.descending)
        .u32(match o.show {
            ShowFilter::All => 0,
            ShowFilter::Released => 1,
            ShowFilter::OnYourServers => 2,
        });
        c.seq(page.tiles.len());
        for (t, cell) in page.tiles.iter().zip(&page.cells) {
            c.str(&t.row.guid).u32(t.state as u32).u32(cell.elem);
        }
        self.stack.write(c);
    }

    fn probe(&self, out: &mut String) {
        out.push_str(&format!(
            "watchlist tiles={} next={} focus={} list={:?}",
            self.page.tiles.len(),
            self.page.keys.next,
            self.focus,
            self.page.list
        ));
    }
}

impl<H: ContentLike> Machine<H> for WatchlistScreen {
    type Ev = ScreenEvent<H>;

    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(_, card)) => Some(card),
            _ => None,
        };
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx);
                Handled::Yes
            }
            ScreenEvent::Enter(_) | ScreenEvent::Uncover => {
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) if *elem == PILL_ELEM => {
                self.route(WatchlistOut::OpenOptions, fx);
                Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    if let Some(out) = self.index_of(elem).and_then(|i| self.press_at(i)) {
                        self.route(out, fx);
                    }
                }
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => {
                if let Some(CardEvent::Hold(elem)) = card {
                    if let Some(out) = self.index_of(elem).and_then(|i| self.hold_at(i)) {
                        self.route(out, fx);
                    }
                    Handled::Yes
                } else {
                    Handled::No
                }
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                self.route(WatchlistOut::Back, fx);
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike> Screen<H> for WatchlistScreen {
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        self.stack.view(&self.page).redraw_focused(f, focus);
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
        let p = f.painter.alpha(f.page_alpha);
        self.ground.draw(p, Rect::FULL);
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
    /// The pill and the grid are one column: DOWN from the pill enters the grid, UP from the first
    /// row returns to the pill.
    fn links(&self, out: &mut Vec<Link>) {
        if !self.page.cells.is_empty() {
            out.push(Link { from: HEADER_GROUP, dir: plx_ui::screen::Dir::Down, to: GRID_GROUP });
            out.push(Link { from: GRID_GROUP, dir: plx_ui::screen::Dir::Up, to: HEADER_GROUP });
        }
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[allow(unused_imports)]
use ui_cards as _;
