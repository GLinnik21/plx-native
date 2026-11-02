//! Rendering conformance for accepted package declarations, beside the frozen native Classic census.
use super::*;
use plx_machine::machine::Measure;
use std::sync::Arc;

const REFERENCE: &str = include_str!("../../../hud_format/fixtures/reference.plxhud.json");

fn load(text: &str) -> Arc<Compiled> {
    let parsed = crate::hud_format::parse::parse(text.as_bytes());
    assert!(parsed.findings.is_empty(), "{:?}", parsed.findings);
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

#[test]
fn labelled_control_width_counts_only_the_glyphs_it_draws() {
    let _serial = plx_base::testlock::serial();
    for widget in ["pill", "button"] {
        for glyph in [false, true] {
            let text = REFERENCE.replace(r#"{ "id": "more", "command": "open_more", "widget": "disc""#,
                &format!(r#"{{ "id": "more", "command": "open_more", "widget": "{widget}""#));
            let text = if glyph { text } else { text.replace(r#", "face": "gear""#, "") };
            let compiled = load(&text);
            let state = compiled.lookup(0, crate::hud_format::Slot::Discs, true, false, false).unwrap();
            let label = CString::new(plx_platform::i18n::msg::widgets_menu_options()).unwrap();
            let expected = if widget == "pill" && !glyph { TabPill::width_measured(label.to_str().unwrap(), theme::size::BODY, &FM) }
                else { plx_ui::widgets::Button::pill_w_measured(&label, theme::size::BODY, glyph, None, &FM) };
            assert_eq!(state.control("more").unwrap().rect.w, expected, "{widget}, glyph {glyph}: no phantom leading/trailing icon");
        }
    }
}

fn info_frame(compiled: &Compiled, focus: GroupId, btn: i32) -> Census {
    let mut store = plx_data::stores::metadata::MetadataStore::default();
    plx_data::metadata::set_current_for_test(store.state_mut(), Some(plx_data::metadata::Detail {
        title: "A film".into(), year: 2001, rating: "PG-13".into(), ..plx_data::metadata::Detail::default()
    }));
    let ps = plx_media::route::PlaybackSession::IDLE;
    let model = HudModel::default();
    let up = Countdown::default();
    let mut row = HudMotion::new();
    row.set_section_appear_for_test(Section::Info, 1.0);
    for _ in 0..30 { row.section_pop.step(Some(0), 0.016_667); }
    let frame = Frame { entry: EntryId(0), ps: &ps, model: &model, up: &up,
        slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None, focus, btn, tab: 0,
        open: crate::hud_format::compile::OPEN_INFO, now: 1_000, transport: true, measure: &FM, meta: store.view() };
    census(|| with_df(|df| assert!(draw(compiled, &frame, df, &mut row, Painter::root()))))
}

#[test]
fn a_row_button_does_not_shift_the_info_actions_focus_or_pop() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let text = REFERENCE.replace(r#"{ "id": "more", "command": "open_more", "widget": "disc""#,
        r#"{ "id": "more", "command": "open_more", "widget": "button""#);
    let text = text.replace(r#""z": 1"#, r#""z": 0"#);
    let text = text.replace(r#"{ "align": "end", "spacer": { "w": 150 } }"#,
        r#"{ "align": "end", "actions": { "axis": "column", "gap": 16, "item_h": 70, "min_w": 352,
        "items": [{ "id": "from_beginning", "command": "from_beginning", "widget": "button", "size": 70 },
        { "id": "detail", "command": "go_to_detail", "widget": "button", "size": 70 }] } }"#);
    let drew = info_frame(&load(&text), GROUP_SECTION, 0);
    let label = plx_platform::i18n::msg::widgets_info_from_beginning();
    let action = drew.1.iter().find(|r| r.text == label).expect("Info's first action draws");
    let expected = plx_ui::widgets::ControlStyle::Accent.colors(true, ControlGround::Keyed).1;
    assert_eq!(action.ink, expected, "section index0 is still the focused action, regardless of row buttons");
}

#[test]
fn actions_measure_the_same_parameterised_label_they_paint() {
    let _serial = plx_base::testlock::serial();
    for (command, expected) in [
        (r#""seek", "by": -10"#, plx_platform::i18n::msg::player_hud_skip_back()),
        (r#""open_tracks", "tab": "audio""#, plx_platform::i18n::msg::widgets_tracks_audio()),
        (r#""open_more", "page": "quality""#, plx_platform::i18n::msg::widgets_menu_quality()),
        (r#""open_more""#, plx_platform::i18n::msg::widgets_menu_options()),
    ] {
        let text = REFERENCE.replace(r#"{ "align": "end", "spacer": { "w": 150 } }"#,
            &format!(r#"{{ "align": "end", "actions": {{ "axis": "column", "gap": 0, "item_h": 70, "min_w": 0,
            "items": [{{ "id": "action", "command": {command}, "widget": "button", "size": 70 }}] }} }}"#));
        let compiled = load(&text);
        let state = compiled.lookup(crate::hud_format::compile::OPEN_INFO, crate::hud_format::Slot::Discs, true, false, false).unwrap();
        let label = CString::new(expected).unwrap();
        let want = plx_ui::widgets::Button::pill_w_measured(&label, theme::size::BODY, false, None, &FM);
        assert_eq!(state.control("action").unwrap().rect.w, want, "{command}: app label and actual glyph count decide width");
        assert!(info_frame(&compiled, GROUP_SECTION, 0).1.iter().any(|r| r.text == expected));
    }
}

fn episode_frame(compiled: &Compiled, open: u8) -> Census {
    let mut store = plx_data::stores::metadata::MetadataStore::default();
    store.run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(Some(plx_data::metadata::NowPlaying {
        is_episode: true, is_real_episode: true, title: "A long show title that must wrap onto more than one line".into(),
        ep_title: "An episode with a long name".into(), season: 1, index: 2, summary: String::new(), year: 2001,
        dur_ms: 600_000, rating: String::new(), thumb: String::new(), detail_rk: "1".into(),
    })));
    let ps = plx_media::route::PlaybackSession::IDLE;
    let model = HudModel { duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
    let up = Countdown::default();
    let mut row = HudMotion::new();
    if open != 0 { row.set_section_appear_for_test(Section::Info, 1.0); }
    let frame = Frame { entry: EntryId(0), ps: &ps, model: &model, up: &up, slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None,
        focus: GROUP_SCRUB, btn: 0, tab: 0, open, now: 1_000, transport: true, measure: &FM, meta: store.view() };
    census(|| with_df(|df| assert!(draw(compiled, &frame, df, &mut row, Painter::root()))))
}

#[test]
fn a_facts_text_run_uses_its_declared_type_rung() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""badges": [], "size": "CAPTION""#, r#""badges": [], "size": "BODY""#);
    assert_ne!(text, REFERENCE);
    let drew = info_frame(&load(&text), GROUP_SCRUB, 0);
    let year = drew.1.iter().find(|r| r.text == "2001").expect("year fact draws");
    assert_eq!(year.size, theme::size::BODY, "facts.size must affect measurement and paint");
    let rating = drew.1.iter().find(|r| r.text == "PG-13").expect("content_rating in facts.order is drawn as a plain fact");
    assert_eq!(rating.size, theme::size::BODY);
}

#[test]
fn real_episode_live_paint_applies_to_facts_and_the_playbar_too() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""genres_max": 1, "bold": false"#,
        r#""genres_max": 1, "bold": { "value": false, "if": { "title.is_episode": true } }"#)
        .replace(r#""fill": "gold""#, r#""fill": { "value": "TEXT_PRIMARY", "if": { "title.is_episode": "gold" } }"#);
    let drew = episode_frame(&load(&text), crate::hud_format::compile::OPEN_INFO);
    let year = drew.1.iter().find(|r| r.text == "2001").expect("episode's year fact draws");
    assert!(year.bold, "a real episode has its declared live weight even without queue neighbours");
    let clock = drew.1.iter().find(|r| r.text == "1:30").expect("elapsed clock");
    assert!((clock.ink[0] - 242.0 / 255.0).abs() < 0.001 && (clock.ink[1] - 181.0 / 255.0).abs() < 0.001,
        "the same live predicate resolves playbar paint: {:?}", clock.ink);
}

#[test]
fn a_declared_multiline_title_wraps_at_its_width_and_leading() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill""#,
        r#""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 2, "w": 260, "leading": 50"#);
    assert_ne!(text, REFERENCE);
    let drew = episode_frame(&load(&text), 0);
    let title: Vec<_> = drew.1.iter().filter(|r| r.size == theme::size::TITLE).collect();
    assert_eq!(title.len(), 2, "the title uses its declared two-line box: {:?}", drew.1);
    assert!((title[1].at.y - title[0].at.y - 50.0).abs() < 0.001, "declared leading");
}

#[test]
fn a_standalone_kicker_is_not_discarded_without_a_main_title() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#"          { "text": { "bind": "title.main", "size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1, "w": "fill" } }
"#, "");
    assert_ne!(text, REFERENCE);
    let text = text.replacen("\"w\": \"fill\" } },\n        ]", "\"w\": \"fill\" } }\n        ]", 1);
    let drew = episode_frame(&load(&text), 0);
    let label = plx_ui::fmt::episode_kicker(1, 2, "An episode with a long name");
    assert!(drew.1.iter().any(|r| r.text == label), "a declared kicker draws independently of title.main: {:?}", drew.1);
    assert!(!drew.1.iter().any(|r| r.size == theme::size::TITLE), "no undeclared main title");
}

#[test]
fn numeric_up_next_reserves_every_countdown_label_in_every_language() {
    let _serial = plx_base::testlock::serial();
    let minimal = include_str!("../../../hud_format/fixtures/minimal.plxhud.json");
    let text = minimal.replace(r#""countdown": "fill_sweep""#, r#""countdown": "numeric""#);
    for language in plx_platform::i18n::SHIPPED {
        let _lang = language_on_this_thread_for_test(language);
        let locale = plx_platform::i18n::current();
        let hud = crate::player_hud::loaded::load(text.as_bytes(), "0.0.0-test", locale, &FM).unwrap();
        let state = hud.compiled.lookup(0, crate::hud_format::Slot::UpNext, false, false, false).unwrap();
        let next = state.standin.up_next.unwrap().next;
        let labels = crate::hud_format::compile::AppLabels::in_language(locale);
        for label in &labels.next_seconds {
            let label = CString::new(label.as_str()).unwrap();
            let width = plx_ui::widgets::Button::pill_w_measured(&label, theme::size::BODY, false, None, &FM);
            assert!(width <= next.w + 0.001, "{}: numeric label {:?} needs {width}, slot is {}", language.tag(), label, next.w);
        }
    }
}

#[test]
fn declared_countdown_primitives_and_labelled_glyphs_are_priced() {
    let _serial = plx_base::testlock::serial();
    let minimal = include_str!("../../../hud_format/fixtures/minimal.plxhud.json");
    let priced = |kind| {
        let text = minimal.replace(r#""countdown": "fill_sweep""#, &format!(r#""countdown": "{kind}""#));
        let compiled = load(&text);
        crate::hud_format::lint_cost::state_cost(compiled.lookup(0, crate::hud_format::Slot::UpNext, false, false, false).unwrap())
    };
    let base = priced("fill_sweep");
    assert_eq!(priced("ring").draw_ops, base.draw_ops + plx_ui::widgets::ButtonProgress::Ring.extra_draw_ops());
    assert_eq!(priced("bar").draw_ops, base.draw_ops + plx_ui::widgets::ButtonProgress::Bar.extra_draw_ops());
    assert_eq!(priced("numeric").glyph_runs, base.glyph_runs, "numeric replaces the primary's one run, it adds no unpriced label");
    let button = REFERENCE.replace(r#"{ "id": "more", "command": "open_more", "widget": "disc""#,
        r#"{ "id": "more", "command": "open_more", "widget": "button""#);
    let plain = button.replace(r#", "face": "gear""#, "");
    let cost = |text: &str| {
        let c = load(text);
        crate::hud_format::lint_cost::state_cost(c.lookup(0, crate::hud_format::Slot::Discs, true, false, false).unwrap())
    };
    assert!(cost(&button).texture_bytes > cost(&plain).texture_bytes, "the declared SVG beside a label costs a mask too");
    assert_eq!(cost(&button).draw_ops, cost(&plain).draw_ops + 1);
}

fn chapter_frame(compiled: &Compiled, focused: bool) -> Census {
    chapter_frame_named(compiled, focused, "Chapter")
}

fn chapter_frame_named(compiled: &Compiled, focused: bool, name: &str) -> Census {
    use crate::hud_format::compile::OPEN_CHAPTERS;
    let mut store = plx_data::stores::metadata::MetadataStore::default();
    plx_data::metadata::set_playing_chapters_for_test(store.state_mut(), vec![
        plx_data::metadata::Chapter { index: 1, start_ms: 125_000, title: name.into(), thumb: String::new() }
    ]);
    let ps = plx_media::route::PlaybackSession::IDLE;
    let model = HudModel::default();
    let up = Countdown::default();
    let mut row = HudMotion::new();
    row.set_section_appear_for_test(Section::Chapters, 1.0);
    let frame = Frame { entry: EntryId(0), ps: &ps, model: &model, up: &up,
        slot: ControlSlot::Discs(Controls::NONE), busy: Busy::None, focus: GROUP_STRIP,
        btn: 0, tab: 0, open: OPEN_CHAPTERS, now: 1_000, transport: true, measure: &FM, meta: store.view() };
    let focus = FocusRead { current: focused.then_some(plx_machine::machine::FocusKey {
        entry: EntryId(0), elem: crate::player_hud::state::ELEM_CHAPTER_BASE }), ..FocusRead::default() };
    census(|| with_df_focus(focus, |df| assert!(draw(compiled, &frame, df, &mut row, Painter::root()))))
}

#[test]
fn chapter_template_text_faces_reach_focused_and_unfocused_cards() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let text = REFERENCE.replace(r#""bind": "item.title", "size": "CAPTION", "bold": false"#,
        r#""bind": "item.title", "size": "BODY", "bold": false"#);
    let compiled = load(&text);
    for focused in [false, true] {
        let drew = chapter_frame(&compiled, focused);
        let title = drew.1.iter().find(|r| r.text == "Chapter").unwrap_or_else(|| panic!("declared title: {:?}", drew.1));
        assert_eq!((title.size, title.bold), (theme::size::BODY, false), "focused={focused}");
        let time = drew.1.iter().find(|r| r.text == "2:05").expect("declared start time");
        assert_eq!((time.size, time.bold), (theme::size::MICRO, false));
    }
}

#[test]
fn chapter_template_omission_alignment_and_paint_are_declared() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let text = REFERENCE.replace(r#""bind": "item.title", "size": "CAPTION", "bold": false"#,
        r#""bind": "item.title", "size": "CAPTION", "bold": false, "alpha": 0.5"#)
        .replace(r#""if": { "item.focused": "TEXT_PRIMARY" }"#, r#""if": { "item.focused": "gold" }"#);
    let compiled = load(&text);
    let focused = chapter_frame(&compiled, true);
    let title = focused.1.iter().find(|r| r.text == "Chapter").unwrap();
    assert!((title.ink[0] - 242.0 / 255.0).abs() < 0.001);
    assert!((title.ink[3] - 0.5).abs() < 0.001);
    let leading = load(&text.replace(r#""focus": "ring",
          "card""#, r#""focus": "ring", "label_align": "leading",
          "card""#));
    let start = chapter_frame(&leading, true);
    let left = start.1.iter().find(|r| r.text == "Chapter").unwrap();
    assert!(title.at.x > left.at.x, "default centre and explicit leading differ");
    let absent = load(&REFERENCE.replace(r#"{ "text": { "bind": "item.title""#, r#"{ "when": "transport.paused", "text": { "bind": "item.title""#));
    for focused in [false, true] {
        assert!(!chapter_frame(&absent, focused).1.iter().any(|r| r.text == "Chapter"), "absent title stays absent");
    }
}

#[test]
fn a_focused_chapter_marquee_does_not_clip_glyphs_to_the_line_pitch() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""bind": "item.title", "size": "CAPTION", "bold": false"#,
        r#""bind": "item.title", "size": "BODY", "bold": false, "leading": 1"#);
    let name = "A very long chapter title overflowing its declared card width";
    let drew = chapter_frame_named(&load(&text), true, name);
    let title = drew.1.iter().find(|r| r.text == name).expect("shared focused marquee");
    assert!(title.clip.is_some_and(|clip| clip.h >= FM.line_h(theme::size::BODY)), "font ink must survive a small line pitch: {title:?}");
}
