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

#[test]
fn a_ladder_of_bands_starts_at_the_grid_top_forty_apart_and_adds_a_step_before_a_scale_change() {
    use super::layout::{BandRows, BAND_PITCH, BAND_RAIL_CX, BAND_STEP_GAP};
    let plain = BandRows::new(6, &[]);
    assert_eq!(plain.geom(), (244.0, BAND_RAIL_CX, 6.0 * BAND_PITCH, 0.0));
    assert_eq!((BAND_RAIL_CX, BAND_PITCH, BAND_STEP_GAP), (1834.0, 40.0, 8.0));
    assert_eq!((0..6).map(|i| plain.top(i)).collect::<Vec<_>>(), [0.0, 40.0, 80.0, 120.0, 160.0, 200.0]);
    // the extra step goes before the rows named, and only there
    let stepped = BandRows::new(5, &[3]);
    assert_eq!((0..5).map(|i| stepped.top(i)).collect::<Vec<_>>(), [0.0, 40.0, 80.0, 128.0, 168.0]);
    assert_eq!(stepped.geom().2, 5.0 * 40.0 + 8.0);
    assert_eq!(BandRows::new(5, &[0]).top(0), 0.0, "no gap above the first row");
    // one past either end continues at the pitch, which the reveal rule reads
    assert_eq!((stepped.top(-1), stepped.top(5)), (-40.0, 208.0));
}

#[test]
fn a_calendar_rails_rows_stand_where_the_board_puts_them() {
    use super::layout::{BandRows, BAND_PITCH, BAND_STEP_GAP};
    use super::rail_bands::steps;
    use plx_data::browse::bands::{plan, BandFamily};
    // 31 October 2026: Today, Oct..Jan, 2025, 2024. Rows are 40 apart from y 244, label centred in the row:
    // Today 264, Oct 304, Jan 664, then 8 more, `2025` at 712 and `2024` at 752.
    let marks: Vec<_> = plan(BandFamily::DateAdded, [2026, 10, 31], 2024).iter().map(|band| band.mark()).collect();
    let rows = BandRows::new(marks.len(), &steps(&marks));
    let centre = |row: isize| rows.geom().0 + rows.top(row) + BAND_PITCH * 0.5;
    assert_eq!([0, 1, 10, 11, 12].map(centre), [264.0, 304.0, 664.0, 712.0, 752.0]);
    assert_eq!(rows.top(11) - rows.top(10), BAND_PITCH + BAND_STEP_GAP, "one step before the first year");
    assert_eq!((1..marks.len() as isize).filter(|row| rows.top(*row) - rows.top(row - 1) != BAND_PITCH).count(), 1, "and no other");
    // the first of January has Today and then the years, a pitch apart
    let marks: Vec<_> = plan(BandFamily::DateAdded, [2026, 1, 1], 2024).iter().map(|band| band.mark()).collect();
    let rows = BandRows::new(marks.len(), &steps(&marks));
    assert_eq!((rows.top(1), rows.top(2)), (BAND_PITCH, 2.0 * BAND_PITCH));
    // Release: 2026, a step, the decades
    let marks: Vec<_> = plan(BandFamily::Release, [2026, 10, 31], 1990).iter().map(|band| band.mark()).collect();
    let rows = BandRows::new(marks.len(), &steps(&marks));
    assert_eq!((0..5).map(|row| rows.top(row)).collect::<Vec<_>>(), [0.0, 48.0, 88.0, 128.0, 168.0]);
}

#[test]
fn a_calendar_rail_of_thirty_five_rows_scrolls_in_whole_rows_and_keeps_the_driving_band_clear_of_both_fades() {
    use super::layout::{BandRows, BAND_PITCH, BAND_STEP_GAP};
    use super::rail_bands::steps;
    use plx_data::browse::bands::{plan, BandFamily};
    // Dec 31 with the oldest item added in 2003: Today, twelve months and 23 years, with the one step
    let marks: Vec<_> = plan(BandFamily::DateAdded, [2026, 12, 31], 2003).iter().map(|band| band.mark()).collect();
    assert_eq!(marks.len(), 36);
    for (name, steps_at) in [("by the marks", steps(&marks)), ("none", vec![]), ("two", vec![13, 20])] {
        let rows = BandRows::new(marks.len(), &steps_at);
        let (y, _, window, max) = rows.geom();
        assert!(max > 0.0, "{name}: 36 rows do not fit the window");
        // the window ends on a row's bottom, and one more row would pass the bottom margin
        let bottom = plx_ui::consts::SCR_H - plx_ui::consts::MARGIN_Y - super::layout::RAIL_CAP_PAD;
        assert!(y + window <= bottom + 0.01 && y + window + BAND_PITCH > bottom, "{name}: a window of {window} is not the whole rows that fit above {bottom}");
        assert!((0..marks.len() as isize).any(|row| (rows.top(row) + BAND_PITCH - window).abs() < 0.01), "{name}: the window ends in the middle of a row");
        assert!((window + max - (36.0 * BAND_PITCH + steps_at.len() as f32 * BAND_STEP_GAP)).abs() < 0.01, "{name}: the range is what does not fit");
        for drive in 0..marks.len() {
            for from in [0.0, max, rows.top(drive as isize)] {
                let scroll = rows.scroll_target(from, drive);
                assert!((0.0..=max).contains(&scroll), "{name}: scroll {scroll} escaped 0..={max} at row {drive}");
                let top = rows.top(drive as isize) - scroll; // the row, window-relative
                let (above, below) = (if drive == 0 { 0.0 } else { BAND_PITCH }, if drive + 1 == marks.len() { 0.0 } else { BAND_PITCH });
                assert!(top >= above - 0.01 && top + BAND_PITCH <= window - below + 0.01,
                    "{name}: row {drive} sits at {top}..{} in a {window} window (scroll {scroll} from {from})", top + BAND_PITCH);
            }
        }
    }
}

#[test]
fn the_bands_track_is_the_widest_pill_never_narrower_than_the_letters_and_never_into_the_posters() {
    use super::layout::{band_track_w, BAND_RAIL_CX, BAND_TRACK_LIMIT_W, GRID_RIGHT, RAIL_TRACK_W};
    assert_eq!(band_track_w(0.0), RAIL_TRACK_W, "the letters' 44 at least");
    assert_eq!(band_track_w(20.0), RAIL_TRACK_W);
    assert_eq!(band_track_w(33.0), 33.0 + 24.0, "12 px each side of the widest label");
    assert_eq!(band_track_w(400.0), BAND_TRACK_LIMIT_W);
    // a label too wide for 12 px each side gives air back, down to 4, so its pill still ends at the limit
    use super::layout::{band_pill_pad, BAND_PILL_PAD, BAND_PILL_PAD_MIN};
    assert_eq!((band_pill_pad(40.0), band_pill_pad(BAND_TRACK_LIMIT_W - 2.0 * BAND_PILL_PAD)), (BAND_PILL_PAD, BAND_PILL_PAD), "the board's 12 while it fits");
    assert_eq!(band_pill_pad(BAND_TRACK_LIMIT_W - 2.0 * BAND_PILL_PAD + 6.0), BAND_PILL_PAD - 3.0, "6 px over gives back 3 each side");
    assert_eq!((band_pill_pad(BAND_TRACK_LIMIT_W - 2.0 * BAND_PILL_PAD_MIN), band_pill_pad(400.0)), (BAND_PILL_PAD_MIN, BAND_PILL_PAD_MIN), "never under 4");
    for label in [60.0, 100.0, 105.0, 111.0, 113.0, 116.0] {
        assert!(label + 2.0 * band_pill_pad(label) <= BAND_TRACK_LIMIT_W + 0.01, "{label}");
        assert_eq!(band_track_w(label), label + 2.0 * band_pill_pad(label), "{label}");
    }
    assert!((BAND_RAIL_CX - BAND_TRACK_LIMIT_W * 0.5 - GRID_RIGHT).abs() < 0.01, "the widest track's left edge meets the posters' edge and not past it");
}
