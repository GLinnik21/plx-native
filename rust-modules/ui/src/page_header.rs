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
/// Gap between two items' plates' text runs (plates may touch their neighbours' overhang).
pub const GAP: f32 = 2.0 * OVERHANG + theme::space::SM;
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
        + item.value.as_deref().map_or(0.0, |v| w(" \u{b7} ", false) + w(v, true))
        + if item.fold_glyph { FOLD_GAP + FOLD_SIZE } else { 0.0 }
}

const FOLD_GAP: f32 = 10.0;
const FOLD_SIZE: f32 = 22.0;

/// Width of a cluster of items laid out with [`GAP`] between them.
pub fn cluster_width(items: &[HeaderItem], measure: &dyn Measure) -> f32 {
    let runs: f32 = items.iter().map(|i| run_w(i, measure)).sum();
    runs + GAP * items.len().saturating_sub(1) as f32
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
        let mut x = TEXT_X0;
        for (i, it) in self.spec.left.iter().enumerate() {
            let w = run_w(it, measure);
            place(Slot::Left(i), x, w, it.focusable);
            x += w + GAP;
        }
        if self.spec.overflow > 0 {
            let w = run_w(&HeaderItem { label: format!("+{}", self.spec.overflow), ..Default::default() }, measure);
            place(Slot::Overflow, x, w, true);
        }
        // The right cluster keeps its measured width; its items sit flush to the right edge.
        let live = cluster_width(&self.spec.right, measure);
        let mut rx = x1 - live;
        for (i, it) in self.spec.right.iter().enumerate() {
            let w = run_w(it, measure);
            place(Slot::Right(i), rx, w, it.focusable);
            rx += w + GAP;
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
                p.rrect(r.plate, r.plate.h * 0.5, r.plate.h * 0.5, super::ACCENT);
            } else if item.is_some_and(|i| i.selected_plate) {
                p.rrect(r.plate, r.plate.h * 0.5, r.plate.h * 0.5, theme::TAB_PLATE_SELECTED);
            }
            let (rest, strong) = if focused {
                (super::ACCENT_INK, super::ACCENT_INK)
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
                    x += p.text(v.as_ptr(), x, y, theme::size::BODY, strong, 0, 1);
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
mod tests {
    use super::*;
    use std::ffi::CStr;

    struct Fixed;
    impl Measure for Fixed {
        fn width(&self, s: &CStr, _: i32, bold: bool) -> f32 { s.to_bytes().len() as f32 * if bold { 10.0 } else { 8.0 } }
        fn cap_h(&self, _: i32) -> f32 { 20.0 }
        fn line_h(&self, _: i32) -> f32 { 36.0 }
    }

    fn item(label: &str, value: Option<&str>) -> HeaderItem {
        HeaderItem { label: label.into(), value: value.map(Into::into), focusable: true, ..Default::default() }
    }

    fn header(value: &str) -> PageHeader {
        let spec = PageHeaderSpec {
            left: vec![item("Films", None), item("Shows", None)],
            overflow: 2,
            right: vec![item("Collections", Some("12")), item("Recommended", None), HeaderItem { fold_glyph: true, ..item("Sort", Some(value)) }],
        };
        let widest = [item("Collections", Some("9,999")), item("Recommended", None), HeaderItem { fold_glyph: true, ..item("Sort", Some("Date added")) }];
        PageHeader::new(spec, &widest, &Fixed)
    }

    #[test]
    fn text_edges_are_96_and_1771_and_plates_overhang_20() {
        let l = header("Date added").layout(1920.0, &Fixed);
        let first = l.items.first().unwrap();
        let last = l.items.last().unwrap();
        assert_eq!(first.text.x, 96.0);
        assert_eq!(first.plate.x, 76.0);
        assert!((last.text.x + last.text.w - 1771.0).abs() < 0.01, "{:?}", last.text);
        assert!((last.plate.x + last.plate.w - 1791.0).abs() < 0.01);
        assert_eq!((first.text.y, first.text.h), (148.0, 56.0));
    }

    #[test]
    fn right_cluster_width_does_not_change_with_the_value() {
        let a = header("Title").layout(1920.0, &Fixed);
        let b = header("Date added").layout(1920.0, &Fixed);
        assert_eq!(a.cluster_w, b.cluster_w);
        assert_eq!(a.left_budget, b.left_budget);
    }

    #[test]
    fn overflow_is_an_item_between_the_names_and_the_controls_and_read_outs_take_no_focus() {
        let h = header("Title");
        let l = h.layout(1920.0, &Fixed);
        assert_eq!(l.items[2].slot, Slot::Overflow);
        assert_eq!(l.items[3].slot, Slot::Right(0));
        let mut spec = h.spec().clone();
        spec.right.insert(0, HeaderItem { focusable: false, ..item("312 of 1,284", None) });
        let l = PageHeader::new(spec, &[], &Fixed).layout(1920.0, &Fixed);
        assert!(PageHeader::item_rects(&l).iter().all(|(i, _)| l.items[*i].slot != Slot::Right(0)));
    }
}
