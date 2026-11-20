//! The GLES2 entry points `plx_gfx` links against, forwarded to the `libGLESv2` this library opened.
//!
//! **Why this exists.** `plx_gfx` declares its GL calls as link-time `extern "C"` (`glClear`, `glDrawArrays`,
//! ...): in the application a Makefile links them against the television's libGLESv2. `libplx.so` must
//! not name a Tizen library in DT_NEEDED (the C# host's loader would refuse it on a device without that
//! exact name, and `build.sh` fails the build on any NEEDED outside the C runtime), and a Rust cdylib is
//! linked BIND_NOW: an unresolved `gl*` symbol makes `dlopen("libplx.so")` fail and takes the whole probe
//! with it (measured on the ARM build with `--features gfx`: 37 unresolved `gl*` names). So this file
//! DEFINES those names here, and each one jumps to the function pointer `dlsym`'d from the already
//! opened libGLESv2 ([`install`]).
//!
//! These are forwarders, not stubs: a call does exactly what the TV's GL does. An entry the library does
//! not export is NOT bound; [`install`] names it (the panel's "not found" line), `gfx_probe` then does
//! not enter `plx_gfx`, and a call that reaches an unbound forwarder anyway (a name added to `plx_gfx`
//! after this list was written) is counted and returns zero instead of jumping to address 0
//! ([`unbound_calls`]).
//!
//! The names are exported from `libplx.so` (a `#[no_mangle]` item in a cdylib is). That is harmless:
//! nothing `dlsym`s the global scope for a `gl*` name here ([`crate::dl::Lib::sym`] asks one library only),
//! and a forwarder calls through its table, never by name, so it cannot recurse into itself.
//!
//! The list is the union of every `gl*` that `rust-modules/gfx/src` and `rust-modules/base/src/surface.rs`
//! declare outside the `hostsim` / `devtriggers` features. `build.sh` fails the ARM build on any
//! unresolved name that is left (its "unexpected unresolved symbols" check), so a `plx_gfx` that grows a
//! new GL call fails loudly at build time and not on a television.

use std::ffi::{c_char, c_int, c_uint, c_void};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;

/// What an unbound forwarder returns: zero of its return type.
pub trait Zero {
    fn zero() -> Self;
}
impl Zero for () {
    fn zero() {}
}
impl Zero for c_uint {
    fn zero() -> Self {
        0
    }
}
impl Zero for c_int {
    fn zero() -> Self {
        0
    }
}
impl Zero for *const c_char {
    fn zero() -> Self {
        std::ptr::null()
    }
}

static UNBOUND_CALLS: AtomicU32 = AtomicU32::new(0);
static UNBOUND_FIRST: Mutex<Option<&'static str>> = Mutex::new(None);

/// A forwarder whose slot is empty was called.
fn unbound<T: Zero>(name: &'static str) -> T {
    UNBOUND_CALLS.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut first) = UNBOUND_FIRST.try_lock() {
        first.get_or_insert(name);
    }
    T::zero()
}

/// How many calls reached an unbound forwarder, and the first one's name.
pub fn unbound_calls() -> (u32, Option<&'static str>) {
    let first = UNBOUND_FIRST.lock().map(|g| *g).unwrap_or(None);
    (UNBOUND_CALLS.load(Ordering::Relaxed), first)
}

macro_rules! forwarders {
    ( $( fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) $(-> $ret:ty)? ; )* ) => {
        $(
            #[allow(non_snake_case)]
            mod $name {
                pub static SLOT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            }
        )*
        $(
            #[no_mangle]
            #[allow(non_snake_case, clippy::missing_safety_doc)]
            pub unsafe extern "C" fn $name( $($arg: $ty),* ) $(-> $ret)? {
                let p = $name::SLOT.load(Ordering::Relaxed);
                if p == 0 {
                    return unbound(stringify!($name));
                }
                // SAFETY: `install` stored the address of the library's function of this name, and
                // the signature here is the C prototype's.
                let f: unsafe extern "C" fn($($ty),*) $(-> $ret)? = std::mem::transmute(p);
                f($($arg),*)
            }
        )*
        /// Every forwarded name, in declaration order.
        pub const NAMES: &[&str] = &[$(stringify!($name)),*];
        static SLOTS: &[(&str, &AtomicUsize)] = &[$((stringify!($name), &$name::SLOT)),*];
    };
}

forwarders! {
    fn glGetString(name: c_uint) -> *const c_char;
    fn glGetIntegerv(name: c_uint, value: *mut c_int);
    fn glGetError() -> c_uint;
    fn glCreateShader(ty: c_uint) -> c_uint;
    fn glShaderSource(shader: c_uint, count: c_int, string: *const *const c_char, length: *const c_int);
    fn glCompileShader(shader: c_uint);
    fn glGetShaderiv(shader: c_uint, pname: c_uint, params: *mut c_int);
    fn glGetShaderInfoLog(shader: c_uint, bufsize: c_int, length: *mut c_int, infolog: *mut c_char);
    fn glCreateProgram() -> c_uint;
    fn glAttachShader(program: c_uint, shader: c_uint);
    fn glBindAttribLocation(program: c_uint, index: c_uint, name: *const c_char);
    fn glLinkProgram(program: c_uint);
    fn glGetProgramiv(program: c_uint, pname: c_uint, params: *mut c_int);
    fn glUseProgram(program: c_uint);
    fn glGetUniformLocation(program: c_uint, name: *const c_char) -> c_int;
    fn glUniform4f(loc: c_int, x: f32, y: f32, z: f32, w: f32);
    fn glUniform2f(loc: c_int, x: f32, y: f32);
    fn glUniform3f(loc: c_int, x: f32, y: f32, z: f32);
    fn glUniform1f(loc: c_int, x: f32);
    fn glUniform4fv(loc: c_int, count: c_int, value: *const f32);
    fn glUniform1i(loc: c_int, x: c_int);
    fn glGenBuffers(n: c_int, buffers: *mut c_uint);
    fn glBindBuffer(target: c_uint, buffer: c_uint);
    fn glBufferData(target: c_uint, size: isize, data: *const c_void, usage: c_uint);
    fn glEnableVertexAttribArray(index: c_uint);
    fn glVertexAttribPointer(index: c_uint, size: c_int, ty: c_uint, normalized: u8, stride: c_int, pointer: *const c_void);
    fn glDrawArrays(mode: c_uint, first: c_int, count: c_int);
    fn glBindTexture(target: c_uint, texture: c_uint);
    fn glActiveTexture(texture: c_uint);
    fn glEnable(cap: c_uint);
    fn glDisable(cap: c_uint);
    fn glScissor(x: c_int, y: c_int, w: c_int, h: c_int);
    fn glBlendFuncSeparate(src_rgb: c_uint, dst_rgb: c_uint, src_a: c_uint, dst_a: c_uint);
    fn glClearColor(r: f32, g: f32, b: f32, a: f32);
    fn glClear(mask: c_uint);
    fn glFinish();
    fn glFlush();
    fn glGenTextures(n: c_int, textures: *mut c_uint);
    fn glDeleteTextures(n: c_int, textures: *const c_uint);
    fn glPixelStorei(pname: c_uint, param: c_int);
    fn glTexImage2D(target: c_uint, level: c_int, ifmt: c_int, w: c_int, h: c_int, border: c_int, format: c_uint, ty: c_uint, pixels: *const c_void);
    fn glTexParameteri(target: c_uint, pname: c_uint, param: c_int);
    fn glGenFramebuffers(n: c_int, ids: *mut c_uint);
    fn glDeleteFramebuffers(n: c_int, ids: *const c_uint);
    fn glBindFramebuffer(target: c_uint, framebuffer: c_uint);
    fn glFramebufferTexture2D(target: c_uint, attachment: c_uint, textarget: c_uint, texture: c_uint, level: c_int);
    fn glCheckFramebufferStatus(target: c_uint) -> c_uint;
    fn glReadPixels(x: c_int, y: c_int, w: c_int, h: c_int, format: c_uint, ty: c_uint, pixels: *mut c_void);
    fn glCopyTexSubImage2D(target: c_uint, level: c_int, xoff: c_int, yoff: c_int, x: c_int, y: c_int, w: c_int, h: c_int);
    fn glViewport(x: c_int, y: c_int, w: c_int, h: c_int);
}

/// Bind every forwarder through `resolve` (name -> address); the names it could not resolve come back,
/// in declaration order, and their forwarders stay unbound. Calling it again re-binds.
pub fn install_with(mut resolve: impl FnMut(&str) -> Option<usize>) -> Vec<&'static str> {
    let mut missing = Vec::new();
    for (name, slot) in SLOTS {
        match resolve(name) {
            Some(addr) if addr != 0 => slot.store(addr, Ordering::Relaxed),
            _ => {
                slot.store(0, Ordering::Relaxed);
                missing.push(*name);
            }
        }
    }
    missing
}

/// Bind every forwarder to `lib`'s function of the same name. Returns the names `lib` does not export.
pub fn install(lib: &crate::dl::Lib) -> Vec<&'static str> {
    install_with(|name| lib.sym(name).map(|p| p as usize))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicI32;

    /// The forwarders share process-global slots; tests that bind or count hold this.
    static GUARD: Mutex<()> = Mutex::new(());

    static CLEARED_WITH: AtomicI32 = AtomicI32::new(-1);
    unsafe extern "C" fn fake_clear(mask: c_uint) {
        CLEARED_WITH.store(mask as i32, Ordering::SeqCst);
    }
    unsafe extern "C" fn fake_get_error() -> c_uint {
        0x0502
    }

    #[test]
    fn a_bound_forwarder_calls_the_function_it_was_bound_to_with_the_same_arguments() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let missing = install_with(|name| match name {
            "glClear" => Some(fake_clear as *const () as usize),
            "glGetError" => Some(fake_get_error as *const () as usize),
            _ => None,
        });
        assert!(missing.contains(&"glDrawArrays") && !missing.contains(&"glClear") && !missing.contains(&"glGetError"));
        // SAFETY: the forwarders are bound to the two fakes above.
        unsafe {
            glClear(0x4000);
            assert_eq!(glGetError(), 0x0502);
        }
        assert_eq!(CLEARED_WITH.load(Ordering::SeqCst), 0x4000);
    }

    #[test]
    fn an_unbound_forwarder_is_counted_and_returns_zero_instead_of_jumping_to_address_zero() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        install_with(|_| None);
        let (before, _) = unbound_calls();
        // SAFETY: unbound forwarders do not call anything.
        unsafe {
            glFlush();
            assert_eq!(glCreateProgram(), 0);
            assert!(glGetString(0x1F02).is_null());
        }
        let (after, first) = unbound_calls();
        assert_eq!(after - before, 3);
        assert!(first.is_some(), "the first unbound name is remembered");
    }

    #[test]
    fn install_names_every_entry_the_library_lacks_in_declaration_order() {
        let _g = GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let missing = install_with(|name| (name != "glReadPixels" && name != "glScissor").then_some(0x1000));
        assert_eq!(missing, vec!["glScissor", "glReadPixels"]);
        // A null address is "not exported", not a binding.
        let missing = install_with(|_| Some(0));
        assert_eq!(missing.len(), NAMES.len());
    }

    #[test]
    fn the_list_names_every_gl_entry_the_renderer_declares() {
        // The renderer's own declarations, read from its source: a name it gains must be added here
        // or this fails (and, on the ARM build, so does `build.sh`'s unresolved-symbol check).
        let mut declared = std::collections::BTreeSet::new();
        for src in [
            include_str!("../../../../rust-modules/gfx/src/gfx.rs"),
            include_str!("../../../../rust-modules/gfx/src/text.rs"),
            include_str!("../../../../rust-modules/gfx/src/egl.rs"),
            include_str!("../../../../rust-modules/gfx/src/gpu_timer.rs"),
        ] {
            for line in src.lines() {
                let t = line.trim_start();
                // `fn glX(` declarations inside `extern "C"` blocks, plus the `link_name = "glX"` aliases.
                let name = if let Some(rest) = t.strip_prefix("fn gl") {
                    rest.split('(').next().map(|n| format!("gl{n}"))
                } else {
                    t.strip_prefix("#[link_name = \"").and_then(|r| r.strip_suffix("\")]")).map(str::to_string)
                };
                if let Some(n) = name.filter(|n| n.starts_with("gl") && n.chars().all(|c| c.is_ascii_alphanumeric())) {
                    declared.insert(n);
                }
            }
        }
        // `glGenVertexArrays`/`glBindVertexArray` are hostsim-only (the TV's GLES2 has neither), the
        // dev-trigger `glFinish`/`glFlush` are forwarded anyway.
        declared.remove("glGenVertexArrays");
        declared.remove("glBindVertexArray");
        let have: std::collections::BTreeSet<String> = NAMES.iter().map(|n| n.to_string()).collect();
        let lacking: Vec<&String> = declared.iter().filter(|n| !have.contains(*n)).collect();
        assert!(lacking.is_empty(), "plx_gfx declares GL entries this shim does not forward: {lacking:?}");
    }
}
