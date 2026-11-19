//! EGL/GLES2 and the render thread.
//!
//! The split mirrors flutter-tizen's embedder: the EGL display, config, context and window surface
//! are created on the MAIN thread inside `plx_tizen_create` ([`setup`]), so each failure comes back
//! as a return code; only `eglMakeCurrent`, the clear and the swaps happen on the render thread,
//! which owns the context from then on. Each key press advances the clear colour through
//! [`PALETTE`], so a tester sees keys arrive without reading any log.

use crate::code;
use crate::dl::Ptr;
use crate::keys;
use crate::log::plog;
use crate::sym::*;
use crate::window::Window;
use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::thread::JoinHandle;
use std::time::Duration;

/// Set false to stop the render thread (terminate).
pub static RUNNING: AtomicBool = AtomicBool::new(false);
/// Set while the host is paused: no clears, no swaps.
pub static PAUSED: AtomicBool = AtomicBool::new(false);

/// The first colour is what "the .so loaded and the window drew" looks like; every key press moves
/// to the next one (wrapping).
const PALETTE: [(&str, f32, f32, f32); 8] = [
    ("orange", 1.00, 0.45, 0.00),
    ("red", 0.90, 0.10, 0.10),
    ("green", 0.10, 0.75, 0.20),
    ("blue", 0.10, 0.25, 0.95),
    ("yellow", 0.95, 0.90, 0.10),
    ("magenta", 0.85, 0.10, 0.80),
    ("cyan", 0.05, 0.85, 0.90),
    ("white", 1.00, 1.00, 1.00),
];

/// EGL objects made on the main thread.
#[derive(Clone, Copy)]
pub struct EglCtx {
    pub display: Ptr,
    pub context: Ptr,
    pub surface: Ptr,
}

fn egl_error(egl: &Egl) -> String {
    // SAFETY: eglGetError takes no arguments.
    format!("0x{:04x}", unsafe { (egl.get_error)() })
}

fn egl_str(egl: &Egl, dpy: Ptr, name: i32) -> String {
    // SAFETY: valid initialized display; the returned string is static.
    let p = unsafe { (egl.query_string)(dpy, name) };
    if p.is_null() {
        "(null)".to_string()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// `eglChooseConfig` with `alpha` (8, or EGL_DONT_CARE for the fallback). Window-capable,
/// RGB888, ES2.
fn choose_config(egl: &Egl, dpy: Ptr, alpha: i32) -> Option<Ptr> {
    let attribs = [
        EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
        EGL_RED_SIZE, 8,
        EGL_GREEN_SIZE, 8,
        EGL_BLUE_SIZE, 8,
        EGL_ALPHA_SIZE, alpha,
        EGL_RENDERABLE_TYPE, EGL_OPENGL_ES2_BIT,
        EGL_NONE,
    ];
    let mut config: Ptr = std::ptr::null_mut();
    let mut n = 0i32;
    // SAFETY: attribs is EGL_NONE-terminated; room for one config.
    let ok = unsafe { (egl.choose_config)(dpy, attribs.as_ptr(), &mut config, 1, &mut n) };
    plog!("eglChooseConfig(alpha={alpha}) -> {ok} configs={n} config={config:p}");
    (ok != 0 && n > 0 && !config.is_null()).then_some(config)
}

fn log_config(egl: &Egl, dpy: Ptr, config: Ptr) {
    let Some(get) = egl.get_config_attrib else { return };
    let mut parts = Vec::new();
    for (name, attr) in [
        ("R", EGL_RED_SIZE),
        ("G", EGL_GREEN_SIZE),
        ("B", EGL_BLUE_SIZE),
        ("A", EGL_ALPHA_SIZE),
        ("buffer", EGL_BUFFER_SIZE),
    ] {
        let mut v = -1i32;
        // SAFETY: valid display/config, writable out-parameter.
        unsafe { get(dpy, config, attr, &mut v) };
        parts.push(format!("{name}={v}"));
    }
    plog!("chosen EGL config: {}", parts.join(" "));
}

/// Main thread: display, initialize, config, ES2 context, window surface (the embedder's
/// `TizenRendererEgl::CreateSurface` + `InitializeEGL`). Nothing is made current here.
pub fn setup(egl: &Egl, win: &Window) -> Result<EglCtx, i32> {
    // SAFETY (whole function): arguments are the embedder's; handles are null-checked.
    let dpy = unsafe { (egl.get_display)(win.wl_display) };
    plog!("eglGetDisplay(wl_display) -> {dpy:p}");
    if dpy.is_null() {
        return Err(code::EGL_DISPLAY);
    }
    let (mut major, mut minor) = (0i32, 0i32);
    let ok = unsafe { (egl.initialize)(dpy, &mut major, &mut minor) };
    if ok == 0 {
        plog!("eglInitialize FAILED error {}", egl_error(egl));
        return Err(code::EGL_INIT);
    }
    plog!("eglInitialize ok {major}.{minor}");
    plog!(
        "EGL vendor={:?} version={:?} client_apis={:?}",
        egl_str(egl, dpy, EGL_VENDOR),
        egl_str(egl, dpy, EGL_VERSION),
        egl_str(egl, dpy, EGL_CLIENT_APIS)
    );
    plog!("EGL extensions: {}", egl_str(egl, dpy, EGL_EXTENSIONS));

    let ok = unsafe { (egl.bind_api)(EGL_OPENGL_ES_API) };
    if ok == 0 {
        plog!("eglBindAPI(OPENGL_ES) FAILED error {}", egl_error(egl));
        return Err(code::EGL_INIT);
    }

    // Alpha-capable first (nuvio's gotcha, and the embedder's Impeller config), then any alpha.
    let Some(config) = choose_config(egl, dpy, 8).or_else(|| choose_config(egl, dpy, EGL_DONT_CARE))
    else {
        plog!("no usable EGL config; error {}", egl_error(egl));
        return Err(code::EGL_CONFIG);
    };
    log_config(egl, dpy, config);

    let ctx_attribs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
    let context = unsafe { (egl.create_context)(dpy, config, EGL_NO_CONTEXT, ctx_attribs.as_ptr()) };
    plog!("eglCreateContext(ES2) -> {context:p}");
    if context.is_null() {
        plog!("eglCreateContext FAILED error {}", egl_error(egl));
        return Err(code::EGL_CONTEXT);
    }

    let surf_attribs = [EGL_NONE];
    let surface = unsafe {
        (egl.create_window_surface)(dpy, config, win.native_window, surf_attribs.as_ptr())
    };
    plog!("eglCreateWindowSurface -> {surface:p}");
    if surface.is_null() {
        plog!("eglCreateWindowSurface FAILED error {}", egl_error(egl));
        return Err(code::EGL_SURFACE);
    }
    Ok(EglCtx { display: dpy, context, surface })
}

/// Release what [`setup`] made. Main thread, after the render thread has joined.
pub fn teardown(egl: &Egl, ctx: &EglCtx) {
    // SAFETY: handles from setup(); the context is no longer current anywhere.
    unsafe {
        (egl.make_current)(ctx.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
        if let Some(f) = egl.destroy_surface {
            plog!("eglDestroySurface -> {}", f(ctx.display, ctx.surface));
        }
        if let Some(f) = egl.destroy_context {
            plog!("eglDestroyContext -> {}", f(ctx.display, ctx.context));
        }
        if let Some(f) = egl.terminate {
            plog!("eglTerminate -> {}", f(ctx.display));
        }
    }
}

struct SendBox<T>(T);
// SAFETY: the EGL/GL tables are fn pointers and the handles are opaque tokens used by exactly one
// thread (the render thread) after the move.
unsafe impl<T> Send for SendBox<T> {}

fn gl_str(gles: &Gles, name: u32) -> String {
    // SAFETY: a current context exists on this thread; the string is static.
    let p = unsafe { (gles.get_string)(name) };
    if p.is_null() {
        "(null)".to_string()
    } else {
        unsafe { CStr::from_ptr(p as *const c_char) }.to_string_lossy().into_owned()
    }
}

/// Start the render thread. `ready` receives `Ok(())` after the first frame was presented, or the
/// error code that stopped it.
pub fn spawn(
    egl: Egl,
    gles: Gles,
    ctx: EglCtx,
    width: i32,
    height: i32,
    ready: Sender<Result<(), i32>>,
) -> std::io::Result<JoinHandle<()>> {
    RUNNING.store(true, Ordering::Release);
    let args = SendBox((egl, gles, ctx, ready));
    std::thread::Builder::new().name("plx-render".into()).spawn(move || {
        let args = args;
        let (egl, gles, ctx, ready) = args.0;
        let result = catch_unwind(AssertUnwindSafe(|| body(&egl, &gles, &ctx, width, height, &ready)));
        if result.is_err() {
            plog!("PANIC in render thread; thread stops");
            let _ = ready.send(Err(code::PANIC));
        }
        plog!("render thread exit");
    })
}

fn body(egl: &Egl, gles: &Gles, ctx: &EglCtx, width: i32, height: i32, ready: &Sender<Result<(), i32>>) {
    // SAFETY (whole function): this thread now owns the context; handles come from setup().
    let ok = unsafe { (egl.make_current)(ctx.display, ctx.surface, ctx.surface, ctx.context) };
    if ok == 0 {
        plog!("eglMakeCurrent FAILED error {}", egl_error(egl));
        let _ = ready.send(Err(code::MAKE_CURRENT));
        return;
    }
    plog!("eglMakeCurrent ok (render thread owns the context)");
    // nuvio: vsync dropped Tizen 9 to 3 fps, so API9+ sets interval 0; the loop paces itself.
    let r = unsafe { (egl.swap_interval)(ctx.display, 0) };
    plog!("eglSwapInterval(0) -> {r}");
    plog!(
        "GL vendor={:?} renderer={:?} version={:?}",
        gl_str(gles, GL_VENDOR),
        gl_str(gles, GL_RENDERER),
        gl_str(gles, GL_VERSION)
    );
    if let Some(vp) = gles.viewport {
        unsafe { vp(0, 0, width, height) };
    }

    let mut idx = 0usize;
    let mut frames = 0u64;
    let mut swap_failures = 0u64;
    plog!("colour #0 {}", PALETTE[0].0);
    while RUNNING.load(Ordering::Acquire) {
        for k in keys::drain() {
            if k.down {
                idx = (idx + 1) % PALETTE.len();
                plog!("key press -> colour #{idx} {}", PALETTE[idx].0);
            }
        }
        if PAUSED.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let (_, r, g, b) = PALETTE[idx];
        let swapped = unsafe {
            (gles.clear_color)(r, g, b, 1.0);
            (gles.clear)(GL_COLOR_BUFFER_BIT);
            (egl.swap_buffers)(ctx.display, ctx.surface)
        };
        if swapped == 0 {
            if swap_failures % 60 == 0 {
                plog!("eglSwapBuffers FAILED (#{swap_failures}) error {}", egl_error(egl));
            }
            swap_failures += 1;
            if frames == 0 {
                let _ = ready.send(Err(code::SWAP));
                return;
            }
        } else {
            if frames == 0 {
                let gl = unsafe { (gles.get_error)() };
                plog!("first frame presented (glGetError=0x{gl:04x})");
                let _ = ready.send(Ok(()));
            }
            frames += 1;
            if frames % 60 == 0 {
                plog!("frames={frames} colour=#{idx} {}", PALETTE[idx].0);
            }
        }
        std::thread::sleep(Duration::from_millis(16));
    }
    unsafe { (egl.make_current)(ctx.display, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT) };
    plog!("render loop stopped after {frames} frames");
}
