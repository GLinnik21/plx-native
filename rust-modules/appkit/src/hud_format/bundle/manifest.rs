//! The manifest descriptor root, read with the same strict walker and allocation budget as the HUD.

use std::collections::BTreeSet;

use plx_ui::rec::strict_json::{decode_ordered_with, Budget, Map, Ordered, Value};

use super::{CURRENT, MAX_MANIFEST_BYTES};
use crate::hud_format::parse::{self, AppRange, Ctx, Object, Val};
use crate::hud_format::{codes, pointer_push, schema, schema_emit, Finding, REASON_NEWER, REASON_OLDER};

pub const FILE: &str = "manifest.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub package_format: schema::Since,
    pub hud_format: schema::Since,
    pub entry: String,
    pub id: String,
    pub version: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub app: Option<AppRange>,
    pub resources: Vec<(String, String)>,
    pub notices: Vec<String>,
    pub(in crate::hud_format) object: Object,
    json: Value,
}

impl Manifest {
    /// The validated source shape, including optional-field absence. Report serialization never
    /// supplies the bytes whose digest identifies the package.
    pub fn json(&self) -> &Value {
        &self.json
    }

    /// Every payload path declared by the manifest, in source order.
    pub fn files(&self) -> impl Iterator<Item = &str> {
        std::iter::once(FILE)
            .chain(std::iter::once(self.entry.as_str()))
            .chain(self.resources.iter().map(|(_, path)| path.as_str()))
            .chain(self.notices.iter().map(String::as_str))
    }
}

pub(super) fn at(mut finding: Finding) -> Finding {
    finding.file = Some(FILE.to_owned());
    finding
}

fn gate(value: Option<&Ordered>, pointer: &str, supported: schema::Since) -> Result<schema::Since, Finding> {
    let Some(value) = value else {
        return Err(at(Finding::new(
            &codes::E0401,
            pointer,
            "missing format version",
            "write the required MAJOR.MINOR version",
        )));
    };
    let Some(version) = value.as_str().and_then(parse::parse_format) else {
        return Err(at(Finding::new(
            &codes::E0401,
            pointer,
            "expected a MAJOR.MINOR format version",
            "write a version such as 1.0",
        )));
    };
    if version != (u64::from(supported.0), u64::from(supported.1)) {
        let newer = version > (u64::from(supported.0), u64::from(supported.1));
        return Err(at(Finding::new(
            if version.0 == u64::from(supported.0) {
                &codes::E0402
            } else {
                &codes::E0403
            },
            pointer,
            if newer { REASON_NEWER } else { REASON_OLDER },
            format!("this app supports {}.{} for this document", supported.0, supported.1),
        )));
    }
    Ok(supported)
}

/// Read a manifest without IO. The caller retains this budget for the HUD document.
pub fn read(bytes: &[u8], budget: &Budget) -> Result<Manifest, Vec<Finding>> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(vec![at(Finding::new(
            &codes::E0100,
            "",
            "manifest exceeds its source byte limit",
            "reduce manifest metadata and inventory",
        ))]);
    }
    let root = decode_ordered_with(bytes, budget).map_err(|error| {
        vec![at(Finding::new(
            &codes::E0100,
            "",
            error.to_string().replace("decoded recording", "decoded JSON"),
            "write one bounded UTF-8 JSON object",
        ))]
    })?;
    if !matches!(root, Ordered::Object(_)) {
        return Err(vec![at(Finding::new(
            &codes::E0104,
            "",
            "manifest must be a JSON object",
            "write the manifest as one JSON object",
        ))]);
    }
    let package_format = gate(root.get("package_format"), "/package_format", CURRENT).map_err(|f| vec![f])?;
    let hud_format = gate(root.get("hud").and_then(|hud| hud.get("format")), "/hud/format", schema::CURRENT).map_err(|f| vec![f])?;
    let (object, findings) = parse::walk_fragment(&schema::MANIFEST, &root, "", &Ctx::v1());
    let Some(object) = object else {
        return Err(findings.into_iter().map(at).collect());
    };
    let get = |key: &str| object.get(key).and_then(Val::as_str).map(str::to_owned);
    let invalid = || {
        vec![at(Finding::new(
            &codes::E0103,
            "",
            "manifest is missing required metadata",
            "supply every required manifest field",
        ))]
    };
    let hud = object.get("hud").and_then(Val::as_object).ok_or_else(invalid)?;
    let entry = hud.get("entry").and_then(Val::as_str).ok_or_else(invalid)?.to_owned();
    let resources = object
        .get("resources")
        .and_then(Val::as_object)
        .map(|resources| {
            resources
                .fields
                .iter()
                .filter_map(|(id, value)| value.as_str().map(|path| (id.clone(), path.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    let notices = object
        .get("notices")
        .and_then(Val::as_list)
        .map(|paths| paths.iter().filter_map(Val::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    let app = object.get("app").and_then(Val::as_object).map(|range| AppRange {
        min: range.get("min").and_then(Val::as_str).map(str::to_owned),
        max: range.get("max").and_then(Val::as_str).map(str::to_owned),
    });
    let manifest = Manifest {
        package_format,
        hud_format,
        entry,
        id: get("id").ok_or_else(invalid)?,
        version: get("version").ok_or_else(invalid)?,
        name: get("name").ok_or_else(invalid)?,
        author: get("author").ok_or_else(invalid)?,
        license: get("license").ok_or_else(invalid)?,
        app,
        resources,
        notices,
        json: json_value(&root),
        object,
    };
    let mut errors = Vec::new();
    if let Some(url) = root.get("$schema").and_then(Ordered::as_str) {
        if url != schema_emit::manifest_schema_id(manifest.package_format) {
            errors.push(at(Finding::new(
                &codes::E0401,
                "/$schema",
                "manifest schema URL disagrees with package_format",
                schema_emit::manifest_schema_id(manifest.package_format),
            )));
        }
    }
    let mut paths = BTreeSet::new();
    for path in manifest.files() {
        if !paths.insert(path) {
            errors.push(at(Finding::new(
                &codes::E1602,
                "",
                format!("member {path:?} is declared more than once"),
                "give each member one owner and reuse its resource id",
            )));
        }
    }
    if paths.len() > super::MAX_ENTRIES {
        errors.push(at(Finding::new(
            &codes::E1602,
            "",
            "manifest inventory exceeds the member-count limit",
            "reduce the declared file inventory",
        )));
    }
    if let Some(range) = &manifest.app {
        use plx_data::hudpacks::Semver;
        if matches!((&range.min, &range.max), (Some(min), Some(max)) if Semver::parse(min) > Semver::parse(max)) {
            errors.push(at(Finding::new(
                &codes::E0105,
                "/app",
                "app.min exceeds app.max",
                "use an increasing app-release range",
            )));
        }
    }
    if errors.is_empty() {
        Ok(manifest)
    } else {
        Err(errors)
    }
}

fn json_value(value: &Ordered) -> Value {
    match value {
        Ordered::Null => Value::Null,
        Ordered::Bool(value) => Value::Bool(*value),
        Ordered::Number(value) => Value::Number(value.clone()),
        Ordered::String(value) => Value::String(value.clone()),
        Ordered::Array(values) => Value::Array(values.iter().map(json_value).collect()),
        Ordered::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), json_value(value)))
                .collect::<Map<_, _>>(),
        ),
    }
}

pub(super) fn resource_pointer(id: &str) -> String {
    pointer_push("/resources", id)
}
