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
//! **Five terms** ([`super::MAX_RECENTS`]). With the keyboard raised the header, the rows and the
//! Clear control all have to finish above its top edge (`SCR_H - super::KEYBOARD_H` = 756); this
//! block ends at [`BLOCK_BOTTOM`], which a host test pins against that line. The sixth is DROPPED,
//! not scrolled — a list you cannot see the end of asks to be paged, and there is no paging in this
//! product.
//!
//! It was FOUR until 2026-08-15, and the cap was not the thing that was wrong: `KEYBOARD_H` was a
//! guess 56px too tall, so the arithmetic said a fifth row would not clear a panel edge that does
//! not exist. Measuring the real panel put the fifth back with 66px to spare. The lesson is in the
//! test below rather than in the number — it grades the CLEARANCE, so the count is free to follow
//! whatever the panel actually does.
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
//!
//! **The file is written on a WORKER, never on the SDL thread** — see [`persist`].
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

// ---- The store --------------------------------------------------------------------------------

/// The terms, the runs their rows are DRAWN from, and the profile generation both were read at.
///
/// The two lists live in one struct because they must move together: every path that touches
/// `terms` drops `runs` in the same breath, so a run can never describe a term that is no longer
/// there — which is what lets the draw index them by the focus cursor.
struct Store {
    gen: u32,
    terms: Vec<String>,
    /// One elided, NUL-terminated run per term, in the same order. `None` = STALE, re-baked by
    /// [`runs`] on the next draw — see [`bake`] for why it is baked there and nowhere else.
    runs: Option<Vec<CString>>,
}

/// The store for the current profile generation. `None` = never read.
static STORE: Mutex<Option<Store>> = Mutex::new(None);

/// THE accessor: run `f` over the live store for the current profile generation.
///
/// The poison recovery is the crate's own idiom (`auth::with_ctl` and ~30 other call sites), and
/// here it is load-bearing rather than tidiness. The alternative — `let Ok(g) = lock() else
/// return` — turns one panic anywhere under this lock into a permanent, SILENT loss of the whole
/// feature for the rest of the run: `count()` answers 0 forever, so the block stops drawing, and
/// `remember`/`clear` become no-ops that report nothing. The data behind the lock is a list of
/// strings with no invariant a panic could have half-broken, so recovering the guard is both safe
/// and the only outcome that degrades visibly.
///
/// **Main thread only, and nothing under this lock may block.** [`cached`] can `peek` the session
/// file from here, and the draw bakes glyph runs under it — so the worker in [`flush`] is
/// deliberately built to never come near it. A worker that took this lock and then read flash would
/// simply move the stall onto the next frame that draws, which is the bug this module just stopped
/// having.
fn with_store<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    let mut g = STORE.lock().unwrap_or_else(|e| e.into_inner());
    f(cached(&mut g))
}

/// [`with_store`] over the terms alone — the shape every reader that is not the draw wants.
fn with_terms<R>(f: impl FnOnce(&[String]) -> R) -> R {
    with_store(|s| f(&s.terms))
}

/// Mutate the list and stale its runs in ONE step — the only way the list ever changes.
///
/// `f` answers whether it changed anything, which is both the re-bake test and the caller's "is
/// there anything to write". Keeping the two halves inseparable is what makes [`Store::runs`] safe
/// to index by the focus cursor.
fn edit(f: impl FnOnce(&mut Vec<String>) -> bool) -> bool {
    with_store(|s| {
        if !f(&mut s.terms) {
            return false;
        }
        s.runs = None;
        true
    })
}

/// The cached store for the CURRENT profile generation, re-read from the session file when the
/// generation has moved (see the module doc).
fn cached(g: &mut Option<Store>) -> &mut Store {
    let gen = crate::plex::session::current_gen();
    if g.as_ref().map(|s| s.gen) != Some(gen) {
        // Keyed on the PROFILE, not the account: the cache generation already moves on a Plex
        // Home switch, and this is the read that has to answer differently when it does.
        let who = crate::plex::session::current_profile_key();
        let terms = sanitize(crate::plex::session::peek().recents_for(&who).to_vec());
        *g = Some(Store {
            gen,
            terms,
            runs: None,
        });
    }
    // filled immediately above when it was not already the current generation
    g.as_mut().expect("cache is populated")
}

/// The drawn runs, baked if the list has moved since the last frame. **The only caller is the
/// draw**, and that is a hard requirement, not a convention — see [`bake`].
fn runs(s: &mut Store) -> &[CString] {
    // destructured so the borrow checker sees two DISJOINT fields rather than one `&mut Store`
    let Store { terms, runs, .. } = s;
    runs.get_or_insert_with(|| bake(terms))
}

/// The run each row DRAWS: elided to [`TEXT_W`] and NUL-terminated, baked once per change to the
/// list instead of once per row per frame.
///
/// `text::elide` hands back an owned `String` (it clones out of its own memo) and `CString::new`
/// allocates again, so the draw was paying 8 allocations a frame for four terms — on a screen whose
/// whole job, once settled, is to cost nothing. The elide budget is a compile-time constant here,
/// so nothing but the term itself can change the answer.
///
/// **Baking happens on the DRAW and nowhere else, and both reasons are load-bearing.** `text::elide`
/// memoises through a `static mut` HashMap and measures with `TTF_SizeUTF8`, so it may not be
/// touched from a worker — which is half of why [`flush`] carries its terms rather than reaching
/// into the store. And it may not be reachable from [`cached`] either: `count()` goes through there
/// and `ui/search/mod.rs`'s tests call it, so baking on the cache fill pulls SDL2_ttf into the HOST
/// test binary, which cannot link it (`_TTF_SizeUTF8`, undefined — the suite runs on Darwin with no
/// SDL2_ttf, the same wall `up_next.rs` documents). Hence the lazy [`Store::runs`]: the draw is the
/// one caller, and the draw never runs in a test.
///
/// ONE run per term, always. An unbakeable term takes an EMPTY run rather than being dropped:
/// the runs are indexed by the focus cursor and by the hit rects, so a dropped row would slide
/// every term below it onto the wrong index. That is also exactly what the old per-frame `continue`
/// drew (the pill and the hit rect, no label), and [`usable`] keeps it unreachable from both
/// directions.
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

/// The terms, most recent first. Allocates, so the DRAW path does not use it — it draws the baked
/// runs through [`with_store`] instead.
pub(crate) fn terms() -> Vec<String> {
    with_terms(|t| t.to_vec())
}

/// Record a term that was actually searched. Moves an existing one to the front rather than
/// duplicating it.
///
/// Called when a query is SEARCHED, never per keystroke — every call that CHANGES the list queues a
/// session-file write, and one that does not must cost nothing: a repeat of the term already at the
/// front, or a blank, returns before queueing anything or waking the frame gate.
pub(crate) fn remember(term: &str) {
    let t = term.trim();
    if !usable(t) {
        return;
    }
    // The lock is released with the closure, before anything is queued.
    let changed = edit(|list| {
        if list.first().is_some_and(|f| f == t) {
            return false; // searching the same thing twice is not a change
        }
        promote(list, t);
        true
    });
    if !changed {
        return;
    }
    persist();
    crate::ui::idle::invalidate();
}

/// Forget the lot. **Focus is the caller's problem**: this empties the store, so the block stops
/// being drawn entirely, and a cursor left in [`Zone::Recents`] would then own the remote with
/// nothing on screen to show for it. The zone lives in the state machine, not here.
pub(crate) fn clear() {
    let had = edit(|list| {
        let had = !list.is_empty();
        list.clear();
        had
    });
    if !had {
        return;
    }
    persist();
    crate::ui::idle::invalidate();
}

// ---- Persistence: the file half, off the SDL thread ---------------------------------------------

/// The list a worker still has to write, and whose it is. `None` = the file already agrees.
///
/// This is a QUEUE, not a handoff. The SDL thread only ever REPLACES it, so it holds the newest
/// list the user has committed and a burst of commits collapses into one write — and, because
/// [`flush`] takes it *inside* the session file's own lock, two workers racing to the file cannot
/// land in the wrong order: the first one in writes the newest state and the second finds nothing
/// to do.
static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

struct Pending {
    /// Whose list this is, captured at COMMIT time. [`merged`] used to ask
    /// `session::current_profile_key()` itself, which was exact while it ran inline on the SDL
    /// thread and is not any more: a profile switch between the commit and the write would file one
    /// person's terms under the next person's key — precisely the leak the per-profile keying
    /// exists to prevent.
    who: String,
    terms: Vec<String>,
}

/// The session to WRITE `terms` for `who`, or `None` when there is nothing to write.
///
/// Pure — `who` is a parameter rather than a global read for the reason [`Pending::who`] gives —
/// and split out from [`flush`] so both refusals are host-testable. The second is the single line
/// standing between a search term and a wiped credentials file, and it is invisible to every other
/// test in the suite.
///
/// **Never write a session we could not READ.** `peek` hands back a default `Session` both for "no
/// file yet" and for "the file did not parse", and saving that would truncate a live one — a
/// silent sign-out, caused by a search term. `client_id` is minted once by `session::load` on the
/// boot path and is never empty afterwards, so it is exactly the test for "something real came
/// back": with no readable session the terms stay in memory for this run and are dropped with it.
/// `session::update` refuses the same case one layer up, for every caller rather than this one;
/// the test stays here because this is where it is *graded*, and because a rule worth having in
/// two places is one whose cost is a string comparison.
fn merged(
    s: &crate::plex::session::Session,
    who: &str,
    terms: &[String],
) -> Option<crate::plex::session::Session> {
    if s.client_id.is_empty() || s.recents_for(who) == terms {
        return None;
    }
    // `set_recents_for`, never a struct update with `recent_searches:` — the field now holds EVERY
    // profile's history, so assigning it here would drop everyone else's. That is the whole reason
    // the setter exists rather than the field being written at this call site.
    let mut next = s.clone();
    next.set_recents_for(who, terms.to_vec());
    Some(next)
}

/// Queue the current list for the session file and hand the write to a worker.
///
/// **The SDL event thread must never touch this file.** One write is `peek` (4–5 `PathBuf`s, an
/// `fs::read` and a `serde_json` parse of the whole credentials file) followed by `to_vec_pretty`
/// and an `O_TRUNC` write — on a 32-bit ARM television's flash. It ran inline on every committed
/// term: the ▼ handoff into the results (the most common gesture on this screen), every ▲ back to
/// the strip, every pointer click off the field, every recents pick and Clear. Nothing on screen
/// waits for it — [`STORE`] is the source of truth for the draw and is already updated — so the
/// write is genuinely fire-and-forget.
///
/// Both halves of the payload are read HERE, on the SDL thread: the profile key because it must be
/// the one that was searching (see [`Pending::who`]), and the terms because [`STORE`]'s lock is
/// main-thread-only — a worker that reached in for them could be holding it while the next frame
/// wants to draw, and could find [`cached`] doing a file read under it.
///
/// A refused spawn is a return value (`task.rs`), and this caller's answer is to leave the list in
/// [`PENDING`] and let `task.rs`'s own log stand. Deliberately NOT a fall-back to writing inline:
/// the refusal means the device is out of threads or address space, which is the worst possible
/// moment to park the event loop on flash — and the change is deferred rather than lost, since the
/// next commit's worker takes the pending list, which by then is the newer one anyway. Only a run
/// that ends with no further commit drops it, and it was never on screen.
fn persist() {
    let who = crate::plex::session::current_profile_key();
    let terms = with_terms(|t| t.to_vec());
    *PENDING.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pending { who, terms });
    let _ = crate::task::spawn_small("recents-save", flush);
}

/// The worker: take whatever is pending and write it.
///
/// Re-reads the session FILE rather than carrying one — the snapshot this module holds is only the
/// terms, and everything else in that file (a roster refresh, a profile pick) may have moved since.
/// Same read-modify-write `auth.rs` does for the roster, and now through the same door.
///
/// **The lock is `session`'s, not ours.** This module kept a `WRITING` mutex of its own, which
/// serialized recents against recents and against nothing else — so the two writers that actually
/// contend, this one and `auth`'s roster refresh, could still interleave a lost update or a torn
/// file between them. [`crate::plex::session::update`] is the one authority now (its doc has the
/// two failures), and the pending list is taken INSIDE it, which keeps the ordering property this
/// worker has always had: whichever worker reaches the file first writes the newest state, and the
/// second finds nothing to do. The second now pays a file read to discover that (`update` reads
/// before it calls the closure) — a worker's read, on a path that runs once per committed term.
///
/// It touches [`PENDING`] and nothing else of this module's: not [`STORE`], not the glyph cache,
/// not the profile key. That is the whole seam — everything it needs was read on the SDL thread by
/// [`persist`].
fn flush() {
    crate::plex::session::update(|s| {
        let p = PENDING.lock().unwrap_or_else(|e| e.into_inner()).take()?;
        merged(s, &p.who, &p.terms)
    });
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
    // The baked runs are BORROWED for the whole block rather than cloned: this runs on every
    // presented frame, and a `Vec` plus four `CString`s a frame is allocation the idle gate was
    // built to avoid paying. Nothing inside mutates the store, so the lock cannot be re-entered.
    with_store(|s| draw_block(p, v, runs(s)));
}

/// `rows` is [`bake`]'s output — one run per stored term, in the same order, so `i` here is the same
/// `i` the focus cursor and the hit rects use.
fn draw_block(p: Painter, v: &View, rows: &[CString]) {
    let shown = rows.len().min(super::MAX_RECENTS);
    if shown == 0 {
        return;
    }

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
        super::note_recent_rect(i, Rect::new(BLOCK_X, ry, BLOCK_W, table::ROW_H));
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
        assert_eq!(
            l,
            list(&["laura", "wallace"]),
            "an existing term is moved, not added"
        );

        // a different CASE is the same term — the new spelling wins
        promote(&mut l, "WALLACE");
        assert_eq!(l, list(&["WALLACE", "laura"]));

        // …and so is one the user typed with stray whitespace around it
        promote(&mut l, "  laura  ");
        assert_eq!(l, list(&["laura", "WALLACE"]));

        // a blank is not a search, and neither is anything undrawable
        for junk in ["", "   ", "\t\n", "wal\0lace"] {
            promote(&mut l, junk);
            assert_eq!(
                l,
                list(&["laura", "WALLACE"]),
                "{junk:?} must not enter the list"
            );
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
        assert!(
            merged(&Session::default(), "uu-1", &terms).is_none(),
            "an unreadable session is never written"
        );

        let live = Session {
            client_id: "cid-1".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        let next = merged(&live, "uu-1", &terms).expect("a real session takes the terms");
        assert_eq!(next.recents_for("uu-1"), terms);
        assert_eq!(
            next.account_token, "acct",
            "everything else in the file is carried over untouched"
        );

        // and an unchanged list is not a write: the worker re-reads the file on every flush
        assert!(
            merged(&next, "uu-1", &terms).is_none(),
            "no change, no write"
        );
    }

    /// The profile key is an ARGUMENT, not a global read — which is what makes the write safe to do
    /// on a worker. `persist` captures it on the SDL thread at commit time, so a profile switch
    /// landing between the commit and the write cannot file one person's terms under the next
    /// person's key, and cannot touch the list already stored for anybody else.
    #[test]
    fn terms_are_written_under_the_profile_that_searched_them() {
        use crate::plex::session::Session;
        let live = Session {
            client_id: "cid-1".into(),
            ..Default::default()
        };

        let a = merged(&live, "uu-a", &list(&["wallace"])).expect("a's terms land");
        let b = merged(&a, "uu-b", &list(&["gromit"])).expect("b's terms land beside them");
        assert_eq!(
            b.recents_for("uu-a"),
            list(&["wallace"]),
            "the other profile's list is untouched"
        );
        assert_eq!(b.recents_for("uu-b"), list(&["gromit"]));

        // the same terms under a DIFFERENT key are still a change — the guard compares this
        // profile's stored list, never the file as a whole
        assert!(
            merged(&b, "uu-c", &list(&["gromit"])).is_some(),
            "a third profile gets its own entry"
        );
        assert!(
            merged(&b, "uu-b", &list(&["gromit"])).is_none(),
            "…but the same profile's is a no-op"
        );
    }

    /// The cap drops the OLDEST, which is the only end that can be dropped without contradicting
    /// "most recent first".
    ///
    /// Written against [`CAP`] rather than against a literal count — the cap is a LAYOUT answer
    /// (see the clearance test), and it moved the day the keyboard was measured. A test that spelt
    /// the number would have failed for a correct change and taught nothing about the rule.
    #[test]
    fn the_cap_drops_the_oldest_term() {
        assert_eq!(
            CAP,
            crate::ui::search::MAX_RECENTS,
            "the store keeps exactly what the screen shows"
        );
        // One more term than fits, newest last, so the survivors are the reverse of the tail.
        let typed: Vec<String> = (0..CAP + 1).map(|i| format!("q{i}")).collect();
        let mut l = Vec::new();
        for t in &typed {
            promote(&mut l, t);
        }
        let want: Vec<String> = typed[1..].iter().rev().cloned().collect();
        assert_eq!(l, want, "the oldest fell off, order is newest-first");
        assert_eq!(l.len(), CAP);
    }

    /// A file is not a promise. `de_soft_vec` guarantees every entry is a `String` and nothing
    /// else, so the store re-imposes its own invariants on read — order preserved, blanks and
    /// repeats gone, length bounded.
    #[test]
    fn a_hand_edited_list_is_cleaned_up_on_the_way_in() {
        let raw = list(&[
            "laura",
            "",
            "  ",
            "LAURA",
            "wallace",
            "gromit",
            "feathers",
            "wendolene",
            "grue",
        ]);
        let got = sanitize(raw);
        // The three rules, stated separately from the LENGTH so the cap can move on its own (see
        // `the_cap_drops_the_oldest_term`): order preserved, blanks gone, the repeat collapsed onto
        // its FIRST place, and whatever survives is bounded.
        let kept = [
            "laura",
            "wallace",
            "gromit",
            "feathers",
            "wendolene",
            "grue",
        ];
        assert_eq!(
            got,
            list(&kept[..CAP.min(kept.len())]),
            "newest-first order kept, blanks dropped, the repeat collapsed onto its FIRST place"
        );
        assert!(got.len() <= CAP);
        assert!(
            !got.iter().any(|t| t.trim().is_empty()),
            "a blank is not a term"
        );
        assert_eq!(sanitize(Vec::new()), Vec::<String>::new());
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
