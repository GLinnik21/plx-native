//! Original fixed-pitch and reveal assertions, exercised against the production rail geometry.
use super::layout::{rail_geom, rail_pitch, rail_scroll_target, RAIL_PITCH};
use super::rail::on_last_column;
use plx_ui::consts::SCR_H;

    #[test]
    fn a_long_alphabet_scrolls_rather_than_squeezing_its_letters() {
        let (y_short, x_short, win_short, max_short) = rail_geom(9); // Movies, Latin
        let (y_long, x_long, win_long, max_long) = rail_geom(40); // a long script

        // same column, so the rail does not move sideways between sections (a short alphabet
        // starts at the grid top, a full one higher)…
        assert_eq!(x_short, x_long);
        assert!(y_long <= y_short);
        // …the short one is exactly as tall as its letters and never scrolls…
        assert_eq!(win_short, 9.0 * RAIL_PITCH);
        assert_eq!(max_short, 0.0);
        // …and the long one fills the band and scrolls the remainder, at the SAME pitch
        assert!(
            max_long > 0.0,
            "40 letters must not fit the band at {} px", rail_pitch(40)
        );
        assert!((win_long + max_long - 40.0 * rail_pitch(40)).abs() < 0.01);
        assert!(y_long + win_long <= SCR_H - 54.0 - 10.0);
    }

    #[test]
    fn the_reveal_rule_keeps_the_driving_letter_clear_of_both_fades() {
        const N: usize = 40;
        let (_, _, win_h, max) = rail_geom(N);
        let pitch = rail_pitch(N);
        for drive in 0..N {
            // from the top, from the bottom, and from where the previous letter left it
            for from in [0.0, max, drive as f32 * pitch] {
                let s = rail_scroll_target(from, drive, N);
                assert!(
                    (0.0..=max).contains(&s),
                    "scroll {s} escaped 0..={max} at letter {drive}"
                );
                let top = drive as f32 * pitch - s; // the letter's slot, window-relative
                let margin = if drive == 0 || drive + 1 == N {
                    0.0
                } else {
                    pitch
                };
                assert!(
                    top >= margin - 0.01 && top + pitch <= win_h - margin + 0.01,
                    "letter {drive} sits at {top}..{} in a {win_h} window (scroll {s} from {from})",
                    top + pitch,
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

#[test]
fn the_right_edge_is_the_last_column_of_whatever_the_grid_has() {
    assert!(!on_last_column(None, 6));
    assert!(!on_last_column(Some(0), 6));
    assert!(on_last_column(Some(5), 6));
    assert!(on_last_column(Some(11), 6));
    assert!(on_last_column(Some(3), 4), "the episode grid has four columns");
    assert!(!on_last_column(Some(3), 0));
}
