//! **Breadcrumb** — the header row's left side on a page pushed from a library page:
//! "Films › Collections".
//!
//! The mock (`Collections.dc.html`, the header row) is a 56 px flex row at left 76:
//! the root is a link (BODY, white 65%, 20 px of padding each side), the `›` is an aria-hidden span
//! holding a 22 px chevron at white 50%, and the page name sits on the
//! selected plate (`TAB_PLATE_SELECTED`) at full white. The mock's 8 px gap and `-4px` margin put the
//! chevron 4 px from the root's plate, which the root's focused pill then covers; here the chevron
//! stands [`AIR`] clear of that pill and of the page plate (design review S9). So the root's text starts at x 96 like a
//! library name on the Library header, and the page name's plate is the one object the eye reads as
//! "you are here".
//!
//! Only the root takes focus; OK on it returns to the page below (the caller's Back). The page name
//! is the title, not a control. The glyph is centred in its own span, so it never rides the text
//! baseline: it is centred on the row's midline.
//!
//! This module is the crumb's GEOMETRY. The crumb is drawn by [`crate::page_header::PageHeader`]
//! (set [`PageHeaderSpec::crumb`](crate::page_header::PageHeaderSpec::crumb)): it is the header
//! row's left side, so it has the row's one draw, motion and focus group.

use std::ffi::CString;

use plx_machine::machine::Measure;

use super::hstack::{Align, HStack, Item};
use super::page_header::{HEIGHT, OVERHANG, TEXT_X0, TOP};
use super::widgets::focus_pill;
use super::{theme, Rect};

/// The air each side of the glyph's box: between the root's FOCUSED pill (its plate plus the focus
/// outset) and the glyph, and between the glyph and the page name's plate. The same air
/// [`page_header::GAP`] leaves between two neighbours, so the chevron reads as a separator with room
/// either side and not as part of the pill it follows (design review S9: it stood 4 px past the
/// plate, ~8 px from the glyph's ink, and a focused pill covered most of that).
pub const AIR: f32 = theme::space::SM;
/// The chevron's box.
pub const GLYPH_SIZE: f32 = 22.0;
/// The root's ink: white at 65%, the header's resting control text.
pub const ROOT_INK: [f32; 4] = theme::CONTROL_TEXT_REST;
/// The chevron's ink: white at 50%.
pub const GLYPH_INK: [f32; 4] = theme::COUNT_FILTERED_INK;

/// The crumb's two labels.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Crumb {
    /// The page below ("Films"): the link.
    pub root: String,
    /// This page ("Collections"): the title on the selected plate.
    pub page: String,
}

/// Where each part stands. `*_text` is the text run's box; the plates reach [`OVERHANG`] past it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CrumbLayout {
    pub root_text: Rect,
    /// The root's focus and hit rect.
    pub root_plate: Rect,
    pub glyph: Rect,
    pub page_text: Rect,
    /// The selected plate under the page name.
    pub page_plate: Rect,
}

impl CrumbLayout {
    /// The right edge of the whole crumb (the page plate's), for whatever follows it.
    pub fn right(&self) -> f32 {
        self.page_plate.x + self.page_plate.w
    }
}

fn text_w(s: &str, bold: bool, measure: &dyn Measure) -> f32 {
    CString::new(s).map_or(0.0, |c| measure.width(&c, theme::size::BODY, bold))
}

impl Crumb {
    pub fn new(root: impl Into<String>, page: impl Into<String>) -> Self {
        Self { root: root.into(), page: page.into() }
    }

    /// Place the three parts on the header row (y 148, 56 tall), the root's text at x 96. The parts
    /// are one [`HStack`] [`AIR`] apart: the root's FOCUSED pill (its plate and the focus outset,
    /// which is the edge the eye reads against the chevron), the chevron's box, and the page name's
    /// plate.
    pub fn layout(&self, measure: &dyn Measure) -> CrumbLayout {
        let root_w = text_w(&self.root, false, measure);
        let page_w = text_w(&self.page, false, measure);
        let root_text = Rect::new(TEXT_X0, TOP, root_w, HEIGHT);
        let root_plate = Rect::new(TEXT_X0 - OVERHANG, TOP, root_w + 2.0 * OVERHANG, HEIGHT);
        let page_plate_w = page_w + 2.0 * OVERHANG;
        let pill = focus_pill(root_plate);
        let parts = [Item::new(pill.w), Item::new(GLYPH_SIZE), Item::new(page_plate_w)];
        let mut at = [pill.x; 3];
        for span in HStack::new(&parts, AIR, Align::Leading(pill.x)).placed() {
            at[span.i] = span.x;
        }
        let glyph = Rect::new(at[1], TOP + (HEIGHT - GLYPH_SIZE) * 0.5, GLYPH_SIZE, GLYPH_SIZE);
        let page_plate = Rect::new(at[2], TOP, page_plate_w, HEIGHT);
        let page_text = Rect::new(page_plate.x + OVERHANG, TOP, page_w, HEIGHT);
        CrumbLayout { root_text, root_plate, glyph, page_text, page_plate }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_base::fontcov::advances::ShippedMeasure;

    #[test]
    fn the_root_text_starts_at_96_and_its_plate_overhangs_by_20() {
        let l = Crumb::new("Films", "Collections").layout(&ShippedMeasure);
        assert_eq!(l.root_text.x, 96.0);
        assert_eq!(l.root_plate.x, 76.0);
        assert_eq!(l.root_plate.w, l.root_text.w + 40.0);
        assert_eq!((l.root_plate.y, l.root_plate.h), (148.0, 56.0));
        assert_eq!((l.page_plate.y, l.page_plate.h), (148.0, 56.0));
    }

    /// **S9.** The chevron is a separator measured from the root's FOCUSED pill: [`AIR`] (`space::SM`)
    /// of air between the pill's extent and the glyph's box, and the same between the box and the
    /// page name's plate. The glyph box is its own 22 px square, centred on the row.
    #[test]
    fn the_glyph_stands_sm_clear_of_the_focused_root_pill_and_of_the_page_plate() {
        for root in ["Movies", "Films", "A Very Long Library Name Indeed Of Many Words"] {
            let l = Crumb::new(root, "Collections").layout(&ShippedMeasure);
            let pill = crate::widgets::focus_pill(l.root_plate);
            assert_eq!(AIR, theme::space::SM);
            assert!((l.glyph.x - (pill.x + pill.w) - AIR).abs() < 0.01, "{root}: {} px from the focused pill", l.glyph.x - (pill.x + pill.w));
            assert!((l.page_plate.x - (l.glyph.x + l.glyph.w) - AIR).abs() < 0.01, "{root}: {} px to the page plate", l.page_plate.x - (l.glyph.x + l.glyph.w));
            assert_eq!((l.glyph.w, l.glyph.h), (22.0, 22.0));
            assert_eq!(l.glyph.y + l.glyph.h * 0.5, l.root_plate.y + l.root_plate.h * 0.5, "centred on the row");
        }
    }

    #[test]
    fn the_page_plate_wraps_its_name_and_the_crumb_ends_at_it() {
        let l = Crumb::new("Shows", "Collections").layout(&ShippedMeasure);
        assert_eq!(l.page_text.x - l.page_plate.x, OVERHANG);
        assert_eq!(l.page_plate.w, l.page_text.w + 40.0);
        assert_eq!(l.right(), l.page_plate.x + l.page_plate.w);
    }

    #[test]
    fn a_longer_root_pushes_the_rest_right_by_exactly_its_width() {
        let a = Crumb::new("Films", "Collections").layout(&ShippedMeasure);
        let b = Crumb::new("Films and Documentaries", "Collections").layout(&ShippedMeasure);
        let grew = b.root_text.w - a.root_text.w;
        assert!(grew > 0.0);
        assert!((b.page_plate.x - a.page_plate.x - grew).abs() < 0.01);
    }
}
