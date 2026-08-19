//! What EGL this television actually has — a boot-time capability probe, and nothing else.
//!
//! # Why this exists
//!
//! The app has never asked. `docs/perf-damage-tracking-verdict.md` §6 names this as the one cheap
//! device experiment that settles a whole architectural direction: every buffer-preservation
//! scheme, "present with empty damage during playback", and `EGL_KHR_partial_update` — the
//! extension that would make partial redraw cost less than a full one on a tiler rather than more
//! — die if the driver does not advertise them. That question cannot be answered off-device and
//! it cannot be answered from the NDK sysroot, which ships a **link stub**: 44 `egl*` symbols all
//! aliased to one empty body, so the presence of `eglSetDamageRegionKHR` *there* proves only that
//! whoever generated the stub saw the name, not that this firmware implements it.
//!
//! # Why it does not link libEGL, and must not
//!
//! `-lEGL` would put `DT_NEEDED: libEGL.so.1` (the stub's SONAME) in the binary, and the loader
//! treats a missing `DT_NEEDED` as fatal at `exec()` — before `main`, before the event log opens.
//! `tools/fwcompat.py --inventory libEGL libEGLfk` says that is exactly what would happen on the
//! releases this app runs on: webOS 2.2.3 through 5.3.1 (**including this dev set at 4.10.0**)
//! carry `libEGLfk.so.2`, and their `libEGL.so.1.5` exports **no symbols at all** — it is a
//! forwarder onto `libmali.so`. The SONAME moves, which is the textbook case for
//! `dynlib.rs`'s treatment.
//!
//! But we do not even need a `dlopen`: SDL created the GLES2 context **through EGL**, so whichever
//! EGL the firmware has is already mapped into the process with its symbols in the global scope.
//! `dynlib::Handle::self_handle()` (`RTLD_DEFAULT`) therefore resolves them with no new library,
//! no new `DT_NEEDED`, and no change to the `fwcompat` matrix — the same mechanism
//! `surface::panel_resolution` uses for the SDL entry points that exist only on some firmwares.
//! A SONAME candidate list is kept as a fallback for the case where EGL is loaded privately
//! (`RTLD_LOCAL`) and so is invisible to `RTLD_DEFAULT`.
//!
//! # What it reports, and why each field is here
//!
//! - `EGL_VENDOR` / `EGL_VERSION` / `EGL_CLIENT_APIS` / **`EGL_EXTENSIONS`** — the answer.
//! - `eglGetProcAddress` for the three damage entry points. An extension string without a
//!   resolvable entry point is a driver bug we would otherwise discover by jumping through null.
//! - **`EGL_SWAP_BEHAVIOR`**. `EGL_KHR_partial_update` makes it an *error* to call
//!   `eglSetDamageRegionKHR` on an `EGL_BUFFER_PRESERVED` surface, so what SDL configured decides
//!   whether the extension is usable at all here.
//! - **`EGL_BUFFER_AGE_KHR`**. The same spec makes it an error to set a damage region without
//!   having queried the buffer age (unless the damage is the whole buffer) — and the age is what
//!   says *how many frames back* the inherited content is, which is the number a damage ring has
//!   to union over. A driver that does not answer this closes the partial-update direction
//!   outright, whatever the extension string says.
//! - `GL_EXTENSIONS`, which nothing in the app logged either.
//!
//! Diagnostic only. Nothing in this module is called from a draw path, it runs exactly once at
//! boot, and no other module reads it — it exists to put a fact in the event log.
use crate::dynlib::Handle;
use std::ffi::CStr;
use std::os::raw::{c_char, c_int, c_uint, c_void};

// EGL 1.4 tokens. Spelled out rather than pulled from a header because nothing in this build
// includes one — the app has no EGL headers on any include path and does not want any.
const EGL_HEIGHT: c_int = 0x3056;
const EGL_WIDTH: c_int = 0x3057;
const EGL_DRAW: c_int = 0x3059;
const EGL_VENDOR: c_int = 0x3053;
const EGL_VERSION: c_int = 0x3054;
const EGL_EXTENSIONS: c_int = 0x3055;
const EGL_CLIENT_APIS: c_int = 0x308D;
const EGL_SWAP_BEHAVIOR: c_int = 0x3093;
const EGL_BUFFER_PRESERVED: c_int = 0x3094;
const EGL_BUFFER_DESTROYED: c_int = 0x3095;
/// `EGL_BUFFER_AGE_KHR` (`EGL_KHR_partial_update`) and `EGL_BUFFER_AGE_EXT`
/// (`EGL_EXT_buffer_age`) are **the same value**; the two extensions differ in name only here.
const EGL_BUFFER_AGE: c_int = 0x313D;
const GL_EXTENSIONS: c_uint = 0x1F03;

extern "C" {
    fn glGetString(name: c_uint) -> *const c_char;
    /// On an EGL backend SDL forwards this to `eglGetProcAddress` and then to a `dlsym` on the
    /// EGL handle **it** opened — which is the one lookup guaranteed to reach the same library
    /// SDL made our context with, even if that library was opened `RTLD_LOCAL`.
    fn SDL_GL_GetProcAddress(name: *const c_char) -> *mut c_void;
}

type FnGetCurrentDisplay = unsafe extern "C" fn() -> *mut c_void;
type FnGetCurrentSurface = unsafe extern "C" fn(c_int) -> *mut c_void;
type FnQueryString = unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char;
type FnQuerySurface = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint;
type FnGetProcAddress = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type FnGetError = unsafe extern "C" fn() -> c_int;

/// The SONAMEs an EGL might carry on a webOS television, in the order worth trying. Only used when
/// `RTLD_DEFAULT` cannot see EGL, which would mean SDL loaded it privately.
///
/// `libEGLfk.so.2` FIRST: it is the one on webOS 2.2.3–5.3.1, which is every release this app has
/// actually run on. `libEGL.so.1` is webOS 6+ (and 1.x). Neither is linked.
const EGL_SONAMES: &[&str] = &["libEGLfk.so.2", "libEGL.so.1", "libEGL.so"];

/// A resolved symbol, or `None`. Tries the process's own scope first, then the candidate list.
fn resolve(name: &str, lib: &mut Option<Handle>) -> Option<*mut c_void> {
    if let Some(p) = Handle::self_handle().sym(name).filter(|p| !p.is_null()) {
        return Some(p);
    }
    if let Ok(c) = std::ffi::CString::new(name) {
        let p = unsafe { SDL_GL_GetProcAddress(c.as_ptr()) };
        if !p.is_null() {
            return Some(p);
        }
    }
    if lib.is_none() {
        *lib = Handle::open(EGL_SONAMES).map(|(h, soname)| {
            crate::log(&format!("egl: RTLD_DEFAULT had no EGL; opened {soname}"));
            h
        });
    }
    lib.as_ref()?.sym(name).filter(|p| !p.is_null())
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return "<null>".to_string();
    }
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// Log everything this driver will tell us about EGL. Call once, after the GL context is current.
///
/// Takes no arguments on purpose: `eglGetCurrentDisplay`/`eglGetCurrentSurface` return the handles
/// of whatever context is current **on the calling thread**, and SDL made ours current on this one.
/// Asking EGL is what makes this a probe of the real surface rather than of a display we created.
pub(crate) fn probe() {
    let mut lib: Option<Handle> = None;
    let Some(get_display) = resolve("eglGetCurrentDisplay", &mut lib) else {
        // Not a fault on a desktop simulator (there is no EGL there at all) and a genuine
        // surprise on a television, so say which one this is rather than guessing.
        crate::log("egl: no eglGetCurrentDisplay in this process — EGL capabilities unknown");
        log_gl_extensions();
        return;
    };
    let get_display: FnGetCurrentDisplay = unsafe { std::mem::transmute(get_display) };
    let dpy = unsafe { get_display() };
    let surface = resolve("eglGetCurrentSurface", &mut lib).map_or(std::ptr::null_mut(), |f| {
        let f: FnGetCurrentSurface = unsafe { std::mem::transmute(f) };
        unsafe { f(EGL_DRAW) }
    });
    crate::log(&format!("egl: display={dpy:p} draw_surface={surface:p}"));
    if dpy.is_null() {
        crate::log("egl: EGL_NO_DISPLAY — SDL is not on an EGL backend here, nothing more to ask");
        log_gl_extensions();
        return;
    }

    if let Some(f) = resolve("eglQueryString", &mut lib) {
        let f: FnQueryString = unsafe { std::mem::transmute(f) };
        for (label, token) in [
            ("vendor", EGL_VENDOR),
            ("version", EGL_VERSION),
            ("client_apis", EGL_CLIENT_APIS),
        ] {
            crate::log(&format!("egl {label}: {}", cstr(unsafe { f(dpy, token) })));
        }
        // The line this whole module exists to produce. Unsplit: a grep for one extension name
        // has to be able to find it, and the event log has no line-length limit.
        crate::log(&format!("egl extensions: {}", cstr(unsafe { f(dpy, EGL_EXTENSIONS) })));
    }

    // An advertised extension whose entry point does not resolve is worse than an absent one:
    // it is a null jump at the first call. Report the two independently.
    if let Some(f) = resolve("eglGetProcAddress", &mut lib) {
        let f: FnGetProcAddress = unsafe { std::mem::transmute(f) };
        let mut out = String::from("egl procs:");
        for name in [
            "eglSetDamageRegionKHR",
            "eglSwapBuffersWithDamageKHR",
            "eglSwapBuffersWithDamageEXT",
            "eglQuerySurface",
            "eglSurfaceAttrib",
        ] {
            let c = std::ffi::CString::new(name).unwrap_or_default();
            let p = unsafe { f(c.as_ptr()) };
            out.push_str(&format!(" {name}={}", i32::from(!p.is_null())));
        }
        crate::log(&out);
    }

    if let (Some(f), false) = (resolve("eglQuerySurface", &mut lib), surface.is_null()) {
        let f: FnQuerySurface = unsafe { std::mem::transmute(f) };
        let get_error: Option<FnGetError> =
            resolve("eglGetError", &mut lib).map(|p| unsafe { std::mem::transmute(p) });
        let ask = |attr: c_int| -> (c_uint, c_int, c_int) {
            let mut v: c_int = -1;
            // Drain any stale error first, or a previous failure is reported against this query.
            if let Some(e) = get_error {
                unsafe { e() };
            }
            let ok = unsafe { f(dpy, surface, attr, &mut v) };
            let err = get_error.map_or(0, |e| unsafe { e() });
            (ok, v, err)
        };
        let (wok, w, _) = ask(EGL_WIDTH);
        let (hok, h, _) = ask(EGL_HEIGHT);
        let (bok, behavior, berr) = ask(EGL_SWAP_BEHAVIOR);
        let behavior_name = match behavior {
            EGL_BUFFER_PRESERVED => "BUFFER_PRESERVED",
            EGL_BUFFER_DESTROYED => "BUFFER_DESTROYED",
            _ => "?",
        };
        // The gate on the whole partial-update direction, and the one nothing else can answer.
        // `EGL_KHR_partial_update`: setting a damage region smaller than the whole buffer without
        // having queried the age is an error, and the age is what says how many frames of damage
        // a correct implementation must union.
        let (aok, age, aerr) = ask(EGL_BUFFER_AGE);
        crate::log(&format!(
            "egl surface: {w}x{h} (ok={wok}/{hok}) swap_behavior=0x{behavior:04x} \
             {behavior_name} (ok={bok} err=0x{berr:04x}) buffer_age={age} \
             (ok={aok} err=0x{aerr:04x})"
        ));
    }
    log_gl_extensions();
}

fn log_gl_extensions() {
    let p = unsafe { glGetString(GL_EXTENSIONS) };
    if p.is_null() {
        crate::log("gl extensions: <null>");
        return;
    }
    crate::log(&format!("gl extensions: {}", cstr(p)));
}
