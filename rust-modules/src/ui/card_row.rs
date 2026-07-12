//! `CardRow` — the shared animated poster-shelf component.
//!
//! Extracts the home shelf's per-row animation state (a focus-scale [`Spring`] per cell + a
//! horizontal scroll spring) and its tile rendering (poster + big glow focus ring + resume bar +
//! centered title) into ONE reusable widget, so the home `Grid` and the detail Related row are
//! literally the same component — [`RowStyle::HOME`] is the single source of the shelf's motion and
//! geometry that both screens read.
//!
//! What stays per-caller is only the x/`base_y` positioning loop, because home threads `env.sp` and
//! splits the loop across the `Grid`'s two-pass **cross-row** focused-last z-order (invariant #3 in
//! `ui/CLAUDE.md`) — which a lone detail row doesn't have. So `CardRow` deliberately owns spring
//! state + the leaf tile draws, and NEVER draws a whole row or applies an in-row focused-last pass
//! on a caller's behalf. It's decoupled from any focus globals / catalog: focus is a plain
//! `Option<usize>`, the item art is a [`Art`] the caller supplies.
use crate::ui::consts::*;
use crate::ui::theme;
use crate::ui::widgets::{card, Art};
use crate::ui::{Painter, Rect, Spring};
use std::os::raw::c_char;

pub(crate) const MAX_ROW_ITEMS: usize = 24; // == home MAX_ITEMS

/// A card row's motion + geometry. [`RowStyle::HOME`] is the single value both the home grid and the
/// detail Related row pass, so the two rows are indistinguishable in look and animation.
#[derive(Clone, Copy)]
pub(crate) struct RowStyle {
    pub w: f32,
    pub h: f32,
    pub gap: f32,
    pub margin_x: f32,
    pub radius: f32,
    pub focus_scale: f32,
    pub ring_pad: f32,
    pub k_scale: f32,
    pub k_scroll: f32,
    /// Circular tiles (cast headshots, who's-watching avatars) vs rounded-rect posters. A circle is
    /// just a tile drawn at `radius = width/2`; the shared springs/scroll/ring are identical.
    pub circular: bool,
}
impl RowStyle {
    /// The home shelf's portrait-poster row: 1.055 focus pop, the big glow ring, animated scroll.
    pub(crate) const HOME: RowStyle = RowStyle {
        w: CARD_W,
        h: CARD_H,
        gap: GAP,
        margin_x: MARGIN_X,
        radius: theme::CARD_RING_RAD,
        focus_scale: 1.055,
        ring_pad: GLOW_PAD,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: false,
    };
    /// Detail "Cast & Crew": circular headshots, a gentle pop, the tight strip ring. Same motion as
    /// HOME (spring magnification + scroll), so cast animates like the poster shelves.
    pub(crate) const CAST: RowStyle = RowStyle {
        w: 190.0,          // = detail CAST_D
        h: 190.0,
        gap: 40.0,         // w+gap = 230 = detail CAST_SLOT (per-member pitch)
        margin_x: MARGIN_X,
        radius: 95.0, // = w/2 (circle); draw_* recomputes per-rect anyway when `circular`
        focus_scale: 1.06,
        ring_pad: 14.0, // matches the detail cast headshot ring (hugs the popped circle)
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: true,
    };
    /// "Who's watching" profile pictures: big circular avatars with a clear pop. Centered by the
    /// caller (a short roster), so the scroll spring stays put unless the row overflows.
    pub(crate) const PROFILES: RowStyle = RowStyle {
        w: 220.0,
        h: 220.0,
        gap: 72.0,
        margin_x: MARGIN_X,
        radius: 110.0,
        focus_scale: 1.08,
        ring_pad: theme::CARD_RING_PAD_STRIP,
        k_scale: K_SCALE,
        k_scroll: K_SCROLL,
        circular: true,
    };
    /// ring focus scalar denominator — `(s-1)/ring_denom` maps scale∈[1, focus_scale] to [0, 1].
    #[inline]
    fn ring_denom(&self) -> f32 {
        self.focus_scale - 1.0
    }
    /// Corner radius for a tile of size `rect` at radius-scale `s`: half-width for a circle, else the
    /// style's fixed radius scaled with the focus pop.
    #[inline]
    fn tile_radius(&self, rect: Rect, s: f32) -> f32 {
        if self.circular {
            rect.w * 0.5
        } else {
            self.radius * s
        }
    }
}

/// Per-row animation state: a focus-scale spring per cell + the horizontal scroll spring. `Copy` so
/// a grid can hold `[CardRow; N]` and copy-init it.
#[derive(Clone, Copy)]
pub(crate) struct CardRow {
    scale: [Spring; MAX_ROW_ITEMS],
    scroll_x: Spring,
    pub base_y: f32,
}
impl CardRow {
    pub(crate) const fn new() -> Self {
        CardRow { scale: [Spring::at(1.0); MAX_ROW_ITEMS], scroll_x: Spring::at(0.0), base_y: 0.0 }
    }
    /// Step every cell's scale spring (all `MAX_ROW_ITEMS` every frame — invariant #10) toward
    /// `focus_scale` for the focused cell else 1.0; then, ONLY when this row is focused, glide the
    /// scroll spring to keep the focused cell at the 2nd slot. `focused == None` freezes the scroll
    /// (a non-focused row holds its position — exactly the home shelf's behavior).
    pub(crate) fn update(&mut self, n: usize, focused: Option<usize>, sty: &RowStyle, dt: f32) {
        for (i, sp) in self.scale.iter_mut().enumerate() {
            let target = if focused == Some(i) { sty.focus_scale } else { 1.0 };
            sp.step(target, sty.k_scale, dt);
        }
        if let Some(fc) = focused {
            if n > 0 {
                let max_sx = (n as f32 * (sty.w + sty.gap) - sty.gap - (SCR_W - 2.0 * sty.margin_x)).max(0.0);
                let want = (fc as f32 * (sty.w + sty.gap) - (sty.w + sty.gap)).clamp(0.0, max_sx);
                self.scroll_x.step(want, sty.k_scroll, dt);
            }
        }
    }
    #[inline]
    pub(crate) fn scale(&self, i: usize) -> f32 {
        self.scale[i.min(MAX_ROW_ITEMS - 1)].pos
    }
    #[inline]
    pub(crate) fn scroll_x(&self) -> f32 {
        self.scroll_x.pos
    }
}

/// A non-focused cell body: the art tile + an optional resume bar. `rect` is the caller's
/// already-scaled rect; `s` scales the corner radius. (The home grid's non-focused cell, verbatim.)
pub(crate) fn draw_tile(p: Painter, art: Art, rect: Rect, s: f32, sty: &RowStyle, resume: Option<f32>) {
    card(p, rect, art, sty.tile_radius(rect, s), false, 1.0, None);
    if let Some(frac) = resume {
        resume_bar(p, rect, frac);
    }
}

/// The focused cell body — the caller draws this LAST for its z-order: the art tile, the big glow
/// ring, an optional resume bar, then the centered title (skipped when `title` is null). (The home
/// grid's focused-last cell, verbatim.)
pub(crate) fn draw_focused(p: Painter, art: Art, rect: Rect, s: f32, sty: &RowStyle, resume: Option<f32>, title: *const c_char) {
    let rad = sty.tile_radius(rect, s);
    card(p, rect, art, rad, false, 1.0, None);
    p.ring(rect, sty.ring_pad, rad, (s - 1.0) / sty.ring_denom());
    if let Some(frac) = resume {
        resume_bar(p, rect, frac);
    }
    if !title.is_null() {
        p.text(title, rect.cx(), rect.y + rect.h + 12.0, theme::size::LABEL, theme::TEXT_PRIMARY, 1, 1);
    }
}

/// resume bar along a card bottom (Continue Watching); `frac` is the played fraction 0..1.
fn resume_bar(p: Painter, r: Rect, frac: f32) {
    let bh = 5.0f32;
    let (bx, bw) = (r.x + 8.0, r.w - 16.0);
    let by = r.y + r.h - bh - 8.0;
    p.rrect(Rect::new(bx, by, bw, bh), bh * 0.5, bh * 0.5, theme::RAIL_BUFFERED);
    p.rrect(Rect::new(bx, by, bw * frac, bh), bh * 0.5, bh * 0.5, theme::RESUME_FILL);
}
