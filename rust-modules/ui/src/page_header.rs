//! **PageHeader** — the one row of plain-text controls a library page wears under the top bar:
//! the library names on the left (with a "+N" for the ones that do not fit), the page's own
//! controls on the right ("Collections · 12", "Recommended", "Sort & Filter · Title ▾").
//!
//! CONTRACT FREEZE: the types and signatures here are what the library header (P4) compiles
//! against. [`PageHeader::layout`] is real (it is the geometry the tests pin); [`PageHeader::draw`]
//! is a first cut that P1 refines in place without changing a signature.
//!
//! Geometry (spec): the text edges are x 96 and x 1771; a selected plate or a focus pill overhangs
//! the text by [`OVERHANG`] (20), so the container is left 76, right 129. The row is
//! [`HEIGHT`] (56) tall at [`TOP`] (148). The right cluster is sized ONCE from its widest case
//! (`widest`), so it never moves when a value changes; the left group gets what is left.

use std::ffi::CString;

use plx_machine::machine::Measure;

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
    /// The fixed width of the right cluster (text only), measured from the widest case.
    pub cluster_w: f32,
    /// The width the left group may fill, text edge to text edge, before it would meet the
    /// cluster: what the caller packs library names (and the "+N") into.
    pub left_budget: f32,
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
            cluster_w: self.cluster_w,
            left_budget: (cluster_x0 - CLUSTER_GAP - TEXT_X0).max(0.0),
        }
    }

    /// The focusable items' rects in focus order, with their `items` index.
    pub fn item_rects(layout: &HeaderLayout) -> Vec<(usize, Rect)> {
        layout.items.iter().enumerate().filter(|(_, r)| r.focusable).map(|(i, r)| (i, r.plate)).collect()
    }

    /// Draw the row. `focus` is an index into `layout.items`.
    pub fn draw(&self, p: Painter, layout: &HeaderLayout, focus: Option<usize>) {
        for (idx, r) in layout.items.iter().enumerate() {
            let item = match r.slot {
                Slot::Left(i) => self.spec.left.get(i),
                Slot::Right(i) => self.spec.right.get(i),
                Slot::Overflow => None,
            };
            let focused = focus == Some(idx);
            let dim = item.is_some_and(|i| i.dimmed);
            if focused {
                let pill = r.plate.scaled(super::widgets::CTRL_FOCUS_SCALE);
                p.rrect(pill, pill.h * 0.5, pill.h * 0.5, super::ACCENT);
            } else if item.is_some_and(|i| i.selected_plate) {
                p.rrect(r.plate, r.plate.h * 0.5, r.plate.h * 0.5, theme::TAB_PLATE_SELECTED);
            }
            let (rest, strong) = if focused {
                (super::ACCENT_INK, super::ACCENT_INK)
            } else if item.is_some_and(|i| i.readout) {
                (theme::COUNT_FILTERED_INK, theme::COUNT_FILTERED_INK)
            } else if dim {
                (theme::with_a(theme::CONTROL_TEXT_REST, 0.6), theme::with_a(theme::CONTROL_IDLE_INK, 0.6))
            } else {
                (theme::CONTROL_TEXT_REST, theme::CONTROL_IDLE_INK)
            };
            let y = plx_gfx::text::text_vcenter_y(theme::size::BODY, 1, r.text.y + r.text.h * 0.5);
            let mut x = r.text.x;
            let label = match (r.slot, item) {
                (Slot::Overflow, _) => format!("+{}", self.spec.overflow),
                (_, Some(i)) => i.label.clone(),
                _ => String::new(),
            };
            let ink = if item.is_some_and(|i| i.selected_plate) { strong } else { rest };
            if let Ok(c) = CString::new(label) {
                x += p.text(c.as_ptr(), x, y, theme::size::BODY, ink, 0, 0);
            }
            if let Some(v) = item.and_then(|i| i.value.as_deref()) {
                if let (Ok(sep), Ok(v)) = (CString::new(" \u{b7} "), CString::new(v)) {
                    x += p.text(sep.as_ptr(), x, y, theme::size::BODY, rest, 0, 0);
                    x += p.text(v.as_ptr(), x, y, theme::size::BODY, strong, 0, 0);
                }
            }
            if item.is_some_and(|i| i.fold_glyph) {
                icons::draw(p, Icon::ChevronDown,
                    Rect::new(x + FOLD_GAP, r.text.y + (r.text.h - FOLD_SIZE) * 0.5, FOLD_SIZE, FOLD_SIZE), strong);
            }
        }
    }
}

#[cfg(test)]
#[path = "page_header_tests.rs"]
mod tests;
