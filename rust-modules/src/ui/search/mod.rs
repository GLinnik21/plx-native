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
//! the bottom ~380px, and the field, the first shelf's heading and that shelf's full row of
//! posters are sized to land exactly on its top edge ([`CONTENT_TOP`] + [`HEAD_TO_ROW`] + a
//! 375-tall poster = 699). That is also why nothing scrolls while it is up: the result set has to
//! be stable under the user's eyes while they are still typing.
#![allow(dead_code)]

use crate::ui::{Painter, Rect};

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

/// Which region owns the remote. Not a focus INDEX — each zone keeps its own cursor, so leaving
/// the shelves for the field and coming back lands where you were.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Zone {
    /// The shared top strip. Reached with ▲ from the field; the screen does not own the pills.
    Strip,
    Field,
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
    pub(crate) recent: usize,
    /// Vertical offset the shelves are drawn at (0 while the keyboard is up).
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

/// Snapshot the state machine for this frame's regions.
fn view() -> View {
    unsafe {
        View {
            zone: *std::ptr::addr_of!(ZONE),
            editing: *std::ptr::addr_of!(EDITING),
            row: *std::ptr::addr_of!(ROW),
            col: *std::ptr::addr_of!(COL),
            recent: *std::ptr::addr_of!(RECENT),
            shift: 0.0,
        }
    }
}

/// Mount the screen. `q` pre-seeds the field — the boot trigger's whole job, since a headless
/// screenshot cannot type.
pub(crate) fn enter(q: &str) {
    unsafe {
        *std::ptr::addr_of_mut!(ZONE) = Zone::Field;
        *std::ptr::addr_of_mut!(EDITING) = false;
        *std::ptr::addr_of_mut!(ROW) = 0;
        *std::ptr::addr_of_mut!(COL) = 0;
        *std::ptr::addr_of_mut!(RECENT) = 0;
    }
    crate::search::set_query(q);
    crate::ui::idle::invalidate();
}

/// Leave it. Dismissing the keyboard here rather than at the press is deliberate: the panel must
/// come down at the fade floor, with the page, not a frame before it while the old screen is still
/// on screen behind it.
pub(crate) fn leave() {
    unsafe { *std::ptr::addr_of_mut!(EDITING) = false };
    crate::textinput::stop();
}

pub(crate) fn update(dt: f32) {
    crate::search::pump(dt);
    let _ = dt;
}

pub(crate) fn draw() {
    crate::gfx::frame_clear(crate::ui::theme::CLEAR_RGB.0, crate::ui::theme::CLEAR_RGB.1, crate::ui::theme::CLEAR_RGB.2);
    let p = Painter::root().alpha(crate::ui::nav::page_alpha());
    let pk = Painter::root().alpha(crate::ui::nav::chrome_alpha());
    let v = view();
    let q = crate::search::query();
    let has_q = !q.trim().is_empty();

    field::draw(p, &v);
    if !has_q && recents::count() > 0 {
        recents::draw(p, &v);
    } else if !has_q || crate::search::shelves().is_empty() {
        empty::draw(p, &v);
    }
    if has_q {
        results::draw(p, &v);
    }

    crate::ui::widgets::profile_chip(pk, Rect::new(crate::ui::consts::MARGIN_X, crate::ui::widgets::TOP_BAR_Y, 54.0, 54.0), 0.0);
    crate::ui::widgets::draw_tab_row(pk);
}

/// Is focus on something a press would OPEN (as opposed to a control that toggles or a field)?
/// `app.rs`'s press machinery asks this to decide whether OK is a tvOS click.
pub(crate) fn focus_is_card() -> bool {
    matches!(view().zone, Zone::Results)
}

pub(crate) fn move_focus(_sym: std::os::raw::c_uint) {}

pub(crate) fn on_ok() -> Action {
    Action::None
}

/// BACK. `false` = "I am done, leave the screen" — the same contract `library::back()` has, and
/// the reason `app.rs`'s BACK arm can treat every screen alike.
pub(crate) fn back() -> bool {
    unsafe {
        if *std::ptr::addr_of!(EDITING) {
            *std::ptr::addr_of_mut!(EDITING) = false;
            crate::textinput::stop();
            crate::ui::idle::invalidate();
            return true;
        }
    }
    false
}

/// The pill the strip should show as SELECTED while this screen is up.
pub(crate) fn selected_pill() -> std::os::raw::c_int {
    crate::ui::widgets::search_pill() as std::os::raw::c_int
}

/// The pill holding remote FOCUS, or -1 when focus is not in the strip.
pub(crate) fn focused_pill() -> std::os::raw::c_int {
    match view().zone {
        Zone::Strip => crate::ui::widgets::search_pill() as std::os::raw::c_int,
        _ => -1,
    }
}

pub(crate) fn pointer_focus(_mx: f32, _my: f32) {}

pub(crate) fn click(_mx: f32, _my: f32) -> Action {
    Action::None
}

pub(crate) fn wheel(_dy: f32) {}
