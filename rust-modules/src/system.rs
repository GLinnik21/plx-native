//! Rust port of src/system.c — webOS/SDL platform glue (system.h). Same C ABI.
//! sys_grab_wayland grabs the wayland surface and clears its opaque region so the
//! Starfish video plane below shows through. CRITICAL: the TV's SDL fork writes a
//! LARGER SDL_SysWMinfo than the headers declare, so SDL_GetWindowWMInfo is given
//! a generous over-allocated buffer — this overrun smashing a bare struct was the
//! modular-split crash. wl_surface opcode 4 = set_opaque_region.
use std::os::raw::{c_int, c_uint, c_void};

const SDL_GL_ALPHA_SIZE: c_int = 3; // SDL_GLattr: RED=0,GREEN=1,BLUE=2,ALPHA=3
/// `SDL_GL_DEPTH_SIZE` / `SDL_GL_STENCIL_SIZE` — same enum, 6 and 7. `app.rs` asks for zero of
/// both; these read back what the driver actually granted, which is the only thing that settles it.
const SDL_GL_DEPTH_SIZE: c_int = 6;
const SDL_GL_STENCIL_SIZE: c_int = 7;
const GL_ALPHA_BITS: c_uint = 0x0D55;
const GL_RED_BITS: c_uint = 0x0D52;
const GL_DEPTH_BITS: c_uint = 0x0D56;
const GL_STENCIL_BITS: c_uint = 0x0D57;
// SDL_SysWMinfo layout (32-bit): version u8[3]@0, subsystem int@4, info union@8

extern "C" {
    // the wayland grab is `cfg(not(hostsim))` — desktop SDL has no webOS surface to reach for
    #[cfg_attr(feature = "hostsim", allow(dead_code))]
    fn SDL_GetWindowWMInfo(window: *mut c_void, info: *mut c_void) -> c_int;
    /// Fills `SDL_version` — three `Uint8`, major/minor/patch.
    fn SDL_GetVersion(ver: *mut u8);
    fn SDL_GL_GetAttribute(attr: c_int, value: *mut c_int) -> c_int;
    fn glGetIntegerv(pname: c_uint, params: *mut c_int);
}

// The three symbols that exist only on a television: wayland's proxy marshaller and glib's main
// context. The desktop simulator links neither — SDL owns its own event loop there, and there is
// no luna bus to pump — so they are declared apart rather than in the block above, which would
// otherwise fail the host link.
#[cfg(not(feature = "hostsim"))]
extern "C" {
    fn wl_proxy_marshal(proxy: *mut c_void, opcode: c_uint, ...);
    fn g_main_context_pending(ctx: *mut c_void) -> c_int;
    fn g_main_context_iteration(ctx: *mut c_void, may_block: c_int) -> c_int;
}

static mut G_WL_SURFACE: *mut c_void = std::ptr::null_mut();
static mut G_WL_DISPLAY: *mut c_void = std::ptr::null_mut();

pub(crate) fn clear_opaque_region() {
    unsafe {
        let surface = G_WL_SURFACE;
        if surface.is_null() {
            return;
        }
        // set_opaque_region(NULL): opcode 4 + one NULL region arg (variadic). The
        // commit is left to SDL_GL_SwapWindow (a bare commit here presents a
        // null-buffer surface and disrupts the slaved video plane).
        //
        // Nothing to do on the simulator: there is no video plane underneath to show through, so
        // a non-opaque surface would buy a desktop compositor nothing but per-frame blending.
        #[cfg(not(feature = "hostsim"))]
        wl_proxy_marshal(surface, 4, std::ptr::null_mut::<c_void>());
    }
}

/// Service the glib main context that luna-service2 replies arrive on.
///
/// A no-op on the simulator: glib is not linked and there is no luna bus to pump, so the loop
/// simply has nothing to service. Every caller stays unchanged.
pub(crate) fn ls2_pump() {
    #[cfg(not(feature = "hostsim"))]
    unsafe {
        let mut guard = 8;
        while guard > 0 && g_main_context_pending(std::ptr::null_mut()) != 0 {
            g_main_context_iteration(std::ptr::null_mut(), 0);
            guard -= 1;
        }
    }
}

pub(crate) fn sys_grab_wayland(winp: *mut c_void) {
    unsafe {
        let mut wmbuf = [0u8; 512];
        // SDL_VERSION(&wm->version): major/minor/patch (u8) at offset 0.
        //
        // ASK THE LIBRARY, do not hardcode. This declares which SDL_SysWMinfo layout the CALLER
        // was built against, and SDL checks it: from 2.0.6 on, `Wayland_GetWindowWMInfo` computes
        // major*1000000 + minor*10000 + patch and, if that is below 2000006, sets
        // subsystem = SDL_SYSWM_UNKNOWN and returns failure WITHOUT filling the union. This used
        // to say 2/0/4 — the version webOS 4.x ships — which is the right answer on exactly the
        // televisions the app has run on and wrong on every newer one: webOS 5.3.1/6.4.0 ship
        // SDL 2.0.10 and 7.4.0+ ship 2.0.14.
        //
        // The failure mode is why this matters more than it looks. A rejected call leaves
        // `G_WL_SURFACE` null, so `clear_opaque_region` silently does nothing, so the UI plane
        // stays opaque — and video decodes correctly, invisibly, underneath it. No error, no log
        // line, no crash: a black screen with working audio.
        //
        // Reporting the runtime version is safe in the other direction because the union's first
        // two members — `wl_display *display` then `wl_surface *surface` — have never moved;
        // later SDL versions only APPEND to that struct, which is also why the buffer is
        // over-allocated to 512 bytes (the TV's fork writes more than the headers declare).
        SDL_GetVersion(wmbuf.as_mut_ptr());
        let mut a: c_int = -1;
        let mut d: c_int = -1;
        let mut s: c_int = -1;
        SDL_GL_GetAttribute(SDL_GL_ALPHA_SIZE, &mut a);
        SDL_GL_GetAttribute(SDL_GL_DEPTH_SIZE, &mut d);
        SDL_GL_GetAttribute(SDL_GL_STENCIL_SIZE, &mut s);
        let mut abits: c_int = -1;
        let mut rbits: c_int = -1;
        let mut dbits: c_int = -1;
        let mut sbits: c_int = -1;
        glGetIntegerv(GL_ALPHA_BITS, &mut abits);
        glGetIntegerv(GL_RED_BITS, &mut rbits);
        // Deprecated in a desktop CORE profile (the simulator), where they leave the value alone
        // and raise `GL_INVALID_ENUM` — harmless and once, at boot, and the SDL attributes above
        // answer the same question portably. On the television's ES2 context both are legal.
        glGetIntegerv(GL_DEPTH_BITS, &mut dbits);
        glGetIntegerv(GL_STENCIL_BITS, &mut sbits);
        // `depth=`/`stencil=` are here to be READ: `app.rs` asks for zero of each because nothing
        // in this renderer uses them, and on a tiler a granted depth buffer is a per-frame
        // write-back of 1920x1080x2 bytes nobody consumes. A non-zero here means the driver
        // refused and the saving is not real.
        log(&format!(
            "FB bits: alpha={abits} red={rbits} depth={dbits} stencil={sbits} \
             (config alpha={a} depth={d} stencil={s})"
        ));
        // The wayland grab is webOS-only, and on a desktop it is not merely useless but UNSOUND.
        // The union is read as `*mut c_void` pairs at a 4-byte offset, which is fine for the
        // television's 32-bit pointers and a misaligned 64-bit dereference anywhere else — the
        // simulator aborted here with "address must be a multiple of 0x8" before drawing a frame.
        // There is also nothing to grab: SDL's cocoa backend reports SDL_SYSWM_COCOA, no wayland
        // surface exists, and no video plane sits underneath needing to show through.
        #[cfg(not(feature = "hostsim"))]
        if SDL_GetWindowWMInfo(winp, wmbuf.as_mut_ptr() as *mut c_void) != 0 {
            // info union @ offset 8: {wl_display*, wl_surface*, ...}; members
            // share offset 0, so read the first two pointers directly.
            let info = wmbuf.as_ptr().add(8) as *const *mut c_void;
            G_WL_DISPLAY = *info.add(0);
            G_WL_SURFACE = *info.add(1);
        }
        #[cfg(feature = "hostsim")]
        let _ = winp;
        let subsystem = i32::from_ne_bytes([wmbuf[4], wmbuf[5], wmbuf[6], wmbuf[7]]);
        let (surf, disp) = (G_WL_SURFACE, G_WL_DISPLAY);
        // The version bytes are still in wmbuf[0..3] — SDL_GetWindowWMInfo validates them and
        // writes only `subsystem` and the union, so there is nothing to re-read.
        log(&format!(
            "wm sdl={}.{}.{} subsys={subsystem} wl_surface={surf:p} wl_display={disp:p} alpha={a}",
            wmbuf[0], wmbuf[1], wmbuf[2]
        ));
        // Loud, because the consequence is a black screen with working audio and nothing else in
        // the log would say why. SDL_SYSWM_WAYLAND is 6 in SDL2's enum; anything else here means
        // we did not get a surface and the video plane will stay hidden under an opaque UI.
        if surf.is_null() && !cfg!(feature = "hostsim") {
            log("wm: NO wl_surface — the UI plane cannot be made transparent, so video will \
                 decode invisibly beneath it. Check the SDL_SysWMinfo version handshake.");
        }
        clear_opaque_region();
    }
}

use crate::log;
