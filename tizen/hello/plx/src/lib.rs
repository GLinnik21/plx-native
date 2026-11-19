//! PlxNative Samsung Tizen TV, step 1: a "hello world" native library.
//!
//! A throwaway test build (branch `samsung/hello-tpk`, never merged). The C# host in
//! `tizen/hello/host/PlxHello` is a plain `CoreUIApplication` whose `OnCreate` calls
//! [`plx_tizen_create`] once; everything else happens here. This follows flutter-tizen's model, not
//! a NUI GLWindow: the native code makes its own Ecore_Wl2 window, an EGL surface on it, and listens
//! for remote keys through `ecore_event_handler_add`.
//!
//! It answers four questions, in the log file and on screen:
//! 1. does the host start (the host's own `[host]` lines),
//! 2. does this .so load (the `plx-hello 0.1.0` line),
//! 3. does the window draw (the orange clear colour),
//! 4. do keys arrive (every `key down/up` line, and the colour changing on each press).
//!
//! Threads: Ecore is not thread-safe and its main loop runs on the host's main thread, so window and
//! key handlers are created there, inside `plx_tizen_create`. A Rust render thread owns the EGL
//! context (made current there) and draws.
//!
//! No Tizen library is linked; all are `dlopen`ed (see `dl.rs`). Nothing here has been run on a
//! device: see the README for what is verified and what is guessed.

mod dl;
mod keys;
mod log;
mod render;
mod sym;
mod sysinfo;
mod window;

use dl::{Lib, Ptr};
use log::plog;
use std::ffi::{c_char, c_void, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use sym::{Ecore, Egl, Gles, WlEgl};
use window::Window;

/// Return codes of `plx_tizen_create` (0 = success). Each failure is also logged by name.
pub(crate) mod code {
    macro_rules! codes {
        ($($name:ident = $n:expr),* $(,)?) => {
            $( pub const $name: i32 = $n; )*
            pub fn describe(c: i32) -> &'static str {
                match c { 0 => "OK", $( $n => stringify!($name), )* _ => "UNKNOWN" }
            }
        };
    }
    codes! {
        PANIC = 1,
        BAD_ARGS = 2,
        ALREADY_CREATED = 3,
        DLOPEN = 4,
        SYMBOLS = 5,
        ECORE_INIT = 6,
        DISPLAY = 7,
        SCREEN_SIZE = 8,
        WINDOW = 9,
        EGL_WINDOW = 10,
        HANDLER = 11,
        EGL_DISPLAY = 12,
        EGL_INIT = 13,
        EGL_CONFIG = 14,
        EGL_CONTEXT = 15,
        EGL_SURFACE = 16,
        THREAD = 17,
        MAKE_CURRENT = 18,
        SWAP = 19,
    }
}

/// How long `create` waits for the render thread's first presented frame before carrying on. A
/// timeout is not an error: the swap may be waiting on a compositor that needs the main loop (which
/// only runs once `OnCreate` returns).
const FIRST_FRAME_WAIT: Duration = Duration::from_secs(3);
/// How long `terminate` waits for the render thread before leaving teardown to process exit.
const JOIN_WAIT: Duration = Duration::from_secs(2);

struct Runtime {
    ecore: Ecore,
    egl: Egl,
    egl_ctx: render::EglCtx,
    window: Window,
    handlers: Vec<Ptr>,
    render: Option<JoinHandle<()>>,
}

// SAFETY: the raw handles are used only by the main thread (Ecore, the window) after creation; the
// render thread got its own copies of the EGL handles and is joined before they are released.
unsafe impl Send for Runtime {}

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

fn runtime() -> std::sync::MutexGuard<'static, Option<Runtime>> {
    RUNTIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` so that a panic never crosses the C ABI.
fn guard<R>(name: &str, on_panic: R, f: impl FnOnce() -> R) -> R {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(_) => {
            plog!("PANIC in {name} (swallowed at the FFI boundary)");
            on_panic
        }
    }
}

fn cstr_arg(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    // SAFETY: the caller promises a NUL-terminated string (the host marshals a C# string).
    Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
}

/// Create the window, EGL surface, key handlers and render thread. MAIN thread, once, from
/// `OnCreate`. `data_dir` is the app's writable data directory (the host's `DirectoryInfo.Data`);
/// `<data_dir>/plx-hello.log` is the log. Returns 0 or one of [`code`].
#[no_mangle]
pub extern "C" fn plx_tizen_create(data_dir: *const c_char) -> i32 {
    guard("plx_tizen_create", code::PANIC, || {
        let rc = create(data_dir);
        if rc == 0 {
            plog!("plx_tizen_create -> 0 (OK)");
        } else {
            plog!("plx_tizen_create FAILED -> {rc} ({})", code::describe(rc));
        }
        rc
    })
}

/// The host went to the background: stop drawing.
#[no_mangle]
pub extern "C" fn plx_tizen_pause() {
    guard("plx_tizen_pause", (), || {
        render::PAUSED.store(true, Ordering::Release);
        plog!("lifecycle: pause (render thread idles)");
    });
}

/// The host came back: draw again.
#[no_mangle]
pub extern "C" fn plx_tizen_resume() {
    guard("plx_tizen_resume", (), || {
        render::PAUSED.store(false, Ordering::Release);
        plog!("lifecycle: resume");
    });
}

/// The host is terminating: stop the render thread and release everything. MAIN thread.
#[no_mangle]
pub extern "C" fn plx_tizen_terminate() {
    guard("plx_tizen_terminate", (), terminate);
}

/// The system locale changed. Logged only (`LC_NUMERIC` stays `C`).
#[no_mangle]
pub extern "C" fn plx_tizen_locale(loc: *const c_char) {
    guard("plx_tizen_locale", (), || {
        plog!(
            "lifecycle: locale changed -> {}",
            cstr_arg(loc).unwrap_or_else(|| "(null)".into())
        );
    });
}

fn create(data_dir: *const c_char) -> i32 {
    let Some(dir) = cstr_arg(data_dir) else {
        log::to_stderr("plx: plx_tizen_create called with a null data_dir\n");
        return code::BAD_ARGS;
    };
    let (path, opened) = log::init(&dir);
    std::panic::set_hook(Box::new(|info| plog!("panic: {info}")));
    sysinfo::log_environment(&dir);
    plog!("log file {path} opened={opened}");

    let mut slot = runtime();
    if slot.is_some() {
        plog!("plx_tizen_create called twice");
        return code::ALREADY_CREATED;
    }

    // nuvio's gotcha: the .NET host puts the process in the TV's locale, and a decimal comma breaks
    // printf("%f")-built JSON. The host also sets the environment variable; .NET's
    // SetEnvironmentVariable need not reach native getenv, so set it here too.
    // SAFETY: valid NUL-terminated string; called on the main thread before any other thread.
    let loc = unsafe { libc::setlocale(libc::LC_NUMERIC, c"C".as_ptr()) };
    plog!(
        "setlocale(LC_NUMERIC, \"C\") -> {:?}; env LC_NUMERIC={:?} LANG={:?} LC_ALL={:?}",
        if loc.is_null() { "(null)".to_string() } else { unsafe { CStr::from_ptr(loc) }.to_string_lossy().into_owned() },
        std::env::var("LC_NUMERIC").ok(),
        std::env::var("LANG").ok(),
        std::env::var("LC_ALL").ok()
    );

    // --- libraries -----------------------------------------------------------------------
    let ecore_wl2 = dl::open_first("ecore_wl2", &["libecore_wl2.so.1", "libecore_wl2.so"]);
    let ecore_lib = dl::open_first("ecore", &["libecore.so.1", "libecore.so"]);
    let ecore_input = dl::open_first("ecore_input", &["libecore_input.so.1", "libecore_input.so"]);
    let wl_egl_lib = dl::open_first("wayland-egl", &["libwayland-egl.so.1", "libwayland-egl.so"]);
    let egl_lib = dl::open_first("EGL", &["libEGL.so.1", "libEGL.so"]);
    let gles_lib = dl::open_first("GLESv2", &["libGLESv2.so.2", "libGLESv2.so"]);
    let (Some(ecore_wl2), Some(ecore_lib), Some(ecore_input), Some(egl_lib), Some(gles_lib)) =
        (ecore_wl2, ecore_lib, ecore_input, egl_lib, gles_lib)
    else {
        plog!("a required library did not open (libwayland-egl is optional)");
        return code::DLOPEN;
    };

    // --- symbols -------------------------------------------------------------------------
    let ecore_libs: [&Lib; 3] = [&ecore_wl2, &ecore_lib, &ecore_input];
    let ecore = Ecore::load(&ecore_libs);
    let wl_egl = wl_egl_lib.and_then(|l| WlEgl::load(&[&l]).ok());
    let egl = Egl::load(&[&egl_lib]);
    let gles = Gles::load(&[&gles_lib]);
    let (Ok(ecore), Ok(egl), Ok(gles)) = (ecore, egl, gles) else {
        plog!("required symbols missing (see the `MISSING (required)` lines above)");
        return code::SYMBOLS;
    };

    // --- window + key handlers (main thread, as the embedder does) -------------------------
    let mut window = match Window::create(&ecore, wl_egl) {
        Ok(w) => w,
        Err(rc) => return rc,
    };

    let mut handlers: Vec<Ptr> = Vec::new();
    let rc = register_handlers(&ecore, &ecore_libs, &ecore_wl2, &mut handlers);
    if rc != 0 {
        abort_partial(&ecore, &handlers, &mut window);
        return rc;
    }
    window.show(&ecore);

    // --- EGL on this thread, drawing on the render thread -------------------------------
    let egl_ctx = match render::setup(&egl, &window) {
        Ok(c) => c,
        Err(rc) => {
            abort_partial(&ecore, &handlers, &mut window);
            return rc;
        }
    };
    let (tx, rx) = mpsc::channel();
    let render = match render::spawn(egl, gles, egl_ctx, window.width, window.height, tx) {
        Ok(h) => h,
        Err(e) => {
            plog!("cannot spawn the render thread: {e}");
            render::teardown(&egl, &egl_ctx);
            abort_partial(&ecore, &handlers, &mut window);
            return code::THREAD;
        }
    };
    match rx.recv_timeout(FIRST_FRAME_WAIT) {
        Ok(Ok(())) => {}
        Ok(Err(rc)) => {
            let _ = render.join();
            render::teardown(&egl, &egl_ctx);
            abort_partial(&ecore, &handlers, &mut window);
            return rc;
        }
        Err(mpsc::RecvTimeoutError::Timeout) => plog!(
            "no first frame within {FIRST_FRAME_WAIT:?}; carrying on (the swap may need the main loop, which runs once OnCreate returns)"
        ),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            plog!("render thread ended without reporting");
            let _ = render.join();
            render::teardown(&egl, &egl_ctx);
            abort_partial(&ecore, &handlers, &mut window);
            return code::PANIC;
        }
    }

    *slot = Some(Runtime { ecore, egl, egl_ctx, window, handlers, render: Some(render) });
    0
}

/// `ecore_event_handler_add` for key down/up (required) and focus in/out (diagnostic). The event
/// ids are ints exported by libecore_input / libecore_wl2, valid only after init.
fn register_handlers(ecore: &Ecore, libs: &[&Lib; 3], wl2: &Lib, out: &mut Vec<Ptr>) -> i32 {
    let id = |lib_set: &[&Lib], name: &str| -> i32 {
        let mut v = dl::read_int(lib_set, name).unwrap_or(0);
        if v == 0 {
            if let Some(init) = ecore.event_init {
                // SAFETY: refcounted init; harmless if already done.
                let n = unsafe { init() };
                plog!("{name} read 0; called ecore_event_init -> {n}");
                v = dl::read_int(lib_set, name).unwrap_or(0);
            }
        }
        plog!("{name} = {v}");
        v
    };
    let mut add = |ty: i32, cb: sym::EventCb, data: usize, what: &str, required: bool| -> bool {
        if ty == 0 {
            plog!("{what}: event id is 0; handler NOT registered");
            return !required;
        }
        // SAFETY: valid callback with the Ecore_Event_Handler_Cb signature; `data` is an integer
        // tag, never dereferenced. Called on the main thread.
        let h = unsafe { (ecore.event_handler_add)(ty, cb, data as *const c_void) };
        plog!("ecore_event_handler_add({what}) -> {h:p}");
        if h.is_null() {
            return !required;
        }
        out.push(h);
        true
    };
    let key_down = id(libs.as_slice(), "ECORE_EVENT_KEY_DOWN");
    let key_up = id(libs.as_slice(), "ECORE_EVENT_KEY_UP");
    let focus_in = id(&[wl2], "ECORE_WL2_EVENT_FOCUS_IN");
    let focus_out = id(&[wl2], "ECORE_WL2_EVENT_FOCUS_OUT");
    if !add(key_down, keys::key_cb, 1, "KEY_DOWN", true) || !add(key_up, keys::key_cb, 0, "KEY_UP", true) {
        return code::HANDLER;
    }
    add(focus_in, keys::focus_cb, 1, "FOCUS_IN", false);
    add(focus_out, keys::focus_cb, 0, "FOCUS_OUT", false);
    0
}

fn del_handlers(ecore: &Ecore, handlers: &[Ptr]) {
    if let Some(del) = ecore.event_handler_del {
        for &h in handlers {
            // SAFETY: handle returned by ecore_event_handler_add on the main thread.
            unsafe { del(h) };
        }
        plog!("deleted {} ecore event handlers", handlers.len());
    }
}

fn abort_partial(ecore: &Ecore, handlers: &[Ptr], window: &mut Window) {
    del_handlers(ecore, handlers);
    window.destroy(ecore);
}

fn terminate() {
    plog!("lifecycle: terminate");
    render::RUNNING.store(false, Ordering::Release);
    let Some(mut rt) = runtime().take() else {
        plog!("terminate: nothing created, nothing to release");
        return;
    };
    del_handlers(&rt.ecore, &rt.handlers);
    if let Some(h) = rt.render.take() {
        let t0 = Instant::now();
        while !h.is_finished() && t0.elapsed() < JOIN_WAIT {
            std::thread::sleep(Duration::from_millis(10));
        }
        if h.is_finished() {
            let _ = h.join();
        } else {
            // A swap stuck in the compositor: do not tear the surface down under it.
            plog!("render thread still busy after {JOIN_WAIT:?}; leaving teardown to process exit");
            return;
        }
    }
    render::teardown(&rt.egl, &rt.egl_ctx);
    rt.window.destroy(&rt.ecore);
    plog!("terminate: done");
}
