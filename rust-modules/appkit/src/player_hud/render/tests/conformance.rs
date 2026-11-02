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
    let model = HudModel { is_episode: true, duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
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
    chapter_frame_content(compiled, focused, name, false)
}

fn chapter_frame_content(compiled: &Compiled, focused: bool, name: &str, episode: bool) -> Census {
    use crate::hud_format::compile::OPEN_CHAPTERS;
    let mut store = plx_data::stores::metadata::MetadataStore::default();
    plx_data::metadata::set_playing_chapters_for_test(store.state_mut(), vec![
        plx_data::metadata::Chapter { index: 1, start_ms: 125_000, title: name.into(), thumb: String::new() }
    ]);
    if episode {
        store.run(plx_data::stores::metadata::MetadataCmd::SetNowPlaying(Some(plx_data::metadata::NowPlaying {
            is_episode: true, is_real_episode: true, title: "Show".into(), ep_title: "Episode".into(), season: 1, index: 2,
            summary: String::new(), year: 2001, dur_ms: 600_000, rating: String::new(), thumb: String::new(), detail_rk: "1".into(),
        })));
    }
    let ps = plx_media::route::PlaybackSession::IDLE;
    let model = HudModel { is_episode: episode, ..HudModel::default() };
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

#[test]
fn a_decorative_cell_below_a_chapter_is_compiled_and_painted() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""below": ["#,
        r#""below": [{ "rule": { "axis": "h", "length": 80, "ink": "gold" } },"#);
    let compiled = load(&text);
    let state = compiled.lookup(crate::hud_format::compile::OPEN_CHAPTERS, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let rect = state.ops.iter().find_map(|op| match op {
        DrawOp::Strip { template, .. } => template.iter().find_map(|c| match c.op {
            DrawOp::Rule { rect, .. } => Some(rect), _ => None,
        }), _ => None,
    }).expect("a declared below rule must survive compilation");
    assert!(chapter_frame(&compiled, false).0.iter().any(|(_, r)| *r == rect), "the declared rule must reach the painter");
}

#[test]
fn interactive_card_descendants_are_rejected_with_a_focus_explanation() {
    for kind in ["segment", "actions", "playbar", "strip"] {
        let node = match kind {
            "segment" => r#"{ "segment": { "id": "hidden_controls", "items": [{ "id": "dead", "command": "open_more", "widget": "disc", "size": 32, "rest": "bare" }] } }"#,
            "actions" => r#"{ "actions": { "item_h": 32, "min_w": 80, "items": [{ "id": "dead", "command": "open_more", "widget": "button", "size": 32, "rest": "bare" }] } }"#,
            "playbar" => r#"{ "playbar": { "w": 80, "thickness": 4, "fill": "gold", "track": "HAIRLINE", "clock_size": "MICRO" } }"#,
            _ => r#"{ "strip": { "bind": "chapters", "w": 80, "card_w": 40, "card_h": 20, "card": { "art": { "bind": "item.thumb", "crop": "cover" } } } }"#,
        };
        let text = REFERENCE.replace(r#""below": ["#, &format!(r#""below": [{node},"#));
        let parsed = crate::hud_format::parse::parse(text.as_bytes());
        let package = parsed.package.unwrap_or_else(|| panic!("{kind}: {:?}", parsed.findings));
        let findings = crate::hud_format::reach::check(&package).findings;
        assert!(findings.iter().any(|f| f.code == "HUD-E0905"), "{kind}: no card-child focus diagnostic: {findings:?}");
    }
}

#[test]
fn a_current_chapter_overlay_draws_its_declared_asset() {
    let _serial = plx_base::testlock::serial();
    let compiled = load(REFERENCE);
    let state = compiled.lookup(crate::hud_format::compile::OPEN_CHAPTERS, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let rect = state.ops.iter().find_map(|op| match op {
        DrawOp::Strip { template, .. } => template.iter().find_map(|c| match &c.op {
            DrawOp::Icon { rect, asset, .. } if asset == "eq" => Some(*rect), _ => None,
        }), _ => None,
    }).unwrap();
    let masks = plx_ui::draw_census::capture_svgs(|| { chapter_frame(&compiled, false); });
    assert!(masks.iter().any(|(_, painted)| *painted == rect), "current chapter's declared eq marker is visible: {masks:?}");
}

#[test]
fn a_nested_card_data_stack_uses_the_shared_collapsing_layout() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""below": ["#,
        r#""below": [{ "vstack": { "w": "fill", "gap": 0, "cells": [
            { "text": { "bind": "info.summary", "size": "MICRO", "ink": "TEXT_PRIMARY", "max_lines": 2 } },
            { "rule": { "axis": "h", "length": 80, "ink": "gold" } }
        ] } },"#);
    let compiled = load(&text);
    let state = compiled.lookup(crate::hud_format::compile::OPEN_CHAPTERS, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let (top, rect) = state.ops.iter().find_map(|op| match op {
        DrawOp::Strip { template, groups, .. } => groups.first().map(|g| {
            let rect = template.iter().find_map(|c| match c.op { DrawOp::Rule { rect, .. } => Some(rect), _ => None }).unwrap();
            (g.rect.y, rect)
        }), _ => None,
    }).expect("nested data group survives compilation");
    let drew = chapter_frame(&compiled, false);
    let expected = Rect::new(rect.x, top, rect.w, rect.h);
    assert!(drew.0.iter().any(|(_, r)| *r == expected), "absent summary collapses before its rule: {expected:?}");
}

#[test]
fn card_facts_inherit_real_episode_paint_from_the_parent_frame() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#""below": ["#, r#""below": [{ "facts": { "bind": "info.facts",
        "order": ["year"], "badges": [], "size": "BODY", "ink": "gold",
        "bold": { "value": false, "if": { "title.is_episode": true } }
    } },"#);
    let drew = chapter_frame_content(&load(&text), false, "Chapter", true);
    let year = drew.1.iter().find(|r| r.text == "2001").expect("declared card facts");
    assert_eq!((year.size, year.bold), (theme::size::BODY, true));
    assert!((year.ink[0] - 242.0 / 255.0).abs() < 0.001);
}

#[test]
fn an_isolated_real_episode_keeps_its_episode_conditioned_band() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#"{ "id": "title", "cells": ["#,
        r#"{ "id": "title", "when": "title.is_episode", "cells": ["#);
    let drew = episode_frame(&load(&text), 0);
    assert!(drew.1.iter().any(|r| r.size == theme::size::TITLE), "real metadata is an episode even when the queue has no neighbours");
}

#[test]
fn content_geometry_focus_and_pair_availability_are_independent() {
    let _serial = plx_base::testlock::serial();
    let text = REFERENCE.replace(r#"{ "id": "bar", "gap_before""#, r#"{ "id": "bar", "when": "title.is_episode", "gap_before""#);
    let compiled = load(&text);
    let numeric = load(&REFERENCE.replace(r#""thickness": 6"#,
        r#""thickness": { "value": 6, "if": { "title.is_episode": 12 } }"#));
    for pair in [false, true] {
        for episode in [false, true] {
            let slot = ControlSlot::Discs(if pair { Controls { previous: true, next: true, restart: false } } else { Controls::NONE });
            let state = compiled.lookup_content(0, crate::hud_format::Slot::Discs, false, pair, episode, false).unwrap();
            assert_eq!(state.control("prev").is_some(), pair, "pair={pair} episode={episode}");
            let sized = crate::player_hud::geometry::geometry_content(&numeric, 0, slot, false, episode, false);
            assert_eq!(sized.playbar.h, if episode { 12.0 } else { 6.0 });
            let geo = crate::player_hud::geometry::geometry_content(&compiled, 0, slot, false, episode, false);
            assert_eq!(geo.playbar.w > 0.0, episode);
            let index = state.row_ids.iter().position(|id| id.as_deref() == Some("more")).unwrap();
            assert_eq!(geo.items[index], Some(state.control("more").unwrap().rect));
            let order = crate::player_hud::travel::order_content(&compiled, 0, slot, false, episode, false);
            assert_eq!(order.groups().contains(&GROUP_SCRUB), episode, "focus and drawing select the same content state");
        }
    }
    assert!(crate::player_hud::loaded::answers_every_frame(&compiled));

}

fn chapter_below(body: &str) -> String {
    let start = REFERENCE.find("\"below\": [").unwrap();
    let end = start + REFERENCE[start..].find("\n            ]").unwrap();
    format!("{}\"below\": [{}{}", &REFERENCE[..start], body, &REFERENCE[end..])
}

#[test]
fn a_card_group_dispatches_when_its_first_child_is_absent() {
    let _serial = plx_base::testlock::serial();
    let compiled = load(&chapter_below(r#"{ "vstack": { "w": "fill", "gap": 0, "cells": [
        { "when": "item.focused", "text": { "bind": "info.summary", "size": "MICRO", "ink": "TEXT_PRIMARY", "max_lines": 2 } },
        { "rule": { "axis": "h", "length": 80, "ink": "gold" } }
    ] } }"#));
    let state = compiled.lookup(crate::hud_format::compile::OPEN_CHAPTERS, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let rect = state.ops.iter().find_map(|op| match op { DrawOp::Strip { groups, .. } => groups.first().map(|g| Rect::new(g.rect.x, g.rect.y, 80.0, 1.0)), _ => None }).unwrap();
    for focused in [false, true] {
        assert!(chapter_frame(&compiled, focused).0.iter().any(|(_, r)| *r == rect), "group's present rule survives first-child absence, focused={focused}");
    }
}

#[test]
fn a_focused_chapter_label_paints_after_its_declared_background() {
    let _serial = plx_base::testlock::serial();
    let compiled = load(&chapter_below(r#"{ "rect": { "h": 60, "ink": "PANEL_TOP", "cells": [
        { "text": { "bind": "item.title", "size": "BODY", "ink": "TEXT_PRIMARY", "max_lines": 1 } }
    ] } }"#));
    let state = compiled.lookup(crate::hud_format::compile::OPEN_CHAPTERS, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let ground = state.ops.iter().find_map(|op| match op { DrawOp::Strip { template, .. } => template.iter().find_map(|c| match c.op { DrawOp::Ground { rect, .. } => Some(rect), _ => None }), _ => None }).unwrap();
    for focused in [false, true] {
        let drew = chapter_frame(&compiled, focused);
        let title = drew.1.iter().find(|r| r.text == "Chapter").unwrap();
        let bg = drew.0.iter().position(|(_, r)| *r == ground).unwrap();
        let text = drew.0.iter().position(|(kind, r)| *kind == 100 && r.x == title.at.x && r.y == title.at.y).unwrap();
        assert!(bg < text, "background precedes visible title, focused={focused}");
    }
}

#[test]
fn nested_card_item_text_tracks_the_collapsed_stack_and_keeps_its_marquee() {
    let _serial = plx_base::testlock::serial();
    let compiled = load(&chapter_below(r#"{ "vstack": { "w": "fill", "gap": 0, "cells": [
        { "text": { "bind": "info.summary", "size": "MICRO", "ink": "TEXT_PRIMARY", "max_lines": 2 } },
        { "text": { "bind": "item.start", "size": "CAPTION", "ink": "TEXT_SECONDARY", "leading": 32 } },
        { "text": { "bind": "item.title", "size": "BODY", "ink": "TEXT_PRIMARY", "max_lines": 1, "leading": 36 } }
    ] } }"#));
    let state = compiled.lookup(crate::hud_format::compile::OPEN_CHAPTERS, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let top = state.ops.iter().find_map(|op| match op { DrawOp::Strip { groups, .. } => groups.first().map(|g| g.rect.y), _ => None }).expect("card item text participates in shared data stack");
    let mut positions = Vec::new();
    for focused in [false, true] {
        let drew = chapter_frame(&compiled, focused);
        let time = drew.1.iter().find(|r| r.text == "2:05").expect("item.start resolves");
        let title = drew.1.iter().find(|r| r.text == "Chapter").expect("item.title resolves");
        positions.push(title.at.y);
        assert!(time.at.y <= top && title.at.y < top + 40.0, "absent summary collapses, focused={focused}: {:?}", drew.1);
    }
    assert!((positions[0] - positions[1]).abs() < 0.001, "focused name follows actual stack placement");
    let name = "A very long chapter name that must glide through its declared slot";
    let drew = chapter_frame_named(&compiled, true, name);
    assert!(drew.1.iter().any(|r| r.text == name && r.clip.is_some()), "shared marquee retains the whole focused name");
}

#[test]
fn section_actions_are_painted_and_addressed_for_every_widget() {
    let _serial = plx_base::testlock::serial();
    for widget in ["pill", "disc", "button"] {
        let text = REFERENCE.replace(r#"{ "align": "end", "spacer": { "w": 150 } }"#,
            &format!(r#"{{ "align": "end", "actions": {{ "axis": "column", "gap": 0, "item_h": 70, "min_w": 0,
            "items": [{{ "id": "action", "command": "open_more", "widget": "{widget}", "size": 70, "face": "builtin:pause" }}] }} }}"#));
        let compiled = load(&text);
        let state = compiled.lookup(crate::hud_format::compile::OPEN_INFO, crate::hud_format::Slot::Discs, true, false, false).unwrap();
        assert_eq!(super::super::super::geometry::section_buttons(state).map(|c| c.id.as_str()).collect::<Vec<_>>(), ["action"], "{widget}: the container owns focus identity");
        let drew = info_frame(&compiled, GROUP_SECTION, 0);
        assert!(drew.2.iter().any(|(icon, _)| *icon == Icon::Pause), "{widget}: accepted action face must paint");
    }
}

#[test]
fn closing_a_section_returns_to_its_row_button_opener() {
    let text = REFERENCE.replace(r#""command": "toggle_section", "section": "chapters", "widget": "disc""#,
        r#""command": "toggle_section", "section": "chapters", "widget": "button""#);
    assert_ne!(text, REFERENCE);
    let compiled = load(&text);
    let state = compiled.lookup(0, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let ordinal = state.row_ids.iter().position(|id| id.as_deref() == Some("chapters")).unwrap();
    assert_eq!(super::super::super::geometry::opener_elem(state, Section::Chapters),
        Some(super::super::super::state::control_elem(ordinal as i32)));
}

#[test]
fn mixed_section_actions_keep_discs_square_inside_wide_cells() {
    let text = REFERENCE.replace(r#"{ "align": "end", "spacer": { "w": 150 } }"#,
        r#"{ "align": "end", "actions": { "axis": "column", "gap": 16, "item_h": 70, "min_w": 352,
        "items": [{ "id": "disc", "command": "open_more", "widget": "disc", "size": 52 },
        { "id": "button", "command": "open_more", "widget": "button", "size": 70 }] } }"#);
    let compiled = load(&text);
    let state = compiled.lookup(crate::hud_format::compile::OPEN_INFO, crate::hud_format::Slot::Discs, true, false, false).unwrap();
    let disc = state.control("disc").unwrap().rect;
    let button = state.control("button").unwrap().rect;
    assert_eq!((disc.w, disc.h), (52.0, 52.0), "the glyph and hit target use the declared disc size");
    assert_eq!(disc.cx(), button.cx(), "disc is centered within the shared 352px action cell");
    assert_eq!(disc.cy() + 86.0, button.cy(), "item_h and gap still place the next row");
}
