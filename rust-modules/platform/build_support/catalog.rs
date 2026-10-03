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
fn read_locale(root: &Path, locale: &str) -> Result<BTreeMap<String, Value>, String> {
    let mut all = BTreeMap::new();
    for entry in std::fs::read_dir(root.join(locale)).map_err(|e| e.to_string())? {
        let p = entry.map_err(|e| e.to_string())?.path();
        if p.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let v = parse(&std::fs::read_to_string(&p).map_err(|e| e.to_string())?)
            .map_err(|e| format!("{}: {e}", p.display()))?;
        for (k, v) in v.as_object().ok_or("catalog must be an object")? {
            if all.insert(k.clone(), v.clone()).is_some() {
                return Err(format!("duplicate catalog key {k}"));
            }
        }
    }
    Ok(all)
}
pub fn compile(catalogs: &[BTreeMap<String, Value>; 3]) -> Result<String, String> {
    let keys: BTreeSet<_> = catalogs[0].keys().collect();
    for (i, c) in catalogs.iter().enumerate() {
        if c.keys().collect::<BTreeSet<_>>() != keys {
            return Err(format!("catalog {i} keys differ from English"));
        }
    }
    let mut out =
        String::from("// Generated from locales: do not edit.\nuse super::LocaleContext;\n");
    let mut symbols = BTreeSet::new();
    let mut corpus = Vec::new();
    for (key, source) in &catalogs[0] {
        if !key.contains('.') || !key.split('.').all(ident) {
            return Err(format!("invalid scoped key {key}"));
        }
        let name = key.replace('.', "_");
        for suffix in ["", "_in", "_c", "_c_in"] {
            if !symbols.insert(format!("{name}{suffix}")) {
                return Err(format!("colliding accessor {name}"));
            }
        }
        if source.get("args").is_some_and(|v| !v.is_object()) {
            return Err(format!("{key}: args must be an object"));
        }
        let args: BTreeMap<String, String> = source
            .get("args")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        Ok((
                            k.clone(),
                            v.as_str().ok_or("argument type must be str or i64")?.into(),
                        ))
                    })
                    .collect::<Result<_, String>>()
            })
            .transpose()?
            .unwrap_or_default();
        for (k, v) in &args {
            if !ident(k)
                || ["cx", "self", "type", "match", "ref"].contains(&k.as_str())
                || !["str", "i64"].contains(&v.as_str())
            {
                return Err(format!("invalid argument {k}: {v}"));
            }
        }
        if source
            .get("description")
            .and_then(Value::as_str)
            .is_none_or(|s| s.trim().is_empty())
        {
            return Err(format!("{key}: missing translator context"));
        }
        let values = [
            forms(
                source
                    .get("value")
                    .ok_or_else(|| format!("{key}: missing value"))?,
            )?,
            forms(&catalogs[1][key])?,
            forms(&catalogs[2][key])?,
        ];
        let plural = !values[0].contains_key("");
        if plural && args.get("count").map(String::as_str) != Some("i64") {
            return Err(format!("{key}: plurals require count:i64"));
        }
        let argkeys: BTreeSet<_> = args.keys().cloned().collect();
        for (i, fs) in values.iter().enumerate() {
            if plural == fs.contains_key("") {
                return Err(format!("{key}: message kind differs"));
            }
            if plural {
                for required in [
                    vec!["one", "other"],
                    vec!["one", "many", "other"],
                    vec!["one", "few", "many", "other"],
                ][i]
                    .iter()
                {
                    if !fs.contains_key(*required) {
                        return Err(format!("{key}: locale {i} missing {required}"));
                    }
                }
            }
            for s in fs.values() {
                let used: BTreeSet<_> = parts(s)?
                    .into_iter()
                    .filter_map(|p| match p {
                        Part::Arg(k) => Some(k),
                        _ => None,
                    })
                    .collect();
                if used.iter().any(|k| !argkeys.contains(k))
                    || argkeys
                        .iter()
                        .any(|k| !used.contains(k) && !(plural && k == "count"))
                {
                    return Err(format!(
                        "{key}: placeholder mismatch expected {argkeys:?}, found {used:?}"
                    ));
                }
                corpus.push(s.clone());
            }
        }
        let params = args
            .iter()
            .map(|(k, v)| format!(", {k}: {}", if v == "str" { "&str" } else { "i64" }))
            .collect::<String>();
        let forwards = args.keys().map(|k| format!(", {k}")).collect::<String>();
        let result = if args.is_empty() {
            "&'static str"
        } else {
            "String"
        };
        out.push_str(&format!(
            "pub fn {name}({}) -> {result} {{ {name}_in(super::current(){forwards}) }}\n",
            params.trim_start_matches(", ")
        ));
        out.push_str(&format!("pub fn {name}_in(cx: &LocaleContext{params}) -> {result} {{ match cx.language() {{\n"));
        for (i, lang) in ["En", "Es", "Be", "Pseudo"].iter().enumerate() {
            let vals = if i == 3 {
                values[0]
                    .iter()
                    .map(|(k, v)| (k.clone(), pseudo(v)))
                    .collect()
            } else {
                values[i].clone()
            };
            out.push_str(&format!("super::Language::{lang} => "));
            if plural {
                out.push_str("match cx.plural(count) {\n");
            }
            let mut ordered: Vec<_> = vals.into_iter().collect();
            ordered.sort_by_key(|(cat, _)| cat == "other");
            for (cat, s) in ordered {
                if plural {
                    out.push_str(&format!(
                        "{} => ",
                        if cat == "other" {
                            "_".into()
                        } else {
                            format!(
                                "icu_plurals::PluralCategory::{}",
                                format!("{}{}", cat[..1].to_uppercase(), &cat[1..])
                            )
                        }
                    ));
                }
                if args.is_empty() {
                    let literal = parts(&s)?
                        .into_iter()
                        .map(|p| match p {
                            Part::Text(t) => t,
                            _ => unreachable!(),
                        })
                        .collect::<String>();
                    out.push_str(&format!("{literal:?}"));
                } else {
                    out.push_str(&expression(&s, &args)?);
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
            for (i, lang) in ["En", "Es", "Be", "Pseudo"].iter().enumerate() {
                let v = if i == 3 {
                    pseudo(&values[0][""])
                } else {
                    values[i][""].clone()
                };
                let v = parts(&v)?
                    .into_iter()
                    .map(|p| match p {
                        Part::Text(s) => s,
                        _ => unreachable!(),
                    })
                    .collect::<String>();
                out.push_str(&format!("super::Language::{lang} => c{v:?},\n"));
            }
            out.push_str("} }\n");
        }
    }
    out.push_str("#[cfg(test)] pub const CORPUS: &[&str] = &[\n");
    for s in corpus {
        out.push_str(&format!("{s:?},\n"));
    }
    out.push_str("];\n");
    Ok(out)
}
pub fn build(root: &Path, out: &Path) -> Result<(), String> {
    let catalogs = [
        read_locale(root, "en")?,
        read_locale(root, "es")?,
        read_locale(root, "be")?,
    ];
    std::fs::write(out.join("messages.rs"), compile(&catalogs)?).map_err(|e| e.to_string())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn catalogs() -> [BTreeMap<String, Value>; 3] {
        [
            BTreeMap::from([(
                "test.label".into(),
                serde_json::json!({"value":"Hello {name}","args":{"name":"str"},"description":"Greeting"}),
            )]),
            BTreeMap::from([("test.label".into(), serde_json::json!("Hola {name}"))]),
            BTreeMap::from([("test.label".into(), serde_json::json!("Вітаем, {name}"))]),
        ]
    }
    #[test]
    fn duplicate_keys_are_errors_at_every_depth() {
        assert!(parse(r#"{"a":{"x":1,"x":2}}"#).is_err());
    }
    #[test]
    fn catalog_contract_is_enforced() {
        let good = catalogs();
        assert!(compile(&good).unwrap().contains("test_label_in"));
        let mut bad = good.clone();
        bad[1].clear();
        assert!(compile(&bad).is_err());
        let mut bad = good.clone();
        bad[2].insert("test.label".into(), serde_json::json!("wrong {other}"));
        assert!(compile(&bad).is_err());
        let mut bad = good;
        bad[1].insert("test.label".into(), serde_json::json!("NUL\u{0}{name}"));
        assert!(compile(&bad).is_err());
    }
    #[test]
    fn malformed_argument_schema_is_rejected() {
        for args in [
            serde_json::json!([]),
            serde_json::json!(42),
            serde_json::json!(null),
        ] {
            let mut cs = catalogs();
            cs[0].get_mut("test.label").unwrap()["args"] = args;
            assert!(compile(&cs).unwrap_err().contains("args must be an object"));
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
    fn plural_categories_are_complete() {
        let mut cs = catalogs();
        cs[0].insert("test.label".into(),serde_json::json!({"value":{"one":"{count} item","other":"{count} items"},"args":{"count":"i64"},"description":"Count"}));
        cs[1].insert(
            "test.label".into(),
            serde_json::json!({"one":"{count}","many":"{count}","other":"{count}"}),
        );
        cs[2].insert(
            "test.label".into(),
            serde_json::json!({"one":"{count}","few":"{count}","many":"{count}","other":"{count}"}),
        );
        assert!(compile(&cs).is_ok());
        cs[2]
            .get_mut("test.label")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove("few");
        assert!(compile(&cs).is_err());
    }
}
