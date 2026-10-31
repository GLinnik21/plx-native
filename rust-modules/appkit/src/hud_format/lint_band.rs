//! **I6, the caption band** (`docs/hud-plugins.md`, "Captions: the band a HUD occupies" and "Invariants" I6).
//!
//! In the compact state under every slot, `Compiled::band()` respects `player_hud::min_controls_top_y()`.
//!
//! **The rule, from the spec's Captions section:** "Bound: `controls_top_y >= MIN_CONTROLS_TOP_Y`, the highest
//! top for which a caption block of `CAPTION_ANCHOR_LINES` lines at the largest `SubtitleSize` and the highest
//! `SubtitlePosition` still fits between `SAFE`'s top and the lifted baseline". The bound is INCLUSIVE: a band
//! whose `controls_top_y` equals the minimum passes, one whose top is any higher ([`violates`]) is
//! `HUD-E1201`. `player_hud::min_controls_top_y` is the plain fn the spec names (it follows `caption_layout`,
//! `size_scale` and `position_lift`), so this check holds no number of its own.
//!
//! **One band, one finding.** The band is the package's (`Compiled::band`, the same value in every state): the
//! top of the highest drawn interactive extent (under `band: "block"`, of any drawn extent) of the compact
//! state, over its three slots and every valuation. So a package violates the bound once, and the finding
//! points at the node that SETS the band, the highest such extent: a control's item, the scrub bar's node,
//! `/layout/up_next` for Up Next's buttons or the Skip pill, or, under `band: "block"`, whatever node drew
//! the highest extent. Its context names the compact state and slot the extent was found in and carries three
//! rects: the `extent` itself, the `band` (from the package's `controls_top_y` down) and the `limit` (from the
//! minimum down). Fixing that node reveals the next-highest extent if it too is above the minimum.
//!
//! **Where the pointer comes from.** The band is the minimum `top` of `Compiled::compact_band_extents`, the one
//! iterator `compile` takes it from, and each extent it yields carries the node that drew it (the provenance
//! accessors `CompiledState::origin` and `control_pointer`, and the `up_next` object for the stand-ins). So
//! [`highest`] decides WHICH node by reading the very extents the band was taken from, never WHETHER the
//! bound is violated: that is `Compiled::band()` alone. If no extent sits at the band's top (it cannot, but a
//! band is a plain value) the finding falls back to `/layout`, the layout root, so the violation is still
//! reported.

use plx_ui::consts::{EDGE_TOL, SCR_H, SCR_W};
use plx_ui::Rect;

use super::compile::BandExtent;
use super::lint::{ctx_for, named, LangCtx, StateCtx};
use super::{codes, Finding};
use crate::player_hud::min_controls_top_y;
#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};

/// The layout root: the pointer of a finding no extent could be named for (see the module doc).
const LAYOUT_ROOT: &str = "/layout";

/// Report this module's findings for the language of `cx`.
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    let band_top = cx.compiled.band().controls_top_y;
    let limit = min_controls_top_y();
    if violates(band_top, limit) {
        out.push(finding(cx, band_top, limit, highest(cx)));
    }
}

/// Is a band whose controls start at `controls_top_y` above the highest top `limit` allows? The spec's bound
/// is `controls_top_y >= limit`, so equality passes.
fn violates(controls_top_y: f32, limit: f32) -> bool {
    controls_top_y < limit
}

/// The highest extent of the compact state over its slots and valuations (the first, in `Compiled::states`
/// order and then the order `CompiledState::band_extents` yields, where several share the top), and the state
/// it was found in.
fn highest<'a>(cx: &LangCtx<'a>) -> Option<(BandExtent<'a>, StateCtx<'a>)> {
    cx.compiled
        .compact_band_extents()
        .min_by(|a, b| a.1.top.total_cmp(&b.1.top))
        .map(|(state, extent)| (extent, StateCtx { key: state.key, state }))
}

/// The `HUD-E1201` finding for a band whose controls start at `band_top`, above `limit`, set by `found`
/// (see [`highest`]). A scan that missed the band's top (`found` is `None`, or sits elsewhere) names the layout
/// root in the first compact state instead.
fn finding(cx: &LangCtx<'_>, band_top: f32, limit: f32, found: Option<(BandExtent<'_>, StateCtx<'_>)>) -> Finding {
    let found = found.filter(|(extent, _)| (extent.top - band_top).abs() < EDGE_TOL);
    let state = found
        .as_ref()
        .map(|(_, s)| *s)
        .or_else(|| cx.states().find(|s| s.key.open == 0))
        .or_else(|| cx.states().next());
    let (pointer, what) = found.as_ref().map_or((LAYOUT_ROOT, "extent"), |(e, _)| (e.pointer, e.what));
    let mut ctx = state.map(|s| ctx_for(cx, &s)).unwrap_or_default();
    if let Some((extent, _)) = &found {
        ctx.rects.push(named("extent", extent.rect));
    }
    ctx.rects.push(named("band", Rect::new(0.0, band_top, SCR_W, SCR_H - band_top)));
    ctx.rects.push(named("limit", Rect::new(0.0, limit, SCR_W, SCR_H - limit)));
    Finding::new(
        &codes::E1201,
        pointer,
        format!(
            "the highest {what} of the compact state starts at y {band_top}, {by} px above y {limit}, the highest top at which \
             the largest caption block still fits under SAFE: captions lifted above this HUD would run out of the safe area",
            by = limit - band_top,
        ),
        format!(
            "keep every {kind} of the compact state at y {limit} or lower: a smaller layout.inset, or less stacked above \
             the control row",
            kind = if found.as_ref().is_some_and(|(e, _)| e.what != "control") { "extent the band counts" } else { "control" },
        ),
    )
    .with(ctx)
}

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[super::lint::testing::InvalidCase] = &[
    // classic with its whole block lifted 200 px (inset 13 -> 213): the control row's top, 792, becomes 592,
    // above the highest top (about 645) a three-line caption at the largest size and position still fits under.
    // A row whose top is above the bound has less than 600 px above it (the menus' `NEED_H`) however it got
    // there, so its Audio and More menus leave the Above side and stop sharing an edge with the Subtitles one:
    // the two `also` are that consequence (`lint_menus`, M4), not a second defect
    InvalidCase {
        name: "classic lifted: the control row's top is above the caption bound",
        code: "HUD-E1201",
        pointer: "/layout/block/0/cells/1/segment/items/2",
        base: Base::Classic,
        edits: &[(r#""inset": 13,"#, r#""inset": 213,"#)],
        langs: &[],
        skew: 0.0,
        also: &[
            ("HUD-E1303", "/layout/block/0/cells/1/segment/items/3"),
            ("HUD-E1303", "/layout/block/0/cells/1/segment/items/4"),
        ],
    },
    // classic under `band: "block"` with a 354 px title: the band is the top of ANY drawn extent, and the
    // title block (a 34 px kicker over a 354 px title, bottom-aligned in its band) now rises to y 468, above
    // every language's Up Next still (about 550), while the control row stays at 792: only the kicker's text
    // sets the band and is named
    InvalidCase {
        name: "classic with a block band and a tall title: a text rises above the caption bound",
        code: "HUD-E1201",
        pointer: "/layout/block/0/cells/0/vstack/cells/0/text",
        base: Base::Classic,
        edits: &[(r#""band": "controls","#, r#""band": "block","#), (r#""leading": 54,"#, r#""leading": 354,"#)],
        langs: &[],
        skew: 0.0,
        also: &[],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case. `HUD-E1201` has an invalid case.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use plx_base::fontcov::advances::ShippedMeasure;

    use super::super::compile::{compile, CompileCtx, DrawOp};
    use super::super::lint::testing::{apply, assert_case};
    use super::super::lint::{lint, LangCtx, LintLang, LintOptions};
    use super::super::parse::{self, Val};
    use super::super::reach::Slot;
    use super::super::BUILTIN_CLASSIC;
    use super::*;

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

    /// Every case of [`INVALID`] yields exactly its code at its pointer, in all eleven languages.
    #[test]
    fn invalid_cases_yield_exactly_their_code() {
        assert!(!INVALID.is_empty());
        for case in INVALID {
            assert_case(case);
        }
    }

    /// The goldens: classic and the reference have no caption-band finding in any shipped language.
    #[test]
    fn classic_and_the_reference_respect_the_band_in_all_eleven_languages() {
        let langs = LintLang::shipped();
        assert_eq!(langs.len(), 11);
        for (name, text) in [("classic", BUILTIN_CLASSIC), ("reference", REFERENCE)] {
            let found: Vec<_> = lint(text, &ShippedMeasure, &langs).into_iter().filter(|f| f.code == "HUD-E1201").collect();
            assert!(found.is_empty(), "{name}: {found:?}");
        }
    }

    /// The other four fixtures are inside the bound too (a finding here would have to be listed, with its
    /// reason, in the handoff).
    #[test]
    fn the_other_fixtures_respect_the_band() {
        let langs = LintLang::shipped();
        for (name, text) in FIXTURES {
            let found: Vec<_> = lint(text, &ShippedMeasure, &langs).into_iter().filter(|f| f.code == "HUD-E1201").collect();
            assert!(found.is_empty(), "{name}: {found:?}");
        }
    }

    /// The bound is inclusive (`controls_top_y >= MIN_CONTROLS_TOP_Y`): the minimum itself passes, a pixel higher
    /// does not.
    #[test]
    fn the_bound_is_inclusive() {
        let limit = min_controls_top_y();
        assert!(!violates(limit, limit));
        assert!(!violates(limit + 1.0, limit));
        assert!(violates(limit - 1.0, limit));
        assert!(violates(limit - 0.01, limit));
    }

    /// Run `f` over a compile of `text` in English.
    fn with_cx<R>(text: &str, f: impl FnOnce(&LangCtx<'_>) -> R) -> R {
        let lang = LintLang::for_tag("en").expect("English");
        let package = parse::parse(text.as_bytes()).package.expect("a package");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        let opts = LintOptions::default();
        f(&LangCtx { package: &package, compiled: &compiled, measure: &ShippedMeasure, lang: &lang, opts: &opts })
    }

    /// **Every extent names a node**, in every state of every fixture, with and without `band: "block"`: a
    /// finding is only as good as the pointer the extent carries (the stand-ins', the bar's, a control's). An
    /// extent whose provenance was not recorded would come out as `""`.
    #[test]
    fn every_band_extent_names_a_node_on_every_fixture() {
        for (name, text) in FIXTURES {
            with_cx(text, |cx| {
                for state in cx.compiled.states() {
                    for block in [false, true] {
                        for extent in state.band_extents(block) {
                            assert!(extent.pointer.starts_with("/layout"), "{name}: {block}: {extent:?}");
                        }
                    }
                }
            });
        }
    }

    /// **A rule is named a rule**, not a ground: under `band: "block"` an E1201 set by a rule says so (the
    /// reference draws rules, so the walk is not vacuous).
    #[test]
    fn a_rule_is_named_a_rule_and_not_a_ground() {
        let mut rules = 0;
        for (name, text) in FIXTURES {
            with_cx(text, |cx| {
                for state in cx.compiled.states() {
                    let extents: Vec<_> = state.band_extents(true).collect();
                    for (i, op) in state.ops.iter().enumerate() {
                        let DrawOp::Rule { rect, .. } = op else { continue };
                        rules += 1;
                        let whats: Vec<&str> =
                            extents.iter().filter(|e| e.rect == *rect && e.pointer == state.origin(i)).map(|e| e.what).collect();
                        assert_eq!(whats, ["rule"], "{name}: the extent of the rule at {}", state.origin(i));
                    }
                }
            });
        }
        assert!(rules > 0, "a fixture draws a rule");
    }

    /// Up Next's buttons and the Skip pill are named by the stand-in's node (`/layout/up_next`), the scrub bar by
    /// its own cell, a control by its item.
    #[test]
    fn extents_name_the_node_that_drew_them() {
        with_cx(BUILTIN_CLASSIC, |cx| {
            let state = |slot| {
                cx.states().find(|s| s.key.open == 0 && s.key.slot == slot && s.key.episode && s.key.chapters).expect("state").state
            };
            let extents = |slot| state(slot).band_extents(false).collect::<Vec<_>>();
            let discs = extents(Slot::Discs);
            let more = discs.iter().find(|e| e.pointer == "/layout/block/0/cells/1/segment/items/4").expect("More");
            assert_eq!((more.what, more.top), ("control", 792.0));
            let bar = discs.iter().find(|e| e.what == "scrub bar").expect("the bar");
            assert!(bar.pointer.starts_with("/layout/block/1/cells/0/playbar"), "{}", bar.pointer);
            assert!(bar.top < bar.rect.y + bar.rect.h);
            let up = extents(Slot::UpNext);
            assert!(up.iter().any(|e| e.what == "Up Next button" && e.pointer == "/layout/up_next"), "{up:?}");
            let skip = extents(Slot::Skip);
            assert!(skip.iter().any(|e| e.what == "Skip pill" && e.pointer == "/layout/up_next"), "{skip:?}");
        });
    }

    /// The finding of the invalid case: the compact state and slot it was found in, and the three rects.
    #[test]
    fn the_finding_names_the_state_and_carries_the_extent_the_band_and_the_limit() {
        let text = apply(Base::Classic, INVALID[0].edits);
        let found: Vec<_> = lint(&text, &ShippedMeasure, &[LintLang::for_tag("en").expect("English")])
            .into_iter()
            .filter(|f| f.code == "HUD-E1201")
            .collect();
        assert_eq!(found.len(), 1, "one band, one finding: {found:?}");
        let ctx = &found[0].ctx;
        assert_eq!(ctx.lang.as_deref(), Some("en"));
        assert!(ctx.state.as_deref().is_some_and(|s| s.starts_with("compact")), "{:?}", ctx.state);
        assert_eq!(ctx.slot.as_deref(), Some("discs"));
        let names: Vec<&str> = ctx.rects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["extent", "band", "limit"]);
        let limit = min_controls_top_y();
        assert_eq!(ctx.rects[0].y, 592.0, "the control row, lifted 200 px from classic's 792");
        assert_eq!(ctx.rects[1].y, 592.0, "the band starts where the extent does");
        assert_eq!(ctx.rects[2].y, limit);
        assert_eq!(ctx.rects[2].h, SCR_H - limit);
        assert!(found[0].message.contains("592") && found[0].message.contains(&limit.to_string()), "{}", found[0].message);
    }

    /// A scan that found no extent at the band's top still reports the violation, at the layout root.
    #[test]
    fn a_scan_that_misses_the_band_names_the_layout_root() {
        with_cx(BUILTIN_CLASSIC, |cx| {
            let limit = min_controls_top_y();
            let f = finding(cx, limit - 10.0, limit, None);
            assert_eq!((f.code, f.pointer.as_str()), ("HUD-E1201", "/layout"));
            assert!(f.ctx.rects.iter().map(|r| r.name.as_str()).eq(["band", "limit"]));
            // an extent that is not at the band's top is not the one that set it
            let elsewhere = highest(cx);
            let f = finding(cx, limit - 10.0, limit, elsewhere);
            assert_eq!(f.pointer, "/layout", "classic's row is not at limit - 10");
        });
    }

    /// Under `band: "block"` any drawn extent counts, so a text above the row sets the band and is named.
    #[test]
    fn under_a_block_band_a_text_can_set_the_band() {
        // the reference declares `band: "block"`: its highest compact extent is the title block
        with_cx(REFERENCE, |cx| {
            assert_eq!(cx.package.layout.get("band").and_then(Val::as_str), Some("block"));
            let (extent, _) = highest(cx).expect("an extent");
            assert!(["text", "image", "ground", "rule", "control"].contains(&extent.what), "{extent:?}");
            assert!(extent.top <= 792.0 + 400.0);
        });
    }
}
