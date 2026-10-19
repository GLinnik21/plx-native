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
    assert_eq!(r1.x, r0.x + 200.0 + CGAP);
    assert_eq!(r1.w, CD);
    let r2 = row_rect(&widths, 2, 7.0);
    assert_eq!(r2.x, r1.x + CD + CGAP);
    assert_eq!(r2.w, 90.0);
}

#[test]
fn row_rect_past_the_end_lands_one_gap_after_the_last_control() {
    let widths = [200.0, 90.0];
    let past = row_rect(&widths, 5, 0.0);
    assert_eq!(past.x, plx_ui::consts::MARGIN_X + 200.0 + CGAP + 90.0 + CGAP);
    assert_eq!(past.w, 90.0);
}

#[test]
fn row_rect_of_an_empty_row_is_a_zero_width_control_at_the_margin() {
    // Not reachable from the Detail row (see `row_rect`); pinned so a change is deliberate.
    let r = row_rect(&[], 0, 3.0);
    assert_eq!((r.x, r.y, r.w, r.h), (plx_ui::consts::MARGIN_X, 3.0, 0.0, CD));
}

#[test]
fn pill_w_adds_icon_boxes_only_when_asked() {
    let label = CString::new("Play").unwrap();
    let text = 4.0 * GLYPH;
    let plain = pill_w(&Mono, &label, 20, false, false);
    assert_eq!(plain, text + plx_ui::widgets::BTN_PILL_AIR);
    let lead = pill_w(&Mono, &label, 20, true, false);
    assert_eq!(lead, plain + 20.0 * 1.15 + 12.0);
    let both = pill_w(&Mono, &label, 20, true, true);
    assert_eq!(both, plain + 2.0 * (20.0 * 1.15 + 12.0));
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

/// **The swap rule, on its own.** Two strings that trade a slot along a swap `s` (wants `1 - s` and
/// `s`): the outgoing one fades over the first half (`1 - 2s`), the incoming one over the second
/// (`2s - 1`), they are never both on screen, and the slot is entirely one or the other at both
/// ends. Alone, a string shows what it is wanted.
#[test]
fn two_strings_in_one_slot_trade_it_out_over_the_first_half_and_in_over_the_second() {
    for tick in 0..=100 {
        let s = tick as f32 / 100.0;
        let (out, inn) = (name_reveal(1.0 - s, s), name_reveal(s, 1.0 - s));
        assert!((out - (1.0 - 2.0 * s).clamp(0.0, 1.0)).abs() < 1e-5, "s={s}: out {out}");
        assert!((inn - (2.0 * s - 1.0).clamp(0.0, 1.0)).abs() < 1e-5, "s={s}: in {inn}");
        assert!(out == 0.0 || inn == 0.0, "s={s}: both strings are on screen ({out}, {inn})");
    }
    assert_eq!((name_reveal(1.0, 0.0), name_reveal(0.0, 1.0)), (1.0, 0.0));
    assert_eq!(name_reveal(0.37, 0.0), 0.37, "alone it arrives with whatever wants it");
    // Wants that are not mirror images (two springs that settle at their own pace) still never overlap.
    for a in 0..=20 {
        for b in 0..=20 {
            let (wa, wb) = (a as f32 / 20.0, b as f32 / 20.0);
            assert!(name_reveal(wa, wb) == 0.0 || name_reveal(wb, wa) == 0.0, "({wa}, {wb})");
        }
    }
}

/// `draw_names` hands each string the rule's reveal against the strongest of the OTHERS, under its own control.
#[test]
fn draw_names_reveal_each_string_against_its_rivals() {
    let ctl = |x: f32| Rect::new(x, 0.0, CD, CD);
    let wants = [
        NameWant { ctl: ctl(0.0), name: "Old", want: 0.75 },
        NameWant { ctl: ctl(80.0), name: "New", want: 0.25 },
        NameWant { ctl: ctl(160.0), name: "Idle", want: 0.0 },
    ];
    let runs = plx_ui::draw_census::capture_runs(|| draw_names(Painter::recording(), &wants, &plx_ui::fixture::FixtureMeasure));
    let alpha = |name: &str| runs.iter().find(|r| r.text == name).map(|r| r.ink[3]);
    assert!((alpha("Old").unwrap() - 0.5).abs() < 1e-5, "Old reveals 0.75 - 0.25: {runs:?}");
    assert_eq!((alpha("New"), alpha("Idle")), (None, None), "outbid and unwanted strings draw nothing");
}
