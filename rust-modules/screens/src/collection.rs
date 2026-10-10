//! Owned collection page: a Person-like header over a stable six-column portrait grid. The
//! Collection store owns resolution and paging; this screen owns only presentation state and the
//! identity→element registry that lets Back restore the exact member tile.
//! The page is a `plx_ui::cards::Stack` of three sections (`Sec`): the head (drawn here, one focus
//! element when the summary truncates), the member grid (this screen supplies its `CardSource`) and
//! the status read-outs. The stack owns the layout, the one scroll, the groups and their seats,
//! placement, focus recovery, the paging request and the item-menu opener redraw; `Page` is the
//! content those sections read, and `Focusable` comes from the stack's view.

use std::ffi::CString;

use plx_data::collection::{Collection, CollectionOrder, CollectionStatus, CollectionTarget, PAGE_SIZE};
use plx_plex::plex::collections::CollectionRef;
use plx_data::pms::PmsMovie;
use plx_data::stores::collection::CollectionCmd;
use plx_ui::cards::{self as ui_cards, TileLabel};
use plx_ui::cards::{CardEvent, CardSource, GridSpec, Kind, SectionSpec, Stack, StackEvent, StackPage, Tile};
use plx_ui::consts::*;
use plx_ui::label::{Label, VAlign};
use plx_machine::machine::{Canon, Cx, Edge, Effects, EntryId, GroupId, Handled, Host, InputEvent,
    InputKind, Key, Leave, LogicalState, Machine, Measure, Tick};
use plx_machine::present::Provenance;
use plx_ui::screen::{At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind,
    FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Link,
    RenderStrategy, Screen, ScreenEvent, Seat};
use plx_ui::text_view::TextView;
use plx_ui::theme;
use plx_ui::widgets::{self, Art, PageGround, StatusKind, StatusOverlay};
use plx_ui::{Env, Painter, Rect};

use super::registry::{tile_facts, AppFx, CardKeys, CardPageMemory, CollectionLike, ContentArg,
    ContentLike, ContentPanel, ContentReq, PageMemory};

#[cfg(test)]
pub(crate) mod cards_harness; // Tier 2 card conformance (cards_conformance_tests.rs)
#[cfg(test)]
mod window_tests; // the bounded projection, the window sent to the store, a hole's landing

const HEADER_ELEM: u32 = 0;
const RETRY_ELEM: u32 = 1;
const FIRST_CARD_ELEM: u32 = 0x1000;
/// Elements at and above this belong to grid slots whose member is not held (a page not read yet,
/// or evicted): slot `i` is `HOLE_ELEM_BASE + i`. Real member keys are interned below it.
///
/// The range is `HOLE_ELEM_BASE..HOLE_ELEM_BASE + 0x3fff_ffff`: above `geom::NAMESPACE_FLOOR`
/// (a hole is not an index into any index-keyed widget, and none here treats it as one), clear of
/// the route-family band (`registry::BAND`, `ALERT`) that a different screen carves from
/// `0x4000_0000`, and below the container's strip (`dispatch::STRIP_BASE`). Member keys count up
/// from `FIRST_CARD_ELEM`; `Page::project` fails loudly before they could reach the base.
const HOLE_ELEM_BASE: u32 = 0x5000_0000;
const _: () = assert!(HOLE_ELEM_BASE >= plx_ui::geom::NAMESPACE_FLOOR);
const _: () = assert!(HOLE_ELEM_BASE as u64 + 0x3fff_ffff < plx_ui::dispatch::STRIP_BASE as u64);
const _: () = assert!(HOLE_ELEM_BASE > super::registry::ALERT + 0x100);

fn hole_elem(index: usize) -> u32 { HOLE_ELEM_BASE + index.min(0x3fff_ffff) as u32 }

/// The page's sections, in document order. Each has the focus group the page names it by
/// ([`GRID_GROUP`], [`HEADER_GROUP`], [`STATUS_GROUP`]) and `groups()` lists them in that order:
/// with nothing focused the engine's first press seats the first member, then the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sec {
    /// The collection's art, title, meta line and summary (one focus element when the summary
    /// truncates), then the "Items" heading — all drawn by the page, in the flow above the grid.
    Head,
    /// The member grid.
    Items,
    /// The quiet read-outs, and Retry for a failed load.
    Status,
}
const GRID_GROUP: GroupId = GroupId(0);
const HEADER_GROUP: GroupId = GroupId(1);
const STATUS_GROUP: GroupId = GroupId(2);

// ── The page's geometry: `Collections.dc.html` C1, measured from its DOM (a 1920×1080 stage).

/// The page's content edge: the header's top, and the line a scrolled row snaps to (C2). The
/// mock's `left:96px; top:96px` — the page reads inside the same 96 on every side.
const CONTENT_TOP: f32 = MARGIN_X;
const HEADER_TOP: f32 = CONTENT_TOP;
/// `.tl { width:200px; height:300px }` — the collection's art, a 2:3 tile.
const ART_W: f32 = 200.0;
const ART_H: f32 = 300.0;
const ART_RES: (std::os::raw::c_int, std::os::raw::c_int) = (200, 300);
const HEADER_GAP: f32 = theme::space::XL;
/// `left:360px` — the text column, one XL gap past the art.
const COL_X: f32 = MARGIN_X + ART_W + HEADER_GAP;
/// `width:1344px`.
const TEXT_W: f32 = 1344.0;
/// Cap tops below the title's (which is `HEADER_TOP`): the LABEL meta line's box sits 16px under
/// the DISPLAY title's, and the BODY summary's 24px under the meta's — C1's CSS resolved to the
/// shipped face's cap bands (meta cap top 161, summary cap top 221).
const META_DY: f32 = 65.0;
const SUMMARY_DY: f32 = 125.0;
const SUMMARY_LINES: usize = 3;
/// `line-height:40px` — person.rs's biography, whose reading block this is.
const SUMMARY_LEAD: f32 = 40.0;
const MORE_GAP: f32 = theme::space::LG;
/// The "Items · Release order" heading's cap top (`.hd { top:443px }`, a HEADLINE run).
const ITEMS_HEADING_Y: f32 = 447.0;
/// The first row's poster top (`.tl { top:520px }`).
const GRID_TOP: f32 = 520.0;
/// C4a: the quiet read-outs' region, where the grid would be (`left:96; right:96; top:460;
/// height:475`), its copy centred in it.
const STATUS_FRAME: Rect = Rect { x: MARGIN_X, y: 460.0, w: SCR_W - 2.0 * MARGIN_X, h: 475.0 };

fn summary_view<'a>(summary: &'a str, measure: &'a dyn plx_machine::machine::Measure) -> TextView<'a> {
    TextView::new(summary, theme::size::BODY, theme::TEXT_READING)
        .with_measure(measure)
        .leading(SUMMARY_LEAD)
        .max_lines(SUMMARY_LINES)
        .fade_for_more(MORE_GAP)
}

/// The header's member count, and whether the line is the kind alone: with no members counted —
/// before the header lands, on a failed load, and for an empty collection (C4a's meta line reads
/// "Collection") — the line is the kind alone rather than "0 items".
fn meta_key(collection: &Collection) -> (i64, bool) {
    let count = if collection.child_count > 0 { collection.child_count } else { collection.total };
    let count = count as i64;
    (count, count == 0)
}

/// The grid heading's annotation: the member order the collection's owner chose, when the
/// server stated one.
fn order_note(collection: &Collection) -> &'static str {
    match collection.order {
        Some(CollectionOrder::Release) => plx_platform::i18n::msg::browse_collection_order_release(),
        Some(CollectionOrder::Title) => plx_platform::i18n::msg::browse_collection_order_title(),
        Some(CollectionOrder::Custom) => plx_platform::i18n::msg::browse_collection_order_custom(),
        None => "",
    }
}

/// How much of the page's head (art, text column, grid heading) is left at `scroll`: all of it at
/// the top, none once the first row has risen to the content edge — so a scrolled page (C2)
/// leaves no remnant of the head above its rows.
fn head_alpha(scroll: f32) -> f32 {
    (1.0 - scroll / (GRID_TOP - CONTENT_TOP)).clamp(0.0, 1.0)
}

/// The header's meta line — "Collection · N items", or the kind alone ([`meta_key`]).
fn meta_line(collection: &Collection) -> CString {
    match meta_key(collection) {
        (_, true) => plx_platform::i18n::msg::browse_collection_kind_c().to_owned(),
        (count, false) => CString::new(plx_platform::i18n::msg::browse_collection_meta(&plx_ui::fmt::item_count(count)))
            .unwrap_or_default(),
    }
}

pub fn member_label(item: &PmsMovie) -> String {
    match item.kind {
        2 if item.season_index > 0 => plx_platform::i18n::msg::browse_collection_season_mark(item.season_index as i64),
        3 => plx_ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64),
        _ => String::new(),
    }
}

pub fn member_caption(item: &PmsMovie) -> TileLabel {
    if item.kind == 3 {
        let address = plx_ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64);
        let caption = match (item.show_title.is_empty(), address.is_empty()) {
            (false, false) => format!("{} · {}", item.show_title, address),
            (false, true) => item.show_title.clone(),
            (true, false) => address,
            (true, true) => String::new(),
        };
        return TileLabel::titled(&item.title, &caption);
    }
    ui_cards::poster_label(&tile_facts::of(item))
}

/// One run of consecutive slots with a key and a label projected: slot `first + n` is `elems[n]`.
#[derive(Default)]
struct Run {
    first: usize,
    elems: Vec<u32>,
    labels: Vec<String>,
}

/// The keys and labels of the slots the page can paint or step to, as of one store revision: a few
/// runs, never the list. The listing's length stays the store's known count (navigation is by
/// index), so a slot outside every run is read through the store's rows and the key table instead.
/// Derived, not logical state.
#[derive(Default)]
struct Projection {
    /// The listing's known count when this was projected.
    total: usize,
    runs: Vec<Run>,
    /// The slot of every member element projected (a hole is its own slot).
    index: std::collections::HashMap<u32, usize>,
    /// The ranges `runs` was projected for, and the store revision they were read at.
    applied: Vec<std::ops::Range<usize>>,
    revision: Option<u64>,
}

impl Projection {
    fn run_of(&self, index: usize) -> Option<(&Run, usize)> {
        let at = self.runs.partition_point(|run| run.first <= index).checked_sub(1)?;
        let run = &self.runs[at];
        let offset = index - run.first;
        (offset < run.elems.len()).then_some((run, offset))
    }

    fn elem(&self, index: usize) -> Option<u32> {
        self.run_of(index).map(|(run, offset)| run.elems[offset])
    }

    fn label(&self, index: usize) -> Option<&str> {
        self.run_of(index).map(|(run, offset)| run.labels[offset].as_str())
    }

    /// The slots held, which is what a projection costs.
    fn cells(&self) -> usize { self.runs.iter().map(|run| run.elems.len()).sum() }
}

/// The slot ranges to project, sorted and merged, clamped to `total`.
fn merged(mut ranges: Vec<std::ops::Range<usize>>, total: usize) -> Vec<std::ops::Range<usize>> {
    ranges.retain_mut(|r| { r.end = r.end.min(total); r.start < r.end });
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
            _ => out.push(r),
        }
    }
    out
}

/// The grid's content (`CardSource`): the store's members read through the screen's projection.
/// Built from the screen's FIELDS, not `&CollectionScreen`, so a call can hold it while the grid
/// itself is borrowed mutably.
struct Members<'a> {
    collection: &'a Collection,
    cards: &'a CardKeys,
    /// [`Page::proj`], projected by `project` for the cells the page paints or steps to.
    proj: &'a Projection,
}

impl<'a> Members<'a> {
    /// Is [`Self::proj`] the listing of `collection` as it stands? Items only append between
    /// syncs, so a length match is the check; a landing the page has not synced yet falls back to
    /// reading the rows against the key table.
    fn fresh(&self) -> bool {
        self.proj.total == self.collection.shown()
    }

    /// The element projected for `index`, if the projection is current and holds the slot.
    fn projected(&self, index: usize) -> Option<u32> {
        self.proj.elem(index).filter(|_| self.fresh())
    }

    fn elem_at(&self, index: usize) -> Option<u32> {
        if index >= self.collection.shown() { return None; }
        if let Some(elem) = self.projected(index) { return Some(elem); }
        Some(self.collection.item(index).and_then(|item| self.cards.elem_for(item.sid, &item.rk))
            .unwrap_or_else(|| hole_elem(index)))
    }

    fn index(&self, elem: u32) -> Option<usize> {
        if elem >= HOLE_ELEM_BASE {
            return Some((elem - HOLE_ELEM_BASE) as usize).filter(|&i| i < self.collection.shown());
        }
        if self.fresh() {
            if let Some(&index) = self.proj.index.get(&elem) { return Some(index); }
        }
        let identity = self.cards.get(elem)?;
        self.collection.position_of(identity.sid, &identity.rk)
    }

    /// Card `index`'s persistent label: the one `project` built, or — for a slot outside the
    /// projection, or a landing the page has not synced yet — formatted now.
    fn label_at(&self, index: usize) -> std::borrow::Cow<'a, str> {
        match self.proj.label(index).filter(|_| self.fresh()) {
            Some(label) => std::borrow::Cow::Borrowed(label),
            None => std::borrow::Cow::Owned(self.collection.item(index).map(member_label).unwrap_or_default()),
        }
    }
}

impl<H: Host<Elem = u32>> CardSource<H> for Members<'_> {
    fn len(&self) -> usize { self.collection.shown() }
    fn elem(&self, i: usize) -> u32 { self.elem_at(i).unwrap_or(HEADER_ELEM) }
    fn index_of(&self, e: &u32) -> Option<usize> { self.index(*e) }
    fn art(&self, i: usize) -> Art<'_> { Art::Poster(self.collection.item(i).map(tile_facts::of)) }
    fn label(&self, i: usize) -> TileLabel {
        self.collection.item(i).map(member_caption).unwrap_or_default()
    }
    fn progress(&self, i: usize) -> Option<f32> { self.collection.item(i)?.resume_frac() }
    fn overlay(&self, p: Painter, i: usize, tile: &Tile, measure: &dyn Measure) {
        let persistent = self.label_at(i);
        if persistent.is_empty() { return; }
        widgets::poster_label(p, tile.rect, tile.radius, &persistent, measure);
        if let Some(frac) = self.collection.item(i).and_then(PmsMovie::resume_frac) { ui_cards::resume_bar(p, tile.rect, frac, tile.radius); }
    }
    fn more(&self) -> bool { self.collection.more }
}

/// The page's content and the state its sections read. The `Stack` is the screen's other field so
/// that `self.stack.on(&self.page, ..)` borrows the two apart; every section reads only this.
struct Page {
    entry: EntryId,
    /// The page's argument; `id.rk` adopts the store's resolution of a tag route.
    id: CollectionRef,
    header_marked: bool,
    cards: CardKeys,
    /// The keys and labels of the cells the page can paint or step to ([`Self::project`]), so a
    /// frame finds a card's element or index without scanning identities. Derived, not logical
    /// state.
    proj: Projection,
    /// The ranges and pinned keys [`CollectionScreen::plan`] last asked [`Self::project`] to keep:
    /// the grid's painted cells with a row either side, the first row, and the rows around the
    /// focus, the restore target and the group's remembered key.
    want: (Vec<std::ops::Range<usize>>, Vec<u32>),
    /// Where each pinned key was last seen, so its row is projected again after the key leaves
    /// the projection (an element is not an index; this is how a pin finds its slot).
    pin_index: Vec<(u32, usize)>,
    /// The header's meta line and the `(count, kind-only)` it was built for ([`meta_line`]).
    /// Derived, not logical state.
    meta: ((i64, bool), CString),
    return_pending: bool,
    /// The focused member's element, and the remembered one a Back is waiting for: neither key is
    /// ever pruned.
    focus_elem: Option<u32>,
    restore_elem: Option<u32>,
    /// The focused member's index when last seen, saved with the page so a return can wait for it.
    focus_index: Option<usize>,
    /// The index a Back is restoring to, from the saved memory.
    restore_index: Option<usize>,
    /// Saved kept counts a return sends the store once it is open.
    restore_counts: Option<(usize, Vec<u8>)>,
    /// The total and kept counts as of the last sync, saved with the page.
    counts: (usize, Vec<u8>),
    /// The window last sent to the store.
    sent_window: Option<(std::ops::Range<usize>, Option<usize>, Option<usize>)>,
    /// The end of the last range the grid asked for.
    want_end: usize,
    summary_more: bool,
    links_c: Vec<Link>,
    /// The store's content revision (`CollectionView::revision`) the last [`Self::sync_to`] saw —
    /// derived, not logical state. The frame tick re-syncs only when it moved (a same-length
    /// content change moves it; the item count does not); `StoreChanged` always re-syncs.
    synced: Option<u64>,
    /// The header summary's focus lift ([`Self::summary_marked`]). Presentation, not logical state.
    summary_lift: plx_ui::text_lift::TextLift,
}

pub struct CollectionScreen {
    page: Page,
    /// The page's layout, one scroll, groups, reconcile and the member grid (`ui::cards::Stack`).
    /// Presentation, not logical state — its motion is in the canon, its content is the store's.
    stack: Stack<Sec>,
    teardown_closed: bool,
    ground: PageGround,
    ground_seeded: bool,
}

impl LogicalState for CollectionScreen {
    fn write(&self, c: &mut Canon) {
        let page = &self.page;
        c.u32(page.entry.0).u32(page.id.sid.raw() as u32).str(&page.id.rk)
            .u64(page.id.sec as u64).u64(page.id.tag as u64).str(&page.id.name)
            .bool(page.header_marked).bool(page.return_pending)
            .u32(page.cards.next).seq(page.cards.len());
        for key in &page.cards.keys { c.u32(key.sid.raw() as u32).str(&key.rk).u32(key.elem); }
        self.stack.write(c);
    }
    fn probe(&self, out: &mut String) {
        let page = &self.page;
        out.push_str(&format!("collection sid={} rk={} sec={} tag={} cards={} next={} return_pending={}",
            page.id.sid.raw(), page.id.rk, page.id.sec, page.id.tag, page.cards.len(), page.cards.next,
            page.return_pending));
    }
}

impl Page {
    fn collection<'a, H: CollectionLike>(&self, cx: &Cx<'a, H>) -> Option<&'a Collection> {
        H::collection(cx).current().filter(|c| c.id.same_collection(&self.id))
    }

    /// [`Self::sync`] for the store's content as of `revision`.
    fn sync_to(&mut self, collection: &Collection, revision: u64, measure: &dyn plx_machine::machine::Measure) {
        self.synced = Some(revision);
        self.sync(collection, measure);
        self.proj.revision = Some(revision);
    }

    fn sync(&mut self, collection: &Collection, measure: &dyn plx_machine::machine::Measure) {
        if self.id.rk.is_empty() && !collection.id.rk.is_empty() { self.id.rk = collection.id.rk.clone(); }
        self.project(collection, None);
        self.counts = (collection.total, collection.kept_counts());
        self.meta = (meta_key(collection), meta_line(collection));
        self.summary_more = !collection.summary.is_empty() && summary_view(&collection.summary, measure).truncates(TEXT_W);
        self.links_c.clear();
        if self.summary_more && collection.shown() > 0 {
            self.links_c.push(Link { from: HEADER_GROUP, dir: Dir::Down, to: GRID_GROUP });
            self.links_c.push(Link { from: GRID_GROUP, dir: Dir::Up, to: HEADER_GROUP });
        }
    }

    /// Project keys and labels for [`Self::want`] and bound the key table. Only the rows held
    /// inside the wanted ranges get a key; a slot whose member is not held is a hole element, so
    /// the work and the memory follow the window and not the length of the list. `revision` names
    /// the store content the projection is current for (`None` forces it): a projection already
    /// made for that revision and these ranges is left as it is.
    fn project(&mut self, collection: &Collection, revision: Option<u64>) {
        let shown = collection.shown();
        let default = [0..4 * ui_cards::GRID_COLS];
        let ranges = merged(if self.want.0.is_empty() { default.to_vec() } else { self.want.0.clone() }, shown);
        if revision.is_some() && self.proj.revision == revision && self.proj.total == shown && self.proj.applied == ranges {
            return;
        }
        let rows: Vec<(usize, &PmsMovie)> = ranges.iter().flat_map(|r| r.clone())
            .filter_map(|i| collection.item(i).map(|item| (i, item))).collect();
        let keys = self.cards.intern_all(rows.iter().map(|(_, item)| (item.sid, item.rk.as_str())), "collection");
        // A member key must never reach the hole range; 0x4000_0000 minted keys is far beyond any
        // session, so this is the loud failure of an impossible state, not a limit.
        assert!(self.cards.next <= HOLE_ELEM_BASE, "collection element-key space reached the hole range");
        let before: std::collections::HashSet<u32> = self.proj.index.keys().copied().collect();
        let mut proj = Projection { total: shown, revision, ..Default::default() };
        let mut held = rows.iter().zip(keys).peekable();
        for range in &ranges {
            let mut run = Run { first: range.start, elems: Vec::with_capacity(range.len()), labels: Vec::with_capacity(range.len()) };
            for i in range.clone() {
                match held.next_if(|((at, _), _)| *at == i) {
                    Some(((_, item), key)) => {
                        proj.index.entry(key).or_insert(i);
                        run.elems.push(key);
                        run.labels.push(member_label(item));
                    }
                    None => { run.elems.push(hole_elem(i)); run.labels.push(String::new()); }
                }
            }
            proj.runs.push(run);
        }
        proj.applied = ranges;
        self.proj = proj;
        let pins = &self.want.1;
        self.pin_index.retain(|(elem, _)| pins.contains(elem));
        for &elem in pins {
            if let Some(&index) = self.proj.index.get(&elem) {
                match self.pin_index.iter_mut().find(|(e, _)| *e == elem) {
                    Some(seen) => seen.1 = index,
                    None => self.pin_index.push((elem, index)),
                }
            }
        }
        let (now, pins) = (&self.proj.index, &self.want.1);
        self.cards.prune(self.proj.cells(), |elem| now.contains_key(&elem) || before.contains(&elem) || pins.contains(&elem));
    }

    /// The grid's content: `collection` read through this page's projection.
    fn members<'a>(&'a self, collection: &'a Collection) -> Members<'a> {
        Members { collection, cards: &self.cards, proj: &self.proj }
    }

    /// Where member element `elem` sits: its slot in the listing, else the last slot a pinned key
    /// was seen at (a focus whose page the store has not read back yet).
    fn slot_of(&self, collection: &Collection, elem: u32) -> Option<usize> {
        self.members(collection).index(elem)
            .or_else(|| self.pin_index.iter().find(|(e, _)| *e == elem).map(|&(_, index)| index))
    }

    #[cfg(test)]
    fn elem_at(&self, collection: &Collection, index: usize) -> Option<u32> {
        self.members(collection).elem_at(index)
    }

    fn item_index(&self, collection: &Collection, elem: u32) -> Option<usize> {
        self.members(collection).index(elem)
    }

    /// Test support: how many cells the page holds an element and a label for.
    #[cfg(test)]
    fn cells(&self) -> usize { self.proj.cells() }

    /// Test support: the element projected for cell `index`, if any.
    #[cfg(test)]
    fn cell_elem(&self, index: usize) -> Option<u32> { self.proj.elem(index) }

    /// Test support: the label projected for cell `index`, if any.
    #[cfg(test)]
    fn cell_label(&self, index: usize) -> Option<&str> { self.proj.label(index) }

    /// Test support: also keep the rows around `index`, as navigating to it would have.
    #[cfg(test)]
    fn reach(&mut self, collection: &Collection, index: usize) {
        self.want.0.push(index.saturating_sub(ui_cards::GRID_COLS)..index + 2 * ui_cards::GRID_COLS);
        self.project(collection, None);
    }

    /// **A Back whose member has not landed yet.** The remembered member may sit past the pages
    /// loaded so far (a page rebuilt after its store was superseded reloads from the first page),
    /// so focus is held on it while the model is still loading or still has pages below the
    /// member's remembered position — `cards` is interned in server order, so that position
    /// is the member's index when the page was left.
    fn awaiting_restore(&self, collection: &Collection, elem: u32) -> bool {
        if !self.return_pending || self.item_index(collection, elem).is_some() { return false; }
        let Some(position) = self.restore_index.or_else(|| self.cards.position(elem)) else { return false };
        matches!(collection.status, CollectionStatus::Loading | CollectionStatus::Failed)
            || collection.item(position).is_none() && (collection.more || position < collection.shown())
    }

    /// The header summary earns its marked/lifted treatment when the header holds focus, is
    /// marked open, and the summary truncates.
    fn summary_marked(&self, header_focused: bool) -> bool {
        header_focused && self.header_marked && self.summary_more
    }

    /// The header's focus rect — its text column — scrolled with the document: the header is the
    /// top of one page with the grid, not a band pinned over it.
    fn header_rect(scroll: f32) -> Rect {
        Rect::new(COL_X, HEADER_TOP - scroll, TEXT_W, ART_H)
    }

    /// The collection's art, scrolled with the document.
    fn art_rect(scroll: f32) -> Rect {
        Rect::new(MARGIN_X, HEADER_TOP - scroll, ART_W, ART_H)
    }

    fn draw_header(&self, p: Painter, collection: &Collection, focused: bool,
        measure: &dyn plx_machine::machine::Measure, scroll: f32) {
        let dy = -scroll;
        let alpha = head_alpha(scroll);
        if alpha <= 0.0 { return; }
        let p = p.alpha(alpha);
        let art = Self::art_rect(scroll);
        let name = if collection.title.is_empty() { &collection.id.name } else { &collection.title };
        if collection.header_ready() && collection.thumb.is_empty() {
            // No artwork of its own (an empty collection has no composite either): the neutral
            // tile the Library grid draws for the same collection — its mark and its name.
            plx_ui::collection_tile::draw(p, art, art, theme::CARD_RING_RAD, name);
        } else {
            // The name is set over the baked fan only when the thumb IS the server's composite.
            let fan_name = tile_facts::is_composite_thumb(&collection.thumb).then_some(name.as_str());
            widgets::card_named(p, art,
                Art::Thumb { sid: collection.id.sid.raw(), key: &collection.thumb, res: ART_RES },
                fan_name, theme::CARD_RING_RAD, false, 1.0, 0.0);
        }
        if collection.status == CollectionStatus::Ready {
            ui_cards::draw_heading(p, plx_platform::i18n::msg::browse_collection_items(), order_note(collection),
                MARGIN_X, ITEMS_HEADING_Y + dy, SCR_W - 2.0 * MARGIN_X, measure);
        }
        let title = measure.fit_line(name, TEXT_W, theme::size::DISPLAY, true);
        Label::new(title.as_ptr(), theme::size::DISPLAY, theme::TEXT_PRIMARY).bold()
            .v(VAlign::CapTop).draw(p, Rect::new(COL_X, HEADER_TOP + dy, TEXT_W, 0.0));
        let (meta_y, summary_y) = CollectionScreen::header_ys(measure);
        let (meta_y, summary_y) = (meta_y + dy, summary_y + dy);
        let built;
        let meta = if self.meta.0 == meta_key(collection) { &self.meta.1 } else {
            built = meta_line(collection);
            &built
        };
        Label::new(meta.as_ptr(), theme::size::LABEL, theme::TEXT_SECONDARY)
            .v(VAlign::CapTop).draw(p, Rect::new(COL_X, meta_y, TEXT_W, 0.0));
        if collection.summary.is_empty() { return; }
        let view = summary_view(&collection.summary, measure);
        let h = view.measure_h(TEXT_W);
        let plate = Rect::new(COL_X - theme::space::SM, summary_y - theme::space::SM,
            TEXT_W + 2.0 * theme::space::SM, h + 2.0 * theme::space::SM);
        plx_ui::text_lift::draw_focused(p, plate, widgets::TEXT_BLOCK_HL_RAD, &self.summary_lift,
            plx_ui::text_lift::CENTRE, |p| {
                view.draw(p, Rect::new(COL_X, summary_y, TEXT_W, h));
                if self.summary_more {
                    view.draw_more(p, COL_X, summary_y, TEXT_W, h, self.summary_marked(focused));
                }
            });
    }
}

impl<H: ContentLike + CollectionLike> StackPage<H> for Page {
    type Key = Sec;
    type Cards<'a> = Members<'a>;

    /// Everything `sections` reads: the store's content, whether a collection is on show and its
    /// status (which sections exist and what they answer), and whether the summary truncates.
    fn revision(&self, cx: &Cx<'_, H>) -> u64 {
        let status = self.collection(cx).map_or(0, |c| match c.status {
            CollectionStatus::Loading => 1,
            CollectionStatus::Ready => 2,
            CollectionStatus::Empty => 3,
            CollectionStatus::Unavailable => 4,
            CollectionStatus::Failed => 5,
        });
        H::collection(cx).revision() << 4 | status << 1 | u64::from(self.summary_more)
    }

    /// A hole whose page has landed: the slot now holds its member under that member's own key.
    /// Focus on the hole moves to it in the frame of the landing, in the same slot, so the
    /// column and the cell under it stay.
    fn supersede(&self, cx: &Cx<'_, H>, want: &u32) -> Option<u32> {
        let slot = want.checked_sub(HOLE_ELEM_BASE)? as usize;
        let collection = self.collection(cx)?;
        let now = self.members(collection).projected(slot)?;
        (now != *want && now < HOLE_ELEM_BASE).then_some(now)
    }

    /// The remembered member of a Back has not landed yet (or the whole collection has not).
    fn pending(&self, cx: &Cx<'_, H>, want: &u32) -> bool {
        match self.collection(cx) {
            Some(collection) => self.awaiting_restore(collection, *want),
            None => self.return_pending && self.cards.get(*want).is_some(),
        }
    }

    fn sections(&self, cx: &Cx<'_, H>, out: &mut Vec<SectionSpec<Sec>>) {
        let collection = self.collection(cx);
        let head = collection.is_some_and(CollectionScreen::shows_head);
        let failed = collection.is_some_and(|c| c.status == CollectionStatus::Failed);
        out.push(SectionSpec::new(Sec::Head, Kind::Custom { height: GRID_TOP, focusable: head && self.summary_more }, HEADER_GROUP).ranked(1));
        out.push(SectionSpec::new(Sec::Items, Kind::Grid { spec: GridSpec::new(GRID_TOP, CONTENT_TOP) }, GRID_GROUP));
        let retry = if failed {
            CollectionScreen::status_overlay(collection, cx.tick.ms, cx.measure).action_frame_measured(cx.measure)
        } else { None };
        out.push(SectionSpec::new(Sec::Status,
            Kind::Overlay { rect: retry.unwrap_or(CollectionScreen::status_frame()), focusable: retry.is_some() }, STATUS_GROUP).ranked(2));
    }

    fn fallback(&self, _cx: &Cx<'_, H>, out: &mut Vec<Sec>) {
        out.extend([Sec::Items, Sec::Status, Sec::Head]);
    }

    fn cards<'a>(&'a self, cx: &'a Cx<'_, H>, k: Sec) -> Option<Members<'a>> {
        match k {
            Sec::Items => self.collection(cx).filter(|c| c.status == CollectionStatus::Ready).map(|c| self.members(c)),
            Sec::Head | Sec::Status => None,
        }
    }

    fn elem_of(&self, k: Sec) -> Option<u32> {
        match k { Sec::Head => Some(HEADER_ELEM), Sec::Status => Some(RETRY_ELEM), Sec::Items => None }
    }

    fn card_has_menu(&self, cx: &Cx<'_, H>, _k: Sec, elem: &u32) -> bool {
        let Some(collection) = self.collection(cx) else { return false };
        self.item_index(collection, *elem)
            .and_then(|index| collection.item(index))
            .is_some_and(crate::registry::item_has_menu)
    }

    fn focus_rect(&self, _cx: &Cx<'_, H>, k: Sec, section: Rect) -> Rect {
        match k {
            Sec::Head => Self::header_rect(-section.y),
            Sec::Items | Sec::Status => section,
        }
    }

    fn plain_group(&self, _cx: &Cx<'_, H>, k: Sec, id: GroupId, extent: Rect) -> GroupSpec {
        let (reachable, edge) = match k {
            Sec::Head => (AxisMask::VERTICAL, [EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop]),
            Sec::Items | Sec::Status => (AxisMask::BOTH, [EdgeRule::Stop; 4]),
        };
        GroupSpec { id, kind: GroupKind::Free, seat: Seat::First, reachable, edge, extent, len: 1, elem: ElemKind::Bare }
    }

    fn custom_draw(&self, k: Sec, f: &mut DrawFrame<'_, '_, H>, _r: Rect, scroll: f32) {
        let p = f.painter.alpha(f.page_alpha);
        let collection = self.collection(f.cx);
        let on = |elem: u32| f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == elem);
        match k {
            Sec::Head => if let Some(collection) = collection.filter(|c| CollectionScreen::shows_head(c)) {
                self.draw_header(p, collection, on(HEADER_ELEM), f.measure, scroll);
            },
            Sec::Status => if collection.is_none_or(|c| c.status != CollectionStatus::Ready) {
                let overlay = CollectionScreen::status_overlay(collection, f.cx.tick.ms, f.measure).focused(on(RETRY_ELEM));
                overlay.draw_measured(&Env::inert(), p, f.measure);
            },
            Sec::Items => {}
        }
    }
}

plx_ui::focusable_via_view!(CollectionScreen, H: [ContentLike + CollectionLike], view);

impl CollectionScreen {
    pub const SHAPE: &'static str = "CollectionScreen{entry:u32,sid:u32,rk:String,sec:i64,tag:i64,name:String,header_marked:bool,return_pending:bool,next_elem:u32,card_keys:[{sid:u32,rk:String,elem:u32}],stack:Stack{scroll:{pos:f32,vel:f32},target:f32,sections:[Grid{bands,pop}]}}";

    pub fn new(entry: EntryId, id: CollectionRef) -> Self {
        Self {
            page: Page { entry, id, header_marked: false, cards: CardKeys::new(FIRST_CARD_ELEM),
                proj: Projection::default(), want: Default::default(), pin_index: Vec::new(), meta: ((0, true), CString::default()),
                return_pending: false, focus_elem: None, restore_elem: None,
                focus_index: None, restore_index: None, restore_counts: None, counts: (0, Vec::new()),
                sent_window: None, want_end: 0, summary_more: false, links_c: Vec::new(), synced: None,
                summary_lift: plx_ui::text_lift::TextLift::new() },
            // A page that leaves focus (a menu over it, a pointer gone) rests at the top.
            stack: Stack::new(entry).hold_hint(plx_ui::hold_hint::Kind::Collection).home_when_unfocused(true),
            teardown_closed: false, ground: PageGround::new(), ground_seeded: false,
        }
    }

    /// The page's `Focusable` (see `focusable_via_view!` below): the stack's view over the page.
    fn view<H: ContentLike + CollectionLike>(&self) -> impl Focusable<H> + '_ {
        self.stack.view(&self.page)
    }

    fn entry(&self) -> EntryId { self.page.entry }

    fn target(&self, want: usize) -> CollectionTarget {
        CollectionTarget { id: self.page.id.clone(), want }
    }

    fn request_store<H: ContentLike + CollectionLike>(&mut self, want: usize, fx: &mut Effects<'_, H>) {
        fx.push(plx_machine::machine::Fx::App(AppFx::Store(
            plx_data::stores::StoreId::Collection,
            plx_data::stores::StoreCmd::Collection(CollectionCmd::Open { target: self.target(want) }),
        )));
    }

    fn collection<'a, H: CollectionLike>(&self, cx: &Cx<'a, H>) -> Option<&'a Collection> {
        self.page.collection(cx)
    }

    /// What the page keeps keys for: the cells the grid can paint or step to (its own window, a row
    /// more each side as look-ahead), the first row (where a vertical move into the grid seats),
    /// and the rows around the focus, the restore target and the group's remembered key, which are
    /// also pinned against the key table's bound. Derived each frame from the grid's painted
    /// range, so a scroll that moves the view without moving focus moves what is kept.
    fn plan<H: ContentLike + CollectionLike>(&mut self, cx: &Cx<'_, H>) {
        let Some(collection) = self.collection(cx) else { return };
        let cols = ui_cards::GRID_COLS;
        let row = |index: usize| (index / cols).saturating_sub(1) * cols..(index / cols + 2) * cols;
        let mut ranges = match self.painted(cx) {
            Some(painted) => vec![painted.start.saturating_sub(cols)..painted.end + cols],
            None => vec![0..4 * cols],
        };
        ranges.push(0..cols);
        let focus = cx.focus.current.filter(|key| key.entry == self.entry()).map(|key| key.elem);
        let mut pins = Vec::new();
        for elem in [focus, self.page.restore_elem, cx.focus.remembered(GRID_GROUP)].into_iter().flatten() {
            if elem == HEADER_ELEM || elem == RETRY_ELEM { continue; }
            if elem < HOLE_ELEM_BASE { pins.push(elem); }
            if let Some(index) = self.page.slot_of(collection, elem) { ranges.push(row(index)); }
        }
        if let Some(index) = self.page.return_pending.then_some(self.page.restore_index).flatten() {
            ranges.push(row(index));
        }
        self.page.want = (ranges, pins);
    }

    /// The cells the grid paints this frame, by the grid's own rule (`None` before its first
    /// layout).
    fn painted<H: ContentLike + CollectionLike>(&self, cx: &Cx<'_, H>) -> Option<std::ops::Range<usize>> {
        self.stack.grid_window(&self.page, cx, Sec::Items)
    }

    /// Project again for the window the grid has NOW: a scroll step moves what it can paint after
    /// the frame's first projection, and every cell it paints or steps to needs a key in the frame.
    /// A no-op when neither the ranges nor the store's content moved.
    fn reproject<H: ContentLike + CollectionLike>(&mut self, cx: &Cx<'_, H>) {
        let Some(collection) = self.collection(cx) else { return };
        self.plan(cx);
        self.page.project(collection, Some(H::collection(cx).revision()));
    }

    /// Tells the store which indices the screen shows or is about to show, which member holds
    /// focus and where a Back is restoring to, so it keeps those pages and drops the rest. The
    /// window is anchored on the focus while it is among the cells the grid paints, else on the
    /// restore target, else on the top of what the grid paints: one page above, two below. The
    /// anchor follows the grid's painted range (the one function the grid draws by), so a scroll
    /// that does not move focus still moves the window. It is sent only when it moves.
    fn send_window<H: ContentLike + CollectionLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(collection) = self.collection(cx) else { return };
        let painted = self.painted(cx).unwrap_or(0..0);
        let focus = cx.focus.current.filter(|key| key.entry == self.entry())
            .and_then(|key| self.item_index(collection, key.elem));
        let restore = self.page.return_pending.then_some(self.page.restore_index).flatten();
        let anchor = focus.filter(|at| painted.contains(at)).or(restore).unwrap_or(painted.start);
        let wanted = anchor.saturating_sub(PAGE_SIZE)..(anchor + 2 * PAGE_SIZE).max(self.page.want_end.min(anchor + 3 * PAGE_SIZE));
        let window = (wanted, focus, restore);
        if self.page.sent_window.as_ref() == Some(&window) { return; }
        let (wanted, focus, restore) = window.clone();
        self.page.sent_window = Some(window);
        fx.push(plx_machine::machine::Fx::App(AppFx::Store(
            plx_data::stores::StoreId::Collection,
            plx_data::stores::StoreCmd::Collection(CollectionCmd::Window { wanted, focus, restore }),
        )));
    }

    pub fn restore(&mut self, memory: &CardPageMemory) {
        self.page.cards.merge(&memory.cards, FIRST_CARD_ELEM, "collection");
        self.page.header_marked |= memory.header_marked;
        self.page.restore_index = memory.focus_index;
        // The counts are sent to the store once it is open (see `Enter`).
        self.page.restore_counts = Some((memory.total, memory.counts.clone())).filter(|(_, c)| !c.is_empty());
        self.page.restore_elem = memory.focus_elem;
    }

    fn memory(&self, focus: Option<plx_machine::machine::FocusKey<u32>>) -> CardPageMemory {
        // The member holding focus when the page is left: its key and index are saved, so a return
        // can wait for it and the keys prune never drops it.
        let elem = focus.filter(|key| key.entry == self.entry()).map(|key| key.elem)
            .filter(|&e| e != HEADER_ELEM && e != RETRY_ELEM).or(self.page.focus_elem);
        let index = elem.and_then(|e| self.page.proj.index.get(&e).copied()).or(self.page.focus_index);
        CardPageMemory { cards: self.page.cards.clone(), header_marked: self.page.header_marked,
            total: self.page.counts.0, counts: self.page.counts.1.clone(),
            focus_index: index, focus_elem: elem }
    }

    /// The member count a page entry asks the store for: one page, or — returning to a page whose
    /// remembered member sits within the second — through that member, so the first ask reads it.
    /// A deeper target is named by the `Window` that follows every tick (and the saved counts), not
    /// by this first ask, so the key table's size is not what says how far the page had read.
    fn entry_want(&self) -> usize {
        match self.page.restore_index.filter(|_| self.page.return_pending) {
            Some(index) => (index + 1).clamp(PAGE_SIZE, 2 * PAGE_SIZE),
            None => PAGE_SIZE,
        }
    }

    #[cfg(test)]
    fn elem_at(&self, collection: &Collection, index: usize) -> Option<u32> {
        self.page.elem_at(collection, index)
    }

    fn item_index(&self, collection: &Collection, elem: u32) -> Option<usize> {
        self.page.item_index(collection, elem)
    }

    #[cfg(test)]
    fn key_at(&self, collection: &Collection, index: usize) -> plx_machine::machine::FocusKey<u32> {
        plx_machine::machine::FocusKey { entry: self.entry(),
            elem: self.elem_at(collection, index).unwrap_or(HEADER_ELEM) }
    }

    fn status_frame() -> Rect { STATUS_FRAME }

    /// The header text column's two anchors — the meta line's and the summary's cap tops — shared
    /// by [`Self::draw_header`] and [`Self::header_text_bottom`] so the read-out's glyph ceiling
    /// is the drawn header, not a copy of its arithmetic.
    fn header_ys(_measure: &dyn plx_machine::machine::Measure) -> (f32, f32) {
        (HEADER_TOP + META_DY, HEADER_TOP + SUMMARY_DY)
    }

    /// Whether the page shows its head. An unavailable collection (C4b) is a page-filling verdict
    /// with no header: there is no collection to describe, only why it cannot be shown.
    fn shows_head(collection: &Collection) -> bool {
        collection.status != CollectionStatus::Unavailable
    }

    /// The lowest y the header's text column paints — the meta line, or the summary block under
    /// it. A failed page keeps its header live above the read-out, as the Library keeps its tab
    /// strip, so this is the read-out's `glyph_ceiling`.
    fn header_text_bottom(collection: &Collection, measure: &dyn plx_machine::machine::Measure) -> f32 {
        let (meta_y, summary_y) = Self::header_ys(measure);
        if collection.summary.is_empty() { return meta_y + measure.line_h(theme::size::LABEL); }
        summary_y + summary_view(&collection.summary, measure).measure_h(TEXT_W)
    }

    /// The page's read-out, one per non-Ready status. Loading and Empty are quiet answers centred
    /// in the grid's region (C4a); an unavailable collection (refused, not shared with this
    /// profile, or gone) fills the page with the shared `Failed` verdict and its reason at the 540
    /// anchor and no header (C4b) — BACK is the way out, so it offers no action; a transport
    /// failure is the page-placed `Failed` read-out every page shares (`StatusOverlay::page`,
    /// `ui/CLAUDE.md` rule 4) — Home's and the Library's untyped "can't reach" verdict, so their
    /// glyph too — with its glyph shrunk under the live header rather than drawn over it.
    fn status_overlay<'a>(collection: Option<&Collection>, tick: u32,
        measure: &dyn plx_machine::machine::Measure) -> StatusOverlay<'a> {
        match collection.map(|c| c.status).unwrap_or(CollectionStatus::Loading) {
            // placeholder-exempt: builds the value only; counted where drawn (StatusOverlay::draw_geometry, Working)
            CollectionStatus::Loading => StatusOverlay::new(Self::status_frame(), plx_platform::i18n::msg::browse_collection_loading_c(), StatusKind::Working).phase(tick),
            CollectionStatus::Empty => StatusOverlay::new(Self::status_frame(), plx_platform::i18n::msg::browse_collection_empty_c(), StatusKind::Empty),
            CollectionStatus::Unavailable => StatusOverlay::new(Rect::FULL, plx_platform::i18n::msg::browse_collection_unavailable_c(), StatusKind::Failed)
                .page(plx_ui::icons::Icon::PersonBadgeXmark)
                .reason(plx_platform::i18n::msg::browse_collection_unavailable_reason_c()),
            CollectionStatus::Failed => {
                let overlay = StatusOverlay::new(Rect::FULL, plx_platform::i18n::msg::browse_home_failed_c(), StatusKind::Failed)
                    .page(plx_ui::icons::Icon::ServerBadgeMinus).action(plx_platform::i18n::msg::browse_action_retry_c());
                match collection {
                    Some(c) => overlay.glyph_ceiling(Self::header_text_bottom(c, measure)),
                    None => overlay,
                }
            }
            CollectionStatus::Ready => StatusOverlay::new(Self::status_frame(), c"", StatusKind::Empty),
        }
    }

    /// OK on the header: the summary read in full, behind the `MORE` mark. Person's gate — the
    /// sheet is offered exactly when the mark is drawn, and both read `summary_more`.
    fn activate_header<H: ContentLike + CollectionLike>(&mut self, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        self.page.header_marked = true;
        if self.collection(cx).is_some() && self.page.summary_more {
            fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Panel(
                ContentPanel::CollectionAbout))));
            fx.invalidate(Provenance::Input);
        }
    }

    /// Re-derive the content index when the store's CONTENT moved. `sync` walks every member
    /// against every key and measures the summary — per landing, not per frame (a few hundred
    /// members would otherwise cost a quadratic walk every tick).
    fn resync<H: ContentLike + CollectionLike>(&mut self, cx: &Cx<'_, H>) {
        let Some(collection) = self.collection(cx) else { return };
        let revision = H::collection(cx).revision();
        if self.page.synced != Some(revision) {
            self.plan(cx);
            self.page.sync_to(collection, revision, cx.measure);
        }
    }

    fn tick<H: CollectionLike>(&mut self, t: Tick, cx: &Cx<'_, H>) {
        let Some(collection) = self.collection(cx) else { return };
        let header_focused = cx.focus.current
            .is_some_and(|key| key.entry == self.entry() && key.elem == HEADER_ELEM);
        let focus = cx.focus.current.filter(|key| key.entry == self.entry());
        self.page.focus_elem = focus.map(|key| key.elem).filter(|&e| e != HEADER_ELEM && e != RETRY_ELEM);
        if let Some(index) = self.page.focus_elem.and_then(|e| self.item_index(collection, e)) {
            self.page.focus_index = Some(index);
        }
        let marked = self.page.summary_marked(header_focused);
        self.page.summary_lift.step(marked, t.dt());
        let focused = cx.focus.current.filter(|key| key.entry == self.entry())
            .and_then(|key| self.item_index(collection, key.elem))
            .and_then(|index| collection.item(index));
        let target = PageGround::page_target(
            focused.or_else(|| collection.item(0)).map(tile_facts::of));
        if self.ground_seeded { self.ground.key_target(target, t.dt()); }
        else { self.ground.jump_target(target); self.ground_seeded = true; }
    }

    pub fn focused_item<'a, H: CollectionLike>(&self,
        focus: Option<plx_machine::machine::FocusKey<u32>>, cx: &Cx<'a, H>) -> Option<&'a PmsMovie> {
        let collection = self.collection(cx)?;
        let index = focus.filter(|key| key.entry == self.entry())
            .and_then(|key| self.item_index(collection, key.elem))?;
        collection.item(index)
    }
}

impl<H: ContentLike + CollectionLike> Machine<H> for CollectionScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        // The content index is current before the stack reads it for this event.
        match ev {
            ScreenEvent::Tick(_) => self.resync(cx),
            ScreenEvent::StoreChanged(ord, _) if *ord == plx_data::stores::StoreId::Collection.ord() => {
                if let Some(collection) = self.collection(cx) {
                    self.plan(cx);
                    self.page.sync_to(collection, H::collection(cx).revision(), cx.measure);
                }
            }
            _ => {}
        }
        let card = match self.stack.on(&self.page, ev, cx, fx) {
            Some(StackEvent::Card(_, card)) => Some(card),
            _ => None,
        };
        // The stack stepped the scroll: the cells the grid paints now hold keys before the frame
        // is drawn or any key is stepped from. (Not before a restored page has merged its saved
        // keys: a projection's fresh keys would take the elements those name.)
        if !matches!(ev, ScreenEvent::RestoreMemory(_)) { self.reproject(cx); }
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Collection(memory)) => {
                self.restore(memory); self.page.return_pending = true; Handled::Yes
            }
            ScreenEvent::Tick(t) => {
                self.tick(*t, cx);
                if let Some(CardEvent::Want(range)) = card {
                    let loaded = self.collection(cx).map_or(0, |c| c.shown());
                    self.page.want_end = range.end.max(loaded.saturating_add(PAGE_SIZE));
                    self.page.sent_window = None;
                }
                self.send_window(cx, fx);
                Handled::Yes
            }
            ScreenEvent::Enter(_) | ScreenEvent::Uncover => {
                self.request_store(self.entry_want(), fx);
                if let Some((total, counts)) = self.page.restore_counts.take() {
                    fx.push(plx_machine::machine::Fx::App(AppFx::Store(
                        plx_data::stores::StoreId::Collection,
                        plx_data::stores::StoreCmd::Collection(CollectionCmd::Restore { total, counts }),
                    )));
                }
                fx.invalidate(Provenance::Nav); Handled::Yes
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, By::Dir | By::Pointer) { self.page.return_pending = false; }
                if self.collection(cx).is_some() && to.elem == HEADER_ELEM && matches!(by, By::Dir | By::Pointer) {
                    self.page.header_marked = true;
                }
                fx.invalidate(Provenance::Input); Handled::Yes
            }
            ScreenEvent::Activate(elem) if *elem == RETRY_ELEM => {
                self.request_store(PAGE_SIZE, fx); Handled::Yes
            }
            ScreenEvent::Activate(elem) if *elem == HEADER_ELEM => {
                self.activate_header(cx, fx); Handled::Yes
            }
            ScreenEvent::PressCommit(_) => {
                if let Some(CardEvent::Activate(elem)) = card {
                    let key = plx_machine::machine::FocusKey { entry: self.entry(), elem };
                    if let Some(item) = self.focused_item(Some(key), cx) {
                        fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Push(
                            ContentArg::Detail { sid: item.sid, rk: item.rk.clone() }))));
                    }
                }
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => {
                if matches!(card, Some(CardEvent::Hold(_))) {
                    fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::ItemMenu)));
                    Handled::Yes
                } else { Handled::No }
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                fx.push(plx_machine::machine::Fx::App(AppFx::Content(ContentReq::Back))); Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge: Edge::Down, .. }, .. })
                if matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) => {
                    self.page.return_pending = false; Handled::No
                }
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { .. }, .. }) => {
                self.page.return_pending = false; Handled::No
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == plx_data::stores::StoreId::Collection.ord() => {
                if self.page.return_pending {
                    let settled = cx.focus.current.filter(|key| key.entry == self.entry())
                        .is_some_and(|key| self.collection(cx).is_some_and(|c| !self.page.awaiting_restore(c, key.elem)));
                    if settled { self.page.return_pending = false; }
                }
                Handled::Yes
            }
            ScreenEvent::WillLeave(Leave::ForGood) | ScreenEvent::Unmount => {
                if self.collection(cx).is_some() && !self.teardown_closed {
                    self.teardown_closed = true;
                    fx.push(plx_machine::machine::Fx::App(AppFx::Store(plx_data::stores::StoreId::Collection,
                        plx_data::stores::StoreCmd::Collection(CollectionCmd::Close))));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike + CollectionLike> Screen<H> for CollectionScreen {
    fn focused_card<'a>(&self, cx: &Cx<'a, H>, focus: Option<plx_machine::machine::FocusKey<u32>>, at: Option<At>) -> Option<plx_ui::screen::FocusedCard<'a>> {
        let item = self.focused_item(focus, cx)?;
        let placed = focus.zip(at).and_then(|(key, at)| Focusable::<H>::place(self, &key.elem, cx, at));
        Some(plx_ui::screen::FocusedCard::new(item, placed))
    }
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<plx_machine::machine::FocusKey<u32>>) {
        self.stack.view(&self.page).redraw_focused(f, focus);
    }
    fn name(&self) -> &'static str { super::registry::word::COLLECTION }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> { None }
    fn prepare(&mut self, _budget: &mut plx_ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        self.ground.draw(p, Rect::FULL);
        self.stack.view(&self.page).paint(f);
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
    fn links(&self, out: &mut Vec<Link>) { out.extend(self.page.links_c.iter().copied()); }
    fn memory_at(&self, focus: Option<plx_machine::machine::FocusKey<u32>>) -> PageMemory {
        PageMemory::Collection(self.memory(focus))
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> { Some(self) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::fixture::FixtureMeasure;
    use plx_ui::screen::Step;
    use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead};

    pub(super) struct CollectionHost;
    impl Host for CollectionHost {
        type Arg = super::super::family::SettingsPage;
        type Fx = AppFx;
        type Msg = super::super::registry::AppMsg;
        type Elem = u32;
        type Views<'a> = plx_data::collection::CollectionView<'a>;
        type Init = super::super::family::NoInit;
        type Memory = PageMemory;
    }
    impl CollectionLike for CollectionHost {
        fn collection<'a>(cx: &Cx<'a, Self>) -> plx_data::collection::CollectionView<'a> { cx.views }
    }

    pub(super) fn item(rk: &str) -> PmsMovie { PmsMovie { rk: rk.into(), title: rk.into(), ..Default::default() } }
    pub(super) fn set() -> CollectionRef {
        CollectionRef { sid: plx_plex::plex::ServerId::UNSET, rk: "50001".into(), sec: 1, tag: 7, name: "Set".into() }
    }
    fn seeded() -> (plx_data::stores::collection::CollectionStore, CollectionScreen) {
        let mut store = plx_data::stores::collection::CollectionStore::default();
        store.run(CollectionCmd::Open { target: CollectionTarget { id: set(), want: PAGE_SIZE } });
        store.install_for_test(vec![item("a"), item("b"), item("c")], CollectionStatus::Ready);
        let mut screen = CollectionScreen::new(EntryId(9), set());
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        step(&mut screen, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }), &cx(store.view(), None));
        (store, screen)
    }
    pub(super) fn cx<'a>(view: plx_data::collection::CollectionView<'a>, focus: Option<plx_machine::machine::FocusKey<u32>>) -> Cx<'a, CollectionHost> {
        Cx { views: view, tick: Tick::default(), measure: &FixtureMeasure,
            press: PressRead::default(), focus: FocusRead { current: focus, ..Default::default() },
            owner: InputOwner::Entry(EntryId(9)) }
    }

    #[test]
    fn season_and_episode_projection_uses_persistent_labels_and_show_address_caption() {
        let season = PmsMovie { kind: 2, season_index: 3, title: "Season 3".into(), ..Default::default() };
        assert_eq!(member_label(&season), "SEASON 3");
        let episode = PmsMovie { kind: 3, season_index: 3, ep_index: 4, title: "The Answer".into(),
            show_title: "Example Show".into(), ..Default::default() };
        assert_eq!(member_label(&episode), "S3 · E4");
        let caption = member_caption(&episode);
        assert_eq!(caption.title.unwrap().to_str().unwrap(), "The Answer");
        assert_eq!(caption.caption.unwrap().to_str().unwrap(), "Example Show · S3 · E4");
    }

    pub(super) fn step(screen: &mut CollectionScreen, ev: ScreenEvent<CollectionHost>,
        cx: &Cx<'_, CollectionHost>) -> Vec<plx_machine::machine::Stamped<CollectionHost>> {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(&mut out,
            plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(9)), &mut present);
        Machine::<CollectionHost>::step(screen, &ev, cx, &mut fx);
        drop(fx);
        out
    }

    fn opens_summary(out: &[plx_machine::machine::Stamped<CollectionHost>]) -> bool {
        out.iter().any(|e| matches!(e.fx, plx_machine::machine::Fx::App(AppFx::Content(
            ContentReq::Panel(ContentPanel::CollectionAbout)))))
    }

    #[test]
    fn header_ok_opens_the_full_summary_exactly_when_more_is_drawn() {
        let (mut store, mut screen) = seeded();
        let short = step(&mut screen, ScreenEvent::Activate(HEADER_ELEM), &cx(store.view(), None));
        assert!(!screen.page.summary_more && !opens_summary(&short),
            "a summary that fits offers no sheet: {}", store.view().current().unwrap().summary);

        store.edit_for_test(|c| c.summary = "A long collection summary that runs on. ".repeat(40));
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        assert!(screen.page.summary_more, "the MORE mark is drawn for a truncated summary");
        let out = step(&mut screen, ScreenEvent::Activate(HEADER_ELEM), &cx(store.view(), None));
        assert!(opens_summary(&out), "OK on the header opens the summary sheet");
        assert!(screen.page.header_marked);
    }

    /// The frame tick re-derives `elems`/`labels` when the store's CONTENT moved, not when its size
    /// did: a member replaced by another (same count, same summary length) must be followed.
    #[test]
    fn a_same_length_content_change_is_synced_by_the_frame_tick() {
        let (mut store, mut screen) = seeded();
        let (before_elem, before_label) = (screen.page.cell_elem(1), screen.page.cell_label(1).map(str::to_owned));
        store.edit_for_test(|c| c.edit_items_for_test(|v| v[1] = PmsMovie { kind: 2, season_index: 4, ..item("z") }));
        tick_at(&mut screen, &store, None, 16);
        assert_ne!(screen.page.cell_elem(1), before_elem, "the replaced member has its own element");
        assert_ne!(screen.page.cell_label(1).map(str::to_owned), before_label, "and its own label");
        assert_eq!(screen.page.cells(), 3);
    }

    #[test]
    fn focus_near_the_end_asks_the_store_for_the_next_page() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.more = true);
        let key = screen.key_at(store.view().current().unwrap(), 2);
        let ticking = |screen: &mut CollectionScreen, store: &plx_data::stores::collection::CollectionStore| {
            step(screen, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir }, &cx(store.view(), Some(key)));
            step(screen, ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }), &cx(store.view(), Some(key)))
        };
        let out = ticking(&mut screen, &store);
        let want = out.iter().find_map(|e| match &e.fx {
            plx_machine::machine::Fx::App(AppFx::Store(_, plx_data::stores::StoreCmd::Collection(
                CollectionCmd::Window { wanted, focus, .. }))) => Some((wanted.end, *focus)),
            _ => None,
        });
        assert_eq!(want, Some((2 + 2 * PAGE_SIZE, Some(2))),
            "the window reaches two pages past the focus, well beyond the last row, so the store reads on");

        store.edit_for_test(|c| c.more = false);
        let mut done = CollectionScreen::new(EntryId(9), set());
        done.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let out = ticking(&mut done, &store);
        assert!(!out.iter().any(|e| matches!(e.fx, plx_machine::machine::Fx::App(AppFx::Store(
            _, plx_data::stores::StoreCmd::Collection(CollectionCmd::Open { .. }))))),
            "a fully loaded collection asks to open nothing further");
    }

    #[test]
    fn a_member_ok_pushes_its_detail_page() {
        let (store, mut screen) = seeded();
        let key = screen.key_at(store.view().current().unwrap(), 1);
        let out = step(&mut screen, ScreenEvent::PressCommit(plx_machine::machine::PressId(1)), &cx(store.view(), Some(key)));
        assert!(out.iter().any(|e| matches!(&e.fx, plx_machine::machine::Fx::App(AppFx::Content(
            ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "b")));
    }

    /// The hold hint on a Collection page (`ui::hold_hint`): the first time it teaches like Home's
    /// (a 1.5 s dwell on a settled member), and a page of this kind never self-teaches again.
    #[test]
    fn the_hold_hint_stands_on_a_resting_member_once_per_run() {
        let _g = plx_base::testlock::serial();
        plx_ui::hold_hint::reset_learned_for_test();
        plx_ui::hold_hint::reset_shown_for_test();
        let (store, mut screen) = seeded();
        let c = store.view().current().unwrap();
        let (a, b) = (screen.key_at(c, 0), screen.key_at(c, 1));
        let mut ms = 100;
        let mut rest = |screen: &mut CollectionScreen, key, secs: f32| {
            for _ in 0..(secs * 60.0) as u32 {
                ms += 17;
                tick_at(screen, &store, Some(key), ms);
            }
        };
        rest(&mut screen, a, 1.2);
        assert!(!screen.stack.hint_visible(), "not before the dwell");
        // The negatives come BEFORE the first stand: once it has stood the kind's latch is spent
        // and nothing could stand whatever the wiring.
        // a menu open over the page: focus is the menu's, not this page's
        let menu = plx_machine::machine::FocusKey { entry: EntryId(77), elem: 0 };
        rest(&mut screen, menu, 3.0);
        assert!(!screen.stack.hint_visible() && !plx_ui::hold_hint::shown_this_run(plx_ui::hold_hint::Kind::Collection), "never while a menu is open");
        // nor on the header, which is not a card
        let header = plx_machine::machine::FocusKey { entry: EntryId(9), elem: HEADER_ELEM };
        rest(&mut screen, header, 3.0);
        assert!(!screen.stack.hint_visible() && !plx_ui::hold_hint::shown_this_run(plx_ui::hold_hint::Kind::Collection), "never on a non-card focus");
        rest(&mut screen, a, 3.6);
        assert!(screen.stack.hint_visible(), "after the dwell, on a settled member");
        rest(&mut screen, menu, 1.0);
        assert!(!screen.stack.hint_visible(), "a menu taking focus hides it");
        // spent for the kind: another member, and a second Collection page, stay quiet
        rest(&mut screen, b, 3.0);
        assert!(!screen.stack.hint_visible(), "not twice on the same page");
        let (store2, mut again) = seeded();
        let k = again.key_at(store2.view().current().unwrap(), 0);
        for n in 0..240 { tick_at(&mut again, &store2, Some(k), 100 + n * 17); }
        assert!(!again.stack.hint_visible(), "not on a second Collection page");
        // a held OK still shows it, with its fill
        let mut held = cx(store2.view(), Some(k));
        held.press.held_ms = Some(300);
        step(&mut again, ScreenEvent::Tick(Tick { ms: 5000, dt_us: 16_667 }), &held);
        assert!(again.stack.hint_visible(), "a physical hold shows it regardless");
    }

    impl CollectionScreen {
        /// Member `key`'s live pop (no press), judged with `focus` as the engine's focus.
        fn scale_of(&self, store: &plx_data::stores::collection::CollectionStore,
            focus: Option<plx_machine::machine::FocusKey<u32>>, key: plx_machine::machine::FocusKey<u32>) -> f32 {
            self.stack.view(&self.page).scale_of(&cx(store.view(), focus), &key.elem).unwrap()
        }

        /// Member `index`'s cell as the grid places it with nothing focused.
        fn rest_cell(&self, store: &plx_data::stores::collection::CollectionStore, index: usize) -> Rect {
            let c = store.view().current().unwrap();
            let key = self.key_at(c, index);
            Focusable::<CollectionHost>::place(self, &key.elem, &cx(store.view(), None), At::Drawn).unwrap().rect
        }
    }

    fn tick_at(screen: &mut CollectionScreen, store: &plx_data::stores::collection::CollectionStore,
        focus: Option<plx_machine::machine::FocusKey<u32>>, ms: u32) {
        step(screen, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }), &cx(store.view(), focus));
    }

    /// RC.1 nit 7: a member gaining focus grew in one frame, and the one losing it snapped back —
    /// the Collection page drew `focus_scale` as a step where the Library grid and every Home
    /// shelf spring the pop. A D-pad move now arms the shared `GridPop` (the `FocusMoved` arm) and
    /// the frame's ticks carry it: the new member is between rest and full one frame in, the old
    /// one is between full and rest, and both settle.
    #[test]
    fn a_member_gaining_focus_grows_over_frames_and_the_one_losing_it_lets_go() {
        let (store, mut screen) = seeded();
        let c = store.view().current().unwrap();
        let (a, b) = (screen.key_at(c, 0), screen.key_at(c, 1));
        let full = plx_ui::cards::GRID_STYLE.focus_scale;
        // A landing adopts its first focus at full scale.
        tick_at(&mut screen, &store, Some(a), 16);
        assert_eq!(screen.scale_of(&store, Some(a), a), full, "a seated focus is adopted whole");
        step(&mut screen, ScreenEvent::FocusMoved { from: Some(a), to: b, by: By::Dir }, &cx(store.view(), Some(b)));
        tick_at(&mut screen, &store, Some(b), 32);
        let (new, old) = (screen.scale_of(&store, Some(b), b), screen.scale_of(&store, Some(b), a));
        assert!(new > 1.0 + 0.001 && new < full - 0.001, "the new member starts growing from rest: {new}");
        assert!(old > 1.0 + 0.001 && old < full - 0.001, "the old member lets go rather than snapping: {old}");
        for n in 0..120 { tick_at(&mut screen, &store, Some(b), 48 + n * 16); }
        assert!((screen.scale_of(&store, Some(b), b) - full).abs() < 0.002, "…and settles at full scale");
        assert_eq!(screen.scale_of(&store, Some(b), a), 1.0, "a settled neighbour costs nothing");
    }

    /// A hold menu: the page keeps ticking (or is frozen) while the menu owns input, and what the
    /// opener redraw (`Screen::redraw_focused` -> `Grid::redraw_focused`) paints is the card drawn
    /// as the focused one with the OPENER as the context's focus: `opener_parts` hands the lift a
    /// `Cx` whose `focus.current` is the opener key, not the engine's (the menu's own rows). The
    /// member whose menu is open stays at full pop in both worlds: the page frozen on its own
    /// focus, and the page ticking while the engine's focus sits on the menu.
    #[test]
    fn a_hold_menu_over_a_settled_member_keeps_it_popped() {
        let (store, mut screen) = seeded();
        let c = store.view().current().unwrap();
        let (a, b) = (screen.key_at(c, 0), screen.key_at(c, 1));
        let full = plx_ui::cards::GRID_STYLE.focus_scale;
        let menu = plx_machine::machine::FocusKey { entry: EntryId(77), elem: 0 };
        tick_at(&mut screen, &store, Some(a), 0);
        step(&mut screen, ScreenEvent::FocusMoved { from: Some(a), to: b, by: By::Pointer }, &cx(store.view(), Some(b)));
        for n in 0..120 { tick_at(&mut screen, &store, Some(b), 16 + n * 16); }
        step(&mut screen, ScreenEvent::Cover, &cx(store.view(), Some(b)));
        // frozen on the page's own focus
        for n in 0..60 { tick_at(&mut screen, &store, Some(b), 3000 + n * 16); }
        assert!((screen.scale_of(&store, Some(b), b) - full).abs() < 0.002, "the member whose menu is open stays lifted");
        // the engine's focus is on the menu's rows while it ticks: the lift (opener as the
        // context's focus) still draws the opener at full pop, and nothing else is lifted
        for n in 0..60 { tick_at(&mut screen, &store, Some(menu), 4000 + n * 16); }
        assert!((screen.scale_of(&store, Some(b), b) - full).abs() < 0.002,
            "a page ticked under a foreign focus still lifts its opener");
        assert_eq!(screen.scale_of(&store, Some(b), a), 1.0, "no other member is lifted");
    }

    /// The menu is dismissed and focus moves off the opener: the page's own springs take over the
    /// tiles at once (the host's `opener_lift` stands down), so the old opener lets go over frames
    /// and the new one grows from rest; nothing stays at full pop for a stale lift to be taken
    /// from.
    #[test]
    fn dismissing_a_hold_menu_and_moving_leaves_no_stale_lift() {
        let (store, mut screen) = seeded();
        let c = store.view().current().unwrap();
        let (a, b) = (screen.key_at(c, 0), screen.key_at(c, 1));
        let full = plx_ui::cards::GRID_STYLE.focus_scale;
        tick_at(&mut screen, &store, Some(a), 0);
        for n in 0..60 { tick_at(&mut screen, &store, Some(a), 16 + n * 16); }
        step(&mut screen, ScreenEvent::Cover, &cx(store.view(), Some(a)));
        step(&mut screen, ScreenEvent::Uncover, &cx(store.view(), Some(a)));
        step(&mut screen, ScreenEvent::FocusMoved { from: Some(a), to: b, by: By::Dir }, &cx(store.view(), Some(b)));
        tick_at(&mut screen, &store, Some(b), 2000);
        let (old, new) = (screen.scale_of(&store, Some(b), a), screen.scale_of(&store, Some(b), b));
        assert!(old < full - 0.001 && old > 1.0, "the old opener is letting go, not held at full pop: {old}");
        assert!(new < full - 0.001, "the new member grows from rest: {new}");
    }

    /// The failed page's read-out is the shared page read-out (#268): page-placed, the untyped
    /// "can't reach" glyph Home and the Library carry, and never drawn over the live header —
    /// with a three-line summary the glyph shrinks or drops, without one it keeps its full size.
    /// The quiet Empty answer stays in the grid's region and carries no glyph.
    #[test]
    fn a_failed_page_reads_out_with_the_shared_glyph_below_the_header() {
        let (mut store, _) = seeded();
        store.edit_for_test(|c| { c.replace_items_for_test(vec![]); c.summary.clear(); c.status = CollectionStatus::Failed; });
        let c = store.view().current().unwrap();
        let overlay = CollectionScreen::status_overlay(Some(c), 0, &FixtureMeasure);
        assert_eq!(overlay.glyph, Some(plx_ui::icons::Icon::ServerBadgeMinus));
        let glyph = overlay.glyph_frame().expect("a bare header leaves room for the full glyph");
        assert_eq!(glyph.h, StatusOverlay::GLYPH_SIZE);
        assert!(glyph.y >= CollectionScreen::header_text_bottom(c, &FixtureMeasure));

        store.edit_for_test(|c| c.summary = "A long collection summary that runs on. ".repeat(40));
        let c = store.view().current().unwrap();
        let bottom = CollectionScreen::header_text_bottom(c, &FixtureMeasure);
        let overlay = CollectionScreen::status_overlay(Some(c), 0, &FixtureMeasure);
        if let Some(glyph) = overlay.glyph_frame() {
            assert!(glyph.y >= bottom, "glyph top {} is above the summary's bottom {bottom}", glyph.y);
        }

        store.edit_for_test(|c| c.status = CollectionStatus::Empty);
        let overlay = CollectionScreen::status_overlay(store.view().current(), 0, &FixtureMeasure);
        assert!(overlay.glyph_frame().is_none() && overlay.kind == StatusKind::Empty,
            "an empty collection is a quiet answer, not a failure");
    }

    /// C4a: an empty collection keeps its header — the kind alone on the meta line, not
    /// "0 items" — and says so in the grid's region, centred where the mock centres it.
    #[test]
    fn an_empty_collection_reads_out_quietly_in_the_grids_region() {
        let (mut store, _) = seeded();
        store.edit_for_test(|c| { c.replace_items_for_test(vec![]); c.child_count = 0; c.total = 0; c.status = CollectionStatus::Empty; });
        let c = store.view().current().unwrap();
        assert_eq!(meta_key(c), (0, true), "the meta line is the kind alone");
        assert!(CollectionScreen::shows_head(c));
        let overlay = CollectionScreen::status_overlay(Some(c), 0, &FixtureMeasure);
        let f = overlay.frame;
        assert_eq!((f.x, f.y, f.w, f.h), (96.0, 460.0, 1728.0, 475.0));
        assert_eq!(overlay.caption, plx_platform::i18n::msg::browse_collection_empty_c());
    }

    /// C4b: an unavailable collection is the page-filling Failed verdict at the shared anchor
    /// with its reason and no way on but BACK — and no header, since there is nothing to head.
    #[test]
    fn an_unavailable_collection_fills_the_page_with_its_verdict_and_no_header() {
        let (mut store, _) = seeded();
        store.edit_for_test(|c| { c.replace_items_for_test(vec![]); c.status = CollectionStatus::Unavailable; });
        let c = store.view().current().unwrap();
        assert!(!CollectionScreen::shows_head(c));
        let overlay = CollectionScreen::status_overlay(Some(c), 0, &FixtureMeasure);
        assert_eq!(overlay.kind, StatusKind::Failed);
        let f = overlay.frame;
        assert_eq!((f.x, f.y, f.w, f.h), (0.0, 0.0, SCR_W, SCR_H));
        assert!(overlay.reason.is_some() && overlay.action.is_none());
    }

    /// C1: the header art is the mock's 200×300 tile at the content edge, the text column starts
    /// at 360, and the first grid row's posters start at 520 under the "Items" heading.
    #[test]
    fn the_header_and_grid_sit_on_the_mocks_lines() {
        let (store, mut screen) = seeded();
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let art = Page::art_rect(screen.stack.scroll());
        assert_eq!((art.x, art.y, art.w, art.h), (96.0, 96.0, 200.0, 300.0));
        let column = Page::header_rect(screen.stack.scroll());
        assert_eq!((column.x, column.y, column.w), (360.0, 96.0, 1344.0));
        let first = screen.rest_cell(&store, 0);
        assert_eq!(first.y, 520.0);
        assert_eq!(first.x, MARGIN_X);
        assert!(ITEMS_HEADING_Y + theme::size::HEADLINE as f32 <= GRID_TOP);
    }

    /// C2: scrolling to a lower row rests the first visible row on the content edge, with the
    /// header gone and nothing of the row above it left showing.
    #[test]
    fn a_scrolled_page_rests_a_row_on_the_content_edge() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.replace_items_for_test((0..30).map(|i| item(&format!("m{i}"))).collect()));
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let cols = plx_ui::cards::GRID_COLS;
        let key = screen.key_at(store.view().current().unwrap(), 2 * cols);
        step(&mut screen, ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore }, &cx(store.view(), Some(key)));
        for n in 0..240 { tick_at(&mut screen, &store, Some(key), 16 + n * 16); }
        let scroll = screen.stack.scroll();
        assert!(scroll > 0.0);
        // the scroll rounds up to some row's top on the edge (row 1's: the first row `Grid` does
        // not cull), so that row's posters rest exactly there and the row above has left
        let row = screen.rest_cell(&store, cols);
        assert!((row.y - CONTENT_TOP).abs() < 0.5, "the snapped row rests at {}", row.y);
        assert!(screen.rest_cell(&store, 0).y + CARD_H < CONTENT_TOP, "nothing of the row above shows");
        assert_eq!(head_alpha(scroll), 0.0, "no remnant of the header");
        assert_eq!(head_alpha(0.0), 1.0);
    }

    /// Down from above a short last row lands on its last member (the Library grid's rule)
    /// rather than stopping where no card sits directly below.
    #[test]
    fn down_onto_a_short_last_row_lands_on_its_last_member() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.replace_items_for_test((0..8).map(|i| item(&format!("m{i}"))).collect()));
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let c = store.view().current().unwrap();
        let from = screen.key_at(c, 4);
        let step = Focusable::<CollectionHost>::neighbour(&screen, from, Dir::Down, &cx(store.view(), Some(from)));
        assert!(matches!(step, Step::Move(key) if key == screen.key_at(c, 7)));
        let from = screen.key_at(c, 7);
        assert!(matches!(Focusable::<CollectionHost>::neighbour(&screen, from, Dir::Down,
            &cx(store.view(), Some(from))), Step::Edge), "the last row has nothing below");
    }

    /// 5,000 members walked to the end and back with focus riding along: the key table follows
    /// the pages the store holds (never the list), and the focused card keeps its element, its
    /// index and its column through the eviction of its neighbours.
    #[test]
    fn a_5000_member_walk_keeps_the_key_table_bounded_and_the_focused_card_keyed() {
        let (mut store, mut screen) = seeded();
        let many: Vec<PmsMovie> = (0..5000).map(|i| item(&format!("m{i}"))).collect();
        let walk: Vec<usize> = (0..5000).step_by(45).chain((0..5000).step_by(45).rev()).collect();
        let cols = plx_ui::cards::GRID_COLS;
        let mut ms = 32;
        let mut peak_keys = 0;
        for (n, &idx) in walk.iter().enumerate() {
            // The store's pages as a read of the window around `idx` leaves them.
            store.edit_for_test(|c| {
                c.replace_items_for_test(many.clone());
                c.evict_for_test(idx.saturating_sub(PAGE_SIZE)..idx + 2 * PAGE_SIZE, Some(idx), None);
            });
            ms += 16;
            tick_at(&mut screen, &store, None, ms);
            let c = store.view().current().unwrap();
            screen.page.reach(c, idx); // navigation would have projected the row it arrives on
            let key = screen.key_at(c, idx);
            ms += 16;
            tick_at(&mut screen, &store, Some(key), ms);
            // The next stop's eviction, with focus still on `idx`.
            let next = walk.get(n + 1).copied().unwrap_or(idx);
            store.edit_for_test(|c| {
                c.replace_items_for_test(many.clone());
                c.evict_for_test(next.saturating_sub(PAGE_SIZE)..next + 2 * PAGE_SIZE, Some(idx), None);
            });
            ms += 16;
            tick_at(&mut screen, &store, Some(key), ms);
            let c = store.view().current().unwrap();
            let held = c.loaded_rows().count();
            assert!(held <= 11 * PAGE_SIZE, "{held} rows held");
            assert!(screen.page.cards.len() <= 2 * held + 256, "{} keys for {held} rows", screen.page.cards.len());
            peak_keys = peak_keys.max(screen.page.cards.len());
            assert_eq!(screen.item_index(c, key.elem), Some(idx), "the focused card keeps its index at step {n}");
            assert_eq!(screen.item_index(c, key.elem).unwrap() % cols, idx % cols, "and its column");
            assert_eq!(screen.focused_item(Some(key), &cx(store.view(), Some(key))).map(|m| m.rk.clone()),
                Some(format!("m{idx}")), "and its identity");
        }
        assert!(peak_keys < 1500, "peak {peak_keys} keys for a 5000-member list");
    }

    /// Leaving to Detail deep in the list saves the focused member and the directory of kept
    /// counts, and the return sends the store the counts so the pages before the member are not
    /// all read again.
    #[test]
    fn a_return_deep_in_the_list_sends_the_saved_counts_with_the_focus() {
        let (mut store, mut screen) = seeded();
        let many: Vec<PmsMovie> = (0..5000).map(|i| item(&format!("m{i}"))).collect();
        store.edit_for_test(|c| {
            c.replace_items_for_test(many);
            c.evict_for_test(2900..3100, Some(3000), None);
        });
        tick_at(&mut screen, &store, None, 48);
        screen.page.reach(store.view().current().unwrap(), 3000);
        let key = screen.key_at(store.view().current().unwrap(), 3000);
        tick_at(&mut screen, &store, Some(key), 64);
        let PageMemory::Collection(memory) = Screen::<CollectionHost>::memory_at(&screen, Some(key)) else { panic!() };
        assert_eq!((memory.total, memory.counts.len(), memory.focus_index), (5000, 5000usize.div_ceil(PAGE_SIZE), Some(3000)));

        let mut back = CollectionScreen::new(EntryId(9), set());
        let _ = step(&mut back, ScreenEvent::RestoreMemory(PageMemory::Collection(memory)), &cx(store.view(), None));
        let out = step(&mut back, ScreenEvent::Enter(plx_ui::screen::Enter::Restored), &cx(store.view(), None));
        let sent = out.iter().find_map(|e| match &e.fx {
            plx_machine::machine::Fx::App(AppFx::Store(_, plx_data::stores::StoreCmd::Collection(
                CollectionCmd::Restore { total, counts }))) => Some((*total, counts.len())),
            _ => None,
        });
        assert_eq!(sent, Some((5000, 84)), "the saved directory goes back to the store");
    }

    /// A slot whose page is not held draws the not-yet-loaded card, and becomes its member's card
    /// when the page lands.
    #[test]
    fn a_hole_in_the_visible_range_draws_the_placeholder_and_fills_when_its_page_lands() {
        let (mut store, mut screen) = seeded();
        let many: Vec<PmsMovie> = (0..5000).map(|i| item(&format!("m{i}"))).collect();
        store.edit_for_test(|c| {
            c.replace_items_for_test(many.clone());
            c.evict_for_test(3000..3120, Some(3000), None);
        });
        tick_at(&mut screen, &store, None, 48);
        let c = store.view().current().unwrap();
        let members = screen.page.members(c);
        assert!(screen.page.elem_at(c, 100).is_some_and(|e| e >= HOLE_ELEM_BASE), "slot 100 is a hole");
        assert!(matches!(CardSource::<CollectionHost>::art(&members, 100), Art::Poster(None)));
        assert_eq!(CardSource::<CollectionHost>::len(&members), 5000, "the grid still spans the known list");

        store.edit_for_test(|c| { c.replace_items_for_test(many.clone()); c.evict_for_test(0..200, Some(100), None); });
        tick_at(&mut screen, &store, None, 64);
        let c = store.view().current().unwrap();
        screen.page.reach(c, 100); // slot 100 is not painted from the top; scrolling to it projects it
        let members = screen.page.members(c);
        assert!(screen.page.elem_at(c, 100).is_some_and(|e| e < HOLE_ELEM_BASE), "slot 100 has its member");
        assert!(matches!(CardSource::<CollectionHost>::art(&members, 100), Art::Poster(Some(_))));
    }

    /// Back onto a page rebuilt from its first page holds the remembered member while the pages
    /// above it load, and the entry asks for every member the page knew.
    #[test]
    fn a_restore_past_the_first_page_waits_for_its_member() {
        let (mut store, original) = seeded();
        let many: Vec<PmsMovie> = (0..PAGE_SIZE + 10).map(|i| item(&format!("m{i}"))).collect();
        store.edit_for_test(|c| c.replace_items_for_test(many.clone()));
        let mut original = original;
        original.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        original.page.reach(store.view().current().unwrap(), PAGE_SIZE + 5);
        let focus = original.key_at(store.view().current().unwrap(), PAGE_SIZE + 5);
        let PageMemory::Collection(memory) = Screen::<CollectionHost>::memory_at(&original, Some(focus)) else { panic!() };

        store.edit_for_test(|c| { c.replace_items_for_test(many[..PAGE_SIZE].to_vec()); c.more = true; });
        let mut restored = CollectionScreen::new(EntryId(9), set());
        let _ = step(&mut restored, ScreenEvent::RestoreMemory(PageMemory::Collection(memory)), &cx(store.view(), None));
        let out = step(&mut restored, ScreenEvent::Enter(plx_ui::screen::Enter::Restored), &cx(store.view(), None));
        let want = out.iter().find_map(|e| match &e.fx {
            plx_machine::machine::Fx::App(AppFx::Store(_, plx_data::stores::StoreCmd::Collection(
                CollectionCmd::Open { target, .. }))) => Some(target.want),
            _ => None,
        });
        assert!(want.is_some_and(|w| w > PAGE_SIZE + 5), "the entry asks for the remembered member's page: {want:?}");
        restored.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let got = Focusable::<CollectionHost>::reconcile(&restored, focus, &cx(store.view(), Some(focus)));
        assert_eq!(got, focus, "focus waits on the member rather than falling to the first card");

        store.edit_for_test(|c| { c.replace_items_for_test(many.clone()); c.more = false; });
        restored.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let got = Focusable::<CollectionHost>::reconcile(&restored, focus, &cx(store.view(), Some(focus)));
        assert_eq!(restored.focused_item(Some(got), &cx(store.view(), Some(got))).unwrap().rk,
            format!("m{}", PAGE_SIZE + 5));
    }

    /// Owner decision 3 (one recovery rule on every card screen): when the focused member leaves the
    /// page, focus goes to the member now at the same position, clamped to the grid — not to the
    /// first member.
    #[test]
    fn a_removed_focused_member_hands_focus_to_the_same_position_clamped() {
        let (mut store, mut screen) = seeded();
        let c = store.view().current().unwrap();
        let (b, last) = (screen.key_at(c, 1), screen.key_at(c, 2));
        tick_at(&mut screen, &store, Some(b), 32);
        store.edit_for_test(|c| { c.edit_items_for_test(|v| { v.remove(1); }); });
        tick_at(&mut screen, &store, Some(b), 48);
        let c = store.view().current().unwrap();
        let got = Focusable::<CollectionHost>::reconcile(&screen, b, &cx(store.view(), Some(b)));
        assert_eq!(got, screen.key_at(c, 1), "the member that slid into position 1 takes focus");
        assert_eq!(got, last);

        tick_at(&mut screen, &store, Some(got), 64);
        store.edit_for_test(|c| { c.edit_items_for_test(|v| { v.remove(1); }); });
        tick_at(&mut screen, &store, Some(got), 80);
        let c = store.view().current().unwrap();
        let got = Focusable::<CollectionHost>::reconcile(&screen, got, &cx(store.view(), Some(got)));
        assert_eq!(got, screen.key_at(c, 0), "past the end, the last member that is left");
    }

    /// The page's group ids are the page's own, and the first group listed is the grid's: a
    /// truncating summary makes the header focusable, and with nothing focused the engine's
    /// first D-pad press still seats the first MEMBER, not the header.
    #[test]
    fn a_page_with_a_truncating_summary_seats_the_first_member_on_the_first_press() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.summary = "A long collection summary that runs on. ".repeat(40));
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        tick_at(&mut screen, &store, None, 32);
        assert!(screen.page.summary_more);
        let context = cx(store.view(), None);
        let mut links = Vec::new();
        Screen::<CollectionHost>::links(&screen, &mut links);
        let mut engine = plx_ui::focus::FocusEngine::new();
        let plx_ui::focus::Outcome::Moved { to, .. } =
            engine.move_dir(context.owner, &screen, &links, Dir::Down, &context)
        else {
            panic!("the first press must seat something");
        };
        assert_eq!(to, screen.key_at(store.view().current().unwrap(), 0), "the first member, not the header");
        let mut groups = Vec::new();
        Focusable::<CollectionHost>::groups(&screen, &context, &mut groups);
        let ids: Vec<u32> = groups.iter().map(|g| g.id.0).collect();
        assert_eq!(ids, [0, 1], "grid is group 0, header group 1, in that order");
    }

    /// With no collection at all, reconcile answers the header, as before (never the bare `want`).
    #[test]
    fn reconcile_with_nothing_to_own_answers_the_header() {
        let store = plx_data::stores::collection::CollectionStore::default();
        let mut screen = CollectionScreen::new(EntryId(9), set());
        tick_at(&mut screen, &store, None, 16);
        let want = plx_machine::machine::FocusKey { entry: EntryId(9), elem: 0x7777 };
        let got = Focusable::<CollectionHost>::reconcile(&screen, want, &cx(store.view(), None));
        assert_eq!(got, plx_machine::machine::FocusKey { entry: EntryId(9), elem: HEADER_ELEM });
    }

    #[test]
    fn page_memory_restores_the_same_member_after_detail() {
        let (store, original) = seeded();
        let focus = original.key_at(store.view().current().unwrap(), 1);
        let PageMemory::Collection(memory) = Screen::<CollectionHost>::memory_at(&original, Some(focus)) else { panic!() };
        let mut restored = CollectionScreen::new(EntryId(9), set());
        restored.restore(&memory);
        restored.page.return_pending = true;
        restored.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let got = Focusable::<CollectionHost>::reconcile(&restored, focus, &cx(store.view(), Some(focus)));
        assert_eq!(got, focus);
        assert_eq!(restored.focused_item(Some(got), &cx(store.view(), Some(got))).unwrap().rk, "b");
    }

    /// **The collection page's fixed slots fit in every shipped language**: the season mark on a
    /// grid poster (`widgets::poster_label` elides to the card less its insets) and the one-line
    /// meta line beside the artwork. Measured with the device's whole-pixel advances.
    #[test]
    fn the_collection_pages_fixed_slots_fit_in_every_language() {
        use plx_base::fontcov::advances::ShippedMeasure;
        use plx_ui::fit::HEADROOM;
        use plx_platform::i18n::{language_on_this_thread_for_test, msg, Preference};
        use plx_machine::machine::Measure;
        let m = ShippedMeasure;
        let mark_budget = (CARD_W - 2.0 * 16.0) * HEADROOM;
        let mut out = Vec::new();
        for language in [Preference::En, Preference::Es, Preference::Be] {
            let _guard = language_on_this_thread_for_test(language);
            let season = PmsMovie { kind: 2, season_index: 99, ..Default::default() };
            let mark = member_label(&season);
            let w = m.width_str(&mark, theme::size::LABEL, true);
            if w > mark_budget { out.push(format!("{}: {mark:?} is {w:.0}px in {mark_budget:.0}px", language.tag())); }
            for meta in [msg::browse_collection_kind().to_owned(),
                msg::browse_collection_meta(&plx_ui::fmt::item_count(99_999))] {
                let w = m.width_str(&meta, theme::size::LABEL, false);
                if w > TEXT_W * HEADROOM { out.push(format!("{}: {meta:?} is {w:.0}px in {TEXT_W:.0}px", language.tag())); }
            }
            // The one-line read-outs: the empty answer across its region, the unavailable verdict
            // (TITLE bold) across the page's reading width.
            let empty = msg::browse_collection_empty();
            let w = m.width_str(empty, theme::size::BODY, false);
            if w > STATUS_FRAME.w * HEADROOM { out.push(format!("{}: {empty:?} is {w:.0}px", language.tag())); }
            let verdict = msg::browse_collection_unavailable();
            let w = m.width_str(verdict, theme::size::TITLE, true);
            if w > (SCR_W - 2.0 * MARGIN_X) * HEADROOM { out.push(format!("{}: {verdict:?} is {w:.0}px", language.tag())); }
            let heading = format!("{} · {}", msg::browse_collection_items(), msg::browse_collection_order_release());
            let w = m.width_str(&heading, theme::size::HEADLINE, true);
            if w > (SCR_W - 2.0 * MARGIN_X) * HEADROOM { out.push(format!("{}: {heading:?} is {w:.0}px", language.tag())); }
        }
        assert!(out.is_empty(), "collection text the television would clip:\n  {}", out.join("\n  "));
    }

    /// **The pseudo-locale sweep of the collection page**, as `detail/identity_tests.rs` does for
    /// Detail: every run the page hands the text renderer, in each status it can show, is catalog
    /// text (the `[!! … !!]` marker or the pseudo-locale's accented vowels), the fixture's own
    /// server values, or letter-free. Anything else is English drawn without the catalog.
    #[test]
    fn every_app_owned_run_on_the_collection_page_comes_from_the_catalog() {
        use plx_ui::screen::DrawFrame;
        let _serial = plx_base::testlock::serial();
        let _pseudo = plx_platform::i18n::pseudo_on_this_thread_for_test();
        let server = ["Set", "Qwerty", "Zzyzx", "Vlox"];
        let season = PmsMovie { rk: "s".into(), kind: 2, season_index: 3, title: "Zzyzx".into(),
            show_title: "Vlox".into(), ..Default::default() };
        let mut stray = Vec::new();
        for status in [CollectionStatus::Ready, CollectionStatus::Loading, CollectionStatus::Empty,
            CollectionStatus::Unavailable, CollectionStatus::Failed] {
            let (mut store, mut screen) = seeded();
            store.edit_for_test(|c| {
                c.summary = "Qwerty".into();
                c.edit_items_for_test(|v| v.push(season.clone()));
                c.child_count = c.shown();
                c.status = status;
                if status != CollectionStatus::Ready { c.replace_items_for_test(vec![]); c.child_count = 0; }
            });
            screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
            let context = cx(store.view(), None);
            let runs = plx_gfx::text::capture_text_runs_for_test(|| {
                let mut f = DrawFrame::new(&context, plx_ui::Painter::recording());
                plx_gfx::gfx::without_frame_clear(|| Screen::<CollectionHost>::draw(&mut screen, &mut f));
            });
            assert!(runs.iter().any(|run| run.contains("[!!")), "{status:?} drew catalog text: {runs:?}");
            let pseudo = |run: &str| run.contains("[!!") || run.contains(['á', 'ë', 'ï', 'ö', 'ü']);
            stray.extend(runs.into_iter().filter(|run| !pseudo(run)).filter(|run| {
                let mut rest = run.replace('\u{a0}', " ");
                for value in server { rest = rest.replace(value, ""); }
                rest.chars().any(char::is_alphabetic)
            }).map(|run| format!("{status:?}: {run:?}")));
        }
        assert!(stray.is_empty(), "text drawn without the catalog: {stray:?}");
    }
}
