//! **I10, app text fits** (`docs/hud-plugins.md`, "Text fit: the app is the enforcement point").
//!
//! App text (tab and button labels, clocks, the chapters fallback) must fit, in every language, inside its
//! box and without growing into a control or past `SAFE`. Server text (titles, chapter names, synopsis) may
//! elide or wrap and is not checked. A Skip state's credits layout is checked as well as the intro one.
//!
//! The contract every check module keeps: ONE entry, `pub(super) fn check(cx: &LangCtx<'_>, out: &mut
//! Vec<Finding>)`, called once per language after the package compiled; it walks `cx.states()` (and a Skip
//! state's `state.skip_variant(SkipKind::Credits)` where the layout differs) and pushes a finding for each
//! violation, built with `Finding::new(&codes::..)` and `.with(ctx)` where `ctx` starts from
//! `lint::ctx_for(cx, &state)` and adds its `rects` (`lint::named`) and `text`. The pointer is the node's
//! (`CompiledState::origin(op)` / `control_pointer(control)`). Pure: no IO, no globals.
//!
//! **Status: contract stub.** `check` reports nothing yet; the package that owns `HUD-E0601` text-fit and
//! `HUD-W0602` text-fit-margin fills it in, with its invalid cases in [`INVALID`] and, for a code no
//! parseable package can reach, its reason in [`UNIT_ONLY`] beside a synthetic unit test.

use super::lint::LangCtx;
use super::Finding;

/// **`CI_TEXT_SKEW`** in px: the measurement skew between the pinned simulator and the TV that registry CI
/// requires app text to fit with to spare (`HUD-W0602`, a warning locally, an error in registry CI)
/// (`docs/hud-plugins.md`, "Text fit: the app is the enforcement point", item (c) Headroom: "Until it is
/// measured the margin is 0 and the TV decides"). PR12 measures it; until then it is `0.0`. It is named so
/// that it does not match `MAX_*`: it is a measured margin, not a bound on a list.
pub(super) const CI_TEXT_SKEW: f32 = 0.0;

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
