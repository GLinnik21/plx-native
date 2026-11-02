//! **PageHeader** — the one row of plain-text controls a library page wears under the top bar:
//! the library names on the left (with a "+N" for the ones that do not fit) or a breadcrumb
//! ("Films › Collections"), and the page's own controls on the right ("Collections · 12",
//! "Recommended", "≡ Date added ⌄"; the Library's own cluster is the board's: "Collections
//! · 24 ›  Recommended ›  |  ≡ Date added ⌄", places with a chevron ([`HeaderItem::place`]), a
//! [`HeaderItem::divider`] and a sort control ([`HeaderItem::sort_control`]) that is a leading
//! [`HeaderItem::glyph`], its criterion alone ([`HeaderItem::value_only`]: the name "Sort & Filter"
//! is carried, never drawn) and a quiet down chevron ([`HeaderItem::fold_quiet`]); the criterion has
//! NO slot, so the control ends at [`TEXT_X1`] and the places slide as its width changes). The
//! Library, its Collections and Recommended pages and the Watchlist all wear THIS row and the same
//! sort control: one layout, one draw, one focus group ([`HeaderFocus`]); every new item field
//! defaults to the look the other pages have.
//!
//! Geometry (spec): the text edges are x 96 and x 1771; a selected plate overhangs the text by
//! [`OVERHANG`] (20), so the container is left 76, right 129, and a focus pill stands a constant
//! [`PILL_FOCUS_OUTSET`](super::widgets::PILL_FOCUS_OUTSET) (2) past its plate, whatever its label (never a scale of the label's width). The row is
//! [`HEIGHT`] (56) tall at [`TOP`] (148). The right cluster's BUDGET is sized ONCE from its widest
//! case (`widest`), so the room the left group may use never changes when a value changes; the
//! items inside the cluster are placed by what they DRAW, one [`GAP`] apart (the filter
//! read-out too, so it moves with the live cluster's width), right-anchored at [`TEXT_X1`]. Both groups are [`HStack`]s ([`crate::hstack`]):
//! the stack places the left names and the cluster, and `width` / `budget_w` are the sums the packing
//! and the budget read, so no `x += w + GAP` of the row's own can disagree with them.
//!
//! **A breadcrumb is a configuration of the row, not a second widget.** With
//! [`PageHeaderSpec::crumb`] set, the left side is the root link (focusable, no plate of its own),
//! the `›` glyph and the page's name on the selected plate (not focusable); the placement is
//! [`Crumb::layout`]'s, so the pieces stand where the mock has them.
//!
//! **A page's own name is plain text** ([`HeaderItem::title`], the Watchlist's "Watchlist"): full white
//! on the bare ground, no plate and no stop. The selected plate means "current among siblings"
//! (a library name, "Collections", a crumb's page) and the white pill means "focusable"; a lone
//! title is neither (design review S10).
//!
//! **Motion is the top bar's own component.** The row paints its plates through a
//! [`TabStrip`] and its focus pop through a [`CtlPop`], both owned by the SCREEN that wears the
//! row (never by the row: a library switch replaces the whole header, and a capsule that lived in
//! it would land afresh instead of travelling). [`PageHeader::step`] steps them (the focus capsule
//! travels on the bar's spring; a changed selection's plate travels with it, [`SelMark::Travels`]) and
//! [`PageHeader::draw`] paints plates first, then each label inked by what is under it, pixel by
//! pixel: inside a capsule's drawn rounded shape in that capsule's ink and elsewhere in the resting
//! one ([`TabStrip::pill_ink`]).

use std::ffi::CString;

use plx_machine::machine::{EntryId, FocusKey, GroupId, Measure};

use super::breadcrumb::{self, Crumb};
use super::hstack::{Align, Flow, HStack, Item};
use super::page_regions::row_neighbour;
use super::screen::{At, AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step};
use super::widgets::{focus_pill_popped, paint_inked, CtlPop, PillInk, SelMark, TabGround, TabStrip};
use super::xfade::Xfade;
use super::{icons::{self, Icon}, theme, Painter, Rect, Spring};

/// Left text edge of the first library name.
pub const TEXT_X0: f32 = 96.0;
/// Right text edge of the last right-cluster item.
pub const TEXT_X1: f32 = 1771.0;
/// How far a plate stands out past its text on each side (a focus pill: that and
/// [`PILL_FOCUS_OUTSET`](super::widgets::PILL_FOCUS_OUTSET)).
pub const OVERHANG: f32 = 20.0;
/// Top of the header row, and its height.
pub const TOP: f32 = 148.0;
pub const HEIGHT: f32 = 56.0;
/// The header block: the row and the air under it. The first row of posters under a header stands
/// this far below the row's top, at [`GRID_TOP`].
pub const BLOCK_H: f32 = 96.0;
/// Where the first grid row's posters stand under a header at rest (244): the line a focused row
/// of a scrolled grid comes back to (`cards::GridSpec::resting`, the Library's `Layout::row_reveal`),
/// well clear of the scroll capsule (`scroll_capsule::Y + H`, 118).
pub const GRID_TOP: f32 = TOP + BLOCK_H;
/// **The text-edge gap between two adjacent items**, whatever their names: the first one's plate
/// overhang, [`theme::space::XS`] between the two plates, then the next item's plate and its
/// overhang, 48 px text to text. The Design System's `PageHeader` is the same sum (`.row{gap:8px}`
/// over items padded 20 px each side), and the owner-approved boards stand at it (the Library's
/// names 30 px past the plate, its places 52 px ink to ink). The focused pill stands
/// [`PILL_FOCUS_OUTSET`](super::widgets::PILL_FOCUS_OUTSET) (2) past its plate, so it still leaves `XS - 2` = 6 px before the
/// neighbour's plate (a pill and a neighbour never touch, for any widths: the S9 review's air was
/// measured from the FOCUSED pill's extent, [`focus_pill`](super::widgets::focus_pill), and a test
/// holds that edge). The outset is a constant (S4), so the gap is one number, and every placement
/// and every packing uses it.
pub const GAP: f32 = 2.0 * OVERHANG + theme::space::XS;
/// Clear space kept between the left group and the right cluster.
pub const CLUSTER_GAP: f32 = 64.0;

/// One item in the row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderItem {
    /// The label (a library name, "Collections", "Sort & Filter").
    pub label: String,
    /// A value segment drawn after the label at full white ("12", "Title").
    pub value: Option<String>,
    /// The item is the CURRENT one: it wears the selected plate.
    pub selected_plate: bool,
    /// The item is the page's own NAME, not one of several siblings (the Watchlist's "Watchlist"):
    /// **plain text** at full white on the bare ground. The plate means "current among siblings" and
    /// the white pill means "focusable"; a lone title is neither, so it wears neither (design
    /// review S10). Implies not focusable; it takes the same room a plate-wearing label would, so a
    /// row's placement never depends on which of the two it is.
    pub title: bool,
    /// A menu control: a fold glyph (down chevron) follows the text.
    pub fold_glyph: bool,
    /// Takes focus. A read-out ("312 of 1,284") does not.
    pub focusable: bool,
    /// Drawn quieter (an owner handle, an unavailable control).
    pub dimmed: bool,
    /// A read-out ("24 of 27"): [`theme::COUNT_FILTERED_INK`], no stop, one [`GAP`] before the next
    /// item like any other (it moves with the live cluster's width).
    pub readout: bool,
    /// A control whose value is a count that comes and goes ("Collections · 4"): the widest value it
    /// ever wears, measured once. The slot sizes the cluster's BUDGET (`cluster_w`, `left_budget`:
    /// the room the library names may use does not change with the count); it is no air inside the
    /// row, where the control is placed by the text it DRAWS, one [`GAP`] from its neighbour, and the
    /// plate hugs that text. Its value, and its being on the row at all, ride the row's
    /// [`HeaderSwap`]: a changed count cross-fades in place, a control the next row does not have
    /// fades out where it stood, one it did not have fades in, and one whose label stands at another
    /// x (a count of another digit count) leaves at the old x and arrives at the new, instead of
    /// being cut (a row for ANOTHER library shows only the controls that library has).
    pub value_slot: Option<String>,
    /// A mark that leads the text ([`GLYPH_SIZE`] px, [`GLYPH_GAP`] before the text), inked as the
    /// value is: the sort control's three bars.
    pub glyph: Option<Icon>,
    /// A PLACE (the control opens a page): a chevron trails the text, [`PLACE_SIZE`] px at
    /// [`theme::HEADER_PLACE_INK`], [`PLACE_GAP`] after it. "Collections · 24 ›", "Recommended ›".
    pub place: bool,
    /// `label` is the control's NAME, its identity in the row (what the row's [`HeaderSwap`] and the
    /// tests know it by), and is NOT drawn, neither on the control nor as the menu's heading: the
    /// control reads as its `value` alone. The sort control, whose visible text is the chosen
    /// criterion and never "Sort & Filter".
    pub value_only: bool,
    /// With `fold_glyph`: the board's quiet down chevron ([`QUIET_FOLD_SIZE`] px at 65%, [`QUIET_FOLD_GAP`]
    /// after the text) in place of the menu fold ([`FOLD_SIZE`] px at full ink, [`FOLD_GAP`]).
    pub fold_quiet: bool,
    /// A hairline between two controls ([`DIVIDER_W`] x [`DIVIDER_H`], [`theme::HEADER_DIVIDER_INK`]):
    /// no stop, no label. It stands in the gap before the next item, [`DIVIDER_MARGIN`] clear of that
    /// item's plate and [`OVERHANG`] clear of the plate before it ([`DIVIDER_TRAIL`] from that
    /// item's text); the row keeps its one [`GAP`] between the divider's two neighbours and the
    /// divider adds its own width and two [`DIVIDER_MARGIN`]s to it.
    pub divider: bool,
}

impl HeaderItem {
    /// **The sort control** (the board's "= Date added v"): the sort icon ([`Icon::Sort`]), the chosen
    /// criterion and a quiet down chevron, and nothing else. `name` is the control's identity in the
    /// row ("Sort & Filter"), which the row's [`HeaderSwap`] and the tests know it by; it is carried
    /// by the item and not drawn (neither on the control nor as the menu's heading, [`Self::value_only`]);
    /// `value` is the criterion, the only text it shows (a narrowing filter is the page's read-out,
    /// not part of it). It is a stop and has no slot: it ends at [`TEXT_X1`] and its neighbours
    /// slide as the criterion's width changes. The ONE declaration of it: the Library's row, the
    /// Collections page's Sort and the Watchlist's wear it, so one page's sort button cannot drift
    /// from another's.
    pub fn sort_control(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: name.into(), value: Some(value.into()), glyph: Some(Icon::Sort), value_only: true,
            fold_glyph: true, fold_quiet: true, focusable: true, ..Default::default()
        }
    }
}

/// What the header is built from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageHeaderSpec {
    /// The breadcrumb that replaces the left side on a page pushed from a library page
    /// ("Films › Collections"). `left` is empty and `overflow` is 0 when this is set.
    pub crumb: Option<Crumb>,
    /// The libraries, in order; `selected_plate` marks the current one. Empty on a single-library
    /// page (the row then carries only the right cluster).
    pub left: Vec<HeaderItem>,
    /// The "+N" count of libraries folded out of `left`; 0 = no "+N" item.
    pub overflow: usize,
    /// The page's controls, left to right.
    pub right: Vec<HeaderItem>,
}

/// Which part of the spec a laid-out item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Slot {
    Left(usize),
    Overflow,
    Right(usize),
    /// The breadcrumb's root link: focusable, no plate of its own.
    CrumbRoot,
    /// The breadcrumb's page name, on the selected plate: the title, not a control.
    CrumbPage,
}

/// One laid-out item: its text box (edges are the spec's x 96 / 1771), the plate / pill box that
/// stands [`OVERHANG`] out on both sides, and whether it takes focus.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ItemRect {
    pub slot: Slot,
    pub text: Rect,
    pub plate: Rect,
    pub focusable: bool,
}

/// The row's geometry. `items` is in reading (and focus) order: left names, "+N", right controls.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct HeaderLayout {
    pub items: Vec<ItemRect>,
    /// The breadcrumb's `›`, when the row has one.
    pub glyph: Option<Rect>,
    /// The right cluster's budget width (text only), measured from the widest case. The cluster
    /// DRAWN is narrower whenever a value slot goes unused.
    pub cluster_w: f32,
    /// The width the left group may fill, text edge to text edge, before it would meet the
    /// cluster: what the caller packs library names (and the "+N") into.
    pub left_budget: f32,
}

impl HeaderLayout {
    /// The y every item's plate stands at (`items[0]`'s: the row is one line). [`TOP`] for an empty
    /// row. A caller that shifts the items (a scrolled page, the single-library nudge) shifts them
    /// all, so this is the one place the capsules' `top` is read from.
    pub fn top(&self) -> f32 {
        self.items.first().map_or(TOP, |item| item.plate.y)
    }
}

pub struct PageHeader {
    spec: PageHeaderSpec,
    /// The right cluster's budget width, from [`PageHeader::new`]'s widest case.
    cluster_w: f32,
}

fn text_w(s: &str, measure: &dyn Measure) -> f32 {
    CString::new(s).map_or(0.0, |c| measure.width(&c, theme::size::BODY, false))
}

/// The separator between a label and its value ("Collections · 24").
const SEP: &str = " \u{b7} ";

/// What an item is made of, left to right, as stack items with the gap each part wears before it:
/// the leading mark, the label (not drawn when the control is [`HeaderItem::value_only`]), the
/// separator and the value, the place chevron, the fold glyph. `budgeted`: a [`HeaderItem::value_slot`]
/// sizes the value at its slot (the room the item is BUDGETED, [`run_w`]); otherwise at what it
/// draws ([`ink_w`]). The one place an item's width is made, so [`run_w`], [`ink_w`] and the paint
/// ([`PageHeader::draw_face_at`], which walks the same parts in the same order) cannot disagree.
fn parts(item: &HeaderItem, measure: &dyn Measure, budgeted: bool) -> Vec<Item> {
    if item.divider { return vec![Item::new(DIVIDER_W)]; }
    let w = |s: &str| text_w(s, measure);
    let mut parts = Vec::with_capacity(6);
    let mut lead = 0.0;
    if item.glyph.is_some() {
        parts.push(Item::new(GLYPH_SIZE));
        lead = GLYPH_GAP;
    }
    let mut text = |parts: &mut Vec<Item>, width: f32| parts.push(Item::new(width).before(std::mem::take(&mut lead)));
    if !item.value_only { text(&mut parts, w(&item.label)); }
    let value = match (&item.value_slot, &item.value) {
        (Some(slot), value) if budgeted => Some(w(slot).max(value.as_deref().map_or(0.0, w))),
        (_, Some(value)) => Some(w(value)),
        _ => None,
    };
    if let Some(value) = value {
        if !item.value_only { text(&mut parts, w(SEP)); }
        text(&mut parts, value);
    }
    if item.place { parts.push(Item::new(PLACE_SIZE).before(PLACE_GAP)); }
    if item.fold_glyph { parts.push(fold_item(item.fold_quiet)); }
    parts
}

/// The room an item is BUDGETED in the row: its drawn run, or for a control with a
/// [`HeaderItem::value_slot`] the label and the separator and the slot (the count it holds may be
/// wider than the slot, never narrower, so the budget never changes while the count is a few
/// digits). It sizes `cluster_w` and `left_budget`; the items are placed by [`ink_w`].
fn run_w(item: &HeaderItem, measure: &dyn Measure) -> f32 {
    HStack::new(&parts(item, measure, true), 0.0, Align::Leading(0.0)).width()
}

/// The fold glyph as a run item: the menu fold ([`FOLD_SIZE`] wide, [`FOLD_GAP`] after whatever
/// precedes it) or the board's quiet one. `parts` sizes the item with it and `draw_face_at` places
/// the glyph from it, so the two cannot disagree.
fn fold_item(quiet: bool) -> Item {
    if quiet { Item::new(QUIET_FOLD_SIZE).before(QUIET_FOLD_GAP) } else { Item::new(FOLD_SIZE).before(FOLD_GAP) }
}

/// The width of what an item DRAWS: [`run_w`], less the unused part of a slot. The item's text box
/// and its plate are this wide, and the next item stands one gap past it.
fn ink_w(item: &HeaderItem, measure: &dyn Measure) -> f32 {
    HStack::new(&parts(item, measure, false), 0.0, Align::Leading(0.0)).width()
}

const FOLD_GAP: f32 = 10.0;
const FOLD_SIZE: f32 = 22.0;
/// The board's down chevron on the sort control: 20 px, 12 after the criterion, at 65%.
pub const QUIET_FOLD_SIZE: f32 = 20.0;
pub const QUIET_FOLD_GAP: f32 = 12.0;
/// The mark that leads a control's text, and its air before the text (the sort icon).
pub const GLYPH_SIZE: f32 = 26.0;
pub const GLYPH_GAP: f32 = 12.0;
/// A place's chevron, and its air after the text.
pub const PLACE_SIZE: f32 = 20.0;
pub const PLACE_GAP: f32 = 8.0;
/// The divider between the places and the sort control: 2 x 32, standing [`DIVIDER_MARGIN`] (12)
/// clear of the sort control's plate AFTER it and [`OVERHANG`] (20) clear of Recommended's plate
/// BEFORE it (the board's 40 px from Recommended's text, [`DIVIDER_TRAIL`], and 32 px to the sort
/// control's text, [`DIVIDER_LEAD`]: the plate's overhang plus the margin). The row's one [`GAP`]
/// plus the divider's width and two margins is what separates the divider's two neighbours' text,
/// 74 px; the part of it before the divider is [`DIVIDER_TRAIL`], the part after it [`DIVIDER_LEAD`].
pub const DIVIDER_W: f32 = 2.0;
pub const DIVIDER_H: f32 = 32.0;
pub const DIVIDER_MARGIN: f32 = 12.0;
pub const DIVIDER_LEAD: f32 = OVERHANG + DIVIDER_MARGIN;
pub const DIVIDER_TRAIL: f32 = GAP + 2.0 * DIVIDER_MARGIN - DIVIDER_LEAD;

/// One right-cluster item as the stack sees it: it DRAWS [`ink_w`], a [`HeaderItem::value_slot`] is
/// its [`Item::slot`] (budget [`run_w`], never air). The one place a right-cluster item becomes a stack item, so
/// the cluster's budget ([`cluster_width`]) and its placement ([`PageHeader::layout`]) read one thing.
fn stack_item(item: &HeaderItem, measure: &dyn Measure) -> Item {
    let drawn = Item::new(ink_w(item, measure));
    if item.value_slot.is_some() { drawn.slot(run_w(item, measure)) } else { drawn }
}

/// The right cluster as stack items, in order. A divider takes the gap [`DIVIDER_TRAIL`] before it
/// and the item after it [`DIVIDER_LEAD`], so the row's one gap and the divider's margins are one
/// rule here and the cluster's budget and its placement cannot disagree.
fn cluster_items(items: &[HeaderItem], measure: &dyn Measure) -> Vec<Item> {
    items.iter().enumerate().map(|(i, item)| {
        let stand = stack_item(item, measure);
        if item.divider { stand.before(DIVIDER_TRAIL) }
        else if i > 0 && items[i - 1].divider { stand.before(DIVIDER_LEAD) }
        else { stand }
    }).collect()
}

/// Width of a cluster of items laid out with [`GAP`] between them, each item as wide as it is BUDGETED ([`run_w`]: a slot's full width): the stack's
/// [`HStack::budget_w`].
pub fn cluster_width(items: &[HeaderItem], measure: &dyn Measure) -> f32 {
    HStack::new(&cluster_items(items, measure), GAP, Align::Leading(0.0)).budget_w()
}

impl PageHeader {
    /// `widest` is the right cluster in its widest case (every value at its longest): it sizes the
    /// cluster's BUDGET (`cluster_w`, and so `left_budget`), whatever `spec.right` holds now. The
    /// live items are placed by what they draw.
    pub fn new(spec: PageHeaderSpec, widest: &[HeaderItem], measure: &dyn Measure) -> Self {
        let live = cluster_width(&spec.right, measure);
        let cluster_w = cluster_width(widest, measure).max(live);
        Self { spec, cluster_w }
    }

    pub fn spec(&self) -> &PageHeaderSpec { &self.spec }

    /// Place every item. `width` is the page width (1920): the right cluster ends at
    /// [`TEXT_X1`] scaled to it (a different width moves the right edge by the same amount).
    pub fn layout(&self, width: f32, measure: &dyn Measure) -> HeaderLayout {
        let x1 = TEXT_X1 + (width - 1920.0);
        let cluster_x0 = x1 - self.cluster_w;
        let mut items = Vec::new();
        let mut glyph = None;
        if let Some(crumb) = &self.spec.crumb {
            debug_assert!(self.spec.left.is_empty() && self.spec.overflow == 0, "a crumb replaces the left side");
            let placed = crumb.layout(measure);
            items.push(ItemRect { slot: Slot::CrumbRoot, text: placed.root_text, plate: placed.root_plate, focusable: true });
            items.push(ItemRect { slot: Slot::CrumbPage, text: placed.page_text, plate: placed.page_plate, focusable: false });
            glyph = Some(placed.glyph);
        }
        let mut place = |slot, x: f32, w: f32, focusable: bool| {
            let text = Rect::new(x, TOP, w, HEIGHT);
            let plate = Rect::new(x - OVERHANG, TOP, w + 2.0 * OVERHANG, HEIGHT);
            items.push(ItemRect { slot, text, plate, focusable });
        };
        // The left names (then the "+N"), leading at the text edge, one `GAP` apart.
        let mut names: Vec<Item> = self.spec.left.iter().map(|it| Item::new(run_w(it, measure))).collect();
        if self.spec.overflow > 0 {
            let more = HeaderItem { label: format!("+{}", self.spec.overflow), ..Default::default() };
            names.push(Item::new(run_w(&more, measure)));
        }
        for span in HStack::new(&names, GAP, Align::Leading(TEXT_X0)).placed() {
            match self.spec.left.get(span.i) {
                Some(it) => place(Slot::Left(span.i), span.x, span.w, it.focusable && !it.title),
                None => place(Slot::Overflow, span.x, span.w, true),
            }
        }
        // The right cluster's items sit flush to the right edge, each placed by the text it DRAWS
        // (`ink_w`, through `stack_item`) and one gap from the next: a slot is budget (`cluster_w`,
        // above), never air between two items, so every gap of the row is the same whatever a count
        // holds. The cluster is right-anchored, so the items after a control whose count changes
        // stand still; the control's own label moves by the digits' width (`HeaderSwap` dissolves
        // that). The read-out is the cluster's leading item and stands one gap before the next like any
        // other, so it moves with the live cluster's width.
        let cluster = cluster_items(&self.spec.right, measure);
        for span in HStack::new(&cluster, GAP, Align::Trailing(x1)).placed() {
            place(Slot::Right(span.i), span.x, span.w, self.spec.right[span.i].focusable);
        }
        HeaderLayout {
            items,
            glyph,
            cluster_w: self.cluster_w,
            left_budget: (cluster_x0 - CLUSTER_GAP - TEXT_X0).max(0.0),
        }
    }

    /// What `slot` says about its item: the label (a crumb's parts and the "+N" are the row's own,
    /// not spec items) and the flags the paint reads.
    fn face(&self, slot: Slot) -> Face<'_> {
        let of = Self::face_of;
        fn plain<'a>(label: String, selected: bool) -> Face<'a> {
            Face { label, value: None, selected, title: false, fold: false, dimmed: false, readout: false, look: Look::default(), fade: None }
        }
        match slot {
            Slot::Left(i) => self.spec.left.get(i).map_or_else(|| plain(String::new(), false), of),
            Slot::Right(i) => self.spec.right.get(i).map_or_else(|| plain(String::new(), false), of),
            Slot::Overflow => plain(format!("+{}", self.spec.overflow), false),
            Slot::CrumbRoot => plain(self.spec.crumb.as_ref().map(|c| c.root.clone()).unwrap_or_default(), false),
            Slot::CrumbPage => plain(self.spec.crumb.as_ref().map(|c| c.page.clone()).unwrap_or_default(), true),
        }
    }

    /// What an item says about itself: the flags the paint reads.
    fn face_of(item: &HeaderItem) -> Face<'_> {
        Face {
            label: item.label.clone(), value: item.value.as_deref(), selected: item.selected_plate, title: item.title,
            fold: item.fold_glyph, dimmed: item.dimmed, readout: item.readout, look: Look::of(item), fade: None,
        }
    }

    /// The index in `layout.items` of the item wearing the selected plate (the library on view,
    /// the crumb's page name), `-1` for none: the `selected` the row's motion follows.
    pub fn selected(&self, layout: &HeaderLayout) -> i32 {
        layout.items.iter().position(|r| self.face(r.slot).selected).map_or(-1, |i| i as i32)
    }

    /// **One frame of the row's motion**, on the screen's own `capsules` and `pop`. The focus
    /// capsule travels on the top bar's spring and the selection plate travels with it
    /// ([`SelMark::Travels`], the same spring): pressing a library name selects it under a focus
    /// capsule already on the pill, and the plate follows rather than being cut onto it in one
    /// frame (design review S5, "OK moves the plate"). `selected` and `focused` are indices into
    /// `layout.items` (`-1`: none; focus off the row closes the pop). The span a capsule follows is
    /// the item's plate, the same geometry the row is laid out and hit-tested with.
    ///
    /// **Decoration, not layout** ([`plx_machine::idle::decorative`]): the capsules and the pop move
    /// nothing but themselves, so they wake the present gate and never hold a pushed page's
    /// snapshot, which the page-freeze keeps until the page's LAYOUT stops moving.
    pub fn step(layout: &HeaderLayout, selected: i32, focused: i32, capsules: &mut TabStrip, pop: &mut CtlPop<1>, dt: f32) {
        plx_machine::idle::decorative(|| {
            capsules.update(selected, focused, |i| layout.items.get(i).map(|r| (r.plate.x, r.plate.w)), SelMark::Travels, dt);
            pop.step((focused >= 0).then_some(0), dt);
        });
    }

    /// The ink regions of item `r`'s plate ([`TabStrip::pill_ink`]): which part of its label each
    /// capsule's drawn shape covers. A read-out is no pill and is never inked by one; a `lifted`
    /// control has no focus capsule under it (`ground` leaves it out), so its ink stays the resting
    /// one.
    fn ink(&self, capsules: &TabStrip, r: &ItemRect, face: &Face<'_>, ground: TabGround) -> PillInk {
        if face.readout || !(r.focusable || face.selected) { return PillInk::resting(); }
        capsules.pill_ink(r.plate, ground)
    }

    /// Draw the row: the plates first (the selection's, then the focus capsule over it, through
    /// the top bar's own [`TabStrip`]), then the `›` and every label inked by what is under it, pixel
    /// by pixel, so a label wears a capsule's ink exactly where the capsule is and not before. `pop` is the
    /// screen's [`CtlPop`] scale for the focused control (press dip included); `lifted` is the
    /// index of a control a menu has taken out of the page (`DrawFrame::lifted`): the row draws
    /// none of it, no capsule and no label, because [`Self::draw_opener`] paints its only copy,
    /// focused, above the menu's scrim (design review S12: one rule for every opener).
    pub fn draw(&self, p: Painter, layout: &HeaderLayout, capsules: &TabStrip, pop: f32, lifted: Option<usize>) {
        self.draw_with(p, layout, capsules, pop, lifted, &HeaderSwap::new());
    }

    /// [`Self::draw`] on a row that is changing under the reader: the controls `swap` carries are
    /// painted as it says (a value cross-fading in place, a control leaving or arriving), every
    /// other face as `draw` paints it.
    pub fn draw_with(&self, p: Painter, layout: &HeaderLayout, capsules: &TabStrip, pop: f32, lifted: Option<usize>, swap: &HeaderSwap) {
        let ground = TabGround::Bare { pop, focus: lifted.is_none() };
        capsules.draw(p, layout.top(), HEIGHT, ground);
        if let Some(glyph) = layout.glyph {
            icons::draw(p, Icon::Chevron, glyph, breadcrumb::GLYPH_INK);
        }
        for (index, r) in layout.items.iter().enumerate() {
            if lifted == Some(index) { continue; }
            let face = self.face_in(r, swap);
            // Inked by what is under each pixel: a label a capsule's arc is crossing wears the
            // capsule's ink on its side of the arc and the resting one on the other.
            paint_inked(p, &self.ink(capsules, r, &face, ground), |focus, selected| {
                self.draw_face_at(p, r.text, &face, (0.0, 0.0), (focus, selected));
            });
        }
        for (item, text_x, text_w, opacity) in swap.leaving(&self.spec.right, layout) {
            let face = Face { fade: Some(FaceFade::fade(opacity, None, 1.0)), ..Self::face_of(item) };
            self.draw_face_at(p, Rect::new(text_x, layout.top(), text_w, HEIGHT), &face, (0.0, 0.0), (0.0, 0.0));
        }
    }

    /// **Item `layout.items[index]` as the focused control a menu opened from**: the accent pill
    /// the focus capsule is (the plate stood out by the pop's outset), and the face inked as the
    /// focused control wears it. What [`Self::draw`] shows for a focused item, without the row
    /// around it, so a menu can keep it lit above its scrim.
    pub fn draw_opener(&self, p: Painter, layout: &HeaderLayout, index: usize, pop: f32) {
        self.draw_opener_with(p, layout, index, pop, &HeaderSwap::new());
    }

    /// [`Self::draw_opener`] on a row that is changing under the reader. **A sort picked from the
    /// menu is seen through the lift**: the menu pushes its edit and dismisses itself on one frame,
    /// so the control that opened it is drawn by the lift (the page's pass leaves it out) for the
    /// whole of the swap, and the lift must paint it as the page's pass would
    /// ([`Self::face_in`], the one place a face meets its swap): the criterion it replaces falling
    /// as the new one rises, the mark travelling and the right edge fixed. The pill rides the
    /// face: the lift has no focus capsule to chase the new plate, so its left edge travels with
    /// the mark (`fade.lead`) and its right edge stays, and it wears the face's opacity.
    pub fn draw_opener_with(&self, p: Painter, layout: &HeaderLayout, index: usize, pop: f32, swap: &HeaderSwap) {
        let Some(r) = layout.items.get(index) else { return };
        let face = self.face_in(r, swap);
        let lead = face.fade.map_or(0.0, |fade| fade.lead);
        let pill = focus_pill_popped(Rect::new(r.plate.x + lead, r.plate.y, r.plate.w - lead, r.plate.h), pop);
        face.fade.map_or(p, |fade| p.alpha(fade.face)).rrect(pill, pill.h * 0.5, pill.h * 0.5, crate::ACCENT);
        self.draw_face_at(p, r.text, &face, (0.0, 0.0), (1.0, 0.0));
    }

    /// What `r` says about itself on a row `swap` is changing: its face, and for a right-cluster
    /// control the swap's say about how it is painted now. The page's pass ([`Self::draw_with`])
    /// and the opener lift ([`Self::draw_opener_with`]) both read a face here, so the two cannot
    /// disagree about a frame of the swap.
    fn face_in<'a>(&'a self, r: &ItemRect, swap: &'a HeaderSwap) -> Face<'a> {
        let mut face = self.face(r.slot);
        if let Slot::Right(i) = r.slot { face.fade = swap.fade_of(&self.spec.right, i, r.text.x); }
        face
    }

    /// One item's label, value segment and fold glyph at the text box `text`, shifted by `(dx, dy)`,
    /// inked at the mixes `(focus, selected)`: `rest` toward `strong` as the selection arrives, then
    /// all the way to the accent ink as the focus capsule covers it (focus outranks selection).
    /// `cross`, not `mix`: the resting ink is a translucent white and the accent ink is opaque, and
    /// a blend that kept the source's alpha would land the dark ink at 65%.
    ///
    /// A face riding a [`HeaderSwap`] wears `fade.face` as its opacity and paints the value it
    /// replaces beside its own, at the same x, under the swap's two opacities: in place the label
    /// is drawn once and the two values cross-fade beside it. When the count's width moves the
    /// label, the swap draws the face twice instead (the old one leaving at its x, the new one
    /// arriving at its own), neither with a replaced value beside it.
    fn draw_face_at(&self, p: Painter, text: Rect, face: &Face<'_>, (dx, dy): (f32, f32), (focus, selected): (f32, f32)) {
        let (rest, strong) = if face.readout {
            (theme::COUNT_FILTERED_INK, theme::COUNT_FILTERED_INK)
        } else if face.title {
            (theme::CONTROL_IDLE_INK, theme::CONTROL_IDLE_INK)
        } else if face.dimmed {
            (theme::with_a(theme::CONTROL_TEXT_REST, 0.6), theme::with_a(theme::CONTROL_IDLE_INK, 0.6))
        } else {
            (theme::CONTROL_TEXT_REST, theme::CONTROL_IDLE_INK)
        };
        let to_accent = |ink: [f32; 4]| theme::cross(ink, super::ACCENT_INK, focus.clamp(0.0, 1.0));
        let fade = face.fade;
        let p = fade.map_or(p, |fade| p.alpha(fade.face));
        let look = face.look;
        if look.divider {
            // A hairline, vertically centred on the row; it has no text, no plate and no stop.
            let line = Rect::new(text.x + dx + fade.map_or(0.0, |fade| fade.lead), text.y + dy + (text.h - DIVIDER_H) * 0.5, DIVIDER_W, DIVIDER_H);
            p.rrect(line, 0.0, 0.0, theme::HEADER_DIVIDER_INK);
            return;
        }
        let y = plx_gfx::text::text_vcenter_y(theme::size::BODY, 1, text.y + dy + text.h * 0.5);
        // A slide in flight shifts what comes before the value by `lead`; the value and what trails
        // it stand `value_slide` from rest (below, where the value begins).
        let (lead, value_slide, from_dx) = fade.map_or((0.0, 0.0, 0.0), |fade| (fade.lead, fade.value_slide, fade.from_dx));
        let mut x = text.x + dx + lead;
        let centred = |x: f32, size: f32| Rect::new(x, text.y + dy + (text.h - size) * 0.5, size, size);
        if let Some(glyph) = look.glyph {
            icons::draw(p, glyph, centred(x, GLYPH_SIZE), to_accent(strong));
            x += GLYPH_SIZE + GLYPH_GAP;
        }
        // The label answers to both plates; what follows it (the separator, the value, the glyph)
        // only to the focus one: a value is full white whatever is selected.
        let label_ink = to_accent(theme::cross(rest, strong, selected.clamp(0.0, 1.0)));
        // (A value-only control's label is its name, not its look: nothing is drawn.)
        if !look.value_only {
            if let Ok(c) = CString::new(face.label.as_str()) {
                x += p.text(c.as_ptr(), x, y, theme::size::BODY, label_ink, 0, 0);
            }
        }
        // The separator stands for whichever value is there, so it fades only when the control
        // gains its first value or loses its last.
        let leaving = fade.and_then(|fade| fade.from_value);
        let arriving = fade.map_or(1.0, |fade| fade.value_in);
        let sep_alpha = match (face.value.is_some(), leaving) {
            (true, Some(_)) => 1.0,
            (true, None) => arriving,
            (false, Some((_, out))) => out,
            (false, None) => 0.0,
        };
        let mut value_end = x;
        if sep_alpha > 0.0 && !look.value_only {
            if let Ok(sep) = CString::new(SEP) {
                let advance = p.alpha(sep_alpha).text(sep.as_ptr(), x, y, theme::size::BODY, to_accent(rest), 0, 0);
                x += advance;
                value_end = x;
            }
        }
        x += value_slide - lead;
        for (value, opacity) in leaving.into_iter().chain(face.value.map(|value| (value, arriving))) {
            let own = face.value == Some(value);
            if opacity <= 0.0 && !own { continue; }
            if let Ok(v) = CString::new(value) {
                let at = if own { x } else { x + from_dx };
                // The fold glyph follows the value the face wears, not the one leaving, and it stands
                // where that value ENDS whether or not any of it is visible yet: a criterion that has
                // not begun to arrive (frame zero of a slide) is not drawn, but still has its width.
                let advance = if opacity > 0.0 {
                    p.alpha(opacity).text(v.as_ptr(), at, y, theme::size::BODY, to_accent(strong), 0, 0)
                } else {
                    p.text_width_tracked(v.as_ptr(), theme::size::BODY, 0, 0.0)
                };
                if own { value_end = x + advance; }
            }
        }
        x = value_end;
        // What trails the text stands where `parts` put it: each glyph its own gap after the run.
        let mut run = Flow::new(x, 0.0);
        run.push(Item::new(0.0));
        if look.place {
            let mark = run.push(Item::new(PLACE_SIZE).before(PLACE_GAP)).expect("present");
            icons::draw(p, Icon::Chevron, centred(mark.box_x, PLACE_SIZE), to_accent(theme::HEADER_PLACE_INK));
        }
        if face.fold {
            let fold = run.push(fold_item(look.fold_quiet)).expect("present");
            let (size, ink) = if look.fold_quiet { (QUIET_FOLD_SIZE, rest) } else { (FOLD_SIZE, strong) };
            icons::draw(p, Icon::ChevronDown, centred(fold.box_x, size), to_accent(ink));
        }
    }
}

/// What the paint reads of one item.
struct Face<'a> {
    label: String,
    value: Option<&'a str>,
    selected: bool,
    /// The page's name: full-white plain text, no plate ([`HeaderItem::title`]).
    title: bool,
    fold: bool,
    dimmed: bool,
    readout: bool,
    look: Look,
    /// Set while the item rides a [`HeaderSwap`].
    fade: Option<FaceFade<'a>>,
}

/// What an item wears beyond its text ([`HeaderItem`]'s `glyph`, `place`, `value_only`, `fold_quiet`
/// and `divider`): the paint's copy of them, which a row's own parts (a crumb, the "+N") never have.
#[derive(Clone, Copy, Default)]
struct Look {
    glyph: Option<Icon>,
    place: bool,
    value_only: bool,
    fold_quiet: bool,
    divider: bool,
}

impl Look {
    fn of(item: &HeaderItem) -> Self {
        Self { glyph: item.glyph, place: item.place, value_only: item.value_only, fold_quiet: item.fold_quiet, divider: item.divider }
    }
}

/// How a [`HeaderSwap`] has an item painted this frame.
#[derive(Clone, Copy)]
struct FaceFade<'a> {
    /// The whole face's opacity: 1 for a control that stays, rising for one that is new and
    /// falling for one that is leaving.
    face: f32,
    /// The value this one replaces, and its opacity (falling); `value`'s own is `value_in`.
    from_value: Option<(&'a str, f32)>,
    value_in: f32,
    /// A slide in flight, in px from where the face stands at rest: `lead` shifts what comes before
    /// the value (the mark, the label), `value_slide` the value and what trails it. Both are the
    /// same for a face whose width is unchanged (it slides whole); a control whose criterion changed
    /// keeps its right edge, so its mark slides and its value does not.
    lead: f32,
    value_slide: f32,
    /// Where the value this one replaces stood, relative to the value now (the old criterion
    /// stood at its own x, ending at the same right edge).
    from_dx: f32,
}

impl<'a> FaceFade<'a> {
    /// A fade with no slide in it.
    const fn fade(face: f32, from_value: Option<(&'a str, f32)>, value_in: f32) -> Self {
        Self { face, from_value, value_in, lead: 0.0, value_slide: 0.0, from_dx: 0.0 }
    }
}

/// **A built header row**: the widget, its geometry (document space: y is the row's own top before
/// any scroll) and which element each focusable item is. The row's placement, hit rects, focus
/// order and paint all read this one value, so they cannot disagree. Held by the page that wears
/// the row in a [`RowCache`] (or [`SharedRow`]) and rebuilt only when its inputs change; the row's
/// MOTION is not in it (see the module doc), so replacing it moves nothing that is on screen.
pub struct HeaderRow {
    pub page: PageHeader,
    pub layout: HeaderLayout,
    /// `(element, index into layout.items)` of every focusable item, in focus order.
    pub stops: Vec<(u32, usize)>,
}

impl HeaderRow {
    pub fn new(page: PageHeader, layout: HeaderLayout, stops: Vec<(u32, usize)>) -> Self {
        Self { page, layout, stops }
    }

    /// The plate (focus and hit rect) of `elem` in document space.
    pub fn plate(&self, elem: u32) -> Option<Rect> {
        let (_, index) = self.stops.iter().find(|(stop, _)| *stop == elem)?;
        self.layout.items.get(*index).map(|item| item.plate)
    }

    /// The filter read-out the right cluster carries ("312 of 1,284"), if the row has one: the
    /// text every page that narrows its list draws before its controls.
    pub fn readout(&self) -> Option<&str> {
        self.page.spec().right.iter().find(|item| item.readout).map(|item| item.label.as_str())
    }

    /// The focusable stop that wears the selected plate (the library on view), if there is one.
    pub fn selected_stop(&self) -> Option<u32> {
        let selected = usize::try_from(self.page.selected(&self.layout)).ok()?;
        self.stops.iter().find(|(_, index)| *index == selected).map(|(elem, _)| *elem)
    }

    /// The row as one focus group for `entry`, at the page's scroll (`(drawn, target)`) and
    /// inside `clip`.
    pub fn focus(&self, entry: EntryId, group: GroupId, scroll: (f32, f32), clip: Rect) -> HeaderFocus<'_> {
        HeaderFocus { entry, group, layout: &self.layout, stops: &self.stops, selected: self.selected_stop(), scroll, clip }
    }

    /// One frame of the row's motion on the screen's `capsules` and `pop`, from the engine's
    /// `current` focus and the page's own `entry`. Focus on one of the row's stops draws the
    /// capsule there; focus anywhere else on the page (or nowhere) closes it. Focus on ANOTHER
    /// entry is a surface over the page, a Sort & Filter menu on its own rows, with the control
    /// lifted out of the row: the motion holds as it was, so the menu's closing morph hands over to
    /// a capsule that is still up, not one that has to fade back in.
    pub fn step(&self, entry: EntryId, current: Option<FocusKey<u32>>, capsules: &mut TabStrip, pop: &mut CtlPop<1>, dt: f32) {
        if current.is_some_and(|key| key.entry != entry) { return; }
        let focused = current.map(|key| key.elem)
            .and_then(|elem| self.stops.iter().position(|(stop, _)| *stop == elem))
            .map_or(-1, |at| self.stops[at].1 as i32);
        PageHeader::step(&self.layout, self.page.selected(&self.layout), focused, capsules, pop, dt);
    }

    /// **The opener, focused, above its menu's scrim** (design review S12): stop `elem` as the
    /// focused control the menu grew out of, `scroll` px up from its document position. The ONE
    /// paint of an opener for every header menu (Sort & Filter, `+N`): the page's pass leaves the
    /// control out of the row ([`Self::draw`]'s `lifted`) and `Screen::redraw_focused` calls this
    /// from the dispatcher's lift pass, which runs after the last surface on every plan (above the
    /// dim AND the menu's panel; `Z::OPENER`), so the panel never covers it. `pop` is the
    /// row's [`CtlPop`] factor, so the pill follows the press spring as the menu opens.
    pub fn draw_opener(&self, p: Painter, scroll: f32, pop: f32, elem: u32) {
        self.draw_opener_with(p, scroll, pop, elem, &HeaderSwap::new());
    }

    /// [`Self::draw_opener`] while the screen's `swap` carries the row's controls from the row
    /// before it ([`PageHeader::draw_opener_with`]): a sort picked from the menu changes the
    /// control that opened it, which only the lift draws until the menu is gone.
    pub fn draw_opener_with(&self, p: Painter, scroll: f32, pop: f32, elem: u32, swap: &HeaderSwap) {
        let Some((_, index)) = self.stops.iter().find(|(stop, _)| *stop == elem) else { return };
        let mut layout = self.layout.clone();
        for item in &mut layout.items {
            item.text.y -= scroll;
            item.plate.y -= scroll;
        }
        self.page.draw_opener_with(p, &layout, *index, pop, swap);
    }

    /// Paint the row `scroll` px up from its document position (it scrolls away with its page).
    /// `lifted` is the element a menu has taken out of the row: the row draws none of it
    /// ([`Self::draw_opener`] is its only copy).
    pub fn draw(&self, p: Painter, scroll: f32, capsules: &TabStrip, pop: f32, lifted: Option<u32>) {
        self.draw_with(p, scroll, capsules, pop, lifted, &HeaderSwap::new());
    }

    /// [`Self::draw`] while the screen's `swap` carries the row's controls from the row before it.
    pub fn draw_with(&self, p: Painter, scroll: f32, capsules: &TabStrip, pop: f32, lifted: Option<u32>, swap: &HeaderSwap) {
        let mut layout = self.layout.clone();
        for item in &mut layout.items {
            item.text.y -= scroll;
            item.plate.y -= scroll;
        }
        if let Some(glyph) = &mut layout.glyph { glyph.y -= scroll; }
        let lifted = lifted.and_then(|elem| self.stops.iter().find(|(stop, _)| *stop == elem).map(|(_, item)| *item));
        self.page.draw_with(p, &layout, capsules, pop, lifted, swap);
    }
}

/// One control of the row a [`HeaderSwap`] carries over from the row before: what it said and where
/// its text stood (x and width; the row is one line, so y is the new row's own).
struct Carried {
    item: HeaderItem,
    x: f32,
    w: f32,
    /// A read-out that rides the slide ([`HeaderSwap::glides_between`]): its move is the glide's, not
    /// a difference to dissolve.
    glides: bool,
}

impl Carried {
    /// Does the new row's `item` stand for this carried one? A slotted control is known by its
    /// label; the row's read-out (there is one) by being the read-out, since its text IS what
    /// changes ("24 of 27" to "8 of 27" is the same read-out saying another thing).
    fn is(&self, item: &HeaderItem) -> bool {
        if self.item.readout || item.readout { self.item.readout && item.readout } else { self.item.label == item.label }
    }

    /// Does `item`, standing at `x`, read differently from this carried one? A slotted control by
    /// its value; a read-out by its text or by where it stands (it is unpinned, so it moves with
    /// the cluster: Collections arriving, a count's digit count, the Sort value), because it has
    /// no value to cross-fade and a move is a dissolve at each x, not a cut. **Unless the move is
    /// the cluster's slide** (`glides`: the criterion changed and the read-out, whose text did not,
    /// travels with its neighbours on the slide's spring): that is no difference to dissolve.
    fn differs(&self, item: &HeaderItem, x: f32) -> bool {
        if self.item.readout { self.item.label != item.label || (!self.glides && (self.x - x).abs() >= SAME_X) } else { self.item.value != item.value }
    }
}

/// Does the header swap carry `item`: a slotted control, or the filter read-out?
fn carried(item: &HeaderItem) -> bool { item.value_slot.is_some() || item.readout }

/// **The header row's content swap**: when a rebuilt row changes a control that has a
/// [`HeaderItem::value_slot`], the reader sees the change as a dissolve and not a cut. The label
/// stays where it is drawn and the value beside it cross-fades in place, old number to new; a
/// control the new row does not have fades out where it stood; one it did not have fades in. A
/// control whose label stands at another x on the new row (a count of another digit count moves it
/// by the digits' width, [`PageHeader::layout`] placing it by what it draws) does both: the old face
/// leaves at the old x while the new one arrives at the new x. That is the SAME page's row changing
/// ([`Self::begin`]). The filter read-out rides the same swap: it is unpinned (one gap before the
/// next item), so it stands at another x when Collections arrives or leaves, its count changes digit
/// count or the Sort value changes, and it says another thing as the filter narrows; each is that
/// leave-and-arrive, never a cut, except that a Sort value change which moves a read-out whose text
/// is the same moves it by what it moves Collections, and it GLIDES with them on the slide (below),
/// not on a second clock beside it. A switch to another library's row ([`Self::begin_switch`]) shows only what the
/// new library has: a control the old one had is not drawn on it, even fading. All on [`Xfade`], the one
/// content-swap clock (`arrive`: the incoming ramp, [`Xfade::alpha`] smoothstepped), so no page keeps
/// a timer of its own. A dissolving label is never drawn between its two places. **Items that a
/// control's new width moves SLIDE** instead (the Library's sort criterion changing: Collections,
/// Recommended and the divider stand where the row before had them and travel to the new row's
/// places, the sort control's mark with them, on the swap's slide: the tab bar's own spring,
/// critically damped, so with no overshoot; every drawn x is a function of its progress, so a landed
/// or [`HeaderSwap::settle`]d spring is the new layout). The control itself keeps its right edge
/// and its chevron: the criterion it replaces fades out at its own place and the new one fades in
/// at its own, each ending at that edge ([`Criterion`]), on the same progress. A row built twice for
/// one press (the queued sort, then the listing under it) carries that slide on, nothing jumping and
/// nothing restarting: a rebuild that moves no target keeps the running spring as it is, and one that
/// does (another criterion picked mid-slide) starts from where the pieces are drawn at the speed
/// they have.
/// A switch ([`HeaderSwap::begin_switch`]) slides nothing.
///
/// **Held by the SCREEN beside the row's capsules**, for the same reason they are: the row is
/// replaced whole on every rebuild, and the swap must outlive the row it began on. It is
/// decoration (the clock invalidates the present gate while it runs), never part of a page's
/// canonical state: a recording and its replay rebuild the same rows, and what is shown between
/// two of them is paint.
pub struct HeaderSwap {
    clock: Xfade,
    /// The slotted controls and the read-out of the row before the swap (none when that row had
    /// none, a control then only arrives); empty at rest.
    from: Vec<Carried>,
    /// A swap was begun and its clock has not run out.
    running: bool,
    /// What the swap does with a control only one of the two rows has.
    edge: Edge,
    /// **The slide**: the progress, 0 (every item where the row before had it) to 1 (where the new
    /// row has it), of the cluster's items that moved when a control's width did (the sort control's
    /// criterion). The tab bar's own spring ([`crate::widgets::K_TAB_CAP`], critically damped, so no
    /// overshoot), the one the row's capsules travel on, so the plate under a place and the place
    /// arrive together. Every drawn position is a pure function of it: a spring that lands (or is
    /// jumped) puts every item at rest.
    slide: Spring,
    /// The items that slide; empty at rest.
    glides: Vec<Glide>,
}

/// One right-cluster item a [`HeaderSwap`]'s slide carries: the item as the row before had it (its
/// identity and the value it is leaving, if it has a [`Criterion`]) and where its text stood,
/// `from`, and stands now, `to`.
struct Glide {
    item: HeaderItem,
    from: f32,
    to: f32,
    /// Set for a control whose criterion changed: it keeps its right edge, so only what leads the
    /// criterion (the mark) slides, and the criterion cross-fades.
    criterion: Option<Criterion>,
}

/// **How a control that keeps its right edge swaps its criterion** while its mark slides: the
/// criterion it replaces is drawn at ITS OWN rest x (`dx` px from where the new one rests, both
/// ending at the control's one right edge) and fades out from the opacity it had when the slide
/// began, `out0`; the new one is drawn at its own rest x and fades in from `in0`. The chevron after
/// them is the new layout's and never moves. A slide begun from rest has `(1, 0)`; one picked up
/// while another runs (a second build of the row, the listing that lands under the queued sort)
/// starts from what the first has reached, so nothing jumps and the pair still sums to one.
#[derive(Clone, Copy)]
struct Criterion {
    dx: f32,
    out0: f32,
    in0: f32,
}

impl Criterion {
    /// The opacity of the criterion leaving, at slide progress `s`.
    fn leaving(self, s: f32) -> f32 { self.out0 * (1.0 - s) }

    /// The opacity of the criterion arriving, at slide progress `s`.
    fn arriving(self, s: f32) -> f32 { self.in0 + (1.0 - self.in0) * s }
}

/// Is `a` the same control as `b`? A read-out is the read-out (whatever it says), a divider the
/// divider, any other item by its label (the sort control's is its NAME, which its criterion does
/// not change).
fn same_item(a: &HeaderItem, b: &HeaderItem) -> bool {
    if a.readout || b.readout { return a.readout && b.readout; }
    a.divider == b.divider && a.label == b.label
}

/// What a [`HeaderSwap`] does with a control that only one of the two rows has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// The row is the same page changing: the control fades out where it stood, or fades in.
    Fade,
    /// The rows are two libraries' (a switch): the new row stands as it is, so a control the old
    /// one had is not drawn on the new library's page (not even fading out) and one it lacked is
    /// simply there. Only a control both rows have dissolves, number to number.
    Cut,
}

/// Two x positions this close are one place: a count of another digit count moves its label by a
/// digit or more, a count of the same width in a proportional font by a fraction of a pixel, which
/// the eye reads as the same spot (the value then cross-fades in place).
const SAME_X: f32 = 0.5;

impl Default for HeaderSwap {
    fn default() -> Self { Self::new() }
}

impl HeaderSwap {
    pub const fn new() -> Self {
        Self { clock: Xfade::new(), from: Vec::new(), running: false, edge: Edge::Fade, slide: Spring::at(0.0), glides: Vec::new() }
    }

    /// Is a swap on screen: a dissolve running or items sliding?
    pub fn is_active(&self) -> bool { self.dissolving() || self.is_sliding() }

    /// Is a dissolve (a value cross-fading, a control arriving or leaving) running?
    fn dissolving(&self) -> bool { self.running && self.clock.is_swapping() }

    /// Are items sliding to their new places?
    pub fn is_sliding(&self) -> bool { !self.glides.is_empty() }

    /// How far the slide has run, 0 (the row before) to 1 (the new row).
    pub fn slide_progress(&self) -> f32 { self.slide.pos.clamp(0.0, 1.0) }

    /// Put the slide at its end (the instant frame, a settle): every item stands at rest.
    pub fn settle(&mut self) {
        self.glides.clear();
        self.slide = Spring::at(0.0);
    }

    /// The right-cluster items that stand elsewhere on `new` than on `old` (or say another value
    /// while they do not dissolve): each where it is DRAWN now (a slide still running is picked up
    /// where it is, so a second change never jumps) and where it will stand.
    ///
    /// **A criterion that changed carries on a slide that already runs to it**: the row is built
    /// twice for one press (the queued sort, then the listing that lands under it), and the second
    /// build says the criterion the first already shows. That one is no new change: the criterion
    /// it replaced keeps fading out from where it had got to, and the control's face stays what
    /// it was (its mark sliding, the criterion and the chevron where the layout has them).
    ///
    /// Also says whether a criterion changed on THIS rebuild (not one the running slide already
    /// carries): [`Self::begin_with`] keeps a running slide for a rebuild that moves no target and
    /// changes no criterion, and hands it over, speed and all, for one that does.
    fn glides_between(&self, old: &HeaderRow, new: &HeaderRow) -> (Vec<Glide>, bool) {
        let spot = |row: &HeaderRow, i: usize| row.layout.items.iter().find(|r| r.slot == Slot::Right(i)).map(|r| r.text.x);
        let s = self.slide_progress();
        let (before, after) = (&old.page.spec().right, &new.page.spec().right);
        let mut changed_now = false;
        let mut glides: Vec<Glide> = after.iter().enumerate().filter_map(|(i, item)| {
            let (j, was) = before.iter().enumerate().find(|(_, was)| same_item(was, item))?;
            let (rest, to) = (spot(old, j)?, spot(new, i)?);
            // A read-out says another thing by its text; every other item by its value.
            let changed = if item.readout { was.label != item.label } else { was.value != item.value };
            // A slotted control whose value changed is the dissolve's (it leaves and arrives).
            if carried(item) && changed { return None; }
            // Where it is drawn now: a slide still running is picked up there, so a second change never jumps.
            let live = self.glides.iter().find(|g| same_item(&g.item, item));
            let from = live.map_or(rest, |g| g.to + (g.from - g.to) * (1.0 - s));
            let running = live.and_then(|g| g.criterion.map(|c| (c, g.item.value.clone())));
            let (value, criterion) = match (changed, running) {
                (false, Some((c, leaving))) => (leaving, Some(Criterion { dx: c.dx, out0: c.leaving(s), in0: c.arriving(s) })),
                (true, running) => (was.value.clone(),
                    Some(Criterion { dx: rest - to, out0: running.map_or(1.0, |(c, _)| c.arriving(s)), in0: 0.0 })),
                (false, None) => (was.value.clone(), None),
            };
            let glides = (from - to).abs() >= SAME_X || changed;
            changed_now |= glides && changed;
            glides.then(|| Glide { item: HeaderItem { value, ..was.clone() }, from, to, criterion })
        }).collect();
        // The read-out is the cluster's leading item, so a criterion change moves it by what it moves
        // Collections, and it travels with them. Moved by anything else (Collections arriving, a
        // count's digit count) there is no slide for it to ride, and it dissolves at each x.
        let slides = glides.iter().chain(&self.glides).any(|g| g.criterion.is_some());
        if !slides { glides.retain(|g| !g.item.readout); }
        (glides, changed_now)
    }

    /// Does a slide that runs already end where every one of `glides` would end? Then a rebuild
    /// that asks for them (no target moved, no criterion changed: the same press building its row
    /// again as the listing refetches) has nothing to say to it.
    fn already_runs_to(&self, glides: &[Glide]) -> bool {
        self.is_sliding() && glides.iter().all(|new| {
            self.glides.iter().any(|running| same_item(&running.item, &new.item) && (running.to - new.to).abs() < SAME_X)
        })
    }

    /// The slide's rate to start the spring over `glides` at, so the item that moves furthest keeps
    /// the speed it has: it is moving `(to - from) * vel` px a second on the running slide and will
    /// cover `new.to - new.from` per unit of progress on the new one. The rate is kept between 0
    /// and the spring's own natural frequency, the range a critically damped spring from rest
    /// covers without overshoot, which a rescale of an undisturbed slide never leaves; a slide that
    /// is not running (or whose item goes the other way) starts from rest.
    fn carried_rate(&self, glides: &[Glide]) -> f32 {
        glides.iter().filter_map(|new| {
            let running = self.glides.iter().find(|running| same_item(&running.item, &new.item))?;
            let span = new.to - new.from;
            (span.abs() >= SAME_X).then(|| ((running.to - running.from) * self.slide.vel / span, span.abs()))
        }).max_by(|a, b| a.1.total_cmp(&b.1)).map_or(0.0, |(rate, _)| rate.clamp(0.0, crate::widgets::K_TAB_CAP.sqrt()))
    }

    /// The row `old` is being replaced by `new`: start a swap when a slotted control changed (its
    /// value, or its being there at all). A row that changes nothing a swap carries starts nothing
    /// and leaves one already running to finish; a breadcrumb row on either side is a different
    /// page, not the same row changing, and cancels it.
    pub fn begin(&mut self, old: &HeaderRow, new: &HeaderRow) {
        self.begin_with(old, new, Edge::Fade);
    }

    /// [`Self::begin`] for rows of two DIFFERENT libraries (a switch): the new library's page
    /// arrives with the controls it has, and the old library's controls it does not have are not
    /// drawn on it, not even fading (a Collections control on a library with none, for a few
    /// frames, reads as that library having collections). A control both rows have still
    /// dissolves: its number cross-fades in place, or leaves and arrives at each x when the
    /// number's width moves its label. A fade of the row being left that is still running ends
    /// with the switch, whether or not the switch itself carries a swap: its faces are the left
    /// library's.
    pub fn begin_switch(&mut self, old: &HeaderRow, new: &HeaderRow) {
        self.begin_with(old, new, Edge::Cut);
    }

    fn begin_with(&mut self, old: &HeaderRow, new: &HeaderRow, edge: Edge) {
        if old.page.spec().crumb.is_some() || new.page.spec().crumb.is_some() { return self.end(); }
        let carry = |row: &HeaderRow| -> Vec<Carried> {
            row.page.spec().right.iter().enumerate().filter(|(_, item)| carried(item)).filter_map(|(i, item)| {
                let at = row.layout.items.iter().find(|r| r.slot == Slot::Right(i))?;
                Some(Carried { item: item.clone(), x: at.text.x, w: at.text.w, glides: false })
            }).collect()
        };
        // A switch shows nothing of the row left, so nothing slides from it; a change of the same
        // page slides the items a control's new width moved.
        let (glides, changed) = if edge == Edge::Fade { self.glides_between(old, new) } else { (Vec::new(), false) };
        if edge == Edge::Cut { self.glides.clear(); }
        let mut before = carry(old);
        for readout in before.iter_mut().filter(|c| c.item.readout) { readout.glides = glides.iter().any(|g| g.item.readout); }
        let after = carry(new);
        let same = match edge {
            Edge::Fade => before.len() == after.len()
                && before.iter().all(|b| after.iter().any(|a| b.is(&a.item) && !b.differs(&a.item, a.x))),
            Edge::Cut => after.iter().all(|a| before.iter().find(|b| b.is(&a.item)).is_none_or(|b| !b.differs(&a.item, a.x))),
        };
        if same {
            // A switch is a different page: a Fade of the row it leaves must not outlive it (its
            // `leaving()` faces are the left library's, not the new page's).
            if edge == Edge::Cut { self.end(); }
        } else {
            self.from = before;
            self.running = true;
            self.edge = edge;
            self.clock.arrive();
        }
        // **A rebuild is not a restart.** One press builds the row three times or more as the listing
        // refetches, and each build re-bases the glides on the x they are drawn at. A slide whose
        // targets did not move (and that no criterion change rides) keeps its glides and its
        // spring, position and velocity both; otherwise the new glides start from where the
        // running ones are drawn, and the spring starts at the speed they have.
        if !glides.is_empty() && (changed || !self.already_runs_to(&glides)) {
            self.slide = Spring { pos: 0.0, vel: self.carried_rate(&glides) };
            self.glides = glides;
        }
    }

    /// Stop at once, the row as it is: the dissolve and the slide both.
    pub fn end(&mut self) {
        self.end_dissolve();
        self.settle();
    }

    fn end_dissolve(&mut self) {
        self.from.clear();
        self.running = false;
        self.clock = Xfade::new();
    }

    /// One frame of the swap's clocks: the dissolve's, and the slide's spring on the tab bar's own
    /// stiffness. **Decoration, not layout** ([`plx_machine::idle::decorative`]).
    pub fn step(&mut self, dt: f32) {
        if self.running {
            plx_machine::idle::decorative(|| { self.clock.tick(dt, true); });
            if !self.clock.is_swapping() { self.end_dissolve(); }
        }
        if self.is_sliding() {
            plx_machine::idle::decorative(|| self.slide.step(1.0, crate::widgets::K_TAB_CAP, dt));
            if self.slide.land(1.0, dt) { self.settle(); }
        }
    }

    /// How far the swap has run, 0 (the old row) to 1 (the new one), eased.
    fn progress(&self) -> f32 { self.clock.alpha() }

    /// Has the carried control `before` MOVED to `item` standing at `x`: it reads differently and
    /// stands at another x? Then its old face leaves at the old x and the new one arrives at `x`
    /// (there is no place between them to cross-fade in). A read-out that only says another thing
    /// at the same x is no move either, but it has no value to cross-fade, so it too is a leave
    /// and an arrival, both at that x.
    fn moved(before: &Carried, item: &HeaderItem, x: f32) -> bool {
        if before.item.readout { return before.differs(item, x); }
        before.item.value != item.value && (before.x - x).abs() >= SAME_X
    }

    /// How the item `right[index]`, whose text stands at `x`, is painted now; `None` when the swap
    /// has nothing to say about it.
    fn fade_of<'a>(&'a self, right: &'a [HeaderItem], index: usize, x: f32) -> Option<FaceFade<'a>> {
        let item = right.get(index)?;
        self.dissolve_of(item, x).or_else(|| self.glide_of(item))
    }

    /// The dissolve's say about `item` (a slotted control or the read-out), standing at `x`.
    fn dissolve_of<'a>(&'a self, item: &'a HeaderItem, x: f32) -> Option<FaceFade<'a>> {
        if !self.dissolving() || !carried(item) { return None; }
        let a = self.progress();
        let Some(before) = self.from.iter().find(|c| c.is(item)) else {
            return (self.edge == Edge::Fade).then_some(FaceFade::fade(a, None, 1.0));
        };
        if !before.differs(item, x) { return None; }
        if Self::moved(before, item, x) { return Some(FaceFade::fade(a, None, 1.0)); }
        let from_value = before.item.value.as_deref().map(|value| (value, 1.0 - a));
        Some(FaceFade::fade(1.0, from_value, if item.value.is_some() { a } else { 0.0 }))
    }

    /// The slide's say about `item`: it stands `(from - to) * (1 - progress)` px from its place, the
    /// whole face together. A control whose criterion changed (the sort control's) changed width
    /// with it and keeps its right edge: the mark before the criterion slides, the new criterion
    /// stands at its own rest x and the chevron after it where the layout has it, so nothing of
    /// the face is ever drawn right of that edge, and the criterion it replaces cross-fades out at
    /// ITS rest x, ending at the same edge ([`Criterion`]). Both opacities come from the slide's one
    /// progress, so they sum to 1 and end exact.
    fn glide_of<'a>(&'a self, item: &'a HeaderItem) -> Option<FaceFade<'a>> {
        let g = self.glides.iter().find(|g| same_item(&g.item, item))?;
        let (s, shift) = (self.slide_progress(), (g.from - g.to) * (1.0 - self.slide_progress()));
        let Some(c) = g.criterion else { return Some(FaceFade { lead: shift, value_slide: shift, ..FaceFade::fade(1.0, None, 1.0) }); };
        let leaving = g.item.value.as_deref().map(|value| (value, c.leaving(s)));
        Some(FaceFade { lead: shift, value_slide: 0.0, from_dx: c.dx, ..FaceFade::fade(1.0, leaving, if item.value.is_some() { c.arriving(s) } else { 0.0 }) })
    }

    /// The faces of the row before that leave: a slotted control the new row (`right`, placed as
    /// `layout`) does not have, and one it has at another x with another value. Each with where its
    /// text stood and its opacity now, falling.
    fn leaving<'a>(&'a self, right: &'a [HeaderItem], layout: &'a HeaderLayout) -> impl Iterator<Item = (&'a HeaderItem, f32, f32, f32)> + 'a {
        let active = self.dissolving();
        let opacity = 1.0 - self.progress();
        let edge = self.edge;
        self.from.iter().filter(move |c| active && {
            let now = right.iter().enumerate().find(|(_, item)| c.is(item));
            now.map_or(edge == Edge::Fade, |(i, item)| {
                layout.items.iter().find(|r| r.slot == Slot::Right(i)).is_some_and(|r| Self::moved(c, item, r.text.x))
            })
        }).map(move |c| (&c.item, c.x, c.w, opacity))
    }
}

/// **The one rule for holding a built header row**: build it when its inputs change, never
/// otherwise. A row is text measurement and packing; the pages ask for it many times a frame (their
/// focus questions, their placement, their paint), so each page keeps ONE row beside a `key`, a
/// fingerprint of everything the build reads, and rebuilds only when that key moves. The Library,
/// its Collections and Recommended pages and the Watchlist all hold their row through this.
///
/// [`RowCache`] is the plain form, for a page that refreshes in its `&mut` ticks and reads from
/// `&self`; [`SharedRow`] is the same rule behind interior mutability, for a page whose readers
/// are `&self` questions that must have the row on the first call. `builds` counts the rebuilds
/// (what a test pins: idle frames build nothing, a changed input builds once).
pub struct RowCache<R> {
    row: Option<R>,
    key: Option<u64>,
    builds: u32,
}

impl<R> Default for RowCache<R> {
    fn default() -> Self { Self::new() }
}

impl<R> RowCache<R> {
    pub const fn new() -> Self { Self { row: None, key: None, builds: 0 } }

    /// Is the row held the one `key` stands for?
    pub fn is_current(&self, key: u64) -> bool { self.row.is_some() && self.key == Some(key) }

    /// Hold `row`, built for `key`.
    pub fn install(&mut self, key: u64, row: R) {
        self.row = Some(row);
        self.key = Some(key);
        self.builds += 1;
    }

    /// The row for `key`: the one held when it was built for this key, else `build()` is held.
    /// Whether it was rebuilt.
    pub fn refresh(&mut self, key: u64, build: impl FnOnce() -> R) -> bool {
        if self.is_current(key) { return false; }
        self.install(key, build());
        true
    }

    /// The row held, `None` before the first build.
    pub fn row(&self) -> Option<&R> { self.row.as_ref() }

    /// The key the row was built for.
    pub fn key(&self) -> Option<u64> { self.key }

    /// How many times a row has been built.
    pub fn builds(&self) -> u32 { self.builds }
}

/// [`RowCache`] behind a `RefCell`, handing the row out as an `Rc`: for the page whose readers are
/// `&self` (the engine's `Focusable` questions, the draw) and must find the row on the first call.
/// The row is cheap to hold across a call (an `Rc` clone) and owns no borrow of the page.
pub struct SharedRow<R>(std::cell::RefCell<RowCache<std::rc::Rc<R>>>);

impl<R> Default for SharedRow<R> {
    fn default() -> Self { Self::new() }
}

impl<R> SharedRow<R> {
    pub const fn new() -> Self { Self(std::cell::RefCell::new(RowCache::new())) }

    /// The row for `key`, built by `build` when the one held was built for another key.
    pub fn get(&self, key: u64, build: impl FnOnce() -> R) -> std::rc::Rc<R> {
        self.get_replacing(key, build, |_, _| {})
    }

    /// [`Self::get`], telling `replaced(old, new)` when a row that was held is replaced by a rebuild
    /// (the first build replaces nothing), before the new row is handed out. A page whose readers
    /// are `&self` begins its [`HeaderSwap`] there (`swap.begin(old, new)`): the one place that
    /// sees every rebuild, from whichever question or paint asked first, so the frame that changes
    /// the row already draws at the swap's first step and never as a cut. `replaced` runs after
    /// the cache is released, but it must not rebuild this row itself.
    pub fn get_replacing(&self, key: u64, build: impl FnOnce() -> R, replaced: impl FnOnce(&R, &R)) -> std::rc::Rc<R> {
        let (row, old) = {
            let mut cache = self.0.borrow_mut();
            let old = if cache.is_current(key) { None } else { cache.row().cloned() };
            cache.refresh(key, || std::rc::Rc::new(build()));
            (std::rc::Rc::clone(cache.row().expect("refreshed above")), old)
        };
        if let Some(old) = old { replaced(&old, &row); }
        row
    }

    /// How many times a row has been built.
    pub fn builds(&self) -> u32 { self.0.borrow().builds() }
}

/// **A header row as ONE focus group**, borrowed per call from the page that wears it. Its stops
/// are the focusable items in `layout.items` order; Left and Right walk them with no wrap and
/// never leave the row (the group's edges Stop), Up and Down are the page's `Link`s
/// ([`crate::page_regions::PageRegions::links`]). Every page that wears a header answers the
/// engine's `Focusable` questions for it by delegating here, so the row means the same on all of
/// them: a `Control` row, seated where the reader last was, else on the selected item, else on the
/// stop nearest the key that came up from below.
#[derive(Clone, Copy)]
pub struct HeaderFocus<'a> {
    pub entry: EntryId,
    pub group: GroupId,
    pub layout: &'a HeaderLayout,
    /// `(element, index into layout.items)` of every stop, in focus order.
    pub stops: &'a [(u32, usize)],
    /// The focusable item wearing the selected plate, if any (a crumb's page name is not one).
    pub selected: Option<u32>,
    /// The page's scroll, `(drawn, target)`: the row rides it.
    pub scroll: (f32, f32),
    /// The panel the row is placed and hit-tested in.
    pub clip: Rect,
}

impl HeaderFocus<'_> {
    /// The group the engine declares for the row; `None` when it has no stop to land on.
    pub fn group_spec(&self) -> Option<GroupSpec> {
        if self.stops.is_empty() { return None; }
        Some(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Remembered,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
            // The row's plates, so a move from the content below ranks it by x.
            extent: Rect::new(TEXT_X0 - OVERHANG, self.layout.top() - self.scroll.1,
                TEXT_X1 - TEXT_X0 + 2.0 * OVERHANG, HEIGHT),
            len: self.stops.len(),
            elem: ElemKind::Control,
        })
    }

    pub fn contains(&self, elem: u32) -> bool {
        self.stops.iter().any(|(stop, _)| *stop == elem)
    }

    /// The stops' elements in focus order.
    pub fn elems(&self) -> Vec<u32> {
        self.stops.iter().map(|(elem, _)| *elem).collect()
    }

    /// Left and Right along the stops; `Step::Edge` at the ends and for Up and Down.
    pub fn neighbour(&self, key: FocusKey<u32>, dir: Dir) -> Step<u32> {
        row_neighbour(self.entry, &self.elems(), key, dir)
    }

    /// Where the stop `elem` is, at the drawn or the target scroll.
    pub fn place(&self, elem: u32, at: At) -> Option<Placed> {
        let (_, index) = self.stops.iter().find(|(stop, _)| *stop == elem)?;
        let plate = self.layout.items.get(*index)?.plate;
        let scroll = match at { At::Drawn => self.scroll.0, At::SpringTarget => self.scroll.1 };
        let rect = Rect::new(plate.x, plate.y - scroll, plate.w, plate.h);
        Some(Placed { rect, rest_rect: rect, clip: self.clip, index: None })
    }

    /// The stop the engine seats when focus comes into the row from `from` and has no remembered
    /// one: the selected item (Up from the grid lands on the library on view), else the stop
    /// nearest `from` by x.
    pub fn seat(&self, from: Placed) -> FocusKey<u32> {
        let nearest = || self.stops.iter()
            .filter_map(|(elem, _)| self.place(*elem, At::SpringTarget).map(|placed| (*elem, (placed.rect.cx() - from.rect.cx()).abs())))
            .min_by(|a, b| a.1.total_cmp(&b.1)).map(|(elem, _)| elem);
        let elem = self.selected.or_else(nearest).unwrap_or(crate::dispatch::STRIP_BASE);
        FocusKey { entry: self.entry, elem }
    }

    /// The scroll a page rests at when focus lands on `elem`: 0 for a stop of the row (it stands
    /// at the top of the document, so reaching it brings the page back to its top), `None` for
    /// anything else.
    pub fn reveal(&self, elem: u32) -> Option<f32> {
        self.contains(elem).then_some(0.0)
    }

    /// `want` if it is a stop of the row, else `None` (the page falls back to where the key was
    /// last placed, then to its first seat).
    pub fn reconcile(&self, want: FocusKey<u32>) -> Option<FocusKey<u32>> {
        (want.entry == self.entry && self.contains(want.elem)).then_some(want)
    }
}

#[cfg(test)]
#[path = "page_header_tests.rs"]
mod tests;
