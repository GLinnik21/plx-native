//! What we are actually drawing into — as opposed to what we asked for, or what the panel is.
//!
//! # Three different numbers, and picking the wrong one breaks a working television
//!
//! The UI is authored at a fixed **1920x1080 logical** canvas: every theme token, every layout
//! constant, every font size on the `theme::size` ladder is in those units, and the text/icon
//! crispness contract (`gfx::snap`) assumes 1:1 texels. That canvas is not going to change, and
//! it should not — see [`scale`] for why rendering at panel resolution is the wrong trade.
//!
//! What can change is the **drawable**: the pixel buffer GL renders into. Today it is 1920x1080
//! because that is what `SDL_CreateWindow` asks for and webOS grants, on a 4K panel that composites
//! the result up to 3840x2160 in hardware, for free. That is the right arrangement and this module
//! exists to keep it working, not to replace it.
//!
//! The trap is the third number. **`SDL_webOSGetPanelResolution` reports the PANEL** — 3840x2160
//! on a 2019 4K set whose UI surface is 1080p. It is present on every webOS release, which makes
//! it an inviting answer to "what resolution are we on", and it is the wrong one: sizing the UI
//! from it would render a 4K interface into a 1080p buffer on hardware that has run correctly for
//! the life of this project. It is logged here for diagnosis and used for nothing.
//!
//! # What this actually guards against
//!
//! `glViewport(0, 0, 1920, 1080)` on a drawable that is not 1920x1080 puts the entire interface in
//! the bottom-left corner of the screen. Nothing errors; the app runs; a quarter of the panel has
//! a UI on it. Since the app has never run on webOS 5+, whether a newer compositor grants exactly
//! the surface we request is an assumption — and it is a cheap one to stop making.
use crate::log;
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

extern "C" {
    fn SDL_GetWindowSize(win: *mut c_void, w: *mut c_int, h: *mut c_int);
    fn SDL_GL_GetDrawableSize(win: *mut c_void, w: *mut c_int, h: *mut c_int);
}

/// The logical canvas the whole UI is authored in — **THE definition**, re-exported by
/// `ui::consts` and used under the name `SCR_W`/`SCR_H` by `gfx`, `text` and `app`. It used to be
/// five independent literals that agreed only because they were all typed the same; a module whose
/// whole premise is "do not assume the drawable equals the canvas" should not leave four other
/// answers to what the canvas is. Not a guess about any device.
pub(crate) const LOGICAL_W: f32 = 1920.0;
pub(crate) const LOGICAL_H: f32 = 1080.0;

/// The real drawable, in pixels. Starts at the logical size so any reader before [`probe`] gets
/// today's behaviour rather than a zero.
static DRAWABLE_W: AtomicI32 = AtomicI32::new(LOGICAL_W as i32);
static DRAWABLE_H: AtomicI32 = AtomicI32::new(LOGICAL_H as i32);

/// The viewport rect and the scale, COMPUTED ONCE by [`probe`] rather than on every read.
///
/// They derive from `DRAWABLE_*`, which is written exactly once at boot — so recomputing them per
/// call was pure repetition, and not free: the two `round()`s compile to `roundf` LIBM CALLS on
/// this target (ARMv7 has no `vrinta`), and `clip_set` runs 20-40 times in a frame with a shelf
/// on screen. Storing them makes every reader a plain relaxed load.
static VX: AtomicI32 = AtomicI32::new(0);
static VY: AtomicI32 = AtomicI32::new(0);
static VW: AtomicI32 = AtomicI32::new(LOGICAL_W as i32);
static VH: AtomicI32 = AtomicI32::new(LOGICAL_H as i32);
/// `scale()` as f32 bits — `AtomicF32` does not exist.
static SCALE_BITS: AtomicU32 = AtomicU32::new(1.0f32.to_bits());

/// The GL drawable size in pixels. Private: outside this module the interesting things are
/// [`viewport`], [`scale`] and the two pointer transforms.
#[inline]
fn drawable() -> (i32, i32) {
    (DRAWABLE_W.load(Ordering::Relaxed), DRAWABLE_H.load(Ordering::Relaxed))
}

/// **Uniform** logical -> physical scale: `min(dw/1920, dh/1080)`.
///
/// 1.0 on every device this has ever run on, and the fast path stays exactly that — `scale() == 1.0`
/// means every drawn coordinate is already a pixel and the crispness contract holds unchanged.
///
/// It is `min`, not `dw/1920`, because the alternative is a stretched interface. Stretching to fill
/// only looks harmless while every drawable is 16:9 — which is true of televisions, and is exactly
/// the assumption this module exists to stop making. A non-16:9 surface (signage, or a compositor
/// that applied `appRotation`) would give circles as ellipses and, worse, would silently disagree
/// with `glScissor`, which has one scale factor to work with and would clip the wrong band.
///
/// Uniform scaling plus [`viewport`]'s centring means the canvas letterboxes instead. The bars are
/// empty, which is honest; a distorted UI is not.
///
/// Why this is not used to render a 4K interface, even where one would be offered: fill rate. The
/// UI took real work to reach 60 fps at 1080p on this Mali part (the SDF fast path, the backdrop
/// dim-fold, the glyph cache, the card-composite fold), and 3840x2160 is four times the pixels
/// through the same shaders. The compositor's upscale, by contrast, is free and in hardware. A
/// sharper interface at 20 fps is not a better interface.
#[inline]
pub(crate) fn scale() -> f32 {
    f32::from_bits(SCALE_BITS.load(Ordering::Relaxed))
}

/// The `glViewport` rect: the logical canvas scaled uniformly and **centred** in the drawable.
///
/// Returns `(x, y, w, h)` in physical pixels. On a 1:1 surface this is `(0, 0, 1920, 1080)` and on
/// a 16:9 surface of any size the offsets are zero — so on every television, letterboxing costs
/// nothing and this is a plain scale. The shaders divide by `u_screen`, which stays logical, so
/// this rect is the entire logical->physical mapping; nothing else in the renderer knows the
/// drawable size at all.
#[inline]
pub(crate) fn viewport() -> (i32, i32, i32, i32) {
    (
        VX.load(Ordering::Relaxed),
        VY.load(Ordering::Relaxed),
        VW.load(Ordering::Relaxed),
        VH.load(Ordering::Relaxed),
    )
}

/// Derive [`scale`] and [`viewport`] from the drawable. Called by [`probe`], and by the tests.
fn recompute() {
    let (dw, dh) = drawable();
    let s = (dw as f32 / LOGICAL_W).min(dh as f32 / LOGICAL_H);
    let (w, h) = ((LOGICAL_W * s).round() as i32, (LOGICAL_H * s).round() as i32);
    SCALE_BITS.store(s.to_bits(), Ordering::Relaxed);
    VW.store(w, Ordering::Relaxed);
    VH.store(h, Ordering::Relaxed);
    VX.store((dw - w) / 2, Ordering::Relaxed);
    VY.store((dh - h) / 2, Ordering::Relaxed);
}

/// Physical window pixels -> the authored 1920x1080 canvas: the exact inverse of [`viewport`].
///
/// SDL reports pointer positions in window pixels, and the UI compares them against layout
/// constants in logical units. Those are the same numbers only while the scale is 1.0 — which it
/// is on every television seen so far, and which is precisely the assumption worth not making
/// twice. Without this, a scaled surface would draw the interface correctly and put every touch
/// and Magic-Remote click in the wrong place, which reads as "the pointer is broken" rather than
/// as a resolution problem.
#[inline]
pub(crate) fn to_logical(px: f32, py: f32) -> (f32, f32) {
    let s = scale();
    let (vx, vy, _, _) = viewport();
    ((px - vx as f32) / s, (py - vy as f32) / s)
}

/// The authored canvas -> physical window pixels. The inverse of [`to_logical`], for the one
/// direction that runs the other way: `remote_synth_ptr` builds SDL events in authored coords and
/// pushes them onto SDL's own queue, where the ordinary handler picks them up and converts them
/// back. Without this the synthetic path would be transformed once too often on a scaled surface.
#[inline]
pub(crate) fn to_physical(lx: f32, ly: f32) -> (f32, f32) {
    let s = scale();
    let (vx, vy, _, _) = viewport();
    (lx * s + vx as f32, ly * s + vy as f32)
}

/// Read back what we were actually given, once, after the GL context exists.
///
/// `SDL_GL_GetDrawableSize` is the authority — on a HiDPI-style surface it and `SDL_GetWindowSize`
/// disagree, and it is the former that `glViewport` speaks. Both are logged because the pair is
/// what tells you which situation you are in.
pub(crate) fn probe(win: *mut c_void) {
    unsafe {
        let (mut ww, mut wh) = (0, 0);
        SDL_GetWindowSize(win, &mut ww, &mut wh);
        let (mut dw, mut dh) = (0, 0);
        SDL_GL_GetDrawableSize(win, &mut dw, &mut dh);
        // A fork that does not implement GetDrawableSize can leave these at 0; the window size is
        // the correct fallback, since without HiDPI they are the same thing by definition.
        if dw <= 0 || dh <= 0 {
            dw = ww;
            dh = wh;
        }
        if dw > 0 && dh > 0 {
            DRAWABLE_W.store(dw, Ordering::Relaxed);
            DRAWABLE_H.store(dh, Ordering::Relaxed);
        }
        recompute();

        // The panel, for the log only. See the module doc: this is the number it is tempting and
        // wrong to build on. Resolved through dlsym because webOS releases below 4.4.2 lack it.
        let panel = panel_resolution();
        let p = panel.map_or("unknown".to_string(), |(w, h)| format!("{w}x{h}"));
        log(&format!(
            "surface: window={ww}x{wh} drawable={dw}x{dh} panel={p} logical={}x{} scale={:.3}",
            LOGICAL_W as i32, LOGICAL_H as i32, scale()
        ));
        if (dw, dh) != (LOGICAL_W as i32, LOGICAL_H as i32) {
            log(&format!(
                "surface: drawable is NOT the {}x{} the UI is authored at — scaling the whole \
                 interface by {:.3}. Text will be softer than on a 1:1 surface. Please report this \
                 with your webOS version; the app has only ever been seen on a 1:1 surface.",
                LOGICAL_W as i32, LOGICAL_H as i32, scale()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `DRAWABLE_*` is a crate global, so these serialize. Every case here is unreachable on the
    /// only hardware anyone involved owns — which is the entire reason they exist: this is
    /// resolution-independence arithmetic for surfaces nobody can hold.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_drawable<T>(w: i32, h: i32, f: impl FnOnce() -> T) -> T {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        DRAWABLE_W.store(w, Ordering::Relaxed);
        DRAWABLE_H.store(h, Ordering::Relaxed);
        recompute();
        let r = f();
        DRAWABLE_W.store(LOGICAL_W as i32, Ordering::Relaxed);
        DRAWABLE_H.store(LOGICAL_H as i32, Ordering::Relaxed);
        recompute();
        r
    }

    /// The path every real television takes. Must be bit-for-bit the old hardcoded behaviour:
    /// full-surface viewport, unit scale, pointer coords passed straight through.
    #[test]
    fn a_1080p_surface_is_the_identity() {
        with_drawable(1920, 1080, || {
            assert_eq!(viewport(), (0, 0, 1920, 1080));
            assert_eq!(scale(), 1.0);
            assert_eq!(to_logical(960.0, 540.0), (960.0, 540.0));
            assert_eq!(to_physical(960.0, 540.0), (960.0, 540.0));
        });
    }

    /// 16:9 at any size is a plain proportional scale with NO letterbox — 4K is exactly 2x and
    /// 8K exactly 4x. This is the answer to "would a 4K canvas scale proportionately": yes, and
    /// by an integer, which is also the best case for the glyph raster.
    #[test]
    fn sixteen_by_nine_scales_uniformly_with_no_bars() {
        for (w, h, s) in [(3840, 2160, 2.0), (7680, 4320, 4.0), (2560, 1440, 4.0 / 3.0)] {
            with_drawable(w, h, || {
                assert_eq!(scale(), s, "scale at {w}x{h}");
                assert_eq!(viewport(), (0, 0, w, h), "viewport at {w}x{h}");
                // The canvas centre maps to the surface centre, at every size.
                let (px, py) = to_physical(960.0, 540.0);
                assert!((px - w as f32 / 2.0).abs() < 0.5 && (py - h as f32 / 2.0).abs() < 0.5);
            });
        }
    }

    /// A non-16:9 surface LETTERBOXES rather than stretching. Stretching is what a naive
    /// `dw/1920` scale would do, and it would also silently disagree with `glScissor`, which has
    /// one factor to work with.
    #[test]
    fn an_odd_aspect_letterboxes_instead_of_distorting() {
        // Wider than 16:9 -> pillarboxed: height binds, bars left and right.
        with_drawable(2560, 1080, || {
            assert_eq!(scale(), 1.0);
            assert_eq!(viewport(), (320, 0, 1920, 1080));
        });
        // Taller than 16:9 -> letterboxed: width binds, bars top and bottom.
        with_drawable(1920, 1440, || {
            assert_eq!(scale(), 1.0);
            assert_eq!(viewport(), (0, 180, 1920, 1080));
        });
    }

    /// The two pointer transforms must compose to the identity, letterbox offset included —
    /// otherwise the interface draws correctly and every click lands somewhere else.
    #[test]
    fn pointer_transforms_round_trip() {
        for (w, h) in [(1920, 1080), (3840, 2160), (7680, 4320), (2560, 1080), (1440, 1080)] {
            with_drawable(w, h, || {
                for (lx, ly) in [(0.0, 0.0), (960.0, 540.0), (1919.0, 1079.0), (137.0, 42.0)] {
                    let (px, py) = to_physical(lx, ly);
                    let (bx, by) = to_logical(px, py);
                    assert!((bx - lx).abs() < 0.01 && (by - ly).abs() < 0.01, "{lx},{ly} at {w}x{h}");
                }
            });
        }
    }

    /// A click at the canvas corners must land inside the drawn area on a letterboxed surface —
    /// i.e. inside the viewport rect, never in a bar.
    #[test]
    fn canvas_corners_map_inside_the_viewport() {
        with_drawable(2560, 1080, || {
            let (vx, vy, vw, vh) = viewport();
            for (lx, ly) in [(0.0, 0.0), (1920.0, 1080.0)] {
                let (px, py) = to_physical(lx, ly);
                assert!(px >= vx as f32 && px <= (vx + vw) as f32, "x {px} outside {vx}..{}", vx + vw);
                assert!(py >= vy as f32 && py <= (vy + vh) as f32, "y {py} outside {vy}..{}", vy + vh);
            }
        });
    }
}

/// `SDL_webOSGetPanelResolution`, if this SDL has it. Diagnostic only — never a layout input.
fn panel_resolution() -> Option<(i32, i32)> {
    let h = crate::dynlib::Handle::self_handle();
    let f = h.sym("SDL_webOSGetPanelResolution")?;
    if f.is_null() {
        return None;
    }
    let f: extern "C" fn(*mut c_int, *mut c_int) -> c_int = unsafe { std::mem::transmute(f) };
    let (mut w, mut hgt) = (0, 0);
    if f(&mut w, &mut hgt) == 0 || w <= 0 || hgt <= 0 {
        return None;
    }
    Some((w, hgt))
}
