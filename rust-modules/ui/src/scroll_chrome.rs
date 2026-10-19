//! **What a scrolled card page does with the chrome around it** — ONE set of rules for the pages
//! that scroll a long grid under the tucking top chrome (the Library, its Collections page, the
//! Watchlist): the chrome tucks away once the first row has gone, a 72 px fade of the page's ground
//! takes the top edge, and a capsule restating the library's name and size appears only where the
//! page's header names libraries ([`capsule_shown`]).
//!
//! **The chrome and the capsule hand the place over in sequence, never together.** The chrome band
//! and the capsule are two live glasses whose blur footprints meet, and two such glasses never share
//! a frame: the upper one cannot run its direct source job (a lower live glass lies inside its
//! footprint), so it captures INLINE in the visible pass, a mid-frame framebuffer copy and the
//! six-pass chain, about 1.4M GPU cycles per frame more than its direct job (measured on the
//! television: 25.6% of the ramp's frames late). So the tuck window ([`TUCK_RAMP`], ending when the
//! first row has gone) splits in two halves that share an edge: the chrome leaves in the first
//! ([`tuck`]), and only when it has gone does the capsule arrive in the second ([`capsule_alpha`]).
//! A page without a capsule keeps the same window, so a library's chrome does not tuck at two speeds
//! depending on how many libraries the server has.
//!
//! Everything here is a pure function of the scroll position and the page's row pitch, so it adds
//! no state to a screen (the tuck follows the scroll spring the page already owns, and a resting
//! page asks for no further frames). The pixels are [`crate::scroll_capsule`] (the nav pill's live
//! glass); this file owns WHEN and WHAT.
//!
//! The module lives here, not in `plx_screens`, because a screen family may not name a sibling
//! module of the crate (`ci/check-deps.sh`, "sibling").
use std::ffi::CString;

/// The scroll distance over which the top chrome and the capsule hand the place over, ending when
/// the first row has gone: the chrome leaves in its first half, the capsule arrives in its second.
pub const TUCK_RAMP: f32 = 120.0;

/// The chrome's half of [`TUCK_RAMP`]; the capsule has the other.
const CHROME_SHARE: f32 = TUCK_RAMP * 0.5;

/// How far the top chrome is tucked at `scroll` (0 shown .. 1 gone). `gone_at` is the scroll at
/// which whatever stands under the chrome at rest has scrolled off: the first row's pitch, since a
/// scrolled grid rests a whole row down at the first rest. The window [`TUCK_RAMP`] ends there;
/// the chrome tucks over its FIRST half (`gone_at - 120 ..= gone_at - 60`), so scroll 0 is always 0
/// and everything from `gone_at - 60` on is 1, which is when [`capsule_alpha`] may begin.
pub fn tuck(scroll: f32, gone_at: f32) -> f32 {
    let end = gone_at.max(TUCK_RAMP);
    ((scroll - (end - TUCK_RAMP)) / CHROME_SHARE).clamp(0.0, 1.0)
}

/// How far the capsule has arrived at `scroll` (0 absent .. 1 shown), for a page that wears one:
/// the SECOND half of the window (`gone_at - 60 ..= gone_at`), after the chrome has gone.
///
/// It is 0 unless [`tuck`] is exactly 1.0, by construction and not by the two ranges happening to
/// abut in floating point: no scroll has a chrome still on screen (a live glass) and a capsule (the
/// other), so the chrome band never meets a lower live glass and never captures inline.
pub fn capsule_alpha(scroll: f32, gone_at: f32) -> f32 {
    if tuck(scroll, gone_at) < 1.0 {
        return 0.0;
    }
    let end = gone_at.max(TUCK_RAMP);
    ((scroll - (end - CHROME_SHARE)) / CHROME_SHARE).clamp(0.0, 1.0)
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
    fn the_tuck_is_zero_at_rest_and_one_once_the_chrome_half_of_the_window_has_run() {
        assert_eq!(tuck(0.0, PITCH), 0.0);
        assert_eq!(tuck(PITCH - TUCK_RAMP, PITCH), 0.0, "the window starts exactly one ramp before");
        assert_eq!(tuck(PITCH, PITCH), 1.0);
        assert_eq!(tuck(PITCH * 5.0, PITCH), 1.0);
        let quarter = tuck(PITCH - TUCK_RAMP * 0.75, PITCH);
        assert!(quarter > 0.0 && quarter < 1.0, "the chrome is mid-tuck a quarter into the window: {quarter}");
        assert_eq!(tuck(PITCH - TUCK_RAMP * 0.5, PITCH), 1.0, "the chrome has gone by the window's middle (the capsule's half is left)");
        let mut last = 0.0;
        for s in 0..=PITCH as i32 {
            let t = tuck(s as f32, PITCH);
            assert!(t >= last, "monotonic in scroll");
            last = t;
        }
    }

    #[test]
    fn the_capsule_arrives_in_the_windows_second_half() {
        assert_eq!(capsule_alpha(0.0, PITCH), 0.0);
        assert_eq!(capsule_alpha(PITCH - TUCK_RAMP, PITCH), 0.0, "absent while the chrome starts to leave");
        assert_eq!(capsule_alpha(PITCH - TUCK_RAMP * 0.5, PITCH), 0.0, "absent at the edge the chrome is gone on");
        assert_eq!(capsule_alpha(PITCH, PITCH), 1.0);
        assert_eq!(capsule_alpha(PITCH * 5.0, PITCH), 1.0);
        let mid = capsule_alpha(PITCH - TUCK_RAMP * 0.25, PITCH);
        assert!(mid > 0.0 && mid < 1.0, "half-arrived a quarter before the end: {mid}");
        let mut last = 0.0;
        for s in 0..=PITCH as i32 * 2 {
            let a = capsule_alpha(s as f32, PITCH);
            assert!(a >= last, "monotonic in scroll");
            last = a;
        }
    }

    /// **The invariant the cost rests on**: for every scroll the capsule is absent unless the chrome
    /// has gone, so a frame never has both live glasses. Swept over the whole range, at fractional
    /// scrolls and at fractional pitches (the grid's pitch is a layout product, not an integer),
    /// and at every edge where float rounding could let the two ranges overlap.
    #[test]
    fn the_capsule_is_absent_unless_the_chrome_has_gone_at_every_scroll() {
        for pitch in [PITCH, 415.0, 415.37, 337.123_46, 200.5, 121.0, 120.0, 90.0, 0.0] {
            let end = pitch.max(TUCK_RAMP);
            let mut scrolls: Vec<f32> = (0..=(3.0 * pitch.max(1.0)) as i32 * 4).map(|s| s as f32 * 0.25).collect();
            for edge in [end - TUCK_RAMP, end - TUCK_RAMP * 0.5, end] {
                let mut s = edge;
                for _ in 0..4 { s = f32::from_bits(s.to_bits().saturating_sub(1)); }
                for _ in 0..9 { scrolls.push(s); s = f32::from_bits(s.to_bits() + 1); }
            }
            for scroll in scrolls {
                let (t, a) = (tuck(scroll, pitch), capsule_alpha(scroll, pitch));
                assert!(a == 0.0 || t == 1.0, "pitch {pitch}, scroll {scroll}: capsule {a} with the chrome at tuck {t}");
                assert!((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&a));
            }
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
