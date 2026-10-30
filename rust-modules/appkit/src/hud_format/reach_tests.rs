//! **`check`'s tests** (`reach.rs`, `validate.rs`): the three valid fixtures pass, the folds of classic and
//! the reference are the plan's, and one invalid package per rule asserts its code AND its JSON pointer.
//!
//! Every invalid package is a valid fixture with ONE edit (`edit` fails the test when the text to replace
//! is not there exactly once), so a failure names the rule that moved. The rules the parser already
//! enforces (I8, I13, I9's sizes, I11's `default`, R4's two buttons and countdown) have their rows too, in
//! `parse_rows`, asserting the parser's own code: `check` must never run on a package that failed them.

use std::collections::BTreeSet;

use super::parse::{self, Package};
use super::reach::Layout;
use super::{check, codes, Code, Finding, Fold};

const CLASSIC: &str = crate::hud_format::CLASSIC_PACKAGE;
const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");

/// `classic`'s control row: the segment `row` of the band `top`, and the one control every row edits.
const ROW: &str = "/layout/block/0/cells/1/segment/items";
const MORE: &str = r#"{ "id": "more", "command": "open_more", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" }"#;
/// The reference's centre segment, where the ladder rows add a control.
const CENTER: &str = "/layout/block/2/cells/0/segment/items";
/// The reference's `previous` and `next` items, each with the comma and the indentation that follow it.
const REF_PREVIOUS: &str = r#"{ "id": "prev", "command": "previous", "widget": "disc", "size": 52, "rest": "bare", "focus": "accent",
            "face": { "default": "builtin:previous", "restart": "builtin:restart" } },
          "#;
const REF_NEXT: &str = r#"{ "id": "next", "command": "next", "widget": "disc", "size": 52, "rest": "bare", "focus": "accent",
            "face": "builtin:next" },
          "#;

fn edit(src: &str, from: &str, to: &str) -> String {
    let n = src.matches(from).count();
    assert_eq!(n, 1, "`{from}` must occur exactly once in the text, found {n}");
    src.replace(from, to)
}

fn package(text: &str) -> Package {
    let parsed = parse::parse(text.as_bytes());
    assert!(
        parsed.findings.is_empty(),
        "the package must parse clean: {:?}",
        parsed.findings
    );
    parsed.package.expect("a package")
}

fn dump(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| format!("  {}@{:?}: {}", f.code, f.pointer, f.message))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_finding<'a>(findings: &'a [Finding], code: &Code, pointer: &str) -> &'a Finding {
    let found = findings
        .iter()
        .find(|f| f.code == code.id && f.pointer == pointer)
        .unwrap_or_else(|| {
            panic!("expected {} at {pointer:?}, found:\n{}", code.id, dump(findings))
        });
    assert_eq!(found.rule, code.rule);
    assert_eq!(found.severity, code.severity);
    assert!(!found.message.is_empty() && !found.hint.is_empty(), "{found:?}");
    found
}

fn fold(command: &'static str, section: Option<&'static str>) -> Fold {
    Fold { command, section }
}

// ---------------------------------------------------------------------------------------------------
// The valid fixtures
// ---------------------------------------------------------------------------------------------------

#[test]
fn builtin_classic_passes_package_invariants() {
    let parsed = parse::parse(CLASSIC.as_bytes());
    assert!(parsed.findings.is_empty(), "{}", dump(&parsed.findings));
    let package = parsed.package.expect("a package");
    assert_eq!(package.id, "builtin:classic");
    assert!(package.palette.is_empty() && package.assets.is_empty());
    let checked = check(&package);
    assert!(checked.findings.is_empty(), "{}", dump(&checked.findings));
    assert_eq!(checked.folds, [], "classic passes as it is: nothing folds");
}

#[test]
fn the_reference_passes_and_folds_only_the_info_actions() {
    let checked = check(&package(REFERENCE));
    assert!(checked.findings.is_empty(), "{}", dump(&checked.findings));
    assert_eq!(
        checked.folds,
        [fold("from_beginning", None), fold("go_to_detail", None)],
        "the reference places everything but those two; its unplaced `open_tracks {{tab: audio}}` does not \
         fold, because the subtitles tab is placed"
    );
}

#[test]
fn the_minimal_package_passes_with_more_placed_and_folds_what_it_could_open() {
    let checked = check(&package(MINIMAL));
    assert!(checked.findings.is_empty(), "{}", dump(&checked.findings));
    assert_eq!(
        checked.folds,
        [
            fold("previous", None),
            fold("next", None),
            fold("open_tracks", None),
            fold("from_beginning", None),
            fold("go_to_detail", None),
        ],
        "it lays out no section, so a More row for Chapters or Info would open nothing and is not folded"
    );
}

#[test]
fn a_section_row_folds_only_for_a_section_the_package_lays_out() {
    // Chapters is laid out (a band shown `when: panel.chapters`), Info is not: only Chapters can fold.
    let text = edit(
        MINIMAL,
        r#"{ "id": "row", "gap_before": "SM","#,
        r#"{ "id": "chapters", "when": "panel.chapters", "cells": [
        { "align": "start", "text": { "bind": "label.chapters", "size": "LABEL", "ink": "TEXT_PRIMARY" } }
      ] },
      { "id": "row", "gap_before": "SM","#,
    );
    let folds = check(&package(&text)).folds;
    assert!(folds.contains(&fold("toggle_section", Some("chapters"))), "{folds:?}");
    assert!(!folds.contains(&fold("toggle_section", Some("info"))), "{folds:?}");
}

#[test]
fn the_state_enumeration_is_bounded_and_deterministic() {
    // classic names `chapters.present` and `title.is_episode`: four valuations. Chapters absent: the
    // compact state under three slots, plus the info section; chapters present: plus the chapters section.
    // (3 + 1) x 2 + (3 + 2) x 2.
    assert_eq!(check(&package(CLASSIC)).states, 18);
    // The reference names neither condition in a `when`, but it PLACES `previous` and `next`, and the
    // spec's command table makes that pair absent for a non-episode whatever the package says: so it
    // names `title.is_episode` too (see `a_placed_previous_or_next_names_title_is_episode`): two
    // valuations (episode false, true) of one form each. Inline sections over two laid-out sections: the
    // compact state under three slots, plus the three non-empty subsets, which wait on Discs (D7), is 6
    // per valuation. 2 x 6 = 12 (it was 1 x 6 = 6 before the reach gap was closed).
    assert_eq!(check(&package(REFERENCE)).states, 12);
    // The minimal package lays out no section: the compact state under its three slots.
    assert_eq!(check(&package(MINIMAL)).states, 3);
    for text in [CLASSIC, REFERENCE, MINIMAL] {
        let package = package(text);
        assert_eq!(check(&package), check(&package), "the same package, the same answer");
    }
}

/// Both values of `title.is_episode` that the walk takes for a package, `chapters.present` held.
fn episode_forms(text: &str) -> Vec<bool> {
    let package = package(text);
    Layout::read(&package.layout)
        .valuations()
        .into_iter()
        .filter(|v| v.chapters)
        .map(|v| v.episode)
        .collect()
}

#[test]
fn a_placed_previous_or_next_names_title_is_episode() {
    // The spec's command table: the previous / next pair is absent for a non-episode or with neither
    // neighbour, whatever the package says. A package that places either therefore has two forms, the
    // episode and the movie, although it never writes `title.is_episode` (the reference).
    assert!(!REFERENCE.contains("title.is_episode"), "the reference does not name the condition itself");
    assert_eq!(episode_forms(REFERENCE), [false, true], "the reference has the movie form and the episode form");
    assert_eq!(episode_forms(CLASSIC), [false, true], "classic names it in a `when` as well");
    // One placed command of the pair is enough: drop `next`, keep `previous`.
    let only_previous = edit(REFERENCE, REF_NEXT, "");
    assert_eq!(episode_forms(&only_previous), [false, true], "previous alone");
    // And the other way round.
    let only_next = edit(REFERENCE, REF_PREVIOUS, "");
    assert_eq!(episode_forms(&only_next), [false, true], "next alone");
    // A package that places neither has the one form it always had.
    let neither = edit(&edit(REFERENCE, REF_PREVIOUS, ""), REF_NEXT, "");
    assert_eq!(episode_forms(&neither), [true], "no previous or next, no movie form");
    assert_eq!(check(&package(&neither)).states, 6, "1 x 6: one valuation of the reference's six states");
    assert_eq!(episode_forms(MINIMAL), [true], "the minimal package places neither");
}

#[test]
fn every_fixture_passes_r1_and_r2_with_the_states_its_valuations_make() {
    // (fixture, valuations, states): the sum over the valuations of the reachable states. Only the
    // reference moved when a placed previous / next began to name `title.is_episode`; the others either
    // name the condition already (classic) or place neither command. The arithmetic:
    //   classic     4 valuations: chapters absent (3 + 1) x 2 episode, present (3 + 2) x 2 = 8 + 10 = 18
    //   reference   2 valuations (movie, episode) x (3 slots + 3 inline subsets) = 12
    //   minimal     1 valuation, no section laid out: 3 slots
    //   hero-center 1 valuation, modal sections but no opener: 3 slots
    //   palette     1 valuation, modal sections but no opener: 3 slots
    //   tabs        2 valuations (chapters.present only): absent 3 + {info} = 4, present 3 + 3 subsets = 6
    let hero = include_str!("fixtures/hero-center.plxhud.json");
    let palette = include_str!("fixtures/palette.plxhud.json");
    let tabs = include_str!("fixtures/tabs.plxhud.json");
    for (name, text, valuations, states) in [
        ("classic", CLASSIC, 4, 18),
        ("reference", REFERENCE, 2, 12),
        ("minimal", MINIMAL, 1, 3),
        ("hero-center", hero, 1, 3),
        ("palette", palette, 1, 3),
        ("tabs", tabs, 2, 10),
    ] {
        let package = package(text);
        let checked = check(&package);
        assert!(checked.findings.is_empty(), "{name}:\n{}", dump(&checked.findings));
        assert!(
            checked.folds.iter().all(|f| f.command != "open_more"),
            "{name}: R1 holds, so open_more never folds: {:?}",
            checked.folds
        );
        assert_eq!(Layout::read(&package.layout).valuations().len(), valuations, "{name}: valuations");
        assert_eq!(checked.states, states, "{name}: states");
    }
}

#[test]
fn the_movie_form_of_the_reference_changes_no_finding_or_fold() {
    // R1 (More is placed) and R2 (the folds) read the same package whether or not the movie form is walked:
    // the pair is PLACED in both forms (a placed control is one a `when` does not hide), so nothing folds.
    let checked = check(&package(REFERENCE));
    assert!(checked.findings.is_empty(), "{}", dump(&checked.findings));
    assert!(
        checked.folds.iter().all(|f| f.command != "previous" && f.command != "next"),
        "{:?}",
        checked.folds
    );
}

// ---------------------------------------------------------------------------------------------------
// One invalid package per rule: the code and the pointer
// ---------------------------------------------------------------------------------------------------

struct Row {
    label: &'static str,
    text: String,
    code: &'static Code,
    pointer: String,
}

fn row(label: &'static str, text: String, code: &'static Code, pointer: &str) -> Row {
    Row { label, text, code, pointer: pointer.to_owned() }
}

fn rows() -> Vec<Row> {
    let c = |from: &str, to: &str| edit(CLASSIC, from, to);
    let reference_ladder = |face: &str| {
        edit(
            REFERENCE,
            r#""face": "fwd30" }"#,
            &format!(
                r#""face": "fwd30" }},
          {{ "id": "skip_back", "command": "seek", "by": "setting", "dir": "back", "widget": "disc", "size": 52, "face": {face} }}"#
            ),
        )
    };
    vec![
        // R1: More is placed in the root state under every valuation.
        row(
            "r1_more_when_episode",
            c(MORE, &MORE.replace(" }", r#", "when": "title.is_episode" }"#)),
            &codes::E0801,
            &format!("{ROW}/4/when"),
        ),
        row(
            "r1_more_absent",
            c(&format!(",\n          {MORE}"), ""),
            &codes::E0801,
            "/layout/block",
        ),
        row(
            "r1_more_opens_a_page_not_the_root",
            c(r#""command": "open_more", "widget""#, r#""command": "open_more", "page": "quality", "widget""#),
            &codes::E0801,
            &format!("{ROW}/4"),
        ),
        row(
            "r1_more_in_a_band_shown_only_in_a_section",
            c(
                r#"{ "id": "top", "when": "!sections","#,
                r#"{ "id": "top", "when": "panel.info","#,
            ),
            &codes::E0801,
            "/layout/block/0/when",
        ),
        // R3: an opener with no band to open.
        row(
            "r3_opener_without_a_section_band",
            c(r#"{ "id": "info", "when": "panel.info", "cells""#, r#"{ "id": "info", "when": "!sections", "cells""#),
            &codes::E0802,
            "/layout/block/4/cells/0/segment/items/0/section",
        ),
        // R4: what Up Next names exists.
        row(
            "r4_up_next_replaces_nothing",
            c(r#""replaces": [ "row" ]"#, r#""replaces": [ ]"#),
            &codes::E0803,
            "/layout/up_next/replaces",
        ),
        row(
            "r4_up_next_replaces_an_unknown_segment",
            c(r#""replaces": [ "row" ]"#, r#""replaces": [ "nope" ]"#),
            &codes::E0803,
            "/layout/up_next/replaces/0",
        ),
        row(
            "r4_up_next_band_unknown",
            c(r#""band": "top""#, r#""band": "nope""#),
            &codes::E0803,
            "/layout/up_next/band",
        ),
        // I3: an id is unique among its kind.
        row(
            "i3_duplicate_control_id",
            c(r#""id": "audio", "command""#, r#""id": "subs", "command""#),
            &codes::E0901,
            &format!("{ROW}/3/id"),
        ),
        row(
            "i3_duplicate_band_id",
            c(r#"{ "id": "bar", "when""#, r#"{ "id": "top", "when""#),
            &codes::E0901,
            "/layout/block/1/id",
        ),
        row(
            "i3_duplicate_segment_id",
            c(r#""segment": { "id": "tabs""#, r#""segment": { "id": "row""#),
            &codes::E0901,
            "/layout/block/4/cells/0/segment/id",
        ),
        // I9: no palette in the built-in.
        row(
            "i9_palette_in_builtin_classic",
            edit(
                &c(r#""license": "GPL-3.0-or-later","#, r##""license": "GPL-3.0-or-later", "palette": { "gold": "#f2b535" },"##),
                r#""track": "RAIL_TRACK""#,
                r#""track": "gold""#,
            ),
            &codes::E0902,
            "/palette",
        ),
        // I11: a by_interval map lists every rung of the viewer's Skip ladder.
        row(
            "i11_ladder_missing_rungs",
            reference_ladder(r#"{ "default": "back10", "by_interval": { "10": "back10", "30": "fwd30" } }"#),
            &codes::W0903,
            &format!("{CENTER}/6/face/by_interval"),
        ),
    ]
}

#[test]
fn every_invalid_package_reports_its_code_at_its_pointer() {
    for row in rows() {
        let package = package(&row.text);
        let checked = check(&package);
        let found = assert_finding(&checked.findings, row.code, &row.pointer);
        assert!(
            found.pointer.starts_with('/'),
            "row `{}`: an RFC 6901 pointer into the package: {found:?}",
            row.label
        );
    }
}

#[test]
fn every_check_code_has_a_row() {
    let covered: BTreeSet<&str> = rows().iter().map(|r| r.code.id).collect();
    for code in codes::ALL {
        let check_code = ["HUD-E08", "HUD-E09", "HUD-W09"]
            .iter()
            .any(|prefix| code.id.starts_with(prefix));
        assert_eq!(covered.contains(code.id), check_code, "{}", code.id);
    }
}

#[test]
fn the_ladder_warning_names_the_missing_rungs_and_a_complete_ladder_is_clean() {
    let with = |face: &str| {
        edit(
            REFERENCE,
            r#""face": "fwd30" }"#,
            &format!(
                r#""face": "fwd30" }},
          {{ "id": "skip_back", "command": "seek", "by": "setting", "dir": "back", "widget": "disc", "size": 52, "face": {face} }}"#
            ),
        )
    };
    let partial = check(&package(&with(
        r#"{ "default": "back10", "by_interval": { "10": "back10", "30": "fwd30" } }"#,
    )));
    let warning = assert_finding(&partial.findings, &codes::W0903, &format!("{CENTER}/6/face/by_interval"));
    assert!(warning.message.contains("5, 15, 60"), "{}", warning.message);
    assert!(!warning.is_error() && !super::has_error(&partial.findings));

    let complete = check(&package(&with(
        r#"{ "default": "back10", "by_interval": { "5": "back10", "10": "back10", "15": "back10", "30": "fwd30", "60": "fwd30" } }"#,
    )));
    assert!(complete.findings.is_empty(), "{}", dump(&complete.findings));
    // No by_interval at all is one face for every interval: nothing to warn about.
    let flat = check(&package(&with(r#"{ "default": "back10" }"#)));
    assert!(flat.findings.is_empty(), "{}", dump(&flat.findings));
}

/// The rules the parser enforces: the row asserts the parser's own code and pointer.
#[test]
fn parse_rows() {
    let c = |from: &str, to: &str| edit(CLASSIC, from, to);
    let rows: Vec<(&str, String, &Code, String)> = vec![
        // I8: the rest and focus faces differ.
        (
            "i8_solid_rest_with_accent_focus",
            c(MORE, &MORE.replace(r#""rest": "film""#, r#""rest": "solid""#)),
            &codes::E0106,
            format!("{ROW}/4"),
        ),
        (
            "i8_solid_rest_without_a_stated_focus",
            c(MORE, &MORE.replace(r#""rest": "film", "focus": "accent""#, r#""rest": "solid""#)),
            &codes::E0103,
            format!("{ROW}/4"),
        ),
        // I13: the anchor is bottom.
        ("i13_anchor_top", c(r#""anchor": "bottom""#, r#""anchor": "top""#), &codes::E0201, "/layout/anchor".into()),
        // I9: a size is a rung, or HUD_TITLE on title.main.
        (
            "i9_size_not_a_rung",
            c(r#""bind": "title.kicker", "size": "CAPTION""#, r#""bind": "title.kicker", "size": "HUGE""#),
            &codes::E0201,
            "/layout/block/0/cells/0/vstack/cells/0/text/size".into(),
        ),
        (
            "i9_hud_title_off_title_main",
            c(r#""bind": "title.kicker", "size": "CAPTION""#, r#""bind": "title.kicker", "size": "HUD_TITLE""#),
            &codes::E0106,
            "/layout/block/0/cells/0/vstack/cells/0/text/size".into(),
        ),
        // R4: both Up Next buttons and the countdown are present.
        (
            "r4_watch_credits_missing",
            c(r#""watch_credits": { "rest": "film", "focus": "accent" }, "#, ""),
            &codes::E0103,
            "/layout/up_next".into(),
        ),
        (
            "r4_play_next_missing",
            c(r#""play_next": { "rest": "film", "focus": "accent" },"#, ""),
            &codes::E0103,
            "/layout/up_next".into(),
        ),
        (
            "r4_countdown_missing",
            c(
                "\"play_next\": { \"rest\": \"film\", \"focus\": \"accent\" },\n                 \"countdown\": \"fill_sweep\" }",
                "\"play_next\": { \"rest\": \"film\", \"focus\": \"accent\" } }",
            ),
            &codes::E0103,
            "/layout/up_next".into(),
        ),
        // I11: a seek-by-setting face map has `default`.
        (
            "i11_face_map_without_default",
            edit(
                REFERENCE,
                r#""face": "fwd30" }"#,
                r#""face": "fwd30" },
          { "id": "skip_back", "command": "seek", "by": "setting", "dir": "back", "widget": "disc", "size": 52,
            "face": { "by_interval": { "10": "back10" } } }"#,
            ),
            &codes::E0303,
            format!("{CENTER}/6/face"),
        ),
    ];
    for (label, text, code, pointer) in rows {
        let parsed = parse::parse(text.as_bytes());
        assert!(parsed.package.is_none(), "row `{label}`: must not parse");
        assert_finding(&parsed.findings, code, &pointer);
    }
}

// ---------------------------------------------------------------------------------------------------
// R2: the folds
// ---------------------------------------------------------------------------------------------------

fn folds_of(text: &str) -> Vec<Fold> {
    let checked = check(&package(text));
    assert!(checked.findings.is_empty(), "{}", dump(&checked.findings));
    checked.folds
}

const PREV: &str = r#"{ "id": "prev", "command": "previous", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" }"#;
const NEXT: &str = r#"{ "id": "next", "command": "next", "gap_after": 40, "widget": "disc", "size": 64, "rest": "film", "focus": "accent" }"#;

fn with_when(item: &str, when: &str) -> String {
    item.replace(" }", &format!(r#", "when": "{when}" }}"#))
}

#[test]
fn previous_and_next_not_placed_fold() {
    let text = edit(
        &edit(CLASSIC, &format!("{PREV},\n          "), ""),
        &format!("{NEXT},\n          "),
        "",
    );
    assert_eq!(folds_of(&text), [fold("previous", None), fold("next", None)]);
}

#[test]
fn previous_and_next_placed_only_under_chapters_present_fold_too() {
    // Enabled for every episode, placed only when the episode has chapters: not placed under every
    // valuation in which it is enabled.
    let text = edit(
        &edit(CLASSIC, PREV, &with_when(PREV, "chapters.present")),
        NEXT,
        &with_when(NEXT, "chapters.present"),
    );
    assert_eq!(folds_of(&text), [fold("previous", None), fold("next", None)]);
}

#[test]
fn previous_and_next_placed_for_episodes_do_not_fold() {
    // A movie asks for no Previous: the control being absent there is the app's own rule.
    let text = edit(
        &edit(CLASSIC, PREV, &with_when(PREV, "title.is_episode")),
        NEXT,
        &with_when(NEXT, "title.is_episode"),
    );
    assert_eq!(folds_of(&text), []);
}

#[test]
fn open_tracks_is_one_command_whose_tab_is_an_opening_address() {
    let subs = r#"{ "id": "subs", "command": "open_tracks", "tab": "subtitles", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" },
          "#;
    let audio = r#"{ "id": "audio", "command": "open_tracks", "tab": "audio", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" },
          "#;
    assert_eq!(folds_of(&edit(CLASSIC, audio, "")), []);
    assert_eq!(folds_of(&edit(CLASSIC, subs, "")), []);
    assert_eq!(
        folds_of(&edit(&edit(CLASSIC, audio, ""), subs, "")),
        [fold("open_tracks", None)]
    );
}

#[test]
fn a_section_without_an_opener_folds_its_row_and_what_only_its_band_places() {
    let tab_info = r#"{ "id": "tab_info", "command": "toggle_section", "section": "info", "widget": "pill", "size": 64 },
          "#;
    // The Info band stays (R3 only asks that an opener has a band, not the other way round). Without the
    // tab the section is not reachable by an edge of the graph, so its row folds, and so do the actions
    // only that band places: a folded row is redundant here, never missing.
    assert_eq!(
        folds_of(&edit(CLASSIC, tab_info, "")),
        [
            fold("toggle_section", Some("info")),
            fold("from_beginning", None),
            fold("go_to_detail", None),
        ]
    );
}

#[test]
fn the_chapters_opener_is_judged_where_chapters_exist() {
    // `tab_chapters` carries `when: chapters.present` and the command is enabled only for chapters, so it
    // is placed under every valuation in which it is enabled: nothing folds (the classic fixture).
    assert_eq!(folds_of(CLASSIC), []);
    // Moved behind a condition that is false while chapters exist, it is placed under none of them.
    let text = edit(
        CLASSIC,
        r#""when": "chapters.present", "widget": "pill""#,
        r#""when": "!chapters.present", "widget": "pill""#,
    );
    assert_eq!(folds_of(&text), [fold("toggle_section", Some("chapters"))]);
}

#[test]
fn toggle_play_seek_and_stats_never_fold_and_open_more_is_not_a_fold() {
    // The minimal package places none of them and folds none: keys reach the first two, More's own row
    // the third, and R1 requires the fourth.
    let names: Vec<&str> = check(&package(MINIMAL)).folds.iter().map(|f| f.command).collect();
    for command in ["toggle_play", "seek", "toggle_stats", "open_more"] {
        assert!(!names.contains(&command), "{command} folded");
    }
}
