//! Rendering conformance for accepted package declarations, beside the frozen native Classic census.
use super::*;
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
