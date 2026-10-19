//! **PageHeader** — the one row of plain-text controls a library page wears under the top bar:
//! the library names on the left (with a "+N" for the ones that do not fit) or a breadcrumb
//! ("Films › Collections"), and the page's own controls on the right ("Collections · 12",
//! "Recommended", "Sort & Filter · Title ▾"). The Library, its Collections and Recommended pages
//! and the Watchlist all wear THIS row: one layout, one draw, one focus group ([`HeaderFocus`]).
//!
//! Geometry (spec): the text edges are x 96 and x 1771; a selected plate or a focus pill overhangs
//! the text by [`OVERHANG`] (20), so the container is left 76, right 129. The row is
//! [`HEIGHT`] (56) tall at [`TOP`] (148). The right cluster is sized ONCE from its widest case
//! (`widest`), so it never moves when a value changes; the left group gets what is left.
//!
//! **A breadcrumb is a configuration of the row, not a second widget.** With
//! [`PageHeaderSpec::crumb`] set, the left side is the root link (focusable, no plate of its own),
//! the `›` glyph and the page's name on the selected plate (not focusable); the placement is
//! [`Crumb::layout`]'s, so the pieces stand where the mock has them.
//!
//! **Motion is the top bar's own component.** The row paints its plates through a
//! [`TabStrip`] and its focus pop through a [`CtlPop`], both owned by the SCREEN that wears the
//! row (never by the row: a library switch replaces the whole header, and a capsule that lived in
//! it would land afresh instead of travelling). [`PageHeader::step`] steps them (the focus capsule
//! travels on the bar's spring; a changed selection lands under it, [`SelMark::Lands`]) and
//! [`PageHeader::draw`] paints plates first, then each label inked by how much of its plate the
//! capsules cover.

use std::ffi::CString;

use plx_machine::machine::{EntryId, FocusKey, GroupId, Measure};

use super::breadcrumb::{self, Crumb};
use super::page_regions::row_neighbour;
use super::screen::{At, AxisMask, Dir, EdgeRule, ElemKind, GroupKind, GroupSpec, Placed, Seat, Step};
use super::widgets::{CtlPop, SelMark, TabGround, TabStrip};
use super::{icons::{self, Icon}, theme, Painter, Rect};

/// Left text edge of the first library name.
pub const TEXT_X0: f32 = 96.0;
/// Right text edge of the last right-cluster item.
pub const TEXT_X1: f32 = 1771.0;
/// How far a plate or pill stands out past its text on each side.
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
/// The least gap between two items' text runs: the plates' overhangs and a little air. See
/// [`gap_between`] for the real one.
pub const GAP: f32 = 2.0 * OVERHANG + theme::space::XS;
/// The text-edge gap between two adjacent items whose text runs are `a` and `b` wide. [`GAP`] is
/// its floor: the two plates' overhangs plus [`theme::space::XS`] of air. On top of that comes how
/// far the focus pill of the WIDER of the two grows past its plate on each side
/// (`(CTRL_FOCUS_SCALE - 1) / 2` of the plate's width), so a focused, scaled pill never reaches its
/// neighbour's plate whatever the names are. Symmetric, and the one gap every placement and every
/// packing uses.
pub fn gap_between(a: f32, b: f32) -> f32 {
    let widest_plate = a.max(b) + 2.0 * OVERHANG;
    GAP + (super::widgets::CTRL_FOCUS_SCALE - 1.0) * 0.5 * widest_plate
}
/// Text edge to text edge between a read-out and the control after it.
pub const READOUT_GAP: f32 = 64.0;
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
    /// A menu control: a fold glyph (down chevron) follows the text.
    pub fold_glyph: bool,
    /// Takes focus. A read-out ("312 of 1,284") does not.
    pub focusable: bool,
    /// Drawn quieter (an owner handle, an unavailable control).
    pub dimmed: bool,
    /// A read-out ("24 of 27"): [`theme::COUNT_FILTERED_INK`] and [`READOUT_GAP`] before the next
    /// item.
    pub readout: bool,
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
    /// The fixed width of the right cluster (text only), measured from the widest case.
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
    /// The right cluster's width, from [`PageHeader::new`]'s widest case.
    cluster_w: f32,
}

fn run_w(item: &HeaderItem, measure: &dyn Measure) -> f32 {
    let w = |s: &str, bold: bool| {
        CString::new(s).map_or(0.0, |c| measure.width(&c, theme::size::BODY, bold))
    };
    w(&item.label, false)
        + item.value.as_deref().map_or(0.0, |v| w(" \u{b7} ", false) + w(v, false))
        + if item.fold_glyph { FOLD_GAP + FOLD_SIZE } else { 0.0 }
}

const FOLD_GAP: f32 = 10.0;
const FOLD_SIZE: f32 = 22.0;

/// How many of the leading library names fit in `budget` (text edge to text edge) once a "+N"
/// item of `more_w` is reserved for the ones that do not. All of them when they all fit with no
/// "+N". The caller (which knows the selected library and the 8-visible cap) may pick a window
/// instead; this is the plain left-to-right case.
pub fn fit_left(widths: &[f32], budget: f32, more_w: f32) -> usize {
    let span = |n: usize| widths[..n].iter().sum::<f32>()
        + widths[..n].windows(2).map(|w| gap_between(w[0], w[1])).sum::<f32>();
    if span(widths.len()) <= budget { return widths.len(); }
    // "+N" follows the last name that stays, one gap after it.
    let mut n = 0;
    while n < widths.len() {
        if span(n + 1) + gap_between(widths[n], more_w) + more_w > budget { break; }
        n += 1;
    }
    n
}

/// The space after `item` before `next`.
fn gap_after(item: &HeaderItem, next: &HeaderItem, measure: &dyn Measure) -> f32 {
    if item.readout { READOUT_GAP } else { gap_between(run_w(item, measure), run_w(next, measure)) }
}

/// Width of a cluster of items laid out with [`gap_between`] between them ([`READOUT_GAP`] after a
/// read-out).
pub fn cluster_width(items: &[HeaderItem], measure: &dyn Measure) -> f32 {
    let runs: f32 = items.iter().map(|i| run_w(i, measure)).sum();
    let gaps: f32 = items.windows(2).map(|w| gap_after(&w[0], &w[1], measure)).sum();
    runs + gaps
}

impl PageHeader {
    /// `widest` is the right cluster in its widest case (every value at its longest); the cluster
    /// takes that width whatever `spec.right` holds now.
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
        let mut left: Vec<(Slot, f32, bool)> = self.spec.left.iter().enumerate()
            .map(|(i, it)| (Slot::Left(i), run_w(it, measure), it.focusable)).collect();
        if self.spec.overflow > 0 {
            let more = HeaderItem { label: format!("+{}", self.spec.overflow), ..Default::default() };
            left.push((Slot::Overflow, run_w(&more, measure), true));
        }
        let mut x = TEXT_X0;
        for (n, &(slot, w, focusable)) in left.iter().enumerate() {
            place(slot, x, w, focusable);
            if let Some(next) = left.get(n + 1) { x += w + gap_between(w, next.1); }
        }
        // The right cluster keeps its measured width; its items sit flush to the right edge.
        let live = cluster_width(&self.spec.right, measure);
        let mut rx = x1 - live;
        for (i, it) in self.spec.right.iter().enumerate() {
            let w = run_w(it, measure);
            place(Slot::Right(i), rx, w, it.focusable);
            if let Some(next) = self.spec.right.get(i + 1) { rx += w + gap_after(it, next, measure); }
        }
        HeaderLayout {
            items,
            glyph,
            cluster_w: self.cluster_w,
            left_budget: (cluster_x0 - CLUSTER_GAP - TEXT_X0).max(0.0),
        }
    }

    /// The focusable items' rects in focus order, with their `items` index.
    pub fn item_rects(layout: &HeaderLayout) -> Vec<(usize, Rect)> {
        layout.items.iter().enumerate().filter(|(_, r)| r.focusable).map(|(i, r)| (i, r.plate)).collect()
    }

    /// What `slot` says about its item: the label (a crumb's parts and the "+N" are the row's own,
    /// not spec items) and the flags the paint reads.
    fn face(&self, slot: Slot) -> Face<'_> {
        fn of(item: &HeaderItem) -> Face<'_> {
            Face {
                label: item.label.clone(), value: item.value.as_deref(), selected: item.selected_plate,
                fold: item.fold_glyph, dimmed: item.dimmed, readout: item.readout,
            }
        }
        fn plain<'a>(label: String, selected: bool) -> Face<'a> {
            Face { label, value: None, selected, fold: false, dimmed: false, readout: false }
        }
        match slot {
            Slot::Left(i) => self.spec.left.get(i).map_or_else(|| plain(String::new(), false), of),
            Slot::Right(i) => self.spec.right.get(i).map_or_else(|| plain(String::new(), false), of),
            Slot::Overflow => plain(format!("+{}", self.spec.overflow), false),
            Slot::CrumbRoot => plain(self.spec.crumb.as_ref().map(|c| c.root.clone()).unwrap_or_default(), false),
            Slot::CrumbPage => plain(self.spec.crumb.as_ref().map(|c| c.page.clone()).unwrap_or_default(), true),
        }
    }

    /// The index in `layout.items` of the item wearing the selected plate (the library on view,
    /// the crumb's page name), `-1` for none: the `selected` the row's motion follows.
    pub fn selected(&self, layout: &HeaderLayout) -> i32 {
        layout.items.iter().position(|r| self.face(r.slot).selected).map_or(-1, |i| i as i32)
    }

    /// **One frame of the row's motion**, on the screen's own `capsules` and `pop`. The focus
    /// capsule always travels, on the top bar's spring; the selection plate LANDS under it
    /// ([`SelMark::Lands`]): selection only ever changes by pressing, under a focus capsule that is
    /// already on the pill, so the path a sliding plate would take is a grey edge crawling out from
    /// behind the accent one. `selected` and `focused` are indices into `layout.items` (`-1`: none;
    /// focus off the row closes the pop). The span a capsule follows is the item's plate, the same
    /// geometry the row is laid out and hit-tested with.
    ///
    /// **Decoration, not layout** ([`plx_machine::idle::decorative`]): the capsules and the pop move
    /// nothing but themselves, so they wake the present gate and never hold a pushed page's
    /// snapshot, which the page-freeze keeps until the page's LAYOUT stops moving.
    pub fn step(layout: &HeaderLayout, selected: i32, focused: i32, capsules: &mut TabStrip, pop: &mut CtlPop<1>, dt: f32) {
        plx_machine::idle::decorative(|| {
            capsules.update(selected, focused, |i| layout.items.get(i).map(|r| (r.plate.x, r.plate.w)), SelMark::Lands, dt);
            pop.step((focused >= 0).then_some(0), dt);
        });
    }

    /// The `(focus, selected)` ink mixes of item `r`: how much of its plate the capsules cover. A
    /// read-out is no pill and is never inked by one; a `lifted` control has no focus capsule under
    /// it, so its ink stays the resting one.
    fn mixes(&self, capsules: &TabStrip, r: &ItemRect, face: &Face<'_>, lifted: bool) -> (f32, f32) {
        if face.readout || !(r.focusable || face.selected) { return (0.0, 0.0); }
        let (focus, selected) = capsules.mixes((r.plate.x, r.plate.w));
        (if lifted { 0.0 } else { focus }, selected)
    }

    /// Draw the row: the plates first (the selection's, then the focus capsule over it, through
    /// the top bar's own [`TabStrip`]), then the `›` and every label inked by how much of its plate
    /// the capsules cover, so a label brightens as the capsule arrives and not before. `pop` is the
    /// screen's [`CtlPop`] scale for the focused control (press dip included); `lifted` is a
    /// control a menu has taken out of the page (`DrawFrame::lifted`): the focus capsule is gone
    /// from the row, and the control's label stays as plain text or goes ([`Lift`]).
    pub fn draw(&self, p: Painter, layout: &HeaderLayout, capsules: &TabStrip, pop: f32, lifted: Option<Lifted>) {
        capsules.draw(p, layout.top(), HEIGHT, TabGround::Bare { pop, focus: lifted.is_none() });
        if let Some(glyph) = layout.glyph {
            icons::draw(p, Icon::Chevron, glyph, breadcrumb::GLYPH_INK);
        }
        for (index, r) in layout.items.iter().enumerate() {
            if lifted == Some(Lifted { item: index, lift: Lift::Away }) { continue; }
            let face = self.face(r.slot);
            let mix = self.mixes(capsules, r, &face, lifted.is_some());
            self.draw_face_at(p, r, &face, (0.0, 0.0), mix);
        }
    }

    /// Draw item `layout.items[index]`'s face (label, value, fold glyph) with its plate at `to`,
    /// and no plate of its own: the face a popover's morph carries while it grows out of the
    /// control (`focused`: the accent ink the focused control wears). The same paint as
    /// [`Self::draw`]'s, so a menu's opener face cannot drift from the row's.
    pub fn draw_face(&self, p: Painter, layout: &HeaderLayout, index: usize, to: Rect, focused: bool) {
        let Some(r) = layout.items.get(index) else { return };
        self.draw_face_at(p, r, &self.face(r.slot), (to.x - r.plate.x, to.y - r.plate.y), (f32::from(focused), 0.0));
    }

    /// One item's label, value segment and fold glyph, shifted by `(dx, dy)`, inked at the mixes
    /// `(focus, selected)`: `rest` toward `strong` as the selection arrives, then all the way to
    /// the accent ink as the focus capsule covers it (focus outranks selection). `cross`, not
    /// `mix`: the resting ink is a translucent white and the accent ink is opaque, and a blend
    /// that kept the source's alpha would land the dark ink at 65%.
    fn draw_face_at(&self, p: Painter, r: &ItemRect, face: &Face<'_>, (dx, dy): (f32, f32), (focus, selected): (f32, f32)) {
        let (rest, strong) = if face.readout {
            (theme::COUNT_FILTERED_INK, theme::COUNT_FILTERED_INK)
        } else if face.dimmed {
            (theme::with_a(theme::CONTROL_TEXT_REST, 0.6), theme::with_a(theme::CONTROL_IDLE_INK, 0.6))
        } else {
            (theme::CONTROL_TEXT_REST, theme::CONTROL_IDLE_INK)
        };
        let to_accent = |ink: [f32; 4]| theme::cross(ink, super::ACCENT_INK, focus.clamp(0.0, 1.0));
        let y = plx_gfx::text::text_vcenter_y(theme::size::BODY, 1, r.text.y + dy + r.text.h * 0.5);
        let mut x = r.text.x + dx;
        // The label answers to both plates; what follows it (the separator, the value, the glyph)
        // only to the focus one: a value is full white whatever is selected.
        let label_ink = to_accent(theme::cross(rest, strong, selected.clamp(0.0, 1.0)));
        if let Ok(c) = CString::new(face.label.as_str()) {
            x += p.text(c.as_ptr(), x, y, theme::size::BODY, label_ink, 0, 0);
        }
        if let Some(v) = face.value {
            if let (Ok(sep), Ok(v)) = (CString::new(" \u{b7} "), CString::new(v)) {
                x += p.text(sep.as_ptr(), x, y, theme::size::BODY, to_accent(rest), 0, 0);
                x += p.text(v.as_ptr(), x, y, theme::size::BODY, to_accent(strong), 0, 0);
            }
        }
        if face.fold {
            icons::draw(p, Icon::ChevronDown,
                Rect::new(x + FOLD_GAP, r.text.y + dy + (r.text.h - FOLD_SIZE) * 0.5, FOLD_SIZE, FOLD_SIZE), to_accent(strong));
        }
    }
}

/// What a menu that has taken a control out of the row leaves of it on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lift {
    /// The label stays, as plain text at its resting ink (the Library's Sort & Filter over its
    /// open panel); only the focus capsule under it is gone.
    Plain,
    /// The label goes too: the menu draws the control's only copy as the morph's opener face
    /// ([`PageHeader::draw_face`]), and a second one on the page would be a ghost beside it.
    Away,
}

/// A control a menu has lifted out of the row: `item` is an index into `layout.items`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lifted {
    pub item: usize,
    pub lift: Lift,
}

/// What the paint reads of one item.
struct Face<'a> {
    label: String,
    value: Option<&'a str>,
    selected: bool,
    fold: bool,
    dimmed: bool,
    readout: bool,
}

/// **A built header row**: the widget, its geometry (document space: y is the row's own top before
/// any scroll) and which element each focusable item is. The row's placement, hit rects, focus
/// order and paint all read this one value, so they cannot disagree. Held by the page that wears
/// the row and replaced when its inputs change; the row's MOTION is not in it (see the module
/// doc), so replacing it moves nothing that is on screen.
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

    /// The face of stop `elem` with its plate at `to` and no plate of its own: the opener face a
    /// popover's morph carries while it grows out of the control (`focused`: the accent ink the
    /// focused control wears). [`PageHeader::draw_face`] for a stop named by its element.
    pub fn draw_face(&self, p: Painter, elem: u32, to: Rect, focused: bool) {
        if let Some((_, index)) = self.stops.iter().find(|(stop, _)| *stop == elem) {
            self.page.draw_face(p, &self.layout, *index, to, focused);
        }
    }

    /// Paint the row `scroll` px up from its document position (it scrolls away with its page).
    /// `lifted` is the element a menu has taken out of the row, and how ([`Lift`]).
    pub fn draw(&self, p: Painter, scroll: f32, capsules: &TabStrip, pop: f32, lifted: Option<(u32, Lift)>) {
        let mut layout = self.layout.clone();
        for item in &mut layout.items {
            item.text.y -= scroll;
            item.plate.y -= scroll;
        }
        if let Some(glyph) = &mut layout.glyph { glyph.y -= scroll; }
        let lifted = lifted.and_then(|(elem, lift)| self.stops.iter().find(|(stop, _)| *stop == elem)
            .map(|(_, item)| Lifted { item: *item, lift }));
        self.page.draw(p, &layout, capsules, pop, lifted);
    }
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
