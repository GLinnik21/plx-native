//! Deterministic JSON-to-Rust catalog compiler, also exercised by the host unit suite.
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::Path;

struct Unique(Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON without duplicate keys")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut m: M) -> Result<Unique, M::Error> {
                let mut out = serde_json::Map::new();
                while let Some((k, v)) = m.next_entry::<String, Unique>()? {
                    if out.insert(k.clone(), v.0).is_some() {
                        return Err(de::Error::custom(format!("duplicate key {k}")));
                    }
                }
                Ok(Unique(Value::Object(out)))
            }
            fn visit_seq<S: SeqAccess<'de>>(self, mut s: S) -> Result<Unique, S::Error> {
                let mut out = Vec::new();
                while let Some(v) = s.next_element::<Unique>()? {
                    out.push(v.0);
                }
                Ok(Unique(Value::Array(out)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Unique, E> {
                Ok(Unique(Value::String(v.into())))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Unique, E> {
                Ok(Unique(Value::Bool(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Unique, E> {
                Ok(Unique(serde_json::json!(v)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Unique, E> {
                Ok(Unique(Value::Null))
            }
        }
        d.deserialize_any(V)
    }
}
fn parse(s: &str) -> Result<Value, String> {
    serde_json::from_str::<Unique>(s)
        .map(|v| v.0)
        .map_err(|e| e.to_string())
}
fn ident(s: &str) -> bool {
    !s.is_empty()
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}
fn forms(v: &Value) -> Result<BTreeMap<String, String>, String> {
    if let Some(s) = v.as_str() {
        return Ok(BTreeMap::from([("".into(), s.into())]));
    }
    let m = v
        .as_object()
        .ok_or("message must be text or plural forms")?;
    if !m.contains_key("other") {
        return Err("plural message requires other".into());
    }
    m.iter()
        .map(|(k, v)| {
            if !["zero", "one", "two", "few", "many", "other"].contains(&k.as_str()) {
                return Err(format!("unknown plural category {k}"));
            }
            Ok((
                k.clone(),
                v.as_str().ok_or("plural forms must be strings")?.into(),
            ))
        })
        .collect()
}
#[derive(Debug, PartialEq)]
enum Part {
    Text(String),
    Arg(String),
}
fn parts(s: &str) -> Result<Vec<Part>, String> {
    if s.trim().is_empty() || s.contains('\0') {
        return Err("empty text or embedded NUL".into());
    }
    let mut out = Vec::new();
    let mut text = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            if chars.peek() == Some(&'{') {
                chars.next();
                text.push('{');
                continue;
            }
            if !text.is_empty() {
                out.push(Part::Text(std::mem::take(&mut text)));
            }
            let mut key = String::new();
            let mut ended = false;
            for ch in chars.by_ref() {
                if ch == '}' {
                    ended = true;
                    break;
                }
                key.push(ch);
            }
            if !ended || !ident(&key) {
                return Err(format!("invalid placeholder {{{key}}}"));
            }
            out.push(Part::Arg(key));
        } else if c == '}' {
            if chars.next() != Some('}') {
                return Err("unmatched closing brace".into());
            }
            text.push('}');
        } else {
            text.push(c);
        }
    }
    if !text.is_empty() {
        out.push(Part::Text(text));
    }
    Ok(out)
}
fn expression(s: &str, args: &BTreeMap<String, String>) -> Result<String, String> {
    let mut pattern = String::new();
    let mut values = Vec::new();
    for p in parts(s)? {
        match p {
            Part::Text(t) => pattern.push_str(&t.replace('{', "{{").replace('}', "}}")),
            Part::Arg(k) => {
                pattern.push_str("{}");
                values.push(if args.get(&k).map(String::as_str) == Some("i64") {
                    format!("cx.number({k})")
                } else {
                    k
                });
            }
        }
    }
    Ok(format!(
        "format!({pattern:?}{})",
        values.iter().map(|v| format!(", {v}")).collect::<String>()
    ))
}
fn pseudo(s: &str) -> String {
    let mut out = String::from("[!! ");
    for p in parts(s).expect("validated template") {
        match p {
            Part::Arg(a) => {
                out.push('{');
                out.push_str(&a);
                out.push('}');
            }
            Part::Text(t) => {
                for c in t.chars() {
                    out.push_str(match c {
                        'a' => "áá",
                        'e' => "ëë",
                        'i' => "ï",
                        'o' => "öö",
                        'u' => "üü",
                        '{' => "{{",
                        '}' => "}}",
                        _ => {
                            out.push(c);
                            continue;
                        }
                    });
                }
            }
        }
    }
    out.push_str(" !!]");
    out
}
/// The scripts the text renderer can shape. Widening this list is renderer work, not data.
const SCRIPTS: [&str; 2] = ["Latn", "Cyrl"];
const CATEGORIES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];
const RESERVED_TAGS: [&str; 3] = ["system", "qps", "und"];
const GUIDE: &str = "docs/localization.md#adding-a-language";
/// Rows at or above this belong to the Language page's non-language rows (Contribute).
const FIRST_NON_LANGUAGE_ROW: u32 = 1000;
/// How many missing/extra key lines one language reports before "+N more".
const KEY_ERROR_LINES: usize = 20;

#[derive(Clone, Debug)]
pub struct LanguageSpec {
    pub tag: String,
    pub variant: String,
    pub native_name: String,
    pub region: String,
    pub script: String,
    pub plurals: Vec<String>,
    /// Regions of this language that resolve to English instead (pt: BR, which has no catalog).
    pub excluded_regions: Vec<String>,
    pub row: u32,
}
#[derive(Clone, Debug)]
pub struct Catalog {
    pub spec: LanguageSpec,
    pub entries: BTreeMap<String, Value>,
    /// The file each key came from, as `<tag>/<file>.json`, for errors a translator can act on.
    pub origin: BTreeMap<String, String>,
}
pub struct Generated {
    pub messages: String,
    pub languages: String,
}

/// Parse and validate `locales/languages.json`. Every problem is reported, not just the first.
pub fn parse_registry(text: &str) -> Result<Vec<LanguageSpec>, Vec<String>> {
    let at = "locales/languages.json";
    let value = parse(text).map_err(|e| vec![format!("{at}: {e}")])?;
    let Some(items) = value.as_array() else {
        return Err(vec![format!("{at}: must be an array of language entries")]);
    };
    let mut errors = Vec::new();
    let mut specs: Vec<LanguageSpec> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let Some(m) = item.as_object() else {
            errors.push(format!("{at}: entry {i} must be an object"));
            continue;
        };
        let name = m.get("tag").and_then(Value::as_str).unwrap_or("?").to_string();
        let here = format!("{at}: {name}");
        for k in m.keys() {
            if !["tag", "native_name", "region", "script", "plurals", "excluded_regions", "row"].contains(&k.as_str()) {
                errors.push(format!("{here}: unknown field {k}"));
            }
        }
        let text_of = |k: &str, errors: &mut Vec<String>| -> String {
            m.get(k).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| {
                errors.push(format!("{here}: {k} must be a string"));
                String::new()
            })
        };
        let tag = text_of("tag", &mut errors);
        let native_name = text_of("native_name", &mut errors);
        let region = text_of("region", &mut errors);
        let script = text_of("script", &mut errors);
        let row = m.get("row").and_then(Value::as_u64).unwrap_or(0);
        let plurals: Vec<String> = m
            .get("plurals")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        let excluded_regions: Vec<String> = match m.get("excluded_regions") {
            None => Vec::new(),
            Some(v) => match v.as_array() {
                Some(a) if a.iter().all(Value::is_string) => {
                    a.iter().filter_map(Value::as_str).map(str::to_string).collect()
                }
                _ => {
                    errors.push(format!("{here}: excluded_regions must be an array of region codes"));
                    Vec::new()
                }
            },
        };
        for r in &excluded_regions {
            if r.len() != 2 || !r.bytes().all(|c| c.is_ascii_uppercase()) {
                errors.push(format!("{here}: excluded_regions entry {r:?} must be two upper-case letters, such as BR"));
            }
        }
        if (1..excluded_regions.len()).any(|i| excluded_regions[..i].contains(&excluded_regions[i])) {
            errors.push(format!("{here}: excluded_regions lists a region twice"));
        }
        if !(2..=3).contains(&tag.len()) || !tag.bytes().all(|c| c.is_ascii_lowercase()) {
            errors.push(format!(
                "{here}: tag must be 2-3 lower-case letters; a region- or script-qualified UI language (pt-BR) needs matcher work, see {GUIDE}"
            ));
            // Nothing below may touch a tag that is not plain ASCII (the variant name slices it).
            continue;
        }
        if RESERVED_TAGS.contains(&tag.as_str()) {
            errors.push(format!("{here}: tag is reserved"));
        }
        if specs.iter().any(|s| s.tag == tag) {
            errors.push(format!("{here}: duplicate tag"));
        }
        if native_name.trim().is_empty() || native_name.chars().any(char::is_control) {
            errors.push(format!("{here}: native_name must be non-empty text without control characters"));
        }
        let region_ok = region
            .strip_prefix(&format!("{tag}-"))
            .is_some_and(|r| r.len() == 2 && r.bytes().all(|c| c.is_ascii_uppercase()));
        if !region_ok {
            errors.push(format!("{here}: region must be <tag>-<REGION>, such as {tag}-XX; found {region:?}"));
        }
        if !SCRIPTS.contains(&script.as_str()) {
            errors.push(format!(
                "{here}: script {script:?} is not one the renderer shapes ({}); shaping or bidi support is renderer work, see docs/localization.md",
                SCRIPTS.join(", ")
            ));
        }
        let mut sorted = plurals.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != plurals.len()
            || !plurals.iter().any(|p| p == "other")
            || plurals.iter().any(|p| !CATEGORIES.contains(&p.as_str()))
        {
            errors.push(format!(
                "{here}: plurals must be distinct CLDR categories ({}) and include other",
                CATEGORIES.join(", ")
            ));
        }
        if row == 0 {
            errors.push(format!("{here}: row 0 is System; a language row is 1..{FIRST_NON_LANGUAGE_ROW}"));
        } else if row >= u64::from(FIRST_NON_LANGUAGE_ROW) {
            errors.push(format!("{here}: row {row} is reserved for the page's non-language rows; use 1..{FIRST_NON_LANGUAGE_ROW}"));
        }
        if specs.iter().any(|s| u64::from(s.row) == row) {
            errors.push(format!("{here}: duplicate row {row}"));
        }
        if let Some(last) = specs.last() {
            if u64::from(last.row) >= row {
                errors.push(format!("{here}: entries are listed in row order (Settings display order); row {row} follows {}", last.row));
            }
        }
        let variant = tag[..1].to_uppercase() + &tag[1..];
        specs.push(LanguageSpec {
            tag,
            variant,
            native_name,
            region,
            script,
            plurals,
            excluded_regions,
            row: u32::try_from(row).unwrap_or(0),
        });
    }
    if !specs.iter().any(|s| s.tag == "en") {
        errors.push(format!("{at}: English (en) is the source and the fallback; it must be registered"));
    }
    if errors.is_empty() { Ok(specs) } else { Err(errors) }
}

fn visible(name: &std::ffi::OsStr) -> bool {
    !name.to_string_lossy().starts_with('.')
}
fn catalog_files(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        if visible(&entry.file_name()) {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}
fn read_locale(root: &Path, spec: &LanguageSpec) -> Result<Catalog, Vec<String>> {
    let tag = &spec.tag;
    let mut entries = BTreeMap::new();
    let mut origin = BTreeMap::new();
    let mut errors = Vec::new();
    for p in catalog_files(&root.join(tag)).map_err(|e| vec![e])? {
        let file = format!("{tag}/{}", p.file_name().unwrap().to_string_lossy());
        if p.extension().and_then(|s| s.to_str()) != Some("json") || !p.is_file() {
            errors.push(format!("{file}: not a catalog file; only <name>.json files belong in locales/{tag}"));
            continue;
        }
        let v = match std::fs::read_to_string(&p).map_err(|e| e.to_string()).and_then(|t| parse(&t)) {
            Ok(v) => v,
            Err(e) => {
                errors.push(format!("{file}: {e}"));
                continue;
            }
        };
        let Some(obj) = v.as_object() else {
            errors.push(format!("{file}: catalog must be an object"));
            continue;
        };
        for (k, v) in obj {
            if entries.insert(k.clone(), v.clone()).is_some() {
                errors.push(format!("{file}: duplicate catalog key {k} (also in {})", origin[k]));
            }
            origin.insert(k.clone(), file.clone());
        }
    }
    if errors.is_empty() { Ok(Catalog { spec: spec.clone(), entries, origin }) } else { Err(errors) }
}

/// Whether translators supply this English entry. `"translate": false` marks a technical
/// read-out that shows in English in every language (the diagnostics panel, support codes);
/// it is declared only on the English entry and `false` is its only value, so there is one
/// spelling and no redundant `true` to drift.
fn translatable(entry: &Value) -> Result<bool, String> {
    match entry.get("translate") {
        None => Ok(true),
        Some(Value::Bool(false)) => Ok(false),
        Some(_) => Err("translate must be false; omit it for a message translators supply".into()),
    }
}

/// Missing and extra keys of one language against the translatable English keys, one line each,
/// capped. A key that is English-only gets its own message: "unknown" would send the translator
/// looking for a typo.
fn check_keys(lang: &Catalog, expected: &BTreeSet<&str>, english_only: &BTreeSet<&str>) -> Vec<String> {
    let have: BTreeSet<&str> = lang.entries.keys().map(String::as_str).collect();
    let tag = &lang.spec.tag;
    let mut lines: Vec<String> = expected
        .difference(&have)
        .map(|k| format!("locales/{tag}: {k}: missing; add a translation (the key is in locales/en)"))
        .collect();
    lines.extend(have.difference(expected).map(|k| {
        if english_only.contains(k) {
            format!("{}: {k}: English-only (translate:false in locales/en); delete it", lang.origin[*k])
        } else {
            format!("{}: {k}: not in locales/en; add it to English with a description, or delete it", lang.origin[*k])
        }
    }));
    if lines.len() > KEY_ERROR_LINES {
        let more = lines.len() - KEY_ERROR_LINES;
        lines.truncate(KEY_ERROR_LINES);
        lines.push(format!("locales/{tag}: +{more} more key errors"));
    }
    lines
}

struct KeyCode {
    code: String,
    corpus: Vec<String>,
}
fn key_code(key: &str, source: &Catalog, langs: &[Catalog]) -> Result<KeyCode, Vec<String>> {
    let entry = &source.entries[key];
    let src_file = &source.origin[key];
    let at = format!("{src_file}: {key}");
    if !key.contains('.') || !key.split('.').all(ident) {
        return Err(vec![format!("{at}: invalid scoped key")]);
    }
    let name = key.replace('.', "_");
    if entry.get("args").is_some_and(|v| !v.is_object()) {
        return Err(vec![format!("{at}: args must be an object")]);
    }
    let mut errors = Vec::new();
    let translate = match translatable(entry) {
        Ok(t) => t,
        Err(e) => {
            errors.push(format!("{at}: {e}"));
            true
        }
    };
    // An English-only message reads no translator catalog: `langs` is not consulted for it.
    let langs: &[Catalog] = if translate { langs } else { &[] };
    let mut args: BTreeMap<String, String> = BTreeMap::new();
    for (k, v) in entry.get("args").and_then(Value::as_object).into_iter().flatten() {
        let Some(ty) = v.as_str() else {
            return Err(vec![format!("{at}: argument type must be str or i64")]);
        };
        if !ident(k) || ["cx", "self", "type", "match", "ref"].contains(&k.as_str()) || !["str", "i64"].contains(&ty) {
            errors.push(format!("{at}: invalid argument {k}: {ty}"));
        }
        args.insert(k.clone(), ty.into());
    }
    // Arguments a language may leave out of its text: the code passes every argument, and a language
    // spells the thing it says with the ones its grammar wants (a decade is "2020s" or "2020–29").
    let mut optional: BTreeSet<String> = BTreeSet::new();
    match entry.get("optional") {
        None => {}
        Some(Value::Array(names)) => {
            for name in names {
                match name.as_str() {
                    Some(k) if args.contains_key(k) && k != "count" => {
                        optional.insert(k.to_string());
                    }
                    _ => errors.push(format!("{at}: optional must name arguments of the message other than count ({name})")),
                }
            }
        }
        Some(_) => errors.push(format!("{at}: optional must be a list of argument names")),
    }
    if entry.get("description").and_then(Value::as_str).is_none_or(|s| s.trim().is_empty()) {
        errors.push(format!("{at}: missing translator context (description)"));
    }
    let Some(source_value) = entry.get("value") else {
        errors.push(format!("{at}: missing value"));
        return Err(errors);
    };
    // (spec, file, forms) for English first, then every other language in order.
    let mut values: Vec<(&LanguageSpec, String, BTreeMap<String, String>)> = Vec::new();
    match forms(source_value) {
        Ok(f) => values.push((&source.spec, src_file.clone(), f)),
        Err(e) => errors.push(format!("{at}: {e}")),
    }
    for l in langs {
        let file = &l.origin[key];
        let v = &l.entries[key];
        if v.as_object().is_some_and(|m| ["value", "description", "args", "optional", "translate"].iter().any(|k| m.contains_key(*k))) {
            errors.push(format!(
                "{file}: {key}: a translation is a bare string or a plural map; \"value\", \"description\", \"args\", \"optional\" and \"translate\" belong only in locales/en"
            ));
            continue;
        }
        match forms(v) {
            Ok(f) => values.push((&l.spec, file.clone(), f)),
            Err(e) => errors.push(format!("{file}: {key}: {e}")),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let plural = !values[0].2.contains_key("");
    if plural && args.get("count").map(String::as_str) != Some("i64") {
        errors.push(format!("{at}: plurals require count:i64"));
    }
    let argkeys: BTreeSet<_> = args.keys().cloned().collect();
    let mut corpus = Vec::new();
    let mut used_anywhere: BTreeSet<String> = BTreeSet::new();
    for (spec, file, fs) in &values {
        if plural == fs.contains_key("") {
            errors.push(format!("{file}: {key}: message kind differs from English (text versus plural forms)"));
            continue;
        }
        if plural {
            let have: BTreeSet<&str> = fs.keys().map(String::as_str).collect();
            let need: BTreeSet<&str> = spec.plurals.iter().map(String::as_str).collect();
            let missing: Vec<_> = need.difference(&have).copied().collect();
            let extra: Vec<_> = have.difference(&need).copied().collect();
            if !missing.is_empty() || !extra.is_empty() {
                let mut msg = format!("{file}: {key}: {} needs {}", spec.tag, spec.plurals.join(", "));
                if !missing.is_empty() {
                    msg.push_str(&format!("; missing {}", missing.join(", ")));
                }
                if !extra.is_empty() {
                    msg.push_str(&format!("; {} is never selected for this language, delete it", extra.join(", ")));
                }
                errors.push(msg);
            }
        }
        for s in fs.values() {
            let used = match parts(s) {
                Ok(p) => p
                    .into_iter()
                    .filter_map(|p| match p {
                        Part::Arg(k) => Some(k),
                        _ => None,
                    })
                    .collect::<BTreeSet<_>>(),
                Err(e) => {
                    errors.push(format!("{file}: {key}: {e}"));
                    continue;
                }
            };
            if used.iter().any(|k| !argkeys.contains(k))
                || argkeys.iter().any(|k| !used.contains(k) && !(plural && k == "count") && !optional.contains(k))
            {
                errors.push(format!(
                    "{file}: {key}: placeholder mismatch expected {argkeys:?}, found {used:?}"
                ));
            }
            used_anywhere.extend(used);
            corpus.push(s.clone());
        }
    }
    // An optional argument no language reads would be a parameter nothing uses.
    for k in optional.iter().filter(|k| !used_anywhere.contains(*k)) {
        errors.push(format!("{at}: optional argument {k} is used by no language; make it required or drop it"));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let (en, rest) = values.split_first().expect("English first");
    if !translate {
        return Ok(KeyCode { code: english_only_code(&name, &en.2, &args, plural), corpus });
    }
    // Language arms in registry order, the generated Pseudo last.
    let mut arms: Vec<(String, BTreeMap<String, String>)> = std::iter::once(en)
        .chain(rest)
        .map(|(spec, _, f)| (spec.variant.clone(), f.clone()))
        .collect();
    arms.push((
        "Pseudo".into(),
        en.2.iter().map(|(k, v)| (k.clone(), pseudo(v))).collect(),
    ));
    let params = args
        .iter()
        .map(|(k, v)| format!(", {k}: {}", if v == "str" { "&str" } else { "i64" }))
        .collect::<String>();
    let forwards = args.keys().map(|k| format!(", {k}")).collect::<String>();
    let result = if args.is_empty() { "&'static str" } else { "String" };
    let mut out = String::new();
    out.push_str(&format!(
        "pub fn {name}({}) -> {result} {{ {name}_in(super::current(){forwards}) }}\n",
        params.trim_start_matches(", ")
    ));
    out.push_str(&format!("pub fn {name}_in(cx: &LocaleContext{params}) -> {result} {{ match cx.language() {{\n"));
    for (variant, vals) in &arms {
        out.push_str(&format!("super::Language::{variant} => "));
        if plural {
            out.push_str("match cx.plural(count) {\n");
        }
        let mut ordered: Vec<_> = vals.iter().collect();
        ordered.sort_by_key(|(cat, _)| *cat == "other");
        for (cat, s) in ordered {
            if plural {
                out.push_str(&format!(
                    "{} => ",
                    if cat == "other" {
                        "_".into()
                    } else {
                        format!("icu_plurals::PluralCategory::{}{}", cat[..1].to_uppercase(), &cat[1..])
                    }
                ));
            }
            if args.is_empty() {
                let literal = parts(s)
                    .expect("validated template")
                    .into_iter()
                    .map(|p| match p {
                        Part::Text(t) => t,
                        _ => unreachable!(),
                    })
                    .collect::<String>();
                out.push_str(&format!("{literal:?}"));
            } else {
                out.push_str(&expression(s, &args).expect("validated template"));
            }
            out.push_str(",\n");
        }
        if plural {
            out.push_str("},\n");
        }
    }
    out.push_str("} }\n");
    if args.is_empty() {
        out.push_str(&format!("pub fn {name}_c() -> &'static std::ffi::CStr {{ {name}_c_in(super::current()) }}\npub fn {name}_c_in(cx: &LocaleContext) -> &'static std::ffi::CStr {{ match cx.language() {{\n"));
        for (variant, vals) in &arms {
            let v = parts(&vals[""])
                .expect("validated template")
                .into_iter()
                .map(|p| match p {
                    Part::Text(s) => s,
                    _ => unreachable!(),
                })
                .collect::<String>();
            out.push_str(&format!("super::Language::{variant} => c{v:?},\n"));
        }
        out.push_str("} }\n");
    }
    Ok(KeyCode { code: out, corpus })
}

/// The accessors of an English-only message: no language match, no `_in` variants (a language
/// parameter to a language-independent message is a mistake the type system can reject), and
/// numbers and plural categories from [`super::english`], never the UI language's: Belarusian
/// `plural(21)` is `One`, which would print "21 callback".
fn english_only_code(name: &str, forms: &BTreeMap<String, String>, args: &BTreeMap<String, String>, plural: bool) -> String {
    let params = args
        .iter()
        .map(|(k, v)| format!("{k}: {}", if v == "str" { "&str" } else { "i64" }))
        .collect::<Vec<_>>()
        .join(", ");
    if args.is_empty() {
        let text: String = parts(&forms[""])
            .expect("validated template")
            .into_iter()
            .map(|p| match p {
                Part::Text(t) => t,
                Part::Arg(_) => unreachable!(),
            })
            .collect();
        return format!(
            "pub fn {name}() -> &'static str {{ {text:?} }}\npub fn {name}_c() -> &'static std::ffi::CStr {{ c{text:?} }}\n"
        );
    }
    let mut out = format!("pub fn {name}({params}) -> String {{\n");
    if plural || args.values().any(|v| v == "i64") {
        out.push_str("let cx = super::english();\n");
    }
    if plural {
        out.push_str("match cx.plural(count) {\n");
        let mut ordered: Vec<_> = forms.iter().collect();
        ordered.sort_by_key(|(cat, _)| *cat == "other");
        for (cat, s) in ordered {
            let arm = if cat == "other" {
                "_".to_string()
            } else {
                format!("icu_plurals::PluralCategory::{}{}", cat[..1].to_uppercase(), &cat[1..])
            };
            out.push_str(&format!("{arm} => {},\n", expression(s, args).expect("validated template")));
        }
        out.push_str("}\n");
    } else {
        out.push_str(&expression(&forms[""], args).expect("validated template"));
        out.push('\n');
    }
    out.push_str("}\n");
    out
}

/// The registry as Rust: `Preference`, `Language` and every table over them. `specs` is every
/// shipped language in display order.
fn languages_rs(specs: &[&LanguageSpec]) -> String {
    let n = specs.len();
    let each = |f: &dyn Fn(usize, &LanguageSpec) -> String| -> String {
        specs.iter().enumerate().map(|(i, s)| f(i, s)).collect()
    };
    let mut o = String::from("// Generated from locales/languages.json: do not edit.\n");
    o.push_str("#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]\npub enum Preference {\n    #[default]\n    #[serde(rename = \"system\")]\n    System,\n");
    o.push_str(&each(&|_, s| format!("    #[serde(rename = {:?})]\n    {},\n", s.tag, s.variant)));
    o.push_str("}\n#[derive(Clone, Copy, Debug, Eq, PartialEq)]\npub enum Language {\n");
    o.push_str(&each(&|_, s| format!("    {},\n", s.variant)));
    o.push_str("    /// Generated expanded labels; selectable only by the host simulator.\n    #[cfg_attr(not(any(test, feature = \"test-support\", feature = \"hostsim\")), allow(dead_code))]\n    Pseudo,\n}\n");
    o.push_str(&format!(
        "/// Every language the app ships, `System` excluded: the list every \"each language\" loop and the Settings page read.\npub const SHIPPED: [Preference; {n}] = Preference::SHIPPED;\n"
    ));
    o.push_str(&format!("impl Preference {{\n    pub const SHIPPED: [Preference; {n}] = ["));
    o.push_str(&each(&|_, s| format!("Self::{}, ", s.variant)));
    o.push_str("];\n    pub fn from_tag(s: &str) -> Self {\n        match s {\n");
    o.push_str(&each(&|_, s| format!("            {:?} => Self::{},\n", s.tag, s.variant)));
    o.push_str("            _ => Self::System,\n        }\n    }\n    pub fn tag(self) -> &'static str {\n        match self {\n            Self::System => \"system\",\n");
    o.push_str(&each(&|_, s| format!("            Self::{} => {:?},\n", s.variant, s.tag)));
    o.push_str("        }\n    }\n    pub fn native_name(self) -> &'static str {\n        match self {\n            Self::System => msg::core_system_default(),\n");
    o.push_str(&each(&|_, s| format!("            Self::{} => {:?},\n", s.variant, s.native_name)));
    o.push_str("        }\n    }\n    /// The Language page's row identity: 0 is System, a language keeps the row its registry entry names.\n    pub fn settings_row(self) -> u32 {\n        match self {\n            Self::System => 0,\n");
    o.push_str(&each(&|_, s| format!("            Self::{} => {},\n", s.variant, s.row)));
    o.push_str("        }\n    }\n    pub fn language(self) -> Option<Language> {\n        match self {\n            Self::System => None,\n");
    o.push_str(&each(&|_, s| format!("            Self::{0} => Some(Language::{0}),\n", s.variant)));
    o.push_str("        }\n    }\n    /// Process-local only (never stored): 0 is System, otherwise position in `SHIPPED` plus one.\n    fn slot(self) -> u8 {\n        match self {\n            Self::System => 0,\n");
    o.push_str(&each(&|i, s| format!("            Self::{} => {},\n", s.variant, i + 1)));
    o.push_str("        }\n    }\n    fn from_slot(slot: u8) -> Self {\n        match slot {\n");
    o.push_str(&each(&|i, s| format!("            {} => Self::{},\n", i + 1, s.variant)));
    o.push_str("            _ => Self::System,\n        }\n    }\n}\n");
    o.push_str(&format!("impl Language {{\n    pub const SHIPPED: [Language; {n}] = ["));
    o.push_str(&each(&|_, s| format!("Self::{}, ", s.variant)));
    o.push_str("];\n    pub fn tag(self) -> &'static str {\n        match self {\n");
    o.push_str(&each(&|_, s| format!("            Self::{} => {:?},\n", s.variant, s.tag)));
    o.push_str("            Self::Pseudo => \"en\",\n        }\n    }\n    fn regional(self) -> &'static str {\n        match self {\n");
    o.push_str(&each(&|_, s| format!("            Self::{} => {:?},\n", s.variant, s.region)));
    o.push_str("            Self::Pseudo => \"en-US\",\n        }\n    }\n    fn parse(s: &str) -> Self {\n        let Ok(l) = s.parse::<Locale>() else {\n            return Self::En;\n        };\n        match l.id.language.as_str() {\n");
    o.push_str(&each(&|_, s| {
        let excluded = s
            .excluded_regions
            .iter()
            .map(|r| format!(" && l.id.region.is_none_or(|r| r.as_str() != {r:?})"))
            .collect::<String>();
        format!(
            "            {:?} if l.id.script.is_none_or(|s| s.as_str() == {:?}){excluded} => Self::{},\n",
            s.tag, s.script, s.variant
        )
    }));
    o.push_str("            _ => Self::En,\n        }\n    }\n    /// Position in `SHIPPED`; `Pseudo` follows the last.\n    pub fn index(self) -> usize {\n        match self {\n");
    o.push_str(&each(&|i, s| format!("            Self::{} => {i},\n", s.variant)));
    o.push_str(&format!("            Self::Pseudo => {n},\n        }}\n    }}\n"));
    o.push_str("    /// The plural categories each language's registry entry declares; a test compares them with ICU's own.\n    #[cfg(any(test, feature = \"test-support\"))]\n    pub const PLURALS: &[(Language, &[&str])] = &[\n");
    o.push_str(&each(&|_, s| format!(
        "        (Language::{}, &[{}]),\n",
        s.variant,
        s.plurals.iter().map(|p| format!("{p:?}")).collect::<Vec<_>>().join(", ")
    )));
    o.push_str("    ];\n}\n");
    o
}

pub fn compile(source: &Catalog, langs: &[Catalog]) -> Result<Generated, Vec<String>> {
    let mut errors = Vec::new();
    // An invalid flag is reported by `key_code` below; for the key sets it reads as translatable.
    let (expected, english_only): (BTreeSet<&str>, BTreeSet<&str>) = {
        let mut split = (BTreeSet::new(), BTreeSet::new());
        for (k, v) in &source.entries {
            if translatable(v).unwrap_or(true) { split.0.insert(k.as_str()); } else { split.1.insert(k.as_str()); }
        }
        split
    };
    for l in langs {
        errors.extend(check_keys(l, &expected, &english_only));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut messages =
        String::from("// Generated from locales: do not edit.\nuse super::LocaleContext;\n");
    let mut symbols = BTreeSet::new();
    let mut corpus = Vec::new();
    for key in source.entries.keys() {
        let name = key.replace('.', "_");
        for suffix in ["", "_in", "_c", "_c_in"] {
            if !symbols.insert(format!("{name}{suffix}")) {
                errors.push(format!("{}: {key}: colliding accessor {name}", source.origin[key]));
            }
        }
        match key_code(key, source, langs) {
            Ok(k) => {
                messages.push_str(&k.code);
                corpus.extend(k.corpus);
            }
            Err(e) => errors.extend(e),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut all: Vec<&LanguageSpec> =
        std::iter::once(&source.spec).chain(langs.iter().map(|l| &l.spec)).collect();
    all.sort_by_key(|s| s.row);
    corpus.extend(all.iter().map(|s| s.native_name.clone()));
    messages.push_str("#[cfg(test)] pub const CORPUS: &[&str] = &[\n");
    for s in corpus {
        messages.push_str(&format!("{s:?},\n"));
    }
    messages.push_str("];\n");
    Ok(Generated { messages, languages: languages_rs(&all) })
}

/// Discover `locales/*/` against the registry. A directory nothing reads, or a registered
/// language with nothing to read, is an error; a leftover directory holding no catalog files is
/// only a warning.
pub fn discover(root: &Path, specs: &[LanguageSpec]) -> Result<Vec<Catalog>, Vec<String>> {
    let mut errors = Vec::new();
    let mut present = BTreeSet::new();
    let entries = catalog_files(root).map_err(|e| vec![e])?;
    for p in entries {
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_file() {
            if name != "languages.json" {
                errors.push(format!("locales/{name}: stray file; only languages.json and one directory per language belong in locales"));
            }
            continue;
        }
        let has_catalog = catalog_files(&p)
            .map_err(|e| vec![e])?
            .iter()
            .any(|f| f.extension().and_then(|s| s.to_str()) == Some("json"));
        let registered = specs.iter().any(|s| s.tag == name);
        if !has_catalog && !registered {
            println!("cargo:warning=locales/{name}: no catalog files; delete the leftover directory");
        } else if !registered {
            errors.push(format!(
                "locales/{name}: directory is not a registered language; add {{\"tag\":\"{name}\",...}} to locales/languages.json ({GUIDE})"
            ));
        } else if !has_catalog {
            errors.push(format!("locales/{name}: registered in languages.json but holds no *.json catalog files"));
        } else {
            present.insert(name);
        }
    }
    for s in specs {
        if !present.contains(&s.tag) && !errors.iter().any(|e| e.starts_with(&format!("locales/{}:", s.tag))) {
            errors.push(format!(
                "locales/{}: registered in languages.json but the directory is missing; create it or remove the entry ({GUIDE})",
                s.tag
            ));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    let mut catalogs = Vec::new();
    for s in specs {
        match read_locale(root, s) {
            Ok(c) => catalogs.push(c),
            Err(e) => errors.extend(e),
        }
    }
    if errors.is_empty() { Ok(catalogs) } else { Err(errors) }
}

pub fn build(root: &Path, out: &Path) -> Result<(), Vec<String>> {
    let text = std::fs::read_to_string(root.join("languages.json"))
        .map_err(|e| vec![format!("locales/languages.json: {e}; every UI language is registered there ({GUIDE})")])?;
    let specs = parse_registry(&text)?;
    let mut catalogs = discover(root, &specs)?;
    let at = catalogs.iter().position(|c| c.spec.tag == "en").expect("registry requires en");
    let source = catalogs.remove(at);
    let generated = compile(&source, &catalogs)?;
    let write = |name: &str, text: &str| {
        std::fs::write(out.join(name), text).map_err(|e| vec![format!("{name}: {e}")])
    };
    write("messages.rs", &generated.messages)?;
    write("languages.rs", &generated.languages)
}
#[cfg(test)]
mod tests {
    use super::*;
    const REGISTRY: &str = r#"[
      {"tag":"en","native_name":"English","region":"en-US","script":"Latn","plurals":["one","other"],"row":1},
      {"tag":"es","native_name":"Español","region":"es-ES","script":"Latn","plurals":["one","many","other"],"row":2},
      {"tag":"be","native_name":"Беларуская","region":"be-BY","script":"Cyrl","plurals":["one","few","many","other"],"row":3},
      {"tag":"fr","native_name":"Français","region":"fr-FR","script":"Latn","plurals":["one","many","other"],"row":5}
    ]"#;
    fn specs() -> Vec<LanguageSpec> {
        parse_registry(REGISTRY).unwrap()
    }
    fn catalog(spec: &LanguageSpec, entries: &[(&str, Value)]) -> Catalog {
        Catalog {
            spec: spec.clone(),
            entries: entries.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
            origin: entries.iter().map(|(k, _)| (k.to_string(), format!("{}/x.json", spec.tag))).collect(),
        }
    }
    /// English source plus es, be, fr translating `test.label` (one text message).
    fn catalogs() -> (Catalog, Vec<Catalog>) {
        let specs = specs();
        let en = catalog(
            &specs[0],
            &[("test.label", serde_json::json!({"value":"Hello {name}","args":{"name":"str"},"description":"Greeting"}))],
        );
        let tr = |i: usize, t: &str| catalog(&specs[i], &[("test.label", serde_json::json!(t))]);
        (en, vec![tr(1, "Hola {name}"), tr(2, "Вітаем, {name}"), tr(3, "Bonjour {name}")])
    }
    fn errors_of(r: Result<Generated, Vec<String>>) -> String {
        r.err().expect("expected errors").join("\n")
    }
    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("plx-catalog-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
    fn write_locale(root: &Path, tag: &str, body: &str) {
        std::fs::create_dir_all(root.join(tag)).unwrap();
        std::fs::write(root.join(tag).join("x.json"), body).unwrap();
    }
    /// A locales root registering en/es/be/fr, each holding one valid key.
    fn locales_root(name: &str) -> std::path::PathBuf {
        let root = temp_root(name);
        std::fs::write(root.join("languages.json"), REGISTRY).unwrap();
        write_locale(&root, "en", r#"{"test.label":{"value":"Hello","description":"Greeting"}}"#);
        for tag in ["es", "be", "fr"] {
            write_locale(&root, tag, r#"{"test.label":"Hola"}"#);
        }
        root
    }
    fn build_in(root: &Path) -> Result<(), Vec<String>> {
        let out = root.with_extension("out");
        std::fs::create_dir_all(&out).unwrap();
        let result = build(root, &out);
        let _ = std::fs::remove_dir_all(&out);
        result
    }
    #[test]
    fn an_unread_locale_directory_is_an_error() {
        let root = locales_root("unread");
        write_locale(&root, "xx", r#"{"test.label":"Hola"}"#);
        let result = build_in(&root);
        let _ = std::fs::remove_dir_all(&root);
        let e = result.expect_err("a locales/xx directory nothing reads must fail the build").join("\n");
        assert!(e.contains("locales/xx: directory is not a registered language"), "{e}");
        assert!(e.contains("locales/languages.json"), "{e}");
    }
    #[test]
    fn leftovers_and_dot_files_are_not_languages() {
        let root = locales_root("leftover");
        std::fs::create_dir_all(root.join("yy")).unwrap();
        std::fs::write(root.join("yy/.DS_Store"), "x").unwrap();
        std::fs::write(root.join("en/.DS_Store"), "x").unwrap();
        std::fs::write(root.join(".DS_Store"), "x").unwrap();
        let result = build_in(&root);
        let _ = std::fs::remove_dir_all(&root);
        result.expect("a dot-file-only directory and stray dot-files do not fail the build");
    }
    #[test]
    fn registry_and_directories_must_agree() {
        let root = locales_root("missing-dir");
        std::fs::remove_dir_all(root.join("fr")).unwrap();
        write_locale(&root, "zz", r#"{"test.label":"x"}"#);
        std::fs::write(root.join("stray.json"), "{}").unwrap();
        std::fs::write(root.join("be/x.json.orig"), "{}").unwrap();
        let e = build_in(&root).expect_err("disagreement").join("\n");
        let _ = std::fs::remove_dir_all(&root);
        assert!(e.contains("locales/fr: registered in languages.json but the directory is missing"), "{e}");
        assert!(e.contains("locales/zz: directory is not a registered language"), "{e}");
        assert!(e.contains("locales/stray.json: stray file"), "{e}");
        let root = locales_root("stray-file");
        std::fs::write(root.join("be/x.json.orig"), "{}").unwrap();
        let e = build_in(&root).expect_err("stray file in a locale").join("\n");
        let _ = std::fs::remove_dir_all(&root);
        assert!(e.contains("be/x.json.orig: not a catalog file"), "{e}");
    }
    #[test]
    fn a_seventh_language_is_data_only() {
        let root = locales_root("seventh");
        let registry = REGISTRY.replace(
            "\"row\":5}\n    ]",
            "\"row\":5},\n      {\"tag\":\"it\",\"native_name\":\"Italiano\",\"region\":\"it-IT\",\"script\":\"Latn\",\"plurals\":[\"one\",\"many\",\"other\"],\"row\":6}\n    ]",
        );
        std::fs::write(root.join("languages.json"), registry).unwrap();
        write_locale(&root, "it", r#"{"test.label":"Ciao"}"#);
        let out = temp_root("seventh-out");
        build(&root, &out).expect("registered plus catalog directory builds");
        let languages = std::fs::read_to_string(out.join("languages.rs")).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&out);
        assert!(languages.contains("Self::It => 6"), "{languages}");
        assert!(languages.contains("SHIPPED: [Preference; 5]"), "{languages}");
    }
    #[test]
    fn the_registry_is_validated() {
        let bad = |from: &str, to: &str| parse_registry(&REGISTRY.replace(from, to)).err().expect("rejected").join("\n");
        assert!(bad("\"row\":5", "\"row\":3").contains("duplicate row 3"));
        assert!(bad("\"row\":5", "\"row\":0").contains("row 0 is System"));
        assert!(bad("\"row\":5", "\"row\":1000").contains("reserved for the page's non-language rows"));
        assert!(bad("\"tag\":\"fr\"", "\"tag\":\"es\"").contains("duplicate tag"));
        assert!(bad("\"script\":\"Cyrl\"", "\"script\":\"Arab\"").contains("renderer"));
        assert!(bad("\"tag\":\"fr\"", "\"tag\":\"pt-BR\"").contains("matcher work"));
        assert!(bad("\"tag\":\"fr\"", "\"tag\":\"qps\"").contains("reserved"));
        assert!(bad("\"fr-FR\"", "\"de-DE\"").contains("region must be"));
        assert!(bad("[\"one\",\"many\",\"other\"],\"row\":5", "[\"one\",\"many\"],\"row\":5").contains("include other"));
        assert!(bad("\"row\":3", "\"row\":9").contains("row order"));
        assert!(parse_registry(&REGISTRY.replace("\"tag\":\"en\"", "\"tag\":\"nl\"")).err().unwrap().join("\n").contains("English"));
        assert_eq!(specs()[3].variant, "Fr");
    }
    #[test]
    fn excluded_regions_are_validated_and_generate_the_matcher_guard() {
        let with = |list: &str| REGISTRY.replace("\"row\":5", &format!("\"excluded_regions\":{list},\"row\":5"));
        let errors = |list: &str| parse_registry(&with(list)).err().expect("rejected").join("\n");
        assert!(errors("\"BR\"").contains("excluded_regions must be an array"));
        assert!(errors("[\"br\"]").contains("two upper-case letters"));
        assert!(errors("[\"BRA\"]").contains("two upper-case letters"));
        assert!(errors("[\"BR\",\"BR\"]").contains("lists a region twice"));
        let specs = parse_registry(&with("[\"CA\"]")).expect("valid");
        assert_eq!(specs[3].excluded_regions, ["CA"]);
        assert!(specs[0].excluded_regions.is_empty());
        let (en, mut langs) = catalogs();
        langs[2].spec.excluded_regions = vec!["CA".into()];
        let languages = compile(&en, &langs).expect("compiles").languages;
        assert!(languages.contains("\"fr\" if l.id.script.is_none_or(|s| s.as_str() == \"Latn\") && l.id.region.is_none_or(|r| r.as_str() != \"CA\") => Self::Fr"), "{languages}");
        assert!(languages.contains("\"es\" if l.id.script.is_none_or(|s| s.as_str() == \"Latn\") => Self::Es"), "{languages}");
    }
    #[test]
    fn a_non_ascii_tag_is_an_error_not_a_panic() {
        let text = REGISTRY.replace("\"tag\":\"fr\"", "\"tag\":\"é\"").replace("\"row\":3", "\"row\":0");
        let errors = parse_registry(&text).err().expect("rejected").join("\n");
        assert!(errors.contains("tag must be 2-3 lower-case letters"), "{errors}");
        assert!(errors.contains("row 0 is System"), "{errors}");
    }
    #[test]
    fn duplicate_keys_are_errors_at_every_depth() {
        assert!(parse(r#"{"a":{"x":1,"x":2}}"#).is_err());
    }
    #[test]
    fn catalog_contract_is_enforced() {
        let (en, langs) = catalogs();
        let good = compile(&en, &langs).unwrap();
        assert!(good.messages.contains("test_label_in"));
        assert!(good.languages.contains("Self::Fr => 5"));
        let mut bad = langs.clone();
        bad[0].entries.clear();
        assert!(errors_of(compile(&en, &bad)).contains("locales/es: test.label: missing"));
        let mut bad = langs.clone();
        bad[1].entries.insert("test.label".into(), serde_json::json!("wrong {other}"));
        assert!(errors_of(compile(&en, &bad)).contains("be/x.json: test.label: placeholder mismatch"));
        let mut bad = langs.clone();
        bad[0].entries.insert("test.label".into(), serde_json::json!("NUL\u{0}{name}"));
        assert!(errors_of(compile(&en, &bad)).contains("es/x.json: test.label"));
        let mut bad = langs;
        bad[2].entries.insert("test.extra".into(), serde_json::json!("x"));
        bad[2].origin.insert("test.extra".into(), "fr/x.json".into());
        assert!(errors_of(compile(&en, &bad)).contains("fr/x.json: test.extra: not in locales/en"));
    }
    /// The slice of generated text holding one accessor: from `pub fn <name>(` to the next `pub fn`
    /// that is not its own `_c` twin.
    fn accessor_text<'a>(messages: &'a str, name: &str) -> &'a str {
        let start = messages.find(&format!("pub fn {name}(")).expect("accessor generated");
        let own_c = format!("pub fn {name}_c(");
        let mut end = messages.len();
        let mut from = start + 1;
        while let Some(i) = messages[from..].find("pub fn ") {
            let at = from + i;
            if !messages[at..].starts_with(&own_c) {
                end = at;
                break;
            }
            from = at + 1;
        }
        &messages[start..end]
    }
    fn with_english_only(en: &mut Catalog) {
        en.entries.insert(
            "test.tech".into(),
            serde_json::json!({"value":"NOTHING demuxed","translate":false,"description":"Panel read-out"}),
        );
        en.origin.insert("test.tech".into(), "en/x.json".into());
    }
    #[test]
    fn english_only_keys_need_no_translation() {
        let (mut en, langs) = catalogs();
        with_english_only(&mut en);
        let g = compile(&en, &langs).expect("an English-only key needs no translator entry");
        let slice = accessor_text(&g.messages, "test_tech");
        assert!(slice.contains("\"NOTHING demuxed\""), "{slice}");
        assert!(slice.contains("c\"NOTHING demuxed\""), "{slice}");
        assert!(!slice.contains("Language::"), "{slice}");
        assert!(!g.messages.contains("test_tech_in") && !g.messages.contains("test_tech_c_in"));
        assert!(g.messages.contains("\"NOTHING demuxed\",\n"), "CORPUS keeps the English text");
        assert!(!g.messages.contains("NÖÖTHÏNG"), "pseudo does not expand it");
    }
    #[test]
    fn english_only_with_arguments_uses_the_english_context() {
        let (mut en, langs) = catalogs();
        en.entries.insert(
            "test.tally".into(),
            serde_json::json!({"value":{"one":"{count} callback","other":"{count} callbacks"},"translate":false,"args":{"count":"i64"},"description":"Panel tally"}),
        );
        en.origin.insert("test.tally".into(), "en/x.json".into());
        let g = compile(&en, &langs).unwrap();
        let slice = accessor_text(&g.messages, "test_tally");
        assert!(slice.contains("super::english()"), "{slice}");
        assert!(!slice.contains("Language::") && !slice.contains("current()"), "{slice}");
        assert!(!g.messages.contains("test_tally_in"));
    }
    #[test]
    fn translator_catalog_with_english_only_key_is_rejected() {
        let (mut en, mut langs) = catalogs();
        with_english_only(&mut en);
        langs[2].entries.insert("test.tech".into(), serde_json::json!("NIČOGA"));
        langs[2].origin.insert("test.tech".into(), "fr/x.json".into());
        let e = errors_of(compile(&en, &langs));
        assert!(e.contains("fr/x.json: test.tech: English-only"), "{e}");
        assert!(e.contains("delete it"), "{e}");
    }
    #[test]
    fn translate_flag_is_english_only_and_false_only() {
        for flag in [serde_json::json!(true), serde_json::json!("no"), serde_json::json!(0)] {
            let (mut en, langs) = catalogs();
            en.entries.get_mut("test.label").unwrap()["translate"] = flag;
            let e = errors_of(compile(&en, &langs));
            assert!(e.contains("translate must be false"), "{e}");
        }
        let (en, mut langs) = catalogs();
        langs[0].entries.insert(
            "test.label".into(),
            serde_json::json!({"value":"Hola {name}","translate":false}),
        );
        let e = errors_of(compile(&en, &langs));
        assert!(e.contains("es/x.json: test.label"), "{e}");
    }
    #[test]
    fn a_wrapped_translation_gets_its_own_message() {
        let (en, mut langs) = catalogs();
        langs[0].entries.insert(
            "test.label".into(),
            serde_json::json!({"value":"Hola {name}","description":"x","args":{"name":"str"}}),
        );
        let e = errors_of(compile(&en, &langs));
        assert!(
            e.contains("es/x.json: test.label: a translation is a bare string or a plural map; \"value\", \"description\", \"args\", \"optional\" and \"translate\" belong only in locales/en"),
            "{e}"
        );
    }
    /// English source for a message with two arguments, `end` optional, and es/be/fr translations.
    fn optional_catalogs(optional: serde_json::Value, es: &str, be: &str, fr: &str) -> (Catalog, Vec<Catalog>) {
        let specs = specs();
        let en = catalog(&specs[0], &[("test.span", serde_json::json!({
            "value": "{start}s", "args": {"start": "str", "end": "str"}, "optional": optional, "description": "A decade"}))]);
        let tr = |i: usize, t: &str| catalog(&specs[i], &[("test.span", serde_json::json!(t))]);
        (en, vec![tr(1, es), tr(2, be), tr(3, fr)])
    }
    #[test]
    fn an_optional_argument_may_be_left_out_by_a_language_that_does_not_need_it() {
        // English never reads `end`; French writes the range with it
        let (en, langs) = optional_catalogs(serde_json::json!(["end"]), "años {start}", "{start}-я", "{start}–{end}");
        let good = compile(&en, &langs).expect("compiles");
        assert!(good.messages.contains("pub fn test_span(end: &str, start: &str) -> String"), "every language is handed both: {}", good.messages);
        // without the declaration the same texts are a mismatch in every language that omits `end`
        let (en, langs) = optional_catalogs(serde_json::json!([]), "años {start}", "{start}-я", "{start}–{end}");
        let e = errors_of(compile(&en, &langs));
        assert!(e.contains("es/x.json: test.span: placeholder mismatch"), "{e}");
        // …but an unknown placeholder is still an error, optional or not
        let (en, langs) = optional_catalogs(serde_json::json!(["end"]), "años {start}", "{start}-я", "{start}–{stop}");
        assert!(errors_of(compile(&en, &langs)).contains("fr/x.json: test.span: placeholder mismatch"));
    }
    #[test]
    fn an_optional_argument_is_a_known_argument_some_language_reads_and_only_english_declares_it() {
        // required `start` is not made optional by listing it: a language that leaves it out is an error
        let (en, langs) = optional_catalogs(serde_json::json!(["end"]), "años", "{start}-я", "{start}–{end}");
        assert!(errors_of(compile(&en, &langs)).contains("es/x.json: test.span: placeholder mismatch"));
        let (en, langs) = optional_catalogs(serde_json::json!(["end", "nope"]), "{start}", "{start}", "{start}–{end}");
        assert!(errors_of(compile(&en, &langs)).contains("optional must name arguments of the message"));
        let (en, langs) = optional_catalogs(serde_json::json!("end"), "{start}", "{start}", "{start}–{end}");
        assert!(errors_of(compile(&en, &langs)).contains("optional must be a list of argument names"));
        // an optional argument no language reads is a dead parameter
        let (en, langs) = optional_catalogs(serde_json::json!(["end"]), "{start}", "{start}", "{start}");
        assert!(errors_of(compile(&en, &langs)).contains("optional argument end is used by no language"));
        // a translation cannot carry the declaration
        let (en, mut langs) = optional_catalogs(serde_json::json!(["end"]), "{start}", "{start}", "{start}–{end}");
        langs[0].entries.insert("test.span".into(), serde_json::json!({"value": "{start}", "optional": ["end"]}));
        assert!(errors_of(compile(&en, &langs)).contains("\"optional\" and \"translate\" belong only in locales/en"));
    }
    #[test]
    fn errors_are_collected_and_capped() {
        let (mut en, mut langs) = catalogs();
        for i in 0..30 {
            let k = format!("test.k{i}");
            en.entries.insert(k.clone(), serde_json::json!({"value":"x","description":"d"}));
            en.origin.insert(k, "en/x.json".into());
        }
        langs[0].entries.clear();
        let e = errors_of(compile(&en, &langs));
        assert!(e.contains("+11 more key errors"), "{e}");
        assert!(e.contains("locales/be: +10 more key errors"), "{e}");
    }
    #[test]
    fn malformed_argument_schema_is_rejected() {
        for args in [
            serde_json::json!([]),
            serde_json::json!(42),
            serde_json::json!(null),
        ] {
            let (mut en, langs) = catalogs();
            en.entries.get_mut("test.label").unwrap()["args"] = args;
            assert!(errors_of(compile(&en, &langs)).contains("args must be an object"));
        }
    }
    #[test]
    fn literal_braces_and_malformed_templates() {
        assert_eq!(
            parts("{{x}} {name}").unwrap(),
            vec![Part::Text("{x} ".into()), Part::Arg("name".into())]
        );
        for s in ["{", "}", "{x:?}", "{0}"] {
            assert!(parts(s).is_err());
        }
    }
    #[test]
    fn plural_categories_are_exact_per_language() {
        let (mut en, mut langs) = catalogs();
        let count = serde_json::json!({"value":{"one":"{count} item","other":"{count} items"},"args":{"count":"i64"},"description":"Count"});
        en.entries.insert("test.label".into(), count);
        let forms = |cats: &[&str]| {
            serde_json::Value::Object(cats.iter().map(|c| (c.to_string(), serde_json::json!("{count}"))).collect())
        };
        langs[0].entries.insert("test.label".into(), forms(&["one", "many", "other"]));
        langs[1].entries.insert("test.label".into(), forms(&["one", "few", "many", "other"]));
        langs[2].entries.insert("test.label".into(), forms(&["one", "many", "other"]));
        assert!(compile(&en, &langs).is_ok());
        langs[1].entries.insert("test.label".into(), forms(&["one", "many", "other"]));
        let e = errors_of(compile(&en, &langs));
        assert!(e.contains("be/x.json: test.label: be needs one, few, many, other; missing few"), "{e}");
        langs[1].entries.insert("test.label".into(), forms(&["one", "few", "many", "other"]));
        langs[0].entries.insert("test.label".into(), forms(&["one", "few", "many", "other"]));
        let e = errors_of(compile(&en, &langs));
        assert!(e.contains("es needs one, many, other; few is never selected for this language"), "{e}");
    }
}
