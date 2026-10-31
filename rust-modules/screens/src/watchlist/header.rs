//! The page header: the Library's one row (`plx_ui::page_header::PageHeader`, container left 76 /
//! right 129, top 148, 56 tall, text edges x 96 and x 1771). The title "Watchlist" stands on the
//! left as plain text (`HeaderItem::title`: no plate, which means "current among siblings", and no
//! pill, which means "focusable"; design review S10); on the right a filter
//! read-out ("12 of 40", only while a Type or Show filter narrows the list, at the read-out ink and
//! one [`GAP`](plx_ui::page_header::GAP) before the control) and the one control, the Library's own
//! sort control (`HeaderItem::sort_control`): the sort icon, the chosen sort and a quiet down
//! chevron, with "Sort & Filter" as its name, carried by the item and never drawn. The criterion is
//! the SORT alone (a Type or Show filter is the read-out's "N of M", never part of the control), has
//! no slot (the control ends at the row's right text edge and keeps its chevron) and a changed one
//! swaps on the row's `HeaderSwap`, as the Library's does.
//!
//! This file DECLARES the row ([`Row::new`]); its layout, paint, motion (the focus capsule that
//! travels, the pop, the criterion's swap) and focus group are the ui's, the same ones the Library,
//! Collections and Recommended pages wear. The control a Sort & Filter menu has lifted is drawn by the
//! host's opener lift, focused, above the menu's scrim ([`Row::draw_opener`], which is
//! [`PageHeader::draw_opener`]: the one paint every header opener has), so the page's pass leaves it
//! out ([`Row::draw`] with `lifted`) and the pill stays lit for as long as its panel is up (design
//! review S12).
//!
//! The row is built here as a value ([`Row`]) from the measure, the summary and the read-out, and
//! every consumer (paint, the focus rect, the menu's anchor) reads that one value.
use plx_machine::machine::Measure;
use plx_ui::page_header::{self, HeaderFocus, HeaderItem, HeaderRow, HeaderSwap, ItemRect, PageHeader, PageHeaderSpec, Slot};
#[cfg(feature = "devtriggers")]
use plx_ui::theme;
use plx_ui::widgets::{focus_pill, CtlPop, TabStrip};
use plx_ui::{Painter, Rect};

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
    /// What the row reads of the count: the narrowed form's two numbers, nothing for the whole list
    /// (it is never drawn, so a list that grows or shrinks unfiltered changes nothing on the row).
    pub fn narrowed(count: Option<Count>) -> Option<(usize, usize)> {
        match count {
            Some(Count::Of { shown, total }) => Some((shown, total)),
            _ => None,
        }
    }

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
pub const BLOCK_H: f32 = page_header::BLOCK_H;
/// The grid's right edge: the row's right text edge.
pub const GRID_RIGHT: f32 = page_header::TEXT_X1;

/// The row laid out: the ui's [`HeaderRow`] (its one stop is the Sort & Filter control) and which
/// laid-out item that is.
pub struct Row {
    row: HeaderRow,
    sort: usize,
}

impl Row {
    /// Build the row for `summary` (the sort in force: the sort control's criterion) and the
    /// read-out `count`.
    pub fn new(measure: &dyn Measure, summary: &str, count: Option<Count>) -> Self {
        use plx_platform::i18n::msg;
        let left = vec![HeaderItem {
            label: msg::watchlist_page_title().to_owned(),
            title: true,
            ..Default::default()
        }];
        let mut right = Vec::new();
        if let Some(readout) = count.and_then(|c| c.readout()) {
            right.push(HeaderItem { label: readout, readout: true, ..Default::default() });
        }
        right.push(HeaderItem::sort_control(msg::watchlist_page_sort_filter(), summary));
        let page = PageHeader::new(PageHeaderSpec { left, right, ..Default::default() }, &[], measure);
        let layout = page.layout(plx_ui::consts::SCR_W, measure);
        let sort = layout.items.iter().position(|it| matches!(it.slot, Slot::Right(_)) && it.focusable)
            .unwrap_or(layout.items.len().saturating_sub(1));
        Self { row: HeaderRow::new(page, layout, vec![(super::page::PILL_ELEM, sort)]), sort }
    }

    /// The control's item, in document space.
    fn control(&self) -> ItemRect {
        self.row.layout.items[self.sort]
    }

    /// The control's plate (its focus rect and hit target), in document space.
    pub fn plate(&self) -> Rect {
        self.control().plate
    }

    /// The control as the focused pill draws it, [`PILL_FOCUS_OUTSET`] past the plate: where the menu's
    /// morph starts and ends when focus is on the control.
    pub fn pill(&self) -> Rect {
        focus_pill(self.plate())
    }

    /// Begin the page's `swap` from the row this one replaces ([`HeaderSwap::begin`]): the criterion
    /// a sort pick changed leaves at its own place and the new one arrives at its own, and a
    /// read-out that comes, goes or says another thing dissolves.
    pub fn begin_swap(&self, from: &Row, swap: &mut HeaderSwap) {
        swap.begin(&from.row, &self.row);
    }

    /// The row as one focus group (`id`), for the page's declaration of its header section.
    pub fn focus(&self, entry: plx_machine::machine::EntryId, id: plx_machine::machine::GroupId) -> HeaderFocus<'_> {
        self.row.focus(entry, id, (0.0, 0.0), Rect::FULL)
    }

    /// The title's box (its text and the room a plate would take), in document space.
    #[cfg(test)]
    pub fn title_box(&self) -> Rect {
        self.row.layout.items[0].plate
    }

    /// The text left edge of the right cluster (the read-out, else the control).
    #[cfg(test)]
    pub fn cluster_left(&self) -> f32 {
        self.row.layout.items.iter().filter(|it| matches!(it.slot, Slot::Right(_))).map(|it| it.text.x).fold(f32::MAX, f32::min)
    }

    /// The control's item as declared (the row's last right item), for tests.
    #[cfg(test)]
    pub fn sort_item(&self) -> &HeaderItem {
        self.row.page.spec().right.last().expect("the control is on the row")
    }

    /// The laid-out items' text and plate rects, for geometry tests.
    #[cfg(test)]
    pub fn items(&self) -> &[ItemRect] {
        &self.row.layout.items
    }

    /// One frame of the row's motion on the page's `capsules` and `pop`, from the engine's
    /// `current` focus and the page's `entry` ([`HeaderRow::step`]).
    pub fn step(&self, entry: plx_machine::machine::EntryId, current: Option<plx_machine::machine::FocusKey<u32>>, capsules: &mut TabStrip, pop: &mut CtlPop<1>, dt: f32) {
        self.row.step(entry, current, capsules, pop, dt);
    }

    /// Paint the row `dy` px from the document origin (a scrolled page: minus the scroll): the title, the read-out and the control, which the Sort & Filter menu has
    /// `lifted` out of the page when it is up (the opener lift draws the only copy). `swap` is the
    /// page's, carrying the controls over from the row before this one ([`HeaderRow::draw_with`]).
    pub fn draw(&self, p: Painter, dy: f32, capsules: &TabStrip, pop: f32, lifted: bool, swap: &HeaderSwap) {
        self.row.draw_with(p, -dy, capsules, pop, lifted.then_some(super::page::PILL_ELEM), swap);
    }

    /// The control as the focused opener of its menu, `dy` px from the document origin
    /// ([`HeaderRow::draw_opener_with`]): what the opener lift paints above the scrim. It paints
    /// through the page's `swap` as the page's pass does: a sort picked from the menu changes the
    /// control that opened it, and until the menu is gone this lift is the only thing that draws it.
    pub fn draw_opener(&self, p: Painter, dy: f32, pop: f32, swap: &HeaderSwap) {
        self.row.draw_opener_with(p, -dy, pop, super::page::PILL_ELEM, swap);
    }
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
