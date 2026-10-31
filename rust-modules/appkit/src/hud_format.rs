//! **The HUD package format, 1.0** (`docs/hud-plugins.md`): the descriptor table ([`schema`]), the strict
//! parser ([`parse`]), the SVG subset checker ([`svg_subset`]), the reachability check and the package-level
//! invariants ([`reach`], [`validate`], entered through [`check`]), the layout compiler ([`compile`]: the
//! rects and the draw list of every state `reach` enumerates), and the finding every one of them reports.
//!
//! A HUD package is declarative DATA, never code (D1): one UTF-8 JSON file, assets inline. This module is
//! PURE: no IO, no globals, no clock, no `cfg(test)` fork. Bytes go in; a typed [`parse::Package`] and a
//! list of [`Finding`]s come out, and [`compile::compile`] lays a package out given a text measurer and the
//! app's labels. [`lint`] runs the whole pipeline over a package's text in every shipped language and
//! reports every finding, the geometric invariants (`lint_zorder`, `lint_safe`, `lint_band`, `lint_menus`,
//! `lint_text`, `lint_size`) among them. [`schema_emit`] serializes the descriptor table to the published
//! JSON Schema, and [`report`] prints findings as the lint JSON and as text.
//!
//! ## The finding
//!
//! `{code, severity, pointer, rule, message, hint, nearest}` (the lint shape of the plan's "LLM
//! authoring" section; `--hud-lint`'s JSON key `path` is [`Finding::pointer`]). `code` is stable and never
//! reused. `rule` names the invariant (`key-known`), the same string for every finding of one code.
//! `hint` is a concrete fix. `nearest` is the closest valid names by edit distance and is empty unless a
//! name was unknown. A finding the lint runs against a COMPILED package also carries a [`Context`]
//! (`ctx`: the language, the state and slot, the rects it concerns, the measured text); a parse or
//! reachability finding has none, and the report prints none.
//!
//! **Pointer conventions** (RFC 6901, `""` is the whole document):
//! * an unknown or duplicate key: the KEY'S OWN pointer (`/layout/blok` for a misspelt `block`);
//! * a missing required key: the OBJECT's pointer (the one that lacks it);
//! * a wrong type or value: the VALUE's pointer;
//! * JSON syntax: `""`;
//! * the version gate: `/format`;
//! * an SVG asset: the asset string's pointer, `/assets/<id>`.

pub mod clocks; // the playbar's two clocks as app text: their widest strings and the rects the layout and the lint see
pub mod compile; // `compile`: a parsed package laid out in every reachable state: the rects the player reads and the draw list (docs/hud-plugins.md, "Architecture")
pub mod lint; // `lint`: the whole pipeline over a package's text, per language: parse, reach, compile, then the geometric checks below
mod lint_band; // I6: the caption band respects the player's minimum controls top
mod lint_menus; // I7: the two player menus place through the shared placer (M1-M4)
mod lint_safe; // I5: SAFE
mod lint_size; // MIN_CONTROL_SIZE, and the compile's item-keyed finding
mod lint_text; // I10: app text fits
mod lint_zorder; // I4: the z-order census
pub mod parse;
pub mod reach; // `check`: reachability R1-R4 and the folds over the HUD's state graph, then `validate`'s invariants
pub mod report;
pub mod schema;
pub mod schema_emit;
pub mod svg_subset;
pub mod validate; // the package-level invariants no geometry is needed for (I3, I9, I11)

pub use reach::{check, Checked, Fold, Slot};

/// **`builtin:classic`, as the bytes the app ships**: the one place the package is embedded
/// (`include_str!`). The player compiles exactly these bytes ([`crate::player_hud::loaded`]), the
/// tests parse exactly these bytes, and `--hud-export builtin:classic` prints exactly these bytes.
pub const CLASSIC_PACKAGE: &str = include_str!("hud_format/fixtures/classic.plxhud.json");

#[cfg(test)]
mod tests; // the corpus of valid and invalid packages, the seeded mutation tests and the table-vs-parser drift test
#[cfg(test)]
mod lint_tests; // `lint`: the contract's own pins, and the union of the check modules' invalid cases
#[cfg(test)]
mod compile_tests; // the golden: classic compiled equals classic's hand-written layout; every fixture compiles in every state
#[cfg(test)]
mod reach_tests; // `check`: one invalid package per rule, the folds of classic and the reference, the state counts
#[cfg(test)]
mod schema_check; // the in-test JSON Schema checker and the regex subset it runs
#[cfg(test)]
mod site_tests; // the published schema, examples and invalid examples with their lint reports (`site/hud/`)

/// How loudly a finding speaks. A package parses (`Parsed::package` is `Some`) only if it has no `Error`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    /// `"error"` or `"warning"`: the spelling `--hud-lint`'s JSON output uses.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// One finding code: its stable id, the invariant it reports, and its default severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Code {
    /// `HUD-E0102`: `E` an error, `W` a warning, four digits grouped by hundreds per rule family.
    pub id: &'static str,
    /// The invariant's name, `key-known`.
    pub rule: &'static str,
    pub severity: Severity,
}

/// A problem in a package: the lint shape, flat, so wave B's lint prints it unchanged.
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    /// [`Code::id`].
    pub code: &'static str,
    pub severity: Severity,
    /// An RFC 6901 JSON pointer into the package (see the module doc for which pointer a finding names).
    pub pointer: String,
    /// [`Code::rule`].
    pub rule: &'static str,
    /// What is wrong, in one sentence. The version gate's message is [`REASON_NEWER`] / [`REASON_OLDER`].
    pub message: String,
    /// A concrete fix.
    pub hint: String,
    /// The closest valid names (at most [`MAX_NEAREST`]), when the finding is an unknown name.
    pub nearest: Vec<String>,
    /// Where the lint found it: the language, state, slot, rects and measured text of a finding that
    /// needs a compiled package. [`Context::default`] (empty) for every parse and reachability finding.
    pub ctx: Context,
}

/// The context a lint finding carries beyond its pointer (docs/hud-plugins.md, "LLM authoring", the
/// finding shape): which language it was measured in, in which state and slot, which rects it concerns
/// and, for a text that does not fit, the string and its measure. Every part is optional, and a finding
/// that is the same in every linted language has no `lang` (the lint collapses it).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Context {
    /// The language tag (`de`), or `None` for a finding that does not depend on the language.
    pub lang: Option<String>,
    /// The state, as `lint::state_name` spells it (`compact`, `chapters`, `info`, with `,movie`,
    /// `,no-chapters` and `,paused` appended where the valuation is not the default).
    pub state: Option<String>,
    /// The slot, as `lint::slot_name` spells it (`discs`, `up_next`, `skip`).
    pub slot: Option<String>,
    /// The rects the finding is about, each named (`next`, `scrim`, `safe`), in the order the finding
    /// reports them.
    pub rects: Vec<NamedRect>,
    /// The text of a text-fit finding.
    pub text: Option<TextFact>,
}

/// A rect on the 1920 x 1080 canvas, named for the report.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedRect {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// What a text-fit finding measured: the app string, its width in the finding's language, and the room it
/// had.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFact {
    pub string: String,
    pub measured: f32,
    pub room: f32,
}

impl Finding {
    /// A finding of `code` at `pointer`, with the code's rule and severity, an empty `nearest` and no
    /// context.
    pub fn new(
        code: &Code,
        pointer: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Finding {
        Finding {
            code: code.id,
            severity: code.severity,
            pointer: pointer.into(),
            rule: code.rule,
            message: message.into(),
            hint: hint.into(),
            nearest: Vec::new(),
            ctx: Context::default(),
        }
    }

    /// The same finding with its closest valid names (build them with [`nearest`]).
    pub fn with_nearest(mut self, nearest: Vec<String>) -> Finding {
        self.nearest = nearest;
        self
    }

    /// The same finding with the context the lint found it in.
    pub fn with(mut self, ctx: Context) -> Finding {
        self.ctx = ctx;
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

/// Whether any finding is an `Error`: the test for "the package did not parse".
pub fn has_error(findings: &[Finding]) -> bool {
    findings.iter().any(Finding::is_error)
}

/// The user-facing reason for a `format` whose minor (or major) is above what this app supports.
pub const REASON_NEWER: &str = "Made for a newer PlxNative";
/// The user-facing reason for a `format` whose major is below what this app supports.
pub const REASON_OLDER: &str = "Uses an older HUD format";

/// The embedded classic package (`builtin:classic`), byte for byte the file `fixtures/classic.plxhud.json`:
/// what `plxnative-sim --hud-export builtin:classic` prints and the package the lint's goldens run.
pub const BUILTIN_CLASSIC: &str = include_str!("hud_format/fixtures/classic.plxhud.json");

/// `base` followed by one more reference token, RFC 6901-escaped (`~` is `~0`, `/` is `~1`). `base` is
/// `""` for the document.
pub fn pointer_push(base: &str, token: &str) -> String {
    let mut out = String::with_capacity(base.len() + token.len() + 1);
    out.push_str(base);
    out.push('/');
    for c in token.chars() {
        match c {
            '~' => out.push_str("~0"),
            '/' => out.push_str("~1"),
            c => out.push(c),
        }
    }
    out
}

/// `base` followed by an array index.
pub fn pointer_index(base: &str, index: usize) -> String {
    format!("{base}/{index}")
}

/// The most names [`nearest`] returns: a hint that lists the whole vocabulary is a table, not a hint.
pub const MAX_NEAREST: usize = 3;

/// How much of a candidate [`nearest`] compares: the edit distance is quadratic, and a candidate is
/// an author's name from a file whose names have no other bound than its size.
const COMPARED_CHARS: usize = 48;

/// The closest of `candidates` to `name` by Levenshtein distance over the ASCII-lower-cased text (so a
/// token spelt in the wrong case is still the nearest), at most [`MAX_NEAREST`], nearest first. Ties
/// break on the candidate's own text, so the same input always gives the same list; a candidate listed
/// twice appears once. There is no distance cutoff: the caller decides whether a far name is worth
/// printing, and an unknown name with no near neighbour still gets the vocabulary's first entries.
pub fn nearest<I>(name: &str, candidates: I) -> Vec<String>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let needle: Vec<char> = name.to_ascii_lowercase().chars().collect();
    let mut scored: Vec<(usize, String)> = candidates
        .into_iter()
        .map(|c| {
            let c = c.as_ref();
            let hay: Vec<char> = c
                .to_ascii_lowercase()
                .chars()
                .take(COMPARED_CHARS)
                .collect();
            (edit_distance(&needle, &hay), c.to_owned())
        })
        .collect();
    scored.sort();
    scored.dedup_by(|a, b| a.1 == b.1);
    scored
        .into_iter()
        .take(MAX_NEAREST)
        .map(|(_, c)| c)
        .collect()
}

fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if ca == cb {
                diag
            } else {
                1 + diag.min(up).min(row[j])
            };
            diag = up;
        }
    }
    row[b.len()]
}

/// Every finding code, frozen. Each const is a [`Code`]; [`ALL`] lists them in id order (string order,
/// so every `E` before every `W`). The doc line says when the code is emitted. The `06xx` and `10xx` to
/// `13xx` families are the lint's (`super::lint`): they need a compiled package, and a language, a state
/// and a slot for context. `M5` of the menus rule (the overscan audit reads the placer) has no code here:
/// it is the renderer's, in PR7.
pub mod codes {
    use super::{Code, Severity};

    macro_rules! define_codes {
        ($($(#[$doc:meta])* $name:ident = ($sev:ident, $id:literal, $rule:literal);)*) => {
            $(
                $(#[$doc])*
                pub const $name: Code = Code { id: $id, rule: $rule, severity: Severity::$sev };
            )*
            /// Every code, in id order.
            pub const ALL: &[Code] = &[$($name),*];
        };
    }

    define_codes! {
        /// The bytes are not one JSON document (or exceed the package size bound). Pointer `""`.
        E0100 = (Error, "HUD-E0100", "json-syntax");
        /// A key is written twice in one object. Pointer: the repeated key's own (the second one).
        E0101 = (Error, "HUD-E0101", "key-unique");
        /// A key the node does not have. Pointer: the key's own. `nearest`: the node's keys.
        E0102 = (Error, "HUD-E0102", "key-known");
        /// A required key is missing. Pointer: the object that lacks it.
        E0103 = (Error, "HUD-E0103", "key-required");
        /// A value of the wrong JSON type. Pointer: the value's.
        E0104 = (Error, "HUD-E0104", "type-matches");
        /// A value of the right type outside its allowed range, length or pattern. Pointer: the value's.
        E0105 = (Error, "HUD-E0105", "value-range");
        /// Keys that may not be combined, or one of a set required (a cell's single content key, a
        /// scrim's `extra` with `height`, `solid` with `accent`). Pointer: the object's.
        E0106 = (Error, "HUD-E0106", "key-combination");
        /// A key introduced in a later minor than the package's `format` declares. Pointer: the key's own.
        E0107 = (Error, "HUD-E0107", "key-since");
        /// A closed-vocabulary name (an enum value, a binding, a size rung, a space token, a built-in
        /// icon) that is not in its list. Pointer: the value's. `nearest`: the list.
        E0201 = (Error, "HUD-E0201", "name-known");
        /// An UPPER_CASE theme token outside the allow-list. Pointer: the value's. `nearest`: the list.
        E0202 = (Error, "HUD-E0202", "token-known");
        /// An asset reference no `assets` entry declares. Pointer: the value's. `nearest`: the assets.
        E0203 = (Error, "HUD-E0203", "asset-known");
        /// A lower-case palette reference no `palette` entry declares. Pointer: the value's. `nearest`:
        /// the palette.
        E0204 = (Error, "HUD-E0204", "palette-known");
        /// A `command` outside the closed vocabulary. Pointer: the `command` value's. `nearest`: the ten.
        E0301 = (Error, "HUD-E0301", "command-known");
        /// A sibling key that is not a parameter of the item's command. Pointer: the key's own.
        E0302 = (Error, "HUD-E0302", "command-param-known");
        /// A parameter the command requires and the item lacks. Pointer: the item's.
        E0303 = (Error, "HUD-E0303", "command-param-required");
        /// A parameter value of the wrong type or outside its values. Pointer: the value's.
        E0304 = (Error, "HUD-E0304", "command-param-value");
        /// Parameters that may not combine (`seek` with a numeric `by` and a `dir`, `by: "setting"`
        /// without one). Pointer: the item's.
        E0305 = (Error, "HUD-E0305", "command-param-combination");
        /// `format` is not `"MAJOR.MINOR"`. Pointer `/format`. The version gate's whole answer.
        E0401 = (Error, "HUD-E0401", "format-malformed");
        /// `format`'s minor is above the highest this app supports for its major. Pointer `/format`;
        /// message [`REASON_NEWER`](super::REASON_NEWER).
        E0402 = (Error, "HUD-E0402", "format-newer-minor");
        /// `format`'s major is not one this app supports. Pointer `/format`; message
        /// [`REASON_NEWER`](super::REASON_NEWER) for a higher major, [`REASON_OLDER`](super::REASON_OLDER)
        /// for a lower.
        E0403 = (Error, "HUD-E0403", "format-major");
        /// A `when` condition (or a keyed value's condition key) outside the closed list. Pointer: the
        /// string's (for a keyed value, the condition KEY's own). `nearest`: the list.
        E0501 = (Error, "HUD-E0501", "condition-known");
        /// A `when` that is not a string or a non-empty array of strings, or a term that is only `!`
        /// or is doubly negated. Pointer: the value's (or the array element's).
        E0502 = (Error, "HUD-E0502", "condition-shape");
        /// A condition that exists but not here (`item.is_current` outside a strip card). Pointer: the
        /// string's.
        E0503 = (Error, "HUD-E0503", "condition-scope");
        /// A state-keyed value that is not `{"value": v, "if": {condition: v, ...}}` exactly, or that
        /// keys a field that cannot be keyed. Pointer: the object's.
        E0504 = (Error, "HUD-E0504", "keyed-shape");
        /// A palette name that is not lower-case `[a-z][a-z0-9_]*`. Pointer: the key's own.
        E0505 = (Error, "HUD-E0505", "palette-name");
        /// A palette value that is not `#rrggbb`. Pointer: the value's.
        E0506 = (Error, "HUD-E0506", "palette-value");
        /// I10: app text (a tab or button label, a clock, the chapters fallback) does not fit in a language:
        /// it is wider than its room, or it grows into a control, past `SAFE` or past its box; or a pill or
        /// button whose label-sized rect runs past the fixed-`w` vstack it lives in. Pointer: the text cell, or
        /// the control's item. Context: `lang`, `text` (the string, its measured width, the room) and the
        /// `rects` it meets. Server text may elide and is never checked.
        E0601 = (Error, "HUD-E0601", "text-fit");
        /// The minimum control size: a focusable control drawn smaller than the smallest seat the app's own
        /// screens give a focus target (`lint_size::MIN_CONTROL_SIZE`, the package format's `size` paragraph;
        /// it is not an I10 text-fit rule). Pointer: the control's item. Context: `state`, `slot`, the
        /// control's `rects`.
        E0603 = (Error, "HUD-E0603", "min-control-size");
        /// The compiler could not carry a number or gap keyed on a strip card's `item.*` term (it would be
        /// a different layout per card): the base value applies. Pointer: the keyed value's. This is
        /// `Compiled::findings()`'s `CompileFinding`, registered.
        E0604 = (Error, "HUD-E0604", "compile-item");
        /// An SVG element other than `svg` and `path` (the gradient family carries the "assets are alpha
        /// masks tinted by the app; a gradient would be discarded" hint). Pointer: `/assets/<id>`.
        E0701 = (Error, "HUD-E0701", "svg-element");
        /// An SVG attribute outside the mask subset: a colour other than mask-white, `stroke*`,
        /// `opacity`, `style`, `class`, `transform` ("assets are masks tinted per state, so colour would
        /// be dropped"). Pointer: `/assets/<id>`.
        E0702 = (Error, "HUD-E0702", "svg-attribute");
        /// SVG syntax the subset refuses before any parser sees it: entities, DOCTYPE, CDATA,
        /// processing instructions, `href`/`xlink`, `url(`, `data:`, `://`. Pointer: `/assets/<id>`.
        E0703 = (Error, "HUD-E0703", "svg-syntax");
        /// An SVG that is not well-formed: unbalanced tags, no `<svg viewBox>`, a bad `viewBox` or path
        /// data. Pointer: `/assets/<id>`.
        E0704 = (Error, "HUD-E0704", "svg-malformed");
        /// An SVG larger than `svg_subset::MAX_ASSET_BYTES`. Pointer: `/assets/<id>`.
        E0705 = (Error, "HUD-E0705", "svg-size");
        /// An SVG with more `<path>` than `svg_subset::MAX_PATHS_PER_ASSET`. Pointer: `/assets/<id>`.
        E0706 = (Error, "HUD-E0706", "svg-paths");
        /// R1: no `open_more` that opens More's root is placed in the root state (compact, Discs) under
        /// every valuation of the content conditions the package names. Pointer: the `when` that removes
        /// the control under some valuation (a control's, its cell's or its band's), else the control
        /// itself when it is gated by something else, else `/layout/block` when the package has none.
        E0801 = (Error, "HUD-E0801", "more-in-root");
        /// R3: a `toggle_section {section: s}` control and no band whose `when` holds `panel.s`: an opener
        /// with nothing to open. Pointer: the control's `section` value.
        E0802 = (Error, "HUD-E0802", "opener-has-section");
        /// R4: Up Next is not whole: `up_next.replaces` is empty or names no segment of the package, or
        /// `up_next.band` names no band. (Both buttons and the countdown are required keys of
        /// `layout.up_next`, so the parser reports them missing as `E0103`.) Pointer: the offending value.
        E0803 = (Error, "HUD-E0803", "up-next-whole");
        /// I3: an `id` used by two bands, two segments or two controls (the three kinds are three
        /// namespaces). Pointer: the later `id` value.
        E0901 = (Error, "HUD-E0901", "identity-unique");
        /// I9: a palette in the package whose id is `builtin:classic`, which declares none. Pointer
        /// `/palette`.
        E0902 = (Error, "HUD-E0902", "builtin-no-palette");
        /// I4: two interactive rects of the SAME `z` overlap in a state (a lower-`z` group may underlie a
        /// higher one, as the scrub grab band does). Pointer: the later control's item. Context: `state`,
        /// `slot`, the two `rects`.
        E1001 = (Error, "HUD-E1001", "z-overlap");
        /// I4: a control's centre does not hit-test back to the control (another item's hit rect wins the
        /// point). Pointer: the control's item. Context: `state`, `slot`, the control's and the winner's `rects`.
        E1002 = (Error, "HUD-E1002", "hit-self");
        /// I4: decor (text, image, icon, rule, ground, facts, Up Next's still and caption) overlaps a drawn
        /// interactive rect, except a `rect` ground under its own cells and a strip card's own overlay and
        /// below content. The scrim is exempt: it is the ground behind the whole HUD, under every control by
        /// construction. Pointer: the decor's node. Context: `state`, `slot`, the decor's and the control's
        /// `rects`.
        E1003 = (Error, "HUD-E1003", "decor-overlap");
        /// I5: an interactive drawn or hit rect lies outside `SAFE`. Pointer: the control's item. Context:
        /// `state`, `slot`, the control's and `SAFE`'s `rects`.
        E1101 = (Error, "HUD-E1101", "safe-interactive");
        /// I5: a text, image, icon, rule or facts box, Up Next's still or caption, or a strip card's content
        /// (vertically) lies outside `SAFE` (only scrims and `rect` grounds may bleed; a strip's cards may run
        /// past the side edges). Pointer: the node's. Context: `state`, `slot`, the node's and `SAFE`'s `rects`.
        E1102 = (Error, "HUD-E1102", "safe-content");
        /// I5: the focused strip card is not revealed inside `SAFE`, or the scrub-preview card's top is
        /// outside it in a state that draws the bar. Pointer: the strip's (or the playbar's) node. Context:
        /// `state`, `slot`, the card's and `SAFE`'s `rects`.
        E1103 = (Error, "HUD-E1103", "safe-reveal");
        /// I6: in the compact state, under some slot, the package's caption band (`Compiled::band()`) rises
        /// above `player_hud::min_controls_top_y()`. Pointer: the node that sets the band: a control's item,
        /// the playbar's node, `/layout/up_next` for the Up Next and Skip stand-ins, or, under
        /// `band: "block"`, whatever node drew the highest extent (`/layout` only if the extent scan drifts
        /// from the compiled band). Context: `state`, `slot`, the `rects` `extent`, `band` and `limit`.
        E1201 = (Error, "HUD-E1201", "caption-band");
        /// Menus M1: a player menu anchored at its opener's rest rect would not sit against the opener
        /// (placed through `plx_ui::popover::anchor_panel(.., Placement::PLAYER_MENU)`). Pointer: the opener's
        /// item. Context: `state`, `slot`, the opener's and the menu's `rects`.
        E1301 = (Error, "HUD-E1301", "menu-opener");
        /// Menus M2: a player menu placed from its opener leaves `SAFE`. Pointer: the opener's item. Context:
        /// `state`, `slot`, the menu's and `SAFE`'s `rects`.
        E1302 = (Error, "HUD-E1302", "menu-safe");
        /// Menus M3 and M4: a player menu rises on the wrong side of its opener (rects `anchor` and `menu`), or
        /// two adjacent menu openers of one segment hang from different near edges (rects `menu` and `previous
        /// menu`; the finding is on the later opener). Pointer: the opener's item. Context: `state`, `slot`,
        /// and the rects named above.
        E1303 = (Error, "HUD-E1303", "menu-side");
        /// I10: app text fits but within `CI_TEXT_SKEW` of its room (a warning locally, an error in registry
        /// CI; 0 until PR12 measures it). Pointer: as `E0601`. Context: as `E0601`.
        W0602 = (Warning, "HUD-W0602", "text-fit-margin");
        /// I11: a `seek {by: "setting"}` face map whose `by_interval` lacks a rung of the viewer's Skip
        /// ladder; that rung draws `default`. A warning: a new rung can never break a published package.
        /// Pointer: the `by_interval` object.
        W0903 = (Warning, "HUD-W0903", "face-ladder-complete");
    }
}

#[cfg(test)]
mod freeze_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn pointers_escape_per_rfc_6901() {
        assert_eq!(pointer_push("", "layout"), "/layout");
        assert_eq!(pointer_push("/layout", "block"), "/layout/block");
        assert_eq!(pointer_push("/a", "x/y~z"), "/a/x~1y~0z");
        assert_eq!(pointer_push("", "$schema"), "/$schema");
        assert_eq!(pointer_index("/layout/block", 2), "/layout/block/2");
        assert_eq!(pointer_index("", 0), "/0");
    }

    #[test]
    fn nearest_is_closest_first_capped_and_deterministic() {
        let verbs = [
            "seek",
            "next",
            "previous",
            "toggle_play",
            "open_more",
            "toggle_stats",
        ];
        assert_eq!(nearest("sek", verbs), ["seek", "next", "previous"]);
        let two = nearest("toggle_pay", verbs);
        assert_eq!(two[0], "toggle_play");
        assert!(two.len() <= MAX_NEAREST);
        // The same input, any order: the same answer, ties broken on the candidate text.
        let mut shuffled = verbs;
        shuffled.reverse();
        assert_eq!(nearest("sek", shuffled), nearest("sek", verbs));
        assert_eq!(
            nearest("ab", ["bb", "ba", "bb"]),
            ["bb", "ba"],
            "a repeated candidate appears once"
        );
        // A token spelt in the wrong case is still the nearest.
        assert_eq!(
            nearest("text_primary", ["TEXT_PRIMARY", "ACCENT"])[0],
            "TEXT_PRIMARY"
        );
        assert!(nearest("x", Vec::<String>::new()).is_empty());
        // Owned strings and borrowed slices both work.
        assert_eq!(nearest("a", vec![String::from("a")]), ["a"]);
        assert_eq!(nearest("a", &["a"]), ["a"]);
    }

    #[test]
    fn the_code_table_is_unique_and_well_formed() {
        assert_eq!(codes::ALL.len(), 52, "38 parse and check codes, 14 lint codes");
        let ids: HashSet<_> = codes::ALL.iter().map(|c| c.id).collect();
        let rules: HashSet<_> = codes::ALL.iter().map(|c| c.rule).collect();
        assert_eq!(ids.len(), codes::ALL.len(), "a code id is never reused");
        assert_eq!(rules.len(), codes::ALL.len(), "one rule name per code");
        let sorted = codes::ALL.windows(2).all(|w| w[0].id < w[1].id);
        assert!(sorted, "ALL is in id order");
        for c in codes::ALL {
            let letter = if c.severity == Severity::Error {
                "HUD-E"
            } else {
                "HUD-W"
            };
            assert!(c.id.starts_with(letter) && c.id.len() == 9, "{}", c.id);
            assert!(c.id[5..].bytes().all(|b| b.is_ascii_digit()), "{}", c.id);
            assert!(
                !c.rule.is_empty() && c.rule.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
            );
        }
    }

    /// The lint's codes (`lint`'s families: `06xx`, `10xx` to `13xx`): the ones a check needs a compiled
    /// package for. Their rows are in `lint_tests` (an invalid case each), not in the parser's corpus.
    #[test]
    fn the_lint_codes_are_registered_with_their_rules_and_severities() {
        let lint = [
            (codes::E0601, "HUD-E0601", "text-fit", Severity::Error),
            (codes::W0602, "HUD-W0602", "text-fit-margin", Severity::Warning),
            (codes::E0603, "HUD-E0603", "min-control-size", Severity::Error),
            (codes::E0604, "HUD-E0604", "compile-item", Severity::Error),
            (codes::E1001, "HUD-E1001", "z-overlap", Severity::Error),
            (codes::E1002, "HUD-E1002", "hit-self", Severity::Error),
            (codes::E1003, "HUD-E1003", "decor-overlap", Severity::Error),
            (codes::E1101, "HUD-E1101", "safe-interactive", Severity::Error),
            (codes::E1102, "HUD-E1102", "safe-content", Severity::Error),
            (codes::E1103, "HUD-E1103", "safe-reveal", Severity::Error),
            (codes::E1201, "HUD-E1201", "caption-band", Severity::Error),
            (codes::E1301, "HUD-E1301", "menu-opener", Severity::Error),
            (codes::E1302, "HUD-E1302", "menu-safe", Severity::Error),
            (codes::E1303, "HUD-E1303", "menu-side", Severity::Error),
        ];
        assert_eq!(lint.len(), 14);
        for (code, id, rule, severity) in lint {
            assert_eq!((code.id, code.rule, code.severity), (id, rule, severity));
            assert!(codes::ALL.contains(&code), "{id} is in ALL");
        }
    }

    #[test]
    fn a_finding_carries_its_codes_rule_and_severity() {
        let f = Finding::new(
            &codes::E0102,
            "/layout/blok",
            "unknown key blok",
            "write block",
        )
        .with_nearest(nearest("blok", ["block", "band", "anchor"]));
        assert_eq!(f.code, "HUD-E0102");
        assert_eq!(f.rule, "key-known");
        assert_eq!(f.severity, Severity::Error);
        assert_eq!(f.severity.as_str(), "error");
        assert_eq!(f.pointer, "/layout/blok");
        assert_eq!(f.nearest, ["block", "band", "anchor"]);
        assert!(f.is_error() && has_error(&[f]));
        assert!(!has_error(&[]));
        assert_eq!(REASON_NEWER, "Made for a newer PlxNative");
        assert_eq!(REASON_OLDER, "Uses an older HUD format");
    }
}
