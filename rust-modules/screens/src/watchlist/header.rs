//! The page header: the title (DISPLAY 48 bold, DS8 tracking), the count just left of the pill (Q7,
//! option D) and the one `Sort & Filter · <summary>` pill (plan §1.2; boards Watchlist, WatchlistOptions).
//!
//! Board values and where they land: the title's box top 172 and the pill's centre (172 + 24 = 196)
//! are [`HEADER_TOP`] / [`HEADER_CY`]; the pill's right edge is the grid's right edge, which the
//! caller passes in [`header_frame`]. The pill is the shared [`ValueChip`] (regular label, bold
//! summary, chevron, the Library toolbar's capsule), so its height is that chip's 52 (the board
//! draws 54: shipped wins) and its ink and fill are the idle control tokens.
use plx_machine::machine::Measure;
use plx_ui::consts::MARGIN_X;
use plx_ui::icons::Icon;
use plx_ui::theme;
use plx_ui::value_chip::ValueChip;
use plx_ui::{Env, Painter, Rect, View};
use std::ffi::CString;

/// What the header shows.
pub struct HeaderFace<'a> {
    pub title: &'a str,
    /// Q7: the count just left of the pill; `None` while the list has not landed.
    pub count: Option<Count>,
    /// `WatchlistOptionsState::summary`.
    pub summary: &'a str,
    pub pill_focused: bool,
}

/// The count the header shows: the whole list, or what a Type / Show filter leaves of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Count {
    /// No filter narrows the list: "{n} titles".
    All(usize),
    /// A filter narrows it: "{shown} of {total}".
    Of { shown: usize, total: usize },
}

impl Count {
    /// The catalog's sentence for it, plural by the language's own rules.
    pub fn text(&self) -> String {
        use plx_platform::i18n::msg;
        match *self {
            Count::All(n) => msg::watchlist_page_count_all(n as i64).to_string(),
            Count::Of { shown, total } => msg::watchlist_page_count_of(shown as i64, total as i64).to_string(),
        }
    }
}

/// The type rung the count wears: LABEL, regular, [`theme::TEXT_SECONDARY`].
const COUNT_SIZE: std::os::raw::c_int = theme::size::LABEL;

/// The title's box top on the 1080 frame (boards: `top: 172px`).
pub const HEADER_TOP: f32 = 172.0;
/// The title and the pill share this centre line: the title box is `DISPLAY` tall at line-height 1.
pub const HEADER_CY: f32 = HEADER_TOP + theme::size::DISPLAY as f32 * 0.5;
/// The pill's height: the shared value chip's, as the Library toolbar draws it.
pub const PILL_H: f32 = 52.0;

/// The header band for a grid whose right edge is `grid_right`: from the title's left (`MARGIN_X`)
/// to the pill's right, `DISPLAY` tall from [`HEADER_TOP`].
pub fn header_frame(grid_right: f32) -> Rect {
    Rect::new(MARGIN_X, HEADER_TOP, grid_right - MARGIN_X, theme::size::DISPLAY as f32)
}

/// The pill's text halves: the label, and the value with its own separator (" · Default"), cut from
/// the catalog's one composed sentence so the separator is the translators'.
fn pill_text(summary: &str) -> (&'static std::ffi::CStr, CString) {
    let name = plx_platform::i18n::msg::watchlist_page_sort_filter_c();
    let full = plx_platform::i18n::msg::watchlist_page_sort_filter_summary(summary);
    let value = full.strip_prefix(name.to_str().unwrap_or("")).unwrap_or(&full);
    (name, CString::new(value).unwrap_or_default())
}

/// The pill's rect inside `frame` (the [`header_frame`]): right-aligned, centred on [`HEADER_CY`].
pub fn pill_rect(frame: Rect, measure: &dyn Measure, summary: &str) -> Rect {
    let (name, value) = pill_text(summary);
    let w = ValueChip::width(measure, name, &value, None);
    Rect::new(frame.x + frame.w - w, frame_cy(frame) - PILL_H * 0.5, w, PILL_H)
}

/// The title's and the pill's shared centre line in `frame`: [`HEADER_CY`] when the frame is the
/// document-space [`header_frame`], and the same line carried down or up by a scrolled page.
fn frame_cy(frame: Rect) -> f32 {
    frame.y + frame.h * 0.5
}

/// Where the count `text` sits in `frame`: right-aligned [`theme::space::MD`] clear of the pill,
/// centred on the pill's line. The pill's rect does not depend on it, so the pill never moves.
pub fn count_rect(frame: Rect, measure: &dyn Measure, summary: &str, text: &str) -> Rect {
    let pill = pill_rect(frame, measure, summary);
    let c = CString::new(text).unwrap_or_default();
    let w = measure.width(&c, COUNT_SIZE, false);
    Rect::new(pill.x - theme::space::MD - w, pill.cy() - COUNT_SIZE as f32 * 0.5, w, COUNT_SIZE as f32)
}

/// The title run and the count: returns the title's painted width. The count rides with the title
/// (not the pill) so it stays under the menu's scrim while the pill alone is lifted.
pub fn draw_title(p: Painter, frame: Rect, measure: &dyn Measure, face: &HeaderFace<'_>) -> f32 {
    let cy = frame_cy(frame);
    let sz = theme::size::DISPLAY;
    let y = plx_gfx::text::text_vcenter_y(sz, 1, cy);
    let title = CString::new(face.title).unwrap_or_default();
    let w = p.text_tracked(title.as_ptr(), MARGIN_X, y, sz, theme::TEXT_PRIMARY, 0, 1, theme::TRACKING_TITLE_TIGHT);
    if let Some(count) = face.count {
        let text = count.text();
        let r = count_rect(frame, measure, face.summary, &text);
        let y = plx_gfx::text::text_vcenter_y(COUNT_SIZE, 0, r.cy());
        let text = CString::new(text).unwrap_or_default();
        p.text(text.as_ptr(), r.x, y, COUNT_SIZE, theme::TEXT_SECONDARY, 0, 0);
    }
    w
}

/// **The debug build's "write blocked" cue** (Q11): one LABEL line in [`theme::TEXT_SECONDARY`] under
/// the title, in the gap above the grid, while the last Watchlist write was refused by the dev-build
/// failsafe (catalog key `watchlist.page.debug_write_blocked`). Compiled only into a `devtriggers`
/// build, so nightly and stable cannot draw it (Q8: no toast, no notice for users).
#[cfg(feature = "devtriggers")]
pub fn dev_blocked_text() -> &'static std::ffi::CStr {
    plx_platform::i18n::msg::watchlist_page_debug_write_blocked_c()
}

/// Draw [`dev_blocked_text`] for the header `frame`: left on the title's edge, centred in the band
/// between the title's foot and the grid's first row.
#[cfg(feature = "devtriggers")]
pub fn draw_dev_blocked(p: Painter, frame: Rect) {
    let band = super::page::GRID_TOP - HEADER_TOP - frame.h;
    let cy = frame.y + frame.h + band * 0.5;
    let y = plx_gfx::text::text_vcenter_y(COUNT_SIZE, 0, cy);
    p.text(dev_blocked_text().as_ptr(), MARGIN_X, y, COUNT_SIZE, theme::TEXT_SECONDARY, 0, 0);
}

/// The pill alone, at `rect` (from [`pill_rect`]).
pub fn draw_pill(p: Painter, rect: Rect, face: &HeaderFace<'_>) {
    pill_chip(rect, face, &pill_text(face.summary)).draw(&Env::inert(), p);
}

/// The pill's label and chevron with no fill of its own: while the Sort & Filter menu is up the
/// page leaves its pill out and the menu surface draws it as its morph's opener face
/// (`OptionsMenu::draw`), the fill ([`pill_fill`]) as the morph's plate and this over it.
pub fn draw_pill_label(p: Painter, rect: Rect, face: &HeaderFace<'_>) {
    pill_chip(rect, face, &pill_text(face.summary)).bare().draw(&Env::inert(), p);
}

/// The pill's fill, focused or not ([`ValueChip::fill`]).
pub fn pill_fill(focused: bool) -> [f32; 4] {
    ValueChip::fill(focused)
}

fn pill_chip<'a>(rect: Rect, face: &HeaderFace<'_>, text: &'a (&'a std::ffi::CStr, CString)) -> ValueChip<'a> {
    ValueChip::new(text.0, &text.1, None, rect).focused(face.pill_focused).icon(Icon::ChevronDown)
}

/// Draw the header into `frame` (the [`header_frame`]): the title and count, then the pill.
pub fn draw_header(p: Painter, frame: Rect, measure: &dyn Measure, face: &HeaderFace<'_>) {
    draw_title(p, frame, measure, face);
    draw_pill(p, pill_rect(frame, measure, face.summary), face);
}
