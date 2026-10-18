use super::scroll_chrome::*;
use plx_ui::scroll_capsule;

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
fn rows_settle_at_120_or_lower_and_the_clamp_never_pushes_down() {
    assert_eq!(settle_scroll(500.0, 130.0), 500.0, "already clear");
    assert_eq!(settle_scroll(500.0, 120.0), 500.0);
    assert_eq!(settle_scroll(500.0, 100.0), 480.0, "pulled back by the 20 px deficit");
    assert_eq!(settle_scroll(10.0, -50.0), 0.0, "never below the page top");
    assert!(SETTLE_MIN_Y >= scroll_capsule::Y + scroll_capsule::H, "clear of the capsule");
}

#[test]
fn the_capsule_label_is_name_then_count_and_shortens_the_name_not_the_count() {
    let w = |s: &str| s.chars().count() as f32 * 10.0;
    let t = capsule_text("Films", 1284, 320.0, w);
    let t = t.to_str().unwrap();
    assert!(t.starts_with("Films ") && t.ends_with("1,284") || t.ends_with("1\u{a0}284") || t.contains("1284"), "{t}");
    let long = capsule_text("Documentaries and Concerts of the Century", 1284, 200.0, w);
    let long = long.to_str().unwrap();
    assert!(w(long) <= 200.0, "{long}");
    assert!(long.contains('\u{2026}'), "{long}");
    assert!(long.contains("1"), "the count survives: {long}");
}

#[test]
fn the_capsule_label_fits_every_shipped_language_for_a_long_name() {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_machine::machine::Measure;
    let _guard = plx_base::testlock::serial();
    let m = ShippedMeasure;
    let w = |s: &str| m.width_str(s, plx_ui::theme::size::BODY, true);
    let budget = scroll_capsule::label_budget() / plx_ui::fit::HEADROOM;
    for language in plx_platform::i18n::SHIPPED {
        let _lang = plx_platform::i18n::language_on_this_thread_for_test(language);
        for name in ["Films", "Television Documentaries and Concerts", "Нарматыўныя дакументальныя фільмы"] {
            let t = capsule_text(name, 12_345, budget, w);
            assert!(w(t.to_str().unwrap()) <= budget, "{language:?} {name}: {t:?}");
        }
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
fn every_grid_row_rests_clear_of_the_capsule() {
    use super::layout::Layout;
    let lay = Layout::new(false, &[], 400, true);
    for row in 0..lay.rows {
        let rest = lay.row_y(row, lay.row_reveal(row));
        assert!(rest >= SETTLE_MIN_Y - 0.01 || lay.row_reveal(row) == 0.0, "row {row} rests at {rest}");
    }
}
