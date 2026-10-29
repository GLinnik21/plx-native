//! **The strict parser of a HUD package**: bytes in, a typed [`Package`] and every [`Finding`] out.
//!
//! It is a generic walker over the descriptor table ([`super::schema`]): nothing about a key, a cell kind
//! or a command is written here. The JSON is read through `plx_ui::rec::strict_json::decode_ordered`, the
//! one duplicate-key-preserving reader in the tree, so a duplicate key is a finding at its own pointer and
//! a state-keyed value keeps its conditions in document order. The module is pure: no IO, no globals.
//!
//! **One walker, two doors.** [`parse`] (the version gate, then the top level) and [`validate_json`] (one
//! fragment against one node) both run the SAME `Walker`; there is no second copy of a rule. A key, a cell
//! kind, a command, a condition or a theme token is never named in this file to JUDGE it: the walker reads
//! them from the table. The one place keys are named is [`Package::from_object`], which READS the package's
//! top-level scalars and the `app` range out of an object that has already passed the table; a test
//! (`the_package_reader_names_only_keys_the_table_has`) fails if the table drops or renames one. What
//! this file also names is the format's own grammar, which is not a table row: the
//! `{"value": v, "if": {...}}` keys of a state-keyed value, the `builtin:` face prefix, the `#rrggbb`
//! palette form, the string shapes a [`schema::Pattern`] checks, and the node `card`, which is the scope
//! of the `item.*` bindings and conditions (the table carries no flag for it; see `schema`'s module doc).
//!
//! **Where a finding points** (the module doc of `hud_format` lists the conventions). Beyond them:
//! * a value that fails under a command PARAMETER is reported with the command code, not the generic one
//!   (`E0304` for a wrong type, range or name; `E0302` for an unknown key; `E0303` for a missing
//!   parameter; `E0305` for a [`schema::Cmd::rules`] entry), so a lint reader sees which control it was;
//! * a [`schema::Rule`] of a node reports `E0103` for a conditionally required key (at the object),
//!   `E0106` for a forbidden key (at that key) or a forbidden combination (at the object), and `E0105`
//!   for a number that breaks a bound (at the value); a rule of a command reports `E0305`
//!   (`E0304` for `NonZero`) at the same places.
//!
//! **Order.** Findings are in DOCUMENT order of the keys of each object, whatever order the keys were
//! walked in: `palette` and `assets` are walked first (so a reference can be resolved) and their findings
//! are put back where the keys stand.
//!
//! **The version gate runs first and answers alone.** Before any other key is read, `/format` is checked,
//! and a package that fails the gate returns ONE finding at `/format`: `E0401` if `format` is missing,
//! not a string, or not `MAJOR.MINOR`; `E0402` if the major is supported but the minor is above its
//! highest (message [`REASON_NEWER`](super::REASON_NEWER)); `E0403` if the major is not supported (message
//! `REASON_NEWER` when it is above the highest supported major, `REASON_OLDER` when below). An older app
//! cannot know whether an unknown key of a newer minor is cosmetic, so strictness makes the gate mandatory.
//! A document that is not JSON is `E0100` at `""` and also answers alone.
//!
//! **Findings are collected, not thrown.** After the gate the walker reports every problem it finds, in
//! document order, and keeps going: `Parsed::package` is `Some` exactly when no finding is an `Error`
//! (a `Warning` does not block a package).

use std::collections::BTreeSet;

use plx_ui::rec::strict_json::{decode_ordered, Ordered};

use super::schema::{self, Cmd, Field, Node, NodeKind, Pattern, Rule, Since, Ty};
use super::{
    codes, has_error, nearest, pointer_index, pointer_push, svg_subset, Code, Finding,
    REASON_NEWER, REASON_OLDER,
};

/// The most bytes a package file may carry; a larger input is `E0100` (a limit of the format: the
/// strict reader's memory is bounded by it).
pub const MAX_PACKAGE_BYTES: usize = 1024 * 1024;

/// The decoded-memory budget given to `decode_ordered`, which charges three times the source and then
/// every node (a string eight times its length, a value a few hundred bytes). 16 MiB holds a full
/// 1 MiB package of inline SVG text (about 11 MiB) and about 45 000 other values, many times the worked
/// example's 600 nodes; past it the file is refused as too large, not read into 64 MiB of tree.
const DECODE_MEMORY_BYTES: usize = 16 * 1024 * 1024;
/// The longest identifier ([`schema::Pattern::Id`]: a band, segment or control id, an asset id).
const ID_LEN: usize = 40;
/// The longest a reverse-DNS id or a schema URL may be: the table gives these no bound, and a package
/// file carries them into logs and the picker.
const PATTERN_LEN: usize = 256;
/// How much of a name a message quotes and `nearest` compares: a key of a hostile file can be a megabyte.
const CLIP_CHARS: usize = 48;
/// How many declared names a hint lists: a package may declare thousands of assets, and every unknown
/// face would otherwise carry them all.
const HINT_NAMES: usize = 12;
/// The node whose subtree is a strip card: `item.*` bindings and conditions are legal inside it only.
/// `schema`'s module doc records that the table has no flag for this and that the walker enters card
/// scope at the node named `card`.
const CARD_NODE: &str = "card";
/// The two keys of a state-keyed value, `{"value": v, "if": {...}}`: the format's grammar, not a table row.
const KEYED_VALUE: &str = "value";
const KEYED_IF: &str = "if";
/// The `node` names of the objects the walker builds that are not nodes of the table: the palette, the
/// assets, and a face map with its Skip-interval ladder.
const PALETTE_NODE: &str = "palette";
const ASSETS_NODE: &str = "assets";
const FACE_MAP_NODE: &str = "face_map";
const FACE_LADDER_NODE: &str = "face_ladder";

/// The result of [`parse`].
#[derive(Clone, Debug, PartialEq)]
pub struct Parsed {
    /// `Some` iff no finding is an `Error`.
    pub package: Option<Package>,
    /// Every finding, in document order.
    pub findings: Vec<Finding>,
}

/// A parsed package: the top-level scalars and a typed tree for `layout`. No defaults are applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    /// `format`'s `(major, minor)`.
    pub format: Since,
    pub id: String,
    pub version: String,
    pub name: String,
    pub author: String,
    pub license: String,
    pub app: Option<AppRange>,
    /// `(name, rgb)` in document order; every name is lower-case, every colour from `#rrggbb`.
    pub palette: Vec<(String, [u8; 3])>,
    /// `(id, svg source)` in document order; every source passed `svg_subset::check_asset`.
    pub assets: Vec<(String, String)>,
    /// The `layout` object (node `layout`).
    pub layout: Object,
}

/// `app.min` / `app.max`: an app-release range (semver text), either bound optional.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppRange {
    pub min: Option<String>,
    pub max: Option<String>,
}

/// An object walked against a node of the table.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    /// The [`schema::Node::name`] it was walked against (`"item"` for a control item, whose command
    /// parameters are ordinary entries of `fields`).
    pub node: &'static str,
    /// Its RFC 6901 pointer in the package (or in the fragment `validate_json` was given).
    pub pointer: String,
    /// `(key, value)` in document order; a key appears once (a duplicate is a finding).
    pub fields: Vec<(String, Val)>,
}

impl Object {
    /// The value written under `key`.
    pub fn get(&self, key: &str) -> Option<&Val> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// A typed value. Which variant a [`schema::Ty`] produces is the table in `schema`'s module doc.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Bool(bool),
    Int(i64),
    Num(f64),
    /// Free text, a pattern-checked string (an id, a semver) or an SVG/`#rrggbb` source.
    Text(String),
    /// A closed-vocabulary name: an enum value, a binding, a size rung, a space token, a command,
    /// `builtin:<icon>`.
    Name(String),
    /// An UPPER_CASE theme token from the allow-list.
    Token(String),
    /// A reference to something the package declares: a palette name or an asset id.
    Ref(String),
    Cond(Cond),
    Keyed(Keyed),
    List(Vec<Val>),
    Object(Object),
}

impl Val {
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Val::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Val::Int(n) => Some(*n),
            _ => None,
        }
    }
    /// An `Int` or a `Num`, as a float.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Val::Int(n) => Some(*n as f64),
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }
    /// The text of a `Text`, `Name`, `Token` or `Ref`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Val::Text(s) | Val::Name(s) | Val::Token(s) | Val::Ref(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[Val]> {
        match self {
            Val::List(items) => Some(items),
            _ => None,
        }
    }
    pub fn as_object(&self) -> Option<&Object> {
        match self {
            Val::Object(o) => Some(o),
            _ => None,
        }
    }
}

/// A `when`: every term must hold (AND only).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cond {
    pub terms: Vec<CondTerm>,
}

/// One condition, optionally negated (`!panel.info`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CondTerm {
    pub negated: bool,
    /// A [`schema::Condition::name`].
    pub name: String,
}

/// A state-keyed value: `{"value": base, "if": {cond: v, ...}}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Keyed {
    pub base: Box<Val>,
    /// In DOCUMENT ORDER: when several conditions hold, the first wins. Each condition is one term.
    pub cases: Vec<(Cond, Val)>,
}

/// What a fragment's references resolve against and which scope it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ctx {
    /// The package's palette names (a lower-case ink reference must be one of them: `E0204`).
    pub palette: Vec<String>,
    /// The package's asset ids (an asset or face reference must be one of them: `E0203`).
    pub assets: Vec<String>,
    /// The package's declared format: a field whose `since` is above it is `E0107`.
    pub format: Since,
    /// Inside a strip card: `item.*` bindings and conditions are legal here and nowhere else.
    pub in_strip_card: bool,
}

impl Ctx {
    /// An empty 1.0 context: no palette, no assets, format 1.0, outside a strip card.
    pub fn v1() -> Ctx {
        Ctx {
            palette: Vec::new(),
            assets: Vec::new(),
            format: schema::CURRENT,
            in_strip_card: false,
        }
    }
}

/// Parse a package. Pure: the same bytes always give the same [`Parsed`].
///
/// The input is size-checked and read strictly (`E0100`), the version gate answers alone (`E0401`,
/// `E0402`, `E0403`), then the table walker judges the whole document and builds the typed [`Package`]
/// when it finds no error.
pub fn parse(bytes: &[u8]) -> Parsed {
    let root = match decode(bytes, "") {
        Ok(root) => root,
        Err(finding) => return Parsed::failed(finding),
    };
    let format = match version_gate(&root) {
        Ok(format) => format,
        Err(finding) => return Parsed::failed(finding),
    };
    let ctx = Ctx {
        palette: Vec::new(),
        assets: Vec::new(),
        format,
        in_strip_card: false,
    };
    let (object, mut findings) = walk_fragment(&schema::ROOT, &root, "", &ctx);
    let package = object.and_then(|object| Package::from_object(&object, format));
    if package.is_none() && !has_error(&findings) {
        // Unreachable while the table carries every key `Package` reads; a finding, never a panic.
        findings.push(Finding::new(
            &codes::E0103,
            "",
            "the parsed package lacks a top-level key the format requires",
            "this is a bug in the format table, not in the package",
        ));
    }
    Parsed { package, findings }
}

/// Walk `json` against `node` as if it sat at `pointer` in a package, resolving references against `ctx`:
/// the SAME walker [`parse`] runs, exposed so wave B's lint and tests can check one fragment (a cell, a
/// control item) without a whole package. Returns the typed object when it has no `Error` finding, and
/// every finding. `json` that is not JSON is `E0100` at `pointer`; JSON that is not an object is `E0104`
/// there. No version gate runs here: `ctx.format` is the declared format.
pub fn validate_json(
    node: &'static schema::Node,
    json: &str,
    pointer: &str,
    ctx: &Ctx,
) -> (Option<Object>, Vec<Finding>) {
    match decode(json.as_bytes(), pointer) {
        Ok(root) => walk_fragment(node, &root, pointer, ctx),
        Err(finding) => (None, vec![finding]),
    }
}

impl Parsed {
    fn failed(finding: Finding) -> Parsed {
        Parsed {
            package: None,
            findings: vec![finding],
        }
    }
}

impl Package {
    /// The top-level scalars out of the walked ROOT object. `None` only if the table lacks a key this
    /// struct reads (a table bug); a walked package always has them.
    fn from_object(object: &Object, format: Since) -> Option<Package> {
        let text = |key: &str| object.get(key).and_then(Val::as_str).map(str::to_owned);
        let app = match object.get("app") {
            Some(Val::Object(range)) => {
                let bound = |key: &str| range.get(key).and_then(Val::as_str).map(str::to_owned);
                Some(AppRange {
                    min: bound("min"),
                    max: bound("max"),
                })
            }
            _ => None,
        };
        let palette = match object.get("palette") {
            Some(Val::Object(colours)) => colours
                .fields
                .iter()
                .map(|(name, value)| Some((name.clone(), parse_hex_colour(value.as_str()?)?)))
                .collect::<Option<Vec<_>>>()?,
            _ => Vec::new(),
        };
        let assets = match object.get("assets") {
            Some(Val::Object(files)) => files
                .fields
                .iter()
                .map(|(id, svg)| Some((id.clone(), svg.as_str()?.to_owned())))
                .collect::<Option<Vec<_>>>()?,
            _ => Vec::new(),
        };
        Some(Package {
            format,
            id: text("id")?,
            version: text("version")?,
            name: text("name")?,
            author: text("author")?,
            license: text("license")?,
            app,
            palette,
            assets,
            layout: object.get("layout")?.as_object()?.clone(),
        })
    }
}

// ---------------------------------------------------------------------------------------------------
// Reading the bytes and the version gate
// ---------------------------------------------------------------------------------------------------

/// The bytes as one JSON document with its objects in document order, or `E0100` at `pointer`.
fn decode(bytes: &[u8], pointer: &str) -> Result<Ordered, Finding> {
    if bytes.len() > MAX_PACKAGE_BYTES {
        return Err(Finding::new(
            &codes::E0100,
            pointer,
            format!(
                "the input is {} bytes; a package may be at most {MAX_PACKAGE_BYTES}",
                bytes.len()
            ),
            "a HUD package is one small JSON file; keep each inline SVG to a few KiB",
        ));
    }
    decode_ordered(bytes, DECODE_MEMORY_BYTES).map_err(|error| {
        let error = error.to_string();
        if error.contains("budget exceeded") {
            return Finding::new(
                &codes::E0100,
                pointer,
                format!(
                    "the package is too large to read: it decodes to more than {DECODE_MEMORY_BYTES} bytes"
                ),
                "a HUD package is a small description of a layout: shorten the SVG assets and drop repeated or unused parts",
            );
        }
        Finding::new(
            &codes::E0100,
            pointer,
            format!("not one valid JSON document: {error}"),
            "write one UTF-8 JSON object, with no comments, no trailing commas and no byte order mark",
        )
    })
}

/// `format` must be `MAJOR.MINOR` and one this build reads; the answer is one finding at `/format`.
/// An older app cannot know whether an unknown key of a newer minor is cosmetic, so this runs before
/// anything else is judged.
fn version_gate(root: &Ordered) -> Result<Since, Finding> {
    if !matches!(root, Ordered::Object(_)) {
        return Err(Finding::new(
            &codes::E0104,
            "",
            format!("a package is a JSON object, found {}", root.type_name()),
            "write the package as one JSON object",
        ));
    }
    let malformed = |why: String| {
        Finding::new(
            &codes::E0401,
            "/format",
            why,
            "write \"format\": \"MAJOR.MINOR\", for example \"1.0\"",
        )
    };
    let Some(declared) = root.get("format") else {
        return Err(malformed("`format` is missing".to_owned()));
    };
    let Ordered::String(text) = declared else {
        return Err(malformed(format!(
            "`format` must be a string, found {}",
            declared.type_name()
        )));
    };
    let Some((major, minor)) = parse_format(text) else {
        return Err(malformed(format!(
            "`{}` is not MAJOR.MINOR with two plain decimal numbers",
            clip(text)
        )));
    };
    let hint = format!("this PlxNative reads HUD format {}", supported_text());
    match schema::SUPPORTED
        .iter()
        .find(|(m, _)| u64::from(*m) == major)
    {
        Some(&(m, highest)) if minor <= u64::from(highest) => {
            Ok((m, u8::try_from(minor).unwrap_or(highest)))
        }
        Some(_) => Err(Finding::new(&codes::E0402, "/format", REASON_NEWER, hint)),
        None => {
            let newer = schema::SUPPORTED.iter().all(|(m, _)| major > u64::from(*m));
            let reason = if newer { REASON_NEWER } else { REASON_OLDER };
            Err(Finding::new(&codes::E0403, "/format", reason, hint))
        }
    }
}

/// The formats this build reads, for a hint: `1.0` or `1.0 to 1.3`, one range per supported major.
fn supported_text() -> String {
    schema::SUPPORTED
        .iter()
        .map(|&(major, minor)| {
            if minor == 0 {
                format!("{major}.0")
            } else {
                format!("{major}.0 to {major}.{minor}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Validate a decoded fragment as an object of `node`.
fn walk_fragment(
    node: &'static Node,
    root: &Ordered,
    pointer: &str,
    ctx: &Ctx,
) -> (Option<Object>, Vec<Finding>) {
    let Ordered::Object(pairs) = root else {
        let finding = Finding::new(
            &codes::E0104,
            pointer,
            format!(
                "expected an object for `{}`, found {}",
                node.name,
                root.type_name()
            ),
            "write a JSON object here",
        );
        return (None, vec![finding]);
    };
    let mut walker = Walker::new(ctx);
    let scope = Scope {
        in_card: ctx.in_strip_card || node.name == CARD_NODE,
        cmd: None,
    };
    let object = walker.walk_object(node, pairs, pointer, scope);
    (object, walker.findings)
}

// ---------------------------------------------------------------------------------------------------
// The walker
// ---------------------------------------------------------------------------------------------------

/// What the values below an object may depend on: whether a strip card encloses them (`item.*` names are
/// legal), and the command of the control whose keys are being walked (a `face` map needs it).
#[derive(Clone, Copy)]
struct Scope {
    in_card: bool,
    cmd: Option<&'static Cmd>,
}

/// The table walker: it holds the references a package declares and the findings so far.
struct Walker {
    /// The declared format: a key, command, condition or binding newer than it is `E0107`.
    format: Since,
    /// The declared palette names (an ink reference must be one of them).
    palette: BTreeSet<String>,
    /// The declared asset ids (an asset or face reference must be one of them).
    assets: BTreeSet<String>,
    findings: Vec<Finding>,
    /// How many findings so far are errors; an object is typed only if this did not move while it was walked.
    errors: usize,
}

impl Walker {
    fn new(ctx: &Ctx) -> Walker {
        Walker {
            format: ctx.format,
            palette: ctx.palette.iter().cloned().collect(),
            assets: ctx.assets.iter().cloned().collect(),
            findings: Vec::new(),
            errors: 0,
        }
    }

    fn emit(&mut self, finding: Finding) {
        if finding.is_error() {
            self.errors += 1;
        }
        self.findings.push(finding);
    }

    fn report(
        &mut self,
        code: &Code,
        pointer: &str,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) {
        self.emit(Finding::new(code, pointer, message, hint));
    }

    fn report_near(
        &mut self,
        code: &Code,
        pointer: &str,
        message: impl Into<String>,
        hint: impl Into<String>,
        near: Vec<String>,
    ) {
        self.emit(Finding::new(code, pointer, message, hint).with_nearest(near));
    }

    /// `E0104`: the JSON value is not of the kind `ty` takes.
    fn wrong_type(&mut self, ty: &Ty, json: &Ordered, pointer: &str) -> Option<Val> {
        let wanted = describe(ty);
        self.report(
            &codes::E0104,
            pointer,
            format!("expected {wanted}, found {}", json.type_name()),
            format!("write {wanted} here"),
        );
        None
    }

    /// A closed-vocabulary name that is not in `names`.
    fn unknown_name(&mut self, code: &Code, pointer: &str, what: &str, got: &str, names: &[&str]) {
        let near = nearest(clip(got), names.iter().copied());
        self.report_near(
            code,
            pointer,
            format!("`{}` is not a known {what}", clip(got)),
            format!("write one of: {}", names.join(", ")),
            near,
        );
    }

    fn since_gate(&mut self, since: Since, what: &str, pointer: &str) -> bool {
        if since <= self.format {
            return true;
        }
        self.report(
            &codes::E0107,
            pointer,
            format!(
                "{what} arrived in format {}, and this package declares {}",
                since_text(since),
                since_text(self.format)
            ),
            format!(
                "declare \"format\": \"{}\" or leave it out",
                since_text(since)
            ),
        );
        false
    }

    // ----- objects -----

    /// An object against its node: the keys, the values, the required keys, the one-of rule and the
    /// node's (and command's) rules. `Some` only if nothing below it is an error.
    fn walk_object(
        &mut self,
        node: &'static Node,
        pairs: &[(String, Ordered)],
        pointer: &str,
        scope: Scope,
    ) -> Option<Object> {
        let errors_before = self.errors;
        let cmd = if matches!(node.kind, NodeKind::CommandDispatched) {
            let Some(cmd) = self.resolve_command(node, pairs, pointer) else {
                self.walk_unresolved(node, pairs, pointer, scope);
                return None;
            };
            Some(cmd)
        } else {
            None
        };
        let field_scope = Scope {
            in_card: scope.in_card,
            cmd,
        };
        let mut seen = BTreeSet::new();
        let first: Vec<bool> = pairs.iter().map(|(k, _)| seen.insert(k.as_str())).collect();
        let mut slots: Vec<Vec<Finding>> = pairs.iter().map(|_| Vec::new()).collect();
        let mut values: Vec<Option<Val>> = vec![None; pairs.len()];
        let mut done = vec![false; pairs.len()];
        // Pass 0 walks what later references resolve against (the palette and the assets); pass 1 the
        // rest. Each key's findings go back into its own slot, so they come out in document order.
        for pass in 0..2 {
            for (i, (key, value)) in pairs.iter().enumerate() {
                let declares = first[i]
                    && lookup(node, cmd, key)
                        .is_some_and(|(f, _)| matches!(f.ty, Ty::Palette | Ty::Assets));
                if done[i] || declares != (pass == 0) {
                    continue;
                }
                done[i] = true;
                let mark = self.findings.len();
                values[i] = self.walk_pair(node, cmd, field_scope, key, value, pointer, first[i]);
                slots[i] = self.findings.split_off(mark);
            }
        }
        for slot in slots {
            self.findings.extend(slot);
        }
        let fields: Vec<(String, Val)> = pairs
            .iter()
            .zip(values)
            .filter_map(|((key, _), value)| value.map(|v| (key.clone(), v)))
            .collect();

        let present = |key: &str| pairs.iter().any(|(k, _)| k == key);
        // A key that arrived after the declared format cannot be required of it.
        let format = self.format;
        let required = move |field: &Field| field.required && field.since <= format;
        for field in node.fields {
            if required(field) && !present(field.key) {
                self.missing_key(&codes::E0103, field, pointer);
            }
        }
        if let Some(cmd) = cmd {
            for field in cmd.params {
                if required(field) && !present(field.key) {
                    self.missing_key(&codes::E0303, field, pointer);
                }
            }
        }
        if let NodeKind::OneOf(kinds) = node.kind {
            let found = kinds.iter().filter(|k| present(**k)).count();
            if found != 1 {
                self.report(
                    &codes::E0106,
                    pointer,
                    format!(
                        "a `{}` holds exactly one of its content keys, and this one has {found}",
                        node.name
                    ),
                    format!("keep exactly one of: {}", kinds.join(", ")),
                );
            }
        }
        let face_key = node
            .fields
            .iter()
            .find(|f| matches!(f.ty, Ty::Face))
            .map(|f| f.key);
        self.check_rules(node.rules, false, pointer, pairs, &fields, face_key);
        if let Some(cmd) = cmd {
            self.check_rules(cmd.rules, true, pointer, pairs, &fields, face_key);
        }
        if self.errors != errors_before {
            return None;
        }
        Some(Object {
            node: node.name,
            pointer: pointer.to_owned(),
            fields,
        })
    }

    /// The keys the NODE owns, judged when the command could not be resolved, so that one round reports
    /// every mistake of the item. A key the node does not have may be a parameter of the command the author
    /// meant, which nobody can name, so it is not reported; the cross-key rules need the command and are
    /// skipped. Whatever the command's own finding said stands.
    fn walk_unresolved(
        &mut self,
        node: &'static Node,
        pairs: &[(String, Ordered)],
        pointer: &str,
        scope: Scope,
    ) {
        let field_scope = Scope {
            in_card: scope.in_card,
            cmd: None,
        };
        let mut seen = BTreeSet::new();
        for (key, value) in pairs {
            let first = seen.insert(key.as_str());
            let owned = lookup(node, None, key).is_some_and(|(f, _)| !matches!(f.ty, Ty::Command));
            if !first || owned {
                self.walk_pair(node, None, field_scope, key, value, pointer, first);
            }
        }
        let format = self.format;
        for field in node.fields {
            let skipped = matches!(field.ty, Ty::Command);
            if field.required
                && field.since <= format
                && !skipped
                && !pairs.iter().any(|(k, _)| k == field.key)
            {
                self.missing_key(&codes::E0103, field, pointer);
            }
        }
    }

    fn missing_key(&mut self, code: &Code, field: &Field, pointer: &str) {
        self.report(
            code,
            pointer,
            format!("the required key `{}` is missing", field.key),
            format!("add \"{}\": {}", field.key, describe(&field.ty)),
        );
    }

    /// The command a control names, read before any other key of the item is judged. `None` after a
    /// finding (`E0103`, `E0104`, `E0301`, or `E0107`): the item's other keys are then judged by no
    /// command ([`Self::walk_unresolved`]).
    fn resolve_command(
        &mut self,
        node: &'static Node,
        pairs: &[(String, Ordered)],
        pointer: &str,
    ) -> Option<&'static Cmd> {
        let Some(field) = node.fields.iter().find(|f| matches!(f.ty, Ty::Command)) else {
            self.report(
                &codes::E0103,
                pointer,
                format!("the table gives `{}` no command key", node.name),
                "this is a bug in the format table, not in the package",
            );
            return None;
        };
        let at = pointer_push(pointer, field.key);
        let Some((_, value)) = pairs.iter().find(|(k, _)| k == field.key) else {
            self.missing_key(&codes::E0103, field, pointer);
            return None;
        };
        let Ordered::String(name) = value else {
            return self.wrong_type(&field.ty, value, &at).and(None);
        };
        let Some(cmd) = schema::command(name) else {
            let names: Vec<&str> = schema::COMMANDS.iter().map(|c| c.name).collect();
            self.unknown_name(&codes::E0301, &at, "command", name, &names);
            return None;
        };
        if !self.since_gate(cmd.since, &format!("the command `{}`", cmd.name), &at) {
            return None;
        }
        Some(cmd)
    }

    /// One key of an object: classify it, then walk its value. `first` is false for a repeat.
    #[allow(clippy::too_many_arguments)]
    fn walk_pair(
        &mut self,
        node: &'static Node,
        cmd: Option<&'static Cmd>,
        scope: Scope,
        key: &str,
        value: &Ordered,
        pointer: &str,
        first: bool,
    ) -> Option<Val> {
        let at = pointer_push(pointer, key);
        if !first {
            self.report(
                &codes::E0101,
                &at,
                format!("`{}` is written more than once", clip(key)),
                "keep one; the first is the one the app would read",
            );
            return None;
        }
        let Some((field, is_param)) = lookup(node, cmd, key) else {
            self.unknown_key(node, cmd, key, &at);
            return None;
        };
        if !self.since_gate(field.since, &format!("the key `{}`", field.key), &at) {
            return None;
        }
        if let (Ty::Command, Some(cmd)) = (field.ty, cmd) {
            return Some(Val::Name(cmd.name.to_owned()));
        }
        let mark = self.findings.len();
        let walked = self.walk_typed(field.ty, field.keyable, value, &at, scope);
        if is_param {
            for finding in &mut self.findings[mark..] {
                as_param_finding(finding);
            }
        }
        walked
    }

    fn unknown_key(&mut self, node: &'static Node, cmd: Option<&'static Cmd>, key: &str, at: &str) {
        let mut names: Vec<&str> = node.fields.iter().map(|f| f.key).collect();
        let (code, owner) = match cmd {
            Some(cmd) => {
                names.extend(cmd.params.iter().map(|f| f.key));
                (
                    &codes::E0302,
                    format!("a control with `command: {}`", cmd.name),
                )
            }
            None => (&codes::E0102, format!("`{}`", node.name)),
        };
        let near = nearest(clip(key), names.iter().copied());
        let hint = match near.first() {
            Some(best) => format!("did you mean `{best}`? The keys are: {}", names.join(", ")),
            None => format!("the keys are: {}", names.join(", ")),
        };
        self.report_near(
            code,
            at,
            format!("`{}` is not a key of {owner}", clip(key)),
            hint,
            near,
        );
    }

    // ----- cross-key rules -----

    /// The table's [`Rule`]s of a node (`command` false: `E01xx`) or of a command (`E03xx`), judged on the
    /// keys that are present and the values that walked.
    fn check_rules(
        &mut self,
        rules: &[Rule],
        command: bool,
        pointer: &str,
        pairs: &[(String, Ordered)],
        fields: &[(String, Val)],
        face_key: Option<&str>,
    ) {
        let (missing, combo, bound) = if command {
            (&codes::E0305, &codes::E0305, &codes::E0304)
        } else {
            (&codes::E0103, &codes::E0106, &codes::E0105)
        };
        let present = |key: &str| pairs.iter().any(|(k, _)| k == key);
        let value = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| plain(v));
        let name = |key: &str| match value(key) {
            Some(Val::Name(n)) => Some(n.as_str()),
            _ => None,
        };
        for rule in rules {
            match *rule {
                Rule::Requires { key, other } => {
                    if present(key) && !present(other) {
                        self.report(
                            combo,
                            pointer,
                            format!("`{key}` needs `{other}`"),
                            format!("add `{other}`, or remove `{key}`"),
                        );
                    }
                }
                Rule::Excludes { a, b } => {
                    if present(a) && present(b) {
                        self.report(
                            combo,
                            pointer,
                            format!("`{a}` and `{b}` cannot both be written"),
                            format!("keep one of `{a}` and `{b}`"),
                        );
                    }
                }
                Rule::IfValue {
                    key,
                    equals,
                    require,
                    forbid,
                } => {
                    if name(key) == Some(equals) {
                        let when = format!("when `{key}` is `{equals}`");
                        self.rule_effects(
                            missing, combo, pointer, &when, require, forbid, &present,
                        );
                    }
                }
                Rule::IfNotValue {
                    key,
                    equals,
                    require,
                    forbid,
                } => {
                    if value(key).is_some() && name(key) != Some(equals) {
                        let when = format!("unless `{key}` is `{equals}`");
                        self.rule_effects(
                            missing, combo, pointer, &when, require, forbid, &present,
                        );
                    }
                }
                Rule::ForbidPair {
                    a,
                    a_value,
                    b,
                    b_value,
                } => {
                    if name(a) == Some(a_value) && name(b) == Some(b_value) {
                        self.report(
                            combo,
                            pointer,
                            format!("`{a}: {a_value}` with `{b}: {b_value}` draws one face twice"),
                            format!("change `{a}` or `{b}`; `{a_value}` needs a different `{b}`"),
                        );
                    }
                }
                Rule::Disjoint { a, b } => {
                    if let (Some(Val::List(x)), Some(Val::List(y))) = (value(a), value(b)) {
                        let shared = x
                            .iter()
                            .filter_map(Val::as_str)
                            .find(|n| y.iter().filter_map(Val::as_str).any(|m| m == *n));
                        if let Some(shared) = shared {
                            self.report(
                                combo,
                                pointer,
                                format!("`{shared}` is in both `{a}` and `{b}`"),
                                format!("list `{shared}` in `{a}` or in `{b}`, not both"),
                            );
                        }
                    }
                }
                Rule::NonZero(key) => {
                    if matches!(value(key), Some(Val::Int(0))) {
                        self.report(
                            bound,
                            &pointer_push(pointer, key),
                            format!("`{key}` is 0, which does nothing"),
                            format!("write a nonzero `{key}`"),
                        );
                    }
                }
                Rule::NameOnlyWith {
                    key,
                    name: only,
                    other,
                    other_value,
                } => {
                    let other_failed = present(other) && value(other).is_none();
                    if name(key) == Some(only) && !other_failed && name(other) != Some(other_value)
                    {
                        self.report(
                            combo,
                            &pointer_push(pointer, key),
                            format!("`{only}` is only for `{other}: {other_value}`"),
                            format!("use another `{key}`, or bind `{other}` to `{other_value}`"),
                        );
                    }
                }
                Rule::AtMost { key, other } => {
                    if let (Some(Val::Int(k)), Some(Val::Int(o))) = (value(key), value(other)) {
                        if k > o {
                            self.report(
                                bound,
                                &pointer_push(pointer, key),
                                format!("`{key}` is {k}, more than `{other}` ({o})"),
                                format!("write a `{key}` of at most {o}, or raise `{other}`"),
                            );
                        }
                    }
                }
                Rule::FaceMapOnlyWhen { param, equals } => {
                    let is_map = face_key
                        .and_then(|k| value(k))
                        .is_some_and(|v| matches!(v, Val::Object(_)));
                    if is_map && name(param) != Some(equals) {
                        if let Some(face) = face_key {
                            self.report(
                                combo,
                                &pointer_push(pointer, face),
                                format!("a per-state `{face}` needs `{param}: {equals}` here"),
                                format!("write a single face, or `{param}: {equals}`"),
                            );
                        }
                    }
                }
            }
        }
    }

    /// The consequences of an `IfValue` / `IfNotValue` rule that applies: the keys it requires, and the
    /// keys it forbids (reported at the forbidden key).
    #[allow(clippy::too_many_arguments)]
    fn rule_effects(
        &mut self,
        missing: &Code,
        combo: &Code,
        pointer: &str,
        when: &str,
        require: &[&str],
        forbid: &[&str],
        present: &dyn Fn(&str) -> bool,
    ) {
        for key in require {
            if !present(key) {
                self.report(
                    missing,
                    pointer,
                    format!("`{key}` is required {when}"),
                    format!("add `{key}`"),
                );
            }
        }
        for key in forbid {
            if present(key) {
                self.report(
                    combo,
                    &pointer_push(pointer, key),
                    format!("`{key}` is not allowed {when}"),
                    format!("remove `{key}`"),
                );
            }
        }
    }

    // ----- values -----

    /// A JSON value against a type. `keyable`: an object may be a state-keyed value.
    fn walk_typed(
        &mut self,
        ty: Ty,
        keyable: bool,
        json: &Ordered,
        pointer: &str,
        scope: Scope,
    ) -> Option<Val> {
        if let Ordered::Object(pairs) = json {
            if !takes_object(&ty) {
                return self.scalar_object(ty, keyable, pairs, pointer, scope);
            }
        }
        match ty {
            Ty::Bool => match json {
                Ordered::Bool(b) => Some(Val::Bool(*b)),
                _ => self.wrong_type(&ty, json, pointer),
            },
            Ty::Int { min, max } => self.walk_int(&ty, min, max, json, pointer),
            Ty::Num { min, max } => {
                let Ordered::Number(n) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                match n.as_f64() {
                    Some(v) if v >= min && v <= max => Some(Val::Num(v)),
                    _ => {
                        self.report(
                            &codes::E0105,
                            pointer,
                            format!("{n} is outside {min} to {max}"),
                            format!("write a number from {min} to {max}"),
                        );
                        None
                    }
                }
            }
            Ty::Text { max_len } => {
                let Ordered::String(s) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                if s.chars().count() > max_len || s.chars().any(char::is_control) {
                    self.report(
                        &codes::E0105,
                        pointer,
                        format!("the text is longer than {max_len} characters or holds a control character"),
                        format!("write at most {max_len} plain characters"),
                    );
                    return None;
                }
                Some(Val::Text(s.clone()))
            }
            Ty::Pattern(pattern) => {
                let Ordered::String(s) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                if !matches_pattern(pattern, s) {
                    self.report(
                        &codes::E0105,
                        pointer,
                        format!("`{}` is not {}", clip(s), pattern_name(pattern)),
                        format!("write {}", pattern_name(pattern)),
                    );
                    return None;
                }
                Some(Val::Text(s.clone()))
            }
            Ty::Enum(names) => {
                let Ordered::String(s) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                if names.contains(&s.as_str()) {
                    return Some(Val::Name(s.clone()));
                }
                self.unknown_name(&codes::E0201, pointer, "name", s, names);
                None
            }
            Ty::EnumList(names) => self.walk_enum_list(&ty, names, json, pointer),
            Ty::Bind(names) => self.walk_bind(&ty, names, json, pointer, scope),
            Ty::Size => {
                let Ordered::String(s) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                if schema::size_rung(s).is_some() {
                    return Some(Val::Name(s.clone()));
                }
                let names: Vec<&str> = schema::SIZE_RUNGS.iter().map(|(n, _)| *n).collect();
                self.unknown_name(&codes::E0201, pointer, "type-scale rung", s, &names);
                None
            }
            Ty::Gap => match json {
                Ordered::String(s) => {
                    if schema::space_token(s).is_some() {
                        return Some(Val::Name(s.clone()));
                    }
                    let names: Vec<&str> = schema::SPACE_TOKENS.iter().map(|(n, _)| *n).collect();
                    self.unknown_name(&codes::E0201, pointer, "spacing token", s, &names);
                    None
                }
                Ordered::Number(_) => self.walk_int(&ty, 0, schema::GAP_TOP_PX, json, pointer),
                _ => self.wrong_type(&ty, json, pointer),
            },
            Ty::Ink => self.walk_ink(json, pointer),
            Ty::Asset => {
                let Ordered::String(s) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                if self.assets.contains(s.as_str()) {
                    return Some(Val::Ref(s.clone()));
                }
                self.unknown_asset(s, pointer);
                None
            }
            Ty::Face => match json {
                Ordered::String(s) => self.face_single(s, pointer),
                Ordered::Object(pairs) => self.face_map(pairs, pointer, scope),
                _ => self.wrong_type(&ty, json, pointer),
            },
            Ty::Cond => self.walk_cond(json, pointer, scope),
            Ty::Object(name) => {
                let Some(node) = schema::node(name) else {
                    self.report(
                        &codes::E0104,
                        pointer,
                        format!("the table has no node `{name}`"),
                        "this is a bug in the format table, not in the package",
                    );
                    return None;
                };
                let Ordered::Object(pairs) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                let inner = Scope {
                    in_card: scope.in_card || name == CARD_NODE,
                    cmd: None,
                };
                self.walk_object(node, pairs, pointer, inner)
                    .map(Val::Object)
            }
            Ty::List { of, max_len } => {
                let Ordered::Array(items) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                let errors_before = self.errors;
                if items.len() > max_len {
                    self.report(
                        &codes::E0105,
                        pointer,
                        format!(
                            "the array has {} entries; at most {max_len} are allowed",
                            items.len()
                        ),
                        format!("keep at most {max_len} entries"),
                    );
                }
                let mut out = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    if i >= max_len {
                        // The entries past the bound are one finding (above), not one each.
                        break;
                    }
                    let at = pointer_index(pointer, i);
                    out.extend(self.walk_typed(*of, false, item, &at, scope));
                }
                (self.errors == errors_before).then_some(Val::List(out))
            }
            Ty::Palette => self.walk_palette(json, pointer),
            Ty::Assets => self.walk_assets(json, pointer),
            Ty::Command => {
                let Ordered::String(s) = json else {
                    return self.wrong_type(&ty, json, pointer);
                };
                if schema::command(s).is_some() {
                    return Some(Val::Name(s.clone()));
                }
                let names: Vec<&str> = schema::COMMANDS.iter().map(|c| c.name).collect();
                self.unknown_name(&codes::E0301, pointer, "command", s, &names);
                None
            }
            Ty::Either(a, b) => {
                let pick = if kind_fits(a, json) {
                    *a
                } else if kind_fits(b, json) {
                    *b
                } else {
                    return self.wrong_type(&ty, json, pointer);
                };
                self.walk_typed(pick, false, json, pointer, scope)
            }
        }
    }

    fn walk_int(
        &mut self,
        ty: &Ty,
        min: i64,
        max: i64,
        json: &Ordered,
        pointer: &str,
    ) -> Option<Val> {
        let Ordered::Number(n) = json else {
            return self.wrong_type(ty, json, pointer);
        };
        match n.as_i64() {
            Some(v) if v >= min && v <= max => Some(Val::Int(v)),
            Some(_) => self.out_of_range(n, min, max, pointer),
            None if n.is_u64() => self.out_of_range(n, min, max, pointer),
            None => {
                self.report(
                    &codes::E0104,
                    pointer,
                    format!("expected an integer, found {n}"),
                    format!("write {}", describe(ty)),
                );
                None
            }
        }
    }

    fn out_of_range(
        &mut self,
        n: &impl std::fmt::Display,
        min: i64,
        max: i64,
        pointer: &str,
    ) -> Option<Val> {
        self.report(
            &codes::E0105,
            pointer,
            format!("{n} is outside {min} to {max}"),
            format!("write a value from {min} to {max}"),
        );
        None
    }

    /// An object where the type takes none: a state-keyed value if the key allows it, else a finding.
    fn scalar_object(
        &mut self,
        ty: Ty,
        keyable: bool,
        pairs: &[(String, Ordered)],
        pointer: &str,
        scope: Scope,
    ) -> Option<Val> {
        if keyable {
            return self.walk_keyed(ty, pairs, pointer, scope);
        }
        if pairs.iter().any(|(k, _)| k == KEYED_VALUE || k == KEYED_IF) {
            self.report(
                &codes::E0504,
                pointer,
                "this key cannot be state-keyed (and a state-keyed value cannot hold another)",
                format!("write a plain {} here", describe(&ty)),
            );
            return None;
        }
        self.wrong_type(&ty, &Ordered::Object(Vec::new()), pointer)
    }

    /// `{"value": v, "if": {"<condition>": v2, ...}}`: exactly those keys, `if` a non-empty object of
    /// single conditions, every value a plain `ty`. The cases stay in document order.
    fn walk_keyed(
        &mut self,
        ty: Ty,
        pairs: &[(String, Ordered)],
        pointer: &str,
        scope: Scope,
    ) -> Option<Val> {
        let find = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v);
        let shape = (pairs.len() == 2)
            .then(|| (find(KEYED_VALUE), find(KEYED_IF)))
            .and_then(|(value, cases)| Some((value?, cases?)));
        let Some((value, Ordered::Object(case_pairs))) = shape else {
            self.keyed_shape(pointer);
            return None;
        };
        if case_pairs.is_empty() {
            self.keyed_shape(pointer);
            return None;
        }
        let base = self.walk_typed(ty, false, value, &pointer_push(pointer, KEYED_VALUE), scope);
        let if_at = pointer_push(pointer, KEYED_IF);
        let mut seen = BTreeSet::new();
        let mut cases = Vec::new();
        let mut ok = base.is_some();
        for (condition, case) in case_pairs {
            let at = pointer_push(&if_at, condition);
            if !seen.insert(condition.as_str()) {
                self.report(
                    &codes::E0101,
                    &at,
                    format!(
                        "the condition `{}` is written more than once",
                        clip(condition)
                    ),
                    "keep one; the first is the one the app would read",
                );
                ok = false;
                continue;
            }
            let term = self.check_term(condition, &at, scope);
            let walked = self.walk_typed(ty, false, case, &at, scope);
            match (term, walked) {
                (Some(term), Some(walked)) => cases.push((Cond { terms: vec![term] }, walked)),
                _ => ok = false,
            }
        }
        match (ok, base) {
            (true, Some(base)) => Some(Val::Keyed(Keyed {
                base: Box::new(base),
                cases,
            })),
            _ => None,
        }
    }

    fn keyed_shape(&mut self, pointer: &str) {
        self.report(
            &codes::E0504,
            pointer,
            "a state-keyed value is exactly {\"value\": v, \"if\": {\"<condition>\": v2, ...}}, with at least one condition",
            "write {\"value\": <the usual value>, \"if\": {\"panel.info\": <value while it holds>}}",
        );
    }

    fn walk_enum_list(
        &mut self,
        ty: &Ty,
        names: &'static [&'static str],
        json: &Ordered,
        pointer: &str,
    ) -> Option<Val> {
        let Ordered::Array(items) = json else {
            return self.wrong_type(ty, json, pointer);
        };
        let errors_before = self.errors;
        let mut seen = BTreeSet::new();
        let mut out = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let at = pointer_index(pointer, i);
            let Ordered::String(s) = item else {
                self.report(
                    &codes::E0104,
                    &at,
                    format!("expected a name, found {}", item.type_name()),
                    format!("write one of: {}", names.join(", ")),
                );
                continue;
            };
            if !names.contains(&s.as_str()) {
                self.unknown_name(&codes::E0201, &at, "name", s, names);
            } else if !seen.insert(s.as_str()) {
                self.report(
                    &codes::E0105,
                    &at,
                    format!("`{s}` is listed more than once"),
                    "list each name once",
                );
            } else {
                out.push(Val::Name(s.clone()));
            }
        }
        (self.errors == errors_before).then_some(Val::List(out))
    }

    fn walk_bind(
        &mut self,
        ty: &Ty,
        names: &'static [&'static str],
        json: &Ordered,
        pointer: &str,
        scope: Scope,
    ) -> Option<Val> {
        let Ordered::String(s) = json else {
            return self.wrong_type(ty, json, pointer);
        };
        let visible: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| scope.in_card || !schema::is_card_scoped(n))
            .collect();
        if visible.contains(&s.as_str()) {
            let newer = schema::binding(s).map(|b| b.since);
            if let Some(since) = newer {
                if !self.since_gate(since, &format!("the binding `{s}`"), pointer) {
                    return None;
                }
            }
            return Some(Val::Name(s.clone()));
        }
        if names.contains(&s.as_str()) {
            self.report_near(
                &codes::E0201,
                pointer,
                format!("`{s}` is only available inside a strip card"),
                format!(
                    "use it under `strip.card`, or write one of: {}",
                    visible.join(", ")
                ),
                nearest(clip(s), visible.iter().copied()),
            );
            return None;
        }
        self.unknown_name(&codes::E0201, pointer, "binding", s, &visible);
        None
    }

    fn unknown_asset(&mut self, id: &str, pointer: &str) {
        let near = nearest(clip(id), self.assets.iter());
        let hint = if self.assets.is_empty() {
            "declare the asset under `assets` first, or use a `builtin:<icon>` face".to_owned()
        } else {
            format!(
                "write one of the declared assets: {}",
                listed(self.assets.iter().map(String::as_str), self.assets.len())
            )
        };
        self.report_near(
            &codes::E0203,
            pointer,
            format!("`{}` is not an asset this package declares", clip(id)),
            hint,
            near,
        );
    }

    // ----- inks, palette, assets -----

    fn walk_ink(&mut self, json: &Ordered, pointer: &str) -> Option<Val> {
        let Ordered::String(s) = json else {
            return self.wrong_type(&Ty::Ink, json, pointer);
        };
        if schema::ink_token(s).is_some() {
            return Some(Val::Token(s.clone()));
        }
        if is_token_shaped(s) {
            let names: Vec<&str> = schema::INK_TOKENS.iter().map(|(n, _)| *n).collect();
            self.unknown_name(&codes::E0202, pointer, "theme token", s, &names);
            return None;
        }
        if is_palette_name(s) {
            if self.palette.contains(s.as_str()) {
                return Some(Val::Ref(s.clone()));
            }
            let near = nearest(clip(s), self.palette.iter());
            let upper = s.to_ascii_uppercase();
            let hint = if schema::ink_token(&upper).is_some() {
                format!(
                    "theme tokens are upper case: write `{upper}`, or declare `{s}` in `palette`"
                )
            } else {
                format!("declare `{s}` in `palette`, or use a theme token (UPPER_CASE)")
            };
            self.report_near(
                &codes::E0204,
                pointer,
                format!(
                    "`{}` is not a colour this package's palette declares",
                    clip(s)
                ),
                hint,
                near,
            );
            return None;
        }
        self.report(
            &codes::E0105,
            pointer,
            format!("`{}` is neither a theme token nor a palette name", clip(s)),
            "put the colour in `palette` as \"name\": \"#rrggbb\" and write the name here",
        );
        None
    }

    fn walk_palette(&mut self, json: &Ordered, pointer: &str) -> Option<Val> {
        let Ordered::Object(pairs) = json else {
            return self.wrong_type(&Ty::Palette, json, pointer);
        };
        let errors_before = self.errors;
        let mut seen = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut fields = Vec::new();
        for (name, value) in pairs {
            let at = pointer_push(pointer, name);
            if !seen.insert(name.as_str()) {
                self.report(
                    &codes::E0101,
                    &at,
                    format!("the colour `{}` is written more than once", clip(name)),
                    "keep one",
                );
                continue;
            }
            let named = is_palette_name(name);
            if named {
                names.insert(name.clone());
            } else {
                self.report(
                    &codes::E0505,
                    &at,
                    format!("`{}` is not a palette name", clip(name)),
                    "name a colour with lower-case letters, digits and underscores, starting with a letter (theme tokens are upper case)",
                );
            }
            match value {
                Ordered::String(hex) if parse_hex_colour(hex).is_some() => {
                    if named {
                        fields.push((name.clone(), Val::Text(hex.clone())));
                    }
                }
                Ordered::String(hex) => self.report(
                    &codes::E0506,
                    &at,
                    format!("`{}` is not #rrggbb", clip(hex)),
                    "write # and six lower-case hex digits, for example \"#f2b535\"; alpha goes on the element that uses the colour",
                ),
                other => self.report(
                    &codes::E0104,
                    &at,
                    format!("expected a \"#rrggbb\" string, found {}", other.type_name()),
                    "write # and six lower-case hex digits, for example \"#f2b535\"",
                ),
            }
        }
        self.palette = names;
        (self.errors == errors_before).then(|| {
            Val::Object(Object {
                node: PALETTE_NODE,
                pointer: pointer.to_owned(),
                fields,
            })
        })
    }

    fn walk_assets(&mut self, json: &Ordered, pointer: &str) -> Option<Val> {
        let Ordered::Object(pairs) = json else {
            return self.wrong_type(&Ty::Assets, json, pointer);
        };
        let errors_before = self.errors;
        let mut seen = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut fields = Vec::new();
        for (id, value) in pairs {
            let at = pointer_push(pointer, id);
            if !seen.insert(id.as_str()) {
                self.report(
                    &codes::E0101,
                    &at,
                    format!("the asset `{}` is written more than once", clip(id)),
                    "keep one",
                );
                continue;
            }
            let named = is_id(id);
            if named {
                ids.insert(id.clone());
            } else {
                self.report(
                    &codes::E0105,
                    &at,
                    format!("`{}` is not an asset id", clip(id)),
                    format!("write {}", pattern_name(Pattern::Id)),
                );
            }
            let Ordered::String(svg) = value else {
                self.report(
                    &codes::E0104,
                    &at,
                    format!(
                        "expected the SVG source as a string, found {}",
                        value.type_name()
                    ),
                    "write the whole <svg>...</svg> document as one JSON string",
                );
                continue;
            };
            let accepted = match svg_subset::check_asset(&at, svg) {
                Ok(_) => true,
                Err(found) => {
                    let accepted = !has_error(&found);
                    found.into_iter().for_each(|f| self.emit(f));
                    accepted
                }
            };
            if accepted && named {
                fields.push((id.clone(), Val::Text(svg.clone())));
            }
        }
        self.assets = ids;
        (self.errors == errors_before).then(|| {
            Val::Object(Object {
                node: ASSETS_NODE,
                pointer: pointer.to_owned(),
                fields,
            })
        })
    }

    // ----- faces -----

    /// A single face: `builtin:<icon>` or a declared asset id.
    fn face_single(&mut self, face: &str, pointer: &str) -> Option<Val> {
        if let Some(icon) = face.strip_prefix(schema::BUILTIN_FACE_PREFIX) {
            if schema::builtin_icon(icon).is_some() {
                return Some(Val::Name(face.to_owned()));
            }
            let all: Vec<String> = schema::BUILTIN_ICONS
                .iter()
                .map(|(n, _)| format!("{}{n}", schema::BUILTIN_FACE_PREFIX))
                .collect();
            let near = nearest(clip(face), &all);
            self.report_near(
                &codes::E0203,
                pointer,
                format!("`{}` is not a built-in icon", clip(face)),
                format!("write one of: {}", all.join(", ")),
                near,
            );
            return None;
        }
        if self.assets.contains(face) {
            return Some(Val::Ref(face.to_owned()));
        }
        self.unknown_asset(face, pointer);
        None
    }

    /// A face inside a map: a string only.
    fn face_leaf(&mut self, json: &Ordered, pointer: &str) -> Option<Val> {
        match json {
            Ordered::String(s) => self.face_single(s, pointer),
            other => {
                self.report(
                    &codes::E0104,
                    pointer,
                    format!(
                        "expected an asset id or `builtin:<icon>`, found {}",
                        other.type_name()
                    ),
                    "write a declared asset id or `builtin:<icon>`",
                );
                None
            }
        }
    }

    /// A per-state face map, legal for a command that has states: its keys are those states, each
    /// required one is present, each value a single face (a ladder state holds a map keyed by seconds).
    fn face_map(
        &mut self,
        pairs: &[(String, Ordered)],
        pointer: &str,
        scope: Scope,
    ) -> Option<Val> {
        let states = match scope.cmd {
            Some(cmd) if !cmd.face_states.is_empty() => cmd.face_states,
            Some(cmd) => {
                self.report(
                    &codes::E0304,
                    pointer,
                    format!("`{}` has one face, not one per state", cmd.name),
                    "write an asset id or `builtin:<icon>`",
                );
                return None;
            }
            None => {
                self.report(
                    &codes::E0304,
                    pointer,
                    "a per-state face belongs to a control",
                    "write an asset id or `builtin:<icon>`",
                );
                return None;
            }
        };
        let errors_before = self.errors;
        let names: Vec<&str> = states.iter().map(|s| s.key).collect();
        let mut seen = BTreeSet::new();
        let mut fields = Vec::new();
        for (key, value) in pairs {
            let at = pointer_push(pointer, key);
            if !seen.insert(key.as_str()) {
                self.report(
                    &codes::E0101,
                    &at,
                    format!("the state `{}` is written more than once", clip(key)),
                    "keep one",
                );
                continue;
            }
            let Some(state) = states.iter().find(|s| s.key == key) else {
                self.report_near(
                    &codes::E0302,
                    &at,
                    format!("`{}` is not a state of this command's face", clip(key)),
                    format!("the states are: {}", names.join(", ")),
                    nearest(clip(key), names.iter().copied()),
                );
                continue;
            };
            let walked = if state.ladder {
                self.face_ladder(value, &at)
            } else {
                self.face_leaf(value, &at)
            };
            fields.extend(walked.map(|v| (key.clone(), v)));
        }
        for state in states.iter().filter(|s| s.required) {
            if !seen.contains(state.key) {
                self.report(
                    &codes::E0303,
                    pointer,
                    format!("the face map lacks the required state `{}`", state.key),
                    format!("add \"{}\": a face", state.key),
                );
            }
        }
        (self.errors == errors_before).then(|| {
            Val::Object(Object {
                node: FACE_MAP_NODE,
                pointer: pointer.to_owned(),
                fields,
            })
        })
    }

    /// A ladder state of a face map: faces keyed by whole seconds, the viewer's Skip intervals.
    fn face_ladder(&mut self, json: &Ordered, pointer: &str) -> Option<Val> {
        let Ordered::Object(pairs) = json else {
            self.report(
                &codes::E0104,
                pointer,
                format!(
                    "expected an object keyed by seconds, found {}",
                    json.type_name()
                ),
                "write {\"10\": <face>, \"30\": <face>}",
            );
            return None;
        };
        let errors_before = self.errors;
        let mut seen = BTreeSet::new();
        let mut fields = Vec::new();
        for (key, value) in pairs {
            let at = pointer_push(pointer, key);
            if !seen.insert(key.as_str()) {
                self.report(
                    &codes::E0101,
                    &at,
                    format!("the rung `{}` is written more than once", clip(key)),
                    "keep one",
                );
                continue;
            }
            if !decimal(key).is_some_and(|n| n > 0) {
                self.report(
                    &codes::E0102,
                    &at,
                    format!("`{}` is not a whole number of seconds", clip(key)),
                    "key each face by seconds, for example \"10\"",
                );
                continue;
            }
            let walked = self.face_leaf(value, &at);
            fields.extend(walked.map(|v| (key.clone(), v)));
        }
        (self.errors == errors_before).then(|| {
            Val::Object(Object {
                node: FACE_LADDER_NODE,
                pointer: pointer.to_owned(),
                fields,
            })
        })
    }

    // ----- conditions -----

    /// A `when`: a condition, or a non-empty array of them (all must hold).
    fn walk_cond(&mut self, json: &Ordered, pointer: &str, scope: Scope) -> Option<Val> {
        let errors_before = self.errors;
        let mut terms = Vec::new();
        match json {
            Ordered::String(raw) => terms.extend(self.check_term(raw, pointer, scope)),
            Ordered::Array(items) if !items.is_empty() => {
                for (i, item) in items.iter().enumerate() {
                    let at = pointer_index(pointer, i);
                    match item {
                        Ordered::String(raw) => terms.extend(self.check_term(raw, &at, scope)),
                        other => self.cond_shape(&format!("found {}", other.type_name()), &at),
                    }
                }
            }
            other => self.cond_shape(&format!("found {}", other.type_name()), pointer),
        }
        (self.errors == errors_before).then_some(Val::Cond(Cond { terms }))
    }

    fn cond_shape(&mut self, found: &str, pointer: &str) {
        self.report(
            &codes::E0502,
            pointer,
            format!("a condition is a string or a non-empty array of strings; {found}"),
            "write \"panel.info\", \"!panel.info\" or [\"panel.info\", \"!transport.paused\"]",
        );
    }

    /// One condition term, `name` or `!name`.
    fn check_term(&mut self, raw: &str, pointer: &str, scope: Scope) -> Option<CondTerm> {
        let (negated, name) = match raw.strip_prefix('!') {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        if name.is_empty() || name.starts_with('!') {
            let what = if raw.is_empty() {
                "an empty condition"
            } else if name.is_empty() {
                "`!` with no condition after it"
            } else {
                "a doubly negated condition"
            };
            self.cond_shape(what, pointer);
            return None;
        }
        let Some(condition) = schema::condition(name) else {
            let visible: Vec<&str> = schema::CONDITIONS
                .iter()
                .map(|c| c.name)
                .filter(|n| scope.in_card || !schema::is_card_scoped(n))
                .collect();
            self.unknown_name(&codes::E0501, pointer, "condition", name, &visible);
            return None;
        };
        if schema::is_card_scoped(name) && !scope.in_card {
            self.report(
                &codes::E0503,
                pointer,
                format!("`{name}` exists only inside a strip card"),
                "use it under `strip.card`, or write another condition",
            );
            return None;
        }
        if !self.since_gate(condition.since, &format!("the condition `{name}`"), pointer) {
            return None;
        }
        Some(CondTerm {
            negated,
            name: name.to_owned(),
        })
    }
}

// ---------------------------------------------------------------------------------------------------
// Helpers of the walker
// ---------------------------------------------------------------------------------------------------

/// The field `key` names on `node`, or else the command's parameter of that name (`true`).
fn lookup(
    node: &'static Node,
    cmd: Option<&'static Cmd>,
    key: &str,
) -> Option<(&'static Field, bool)> {
    if let Some(field) = schema::field(node, key) {
        return Some((field, false));
    }
    cmd.and_then(|c| c.param(key)).map(|field| (field, true))
}

/// A finding under a command parameter reports the command's code for the kinds of mistake a parameter
/// can make (`E0104`, `E0105`, `E0201`: its type, range or name).
fn as_param_finding(finding: &mut Finding) {
    let generic = [codes::E0104.id, codes::E0105.id, codes::E0201.id];
    if generic.contains(&finding.code) {
        finding.code = codes::E0304.id;
        finding.rule = codes::E0304.rule;
        finding.severity = codes::E0304.severity;
    }
}

/// A keyed value's plain face: rules compare plain values, never cases.
fn plain(value: &Val) -> &Val {
    match value {
        Val::Keyed(keyed) => &keyed.base,
        other => other,
    }
}

/// The start of `text` a message quotes and `nearest` compares.
fn clip(text: &str) -> &str {
    match text.char_indices().nth(CLIP_CHARS) {
        Some((end, _)) => &text[..end],
        None => text,
    }
}

/// The first [`HINT_NAMES`] of `names` (`total` of them), comma-separated, and how many more there are.
fn listed<'a>(names: impl Iterator<Item = &'a str>, total: usize) -> String {
    let mut text = names.take(HINT_NAMES).collect::<Vec<_>>().join(", ");
    if total > HINT_NAMES {
        text.push_str(&format!(", and {} more", total - HINT_NAMES));
    }
    text
}

fn since_text(since: Since) -> String {
    format!("{}.{}", since.0, since.1)
}

/// Whether a type is read from a JSON object (so an object value is not a state-keyed value).
fn takes_object(ty: &Ty) -> bool {
    match ty {
        Ty::Object(_) | Ty::Face | Ty::Palette | Ty::Assets | Ty::Cond => true,
        Ty::Either(a, b) => takes_object(a) || takes_object(b),
        Ty::Bool
        | Ty::Int { .. }
        | Ty::Num { .. }
        | Ty::Text { .. }
        | Ty::Pattern(_)
        | Ty::Enum(_)
        | Ty::EnumList(_)
        | Ty::Bind(_)
        | Ty::Size
        | Ty::Gap
        | Ty::Ink
        | Ty::Asset
        | Ty::List { .. }
        | Ty::Command => false,
    }
}

/// Whether the JSON value is of a kind `ty` reads, for `Ty::Either` (the first of the two that fits).
fn kind_fits(ty: &Ty, json: &Ordered) -> bool {
    match ty {
        Ty::Bool => matches!(json, Ordered::Bool(_)),
        Ty::Int { .. } | Ty::Num { .. } => matches!(json, Ordered::Number(_)),
        Ty::Text { .. }
        | Ty::Pattern(_)
        | Ty::Enum(_)
        | Ty::Bind(_)
        | Ty::Size
        | Ty::Ink
        | Ty::Asset
        | Ty::Command => matches!(json, Ordered::String(_)),
        Ty::Gap => matches!(json, Ordered::String(_) | Ordered::Number(_)),
        Ty::Face => matches!(json, Ordered::String(_) | Ordered::Object(_)),
        Ty::Cond => matches!(json, Ordered::String(_) | Ordered::Array(_)),
        Ty::Object(_) | Ty::Palette | Ty::Assets => matches!(json, Ordered::Object(_)),
        Ty::EnumList(_) | Ty::List { .. } => matches!(json, Ordered::Array(_)),
        Ty::Either(a, b) => kind_fits(a, json) || kind_fits(b, json),
    }
}

/// What a type accepts, in words, for a message and a hint.
fn describe(ty: &Ty) -> String {
    match ty {
        Ty::Bool => "true or false".to_owned(),
        Ty::Int { min, max } => format!("an integer from {min} to {max}"),
        Ty::Num { min, max } => format!("a number from {min} to {max}"),
        Ty::Text { max_len } => format!("text of at most {max_len} characters"),
        Ty::Pattern(pattern) => pattern_name(*pattern).to_owned(),
        Ty::Enum(names) => format!("one of: {}", names.join(", ")),
        Ty::EnumList(names) => format!("an array of distinct names from: {}", names.join(", ")),
        Ty::Bind(_) => "a binding name".to_owned(),
        Ty::Size => "a type-scale rung name".to_owned(),
        Ty::Gap => "a spacing token or an integer number of px".to_owned(),
        Ty::Ink => "a theme token (UPPER_CASE) or a palette name (lower case)".to_owned(),
        Ty::Asset => "the id of a declared asset".to_owned(),
        Ty::Face => "an asset id, `builtin:<icon>` or a per-state map".to_owned(),
        Ty::Cond => "a condition name or a non-empty array of them".to_owned(),
        Ty::Object(name) => format!("a `{name}` object"),
        Ty::List { max_len, .. } => format!("an array of at most {max_len} values"),
        Ty::Palette => "an object of palette names to \"#rrggbb\"".to_owned(),
        Ty::Assets => "an object of asset ids to SVG text".to_owned(),
        Ty::Command => "a command name".to_owned(),
        Ty::Either(a, b) => format!("{} or {}", describe(a), describe(b)),
    }
}

// ---------------------------------------------------------------------------------------------------
// String shapes (a `Pattern` is a closed grammar, never a regular expression)
// ---------------------------------------------------------------------------------------------------

fn pattern_name(pattern: Pattern) -> &'static str {
    match pattern {
        Pattern::ReverseDns => "a reverse-DNS id such as com.example.my-hud",
        Pattern::Semver => "a MAJOR.MINOR.PATCH version such as 1.0.0",
        Pattern::FormatVersion => "MAJOR.MINOR such as 1.0",
        Pattern::SchemaUrl => "an https:// URL",
        Pattern::Id => {
            "a lower-case identifier (a-z, 0-9, _ and -, starting with a letter, at most 40 characters)"
        }
    }
}

fn matches_pattern(pattern: Pattern, text: &str) -> bool {
    match pattern {
        Pattern::ReverseDns => is_reverse_dns(text),
        Pattern::Semver => is_semver(text),
        Pattern::FormatVersion => parse_format(text).is_some(),
        Pattern::SchemaUrl => is_schema_url(text),
        Pattern::Id => is_id(text),
    }
}

/// A plain decimal number: digits only, no sign, no leading zero (but `0` itself). A number too large
/// for `u64` saturates, which no supported version reaches.
fn decimal(text: &str) -> Option<u64> {
    let plain = !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text.len() == 1 || !text.starts_with('0'));
    plain.then(|| {
        text.bytes().fold(0u64, |n, b| {
            n.saturating_mul(10).saturating_add(u64::from(b - b'0'))
        })
    })
}

/// `MAJOR.MINOR`, two plain decimal numbers.
fn parse_format(text: &str) -> Option<(u64, u64)> {
    let (major, minor) = text.split_once('.')?;
    Some((decimal(major)?, decimal(minor)?))
}

fn is_semver(text: &str) -> bool {
    text.len() <= PATTERN_LEN && {
        let parts: Vec<&str> = text.split('.').collect();
        parts.len() == 3 && parts.iter().all(|p| decimal(p).is_some())
    }
}

fn is_reverse_dns(text: &str) -> bool {
    if text == schema::BUILTIN_CLASSIC_ID {
        return true;
    }
    let alphabet = text
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-');
    let labels: Vec<&str> = text.split('.').collect();
    text.len() <= PATTERN_LEN
        && alphabet
        && labels.len() >= 2
        && labels
            .iter()
            .all(|l| matches!(l.bytes().next(), Some(b) if b.is_ascii_lowercase()))
}

fn is_schema_url(text: &str) -> bool {
    text.len() <= PATTERN_LEN
        && text
            .strip_prefix("https://")
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_graphic()))
}

/// `[a-z][a-z0-9_-]*`, at most [`ID_LEN`]: an author-chosen id.
fn is_id(text: &str) -> bool {
    text.len() <= ID_LEN
        && matches!(text.bytes().next(), Some(b) if b.is_ascii_lowercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// `[a-z][a-z0-9_]*`, at most [`ID_LEN`]: a palette name, and the shape of an ink that refers to one.
fn is_palette_name(text: &str) -> bool {
    text.len() <= ID_LEN
        && matches!(text.bytes().next(), Some(b) if b.is_ascii_lowercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// `[A-Z][A-Z0-9_]*`: the shape of a theme token.
fn is_token_shaped(text: &str) -> bool {
    matches!(text.bytes().next(), Some(b) if b.is_ascii_uppercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

/// `#` and six LOWER-case hex digits, as the three bytes. Anything else (`#fff`, `#f2b535ff`, `f2b535`,
/// `#F2B535`, a name) is `None`.
fn parse_hex_colour(text: &str) -> Option<[u8; 3]> {
    let digits = text.strip_prefix('#')?.as_bytes();
    if digits.len() != 6 {
        return None;
    }
    let mut out = [0u8; 3];
    for (slot, pair) in out.iter_mut().zip(digits.chunks(2)) {
        *slot = hex_digit(*pair.first()?)? * 16 + hex_digit(*pair.get(1)?)?;
    }
    Some(out)
}

fn hex_digit(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod freeze_tests {
    use super::*;

    #[test]
    fn the_typed_tree_reads_back_what_it_holds() {
        let inner = Object {
            node: "text",
            pointer: "/a/0/text".into(),
            fields: vec![("bold".into(), Val::Bool(true))],
        };
        let outer = Object {
            node: "cell",
            pointer: "/a/0".into(),
            fields: vec![
                ("size".into(), Val::Name("CAPTION".into())),
                ("ink".into(), Val::Token("TEXT_PRIMARY".into())),
                ("gap".into(), Val::Int(24)),
                ("alpha".into(), Val::Num(0.5)),
                ("text".into(), Val::Object(inner)),
            ],
        };
        assert_eq!(outer.get("size").and_then(Val::as_str), Some("CAPTION"));
        assert_eq!(outer.get("ink").and_then(Val::as_str), Some("TEXT_PRIMARY"));
        assert_eq!(outer.get("gap").and_then(Val::as_i64), Some(24));
        assert_eq!(outer.get("gap").and_then(Val::as_f64), Some(24.0));
        assert_eq!(outer.get("alpha").and_then(Val::as_f64), Some(0.5));
        assert_eq!(outer.get("alpha").and_then(Val::as_i64), None);
        let text = outer.get("text").and_then(Val::as_object).unwrap();
        assert_eq!(text.get("bold").and_then(Val::as_bool), Some(true));
        assert!(
            outer.get("missing").is_none()
                && Val::List(vec![]).as_list().map(<[Val]>::len) == Some(0)
        );
    }

    #[test]
    fn the_context_starts_empty_at_format_1_0() {
        let ctx = Ctx::v1();
        assert_eq!(ctx.format, (1, 0));
        assert!(ctx.palette.is_empty() && ctx.assets.is_empty() && !ctx.in_strip_card);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
    const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");

    /// The only control of `MINIMAL`, and its playbar.
    const ITEM_AT: &str = "/layout/block/1/cells/0/segment/items/0";
    const PLAYBAR_AT: &str = "/layout/block/0/cells/0/playbar";
    /// A one-path mask the SVG subset accepts.
    const SVG: &str = "<svg viewBox='0 0 24 24'><path d='M0 0h4v4z'/></svg>";

    /// Parse, and check the one invariant every parse must keep: a package iff no error finding.
    fn parse_text(text: &str) -> Parsed {
        let parsed = parse(text.as_bytes());
        assert_eq!(
            parsed.package.is_some(),
            !has_error(&parsed.findings),
            "a package exists exactly when no finding is an error: {:#?}",
            parsed.findings
        );
        parsed
    }

    /// `base` with each `(from, to)` applied once; a `from` that is absent or repeated is a broken test.
    fn edit(base: &str, edits: &[(&str, &str)]) -> String {
        let mut text = base.to_owned();
        for (from, to) in edits {
            assert_eq!(text.matches(from).count(), 1, "`{from}` must occur once");
            text = text.replace(from, to);
        }
        text
    }

    /// `MINIMAL` with `extra` (a `"key": value` fragment) written before `layout`.
    fn with_header(extra: &str) -> String {
        let license = r#""license": "CC0-1.0","#;
        edit(
            MINIMAL,
            &[(license, &format!(r#""license": "CC0-1.0", {extra},"#))],
        )
    }

    /// `MINIMAL` with the control's `"command": "open_more"` replaced by `command_and_keys`.
    fn with_command(command_and_keys: &str) -> String {
        edit(MINIMAL, &[(r#""command": "open_more""#, command_and_keys)])
    }

    fn found(findings: &[Finding]) -> Vec<(&'static str, &str)> {
        findings
            .iter()
            .map(|f| (f.code, f.pointer.as_str()))
            .collect()
    }

    /// The parse has exactly one finding, of `code` at `pointer`; returns it.
    fn only<'a>(parsed: &'a Parsed, code: &Code, pointer: &str) -> &'a Finding {
        assert_eq!(
            found(&parsed.findings),
            vec![(code.id, pointer)],
            "{:#?}",
            parsed.findings
        );
        &parsed.findings[0]
    }

    /// The text that parses to exactly one finding of `code` at `pointer`.
    fn refuses(text: &str, code: &Code, pointer: &str) -> Finding {
        only(&parse_text(text), code, pointer).clone()
    }

    fn controls(layout: &Object) -> Vec<&Object> {
        let mut out = Vec::new();
        let bands = layout
            .get("block")
            .and_then(Val::as_list)
            .unwrap_or_default();
        for band in bands.iter().filter_map(Val::as_object) {
            let cells = band.get("cells").and_then(Val::as_list).unwrap_or_default();
            for cell in cells.iter().filter_map(Val::as_object) {
                let items = cell
                    .get("segment")
                    .and_then(Val::as_object)
                    .and_then(|segment| segment.get("items"))
                    .and_then(Val::as_list)
                    .unwrap_or_default();
                out.extend(items.iter().filter_map(Val::as_object));
            }
        }
        out
    }

    fn control<'a>(layout: &'a Object, id: &str) -> &'a Object {
        controls(layout)
            .into_iter()
            .find(|c| c.get("id").and_then(Val::as_str) == Some(id))
            .unwrap_or_else(|| panic!("no control `{id}`"))
    }

    fn cond(names: &[&str]) -> Cond {
        Cond {
            terms: names
                .iter()
                .map(|n| CondTerm {
                    negated: n.starts_with('!'),
                    name: n.trim_start_matches('!').to_owned(),
                })
                .collect(),
        }
    }

    fn name(text: &str) -> Val {
        Val::Name(text.to_owned())
    }

    fn token(text: &str) -> Val {
        Val::Token(text.to_owned())
    }

    // ----- (1) and (2): the two fixtures -----

    #[test]
    fn the_reference_package_parses_without_a_finding_and_is_typed() {
        let parsed = parse_text(REFERENCE);
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        let package = parsed.package.expect("the reference parses");
        assert_eq!(package.format, (1, 0));
        assert_eq!(package.id, "com.plxnative.examples.reference");
        assert_eq!(package.version, "1.0.0");
        assert_eq!(package.name, "Reference");
        assert_eq!(package.author, "PlxNative");
        assert_eq!(package.license, "CC0-1.0");
        assert_eq!(package.app, None);
        assert_eq!(
            package.palette,
            vec![("gold".to_owned(), [0xf2, 0xb5, 0x35])]
        );
        let ids: Vec<&str> = package.assets.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            ["subs-bubble", "list", "gear", "eq", "back10", "fwd30"]
        );
        assert!(package
            .assets
            .iter()
            .all(|(_, svg)| svg.starts_with("<svg")));

        let layout = &package.layout;
        assert_eq!(
            (layout.node, layout.pointer.as_str()),
            ("layout", "/layout")
        );
        let bands = layout.get("block").and_then(Val::as_list).expect("block");
        assert_eq!(bands.len(), 7);
        let band_ids: Vec<&str> = bands
            .iter()
            .filter_map(Val::as_object)
            .filter_map(|b| b.get("id").and_then(Val::as_str))
            .collect();
        assert_eq!(
            band_ids,
            [
                "title",
                "bar",
                "row",
                "chapters_head",
                "chapters",
                "info_rule",
                "info"
            ]
        );

        // The seek controls carry their numbers; the others carry their command.
        let seeks: Vec<i64> = controls(layout)
            .into_iter()
            .filter(|c| c.get("command").and_then(Val::as_str) == Some("seek"))
            .filter_map(|c| c.get("by").and_then(Val::as_i64))
            .collect();
        assert_eq!(seeks, [-10, 30]);
        assert_eq!(controls(layout).len(), 9);
        let back = control(layout, "back");
        assert_eq!(
            (back.node, back.pointer.as_str()),
            ("item", "/layout/block/2/cells/0/segment/items/1")
        );
        assert_eq!(back.get("face"), Some(&Val::Ref("back10".to_owned())));

        // Faces: a per-state map is an object of single faces.
        let previous = control(layout, "prev");
        let face = previous
            .get("face")
            .and_then(Val::as_object)
            .expect("a map");
        assert_eq!(face.node, "face_map");
        assert_eq!(face.get("restart"), Some(&name("builtin:restart")));
        assert_eq!(
            control(layout, "next").get("face"),
            Some(&name("builtin:next"))
        );

        // An ink is a token or a reference to the palette.
        let bar = bands[1]
            .as_object()
            .and_then(|b| b.get("cells"))
            .and_then(Val::as_list);
        let playbar = bar
            .and_then(|cells| cells.first())
            .and_then(Val::as_object)
            .and_then(|cell| cell.get("playbar"))
            .and_then(Val::as_object)
            .expect("playbar");
        assert_eq!(playbar.get("fill"), Some(&Val::Ref("gold".to_owned())));
        assert_eq!(playbar.get("track"), Some(&token("RAIL_TRACK")));
        assert_eq!(playbar.get("thickness"), Some(&Val::Int(6)));

        // The keyed gap_before of the chapters head keeps its panel.info case.
        let head = bands[3].as_object().expect("chapters_head");
        assert_eq!(
            head.get("gap_before"),
            Some(&Val::Keyed(Keyed {
                base: Box::new(name("SM")),
                cases: vec![(cond(&["panel.info"]), name("XS"))],
            }))
        );
        assert_eq!(
            head.get("when"),
            Some(&Val::Cond(cond(&["panel.chapters"])))
        );

        // The strip card: a keyed number, and the keyed ink of its second `below` text.
        let strip = bands[4]
            .as_object()
            .and_then(|b| b.get("cells"))
            .and_then(Val::as_list)
            .and_then(|cells| cells.first())
            .and_then(Val::as_object)
            .and_then(|cell| cell.get("strip"))
            .and_then(Val::as_object)
            .expect("strip");
        assert_eq!(
            strip.get("card_h"),
            Some(&Val::Keyed(Keyed {
                base: Box::new(Val::Int(108)),
                cases: vec![(cond(&["panel.info"]), Val::Int(84))],
            }))
        );
        let card = strip.get("card").and_then(Val::as_object).expect("card");
        assert_eq!(card.node, "card");
        let below = card.get("below").and_then(Val::as_list).expect("below");
        assert_eq!(below.len(), 2);
        let second = below[1]
            .as_object()
            .and_then(|cell| cell.get("text"))
            .and_then(Val::as_object)
            .expect("the second text");
        assert_eq!(second.get("bind"), Some(&name("item.title")));
        assert_eq!(
            second.get("ink"),
            Some(&Val::Keyed(Keyed {
                base: Box::new(token("TEXT_SECONDARY")),
                cases: vec![(cond(&["item.focused"]), token("TEXT_PRIMARY"))],
            }))
        );
        let overlay = card.get("overlay").and_then(Val::as_list).expect("overlay");
        let current = overlay[0].as_object().expect("overlay cell");
        assert_eq!(
            current.get("when"),
            Some(&Val::Cond(cond(&["item.is_current"])))
        );

        // Up Next.
        let up_next = layout
            .get("up_next")
            .and_then(Val::as_object)
            .expect("up_next");
        assert_eq!(
            up_next.get("replaces"),
            Some(&Val::List(vec![name_text("center"), name_text("right")]))
        );
    }

    fn name_text(text: &str) -> Val {
        Val::Text(text.to_owned())
    }

    #[test]
    fn the_minimal_package_parses_without_a_finding() {
        let parsed = parse_text(MINIMAL);
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        let package = parsed.package.expect("the minimal package parses");
        assert_eq!(package.id, "com.plxnative.examples.minimal");
        assert!(package.palette.is_empty() && package.assets.is_empty() && package.app.is_none());
        let items = controls(&package.layout);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].get("command"), Some(&name("open_more")));
        assert_eq!(items[0].pointer, ITEM_AT);
    }

    #[test]
    fn one_walker_serves_both_doors() {
        let package = parse_text(REFERENCE).package.expect("reference");
        let (object, findings) = validate_json(&schema::ROOT, REFERENCE, "", &Ctx::v1());
        assert!(findings.is_empty(), "{findings:#?}");
        let object = object.expect("typed");
        assert_eq!(object.node, "package");
        assert_eq!(object.get("layout"), Some(&Val::Object(package.layout)));

        // A fragment is judged by the same rules, in the scope its context gives.
        let cell = r#"{ "text": { "bind": "item.start", "size": "MICRO", "ink": "TEXT_SECONDARY", "bold": false } }"#;
        let (object, findings) = validate_json(&schema::CELL, cell, "/x", &Ctx::v1());
        assert!(object.is_none());
        assert_eq!(found(&findings), [(codes::E0201.id, "/x/text/bind")]);
        let card = Ctx {
            in_strip_card: true,
            ..Ctx::v1()
        };
        let (object, findings) = validate_json(&schema::CELL, cell, "/x", &card);
        assert!(findings.is_empty(), "{findings:#?}");
        assert_eq!(object.expect("typed").node, "cell");
        // The context's palette and assets are what a fragment's references resolve against.
        let icon = r#"{ "icon": { "asset": "eq", "size": 28, "ink": "gold" } }"#;
        let (_, findings) = validate_json(&schema::CELL, icon, "/y", &Ctx::v1());
        assert_eq!(
            found(&findings),
            [
                (codes::E0203.id, "/y/icon/asset"),
                (codes::E0204.id, "/y/icon/ink")
            ]
        );
        let ctx = Ctx {
            palette: vec!["gold".to_owned()],
            assets: vec!["eq".to_owned()],
            ..Ctx::v1()
        };
        let (object, findings) = validate_json(&schema::CELL, icon, "/y", &ctx);
        assert!(findings.is_empty() && object.is_some(), "{findings:#?}");

        // Not JSON, and not an object: at the fragment's own pointer.
        let (object, findings) = validate_json(&schema::CELL, "{", "/x", &Ctx::v1());
        assert!(object.is_none());
        assert_eq!(found(&findings), [(codes::E0100.id, "/x")]);
        let (_, findings) = validate_json(&schema::CELL, "[]", "/x", &Ctx::v1());
        assert_eq!(found(&findings), [(codes::E0104.id, "/x")]);
    }

    // ----- (3) a smoke test per code family -----

    #[test]
    fn json_that_is_not_json_or_too_large_is_e0100_at_the_root() {
        let inputs: [&[u8]; 6] = [
            b"",
            b"{",
            b"{\"format\": }",
            b"\xff\xfe",
            b"{} {}",
            b"{\"a\":1,}",
        ];
        for bytes in inputs {
            let parsed = parse(bytes);
            only(&parsed, &codes::E0100, "");
        }
        let oversize = vec![b' '; MAX_PACKAGE_BYTES + 1];
        let parsed = parse(&oversize);
        let finding = only(&parsed, &codes::E0100, "");
        assert!(finding.message.contains(&MAX_PACKAGE_BYTES.to_string()));
        // Within the byte cap but past the decoded-memory bound the HUD gives the reader: 100 000
        // array nodes in 200 KB. The refusal is worded for a package, not for the recorder.
        let mut many = String::from("{\"layout\": [0");
        many.push_str(&",0".repeat(100_000));
        many.push_str("]}");
        assert!(many.len() < MAX_PACKAGE_BYTES);
        let parsed = parse(many.as_bytes());
        let finding = only(&parsed, &codes::E0100, "");
        assert!(finding.message.contains("too large"), "{}", finding.message);
        assert!(
            !finding.message.contains("recording"),
            "{}",
            finding.message
        );
        // A duplicate-free but non-object root is not E0100: it is JSON.
        only(&parse(b"[]"), &codes::E0104, "");
        only(&parse(b"null"), &codes::E0104, "");
    }

    #[test]
    fn one_hostile_name_or_a_crowd_of_assets_cannot_make_a_finding_huge_or_slow() {
        // A palette name is bounded like an id: 41 letters is not a name, declared or referenced.
        let long = "a".repeat(ID_LEN + 1);
        let declared = with_header(&format!(r##""palette": {{ "{long}": "#f2b535" }}"##));
        refuses(&declared, &codes::E0505, &format!("/palette/{long}"));
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""fill": "ACCENT""#, &format!(r#""fill": "{long}""#))],
            ),
            &codes::E0105,
            &format!("{PLAYBAR_AT}/fill"),
        );
        assert!(finding.nearest.is_empty());
        // A 500 000-letter name is one finding with a clipped message, not a 500 000-step compare per ink.
        let huge = "a".repeat(500_000);
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""fill": "ACCENT""#, &format!(r#""fill": "{huge}""#))],
            ),
            &codes::E0105,
            &format!("{PLAYBAR_AT}/fill"),
        );
        assert!(finding.message.len() < 200, "{}", finding.message.len());
        // `nearest` compares only the start of a candidate, so a hostile candidate costs what any does.
        let long_candidate = format!("{}{}", "a".repeat(48), "b".repeat(500_000));
        let near = nearest(&"a".repeat(48), [long_candidate.as_str(), &"b".repeat(48)]);
        assert_eq!(near.first(), Some(&long_candidate));
        // An unknown face lists a dozen declared assets and counts the rest, however many are declared.
        let assets: Vec<String> = (0..40).map(|n| format!(r#""a{n:02}": "{SVG}""#)).collect();
        let text = with_header(&format!(r#""assets": {{ {} }}"#, assets.join(", ")));
        let finding = refuses(
            &edit(
                &text,
                &[(r#""size": 52 }"#, r#""size": 52, "face": "nope" }"#)],
            ),
            &codes::E0203,
            &format!("{ITEM_AT}/face"),
        );
        assert!(finding.hint.contains("a00"), "{}", finding.hint);
        assert!(!finding.hint.contains("a39"), "{}", finding.hint);
        assert!(finding.hint.contains("and 28 more"), "{}", finding.hint);
    }

    #[test]
    fn the_package_reader_names_only_keys_the_table_has() {
        for key in [
            "id", "version", "name", "author", "license", "app", "palette", "assets", "layout",
        ] {
            assert!(
                schema::ROOT.fields.iter().any(|f| f.key == key),
                "`Package::from_object` reads `{key}`, which `schema::ROOT` does not list"
            );
        }
        let app = schema::node("app").expect("the `app` node");
        for key in ["min", "max"] {
            assert!(
                app.fields.iter().any(|f| f.key == key),
                "`Package::from_object` reads `app.{key}`, which the `app` node does not list"
            );
        }
    }

    #[test]
    fn the_version_gate_answers_alone_at_slash_format() {
        let other_mistake = (r#""name": "Minimal""#, r#""name": null"#);
        let with_format =
            |format: &str| edit(MINIMAL, &[(r#""format": "1.0""#, format), other_mistake]);
        for malformed in [
            r#""format": "x""#,
            r#""format": "1""#,
            r#""format": "1.0.0""#,
            r#""format": "01.0""#,
            r#""format": "1.00""#,
            r#""format": " 1.0""#,
            r#""format": "-1.0""#,
            r#""format": "1.-0""#,
            r#""format": """#,
            r#""format": 1.0"#,
            r#""format": null"#,
            r#""format": ["1.0"]"#,
        ] {
            refuses(&with_format(malformed), &codes::E0401, "/format");
        }
        let missing = edit(MINIMAL, &[(r#""format": "1.0","#, ""), other_mistake]);
        refuses(&missing, &codes::E0401, "/format");

        for newer_minor in ["1.1", "1.7", "1.255", "1.99999999999999999999999"] {
            let text = with_format(&format!(r#""format": "{newer_minor}""#));
            let finding = refuses(&text, &codes::E0402, "/format");
            assert_eq!(finding.message, REASON_NEWER, "{newer_minor}");
        }
        for newer_major in ["2.0", "9.9", "256.0", "99999999999999999999999.0"] {
            let text = with_format(&format!(r#""format": "{newer_major}""#));
            let finding = refuses(&text, &codes::E0403, "/format");
            assert_eq!(finding.message, REASON_NEWER, "{newer_major}");
        }
        let finding = refuses(&with_format(r#""format": "0.9""#), &codes::E0403, "/format");
        assert_eq!(finding.message, REASON_OLDER);
        let finding = refuses(&with_format(r#""format": "0.0""#), &codes::E0403, "/format");
        assert_eq!(finding.message, REASON_OLDER);
        // Past the gate, the same other mistake is reported.
        let parsed = parse_text(&with_format(r#""format": "1.0""#));
        only(&parsed, &codes::E0104, "/name");
    }

    #[test]
    fn walker_findings_e0101_to_e0106_point_where_the_convention_says() {
        // E0101: the repeated key's own pointer, once per extra occurrence.
        let twice = edit(
            MINIMAL,
            &[(
                r#""name": "Minimal","#,
                r#""name": "Minimal", "name": "Two", "name": "Three","#,
            )],
        );
        let parsed = parse_text(&twice);
        assert_eq!(
            found(&parsed.findings),
            [(codes::E0101.id, "/name"), (codes::E0101.id, "/name")]
        );
        refuses(
            &edit(
                MINIMAL,
                &[(r#""thickness": 6"#, r#""thickness": 6, "thickness": 7"#)],
            ),
            &codes::E0101,
            &format!("{PLAYBAR_AT}/thickness"),
        );

        // E0102: the key's own pointer, with the nearest keys.
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(
                    r#""author": "PlxNative","#,
                    r#""author": "PlxNative", "authr": "x","#,
                )],
            ),
            &codes::E0102,
            "/authr",
        );
        assert_eq!(finding.nearest.first().map(String::as_str), Some("author"));
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""id": "bar""#, r#""id": "bar", "gapp_before": "SM""#)],
            ),
            &codes::E0102,
            "/layout/block/0/gapp_before",
        );
        assert_eq!(
            finding.nearest.first().map(String::as_str),
            Some("gap_before")
        );
        // An unknown key of a control is the command's E0302, not the generic one.
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""id": "more""#, r#""id": "more", "wigdet": "pill""#)],
            ),
            &codes::E0302,
            &format!("{ITEM_AT}/wigdet"),
        );
        assert_eq!(finding.nearest.first().map(String::as_str), Some("widget"));
        // Pointers escape per RFC 6901.
        let parsed = parse_text(r#"{ "format": "1.0", "a/b~c": 1 }"#);
        assert!(found(&parsed.findings).contains(&(codes::E0102.id, "/a~1b~0c")));

        // E0103: at the object that lacks the key.
        refuses(
            &edit(MINIMAL, &[(r#""license": "CC0-1.0","#, "")]),
            &codes::E0103,
            "",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(r#"{ "id": "more", "command""#, r#"{ "command""#)],
            ),
            &codes::E0103,
            ITEM_AT,
        );

        // E0104: null anywhere, a wrong kind, a non-integer for an Int.
        refuses(
            &edit(MINIMAL, &[(r#""name": "Minimal""#, r#""name": null"#)]),
            &codes::E0104,
            "/name",
        );
        refuses(
            &edit(MINIMAL, &[(r#""name": "Minimal""#, r#""name": 7"#)]),
            &codes::E0104,
            "/name",
        );
        for bad in ["6.5", "6.0", r#""6""#, "null", "true", "[6]"] {
            refuses(
                &edit(
                    MINIMAL,
                    &[(r#""thickness": 6"#, &format!(r#""thickness": {bad}"#))],
                ),
                &codes::E0104,
                &format!("{PLAYBAR_AT}/thickness"),
            );
        }
        refuses(
            &edit(MINIMAL, &[(r#""sections": "modal""#, r#""sections": 5"#)]),
            &codes::E0104,
            "/layout/sections",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(r#""sections": "modal""#, r#""sections": ["modal"]"#)],
            ),
            &codes::E0104,
            "/layout/sections",
        );
    }

    #[test]
    fn a_repeat_is_e0101_even_for_an_unknown_key_and_entries_past_a_bound_are_one_finding() {
        let parsed = parse_text(&edit(
            MINIMAL,
            &[(
                r#""name": "Minimal","#,
                r#""name": "Minimal", "zz": 1, "zz": 2,"#,
            )],
        ));
        assert_eq!(
            found(&parsed.findings),
            [(codes::E0102.id, "/zz"), (codes::E0101.id, "/zz")]
        );
        // Sixteen valid ids and then five that are not ids: the list is over its bound, once.
        let ids = ["right"; 16].map(|id| format!(r#""{id}""#)).join(", ");
        let list = format!(r#""replaces": [ {ids}, "A", "B", "C", "D", "E" ]"#);
        let text = edit(MINIMAL, &[(r#""replaces": [ "right" ]"#, &list)]);
        refuses(&text, &codes::E0105, "/layout/up_next/replaces");
    }

    #[test]
    fn ranges_lengths_and_shapes_are_e0105() {
        refuses(
            &edit(MINIMAL, &[(r#""version": "1.0.0""#, r#""version": "1.0""#)]),
            &codes::E0105,
            "/version",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(r#""version": "1.0.0""#, r#""version": "1.01.0""#)],
            ),
            &codes::E0105,
            "/version",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(
                    r#""id": "com.plxnative.examples.minimal""#,
                    r#""id": "Com.Plx""#,
                )],
            ),
            &codes::E0105,
            "/id",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(
                    r#""id": "com.plxnative.examples.minimal""#,
                    r#""id": "single""#,
                )],
            ),
            &codes::E0105,
            "/id",
        );
        let long = "x".repeat(65);
        refuses(
            &edit(
                MINIMAL,
                &[(r#""name": "Minimal""#, &format!(r#""name": "{long}""#))],
            ),
            &codes::E0105,
            "/name",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(r#""name": "Minimal""#, r#""name": "a\u0007b""#)],
            ),
            &codes::E0105,
            "/name",
        );
        for out_of_range in ["0", "99", "-3", "18446744073709551615"] {
            refuses(
                &edit(
                    MINIMAL,
                    &[(
                        r#""thickness": 6"#,
                        &format!(r#""thickness": {out_of_range}"#),
                    )],
                ),
                &codes::E0105,
                &format!("{PLAYBAR_AT}/thickness"),
            );
        }
        // A band id is an Id: lower case, starting with a letter.
        refuses(
            &edit(MINIMAL, &[(r#""id": "bar""#, r#""id": "Bar""#)]),
            &codes::E0105,
            "/layout/block/0/id",
        );
        // A list over its length: the finding is at the list.
        let many = vec![r#""right""#; 17].join(", ");
        refuses(
            &edit(
                MINIMAL,
                &[(
                    r#""replaces": [ "right" ]"#,
                    &format!(r#""replaces": [ {many} ]"#),
                )],
            ),
            &codes::E0105,
            "/layout/up_next/replaces",
        );
        // A px gap is a bounded integer.
        refuses(
            &edit(
                MINIMAL,
                &[(r#""gap_before": "SM""#, r#""gap_before": 5000"#)],
            ),
            &codes::E0105,
            "/layout/block/1/gap_before",
        );
        let parsed = parse_text(&edit(
            MINIMAL,
            &[(r#""gap_before": "SM""#, r#""gap_before": 24"#)],
        ));
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
    }

    #[test]
    fn combinations_are_e0106_and_a_conditional_requirement_is_e0103() {
        // The rule of an object is reported at the object.
        let solid_accent = edit(
            MINIMAL,
            &[(
                r#""size": 52 }"#,
                r#""size": 52, "rest": "solid", "focus": "accent" }"#,
            )],
        );
        refuses(&solid_accent, &codes::E0106, ITEM_AT);
        let solid = edit(
            MINIMAL,
            &[(r#""size": 52 }"#, r#""size": 52, "rest": "solid" }"#)],
        );
        refuses(&solid, &codes::E0103, ITEM_AT);
        let solid_ring = edit(
            MINIMAL,
            &[(
                r#""size": 52 }"#,
                r#""size": 52, "rest": "solid", "focus": "ring" }"#,
            )],
        );
        assert!(parse_text(&solid_ring).findings.is_empty());

        // A cell holds exactly one content key.
        let two = edit(
            MINIMAL,
            &[(
                r#"{ "align": "start", "playbar":"#,
                r#"{ "align": "start", "spacer": { "w": 10 }, "playbar":"#,
            )],
        );
        refuses(&two, &codes::E0106, "/layout/block/0/cells/0");
        let none = edit(
            MINIMAL,
            &[(
                r#", "playbar": { "w": "fill", "fill": "ACCENT", "track": "RAIL_TRACK", "thickness": 6, "clock_size": "CAPTION" }"#,
                "",
            )],
        );
        refuses(&none, &codes::E0106, "/layout/block/0/cells/0");
    }

    /// A node and a field that arrived in format 1.1, built by hand: the table has no such entry yet.
    static PROBE: schema::Node = schema::Node {
        name: "probe",
        kind: NodeKind::Plain,
        fields: &[
            Field::req("old", Ty::Bool, "since 1.0"),
            Field::req("fresh", Ty::Bool, "since 1.1").at((1, 1)),
        ],
        rules: &[],
        doc: "a hand-built node",
    };

    #[test]
    fn a_key_newer_than_the_declared_format_is_e0107_and_not_required_of_it() {
        let both = r#"{ "old": true, "fresh": true }"#;
        let (object, findings) = validate_json(&PROBE, both, "/probe", &Ctx::v1());
        assert!(object.is_none());
        assert_eq!(found(&findings), [(codes::E0107.id, "/probe/fresh")]);

        let at_1_1 = Ctx {
            format: (1, 1),
            ..Ctx::v1()
        };
        let (object, findings) = validate_json(&PROBE, both, "/probe", &at_1_1);
        assert!(findings.is_empty(), "{findings:#?}");
        let object = object.expect("typed");
        assert_eq!((object.node, object.pointer.as_str()), ("probe", "/probe"));
        assert_eq!(object.get("fresh").and_then(Val::as_bool), Some(true));

        // A 1.0 package without the 1.1 key is complete; a 1.1 package without it is not.
        let old_only = r#"{ "old": true }"#;
        let (object, findings) = validate_json(&PROBE, old_only, "/probe", &Ctx::v1());
        assert!(findings.is_empty() && object.is_some(), "{findings:#?}");
        let (object, findings) = validate_json(&PROBE, old_only, "/probe", &at_1_1);
        assert!(object.is_none());
        assert_eq!(found(&findings), [(codes::E0103.id, "/probe")]);
    }

    #[test]
    fn closed_names_tokens_assets_and_palette_references_are_e0201_to_e0204() {
        // E0201: a closed name, with the nearest.
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""band": "controls""#, r#""band": "contrls""#)],
            ),
            &codes::E0201,
            "/layout/band",
        );
        assert_eq!(
            finding.nearest.first().map(String::as_str),
            Some("controls")
        );
        refuses(
            &edit(MINIMAL, &[(r#""inset": "MD""#, r#""inset": "MEDIUM""#)]),
            &codes::E0201,
            "/layout/inset",
        );
        refuses(
            &edit(
                MINIMAL,
                &[(r#""clock_size": "CAPTION""#, r#""clock_size": "TINY""#)],
            ),
            &codes::E0201,
            &format!("{PLAYBAR_AT}/clock_size"),
        );

        // E0202: UPPER_CASE and not a token.
        let finding = refuses(
            &edit(MINIMAL, &[(r#""fill": "ACCENT""#, r#""fill": "ACCNT""#)]),
            &codes::E0202,
            &format!("{PLAYBAR_AT}/fill"),
        );
        assert_eq!(finding.nearest.first().map(String::as_str), Some("ACCENT"));

        // E0203: an asset or a built-in face that does not exist.
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""size": 52 }"#, r#""size": 52, "face": "nope" }"#)],
            ),
            &codes::E0203,
            &format!("{ITEM_AT}/face"),
        );
        assert!(!finding.hint.is_empty());
        let finding = refuses(
            &edit(
                MINIMAL,
                &[(r#""size": 52 }"#, r#""size": 52, "face": "builtin:gea" }"#)],
            ),
            &codes::E0203,
            &format!("{ITEM_AT}/face"),
        );
        assert!(
            finding.nearest.iter().all(|n| n.starts_with("builtin:")),
            "{:?}",
            finding.nearest
        );

        // E0204: a lower-case name the palette does not declare; the hint knows a token's lower-case spelling.
        let finding = refuses(
            &edit(MINIMAL, &[(r#""fill": "ACCENT""#, r#""fill": "accent""#)]),
            &codes::E0204,
            &format!("{PLAYBAR_AT}/fill"),
        );
        assert!(finding.hint.contains("ACCENT"), "{}", finding.hint);
        let declared = with_header(r##""palette": { "gold": "#f2b535" }"##);
        let finding = refuses(
            &edit(&declared, &[(r#""fill": "ACCENT""#, r#""fill": "golt""#)]),
            &codes::E0204,
            &format!("{PLAYBAR_AT}/fill"),
        );
        assert_eq!(finding.nearest.first().map(String::as_str), Some("gold"));
        // ... and resolves when it is declared. A mixed-case ink is neither a token nor a name.
        let parsed = parse_text(&edit(
            &declared,
            &[(r#""fill": "ACCENT""#, r#""fill": "gold""#)],
        ));
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        assert_eq!(
            parsed.package.expect("package").palette,
            vec![("gold".to_owned(), [0xf2, 0xb5, 0x35])]
        );
        refuses(
            &edit(MINIMAL, &[(r#""fill": "ACCENT""#, r#""fill": "Gold""#)]),
            &codes::E0105,
            &format!("{PLAYBAR_AT}/fill"),
        );
    }

    #[test]
    fn commands_are_judged_by_their_own_table_e0301_to_e0305() {
        // E0301: an unknown command judges nothing else of the item.
        let text = edit(
            MINIMAL,
            &[(
                r#""command": "open_more""#,
                r#""command": "open_mre", "bogus": 1"#,
            )],
        );
        let finding = refuses(&text, &codes::E0301, &format!("{ITEM_AT}/command"));
        assert_eq!(
            finding.nearest.first().map(String::as_str),
            Some("open_more")
        );
        refuses(
            &with_command(r#""command": 7"#),
            &codes::E0104,
            &format!("{ITEM_AT}/command"),
        );
        // ... but it still judges the item's OWN keys, so one round reports every mistake: here a bad
        // command, a repeated `size`, a bad `widget` and a missing `id`. A key only the command's
        // parameters could own (`bogus`) is not reported against a command nobody could resolve.
        let text = edit(
            MINIMAL,
            &[
                (
                    r#""id": "more", "command": "open_more""#,
                    r#""command": "open_mre", "bogus": 1"#,
                ),
                (
                    r#""widget": "disc", "size": 52"#,
                    r#""widget": "nope", "size": 52, "size": 9"#,
                ),
            ],
        );
        let parsed = parse_text(&text);
        assert_eq!(
            found(&parsed.findings),
            [
                (codes::E0301.id, &*format!("{ITEM_AT}/command")),
                (codes::E0201.id, &*format!("{ITEM_AT}/widget")),
                (codes::E0101.id, &*format!("{ITEM_AT}/size")),
                (codes::E0103.id, ITEM_AT),
            ]
        );

        // E0302: a key the command does not have, with the item's keys and its parameters as the nearest.
        let finding = refuses(
            &with_command(r#""command": "open_more", "pagee": "root""#),
            &codes::E0302,
            &format!("{ITEM_AT}/pagee"),
        );
        assert_eq!(finding.nearest.first().map(String::as_str), Some("page"));
        refuses(
            &with_command(r#""command": "next", "tab": "audio""#),
            &codes::E0302,
            &format!("{ITEM_AT}/tab"),
        );

        // E0303: a parameter the command requires, at the item.
        refuses(
            &with_command(r#""command": "open_tracks""#),
            &codes::E0303,
            ITEM_AT,
        );
        let parsed = parse_text(&with_command(r#""command": "open_tracks", "tab": "audio""#));
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);

        // E0304: a parameter of the wrong kind or value, and a NonZero rule, at the value.
        refuses(
            &with_command(r#""command": "open_more", "page": "nope""#),
            &codes::E0304,
            &format!("{ITEM_AT}/page"),
        );
        refuses(
            &with_command(r#""command": "open_more", "page": 3"#),
            &codes::E0304,
            &format!("{ITEM_AT}/page"),
        );
        refuses(
            &with_command(r#""command": "seek", "by": 0"#),
            &codes::E0304,
            &format!("{ITEM_AT}/by"),
        );
        refuses(
            &with_command(r#""command": "seek", "by": "later""#),
            &codes::E0304,
            &format!("{ITEM_AT}/by"),
        );

        // E0305: parameters that may not combine, at the item (a forbidden key at its own pointer).
        refuses(
            &with_command(r#""command": "seek", "by": "setting""#),
            &codes::E0305,
            ITEM_AT,
        );
        refuses(
            &with_command(r#""command": "seek", "by": 5, "dir": "back""#),
            &codes::E0305,
            &format!("{ITEM_AT}/dir"),
        );
        let ok = with_command(r#""command": "seek", "by": "setting", "dir": "back""#);
        assert!(parse_text(&ok).findings.is_empty());
        let ok = with_command(r#""command": "seek", "by": -15"#);
        assert!(parse_text(&ok).findings.is_empty());
    }

    #[test]
    fn a_face_map_belongs_to_a_command_with_states() {
        let ok = with_command(
            r#""command": "previous", "face": { "default": "builtin:previous", "restart": "builtin:restart" }"#,
        );
        assert!(parse_text(&ok).findings.is_empty());
        let face = format!("{ITEM_AT}/face");
        // A state the map lacks: at the map. A state the command does not have: at the key.
        let lacks =
            with_command(r#""command": "previous", "face": { "default": "builtin:previous" }"#);
        refuses(&lacks, &codes::E0303, &face);
        let extra = with_command(
            r#""command": "previous", "face": { "default": "builtin:previous", "restart": "builtin:restart", "restrt": "builtin:next" }"#,
        );
        let finding = refuses(&extra, &codes::E0302, &format!("{face}/restrt"));
        assert_eq!(finding.nearest.first().map(String::as_str), Some("restart"));
        // A command with one face takes no map.
        let single = with_command(r#""command": "next", "face": { "default": "builtin:next" }"#);
        refuses(&single, &codes::E0304, &face);
        // A state's face is a face like any other.
        let unknown = with_command(
            r#""command": "previous", "face": { "default": "nope", "restart": "builtin:restart" }"#,
        );
        refuses(&unknown, &codes::E0203, &format!("{face}/default"));
        // A seek's map needs `by: "setting"`; its interval ladder is keyed by whole seconds.
        let fixed =
            with_command(r#""command": "seek", "by": 5, "face": { "default": "builtin:next" }"#);
        refuses(&fixed, &codes::E0305, &face);
        let ladder = with_command(
            r#""command": "seek", "by": "setting", "dir": "forward", "face": { "default": "builtin:next", "by_interval": { "10": "builtin:next", "30": "builtin:next" } }"#,
        );
        let parsed = parse_text(&ladder);
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        let package = parsed.package.expect("package");
        let map = control(&package.layout, "more")
            .get("face")
            .and_then(Val::as_object)
            .expect("face");
        let rungs = map
            .get("by_interval")
            .and_then(Val::as_object)
            .expect("ladder");
        assert_eq!((map.node, rungs.node), ("face_map", "face_ladder"));
        assert_eq!(rungs.fields.len(), 2);
        let bad_rung = with_command(
            r#""command": "seek", "by": "setting", "dir": "forward", "face": { "default": "builtin:next", "by_interval": { "ten": "builtin:next" } }"#,
        );
        refuses(&bad_rung, &codes::E0102, &format!("{face}/by_interval/ten"));
    }

    #[test]
    fn conditions_are_e0501_to_e0503() {
        let band_when = |when: &str| {
            edit(
                MINIMAL,
                &[(
                    r#"{ "id": "row", "gap_before": "SM""#,
                    &format!(r#"{{ "id": "row", "when": {when}, "gap_before": "SM""#),
                )],
            )
        };
        let at = "/layout/block/1/when";
        let finding = refuses(&band_when(r#""panel.nope""#), &codes::E0501, at);
        assert!(
            finding
                .nearest
                .first()
                .is_some_and(|n| n.starts_with("panel.")),
            "{:?}",
            finding.nearest
        );
        refuses(
            &band_when(r#"["panel.info", "!panel.nope"]"#),
            &codes::E0501,
            &format!("{at}/1"),
        );
        for shape in [
            r#""!""#,
            r#""!!panel.info""#,
            r#""""#,
            "[]",
            "5",
            "null",
            r#"{ "a": 1 }"#,
        ] {
            refuses(&band_when(shape), &codes::E0502, at);
        }
        refuses(
            &band_when(r#"["panel.info", 3]"#),
            &codes::E0502,
            &format!("{at}/1"),
        );
        refuses(&band_when(r#""item.focused""#), &codes::E0503, at);
        refuses(
            &band_when(r#"["panel.info", "!item.is_current"]"#),
            &codes::E0503,
            &format!("{at}/1"),
        );

        let parsed = parse_text(&band_when(r#"["panel.info", "!transport.paused"]"#));
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        let package = parsed.package.expect("package");
        let bands = package
            .layout
            .get("block")
            .and_then(Val::as_list)
            .expect("block");
        let row = bands[1].as_object().expect("row");
        assert_eq!(
            row.get("when"),
            Some(&Val::Cond(cond(&["panel.info", "!transport.paused"])))
        );
    }

    #[test]
    fn a_state_keyed_value_is_exactly_value_and_if_e0504() {
        let gap = |keyed: &str| {
            edit(
                MINIMAL,
                &[(
                    r#""gap_before": "SM""#,
                    &format!(r#""gap_before": {keyed}"#),
                )],
            )
        };
        let at = "/layout/block/1/gap_before";
        let ok = parse_text(&gap(
            r#"{ "value": "SM", "if": { "panel.info": "XS", "!panel.chapters": 12 } }"#,
        ));
        assert!(ok.findings.is_empty(), "{:#?}", ok.findings);
        let package = ok.package.expect("package");
        let bands = package
            .layout
            .get("block")
            .and_then(Val::as_list)
            .expect("block");
        assert_eq!(
            bands[1].as_object().and_then(|b| b.get("gap_before")),
            Some(&Val::Keyed(Keyed {
                base: Box::new(name("SM")),
                cases: vec![
                    (cond(&["panel.info"]), name("XS")),
                    (cond(&["!panel.chapters"]), Val::Int(12)),
                ],
            }))
        );

        for malformed in [
            r#"{ "value": "SM" }"#,
            r#"{ "if": { "panel.info": "XS" } }"#,
            r#"{ "value": "SM", "if": {} }"#,
            r#"{ "value": "SM", "if": [ "panel.info" ] }"#,
            r#"{ "value": "SM", "if": { "panel.info": "XS" }, "extra": 1 }"#,
            r#"{}"#,
        ] {
            refuses(&gap(malformed), &codes::E0504, at);
        }
        // A case that keys again, and a field that cannot be keyed.
        refuses(
            &gap(
                r#"{ "value": "SM", "if": { "panel.info": { "value": "XS", "if": { "panel.chapters": "SM" } } } }"#,
            ),
            &codes::E0504,
            &format!("{at}/if/panel.info"),
        );
        refuses(
            &edit(
                MINIMAL,
                &[(
                    r#""id": "bar""#,
                    r#""id": { "value": "bar", "if": { "panel.info": "x" } }"#,
                )],
            ),
            &codes::E0504,
            "/layout/block/0/id",
        );
        // The condition key is a condition; the case value is the field's type; a key twice is E0101.
        refuses(
            &gap(r#"{ "value": "SM", "if": { "panel.nope": "XS" } }"#),
            &codes::E0501,
            &format!("{at}/if/panel.nope"),
        );
        refuses(
            &gap(r#"{ "value": "SM", "if": { "item.focused": "XS" } }"#),
            &codes::E0503,
            &format!("{at}/if/item.focused"),
        );
        refuses(
            &gap(r#"{ "value": "SM", "if": { "panel.info": "HUGE" } }"#),
            &codes::E0201,
            &format!("{at}/if/panel.info"),
        );
        refuses(
            &gap(r#"{ "value": "HUGE", "if": { "panel.info": "XS" } }"#),
            &codes::E0201,
            &format!("{at}/value"),
        );
        refuses(
            &gap(r#"{ "value": "SM", "if": { "panel.info": "XS", "panel.info": "MD" } }"#),
            &codes::E0101,
            &format!("{at}/if/panel.info"),
        );
        // A plain object where a plain value goes is a wrong type, not a keyed value.
        refuses(
            &edit(MINIMAL, &[(r#""name": "Minimal""#, r#""name": {}"#)]),
            &codes::E0104,
            "/name",
        );
    }

    #[test]
    fn palette_names_and_values_are_e0505_and_e0506() {
        refuses(
            &with_header(r##""palette": { "Gold": "#f2b535" }"##),
            &codes::E0505,
            "/palette/Gold",
        );
        refuses(
            &with_header(r##""palette": { "1st": "#f2b535" }"##),
            &codes::E0505,
            "/palette/1st",
        );
        refuses(
            &with_header(r##""palette": { "ACCENT": "#f2b535" }"##),
            &codes::E0505,
            "/palette/ACCENT",
        );
        for value in [
            "#F2B535",
            "#fff",
            "#f2b53",
            "#f2b535ff",
            "f2b535",
            "red",
            "#g2b535",
            "",
            "# f2b53",
        ] {
            refuses(
                &with_header(&format!(r##""palette": {{ "gold": "{value}" }}"##)),
                &codes::E0506,
                "/palette/gold",
            );
        }
        refuses(
            &with_header(r#""palette": { "gold": 7 }"#),
            &codes::E0104,
            "/palette/gold",
        );
        refuses(
            &with_header(r##""palette": [ "#f2b535" ]"##),
            &codes::E0104,
            "/palette",
        );
        refuses(
            &with_header(r##""palette": { "gold": "#f2b535", "gold": "#000000" }"##),
            &codes::E0101,
            "/palette/gold",
        );
        let parsed = parse_text(&with_header(
            r##""palette": { "gold": "#f2b535", "ink_2": "#000000" }"##,
        ));
        assert_eq!(
            parsed.package.expect("package").palette,
            vec![
                ("gold".to_owned(), [0xf2, 0xb5, 0x35]),
                ("ink_2".to_owned(), [0, 0, 0])
            ]
        );
    }

    #[test]
    fn assets_are_ids_and_pass_the_svg_subset() {
        let parsed = parse_text(&with_header(&format!(r#""assets": {{ "mark": "{SVG}" }}"#)));
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        assert_eq!(
            parsed.package.expect("package").assets,
            vec![("mark".to_owned(), SVG.to_owned())]
        );

        // An SVG finding is merged as it is: its code, its pointer.
        let circle = "<svg viewBox='0 0 24 24'><circle r='1'/></svg>";
        let parsed = parse_text(&with_header(&format!(
            r#""assets": {{ "mark": "{circle}" }}"#
        )));
        assert!(parsed.package.is_none());
        let direct =
            svg_subset::check_asset("/assets/mark", circle).expect_err("a circle is refused");
        assert_eq!(parsed.findings, direct);
        assert_eq!(parsed.findings[0].pointer, "/assets/mark");
        assert_eq!(parsed.findings[0].code, codes::E0701.id);

        refuses(
            &with_header(&format!(r#""assets": {{ "Bad Id": "{SVG}" }}"#)),
            &codes::E0105,
            "/assets/Bad Id",
        );
        refuses(
            &with_header(r#""assets": { "mark": 5 }"#),
            &codes::E0104,
            "/assets/mark",
        );
        refuses(
            &with_header(r#""assets": [ "x" ]"#),
            &codes::E0104,
            "/assets",
        );
        refuses(
            &with_header(&format!(
                r#""assets": {{ "mark": "{SVG}", "mark": "{SVG}" }}"#
            )),
            &codes::E0101,
            "/assets/mark",
        );

        // A face may name a declared asset, wherever `assets` is written; findings stay in document order.
        let with_face = edit(
            MINIMAL,
            &[(r#""size": 52 }"#, r#""size": 52, "face": "late" }"#)],
        );
        let body = with_face.trim_end().strip_suffix('}').expect("an object");
        let late = format!(r#"{body}, "assets": {{ "late": "{SVG}" }} }}"#);
        let parsed = parse_text(&late);
        assert!(parsed.findings.is_empty(), "{:#?}", parsed.findings);
        let package = parsed.package.expect("package");
        assert_eq!(
            control(&package.layout, "more").get("face"),
            Some(&Val::Ref("late".to_owned()))
        );

        let both = edit(
            &late,
            &[(r#""fill": "ACCENT""#, r#""fill": "ACCNT""#), (SVG, circle)],
        );
        let parsed = parse_text(&both);
        assert!(parsed.package.is_none());
        assert_eq!(
            parsed.findings[0].code,
            codes::E0202.id,
            "the layout's finding stands first"
        );
        assert!(parsed.findings[1..]
            .iter()
            .all(|f| f.pointer == "/assets/late" && f.code.starts_with("HUD-E07")));
        assert!(parsed.findings.len() >= 2, "{:#?}", parsed.findings);
    }

    // ----- (5) hostile input -----

    #[test]
    fn hostile_input_never_panics() {
        let deep_array = "[".repeat(100_000);
        let deep_object = "{\"a\":".repeat(100_000);
        let deep_layout = format!("{{\"format\": \"1.0\", \"layout\": {deep_array}");
        let inputs: [&str; 8] = [
            "",
            "{}",
            "null",
            "[]",
            "0",
            "\"1.0\"",
            &deep_array,
            &deep_object,
        ];
        for input in inputs {
            let parsed = parse_text(input);
            assert!(parsed.package.is_none(), "{input:.20}");
            assert!(!parsed.findings.is_empty());
        }
        only(&parse(deep_array.as_bytes()), &codes::E0100, "");
        only(&parse(deep_object.as_bytes()), &codes::E0100, "");
        only(&parse(deep_layout.as_bytes()), &codes::E0100, "");
        only(&parse(b"{}"), &codes::E0401, "/format");
        // Deep but legal nesting reaches the walker, which reports it and goes on.
        let nested = format!(
            r#"{{"format": "1.0", "id": {}0{}, "layout": {}0{}}}"#,
            "[".repeat(100),
            "]".repeat(100),
            "[".repeat(100),
            "]".repeat(100)
        );
        let parsed = parse_text(&nested);
        assert!(
            found(&parsed.findings).contains(&(codes::E0104.id, "/id")),
            "{:#?}",
            parsed.findings
        );
        assert!(found(&parsed.findings).contains(&(codes::E0104.id, "/layout")));
        // The fragment door too.
        for node in schema::all_nodes() {
            for input in ["", "{}", "null", "[]", "[[[[", "{\"a\":{\"b\":[{}]}}"] {
                let (object, findings) = validate_json(node, input, "/p", &Ctx::v1());
                assert_eq!(
                    object.is_some(),
                    !has_error(&findings),
                    "{} {input}",
                    node.name
                );
            }
        }
    }

    #[test]
    fn every_truncation_and_byte_change_of_a_package_is_handled() {
        for text in [MINIMAL, REFERENCE] {
            let bytes = text.as_bytes();
            let step = if text.len() > 2_000 { 5 } else { 1 };
            for end in (0..bytes.len()).step_by(step) {
                parse_text_bytes(&bytes[..end]);
            }
            for at in (0..bytes.len()).step_by(step) {
                for replacement in [b'"', b'{', b'}', b',', b'1', b'-', 0xff, b'\\'] {
                    let mut changed = bytes.to_vec();
                    changed[at] = replacement;
                    parse_text_bytes(&changed);
                }
            }
        }
    }

    fn parse_text_bytes(bytes: &[u8]) {
        let parsed = parse(bytes);
        assert_eq!(
            parsed.package.is_some(),
            !has_error(&parsed.findings),
            "{:#?}",
            parsed.findings
        );
        assert_eq!(parse(bytes), parsed, "the same bytes give the same answer");
    }
}
