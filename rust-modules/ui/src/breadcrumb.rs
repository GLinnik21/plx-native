//! **Breadcrumb** — the header row's left side on a page pushed from a library page:
//! "Films › Collections".
//!
//! The mock (`Collections.dc.html`, the header row) is a 56 px flex row at left 76 with an 8 px gap:
//! the root is a link (BODY, white 65%, 20 px of padding each side), the `›` is an aria-hidden span
//! with `margin: 0 -4px` holding a 22 px chevron at white 50%, and the page name sits on the
//! selected plate (`TAB_PLATE_SELECTED`) at full white. So the root's text starts at x 96 like a
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

use super::page_header::{HEIGHT, OVERHANG, TEXT_X0, TOP};
use super::{theme, Rect};

/// The flex gap between the root, the glyph span and the page name.
pub const GAP: f32 = 8.0;
/// The glyph span's side margin (`margin: 0 -4px`).
pub const GLYPH_MARGIN: f32 = -4.0;
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

    /// Place the three parts on the header row (y 148, 56 tall), the root's text at x 96.
    pub fn layout(&self, measure: &dyn Measure) -> CrumbLayout {
        let root_w = text_w(&self.root, false, measure);
        let page_w = text_w(&self.page, false, measure);
        let root_text = Rect::new(TEXT_X0, TOP, root_w, HEIGHT);
        let root_plate = Rect::new(TEXT_X0 - OVERHANG, TOP, root_w + 2.0 * OVERHANG, HEIGHT);
        // flex gap, then the span's negative margin on its left; the same on its right.
        let gx = root_plate.x + root_plate.w + GAP + GLYPH_MARGIN;
        let glyph = Rect::new(gx, TOP + (HEIGHT - GLYPH_SIZE) * 0.5, GLYPH_SIZE, GLYPH_SIZE);
        let px = gx + GLYPH_SIZE + GLYPH_MARGIN + GAP;
        let page_plate = Rect::new(px, TOP, page_w + 2.0 * OVERHANG, HEIGHT);
        let page_text = Rect::new(px + OVERHANG, TOP, page_w, HEIGHT);
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

    /// `gap 8` + `margin 0 -4px` on a 22 px span: the glyph starts 4 px past the root plate and the
    /// page plate starts 4 px past the glyph, so the glyph box overlaps neither plate by more than
    /// the margin and the pair reads as one phrase.
    #[test]
    fn the_glyph_sits_in_its_own_span_between_the_plates() {
        let l = Crumb::new("Films", "Collections").layout(&ShippedMeasure);
        assert_eq!(l.glyph.x - (l.root_plate.x + l.root_plate.w), GAP + GLYPH_MARGIN);
        assert_eq!(l.page_plate.x - (l.glyph.x + l.glyph.w), GAP + GLYPH_MARGIN);
        assert_eq!((l.glyph.w, l.glyph.h), (22.0, 22.0));
        assert_eq!(l.glyph.y + l.glyph.h * 0.5, l.root_plate.y + l.root_plate.h * 0.5, "centred on the row");
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

    /// The focused root pill (plate grown by the control scale) must clear the page name's plate,
    /// for a short root and for a very long one.
    #[test]
    fn the_focused_root_pill_never_reaches_the_page_plate() {
        for root in ["Movies", "LDN Films bamx23", "A Very Long Library Name Indeed Of Many Words"] {
            let l = Crumb::new(root, "Recommended").layout(&ShippedMeasure);
            let pill = l.root_plate.scaled(crate::widgets::CTRL_FOCUS_SCALE);
            assert!(pill.x + pill.w < l.page_plate.x, "{root}: pill ends {} past plate at {}", pill.x + pill.w, l.page_plate.x);
        }
    }
}
