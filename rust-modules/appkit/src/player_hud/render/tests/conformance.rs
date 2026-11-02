//! Rendering conformance for accepted package declarations, beside the frozen native Classic census.
use super::*;
use plx_machine::machine::Measure;
use std::sync::Arc;

const REFERENCE: &str = include_str!("../../../hud_format/fixtures/reference.plxhud.json");

fn load(text: &str) -> Arc<Compiled> {
    crate::player_hud::loaded::load(text.as_bytes(), "0.0.0-test", plx_platform::i18n::english(), &FM).unwrap().compiled
}

fn frame(compiled: &Compiled) -> Census {
    drawn(compiled, Case { name: "conformance", slot: ControlSlot::Discs(Controls::NONE), focus: GROUP_SCRUB,
        btn: 0, tab: 0, paused: false, transport: true }).0
}

#[test]
fn declared_title_ink_weight_and_alpha_reach_the_painter() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let text = REFERENCE.replace(r#""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true"#,
        r#""size": "TITLE", "ink": "gold", "bold": false, "alpha": 0.5"#);
    assert_ne!(text, REFERENCE);
    let drew = frame(&load(&text));
    let title = drew.1.iter().find(|r| r.size == theme::size::TITLE && r.text.is_empty()).expect("the title run");
    assert!(!title.bold, "the package asks for regular type");
    for (got, want) in title.ink.iter().zip([242.0 / 255.0, 181.0 / 255.0, 53.0 / 255.0, 0.5]) {
        assert!((got - want).abs() < 0.001, "declared ink/alpha: {:?}", title.ink);
    }
}

#[test]
fn omitting_the_kicker_does_not_draw_a_native_kicker() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#"          { "text": { "bind": "title.kicker", "size": "CAPTION", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 1, "w": "fill" } },
"#, "");
    assert_ne!(text, REFERENCE);
    let drew = frame(&load(&text));
    assert!(!drew.1.iter().any(|r| r.text.is_empty() && r.size == theme::size::CAPTION && r.at.x == 136.0),
        "no undeclared context or episode line: {:?}", drew.1);
}

#[test]
fn a_vertical_rule_keeps_its_compiled_extent() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#"{ "id": "title", "cells": ["#,
        r#"{ "id": "vertical", "cells": [{ "rule": { "axis": "v", "length": 80, "ink": "TEXT_PRIMARY" } }] },
      { "id": "title", "cells": ["#);
    let compiled = load(&text);
    let state = compiled.lookup(0, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let rect = state.ops.iter().find_map(|op| match op { DrawOp::Rule { rect, .. } if rect.h == 80.0 => Some(*rect), _ => None }).expect("vertical rule compiled");
    let drew = frame(&compiled);
    assert!(drew.0.iter().any(|(_, painted)| *painted == rect), "the 1x80 compiled rule must reach the painter: {rect:?}");
}

#[test]
fn a_more_pill_or_button_draws_its_measured_app_label() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    for widget in ["pill", "button"] {
        let text = REFERENCE.replace(r#"{ "id": "more", "command": "open_more", "widget": "disc""#,
            &format!(r#"{{ "id": "more", "command": "open_more", "widget": "{widget}""#));
        assert_ne!(text, REFERENCE);
        let compiled = load(&text);
        let drew = frame(&compiled);
        let label = plx_platform::i18n::msg::widgets_menu_options();
        assert!(drew.1.iter().any(|r| r.text == label), "{widget}: accepted More control must be visible as {label:?}");
        let state = compiled.lookup(0, crate::hud_format::Slot::Discs, true, false, false).unwrap();
        let control = state.control("more").unwrap();
        assert!(control.rect.w > FM.width_str(label, theme::size::BODY, true), "the same label must be measured before paint");
    }
}

#[test]
fn a_custom_rest_without_any_declared_inks_is_rejected() {
    let text = REFERENCE.replace(r#""rest": "bare""#, r#""rest": "custom""#);
    let parsed = crate::hud_format::parse::parse(text.as_bytes());
    assert!(crate::hud_format::has_error(&parsed.findings), "format1.0 has no custom rest ink keys; accepting it silently draws an unrelated film");
}

#[test]
fn a_labelled_control_draws_its_declared_face_and_bare_rest() {
    let _serial = plx_base::testlock::serial();
    for widget in ["pill", "button"] {
        let text = REFERENCE.replace(r#"{ "id": "more", "command": "open_more", "widget": "disc""#,
            &format!(r#"{{ "id": "more", "command": "open_more", "widget": "{widget}""#))
            .replace(r#""face": "gear""#, r#""face": "builtin:pause""#);
        let compiled = load(&text);
        let state = compiled.lookup(0, crate::hud_format::Slot::Discs, true, false, false).unwrap();
        let rect = state.control("more").unwrap().rect;
        let drew = frame(&compiled);
        assert!(drew.2.iter().any(|(icon, r)| *icon == Icon::Pause && r.x >= rect.x && r.x + r.w <= rect.x + rect.w && (r.cy() - rect.cy()).abs() < 0.01), "{widget}: declared Pause face must be drawn beside the label");
        assert!(!drew.3.iter().any(|(_, r, a)| *a > 0.0 && r.w >= rect.w && r.h >= rect.h && r.h < rect.h + 20.0 && (r.cx() - rect.cx()).abs() < 0.01),
            "{widget}: bare rest has no plate: {:?}", drew.3);
    }
}

#[test]
fn a_scrub_preview_follows_a_moved_and_resized_track() {
    let track = Rect::new(260.0, 520.0, 1000.0, 6.0);
    let knob = super::super::super::classic::bar_x(track, 25, 100);
    assert_eq!(knob, 510.0, "the preview's knob uses the declared track, not native constants");
    for scale in [0.75, 1.0] {
        let expected = crate::scrub_preview::scaled_about(
            crate::scrub_preview::card_rect(knob, track.x, track.w, track.y, 320.0), knob, track.cy(), scale);
        assert_eq!(super::super::super::classic::scrub_preview_rect(track, knob, 320.0, scale), expected);
        // Inline motion translates the whole painter: author-space placement must retain that same offset.
        let dy = 37.0;
        let moved = Rect { y: track.y + dy, ..track };
        let shifted = super::super::super::classic::scrub_preview_rect(moved, knob, 320.0, scale);
        assert!((shifted.y - expected.y - dy).abs() < 0.001, "the card follows the bar's vertical motion");
    }
}
