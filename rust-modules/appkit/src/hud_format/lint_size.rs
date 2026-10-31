//! **`MIN_CONTROL_SIZE`, and the compile's item-keyed finding** (`docs/hud-plugins.md`, "Package format
//! 1.0", the `MIN_CONTROL_SIZE` paragraph).
//!
//! A focusable control drawn smaller than [`MIN_CONTROL_SIZE`] on its smaller side is `HUD-E0603`;
//! `Compiled::findings()` (a number or gap keyed on a strip card's `item.*` term) is mapped onto
//! `HUD-E0604` and printed with the rest.
//!
//! **E0603 is reported once per control per language**, not once per state: a control's `size` is a plain
//! number, never state-keyed, so every state that places it repeats the same fact. The state and slot of
//! the finding are those of the first state, in `Compiled::states()` order, that places the control at its
//! smallest (a Skip state's credits layout is walked too). A pill or a button is as wide as its label, so
//! its width can differ between languages; the language collapse merges the finding only where it is the
//! same in every one.
//!
//! **E0604 carries no state or slot**: the compiler finds it once in the typed tree, whatever the state.
//! It has the default context, the driver tags the language and the collapse reports it once.
//!
//! The pointer is the control's item (`CompiledState::control_pointer`), or the keyed value's own for
//! E0604. Pure: no IO, no globals.

use plx_ui::widgets::{StatusOverlay, TAB_PILL_H};
use plx_ui::Rect;

use super::compile::{CompileFinding, SkipKind};
use super::lint::{ctx_for, named, LangCtx, StateCtx};
use super::{codes, Finding};
use crate::player_hud::classic;

/// **The smallest side, in px, a package's focusable control may be drawn at**
/// (`docs/hud-plugins.md`, "Package format 1.0", the `size` paragraph). It is the narrowest seat the app's
/// own screens give a focus target, so a package never asks the remote for a smaller one than the app does.
///
/// **Derivation** (every figure re-checked against the tree this constant landed on):
/// - The narrowest focus target the app places is a LETTER of the Library's jump rail, which the Right key
///   reaches from the grid's last column (`screens/src/library/rail.rs:251`, `Focusable for RailPart`). Its
///   seat is `RAIL_TRACK_W` (44) wide (`screens/src/library/layout.rs:29`) and one pitch tall
///   (`screens/src/library/rail.rs:197-203`, the slot a focus step lands on). The pitch is
///   `RAIL_AVAIL / n` clamped to `[RAIL_MIN_PITCH, RAIL_PITCH]` (`screens/src/library/layout.rs:235-237`)
///   with `RAIL_PITCH` 32 (`:31`) and `RAIL_MIN_PITCH` 26 (`:33`, "the least a letter is squeezed to"), and
///   `RAIL_AVAIL` 864 (`:227`: `RAIL_BOTTOM` 1016 less `RAIL_TOP_HIGH` 152, `:221-225`). A listing of 34 or
///   more labels (864 / 34 = 25.4; a script other than Latin lists the server's own labels) is therefore
///   placed at 26, and scrolls, so that seat's smaller side is 26.
/// - **26, not 32.** 32 is the pitch of a SHORT alphabet, the most a letter is ever given (a Latin one, 27
///   letters with `#`, is 864 / 27 = 32). The seat the app places in the worst case is 26, and a floor that
///   the app itself undercuts would not be derived from it.
/// - **A seat, not the drawn disc.** The focused letter draws a disc `pitch - 2` wide inside its seat
///   (`screens/src/library/rail.rs:14`, 24 at the least). A package control's rect is both its face and
///   its seat (a disc is `size` x `size`, `ControlRect::rect` is the rect it is DRAWN at, and the focus
///   engine seats on it), so the comparison is seat against seat: the smaller side of
///   `ControlRect::rect` against the app's narrowest seat.
/// - **Why this is not a larger number.** The standalone control FACES the app places are all larger:
///   the page header's pills are 56 tall (`ui/src/page_header.rs:56`, the plate at `:261`), the hero
///   discs, the tab pills and a plain table row are 60 (`StatusOverlay::CTRL_H`, `ui/src/widgets.rs:4299`;
///   `TAB_PILL_H`, `:6429`; `ROW_H`, `ui/src/table.rs:341`), the transport discs 64 (`BTN_S`,
///   `appkit/src/player_hud/classic.rs:60`). A floor at the smallest FACE (56) would reject the reference
///   package's 52 px discs, which the format's own example must keep (`docs/hud-plugins.md`, the spec's
///   R1). The rail letter is the one focus target of the app below the reference's size, so the two
///   requirements ("derived from the smallest control the app's own screens place" and "admits 52")
///   meet there. Raising the floor to the face minimum is a one-line change here, in the pin test and in
///   the reference.
///
/// `appkit` sits below `screens` (`ci/module-layers.ini`, `[screens]` uses `appkit`), so the rail's
/// constants cannot be named from here: this is a cited literal, and `min_control_size_is_the_narrowest_seat`
/// pins it. The test does compare it against every control face above that `appkit` CAN name.
pub(crate) const MIN_CONTROL_SIZE: f32 = 26.0;

/// The standalone control FACES the app places that `appkit` can name (the doc of [`MIN_CONTROL_SIZE`] lists
/// them): the page header's pill, the hero discs, the tab pill, a plain table row and the transport discs. The
/// hint of `HUD-E0603` prints their range, and `min_control_size_is_the_narrowest_seat` checks the floor
/// against them, so neither keeps a copy of a number.
const APP_FACES: [f32; 5] =
    [plx_ui::page_header::HEIGHT, StatusOverlay::CTRL_H, TAB_PILL_H, plx_ui::table::ROW_H, classic::CTRL_H];

/// Report this module's findings for the language of `cx`.
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    undersized(cx, out);
    out.extend(cx.compiled.findings().iter().map(compile_item));
}

/// The smallest a control is drawn at, and where it was first seen so.
struct Smallest<'a> {
    pointer: &'a str,
    id: &'a str,
    rect: Rect,
    side: f32,
    state: StateCtx<'a>,
}

/// `HUD-E0603` for each control, in every state and every Skip layout of `cx`, whose smaller drawn side is
/// under [`MIN_CONTROL_SIZE`], once per control.
fn undersized(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    let mut worst: Vec<Smallest<'_>> = Vec::new();
    for state in cx.states() {
        let credits = state.state.skip_variant(SkipKind::Credits);
        // `skip_variant` answers the state itself when it has no credits layout of its own
        let layouts = [Some(state.state), (!std::ptr::eq(credits, state.state)).then_some(credits)];
        for laid in layouts.into_iter().flatten() {
            for (i, c) in laid.controls.iter().enumerate() {
                let side = c.rect.w.min(c.rect.h);
                if side >= MIN_CONTROL_SIZE {
                    continue;
                }
                let here = Smallest { pointer: laid.control_pointer(i), id: &c.id, rect: c.rect, side, state };
                match worst.iter_mut().find(|w| w.pointer == here.pointer) {
                    Some(seen) if side < seen.side => *seen = here,
                    Some(_) => {}
                    None => worst.push(here),
                }
            }
        }
    }
    let smallest_face = APP_FACES.iter().copied().fold(f32::INFINITY, f32::min);
    let largest_face = APP_FACES.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for w in worst {
        let mut ctx = ctx_for(cx, &w.state);
        ctx.rects.push(named(w.id, w.rect));
        out.push(
            Finding::new(
                &codes::E0603,
                w.pointer,
                format!(
                    "control `{}` is drawn {} x {} px, under the {MIN_CONTROL_SIZE} px floor on its smaller side \
                     (the narrowest seat the app's own screens give a focus target)",
                    w.id, w.rect.w, w.rect.h
                ),
                format!(
                    "Set `size` to at least {MIN_CONTROL_SIZE}; the app's own control faces are {smallest_face} to \
                     {largest_face} px (classic's discs are {}).",
                    classic::CTRL_H
                ),
            )
            .with(ctx),
        );
    }
}

/// A [`CompileFinding`] as the registered `HUD-E0604`: the keyed value's pointer, the compiler's sentence,
/// and a hint that names the `item.*` term and what to key instead.
fn compile_item(f: &CompileFinding) -> Finding {
    Finding::new(
        &codes::E0604,
        f.pointer.clone(),
        f.message.clone(),
        format!(
            "Remove the `{term}` case: a strip card is laid out once for every card, so a number or gap cannot \
             depend on `{term}`. Key it on a state condition (`panel.chapters`, `panel.info`, \
             `chapters.present`, `title.is_episode`, `transport.paused`), or key an ink or `bold` on `{term}` instead.",
            term = f.term
        ),
    )
}

#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield. Both codes are reachable through a package
/// that parses and passes reachability, so neither is `UNIT_ONLY`.
#[cfg(test)]
pub(super) const INVALID: &[InvalidCase] = &[
    // The reference's `info` disc (52 px) drawn at 20 px: under the floor on its smaller side.
    InvalidCase {
        name: "the reference's info disc drawn at 20 px",
        code: "HUD-E0603",
        pointer: "/layout/block/2/cells/1/segment/items/1",
        base: Base::Reference,
        edits: &[(
            r#""section": "info", "widget": "disc", "size": 52,"#,
            r#""section": "info", "widget": "disc", "size": 20,"#,
        )],
        langs: &[],
        skew: 0.0,
        also: &[],
    },
    // A strip card is laid out once for every card, so a gap keyed on `item.focused` cannot apply per card:
    // the compile reports it (the layout is the unedited reference's, so nothing else is found).
    InvalidCase {
        name: "the reference's card gap keyed on item.focused",
        code: "HUD-E0604",
        pointer: "/layout/block/4/cells/0/strip/card/below_gap",
        base: Base::Reference,
        edits: &[(
            r#""below_gap": "XS","#,
            r#""below_gap": { "value": "XS", "if": { "item.focused": "SM" } },"#,
        )],
        langs: &[],
        skew: 0.0,
        also: &[],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case. None: see [`INVALID`].
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use plx_base::fontcov::advances::ShippedMeasure;

    use super::*;
    use crate::hud_format::compile::{compile, CompileCtx};
    use crate::hud_format::lint::testing::{apply, assert_case, Base};
    use crate::hud_format::lint::{lint, LintLang, LintOptions};
    use crate::hud_format::parse;
    use crate::hud_format::{Code, BUILTIN_CLASSIC};

    const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
    const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");
    const HERO: &str = include_str!("fixtures/hero-center.plxhud.json");
    const TABS: &str = include_str!("fixtures/tabs.plxhud.json");
    const PALETTE: &str = include_str!("fixtures/palette.plxhud.json");

    const FIXTURES: [(&str, &str); 6] = [
        ("classic", BUILTIN_CLASSIC),
        ("reference", REFERENCE),
        ("minimal", MINIMAL),
        ("hero-center", HERO),
        ("tabs", TABS),
        ("palette", PALETTE),
    ];

    /// The reference's `info` disc, 52 px, and the same item at `size`.
    const INFO_DISC: &str = r#""section": "info", "widget": "disc", "size": 52,"#;
    fn info_disc_at(size: u32) -> String {
        format!(r#""section": "info", "widget": "disc", "size": {size},"#)
    }

    /// What this module's `check` reports for `text` in the language `tag`, with the real font advances.
    fn found(text: &str, tag: &str) -> Vec<Finding> {
        let parsed = parse::parse(text.as_bytes());
        let package = parsed.package.expect("a package");
        let lang = LintLang::for_tag(tag).expect("a shipped language");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        let opts = LintOptions::default();
        let cx = LangCtx { package: &package, compiled: &compiled, measure: &ShippedMeasure, lang: &lang, opts: &opts };
        let mut out = Vec::new();
        check(&cx, &mut out);
        out
    }

    /// The findings of `code` in the whole lint of `text`, in every shipped language.
    fn linted(text: &str, code: &Code) -> Vec<Finding> {
        lint(text, &ShippedMeasure, &LintLang::shipped()).into_iter().filter(|f| f.code == code.id).collect()
    }

    #[test]
    fn min_control_size_is_the_narrowest_seat() {
        // the rail's letter: `RAIL_MIN_PITCH` (screens/src/library/layout.rs:33); `screens` is above `appkit`, so the
        // value is pinned here and cited at the constant
        assert_eq!(MIN_CONTROL_SIZE, 26.0);
        // every control FACE the app places that `appkit` can name is at or above it, and the smallest of them,
        // the page header's pill, is the figure the constant's doc gives for why it is not the floor
        let faces = APP_FACES;
        for face in faces {
            assert!(face >= MIN_CONTROL_SIZE, "a control face of {face} px is under the floor");
        }
        assert_eq!(faces.iter().copied().fold(f32::INFINITY, f32::min), 56.0);
        assert_eq!(plx_ui::page_header::HEIGHT, 56.0);
    }

    #[test]
    fn reference_52_is_admitted() {
        let reference_disc = 52.0_f32;
        assert!(reference_disc >= MIN_CONTROL_SIZE);
        let parsed = parse::parse(REFERENCE.as_bytes());
        let package = parsed.package.expect("a package");
        let lang = LintLang::for_tag("en").expect("en");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        let sides: BTreeSet<u32> = compiled
            .states()
            .iter()
            .flat_map(|s| s.controls.iter().map(|c| c.rect.w.min(c.rect.h) as u32))
            .collect();
        assert!(sides.contains(&52), "the reference places 52 px controls: {sides:?}");
        assert!(sides.iter().all(|&s| s as f32 >= MIN_CONTROL_SIZE), "{sides:?}");
        for lang in LintLang::shipped() {
            assert!(found(REFERENCE, lang.tag).is_empty(), "{}", lang.tag);
        }
    }

    #[test]
    fn classic_the_reference_and_every_fixture_have_no_finding_here_in_every_shipped_language() {
        for (name, text) in FIXTURES {
            for lang in LintLang::shipped() {
                let got = found(text, lang.tag);
                assert!(got.is_empty(), "{name} in {}: {got:?}", lang.tag);
            }
        }
    }

    #[test]
    fn invalid_cases_yield_exactly_their_code() {
        assert!(!INVALID.is_empty());
        for case in INVALID {
            assert_case(case);
        }
        let codes: BTreeSet<&str> = INVALID.iter().map(|c| c.code).collect();
        assert_eq!(codes, BTreeSet::from(["HUD-E0603", "HUD-E0604"]));
        assert!(UNIT_ONLY.is_empty(), "both codes are reachable through a parseable package: {UNIT_ONLY:?}");
    }

    #[test]
    fn an_undersized_control_is_one_finding_with_its_state_slot_and_rect() {
        let text = apply(Base::Reference, &[(INFO_DISC, &info_disc_at(20))]);
        // one language: the finding keeps its tag
        let one = found(&text, "de");
        let [f] = one.as_slice() else { panic!("one finding in one language, got {one:?}") };
        assert_eq!((f.code, f.pointer.as_str()), ("HUD-E0603", "/layout/block/2/cells/1/segment/items/1"));
        assert!(f.is_error());
        assert_eq!(f.ctx.state.as_deref(), Some("compact,movie"));
        assert_eq!(f.ctx.slot.as_deref(), Some("discs"));
        let [rect] = f.ctx.rects.as_slice() else { panic!("one rect, got {:?}", f.ctx.rects) };
        assert_eq!((rect.name.as_str(), rect.w, rect.h), ("info", 20.0, 20.0));
        assert!(f.message.contains("`info`") && f.message.contains("20 x 20"), "{}", f.message);
        assert!(f.hint.contains("26"), "{}", f.hint);
        // eleven languages: a disc is the same everywhere, so it collapses to ONE finding with no `lang`
        let all = linted(&text, &codes::E0603);
        let [f] = all.as_slice() else { panic!("one collapsed finding, got {all:?}") };
        assert_eq!(f.ctx.lang, None);
    }

    #[test]
    fn the_floor_is_inclusive() {
        let size = MIN_CONTROL_SIZE as u32;
        let at = apply(Base::Reference, &[(INFO_DISC, &info_disc_at(size))]);
        assert!(found(&at, "en").is_empty(), "{size} px is at the floor");
        let under = apply(Base::Reference, &[(INFO_DISC, &info_disc_at(size - 1))]);
        assert_eq!(found(&under, "en").len(), 1, "{} px is under it", size - 1);
    }

    #[test]
    fn a_pill_is_judged_on_its_smaller_side_which_is_its_height() {
        let find = r#""id": "open_info", "command": "toggle_section", "section": "info", "widget": "pill", "size": 52"#;
        assert_eq!(TABS.matches(find).count(), 1);
        let text = TABS.replace(find, &find.replace("52", "20"));
        for lang in LintLang::shipped() {
            let got = found(&text, lang.tag);
            let [f] = got.as_slice() else { panic!("{}: one finding, got {got:?}", lang.tag) };
            let [rect] = f.ctx.rects.as_slice() else { panic!("{}: one rect", lang.tag) };
            assert_eq!((rect.name.as_str(), rect.h), ("open_info", 20.0), "{}", lang.tag);
            assert!(rect.w > 20.0, "{}: a labelled pill is wider than tall: {}", lang.tag, rect.w);
        }
    }

    #[test]
    fn a_compile_finding_becomes_the_registered_code_with_its_term_in_the_hint() {
        let synthetic = CompileFinding {
            pointer: "/layout/block/4/cells/0/strip/card/below_gap".to_owned(),
            term: "item.is_current".to_owned(),
            message: "`below_gap` is keyed on `item.is_current`, but a card is laid out once".to_owned(),
        };
        let f = compile_item(&synthetic);
        assert_eq!(f.code, "HUD-E0604");
        assert_eq!(f.rule, "compile-item");
        assert!(f.is_error());
        assert_eq!(f.pointer, synthetic.pointer);
        assert_eq!(f.message, synthetic.message);
        assert!(f.hint.contains("`item.is_current`") && f.hint.contains("transport.paused"), "{}", f.hint);
        assert_eq!(f.ctx, crate::hud_format::Context::default(), "language-free: the driver tags and collapses it");
    }

    #[test]
    fn a_keyed_gap_is_printed_once_for_every_shipped_language() {
        let text = apply(
            Base::Reference,
            &[(r#""below_gap": "XS","#, r#""below_gap": { "value": "XS", "if": { "item.focused": "SM" } },"#)],
        );
        let got = linted(&text, &codes::E0604);
        let [f] = got.as_slice() else { panic!("one collapsed finding, got {got:?}") };
        assert_eq!(f.ctx.lang, None);
        assert!(f.message.contains("below_gap") && f.hint.contains("`item.focused`"), "{f:?}");
    }
}
