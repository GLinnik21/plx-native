//! The collection FAN: what a collection shows when its thumb is the server's generated 2×2
//! composite (`/library/collections/{rk}/composite/{stamp}`), laid out as `Collections.dc.html`
//! G1 draws it. The first three members' posters are fanned in the UPPER part of the tile as
//! rounded cards with a rim and a soft shadow — the second rotated left behind, the third rotated
//! right behind, the first upright on top — over a four-corner gradient combined from their
//! `UltraBlurColors` (top-left from the left poster, top-right from the right one, both bottom
//! corners from the front one), with a bottom scrim so the collection's name, which the card sets
//! LIVE in the free lower part (`ui::collection_tile::draw_fan_name`), stays legible. The name is
//! never baked.
//!
//! **Rendered once.** The poster worker bakes on the CPU into ONE image, persists it as PNG under
//! a stamp-keyed [`plx_platform::imgcache::classify_baked`] entry, and delivers it as an ordinary
//! decoded poster through the store's normal admission, upload and LRU. The members' pixels live
//! only for the duration of one bake on one worker (three ~375 KB decodes plus the 540 KB output)
//! and never become GL textures. A warm disk hit decodes the baked PNG and fetches nothing.
//!
//! A custom poster (`/library/metadata/{rk}/thumb/…`) is untouched: only a path that
//! [`crate::plex::collections::composite_parts`] reads as a composite is rerouted, at key
//! build time, to the synthetic `/plx/fan/{rk}/{stamp}` key this module parses back. The server's
//! composite is never a fallback: no usable member art is [`Got::Final`].

use crate::plex::collections::composite_parts;

/// The store key prefix of a baked fan. Not a server path: the worker recognises it before any
/// request is built, and it carries no token by construction.
pub(super) const FAN_PREFIX: &str = "/plx/fan/";
/// Bake size: the portrait card at its largest use, so every consumer samples down.
pub(super) const FAN_W: u32 = 300;
pub(super) const FAN_H: u32 = 450;
/// Members fanned, and so the page size of the children request.
pub(super) const FAN_MEMBERS: usize = 3;
/// The baked-image kind, versioned: change it whenever [`compose`]'s output changes so every
/// persisted fan re-bakes instead of showing the old look until its stamp moves.
pub(super) const FAN_KIND: &str = "fan.3";

/// The store key for a thumb that is a server composite; `None` for every other path.
pub(super) fn fan_key(thumb: &str) -> Option<String> {
    composite_parts(thumb).map(|(rk, stamp)| format!("{FAN_PREFIX}{rk}/{stamp}"))
}

/// `(ratingKey, stamp)` back out of a [`fan_key`].
pub(super) fn parse_fan_key(key: &str) -> Option<(&str, &str)> {
    let (rk, stamp) = key.strip_prefix(FAN_PREFIX)?.split_once('/')?;
    (!rk.is_empty() && !stamp.is_empty() && !stamp.contains('/')).then_some((rk, stamp))
}

/// Owned RGBA, row-major, `w * h * 4` bytes.
pub(super) struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Rgba {
    fn texel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = ((y * self.w + x) * 4) as usize;
        let p = &self.px[i..i + 4];
        [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
    }

    /// Store an opaque colour (0–255 per channel, rounded and clamped).
    fn put(&mut self, x: u32, y: u32, c: [f32; 3]) {
        let i = ((y * self.w + x) * 4) as usize;
        for k in 0..3 {
            self.px[i + k] = c[k].round().clamp(0.0, 255.0) as u8;
        }
        self.px[i + 3] = 255;
    }

    /// Bilinear sample at a continuous texel coordinate, clamped to the edge.
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let u = (u - 0.5).clamp(0.0, (self.w - 1) as f32);
        let v = (v - 0.5).clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (u as u32, v as u32);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (u - x0 as f32, v - y0 as f32);
        let (a, b) = (self.texel(x0, y0), self.texel(x1, y0));
        let (c, d) = (self.texel(x0, y1), self.texel(x1, y1));
        std::array::from_fn(|k| {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bot = c[k] + (d[k] - c[k]) * fx;
            top + (bot - top) * fy
        })
    }

    /// Mean colour (0–255) of the quarter-size patch at ring corner `i` (TL, TR, BR, BL).
    fn corner_mean(&self, i: usize) -> [f32; 3] {
        let (pw, ph) = ((self.w / 4).max(1), (self.h / 4).max(1));
        let x0 = if matches!(i, 1 | 2) { self.w - pw } else { 0 };
        let y0 = if matches!(i, 2 | 3) { self.h - ph } else { 0 };
        let mut sum = [0f64; 3];
        for y in y0..y0 + ph {
            for x in x0..x0 + pw {
                let t = self.texel(x, y);
                for k in 0..3 {
                    sum[k] += t[k] as f64;
                }
            }
        }
        let n = (pw * ph) as f64;
        sum.map(|s| (s / n) as f32)
    }
}

/// One fanned member: its decoded poster and, when the server sent a non-black one, its
/// `UltraBlurColors` in [`crate::plex::models::UltraBlurColors::corners`]' ring order (0–1).
pub(super) struct Member {
    pub poster: Rgba,
    pub blur: Option<[[f32; 3]; 4]>,
}

impl Member {
    /// The colour (0–255) this member contributes at ring corner `i`: its UltraBlur corner, else
    /// the averaged pixels of that corner of its own poster.
    fn corner(&self, i: usize) -> [f32; 3] {
        match self.blur {
            Some(c) => c[i].map(|v| v * 255.0),
            None => self.poster.corner_mean(i),
        }
    }
}

// ── The G1 tile (`Collections.dc.html`, section G), whose CSS is written for a 250×375 tile.
// Every length below is that mock's, in mock pixels, and [`mock_px`] carries it into the bake.

/// The mock tile's width, in mock pixels: the unit every `*_MOCK` length is written in.
const MOCK_TILE_W: f32 = 250.0;

/// A mock length in bake pixels. The bake is the same 2:3 tile at [`FAN_W`] = 300, so ×1.2.
fn mock_px(v: f32) -> f32 {
    v * FAN_W as f32 / MOCK_TILE_W
}

/// `.mc { border-radius:6px }`.
const MEMBER_RADIUS_MOCK: f32 = 6.0;
/// `.mc { box-shadow: 0 6px 14px rgba(0,0,0,.45) }` — offset, blur, alpha.
const SHADOW_DY_MOCK: f32 = 6.0;
const SHADOW_BLUR_MOCK: f32 = 14.0;
const SHADOW_ALPHA: f32 = 0.45;
/// `.mc { box-shadow: …, inset 0 0 0 1px rgba(255,255,255,.18) }` — the rim's width and alpha.
const RIM_W_MOCK: f32 = 1.0;
const RIM_ALPHA: f32 = 0.18;
/// `.scr { height:45%; background:linear-gradient(transparent, rgba(0,0,0,.55)) }`: the scrim
/// starts at 55% of the height and darkens LINEARLY to 0.55 at the bottom edge.
const SCRIM_FROM: f32 = crate::ui::collection_tile::FAN_SCRIM_FROM;
const SCRIM_MAX: f32 = 0.55;

// The deck — where each member lands, its tilt and the deck's scale — is shared with the name's
// placement (`ui::collection_tile::fan_member`), so the name's band is computed from the members
// this bake actually draws.
use crate::ui::collection_tile::{FanMember, FAN_BACK_LEFT as BACK_LEFT, FAN_BACK_RIGHT as BACK_RIGHT,
    FAN_FRONT as FRONT};

/// Composite the fan in the mock's paint order: the UltraBlur ground (`.ubg`), the scrim
/// (`.scr`, UNDER the members), then `.c1`, `.c2`, `.c3`. `front` is the collection's first
/// member; `left`/`right` the second and third when they exist. Output is exactly
/// [`FAN_W`]×[`FAN_H`], opaque. The name the mock sets at the bottom is drawn LIVE by the card
/// (`ui::collection_tile::draw_fan_name`), never baked. Everything is composed in place in the one
/// output buffer, so a bake's scratch is that buffer plus the members.
pub(super) fn compose(front: &Member, left: Option<&Member>, right: Option<&Member>) -> Rgba {
    let (w, h) = (FAN_W, FAN_H);
    let (wf, hf) = (w as f32, h as f32);
    let tl = left.unwrap_or(front).corner(0);
    let tr = right.unwrap_or(front).corner(1);
    let br = front.corner(2);
    let bl = front.corner(3);
    let mut out = Rgba {
        w,
        h,
        px: vec![255u8; (w * h * 4) as usize],
    };
    let y0 = SCRIM_FROM * hf;
    for y in 0..h {
        let fy = y as f32 / (hf - 1.0);
        let t = ((y as f32 + 0.5 - y0) / (hf - y0)).clamp(0.0, 1.0);
        let keep = 1.0 - SCRIM_MAX * t;
        for x in 0..w {
            let fx = x as f32 / (wf - 1.0);
            let c: [f32; 3] = std::array::from_fn(|k| {
                let top = tl[k] + (tr[k] - tl[k]) * fx;
                let bot = bl[k] + (br[k] - bl[k]) * fx;
                (top + (bot - top) * fy) * keep
            });
            out.put(x, y, c);
        }
    }
    if let Some(m) = left {
        draw(&mut out, &m.poster, &BACK_LEFT);
    }
    if let Some(m) = right {
        draw(&mut out, &m.poster, &BACK_RIGHT);
    }
    draw(&mut out, &front.poster, &FRONT);
    out
}

/// Signed distance from `(x, y)` to a box of half-extents `(hw, hh)` centred on the origin whose
/// corners are rounded by `r`: negative inside, positive outside, in pixels.
fn rounded_box_sd(x: f32, y: f32, hw: f32, hh: f32, r: f32) -> f32 {
    let (qx, qy) = (x.abs() - (hw - r), y.abs() - (hh - r));
    let (ox, oy) = (qx.max(0.0), qy.max(0.0));
    (ox * ox + oy * oy).sqrt() + qx.max(qy).min(0.0) - r
}

/// Smoothstep from 0 at `-half` to 1 at `+half`.
fn ramp(v: f32, half: f32) -> f32 {
    let t = ((v + half) / (2.0 * half)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// One `.mc`: its soft drop shadow, then the poster cover-fitted into the rounded 2:3 box with the
/// inset rim over it, all rotated about the box's centre. Every edge is one signed distance to the
/// rounded box in the member's own (rotated) frame, so the corners of the poster, its rim and its
/// shadow are round together; the poster's edge is anti-aliased over one pixel.
fn draw(dst: &mut Rgba, src: &Rgba, p: &FanMember) {
    if src.w == 0 || src.h == 0 {
        return;
    }
    let (wf, hf) = (dst.w as f32, dst.h as f32);
    let placed = crate::ui::collection_tile::fan_member(p, wf, hf);
    let (hw, hh, cx, cy) = (placed.hw, placed.hh, placed.cx, placed.cy);
    let r = mock_px(MEMBER_RADIUS_MOCK);
    let rim = mock_px(RIM_W_MOCK);
    let drop = mock_px(SHADOW_DY_MOCK);
    // CSS blurs a shadow with a Gaussian of σ = blur / 2. A smoothstep across the edge has the
    // Gaussian CDF's slope at the edge when its half-width is σ·0.75·√(2π) ≈ 1.88σ.
    let soft = 1.88 * mock_px(SHADOW_BLUR_MOCK) / 2.0;
    let (sin, cos) = (placed.sin, placed.cosine());
    let ex = hw * cos + hh * sin.abs() + soft + 1.0;
    let ey = hw * sin.abs() + hh * cos + soft + drop + 1.0;
    let x0 = (cx - ex).floor().max(0.0) as u32;
    let x1 = ((cx + ex).ceil().max(0.0) as u32).min(dst.w);
    let y0 = (cy - ey).floor().max(0.0) as u32;
    let y1 = ((cy + ey).ceil().max(0.0) as u32).min(dst.h);
    let local = |dx: f32, dy: f32| (dx * cos + dy * sin, -dx * sin + dy * cos);
    let scale = (2.0 * hw / src.w as f32).max(2.0 * hh / src.h as f32);
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
            let t = dst.texel(x, y);
            let mut c = [t[0], t[1], t[2]];
            // The shadow's box is the member's own, pushed down the page (CSS offsets it before
            // the rotation's frame applies, i.e. in screen space).
            let (sx, sy) = local(dx, dy - drop);
            let shadow = 1.0 - ramp(rounded_box_sd(sx, sy, hw, hh, r), soft);
            if shadow > 0.0 {
                let k = 1.0 - SHADOW_ALPHA * shadow;
                c = c.map(|v| v * k);
            }
            let (lx, ly) = local(dx, dy);
            let d = rounded_box_sd(lx, ly, hw, hh, r);
            let cover = (0.5 - d).clamp(0.0, 1.0);
            if cover > 0.0 {
                let s = src.sample(
                    src.w as f32 / 2.0 + lx / scale,
                    src.h as f32 / 2.0 + ly / scale,
                );
                // The rim: the band within `rim` of the edge, inside it.
                let band = cover - (0.5 - (d + rim)).clamp(0.0, 1.0);
                let lit = RIM_ALPHA * band.max(0.0);
                let a = cover * s[3] / 255.0;
                c = std::array::from_fn(|k| {
                    let px = s[k] + (255.0 - s[k]) * lit;
                    c[k] + (px - c[k]) * a
                });
            }
            dst.put(x, y, c);
        }
    }
}

/// Every answer the bake deals in — the children listing, one member poster's load and the bake
/// itself — is one of three: the value, a final "nothing to show", or a failure that can change.
pub(super) enum Got<T> {
    Ok(T),
    /// A final answer that there is nothing to show (denied, gone, no usable art). For the bake
    /// this is final for its key: a changed collection has a new stamp and so a new key, and the
    /// consumer draws its neutral tile.
    Final,
    /// A failure that can change (transport, 5xx): retry under the store's transient backoff.
    Transient,
}

/// Member thumbs in collection order, each with its UltraBlur corners when present.
pub(super) type Members = Vec<(String, Option<[[f32; 3]; 4]>)>;

/// The bake's I/O, a seam so the orchestration is host-testable without a server or a disk.
pub(super) trait FanIo {
    /// The persisted baked PNG, if any.
    fn cached(&mut self) -> Option<Vec<u8>>;
    /// The persisted entry did not decode; drop it.
    fn discard(&mut self);
    fn members(&mut self) -> Got<Members>;
    fn poster(&mut self, thumb: &str) -> Got<Rgba>;
    fn persist(&mut self, png: &[u8]);
}

/// Disk first; otherwise list the first members, load their posters one after another,
/// composite, persist (only a bake no transient failure degraded) and hand the pixels back. `Ok`
/// is one opaque [`FAN_W`]×[`FAN_H`] image, delivered as an ordinary decoded poster.
pub(super) fn bake(io: &mut dyn FanIo) -> Got<Rgba> {
    if let Some(bytes) = io.cached() {
        match crate::img::img_decode_owned(&bytes) {
            Some((w, h, px)) if w == FAN_W && h == FAN_H => {
                return Got::Ok(Rgba { w, h, px })
            }
            _ => io.discard(),
        }
    }
    let listed = match io.members() {
        Got::Ok(v) => v,
        Got::Final => return Got::Final,
        Got::Transient => return Got::Transient,
    };
    let mut got: Vec<Member> = Vec::with_capacity(FAN_MEMBERS);
    let mut transient = false;
    for (thumb, blur) in listed
        .into_iter()
        .filter(|(t, _)| !t.is_empty())
        .take(FAN_MEMBERS)
    {
        match io.poster(&thumb) {
            Got::Ok(poster) if poster.w > 0 && poster.h > 0 => got.push(Member { poster, blur }),
            Got::Transient => transient = true,
            Got::Ok(_) | Got::Final => {}
        }
    }
    if got.is_empty() {
        return if transient { Got::Transient } else { Got::Final };
    }
    let out = {
        let mut it = got.into_iter();
        let front = it.next().expect("non-empty");
        let (left, right) = (it.next(), it.next());
        compose(&front, left.as_ref(), right.as_ref())
    };
    // A member that failed transiently would freeze a degraded fan on disk until the stamp
    // moves; show it now, but let the next demand bake the whole one.
    if !transient {
        if let Some(png) = crate::img::img_encode_png(out.w, out.h, &out.px) {
            io.persist(&png);
        }
    }
    Got::Ok(out)
}

#[cfg(test)]
mod tests;
