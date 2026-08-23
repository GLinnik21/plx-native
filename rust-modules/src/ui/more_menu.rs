//! The player transport's **overflow menu** — the popover behind the third control disc (`…`), on
//! the same animated [`TableView`] as the subtitle/audio and profile menus. It only REPORTS the
//! chosen [`Action`]; `app.rs` performs it, exactly as [`crate::ui::account_menu`] does.
//!
//! # Why an overflow menu exists at all
//!
//! **Stats for nerds**, the diagnostics overlay ([`crate::ui::stats`]), needs a home a stranger can
//! find, because it is how this app gets bug reports off televisions nobody here owns — every other
//! diagnostic surface in the codebase (the `/tmp/plxnative-*` triggers, the remote FIFO, the
//! capture stream) is compiled out of RELEASE builds by the `devtriggers` feature, which is what a
//! user installs. "Press `…`, turn Stats for nerds on, photograph the screen" is a sentence that
//! fits in a GitHub reply and needs no ssh, no root and no rebuild.
//!
//! It held that one row for a while, and a menu with one row is not a mistake either: the
//! alternative — hanging the toggle off a hidden key chord — is undiscoverable by exactly the
//! people who would report the bug, and the alternative to THAT is a fourth disc for a control most
//! users touch once. Overflow is what a `…` means.
//!
//! # Two sections, and the two row idioms they are each drawn in
//!
//! **Options** holds switches. Its row carries [`Row::toggle`], so it states itself as the WORD
//! `On`/`Off` at the row's trailing edge. It is a STATE, not a destination: a chevron would promise
//! a page behind the row and there is none.
//!
//! **Quality** is the [`crate::route::Quality`] ladder — Auto plus a few rungs — and its rows carry
//! [`Row::checked`]'s LEADING checkmark, which means "the active one of several". That is the same
//! design-system rule from the other side: **a mark says where you are and a word says what is set,
//! and no row says both**, which is why a rung's rate rides inside its own label rather than in a
//! trailing value beside the mark. (The Options row drew as a PAIR OF MARKS for one day, a ring
//! ticked when on; those assets were deleted the same evening — see [`crate::ui::icons`].)
//!
//! A flat popover with a header per section, deliberately, rather than a Quality row that drills
//! into a second page: `docs/parity-gaps.md`'s standing decision is that this app has **no
//! full-screen menu sheets** — the reference clients put playback quality in one and we do not —
//! and a drill-in inside a popover would need a BACK that means "up one page" where every other
//! panel's BACK means "dismiss". Six rungs and a switch fit; when they stop fitting, the
//! [`TableView`] scrolls, which is what it is for.
//!
//! The menu closes on commit either way, so the read-out is never what confirms the press: the
//! overlay appearing behind the dismissed panel — or, for a rung, the next play routing differently
//! — is.
//!
//! # What a picked rung does, and what it deliberately does not
//!
//! It is a ROUTING policy, not a number handed to the transcoder: over-ceiling content loses direct
//! play *and* the container remux, which is the only way a cap can bind at all. The whole argument
//! is [`crate::route::Quality`]'s doc.
//!
//! It binds every future play, **and it re-decides the one on screen** — because this menu is the
//! ladder's only entry point, so a rung that waited for the next play would be a control that
//! visibly does nothing everywhere it can be reached. `route::set_quality` re-asks the routing
//! question with the new rung and reloads only when the answer changed; picking a HIGHER rung than
//! the picture already satisfies does nothing at all. That is a user-initiated switch and not an
//! adaptive one — nothing measures a link or moves a rung on its own.
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
    /// select a rung of the playback-quality ladder ([`crate::route::set_quality`])
    SetQuality(crate::route::Quality),
}

static mut POP: Popover = Popover::new(); // shared open/appear choreography
static mut TABLE: TableView = TableView::new(); // main-thread only
/// The ordered rows captured at [`open`] — the ONE place row order lives, so [`on_ok`]'s index
/// mapping cannot drift from what was drawn. (`account_menu`'s rationale, and its bug.)
///
/// An owned `Vec` rather than the `&'static [Action]` it was, because the Quality section's rows
/// are BUILT from `route::QUALITY_LADDER` rather than written out here. Main-thread only, like
/// `TABLE` beside it, and read through `addr_of!` for the same reason — never as `&ROWS`.
static mut ROWS: Vec<Action> = Vec::new();

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

/// Every row the menu can offer, in order and ACROSS SECTIONS. A free function (rather than a
/// literal inside [`open`]) so the index mapping [`on_ok`] relies on is one testable value.
///
/// **The order here is the whole contract**, because [`TableView`]'s `sel` is a single flat index
/// over every row of every section: this list must be built in exactly the order [`open`] pushes
/// rows, or a press commits its neighbour. A separator would be a row here too — there is none, and
/// the debug assert in [`open`] is what would catch one being added on one side only.
fn rows_for() -> Vec<Action> {
    let mut v = vec![Action::ToggleStats];
    v.extend(crate::route::QUALITY_LADDER.iter().map(|q| Action::SetQuality(*q)));
    v
}

fn label(a: Action) -> &'static str {
    match a {
        Action::ToggleStats => "Stats for nerds",
        // the rung names itself — rate and frame in one string, because the row already carries
        // the picker's leading mark (see this module's doc)
        Action::SetQuality(q) => q.label(),
        Action::None => "",
    }
}

/// Whether the SWITCH a row names is currently on. It reaches the row as [`Row::toggle`] and so
/// draws as the WORD `On`/`Off` at the trailing edge — never as a picker's leading checkmark, which
/// means "the active one of several" and is what the Quality rung rows use instead. Two idioms, one
/// rule: a mark says where you are and a word says what is set, and no row says both. (Named
/// `checked` until 2026-08-21, from the row builder it does not call — a name that read as a
/// promise of the leading mark an Options row deliberately does not draw.)
fn is_on(a: Action) -> bool {
    match a {
        Action::ToggleStats => crate::ui::stats::enabled(),
        // a rung is not a switch — see `row_for`, which gives it the leading mark instead
        Action::SetQuality(_) | Action::None => false,
    }
}

/// One row, drawn in the idiom its ACTION calls for. Free-standing (rather than inline in [`open`])
/// so the two idioms are decided in one place: a switch gets the trailing word, a picker rung gets
/// the leading mark, and nothing gets both.
fn row_for(a: Action) -> Row {
    match a {
        Action::SetQuality(q) => Row::new(label(a)).checked(crate::route::quality() == q),
        _ => Row::new(label(a)).toggle(is_on(a)),
    }
}

pub fn open() {
    let rows = rows_for();
    // TWO sections, built in ROWS order — see `rows_for`: `TableView::sel` is one flat index over
    // both, so the split here is presentational and the ORDER is the contract.
    let mut options = Section::new("Options");
    let mut quality = Section::new("Quality");
    for a in &rows {
        match a {
            Action::SetQuality(_) => quality = quality.row(row_for(*a)),
            _ => options = options.row(row_for(*a)),
        }
    }
    table().compact = true; // a short action list — BODY labels, like the profile menu
    table().set_sections(vec![options, quality], 0, false);
    // ROWS *is* the index→action map, so it must stay one-to-one with what was built above.
    debug_assert_eq!(rows.len() as i32, table().n_rows());
    // ASSIGN, never `ptr::write`: `ROWS` owns its `Vec` now, and `write` does not drop what was
    // there — so every `…` press leaked the previous row list. (The `&'static [Action]` this
    // replaced had nothing to drop, which is why the old spelling was correct and this one is not.)
    unsafe { *addr_of_mut!(ROWS) = rows };
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
    // BORROW the row list, never `read()` it: `ROWS` owns its `Vec` now, and a `read` would move
    // the allocation out of the static and drop it at the end of this expression.
    action_at(unsafe { &*addr_of!(ROWS) }, sel)
}

/// The row list IS the mapping — a selection outside it is `None` rather than whatever action
/// happens to sit at that index in some other row set.
fn action_at(rows: &[Action], sel: i32) -> Action {
    usize::try_from(sel).ok().and_then(|i| rows.get(i)).copied().unwrap_or(Action::None)
}

/// The panel at its TALLEST, for the overscan audit ([`crate::ui::consts::SAFE`]).
#[cfg(test)]
pub(crate) fn overscan_rects(out: &mut Vec<(&'static str, Rect)>) {
    let (pw, ph) = (448.0f32, 320.0f32);
    let bottom = SCR_H - 316.0;
    out.push(("… overflow menu panel", Rect::new(crate::ui::player_hud::CTRL_RIGHT - pw, bottom - ph, pw, ph)));
}

/// Bottom-right, above the control row — anchored to the `…` disc that opened it, the way the
/// track menu is anchored to the pair beside it. Shares the track menu's right margin
/// (`player_hud::CTRL_RIGHT`, the discs' own edge) and its bottom edge, so opening one after the
/// other does not make the panel hop.
fn panel_rect() -> Rect {
    let pw = 448.0f32;
    let px = crate::ui::player_hud::CTRL_RIGHT - pw;
    let bottom = SCR_H - 316.0; // ~28px above the discs, as track_menu
    // The ceiling was 320 while this menu held one row, and it was invisible then. With the
    // Quality ladder beside it `measured_height()` is 600 — two headers, seven rows, a divider,
    // AND the table's own top/bottom padding — so a 320 cap put four of nine rows on screen and
    // silently scrolled the rest, which is a picker whose options you cannot see.
    //
    // The cap is a FRACTION of the room the panel has rather than a subtraction from it: the panel
    // is anchored at `bottom` and grows upward, so `bottom` IS the space, and 0.86 of it leaves a
    // clear margin at the top of the frame while comfortably clearing 600. Reaching for a
    // `bottom - <margin>` literal is what put the first version of this line 4px UNDER the content
    // — the margin was derived from the 560 of content and forgot the 40 of padding, so the last
    // rung was clipped until you scrolled: the same symptom, one row deep instead of five. Past
    // the cap it scrolls, which is what `TableView` is for.
    let ph = table().measured_height().clamp(120.0, bottom * 0.86);
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
            assert!(!label(a).is_empty(), "{a:?} would draw a blank row");
        }
    }

    #[test]
    fn a_selection_maps_to_its_row() {
        let rows = rows_for();
        assert_eq!(action_at(&rows, 0), Action::ToggleStats);
        // …and the Quality section follows the Options one, in `route::QUALITY_LADDER` order.
        // `sel` is ONE flat index over both sections, so this is the join that a section split
        // could quietly break: row 1 must be the ladder's head, not its second rung.
        assert_eq!(action_at(&rows, 1), Action::SetQuality(crate::route::Quality::Auto));
        for (i, q) in crate::route::QUALITY_LADDER.iter().enumerate() {
            assert_eq!(action_at(&rows, 1 + i as i32), Action::SetQuality(*q));
        }
    }

    /// Out-of-range must be `None`, never a neighbouring action: `sel` survives a rebuild, so a
    /// shorter row set can be asked for an index the previous one had.
    #[test]
    fn an_out_of_range_selection_is_none_not_a_neighbour() {
        let rows = rows_for();
        assert_eq!(action_at(&rows, rows.len() as i32), Action::None);
        assert_eq!(action_at(&rows, -1), Action::None);
        assert_eq!(action_at(&[], 0), Action::None);
    }
}
