//! Original fixed-pitch and reveal assertions, exercised against the production rail geometry.
use super::layout::{rail_geom, rail_scroll_target, RAIL_PITCH};
use crate::ui::consts::SCR_H;
const GRID_TOP: f32 = 232.0;

    #[test]
    fn a_long_alphabet_scrolls_rather_than_squeezing_its_letters() {
        let (y_short, x_short, win_short, max_short) = rail_geom(9); // Movies, Latin
        let (y_long, x_long, win_long, max_long) = rail_geom(30); // a Cyrillic section

        // same origin, so the rail does not move between sections…
        assert_eq!((y_short, x_short), (y_long, x_long));
        // …the short one is exactly as tall as its letters and never scrolls…
        assert_eq!(win_short, 9.0 * RAIL_PITCH);
        assert_eq!(max_short, 0.0);
        // …and the long one fills the band and scrolls the remainder, at the SAME pitch
        assert!(
            max_long > 0.0,
            "30 letters must not fit the band at {RAIL_PITCH} px"
        );
        assert_eq!(win_long + max_long, 30.0 * RAIL_PITCH);
        assert!(win_long <= SCR_H - GRID_TOP - 40.0);
    }

    #[test]
    fn the_reveal_rule_keeps_the_driving_letter_clear_of_both_fades() {
        const N: usize = 30;
        let (_, _, win_h, max) = rail_geom(N);
        for drive in 0..N {
            // from the top, from the bottom, and from where the previous letter left it
            for from in [0.0, max, drive as f32 * RAIL_PITCH] {
                let s = rail_scroll_target(from, drive, N);
                assert!(
                    (0.0..=max).contains(&s),
                    "scroll {s} escaped 0..={max} at letter {drive}"
                );
                let top = drive as f32 * RAIL_PITCH - s; // the letter's slot, window-relative
                let margin = if drive == 0 || drive + 1 == N {
                    0.0
                } else {
                    RAIL_PITCH
                };
                assert!(
                    top >= margin - 0.01 && top + RAIL_PITCH <= win_h - margin + 0.01,
                    "letter {drive} sits at {top}..{} in a {win_h} window (scroll {s} from {from})",
                    top + RAIL_PITCH,
                );
            }
        }
    }

    #[test]
    fn a_rail_that_fits_never_scrolls() {
        for drive in 0..9 {
            assert_eq!(rail_scroll_target(0.0, drive, 9), 0.0);
        }
    }
