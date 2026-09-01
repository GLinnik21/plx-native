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
const EGL_CONFIG_ID: c_int = 0x3028;
const EGL_SURFACE_TYPE: c_int = 0x3033;
const EGL_NONE: c_int = 0x3038;
/// `EGL_SWAP_BEHAVIOR_PRESERVED_BIT` in a config's `EGL_SURFACE_TYPE` mask. Without it,
/// `eglSurfaceAttrib(EGL_SWAP_BEHAVIOR, EGL_BUFFER_PRESERVED)` cannot succeed on any surface of
/// that config — which is the whole "keep the previous frame and repair it" family in one bit.
const EGL_SWAP_BEHAVIOR_PRESERVED_BIT: c_int = 0x0400;
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
type FnGetCurrentContext = unsafe extern "C" fn() -> *mut c_void;
type FnQueryContext = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint;
type FnChooseConfig =
    unsafe extern "C" fn(*mut c_void, *const c_int, *mut *mut c_void, c_int, *mut c_int) -> c_uint;
type FnGetConfigAttrib =
    unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, *mut c_int) -> c_uint;
type FnSurfaceAttrib = unsafe extern "C" fn(*mut c_void, *mut c_void, c_int, c_int) -> c_uint;

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
        crate::log(&format!(
            "egl extensions: {}",
            cstr(unsafe { f(dpy, EGL_EXTENSIONS) })
        ));
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
        // Can this surface's CONFIG even offer buffer preservation? Without
        // EGL_SWAP_BEHAVIOR_PRESERVED_BIT the `eglSurfaceAttrib` route is closed by the config,
        // not by policy, and no amount of asking will open it.
        probe_config(dpy, &mut lib);
        // Only with `/tmp/plxnative-eglprobe`, because it MUTATES the live surface: ask for
        // EGL_BUFFER_PRESERVED, read back what we got, and put it back the way SDL had it.
        // Empirical, because a config bit and a driver's answer have disagreed before.
        if crate::dev::flag("eglprobe") {
            try_preserve(dpy, surface, &mut lib);
            try_damage(dpy, surface, &mut lib);
        }
        damage_init(dpy, surface, &mut lib);
        // Remember the handles so `late_probe` can ask again once frames have actually been
        // presented — an age queried before the first swap is 0 by definition and says nothing.
        unsafe {
            LATE_DPY = dpy;
            LATE_SURFACE = surface;
        }
    }
    log_gl_extensions();
}

/// The config behind the current context, and whether it can preserve a swapped buffer.
fn probe_config(dpy: *mut c_void, lib: &mut Option<Handle>) {
    let (Some(get_ctx), Some(query_ctx), Some(choose), Some(get_attr)) = (
        resolve("eglGetCurrentContext", lib),
        resolve("eglQueryContext", lib),
        resolve("eglChooseConfig", lib),
        resolve("eglGetConfigAttrib", lib),
    ) else {
        return;
    };
    let get_ctx: FnGetCurrentContext = unsafe { std::mem::transmute(get_ctx) };
    let query_ctx: FnQueryContext = unsafe { std::mem::transmute(query_ctx) };
    let choose: FnChooseConfig = unsafe { std::mem::transmute(choose) };
    let get_attr: FnGetConfigAttrib = unsafe { std::mem::transmute(get_attr) };
    let ctx = unsafe { get_ctx() };
    let mut id: c_int = -1;
    if ctx.is_null() || unsafe { query_ctx(dpy, ctx, EGL_CONFIG_ID, &mut id) } == 0 {
        return;
    }
    // Ask for that ONE config by id. `eglChooseConfig` with EGL_CONFIG_ID is the documented way
    // back from an id to an EGLConfig; there is no eglGetConfigById.
    let attribs = [EGL_CONFIG_ID, id, EGL_NONE];
    let mut config: *mut c_void = std::ptr::null_mut();
    let mut n: c_int = 0;
    if unsafe { choose(dpy, attribs.as_ptr(), &mut config, 1, &mut n) } == 0 || n < 1 {
        crate::log(&format!("egl config: id={id} could not be re-selected"));
        return;
    }
    let mut surface_type: c_int = 0;
    let ok = unsafe { get_attr(dpy, config, EGL_SURFACE_TYPE, &mut surface_type) };
    let preserved = surface_type & EGL_SWAP_BEHAVIOR_PRESERVED_BIT != 0;
    crate::log(&format!(
        "egl config: id={id} surface_type=0x{surface_type:04x} (ok={ok})          SWAP_BEHAVIOR_PRESERVED_BIT={}",
        i32::from(preserved)
    ));
}

/// Ask the live surface for `EGL_BUFFER_PRESERVED`, report what it says, and put it back.
///
/// Mutating, so it is trigger-gated. The restore is unconditional — leaving a surface preserved
/// would silently change every later frame's tile handling, which is precisely the confound this
/// probe exists to avoid introducing.
fn try_preserve(dpy: *mut c_void, surface: *mut c_void, lib: &mut Option<Handle>) {
    let (Some(set), Some(query)) = (
        resolve("eglSurfaceAttrib", lib),
        resolve("eglQuerySurface", lib),
    ) else {
        return;
    };
    let set: FnSurfaceAttrib = unsafe { std::mem::transmute(set) };
    let query: FnQuerySurface = unsafe { std::mem::transmute(query) };
    let get_error: Option<FnGetError> =
        resolve("eglGetError", lib).map(|p| unsafe { std::mem::transmute(p) });
    if let Some(e) = get_error {
        unsafe { e() };
    }
    let ok = unsafe { set(dpy, surface, EGL_SWAP_BEHAVIOR, EGL_BUFFER_PRESERVED) };
    let err = get_error.map_or(0, |e| unsafe { e() });
    let mut got: c_int = -1;
    unsafe { query(dpy, surface, EGL_SWAP_BEHAVIOR, &mut got) };
    crate::log(&format!(
        "egl preserve: eglSurfaceAttrib(BUFFER_PRESERVED) ok={ok} err=0x{err:04x}          readback=0x{got:04x} ({})",
        if got == EGL_BUFFER_PRESERVED { "PRESERVED" } else { "DESTROYED" }
    ));
    unsafe { set(dpy, surface, EGL_SWAP_BEHAVIOR, EGL_BUFFER_DESTROYED) };
}

/// The EGL error codes this probe can provoke, by name. A bare `0x3009` in a log is a number
/// somebody has to go and look up, and the difference between BAD_MATCH ("the driver understood
/// and refused") and BAD_ACCESS or a segfault ("it does not implement this at all") is the whole
/// point of asking.
fn egl_error_name(code: c_int) -> &'static str {
    match code {
        0x3000 => "EGL_SUCCESS",
        0x3001 => "EGL_NOT_INITIALIZED",
        0x3002 => "EGL_BAD_ACCESS",
        0x3003 => "EGL_BAD_ALLOC",
        0x3004 => "EGL_BAD_ATTRIBUTE",
        0x3005 => "EGL_BAD_CONFIG",
        0x3006 => "EGL_BAD_CONTEXT",
        0x3007 => "EGL_BAD_CURRENT_SURFACE",
        0x3008 => "EGL_BAD_DISPLAY",
        0x3009 => "EGL_BAD_MATCH",
        0x300A => "EGL_BAD_NATIVE_PIXMAP",
        0x300B => "EGL_BAD_NATIVE_WINDOW",
        0x300C => "EGL_BAD_PARAMETER",
        0x300D => "EGL_BAD_SURFACE",
        _ => "?",
    }
}

/// Call the two damage entry points and report what the driver says.
///
/// They are **not in this display's extension string** — but `eglGetProcAddress` returns a
/// non-NULL pointer for both, and `EGL_KHR_get_all_proc_addresses` IS advertised, which is exactly
/// the condition under which a resolvable address proves nothing. The EGL 1.4 spec is explicit
/// that `eglGetProcAddress` may answer for entry points the implementation does not support, so
/// the only way past "the name resolves" is to call it and read `eglGetError`.
///
/// Trigger-gated, because it makes a real request against the live surface and issues a swap.
/// Done at boot, before the first frame, which is also the only moment `eglSetDamageRegionKHR`
/// is legal by its own spec ("before any client API rendering command since the last swap").
fn try_damage(dpy: *mut c_void, surface: *mut c_void, lib: &mut Option<Handle>) {
    let Some(gpa) = resolve("eglGetProcAddress", lib) else {
        return;
    };
    let gpa: FnGetProcAddress = unsafe { std::mem::transmute(gpa) };
    let get_error: Option<FnGetError> =
        resolve("eglGetError", lib).map(|p| unsafe { std::mem::transmute(p) });
    let err = || get_error.map_or(0, |e| unsafe { e() });
    let clear = || {
        if let Some(e) = get_error {
            unsafe { e() };
        }
    };
    // Bottom-left origin, per both damage specs. One small rect, deliberately NOT the whole
    // buffer — the whole buffer is the one case partial_update allows without a buffer age.
    let rects: [c_int; 4] = [0, 0, 64, 64];

    let p = unsafe { gpa(c"eglSetDamageRegionKHR".as_ptr()) };
    if !p.is_null() {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int, c_int) -> c_uint =
            unsafe { std::mem::transmute(p) };
        clear();
        let ok = unsafe { f(dpy, surface, rects.as_ptr(), 1) };
        let e = err();
        crate::log(&format!(
            "egl damage: eglSetDamageRegionKHR(0,0,64,64) ok={ok} err=0x{e:04x} {}",
            egl_error_name(e)
        ));
    }
    let p = unsafe { gpa(c"eglSwapBuffersWithDamageKHR".as_ptr()) };
    if !p.is_null() {
        let f: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int, c_int) -> c_uint =
            unsafe { std::mem::transmute(p) };
        clear();
        let ok = unsafe { f(dpy, surface, rects.as_ptr(), 1) };
        let e = err();
        crate::log(&format!(
            "egl damage: eglSwapBuffersWithDamageKHR(0,0,64,64) ok={ok} err=0x{e:04x} {}",
            egl_error_name(e)
        ));
    }
}

// ---------------------------------------------------------------------------------------------
// EXPERIMENT (`/tmp/plxnative-egldamage[=WxH]`): is the unadvertised damage region REAL?
// ---------------------------------------------------------------------------------------------
//
// `eglSetDamageRegionKHR` resolves and returns `EGL_TRUE`/`EGL_SUCCESS` on this driver, but
// `EGL_KHR_partial_update` is NOT in `EGL_EXTENSIONS` — and a stub that accepts everything and
// does nothing is indistinguishable from a working implementation by return code alone. So do not
// ask it; measure it. Each frame this declares a small damage rect and then draws the WHOLE
// screen as usual. If the driver honours the rect, the tiles outside it are never rasterized:
// `FRAG_ACTIVE`/`ARITH_WORDS`/`GPU_ACTIVE` must collapse, and the picture outside the rect must
// visibly go stale or garbage. If the counters do not move and the picture is perfect, the entry
// point is a no-op and the whole partial-update direction is closed on this television.
//
// DELIBERATELY DESTRUCTIVE, which is the point: a correct dirty-rect renderer would draw only
// inside the rect, and then a wrong picture would prove nothing. Drawing everything makes the
// driver's behaviour the only variable.
static mut DMG_QUERY: Option<FnQuerySurface> = None;
static mut DMG_SET: Option<
    unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_int, c_int) -> c_uint,
> = None;
static mut DMG_RECT: [c_int; 4] = [0, 0, 0, 0];
static mut DMG_FULL: [c_int; 4] = [0, 0, 0, 0];
static mut DMG_WARMUP: u32 = 0;
/// Frames of FULL damage before the sub-rect starts. Not a nicety: declaring a sub-rect from the
/// very first frame left the panel showing the boot splash forever — the app's own picture never
/// appeared at all, because no frame ever declared the whole surface valid. That is the "default
/// must be full damage" rule of any shippable version, arrived at from the wrong end.
const DMG_WARMUP_FRAMES: u32 = 180;

/// Resolve the damage entry points once, if `/tmp/plxnative-egldamage` is armed. `WxH` in the
/// trigger sets the rect (default 480x270, a sixteenth of the panel), anchored bottom-left
/// because both damage specs use GL's origin, not the authored top-left one.
fn damage_init(dpy: *mut c_void, surface: *mut c_void, lib: &mut Option<Handle>) {
    let Some(spec) = crate::dev::read("egldamage") else {
        return;
    };
    let (w, h) = spec
        .split_once('x')
        .and_then(|(a, b)| Some((a.trim().parse().ok()?, b.trim().parse().ok()?)))
        .unwrap_or((480, 270));
    let (Some(gpa), Some(query)) = (
        resolve("eglGetProcAddress", lib),
        resolve("eglQuerySurface", lib),
    ) else {
        return;
    };
    let gpa: FnGetProcAddress = unsafe { std::mem::transmute(gpa) };
    let set = unsafe { gpa(c"eglSetDamageRegionKHR".as_ptr()) };
    if set.is_null() {
        crate::log("egldamage: eglSetDamageRegionKHR did not resolve — experiment not armed");
        return;
    }
    let (fw, fh) = (
        crate::surface::LOGICAL_W as c_int,
        crate::surface::LOGICAL_H as c_int,
    );
    unsafe {
        DMG_QUERY = Some(std::mem::transmute(query));
        DMG_SET = Some(std::mem::transmute(set));
        DMG_RECT = [0, 0, w, h];
        DMG_FULL = [0, 0, fw, fh];
        DMG_WARMUP = 0;
        LATE_DPY = dpy;
        LATE_SURFACE = surface;
    }
    crate::log(&format!(
        "egldamage: ARMED — {DMG_WARMUP_FRAMES} frames of full damage, then {w}x{h} at (0,0)"
    ));
}

/// Declare this frame's damage. **Must run before any rendering command since the last swap**,
/// which is what `EGL_KHR_partial_update` requires and why the call site is the first statement
/// of the present block rather than anywhere near the draws. Queries the buffer age first for the
/// same reason: the spec makes a sub-buffer damage region an error without it.
///
/// One `static` read and a return when the trigger is absent.
pub(crate) fn frame_damage() {
    unsafe {
        let (Some(query), Some(set)) = (DMG_QUERY, DMG_SET) else {
            return;
        };
        let mut age: c_int = 0;
        query(LATE_DPY, LATE_SURFACE, EGL_BUFFER_AGE, &mut age);
        let rect = if DMG_WARMUP < DMG_WARMUP_FRAMES {
            DMG_WARMUP += 1;
            if DMG_WARMUP == DMG_WARMUP_FRAMES {
                crate::log("egldamage: warm-up over — narrowing to the sub-rect now");
            }
            std::ptr::addr_of!(DMG_FULL)
        } else {
            std::ptr::addr_of!(DMG_RECT)
        };
        set(LATE_DPY, LATE_SURFACE, rect.cast::<c_int>(), 1);
    }
}

static mut LATE_DPY: *mut c_void = std::ptr::null_mut();
static mut LATE_SURFACE: *mut c_void = std::ptr::null_mut();
static mut LATE_FRAMES: u32 = 0;

/// Re-ask for `EGL_BUFFER_AGE` once frames have really been presented.
///
/// The boot reading is 0 by construction — before the first `eglSwapBuffers` the back buffer has
/// no history — so it cannot distinguish "this driver does not track age" from "there is no age
/// yet". After a hundred presents the two are distinguishable, and the answer decides whether a
/// damage region could ever be legal here: `EGL_KHR_partial_update` makes it an error to set one
/// smaller than the whole buffer without a queried age. Costs one increment per presented frame
/// and then nothing at all.
pub(crate) fn late_probe() {
    unsafe {
        if LATE_FRAMES > 120 || LATE_DPY.is_null() {
            return;
        }
        LATE_FRAMES += 1;
        if LATE_FRAMES != 120 {
            return;
        }
        let mut lib: Option<Handle> = None;
        let (Some(query), Some(err)) = (
            resolve("eglQuerySurface", &mut lib),
            resolve("eglGetError", &mut lib),
        ) else {
            return;
        };
        let query: FnQuerySurface = std::mem::transmute(query);
        let err: FnGetError = std::mem::transmute(err);
        err();
        let mut age: c_int = -1;
        let ok = query(LATE_DPY, LATE_SURFACE, EGL_BUFFER_AGE, &mut age);
        let e = err();
        crate::log(&format!(
            "egl surface (after 120 presents): buffer_age={age} ok={ok} err=0x{e:04x}"
        ));
    }
}

fn log_gl_extensions() {
    let p = unsafe { glGetString(GL_EXTENSIONS) };
    if p.is_null() {
        crate::log("gl extensions: <null>");
        return;
    }
    crate::log(&format!("gl extensions: {}", cstr(p)));
}
