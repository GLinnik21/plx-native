//! The empty-query state: the user's own recent searches, and the control that clears them.
//!
//! ## The design
//!
//! Rows, not a shelf: nothing here has artwork. They use TableView's own geometry on the app's
//! ground instead of inside a panel — [`ROW_H`] tall, [`PILL_SIDE`] margins,
//! [`crate::ui::table::CONTENT_X`] inside, the focus pill inset by [`PILL_INSET`], HEADLINE labels.
//! Written as markup rather than mounted as a [`crate::ui::table::TableView`] for one reason, and
//! it is worth keeping: **these rows are the user's own words and have to stay editable in place.**
//!
//! Above them sits a section HEADER, not a heading — it names the source of the rows, so it sits a
//! full step BELOW their labels: CAPTION, caps, tertiary, on the same [`HDR_H`] band TableView
//! reserves. At HEADLINE it read as another row.
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
//! **Four terms, not five** ([`super::MAX_RECENTS`]). With the keyboard raised the header, the rows
//! and the Clear control all have to finish above its top edge (`SCR_H - super::KEYBOARD_H` = 700);
//! this block ends at [`BLOCK_BOTTOM`], which a host test pins against that line. The fifth term is
//! DROPPED, not scrolled — a list you cannot see the end of asks to be paged, and there is no
//! paging in this product.
//!
//! ## Persistence
//!
//! The terms live in the session file beside the roster ([`crate::plex::session::Session`]'s
//! `recent_searches`), behind `#[serde(default, deserialize_with = "de_soft_vec")]` — a corrupt
//! entry costs that entry, never the session.
//!
//! The file is read at most once per **profile generation** ([`crate::plex::session::current_gen`],
//! bumped by every `set_current`), not once per frame: `count()` is called from the screen's draw.
//!
//! **The generation is a cache key; the SCOPING is in the storage shape.** A sign-out drops the
//! terms because `auth::sign_out` calls `session::clear()` (the file goes) and then
//! `set_current(None)` (the generation moves), so one account's terms cannot survive into the
//! next account's session. A Plex Home **profile switch keeps them apart** for a different
//! reason: `Session::recent_searches` is keyed by the Home user's `uuid`
//! ([`crate::plex::session::RecentSearches`]), read through `recents_for` and written through
//! `set_recents_for`, so each managed user has their own list and Clear reaches only their own.
//!
//! It was one flat `Vec<String>` on the `Session` until 2026-08-14, which made a search history
//! the ACCOUNT's rather than the person's — the next user's empty search screen offered back what
//! the previous one had looked for. Clearing on a switch would also have stopped that and is the
//! wrong fix: it costs you your own list every time you hand the remote over and take it back.
//!
//! `search::reset()` deliberately does NOT drop the terms. It runs on a plain SERVER switch too,
//! where the terms are still this person's, and the profile case is already answered by the key.
#![allow(dead_code)]

use crate::ui::label::Label;
use crate::ui::search::{View, Zone};
use crate::ui::widgets::Button;
// anonymous: this screen's own `View` is the per-frame SNAPSHOT above, and the retui trait of the
// same name is wanted only so `Button::draw` resolves
use crate::ui::View as _;
use crate::ui::{theme, Env, Painter, Rect};
use std::ffi::CString;
use std::sync::Mutex;

/// How many terms are KEPT — and there is exactly ONE cap, applied on both sides of the file.
///
/// [`sanitize`] imposes it on READ and [`promote`] on WRITE, both at [`super::MAX_RECENTS`], so a
/// file holding more (hand-edited, or written by a build whose list was taller) is read as four and
/// **truncated to four by the next write**. That is deliberate rather than incidental: a term the
/// screen can never show is dead weight in a credentials file, and the design's "the fifth is
/// dropped, not scrolled" is a statement about the list, not only about the drawing of it.
///
/// The drawer's own `min`/`take` therefore cannot bind today. They stay because `draw` must not
/// depend on a store invariant to avoid running off the block's own layout — a taller store would
/// otherwise draw rows through the raised keyboard.
const CAP: usize = super::MAX_RECENTS;

// ---- Geometry ---------------------------------------------------------------------------------
//
// The block is the FIELD's own column: same left edge, same width, so the rows line up under the
// thing that produced them. Everything below restates `table.rs`'s row geometry, which is private
// to that widget — see the module doc for why these rows are drawn rather than mounted.

/// A row's height, and the focus pill's inset from its top and bottom edges.
const ROW_H: f32 = 60.0;
const PILL_INSET: f32 = 3.0;
/// The pill's inset from the block's left/right edges, and its corner radius.
const PILL_SIDE: f32 = 12.0;
const PILL_RAD: f32 = 18.0;
/// The section header's band. Fixed whatever the header's own size, so a size change cannot reflow
/// the rows under it.
const HDR_H: f32 = 58.0;
/// Header caps, pre-uppercased. The header is a constant here (TableView's `to_uppercase` exists
/// because its headers are runtime machine and library names).
const HDR: &std::ffi::CStr = c"RECENT SEARCHES";
/// The Clear control's label and the air above it. A `space::MD` rung, not a hand-tuned gap: it
/// separates two different KINDS of thing (the list, then a verb), which is exactly the rung's job.
const CLEAR: &std::ffi::CStr = c"Clear";
const CLEAR_GAP: f32 = theme::space::MD;

/// The block's left edge and width — the field's column (see above).
const BLOCK_X: f32 = super::FIELD.x;
const BLOCK_W: f32 = super::FIELD.w;
/// Where a row's own content starts. `table::CONTENT_X` is public for exactly this: a caller that
/// draws beside a list starting its text on the same line the rows do, rather than re-deriving two
/// private constants and drifting from them.
const TEXT_X: f32 = BLOCK_X + crate::ui::table::CONTENT_X;
/// First row's top: the header band is reserved whether or not anything is under it.
const ROWS_TOP: f32 = super::CONTENT_TOP + HDR_H;
/// The bottom edge of the Clear control at a FULL list — the number the four-term cap exists to
/// keep under the raised keyboard's top edge. Asserted by a host test, not by the eye.
const BLOCK_BOTTOM: f32 =
    ROWS_TOP + super::MAX_RECENTS as f32 * ROW_H + CLEAR_GAP + super::FIELD.h;

// ---- The store --------------------------------------------------------------------------------

/// The terms, and the profile generation they were read at. `None` = never read.
static TERMS: Mutex<Option<(u32, Vec<String>)>> = Mutex::new(None);

/// THE accessor: run `f` over the live list for the current profile generation.
///
/// The poison recovery is the crate's own idiom (`auth::with_ctl` and ~30 other call sites), and
/// here it is load-bearing rather than tidiness. The alternative — `let Ok(g) = lock() else
/// return` — turns one panic anywhere under this lock into a permanent, SILENT loss of the whole
/// feature for the rest of the run: `count()` answers 0 forever, so the block stops drawing, and
/// `remember`/`clear` become no-ops that report nothing. The data behind the lock is a list of
/// strings with no invariant a panic could have half-broken, so recovering the guard is both safe
/// and the only outcome that degrades visibly.
fn with_terms<R>(f: impl FnOnce(&mut Vec<String>) -> R) -> R {
    let mut g = TERMS.lock().unwrap_or_else(|e| e.into_inner());
    f(cached(&mut g))
}

/// The cached list for the CURRENT profile generation, re-read from the session file when the
/// generation has moved (see the module doc).
fn cached(g: &mut Option<(u32, Vec<String>)>) -> &mut Vec<String> {
    let gen = crate::plex::session::current_gen();
    if g.as_ref().map(|(v, _)| *v) != Some(gen) {
        // Keyed on the PROFILE, not the account: the cache generation already moves on a Plex
        // Home switch, and this is the read that has to answer differently when it does.
        let who = crate::plex::session::current_profile_key();
        *g = Some((gen, sanitize(crate::plex::session::peek().recents_for(&who).to_vec())));
    }
    // filled immediately above when it was not already the current generation
    &mut g.as_mut().expect("cache is populated").1
}

/// What the store will hold, whatever the file said. `de_soft_vec` guarantees each entry is a
/// `String` and nothing more, so a hand-edited file can still hand us blanks, whitespace, repeats
/// or a hundred of them.
///
/// Deliberately not a fold of [`promote`], which inserts at the FRONT: replaying a
/// newest-first file through it would build the list backwards, and the [`CAP`] would then drop
/// the newest terms instead of the oldest. This walks the file in its own order and keeps the
/// FIRST spelling of each term, which for a newest-first list is the most recent one.
fn sanitize(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in raw {
        let t = t.trim();
        if !usable(t) {
            continue;
        }
        let key = t.to_lowercase();
        if out.iter().any(|s| s.to_lowercase() == key) {
            continue;
        }
        out.push(t.to_string());
        if out.len() == CAP {
            break;
        }
    }
    out
}

/// Can this ALREADY-TRIMMED term be stored and drawn? Blank is not a search. An interior NUL is
/// the non-obvious half: `de_soft_vec` accepts it (it is a valid `String`) and trimming and
/// de-duplication both survive it, but `CString::new` refuses it, so the row's label would be
/// skipped and a focused term would draw as a **full-width accent pill with nothing in it** — a
/// control the user can move onto and press with no way to tell what it is. This module's stated
/// job is to re-impose its own invariants on the way in, and drawability is one of them.
fn usable(trimmed: &str) -> bool {
    !trimmed.is_empty() && !trimmed.contains('\0')
}

/// The pure list operation behind [`remember`]: `term` becomes the most recent, an existing spelling
/// of it is REMOVED rather than duplicated, and the oldest fall off the end at [`CAP`].
///
/// Case-insensitive by `to_lowercase`, not `eq_ignore_ascii_case`: the libraries measured here are
/// Cyrillic, and a term is whatever the user typed. The NEW spelling is what is kept — you get back
/// the words you just searched, capitalised the way you just wrote them.
///
/// A term that is not [`usable`] is dropped.
fn promote(list: &mut Vec<String>, term: &str) {
    let t = term.trim();
    if !usable(t) {
        return;
    }
    let key = t.to_lowercase();
    list.retain(|s| s.to_lowercase() != key);
    list.insert(0, t.to_string());
    list.truncate(CAP);
}

/// How many terms are stored. Never more than [`super::MAX_RECENTS`] — see [`CAP`].
pub(crate) fn count() -> usize {
    with_terms(|t| t.len())
}

/// The terms, most recent first. Allocates, so the DRAW path does not use it — it borrows through
/// [`with_terms`] instead.
pub(crate) fn terms() -> Vec<String> {
    with_terms(|t| t.clone())
}

/// Record a term that was actually searched. Moves an existing one to the front rather than
/// duplicating it.
///
/// Called when a query is SEARCHED, never per keystroke — every call that CHANGES the list writes
/// the session file, and one that does not must cost nothing: a repeat of the term already at the
/// front, or a blank, returns before touching the file or waking the frame gate.
pub(crate) fn remember(term: &str) {
    let t = term.trim();
    if !usable(t) {
        return;
    }
    // The lock is released with the closure, before the file write.
    let Some(next) = with_terms(|list| {
        if list.first().is_some_and(|f| f == t) {
            return None; // searching the same thing twice is not a change
        }
        promote(list, t);
        Some(list.clone())
    }) else {
        return;
    };
    persist(&next);
    crate::ui::idle::invalidate();
}

/// Forget the lot. **Focus is the caller's problem**: this empties the store, so the block stops
/// being drawn entirely, and a cursor left in [`Zone::Recents`] would then own the remote with
/// nothing on screen to show for it. The zone lives in the state machine, not here.
pub(crate) fn clear() {
    let had = with_terms(|list| {
        let had = !list.is_empty();
        list.clear();
        had
    });
    if !had {
        return;
    }
    persist(&[]);
    crate::ui::idle::invalidate();
}

/// The session to WRITE for these terms, or `None` when there is nothing to write.
///
/// Pure, and split out from [`persist`] so both refusals are host-testable — the second one is the
/// single line standing between a search term and a wiped credentials file, and it is invisible to
/// every other test in the suite.
///
/// **Never write a session we could not READ.** `peek` hands back a default `Session` both for "no
/// file yet" and for "the file did not parse", and saving that would truncate a live one — a
/// silent sign-out, caused by a search term. `client_id` is minted once by `session::load` on the
/// boot path and is never empty afterwards, so it is exactly the test for "something real came
/// back": with no readable session the terms stay in memory for this run and are dropped with it.
fn merged(s: &crate::plex::session::Session, terms: &[String]) -> Option<crate::plex::session::Session> {
    let who = crate::plex::session::current_profile_key();
    if s.client_id.is_empty() || s.recents_for(&who) == terms {
        return None;
    }
    // `set_recents_for`, never a struct update with `recent_searches:` — the field now holds EVERY
    // profile's history, so assigning it here would drop everyone else's. That is the whole reason
    // the setter exists rather than the field being written at this call site.
    let mut next = s.clone();
    next.set_recents_for(&who, terms.to_vec());
    Some(next)
}

/// Write the list back to the session file, re-reading it first: the snapshot this store holds is
/// only the terms, and everything else in that file (a roster refresh, a profile pick) may have
/// moved since. Same read-modify-write `auth.rs` does for the roster, and for the same reason.
fn persist(terms: &[String]) {
    if let Some(next) = merged(&crate::plex::session::peek(), terms) {
        crate::plex::session::save(&next);
    }
}

// ---- The drawing ------------------------------------------------------------------------------

/// Does the Clear control hold focus? Its index is [`super::MAX_RECENTS`] — one past the LAST
/// possible term — but the test is `>= shown` so that a list of two terms still lands the ring on
/// the control rather than on nothing, whichever of the two the state machine's cursor names.
fn clear_focused(v: &View, shown: usize) -> bool {
    v.zone == Zone::Recents && v.recent >= shown
}

pub(crate) fn draw(p: Painter, v: &View) {
    // The list is BORROWED for the whole block rather than cloned: this runs on every presented
    // frame, and a `Vec` plus four `String`s a frame is allocation the idle gate was built to
    // avoid paying. Nothing inside mutates the store, so the lock cannot be re-entered.
    with_terms(|terms| draw_block(p, v, terms));
}

fn draw_block(p: Painter, v: &View, terms: &[String]) {
    let shown = terms.len().min(super::MAX_RECENTS);
    if shown == 0 {
        return;
    }

    // The header: a step BELOW the rows it names, on its own fixed band.
    Label::new(HDR.as_ptr(), theme::size::CAPTION, theme::TEXT_TERTIARY)
        .draw(p, Rect::new(TEXT_X, super::CONTENT_TOP, 0.0, HDR_H));

    // The rows. One elided HEADLINE-bold run, cap-band-centred in the row box — TableView's own
    // single-line label path, because these are the same rows on a different ground.
    let text_w = BLOCK_W - 2.0 * crate::ui::table::CONTENT_X;
    for (i, term) in terms.iter().take(shown).enumerate() {
        let ry = ROWS_TOP + i as f32 * ROW_H;
        let focused = v.zone == Zone::Recents && v.recent == i;
        if focused {
            let pill = Rect::new(
                BLOCK_X + PILL_SIDE,
                ry + PILL_INSET,
                BLOCK_W - 2.0 * PILL_SIDE,
                ROW_H - 2.0 * PILL_INSET,
            );
            p.rrect(pill, PILL_RAD, PILL_RAD, crate::ui::ACCENT);
        }
        // The whole row box, not the pill: a term is clickable across the band it occupies,
        // and the pill is only drawn when it is focused. Without this the recents list answered
        // to the d-pad alone — `mod.rs::hit` scans an array `draw` parks every frame and no
        // drawer ever filled, so every click missed.
        super::note_recent_rect(i, Rect::new(BLOCK_X, ry, BLOCK_W, ROW_H));
        let ink = if focused { crate::ui::ACCENT_INK } else { theme::TEXT_PRIMARY };
        let run = crate::text::elide(term, text_w, theme::size::HEADLINE, 1, false);
        let Ok(cs) = CString::new(run) else { continue };
        Label::new(cs.as_ptr(), theme::size::HEADLINE, ink)
            .bold()
            .draw(p, Rect::new(TEXT_X, ry, 0.0, ROW_H));
    }

    // Clearing is a CONTROL: it leaves the column of words and becomes the shared pill, at the
    // rows' own text x so it reads as belonging to the block without sitting in their column.
    let by = ROWS_TOP + shown as f32 * ROW_H + CLEAR_GAP;
    let bw = Button::pill_w(CLEAR.as_ptr(), theme::size::BODY, false);
    let cr = Rect::new(TEXT_X, by, bw, super::FIELD.h);
    // Clear sits at `MAX_RECENTS`, one past the last term it could ever follow — the index
    // `mod.rs` reserves for it whatever `shown` turns out to be.
    super::note_recent_rect(super::MAX_RECENTS, cr);
    Button::new(CLEAR.as_ptr(), theme::size::BODY, cr)
        .focused(clear_focused(v, shown))
        .draw(&Env::inert(), p);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// The whole point of `remember`: searching something you have searched before REORDERS the
    /// list, it does not lengthen it — and the spelling you just typed is the one you get back.
    #[test]
    fn remembering_a_term_moves_it_to_the_front_instead_of_duplicating_it() {
        let mut l = list(&["wallace", "laura"]);
        promote(&mut l, "laura");
        assert_eq!(l, list(&["laura", "wallace"]), "an existing term is moved, not added");

        // a different CASE is the same term — the new spelling wins
        promote(&mut l, "WALLACE");
        assert_eq!(l, list(&["WALLACE", "laura"]));

        // …and so is one the user typed with stray whitespace around it
        promote(&mut l, "  laura  ");
        assert_eq!(l, list(&["laura", "WALLACE"]));

        // a blank is not a search, and neither is anything undrawable
        for junk in ["", "   ", "\t\n", "wal\0lace"] {
            promote(&mut l, junk);
            assert_eq!(l, list(&["laura", "WALLACE"]), "{junk:?} must not enter the list");
        }
    }

    /// A term carrying an interior NUL is not storable, because it is not DRAWABLE: `CString::new`
    /// refuses it, the row's label is skipped, and a focused term becomes a full-width accent pill
    /// with nothing in it. `de_soft_vec` cannot catch this — it is a perfectly good `String`.
    #[test]
    fn an_undrawable_term_never_reaches_the_store() {
        assert!(usable("wallace"));
        assert!(!usable("") && !usable("wal\0lace") && !usable("\0"));
        assert_eq!(sanitize(list(&["wal\0lace", "gromit"])), list(&["gromit"]));
    }

    /// The one line between a search term and a wiped credentials file. Both refusals matter, and
    /// neither is visible to any other test in the suite — delete the `client_id` guard and 542
    /// tests still pass.
    #[test]
    fn a_session_that_could_not_be_read_is_never_written_back() {
        use crate::plex::session::Session;
        let terms = list(&["wallace"]);

        // `peek` hands back a DEFAULT session both for "no file yet" and for "the file did not
        // parse" — writing that would truncate a live one, i.e. sign the device out over a search.
        assert!(merged(&Session::default(), &terms).is_none(), "an unreadable session is never written");

        let live = Session { client_id: "cid-1".into(), account_token: "acct".into(), ..Default::default() };
        let next = merged(&live, &terms).expect("a real session takes the terms");
        assert_eq!(next.recents_for(&crate::plex::session::current_profile_key()), terms);
        assert_eq!(next.account_token, "acct", "everything else in the file is carried over untouched");

        // and an unchanged list is not a write: `remember` re-reads the file on every call
        assert!(merged(&next, &terms).is_none(), "no change, no write");
    }

    /// Four, not five. The cap drops the OLDEST, which is the only end that can be dropped without
    /// contradicting "most recent first".
    #[test]
    fn the_cap_drops_the_oldest_term() {
        assert_eq!(CAP, crate::ui::search::MAX_RECENTS, "the store keeps exactly what the screen shows");
        let mut l = Vec::new();
        for t in ["a", "b", "c", "d", "e"] {
            promote(&mut l, t);
        }
        assert_eq!(l, list(&["e", "d", "c", "b"]), "the oldest fell off, order is newest-first");
        assert_eq!(l.len(), CAP);
    }

    /// A file is not a promise. `de_soft_vec` guarantees every entry is a `String` and nothing
    /// else, so the store re-imposes its own invariants on read — order preserved, blanks and
    /// repeats gone, length bounded.
    #[test]
    fn a_hand_edited_list_is_cleaned_up_on_the_way_in() {
        let raw = list(&["laura", "", "  ", "LAURA", "wallace", "gromit", "feathers", "wendolene"]);
        let got = sanitize(raw);
        assert_eq!(got, list(&["laura", "wallace", "gromit", "feathers"]),
            "newest-first order kept, blanks dropped, the repeat collapsed onto its FIRST place");
        assert_eq!(sanitize(Vec::new()), Vec::<String>::new());
    }

    /// The layout rule the four-term cap exists to satisfy: with the keyboard raised, nothing this
    /// screen owns may hide behind it. Graded here rather than by eye, because the failure is a
    /// control the user cannot see or reach — and every term in the design's own copy is short, so
    /// the case that breaks is a full list, which a screenshot of a fresh install never shows.
    ///
    /// The clearance floor is the second half of the claim: the block must not merely *fit*, it has
    /// to sit a block gap clear of the panel, or the Clear control reads as attached to a piece of
    /// television chrome it has nothing to do with.
    #[test]
    fn a_full_block_finishes_clear_of_the_raised_keyboard() {
        let kbd_top = crate::ui::consts::SCR_H - crate::ui::search::KEYBOARD_H;
        let clearance = kbd_top - BLOCK_BOTTOM;
        assert!(clearance >= theme::space::LG,
            "a full block ends at {BLOCK_BOTTOM} and the keyboard starts at {kbd_top} — {clearance}px");
    }

    /// Focus never falls off the end of a SHORT list: with two terms stored, the state machine's
    /// cursor at `MAX_RECENTS` — and at every index in between — lands on the Clear control.
    #[test]
    fn the_clear_control_takes_focus_past_the_last_shown_term() {
        let v = |zone, recent| View { zone, editing: false, row: 0, col: 0, recent, shift: 0.0 };
        for r in 2..=crate::ui::search::MAX_RECENTS {
            assert!(clear_focused(&v(Zone::Recents, r), 2), "recent={r} with 2 terms shown");
        }
        assert!(!clear_focused(&v(Zone::Recents, 1), 2), "a term is focused, not the control");
        assert!(!clear_focused(&v(Zone::Field, 4), 2), "the field owns the remote, so nothing here does");
    }
}
