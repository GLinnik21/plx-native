//! **A small JSON Schema checker for the tests**, with no dependency: it walks ONLY the keywords the plan's
//! "LLM authoring" lists (`type`, `properties`, `required`, `additionalProperties`, `enum`, `const`,
//! `pattern`, `minimum`, `maximum`, single-schema `items`, `oneOf`) plus the local `$ref` into `$defs` the
//! emitter adds, and ignores the annotations. Any other keyword at a schema position is a panic, so the
//! emitter cannot start using one the checker does not understand.
//!
//! It reports a failure the way a stock validator does (Python `jsonschema`): a missing key or an
//! unexpected one at the OBJECT; a wrong value at the value; a `oneOf` that does not hold exactly once at
//! the instance carrying it, never at what failed inside one branch. That is what lets the Agreement rule
//! be tested here: the pointer of a failure equals or is a prefix of the parser's.
//!
//! `pattern` runs [`regex_matches`], a matcher for the subset the table's patterns use: `^` and `$` at the
//! ends, literals, `\.`, classes with ranges, groups with `|`, and `*`, `+`, `?`, `{n}`.

use plx_ui::rec::strict_json::Value;

/// The keywords the emitter may use at a schema position.
pub(super) const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "enum",
    "const",
    "pattern",
    "minimum",
    "maximum",
    "items",
    "oneOf",
    "$ref",
];

/// Annotations and the document's own words: never a constraint.
const IGNORED: &[&str] = &["$schema", "$id", "$defs", "title", "description", "default"];

/// One place the instance does not satisfy the schema.
#[derive(Debug, Clone)]
pub(super) struct Failure {
    pub pointer: String,
    pub why: String,
}

/// Every failure of `instance` against `root` (a whole schema document, `$defs` included).
pub(super) fn check(root: &Value, instance: &Value) -> Vec<Failure> {
    let mut out = Vec::new();
    walk(root, root, instance, "", &mut out);
    out
}

/// Whether the instance satisfies the schema.
pub(super) fn accepts(root: &Value, instance: &Value) -> bool {
    check(root, instance).is_empty()
}

/// Panics if any schema position of `schema` (recursively, `$defs` included) holds a keyword that is neither
/// in [`KEYWORDS`] nor an annotation; and if a `$ref` is not local or does not resolve.
pub(super) fn assert_only_listed_keywords(root: &Value) {
    fn visit(root: &Value, schema: &Value, at: &str) {
        let Value::Object(map) = schema else {
            panic!("{at}: a schema is an object, found {schema}");
        };
        for (key, value) in map {
            let here = format!("{at}/{key}");
            match key.as_str() {
                "properties" => {
                    for (name, sub) in value.as_object().expect("properties is an object") {
                        visit(root, sub, &format!("{here}/{name}"));
                    }
                }
                "items" => visit(root, value, &here),
                "additionalProperties" => {
                    if !value.is_boolean() {
                        visit(root, value, &here);
                    }
                }
                "oneOf" => {
                    for (i, sub) in value
                        .as_array()
                        .expect("oneOf is an array")
                        .iter()
                        .enumerate()
                    {
                        visit(root, sub, &format!("{here}/{i}"));
                    }
                }
                "$defs" => {
                    for (name, sub) in value.as_object().expect("$defs is an object") {
                        visit(root, sub, &format!("{here}/{name}"));
                    }
                }
                "$ref" => {
                    resolve(root, value.as_str().expect("$ref is a string"));
                }
                k if KEYWORDS.contains(&k) || IGNORED.contains(&k) => {}
                other => panic!("{at}: the keyword `{other}` is not one the plan lists"),
            }
        }
    }
    visit(root, root, "");
}

fn resolve<'a>(root: &'a Value, reference: &str) -> &'a Value {
    let name = reference
        .strip_prefix("#/$defs/")
        .unwrap_or_else(|| panic!("`{reference}` is not a local reference into $defs"));
    root.get("$defs")
        .and_then(|defs| defs.get(name))
        .unwrap_or_else(|| panic!("`{reference}` does not resolve"))
}

fn push(out: &mut Vec<Failure>, pointer: &str, why: impl Into<String>) {
    out.push(Failure {
        pointer: pointer.to_owned(),
        why: why.into(),
    });
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// JSON Schema's `integer`: a number with no fractional part (so `1.0` is one).
fn is_integer(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
        }
        _ => false,
    }
}

fn type_holds(want: &str, value: &Value) -> bool {
    match want {
        "integer" => is_integer(value),
        "number" => value.is_number(),
        other => type_name(value) == other,
    }
}

fn walk(root: &Value, schema: &Value, inst: &Value, at: &str, out: &mut Vec<Failure>) {
    let Value::Object(map) = schema else {
        panic!("{at}: a schema is an object, found {schema}");
    };
    for (key, rule) in map {
        match key.as_str() {
            "$ref" => {
                let target = resolve(root, rule.as_str().expect("$ref is a string"));
                walk(root, target, inst, at, out);
            }
            "type" => {
                let want = rule.as_str().expect("type is a string");
                if !type_holds(want, inst) {
                    push(
                        out,
                        at,
                        format!("expected {want}, found {}", type_name(inst)),
                    );
                }
            }
            "enum" => {
                let names = rule.as_array().expect("enum is an array");
                if !names.contains(inst) {
                    push(out, at, format!("{inst} is not one of the enum"));
                }
            }
            "const" => {
                if rule != inst {
                    push(out, at, format!("{inst} is not the constant {rule}"));
                }
            }
            "pattern" => {
                if let Value::String(s) = inst {
                    let regex = rule.as_str().expect("pattern is a string");
                    if !regex_matches(regex, s) {
                        push(out, at, format!("{s:?} does not match {regex}"));
                    }
                }
            }
            "minimum" | "maximum" => {
                if let (Some(n), Some(bound)) = (inst.as_f64(), rule.as_f64()) {
                    let low = key == "minimum";
                    if (low && n < bound) || (!low && n > bound) {
                        push(out, at, format!("{n} is outside the {key} {bound}"));
                    }
                }
            }
            "properties" => {
                if let Value::Object(fields) = inst {
                    for (name, sub) in rule.as_object().expect("properties is an object") {
                        if let Some(value) = fields.get(name) {
                            walk(root, sub, value, &format!("{at}/{}", escape(name)), out);
                        }
                    }
                }
            }
            "required" => {
                if let Value::Object(fields) = inst {
                    for name in rule.as_array().expect("required is an array") {
                        let name = name.as_str().expect("a required name is a string");
                        if !fields.contains_key(name) {
                            push(out, at, format!("the key `{name}` is required"));
                        }
                    }
                }
            }
            "additionalProperties" => {
                if let Value::Object(fields) = inst {
                    let known = map.get("properties").and_then(Value::as_object);
                    for (name, value) in fields {
                        if known.is_some_and(|k| k.contains_key(name)) {
                            continue;
                        }
                        match rule {
                            Value::Bool(false) => {
                                push(out, at, format!("the key `{name}` is not allowed"));
                            }
                            Value::Bool(true) => {}
                            sub => walk(root, sub, value, &format!("{at}/{}", escape(name)), out),
                        }
                    }
                }
            }
            "items" => {
                if let Value::Array(items) = inst {
                    for (i, value) in items.iter().enumerate() {
                        walk(root, rule, value, &format!("{at}/{i}"), out);
                    }
                }
            }
            "oneOf" => {
                let branches = rule.as_array().expect("oneOf is an array");
                let held = branches
                    .iter()
                    .filter(|branch| {
                        let mut inner = Vec::new();
                        walk(root, branch, inst, at, &mut inner);
                        inner.is_empty()
                    })
                    .count();
                if held != 1 {
                    push(
                        out,
                        at,
                        format!("{held} of {} branches of oneOf hold", branches.len()),
                    );
                }
            }
            k if IGNORED.contains(&k) => {}
            other => panic!("{at}: the keyword `{other}` is not one the plan lists"),
        }
    }
}

/// An RFC 6901 token.
fn escape(name: &str) -> String {
    name.replace('~', "~0").replace('/', "~1")
}

/// Whether `pointer` is `prefix` or lies under it (`/a` is a prefix of `/a/b`, `""` of everything, `/a` is
/// not a prefix of `/ab`).
pub(super) fn is_prefix(prefix: &str, pointer: &str) -> bool {
    pointer == prefix || prefix.is_empty() || pointer.starts_with(&format!("{prefix}/"))
}

// ---------------------------------------------------------------------------------------------------
// The regex subset
// ---------------------------------------------------------------------------------------------------

enum Atom {
    Lit(char),
    Class(Vec<(char, char)>),
    Group(Vec<Vec<Item>>),
}

struct Item {
    atom: Atom,
    min: usize,
    max: usize,
}

fn parse_alternatives(chars: &[char], at: &mut usize) -> Vec<Vec<Item>> {
    let mut alternatives = vec![Vec::new()];
    while let Some(&c) = chars.get(*at) {
        let atom = match c {
            ')' => break,
            '|' => {
                *at += 1;
                alternatives.push(Vec::new());
                continue;
            }
            '(' => {
                *at += 1;
                let inner = parse_alternatives(chars, at);
                assert_eq!(chars.get(*at), Some(&')'), "an unclosed group");
                *at += 1;
                Atom::Group(inner)
            }
            '[' => {
                *at += 1;
                let mut ranges = Vec::new();
                while chars.get(*at).is_some_and(|c| *c != ']') {
                    let low = chars[*at];
                    let dash = chars.get(*at + 1) == Some(&'-')
                        && chars.get(*at + 2).is_some_and(|c| *c != ']');
                    if dash {
                        ranges.push((low, chars[*at + 2]));
                        *at += 3;
                    } else {
                        ranges.push((low, low));
                        *at += 1;
                    }
                }
                assert_eq!(chars.get(*at), Some(&']'), "an unclosed class");
                *at += 1;
                Atom::Class(ranges)
            }
            '\\' => {
                let escaped = *chars.get(*at + 1).expect("a dangling backslash");
                *at += 2;
                Atom::Lit(escaped)
            }
            '*' | '+' | '?' | '{' | '}' | ']' | '.' | '^' | '$' => {
                panic!("`{c}` is not in the regex subset the checker runs")
            }
            literal => {
                *at += 1;
                Atom::Lit(literal)
            }
        };
        let (min, max) = match chars.get(*at) {
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let close = chars[*at..]
                    .iter()
                    .position(|c| *c == '}')
                    .expect("an unclosed {");
                let digits: String = chars[*at + 1..*at + close].iter().collect();
                let n: usize = digits.parse().expect("{n} holds one number");
                *at += close;
                (n, n)
            }
            _ => (1, 1),
        };
        if matches!(chars.get(*at), Some('*' | '+' | '?' | '}')) {
            *at += 1;
        }
        alternatives
            .last_mut()
            .expect("there is always one alternative")
            .push(Item { atom, min, max });
    }
    alternatives
}

fn dedup(mut positions: Vec<usize>) -> Vec<usize> {
    positions.sort_unstable();
    positions.dedup();
    positions
}

/// The positions the atom can end at when it starts at any of `starts`.
fn step(atom: &Atom, text: &[char], starts: &[usize]) -> Vec<usize> {
    match atom {
        Atom::Lit(c) => starts
            .iter()
            .filter(|p| text.get(**p) == Some(c))
            .map(|p| p + 1)
            .collect(),
        Atom::Class(ranges) => starts
            .iter()
            .filter(|p| {
                text.get(**p)
                    .is_some_and(|c| ranges.iter().any(|(low, high)| low <= c && c <= high))
            })
            .map(|p| p + 1)
            .collect(),
        Atom::Group(alternatives) => dedup(
            alternatives
                .iter()
                .flat_map(|seq| ends_of_sequence(seq, text, starts.to_vec()))
                .collect(),
        ),
    }
}

fn ends_of_item(item: &Item, text: &[char], starts: Vec<usize>) -> Vec<usize> {
    let mut result = Vec::new();
    let mut frontier = dedup(starts);
    for count in 0..=text.len() + 1 {
        if count >= item.min {
            result.extend(frontier.iter().copied());
        }
        if count >= item.max {
            break;
        }
        frontier = dedup(step(&item.atom, text, &frontier));
        if frontier.is_empty() {
            break;
        }
    }
    dedup(result)
}

fn ends_of_sequence(items: &[Item], text: &[char], starts: Vec<usize>) -> Vec<usize> {
    let mut at = starts;
    for item in items {
        at = ends_of_item(item, text, at);
        if at.is_empty() {
            break;
        }
    }
    at
}

/// Whether `text` matches `pattern`, which must be written `^...$`.
pub(super) fn regex_matches(pattern: &str, text: &str) -> bool {
    let body = pattern
        .strip_prefix('^')
        .and_then(|rest| rest.strip_suffix('$'))
        .unwrap_or_else(|| panic!("`{pattern}` is not anchored with ^ and $"));
    let chars: Vec<char> = body.chars().collect();
    let mut at = 0;
    let alternatives = parse_alternatives(&chars, &mut at);
    assert_eq!(at, chars.len(), "`{pattern}`: an unmatched )");
    let text: Vec<char> = text.chars().collect();
    alternatives
        .iter()
        .any(|seq| ends_of_sequence(seq, &text, vec![0]).contains(&text.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(text: &str) -> Value {
        text.parse().expect("test JSON")
    }

    #[test]
    fn the_regex_subset_matches_what_the_table_patterns_need() {
        let yes = |p: &str, s: &str| assert!(regex_matches(p, s), "{p} should match {s:?}");
        let no = |p: &str, s: &str| assert!(!regex_matches(p, s), "{p} should not match {s:?}");
        yes(r"^abc$", "abc");
        no(r"^abc$", "abcd");
        no(r"^abc$", "ab");
        yes(r"^[a-z][a-z0-9_-]*$", "a");
        yes(r"^[a-z][a-z0-9_-]*$", "a-b_9");
        no(r"^[a-z][a-z0-9_-]*$", "");
        no(r"^[a-z][a-z0-9_-]*$", "A");
        no(r"^[a-z][a-z0-9_-]*$", "a.b");
        yes(r"^[a-z]+(\.[a-z]+)+$", "ab.cd.ef");
        no(r"^[a-z]+(\.[a-z]+)+$", "ab");
        no(r"^[a-z]+(\.[a-z]+)+$", "ab..cd");
        yes(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$", "0.10");
        no(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$", "01.0");
        no(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$", "1.");
        yes(r"^#[0-9a-f]{6}$", "#00ff7a");
        no(r"^#[0-9a-f]{6}$", "#00ff7");
        no(r"^#[0-9a-f]{6}$", "#00ff7aa");
        no(r"^#[0-9a-f]{6}$", "#00FF7A");
        yes(r"^https://[!-~]+$", "https://x.y/z?q=1");
        no(r"^https://[!-~]+$", "https://");
        no(r"^https://[!-~]+$", "https://a b");
        yes(r"^a?b$", "b");
        yes(r"^(a|bc)d$", "bcd");
        no(r"^(a|bc)d$", "bd");
    }

    #[test]
    fn the_checker_walks_every_listed_keyword() {
        let root = json(
            r##"{
                "$defs": { "id": { "type": "string", "pattern": "^[a-z]+$" } },
                "type": "object",
                "properties": {
                    "n": { "type": "integer", "minimum": 1, "maximum": 3 },
                    "id": { "$ref": "#/$defs/id" },
                    "e": { "enum": ["a", "b"] },
                    "k": { "const": "x" },
                    "l": { "type": "array", "items": { "type": "boolean" } },
                    "o": { "oneOf": [ { "type": "string" }, { "type": "integer" } ] },
                    "m": { "type": "object", "additionalProperties": { "type": "string" } }
                },
                "required": ["n"],
                "additionalProperties": false
            }"##,
        );
        assert_only_listed_keywords(&root);
        let ok = |text: &str| assert!(accepts(&root, &json(text)), "{text}");
        let bad = |text: &str, pointer: &str| {
            let found = check(&root, &json(text));
            assert!(
                found.iter().any(|f| f.pointer == pointer),
                "{text}: expected a failure at {pointer:?}, found {found:?}"
            );
        };
        ok(r#"{"n": 2}"#);
        ok(r#"{"n": 2.0, "id": "abc", "e": "a", "k": "x", "l": [true], "o": 5, "m": {"q": "r"}}"#);
        bad(r#"{}"#, "");
        bad(r#"{"n": 0}"#, "/n");
        bad(r#"{"n": 4}"#, "/n");
        bad(r#"{"n": 1.5}"#, "/n");
        bad(r#"{"n": 1, "id": "ABC"}"#, "/id");
        bad(r#"{"n": 1, "e": "c"}"#, "/e");
        bad(r#"{"n": 1, "k": "y"}"#, "/k");
        bad(r#"{"n": 1, "l": [1]}"#, "/l/0");
        bad(r#"{"n": 1, "o": true}"#, "/o");
        bad(r#"{"n": 1, "m": {"q": 1}}"#, "/m/q");
        bad(r#"{"n": 1, "extra": 1}"#, "");
    }

    #[test]
    fn a_one_of_that_holds_twice_fails_and_is_reported_at_the_instance() {
        let root = json(r#"{"oneOf": [ {"type": "integer"}, {"type": "number"} ]}"#);
        assert!(!accepts(&root, &json("1")));
        assert!(accepts(&root, &json("1.5")));
        assert_eq!(check(&root, &json("1"))[0].pointer, "");
    }

    #[test]
    #[should_panic(expected = "not one the plan lists")]
    fn a_keyword_outside_the_list_is_refused() {
        assert_only_listed_keywords(&json(r#"{"type": "array", "maxItems": 3}"#));
    }

    #[test]
    #[should_panic(expected = "does not resolve")]
    fn a_reference_that_does_not_resolve_is_refused() {
        assert_only_listed_keywords(&json(r##"{"$ref": "#/$defs/nowhere", "$defs": {}}"##));
    }

    #[test]
    fn prefixes_are_whole_pointer_tokens() {
        assert!(is_prefix("", "/a"));
        assert!(is_prefix("/a", "/a"));
        assert!(is_prefix("/a", "/a/b"));
        assert!(!is_prefix("/a", "/ab"));
        assert!(!is_prefix("/a/b", "/a"));
    }
}
