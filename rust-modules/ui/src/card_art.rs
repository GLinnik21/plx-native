//! `card_art` — the artwork of a Continue Watching deck card (420x236): where it comes from and, for
//! the one source the app makes itself, how it is made.
//!
//! A deck card draws the best of three artworks, in order (the design record "Continue Watching and
//! episode footer development handoff"): **Plex's own cover** (16:9, the title baked in), then **our
//! composite** (the server's background with its clearLogo baked in), then **the title in type** over
//! the best plain art there is. [`pick`] is that walk, pure over a [`TileFacts`] and an
//! [`ArtProbe`]; [`compose`] is the composite, pure over two decoded bitmaps and a [`CardSpec`].
//!
//! **A logo is never a live widget here.** The composite is rendered ONCE (by the app's poster
//! worker, off the main thread), cached on disk as one image and drawn as an ordinary still: twenty
//! cards must not each hold a 1.1 MB clearLogo texture and a per-frame blend. So everything in this
//! file is a pure function over values: no GL, no worker, no clock, no global registry (the one
//! `tex` peek is behind [`ArtProbe`], which a host test replaces with a table). The Type rung's
//! title is set live, never baked, by [`crate::widgets::still_overlay`] through
//! [`crate::hero_logo::draw_title`] (the text half of `HeroLogo::draw`, built on
//! [`crate::hero_logo::title_text`]) and NEVER through `HeroLogo::draw`, which paints a live logo
//! texture whenever one resolves.
//!
//! **Bounds of the composite.** The logo is sized by the same constant-area rule as the heroes
//! ([`crate::hero_logo::fit`] at [`LogoRung::Card`]), centred, its bottom edge at
//! [`CardSpec::logo_bottom`]. It is filtered in PREMULTIPLIED alpha (a transparent texel's colour is
//! meaningless, and filtering it as if it meant something draws a dark halo round a white mark),
//! box-filtered when it shrinks (a 1200 px wordmark to ~250 px is a 4.7x reduction, where bilinear
//! aliases) and composited source-over. The background is cover-cropped to the card's aspect, never
//! stretched. The output is opaque and exactly the spec's box.
use crate::hero_logo::{fit, LogoRung};
use crate::theme;
use crate::tile::{TileFacts, TileKind};
use crate::widgets::STILL_RES;

/// Owned RGBA8, row-major, `w * h * 4` bytes, STRAIGHT (non-premultiplied) alpha as the decoders
/// deliver it. Shared by every baked image: the collection fan and the card composite.
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Rgba {
    pub fn texel(&self, x: u32, y: u32) -> [f32; 4] {
        let i = ((y * self.w + x) * 4) as usize;
        let p = &self.px[i..i + 4];
        [p[0] as f32, p[1] as f32, p[2] as f32, p[3] as f32]
    }

    /// Store an opaque colour (0–255 per channel, rounded and clamped).
    pub fn put(&mut self, x: u32, y: u32, c: [f32; 3]) {
        let i = ((y * self.w + x) * 4) as usize;
        for k in 0..3 {
            self.px[i + k] = c[k].round().clamp(0.0, 255.0) as u8;
        }
        self.px[i + 3] = 255;
    }

    /// Bilinear sample at a continuous texel coordinate, clamped to the edge.
    pub fn sample(&self, u: f32, v: f32) -> [f32; 4] {
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

    /// Whether `px` really holds a `w` x `h` image with at least one texel — what every read above
    /// assumes, and what a decoder's output is checked against before it is composed.
    fn well_formed(&self) -> bool {
        self.w > 0 && self.h > 0 && self.px.len() as u64 >= self.w as u64 * self.h as u64 * 4
    }
}

/// What one baked card is: the box in output pixels, where the logo's bottom edge sits in it, and
/// the rung that sizes the logo. The logo's size and place are the CARD's (420x236) numbers scaled
/// by `w / 420`, so a bake at render scale 2 is the same picture at twice the pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct CardSpec {
    pub w: u32,
    pub h: u32,
    /// The row (from the top of the box) the logo's bottom edge lands on: the logo covers the rows
    /// above it, never this one.
    pub logo_bottom: u32,
    /// The rung whose bounds size the logo ([`LogoRung::Card`] for a deck card).
    pub rung: LogoRung,
}

impl CardSpec {
    /// The deck card's box at render scale `scale` (1 on the TV): [`STILL_RES`] times the scale,
    /// with the logo [`theme::logo::CARD_BOTTOM_INSET`] above the bottom edge.
    pub fn deck(scale: i32) -> Self {
        let n = scale.max(1) as u32;
        let (w, h) = (STILL_RES.0 as u32 * n, STILL_RES.1 as u32 * n);
        let inset = (theme::logo::CARD_BOTTOM_INSET * n as f32).round() as u32;
        Self { w, h, logo_bottom: h.saturating_sub(inset), rung: LogoRung::Card }
    }

    /// The logo's size in output pixels for a `src_w` x `src_h` source: [`fit`] at the card's own
    /// scale, rounded to whole pixels (at least one), or `None` for a source that has no size.
    pub fn logo_size(&self, src_w: u32, src_h: u32) -> Option<(u32, u32)> {
        let k = self.w as f32 / STILL_RES.0 as f32;
        let (w, h) = fit(self.rung, src_w as f32, src_h as f32, self.w as f32 / k);
        (w > 0.0 && h > 0.0 && k > 0.0).then(|| ((w * k).round().max(1.0) as u32, (h * k).round().max(1.0) as u32))
    }
}

/// A premultiplied pixel: colour 0–255 already multiplied by alpha, then alpha 0–1.
type Px = [f32; 4];

/// The texel at `(x, y)` (clamped into the image) as a premultiplied pixel. `opaque` ignores the
/// source's alpha (a background is a picture, not a cut-out).
fn tap(src: &Rgba, x: i64, y: i64, opaque: bool) -> Px {
    let t = src.texel(x.clamp(0, src.w as i64 - 1) as u32, y.clamp(0, src.h as i64 - 1) as u32);
    let a = if opaque { 1.0 } else { t[3] / 255.0 };
    [t[0] * a, t[1] * a, t[2] * a, a]
}

/// Resample the `crop` (`[x, y, w, h]` in source texels) of `src` to `dw` x `dh` premultiplied
/// pixels. Shrinking on either axis takes the AREA-WEIGHTED mean of every source texel under the
/// output pixel (a box filter, whose weights sum to one, so the mean colour survives); growing
/// takes the bilinear sample the fan uses. Both blend in premultiplied space.
fn resample(src: &Rgba, crop: [f32; 4], dw: u32, dh: u32, opaque: bool) -> Vec<Px> {
    let [cx, cy, cw, ch] = crop;
    let (sx, sy) = (cw / dw as f32, ch / dh as f32);
    let mut out = Vec::with_capacity((dw * dh) as usize);
    let shrinking = sx > 1.0 || sy > 1.0;
    for j in 0..dh {
        for i in 0..dw {
            let (x0, y0) = (cx + i as f32 * sx, cy + j as f32 * sy);
            if shrinking {
                out.push(box_mean(src, x0, y0, x0 + sx, y0 + sy, opaque));
            } else {
                out.push(bilinear(src, x0 + sx * 0.5, y0 + sy * 0.5, opaque));
            }
        }
    }
    out
}

/// The area-weighted mean premultiplied pixel of the source rect `[x0, x1) x [y0, y1)`.
fn box_mean(src: &Rgba, x0: f32, y0: f32, x1: f32, y1: f32, opaque: bool) -> Px {
    let mut acc = [0f32; 4];
    let mut total = 0f32;
    for iy in y0.floor() as i64..y1.ceil() as i64 {
        let wy = (y1.min(iy as f32 + 1.0) - y0.max(iy as f32)).max(0.0);
        for ix in x0.floor() as i64..x1.ceil() as i64 {
            let w = wy * (x1.min(ix as f32 + 1.0) - x0.max(ix as f32)).max(0.0);
            let t = tap(src, ix, iy, opaque);
            for k in 0..4 {
                acc[k] += t[k] * w;
            }
            total += w;
        }
    }
    if total <= 0.0 {
        return tap(src, x0 as i64, y0 as i64, opaque);
    }
    acc.map(|v| v / total)
}

/// [`Rgba::sample`]'s bilinear read of the premultiplied texels, at a continuous coordinate.
fn bilinear(src: &Rgba, u: f32, v: f32, opaque: bool) -> Px {
    let u = (u - 0.5).clamp(0.0, (src.w - 1) as f32);
    let v = (v - 0.5).clamp(0.0, (src.h - 1) as f32);
    let (x0, y0) = (u as i64, v as i64);
    let (fx, fy) = (u - x0 as f32, v - y0 as f32);
    let (a, b) = (tap(src, x0, y0, opaque), tap(src, x0 + 1, y0, opaque));
    let (c, d) = (tap(src, x0, y0 + 1, opaque), tap(src, x0 + 1, y0 + 1, opaque));
    std::array::from_fn(|k| {
        let top = a[k] + (b[k] - a[k]) * fx;
        let bot = c[k] + (d[k] - c[k]) * fx;
        top + (bot - top) * fy
    })
}

/// The largest centred rect of `src_w` x `src_h` with the aspect of `dw` x `dh`, as `[x, y, w, h]`:
/// what "cover" crops to.
fn cover_crop(src_w: f32, src_h: f32, dw: f32, dh: f32) -> [f32; 4] {
    if src_w * dh > src_h * dw {
        let w = src_h * dw / dh;
        [(src_w - w) * 0.5, 0.0, w, src_h]
    } else {
        let h = src_w * dh / dw;
        [0.0, (src_h - h) * 0.5, src_w, h]
    }
}

/// **The composite.** `bg` cover-cropped to the spec's box, `logo` (when there is one) sized by
/// [`CardSpec::logo_size`], centred, its bottom edge on [`CardSpec::logo_bottom`], composited
/// source-over. Opaque, and exactly `spec.w` x `spec.h`. A background that is not a real image is
/// black rather than a panic; a logo that is not one is left off.
pub fn compose(bg: &Rgba, logo: Option<&Rgba>, spec: &CardSpec) -> Rgba {
    let mut out = Rgba { w: spec.w, h: spec.h, px: vec![0; spec.w as usize * spec.h as usize * 4] };
    if spec.w == 0 || spec.h == 0 {
        return out;
    }
    let mut ground: Vec<Px> = if bg.well_formed() {
        let crop = cover_crop(bg.w as f32, bg.h as f32, spec.w as f32, spec.h as f32);
        resample(bg, crop, spec.w, spec.h, true)
    } else {
        vec![[0.0, 0.0, 0.0, 1.0]; spec.w as usize * spec.h as usize]
    };
    if let Some((logo, (lw, lh))) = logo.filter(|l| l.well_formed()).and_then(|l| Some((l, spec.logo_size(l.w, l.h)?))) {
        let mark = resample(logo, [0.0, 0.0, logo.w as f32, logo.h as f32], lw, lh, false);
        let x0 = (spec.w as i64 - lw as i64).div_euclid(2);
        let y0 = spec.logo_bottom as i64 - lh as i64;
        for j in 0..lh as i64 {
            for i in 0..lw as i64 {
                let (x, y) = (x0 + i, y0 + j);
                if x < 0 || y < 0 || x >= spec.w as i64 || y >= spec.h as i64 {
                    continue;
                }
                let m = mark[(j * lw as i64 + i) as usize];
                let g = &mut ground[(y * spec.w as i64 + x) as usize];
                for k in 0..3 {
                    g[k] = m[k] + (1.0 - m[3]) * g[k];
                }
            }
        }
    }
    for y in 0..spec.h {
        for x in 0..spec.w {
            let g = ground[(y * spec.w + x) as usize];
            out.put(x, y, [g[0], g[1], g[2]]);
        }
    }
    out
}

/// Which artwork a deck card draws.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CardRung {
    /// Plex's own cover.
    Cover,
    /// Our composite of the server's background and clearLogo.
    Composite,
    /// Neither exists: plain art in the deck order, with the title in type over it
    /// ([`needs_type_title`]).
    Type,
}

/// What [`pick`] chose: the art key to resolve (empty when the row has no art at all) and the rung it is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CardArt<'a> {
    pub path: &'a str,
    pub rung: CardRung,
}

/// The one question [`pick`] asks of the world: has this art SETTLED as a miss? `false` for
/// everything that can still change (unrequested, in flight), which is what makes a pending rung
/// WAIT instead of dropping to the next one. A trait so a host test answers it from a table.
pub trait ArtProbe {
    fn failed(&self, srv: u16, path: &str) -> bool;
}

/// The live probe: `tex::failed_on` at the deck card's request box (the key
/// `widgets::card_art_request` resolves).
pub struct TexProbe;

impl ArtProbe for TexProbe {
    fn failed(&self, srv: u16, path: &str) -> bool {
        crate::tex::failed_on(srv, path, STILL_RES.0, STILL_RES.1, false)
    }
}

/// **The deck card's art, in order**: the cover, unless it is empty (no catalog id) or has settled
/// as a miss; else the composite, on the same terms; else plain art in the deck order — a movie's
/// background before its poster, an episode's show background, then its still, then the show
/// poster. A rung whose art is merely PENDING is the answer (the card draws it as pending): a
/// lower rung is never shown while a higher one can still arrive.
///
/// `None` unless the facts opted in ([`TileFacts::deck`]); `widgets::still_key` keeps its own chain
/// for everything else.
pub fn pick<'a>(m: &TileFacts<'a>, probe: &dyn ArtProbe) -> Option<CardArt<'a>> {
    let deck = m.deck?;
    let live = |path: &'a str| !path.is_empty() && !probe.failed(m.src, path);
    if live(deck.cover) {
        return Some(CardArt { path: deck.cover, rung: CardRung::Cover });
    }
    if live(deck.card) {
        return Some(CardArt { path: deck.card, rung: CardRung::Composite });
    }
    let order: &[&'a str] = if m.kind == TileKind::Episode { &[m.art, m.still, m.thumb] } else { &[m.art, m.thumb] };
    let path = order.iter().copied().find(|p| !p.is_empty()).unwrap_or("");
    Some(CardArt { path, rung: CardRung::Type })
}

/// Does this card draw its title in type ([`crate::hero_logo::title_text`]) — i.e. is it an
/// opted-in card whose cover and composite are both gone? The covers and composites carry a title
/// of their own.
pub fn needs_type_title(m: &TileFacts<'_>, probe: &dyn ArtProbe) -> bool {
    pick(m, probe).is_some_and(|a| a.rung == CardRung::Type)
}

/// **Does this card's art stand clean, with no bottom scrim?** A deck card's footer line stands BELOW
/// the tile, so the art has no footer text to keep legible and stays clean, unless its title is live
/// type standing on it ([`needs_type_title`]): type over a bare photograph needs the scrim under it,
/// where a cover's or a composite's baked-in title does not. A card that did not opt in keeps its
/// scrim whatever its footer.
pub fn art_is_clean(m: &TileFacts<'_>, probe: &dyn ArtProbe) -> bool {
    m.deck.is_some() && !needs_type_title(m, probe)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tile::DeckArt;
    use crate::widgets::still_key;

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Rgba {
        Rgba { w, h, px: (0..w * h).flat_map(|_| c).collect() }
    }

    fn at(img: &Rgba, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.w + x) * 4) as usize;
        img.px[i..i + 4].try_into().unwrap()
    }

    /// The bounding box `[x0, y0, x1, y1)` of the texels `inside` accepts.
    fn bbox(img: &Rgba, inside: impl Fn([u8; 4]) -> bool) -> [u32; 4] {
        let mut b = [u32::MAX, u32::MAX, 0, 0];
        for y in 0..img.h {
            for x in 0..img.w {
                if inside(at(img, x, y)) {
                    b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
                }
            }
        }
        b
    }

    fn gradient_bg() -> Rgba {
        let (w, h) = (400u32, 300u32);
        let px = (0..w * h).flat_map(|i| [(i % w * 255 / w) as u8, (i / w * 255 / h) as u8, 90, 255]).collect();
        Rgba { w, h, px }
    }

    #[test]
    fn the_output_is_exactly_the_spec_box_and_opaque() {
        let logo = solid(1200, 240, [255, 255, 255, 128]);
        for scale in [1, 2] {
            let spec = CardSpec::deck(scale);
            let out = compose(&gradient_bg(), Some(&logo), &spec);
            assert_eq!((out.w, out.h), (420 * scale as u32, 236 * scale as u32));
            assert_eq!(out.px.len(), (out.w * out.h * 4) as usize);
            assert!(out.px.chunks(4).all(|p| p[3] == 255), "scale {scale}: a translucent output texel");
        }
        let out = compose(&gradient_bg(), None, &CardSpec::deck(1));
        assert_eq!((out.w, out.h, out.px.len()), (420, 236, 420 * 236 * 4));
    }

    #[test]
    fn a_degenerate_input_is_a_black_box_not_a_panic() {
        let spec = CardSpec::deck(1);
        let empty = Rgba { w: 0, h: 0, px: Vec::new() };
        let out = compose(&empty, Some(&empty), &spec);
        assert_eq!((out.w, out.h), (420, 236));
        assert!(out.px.chunks(4).all(|p| p == [0, 0, 0, 255]));
        let short = Rgba { w: 10, h: 10, px: vec![255; 8] };
        assert_eq!(compose(&short, None, &spec).px.len(), 420 * 236 * 4);
    }

    #[test]
    fn the_logo_is_centred_with_its_bottom_edge_on_logo_bottom() {
        let spec = CardSpec::deck(1);
        assert_eq!(spec.logo_bottom, 236 - 48);
        let black = solid(420, 236, [0, 0, 0, 255]);
        for (lw, lh) in [(1200u32, 240u32), (800, 800), (1500, 300)] {
            let out = compose(&black, Some(&solid(lw, lh, [255, 255, 255, 255])), &spec);
            let [x0, y0, x1, y1] = bbox(&out, |p| p[0] > 127);
            let (w, h) = spec.logo_size(lw, lh).unwrap();
            assert_eq!((x1 - x0, y1 - y0), (w, h), "{lw}x{lh}");
            assert_eq!(y1, spec.logo_bottom, "{lw}x{lh}: the bottom edge");
            assert!(x0.abs_diff(420 - x1) <= 1, "{lw}x{lh}: margins {x0} / {}", 420 - x1);
        }
    }

    #[test]
    fn the_logo_scales_with_the_render_scale() {
        let (w1, h1) = CardSpec::deck(1).logo_size(1200, 240).unwrap();
        let (w2, h2) = CardSpec::deck(2).logo_size(1200, 240).unwrap();
        assert!(w2.abs_diff(2 * w1) <= 1 && h2.abs_diff(2 * h1) <= 1, "{w1}x{h1} vs {w2}x{h2}");
        assert_eq!(CardSpec::deck(2).logo_bottom, 2 * CardSpec::deck(1).logo_bottom);
        assert_eq!(CardSpec::deck(0), CardSpec::deck(1), "a render scale below 1 is 1");
    }

    /// The brief's five marks at card scale. The aspects are the brief's own drawn sizes
    /// (232x41, 128x56, 107x56, 253x38, 189x51); the area constant was fitted to them, so each
    /// lands within a pixel and a half and the two tall ones clamp at the 56 px ceiling.
    #[test]
    fn fit_sizes_for_the_five_brief_aspects_at_the_card_rung() {
        for (name, aspect, (bw, bh)) in [
            ("Summer I Turned Pretty", 232.0 / 41.0, (232.0f32, 41.0f32)),
            ("Project Hail Mary", 128.0 / 56.0, (128.0, 56.0)),
            ("Toy Story 4", 107.0 / 56.0, (107.0, 56.0)),
            ("Wild Child", 253.0 / 38.0, (253.0, 38.0)),
            ("Snatch", 189.0 / 51.0, (189.0, 51.0)),
        ] {
            let (w, h) = fit(LogoRung::Card, aspect * 100.0, 100.0, 420.0);
            assert!((w - bw).abs() <= 1.5 && (h - bh).abs() <= 1.5, "{name}: {w}x{h}, the brief has {bw}x{bh}");
            assert!(h <= theme::logo::CARD_H_MAX + 1e-3 && w <= theme::logo::CARD_W_MAX + 1e-3, "{name}");
        }
    }

    /// The failure a straight-alpha filter makes: a white mark beside transparent BLACK texels
    /// averages to grey at the edge, and over a white card that grey shows as a dark fringe. In
    /// premultiplied space the transparent texels carry no colour, so white-on-white stays white.
    #[test]
    fn a_transparent_logo_edge_leaves_no_dark_fringe() {
        let mut logo = solid(600, 120, [0, 0, 0, 0]);
        for y in 0..120 {
            for x in 300..600 {
                let i = ((y * 600 + x) * 4) as usize;
                logo.px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let out = compose(&solid(420, 236, [255, 255, 255, 255]), Some(&logo), &CardSpec::deck(1));
        let darkest = out.px.chunks(4).map(|p| p[0].min(p[1]).min(p[2])).min().unwrap();
        assert!(darkest >= 254, "a fringe of {darkest} at a transparent edge");
    }

    /// 1-px black/white stripes shrunk 5:1 and 4.74:1: a bilinear tap would hit a stripe or a gap
    /// and alias; the box filter keeps every output texel near the mean and the total exact.
    #[test]
    fn the_box_filter_downscale_keeps_the_mean_colour() {
        let (sw, sh) = (1200u32, 240u32);
        let px = (0..sw * sh).flat_map(|i| if (i % sw) % 2 == 0 { [0, 0, 0, 255] } else { [255, 255, 255, 255] }).collect();
        let spec = CardSpec::deck(1);
        let out = compose(&solid(420, 236, [0, 0, 0, 255]), Some(&Rgba { w: sw, h: sh, px }), &spec);
        let (w, h) = spec.logo_size(sw, sh).unwrap();
        let (x0, y0) = ((420 - w) / 2, spec.logo_bottom - h);
        let mut sum = 0f64;
        let mut worst = 0i32;
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                let v = at(&out, x, y)[0] as i32;
                sum += v as f64;
                worst = worst.max((v - 128).abs());
            }
        }
        let mean = sum / (w * h) as f64;
        assert!((mean - 127.5).abs() < 1.5, "mean {mean}");
        assert!(worst <= 40, "an output texel {worst} off the mean: aliasing");
    }

    #[test]
    fn a_4_3_background_is_cover_cropped_not_stretched() {
        // 400x300: the top and bottom 30 rows are red, the rest green. Cover into 16:9 keeps rows
        // 37.6..262.4, so no red survives; a stretch would put a red band at the top and bottom.
        let mut bg = solid(400, 300, [0, 255, 0, 255]);
        for y in (0..30).chain(270..300) {
            for x in 0..400 {
                let i = ((y * 400 + x) * 4) as usize;
                bg.px[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let out = compose(&bg, None, &CardSpec::deck(1));
        assert!(out.px.chunks(4).all(|p| p[0] == 0 && p[1] == 255), "red from the cropped rows leaked in");
        // A wider-than-16:9 source is cropped on the other axis (1000x300 keeps cols 233..767).
        let mut wide = solid(1000, 300, [0, 255, 0, 255]);
        for y in 0..300 {
            for x in (0..200).chain(800..1000) {
                let i = ((y * 1000 + x) * 4) as usize;
                wide.px[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let out = compose(&wide, None, &CardSpec::deck(1));
        assert!(out.px.chunks(4).all(|p| p[0] == 0 && p[1] == 255), "red from the cropped columns leaked in");
    }

    #[test]
    fn a_source_already_the_box_size_comes_through_unchanged() {
        let bg = Rgba { w: 420, h: 236, px: (0..420 * 236).flat_map(|i| [(i % 251) as u8, (i % 97) as u8, 7, 255]).collect() };
        let out = compose(&bg, None, &CardSpec::deck(1));
        assert_eq!(out.px, bg.px);
    }

    // ---- pick ----------------------------------------------------------------------------------

    struct Table(&'static [&'static str]);
    impl ArtProbe for Table {
        fn failed(&self, srv: u16, path: &str) -> bool {
            assert_eq!(srv, 7, "the probe is asked on the facts' own server");
            assert!(!path.is_empty(), "an empty key is never probed");
            self.0.contains(&path)
        }
    }

    fn facts(kind: TileKind, cover: &'static str, card: &'static str) -> TileFacts<'static> {
        TileFacts {
            src: 7,
            kind,
            thumb: "/thumb",
            still: if kind == TileKind::Episode { "/still" } else { "" },
            art: "/art",
            deck: Some(DeckArt { cover, card }),
            ..Default::default()
        }
    }

    fn picked(m: &TileFacts<'static>, failed: &'static [&'static str]) -> (&'static str, CardRung) {
        let a = pick(m, &Table(failed)).expect("opted in");
        (a.path, a.rung)
    }

    #[test]
    fn a_pending_cover_waits_and_a_lower_rung_is_never_shown_early() {
        let m = facts(TileKind::Movie, "/plx/cover/a", "/plx/card/b");
        assert_eq!(picked(&m, &[]), ("/plx/cover/a", CardRung::Cover));
        // The composite settling first changes nothing while the cover can still arrive.
        assert_eq!(picked(&m, &["/plx/card/b"]), ("/plx/cover/a", CardRung::Cover));
    }

    #[test]
    fn a_failed_cover_falls_to_the_composite_and_both_failed_to_type() {
        let m = facts(TileKind::Movie, "/plx/cover/a", "/plx/card/b");
        assert_eq!(picked(&m, &["/plx/cover/a"]), ("/plx/card/b", CardRung::Composite));
        assert_eq!(picked(&m, &["/plx/cover/a", "/plx/card/b"]), ("/art", CardRung::Type));
    }

    #[test]
    fn an_empty_cover_goes_straight_to_the_composite() {
        let m = facts(TileKind::Episode, "", "/plx/card/b");
        assert_eq!(picked(&m, &[]), ("/plx/card/b", CardRung::Composite));
        assert_eq!(picked(&m, &["/plx/card/b"]), ("/art", CardRung::Type));
    }

    #[test]
    fn the_type_rung_follows_the_deck_art_order() {
        let both_failed: &[&str] = &["/c", "/k"];
        let mut movie = facts(TileKind::Movie, "/c", "/k");
        assert_eq!(picked(&movie, both_failed), ("/art", CardRung::Type), "a movie: the background before the poster");
        movie.art = "";
        assert_eq!(picked(&movie, both_failed), ("/thumb", CardRung::Type));
        movie.thumb = "";
        assert_eq!(picked(&movie, both_failed), ("", CardRung::Type), "no art at all is still the Type rung");
        let mut ep = facts(TileKind::Episode, "/c", "/k");
        assert_eq!(picked(&ep, both_failed), ("/art", CardRung::Type), "an episode: the show background first");
        ep.art = "";
        assert_eq!(picked(&ep, both_failed), ("/still", CardRung::Type), "…then its own still");
        ep.still = "";
        assert_eq!(picked(&ep, both_failed), ("/thumb", CardRung::Type), "…then the show poster");
    }

    #[test]
    fn a_card_with_no_keys_at_all_is_still_an_opted_in_card() {
        let m = facts(TileKind::Movie, "", "");
        assert_eq!(picked(&m, &[]), ("/art", CardRung::Type));
        assert!(needs_type_title(&m, &Table(&[])));
    }

    #[test]
    fn only_the_type_rung_needs_a_title_in_type() {
        let m = facts(TileKind::Movie, "/c", "/k");
        assert!(!needs_type_title(&m, &Table(&[])), "a cover names itself");
        assert!(!needs_type_title(&m, &Table(&["/c"])), "so does a composite");
        assert!(needs_type_title(&m, &Table(&["/c", "/k"])));
        let plain = TileFacts { deck: None, ..m };
        assert!(!needs_type_title(&plain, &Table(&["/c", "/k"])), "a surface that did not opt in draws no deck title");
    }

    /// **The art stands clean only where no live title stands on it**: a cover and a composite carry
    /// their title in the picture (no scrim), the Type rung sets it in type (the scrim stays under it),
    /// and a card that did not opt in keeps its scrim.
    #[test]
    fn the_art_keeps_its_scrim_under_a_title_in_type() {
        let m = facts(TileKind::Movie, "/c", "/k");
        assert!(art_is_clean(&m, &Table(&[])), "a cover names itself");
        assert!(art_is_clean(&m, &Table(&["/c"])), "so does a composite");
        assert!(!art_is_clean(&m, &Table(&["/c", "/k"])), "the Type rung draws live type on the art");
        let bare = TileFacts { deck: Some(DeckArt::default()), ..m };
        assert!(!art_is_clean(&bare, &Table(&[])), "no keys at all is the Type rung too");
        let plain = TileFacts { deck: None, ..m };
        assert!(!art_is_clean(&plain, &Table(&[])), "a card that did not opt in keeps its scrim");
    }

    /// `deck == None` is `still_key`'s old chain, exactly: still, then thumb, then art.
    #[test]
    fn a_card_that_did_not_opt_in_keeps_the_old_still_key_chain() {
        let m = TileFacts { still: "/still", thumb: "/thumb", art: "/art", ..Default::default() };
        assert!(pick(&m, &Table(&[])).is_none());
        assert_eq!(still_key(&m), "/still");
        assert_eq!(still_key(&TileFacts { still: "", ..m }), "/thumb");
        assert_eq!(still_key(&TileFacts { still: "", thumb: "", ..m }), "/art");
        assert_eq!(still_key(&TileFacts::default()), "");
        // …and an opted-in card is walked by `pick` instead (no source installed: nothing failed).
        let deck = TileFacts { deck: Some(DeckArt { cover: "/plx/cover/a", card: "/plx/card/b" }), ..m };
        assert_eq!(still_key(&deck), "/plx/cover/a");
        let no_keys = TileFacts { deck: Some(DeckArt::default()), kind: TileKind::Episode, ..m };
        assert_eq!(still_key(&no_keys), "/art", "an opted-in episode prefers the show background to its still");
    }
}
