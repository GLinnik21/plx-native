//! **Synthetic grounds for judging a glass material — the test-pattern backdrop.**
//!
//! Every material question this app has (how dense the tab track should be, where its polarity
//! crosses, what the blur does to detail, whether the rim survives) is a question about the bar
//! OVER A GROUND. Until now the ground was whatever poster the hero happened to be showing, which
//! made the answers unrepeatable in three separate ways: the hero advances on its own clock, two
//! simulator instances launched together drift apart within seconds, and a comparison assembled by
//! matching log lines can silently pair two different pictures. All three bit this work.
//!
//! So the ground becomes an input. `plxnative-testpat=<spec>` at boot, or the `pat:<spec>` remote
//! token live, replaces the page's picture with a pattern we choose:
//!
//! | spec | what it is | what it answers |
//! |---|---|---|
//! | `flat:<L*>` | a neutral at that CIE lightness | the density solve and the polarity, one rung at a time |
//! | `ramp` | black → white across the screen | where the material changes as the ground brightens |
//! | `edge` | L\* 10 on the left, L\* 90 on the right | a bar spanning both — the case a 5-tap average cannot see |
//! | `checker:<px>` | a checkerboard | what the blur does to detail, and whether it aliases |
//! | `lines:<px>` | vertical bars | the same at the frequency text actually has |
//! | `hue[:L*]` | six saturated bands | whether the material carries colour or washes it out |
//! | `rainbow[:L*]` | a continuous hue sweep | what a colourful poster does to the bar |
//! | `solid:<deg>[:L*]` | ONE hue, whole screen | whether the solve is even-handed across hues |
//!
//! `solid` is to hue what `flat` is to lightness, and for the same reason: the bar averages five
//! taps across its own width, so a ground that varies UNDER it collapses to one number and the
//! sweep answers nothing. Stepping `solid:0:60`, `solid:30:60`, … holds everything still except the
//! one variable.
//!
//! `flat` is the one that makes a SNAPSHOT possible: drive the ladder with `pat:flat:10`,
//! `pat:flat:20`, … and one launch produces a whole graded set, on grounds that are identical
//! between runs and between packages. The comparison stops being a matching problem.
//!
//! **The pattern is drawn as PAGE CONTENT** — after the hero and the grid, before the chrome — so
//! it is exactly what `gfx::sample_ground` reads and exactly what the backdrop blur sources. It
//! rides the page's own transition alpha, so a route change still dips it like anything else.
use crate::ui::{Painter, Rect};
use std::ptr::{addr_of, addr_of_mut};

/// The armed pattern, or `None` for the ordinary page.
static mut CURRENT: Option<Pattern> = None;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Pattern {
    /// A neutral at a CIE L\*, which is the axis a person judges lightness on and the one every
    /// measurement in this material's notes is quoted in.
    Flat(f32),
    Ramp,
    Edge,
    Checker(f32),
    Lines(f32),
    /// Six saturated bands, optionally normalised to one lightness.
    Hue(Option<f32>),
    /// A continuous hue sweep, optionally normalised to one lightness.
    Rainbow(Option<f32>),
    /// One hue (degrees) filling the screen, optionally normalised to a lightness.
    Solid(f32, Option<f32>),
}

/// CIE L\* of an opaque colour — the same function every contrast note in this material quotes.
fn lstar_of(c: [f32; 4]) -> f32 {
    fn lin(v: f32) -> f32 {
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    }
    let y = 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
    if y > 0.008856 { 116.0 * y.cbrt() - 16.0 } else { 903.3 * y }
}

/// Fully saturated sRGB for a hue in 0..1 — the sweep's raw colour, before any lightness is asked of
/// it.
fn hue_rgb(h: f32) -> [f32; 4] {
    let h6 = (h.rem_euclid(1.0)) * 6.0;
    let x = 1.0 - (h6 % 2.0 - 1.0).abs();
    let (r, g, b) = match h6 as i32 {
        0 => (1.0, x, 0.0),
        1 => (x, 1.0, 0.0),
        2 => (0.0, 1.0, x),
        3 => (0.0, x, 1.0),
        4 => (x, 0.0, 1.0),
        _ => (1.0, 0.0, x),
    };
    [r, g, b, 1.0]
}

/// Blend `c` toward white or black until it sits at `target` L\*.
///
/// **Saturation is spent to buy lightness, and that is the physics rather than a defect in the
/// pattern.** A fully saturated blue is L\* 32 at its brightest; asking it for 70 can only be
/// answered by mixing white in, which desaturates it. The alternative — clipping in a wide gamut and
/// mapping back — would put colours on the screen the panel cannot show, which is worse for a test
/// whose whole point is that the ground is exactly what it says.
fn to_lstar(c: [f32; 4], target: f32) -> [f32; 4] {
    let toward = if lstar_of(c) < target { 1.0 } else { 0.0 };
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..24 {
        let m = 0.5 * (lo + hi);
        let mixed = [
            c[0] + (toward - c[0]) * m,
            c[1] + (toward - c[1]) * m,
            c[2] + (toward - c[2]) * m,
            1.0,
        ];
        if (lstar_of(mixed) < target) == (toward > 0.5) { lo = m } else { hi = m }
    }
    let m = 0.5 * (lo + hi);
    [
        c[0] + (toward - c[0]) * m,
        c[1] + (toward - c[1]) * m,
        c[2] + (toward - c[2]) * m,
        1.0,
    ]
}

/// sRGB for a neutral at CIE L\* — the inverse of `lstar`, so a rung named 40 IS the rung the
/// contrast tables call 40.
fn gray(lstar: f32) -> [f32; 4] {
    let l = lstar.clamp(0.0, 100.0);
    let y = if l > 8.0 { ((l + 16.0) / 116.0).powi(3) } else { l / 903.3 };
    let c = if y <= 0.003_130_8 { 12.92 * y } else { 1.055 * y.powf(1.0 / 2.4) - 0.055 };
    [c, c, c, 1.0]
}

/// Parse a spec. Returns false (and changes nothing) on anything unrecognised, so a typo in a
/// remote token cannot blank the screen.
pub(crate) fn set(spec: &str) -> bool {
    let spec = spec.trim();
    let p = match spec {
        "off" | "" => {
            unsafe { *addr_of_mut!(CURRENT) = None };
            crate::ui::idle::invalidate();
            return true;
        }
        "ramp" => Pattern::Ramp,
        "edge" => Pattern::Edge,
        _ => {
            let (name, arg) = spec.split_once(':').unwrap_or((spec, ""));
            let n: f32 = arg.parse().unwrap_or(f32::NAN);
            let at = if n.is_finite() { Some(n.clamp(0.0, 100.0)) } else { None };
            match name {
                "flat" if n.is_finite() => Pattern::Flat(n),
                "checker" => Pattern::Checker(if n.is_finite() && n >= 2.0 { n } else { 32.0 }),
                "lines" => Pattern::Lines(if n.is_finite() && n >= 1.0 { n } else { 6.0 }),
                // bare `hue` / `rainbow` keep each hue's own natural lightness; with an argument
                // every column is normalised to it, which is the form that isolates HUE from
                // brightness — the only way to ask whether the solve treats them evenly
                "hue" if arg.is_empty() || at.is_some() => Pattern::Hue(at),
                "solid" => {
                    let (deg, rest) = arg.split_once(':').unwrap_or((arg, ""));
                    let deg: f32 = match deg.parse() { Ok(d) => d, Err(_) => return false };
                    let l: f32 = rest.parse().unwrap_or(f32::NAN);
                    Pattern::Solid(deg, l.is_finite().then(|| l.clamp(0.0, 100.0)))
                }
                "rainbow" if arg.is_empty() || at.is_some() => Pattern::Rainbow(at),
                _ => return false,
            }
        }
    };
    unsafe { *addr_of_mut!(CURRENT) = Some(p) };
    // A pattern change is discrete damage: nothing is moving, so without this the present gate
    // would hold the old ground on screen until something else asked for a frame.
    crate::ui::idle::invalidate();
    true
}

/// Armed at boot from `/tmp/plxnative-testpat`. Called once, from the same place every other
/// boot trigger is read.
pub(crate) fn boot() {
    if let Some(v) = crate::dev::read("testpat") {
        if !set(&v) {
            crate::log(&format!("testpat: unrecognised spec {v:?} — ignored"));
        }
    }
    if let Some(p) = unsafe { *addr_of!(CURRENT) } {
        crate::log(&format!("testpat: armed — {}", describe(p)));
    }
}

fn describe(p: Pattern) -> String {
    match p {
        Pattern::Flat(l) => format!("flat L*={l:.0}"),
        Pattern::Ramp => "ramp (black → white, left to right)".into(),
        Pattern::Edge => "edge (L* 10 | L* 90)".into(),
        Pattern::Checker(px) => format!("checker {px:.0}px"),
        Pattern::Lines(px) => format!("lines {px:.0}px"),
        Pattern::Hue(None) => "hue (six saturated bands, natural lightness)".into(),
        Pattern::Hue(Some(l)) => format!("hue (six bands, all at L*={l:.0})"),
        Pattern::Rainbow(None) => "rainbow (hue sweep, natural lightness)".into(),
        Pattern::Rainbow(Some(l)) => format!("rainbow (hue sweep, all at L*={l:.0})"),
        Pattern::Solid(d, None) => format!("solid hue {d:.0}deg, natural lightness"),
        Pattern::Solid(d, Some(l)) => format!("solid hue {d:.0}deg at L*={l:.0}"),
    }
}

/// Draw the armed pattern over the whole canvas. A no-op when nothing is armed, so the call site
/// needs no test of its own.
pub(crate) fn draw(p: Painter) {
    let Some(pat) = (unsafe { *addr_of!(CURRENT) }) else { return };
    let (w, h) = (crate::ui::consts::SCR_W, crate::ui::consts::SCR_H);
    let full = Rect::new(0.0, 0.0, w, h);
    let flat = |p: Painter, r: Rect, c: [f32; 4]| p.rrect(r, 0.0, 0.0, c);
    match pat {
        Pattern::Flat(l) => flat(p, full, gray(l)),
        // Columns rather than one gradient quad: `Painter::rect`'s ramp runs VERTICALLY, and the
        // question here is what the bar does as the ground brightens ALONG it.
        Pattern::Ramp => {
            const N: usize = 120;
            let cw = w / N as f32;
            for i in 0..N {
                let l = 100.0 * (i as f32 + 0.5) / N as f32;
                flat(p, Rect::new(i as f32 * cw, 0.0, cw + 1.0, h), gray(l));
            }
        }
        // The case a five-tap average is blind to: half the bar over near-black, half over
        // near-white, one number describing neither.
        Pattern::Edge => {
            flat(p, Rect::new(0.0, 0.0, w * 0.5, h), gray(10.0));
            flat(p, Rect::new(w * 0.5, 0.0, w * 0.5, h), gray(90.0));
        }
        Pattern::Checker(px) => {
            let (cols, rows) = ((w / px).ceil() as i32, (h / px).ceil() as i32);
            for r in 0..rows {
                for c in 0..cols {
                    if (r + c) % 2 == 0 {
                        continue;
                    }
                    flat(p, Rect::new(c as f32 * px, r as f32 * px, px, px), gray(88.0));
                }
            }
            // the dark half is one quad under it rather than half the checker's worth of draws
        }
        Pattern::Lines(px) => {
            let n = (w / (px * 2.0)).ceil() as i32;
            for i in 0..n {
                flat(p, Rect::new(i as f32 * px * 2.0, 0.0, px, h), gray(88.0));
            }
        }
        Pattern::Hue(at) => {
            const N: usize = 6;
            let bw = w / N as f32;
            for i in 0..N {
                let c = hue_rgb(i as f32 / N as f32);
                let c = at.map(|l| to_lstar(c, l)).unwrap_or(c);
                flat(p, Rect::new(i as f32 * bw, 0.0, bw + 1.0, h), c);
            }
        }
        Pattern::Solid(deg, at) => {
            let c = hue_rgb(deg / 360.0);
            flat(p, full, at.map(|l| to_lstar(c, l)).unwrap_or(c));
        }
        // A continuous sweep rather than bands, because the question is whether the material moves
        // ANYWHERE along the hue circle, and a band can hide a step inside itself.
        Pattern::Rainbow(at) => {
            const N: usize = 120;
            let cw = w / N as f32;
            for i in 0..N {
                let c = hue_rgb((i as f32 + 0.5) / N as f32);
                let c = at.map(|l| to_lstar(c, l)).unwrap_or(c);
                flat(p, Rect::new(i as f32 * cw, 0.0, cw + 1.0, h), c);
            }
        }
    }
}

/// The dark ground a checker/lines pattern is drawn over — one quad, laid first.
pub(crate) fn underlay(p: Painter) {
    match unsafe { *addr_of!(CURRENT) } {
        Some(Pattern::Checker(_)) | Some(Pattern::Lines(_)) => {
            p.rrect(Rect::new(0.0, 0.0, crate::ui::consts::SCR_W, crate::ui::consts::SCR_H), 0.0, 0.0, gray(12.0));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rung named 40 must BE the rung every contrast table in this material's notes calls 40 —
    /// the specs are read against those tables, so a pattern that is off by a few L\* silently
    /// invalidates the comparison it was built to make.
    #[test]
    fn a_flat_rung_lands_on_the_lightness_it_is_named_for() {
        fn lin(c: f32) -> f32 {
            if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        }
        for want in [0.0, 10.0, 31.3, 50.0, 78.3, 95.7, 100.0] {
            let g = gray(want);
            let y = 0.2126 * lin(g[0]) + 0.7152 * lin(g[1]) + 0.0722 * lin(g[2]);
            let got = if y > 0.008856 { 116.0 * y.cbrt() - 16.0 } else { 903.3 * y };
            assert!((got - want).abs() < 0.2, "asked L*={want}, drew L*={got:.2}");
        }
    }

    /// **A normalised sweep must actually BE one lightness.** That is the whole claim the pattern
    /// makes: if the columns drift, a density that moves along the sweep can no longer be read as
    /// the solve reacting to HUE, which is the one thing this ground exists to isolate.
    #[test]
    fn a_normalised_rainbow_holds_one_lightness_across_every_hue() {
        for target in [30.0, 55.0, 75.0] {
            for i in 0..48 {
                let c = to_lstar(hue_rgb(i as f32 / 48.0), target);
                let got = lstar_of(c);
                assert!(
                    (got - target).abs() < 0.5,
                    "hue {i}/48 at L*={target}: drew L*={got:.2}"
                );
            }
        }
    }

    /// A typo must leave the screen alone rather than blank it: these arrive over a world-writable
    /// FIFO, one token at a time, with no way to see what was rejected.
    #[test]
    fn an_unrecognised_spec_changes_nothing() {
        let restore = unsafe { *addr_of!(CURRENT) };
        assert!(set("flat:40"));
        assert!(!set("flat:banana"), "a bad argument is refused");
        assert!(!set("nonsense"), "…and so is a bad name");
        assert_eq!(unsafe { *addr_of!(CURRENT) }, Some(Pattern::Flat(40.0)), "the armed one survives");
        assert!(set("off"));
        assert_eq!(unsafe { *addr_of!(CURRENT) }, None);
        unsafe { *addr_of_mut!(CURRENT) = restore };
    }
}
