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
pub const GRID_TOP_Y: f32 = 150.0; // shelf top in grid view

// SDL keycodes (scancode | SDLK_SCANCODE_MASK)
pub const SDLK_RIGHT: c_uint = 79 | (1 << 30);
pub const SDLK_LEFT: c_uint = 80 | (1 << 30);
pub const SDLK_DOWN: c_uint = 81 | (1 << 30);
pub const SDLK_UP: c_uint = 82 | (1 << 30);

// spring stiffnesses (from ui_home.c, redistributed 1:1 to their owning views)
pub const K_SCALE: f32 = 320.0;
pub const K_SCROLL: f32 = 170.0;
pub const K_SNAP: f32 = 200.0;
