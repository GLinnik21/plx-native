#![no_main]
//! `hud_format::svg_subset::check_asset`: the gate in front of nanosvg (C). Whatever bytes arrive, it
//! answers `Ok` or a non-empty list of `HUD-E07xx` findings at the pointer it was given, and what it
//! admits is inside the policy. The assertions are the in-crate seeded mutation test's
//! (`svg_subset.rs`, `seeded_mutations_never_panic_and_never_admit_what_the_policy_forbids`), run over
//! every input the fuzzer finds instead of 2000 fixed ones:
//! at most `MAX_PATHS_PER_ASSET` paths, a finite `viewBox` of positive size, and, comments and the leading
//! `<?xml ... ?>` declaration (which the subset skips) aside, none of the constructs the subset forbids (entities, a doctype, references, scripts, styles, text, images,
//! strokes, opacity, a second declaration, a scheme other than the SVG namespace).
use libfuzzer_sys::fuzz_target;
use plx_appkit::hud_format::codes;
use plx_appkit::hud_format::svg_subset::{check_asset, MAX_ASSET_BYTES, MAX_PATHS_PER_ASSET};

const POINTER: &str = "/assets/mark";
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

/// The text with its comments removed (the one place `&` and the like are allowed).
fn without_comments(svg: &str) -> String {
    let mut out = String::new();
    let mut rest = svg;
    while let Some(i) = rest.find("<!--") {
        out.push_str(&rest[..i]);
        match rest[i + 4..].find("-->") {
            Some(j) => rest = &rest[i + 4 + j + 3..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// The text without its leading `<?xml ... ?>` declaration, which the subset skips (only `&` and `<` are
/// refused inside it, `Parser::declaration`): it sits after an optional BOM and whitespace, needs
/// whitespace after `<?xml`, and ends at the first `?>`. Anything the scan below forbids is still
/// forbidden everywhere else, but a word inside the declaration is not the parser's to refuse.
fn without_declaration(svg: &str) -> &str {
    let start = svg.strip_prefix('\u{feff}').unwrap_or(svg);
    let start = start.trim_start_matches([' ', '\t', '\r', '\n']);
    let Some(after) = start.strip_prefix("<?xml") else {
        return svg;
    };
    if !after.starts_with(|c: char| c.is_ascii_whitespace()) {
        return svg;
    }
    after.find("?>").map_or(svg, |close| &after[close + 2..])
}

fuzz_target!(|data: &[u8]| {
    let svg = String::from_utf8_lossy(data);
    match check_asset(POINTER, &svg) {
        Ok(info) => {
            assert!(svg.len() <= MAX_ASSET_BYTES);
            assert!(info.paths >= 1 && info.paths <= MAX_PATHS_PER_ASSET, "{svg:?}");
            assert!(info.view_box.iter().all(|v| v.is_finite()), "{svg:?}");
            assert!(info.view_box[2] > 0.0 && info.view_box[3] > 0.0, "{svg:?}");
            let text = without_comments(&svg);
            let lower = without_comments(without_declaration(&svg)).to_ascii_lowercase();
            for forbidden in [
                "&", "<!", "<?xml-", "href", "url(", "data:", "<script", "<style", "<text", "<image",
                "stroke", "opacity",
            ] {
                assert!(!lower.contains(forbidden), "accepted {svg:?}, which holds {forbidden:?}");
            }
            assert!(!lower.replace(SVG_NAMESPACE, "").contains("://"), "{svg:?}");
            assert!(text.matches("<?").count() <= 1, "{svg:?}");
        }
        Err(found) => {
            assert!(!found.is_empty(), "a refusal with no finding: {svg:?}");
            for f in &found {
                assert!(codes::ALL.iter().any(|c| c.id == f.code), "{}", f.code);
                assert!(f.code.starts_with("HUD-E07"), "{}", f.code);
                assert_eq!(f.pointer, POINTER);
                plxnative_fuzz::lint_shaped(f);
            }
        }
    }
});
