//! The window and its GL context: asking SDL for one (with the one retry a stored size needs), the
//! desktop's window arithmetic, the swap, and the swap interval.
//!
//! [`Window`] is the one SDL implementation of the application's `Surface`. It holds the raw
//! `SDL_Window*` and nothing else: it has no destructor, so it never frees or nulls itself when the
//! compositor takes the screen away (the application is leaked at exit and a worker may still hold
//! the handle), and `init::quit` is explicit. The handle is the single contract with the television's
//! Wayland half, which lives in the webOS platform crate's `system.rs`: that code receives
//! [`Window::raw`] and the order of calls on one thread, and nothing here ever names the Wayland or
//! `SDL_SysWMinfo` calls (the television's SDL writes a larger info struct than the headers declare;
//! `system.rs` has the buffer that survives that, and a pin in `lib.rs` keeps this crate off it).
use crate::sys;
#[cfg(feature = "hostsim")]
use crate::sys::{SDL_GetDisplayUsableBounds, SDL_SetWindowPosition, SDL_SetWindowSize};
use plx_base::eventlog::log;
use plx_base::task::MainThread;
use std::ffi::CStr;
use std::os::raw::{c_int, c_void};

/// OPENGL | FULLSCREEN on the television, which owns the whole panel.
///
/// The desktop asks for OPENGL | ALLOW_HIGHDPI — no fullscreen grab (hostile on a laptop) and
/// **not RESIZABLE**: `surface::probe` reads the drawable once, at boot, so a dragged edge would
/// leave the viewport describing a window that no longer exists, and the interface would sit in a
/// 1920x1080-shaped corner of the new one with every pointer hit landing somewhere else. The window
/// opens at an exact divisor of the canvas instead — see `plan_desktop_window`.
///
/// ALLOW_HIGHDPI is what makes that divisor land on a **1:1 surface** on the Mac people actually
/// have: without it a Retina display gives a drawable equal to the window in POINTS, which the
/// compositor then doubles, so the whole interface is an upscale of a half-size render. With it,
/// `fit_window_to_density` measures the pixels per point once the window exists and sizes
/// it in points for the drawable it should give: 960x540 points for a 1920x1080 drawable on a 2x
/// display, so `surface::scale() == 1.0`, the same 1:1 texel contract the television gets.
pub const SDL_WINDOW_FLAGS: u32 = if cfg!(feature = "hostsim") {
    0x2 | 0x2000
} else {
    0x2 | 0x1
};
/// `SDL_WINDOW_ALLOW_HIGHDPI`, the one bit of [`SDL_WINDOW_FLAGS`] a frame dump clears.
const SDL_WINDOW_ALLOW_HIGHDPI: u32 = 0x2000;
/// `SDL_WINDOW_INPUT_FOCUS`. Note it is NOT among the flags requested above — no window flag can
/// ask for it; SDL sets it when the compositor gives this surface the keyboard. Read, never asked
/// for, and read by exactly one thing: the application's `textinput`, whose panel it silently gates.
pub const SDL_WINDOW_INPUT_FOCUS: u32 = 0x200;

// The window we ASK SDL for. `surface::probe` then reads back what we actually got.
#[cfg(any(feature = "hostsim", test))]
const SCR_W: c_int = plx_base::surface::LOGICAL_W as c_int;
#[cfg(any(feature = "hostsim", test))]
const SCR_H: c_int = plx_base::surface::LOGICAL_H as c_int;

/// Where, and how big, to ask for the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// The television: placed at 0,0 at exactly `size` and takes the panel. `fallback` is the one
    /// size to retry at when the window or its GL context is refused at `size`.
    ///
    /// **A stored size the set will not give must never brick launch.** If the window or its GL
    /// context is refused at a smaller size the owner chose, free whatever was made and retry ONCE
    /// at the native canvas; only a refused native window is fatal.
    Panel { size: (c_int, c_int), fallback: Option<(c_int, c_int)> },
    /// A desktop: centred at whatever fits the display (`plan_desktop_window`), sized for the
    /// display's pixel density once the window exists (`fit_window_to_density`). The request is the
    /// desktop's own size, not a setting, so there is no retry here. Only a simulator build has the
    /// SDL calls that read a display's size.
    #[cfg(feature = "hostsim")]
    Desktop,
}

/// What the application asks for. Plain data: the title the platform chooses, where and how big, and
/// whether the desktop window may be HiDPI.
#[derive(Clone, Copy, Debug)]
pub struct OpenAsk<'a> {
    pub title: &'a CStr,
    pub placement: Placement,
    /// `false` clears `SDL_WINDOW_ALLOW_HIGHDPI`. A frame dump's drawable is exactly the window's
    /// size in points: it is the one desktop window that does not ask for it, so its window is the
    /// 1920x1080 points of the canonical Linux host on a Retina display too, and
    /// `fit_window_to_density` finds nothing to correct. A no-op for the television's flags, which
    /// never carry the bit. (Absent from every shipping build.)
    pub high_dpi: bool,
}

/// SDL refused the window or its GL context at the native size (the reason is in the log lines
/// `CreateWindow failed at WxH` / `GL ctx failed at WxH`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFailed;

/// An open SDL window, current on this thread's GL context. `!Send`/`!Sync` (it is a raw pointer):
/// the swap, the poll and the interval are the SDL main thread's.
#[derive(Debug)]
pub struct Window {
    win: *mut c_void,
}

impl Window {
    /// A window that does not exist: the test rig's and the host shells'. Its swap is
    /// `SDL_GL_SwapWindow(NULL)`, which SDL answers with an error and nothing else, exactly as the
    /// rig's null handle always has.
    pub const DETACHED: Window = Window { win: std::ptr::null_mut() };

    /// Ask SDL for the window and its GL context, correct it for the display's density where the
    /// platform has one to correct, and probe the drawable it actually gave.
    ///
    /// Called after [`crate::init::init`] and [`crate::init::request_gl`], and after the
    /// application has read the stored session, which the placement may depend on: that order is
    /// the application's and is pinned there.
    pub fn open(_mt: &MainThread, ask: &OpenAsk) -> Result<Window, OpenFailed> {
        // The television is placed at 0,0 at exactly canvas size (unless a webOS 1-3 owner picked a
        // smaller Interface resolution) and takes the panel. A desktop window is centred
        // (`SDL_WINDOWPOS_CENTERED`) at whatever fits — see `plan_desktop_window`. The density is not
        // known until a window exists, so this is the 1x answer; `fit_window_to_density` below
        // corrects it on a display where that was wrong.
        #[cfg(feature = "hostsim")]
        let mut desktop_ask: Option<WindowAsk> = None;
        let (wx, wy, ww_req, wh_req, mut fallback) = match ask.placement {
            Placement::Panel { size: (w, h), fallback } => (0, 0, w, h, fallback),
            #[cfg(feature = "hostsim")]
            Placement::Desktop => {
                let planned = desktop_window_ask();
                desktop_ask = Some(planned);
                let (w, h) = plan_desktop_window(planned, Density::UNIT);
                (WINDOWPOS_CENTERED, WINDOWPOS_CENTERED, w, h, None)
            }
        };
        let window_flags = if ask.high_dpi {
            SDL_WINDOW_FLAGS
        } else {
            SDL_WINDOW_FLAGS & !SDL_WINDOW_ALLOW_HIGHDPI
        };
        let (mut ww_try, mut wh_try) = (ww_req, wh_req);
        let win = unsafe {
            loop {
                let win =
                    sys::SDL_CreateWindow(ask.title.as_ptr(), wx, wy, ww_try, wh_try, window_flags);
                if win.is_null() {
                    log(&format!("CreateWindow failed at {ww_try}x{wh_try}"));
                } else {
                    let ctx = sys::SDL_GL_CreateContext(win);
                    if !ctx.is_null() {
                        break win;
                    }
                    log(&format!("GL ctx failed at {ww_try}x{wh_try}"));
                    sys::SDL_DestroyWindow(win);
                }
                let Some((w, h)) = fallback.take() else {
                    return Err(OpenFailed);
                };
                log(&format!("surface: retrying the window at the native {w}x{h}"));
                (ww_try, wh_try) = (w, h);
            }
        };
        // Correct the size for the display's density now: `surface::probe` below reads the drawable once.
        #[cfg(feature = "hostsim")]
        if let Some(desktop_ask) = desktop_ask {
            unsafe { fit_window_to_density(win, desktop_ask) };
        }
        plx_base::surface::probe(win);
        Ok(Window { win })
    }

    /// The raw `SDL_Window*`: the handle the application's `Shell` hands to the television's
    /// Wayland half and to `textinput`. Null for [`Window::DETACHED`].
    pub fn raw(&self) -> *mut c_void {
        self.win
    }

    /// `SDL_GL_SetSwapInterval` for the current context: 1 locks the frame rate to the panel
    /// refresh, 0 uncaps it. The caller computes the value (vsync, `/tmp/plxnative-novsync`, a
    /// frame dump) because those are application triggers.
    pub fn set_swap_interval(&self, interval: i32) {
        unsafe { sys::SDL_GL_SetSwapInterval(interval) };
    }

    /// `SDL_GL_SwapWindow`: present the frame. The compositor commit of the opaque-region request
    /// the application made just before it is this swap's, not a call of its own.
    pub fn swap(&self) {
        unsafe { sys::SDL_GL_SwapWindow(self.win) };
    }
}


/// What a DESKTOP window is asked to be, before the display's pixel density is known.
#[cfg(any(feature = "hostsim", test))]
#[derive(Clone, Copy)]
enum WindowAsk {
    /// `PLXNATIVE_WIN=<w>x<h>`: a DRAWABLE of exactly this many pixels, on any display.
    Drawable(c_int, c_int),
    /// No override: the largest whole fraction of the canvas whose window fits this usable display
    /// area (`SDL_GetDisplayUsableBounds`, in points).
    Fit(c_int, c_int),
}

/// How many drawable pixels a window point is worth on this display, as the integer pair SDL
/// reported for a window that exists: `window_w` points wide, `drawable_w` pixels wide.
///
/// Integers, never a float multiplied back: 960 points of a 2x window must come out as exactly
/// 1920 pixels, and a ratio that is not a whole number (a 150% Windows display) must round the
/// same way on the way out as on the way in.
#[cfg(any(feature = "hostsim", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Density {
    window_w: c_int,
    drawable_w: c_int,
}

#[cfg(any(feature = "hostsim", test))]
impl Density {
    /// One pixel per point: every 1x display, and what is assumed until a window has been measured.
    const UNIT: Density = Density { window_w: 1, drawable_w: 1 };

    /// The measured pair. A drawable that is not larger than the window (a 1x display) is
    /// [`Density::UNIT`]: a drawable never has fewer pixels than points, and a window is never
    /// grown to make up for one that seems to.
    fn measured(window_w: c_int, drawable_w: c_int) -> Density {
        if window_w <= 0 || drawable_w <= window_w {
            Density::UNIT
        } else {
            Density { window_w, drawable_w }
        }
    }

    /// The window size, in points, whose drawable is `px` pixels: the nearest whole number of
    /// points, and one fewer if rounding up would make the drawable LARGER than asked (never
    /// larger than the canvas is the contract; a pixel short is a letterbox, a pixel over is not).
    fn points(self, px: c_int) -> c_int {
        let (w, d) = (i64::from(self.window_w), i64::from(self.drawable_w));
        let pt = ((i64::from(px) * w + d / 2) / d).max(1) as c_int;
        if pt > 1 && self.pixels(pt) > px {
            pt - 1
        } else {
            pt
        }
    }

    /// The drawable, in pixels, of a window `pt` points wide.
    fn pixels(self, pt: c_int) -> c_int {
        let (w, d) = (i64::from(self.window_w), i64::from(self.drawable_w));
        ((i64::from(pt) * d + w / 2) / w) as c_int
    }
}

/// `PLXNATIVE_WIN`'s value: `<w>x<h>`, both positive. Anything else is no override.
#[cfg(any(feature = "hostsim", test))]
fn parse_window_override(v: &str) -> Option<(c_int, c_int)> {
    let (w, h) = v.split_once(['x', 'X'])?;
    match (w.trim().parse::<c_int>(), h.trim().parse::<c_int>()) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    }
}

/// The window a DESKTOP should open at, in points, for a display of this pixel `density`: the
/// pure arithmetic of the size, so the Retina cases are testable without a Retina display.
///
/// **The drawable is what is sized; the window follows from it.** The UI is authored at a 1920x1080
/// canvas and every glyph, icon mask and poster is rasterized for 1:1 texels against it
/// (`gfx::snap`, the crispness contract in `theme.rs`), so a drawable LARGER than the canvas is
/// not a sharper picture: it is the same atlas stretched, with every drawable-sized buffer (the
/// page snapshot, the glass sources) four times the television's, which is how a 2x display once
/// breached the render-set ceiling on every frame (`ui/src/frame/render_set.rs`).
///
/// * `Drawable` is that drawable, exactly. The fit below is chosen for a HUMAN looking at a
///   window, and a screenshot is not that: on a 1x display its divisor lands on 2 and every shot
///   comes back 960x540, half the canvas the UI is authored at, so a shot taken to JUDGE the
///   interface asks for full size (`make sim-shot SIM_W=1920 SIM_H=1080`). Off-screen edges are
///   fine for a headless grab: the drawable is the window's own framebuffer.
/// * `Fit` is **an exact divisor, never a best fit**: the largest of 1/1, 1/2 and 1/3 of the canvas
///   whose window, in points, fits the usable area. `surface::scale` will letterbox any drawable it
///   is given, so an arbitrary size would *work*, but a fractional scale resamples all of the above.
///   The bounds are in points, so the window is `canvas / div / density` of them.
///
/// The television asks for the canvas itself (1:1) unless the owner picked a smaller Interface
/// resolution in Settings on webOS 1-3 (`requested_drawable`), in which case the compositor fills the panel.
/// A Mac is the case the `surface` doc was written against — 1920x1080 exceeds the usable area of
/// every laptop display Apple ships, so asking for it flatly would put the title bar above the
/// screen and the bottom of the interface under the Dock.
#[cfg(any(feature = "hostsim", test))]
fn plan_desktop_window(ask: WindowAsk, density: Density) -> (c_int, c_int) {
    let points = |(w, h): (c_int, c_int)| (density.points(w), density.points(h));
    match ask {
        WindowAsk::Drawable(w, h) => points((w, h)),
        WindowAsk::Fit(uw, uh) => {
            // A little headroom under the usable bounds: a window flush against them reads as a
            // fullscreen that went wrong rather than as a deliberate size.
            let (max_w, max_h) = ((uw as f32 * 0.95) as c_int, (uh as f32 * 0.95) as c_int);
            let fits = |div| {
                let (w, h) = points((SCR_W / div, SCR_H / div));
                w <= max_w && h <= max_h
            };
            let div = (1..3).find(|&div| fits(div)).unwrap_or(3);
            points((SCR_W / div, SCR_H / div))
        }
    }
}

/// `SDL_WINDOWPOS_CENTERED` on display 0.
#[cfg(feature = "hostsim")]
const WINDOWPOS_CENTERED: c_int = 0x2FFF_0000u32 as c_int;

/// What this process is asked for: `PLXNATIVE_WIN` if it holds a size, else the fit to the display,
/// or the canvas itself if SDL cannot say how big the display is.
#[cfg(feature = "hostsim")]
fn desktop_window_ask() -> WindowAsk {
    if let Some((w, h)) = std::env::var_os("PLXNATIVE_WIN").and_then(|v| parse_window_override(&v.to_string_lossy())) {
        return WindowAsk::Drawable(w, h);
    }
    let mut r = [0 as c_int; 4]; // SDL_Rect: x, y, w, h
    let ok = unsafe { SDL_GetDisplayUsableBounds(0, r.as_mut_ptr()) } == 0 && r[2] > 0 && r[3] > 0;
    if ok {
        WindowAsk::Fit(r[2], r[3])
    } else {
        WindowAsk::Drawable(SCR_W, SCR_H)
    }
}

/// Size the just-created desktop `win` for the pixel density of the display it opened on.
///
/// `SDL_GetDisplayDPI` is unreliable on macOS, so the density is MEASURED instead: the window was
/// opened at the 1x answer of [`plan_desktop_window`], and `SDL_GL_GetDrawableSize` over
/// `SDL_GetWindowSize` says how many pixels that bought per point. On a 1x display nothing happens.
/// On a 2x display the window is resized (and put back in the middle of the display, since it was
/// centred at the old size) to the points that give the drawable that was asked for:
/// `PLXNATIVE_WIN=1920x1080` is a 960x540-point window, not a 1920x1080-point one with a 3840x2160
/// drawable. Called before `surface::probe`, which reads the drawable once and for good.
#[cfg(feature = "hostsim")]
unsafe fn fit_window_to_density(win: *mut c_void, ask: WindowAsk) {
    let ((ww, wh), (dw, dh)) = plx_base::surface::window_and_drawable_size(win);
    let density = Density::measured(ww, dw);
    if density == Density::UNIT {
        return;
    }
    let (pw, ph) = plan_desktop_window(ask, density);
    if (pw, ph) == (ww, wh) {
        return;
    }
    SDL_SetWindowSize(win, pw, ph);
    SDL_SetWindowPosition(win, WINDOWPOS_CENTERED, WINDOWPOS_CENTERED);
    let ((nw, nh), (gw, gh)) = plx_base::surface::window_and_drawable_size(win);
    log(&format!(
        "surface: {:.3} pixels per point (a {ww}x{wh} point window read back a {dw}x{dh} drawable); \
         resized to {pw}x{ph} points, which now reads {nw}x{nh} points / {gw}x{gh} drawable",
        f64::from(dw) / f64::from(ww)
    ));
}

#[cfg(test)]
mod desktop_window_tests {
    use super::WindowAsk::{Drawable, Fit};
    use super::{parse_window_override, plan_desktop_window, Density, WindowAsk, SCR_H, SCR_W};

    /// A window of 1920 points whose drawable reads back 3840 pixels: macOS Retina, the case this
    /// module exists for. (macOS always reports a whole backing factor, even in a "scaled" mode.)
    fn retina() -> Density {
        Density::measured(1920, 3840)
    }

    /// The window the planner opens, in points, and the drawable that window gives.
    fn plan(ask: WindowAsk, density: Density) -> ((i32, i32), (i32, i32)) {
        let points = plan_desktop_window(ask, density);
        (points, (density.pixels(points.0), density.pixels(points.1)))
    }

    /// `PLXNATIVE_WIN=1920x1080` on a 1x display is the 1920x1080 window it always was.
    #[test]
    fn a_1x_display_gives_the_override_as_asked() {
        assert_eq!(plan(Drawable(1920, 1080), Density::UNIT), ((1920, 1080), (1920, 1080)));
    }

    /// THE BUG: `PLXNATIVE_WIN=1920x1080` on a 2x display was a 1920x1080-POINT window, whose
    /// drawable is 3840x2160 (`scale=2.000`) and which breached the render-set ceiling on every
    /// frame of a Library scroll. The override is the DRAWABLE, so the window is half of it.
    #[test]
    fn the_override_is_the_drawable_on_a_2x_display() {
        assert_eq!(plan(Drawable(1920, 1080), retina()), ((960, 540), (1920, 1080)));
    }

    /// A 2x laptop (usable ~1512x945 points): 960x540 points, a 1:1 drawable. What it always got,
    /// by the coincidence of div=2 meeting a 2x density; now by arithmetic.
    #[test]
    fn a_2x_laptop_fits_960x540_points_for_a_1_to_1_drawable() {
        assert_eq!(plan(Fit(1512, 945), retina()), ((960, 540), (1920, 1080)));
    }

    /// THE SECOND HOLE: a 5K/4K display "looking like 2560x1440" has usable ~2560x1415 points, so
    /// the fit in points took div=1 -- a 1920x1080-point window, a 3840x2160 drawable -- with no
    /// override at all. The fit is of the DRAWABLE, measured in the points it occupies.
    #[test]
    fn a_large_2x_display_gives_the_same_1_to_1_drawable_without_any_override() {
        assert_eq!(plan(Fit(2560, 1415), retina()), ((960, 540), (1920, 1080)));
    }

    /// Nothing changes on a 1x display: the divisor lands where it did, on every size.
    #[test]
    fn a_1x_display_fits_exactly_as_it_did() {
        // a 1080p display's usable area under a menu bar and a Dock: div=2
        assert_eq!(plan(Fit(1920, 1050), Density::UNIT), ((960, 540), (960, 540)));
        // a display big enough for the canvas itself (div=1) needs 5% headroom: 1920 / 0.95 = 2021.05
        assert_eq!(plan(Fit(2022, 1140), Density::UNIT), ((1920, 1080), (1920, 1080)));
        assert_eq!(plan(Fit(2020, 1140), Density::UNIT), ((960, 540), (960, 540)));
        // a small one: div=3
        assert_eq!(plan(Fit(100, 100), Density::UNIT), ((640, 360), (640, 360)));
    }

    /// A density that is not a whole number (a 150% Windows display, a fractional Wayland scale):
    /// the window is the drawable over the ratio, exactly where that is exact.
    #[test]
    fn a_fractional_density_is_divided_out_too() {
        let d150 = Density::measured(1280, 1920);
        assert_eq!(plan(Drawable(1920, 1080), d150), ((1280, 720), (1920, 1080)));
        // 1280x720 points is over 95% of a 1280x800 display, so the half (960x540 px) wins
        assert_eq!(plan(Fit(1280, 800), d150), ((640, 360), (960, 540)));
    }

    /// Whatever the ratio, the window never gives a drawable LARGER than the one asked for: a
    /// pixel short is a letterbox, a pixel over is a texture that is not the canvas.
    #[test]
    fn the_drawable_is_never_larger_than_asked_on_any_density() {
        for (window_w, drawable_w) in [(1000, 1100), (1000, 1250), (1000, 1333), (1000, 1750), (1000, 2000), (1000, 2500), (1000, 3000), (1920, 2880), (1366, 2049)] {
            let d = Density::measured(window_w, drawable_w);
            for (w, h) in [(1920, 1080), (960, 540), (640, 360), (1280, 720)] {
                let (_, got) = plan(Drawable(w, h), d);
                assert!(got.0 <= w && got.1 <= h, "{window_w}->{drawable_w} asked {w}x{h}: {got:?}");
                // and no more than a point short of it, in either axis
                let point = (drawable_w + window_w - 1) / window_w;
                assert!(w - got.0 < point && h - got.1 < point, "{window_w}->{drawable_w} asked {w}x{h}: {got:?}");
            }
            for (uw, uh) in [(1280, 720), (1512, 945), (1920, 1050), (2560, 1415), (3840, 2100)] {
                let (_, got) = plan(Fit(uw, uh), d);
                assert!(got.0 <= SCR_W && got.1 <= SCR_H, "{window_w}->{drawable_w} usable {uw}x{uh}: {got:?}");
            }
        }
    }

    /// A drawable that is not larger than the window is no HiDPI at all, however it got that way:
    /// a window is never grown to cover for a measurement that failed.
    #[test]
    fn a_drawable_no_larger_than_the_window_is_unit_density() {
        assert_eq!(Density::measured(1920, 1920), Density::UNIT);
        assert_eq!(Density::measured(0, 1920), Density::UNIT);
        assert_eq!(Density::measured(1920, 960), Density::UNIT);
    }

    #[test]
    fn the_override_parses_a_positive_size_and_nothing_else() {
        assert_eq!(parse_window_override("1920x1080"), Some((1920, 1080)));
        assert_eq!(parse_window_override(" 640X360 "), Some((640, 360)));
        assert_eq!(parse_window_override("1920 x 1080"), Some((1920, 1080)));
        for bad in ["", "1920", "x", "0x1080", "1920x0", "-1x5", "axb", "1920x1080x2"] {
            assert_eq!(parse_window_override(bad), None, "{bad:?}");
        }
    }
}

/// The order `Window::open` does its work in, and the one thing this file must never grow. Each
/// test reads the source, because no host test can open a window; the needles are split with
/// `concat!` so this module's own text, which `include_str!` reads too, can never be what they match.
#[cfg(test)]
mod open_order_tests {
    fn source() -> &'static str {
        include_str!("window.rs")
    }

    /// The body of `Window::open`, from its signature to the next method.
    fn open_body() -> &'static str {
        source()
            .split_once(concat!("pub fn ", "open("))
            .expect("Window::open")
            .1
            .split_once(concat!("pub fn ", "raw("))
            .expect("the method after open")
            .0
    }

    /// `surface::probe` reads the drawable once, at boot, for good: the desktop window has to be
    /// sized for the display's pixel density BEFORE it, or a 2x display keeps the 3840x2160
    /// drawable the 1x size gave it.
    #[test]
    fn the_desktop_window_is_fitted_to_the_density_before_the_drawable_is_probed() {
        let body = open_body();
        let create = body.find(concat!("sys::SDL_CreateWindow", "(")).expect("creates the window");
        let fit = body.find(concat!("fit_window_to_", "density(win")).expect("fits the window to the density");
        let probe = body.find(concat!("surface::", "probe(win)")).expect("probes the drawable");
        assert!(create < fit && fit < probe, "create the window, fit it to the density, then probe the drawable");
        assert_eq!(body.matches(concat!("surface::", "probe(")).count(), 1, "one probe");
    }

    /// `open` frees a half-made window before it retries (a leaked one would keep the display), and
    /// does it inside the creation loop, ahead of the probe.
    #[test]
    fn open_destroys_a_half_made_window_before_the_retry() {
        let body = open_body();
        let create = body.find(concat!("sys::SDL_CreateWindow", "(")).expect("creates the window");
        let context = body.find(concat!("sys::SDL_GL_CreateContext", "(")).expect("creates the context");
        let destroy = body.find(concat!("sys::SDL_DestroyWindow", "(win)")).expect("destroys a half-made window");
        let retry = body.find(concat!("fallback.", "take()")).expect("takes the one fallback size");
        let probe = body.find(concat!("surface::", "probe(win)")).expect("probes the drawable");
        assert!(create < context && context < destroy && destroy < retry && retry < probe,
            "create, context, destroy the half-made window, retry at the fallback, then probe");
    }

    /// **`Window` has no destructor.** The application is leaked at exit and a worker may still hold
    /// the handle, so a window that destroyed itself (or nulled itself when the compositor took the
    /// screen) would free a surface under a live reader. `init::quit` is the explicit end.
    #[test]
    fn window_has_no_destructor() {
        assert!(!source().contains(concat!("impl Dr", "op for Window")), "a Window must never free itself");
        assert!(!source().contains(concat!("fn dr", "op(&mut self)")), "no destructor of any shape");
    }
}
