//! The page header: the title (DISPLAY 48 bold, DS8 tracking), the count beside it (Q7) and the one
//! `Sort & Filter · <summary>` pill (plan §1.2; boards Watchlist, WatchlistOptions).
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
    /// Q7: the count beside the title; `None` while the list has not landed.
    pub count: Option<usize>,
    /// `WatchlistOptionsState::summary`.
    pub summary: &'a str,
    pub pill_focused: bool,
}

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

/// The title run (and the count after it): returns the title's painted width.
pub fn draw_title(p: Painter, frame: Rect, face: &HeaderFace<'_>) -> f32 {
    let cy = frame_cy(frame);
    let sz = theme::size::DISPLAY;
    let y = plx_gfx::text::text_vcenter_y(sz, 1, cy);
    let title = CString::new(face.title).unwrap_or_default();
    let w = p.text_tracked(title.as_ptr(), MARGIN_X, y, sz, theme::TEXT_PRIMARY, 0, 1, theme::TRACKING_TITLE_TIGHT);
    if let Some(n) = face.count {
        let sz = theme::size::HEADLINE;
        let y = plx_gfx::text::baseline_y(sz, 0, theme::size::DISPLAY, 1, plx_gfx::text::text_vcenter_y(theme::size::DISPLAY, 1, cy));
        let count = CString::new(n.to_string()).unwrap_or_default();
        p.text(count.as_ptr(), MARGIN_X + w + theme::space::MD, y, sz, theme::TEXT_TERTIARY, 0, 0);
    }
    w
}

/// The pill alone, at `rect` (from [`pill_rect`]). The page passes the painter the open menu's
/// `OptionsMenu::pill_painter` hands back, so the label dissolves while the panel morphs out of it.
pub fn draw_pill(p: Painter, rect: Rect, face: &HeaderFace<'_>) {
    let (name, value) = pill_text(face.summary);
    ValueChip::new(name, &value, None, rect).focused(face.pill_focused).icon(Icon::ChevronDown).draw(&Env::inert(), p);
}

/// Draw the header into `frame` (the [`header_frame`]): the title and count, then the pill.
pub fn draw_header(p: Painter, frame: Rect, measure: &dyn Measure, face: &HeaderFace<'_>) {
    draw_title(p, frame, face);
    draw_pill(p, pill_rect(frame, measure, face.summary), face);
}
