//! **The descriptor table of HUD package format 1.0.** One static table names every key, cell kind,
//! widget, rest and focus style, command (with its typed parameters), condition and theme token: its
//! type, allowed values, default and the format minor it arrived in. The strict parser
//! ([`super::parse`]) is a generic walker over this table and the schema emitter
//! ([`super::schema_emit`], which `plxnative-sim --hud-schema` will print) serializes it, so nothing is listed twice and the two cannot drift
//! (a test walks the table, proves every entry is accepted, and proves an unlisted name is refused).
//!
//! The table is `'static` data: plain `&'static` slices and `Copy` types, no `Vec`, no `Box`, no
//! globals to read, no IO.
//!
//! **Complete for 1.0.** The TYPES and the lookup functions below, and the DATA, are complete
//! for 1.0: [`COMMANDS`], [`CONDITIONS`], [`BINDINGS`], the resolver tables, and every node from
//! [`ROOT`] down (`layout`, bands, the twelve cell kinds, control items, the strip card, Up Next). A
//! [`Ty::Object`] whose node is missing is a table bug, which the tests of this module rule out.
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
//!
//! ## The layout nodes: rules the data follows, and what stays open
//!
//! **Required.** A key is `required` unless the spec marks it optional, states its default, or the worked
//! example leaves it out of some instance. So `layout` requires `anchor inset x_inset band sections block
//! up_next` (`scrim` is optional: absent is no scrim); a band requires `id` and `cells`; a control item
//! requires `id command widget size` (`widget` has no natural default: a `disc`, a `pill` and a `button` are
//! different controls); `up_next` requires every key, `still` and `caption` included, because R4 says both
//! buttons are placed there and the package "chooses whether" the still and caption draw. A text, icon,
//! rule, rect, facts and playbar cell requires its `ink` (or `fill` and `track`): no ink has a stated default.
//!
//! **State-keyed.** Keyable are the plain numbers (`Int`/`Num`), every `Ink`, every `Gap` and `bold`, as
//! the spec's "a number (not a control's `size`), an ink, a gap or `bold`" says, but for: a control's
//! `size` and the command parameters (a press fires one fixed command); `z` and `pad`/`bleed`, which are
//! structural or compared with each other by a [`Rule`] (a rule compares plain numbers, never cases); and
//! every [`Ty::Either`] width (`w`, `length`), which is a number OR the name `fill`, not a plain number. A
//! minor can make a key keyable; it cannot take that back, so the table starts at the stricter reading.
//!
//! **Bindings** are split by position ([`TEXT_BINDS`], [`IMAGE_BINDS`], [`STRIP_BINDS`], [`FACTS_BINDS`],
//! [`CARD_ART_BINDS`]); the rest of the plan's binding list (the `playbar.*`, `scrub.*`, `chrome.alpha`,
//! `slot.kind`, `focus.id`, `stats.on`, `seek.interval` names and the ones a condition already carries) is
//! consumed by the app's own widgets and bindable from no key in 1.0. [`BINDINGS`] lists every name.
//!
//! **`HUD_TITLE`** is a [`Ty::Size`] rung legal on `text.size` only under its [`Rule::NameOnlyWith`] (bound to
//! `title.main`). The other size keys (`facts.size`, `playbar.clock_size`) are [`Ty::Enum`]s over
//! [`BODY_SIZES`], the rungs but for `HUD_TITLE`, which a test ties to [`SIZE_RUNGS`].
//!
//! **Defaults** are recorded only where the spec or the app states one (`open_more.page`, `chrome`, `grab`,
//! `actions.axis`, `label_align`, `rest`/`focus`, `knob`/`clocks`/`mark` as classic has them) or the value is
//! the identity (`bold` false, a `pad` of 0, a cell `align` of `start`). The compiler reads them from here.
//!
//! **Open items, not settled by the spec** (the compiler and lint inherit them):
//! * `rest: "custom"`: the spec says "with allowed inks" but names no key, so 1.0 has none; the name is
//!   accepted and the compiler decides what it draws.
//! * `valign` has no default; a band cell without `align` packs at `start`.
//! * `label_align` is spelled `centre` (the spec's spelling) while `align` is spelled `center`.
//! * D10's "app text takes no `w` and no `max_lines`" is a set of [`Rule::IfValue`] entries on the three
//!   app-only binds (`label.chapters`, `label.info`, `item.start`); `title.kicker` mixes app and server
//!   text and takes both.
//! * A cell kind that is a focus stop (`playbar`, `segment`, `strip`, `actions`) inside a strip card, a
//!   band id or segment id that names nothing, `MIN_CONTROL_SIZE`, the playbar band's `chrome`, the
//!   exactly-one `playbar`, id uniqueness and a `toggle_section` without its section band are compile and
//!   lint checks, not parse rules: the table has no variant for them.
//! * The list bounds (`block` 16 bands, `cells` 32, `items` 24, `actions.items` 12, card lists 8,
//!   `replaces` 16) are parser bounds on an untrusted file, not product limits.
//! * `rewind` and `fast-forward` are not [`BUILTIN_ICONS`] (see there).
//! * `inset`, `x_inset`, the scrim and similar layout-level keys are keyable like any gap or number.

use plx_ui::icons::Icon;
use plx_ui::theme;

use crate::player_hud::classic::{HUD_TITLE_SZ, SCRIM_H};

/// A format version: `(major, minor)`.
pub type Since = (u8, u8);

/// The format this build writes and the newest minor it reads of that major.
pub const CURRENT: Since = (1, 0);
/// Every format major this build reads, with the highest minor of that major it understands. A package
/// whose major is absent here is `E0403`; one whose minor is above its major's entry is `E0402`.
pub const SUPPORTED: &[(u8, u8)] = &[(1, 0)];
/// The version every 1.0 entry arrived in.
pub const V1_0: Since = (1, 0);

/// The one `id` that is not reverse-DNS: the built-in HUD (D2). [`Pattern::ReverseDns`] accepts exactly this
/// string besides reverse-DNS names, so the built-in's own file parses like any package. That the id parses
/// does not make a file the built-in: whether a package IS the built-in comes from where it was loaded
/// from (compiled in), never from this string, and a store that takes files from outside refuses it.
pub const BUILTIN_CLASSIC_ID: &str = "builtin:classic";

/// A string shape a [`Ty::Pattern`] checks. Each is a closed grammar, never a regular expression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// `[a-z0-9.-]`, at least two dot-separated labels, each starting with a letter: `com.plxnative.x`.
    /// The one other string it accepts is [`BUILTIN_CLASSIC_ID`].
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

impl Pattern {
    /// The shape as an ECMA-262 regular expression, for the published JSON Schema. The strict parser does not
    /// run it (a pattern is a closed grammar there, so no untrusted input reaches a regex engine); a test
    /// pins the two to the same answer. A length bound is the parser's, so a regex may accept a longer string.
    pub const fn regex(self) -> &'static str {
        match self {
            // The first alternative is [`BUILTIN_CLASSIC_ID`], the one string the parser accepts besides a
            // reverse-DNS name; a test pins the regex to the parser on it.
            Pattern::ReverseDns => r"^(builtin:classic|[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*)+)$",
            Pattern::Semver => r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$",
            Pattern::FormatVersion => r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$",
            Pattern::SchemaUrl => r"^https://[!-~]+$",
            Pattern::Id => r"^[a-z][a-z0-9_-]*$",
        }
    }
}

/// The shape of a palette name, and of an ink that refers to one: lower-case `[a-z][a-z0-9_]*`. As
/// [`Pattern::regex`]: the parser's own check is `parse::is_palette_name`, and a test pins the two.
pub const PALETTE_NAME_REGEX: &str = r"^[a-z][a-z0-9_]*$";

/// The shape of a palette value: `#` and six lower-case hex digits. As [`Pattern::regex`].
pub const HEX_COLOUR_REGEX: &str = r"^#[0-9a-f]{6}$";

/// The most px a [`Ty::Gap`] number may be: the canvas height, the 1080-tall logical screen. The parser reads
/// it from here, and a test pins its boundary.
pub const GAP_TOP_PX: i64 = 1080;

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

/// The type-scale rungs a text cell may name: `theme::size` but for `DIAGNOSTIC` (the diagnostics
/// instrument's own rung, below the couch floor), plus `HUD_TITLE`, the existing 54 px carve-out (the
/// now-playing title's own `classic::HUD_TITLE_SZ`, the one source), legal only on a text cell bound to
/// `title.main` (a [`Rule::NameOnlyWith`]).
pub static SIZE_RUNGS: &[(&str, i32)] = &[
    ("HERO", theme::size::HERO),
    ("DISPLAY", theme::size::DISPLAY),
    ("TITLE", theme::size::TITLE),
    ("HEADLINE", theme::size::HEADLINE),
    ("BODY", theme::size::BODY),
    ("LABEL", theme::size::LABEL),
    ("CAPTION", theme::size::CAPTION),
    ("MICRO", theme::size::MICRO),
    ("HUD_TITLE", HUD_TITLE_SZ),
];

/// The spacing rungs (`theme::space`) a gap may name.
pub static SPACE_TOKENS: &[(&str, f32)] = &[
    ("XS", theme::space::XS),
    ("SM", theme::space::SM),
    ("MD", theme::space::MD),
    ("LG", theme::space::LG),
    ("XL", theme::space::XL),
];

/// The built-in glyphs a face may name as `builtin:<name>`: the ones classic's HUD draws, plus `close`
/// and `check`. The names are kebab-case and mirror the files of `assets/icons/`. `rewind` and
/// `fast-forward` are deliberately NOT here: `Icon::Rewind` and `Icon::FastForward` are a read-out
/// beside the clock, "never a control" (the design system forbids a rewind button), so they cannot be a
/// control face; a package that wants them draws its own asset. A minor can add a glyph; none can be
/// removed, so the list starts at the stricter reading.
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
    ("close", Icon::Close),
    ("check", Icon::Check),
];

/// What precedes a built-in glyph's name in a face: `builtin:play`. The parser reads it from here.
pub const BUILTIN_FACE_PREFIX: &str = "builtin:";

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
// Bindings
// ---------------------------------------------------------------------------------------------------

/// A name the app's HUD model exposes to a package. [`BINDINGS`] lists every one of 1.0. Only some can be
/// WRITTEN into a package, each at the keys of the per-position lists below ([`TEXT_BINDS`] and so on);
/// the rest are consumed by the app's own widgets (the playbar, the chrome, the slot machinery) and are
/// not bindable from any key in 1.0, which is the stricter reading of the plan's binding list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: &'static str,
    pub since: Since,
    pub doc: &'static str,
}

/// Every binding of 1.0. A name starting `item.` exists only inside a strip card ([`is_card_scoped`]).
pub static BINDINGS: &[Binding] = &[
    Binding { name: "title.kicker", since: V1_0, doc: "The kicker line: an episode's \"S2, E3 · <episode title>\" (the numbering prefix is app text and never elides, the title is server text and may), else the item's context line." },
    Binding { name: "title.main", since: V1_0, doc: "The playing item's title (server text)." },
    Binding { name: "title.is_episode", since: V1_0, doc: "Whether the item is a real episode; the same name is the `title.is_episode` condition. Not bindable from a key in 1.0." },
    Binding { name: "transport.paused", since: V1_0, doc: "Whether playback is paused; the same name is a condition. Consumed by the transport; not bindable from a key in 1.0." },
    Binding { name: "playbar.pos", since: V1_0, doc: "The playhead position. Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "playbar.dur", since: V1_0, doc: "The duration. Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "playbar.live", since: V1_0, doc: "Whether the item is live. Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "playbar.seek_display", since: V1_0, doc: "The seek read-out a scrub shows. Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "scrub.live", since: V1_0, doc: "Whether a scrub is in progress. Consumed by the chrome; not bindable from a key in 1.0." },
    Binding { name: "scrub.pos", since: V1_0, doc: "The scrub target position. Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "playbar.busy", since: V1_0, doc: "Whether the pipeline is busy (the spinner). Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "playbar.mark", since: V1_0, doc: "The transport mark beside the clock. Consumed by the `playbar`; not bindable from a key in 1.0." },
    Binding { name: "chrome.alpha", since: V1_0, doc: "The chrome's fade alpha while a scrub is live. Consumed by a band's `chrome`; not bindable from a key in 1.0." },
    Binding { name: "chapters.present", since: V1_0, doc: "Whether the item has chapters; the same name is a condition. Not bindable from a key in 1.0." },
    Binding { name: "slot.kind", since: V1_0, doc: "Which control slot is showing: the discs, the Up Next offer or the Skip pill. The `slot.*` conditions read it; not bindable from a key in 1.0." },
    Binding { name: "focus.id", since: V1_0, doc: "The focused control's `id`. App-internal; not bindable from a key in 1.0." },
    Binding { name: "stats.on", since: V1_0, doc: "Whether Stats for nerds is on; selects a `toggle_stats` face. Not bindable from a key in 1.0." },
    Binding { name: "seek.interval", since: V1_0, doc: "The viewer's Skip interval; selects a `by_interval` face. Not bindable from a key in 1.0." },
    Binding { name: "chapters", since: V1_0, doc: "The chapter list a `strip` shows." },
    Binding { name: "item.thumb", since: V1_0, doc: "In a strip card: the chapter's thumbnail." },
    Binding { name: "item.start", since: V1_0, doc: "In a strip card: the chapter's start time, formatted as a clock (app text)." },
    Binding { name: "item.title", since: V1_0, doc: "In a strip card: the chapter's name (server text, may elide); when it is empty, the app's own numbered fallback, which is app text and is drawn whole." },
    Binding { name: "item.is_current", since: V1_0, doc: "In a strip card: whether the card is the playing chapter; the same name is a condition." },
    Binding { name: "item.focused", since: V1_0, doc: "In a strip card: whether the card is focused; the same name is a condition." },
    Binding { name: "info.still", since: V1_0, doc: "The playing item's still image." },
    Binding { name: "info.title", since: V1_0, doc: "The info section's title (server text)." },
    Binding { name: "info.summary", since: V1_0, doc: "The playing item's synopsis (server text)." },
    Binding { name: "info.facts", since: V1_0, doc: "The playing item's facts run, fitted by a `facts` cell." },
    Binding { name: "label.chapters", since: V1_0, doc: "The app's Chapters label (app text, localized)." },
    Binding { name: "label.info", since: V1_0, doc: "The app's Info label (app text, localized)." },
];

/// What a `text` cell's `bind` may name.
pub const TEXT_BINDS: &[&str] = &[
    "title.kicker",
    "title.main",
    "info.title",
    "info.summary",
    "label.chapters",
    "label.info",
    "item.start",
    "item.title",
];
/// What an `image` cell's `bind` may name.
pub const IMAGE_BINDS: &[&str] = &["info.still"];
/// What a `strip`'s `bind` may name.
pub const STRIP_BINDS: &[&str] = &["chapters"];
/// What a `facts` cell's `bind` may name.
pub const FACTS_BINDS: &[&str] = &["info.facts"];
/// What a strip card's `art.bind` may name.
pub const CARD_ART_BINDS: &[&str] = &["item.thumb"];

/// The binding called `name`.
pub fn binding(name: &str) -> Option<&'static Binding> {
    let bindings: &'static [Binding] = BINDINGS;
    bindings.iter().find(|b| b.name == name)
}

// ---------------------------------------------------------------------------------------------------
// Nodes: the package, `layout` and everything under it
// ---------------------------------------------------------------------------------------------------

/// A width on the 1920-wide canvas.
const PX_W: Ty = Ty::Int { min: 1, max: 1920 };
/// A height on the 1080-tall canvas.
const PX_H: Ty = Ty::Int { min: 1, max: 1080 };
/// A width in px, or `fill`: the band's free width (spec, "Fill").
const FILL_OR_PX: Ty = Ty::Either(&PX_W, &Ty::Enum(&["fill"]));
/// The sizes a cell may name when `HUD_TITLE` is not legal: [`SIZE_RUNGS`] but for `HUD_TITLE`. A test
/// ties it to that table. `HUD_TITLE` is legal only on a text cell bound to `title.main`, so `text.size`
/// is a [`Ty::Size`] guarded by a [`Rule::NameOnlyWith`], and every other size key is this list.
pub const BODY_SIZES: &[&str] = &[
    "HERO", "DISPLAY", "TITLE", "HEADLINE", "BODY", "LABEL", "CAPTION", "MICRO",
];
/// The three alignments of a cell in its band, and of the Up Next column.
const ALIGNS: Ty = Ty::Enum(&["start", "center", "end"]);
/// The two focus faces and the four rest faces of a control (the item node and an Up Next button).
const RESTS: Ty = Ty::Enum(&["film", "bare", "solid", "custom"]);
const FOCI: Ty = Ty::Enum(&["accent", "ring"]);

const F_WHEN: Field = Field::opt(
    "when",
    Ty::Cond,
    "Present only while the condition(s) hold: a condition name, or an array that must all hold, each optionally prefixed `!`. An absent element collapses as an absent control does.",
);
const F_GAP_BEFORE: Field = Field::opt(
    "gap_before",
    Ty::Gap,
    "The gap to the previous PRESENT neighbour, in place of the stack's own: a spacing token or px.",
)
.keyable();
const F_GAP_AFTER: Field = Field::opt(
    "gap_after",
    Ty::Gap,
    "The gap to the next PRESENT neighbour, in place of the stack's own: a spacing token or px.",
)
.keyable();
const F_REST: Field = Field::opt(
    "rest",
    RESTS,
    "The resting face: `film` today's, `bare` the glyph only, `solid` the focused colours at rest, `custom` (1.0 defines no ink keys for it: the compiler decides).",
)
.default("\"film\"");
const F_FOCUS: Field = Field::opt(
    "focus",
    FOCI,
    "The focus face: `accent` today's, `ring` the rest face plus the white focus ring. `solid` rest with `accent` focus draws one face twice and is rejected.",
)
.default("\"accent\"");

/// The pair of rules a `rest` and `focus` key share: a `solid` rest needs a stated focus (the default
/// `accent` would draw one face twice), and the pair `solid` with `accent` is refused.
const REST_FOCUS_RULES: &[Rule] = &[
    Rule::IfValue {
        key: "rest",
        equals: "solid",
        require: &["focus"],
        forbid: &[],
    },
    Rule::ForbidPair {
        a: "rest",
        a_value: "solid",
        b: "focus",
        b_value: "accent",
    },
];

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
        Field::req("id", Ty::Pattern(Pattern::ReverseDns), "The package's reverse-DNS identity (the built-in HUD's reserved `builtin:classic` is the one other id that parses)."),
        Field::req("version", Ty::Pattern(Pattern::Semver), "The package's own semver; it only orders updates of one id."),
        Field::req("name", Ty::Text { max_len: 64 }, "The display name."),
        Field::req("author", Ty::Text { max_len: 64 }, "The author."),
        Field::req("license", Ty::Text { max_len: 64 }, "An SPDX license identifier."),
        Field::opt("app", Ty::Object("app"), "The app releases this package renders correctly on."),
        Field::opt("palette", Ty::Palette, "Named colours: lower-case name -> \"#rrggbb\". Every entry must be used."),
        Field::opt("assets", Ty::Assets, "Inline SVG masks, by id."),
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

/// `layout`: where the HUD sits and the bands it holds.
pub static LAYOUT: Node = Node {
    name: "layout",
    kind: NodeKind::Plain,
    fields: &[
        Field::req(
            "anchor",
            Ty::Enum(&["bottom"]),
            "Where the HUD is anchored. Only `bottom` in 1.0 (D13): a top HUD needs a caption keep-out the media layer lacks.",
        ),
        Field::req(
            "inset",
            Ty::Gap,
            "The block's distance above the bottom edge of the safe area: a spacing token or px.",
        )
        .keyable(),
        Field::req(
            "x_inset",
            Ty::Gap,
            "The block's distance from the safe area's sides: a spacing token or px.",
        )
        .keyable(),
        Field::req(
            "band",
            Ty::Enum(&["controls", "block"]),
            "What lifted captions keep clear of (D8): the control row only, or the whole compact block.",
        ),
        Field::req(
            "sections",
            Ty::Enum(&["modal", "inline"]),
            "How the chapters and info sections open (D6): `modal` one at a time, hiding the row; `inline` any subset beside the row.",
        ),
        Field::opt(
            "scrim",
            Ty::Object("scrim"),
            "The darkening behind the HUD. Absent: none.",
        ),
        Field::req(
            "block",
            Ty::List { of: &Ty::Object("band"), max_len: 16 },
            "The bands, top to bottom, in one bottom-anchored stack.",
        ),
        Field::req(
            "up_next",
            Ty::Object("up_next"),
            "How the Up Next offer and the Skip pill replace segments of the row (R4: both buttons are placed here).",
        ),
    ],
    rules: &[],
    doc: "Where the HUD sits and what it holds.",
};

/// `layout.scrim`: a tagged union on `cover`.
pub static SCRIM: Node = Node {
    name: "scrim",
    kind: NodeKind::Plain,
    fields: &[
        Field::req(
            "cover",
            Ty::Enum(&["block", "fixed"]),
            "`block`: the scrim's top follows the compiled block top per state, `extra` px above it. `fixed`: one `height` from the screen bottom in every state, drawn before any band.",
        ),
        Field::opt(
            "extra",
            Ty::Int { min: 0, max: SCRIM_H as i64 },
            "With `cover: \"block\"`: px of scrim above the block's top. At most classic's whole scrim height.",
        )
        .keyable(),
        Field::opt(
            "height",
            Ty::Int { min: 0, max: SCRIM_H as i64 },
            "With `cover: \"fixed\"`: the scrim's height from the screen bottom. At most classic's whole scrim height.",
        )
        .keyable(),
    ],
    rules: &[
        Rule::IfValue {
            key: "cover",
            equals: "block",
            require: &["extra"],
            forbid: &["height"],
        },
        Rule::IfValue {
            key: "cover",
            equals: "fixed",
            require: &["height"],
            forbid: &["extra"],
        },
    ],
    doc: "The scrim: `{\"cover\": \"block\", \"extra\": px}` or `{\"cover\": \"fixed\", \"height\": px}`.",
};

/// A band of `layout.block`: one row of the bottom-anchored stack.
pub static BAND: Node = Node {
    name: "band",
    kind: NodeKind::Plain,
    fields: &[
        Field::req(
            "id",
            Ty::Pattern(Pattern::Id),
            "The band's name, unique in the package: `up_next.band` and lint findings name it.",
        ),
        F_WHEN,
        F_GAP_BEFORE,
        Field::opt(
            "gap",
            Ty::Gap,
            "The gap between the band's packed cells: a spacing token or px.",
        )
        .keyable(),
        Field::opt(
            "z",
            Ty::Int { min: 0, max: 16 },
            "Draw and hit-test order among bands: a higher band is above a lower one.",
        ),
        Field::opt(
            "chrome",
            Ty::Enum(&["fades", "steady"]),
            "While a scrub is live a `fades` band draws on the chrome alpha and a `steady` one at full alpha. The band holding the `playbar` must be `steady`.",
        )
        .default("\"fades\""),
        Field::req(
            "cells",
            Ty::List { of: &Ty::Object("cell"), max_len: 32 },
            "The band's cells; cells of one alignment pack left to right in array order.",
        ),
    ],
    rules: &[],
    doc: "One band of the HUD block.",
};

/// A cell: its position keys plus exactly one content key, each naming that kind's node.
pub static CELL: Node = Node {
    name: "cell",
    kind: NodeKind::OneOf(&[
        "text", "image", "icon", "rule", "rect", "facts", "vstack", "spacer", "playbar", "segment",
        "strip", "actions",
    ]),
    fields: &[
        Field::opt(
            "align",
            ALIGNS,
            "Which edge the cell packs against in its band (a strip card's cells omit it).",
        )
        .default("\"start\""),
        Field::opt(
            "valign",
            Ty::Enum(&["top", "center", "bottom"]),
            "Where the cell stands against the band's height. Absent: the compiler's choice.",
        ),
        F_WHEN,
        F_GAP_BEFORE,
        F_GAP_AFTER,
        Field::opt("text", Ty::Object("text"), "A line of bound text."),
        Field::opt("image", Ty::Object("image"), "A bound image through the image cache."),
        Field::opt("icon", Ty::Object("icon"), "An asset glyph tinted by an ink."),
        Field::opt("rule", Ty::Object("rule"), "A hairline."),
        Field::opt("rect", Ty::Object("rect"), "A filled panel holding cells."),
        Field::opt("facts", Ty::Object("facts"), "The facts run: duration, year, genres, rating and badges, fitted as chips."),
        Field::opt("vstack", Ty::Object("vstack"), "A vertical stack of cells."),
        Field::opt("spacer", Ty::Object("spacer"), "Empty width."),
        Field::opt("playbar", Ty::Object("playbar"), "The scrub bar with its clocks and knob: exactly one per package."),
        Field::opt("segment", Ty::Object("segment"), "One row of controls: a focus group."),
        Field::opt("strip", Ty::Object("strip"), "The chapters strip."),
        Field::opt("actions", Ty::Object("actions"), "Controls inside a section, in a row or a column."),
    ],
    rules: &[],
    doc: "A cell of a band (or of a vstack, a rect or a strip card): its position keys and exactly one of the twelve cell kinds. The scrub group, segments, the strip and actions are focus stops, so the compiler refuses them inside a strip card.",
};

pub static CELL_TEXT: Node = Node {
    name: "text",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("bind", Ty::Bind(TEXT_BINDS), "What the text shows."),
        Field::req(
            "size",
            Ty::Size,
            "A type-scale rung name. `HUD_TITLE` (54 px) is legal only on a text bound to `title.main`.",
        ),
        Field::req(
            "ink",
            Ty::Ink,
            "A theme token (upper case) or a palette name (lower case).",
        )
        .keyable(),
        Field::opt("alpha", Ty::Num { min: 0.0, max: 1.0 }, "Opacity of the ink.")
            .keyable()
            .default("1.0"),
        Field::opt("bold", Ty::Bool, "The bold weight; the app ships two weights.")
            .keyable()
            .default("false"),
        Field::opt(
            "leading",
            Ty::Int { min: 1, max: 400 },
            "The line pitch in px, and a one-line text's box height. Absent: the rung's line height.",
        )
        .keyable(),
        Field::opt(
            "w",
            FILL_OR_PX,
            "The text's width in px, or `fill`. Server text only (D10): app text hugs its measured width.",
        ),
        Field::opt(
            "max_lines",
            Ty::Int { min: 1, max: 8 },
            "1 elides; more wraps. Server text only (D10).",
        )
        .keyable(),
    ],
    rules: &[
        Rule::NameOnlyWith {
            key: "size",
            name: "HUD_TITLE",
            other: "bind",
            other_value: "title.main",
        },
        // D10: app-owned text never elides, so it takes no `w` and no `max_lines`.
        Rule::IfValue {
            key: "bind",
            equals: "label.chapters",
            require: &[],
            forbid: &["w", "max_lines"],
        },
        Rule::IfValue {
            key: "bind",
            equals: "label.info",
            require: &[],
            forbid: &["w", "max_lines"],
        },
        Rule::IfValue {
            key: "bind",
            equals: "item.start",
            require: &[],
            forbid: &["w", "max_lines"],
        },
    ],
    doc: "A line of bound text. App-owned text (`label.*`, `item.start`) takes no `w` and no `max_lines`.",
};

pub static CELL_IMAGE: Node = Node {
    name: "image",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("bind", Ty::Bind(IMAGE_BINDS), "Which image."),
        Field::req("w", PX_W, "Width in px.").keyable(),
        Field::req("h", PX_H, "Height in px.").keyable(),
        Field::opt(
            "radius",
            Ty::Int { min: 0, max: 540 },
            "Corner radius in px.",
        )
        .keyable(),
        Field::opt("crop", Ty::Enum(&["cover"]), "How the image fills its box.")
            .default("\"cover\""),
    ],
    rules: &[],
    doc: "A bound image through the image cache.",
};

pub static CELL_ICON: Node = Node {
    name: "icon",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("asset", Ty::Asset, "An id declared in `assets`."),
        Field::req("size", Ty::Int { min: 1, max: 512 }, "Side in px.").keyable(),
        Field::req(
            "ink",
            Ty::Ink,
            "The mask's tint: a theme token or a palette name.",
        )
        .keyable(),
    ],
    rules: &[],
    doc: "An asset glyph: a mask tinted by one ink.",
};

pub static CELL_RULE: Node = Node {
    name: "rule",
    kind: NodeKind::Plain,
    fields: &[
        Field::req(
            "axis",
            Ty::Enum(&["h", "v"]),
            "`h` horizontal, `v` vertical.",
        ),
        Field::req("length", FILL_OR_PX, "Length in px, or `fill`."),
        Field::req("ink", Ty::Ink, "A theme token or a palette name.").keyable(),
    ],
    rules: &[],
    doc: "A hairline.",
};

pub static CELL_RECT: Node = Node {
    name: "rect",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("ink", Ty::Ink, "The panel's fill: a theme token or a palette name.").keyable(),
        Field::opt("radius", Ty::Int { min: 0, max: 540 }, "Corner radius in px.")
            .keyable()
            .default("0"),
        Field::opt(
            "pad",
            Ty::Int { min: 0, max: 540 },
            "Px between the ground's edge and its content box, which must lie inside the safe area. A plain number: `bleed` is compared with it.",
        )
        .default("0"),
        Field::opt(
            "bleed",
            Ty::Int { min: 0, max: 540 },
            "Px the ground extends past the safe area, never past the screen. At most `pad`; needs `pad`.",
        )
        .default("0"),
        Field::opt("gap", Ty::Gap, "The gap between the panel's cells: a spacing token or px.")
            .keyable()
            .default("0"),
        Field::opt(
            "h",
            PX_H,
            "A fixed panel height in px, its cells centred in it.",
        )
        .keyable(),
        Field::req(
            "cells",
            Ty::List { of: &Ty::Object("cell"), max_len: 32 },
            "The panel's cells.",
        ),
    ],
    rules: &[
        Rule::Requires { key: "bleed", other: "pad" },
        Rule::AtMost { key: "bleed", other: "pad" },
    ],
    doc: "A filled panel. Its content box is the ground inset by `pad` and lies inside the safe area; the ground may bleed past it.",
};

pub static CELL_FACTS: Node = Node {
    name: "facts",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("bind", Ty::Bind(FACTS_BINDS), "The facts run."),
        Field::req(
            "order",
            Ty::EnumList(&["duration", "year", "genres", "content_rating"]),
            "The plain facts, in order: an ordered subset.",
        ),
        Field::opt(
            "genres_max",
            Ty::Int { min: 1, max: 16 },
            "How many genres the run shows at most.",
        )
        .keyable(),
        Field::opt("bold", Ty::Bool, "The bold weight.").keyable().default("false"),
        Field::req("ink", Ty::Ink, "A theme token or a palette name.").keyable(),
        Field::opt(
            "playback_fact",
            Ty::Bool,
            "Whether the playback fact is drawn, regular in `TEXT_TERTIARY`.",
        )
        .default("false"),
        Field::opt(
            "badges",
            Ty::EnumList(&["content_rating", "audio_format", "cc", "sdh", "ad"]),
            "The badge chips, in order: an ordered subset. `content_rating` is in `order` or `badges`, never both.",
        )
        .default("[]"),
        Field::req("size", Ty::Enum(BODY_SIZES), "A type-scale rung name."),
    ],
    rules: &[Rule::Disjoint { a: "order", b: "badges" }],
    doc: "The facts run. All of it is fitted as chips that drop whole chips tail first, so it never elides.",
};

pub static CELL_VSTACK: Node = Node {
    name: "vstack",
    kind: NodeKind::Plain,
    fields: &[
        Field::opt(
            "gap",
            Ty::Gap,
            "The gap between the cells: a spacing token or px.",
        )
        .keyable(),
        Field::opt(
            "w",
            FILL_OR_PX,
            "Width in px, or `fill`. Absent: the widest cell.",
        ),
        Field::req(
            "cells",
            Ty::List {
                of: &Ty::Object("cell"),
                max_len: 32,
            },
            "The stacked cells.",
        ),
    ],
    rules: &[],
    doc: "A vertical stack of cells.",
};

pub static CELL_SPACER: Node = Node {
    name: "spacer",
    kind: NodeKind::Plain,
    fields: &[Field::req("w", PX_W, "Width in px.").keyable()],
    rules: &[],
    doc: "Empty width.",
};

/// The most controls one `segment` holds. The player's row is a segment, and
/// [`crate::player_hud::geometry::MAX_ROW_ITEMS`], the width of the geometry report's `items`, is this.
pub const MAX_SEGMENT_ITEMS: usize = 24;

pub static CELL_PLAYBAR: Node = Node {
    name: "playbar",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("w", FILL_OR_PX, "The bar's width in px, or `fill`."),
        Field::req("fill", Ty::Ink, "The elapsed part's ink: a theme token or a palette name.").keyable(),
        Field::req("track", Ty::Ink, "The track's ink: a theme token or a palette name.").keyable(),
        Field::req(
            "thickness",
            Ty::Int { min: 1, max: 64 },
            "The track's height in px. The bar's box is the track and the line the clocks are drawn on under it, or this if taller; the knob and the mark overhang the track.",
        )
        .keyable(),
        Field::opt(
            "knob",
            Ty::Enum(&["tick", "always"]),
            "`tick` classic's knob, `always` a permanent one.",
        )
        .default("\"tick\""),
        Field::opt(
            "clocks",
            Ty::Enum(&["follow_head", "ends"]),
            "`follow_head` classic's clocks that follow the playhead, `ends` clocks at the bar's two ends.",
        )
        .default("\"follow_head\""),
        Field::req("clock_size", Ty::Enum(BODY_SIZES), "The clocks' type-scale rung."),
        Field::opt(
            "mark",
            Ty::Enum(&["after_elapsed", "busy_only"]),
            "The transport mark: `after_elapsed` beside the elapsed clock, `busy_only` the spinner alone (the paused glyph may go when a `toggle_play` control is placed).",
        )
        .default("\"after_elapsed\""),
        Field::opt("grab", Ty::Object("grab"), "The pointer grab band around the track."),
    ],
    rules: &[],
    doc: "The scrub bar. Exactly one per package: the scrub group.",
};

pub static GRAB: Node = Node {
    name: "grab",
    kind: NodeKind::Plain,
    fields: &[
        Field::opt(
            "above",
            Ty::Int { min: 0, max: 540 },
            "Px of grab area above the track's top edge.",
        )
        .keyable()
        .default("72"),
        Field::opt(
            "below",
            Ty::Int { min: 0, max: 540 },
            "Px of grab area below the track's bottom edge.",
        )
        .keyable()
        .default("80"),
    ],
    rules: &[],
    doc: "The playbar's pointer grab band, registered as the lowest stop.",
};

pub static CELL_SEGMENT: Node = Node {
    name: "segment",
    kind: NodeKind::Plain,
    fields: &[
        Field::req(
            "id",
            Ty::Pattern(Pattern::Id),
            "The segment's name, unique in the package: `up_next.replaces` names it.",
        ),
        Field::opt(
            "gap",
            Ty::Gap,
            "The gap between the controls: a spacing token or px.",
        )
        .keyable(),
        Field::req(
            "items",
            Ty::List {
                of: &Ty::Object("item"),
                max_len: MAX_SEGMENT_ITEMS,
            },
            "The controls, in order.",
        ),
    ],
    rules: &[],
    doc: "One row of controls. The segments of one band form one focus group.",
};

pub static CELL_STRIP: Node = Node {
    name: "strip",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("bind", Ty::Bind(STRIP_BINDS), "The chapter list."),
        Field::req("w", FILL_OR_PX, "The strip's width in px, or `fill`."),
        Field::req("card_w", PX_W, "A card's width in px.").keyable(),
        Field::req("card_h", PX_H, "A card's height in px.").keyable(),
        Field::opt(
            "gap",
            Ty::Gap,
            "The gap between cards: a spacing token or px.",
        )
        .keyable(),
        Field::opt(
            "radius",
            Ty::Int { min: 0, max: 540 },
            "A card's corner radius in px.",
        )
        .keyable(),
        Field::opt("focus", Ty::Enum(&["ring"]), "The focus face of a card.").default("\"ring\""),
        Field::opt(
            "label_align",
            Ty::Enum(&["centre", "leading"]),
            "How a card's labels align: `centre` the shelf's default, or `leading`.",
        )
        .default("\"centre\""),
        Field::req("card", Ty::Object("card"), "What a card holds."),
    ],
    rules: &[],
    doc: "The chapters strip: a shelf of cards. Scrolling and seating are app-owned.",
};

pub static CARD: Node = Node {
    name: "card",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("art", Ty::Object("card_art"), "The card's picture."),
        Field::opt(
            "overlay",
            Ty::List { of: &Ty::Object("cell"), max_len: 8 },
            "Cells drawn over the art, positioned by their `align` and `valign`.",
        ),
        Field::opt(
            "below",
            Ty::List { of: &Ty::Object("cell"), max_len: 8 },
            "Cells stacked under the art.",
        ),
        Field::opt("below_gap", Ty::Gap, "The gap between the art and the cells below: a spacing token or px.").keyable(),
    ],
    rules: &[],
    doc: "A strip card: its art, what is drawn over it and what stands under it. `item.*` bindings and conditions are legal only inside it.",
};

pub static CARD_ART: Node = Node {
    name: "card_art",
    kind: NodeKind::Plain,
    fields: &[
        Field::req("bind", Ty::Bind(CARD_ART_BINDS), "Which image."),
        Field::opt(
            "crop",
            Ty::Enum(&["cover"]),
            "How the image fills the card.",
        )
        .default("\"cover\""),
    ],
    rules: &[],
    doc: "A strip card's art.",
};

pub static CELL_ACTIONS: Node = Node {
    name: "actions",
    kind: NodeKind::Plain,
    fields: &[
        Field::opt("axis", Ty::Enum(&["row", "column"]), "The direction the controls run.").default("\"row\""),
        Field::opt("gap", Ty::Gap, "The gap between the controls: a spacing token or px.").keyable(),
        Field::req("item_h", Ty::Int { min: 1, max: 400 }, "A control's height in px.").keyable(),
        Field::opt(
            "min_w",
            Ty::Int { min: 0, max: 1920 },
            "The floor of the one width every `button` takes: the widest label over every variant, at least this.",
        )
        .keyable()
        .default("0"),
        Field::req(
            "items",
            Ty::List { of: &Ty::Object("item"), max_len: 12 },
            "The controls, shaped as a segment's.",
        ),
    ],
    rules: &[],
    doc: "Controls inside a section, in a row or a column.",
};

/// A control item: `command` selects a [`Cmd`] whose typed parameters are sibling keys.
pub static ITEM: Node = Node {
    name: "item",
    kind: NodeKind::CommandDispatched,
    fields: &[
        Field::req(
            "id",
            Ty::Pattern(Pattern::Id),
            "The control's identity, unique in the package: focus and the pointer stops key on it.",
        ),
        Field::req("command", Ty::Command, "The app-owned command the control fires (D4)."),
        Field::req(
            "widget",
            Ty::Enum(&["disc", "pill", "button"]),
            "`disc` a round transport button, `pill` a tab pill labelled by the command's app string, `button` a labelled button. No default: each reads differently.",
        ),
        Field::req(
            "size",
            Ty::Int { min: 1, max: 480 },
            "The control's size in px: a plain number, never state-keyed. Its floor, `MIN_CONTROL_SIZE`, is the compiler's lint, not a parse bound.",
        ),
        F_REST,
        F_FOCUS,
        Field::opt(
            "face",
            Ty::Face,
            "The glyph: an asset id, `builtin:<icon>`, or a per-state map for a command that has states. Absent: the command's classic glyph.",
        ),
        F_WHEN,
        F_GAP_BEFORE,
        F_GAP_AFTER,
    ],
    rules: REST_FOCUS_RULES,
    doc: "A control: an `id`, a `command` with its parameters as sibling keys, and how it looks. The legal sibling keys are those of the named command (`Cmd::params`).",
};

/// `layout.up_next`: how the offer replaces row segments.
pub static UP_NEXT: Node = Node {
    name: "up_next",
    kind: NodeKind::Plain,
    fields: &[
        Field::req(
            "replaces",
            Ty::List { of: &Ty::Pattern(Pattern::Id), max_len: 16 },
            "The segment ids that leave the frame and the focus map while the offer or the Skip pill shows.",
        ),
        Field::req("band", Ty::Pattern(Pattern::Id), "The id of the band the offer is drawn in."),
        Field::req(
            "align",
            ALIGNS,
            "Where the offer's column stands against the replaced segments: `end` at their end edge, `center` on their centre, `start` on their start edge.",
        ),
        Field::req("still", Ty::Bool, "Whether the offer draws the next episode's still."),
        Field::req("caption", Ty::Bool, "Whether the offer draws its caption."),
        Field::req("watch_credits", Ty::Object("up_next_button"), "The Watch Credits button's looks."),
        Field::req("play_next", Ty::Object("up_next_button"), "The Play Next button's looks."),
        Field::req(
            "countdown",
            Ty::Enum(&["fill_sweep", "ring", "bar", "numeric"]),
            "How the auto-advance countdown shows on or in the Play Next button; never absent while running.",
        ),
    ],
    rules: &[],
    doc: "The Up Next offer. Its inside is app-owned; a package chooses what it replaces, where it stands and how its two buttons look.",
};

/// One of Up Next's two fixed buttons: `watch_credits` and `play_next`.
pub static UP_NEXT_BUTTON: Node = Node {
    name: "up_next_button",
    kind: NodeKind::Plain,
    fields: &[F_REST, F_FOCUS],
    rules: REST_FOCUS_RULES,
    doc: "An Up Next button's looks. Its command is fixed by the app.",
};

/// Every node of the table, [`ROOT`] first. `node(name)` and `all_nodes()` read it.
static NODES: &[&Node] = &[
    &ROOT,
    &APP,
    &LAYOUT,
    &SCRIM,
    &BAND,
    &CELL,
    &CELL_TEXT,
    &CELL_IMAGE,
    &CELL_ICON,
    &CELL_RULE,
    &CELL_RECT,
    &CELL_FACTS,
    &CELL_VSTACK,
    &CELL_SPACER,
    &CELL_PLAYBAR,
    &GRAB,
    &CELL_SEGMENT,
    &CELL_STRIP,
    &CARD,
    &CARD_ART,
    &CELL_ACTIONS,
    &ITEM,
    &UP_NEXT,
    &UP_NEXT_BUTTON,
];

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

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::rec::strict_json::{decode_ordered, Ordered};
    use std::collections::{BTreeSet, HashSet};

    /// Every type reachable through `ty`: itself, a list's element, both sides of an `Either`.
    fn walk_ty<'a>(ty: &'a Ty, f: &mut impl FnMut(&'a Ty)) {
        f(ty);
        match ty {
            Ty::List { of, .. } => walk_ty(of, f),
            Ty::Either(a, b) => {
                walk_ty(a, f);
                walk_ty(b, f);
            }
            _ => {}
        }
    }

    /// The node names `ty` refers to.
    fn objects_of(ty: &Ty) -> Vec<&'static str> {
        let mut out = Vec::new();
        walk_ty(ty, &mut |t| {
            if let Ty::Object(n) = t {
                out.push(*n);
            }
        });
        out
    }

    /// The names an enum-like type allows, or `None` for a type with no closed name list.
    fn names_of(ty: &Ty) -> Option<&'static [&'static str]> {
        match ty {
            Ty::Enum(n) | Ty::EnumList(n) | Ty::Bind(n) => Some(*n),
            _ => None,
        }
    }

    fn every_field() -> Vec<(&'static str, &'static Field)> {
        let mut out = Vec::new();
        for n in all_nodes() {
            for f in n.fields {
                out.push((n.name, f));
            }
        }
        for c in COMMANDS {
            for f in c.params {
                out.push((c.name, f));
            }
        }
        out
    }

    /// Whether the JSON text `v` is a valid value of `ty`, for the types a default can have.
    fn fits(ty: &Ty, v: &Ordered) -> bool {
        match ty {
            Ty::Bool => v.as_bool().is_some(),
            Ty::Int { min, max } => v.as_i64().is_some_and(|i| (*min..=*max).contains(&i)),
            Ty::Num { min, max } => v.as_f64().is_some_and(|x| *min <= x && x <= *max),
            Ty::Text { max_len } => v.as_str().is_some_and(|s| s.chars().count() <= *max_len),
            Ty::Enum(names) => v.as_str().is_some_and(|s| names.contains(&s)),
            Ty::EnumList(names) => v.as_array().is_some_and(|a| {
                let mut seen = HashSet::new();
                a.iter().all(|e| {
                    e.as_str()
                        .is_some_and(|s| names.contains(&s) && seen.insert(s))
                })
            }),
            Ty::Gap => {
                v.as_str().is_some_and(|s| space_token(s).is_some())
                    || v.as_i64().is_some_and(|i| (0..=1080).contains(&i))
            }
            Ty::Ink => v.as_str().is_some_and(|s| {
                ink_token(s).is_some() || s.chars().all(|c| c.is_ascii_lowercase())
            }),
            Ty::Either(a, b) => fits(a, v) || fits(b, v),
            Ty::List { of, max_len } => v
                .as_array()
                .is_some_and(|a| a.len() <= *max_len && a.iter().all(|e| fits(of, e))),
            // No other type carries a documented default.
            _ => false,
        }
    }

    #[test]
    fn hud_format_schema_every_object_reference_resolves_and_all_nodes_is_the_reachable_set() {
        let mut reached: BTreeSet<&str> = BTreeSet::new();
        let mut todo = vec![ROOT.name];
        while let Some(name) = todo.pop() {
            if !reached.insert(name) {
                continue;
            }
            let n = node(name).unwrap_or_else(|| panic!("node `{name}` is referenced but missing"));
            for f in n.fields {
                todo.extend(objects_of(&f.ty));
            }
        }
        let listed: Vec<&str> = all_nodes().iter().map(|n| n.name).collect();
        let listed_set: BTreeSet<&str> = listed.iter().copied().collect();
        assert_eq!(
            listed.len(),
            listed_set.len(),
            "a node is listed twice: {listed:?}"
        );
        assert_eq!(
            reached, listed_set,
            "all_nodes() is exactly what ROOT reaches"
        );
        for c in COMMANDS {
            for p in c.params {
                assert!(
                    objects_of(&p.ty).is_empty(),
                    "a command parameter is a scalar: {}",
                    p.key
                );
            }
        }
        assert_eq!(all_nodes().len(), 24);
    }

    #[test]
    fn hud_format_schema_one_of_keys_are_object_fields_and_the_cell_has_the_twelve_kinds() {
        for n in all_nodes() {
            if let NodeKind::OneOf(keys) = n.kind {
                let mut seen = HashSet::new();
                for k in keys {
                    assert!(seen.insert(*k), "{}: duplicate content key {k}", n.name);
                    let f =
                        field(n, k).unwrap_or_else(|| panic!("{}: `{k}` is not a field", n.name));
                    assert!(
                        !f.required,
                        "{}.{k}: exactly-one is the OneOf's job, not `required`",
                        n.name
                    );
                    assert!(
                        matches!(f.ty, Ty::Object(t) if node(t).is_some_and(|x| x.name == *k)),
                        "{}.{k} must be an Object naming the node `{k}`",
                        n.name
                    );
                }
            }
        }
        assert_eq!(
            CELL.kind,
            NodeKind::OneOf(&[
                "text", "image", "icon", "rule", "rect", "facts", "vstack", "spacer", "playbar",
                "segment", "strip", "actions"
            ])
        );
        assert_eq!(ITEM.kind, NodeKind::CommandDispatched);
        assert!(all_nodes()
            .iter()
            .filter(|n| n.name != "cell" && n.name != "item")
            .all(|n| n.kind == NodeKind::Plain));
    }

    #[test]
    fn hud_format_schema_keys_are_unique_in_every_node_and_command_and_never_collide() {
        for n in all_nodes() {
            let keys: HashSet<_> = n.fields.iter().map(|f| f.key).collect();
            assert_eq!(keys.len(), n.fields.len(), "keys of {} are unique", n.name);
            assert!(!n.doc.is_empty(), "{} has a doc", n.name);
            assert!(
                n.fields.iter().all(|f| !f.doc.is_empty()),
                "{}: every field has a doc",
                n.name
            );
        }
        for c in COMMANDS {
            let keys: HashSet<_> = c.params.iter().map(|f| f.key).collect();
            assert_eq!(
                keys.len(),
                c.params.len(),
                "parameters of {} are unique",
                c.name
            );
            // A control's keys are its node's fields PLUS the command's parameters: no overlap.
            for p in c.params {
                assert!(
                    field(&ITEM, p.key).is_none(),
                    "{}.{} collides with a control item key",
                    c.name,
                    p.key
                );
                assert!(
                    !p.required || !p.keyable,
                    "a command parameter is never state-keyed"
                );
            }
        }
    }

    #[test]
    fn hud_format_schema_enums_are_non_empty_unique_and_lower_snake_or_token_shaped() {
        for (owner, f) in every_field() {
            let mut tys = Vec::new();
            walk_ty(&f.ty, &mut |t| tys.push(t));
            for t in tys {
                if let Some(names) = names_of(t) {
                    assert!(!names.is_empty(), "{owner}.{}: an empty name list", f.key);
                    let set: HashSet<_> = names.iter().collect();
                    assert_eq!(set.len(), names.len(), "{owner}.{}: duplicate names", f.key);
                }
            }
        }
        for (owner, f) in every_field() {
            if let Ty::Enum(names) = f.ty {
                for name in names {
                    assert!(
                        !name.is_empty() && !name.contains(char::is_whitespace),
                        "{owner}.{}: `{name}` is not a name",
                        f.key
                    );
                }
            }
        }
    }

    #[test]
    fn hud_format_schema_every_since_is_supported_by_this_build() {
        let max_minor = SUPPORTED
            .iter()
            .find(|(major, _)| *major == CURRENT.0)
            .map(|(_, minor)| *minor);
        assert_eq!(max_minor, Some(0), "SUPPORTED is {{1: 0..=0}}");
        assert_eq!(SUPPORTED, &[(1, 0)]);
        for (owner, f) in every_field() {
            assert!(
                f.since <= CURRENT,
                "{owner}.{} is newer than this build",
                f.key
            );
        }
        assert!(COMMANDS.iter().all(|c| c.since <= CURRENT));
        assert!(CONDITIONS.iter().all(|c| c.since <= CURRENT));
        assert!(BINDINGS.iter().all(|b| b.since <= CURRENT));
        assert!(
            every_field().iter().all(|(_, f)| f.since == V1_0),
            "format 1.0 has only 1.0 entries"
        );
    }

    #[test]
    fn hud_format_schema_every_default_is_a_valid_value_of_its_type() {
        let mut defaults = 0;
        for (owner, f) in every_field() {
            let Some(text) = f.default else { continue };
            defaults += 1;
            let v = decode_ordered(text.as_bytes(), 1 << 12)
                .unwrap_or_else(|e| panic!("{owner}.{}: default `{text}` is not JSON: {e}", f.key));
            assert!(
                fits(&f.ty, &v),
                "{owner}.{}: default `{text}` does not fit {:?}",
                f.key,
                f.ty
            );
            assert!(
                !f.required,
                "{owner}.{}: a required key has no default",
                f.key
            );
        }
        assert!(
            defaults >= 20,
            "the table carries its defaults ({defaults})"
        );
        // The ones the spec states.
        let d = |n: &'static Node, k: &str| field(n, k).and_then(|f| f.default);
        assert_eq!(d(&GRAB, "above"), Some("72"));
        assert_eq!(d(&GRAB, "below"), Some("80"));
        assert_eq!(d(&BAND, "chrome"), Some("\"fades\""));
        assert_eq!(d(&CELL_ACTIONS, "axis"), Some("\"row\""));
        assert_eq!(d(&CELL_STRIP, "label_align"), Some("\"centre\""));
        assert_eq!(d(&ITEM, "rest"), Some("\"film\""));
        assert_eq!(d(&ITEM, "focus"), Some("\"accent\""));
    }

    #[test]
    fn hud_format_schema_rules_name_fields_of_their_node_and_values_of_those_fields() {
        let mut checked = 0;
        let check = |owner: &str, keys_in: &dyn Fn(&str) -> Option<&'static Field>, r: &Rule| {
            let need = |k: &str| {
                assert!(
                    keys_in(k).is_some(),
                    "{owner}: rule {r:?} names `{k}`, which is not a key"
                );
            };
            // A name `value` of a key `k`: `k` must be a closed name list that holds it.
            let named = |k: &str, value: &str| {
                let f = keys_in(k).unwrap_or_else(|| panic!("{owner}: `{k}` is not a key"));
                let names = names_of(&f.ty).unwrap_or_else(|| {
                    panic!("{owner}.{k}: rule {r:?} compares a name but {k} has none")
                });
                assert!(
                    names.contains(&value),
                    "{owner}.{k}: `{value}` is not one of {names:?}"
                );
            };
            match *r {
                Rule::Requires { key, other } => {
                    need(key);
                    need(other);
                }
                Rule::Excludes { a, b } | Rule::Disjoint { a, b } => {
                    need(a);
                    need(b);
                }
                Rule::IfValue {
                    key,
                    equals,
                    require,
                    forbid,
                }
                | Rule::IfNotValue {
                    key,
                    equals,
                    require,
                    forbid,
                } => {
                    named(key, equals);
                    require.iter().chain(forbid).for_each(|k| need(k));
                }
                Rule::ForbidPair {
                    a,
                    a_value,
                    b,
                    b_value,
                } => {
                    named(a, a_value);
                    named(b, b_value);
                }
                Rule::NonZero(key) => need(key),
                Rule::NameOnlyWith {
                    key,
                    name,
                    other,
                    other_value,
                } => {
                    // `HUD_TITLE` is a rung `Ty::Size` accepts, so it has no name list to look in.
                    assert!(
                        matches!(keys_in(key).map(|f| f.ty), Some(Ty::Size))
                            && size_rung(name).is_some()
                    );
                    named(other, other_value);
                }
                Rule::AtMost { key, other } => {
                    need(key);
                    need(other);
                    for k in [key, other] {
                        let f = keys_in(k).unwrap();
                        assert!(
                            matches!(f.ty, Ty::Int { .. } | Ty::Num { .. }),
                            "{owner}.{k}: AtMost compares numbers"
                        );
                        assert!(
                            !f.keyable,
                            "{owner}.{k}: a compared number is never state-keyed"
                        );
                    }
                }
                Rule::FaceMapOnlyWhen { .. } => {
                    panic!("{owner}: FaceMapOnlyWhen belongs to a command")
                }
            }
        };
        for n in all_nodes() {
            let n: &'static Node = n;
            for r in n.rules {
                check(n.name, &|k| field(n, k), r);
                checked += 1;
            }
        }
        for c in COMMANDS {
            let c: &'static Cmd = c;
            for r in c.rules {
                // A command's rules read the item's keys as well as its own parameters.
                let lookup = |k: &str| c.param(k).or_else(|| field(&ITEM, k));
                match *r {
                    Rule::FaceMapOnlyWhen { param, equals } => {
                        let f = c
                            .param(param)
                            .unwrap_or_else(|| panic!("{}: no parameter {param}", c.name));
                        assert!(matches!(f.ty, Ty::Either(..)), "{}.{param}", c.name);
                        let mut has = false;
                        walk_ty(&f.ty, &mut |t| {
                            has |= names_of(t).is_some_and(|n| n.contains(&equals))
                        });
                        assert!(has, "{}.{param} has no name {equals}", c.name);
                        assert!(!c.face_states.is_empty());
                    }
                    Rule::NonZero(key) => {
                        assert!(matches!(c.param(key).map(|f| f.ty), Some(Ty::Either(..))))
                    }
                    Rule::IfValue {
                        key,
                        equals,
                        require,
                        forbid,
                    }
                    | Rule::IfNotValue {
                        key,
                        equals,
                        require,
                        forbid,
                    } => {
                        let f = c
                            .param(key)
                            .unwrap_or_else(|| panic!("{}: no parameter {key}", c.name));
                        let mut has = false;
                        walk_ty(&f.ty, &mut |t| {
                            has |= names_of(t).is_some_and(|n| n.contains(&equals))
                        });
                        assert!(has, "{}.{key} has no name {equals}", c.name);
                        for k in require.iter().chain(forbid) {
                            assert!(lookup(k).is_some(), "{}: rule names unknown `{k}`", c.name);
                        }
                    }
                    other => panic!("{}: unexpected command rule {other:?}", c.name),
                }
                checked += 1;
            }
        }
        assert!(
            checked >= 15,
            "the table carries its cross-key rules ({checked})"
        );
    }

    #[test]
    fn hud_format_schema_the_ten_commands_have_the_spec_parameters_and_face_states() {
        let states = |c: &str| -> Vec<(&'static str, bool, bool)> {
            command(c)
                .unwrap()
                .face_states
                .iter()
                .map(|s| (s.key, s.required, s.ladder))
                .collect()
        };
        assert_eq!(
            states("toggle_play"),
            [("playing", true, false), ("paused", true, false)]
        );
        assert_eq!(
            states("previous"),
            [("default", true, false), ("restart", true, false)]
        );
        assert_eq!(
            states("toggle_stats"),
            [("on", true, false), ("off", true, false)]
        );
        assert_eq!(
            states("seek"),
            [("default", true, false), ("by_interval", false, true)]
        );
        for single in [
            "next",
            "open_tracks",
            "open_more",
            "toggle_section",
            "from_beginning",
            "go_to_detail",
        ] {
            assert!(
                command(single).unwrap().face_states.is_empty(),
                "{single} takes a single face"
            );
        }
        let params = |c: &str| -> Vec<(&'static str, bool)> {
            command(c)
                .unwrap()
                .params
                .iter()
                .map(|p| (p.key, p.required))
                .collect()
        };
        assert_eq!(params("seek"), [("by", true), ("dir", false)]);
        assert_eq!(params("open_tracks"), [("tab", true)]);
        assert_eq!(params("open_more"), [("page", false)]);
        assert_eq!(params("toggle_section"), [("section", true)]);
        for none in [
            "toggle_play",
            "previous",
            "next",
            "from_beginning",
            "go_to_detail",
            "toggle_stats",
        ] {
            assert!(
                command(none).unwrap().params.is_empty(),
                "{none} takes no parameters"
            );
        }
        let en = |c: &str, p: &str| names_of(&command(c).unwrap().param(p).unwrap().ty).unwrap();
        assert_eq!(en("open_tracks", "tab"), ["subtitles", "audio"]);
        assert_eq!(en("open_more", "page"), ["root", "quality", "speed"]);
        assert_eq!(en("toggle_section", "section"), ["chapters", "info"]);
        assert_eq!(en("seek", "dir"), ["back", "forward"]);
        let by = command("seek").unwrap().param("by").unwrap();
        let Ty::Either(Ty::Int { min, max }, Ty::Enum(setting)) = by.ty else {
            panic!("{:?}", by.ty)
        };
        assert!(
            *min < 0 && *max > 0 && *setting == ["setting"],
            "`by` is a signed integer or `setting`"
        );
        assert_eq!(
            command("open_more").unwrap().param("page").unwrap().default,
            Some("\"root\"")
        );
        // The unique, required More opener has no condition of its own: it is a normal command.
        assert!(COMMANDS.iter().all(|c| c.params.iter().all(|p| !p.keyable)));
    }

    #[test]
    fn hud_format_schema_only_plain_numbers_inks_gaps_and_bold_are_keyable() {
        for (owner, f) in every_field() {
            if !f.keyable {
                continue;
            }
            let ok = matches!(f.ty, Ty::Int { .. } | Ty::Num { .. } | Ty::Ink | Ty::Gap)
                || (matches!(f.ty, Ty::Bool) && f.key == "bold");
            assert!(
                ok,
                "{owner}.{} is keyable but is not a number, an ink, a gap or bold",
                f.key
            );
        }
        assert!(
            !field(&ITEM, "size").unwrap().keyable,
            "a control's size is never state-keyed"
        );
        // The three the reference keys.
        assert!(field(&BAND, "gap_before").unwrap().keyable);
        assert!(field(&CELL_STRIP, "card_h").unwrap().keyable);
        assert!(field(&CELL_TEXT, "ink").unwrap().keyable);
        assert!(
            !field(&CELL_RECT, "pad").unwrap().keyable
                && !field(&CELL_RECT, "bleed").unwrap().keyable
        );
    }

    #[test]
    fn hud_format_schema_binding_lists_are_subsets_of_the_catalogue_and_card_scope_is_by_name() {
        let names: HashSet<_> = BINDINGS.iter().map(|b| b.name).collect();
        assert_eq!(names.len(), BINDINGS.len(), "binding names are unique");
        assert_eq!(BINDINGS.len(), 30);
        for list in [
            TEXT_BINDS,
            IMAGE_BINDS,
            STRIP_BINDS,
            FACTS_BINDS,
            CARD_ART_BINDS,
        ] {
            assert!(
                !list.is_empty() && list.iter().all(|n| names.contains(n)),
                "{list:?}"
            );
        }
        // Every Bind in the tree is one of the five position lists, whole.
        let lists: [&[&str]; 5] = [
            TEXT_BINDS,
            IMAGE_BINDS,
            STRIP_BINDS,
            FACTS_BINDS,
            CARD_ART_BINDS,
        ];
        let mut binds = 0;
        for (owner, f) in every_field() {
            if let Ty::Bind(l) = f.ty {
                binds += 1;
                assert!(
                    lists.iter().any(|x| *x == l),
                    "{owner}.{} binds an unknown list",
                    f.key
                );
            }
        }
        assert_eq!(binds, 5);
        assert_eq!(
            TEXT_BINDS.iter().filter(|n| is_card_scoped(n)).count(),
            2,
            "item.start, item.title"
        );
        assert!(CARD_ART_BINDS.iter().all(|n| is_card_scoped(n)));
        // A condition that is also a binding is spelled the same.
        for c in CONDITIONS {
            if !c.name.starts_with("panel.") && !c.name.starts_with("slot.") && c.name != "sections"
            {
                assert!(
                    binding(c.name).is_some(),
                    "condition {} is a binding too",
                    c.name
                );
            }
        }
        for b in BINDINGS {
            assert!(!b.doc.is_empty(), "{}", b.name);
        }
        assert!(binding("title.main").is_some() && binding("title.mian").is_none());
    }

    #[test]
    fn hud_format_schema_hud_title_is_legal_on_title_text_only_and_body_sizes_follow_the_rungs() {
        let from_rungs: Vec<&str> = SIZE_RUNGS
            .iter()
            .map(|(n, _)| *n)
            .filter(|n| *n != "HUD_TITLE")
            .collect();
        assert_eq!(
            BODY_SIZES,
            from_rungs.as_slice(),
            "BODY_SIZES is SIZE_RUNGS but for HUD_TITLE"
        );
        assert!(size_rung("HUD_TITLE") == Some(HUD_TITLE_SZ));
        // Every Ty::Size sits in a node whose rule restricts HUD_TITLE; every other size key excludes it.
        for n in all_nodes() {
            for f in n.fields {
                if matches!(f.ty, Ty::Size) {
                    assert!(
                        n.rules.iter().any(|r| matches!(r, Rule::NameOnlyWith { key, name: "HUD_TITLE", .. } if *key == f.key)),
                        "{}.{} accepts HUD_TITLE without a rule",
                        n.name,
                        f.key
                    );
                }
                if let Ty::Enum(names) = f.ty {
                    assert!(!names.contains(&"HUD_TITLE"), "{}.{}", n.name, f.key);
                }
            }
        }
        for (n, k) in [(&CELL_FACTS, "size"), (&CELL_PLAYBAR, "clock_size")] {
            assert_eq!(field(n, k).unwrap().ty, Ty::Enum(BODY_SIZES));
        }
        assert_eq!(field(&CELL_TEXT, "size").unwrap().ty, Ty::Size);
    }

    #[test]
    fn hud_format_schema_the_resolvers_refuse_the_excluded_tokens_and_unlisted_names() {
        for refused in [
            "PASS_GOLD",
            "PASS_GOLD_INK",
            "DIAG_FLIP_A",
            "DIAG_LINK_DEFICIT",
            "RATING_FRESH",
            "RATING_CERTIFIED",
            "AMBER_500",
            "RED_500",
            "GREEN_400",
            "text_primary",
            "",
        ] {
            assert!(ink_token(refused).is_none(), "{refused} must not resolve");
        }
        assert_eq!(INK_TOKENS.len(), 11);
        assert!(size_rung("DIAGNOSTIC").is_none() && size_rung("hero").is_none());
        assert!(space_token("XXL").is_none() && space_token("md").is_none());
        assert!(builtin_icon("rewind").is_none() && builtin_icon("fast-forward").is_none());
        // `close` and `check` are listed; every name is kebab-case and unique.
        assert!(
            builtin_icon("close") == Some(Icon::Close)
                && builtin_icon("check") == Some(Icon::Check)
        );
        let names: HashSet<_> = BUILTIN_ICONS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), BUILTIN_ICONS.len());
        assert!(BUILTIN_ICONS
            .iter()
            .all(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_lowercase() || c == '-')));
        // The worked example's built-in faces all resolve.
        for used in ["previous", "restart", "pause", "play", "next", "info"] {
            assert!(builtin_icon(used).is_some(), "builtin:{used}");
        }
    }

    #[test]
    fn hud_format_schema_layout_nodes_carry_the_spec_keys() {
        let keys = |n: &Node| -> Vec<&'static str> { n.fields.iter().map(|f| f.key).collect() };
        let req = |n: &Node| -> Vec<&'static str> {
            n.fields
                .iter()
                .filter(|f| f.required)
                .map(|f| f.key)
                .collect()
        };
        assert_eq!(
            keys(&LAYOUT),
            ["anchor", "inset", "x_inset", "band", "sections", "scrim", "block", "up_next"]
        );
        assert_eq!(
            field(&LAYOUT, "anchor").unwrap().ty,
            Ty::Enum(&["bottom"]),
            "D13: bottom only"
        );
        assert_eq!(keys(&SCRIM), ["cover", "extra", "height"]);
        assert_eq!(
            field(&SCRIM, "extra").unwrap().ty,
            Ty::Int {
                min: 0,
                max: SCRIM_H as i64
            },
            "extra is at most classic's scrim"
        );
        assert_eq!(
            keys(&BAND),
            ["id", "when", "gap_before", "gap", "z", "chrome", "cells"]
        );
        assert_eq!(req(&BAND), ["id", "cells"]);
        assert_eq!(
            keys(&CELL_TEXT),
            [
                "bind",
                "size",
                "ink",
                "alpha",
                "bold",
                "leading",
                "w",
                "max_lines"
            ]
        );
        assert_eq!(keys(&CELL_IMAGE), ["bind", "w", "h", "radius", "crop"]);
        assert_eq!(keys(&CELL_ICON), ["asset", "size", "ink"]);
        assert_eq!(keys(&CELL_RULE), ["axis", "length", "ink"]);
        assert_eq!(
            keys(&CELL_RECT),
            ["ink", "radius", "pad", "bleed", "gap", "h", "cells"]
        );
        assert_eq!(
            keys(&CELL_FACTS),
            [
                "bind",
                "order",
                "genres_max",
                "bold",
                "ink",
                "playback_fact",
                "badges",
                "size"
            ]
        );
        assert_eq!(keys(&CELL_VSTACK), ["gap", "w", "cells"]);
        assert_eq!(keys(&CELL_SPACER), ["w"]);
        assert_eq!(
            keys(&CELL_PLAYBAR),
            [
                "w",
                "fill",
                "track",
                "thickness",
                "knob",
                "clocks",
                "clock_size",
                "mark",
                "grab"
            ]
        );
        assert_eq!(keys(&GRAB), ["above", "below"]);
        assert_eq!(keys(&CELL_SEGMENT), ["id", "gap", "items"]);
        assert_eq!(
            keys(&CELL_STRIP),
            [
                "bind",
                "w",
                "card_w",
                "card_h",
                "gap",
                "radius",
                "focus",
                "label_align",
                "card"
            ]
        );
        assert_eq!(keys(&CARD), ["art", "overlay", "below", "below_gap"]);
        assert_eq!(keys(&CARD_ART), ["bind", "crop"]);
        assert_eq!(
            keys(&CELL_ACTIONS),
            ["axis", "gap", "item_h", "min_w", "items"]
        );
        assert_eq!(
            keys(&ITEM),
            [
                "id",
                "command",
                "widget",
                "size",
                "rest",
                "focus",
                "face",
                "when",
                "gap_before",
                "gap_after"
            ]
        );
        assert_eq!(req(&ITEM), ["id", "command", "widget", "size"]);
        assert_eq!(
            keys(&UP_NEXT),
            [
                "replaces",
                "band",
                "align",
                "still",
                "caption",
                "watch_credits",
                "play_next",
                "countdown"
            ]
        );
        assert_eq!(keys(&UP_NEXT_BUTTON), ["rest", "focus"]);
        fn en(n: &'static Node, k: &str) -> &'static [&'static str] {
            names_of(&field(n, k).unwrap().ty).unwrap()
        }
        assert_eq!(en(&CELL, "align"), ["start", "center", "end"]);
        assert_eq!(en(&CELL, "valign"), ["top", "center", "bottom"]);
        assert_eq!(en(&ITEM, "widget"), ["disc", "pill", "button"]);
        assert_eq!(en(&ITEM, "rest"), ["film", "bare", "solid", "custom"]);
        assert_eq!(en(&ITEM, "focus"), ["accent", "ring"]);
        assert_eq!(
            en(&UP_NEXT, "countdown"),
            ["fill_sweep", "ring", "bar", "numeric"]
        );
        assert_eq!(en(&CELL_PLAYBAR, "knob"), ["tick", "always"]);
        assert_eq!(en(&CELL_PLAYBAR, "clocks"), ["follow_head", "ends"]);
        assert_eq!(en(&CELL_PLAYBAR, "mark"), ["after_elapsed", "busy_only"]);
        assert_eq!(
            en(&CELL_FACTS, "order"),
            ["duration", "year", "genres", "content_rating"]
        );
        assert_eq!(
            en(&CELL_FACTS, "badges"),
            ["content_rating", "audio_format", "cc", "sdh", "ad"]
        );
        assert_eq!(en(&CELL_STRIP, "label_align"), ["centre", "leading"]);
        assert_eq!(en(&BAND, "chrome"), ["fades", "steady"]);
        assert_eq!(en(&LAYOUT, "band"), ["controls", "block"]);
        assert_eq!(en(&LAYOUT, "sections"), ["modal", "inline"]);
        // The rules the spec states are data.
        assert!(CELL_FACTS.rules.contains(&Rule::Disjoint {
            a: "order",
            b: "badges"
        }));
        assert!(CELL_RECT.rules.contains(&Rule::AtMost {
            key: "bleed",
            other: "pad"
        }));
        assert!(ITEM.rules.iter().any(|r| matches!(
            r,
            Rule::ForbidPair {
                a: "rest",
                a_value: "solid",
                b: "focus",
                b_value: "accent"
            }
        )));
        // Up Next's looks and an item's share one definition of rest and focus.
        assert_eq!(field(&ITEM, "rest"), field(&UP_NEXT_BUTTON, "rest"));
        assert_eq!(field(&ITEM, "focus"), field(&UP_NEXT_BUTTON, "focus"));
    }
}
