//! **Tests of what `site/hud/` publishes**: the emitted JSON Schema as a golden, the seven valid examples
//! (byte-identical to the fixtures the parser's own tests use, so the two copies cannot drift), and the
//! invalid examples with the lint report [`super::report`] prints for each, also goldens.
//!
//! Goldens are compared, never silently rewritten. After a deliberate change, regenerate them (the schema, the
//! examples' copies and every `invalid/*/lint.json`) with
//!
//! ```text
//! PLX_HUD_SCHEMA_WRITE=1 PATH="$HOME/.cargo/bin:$PATH" make test-crate C=plx_appkit T=hud_format
//! ```
//!
//! and commit what changed. The invalid examples' own `source directories` are authored, not generated.
//!
//! `pages.yml` stages this directory at `/hud/`, preserving the schema and examples' bytes.
//! `ci.yml` includes `site/hud/**` in its gate inputs, so an authoring-file-only PR runs these
//! goldens too. Registry CI additionally compares the app's emitted schema with its mirror and
//! checks the pinned corpus through stock Draft 2020-12 validation and the actual compiler.

use std::fs;
use std::path::PathBuf;

use plx_ui::rec::strict_json::Value;

use super::schema_check::{accepts, check as schema_check, is_prefix};
use super::{bundle, check, fixtures, report, schema, schema_emit};
use plx_ui::rec::strict_json::Budget;

/// The environment variable that turns a golden comparison into a rewrite.
const BLESS: &str = "PLX_HUD_SCHEMA_WRITE";

const EXAMPLES: [(&str, &str); 7] = [
    ("classic", include_str!("fixtures/classic/hud.json")),
    ("reference", include_str!("fixtures/reference/hud.json")),
    ("minimal", include_str!("fixtures/minimal/hud.json")),
    (
        "hero-center",
        include_str!("fixtures/hero-center/hud.json"),
    ),
    ("tabs", include_str!("fixtures/tabs/hud.json")),
    ("palette", include_str!("fixtures/palette/hud.json")),
    ("rising-tabs", include_str!("fixtures/rising-tabs/hud.json")),
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
    golden("schema/hud/1.0.json", &schema_emit::emit_text(schema::CURRENT));
    golden("schema/manifest/1.0.json", &format!("{:#}\n", schema_emit::emit_manifest(bundle::CURRENT)));
}

#[test]
fn the_golden_check_fails_on_a_one_character_change_and_passes_on_the_file() {
    let emitted = schema_emit::emit_text(schema::CURRENT);
    assert_eq!(first_difference("x", &emitted, &emitted), None);
    let committed = if blessing() {
        emitted.clone()
    } else {
        fs::read_to_string(site("schema/hud/1.0.json")).expect("the committed schema")
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
        let found = first_difference("site/hud/schema/hud/1.0.json", &emitted, &text)
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
    if blessing() { return; }
    for kind in ["manifest", "hud"] {
        let text = fs::read_to_string(site(&format!("schema/{kind}/1.0.json"))).expect("published schema");
        let schema: Value = text.parse().expect("schema JSON");
        assert_eq!(schema["$schema"], "https://json-schema.org/draft/2020-12/schema");
        assert_eq!(schema["$id"], format!("https://plxnative.com/hud/schema/{kind}/1.0.json"));
        assert!(text.ends_with("}\n"), "one trailing newline");
    }
}

// ---------------------------------------------------------------------------------------------------
// The valid examples
// ---------------------------------------------------------------------------------------------------

#[test]
fn the_published_examples_are_byte_identical_to_the_fixtures() {
    for (name, _) in EXAMPLES {
        let files = fixtures::files(name);
        for file in files {
            let path = site(&format!("examples/{name}/{}", file.path));
            assert_eq!(fs::read(&path).expect("source member"), file.bytes.as_ref(), "{}", path.display());
        }
        assert_eq!(fs::read(site(&format!("examples/{name}.plxhud"))).expect("published artifact"), fixtures::bytes(name), "{name}");
    }
    let published = names_in("examples", ".plxhud");
    let mut want: Vec<String> = EXAMPLES.iter().map(|(name, _)| (*name).to_owned()).collect();
    want.sort();
    assert_eq!(published, want);
}

#[test]
fn every_example_parses_with_no_finding_and_the_schemas_accept_it() {
    for (name, _) in EXAMPLES {
        let decoded = bundle::read(&fixtures::bytes(name), None);
        assert!(decoded.package.is_some() && decoded.findings.is_empty(), "{name}: {:?}", decoded.findings);
        for file in fixtures::files(name).into_iter().filter(|file| matches!(file.path.as_str(), "manifest.json" | "hud.json")) {
            let schema = if file.path == "manifest.json" { schema_emit::emit_manifest(bundle::CURRENT) } else { schema_emit::emit_current() };
            let value: Value = std::str::from_utf8(&file.bytes).unwrap().parse().unwrap();
            assert!(schema_check(&schema, &value).is_empty(), "{name}/{}", file.path);
        }
    }
}

#[test]
fn every_example_passes_the_package_checker_with_no_finding() {
    for (name, _) in EXAMPLES {
        let decoded = bundle::read(&fixtures::bytes(name), None);
        let checked = check(decoded.package.as_ref().expect("verified fixture").hud());
        assert!(checked.findings.is_empty(), "{name}: {:?}", checked.findings);
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
    assert!(has("rising-tabs", r#""sections": "modal""#) && has("rising-tabs", r#""widget": "pill""#));
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

fn invalid_source(name: &str) -> bundle::Decoded {
    let directory = site(&format!("examples/invalid/{name}"));
    let bytes = fs::read(directory.join("manifest.json")).expect("authored manifest");
    let manifest = match bundle::manifest::read(&bytes, &Budget::new(bundle::JSON_DECODE_BYTES)) {
        Ok(manifest) => manifest,
        Err(findings) => return bundle::Decoded::source_error(None, findings),
    };
    let files: Vec<_> = manifest.files().map(|path| {
        bundle::archive::File::new(path, fs::read(directory.join(path)).expect("declared source member"))
    }).collect();
    match bundle::pack(&files) {
        Ok((_, decoded)) | Err(decoded) => decoded,
    }
}

#[test]
fn the_invalid_examples_are_the_ones_this_test_knows() {
    let mut published: Vec<_> = fs::read_dir(site("examples/invalid")).unwrap().map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().unwrap().is_dir())
        .map(|entry| entry.file_name().into_string().unwrap()).collect();
    published.sort();
    let mut want: Vec<_> = INVALID.iter().map(|(name, _, _)| name.to_string()).collect();
    want.sort();
    assert_eq!(published, want);
    for name in published {
        assert!(site(&format!("examples/invalid/{name}/lint.json")).is_file());
    }
}

#[test]
fn every_invalid_example_produces_its_code_and_is_never_a_package() {
    for (name, code, _) in INVALID {
        let decoded = invalid_source(name);
        assert!(decoded.package.is_none(), "{name}");
        assert!(decoded.findings.iter().any(|finding| finding.code == code), "{name}: {:?}", decoded.findings);
    }
}

#[test]
fn every_invalid_example_has_the_lint_report_the_report_module_prints() {
    for (name, _, _) in INVALID {
        let decoded = invalid_source(name);
        let file = format!("examples/invalid/{name}");
        let value = report::with_digest(report::to_json_with_costs(&file, &decoded.findings, &[]), decoded.sha256);
        golden(&format!("{file}/lint.json"), &format!("{value:#}\n"));
        assert_eq!(value["protocol"], 1);
        assert_eq!(value["file"], file);
    }
}

#[test]
fn the_schema_rejects_the_key_type_and_value_faults_among_them_where_the_parser_agrees() {
    for (name, _, rejected) in INVALID {
        let decoded = invalid_source(name);
        let mut any = false;
        for (member, schema) in [
            ("manifest.json", schema_emit::emit_manifest(bundle::CURRENT)),
            ("hud.json", schema_emit::emit_current()),
        ] {
            let text = fs::read_to_string(site(&format!("examples/invalid/{name}/{member}"))).unwrap();
            let instance: Value = text.parse().expect("invalid example is still JSON");
            let failures = schema_check(&schema, &instance);
            any |= !failures.is_empty();
            assert_eq!(accepts(&schema, &instance), failures.is_empty());
            for failure in failures {
                assert!(decoded.findings.iter().any(|finding| finding.file.as_deref() == Some(member)
                    && is_prefix(&failure.pointer, &finding.pointer)), "{name}/{member}: {:?}", failure);
            }
        }
        assert_eq!(any, rejected, "{name}");
    }
}
