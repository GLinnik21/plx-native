//! **The lint report: findings as the JSON the plan's "Lint" item names, and as plain text.**
//!
//! `plxnative-sim --hud-lint <file> [--format text|json]` (PR5d) prints through [`json_text`] and
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
//! `/block/10`), a key by its text. Findings at one pointer keep the order they were reported in, EXCEPT that
//! findings carrying a lint [`Context`] sort among themselves by `(code, lang, state, slot, message)` (and
//! come after the context-free ones at that pointer), so the report does not depend on the order the
//! lint's checks ran in. `nearest` is always an array, empty unless the finding is an unknown name.
//!
//! **Context.** A finding the lint produced against a compiled package carries a [`Context`]; its JSON
//! gains a `context` object holding only the keys that are set: `lang`, `state`, `slot`, `rects` (each
//! `{name, x, y, w, h}`) and `text` (`{string, measured, room}`). A finding with no context prints
//! exactly the seven keys above and nothing else. Object keys come out in `serde_json`'s sorted order
//! (`Map` is a `BTreeMap`; there is no insertion-order feature in this build), which is deterministic: so
//! `context` sits between `code` and `hint`, and its own keys read `lang, rects, slot, state, text`.
//!
//! The text format prints the context that tells findings apart, `lang`, `state` and `slot`, in brackets after
//! the message (` [de, compact, discs]`, the keys that are set): with `--lang all` a finding whose rects differ
//! between languages prints one line per language, and the lines must differ. The rects and the measured text
//! are the JSON's; a lint message names the string itself.
//!
//! This module is PURE: findings in, text out.

use std::cmp::Ordering;

use plx_ui::rec::strict_json::{Map, Number, Value};

use super::{Context, Finding};

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

/// What orders findings at ONE pointer: nothing for a finding without a context (they keep the order they
/// were reported in), else `(code, lang, state, slot, message)`; a finding without a context comes first.
fn tie_break(a: &Finding, b: &Finding) -> Ordering {
    type Key<'a> = Option<(&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>, &'a str)>;
    fn key(f: &Finding) -> Key<'_> {
        (f.ctx != Context::default()).then(|| {
            (
                f.code,
                f.ctx.lang.as_deref(),
                f.ctx.state.as_deref(),
                f.ctx.slot.as_deref(),
                f.message.as_str(),
            )
        })
    }
    key(a).cmp(&key(b))
}

/// The findings in report order (see the module doc). The sort is stable.
pub fn sorted(findings: &[Finding]) -> Vec<&Finding> {
    let mut out: Vec<&Finding> = findings.iter().collect();
    out.sort_by(|a, b| pointer_order(&a.pointer, &b.pointer).then_with(|| tie_break(a, b)));
    out
}

/// [`sorted`], owned: what the lint returns.
pub fn sorted_owned(findings: &[Finding]) -> Vec<Finding> {
    sorted(findings).into_iter().cloned().collect()
}

/// A coordinate as the shortest decimal that reads back as the same `f32` (`0.1`, not
/// `0.10000000149011612`); `null` for a value JSON cannot carry.
fn number(x: f32) -> Value {
    x.to_string()
        .parse::<f64>()
        .ok()
        .and_then(Number::from_f64)
        .map_or(Value::Null, Value::Number)
}

fn context_json(c: &Context) -> Value {
    let mut map = Map::new();
    for (key, value) in [("lang", &c.lang), ("state", &c.state), ("slot", &c.slot)] {
        if let Some(value) = value {
            map.insert(key.to_owned(), Value::String(value.clone()));
        }
    }
    if !c.rects.is_empty() {
        let rects = c.rects.iter().map(|r| {
            let mut rect = Map::new();
            rect.insert("name".to_owned(), Value::String(r.name.clone()));
            for (key, value) in [("x", r.x), ("y", r.y), ("w", r.w), ("h", r.h)] {
                rect.insert(key.to_owned(), number(value));
            }
            Value::Object(rect)
        });
        map.insert("rects".to_owned(), Value::Array(rects.collect()));
    }
    if let Some(t) = &c.text {
        let mut text = Map::new();
        text.insert("string".to_owned(), Value::String(t.string.clone()));
        text.insert("measured".to_owned(), number(t.measured));
        text.insert("room".to_owned(), number(t.room));
        map.insert("text".to_owned(), Value::Object(text));
    }
    Value::Object(map)
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
    if f.ctx != Context::default() {
        map.insert("context".to_owned(), context_json(&f.ctx));
    }
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
    // the lint's language, state and slot, so the lines of one finding in several languages differ
    let context: Vec<&str> = [&f.ctx.lang, &f.ctx.state, &f.ctx.slot]
        .into_iter()
        .flatten()
        .map(String::as_str)
        .collect();
    if !context.is_empty() {
        out.push_str(&format!(" [{}]", context.join(", ")));
    }
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
    use crate::hud_format::{codes, nearest, Code, NamedRect, TextFact};

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

    /// With `--lang all` a finding whose rects differ between languages prints once per language: the text
    /// lines must tell them apart, so each carries its language, state and slot.
    #[test]
    fn two_findings_that_differ_only_in_language_print_two_different_lines() {
        let found = [
            in_lang(
                &codes::E1001,
                "/layout/block/0",
                Some("de"),
                "compact",
                "a and b overlap",
            ),
            in_lang(
                &codes::E1001,
                "/layout/block/0",
                Some("fr"),
                "compact",
                "a and b overlap",
            ),
        ];
        let out = text("p.plxhud.json", &found);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out:?}");
        assert_ne!(lines[0], lines[1]);
        assert_eq!(
            lines[0],
            "p.plxhud.json#/layout/block/0: error HUD-E1001 z-overlap: a and b overlap [de, compact, discs] | hint: h"
        );
        assert!(lines[1].contains("[fr, compact, discs]"), "{}", lines[1]);
        // a finding that is the same in every language has no language to print, and one with no context
        // prints no brackets at all
        let free = Finding::new(&codes::E1001, "/x", "m", "h").with(Context {
            state: Some("info".to_owned()),
            ..Context::default()
        });
        assert_eq!(text("f", &[free]), "f#/x: error HUD-E1001 z-overlap: m [info] | hint: h\n");
        assert!(!text("f", &[finding(&codes::E0104, "/a", "m")]).contains('['));
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

    // ---- the lint context ----

    fn rect(name: &str, x: f32, y: f32, w: f32, h: f32) -> NamedRect {
        NamedRect { name: name.to_owned(), x, y, w, h }
    }

    fn in_lang(code: &Code, pointer: &str, lang: Option<&str>, state: &str, message: &str) -> Finding {
        Finding::new(code, pointer, message, "h").with(Context {
            lang: lang.map(str::to_owned),
            state: Some(state.to_owned()),
            slot: Some("discs".to_owned()),
            ..Context::default()
        })
    }

    fn keys(value: &Value) -> Vec<&str> {
        value.as_object().unwrap().keys().map(String::as_str).collect()
    }

    #[test]
    fn a_finding_without_context_prints_exactly_the_seven_keys() {
        let json = to_json("f", &[finding(&codes::E0104, "/a", "m")]);
        assert_eq!(keys(&json["findings"][0]), ["code", "hint", "message", "nearest", "path", "rule", "severity"]);
        // An empty context is no context.
        let empty = Finding::new(&codes::E0104, "/a", "m", "h").with(Context::default());
        assert_eq!(to_json("f", &[empty])["findings"][0].get("context"), None);
    }

    #[test]
    fn a_context_prints_only_the_keys_it_sets_in_sorted_order() {
        let lang_only = Finding::new(&codes::E0601, "/a", "m", "h")
            .with(Context { lang: Some("de".to_owned()), ..Context::default() });
        let json = to_json("f", &[lang_only]);
        let found = &json["findings"][0];
        assert_eq!(keys(found), ["code", "context", "hint", "message", "nearest", "path", "rule", "severity"]);
        assert_eq!(keys(&found["context"]), ["lang"]);
        assert_eq!(found["context"]["lang"], "de");

        let full = Finding::new(&codes::E0601, "/a", "m", "h").with(Context {
            lang: Some("de".to_owned()),
            state: Some("compact".to_owned()),
            slot: Some("up_next".to_owned()),
            rects: vec![rect("tab", 10.0, 20.5, 0.1, 40.0)],
            text: Some(TextFact { string: "Kapitel".to_owned(), measured: 130.25, room: 120.0 }),
        });
        let json = to_json("f", &[full]);
        let context = &json["findings"][0]["context"];
        assert_eq!(keys(context), ["lang", "rects", "slot", "state", "text"]);
        assert_eq!(context["state"], "compact");
        assert_eq!(context["slot"], "up_next");
        assert_eq!(keys(&context["rects"][0]), ["h", "name", "w", "x", "y"]);
        assert_eq!(context["rects"][0]["name"], "tab");
        assert_eq!(context["rects"][0]["w"].as_f64(), Some(0.1), "the shortest decimal of the f32, not 0.10000000149011612");
        assert_eq!(context["rects"][0]["y"].as_f64(), Some(20.5));
        assert_eq!(keys(&context["text"]), ["measured", "room", "string"]);
        assert_eq!(context["text"]["string"], "Kapitel");
        assert_eq!(context["text"]["measured"].as_f64(), Some(130.25));
        assert_eq!(context["text"]["room"].as_f64(), Some(120.0));
    }

    #[test]
    fn a_coordinate_json_cannot_carry_prints_null() {
        assert_eq!(number(f32::NAN), Value::Null);
        assert_eq!(number(f32::INFINITY), Value::Null);
        assert_eq!(number(764.0).as_f64(), Some(764.0));
    }

    #[test]
    fn findings_with_a_context_tie_break_by_code_lang_state_slot_message() {
        let at = "/layout/block/0";
        let found = [
            in_lang(&codes::E1002, at, Some("de"), "compact", "b"),
            in_lang(&codes::E1001, at, Some("fr"), "compact", "b"),
            in_lang(&codes::E1001, at, Some("de"), "info", "b"),
            in_lang(&codes::E1001, at, Some("de"), "compact", "z"),
            in_lang(&codes::E1001, at, Some("de"), "compact", "a"),
            in_lang(&codes::E1001, at, None, "compact", "b"),
            finding(&codes::E0105, at, "context-free, sorts first"),
        ];
        let order: Vec<String> = sorted(&found)
            .iter()
            .map(|f| {
                format!(
                    "{} {} {} {}",
                    f.code,
                    f.ctx.lang.as_deref().unwrap_or("-"),
                    f.ctx.state.as_deref().unwrap_or("-"),
                    f.message
                )
            })
            .collect();
        assert_eq!(
            order,
            [
                "HUD-E0105 - - context-free, sorts first",
                "HUD-E1001 - compact b",
                "HUD-E1001 de compact a",
                "HUD-E1001 de compact z",
                "HUD-E1001 de info b",
                "HUD-E1001 fr compact b",
                "HUD-E1002 de compact b",
            ]
        );
        // The same findings in any order give the same report.
        let mut reversed = found.to_vec();
        reversed.reverse();
        assert_eq!(json_text("f", &found), json_text("f", &reversed));
        // The pointer still comes first.
        let earlier = in_lang(&codes::E1003, "/layout/anchor", Some("zz"), "compact", "m");
        let both = [found[0].clone(), earlier];
        assert_eq!(sorted(&both)[0].pointer, "/layout/anchor");
    }

    #[test]
    fn the_owned_sort_is_the_borrowed_one() {
        let found = [finding(&codes::E0104, "/b", "x"), finding(&codes::E0104, "/a", "y")];
        let owned = sorted_owned(&found);
        let borrowed: Vec<&Finding> = sorted(&found);
        assert_eq!(owned.iter().collect::<Vec<_>>(), borrowed);
        assert_eq!(owned[0].pointer, "/a");
    }
}
