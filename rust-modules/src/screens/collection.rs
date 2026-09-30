//! Owned collection page: a Person-like header over a stable six-column portrait grid. The
//! Collection store owns resolution and paging; this screen owns only presentation state and the
//! identity→element registry that lets Back restore the exact member tile.

use std::ffi::CString;

use crate::collection::{Collection, CollectionOrder, CollectionStatus, CollectionTarget, PAGE_SIZE};
use crate::plex::collections::CollectionRef;
use crate::pms::PmsMovie;
use crate::stores::collection::CollectionCmd;
use crate::ui::card_row::{self, TileLabel};
use crate::ui::consts::*;
use crate::ui::label::{Label, VAlign};
use crate::ui::machine::{Canon, Cx, Edge, Effects, EntryId, GroupId, Handled, InputEvent,
    InputKind, Key, Leave, LogicalState, Machine, Tick};
use crate::ui::present::{PresentEvent, Provenance};
use crate::ui::screen::{Activate, At, AxisMask, By, Dir, DrawFrame, EdgeRule, ElemKind,
    FocusSource, Focusable, GroupKind, GroupSpec, HitSource, Hover, Link, Placed,
    RenderStrategy, Screen, ScreenEvent, Seat, Step, Stop};
use crate::ui::text_view::TextView;
use crate::ui::theme;
use crate::ui::widgets::{self, Art, PageGround, StatusKind, StatusOverlay};
use crate::ui::{Env, Painter, Rect, Spring};

use super::registry::{AppFx, CardKeys, CardPageMemory, CollectionLike, ContentArg,
    ContentLike, ContentPanel, ContentReq, PageMemory};

const HEADER_ELEM: u32 = 0;
const RETRY_ELEM: u32 = 1;
const FIRST_CARD_ELEM: u32 = 0x1000;
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
const LOAD_AHEAD: usize = crate::ui::poster_grid::COLS * 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Located { Header, Retry, Card(usize) }

fn summary_view<'a>(summary: &'a str, measure: &'a dyn crate::ui::machine::Measure) -> TextView<'a> {
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
        Some(CollectionOrder::Release) => crate::i18n::msg::browse_collection_order_release(),
        Some(CollectionOrder::Title) => crate::i18n::msg::browse_collection_order_title(),
        Some(CollectionOrder::Custom) => crate::i18n::msg::browse_collection_order_custom(),
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
        (_, true) => crate::i18n::msg::browse_collection_kind_c().to_owned(),
        (count, false) => CString::new(crate::i18n::msg::browse_collection_meta(&crate::ui::fmt::item_count(count)))
            .unwrap_or_default(),
    }
}

pub(crate) fn member_label(item: &PmsMovie) -> String {
    match item.kind {
        2 if item.season_index > 0 => crate::i18n::msg::browse_collection_season_mark(item.season_index as i64),
        3 => crate::ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64),
        _ => String::new(),
    }
}

pub(crate) fn member_caption(item: &PmsMovie) -> TileLabel {
    if item.kind == 3 {
        let address = crate::ui::fmt::episode_address(item.season_index as i64, item.ep_index as i64);
        let caption = match (item.show_title.is_empty(), address.is_empty()) {
            (false, false) => format!("{} · {}", item.show_title, address),
            (false, true) => item.show_title.clone(),
            (true, false) => address,
            (true, true) => String::new(),
        };
        return TileLabel::titled(&item.title, &caption);
    }
    card_row::poster_label(item)
}

pub(crate) struct CollectionScreen {
    entry: EntryId,
    /// The page's argument; `id.rk` adopts the store's resolution of a tag route.
    id: CollectionRef,
    header_marked: bool,
    cards: CardKeys,
    /// `elems[i]` is the engine element of the synced collection's `items[i]`, rebuilt by
    /// [`Self::sync`] so a frame finds a card's element or index without scanning identities.
    /// Derived, not logical state.
    elems: Vec<u32>,
    /// `labels[i]` is `items[i]`'s persistent poster label ([`member_label`]), built beside
    /// [`Self::elems`] so a frame formats none. Derived, not logical state.
    labels: Vec<String>,
    /// The header's meta line and the `(count, kind-only)` it was built for ([`meta_line`]).
    /// Derived, not logical state.
    meta: ((i64, bool), CString),
    return_pending: bool,
    teardown_closed: bool,
    scroll: Spring,
    scroll_target: f32,
    ground: PageGround,
    ground_seeded: bool,
    summary_more: bool,
    links_c: Vec<Link>,
    /// `(items, summary bytes)` the last [`Self::sync`] saw — derived, not logical state. The
    /// frame tick re-syncs only when either moved; `StoreChanged` always re-syncs.
    synced: Option<(usize, usize)>,
    /// The focused row's caption band and the rows still closing behind it — the Library grid's
    /// motion (`ui::poster_grid::GridBands`), so a focused member's caption opens room under its
    /// row rather than drawing over the posters below. Presentation, not logical state.
    bands: crate::ui::poster_grid::GridBands,
    /// The header summary's focus lift ([`Self::summary_marked`]). Presentation, not logical state.
    summary_lift: crate::ui::text_lift::TextLift,
}

impl LogicalState for CollectionScreen {
    fn write(&self, c: &mut Canon) {
        c.u32(self.entry.0).u32(self.id.sid.raw() as u32).str(&self.id.rk)
            .u64(self.id.sec as u64).u64(self.id.tag as u64).str(&self.id.name)
            .bool(self.header_marked).bool(self.return_pending)
            .u32(self.cards.next).seq(self.cards.len());
        for key in &self.cards.keys { c.u32(key.sid.raw() as u32).str(&key.rk).u32(key.elem); }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("collection sid={} rk={} sec={} tag={} cards={} next={} return_pending={}",
            self.id.sid.raw(), self.id.rk, self.id.sec, self.id.tag, self.cards.len(), self.cards.next,
            self.return_pending));
    }
}

impl CollectionScreen {
    pub(crate) const SHAPE: &'static str = "CollectionScreen{entry:u32,sid:u32,rk:String,sec:i64,tag:i64,name:String,header_marked:bool,return_pending:bool,next_elem:u32,card_keys:[{sid:u32,rk:String,elem:u32}]}";

    pub(crate) fn new(entry: EntryId, id: CollectionRef) -> Self {
        Self { entry, id, header_marked: false, cards: CardKeys::new(FIRST_CARD_ELEM),
            elems: Vec::new(), labels: Vec::new(), meta: ((0, true), CString::default()), return_pending: false, teardown_closed: false,
            scroll: Spring::default(), scroll_target: 0.0, ground: PageGround::new(),
            ground_seeded: false, summary_more: false, links_c: Vec::new(), synced: None,
            bands: crate::ui::poster_grid::GridBands::new(),
            summary_lift: crate::ui::text_lift::TextLift::new() }
    }

    /// The header summary earns its marked/lifted treatment when the header holds focus, is
    /// marked open, and the summary truncates.
    fn summary_marked(&self, header_focused: bool) -> bool {
        header_focused && self.header_marked && self.summary_more
    }

    fn target(&self, want: usize) -> CollectionTarget {
        CollectionTarget { id: self.id.clone(), want }
    }

    fn request_store<H: ContentLike + CollectionLike>(&mut self, want: usize, fx: &mut Effects<'_, H>) {
        fx.push(crate::ui::machine::Fx::App(AppFx::Store(
            crate::stores::StoreId::Collection,
            crate::stores::StoreCmd::Collection(CollectionCmd::Open { target: self.target(want) }),
        )));
    }

    fn collection<'a, H: CollectionLike>(&self, cx: &Cx<'a, H>) -> Option<&'a Collection> {
        H::collection(cx).current().filter(|c| c.id.same_collection(&self.id))
    }

    fn sync(&mut self, collection: &Collection, measure: &dyn crate::ui::machine::Measure) {
        self.synced = Some((collection.items.len(), collection.summary.len()));
        if self.id.rk.is_empty() && !collection.id.rk.is_empty() { self.id.rk = collection.id.rk.clone(); }
        self.elems = self.cards.intern_all(
            collection.items.iter().map(|item| (item.sid, item.rk.as_str())), "collection");
        self.labels = collection.items.iter().map(member_label).collect();
        self.meta = (meta_key(collection), meta_line(collection));
        self.summary_more = !collection.summary.is_empty() && summary_view(&collection.summary, measure).truncates(TEXT_W);
        self.links_c.clear();
        if self.summary_more && !collection.items.is_empty() {
            self.links_c.push(Link { from: HEADER_GROUP, dir: Dir::Down, to: GRID_GROUP });
            self.links_c.push(Link { from: GRID_GROUP, dir: Dir::Up, to: HEADER_GROUP });
        }
    }

    pub(crate) fn restore(&mut self, memory: &CardPageMemory) {
        self.cards.merge(&memory.cards, FIRST_CARD_ELEM, "collection");
        self.header_marked |= memory.header_marked;
    }

    fn memory(&self) -> CardPageMemory {
        CardPageMemory { cards: self.cards.clone(), header_marked: self.header_marked }
    }

    /// **A Back whose member has not landed yet.** The remembered member may sit past the pages
    /// loaded so far (a page rebuilt after its store was superseded reloads from the first page),
    /// so focus is held on it while the model is still loading or still has pages below the
    /// member's remembered position — `cards` is interned in server order, so that position
    /// is the member's index when the page was left.
    fn awaiting_restore(&self, collection: &Collection, elem: u32) -> bool {
        if !self.return_pending || self.item_index(collection, elem).is_some() { return false; }
        let Some(position) = self.cards.position(elem) else { return false };
        matches!(collection.status, CollectionStatus::Loading | CollectionStatus::Failed)
            || (collection.more && collection.items.len() <= position)
    }

    /// The member count a page entry asks the store for: one page, or — returning to a page
    /// that remembers members — every member it knew, so the remembered one can be reached.
    fn entry_want(&self) -> usize {
        if self.return_pending { PAGE_SIZE.max(self.cards.len()) } else { PAGE_SIZE }
    }

    /// Is [`Self::elems`] the index of `collection` as it stands? Items only append between
    /// syncs, so a length match is the check; a landing the page has not synced yet falls back to
    /// the identity scan.
    fn indexed(&self, collection: &Collection) -> bool {
        self.elems.len() == collection.items.len()
    }

    fn elem_at(&self, collection: &Collection, index: usize) -> Option<u32> {
        if self.indexed(collection) { return self.elems.get(index).copied(); }
        collection.items.get(index).and_then(|item| self.cards.elem_for(item.sid, &item.rk))
    }

    fn item_index(&self, collection: &Collection, elem: u32) -> Option<usize> {
        if self.indexed(collection) { return self.elems.iter().position(|&e| e == elem); }
        let identity = self.cards.get(elem)?;
        collection.items.iter().position(|item| crate::plex::same_item(
            (identity.sid, identity.rk.as_str()), (item.sid, item.rk.as_str())))
    }

    fn locate(&self, collection: &Collection, elem: u32) -> Option<Located> {
        if elem == HEADER_ELEM && self.summary_more { return Some(Located::Header); }
        if elem == RETRY_ELEM && collection.status == CollectionStatus::Failed { return Some(Located::Retry); }
        self.item_index(collection, elem).map(Located::Card)
    }

    fn key_at(&self, collection: &Collection, index: usize) -> crate::ui::machine::FocusKey<u32> {
        crate::ui::machine::FocusKey { entry: self.entry,
            elem: self.elem_at(collection, index).unwrap_or(HEADER_ELEM) }
    }

    fn card_rect(&self, index: usize, focused: bool, press: f32) -> Rect {
        let scale = if focused { crate::ui::poster_grid::STYLE.focus_scale * if press > 0.0 { press } else { 1.0 } } else { 1.0 };
        self.cell(index).scaled(scale)
    }

    /// Member `index`'s resting cell at the live scroll and live caption bands — the ONE rect
    /// draw, hit stops and focus placement all read.
    fn cell(&self, index: usize) -> Rect {
        crate::ui::poster_grid::cell(index, GRID_TOP, self.scroll.pos, &self.bands.geometry())
    }

    /// The row whose caption band is open: the focused member's, while focus is on a member.
    fn focused_row<H: CollectionLike>(&self, collection: &Collection, cx: &Cx<'_, H>) -> Option<usize> {
        cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| self.item_index(collection, key.elem))
            .map(|index| index / crate::ui::poster_grid::COLS)
    }

    fn status_frame() -> Rect { STATUS_FRAME }

    /// The header text column's two anchors — the meta line's and the summary's cap tops — shared
    /// by [`Self::draw_header`] and [`Self::header_text_bottom`] so the read-out's glyph ceiling
    /// is the drawn header, not a copy of its arithmetic.
    fn header_ys(_measure: &dyn crate::ui::machine::Measure) -> (f32, f32) {
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
    fn header_text_bottom(collection: &Collection, measure: &dyn crate::ui::machine::Measure) -> f32 {
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
        measure: &dyn crate::ui::machine::Measure) -> StatusOverlay<'a> {
        match collection.map(|c| c.status).unwrap_or(CollectionStatus::Loading) {
            CollectionStatus::Loading => StatusOverlay::new(Self::status_frame(), crate::i18n::msg::browse_collection_loading_c(), StatusKind::Working).phase(tick),
            CollectionStatus::Empty => StatusOverlay::new(Self::status_frame(), crate::i18n::msg::browse_collection_empty_c(), StatusKind::Empty),
            CollectionStatus::Unavailable => StatusOverlay::new(Rect::FULL, crate::i18n::msg::browse_collection_unavailable_c(), StatusKind::Failed)
                .page(crate::ui::icons::Icon::PersonBadgeXmark)
                .reason(crate::i18n::msg::browse_collection_unavailable_reason_c()),
            CollectionStatus::Failed => {
                let overlay = StatusOverlay::new(Rect::FULL, crate::i18n::msg::browse_home_failed_c(), StatusKind::Failed)
                    .page(crate::ui::icons::Icon::ServerBadgeMinus).action(crate::i18n::msg::browse_action_retry_c());
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
        self.header_marked = true;
        if self.collection(cx).is_some() && self.summary_more {
            fx.push(crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Panel(
                ContentPanel::CollectionAbout))));
            fx.invalidate(Provenance::Input);
        }
    }

    fn maybe_page<H: ContentLike + CollectionLike>(&mut self, collection: &Collection, index: usize, fx: &mut Effects<'_, H>) {
        if collection.more && index.saturating_add(LOAD_AHEAD) >= collection.items.len() {
            self.request_store(collection.items.len().saturating_add(PAGE_SIZE), fx);
        }
    }

    fn tick<H: ContentLike + CollectionLike>(&mut self, t: Tick, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) {
        let Some(collection) = self.collection(cx) else { return };
        // `sync` walks every member against every key and measures the summary — per landing, not
        // per frame (a few hundred members would otherwise cost a quadratic walk every tick).
        if self.synced != Some((collection.items.len(), collection.summary.len())) {
            self.sync(collection, cx.measure);
        }
        if let Some(index) = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| self.item_index(collection, key.elem)) {
            self.scroll_target = crate::ui::poster_grid::snap_row(self.scroll.pos,
                index / crate::ui::poster_grid::COLS, collection.items.len(), GRID_TOP, CONTENT_TOP);
        } else { self.scroll_target = 0.0; }
        // A focus the reader did not move (a restore, a landing) adopts its band settled; a D-pad
        // move opens it with motion from `FocusMoved`, as the Library grid does.
        self.bands.focus(self.focused_row(collection, cx), false);
        self.bands.tick(crate::ui::poster_grid::STYLE.k_scroll, t.dt());
        self.scroll.step(self.scroll_target, K_SCROLL, t.dt());
        let header_focused = cx.focus.current
            .is_some_and(|key| key.entry == self.entry && key.elem == HEADER_ELEM);
        self.summary_lift.step(self.summary_marked(header_focused), t.dt());
        if (self.scroll.pos - self.scroll_target).abs() > 0.25 || self.scroll.vel.abs() > 0.5 {
            fx.note(PresentEvent::Motion);
        }
        let focused = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| self.item_index(collection, key.elem))
            .and_then(|index| collection.items.get(index));
        let target = PageGround::page_target(focused.or_else(|| collection.items.first()));
        if self.ground_seeded { self.ground.key_target(target, t.dt()); }
        else { self.ground.jump_target(target); self.ground_seeded = true; }
    }

    pub(crate) fn focused_item<'a, H: CollectionLike>(&self,
        focus: Option<crate::ui::machine::FocusKey<u32>>, cx: &Cx<'a, H>) -> Option<&'a PmsMovie> {
        let collection = self.collection(cx)?;
        let index = focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.item_index(collection, key.elem))?;
        collection.items.get(index)
    }

    pub(crate) fn focused_rect<H: ContentLike + CollectionLike>(&self,
        focus: Option<crate::ui::machine::FocusKey<u32>>, cx: &Cx<'_, H>, at: At) -> Option<Rect> {
        let key = focus.filter(|key| key.entry == self.entry)?;
        Focusable::<H>::place(self, &key.elem, cx, at).map(|placed| placed.rect)
    }

    pub(crate) fn redraw_focused<H: ContentLike + CollectionLike>(&self,
        f: &mut DrawFrame<'_, '_, H>, focus: Option<crate::ui::machine::FocusKey<u32>>) {
        let Some(collection) = self.collection(f.cx) else { return };
        let Some(index) = focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.item_index(collection, key.elem)) else { return };
        let Some(item) = collection.items.get(index) else { return };
        let label = self.label_at(collection, index, item);
        self.draw_card(f.painter.alpha(f.page_alpha), item, &label, index, true, f.press.scale, f.measure);
    }

    /// The header's focus rect — its text column — scrolled with the document: the header is the
    /// top of one page with the grid, not a band pinned over it.
    fn header_rect(&self) -> Rect {
        Rect::new(COL_X, HEADER_TOP - self.scroll.pos, TEXT_W, ART_H)
    }

    /// The collection's art, scrolled with the document.
    fn art_rect(&self) -> Rect {
        Rect::new(MARGIN_X, HEADER_TOP - self.scroll.pos, ART_W, ART_H)
    }

    fn draw_header(&self, p: Painter, collection: &Collection, focused: bool,
        measure: &dyn crate::ui::machine::Measure) {
        let dy = -self.scroll.pos;
        let alpha = head_alpha(self.scroll.pos);
        if alpha <= 0.0 { return; }
        let p = p.alpha(alpha);
        let art = self.art_rect();
        let name = if collection.title.is_empty() { &collection.id.name } else { &collection.title };
        if collection.header_ready() && collection.thumb.is_empty() {
            // No artwork of its own (an empty collection has no composite either): the neutral
            // tile the Library grid draws for the same collection — its mark and its name.
            crate::ui::collection_tile::draw(p, art, art, theme::CARD_RING_RAD, name);
        } else {
            widgets::card_named(p, art, Art::Thumb { sid: collection.id.sid, key: &collection.thumb, res: ART_RES },
                Some(name), theme::CARD_RING_RAD, false, 1.0, 0.0);
        }
        if collection.status == CollectionStatus::Ready {
            card_row::draw_heading(p, crate::i18n::msg::browse_collection_items(), order_note(collection),
                MARGIN_X, ITEMS_HEADING_Y + dy, SCR_W - 2.0 * MARGIN_X, measure);
        }
        let title = measure.fit_line(name, TEXT_W, theme::size::DISPLAY, true);
        Label::new(title.as_ptr(), theme::size::DISPLAY, theme::TEXT_PRIMARY).bold()
            .v(VAlign::CapTop).draw(p, Rect::new(COL_X, HEADER_TOP + dy, TEXT_W, 0.0));
        let (meta_y, summary_y) = Self::header_ys(measure);
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
        crate::ui::text_lift::draw_focused(p, plate, widgets::TEXT_BLOCK_HL_RAD, &self.summary_lift,
            crate::ui::text_lift::CENTRE, |p| {
                view.draw(p, Rect::new(COL_X, summary_y, TEXT_W, h));
                if self.summary_more {
                    view.draw_more(p, COL_X, summary_y, TEXT_W, h, self.summary_marked(focused));
                }
            });
    }

    /// Card `index`'s persistent label: the one [`Self::sync`] built, or — for a landing the page
    /// has not synced yet — formatted now.
    fn label_at<'a>(&'a self, collection: &Collection, index: usize, item: &PmsMovie) -> std::borrow::Cow<'a, str> {
        match self.labels.get(index).filter(|_| self.indexed(collection)) {
            Some(label) => std::borrow::Cow::Borrowed(label),
            None => std::borrow::Cow::Owned(member_label(item)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_card(&self, p: Painter, item: &PmsMovie, persistent: &str, index: usize, focused: bool,
        press: f32, measure: &dyn crate::ui::machine::Measure) {
        let rect = self.card_rect(index, focused, press);
        let scale = rect.w / CARD_W;
        if !card_row::paint_visible(p, rect, scale, focused) { return; }
        let resume = item.resume_frac();
        if focused {
            let row = index / crate::ui::poster_grid::COLS;
            let open = self.bands.geometry().iter().find(|band| band.row == row).map_or(0.0, |band| band.expansion);
            let label = member_caption(item).revealed(card_row::band_reveal(open));
            card_row::draw_focused(p, Art::Poster(Some(item)), rect, scale,
                &crate::ui::poster_grid::STYLE, resume, &label, measure);
        } else {
            card_row::draw_tile(p, Art::Poster(Some(item)), rect, scale,
                &crate::ui::poster_grid::STYLE, resume);
        }
        if !persistent.is_empty() {
            widgets::poster_label(p, rect,
                crate::ui::poster_grid::STYLE.tile_radius(rect, scale), persistent, measure);
            if let Some(frac) = resume {
                card_row::resume_bar(p, rect, frac,
                    crate::ui::poster_grid::STYLE.tile_radius(rect, scale));
            }
        }
    }

    /// Whether member `index`'s row rests wholly above the content edge — the row over the one a
    /// snapped scroll put on the edge (C2), which would otherwise show its last few pixels there.
    fn above_edge(&self, index: usize) -> bool {
        self.cell(index).y + CARD_H <= CONTENT_TOP - (crate::ui::poster_grid::ROW_PITCH - CARD_H) + 0.5
    }

    fn draw_grid<H: ContentLike + CollectionLike>(&self, f: &mut DrawFrame<'_, '_, H>, collection: &Collection) {
        let focus = f.focus.current.filter(|key| key.entry == self.entry);
        let current = focus.and_then(|key| self.item_index(collection, key.elem));
        let p = f.painter.alpha(f.page_alpha);
        for index in crate::ui::poster_grid::visible(collection.items.len(), GRID_TOP, self.scroll.pos) {
            if current == Some(index) || self.above_edge(index) { continue; }
            if let Some(item) = collection.items.get(index) {
                self.draw_card(p, item, &self.label_at(collection, index, item), index, false, 1.0, f.measure);
            }
        }
        if let Some(index) = current {
            if let Some(item) = collection.items.get(index) {
                self.draw_card(p, item, &self.label_at(collection, index, item), index, true, f.press.scale, f.measure);
            }
        }
        let visible = if f.records_stops() { crate::ui::poster_grid::visible(collection.items.len(), GRID_TOP, self.scroll.pos) } else { 0..0 };
        for index in visible {
            let Some(elem) = self.elem_at(collection, index) else { continue };
            let focused = current == Some(index);
            let rect = self.card_rect(index, focused, if focused { f.press.scale } else { 1.0 });
            f.stop(f.painter, Stop { key: crate::ui::machine::FocusKey { entry: self.entry, elem },
                rect, rest_rect: self.cell(index),
                clip: Rect::FULL, hover: Hover::Focus, activate: Activate::Press });
        }
    }
}

impl<H: ContentLike + CollectionLike> Focusable<H> for CollectionScreen {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let Some(collection) = self.collection(cx) else { return };
        if !collection.items.is_empty() {
            out.push(GroupSpec { id: GRID_GROUP,
                kind: GroupKind::Grid { cols: crate::ui::poster_grid::COLS, holes: &[] },
                seat: Seat::Remembered, reachable: AxisMask::BOTH,
                edge: [EdgeRule::Geometric; 4],
                extent: Rect::new(MARGIN_X, GRID_TOP - self.scroll.pos,
                    SCR_W - 2.0 * MARGIN_X, CARD_H), len: collection.items.len(), elem: ElemKind::Card });
        }
        if self.summary_more {
            out.push(GroupSpec { id: HEADER_GROUP, kind: GroupKind::Free, seat: Seat::First,
                reachable: AxisMask::VERTICAL, edge: [EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
                extent: self.header_rect(), len: 1, elem: ElemKind::Bare });
        }
        if collection.status == CollectionStatus::Failed {
            let rect = Self::status_overlay(Some(collection), cx.tick.ms, cx.measure).action_frame_measured(cx.measure)
                .unwrap_or(Self::status_frame());
            out.push(GroupSpec { id: STATUS_GROUP, kind: GroupKind::Free, seat: Seat::First,
                reachable: AxisMask::BOTH, edge: [EdgeRule::Stop; 4], extent: rect, len: 1, elem: ElemKind::Bare });
        }
    }

    fn group_of(&self, key: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        match self.locate(self.collection(cx)?, *key)? {
            Located::Header => Some(HEADER_GROUP), Located::Retry => Some(STATUS_GROUP),
            Located::Card(_) => Some(GRID_GROUP),
        }
    }

    fn neighbour(&self, key: crate::ui::machine::FocusKey<u32>, dir: Dir, cx: &Cx<'_, H>) -> Step<u32> {
        let Some(collection) = self.collection(cx) else { return Step::Edge };
        let Some(index) = self.item_index(collection, key.elem) else { return Step::Edge };
        let next = crate::ui::poster_grid::neighbour(index, collection.items.len(),
            crate::ui::poster_grid::COLS, dir);
        next.map(|index| Step::Move(self.key_at(collection, index))).unwrap_or(Step::Edge)
    }

    fn place(&self, key: &u32, cx: &Cx<'_, H>, _at: At) -> Option<Placed> {
        let collection = self.collection(cx)?;
        match self.locate(collection, *key)? {
            Located::Header => {
                let rect = self.header_rect();
                Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
            }
            Located::Retry => {
                let rect = Self::status_overlay(Some(collection), cx.tick.ms, cx.measure).action_frame_measured(cx.measure)?;
                Some(Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some(0) })
            }
            Located::Card(index) => {
                let rect = self.cell(index);
                Some(Placed { rect: rect.scaled(crate::ui::poster_grid::STYLE.focus_scale),
                    rest_rect: rect, clip: Rect::FULL, index: Some(index as u32) })
            }
        }
    }

    fn reconcile(&self, want: crate::ui::machine::FocusKey<u32>, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        let Some(collection) = self.collection(cx) else {
            return if self.return_pending && self.cards.get(want.elem).is_some() { want }
                else { crate::ui::machine::FocusKey { entry: self.entry, elem: HEADER_ELEM } };
        };
        if self.locate(collection, want.elem).is_some() { return crate::ui::machine::FocusKey { entry: self.entry, elem: want.elem }; }
        if self.awaiting_restore(collection, want.elem) { return want; }
        if !collection.items.is_empty() { return self.key_at(collection, 0); }
        if collection.status == CollectionStatus::Failed {
            return crate::ui::machine::FocusKey { entry: self.entry, elem: RETRY_ELEM };
        }
        crate::ui::machine::FocusKey { entry: self.entry, elem: HEADER_ELEM }
    }

    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> crate::ui::machine::FocusKey<u32> {
        let Some(collection) = self.collection(cx) else { return crate::ui::machine::FocusKey { entry: self.entry, elem: HEADER_ELEM } };
        if group == HEADER_GROUP { return crate::ui::machine::FocusKey { entry: self.entry, elem: HEADER_ELEM }; }
        if group == STATUS_GROUP { return crate::ui::machine::FocusKey { entry: self.entry, elem: RETRY_ELEM }; }
        let col = (0..crate::ui::poster_grid::COLS).min_by(|&a, &b| {
            let d = |c: usize| (self.cell(c).cx() - from.rect.cx()).abs();
            d(a).total_cmp(&d(b))
        }).unwrap_or(0);
        self.key_at(collection, col.min(collection.items.len().saturating_sub(1)))
    }
}

impl<H: ContentLike + CollectionLike> Machine<H> for CollectionScreen {
    type Ev = ScreenEvent<H>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled {
        match ev {
            ScreenEvent::RestoreMemory(PageMemory::Collection(memory)) => {
                self.restore(memory); self.return_pending = true; Handled::Yes
            }
            ScreenEvent::Tick(t) => { self.tick(*t, cx, fx); Handled::Yes }
            ScreenEvent::Enter(_) | ScreenEvent::Uncover => {
                self.request_store(self.entry_want(), fx); fx.invalidate(Provenance::Nav); Handled::Yes
            }
            ScreenEvent::FocusMoved { to, by, .. } => {
                if matches!(by, By::Dir | By::Pointer) { self.return_pending = false; }
                if let Some(collection) = self.collection(cx) {
                    let row = self.item_index(collection, to.elem).map(|index| index / crate::ui::poster_grid::COLS);
                    self.bands.focus(row, matches!(by, By::Dir | By::Pointer));
                    if let Some(index) = self.item_index(collection, to.elem) { self.maybe_page(collection, index, fx); }
                    if to.elem == HEADER_ELEM && matches!(by, By::Dir | By::Pointer) { self.header_marked = true; }
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
                if let Some(item) = self.focused_item(cx.focus.current, cx) {
                    fx.push(crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Push(
                        ContentArg::Detail { sid: item.sid, rk: item.rk.clone() }))));
                }
                Handled::Yes
            }
            ScreenEvent::PressHold(_) => {
                if self.focused_item(cx.focus.current, cx).is_some() {
                    fx.push(crate::ui::machine::Fx::App(AppFx::Content(ContentReq::ItemMenu)));
                    Handled::Yes
                } else { Handled::No }
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key: Key::Back, edge: Edge::Down, .. }, .. }) => {
                fx.push(crate::ui::machine::Fx::App(AppFx::Content(ContentReq::Back))); Handled::Yes
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge: Edge::Down, .. }, .. })
                if matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) => {
                    self.return_pending = false; Handled::No
                }
            ScreenEvent::Input(InputEvent { kind: InputKind::Click { .. }, .. }) => {
                self.return_pending = false; Handled::No
            }
            ScreenEvent::StoreChanged(ord, _) if *ord == crate::stores::StoreId::Collection.ord() => {
                if let Some(collection) = self.collection(cx) { self.sync(collection, cx.measure); }
                if self.return_pending {
                    let settled = cx.focus.current.filter(|key| key.entry == self.entry)
                        .is_some_and(|key| self.collection(cx).is_some_and(|c| !self.awaiting_restore(c, key.elem)));
                    if settled { self.return_pending = false; }
                }
                Handled::Yes
            }
            ScreenEvent::WillLeave(Leave::ForGood) | ScreenEvent::Unmount => {
                if self.collection(cx).is_some() && !self.teardown_closed {
                    self.teardown_closed = true;
                    fx.push(crate::ui::machine::Fx::App(AppFx::Store(crate::stores::StoreId::Collection,
                        crate::stores::StoreCmd::Collection(CollectionCmd::Close))));
                }
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl<H: ContentLike + CollectionLike> Screen<H> for CollectionScreen {
    fn redraw_focused(&self, f: &mut DrawFrame<'_, '_, H>, focus: Option<crate::ui::machine::FocusKey<u32>>) {
        CollectionScreen::redraw_focused::<H>(self, f, focus)
    }
    fn name(&self) -> &'static str { super::registry::word::COLLECTION }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _cx: &Cx<'_, H>) -> Option<std::borrow::Cow<'_, str>> { None }
    fn prepare(&mut self, _budget: &mut crate::ui::frame::Budget, _cx: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>) {
        let p = f.painter.alpha(f.page_alpha);
        self.ground.draw(p, Rect::FULL);
        let collection = self.collection(f.cx);
        if let Some(collection) = collection {
            if Self::shows_head(collection) {
                self.draw_header(p, collection, f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == HEADER_ELEM), f.measure);
            }
            if self.summary_more && Self::shows_head(collection) {
                let rect = self.header_rect();
                f.stop(f.painter, Stop { key: crate::ui::machine::FocusKey { entry: self.entry, elem: HEADER_ELEM },
                    rect, rest_rect: rect, clip: Rect::FULL, hover: Hover::Focus,
                    activate: Activate::Direct });
            }
            if collection.status == CollectionStatus::Ready { self.draw_grid(f, collection); }
            else {
                let overlay = Self::status_overlay(Some(collection), f.cx.tick.ms, f.measure)
                    .focused(f.focus.current.is_some_and(|key| key.entry == self.entry && key.elem == RETRY_ELEM));
                overlay.draw_measured(&Env::inert(), p, f.measure);
                if let Some(rect) = overlay.action_frame_measured(f.measure) {
                    f.stop(f.painter, Stop { key: crate::ui::machine::FocusKey { entry: self.entry, elem: RETRY_ELEM },
                        rect, rest_rect: rect, clip: Rect::FULL, hover: Hover::Focus, activate: Activate::Direct });
                }
            }
        } else {
            Self::status_overlay(None, f.cx.tick.ms, f.measure).draw_measured(&Env::inert(), p, f.measure);
        }
    }
    fn render(&self) -> RenderStrategy { RenderStrategy::Page }
    fn focus_source(&self) -> FocusSource { FocusSource::Engine }
    fn hit_source(&self) -> HitSource { HitSource::Engine }
    fn links(&self, out: &mut Vec<Link>) { out.extend(self.links_c.iter().copied()); }
    fn memory_at(&self, _focus: Option<crate::ui::machine::FocusKey<u32>>) -> PageMemory {
        PageMemory::Collection(self.memory())
    }
    fn as_any(&self) -> Option<&dyn std::any::Any> { Some(self) }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> { Some(self) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::FixtureMeasure;
    use crate::ui::machine::{FocusRead, Host, InputOwner, PressRead};

    struct CollectionHost;
    impl Host for CollectionHost {
        type Arg = super::super::family::SettingsPage;
        type Fx = AppFx;
        type Msg = super::super::registry::AppMsg;
        type Elem = u32;
        type Views<'a> = crate::collection::CollectionView<'a>;
        type Init = super::super::family::NoInit;
        type Memory = PageMemory;
    }
    impl CollectionLike for CollectionHost {
        fn collection<'a>(cx: &Cx<'a, Self>) -> crate::collection::CollectionView<'a> { cx.views }
    }

    fn item(rk: &str) -> PmsMovie { PmsMovie { rk: rk.into(), title: rk.into(), ..Default::default() } }
    fn set() -> CollectionRef {
        CollectionRef { sid: crate::plex::ServerId::UNSET, rk: "50001".into(), sec: 1, tag: 7, name: "Set".into() }
    }
    fn seeded() -> (crate::stores::collection::CollectionStore, CollectionScreen) {
        let mut store = crate::stores::collection::CollectionStore::default();
        store.run(CollectionCmd::Open { target: CollectionTarget { id: set(), want: PAGE_SIZE } });
        store.install_for_test(vec![item("a"), item("b"), item("c")], CollectionStatus::Ready);
        let mut screen = CollectionScreen::new(EntryId(9), set());
        screen.sync(store.view().current().unwrap(), &FixtureMeasure);
        (store, screen)
    }
    fn cx<'a>(view: crate::collection::CollectionView<'a>, focus: Option<crate::ui::machine::FocusKey<u32>>) -> Cx<'a, CollectionHost> {
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

    fn step(screen: &mut CollectionScreen, ev: ScreenEvent<CollectionHost>,
        cx: &Cx<'_, CollectionHost>) -> Vec<crate::ui::machine::Stamped<CollectionHost>> {
        let mut present = crate::ui::present::Present::new();
        let mut out = Vec::new();
        let mut fx = Effects::new(&mut out,
            crate::ui::machine::MachineId::Instance(crate::ui::machine::InstanceId(9)), &mut present);
        Machine::<CollectionHost>::step(screen, &ev, cx, &mut fx);
        drop(fx);
        out
    }

    fn opens_summary(out: &[crate::ui::machine::Stamped<CollectionHost>]) -> bool {
        out.iter().any(|e| matches!(e.fx, crate::ui::machine::Fx::App(AppFx::Content(
            ContentReq::Panel(ContentPanel::CollectionAbout)))))
    }

    #[test]
    fn header_ok_opens_the_full_summary_exactly_when_more_is_drawn() {
        let (mut store, mut screen) = seeded();
        let short = step(&mut screen, ScreenEvent::Activate(HEADER_ELEM), &cx(store.view(), None));
        assert!(!screen.summary_more && !opens_summary(&short),
            "a summary that fits offers no sheet: {}", store.view().current().unwrap().summary);

        store.edit_for_test(|c| c.summary = "A long collection summary that runs on. ".repeat(40));
        screen.sync(store.view().current().unwrap(), &FixtureMeasure);
        assert!(screen.summary_more, "the MORE mark is drawn for a truncated summary");
        let out = step(&mut screen, ScreenEvent::Activate(HEADER_ELEM), &cx(store.view(), None));
        assert!(opens_summary(&out), "OK on the header opens the summary sheet");
        assert!(screen.header_marked);
    }

    #[test]
    fn focus_near_the_end_asks_the_store_for_the_next_page() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.more = true);
        let key = screen.key_at(store.view().current().unwrap(), 2);
        let out = step(&mut screen, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir },
            &cx(store.view(), Some(key)));
        let want = out.iter().find_map(|e| match &e.fx {
            crate::ui::machine::Fx::App(AppFx::Store(_, crate::stores::StoreCmd::Collection(
                CollectionCmd::Open { target, .. }))) => Some(target.want),
            _ => None,
        });
        assert_eq!(want, Some(3 + PAGE_SIZE), "the next page is requested ahead of the last row");

        store.edit_for_test(|c| c.more = false);
        let out = step(&mut screen, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir },
            &cx(store.view(), Some(key)));
        assert!(!out.iter().any(|e| matches!(e.fx, crate::ui::machine::Fx::App(AppFx::Store(..)))),
            "a fully loaded collection asks for nothing");
    }

    #[test]
    fn a_member_ok_pushes_its_detail_page() {
        let (store, mut screen) = seeded();
        let key = screen.key_at(store.view().current().unwrap(), 1);
        let out = step(&mut screen, ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &cx(store.view(), Some(key)));
        assert!(out.iter().any(|e| matches!(&e.fx, crate::ui::machine::Fx::App(AppFx::Content(
            ContentReq::Push(ContentArg::Detail { rk, .. }))) if rk == "b")));
    }

    /// The failed page's read-out is the shared page read-out (#268): page-placed, the untyped
    /// "can't reach" glyph Home and the Library carry, and never drawn over the live header —
    /// with a three-line summary the glyph shrinks or drops, without one it keeps its full size.
    /// The quiet Empty answer stays in the grid's region and carries no glyph.
    #[test]
    fn a_failed_page_reads_out_with_the_shared_glyph_below_the_header() {
        let (mut store, _) = seeded();
        store.edit_for_test(|c| { c.items.clear(); c.summary.clear(); c.status = CollectionStatus::Failed; });
        let c = store.view().current().unwrap();
        let overlay = CollectionScreen::status_overlay(Some(c), 0, &FixtureMeasure);
        assert_eq!(overlay.glyph, Some(crate::ui::icons::Icon::ServerBadgeMinus));
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
        store.edit_for_test(|c| { c.items.clear(); c.child_count = 0; c.total = 0; c.status = CollectionStatus::Empty; });
        let c = store.view().current().unwrap();
        assert_eq!(meta_key(c), (0, true), "the meta line is the kind alone");
        assert!(CollectionScreen::shows_head(c));
        let overlay = CollectionScreen::status_overlay(Some(c), 0, &FixtureMeasure);
        let f = overlay.frame;
        assert_eq!((f.x, f.y, f.w, f.h), (96.0, 460.0, 1728.0, 475.0));
        assert_eq!(overlay.caption, crate::i18n::msg::browse_collection_empty_c());
    }

    /// C4b: an unavailable collection is the page-filling Failed verdict at the shared anchor
    /// with its reason and no way on but BACK — and no header, since there is nothing to head.
    #[test]
    fn an_unavailable_collection_fills_the_page_with_its_verdict_and_no_header() {
        let (mut store, _) = seeded();
        store.edit_for_test(|c| { c.items.clear(); c.status = CollectionStatus::Unavailable; });
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
        screen.sync(store.view().current().unwrap(), &FixtureMeasure);
        let art = screen.art_rect();
        assert_eq!((art.x, art.y, art.w, art.h), (96.0, 96.0, 200.0, 300.0));
        let column = screen.header_rect();
        assert_eq!((column.x, column.y, column.w), (360.0, 96.0, 1344.0));
        assert_eq!(screen.cell(0).y, 520.0);
        assert_eq!(screen.cell(0).x, MARGIN_X);
        assert!(ITEMS_HEADING_Y + theme::size::HEADLINE as f32 <= GRID_TOP);
    }

    /// C2: scrolling to a lower row rests the first visible row on the content edge, with the
    /// header gone and nothing of the row above it left showing.
    #[test]
    fn a_scrolled_page_rests_a_row_on_the_content_edge() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.items = (0..30).map(|i| item(&format!("m{i}"))).collect());
        screen.sync(store.view().current().unwrap(), &FixtureMeasure);
        let cols = crate::ui::poster_grid::COLS;
        let scroll = crate::ui::poster_grid::snap_row(0.0, 2, 30, GRID_TOP, CONTENT_TOP);
        assert!(scroll > 0.0);
        screen.scroll.pos = scroll;
        let rows: Vec<f32> = (0..30).step_by(cols).filter(|&i| !screen.above_edge(i))
            .map(|i| screen.cell(i).y).collect();
        assert!((rows[0] - CONTENT_TOP).abs() < 0.5, "first drawn row at {}", rows[0]);
        assert_eq!(head_alpha(scroll), 0.0, "no remnant of the header");
        assert_eq!(head_alpha(0.0), 1.0);
    }

    /// Down from above a short last row lands on its last member (the Library grid's rule)
    /// rather than stopping where no card sits directly below.
    #[test]
    fn down_onto_a_short_last_row_lands_on_its_last_member() {
        let (mut store, mut screen) = seeded();
        store.edit_for_test(|c| c.items = (0..8).map(|i| item(&format!("m{i}"))).collect());
        screen.sync(store.view().current().unwrap(), &FixtureMeasure);
        let c = store.view().current().unwrap();
        let from = screen.key_at(c, 4);
        let step = Focusable::<CollectionHost>::neighbour(&screen, from, Dir::Down, &cx(store.view(), Some(from)));
        assert!(matches!(step, Step::Move(key) if key == screen.key_at(c, 7)));
        let from = screen.key_at(c, 7);
        assert!(matches!(Focusable::<CollectionHost>::neighbour(&screen, from, Dir::Down,
            &cx(store.view(), Some(from))), Step::Edge), "the last row has nothing below");
    }

    /// Back onto a page rebuilt from its first page holds the remembered member while the pages
    /// above it load, and the entry asks for every member the page knew.
    #[test]
    fn a_restore_past_the_first_page_waits_for_its_member() {
        let (mut store, original) = seeded();
        let many: Vec<PmsMovie> = (0..PAGE_SIZE + 10).map(|i| item(&format!("m{i}"))).collect();
        store.edit_for_test(|c| c.items = many.clone());
        let mut original = original;
        original.sync(store.view().current().unwrap(), &FixtureMeasure);
        let focus = original.key_at(store.view().current().unwrap(), PAGE_SIZE + 5);
        let PageMemory::Collection(memory) = Screen::<CollectionHost>::memory_at(&original, Some(focus)) else { panic!() };

        store.edit_for_test(|c| { c.items = many[..PAGE_SIZE].to_vec(); c.more = true; });
        let mut restored = CollectionScreen::new(EntryId(9), set());
        let _ = step(&mut restored, ScreenEvent::RestoreMemory(PageMemory::Collection(memory)), &cx(store.view(), None));
        let out = step(&mut restored, ScreenEvent::Enter(crate::ui::screen::Enter::Restored), &cx(store.view(), None));
        let want = out.iter().find_map(|e| match &e.fx {
            crate::ui::machine::Fx::App(AppFx::Store(_, crate::stores::StoreCmd::Collection(
                CollectionCmd::Open { target, .. }))) => Some(target.want),
            _ => None,
        });
        assert!(want.is_some_and(|w| w > PAGE_SIZE + 5), "the entry asks for the remembered member's page: {want:?}");
        restored.sync(store.view().current().unwrap(), &FixtureMeasure);
        let got = Focusable::<CollectionHost>::reconcile(&restored, focus, &cx(store.view(), Some(focus)));
        assert_eq!(got, focus, "focus waits on the member rather than falling to the first card");

        store.edit_for_test(|c| { c.items = many.clone(); c.more = false; });
        restored.sync(store.view().current().unwrap(), &FixtureMeasure);
        let got = Focusable::<CollectionHost>::reconcile(&restored, focus, &cx(store.view(), Some(focus)));
        assert_eq!(restored.focused_item(Some(got), &cx(store.view(), Some(got))).unwrap().rk,
            format!("m{}", PAGE_SIZE + 5));
    }

    #[test]
    fn page_memory_restores_the_same_member_after_detail() {
        let (store, original) = seeded();
        let focus = original.key_at(store.view().current().unwrap(), 1);
        let PageMemory::Collection(memory) = Screen::<CollectionHost>::memory_at(&original, Some(focus)) else { panic!() };
        let mut restored = CollectionScreen::new(EntryId(9), set());
        restored.restore(&memory);
        restored.return_pending = true;
        restored.sync(store.view().current().unwrap(), &FixtureMeasure);
        let got = Focusable::<CollectionHost>::reconcile(&restored, focus, &cx(store.view(), Some(focus)));
        assert_eq!(got, focus);
        assert_eq!(restored.focused_item(Some(got), &cx(store.view(), Some(got))).unwrap().rk, "b");
    }

    /// **The collection page's fixed slots fit in every shipped language**: the season mark on a
    /// grid poster (`widgets::poster_label` elides to the card less its insets) and the one-line
    /// meta line beside the artwork. Measured with the device's whole-pixel advances.
    #[test]
    fn the_collection_pages_fixed_slots_fit_in_every_language() {
        use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
        use crate::i18n::{language_on_this_thread_for_test, msg, Preference};
        use crate::ui::machine::Measure;
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
                msg::browse_collection_meta(&crate::ui::fmt::item_count(99_999))] {
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
        use crate::ui::screen::DrawFrame;
        let _serial = crate::testlock::serial();
        let _pseudo = crate::i18n::pseudo_on_this_thread_for_test();
        let server = ["Set", "Qwerty", "Zzyzx", "Vlox"];
        let season = PmsMovie { rk: "s".into(), kind: 2, season_index: 3, title: "Zzyzx".into(),
            show_title: "Vlox".into(), ..Default::default() };
        let mut stray = Vec::new();
        for status in [CollectionStatus::Ready, CollectionStatus::Loading, CollectionStatus::Empty,
            CollectionStatus::Unavailable, CollectionStatus::Failed] {
            let (mut store, mut screen) = seeded();
            store.edit_for_test(|c| {
                c.summary = "Qwerty".into();
                c.items.push(season.clone());
                c.child_count = c.items.len();
                c.status = status;
                if status != CollectionStatus::Ready { c.items.clear(); c.child_count = 0; }
            });
            screen.sync(store.view().current().unwrap(), &FixtureMeasure);
            let context = cx(store.view(), None);
            let runs = crate::text::capture_text_runs_for_test(|| {
                let mut f = DrawFrame::new(&context, crate::ui::Painter::recording());
                crate::gfx::without_frame_clear(|| Screen::<CollectionHost>::draw(&mut screen, &mut f));
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
