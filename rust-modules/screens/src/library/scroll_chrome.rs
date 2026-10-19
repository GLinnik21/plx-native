//! **What a scrolled Library page does with the chrome around it** (PR7): the top chrome tucks away
//! once the first row has gone, a capsule keeps the library's name and size on screen, a 72 px
//! fade of the page's ground takes the top edge, and rows settle clear of the capsule.
//!
//! Everything here is a pure function of the scroll position and the layout's row pitch, so it
//! adds no state to the screen (the tuck follows the `K_SCROLL` scroll spring the page already
//! owns, and a resting page asks for no further frames). The pixels are `plx_ui::scroll_capsule`
//! (frozen-ground glass, never live blur); this file owns WHEN and WHAT.
use std::ffi::CString;

/// A row's top edge never rests above this y: it stays clear of the capsule (y 54 + 64 = 118).
pub(super) const SETTLE_MIN_Y: f32 = 120.0;
/// The scroll distance over which the chrome tucks, ending when the first row has gone.
pub(super) const TUCK_RAMP: f32 = 120.0;

/// How far the top chrome is tucked at `scroll` (0 shown .. 1 gone). `first_row_pitch` is the
/// distance the first row takes to scroll off; the tuck completes there and starts [`TUCK_RAMP`]
/// before it, so scroll 0 is always 0 and everything past the first row is 1.
pub(super) fn tuck(scroll: f32, first_row_pitch: f32) -> f32 {
    let end = first_row_pitch.max(TUCK_RAMP);
    ((scroll - (end - TUCK_RAMP)) / TUCK_RAMP).clamp(0.0, 1.0)
}

/// `scroll` pulled back so a row whose top edge would sit at `row_top_y` (at that scroll) rests at
/// [`SETTLE_MIN_Y`] or lower. Never below 0.
pub(super) fn settle_scroll(scroll: f32, row_top_y: f32) -> f32 {
    (scroll - (SETTLE_MIN_Y - row_top_y).max(0.0)).max(0.0)
}

/// The capsule's two runs: the library's name, shortened (with an ellipsis) until the name, the gap
/// and the count fit `budget`, and the count (never cut). `name_w` measures a name at its own size
/// and weight, `count_w` the count at its.
pub(super) fn capsule_parts(name: &str, total: i64, budget: f32, name_w: impl Fn(&str) -> f32,
    count_w: impl Fn(&str) -> f32) -> (CString, CString)
{
    let count = plx_platform::i18n::current().number(total);
    let room = budget - plx_ui::scroll_capsule::NAME_COUNT_GAP - count_w(&count);
    let mut chars: Vec<char> = name.chars().collect();
    let mut text = name.to_owned();
    while !chars.is_empty() && name_w(&text) > room {
        chars.pop();
        let cut: String = chars.iter().collect();
        text = format!("{}\u{2026}", cut.trim_end());
    }
    (CString::new(text).unwrap_or_default(), CString::new(count).unwrap_or_default())
}

/// Focus on the grid's last column, one Right press from the rail: the rail shows itself ahead of
/// the press (`RailHint::right_edge`).
pub(super) fn on_last_column(grid_index: Option<usize>, cols: usize) -> bool {
    cols > 0 && grid_index.is_some_and(|index| index % cols == cols - 1)
}
