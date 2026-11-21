//! Rust port of src/system.c — webOS/SDL platform glue (system.h). Same C ABI.
//! sys_grab_wayland grabs the wayland surface and clears its opaque region so the
//! Starfish video plane below shows through. CRITICAL: the TV's SDL fork writes a
//! LARGER SDL_SysWMinfo than the headers declare, so SDL_GetWindowWMInfo is given
//! a generous over-allocated buffer — this overrun smashing a bare struct was the
//! modular-split crash. wl_surface opcode 4 = set_opaque_region.
use std::os::raw::{c_int, c_uint, c_void};

const SDL_GL_ALPHA_SIZE: c_int = 3; // SDL_GLattr: RED=0,GREEN=1,BLUE=2,ALPHA=3
/// `SDL_GL_DEPTH_SIZE` / `SDL_GL_STENCIL_SIZE` — same enum, 6 and 7. `plx_shell_sdl::init` asks for zero of
/// both; these read back what the driver actually granted, which is the only thing that settles it.
const SDL_GL_DEPTH_SIZE: c_int = 6;
const SDL_GL_STENCIL_SIZE: c_int = 7;
const GL_ALPHA_BITS: c_uint = 0x0D55;
const GL_RED_BITS: c_uint = 0x0D52;
const GL_DEPTH_BITS: c_uint = 0x0D56;
const GL_STENCIL_BITS: c_uint = 0x0D57;
// SDL_SysWMinfo layout (32-bit): version u8[3]@0, subsystem int@4, info union@8

extern "C" {
    fn SDL_GetWindowWMInfo(window: *mut c_void, info: *mut c_void) -> c_int;
    /// Fills `SDL_version` — three `Uint8`, major/minor/patch.
    fn SDL_GetVersion(ver: *mut u8);
    fn SDL_GL_GetAttribute(attr: c_int, value: *mut c_int) -> c_int;
    fn glGetIntegerv(pname: c_uint, params: *mut c_int);
}

// The three symbols that exist only on a television: wayland's proxy marshaller and glib's main
// context. A host build links neither: the desktop platform (`platform_desktop`) brings its own
// shell and does not link this crate.
// They are declared apart rather than in the block above, which every host build links against.
extern "C" {
    fn wl_proxy_marshal(proxy: *mut c_void, opcode: c_uint, ...);
    fn g_main_context_pending(ctx: *mut c_void) -> c_int;
    fn g_main_context_iteration(ctx: *mut c_void, may_block: c_int) -> c_int;
}

static mut G_WL_SURFACE: *mut c_void = std::ptr::null_mut();
static mut G_WL_DISPLAY: *mut c_void = std::ptr::null_mut();
/// The last non-null `wl_surface` SDL handed [`update_wayland_info`], kept as an ADDRESS to compare
/// against and never dereferenced or marshalled through. Unlike `G_WL_SURFACE` it survives
/// [`sys_release_wayland`], so the DID-foreground grab can tell a replaced surface (PLX-NATIVE-K:
/// SDL recreating the window's `wl_surface` under a live EGLSurface) from the first one.
///
/// The comparison is by address only. If SDL frees the old `wl_surface` proxy before it allocates
/// its replacement, the allocator can hand back the same chunk at the same address, and then the
/// crumb does not fire. So a `surface_changed` crumb proves a replacement, but a missing crumb
/// does not prove the surface was kept. Comparing `wl_proxy_get_id` would not help, because
/// Wayland recycles object ids through a free list.
static G_WL_SURFACE_SEEN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

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
        // Nothing to LINK against under `cargo test`. Since phase 9 this is reached through
        // `Rig::clear_opaque_region` (`app/bridge.rs`), which the host suite exercises, where
        // before it was reachable only from `plex_run` and the dead-strip hid the missing
        // `libwayland-client`. The guard below is `G_WL_SURFACE`, which is null in a test, so
        // nothing is skipped that would have run.
        #[cfg(not(test))]
        wl_proxy_marshal(surface, 4, std::ptr::null_mut::<c_void>());
    }
}

/// Null the Wayland surface and display pointers on app background.
///
/// SDL owns these client-side proxies; our references must not outlive an active window.
/// Background notifications revoke the borrow before any later frame can marshal through it.
/// This does not destroy SDL's objects. Foreground reacquires them from SDL, and resets the
/// opaque-region cache so a replacement surface receives its own request. Answers whether a
/// surface was held.
pub(crate) fn sys_release_wayland() -> bool {
    unsafe {
        let had_surface = !G_WL_SURFACE.is_null();
        G_WL_SURFACE = std::ptr::null_mut();
        G_WL_DISPLAY = std::ptr::null_mut();
        G_OPAQUE_SENT = -1;
        if had_surface {
            log("wm: released borrowed Wayland handles");
        }
        had_surface
    }
}

/// Service the glib main context that luna-service2 replies arrive on.
pub(crate) fn ls2_pump() {
    unsafe {
        let mut guard = 8;
        while guard > 0 && g_main_context_pending(std::ptr::null_mut()) != 0 {
            g_main_context_iteration(std::ptr::null_mut(), 0);
            guard -= 1;
        }
    }
}

pub(crate) fn sys_grab_wayland(winp: *mut c_void) {
    plx_telemetry::telemetry::window::record(plx_telemetry::telemetry::window::Observation::step(
        plx_telemetry::telemetry::window::Stage::WmQuery, None));
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
        // Deprecated in a desktop CORE profile, where they would leave the value alone and raise
        // `GL_INVALID_ENUM`; the SDL attributes above answer the same question portably. On the
        // television's ES2 context, the only one this runs on, both are legal.
        glGetIntegerv(GL_DEPTH_BITS, &mut dbits);
        glGetIntegerv(GL_STENCIL_BITS, &mut sbits);
        // `depth=`/`stencil=` are here to be READ: `plx_shell_sdl::init` asks for zero of each because nothing
        // in this renderer uses them, and on a tiler a granted depth buffer is a per-frame
        // write-back of 1920x1080x2 bytes nobody consumes. A non-zero here means the driver
        // refused and the saving is not real.
        log(&format!(
            "FB bits: alpha={abits} red={rbits} depth={dbits} stencil={sbits} \
             (config alpha={a} depth={d} stencil={s})"
        ));
        // update_wayland_info reads unaligned because the oversized byte buffer has no pointer
        // alignment guarantee, and rejects any non-Wayland subsystem before reading the union.
        update_wayland_info(SDL_GetWindowWMInfo(winp, wmbuf.as_mut_ptr() as *mut c_void), &wmbuf);
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
        if surf.is_null() {
            log(
                "wm: NO wl_surface — the UI plane cannot be made transparent, so video will \
                 decode invisibly beneath it. Check the SDL_SysWMinfo version handshake.",
            );
        }
        clear_opaque_region();
    }
}

use plx_base::eventlog::log;

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
const WL_REGISTRY_BIND: c_uint = 0;
const WL_DISPLAY_GET_REGISTRY: c_uint = 1;
const WL_COMPOSITOR_CREATE_REGION: c_uint = 1;
const WL_REGION_ADD: c_uint = 1;
const WL_SURFACE_SET_OPAQUE_REGION: c_uint = 4;

/// The full-surface `wl_region`, created once at boot. Never per frame — a region is a server
/// object and allocating one every present would be protocol traffic on the frame path.
static mut G_WL_REGION: *mut c_void = std::ptr::null_mut();
/// What we last told the compositor: `1` = the full region, `0` = NULL, `-1` = nothing yet.
static mut G_OPAQUE_SENT: i8 = -1;
/// Set only by [`opaque_region_init`], and only when the trigger armed AND a region was built.
static mut G_OPAQUE_ENABLED: bool = false;
/// Scratch for the registry callback: the `wl_compositor` proxy it binds.
static mut G_WL_COMPOSITOR: *mut c_void = std::ptr::null_mut();

#[repr(C)]
struct RegistryListener {
    global: unsafe extern "C" fn(*mut c_void, *mut c_void, c_uint, *const std::ffi::c_char, c_uint),
    global_remove: unsafe extern "C" fn(*mut c_void, *mut c_void, c_uint),
}

/// A symbol from the process's own scope, `None` if absent or NULL. `libwayland-client` is linked
/// normally, so its symbols are already mapped; this only avoids naming these particular ones in
/// `DT_NEEDED`. The Magic Remote's cursor door ([`cursor_door`]) resolves the system SDL's cursor
/// calls the same way, for the reason its doc gives.
fn self_sym(name: &str) -> Option<*mut c_void> {
    plx_base::dynlib::Handle::self_handle()
        .sym(name)
        .filter(|p| !p.is_null())
}

unsafe extern "C" fn on_global_remove(_data: *mut c_void, _reg: *mut c_void, _name: c_uint) {}

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
        self_sym("wl_compositor_interface"),
        self_sym("wl_proxy_marshal_constructor_versioned"),
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
pub(crate) fn opaque_region_init() {
    if !plx_base::devtrig::flag("opaque") {
        return;
    }
    let (display, surface) = unsafe { (G_WL_DISPLAY, G_WL_SURFACE) };
    if display.is_null() || surface.is_null() {
        log("opaque: no wl_display/wl_surface — experiment not armed");
        return;
    }
    let (Some(ctor), Some(add_listener), Some(roundtrip), Some(destroy), Some(reg_iface)) = (
        self_sym("wl_proxy_marshal_constructor"),
        self_sym("wl_proxy_add_listener"),
        self_sym("wl_display_roundtrip"),
        self_sym("wl_proxy_destroy"),
        self_sym("wl_registry_interface"),
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

    let Some(region_iface) = self_sym("wl_region_interface") else {
        log("opaque: libwayland has no wl_region_interface — experiment not armed");
        return;
    };
    // Surface-local coordinates. `viewport()` is the rect the renderer actually draws into, which
    // is the whole drawable (1920x1080 unless a webOS 1-3 owner picked a smaller Interface resolution, or the debug drawable trigger asked for one)
    // and is the honest answer on a letterboxed one, where the bars are not ours to claim.
    let (vx, vy, vw, vh) = plx_base::surface::viewport();
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

/// Assert the opaque region appropriate to this route, if the experiment is armed.
///
/// A no-op — one read of a `static` and a return — whenever `/tmp/plxnative-opaque` is absent,
/// which is what keeps the default path byte-identical. Only sends a request when the answer
/// CHANGES, because the opaque region is sticky server-side.
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
// It also prices the frame thread over the three stretches of a frame's present: `w` (the
// first framebuffer command, where the driver waits for a back buffer), `d` (the draw after it)
// and `s` (the swap). Each prints `<p>=<wall>/<cpu>/<runq>` in ms and `<p>sw=<slept>/<preempted>`
// context switches: wall time that is neither CPU nor run-queue wait was spent BLOCKED, run-queue
// wait is a thread that wanted to run and was not given a CPU, and CPU is work. `wait_at=` is the
// monotonic ms at which the wait began, for lining a frame up with a kernel trace.
//
// A fourth stretch, `p`, prices the frame loop's `prepare_window` (the `prepare=` stage): it runs
// BEFORE the back-buffer wait, on every frame, presenting or not, so it has its own pair of marks
// (`frame_probe_prepare_begin`/`_end`) apart from the wait/commit slots. Each frame's begin
// overwrites the pair, so a value is never carried into a later frame's line; it prints only when
// both marks of the frame that is being reported were taken.
//
// Same linkage rule as the opaque experiment above: every libwayland entry point is resolved
// from the process's own scope, ONCE (`frame_syms`, on the first armed present, before the swap
// stretch is marked), so the probe adds no symbol to the link and its lookups are not charged to
// the `s` stretch. Unarmed it is one latched bool per presented frame. The whole probe is
// compiled only with `devtriggers`; the shipping build has the empty
// stubs at the end of this section.
#[cfg(feature = "devtriggers")]
plx_base::devtrig::latched_flag!(
    /// `/tmp/plxnative-framecb` — see the section comment above.
    pub(crate) fn frame_probe_armed = "framecb";
);

/// `(frame seq, compositor stamp ms, our CLOCK_MONOTONIC µs at dispatch)` for every `done` that
/// arrived since the last [`frame_probe_fields`].
#[cfg(feature = "devtriggers")]
static FRAME_DONE: std::sync::Mutex<Vec<(u32, u32, u64)>> = std::sync::Mutex::new(Vec::new());
#[cfg(feature = "devtriggers")]
static FRAME_SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

#[cfg(feature = "devtriggers")]
fn mono_us() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a plain clock read into a local.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1000
}

/// What the frame thread has cost so far, read at each boundary of a frame's present.
#[cfg(feature = "devtriggers")]
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

#[cfg(feature = "devtriggers")]
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
#[cfg(feature = "devtriggers")]
static FRAME_COST: std::sync::Mutex<[Option<ThreadCost>; 3]> = std::sync::Mutex::new([None; 3]);

#[cfg(feature = "devtriggers")]
fn frame_probe_mark(slot: usize) {
    if !frame_probe_armed() {
        return;
    }
    if let Ok(mut cost) = FRAME_COST.lock() {
        cost[slot] = Some(ThreadCost::now());
    }
}

/// `prepare_window`'s boundaries: `[before, after]`. Independent of [`FRAME_COST`]: it is marked on
/// frames that never reach the wait. Frame-thread only.
#[cfg(feature = "devtriggers")]
static PREPARE_COST: std::sync::Mutex<[Option<ThreadCost>; 2]> = std::sync::Mutex::new([None; 2]);

/// `prepare_window` is next. Starts the frame's `p` stretch, dropping any earlier frame's marks.
#[cfg(feature = "devtriggers")]
pub(crate) fn frame_probe_prepare_begin() {
    if !frame_probe_armed() {
        return;
    }
    if let Ok(mut cost) = PREPARE_COST.lock() {
        *cost = [Some(ThreadCost::now()), None];
    }
}

/// `prepare_window` has returned: closes the frame's `p` stretch.
#[cfg(feature = "devtriggers")]
pub(crate) fn frame_probe_prepare_end() {
    if !frame_probe_armed() {
        return;
    }
    if let Ok(mut cost) = PREPARE_COST.lock() {
        cost[1] = Some(ThreadCost::now());
    }
}

/// The frame's first framebuffer command is next: the wait for a back buffer starts here.
#[cfg(feature = "devtriggers")]
pub(crate) fn frame_probe_waiting() {
    frame_probe_mark(0);
}

/// The frame's first framebuffer command has returned: the back buffer is acquired.
#[cfg(feature = "devtriggers")]
pub(crate) fn frame_probe_acquired() {
    frame_probe_mark(1);
}

/// The libwayland entry points the probe calls, as addresses (a raw pointer is not `Sync`).
#[cfg(feature = "devtriggers")]
struct FrameSyms {
    ctor: usize,
    add_listener: usize,
    callback_interface: usize,
    /// Absent on a libwayland without it: the callback is then simply not destroyed.
    destroy: Option<usize>,
}

/// Resolved once. `None` when any of the three request symbols is missing, which makes the probe a
/// silent no-op.
#[cfg(feature = "devtriggers")]
fn frame_syms() -> Option<&'static FrameSyms> {
    static SYMS: std::sync::OnceLock<Option<FrameSyms>> = std::sync::OnceLock::new();
    SYMS.get_or_init(|| {
        Some(FrameSyms {
            ctor: self_sym("wl_proxy_marshal_constructor")? as usize,
            add_listener: self_sym("wl_proxy_add_listener")? as usize,
            callback_interface: self_sym("wl_callback_interface")? as usize,
            destroy: self_sym("wl_proxy_destroy").map(|p| p as usize),
        })
    })
    .as_ref()
}

#[cfg(feature = "devtriggers")]
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
#[cfg(feature = "devtriggers")]
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
/// frame made), `mono=` (our monotonic clock, ms, now), the three present stretches described in
/// the section comment, then `p=`/`psw=` for the frame's `prepare_window` (independent of them: it
/// prints whenever that frame took both of its marks, wait or no wait), then `cb=<seq>:<compositor ms>:<our ms at dispatch>` for each callback that
/// arrived since the previous line. `""` unarmed.
#[cfg(feature = "devtriggers")]
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
    if let Ok(mut prepare) = PREPARE_COST.lock() {
        if let [Some(begin), Some(end)] = std::mem::take(&mut *prepare) {
            out.push_str(&end.since(&begin, "p"));
        }
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

#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_probe_request() {}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_probe_prepare_begin() {}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_probe_prepare_end() {}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_probe_waiting() {}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_probe_acquired() {}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_probe_fields() -> String {
    String::new()
}

// Publish SDL's borrowed handles. Kept separate from the native query so failed queries can
// be exercised without loading the television's SDL or marshalling a fake proxy.
unsafe fn update_wayland_info(ok: c_int, info: &[u8; 512]) {
    sys_release_wayland();
    let subsystem = i32::from_ne_bytes(info[4..8].try_into().unwrap());
    let (mut display_present, mut surface_present) = (false, false);
    let mut replaced = false;
    if ok != 0 && subsystem == 6 { // SDL_SYSWM_WAYLAND
        let pointers = info.as_ptr().add(8) as *const *mut c_void;
        let display = pointers.read_unaligned();
        let surface = pointers.add(1).read_unaligned();
        display_present = !display.is_null();
        surface_present = !surface.is_null();
        if display_present && surface_present {
            G_WL_DISPLAY = display;
            G_WL_SURFACE = surface;
            // Compared as an address against the last one SDL published: a different non-null
            // surface means SDL replaced the window's `wl_surface`, which a live EGLSurface that
            // was created on the old one does not follow (PLX-NATIVE-K).
            let previous = G_WL_SURFACE_SEEN.swap(surface as usize, std::sync::atomic::Ordering::Relaxed);
            replaced = previous != 0 && previous != surface as usize;
        }
    }
    use plx_telemetry::telemetry::window::{Observation, Stage};
    if replaced {
        plx_telemetry::telemetry::window::record(Observation::step(Stage::SurfaceChanged, None));
    }
    let stage = if ok == 0 { Stage::WmFailed }
        else if subsystem != 6 { Stage::WmWrongBackend }
        else if G_WL_SURFACE.is_null() { Stage::WmNoSurface }
        else { Stage::WmReady };
    plx_telemetry::telemetry::window::record(Observation { stage, playing: None,
        version: Some([info[0], info[1], info[2]]),
        display: Some(display_present), surface: Some(surface_present) });
}

// ------------------------------------------------------------------------------------------
// The Magic Remote's pointer: hide it when the D-pad takes over, show it again on motion
// ------------------------------------------------------------------------------------------
//
// Moved here from `app/boot.rs` (platform plan, W5c) with its hostsim arms removed, the desktop's
// `Shell` stating the no-op instead, and its three entry points made safe: the pointer's visibility is
// the platform's, so the loop reaches it through `Shell::{hide_pointer, show_pointer_after_motion,
// hide_is_cursor_swap}` and `port::WebosShell` is the table over these three. A desktop's pointer is
// a mouse the application does not hide, so `platform_desktop`'s shell states no-ops.

/// Hide the Magic Remote's on-screen pointer. A webOS-only concept: there is no such cursor to
/// hide on a desktop, and `SDL_webOSCursorVisibility` exists in no SDL but LG's fork.
///
/// One door rather than a branch at each caller, so the platform question is asked once
/// and the callers read the same on LG's extension and on the transparent-cursor path.
///
/// Resolved at runtime rather than linked: webOS 3.4 ships SDL 2.0.3, which has none of LG's
/// `SDL_webOS*` extensions, and a strong reference there is a lazily-bound PLT slot that kills
/// the process on the first D-pad press (`tools/fwcompat.py --release 3.4.0`).
///
/// **Without LG's extension the pointer is SDL's own cursor, and it is swapped for a transparent
/// one rather than switched off.** webOS 3.9.3 ships SDL 2.0.4 with no `SDL_webOS*` export at all,
/// and that SDL draws the pointer itself (its `.rodata` carries `/usr/share/im/cursorType*.png`
/// and `wl_starfish_pointer`). With nothing to call, every hide was a no-op and the pointer stayed
/// up until LG's ~5-minute idle timeout, issue #642. `SDL_ShowCursor(SDL_DISABLE)` does hide it
/// there, but measured on that set it also stops pointer motion reaching the app until the pointer
/// re-enters the window (opening and closing the Settings overlay), so no motion could bring it
/// back. With a transparent cursor the motion keeps arriving, and [`show_cursor_after_motion`] puts
/// the previous cursor back on the next one. Every SDL call is dlsym'd, so the binary gains no
/// import.
#[inline]
pub(crate) fn hide_cursor() {
    // SAFETY: the door's functions are the process's own SDL exports (or LG's extension), resolved
    // by `cursor_door` and called here on the loop thread, which is the thread every `Shell` call runs on.
    unsafe { hide_through(cursor_door(), &SHOWN_CURSOR) }
}

/// True where the pointer is hidden by swapping in a transparent cursor rather than by LG's call,
/// so the system's pointer stays up and the remote's OK arrives as a click (`input::click_is_ok`).
/// False wherever LG's extension exists, and where there is no way to hide it at all.
pub(crate) fn cursor_is_transparent() -> bool {
    matches!(cursor_door(), CursorDoor::Sdl { .. })
}

/// Bring the pointer back after [`hide_cursor`] swapped in the transparent cursor. Call on pointer
/// motion while the cursor is hidden. A no-op wherever LG's extension exists: there the
/// compositor shows its own pointer on motion, as it always has.
#[inline]
pub(crate) fn show_cursor_after_motion() {
    // SAFETY: as `hide_cursor`.
    unsafe { show_through(cursor_door(), &SHOWN_CURSOR) }
}

/// [`hide_cursor`] on a given door, the cursor it replaced kept in `shown`. Split out so the
/// bookkeeping is gradeable on the host with a door of fake functions.
unsafe fn hide_through(door: CursorDoor, shown: &std::sync::atomic::AtomicUsize) {
    match door {
        CursorDoor::Lg(f) => {
            f(0);
        }
        CursorDoor::Sdl { set, get, def, blank } => {
            let cur = get() as usize;
            if cur != blank {
                let keep = if cur != 0 { cur } else { def() as usize };
                if keep == 0 {
                    return; // nothing to put back, so the pointer would never return: leave it
                }
                shown.store(keep, std::sync::atomic::Ordering::Relaxed);
            }
            set(blank as *mut std::ffi::c_void);
        }
        CursorDoor::None => {}
    }
}

/// [`show_cursor_after_motion`] on a given door.
unsafe fn show_through(door: CursorDoor, shown: &std::sync::atomic::AtomicUsize) {
    if let CursorDoor::Sdl { set, .. } = door {
        let shown = shown.load(std::sync::atomic::Ordering::Relaxed);
        if shown != 0 {
            set(shown as *mut std::ffi::c_void);
        }
    }
}

/// The cursor [`hide_cursor`] replaced with the transparent one, for the motion that restores it.
static SHOWN_CURSOR: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

type CursorVisibilityFn = unsafe extern "C" fn(c_int) -> c_int;
type SetCursorFn = unsafe extern "C" fn(*mut std::ffi::c_void);
type GetCursorFn = unsafe extern "C" fn() -> *mut std::ffi::c_void;
type CreateCursorFn =
    unsafe extern "C" fn(*const u8, *const u8, c_int, c_int, c_int, c_int) -> *mut std::ffi::c_void;

/// Which call hides the pointer on this firmware, resolved once: LG's extension when the system
/// SDL has it (webOS 4+), else a transparent SDL cursor (webOS 3.x).
#[derive(Clone, Copy)]
enum CursorDoor {
    Lg(CursorVisibilityFn),
    /// `SDL_SetCursor` / `SDL_GetCursor` / `SDL_GetDefaultCursor`, and the transparent cursor.
    Sdl { set: SetCursorFn, get: GetCursorFn, def: GetCursorFn, blank: usize },
    None,
}

/// Resolved on the first hide, after `SDL_Init`, because `SDL_CreateCursor` needs the video
/// subsystem.
fn cursor_door() -> CursorDoor {
    static DOOR: std::sync::OnceLock<CursorDoor> = std::sync::OnceLock::new();
    *DOOR.get_or_init(|| {
        use std::ffi::c_void;
        use std::mem::transmute;
        // SAFETY: each symbol is transmuted to the signature SDL2's public API (or, for
        // `SDL_webOSCursorVisibility`, LG's fork) gives it, and is called on the main thread.
        let door = unsafe {
            if let Some(f) = self_sym("SDL_webOSCursorVisibility") {
                CursorDoor::Lg(transmute::<*mut c_void, CursorVisibilityFn>(f))
            } else if let (Some(set), Some(get), Some(def), Some(create)) = (
                self_sym("SDL_SetCursor"),
                self_sym("SDL_GetCursor"),
                self_sym("SDL_GetDefaultCursor"),
                self_sym("SDL_CreateCursor"),
            ) {
                // 8x1 with every data and mask bit clear: SDL's transparent pixel throughout.
                let bits = [0u8; 1];
                let create = transmute::<*mut c_void, CreateCursorFn>(create);
                let blank = create(bits.as_ptr(), bits.as_ptr(), 8, 1, 0, 0);
                if blank.is_null() {
                    CursorDoor::None
                } else {
                    CursorDoor::Sdl {
                        set: transmute::<*mut c_void, SetCursorFn>(set),
                        get: transmute::<*mut c_void, GetCursorFn>(get),
                        def: transmute::<*mut c_void, GetCursorFn>(def),
                        blank: blank as usize,
                    }
                }
            } else {
                CursorDoor::None
            }
        };
        log(match door {
            CursorDoor::Lg(_) => "cursor: SDL_webOSCursorVisibility",
            CursorDoor::Sdl { .. } => "cursor: transparent SDL cursor (no SDL_webOS* in this SDL)",
            CursorDoor::None => "cursor: no way to hide the pointer",
        });
        door
    })
}

// ---- The LG SDL's boot hooks (moved from `app::boot`, W6) -------------------------------------------
//
// The two steps a webOS platform runs around `SDL_Init`, which the shell crate (`plx_shell_sdl::init`)
// calls in the order it pins: [`webos3_cursor_sleep_hint`] before it and [`tap_relaunch_payloads`] after
// it. They name LG's SDL fork (its `SDL_webOSCursorVisibility`, its `HELPER_HLunaServiceMessage` slot),
// so they are the webOS platform's and not the stock-SDL shell's; `WebosShell::init` (`port.rs`) is the
// only caller. They compile in every build of this crate, which no desktop build links.

/// **webOS 3.x: shorten LG's own idle hide from about five minutes to five seconds.** Without
/// LG's extension, LG's SDL hides its pointer by itself after `SDL_WEBOS_CURSOR_SLEEP_TIME` of
/// stillness, and its default is about five minutes (timed on a 3.9.3 set, issue #642), where
/// every other app on that television hides it within seconds. Measured on the same set: the value
/// is milliseconds, `5000` hides a still pointer after about five seconds, and a deliberate
/// movement brings it back. Set only where `SDL_webOSCursorVisibility` is absent, so webOS 4+
/// keeps its SDL's own behaviour. Must run before `SDL_Init`, which reads the hint.
pub(crate) unsafe fn webos3_cursor_sleep_hint() {
    if let Some(ms) = cursor_sleep_hint(self_sym("SDL_webOSCursorVisibility").is_some()) {
        plx_shell_sdl::set_hint(c"SDL_WEBOS_CURSOR_SLEEP_TIME", ms);
        log("cursor: SDL_WEBOS_CURSOR_SLEEP_TIME=5000 (no SDL_webOS* in this SDL)");
    }
}

/// The idle-hide time to ask SDL for, in milliseconds as a string; `None` where LG's extension is
/// present and its SDL keeps its own behaviour.
fn cursor_sleep_hint(has_lg_extension: bool) -> Option<&'static std::ffi::CStr> {
    (!has_lg_extension).then_some(c"5000")
}

/// **A warm launch's parameters, read where LG's SDL reads its `registerApp` replies.** SAM sends a
/// relaunch's parameters only to the `registerApp` subscriber, which is LG's SDL; its handler
/// (`callback_RegistNativeApp`) raises the window and never reads `parameters`. It fetches every
/// reply's payload through `HELPER_HLunaServiceMessage`, an exported pointer slot that
/// `LoadWEBOSBASELibrary` fills from `libhelpers.so.2` inside `SDL_Init` and that the handler
/// reloads on every call. Wrapping that slot hands a relaunch payload to `tv::launch` and returns
/// it unchanged, so SDL still raises the window. No new Luna registration: SAM refuses the app's
/// own (`tv::launch`). The firmware inventories have the slot from webOS 7.4 on; where it is missing
/// or empty (webOS 4.x to 6.x), nothing changes and callers keep closing the app first.
/// The tap installs only when the slot holds the address `libhelpers.so.2` (already mapped,
/// `RTLD_NOLOAD`) exports as `HLunaServiceMessage` ([`slot_holds`]); any other value is left alone.
pub(crate) unsafe fn tap_relaunch_payloads() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let Some(slot) = self_sym("HELPER_HLunaServiceMessage") else {
        log("relaunch tap: no HELPER_HLunaServiceMessage in this SDL");
        return;
    };
    // An atomic reference to a misaligned address is undefined behaviour: refuse a symbol that is
    // not a word-aligned slot before forming it.
    if !slot_aligned(slot as usize) {
        log("relaunch tap: slot misaligned \u{2014} not installed");
        return;
    }
    // SDL's Luna thread may read the slot while it is replaced: store it atomically, after the
    // function it forwards to is published.
    let slot = &*(slot as *const AtomicUsize);
    let wrapped = wrapped_message as *const () as usize;
    let current = slot.load(Ordering::Acquire);
    if current == 0 || current == wrapped {
        log(if current == 0 { "relaunch tap: HELPER_HLunaServiceMessage is empty" }
            else { "relaunch tap: already installed" });
        return;
    }
    // The slot is only ours to wrap while it holds libhelpers' own `HLunaServiceMessage`: another
    // value (a different SDL build, a slot of some other type) would be called as a function
    // pointer it is not.
    let expected = plx_base::dynlib::Handle::open_loaded(&["libhelpers.so.2"])
        .and_then(|(h, _)| h.sym("HLunaServiceMessage"))
        .filter(|p| !p.is_null())
        .map(|p| p as usize);
    if !slot_holds(current, expected) {
        log("relaunch tap: slot does not hold libhelpers' HLunaServiceMessage \u{2014} not installed");
        return;
    }
    REAL_MESSAGE.store(current, Ordering::Release);
    MAIN_THREAD.store(libc::pthread_self() as usize, Ordering::Relaxed);
    slot.store(wrapped, Ordering::Release);
    log("relaunch tap: installed");
}

/// Does the SDL slot hold exactly the function `libhelpers.so.2` exports as `HLunaServiceMessage`?
/// The Thumb bit is compared as `dlsym` and the slot both carry it. `expected` is `None` when
/// libhelpers is not mapped or lacks the symbol, and then nothing holds.
fn slot_holds(current: usize, expected: Option<usize>) -> bool {
    expected == Some(current)
}

/// Is `addr` aligned for the `AtomicUsize` the slot is read and written as?
fn slot_aligned(addr: usize) -> bool {
    addr % core::mem::align_of::<usize>() == 0
}

type LunaMessageFn = unsafe extern "C" fn(*mut std::ffi::c_void) -> *const std::ffi::c_char;
static REAL_MESSAGE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static MAIN_THREAD: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// SDL's payload read, passed through. Every Luna reply SDL subscribed to comes through here, so
/// anything that is not a relaunch costs one substring scan and nothing else.
unsafe extern "C" fn wrapped_message(msg: *mut std::ffi::c_void) -> *const std::ffi::c_char {
    let real = REAL_MESSAGE.load(std::sync::atomic::Ordering::Acquire);
    if real == 0 {
        return std::ptr::null();
    }
    let real: LunaMessageFn = std::mem::transmute(real);
    let p = real(msg);
    if !p.is_null() {
        let payload = std::ffi::CStr::from_ptr(p).to_string_lossy();
        if payload.contains("\"relaunch\"") {
            let main = libc::pthread_self() as usize == MAIN_THREAD.load(std::sync::atomic::Ordering::Relaxed);
            log(&format!("relaunch tap: {} bytes on the {} thread, parameters={}",
                payload.len(), if main { "main" } else { "other" }, payload.contains("\"parameters\"")));
            plx_os::tv::launch::offer(&payload);
        }
    }
    p
}

#[cfg(test)]
mod relaunch_tap_tests {
    use super::slot_holds;

    #[test]
    fn relaunch_slot_must_hold_libhelpers() {
        assert!(slot_holds(0x1000, Some(0x1000)), "the slot holds libhelpers' function");
        assert!(!slot_holds(0x1000, Some(0x1001)), "a different value, even by the Thumb bit");
        assert!(!slot_holds(0x1000, None), "libhelpers is not mapped or lacks the symbol");
        assert!(!slot_holds(0, None));
    }

    #[test]
    fn the_slot_must_be_word_aligned() {
        use super::slot_aligned;
        let word = core::mem::align_of::<usize>();
        assert!(slot_aligned(0x1000) && slot_aligned(word));
        assert!(!slot_aligned(0x1000 + 1) && !slot_aligned(word + word / 2));
    }
}

#[cfg(test)]
mod cursor_sleep_hint_tests {
    use super::*;

    #[test]
    fn the_idle_hide_hint_is_asked_for_only_where_lgs_extension_is_absent() {
        assert_eq!(cursor_sleep_hint(false), Some(c"5000"));
        assert_eq!(cursor_sleep_hint(true), None);
    }
}

#[cfg(test)]
mod wayland_tests {
    use super::*;

    #[test]
    fn failed_refresh_cannot_reuse_a_previous_surface() {
        let _guard = plx_base::testlock::serial();
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
        let _guard = plx_base::testlock::serial();
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
            G_OPAQUE_SENT = 1;
            assert!(sys_release_wayland(), "a held surface answers true");
            assert!(!sys_release_wayland(), "WILL + DID background is idempotent: nothing left to release");
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

    /// A `SDL_SysWMinfo` buffer that reports Wayland with `surface` (and a fixed display).
    fn wayland_info(surface: *mut c_void) -> [u8; 512] {
        let mut info = [0u8; 512];
        info[4..8].copy_from_slice(&6i32.to_ne_bytes());
        unsafe {
            let pointers = info.as_mut_ptr().add(8) as *mut *mut c_void;
            pointers.write_unaligned(0x10 as *mut c_void);
            pointers.add(1).write_unaligned(surface);
        }
        info
    }

    /// The window crumbs `refreshes` leaves, with consent on and the compare-only memory of the
    /// last surface cleared, under the serial lock the process-wide handles need.
    fn crumbs_of(refreshes: impl FnOnce()) -> Vec<&'static str> {
        use plx_telemetry::telemetry::{consent::{self, Consent}, window};
        let _guard = plx_base::testlock::serial();
        let previous = consent::current();
        let mut enabled = Consent::default();
        enabled.errors = true;
        enabled.asked_version = consent::POLICY_VERSION;
        consent::install(enabled);
        window::close_cycle_for_test();
        window::take_recorded_for_test();
        G_WL_SURFACE_SEEN.store(0, std::sync::atomic::Ordering::Relaxed);
        refreshes();
        sys_release_wayland();
        G_WL_SURFACE_SEEN.store(0, std::sync::atomic::Ordering::Relaxed);
        consent::install(previous.unwrap_or_default());
        window::take_recorded_for_test()
    }

    #[test]
    fn a_replaced_surface_is_a_crumb_after_the_query_that_found_it() {
        let crumbs = crumbs_of(|| unsafe {
            update_wayland_info(1, &wayland_info(0x1000 as *mut c_void));
            update_wayland_info(1, &wayland_info(0x2000 as *mut c_void));
        });
        assert_eq!(crumbs, ["wm_ready", "surface_changed", "wm_ready"]);
    }

    #[test]
    fn the_same_surface_again_and_the_first_surface_are_not_changes() {
        let again = crumbs_of(|| unsafe {
            update_wayland_info(1, &wayland_info(0x1000 as *mut c_void));
            update_wayland_info(1, &wayland_info(0x1000 as *mut c_void));
        });
        assert_eq!(again, ["wm_ready", "wm_ready"]);
        let first = crumbs_of(|| unsafe {
            update_wayland_info(0, &[0; 512]);
            update_wayland_info(1, &wayland_info(0x1000 as *mut c_void));
        });
        assert_eq!(first, ["wm_failed", "wm_ready"]);
    }

    /// The foreground grab is where a replaced surface is most likely to show: the background
    /// release has nulled the borrow, and the surface SDL answers with afterwards is compared with
    /// the one published before it.
    #[test]
    fn a_surface_replaced_across_a_background_release_is_a_crumb() {
        let crumbs = crumbs_of(|| unsafe {
            update_wayland_info(1, &wayland_info(0x1000 as *mut c_void));
            sys_release_wayland();
            update_wayland_info(1, &wayland_info(0x2000 as *mut c_void));
        });
        assert!(crumbs.contains(&"surface_changed"), "{crumbs:?}");
    }

    #[test]
    fn foreign_or_incomplete_wm_info_cannot_supply_wayland_handles() {
        let _guard = plx_base::testlock::serial();
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

#[cfg(test)]
mod cursor_door_tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    const BLANK: usize = 0x99;

    thread_local! {
        static CURRENT: Cell<usize> = const { Cell::new(0) };
        static DEFAULT: Cell<usize> = const { Cell::new(0) };
        static SET: RefCell<Vec<usize>> = const { RefCell::new(Vec::new()) };
        static LG: RefCell<Vec<c_int>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn set(c: *mut c_void) {
        SET.with(|s| s.borrow_mut().push(c as usize));
        CURRENT.with(|cur| cur.set(c as usize));
    }
    unsafe extern "C" fn get() -> *mut c_void {
        CURRENT.with(Cell::get) as *mut c_void
    }
    unsafe extern "C" fn def() -> *mut c_void {
        DEFAULT.with(Cell::get) as *mut c_void
    }
    unsafe extern "C" fn lg(visible: c_int) -> c_int {
        LG.with(|l| l.borrow_mut().push(visible));
        0
    }

    /// A fresh door with SDL's current cursor and default cursor as given (0 = NULL).
    fn sdl(current: usize, default: usize) -> CursorDoor {
        CURRENT.with(|c| c.set(current));
        DEFAULT.with(|d| d.set(default));
        SET.with(|s| s.borrow_mut().clear());
        CursorDoor::Sdl { set, get, def, blank: BLANK }
    }
    fn installed() -> Vec<usize> {
        SET.with(|s| s.borrow().clone())
    }

    #[test]
    fn hiding_swaps_in_the_blank_cursor_and_motion_puts_the_previous_one_back() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0x10, 0x20);
        unsafe {
            hide_through(door, &shown);
            assert_eq!(installed(), [BLANK]);
            show_through(door, &shown);
        }
        assert_eq!(installed(), [BLANK, 0x10]);
    }

    #[test]
    fn with_no_current_cursor_the_default_one_is_what_comes_back() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0, 0x20);
        unsafe {
            hide_through(door, &shown);
            show_through(door, &shown);
        }
        assert_eq!(installed(), [BLANK, 0x20]);
    }

    #[test]
    fn hiding_again_while_hidden_keeps_the_cursor_to_restore() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0x10, 0x20);
        unsafe {
            hide_through(door, &shown);
            hide_through(door, &shown); // `get` now answers the blank cursor
            show_through(door, &shown);
        }
        assert_eq!(installed(), [BLANK, BLANK, 0x10]);
    }

    /// With no current and no default cursor there is nothing to put back, so the pointer must not
    /// be hidden: it would never come back.
    #[test]
    fn with_nothing_to_restore_the_cursor_is_left_alone() {
        let shown = AtomicUsize::new(0);
        let door = sdl(0, 0);
        unsafe {
            hide_through(door, &shown);
            show_through(door, &shown);
        }
        assert_eq!(installed(), Vec::<usize>::new());
        assert_eq!(shown.load(Relaxed), 0);
    }

    #[test]
    fn lgs_extension_is_asked_to_hide_and_motion_leaves_it_to_the_compositor() {
        let shown = AtomicUsize::new(0);
        LG.with(|l| l.borrow_mut().clear());
        unsafe {
            hide_through(CursorDoor::Lg(lg), &shown);
            show_through(CursorDoor::Lg(lg), &shown);
        }
        assert_eq!(LG.with(|l| l.borrow().clone()), [0]);
    }
}
