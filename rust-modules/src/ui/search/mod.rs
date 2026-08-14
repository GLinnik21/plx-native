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
//! the bottom [`KEYBOARD_H`], so its top edge is at 700, and the field, the first shelf's heading
//! and that shelf's full row of posters all finish above it: [`CONTENT_TOP`] 248 + [`HEAD_TO_ROW`]
//! 60 + a 375-tall poster = **683**, seventeen clear. (This paragraph said 699 — "exactly on its
//! top edge" — which is the number `CONTENT_TOP` = 264 would give, not the 248 the constant is and
//! not what any of them draws. A stated equality nobody can reproduce is worse than the slack,
//! because the next person tuning either constant balances against a figure that was never true;
//! `a_poster_shelf_stacks_at_the_apps_own_row_pitch` now asserts the real one.) That clearance is
//! also why nothing scrolls while the panel is up: the result set has to be stable under the
//! user's eyes while they are still typing.
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
//! rows are chrome at fixed y. The focused shelf's block top comes to [`CONTENT_TOP`], clamped so
//! the flow cannot be scrolled past its own end — and it is **zero whenever the shelves do not hold
//! focus**, which is forced rather than chosen: nothing clips the flow, so a non-zero shift with
//! focus on the field would draw shelf 0 up through the field and the tab strip. That is also why
//! ▼ off the field lands on shelf **0** and not on the shelf last left — the shift is already back
//! at 0 by then, so restoring a deep row would jump the page under the user's eyes at the exact
//! moment they asked for a small move. The COLUMN is remembered (and clamped), which is the half of
//! "lands where you were" that costs nothing.
#![allow(dead_code)]

use crate::ui::consts::{K_SCROLL, SCR_H, SDLK_DOWN, SDLK_LEFT, SDLK_RIGHT, SDLK_UP};
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
/// reflows as focus travels along a row.
pub(crate) const LABEL_BLOCK: f32 = 114.0;
/// How much of the panel the TV's keyboard covers. Not ours to draw or to style — this is the
/// clearance the layout above keeps.
pub(crate) const KEYBOARD_H: f32 = 380.0;
/// The field: 820 wide at the app's own side margin, on the one control height.
pub(crate) const FIELD: Rect = Rect { x: 90.0, y: 148.0, w: 820.0, h: 60.0 };
/// Terms kept. **Four, not five**: with the keyboard raised the header, the rows and the Clear
/// control all have to finish above its top edge. The fifth is DROPPED, not scrolled — a list you
/// cannot see the end of asks to be paged, and there is no paging in this product.
pub(crate) const MAX_RECENTS: usize = 4;

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

/// SDL's backspace, which `ui/consts.rs` does not carry because nothing in this app took typed text
/// before. It stays local until a second screen needs it — a keycode belongs beside the other
/// `SDLK_*` the moment it has two readers, and not before.
const SDLK_BACKSPACE: c_uint = 8;

/// Parked far off the panel rather than at the origin: [`Rect::contains`] is inclusive, so a zero
/// rect at (0,0) would "contain" a click at exactly (0,0) — `library::STATUS_BTN`'s lesson.
const PARKED: Rect = Rect::new(-1.0, -1.0, 0.0, 0.0);

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

/// Recent-row hit rects — index `i` is term `i`, and [`MAX_RECENTS`] is the Clear control.
/// Recorded AT DRAW by [`recents`] through [`note_recent_rect`], the `library::TOOL_RECTS` idiom:
/// this module owns the flow but not the rows, so the only rect it can honestly hit-test is the one
/// that was actually painted.
static mut RECENT_R: [Rect; MAX_RECENTS + 1] = [PARKED; MAX_RECENTS + 1];
/// Result-tile hit rects as `(row, col, rect)`, recorded AT DRAW by [`results`] through
/// [`note_tile_rect`]. A list rather than a fixed array because a shelf's tile count is the
/// server's answer, and it is CLEARED (never freed) each frame, so the allocation is paid once —
/// `library::SRC_ACTIONS`' pattern.
///
/// These cannot be derived here even in principle: a shelf's horizontal offset is the scroll spring
/// inside its own `card_row::CardRow`, which lives in [`results`] and is invisible from this side.
static mut TILE_R: Vec<(usize, usize, Rect)> = Vec::new();

// ---- the projection the regions draw from ----------------------------------------------------

fn zone() -> Zone {
    unsafe { addr_of!(ZONE).read() }
}
fn editing() -> bool {
    unsafe { addr_of!(EDITING).read() }
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

/// A shelf's whole block: its heading band, its row of tiles, and the caption pair reserved under
/// them. There is no gap term because the reserved band IS the air — a poster shelf comes out at
/// `HEAD_TO_ROW + 375 + LABEL_BLOCK` = 549, which is `consts::ROW_PITCH` exactly, so a search shelf
/// and a home shelf stack at the same rhythm.
///
/// **[`LABEL_BLOCK`] must stay at least `card_row::TileLabel::height` for whatever `RowStyle`
/// [`results`] draws with** — that function is `ui/CLAUDE.md`'s named single authority on the band,
/// and the flow here is what has to reserve it. At `title_lines: 1` it is 92, so the 114 declared
/// here holds it with 22 to spare (the same 22 home's `ROW_PITCH` carries). At `title_lines: 2` it
/// is **122 and this constant is 8 short** — and two lines is what `ui/CLAUDE.md` says a shelf of
/// real Plex titles needs. The one-line case is asserted below; the two-line case is a cross-file
/// decision (`LABEL_BLOCK` is shared with [`results`]) and belongs to whoever raises `title_lines`,
/// who must raise this with it rather than discovering the overlap on a panel.
fn block_h(kind: crate::search::Kind) -> f32 {
    HEAD_TO_ROW + results::tile_size(kind).1 + LABEL_BLOCK
}

/// Top of shelf `i` in CONTENT space, before [`View::shift`]. `i == len` is the flow's end, which
/// is how [`content_h`] asks for it.
pub(crate) fn shelf_top(i: usize) -> f32 {
    CONTENT_TOP + crate::search::shelves().iter().take(i).map(|s| block_h(s.kind)).sum::<f32>()
}

fn content_h() -> f32 {
    shelf_top(crate::search::shelves().len())
}

/// Where the scroll spring is heading: the focused shelf's block top pinned to [`CONTENT_TOP`],
/// clamped to the content's own end. Pure, so the host suite can grade the rule that the device
/// only ever shows as motion. `top` is `None` when the focused row is not a shelf that exists —
/// the one frame between a landing shrinking the set and [`clamp_focus`] re-seating the cursor.
fn scroll_target_for(zone: Zone, editing: bool, top: Option<f32>, end: f32) -> f32 {
    // Frozen while the keyboard is up (the module doc's layout rule) and whenever the shelves do
    // not hold focus (nothing clips the flow — see the Scroll note).
    if editing || zone != Zone::Results {
        return 0.0;
    }
    let Some(top) = top else { return 0.0 };
    (top - CONTENT_TOP).clamp(0.0, (end - SCR_H).max(0.0))
}

fn scroll_target() -> f32 {
    let row = unsafe { addr_of!(ROW).read() };
    let top = (row < crate::search::shelves().len()).then(|| shelf_top(row));
    scroll_target_for(zone(), editing(), top, content_h())
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
fn view() -> View {
    unsafe {
        View {
            zone: addr_of!(ZONE).read(),
            editing: addr_of!(EDITING).read(),
            row: addr_of!(ROW).read(),
            col: addr_of!(COL).read(),
            recent: addr_of!(RECENT).read(),
            shift: addr_of!(SCROLL).read().pos,
        }
    }
}

// ---- entry / exit ----------------------------------------------------------------------------

/// Mount the screen. `q` pre-seeds the field — the boot trigger's whole job, since a headless
/// screenshot cannot type.
pub(crate) fn enter(q: &str) {
    // Through `stop()`, not by clearing the flag: `textinput` keeps its OWN `STARTED`, and
    // `start()` is a no-op while that is set. Clearing `EDITING` alone would leave the two
    // disagreeing, after which no later press could ever raise the panel again — the same symptom
    // as the driver's reopen wedge, with a cause entirely our own. `stop()` is guarded, so this
    // costs nothing on the normal arrival where the panel was never up.
    leave();
    unsafe {
        *addr_of_mut!(ZONE) = Zone::Field;
        *addr_of_mut!(ROW) = 0;
        *addr_of_mut!(COL) = 0;
        *addr_of_mut!(RECENT) = 0;
        // The strip opens under our own pill, the one the screen is drawn as SELECTED on, so ▲
        // lands where the eye already is.
        *addr_of_mut!(STRIP) = crate::ui::widgets::search_pill();
        (*addr_of_mut!(SCROLL)) = Spring::at(0.0);
    }
    crate::search::set_query(q);
    // Arriving is a content change like any other. `mount` (not `reload`) because there is nothing
    // on screen to fade OUT — the page transition has already covered that half.
    xf().mount();
    unsafe { EPOCH = crate::search::query_gen() };
    crate::ui::idle::invalidate();
}

/// Leave it. Dismissing the keyboard here rather than at the press is deliberate: the panel must
/// come down at the fade floor, with the page, not a frame before it while the old screen is still
/// on screen behind it.
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
    clamp_focus();
    unsafe { (*addr_of_mut!(SCROLL)).step(scroll_target(), K_SCROLL, dt) };

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

/// Drain the keyboard and append what it committed. Text arriving while the panel is DOWN is
/// dropped rather than typed: a commit that outlives its own field is the IME's parting shot, and
/// silently extending a query the user has stopped editing is worse than losing a keystroke.
fn pump_text() {
    let commits = crate::textinput::drain();
    if commits.is_empty() || !editing() {
        return;
    }
    let mut q = crate::search::query().to_string();
    for s in &commits {
        q.push_str(s);
    }
    crate::search::set_query(&q); // invalidates
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
    // Park last frame's hit rects BEFORE the regions record this frame's, so a row or a tile that
    // has stopped being drawn cannot still be clicked.
    unsafe {
        *addr_of_mut!(RECENT_R) = [PARKED; MAX_RECENTS + 1];
        (*addr_of_mut!(TILE_R)).clear();
    }

    field::draw(p, &v);
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

    // `CHIP_D`, not a literal 54: Home draws this same chip at 60, and Home<->Search is a
    // chrome-CONTINUOUS cross-fade, so a six-pixel disagreement was visible as the avatar shrinking
    // mid-transition — on this screen alone.
    let d = crate::ui::widgets::CHIP_D;
    crate::ui::widgets::profile_chip(pk, Rect::new(crate::ui::consts::MARGIN_X, crate::ui::widgets::TOP_BAR_Y, d, d), 0.0);
    crate::ui::widgets::draw_tab_row(pk);
}

/// Record the rect a recent-term row (or, at [`MAX_RECENTS`], the Clear control) was DRAWN at.
/// Called by [`recents`] from its draw; see [`RECENT_R`].
///
/// An index past the Clear control is DROPPED, never clamped onto it: the store is documented to
/// hold more terms than the screen has room for, so a drawer that looped the whole list would
/// otherwise stamp a term's rect over Clear's slot — and hovering that term would then park the
/// cursor on the control that wipes the list.
pub(crate) fn note_recent_rect(i: usize, r: Rect) {
    if let Some(slot) = unsafe { (*addr_of_mut!(RECENT_R)).get_mut(i) } {
        *slot = r;
    }
}

/// Record the rect a result tile was DRAWN at, scroll and pop included. Called by [`results`] from
/// its draw; see [`TILE_R`].
pub(crate) fn note_tile_rect(row: usize, col: usize, r: Rect) {
    unsafe { (*addr_of_mut!(TILE_R)).push((row, col, r)) };
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
/// Only backspace today, and only while the panel is up — the TV's keyboard delivers its printable
/// characters as `SDL_TEXTINPUT` ([`crate::textinput::drain`]) but its delete as an ordinary key,
/// so this is the other half of typing rather than an extra feature.
pub(crate) fn key(sym: c_uint) -> bool {
    if sym == SDLK_BACKSPACE && editing() {
        backspace();
        return true;
    }
    false
}

/// Delete the last CHARACTER of the query — `String::pop`, not a byte, so a multi-byte term does
/// not lose a codepoint's tail and become invalid UTF-8.
fn backspace() {
    let mut q = crate::search::query().to_string();
    if q.pop().is_none() {
        return;
    }
    crate::search::set_query(&q); // invalidates
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

/// The one hit test, shared by hover and click. Every rect here was RECORDED at draw (or is a
/// constant, for the field), `library.rs`'s discipline: what the pointer addresses and what the eye
/// sees are the same rect rather than two derivations that agree by luck.
///
/// Every scan tests `w > 0.5` first. A parked rect is off-panel, but a recorded one can legitimately
/// be zero-SIZE (a culled or fully clipped tile), and `Rect::contains` is inclusive — so a zero rect
/// is clickable at exactly its corner. `library::STATUS_BTN`'s lesson, applied to both lists rather
/// than only to the one it was first noticed on.
fn hit(mx: f32, my: f32) -> Option<Hit> {
    if let Some(i) = crate::ui::widgets::tab_pill_at(mx, my) {
        return Some(Hit::Pill(i.min(strip_last())));
    }
    if FIELD.contains(mx, my) {
        return Some(Hit::Field);
    }
    match below() {
        Below::Recents => {
            let rects = unsafe { addr_of!(RECENT_R).read() };
            rects.iter().position(|r| r.w > 0.5 && r.contains(mx, my)).map(Hit::Recent)
        }
        Below::Results => unsafe { addr_of!(TILE_R).as_ref() }
            .and_then(|v| v.iter().find(|(_, _, q)| q.w > 0.5 && q.contains(mx, my)))
            .map(|&(r, c, _)| Hit::Tile(r, c)),
        Below::Nothing => None,
    }
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

    /// A parked or zero-size recorded rect must not be hittable. `Rect::contains` is inclusive, so
    /// a zero rect is clickable at exactly its corner — `library::STATUS_BTN`'s lesson.
    #[test]
    fn a_parked_or_zero_size_rect_is_not_hittable() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        assert!(!PARKED.contains(0.0, 0.0), "the park is off-panel, not a zero rect at the origin");
        // A recorded tile that got culled to nothing still sits in the list; it must not answer.
        unsafe {
            (*addr_of_mut!(TILE_R)).clear();
            (*addr_of_mut!(TILE_R)).push((0, 0, Rect::new(400.0, 400.0, 0.0, 0.0)));
        }
        assert_eq!(hit(400.0, 400.0), None, "a zero-size tile is not a target");
        unsafe { (*addr_of_mut!(TILE_R)).clear() };
        crate::search::reset();
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

    // ---- the scroll target ---------------------------------------------------------------------

    /// The whole rule, on the pure form: pin the focused shelf's top to `CONTENT_TOP`, clamp to the
    /// content's end, and hold at zero whenever the shelves are not what focus is on.
    #[test]
    fn the_focused_shelf_scrolls_its_top_to_content_top() {
        // Three poster shelves at 549 apiece — `HEAD_TO_ROW + CARD_H + LABEL_BLOCK`.
        let tops = [CONTENT_TOP, CONTENT_TOP + 549.0, CONTENT_TOP + 1098.0];
        let end = CONTENT_TOP + 1647.0;
        let at = |z, e, i: usize| scroll_target_for(z, e, tops.get(i).copied(), end);

        assert_eq!(at(Zone::Results, false, 0), 0.0, "shelf 0 is already at CONTENT_TOP");
        assert_eq!(at(Zone::Results, false, 1), 549.0, "shelf 1's top comes to CONTENT_TOP");

        // The last shelf is clamped by the content's own end, not pinned: scrolling it all the way
        // to CONTENT_TOP would drag empty space up from under the flow.
        let want_pin = tops[2] - CONTENT_TOP;
        let cap = end - SCR_H;
        assert!(cap < want_pin, "the fixture must actually exercise the clamp");
        assert_eq!(at(Zone::Results, false, 2), cap);

        // Frozen while the keyboard is up — the result set stays still under the user's eyes.
        assert_eq!(at(Zone::Results, true, 2), 0.0);
        // …and whenever the shelves do not hold focus, or shelf 0 would draw up through the field.
        for z in [Zone::Field, Zone::Strip, Zone::Recents] {
            assert_eq!(at(z, false, 2), 0.0, "{z:?} is not the shelves");
        }
        // A flow shorter than the panel never scrolls at all.
        assert_eq!(scroll_target_for(Zone::Results, false, Some(CONTENT_TOP), CONTENT_TOP + 549.0), 0.0);
        // A row index past the end (a landing shrank the set this very frame) is 0, not a panic.
        assert_eq!(at(Zone::Results, false, 9), 0.0);
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
        let mut settle = |now: u32| {
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

    /// The block ladder the tops above are built from, so a tile-size change cannot silently
    /// re-space the flow: a poster shelf must stack at `consts::ROW_PITCH`, the app's one shelf
    /// rhythm, and the two odd tile sizes must fall out of the same expression.
    #[test]
    fn a_poster_shelf_stacks_at_the_apps_own_row_pitch() {
        use crate::search::Kind;
        assert_eq!(block_h(Kind::Movie), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Show), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Collection), crate::ui::consts::ROW_PITCH);
        assert_eq!(block_h(Kind::Episode), HEAD_TO_ROW + 236.0 + LABEL_BLOCK);
        assert_eq!(block_h(Kind::Person), HEAD_TO_ROW + 250.0 + LABEL_BLOCK);

        // The layout promise the module doc makes, as the ACTUAL numbers rather than the 699 the
        // doc used to claim: the first shelf's posters end at 683 against a panel edge of 700.
        let first_row_bottom = CONTENT_TOP + HEAD_TO_ROW + crate::ui::consts::CARD_H;
        assert_eq!(first_row_bottom, 683.0);
        assert_eq!(SCR_H - KEYBOARD_H, 700.0);
        assert!(first_row_bottom <= SCR_H - KEYBOARD_H, "with the keyboard up, nothing we own hides behind it");

        // `LABEL_BLOCK` is the band `results.rs` will draw a `TileLabel` into, and `ui/CLAUDE.md`
        // names `TileLabel::height` the ONE authority on how tall that is. Reserving less than it
        // returns runs the caption into the next shelf's heading.
        let one_line = crate::ui::card_row::TileLabel::height(&crate::ui::card_row::RowStyle::HOME, true);
        assert!(LABEL_BLOCK >= one_line, "the reserved band ({LABEL_BLOCK}) must hold a one-line captioned label ({one_line})");
    }

    // ---- typing ---------------------------------------------------------------------------------

    #[test]
    fn backspace_only_bites_while_the_panel_is_up_and_takes_a_whole_character() {
        let _s = crate::testlock::serial();
        let _g = ZLOCK.lock().unwrap_or_else(|e| e.into_inner());
        reset();
        crate::search::set_query("wallacé");
        assert!(!key(SDLK_BACKSPACE), "with the panel down the screen has no claim on the key");
        assert_eq!(crate::search::query(), "wallacé", "…and must not have edited the query anyway");

        unsafe { *addr_of_mut!(EDITING) = true };
        assert!(key(SDLK_BACKSPACE), "editing, the screen consumes it");
        assert_eq!(crate::search::query(), "wallac", "a whole codepoint, not one byte of a two-byte char");
        assert!(!key(SDLK_UP), "everything else falls through to app.rs");

        crate::search::set_query("");
        assert!(key(SDLK_BACKSPACE), "an empty query still consumes the key — it is the field's");
        assert_eq!(crate::search::query(), "");
        unsafe { *addr_of_mut!(EDITING) = false };
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
