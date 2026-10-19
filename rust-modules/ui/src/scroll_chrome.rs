//! **What a scrolled card page does with the chrome around it**, for the pages other than the
//! Library: the top chrome tucks away once the first row has gone, a capsule keeps the page's name
//! and size on screen, a 72 px fade of the page's ground takes the top edge, and rows settle clear
//! of the capsule ([`SETTLE_MIN_Y`], a grid's snap edge).
//!
//! These are the Library's rules (`screens::library::scroll_chrome`, whose functions are private to that
//! page): the same two constants and the same two pure functions, so a page drawn with them tucks
//! and shortens its name exactly as the Library does. The pixels are [`crate::scroll_capsule`].
//! The module lives here, not in `plx_screens`, because a screen family may not name a sibling
//! module of the crate (`ci/check-deps.sh`, "sibling").
use std::ffi::CString;

/// A row's top edge never rests above this y: it stays clear of the capsule (y 54 + 64 = 118).
pub const SETTLE_MIN_Y: f32 = 120.0;
/// The scroll distance over which the chrome tucks, ending when the first row has gone.
pub const TUCK_RAMP: f32 = 120.0;

/// How far the top chrome is tucked at `scroll` (0 shown .. 1 gone). `first_row_pitch` is the
/// distance the first row takes to scroll off; the tuck completes there and starts [`TUCK_RAMP`]
/// before it, so scroll 0 is always 0 and everything past the first row is 1.
pub fn tuck(scroll: f32, first_row_pitch: f32) -> f32 {
    let end = first_row_pitch.max(TUCK_RAMP);
    ((scroll - (end - TUCK_RAMP)) / TUCK_RAMP).clamp(0.0, 1.0)
}

/// The capsule's two runs: the page's name, shortened (with an ellipsis) until the name, the gap and
/// the count fit `budget`, and the count (never cut). `name_w` measures a name at its own size and
/// weight, `count_w` the count at its.
pub fn capsule_parts(name: &str, total: i64, budget: f32, name_w: impl Fn(&str) -> f32,
    count_w: impl Fn(&str) -> f32) -> (CString, CString)
{
    let count = plx_platform::i18n::current().number(total);
    let room = budget - crate::scroll_capsule::NAME_COUNT_GAP - count_w(&count);
    let mut chars: Vec<char> = name.chars().collect();
    let mut text = name.to_owned();
    while !chars.is_empty() && name_w(&text) > room {
        chars.pop();
        let cut: String = chars.iter().collect();
        text = format!("{}\u{2026}", cut.trim_end());
    }
    (CString::new(text).unwrap_or_default(), CString::new(count).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tuck_matches_the_librarys_ramp() {
        const PITCH: f32 = 480.0;
        assert_eq!(tuck(0.0, PITCH), 0.0);
        assert_eq!(tuck(PITCH - TUCK_RAMP, PITCH), 0.0);
        assert_eq!(tuck(PITCH, PITCH), 1.0);
        assert_eq!(tuck(0.0, 40.0), 0.0, "a short pitch still leaves scroll zero untucked");
        assert!(SETTLE_MIN_Y >= crate::scroll_capsule::Y + crate::scroll_capsule::H);
    }

    #[test]
    fn the_name_shortens_and_the_count_survives() {
        let w = |s: &str| s.chars().count() as f32 * 10.0;
        let (name, count) = capsule_parts("Watchlist", 1284, 320.0, w, w);
        assert_eq!(name.to_str().unwrap(), "Watchlist");
        let (long, count2) = capsule_parts("A Very Long Watchlist Name Indeed", 1284, 200.0, w, w);
        assert!(long.to_str().unwrap().contains('\u{2026}'));
        assert_eq!(count, count2);
    }
}
