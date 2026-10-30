//! **The lint contract's own pins** (`lint.rs`, the six `lint_*` check modules): the shipped languages and
//! their labels, the language collapse, the state and slot names, the compile's provenance accessors, the
//! invalid-case support, and the union of every check module's [`INVALID`](super::lint_zorder::INVALID)
//! and [`UNIT_ONLY`](super::lint_zorder::UNIT_ONLY) lists.
//!
//! The goldens (`builtin_classic_passes_every_invariant`, `reference_passes_every_invariant`) and the
//! requirement that every lint code has a case belong to the package that lands the last check; the
//! ignored `every_lint_code_has_an_invalid_case_or_a_reason` below is its shell.

use std::collections::{BTreeSet, HashMap};

use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, LocaleContext, Preference};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::Rect;

use super::compile::{compile, CompileCtx, CompiledState, DrawOp, StateKey, OPEN_CHAPTERS, OPEN_INFO};
use super::lint::testing::{apply, assert_case, Base, InvalidCase};
use super::lint::{collapse, ctx_for, lint, named, slot_name, state_name, tag_language, LangCtx, LintLang, LintOptions};
use super::parse::{self, Object, Package};
use super::reach::Slot;
use super::validate::each_object;
use super::{codes, lint_band, lint_menus, lint_safe, lint_size, lint_text, lint_zorder};
use super::{Context, Finding, BUILTIN_CLASSIC};

const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");
const HERO: &str = include_str!("fixtures/hero-center.plxhud.json");
const TABS: &str = include_str!("fixtures/tabs.plxhud.json");
const PALETTE: &str = include_str!("fixtures/palette.plxhud.json");

/// The fourteen codes the lint registers (`HUD-E0601` ... `HUD-E1303`).
const LINT_CODES: [&str; 14] = [
    "HUD-E0601", "HUD-W0602", "HUD-E0603", "HUD-E0604", "HUD-E1001", "HUD-E1002", "HUD-E1003", "HUD-E1101",
    "HUD-E1102", "HUD-E1103", "HUD-E1201", "HUD-E1301", "HUD-E1302", "HUD-E1303",
];

fn package(text: &str) -> Package {
    let parsed = parse::parse(text.as_bytes());
    assert!(parsed.findings.is_empty(), "the fixture must parse clean: {:?}", parsed.findings);
    parsed.package.expect("a package")
}

fn finding(code: &super::Code, pointer: &str, lang: Option<&str>, measured: f32) -> Finding {
    Finding::new(code, pointer, "m", "h").with(Context {
        lang: lang.map(str::to_owned),
        state: Some("compact".to_owned()),
        slot: Some("discs".to_owned()),
        text: Some(super::TextFact { string: "Kapitel".to_owned(), measured, room: 100.0 }),
        ..Context::default()
    })
}

// ---------------------------------------------------------------------------------------------------
// The languages
// ---------------------------------------------------------------------------------------------------

#[test]
fn the_ten_shipped_languages_in_catalog_order() {
    let tags: Vec<&str> = LintLang::shipped().iter().map(|l| l.tag).collect();
    assert_eq!(tags, ["en", "es", "be", "fr", "it", "pt", "de", "hu", "ru", "pl"]);
    let chapters: BTreeSet<String> = LintLang::shipped().into_iter().map(|l| l.labels.tab_chapters).collect();
    assert!(chapters.len() > 5, "the labels are the languages' own, not one language ten times: {chapters:?}");
}

#[test]
fn a_language_is_looked_up_by_its_tag_and_nothing_else_is_one() {
    for tag in ["en", "de", "be", "pl"] {
        assert_eq!(LintLang::for_tag(tag).map(|l| l.tag), Some(tag));
    }
    for not in ["", "system", "xx", "EN", "en-US", "de "] {
        assert!(LintLang::for_tag(not).is_none(), "`{not}` is not a shipped language tag");
    }
}

#[test]
fn the_labels_come_from_the_language_asked_for_never_from_the_current_one() {
    let english = LocaleContext::resolve(Preference::En, None, None, None, None);
    let _russian = language_on_this_thread_for_test(Preference::Ru);
    assert_ne!(
        msg::widgets_player_chapters(),
        msg::widgets_player_chapters_in(&english),
        "the thread speaks Russian, so the test can tell the two apart"
    );
    let lang = LintLang::for_tag("en").expect("English");
    assert_eq!(lang.labels.tab_chapters, msg::widgets_player_chapters_in(&english));
    assert_eq!(lang.labels.skip_intro, msg::widgets_skip_intro_in(&english));
    assert_eq!(lang.labels.go_to_movie, msg::widgets_info_go_movie_in(&english));
}

// ---------------------------------------------------------------------------------------------------
// The pipeline
// ---------------------------------------------------------------------------------------------------

#[test]
fn classic_and_the_fixtures_run_the_whole_pipeline_in_every_language() {
    let langs = LintLang::shipped();
    for (name, text) in [
        ("classic", BUILTIN_CLASSIC),
        ("reference", REFERENCE),
        ("minimal", MINIMAL),
        ("hero-center", HERO),
        ("tabs", TABS),
        ("palette", PALETTE),
    ] {
        let found = lint(text, &ShippedMeasure, &langs);
        // The check modules are still empty (the contract freeze): the stages before them are clean.
        assert!(found.iter().all(|f| f.ctx == Context::default()), "{name}: {found:?}");
    }
}

#[test]
fn a_parse_error_stops_the_run_and_carries_no_context() {
    let found = lint("{", &ShippedMeasure, &LintLang::shipped());
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!((found[0].code, found[0].pointer.as_str()), ("HUD-E0100", ""));
    assert_eq!(found[0].ctx, Context::default());
}

#[test]
fn with_no_language_only_the_language_free_stages_run() {
    assert!(lint(BUILTIN_CLASSIC, &ShippedMeasure, &[]).is_empty());
    assert_eq!(lint("{", &ShippedMeasure, &[]).len(), 1);
}

#[test]
fn the_default_options_carry_the_ci_text_skew() {
    assert_eq!(LintOptions::default().text_skew, lint_text::CI_TEXT_SKEW);
    assert_eq!(lint_text::CI_TEXT_SKEW, 0.0, "0 until PR12 measures it");
}

// ---------------------------------------------------------------------------------------------------
// The language collapse and the tagging
// ---------------------------------------------------------------------------------------------------

#[test]
fn a_finding_identical_in_every_language_is_reported_once_without_a_language() {
    let same = |lang: &str| finding(&codes::E1001, "/layout/block/0", Some(lang), 90.0);
    let out = collapse(vec![vec![same("en")], vec![same("de")], vec![same("fr")]]);
    assert_eq!(out.len(), 1, "{out:?}");
    assert_eq!(out[0].ctx.lang, None);
    assert_eq!(out[0].ctx.state.as_deref(), Some("compact"), "everything else of the context stays");
}

#[test]
fn a_finding_that_differs_between_languages_keeps_its_language() {
    let out = collapse(vec![
        vec![finding(&codes::E0601, "/p", Some("en"), 90.0)],
        vec![finding(&codes::E0601, "/p", Some("de"), 140.0)],
    ]);
    let langs: Vec<_> = out.iter().map(|f| f.ctx.lang.as_deref()).collect();
    assert_eq!(langs, [Some("en"), Some("de")]);
}

#[test]
fn a_finding_missing_from_one_language_is_not_shared() {
    let out = collapse(vec![
        vec![finding(&codes::E1001, "/p", Some("en"), 1.0)],
        vec![finding(&codes::E1001, "/p", Some("de"), 1.0)],
        vec![],
    ]);
    let langs: Vec<_> = out.iter().map(|f| f.ctx.lang.as_deref()).collect();
    assert_eq!(langs, [Some("en"), Some("de")]);
}

#[test]
fn a_repeated_finding_collapses_as_often_as_every_language_repeats_it() {
    let one = |lang: &str| finding(&codes::E1001, "/p", Some(lang), 1.0);
    let out = collapse(vec![vec![one("en"), one("en")], vec![one("de")]]);
    let langs: Vec<_> = out.iter().map(|f| f.ctx.lang.as_deref()).collect();
    assert_eq!(langs, [None, Some("en")], "one shared, one only English has");
}

#[test]
fn a_single_language_keeps_its_tag() {
    let out = collapse(vec![vec![finding(&codes::E1001, "/p", Some("de"), 1.0)]]);
    assert_eq!(out[0].ctx.lang.as_deref(), Some("de"));
    assert!(collapse(Vec::new()).is_empty());
}

#[test]
fn a_finding_that_names_no_language_is_the_one_it_was_linted_in() {
    let mut found = vec![
        finding(&codes::E1001, "/a", None, 1.0),
        finding(&codes::E1001, "/b", Some("fr"), 1.0),
    ];
    tag_language(&mut found, "de");
    assert_eq!(found[0].ctx.lang.as_deref(), Some("de"));
    assert_eq!(found[1].ctx.lang.as_deref(), Some("fr"), "a language a check set is kept");
}

// ---------------------------------------------------------------------------------------------------
// The names a finding's context carries
// ---------------------------------------------------------------------------------------------------

fn key(open: u8, slot: Slot, chapters: bool, episode: bool, paused: bool) -> StateKey {
    StateKey { open, slot, chapters, episode, paused }
}

#[test]
fn state_names_spell_the_sections_then_what_departs_from_the_default() {
    assert_eq!(state_name(key(0, Slot::Discs, true, true, false)), "compact");
    assert_eq!(state_name(key(OPEN_CHAPTERS, Slot::Discs, true, true, false)), "chapters");
    assert_eq!(state_name(key(OPEN_INFO, Slot::Discs, true, true, false)), "info");
    assert_eq!(state_name(key(OPEN_CHAPTERS | OPEN_INFO, Slot::Discs, true, true, false)), "chapters+info");
    assert_eq!(state_name(key(0, Slot::Discs, true, false, false)), "compact,movie");
    assert_eq!(state_name(key(0, Slot::Discs, false, true, false)), "compact,no-chapters");
    assert_eq!(state_name(key(0, Slot::Discs, true, true, true)), "compact,paused");
    assert_eq!(state_name(key(OPEN_INFO, Slot::Discs, false, false, true)), "info,movie,no-chapters,paused");
    // the slot is not part of the state's name
    assert_eq!(state_name(key(0, Slot::Skip, true, true, false)), "compact");
}

#[test]
fn slot_names_are_the_three_slots() {
    assert_eq!(slot_name(key(0, Slot::Discs, true, true, false)), "discs");
    assert_eq!(slot_name(key(0, Slot::UpNext, true, true, false)), "up_next");
    assert_eq!(slot_name(key(0, Slot::Skip, true, true, false)), "skip");
}

#[test]
fn the_state_names_of_a_compiled_package_are_unique_per_slot() {
    let lang = LintLang::for_tag("en").expect("English");
    for text in [BUILTIN_CLASSIC, REFERENCE, HERO, TABS] {
        let package = package(text);
        let compiled = compile(&package, &FixtureMeasure, CompileCtx { labels: lang.labels.clone() });
        let opts = LintOptions::default();
        let cx = LangCtx { package: &package, compiled: &compiled, measure: &FixtureMeasure, lang: &lang, opts: &opts };
        let names: Vec<String> = cx.states().map(|s| format!("{}/{}", state_name(s.key), slot_name(s.key))).collect();
        assert_eq!(names.len(), compiled.states().len());
        assert_eq!(names.iter().collect::<BTreeSet<_>>().len(), names.len(), "{names:?}");
    }
}

#[test]
fn a_findings_context_starts_with_the_language_state_and_slot() {
    let lang = LintLang::for_tag("de").expect("German");
    let package = package(BUILTIN_CLASSIC);
    let compiled = compile(&package, &FixtureMeasure, CompileCtx { labels: lang.labels.clone() });
    let opts = LintOptions::default();
    let cx = LangCtx { package: &package, compiled: &compiled, measure: &FixtureMeasure, lang: &lang, opts: &opts };
    let first = cx
        .states()
        .find(|s| state_name(s.key) == "compact" && slot_name(s.key) == "discs")
        .expect("the compact state under the discs");
    let ctx = ctx_for(&cx, &first);
    assert_eq!(ctx.lang.as_deref(), Some("de"));
    assert_eq!(ctx.state.as_deref(), Some("compact"));
    assert_eq!(ctx.slot.as_deref(), Some("discs"));
    assert!(ctx.rects.is_empty() && ctx.text.is_none());
    let rect = named("next", Rect::new(1.0, 2.0, 3.0, 4.0));
    assert_eq!((rect.name.as_str(), rect.x, rect.y, rect.w, rect.h), ("next", 1.0, 2.0, 3.0, 4.0));
}

// ---------------------------------------------------------------------------------------------------
// The compile's provenance: which node of the package produced each op and control
// ---------------------------------------------------------------------------------------------------

fn nodes(package: &Package) -> HashMap<&str, &Object> {
    let mut map = HashMap::new();
    each_object(&package.layout, &mut |o| {
        map.insert(o.pointer.as_str(), o);
    });
    map
}

fn check_provenance(name: &str, package: &Package, state: &CompiledState, nodes: &HashMap<&str, &Object>) {
    let at = |what: &str, pointer: &str| {
        assert!(!pointer.is_empty(), "{name}: {what} has no origin");
        assert!(nodes.contains_key(pointer), "{name}: {what} names `{pointer}`, which is no node of the package");
    };
    let mut control = 0;
    for (i, op) in state.ops.iter().enumerate() {
        let origin = state.origin(i);
        at(&format!("ops[{i}] {op:?}"), origin);
        match op {
            DrawOp::Control(c) => {
                let pointer = state.control_pointer(control);
                assert_eq!(origin, pointer, "{name}: a control op and its control name one item");
                assert_eq!(state.controls[control].id, c.id, "{name}: the controls are in draw order");
                let item = nodes[pointer];
                assert_eq!(item.node, "item", "{name}: {pointer}");
                assert_eq!(item.get("id").and_then(parse::Val::as_str), Some(c.id.as_str()), "{name}: {pointer}");
                control += 1;
            }
            DrawOp::Scrim { .. } => assert_eq!(origin, "/layout/scrim", "{name}"),
            DrawOp::UpNext { .. } | DrawOp::SkipPill { .. } => assert_eq!(origin, "/layout/up_next", "{name}"),
            DrawOp::Strip { template, .. } => {
                assert!(!template.is_empty(), "{name}: a strip has a template");
                for card in template {
                    at(&format!("a card op {:?}", card.op), &card.origin);
                }
            }
            _ => {}
        }
    }
    assert_eq!(control, state.controls.len(), "{name}: every control has its op");
    assert_eq!(state.origin(state.ops.len()), "", "an index past the ops answers an empty pointer");
    assert_eq!(state.control_pointer(state.controls.len()), "");
    let _ = package;
}

#[test]
fn every_op_and_control_of_every_fixture_names_the_node_that_produced_it() {
    let lang = LintLang::for_tag("en").expect("English");
    for (name, text) in [
        ("classic", BUILTIN_CLASSIC),
        ("reference", REFERENCE),
        ("minimal", MINIMAL),
        ("hero-center", HERO),
        ("tabs", TABS),
        ("palette", PALETTE),
    ] {
        let package = package(text);
        let nodes = nodes(&package);
        let compiled = compile(&package, &FixtureMeasure, CompileCtx { labels: lang.labels.clone() });
        let mut checked = 0;
        for state in compiled.states() {
            check_provenance(name, &package, state, &nodes);
            checked += 1;
            if let Some(alt) = &state.skip_alt {
                check_provenance(name, &package, alt, &nodes);
                checked += 1;
            }
        }
        assert!(checked >= compiled.states().len() && checked > 0, "{name}");
    }
}

#[test]
fn classic_names_its_nodes() {
    let lang = LintLang::for_tag("en").expect("English");
    let package = package(BUILTIN_CLASSIC);
    let compiled = compile(&package, &FixtureMeasure, CompileCtx { labels: lang.labels.clone() });
    let root = compiled
        .lookup(0, Slot::Discs, true, true, false)
        .expect("the compact state");
    let more = root.controls.iter().position(|c| c.id == "more").expect("classic places More");
    assert_eq!(root.control_pointer(more), "/layout/block/0/cells/1/segment/items/4");
}

// ---------------------------------------------------------------------------------------------------
// The invalid-case support
// ---------------------------------------------------------------------------------------------------

/// The classic control row's More item, edited to a command that does not exist: a parse finding, which the
/// lint reports (and stops at) like any other.
const SUPPORT_CASE: InvalidCase = InvalidCase {
    name: "support: an unknown command",
    code: "HUD-E0301",
    pointer: "/layout/block/0/cells/1/segment/items/4/command",
    base: Base::Classic,
    edits: &[(r#""command": "open_more""#, r#""command": "open_mor""#)],
    langs: &["en", "de"],
    skew: 0.0,
    also: &[],
};

#[test]
fn an_invalid_case_is_checked_as_the_set_of_its_code_and_pointer() {
    assert_case(&SUPPORT_CASE);
}

#[test]
#[should_panic(expected = "findings were")]
fn a_case_whose_findings_differ_from_its_expectation_fails() {
    assert_case(&InvalidCase { pointer: "/layout/block/0", ..SUPPORT_CASE });
}

#[test]
fn a_case_lists_the_further_findings_it_expects_in_also() {
    // Two edits, two findings: the second is declared in `also`.
    assert_case(&InvalidCase {
        edits: &[
            (r#""command": "open_more""#, r#""command": "open_mor""#),
            (r#""license": "GPL-3.0-or-later","#, r#""license": "GPL-3.0-or-later", "licence": "x","#),
        ],
        also: &[("HUD-E0102", "/licence")],
        ..SUPPORT_CASE
    });
}

#[test]
#[should_panic(expected = "exactly once")]
fn an_edit_that_is_not_there_exactly_once_fails() {
    let _ = apply(Base::Classic, &[("no such text in classic", "x")]);
}

#[test]
#[should_panic(expected = "exactly once")]
fn an_edit_that_is_there_twice_fails() {
    let _ = apply(Base::Classic, &[(r#""widget""#, r#""widget""#)]);
}

#[test]
fn the_bases_are_the_classic_embed_and_the_reference_fixture() {
    assert_eq!(Base::Classic.text(), BUILTIN_CLASSIC);
    assert_eq!(Base::Reference.text(), REFERENCE);
    assert_eq!(apply(Base::Reference, &[]), REFERENCE);
}

// ---------------------------------------------------------------------------------------------------
// The union of the check modules' cases
// ---------------------------------------------------------------------------------------------------

/// Every check module's invalid cases.
fn invalid_cases() -> Vec<&'static InvalidCase> {
    [
        lint_zorder::INVALID,
        lint_safe::INVALID,
        lint_band::INVALID,
        lint_menus::INVALID,
        lint_text::INVALID,
        lint_size::INVALID,
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Every check module's `(code, reason)` list of codes only a synthetic unit test reaches.
fn unit_only() -> Vec<(&'static str, &'static str)> {
    [
        lint_zorder::UNIT_ONLY,
        lint_safe::UNIT_ONLY,
        lint_band::UNIT_ONLY,
        lint_menus::UNIT_ONLY,
        lint_text::UNIT_ONLY,
        lint_size::UNIT_ONLY,
    ]
    .into_iter()
    .flatten()
    .copied()
    .collect()
}

#[test]
fn every_invalid_case_yields_exactly_its_findings() {
    for case in invalid_cases() {
        assert_case(case);
    }
}

#[test]
fn the_cases_are_well_formed() {
    let cases = invalid_cases();
    let names: BTreeSet<&str> = cases.iter().map(|c| c.name).collect();
    assert_eq!(names.len(), cases.len(), "a case name is used once");
    for case in &cases {
        assert!(LINT_CODES.contains(&case.code), "case `{}`: `{}` is not a lint code", case.name, case.code);
        assert!(case.pointer.is_empty() || case.pointer.starts_with('/'), "case `{}`: an RFC 6901 pointer", case.name);
        assert!(!case.edits.is_empty(), "case `{}`: an invalid case edits its base", case.name);
        for (code, _) in case.also {
            assert!(codes::ALL.iter().any(|c| c.id == *code), "case `{}`: `{code}` is not a code", case.name);
        }
        for tag in case.langs {
            assert!(LintLang::for_tag(tag).is_some(), "case `{}`: `{tag}` is not a shipped language", case.name);
        }
    }
    for (code, reason) in unit_only() {
        assert!(LINT_CODES.contains(&code), "`{code}` is not a lint code");
        assert!(!reason.is_empty(), "`{code}`: a unit-only code says why");
    }
}

/// The shell of the requirement the last package to land a check closes: every one of the fourteen codes
/// has an invalid case, or a reason a parseable package cannot reach it.
#[test]
#[ignore = "contract shell: the package landing the last check un-ignores it once the six modules hold their cases"]
fn every_lint_code_has_an_invalid_case_or_a_reason() {
    let covered: BTreeSet<&str> = invalid_cases()
        .iter()
        .flat_map(|c| std::iter::once(c.code).chain(c.also.iter().map(|(code, _)| *code)))
        .chain(unit_only().into_iter().map(|(code, _)| code))
        .collect();
    let missing: Vec<&str> = LINT_CODES.iter().copied().filter(|code| !covered.contains(code)).collect();
    assert!(missing.is_empty(), "codes with no invalid case and no unit-only reason: {missing:?}");
}
