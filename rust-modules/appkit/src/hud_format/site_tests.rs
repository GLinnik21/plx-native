//! **Tests of what `site/hud/` publishes**: the emitted JSON Schema as a golden, the five valid examples
//! (byte-identical to the fixtures the parser's own tests use, so the two copies cannot drift), and the
//! invalid examples with the lint report [`super::report`] prints for each, also goldens.
//!
//! Goldens are compared, never silently rewritten. After a deliberate change, regenerate them (the schema, the
//! examples' copies and every `invalid/*.lint.json`) with
//!
//! ```text
//! PLX_HUD_SCHEMA_WRITE=1 PATH="$HOME/.cargo/bin:$PATH" make test-crate C=plx_appkit T=hud_format
//! ```
//!
//! and commit what changed. The invalid examples' own `*.plxhud.json` are authored, not generated.
//!
//! The site is not deployed by this change: `pages.yml` copies named files, and publishing `site/hud/` is a
//! later PR. These files are committed so that the golden and the examples are held by CI whenever a PR
//! touches Rust. KNOWN GAP: `ci.yml`'s path filter still excludes `site/**`, so a PR that edits only
//! `site/hud/` does not run these tests; the PR that publishes `site/hud/` adds `site/hud/**` to `ci_paths`.

use std::fs;
use std::path::PathBuf;

use plx_ui::rec::strict_json::Value;

use super::schema_check::{accepts, check as schema_check, is_prefix};
use super::{check, parse, report, schema, schema_emit};

/// The environment variable that turns a golden comparison into a rewrite.
const BLESS: &str = "PLX_HUD_SCHEMA_WRITE";

const EXAMPLES: [(&str, &str); 5] = [
    ("reference", include_str!("fixtures/reference.plxhud.json")),
    ("minimal", include_str!("fixtures/minimal.plxhud.json")),
    (
        "hero-center",
        include_str!("fixtures/hero-center.plxhud.json"),
    ),
    ("tabs", include_str!("fixtures/tabs.plxhud.json")),
    ("palette", include_str!("fixtures/palette.plxhud.json")),
];

fn blessing() -> bool {
    std::env::var_os(BLESS).is_some_and(|v| v == "1")
}

/// `site/hud/<rel>` of this checkout.
fn site(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../site/hud")
        .join(rel)
}

/// `None` when the texts are one, else the first line they differ at, both lines, and the command that
/// regenerates the golden.
fn first_difference(what: &str, expected: &str, actual: &str) -> Option<String> {
    if expected == actual {
        return None;
    }
    let (mut want, mut got) = (expected.lines(), actual.lines());
    let mut line = 1;
    loop {
        match (want.next(), got.next()) {
            (Some(a), Some(b)) if a == b => line += 1,
            (a, b) => {
                return Some(format!(
                    "{what} is not what the table emits.\n  line {line}, regenerated: {}\n  line {line}, committed:   {}\n  \
                     if the change is deliberate, rewrite the golden with `{BLESS}=1 make test-crate C=plx_appkit T=hud_format` and commit it",
                    a.unwrap_or("<end of text>"),
                    b.unwrap_or("<end of text>"),
                ));
            }
        }
    }
}

/// Compare `site/hud/<rel>` with `expected`; under [`BLESS`] write it instead.
fn golden(rel: &str, expected: &str) {
    let path = site(rel);
    if blessing() {
        fs::create_dir_all(path.parent().expect("a parent directory"))
            .expect("create the site directory");
        fs::write(&path, expected).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        return;
    }
    let committed = fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{}: {e}; if the file is new, write it with `{BLESS}=1 make test-crate C=plx_appkit T=hud_format`",
            path.display()
        )
    });
    if let Some(difference) = first_difference(&format!("site/hud/{rel}"), expected, &committed) {
        panic!("{difference}");
    }
}

fn names_in(rel: &str, suffix: &str) -> Vec<String> {
    let dir = site(rel);
    let mut out: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .filter_map(|entry| {
            let name = entry
                .expect("a directory entry")
                .file_name()
                .into_string()
                .expect("a UTF-8 name");
            name.strip_suffix(suffix).map(str::to_owned)
        })
        .collect();
    out.sort();
    out
}

// ---------------------------------------------------------------------------------------------------
// The schema
// ---------------------------------------------------------------------------------------------------

#[test]
fn the_published_schema_is_the_one_the_table_emits() {
    golden("schema/1.0.json", &schema_emit::emit_text(schema::CURRENT));
}

#[test]
fn the_golden_check_fails_on_a_one_character_change_and_passes_on_the_file() {
    let emitted = schema_emit::emit_text(schema::CURRENT);
    assert_eq!(first_difference("x", &emitted, &emitted), None);
    let committed = if blessing() {
        emitted.clone()
    } else {
        fs::read_to_string(site("schema/1.0.json")).expect("the committed schema")
    };
    assert_eq!(
        first_difference("x", &emitted, &committed),
        None,
        "the committed file passes"
    );
    let mut changed = 0;
    for (at, c) in committed
        .char_indices()
        .filter(|(_, c)| c.is_ascii_alphanumeric())
        .step_by(97)
    {
        let mut text = committed.clone();
        let other = if c == 'x' { "y" } else { "x" };
        text.replace_range(at..at + 1, other);
        let found = first_difference("site/hud/schema/1.0.json", &emitted, &text)
            .unwrap_or_else(|| panic!("a change at byte {at} went unnoticed"));
        assert!(
            found.contains("line ") && found.contains(BLESS),
            "a readable message: {found}"
        );
        changed += 1;
    }
    assert!(
        changed > 50,
        "{changed} single-character changes were tried"
    );
    let mut shorter = committed.clone();
    shorter.pop();
    assert!(
        first_difference("x", &emitted, &shorter).is_some(),
        "a missing last character"
    );
    assert!(
        first_difference("x", &emitted, &format!("{committed} ")).is_some(),
        "an extra character"
    );
}

#[test]
fn the_published_schema_describes_itself_as_draft_2020_12_at_its_own_address() {
    let text = fs::read_to_string(site("schema/1.0.json")).unwrap_or_default();
    if blessing() {
        return;
    }
    let schema: Value = text.parse().expect("the committed schema is JSON");
    assert_eq!(
        schema["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(schema["$id"], "https://plxnative.com/hud/schema/1.0.json");
    assert!(text.ends_with("}\n"), "one trailing newline");
}

// ---------------------------------------------------------------------------------------------------
// The valid examples
// ---------------------------------------------------------------------------------------------------

#[test]
fn the_published_examples_are_byte_identical_to_the_fixtures() {
    for (name, fixture) in EXAMPLES {
        golden(&format!("examples/{name}.plxhud.json"), fixture);
    }
    let published = names_in("examples", ".plxhud.json");
    let mut want: Vec<String> = EXAMPLES.iter().map(|(n, _)| (*n).to_owned()).collect();
    want.sort();
    assert_eq!(
        published, want,
        "site/hud/examples holds exactly the examples (classic is exported from the binary once it embeds it, not copied)"
    );
}

#[test]
fn every_example_parses_with_no_finding_and_the_schema_accepts_it() {
    let schema = schema_emit::emit_current();
    for (name, text) in EXAMPLES {
        let parsed = parse::parse(text.as_bytes());
        assert!(
            parsed.findings.is_empty() && parsed.package.is_some(),
            "{name}: {:#?}",
            parsed.findings
        );
        let value: Value = text.parse().expect("an example is JSON");
        let found = schema_check(&schema, &value);
        assert!(found.is_empty(), "{name}: {found:#?}");
    }
}

#[test]
fn every_example_passes_the_package_checker_with_no_finding() {
    for (name, text) in EXAMPLES {
        let package = parse::parse(text.as_bytes())
            .package
            .unwrap_or_else(|| panic!("{name} parses"));
        let checked = check(&package);
        assert!(
            checked.findings.is_empty(),
            "{name}: R1 to R4 and the invariants: {:#?}",
            checked.findings
        );
    }
}

#[test]
fn the_examples_cover_what_the_plan_names() {
    let has = |name: &str, needle: &str| {
        let (_, text) = EXAMPLES
            .iter()
            .find(|(n, _)| *n == name)
            .expect("an example");
        text.contains(needle)
    };
    assert!(has("hero-center", r#""command": "toggle_play""#));
    assert!(has("tabs", r#""command": "toggle_section""#) && has("tabs", r#""widget": "pill""#));
    assert!(has("palette", r#""palette": {"#));
    assert!(!has("minimal", "palette") && !has("minimal", "assets"));
}

// ---------------------------------------------------------------------------------------------------
// The invalid examples and their lint reports
// ---------------------------------------------------------------------------------------------------

/// What each invalid example shows: the code it must produce, and whether the schema rejects it. The schema
/// rejects a key, type or value fault; a duplicate key, an SVG, and the version gate are the parser's.
const INVALID: [(&str, &str, bool); 10] = [
    ("unknown-key", "HUD-E0102", true),
    ("duplicate-key", "HUD-E0101", false),
    ("unknown-command", "HUD-E0301", true),
    ("bad-command-parameter", "HUD-E0304", true),
    ("bad-when", "HUD-E0501", true),
    ("bad-palette-value", "HUD-E0506", true),
    ("svg-gradient", "HUD-E0701", false),
    ("missing-key", "HUD-E0103", true),
    ("wrong-type", "HUD-E0104", true),
    ("newer-format", "HUD-E0402", false),
];

fn invalid_text(name: &str) -> String {
    let path = site(&format!("examples/invalid/{name}.plxhud.json"));
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn the_invalid_examples_are_the_ones_this_test_knows() {
    let published = names_in("examples/invalid", ".plxhud.json");
    let mut want: Vec<String> = INVALID.iter().map(|(n, _, _)| (*n).to_owned()).collect();
    want.sort();
    assert_eq!(
        published, want,
        "an invalid example is listed in INVALID with the code it shows"
    );
    assert!(published.len() >= 7, "one per parse rule family at least");
    if !blessing() {
        assert_eq!(
            names_in("examples/invalid", ".lint.json"),
            want,
            "each example has its lint report, and no report is orphaned"
        );
    }
}

#[test]
fn every_invalid_example_produces_its_code_and_is_never_a_package() {
    for (name, code, _) in INVALID {
        let parsed = parse::parse(invalid_text(name).as_bytes());
        assert!(parsed.package.is_none(), "{name} must not parse");
        assert!(
            parsed.findings.iter().any(|f| f.code == code),
            "{name}: expected {code}, found {:#?}",
            parsed
                .findings
                .iter()
                .map(|f| format!("{} {}", f.code, f.pointer))
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn every_invalid_example_has_the_lint_report_the_report_module_prints() {
    for (name, _, _) in INVALID {
        let file = format!("{name}.plxhud.json");
        let findings = parse::parse(invalid_text(name).as_bytes()).findings;
        let json = report::json_text(&file, &findings);
        golden(&format!("examples/invalid/{name}.lint.json"), &json);
        if !blessing() {
            let read: Value = json.parse().expect("the report is JSON");
            assert_eq!(read["lint"], 1);
            assert_eq!(read["file"], file.as_str());
            assert_eq!(
                read["findings"].as_array().map(Vec::len),
                Some(findings.len()),
                "{name}"
            );
        }
    }
}

#[test]
fn the_schema_rejects_the_key_type_and_value_faults_among_them_where_the_parser_agrees() {
    let schema = schema_emit::emit_current();
    for (name, _, rejected) in INVALID {
        let text = invalid_text(name);
        let instance: Value = text.parse().expect("an invalid example is still JSON");
        let failures = schema_check(&schema, &instance);
        assert_eq!(!failures.is_empty(), rejected, "{name}: {failures:#?}");
        assert_eq!(accepts(&schema, &instance), !rejected, "{name}");
        let parser = parse::parse(text.as_bytes()).findings;
        for failure in &failures {
            assert!(
                parser
                    .iter()
                    .any(|f| is_prefix(&failure.pointer, &f.pointer)),
                "{name}: the schema fails at {:?} and the parser names nothing under it",
                failure.pointer
            );
        }
    }
}
