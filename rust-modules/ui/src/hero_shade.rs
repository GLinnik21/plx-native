//! **Catalog's hero shade over content rects.**
//!
//! A hero draws copy, a logo and controls straight on a photograph, and the photograph can be
//! anything. The shade is a soft, feathered darkening of exactly the places the content stands: one
//! rounded block per rect a screen is drawing (the logo or typed title, the copy column, the
//! action row, the pager), holding [`theme::shade`]'s darkness inside the
//! content's box and feathering to nothing [`theme::shade::FEATHER`] px beyond it. Away from any
//! content it is exactly zero, and a block that moves takes its shade with it, because the screen
//! hands this builder the rects of the current layout.
//!
//! Catalog draws it as a separate scrim layer. The vertex shader evaluates
//! `gfx::scrim::GroundShade` from the rect uniforms and dithers the field. [`MAX_BLOCKS`] is that shader's array size; a
//! hero that offers more rects than fit has the surplus folded into the last block (its box widened
//! to hold both, the darker peak kept), which no screen in the tree reaches.
//!
//! **A block is as wide as the text drawn in it, not the column it is laid into.** A paragraph
//! wrapped into 943 px that ends at 600 shades to 600 (plus the feather): the shade reads as a
//! darkening around the words, never as a slab of the art. A block of copy takes its width from the
//! measured extent of each line drawn there ([`HeroShade::add_ink`]); the typed title takes its own
//! ([`HeroLogo::painted`](crate::hero_logo::HeroLogo::painted)).
//!
//! **How a screen uses it.** Build a [`HeroShade`] from this frame's content rects, each with the
//! role it plays ([`Role`]) and its own alpha (a block fades with the content it stands behind, not
//! with the hero as a whole), then hand [`HeroShade::scrim`] to `AmbientWash::draw_ground` at the
//! hero's fade. [`HeroShade::at`] is the closed form the legibility contract is graded on and the
//! GPU's field is held to.

use crate::widgets::WashScrim;
use crate::{theme, Rect};
use plx_gfx::gfx::scrim::{GroundShade, ShadeBlock, MAX_BLOCKS};

/// What a block stands behind, which sets how dark it holds ([`theme::shade`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Text and the logo or typed title: [`theme::shade::COPY_A`].
    Copy,
    /// The action row and the pager after it: [`theme::shade::CONTROLS_A`].
    Controls,
}

impl Role {
    /// The darkness this role holds inside its box.
    pub const fn peak(self) -> f32 {
        match self {
            Role::Copy => theme::shade::COPY_A,
            Role::Controls => theme::shade::CONTROLS_A,
        }
    }
}

/// The content rects of one hero frame, with their roles, becoming the ground's shade.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeroShade {
    /// `(content rect, peak alpha)` of each live block.
    blocks: [(Rect, f32); MAX_BLOCKS],
    len: usize,
}

impl Default for HeroShade {
    fn default() -> Self {
        Self::new()
    }
}

impl HeroShade {
    /// No content yet: no shade.
    pub const fn new() -> Self {
        Self { blocks: [(Rect::new(0.0, 0.0, 0.0, 0.0), 0.0); MAX_BLOCKS], len: 0 }
    }

    /// Add the block behind `rect` (the content's own rect in authored pixels, as drawn this
    /// frame), at `role`'s darkness times `alpha`, the content's own fade (0..1). Content that is
    /// not there (an empty rect, or an alpha that is 0) adds nothing. See the module doc for what
    /// becomes of a block past [`MAX_BLOCKS`].
    pub fn add(mut self, rect: Rect, role: Role, alpha: f32) -> Self {
        let peak = role.peak() * alpha.clamp(0.0, 1.0);
        if rect.w <= 0.0 || rect.h <= 0.0 || peak <= 0.0 {
            return self;
        }
        if self.len < MAX_BLOCKS {
            self.blocks[self.len] = (rect, peak);
            self.len += 1;
        } else {
            let (last, last_peak) = &mut self.blocks[MAX_BLOCKS - 1];
            *last = last.union(rect);
            *last_peak = last_peak.max(peak);
        }
        self
    }

    /// Add the block behind copy drawn in the column `col` (the column as the page lays it out: its
    /// top, its height and the room it wraps into), as wide as the copy actually is: `ink` is the
    /// `(left, right)` x-extent of each line drawn there, the measured text extents
    /// ([`TextView::ink_span`](crate::text_view::TextView::ink_span), a measured run's width), never
    /// the column's own width. A column wider than its widest line (a paragraph wrapped into 943 px
    /// that ends at 600) would otherwise darken the art past the last letter and read as a slab, not a
    /// shade around the text. The block spans the widest of the lines, in the same screen coordinates
    /// as `col`, and is held inside the column; with no ink at all (nothing drawn) it adds nothing.
    pub fn add_ink(self, col: Rect, ink: impl IntoIterator<Item = (f32, f32)>, role: Role, alpha: f32) -> Self {
        let span = ink.into_iter().filter(|&(l, r)| r > l).reduce(|(l0, r0), (l, r)| (l0.min(l), r0.max(r)));
        let Some((left, right)) = span else { return self };
        let (left, right) = (left.max(col.x), right.min(col.x + col.w));
        self.add(Rect::new(left, col.y, right - left, col.h), role, alpha)
    }

    /// The live blocks' content rects with their peaks, in the order they were added.
    pub fn blocks(&self) -> &[(Rect, f32)] {
        &self.blocks[..self.len]
    }

    /// The field the ground carries, at full strength, in authored pixels.
    pub fn field(&self) -> GroundShade {
        let blocks: Vec<ShadeBlock> = self.blocks().iter().map(|&(r, peak)| Self::block(r, peak)).collect();
        GroundShade::from_blocks(&blocks)
    }

    /// The block behind one content rect: its box is the rect grown by [`theme::shade::PAD`].
    fn block(r: Rect, peak: f32) -> ShadeBlock {
        let pad = theme::shade::PAD;
        ShadeBlock::new(
            [r.x - pad, r.y - pad, r.w + 2.0 * pad, r.h + 2.0 * pad],
            theme::shade::RADIUS,
            theme::shade::FEATHER,
            peak,
        )
    }

    /// The shade at the authored point `(x, y)` at the hero's fade `strength`: the closed form the
    /// legibility contract is graded on, and the one the GPU's per-vertex field is held to. Pure.
    pub fn at(&self, x: f32, y: f32, strength: f32) -> f32 {
        self.scrim(strength).field.at(x, y)
    }

    /// The shade as [`AmbientWash::draw_ground`](crate::widgets::AmbientWash::draw_ground) carries
    /// it, at the hero's fade `strength` (it scales every block, then they stack). The strength
    /// saturates at the video-bound ceiling ([`crate::landing_hero::PREVIEW_FIELD`]): a bound
    /// preview raises the shade, nothing raises it past that.
    pub fn scrim(&self, strength: f32) -> WashScrim {
        WashScrim {
            ink: theme::scrim(1.0),
            field: self.field().scaled(strength.clamp(0.0, crate::landing_hero::PREVIEW_FIELD)),
        }
    }
}
