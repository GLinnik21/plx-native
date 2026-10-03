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
        //
        // …and nothing to LINK against under `cargo test`. Since phase 9 this is reached through
        // `Rig::clear_opaque_region` (`app/bridge.rs`), which the host suite exercises, where
        // before it was reachable only from `plex_run` and the dead-strip hid the missing
        // `libwayland-client`. The guard below is `G_WL_SURFACE`, which is null in a test, so
        // nothing is skipped that would have run.
        #[cfg(all(not(feature = "hostsim"), not(test)))]
        wl_proxy_marshal(surface, 4, std::ptr::null_mut::<c_void>());
    }
}

/// Null the Wayland surface and display pointers on app background.
///
/// SDL owns these client-side proxies; our references must not outlive an active window.
/// Background notifications revoke the borrow before any later frame can marshal through it.
/// This does not destroy SDL's objects. Foreground reacquires them from SDL, and resets the
/// opaque-region cache so a replacement surface receives its own request.
pub(crate) fn sys_release_wayland() {
    unsafe {
        let had_surface = !G_WL_SURFACE.is_null();
        G_WL_SURFACE = std::ptr::null_mut();
        G_WL_DISPLAY = std::ptr::null_mut();
        #[cfg(not(feature = "hostsim"))]
        {
            G_OPAQUE_SENT = -1;
        }
        if had_surface {
            log("wm: released borrowed Wayland handles");
        }
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
    crate::telemetry::window::record(crate::telemetry::window::Observation::step(
        crate::telemetry::window::Stage::WmQuery, None));
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
        // The Wayland query is webOS-only. Desktop SDL reports a different backend with a
        // different union layout, and has no hardware video plane needing this request.
        // update_wayland_info reads unaligned because the oversized byte buffer has no pointer
        // alignment guarantee, and rejects any non-Wayland subsystem before reading the union.
        #[cfg(not(feature = "hostsim"))]
        update_wayland_info(SDL_GetWindowWMInfo(winp, wmbuf.as_mut_ptr() as *mut c_void), &wmbuf);
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
            log(
                "wm: NO wl_surface — the UI plane cannot be made transparent, so video will \
                 decode invisibly beneath it. Check the SDL_SysWMinfo version handshake.",
            );
        }
        clear_opaque_region();
    }
}

use crate::eventlog::log;

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
// **Plane-scoped, and it must be.** Marking the surface opaque while the hardware video plane is
// bound beneath it would occlude a plane that has not torn down yet — which is why this reasserts
// NULL every frame while the plane is bound. `opaque_route(video_plane_bound)` therefore asserts
// NULL while the plane is bound and the full region otherwise (a pre-bind spinner or a post-unbind
// read-out gets the opaque region back), and remembers which it last sent so an unchanged state
// costs one atomic load and no protocol traffic (the region is double-buffered but otherwise
// STICKY: "the pending and current regions are never changed" otherwise).
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
    crate::dynlib::Handle::self_handle()
        .sym(name)
        .filter(|p| !p.is_null())
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
    let (Some(iface), Some(bind)) = (
        wl_sym("wl_compositor_interface"),
        wl_sym("wl_proxy_marshal_constructor_versioned"),
    ) else {
        log("opaque: libwayland has no versioned constructor — cannot bind wl_compositor");
        return;
    };
    // `struct wl_interface`'s first member is `const char *name`; `wl_registry_bind` passes it as
    // the third variadic argument. Read it rather than spelling the string twice.
    let iface_name = unsafe { *(iface as *const *const std::ffi::c_char) };
    let bind: unsafe extern "C" fn(*mut c_void, c_uint, *const c_void, c_uint, ...) -> *mut c_void =
        unsafe { std::mem::transmute(bind) };
    let proxy = unsafe {
        bind(
            registry,
            WL_REGISTRY_BIND,
            iface,
            want,
            name,
            iface_name,
            want,
            std::ptr::null_mut::<c_void>(),
        )
    };
    unsafe { G_WL_COMPOSITOR = proxy };
    log(&format!(
        "opaque: bound wl_compositor v{want} (advertised v{version}) proxy={proxy:p}"
    ));
}

/// Build the full-surface opaque region, once, at boot. No-op unless `/tmp/plxnative-opaque` is
/// armed; returns without touching the surface's current (NULL) region either way.
#[cfg(not(feature = "hostsim"))]
pub(crate) fn opaque_region_init() {
    if !crate::devtrig::flag("opaque") {
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
    static LISTENER: RegistryListener = RegistryListener {
        global: on_global,
        global_remove: on_global_remove,
    };
    let registry = unsafe {
        ctor(
            display,
            WL_DISPLAY_GET_REGISTRY,
            reg_iface,
            std::ptr::null_mut::<c_void>(),
        )
    };
    if registry.is_null() {
        log("opaque: wl_display.get_registry returned NULL — experiment not armed");
        return;
    }
    unsafe {
        add_listener(
            registry,
            std::ptr::from_ref(&LISTENER).cast::<c_void>(),
            std::ptr::null_mut(),
        );
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
    log(&format!(
        "opaque: ARMED — full-surface opaque region {vw}x{vh} at ({vx},{vy})"
    ));
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
        // Opaque only where nothing is behind us. A bound video plane keeps NULL, and gets it back
        // on the transition, so a video plane is never occluded by a claim we made on Home.
        let want = i8::from(!player);
        if G_OPAQUE_SENT == want {
            return;
        }
        let surface = G_WL_SURFACE;
        if surface.is_null() {
            return;
        }
        let region = if want == 1 {
            G_WL_REGION
        } else {
            std::ptr::null_mut()
        };
        // See `clear_opaque_region` for why the host suite must not need this symbol. `G_WL_SURFACE`
        // and `G_OPAQUE_ENABLED` are both false/null in a test, so the two returns above have
        // already fired.
        #[cfg(not(test))]
        wl_proxy_marshal(surface, WL_SURFACE_SET_OPAQUE_REGION, region);
        #[cfg(test)]
        let _ = (region, WL_SURFACE_SET_OPAQUE_REGION);
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

// ---- The COMPOSITOR FRAME-CALLBACK PROBE (`/tmp/plxnative-framecb`) -----------------------------
//
// A present's wait for a free back buffer is paid inside the frame's first framebuffer-0 command
// (the `clear` span), and from inside the app that wait reads the same whether the compositor
// repainted late, skipped a repaint, or repainted on time and we were slow to notice. This probe
// asks the compositor directly: one `wl_surface.frame` request per presented frame, placed before
// the swap that commits it, whose `done(time)` event carries the COMPOSITOR'S OWN millisecond
// stamp of the repaint that picked the commit up. Printed beside the frame's `FRAMEDROP` line
// with our own monotonic clock, the two tell those cases apart.
//
// It also prices the frame thread over the three stretches of a player frame's present: `w` (the
// first framebuffer command, where the driver waits for a back buffer), `d` (the draw after it)
// and `s` (the swap). Each prints `<p>=<wall>/<cpu>/<runq>` in ms and `<p>sw=<slept>/<preempted>`
// context switches: wall time that is neither CPU nor run-queue wait was spent BLOCKED, run-queue
// wait is a thread that wanted to run and was not given a CPU, and CPU is work. `wait_at=` is the
// monotonic ms at which the wait began, for lining a frame up with a kernel trace.
//
// Same linkage rule as the opaque experiment above: every libwayland entry point is resolved
// from the process's own scope, ONCE (`frame_syms`, on the first armed present, before the swap
// stretch is marked), so the probe adds no symbol to the link and its lookups are not charged to
// the `s` stretch. Unarmed it is one latched bool per presented frame. The whole probe is
// compiled only with `devtriggers` (and not in the simulator); the shipping build has the empty
// stubs at the end of this section.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
crate::devtrig::latched_flag!(
    /// `/tmp/plxnative-framecb` — see the section comment above.
    pub(crate) fn frame_probe_armed = "framecb";
);

/// `(frame seq, compositor stamp ms, our CLOCK_MONOTONIC µs at dispatch)` for every `done` that
/// arrived since the last [`frame_probe_fields`].
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
static FRAME_DONE: std::sync::Mutex<Vec<(u32, u32, u64)>> = std::sync::Mutex::new(Vec::new());
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
static FRAME_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
fn mono_us() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a plain clock read into a local.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

/// What the frame thread has cost so far, read at each boundary of a frame's present.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
#[derive(Clone, Copy, Default)]
struct ThreadCost {
    /// `CLOCK_MONOTONIC`, µs.
    at: u64,
    /// On-CPU time, µs.
    cpu: u64,
    /// Time spent RUNNABLE but waiting for a CPU, µs (`/proc/thread-self/schedstat`, field 2).
    /// Wall minus `cpu` minus this is time spent blocked.
    runq: u64,
    /// Voluntary context switches: the thread slept.
    slept: u64,
    /// Involuntary ones: it was preempted.
    preempted: u64,
}

#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
impl ThreadCost {
    fn now() -> Self {
        let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a plain clock read into a local.
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
        let cpu = ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000;
        #[cfg(target_os = "linux")]
        let (slept, preempted) = {
            // SAFETY: `getrusage` fills the zeroed struct it is handed.
            let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
            unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut ru) };
            (ru.ru_nvcsw as u64, ru.ru_nivcsw as u64)
        };
        #[cfg(not(target_os = "linux"))]
        let (slept, preempted) = (0, 0);
        Self { at: mono_us(), cpu, runq: Self::runq_us(), slept, preempted }
    }

    /// The scheduler's own run-queue wait for this thread. One `pread` on a descriptor opened
    /// once, by the frame thread, for itself; 0 where the file does not exist.
    fn runq_us() -> u64 {
        static FD: std::sync::OnceLock<c_int> = std::sync::OnceLock::new();
        // SAFETY: a NUL-terminated literal path; the descriptor lives for the process.
        let fd = *FD.get_or_init(|| unsafe {
            libc::open(c"/proc/thread-self/schedstat".as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC)
        });
        if fd < 0 {
            return 0;
        }
        let mut buf = [0u8; 96];
        // SAFETY: `buf` is a live local of the length passed.
        let n = unsafe { libc::pread(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
        let text = std::str::from_utf8(&buf[..n.max(0) as usize]).unwrap_or("");
        // "<run ns> <run-queue wait ns> <timeslices>"
        text.split_ascii_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) / 1000
    }

    /// ` <p>=<wall>/<cpu>/<runq> <p>sw=<slept>/<preempted>` for the stretch from `from` to `self`.
    fn since(&self, from: &Self, phase: &str) -> String {
        let ms = |a: u64, b: u64| a.saturating_sub(b) as f64 / 1000.0;
        format!(
            " {phase}={:.2}/{:.2}/{:.2} {phase}sw={}/{}",
            ms(self.at, from.at),
            ms(self.cpu, from.cpu),
            ms(self.runq, from.runq),
            self.slept.saturating_sub(from.slept),
            self.preempted.saturating_sub(from.preempted)
        )
    }
}

/// This frame's boundaries: `[before the first framebuffer command, back buffer acquired, before
/// the swap]`. Frame-thread only; a `Mutex` because it is a `static`.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
static FRAME_COST: std::sync::Mutex<[Option<ThreadCost>; 3]> = std::sync::Mutex::new([None; 3]);

#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
fn frame_probe_mark(slot: usize) {
    if !frame_probe_armed() {
        return;
    }
    if let Ok(mut cost) = FRAME_COST.lock() {
        cost[slot] = Some(ThreadCost::now());
    }
}

/// The frame's first framebuffer command is next: the wait for a back buffer starts here.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
pub(crate) fn frame_probe_waiting() {
    frame_probe_mark(0);
}

/// The frame's first framebuffer command has returned: the back buffer is acquired.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
pub(crate) fn frame_probe_acquired() {
    frame_probe_mark(1);
}

/// The libwayland entry points the probe calls, as addresses (a raw pointer is not `Sync`).
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
struct FrameSyms {
    ctor: usize,
    add_listener: usize,
    callback_interface: usize,
    /// Absent on a libwayland without it: the callback is then simply not destroyed.
    destroy: Option<usize>,
}

/// Resolved once. `None` when any of the three request symbols is missing, which makes the probe a
/// silent no-op.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
fn frame_syms() -> Option<&'static FrameSyms> {
    static SYMS: std::sync::OnceLock<Option<FrameSyms>> = std::sync::OnceLock::new();
    SYMS.get_or_init(|| {
        Some(FrameSyms {
            ctor: wl_sym("wl_proxy_marshal_constructor")? as usize,
            add_listener: wl_sym("wl_proxy_add_listener")? as usize,
            callback_interface: wl_sym("wl_callback_interface")? as usize,
            destroy: wl_sym("wl_proxy_destroy").map(|p| p as usize),
        })
    })
    .as_ref()
}

#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
unsafe extern "C" fn on_frame_done(data: *mut c_void, callback: *mut c_void, time: c_uint) {
    if let Ok(mut done) = FRAME_DONE.lock() {
        done.push((data as usize as u32, time, mono_us()));
    }
    // `wl_callback` is a one-shot: the client destroys it from its own `done`.
    if let Some(destroy) = frame_syms().and_then(|s| s.destroy) {
        let destroy: unsafe extern "C" fn(*mut c_void) = unsafe { std::mem::transmute(destroy) };
        unsafe { destroy(callback) };
    }
}

/// Ask for this frame's callback. Call on a PRESENTING frame, before the swap that commits it.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
pub(crate) fn frame_probe_request() {
    if !frame_probe_armed() {
        return;
    }
    // Before the swap stretch is marked, so a first-use lookup is not charged to it.
    let syms = frame_syms();
    frame_probe_mark(2);
    let surface = unsafe { G_WL_SURFACE };
    if surface.is_null() {
        return;
    }
    let Some(syms) = syms else {
        return;
    };
    let iface = syms.callback_interface as *const c_void;
    let ctor: unsafe extern "C" fn(*mut c_void, c_uint, *const c_void, ...) -> *mut c_void =
        unsafe { std::mem::transmute(syms.ctor) };
    let add_listener: unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_void) -> c_int =
        unsafe { std::mem::transmute(syms.add_listener) };
    // `struct wl_callback_listener` is one function pointer.
    static LISTENER: unsafe extern "C" fn(*mut c_void, *mut c_void, c_uint) = on_frame_done;
    const WL_SURFACE_FRAME: c_uint = 3;
    let seq = FRAME_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    unsafe {
        let callback = ctor(surface, WL_SURFACE_FRAME, iface, std::ptr::null_mut::<c_void>());
        if !callback.is_null() {
            add_listener(
                callback,
                std::ptr::from_ref(&LISTENER).cast::<c_void>(),
                seq as usize as *mut c_void,
            );
        }
    }
}

/// The probe's trailing `FRAMEDROP` fields for the frame just presented: `seq=` (the request this
/// frame made), `mono=` (our monotonic clock, ms, now), the three stretches described in the
/// section comment, then `cb=<seq>:<compositor ms>:<our ms at dispatch>` for each callback that
/// arrived since the previous line. `""` unarmed.
#[cfg(all(not(feature = "hostsim"), feature = "devtriggers"))]
pub(crate) fn frame_probe_fields() -> String {
    if !frame_probe_armed() {
        return String::new();
    }
    let done = FRAME_DONE.lock().map(|mut d| std::mem::take(&mut *d)).unwrap_or_default();
    let mut out = format!(
        " seq={} mono={:.1}",
        FRAME_SEQ.load(std::sync::atomic::Ordering::Relaxed),
        mono_us() as f64 / 1000.0
    );
    let [waiting, acquired, before_swap] =
        FRAME_COST.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
    if let Some(before_swap) = before_swap {
        if let (Some(waiting), Some(acquired)) = (waiting, acquired) {
            out.push_str(&format!(" wait_at={:.2}", waiting.at as f64 / 1000.0));
            out.push_str(&acquired.since(&waiting, "w"));
            out.push_str(&before_swap.since(&acquired, "d"));
        }
        out.push_str(&ThreadCost::now().since(&before_swap, "s"));
    }
    out.push_str(" cb=");
    for (i, (seq, stamp, at)) in done.iter().enumerate() {
        if i > 0 {
            out.push('|');
        }
        out.push_str(&format!("{seq}:{stamp}:{:.1}", *at as f64 / 1000.0));
    }
    out
}

#[cfg(any(feature = "hostsim", not(feature = "devtriggers")))]
pub(crate) fn frame_probe_request() {}
#[cfg(any(feature = "hostsim", not(feature = "devtriggers")))]
pub(crate) fn frame_probe_waiting() {}
#[cfg(any(feature = "hostsim", not(feature = "devtriggers")))]
pub(crate) fn frame_probe_acquired() {}
#[cfg(any(feature = "hostsim", not(feature = "devtriggers")))]
pub(crate) fn frame_probe_fields() -> String {
    String::new()
}

// Publish SDL's borrowed handles. Kept separate from the native query so failed queries can
// be exercised without loading the television's SDL or marshalling a fake proxy.
#[cfg(any(not(feature = "hostsim"), test))]
unsafe fn update_wayland_info(ok: c_int, info: &[u8; 512]) {
    sys_release_wayland();
    let subsystem = i32::from_ne_bytes(info[4..8].try_into().unwrap());
    let (mut display_present, mut surface_present) = (false, false);
    if ok != 0 && subsystem == 6 { // SDL_SYSWM_WAYLAND
        let pointers = info.as_ptr().add(8) as *const *mut c_void;
        let display = pointers.read_unaligned();
        let surface = pointers.add(1).read_unaligned();
        display_present = !display.is_null();
        surface_present = !surface.is_null();
        if display_present && surface_present {
            G_WL_DISPLAY = display;
            G_WL_SURFACE = surface;
        }
    }
    use crate::telemetry::window::{Observation, Stage};
    let stage = if ok == 0 { Stage::WmFailed }
        else if subsystem != 6 { Stage::WmWrongBackend }
        else if G_WL_SURFACE.is_null() { Stage::WmNoSurface }
        else { Stage::WmReady };
    crate::telemetry::window::record(Observation { stage, playing: None,
        version: Some([info[0], info[1], info[2]]),
        display: Some(display_present), surface: Some(surface_present) });
}

#[cfg(test)]
mod wayland_tests {
    use super::*;

    #[test]
    fn failed_refresh_cannot_reuse_a_previous_surface() {
        let _guard = crate::testlock::serial();
        unsafe {
            let mut display = 0u8;
            let mut surface = 0u8;
            G_WL_DISPLAY = std::ptr::addr_of_mut!(display).cast();
            G_WL_SURFACE = std::ptr::addr_of_mut!(surface).cast();
            update_wayland_info(0, &[0; 512]);
            let (display, surface) = (G_WL_DISPLAY, G_WL_SURFACE);
            G_WL_DISPLAY = std::ptr::null_mut();
            G_WL_SURFACE = std::ptr::null_mut();
            assert!(display.is_null() && surface.is_null(),
                "a failed SDL query must revoke both borrowed handles");
        }
    }

    #[test]
    fn background_release_and_foreground_refresh_replace_the_borrow() {
        let _guard = crate::testlock::serial();
        unsafe {
            let mut display = 0u8;
            let mut surface = 0u8;
            let display_ptr = std::ptr::addr_of_mut!(display).cast::<c_void>();
            let surface_ptr = std::ptr::addr_of_mut!(surface).cast::<c_void>();
            let mut info = [0u8; 512];
            info[4..8].copy_from_slice(&6i32.to_ne_bytes());
            let pointers = info.as_mut_ptr().add(8) as *mut *mut c_void;
            pointers.write_unaligned(display_ptr);
            pointers.add(1).write_unaligned(surface_ptr);
            update_wayland_info(1, &info);
            let acquired = (G_WL_DISPLAY, G_WL_SURFACE);
            #[cfg(not(feature = "hostsim"))]
            { G_OPAQUE_SENT = 1; }
            sys_release_wayland();
            sys_release_wayland(); // WILL + DID background is idempotent.
            #[cfg(not(feature = "hostsim"))]
            { let sent = G_OPAQUE_SENT; assert_eq!(sent, -1); }
            let released = (G_WL_DISPLAY, G_WL_SURFACE);
            clear_opaque_region(); // No native marshal can run after revocation.
            let mut replacement = 0u8;
            let replacement_ptr = std::ptr::addr_of_mut!(replacement).cast::<c_void>();
            pointers.add(1).write_unaligned(replacement_ptr);
            update_wayland_info(1, &info);
            let refreshed = (G_WL_DISPLAY, G_WL_SURFACE);
            sys_release_wayland();
            assert_eq!(acquired, (display_ptr, surface_ptr));
            assert!(released.0.is_null() && released.1.is_null());
            assert_eq!(refreshed, (display_ptr, replacement_ptr));
        }
    }

    #[test]
    fn foreign_or_incomplete_wm_info_cannot_supply_wayland_handles() {
        let _guard = crate::testlock::serial();
        unsafe {
            for subsystem in [0i32, 4, 6] {
                let mut info = [0u8; 512];
                info[4..8].copy_from_slice(&subsystem.to_ne_bytes());
                // A foreign backend with two non-null pointers, or Wayland with no surface.
                let mut display = 0u8;
                let mut surface = 0u8;
                let pointers = info.as_mut_ptr().add(8) as *mut *mut c_void;
                pointers.write_unaligned(std::ptr::addr_of_mut!(display).cast());
                if subsystem != 6 {
                    pointers.add(1).write_unaligned(std::ptr::addr_of_mut!(surface).cast());
                }
                update_wayland_info(1, &info);
                let handles = (G_WL_DISPLAY, G_WL_SURFACE);
                sys_release_wayland();
                assert!(handles.0.is_null() && handles.1.is_null());
            }
        }
    }
}
