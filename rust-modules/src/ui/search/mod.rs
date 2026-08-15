//! search — the Search screen (`Search Screen.dc.html`).
//!
//! The last pill in the shared top strip, and the only one that is a mark instead of a word. Text
//! entry is the TELEVISION's own keyboard ([`crate::textinput`]), so this screen owns no keyboard
//! layout at all: it owns the field, the live result set behind the raised panel, and the handoff
//! back to the shelves on ▼.
//!
//! ## Split across five files, deliberately
//!
//! This module is the state machine — zones, focus, scroll, and the draw ORDER — and nothing else.
//! Each region draws itself:
//!
//! | file | draws |
//! |---|---|
//! | [`field`] | the query capsule, the caret, and the scope line beside it |
//! | [`recents`] | the empty-query state: the user's own recent terms, and Clear |
//! | [`results`] | the typed shelves |
//! | [`empty`] | "You haven't searched yet" / "No results for …" |
//!
//! Every one takes the same [`View`] snapshot, so a region can never read a different focus than
//! the one the state machine believes in.
//!
//! ## The layout rule that explains the numbers
//!
//! With the keyboard raised, **nothing the app owns hides behind it**. The TV puts the panel over
//! the bottom [`KEYBOARD_H`], so its top edge is at **756** (measured, see that constant), and the
//! field, the first shelf's heading and that shelf's full row of posters all finish above it:
//! [`CONTENT_TOP`] 248 + [`HEAD_TO_ROW`] 60 + a 375-tall poster = **683**, seventy-three clear.
//! (This paragraph has now been wrong twice, in opposite directions: first 699 — "exactly on its
//! top edge", a number `CONTENT_TOP` = 264 would give and nothing draws — then 700, from a
//! `KEYBOARD_H` nobody had measured. `results`'
//! `the_first_shelfs_whole_row_clears_the_raised_keyboard` asserts the real figures, for every tile
//! size a first shelf can have, which is why the correction was a two-line edit rather than an
//! archaeology exercise.) That clearance is also why nothing scrolls while the panel is up: the
//! result set has to be stable under the user's eyes while they are still typing.
//!
//! ## The ▼ handoff, which is the rule the whole state machine exists for
//!
//! ▼ off the field lands on something **that is DRAWN**, and on nothing otherwise — [`below`] is
//! the one expression that answers it, and [`draw`] dispatches its regions from the same call, so
//! the zone focus moves to and the region on screen can never be two different answers. With no
//! query and no remembered terms there is nothing under the field at all, and the press is a
//! no-op: focus staying visibly put is the honest report, where dropping it into an invisible list
//! is a screen the remote has silently stopped addressing.
//!
//! ## Scroll
//!
//! One spring, and only the SHELVES ride it ([`View::shift`]): the field, the strip and the recents
//! rows are chrome at fixed y. **The flow's whole shape lives in [`results`]** — this module owns
//! the two FREEZES ([`scroll_frozen`]) and nothing else about it, and asks
//! [`results::reveal_shelf`] for the rest.
//!
//! The rule is the shared minimal reveal (`card_row::reveal`, as the home grid, the person page and
//! the Library grid all scroll), **not** the mock's pin of the focused block to [`CONTENT_TOP`];
//! `results`' module doc carries that decision and the reasons. This paragraph used to describe the
//! pin and to call the second freeze "forced rather than chosen: nothing clips the flow", which
//! stopped being true when `results::draw` gained a real scissor at the field's bottom edge.
//!
//! So the freeze at zero whenever the shelves do not hold focus is now a CHOICE, and the reason it
//! stays is the ▼ handoff: it lands on shelf **0** and not on the shelf last left, and it can only
//! mean that if the flow is already back at 0 — restoring a deep row would jump the page under the
//! user's eyes at the exact moment they asked for a small move. The COLUMN is remembered (and
//! clamped), which is the half of "lands where you were" that costs nothing.

use crate::ui::consts::{K_SCALE, K_SCROLL, SDLK_BACKSPACE, SDLK_CLEAR, SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT, SDLK_UP};
use crate::ui::xfade::Xfade;
use crate::ui::{Painter, Rect, Spring};
use std::os::raw::{c_int, c_uint};
use std::ptr::{addr_of, addr_of_mut};

pub(crate) mod empty;
pub(crate) mod field;
pub(crate) mod recents;
pub(crate) mod results;

/// Field bottom (148 + one control height) plus one 40 rung — where every state's content starts,
/// and what a scrolled shelf returns to.
pub(crate) const CONTENT_TOP: f32 = 248.0;
/// Shelf heading to the top of its row: the 30px heading plus a 30px gap, as `home.rs` draws it.
pub(crate) const HEAD_TO_ROW: f32 = 60.0;
/// The caption pair reserved under every tile. Reserved whether or not it is drawn, so nothing
/// reflows as focus travels along a row. Shared with [`results`], which stacks it into `block_h`
/// and owns the contract that it must hold a `card_row::TileLabel` — see that function's doc, and
/// raise it there rather than restating the arithmetic here.
pub(crate) const LABEL_BLOCK: f32 = 114.0;
/// How much of the panel the TV's keyboard covers. Not ours to draw or to style — this is the
/// clearance the layout above keeps.
///
/// **MEASURED, off four device captures 2026-08-15**, not estimated: the panel's top edge is at
/// y=756 on every one of them, to the pixel, which is the 324 `Search Screen.dc.html` states. It
/// was 380 — a guess, 56px too tall — and the cost was not slack but a layout decision made against
/// a number that was never true: [`MAX_RECENTS`] was cut to four because five would not clear a
/// keyboard top edge 56px higher than the real one.
///
/// If a firmware ever moves it, this is the one constant to re-measure (a `DISPLAY` capture with the
/// panel up, scan a column for the discontinuity) — everything else on this screen is derived.
pub(crate) const KEYBOARD_H: f32 = 324.0;
/// The field: 820 wide at the app's own side margin, on the one control height.
pub(crate) const FIELD: Rect = Rect { x: 90.0, y: 148.0, w: 820.0, h: 60.0 };
/// Where the app's own chrome ends and the result band begins — the field's bottom edge.
///
/// It is the navigation scrim's OPAQUE floor: [`crate::ui::widgets::nav_scrim`] fills flat from the
/// top of the panel down to here and only THEN ramps out to [`CONTENT_TOP`], so this is the line
/// above which a scrolled shelf cannot be seen at all. Which makes it two things at once, and they
/// must stay one number — the argument [`draw`] passes that function, and the floor
/// [`results`] records its pointer rects against. A tile the user cannot see is a tile the pointer
/// must not be able to reach.
pub(crate) const CHROME_BOTTOM: f32 = FIELD.y + FIELD.h;
/// Terms kept. **Five**, which is `Search Screen.dc.html`'s number and was four here for a reason
/// that turned out to be arithmetic against a wrong constant: the header, the rows and the Clear
/// control all have to finish above the raised keyboard, and [`KEYBOARD_H`] claimed a panel 56px
/// taller than the real one. At the measured height a full block ends at 690 against a panel edge
/// of 756 — `recents`' own test grades the clearance rather than the count.
///
/// The sixth is DROPPED, not scrolled — a list you cannot see the end of asks to be paged, and
/// there is no paging in this product.
pub(crate) const MAX_RECENTS: usize = 5;

/// The RESULT BAND's content cross-fade — the Library's [`Xfade`] doing the same job one screen
/// over. A query change replaces everything below the field, and a replacement that CUTS reads as
/// a glitch rather than as an answer arriving.
///
/// It also closes a hole the regions could not close between them. `search::set_query` clears the
/// shelves synchronously on the keystroke and the answer lands a debounce later, so the band is
/// legitimately EMPTY in between — and three regions each correctly declined to draw anything for
/// `State::Searching`, which left the gap blank. [`Xfade::tick`]'s `ready` gate is exactly the
/// missing piece: the band holds at the floor while the query is in flight and rises with the new
/// shelves, so the empty frames are spent invisible instead of on screen.
///
/// The FIELD is deliberately outside it. You are typing into that control; fading it out under
/// your own keystroke would say the app had lost the query, which is the opposite of what happened.
static mut XF: Xfade = Xfade::new();

/// The content epoch this screen last saw, for the watchdog in [`update`]. A store replaced by
/// something OTHER than this screen — `search::reset()` on a profile switch — must fade too, and
/// the only way to notice that is to watch the generation rather than our own calls.
static mut EPOCH: u32 = 0;

fn xf() -> &'static mut Xfade {
    unsafe { &mut *addr_of_mut!(XF) }
}

/// Which region owns the remote. Not a focus INDEX — each zone keeps its own cursor, so leaving
/// the shelves for the field and coming back lands where you were.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Zone {
    /// The shared top strip. Reached with ▲ from the field; the screen does not own the pills.
    Strip,
    Field,
    Recents,
    Results,
}

/// What is DRAWN under the field. The ▼ handoff and [`draw`]'s region dispatch are the same
/// question asked twice, so they are one function — a screen whose focus can reach a region its
/// draw declined to run is the failure this replaces.
///
/// `Nothing` covers both of [`empty`]'s states (never searched, and searched with no match) and is
/// deliberately not split: neither one is focusable, so the state machine has no reason to tell
/// them apart and [`empty`] reads the store for the copy.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Below {
    Nothing,
    Recents,
    Results,
}

/// Everything a region needs to draw itself, snapshotted once per frame.
pub(crate) struct View {
    pub(crate) zone: Zone,
    /// Is the keyboard up? The field draws its caret only then, and the shelves freeze.
    pub(crate) editing: bool,
    /// Focused shelf, and the column within it.
    pub(crate) row: usize,
    pub(crate) col: usize,
    /// Focused recent term; `MAX_RECENTS` addresses the Clear control, which is a CONTROL and not
    /// another term — a verb never sits in the same column as the words you searched for.
    ///
    /// More precisely the Clear control is [`clear_index`] — one past the last term SHOWN, which is
    /// `MAX_RECENTS` only once the list is full. With two terms stored it is 2, because focus must
    /// never rest on a row that was never drawn.
    pub(crate) recent: usize,
    /// Vertical offset the shelves are drawn at (0 while the keyboard is up). POSITIVE moves the
    /// content UP: a shelf whose block top is `t` draws at `t - shift`.
    pub(crate) shift: f32,
    /// The insertion point, as a BYTE offset into `search::query()` — always on a char boundary
    /// (see [`caret`]). Meaningful only while `editing`.
    pub(crate) caret: usize,
    /// Is the caret in its ON phase this frame? See [`BLINK_MS`].
    pub(crate) caret_on: bool,
    /// How far the field is into its focused face, 0…1 — see [`HOT`]. A scalar and not a bool
    /// precisely so the region can cross-fade rather than branch.
    pub(crate) hot: f32,
}

/// What the screen reports for `app.rs` to perform. A screen never routes itself — the same rule
/// `ui/trail.rs` states as "this module decides nothing about screens".
pub(crate) enum Action {
    None,
    /// A result was activated. The node carries everything its page needs to mount, so `app.rs`
    /// hands it straight to `nav_open` without asking this screen anything else.
    Open(crate::ui::trail::Node),
}

static mut ZONE: Zone = Zone::Field;
static mut EDITING: bool = false;
static mut ROW: usize = 0;
static mut COL: usize = 0;
static mut RECENT: usize = 0;
/// The pill the strip cursor rests on while [`Zone::Strip`] holds focus. A cursor of its own — the
/// screen does not own the pills, but it does own where the ring is standing among them.
static mut STRIP: usize = 0;
/// The shelves' vertical scroll. A [`Spring`], so `ui::idle` hears the motion for free (both
/// integrators report) and this screen owes the frame gate no clock of its own.
static mut SCROLL: Spring = Spring::at(0.0);
/// How far the field is into its FOCUSED face, 0…1 — the one thing on this screen that changed
/// state as a cut.
///
/// The capsule swaps between two complete faces (`CONTROL_IDLE_FILL`/`CONTROL_IDLE_INK` and
/// `ACCENT`/`ACCENT_INK`), and it did it instantly while every other control in the app animates:
/// the tab strip's capsules TRAVEL between pills, a card's focus pop is a spring, the season tabs
/// share that spring by owner directive. A field that snapped from dark to white was the one place
/// focus arrived without motion, which reads as a repaint rather than as a control taking the
/// remote.
///
/// A [`Spring`], so `ui::idle` hears it for free and the fade cannot outlive its own frame — and at
/// `K_SCALE`, the focus-pop rate, because that is what this IS: the same event a card answers with
/// a pop, on a control whose only affordance is its fill.
static mut HOT: Spring = Spring::at(0.0);
/// The insertion point, a BYTE offset into `crate::search::query()`. Read through [`caret`], which
/// clamps it into the CURRENT string — the query is the store's and can be replaced under us (a
/// recents pick, `enter`'s pre-seed, a profile switch wiping the store), so a raw offset held here
/// is a promise this module cannot keep on its own.
static mut CARET: usize = 0;
/// Milliseconds the caret spends in each phase. A **blink**, which this screen deliberately did not
/// have: the field's own doc argued a solid bar because `ui::idle` cannot see a clock, so a blink
/// would either freeze under the present gate or defeat it. Both halves are answered rather than
/// traded — [`step_blink`] reports to the gate ONLY on the phase flip, so a field with the panel up
/// costs ~2 presents a second instead of 60, and a settled screen with the panel DOWN is untouched
/// (nothing blinks when nothing is being typed into). The user asked for it on the device, and a
/// text field with no blinking caret reads as a text field that is not accepting input.
const BLINK_MS: f32 = 530.0;
/// Time inside the current blink phase, in ms. Reset to 0 (a full ON phase) by every edit, so the
/// bar is solid while you are actually typing — the standard behaviour, and the reason a caret that
/// blinked THROUGH a keystroke would look like a dropped character.
static mut BLINK_T: f32 = 0.0;

/// **The** hit-rect store: every target under the field that a region PAINTED this frame, as the
/// [`Hit`] it answers with. Recorded at draw — recent rows and the Clear control by [`recents`]
/// through [`note_recent_rect`], result tiles by [`results`] through [`note_tile_rect`] — the
/// `library::TOOL_RECTS` idiom: this module owns the flow but not the rows, so the only rect it can
/// honestly hit-test is the one that was actually painted.
///
/// One list, because there was never a frame with both kinds in it: [`draw`] dispatches ONE region
/// under the field ([`below`]). Two stores meant two parks, two scans and the `w > 0.5` rule
/// written twice, and the tile half of it shipped with nothing ever filling it.
///
/// A `Vec`, because a shelf's tile count is the server's answer; it is CLEARED (never freed) each
/// frame, so the allocation is paid once — the Library's Sources panel does the same, with its
/// action list now carried inside the open `Menu::Source` variant rather than beside it.
///
/// The rects **cannot be derived here**: a shelf's horizontal offset is the scroll spring inside
/// its own `card_row::CardRow`, which lives in [`results`] and is invisible from this side. (This
/// said "even in principle", which was false — `results` could derive them and once did, in a
/// `tile_at` no caller ever reached. It declines to on purpose: a recorded rect is the tile as
/// PAINTED, focus pop included, where a derivation is a second expression to keep in step by hand.)
static mut HIT_R: Vec<(Hit, Rect)> = Vec::new();

// ---- the projection the regions draw from ----------------------------------------------------

fn zone() -> Zone {
    unsafe { addr_of!(ZONE).read() }
}
fn editing() -> bool {
    unsafe { addr_of!(EDITING).read() }
}

// ---- the caret -------------------------------------------------------------------------------

/// The insertion point, clamped into the query as it is RIGHT NOW and rounded DOWN to a char
/// boundary. Every reader goes through this: [`CARET`] is a byte offset into a string this module
/// does not own, so a stale one is the ordinary case (the store wipes and refills on every
/// keystroke, a recents pick replaces the whole term, a profile switch empties it) and slicing on
/// it un-clamped is a panic inside the SDL event loop.
fn caret() -> usize {
    let q = crate::search::query();
    let mut c = unsafe { addr_of!(CARET).read() }.min(q.len());
    while c > 0 && !q.is_char_boundary(c) {
        c -= 1;
    }
    c
}

/// Move the insertion point, and restart the blink at full ON — an edit or a caret move must leave
/// the bar visible, or the user cannot see where the next character will land.
fn set_caret(c: usize) {
    unsafe {
        *addr_of_mut!(CARET) = c;
        *addr_of_mut!(BLINK_T) = 0.0;
    }
    crate::ui::idle::invalidate();
}

/// The byte offset one character before/after `at`. `str::floor_char_boundary` is unstable, so this
/// is the hand-rolled pair — and it must be characters and never bytes, since a Cyrillic query
/// (device-confirmed: `q='су'` typed on the panel) is two bytes per letter and a byte step would
/// cut a codepoint in half.
fn prev_boundary(q: &str, at: usize) -> usize {
    let mut i = at.min(q.len());
    loop {
        if i == 0 {
            return 0;
        }
        i -= 1;
        if q.is_char_boundary(i) {
            return i;
        }
    }
}
fn next_boundary(q: &str, at: usize) -> usize {
    let mut i = (at + 1).min(q.len());
    while i < q.len() && !q.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// One commit from the panel, INSERTED or REPLACING — the difference being the panel's word
/// prediction, which it does not tell us about.
///
/// **Tapping a prediction is a replace, and the delete half never arrives.** Typing `sum` and
/// tapping the offered `summer` produced `sumsummer` (reported 2026-08-15). The event log settles
/// what the panel actually sends, which is the whole reason this is a rule and not a guess — three
/// single-character commits for the keys, then one commit of `"summer "`, and **nothing else**: no
/// `SDL_TEXTEDITING`, no backspace keys, no `delete_surrounding_text` reaching SDL at all. The
/// television simply assumes the field replaced the word it was predicting on.
///
/// So the signal is the commit's own LENGTH, which that log makes unambiguous: every key press
/// arrives as exactly one character (Cyrillic included — `с`, `у`, `б` came one per event), and
/// only the prediction bar ever commits more. [`replaces_word_at`] is that rule with its two
/// guards, and its doc carries what each one is protecting.
fn commit_text(text: &str) {
    let q = crate::search::query();
    if let Some(from) = replaces_word_at(q, caret(), text) {
        let mut s = q.to_string();
        s.replace_range(from..caret(), "");
        set_caret(from);
        crate::search::set_query(&s);
    }
    insert_text(text);
}

/// Where the word this commit REPLACES begins, or `None` to insert it plainly. Pure, because it is
/// the whole of the rule above and the cases that must NOT fire are the ones worth grading.
///
/// Two guards, each protecting a case that would otherwise eat the user's text:
///
/// 1. **More than one character.** A single-character commit is a key press, always — so typing
///    `a` twice stays `aa` rather than replacing itself, and the space bar is a space.
/// 2. **The caret is at the END.** A prediction is offered on the word you are finishing; a
///    multi-character commit landing mid-string is something else entirely (the `txt:` dev token
///    is one), and replacing there would delete text on either side of an insertion point the user
///    deliberately moved.
///
/// The word is the trailing run of non-whitespace before the caret, so a query that already ends in
/// a space has nothing to replace and the commit simply appends — which is exactly right for a
/// prediction offered on the NEXT word.
///
/// Deliberately **not** also requiring the commit to start with that word. A completion does
/// (`sum` → `summer`), but the same bar offers CORRECTIONS, and those do not; the two guards above
/// are what make the rule safe, and adding a prefix test would only leave corrections broken.
fn replaces_word_at(q: &str, caret: usize, text: &str) -> Option<usize> {
    if text.chars().count() < 2 || caret != q.len() {
        return None;
    }
    let head = q.get(..caret)?;
    // Past the separator, not onto it — and by its own BYTE length, since a multi-byte space (the
    // one `remote.rs` lost the app to) would otherwise leave the index mid-codepoint.
    let start = match head.rfind(char::is_whitespace) {
        Some(i) => i + head[i..].chars().next().map_or(1, char::len_utf8),
        None => 0,
    };
    (start < caret).then_some(start)
}

/// Insert committed text AT the caret, not at the end. The panel's `◀`/`▶` can put the insertion
/// point anywhere, so appending would type the new letter at the far end of a word the user had
/// just stepped into the middle of.
fn insert_text(text: &str) {
    let c = caret();
    let mut q = crate::search::query().to_string();
    q.insert_str(c, text);
    set_caret(c + text.len());
    crate::search::set_query(&q); // invalidates
}

/// Delete the character BEFORE the caret.
fn backspace() {
    let c = caret();
    if c == 0 {
        return;
    }
    let mut q = crate::search::query().to_string();
    let prev = prev_boundary(&q, c);
    q.replace_range(prev..c, "");
    set_caret(prev);
    crate::search::set_query(&q); // invalidates
}

/// The panel's **Clear all**: the whole query goes, caret to the front. Not "delete to the caret" —
/// the button is one word and it says all of it.
fn clear_query() {
    set_caret(0);
    crate::search::set_query("");
}

/// The panel's `◀`/`▶`. One character, clamped at both ends rather than wrapping: a caret that
/// jumped from the start of a term to its end on one press is a lost keystroke waiting to happen.
fn move_caret(sym: c_uint) {
    let q = crate::search::query();
    let c = caret();
    set_caret(if sym == SDLK_LEFT { prev_boundary(q, c) } else { next_boundary(q, c) });
}

/// Advance the blink and report to the frame gate on the FLIP alone — the whole reason a blinking
/// caret is affordable here (see [`BLINK_MS`]). Returns the phase to draw.
///
/// Pure but for the two statics, so the "reports exactly on the flip" rule is host-gradeable: it is
/// the property `ui/CLAUDE.md` demands of anything that animates from a clock, and the one that
/// `Xfade::tick` and `Spinner::draw` both shipped without.
fn step_blink(dt: f32, editing: bool) -> bool {
    if !editing {
        unsafe { *addr_of_mut!(BLINK_T) = 0.0 };
        return true; // no bar is drawn, and a phase left mid-cycle would blink on the next open
    }
    let was = blink_on(unsafe { addr_of!(BLINK_T).read() });
    let t = unsafe {
        *addr_of_mut!(BLINK_T) += dt * 1000.0;
        if *addr_of!(BLINK_T) >= BLINK_MS * 2.0 {
            *addr_of_mut!(BLINK_T) -= BLINK_MS * 2.0;
        }
        addr_of!(BLINK_T).read()
    };
    let now = blink_on(t);
    if now != was {
        crate::ui::idle::invalidate();
    }
    now
}

/// Which half of the cycle `t` (ms) falls in. Pure, and separate so the flip test above and the
/// tests below read the same expression.
fn blink_on(t: f32) -> bool {
    t < BLINK_MS
}

/// The Clear control's index: one past the last term SHOWN. See [`View::recent`].
pub(crate) fn clear_index() -> usize {
    recents::count().min(MAX_RECENTS)
}

/// Is there a query the SERVER was actually asked about? [`crate::search::MIN_QUERY`], **not**
/// "non-empty" — below that the store stays `Idle` and sends nothing, so counting one typed
/// character as a live search would swap the recents list for a never-searched statement for
/// exactly one keystroke, and swap it back on a backspace. A flicker at the start of every search.
fn has_query() -> bool {
    crate::search::query().trim().chars().count() >= crate::search::MIN_QUERY
}

/// [`below`]'s rule over nothing but the three facts it reads. Pure for the reason
/// `library::readout_of` is: it is the decision this screen turns on, and neither store behind it
/// can be seeded from a host — the recents list is the session file's and the shelves are a live
/// server answer — so a test of the live function could only ever reach one of its four cases.
fn below_of(has_query: bool, n_recents: usize, n_shelves: usize) -> Below {
    if !has_query {
        if n_recents > 0 {
            Below::Recents
        } else {
            Below::Nothing
        }
    } else if n_shelves == 0 {
        Below::Nothing
    } else {
        Below::Results
    }
}

/// What is drawn under the field right now — the ▼ handoff and the draw dispatch, once.
pub(crate) fn below() -> Below {
    below_of(has_query(), recents::count(), crate::search::shelves().len())
}

/// Is the shelf flow held at zero? The two freezes, and the ONLY thing this module says about the
/// flow's shape — [`results`] owns the geometry and the reveal rule (see the module doc's Scroll
/// note).
///
/// **While the keyboard is up**, because the result set has to be stable under the user's eyes
/// while they are still typing, and the layout is sized so the first shelf's whole row lands above
/// the panel's top edge with no scroll needed at all. **And whenever the shelves do not hold
/// focus**, so ▼ off the field can land on shelf 0 without the page jumping.
///
/// Pure, so the host suite can grade the rule the device only ever shows as motion.
fn scroll_frozen(zone: Zone, editing: bool) -> bool {
    editing || zone != Zone::Results
}

fn scroll_target() -> f32 {
    if scroll_frozen(zone(), editing()) {
        return 0.0;
    }
    // Minimal reveal, from where the spring actually IS: `card_row::reveal` returns `cur` unchanged
    // for a block already on screen, so feeding it anything but the live position would re-seat a
    // page that did not need to move. A row index past the end (the one frame between a landing
    // shrinking the set and `clamp_focus` re-seating the cursor) answers 0 rather than panicking.
    let (cur, row) = unsafe { (addr_of!(SCROLL).read().pos, addr_of!(ROW).read()) };
    results::reveal_shelf(cur, row)
}

/// How many items each shelf holds, into a caller-owned buffer. There is at most one shelf per
/// [`crate::search::KINDS`], so the whole ragged grid fits a fixed array and the per-frame
/// [`clamp_focus`] costs no allocation.
fn shelf_lens(buf: &mut [usize; crate::search::KINDS.len()]) -> &[usize] {
    let sh = crate::search::shelves();
    let n = sh.len().min(buf.len());
    for (slot, s) in buf.iter_mut().zip(sh.iter()) {
        *slot = s.items.len();
    }
    &buf[..n]
}

/// Seat `(row, col)` inside a ragged grid of shelf lengths — the ONE clamp, shared by the per-frame
/// re-seat and every d-pad step, so a shelf that shrank under the cursor cannot be handled two
/// ways. `None` means there is no shelf to sit in at all.
fn seat(row: usize, col: usize, lens: &[usize]) -> Option<(usize, usize)> {
    let last = lens.len().checked_sub(1)?;
    let r = row.min(last);
    Some((r, col.min(lens[r].saturating_sub(1))))
}

/// One d-pad step inside the shelves, over nothing but their LENGTHS. `None` = focus leaves for the
/// field, which is both ▲ off shelf 0 and "there are no shelves any more" — the same destination,
/// because the field is the one region that is always drawn.
///
/// Pure for the same reason [`below_of`] is, and because this is where the ragged-grid arithmetic
/// actually goes wrong: a column carried onto a shorter shelf.
fn step_results(sym: c_uint, row: usize, col: usize, lens: &[usize]) -> Option<(usize, usize)> {
    let (r, c) = seat(row, col, lens)?;
    if sym == SDLK_LEFT {
        Some((r, c.saturating_sub(1)))
    } else if sym == SDLK_RIGHT {
        Some((r, (c + 1).min(lens[r].saturating_sub(1))))
    } else if sym == SDLK_UP {
        if r == 0 {
            None
        } else {
            seat(r - 1, c, lens)
        }
    } else if sym == SDLK_DOWN && r + 1 < lens.len() {
        seat(r + 1, c, lens)
    } else {
        Some((r, c))
    }
}

/// Snapshot the state machine for this frame's regions.
///
/// `pub(crate)` for the focus probe (`crate::focusprobe`), which needs all five of the fields this
/// already builds — zone, editing, row/col, recents cursor, caret. `View` was `pub(crate)` before
/// this; only the constructor was private, so nothing new is exposed but the snapshot itself.
pub(crate) fn view() -> View {
    unsafe {
        View {
            zone: addr_of!(ZONE).read(),
            editing: addr_of!(EDITING).read(),
            row: addr_of!(ROW).read(),
            col: addr_of!(COL).read(),
            recent: addr_of!(RECENT).read(),
            shift: addr_of!(SCROLL).read().pos,
            caret: caret(),
            // Read, never stepped: `update` owns the clock, so every region drawing this frame sees
            // one phase and the flip cannot land between two of them.
            caret_on: blink_on(addr_of!(BLINK_T).read()),
            hot: addr_of!(HOT).read().pos.clamp(0.0, 1.0),
        }
    }
}

/// Does the field hold the remote? The spring's target, and the predicate `field` used to compute
/// for itself — hoisted here because a state machine's own state belongs to the state machine, and
/// because the spring and the draw reading two expressions of one question is how they come to
/// disagree for a frame.
///
/// The keyboard being up counts as focus by itself: it is the strongest possible statement that
/// this control has the remote, and a field that dimmed while you typed into it would be saying the
/// opposite.
fn field_hot() -> bool {
    zone() == Zone::Field || editing()
}

// ---- entry / exit ----------------------------------------------------------------------------

/// Mount the screen with `q` in the field, REPLACING whatever was there. The only caller is the
/// `/tmp/plxnative-search=<query>` boot trigger, whose whole job this is — a headless screenshot
/// cannot type, so a seeded field is the only way an automated run reaches a populated result set.
///
/// **Every interactive way in is [`resume`], not this.** Pressing the Search pill is a
/// navigation, and a navigation to a peer screen restores it (`library::enter`'s `restore_view`,
/// one screen over); wiping the query the user is still reading is not an entry, it is a reset
/// they did not ask for. See [`mount`] for what the two share and where they part.
pub(crate) fn enter(q: &str) {
    mount(Some(q));
}

/// Return to the screen the way this profile left it — the QUERY, the shelves under it and both
/// cursors intact. Every interactive entry ([`crate::ui::widgets::Pill::Search`], from Home, from
/// the Library, by remote or by pointer) comes through here.
///
/// **A first entry needs no special case, which is why there is no "have I been here" flag.** The
/// store starts empty and stays empty until something is typed, so a resume onto a profile that
/// has never searched seats exactly what [`enter`]`("")` would have: an empty field over the
/// recents list, every cursor at 0. The two only diverge once there IS state, which is the whole
/// point.
pub(crate) fn resume() {
    mount(None);
}

/// The one mount. `seed` is `Some` only for the boot trigger; `None` RETURNS to the screen.
///
/// What both do is seat the screen's ENTRY POSTURE — the field holds the remote, the keyboard is
/// down, the strip cursor is under our own pill, the shelf flow is at the top. None of that is
/// state the user would miss: the flow is frozen at 0 whenever the shelves do not hold focus
/// anyway (`scroll_frozen`), and the ▼ handoff deliberately lands on shelf **0** rather than the
/// shelf last left, with the COLUMN remembered — the module doc's Scroll note argues both.
///
/// What only the seeded form does is replace the query and re-park the cursors, because a seed is
/// a different search and the cursors belong to the results of the old one.
fn mount(seed: Option<&str>) {
    // Through `stop()`, not by clearing the flag: `textinput` keeps its OWN `STARTED`, and
    // `start()` is a no-op while that is set. Clearing `EDITING` alone would leave the two
    // disagreeing, after which no later press could ever raise the panel again — the same symptom
    // as the driver's reopen wedge, with a cause entirely our own. `stop()` is guarded, so this
    // costs nothing on the normal arrival where the panel was never up.
    leave();
    unsafe {
        *addr_of_mut!(ZONE) = Zone::Field;
        // The strip opens under our own pill, the one the screen is drawn as SELECTED on, so ▲
        // lands where the eye already is. Re-seated on a RESUME too: the cursor may have been left
        // parked on the Home pill by the very press that took the user away, and a Search screen
        // drawing its focus ring on somebody else's pill is the one thing worse than losing it.
        *addr_of_mut!(STRIP) = crate::ui::widgets::search_pill();
        (*addr_of_mut!(SCROLL)) = Spring::at(0.0);
        // SEATED, not sprung: the screen mounts with the field already focused, so there is no
        // state CHANGE to animate — gliding the fill up from idle on arrival would be a second
        // fade under `nav`'s page transition, and would report motion to `ui::idle` for the first
        // ~8 frames of a screen that is not moving.
        (*addr_of_mut!(HOT)) = Spring::at(1.0);
    }
    if let Some(q) = seed {
        unsafe {
            *addr_of_mut!(ROW) = 0;
            *addr_of_mut!(COL) = 0;
            *addr_of_mut!(RECENT) = 0;
        }
        // Stripped of control bytes, and NUL is the one that bites: this arrives from a FILE, and
        // an interior NUL survives every `String` operation on the way in while `CString::new`
        // refuses it at the far end — so the field's capsule and the empty state's own statement
        // both go blank over a query the app still believes it is holding (`empty::draw` returns
        // early; `field::draw` truncates for the same reason and keeps that guard as a belt).
        // Filtered HERE because this is where untrusted text enters the query: the panel's own
        // commits cannot carry one (`textinput::decode_text_at` cuts the string AT the NUL) and a
        // recents pick is already refused by `recents::usable`.
        let q: String = q.chars().filter(|c| !c.is_control()).collect();
        crate::search::set_query(&q);
        // The caret lands at the END of a pre-seeded term, which is where a person who had just
        // typed it would be standing — and is what makes the boot trigger's field behave like a
        // typed one.
        set_caret(q.len());
    }
    // Arriving is a content change like any other. `mount` (not `reload`) because there is nothing
    // on screen to fade OUT — the page transition has already covered that half.
    xf().mount();
    unsafe { EPOCH = crate::search::query_gen() };
    crate::ui::idle::invalidate();
}

/// Leave it. Dismissing the keyboard here rather than at the press is deliberate: the panel must
/// come down at the fade floor, with the page, not a frame before it while the old screen is still
/// on screen behind it.
///
/// **It is a WITHDRAWAL and records nothing**, which is what makes it the right call for the other
/// caller too: `app.rs`'s background arm, where the OS has taken the screen away and the
/// compositor's IME went with it. Neither exit is the user saying "that is the search I meant" —
/// that moment is [`leave_field`], and only it commits the term to the recents list.
pub(crate) fn leave() {
    unsafe { *addr_of_mut!(EDITING) = false };
    crate::textinput::stop();
}

// ---- per-frame -------------------------------------------------------------------------------

pub(crate) fn update(dt: f32) {
    // Typed text FIRST: a commit this frame changes the query, `set_query` wipes the shelves under
    // it synchronously, and the clamp below is what has to see that.
    pump_text();
    // The roster's LIBRARY names, for the scope line beside the field. Only the Library screen runs
    // the full `browse::pump`, so a boot straight into this one knew it had two sources and could
    // not name either of them.
    crate::browse::discover_pump();
    crate::search::pump(dt);
    // After `pump_text`, so a frame that typed a character draws a solid bar rather than whichever
    // phase the clock happened to be in.
    step_blink(dt, editing());
    clamp_focus();
    // AFTER the clamp, so a zone the clamp just handed back to the field lights it on the same
    // frame rather than one late.
    unsafe { (*addr_of_mut!(HOT)).step(f32::from(u8::from(field_hot())), K_SCALE, dt) };
    unsafe { (*addr_of_mut!(SCROLL)).step(scroll_target(), K_SCROLL, dt) };
    // The shelves' own springs, in the UPDATE phase — after `clamp_focus`, so they are handed a
    // seated cursor, and before `idle::should_present`, which is the whole reason this line exists.
    // `results` used to advance from a clock inside its draw and hand-roll a discrete report,
    // because a `note_spring` raised during frame N's draw is cleared by frame N+1's
    // `idle::frame_begin` before anything reads it; from here both integrators speak for
    // themselves, as they do on every other screen.
    results::update(dt, &view());

    // `ready` is "the band has something to say", not "there are items": a query that legitimately
    // matched nothing is an ANSWER and its read-out has to rise like any other content, so
    // `Ready`-with-no-shelves counts. Only `Searching` holds the fade down — the same distinction
    // `State` exists to make, and the same shape as `library`'s `readout() != Loading`.
    let ready = crate::search::state() != crate::search::State::Searching;
    xf().tick(dt, ready);
    // Watchdog, read AFTER the tick so our own bump is never mistaken for a foreign one: a store
    // wiped by a profile switch still gets a fade rather than a cut.
    let g = crate::search::query_gen();
    if g != unsafe { addr_of!(EPOCH).read() } {
        unsafe { EPOCH = g };
        if !xf().is_swapping() {
            xf().mount();
        }
    }
}

/// Drain the keyboard and insert what it committed.
///
/// **A commit arriving while this screen thinks it is not editing ADOPTS the panel rather than
/// being dropped**, and that rule is the answer to a class of bug rather than a nicety. A character
/// can only come from a panel that is on screen, so the two states disagreeing means OURS is stale
/// — and every way that happens ends the same way for the user: the keyboard is up, they are
/// typing, and nothing appears. Device-observed twice in one day, from two different causes (the
/// television dismissing and re-raising its own panel, and a `SDLK_DOWN` the PANEL forwarded to us
/// being read as "leave the field"), which is exactly why this is keyed on the one signal that
/// cannot lie instead of on detecting each cause.
///
/// It replaces the opposite rule — "text arriving while the panel is DOWN is the IME's parting shot
/// and is dropped" — which was defending against a case that cannot occur on the television:
/// `textinput::start` clears the queue, and text events are OFF until it is called. On a desktop
/// they are on from `SDL_VideoInit`, so the adoption is gated on there being a real panel
/// ([`crate::textinput::available`]) and the simulator keeps the old behaviour exactly.
fn pump_text() {
    let commits = crate::textinput::drain();
    if commits.is_empty() {
        return;
    }
    if !editing() {
        if !crate::textinput::available() {
            return; // no panel on this platform, so this is a desktop keystroke meant for nothing
        }
        crate::textinput::adopt();
        unsafe { *addr_of_mut!(EDITING) = true };
        // The insertion point goes to the END: we are joining a session already in progress and
        // have no idea where the panel thinks the caret is.
        set_caret(crate::search::query().len());
    }
    for s in &commits {
        commit_text(s);
    }
}

/// Keep every cursor inside what is actually there — after a landing, after a keystroke wiped the
/// shelves, and after the strip grew or lost a pill. A zone whose region has stopped being drawn
/// hands focus back to the field, which is the one place that is always there.
fn clamp_focus() {
    unsafe {
        match addr_of!(ZONE).read() {
            Zone::Results => {
                let mut buf = [0usize; crate::search::KINDS.len()];
                let lens = shelf_lens(&mut buf);
                match (below() == Below::Results).then(|| seat(addr_of!(ROW).read(), addr_of!(COL).read(), lens)).flatten() {
                    Some((r, c)) => {
                        *addr_of_mut!(ROW) = r;
                        *addr_of_mut!(COL) = c;
                    }
                    // The zone goes, the CURSOR stays — the same answer `move_focus`'s own `None`
                    // arm gives, and the module doc's "the column is remembered" promise. Zeroing
                    // it here (which this did) meant every keystroke, which wipes the store
                    // synchronously, silently threw the column away before the answer came back.
                    // Nothing downstream needs it seated: `focused_item` and `scroll_target` both
                    // bounds-check, and ▼ re-seats through `seat`.
                    None => *addr_of_mut!(ZONE) = Zone::Field,
                }
            }
            Zone::Recents => {
                if below() != Below::Recents {
                    *addr_of_mut!(ZONE) = Zone::Field;
                    return;
                }
                *addr_of_mut!(RECENT) = addr_of!(RECENT).read().min(clear_index());
            }
            Zone::Strip => {
                *addr_of_mut!(STRIP) = addr_of!(STRIP).read().min(strip_last());
            }
            Zone::Field => {}
        }
    }
}

fn strip_last() -> usize {
    crate::ui::widgets::tab_count().saturating_sub(1)
}

pub(crate) fn draw() {
    crate::gfx::frame_clear(crate::ui::theme::CLEAR_RGB.0, crate::ui::theme::CLEAR_RGB.1, crate::ui::theme::CLEAR_RGB.2);
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    let pk = Painter::root().alpha(crate::ui::nav::chrome_alpha());
    let v = view();
    // Drop last frame's hit rects BEFORE the regions record this frame's, so a row or a tile that
    // has stopped being drawn cannot still be clicked.
    unsafe { (*addr_of_mut!(HIT_R)).clear() };

    // Everything BELOW the field rides the content fade; the field itself does not (see [`XF`]).
    let pg = p.alpha(xf().alpha());
    match below() {
        Below::Recents => recents::draw(pg, &v),
        Below::Nothing => empty::draw(pg, &v),
        Below::Results => {}
    }
    // Unconditional on a live query, exactly as before the dispatch above was folded into `below`:
    // with no shelves it draws nothing, and the in-flight read-out is the region's own to decide.
    // The SAME `has_query` the dispatch used, so a sub-`MIN_QUERY` string cannot put this region on
    // screen for a request that was never sent.
    if has_query() {
        results::draw(pg, &v);
    }
    // …THEN the navigation bar's scrim, THEN the chrome over it. The ORDER is the fix: this screen
    // drew the field first and the shelves over it, so a poster crossing the capsule was stopped
    // only by `results`' scissor — a razor edge in open ground, which is what got photographed.
    // `widgets::nav_scrim` is the shared treatment (the Library's, and the design system's
    // `route-screen` card); the scissor is a bound again, cutting inside the opaque band.
    crate::ui::widgets::nav_scrim(p, CHROME_BOTTOM, CONTENT_TOP, v.shift);
    field::draw(p, &v);

    // `CHIP_D`, not a literal 54: Home draws this same chip at 60, and Home<->Search is a
    // chrome-CONTINUOUS cross-fade, so a six-pixel disagreement was visible as the avatar shrinking
    // mid-transition — on this screen alone.
    let d = crate::ui::widgets::CHIP_D;
    crate::ui::widgets::profile_chip(pk, Rect::new(crate::ui::consts::MARGIN_X, crate::ui::widgets::TOP_BAR_Y, d, d), 0.0);
    crate::ui::widgets::draw_tab_row(pk);
}

/// Record the rect a recent-term row (or, at [`MAX_RECENTS`], the Clear control) was DRAWN at.
/// Called by [`recents`] from its draw; see [`HIT_R`].
///
/// An index past the Clear control is DROPPED, never clamped onto it: the store is documented to
/// hold more terms than the screen has room for, so a drawer that looped the whole list would
/// otherwise stamp a term's rect over Clear's slot — and hovering that term would then park the
/// cursor on the control that wipes the list.
pub(crate) fn note_recent_rect(i: usize, r: Rect) {
    if i <= MAX_RECENTS {
        note(Hit::Recent(i), r);
    }
}

/// Record the rect a result tile was DRAWN at, scroll and pop included. Called by [`results`] from
/// its draw; see [`HIT_R`].
pub(crate) fn note_tile_rect(row: usize, col: usize, r: Rect) {
    note(Hit::Tile(row, col), r);
}

/// The one recorder. Push order is DRAW order and [`find_rect`] takes the first match, which is
/// what keeps a focused tile's magnified rect from stealing a click that landed on the neighbour
/// drawn under it (`card_row::strip` draws the focused tile LAST).
fn note(h: Hit, r: Rect) {
    unsafe { (*addr_of_mut!(HIT_R)).push((h, r)) };
}

// ---- input: focus ------------------------------------------------------------------------------

/// Is focus on something a press would OPEN (as opposed to a control that toggles or a field)?
/// `app.rs`'s press machinery asks this to decide whether OK is a tvOS click.
///
/// Asked of [`open_focused`] itself rather than of "is there an item here", so it cannot come to
/// disagree with what OK will actually do. That distinction is not academic on this screen: a
/// COLLECTION tile is an item and opens nothing (see [`open_focused`]), so the cheaper test would
/// arm a press that dips the card, bounces it and commits nothing — precisely the affordance
/// `library::focus_is_card` exists to refuse.
pub(crate) fn focus_is_card() -> bool {
    zone() == Zone::Results && matches!(open_focused(), Action::Open(_))
}

fn focused_item() -> Option<(crate::search::Kind, &'static crate::search::Item)> {
    let (r, c) = unsafe { (addr_of!(ROW).read(), addr_of!(COL).read()) };
    let shelf = crate::search::shelves().get(r)?;
    Some((shelf.kind, shelf.items.get(c)?))
}

pub(crate) fn move_focus(sym: c_uint) {
    unsafe {
        match addr_of!(ZONE).read() {
            Zone::Strip => {
                if sym == SDLK_LEFT {
                    *addr_of_mut!(STRIP) = addr_of!(STRIP).read().saturating_sub(1);
                } else if sym == SDLK_RIGHT {
                    *addr_of_mut!(STRIP) = (addr_of!(STRIP).read() + 1).min(strip_last());
                } else if sym == SDLK_DOWN {
                    *addr_of_mut!(ZONE) = Zone::Field;
                }
            }
            Zone::Field => {
                if sym == SDLK_UP {
                    leave_field(Zone::Strip);
                    *addr_of_mut!(STRIP) = crate::ui::widgets::search_pill();
                } else if sym == SDLK_DOWN {
                    // ONLY into something that is drawn — the rule this screen is built around.
                    match below() {
                        Below::Recents => {
                            leave_field(Zone::Recents);
                            *addr_of_mut!(RECENT) = addr_of!(RECENT).read().min(clear_index());
                        }
                        Below::Results => {
                            leave_field(Zone::Results);
                            // Shelf 0, and the remembered column clamped into it — see the Scroll
                            // note on why the row is not restored.
                            let mut buf = [0usize; crate::search::KINDS.len()];
                            let lens = shelf_lens(&mut buf);
                            let (r, c) = seat(0, addr_of!(COL).read(), lens).unwrap_or((0, 0));
                            *addr_of_mut!(ROW) = r;
                            *addr_of_mut!(COL) = c;
                        }
                        // Nothing below: focus stays put, and the keyboard stays up with it. A ▼
                        // that neither moved nor dismissed is the honest report of a screen with
                        // one thing on it.
                        Below::Nothing => {}
                    }
                }
            }
            Zone::Recents => {
                let last = clear_index();
                if sym == SDLK_UP {
                    if addr_of!(RECENT).read() == 0 {
                        *addr_of_mut!(ZONE) = Zone::Field;
                    } else {
                        *addr_of_mut!(RECENT) = addr_of!(RECENT).read() - 1;
                    }
                } else if sym == SDLK_DOWN {
                    *addr_of_mut!(RECENT) = (addr_of!(RECENT).read() + 1).min(last);
                }
                // ◀/▶ do nothing: the list is one column wide, and Clear sits under it rather
                // than beside it.
            }
            Zone::Results => {
                let mut buf = [0usize; crate::search::KINDS.len()];
                let lens = shelf_lens(&mut buf);
                match step_results(sym, addr_of!(ROW).read(), addr_of!(COL).read(), lens) {
                    Some((r, c)) => {
                        *addr_of_mut!(ROW) = r;
                        *addr_of_mut!(COL) = c;
                    }
                    // ▲ off shelf 0 (or a set that has emptied): back to the field, and the cursor
                    // is LEFT where it was — the shelves are still there and ▼ returns to them.
                    None => *addr_of_mut!(ZONE) = Zone::Field,
                }
            }
        }
    }
    crate::ui::idle::invalidate();
}

/// Focus is leaving the field for `to`: the keyboard comes down and the term is recorded. Leaving
/// the field is the only moment the user ever says "that is the search I meant" — every other exit
/// (BACK, a route change) is a withdrawal and deliberately remembers nothing.
fn leave_field(to: Zone) {
    commit_query();
    unsafe { *addr_of_mut!(ZONE) = to };
}

fn start_editing() {
    unsafe { *addr_of_mut!(EDITING) = true };
    // Opening the panel puts the insertion point after whatever is already in the field — you are
    // resuming a term, not editing its front. Also restarts the blink at full ON.
    set_caret(crate::search::query().len());
    crate::textinput::start();
    crate::ui::idle::invalidate();
}

/// Dismiss the keyboard and record the term. A query below `search::MIN_QUERY` never reached the
/// server, so it is not something the user searched and does not join the list.
fn commit_query() {
    if !editing() {
        return;
    }
    unsafe { *addr_of_mut!(EDITING) = false };
    crate::textinput::stop();
    let q = crate::search::query().trim().to_string();
    if q.chars().count() >= crate::search::MIN_QUERY {
        recents::remember(&q);
    }
    crate::ui::idle::invalidate();
}

// ---- input: keys, OK, BACK -----------------------------------------------------------------

/// A key that is neither a d-pad move nor OK/BACK. Answers whether the screen CONSUMED it, so
/// `app.rs`'s key arm can offer one here before falling through to its own handling.
///
/// **This is the panel's edit protocol, and every key in it is one the television sends us.** The
/// TV's keyboard commits its printable characters as `SDL_TEXTINPUT` ([`crate::textinput::drain`])
/// and then delegates every EDIT to the app, because the app owns the text: its delete arrives as
/// `SDLK_BACKSPACE` (wcode 42), its **Clear all** as `SDLK_CLEAR` (wcode 156), and its `◀`/`▶` as
/// ordinary `SDLK_LEFT`/`SDLK_RIGHT` (wcodes 80/79) meaning *move the caret*. All four measured on
/// the dev set 2026-08-15.
///
/// Only backspace was handled, so three of the panel's own buttons visibly did nothing — the
/// arrows because `move_focus` has no horizontal move in [`Zone::Field`], Clear because nothing
/// matched its sym at all. That is not a missing feature; it is half of typing.
///
/// **Gated on `editing`**, so with the panel down ◀/▶ stay whatever the screen says they are.
pub(crate) fn key(sym: c_uint) -> bool {
    if !editing() {
        return false;
    }
    if sym == SDLK_BACKSPACE {
        backspace();
    } else if sym == SDLK_CLEAR {
        clear_query();
    } else if sym == SDLK_LEFT || sym == SDLK_RIGHT {
        move_caret(sym);
    } else {
        return false;
    }
    true
}

pub(crate) fn on_ok() -> Action {
    match zone() {
        // The pills are not this screen's; `app.rs` reads `focused_pill()` and performs the press.
        Zone::Strip => Action::None,
        Zone::Field => {
            if editing() {
                commit_query();
            } else {
                start_editing();
            }
            Action::None
        }
        Zone::Recents => {
            let terms = recents::terms();
            let i = unsafe { addr_of!(RECENT).read() };
            if i >= terms.len().min(MAX_RECENTS) {
                recents::clear();
            } else {
                let t = terms[i].clone();
                crate::search::set_query(&t);
                set_caret(t.len()); // as if it had just been typed — see `start_editing`
                recents::remember(&t); // picking a term is searching it: it moves to the front
            }
            // Either way the list under the cursor has just changed shape, and the answer for the
            // picked term has not landed yet — the field is the one place that is always there.
            unsafe {
                *addr_of_mut!(ZONE) = Zone::Field;
                *addr_of_mut!(RECENT) = 0;
            }
            crate::ui::idle::invalidate();
            Action::None
        }
        Zone::Results => open_focused(),
    }
}

/// The focused result as a trail [`Node`](crate::ui::trail::Node), or nothing.
///
/// **A COLLECTION opens nothing, deliberately.** Its `Directory` row carries no `ratingKey`, so it
/// is not a detail page, and what it does carry (`key` = `/library/sections/1/all?collection=…`) is
/// a filtered LISTING that `browse.rs` has no query for — there is no `Node` this could build and
/// no screen to mount it on. Reporting `Action::None` leaves the tile inert rather than routing
/// somewhere that would be a lie about what was pressed; the shelf still earns its place, because
/// "this collection exists" is the answer a lot of searches are actually after. When browse learns
/// a collection listing, this arm is where it lands.
fn open_focused() -> Action {
    let Some((kind, item)) = focused_item() else { return Action::None };
    match (kind, item) {
        (_, crate::search::Item::Media(m)) => Action::Open(crate::ui::trail::Node::Detail {
            sid: m.sid,
            rk: m.rk.clone(),
            // A page entered and not yet left — `detail::Spot`'s own definition of its default.
            spot: Default::default(),
        }),
        (crate::search::Kind::Person, crate::search::Item::Tag(t)) => {
            // `metadata::Credit::person_key`'s rule, which the cast row already opens people by:
            // the LOCAL numeric id addresses `/library/people/{id}/media`, and the `tagKey` guid is
            // the only id plex.tv's biography provider answers to. Either can be missing; passing
            // one for the other 404s, so both ride along.
            let key = if t.id.is_empty() || t.id == "0" { t.tag_key.clone() } else { t.id.clone() };
            if key.is_empty() {
                return Action::None;
            }
            Action::Open(crate::ui::trail::Node::Person {
                sid: t.sid,
                key,
                guid: t.tag_key.clone(),
                name: t.name.clone(),
                thumb: t.thumb.clone(),
            })
        }
        (_, crate::search::Item::Tag(_)) => Action::None, // a collection — see the doc above
    }
}

/// BACK. `false` = "I am done, leave the screen" — the same contract `library::back()` has, and
/// the reason `app.rs`'s BACK arm can treat every screen alike.
///
/// Dismissing the panel here is a WITHDRAWAL, not a commit: unlike [`leave_field`], it records no
/// term. BACK is the press that means "not that", and a list of the searches the user backed out of
/// is a list of their mistakes.
pub(crate) fn back() -> bool {
    if !editing() {
        return false;
    }
    unsafe { *addr_of_mut!(EDITING) = false };
    crate::textinput::stop();
    crate::ui::idle::invalidate();
    true
}

/// The pill the strip should show as SELECTED while this screen is up.
pub(crate) fn selected_pill() -> c_int {
    crate::ui::widgets::search_pill() as c_int
}

/// The pill holding remote FOCUS, or -1 when focus is not in the strip. Clamped on read as well as
/// in [`clamp_focus`]: `app.rs` hands this to `widgets::tab_row_update` BEFORE it calls
/// [`update`], so on the frame a server appears or drops the cursor is one update behind the row.
pub(crate) fn focused_pill() -> c_int {
    match zone() {
        Zone::Strip => unsafe { addr_of!(STRIP).read() }.min(strip_last()) as c_int,
        _ => -1,
    }
}

// ---- input: pointer ----------------------------------------------------------------------------

/// What the pointer is over. `None` is BLANK GROUND, and the distinction is the whole reason this
/// is a function rather than two scans: a miss must never activate anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Pill(usize),
    Field,
    Recent(usize),
    Tile(usize, usize),
}

/// The one scan over [`HIT_R`], pure so the host suite can reach it — the live store is filled by
/// a draw the host cannot run.
///
/// Two rules, both of which used to be written once per store. **`w > 0.5` first**: a recorded rect
/// can legitimately be zero-SIZE (a culled or fully clipped tile) and `Rect::contains` is
/// inclusive, so a zero rect is clickable at exactly its corner — `library::STATUS_BTN`'s lesson.
/// **And the target's region must be the one drawn NOW**: the rects are last frame's, so between
/// that draw and this press [`below`] may have changed under them, and a stale recents row that
/// still answered would put focus in a zone whose region is gone — with a click on it running the
/// Clear control that wiped the list.
fn find_rect(rects: &[(Hit, Rect)], below: Below, mx: f32, my: f32) -> Option<Hit> {
    rects
        .iter()
        .find(|&&(h, r)| {
            r.w > 0.5
                && matches!((h, below), (Hit::Recent(_), Below::Recents) | (Hit::Tile(..), Below::Results))
                && r.contains(mx, my)
        })
        .map(|&(h, _)| h)
}

/// The one hit test, shared by hover and click. Every rect here was RECORDED at draw (or is a
/// constant, for the field), `library.rs`'s discipline: what the pointer addresses and what the eye
/// sees are the same rect rather than two derivations that agree by luck.
fn hit(mx: f32, my: f32) -> Option<Hit> {
    if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
        return Some(Hit::Pill(i.min(strip_last())));
    }
    if FIELD.contains(mx, my) {
        return Some(Hit::Field);
    }
    let rects = unsafe { addr_of!(HIT_R).as_ref() }?;
    find_rect(rects, below(), mx, my)
}

/// Move focus onto what the pointer found.
///
/// Leaving the FIELD goes through [`leave_field`], exactly as the remote does. Assigning `ZONE`
/// directly (which this did) left `EDITING` true underneath another zone, and a true `EDITING`
/// freezes the shelf scroll unconditionally — so ▼ walked focus off the bottom of the panel with
/// nothing moving, the very "screen the remote has silently stopped addressing" the module doc
/// says this state machine exists to prevent. It also skipped the term's only commit point.
fn focus(h: Hit) {
    let to = match h {
        Hit::Pill(_) => Zone::Strip,
        Hit::Field => Zone::Field,
        Hit::Recent(_) => Zone::Recents,
        Hit::Tile(..) => Zone::Results,
    };
    unsafe {
        if to != Zone::Field && zone() == Zone::Field {
            leave_field(to);
        } else {
            *addr_of_mut!(ZONE) = to;
        }
        match h {
            Hit::Pill(i) => *addr_of_mut!(STRIP) = i,
            Hit::Field => {}
            // The Clear control records itself at MAX_RECENTS whatever the list length; the cursor
            // addresses it at `clear_index`.
            Hit::Recent(i) => *addr_of_mut!(RECENT) = if i >= MAX_RECENTS { clear_index() } else { i.min(clear_index()) },
            Hit::Tile(r, c) => {
                *addr_of_mut!(ROW) = r;
                *addr_of_mut!(COL) = c;
            }
        }
    }
    crate::ui::idle::invalidate();
}

/// Hover moves focus — and only onto something the pointer is actually over.
pub(crate) fn pointer_focus(mx: f32, my: f32) {
    // **A hover is not a decision.** While the keyboard is up, moving focus out of the field runs
    // `leave_field`, which COMMITS the partial query to the recents file and calls
    // `textinput::stop()`. The Magic Remote's pointer wakes on the smallest movement, so a drift
    // across the top strip — a gesture the user never made as input — would file half a term and
    // put the panel away mid-word.
    //
    // Deliberately the whole hover and not just the field→elsewhere edge: while editing, the field
    // IS where focus belongs, and a pointer that merely passes over a tile has said nothing about
    // wanting to leave. A CLICK still moves focus, because that is a decision.
    if editing() {
        return;
    }
    if let Some(h) = hit(mx, my) {
        focus(h);
    }
}

/// Pointer click — the same activation map as OK, after the hover has moved focus onto what was
/// clicked (`library::click`'s shape).
///
/// **A click on blank ground is a MISS, never an activation.** Falling through to [`on_ok`] on a
/// miss (which this did) activates whatever happens to hold focus: a stray click on the page
/// background with the cursor resting on Clear wiped the user's recent searches, and with it on a
/// result tile opened that page.
pub(crate) fn click(mx: f32, my: f32) -> Action {
    let Some(h) = hit(mx, my) else { return Action::None };
    focus(h);
    // A click on the field is the one place the pointer and the remote differ: OK on a focused
    // field toggles editing, but a click IS the request to type, so it only ever raises the panel.
    if h == Hit::Field {
        if !editing() {
            start_editing();
        }
        return Action::None;
    }
    on_ok()
}

/// The wheel is a vertical d-pad, `library::wheel`'s rule — one focus step per notch, so the
/// scroll stays a consequence of focus and can never disagree with it.
pub(crate) fn wheel(dy: f32) {
    move_focus(if dy < 0.0 { SDLK_DOWN } else { SDLK_UP });
}

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    //! Zones, the ▼ handoff and the scroll rule. Focus lives in this module's `static mut`s and the
    //! query lives in `crate::search`'s, so every test here holds BOTH the module's own [`ZLOCK`]
    //! and `testlock::serial()` — the query is a crate-wide global that `browse`/`route` tests also
    //! move, which a per-module mutex cannot see (`ui/library.rs`'s test module states the same
    //! rule for the same reason).
    use super::*;
    use std::sync::Mutex;

    static ZLOCK: Mutex<()> = Mutex::new(());

    /// A fresh boot: field focused, keyboard down, no query.
    fn reset() {
        crate::search::reset();
        unsafe {
            *addr_of_mut!(ZONE) = Zone::Field;
            *addr_of_mut!(EDITING) = false;
            *addr_of_mut!(ROW) = 0;
            *addr_of_mut!(COL) = 0;
            *addr_of_mut!(RECENT) = 0;
            *addr_of_mut!(STRIP) = 0;
            *addr_of_mut!(SCROLL) = Spring::at(0.0);
            // Seated on its target, exactly as `enter` seats it — a reset screen is SETTLED by
            // definition, and a focus spring left at 0 under a focused field would report motion
            // for its first frames and read as the idle gate being broken.
            *addr_of_mut!(HOT) = Spring::at(1.0);
            *addr_of_mut!(CARET) = 0;
            *addr_of_mut!(BLINK_T) = 0.0;
            // The content fader is screen state like the rest of it, and a reset screen is SETTLED
            // by definition. Without this the watchdog in `update` sees a generation it has never
            // seen, mounts a fade, and the next ~8 frames legitimately report motion — which reads
            // as the idle gate being broken when it is in fact working.
            *addr_of_mut!(XF) = Xfade::new();
            *addr_of_mut!(EPOCH) = crate::search::query_gen();
        }
    }

    // ---- the ▼ handoff, in all four cases ----------------------------------------------------

    /// The whole rule, on the pure form: ▼ leaves the field only for a region that is DRAWN, and
    /// [`below_of`] is the single expression both the handoff and [`draw`]'s dispatch read.
    #[test]
    fn the_handoff_answers_only_with_regions_that_are_drawn() {
        // no query, no terms → nothing is under the field at all
        assert_eq!(below_of(false, 0, 0), Below::Nothing);
        // no query, terms remembered → the recents list
        assert_eq!(below_of(false, 1, 0), Below::Recents);
        assert_eq!(below_of(false, MAX_RECENTS + 3, 0), Below::Recents, "a longer store still draws a list");
        // a query with shelves → the results
        assert_eq!(below_of(true, 0, 1), Below::Results);
        assert_eq!(below_of(true, 4, 5), Below::Results, "a live query outranks the remembered terms");
        // a query whose answer is empty → the no-results STATEMENT, which holds no focus
        assert_eq!(below_of(true, 0, 0), Below::Nothing);
        assert_eq!(below_of(true, 4, 0), Below::Nothing, "an empty answer does not fall back to the recents list");
    }

    /// …and the live wiring of it, in the two cases a host can actually reach (both stores are
    /// stubs today: `recents::count()` answers 0 and nothing lands shelves). Those two are exactly
    /// the "stays put" half, which is the half worth proving against the real statics.
    #[test]
    fn down_from_the_field_with_nothing_below_it_stays_put() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert_eq!(below(), Below::Nothing, "no query and no terms: the field is the only thing on screen");
        move_focus(SDLK_DOWN);
        assert_eq!(zone(), Zone::Field, "there is nothing drawn below the field, so focus must not leave it");

        crate::search::set_query("wallace");
        assert!(crate::search::shelves().is_empty(), "the store lands nothing yet — that is the second case");
        assert_eq!(below(), Below::Nothing, "a query whose answer is empty draws the statement, not shelves");
        move_focus(SDLK_DOWN);
        assert_eq!(zone(), Zone::Field, "a searched-but-empty screen has nothing focusable under the field either");

        crate::search::set_query("   ");
        assert_eq!(below(), Below::Nothing, "a whitespace-only query is not a query, and there are no terms");
        crate::search::reset();
    }

    /// ▲ from the field reaches the strip and re-seats the cursor on OUR pill, ▼ comes back.
    #[test]
    fn the_strip_is_a_zone_with_its_own_cursor() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        move_focus(SDLK_UP);
        assert_eq!(zone(), Zone::Strip);
        assert_eq!(focused_pill(), crate::ui::widgets::search_pill().min(strip_last()) as c_int, "▲ lands under the pill the screen is drawn as selected on");
        move_focus(SDLK_LEFT);
        assert!(focused_pill() <= crate::ui::widgets::search_pill() as c_int, "◀ walks the pills, never past the first");
        for _ in 0..8 {
            move_focus(SDLK_LEFT);
        }
        assert_eq!(focused_pill(), 0, "◀ clamps at the first pill rather than wrapping or underflowing");
        for _ in 0..32 {
            move_focus(SDLK_RIGHT);
        }
        assert_eq!(focused_pill(), strip_last() as c_int, "▶ clamps at the last pill `widgets::tab_count` reports");
        move_focus(SDLK_DOWN);
        assert_eq!(zone(), Zone::Field, "▼ off the strip is the field, whatever pill the cursor was on");
        assert_eq!(focused_pill(), -1, "focus is not in the strip any more, and the pill read-out must say so");
        crate::search::reset();
    }

    // ---- focus clamping ------------------------------------------------------------------------

    /// A column carried onto a SHORTER shelf, which is the ragged-grid arithmetic that actually
    /// goes wrong. Graded on the pure stepper, because the item vectors are a live server answer
    /// no host can seed.
    #[test]
    fn a_shelf_that_shrinks_under_the_cursor_re_seats_it() {
        // Movies 8, TV 2, Episodes 5 — the shape a real search returns.
        let lens = [8usize, 2, 5];

        // Standing on the 7th movie, ▼ lands inside a two-item shelf rather than off its end.
        assert_eq!(step_results(SDLK_DOWN, 0, 6, &lens), Some((1, 1)));
        // …and ▼ again keeps the column it can now afford, NOT the 6 it started with: the cursor is
        // where the user last actually stood, not where they once were.
        assert_eq!(step_results(SDLK_DOWN, 1, 1, &lens), Some((2, 1)));
        // ▲ back up re-seats the same way.
        assert_eq!(step_results(SDLK_UP, 2, 4, &lens), Some((1, 1)));

        // The ends hold: ◀ at column 0, ▶ at the last item, ▼ on the last shelf.
        assert_eq!(step_results(SDLK_LEFT, 0, 0, &lens), Some((0, 0)));
        assert_eq!(step_results(SDLK_RIGHT, 0, 7, &lens), Some((0, 7)));
        assert_eq!(step_results(SDLK_DOWN, 2, 0, &lens), Some((2, 0)));
        // ▲ off shelf 0 leaves the shelves entirely — the field.
        assert_eq!(step_results(SDLK_UP, 0, 3, &lens), None);
        // So does any step in a set that has emptied under the cursor (every keystroke does this).
        for sym in [SDLK_UP, SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT] {
            assert_eq!(step_results(sym, 3, 9, &[]), None, "no shelves means no zone to be in");
        }
        // A cursor addressing a shelf that no longer exists is SEATED before the step is applied,
        // never trusted: (9,9) seats to the last shelf's last item (2,4), and ◀ then walks from
        // THERE rather than from an index that was never on screen.
        assert_eq!(seat(9, 9, &lens), Some((2, 4)));
        assert_eq!(step_results(SDLK_LEFT, 9, 9, &lens), Some((2, 3)));
        assert_eq!(seat(0, 0, &[0]), Some((0, 0)), "an empty shelf still seats at column 0");
        assert_eq!(seat(0, 0, &[]), None);
    }

    /// A shelf shrinking (or vanishing) under the cursor must never leave focus addressing an item
    /// that is not there — the as-you-type case, where every keystroke wipes the store.
    #[test]
    fn focus_is_clamped_when_the_shelves_shrink_under_it() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // Stand deep in a result set that then disappears (`set_query` clears SHELVES synchronously).
        unsafe {
            *addr_of_mut!(ZONE) = Zone::Results;
            *addr_of_mut!(ROW) = 3;
            *addr_of_mut!(COL) = 11;
        }
        clamp_focus();
        assert_eq!(zone(), Zone::Field, "with no shelves drawn, Results is not a zone focus may sit in");
        // …and the CURSOR survives, the same answer `move_focus`'s own fall-back gives. Zeroing it
        // here threw the column away on every keystroke, since `set_query` wipes the store
        // synchronously — so the "the column is remembered" promise held for a d-pad ▲ and not for
        // the far more common case of typing one more letter.
        assert_eq!((view().row, view().col), (3, 11), "the zone goes, the cursor stays");

        // The same rule one zone over: the recents cursor cannot outlive the list.
        unsafe {
            *addr_of_mut!(ZONE) = Zone::Recents;
            *addr_of_mut!(RECENT) = 3;
        }
        clamp_focus();
        assert_eq!(zone(), Zone::Field, "an empty recents list draws nothing, so it holds no focus either");
        crate::search::reset();
    }

    /// A click on blank ground is a MISS. It used to fall through to `on_ok`, which activates
    /// whatever holds focus — so a stray click on the page background with the cursor resting on
    /// the Clear control wiped the user's recent searches.
    #[test]
    fn a_click_on_blank_ground_activates_nothing() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // Nothing is drawn below the field and no rect has been recorded, so every point off the
        // field is blank ground.
        let blank = (960.0, 900.0);
        assert_eq!(hit(blank.0, blank.1), None);
        unsafe { *addr_of_mut!(ZONE) = Zone::Recents };
        assert!(matches!(click(blank.0, blank.1), Action::None));
        assert_eq!(zone(), Zone::Recents, "a miss must not move focus either");

        // The field is a constant rect, so it hits without any region having drawn.
        assert_eq!(hit(FIELD.cx(), FIELD.cy()), Some(Hit::Field));
        assert!(matches!(click(FIELD.cx(), FIELD.cy()), Action::None));
        assert_eq!(zone(), Zone::Field);
        assert!(editing(), "a click on the field IS the request to type");
        unsafe { *addr_of_mut!(EDITING) = false };
        crate::textinput::stop();
        crate::search::reset();
    }

    /// The one scan's two rules, on the pure form — the live store is filled by a draw no host can
    /// run, which is why the vec is passed in. A zero-size rect must not answer (`Rect::contains`
    /// is inclusive, so it is clickable at exactly its corner — `library::STATUS_BTN`'s lesson),
    /// and a rect belonging to a region that is no longer drawn must not either.
    #[test]
    fn a_zero_size_or_stale_region_rect_is_not_hittable() {
        let tile = Rect::new(400.0, 400.0, 250.0, 375.0);
        let rects = [(Hit::Tile(1, 2), tile), (Hit::Recent(0), Rect::new(90.0, 300.0, 820.0, 84.0))];

        assert_eq!(find_rect(&rects, Below::Results, 500.0, 500.0), Some(Hit::Tile(1, 2)));
        assert_eq!(find_rect(&rects, Below::Recents, 500.0, 500.0), None, "a tile is not a target while the recents list is what is drawn");
        assert_eq!(find_rect(&rects, Below::Recents, 500.0, 340.0), Some(Hit::Recent(0)));
        assert_eq!(find_rect(&rects, Below::Results, 500.0, 340.0), None, "…and the converse");
        assert_eq!(find_rect(&rects, Below::Nothing, 500.0, 500.0), None, "nothing drawn, nothing hittable");

        // A tile culled to nothing still sits in the list; it must not answer at its own corner.
        let culled = [(Hit::Tile(0, 0), Rect::new(400.0, 400.0, 0.0, 0.0))];
        assert_eq!(find_rect(&culled, Below::Results, 400.0, 400.0), None, "a zero-size tile is not a target");

        // First match wins, which is what keeps the focused tile — drawn LAST, so recorded last —
        // from stealing a click that landed on the neighbour underneath it.
        let stacked = [(Hit::Tile(0, 3), tile), (Hit::Tile(0, 4), tile)];
        assert_eq!(find_rect(&stacked, Below::Results, 500.0, 500.0), Some(Hit::Tile(0, 3)));
    }

    /// One typed character is not a search: the store sends nothing below `MIN_QUERY`, so the
    /// recents list must stay up rather than flicker out and back on a backspace.
    #[test]
    fn a_query_below_the_stores_own_threshold_is_not_a_query() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        crate::search::set_query("w");
        assert!(!has_query(), "one character never reaches the server — `search::MIN_QUERY` is 2");
        // …and the store agrees, which is the point: it went Idle rather than Searching, so no
        // request exists for an empty-results statement to be about.
        assert!(matches!(crate::search::state(), crate::search::State::Idle));
        assert_eq!(below_of(has_query(), 3, 0), Below::Recents, "so the remembered terms stay on screen");
        crate::search::set_query("wa");
        assert!(has_query());
        assert_eq!(below_of(has_query(), 3, 0), Below::Nothing, "now it IS a search, and an empty answer says so");
        crate::search::reset();
    }

    /// Moving in a zone whose region is gone must not panic and must not strand focus — the arm
    /// `move_focus` takes when a landing empties the store between two key presses.
    #[test]
    fn moving_inside_an_emptied_result_set_falls_back_to_the_field() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        unsafe { *addr_of_mut!(ZONE) = Zone::Results };
        move_focus(SDLK_RIGHT);
        assert_eq!(zone(), Zone::Field);
        crate::search::reset();
    }

    #[test]
    fn the_recents_cursor_stops_on_the_clear_control() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        // `clear_index` is one PAST the last term shown — never `MAX_RECENTS` on a short list, or
        // ▼ would walk onto rows that were never drawn.
        assert_eq!(clear_index(), recents::count().min(MAX_RECENTS));
        unsafe { *addr_of_mut!(ZONE) = Zone::Recents };
        for _ in 0..10 {
            move_focus(SDLK_DOWN);
        }
        assert!(view().recent <= clear_index(), "▼ caps at the Clear control, never past it");
        crate::search::reset();
    }

    // ---- the scroll freeze -----------------------------------------------------------------------

    /// The two freezes, which is all this module still says about the flow — the SHAPE (a minimal
    /// `card_row::reveal`, not the mock's pin) moved to `results.rs` and is graded there, in
    /// `a_shelf_scrolls_only_as_far_as_its_own_block_needs`.
    #[test]
    fn the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down() {
        assert!(!scroll_frozen(Zone::Results, false), "the one case that scrolls at all");
        // The keyboard is up: the result set stays still under a user who is still typing.
        assert!(scroll_frozen(Zone::Results, true));
        // …and whenever the shelves do not hold focus, so ▼ off the field lands on shelf 0 without
        // the page jumping under it.
        for z in [Zone::Field, Zone::Strip, Zone::Recents] {
            assert!(scroll_frozen(z, false), "{z:?} is not the shelves");
            assert!(scroll_frozen(z, true));
        }
    }

    /// The scroll spring owes `ui::idle` **both halves** — `ui/CLAUDE.md`: "it reports while running
    /// AND goes quiet at rest", because the two failures are opposite and each is invisible to the
    /// other's gate. An over-reporting animator costs the whole idle saving while every fps floor
    /// still passes.
    ///
    /// **It restores `ui::idle`'s clock before it returns**, and that is not politeness. The gate's
    /// `LAST_PRESENT` is a process-global that `testlock::serial()` orders but does not clean, and
    /// `pms.rs`'s repaint test asserts `should_present(0)` at an ABSOLUTE now of 0 — so a timeline
    /// left parked in the ten-thousands makes `0.wrapping_sub(LAST_PRESENT)` enormous, trips the 2 s
    /// keepalive, and fails an assertion in a module this one never touches. The first version of
    /// this test did exactly that, and the failure surfaced two tests further on as a corrupted pms
    /// catalog (the panic ran while the lock was held, so the seeded statics never got their
    /// `reset()`), which is about as far from its cause as a test failure can land.
    #[test]
    fn the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest() {
        use crate::ui::idle;
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        idle::set_enabled(true);
        const DT: f32 = 1.0 / 60.0;
        // One frame whose verdict is DISCARDED, because `should_present` is what clears the
        // discrete flag and `reset` above raised one. Without it this grades the mount, not the
        // spring.
        let settle = |now: u32| {
            idle::frame_begin(DT);
            update(DT);
            let asked = idle::should_present(now);
            idle::note_present(now);
            asked
        };
        settle(10_000);

        // A settled screen: the spring is on target, so a frame that only steps it must not ask.
        assert!(!settle(10_016), "a Search screen with nothing moving must stop repainting");
        assert!(!settle(10_032), "…and stays quiet frame after frame");

        // …and one in flight: shove the spring off its target and it must ask for as long as it is
        // visibly travelling.
        unsafe { *addr_of_mut!(SCROLL) = Spring { pos: 400.0, vel: 0.0 } };
        assert!(settle(10_048), "a scrolling shelf must keep the panel awake");

        // …and settles on its own rather than ringing forever.
        let mut t = 10_064;
        let mut frames = 0;
        while settle(t) && frames < 600 {
            t += 16;
            frames += 1;
        }
        assert!(frames < 600, "the scroll must arrive, not ring forever");
        assert!(view().shift.abs() < 0.25, "with focus off the shelves the flow rests at zero");

        // Put the gate's clock back where a fresh process has it — see the doc above.
        idle::note_present(0);
        idle::should_present(0);
        idle::take_presents();
        crate::search::reset();
    }

    // (The block ladder — a poster shelf stacking at `consts::ROW_PITCH`, the keyboard clearance's
    // real 683-against-700, and `LABEL_BLOCK` holding a one-line `TileLabel` — moved to
    // `results.rs`'s tests with the geometry itself. Restating any of it here is what made the two
    // copies survive each other for as long as they did.)

    // ---- typing ---------------------------------------------------------------------------------

    /// Put a term in the field the way the device does — through the panel — so the caret lands
    /// where typing leaves it rather than wherever the last test parked it.
    fn typed(s: &str) {
        if !editing() {
            on_ok();
        }
        crate::search::set_query("");
        set_caret(0);
        insert_text(s);
    }

    #[test]
    fn backspace_only_bites_while_the_panel_is_up_and_takes_a_whole_character() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        crate::search::set_query("wallacé");
        assert!(!key(SDLK_BACKSPACE), "with the panel down the screen has no claim on the key");
        assert_eq!(crate::search::query(), "wallacé", "…and must not have edited the query anyway");

        typed("wallacé");
        assert!(key(SDLK_BACKSPACE), "editing, the screen consumes it");
        assert_eq!(crate::search::query(), "wallac", "a whole codepoint, not one byte of a two-byte char");
        assert!(!key(SDLK_UP), "everything else falls through to app.rs");

        crate::search::set_query("");
        set_caret(0);
        assert!(key(SDLK_BACKSPACE), "an empty query still consumes the key — it is the field's");
        assert_eq!(crate::search::query(), "");
        leave();
        crate::search::reset();
    }

    /// **The television's own keyboard delegates every edit to us**, so the four keys it sends are
    /// one protocol and are graded as one. `◀`/`▶` (its cursor buttons), `Clear all` and backspace
    /// all arrived at this screen and three of them did nothing, which is how the panel shipped
    /// with visibly dead buttons — the device report that produced this test.
    #[test]
    fn the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        typed("суббота"); // Cyrillic: two bytes a letter, so a byte step would split a codepoint
        assert_eq!(caret(), "суббота".len(), "typing leaves the caret after what was typed");

        // ◀ walks back one CHARACTER at a time, and stops at the front rather than wrapping.
        for _ in 0..3 {
            assert!(key(SDLK_LEFT));
        }
        assert_eq!(caret(), "суббота".len() - "ота".len(), "three letters back, not three bytes");
        for _ in 0..40 {
            key(SDLK_LEFT);
        }
        assert_eq!(caret(), 0, "◀ clamps at the front");
        assert!(key(SDLK_RIGHT));
        assert_eq!(caret(), "с".len(), "▶ steps one whole character forward");

        // …and a commit lands AT the caret, not at the end of the string.
        insert_text("X");
        assert_eq!(crate::search::query(), "сXуббота");
        assert_eq!(caret(), "сX".len(), "…leaving the caret after the inserted text");
        // backspace takes the character BEFORE the caret, which is the one just typed
        assert!(key(SDLK_BACKSPACE));
        assert_eq!(crate::search::query(), "суббота");

        // Clear all is the whole field, wherever the caret was standing.
        key(SDLK_RIGHT);
        assert!(key(SDLK_CLEAR), "the panel's Clear all is the screen's key");
        assert_eq!(crate::search::query(), "");
        assert_eq!(caret(), 0);

        // A caret held past a query the STORE replaced under us is clamped on read, never sliced
        // on — `set_query` is called from four places this screen does not drive.
        crate::search::set_query("ab");
        unsafe { *addr_of_mut!(CARET) = 99 };
        assert_eq!(caret(), 2, "a stale offset clamps to the end of the live query");
        crate::search::set_query("é"); // 2 bytes: a clamp to len-1 would land mid-codepoint
        unsafe { *addr_of_mut!(CARET) = 1 };
        assert_eq!(caret(), 0, "…and rounds DOWN to a char boundary rather than panicking");
        leave();
        crate::search::reset();
    }

    /// The field's two faces CROSS-FADE, and the fade is a spring — so it owes the frame gate the
    /// same two halves everything else that moves does. This is the "reports while running" half;
    /// the quiet-at-rest half is `the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest`,
    /// which passes only because `enter`/`reset` SEAT this spring on its target (a focused field
    /// mounting from 0 would report motion for its first frames).
    #[test]
    fn the_fields_focus_fade_runs_when_focus_leaves_it_and_settles() {
        use crate::ui::idle;
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        idle::set_enabled(true);
        const DT: f32 = 1.0 / 60.0;
        let frame = |now: u32| {
            idle::frame_begin(DT);
            update(DT);
            let asked = idle::should_present(now);
            idle::note_present(now);
            asked
        };
        frame(20_000); // discard the mount's own discrete invalidate

        assert_eq!(view().hot, 1.0, "the screen mounts with the field focused and the fill seated");
        // ▲ to the strip: the field is no longer hot, so the fill has somewhere to travel.
        move_focus(SDLK_UP);
        assert_eq!(zone(), Zone::Strip);
        frame(20_016); // spends `move_focus`'s discrete invalidate
        let mut ran = 0;
        let mut t = 20_032;
        for _ in 0..200 {
            if !frame(t) {
                break;
            }
            ran += 1;
            t += 16;
        }
        assert!(ran > 4, "the fade must keep the panel awake while it travels (ran {ran} frames)");
        assert!(ran < 120, "…and settle rather than ring forever");
        assert!(view().hot < 0.02, "…arriving on the idle face (got {})", view().hot);

        // …and back: ▼ returns to the field and the fill travels the other way.
        move_focus(SDLK_DOWN);
        frame(t);
        assert!(frame(t + 16), "focus returning to the field must animate too");
        for _ in 0..200 {
            update(DT);
        }
        assert!(view().hot > 0.98, "…and arrive on the focused face (got {})", view().hot);
        crate::search::reset();
    }

    /// **Tapping a word prediction is a REPLACE and the panel never says so.** Typing `sum` and
    /// tapping the offered `summer` gave `sumsummer` — device-reported, and the event log shows why
    /// there is no exact fix available: three single-character commits for the keys, then one
    /// commit of `"summer "`, and no `SDL_TEXTEDITING`, no backspace keys, nothing else at all.
    ///
    /// So this is a rule, and a rule's tests are the cases it must NOT fire on.
    #[test]
    fn a_multi_character_commit_at_the_end_replaces_the_word_being_typed() {
        // the reported case, exactly as the log has it — trailing space included
        assert_eq!(replaces_word_at("sum", 3, "summer "), Some(0));
        // …and mid-sentence, where only the LAST word goes
        assert_eq!(replaces_word_at("the sum", 7, "summer "), Some(4));

        // ---- the cases it must not fire on ----
        // A single character is a KEY PRESS, always. Without this guard, typing a letter twice
        // replaces it with itself and `aa` is impossible to type.
        assert_eq!(replaces_word_at("a", 1, "a"), None);
        assert_eq!(replaces_word_at("sum", 3, "m"), None);
        assert_eq!(replaces_word_at("sum", 3, " "), None, "the space bar is a space");
        // A caret the user MOVED is not a word being predicted on: a multi-character insertion
        // there must land where they put it (this is the `txt:` dev token's path, and the ◀/▶ one).
        assert_eq!(replaces_word_at("summer", 4, "XY"), None);
        // Nothing to replace: the query ends in a space, so the prediction is for the NEXT word.
        assert_eq!(replaces_word_at("the ", 4, "office "), None);
        assert_eq!(replaces_word_at("", 0, "summer "), None);

        // Multi-byte: the word boundary is found by BYTE index and must land on a char boundary,
        // or the `replace_range` that follows panics inside the SDL event loop.
        assert_eq!(replaces_word_at("суб", "суб".len(), "суббота "), Some(0));
        let two = "я суб";
        assert_eq!(replaces_word_at(two, two.len(), "суббота "), Some("я ".len()));
    }

    /// …and the same rule through the live store, since `commit_text` is what actually edits.
    #[test]
    fn the_prediction_replaces_rather_than_doubling_the_partial_word() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        typed("sum");
        commit_text("summer ");
        assert_eq!(crate::search::query(), "summer ", "the partial word is replaced, not doubled");
        assert_eq!(caret(), "summer ".len());
        // a second prediction on the next word appends, because there is no partial word to eat
        commit_text("house ");
        assert_eq!(crate::search::query(), "summer house ");
        // …and ordinary typing after one is still ordinary typing
        commit_text("a");
        commit_text("a");
        assert_eq!(crate::search::query(), "summer house aa");
        leave();
        crate::search::reset();
    }

    /// The blink is a CLOCK, so it owes `ui::idle` the two halves `ui/CLAUDE.md` demands: it must
    /// ask for a frame when the bar changes state, and must ask for nothing in between — a blink
    /// that reported every frame would cost the whole idle saving to animate one 3px bar.
    #[test]
    fn the_caret_blinks_and_reports_only_on_the_flip() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let dt = 1.0 / 60.0;
        let asked = |editing: bool| {
            crate::ui::idle::frame_begin(dt);
            crate::ui::idle::note_present(0);
            let on = step_blink(dt, editing);
            (on, crate::ui::idle::should_present(0))
        };
        asked(true); // drain whatever the previous test left on the shared flag

        // A fresh field is ON, and stays on for a whole phase without asking for anything.
        let mut flips = 0;
        let mut was = true;
        for f in 0..((BLINK_MS * 2.0 / 1000.0 / dt) as i32 + 4) {
            let (on, present) = asked(true);
            if on != was {
                flips += 1;
                assert!(present, "frame {f}: the bar changed state and did not ask to be drawn");
                was = on;
            } else {
                assert!(!present, "frame {f}: a caret mid-phase asked for a repaint");
            }
        }
        assert_eq!(flips, 2, "one full cycle is exactly two flips");

        // An edit restarts the phase at full ON — you must see where the next glyph lands.
        typed("s");
        assert!(blink_on(unsafe { addr_of!(BLINK_T).read() }), "a keystroke leaves the bar solid");
        // …and with the panel down nothing animates at all: this must not hold a settled screen
        // awake, which is the entire reason the field drew a solid bar before.
        leave();
        asked(false); // the edit above raised a DISCRETE invalidate; spend it before grading quiet
        for f in 0..200 {
            let (on, present) = asked(false);
            assert!(on, "the phase parks ON so the next open does not start invisible");
            assert!(!present, "frame {f}: the caret asked for a repaint with no keyboard up");
        }
        crate::search::reset();
    }

    /// OK toggles the panel, and leaving the field for a zone below drops it — the two edges that
    /// decide whether the TV's keyboard is on screen.
    #[test]
    fn ok_raises_the_panel_and_leaving_the_field_drops_it() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(matches!(on_ok(), Action::None));
        assert!(editing(), "OK on the field starts editing");
        assert!(matches!(on_ok(), Action::None));
        assert!(!editing(), "OK again commits and drops the panel");

        // ▲ to the strip drops it too; ▼ into nothing must NOT, because focus never left.
        on_ok();
        assert!(editing());
        move_focus(SDLK_DOWN);
        assert!(editing(), "a ▼ that moved nothing must not silently dismiss the keyboard");
        assert_eq!(zone(), Zone::Field);
        move_focus(SDLK_UP);
        assert!(!editing(), "▲ leaves the field, so the panel comes down with it");
        assert_eq!(zone(), Zone::Strip);
        crate::search::reset();
    }

    /// BACK's contract, which `app.rs` treats every screen through: true while there was something
    /// to close, false to leave.
    #[test]
    fn back_dismisses_the_panel_once_and_then_leaves() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        start_editing();
        assert!(back(), "the first BACK takes the keyboard down");
        assert!(!editing());
        assert!(!back(), "the second leaves the screen — app.rs routes to Home");
        crate::search::reset();
    }

    /// **The Search pill is a RETURN, not a reset**, and this is the difference between the two
    /// mounts. Every interactive entry ran `enter("")`, which wipes the query, the shelves under
    /// it and both cursors — on the one screen whose BACK-trail re-entry deliberately preserves
    /// all three, and whose own `trail.rs` promises the state is kept "for as long as the profile
    /// does". Walk to a result, press the Search pill on the way past, and the search you were
    /// reading was gone.
    #[test]
    fn the_pill_returns_to_the_search_that_was_there_and_a_seed_replaces_it() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();

        // A search in progress, with the cursors standing in its results and the strip cursor
        // parked on somebody else's pill by the very press that took the user away.
        crate::search::set_query("wallace");
        let gen = crate::search::query_gen();
        unsafe {
            *addr_of_mut!(ZONE) = Zone::Strip;
            *addr_of_mut!(STRIP) = 0;
            *addr_of_mut!(ROW) = 2;
            *addr_of_mut!(COL) = 5;
            *addr_of_mut!(RECENT) = 3;
            *addr_of_mut!(SCROLL) = Spring::at(400.0);
            *addr_of_mut!(HOT) = Spring::at(0.0);
        }

        resume();
        assert_eq!(crate::search::query(), "wallace", "the pill must not wipe the term still on screen");
        // The SHELVES cannot be seeded from a host — they are a live server answer — so the proof
        // is the store's own rule: `set_query` is the only thing that clears them, and it
        // SUPERSEDES when it does. An unmoved generation is an unmoved result set.
        assert_eq!(crate::search::query_gen(), gen, "the store was superseded, so its shelves went with it");
        assert!(matches!(crate::search::state(), crate::search::State::Searching), "…and the request is still the one in flight");
        let v = view();
        assert_eq!((v.row, v.col, v.recent), (2, 5, 3), "both cursors survive the return");

        // …and the ENTRY POSTURE is seated, which is not state the user misses: the flow is frozen
        // at 0 whenever the shelves do not hold focus anyway, and the strip cursor comes back
        // under OUR pill rather than the Home one it was left on.
        assert_eq!(zone(), Zone::Field);
        assert!(!editing(), "a return does not raise the television's keyboard");
        assert_eq!(v.shift, 0.0);
        assert_eq!(v.hot, 1.0, "the field mounts focused and SEATED, or it reports motion on arrival");
        assert_eq!(unsafe { addr_of!(STRIP).read() }, crate::ui::widgets::search_pill());

        // The seeded mount is the contrast, and it is what every pill press used to run: the query
        // is replaced outright and every cursor re-parked. Right for the boot trigger, which is
        // its one caller, and wrong for a navigation.
        enter("");
        assert_eq!(crate::search::query(), "");
        assert_ne!(crate::search::query_gen(), gen, "a seed supersedes: the shelves go with the term");
        let v = view();
        assert_eq!((v.row, v.col, v.recent), (0, 0, 0));
        crate::search::reset();
    }

    /// A FIRST entry needs no "have I been here" flag, which is why there is none: the store is
    /// empty until something is typed, so a resume onto a profile that has never searched seats
    /// exactly what a mount would have.
    #[test]
    fn resuming_a_profile_that_has_never_searched_is_a_clean_mount() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        unsafe { *addr_of_mut!(ZONE) = Zone::Results };
        resume();
        assert_eq!(crate::search::query(), "", "nothing was ever searched, so the field opens empty");
        assert!(matches!(crate::search::state(), crate::search::State::Idle), "…and nothing was ever asked");
        let v = view();
        assert_eq!((zone(), v.row, v.col, v.recent, v.shift), (Zone::Field, 0, 0, 0, 0.0));
        assert!(!editing());
        crate::search::reset();
    }

    /// **An OS app switch takes the television's keyboard with it and tells the app nothing.**
    /// `app.rs`'s `0x103`/`0x104` arm runs this dismissal for that: without it the screen comes
    /// back to the foreground drawing an editing layout and a blinking caret over a panel that is
    /// gone, and typing is dead in a way no press recovers — `textinput::start` early-returns while
    /// its own `STARTED` is set, so OK on the field toggles our flag and raises nothing.
    ///
    /// The arm itself lives inside the SDL event loop, where no host test can reach it (the
    /// `trail.rs` case, for the same reason); what is gradeable is the path it takes, and the half
    /// that is easy to get wrong is WHICH path. `leave` is the withdrawal, so nothing is filed —
    /// an app switch is not the user saying "that is the search I meant", and a list of
    /// half-typed terms is a list of interruptions.
    #[test]
    fn dismissing_the_panel_from_outside_files_nothing_and_can_be_re_opened() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        let before = recents::count();
        typed("wallace"); // the panel is up and the term is well past `MIN_QUERY`
        assert!(editing());

        leave();
        assert!(!editing(), "the panel is gone, so the screen must stop drawing a field being typed into");
        assert_eq!(recents::count(), before, "a withdrawal records nothing — BACK's rule, not `leave_field`'s");

        // …and the keyboard comes back, which is the half `textinput`'s own `STARTED` decides:
        // `leave` goes through `stop()`, so the next `start()` is not a silent no-op.
        on_ok();
        assert!(editing(), "a field that cannot re-open its own keyboard is a dead screen");
        leave();
        crate::search::reset();
    }

    /// A seed arrives from a FILE, and a control byte in one is not the user's text. **NUL is the
    /// one that bites**: it survives every `String` operation on the way in, and `CString::new`
    /// refuses it at the far end — so `empty::draw` returns without drawing its statement and the
    /// field's capsule blanks, leaving a Search screen showing nothing at all for a query the app
    /// still believes it is holding.
    ///
    /// Filtered at the mount because that is where untrusted text enters the query. The panel's own
    /// commits cannot carry one (`textinput::decode_text_at` ends the string AT the NUL) and a
    /// recents pick is already refused by `recents::usable`, so this is the last way in.
    #[test]
    fn a_control_byte_in_the_seed_never_reaches_the_query() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        enter("wal\0lace");
        assert_eq!(crate::search::query(), "wallace");
        assert!(std::ffi::CString::new(crate::search::query()).is_ok(), "…so every run on this screen can be drawn");
        assert_eq!(caret(), "wallace".len(), "and the caret is the LIVE string's end, not the seed's");
        // a hand-edited file's tab or newline is not a query either
        enter("a\tb\nc");
        assert_eq!(crate::search::query(), "abc");
        // …and an ordinary seed is untouched, trailing space and all — the field draws that
        enter("wallace ");
        assert_eq!(crate::search::query(), "wallace ");
        crate::search::reset();
    }

    /// `enter` is the only mount, and a headless boot trigger reaches the screen through it: the
    /// seed must be in the field and every cursor at rest.
    #[test]
    fn entering_seeds_the_field_and_parks_every_cursor() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        unsafe {
            *addr_of_mut!(ZONE) = Zone::Results;
            *addr_of_mut!(ROW) = 2;
            *addr_of_mut!(SCROLL) = Spring::at(400.0);
        }
        enter("wallace");
        assert_eq!(crate::search::query(), "wallace");
        assert_eq!(zone(), Zone::Field, "a mount opens on the field, whatever the last visit left");
        assert!(!editing(), "…and does NOT raise the keyboard: a seeded boot has nothing to type");
        let v = view();
        assert_eq!((v.row, v.col, v.recent, v.shift), (0, 0, 0, 0.0));
        crate::search::reset();
    }

    /// A screen with no items reports no card, so `app.rs` never arms a tvOS press that would
    /// commit nothing (`library::focus_is_card`'s rule).
    #[test]
    fn an_empty_result_set_is_not_a_card() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        unsafe { *addr_of_mut!(ZONE) = Zone::Results };
        assert!(!focus_is_card());
        assert!(matches!(on_ok(), Action::None), "…and OK on it opens nothing");
        unsafe { *addr_of_mut!(ZONE) = Zone::Field };
        assert!(!focus_is_card(), "the field is never a card");
        crate::search::reset();
    }
}
