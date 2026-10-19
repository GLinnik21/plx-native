//! **What a scrolled Library page does with the chrome around it** (PR7): the top chrome tucks away
//! once the first row has gone, a capsule keeps the library's name and size on screen, a 72 px
//! fade of the page's ground takes the top edge, and rows settle at the first row's resting y (`Layout::row_reveal`), well clear of the capsule.
//!
//! Everything here is a pure function of the scroll position and the layout's row pitch, so it
//! adds no state to the screen (the tuck follows the `K_SCROLL` scroll spring the page already
//! owns, and a resting page asks for no further frames). The pixels are `plx_ui::scroll_capsule`
//! (the nav pill's live glass); this file owns WHEN and WHAT.
use std::ffi::CString;

/// The scroll distance over which the chrome tucks, ending when the first row has gone.
pub(super) const TUCK_RAMP: f32 = 120.0;

/// How far the top chrome is tucked at `scroll` (0 shown .. 1 gone). `first_row_pitch` is the
/// distance the first row takes to scroll off; the tuck completes there and starts [`TUCK_RAMP`]
/// before it, so scroll 0 is always 0 and everything past the first row is 1.
pub(super) fn tuck(scroll: f32, first_row_pitch: f32) -> f32 {
    let end = first_row_pitch.max(TUCK_RAMP);
    ((scroll - (end - TUCK_RAMP)) / TUCK_RAMP).clamp(0.0, 1.0)
}

/// The capsule's two runs: the library's whole name (never shortened: the capsule grows to hold
/// it) and the count.
pub(super) fn capsule_parts(name: &str, total: i64) -> (CString, CString) {
    let count = plx_platform::i18n::current().number(total);
    (CString::new(name).unwrap_or_default(), CString::new(count).unwrap_or_default())
}

/// Does the page show the capsule? It restates the library the header row names, so only a row
/// that shows library names (2+ libraries) has anything to restate; a one-library page draws no
/// capsule, though its chrome still tucks and its top fade stays.
pub(super) fn capsule_shown(names_in_header: usize) -> bool {
    names_in_header > 0
}

/// Focus on the grid's last column, one Right press from the rail: the rail shows itself ahead of
/// the press (`RailHint::right_edge`).
pub(super) fn on_last_column(grid_index: Option<usize>, cols: usize) -> bool {
    cols > 0 && grid_index.is_some_and(|index| index % cols == cols - 1)
}
