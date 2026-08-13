//! Layout + input + animation constants, mirroring src/ui_home.h and ui_home.c.
//! Single source so the hand-tuned pixel offsets can't drift between widgets.
#![allow(dead_code)]
use std::os::raw::c_uint;

pub const CARD_W: f32 = 250.0;
pub const CARD_H: f32 = 375.0;
pub const GAP: f32 = 30.0;
pub const MARGIN_X: f32 = 90.0;
pub const ROW_TITLE_H: f32 = 30.0;
pub const ROW_PITCH: f32 = CARD_H + ROW_TITLE_H + 144.0; // 549: room for the shelf title above + the focused card's title AND caption below (clears the next shelf's title)
/// Hub-title cap top above the shelf's `row_y` origin — the heading draws at `row_y − TITLE_DY`,
/// minus whatever `CardRow::lift` has raised it by. Named because it is a LAYOUT relationship two
/// other constants here are derived against ([`CARD_DY`]'s air, [`GRID_TOP_Y`]'s clearance under the
/// profile chip) and because the shelf heading is now a multi-run flow rather than one `p.text`.
pub const TITLE_DY: f32 = 34.0;
/// Card top below the shelf's `row_y` origin (the hub title draws at `row_y − `[`TITLE_DY`]): the air
/// between a section title and its posters, held on magnification too because `title_lift` raises the
/// title by the same amount the popped card's top rises.
pub const CARD_DY: f32 = 26.0;
pub const CONTENT_Y: f32 = 200.0;
pub const GLOW_PAD: f32 = 48.0;
pub(crate) use crate::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};

// hero <-> grid continuum
/// Shelf top in hero view. Re-derived once the peek row stopped magnifying its focused cell: the
/// peek used to be judged off that popped tile, whose 1.09 scale about its centre lifted its top
/// edge `CARD_H * 0.09 / 2 ≈ 17px` above every other card in the row. Un-popping the row dropped
/// the whole shelf by that much, so the peek is 17px shallower here to keep the composition the
/// hero view was tuned to (828 → 811; card top = `PEEK_Y + CARD_DY` = 837, as the popped one was).
pub const PEEK_Y: f32 = 811.0;
// shelf top in grid view — leaves the first hub title (row_y − TITLE_DY, lifted up to ~10 more when its
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
pub const SDLK_PAGEUP: c_uint = 75 | (1 << 30);
pub const SDLK_PAGEDOWN: c_uint = 78 | (1 << 30);
/// Magic-Remote CH▲/CH▼ rocker — webOS keyCodes 33/34 (page the Library grid). Matched
/// alongside the SDLK_PAGE* syms; verify the raw wcodes in the event log on a new remote.
pub const WCODE_CH_UP: c_uint = 33;
pub const WCODE_CH_DOWN: c_uint = 34;
/// Magic-Remote transport keys. The ONE home for these wcodes: the real key handler
/// (app.rs) and the remote-injection token map both match against these names.
pub const WCODE_PAUSE: c_uint = 72;
pub const WCODE_STOP: c_uint = 413;
pub const WCODE_PLAY: c_uint = 450;

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
