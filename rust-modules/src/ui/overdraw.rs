//! Draw-class OVERDRAW ledger and draw-class MASK — the attribution instrument for the 88.6% of
//! the frame that is not the glass.
//!
//! `docs/backdrop-blur-profiling.md` established that the whole frame rasterizes **3.65 quads per
//! screen pixel** and that the glass is only 11.4% of it. That number came from the GPU's global
//! `FRAG_QUADS_RAST`, which cannot say *whose* quads they are: the wayland compositor draws into
//! the same counters, and one full-screen composite is a quarter of the budget on its own. This
//! module answers the two questions that follow, and neither of them needs a GPU counter:
//!
//! 1. **What does the APP submit?** [`gate`] sums, per draw class, the screen-visible area of every
//!    quad the app actually issues, on the CPU, exactly. `/tmp/plxnative-overdraw` arms it and it
//!    logs one `OVERDRAW` line a second. This is immune to the two traps that make per-phase HWCNT
//!    cycle counts a bad budget — it is not `glFinish`-serialised and it cannot be billed for
//!    another process's work.
//! 2. **What does a class COST?** `/tmp/plxnative-drawmask=<classes>` refuses every draw of the
//!    named classes, so a whole-frame `frame.ui` HWCNT A/B against the unmasked control prices that
//!    class *as the frame sees it*, un-serialised. `drawmask=all` draws nothing at all and is
//!    therefore the **compositor floor**: whatever `frame.ui` still costs with an empty page is
//!    work this app cannot remove.
//!
//! A masked leg is deliberately a BROKEN PICTURE. This is a measurement knob, not a feature — the
//! mask exists so a control-leg difference can be taken, and every leg it produces except the
//! control is unshippable by construction.
//!
//! Both surfaces are dev-only: with `devtriggers` off, [`gate`] is `false` and [`note_px`] is empty
//! at compile time, so a release binary carries neither the branch nor the counters.

/// What kind of quad a draw call submits. One variant per PRIMITIVE FAMILY rather than per screen
/// element, because that is the granularity `gfx`/`text` can label a draw at without every screen
/// having to declare itself — and it is enough to separate the things that plausibly differ in
/// cost per pixel (a four-tap blur pass, a textured card composite, a flat SDF fill).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    /// `draw_ambient` — the OPAQUE four-corner page wash.
    Ambient = 0,
    /// `draw_grad4` — the translucent four-corner gradient (hero corner scrim).
    Grad = 1,
    /// `draw_rect`/`draw_rrect` and their sheened twins — every SDF fill, focus glow included.
    Rect = 2,
    /// `draw_shadow` — the standalone soft drop-shadow pass (the profile chip; cards fold theirs).
    Shadow = 3,
    /// `draw_tex_carded` — the folded card composite (texture + rim + penumbra), i.e. every art tile.
    Card = 4,
    /// `draw_tex`/`draw_tex_stroked` — any other textured quad (icons, masks, backdrop art).
    Image = 5,
    /// `text::draw_text*` — one quad per cached glyph STRING.
    Text = 6,
    /// `fs_glass` — the backdrop panel composite.
    Glass = 7,
    /// The blur chain's own FBO passes, measured in TARGET pixels (they do not touch the panel).
    Blur = 8,
}

pub(crate) const NCLASS: usize = 9;

/// The name each class answers to in `/tmp/plxnative-drawmask`, in [`Class`] order.
pub(crate) const NAMES: [&str; NCLASS] =
    ["ambient", "grad", "rect", "shadow", "card", "image", "text", "glass", "blur"];

/// The dev arm: the real ledger and the real mask. Its release twin sits below, same names, same
/// shape, doing nothing — and the `pub(crate) use` under it re-exports whichever one is compiled.
#[cfg(feature = "devtriggers")]
mod imp {
    use super::{Class, NAMES, NCLASS};
    use crate::log;
    use crate::surface::{LOGICAL_H as SCR_H, LOGICAL_W as SCR_W};
    use std::cell::Cell;

    thread_local! {
        /// Bit per class: refuse every draw of that class. Zero = the shipped picture.
        static MASK: Cell<u32> = const { Cell::new(0) };
        /// Is the ledger armed? Separate from the mask so a masked leg can also be counted.
        static ON: Cell<bool> = const { Cell::new(false) };
        /// `Painter::clip`'s live box in AUTHORED coordinates, or `None` for the whole canvas.
        static CLIP: Cell<Option<[f32; 4]>> = const { Cell::new(None) };
        /// PER-CLASS cells, not one `Cell<[f64; NCLASS]>`. An array cell has to be read and
        /// written WHOLE — 72 + 36 bytes copied twice for every booked draw — and the DIRECTION of
        /// that cost is what makes it a measurement problem rather than a micro-optimisation: a
        /// `drawmask` leg books fewer draws than its control, so it pays less bookkeeping, IN THE
        /// SAME DIRECTION as the saving it is trying to price. This module's whole claim is that
        /// it prices a class without perturbing it. Eight bytes per draw, and the bias is gone.
        static PX: [Cell<f64>; NCLASS] = const { [const { Cell::new(0.0) }; NCLASS] };
        static DRAWS: [Cell<u32>; NCLASS] = const { [const { Cell::new(0) }; NCLASS] };
        static FRAMES: Cell<u32> = const { Cell::new(0) };
    }

    /// Arm the ledger (`/tmp/plxnative-overdraw`).
    pub(crate) fn set_ledger(on: bool) {
        ON.with(|f| f.set(on));
        if on {
            log("OVERDRAW ledger on: per-class submitted quad area, logged once a second");
        }
    }

    /// Arm the mask from a trigger body: class names separated by anything non-alphanumeric, or
    /// `all`. An unknown name is reported rather than silently ignored — a leg that measured the
    /// control twice reads exactly like "this class costs nothing".
    pub(crate) fn set_mask(spec: &str) {
        let mut mask = 0u32;
        let mut bad: Vec<&str> = Vec::new();
        for word in spec.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()) {
            if word.eq_ignore_ascii_case("all") {
                mask = (1 << NCLASS) - 1;
            } else if word.eq_ignore_ascii_case(NAMES[Class::Blur as usize]) {
                log("OVERDRAW drawmask: \"blur\" is accounting-only; mask \"glass\" to remove the whole chain");
            } else if let Some(i) = NAMES.iter().position(|n| n.eq_ignore_ascii_case(word)) {
                mask |= 1 << i;
            } else {
                bad.push(word);
            }
        }
        if !bad.is_empty() {
            log(&format!(
                "OVERDRAW drawmask: unknown class {:?}; valid names: {} all",
                bad,
                NAMES.join(" ")
            ));
        }
        MASK.with(|f| f.set(mask));
        let on: Vec<&str> =
            NAMES.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, n)| *n).collect();
        log(&format!("OVERDRAW drawmask=0x{mask:x} skipping: {}", if on.is_empty() { "(nothing)".to_string() } else { on.join(",") }));
    }

    /// Record `Painter::clip`'s live box, so the ledger counts the fragments a scissored quad can
    /// actually produce rather than the quad it declared.
    #[inline]
    pub(crate) fn set_clip(box_: Option<[f32; 4]>) {
        CLIP.with(|f| f.set(box_));
    }

    /// Is this class masked out? The REFUSAL half of [`gate`], with no booking.
    ///
    /// Exists for one caller: a surface whose mask leg has to be decided EARLY (before it does
    /// bookkeeping the leg is meant to remove) but whose ledger entry must be booked LATE, after
    /// every path that returns without drawing. Asking `gate` twice would double-count; asking it
    /// once, early, books quads that never rasterize.
    #[inline]
    pub(crate) fn masked(c: Class) -> bool {
        c != Class::Blur && MASK.with(|f| f.get()) & (1 << (c as usize)) != 0
    }

    /// Should this draw be REFUSED, and if not, book it. One call per primitive, immediately after
    /// that primitive's own `gfx::culled` test so a culled quad is never counted.
    #[inline]
    pub(crate) fn gate(c: Class, x: f32, y: f32, w: f32, h: f32) -> bool {
        let i = c as usize;
        // [`Class::Blur`] is ACCOUNTING-ONLY and books itself through [`note_px`]: its passes write
        // reduced-resolution FBOs, so their authored-coordinate quad is meaningless here, and half
        // a masked chain would leave the panel sampling a stale texture rather than measuring
        // anything. The whole glass — chain included — is masked at `Class::Glass` instead.
        if c == Class::Blur {
            return false;
        }
        if MASK.with(|f| f.get()) & (1 << i) != 0 {
            return true;
        }
        // A blur source pass draws the page a second time into a small target; its quads are not
        // on the panel and would double every class in the ledger.
        if ON.with(|f| f.get()) && !crate::gfx::blur_source_pass() {
            let (mut x0, mut y0, mut x1, mut y1) = (x.max(0.0), y.max(0.0), (x + w).min(SCR_W), (y + h).min(SCR_H));
            if let Some(cl) = CLIP.with(|f| f.get()) {
                x0 = x0.max(cl[0]);
                y0 = y0.max(cl[1]);
                x1 = x1.min(cl[2]);
                y1 = y1.min(cl[3]);
            }
            add(i, ((x1 - x0).max(0.0) * (y1 - y0).max(0.0)) as f64);
        }
        false
    }

    /// Book `px` fragments against `c` directly — for a pass whose quad is not in authored
    /// coordinates at all (the blur chain writes reduced-resolution FBOs).
    #[inline]
    pub(crate) fn note_px(c: Class, px: f64) {
        if ON.with(|f| f.get()) {
            add(c as usize, px);
        }
    }

    #[inline]
    fn add(i: usize, px: f64) {
        PX.with(|f| f[i].set(f[i].get() + px));
        DRAWS.with(|f| f[i].set(f[i].get() + 1));
    }

    /// One PRESENTED frame has ended. Logs a window every `LOG_EVERY` frames and resets.
    pub(crate) fn frame_end() {
        if !ON.with(|f| f.get()) {
            return;
        }
        const LOG_EVERY: u32 = 60;
        let n = FRAMES.with(|f| {
            let n = f.get() + 1;
            f.set(n);
            n
        });
        if n < LOG_EVERY {
            return;
        }
        let px: [f64; NCLASS] = PX.with(|f| std::array::from_fn(|i| f[i].replace(0.0)));
        let draws: [u32; NCLASS] = DRAWS.with(|f| std::array::from_fn(|i| f[i].replace(0)));
        FRAMES.with(|f| f.set(0));
        let frames = n as f64;
        let screen = (SCR_W * SCR_H) as f64;
        // `blur` writes reduced-resolution targets, so it is NOT part of the panel's overdraw
        // ratio; it is reported beside it rather than inside it.
        let panel: f64 = (0..NCLASS).filter(|i| *i != Class::Blur as usize).map(|i| px[i]).sum();
        let per = |i: usize| px[i] / frames;
        let parts: Vec<String> = (0..NCLASS)
            .map(|i| format!("{}={:.0}/{}", NAMES[i], per(i), (draws[i] as f64 / frames + 0.5) as u32))
            .collect();
        log(&format!(
            "OVERDRAW frames={n} panel={:.0}px x{:.2} | {}",
            panel / frames,
            panel / frames / screen,
            parts.join(" ")
        ));
    }
}

/// The release arm: every entry point is present, `#[inline]`, and does nothing — so a release
/// binary carries neither the branch nor the counters, and every call site compiles unchanged.
///
/// A MODULE rather than seven `#[cfg(not(…))]` + `#[inline]` attribute pairs standing loose at
/// file scope, which is the shape this repository has been bitten by twice: a function inserted
/// between such a pair captures the attribute meant for its neighbour, nothing warns, and the only
/// symptom is codegen (`ff.rs`'s `stream_index` lost its `#[inline]` exactly that way). One `cfg`
/// on the module and one re-export cannot be split by an insertion, and the two arms now have the
/// same shape as each other.
#[cfg(not(feature = "devtriggers"))]
mod imp {
    use super::Class;

    #[inline]
    pub(crate) fn gate(_c: Class, _x: f32, _y: f32, _w: f32, _h: f32) -> bool {
        false
    }
    #[inline]
    pub(crate) fn masked(_c: Class) -> bool {
        false
    }
    #[inline]
    pub(crate) fn note_px(_c: Class, _px: f64) {}
    #[inline]
    pub(crate) fn set_clip(_box_: Option<[f32; 4]>) {}
    #[inline]
    pub(crate) fn frame_end() {}
    #[inline]
    pub(crate) fn set_ledger(_on: bool) {}
    #[inline]
    pub(crate) fn set_mask(_spec: &str) {}
}

pub(crate) use imp::{frame_end, gate, masked, note_px, set_clip, set_ledger, set_mask};

#[cfg(test)]
mod tests {
    use super::*;

    /// The mask parser and the ledger's class list are one table; a name that parses to no bit is
    /// the failure that reads as "this class is free".
    #[test]
    fn every_class_has_a_name_and_the_names_are_distinct() {
        assert_eq!(NAMES.len(), NCLASS);
        for (i, n) in NAMES.iter().enumerate() {
            assert!(!n.is_empty(), "class {i} has no name");
            assert_eq!(NAMES.iter().filter(|m| *m == n).count(), 1, "duplicate class name {n}");
        }
        // The enum's discriminants ARE the ledger's indices; a reorder that broke this would
        // silently attribute every card to whatever moved into slot 4.
        assert_eq!(Class::Ambient as usize, 0);
        assert_eq!(Class::Card as usize, 4);
        assert_eq!(Class::Blur as usize, NCLASS - 1);
    }
}
