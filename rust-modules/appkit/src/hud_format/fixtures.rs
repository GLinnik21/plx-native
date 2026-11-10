//! Embedded source fixtures shared by the package, compiler and runtime tests.
use super::bundle::{self, archive::File, manifest};
use super::parse::{self, Ctx, Parsed};
use super::{codes, Finding};
use plx_ui::rec::strict_json::{decode_ordered_with, Budget};
use std::sync::Arc;
macro_rules! source {
    ($name:literal, [$($asset:literal),* $(,)?]) => {
        vec![
            File::new("manifest.json", &include_bytes!(concat!("fixtures/", $name, "/manifest.json"))[..]),
            File::new("hud.json", &include_bytes!(concat!("fixtures/", $name, "/hud.json"))[..]),
            $(File::new(concat!("assets/", $asset, ".svg"), &include_bytes!(concat!("fixtures/", $name, "/assets/", $asset, ".svg"))[..]),)*
        ]
    };
}

pub fn files(name: &str) -> Vec<File> {
    match name {
        "classic" => source!("classic", []),
        "minimal" => source!("minimal", []),
        "reference" => source!("reference", ["subs-bubble", "list", "gear", "eq", "back10", "fwd30"]),
        "hero-center" => source!("hero-center", []),
        "tabs" => source!("tabs", []),
        "palette" => source!("palette", []),
        "rising-tabs" => source!("rising-tabs", ["subs-bubble", "eq"]),
        _ => panic!("unknown source fixture {name}"),
    }
}
pub fn hud(name: &str) -> &'static str {
    match name {
        "classic" => include_str!("fixtures/classic/hud.json"),
        "minimal" => include_str!("fixtures/minimal/hud.json"),
        "reference" => include_str!("fixtures/reference/hud.json"),
        "hero-center" => include_str!("fixtures/hero-center/hud.json"),
        "tabs" => include_str!("fixtures/tabs/hud.json"),
        "palette" => include_str!("fixtures/palette/hud.json"),
        "rising-tabs" => include_str!("fixtures/rising-tabs/hud.json"),
        _ => panic!("unknown source fixture {name}"),
    }
}

/// Repack exact fixture members, including deliberately invalid edits used by admission tests.
pub fn bytes(name: &str) -> Vec<u8> {
    bundle::archive::pack(&files(name)).expect("fixture paths fit the archive profile")
}

pub fn with_hud(name: &str, text: &str) -> Vec<u8> {
    replace(name, "hud.json", text.as_bytes())
}

/// A layout-focused fixture edit with its resource inventory reduced to the masks it still uses.
/// Bundle-inventory tests use `replace` directly so malformed inventories remain observable.
pub fn with_hud_resources(name: &str, text: &str) -> Vec<u8> {
    let mut source = files(name);
    source.iter_mut().find(|file| file.path == "hud.json").unwrap().bytes = Arc::from(text.as_bytes());
    if let Some(package) = parse_for(name, text.as_bytes()).package {
        let used: std::collections::BTreeSet<&str> = package.assets.iter().map(|(id, _)| id.as_str()).collect();
        let metadata = manifest::read(&source[0].bytes, &Budget::new(bundle::JSON_DECODE_BYTES)).unwrap();
        let mut value = metadata.json().clone();
        let mut kept = std::collections::BTreeSet::new();
        if let Some(resources) = value.get_mut("resources").and_then(|value| value.as_object_mut()) {
            resources.retain(|id, path| {
                let keep = used.contains(id.as_str());
                if keep {
                    kept.insert(path.as_str().unwrap().to_owned());
                }
                keep
            });
        }
        source.retain(|file| !file.path.starts_with("assets/") || kept.contains(&file.path));
        source[0].bytes = format!("{value:#}\n").into_bytes().into();
    }
    bundle::archive::pack(&source).expect("fixture paths fit the archive profile")
}

pub fn with_manifest(name: &str, text: &str) -> Vec<u8> {
    replace(name, "manifest.json", text.as_bytes())
}

pub fn replace(name: &str, member: &str, bytes: &[u8]) -> Vec<u8> {
    let mut files = files(name);
    files.iter_mut().find(|file| file.path == member).expect("fixture member").bytes = Arc::from(bytes);
    bundle::archive::pack(&files).expect("fixture paths fit the archive profile")
}

/// Parse a HUD document against the reference fixture's resource namespace. Metadata/resource
/// admission has its own bundle tests; these callers exercise the shared HUD walker and compiler.
pub fn parse(bytes: &[u8]) -> Parsed {
    let source = files("reference");
    let (major, minor) = super::schema::CURRENT;
    let manifest = std::str::from_utf8(&source[0].bytes).unwrap()
        .replacen(r#""format": "1.0""#, &format!(r#""format": "{major}.{minor}""#), 1);
    parse_documents(&source, manifest.as_bytes(), bytes)
}

pub fn parse_for(name: &str, bytes: &[u8]) -> Parsed {
    let source = files(name);
    parse_documents(&source, &source[0].bytes, bytes)
}

/// Exercise a manifest edit against the unchanged HUD and SVG source fixture.
pub fn parse_manifest(name: &str, bytes: &[u8]) -> Parsed {
    let source = files(name);
    parse_documents(&source, bytes, &source[1].bytes)
}

fn parse_documents(files: &[File], manifest_bytes: &[u8], bytes: &[u8]) -> Parsed {
    let budget = Budget::new(bundle::JSON_DECODE_BYTES);
    let metadata = match manifest::read(manifest_bytes, &budget) {
        Ok(manifest) => manifest,
        Err(findings) => return Parsed { package: None, findings },
    };
    if bytes.len() > parse::MAX_HUD_JSON_BYTES {
        return Parsed {
            package: None,
            findings: vec![Finding::new(
                &codes::E0100,
                "",
                format!("HUD document exceeds {} bytes", parse::MAX_HUD_JSON_BYTES),
                "reduce the HUD document",
            )],
        };
    }
    let root = match decode_ordered_with(bytes, &budget) {
        Ok(root) => root,
        Err(error) => {
            return Parsed {
                package: None,
                findings: vec![Finding::new(
                    &codes::E0100,
                    "",
                    format!(
                        "JSON is malformed or too large to read: {}",
                        error.to_string().replace("decoded recording", "decoded JSON")
                    ),
                    "write one bounded UTF-8 JSON object",
                )],
            }
        }
    };
    let ctx = Ctx {
        assets: metadata.resources.iter().map(|(id, _)| id.clone()).collect(),
        format: metadata.hud_format,
        ..Ctx::v1()
    };
    let (hud, findings) = parse::walk_fragment(&super::schema::ROOT, &root, "", &ctx);
    let assets = metadata
        .resources
        .iter()
        .filter_map(|(id, path)| {
            let bytes = &files.iter().find(|file| file.path == *path)?.bytes;
            Some((id.clone(), Arc::<str>::from(std::str::from_utf8(bytes).expect("fixture SVG"))))
        })
        .collect();
    let mut package = hud.and_then(|hud| parse::Package::from_documents(&metadata.object, &hud, metadata.hud_format, assets));
    if let Some(package) = &mut package {
        let used: std::collections::BTreeSet<String> = bundle::resource_uses(package).into_iter().map(str::to_owned).collect();
        package.assets.retain(|(id, _)| used.contains(id));
    }
    Parsed { package, findings }
}
