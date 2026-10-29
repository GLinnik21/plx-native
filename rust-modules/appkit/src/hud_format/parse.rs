//! **The strict parser of a HUD package**: bytes in, a typed [`Package`] and every [`Finding`] out.
//!
//! It is a generic walker over the descriptor table ([`super::schema`]): nothing about a key, a cell kind
//! or a command is written here. The JSON is read through `plx_ui::rec::strict_json::decode_ordered`, the
//! one duplicate-key-preserving reader in the tree, so a duplicate key is a finding at its own pointer and
//! a state-keyed value keeps its conditions in document order. The module is pure: no IO, no globals.
//!
//! **Contract freeze.** The types and signatures here are final; the bodies of [`parse`] and
//! [`validate_json`] are worker C's, marked `CONTRACT FREEZE: C fills`.
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

use super::schema::{self, Since};
use super::Finding;

/// The most bytes a package file may carry; a larger input is `E0100` (a limit of the format: the
/// strict reader's memory is bounded by it).
pub const MAX_PACKAGE_BYTES: usize = 1024 * 1024;

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
// CONTRACT FREEZE: C fills. This stub panics, so no test can mistake it for a parser.
pub fn parse(_bytes: &[u8]) -> Parsed {
    unimplemented!("CONTRACT FREEZE: worker C writes the strict parser")
}

/// Walk `json` against `node` as if it sat at `pointer` in a package, resolving references against `ctx`:
/// the SAME walker [`parse`] runs, exposed so wave B's lint and tests can check one fragment (a cell, a
/// control item) without a whole package. Returns the typed object when it has no `Error` finding, and
/// every finding. `json` that is not JSON is `E0100` at `pointer`.
// CONTRACT FREEZE: C fills (parse is this walker plus the version gate and the top-level scalars).
pub fn validate_json(
    _node: &'static schema::Node,
    _json: &str,
    _pointer: &str,
    _ctx: &Ctx,
) -> (Option<Object>, Vec<Finding>) {
    unimplemented!("CONTRACT FREEZE: worker C writes the table walker")
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
