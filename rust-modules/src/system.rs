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

// ------------------------------------------------------------------------------------------
// EXPERIMENT (`/tmp/plxnative-opaque`): declare the UI surface OPAQUE where nothing is behind it
// ------------------------------------------------------------------------------------------
//
// `docs/perf-damage-tracking-verdict.md` §5 is the design. The whole app has run with
// `set_opaque_region(NULL)` since boot — on Home, Library, Detail, Person, Login and Profiles,
// where there is no video plane underneath and nothing to show through. The protocol calls the
// opaque region "an optimization hint for the compositor that lets it optimize the redrawing of
// content behind opaque regions", and a sibling measured LG's `surface-manager` at 34.4% of every
// frame's GPU cycles, doing exactly one full-screen textured blit of our surface. Whether that
// charge is per-COMMIT bookkeeping or per-BLENDED-PIXEL is the question, and nobody has varied the
// variable: all four prior compositor measurements held the surface non-opaque at 1920x1080.
//
// **Default behaviour is byte-identical.** Without the trigger `region_init` never runs, `ENABLED`
// stays false, and `opaque_route` returns on its first load.
//
// **Route-scoped, and it must be.** Marking the surface opaque while the hardware video plane is
// slaved beneath it would occlude a plane that has not torn down yet — which is why the player
// route re-asserts NULL every frame today. `opaque_route(player)` therefore asserts NULL on the
// player route and the full region everywhere else, and remembers which it last sent so an
// unchanged route costs one atomic load and no protocol traffic (the region is double-buffered
// but otherwise STICKY: "the pending and current regions are never changed" otherwise).
//
// **No new link dependency, deliberately.** A `wl_region` needs `wl_compositor.create_region`, and
// the only wayland objects this app has are the display and surface SDL handed it — so the
// compositor has to come from a registry bind. `wl_proxy_marshal_constructor_versioned` is absent
// from `libwayland-client` on webOS below 4.4.2 and the `*_interface` data symbols would be three
// more `DT_NEEDED` requirements, so every one of them is resolved through `RTLD_DEFAULT` against
// the libwayland-client already linked and loaded. `tools/fwcompat.py` is unchanged by this file.

/// `wl_registry.bind` / `wl_compositor.create_region` / `wl_region.add` — opcodes from
/// `wayland-client-protocol.h` (`WL_REGISTRY_BIND 0`, `WL_COMPOSITOR_CREATE_REGION 1`,
/// `WL_REGION_ADD 1`), beside `WL_SURFACE_SET_OPAQUE_REGION 4` which this file already used.
#[cfg(not(feature = "hostsim"))]
const WL_REGISTRY_BIND: c_uint = 0;
#[cfg(not(feature = "hostsim"))]
const WL_DISPLAY_GET_REGISTRY: c_uint = 1;
#[cfg(not(feature = "hostsim"))]
const WL_COMPOSITOR_CREATE_REGION: c_uint = 1;
#[cfg(not(feature = "hostsim"))]
const WL_REGION_ADD: c_uint = 1;
#[cfg(not(feature = "hostsim"))]
const WL_SURFACE_SET_OPAQUE_REGION: c_uint = 4;

/// The full-surface `wl_region`, created once at boot. Never per frame — a region is a server
/// object and allocating one every present would be protocol traffic on the frame path.
#[cfg(not(feature = "hostsim"))]
static mut G_WL_REGION: *mut c_void = std::ptr::null_mut();
/// What we last told the compositor: `1` = the full region, `0` = NULL, `-1` = nothing yet.
#[cfg(not(feature = "hostsim"))]
static mut G_OPAQUE_SENT: i8 = -1;
/// Set only by [`opaque_region_init`], and only when the trigger armed AND a region was built.
#[cfg(not(feature = "hostsim"))]
static mut G_OPAQUE_ENABLED: bool = false;
/// Scratch for the registry callback: the `wl_compositor` proxy it binds.
#[cfg(not(feature = "hostsim"))]
static mut G_WL_COMPOSITOR: *mut c_void = std::ptr::null_mut();

#[cfg(not(feature = "hostsim"))]
#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, *mut c_void, c_uint, *const std::ffi::c_char, c_uint),
    global_remove: unsafe extern "C" fn(*mut c_void, *mut c_void, c_uint),
}

/// A libwayland symbol from the process's own scope. `libwayland-client` is linked normally, so it
/// is already mapped; this only avoids naming these particular symbols in `DT_NEEDED`.
#[cfg(not(feature = "hostsim"))]
fn wl_sym(name: &str) -> Option<*mut c_void> {
    crate::dynlib::Handle::self_handle().sym(name).filter(|p| !p.is_null())
}

#[cfg(not(feature = "hostsim"))]
unsafe extern "C" fn on_global_remove(_data: *mut c_void, _reg: *mut c_void, _name: c_uint) {}

#[cfg(not(feature = "hostsim"))]
unsafe extern "C" fn on_global(
    _data: *mut c_void,
    registry: *mut c_void,
    name: c_uint,
    interface: *const std::ffi::c_char,
    version: c_uint,
) {
    // First wins: the compositor is advertised once, and a second bind would leak a proxy.
    if interface.is_null() || !unsafe { G_WL_COMPOSITOR }.is_null() {
        return;
    }
    let which = unsafe { std::ffi::CStr::from_ptr(interface) };
    if which.to_bytes() != b"wl_compositor" {
        return;
    }
    // Bind version 1: `create_region` is `WL_COMPOSITOR_CREATE_REGION_SINCE_VERSION 1`, and asking
    // for more than the compositor advertises is a protocol error that kills the connection.
    let want = version.min(1);
    let (Some(iface), Some(bind)) =
        (wl_sym("wl_compositor_interface"), wl_sym("wl_proxy_marshal_constructor_versioned"))
    else {
        log("opaque: libwayland has no versioned constructor — cannot bind wl_compositor");
        return;
    };
    // `struct wl_interface`'s first member is `const char *name`; `wl_registry_bind` passes it as
    // the third variadic argument. Read it rather than spelling the string twice.
    let iface_name = unsafe { *(iface as *const *const std::ffi::c_char) };
    let bind: unsafe extern "C" fn(
        *mut c_void,
        c_uint,
        *const c_void,
        c_uint,
        ...
    ) -> *mut c_void = unsafe { std::mem::transmute(bind) };
    let proxy = unsafe {
        bind(registry, WL_REGISTRY_BIND, iface, want, name, iface_name, want, std::ptr::null_mut::<c_void>())
    };
    unsafe { G_WL_COMPOSITOR = proxy };
    log(&format!("opaque: bound wl_compositor v{want} (advertised v{version}) proxy={proxy:p}"));
}

/// Build the full-surface opaque region, once, at boot. No-op unless `/tmp/plxnative-opaque` is
/// armed; returns without touching the surface's current (NULL) region either way.
#[cfg(not(feature = "hostsim"))]
pub(crate) fn opaque_region_init() {
    if !crate::dev::flag("opaque") {
        return;
    }
    let (display, surface) = unsafe { (G_WL_DISPLAY, G_WL_SURFACE) };
    if display.is_null() || surface.is_null() {
        log("opaque: no wl_display/wl_surface — experiment not armed");
        return;
    }
    let (Some(ctor), Some(add_listener), Some(roundtrip), Some(destroy), Some(reg_iface)) = (
        wl_sym("wl_proxy_marshal_constructor"),
        wl_sym("wl_proxy_add_listener"),
        wl_sym("wl_display_roundtrip"),
        wl_sym("wl_proxy_destroy"),
        wl_sym("wl_registry_interface"),
    ) else {
        log("opaque: libwayland is missing a registry entry point — experiment not armed");
        return;
    };
    let ctor: unsafe extern "C" fn(*mut c_void, c_uint, *const c_void, ...) -> *mut c_void =
        unsafe { std::mem::transmute(ctor) };
    let add_listener: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int =
        unsafe { std::mem::transmute(add_listener) };
    let roundtrip: unsafe extern "C" fn(*mut c_void) -> c_int =
        unsafe { std::mem::transmute(roundtrip) };
    let destroy: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(destroy) };

    // The DEFAULT queue, on the main thread, at boot, with no SDL call in flight. A private queue
    // would be tidier, but a roundtrip here dispatches exactly the events SDL's own listeners
    // would have taken on the next `SDL_PumpEvents` — which is the same thing SDL does itself
    // while creating the window.
    static LISTENER: RegistryListener =
        RegistryListener { global: on_global, global_remove: on_global_remove };
    let registry = unsafe {
        ctor(display, WL_DISPLAY_GET_REGISTRY, reg_iface, std::ptr::null_mut::<c_void>())
    };
    if registry.is_null() {
        log("opaque: wl_display.get_registry returned NULL — experiment not armed");
        return;
    }
    unsafe {
        add_listener(registry, std::ptr::from_ref(&LISTENER).cast::<c_void>(), std::ptr::null_mut());
        roundtrip(display);
    }
    let compositor = unsafe { G_WL_COMPOSITOR };
    unsafe { destroy(registry) };
    if compositor.is_null() {
        log("opaque: wl_compositor was never advertised — experiment not armed");
        return;
    }

    let Some(region_iface) = wl_sym("wl_region_interface") else {
        log("opaque: libwayland has no wl_region_interface — experiment not armed");
        return;
    };
    // Surface-local coordinates. `viewport()` is the rect the renderer actually draws into, which
    // is the whole 1920x1080 surface on every set seen so far and is the honest answer on a
    // letterboxed one, where the bars are not ours to claim.
    let (vx, vy, vw, vh) = crate::surface::viewport();
    let region = unsafe {
        let r = ctor(
            compositor,
            WL_COMPOSITOR_CREATE_REGION,
            region_iface,
            std::ptr::null_mut::<c_void>(),
        );
        if !r.is_null() {
            wl_proxy_marshal(r, WL_REGION_ADD, vx, vy, vw, vh);
        }
        r
    };
    if region.is_null() {
        log("opaque: wl_compositor.create_region returned NULL — experiment not armed");
        return;
    }
    unsafe {
        G_WL_REGION = region;
        G_OPAQUE_ENABLED = true;
    }
    log(&format!("opaque: ARMED — full-surface opaque region {vw}x{vh} at ({vx},{vy})"));
}

#[cfg(feature = "hostsim")]
pub(crate) fn opaque_region_init() {}

/// Assert the opaque region appropriate to this route, if the experiment is armed.
///
/// A no-op — one read of a `static` and a return — whenever `/tmp/plxnative-opaque` is absent,
/// which is what keeps the default path byte-identical. Only sends a request when the answer
/// CHANGES, because the opaque region is sticky server-side.
#[cfg(not(feature = "hostsim"))]
pub(crate) fn opaque_route(player: bool) {
    unsafe {
        if !G_OPAQUE_ENABLED {
            return;
        }
        // Opaque only where nothing is behind us. The player route keeps NULL, and gets it back on
        // the transition, so a video plane is never occluded by a claim we made on Home.
        let want = i8::from(!player);
        if G_OPAQUE_SENT == want {
            return;
        }
        let surface = G_WL_SURFACE;
        if surface.is_null() {
            return;
        }
        let region = if want == 1 { G_WL_REGION } else { std::ptr::null_mut() };
        wl_proxy_marshal(surface, WL_SURFACE_SET_OPAQUE_REGION, region);
        G_OPAQUE_SENT = want;
        log(&format!(
            "opaque: set_opaque_region({}) for route player={player}",
            if want == 1 { "full" } else { "NULL" }
        ));
    }
}

/// Nothing to declare opaque on a desktop: there is no video plane underneath and no LG compositor
/// to hint. The simulator keeps the same call site rather than growing a `cfg` at it.
#[cfg(feature = "hostsim")]
pub(crate) fn opaque_route(_player: bool) {}
