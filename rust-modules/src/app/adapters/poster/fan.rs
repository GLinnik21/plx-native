//! The collection FAN: what a collection shows when its thumb is the server's generated 2×2
//! composite (`/library/collections/{rk}/composite/{stamp}`). The first three members' posters
//! are fanned — the second rotated left behind, the third rotated right behind, the first centred
//! on top — over a four-corner gradient combined from their `UltraBlurColors` (top-left from the
//! left poster, top-right from the right one, both bottom corners from the front one), with a
//! bottom scrim so the title the card draws LIVE stays legible. The title is never baked.
//!
//! **Rendered once.** The poster worker bakes on the CPU into ONE image, persists it as PNG under
//! a stamp-keyed [`crate::imgcache::classify_baked`] entry, and delivers it as an ordinary
//! decoded poster through the store's normal admission, upload and LRU. The members' pixels live
//! only for the duration of one bake on one worker (three ~240 KB decodes plus the 540 KB output)
//! and never become GL textures. A warm disk hit decodes the baked PNG and fetches nothing.
//!
//! A custom poster (`/library/metadata/{rk}/thumb/…`) is untouched: only a path that
//! [`crate::plex::collections::collection_art`] classifies as a composite is rerouted, at key
//! build time, to the synthetic `/plx/fan/{rk}/{stamp}` key this module parses back. The server's
//! composite is never a fallback: no usable member art is [`FanOutcome::NoArt`].

use crate::plex::collections::{collection_art, CollectionArt};

/// The store key prefix of a baked fan. Not a server path: the worker recognises it before any
/// request is built, and it carries no token by construction.
pub(super) const FAN_PREFIX: &str = "/plx/fan/";
/// Bake size: the portrait card at its largest use, so every consumer samples down.
pub(super) const FAN_W: u32 = 300;
pub(super) const FAN_H: u32 = 450;
/// What each member poster is requested at from the transcoder.
pub(super) const MEMBER_W: i64 = 200;
pub(super) const MEMBER_H: i64 = 300;
/// Members fanned, and so the page size of the children request.
pub(super) const FAN_MEMBERS: usize = 3;
/// The baked-image kind, versioned: change it whenever [`compose`]'s output changes so every
/// persisted fan re-bakes instead of showing the old look until its stamp moves.
pub(super) const FAN_KIND: &str = "fan.1";

/// The store key for a thumb that is a server composite; `None` for every other path.
pub(super) fn fan_key(thumb: &str) -> Option<String> {
    match collection_art(Some(thumb)) {
        CollectionArt::Composite { rk, stamp } => Some(format!("{FAN_PREFIX}{rk}/{stamp}")),
        CollectionArt::Custom(_) | CollectionArt::None => None,
    }
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

/// Where a poster lands, in output pixels; `sin` is the sine of its tilt (positive = clockwise on
/// screen). Tilts are constants, so no transcendental is evaluated here (`ci/allow/libm.txt`).
struct Placement {
    cx: f32,
    cy: f32,
    w: f32,
    sin: f32,
    shade: f32,
}

/// sin(10°): how far the two back posters lean out.
const TILT_SIN: f32 = 0.173_648_18;
const SHADOW_DROP: f32 = 6.0;
const SHADOW_SOFT: f32 = 8.0;
const SHADOW_ALPHA: f32 = 0.45;
/// Where the title scrim starts (fraction of height) and how dark it ends.
const SCRIM_FROM: f32 = 0.55;
const SCRIM_MAX: f32 = 0.7;

/// Composite the fan. `front` is the collection's first member; `left`/`right` the second and
/// third when they exist. Output is exactly [`FAN_W`]×[`FAN_H`], opaque. Everything is composed
/// in place in the one output buffer, so a bake's scratch is that buffer plus the members.
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
    for y in 0..h {
        let fy = y as f32 / (hf - 1.0);
        for x in 0..w {
            let fx = x as f32 / (wf - 1.0);
            let c: [f32; 3] = std::array::from_fn(|k| {
                let top = tl[k] + (tr[k] - tl[k]) * fx;
                let bot = bl[k] + (br[k] - bl[k]) * fx;
                top + (bot - top) * fy
            });
            out.put(x, y, c);
        }
    }
    let tilt = TILT_SIN;
    // Sized and inset so a tilted back poster's outer corner stays ~10 px inside the canvas
    // rather than being cut by it.
    let side = |cx: f32, sin: f32| Placement {
        cx: cx * wf,
        cy: 0.43 * hf,
        w: 0.46 * wf,
        sin,
        shade: 0.82,
    };
    if let Some(m) = left {
        draw(&mut out, &m.poster, &side(0.32, -tilt));
    }
    if let Some(m) = right {
        draw(&mut out, &m.poster, &side(0.68, tilt));
    }
    let centre = Placement {
        cx: 0.5 * wf,
        cy: 0.47 * hf,
        w: 0.6 * wf,
        sin: 0.0,
        shade: 1.0,
    };
    draw(&mut out, &front.poster, &centre);
    let y0 = SCRIM_FROM * hf;
    for y in 0..h {
        let t = ((y as f32 + 0.5 - y0) / (hf - y0)).clamp(0.0, 1.0);
        let keep = 1.0 - SCRIM_MAX * t * t * (3.0 - 2.0 * t);
        if keep < 1.0 {
            for x in 0..w {
                let c = out.texel(x, y);
                out.put(x, y, [c[0] * keep, c[1] * keep, c[2] * keep]);
            }
        }
    }
    out
}

/// A soft drop shadow, then the poster cover-fitted into a 2:3 rectangle, both rotated about the
/// placement centre and anti-aliased by edge coverage.
fn draw(dst: &mut Rgba, src: &Rgba, p: &Placement) {
    if src.w == 0 || src.h == 0 {
        return;
    }
    let (hw, hh) = (p.w / 2.0, p.w * 0.75);
    let (sin, cos) = (p.sin, (1.0 - p.sin * p.sin).sqrt());
    let ex = hw * cos.abs() + hh * sin.abs() + SHADOW_SOFT + 1.0;
    let ey = hw * sin.abs() + hh * cos.abs() + SHADOW_SOFT + SHADOW_DROP + 1.0;
    let x0 = (p.cx - ex).floor().max(0.0) as u32;
    let x1 = ((p.cx + ex).ceil().max(0.0) as u32).min(dst.w);
    let y0 = (p.cy - ey).floor().max(0.0) as u32;
    let y1 = ((p.cy + ey).ceil().max(0.0) as u32).min(dst.h);
    let local = |dx: f32, dy: f32| (dx * cos + dy * sin, -dx * sin + dy * cos);
    let scale = (2.0 * hw / src.w as f32).max(2.0 * hh / src.h as f32);
    for y in y0..y1 {
        for x in x0..x1 {
            let (dx, dy) = (x as f32 + 0.5 - p.cx, y as f32 + 0.5 - p.cy);
            let t = dst.texel(x, y);
            let mut c = [t[0], t[1], t[2]];
            let (sx, sy) = local(dx, dy - SHADOW_DROP);
            let shadow = ((hw + SHADOW_SOFT - sx.abs()) / (2.0 * SHADOW_SOFT)).clamp(0.0, 1.0)
                * ((hh + SHADOW_SOFT - sy.abs()) / (2.0 * SHADOW_SOFT)).clamp(0.0, 1.0);
            if shadow > 0.0 {
                let k = 1.0 - SHADOW_ALPHA * shadow;
                c = c.map(|v| v * k);
            }
            let (lx, ly) = local(dx, dy);
            let cover =
                (hw - lx.abs() + 0.5).clamp(0.0, 1.0) * (hh - ly.abs() + 0.5).clamp(0.0, 1.0);
            if cover > 0.0 {
                let s = src.sample(
                    src.w as f32 / 2.0 + lx / scale,
                    src.h as f32 / 2.0 + ly / scale,
                );
                let a = cover * s[3] / 255.0;
                c = std::array::from_fn(|k| c[k] + (s[k] * p.shade - c[k]) * a);
            }
            dst.put(x, y, c);
        }
    }
}

/// What the children listing answered.
pub(super) enum Members {
    /// Member thumbs in collection order, each with its UltraBlur corners when present.
    Listed(Vec<(String, Option<[[f32; 3]; 4]>)>),
    /// A final answer that there is nothing to show (denied, gone).
    Final,
    /// A failure that can change (transport, 5xx): try the bake again later.
    Transient,
}

/// One member poster's load.
pub(super) enum Art {
    Decoded(Rgba),
    Final,
    Transient,
}

/// The bake's result, which the worker maps onto the store's slot states.
pub(super) enum FanOutcome {
    /// One opaque [`FAN_W`]×[`FAN_H`] image, to be delivered as an ordinary decoded poster.
    Baked(Rgba),
    /// The collection has no usable member art: the consumer draws its neutral tile. Final for
    /// this key (a changed collection has a new stamp and so a new key).
    NoArt,
    /// Retry under the store's transient backoff.
    Transient,
}

/// The bake's I/O, a seam so the orchestration is host-testable without a server or a disk.
pub(super) trait FanIo {
    /// The persisted baked PNG, if any.
    fn cached(&mut self) -> Option<Vec<u8>>;
    /// The persisted entry did not decode; drop it.
    fn discard(&mut self);
    fn members(&mut self) -> Members;
    fn poster(&mut self, thumb: &str) -> Art;
    fn persist(&mut self, png: &[u8]);
}

/// Disk first; otherwise list the first members, load their posters one after another,
/// composite, persist (only a bake no transient failure degraded) and hand the pixels back.
pub(super) fn bake(io: &mut dyn FanIo) -> FanOutcome {
    if let Some(bytes) = io.cached() {
        match crate::img::img_decode_owned(&bytes) {
            Some((w, h, px)) if w == FAN_W && h == FAN_H => {
                return FanOutcome::Baked(Rgba { w, h, px })
            }
            _ => io.discard(),
        }
    }
    let listed = match io.members() {
        Members::Listed(v) => v,
        Members::Final => return FanOutcome::NoArt,
        Members::Transient => return FanOutcome::Transient,
    };
    let mut got: Vec<Member> = Vec::with_capacity(FAN_MEMBERS);
    let mut transient = false;
    for (thumb, blur) in listed
        .into_iter()
        .filter(|(t, _)| !t.is_empty())
        .take(FAN_MEMBERS)
    {
        match io.poster(&thumb) {
            Art::Decoded(poster) if poster.w > 0 && poster.h > 0 => {
                got.push(Member { poster, blur })
            }
            Art::Transient => transient = true,
            Art::Decoded(_) | Art::Final => {}
        }
    }
    if got.is_empty() {
        return if transient {
            FanOutcome::Transient
        } else {
            FanOutcome::NoArt
        };
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
    FanOutcome::Baked(out)
}

#[cfg(test)]
mod tests;
