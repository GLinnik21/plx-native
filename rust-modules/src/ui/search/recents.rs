//! The empty-query state: the user's own recent searches, and the control that clears them.
//!
//! ## The design
//!
//! Rows, not a shelf: nothing here has artwork. They use TableView's own geometry on the app's
//! ground instead of inside a panel — [`table::ROW_H`] tall, [`table::SIDE`] margins,
//! [`table::CONTENT_X`] inside, the focus pill inset by [`table::PILL_INSET`], HEADLINE labels.
//! Every one of those numbers is IMPORTED from that widget rather than restated here, so a row
//! height change there moves this block with it (it did not until 2026-08-14, and the mismatch was
//! invisible: [`BLOCK_BOTTOM`] is what the keyboard-clearance test pins, and it was derived from
//! this module's own copies).
//! Written as markup rather than mounted as a [`table::TableView`] for one reason, and
//! it is worth keeping: **these rows are the user's own words and have to stay editable in place.**
//!
//! Above them sits a section HEADER, not a heading — it names the source of the rows, so it sits a
//! full step BELOW their labels: CAPTION, caps, tertiary, on the same [`table::HDR_H`] band
//! TableView reserves. At HEADLINE it read as another row.
//!
//! It is **placed by cap band and not by TableView's number.** That widget draws its own header at
//! a raw `sy + 8.0` — the magic y `ui/CLAUDE.md` rule 3 bans outright — so copying it here would
//! spread the one thing the rule exists to stop. The two therefore sit ~6px apart. The fix is to
//! move `table.rs` onto [`Label`] as well, not to re-add the offset on this side; until then this
//! is the one that is right.
//!
//! Clearing is a **control, not another term**: it leaves the list and becomes a
//! [`crate::ui::widgets::Button`], so a verb never sits in the same column as the words you
//! searched for.
//!
//! **Four terms** ([`super::MAX_RECENTS`]). With the keyboard raised the header, the rows and the
//! Clear control all have to finish above its top edge (`SCR_H - super::KEYBOARD_H` = 756); this
//! block ends at [`BLOCK_BOTTOM`], which a host test pins against that line. The fifth is DROPPED,
//! not scrolled — a list you cannot see the end of asks to be paged, and there is no paging in this
//! product.
//!
//! It was FOUR until 2026-08-15, and the cap was not the thing that was wrong: `KEYBOARD_H` was a
//! guess 56px too tall, so the arithmetic said a fifth row would not clear a panel edge that does
//! not exist. Measuring the real panel put the fifth back with 66px to spare. The lesson is in the
//! test below rather than in the number — it grades the CLEARANCE, so the count is free to follow
//! whatever the panel actually does.
//! The later document-head expansion reduced the current capacity to four again; the shared
//! data cap and the clearance test below keep storage and layout in agreement.
//!
//! Data and worker persistence live in `crate::search::recents`. This legacy rendering
//! module keeps only geometry and a glyph cache keyed on an immutable publication.
#![allow(dead_code)]

use crate::ui::label::Label;
use crate::ui::search::{View, Zone};
use crate::ui::table;
use crate::ui::widgets::Button;
// anonymous: this screen's own `View` is the per-frame SNAPSHOT above, and the retui trait of the
// same name is wanted only so `Button::draw` resolves
use crate::ui::View as _;
use crate::ui::{theme, Env, Painter, Rect};
use std::ffi::CString;
use std::sync::{Mutex, OnceLock};

// ---- Geometry ---------------------------------------------------------------------------------
//
// The block is the FIELD's own column: same left edge, same width, so the rows line up under the
// thing that produced them. Its ROW geometry is `table.rs`'s, imported — `table::ROW_H` /
// `table::PILL_INSET` / `table::SIDE` / `table::PILL_RAD` / `table::HDR_H` / `table::CONTENT_X`,
// all `pub` for this caller shape. See the module doc for why these rows are drawn rather than
// mounted, and why the numbers are borrowed rather than copied.

/// Header caps, pre-uppercased. The header is a constant here (TableView's `to_uppercase` exists
/// because its headers are runtime machine and library names).
/// `pub(super)` for [`super::empty`], which draws this same header over its own empty state — the
/// list keeps its name when it has nothing in it, and two spellings of one header is exactly the
/// drift that would make them look like two regions.
pub(super) const HDR: &std::ffi::CStr = c"RECENT SEARCHES";
/// The Clear control's label and the air above it. A `space::MD` rung, not a hand-tuned gap: it
/// separates two different KINDS of thing (the list, then a verb), which is exactly the rung's job.
const CLEAR: &std::ffi::CStr = c"Clear recent searches";
const CLEAR_GAP: f32 = theme::space::MD;

/// The block's left edge and width. `x` is the field's column; the WIDTH is its own number and no
/// longer the field's, because the field became the full content width when its capsule went and a
/// list of short terms does not want 1728px of row. 820 is the design's, and it is what the field
/// itself used to be.
const BLOCK_X: f32 = super::FIELD.x;
const BLOCK_W: f32 = 820.0;
/// Where a row's own content starts. `table::CONTENT_X` is public for exactly this: a caller that
/// draws beside a list starting its text on the same line the rows do, rather than re-deriving two
/// private constants and drifting from them.
const TEXT_X: f32 = BLOCK_X + table::CONTENT_X;
/// The width a row's label is elided to — the block inset by the row's own content padding on both
/// sides, which is the same box `TableView` gives a label with no accessories.
const TEXT_W: f32 = BLOCK_W - 2.0 * table::CONTENT_X;
/// First row's top: the header band is reserved whether or not anything is under it.
const ROWS_TOP: f32 = super::CONTENT_TOP + table::HDR_H;
/// The bottom edge of the Clear control at a FULL list — the number the four-term cap exists to
/// keep under the raised keyboard's top edge. Asserted by a host test, not by the eye.
const BLOCK_BOTTOM: f32 = ROWS_TOP + super::MAX_RECENTS as f32 * table::ROW_H + CLEAR_GAP + CLEAR_H;
/// The Clear control's own height — one control height, and no longer `FIELD.h`: the field is a
/// line box now (80px for a 72px run), not a control, so reading its height here would have
/// measured this block against the wrong object.
const CLEAR_H: f32 = 60.0;

// ---- Legacy data access; writes still enter through the store command vocabulary --------------
pub(crate) fn count() -> usize { crate::search::recents::snapshot().terms().len() }
pub(crate) fn terms() -> Vec<String> { crate::search::recents::snapshot().terms().to_vec() }
pub(crate) fn remember(term: &str) {
    crate::stores::search::apply(crate::stores::search::SearchCmd::RememberRecent {
        profile_generation: crate::search::recents::snapshot().generation(), term: term.into(),
    });
}
pub(crate) fn clear() {
    crate::stores::search::apply(crate::stores::search::SearchCmd::ClearRecents {
        profile_generation: crate::search::recents::snapshot().generation(),
    });
}

// Glyphs are render state, never part of the profile data store. Hold one publication alongside
// its baked rows so a same-frame data edit cannot make row indices name another term.
struct Runs {
    publication: crate::search::recents::RecentsSnapshot,
    rows: Vec<CString>,
}
static RUNS: Mutex<Option<Runs>> = Mutex::new(None);

fn with_runs<R>(publication: crate::search::recents::RecentsSnapshot, f: impl FnOnce(&[CString]) -> R) -> R {
    let mut cache = RUNS.lock().unwrap_or_else(|e| e.into_inner());
    f(runs_for(&mut cache, publication, bake))
}

fn runs_for(cache: &mut Option<Runs>, publication: crate::search::recents::RecentsSnapshot,
    bake: impl FnOnce(&[String]) -> Vec<CString>) -> &[CString] {
    if cache.as_ref().map_or(true, |c| !c.publication.same_publication(&publication)) {
        *cache = Some(Runs { rows: bake(publication.terms()), publication });
    }
    &cache.as_ref().expect("glyph cache is populated").rows
}

/// Main-thread draw only. One NUL-terminated, elided run per term, rebuilt only on publication.
fn bake(terms: &[String]) -> Vec<CString> {
    terms
        .iter()
        .map(|t| {
            CString::new(crate::text::elide(
                t,
                TEXT_W,
                theme::size::HEADLINE,
                1,
                false,
            ))
            .unwrap_or_default()
        })
        .collect()
}


// ---- The drawing ------------------------------------------------------------------------------

/// Does the Clear control hold focus? Its index is [`super::MAX_RECENTS`] — one past the LAST
/// possible term — but the test is `>= shown` so that a list of two terms still lands the ring on
/// the control rather than on nothing, whichever of the two the state machine's cursor names.
fn clear_focused(v: &View, shown: usize) -> bool {
    v.zone == Zone::Recents && v.recent >= shown
}

/// The Clear pill's width, measured ONCE for the life of the app.
///
/// `Button::pill_w` ends in a real `TTF_SizeUTF8`, and [`CLEAR`] is a compile-time constant — so
/// the per-frame call was a font-engine round trip for a number that cannot change. A `const`
/// cannot hold it: the answer comes out of the font, which does not exist until `init_text` runs,
/// which is why this is a `OnceLock` filled on the first draw.
fn clear_w() -> f32 {
    static W: OnceLock<f32> = OnceLock::new();
    *W.get_or_init(|| Button::pill_w(CLEAR.as_ptr(), theme::size::BODY, false))
}

pub(crate) fn draw(p: Painter, v: &View) {
    with_runs(crate::search::recents::snapshot(), |runs| draw_block(p, v, runs));
}

/// `rows` is [`bake`]'s output — one run per stored term, in the same order, so `i` here is the same
/// `i` the focus cursor and the hit rects use.
fn draw_block(p: Painter, v: &View, rows: &[CString]) {
    let shown = rows.len().min(super::MAX_RECENTS);
    if shown == 0 {
        return;
    }

    // **This block is in the document too**, like the field above it and the shelves that replace
    // it (2026-09-05). At rest the shift is zero and always will be for as long as the recents list
    // is what is drawn — `super::scroll_frozen` holds the flow at zero whenever the shelves do not
    // hold focus, and they cannot hold it while this region exists. What the translate is for is
    // the frame the two regions CHANGE PLACES on: a store emptied under a scrolled screen (a
    // profile switch, a re-query landing) re-seats focus to the field and springs the flow back to
    // zero over the next few hundred ms, and a block pinned at `CONTENT_TOP` would appear at its
    // resting place while the field it belongs under was still off the top of the panel.
    //
    // The rects are recorded through `note` so the shift is applied ONCE: a drawer that translated
    // its painter and left `note_recent_rect` taking flow coordinates is the same defect one layer
    // down — a row you can see here and click there.
    let p = p.translate(0.0, -v.shift);
    let note = |i: usize, r: Rect| {
        super::note_recent_rect(i, Rect::new(r.x, r.y - v.shift, r.w, r.h));
    };

    // The header: a step BELOW the rows it names, on its own fixed band.
    Label::new(HDR.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .draw(p, Rect::new(TEXT_X, super::CONTENT_TOP, 0.0, table::HDR_H));

    // The rows. One elided HEADLINE-bold run, cap-band-centred in the row box — TableView's own
    // single-line label path, because these are the same rows on a different ground.
    for (i, run) in rows.iter().take(shown).enumerate() {
        let ry = ROWS_TOP + i as f32 * table::ROW_H;
        let focused = v.zone == Zone::Recents && v.recent == i;
        if focused {
            let pill = Rect::new(
                BLOCK_X + table::SIDE,
                ry + table::PILL_INSET,
                BLOCK_W - 2.0 * table::SIDE,
                table::ROW_H - 2.0 * table::PILL_INSET,
            );
            p.rrect(pill, table::PILL_RAD, table::PILL_RAD, crate::ui::ACCENT);
        }
        // The whole row box, not the pill: a term is clickable across the band it occupies,
        // and the pill is only drawn when it is focused. Without this the recents list answered
        // to the d-pad alone — `mod.rs::hit` scans an array `draw` parks every frame and no
        // drawer ever filled, so every click missed.
        note(i, Rect::new(BLOCK_X, ry, BLOCK_W, table::ROW_H));
        let ink = if focused {
            crate::ui::ACCENT_INK
        } else {
            theme::TEXT_PRIMARY
        };
        Label::new(run.as_ptr(), theme::size::HEADLINE, ink)
            .bold()
            .draw(p, Rect::new(TEXT_X, ry, 0.0, table::ROW_H));
    }

    // Clearing is a CONTROL: it leaves the column of words and becomes the shared pill, at the
    // rows' own text x so it reads as belonging to the block without sitting in their column.
    let by = ROWS_TOP + shown as f32 * table::ROW_H + CLEAR_GAP;
    let cr = Rect::new(TEXT_X, by, clear_w(), CLEAR_H);
    // Clear sits at `MAX_RECENTS`, one past the last term it could ever follow — the index
    // `mod.rs` reserves for it whatever `shown` turns out to be.
    note(super::MAX_RECENTS, cr);
    Button::new(CLEAR.as_ptr(), theme::size::BODY, cr)
        .focused(clear_focused(v, shown))
        .draw(&Env::inert(), p);
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_runs_follow_publication_identity_and_settle_without_rebaking() {
        use crate::search::recents::RecentsSnapshot;
        let calls = std::cell::Cell::new(0);
        let bake = |terms: &[String]| {
            calls.set(calls.get() + 1);
            terms.iter().map(|t| CString::new(t.as_str()).unwrap()).collect()
        };
        let first = RecentsSnapshot::fixture(1, vec!["alpha".into(), "beta".into()]);
        let mut cache = None;
        let rows = runs_for(&mut cache, first.clone(), bake);
        assert_eq!(rows.iter().map(|r| r.to_str().unwrap()).collect::<Vec<_>>(), ["alpha", "beta"]);
        assert_eq!(calls.get(), 1);
        runs_for(&mut cache, first.clone(), bake);
        assert_eq!(calls.get(), 1, "the actual cache path does not rebake an unchanged publication");
        let second = RecentsSnapshot::fixture(2, vec!["gamma".into()]);
        assert_eq!(runs_for(&mut cache, second.clone(), bake)[0].to_str().unwrap(), "gamma");
        assert_eq!(calls.get(), 2);
        assert_eq!(runs_for(&mut cache, second, bake).len(), 1);
        assert_eq!(calls.get(), 2);
        assert_eq!(first.terms(), &["alpha", "beta"]);
    }
    /// The layout rule the term cap exists to satisfy: with the keyboard raised, nothing this
    /// screen owns may hide behind it. Graded here rather than by eye, because the failure is a
    /// control the user cannot see or reach — and every term in the design's own copy is short, so
    /// the case that breaks is a full list, which a screenshot of a fresh install never shows.
    ///
    /// **This asserts the clearance, not the count, and that is what made the count fixable.** The
    /// cap was four against a `KEYBOARD_H` guessed 56px too tall; when the panel was finally
    /// measured, the fifth row simply passed. A test written as `MAX_RECENTS == 4` would have had
    /// to be edited to allow the fix, which is the difference between a test and a restatement.
    ///
    /// The clearance floor is the second half of the claim: the block must not merely *fit*, it has
    /// to sit a block gap clear of the panel, or the Clear control reads as attached to a piece of
    /// television chrome it has nothing to do with.
    #[test]
    fn a_full_block_finishes_clear_of_the_raised_keyboard() {
        assert_eq!(crate::search::recents::CAP, super::super::MAX_RECENTS,
            "the store keeps exactly what the screen shows");
        let kbd_top = crate::ui::consts::SCR_H - crate::ui::search::KEYBOARD_H;
        let clearance = kbd_top - BLOCK_BOTTOM;
        assert!(clearance >= theme::space::LG,
            "a full block ends at {BLOCK_BOTTOM} and the keyboard starts at {kbd_top} — {clearance}px");
    }

    /// Focus never falls off the end of a SHORT list: with two terms stored, the state machine's
    /// cursor at `MAX_RECENTS` — and at every index in between — lands on the Clear control.
    #[test]
    fn the_clear_control_takes_focus_past_the_last_shown_term() {
        let v = |zone, recent| View {
            zone,
            editing: false,
            row: 0,
            col: 0,
            recent,
            shift: 0.0,
            caret: 0,
            caret_on: true,
            hot: 0.0,
        };
        for r in 2..=crate::ui::search::MAX_RECENTS {
            assert!(
                clear_focused(&v(Zone::Recents, r), 2),
                "recent={r} with 2 terms shown"
            );
        }
        assert!(
            !clear_focused(&v(Zone::Recents, 1), 2),
            "a term is focused, not the control"
        );
        assert!(
            !clear_focused(&v(Zone::Field, 4), 2),
            "the field owns the remote, so nothing here does"
        );
    }
}
