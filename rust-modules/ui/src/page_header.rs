//! **PageHeader** — the one row of plain-text controls a library page wears under the top bar:
//! the library names on the left (with a "+N" for the ones that do not fit) or a breadcrumb
//! ("Films › Collections"), and the page's own controls on the right ("Collections · 12",
//! "Recommended", "Sort & Filter · Title ▾"). The Library, its Collections and Recommended pages
//! and the Watchlist all wear THIS row: one layout, one draw, one focus group ([`HeaderFocus`]).
//!
//! Geometry (spec): the text edges are x 96 and x 1771; a selected plate overhangs the text by
//! [`OVERHANG`] (20), so the container is left 76, right 129, and a focus pill stands a constant
//! [`PILL_FOCUS_OUTSET`] (2) past its plate, whatever its label (never a scale of the label's width). The row is
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
use super::widgets::{focus_pill_popped, paint_inked, CtlPop, PillInk, SelMark, TabGround, TabStrip, PILL_FOCUS_OUTSET};
use super::xfade::Xfade;
use super::{icons::{self, Icon}, theme, Painter, Rect};

/// Left text edge of the first library name.
pub const TEXT_X0: f32 = 96.0;
/// Right text edge of the last right-cluster item.
pub const TEXT_X1: f32 = 1771.0;
/// How far a plate stands out past its text on each side (a focus pill: that and [`PILL_FOCUS_OUTSET`]).
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
/// overhang, the focused pill's [`PILL_FOCUS_OUTSET`] past that plate, [`theme::space::SM`] of air,
/// then the next item's plate and its overhang. The air is measured from the FOCUSED pill's extent
/// ([`focus_pill`](super::widgets::focus_pill)) because that is the edge the eye reads against the neighbour (design review S9: the
/// old `XS` of air was spent before the pill grew, and a 1.07 scale grew by more the wider the name
/// was, so "Classics" read ~10 px from the "Movies" plate). The outset is a constant (S4), so the gap
/// is one number, and every placement and every packing uses it. The breadcrumb's chevron takes the
/// same air on each side ([`breadcrumb::AIR`]).
pub const GAP: f32 = 2.0 * OVERHANG + PILL_FOCUS_OUTSET + theme::space::SM;
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

/// The room an item is BUDGETED in the row: its drawn run, or for a control with a
/// [`HeaderItem::value_slot`] the label and the separator and the slot (the count it holds may be
/// wider than the slot, never narrower, so the budget never changes while the count is a few
/// digits). It sizes `cluster_w` and `left_budget`; the items are placed by [`ink_w`].
fn run_w(item: &HeaderItem, measure: &dyn Measure) -> f32 {
    let w = |s: &str| text_w(s, measure);
    let (sep, value) = match (&item.value_slot, &item.value) {
        (Some(slot), value) => (Item::new(w(" \u{b7} ")), Item::new(w(slot).max(value.as_deref().map_or(0.0, w)))),
        (None, Some(value)) => (Item::new(w(" \u{b7} ")), Item::new(w(value))),
        (None, None) => (Item::absent(), Item::absent()),
    };
    let fold = if item.fold_glyph { fold_item() } else { Item::absent() };
    HStack::new(&[Item::new(w(&item.label)), sep, value, fold], 0.0, Align::Leading(0.0)).width()
}

/// The fold glyph as a run item: [`FOLD_SIZE`] wide, [`FOLD_GAP`] after whatever precedes it. `run_w`
/// sizes the item with it and `draw_face_at` places the glyph from it, so the two cannot disagree.
fn fold_item() -> Item {
    Item::new(FOLD_SIZE).before(FOLD_GAP)
}

/// The width of what an item DRAWS: [`run_w`], less the unused part of a slot. The item's text box
/// and its plate are this wide, and the next item stands one gap past it.
fn ink_w(item: &HeaderItem, measure: &dyn Measure) -> f32 {
    if item.value_slot.is_none() { return run_w(item, measure); }
    let w = |s: &str| text_w(s, measure);
    w(&item.label) + item.value.as_deref().map_or(0.0, |value| w(" \u{b7} ") + w(value))
}

const FOLD_GAP: f32 = 10.0;
const FOLD_SIZE: f32 = 22.0;

/// One right-cluster item as the stack sees it: it DRAWS [`ink_w`], a [`HeaderItem::value_slot`] is
/// its [`Item::slot`] (budget [`run_w`], never air). The one place a right-cluster item becomes a stack item, so
/// the cluster's budget ([`cluster_width`]) and its placement ([`PageHeader::layout`]) read one thing.
fn stack_item(item: &HeaderItem, measure: &dyn Measure) -> Item {
    let drawn = Item::new(ink_w(item, measure));
    if item.value_slot.is_some() { drawn.slot(run_w(item, measure)) } else { drawn }
}

/// The right cluster as stack items, in order.
fn cluster_items(items: &[HeaderItem], measure: &dyn Measure) -> Vec<Item> {
    items.iter().map(|item| stack_item(item, measure)).collect()
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
            Face { label, value: None, selected, title: false, fold: false, dimmed: false, readout: false, fade: None }
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
            fold: item.fold_glyph, dimmed: item.dimmed, readout: item.readout, fade: None,
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
            let mut face = self.face(r.slot);
            if let Slot::Right(i) = r.slot { face.fade = swap.fade_of(&self.spec.right, i, r.text.x); }
            // Inked by what is under each pixel: a label a capsule's arc is crossing wears the
            // capsule's ink on its side of the arc and the resting one on the other.
            paint_inked(p, &self.ink(capsules, r, &face, ground), |focus, selected| {
                self.draw_face_at(p, r.text, &face, (0.0, 0.0), (focus, selected));
            });
        }
        for (item, text_x, text_w, opacity) in swap.leaving(&self.spec.right, layout) {
            let face = Face { fade: Some(FaceFade { face: opacity, from_value: None, value_in: 1.0 }), ..Self::face_of(item) };
            self.draw_face_at(p, Rect::new(text_x, layout.top(), text_w, HEIGHT), &face, (0.0, 0.0), (0.0, 0.0));
        }
    }

    /// **Item `layout.items[index]` as the focused control a menu opened from**: the accent pill
    /// the focus capsule is (the plate stood out by the pop's outset), and the face inked as the
    /// focused control wears it. What [`Self::draw`] shows for a focused item, without the row
    /// around it, so a menu can keep it lit above its scrim.
    pub fn draw_opener(&self, p: Painter, layout: &HeaderLayout, index: usize, pop: f32) {
        let Some(r) = layout.items.get(index) else { return };
        let pill = focus_pill_popped(r.plate, pop);
        p.rrect(pill, pill.h * 0.5, pill.h * 0.5, crate::ACCENT);
        self.draw_face(p, layout, index, r.plate, true);
    }

    /// Draw item `layout.items[index]`'s face (label, value, fold glyph) with its plate at `to`,
    /// and no plate of its own (`focused`: the accent ink the focused control wears). The same
    /// paint as [`Self::draw`]'s, so the opener's face ([`Self::draw_opener`] is its caller) cannot
    /// drift from the row's.
    pub fn draw_face(&self, p: Painter, layout: &HeaderLayout, index: usize, to: Rect, focused: bool) {
        let Some(r) = layout.items.get(index) else { return };
        self.draw_face_at(p, r.text, &self.face(r.slot), (to.x - r.plate.x, to.y - r.plate.y), (f32::from(focused), 0.0));
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
        let y = plx_gfx::text::text_vcenter_y(theme::size::BODY, 1, text.y + dy + text.h * 0.5);
        let mut x = text.x + dx;
        // The label answers to both plates; what follows it (the separator, the value, the glyph)
        // only to the focus one: a value is full white whatever is selected.
        let label_ink = to_accent(theme::cross(rest, strong, selected.clamp(0.0, 1.0)));
        if let Ok(c) = CString::new(face.label.as_str()) {
            x += p.text(c.as_ptr(), x, y, theme::size::BODY, label_ink, 0, 0);
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
        if sep_alpha > 0.0 {
            if let Ok(sep) = CString::new(" \u{b7} ") {
                let advance = p.alpha(sep_alpha).text(sep.as_ptr(), x, y, theme::size::BODY, to_accent(rest), 0, 0);
                x += advance;
                value_end = x;
            }
        }
        for (value, opacity) in leaving.into_iter().chain(face.value.map(|value| (value, arriving))) {
            if opacity <= 0.0 { continue; }
            if let Ok(v) = CString::new(value) {
                let advance = p.alpha(opacity).text(v.as_ptr(), x, y, theme::size::BODY, to_accent(strong), 0, 0);
                // The fold glyph follows the value the face wears, not the one leaving.
                if face.value == Some(value) { value_end = x + advance; }
            }
        }
        x = value_end;
        if face.fold {
            // The run ends at `x` (a zero-width stand-in for the text before it): the glyph stands
            // where `run_w` put it, `FOLD_GAP` on.
            let mut run = Flow::new(x, 0.0);
            run.push(Item::new(0.0));
            let fold = run.push(fold_item()).expect("present");
            icons::draw(p, Icon::ChevronDown,
                Rect::new(fold.box_x, text.y + dy + (text.h - FOLD_SIZE) * 0.5, FOLD_SIZE, FOLD_SIZE), to_accent(strong));
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
    /// Set while the item rides a [`HeaderSwap`].
    fade: Option<FaceFade<'a>>,
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
        let Some((_, index)) = self.stops.iter().find(|(stop, _)| *stop == elem) else { return };
        let mut layout = self.layout.clone();
        for item in &mut layout.items {
            item.text.y -= scroll;
            item.plate.y -= scroll;
        }
        self.page.draw_opener(p, &layout, *index, pop);
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
    /// no value to cross-fade and a move is a dissolve at each x, not a cut.
    fn differs(&self, item: &HeaderItem, x: f32) -> bool {
        if self.item.readout { self.item.label != item.label || (self.x - x).abs() >= SAME_X } else { self.item.value != item.value }
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
/// leave-and-arrive, never a cut. A switch to another library's row ([`Self::begin_switch`]) shows only what the
/// new library has: a control the old one had is not drawn on it, even fading. All on [`Xfade`], the one
/// content-swap clock (`arrive`: the incoming ramp, [`Xfade::alpha`] smoothstepped), so no page keeps
/// a timer of its own. Nothing glides: a label is never drawn between its two places.
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
    pub const fn new() -> Self { Self { clock: Xfade::new(), from: Vec::new(), running: false, edge: Edge::Fade } }

    /// Is a swap on screen?
    pub fn is_active(&self) -> bool { self.running && self.clock.is_swapping() }

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
                Some(Carried { item: item.clone(), x: at.text.x, w: at.text.w })
            }).collect()
        };
        let before = carry(old);
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
            return;
        }
        self.from = before;
        self.running = true;
        self.edge = edge;
        self.clock.arrive();
    }

    /// Stop at once, the row as it is.
    pub fn end(&mut self) {
        self.from.clear();
        self.running = false;
        self.clock = Xfade::new();
    }

    /// One frame of the swap's clock: **decoration, not layout** ([`plx_machine::idle::decorative`]).
    pub fn step(&mut self, dt: f32) {
        if !self.running { return; }
        plx_machine::idle::decorative(|| { self.clock.tick(dt, true); });
        if !self.clock.is_swapping() { self.end(); }
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
        if !self.is_active() { return None; }
        let item = right.get(index).filter(|item| carried(item))?;
        let a = self.progress();
        let Some(before) = self.from.iter().find(|c| c.is(item)) else {
            return (self.edge == Edge::Fade).then_some(FaceFade { face: a, from_value: None, value_in: 1.0 });
        };
        if !before.differs(item, x) { return None; }
        if Self::moved(before, item, x) { return Some(FaceFade { face: a, from_value: None, value_in: 1.0 }); }
        let from_value = before.item.value.as_deref().map(|value| (value, 1.0 - a));
        Some(FaceFade { face: 1.0, from_value, value_in: if item.value.is_some() { a } else { 0.0 } })
    }

    /// The faces of the row before that leave: a slotted control the new row (`right`, placed as
    /// `layout`) does not have, and one it has at another x with another value. Each with where its
    /// text stood and its opacity now, falling.
    fn leaving<'a>(&'a self, right: &'a [HeaderItem], layout: &'a HeaderLayout) -> impl Iterator<Item = (&'a HeaderItem, f32, f32, f32)> + 'a {
        let active = self.is_active();
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
        let mut cache = self.0.borrow_mut();
        cache.refresh(key, || std::rc::Rc::new(build()));
        std::rc::Rc::clone(cache.row().expect("refreshed above"))
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
