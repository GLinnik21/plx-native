use super::*;

#[test]
fn the_capsule_is_the_boards_320_by_64_at_96_54() {
    let r = rect();
    assert_eq!((r.x, r.y, r.w, r.h), (96.0, 54.0, 320.0, 64.0));
    assert_eq!(RADIUS, 32.0);
    assert!(label_budget() > 0.0 && label_budget() < W);
}

#[test]
fn the_flat_form_is_the_ground_at_55_percent_and_never_black() {
    let t = tint([0.2, 0.3, 0.4]);
    assert_eq!(t, [0.2, 0.3, 0.4, 0.55]);
}

#[test]
fn the_fade_runs_from_opaque_ground_to_clear_in_the_ground_colour() {
    let (top, bottom) = fade_stops([0.2, 0.3, 0.4]);
    assert_eq!(&top[..3], &bottom[..3], "same hue at both stops: no grey band through black");
    assert_eq!((top[3], bottom[3]), (1.0, 0.0));
    assert_eq!(FADE_H, 72.0);
}

#[test]
fn the_capsule_stands_on_frozen_ground_and_declares_no_live_blur() {
    use crate::popover_glass::tests::{declared_all, latched, unlatch};
    let _g = plx_base::testlock::serial();
    let label = std::ffi::CString::new("Films 1,284").unwrap();
    let tex = 9_751;
    let field = latched(tex);
    let all = declared_all(|| {
        draw(Painter::root(), &label, [0.2; 3], Some(&field), 1.0);
        draw_top_fade(Painter::root(), 1920.0, [0.2; 3], 1.0);
    });
    assert_eq!(all.iter().filter(|p| p.kind() == "glass_frozen").count(), 1, "one frozen glass");
    assert!(all.iter().all(|p| p.kind() != "backdrop"), "no live blur source");
    unlatch(tex);
    // Without a latched field (a page's own frame) it is the flat form: still no live glass.
    let flat = declared_all(|| draw(Painter::root(), &label, [0.2; 3], None, 1.0));
    assert!(flat.iter().all(|p| p.kind() != "glass_frozen" && p.kind() != "backdrop"));
}

#[test]
fn a_zero_tuck_draws_nothing() {
    use crate::popover_glass::tests::declared_all;
    let _g = plx_base::testlock::serial();
    let label = std::ffi::CString::new("Films 1,284").unwrap();
    let all = declared_all(|| {
        draw(Painter::root(), &label, [0.2; 3], None, 0.0);
        draw_top_fade(Painter::root(), 1920.0, [0.2; 3], 0.0);
    });
    assert!(all.is_empty());
}

#[test]
fn the_tuck_fades_the_chrome_out_and_lifts_it_by_its_own_height() {
    use crate::screen::{chrome_lift, NavPresentation};
    let nav = NavPresentation { chrome_alpha: 0.8, ..Default::default() };
    assert_eq!(nav.tucked(0.0), nav, "an untucked page changes nothing");
    assert!((nav.tucked(0.25).chrome_alpha - 0.6).abs() < 1e-6);
    assert_eq!(nav.tucked(1.0).chrome_alpha, 0.0);
    assert_eq!(nav.tucked(7.0).chrome_alpha, 0.0, "clamped");
    assert_eq!(chrome_lift(0.0), 0.0);
    assert_eq!(chrome_lift(1.0), crate::widgets::TOP_BAR_BOTTOM);
}
