//! **The JSON Schema of HUD package format 1.0, emitted from the descriptor table** ([`super::schema`]).
//!
//! `plxnative-sim --hud-schema` (PR5d) prints [`emit_text`]; the committed copy is
//! `site/hud/schema/1.0.json`, served at `https://plxnative.com/hud/schema/1.0.json` (`docs/hud-plugins.md`,
//! "LLM authoring"). Nothing is listed here that the table does not list: every key, type, range, closed
//! vocabulary, pattern, default and description is read from it, and [`Ty`] is matched in full, so a new
//! table type is a compile error here as it is in the parser.
//!
//! This module is PURE: a table in, a [`Value`] out. No IO, no globals, no clock.
//!
//! ## The shape
//!
//! Draft 2020-12 (`$schema`, `$id`), written with the keywords whose meaning has not changed since
//! draft-07: `type`, `properties`, `required`, `additionalProperties`, `enum`, `const`, `pattern`,
//! `minimum`, `maximum`, single-schema `items` and `oneOf`. Three groups of words are added, and no more:
//! * `$defs` and `$ref`, because the format is recursive (a `cell` holds a `vstack` or a `rect` that holds
//!   `cell`s) and a recursive schema cannot be written without a reference. They also keep the repeated
//!   vocabularies (the inks, the gaps, the conditions) one definition each. Every reference is local.
//! * the annotations `description` (the table's `doc`) and `default` (the table's, as JSON). A validator
//!   ignores both.
//! * the root's `title`, an annotation as well, naming the format.
//!
//! Every object is `additionalProperties: false`, so a misspelt key is an error at the object. A control
//! item is a `oneOf` with one branch per command, keyed by a `const` on `command`; each branch lists the
//! item's keys and that command's typed parameters and is `additionalProperties: false`, so a validator
//! reports a wrong parameter at the item. A state-keyable value ([`Field::keyable`]) admits the plain value
//! and its one-level `{"value": v, "if": {"<condition>": v, ...}}` form. A cell's single content key is a
//! `oneOf` of `required` lists, the table's [`NodeKind::OneOf`].
//!
//! The file is for one format MINOR: [`emit`] leaves out everything that arrived after it, so
//! `1.0.json` stays what it was when a later minor adds to the table (a minor only adds).
//!
//! ## What the schema leaves to the parser and to lint
//!
//! The schema is a SOUND OVER-APPROXIMATION: everything the strict parser accepts, it accepts, and a
//! test holds that over the whole valid corpus. It does not say, because JSON Schema cannot, or because
//! the keywords above do not:
//! * a duplicate key (the schema sees a decoded object, where the later one has won);
//! * the version gate's minor rule (a `format` is only checked for its shape here) and the `$schema` URL's
//!   major and minor;
//! * the table's cross-key [`Rule`](super::schema::Rule)s (`requires`, `excludes`, `if_value`, a `seek`'s `by`
//!   against its `dir`, a rect's `bleed` against its `pad`, ...), and every compile invariant (reachability,
//!   id uniqueness, the playbar, the section bands);
//! * a reference that has to be declared: an asset id in `assets`, a palette name in `palette`, and a
//!   card-scoped binding or condition inside a strip card (the schema admits them anywhere);
//! * the bounds that are not a number, an enum or a pattern: a text's and a list's length, a repeated name
//!   in an `EnumList`, the non-emptiness of a condition array, an id's length, a palette's and an asset's
//!   key names, the keys of a face ladder, and what an SVG asset holds.
//!
//! Where the schema does reject, it names a pointer that equals the parser's or is a prefix of it: a
//! stock validator reports a bad parameter at the ITEM, through the `oneOf`, where the parser names the
//! key (the plan's "Agreement" rule; the tests of this module hold it over the invalid corpus).
//!
//! ## Regenerating the golden
//!
//! `site/hud/schema/1.0.json` is a committed golden. A test regenerates it and fails, with the first
//! differing line, on any difference. After a deliberate change to the table, rewrite it with
//!
//! ```text
//! PLX_HUD_SCHEMA_WRITE=1 PATH="$HOME/.cargo/bin:$PATH" make test-crate C=plx_appkit T=hud_format
//! ```
//!
//! and commit the result. The same variable rewrites the published examples' lint reports
//! (`site/hud/examples/invalid/*.lint.json`).

use plx_ui::rec::strict_json::{Map, Number, Value};

use super::schema::{self, Cmd, Field, Node, NodeKind, Pattern, Since, Ty};

/// The meta-schema every emitted schema declares.
pub const SCHEMA_DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// Where schemas are published: the file for a format is `<base><major>.<minor>.json`.
pub const SCHEMA_URL_BASE: &str = "https://plxnative.com/hud/schema/";

/// The `$id` of the schema of `version`.
pub fn schema_id(version: Since) -> String {
    format!("{SCHEMA_URL_BASE}{}.{}.json", version.0, version.1)
}

/// The schema of the format this build writes.
pub fn emit_current() -> Value {
    emit(schema::CURRENT)
}

/// The schema of `version` as published: pretty-printed, keys sorted, one trailing newline.
pub fn emit_text(version: Since) -> String {
    format!("{:#}\n", emit(version))
}

/// The schema of format `version`: the table's entries that arrived in it or before.
pub fn emit(version: Since) -> Value {
    let mut emitter = Emitter {
        version,
        defs: Map::new(),
    };
    emitter.shared_defs();
    for node in schema::all_nodes() {
        if node.name != schema::ROOT.name {
            let def = emitter.node_schema(node);
            emitter.def(node.name, def);
        }
    }
    let mut root = emitter.node_object(&schema::ROOT);
    root.insert("$schema".to_owned(), text(SCHEMA_DRAFT));
    root.insert("$id".to_owned(), Value::String(schema_id(version)));
    root.insert(
        "title".to_owned(),
        Value::String(format!(
            "PlxNative HUD package, format {}.{}",
            version.0, version.1
        )),
    );
    root.insert("$defs".to_owned(), Value::Object(emitter.defs));
    Value::Object(root)
}

struct Emitter {
    version: Since,
    defs: Map<String, Value>,
}

fn text(s: &str) -> Value {
    Value::String(s.to_owned())
}

fn texts<'a>(names: impl IntoIterator<Item = &'a str>) -> Value {
    Value::Array(names.into_iter().map(text).collect())
}

fn number(v: f64) -> Value {
    Number::from_f64(v).map_or(Value::Null, Value::Number)
}

fn object(pairs: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::Object(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

fn reference(def: &str) -> Value {
    object([("$ref", Value::String(format!("#/$defs/{def}")))])
}

fn one_of(branches: Vec<Value>) -> Value {
    object([("oneOf", Value::Array(branches))])
}

fn string_enum<'a>(names: impl IntoIterator<Item = &'a str>) -> Value {
    object([("type", text("string")), ("enum", texts(names))])
}

fn string_pattern(regex: &str) -> Value {
    object([("type", text("string")), ("pattern", text(regex))])
}

/// `{"type": "object", "additionalProperties": <value>}`: an object whose keys the schema does not name.
fn map_of(value: Value) -> Value {
    object([("type", text("object")), ("additionalProperties", value)])
}

fn annotate(schema: &mut Value, doc: &str, default: Option<&str>) {
    let Value::Object(map) = schema else {
        return;
    };
    if !doc.is_empty() {
        map.insert("description".to_owned(), text(doc));
    }
    if let Some(value) = default.and_then(|json| json.parse::<Value>().ok()) {
        map.insert("default".to_owned(), value);
    }
}

/// A def name for a plain number or flag a state-keyed value is built over; `None` for a type that is
/// keyed inline.
fn slug(ty: &Ty) -> Option<String> {
    let clean = |s: String| s.replace('-', "m").replace('.', "p");
    match ty {
        Ty::Bool => Some("bool".to_owned()),
        Ty::Int { min, max } => Some(clean(format!("int_{min}_{max}"))),
        Ty::Num { min, max } => Some(clean(format!("num_{min}_{max}"))),
        Ty::Ink => Some("ink".to_owned()),
        Ty::Gap => Some("gap".to_owned()),
        Ty::Text { .. }
        | Ty::Pattern(_)
        | Ty::Enum(_)
        | Ty::EnumList(_)
        | Ty::Bind(_)
        | Ty::Size
        | Ty::Asset
        | Ty::Face
        | Ty::Cond
        | Ty::Object(_)
        | Ty::List { .. }
        | Ty::Palette
        | Ty::Assets
        | Ty::Command
        | Ty::Either(..) => None,
    }
}

impl Emitter {
    fn def(&mut self, name: &str, schema: Value) {
        self.defs.insert(name.to_owned(), schema);
    }

    /// The vocabularies several keys share: one definition each.
    fn shared_defs(&mut self) {
        self.def("ink", self.ink());
        self.def("gap", self.gap());
        self.def(
            "size",
            string_enum(schema::SIZE_RUNGS.iter().map(|(n, _)| *n)),
        );
        self.def("when", self.when());
        self.def("face_name", self.face_name());
    }

    /// A theme token, or a palette name (whether the package declares it is lint's).
    fn ink(&self) -> Value {
        one_of(vec![
            string_enum(schema::INK_TOKENS.iter().map(|(n, _)| *n)),
            string_pattern(schema::PALETTE_NAME_REGEX),
        ])
    }

    /// A spacing token, or a whole number of px up to the canvas height.
    fn gap(&self) -> Value {
        one_of(vec![
            string_enum(schema::SPACE_TOKENS.iter().map(|(n, _)| *n)),
            object([
                ("type", text("integer")),
                ("minimum", Value::from(0)),
                ("maximum", Value::from(schema::GAP_TOP_PX)),
            ]),
        ])
    }

    /// The conditions of this format, each also with its `!` form.
    fn terms(&self) -> Vec<String> {
        schema::CONDITIONS
            .iter()
            .filter(|c| c.since <= self.version)
            .flat_map(|c| [c.name.to_owned(), format!("!{}", c.name)])
            .collect()
    }

    /// A `when`: a term, or an array of terms that must all hold.
    fn when(&self) -> Value {
        let terms = self.terms();
        let term = string_enum(terms.iter().map(String::as_str));
        one_of(vec![
            term.clone(),
            object([("type", text("array")), ("items", term)]),
        ])
    }

    /// A single face: a built-in glyph, or the id of an asset (declared in `assets`: lint's).
    fn face_name(&self) -> Value {
        let builtin: Vec<String> = schema::BUILTIN_ICONS
            .iter()
            .map(|(n, _)| format!("{}{n}", schema::BUILTIN_FACE_PREFIX))
            .collect();
        one_of(vec![
            string_enum(builtin.iter().map(String::as_str)),
            string_pattern(Pattern::Id.regex()),
        ])
    }

    /// The object schema of a node: its keys, its required keys, no others.
    fn node_object(&mut self, node: &Node) -> Map<String, Value> {
        let mut properties = Map::new();
        let mut required = Vec::new();
        let version = self.version;
        for field in node.fields.iter().filter(|f| f.since <= version) {
            let schema = self.field_schema(field, None);
            properties.insert(field.key.to_owned(), schema);
            if field.required {
                required.push(field.key);
            }
        }
        let mut out = Map::new();
        out.insert("type".to_owned(), text("object"));
        out.insert("description".to_owned(), text(node.doc));
        out.insert("properties".to_owned(), Value::Object(properties));
        if !required.is_empty() {
            out.insert("required".to_owned(), texts(required));
        }
        out.insert("additionalProperties".to_owned(), Value::Bool(false));
        if let NodeKind::OneOf(keys) = node.kind {
            let branches = keys
                .iter()
                .map(|k| object([("required", texts([*k]))]))
                .collect();
            out.insert("oneOf".to_owned(), Value::Array(branches));
        }
        out
    }

    fn node_schema(&mut self, node: &Node) -> Value {
        match node.kind {
            NodeKind::Plain | NodeKind::OneOf(_) => Value::Object(self.node_object(node)),
            NodeKind::CommandDispatched => self.dispatched(node),
        }
    }

    /// A control item: one branch per command, keyed by a `const` on the key that names it.
    fn dispatched(&mut self, node: &Node) -> Value {
        let version = self.version;
        let branches = schema::COMMANDS
            .iter()
            .filter(|c| c.since <= version)
            .map(|cmd| self.command_branch(node, cmd))
            .collect();
        let mut out = Map::new();
        out.insert("description".to_owned(), text(node.doc));
        out.insert("oneOf".to_owned(), Value::Array(branches));
        Value::Object(out)
    }

    fn command_branch(&mut self, node: &Node, cmd: &Cmd) -> Value {
        let mut properties = Map::new();
        let mut required = Vec::new();
        let version = self.version;
        let own = node.fields.iter().filter(|f| f.since <= version);
        let params = cmd.params.iter().filter(|f| f.since <= version);
        for field in own.chain(params) {
            let schema = if matches!(field.ty, Ty::Command) {
                let mut named = object([("const", text(cmd.name))]);
                annotate(&mut named, field.doc, None);
                named
            } else {
                self.field_schema(field, Some(cmd))
            };
            properties.insert(field.key.to_owned(), schema);
            if field.required {
                required.push(field.key);
            }
        }
        let mut out = Map::new();
        out.insert("type".to_owned(), text("object"));
        out.insert("description".to_owned(), text(cmd.doc));
        out.insert("properties".to_owned(), Value::Object(properties));
        if !required.is_empty() {
            out.insert("required".to_owned(), texts(required));
        }
        out.insert("additionalProperties".to_owned(), Value::Bool(false));
        Value::Object(out)
    }

    fn field_schema(&mut self, field: &Field, cmd: Option<&Cmd>) -> Value {
        let mut schema = if field.keyable {
            self.keyed(&field.ty, cmd)
        } else {
            self.ty_schema(&field.ty, cmd)
        };
        annotate(&mut schema, field.doc, field.default);
        schema
    }

    /// The plain schema of a number or flag a keyed value is built over, as a reference to its definition.
    fn plain_ref(&mut self, ty: &Ty, cmd: Option<&Cmd>) -> Value {
        match slug(ty) {
            Some(name) => {
                if !self.defs.contains_key(&name) {
                    let plain = self.ty_schema(ty, cmd);
                    self.def(&name, plain);
                }
                reference(&name)
            }
            None => self.ty_schema(ty, cmd),
        }
    }

    /// A state-keyable value: the plain value, or `{"value": v, "if": {"<condition>": v, ...}}`.
    fn keyed(&mut self, ty: &Ty, cmd: Option<&Cmd>) -> Value {
        let name = slug(ty).map(|s| format!("keyed_{s}"));
        if let Some(name) = &name {
            if self.defs.contains_key(name) {
                return reference(name);
            }
        }
        let plain = self.plain_ref(ty, cmd);
        let cases: Map<String, Value> = self
            .terms()
            .into_iter()
            .map(|term| (term, plain.clone()))
            .collect();
        let cased = object([
            ("type", text("object")),
            ("properties", Value::Object(cases)),
            ("additionalProperties", Value::Bool(false)),
        ]);
        let form = object([
            ("type", text("object")),
            (
                "properties",
                object([("value", plain.clone()), ("if", cased)]),
            ),
            ("required", texts(["value", "if"])),
            ("additionalProperties", Value::Bool(false)),
        ]);
        let both = one_of(vec![plain, form]);
        match name {
            Some(name) => {
                self.def(&name, both);
                reference(&name)
            }
            None => both,
        }
    }

    /// The schema of a value of type `ty`. `cmd` is the command of the control item being described, which
    /// decides what a `face` may be.
    fn ty_schema(&mut self, ty: &Ty, cmd: Option<&Cmd>) -> Value {
        match ty {
            Ty::Bool => object([("type", text("boolean"))]),
            Ty::Int { min, max } => object([
                ("type", text("integer")),
                ("minimum", Value::from(*min)),
                ("maximum", Value::from(*max)),
            ]),
            Ty::Num { min, max } => object([
                ("type", text("number")),
                ("minimum", number(*min)),
                ("maximum", number(*max)),
            ]),
            Ty::Text { .. } => object([("type", text("string"))]),
            Ty::Pattern(pattern) => string_pattern(pattern.regex()),
            Ty::Enum(names) => string_enum(names.iter().copied()),
            Ty::EnumList(names) => object([
                ("type", text("array")),
                ("items", string_enum(names.iter().copied())),
            ]),
            Ty::Bind(names) => string_enum(
                names
                    .iter()
                    .copied()
                    .filter(|n| schema::binding(n).map_or(true, |b| b.since <= self.version)),
            ),
            Ty::Size => reference("size"),
            Ty::Gap => reference("gap"),
            Ty::Ink => reference("ink"),
            Ty::Asset => string_pattern(Pattern::Id.regex()),
            Ty::Face => self.face(cmd),
            Ty::Cond => reference("when"),
            Ty::Object(name) => reference(name),
            Ty::List { of, .. } => {
                let item = self.ty_schema(of, cmd);
                object([("type", text("array")), ("items", item)])
            }
            Ty::Palette => map_of(string_pattern(schema::HEX_COLOUR_REGEX)),
            Ty::Assets => map_of(object([("type", text("string"))])),
            Ty::Command => string_enum(
                schema::COMMANDS
                    .iter()
                    .filter(|c| c.since <= self.version)
                    .map(|c| c.name),
            ),
            Ty::Either(a, b) => {
                let first = self.ty_schema(a, cmd);
                let second = self.ty_schema(b, cmd);
                one_of(vec![first, second])
            }
        }
    }

    /// A control's `face`: a single face, and for a command that has states also a map of them.
    fn face(&mut self, cmd: Option<&Cmd>) -> Value {
        let single = reference("face_name");
        let Some(cmd) = cmd.filter(|c| !c.face_states.is_empty()) else {
            return single;
        };
        let mut properties = Map::new();
        let mut required = Vec::new();
        for state in cmd.face_states {
            let face = if state.ladder {
                map_of(single.clone())
            } else {
                single.clone()
            };
            properties.insert(state.key.to_owned(), face);
            if state.required {
                required.push(state.key);
            }
        }
        let mut map = Map::new();
        map.insert("type".to_owned(), text("object"));
        map.insert("properties".to_owned(), Value::Object(properties));
        if !required.is_empty() {
            map.insert("required".to_owned(), texts(required));
        }
        map.insert("additionalProperties".to_owned(), Value::Bool(false));
        one_of(vec![single, Value::Object(map)])
    }
}

#[cfg(test)]
mod tests;
