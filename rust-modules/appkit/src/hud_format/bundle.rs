//! Bounded, data-only HUD bundles. The archive profile and all package decisions are pure.

pub mod archive;
pub mod manifest;

use std::collections::BTreeSet;
use std::sync::Arc;

use plx_ui::rec::strict_json::{decode_ordered_with, Budget, Ordered};

use super::parse::{self, Ctx, Package, Val};
use super::{codes, has_error, schema_emit, svg_subset, validate, Finding};
pub use manifest::Manifest;

/// Package profiles and HUD schemas evolve independently.
pub const CURRENT: (u8, u8) = (1, 0);
/// Maximum complete transport artifact for package profile 1.0.
pub const MAX_ARTIFACT_BYTES: usize = 2 * 1024 * 1024;
/// Maximum sum of all member payloads for package profile 1.0.
pub const MAX_MEMBER_BYTES: usize = 1024 * 1024;
/// Maximum manifest source length.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// Maximum central-directory metadata, checked before constructing the ZIP reader.
pub const MAX_DIRECTORY_BYTES: usize = 64 * 1024;
/// Maximum regular-file entries in a bundle, including manifest, HUD and notices.
pub const MAX_ENTRIES: usize = 128;
/// Maximum bytes in a portable package-root-relative path.
pub const MAX_PATH_BYTES: usize = 128;
/// Maximum path segments, including the filename.
pub const MAX_PATH_DEPTH: usize = 8;
/// One allocation budget shared by the manifest and HUD JSON trees.
pub const JSON_DECODE_BYTES: usize = 16 * 1024 * 1024;

/// Portable package path. Every segment starts with a lower-case ASCII letter or digit, then
/// contains only lower-case ASCII letters, digits, dots, underscores and hyphens.
pub fn valid_path(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES {
        return false;
    }
    let mut depth = 0;
    for segment in path.split('/') {
        depth += 1;
        let first = segment.as_bytes().first().copied();
        if depth > MAX_PATH_DEPTH
            || !first.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            || segment.ends_with('.')
            || !segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'))
        {
            return false;
        }
        let base = segment.split('.').next().unwrap_or("");
        if matches!(base, "con" | "prn" | "aux" | "nul")
            || base
                .strip_prefix("com")
                .or_else(|| base.strip_prefix("lpt"))
                .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
        {
            return false;
        }
    }
    true
}

/// The complete artifact's digest bound to the resolved, validated package. Only this module can
/// construct the value, so compilation never receives a caller-invented digest/package pairing.
#[derive(Clone, Debug)]
pub struct VerifiedPackage {
    sha256: [u8; 32],
    manifest: Manifest,
    hud: Package,
}

impl VerifiedPackage {
    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn hud(&self) -> &Package {
        &self.hud
    }
}

/// Admission preserves a validated manifest for diagnostics when the HUD document fails later.
#[derive(Clone, Debug)]
pub struct Decoded {
    pub package: Option<VerifiedPackage>,
    pub manifest: Option<Manifest>,
    pub sha256: Option<[u8; 32]>,
    pub findings: Vec<Finding>,
    pub(super) diagnostic: Option<Package>,
}

impl Decoded {
    /// A source read failed before a complete artifact existed.
    pub fn source_error(manifest: Option<Manifest>, findings: Vec<Finding>) -> Self {
        Self::failed(None, manifest, findings)
    }

    /// A typed HUD is available for complete authoring diagnostics.
    pub fn has_layout(&self) -> bool {
        self.package.is_some() || self.diagnostic.is_some()
    }

    fn failed(sha256: Option<[u8; 32]>, manifest: Option<Manifest>, findings: Vec<Finding>) -> Self {
        Self {
            package: None,
            manifest,
            sha256,
            findings,
            diagnostic: None,
        }
    }
}

fn archive_finding(error: archive::Error) -> Finding {
    let mut finding = Finding::new(&codes::E1601, "", error.message, "build the bundle with --hud-pack");
    finding.file = error.member;
    finding
}

fn in_file(mut finding: Finding, file: &str) -> Finding {
    finding.file = Some(file.to_owned());
    finding
}

/// Decode one complete, bounded archive. The digest is computed here, before parsing, and compared
/// with the confirmed/published digest when supplied. Oversized input never receives a digest.
pub fn read(bytes: &[u8], expected: Option<[u8; 32]>) -> Decoded {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Decoded::failed(
            None,
            None,
            vec![archive_finding(archive::Error {
                member: None,
                message: "bundle exceeds the package artifact byte limit".to_owned(),
            })],
        );
    }
    let sha256 = plx_base::sha256::sha256(bytes);
    if expected.is_some_and(|expected| expected != sha256) {
        return Decoded::failed(
            Some(sha256),
            None,
            vec![Finding::new(
                &codes::E1603,
                "",
                "artifact digest differs from the expected package",
                "fetch or confirm the complete artifact again",
            )],
        );
    }
    let files = match archive::read(bytes) {
        Ok(files) => files,
        Err(error) => return Decoded::failed(Some(sha256), None, vec![archive_finding(error)]),
    };
    let file = |path: &str| files.iter().find(|file| file.path == path);
    let Some(manifest_file) = file(manifest::FILE) else {
        return Decoded::failed(
            Some(sha256),
            None,
            vec![Finding::new(
                &codes::E1602,
                "",
                "bundle has no manifest.json",
                "put manifest.json at the package root",
            )],
        );
    };
    let budget = Budget::new(JSON_DECODE_BYTES);
    let manifest = match manifest::read(&manifest_file.bytes, &budget) {
        Ok(manifest) => manifest,
        Err(findings) => return Decoded::failed(Some(sha256), None, findings),
    };
    let mut findings = Vec::new();
    let inventory: BTreeSet<&str> = manifest.files().collect();
    for path in &inventory {
        if file(path).is_none() {
            findings.push(manifest::at(Finding::new(
                &codes::E1602,
                "",
                format!("declared member {path:?} is missing"),
                "include every declared member",
            )));
        }
    }
    for member in &files {
        if !inventory.contains(member.path.as_str()) {
            findings.push(in_file(
                Finding::new(
                    &codes::E1602,
                    "",
                    "archive member has no manifest declaration",
                    "declare the member or remove it from the source snapshot",
                ),
                &member.path,
            ));
        }
    }
    for path in &manifest.notices {
        if file(path).is_some_and(|file| std::str::from_utf8(&file.bytes).is_err()) {
            findings.push(in_file(
                Finding::new(
                    &codes::E1602,
                    "",
                    "notice is not UTF-8 text",
                    "store legal notices as plain UTF-8 text",
                ),
                path,
            ));
        }
    }
    if has_error(&findings) {
        return Decoded::failed(Some(sha256), Some(manifest), findings);
    }
    let mut assets = Vec::new();
    for (id, path) in &manifest.resources {
        let Some(member) = file(path) else { continue };
        match std::str::from_utf8(&member.bytes) {
            Ok(svg) => match svg_subset::check_asset("", svg) {
                Ok(_) => assets.push((id.clone(), Arc::<str>::from(svg))),
                Err(errors) => findings.extend(errors.into_iter().map(|finding| in_file(finding, path))),
            },
            Err(_) => findings.push(in_file(
                Finding::new(&codes::E0704, "", "SVG mask is not UTF-8", "store the SVG source as UTF-8 text"),
                path,
            )),
        }
    }
    let hud_file = file(&manifest.entry).expect("the checked inventory contains its HUD entry");
    if hud_file.bytes.len() > parse::MAX_HUD_JSON_BYTES {
        findings.push(in_file(
            Finding::new(
                &codes::E0100,
                "",
                "HUD document exceeds its source byte limit",
                "reduce the HUD JSON document",
            ),
            &manifest.entry,
        ));
        return Decoded::failed(Some(sha256), Some(manifest), findings);
    }
    let root = match decode_ordered_with(&hud_file.bytes, &budget) {
        Ok(root) => root,
        Err(error) => {
            findings.push(in_file(
                Finding::new(
                    &codes::E0100,
                    "",
                    error.to_string().replace("decoded recording", "decoded JSON"),
                    "write one bounded UTF-8 HUD JSON object",
                ),
                &manifest.entry,
            ));
            return Decoded::failed(Some(sha256), Some(manifest), findings);
        }
    };
    if let Some(url) = root.get("$schema").and_then(Ordered::as_str) {
        if url != schema_emit::schema_id(manifest.hud_format) {
            findings.push(in_file(
                Finding::new(
                    &codes::E0401,
                    "/$schema",
                    "HUD schema URL disagrees with hud.format",
                    schema_emit::schema_id(manifest.hud_format),
                ),
                &manifest.entry,
            ));
        }
    }
    let ctx = Ctx {
        assets: manifest.resources.iter().map(|(id, _)| id.clone()).collect(),
        format: manifest.hud_format,
        ..Ctx::v1()
    };
    let (hud, errors) = parse::walk_fragment(&super::schema::ROOT, &root, "", &ctx);
    findings.extend(errors.into_iter().map(|finding| in_file(finding, &manifest.entry)));
    let Some(hud) = hud.and_then(|hud| Package::from_documents(&manifest.object, &hud, manifest.hud_format, assets)) else {
        return Decoded::failed(Some(sha256), Some(manifest), findings);
    };
    let used = resource_uses(&hud);
    for (id, _) in &manifest.resources {
        if !used.contains(id.as_str()) {
            findings.push(manifest::at(Finding::new(
                &codes::E1602,
                manifest::resource_pointer(id),
                format!("resource {id:?} is never used by the HUD"),
                "remove the resource declaration or reference it from the HUD",
            )));
        }
    }
    if has_error(&findings) {
        return Decoded::failed(Some(sha256), Some(manifest), findings);
    }
    findings.extend(
        super::check(&hud)
            .findings
            .into_iter()
            .map(|finding| in_file(finding, &manifest.entry)),
    );
    let (package, diagnostic) = if has_error(&findings) {
        (None, Some(hud))
    } else {
        (
            Some(VerifiedPackage {
                sha256,
                manifest: manifest.clone(),
                hud,
            }),
            None,
        )
    };
    Decoded {
        package,
        manifest: Some(manifest),
        sha256: Some(sha256),
        findings,
        diagnostic,
    }
}

pub(super) fn resource_uses(package: &Package) -> BTreeSet<&str> {
    fn collect<'a>(value: &'a Val, out: &mut BTreeSet<&'a str>) {
        match value {
            Val::Ref(id) => {
                out.insert(id);
            }
            Val::Object(object) => {
                for (_, value) in &object.fields {
                    collect(value, out);
                }
            }
            _ => {}
        }
    }
    let mut used = BTreeSet::new();
    validate::each_object(&package.layout, &mut |object| {
        for (key, value) in &object.fields {
            if matches!(key.as_str(), "asset" | "face") {
                collect(value, &mut used);
            }
        }
    });
    used
}

/// The canonical archive and the verdict on those exact bytes. A malformed source never publishes
/// a partial output artifact, and caller-side filesystem writes happen only after this succeeds.
pub fn pack(files: &[archive::File]) -> Result<(Vec<u8>, Decoded), Decoded> {
    let bytes = archive::pack(files).map_err(|error| Decoded::failed(None, None, vec![archive_finding(error)]))?;
    let decoded = read(&bytes, None);
    if decoded.package.is_some() {
        Ok((bytes, decoded))
    } else {
        Err(decoded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> Vec<archive::File> {
        vec![
            archive::File::new("manifest.json", &include_bytes!("fixtures/minimal/manifest.json")[..]),
            archive::File::new("hud.json", &include_bytes!("fixtures/minimal/hud.json")[..]),
        ]
    }

    fn changed(mut files: Vec<archive::File>, path: &str, from: &str, to: &str) -> Vec<archive::File> {
        let file = files.iter_mut().find(|file| file.path == path).unwrap();
        let text = std::str::from_utf8(&file.bytes).unwrap();
        assert!(text.contains(from), "{from}");
        file.bytes = text.replacen(from, to, 1).into_bytes().into();
        files
    }

    #[test]
    fn the_complete_archive_digest_is_bound_to_the_resolved_package() {
        let (bytes, decoded) = pack(&minimal()).unwrap();
        let package = decoded.package.unwrap();
        assert_eq!(package.sha256(), plx_base::sha256::sha256(&bytes));
        assert_eq!(package.manifest().id, "com.plxnative.examples.minimal");
        assert_eq!(package.manifest().package_format, (1, 0));
        assert_eq!(package.manifest().hud_format, (1, 0));
        assert!(package.manifest().json().get("resources").is_none());
        assert!(read(&bytes, Some(package.sha256())).package.is_some());
        let mismatch = read(&bytes, Some([0; 32]));
        assert_eq!(mismatch.findings[0].code, codes::E1603.id);
        assert!(mismatch.package.is_none());
    }

    #[test]
    fn independent_format_gates_and_document_locations_are_explicit() {
        for (from, to, pointer) in [
            ("\"package_format\": \"1.0\"", "\"package_format\": \"1.1\"", "/package_format"),
            ("\"format\": \"1.0\"", "\"format\": \"1.1\"", "/hud/format"),
        ] {
            let files = changed(minimal(), "manifest.json", from, to);
            let bytes = archive::pack(&files).unwrap();
            let decoded = read(&bytes, None);
            assert_eq!(decoded.findings.len(), 1);
            assert_eq!(decoded.findings[0].file.as_deref(), Some("manifest.json"));
            assert_eq!(decoded.findings[0].pointer, pointer);
            assert_eq!(decoded.findings[0].code, codes::E0402.id);
        }
        let files = changed(minimal(), "hud.json", "\"anchor\"", "\"anchoor\"");
        let decoded = read(&archive::pack(&files).unwrap(), None);
        assert!(decoded
            .findings
            .iter()
            .any(|f| f.file.as_deref() == Some("hud.json") && f.pointer == "/layout/anchoor"));
    }

    #[test]
    fn inventory_requires_every_declared_member_and_rejects_unused_payloads() {
        let mut files = minimal();
        files.push(archive::File::new("surprise.txt", &b"undeclared"[..]));
        assert!(pack(&files).is_err());
        files.pop();
        files.pop();
        assert!(pack(&files).is_err());
        let mut files = changed(
            minimal(),
            "manifest.json",
            "\"hud\":",
            "\"resources\": {\"unused\": \"assets/unused.svg\"}, \"hud\":",
        );
        files.push(archive::File::new(
            "assets/unused.svg",
            &b"<svg viewBox='0 0 24 24'><path d='M0 0h4v4z'/></svg>"[..],
        ));
        let decoded = pack(&files).unwrap_err();
        assert!(decoded
            .findings
            .iter()
            .any(|f| f.code == codes::E1602.id && f.pointer == "/resources/unused"));
    }

    #[test]
    fn source_order_and_external_svg_bytes_survive_packing() {
        let mut files = changed(
            minimal(),
            "manifest.json",
            "\"hud\":",
            "\"resources\": {\"more\": \"assets/more.svg\"}, \"hud\":",
        );
        files = changed(
            files,
            "hud.json",
            "\"command\": \"open_more\"",
            "\"command\": \"open_more\", \"face\": \"more\"",
        );
        let svg = b"<svg viewBox='0 0 24 24'><path d='M0 0h4v4z'/></svg>";
        files.push(archive::File::new("assets/more.svg", &svg[..]));
        let (bytes, decoded) = pack(&files).unwrap();
        let package = decoded.package.unwrap();
        assert_eq!(package.hud().assets[0].1.as_ref(), std::str::from_utf8(svg).unwrap());
        let before = package.sha256();
        let files = changed(files, "assets/more.svg", "h4", "h5");
        let (after_bytes, after) = pack(&files).unwrap();
        assert_ne!(before, after.package.unwrap().sha256());
        assert_ne!(bytes, after_bytes);
    }

    #[test]
    fn an_oversized_artifact_has_no_hash_of_a_capped_prefix() {
        let decoded = read(&vec![0; MAX_ARTIFACT_BYTES + 1], None);
        assert!(decoded.sha256.is_none());
        assert!(decoded.package.is_none());
    }

    #[test]
    fn source_fixtures_use_the_canonical_writer() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/hud_format/fixtures");
        for name in ["classic", "minimal", "reference", "hero-center", "tabs", "palette", "rising-tabs"] {
            let source = root.join(name);
            let manifest_bytes = std::fs::read(source.join("manifest.json")).unwrap();
            let manifest = manifest::read(&manifest_bytes, &Budget::new(JSON_DECODE_BYTES)).unwrap();
            let files: Vec<_> = manifest
                .files()
                .map(|path| archive::File::new(path, std::fs::read(source.join(path)).unwrap()))
                .collect();
            let (bytes, _) = pack(&files).unwrap_or_else(|failure| panic!("{name}: {:?}", failure.findings));
            let path = root.join(format!("{name}.plxhud"));
            if std::env::var_os("PLX_HUD_BUNDLE_WRITE").is_some_and(|value| value == "1") {
                std::fs::write(&path, &bytes).unwrap();
            }
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{name}: regenerate with PLX_HUD_BUNDLE_WRITE=1"
            );
        }
    }
    #[test]
    fn invariant_failures_keep_complete_lint_without_admitting_a_runtime_package() {
        use crate::hud_format::lint::{lint_decoded, LintLang, LintOptions};
        use plx_base::fontcov::advances::ShippedMeasure;
        let hud = crate::hud_format::fixtures::hud("minimal")
            .replace(
                "\"command\": \"open_more\"",
                "\"command\": \"open_more\", \"when\": \"title.is_episode\"",
            )
            .replace("\"size\": 52", "\"size\": 20");
        let bytes = crate::hud_format::fixtures::with_hud("minimal", &hud);
        let decoded = read(&bytes, None);
        assert!(decoded.package.is_none() && decoded.has_layout(), "{:?}", decoded.findings);
        let linted = lint_decoded(
            &decoded,
            &ShippedMeasure,
            &ShippedMeasure,
            &[LintLang::for_tag("en").unwrap()],
            &LintOptions::default(),
        );
        assert!(linted.findings.iter().any(|finding| finding.code == "HUD-E0801"));
        assert!(linted.findings.iter().any(|finding| finding.code == "HUD-E0603"));
        assert!(!linted.costs.is_empty());
        assert!(linted.findings.iter().all(|finding| finding.file.as_deref() == Some("hud.json")));
    }

    #[test]
    fn an_invalid_resource_stops_before_layout_admission() {
        let mut files = crate::hud_format::fixtures::files("reference");
        files.iter_mut().find(|file| file.path == "assets/eq.svg").unwrap().bytes =
            b"<svg viewBox='0 0 24 24'><linearGradient/></svg>".as_slice().into();
        let decoded = read(&archive::pack(&files).unwrap(), None);
        assert!(decoded.package.is_none() && !decoded.has_layout());
        assert!(decoded
            .findings
            .iter()
            .all(|finding| finding.file.as_deref() == Some("assets/eq.svg")));
    }
}
