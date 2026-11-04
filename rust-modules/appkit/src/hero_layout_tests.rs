//! Host geometry for `hero_layout`: the row accumulation, the disc-unfurl budget and the pill
//! width formula, on their own (the Detail page's own tests cover them through `HeroSet`).
use super::*;
use std::ffi::CString;

/// Every glyph `GLYPH` wide: deterministic metrics with no font.
struct Mono;
const GLYPH: f32 = 10.0;
impl Measure for Mono {
    fn width(&self, s: &CStr, _sz: i32, _bold: bool) -> f32 {
        s.to_bytes().len() as f32 * GLYPH
    }
    fn cap_h(&self, sz: i32) -> f32 {
        sz as f32
    }
    fn line_h(&self, sz: i32) -> f32 {
        sz as f32
    }
}

#[test]
fn row_rect_steps_by_width_plus_gap_from_the_margin() {
    let widths = [200.0, CD, 90.0];
    let r0 = row_rect(&widths, 0, 7.0);
    assert_eq!((r0.x, r0.y, r0.w, r0.h), (plx_ui::consts::MARGIN_X, 7.0, 200.0, CD));
    let r1 = row_rect(&widths, 1, 7.0);
    assert_eq!(r1.x, r0.x + 200.0 + 200.0 * (plx_ui::widgets::CTRL_FOCUS_SCALE - 1.0) * 0.5 + CGAP);
    assert_eq!(r1.w, CD);
    let r2 = row_rect(&widths, 2, 7.0);
    assert_eq!(r2.x, r1.x + CD + CGAP + 90.0 * (plx_ui::widgets::CTRL_FOCUS_SCALE - 1.0) * 0.5);
    assert_eq!(r2.w, 90.0);
}

#[test]
fn row_rect_past_the_end_lands_one_gap_after_the_last_control() {
    let widths = [200.0, 90.0];
    let past = row_rect(&widths, 5, 0.0);
    assert_eq!(past.x, row_rect(&widths, 1, 0.0).x + 90.0 + capsule_focus_air(90.0) + CGAP);
    assert_eq!(past.w, 90.0);
}

#[test]
fn row_rect_of_an_empty_row_is_a_zero_width_control_at_the_margin() {
    // Not reachable from the Detail row (see `row_rect`); pinned so a change is deliberate.
    let r = row_rect(&[], 0, 3.0);
    assert_eq!((r.x, r.y, r.w, r.h), (plx_ui::consts::MARGIN_X, 3.0, 0.0, CD));
}

const CHEV: Option<plx_ui::icons::Icon> = Some(plx_ui::icons::Icon::ChevronDown);

/// The trailing chevron counts as its INK (`icons::ink_x`), not its box, at a 20 px type size.
fn chevron_ink() -> f32 {
    let (l, r) = plx_ui::icons::ink_x(plx_ui::icons::Icon::ChevronDown);
    (r - l) * 20.0 * HERO_ICON_RATIO
}

#[test]
fn pill_w_adds_icon_boxes_only_when_asked() {
    let label = CString::new("Play").unwrap();
    let text = 4.0 * GLYPH;
    let plain = pill_w(&Mono, &label, 20, false, None);
    assert_eq!(plain, text + plx_ui::widgets::BTN_PILL_AIR);
    let lead = pill_w(&Mono, &label, 20, true, None);
    assert_eq!(lead, plain + 20.0 * 1.15 + 12.0);
    let both = pill_w(&Mono, &label, 20, true, CHEV);
    assert_eq!(both, plain + (20.0 * 1.15 + 12.0) + chevron_ink() + 12.0);
}

#[test]
fn disc_caps_stay_circles_when_the_verb_cannot_fit() {
    // The last control already ends at the People column: no room for any verb.
    let caps = disc_caps_with([1.0; 3], [200.0; 3], |_| FACTS_R);
    assert_eq!(caps, [CD; 3]);
}

#[test]
fn disc_caps_open_when_room_and_close_when_the_trial_row_overruns() {
    let roomy = disc_caps_with([1.0; 3], [60.0, 60.0, 60.0], |disc| 100.0 + disc.iter().sum::<f32>());
    assert!(roomy.iter().all(|w| *w > CD), "all three unfurl with room: {roomy:?}");
    // Three open discs together overrun FACTS_R although one alone would fit.
    let tight = disc_caps_with([1.0; 3], [60.0; 3], |disc| FACTS_R - 3.0 * CD - 20.0 + disc.iter().sum::<f32>());
    assert_eq!(tight, [CD; 3]);
    // A closed fraction keeps a circle.
    let shut = disc_caps_with([0.0; 3], [60.0; 3], |_| 100.0);
    assert_eq!(shut, [CD; 3]);
}

/// Plain discs retain the ordinary `CGAP`; capsules also reserve room for their focus pop.
/// and a control that is `0` wide is still on the row: it keeps a gap on each side. A disc springs
/// between `CD` and its open width, never through 0, so this is the rule's edge, not a live case.
#[test]
fn plain_controls_keep_their_gap_even_at_zero_width() {
    for widths in [[CD, CD, CD], [CD, 0.0, CD]] {
        let rects: Vec<Rect> = (0..3).map(|i| row_rect(&widths, i, 0.0)).collect();
        for pair in rects.windows(2) {
            assert_eq!(pair[1].x - (pair[0].x + pair[0].w), CGAP, "{widths:?}");
        }
        assert_eq!(rects.iter().map(|r| r.w).collect::<Vec<_>>(), widths);
    }
}

/// A pill's icon boxes and label are one run `HERO_ICON_GAP` apart; an icon the pill does not carry
/// leaves neither its box nor its gap (so no double gap beside the label).
#[test]
fn pill_w_gaps_only_the_icons_it_carries() {
    let label = CString::new("Play").unwrap();
    let text = 4.0 * GLYPH;
    let box_w = 20.0 * HERO_ICON_RATIO;
    let air = plx_ui::widgets::BTN_PILL_AIR;
    assert_eq!(pill_w(&Mono, &label, 20, false, None), text + air);
    assert_eq!(pill_w(&Mono, &label, 20, true, None), box_w + text + HERO_ICON_GAP + air);
    assert_eq!(pill_w(&Mono, &label, 20, false, CHEV), text + chevron_ink() + HERO_ICON_GAP + air);
    assert_eq!(pill_w(&Mono, &label, 20, true, CHEV), box_w + text + chevron_ink() + 2.0 * HERO_ICON_GAP + air);
}

#[test]
fn proposal_play_capsule_keeps_its_minimum_and_grows_for_translations() {
    for resumes in [false, true] {
        assert_eq!(play_width(&Mono, play_label(resumes)), 224.0);
    }
    struct Wide;
    impl Measure for Wide {
        fn width(&self, _: &CStr, _: i32, _: bool) -> f32 { 300.0 }
        fn cap_h(&self, sz: i32) -> f32 { sz as f32 }
        fn line_h(&self, sz: i32) -> f32 { sz as f32 }
    }
    assert!(play_width(&Wide, play_label(false)) > PW);
}

#[test]
fn pdf_proposal_movie_facts_use_year_and_remaining_time() {
    let d = Detail { year: 2024, aired: "2024-03-09".into(), dur_ms: 112 * 60_000, resume_ms: 74 * 60_000, ..Default::default() };
    assert_eq!(meta_facts(&d, None), ("2024".into(), Some(plx_platform::i18n::msg::browse_detail_minutes_left(38))));
    let fresh = Detail { resume_ms: 0, ..d };
    assert_eq!(meta_facts(&fresh, None), ("2024".into(), Some(plx_ui::fmt::duration(112 * 60_000))));
}

#[test]
fn pdf_proposal_episode_facts_use_a_named_month_and_validate_resume() {
    let _locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    assert_eq!(time_facts(2024, "2024-03-02", true, 48 * 60_000, 10 * 60_000),
        ("Mar 2, 2024".into(), Some("38 min left".into())));
    assert_eq!(time_facts(2024, "2024-02-30", true, 48 * 60_000, 49 * 60_000),
        ("2024".into(), Some(plx_ui::fmt::duration(48 * 60_000))));
}

#[test]
fn focused_hero_capsules_keep_twenty_pixels_clear_on_both_sides() {
    let widths = [224.0, CD, CD, 456.0, CD];
    for focused in [0, 3] {
        let frame = row_rect(&widths, focused, 0.0);
        let painted = frame.scaled(plx_ui::widgets::CTRL_FOCUS_SCALE);
        if focused > 0 {
            let previous = row_rect(&widths, focused - 1, 0.0);
            assert!((painted.x - (previous.x + previous.w) - 20.0).abs() < 0.001);
        }
        let next = row_rect(&widths, focused + 1, 0.0);
        assert!((next.x - (painted.x + painted.w) - 20.0).abs() < 0.001);
    }
    assert_eq!(row_rect(&widths, 0, 0.0).x, plx_ui::consts::MARGIN_X);
    let circle = row_rect(&[224.0, CD, CD], 2, 0.0).x;
    let barely_open = row_rect(&[224.0, CD + 0.001, CD], 2, 0.0).x;
    assert!((barely_open - circle).abs() < 0.01, "unfurl must not jump its neighbours");
}

#[test]
fn a_dense_translated_row_gives_up_focus_air_before_crossing_people() {
    let widths = [224.0, CD, CD, FACTS_R - plx_ui::consts::MARGIN_X - 224.0 - 2.0 * CD - 3.0 * CGAP - 1.0];
    let last = row_rect(&widths, 3, 0.0);
    assert!(last.x + last.w <= FACTS_R + 0.001);
    for i in 1..widths.len() {
        let previous = row_rect(&widths, i - 1, 0.0);
        let next = row_rect(&widths, i, 0.0);
        assert!(next.x - (previous.x + previous.w) >= CGAP - 0.001);
    }
}
