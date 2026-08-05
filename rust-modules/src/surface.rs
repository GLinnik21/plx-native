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
use std::sync::atomic::{AtomicI32, Ordering};

extern "C" {
    fn SDL_GetWindowSize(win: *mut c_void, w: *mut c_int, h: *mut c_int);
    fn SDL_GL_GetDrawableSize(win: *mut c_void, w: *mut c_int, h: *mut c_int);
}

/// The logical canvas the whole UI is authored in. Not a guess about any device.
pub(crate) const LOGICAL_W: f32 = 1920.0;
pub(crate) const LOGICAL_H: f32 = 1080.0;

/// The real drawable, in pixels. Starts at the logical size so any reader before [`probe`] gets
/// today's behaviour rather than a zero.
static DRAWABLE_W: AtomicI32 = AtomicI32::new(LOGICAL_W as i32);
static DRAWABLE_H: AtomicI32 = AtomicI32::new(LOGICAL_H as i32);

/// The GL drawable size in pixels — what `glViewport` and `glScissor` must be expressed in.
#[inline]
pub(crate) fn drawable() -> (i32, i32) {
    (DRAWABLE_W.load(Ordering::Relaxed), DRAWABLE_H.load(Ordering::Relaxed))
}

/// Logical -> physical scale. **1.0 on every device this has ever run on**, and the fast path
/// stays exactly that: `scale() == 1.0` means every drawn coordinate is already a pixel and the
/// crispness contract holds unchanged.
///
/// Why this is not used to render a 4K interface, even where one would be offered: fill rate. The
/// UI took real work to reach 60 fps at 1080p on this Mali part (the SDF fast path, the backdrop
/// dim-fold, the glyph cache, the card-composite fold), and 3840x2160 is four times the pixels
/// through the same shaders. The compositor's upscale, by contrast, is free and in hardware. A
/// sharper interface at 20 fps is not a better interface.
#[inline]
pub(crate) fn scale() -> f32 {
    DRAWABLE_W.load(Ordering::Relaxed) as f32 / LOGICAL_W
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
