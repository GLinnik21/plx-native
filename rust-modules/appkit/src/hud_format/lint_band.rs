//! **I6, the caption band** (`docs/hud-plugins.md`, "Captions: the band a HUD occupies").
//!
//! In the compact state under every slot, `Compiled::band()` respects `player_hud::min_controls_top_y()`.
//!
//! The contract every check module keeps: ONE entry, `pub(super) fn check(cx: &LangCtx<'_>, out: &mut
//! Vec<Finding>)`, called once per language after the package compiled; it walks `cx.states()` (and a Skip
//! state's `state.skip_variant(SkipKind::Credits)` where the layout differs) and pushes a finding for each
//! violation, built with `Finding::new(&codes::..)` and `.with(ctx)` where `ctx` starts from
//! `lint::ctx_for(cx, &state)` and adds its `rects` (`lint::named`) and `text`. The pointer is the node's
//! (`CompiledState::origin(op)` / `control_pointer(control)`). Pure: no IO, no globals.
//!
//! **Status: contract stub.** `check` reports nothing yet; the package that owns `HUD-E1201` caption-band
//! fills it in, with its invalid cases in [`INVALID`] and, for a code no parseable package can reach, its
//! reason in [`UNIT_ONLY`] beside a synthetic unit test.

use super::lint::LangCtx;
use super::Finding;

/// Report this module's findings for the language of `cx`.
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    let _ = (cx, out);
}

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[super::lint::testing::InvalidCase] = &[];

/// `(code, reason)` for a code of this module that no parseable package can reach, covered by a synthetic
/// unit test instead of an invalid case.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];
