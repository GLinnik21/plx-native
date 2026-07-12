//! Layout + input + animation constants, mirroring src/ui_home.h and ui_home.c.
//! Single source so the hand-tuned pixel offsets can't drift between widgets.
#![allow(dead_code)]
use std::os::raw::c_uint;

pub const ROWS: usize = 5;
pub const COLS: usize = 10;
pub const CARD_W: f32 = 250.0;
pub const CARD_H: f32 = 375.0;
pub const GAP: f32 = 30.0;
pub const MARGIN_X: f32 = 90.0;
pub const ROW_TITLE_H: f32 = 30.0;
pub const ROW_PITCH: f32 = CARD_H + ROW_TITLE_H + 100.0; // 505: room for the shelf title above + the focused card's title below (clears the next shelf's title)
pub const CONTENT_Y: f32 = 200.0;
pub const GLOW_PAD: f32 = 48.0;
pub const SCR_W: f32 = 1920.0;
pub const SCR_H: f32 = 1080.0;

// hero <-> grid continuum
pub const PEEK_Y: f32 = 828.0; // shelf top in hero view
// shelf top in grid view — leaves the first hub title (row_y − 34, lifted up to ~10 more when its
// leftmost card magnifies) a clear space::MD under the profile chip (bottom edge 108)
pub const GRID_TOP_Y: f32 = 176.0;

// SDL keycodes (scancode | SDLK_SCANCODE_MASK, or ASCII)
pub const SDLK_RIGHT: c_uint = 79 | (1 << 30);
pub const SDLK_LEFT: c_uint = 80 | (1 << 30);
pub const SDLK_DOWN: c_uint = 81 | (1 << 30);
pub const SDLK_UP: c_uint = 82 | (1 << 30);
pub const SDLK_RETURN: c_uint = 13;
pub const SDLK_KP_ENTER: c_uint = 88 | (1 << 30);
pub const SDLK_SELECT: c_uint = 77 | (1 << 30);
pub const SDLK_ESCAPE: c_uint = 27;

/// OK/confirm press — RETURN, keypad ENTER, or the remote's SELECT. The ONE OK predicate
/// (app.rs + the login/profiles screens all route through it).
#[inline]
pub fn is_ok(sym: c_uint) -> bool {
    sym == SDLK_RETURN || sym == SDLK_KP_ENTER || sym == SDLK_SELECT
}
/// webOS BACK — ESC / 'q' (dev keyboards) or the remote BACK wcodes (this Magic Remote sends
/// 482 = 0x1E2; 461 kept for other remotes). The ONE BACK predicate.
#[inline]
pub fn is_back(sym: c_uint, wcode: c_uint) -> bool {
    sym == SDLK_ESCAPE || sym == 'q' as c_uint || wcode == 461 || wcode == 482
}

// spring stiffnesses (from ui_home.c, redistributed 1:1 to their owning views)
pub const K_SCALE: f32 = 320.0;
pub const K_SCROLL: f32 = 170.0;
pub const K_SNAP: f32 = 200.0;
