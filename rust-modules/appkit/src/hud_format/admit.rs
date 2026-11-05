//! **The verdict on a package's bytes**, as the app's installer and list ask for it
//! (`docs/hud-plugins.md`, "Versioning and mismatch behaviour"): does the file parse, does it pass the
//! reachability and package-level invariants, and can THIS app run it. Pure: bytes and the app's version
//! in, a value out. The geometric checks that need a measurer and a language are not here: they are the
//! lint's, per language (`lint`), and a package that fails one runs as `builtin:classic` in that language only.
//!
//! A file refused by the version gate carries no parsed package, so its facts (`id`, `name`, `version`,
//! `format`) are read from the strict JSON tree directly: enough to list it as "Made for a newer
//! PlxNative" or "Uses an older HUD format" and to keep it on disk. The id is checked against the same
//! reverse-DNS pattern a parsed package's is, so a hostile file cannot put anything else in the list.

use plx_ui::rec::strict_json::{decode_ordered, Ordered};

use super::bundle::{self, MAX_ARTIFACT_BYTES};
use plx_data::hudpacks::{valid_package_id, Semver};
use super::{codes, REASON_NEWER};

/// The longest `name` the list keeps, in characters.
const NAME_CHARS: usize = 64;
/// The decoded-memory budget of the gate's lenient read: the same bound `parse` reads under.
const DECODE_BYTES: usize = 16 * 1024 * 1024;

/// What a package says about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `MAJOR.MINOR`.
    pub package_format: String,
    pub hud_format: String,
}

/// Why a package that parsed cannot run on this app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cannot {
    /// An unsupported major above the app's, or a minor above its highest.
    NewerFormat,
    /// A major this app no longer compiles.
    OlderFormat,
    /// Outside the package's own `app.min` / `app.max`.
    AppRange,
}

/// The verdict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Parses, passes the invariants and runs on this app.
    Runs(Facts),
    /// Parses (or is gated by version), but this app cannot run it.
    Cannot(Facts, Cannot),
    /// Does not parse, breaks an invariant or is not a package: nothing about it, not even its id, is trusted.
    Invalid,
}

/// Judge `bytes` for the app release `app_version`.
pub fn admit(bytes: &[u8], app_version: &str) -> Admission {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Admission::Invalid;
    }
    let decoded = bundle::read(bytes, None);
    let Some(package) = decoded.package else {
        return match decoded.findings.as_slice() {
            [gate] if gate.code == codes::E0402.id || gate.code == codes::E0403.id => match gated_facts(bytes) {
                Some(facts) => {
                    let why = if gate.message == REASON_NEWER { Cannot::NewerFormat } else { Cannot::OlderFormat };
                    Admission::Cannot(facts, why)
                }
                None => Admission::Invalid,
            },
            _ => Admission::Invalid,
        };
    };
    let manifest = package.manifest();
    let facts = Facts {
        id: manifest.id.clone(),
        name: clean_name(&manifest.name),
        version: manifest.version.clone(),
        package_format: format!("{}.{}", manifest.package_format.0, manifest.package_format.1),
        hud_format: format!("{}.{}", manifest.hud_format.0, manifest.hud_format.1),
    };
    let in_range = manifest.app.as_ref().is_none_or(|range| {
        let app = Semver::app_release(app_version);
        let below = matches!((app, range.min.as_deref().and_then(Semver::parse)), (Some(app), Some(min)) if app < min);
        let above = matches!((app, range.max.as_deref().and_then(Semver::parse)), (Some(app), Some(max)) if app > max);
        !(below || above)
    });
    if in_range {
        Admission::Runs(facts)
    } else {
        Admission::Cannot(facts, Cannot::AppRange)
    }
}

fn clean_name(name: &str) -> String {
    name.chars().filter(|c| !c.is_control()).take(NAME_CHARS).collect()
}

/// The four facts of a package the version gate turned away, read from its tree; `None` when any is
/// missing, is not a string, or the id is not a reverse-DNS id.
fn gated_facts(bytes: &[u8]) -> Option<Facts> {
    let files = bundle::archive::read(bytes).ok()?;
    let manifest = files.iter().find(|file| file.path == bundle::manifest::FILE)?;
    let root = decode_ordered(&manifest.bytes, DECODE_BYTES).ok()?;
    let text = |key: &str| match root.get(key)? {
        Ordered::String(s) => Some(s.clone()),
        _ => None,
    };
    let id = text("id").filter(|id| valid_package_id(id))?;
    Some(Facts { id, name: clean_name(&text("name")?), version: text("version")?,
        package_format: text("package_format")?, hud_format: root.get("hud")?.get("format")?.as_str()?.to_owned() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud_format::{CLASSIC_PACKAGE, fixtures};

    const REFERENCE: &str = include_str!("fixtures/reference/hud.json");
    const MINIMAL: &str = include_str!("fixtures/minimal/manifest.json");

    #[test]
    fn the_embedded_classic_runs_and_says_who_it_is() {
        let Admission::Runs(facts) = admit(CLASSIC_PACKAGE, "0.2.1") else { panic!("classic runs") };
        assert_eq!(facts.id, "builtin:classic");
        assert_eq!(facts.hud_format, "1.0");
    }

    #[test]
    fn a_fixture_package_runs() {
        assert!(matches!(admit(&fixtures::bytes("reference"), "0.2.1"), Admission::Runs(_)));
        assert!(matches!(admit(&fixtures::bytes("minimal"), "0.2.1"), Admission::Runs(_)));
    }

    #[test]
    fn what_is_not_a_package_is_invalid() {
        for bytes in [&b""[..], b"{", b"[]", b"{}", b"\xff\xfe"] {
            assert_eq!(admit(bytes, "0.2.1"), Admission::Invalid, "{bytes:?}");
        }
        let huge = vec![b' '; MAX_ARTIFACT_BYTES + 1];
        assert_eq!(admit(&huge, "0.2.1"), Admission::Invalid);
    }

    #[test]
    fn a_newer_or_older_format_is_unloadable_and_keeps_its_facts() {
        let with = |format: &str| MINIMAL.replacen("\"format\": \"1.0\"", &format!("\"format\": \"{format}\""), 1);
        assert_ne!(with("9.0"), MINIMAL, "the fixture spells its format as the test expects");
        for (format, why) in [("9.0", Cannot::NewerFormat), ("1.99", Cannot::NewerFormat), ("0.5", Cannot::OlderFormat)] {
            match admit(&fixtures::with_manifest("minimal", &with(format)), "0.2.1") {
                Admission::Cannot(facts, got) => {
                    assert_eq!(got, why, "{format}");
                    assert_eq!(facts.hud_format, format);
                    assert!(valid_package_id(&facts.id));
                }
                other => panic!("{format}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_gated_file_with_a_hostile_id_is_invalid() {
        let hostile = MINIMAL.replacen("\"format\": \"1.0\"", "\"format\": \"9.0\"", 1);
        let id = hostile.split("\"id\": \"").nth(1).and_then(|r| r.split('"').next()).expect("an id").to_owned();
        let bad = hostile.replacen(&format!("\"id\": \"{id}\""), "\"id\": \"../../etc\"", 1);
        assert_eq!(admit(&fixtures::with_manifest("minimal", &bad), "0.2.1"), Admission::Invalid);
    }

    #[test]
    fn an_app_range_is_judged_against_the_leading_release_triple() {
        let with = |range: &str| MINIMAL.replacen("\"package_format\": \"1.0\",", &format!("\"package_format\": \"1.0\", \"app\": {range},"), 1);
        assert_ne!(with("{}"), MINIMAL, "the fixture has a place for `app`");
        let case = |range: &str, app: &str| admit(&fixtures::with_manifest("minimal", &with(range)), app);
        assert!(matches!(case(r#"{"min": "0.3.0"}"#, "0.2.9"), Admission::Cannot(_, Cannot::AppRange)));
        assert!(matches!(case(r#"{"min": "0.3.0"}"#, "0.3.0-nightly.4"), Admission::Runs(_)));
        assert!(matches!(case(r#"{"max": "0.2.0"}"#, "0.2.1"), Admission::Cannot(_, Cannot::AppRange)));
        assert!(matches!(case(r#"{"min": "0.1.0", "max": "0.9.0"}"#, "0.2.1"), Admission::Runs(_)));
        assert!(matches!(case(r#"{"min": "0.3.0"}"#, "dev"), Admission::Runs(_)), "an unreadable app release is not a reason to refuse");
    }
}
