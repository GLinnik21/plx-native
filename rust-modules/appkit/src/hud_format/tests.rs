//! **The test corpus of the HUD package format.** Valid fixtures that parse with no finding, an invalid
//! corpus with one row per parse rule and per command parameter (each asserting its frozen code AND its
//! JSON pointer), seeded mutation tests that must never panic, and the drift test that walks the
//! descriptor table so the table and the parser cannot disagree.
//!
//! Everything here is data in, findings out: fixtures come in by `include_str!`, the PRNG is a
//! fixed-seed xorshift written below, and nothing reads a clock, a file or the environment.
//!
//! Where a case documents what `parse.rs` does that the table's module doc words differently, the test
//! asserts what the parser does and the comment says so:
//! * a cell with NO content key is `E0106` at the cell (the `schema` module doc says `E0103`);
//! * every malformed `format` (a number, `null`, `"1"`, `"v1.0"`, `"1.0.0"`, a missing key) is `E0401`
//!   at `/format`, never `E0104`: the version gate answers alone, before any type is judged;
//! * `E0107` (a key from a later minor) has no row: format 1.0 is the first and only minor, so no key
//!   can be newer than a package that parses. `parse::tests` builds a hand-made node to reach it.

use plx_ui::rec::strict_json::{decode_ordered, Ordered};

use super::parse::{self, Ctx, Object, Parsed, Val};
use super::schema::{self, Cmd, Node, NodeKind, Pattern, Rule, Ty};
use super::svg_subset;
use super::{codes, has_error, Code, Finding, MAX_NEAREST, REASON_NEWER, REASON_OLDER};

const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");

/// Where things stand in `REFERENCE`: the playbar, the two segments' controls, the two stacks of text,
/// the chapters strip.
const BAR: &str = "/layout/block/1/cells/0/playbar";
const CENTER: &str = "/layout/block/2/cells/0/segment/items";
const RIGHT: &str = "/layout/block/2/cells/1/segment/items";
const TITLE_CELLS: &str = "/layout/block/0/cells/0/vstack/cells";
const INFO_CELLS: &str = "/layout/block/6/cells/1/vstack/cells";
const STRIP: &str = "/layout/block/4/cells/0/strip";

// ---------------------------------------------------------------------------------------------------
// Helpers shared by every group
// ---------------------------------------------------------------------------------------------------

/// Whether `pointer` is RFC 6901: empty, or `/` first and every `~` followed by `0` or `1`.
fn well_formed_pointer(pointer: &str) -> bool {
    if pointer.is_empty() {
        return true;
    }
    if !pointer.starts_with('/') {
        return false;
    }
    let bytes = pointer.as_bytes();
    bytes
        .iter()
        .enumerate()
        .all(|(i, &b)| b != b'~' || matches!(bytes.get(i + 1), Some(b'0') | Some(b'1')))
}

/// The lint shape every finding of every test must have: a code of the table with its own rule and
/// severity, an RFC 6901 pointer, a message and a hint, and at most [`MAX_NEAREST`] nearest names.
fn assert_lint_shaped(f: &Finding) {
    let code = codes::ALL
        .iter()
        .find(|c| c.id == f.code)
        .unwrap_or_else(|| panic!("`{}` is not a code of the table: {f:?}", f.code));
    assert_eq!(f.rule, code.rule, "a finding carries its code's rule: {f:?}");
    assert_eq!(f.severity, code.severity, "and its severity: {f:?}");
    assert!(
        well_formed_pointer(&f.pointer),
        "not an RFC 6901 pointer: {f:?}"
    );
    assert!(!f.message.is_empty(), "no message: {f:?}");
    assert!(!f.hint.is_empty(), "no hint: {f:?}");
    assert!(f.nearest.len() <= MAX_NEAREST, "too many nearest: {f:?}");
}

/// `code@pointer` for every finding, one per line: what a failure prints.
fn dump(findings: &[Finding]) -> String {
    findings
        .iter()
        .map(|f| format!("  {}@{:?}: {}", f.code, f.pointer, f.message))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first finding of `code` at `pointer`; a failure prints every finding.
fn assert_finding<'a>(findings: &'a [Finding], code: &Code, pointer: &str) -> &'a Finding {
    findings
        .iter()
        .find(|f| f.code == code.id && f.pointer == pointer)
        .unwrap_or_else(|| {
            panic!(
                "expected {} at {pointer:?}, found:\n{}",
                code.id,
                dump(findings)
            )
        })
}

/// `(code, pointer)` of every finding, sorted: the shape a table row compares.
fn pairs(findings: &[Finding]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = findings
        .iter()
        .map(|f| (f.code.to_owned(), f.pointer.clone()))
        .collect();
    out.sort();
    out
}

/// Parse, and keep the two invariants every parse must: a package exists exactly when no finding is an
/// error, and every finding has the lint shape.
fn parse_checked(bytes: &[u8]) -> Parsed {
    let parsed = parse::parse(bytes);
    assert_eq!(
        parsed.package.is_some(),
        !has_error(&parsed.findings),
        "a package exists exactly when no finding is an error:\n{}",
        dump(&parsed.findings)
    );
    parsed.findings.iter().for_each(assert_lint_shaped);
    parsed
}

/// The findings of a package text.
fn findings_of(json: &str) -> Vec<Finding> {
    parse_checked(json.as_bytes()).findings
}

/// `src` with the one occurrence of `from` replaced by `to`; zero or several occurrences is a broken
/// test, never a silent no-op.
fn edit(src: &str, from: &str, to: &str) -> String {
    let n = src.matches(from).count();
    assert_eq!(n, 1, "`{from}` must occur exactly once in the text, found {n}");
    src.replace(from, to)
}

fn reference(from: &str, to: &str) -> String {
    edit(REFERENCE, from, to)
}

/// One invalid-corpus row: a package (as bytes, since some rows are not UTF-8), and the finding it must
/// produce: its code, its pointer, optionally a name that must be among the nearest, optionally the
/// exact message, and whether it is the ONLY finding.
pub(in crate::hud_format) struct Row {
    pub(in crate::hud_format) label: &'static str,
    pub(in crate::hud_format) bytes: Vec<u8>,
    pub(in crate::hud_format) code: &'static Code,
    pub(in crate::hud_format) pointer: String,
    near: Option<&'static str>,
    message: Option<&'static str>,
    only: bool,
}

fn row(label: &'static str, text: String, code: &'static Code, pointer: &str) -> Row {
    Row {
        label,
        bytes: text.into_bytes(),
        code,
        pointer: pointer.to_owned(),
        near: None,
        message: None,
        only: false,
    }
}

impl Row {
    fn near(mut self, name: &'static str) -> Row {
        self.near = Some(name);
        self
    }
    fn message(mut self, message: &'static str) -> Row {
        self.message = Some(message);
        self
    }
    fn only(mut self) -> Row {
        self.only = true;
        self
    }
    /// `Err` names what this row expected and what the parser answered instead.
    fn verify(&self) -> Result<(), String> {
        let parsed = parse_checked(&self.bytes);
        let at = format!("row `{}`", self.label);
        if parsed.package.is_some() {
            return Err(format!("{at}: an invalid package must not parse"));
        }
        let Some(found) = parsed
            .findings
            .iter()
            .find(|f| f.code == self.code.id && f.pointer == self.pointer)
        else {
            return Err(format!(
                "{at}: expected {} at {:?}, found:\n{}",
                self.code.id,
                self.pointer,
                dump(&parsed.findings)
            ));
        };
        if let Some(name) = self.near {
            if !found.nearest.iter().any(|n| n == name) {
                return Err(format!(
                    "{at}: `{name}` must be among the nearest names, found {:?}",
                    found.nearest
                ));
            }
        }
        if let Some(message) = self.message {
            if found.message != message {
                return Err(format!("{at}: message {:?}, expected {message:?}", found.message));
            }
        }
        if self.only && parsed.findings.len() != 1 {
            return Err(format!(
                "{at}: expected this finding alone, found:\n{}",
                dump(&parsed.findings)
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------------
// Valid fixtures
// ---------------------------------------------------------------------------------------------------

pub(in crate::hud_format) mod valid {
    use super::*;

    #[test]
    fn the_reference_example_parses_with_no_finding() {
        let parsed = parse_checked(REFERENCE.as_bytes());
        assert!(
            parsed.findings.is_empty(),
            "the worked example of the spec is a valid package:\n{}",
            dump(&parsed.findings)
        );
        let package = parsed.package.expect("a package");
        assert_eq!(package.format, (1, 0));
        assert_eq!(package.id, "com.plxnative.examples.reference");
        assert_eq!(package.palette.len(), 1);
        assert_eq!(package.assets.len(), 6);
        assert_eq!(package.layout.node, "layout");
    }

    #[test]
    fn the_minimal_package_parses_with_no_finding() {
        let parsed = parse_checked(MINIMAL.as_bytes());
        assert!(parsed.findings.is_empty(), "{}", dump(&parsed.findings));
        let package = parsed.package.expect("a package");
        assert_eq!(package.id, "com.plxnative.examples.minimal");
        assert!(package.palette.is_empty() && package.assets.is_empty() && package.app.is_none());
    }

    #[test]
    fn parsing_is_pure() {
        for text in [REFERENCE, MINIMAL] {
            assert_eq!(parse::parse(text.as_bytes()), parse::parse(text.as_bytes()));
        }
    }

    #[test]
    fn the_typed_tree_keeps_document_order_and_keyed_cases() {
        let package = parse_checked(REFERENCE.as_bytes()).package.expect("a package");
        let block = package
            .layout
            .get("block")
            .and_then(Val::as_list)
            .expect("block");
        let ids: Vec<&str> = block
            .iter()
            .filter_map(Val::as_object)
            .filter_map(|b| b.get("id").and_then(Val::as_str))
            .collect();
        assert_eq!(
            ids,
            [
                "title",
                "bar",
                "row",
                "chapters_head",
                "chapters",
                "info_rule",
                "info"
            ]
        );
        // The keyed `gap_before` of the chapters head keeps its base and its one case.
        let head = block[3].as_object().expect("band");
        let Some(Val::Keyed(gap)) = head.get("gap_before") else {
            panic!("gap_before is state-keyed: {:?}", head.get("gap_before"));
        };
        assert_eq!(gap.base.as_str(), Some("SM"));
        assert_eq!(gap.cases.len(), 1);
        assert_eq!(gap.cases[0].0.terms[0].name, "panel.info");
        assert_eq!(gap.cases[0].1.as_str(), Some("XS"));
    }

    #[test]
    fn keyed_cases_stay_in_document_order() {
        let text = reference(
            r##""if": { "panel.info": "XS" } }, "cells": ["##,
            r##""if": { "sections": "XS", "panel.info": "MD", "!panel.chapters": "LG" } }, "cells": ["##,
        );
        let package = parse_checked(text.as_bytes()).package.expect("a package");
        let block = package.layout.get("block").and_then(Val::as_list).expect("block");
        let Some(Val::Keyed(gap)) = block[3].as_object().and_then(|b| b.get("gap_before")) else {
            panic!("keyed");
        };
        let names: Vec<(&str, bool)> = gap
            .cases
            .iter()
            .map(|(c, _)| (c.terms[0].name.as_str(), c.terms[0].negated))
            .collect();
        assert_eq!(
            names,
            [("sections", false), ("panel.info", false), ("panel.chapters", true)],
            "the first condition that holds wins, so the order is the document's"
        );
    }

    /// Edits of the reference that must still parse with no finding: the boundary values, every
    /// condition form, and the choices a package may make.
    pub(in crate::hud_format) fn accepted() -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = vec![
            (
                "when_and_array",
                reference(
                    r##"{ "id": "info_rule", "when": "panel.info""##,
                    r##"{ "id": "info_rule", "when": [ "panel.info", "!transport.paused" ]"##,
                ),
            ),
            (
                "when_negated",
                reference(
                    r##"{ "id": "info_rule", "when": "panel.info""##,
                    r##"{ "id": "info_rule", "when": "!panel.chapters""##,
                ),
            ),
            (
                "when_array_of_one",
                reference(
                    r##"{ "id": "info_rule", "when": "panel.info""##,
                    r##"{ "id": "info_rule", "when": [ "!sections" ]"##,
                ),
            ),
            (
                "keyed_negated_condition",
                reference(r##""if": { "panel.info": "XS" }"##, r##""if": { "!panel.info": "XS" }"##),
            ),
            (
                "scrim_extra_is_the_classic_height",
                reference(r##""extra": 120"##, r##""extra": 470"##),
            ),
            ("scrim_extra_zero", reference(r##""extra": 120"##, r##""extra": 0"##)),
            (
                "scrim_fixed",
                reference(
                    r##""scrim": { "cover": "block", "extra": 120 }"##,
                    r##""scrim": { "cover": "fixed", "height": 470 }"##,
                ),
            ),
            ("no_scrim", reference(r##""scrim": { "cover": "block", "extra": 120 },"##, "")),
            ("no_schema_url", reference(r##""$schema": "https://plxnative.com/hud/schema/1.0.json","##, "")),
            (
                "an_app_range",
                reference(
                    r##""license": "CC0-1.0","##,
                    r##""license": "CC0-1.0", "app": { "min": "0.9.0", "max": "2.0.0" },"##,
                ),
            ),
            (
                "hud_title_on_the_main_title",
                reference(r##""size": "TITLE""##, r##""size": "HUD_TITLE""##),
            ),
            (
                "gap_in_px_at_both_bounds",
                reference(r##""id": "bar", "gap_before": "SM""##, r##""id": "bar", "gap_before": 0"##),
            ),
            (
                "gap_in_px_at_the_canvas_height",
                reference(r##""id": "bar", "gap_before": "SM""##, r##""id": "bar", "gap_before": 1080"##),
            ),
            (
                "no_palette_and_an_ink_token",
                reference(r##""palette": { "gold": "#f2b535" },"##, "")
                    .replace(r##""fill": "gold""##, r##""fill": "ACCENT""##),
            ),
            (
                "an_empty_palette_and_assets_are_fine_for_a_package_that_uses_neither",
                MINIMAL.replace(
                    r##""license": "CC0-1.0","##,
                    r##""license": "CC0-1.0", "palette": {}, "assets": {},"##,
                ),
            ),
            (
                "every_facts_name_once",
                reference(
                    r##""order": [ "duration", "year", "content_rating", "genres" ]"##,
                    r##""order": [ "genres", "year", "duration" ]"##,
                ),
            ),
            (
                "facts_badges_without_the_rating_in_order",
                reference(
                    r##""order": [ "duration", "year", "content_rating", "genres" ]"##.to_owned().as_str(),
                    r##""order": [ "duration" ]"##,
                )
                .replace(r##""badges": []"##, r##""badges": [ "content_rating", "cc", "sdh" ]"##),
            ),
            (
                "seek_by_a_large_literal",
                reference(r##""by": 30"##, r##""by": 2147483647"##),
            ),
            (
                "an_unused_asset_and_an_unused_palette_entry_are_lint_not_parse",
                reference(
                    r##""gold": "#f2b535""##,
                    r##""gold": "#f2b535", "unused": "#000000""##,
                ),
            ),
        ];
        // Every condition of the table, on a band: the card-scoped ones only inside a card.
        for condition in schema::CONDITIONS {
            if !schema::is_card_scoped(condition.name) {
                out.push((
                    "every_band_condition",
                    reference(
                        r##"{ "id": "info_rule", "when": "panel.info""##,
                        &format!(r##"{{ "id": "info_rule", "when": "{}""##, condition.name),
                    ),
                ));
            }
        }
        out
    }

    #[test]
    fn variants_of_the_reference_that_must_still_parse() {
        for (label, text) in accepted() {
            let parsed = parse_checked(text.as_bytes());
            assert!(
                parsed.findings.is_empty() && parsed.package.is_some(),
                "`{label}` must parse with no finding:\n{}",
                dump(&parsed.findings)
            );
        }
    }

    #[test]
    fn a_package_at_exactly_the_size_bound_parses_and_one_byte_more_does_not() {
        let mut at = REFERENCE.as_bytes().to_vec();
        at.resize(parse::MAX_PACKAGE_BYTES, b' ');
        let parsed = parse_checked(&at);
        assert!(parsed.findings.is_empty(), "{}", dump(&parsed.findings));
        at.push(b' ');
        let parsed = parse_checked(&at);
        assert_eq!(pairs(&parsed.findings), [("HUD-E0100".to_owned(), String::new())]);
    }

    #[test]
    fn the_blocks_bound_is_a_boundary() {
        // 14 extra bands plus the minimal package's two make exactly 16.
        let extra = |n: usize| -> String {
            (0..n)
                .map(|i| format!(r##"{{ "id": "b{i}", "cells": [] }}, "##))
                .collect()
        };
        let sixteen = edit(MINIMAL, r##""block": ["##, &format!(r##""block": [ {}"##, extra(14)));
        assert!(findings_of(&sixteen).is_empty());
        let seventeen = edit(MINIMAL, r##""block": ["##, &format!(r##""block": [ {}"##, extra(15)));
        let found = findings_of(&seventeen);
        assert_finding(&found, &codes::E0105, "/layout/block");
    }
}

// ---------------------------------------------------------------------------------------------------
// The invalid corpus
// ---------------------------------------------------------------------------------------------------

/// A palette-less `format` row: replace the value of `"format"`.
fn format_row(label: &'static str, value: &str, code: &'static Code) -> Row {
    row(
        label,
        reference(r##""format": "1.0""##, &format!(r##""format": {value}"##)),
        code,
        "/format",
    )
    .only()
}

/// Every row of the corpus. One row per parse rule at least (see `corpus_covers_every_code`).
pub(in crate::hud_format) fn corpus() -> Vec<Row> {
    let r = reference;
    let mut rows: Vec<Row> = Vec::new();
    let mut add = |row: Row| rows.push(row);

    // ----- HUD-E0102: an unknown key (at the key's own pointer, with the near-miss's neighbour) -----
    add(row(
        "unknown_key_top_level",
        r(r##""license": "CC0-1.0","##, r##""license": "CC0-1.0", "licence": "CC0-1.0","##),
        &codes::E0102,
        "/licence",
    )
    .near("license")
    .only());
    add(row(
        "unknown_key_layout",
        r(r##""sections": "inline","##, r##""sections": "inline", "sectoins": "modal","##),
        &codes::E0102,
        "/layout/sectoins",
    )
    .near("sections")
    .only());
    add(row(
        "unknown_key_text_cell",
        r(r##""size": "TITLE","##, r##""size": "TITLE", "boldd": true,"##),
        &codes::E0102,
        &format!("{TITLE_CELLS}/1/text/boldd"),
    )
    .near("bold")
    .only());
    add(row(
        "unknown_key_playbar",
        r(r##""thickness": 6,"##, r##""thickness": 6, "thicknes": 6,"##),
        &codes::E0102,
        &format!("{BAR}/thicknes"),
    )
    .near("thickness")
    .only());
    add(row(
        "unknown_key_scrim",
        r(r##""extra": 120 }"##, r##""extra": 120, "ekstra": 1 }"##),
        &codes::E0102,
        "/layout/scrim/ekstra",
    )
    .near("extra")
    .only());
    add(row(
        "unknown_key_up_next",
        r(r##""still": false,"##, r##""still": false, "stil": true,"##),
        &codes::E0102,
        "/layout/up_next/stil",
    )
    .near("still")
    .only());
    add(row(
        "unknown_key_strip_card",
        r(r##""below_gap": "XS","##, r##""below_gap": "XS", "below_gp": "XS","##),
        &codes::E0102,
        &format!("{STRIP}/card/below_gp"),
    )
    .near("below_gap")
    .only());
    add(row(
        "unknown_key_in_a_band",
        r(r##"{ "id": "info_rule", "when": "panel.info","##, r##"{ "id": "info_rule", "wen": "panel.info", "when": "panel.info","##),
        &codes::E0102,
        "/layout/block/5/wen",
    )
    .near("when")
    .only());

    // ----- HUD-E0101: a duplicate key (at the second one) -----
    add(row(
        "duplicate_top_level_name",
        r(r##""name": "Reference","##, r##""name": "Reference", "name": "Again","##),
        &codes::E0101,
        "/name",
    )
    .only());
    add(row(
        "duplicate_playbar_thickness",
        r(r##""thickness": 6,"##, r##""thickness": 6, "thickness": 7,"##),
        &codes::E0101,
        &format!("{BAR}/thickness"),
    )
    .only());
    add(row(
        "duplicate_palette_name",
        r(r##""gold": "#f2b535""##, r##""gold": "#f2b535", "gold": "#f2b536""##),
        &codes::E0101,
        "/palette/gold",
    )
    .only());
    add(row(
        "duplicate_asset_id",
        r(r##""eq": "<svg"##, r##""gear": "<svg viewBox='0 0 24 24'><path d='M0 0h4v4z'/></svg>", "eq": "<svg"##),
        &codes::E0101,
        "/assets/gear",
    )
    .only());
    add(row(
        "duplicate_keyed_condition",
        r(r##""if": { "panel.info": "XS" }"##, r##""if": { "panel.info": "XS", "panel.info": "SM" }"##),
        &codes::E0101,
        "/layout/block/3/gap_before/if/panel.info",
    )
    .only());
    add(row(
        "duplicate_command_parameter",
        r(r##""tab": "subtitles","##, r##""tab": "subtitles", "tab": "audio","##),
        &codes::E0101,
        &format!("{CENTER}/0/tab"),
    )
    .only());
    add(row(
        "duplicate_key_of_an_unknown_name_is_both",
        r(r##""license": "CC0-1.0","##, r##""license": "CC0-1.0", "zz": 1, "zz": 2,"##),
        &codes::E0101,
        "/zz",
    ));

    // ----- HUD-E0103: a required key is missing (at the object that lacks it) -----
    add(row("missing_id", r(r##""id": "com.plxnative.examples.reference","##, ""), &codes::E0103, "").only());
    add(row(
        "missing_countdown",
        r(r##""countdown": "ring""##, r##""cdown": "ring""##),
        &codes::E0103,
        "/layout/up_next",
    ));
    add(row(
        "missing_text_bind",
        r(r##""bind": "title.main", "size": "TITLE","##, r##""size": "TITLE","##),
        &codes::E0103,
        &format!("{TITLE_CELLS}/1/text"),
    )
    .only());
    add(row(
        "missing_item_command",
        r(r##""id": "subs", "command": "open_tracks", "tab": "subtitles","##, r##""id": "subs", "tab": "subtitles","##),
        &codes::E0103,
        &format!("{CENTER}/0"),
    )
    .only());
    add(row(
        "missing_item_widget",
        r(r##""tab": "subtitles", "widget": "disc","##, r##""tab": "subtitles","##),
        &codes::E0103,
        &format!("{CENTER}/0"),
    )
    .only());
    add(row(
        "missing_band_cells",
        r(r##"{ "id": "title", "cells": ["##, r##"{ "id": "title", "ghost": [], "gone": ["##),
        &codes::E0103,
        "/layout/block/0",
    ));
    add(row(
        "rest_solid_without_a_focus_is_missing_the_focus",
        r(r##""size": 64, "rest": "solid", "focus": "ring","##, r##""size": 64, "rest": "solid","##),
        &codes::E0103,
        &format!("{CENTER}/3"),
    )
    .only());
    add(row(
        "scrim_block_without_extra",
        r(r##""scrim": { "cover": "block", "extra": 120 }"##, r##""scrim": { "cover": "block" }"##),
        &codes::E0103,
        "/layout/scrim",
    )
    .only());

    // ----- HUD-E0104: a value of the wrong JSON type (at the value) -----
    add(row(
        "thickness_as_a_string",
        r(r##""thickness": 6,"##, r##""thickness": "6","##),
        &codes::E0104,
        &format!("{BAR}/thickness"),
    )
    .only());
    add(row(
        "bold_as_a_string",
        r(r##""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true,"##, r##""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": "yes","##),
        &codes::E0104,
        &format!("{TITLE_CELLS}/1/text/bold"),
    )
    .only());
    add(row(
        "cells_as_an_object",
        r(r##""gap": "XS", "w": "fill", "cells": ["##, r##""gap": "XS", "w": "fill", "cells": { "x": 1 }, "ghost": ["##),
        &codes::E0104,
        &format!("/layout/block/0/cells/0/vstack/cells"),
    ));
    add(row(
        "null_for_a_number",
        r(r##""thickness": 6,"##, r##""thickness": null,"##),
        &codes::E0104,
        &format!("{BAR}/thickness"),
    )
    .only());
    add(row(
        "null_for_a_string",
        r(r##""license": "CC0-1.0","##, r##""license": null,"##),
        &codes::E0104,
        "/license",
    )
    .only());
    add(row(
        "a_float_for_an_integer",
        r(r##""thickness": 6,"##, r##""thickness": 6.5,"##),
        &codes::E0104,
        &format!("{BAR}/thickness"),
    )
    .only());
    add(row(
        "a_bool_for_a_gap",
        r(r##""gap_before": "XS", "z": 1,"##, r##""gap_before": true, "z": 1,"##),
        &codes::E0104,
        "/layout/block/2/gap_before",
    )
    .only());
    add(row(
        "layout_as_a_string",
        r(r##""layout": {"##, r##""layout": "x", "ghost": {"##),
        &codes::E0104,
        "/layout",
    ));
    add(row("the_package_is_an_array", "[]".to_owned(), &codes::E0104, "").only());
    add(row("the_package_is_null", "null".to_owned(), &codes::E0104, "").only());
    add(row("the_package_is_a_string", "\"x\"".to_owned(), &codes::E0104, "").only());
    add(row(
        "a_palette_value_as_a_number",
        r(r##""gold": "#f2b535""##, r##""gold": 5"##),
        &codes::E0104,
        "/palette/gold",
    ));
    add(row(
        "an_asset_as_a_number",
        r(r##""<svg viewBox='0 0 24 24'><path d='M6 14h2v6H6zM11 8h2v12h-2zM16 11h2v9h-2z'/></svg>""##, "5"),
        &codes::E0104,
        "/assets/eq",
    ));
    add(row(
        "a_facts_name_as_a_number",
        r(r##""order": [ "duration", "year", "content_rating", "genres" ]"##, r##""order": [ "duration", 7 ]"##),
        &codes::E0104,
        &format!("/layout/block/6/cells/3/facts/order/1"),
    ));

    // ----- HUD-E0105: right type, outside its range, length or pattern (at the value) -----
    add(row("thickness_negative", r(r##""thickness": 6,"##, r##""thickness": -1,"##), &codes::E0105, &format!("{BAR}/thickness")).only());
    add(row("thickness_zero", r(r##""thickness": 6,"##, r##""thickness": 0,"##), &codes::E0105, &format!("{BAR}/thickness")).only());
    add(row("thickness_above_the_bound", r(r##""thickness": 6,"##, r##""thickness": 65,"##), &codes::E0105, &format!("{BAR}/thickness")).only());
    add(row(
        "id_not_reverse_dns_capitals",
        r(r##""id": "com.plxnative.examples.reference""##, r##""id": "Com.Bad""##),
        &codes::E0105,
        "/id",
    )
    .only());
    add(row(
        "id_not_reverse_dns_one_label",
        r(r##""id": "com.plxnative.examples.reference""##, r##""id": "nodots""##),
        &codes::E0105,
        "/id",
    )
    .only());
    add(row("version_not_semver_two_parts", r(r##""version": "1.0.0""##, r##""version": "1.0""##), &codes::E0105, "/version").only());
    add(row("version_not_semver_prerelease", r(r##""version": "1.0.0""##, r##""version": "1.0.0-beta""##), &codes::E0105, "/version").only());
    add(row("version_with_a_leading_zero", r(r##""version": "1.0.0""##, r##""version": "1.00.0""##), &codes::E0105, "/version").only());
    add(row("scrim_extra_above_the_classic_scrim", r(r##""extra": 120"##, r##""extra": 471"##), &codes::E0105, "/layout/scrim/extra").only());
    add(row("scrim_extra_negative", r(r##""extra": 120"##, r##""extra": -1"##), &codes::E0105, "/layout/scrim/extra").only());
    add(row("name_too_long", r(r##""name": "Reference""##, &format!(r##""name": "{}""##, "n".repeat(65))), &codes::E0105, "/name").only());
    add(row("name_with_a_control_character", r(r##""name": "Reference""##, r##""name": "A\u0007B""##), &codes::E0105, "/name").only());
    add(row("schema_url_not_https", r(r##""$schema": "https://plxnative.com/hud/schema/1.0.json""##, r##""$schema": "http://plxnative.com/x""##), &codes::E0105, "/$schema").only());
    add(row("gap_in_px_above_the_canvas", r(r##""gap_before": "XS", "z": 1,"##, r##""gap_before": 1081, "z": 1,"##), &codes::E0105, "/layout/block/2/gap_before").only());
    add(row("gap_in_px_negative", r(r##""gap_before": "XS", "z": 1,"##, r##""gap_before": -1, "z": 1,"##), &codes::E0105, "/layout/block/2/gap_before").only());
    add(row("z_above_the_bound", r(r##""z": 1,"##, r##""z": 17,"##), &codes::E0105, "/layout/block/2/z").only());
    add(row("card_width_zero", r(r##""card_w": 192"##, r##""card_w": 0"##), &codes::E0105, &format!("{STRIP}/card_w")).only());
    add(row("max_lines_above_the_bound", r(r##""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 1,"##, r##""size": "TITLE", "ink": "TEXT_PRIMARY", "bold": true, "max_lines": 9,"##), &codes::E0105, &format!("{TITLE_CELLS}/1/text/max_lines")).only());
    add(row("a_repeated_facts_name", r(r##""order": [ "duration", "year", "content_rating", "genres" ]"##, r##""order": [ "duration", "duration" ]"##), &codes::E0105, "/layout/block/6/cells/3/facts/order/1").only());
    add(row(
        "an_array_over_its_bound",
        edit(MINIMAL, r##""block": ["##, &format!(r##""block": [ {}"##, (0..15).map(|i| format!(r##"{{ "id": "b{i}", "cells": [] }}, "##)).collect::<String>())),
        &codes::E0105,
        "/layout/block",
    ));
    add(row(
        "a_color_with_no_palette_or_token_shape",
        r(r##""track": "RAIL_TRACK""##, r##""track": "#ffffff""##),
        &codes::E0105,
        &format!("{BAR}/track"),
    )
    .only());
    add(row("an_asset_id_with_capitals", r(r##""eq": "<svg"##, r##""EQ": "<svg"##), &codes::E0105, "/assets/EQ"));

    // ----- HUD-E0106: keys that may not combine, or one of a set required -----
    add(row(
        "cell_with_two_kinds",
        r(r##"{ "align": "end", "spacer": { "w": 150 } }"##, r##"{ "align": "end", "spacer": { "w": 150 }, "rule": { "axis": "h", "length": "fill", "ink": "HAIRLINE" } }"##),
        &codes::E0106,
        "/layout/block/6/cells/4",
    )
    .only());
    // The parser reports a cell with NO kind as E0106 (the schema doc words it as E0103).
    add(row(
        "cell_with_no_kind",
        r(r##"{ "align": "end", "spacer": { "w": 150 } }"##, r##"{ "align": "end" }"##),
        &codes::E0106,
        "/layout/block/6/cells/4",
    )
    .only());
    add(row(
        "rest_solid_with_focus_accent",
        r(r##""size": 64, "rest": "solid", "focus": "ring","##, r##""size": 64, "rest": "solid", "focus": "accent","##),
        &codes::E0106,
        &format!("{CENTER}/3"),
    )
    .only());
    add(row(
        "up_next_button_solid_with_accent",
        r(r##""play_next": { "rest": "solid", "focus": "ring" }"##, r##""play_next": { "rest": "solid", "focus": "accent" }"##),
        &codes::E0106,
        "/layout/up_next/play_next",
    )
    .only());
    add(row(
        "scrim_block_with_a_height",
        r(r##""scrim": { "cover": "block", "extra": 120 }"##, r##""scrim": { "cover": "block", "extra": 120, "height": 100 }"##),
        &codes::E0106,
        "/layout/scrim/height",
    )
    .only());
    add(row(
        "scrim_fixed_with_an_extra",
        r(r##""scrim": { "cover": "block", "extra": 120 }"##, r##""scrim": { "cover": "fixed", "height": 100, "extra": 5 }"##),
        &codes::E0106,
        "/layout/scrim/extra",
    )
    .only());
    add(row(
        "the_rating_in_order_and_in_badges",
        r(r##""badges": []"##, r##""badges": [ "content_rating" ]"##),
        &codes::E0106,
        "/layout/block/6/cells/3/facts",
    )
    .only());
    add(row(
        "hud_title_on_the_kicker",
        r(r##""w": "fill", "cells": [
          { "text": { "bind": "title.kicker", "size": "CAPTION""##, r##""w": "fill", "cells": [
          { "text": { "bind": "title.kicker", "size": "HUD_TITLE""##),
        &codes::E0106,
        &format!("{TITLE_CELLS}/0/text/size"),
    )
    .only());
    add(row(
        "app_text_with_max_lines",
        r(r##""bind": "label.chapters", "size": "LABEL", "ink": "TEXT_PRIMARY", "bold": false }"##, r##""bind": "label.chapters", "size": "LABEL", "ink": "TEXT_PRIMARY", "bold": false, "max_lines": 1 }"##),
        &codes::E0106,
        "/layout/block/3/cells/0/text/max_lines",
    )
    .only());
    add(row(
        "app_text_with_a_width",
        r(r##""bind": "item.start", "size": "MICRO", "ink": "TEXT_SECONDARY", "bold": false }"##, r##""bind": "item.start", "size": "MICRO", "ink": "TEXT_SECONDARY", "bold": false, "w": 80 }"##),
        &codes::E0106,
        &format!("{STRIP}/card/below/0/text/w"),
    )
    .only());

    // ----- HUD-E0201: a name outside a closed vocabulary (at the value, with the list nearby) -----
    add(row("widget_disk", r(r##""tab": "subtitles", "widget": "disc""##, r##""tab": "subtitles", "widget": "disk""##), &codes::E0201, &format!("{CENTER}/0/widget")).near("disc").only());
    add(row("align_middle", r(r##""align": "center", "segment": { "id": "center""##, r##""align": "middle", "segment": { "id": "center""##), &codes::E0201, "/layout/block/2/cells/0/align").near("center").only());
    add(row(
        "bind_title_kickr",
        r(r##""w": "fill", "cells": [
          { "text": { "bind": "title.kicker""##, r##""w": "fill", "cells": [
          { "text": { "bind": "title.kickr""##),
        &codes::E0201,
        &format!("{TITLE_CELLS}/0/text/bind"),
    )
    .near("title.kicker")
    .only());
    add(row("bind_info_summery", r(r##""bind": "info.summary""##, r##""bind": "info.summery""##), &codes::E0201, &format!("{INFO_CELLS}/2/text/bind")).near("info.summary").only());
    add(row("knob_sometimes", r(r##""knob": "always""##, r##""knob": "sometimes""##), &codes::E0201, &format!("{BAR}/knob")).near("always").only());
    add(row("size_rung_unknown", r(r##""size": "TITLE","##, r##""size": "GIANT","##), &codes::E0201, &format!("{TITLE_CELLS}/1/text/size")).only());
    add(row("size_rung_diagnostic_is_not_offered", r(r##""size": "TITLE","##, r##""size": "DIAGNOSTIC","##), &codes::E0201, &format!("{TITLE_CELLS}/1/text/size")).only());
    add(row("spacing_token_unknown", r(r##""id": "bar", "gap_before": "SM""##, r##""id": "bar", "gap_before": "XXL""##), &codes::E0201, "/layout/block/1/gap_before").near("XL").only());
    add(row("rest_unknown", r(r##""rest": "bare", "focus": "accent", "face": "subs-bubble""##, r##""rest": "bear", "focus": "accent", "face": "subs-bubble""##), &codes::E0201, &format!("{CENTER}/0/rest")).near("bare").only());
    add(row("countdown_unknown", r(r##""countdown": "ring""##, r##""countdown": "dial""##), &codes::E0201, "/layout/up_next/countdown").only());
    add(row("anchor_top_is_not_in_1_0", r(r##""anchor": "bottom""##, r##""anchor": "top""##), &codes::E0201, "/layout/anchor").near("bottom").only());
    add(row("crop_unknown", r(r##""h": 164, "radius": 8, "crop": "cover""##, r##""h": 164, "radius": 8, "crop": "contain""##), &codes::E0201, "/layout/block/6/cells/0/image/crop").near("cover").only());
    add(row("facts_name_unknown", r(r##""order": [ "duration", "year", "content_rating", "genres" ]"##, r##""order": [ "duration", "decade" ]"##), &codes::E0201, "/layout/block/6/cells/3/facts/order/1").only());
    add(row("card_binding_outside_a_card", r(r##"{ "text": { "bind": "info.summary""##, r##"{ "text": { "bind": "item.title""##), &codes::E0201, &format!("{INFO_CELLS}/2/text/bind")).only());
    add(row("a_binding_no_key_can_name", r(r##""bind": "info.summary""##, r##""bind": "playbar.pos""##), &codes::E0201, &format!("{INFO_CELLS}/2/text/bind")).only());

    // ----- HUD-E0202: a theme token outside the allow-list -----
    add(row("ink_token_typo", r(r##""track": "RAIL_TRACK""##, r##""track": "RAIL_TRACKK""##), &codes::E0202, &format!("{BAR}/track")).near("RAIL_TRACK").only());
    add(row("ink_token_pass_gold", r(r##""fill": "gold""##, r##""fill": "PASS_GOLD""##), &codes::E0202, &format!("{BAR}/fill")).only());
    add(row("ink_token_diagnostic", r(r##""track": "RAIL_TRACK""##, r##""track": "DIAG_RED""##), &codes::E0202, &format!("{BAR}/track")).only());

    // ----- HUD-E0203: an asset or built-in face nothing declares -----
    add(row("face_unknown_asset", r(r##""face": "subs-bubble""##, r##""face": "subs-bubbel""##), &codes::E0203, &format!("{CENTER}/0/face")).near("subs-bubble").only());
    add(row("face_unknown_builtin", r(r##""face": "builtin:next""##, r##""face": "builtin:nope""##), &codes::E0203, &format!("{CENTER}/4/face")).only());
    add(row("face_builtin_rewind_is_never_a_control", r(r##""face": "builtin:next""##, r##""face": "builtin:rewind""##), &codes::E0203, &format!("{CENTER}/4/face")).only());
    add(row("icon_unknown_asset", r(r##""asset": "eq""##, r##""asset": "eqq""##), &codes::E0203, &format!("{STRIP}/card/overlay/0/icon/asset")).near("eq").only());
    add(row("face_map_leaf_unknown_builtin", r(r##""restart": "builtin:restart""##, r##""restart": "builtin:restrat""##), &codes::E0203, &format!("{CENTER}/2/face/restart")).only());
    add(row("face_whose_asset_was_renamed", r(r##""gear": "<svg"##, r##""gearx": "<svg"##), &codes::E0203, &format!("{RIGHT}/2/face")).near("gearx").only());

    // ----- HUD-E0204: a lower-case ink no palette entry declares -----
    add(row("ink_names_a_missing_palette_entry", r(r##""fill": "gold""##, r##""fill": "silver""##), &codes::E0204, &format!("{BAR}/fill")).near("gold").only());
    add(row("palette_removed_but_still_used", r(r##""palette": { "gold": "#f2b535" },"##, ""), &codes::E0204, &format!("{BAR}/fill")).only());
    add(row("a_token_spelt_in_lower_case_is_a_palette_name", r(r##""track": "RAIL_TRACK""##, r##""track": "rail_track""##), &codes::E0204, &format!("{BAR}/track")).only());

    // ----- HUD-E0301: a command outside the ten (the other command codes are in `commands`) -----
    add(row("command_unknown", r(r##""command": "open_tracks""##, r##""command": "skip_back""##), &codes::E0301, &format!("{CENTER}/0/command")).only());
    add(row("command_in_the_wrong_case", r(r##""command": "open_tracks""##, r##""command": "Open_Tracks""##), &codes::E0301, &format!("{CENTER}/0/command")).near("open_tracks").only());

    // ----- HUD-E0401, 0402, 0403: the version gate answers alone, at /format -----
    add(format_row("format_1_1_is_a_newer_minor", r##""1.1""##, &codes::E0402).message(REASON_NEWER));
    add(format_row("format_1_9_is_a_newer_minor", r##""1.9""##, &codes::E0402).message(REASON_NEWER));
    add(format_row("format_1_10_is_a_newer_minor", r##""1.10""##, &codes::E0402).message(REASON_NEWER));
    add(format_row("format_2_0_is_a_newer_major", r##""2.0""##, &codes::E0403).message(REASON_NEWER));
    add(format_row("format_10_0_is_a_newer_major", r##""10.0""##, &codes::E0403).message(REASON_NEWER));
    add(format_row("format_0_9_is_an_older_major", r##""0.9""##, &codes::E0403).message(REASON_OLDER));
    add(format_row("format_0_0_is_an_older_major", r##""0.0""##, &codes::E0403).message(REASON_OLDER));
    add(format_row("format_one_part", r##""1""##, &codes::E0401));
    add(format_row("format_with_a_v", r##""v1.0""##, &codes::E0401));
    add(format_row("format_three_parts", r##""1.0.0""##, &codes::E0401));
    add(format_row("format_with_a_leading_zero", r##""01.0""##, &codes::E0401));
    add(format_row("format_with_a_double_zero_minor", r##""1.00""##, &codes::E0401));
    add(format_row("format_with_a_space", r##"" 1.0""##, &codes::E0401));
    add(format_row("format_empty", r#""""#, &codes::E0401));
    add(format_row("format_signed", r##""+1.0""##, &codes::E0401));
    // A number, null, a bool: parse.rs answers E0401 ("must be a string"), not E0104.
    add(format_row("format_as_a_number", "1.0", &codes::E0401));
    add(format_row("format_as_null", "null", &codes::E0401));
    add(format_row("format_as_a_bool", "true", &codes::E0401));
    add(row("format_missing", r(r##""format": "1.0","##, ""), &codes::E0401, "/format").only());
    add(row(
        "a_newer_package_is_judged_by_its_version_alone",
        edit(&r(r##""format": "1.0""##, r##""format": "1.1""##), r##""thickness": 6,"##, r##""thickness": "six","##),
        &codes::E0402,
        "/format",
    )
    .message(REASON_NEWER)
    .only());
    add(row(
        "an_older_package_is_judged_by_its_version_alone",
        edit(&r(r##""format": "1.0""##, r##""format": "0.9""##), r##""sections": "inline","##, r##""sections": "inline", "zz": 1,"##),
        &codes::E0403,
        "/format",
    )
    .message(REASON_OLDER)
    .only());

    // ----- HUD-E0501, 0502, 0503: conditions -----
    add(row("when_unknown_condition", r(r##"{ "id": "chapters_head", "when": "panel.chapters""##, r##"{ "id": "chapters_head", "when": "panel.chapterz""##), &codes::E0501, "/layout/block/3/when").near("panel.chapters").only());
    add(row("when_negated_unknown_condition", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": "!panel.infoo""##), &codes::E0501, "/layout/block/5/when").near("panel.info").only());
    add(row("when_array_with_an_unknown_condition", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": [ "panel.info", "nope" ]"##), &codes::E0501, "/layout/block/5/when/1").only());
    add(row("when_array_holding_a_number", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": [ "panel.info", 3 ]"##), &codes::E0502, "/layout/block/5/when/1").only());
    add(row("when_empty_array", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": []"##), &codes::E0502, "/layout/block/5/when").only());
    add(row("when_bang_alone", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": "!""##), &codes::E0502, "/layout/block/5/when").only());
    add(row("when_double_bang", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": "!!panel.info""##), &codes::E0502, "/layout/block/5/when").only());
    add(row("when_empty_string", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": """##), &codes::E0502, "/layout/block/5/when").only());
    add(row("when_a_number", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": 5"##), &codes::E0502, "/layout/block/5/when").only());
    add(row("when_an_object", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": { "panel.info": true }"##), &codes::E0502, "/layout/block/5/when").only());
    add(row("when_item_focused_on_a_band", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": "item.focused""##), &codes::E0503, "/layout/block/5/when").only());
    add(row("when_item_is_current_in_an_array_on_a_band", r(r##"{ "id": "info_rule", "when": "panel.info""##, r##"{ "id": "info_rule", "when": [ "panel.info", "!item.is_current" ]"##), &codes::E0503, "/layout/block/5/when/1").only());
    add(row("when_item_condition_misspelt_inside_a_card", r(r##""when": "item.is_current""##, r##""when": "item.is_curent""##), &codes::E0501, &format!("{STRIP}/card/overlay/0/when")).near("item.is_current").only());

    // ----- HUD-E0504: a state-keyed value that is not exactly {"value", "if"} or keys the unkeyable -----
    add(row("keyed_unknown_condition", r(r##""if": { "panel.info": "XS" }"##, r##""if": { "panel.infoo": "XS" }"##), &codes::E0501, "/layout/block/3/gap_before/if/panel.infoo").near("panel.info").only());
    add(row("keyed_card_condition_outside_a_card", r(r##""if": { "panel.info": "XS" }"##, r##""if": { "item.focused": "XS" }"##), &codes::E0503, "/layout/block/3/gap_before/if/item.focused").only());
    add(row("keyed_missing_if", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "SM" }"##), &codes::E0504, "/layout/block/3/gap_before").only());
    add(row("keyed_missing_value", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "if": { "panel.info": "XS" } }"##), &codes::E0504, "/layout/block/3/gap_before").only());
    add(row("keyed_with_an_extra_key", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "SM", "if": { "panel.info": "XS" }, "extra": 1 }"##), &codes::E0504, "/layout/block/3/gap_before").only());
    add(row("keyed_with_an_empty_if", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "SM", "if": {} }"##), &codes::E0504, "/layout/block/3/gap_before").only());
    add(row("keyed_if_is_not_an_object", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "SM", "if": "panel.info" }"##), &codes::E0504, "/layout/block/3/gap_before").only());
    add(row("keyed_nested", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "SM", "if": { "panel.info": { "value": "XS", "if": { "sections": "MD" } } } }"##), &codes::E0504, "/layout/block/3/gap_before/if/panel.info").only());
    add(row("keyed_value_is_itself_keyed", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": { "value": "SM", "if": { "sections": "MD" } }, "if": { "panel.info": "XS" } }"##), &codes::E0504, "/layout/block/3/gap_before/value").only());
    add(row("keyed_on_a_controls_size", r(r##""tab": "subtitles", "widget": "disc", "size": 52,"##, r##""tab": "subtitles", "widget": "disc", "size": { "value": 52, "if": { "panel.info": 40 } },"##), &codes::E0504, &format!("{CENTER}/0/size")).only());
    add(row("keyed_on_a_z_order", r(r##""z": 1,"##, r##""z": { "value": 1, "if": { "panel.info": 2 } },"##), &codes::E0504, "/layout/block/2/z").only());
    add(row("keyed_base_is_an_unknown_name", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "XXL", "if": { "panel.info": "XS" } }"##), &codes::E0201, "/layout/block/3/gap_before/value").only());
    add(row("keyed_case_is_an_unknown_name", r(r##"{ "value": "SM", "if": { "panel.info": "XS" } }"##, r##"{ "value": "SM", "if": { "panel.info": "XXL" } }"##), &codes::E0201, "/layout/block/3/gap_before/if/panel.info").only());

    // ----- HUD-E0505, 0506: the palette -----
    add(row("palette_short_hex", r(r##""gold": "#f2b535""##, r##""gold": "#f2b53""##), &codes::E0506, "/palette/gold").only());
    add(row("palette_hex_with_alpha", r(r##""gold": "#f2b535""##, r##""gold": "#f2b535ff""##), &codes::E0506, "/palette/gold").only());
    add(row("palette_a_colour_name", r(r##""gold": "#f2b535""##, r##""gold": "red""##), &codes::E0506, "/palette/gold").only());
    add(row("palette_upper_case_hex", r(r##""gold": "#f2b535""##, r##""gold": "#F2B535""##), &codes::E0506, "/palette/gold").only());
    add(row("palette_hex_without_the_hash", r(r##""gold": "#f2b535""##, r##""gold": "f2b535""##), &codes::E0506, "/palette/gold").only());
    add(row("palette_name_in_upper_case", r(r##""gold": "#f2b535""##, r##""Gold": "#f2b535""##), &codes::E0505, "/palette/Gold"));
    add(row("palette_name_with_a_dash", r(r##""gold": "#f2b535""##, r##""gold-2": "#f2b535""##), &codes::E0505, "/palette/gold-2"));
    add(row("palette_name_starting_with_a_digit", r(r##""gold": "#f2b535""##, r##""2gold": "#f2b535""##), &codes::E0505, "/palette/2gold"));

    // ----- HUD-E0701 to E0706: the inline assets -----
    const EQ_PATH: &str = "<path d='M6 14h2v6H6zM11 8h2v12h-2zM16 11h2v9h-2z'/>";
    add(row("asset_gradient", r(EQ_PATH, "<linearGradient id='g'><stop offset='0'/></linearGradient><path d='M6 14h2v6H6z'/>"), &codes::E0701, "/assets/eq"));
    add(row("asset_text_element", r(EQ_PATH, "<text>x</text><path d='M6 14h2v6H6z'/>"), &codes::E0701, "/assets/eq").only());
    add(row("asset_style_element", r(EQ_PATH, "<style>p{}</style><path d='M6 14h2v6H6z'/>"), &codes::E0701, "/assets/eq").only());
    add(row("asset_multi_colour", r(EQ_PATH, "<path d='M6 14h2v6H6z' fill='#ff0000'/>"), &codes::E0702, "/assets/eq").only());
    add(row("asset_stroke", r(EQ_PATH, "<path d='M6 14h2v6H6z' stroke='white'/>"), &codes::E0702, "/assets/eq").only());
    add(row("asset_entity", r(EQ_PATH, "<path d='M6 14h2v6H6z'/>&amp;"), &codes::E0703, "/assets/eq").only());
    add(row("asset_external_reference", r(EQ_PATH, "<path d='M6 14h2v6H6z' href='http://x.example/a.svg'/>"), &codes::E0703, "/assets/eq").only());
    add(row("asset_without_a_view_box", r(r##""eq": "<svg viewBox='0 0 24 24'>"##, r##""eq": "<svg>"##), &codes::E0704, "/assets/eq").only());
    add(row("asset_over_the_byte_bound", r(EQ_PATH, &format!("<path d='M0 0{}'/>", "h1z".repeat(svg_subset::MAX_ASSET_BYTES / 3 + 1))), &codes::E0705, "/assets/eq").only());
    add(row("asset_with_too_many_paths", r(EQ_PATH, &"<path d='M0 0h1v1z'/>".repeat(svg_subset::MAX_PATHS_PER_ASSET + 1)), &codes::E0706, "/assets/eq").only());

    // ----- HUD-E0100: not one JSON document (at "", and the only finding) -----
    let cut = REFERENCE.len() / 2;
    add(row("json_truncated_in_the_middle", REFERENCE[..cut].to_owned(), &codes::E0100, "").only());
    add(row("json_truncated_after_the_open_brace", "{".to_owned(), &codes::E0100, "").only());
    add(row("json_trailing_comma", r(r##""gold": "#f2b535" }"##, r##""gold": "#f2b535", }"##), &codes::E0100, "").only());
    add(row("json_trailing_junk", format!("{REFERENCE} x"), &codes::E0100, "").only());
    add(row("json_two_documents", format!("{REFERENCE}{REFERENCE}"), &codes::E0100, "").only());
    add(row("json_a_comment", r(r##""format": "1.0","##, r##""format": "1.0", // note"##), &codes::E0100, "").only());
    add(row("json_single_quotes", "{ 'format': '1.0' }".to_owned(), &codes::E0100, "").only());
    add(row("json_empty_input", String::new(), &codes::E0100, "").only());
    add(row("json_only_whitespace", " \n\t ".to_owned(), &codes::E0100, "").only());
    add(row("json_a_byte_order_mark", format!("\u{feff}{REFERENCE}"), &codes::E0100, "").only());
    add(row("json_a_number_out_of_range", r(r##""thickness": 6,"##, r##""thickness": 1e400,"##), &codes::E0100, "").only());
    let mut bad_utf8 = REFERENCE.as_bytes().to_vec();
    bad_utf8.insert(bad_utf8.len() / 2, 0xFF);
    let mut invalid = row("json_invalid_utf8", String::new(), &codes::E0100, "").only();
    invalid.bytes = bad_utf8;
    add(invalid);
    let mut lone_continuation = REFERENCE.as_bytes().to_vec();
    lone_continuation.splice(
        REFERENCE.find("\"Reference\"").map_or(0, |at| at + 1)..REFERENCE.find("\"Reference\"").map_or(0, |at| at + 1),
        [0x80u8],
    );
    let mut stray = row("json_a_stray_continuation_byte", String::new(), &codes::E0100, "").only();
    stray.bytes = lone_continuation;
    add(stray);
    let mut over = row("json_over_the_size_bound", String::new(), &codes::E0100, "").only();
    over.bytes = vec![b' '; parse::MAX_PACKAGE_BYTES + 1];
    add(over);
    add(row("json_nested_deeper_than_the_reader_allows", "[".repeat(5000), &codes::E0100, "").only());

    rows
}

mod invalid {
    use super::*;

    #[test]
    fn every_corpus_row_yields_its_code_at_its_pointer() {
        let rows = corpus();
        assert!(rows.len() > 150, "the corpus has {} rows", rows.len());
        let failures: Vec<String> = rows.iter().filter_map(|row| row.verify().err()).collect();
        assert!(
            failures.is_empty(),
            "{} of {} corpus rows failed:\n{}",
            failures.len(),
            rows.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn corpus_labels_are_unique() {
        let mut labels: Vec<&str> = corpus().iter().map(|r| r.label).collect();
        labels.sort_unstable();
        let before = labels.len();
        labels.dedup();
        assert_eq!(before, labels.len(), "a label names one row");
    }

    #[test]
    fn the_version_rows_carry_the_user_facing_reasons() {
        let by_label = |label: &str| {
            let rows = corpus();
            let row = rows.into_iter().find(|r| r.label == label).expect(label);
            parse_checked(&row.bytes).findings
        };
        let newer_minor = by_label("format_1_1_is_a_newer_minor");
        assert_eq!(newer_minor.len(), 1, "the version gate answers alone");
        assert_eq!(newer_minor[0].message, "Made for a newer PlxNative");
        assert_eq!(newer_minor[0].message, REASON_NEWER);
        assert_eq!(newer_minor[0].code, "HUD-E0402");
        assert_eq!(newer_minor[0].pointer, "/format");
        let newer_major = by_label("format_2_0_is_a_newer_major");
        assert_eq!(newer_major[0].message, "Made for a newer PlxNative");
        assert_eq!(newer_major[0].code, "HUD-E0403");
        let older = by_label("format_0_9_is_an_older_major");
        assert_eq!(older[0].message, "Uses an older HUD format");
        assert_eq!(older[0].message, REASON_OLDER);
        assert_eq!(older[0].code, "HUD-E0403");
        assert!(older.iter().all(|f| !f.hint.is_empty()), "the hint says what this app reads");
    }

    #[test]
    fn a_duplicate_key_reaches_the_duplicate_preserving_reader() {
        // `plx_ui::rec::strict_json` is the one reader that keeps duplicate keys; serde_json's own map
        // would silently keep the last. A finding at the second key's pointer proves it was reached.
        let found = findings_of(&reference(r##""name": "Reference","##, r##""name": "Reference", "name": "Again","##));
        let f = assert_finding(&found, &codes::E0101, "/name");
        assert_eq!(f.rule, "key-unique");
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn an_unknown_key_lists_its_nearest_names_and_no_more_than_the_bound() {
        let found = findings_of(&reference(r##""sections": "inline","##, r##""sections": "inline", "blok": [],"##));
        let f = assert_finding(&found, &codes::E0102, "/layout/blok");
        assert!(f.nearest.contains(&"block".to_owned()), "{:?}", f.nearest);
        assert!(f.nearest.len() <= MAX_NEAREST);
        assert!(f.hint.contains("block"), "the hint names the fix: {}", f.hint);
    }

    #[test]
    fn nearest_names_are_valid_names() {
        // Whatever a finding offers as a nearest name is a name the table has.
        let all_keys: Vec<&str> = schema::all_nodes()
            .iter()
            .flat_map(|n| n.fields.iter().map(|f| f.key))
            .chain(schema::COMMANDS.iter().flat_map(|c| c.params.iter().map(|p| p.key)))
            .collect();
        for row in corpus() {
            for f in parse_checked(&row.bytes).findings {
                if f.code == codes::E0102.id || f.code == codes::E0302.id {
                    for n in &f.nearest {
                        assert!(all_keys.contains(&n.as_str()), "row `{}`: `{n}` is no key", row.label);
                    }
                }
                if f.code == codes::E0501.id {
                    for n in &f.nearest {
                        assert!(schema::condition(n).is_some(), "row `{}`: `{n}` is no condition", row.label);
                    }
                }
                if f.code == codes::E0202.id {
                    for n in &f.nearest {
                        assert!(schema::ink_token(n).is_some(), "row `{}`: `{n}` is no token", row.label);
                    }
                }
                if f.code == codes::E0301.id {
                    for n in &f.nearest {
                        assert!(schema::command(n).is_some(), "row `{}`: `{n}` is no command", row.label);
                    }
                }
            }
        }
    }

    #[test]
    fn a_package_with_several_mistakes_reports_each_in_document_order() {
        let text = edit(
            &edit(
                &reference(r##""license": "CC0-1.0","##, r##""license": "CC0-1.0", "licence": 1,"##),
                r##""thickness": 6,"##,
                r##""thickness": "6","##,
            ),
            r##""anchor": "bottom""##,
            r##""anchor": "top""##,
        );
        let parsed = parse_checked(text.as_bytes());
        assert!(parsed.package.is_none());
        let at: Vec<&str> = parsed.findings.iter().map(|f| f.pointer.as_str()).collect();
        assert_eq!(at, ["/licence", "/layout/anchor", &format!("{BAR}/thickness")]);
    }

    #[test]
    fn severity_is_error_for_every_parse_finding() {
        for row in corpus() {
            for f in parse_checked(&row.bytes).findings {
                assert!(f.is_error(), "row `{}`: {f:?}", row.label);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Control items: one row per command and per parameter
// ---------------------------------------------------------------------------------------------------

mod commands {
    use super::*;

    const AT: &str = "/i";

    fn ctx() -> Ctx {
        Ctx {
            palette: vec!["gold".to_owned()],
            assets: vec!["glyph".to_owned()],
            format: schema::CURRENT,
            in_strip_card: false,
        }
    }

    fn item_node() -> &'static Node {
        schema::node("item").expect("the item node")
    }

    /// An item of the given `command` and keys (the rest of the item is valid).
    fn it(command_and_keys: &str) -> String {
        format!(r##"{{ "id": "x", "widget": "disc", "size": 52, {command_and_keys} }}"##)
    }

    fn judge(json: &str) -> Vec<Finding> {
        let (object, findings) = parse::validate_json(item_node(), json, AT, &ctx());
        assert_eq!(object.is_some(), !has_error(&findings), "{json}\n{}", dump(&findings));
        findings.iter().for_each(assert_lint_shaped);
        findings
    }

    /// What one row expects: the exact multiset of `(code, pointer)`.
    pub(super) struct Case {
        pub(super) label: &'static str,
        pub(super) json: String,
        pub(super) expect: Vec<(&'static Code, String)>,
    }

    fn case(label: &'static str, keys: &str, expect: &[(&'static Code, &str)]) -> Case {
        Case {
            label,
            json: it(keys),
            expect: expect
                .iter()
                .map(|(c, p)| (*c, format!("{AT}{p}")))
                .collect(),
        }
    }

    pub(super) fn cases() -> Vec<Case> {
        let mut c: Vec<Case> = Vec::new();
        let ok: &[(&'static Code, &str)] = &[];

        // ----- toggle_play -----
        c.push(case("toggle_play", r##""command": "toggle_play""##, ok));
        c.push(case("toggle_play_single_builtin_face", r##""command": "toggle_play", "face": "builtin:pause""##, ok));
        c.push(case("toggle_play_state_faces", r##""command": "toggle_play", "face": { "playing": "glyph", "paused": "builtin:play" }"##, ok));
        c.push(case(
            "toggle_play_wrong_states",
            r##""command": "toggle_play", "face": { "on": "glyph", "off": "glyph" }"##,
            &[(&codes::E0302, "/face/on"), (&codes::E0302, "/face/off"), (&codes::E0303, "/face"), (&codes::E0303, "/face")],
        ));
        c.push(case("toggle_play_one_state_only", r##""command": "toggle_play", "face": { "playing": "glyph" }"##, &[(&codes::E0303, "/face")]));
        c.push(case("toggle_play_extra_state", r##""command": "toggle_play", "face": { "playing": "glyph", "paused": "glyph", "stopped": "glyph" }"##, &[(&codes::E0302, "/face/stopped")]));
        c.push(case("toggle_play_state_face_not_a_string", r##""command": "toggle_play", "face": { "playing": 5, "paused": "glyph" }"##, &[(&codes::E0104, "/face/playing")]));
        c.push(case("toggle_play_state_face_unknown_asset", r##""command": "toggle_play", "face": { "playing": "nope", "paused": "glyph" }"##, &[(&codes::E0203, "/face/playing")]));
        c.push(case("toggle_play_with_a_seek_parameter", r##""command": "toggle_play", "by": 10"##, &[(&codes::E0302, "/by")]));

        // ----- seek: by, dir -----
        c.push(case("seek_back_ten", r##""command": "seek", "by": -10"##, ok));
        c.push(case("seek_forward_thirty", r##""command": "seek", "by": 30"##, ok));
        c.push(case("seek_the_largest_literal", r##""command": "seek", "by": 2147483647"##, ok));
        c.push(case("seek_the_smallest_literal", r##""command": "seek", "by": -2147483648"##, ok));
        c.push(case("seek_by_setting_back", r##""command": "seek", "by": "setting", "dir": "back""##, ok));
        c.push(case("seek_by_setting_forward", r##""command": "seek", "by": "setting", "dir": "forward""##, ok));
        c.push(case("seek_by_missing", r##""command": "seek""##, &[(&codes::E0303, "")]));
        c.push(case("seek_by_zero", r##""command": "seek", "by": 0"##, &[(&codes::E0304, "/by")]));
        c.push(case("seek_by_a_fraction", r##""command": "seek", "by": 1.5"##, &[(&codes::E0304, "/by")]));
        c.push(case("seek_by_a_word", r##""command": "seek", "by": "later""##, &[(&codes::E0304, "/by")]));
        c.push(case("seek_by_a_bool", r##""command": "seek", "by": true"##, &[(&codes::E0304, "/by")]));
        c.push(case("seek_by_null", r##""command": "seek", "by": null"##, &[(&codes::E0304, "/by")]));
        c.push(case("seek_by_above_the_range", r##""command": "seek", "by": 2147483648"##, &[(&codes::E0304, "/by")]));
        c.push(case("seek_by_setting_without_dir", r##""command": "seek", "by": "setting""##, &[(&codes::E0305, "")]));
        c.push(case("seek_literal_with_dir", r##""command": "seek", "by": 10, "dir": "back""##, &[(&codes::E0305, "/dir")]));
        c.push(case("seek_dir_without_by_is_missing_by", r##""command": "seek", "dir": "back""##, &[(&codes::E0303, "")]));
        c.push(case("seek_by_setting_dir_up", r##""command": "seek", "by": "setting", "dir": "up""##, &[(&codes::E0304, "/dir")]));
        c.push(case("seek_by_setting_dir_a_number", r##""command": "seek", "by": "setting", "dir": 5"##, &[(&codes::E0304, "/dir")]));
        c.push(case("seek_zero_with_dir_is_both_rules", r##""command": "seek", "by": 0, "dir": "back""##, &[(&codes::E0304, "/by"), (&codes::E0305, "/dir")]));
        c.push(case("seek_near_miss_parameter", r##""command": "seek", "bye": 10"##, &[(&codes::E0302, "/bye"), (&codes::E0303, "")]));
        c.push(case("seek_with_an_open_tracks_parameter", r##""command": "seek", "by": 10, "tab": "audio""##, &[(&codes::E0302, "/tab")]));
        // seek face maps need `by: "setting"`, and `default`.
        c.push(case(
            "seek_setting_face_ladder",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": { "10": "glyph", "30": "builtin:play" } }"##,
            ok,
        ));
        c.push(case("seek_setting_face_default_only", r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph" }"##, ok));
        c.push(case(
            "seek_setting_face_ladder_without_default",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "by_interval": { "10": "glyph", "30": "glyph" } }"##,
            &[(&codes::E0303, "/face")],
        ));
        c.push(case("seek_literal_with_a_face_map", r##""command": "seek", "by": 10, "face": { "default": "glyph" }"##, &[(&codes::E0305, "/face")]));
        c.push(case("seek_literal_with_a_single_face", r##""command": "seek", "by": 10, "face": "glyph""##, ok));
        c.push(case(
            "seek_ladder_rung_not_a_number",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": { "abc": "glyph" } }"##,
            &[(&codes::E0102, "/face/by_interval/abc")],
        ));
        c.push(case(
            "seek_ladder_rung_zero",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": { "0": "glyph" } }"##,
            &[(&codes::E0102, "/face/by_interval/0")],
        ));
        c.push(case(
            "seek_ladder_rung_with_a_leading_zero",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": { "010": "glyph" } }"##,
            &[(&codes::E0102, "/face/by_interval/010")],
        ));
        c.push(case(
            "seek_ladder_face_not_a_string",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": { "10": 3 } }"##,
            &[(&codes::E0104, "/face/by_interval/10")],
        ));
        c.push(case(
            "seek_ladder_is_not_an_object",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": "10" }"##,
            &[(&codes::E0104, "/face/by_interval")],
        ));
        c.push(case(
            "seek_ladder_repeated_rung",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "by_interval": { "10": "glyph", "10": "glyph" } }"##,
            &[(&codes::E0101, "/face/by_interval/10")],
        ));
        c.push(case(
            "seek_face_unknown_state",
            r##""command": "seek", "by": "setting", "dir": "back", "face": { "default": "glyph", "extra": "glyph" }"##,
            &[(&codes::E0302, "/face/extra")],
        ));

        // ----- previous, next -----
        c.push(case("previous", r##""command": "previous""##, ok));
        c.push(case("previous_state_faces", r##""command": "previous", "face": { "default": "builtin:previous", "restart": "builtin:restart" }"##, ok));
        c.push(case("previous_missing_a_state", r##""command": "previous", "face": { "default": "builtin:previous" }"##, &[(&codes::E0303, "/face")]));
        c.push(case("previous_with_a_seek_parameter", r##""command": "previous", "by": 10"##, &[(&codes::E0302, "/by")]));
        c.push(case("next", r##""command": "next""##, ok));
        c.push(case("next_single_face", r##""command": "next", "face": "builtin:next""##, ok));
        c.push(case("next_has_one_face_not_one_per_state", r##""command": "next", "face": { "default": "glyph" }"##, &[(&codes::E0304, "/face")]));
        c.push(case("next_with_an_open_tracks_parameter", r##""command": "next", "tab": "audio""##, &[(&codes::E0302, "/tab")]));

        // ----- open_tracks: tab -----
        c.push(case("open_tracks_subtitles", r##""command": "open_tracks", "tab": "subtitles""##, ok));
        c.push(case("open_tracks_audio", r##""command": "open_tracks", "tab": "audio""##, ok));
        c.push(case("open_tracks_tab_missing", r##""command": "open_tracks""##, &[(&codes::E0303, "")]));
        c.push(case("open_tracks_tab_video", r##""command": "open_tracks", "tab": "video""##, &[(&codes::E0304, "/tab")]));
        c.push(case("open_tracks_tab_a_number", r##""command": "open_tracks", "tab": 1"##, &[(&codes::E0304, "/tab")]));
        c.push(case("open_tracks_with_a_page", r##""command": "open_tracks", "tab": "audio", "page": "speed""##, &[(&codes::E0302, "/page")]));

        // ----- open_more: page -----
        c.push(case("open_more_page_omitted", r##""command": "open_more""##, ok));
        c.push(case("open_more_page_root", r##""command": "open_more", "page": "root""##, ok));
        c.push(case("open_more_page_quality", r##""command": "open_more", "page": "quality""##, ok));
        c.push(case("open_more_page_speed", r##""command": "open_more", "page": "speed""##, ok));
        c.push(case("open_more_page_volume", r##""command": "open_more", "page": "volume""##, &[(&codes::E0304, "/page")]));
        c.push(case("open_more_page_a_bool", r##""command": "open_more", "page": false"##, &[(&codes::E0304, "/page")]));
        c.push(case("open_more_with_a_section", r##""command": "open_more", "section": "info""##, &[(&codes::E0302, "/section")]));

        // ----- toggle_section: section -----
        c.push(case("toggle_section_chapters", r##""command": "toggle_section", "section": "chapters""##, ok));
        c.push(case("toggle_section_info", r##""command": "toggle_section", "section": "info""##, ok));
        c.push(case("toggle_section_missing", r##""command": "toggle_section""##, &[(&codes::E0303, "")]));
        c.push(case("toggle_section_recap", r##""command": "toggle_section", "section": "recap""##, &[(&codes::E0304, "/section")]));

        // ----- the parameterless commands -----
        c.push(case("from_beginning", r##""command": "from_beginning""##, ok));
        c.push(case("from_beginning_with_a_parameter", r##""command": "from_beginning", "section": "info""##, &[(&codes::E0302, "/section")]));
        c.push(case("go_to_detail", r##""command": "go_to_detail""##, ok));
        c.push(case("go_to_detail_with_a_parameter", r##""command": "go_to_detail", "page": "root""##, &[(&codes::E0302, "/page")]));
        c.push(case("toggle_stats", r##""command": "toggle_stats""##, ok));
        c.push(case("toggle_stats_state_faces", r##""command": "toggle_stats", "face": { "on": "glyph", "off": "builtin:more" }"##, ok));
        c.push(case(
            "toggle_stats_with_play_states",
            r##""command": "toggle_stats", "face": { "playing": "glyph", "paused": "glyph" }"##,
            &[(&codes::E0302, "/face/playing"), (&codes::E0302, "/face/paused"), (&codes::E0303, "/face"), (&codes::E0303, "/face")],
        ));
        c.push(case("toggle_stats_with_a_parameter", r##""command": "toggle_stats", "dir": "back""##, &[(&codes::E0302, "/dir")]));

        // ----- the command itself -----
        c.push(case("command_unknown_skip_back", r##""command": "skip_back""##, &[(&codes::E0301, "/command")]));
        c.push(case("command_missing", r##""by": 10"##, &[(&codes::E0103, "")]));
        c.push(case("command_a_number", r##""command": 7"##, &[(&codes::E0104, "/command")]));
        c.push(case("command_null", r##""command": null"##, &[(&codes::E0104, "/command")]));

        // ----- the item's own keys -----
        c.push(case("item_rest_solid_with_focus_ring", r##""command": "next", "rest": "solid", "focus": "ring""##, ok));
        c.push(case("item_rest_solid_with_focus_accent", r##""command": "next", "rest": "solid", "focus": "accent""##, &[(&codes::E0106, "")]));
        c.push(case("item_rest_solid_without_focus", r##""command": "next", "rest": "solid""##, &[(&codes::E0103, "")]));
        c.push(case("item_rest_film_with_focus_accent", r##""command": "next", "rest": "film", "focus": "accent""##, ok));
        // An item is command-dispatched: an unknown key is E0302 (a parameter no command has), not E0102.
        c.push(case("item_unknown_key", r##""command": "next", "colour": "red""##, &[(&codes::E0302, "/colour")]));
        c
    }

    #[test]
    fn every_command_row_gives_exactly_its_findings() {
        let all = cases();
        assert!(all.len() > 80, "{} rows", all.len());
        for case in &all {
            let found = judge(&case.json);
            let mut want: Vec<(String, String)> = case
                .expect
                .iter()
                .map(|(c, p)| (c.id.to_owned(), p.clone()))
                .collect();
            want.sort();
            let got = pairs(&found);
            assert_eq!(got, want, "row `{}`: {}\n{}", case.label, case.json, dump(&found));
        }
    }

    #[test]
    fn a_parameter_error_names_the_parameter_and_offers_the_valid_values() {
        let found = judge(&it(r##""command": "open_tracks", "tab": "video""##));
        let f = assert_finding(&found, &codes::E0304, "/i/tab");
        assert!(f.nearest.iter().any(|n| n == "subtitles" || n == "audio"), "{:?}", f.nearest);
        let found = judge(&it(r##""command": "seek", "by": "later""##));
        let f = assert_finding(&found, &codes::E0304, "/i/by");
        assert!(f.nearest.contains(&"setting".to_owned()), "{:?}", f.nearest);
        let found = judge(&it(r##""command": "toggle_section", "section": "recap""##));
        let f = assert_finding(&found, &codes::E0304, "/i/section");
        assert!(f.nearest.contains(&"info".to_owned()), "{:?}", f.nearest);
        let found = judge(&it(r##""command": "open_more", "page": "volume""##));
        let f = assert_finding(&found, &codes::E0304, "/i/page");
        assert!(f.nearest.len() >= 2, "{:?}", f.nearest);
    }

    #[test]
    fn an_unknown_command_lists_commands() {
        let found = judge(&it(r##""command": "skip_back""##));
        let f = assert_finding(&found, &codes::E0301, "/i/command");
        assert!(!f.nearest.is_empty());
        assert!(f.nearest.iter().all(|n| schema::command(n).is_some()), "{:?}", f.nearest);
        let found = judge(&it(r##""command": "seeks""##));
        let f = assert_finding(&found, &codes::E0301, "/i/command");
        assert_eq!(f.nearest[0], "seek");
    }

    #[test]
    fn an_unknown_parameter_is_e0302_for_every_command_and_lists_only_that_commands_keys() {
        for cmd in schema::COMMANDS {
            let json = valid_item(cmd, r##", "zz_param": 1"##);
            let found = judge(&json);
            assert_eq!(pairs(&found), [("HUD-E0302".to_owned(), "/i/zz_param".to_owned())], "{}: {}", cmd.name, dump(&found));
            let keys: Vec<&str> = item_node()
                .fields
                .iter()
                .map(|f| f.key)
                .chain(cmd.params.iter().map(|p| p.key))
                .collect();
            for n in &found[0].nearest {
                assert!(keys.contains(&n.as_str()), "{}: `{n}` is neither an item key nor a parameter of the command", cmd.name);
            }
        }
    }

    /// A valid item of `cmd` with its required parameters, then `tail` appended.
    fn valid_item(cmd: &Cmd, tail: &str) -> String {
        let params = match cmd.name {
            "seek" => r##", "by": 10"##,
            "open_tracks" => r##", "tab": "audio""##,
            "toggle_section" => r##", "section": "info""##,
            _ => "",
        };
        it(&format!(r##""command": "{}"{params}{tail}"##, cmd.name))
    }

    #[test]
    fn every_command_is_accepted_with_its_required_parameters() {
        for cmd in schema::COMMANDS {
            let found = judge(&valid_item(cmd, ""));
            assert!(found.is_empty(), "{}: {}", cmd.name, dump(&found));
        }
        assert_eq!(schema::COMMANDS.len(), 10, "the ten commands of 1.0");
    }

    #[test]
    fn a_command_rejects_every_other_commands_parameters() {
        for cmd in schema::COMMANDS {
            for other in schema::COMMANDS {
                for param in other.params {
                    if cmd.param(param.key).is_some() || schema::field(item_node(), param.key).is_some() {
                        continue;
                    }
                    let found = judge(&valid_item(cmd, &format!(r##", "{}": "audio""##, param.key)));
                    assert_eq!(
                        pairs(&found),
                        [("HUD-E0302".to_owned(), format!("/i/{}", param.key))],
                        "{} given {}'s parameter: {}",
                        cmd.name,
                        other.name,
                        dump(&found)
                    );
                }
            }
        }
    }

    #[test]
    fn the_command_parameters_are_exactly_the_five_of_the_spec() {
        let mut params: Vec<(&str, &str)> = schema::COMMANDS
            .iter()
            .flat_map(|c| c.params.iter().map(move |p| (c.name, p.key)))
            .collect();
        params.sort_unstable();
        assert_eq!(
            params,
            [
                ("open_more", "page"),
                ("open_tracks", "tab"),
                ("seek", "by"),
                ("seek", "dir"),
                ("toggle_section", "section"),
            ]
        );
    }

    #[test]
    fn items_of_every_command_parse_inside_a_whole_package() {
        // The same item text, inside the minimal package, through `parse` (not `validate_json`).
        for cmd in schema::COMMANDS {
            let item = valid_item(cmd, "");
            let item = item.replace(r##""id": "x""##, r##""id": "more""##);
            let text = edit(
                MINIMAL,
                r##"{ "id": "more", "command": "open_more", "widget": "disc", "size": 52 }"##,
                &item,
            );
            let parsed = parse_checked(text.as_bytes());
            assert!(parsed.findings.is_empty(), "{}: {}", cmd.name, dump(&parsed.findings));
        }
    }

    #[test]
    fn a_whole_package_reports_a_command_error_at_the_full_pointer() {
        let text = edit(
            MINIMAL,
            r##""command": "open_more", "widget": "disc""##,
            r##""command": "open_more", "page": "volume", "widget": "disc""##,
        );
        let found = findings_of(&text);
        assert_eq!(
            pairs(&found),
            [("HUD-E0304".to_owned(), "/layout/block/1/cells/0/segment/items/0/page".to_owned())]
        );
    }
}

// ---------------------------------------------------------------------------------------------------
// Fragments of nodes the reference does not use
// ---------------------------------------------------------------------------------------------------

mod fragments {
    use super::*;

    fn ctx() -> Ctx {
        Ctx {
            palette: vec!["gold".to_owned()],
            assets: vec!["glyph".to_owned()],
            format: schema::CURRENT,
            in_strip_card: false,
        }
    }

    fn judge(node: &str, json: &str) -> Vec<Finding> {
        let node = schema::node(node).expect("a node of the table");
        let (object, findings) = parse::validate_json(node, json, "/n", &ctx());
        assert_eq!(object.is_some(), !has_error(&findings), "{json}\n{}", dump(&findings));
        findings.iter().for_each(assert_lint_shaped);
        findings
    }

    fn expect(node: &str, json: &str, want: &[(&Code, &str)]) {
        let mut want: Vec<(String, String)> = want
            .iter()
            .map(|(c, p)| (c.id.to_owned(), format!("/n{p}")))
            .collect();
        want.sort();
        let found = judge(node, json);
        assert_eq!(pairs(&found), want, "{node}: {json}\n{}", dump(&found));
    }

    #[test]
    fn rect_pad_and_bleed() {
        let cells = r##""cells": []"##;
        expect("rect", &format!(r##"{{ "ink": "gold", {cells} }}"##), &[]);
        expect("rect", &format!(r##"{{ "ink": "gold", "pad": 40, "bleed": 40, {cells} }}"##), &[]);
        expect("rect", &format!(r##"{{ "ink": "gold", "pad": 40, "bleed": 0, {cells} }}"##), &[]);
        // `bleed` needs `pad` (E0106 at the object) and is at most `pad` (E0105 at the value).
        expect("rect", &format!(r##"{{ "ink": "gold", "bleed": 10, {cells} }}"##), &[(&codes::E0106, "")]);
        expect("rect", &format!(r##"{{ "ink": "gold", "pad": 10, "bleed": 11, {cells} }}"##), &[(&codes::E0105, "/bleed")]);
        expect("rect", &format!(r##"{{ "ink": "gold", "pad": 541, {cells} }}"##), &[(&codes::E0105, "/pad")]);
        expect("rect", &format!(r##"{{ "pad": 4, {cells} }}"##), &[(&codes::E0103, "")]);
    }

    #[test]
    fn scrim_forms() {
        expect("scrim", r##"{ "cover": "block", "extra": 470 }"##, &[]);
        expect("scrim", r##"{ "cover": "fixed", "height": 0 }"##, &[]);
        expect("scrim", r##"{ "cover": "fixed" }"##, &[(&codes::E0103, "")]);
        expect("scrim", r##"{ "cover": "fixed", "height": 471 }"##, &[(&codes::E0105, "/height")]);
        expect("scrim", r##"{ "cover": "behind", "height": 4 }"##, &[(&codes::E0201, "/cover")]);
        expect("scrim", r##"{ "extra": 4 }"##, &[(&codes::E0103, "")]);
        expect("scrim", r##"{ "cover": "block", "extra": { "value": 100, "if": { "panel.info": 300 } } }"##, &[]);
    }

    #[test]
    fn actions_and_grab() {
        let item = r##"{ "id": "x", "command": "from_beginning", "widget": "button", "size": 60 }"##;
        expect("actions", &format!(r##"{{ "item_h": 60, "items": [ {item} ] }}"##), &[]);
        expect("actions", &format!(r##"{{ "axis": "column", "item_h": 60, "min_w": 200, "items": [ {item} ] }}"##), &[]);
        expect("actions", &format!(r##"{{ "axis": "diagonal", "item_h": 60, "items": [ {item} ] }}"##), &[(&codes::E0201, "/axis")]);
        expect("actions", &format!(r##"{{ "items": [ {item} ] }}"##), &[(&codes::E0103, "")]);
        expect("actions", r##"{ "item_h": 60, "items": [ { "id": "x", "command": "skip_back", "widget": "button", "size": 60 } ] }"##, &[(&codes::E0301, "/items/0/command")]);
        expect("grab", r##"{ "above": 72, "below": 80 }"##, &[]);
        expect("grab", r##"{ "above": -1 }"##, &[(&codes::E0105, "/above")]);
        expect("grab", r##"{ "below": 541 }"##, &[(&codes::E0105, "/below")]);
        expect("grab", r##"{ "sideways": 1 }"##, &[(&codes::E0102, "/sideways")]);
    }

    #[test]
    fn up_next_buttons() {
        expect("up_next_button", "{}", &[]);
        expect("up_next_button", r##"{ "rest": "film", "focus": "accent" }"##, &[]);
        expect("up_next_button", r##"{ "rest": "solid", "focus": "accent" }"##, &[(&codes::E0106, "")]);
        expect("up_next_button", r##"{ "rest": "solid" }"##, &[(&codes::E0103, "")]);
        expect("up_next_button", r##"{ "rest": "bare", "focus": "glow" }"##, &[(&codes::E0201, "/focus")]);
        expect("up_next_button", r##"{ "tab": "audio" }"##, &[(&codes::E0102, "/tab")]);
    }

    #[test]
    fn a_strip_card_enters_card_scope_at_the_card() {
        // The card node allows `item.*` below it; the same text outside a card does not.
        let card = r##"{ "art": { "bind": "item.thumb" }, "below": [ { "text": { "bind": "item.title", "size": "CAPTION", "ink": "TEXT_PRIMARY" }, "when": "item.focused" } ] }"##;
        expect("card", card, &[]);
        let text = r##"{ "bind": "item.title", "size": "CAPTION", "ink": "TEXT_PRIMARY" }"##;
        expect("text", text, &[(&codes::E0201, "/bind")]);
        let mut in_card = ctx();
        in_card.in_strip_card = true;
        let (_, found) = parse::validate_json(schema::node("text").expect("text"), text, "/n", &in_card);
        assert!(found.is_empty(), "{}", dump(&found));
        // The same condition outside a card is E0503.
        expect("band", r##"{ "id": "a", "when": "item.focused", "cells": [] }"##, &[(&codes::E0503, "/when")]);
    }

    #[test]
    fn a_fragment_that_is_not_an_object_or_not_json() {
        expect("text", "5", &[(&codes::E0104, "")]);
        expect("text", "[]", &[(&codes::E0104, "")]);
        expect("text", "{", &[(&codes::E0100, "")]);
        expect("text", "", &[(&codes::E0100, "")]);
    }

    #[test]
    fn a_fragment_reports_pointers_under_the_base_it_was_given() {
        let node = schema::node("text").expect("text");
        let (object, found) = parse::validate_json(node, r##"{ "bind": "nope" }"##, "/a/b~1c", &ctx());
        assert!(object.is_none());
        assert_finding(&found, &codes::E0201, "/a/b~1c/bind");
        assert_finding(&found, &codes::E0103, "/a/b~1c");
    }

    #[test]
    fn pointer_tokens_are_escaped() {
        // A key with a slash and a tilde: the pointer carries `~1` and `~0`.
        let found = findings_of(&reference(r##""license": "CC0-1.0","##, r##""license": "CC0-1.0", "a/b~c": 1,"##));
        assert_finding(&found, &codes::E0102, "/a~1b~0c");
    }
}

// ---------------------------------------------------------------------------------------------------
// Every code has a row
// ---------------------------------------------------------------------------------------------------

mod coverage {
    use super::*;

    #[test]
    fn the_corpus_covers_every_parse_rule() {
        let covered: std::collections::BTreeSet<&str> = corpus().iter().map(|r| r.code.id).collect();
        // The command codes are covered by `commands`, which asserts them one by one.
        let command_codes = ["HUD-E0302", "HUD-E0303", "HUD-E0304", "HUD-E0305"];
        for code in codes::ALL {
            // E0107 (a key newer than the declared format) cannot be written in a package that parses:
            // 1.0 is the first minor. `parse::tests` reaches it with a hand-built node.
            // The E08xx/E09xx/W09xx codes are `check`'s, not the parser's: `reach_tests` has their rows.
            // The E06xx/W06xx and E10xx-E13xx codes are the lint's (they need a compiled package, a
            // language and a state): `lint_tests` holds their rows, one invalid case per code.
            let check_code = ["HUD-E08", "HUD-E09", "HUD-W09"].iter().any(|p| code.id.starts_with(p));
            let lint_code = ["HUD-E06", "HUD-W06", "HUD-E10", "HUD-E11", "HUD-E12", "HUD-E13"]
                .iter()
                .any(|p| code.id.starts_with(p));
            if code.id == "HUD-E0107" || check_code || lint_code || command_codes.contains(&code.id) {
                continue;
            }
            assert!(covered.contains(code.id), "{} has no corpus row", code.id);
        }
    }

    #[test]
    fn the_command_cases_cover_the_command_codes() {
        let mut seen = std::collections::BTreeSet::new();
        for case in commands::cases() {
            for (code, _) in &case.expect {
                seen.insert(code.id);
            }
        }
        for id in ["HUD-E0301", "HUD-E0302", "HUD-E0303", "HUD-E0304", "HUD-E0305"] {
            assert!(seen.contains(id), "{id} has no command row");
        }
    }

    #[test]
    fn every_parse_rule_named_by_the_task_has_a_row() {
        let labels: Vec<&str> = corpus().iter().map(|r| r.label).collect();
        for needle in [
            "unknown_key", "duplicate", "missing", "wrong", "thickness_as_a_string", "format_1_1", "format_2_0",
            "format_0_9", "when_", "keyed_", "palette_", "ink_token", "asset_", "json_",
        ] {
            assert!(labels.iter().any(|l| l.contains(needle)), "no row for `{needle}`");
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// Seeded mutations: whatever the bytes, the parser answers in the lint shape and never panics
// ---------------------------------------------------------------------------------------------------

mod mutation {
    use super::*;
    use std::collections::BTreeSet;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    /// xorshift64*: a fixed seed gives the same sequence on every host, and nothing else is read.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Rng {
            // The state must never be zero; the scramble keeps neighbouring seeds apart.
            let scrambled = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
            Rng(scrambled.max(1))
        }
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        /// A number below `n` (a zero `n` counts as one).
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n.max(1) as u64) as usize
        }
        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[self.below(items.len())]
        }
    }

    /// Bytes that change the shape of a JSON document when they land in the wrong place.
    const DELIMS: &[u8] = b"{}[],:\"\\ \n-.0e19tfnul";
    /// What a value is swapped for: every JSON kind, and the awkward numbers and strings.
    const SWAPS: &[&str] = &[
        "null",
        "true",
        "false",
        "0",
        "-1",
        "1.5",
        "1e999",
        "99999999999999999999",
        "\"\"",
        "\"x\"",
        "\"#ffffff\"",
        "\"\\u0001\"",
        "[]",
        "[1]",
        "{}",
        "{\"value\": 1, \"if\": {}}",
        "{\"value\": 1, \"if\": {\"panel.info\": 2}}",
    ];

    fn replace_range(src: &mut Vec<u8>, from: usize, to: usize, with: &[u8]) {
        let tail = src.split_off(to);
        src.truncate(from);
        src.extend_from_slice(with);
        src.extend(tail);
    }

    fn insert_at(src: &mut Vec<u8>, at: usize, with: &[u8]) {
        replace_range(src, at, at, with);
    }

    fn mutate_bytes(rng: &mut Rng, src: &mut Vec<u8>) {
        if src.is_empty() {
            src.push(*rng.pick(DELIMS));
            return;
        }
        let at = rng.below(src.len());
        let end = (at + 1 + rng.below(16)).min(src.len());
        match rng.below(9) {
            0 => src[at] = rng.next_u64() as u8,
            1 => src[at] = *rng.pick(DELIMS),
            2 => {
                src.remove(at);
            }
            3 => insert_at(src, at, &[*rng.pick(DELIMS)]),
            4 => insert_at(src, at, &[rng.next_u64() as u8]),
            5 => src.truncate(at),
            6 => {
                let other = rng.below(src.len());
                src.swap(at, other);
            }
            7 => {
                let run = src[at..end].to_vec();
                insert_at(src, at, &run);
            }
            _ => replace_range(src, at, end, &[]),
        }
    }

    /// The `(start, end)` of the name inside every `"name":` of `src`.
    fn key_spans(src: &[u8]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < src.len() {
            if src[i] == b'"' {
                let start = i + 1;
                let mut j = start;
                while j < src.len()
                    && (src[j].is_ascii_alphanumeric() || matches!(src[j], b'_' | b'.' | b'$' | b'-'))
                {
                    j += 1;
                }
                if j > start && j < src.len() && src[j] == b'"' {
                    let mut k = j + 1;
                    while k < src.len() && src[k] == b' ' {
                        k += 1;
                    }
                    if k < src.len() && src[k] == b':' {
                        out.push((start, j));
                    }
                    i = j + 1;
                    continue;
                }
            }
            i += 1;
        }
        out
    }

    /// The end of the JSON value that starts at `at`, or `None` when it does not end cleanly.
    fn value_end(src: &[u8], at: usize) -> Option<usize> {
        let first = *src.get(at)?;
        match first {
            b'"' => {
                let mut i = at + 1;
                while i < src.len() {
                    match src[i] {
                        b'\\' => i += 2,
                        b'"' => return Some(i + 1),
                        _ => i += 1,
                    }
                }
                None
            }
            b'-' | b'0'..=b'9' => {
                let mut i = at;
                while i < src.len() && matches!(src[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    i += 1;
                }
                Some(i)
            }
            b't' | b'f' | b'n' => {
                let mut i = at;
                while i < src.len() && src[i].is_ascii_lowercase() {
                    i += 1;
                }
                Some(i)
            }
            b'[' | b'{' => {
                let (mut depth, mut i, mut in_string) = (0usize, at, false);
                while i < src.len() {
                    let b = src[i];
                    if in_string {
                        match b {
                            b'\\' => i += 1,
                            b'"' => in_string = false,
                            _ => {}
                        }
                    } else {
                        match b {
                            b'"' => in_string = true,
                            b'[' | b'{' => depth += 1,
                            b']' | b'}' => {
                                depth -= 1;
                                if depth == 0 {
                                    return Some(i + 1);
                                }
                            }
                            _ => {}
                        }
                    }
                    i += 1;
                }
                None
            }
            _ => None,
        }
    }

    fn mutate_key(rng: &mut Rng, src: &mut Vec<u8>, pool: &[&str]) {
        let spans = key_spans(src);
        if spans.is_empty() {
            return mutate_bytes(rng, src);
        }
        let (start, end) = *rng.pick(&spans);
        let name = src[start..end].to_vec();
        match rng.below(8) {
            0 => {
                let with = *rng.pick(pool);
                replace_range(src, start, end, with.as_bytes());
            }
            1 => {
                // A near miss: one letter fewer.
                let mut near = name;
                near.remove(rng.below(near.len()));
                replace_range(src, start, end, &near);
            }
            2 => {
                let mut shouted = name;
                shouted[0] = shouted[0].to_ascii_uppercase();
                replace_range(src, start, end, &shouted);
            }
            3 => {
                // The same key twice in one object.
                let mut twin = vec![b'"'];
                twin.extend_from_slice(&name);
                twin.extend_from_slice(b"\": 0, ");
                insert_at(src, start - 1, &twin);
            }
            4 => replace_range(src, start, end, b""),
            5 => replace_range(src, start, end, &[b'k'; 300]),
            6 => replace_range(src, start, end, b"a/b~c"),
            _ => {
                // The value after the key becomes another kind of value.
                let mut at = end + 1;
                while at < src.len() && matches!(src[at], b' ' | b':' | b'\n') {
                    at += 1;
                }
                if let Some(stop) = value_end(src, at) {
                    let with = *rng.pick(SWAPS);
                    replace_range(src, at, stop, with.as_bytes());
                }
            }
        }
    }

    /// Every key name the table knows: the pool a renamed key is drawn from.
    fn key_pool() -> Vec<&'static str> {
        let mut pool: Vec<&'static str> = Vec::new();
        for node in schema::all_nodes() {
            pool.extend(node.fields.iter().map(|f| f.key));
        }
        for cmd in schema::COMMANDS {
            pool.extend(cmd.params.iter().map(|p| p.key));
            pool.push(cmd.name);
        }
        pool.extend(schema::CONDITIONS.iter().map(|c| c.name));
        pool.sort_unstable();
        pool.dedup();
        pool
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Mode {
        Bytes,
        Keys,
        Both,
    }

    /// The mutated copy number `index` of `src` under `seed`: one to three mutations.
    fn mutant(seed: u64, index: usize, src: &str, mode: Mode, pool: &[&str]) -> Vec<u8> {
        let mut rng = Rng::new(seed.wrapping_mul(1_000_003).wrapping_add(index as u64));
        let mut bytes = src.as_bytes().to_vec();
        for _ in 0..1 + rng.below(3) {
            match mode {
                Mode::Bytes => mutate_bytes(&mut rng, &mut bytes),
                Mode::Keys => mutate_key(&mut rng, &mut bytes, pool),
                Mode::Both => {
                    if rng.below(2) == 0 {
                        mutate_bytes(&mut rng, &mut bytes);
                    } else {
                        mutate_key(&mut rng, &mut bytes, pool);
                    }
                }
            }
        }
        bytes
    }

    /// What a run saw, so a test can prove its mutations reached more than one outcome.
    #[derive(Default)]
    struct Tally {
        accepted: usize,
        rejected: usize,
        codes: BTreeSet<&'static str>,
    }

    fn show(input: &[u8]) -> String {
        let text = String::from_utf8_lossy(input);
        let clipped: String = text.chars().take(400).collect();
        format!("{clipped:?} ({} bytes)", input.len())
    }

    fn lint_shaped(label: &str, input: &[u8], findings: &[Finding]) {
        for f in findings {
            let shaped = std::panic::catch_unwind(AssertUnwindSafe(|| assert_lint_shaped(f)));
            assert!(shaped.is_ok(), "{label}: a finding is not lint-shaped: {f:?}\ninput: {}", show(input));
        }
    }

    /// Everything that must hold for ANY input.
    fn check(label: &str, input: &[u8], tally: &mut Tally) {
        let parsed = catch_unwind(AssertUnwindSafe(|| parse::parse(input)))
            .unwrap_or_else(|_| panic!("{label}: parse panicked on {}", show(input)));
        assert_eq!(
            parsed.package.is_some(),
            !has_error(&parsed.findings),
            "{label}: a package exists exactly when no finding is an error\n{}\ninput: {}",
            dump(&parsed.findings),
            show(input)
        );
        lint_shaped(label, input, &parsed.findings);
        match &parsed.package {
            Some(package) => {
                tally.accepted += 1;
                assert_eq!(package.layout.pointer, "/layout", "{label}: input: {}", show(input));
            }
            None => {
                tally.rejected += 1;
                assert!(!parsed.findings.is_empty(), "{label}: no package and no finding: {}", show(input));
            }
        }
        tally.codes.extend(parsed.findings.iter().map(|f| f.code));
        assert_eq!(parsed, parse::parse(input), "{label}: parsing is pure");

        // The second door: the same walker over the root node, text in, no version gate.
        let Ok(text) = std::str::from_utf8(input) else {
            return;
        };
        let (object, findings) =
            catch_unwind(AssertUnwindSafe(|| parse::validate_json(&schema::ROOT, text, "", &Ctx::v1())))
                .unwrap_or_else(|_| panic!("{label}: validate_json panicked on {}", show(input)));
        assert_eq!(
            object.is_some(),
            !has_error(&findings),
            "{label}: an object exists exactly when no finding is an error\n{}\ninput: {}",
            dump(&findings),
            show(input)
        );
        lint_shaped(label, input, &findings);
        let gated = parsed
            .findings
            .iter()
            .any(|f| matches!(f.code, "HUD-E0401" | "HUD-E0402" | "HUD-E0403"));
        if !gated {
            assert_eq!(
                pairs(&parsed.findings),
                pairs(&findings),
                "{label}: the two doors run one walker\ninput: {}",
                show(input)
            );
        }
    }

    fn run(seeds: std::ops::RangeInclusive<u64>, per_seed: usize, src: &str, mode: Mode) -> Tally {
        let pool = key_pool();
        let mut tally = Tally::default();
        for seed in seeds {
            for index in 0..per_seed {
                let input = mutant(seed, index, src, mode, &pool);
                check(&format!("{mode:?} seed {seed} #{index}"), &input, &mut tally);
            }
        }
        tally
    }

    #[test]
    fn byte_mutations_of_the_reference_never_panic() {
        let tally = run(1..=3, 350, REFERENCE, Mode::Bytes);
        assert!(tally.rejected > 100, "the mutations reach the parser's refusals: {}", tally.rejected);
        assert!(tally.codes.len() >= 4, "and more than one kind of them: {:?}", tally.codes);
    }

    #[test]
    fn key_mutations_of_the_reference_never_panic() {
        let tally = run(1..=3, 350, REFERENCE, Mode::Keys);
        assert!(tally.rejected > 400, "{}", tally.rejected);
        for code in ["HUD-E0101", "HUD-E0102", "HUD-E0103", "HUD-E0104"] {
            assert!(tally.codes.contains(code), "no key mutation reached {code}: {:?}", tally.codes);
        }
        assert!(tally.codes.len() >= 8, "{:?}", tally.codes);
    }

    #[test]
    fn mixed_mutations_of_both_fixtures_never_panic() {
        let reference = run(10..=12, 250, REFERENCE, Mode::Both);
        let minimal = run(20..=22, 250, MINIMAL, Mode::Both);
        assert!(reference.rejected > 100 && minimal.rejected > 100);
        let mut codes = reference.codes;
        codes.extend(minimal.codes);
        assert!(codes.len() >= 8, "{codes:?}");
    }

    #[test]
    fn some_mutations_stay_valid_and_the_rest_do_not() {
        // A package is not a brittle thing: changing the text of a name leaves it valid.
        let mut tally = Tally::default();
        let renamed = REFERENCE.replace(r##""name": "Reference""##, r##""name": "Renamed""##);
        check("renamed", renamed.as_bytes(), &mut tally);
        check("reference", REFERENCE.as_bytes(), &mut tally);
        assert_eq!((tally.accepted, tally.rejected), (2, 0));
    }

    #[test]
    fn the_generator_is_deterministic_and_seeds_differ() {
        let pool = key_pool();
        for mode in [Mode::Bytes, Mode::Keys, Mode::Both] {
            for index in 0..20 {
                assert_eq!(
                    mutant(7, index, REFERENCE, mode, &pool),
                    mutant(7, index, REFERENCE, mode, &pool),
                    "{mode:?} #{index}"
                );
            }
        }
        let a: Vec<Vec<u8>> = (0..20).map(|i| mutant(7, i, REFERENCE, Mode::Both, &pool)).collect();
        let b: Vec<Vec<u8>> = (0..20).map(|i| mutant(8, i, REFERENCE, Mode::Both, &pool)).collect();
        assert_ne!(a, b);
        // The first outputs of the generator are pinned, so a change of the algorithm is noticed.
        let mut rng = Rng::new(1);
        let first: Vec<u64> = (0..3).map(|_| rng.next_u64()).collect();
        let mut again = Rng::new(1);
        assert_eq!(first, (0..3).map(|_| again.next_u64()).collect::<Vec<_>>());
        assert!(first.iter().all(|&n| n != 0));
    }

    #[test]
    fn the_pointer_checker_checks() {
        for good in ["", "/", "/a", "/a/0", "/a~0b", "/a~1b", "/~0~1", "//"] {
            assert!(well_formed_pointer(good), "{good:?}");
        }
        for bad in ["a", "a/b", "/a~", "/a~2", "/a~b", "~0"] {
            assert!(!well_formed_pointer(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_key_scanner_finds_keys_and_not_values() {
        let src = br#"{ "id": "x", "a.b": 1, "list": ["not_a_key", { "c-d": true }] }"#;
        let spans = key_spans(src);
        let names: Vec<&str> = spans
            .iter()
            .map(|&(s, e)| std::str::from_utf8(&src[s..e]).expect("ascii"))
            .collect();
        assert_eq!(names, ["id", "a.b", "list", "c-d"]);
        let at = src.iter().position(|&b| b == b'[').expect("a list");
        let stop = value_end(src, at).expect("it ends");
        assert_eq!(&src[at..stop], br#"["not_a_key", { "c-d": true }]"#);
    }

    #[test]
    fn the_reader_keeps_duplicate_keys_in_document_order() {
        // Only the duplicate-preserving reader lets the parser report E0101 at the second key.
        let Ok(Ordered::Object(pairs)) = decode_ordered(br#"{"a": 1, "b": 2, "a": 3}"#, 1 << 20) else {
            panic!("an object");
        };
        let keys: Vec<&str> = pairs.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["a", "b", "a"]);
    }
}

// ---------------------------------------------------------------------------------------------------
// The drift test: the descriptor table against the parser that reads it
// ---------------------------------------------------------------------------------------------------

/// Walks `schema` itself: for every key of every node, and every parameter of every command, it builds
/// the values the table lists and proves the parser accepts each; it builds the neighbours the table
/// does not list and proves the parser refuses each with the frozen code at the frozen pointer. The
/// sampler matches EXHAUSTIVELY on [`Ty`], [`Pattern`], [`NodeKind`] and [`Rule`], so a new variant
/// of any of them stops this file compiling until a sample and a refusal are written for it.
mod drift {
    use super::*;
    use std::collections::BTreeSet;

    const AT: &str = "/n";
    const SVG: &str = "<svg viewBox='0 0 24 24'><path d='M6 14h2v6H6z'/></svg>";
    const NOT_LISTED: &str = "zz_unlisted";

    // ----- a tiny JSON builder (the table is walked, so the values are built, not typed) -----

    #[derive(Clone, Debug, PartialEq)]
    enum J {
        Null,
        Bool(bool),
        Int(i64),
        Num(f64),
        Str(String),
        Arr(Vec<J>),
        Obj(Vec<(String, J)>),
        /// Text written as it is: a field's documented default is JSON text.
        Raw(String),
    }

    fn s(text: &str) -> J {
        J::Str(text.to_owned())
    }

    fn write_str(out: &mut String, text: &str) {
        out.push('"');
        for c in text.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
    }

    impl J {
        fn render(&self) -> String {
            let mut out = String::new();
            self.write(&mut out);
            out
        }

        fn write(&self, out: &mut String) {
            match self {
                J::Null => out.push_str("null"),
                J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                J::Int(i) => out.push_str(&i.to_string()),
                J::Num(n) => out.push_str(&format!("{n:?}")),
                J::Str(text) => write_str(out, text),
                J::Raw(text) => out.push_str(text),
                J::Arr(items) => {
                    out.push('[');
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        item.write(out);
                    }
                    out.push(']');
                }
                J::Obj(pairs) => {
                    out.push('{');
                    for (i, (key, value)) in pairs.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        write_str(out, key);
                        out.push(':');
                        value.write(out);
                    }
                    out.push('}');
                }
            }
        }

        fn get(&self, key: &str) -> Option<&J> {
            match self {
                J::Obj(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
                _ => None,
            }
        }
        fn has(&self, key: &str) -> bool {
            self.get(key).is_some()
        }
        fn name(&self, key: &str) -> Option<&str> {
            match self.get(key) {
                Some(J::Str(text)) => Some(text.as_str()),
                _ => None,
            }
        }
        fn int(&self, key: &str) -> Option<i64> {
            match self.get(key) {
                Some(J::Int(n)) => Some(*n),
                _ => None,
            }
        }
        fn set(&mut self, key: &str, value: J) {
            if let J::Obj(pairs) = self {
                match pairs.iter_mut().find(|(k, _)| k == key) {
                    Some(slot) => slot.1 = value,
                    None => pairs.push((key.to_owned(), value)),
                }
            }
        }
        fn remove(&mut self, key: &str) {
            if let J::Obj(pairs) = self {
                pairs.retain(|(k, _)| k != key);
            }
        }
    }

    // ----- what the table lists -----

    /// Strings of a pattern, valid and (for the refusals) not.
    fn pattern_valid(pattern: Pattern) -> Vec<String> {
        match pattern {
            Pattern::ReverseDns => vec!["com.example.hud".into(), "a.b".into(), schema::BUILTIN_CLASSIC_ID.into()],
            Pattern::Semver => vec!["1.2.3".into(), "0.0.0".into()],
            Pattern::FormatVersion => vec!["1.0".into(), "12.34".into()],
            Pattern::SchemaUrl => vec!["https://plxnative.com/hud/schema/1.0.json".into()],
            Pattern::Id => vec!["a".into(), "a_b-c9".into(), "a".repeat(40)],
        }
    }

    fn pattern_invalid(pattern: Pattern) -> Vec<String> {
        let common = String::new();
        match pattern {
            Pattern::ReverseDns => vec![
                common,
                "nodots".into(),
                "Com.Bad".into(),
                "a.1b".into(),
                "builtin:Classic".into(),
                "builtin:other".into(),
                "builtin:".into(),
                "xbuiltin:classic".into(),
                "builtin:classic ".into(),
            ],
            Pattern::Semver => vec![common, "1.0".into(), "1.0.0-beta".into(), "01.0.0".into()],
            Pattern::FormatVersion => vec![common, "1".into(), "v1.0".into(), "1.0.0".into()],
            Pattern::SchemaUrl => vec![common, "http://plxnative.com/x".into(), "plxnative.com".into()],
            Pattern::Id => vec![common, "A".into(), "1a".into(), "a".repeat(41)],
        }
    }

    /// The values of `ty` the table lists: every one must be accepted.
    fn variants(ty: &Ty) -> Vec<J> {
        match *ty {
            Ty::Bool => vec![J::Bool(false), J::Bool(true)],
            Ty::Int { min, max } => {
                let mut out = Vec::new();
                for n in [min, max, min + (max - min) / 2] {
                    if !out.contains(&J::Int(n)) {
                        out.push(J::Int(n));
                    }
                }
                out
            }
            Ty::Num { min, max } => {
                let mut out = Vec::new();
                for n in [min, max, (min + max) / 2.0] {
                    if !out.contains(&J::Num(n)) {
                        out.push(J::Num(n));
                    }
                }
                out
            }
            Ty::Text { max_len } => vec![s("x"), J::Str("a".repeat(max_len))],
            Ty::Pattern(pattern) => pattern_valid(pattern).into_iter().map(J::Str).collect(),
            Ty::Enum(names) => names.iter().map(|n| s(n)).collect(),
            Ty::EnumList(names) => {
                let mut out = vec![J::Arr(Vec::new())];
                out.extend(names.iter().map(|n| J::Arr(vec![s(n)])));
                out.push(J::Arr(names.iter().map(|n| s(n)).collect()));
                out
            }
            Ty::Bind(names) => names.iter().map(|n| s(n)).collect(),
            Ty::Size => schema::SIZE_RUNGS.iter().map(|(n, _)| s(n)).collect(),
            Ty::Gap => {
                let mut out: Vec<J> = schema::SPACE_TOKENS.iter().map(|(n, _)| s(n)).collect();
                out.extend([J::Int(0), J::Int(24), J::Int(1080)]);
                out
            }
            Ty::Ink => {
                let mut out: Vec<J> = schema::INK_TOKENS.iter().map(|(n, _)| s(n)).collect();
                out.push(s("gold"));
                out
            }
            Ty::Asset => vec![s("glyph")],
            Ty::Face => {
                let mut out = vec![s("glyph")];
                out.extend(schema::BUILTIN_ICONS.iter().map(|(n, _)| J::Str(format!("builtin:{n}"))));
                out
            }
            Ty::Cond => {
                let mut out: Vec<J> = schema::CONDITIONS.iter().map(|c| s(c.name)).collect();
                out.extend(schema::CONDITIONS.iter().map(|c| J::Str(format!("!{}", c.name))));
                out.push(J::Arr(vec![s("panel.info"), s("!sections")]));
                out
            }
            Ty::Object(name) => {
                let node = schema::node(name).unwrap_or_else(|| panic!("the table has no node `{name}`"));
                vec![node_sample(node, None, None)]
            }
            Ty::List { of, max_len } => {
                let one = first(of);
                vec![J::Arr(Vec::new()), J::Arr(vec![one.clone()]), J::Arr(vec![one; max_len])]
            }
            Ty::Palette => vec![J::Obj(Vec::new()), J::Obj(vec![("gold".into(), s("#f2b535"))])],
            Ty::Assets => vec![J::Obj(Vec::new()), J::Obj(vec![("glyph".into(), s(SVG))])],
            Ty::Command => schema::COMMANDS.iter().map(|c| s(c.name)).collect(),
            Ty::Either(a, b) => {
                let mut out = variants(a);
                out.extend(variants(b));
                out
            }
        }
    }

    fn first(ty: &Ty) -> J {
        variants(ty).remove(0)
    }

    /// Whether a JSON object can be the value of `ty` itself (so it is not read as a state-keyed value).
    fn takes_object(ty: &Ty) -> bool {
        match *ty {
            Ty::Object(_) | Ty::Face | Ty::Palette | Ty::Assets | Ty::Cond => true,
            Ty::Either(a, b) => takes_object(a) || takes_object(b),
            Ty::Bool
            | Ty::Int { .. }
            | Ty::Num { .. }
            | Ty::Text { .. }
            | Ty::Pattern(_)
            | Ty::Enum(_)
            | Ty::EnumList(_)
            | Ty::Bind(_)
            | Ty::Size
            | Ty::Gap
            | Ty::Ink
            | Ty::Asset
            | Ty::List { .. }
            | Ty::Command => false,
        }
    }

    type Refusal = (J, &'static Code, String);

    /// Values the table does not list, with the code and the pointer tail below the value that refuses each.
    fn refusals(ty: &Ty) -> Vec<Refusal> {
        let at = |value: J, code: &'static Code, tail: &str| -> Refusal { (value, code, tail.to_owned()) };
        let wrong = |value: J| at(value, &codes::E0104, "");
        match *ty {
            Ty::Bool => vec![wrong(J::Null), wrong(s("true")), wrong(J::Int(1))],
            Ty::Int { min, max } => {
                let mut out = vec![wrong(J::Null), wrong(s("1")), wrong(J::Num(1.5))];
                if let Some(below) = min.checked_sub(1) {
                    out.push(at(J::Int(below), &codes::E0105, ""));
                }
                if let Some(above) = max.checked_add(1) {
                    out.push(at(J::Int(above), &codes::E0105, ""));
                }
                out
            }
            Ty::Num { min, max } => vec![
                wrong(J::Null),
                wrong(s("1")),
                at(J::Num(min - 1.0), &codes::E0105, ""),
                at(J::Num(max + 1.0), &codes::E0105, ""),
            ],
            Ty::Text { max_len } => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(J::Str("a".repeat(max_len + 1)), &codes::E0105, ""),
                at(s("a\u{1}b"), &codes::E0105, ""),
            ],
            Ty::Pattern(pattern) => {
                let mut out = vec![wrong(J::Null), wrong(J::Int(5))];
                out.extend(pattern_invalid(pattern).into_iter().map(|text| at(J::Str(text), &codes::E0105, "")));
                out
            }
            Ty::Enum(_) => vec![wrong(J::Null), wrong(J::Int(5)), at(s(NOT_LISTED), &codes::E0201, "")],
            Ty::EnumList(names) => vec![
                wrong(J::Null),
                wrong(s("x")),
                at(J::Arr(vec![s(NOT_LISTED)]), &codes::E0201, "/0"),
                at(J::Arr(vec![J::Int(7)]), &codes::E0104, "/0"),
                at(J::Arr(vec![s(names[0]), s(names[0])]), &codes::E0105, "/1"),
            ],
            Ty::Bind(_) => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(s(NOT_LISTED), &codes::E0201, ""),
                // A name of the binding list that no key may write.
                at(s("playbar.pos"), &codes::E0201, ""),
            ],
            Ty::Size => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(s(NOT_LISTED), &codes::E0201, ""),
                at(s("DIAGNOSTIC"), &codes::E0201, ""),
            ],
            Ty::Gap => vec![
                wrong(J::Null),
                wrong(J::Bool(true)),
                at(s(NOT_LISTED), &codes::E0201, ""),
                at(J::Int(-1), &codes::E0105, ""),
                at(J::Int(1081), &codes::E0105, ""),
            ],
            Ty::Ink => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(s(NOT_LISTED), &codes::E0204, ""),
                at(s("ZZ_UNLISTED"), &codes::E0202, ""),
                at(s("PASS_GOLD"), &codes::E0202, ""),
            ],
            Ty::Asset => vec![wrong(J::Null), wrong(J::Int(5)), at(s(NOT_LISTED), &codes::E0203, "")],
            Ty::Face => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(s(NOT_LISTED), &codes::E0203, ""),
                at(s("builtin:zz_unlisted"), &codes::E0203, ""),
                at(s("builtin:rewind"), &codes::E0203, ""),
            ],
            Ty::Cond => vec![
                at(J::Null, &codes::E0502, ""),
                at(J::Int(5), &codes::E0502, ""),
                at(s(NOT_LISTED), &codes::E0501, ""),
                at(s("!"), &codes::E0502, ""),
                at(J::Arr(Vec::new()), &codes::E0502, ""),
                at(J::Arr(vec![J::Int(5)]), &codes::E0502, "/0"),
            ],
            Ty::Object(_) => vec![wrong(J::Null), wrong(J::Int(5)), wrong(s("x")), wrong(J::Arr(Vec::new()))],
            Ty::List { of, max_len } => vec![
                wrong(J::Null),
                wrong(s("x")),
                at(J::Arr(vec![first(of); max_len + 1]), &codes::E0105, ""),
            ],
            Ty::Palette => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(J::Obj(vec![("Zz_Name".into(), s("#ffffff"))]), &codes::E0505, "/Zz_Name"),
                at(J::Obj(vec![("gold".into(), s("red"))]), &codes::E0506, "/gold"),
                at(J::Obj(vec![("gold".into(), J::Int(5))]), &codes::E0104, "/gold"),
            ],
            Ty::Assets => vec![
                wrong(J::Null),
                wrong(J::Int(5)),
                at(J::Obj(vec![("ZZ".into(), s(SVG))]), &codes::E0105, "/ZZ"),
                at(J::Obj(vec![("glyph".into(), s("<svg>"))]), &codes::E0704, "/glyph"),
                at(J::Obj(vec![("glyph".into(), J::Int(5))]), &codes::E0104, "/glyph"),
            ],
            Ty::Command => vec![wrong(J::Null), wrong(J::Int(5)), at(s(NOT_LISTED), &codes::E0301, "")],
            Ty::Either(a, b) => {
                // A value of the wrong kind for both sides, then each side's out-of-vocabulary values
                // (the other refusals' kinds would be read by the other side).
                let mut out = vec![wrong(J::Null)];
                for side in [a, b] {
                    out.extend(refusals(side).into_iter().filter(|(_, code, _)| code.id != codes::E0104.id));
                }
                out
            }
        }
    }

    // ----- samples of whole nodes, and the repair that makes their rules hold -----

    fn effective_cmd(node: &'static Node, cmd: Option<&'static Cmd>) -> Option<&'static Cmd> {
        match node.kind {
            NodeKind::CommandDispatched => {
                Some(cmd.unwrap_or_else(|| schema::command("next").expect("`next` is a command")))
            }
            NodeKind::Plain | NodeKind::OneOf(_) => None,
        }
    }

    fn field_of(
        node: &'static Node,
        cmd: Option<&'static Cmd>,
        key: &str,
    ) -> Option<&'static schema::Field> {
        schema::field(node, key).or_else(|| cmd.and_then(|c| c.param(key)))
    }

    /// A valid object of `node`: its required keys at their first listed value, and for a cell the
    /// content key `kind` (a `spacer` when none is named), for a control item the command's own
    /// required parameters.
    fn node_sample(node: &'static Node, kind: Option<&str>, cmd: Option<&'static Cmd>) -> J {
        let cmd = effective_cmd(node, cmd);
        let mut d = J::Obj(Vec::new());
        for f in node.fields.iter().filter(|f| f.required) {
            let value = match (f.ty, cmd) {
                (Ty::Command, Some(c)) => s(c.name),
                _ => first(&f.ty),
            };
            d.set(f.key, value);
        }
        if let NodeKind::OneOf(kinds) = node.kind {
            let chosen = kinds
                .iter()
                .copied()
                .find(|k| Some(*k) == kind)
                .unwrap_or("spacer");
            let f = schema::field(node, chosen).expect("a content key is a field of its cell");
            d.set(chosen, first(&f.ty));
        }
        if let Some(c) = cmd {
            for p in c.params.iter().filter(|p| p.required) {
                d.set(p.key, first(&p.ty));
            }
        }
        repair(node, cmd, &mut d, "");
        d
    }

    /// `node`'s sample with `key` set to `value`, repaired around it (`key` is never changed). The flag
    /// says whether every rule could be satisfied around that value.
    fn draft(node: &'static Node, cmd: Option<&'static Cmd>, key: &str, value: J) -> (J, bool) {
        let cmd = effective_cmd(node, cmd);
        let mut d = node_sample(node, Some(key), cmd);
        d.set(key, value);
        let ok = repair(node, cmd, &mut d, key);
        (d, ok)
    }

    fn command_named(ty: &Ty, value: &J) -> Option<&'static Cmd> {
        match (ty, value) {
            (Ty::Command, J::Str(name)) => schema::command(name),
            _ => None,
        }
    }

    /// Edit `d` until the rules of `node` (and of `cmd`) hold, never touching the `pinned` key. `false`
    /// when a rule contradicts the pinned value.
    fn repair(node: &'static Node, cmd: Option<&'static Cmd>, d: &mut J, pinned: &str) -> bool {
        let mut rules: Vec<Rule> = node.rules.to_vec();
        if let Some(c) = cmd {
            rules.extend_from_slice(c.rules);
        }
        for _ in 0..8 {
            let before = d.clone();
            for rule in &rules {
                if !apply(rule, node, cmd, d, pinned) {
                    return false;
                }
            }
            if *d == before {
                return true;
            }
        }
        false
    }

    /// The first listed value of `key`'s type that is not the name `not`.
    fn other_than(node: &'static Node, cmd: Option<&'static Cmd>, key: &str, not: &str) -> Option<J> {
        let f = field_of(node, cmd, key)?;
        variants(&f.ty)
            .into_iter()
            .find(|v| !matches!(v, J::Str(text) if text == not))
    }

    fn insert_first(node: &'static Node, cmd: Option<&'static Cmd>, d: &mut J, key: &str) -> bool {
        match field_of(node, cmd, key) {
            Some(f) => {
                d.set(key, first(&f.ty));
                true
            }
            None => false,
        }
    }

    fn apply(rule: &Rule, node: &'static Node, cmd: Option<&'static Cmd>, d: &mut J, pinned: &str) -> bool {
        match *rule {
            Rule::Requires { key, other } => {
                if d.has(key) && !d.has(other) {
                    return insert_first(node, cmd, d, other);
                }
                true
            }
            Rule::Excludes { a, b } => {
                if d.has(a) && d.has(b) {
                    if b != pinned {
                        d.remove(b);
                    } else if a != pinned {
                        d.remove(a);
                    } else {
                        return false;
                    }
                }
                true
            }
            Rule::IfValue { key, equals, require, forbid } => {
                if d.name(key) == Some(equals) {
                    for r in require {
                        if !d.has(r) && !insert_first(node, cmd, d, r) {
                            return false;
                        }
                    }
                    for f in forbid {
                        if !d.has(f) {
                            continue;
                        }
                        if *f != pinned {
                            d.remove(f);
                        } else if key != pinned {
                            // The pinned key is forbidden by this value: take another value.
                            match other_than(node, cmd, key, equals) {
                                Some(v) => d.set(key, v),
                                None => return false,
                            }
                        } else {
                            return false;
                        }
                    }
                }
                true
            }
            Rule::IfNotValue { key, equals, require, forbid } => {
                if d.has(key) && d.name(key) != Some(equals) {
                    for r in require {
                        if !d.has(r) && !insert_first(node, cmd, d, r) {
                            return false;
                        }
                    }
                    for f in forbid {
                        if !d.has(f) {
                            continue;
                        }
                        if *f != pinned {
                            d.remove(f);
                        } else if key != pinned {
                            d.set(key, s(equals));
                        } else {
                            return false;
                        }
                    }
                }
                true
            }
            Rule::ForbidPair { a, a_value, b, b_value } => {
                if d.name(a) == Some(a_value) && d.name(b) == Some(b_value) {
                    if b != pinned {
                        match other_than(node, cmd, b, b_value) {
                            Some(v) => d.set(b, v),
                            None => return false,
                        }
                    } else if a != pinned {
                        match other_than(node, cmd, a, a_value) {
                            Some(v) => d.set(a, v),
                            None => return false,
                        }
                    } else {
                        return false;
                    }
                }
                true
            }
            Rule::Disjoint { a, b } => {
                let (Some(J::Arr(la)), Some(J::Arr(lb))) = (d.get(a).cloned(), d.get(b).cloned()) else {
                    return true;
                };
                let shared: Vec<J> = la.iter().filter(|x| lb.contains(x)).cloned().collect();
                if shared.is_empty() {
                    return true;
                }
                if a == pinned && b == pinned {
                    return false;
                }
                let (key, list) = if b == pinned { (a, la) } else { (b, lb) };
                d.set(key, J::Arr(list.into_iter().filter(|x| !shared.contains(x)).collect()));
                true
            }
            Rule::NonZero(key) => {
                if d.int(key) == Some(0) {
                    if key == pinned {
                        return false;
                    }
                    d.set(key, J::Int(1));
                }
                true
            }
            Rule::NameOnlyWith { key, name, other, other_value } => {
                if d.name(key) == Some(name) && d.name(other) != Some(other_value) {
                    if other != pinned {
                        d.set(other, s(other_value));
                    } else if key != pinned {
                        match other_than(node, cmd, key, name) {
                            Some(v) => d.set(key, v),
                            None => return false,
                        }
                    } else {
                        return false;
                    }
                }
                true
            }
            Rule::AtMost { key, other } => {
                if let (Some(a), Some(b)) = (d.int(key), d.int(other)) {
                    if a > b {
                        if other != pinned {
                            d.set(other, J::Int(a));
                        } else if key != pinned {
                            d.set(key, J::Int(b));
                        } else {
                            return false;
                        }
                    }
                }
                true
            }
            Rule::FaceMapOnlyWhen { param, equals } => {
                if matches!(d.get("face"), Some(J::Obj(_))) && d.name(param) != Some(equals) {
                    if param == pinned {
                        return false;
                    }
                    d.set(param, s(equals));
                }
                true
            }
        }
    }

    // ----- judging -----

    /// Inside a strip card, so every binding and condition of the table may be written.
    fn ctx() -> Ctx {
        Ctx {
            palette: vec!["gold".to_owned()],
            assets: vec!["glyph".to_owned()],
            format: schema::CURRENT,
            in_strip_card: true,
        }
    }

    fn judge(node: &'static Node, d: &J, ctx: &Ctx) -> Vec<Finding> {
        let (object, findings) = parse::validate_json(node, &d.render(), AT, ctx);
        assert_eq!(
            object.is_some(),
            !has_error(&findings),
            "an object exists exactly when no finding is an error: {}\n{}",
            d.render(),
            dump(&findings)
        );
        findings.iter().for_each(assert_lint_shaped);
        findings
    }

    fn expect_accepted(node: &'static Node, d: &J, ctx: &Ctx, what: &str) {
        let found = judge(node, d, ctx);
        assert!(found.is_empty(), "{what} must be accepted:\n{}\n{}", d.render(), dump(&found));
    }

    fn expect_refused(node: &'static Node, d: &J, ctx: &Ctx, code: &Code, pointer: &str, what: &str) {
        let found = judge(node, d, ctx);
        assert!(
            found.iter().any(|f| f.code == code.id && f.pointer == pointer),
            "{what}: expected {} at {pointer:?}\n{}\n{}",
            code.id,
            d.render(),
            dump(&found)
        );
    }

    fn entry(node: &Node, key: &str) -> String {
        format!("{}:{key}", node.name)
    }

    fn table_entries() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for node in schema::all_nodes() {
            for f in node.fields {
                out.insert(entry(node, f.key));
            }
        }
        out
    }

    fn table_params() -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for cmd in schema::COMMANDS {
            for p in cmd.params {
                out.insert(format!("{}:{}", cmd.name, p.key));
            }
        }
        out
    }

    fn item_node() -> &'static Node {
        schema::node("item").expect("the item node")
    }

    // ----- the tests -----

    #[test]
    fn the_table_has_the_shape_the_spec_freezes() {
        assert_eq!(schema::all_nodes().len(), 24, "nodes");
        assert_eq!(schema::COMMANDS.len(), 10, "commands");
        assert_eq!(schema::CONDITIONS.len(), 11, "conditions");
        assert_eq!(schema::all_nodes()[0].name, "package");
        let mut names: Vec<&str> = schema::all_nodes().iter().map(|n| n.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "a node name is unique");
        for node in schema::all_nodes() {
            let mut keys: Vec<&str> = node.fields.iter().map(|f| f.key).collect();
            keys.sort_unstable();
            let before = keys.len();
            keys.dedup();
            assert_eq!(before, keys.len(), "`{}` lists a key twice", node.name);
        }
    }

    #[test]
    fn every_key_of_every_node_accepts_every_value_the_table_lists() {
        let mut exercised = BTreeSet::new();
        let (mut accepted, mut skipped) = (0usize, 0usize);
        for node in schema::all_nodes() {
            for f in node.fields {
                let mut placed = 0usize;
                for value in variants(&f.ty) {
                    let cmd = command_named(&f.ty, &value);
                    let (d, ok) = draft(node, cmd, f.key, value.clone());
                    if !ok {
                        skipped += 1;
                        continue;
                    }
                    expect_accepted(node, &d, &ctx(), &format!("{}.{} = {}", node.name, f.key, value.render()));
                    placed += 1;
                }
                assert!(placed > 0, "`{}.{}`: the sampler placed no value", node.name, f.key);
                accepted += placed;
                exercised.insert(entry(node, f.key));
            }
        }
        let expected = table_entries();
        let fields: usize = schema::all_nodes().iter().map(|n| n.fields.len()).sum();
        assert_eq!(expected.len(), fields, "`node:key` names one table entry");
        assert_eq!(exercised, expected, "the walk reached every key of every node");
        assert!(accepted >= 3 * fields / 2, "{accepted} accepted values for {fields} keys");
        assert!(skipped * 10 <= accepted, "the sampler skipped {skipped} of {accepted}");
    }

    #[test]
    fn every_key_of_every_node_refuses_what_the_table_does_not_list() {
        let mut exercised = BTreeSet::new();
        let mut refused = 0usize;
        for node in schema::all_nodes() {
            for f in node.fields {
                let mut any = false;
                for (bad, code, tail) in refusals(&f.ty) {
                    let cmd = command_named(&f.ty, &bad);
                    let (d, _) = draft(node, cmd, f.key, bad.clone());
                    let pointer = format!("{AT}/{}{tail}", f.key);
                    expect_refused(node, &d, &ctx(), code, &pointer, &format!("{}.{} = {}", node.name, f.key, bad.render()));
                    refused += 1;
                    any = true;
                }
                assert!(any, "`{}.{}` has no refusal", node.name, f.key);
                exercised.insert(entry(node, f.key));
            }
        }
        assert_eq!(exercised, table_entries());
        assert!(refused >= 4 * exercised.len(), "{refused} refusals for {} keys", exercised.len());
    }

    #[test]
    fn every_node_refuses_an_unlisted_key_and_a_missing_required_key() {
        for node in schema::all_nodes() {
            let base = node_sample(node, None, None);
            expect_accepted(node, &base, &ctx(), &format!("the {} sample", node.name));

            let mut unlisted = base.clone();
            unlisted.set("zz_unlisted_key", J::Int(1));
            // A control item reads sibling keys as the command's parameters, so its unknown key is E0302.
            let code = match node.kind {
                NodeKind::CommandDispatched => &codes::E0302,
                NodeKind::Plain | NodeKind::OneOf(_) => &codes::E0102,
            };
            let at = format!("{AT}/zz_unlisted_key");
            expect_refused(node, &unlisted, &ctx(), code, &at, &format!("{} with an unlisted key", node.name));

            // The same key spelt like a listed one, one letter off, offers the listed name.
            if let Some(f) = node.fields.first() {
                let mut off = base.clone();
                off.set(&format!("{}x", f.key), J::Int(1));
                let found = judge(node, &off, &ctx());
                let hit = found
                    .iter()
                    .find(|x| x.code == code.id && x.pointer == format!("{AT}/{}x", f.key))
                    .unwrap_or_else(|| panic!("{}: a misspelt key is refused\n{}", node.name, dump(&found)));
                assert!(!hit.nearest.is_empty(), "{}: the finding offers a nearest name", node.name);
            }

            for f in node.fields.iter().filter(|f| f.required) {
                let mut missing = base.clone();
                missing.remove(f.key);
                expect_refused(node, &missing, &ctx(), &codes::E0103, AT, &format!("{} without {}", node.name, f.key));
            }
        }
    }

    #[test]
    fn a_state_keyed_value_is_accepted_exactly_where_the_table_allows_it() {
        let (mut keyed_ok, mut keyed_refused) = (0usize, 0usize);
        for node in schema::all_nodes() {
            for f in node.fields {
                if takes_object(&f.ty) {
                    continue;
                }
                let base = first(&f.ty);
                let other = variants(&f.ty).pop().expect("a variant");
                let keyed = J::Obj(vec![
                    ("value".into(), base),
                    ("if".into(), J::Obj(vec![("panel.info".into(), other.clone()), ("!sections".into(), other)])),
                ]);
                let (d, ok) = draft(node, None, f.key, keyed);
                if !ok {
                    continue;
                }
                let what = format!("{}.{} state-keyed", node.name, f.key);
                if matches!(f.ty, Ty::Command) {
                    // The command name selects the table the rest of the item is read against, so it is
                    // read before anything else: a keyed object there is a wrong type (E0104), not E0504.
                    expect_refused(node, &d, &ctx(), &codes::E0104, &format!("{AT}/{}", f.key), &what);
                    continue;
                }
                if f.keyable {
                    expect_accepted(node, &d, &ctx(), &what);
                    keyed_ok += 1;
                } else {
                    expect_refused(node, &d, &ctx(), &codes::E0504, &format!("{AT}/{}", f.key), &what);
                    keyed_refused += 1;
                }
            }
        }
        let keyable = schema::all_nodes()
            .iter()
            .flat_map(|n| n.fields.iter())
            .filter(|f| f.keyable && !takes_object(&f.ty))
            .count();
        assert_eq!(keyed_ok, keyable, "every keyable key was tried");
        assert!(keyed_ok >= 30 && keyed_refused >= 60, "{keyed_ok} keyed, {keyed_refused} refused");
    }

    #[test]
    fn a_key_with_a_documented_default_accepts_it() {
        let mut checked = 0usize;
        for node in schema::all_nodes() {
            for f in node.fields {
                let Some(text) = f.default else { continue };
                let (d, ok) = draft(node, None, f.key, J::Raw(text.to_owned()));
                assert!(ok, "{}.{} default {text}", node.name, f.key);
                expect_accepted(node, &d, &ctx(), &format!("{}.{} = its default {text}", node.name, f.key));
                checked += 1;
            }
        }
        for cmd in schema::COMMANDS {
            for p in cmd.params {
                let Some(text) = p.default else { continue };
                let (d, ok) = draft(item_node(), Some(cmd), p.key, J::Raw(text.to_owned()));
                assert!(ok, "{}:{} default {text}", cmd.name, p.key);
                expect_accepted(item_node(), &d, &ctx(), &format!("{}:{} = its default {text}", cmd.name, p.key));
                checked += 1;
            }
        }
        assert!(checked >= 15, "{checked} defaults");
    }

    fn state_value(state: &schema::FaceState) -> J {
        if state.ladder {
            J::Obj(vec![("10".into(), s("glyph")), ("30".into(), s("builtin:play"))])
        } else {
            s("glyph")
        }
    }

    fn face_map(states: &[(&str, J)]) -> J {
        J::Obj(states.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect())
    }

    #[test]
    fn every_command_and_parameter_is_accepted_and_every_unlisted_one_refused() {
        let item = item_node();
        let mut exercised = BTreeSet::new();
        for cmd in schema::COMMANDS {
            let base = node_sample(item, None, Some(cmd));
            expect_accepted(item, &base, &ctx(), &format!("an item of `{}`", cmd.name));

            // A parameter no command lists.
            let mut unlisted = base.clone();
            unlisted.set("zz_unlisted_param", J::Int(1));
            expect_refused(item, &unlisted, &ctx(), &codes::E0302, &format!("{AT}/zz_unlisted_param"), cmd.name);

            for p in cmd.params {
                let mut placed = 0usize;
                for value in variants(&p.ty) {
                    let (d, ok) = draft(item, Some(cmd), p.key, value.clone());
                    if !ok {
                        continue;
                    }
                    expect_accepted(item, &d, &ctx(), &format!("{}:{} = {}", cmd.name, p.key, value.render()));
                    placed += 1;
                }
                assert!(placed > 0, "{}:{}: nothing placed", cmd.name, p.key);

                // Every refusal of a parameter's type is E0304 at the parameter (a command-shaped code).
                for (bad, code, tail) in refusals(&p.ty) {
                    let (d, _) = draft(item, Some(cmd), p.key, bad.clone());
                    let found = judge(item, &d, &ctx());
                    let wanted = if code.id == codes::E0301.id { code } else { &codes::E0304 };
                    let at = format!("{AT}/{}{tail}", p.key);
                    assert!(
                        found.iter().any(|f| f.code == wanted.id && f.pointer == at),
                        "{}:{} = {}: expected {} at {at}\n{}",
                        cmd.name,
                        p.key,
                        bad.render(),
                        wanted.id,
                        dump(&found)
                    );
                }

                // A required parameter that is missing.
                if p.required {
                    let mut missing = base.clone();
                    missing.remove(p.key);
                    expect_refused(item, &missing, &ctx(), &codes::E0303, AT, &format!("{} without {}", cmd.name, p.key));
                }
                exercised.insert(format!("{}:{}", cmd.name, p.key));
            }

            // Another command's parameters are unlisted for this one.
            for other in schema::COMMANDS.iter().filter(|o| o.name != cmd.name) {
                for p in other.params.iter().filter(|p| cmd.param(p.key).is_none()) {
                    let mut d = base.clone();
                    d.set(p.key, first(&p.ty));
                    expect_refused(item, &d, &ctx(), &codes::E0302, &format!("{AT}/{}", p.key), cmd.name);
                }
            }

            // The face: a single face always; a map only for a command with states.
            let make = |face: J| draft(item, Some(cmd), "face", face);
            if cmd.face_states.is_empty() {
                let (d, _) = make(face_map(&[("default", s("glyph"))]));
                expect_refused(item, &d, &ctx(), &codes::E0304, &format!("{AT}/face"), &format!("{} has no face states", cmd.name));
            } else {
                let full: Vec<(&str, J)> = cmd.face_states.iter().map(|st| (st.key, state_value(st))).collect();
                let (d, ok) = make(face_map(&full));
                assert!(ok, "{}: the face map could not be placed", cmd.name);
                expect_accepted(item, &d, &ctx(), &format!("{} with every face state", cmd.name));

                let required: Vec<(&str, J)> = cmd
                    .face_states
                    .iter()
                    .filter(|st| st.required)
                    .map(|st| (st.key, state_value(st)))
                    .collect();
                let (d, _) = make(face_map(&required));
                expect_accepted(item, &d, &ctx(), &format!("{} with the required face states", cmd.name));

                for st in cmd.face_states.iter().filter(|st| st.required) {
                    let without: Vec<(&str, J)> = full.iter().filter(|(k, _)| *k != st.key).cloned().collect();
                    let (d, _) = make(face_map(&without));
                    expect_refused(item, &d, &ctx(), &codes::E0303, &format!("{AT}/face"), &format!("{} without face state {}", cmd.name, st.key));
                }

                let mut extra = full.clone();
                extra.push((NOT_LISTED, s("glyph")));
                let (d, _) = make(face_map(&extra));
                expect_refused(item, &d, &ctx(), &codes::E0302, &format!("{AT}/face/{NOT_LISTED}"), &format!("{} with an unlisted face state", cmd.name));

                for st in cmd.face_states {
                    let mut bad = full.clone();
                    if let Some(slot) = bad.iter_mut().find(|(k, _)| *k == st.key) {
                        slot.1 = if st.ladder { J::Obj(vec![("abc".into(), s("glyph"))]) } else { s("zz_unlisted") };
                    }
                    let (d, _) = make(face_map(&bad));
                    if st.ladder {
                        expect_refused(item, &d, &ctx(), &codes::E0102, &format!("{AT}/face/{}/abc", st.key), &format!("{} ladder rung", cmd.name));
                    } else {
                        expect_refused(item, &d, &ctx(), &codes::E0203, &format!("{AT}/face/{}", st.key), &format!("{} face state asset", cmd.name));
                    }
                }
            }
        }
        assert_eq!(exercised, table_params(), "the walk reached every parameter of every command");
        assert_eq!(exercised.len(), 5);
    }

    #[test]
    fn a_cell_has_exactly_one_content_key() {
        let cell = schema::node("cell").expect("the cell node");
        let NodeKind::OneOf(kinds) = cell.kind else {
            panic!("a cell is a one-of node");
        };
        assert_eq!(kinds.len(), 12);
        for kind in kinds {
            let (d, _) = draft(cell, None, kind, first(&schema::field(cell, kind).expect("a field").ty));
            expect_accepted(cell, &d, &ctx(), &format!("a cell of kind {kind}"));
            // Every content key is a field of the cell, and optional (the one-of rule is the requirement).
            assert!(!schema::field(cell, kind).expect("a field").required, "{kind}");
            // Naming a second kind as well is E0106 at the cell.
            let other = kinds.iter().find(|k| *k != kind).expect("another kind");
            let mut two = d.clone();
            two.set(other, first(&schema::field(cell, other).expect("a field").ty));
            expect_refused(cell, &two, &ctx(), &codes::E0106, AT, &format!("a cell of {kind} and {other}"));
            // And naming none is E0106 too (the schema doc says E0103; the parser answers E0106).
            let mut none = d.clone();
            none.remove(kind);
            expect_refused(cell, &none, &ctx(), &codes::E0106, AT, &format!("a cell without {kind}"));
        }
    }

    #[test]
    fn card_scoped_names_are_legal_only_in_a_strip_card() {
        let outside = Ctx { in_strip_card: false, ..ctx() };
        let (mut binds, mut conds) = (0usize, 0usize);
        for node in schema::all_nodes() {
            for f in node.fields {
                match f.ty {
                    Ty::Bind(names) => {
                        for name in names {
                            let (d, ok) = draft(node, None, f.key, s(name));
                            assert!(ok, "{}.{} = {name}", node.name, f.key);
                            let what = format!("{}.{} = {name} outside a card", node.name, f.key);
                            if schema::is_card_scoped(name) {
                                expect_refused(node, &d, &outside, &codes::E0201, &format!("{AT}/{}", f.key), &what);
                            } else {
                                expect_accepted(node, &d, &outside, &what);
                            }
                            expect_accepted(node, &d, &ctx(), &format!("{}.{} = {name} inside a card", node.name, f.key));
                            binds += 1;
                        }
                    }
                    Ty::Cond => {
                        for condition in schema::CONDITIONS {
                            for text in [condition.name.to_owned(), format!("!{}", condition.name)] {
                                let (d, ok) = draft(node, None, f.key, J::Str(text.clone()));
                                assert!(ok);
                                let what = format!("{}.{} = {text} outside a card", node.name, f.key);
                                if schema::is_card_scoped(condition.name) {
                                    expect_refused(node, &d, &outside, &codes::E0503, &format!("{AT}/{}", f.key), &what);
                                } else {
                                    expect_accepted(node, &d, &outside, &what);
                                }
                                expect_accepted(node, &d, &ctx(), &format!("{}.{} = {text} inside a card", node.name, f.key));
                                conds += 1;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        assert!(binds >= 10 && conds >= 3 * 22, "{binds} bindings, {conds} conditions");

        // A state-keyed value names its conditions as keys, with the same scope.
        let band = schema::node("band").expect("band");
        for condition in schema::CONDITIONS {
            let keyed = J::Obj(vec![
                ("value".into(), s("XS")),
                ("if".into(), J::Obj(vec![(condition.name.to_owned(), s("SM"))])),
            ]);
            let (d, _) = draft(band, None, "gap_before", keyed);
            if schema::is_card_scoped(condition.name) {
                expect_refused(band, &d, &outside, &codes::E0503, &format!("{AT}/gap_before/if/{}", condition.name), condition.name);
            } else {
                expect_accepted(band, &d, &outside, condition.name);
            }
            expect_accepted(band, &d, &ctx(), condition.name);
        }
    }

    #[test]
    fn every_name_of_a_vocabulary_is_unique_and_spelt_in_its_own_case() {
        fn unique(what: &str, names: Vec<&str>) {
            let mut sorted = names.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), names.len(), "{what} lists a name twice");
        }
        let upper = |n: &str| n.chars().all(|c| c.is_ascii_uppercase() || c == '_');
        unique("INK_TOKENS", schema::INK_TOKENS.iter().map(|(n, _)| *n).collect());
        unique("SIZE_RUNGS", schema::SIZE_RUNGS.iter().map(|(n, _)| *n).collect());
        unique("SPACE_TOKENS", schema::SPACE_TOKENS.iter().map(|(n, _)| *n).collect());
        unique("BUILTIN_ICONS", schema::BUILTIN_ICONS.iter().map(|(n, _)| *n).collect());
        unique("COMMANDS", schema::COMMANDS.iter().map(|c| c.name).collect());
        unique("CONDITIONS", schema::CONDITIONS.iter().map(|c| c.name).collect());
        unique("BINDINGS", schema::BINDINGS.iter().map(|b| b.name).collect());
        assert!(schema::INK_TOKENS.iter().all(|(n, _)| upper(n)), "tokens are UPPER_CASE");
        assert!(schema::SIZE_RUNGS.iter().all(|(n, _)| upper(n)));
        assert!(schema::SPACE_TOKENS.iter().all(|(n, _)| upper(n)));
        assert!(schema::BUILTIN_ICONS
            .iter()
            .all(|(n, _)| n.chars().all(|c| c.is_ascii_lowercase() || c == '-')));
        assert!(schema::COMMANDS
            .iter()
            .all(|c| c.name.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_')));

        // The body sizes are the rungs but for HUD_TITLE.
        let rungs: Vec<&str> = schema::SIZE_RUNGS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| *n != "HUD_TITLE")
            .collect();
        assert_eq!(schema::BODY_SIZES, rungs.as_slice());

        // Every name a key may bind is a binding of the list, and the conditions that are bindings agree.
        let bindings: Vec<&str> = schema::BINDINGS.iter().map(|b| b.name).collect();
        for list in [
            schema::TEXT_BINDS,
            schema::IMAGE_BINDS,
            schema::STRIP_BINDS,
            schema::FACTS_BINDS,
            schema::CARD_ART_BINDS,
        ] {
            for name in list {
                assert!(bindings.contains(name), "`{name}` is bindable but not a binding");
            }
        }
        for c in schema::CONDITIONS {
            if schema::is_card_scoped(c.name) {
                assert!(bindings.contains(&c.name), "the card condition `{}` is a binding", c.name);
            }
        }
    }

    /// The keys a rule names, in the node or command it belongs to.
    fn rule_keys(rule: &Rule) -> Vec<&'static str> {
        match *rule {
            Rule::Requires { key, other } => vec![key, other],
            Rule::Excludes { a, b } => vec![a, b],
            Rule::IfValue { key, require, forbid, .. } | Rule::IfNotValue { key, require, forbid, .. } => {
                let mut out = vec![key];
                out.extend_from_slice(require);
                out.extend_from_slice(forbid);
                out
            }
            Rule::ForbidPair { a, b, .. } => vec![a, b],
            Rule::Disjoint { a, b } => vec![a, b],
            Rule::NonZero(key) => vec![key],
            Rule::NameOnlyWith { key, other, .. } => vec![key, other],
            Rule::AtMost { key, other } => vec![key, other],
            Rule::FaceMapOnlyWhen { param, .. } => vec![param],
        }
    }

    #[test]
    fn every_rule_names_keys_of_its_own_node_or_command() {
        let mut rules = 0usize;
        for node in schema::all_nodes() {
            for rule in node.rules {
                for key in rule_keys(rule) {
                    assert!(schema::field(node, key).is_some(), "{}: rule {rule:?} names `{key}`", node.name);
                }
                rules += 1;
            }
        }
        for cmd in schema::COMMANDS {
            for rule in cmd.rules {
                for key in rule_keys(rule) {
                    assert!(
                        cmd.param(key).is_some() || schema::field(item_node(), key).is_some(),
                        "{}: rule {rule:?} names `{key}`",
                        cmd.name
                    );
                }
                rules += 1;
            }
        }
        assert!(rules >= 15, "{rules} rules");
    }

    #[test]
    fn nothing_in_the_table_is_newer_than_the_current_format() {
        // `E0107` (a key of a later minor) is unreachable at 1.0, the first and only minor.
        let current = schema::CURRENT;
        for node in schema::all_nodes() {
            for f in node.fields {
                assert!(f.since <= current, "{}.{}", node.name, f.key);
            }
        }
        for cmd in schema::COMMANDS {
            assert!(cmd.since <= current, "{}", cmd.name);
            for p in cmd.params {
                assert!(p.since <= current, "{}:{}", cmd.name, p.key);
            }
        }
        for c in schema::CONDITIONS {
            assert!(c.since <= current, "{}", c.name);
        }
        for b in schema::BINDINGS {
            assert!(b.since <= current, "{}", b.name);
        }
        assert!(schema::SUPPORTED.contains(&current));
    }

    #[test]
    fn the_drift_test_reaches_every_entry_of_the_table() {
        // The counting assertion: the entries the table has, the entries the walks above exercise.
        let keys = table_entries().len();
        let params = table_params().len();
        let fields: usize = schema::all_nodes().iter().map(|n| n.fields.len()).sum();
        let command_params: usize = schema::COMMANDS.iter().map(|c| c.params.len()).sum();
        assert_eq!(keys, fields);
        assert_eq!(params, command_params);
        assert!(keys >= 120, "the table lists {keys} node keys");
        assert_eq!(params, 5);
    }
}

// ---------------------------------------------------------------------------------------------------
// The reference example against the table
// ---------------------------------------------------------------------------------------------------

mod reference_keys {
    use super::*;
    use std::collections::BTreeSet;

    /// The `node` names of the objects the walker builds that are not nodes of the table.
    const NON_TABLE: &[&str] = &["face_map", "face_ladder", "palette", "assets"];

    fn visit_val(value: &Val, seen: &mut BTreeSet<String>) {
        match value {
            Val::Object(object) => visit_object(object, seen),
            Val::List(items) => items.iter().for_each(|item| visit_val(item, seen)),
            Val::Keyed(keyed) => {
                visit_val(&keyed.base, seen);
                keyed.cases.iter().for_each(|(_, case)| visit_val(case, seen));
            }
            Val::Bool(_)
            | Val::Int(_)
            | Val::Num(_)
            | Val::Text(_)
            | Val::Name(_)
            | Val::Token(_)
            | Val::Ref(_)
            | Val::Cond(_) => {}
        }
    }

    fn visit_object(object: &Object, seen: &mut BTreeSet<String>) {
        assert!(well_formed_pointer(&object.pointer), "{:?}", object.pointer);
        let Some(node) = schema::node(object.node) else {
            assert!(NON_TABLE.contains(&object.node), "`{}` is not a node of the table", object.node);
            object.fields.iter().for_each(|(_, value)| visit_val(value, seen));
            return;
        };
        seen.insert(node.name.to_owned());
        let cmd = match node.kind {
            schema::NodeKind::CommandDispatched => object
                .get("command")
                .and_then(Val::as_str)
                .and_then(schema::command),
            schema::NodeKind::Plain | schema::NodeKind::OneOf(_) => None,
        };
        for (key, value) in &object.fields {
            let listed = schema::field(node, key).is_some() || cmd.is_some_and(|c| c.param(key).is_some());
            assert!(listed, "{}: the typed tree carries `{key}`, which the table does not list", object.pointer);
            visit_val(value, seen);
        }
    }

    #[test]
    fn the_typed_tree_of_the_reference_holds_only_what_the_table_lists() {
        let package = parse_checked(REFERENCE.as_bytes()).package.expect("a package");
        let mut seen = BTreeSet::new();
        visit_object(&package.layout, &mut seen);
        assert!(seen.contains("layout") && seen.contains("band") && seen.contains("cell"), "{seen:?}");
        // The worked example is the spec's tour of the format (the package root and the app range sit
        // above `layout`).
        // Three nodes stay unused by it: `rect`, `grab` and `actions`, which the fragment tests cover.
        let missing: Vec<&str> = schema::all_nodes()
            .iter()
            .map(|n| n.name)
            .filter(|n| !seen.contains(*n) && !matches!(*n, "package" | "app"))
            .collect();
        assert!(
            missing.iter().all(|n| matches!(*n, "rect" | "grab" | "actions")),
            "the reference leaves these nodes unused: {missing:?}"
        );
        assert!(seen.len() >= 18, "{seen:?}");
    }

    #[test]
    fn the_reference_and_the_minimal_package_name_their_commands_from_the_table() {
        for text in [REFERENCE, MINIMAL] {
            let package = parse_checked(text.as_bytes()).package.expect("a package");
            let mut seen = BTreeSet::new();
            visit_object(&package.layout, &mut seen);
            assert!(seen.contains("item"), "{seen:?}");
        }
    }
}
