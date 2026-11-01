//! **The lint's own tests** (`lint.rs`, the six `lint_*` check modules): the shipped languages and their
//! labels, the language collapse, the state and slot names, the compile's provenance accessors, the
//! invalid-case support, the union of every check module's [`INVALID`](super::lint_zorder::INVALID) and
//! [`UNIT_ONLY`](super::lint_zorder::UNIT_ONLY) lists and what the whole pipeline says about them.
//!
//! The goldens are here: `builtin_classic_passes_every_invariant` and `reference_passes_every_invariant`
//! (zero findings in all eleven shipped languages, measured with the real font advances), the four other
//! fixtures (`other_fixtures_lint_without_an_error`), the requirement that every one of the fourteen lint
//! codes has an invalid case or a stated reason it has none (`every_new_code_has_an_invalid_case`), and the
//! behaviours of the whole run: sorted and deterministic output, every finding of a package reported in one
//! run, the JSON report and the language context.

use std::collections::{BTreeSet, HashMap};

use plx_base::fontcov::advances::ShippedMeasure;
use plx_platform::i18n::{language_on_this_thread_for_test, msg, LocaleContext, Preference};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::Rect;

use super::compile::{compile, CompileCtx, CompiledState, DrawOp, StateKey, OPEN_CHAPTERS, OPEN_INFO};
use super::lint::testing::{apply, assert_case, Base, InvalidCase};
use super::lint::{
    collapse, ctx_for, lint, lint_with, named, slot_name, state_name, tag_language, LangCtx, LintLang, LintOptions,
};
use super::parse::{self, Object, Package};
use super::reach::Slot;
use super::validate::each_object;
use super::{codes, lint_band, lint_menus, lint_safe, lint_size, lint_text, lint_zorder, report};
use super::{Context, Finding, Severity, BUILTIN_CLASSIC};

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
fn the_shipped_languages_in_catalog_order() {
    let tags: Vec<&str> = LintLang::shipped().iter().map(|l| l.tag).collect();
    assert_eq!(tags, ["en", "es", "be", "fr", "it", "pt", "de", "hu", "ru", "pl", "nl"]);
    let chapters: BTreeSet<String> = LintLang::shipped().into_iter().map(|l| l.labels.tab_chapters).collect();
    assert!(chapters.len() > 5, "the labels are the languages' own, not one language eleven times: {chapters:?}");
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

// ---------------------------------------------------------------------------------------------------
// The goldens
// ---------------------------------------------------------------------------------------------------

/// `text` linted in every shipped language, measured with the real font advances.
fn lint_in_all_languages(text: &str) -> Vec<Finding> {
    let langs = LintLang::shipped();
    assert_eq!(langs.len(), 11, "the goldens are the eleven shipped languages");
    lint(text, &ShippedMeasure, &langs)
}

/// What a failing golden prints: every finding, as the text report and as the JSON report (which carries
/// the rects and measured text the text report leaves out).
fn describe(name: &str, found: &[Finding]) -> String {
    format!("{} finding(s)\n{}\n{}", found.len(), report::text(name, found), report::json_text(name, found))
}

#[test]
fn builtin_classic_passes_every_invariant() {
    let found = lint_in_all_languages(BUILTIN_CLASSIC);
    assert!(found.is_empty(), "builtin:classic has {}", describe("builtin:classic", &found));
}

#[test]
fn reference_passes_every_invariant() {
    let found = lint_in_all_languages(REFERENCE);
    assert!(found.is_empty(), "the reference package has {}", describe("reference.plxhud.json", &found));
}

/// The four other example packages, and the `(code, pointer)` pairs each is pinned to lint to. Every list
/// is empty: all four are clean in every shipped language, so a finding in one is a new finding and a
/// change to a check or to the fixture, to be justified where it is added to its list.
const OTHER_FIXTURES: [(&str, &str, &[(&str, &str)]); 4] = [
    ("hero-center", HERO, &[]),
    ("minimal", MINIMAL, &[]),
    ("palette", PALETTE, &[]),
    ("tabs", TABS, &[]),
];

#[test]
fn other_fixtures_lint_without_an_error() {
    for (name, text, pinned) in OTHER_FIXTURES {
        let found = lint_in_all_languages(text);
        let errors: Vec<Finding> = found.iter().filter(|f| f.severity == Severity::Error).cloned().collect();
        assert!(errors.is_empty(), "{name} has an error: {}", describe(name, &errors));
        let got: BTreeSet<(&str, &str)> = found.iter().map(|f| (f.code, f.pointer.as_str())).collect();
        let want: BTreeSet<(&str, &str)> = pinned.iter().copied().collect();
        assert_eq!(got, want, "{name}: {}", describe(name, &found));
    }
}

#[test]
fn the_goldens_are_clean_language_by_language_too() {
    // The eleven-language run collapses a finding every language shares and tags the rest, so an empty answer
    // already means every language was clean; one language at a time says which, were it not.
    for lang in LintLang::shipped() {
        for (name, text) in [("builtin:classic", BUILTIN_CLASSIC), ("reference", REFERENCE)] {
            let found = lint(text, &ShippedMeasure, std::slice::from_ref(&lang));
            assert!(found.is_empty(), "{name} in `{}` has {}", lang.tag, describe(name, &found));
        }
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

#[test]
fn a_vstack_names_the_rect_it_was_laid_out_in() {
    let lang = LintLang::for_tag("en").expect("English");
    let package = package(REFERENCE);
    let nodes = nodes(&package);
    let compiled = compile(&package, &FixtureMeasure, CompileCtx { labels: lang.labels.clone() });
    let vstacks: Vec<&str> = nodes.iter().filter(|(_, o)| o.node == "vstack").map(|(p, _)| *p).collect();
    assert!(!vstacks.is_empty(), "the reference has vstacks");
    for pointer in &vstacks {
        let placed: Vec<Rect> = compiled.states().iter().filter_map(|state| state.vstack_rect(pointer)).collect();
        assert!(!placed.is_empty(), "{pointer}: some state places it");
        assert!(placed.iter().all(|r| r.x >= 0.0 && r.w > 0.0), "{pointer}: {placed:?}");
    }
    // the info card's vstack is a fixed 760 px wide, wherever a state puts it
    let info = "/layout/block/6/cells/1/vstack";
    assert!(compiled.states().iter().filter_map(|state| state.vstack_rect(info)).all(|r| r.w == 760.0));
    // what is no vstack, and what is no node, has no rect
    for state in compiled.states() {
        assert!(state.vstack_rect("/layout/block/6/cells/1").is_none());
        assert!(state.vstack_rect("/layout/block/6/cells/1/vstack/cells/0/text").is_none());
        assert!(state.vstack_rect("").is_none());
    }
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

/// The first invalid case whose own `code` is `code` (some codes have several): the tests below build on
/// its base and edits, so a fixture edit lives in one place, the check module that owns the code.
fn case_for(code: &str) -> &'static InvalidCase {
    invalid_cases().into_iter().find(|c| c.code == code).unwrap_or_else(|| panic!("no invalid case for {code}"))
}

/// Every one of the fourteen codes the lint registers has an invalid case of its own (a case whose `code`
/// it is: being only in another case's `also` list proves the code fires, not that its own fault is
/// pinned), or is in a check module's `UNIT_ONLY` with the reason no parseable package reaches it.
#[test]
fn every_new_code_has_an_invalid_case() {
    // the fourteen are the whole of the lint families: a code registered in one of them without a case is
    // caught here, whatever LINT_CODES says
    let registered: BTreeSet<&str> = codes::ALL
        .iter()
        .map(|c| c.id)
        .filter(|id| ["HUD-E06", "HUD-W06", "HUD-E10", "HUD-E11", "HUD-E12", "HUD-E13"].iter().any(|p| id.starts_with(p)))
        .collect();
    assert_eq!(registered, LINT_CODES.iter().copied().collect::<BTreeSet<_>>(), "the lint's code families");

    let cased: BTreeSet<&str> = invalid_cases().iter().map(|c| c.code).collect();
    let unit_only: BTreeSet<&str> = unit_only().into_iter().map(|(code, _)| code).collect();
    for code in LINT_CODES {
        assert!(
            cased.contains(code) || unit_only.contains(code),
            "{code} has no invalid case and no unit-only reason (cased: {cased:?}, unit only: {unit_only:?})"
        );
        assert!(
            !(cased.contains(code) && unit_only.contains(code)),
            "{code} has an invalid case, so its UNIT_ONLY entry is stale"
        );
    }
    // No code is out of a parseable package's reach: the menu's panel over its own opener (M1) is reached by an
    // opener wider than 800 px, whose pop outgrows PLAYER_MENU_GAP (lint_menus, the last INVALID case)
    assert!(unit_only.is_empty(), "every lint code has an invalid case; UNIT_ONLY is for the exception: {unit_only:?}");
}

#[test]
fn every_lint_finding_says_what_is_wrong_what_to_change_and_where() {
    for case in invalid_cases() {
        let text = apply(case.base, case.edits);
        let found = lint_with(&text, &ShippedMeasure, &LintLang::shipped(), &LintOptions { text_skew: case.skew });
        let ours: Vec<&Finding> = found.iter().filter(|f| LINT_CODES.contains(&f.code)).collect();
        assert!(!ours.is_empty(), "case `{}`: no lint finding", case.name);
        for f in ours {
            let at = format!("case `{}`: {} at {}", case.name, f.code, f.pointer);
            assert!(!f.message.is_empty() && !f.hint.is_empty(), "{at}: a finding names the fault and the fix");
            assert_eq!(f.severity == Severity::Warning, f.code == "HUD-W0602", "{at}: only W0602 is a warning");
            if f.code == "HUD-E0604" {
                // the compile finds it once, in the typed tree: no language, state or slot
                assert_eq!(f.ctx, Context::default(), "{at}");
            } else {
                assert!(f.ctx.state.is_some() && f.ctx.slot.is_some(), "{at}: the state and slot it was found in");
                assert!(!f.ctx.rects.is_empty() || f.ctx.text.is_some(), "{at}: the rects or the text it concerns");
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// The whole run
// ---------------------------------------------------------------------------------------------------

/// The `(code, pointer)` pairs a case is expected to yield: its own and its `also` list.
fn expected_of(case: &InvalidCase) -> BTreeSet<(String, String)> {
    std::iter::once((case.code, case.pointer))
        .chain(case.also.iter().copied())
        .map(|(code, pointer)| (code.to_owned(), pointer.to_owned()))
        .collect()
}

#[test]
fn findings_are_sorted_by_pointer_and_deterministic() {
    for case in invalid_cases() {
        let text = apply(case.base, case.edits);
        let opts = LintOptions { text_skew: case.skew };
        let first = lint_with(&text, &ShippedMeasure, &LintLang::shipped(), &opts);
        let again = lint_with(&text, &ShippedMeasure, &LintLang::shipped(), &opts);
        assert!(!first.is_empty(), "case `{}`", case.name);
        assert_eq!(first, again, "case `{}`: the same package lints to the same findings, in the same order", case.name);
        assert_eq!(first, report::sorted_owned(&first), "case `{}`: the lint returns report order", case.name);
        // the order is the report's, not the order the checks happened to run in
        let mut backwards = first.clone();
        backwards.reverse();
        assert_eq!(report::sorted_owned(&backwards), first, "case `{}`: sorting does not depend on arrival order", case.name);
    }
}

#[test]
fn a_report_with_findings_at_several_pointers_reads_in_document_order() {
    // lint_menus' wide-row case: findings from four checks at five pointers across the title column, the
    // control row and Up Next
    let case = case_for("HUD-E1302");
    let found = lint_with(&apply(case.base, case.edits), &ShippedMeasure, &LintLang::shipped(), &LintOptions::default());
    let mut pointers: Vec<&str> = found.iter().map(|f| f.pointer.as_str()).collect();
    pointers.dedup();
    assert_eq!(pointers, EXPECTED_POINTERS, "{}", describe("wide-row", &found));
}

/// Where the wide-row case reports, in report order: the title column's kicker (`cells/0`), the three
/// openers of the control row (`cells/1`) and Up Next, which the package lists after its blocks. (How tokens
/// compare, `/block/2` before `/block/10`, is `report`'s own test; this pins a real lint's order.)
const EXPECTED_POINTERS: [&str; 5] = [
    "/layout/block/0/cells/0/vstack/cells/0/text",
    "/layout/block/0/cells/1/segment/items/2",
    "/layout/block/0/cells/1/segment/items/3",
    "/layout/block/0/cells/1/segment/items/4",
    "/layout/up_next",
];

/// The `(code, pointer)` pairs `edits` yield on `base`, linted in `langs` with `skew`.
fn lint_set(base: Base, edits: &[(&str, &str)], langs: &[&str], skew: f32) -> BTreeSet<(String, String)> {
    let langs: Vec<LintLang> = langs.iter().map(|t| LintLang::for_tag(t).expect("a shipped tag")).collect();
    lint_with(&apply(base, edits), &ShippedMeasure, &langs, &LintOptions { text_skew: skew })
        .iter()
        .map(|f| (f.code.to_owned(), f.pointer.clone()))
        .collect()
}

#[test]
fn lint_runs_to_the_end_and_reports_every_finding() {
    // Three independent faults of the reference, one for each of three check modules that run in turn:
    // the compile's item-keyed gap (lint_size, the last), a title box past SAFE (lint_safe, the second) and
    // a chapter card too narrow for its fallback name (lint_text). Each is the edit of that module's own case.
    let cases = [case_for("HUD-E0604"), case_for("HUD-E1102"), case_for("HUD-E0601")];
    assert!(cases.iter().all(|c| c.base == Base::Reference));
    let edits: Vec<(&str, &str)> = cases.iter().flat_map(|c| c.edits.iter().copied()).collect();
    let mut want = BTreeSet::new();
    for case in cases {
        want.extend(expected_of(case));
        // alone, each case finds its own fault ...
        assert_eq!(lint_set(case.base, case.edits, &["en"], case.skew), expected_of(case), "{}", case.name);
    }
    // ... and together one run finds all three, none hiding another
    assert_eq!(lint_set(Base::Reference, &edits, &["en"], 0.0), want);
    assert_eq!(want.len(), 3, "three codes, three pointers: {want:?}");
}

#[test]
fn a_fault_in_a_package_that_does_not_parse_is_the_only_thing_reported() {
    // The converse, pinned so the run's end is where the lint says it is: the driver stops only when there
    // is no package to compile, which is a parse or validate-table error (`parse` returns `Some` exactly
    // when it found no error). Classic with an unknown command (a parse finding) AND a decor image under a
    // button (a lint finding).
    let mut edits = SUPPORT_CASE.edits.to_vec();
    edits.extend_from_slice(case_for("HUD-E1003").edits);
    let found = lint_with(&apply(Base::Classic, &edits), &ShippedMeasure, &LintLang::shipped(), &LintOptions::default());
    let got: BTreeSet<(&str, &str)> = found.iter().map(|f| (f.code, f.pointer.as_str())).collect();
    assert_eq!(got, BTreeSet::from([(SUPPORT_CASE.code, SUPPORT_CASE.pointer)]), "{}", describe("broken", &found));
}

/// The reference's More cell gated on `title.is_episode` (R1: More is not placed in a movie's root state,
/// `HUD-E0801`) and its `info` disc drawn at 20 px (`HUD-E0603`, a geometric finding of stage 3).
const MORE_GATED_AND_INFO_SMALL: [(&str, &str); 2] = [
    (
        r#""command": "open_more", "widget": "disc", "size": 52,"#,
        r#""command": "open_more", "when": "title.is_episode", "widget": "disc", "size": 52,"#,
    ),
    (r#""section": "info", "widget": "disc", "size": 52,"#, r#""section": "info", "widget": "disc", "size": 20,"#),
];

#[test]
fn a_reachability_error_does_not_hide_the_geometric_findings() {
    // The spec ("LLM authoring" item 2) has the lint run to the end and report EVERY finding, so one round
    // fixes many. The package parses (nothing to compile is missing), so a reach error (R1 here) is reported
    // together with what stage 3 finds in the same package, not in the round after the author fixed it.
    let got = lint_set(Base::Reference, &MORE_GATED_AND_INFO_SMALL, &["en"], 0.0);
    let want: BTreeSet<(String, String)> = [
        ("HUD-E0801", "/layout/block/2/cells/1/segment/items/2/when"),
        ("HUD-E0603", "/layout/block/2/cells/1/segment/items/1"),
    ]
    .into_iter()
    .map(|(code, pointer)| (code.to_owned(), pointer.to_owned()))
    .collect();
    assert_eq!(got, want);
}

/// The classic control row's More item, and the pointer of the row it sits in (`reach_tests` has the same two).
const CLASSIC_MORE: &str = r#"{ "id": "more", "command": "open_more", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" }"#;
const CLASSIC_ROW: &str = "/layout/block/0/cells/1/segment/items";

#[test]
fn every_kind_of_reach_error_is_linted_on_to_stage_three_in_every_language() {
    // Continuing past the reach and validate findings is only sound while `compile` and the six checks are
    // total over a package that parsed (`pr5c-handoff.md`). This runs one package for each rule of `check`
    // (R1, R3, R4, I3, I9) through all of stage 3 in the eleven languages: a panic in the compile or in a check
    // fails it, the way an index into a part the broken package lacks would. Each must still report its own
    // reach code at its pointer.
    let more_absent = format!(",\n          {CLASSIC_MORE}");
    let more_gated = CLASSIC_MORE.replace(" }", r#", "when": "title.is_episode" }"#);
    let cases: [(&str, Vec<(&str, &str)>, &str, String); 9] = [
        ("more gated on the episode", vec![(CLASSIC_MORE, &more_gated)], "HUD-E0801", format!("{CLASSIC_ROW}/4/when")),
        ("more absent", vec![(&more_absent, "")], "HUD-E0801", "/layout/block".to_owned()),
        (
            "more opens a page",
            vec![(r#""command": "open_more", "widget""#, r#""command": "open_more", "page": "quality", "widget""#)],
            "HUD-E0801",
            format!("{CLASSIC_ROW}/4"),
        ),
        (
            "more in a band shown only in a section",
            vec![(r#"{ "id": "top", "when": "!sections","#, r#"{ "id": "top", "when": "panel.info","#)],
            "HUD-E0801",
            "/layout/block/0/when".to_owned(),
        ),
        (
            "an opener with no band to open",
            vec![(
                r#"{ "id": "info", "when": "panel.info", "cells""#,
                r#"{ "id": "info", "when": "!sections", "cells""#,
            )],
            "HUD-E0802",
            "/layout/block/4/cells/0/segment/items/0/section".to_owned(),
        ),
        (
            "up next replaces nothing",
            vec![(r#""replaces": [ "row" ]"#, r#""replaces": [ ]"#)],
            "HUD-E0803",
            "/layout/up_next/replaces".to_owned(),
        ),
        (
            "up next names a band that is not there",
            vec![(r#""band": "top""#, r#""band": "nope""#)],
            "HUD-E0803",
            "/layout/up_next/band".to_owned(),
        ),
        (
            "two controls with one id",
            vec![(r#""id": "audio", "command""#, r#""id": "subs", "command""#)],
            "HUD-E0901",
            format!("{CLASSIC_ROW}/3/id"),
        ),
        (
            "a palette in the built-in",
            vec![
                (r#""license": "GPL-3.0-or-later","#, r##""license": "GPL-3.0-or-later", "palette": { "gold": "#f2b535" },"##),
                (r#""track": "RAIL_TRACK""#, r#""track": "gold""#),
            ],
            "HUD-E0902",
            "/palette".to_owned(),
        ),
    ];
    for (name, edits, code, pointer) in &cases {
        let found = lint(&apply(Base::Classic, edits), &ShippedMeasure, &LintLang::shipped());
        assert!(
            found.iter().any(|f| f.code == *code && f.pointer == *pointer),
            "{name}: {code} at {pointer} expected\n{}",
            describe(name, &found)
        );
    }
}

// ---------------------------------------------------------------------------------------------------
// The report and the language context
// ---------------------------------------------------------------------------------------------------

/// The reference's info disc drawn at 20 px, linted in one language: one finding, E0603.
fn small_disc(langs: &[&str]) -> Vec<Finding> {
    let case = case_for("HUD-E0603");
    let langs: Vec<LintLang> = langs.iter().map(|t| LintLang::for_tag(t).expect("a shipped tag")).collect();
    lint(&apply(case.base, case.edits), &ShippedMeasure, &langs)
}

#[test]
fn json_output_of_an_invalid_case_is_stable() {
    let found = small_disc(&["en"]);
    assert_eq!(found.len(), 1, "{}", describe("small-disc.plxhud.json", &found));
    let json = report::json_text("small-disc.plxhud.json", &found);
    assert_eq!(json, EXPECTED_JSON, "{json}");
    // and it is what the text report names, one line
    assert_eq!(report::text("small-disc.plxhud.json", &found).lines().count(), 1);
}

/// `report::json_text` of the one finding above: the registry's CI reads this shape. Keys are in
/// `serde_json`'s sorted order (`context` between `code` and `hint`; inside it `lang, rects, slot, state`).
/// The `message` and `hint` are `lint_size`'s wording, so a rewording there is a one-line edit here.
const EXPECTED_JSON: &str = r##"{
  "file": "small-disc.plxhud.json",
  "findings": [
    {
      "code": "HUD-E0603",
      "context": {
        "lang": "en",
        "rects": [
          {
            "h": 20.0,
            "name": "info",
            "w": 20.0,
            "x": 1688.0,
            "y": 960.0
          }
        ],
        "slot": "discs",
        "state": "compact,movie"
      },
      "hint": "Set `size` to at least 26; the app's own control faces are 56 to 64 px (classic's discs are 64).",
      "message": "control `info` is drawn 20 x 20 px, under the 26 px floor on its smaller side (the narrowest seat the app's own screens give a focus target)",
      "nearest": [],
      "path": "/layout/block/2/cells/1/segment/items/1",
      "rule": "min-control-size",
      "severity": "error"
    }
  ],
  "lint": 1
}
"##;

#[test]
fn collapsed_language_context() {
    // A fault that does not depend on the language (a disc's size) is one finding without a language when
    // the eleven languages are linted together ...
    let all = small_disc(&LintLang::shipped().iter().map(|l| l.tag).collect::<Vec<_>>());
    assert_eq!(all.len(), 1, "{}", describe("small-disc", &all));
    assert_eq!(all[0].ctx.lang, None, "identical in every language: reported once, for all of them");
    // ... and the same finding carries its tag when only that language is linted
    for tag in ["de", "ru", "en"] {
        let one = small_disc(&[tag]);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].ctx.lang.as_deref(), Some(tag));
        let (mut a, mut b) = (one[0].ctx.clone(), all[0].ctx.clone());
        a.lang = None;
        b.lang = None;
        assert_eq!(a, b, "everything but the language is the same");
        assert_eq!((one[0].code, &one[0].pointer, &one[0].message), (all[0].code, &all[0].pointer, &all[0].message));
    }
    // A fault that depends on the language (a text's width) keeps the language it was measured in.
    let case = case_for("HUD-E0601");
    let text = apply(case.base, case.edits);
    let tags: Vec<&str> = LintLang::shipped().iter().map(|l| l.tag).collect();
    let by_language = lint(&text, &ShippedMeasure, &LintLang::shipped());
    let seen: BTreeSet<Option<&str>> = by_language.iter().filter(|f| f.code == case.code).map(|f| f.ctx.lang.as_deref()).collect();
    assert!(!seen.is_empty() && !seen.contains(&None), "a width differs by language: {seen:?}");
    assert!(seen.iter().flatten().all(|t| tags.contains(t)), "{seen:?}");
}
