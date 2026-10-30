//! **I7, the menus M1-M4** (`docs/hud-plugins.md`, "Menus and other HUD popovers", "Invariants").
//!
//! Both player menus (the track menu, opened by `open_tracks`, and More, opened by `open_more`) are placed
//! through ONE call of the shared placer, `plx_ui::popover::anchor_panel(.., Placement::PLAYER_MENU)` with
//! `theme::layout::PLAYER_MENU_GAP`, which each menu exposes as `place_panel` (the live panel, the overscan
//! audit and this lint all call it, so none can drift from another). This module asks it, for every opener
//! of every compiled state, where the menu would open, and holds the answer to four invariants.
//!
//! **The anchor** (`CompiledState::menu_anchor`, the one derivation the player screen also hands to the menu it
//! mounts) is the spec's: the opener's REST rect (`ControlRect::rect`, not its popped one, so a focus
//! pop never moves a panel), widened across to its SEGMENT's outer edge when the segment's cell is start- or
//! end-aligned (`align` defaults to `start`): an end-aligned segment's anchor runs from the opener's left edge to
//! the right edge of the segment's last control, a start-aligned one's from the left edge of the segment's first
//! control to the opener's right edge, a centred one's is the rest rect itself. So the openers that sit
//! together share an edge, and classic, whose one end-aligned segment ends at `CTRL_RIGHT`, gets
//! `CTRL_RIGHT - pw` and bottom 764 for both menus (the classic pin test below).
//!
//! **The panel's size** is `(MENU_MAX_W, need_h)`: the widest a menu hugs its rows (the track menu and More
//! between `PLAYER_MENU_MIN_W` and `MENU_MAX_W`) and the menu's own `NEED_H`, the tallest app-owned page it can
//! show. The side is decided by `need_h` alone (that is the placer's contract) and a side to the left or right
//! must hold the panel's WIDTH, so the widest width is the conservative one: a side that holds it holds any
//! narrower panel. The numbers are the menus' own (`track_menu::NEED_H`, `more_menu::NEED_H`), copied nowhere.
//!
//! **The four invariants**, each a predicate below over the placed rect:
//! - **M1 `HUD-E1301` menu-opener**: the menu never intersects its opener's rest or popped (`CTRL_FOCUS_SCALE`)
//!   rect ([`meets_opener`]).
//! - **M2 `HUD-E1302` menu-safe**: it lies inside `SAFE` with its full `need_h`, so no app-owned row scrolls
//!   ([`safe_shortfall`]). When no side holds the panel the placer cuts its height to the roomiest vertical
//!   side; that cut is the finding.
//! - **M3 `HUD-E1303` menu-side**: the rise originates on the opener's side: the placed rect lies on the side the
//!   placer reports, which is the side `Side::toward_opener` displaces the rise from ([`on_side`]).
//! - **M4 `HUD-E1303` menu-side**: openers adjacent in one start- or end-aligned segment share the panel's near
//!   edge (its outer edge: the right one under `end`, the left one under `start`), so the menu does not hop
//!   from one disc to the next ([`near_edge`]). Adjacent means consecutive controls of the segment that both
//!   open a menu; the finding is on the later of the pair, and compares the two menus' own placements, so a
//!   future difference between them shows up here.
//!
//! **M5 is not here.** The overscan audit's menu rects coming from the placer rather than from constants is the
//! renderer's (PR7): this lint cannot see the renderer and does not pretend to.
//!
//! **Which states.** Every compiled state of the language (state x slot x valuation, and a Skip state's credits
//! layout, `CompiledState::skip_alt`): a menu opens from any state that draws its opener, and the reference
//! needs the Start side in its chapters-plus-info state. A violation is reported ONCE per (code, opener
//! item) in the first state, in `Compiled::states` order, that has it; fixing the node fixes it everywhere,
//! and the finding's context names that one state. The pointer is always the opener's item.
//!
//! Pure: no IO, no global; the placer is the only thing asked.

use plx_ui::consts::{EDGE_TOL, SAFE};
use plx_ui::popover::{Anchored, Side};
use plx_ui::table::MENU_MAX_W;
use plx_ui::widgets::CTRL_FOCUS_SCALE;
use plx_ui::Rect;

use super::compile::{segment_of, CompiledState, ControlRect, SegmentAlign};
use super::lint::{ctx_for, named, slot_name, state_name, LangCtx, StateCtx};
use super::{codes, Finding};
use crate::{more_menu, track_menu};
#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};

/// Report this module's findings for the language of `cx`: each violation once per (code, opener), in the
/// first state that has it (see the module doc).
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    for state in cx.states() {
        // a Skip state whose credits pill changes the layout is a second layout of the same state
        for variant in std::iter::once(state.state).chain(state.state.skip_alt.as_deref()) {
            let here = StateCtx { key: state.key, state: variant };
            for finding in findings(cx, &here) {
                if !out.iter().any(|f| f.code == finding.code && f.pointer == finding.pointer) {
                    out.push(finding);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// The openers and where their menus go
// ---------------------------------------------------------------------------------------------------

/// The two menus a control can open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    /// `open_tracks`: the Subtitles and Audio menu
    Tracks,
    /// `open_more`: the More menu
    More,
}

impl Menu {
    /// The menu `command` opens, if it opens one.
    fn opened_by(command: &str) -> Option<Menu> {
        match command {
            "open_tracks" => Some(Menu::Tracks),
            "open_more" => Some(Menu::More),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Menu::Tracks => "Subtitles/Audio",
            Menu::More => "More",
        }
    }

    /// The menu's own `NEED_H`.
    fn need_h(self) -> f32 {
        match self {
            Menu::Tracks => track_menu::NEED_H,
            Menu::More => more_menu::NEED_H,
        }
    }

    /// The menu's own call of the shared placer.
    fn place(self, anchor: Rect, size: (f32, f32)) -> Anchored {
        match self {
            Menu::Tracks => track_menu::place_panel(anchor, size),
            Menu::More => more_menu::place_panel(anchor, size),
        }
    }
}

/// One control that opens a menu, in one state, with the anchor its menu hangs from and where the placer puts
/// the menu.
struct Opener<'a> {
    /// its index in `CompiledState::controls`
    index: usize,
    control: &'a ControlRect,
    /// the RFC 6901 pointer of its item
    pointer: &'a str,
    /// the pointer of the segment holding it
    segment: &'a str,
    menu: Menu,
    align: SegmentAlign,
    anchor: Rect,
    placed: Anchored,
}

/// Every control of `state` that opens a menu, with its menu placed.
fn openers(state: &CompiledState) -> Vec<Opener<'_>> {
    let mut out = Vec::new();
    for (index, control) in state.controls.iter().enumerate() {
        let Some(menu) = Menu::opened_by(&control.command) else { continue };
        let pointer = state.control_pointer(index);
        let segment = segment_of(pointer);
        let align = state.segment_align(segment);
        let anchor = state.menu_anchor(index).expect("an index of `controls`");
        let placed = menu.place(anchor, (MENU_MAX_W, menu.need_h()));
        out.push(Opener { index, control, pointer, segment, menu, align, anchor, placed });
    }
    out
}

// ---------------------------------------------------------------------------------------------------
// The four predicates
// ---------------------------------------------------------------------------------------------------

/// Do `a` and `b` overlap by more than a touch?
fn overlaps(a: Rect, b: Rect) -> bool {
    let both = a.intersect(b);
    both.w > EDGE_TOL && both.h > EDGE_TOL
}

/// Is `r` inside `outer`?
fn within(outer: Rect, r: Rect) -> bool {
    r.x >= outer.x - EDGE_TOL
        && r.y >= outer.y - EDGE_TOL
        && r.x + r.w <= outer.x + outer.w + EDGE_TOL
        && r.y + r.h <= outer.y + outer.h + EDGE_TOL
}

/// **M1**: does a menu panel meet its opener drawn at `rest`, at rest or popped?
fn meets_opener(panel: Rect, rest: Rect) -> bool {
    overlaps(panel, rest) || overlaps(panel, rest.scaled(CTRL_FOCUS_SCALE))
}

/// **M2**: why a placed `panel` does not lie inside `SAFE` with its full `need_h`, or `None` where it does.
fn safe_shortfall(panel: Rect, need_h: f32) -> Option<&'static str> {
    if !within(SAFE, panel) {
        Some("leaves SAFE")
    } else if panel.h + EDGE_TOL < need_h {
        Some("is cut short of its full height by the room on its side")
    } else {
        None
    }
}

/// **M3**: does `panel` lie on `side` of `anchor`, so that the rise `Side::toward_opener` displaces from `side`
/// starts at the opener?
fn on_side(anchor: Rect, panel: Rect, side: Side) -> bool {
    match side {
        Side::Above => panel.y + panel.h <= anchor.y + EDGE_TOL,
        Side::Below => panel.y >= anchor.y + anchor.h - EDGE_TOL,
        Side::Start => panel.x + panel.w <= anchor.x + EDGE_TOL,
        Side::End => panel.x >= anchor.x + anchor.w - EDGE_TOL,
    }
}

/// **M4**: the x of a panel's near edge under `align`: its right edge for an end-aligned segment, its left one
/// for a start-aligned one, none for a centred one (which promises no shared edge).
fn near_edge(panel: Rect, align: SegmentAlign) -> Option<f32> {
    match align {
        SegmentAlign::End => Some(panel.x + panel.w),
        SegmentAlign::Start => Some(panel.x),
        SegmentAlign::Center => None,
    }
}

// ---------------------------------------------------------------------------------------------------
// The findings
// ---------------------------------------------------------------------------------------------------

/// A rect as the messages spell it.
fn spell(r: Rect) -> String {
    format!("x {:.0}..{:.0}, y {:.0}..{:.0}", r.x, r.x + r.w, r.y, r.y + r.h)
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Above => "above",
        Side::Below => "below",
        Side::Start => "to the left of",
        Side::End => "to the right of",
    }
}

/// Where a finding happened: `in the compact state under the discs slot`.
fn where_(state: &StateCtx<'_>) -> String {
    format!("in the {} state under the {} slot", state_name(state.key), slot_name(state.key))
}

/// Every finding of the openers of `state`.
fn findings(cx: &LangCtx<'_>, state: &StateCtx<'_>) -> Vec<Finding> {
    let found = openers(state.state);
    let mut out = Vec::new();
    for o in &found {
        out.extend(m1(cx, state, o));
        out.extend(m2(cx, state, o));
        out.extend(m3(cx, state, o));
        // the control before it in its segment, if that opens a menu too
        let before = (0..o.index).rev().find(|&j| segment_of(state.state.control_pointer(j)) == o.segment);
        let before = before.and_then(|j| found.iter().find(|p| p.index == j));
        out.extend(before.and_then(|prev| m4(cx, state, prev, o)));
    }
    out
}

fn m1(cx: &LangCtx<'_>, state: &StateCtx<'_>, o: &Opener<'_>) -> Option<Finding> {
    if !meets_opener(o.placed.rect, o.control.rect) {
        return None;
    }
    let mut ctx = ctx_for(cx, state);
    ctx.rects = vec![named("opener", o.control.rect), named("menu", o.placed.rect)];
    Some(
        Finding::new(
            &codes::E1301,
            o.pointer,
            format!(
                "the {} menu opened from `{}` {} would cover the control itself: its panel ({}) meets the opener ({}) at rest \
                 or popped",
                o.menu.name(),
                o.control.id,
                where_(state),
                spell(o.placed.rect),
                spell(o.control.rect),
            ),
            "leave room beside the opener for the panel: a menu keeps a gap from its opener and never opens over it",
        )
        .with(ctx),
    )
}

fn m2(cx: &LangCtx<'_>, state: &StateCtx<'_>, o: &Opener<'_>) -> Option<Finding> {
    let need_h = o.menu.need_h();
    let why = safe_shortfall(o.placed.rect, need_h)?;
    let mut ctx = ctx_for(cx, state);
    ctx.rects = vec![named("menu", o.placed.rect), named("safe", SAFE)];
    Some(
        Finding::new(
            &codes::E1302,
            o.pointer,
            format!(
                "the {} menu opened from `{}` {} {why}: placed {} its opener at {}, {:.0} px tall where it needs {need_h:.0} px",
                o.menu.name(),
                o.control.id,
                where_(state),
                side_name(o.placed.side),
                spell(o.placed.rect),
                o.placed.rect.h,
            ),
            format!(
                "give `{}` room inside SAFE: {need_h:.0} px above or below its anchor, or {MENU_MAX_W:.0} px to its left or right \
                 (move the control row or the control toward the middle of the screen, or close the gaps that widen its anchor); \
                 the placer picks the side and a package cannot",
                o.control.id,
            ),
        )
        .with(ctx),
    )
}

fn m3(cx: &LangCtx<'_>, state: &StateCtx<'_>, o: &Opener<'_>) -> Option<Finding> {
    if on_side(o.anchor, o.placed.rect, o.placed.side) {
        return None;
    }
    let mut ctx = ctx_for(cx, state);
    ctx.rects = vec![named("anchor", o.anchor), named("menu", o.placed.rect)];
    Some(
        Finding::new(
            &codes::E1303,
            o.pointer,
            format!(
                "the {} menu opened from `{}` {} rises from {} its opener, but its panel ({}) is not there (anchor {})",
                o.menu.name(),
                o.control.id,
                where_(state),
                side_name(o.placed.side),
                spell(o.placed.rect),
                spell(o.anchor),
            ),
            "the rise must start at the opener: leave room for the panel on the side it opens on",
        )
        .with(ctx),
    )
}

fn m4(cx: &LangCtx<'_>, state: &StateCtx<'_>, prev: &Opener<'_>, o: &Opener<'_>) -> Option<Finding> {
    let (a, b) = (near_edge(prev.placed.rect, o.align)?, near_edge(o.placed.rect, o.align)?);
    if (a - b).abs() <= EDGE_TOL {
        return None;
    }
    let mut ctx = ctx_for(cx, state);
    ctx.rects = vec![named("menu", o.placed.rect), named("previous menu", prev.placed.rect)];
    let edge = if o.align == SegmentAlign::End { "right" } else { "left" };
    Some(
        Finding::new(
            &codes::E1303,
            o.pointer,
            format!(
                "the {} menu opened from `{}` {} hangs from a different {edge} edge (x {b:.0}) than the {} menu of `{}` next to it \
                 (x {a:.0}): the openers of one segment do not share the panel's near edge, so the menu hops between them",
                o.menu.name(),
                o.control.id,
                where_(state),
                prev.menu.name(),
                prev.control.id,
            ),
            "keep adjacent menu openers where their menus open on the same side with room: lift or lower the control row so \
             every opener has the room above or below it, or end the segment where the panels can hang from one edge",
        )
        .with(ctx),
    )
}

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[super::lint::testing::InvalidCase] = &[
    // classic lifted 130 px into the vertical middle (inset 13 -> 143): the row's top is 662, so neither 580 px
    // above nor about 270 below holds the menus' 600, and every menu opens on the START side of its opener,
    // at the opener's own left edge: Subtitles, Audio and More no longer share one edge (M4), where classic's
    // upward panels all hang from CTRL_RIGHT
    InvalidCase {
        name: "classic lifted into the middle: its three openers' panels hop",
        code: "HUD-E1303",
        pointer: "/layout/block/0/cells/1/segment/items/3",
        base: Base::Classic,
        edits: &[(r#""inset": 13,"#, r#""inset": 143,"#)],
        langs: &[],
        skew: 0.0,
        also: &[("HUD-E1303", "/layout/block/0/cells/1/segment/items/4")],
    },
    // the same lift with the row's gap opened to 440: Subtitles' anchor then runs from x 752 to CTRL_RIGHT, so
    // the Start side (628 px of room) and the End side (none) both fail to hold the 650-wide panel, and the
    // Above side's 580 px cuts it. Audio and More still open on the Start side, so their panels no longer hang
    // from the edge Subtitles' cut one does (M4): the first two `also`. The same edit is seen by two other checks,
    // which is correct (found when the packages were woven): the wide row leaves the title column too narrow for
    // the kicker's "S11, E111 · " (I10, HUD-E0601), and Up Next, which replaces the row, takes the wide gap with it
    // and its still leaves SAFE (I5, HUD-E1102): the last two `also`
    InvalidCase {
        name: "classic lifted with a wide row: no side holds the Subtitles menu",
        code: "HUD-E1302",
        pointer: "/layout/block/0/cells/1/segment/items/2",
        base: Base::Classic,
        edits: &[(r#""inset": 13,"#, r#""inset": 143,"#), (r#""id": "row", "gap": 20,"#, r#""id": "row", "gap": 440,"#)],
        langs: &[],
        skew: 0.0,
        also: &[
            ("HUD-E1303", "/layout/block/0/cells/1/segment/items/3"),
            ("HUD-E1303", "/layout/block/0/cells/1/segment/items/4"),
            ("HUD-E0601", "/layout/block/0/cells/0/vstack/cells/0/text"),
            ("HUD-E1102", "/layout/up_next"),
        ],
    },
    // the placer measures its gap from the opener's REST rect, and the opener pops (`CTRL_FOCUS_SCALE`) by
    // width x 0.07 / 2 a side: past 800 px the pop outgrows PLAYER_MENU_GAP (28 px). A 960-wide opener (the
    // `actions` cell admits `min_w` up to 1920) in the Info card's button column, lifted to y 644 by a tall
    // card (neither Above nor Below holds the menus' 600), takes the Start side: the panel stands at x 174..824
    // beside the opener at x 852..1812, and the opener's popped rect (33.6 px a side) reaches x 818.4, into it.
    // The REST rect does not overlap the panel, so only the popped one does, and nothing else in the package
    // is wrong
    InvalidCase {
        name: "a 960-wide opener pops across the gap to its menu",
        code: "HUD-E1301",
        pointer: "/layout/block/3/cells/0/rect/cells/2/actions/items/1",
        base: Base::Classic,
        edits: &[
            (r#""min_w": 352,"#, r#""min_w": 960,"#),
            (r#""bleed": 16, "h": 236,"#, r#""bleed": 16, "h": 536,"#),
            (
                r#"{ "id": "go_to_detail", "command": "go_to_detail", "widget": "button", "size": 70 }"#,
                r#"{ "id": "go_to_detail", "command": "open_more", "page": "quality", "widget": "button", "size": 70 }"#,
            ),
        ],
        langs: &[],
        skew: 0.0,
        also: &[],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case. None: M1 is reached by a 960-wide opener (see the last `INVALID` case).
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use plx_base::fontcov::advances::ShippedMeasure;
    use plx_ui::consts::SCR_H;

    use super::super::compile::{compile, widen, CompileCtx};
    use super::super::lint::testing::assert_case;
    use super::super::lint::{lint, LintLang, LintOptions};
    use super::super::parse;
    use super::super::reach::Slot;
    use super::super::BUILTIN_CLASSIC;
    use super::*;
    use crate::player_hud::classic::{CTRL_H, CTRL_RIGHT, CTRL_Y};

    const REFERENCE: &str = include_str!("fixtures/reference.plxhud.json");
    const MINIMAL: &str = include_str!("fixtures/minimal.plxhud.json");
    const HERO: &str = include_str!("fixtures/hero-center.plxhud.json");
    const TABS: &str = include_str!("fixtures/tabs.plxhud.json");
    const PALETTE: &str = include_str!("fixtures/palette.plxhud.json");

    /// Every case of [`INVALID`] yields exactly its code at its pointer, in all eleven languages.
    #[test]
    fn invalid_cases_yield_exactly_their_code() {
        assert!(!INVALID.is_empty());
        for case in INVALID {
            assert_case(case);
        }
    }

    /// The goldens: classic and the reference place both menus cleanly in every shipped language.
    #[test]
    fn classic_and_the_reference_place_both_menus_cleanly_in_all_eleven_languages() {
        let langs = LintLang::shipped();
        assert_eq!(langs.len(), 11);
        for (name, text) in [("classic", BUILTIN_CLASSIC), ("reference", REFERENCE)] {
            let found: Vec<_> = lint(text, &ShippedMeasure, &langs).into_iter().filter(|f| f.code.starts_with("HUD-E13")).collect();
            assert!(found.is_empty(), "{name}: {found:?}");
        }
    }

    /// The other four fixtures place their menus cleanly too.
    #[test]
    fn the_other_fixtures_place_both_menus_cleanly() {
        let langs = LintLang::shipped();
        for (name, text) in [("minimal", MINIMAL), ("hero-center", HERO), ("tabs", TABS), ("palette", PALETTE)] {
            let found: Vec<_> = lint(text, &ShippedMeasure, &langs).into_iter().filter(|f| f.code.starts_with("HUD-E13")).collect();
            assert!(found.is_empty(), "{name}: {found:?}");
        }
    }

    /// Run `f` over a compile of `text` in English.
    fn with_cx<R>(text: &str, f: impl FnOnce(&LangCtx<'_>) -> R) -> R {
        let lang = LintLang::for_tag("en").expect("English");
        let package = parse::parse(text.as_bytes()).package.expect("a package");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        let opts = LintOptions::default();
        f(&LangCtx { package: &package, compiled: &compiled, measure: &ShippedMeasure, lang: &lang, opts: &opts })
    }

    /// The state of the default valuation (an episode with chapters) with `open` sections under `slot`.
    fn state_of<'a>(cx: &LangCtx<'a>, open: u8, slot: Slot) -> StateCtx<'a> {
        cx.states().find(|s| s.key.open == open && s.key.slot == slot && s.key.episode && s.key.chapters).expect("the state")
    }

    /// **The classic pin.** Both menus, opened from classic's Subtitles, Audio and `…` discs, settle where the
    /// constants put them: right edge `CTRL_RIGHT`, bottom edge 764, the hugged width and height of each menu's
    /// own pins (`track_menu::classic_tracks_panels_settle_where_they_always_did`,
    /// `more_menu::classic_more_panels_settle_where_they_always_did`), on the Above side; and the widest panel
    /// is exactly the rect each menu's overscan audit reports.
    #[test]
    fn classic_places_both_menus_where_the_constants_did() {
        with_cx(BUILTIN_CLASSIC, |cx| {
            let state = state_of(cx, 0, Slot::Discs);
            let found = openers(state.state);
            assert_eq!(found.iter().map(|o| o.control.id.as_str()).collect::<Vec<_>>(), ["subs", "audio", "more"]);
            // (opener, the panel sizes of its menu's pins): (width, height)
            let pins: [(&str, &[(f32, f32)]); 3] = [
                ("subs", &[(447.0, 488.0)]),
                ("audio", &[(468.0, 356.0), (440.0, 296.0)]),
                ("more", &[(470.0, 284.0), (440.0, 536.0)]),
            ];
            let mut audit = Vec::new();
            track_menu::overscan_rects(&mut audit);
            more_menu::overscan_rects(&mut audit);
            let (tracks_audit, more_audit) = (audit[0].1, audit[1].1);
            for (o, (id, sizes)) in found.iter().zip(pins) {
                assert_eq!(o.control.id, id);
                assert_eq!(o.align, SegmentAlign::End, "{id}: classic's one segment is end-aligned");
                assert_eq!(o.anchor.x + o.anchor.w, CTRL_RIGHT, "{id}: the anchor runs to the segment's outer edge");
                assert_eq!((o.anchor.y, o.anchor.h), (CTRL_Y, CTRL_H), "{id}: the opener's own band");
                for &(w, h) in sizes {
                    let placed = o.menu.place(o.anchor, (w, h));
                    assert_eq!(placed.side, Side::Above, "{id}");
                    assert_eq!(
                        (placed.rect.x + placed.rect.w, placed.rect.y + placed.rect.h, placed.rect.w, placed.rect.h),
                        (CTRL_RIGHT, 764.0, w, h),
                        "{id} at {w} x {h}: right CTRL_RIGHT - pw, bottom 764",
                    );
                    assert_eq!(placed.rect.x, CTRL_RIGHT - w, "{id}");
                }
                let widest = o.menu.place(o.anchor, (MENU_MAX_W, SCR_H)).rect;
                assert_eq!(widest, if o.menu == Menu::Tracks { tracks_audit } else { more_audit }, "{id}: the overscan audit's rect");
                // and the whole answer for this opener is clean
                assert!(m1(cx, &state, o).is_none() && m2(cx, &state, o).is_none() && m3(cx, &state, o).is_none(), "{id}");
            }
        });
    }

    /// The reference opens More on the START side in its chapters-plus-info state (the spec's "the reference
    /// needs the Start side"), and its menus still pass.
    #[test]
    fn the_reference_opens_more_beside_the_gear_when_the_sections_are_up() {
        with_cx(REFERENCE, |cx| {
            let state = state_of(cx, super::super::compile::OPEN_CHAPTERS | super::super::compile::OPEN_INFO, Slot::Discs);
            let found = openers(state.state);
            let more = found.iter().find(|o| o.menu == Menu::More).expect("the gear");
            assert_eq!(more.placed.side, Side::Start, "{:?}", more.placed);
            assert!(found.iter().all(|o| m1(cx, &state, o).is_none() && m2(cx, &state, o).is_none() && m3(cx, &state, o).is_none()));
        });
    }

    /// The finding of the E1302 case names the opener, the state and the slot and carries the menu's and
    /// `SAFE`'s rects.
    #[test]
    fn the_m2_finding_carries_the_menu_and_safe() {
        let text = super::super::lint::testing::apply(Base::Classic, INVALID[1].edits);
        let en = [LintLang::for_tag("en").expect("English")];
        let found: Vec<_> = lint(&text, &ShippedMeasure, &en).into_iter().filter(|f| f.code == "HUD-E1302").collect();
        assert_eq!(found.len(), 1, "once per opener, however many states: {found:?}");
        let f = &found[0];
        assert_eq!(f.pointer, "/layout/block/0/cells/1/segment/items/2");
        assert_eq!(f.ctx.lang.as_deref(), Some("en"));
        assert!(f.ctx.state.is_some() && f.ctx.slot.is_some());
        let names: Vec<&str> = f.ctx.rects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["menu", "safe"]);
        assert_eq!((f.ctx.rects[1].x, f.ctx.rects[1].y, f.ctx.rects[1].w, f.ctx.rects[1].h), (SAFE.x, SAFE.y, SAFE.w, SAFE.h));
        assert!(f.message.contains("cut short"), "{}", f.message);
    }

    /// The M4 finding is on the LATER opener of a pair, with both menus' rects.
    #[test]
    fn the_m4_finding_is_on_the_later_opener_with_both_menus() {
        let text = super::super::lint::testing::apply(Base::Classic, INVALID[0].edits);
        let en = [LintLang::for_tag("en").expect("English")];
        let found: Vec<_> = lint(&text, &ShippedMeasure, &en).into_iter().filter(|f| f.code == "HUD-E1303").collect();
        let pointers: Vec<&str> = found.iter().map(|f| f.pointer.as_str()).collect();
        assert_eq!(pointers, ["/layout/block/0/cells/1/segment/items/3", "/layout/block/0/cells/1/segment/items/4"]);
        for f in &found {
            let names: Vec<&str> = f.ctx.rects.iter().map(|r| r.name.as_str()).collect();
            assert_eq!(names, ["menu", "previous menu"]);
        }
    }

    // ---- the predicates, on synthetic rects ------------------------------------------------------

    #[test]
    fn m1_a_panel_that_meets_its_opener_at_rest_or_popped_is_found() {
        let rest = Rect::new(1000.0, 800.0, 64.0, 64.0);
        let gap = 28.0;
        // above the opener with the placer's gap: clear, even of the popped rect
        assert!(!meets_opener(Rect::new(800.0, 800.0 - gap - 300.0, 264.0, 300.0), rest));
        // over the opener
        assert!(meets_opener(Rect::new(900.0, 700.0, 300.0, 300.0), rest));
        // touching the rest rect is not meeting it
        assert!(!meets_opener(Rect::new(1064.0 + 5.0, 700.0, 300.0, 300.0), rest));
        // clear of the rest rect but inside the popped one (1.07 grows 64 to 68.5)
        let popped = rest.scaled(CTRL_FOCUS_SCALE);
        assert!(popped.x < rest.x);
        assert!(meets_opener(Rect::new(popped.x - 200.0, 700.0, 200.0 + 1.0, 300.0), rest), "reaches into the pop by a pixel");
    }

    #[test]
    fn m2_a_panel_outside_safe_or_short_of_its_height_is_found() {
        let inside = Rect::new(1000.0, 200.0, 650.0, 600.0);
        assert_eq!(safe_shortfall(inside, 600.0), None);
        assert_eq!(safe_shortfall(Rect::new(SAFE.x - 1.0, 200.0, 650.0, 600.0), 600.0), Some("leaves SAFE"));
        assert_eq!(safe_shortfall(Rect::new(1000.0, SAFE.y - 1.0, 650.0, 600.0), 600.0), Some("leaves SAFE"));
        assert_eq!(safe_shortfall(Rect::new(1300.0, 200.0, 650.0, 600.0), 600.0), Some("leaves SAFE"));
        assert!(safe_shortfall(Rect::new(1000.0, 200.0, 650.0, 580.0), 600.0).is_some_and(|w| w.contains("cut short")));
    }

    #[test]
    fn m3_a_panel_on_the_wrong_side_is_found() {
        let anchor = Rect::new(1000.0, 500.0, 100.0, 64.0);
        let above = Rect::new(900.0, 100.0, 300.0, 372.0);
        let left = Rect::new(500.0, 400.0, 472.0, 264.0);
        let (below, right) = (Rect::new(900.0, 592.0, 300.0, 200.0), Rect::new(1128.0, 400.0, 300.0, 264.0));
        for (side, panel) in [(Side::Above, above), (Side::Start, left), (Side::Below, below), (Side::End, right)] {
            assert!(on_side(anchor, panel, side), "{side:?}");
        }
        // the same panels declared on the opposite side
        for (side, panel) in [(Side::Below, above), (Side::End, left), (Side::Above, below), (Side::Start, right)] {
            assert!(!on_side(anchor, panel, side), "{side:?}");
        }
    }

    #[test]
    fn m4_the_near_edge_is_the_outer_one_of_the_segment() {
        let panel = Rect::new(100.0, 100.0, 300.0, 200.0);
        assert_eq!(near_edge(panel, SegmentAlign::End), Some(400.0));
        assert_eq!(near_edge(panel, SegmentAlign::Start), Some(100.0));
        assert_eq!(near_edge(panel, SegmentAlign::Center), None);
    }

    /// The anchor is widened toward the segment's outer edge only.
    #[test]
    fn the_anchor_widens_toward_the_outer_edge_only() {
        let rest = Rect::new(500.0, 800.0, 64.0, 64.0);
        let extent = (100.0, 900.0);
        assert_eq!(widen(rest, SegmentAlign::End, extent), Rect::new(500.0, 800.0, 400.0, 64.0));
        assert_eq!(widen(rest, SegmentAlign::Start, extent), Rect::new(100.0, 800.0, 464.0, 64.0));
        assert_eq!(widen(rest, SegmentAlign::Center, extent), rest);
    }

    // ---- the findings, on a synthetic placement ---------------------------------------------------

    /// A classic opener placed by hand where the placer never would: `rect` over `o.control`.
    fn with_placement<R>(rect: Rect, side: Side, f: impl FnOnce(&LangCtx<'_>, &StateCtx<'_>, &Opener<'_>) -> R) -> R {
        with_cx(BUILTIN_CLASSIC, |cx| {
            let state = state_of(cx, 0, Slot::Discs);
            let mut found = openers(state.state);
            let mut more = found.pop().expect("the gear");
            more.placed = Anchored { rect, side };
            f(cx, &state, &more)
        })
    }

    /// **E1301 from a hand-made placement**: the finding's context (both rects, the opener's name) and its
    /// absence where the placer's own answer is clear of the opener. The parseable package that reaches it
    /// (a 960-wide opener whose pop outgrows the gap) is the last `INVALID` case.
    #[test]
    fn a_panel_over_its_opener_is_e1301() {
        let over = Rect::new(1700.0, 700.0, 200.0, 300.0);
        with_placement(over, Side::Above, |cx, state, o| {
            let f = m1(cx, state, o).expect("a finding");
            assert_eq!((f.code, f.pointer.as_str()), ("HUD-E1301", "/layout/block/0/cells/1/segment/items/4"));
            assert_eq!(f.ctx.lang.as_deref(), Some("en"));
            assert_eq!(f.ctx.slot.as_deref(), Some("discs"));
            assert_eq!(f.ctx.rects.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["opener", "menu"]);
            assert!(f.message.contains("`more`"), "{}", f.message);
        });
        // and the placer's own answer for that opener does not
        with_placement(Rect::new(1174.0, 54.0, 650.0, 710.0), Side::Above, |cx, state, o| assert!(m1(cx, state, o).is_none()));
    }

    /// **E1303 by M3**: a panel that is not on the side the placer reports. The placer never answers so, so the
    /// finding is built from a hand-made placement.
    #[test]
    fn a_panel_off_its_side_is_e1303() {
        with_placement(Rect::new(1174.0, 54.0, 650.0, 710.0), Side::Below, |cx, state, o| {
            let f = m3(cx, state, o).expect("a finding");
            assert_eq!((f.code, f.pointer.as_str()), ("HUD-E1303", "/layout/block/0/cells/1/segment/items/4"));
            assert_eq!(f.ctx.rects.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["anchor", "menu"]);
        });
        with_placement(Rect::new(1174.0, 54.0, 650.0, 710.0), Side::Above, |cx, state, o| assert!(m3(cx, state, o).is_none()));
    }
}
