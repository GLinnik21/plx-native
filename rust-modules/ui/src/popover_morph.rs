//! The **morph** open style: a glass opener (a pill) grows into its panel instead of fading and
//! sliding in on [`crate::popover::K_APPEAR`]. Selected per opener by
//! [`crate::popover::Opener::open_style`] = [`crate::popover::OpenStyle::Morph`].
//!
//! CONTRACT FREEZE stub: the public shape below is what the other packages compile against; the
//! spring bodies, the pill-to-panel rect interpolation and the cross-dissolve are DS7's. Until DS7
//! lands, [`rect_at`] returns the panel rect unchanged (no morph), so selecting `Morph` is
//! visually the final panel and never an empty glass frame.

use crate::Rect;

/// Fallback when the opener has no measured rect to morph from: a fade+scale of the panel about
/// its centre. This is the starting scale of that fallback (DS7 fixes the value from the board).
pub const FALLBACK_SCALE_FROM: f32 = 1.0;

/// The panel's rect at morph progress `t` (0 = the opener's pill rect, 1 = the final panel).
/// `from` is `None` when the opener has no rect (use the fade+scale fallback).
pub fn rect_at(from: Option<Rect>, panel: Rect, t: f32) -> Rect {
    let _ = (from, t);
    panel
}
