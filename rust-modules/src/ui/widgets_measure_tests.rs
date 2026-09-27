//! Pure text-measurement paths that bypass SDL2_ttf: diagnostic field wrapping and `StatusOverlay`/`Button` measured layout.

use super::*;
use crate::ui::machine::Measure;
#[allow(unused_imports)]
use super::test_support::*;

/// Diagnostics are support evidence: the right edge may never turn an unfamiliar opaque token
/// into a plausible-looking prefix. Unit tests have no SDL_ttf, so this specifically grades
/// the conservative fallback used by the schema/height tests.
#[test]
fn diagnostic_wrapping_preserves_even_one_oversized_word() {
    let value = "abcdefghijklmnopqrstuvwxyz";
    let width = FIELD_KEY_W + theme::space::SM + 4.0 * VAL_AVG_ADVANCE;
    let lines = value_lines(value, width);
    assert!(lines.len() > 1, "the token did not wrap: {lines:?}");
    assert!(
        lines.iter().all(|line| line.chars().count() <= 4),
        "a line escaped its frame: {lines:?}"
    );
    assert_eq!(
        lines.concat(),
        value,
        "wrapping dropped or changed evidence"
    );
}

#[test]
fn diagnostic_wrapping_preserves_bounded_sentences() {
    let value = "conservative horizon 116 s · risk 4%";
    let width = FIELD_KEY_W + theme::space::SM + 12.0 * VAL_AVG_ADVANCE;
    let lines = value_lines(value, width);
    assert_eq!(
        lines.join(" "),
        value,
        "wrapping dropped or changed evidence"
    );
    assert!(
        lines.iter().all(|line| line.chars().count() <= 12),
        "a line escaped its frame: {lines:?}"
    );
}

#[test]
fn measured_status_action_uses_shared_button_width_height_and_reason_spacing() {
    let frame = Rect::new(100.0, 200.0, 600.0, 500.0);
    let plain = StatusOverlay::new(frame, c"Unavailable", StatusKind::Failed).action(c"Try again");
    let action = plain.action_frame_measured(&StatusMetrics).unwrap();
    assert_eq!(action.w, 96.0 + BTN_PILL_AIR);
    assert_eq!(action.h, StatusOverlay::CTRL_H);
    assert_eq!(action.cx(), frame.cx());
    assert_eq!(action.y, frame.cy() + 20.0 + theme::space::LG);
    let explained = plain.reason(c"Shared source");
    let shifted = explained.action_frame_measured(&StatusMetrics).unwrap();
    assert_eq!(shifted.y - action.y, theme::space::SM + 28.0);
    let bands = explained.bands_measured(&StatusMetrics);
    let drawn = explained.action_rect(
        Button::pill_w_measured(c"Try again", STATUS_CAP_SZ, false, false, &StatusMetrics), &bands);
    assert_eq!((drawn.x, drawn.y, drawn.w, drawn.h), (shifted.x, shifted.y, shifted.w, shifted.h));
}

#[test]
fn measured_status_without_action_does_not_consult_metrics() {
    struct Unused;
    impl crate::ui::machine::Measure for Unused {
        fn width(&self, _: &core::ffi::CStr, _: i32, _: bool) -> f32 { panic!("no action") }
        fn cap_h(&self, _: i32) -> f32 { panic!("no action") }
        fn line_h(&self, _: i32) -> f32 { panic!("no action") }
    }
    assert!(StatusOverlay::new(Rect::FULL, c"Empty", StatusKind::Empty)
        .action_frame_measured(&Unused).is_none());
}

#[test]
fn measured_button_preserves_both_accessory_slots() {
    let plain = Button::pill_w_measured(c"Try again", STATUS_CAP_SZ, false, false, &StatusMetrics);
    let slot = STATUS_CAP_SZ as f32 * BTN_ICON_RATIO + BTN_ICON_GAP;
    for (leading, trailing) in [(true, false), (false, true), (true, true)] {
        let width = Button::pill_w_measured(c"Try again", STATUS_CAP_SZ, leading, trailing, &StatusMetrics);
        assert!((width - plain - slot * (u8::from(leading) + u8::from(trailing)) as f32).abs() < 0.001);
    }
}

#[test]
fn localized_key_hints_allow_reordered_keys_and_preserve_belarusian() {
    let (before, after) = key_hint_parts("Каб вярнуцца, націсніце \u{fffc}");
    assert_eq!(before.to_str().unwrap(), "Каб вярнуцца, націсніце");
    assert!(after.is_empty());
    let (before, after) = key_hint_parts("\u{fffc} — вярнуцца ў бібліятэку");
    assert!(before.is_empty());
    assert_eq!(after.to_str().unwrap(), "— вярнуцца ў бібліятэку");
}

#[test]
fn localized_key_hint_omits_spacing_for_an_empty_sentence_run() {
    let measure = crate::ui::fixture::FixtureMeasure;
    let key_only = KeyHint::translated("\u{fffc}".into(), c"BACK");
    assert_eq!(key_only.width(&measure), key_cap_w(c"BACK", &measure));
    let before = KeyHint::translated("Return \u{fffc}".into(), c"BACK");
    let after = KeyHint::translated("\u{fffc} Return".into(), c"BACK");
    assert_eq!(before.width(&measure), after.width(&measure));
    assert_eq!(before.width(&measure), key_only.width(&measure)
        + measure.width_str("Return", theme::size::CAPTION, false) + KEYCAP_GAP);
}

#[test]
fn translated_tab_width_uses_glyph_advance_instead_of_character_count() {
    struct GlyphMetrics;
    impl crate::ui::machine::Measure for GlyphMetrics {
        fn width(&self, text: &core::ffi::CStr, _: i32, _: bool) -> f32 {
            text.to_str().unwrap().chars().map(|c| if c.is_ascii() { 7.0 } else { 23.0 }).sum()
        }
        fn cap_h(&self, _: i32) -> f32 { unreachable!() }
        fn line_h(&self, _: i32) -> f32 { unreachable!() }
    }
    let latin = TabPill::width_measured("Info", theme::size::BODY, &GlyphMetrics);
    let cyrillic = TabPill::width_measured("Інфа", theme::size::BODY, &GlyphMetrics);
    assert!(cyrillic > latin, "equal character counts need different glyph widths");
    assert_eq!(cyrillic, 4.0 * 23.0 + 44.0);
}
