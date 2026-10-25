//! `AmbientWash::draw_ground`'s geometry and algebra: the ONE-pass ground (wash, the photograph
//! dissolving over it, the atmospheric ramp over both) must be the layered picture, pixel for pixel
//! up to rounding. All pure — no GL — because a GLSL expression cannot be graded by `make check`;
//! these are the replicas the shaders are held to (`fs_art_wash.frag`, `vs_ambient.vert`).

use super::*;
use crate::consts::{SCR_H, SCR_W};

/// The pixel rows a quad covers under the rasteriser's pixel-centre rule — what the layered path
/// actually painted, which is the thing the one-pass split has to reproduce.
fn rows(y: f32, h: f32) -> std::ops::Range<i32> {
    let first = (y - 0.5).ceil() as i32;
    let end = (y + h - 0.5).ceil() as i32;
    first..end.max(first)
}

/// The hero's art at a dive position, exactly as Home lays it out (`art_rect`: the panel lifted by
/// `snap * (SCR_H - 120)`, covered by a 16:9 picture).
fn hero_art_at(snap: f32) -> Rect {
    Rect::new(0.0, -snap * (SCR_H - 120.0), SCR_W, SCR_H).cover(1280.0, 720.0)
}

/// **The split tiles the wash exactly** — the overlap plus the bands are every pixel of `r` once:
/// no crack for the clear colour to show through, no row painted twice. And the overlap is
/// exactly the rows and columns the layered art quad covered, so the one-pass region neither
/// paints art where the layered path had only wash nor misses a row it had art on.
#[test]
fn the_one_pass_split_tiles_the_wash_and_covers_exactly_the_arts_pixels() {
    let r = Rect::FULL;
    let mut arts: Vec<Rect> = [0.0f32, 0.013, 0.2, 0.4517, 0.73, 0.9, 0.995]
        .iter()
        .map(|&s| hero_art_at(s))
        .collect();
    // A picture narrower than the panel, and one wholly inside it: every band gets exercised.
    arts.push(Rect::new(300.3, 120.7, 900.2, 600.1));
    arts.push(Rect::new(-40.0, 500.49, 800.0, 2000.0));
    for art in arts {
        let (over, bands) = art_wash_split(r, art).expect("the art reaches the panel");
        let mut hits = vec![0u8; (SCR_W as usize) * (SCR_H as usize)];
        let mut paint = |q: Rect| {
            for y in rows(q.y, q.h) {
                for x in rows(q.x, q.w) {
                    hits[y as usize * SCR_W as usize + x as usize] += 1;
                }
            }
        };
        paint(over);
        bands.into_iter().flatten().for_each(&mut paint);
        assert!(hits.iter().all(|&n| n == 1), "art {art:?}: a pixel painted {:?} times",
            hits.iter().find(|&&n| n != 1));
        // …and the one-pass region is exactly where the layered art rasterised on the panel.
        let clip = |v: std::ops::Range<i32>, hi: f32| v.start.max(0)..v.end.min(hi as i32);
        assert_eq!(rows(over.y, over.h), clip(rows(art.y, art.h), SCR_H), "art {art:?}: rows");
        assert_eq!(rows(over.x, over.w), clip(rows(art.x, art.w), SCR_W), "art {art:?}: columns");
        // Every edge on a whole pixel, so neighbours share identical vertices.
        for q in std::iter::once(over).chain(bands.into_iter().flatten()) {
            for v in [q.x, q.y, q.x + q.w, q.y + q.h] {
                assert_eq!(v, v.round(), "art {art:?}: edge {v} is not on a pixel boundary");
            }
        }
    }
    assert!(art_wash_split(r, Rect::new(0.0, -2000.0, SCR_W, 1000.0)).is_none(), "a miss is None");
}

/// The dive lifts the art off the bottom of the panel: from then on the wash below it is a band of
/// its own, and the overlap is the art's visible part — the two the dive draws every frame.
#[test]
fn mid_dive_the_art_keeps_the_top_and_the_wash_alone_has_the_foot() {
    let (over, bands) = art_wash_split(Rect::FULL, hero_art_at(0.5)).unwrap();
    assert_eq!((over.y, over.h), (0.0, 600.0));
    let q = |b: Option<Rect>| b.map(|b| (b.x, b.y, b.w, b.h));
    assert_eq!(bands.map(q), [None, Some((0.0, 600.0, SCR_W, SCR_H - 600.0)), None, None]);
}

/// Home's ramp as the ground carries it is `base_scrim_a` — the curve the two layered `rect`s
/// drew and the legibility table is graded on — at both stops, either side of each, and beyond.
#[test]
fn the_grounds_ramp_is_the_hero_scrim_curve() {
    for hero_a in [0.02f32, 0.35, 1.0] {
        let [y0, knee, mid, foot] = crate::landing_hero::base_scrim_ramp(hero_a);
        let ramp = WashRamp { ink: theme::scrim(1.0), stops: [(y0, 0.0), (knee, mid), (SCR_H, foot)] };
        for y in [0.0f32, 200.0, y0, y0 + 0.4, 500.0, knee - 1.0, knee, knee + 1.0, 900.0, SCR_H] {
            let want = crate::landing_hero::base_scrim_a(y, hero_a);
            assert!((ramp.alpha(y) - want).abs() < 1e-5, "hero_a {hero_a} y {y}: {} vs {want}", ramp.alpha(y));
        }
    }
    // Detail's single stop, written by repeating its foot: linear to the foot, never a divide by 0.
    let ramp = WashRamp { ink: theme::scrim(1.0), stops: [(400.0, 0.0), (SCR_H, 0.6), (SCR_H, 0.6)] };
    assert_eq!(ramp.alpha(400.0), 0.0);
    assert!((ramp.alpha(740.0) - 0.3).abs() < 1e-6);
    assert!((ramp.alpha(SCR_H) - 0.6).abs() < 1e-6);
}

/// **The ramp is carried per VERTEX, so it must be one straight segment inside every strip.** The
/// strips `cut_at` makes tile their rect, cut on whole rows at the knees, and within each the
/// ramp at every pixel centre is the linear interpolation of its two edge values — which is what
/// the vertex shader's `mix(u_inka.x, u_inka.y, a_pos.y)` interpolates to.
#[test]
fn the_ramp_is_straight_inside_every_strip_the_ground_draws() {
    let [y0, knee, mid, foot] = crate::landing_hero::base_scrim_ramp(1.0);
    let ramp = WashRamp { ink: theme::scrim(1.0), stops: [(y0, 0.0), (knee, mid), (SCR_H, foot)] };
    for band in [Rect::FULL, Rect::new(0.0, 600.0, SCR_W, 480.0), Rect::new(0.0, 0.0, SCR_W, 300.0)] {
        let strips: Vec<Rect> = cut_at(band, [Some(y0), Some(knee)]).collect();
        let covered: f32 = strips.iter().map(|s| s.h).sum();
        assert_eq!(covered, band.h, "{band:?}: strips {strips:?} must tile it");
        for s in &strips {
            let (a0, a1) = (ramp.alpha(s.y), ramp.alpha(s.y + s.h));
            for y in rows(s.y, s.h) {
                let c = y as f32 + 0.5;
                let lerp = a0 + (a1 - a0) * (c - s.y) / s.h;
                // A knee snapped to its row moves the kink by at most half a pixel.
                assert!((lerp - ramp.alpha(c)).abs() < 2e-3, "{band:?} strip {s:?} y {c}: {lerp} vs {}",
                    ramp.alpha(c));
            }
        }
    }
}

/// `fs_art_wash.frag`'s ONE blend is the layered picture's three: the dithered wash, the art over it
/// at `art.a * tint.a`, and the ramp's ink over both — executed, channel by channel, over a grid.
#[test]
fn one_opaque_fragment_is_the_wash_the_art_and_the_ramp_stacked() {
    let over = |dst: f32, src: f32, a: f32| dst * (1.0 - a) + src * a;
    let mix = |x: f32, y: f32, a: f32| x + (y - x) * a;
    const INK: f32 = 0.04;
    for wash in [0.0f32, 0.23, 1.0] {
        for art in [0.0f32, 0.62, 1.0] {
            for a in [0.0f32, 0.3, 0.97, 1.0] {
                for inka in [0.0f32, 0.18, 0.94] {
                    let layered = over(over(wash, art, a), INK, inka);
                    let one = mix(mix(wash, art, a), INK, inka);
                    assert!((one - layered).abs() < 1e-6, "wash {wash} art {art} a {a} ink {inka}");
                }
            }
        }
    }
}

// ── The flat ground (`AmbientWash::draw_flat_ground`) is its own call; `draw_ground` stays layered ──

/// A ramp with both knees inside the panel, so the layered path cuts the wash into three bands.
fn knee_ramp() -> WashRamp {
    WashRamp { ink: [0.0, 0.0, 0.0, 1.0], stops: [(300.0, 0.0), (700.0, 0.4), (SCR_H, 0.7)] }
}

/// What a recording painter was handed while `f` ran: `(command tag, screen rect)` in draw order.
fn handed(f: impl FnOnce(Painter)) -> Vec<(u64, Rect)> {
    crate::draw_census::capture(|| f(Painter::recording()))
}

/// **`draw_ground` with no photograph is still the layered picture, and never takes the ramp the
/// driver cannot carry.** A ground whose art is pending, held, sliding or scrolled to nothing is a
/// gradient with a photograph about to go over it; folding it to one colour for those frames jumps
/// the page in a frame. So the call is main's: the wash as the four sampled corners, the ramp riding
/// the wash per band only where the ink program exists (`gfx::wash_ink_ok`) and otherwise left to
/// the caller (`false`). Art whose texture is 0 is no art.
#[test]
fn a_ground_with_no_art_is_still_the_layered_picture() {
    let w = wash_on([
        [code8(200), code8(10), code8(90)],
        [code8(20), code8(240), code8(90)],
        [code8(60), code8(130), code8(5)],
        [code8(250), code8(70), code8(180)],
    ]);
    let pending = WashArt {
        tex: 0,
        rect: Rect::FULL.cover(1280.0, 720.0),
        uv: plx_gfx::gfx::UV_FULL,
        tint: [1.0; 4],
    };
    let inked = plx_gfx::gfx::wash_ink_ok();
    let ramp = knee_ramp();
    for art in [None, Some(pending)] {
        // No ramp: one plain wash pass over the rect, and nothing taken.
        let mut took = true;
        assert_eq!(handed(|p| took = w.draw_ground(p, Rect::FULL, art, None)), [(16, Rect::FULL)]);
        assert!(!took, "no ramp to take");
        // A ramp: carried per band (tag 23) only where the driver has the program, else the caller's.
        let mut took = !inked;
        let got = handed(|p| took = w.draw_ground(p, Rect::FULL, art, Some(ramp)));
        let want: Vec<(u64, Rect)> = if inked {
            cut_at(Rect::FULL, [Some(300.0), Some(700.0)]).map(|b| (23, b)).collect()
        } else {
            vec![(16, Rect::FULL)]
        };
        assert_eq!(got, want, "art {:?}", art.map(|a| a.tex));
        assert_eq!(took, inked, "the ramp is taken exactly when the wash can carry it");
    }
}

/// **`draw_flat_ground` is the flat colour, as ONE primitive over the rect** (the renderer clears
/// it), never the per-band split of the layered ground, and it takes no ramp: it has nothing to
/// hand back.
#[test]
fn the_flat_ground_is_one_primitive() {
    let w = wash_on([[code8(31), code8(27), code8(40)]; 4]);
    assert_eq!(handed(|p| w.draw_flat_ground(p, Rect::FULL)), [(16, Rect::FULL)]);
    let part = Rect::new(100.0, 50.0, 1200.0, 700.0);
    assert_eq!(handed(|p| w.draw_flat_ground(p, part)), [(16, part)]);
}

// ── A ground with no photograph is ONE flat colour (`AmbientWash::flat_ground`) ──────────────────

/// A wash resting on `corners` (tl, tr, br, bl), the way `AmbientWash::jump` puts it there.
fn wash_on(corners: [[f32; 3]; 4]) -> AmbientWash {
    let mut w = AmbientWash::flat([0.0; 4]);
    w.jump(corners.map(|c| [c[0], c[1], c[2], 1.0]));
    w
}

/// An 8-bit code as the float the GPU is sent.
fn code8(v: u8) -> f32 {
    v as f32 / 255.0
}

/// A channel sits on an 8-bit code to float precision (the clear is exact only there).
fn on_code(v: f32) -> bool {
    ((v * 255.0) - (v * 255.0).round()).abs() < 1e-4
}

/// **A wash that is already one colour is that colour, to the code** - the page's resting ground
/// and every artless poster's, so a flat clear of it is the shader's picture minus its dither.
/// Off-code springs (a cascade mix, a settling tail) land on the nearest code, never between two.
#[test]
fn a_flat_wash_is_its_own_colour_on_exact_codes() {
    for c in [[code8(31), code8(27), code8(40)], [0.0; 3], [1.0; 3], [0.2001, 0.5004, 0.9996]] {
        let want = c.map(code);
        let w = wash_on([c; 4]);
        assert_eq!(w.flat_ground(), want, "{c:?}");
        assert!(want.into_iter().all(on_code), "{want:?}");
    }
}

/// **A gradient wash is the mean of the field the shader would have drawn**: the 8x8 grid of the
/// bilinear (`sample`) over the panel, which for a bilinear is the mean of its four corners. Checked
/// against the grid itself, on a field that leans a different way in every channel.
#[test]
fn a_gradient_wash_is_the_mean_of_its_eight_by_eight_field() {
    let corners = [
        [code8(200), code8(10), code8(90)],
        [code8(20), code8(240), code8(90)],
        [code8(60), code8(130), code8(5)],
        [code8(250), code8(70), code8(180)],
    ];
    let w = wash_on(corners);
    for r in [Rect::FULL, Rect::new(100.0, 50.0, 1200.0, 700.0)] {
        let mut mean = [0.0f32; 3];
        for j in 0..8 {
            for i in 0..8 {
                let c = w.sample(r, r.x + r.w * (i as f32 + 0.5) / 8.0, r.y + r.h * (j as f32 + 0.5) / 8.0);
                for (m, v) in mean.iter_mut().zip(c) {
                    *m += v / 64.0;
                }
            }
        }
        let got = w.flat_ground();
        for ch in 0..3 {
            assert!(on_code(got[ch]), "channel {ch}: {} is between codes", got[ch]);
            // Rounding to the nearest code moves a channel by at most half of one.
            assert!((got[ch] - mean[ch]).abs() <= 0.5 / 255.0 + 1e-5, "channel {ch}: {} vs mean {}", got[ch], mean[ch]);
        }
    }
}
