//! The page header: the Library's one row (`plx_ui::page_header::PageHeader`, container left 76 /
//! right 129, top 148, 56 tall, text edges x 96 and x 1771). The title "Watchlist" stands on the
//! left on the selected plate, as the Collections page's crumb does; on the right a filter
//! read-out ("12 of 40", only while a Type or Show filter narrows the list, at the read-out ink and
//! [`READOUT_GAP`] before the control) and the one control, "Sort & Filter · <value> ▾".
//!
//! Labels are 65% white, the value 100% white, both regular; the focused control is the accent pill
//! at [`CTRL_FOCUS_SCALE`]. A control the Sort & Filter menu has lifted is drawn by the menu
//! ([`Row::draw_face`]), so the page's row leaves it out ([`Row::draw`] with `lifted`).
//!
//! The row is built here as a value ([`Row`]) from the measure, the summary and the read-out, and
//! every consumer (paint, the focus rect, the menu's anchor) reads that one value.
use plx_machine::machine::Measure;
use plx_ui::icons::{self, Icon};
use plx_ui::page_header::{self, HeaderItem, HeaderLayout, ItemRect, PageHeader, PageHeaderSpec, Slot};
use plx_ui::theme;
use plx_ui::widgets::CTRL_FOCUS_SCALE;
use plx_ui::{Painter, Rect};
use std::ffi::CString;

/// The count the page holds: the whole list, or what a Type / Show filter leaves of it. Only the
/// narrowed form is ever drawn (the Library's rule: "312 of 1,284" while filtered, nothing else).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// No filter narrows the list.
    All(usize),
    /// A filter narrows it: "{shown} of {total}".
    Of { shown: usize, total: usize },
}

impl Count {
    /// The read-out sentence, `None` when nothing narrows the list. Numbers group as the Library's
    /// do (`LocaleContext::number`).
    pub fn readout(&self) -> Option<String> {
        let Count::Of { shown, total } = *self else { return None };
        let locale = plx_platform::i18n::current();
        Some(plx_platform::i18n::msg::library_header_filtered_of(
            &locale.number(shown as i64),
            &locale.number(total as i64),
        ))
    }
}

/// The type rung of the debug cue under the row: LABEL, regular, [`theme::TEXT_SECONDARY`].
#[cfg(feature = "devtriggers")]
const CUE_SIZE: std::os::raw::c_int = theme::size::LABEL;

/// The row's top on the 1080 frame and the band the grid's first row sits [`page_header::HEIGHT`]
/// plus this air below.
pub const HEADER_TOP: f32 = page_header::TOP;
/// The header block: the row and the air under it, so the grid's first poster top is 96 under the
/// row's top (244), as the Library's.
pub const BLOCK_H: f32 = 96.0;
/// The grid's right edge: the row's right text edge.
pub const GRID_RIGHT: f32 = page_header::TEXT_X1;

/// The focus ring size of the control: the label size the glyph chevron is drawn at.
const FOLD_SIZE: f32 = 22.0;

/// The row laid out: the widget, its geometry in document space (the row's own top, 148) and which
/// laid-out item is the Sort & Filter control.
pub struct Row {
    page: PageHeader,
    layout: HeaderLayout,
    sort: usize,
}

impl Row {
    /// Build the row for `summary` and the read-out `count`.
    pub fn new(measure: &dyn Measure, summary: &str, count: Option<Count>) -> Self {
        use plx_platform::i18n::msg;
        let left = vec![HeaderItem {
            label: msg::watchlist_page_title().to_owned(),
            selected_plate: true,
            ..Default::default()
        }];
        let mut right = Vec::new();
        if let Some(readout) = count.and_then(|c| c.readout()) {
            right.push(HeaderItem { label: readout, readout: true, ..Default::default() });
        }
        right.push(HeaderItem {
            label: msg::watchlist_page_sort_filter().to_owned(),
            value: Some(summary.to_owned()),
            fold_glyph: true,
            focusable: true,
            ..Default::default()
        });
        let page = PageHeader::new(PageHeaderSpec { left, overflow: 0, right }, &[], measure);
        let layout = page.layout(plx_ui::consts::SCR_W, measure);
        let sort = layout.items.iter().position(|it| matches!(it.slot, Slot::Right(_)) && it.focusable)
            .unwrap_or(layout.items.len().saturating_sub(1));
        Self { page, layout, sort }
    }

    /// The control's item, in document space.
    fn control(&self) -> ItemRect {
        self.layout.items[self.sort]
    }

    /// The control's plate (its focus rect and hit target), in document space.
    pub fn plate(&self) -> Rect {
        self.control().plate
    }

    /// The control as the focused pill draws it, [`CTRL_FOCUS_SCALE`] over the plate: where the menu's
    /// morph starts and ends when focus is on the control.
    pub fn pill(&self) -> Rect {
        self.plate().scaled(CTRL_FOCUS_SCALE)
    }

    /// The title's plate, in document space.
    #[cfg(test)]
    pub fn title_plate(&self) -> Rect {
        self.layout.items[0].plate
    }

    /// The text left edge of the right cluster (the read-out, else the control).
    #[cfg(test)]
    pub fn cluster_left(&self) -> f32 {
        self.layout.items.iter().filter(|it| matches!(it.slot, Slot::Right(_))).map(|it| it.text.x).fold(f32::MAX, f32::min)
    }

    /// The laid-out items' text and plate rects, for geometry tests.
    #[cfg(test)]
    pub fn items(&self) -> &[ItemRect] {
        &self.layout.items
    }

    /// The row `dy` px from the document origin (a scrolled page: minus the scroll).
    fn shifted(&self, dy: f32) -> HeaderLayout {
        let mut layout = self.layout.clone();
        for it in &mut layout.items {
            it.text.y += dy;
            it.plate.y += dy;
        }
        layout
    }

    /// Paint the row at `dy` from the document origin: the title, the read-out, and the control
    /// (unless `lifted`: the Sort & Filter menu draws its only copy).
    pub fn draw(&self, p: Painter, dy: f32, pill_focused: bool, lifted: bool) {
        let mut layout = self.shifted(dy);
        let focus = (pill_focused && !lifted).then_some(self.sort);
        if lifted {
            layout.items.remove(self.sort);
        }
        self.page.draw(p, &layout, focus);
    }

    /// The control as the menu's morph opener face, label only (no plate of its own: the morph
    /// draws the plate), placed so its plate is `anchor` less the focus scale.
    pub fn draw_face(&self, p: Painter, anchor: Rect, focused: bool) {
        let item = self.control();
        let plate = anchor.scaled(1.0 / CTRL_FOCUS_SCALE);
        let (dx, dy) = (plate.x - item.plate.x, plate.y - item.plate.y);
        let Some(spec) = self.page.spec().right.get(match item.slot { Slot::Right(i) => i, _ => 0 }) else { return };
        let (rest, strong) = if focused {
            (plx_ui::ACCENT_INK, plx_ui::ACCENT_INK)
        } else {
            (theme::CONTROL_TEXT_REST, theme::CONTROL_IDLE_INK)
        };
        let cy = item.text.y + dy + item.text.h * 0.5;
        let y = plx_gfx::text::text_vcenter_y(theme::size::BODY, 1, cy);
        let mut x = item.text.x + dx;
        let run = |p: Painter, s: &str, x: f32, ink: [f32; 4]| -> f32 {
            CString::new(s).map_or(0.0, |c| p.text(c.as_ptr(), x, y, theme::size::BODY, ink, 0, 0))
        };
        x += run(p, &spec.label, x, rest);
        if let Some(value) = spec.value.as_deref() {
            x += run(p, " \u{b7} ", x, rest);
            run(p, value, x, strong);
        }
        let right = item.text.x + dx + item.text.w;
        icons::draw(p, Icon::ChevronDown, Rect::new(right - FOLD_SIZE, item.text.y + dy + (item.text.h - FOLD_SIZE) * 0.5, FOLD_SIZE, FOLD_SIZE), strong);
    }
}

/// The plate the menu's morph starts from and ends on: the accent while focus will return to the
/// control, nothing when focus goes elsewhere (a resting control is plain text).
pub fn pill_fill(focused: bool) -> [f32; 4] {
    if focused { plx_ui::ACCENT } else { [0.0; 4] }
}

/// **The debug build's "write blocked" cue** (Q11): one LABEL line in [`theme::TEXT_SECONDARY`] under
/// the row, in the air above the grid, while the last Watchlist write was refused by the dev-build
/// failsafe (catalog key `watchlist.page.debug_write_blocked`). Compiled only into a `devtriggers`
/// build, so nightly and stable cannot draw it (Q8: no toast, no notice for users).
#[cfg(feature = "devtriggers")]
pub fn dev_blocked_text() -> &'static std::ffi::CStr {
    plx_platform::i18n::msg::watchlist_page_debug_write_blocked_c()
}

/// Draw [`dev_blocked_text`] `dy` from the document origin: left on the title's text edge, centred
/// in the air between the row's foot and the grid's first row.
#[cfg(feature = "devtriggers")]
pub fn draw_dev_blocked(p: Painter, dy: f32) {
    let band = BLOCK_H - page_header::HEIGHT;
    let cy = HEADER_TOP + page_header::HEIGHT + band * 0.5 + dy;
    let y = plx_gfx::text::text_vcenter_y(CUE_SIZE, 0, cy);
    p.text(dev_blocked_text().as_ptr(), page_header::TEXT_X0, y, CUE_SIZE, theme::TEXT_SECONDARY, 0, 0);
}
