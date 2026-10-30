//! **`MIN_CONTROL_SIZE`, and the compile's item-keyed finding** (`docs/hud-plugins.md`, "Package format
//! 1.0", the `MIN_CONTROL_SIZE` paragraph).
//!
//! A focusable control smaller than `MIN_CONTROL_SIZE` (derived from the smallest focusable control the
//! app's own screens place) is `HUD-E0603`; `Compiled::findings()` (a number or gap keyed on a strip card's
//! `item.*` term) is mapped onto `HUD-E0604` and printed with the rest.
//!
//! The contract every check module keeps: ONE entry, `pub(super) fn check(cx: &LangCtx<'_>, out: &mut
//! Vec<Finding>)`, called once per language after the package compiled; it walks `cx.states()` (and a Skip
//! state's `state.skip_variant(SkipKind::Credits)` where the layout differs) and pushes a finding for each
//! violation, built with `Finding::new(&codes::..)` and `.with(ctx)` where `ctx` starts from
//! `lint::ctx_for(cx, &state)` and adds its `rects` (`lint::named`) and `text`. The pointer is the node's
//! (`CompiledState::origin(op)` / `control_pointer(control)`). Pure: no IO, no globals.
//!
//! **Status: contract stub.** `check` reports nothing yet; the package that owns `HUD-E0603` min-control-
//! size and `HUD-E0604` compile-item fills it in, with its invalid cases in [`INVALID`] and, for a code no
//! parseable package can reach, its reason in [`UNIT_ONLY`] beside a synthetic unit test.

use super::lint::LangCtx;
use super::Finding;

// MIN_CONTROL_SIZE lands here, derived from the app's own smallest focusable control with its file:line
// cited, admitting the reference's 52 and pinned in a test.

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
