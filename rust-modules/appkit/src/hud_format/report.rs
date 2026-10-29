//! **The lint report: findings as the JSON the plan's "Lint" item names, and as plain text.**
//!
//! `plxnative-sim --hud-lint <file> [--format text|json]` (PR5c) prints through [`json_text`] and
//! [`text`]; the registry's CI reads the JSON. The shape is exactly the plan's, and nothing more:
//!
//! ```text
//! {"lint": 1, "file": "<name>", "findings": [
//!   {"code": "HUD-E0301", "severity": "error", "path": "/layout/block/2/cells/0/segment/items/1/command",
//!    "rule": "command-known", "message": "...", "hint": "...", "nearest": ["seek"]}]}
//! ```
//!
//! `path` is [`Finding::pointer`], an RFC 6901 pointer. The findings are sorted by pointer, so one round
//! fixes many: tokens compare as the document reads (an array index by its number, so `/block/2` comes before
//! `/block/10`), a key by its text, and findings at one pointer keep the order they were reported in.
//! `nearest` is always an array, empty unless the finding is an unknown name. Object keys come out in
//! `serde_json`'s sorted order, which is deterministic.
//!
//! The context a compile finding adds (`state`, `slot`, `lang`, `rects`) belongs to PR5c, which has the
//! compiler; a parse finding has none, so this module carries no place for it and invents no value.
//!
//! This module is PURE: findings in, text out.

use std::cmp::Ordering;

use plx_ui::rec::strict_json::{Map, Value};

use super::Finding;

/// The `lint` key's value: the version of this report shape.
pub const REPORT_VERSION: u64 = 1;

/// `a` before `b` as the document reads: token by token of the pointer, an array index by its number and
/// any other token by its text (a number comes before a key at the same depth).
fn pointer_order(a: &str, b: &str) -> Ordering {
    let mut left = a.split('/');
    let mut right = b.split('/');
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => {
                let order = match (index(x), index(y)) {
                    (Some(m), Some(n)) => m.cmp(&n),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

/// An array index token: plain decimal digits.
fn index(token: &str) -> Option<u64> {
    let digits = !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit());
    if !digits {
        return None;
    }
    Some(token.bytes().fold(0u64, |n, b| {
        n.saturating_mul(10).saturating_add(u64::from(b - b'0'))
    }))
}

/// The findings in report order (see the module doc). The sort is stable.
pub fn sorted(findings: &[Finding]) -> Vec<&Finding> {
    let mut out: Vec<&Finding> = findings.iter().collect();
    out.sort_by(|a, b| pointer_order(&a.pointer, &b.pointer));
    out
}

fn finding_json(f: &Finding) -> Value {
    let mut map = Map::new();
    map.insert("code".to_owned(), Value::String(f.code.to_owned()));
    map.insert(
        "severity".to_owned(),
        Value::String(f.severity.as_str().to_owned()),
    );
    map.insert("path".to_owned(), Value::String(f.pointer.clone()));
    map.insert("rule".to_owned(), Value::String(f.rule.to_owned()));
    map.insert("message".to_owned(), Value::String(f.message.clone()));
    map.insert("hint".to_owned(), Value::String(f.hint.clone()));
    map.insert(
        "nearest".to_owned(),
        Value::Array(f.nearest.iter().cloned().map(Value::String).collect()),
    );
    Value::Object(map)
}

/// The report of `findings` for the package called `file`.
pub fn to_json(file: &str, findings: &[Finding]) -> Value {
    let mut map = Map::new();
    map.insert("lint".to_owned(), Value::from(REPORT_VERSION));
    map.insert("file".to_owned(), Value::String(file.to_owned()));
    map.insert(
        "findings".to_owned(),
        Value::Array(sorted(findings).into_iter().map(finding_json).collect()),
    );
    Value::Object(map)
}

/// [`to_json`] as printed: pretty, with one trailing newline.
pub fn json_text(file: &str, findings: &[Finding]) -> String {
    format!("{:#}\n", to_json(file, findings))
}

/// One line of a finding, with no line break in it: a control character of the message becomes a space.
fn line(file: &str, f: &Finding) -> String {
    let mut out = format!(
        "{file}#{}: {} {} {}: {}",
        f.pointer,
        f.severity.as_str(),
        f.code,
        f.rule,
        f.message
    );
    if !f.hint.is_empty() {
        out.push_str(" | hint: ");
        out.push_str(&f.hint);
    }
    if !f.nearest.is_empty() {
        out.push_str(" | nearest: ");
        out.push_str(&f.nearest.join(", "));
    }
    out.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// The report as plain text: one line per finding, in report order, each ending in a newline. A clean
/// package is the empty string.
pub fn text(file: &str, findings: &[Finding]) -> String {
    sorted(findings)
        .into_iter()
        .map(|f| line(file, f) + "\n")
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud_format::{codes, nearest, Code};

    fn finding(code: &Code, pointer: &str, message: &str) -> Finding {
        Finding::new(code, pointer, message, "write it right")
    }

    #[test]
    fn the_json_has_the_shape_of_the_plan() {
        let f = finding(
            &codes::E0301,
            "/layout/block/2/command",
            "unknown command skip_back",
        )
        .with_nearest(nearest("seeq", ["seek", "next"]));
        let json = to_json("my.plxhud.json", &[f]);
        assert_eq!(json["lint"], 1);
        assert_eq!(json["file"], "my.plxhud.json");
        let found = &json["findings"][0];
        let keys: Vec<&str> = found
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["code", "hint", "message", "nearest", "path", "rule", "severity"]
        );
        assert_eq!(found["code"], "HUD-E0301");
        assert_eq!(found["severity"], "error");
        assert_eq!(found["path"], "/layout/block/2/command");
        assert_eq!(found["rule"], "command-known");
        assert_eq!(found["hint"], "write it right");
        assert_eq!(found["nearest"], serde_json_array(&["seek", "next"]));
        assert_eq!(json.as_object().unwrap().len(), 3, "{json:#}");
    }

    fn serde_json_array(names: &[&str]) -> Value {
        Value::Array(
            names
                .iter()
                .map(|n| Value::String((*n).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn a_finding_without_nearest_names_carries_an_empty_array() {
        let json = to_json("f", &[finding(&codes::E0103, "/layout", "missing")]);
        assert_eq!(json["findings"][0]["nearest"], Value::Array(Vec::new()));
    }

    #[test]
    fn a_clean_package_is_an_empty_list_and_an_empty_text() {
        assert_eq!(to_json("f", &[])["findings"], Value::Array(Vec::new()));
        assert_eq!(text("f", &[]), "");
        assert!(json_text("f", &[]).ends_with("}\n"));
    }

    #[test]
    fn findings_come_out_in_document_order_not_text_order() {
        let at = |p: &str| finding(&codes::E0104, p, "x");
        let found = [
            at("/layout/block/10/id"),
            at("/layout/block/2/cells/0"),
            at("/layout/block"),
            at("/layout/anchor"),
            at(""),
            at("/layout/block/2"),
            at("/format"),
        ];
        let order: Vec<&str> = sorted(&found).iter().map(|f| f.pointer.as_str()).collect();
        assert_eq!(
            order,
            [
                "",
                "/format",
                "/layout/anchor",
                "/layout/block",
                "/layout/block/2",
                "/layout/block/2/cells/0",
                "/layout/block/10/id",
            ]
        );
    }

    #[test]
    fn findings_at_one_pointer_keep_the_order_they_were_reported_in() {
        let found = [
            finding(&codes::E0105, "/a", "first"),
            finding(&codes::E0104, "/a", "second"),
            finding(&codes::E0103, "/a", "third"),
        ];
        let order: Vec<&str> = sorted(&found).iter().map(|f| f.message.as_str()).collect();
        assert_eq!(order, ["first", "second", "third"]);
    }

    #[test]
    fn a_number_comes_before_a_key_and_a_huge_index_does_not_overflow() {
        assert_eq!(pointer_order("/a/0", "/a/x"), Ordering::Less);
        assert_eq!(pointer_order("/a/x", "/a/0"), Ordering::Greater);
        assert_eq!(
            pointer_order("/a/99999999999999999999999", "/a/99999999999999999999998"),
            Ordering::Equal,
            "an index past u64 saturates: the sort stays total, and stable"
        );
        assert_eq!(pointer_order("/a", "/a"), Ordering::Equal);
    }

    #[test]
    fn the_text_is_one_line_per_finding() {
        let mut f = finding(&codes::E0102, "/layout/blok", "unknown key\nblok");
        f.nearest = vec!["block".to_owned()];
        let out = text(
            "p.plxhud.json",
            &[f, finding(&codes::E0103, "", "no format")],
        );
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out:?}");
        assert!(out.ends_with('\n'));
        assert_eq!(
            lines[0],
            "p.plxhud.json#: error HUD-E0103 key-required: no format | hint: write it right"
        );
        assert_eq!(
            lines[1],
            "p.plxhud.json#/layout/blok: error HUD-E0102 key-known: unknown key blok | hint: write it right | nearest: block"
        );
    }

    #[test]
    fn a_warning_reads_as_one() {
        let warning = Code {
            id: "HUD-W0999",
            rule: "test-warning",
            severity: crate::hud_format::Severity::Warning,
        };
        let out = json_text("f", &[Finding::new(&warning, "/x", "m", "h")]);
        assert!(out.contains("\"severity\": \"warning\""), "{out}");
        assert!(
            text("f", &[Finding::new(&warning, "/x", "m", "h")]).contains(": warning HUD-W0999 ")
        );
    }

    #[test]
    fn the_report_is_deterministic() {
        let found = [
            finding(&codes::E0104, "/b", "x"),
            finding(&codes::E0104, "/a", "y"),
        ];
        assert_eq!(json_text("f", &found), json_text("f", &found));
        let reversed = [found[1].clone(), found[0].clone()];
        assert_eq!(json_text("f", &found), json_text("f", &reversed));
    }
}
