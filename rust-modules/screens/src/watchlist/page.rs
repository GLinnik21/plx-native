//! **The Watchlist page** (plan §2.4 W45; boards Watchlist, WatchlistRemoved, TileStates): the
//! header, the six-column 250 x 375 grid of titles, the ground keyed to the focused title and the
//! focus caption.
//!
//! The page is a `plx_ui::cards::Stack` of three sections, like the Collection page: the head (the
//! title band and its one focus element, the Sort & Filter pill), the grid, and the full-screen
//! read-outs. Unlike Collection it is not windowed: the app hands it the snapshot loaded so far, already
//! ordered and filtered by the options ([`WatchlistIn::Rows`]). While later pages arrive the grid
//! reserves cells past the loaded tail (`UNLOADED_ELEM`, `unloaded()`): `len()` is the list total,
//! not the loaded rows.
//!
//! **Routing.** A screen names no sibling, so a press becomes an effect: OK on a matched title is
//! a Detail push (`ContentReq::Push`), and everything else leaves as `AppFx::Watchlist` (the
//! registry's spelling of [`WatchlistOut`]) for the application to route: the strip's tabs and the
//! profile chip, BACK, the Sort & Filter menu, the hold menu, and the catalog page of an unmatched
//! title (the application opens `ContentArg::Catalog` for it).
use std::collections::HashMap;

use plx_data::watchlist::model::{Kind as WatchKind, ServerMatch, TileState, WatchRow};
use plx_machine::machine::{
    Canon, Cx, Delivery, Edge, Effects, Enter, EntryId, FocusKey, FocusTarget, Fx, GroupId, Handled, InputEvent,
    InputKind, Key, LogicalState, Machine, Tick,
};
use plx_machine::present::Provenance;
use plx_ui::cards::{
    self as ui_cards, CardEvent, Grid, CardSource, GridSpec, Kind, SectionSpec, Stack, StackEvent, StackPage, Tile,
    TileLabel,
};
use plx_ui::consts::*;
use plx_ui::screen::{
    AxisMask, DrawFrame, EdgeRule, ElemKind, FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Link,
    RenderStrategy, Screen, ScreenEvent, Seat,
};
use plx_ui::page_regions::{Arrival, BackStep, PageRegions};
use plx_ui::theme;
use plx_ui::tile::{Resume, TileFacts, TileKind};
use plx_ui::widgets::{AmbientWash, Art, PageGround};
use plx_plex::plex::{ListingFacts, ServerId};
use plx_ui::{Painter, Rect};

use super::caption;
use super::header::{self, Count};
use super::options::{ShowFilter, TypeFilter, WatchlistOptionsState};
use super::readout::{draw_readout, unreachable_overlay, Readout};
use super::tiles;
use crate::registry::{AppFx, ContentArg, ContentLike, ContentReq, HomeTab};

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
    /// What the matched item's listing row says of the title (genre, video, ratings, length, counts):
    /// the facts the focused caption words under the Under posters setting
    /// ([`caption::label`](super::caption::label)), as every poster's caption does.
    pub listing: ListingFacts,
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
    /// The first read failed (plex.tv unreachable) and no list had landed: the read-out with Try
    /// again. A list that has landed is never replaced by this.
    Failed,
}

/// What the page asks the app to do. The app owns routing (a screen names no sibling).
#[derive(Clone, Debug, PartialEq)]
pub enum WatchlistOut {
    /// OK on a tile with a reachable matched copy: the Detail page of that (server, ratingKey).
    OpenDetail(ServerMatch),
    /// OK on a tile with no playable copy: the catalog page of this guid.
    OpenCatalog(String),
    /// Hold on the tile at `index` (in the shown order).
    Hold { index: usize, guid: String, state: TileState, copy: Option<ServerMatch> },
    /// OK on the Sort & Filter pill.
    OpenOptions,
    /// BACK.
    Back,
    /// A press on the shared strip's other tabs (Home, Movies, TV Shows, Search).
    Tab(HomeTab),
    /// A press on the profile chip.
    Account,
    /// Try again on the unreachable read-out: read the list again.
    Retry,
    /// The Sort & Filter menu changed the value (sent by the menu surface, not the page).
    SetOptions(WatchlistOptionsState),
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
    /// The reachable server whose photo proxy fetches the art of titles no server holds (plan §0,
    /// §3). `None`: no server is reachable, and those titles draw the skeleton (Q9).
    ArtServer(Option<ServerId>),
    /// DEBUG BUILDS ONLY (the app feeds it under `cfg(feature = "devtriggers")`): whether the last
    /// Watchlist write was refused by the dev-build failsafe, so the header says so instead of the
    /// press looking like it did nothing. Never sent in nightly or stable (Q8: no toast for users).
    DevBlocked(bool),
}

// ── Geometry: `Watchlist.dc.html`, a 1920 x 1080 stage.

/// The grid's first row of posters: 96 under the header row's top, as the Library's (244).
pub const GRID_TOP: f32 = plx_ui::page_header::GRID_TOP;
/// Six columns of 250 posters from the row's text edge, 285 apart: the gap that puts the last
/// column's right edge on the row's right text edge.
const COLS: usize = 6;
const GRID_GAP: f32 = (header::GRID_RIGHT - MARGIN_X - COLS as f32 * CARD_W) / (COLS as f32 - 1.0);

/// The grid's cards: the shared home poster, at the Library's gap.
fn grid_style() -> plx_ui::cards::RowStyle {
    plx_ui::cards::RowStyle { gap: GRID_GAP, ..plx_ui::cards::RowStyle::HOME }
}

/// The grid's spec: top, columns. Its rows REST, as the Library's do: a focused row below the first
/// comes to rest with its posters at the first row's y (`GRID_TOP`, 244), held by the document's end
/// (`GridSpec::resting`).
fn grid_spec() -> GridSpec {
    GridSpec::resting(GRID_TOP).columns(COLS, grid_style(), MARGIN_X)
}

/// The pill's focus element, below the card keys.
pub const PILL_ELEM: u32 = 1;
/// The unreachable read-out's Try again, below the card keys.
pub const RETRY_ELEM: u32 = 2;
/// The first card's element; cards count up from here, never reused within a page.
const FIRST_CARD_ELEM: u32 = 0x1000;
/// The base of the reserved cells' keys past the loaded tail (plan §1.3): the card keys count up
/// to here, so the two ranges never meet. A reserved cell at list position `i` is keyed
/// `UNLOADED_ELEM + i` — its ABSOLUTE position, not its distance past the tail — so a page landing
/// never hands one cell's key to another cell; when position `i`'s row lands, the key is retired and
/// `StackPage::supersede` moves focus to the title keyed in the same cell.
const UNLOADED_ELEM: u32 = plx_ui::geom::NAMESPACE_FLOOR / 2;

/// The key of the reserved cell at list position `i`. A list longer than the key range left below
/// the namespace floor (some 134 million titles) is the loud failure of an impossible state, not a
/// limit on the list.
fn reserved_elem(i: usize) -> u32 {
    u32::try_from(i)
        .ok()
        .and_then(|i| UNLOADED_ELEM.checked_add(i))
        .filter(|&e| e < plx_ui::geom::NAMESPACE_FLOOR)
        .expect("watchlist reserved-cell key space exhausted")
}

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

/// One title's derived drawing facts, rebuilt when the snapshot or the facts move.
#[derive(Clone)]
struct Cell {
    elem: u32,
    src: u16,
    thumb: String,
    watched: bool,
    resume: Option<f32>,
    blur: Option<[[f32; 3]; 4]>,
    /// The matched copy's caption facts (default: none known).
    listing: ListingFacts,
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
                    assert!(self.next < UNLOADED_ELEM, "watchlist element-key space exhausted");
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
    /// The reachable server whose photo proxy fetches art for titles no server holds.
    art_server: Option<ServerId>,
    /// The tiles a landing is fading out, by `elem_key` (DS4): kept from the removal until the
    /// grid has nothing left to fade, since the snapshot no longer knows them. Not logical state.
    ghosts: HashMap<u64, (WatchTile, Cell)>,
    /// The count a Remove replaced, kept on the header until the grid's landing starts (so the
    /// number and the tiles move on one frame, after the hold menu is gone). Not logical state.
    count_hold: Option<CountHold>,
    /// A debug build's cue (`WatchlistIn::DevBlocked`): the last write was refused by the dev-build
    /// failsafe. Only a `devtriggers` build is ever told `true` and only it draws the line. Not
    /// logical state (no recorded shape).
    dev_blocked: bool,
    /// Test support: how many ghost posters the grid has drawn.
    #[cfg(test)]
    ghost_draws: std::cell::Cell<usize>,
    /// Test support: the reserved cells the grid has drawn, by index.
    #[cfg(test)]
    skeleton_draws: std::cell::RefCell<Vec<usize>>,
    /// Test support: every poster the grid asked for, in the order it asked: `(is a ghost, index)`.
    #[cfg(test)]
    art_log: std::cell::RefCell<Vec<(bool, usize)>>,
    /// Moves whenever anything the sections read moves (`StackPage::revision`).
    rev: u64,
    /// The header row's pills (the focus capsule, the selection plate under the title) and its
    /// focus pop: the shared `PageHeader` motion, held by the page that wears the row and stepped
    /// by the screen's tick.
    header_capsules: plx_ui::widgets::TabStrip,
    header_pop: plx_ui::widgets::CtlPop<1>,
    /// The top chrome as drawn (and the header row with it): decided from the stack's scroll target and
    /// run on the swap clock (`plx_ui::scroll_chrome`), stepped by the screen's tick. This page wears
    /// no capsule.
    handoff: plx_ui::scroll_chrome::Handoff,
    /// The row itself, held by the one rule every header row is held by
    /// (`plx_ui::page_header::SharedRow`): built when the options, the count or the locale change,
    /// not per question. Derived from them; not canon.
    header_rows: plx_ui::page_header::SharedRow<header::Row>,
    /// What the row carries over from the row it replaced (`plx_ui::page_header::HeaderSwap`): the
    /// sort control's criterion changing, the read-out coming, going or saying another thing. The
    /// row is held behind `&self` (the focus questions find it on the first ask), so the swap is
    /// begun from the rebuild itself ([`Page::header_row`], `SharedRow::get_replacing`) and sits
    /// behind the same kind of cell. Decoration, never canon: not in `SHAPE` or `write_canon`.
    header_swap: std::cell::RefCell<plx_ui::page_header::HeaderSwap>,
}

impl Page {
    fn touch(&mut self) {
        self.rev += 1;
    }

    /// The caption of the tile at `i` under the Under posters setting: its row and state, and the
    /// facts of its matched copy.
    fn label(&self, i: usize) -> TileLabel {
        match self.tiles.get(i).zip(self.cells.get(i)) {
            Some((t, c)) => caption::label(&t.row, t.state, &caption::policy(), &c.listing),
            None => TileLabel::default(),
        }
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
                // No copy to ask: the Discover poster, fetched by a reachable server's photo proxy
                // (`src` names that server). With none reachable the tile draws the skeleton (Q9).
                (None, _) if self.art_server.is_some() => t.row.thumb.clone(),
                _ => String::new(),
            };
            let src = t.copy.map(|c| c.server_id).or(self.art_server.filter(|_| !thumb.is_empty())).map_or(0, |s| s.raw());
            // One corner mark per tile: an unplayable mark replaces the watched tick.
            let watched = facts.is_some_and(|f| f.watched) && !tiles::owns_corner(t.state);
            self.cells.push(Cell {
                elem,
                src,
                thumb,
                watched,
                resume: facts.and_then(|f| f.resume),
                blur: facts.and_then(|f| f.blur).or_else(|| t.copy.and_then(|c| c.blur).map(unquantize)),
                listing: facts.map(|f| f.listing.clone()).unwrap_or_default(),
            });
            self.index.insert(elem, i);
        }
        self.touch();
    }

    /// Swap the snapshot for `rows`: a tile that leaves is kept, with what was derived for it, as
    /// a ghost for the grid's landing to fade out (DS4); the survivors keep their keys.
    fn replace_tiles(&mut self, rows: Vec<WatchTile>) {
        let staying: std::collections::HashSet<&str> = rows.iter().map(|t| t.row.guid.as_str()).collect();
        for (t, cell) in self.tiles.iter().zip(&self.cells) {
            if !staying.contains(t.row.guid.as_str()) {
                self.ghosts.insert(ui_cards::elem_key(&cell.elem), (t.clone(), cell.clone()));
            }
        }
        self.tiles = rows;
        self.rebuild();
    }

    /// **This page's column** (`plx_ui::page_regions`): the Sort & Filter header, then the grid once
    /// it has a cell. The page arrives in its content and BACK leaves for Home from the top of it; it
    /// does not walk the header. **While the first read has not landed the grid is
    /// [pending](PageRegions::pending)**: the page seats nothing (no tile to seat, and the pill is
    /// not the arrival), and the tiles that land seat the first one unless the reader moved first.
    fn regions(&self) -> PageRegions<'static> {
        PageRegions {
            header: Some(HEADER_GROUP),
            content: if self.cells.is_empty() { &[] } else { &[GRID_GROUP] },
            rail: None,
            back_walks_header: false,
            arrival: Arrival::Content,
            pending: (self.list == ListState::Loading).then_some(GRID_GROUP),
        }
    }

    /// The header row for the options and count as they are now: the one held, unless one of them
    /// (or the locale) moved since it was built.
    fn header_row(&self, measure: &dyn plx_machine::machine::Measure) -> std::rc::Rc<header::Row> {
        use std::hash::{Hash, Hasher};
        let count = self.count();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (std::ptr::from_ref(plx_platform::i18n::current()) as usize).hash(&mut h);
        self.options.hash(&mut h);
        header::Count::narrowed(count).hash(&mut h);
        self.header_rows.get_replacing(
            h.finish(),
            || header::Row::new(measure, &self.options.summary(), count),
            |old, new| new.begin_swap(old, &mut self.header_swap.borrow_mut()),
        )
    }

    fn readout(&self) -> Option<Readout> {
        match self.list {
            ListState::Unavailable => Some(Readout::NoToken),
            ListState::Failed => Some(Readout::Unreachable),
            ListState::Ready { total } if self.tiles.is_empty() => {
                Some(if total == 0 { Readout::Empty } else { Readout::Filtered })
            }
            _ => None,
        }
    }

    /// Q7: the count just left of the pill, once the list has landed and is not empty: the list's
    /// whole length while the options show everything (the service's own, while later pages are
    /// still arriving, so the count does not climb as they land), else the titles shown of the
    /// whole list. Sort alone does not narrow. A filter that leaves nothing still counts "0 of N";
    /// the empty list and the unavailable reads show none.
    fn count(&self) -> Option<Count> {
        if let Some(hold) = &self.count_hold {
            return Some(hold.count);
        }
        self.live_count()
    }

    /// [`count`](Self::count) from the current list, ignoring a hold.
    fn live_count(&self) -> Option<Count> {
        let ListState::Ready { total } = self.list else { return None };
        if total == 0 {
            return None;
        }
        let unfiltered = self.options.type_ == TypeFilter::All && self.options.show == ShowFilter::All;
        Some(if unfiltered {
            Count::All(total.max(self.tiles.len()))
        } else {
            Count::Of { shown: self.tiles.len(), total }
        })
    }

    /// Whether the title at `i` will have colours once its facts arrive: a playable matched copy
    /// whose facts the page has not been told yet (a title with facts and no colours has none).
    fn colours_pending(&self, i: usize) -> bool {
        self.tiles.get(i).is_some_and(|t| {
            matches!(t.state, TileState::Available | TileState::ComingSoonMatched)
                && t.copy.is_some_and(|c| c.blur.is_none())
                && !self.facts.contains_key(&t.row.guid)
        })
    }

    /// The grid's length: the loaded rows and the reserved cells past them.
    fn len(&self) -> usize {
        self.cells.len() + self.unloaded()
    }

    /// The cell showing `e`: a title's, or a reserved cell still past the loaded tail.
    fn cell_of(&self, e: u32) -> Option<usize> {
        match self.reserved_at(e) {
            Some(i) => (i >= self.cells.len() && i < self.len()).then_some(i),
            None => self.index.get(&e).copied(),
        }
    }

    /// The list position a reserved cell's key names, or `None` for any other key.
    fn reserved_at(&self, e: u32) -> Option<usize> {
        e.checked_sub(UNLOADED_ELEM).filter(|_| e < plx_ui::geom::NAMESPACE_FLOOR).map(|n| n as usize)
    }

    /// The cells the list has that no row has landed for yet (plan §1.3): the service's total less
    /// the loaded rows, while the options show everything (a narrowed list has no total to compare).
    fn unloaded(&self) -> usize {
        let ListState::Ready { total } = self.list else { return 0 };
        let unfiltered = self.options.type_ == TypeFilter::All && self.options.show == ShowFilter::All;
        if unfiltered {
            total.saturating_sub(self.tiles.len())
        } else {
            0
        }
    }
}

/// A matched copy's colours as the painter takes them (the wire's bytes, back to 0..1).
fn unquantize(b: plx_data::watchlist::model::Blur) -> [[f32; 3]; 4] {
    b.map(|corner| corner.map(|v| f32::from(v) / 255.0))
}

/// What the tile showing `t` (derived as `c`) draws as its poster.
fn tile_facts<'a>(t: &'a WatchTile, c: &'a Cell) -> TileFacts<'a> {
    TileFacts {
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
    }
}

/// The grid's content (`CardSource`): the snapshot's titles and what was derived for them.
struct Titles<'a> {
    page: &'a Page,
}

impl<H: ContentLike> CardSource<H> for Titles<'_> {
    /// The loaded rows and the reserved cells past them, so the grid's extent is the list's.
    fn len(&self) -> usize {
        self.page.len()
    }
    fn elem(&self, i: usize) -> u32 {
        match self.page.cells.get(i) {
            Some(c) => c.elem,
            None => reserved_elem(i),
        }
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.page.cell_of(*e)
    }
    fn art(&self, i: usize) -> Art<'_> {
        #[cfg(test)]
        self.page.art_log.borrow_mut().push((false, i));
        match (self.page.tiles.get(i), self.page.cells.get(i)) {
            (Some(t), Some(c)) => Art::Poster(Some(tile_facts(t, c))),
            _ => {
                #[cfg(test)]
                self.page.skeleton_draws.borrow_mut().push(i);
                Art::Poster(None)
            }
        }
    }
    fn label(&self, i: usize) -> TileLabel {
        self.page.label(i)
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.page.cells.get(i)?.resume
    }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, _measure: &dyn plx_machine::machine::Measure) {
        if let Some(t) = self.page.tiles.get(i) {
            tiles::paint(p, t.state, tile);
        }
    }
    /// Every cell is painted: a reserved one, whose row has not landed, as the skeleton poster
    /// (`art` answers `Poster(None)`), so focus that enters it is on something drawn.
    fn loaded(&self, _i: usize) -> bool {
        true
    }
    /// Nothing is for the grid to ask: the list worker reads every page of the list on its own
    /// (`WatchlistStore`), so a reserved cell is waiting for a read already under way.
    fn unread(&self, _i: usize) -> bool {
        false
    }
}

/// One thing the grid draws (`WatchlistScreen::drawn_probe`).
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq)]
pub struct Drawn {
    pub guid: String,
    /// `None`: a ghost, a removed card fading out.
    pub role: Option<ui_cards::DrawRole>,
    pub rect: Rect,
    pub alpha: f32,
}

/// The count a Remove replaced and the header still shows (`Page::count_hold`).
#[derive(Clone, Copy)]
struct CountHold {
    count: Count,
}

/// The tiles a landing is fading out, in the order the grid names them (`Grid::ghost_keys`); one
/// the page did not keep is `None` and is not drawn.
struct GhostTitles<'a> {
    tiles: Vec<Option<&'a (WatchTile, Cell)>>,
    /// Test support: the ghost posters actually asked for (`Page::ghost_draws`).
    #[cfg(test)]
    drawn: &'a std::cell::Cell<usize>,
    #[cfg(test)]
    log: &'a std::cell::RefCell<Vec<(bool, usize)>>,
}

impl<H: ContentLike> CardSource<H> for GhostTitles<'_> {
    fn len(&self) -> usize {
        self.tiles.len()
    }
    /// The tile's own element; a ghost is never focused, pressed or registered, so nothing asks.
    fn elem(&self, i: usize) -> u32 {
        self.tiles.get(i).copied().flatten().map_or(0, |(_, c)| c.elem)
    }
    fn index_of(&self, e: &u32) -> Option<usize> {
        self.tiles.iter().position(|t| t.is_some_and(|(_, c)| c.elem == *e))
    }
    fn art(&self, i: usize) -> Art<'_> {
        #[cfg(test)]
        {
            self.drawn.set(self.drawn.get() + 1);
            self.log.borrow_mut().push((true, i));
        }
        Art::Poster(self.tiles.get(i).copied().flatten().map(|(t, c)| tile_facts(t, c)))
    }
    /// Read only for the focused card's ghost, which keeps its caption until the landing steps.
    fn label(&self, i: usize) -> TileLabel {
        match self.tiles.get(i).copied().flatten() {
            Some((t, c)) => caption::label(&t.row, t.state, &caption::policy(), &c.listing),
            None => TileLabel::default(),
        }
    }
    fn progress(&self, i: usize) -> Option<f32> {
        self.tiles.get(i).copied().flatten().and_then(|(_, c)| c.resume)
    }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, _measure: &dyn plx_machine::machine::Measure) {
        if let Some((t, _)) = self.tiles.get(i).copied().flatten() {
            tiles::paint(p, t.state, tile);
        }
    }
    fn loaded(&self, i: usize) -> bool {
        self.tiles.get(i).copied().flatten().is_some()
    }
}

impl<H: ContentLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = Titles<'a>;

    fn revision(&self, _cx: &Cx<'_, H>) -> u64 {
        self.rev
    }

    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: GRID_TOP, focusable: true }, HEADER_GROUP).ranked(1));
        // DS4: a removal plays (survivors spring to their cells) instead of cutting.
        out.push(SectionSpec::new(
            Sec::Items,
            Kind::Grid { spec: grid_spec().animate_landings(true) },
            GRID_GROUP,
        ).seated(plx_ui::page_regions::CONTENT_HEAD_SEAT));
        // The unreachable read-out's Try again is the one focusable thing in the status section.
        let retry = (self.list == ListState::Failed)
            .then(|| unreachable_overlay().action_frame_measured(cx.measure))
            .flatten();
        out.push(
            SectionSpec::new(Sec::Status, Kind::Overlay { rect: retry.unwrap_or(Rect::FULL), focusable: retry.is_some() }, STATUS_GROUP)
                .ranked(2),
        );
    }

    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        out.extend([Sec::Items, Sec::Status, Sec::Head]);
    }

    fn cards<'a>(&'a self, _cx: &'a Cx<'_, H>, k: Sec) -> Option<Titles<'a>> {
        (k == Sec::Items && !self.cells.is_empty()).then_some(Titles { page: self })
    }

    /// A reserved cell whose row has landed: the title now keyed in the same cell stands for it.
    fn supersede(&self, _cx: &Cx<'_, H>, want: &u32) -> Option<u32> {
        self.reserved_at(*want).and_then(|i| self.cells.get(i)).map(|c| c.elem)
    }

    fn elem_of(&self, k: Sec) -> Option<u32> {
        match k {
            Sec::Head => Some(PILL_ELEM),
            Sec::Status => (self.list == ListState::Failed).then_some(RETRY_ELEM),
            Sec::Items => None,
        }
    }

    /// Every title has a hold menu (at least Remove), so the hold hint shows on every card.
    fn card_has_menu(&self, _cx: &Cx<'_, H>, _k: Sec, _elem: &u32) -> bool {
        true
    }

    fn focus_rect(&self, cx: &Cx<'_, H>, k: Sec, section: Rect) -> Rect {
        match k {
            // The drawn pill's own rect: the header's, from the same frame `custom_draw` paints.
            // The drawn control's own plate: the row's, shifted to where the section stands.
            Sec::Head => {
                let plate = self.header_row(cx.measure).plate();
                Rect::new(plate.x, plate.y + section.y, plate.w, plate.h)
            }
            Sec::Items | Sec::Status => section,
        }
    }

    /// DS4: the removed tile fades where it stood, under the survivors that spring over its cell.
    fn paint_ghosts(&self, _k: Sec, grid: &Grid, f: &mut DrawFrame<'_, '_, H>, p: Painter) {
        let ghosts = GhostTitles {
            tiles: grid.ghost_keys().map(|key| self.ghosts.get(&key)).collect(),
            #[cfg(test)]
            drawn: &self.ghost_draws,
            #[cfg(test)]
            log: &self.art_log,
        };
        grid.paint_ghosts(f, p, &ghosts);
    }

    /// The grid's focused band follows the Under posters setting, as the Library's does.
    fn caption_policy(&self, k: Sec) -> Option<plx_ui::caption_policy::CaptionPolicy> {
        (k == Sec::Items).then(caption::policy)
    }

    /// The header section's group is the shared row's (`HeaderFocus::group_spec`: a Control row,
    /// Left and Right stopping at its ends, Up and Down the page's links); the extent is where the
    /// stack stands the section. The status section is not a header and keeps its own.
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

    /// The grid's place while the first read is on its way, declared empty so a fresh enter naming it
    /// seats nothing (`PageRegions::declare_pending`).
    fn extra_groups(&self, _cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        self.regions().declare_pending(out);
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, r: Rect, _scroll: f32) {
        let p = f.painter.alpha(f.page_alpha);
        match k {
            Sec::Head => {
                // The row rides the page: its frame is the section's own top, scroll included.
                // While the Sort & Filter menu is up the control is the opener lift's (its `opener`,
                // left out of this pass): `redraw_focused` draws the only copy, lit above the scrim.
                let lifted = f.lifted.is_some_and(|key| key.entry == self.entry && key.elem == PILL_ELEM);
                // The header row leaves with the top chrome and returns with it, on the handoff's
                // one clock (`scroll_chrome`): no ramp of its own, so a scrolled page leaves no
                // remnant of the head above its rows. It also keeps out of the band the drawn chrome
                // stands in while the scroll is still bringing it home (`Handoff::clearance`).
                let p = p.alpha(self.handoff.head_alpha(r.y));
                let row = self.header_row(f.measure);
                row.draw(p, r.y, &self.header_capsules, self.header_pop.scale_with(0, f.press.scale), lifted, &self.header_swap.borrow());
                #[cfg(feature = "devtriggers")]
                if self.dev_blocked {
                    header::draw_dev_blocked(p, r.y);
                }
            }
            Sec::Status => match self.readout() {
                Some(readout) if readout.retries() => {
                    let on = f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == RETRY_ELEM);
                    unreachable_overlay().focused(on).draw_measured(&plx_ui::Env::inert(), p, f.measure);
                }
                Some(readout) => draw_readout(readout, p, Rect::FULL),
                None => {}
            },
            Sec::Items => {}
        }
    }
}

/// The wash's rate for the ease that follows a Remove's landing: eight times the shared
/// `AmbientWash::K` (80). The cost is the frames on which a drawn 8-bit code changes (each
/// re-blurs the tab strip's glass, so the page is drawn twice), and that count falls with `ln k`,
/// not `1/sqrt(k)`: measured in the sim, 4x left 0.63 of the frames and 8x leaves 0.45 (35-37
/// down to 16 double-drawn frames). Only this screen's post-landing ease uses it; focus moves and
/// every other page, and a sort or filter's landing, keep the shared rate.
const POST_REMOVE_EASE_K: f32 = 8.0 * AmbientWash::K;

pub struct WatchlistScreen {
    page: Page,
    /// The page's layout, one scroll, groups, reconcile and the grid (`ui::cards::Stack`).
    stack: Stack<Sec>,
    ground: PageGround,
    ground_seeded: bool,
    /// The target last handed to the wash: what a Remove's landing holds it at.
    ground_applied: [[f32; 4]; 4],
    /// A landing held the wash, so the ease that follows it runs at [`POST_REMOVE_EASE_K`] until it
    /// has reached its target.
    ground_catching_up: bool,
    /// A Remove's tile left and its landing has not yet run its course: only then does the ease
    /// after the landing run at [`POST_REMOVE_EASE_K`]. A sort or filter landing holds the wash
    /// the same way (`landing_in_flight`) and eases back at the shared rate, as a focus move does.
    removal_owed: bool,
    /// The focused title's index as the engine last told us (the last one while the pill holds
    /// focus): the screen keeps no focus of its own, this mirrors the engine's for `press`.
    focus: usize,
    /// The element that index was last read from while the page held the engine's focus: while a
    /// menu holds it instead (Sort & Filter reflows the grid under its own menu) the index follows
    /// this element through the reflow, not whichever title the new list puts at the old index.
    focus_elem: Option<u32>,
    /// The page's own arrival seat, while nobody has chosen another (the screens guide's "a seat
    /// nobody chose follows the head"): the group it last seated (`Some(None)`: it asked to seat
    /// nothing, while the first read is on its way), `None` once the reader acts, the page is
    /// restored, or the first tile holds it. See [`follow_seat`](Self::follow_seat).
    seat: Option<Option<GroupId>>,
    /// Seats this page asked the engine for whose `Enter` has not come back yet: those are the
    /// page's own, not a new arrival.
    own_enters: u8,
    /// How many of the [`own_enters`](Self::own_enters) still in flight were queued BEFORE the
    /// reader acted (the oldest ones: the dispatcher's queue is FIFO). A drain budget can carry a
    /// seat `Enter` past the reader's next key, and the engine seats an `Enter` whatever the page
    /// answers, so the page cannot refuse it: it puts the reader's focus back when it arrives
    /// (see [`step`](Machine::step)).
    stale_enters: u8,
}

impl WatchlistScreen {
    /// The header's group and its stops (the Sort & Filter pill alone): the per-page probe the
    /// app-tier route tables read in place of the private group ids.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header(&self) -> (GroupId, Vec<(&'static str, u32)>) { (HEADER_GROUP, vec![("sort", PILL_ELEM)]) }

    /// The scroll the page is travelling to: 0 at its top.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_scroll_target(&self) -> f32 { self.stack.target() }

    /// The sort control's criterion, the one text it draws: what the header draws is built from
    /// this one string (`header_row`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_pill_value(&self) -> String { self.page.options.summary() }

    /// How many header rows this page has built: the build counter of its `RowCache`/`SharedRow`.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header_builds(&self) -> u32 { self.page.header_rows.builds() }

    /// Is the header row carrying anything over from the row it replaced (a criterion cross-fading, a
    /// read-out dissolving)?
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_header_swap_active(&self) -> bool { self.page.header_swap.borrow().is_active() }

    /// The heartbeat word, byte-identical to the route word (`tests/manifest.json` selects on it).
    pub const WORD: &'static str = "watchlist";

    pub const SHAPE: &'static str = "WatchlistScreen{entry:u32,next_elem:u32,list:{tag:u32,total:u64},options:{type:u32,sort:u32,descending:bool,show:u32},tiles:[{guid:str,state:u32,elem:u32}],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Grid{bands,pop,landing}]},header_capsules:TabStrip{sel:Capsule{x:Spring{pos:f32,vel:f32},w:Spring{pos:f32,vel:f32},a:Spring{pos:f32,vel:f32},at:u32},foc:Capsule{x:Spring{pos:f32,vel:f32},w:Spring{pos:f32,vel:f32},a:Spring{pos:f32,vel:f32},at:u32}},header_pop:{sp:[Spring{pos:f32,vel:f32};1],focused:Option<u32>},handoff:Handoff{tucked:bool,chrome:f32,capsule:f32}}";

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
            art_server: None,
            ghosts: HashMap::new(),
            count_hold: None,
            dev_blocked: false,
            #[cfg(test)]
            ghost_draws: std::cell::Cell::new(0),
            #[cfg(test)]
            skeleton_draws: Default::default(),
            #[cfg(test)]
            art_log: Default::default(),
            rev: 0,
            header_capsules: plx_ui::widgets::TabStrip::new(),
            header_pop: plx_ui::widgets::CtlPop::new(),
            handoff: plx_ui::scroll_chrome::Handoff::new(),
            header_rows: plx_ui::page_header::SharedRow::new(),
            header_swap: std::cell::RefCell::new(plx_ui::page_header::HeaderSwap::new()),
        };
        page.rebuild();
        Self {
            page,
            // A page that leaves focus (a menu over it, a pointer gone) rests at the top.
            stack: Stack::new(entry).hold_hint().home_when_unfocused(true),
            ground: PageGround::new(),
            ground_seeded: false,
            ground_applied: [theme::SURFACE_APP; 4],
            ground_catching_up: false,
            removal_owed: false,
            focus: 0,
            focus_elem: None,
            seat: None,
            own_enters: 0,
            stale_enters: 0,
        }
    }

    /// The page as it mounts: nothing landed yet, so no tiles and no read-out. The application
    /// feeds it the list state, the rows and the options ([`WatchlistIn`]) from its store.
    pub fn loading(entry: EntryId) -> Self {
        let mut screen = Self::new(Vec::new(), WatchlistOptionsState::default()).at_entry(entry);
        screen.page.list = ListState::Loading;
        screen.page.touch();
        screen
    }

    /// The same page mounted as `entry`.
    pub fn at_entry(mut self, entry: EntryId) -> Self {
        self.page.entry = entry;
        self.stack = Stack::new(entry).hold_hint().home_when_unfocused(true);
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

    /// Q7: the count just left of the pill.
    pub fn count(&self) -> Option<Count> {
        self.page.count()
    }

    /// The focused tile's index in the shown order, if there is one.
    pub fn focused(&self) -> Option<usize> {
        (self.focus < self.page.tiles.len()).then_some(self.focus)
    }

    /// The focused cell's position in the grid, a reserved cell past the loaded tail included:
    /// where the hold window follows the focus to.
    pub fn cursor(&self) -> usize {
        self.focus
    }

    /// The element of the tile at `index`.
    pub fn elem_at(&self, index: usize) -> Option<u32> {
        self.page.cells.get(index).map(|c| c.elem)
    }

    /// The tile showing element `elem`.
    pub fn index_of(&self, elem: u32) -> Option<usize> {
        self.page.index.get(&elem).copied()
    }

    /// **The Sort & Filter control, lit above its menu's scrim** (the opener lift, design review
    /// S12): painted as the focused control at the row's drawn place, with the head's own tuck
    /// alpha (the handoff's `chrome`, which the page's pass wears too), since the page's pass leaves
    /// it out while the menu is up. A control the top chrome has left (`chrome` 0) draws nothing, as
    /// the row would not.
    fn redraw_opener<H: ContentLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let Some(placed) = Focusable::<H>::place(self, &PILL_ELEM, f.cx, plx_ui::screen::At::Drawn) else { return };
        let row = self.page.header_row(f.measure);
        let dy = placed.rect.y - row.plate().y;
        let p = f.painter.alpha(f.page_alpha).alpha(self.page.handoff.head_alpha(dy));
        row.draw_opener(p, dy, self.page.header_pop.scale_with(0, f.press.scale), &self.page.header_swap.borrow());
    }

    /// The Sort & Filter control as the focused pill draws it, in the page's document space: the
    /// rect the menu's morph grows out of.
    pub fn pill_rect(&self, measure: &dyn plx_machine::machine::Measure) -> Rect {
        self.page.header_row(measure).pill()
    }

    /// Take an input. `Rows` and `Options` replace the state; `Removed` drops the tile, leaving
    /// the engine's reconcile to put focus on the title that took its index (the next one), and
    /// the grid's landing to play the survivors into their cells (DS4).
    pub fn input(&mut self, ev: WatchlistIn) {
        // What the header shows now: an input that moves the cards (rows, options) moves the count
        // with them, so the number it replaces stays until the landing's first step.
        let shown = self.page.count();
        match ev {
            WatchlistIn::Rows(rows) => {
                self.page.replace_tiles(rows);
                self.hold_count(shown);
            }
            WatchlistIn::Options(options) => {
                self.page.options = options;
                self.page.touch();
                self.hold_count(shown);
            }
            WatchlistIn::Removed(guid) => {
                let rows: Vec<WatchTile> = self.page.tiles.iter().filter(|t| t.row.guid != guid).cloned().collect();
                let gone = rows.len() < self.page.tiles.len();
                self.removal_owed |= gone;
                if gone && self.page.count_hold.is_none() {
                    self.page.count_hold = self.page.count().map(|count| CountHold { count });
                }
                self.page.facts.remove(&guid);
                if let (ListState::Ready { total }, true) = (&mut self.page.list, gone) {
                    *total = total.saturating_sub(1);
                }
                self.page.replace_tiles(rows);
            }
            WatchlistIn::List(list) => {
                self.page.list = list;
                self.page.touch();
            }
            WatchlistIn::Facts(guid, facts) => {
                self.page.facts.insert(guid, facts);
                self.page.rebuild();
            }
            WatchlistIn::ArtServer(server) => {
                if self.page.art_server != server {
                    self.page.art_server = server;
                    self.page.rebuild();
                }
            }
            WatchlistIn::DevBlocked(on) => {
                if self.page.dev_blocked != on {
                    self.page.dev_blocked = on;
                    self.page.touch();
                }
            }
        }
        // A focus past the new end (a removal at the tail) is clamped; one on a reserved cell
        // keeps its position, which `focused()` answers as no title.
        self.focus = self.focus.min(self.page.len().saturating_sub(1));
    }

    /// Keep `shown`, the count the header showed before an input that moves the cards, until the
    /// grid's landing steps ([`release_count_hold`](Self::release_count_hold)), when the input
    /// changed what the count reads. A hold already kept stays: it is the older number.
    fn hold_count(&mut self, shown: Option<Count>) {
        if self.stack.grid(Sec::Items).is_some() && self.page.count_hold.is_none() && shown.is_some() && self.page.live_count() != shown {
            self.page.count_hold = shown.map(|count| CountHold { count });
        }
    }

    /// OK on the tile at `index`, per the plan §1.9 table. Reads the drawn, committed state.
    pub fn press_at(&self, index: usize) -> Option<WatchlistOut> {
        let t = self.page.tiles.get(index)?;
        Some(match (t.state, t.copy) {
            // Q6 (default): coming soon AND matched keeps the clock and stays playable.
            (TileState::Available | TileState::ComingSoonMatched, Some(copy)) => WatchlistOut::OpenDetail(copy),
            // The catalog page: the application pushes `ContentArg::Catalog` for the guid.
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

    /// The ground the focused title keys, as a target: its matched item's UltraBlur colours, else
    /// the flat app surface. A discover row carries no colours of its own (plan §1.6), so the
    /// matched PMS item's are the only source.
    ///
    /// **Held while a Remove's landing is in flight**: the wash eases one 8-bit step a frame, and
    /// every step invalidates the tab strip's glass, so the page is drawn twice for each landing
    /// frame. The target stays at what the wash was last given; once the landing ends it eases to
    /// the newly focused title at [`POST_REMOVE_EASE_K`] (until it is at rest, then the shared rate).
    ///
    /// **Never through neutral.** A focused title whose colours have not been loaded yet (a matched
    /// copy whose facts the store has not sent) keeps the key the wash has, and eases to the title's
    /// own once they arrive: the flat surface is the answer for a page that has no key, or for a title
    /// that has none (facts in, no colours; no copy to ask), never a step on the way between two keys.
    fn ground_target(&self) -> [[f32; 4]; 4] {
        const FLAT: [[f32; 4]; 4] = [theme::SURFACE_APP; 4];
        if self.ground_seeded && self.landing_in_flight() {
            return self.ground_applied;
        }
        match self.page.cells.get(self.focus).and_then(|c| c.blur) {
            Some(blur) => AmbientWash::keyed(blur, PageGround::CARD_W),
            None if self.ground_seeded && self.ground_applied != FLAT && self.page.colours_pending(self.focus) => {
                self.ground_applied
            }
            None => FLAT,
        }
    }

    /// A landing is in flight (a Remove, a sort or a filter): the grid's landing is playing (or waiting for the menu's
    /// veil to drop) or still fading the removed tile's ghost, or the page still holds the ghost.
    pub(crate) fn landing_in_flight(&self) -> bool {
        !self.page.ghosts.is_empty()
            || self.stack.grid(Sec::Items).is_some_and(|g| g.landing_pending() || g.has_ghosts())
    }

    /// The header's count follows the cards when the grid's landing STARTS: not at the commit (a
    /// Remove's hold menu is still up, a Sort & Filter click has drawn only the frame before), not
    /// under a menu's veil, but on the frame the springs step. A change the grid made no landing for
    /// releases on the next tick.
    fn release_count_hold(&mut self) {
        if self.page.count_hold.is_none() {
            return;
        }
        let grid = self.stack.grid(Sec::Items);
        let pending = grid.is_some_and(|g| g.landing_pending());
        let waiting = grid.is_some_and(|g| g.landing_waiting());
        if !(pending && waiting) {
            self.page.count_hold = None;
            self.page.touch();
        }
    }

    fn tick<H: ContentLike>(&mut self, t: Tick, cx: &Cx<'_, H>) {
        // The focused cell's index, a reserved one included: `focused()` then answers no title,
        // and the ground falls back to the flat surface, rather than keying a title left behind.
        if let Some((key, i)) = cx
            .focus
            .current
            .filter(|key| key.entry == self.page.entry)
            .and_then(|key| Some((key, self.page.cell_of(key.elem)?)))
        {
            self.focus = i;
            self.focus_elem = Some(key.elem);
        } else if !cx.focus.current.is_some_and(|key| key.entry == self.page.entry) {
            // a menu holds the engine's focus: the page's own title moved with the reflow
            if let Some(i) = self.focus_elem.and_then(|e| self.page.cell_of(e)) {
                self.focus = i;
            }
        }
        // The removed tiles are let go once the grid has nothing left to fade (the step ran the
        // stack first, so a landing the last input started is already pending here).
        if !self.page.ghosts.is_empty()
            && !self.stack.grid(Sec::Items).is_some_and(|g| g.landing_pending() || g.has_ghosts())
        {
            self.page.ghosts.clear();
        }
        self.release_count_hold();
        self.page.header_row(cx.measure).step(self.page.entry, cx.focus.current, &mut self.page.header_capsules, &mut self.page.header_pop, t.dt());
        // After the row is current: a rebuild this tick began the swap, and it takes its first step now.
        self.page.header_swap.get_mut().step(t.dt());
        // The stack stepped the scroll on this tick already (`step` runs it first).
        self.page.handoff.step(t.dt(), self.stack.scroll(), self.stack.target(), grid_spec().pitch(), false);
        let held = self.ground_seeded && self.landing_in_flight();
        let target = self.ground_target();
        self.ground_applied = target;
        if held && self.removal_owed {
            self.ground_catching_up = true;
        }
        if !held && !self.ground_catching_up {
            // a Remove that played no landing owes no ease
            self.removal_owed = false;
        }
        if self.ground_seeded {
            if self.ground_catching_up {
                self.ground.key_target_k(target, POST_REMOVE_EASE_K, t.dt());
                if !held && self.ground.at_rest_on(target) {
                    self.ground_catching_up = false;
                    self.removal_owed = false;
                }
            } else {
                self.ground.key_target(target, t.dt());
            }
        } else {
            self.ground.jump_target(target);
            self.ground_seeded = true;
        }
    }

    /// Send `out` where it goes: an existing content effect, or the Watchlist request the
    /// application routes (`app::content::watchlist_requests`).
    fn route<H: ContentLike>(&mut self, out: WatchlistOut, fx: &mut Effects<'_, H>) {
        match out {
            WatchlistOut::OpenDetail(m) => fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Push(
                ContentArg::Detail { sid: m.server_id, rk: m.rating_key.to_string() },
            )))),
            other => {
                fx.push(plx_machine::machine::Fx::App(AppFx::Watchlist(other)));
                fx.invalidate(Provenance::Input);
            }
        }
    }

    /// **Seat the arrival in the page, and follow it to the grid when the tiles land.** A strip
    /// tab's mint asks for group 0, the grid, but the page mounts before its rows are fed: with the
    /// grid empty the engine fell back to the first group it was given, the strip, at its left end
    /// (the account chip). Search seats its field and the Library its head the same way, through a
    /// fresh `Enter` addressed to themselves. While the first read is on its way the page seats
    /// NOTHING (`PageRegions::unseat`, the rule the Library and Recommended follow), and the first
    /// tile takes the arrival when it lands unless the reader has moved first; a list that landed
    /// empty or failed keeps the header's pill.
    fn seat_first<H: ContentLike>(&mut self, fx: &mut Effects<'_, H>) {
        let (want, target) = self.with_regions(|regions| {
            (regions.first_seat(), regions.first_seat().map(FocusTarget::FirstInGroupAnimated).or_else(|| regions.unseat()))
        });
        // The first tile is where the arrival ends: nothing lands above it to follow.
        self.seat = (want != Some(GRID_GROUP)).then_some(want);
        if let Some(target) = target {
            self.seat_own(target, fx);
        }
    }

    /// Re-seat the arrival when what the page can offer has moved (`seat_first`), while it is still
    /// the page's own.
    fn follow_seat<H: ContentLike>(&mut self, fx: &mut Effects<'_, H>) {
        let Some(seated) = self.seat else { return };
        if seated != self.with_regions(|regions| regions.first_seat()) {
            self.seat_first(fx);
        }
    }

    /// Ask the engine to seat `focus` as the page's OWN seat: counted in [`own_enters`], so the
    /// `Enter` that comes back is not mistaken for a new arrival.
    ///
    /// [`own_enters`]: Self::own_enters
    fn seat_own<H: ContentLike>(&mut self, focus: FocusTarget<u32>, fx: &mut Effects<'_, H>) {
        self.own_enters = self.own_enters.saturating_add(1);
        fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))));
    }

    /// **This page's column** (`plx_ui::page_regions`): see [`Page::regions`].
    fn with_regions<R>(&self, then: impl FnOnce(&PageRegions<'_>) -> R) -> R {
        then(&self.page.regions())
    }

    /// **BACK**, decided by [`PageRegions::back`] and executed here: a scrolled page first returns
    /// to its top, on the first row in the column focus was in; from there it leaves for Home.
    fn back<H: ContentLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        let focus = cx.focus.current.filter(|key| key.entry == self.page.entry);
        let step = self.with_regions(|regions| {
            let at = focus.and_then(|key| regions.region_of(key.elem, Focusable::<H>::group_of(self, &key.elem, cx)));
            regions.back(at, self.stack.target() > 0.5)
        });
        match step {
            BackStep::Top => {
                let column = focus.and_then(|key| self.page.cell_of(key.elem)).map_or(0, |index| index % COLS);
                match self.page.cells.get(column).or_else(|| self.page.cells.last()) {
                    Some(cell) => self.seat_own(FocusTarget::Elem(FocusKey { entry: self.page.entry, elem: cell.elem }), fx),
                    None => self.seat_own(FocusTarget::ContainerGroup(HEADER_GROUP), fx),
                }
            }
            BackStep::Content => self.seat_own(FocusTarget::ContainerGroup(GRID_GROUP), fx),
            BackStep::Header => self.seat_own(FocusTarget::ContainerGroup(HEADER_GROUP), fx),
            BackStep::Nav => self.seat_own(FocusTarget::ContainerGroup(plx_ui::containers::tabs::STRIP), fx),
            BackStep::Leave => self.route(WatchlistOut::Back, fx),
        }
        Handled::Yes
    }

    /// A non-card element activated, by an `Activate` (a `Bare` element, a pointer) or by a committed
    /// press on it (the strip's `Control` elements): Try again, the Sort & Filter pill, the strip.
    fn activate<H: ContentLike>(&mut self, elem: u32, fx: &mut Effects<'_, H>) -> Handled {
        match elem {
            RETRY_ELEM => self.route(WatchlistOut::Retry, fx),
            PILL_ELEM => self.route(WatchlistOut::OpenOptions, fx),
            elem if elem >= plx_ui::dispatch::STRIP_BASE => match Self::strip_press(elem) {
                Some(Some(out)) => self.route(out, fx),
                Some(None) => {}
                None => return Handled::No,
            },
            _ => return Handled::No,
        }
        Handled::Yes
    }

    /// A press on the shared strip (the container composes it above the page): the other tabs and
    /// the chip are requests; the Watchlist mark is the page the user is already on.
    fn strip_press(elem: u32) -> Option<Option<WatchlistOut>> {
        use plx_ui::widgets::{TAB_KEY_CHIP, TAB_KEY_HOME, TAB_KEY_MOVIES, TAB_KEY_SEARCH, TAB_KEY_SHOWS, TAB_KEY_WATCHLIST};
        match elem {
            TAB_KEY_HOME => Some(Some(WatchlistOut::Tab(HomeTab::Home))),
            TAB_KEY_MOVIES => Some(Some(WatchlistOut::Tab(HomeTab::Movies))),
            TAB_KEY_SHOWS => Some(Some(WatchlistOut::Tab(HomeTab::Shows))),
            TAB_KEY_SEARCH => Some(Some(WatchlistOut::Tab(HomeTab::Search))),
            TAB_KEY_CHIP => Some(Some(WatchlistOut::Account)),
            TAB_KEY_WATCHLIST => Some(None),
            plx_ui::widgets::TAB_KEY_LIVETV => Some(Some(WatchlistOut::Tab(HomeTab::LiveTv))),
            _ => None,
        }
    }

    /// The page's `Focusable` (see `focusable_via_view!` below): the stack's view over the page.
    fn view<H: ContentLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    /// How far the page has tucked the top chrome: the Library's handoff, decided from the scroll
    /// target (the first scrolled rest is a whole row, one pitch, down) and run on the swap clock.
    /// The header row leaves with it, on the same clock; this page has no capsule to hand over to.
    fn tuck(&self) -> f32 {
        self.page.handoff.tuck()
    }

    /// The page's own top edge once the chrome has tucked away: the eased 72 px fade of the ground, as
    /// the Library draws it. No capsule: the header names no library, so there is nothing to restate
    /// (`plx_ui::scroll_chrome::capsule_shown(0)` is false; the Library asks it of its own header).
    fn draw_chrome<H: ContentLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        let tuck = self.tuck();
        if tuck <= 0.0 {
            return;
        }
        let ground = self.ground.sample(Rect::FULL, SCR_W * 0.5, 0.0);
        plx_ui::scroll_capsule::draw_top_fade(f.painter.alpha(f.page_alpha), SCR_W, ground, tuck, None);
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

    /// Poster `i`'s settled cell as the grid stands now, the caption band folded in: where an app-tier
    /// test reads how much room the focused row opened under its posters.
    #[cfg(any(test, feature = "test-support"))]
    pub fn probe_cell(&self, i: usize) -> Option<plx_ui::Rect> {
        self.stack.grid(Sec::Items).map(|grid| grid.cell_probe(i))
    }

    /// Test support: the focused caption band's height the grid opens (0 / 80 / 112 by the policy).
    #[cfg(test)]
    pub(crate) fn band_h(&self) -> Option<f32> {
        self.stack.grid(Sec::Items).map(|g| g.band_h())
    }

    /// Test support: the page scroll.
    #[cfg(test)]
    pub(crate) fn scroll(&self) -> f32 {
        self.stack.scroll()
    }

    /// Test support: put the page at scroll `y` at once.
    #[cfg(test)]
    pub(crate) fn jump_scroll(&mut self, y: f32) {
        self.stack.jump_to(y);
        // A jump lands on the decided state, as the next tick's step would see it.
        self.page.handoff.step(0.0, y, y, grid_spec().pitch(), false);
    }

    /// Test support: the alpha the header row is drawn with at the page's scroll (it leaves with the
    /// top chrome and keeps out of the band the chrome is drawn in): the very value `custom_draw` wears.
    #[cfg(test)]
    pub(crate) fn head_alpha(&self) -> f32 {
        self.page.handoff.head_alpha(-self.stack.scroll())
    }

    /// Test support: the ground colours the focused title keys (before the wash eases to them).
    #[cfg(test)]
    pub(crate) fn ground_goal(&self) -> [[f32; 4]; 4] {
        self.ground_target()
    }

    /// Test support: the wash's corners as the 8-bit codes the GPU is sent.
    #[cfg(test)]
    pub(crate) fn ground_codes(&self) -> Vec<i32> {
        self.ground.corners().iter().flatten().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as i32).collect()
    }

    /// Test support: the leaving copies the grid draws, `(index, rect, alpha)`.
    #[cfg(test)]
    pub(crate) fn leaving_probe(&self) -> Vec<(usize, Rect, f32)> {
        self.stack.grid(Sec::Items).map(|g| g.leaving_rects(self.page.len())).unwrap_or_default()
    }

    /// Test support: the grid's removal landing has stepped (it is playing and no longer waiting).
    #[cfg(test)]
    pub(crate) fn landing_started(&self) -> bool {
        self.stack.grid(Sec::Items).is_some_and(|g| g.landing_pending() && !g.landing_waiting())
    }

    /// Test support: the grid's ghost keys (`Grid::ghost_keys`) and the keys of the removed tiles
    /// the page kept for them.
    #[cfg(test)]
    pub(crate) fn ghost_probe(&self) -> (Vec<u64>, Vec<u64>) {
        let named = self.stack.grid(Sec::Items).map(|g| g.ghost_keys().collect()).unwrap_or_default();
        (named, self.page.ghosts.keys().copied().collect())
    }

    /// Test support: the grid's content, as the stack reads it.
    #[cfg(test)]
    pub(crate) fn cards_for_test(&self) -> impl ui_cards::CardSource<crate::watchlist::cards_harness::WatchHost> + '_ {
        Titles { page: &self.page }
    }

    /// Test support: the reserved cells drawn since the last call, by index.
    #[cfg(test)]
    pub(crate) fn take_skeleton_draws(&self) -> Vec<usize> {
        std::mem::take(&mut *self.page.skeleton_draws.borrow_mut())
    }

    /// Test support: the ghost posters the grid has drawn so far.
    #[cfg(test)]
    pub(crate) fn ghost_draws(&self) -> usize {
        self.page.ghost_draws.get()
    }

    /// Test support: the posters the grid asked for since the last call, in the order it asked:
    /// `(is a ghost, index)`. That is the draw order itself.
    #[cfg(test)]
    pub(crate) fn take_art_log(&self) -> Vec<(bool, usize)> {
        std::mem::take(&mut *self.page.art_log.borrow_mut())
    }

    /// Test support: everything the grid draws now, back to front, as the guid it shows, the
    /// card's role (`None` for a ghost), its rect and its alpha. `focus` is the element focus is
    /// on. Ghosts first (the page paints them before the grid), then the leaving copies, then the
    /// cards.
    #[cfg(any(test, feature = "test-support"))]
    pub fn drawn_probe(&self, focus: Option<u32>) -> Vec<Drawn> {
        let Some(grid) = self.stack.grid(Sec::Items) else { return Vec::new() };
        let n = self.page.len();
        let guid_of = |i: usize| self.page.tiles.get(i).map(|t| t.row.guid.clone());
        let mut out: Vec<Drawn> = grid
            .ghost_slots()
            .into_iter()
            .filter_map(|(key, rect, alpha)| {
                let guid = self.page.ghosts.get(&key)?.0.row.guid.clone();
                Some(Drawn { guid, role: None, rect, alpha })
            })
            .collect();
        let focus = focus.and_then(|e| self.page.cell_of(e));
        out.extend(grid.plan_probe(n, focus).into_iter().filter_map(|(role, i, rect, alpha)| {
            Some(Drawn { guid: guid_of(i)?, role: Some(role), rect, alpha })
        }));
        out
    }

    /// Test support: the title the focus caption shows now: the focused card's ghost while the
    /// landing holds the caption of a card that left, else the focused tile's own.
    #[cfg(any(test, feature = "test-support"))]
    pub fn caption_probe(&self, focus: Option<u32>) -> Option<String> {
        let grid = self.stack.grid(Sec::Items)?;
        if let Some(key) = grid.ghost_caption() {
            return self.page.ghosts.get(&key).map(|g| g.0.row.title.clone());
        }
        let i = self.page.cell_of(focus?)?;
        self.page.tiles.get(i).map(|t| t.row.title.clone())
    }

    /// The poster path and server the tile at `i` asks the poster store for (empty path: the
    /// skeleton); the app's wiring tests read it too.
    pub fn art_of(&self, i: usize) -> Option<(String, u16)> {
        self.page.cells.get(i).map(|c| (c.thumb.clone(), c.src))
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
            ListState::Failed => c.u32(3).u64(0),
        };
        page.options.write_canon(c);
        c.seq(page.tiles.len());
        for (t, cell) in page.tiles.iter().zip(&page.cells) {
            c.str(&t.row.guid).u32(t.state as u32).u32(cell.elem);
        }
        self.stack.write(c);
        page.header_capsules.write_motion(c);
        page.header_pop.write_motion(c);
        page.handoff.write(c);
    }

    fn probe(&self, out: &mut String) {
        let (list, total) = match self.page.list {
            ListState::Loading => ("loading", 0),
            ListState::Ready { total } => ("ready", total),
            ListState::Unavailable => ("unavailable", 0),
            ListState::Failed => ("failed", 0),
        };
        out.push_str(&format!(
            "watchlist tiles={} next={} focus={} list={list} total={total}",
            self.page.tiles.len(),
            self.page.keys.next,
            self.focus,
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
        // The reader acting at the seat makes it theirs. It must be claimed HERE, on the event: an
        // OK-down arms a press the dispatcher commits only if focus is still on its key, and this
        // page's next Tick may run before that commit.
        if let ScreenEvent::Input(InputEvent { kind, .. }) = ev {
            if matches!(kind, InputKind::Key { edge: Edge::Down, .. } | InputKind::Pointer { .. }
                | InputKind::Click { .. } | InputKind::Drag { .. } | InputKind::Wheel { .. }) {
                self.seat = None;
                // Whatever seat Enters are still queued were asked for before this act.
                self.stale_enters = self.own_enters;
            }
        }
        match ev {
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx);
                self.follow_seat(fx);
                Handled::Yes
            }
            ScreenEvent::Enter(enter) => {
                match enter {
                    Enter::Fresh { .. } if self.own_enters > 0 => {
                        self.own_enters -= 1;
                        if self.stale_enters > 0 {
                            // The reader acted after this seat was asked for and before it came
                            // back: the engine is about to seat it over their move. Ask for their
                            // focus back, as an Enter of the page's own like the seat itself (so a
                            // key before THAT one is stale in the same way).
                            self.stale_enters -= 1;
                            if let Some(held) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                                self.own_enters = self.own_enters.saturating_add(1);
                                fx.push(Fx::Deliver(fx.from(), Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                                    focus: FocusTarget::Elem(held),
                                }))));
                            }
                        }
                    }
                    // The container's mint: a page shown for the first time in this visit.
                    Enter::Fresh { .. } => self.seat_first(fx),
                    // Restored (an evicted page coming back): the seat is the reader's.
                    Enter::Restored => {
                        self.seat = None;
                        self.stale_enters = self.own_enters;
                    }
                }
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::Uncover => {
                fx.invalidate(Provenance::Nav);
                Handled::Yes
            }
            ScreenEvent::FocusMoved { .. } | ScreenEvent::StoreChanged(..) => {
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Activate(elem) => self.activate(*elem, fx),
            // A card's commit is the card's. Anything else focused here committed a press too: the
            // strip is a `Control` group (`TabContainer::strip_group`), so the remote's OK on a
            // pill or the chip arms a press and arrives HERE, never as `Activate`.
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    if let Some(out) = self.index_of(elem).and_then(|i| self.press_at(i)) {
                        self.route(out, fx);
                    }
                } else if let Some(key) = cx.focus.current.filter(|key| key.entry == self.page.entry) {
                    self.activate(key.elem, fx);
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
                self.back(cx, fx)
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike> Screen<H> for WatchlistScreen {
    /// The cards are moving (a landing plays): the host a Sort & Filter menu stands on follows it.
    fn reflowing(&self) -> bool {
        self.landing_in_flight()
    }
    /// Where the focused tile sits, for the hold menu's anchor. A title is not carried by it: the
    /// hold request names the guid, so the menu never re-reads the list.
    fn focused_card_rect(&self, cx: &Cx<'_, H>, focus: Option<FocusKey<u32>>, at: plx_ui::screen::At) -> Option<Rect> {
        let key = focus.filter(|k| k.entry == self.page.entry && k.elem >= FIRST_CARD_ELEM)?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|placed| placed.rect)
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<FocusKey<u32>>) {
        if focus.is_some_and(|key| key.entry == self.page.entry && key.elem == PILL_ELEM) {
            return self.redraw_opener(f);
        }
        self.stack.view(&self.page).redraw_focused(f, focus);
    }
    fn opener_departing(&self, key: FocusKey<u32>) -> bool {
        self.stack.view(&self.page).opener_departing::<H>(key)
    }
    fn glides(&self) -> bool { true }
    fn name(&self) -> &'static str {
        Self::WORD
    }
    /// The top chrome tucks away as the first row scrolls off (the Library's rule).
    fn chrome_tuck(&self) -> f32 {
        self.tuck()
    }
    fn state(&self) -> &dyn LogicalState {
        self
    }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, _budget: &mut plx_ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        // The app's ground first, as every owned page lays it: `PageGround::draw` skips a ground
        // resolved to the flat app surface (the read-outs) because this clear already painted it.
        plx_gfx::gfx::frame_clear(theme::CLEAR_RGB.0, theme::CLEAR_RGB.1, theme::CLEAR_RGB.2);
        let p = f.painter.alpha(f.page_alpha);
        self.ground.draw(p, Rect::FULL);
        self.stack.view(&self.page).paint(f);
        self.draw_chrome(f);
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
    /// The bar, the pill and the grid are one column: DOWN from the bar enters the pill, DOWN from
    /// the pill enters the grid, UP from the first row returns to the pill and UP from the pill to
    /// the bar (on the Watchlist's own pill).
    fn links(&self, out: &mut Vec<Link>) {
        self.with_regions(|regions| regions.links(out));
    }
    fn holds_arrival(&self, dir: plx_ui::screen::Dir) -> bool {
        self.with_regions(|regions| regions.holds(dir))
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
