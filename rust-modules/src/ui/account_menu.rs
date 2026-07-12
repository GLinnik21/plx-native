//! The Home **profile menu** — a small popover opened from the top-left profile chip, on the SAME
//! animated [`TableView`] as the in-player subtitle/audio menu. Two actions: switch Plex Home
//! profile ("Change profile" → who's-watching) or "Sign out". The menu only reports the chosen
//! action via [`on_ok`]; `app.rs` performs the routing.
#![allow(non_upper_case_globals)]
use crate::ui::consts::*;
use crate::ui::table::{Row, Section, TableView};
use crate::ui::{theme, Painter, Rect, Spring};
use std::os::raw::c_int;
use std::ptr::{addr_of, addr_of_mut};

/// What the highlighted row does on OK.
pub enum Action {
    None,
    ChangeProfile,
    SignOut,
}

static mut OPEN: bool = false;
static mut APPEAR: Spring = Spring::at(0.0); // 0→1 fade+slide on open
static mut TABLE: TableView = TableView::new(); // main-thread only

fn table() -> &'static mut TableView {
    unsafe { &mut *addr_of_mut!(TABLE) }
}

pub fn is_open() -> bool {
    unsafe { addr_of!(OPEN).read() }
}

pub fn open() {
    // header = the signed-in profile name (owner with no Plex Home selection → "Account").
    let name = crate::plex::session::current()
        .map(|u| u.title)
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "Account".to_string());
    let sec = Section::new(name)
        .row(Row::new("Change profile").chevron(true))
        .row(Row::new("Sign out"));
    table().set_sections(vec![sec], 0, false);
    unsafe {
        addr_of_mut!(APPEAR).write(Spring::at(0.0));
        addr_of_mut!(OPEN).write(true);
    }
}

pub fn close() {
    unsafe { addr_of_mut!(OPEN).write(false) }
}

pub fn move_focus(sym: c_int) {
    let s = sym as u32;
    if s == SDLK_UP {
        table().move_sel(-1);
    } else if s == SDLK_DOWN {
        table().move_sel(1);
    }
}

/// Commit the highlighted row and close.
pub fn on_ok() -> Action {
    let sel = table().sel;
    close();
    match sel {
        0 => Action::ChangeProfile,
        1 => Action::SignOut,
        _ => Action::None,
    }
}

/// Top-left popover, tucked under the profile chip.
fn panel_rect() -> Rect {
    let pw = 440.0f32;
    let px = 80.0f32;
    let py = 150.0f32;
    let ph = table().measured_height().clamp(120.0, 440.0);
    Rect::new(px, py, pw, ph)
}

pub fn update(dt: f32) {
    if !is_open() {
        return;
    }
    let sp = unsafe { &mut *addr_of_mut!(APPEAR) };
    sp.step(1.0, 300.0, dt);
    let ph = panel_rect().h;
    table().update(dt, ph - 40.0);
}

pub fn draw() {
    if !is_open() {
        return;
    }
    let appear = unsafe { addr_of!(APPEAR).read() }.pos.clamp(0.0, 1.0);
    let dim = theme::scrim_black(0.5 * appear);
    Painter::root().rect(Rect::FULL, 0.0, dim, dim, 0.0);

    let r = panel_rect();
    let rise = (1.0 - appear) * 16.0; // slide DOWN into place from above the chip
    let p = Painter::root().alpha(appear).translate(0.0, -rise);
    p.rect(r, 24.0, theme::PANEL_TOP, theme::PANEL_BOT, 0.0);
    table().draw(p, r);
}
