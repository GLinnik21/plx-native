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

// ---- the ROUTE frame (a full screen that asks one question and ends) -------------------------
// The design system's own `screens.css` route tokens, which had no equivalent here because the
// first-run question (`ui/first_run.rs`) is the first screen built to them. Sign-in and the
// who's-watching picker predate the frame and still centre their own compositions.
//
// The shape is two columns on the app's own ground: what this is and the single action on the
// LEFT, the list itself on the RIGHT. No panel under either — a panel's depth claims there is a
// screen behind it, and a route has none.

/// **ONE top guide, and both columns hang from it** (`--route-top`). The design canvas draws two
/// separate `--route-copy-top` / `--route-list-top` vars that the design system never defines;
/// its own route-screen card uses this single line for the copy column's `top` and the list
/// column's `padding-top`, which is what this follows.
pub const ROUTE_TOP: f32 = 150.0;
/// Left edge of both the copy column and its action — the shared page margin, not a second number
/// (`--route-copy-x` IS `--margin-x` in the design system).
pub const ROUTE_COPY_X: f32 = MARGIN_X;
/// Width of the copy column (`--route-copy-w`): the measure the heading and the paragraph wrap to.
pub const ROUTE_COPY_W: f32 = 760.0;
/// Gap from the bottom of the frame to the action's BOTTOM edge (`--route-action-bottom`). The copy
/// sits at the top of its column and the action at the foot of it, so the two ends of the screen
/// are the two things you do: read it once, then leave.
pub const ROUTE_ACTION_BOTTOM: f32 = 74.0;
/// The action pill's height — the app's one control height, as every other `Button` frame uses.
pub const ROUTE_ACTION_H: f32 = 60.0;
/// Left edge of the list column (`--route-list-x`). Its right edge is the page margin, so the
/// column is [`ROUTE_LIST_W`] wide.
pub const ROUTE_LIST_X: f32 = 930.0;
/// The list column's width — derived, never a second literal: right edge at the page margin.
pub const ROUTE_LIST_W: f32 = SCR_W as f32 - ROUTE_LIST_X - MARGIN_X;
/// The tallest a route's list may draw: from the shared top guide down to the action's own bottom
/// line, so a long list is cut level with the control rather than running off the frame.
pub const ROUTE_LIST_H_MAX: f32 = SCR_H as f32 - ROUTE_TOP - ROUTE_ACTION_BOTTOM;

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
