//! The renderer's own geometry: the authored-pixel rectangle every draw call is described in, the
//! cover-crop rule that maps a picture onto one, and the uniform zoom a draw is folded through.
//!
//! These lived in `ui/mod.rs` and moved here (module-layers step L5) because `gfx` and `text` draw
//! with them and the `gfx` layer may not name `ui`. `ui` re-exports them (`ui::Rect`, `ui::Crop`,
//! `ui::Zoom`), so every UI caller names what it always did. The inherent impls stay with their
//! types, which is what a crate split needs.
// `ui/mod.rs` blankets its whole tree with this attribute, which is what kept the rect helpers no
// screen happens to call (and `Zoom::map_y`) from being dead code while they lived there.
#![allow(dead_code)]

/// Where a cover crop ([`Rect::cover_uv`]) keeps a picture that does not share its box's aspect.
/// A source WIDER than the box always loses its sides evenly; this decides only how a TALLER one
/// splits its vertical overflow between top and bottom.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Crop {
    /// Even: art whose subject is wherever the artist put it — posters, stills, extras, avatars.
    Centre,
    /// A person's photo. A portrait headshot has the face in its upper third, so an even crop into
    /// a circle keeps the chest and cuts the forehead; this takes a fifth of the overflow off the
    /// top and the rest off the bottom, so the kept window rides high on the photo.
    Headshot,
}

impl Crop {
    /// The fraction of a vertical overflow cut from the TOP; the rest comes off the bottom.
    #[inline]
    pub const fn top_share(self) -> f32 {
        match self {
            Crop::Centre => 0.5,
            Crop::Headshot => 0.2,
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}
impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub const FULL: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1920.0,
        h: 1080.0,
    };
    #[inline]
    pub fn cx(&self) -> f32 {
        self.x + self.w * 0.5
    }
    #[inline]
    pub fn cy(&self) -> f32 {
        self.y + self.h * 0.5
    }
    #[inline]
    pub fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.x && px <= self.x + self.w && py >= self.y && py <= self.y + self.h
    }
    /// scale about center — reproduces the C card pop exactly (cx = x-(w-W)/2).
    #[inline]
    pub fn scaled(&self, s: f32) -> Rect {
        let (w, h) = (self.w * s, self.h * s);
        Rect::new(
            self.x - (w - self.w) * 0.5,
            self.y - (h - self.h) * 0.5,
            w,
            h,
        )
    }
    /// This rect shrunk by `d` on every side (negative grows it).
    #[inline]
    pub fn inset(&self, d: f32) -> Rect {
        Rect::new(self.x + d, self.y + d, self.w - 2.0 * d, self.h - 2.0 * d)
    }
    /// This rect FILLED by a `tw × th` source with its aspect preserved and centred — the
    /// `background-size: cover` rule, and the counterpart of the CONTAIN math
    /// `ui::hero_logo::fit` does (that one keeps a logo INSIDE its column;
    /// this one overflows a picture PAST its frame so the frame is never
    /// letterboxed). Full-bleed artwork needs it because `Painter::tex` maps UV 0..1 across the
    /// rect: a source that is not the frame's aspect is SQUASHED, and an episode still is a video
    /// frame whose aspect we do not control.
    ///
    /// A degenerate source (either dimension ≤ 0 — i.e. the texture has not decoded yet, so
    /// `widgets::resolve_tex_wh` answers 0) returns the frame UNCHANGED, so a caller drawing before
    /// the size is known gets today's stretch rather than a zero-area quad that blanks the backdrop.
    ///
    /// The overflow costs no fill: GL rasterizes only inside the viewport, so the off-panel part of
    /// the quad generates no fragments.
    #[inline]
    pub fn cover(&self, tw: f32, th: f32) -> Rect {
        if tw <= 0.0 || th <= 0.0 {
            return *self;
        }
        let s = (self.w / tw).max(self.h / th);
        let (w, h) = (tw * s, th * s);
        Rect::new(
            self.x + (self.w - w) * 0.5,
            self.y + (self.h - h) * 0.5,
            w,
            h,
        )
    }
    /// The UV window `(u0, v0, su, sv)` of a `tw × th` source that COVERS this rect with its aspect
    /// preserved — [`cover`](Self::cover) expressed as a crop of the texture instead of an overflow
    /// of the quad. That is the form a CLIPPED picture needs: a card's rounded rect or a headshot's
    /// circle is masked by the rect itself, so the only way to keep the frame fully painted without
    /// squashing the source is to sample less of it. `crop` says where the kept window sits.
    ///
    /// Cover and not contain, deliberately: a letterboxed picture inside a circle or a rounded card
    /// leaves empty bands that read as a broken image, and the source is never scaled unevenly
    /// either way. A degenerate source or rect (the texture has not decoded yet) answers
    /// [`crate::gfx::UV_FULL`], the whole texture, exactly as `cover` returns the frame unchanged.
    #[inline]
    pub fn cover_uv(&self, tw: f32, th: f32, crop: Crop) -> [f32; 4] {
        if tw <= 0.0 || th <= 0.0 || self.w <= 0.0 || self.h <= 0.0 {
            return crate::gfx::UV_FULL;
        }
        let (box_a, src_a) = (self.w / self.h, tw / th);
        if src_a > box_a {
            // wider than the box: keep the full height, crop the sides evenly
            let su = box_a / src_a;
            [(1.0 - su) * 0.5, 0.0, su, 1.0]
        } else {
            // taller (or equal — `sv` is then 1 and the window is the identity)
            let sv = src_a / box_a;
            [0.0, (1.0 - sv) * crop.top_share(), 1.0, sv]
        }
    }
    /// The overlap of two rects — the part of `self` that `o` lets through. A miss returns a
    /// ZERO-SIZE rect (never a negative one), so `w > 0` is a clean "any of this is visible?"
    /// test. This is how a scissor-clipped strip records what it actually drew: hit-testing the
    /// clipped rect instead of the laid-out one is what stops an off-screen item staying
    /// clickable at coordinates it no longer occupies.
    #[inline]
    pub fn intersect(&self, o: Rect) -> Rect {
        let (x0, y0) = (self.x.max(o.x), self.y.max(o.y));
        let (x1, y1) = (
            (self.x + self.w).min(o.x + o.w),
            (self.y + self.h).min(o.y + o.h),
        );
        Rect::new(x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0))
    }
    /// The smallest rect holding both — a group's extent from its elements (`ui::geom`).
    #[inline]
    pub fn union(&self, o: Rect) -> Rect {
        let (x0, y0) = (self.x.min(o.x), self.y.min(o.y));
        let (x1, y1) = ((self.x + self.w).max(o.x + o.w), (self.y + self.h).max(o.y + o.h));
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }
}

/// A uniform scale `s` about the fixed point `(ox, oy)` — the `ui::Painter`'s visual zoom, and the
/// ONE implementation of "grow a rect about a point" (`Painter::place` for every primitive,
/// `text::draw_text` for a glyph quad, `ui::text_lift::lifted` for focus geometry). `s == 1.0` is
/// the identity and every method returns its input untouched.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Zoom {
    pub s: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Zoom {
    pub(crate) const NONE: Zoom = Zoom { s: 1.0, ox: 0.0, oy: 0.0 };

    /// `s` about the point at fraction `origin` of `r` (`(0.5, 0.5)` centre, `(0.5, 0.0)` top edge).
    pub(crate) fn about(r: Rect, origin: (f32, f32), s: f32) -> Self {
        Self { s, ox: r.x + r.w * origin.0, oy: r.y + r.h * origin.1 }
    }
    #[inline]
    pub(crate) fn is_none(self) -> bool {
        self.s == 1.0
    }
    #[inline]
    pub(crate) fn map(self, r: Rect) -> Rect {
        if self.is_none() {
            return r;
        }
        Rect::new(
            self.ox + (r.x - self.ox) * self.s,
            self.oy + (r.y - self.oy) * self.s,
            r.w * self.s,
            r.h * self.s,
        )
    }
    /// The vertical half of [`map`](Self::map), for an absolute screen y (a fade band).
    #[inline]
    pub(crate) fn map_y(self, y: f32) -> f32 {
        if self.is_none() { y } else { self.oy + (y - self.oy) * self.s }
    }
}
