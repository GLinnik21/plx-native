//! **Tests of the schema emitter**: the emitted document is walked against the descriptor table (every key,
//! command, parameter, enum value, pattern, range, default and condition is in it, and every object is
//! closed), the regexes it publishes are pinned to the strict parser's own checks, and the whole valid corpus
//! is accepted by it while every invalid row it rejects is rejected where the parser agrees (the plan's
//! "Agreement" rule at test scale). The checker is [`crate::hud_format::schema_check`], which walks only the
//! keywords the plan lists.

use std::collections::BTreeSet;

use plx_ui::rec::strict_json::Value;

use super::{emit, emit_current, emit_text, schema_id, SCHEMA_DRAFT};
use crate::hud_format::parse;
use crate::hud_format::schema::{self, Cmd, Field, Node, NodeKind, Pattern, Ty};
use crate::hud_format::schema_check::{
    accepts, assert_only_listed_keywords, check, is_prefix, regex_matches,
};
use crate::hud_format::tests::{corpus, valid};
use crate::hud_format::{codes, Finding};

const REFERENCE: &str = include_str!("../fixtures/reference.plxhud.json");
const MINIMAL: &str = include_str!("../fixtures/minimal.plxhud.json");
const CLASSIC: &str = crate::hud_format::CLASSIC_PACKAGE;
const HERO_CENTER: &str = include_str!("../fixtures/hero-center.plxhud.json");
const TABS: &str = include_str!("../fixtures/tabs.plxhud.json");
const PALETTE: &str = include_str!("../fixtures/palette.plxhud.json");

fn json(text: &str) -> Value {
    text.parse()
        .unwrap_or_else(|e| panic!("not JSON ({e}): {text:.80}"))
}

fn strs(value: &Value) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("not an array: {value}"))
        .iter()
        .map(|v| {
            v.as_str()
                .unwrap_or_else(|| panic!("not a string: {v}"))
                .to_owned()
        })
        .collect()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|n| (*n).to_owned()).collect()
}

fn keys(value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("not an object: {value}"))
        .keys()
        .cloned()
        .collect()
}

/// A schema with its `$ref` replaced by its target (siblings of the `$ref` laid over it), repeatedly. The
/// children stay as written: the format is recursive, so a full inlining would not end.
fn shallow(root: &Value, schema: &Value) -> Value {
    let mut current = schema.clone();
    while let Some(target) = current
        .get("$ref")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        let name = target.strip_prefix("#/$defs/").expect("a local reference");
        let mut merged = root["$defs"][name]
            .as_object()
            .unwrap_or_else(|| panic!("`{target}` does not resolve"))
            .clone();
        for (key, value) in current.as_object().expect("a schema is an object") {
            if key != "$ref" {
                merged.insert(key.clone(), value.clone());
            }
        }
        current = Value::Object(merged);
    }
    current
}

/// Every schema position of the document, `$defs` included.
fn visit_schemas(schema: &Value, f: &mut impl FnMut(&Value)) {
    f(schema);
    let Value::Object(map) = schema else {
        return;
    };
    for (key, value) in map {
        match key.as_str() {
            "properties" | "$defs" => value
                .as_object()
                .expect("an object of schemas")
                .values()
                .for_each(|sub| visit_schemas(sub, f)),
            "items" => visit_schemas(value, f),
            "additionalProperties" if !value.is_boolean() => visit_schemas(value, f),
            "oneOf" => value
                .as_array()
                .expect("an array of schemas")
                .iter()
                .for_each(|sub| visit_schemas(sub, f)),
            _ => {}
        }
    }
}

fn terms() -> Vec<String> {
    schema::CONDITIONS
        .iter()
        .flat_map(|c| [c.name.to_owned(), format!("!{}", c.name)])
        .collect()
}

fn face_names() -> Vec<String> {
    schema::BUILTIN_ICONS
        .iter()
        .map(|(n, _)| format!("builtin:{n}"))
        .collect()
}

// ---------------------------------------------------------------------------------------------------
// The document
// ---------------------------------------------------------------------------------------------------

#[test]
fn the_schema_names_its_draft_and_its_published_address() {
    let root = emit_current();
    assert_eq!(
        root["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert_eq!(root["$schema"], SCHEMA_DRAFT);
    assert_eq!(root["$id"], "https://plxnative.com/hud/schema/1.0.json");
    assert_eq!(
        schema_id((1, 0)),
        "https://plxnative.com/hud/schema/1.0.json"
    );
    assert_eq!(
        schema_id((2, 13)),
        "https://plxnative.com/hud/schema/2.13.json"
    );
    assert_eq!(root["type"], "object");
}

#[test]
fn the_package_the_table_calls_current_is_1_0_and_emission_is_deterministic() {
    assert_eq!(schema::CURRENT, (1, 0));
    assert_eq!(emit_current(), emit((1, 0)));
    assert_eq!(emit_text((1, 0)), emit_text((1, 0)));
    let text = emit_text((1, 0));
    assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
    assert_eq!(
        json(&text),
        emit_current(),
        "the printed text reads back to the same value"
    );
}

#[test]
fn an_earlier_format_leaves_out_what_arrived_after_it() {
    // Nothing of the table is older than 1.0, so format 0.9 holds none of it: the proof that `since` filters.
    let old = emit((0, 9));
    assert!(keys(&old["properties"]).is_empty());
    assert_eq!(old["$id"], "https://plxnative.com/hud/schema/0.9.json");
    assert!(strs(&old["$defs"]["when"]["oneOf"][0]["enum"]).is_empty());
    assert!(old["$defs"]["item"]["oneOf"].as_array().unwrap().is_empty());
    assert_only_listed_keywords(&old);
}

#[test]
fn only_the_keywords_the_plan_lists_are_used_and_every_reference_resolves() {
    assert_only_listed_keywords(&emit_current());
}

#[test]
fn every_object_is_closed() {
    let root = emit_current();
    let mut objects = 0;
    visit_schemas(&root, &mut |schema| {
        if schema.get("properties").is_some() {
            objects += 1;
            assert_eq!(
                schema.get("additionalProperties"),
                Some(&Value::Bool(false)),
                "an object that names its keys is closed: {schema:#}"
            );
        }
    });
    assert!(objects > 60, "the walk reached the objects: {objects}");
}

#[test]
fn the_defs_are_the_nodes_of_the_table_and_the_shared_vocabularies() {
    let root = emit_current();
    let defs = keys(&root["$defs"]);
    for node in schema::all_nodes() {
        if node.name != schema::ROOT.name {
            assert!(
                defs.contains(node.name),
                "no definition of node `{}`",
                node.name
            );
        }
    }
    for shared in ["ink", "gap", "size", "when", "face_name"] {
        assert!(defs.contains(shared), "no shared definition `{shared}`");
        assert!(
            schema::node(shared).is_none() || shared == "size",
            "a shared definition must not shadow the node `{shared}`"
        );
    }
    assert!(
        schema::node("size").is_none(),
        "`size` is a vocabulary, not a node"
    );
    for name in &defs {
        let used = name == "item" || name == "cell" || {
            let needle = format!("\"#/$defs/{name}\"");
            root.to_string().contains(&needle)
        };
        assert!(used, "the definition `{name}` is never referenced");
    }
}

// ---------------------------------------------------------------------------------------------------
// The table walk
// ---------------------------------------------------------------------------------------------------

/// `got` is the emitted schema of a value of `ty` (a control's `cmd` decides what a face may be).
fn expect_ty(root: &Value, ty: &Ty, cmd: Option<&Cmd>, got: &Value, what: &str) {
    let s = shallow(root, got);
    match ty {
        Ty::Bool => assert_eq!(s["type"], "boolean", "{what}"),
        Ty::Int { min, max } => {
            assert_eq!(s["type"], "integer", "{what}");
            assert_eq!(s["minimum"].as_i64(), Some(*min), "{what}");
            assert_eq!(s["maximum"].as_i64(), Some(*max), "{what}");
        }
        Ty::Num { min, max } => {
            assert_eq!(s["type"], "number", "{what}");
            assert_eq!(s["minimum"].as_f64(), Some(*min), "{what}");
            assert_eq!(s["maximum"].as_f64(), Some(*max), "{what}");
        }
        Ty::Text { .. } => assert_eq!(s["type"], "string", "{what}"),
        Ty::Pattern(p) => {
            assert_eq!(s["type"], "string", "{what}");
            assert_eq!(s["pattern"], p.regex(), "{what}");
        }
        Ty::Enum(list) | Ty::Bind(list) => assert_eq!(strs(&s["enum"]), names(list), "{what}"),
        Ty::EnumList(list) => {
            assert_eq!(s["type"], "array", "{what}");
            assert_eq!(strs(&s["items"]["enum"]), names(list), "{what}");
        }
        Ty::Size => {
            let rungs: Vec<String> = schema::SIZE_RUNGS
                .iter()
                .map(|(n, _)| (*n).to_owned())
                .collect();
            assert_eq!(strs(&s["enum"]), rungs, "{what}");
        }
        Ty::Gap => {
            let tokens: Vec<String> = schema::SPACE_TOKENS
                .iter()
                .map(|(n, _)| (*n).to_owned())
                .collect();
            assert_eq!(strs(&s["oneOf"][0]["enum"]), tokens, "{what}");
            let px = &s["oneOf"][1];
            assert_eq!(px["type"], "integer", "{what}");
            assert_eq!(px["minimum"].as_i64(), Some(0), "{what}");
            assert_eq!(px["maximum"].as_i64(), Some(schema::GAP_TOP_PX), "{what}");
        }
        Ty::Ink => {
            let tokens: Vec<String> = schema::INK_TOKENS
                .iter()
                .map(|(n, _)| (*n).to_owned())
                .collect();
            assert_eq!(strs(&s["oneOf"][0]["enum"]), tokens, "{what}");
            assert_eq!(
                s["oneOf"][1]["pattern"],
                schema::PALETTE_NAME_REGEX,
                "{what}"
            );
        }
        Ty::Asset => assert_eq!(s["pattern"], Pattern::Id.regex(), "{what}"),
        Ty::Face => expect_face(root, cmd, &s, what),
        Ty::Cond => {
            assert_eq!(strs(&s["oneOf"][0]["enum"]), terms(), "{what}");
            assert_eq!(s["oneOf"][1]["type"], "array", "{what}");
            assert_eq!(strs(&s["oneOf"][1]["items"]["enum"]), terms(), "{what}");
        }
        Ty::Object(name) => {
            assert_eq!(got["$ref"], format!("#/$defs/{name}"), "{what}");
            assert!(
                root["$defs"].get(name).is_some(),
                "{what}: no definition `{name}`"
            );
        }
        Ty::List { of, .. } => {
            assert_eq!(s["type"], "array", "{what}");
            expect_ty(root, of, cmd, &s["items"], what);
        }
        Ty::Palette => {
            assert_eq!(s["type"], "object", "{what}");
            assert_eq!(
                s["additionalProperties"]["pattern"],
                schema::HEX_COLOUR_REGEX,
                "{what}"
            );
        }
        Ty::Assets => {
            assert_eq!(s["type"], "object", "{what}");
            assert_eq!(s["additionalProperties"]["type"], "string", "{what}");
        }
        Ty::Command => {
            let commands: Vec<String> =
                schema::COMMANDS.iter().map(|c| c.name.to_owned()).collect();
            assert_eq!(strs(&s["enum"]), commands, "{what}");
        }
        Ty::Either(a, b) => {
            expect_ty(root, a, cmd, &s["oneOf"][0], what);
            expect_ty(root, b, cmd, &s["oneOf"][1], what);
        }
    }
}

fn expect_single_face(root: &Value, got: &Value, what: &str) {
    let s = shallow(root, got);
    assert_eq!(strs(&s["oneOf"][0]["enum"]), face_names(), "{what}");
    assert_eq!(s["oneOf"][1]["pattern"], Pattern::Id.regex(), "{what}");
}

fn expect_face(root: &Value, cmd: Option<&Cmd>, s: &Value, what: &str) {
    match cmd.filter(|c| !c.face_states.is_empty()) {
        None => expect_single_face(root, s, what),
        Some(cmd) => {
            let branches = s["oneOf"]
                .as_array()
                .unwrap_or_else(|| panic!("{what}: {s}"));
            assert_eq!(branches.len(), 2, "{what}");
            expect_single_face(root, &branches[0], what);
            let map = &branches[1];
            let states: BTreeSet<String> =
                cmd.face_states.iter().map(|f| f.key.to_owned()).collect();
            assert_eq!(keys(&map["properties"]), states, "{what}");
            let required: Vec<String> = cmd
                .face_states
                .iter()
                .filter(|f| f.required)
                .map(|f| f.key.to_owned())
                .collect();
            assert_eq!(strs(&map["required"]), required, "{what}");
            assert_eq!(map["additionalProperties"], Value::Bool(false), "{what}");
            for state in cmd.face_states {
                let face = &map["properties"][state.key];
                if state.ladder {
                    assert_eq!(face["type"], "object", "{what}");
                    expect_single_face(root, &face["additionalProperties"], what);
                } else {
                    expect_single_face(root, face, what);
                }
            }
        }
    }
}

fn expect_field(root: &Value, field: &Field, cmd: Option<&Cmd>, got: &Value, what: &str) {
    if field.keyable {
        let both = shallow(root, got);
        let plain = &both["oneOf"][0];
        expect_ty(root, &field.ty, cmd, plain, what);
        let form = &both["oneOf"][1];
        assert_eq!(strs(&form["required"]), names(&["value", "if"]), "{what}");
        assert_eq!(form["additionalProperties"], Value::Bool(false), "{what}");
        assert_eq!(
            keys(&form["properties"]),
            BTreeSet::from(["value".to_owned(), "if".to_owned()]),
            "{what}"
        );
        assert_eq!(
            shallow(root, &form["properties"]["value"]),
            shallow(root, plain),
            "{what}"
        );
        let cases = &form["properties"]["if"];
        assert_eq!(cases["additionalProperties"], Value::Bool(false), "{what}");
        let wanted: BTreeSet<String> = terms().into_iter().collect();
        assert_eq!(
            keys(&cases["properties"]),
            wanted,
            "{what}: every condition, negated too"
        );
        for case in cases["properties"].as_object().unwrap().values() {
            assert_eq!(shallow(root, case), shallow(root, plain), "{what}");
        }
    } else {
        expect_ty(root, &field.ty, cmd, got, what);
    }
    assert_eq!(got["description"], field.doc, "{what}");
    match field.default {
        Some(default) => {
            let want: Value = default
                .parse()
                .unwrap_or_else(|e| panic!("{what}: default `{default}`: {e}"));
            assert_eq!(got["default"], want, "{what}");
        }
        None => assert!(
            got.get("default").is_none(),
            "{what}: a default nobody documented"
        ),
    }
}

fn node_def<'a>(root: &'a Value, node: &Node) -> &'a Value {
    if node.name == schema::ROOT.name {
        root
    } else {
        &root["$defs"][node.name]
    }
}

#[test]
fn every_key_of_every_plain_node_is_in_the_schema_with_its_type_range_default_and_doc() {
    let root = emit_current();
    let mut fields = 0;
    for node in schema::all_nodes() {
        if matches!(node.kind, NodeKind::CommandDispatched) {
            continue;
        }
        let def = node_def(&root, node);
        let at = format!("node `{}`", node.name);
        assert_eq!(def["type"], "object", "{at}");
        assert_eq!(def["description"], node.doc, "{at}");
        assert_eq!(def["additionalProperties"], Value::Bool(false), "{at}");
        let table: BTreeSet<String> = node.fields.iter().map(|f| f.key.to_owned()).collect();
        assert_eq!(
            keys(&def["properties"]),
            table,
            "{at}: the keys are the table's"
        );
        let required: Vec<String> = node
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.key.to_owned())
            .collect();
        match def.get("required") {
            Some(list) => assert_eq!(strs(list), required, "{at}"),
            None => assert!(required.is_empty(), "{at}: required keys are missing"),
        }
        for field in node.fields {
            fields += 1;
            let what = format!("{at}, key `{}`", field.key);
            expect_field(&root, field, None, &def["properties"][field.key], &what);
        }
        match node.kind {
            NodeKind::OneOf(content) => {
                let one: Vec<Value> = content
                    .iter()
                    .map(|k| json(&format!("{{\"required\": [\"{k}\"]}}")))
                    .collect();
                assert_eq!(
                    def["oneOf"],
                    Value::Array(one),
                    "{at}: exactly one content key"
                );
            }
            _ => assert!(def.get("oneOf").is_none(), "{at}"),
        }
    }
    assert!(fields > 100, "the walk reached the table: {fields} keys");
}

#[test]
fn a_control_is_a_one_of_with_one_closed_branch_per_command_keyed_by_const() {
    let root = emit_current();
    let item = schema::node("item").expect("the item node");
    assert!(matches!(item.kind, NodeKind::CommandDispatched));
    let def = &root["$defs"]["item"];
    assert_eq!(def["description"], item.doc);
    let branches = def["oneOf"].as_array().expect("a oneOf");
    assert_eq!(branches.len(), schema::COMMANDS.len());
    let mut parameters = 0;
    for (branch, cmd) in branches.iter().zip(schema::COMMANDS) {
        let at = format!("command `{}`", cmd.name);
        assert_eq!(branch["type"], "object", "{at}");
        assert_eq!(branch["description"], cmd.doc, "{at}");
        assert_eq!(
            branch["additionalProperties"],
            Value::Bool(false),
            "{at}: a wrong parameter is an error at the item"
        );
        assert_eq!(branch["properties"]["command"]["const"], cmd.name, "{at}");
        let table: BTreeSet<String> = item
            .fields
            .iter()
            .chain(cmd.params)
            .map(|f| f.key.to_owned())
            .collect();
        assert_eq!(
            keys(&branch["properties"]),
            table,
            "{at}: the item's keys and the command's parameters"
        );
        let required: Vec<String> = item
            .fields
            .iter()
            .chain(cmd.params)
            .filter(|f| f.required)
            .map(|f| f.key.to_owned())
            .collect();
        assert_eq!(strs(&branch["required"]), required, "{at}");
        for field in item.fields.iter().chain(cmd.params) {
            if matches!(field.ty, Ty::Command) {
                continue;
            }
            parameters += usize::from(cmd.params.iter().any(|p| p.key == field.key));
            let what = format!("{at}, key `{}`", field.key);
            expect_field(
                &root,
                field,
                Some(cmd),
                &branch["properties"][field.key],
                &what,
            );
        }
    }
    assert!(
        parameters >= 5,
        "the five parameters of the spec were walked: {parameters}"
    );
}

#[test]
fn no_command_parameter_shares_a_name_with_a_key_of_the_item() {
    let item = schema::node("item").expect("the item node");
    for cmd in schema::COMMANDS {
        for param in cmd.params {
            assert!(
                item.fields.iter().all(|f| f.key != param.key),
                "`{}` of `{}` would shadow a key of the item in the branch",
                param.key,
                cmd.name
            );
        }
    }
}

#[test]
fn every_vocabulary_of_the_table_is_in_the_schema() {
    let root = emit_current();
    let text = root.to_string();
    let quoted = |name: &str| format!("\"{name}\"");
    for condition in schema::CONDITIONS {
        for term in [condition.name.to_owned(), format!("!{}", condition.name)] {
            assert!(text.contains(&quoted(&term)), "condition `{term}`");
        }
    }
    for (token, _) in schema::INK_TOKENS {
        assert!(text.contains(&quoted(token)), "ink `{token}`");
    }
    for (rung, _) in schema::SIZE_RUNGS {
        assert!(text.contains(&quoted(rung)), "size `{rung}`");
    }
    for (space, _) in schema::SPACE_TOKENS {
        assert!(text.contains(&quoted(space)), "space `{space}`");
    }
    for face in face_names() {
        assert!(text.contains(&quoted(&face)), "face `{face}`");
    }
    for cmd in schema::COMMANDS {
        assert!(text.contains(&quoted(cmd.name)), "command `{}`", cmd.name);
        for param in cmd.params {
            assert!(
                text.contains(&quoted(param.key)),
                "parameter `{}`",
                param.key
            );
        }
        for state in cmd.face_states {
            assert!(
                text.contains(&quoted(state.key)),
                "face state `{}`",
                state.key
            );
        }
    }
    for binding in schema::TEXT_BINDS
        .iter()
        .chain(schema::IMAGE_BINDS)
        .chain(schema::STRIP_BINDS)
        .chain(schema::FACTS_BINDS)
        .chain(schema::CARD_ART_BINDS)
    {
        assert!(text.contains(&quoted(binding)), "binding `{binding}`");
    }
}

#[test]
fn a_keyed_value_is_admitted_exactly_where_the_table_says_it_is_keyable() {
    let root = emit_current();
    let both = |text: &str| json(text);
    let mut keyable = 0;
    for node in schema::all_nodes() {
        if matches!(node.kind, NodeKind::CommandDispatched) {
            continue;
        }
        for field in node.fields {
            let prop = shallow(&root, &node_def(&root, node)["properties"][field.key]);
            let keyed = prop
                .get("oneOf")
                .and_then(Value::as_array)
                .is_some_and(|b| b.iter().any(|x| x.get("required").is_some()));
            assert_eq!(
                keyed, field.keyable,
                "node `{}`, key `{}`",
                node.name, field.key
            );
            keyable += usize::from(field.keyable);
        }
    }
    assert!(keyable > 30, "{keyable}");
    // A keyed number is accepted, one that is out of range or keyed by an unknown condition is not.
    let radius = root["$defs"]["rect"]["properties"]["radius"].clone();
    let mut probe = json(r#"{"type": "object", "properties": {}}"#);
    probe["$defs"] = root["$defs"].clone();
    probe["properties"]["x"] = radius;
    let rooted = |instance: &Value| accepts(&probe, &both(&format!("{{\"x\": {instance}}}")));
    assert!(rooted(&both("8")));
    assert!(rooted(&both(
        r#"{"value": 8, "if": {"panel.info": 4, "!panel.chapters": 6}}"#
    )));
    assert!(!rooted(&both("541")));
    assert!(!rooted(&both(r#"{"value": 8, "if": {"panel.nope": 4}}"#)));
    assert!(!rooted(&both(r#"{"value": 8}"#)));
    assert!(!rooted(&both(
        r#"{"value": 8, "if": {"panel.info": 4}, "extra": 1}"#
    )));
    assert!(!rooted(&both(r#"{"value": 600, "if": {"panel.info": 4}}"#)));
}

// ---------------------------------------------------------------------------------------------------
// The schema against the parser
// ---------------------------------------------------------------------------------------------------

fn parsed(text: &str) -> Vec<Finding> {
    parse::parse(text.as_bytes()).findings
}

/// The JSON text of a string, escaped.
fn quoted(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

fn edit(src: &str, from: &str, to: &str) -> String {
    assert_eq!(src.matches(from).count(), 1, "`{from}` must occur once");
    src.replace(from, to)
}

/// Whether the regex and the parser give one answer on each sample, with some of each so the test is not
/// vacuous. `judge` builds the package text for a sample and says whether the parser took it.
fn same_answer(label: &str, regex: &str, samples: &[&str], judge: impl Fn(&str) -> bool) {
    let (mut yes, mut no) = (0, 0);
    for sample in samples {
        let parser = judge(sample);
        assert_eq!(
            regex_matches(regex, sample),
            parser,
            "{label}: the regex `{regex}` and the parser disagree on {sample:?} (the parser says {parser})"
        );
        if parser {
            yes += 1;
        } else {
            no += 1;
        }
    }
    assert!(
        yes > 0 && no > 0,
        "{label}: samples of both kinds ({yes} accepted, {no} refused)"
    );
}

fn refused_at(findings: &[Finding], code: &str, pointer: &str) -> bool {
    findings
        .iter()
        .any(|f| f.code == code && f.pointer == pointer)
}

#[test]
fn every_pattern_regex_gives_the_parsers_answer() {
    same_answer(
        "reverse DNS",
        Pattern::ReverseDns.regex(),
        &[
            "com.example.x",
            "a.b",
            "a",
            "",
            ".a.b",
            "a.b.",
            "a..b",
            "A.b",
            "a.B",
            "a.1b",
            "a.b-c",
            "a-.b",
            "-a.b",
            "a_b.c",
            "1a.b",
            "a.b.c.d",
            "a.b c",
            "é.b",
            "a.b9.c0",
            "builtin:classic",
            "builtin:Classic",
            "builtin:other",
            "builtin:",
            "xbuiltin:classic",
            "builtin:classic ",
            "builtin:classic.x",
        ],
        |s| {
            !refused_at(
                &parsed(&edit(
                    MINIMAL,
                    r#""id": "com.plxnative.examples.minimal""#,
                    &format!("\"id\": {}", quoted(s)),
                )),
                "HUD-E0105",
                "/id",
            )
        },
    );
    same_answer(
        "semver",
        Pattern::Semver.regex(),
        &[
            "1.0.0",
            "0.0.0",
            "01.0.0",
            "1.00.0",
            "1.0",
            "1.0.0.0",
            "1.0.x",
            "1.0.0-beta",
            "+1.0.0",
            "",
            "10.20.30",
            "1..0",
            " 1.0.0",
            "1.0.0 ",
        ],
        |s| {
            !refused_at(
                &parsed(&edit(
                    MINIMAL,
                    r#""version": "1.0.0""#,
                    &format!("\"version\": {}", quoted(s)),
                )),
                "HUD-E0105",
                "/version",
            )
        },
    );
    same_answer(
        "format version",
        Pattern::FormatVersion.regex(),
        &[
            "1.0", "1.12", "01.0", "1.", "1", "1.0.0", ".1", "v1.0", "0.0", "10.0", "1.-1", " 1.0",
            "2.0",
        ],
        |s| {
            !refused_at(
                &parsed(&edit(
                    MINIMAL,
                    r#""format": "1.0""#,
                    &format!("\"format\": {}", quoted(s)),
                )),
                "HUD-E0401",
                "/format",
            )
        },
    );
    same_answer(
        "schema URL",
        Pattern::SchemaUrl.regex(),
        &[
            "https://plxnative.com/hud/schema/1.0.json",
            "https://",
            "http://x",
            "https:/x",
            "https://a b",
            "https://é",
            "HTTPS://x",
            "https://x\u{7f}",
            "https://x/~",
            "https://x/!",
        ],
        |s| {
            !refused_at(
                &parsed(&edit(
                    MINIMAL,
                    r#""format": "1.0","#,
                    &format!("\"format\": \"1.0\", \"$schema\": {},", quoted(s)),
                )),
                "HUD-E0105",
                "/$schema",
            )
        },
    );
    same_answer(
        "id",
        Pattern::Id.regex(),
        &[
            "bar",
            "a",
            "",
            "A",
            "a-b_9",
            "9a",
            "-a",
            "_a",
            "a.b",
            "a b",
            "a/b",
            "é",
            "under_score",
            "dash-ed",
        ],
        |s| {
            !refused_at(
                &parsed(&edit(
                    MINIMAL,
                    r#"{ "id": "bar", "cells""#,
                    &format!("{{ \"id\": {}, \"cells\"", quoted(s)),
                )),
                "HUD-E0105",
                "/layout/block/0/id",
            )
        },
    );
}

fn palette_package(name: &str, value: &str) -> String {
    edit(
        MINIMAL,
        r#""license": "CC0-1.0","#,
        &format!(
            r#""license": "CC0-1.0", "palette": {{ {}: {} }},"#,
            quoted(name),
            quoted(value)
        ),
    )
}

#[test]
fn the_palette_regexes_give_the_parsers_answer() {
    same_answer(
        "palette name",
        schema::PALETTE_NAME_REGEX,
        &[
            "gold", "g", "gold_2", "Gold", "2gold", "gold-2", "_gold", "", "gold ", "go ld",
            "GOLD", "é",
        ],
        |s| {
            let found = parsed(&palette_package(s, "#000000"));
            !found.iter().any(|f| f.code == "HUD-E0505")
        },
    );
    same_answer(
        "palette value",
        schema::HEX_COLOUR_REGEX,
        &[
            "#f2b535", "#F2B535", "#fff", "#f2b53", "#f2b5355", "f2b535", "#f2b53g", "#00000a",
            "# 2b535", "",
        ],
        |s| {
            let found = parsed(&palette_package("gold", s));
            !found.iter().any(|f| f.code == "HUD-E0506")
        },
    );
    // An ink is a theme token or a palette-shaped name: the same shape as a palette name.
    let ink = |s: &str| {
        let text = edit(
            MINIMAL,
            r#""fill": "ACCENT""#,
            &format!("\"fill\": {}", quoted(s)),
        );
        let found = parsed(&text);
        // A well-shaped name the package does not declare is E0204 (a reference, lint's); a token that is
        // not one is E0202; anything else that is not shaped like either is E0105 (a shape).
        !found
            .iter()
            .any(|f| f.code == "HUD-E0105" || f.code == "HUD-E0202")
    };
    assert!(ink("gold") && ink("ACCENT") && ink("gold_2"));
    assert!(
        !ink("Gold") && !ink("TEXT-PRIMARY") && !ink("GOLD_NOPE") && !ink("") && !ink("gold-2")
    );
}

#[test]
fn the_constants_the_emitter_borrows_are_the_parsers_boundaries() {
    let gap = |px: i64| {
        parsed(&edit(
            MINIMAL,
            r#""id": "row", "gap_before": "SM""#,
            &format!(r#""id": "row", "gap_before": {px}"#),
        ))
    };
    assert!(
        gap(schema::GAP_TOP_PX).is_empty(),
        "the limit itself is a gap"
    );
    assert!(refused_at(
        &gap(schema::GAP_TOP_PX + 1),
        "HUD-E0105",
        "/layout/block/1/gap_before"
    ));
    assert!(gap(0).is_empty());
    assert!(refused_at(
        &gap(-1),
        "HUD-E0105",
        "/layout/block/1/gap_before"
    ));

    let face = |face: &str| {
        let text = edit(
            REFERENCE,
            r#""face": "builtin:next""#,
            &format!("\"face\": {}", quoted(face)),
        );
        parsed(&text)
    };
    for (name, _) in schema::BUILTIN_ICONS {
        assert!(
            face(&format!("{}{name}", schema::BUILTIN_FACE_PREFIX)).is_empty(),
            "{name}"
        );
    }
    assert!(!face("builtin:nope").is_empty());
    assert!(!face("builtin-play").is_empty());
}

fn is_json_object(text: &str) -> bool {
    text.parse::<Value>().is_ok_and(|v| v.is_object())
}

#[test]
fn the_schema_accepts_every_valid_package_of_the_corpus() {
    let root = emit_current();
    let mut texts: Vec<(String, String)> = [
        ("reference", REFERENCE),
        ("minimal", MINIMAL),
        ("classic", CLASSIC),
        ("hero-center", HERO_CENTER),
        ("tabs", TABS),
        ("palette", PALETTE),
    ]
    .iter()
    .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
    .collect();
    texts.extend(
        valid::accepted()
            .into_iter()
            .map(|(n, t)| (n.to_owned(), t)),
    );
    assert!(texts.len() > 25, "{}", texts.len());
    for (label, text) in &texts {
        assert!(is_json_object(text), "{label}");
        let found = check(&root, &json(text));
        assert!(
            found.is_empty(),
            "the parser accepts `{label}`, so the schema must: {found:#?}"
        );
    }
}

/// The codes whose fault the schema has the keywords to see: an unknown or missing key, a wrong type, a value
/// outside a closed list, a range or a pattern, an unknown command or parameter, a malformed condition or
/// keyed value, a palette value. Everything else (a duplicate key, a cross-key rule, a reference, the version
/// gate, SVG, the format's own bounds) is the parser's and lint's, and the schema is silent on it.
const SEEN: &[&str] = &[
    "HUD-E0102",
    "HUD-E0103",
    "HUD-E0104",
    "HUD-E0105",
    "HUD-E0201",
    "HUD-E0202",
    "HUD-E0301",
    "HUD-E0302",
    "HUD-E0303",
    "HUD-E0304",
    "HUD-E0401",
    "HUD-E0501",
    "HUD-E0502",
    "HUD-E0504",
    "HUD-E0506",
];

/// Rows of those codes whose fault the schema still cannot see, and why. The test refuses a row that is
/// listed here and is nonetheless rejected, so the list cannot rot.
const BLIND: &[(&str, &str)] = &[
    (
        "rest_solid_without_a_focus_is_missing_the_focus",
        "a table rule (IfValue): a key one value of another requires",
    ),
    (
        "scrim_block_without_extra",
        "a table rule (IfValue): `cover: block` requires `extra`",
    ),
    (
        "name_too_long",
        "a text's length: no keyword of the plan's list bounds it",
    ),
    (
        "name_with_a_control_character",
        "a text's characters: the parser's own check",
    ),
    (
        "a_repeated_facts_name",
        "a name repeated in an enum list: `uniqueItems` is not in the plan's list",
    ),
    (
        "an_array_over_its_bound",
        "a list's length: no keyword of the plan's list bounds it",
    ),
    (
        "an_asset_id_with_capitals",
        "the key of an `assets` object: `propertyNames` is not in the plan's list",
    ),
    (
        "card_binding_outside_a_card",
        "a card-scoped name outside a strip card: scope is not a keyword",
    ),
    (
        "when_empty_array",
        "a condition array's non-emptiness: no keyword of the plan's list says it",
    ),
    (
        "keyed_with_an_empty_if",
        "a keyed value's non-empty `if`: no keyword of the plan's list says it",
    ),
];

#[test]
fn every_invalid_row_the_schema_rejects_is_rejected_where_the_parser_agrees() {
    let root = emit_current();
    let (mut rejected, mut silent, mut checked) = (0, 0, 0);
    let mut unexplained = Vec::new();
    for row in corpus() {
        let Ok(text) = std::str::from_utf8(&row.bytes) else {
            continue;
        };
        let Ok(instance) = text.parse::<Value>() else {
            continue;
        };
        checked += 1;
        let parser = parse::parse(&row.bytes).findings;
        let failures = check(&root, &instance);
        // The version gate answers alone, before any key is judged, so the parser says nothing else about a
        // package of another format; the schema, which has no gate, still reads the rest.
        let gated = parser
            .iter()
            .all(|f| f.pointer == "/format" && f.code.starts_with("HUD-E04"));
        for failure in failures.iter().filter(|_| !gated) {
            assert!(
                parser.iter().any(|f| is_prefix(&failure.pointer, &f.pointer)),
                "row `{}`: the schema fails at {:?} ({}), and the parser reports nothing there or under it:\n{:#?}",
                row.label,
                failure.pointer,
                failure.why,
                parser.iter().map(|f| format!("{} {}", f.code, f.pointer)).collect::<Vec<_>>()
            );
        }
        let blind = BLIND.iter().any(|(label, _)| *label == row.label);
        if failures.is_empty() {
            silent += 1;
            if SEEN.contains(&row.code.id) && !blind {
                unexplained.push(format!(
                    "{} ({} at {})",
                    row.label, row.code.id, row.pointer
                ));
            }
        } else {
            rejected += 1;
            assert!(
                !blind,
                "row `{}` is listed as one the schema cannot see, and it does",
                row.label
            );
        }
    }
    assert!(checked > 150, "the corpus was reached: {checked} rows");
    assert!(
        rejected > 80,
        "the schema rejected {rejected} of {checked} rows ({silent} silent)"
    );
    assert!(
        unexplained.is_empty(),
        "invalid rows of a code the schema can see, accepted by it:\n  {}",
        unexplained.join("\n  ")
    );
    assert!(codes::ALL.iter().any(|c| SEEN.contains(&c.id)));
}
