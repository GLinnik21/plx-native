//! **What a scrolled card page does with the chrome around it** — ONE set of rules for the pages
//! that scroll a long grid under the tucking top chrome (the Library, its Collections page, the
//! Watchlist): the chrome tucks away once the first row has gone, a 72 px fade of the page's ground
//! takes the top edge, and a capsule restating the library's name and size appears only where the
//! page's header names libraries ([`capsule_shown`]).
//!
//! Everything here is a pure function of the scroll position and the page's row pitch, so it adds
//! no state to a screen (the tuck follows the scroll spring the page already owns, and a resting
//! page asks for no further frames). The pixels are [`crate::scroll_capsule`] (the nav pill's live
//! glass); this file owns WHEN and WHAT.
//!
//! The module lives here, not in `plx_screens`, because a screen family may not name a sibling
//! module of the crate (`ci/check-deps.sh`, "sibling").
use std::ffi::CString;

/// The scroll distance over which the chrome tucks, ending when the first row has gone.
pub const TUCK_RAMP: f32 = 120.0;

/// How far the top chrome is tucked at `scroll` (0 shown .. 1 gone). `gone_at` is the scroll at
/// which whatever stands under the chrome at rest has scrolled off: the first row's pitch, since a
/// scrolled grid rests a whole row down at the first rest. The tuck completes there and starts
/// [`TUCK_RAMP`] before it, so scroll 0 is always 0 and everything past `gone_at` is 1.
pub fn tuck(scroll: f32, gone_at: f32) -> f32 {
    let end = gone_at.max(TUCK_RAMP);
    ((scroll - (end - TUCK_RAMP)) / TUCK_RAMP).clamp(0.0, 1.0)
}

/// Does the page show the capsule? It restates the library the header row names, so only a header
/// that names libraries to choose between (`selectable`: the names in its row, the `+N` overflow
/// stop counted) has anything to restate. A one-library page and a page whose header names none
/// (the Watchlist) draw no capsule, though their chrome still tucks and their top fade stays.
pub fn capsule_shown(selectable: usize) -> bool {
    selectable >= 2
}

/// The capsule's two runs: the library's whole name (never shortened: the capsule grows to hold
/// it) and the count.
pub fn capsule_parts(name: &str, total: i64) -> (CString, CString) {
    let count = plx_platform::i18n::current().number(total);
    (CString::new(name).unwrap_or_default(), CString::new(count).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PITCH: f32 = 480.0;

    #[test]
    fn the_tuck_is_zero_at_rest_and_one_once_the_first_row_has_gone() {
        assert_eq!(tuck(0.0, PITCH), 0.0);
        assert_eq!(tuck(PITCH - TUCK_RAMP, PITCH), 0.0, "the ramp starts exactly one ramp before");
        assert_eq!(tuck(PITCH, PITCH), 1.0);
        assert_eq!(tuck(PITCH * 5.0, PITCH), 1.0);
        let mid = tuck(PITCH - TUCK_RAMP * 0.5, PITCH);
        assert!(mid > 0.0 && mid < 1.0);
        let mut last = 0.0;
        for s in 0..=PITCH as i32 {
            let t = tuck(s as f32, PITCH);
            assert!(t >= last, "monotonic in scroll");
            last = t;
        }
    }

    #[test]
    fn a_short_pitch_still_leaves_scroll_zero_untucked() {
        assert_eq!(tuck(0.0, 40.0), 0.0);
        assert_eq!(tuck(0.0, 0.0), 0.0);
    }

    #[test]
    fn a_resting_page_has_a_constant_tuck_so_it_asks_for_no_frames() {
        let (a, moving) = plx_machine::idle::scoped_motion(|| tuck(PITCH * 3.0, PITCH));
        assert_eq!(a, 1.0);
        assert!(!moving, "the tuck is derived from the scroll spring; it animates nothing itself");
    }

    #[test]
    fn the_capsule_runs_are_the_whole_name_then_the_count() {
        let (name, count) = capsule_parts("Films", 1284);
        assert_eq!(name.to_str().unwrap(), "Films");
        assert!(count.to_str().unwrap().contains("284"), "{count:?}");
        let long = "Documentaries and Concerts of the Century, Remastered In Full";
        let (name, count) = capsule_parts(long, 1284);
        assert_eq!(name.to_str().unwrap(), long, "never shortened, no ellipsis");
        assert!(count.to_str().unwrap().contains('1'));
    }

    /// One predicate for every page: a header that names no library (the Watchlist, a one-library
    /// page) and a header that names exactly one has nothing to restate; two or more do.
    #[test]
    fn the_capsule_shows_only_where_the_header_names_two_or_more_libraries() {
        assert!(!capsule_shown(0), "a header that names none has nothing to restate");
        assert!(!capsule_shown(1), "one name is not a choice between libraries");
        assert!(capsule_shown(2));
        assert!(capsule_shown(8));
    }
}
