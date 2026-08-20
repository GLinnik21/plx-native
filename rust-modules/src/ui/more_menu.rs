//! The player transport's **overflow menu** — the popover behind the third control disc (`…`), on
//! the same animated [`TableView`] as the subtitle/audio and profile menus. It only REPORTS the
//! chosen [`Action`]; `app.rs` performs it, exactly as [`crate::ui::account_menu`] does.
//!
//! # Why an overflow menu exists at all
//!
//! One row today: **Stats for nerds**, the diagnostics overlay ([`crate::ui::stats`]). It needs a
//! home a stranger can find, because it is how this app gets bug reports off televisions nobody
//! here owns — every other diagnostic surface in the codebase (the `/tmp/plxnative-*` triggers, the
//! remote FIFO, the capture stream) is compiled out of RELEASE builds by the `devtriggers` feature,
//! which is what a user installs. "Press `…`, tick Stats for nerds, photograph the screen" is a
//! sentence that fits in a GitHub reply and needs no ssh, no root and no rebuild.
//!
//! A menu with one row is not a mistake. The alternative — hanging the toggle off a hidden key
//! chord — is undiscoverable by exactly the people who would report the bug, and the alternative to
//! THAT is a fourth disc for a control most users touch once. Overflow is what a `…` means.
//!
//! # A switch, not a chevron — and not a picker's checkmark either
//!
//! The row carries [`Row::toggle`], so it states itself as the WORD `On`/`Off` at the row's
//! trailing edge. It is a STATE, not a destination: a chevron would promise a page behind the row
//! and there is none. It is equally not [`Row::checked`]'s leading checkmark, which means "the
//! active one of several" and would be answering a question a lone switch never asked — the design
//! system's rule is that a mark says where you are and a word says what is set, and no row says
//! both. (It drew as a PAIR OF MARKS for one day, a ring ticked when on; those assets were deleted
//! the same evening — see [`crate::ui::icons`].) The menu closes on commit either way, so the
//! read-out is never what confirms the press: the overlay appearing behind the dismissed panel is,
//! which is louder than a word the dismissal would take off screen anyway.
#![allow(non_upper_case_globals)]
use crate::ui::consts::*;
use crate::ui::popover::Popover;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::{theme, Rect};
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// What the highlighted row does on OK.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    None,
    /// flip [`crate::ui::stats`]'s overlay on/off
    ToggleStats,
}

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The ordered rows captured at [`open`] — the ONE place row order lives, so [`on_ok`]'s index
/// mapping cannot drift from what was drawn. (`account_menu`'s rationale, and its bug.)
static mut ROWS: &[Action] = &[];

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

/// The highlighted row, for the focus probe (`crate::focusprobe`) — a READ of the cursor the key
/// ladder moves, and the reason it exists: `app.rs`'s UP/DOWN arm for this panel changes nothing
/// else, so without this the fingerprint records the panel opening and closing and nothing between.
/// Through `addr_of!` rather than the module's own `table()`, which hands out a `&'static mut`.
pub(crate) fn sel() -> i32 {
    unsafe { (*addr_of!(TABLE)).sel }
}
fn pop() -> &'static mut Popover {
    unsafe { &mut *addr_of_mut!(POP) }
}

pub fn is_open() -> bool {
    unsafe { (*addr_of!(POP)).is_open() }
}

/// Every row the menu can offer, in order. A free function (rather than a literal inside [`open`])
/// so the index mapping [`on_ok`] relies on is one testable value.
fn rows_for() -> &'static [Action] {
    &[Action::ToggleStats]
}

fn label(a: Action) -> &'static str {
    match a {
        Action::ToggleStats => "Stats for nerds",
        Action::None => "",
    }
}

/// Whether the state a row names is currently on. It reaches the row as [`Row::toggle`] and so
/// draws as the WORD `On`/`Off` at the trailing edge — never as a picker's leading checkmark: this
/// menu's rows are things you turn on and off, and nothing here is "the active one of several".
fn checked(a: Action) -> bool {
    match a {
        Action::ToggleStats => crate::ui::stats::enabled(),
        Action::None => false,
    }
}

pub fn open() {
    let rows = rows_for();
    unsafe { addr_of_mut!(ROWS).write(rows) };
    let mut sec = Section::new("Options");
    for a in rows {
        sec = sec.row(Row::new(label(*a)).toggle(checked(*a)));
    }
    table().compact = true; // a short action list — BODY labels, like the profile menu
    table().set_sections(vec![sec], 0, false);
    // ROWS *is* the index→action map, so it must stay one-to-one with what was built above.
    debug_assert_eq!(rows.len() as i32, table().n_rows());
    pop().open();
}

pub fn close() {
    pop().close();
}

pub fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    }
}

/// Pointer hover: focus follows the cursor over the popover rows.
pub fn pointer_focus(mx: f32, my: f32) {
    if !is_open() {
        return;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
    }
}

/// Pointer click: commit the row under the cursor (same as OK); a click elsewhere reports
/// `Action::None` and the caller dismisses like BACK.
pub fn click(mx: f32, my: f32) -> Action {
    if !is_open() {
        return Action::None;
    }
    if let Some(gi) = table().hit_row(panel_rect(), mx, my) {
        table().sel = gi;
        return on_ok();
    }
    Action::None
}

/// Commit the highlighted row and close.
pub fn on_ok() -> Action {
    let sel = table().sel;
    close();
    action_at(unsafe { addr_of!(ROWS).read() }, sel)
}

/// The row list IS the mapping — a selection outside it is `None` rather than whatever action
/// happens to sit at that index in some other row set.
fn action_at(rows: &[Action], sel: i32) -> Action {
    usize::try_from(sel).ok().and_then(|i| rows.get(i)).copied().unwrap_or(Action::None)
}

/// Bottom-right, above the control row — anchored to the `…` disc that opened it, the way the
/// track menu is anchored to the pair beside it. Shares the track menu's right margin (80) and its
/// bottom edge, so opening one after the other does not make the panel hop.
fn panel_rect() -> Rect {
    let pw = 448.0f32;
    let px = SCR_W - 80.0 - pw;
    let bottom = SCR_H - 316.0; // ~28px above the discs, as track_menu
    let ph = table().measured_height().clamp(120.0, 320.0);
    Rect::new(px, bottom - ph, pw, ph)
}

pub fn update(dt: f32) {
    if !is_open() {
        return;
    }
    pop().update(dt);
    let ph = panel_rect().h;
    table().update(dt, ph - 40.0);
}

pub fn draw() {
    if !is_open() {
        return;
    }
    let p = pop().painter(0.5, 16.0); // rises INTO place from below, toward the disc that opened it
    let r = panel_rect();
    p.rect(r, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
    table().draw(p, r);
}

/// The index→action mapping, which is the only part of a popover that is testable off the main
/// thread: `open`/`draw` own `static mut TABLE`/`POP` and are deliberately not `Sync`.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_has_a_label() {
        for a in rows_for() {
            assert!(!label(*a).is_empty(), "{a:?} would draw a blank row");
        }
    }

    #[test]
    fn a_selection_maps_to_its_row() {
        let rows = rows_for();
        assert_eq!(action_at(rows, 0), Action::ToggleStats);
    }

    /// Out-of-range must be `None`, never a neighbouring action: `sel` survives a rebuild, so a
    /// shorter row set can be asked for an index the previous one had.
    #[test]
    fn an_out_of_range_selection_is_none_not_a_neighbour() {
        let rows = rows_for();
        assert_eq!(action_at(rows, rows.len() as i32), Action::None);
        assert_eq!(action_at(rows, -1), Action::None);
        assert_eq!(action_at(&[], 0), Action::None);
    }
}
