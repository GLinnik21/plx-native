//! Rust port of src/system.c — webOS/SDL platform glue (system.h). Same C ABI.
//! sys_grab_wayland grabs the wayland surface and clears its opaque region so the
//! Starfish video plane below shows through. CRITICAL: the TV's SDL fork writes a
//! LARGER SDL_SysWMinfo than the headers declare, so SDL_GetWindowWMInfo is given
//! a generous over-allocated buffer — this overrun smashing a bare struct was the
//! modular-split crash. wl_surface opcode 4 = set_opaque_region.
use std::os::raw::{c_int, c_uint, c_void};

// SDL 2.0.4 (the TV's SDL); SDL_GetWindowWMInfo checks these version bytes.
const SDL_MAJOR: u8 = 2;
const SDL_MINOR: u8 = 0;
const SDL_PATCH: u8 = 4;
const SDL_GL_ALPHA_SIZE: c_int = 3; // SDL_GLattr: RED=0,GREEN=1,BLUE=2,ALPHA=3
const GL_ALPHA_BITS: c_uint = 0x0D55;
const GL_RED_BITS: c_uint = 0x0D52;
// SDL_SysWMinfo layout (32-bit): version u8[3]@0, subsystem int@4, info union@8

extern "C" {
    fn wl_proxy_marshal(proxy: *mut c_void, opcode: c_uint, ...);
    fn SDL_GetWindowWMInfo(window: *mut c_void, info: *mut c_void) -> c_int;
    fn SDL_GL_GetAttribute(attr: c_int, value: *mut c_int) -> c_int;
    fn glGetIntegerv(pname: c_uint, params: *mut c_int);
    fn g_main_context_pending(ctx: *mut c_void) -> c_int;
    fn g_main_context_iteration(ctx: *mut c_void, may_block: c_int) -> c_int;
}

static mut G_WL_SURFACE: *mut c_void = std::ptr::null_mut();
static mut G_WL_DISPLAY: *mut c_void = std::ptr::null_mut();

#[no_mangle]
pub extern "C" fn clear_opaque_region() {
    unsafe {
        let surface = G_WL_SURFACE;
        if surface.is_null() {
            return;
        }
        // set_opaque_region(NULL): opcode 4 + one NULL region arg (variadic). The
        // commit is left to SDL_GL_SwapWindow (a bare commit here presents a
        // null-buffer surface and disrupts the slaved video plane).
        wl_proxy_marshal(surface, 4, std::ptr::null_mut::<c_void>());
    }
}

#[no_mangle]
pub extern "C" fn ls2_pump() {
    unsafe {
        let mut guard = 8;
        while guard > 0 && g_main_context_pending(std::ptr::null_mut()) != 0 {
            g_main_context_iteration(std::ptr::null_mut(), 0);
            guard -= 1;
        }
    }
}

#[no_mangle]
pub extern "C" fn sys_grab_wayland(winp: *mut c_void) {
    unsafe {
        let mut wmbuf = [0u8; 512];
        // SDL_VERSION(&wm->version): major/minor/patch (u8) at offset 0
        wmbuf[0] = SDL_MAJOR;
        wmbuf[1] = SDL_MINOR;
        wmbuf[2] = SDL_PATCH;
        let mut a: c_int = -1;
        SDL_GL_GetAttribute(SDL_GL_ALPHA_SIZE, &mut a);
        let mut abits: c_int = -1;
        let mut rbits: c_int = -1;
        glGetIntegerv(GL_ALPHA_BITS, &mut abits);
        glGetIntegerv(GL_RED_BITS, &mut rbits);
        log(&format!("FB bits: alpha={abits} red={rbits} (config alpha={a})"));
        if SDL_GetWindowWMInfo(winp, wmbuf.as_mut_ptr() as *mut c_void) != 0 {
            // info union @ offset 8: {wl_display*, wl_surface*, ...}; members
            // share offset 0, so read the first two pointers directly.
            let info = wmbuf.as_ptr().add(8) as *const *mut c_void;
            G_WL_DISPLAY = *info.add(0);
            G_WL_SURFACE = *info.add(1);
        }
        let subsystem = i32::from_ne_bytes([wmbuf[4], wmbuf[5], wmbuf[6], wmbuf[7]]);
        let (surf, disp) = (G_WL_SURFACE, G_WL_DISPLAY);
        log(&format!("wm subsys={subsystem} wl_surface={surf:p} wl_display={disp:p} alpha={a}"));
        clear_opaque_region();
    }
}

fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}
