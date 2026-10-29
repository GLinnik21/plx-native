//! **The descriptor table of HUD package format 1.0.** One static table names every key, cell kind,
//! widget, rest and focus style, command (with its typed parameters), condition and theme token: its
//! type, allowed values, default and the format minor it arrived in. The strict parser
//! ([`super::parse`]) is a generic walker over this table and wave B's schema emitter
//! (`plxnative-sim --hud-schema`) serializes it, so nothing is listed twice and the two cannot drift
//! (a test walks the table, proves every entry is accepted, and proves an unlisted name is refused).
//!
//! The table is `'static` data: plain `&'static` slices and `Copy` types, no `Vec`, no `Box`, no
//! globals to read, no IO.
//!
//! **Contract freeze.** The TYPES and the lookup functions below are final. The DATA is partly landed:
//! [`COMMANDS`], [`CONDITIONS`], the four resolver tables and the top-level node [`ROOT`] (with
//! `app`) are complete for 1.0; the layout nodes ([`ROOT`]'s `layout` and everything under it: bands,
//! cells, control items, segments, the strip, Up Next) are worker B's, marked `CONTRACT FREEZE: B fills`.
//! Until B lands them `node("layout")` is `None`, and a walker must treat a [`Ty::Object`] whose node
//! is missing as an accept-nothing table bug, not a package error.
//!
//! ## How a walker reads the table (the contract between table and parser)
//!
//! Walk an object against its [`Node`]: every key must be a [`Field::key`] of the node (else `E0102`
//! at the key's pointer, `nearest` = the node's keys); a duplicate key is `E0101` (the reader keeps
//! duplicates, `ui::rec::strict_json::Ordered`); every `required` field must be present (else `E0103`
//! at the OBJECT's pointer); a field whose `since` is above the package's format is `E0107` at the key.
//! The parser applies NO defaults: [`Field::default`] documents the app's default and feeds the schema
//! emitter, and wave B's compiler applies it. Then check each value against its [`Ty`]:
//!
//! | `Ty` | JSON accepted | `parse::Val` | on failure |
//! |---|---|---|---|
//! | `Bool` | `true`/`false` | `Bool` | `E0104` |
//! | `Int {min,max}` | an integer in `min..=max` (`1.0` is not an integer) | `Int` | `E0104`, range `E0105` |
//! | `Num {min,max}` | any number in `min..=max` (an integer converts) | `Num` | `E0104`, range `E0105` |
//! | `Text {max_len}` | a string of at most `max_len` chars, no control characters | `Text` | `E0104`, `E0105` |
//! | `Pattern(p)` | a string matching [`Pattern`] `p` | `Text` | `E0104`, `E0105` |
//! | `Enum(names)` | a string equal to one of `names` | `Name` | `E0104`, unknown `E0201` + `nearest` |
//! | `EnumList(names)` | an array of DISTINCT `names` (an ordered subset; a repeat is `E0105`) | `List` of `Name` | `E0104`, `E0201` |
//! | `Bind(names)` | a string in `names`; an [`is_card_scoped`] name outside a strip card is `E0201` | `Name` | `E0104`, `E0201` |
//! | `Size` | a [`SIZE_RUNGS`] name | `Name` | `E0104`, `E0201` |
//! | `Gap` | a [`SPACE_TOKENS`] name, or an integer `0..=1080` (px) | `Name` / `Int` | `E0104`, `E0201`, `E0105` |
//! | `Ink` | an UPPER_CASE [`INK_TOKENS`] name, or a lower-case palette name | `Token` / `Ref` | `E0104`, `E0202` (a string with an upper-case letter), `E0204` |
//! | `Asset` | an id declared in `assets` | `Ref` | `E0104`, `E0203` + `nearest` |
//! | `Face` | see below | `Ref` / `Name` / `Object` | `E0104`, `E0203`, `E0201`, `E0102`, `E0103` |
//! | `Cond` | a `when`: a condition string, or a non-empty array of them, each optionally `!`-prefixed | `Cond` | `E0501` `E0502` `E0503` |
//! | `Object(node)` | an object walked against `node(node)` | `Object` | `E0104` |
//! | `List {of,max_len}` | an array of at most `max_len` values of `of` | `List` | `E0104`, `E0105` |
//! | `Palette` | an object: lower-case name -> `"#rrggbb"` | `Object` of `Text` | `E0505`, `E0506`, `E0104` |
//! | `Assets` | an object: id ([`Pattern::Id`]) -> SVG source, each through `svg_subset::check_asset` | `Object` of `Text` | `E0105` (id), `E0104`, `E07xx` |
//! | `Command` | a string naming a [`Cmd`] | `Name` | `E0104`, `E0301` + `nearest` |
//! | `Either(a, b)` | the first of `a`, `b` whose JSON kind the value has | that one's | that one's, else `E0104` |
//!
//! **State-keyed values.** When a field is [`Field::keyable`] and the JSON value is an object, it must be
//! exactly `{"value": v, "if": {"<condition>": v2, ...}}` (anything else is `E0504` at the object):
//! `v` and every `v2` are walked against the field's `ty`; each key of `if` is one condition, optionally
//! `!`-prefixed, from [`CONDITIONS`] (`E0501`, `E0503`). The result is `Val::Keyed`, its cases in
//! DOCUMENT ORDER (the first condition that holds wins, in the compiler).
//!
//! **A face** (`Ty::Face`, the control item's `face`): a string is an asset id (`E0203` if undeclared) or
//! `builtin:<icon>` with `<icon>` in [`BUILTIN_ICONS`] (`E0201`). An object is a face MAP, legal only when
//! the item's command has [`Cmd::face_states`] (else `E0304`): its keys must be those states (`E0102`),
//! each `required` one present (`E0103` at the map), each value a single face; a state with
//! [`FaceState::ladder`] holds an object keyed by decimal seconds (a positive integer, else `E0102`)
//! whose values are single faces.
//!
//! **A node** by [`NodeKind`]: `Plain` is a plain object; `OneOf(keys)` also requires EXACTLY ONE of
//! `keys` (each is also a `Field` of `Ty::Object`): none is `E0103`, several `E0106`, both at the object;
//! `CommandDispatched` is a control item: read the item's `command` first (`E0301` if unknown, `E0103` if
//! absent), then the legal keys are the node's `fields` plus that [`Cmd::params`] (an unknown one is
//! `E0302`, a missing required one `E0303`, a bad value `E0304`, a failed [`Cmd::rules`] entry `E0305`).
//!
//! **Rules** ([`Rule`]) are the cross-key invariants; each is checked after the keys and values, and its
//! doc names the code (a rule of a [`Cmd`] reports the `E03xx` form of the same code, pointer = the item).

use plx_ui::icons::Icon;
use plx_ui::theme;

/// A format version: `(major, minor)`.
pub type Since = (u8, u8);

/// The format this build writes and the newest minor it reads of that major.
pub const CURRENT: Since = (1, 0);
/// Every format major this build reads, with the highest minor of that major it understands. A package
/// whose major is absent here is `E0403`; one whose minor is above its major's entry is `E0402`.
pub const SUPPORTED: &[(u8, u8)] = &[(1, 0)];
/// The version every 1.0 entry arrived in.
pub const V1_0: Since = (1, 0);

/// A string shape a [`Ty::Pattern`] checks. Each is a closed grammar, never a regular expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// `[a-z0-9.-]`, at least two dot-separated labels, each starting with a letter: `com.plxnative.x`.
    ReverseDns,
    /// `MAJOR.MINOR.PATCH` numeric semver (no pre-release, no build metadata).
    Semver,
    /// `MAJOR.MINOR`, both unsigned decimal.
    FormatVersion,
    /// An `https://` URL (editors only: accepted, its value ignored; lint checks its major.minor).
    SchemaUrl,
    /// An author-chosen identifier: lower-case `[a-z][a-z0-9_-]*`, at most 40 chars.
    Id,
}

/// What a value is. Exhaustively matchable: a walker and the schema emitter each `match` it in full, so a
/// new variant is a compile error in both. The table above says what each accepts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Ty {
    Bool,
    Int {
        min: i64,
        max: i64,
    },
    Num {
        min: f64,
        max: f64,
    },
    Text {
        max_len: usize,
    },
    Pattern(Pattern),
    Enum(&'static [&'static str]),
    EnumList(&'static [&'static str]),
    Bind(&'static [&'static str]),
    Size,
    Gap,
    Ink,
    Asset,
    Face,
    Cond,
    /// A nested object, by [`Node::name`] (looked up with [`node`]).
    Object(&'static str),
    List {
        of: &'static Ty,
        max_len: usize,
    },
    Palette,
    Assets,
    Command,
    Either(&'static Ty, &'static Ty),
}

/// One key of a node (or one parameter of a command).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub ty: Ty,
    /// Missing is `E0103` (`E0303` for a command parameter).
    pub required: bool,
    /// May be state-keyed one level deep (`{"value": v, "if": {...}}`). Only a number (not a control's
    /// `size`), an ink, a gap or `bold` is; a control's `size` is a plain number, never keyed.
    pub keyable: bool,
    /// The format minor the key arrived in. A package declaring an earlier format that uses it: `E0107`.
    pub since: Since,
    /// The app's default, as JSON text (`"false"`, `"\"start\""`, `"72"`), or `None` for no default. For
    /// documentation and the schema emitter: the parser applies none.
    pub default: Option<&'static str>,
    pub doc: &'static str,
}

impl Field {
    /// A required key, since 1.0, not keyable, no default.
    pub const fn req(key: &'static str, ty: Ty, doc: &'static str) -> Field {
        Field {
            key,
            ty,
            required: true,
            keyable: false,
            since: V1_0,
            default: None,
            doc,
        }
    }
    /// An optional key, since 1.0, not keyable, no default.
    pub const fn opt(key: &'static str, ty: Ty, doc: &'static str) -> Field {
        Field {
            key,
            ty,
            required: false,
            keyable: false,
            since: V1_0,
            default: None,
            doc,
        }
    }
    /// The same field, state-keyable.
    pub const fn keyable(mut self) -> Field {
        self.keyable = true;
        self
    }
    /// The same field with its documented default (JSON text).
    pub const fn default(mut self, json: &'static str) -> Field {
        self.default = Some(json);
        self
    }
    /// The same field, arrived in `since`.
    pub const fn at(mut self, since: Since) -> Field {
        self.since = since;
        self
    }
}

/// How a node's object is read (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Plain,
    /// Exactly one of these keys is the object's content (a cell's `text`, `image`, `playbar`, ...).
    OneOf(&'static [&'static str]),
    /// A control item: `command` selects a [`Cmd`], whose [`Cmd::params`] are sibling keys.
    CommandDispatched,
}

/// A cross-key invariant. `E01xx` is reported for a node, `E03xx` for a [`Cmd`] (same pointer rule:
/// the object). Key names are siblings in the same object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// `key` present requires `other` present: else `E0106` (`E0305`).
    Requires {
        key: &'static str,
        other: &'static str,
    },
    /// `a` and `b` are never both present: `E0106` (`E0305`).
    Excludes { a: &'static str, b: &'static str },
    /// When `key` is present and its NAME is `equals`: each of `require` must be present (`E0103`,
    /// `E0303`) and each of `forbid` absent (`E0106`, `E0305`). Scrim: `cover: "block"` requires
    /// `extra` and forbids `height`.
    IfValue {
        key: &'static str,
        equals: &'static str,
        require: &'static [&'static str],
        forbid: &'static [&'static str],
    },
    /// The same, when `key` is present and is NOT the name `equals` (any other name or any number).
    IfNotValue {
        key: &'static str,
        equals: &'static str,
        require: &'static [&'static str],
        forbid: &'static [&'static str],
    },
    /// The pair `a == a_value` with `b == b_value` (both names) is rejected: `E0106`. A control draws
    /// one face twice with `rest: "solid"` and `focus: "accent"`.
    ForbidPair {
        a: &'static str,
        a_value: &'static str,
        b: &'static str,
        b_value: &'static str,
    },
    /// The lists `a` and `b` share no element: `E0106`. `content_rating` is in a facts `order` OR
    /// `badges`, never both.
    Disjoint { a: &'static str, b: &'static str },
    /// The integer at `key` is not zero: `E0105` (`E0304`) at the value. A `seek` `by` of 0 does nothing.
    NonZero(&'static str),
    /// `key` may hold the name `name` only when `other` holds the name `other_value`: `E0106`. `HUD_TITLE`
    /// is legal only on a text cell bound to `title.main`.
    NameOnlyWith {
        key: &'static str,
        name: &'static str,
        other: &'static str,
        other_value: &'static str,
    },
    /// The number at `key` is at most the number at `other` (both present): `E0105` at `key`'s value.
    /// A rect's `bleed` is at most its `pad`.
    AtMost {
        key: &'static str,
        other: &'static str,
    },
    /// A [`Cmd`]-only rule: the item's `face` may be a MAP (see [`Cmd::face_states`]) only when its
    /// parameter `param` is the name `equals`; else `E0305`. A `seek` face map needs `by: "setting"`.
    FaceMapOnlyWhen {
        param: &'static str,
        equals: &'static str,
    },
}

/// One state of a command's face map.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FaceState {
    pub key: &'static str,
    /// The map must carry it.
    pub required: bool,
    /// The value is an object keyed by decimal seconds (the viewer's Skip-interval ladder), each a face,
    /// not a single face. A rung the ladder has and the map lacks is a lint warning, never a load failure.
    pub ladder: bool,
}

/// A node of the package tree: the top level, `layout`, a band, a cell, a control item, ...
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Node {
    /// The name `Ty::Object` refers to and `parse::Object::node` carries.
    pub name: &'static str,
    pub kind: NodeKind,
    pub fields: &'static [Field],
    pub rules: &'static [Rule],
    pub doc: &'static str,
}

/// A command of the closed, app-owned vocabulary (D4): a control names one and gives its parameters as
/// sibling keys. A new command is a format MINOR.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cmd {
    pub name: &'static str,
    pub since: Since,
    pub params: &'static [Field],
    pub rules: &'static [Rule],
    /// The states of this command's face MAP, empty when the command has only a single face.
    pub face_states: &'static [FaceState],
    pub doc: &'static str,
}

impl Cmd {
    /// The parameter named `key`.
    pub fn param(&self, key: &str) -> Option<&'static Field> {
        let params: &'static [Field] = self.params;
        params.iter().find(|f| f.key == key)
    }
}

/// A `when` condition (and the key of a state-keyed value). Names starting `item.` are legal only in a
/// strip card ([`is_card_scoped`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Condition {
    pub name: &'static str,
    pub since: Since,
    pub doc: &'static str,
}

/// A binding or condition that exists only inside a strip card (`item.thumb`, `item.is_current`).
pub fn is_card_scoped(name: &str) -> bool {
    name.starts_with("item.")
}

// ---------------------------------------------------------------------------------------------------
// Commands (complete for 1.0)
// ---------------------------------------------------------------------------------------------------

/// The ten commands of 1.0. Up Next's `watch_credits` and `play_next` are fixed buttons of
/// `layout.up_next`, not part of this vocabulary.
pub static COMMANDS: &[Cmd] = &[
    Cmd {
        name: "toggle_play",
        since: V1_0,
        params: &[],
        rules: &[],
        face_states: &[
            FaceState { key: "playing", required: true, ladder: false },
            FaceState { key: "paused", required: true, ladder: false },
        ],
        doc: "Play or pause; also the Play, Pause and PlayPause keys and OK on the picture.",
    },
    Cmd {
        name: "seek",
        since: V1_0,
        params: &[
            Field::req(
                "by",
                Ty::Either(
                    &Ty::Int { min: -2_147_483_648, max: 2_147_483_647 },
                    &Ty::Enum(&["setting"]),
                ),
                "A nonzero number of seconds, its sign the direction (then `dir` is forbidden), or \"setting\": the viewer's Skip interval (then `dir` is required). A literal is not capped: the target clamps to the content.",
            ),
            Field::opt("dir", Ty::Enum(&["back", "forward"]), "The direction of a `by: \"setting\"` seek."),
        ],
        rules: &[
            Rule::NonZero("by"),
            Rule::IfValue { key: "by", equals: "setting", require: &["dir"], forbid: &[] },
            Rule::IfNotValue { key: "by", equals: "setting", require: &[], forbid: &["dir"] },
            Rule::FaceMapOnlyWhen { param: "by", equals: "setting" },
        ],
        face_states: &[
            FaceState { key: "default", required: true, ladder: false },
            FaceState { key: "by_interval", required: false, ladder: true },
        ],
        doc: "Jump the playhead; also Left/Right, FF/REW and the held scrub.",
    },
    Cmd {
        name: "previous",
        since: V1_0,
        params: &[],
        rules: &[],
        face_states: &[
            FaceState { key: "default", required: true, ladder: false },
            FaceState { key: "restart", required: true, ladder: false },
        ],
        doc: "The previous episode (or a restart of this one). Absent for an item with no neighbour.",
    },
    Cmd {
        name: "next",
        since: V1_0,
        params: &[],
        rules: &[],
        face_states: &[],
        doc: "The next episode. Absent for an item with no neighbour.",
    },
    Cmd {
        name: "open_tracks",
        since: V1_0,
        params: &[Field::req(
            "tab",
            Ty::Enum(&["subtitles", "audio"]),
            "Which tab of the track menu opens first.",
        )],
        rules: &[],
        face_states: &[],
        doc: "The subtitle and audio track menu.",
    },
    Cmd {
        name: "open_more",
        since: V1_0,
        params: &[Field::opt(
            "page",
            Ty::Enum(&["root", "quality", "speed"]),
            "Which page of the More menu opens.",
        )
        .default("\"root\"")],
        rules: &[],
        face_states: &[],
        doc: "The More menu (Stats for nerds, Quality, Speed and every folded command). The one required control.",
    },
    Cmd {
        name: "toggle_section",
        since: V1_0,
        params: &[Field::req(
            "section",
            Ty::Enum(&["chapters", "info"]),
            "Which section opens or closes.",
        )],
        rules: &[],
        face_states: &[],
        doc: "Open or close the chapters or the info section.",
    },
    Cmd {
        name: "from_beginning",
        since: V1_0,
        params: &[],
        rules: &[],
        face_states: &[],
        doc: "The Info card's From the Beginning action.",
    },
    Cmd {
        name: "go_to_detail",
        since: V1_0,
        params: &[],
        rules: &[],
        face_states: &[],
        doc: "Leave playback for the playing item's own page.",
    },
    Cmd {
        name: "toggle_stats",
        since: V1_0,
        params: &[],
        rules: &[],
        face_states: &[
            FaceState { key: "on", required: true, ladder: false },
            FaceState { key: "off", required: true, ladder: false },
        ],
        doc: "Stats for nerds, on or off for this session.",
    },
];

/// The conditions of 1.0 (`when` and the keys of a state-keyed value): a string, or an array that must
/// all hold, each optionally prefixed `!`. AND only, no OR, no comparison.
pub static CONDITIONS: &[Condition] = &[
    Condition {
        name: "panel.chapters",
        since: V1_0,
        doc: "The chapters section is open.",
    },
    Condition {
        name: "panel.info",
        since: V1_0,
        doc: "The info section is open.",
    },
    Condition {
        name: "sections",
        since: V1_0,
        doc: "Any section is open.",
    },
    Condition {
        name: "chapters.present",
        since: V1_0,
        doc: "The playing item has chapters.",
    },
    Condition {
        name: "title.is_episode",
        since: V1_0,
        doc: "The playing item is a real episode (has an address): a show's trailer is not.",
    },
    Condition {
        name: "transport.paused",
        since: V1_0,
        doc: "Playback is paused.",
    },
    Condition {
        name: "slot.discs",
        since: V1_0,
        doc: "The control slot shows the discs.",
    },
    Condition {
        name: "slot.up_next",
        since: V1_0,
        doc: "The control slot shows the Up Next offer.",
    },
    Condition {
        name: "slot.skip",
        since: V1_0,
        doc: "The control slot shows the Skip pill.",
    },
    Condition {
        name: "item.is_current",
        since: V1_0,
        doc: "In a strip card: the card is the playing chapter.",
    },
    Condition {
        name: "item.focused",
        since: V1_0,
        doc: "In a strip card: the card is focused.",
    },
];

// ---------------------------------------------------------------------------------------------------
// Theme-tied allow-lists and their resolvers (complete for 1.0 but the icon list, which B extends)
// ---------------------------------------------------------------------------------------------------

/// The theme tokens a package may ink with (D12). Excluded: `PASS_GOLD`/`PASS_GOLD_INK` (another
/// company's brand), `DIAG_*`, `RATING_*` and the semantic amber, red and green stops.
pub static INK_TOKENS: &[(&str, [f32; 4])] = &[
    ("TEXT_PRIMARY", theme::TEXT_PRIMARY),
    ("TEXT_SECONDARY", theme::TEXT_SECONDARY),
    ("TEXT_TERTIARY", theme::TEXT_TERTIARY),
    ("ACCENT", theme::ACCENT),
    ("ACCENT_INK", theme::ACCENT_INK),
    ("RAIL_TRACK", theme::RAIL_TRACK),
    ("HAIRLINE", theme::HAIRLINE),
    ("RESUME_FILL", theme::RESUME_FILL),
    ("PANEL_TOP", theme::PANEL_TOP),
    ("INK_DISABLED", theme::INK_DISABLED),
    (
        "CONTROL_RIM_FOCUS_UNKEYED",
        theme::CONTROL_RIM_FOCUS_UNKEYED,
    ),
];

/// The pixel size of the `HUD_TITLE` rung: the existing carve-out of the now-playing title, 54 px. It
/// mirrors `player_hud`'s private `HUD_TITLE_SZ`; wave B, which has `player_hud` in scope, makes that one
/// const the source and a test pin the two together.
pub const HUD_TITLE_PX: i32 = 54;

/// The type-scale rungs a text cell may name: `theme::size` but for `DIAGNOSTIC` (the diagnostics
/// instrument's own rung, below the couch floor), plus `HUD_TITLE`, the existing 54 px carve-out, legal
/// only on a text cell bound to `title.main` (a [`Rule::NameOnlyWith`]).
pub static SIZE_RUNGS: &[(&str, i32)] = &[
    ("HERO", theme::size::HERO),
    ("DISPLAY", theme::size::DISPLAY),
    ("TITLE", theme::size::TITLE),
    ("HEADLINE", theme::size::HEADLINE),
    ("BODY", theme::size::BODY),
    ("LABEL", theme::size::LABEL),
    ("CAPTION", theme::size::CAPTION),
    ("MICRO", theme::size::MICRO),
    ("HUD_TITLE", HUD_TITLE_PX),
];

/// The spacing rungs (`theme::space`) a gap may name.
pub static SPACE_TOKENS: &[(&str, f32)] = &[
    ("XS", theme::space::XS),
    ("SM", theme::space::SM),
    ("MD", theme::space::MD),
    ("LG", theme::space::LG),
    ("XL", theme::space::XL),
];

/// The built-in glyphs a face may name as `builtin:<name>`: the ones classic's HUD draws. Worker B
/// extends it when the reference or a command's face states need another.
pub static BUILTIN_ICONS: &[(&str, Icon)] = &[
    ("previous", Icon::Previous),
    ("restart", Icon::Restart),
    ("pause", Icon::Pause),
    ("play", Icon::Play),
    ("next", Icon::Next),
    ("info", Icon::Info),
    ("more", Icon::More),
    ("cc", Icon::Cc),
    ("audio", Icon::Audio),
];

/// An allow-listed theme token's colour (straight, not premultiplied, RGBA: `theme`'s own value), or `None`.
pub fn ink_token(name: &str) -> Option<[f32; 4]> {
    INK_TOKENS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// A type-scale rung's pixel size (`HUD_TITLE` is 54), or `None`.
pub fn size_rung(name: &str) -> Option<i32> {
    SIZE_RUNGS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, px)| *px)
}

/// A spacing rung's pixels, or `None`.
pub fn space_token(name: &str) -> Option<f32> {
    SPACE_TOKENS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, px)| *px)
}

/// A built-in glyph by its name WITHOUT the `builtin:` prefix, or `None`.
pub fn builtin_icon(name: &str) -> Option<Icon> {
    BUILTIN_ICONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, icon)| *icon)
}

// ---------------------------------------------------------------------------------------------------
// Nodes
// ---------------------------------------------------------------------------------------------------

/// The package's top level.
pub static ROOT: Node = Node {
    name: "package",
    kind: NodeKind::Plain,
    fields: &[
        Field::opt(
            "$schema",
            Ty::Pattern(Pattern::SchemaUrl),
            "The schema URL, for editors: accepted and ignored by the app; lint checks its major.minor equals `format`.",
        ),
        Field::req("format", Ty::Pattern(Pattern::FormatVersion), "MAJOR.MINOR of the schema, bindings and command vocabulary."),
        Field::req("id", Ty::Pattern(Pattern::ReverseDns), "The package's reverse-DNS identity."),
        Field::req("version", Ty::Pattern(Pattern::Semver), "The package's own semver; it only orders updates of one id."),
        Field::req("name", Ty::Text { max_len: 64 }, "The display name."),
        Field::req("author", Ty::Text { max_len: 64 }, "The author."),
        Field::req("license", Ty::Text { max_len: 64 }, "An SPDX license identifier."),
        Field::opt("app", Ty::Object("app"), "The app releases this package renders correctly on."),
        Field::opt("palette", Ty::Palette, "Named colours: lower-case name -> \"#rrggbb\". Every entry must be used."),
        Field::opt("assets", Ty::Assets, "Inline SVG masks, by id."),
        // CONTRACT FREEZE: B fills the node "layout" (and every node under it).
        Field::req("layout", Ty::Object("layout"), "Where the HUD sits and what it holds."),
    ],
    rules: &[],
    doc: "A HUD package: one UTF-8 JSON file, `*.plxhud.json`, assets inline.",
};

/// `app`: an app-release range, an escape hatch for "this release renders me wrong".
pub static APP: Node = Node {
    name: "app",
    kind: NodeKind::Plain,
    fields: &[
        Field::opt(
            "min",
            Ty::Pattern(Pattern::Semver),
            "The oldest app release this package renders correctly on.",
        ),
        Field::opt(
            "max",
            Ty::Pattern(Pattern::Semver),
            "The newest app release this package renders correctly on.",
        ),
    ],
    rules: &[],
    doc: "An app-release range.",
};

// CONTRACT FREEZE: B appends every layout node to this list. `node(name)` and `all_nodes()` read it.
static NODES: &[&Node] = &[&ROOT, &APP];

/// Every node of the table, [`ROOT`] first.
pub fn all_nodes() -> &'static [&'static Node] {
    NODES
}

/// The node called `name`.
pub fn node(name: &str) -> Option<&'static Node> {
    let nodes: &'static [&'static Node] = NODES;
    nodes.iter().copied().find(|n| n.name == name)
}

/// The field of `node` whose key is `key`.
pub fn field(node: &'static Node, key: &str) -> Option<&'static Field> {
    node.fields.iter().find(|f| f.key == key)
}

/// The command called `name`.
pub fn command(name: &str) -> Option<&'static Cmd> {
    let commands: &'static [Cmd] = COMMANDS;
    commands.iter().find(|c| c.name == name)
}

/// The condition called `name` (without any `!` prefix).
pub fn condition(name: &str) -> Option<&'static Condition> {
    let conditions: &'static [Condition] = CONDITIONS;
    conditions.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod freeze_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn the_vocabulary_is_the_ten_commands_and_eleven_conditions_of_1_0() {
        let names: Vec<_> = COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "toggle_play",
                "seek",
                "previous",
                "next",
                "open_tracks",
                "open_more",
                "toggle_section",
                "from_beginning",
                "go_to_detail",
                "toggle_stats"
            ]
        );
        assert_eq!(CONDITIONS.len(), 11);
        assert!(COMMANDS
            .iter()
            .all(|c| c.since == V1_0 && !c.doc.is_empty()));
        assert!(CONDITIONS
            .iter()
            .all(|c| c.since == V1_0 && !c.doc.is_empty()));
        assert_eq!(
            command("seek")
                .and_then(|c| c.param("dir"))
                .map(|f| f.required),
            Some(false)
        );
        assert!(command("skip_back").is_none() && condition("item.focused").is_some());
        assert!(is_card_scoped("item.is_current") && !is_card_scoped("panel.info"));
        let unique_commands: HashSet<_> = names.iter().copied().collect();
        let unique_conditions: HashSet<_> = CONDITIONS.iter().map(|c| c.name).collect();
        assert_eq!(
            (unique_commands.len(), unique_conditions.len()),
            (10, 11),
            "names are unique"
        );
    }

    #[test]
    fn the_resolvers_cover_their_lists_and_follow_the_theme() {
        assert_eq!(INK_TOKENS.len(), 11);
        assert!(INK_TOKENS.iter().all(|(n, c)| ink_token(n) == Some(*c)));
        assert_eq!(ink_token("TEXT_PRIMARY"), Some(theme::TEXT_PRIMARY));
        assert!(ink_token("PASS_GOLD").is_none() && ink_token("text_primary").is_none());
        assert!(SIZE_RUNGS.iter().all(|(n, px)| size_rung(n) == Some(*px)));
        assert_eq!(size_rung("HUD_TITLE"), Some(54));
        assert_eq!(size_rung("CAPTION"), Some(theme::size::CAPTION));
        assert!(size_rung("DIAGNOSTIC").is_none());
        assert!(SPACE_TOKENS
            .iter()
            .all(|(n, px)| space_token(n) == Some(*px)));
        assert_eq!(space_token("MD"), Some(theme::space::MD));
        assert!(BUILTIN_ICONS
            .iter()
            .all(|(n, i)| builtin_icon(n) == Some(*i)));
        assert_eq!(builtin_icon("restart"), Some(Icon::Restart));
        assert!(builtin_icon("builtin:play").is_none());
    }

    #[test]
    fn the_landed_nodes_resolve_and_every_entry_is_since_1_0() {
        assert!(std::ptr::eq(node("package").unwrap(), &ROOT));
        assert!(std::ptr::eq(all_nodes()[0], &ROOT));
        assert_eq!(field(&ROOT, "format").map(|f| f.required), Some(true));
        assert!(field(&ROOT, "formt").is_none());
        let mut seen = HashSet::new();
        for n in all_nodes() {
            assert!(seen.insert(n.name), "node names are unique: {}", n.name);
            let keys: HashSet<_> = n.fields.iter().map(|f| f.key).collect();
            assert_eq!(keys.len(), n.fields.len(), "keys of {} are unique", n.name);
            assert!(n.fields.iter().all(|f| f.since <= CURRENT));
        }
        assert!(SUPPORTED.contains(&CURRENT));
    }
}
