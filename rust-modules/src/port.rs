//! The webOS port: the one table that fills the `tv` interfaces, and the C entry that installs it.
//!
//! Everything outside this module reaches the television through `plx_os::tv`; this is the only
//! place that names `webos`, `keymanager`, `system` and the native video sink together. Its functions keep
//! their own `cfg(any(hostsim, test))` arms, so the simulator installs this same table and behaves
//! as it always did.
use std::os::raw::{c_char, c_int};

#[cfg(all(not(feature = "hostsim"), not(test), feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::webos::video_sink::StarfishSink;
#[cfg(all(feature = "hostsim", feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &crate::app::adapters::hostsim::HOST_SINK;
#[cfg(any(
    all(feature = "hostsim", not(feature = "play-feed")),
    all(not(feature = "hostsim"), any(test, not(feature = "play-feed"))),
))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::tv::sink::NoSink;

// URI-only native builds retain the display vtable without the packet-feed vtable.
#[cfg(all(not(feature = "hostsim"), not(test)))]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = &plx_os::webos::video_sink::StarfishSink;
#[cfg(any(feature = "hostsim", test))]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = SINK;

static PORT: plx_os::tv::Port = plx_os::tv::Port {
    probe_device: plx_os::webos::probe,
    start_capability_probe: plx_os::webos::caps::start_probe,
    repair_sandbox: plx_os::webos::jail_repair::execute,
    seal: plx_os::keymanager::seal,
    open: plx_os::keymanager::open,
    remove: plx_os::keymanager::remove,
    system_locale: plx_os::webos::system_locale,
    sound_output: plx_os::webos::sound_output,
    go_home: plx_os::webos::go_home,
    poll_home: plx_os::webos::poll_home,
    deliver_toast: plx_os::webos::toast::deliver,
    bind_window: plx_os::webos::bind_window,
    grab_surface: crate::system::sys_grab_wayland,
    release_surface: crate::system::sys_release_wayland,
    arm_opaque_region: crate::system::opaque_region_init,
    opaque_route: crate::system::opaque_route,
    clear_opaque_region: crate::system::clear_opaque_region,
    pump_bus: crate::system::ls2_pump,
    frame_probe_request: crate::system::frame_probe_request,
    frame_probe_prepare_begin: crate::system::frame_probe_prepare_begin,
    frame_probe_prepare_end: crate::system::frame_probe_prepare_end,
    frame_probe_waiting: crate::system::frame_probe_waiting,
    frame_probe_acquired: crate::system::frame_probe_acquired,
    frame_probe_fields: crate::system::frame_probe_fields,
    sink: SINK,
    display_sink: DISPLAY,
    uri_media: plx_os::webos::URI_MEDIA,
};

/// The C shim's entry (`src/main.c`) and the simulator's (`src/bin/sim.rs`). The `-> c_int` is only
/// the C signature: `plex_main` never returns, so neither does this.
#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    plex_main(pms_host, pms_port)
}

/// The process entry: install the port, hand over to `app::run_application`, then end the process
/// with the status it returned.
///
/// The executable owns the process end: `exit_now` (`_exit`, never libc's `exit`, #1115) right after
/// the app returns, in the order the app always ended in. A process that hosts the app never calls it.
///
/// It is typed `-> !` rather than left to diverge through its last expression. `src/main.c` does
/// `return plex_run(..)`, so an entry that returned would fall into `__libc_start_main`, which calls
/// libc `exit` directly (the interposer in `src/exit.c` cannot catch that), and `OPENSSL_cleanup`
/// would then free libcrypto's locks under the live curl workers (#1115). The `const` below is the
/// compile-time proof that replaced the ones `app::boot`'s `seam_order_tests` held for
/// `run_application` and `run_and_shutdown` when those started to return an `Exit`: a later edit that
/// lets this return does not compile.
fn plex_main(pms_host: *const c_char, pms_port: c_int) -> ! {
    let _ = plx_os::tv::install(&PORT);
    #[cfg(target_os = "linux")]
    let _ = plx_os::storage::client::install_activator(plx_os::webos::activate_storage_helper);
    plx_base::process::exit_now(crate::app::run_application(pms_host, pms_port).code)
}

const _: fn(*const c_char, c_int) -> ! = plex_main;

/// `src/main.c` hands over SAM's launch payload (`argv[1]`) before `plex_run`. Kept raw in
/// `tv::launch`; `app::deeplink` reads it once the app has a session to act in.
///
/// # Safety
/// `json` is NULL or a NUL-terminated string that outlives the call.
#[no_mangle]
pub unsafe extern "C" fn plx_launch_args(json: *const c_char) {
    if json.is_null() {
        return;
    }
    let raw = unsafe { std::ffi::CStr::from_ptr(json) }.to_string_lossy();
    plx_os::tv::launch::offer(&raw);
}

#[cfg(all(test, feature = "hostsim", feature = "play-feed"))]
mod tests {
    use super::PORT;

    /// The app's plane boot resolves the display through `display_installed()` (this table's
    /// `display_sink`), where media's old `acb_init` resolved it through `player::display()`. In
    /// the simulator both must be the host sink, never `NoSink`: a boot that read `NoSink` would
    /// report no plane to every backend and nothing in a simulator run would say so.
    #[test]
    fn the_simulator_port_display_is_the_host_sink_the_acb_init_reads() {
        let host = std::ptr::from_ref(&crate::app::adapters::hostsim::HOST_SINK).cast::<()>();
        assert_eq!(std::ptr::from_ref(PORT.display_sink).cast::<()>(), host,
            "display_installed() is the host sink");
        assert_eq!(std::ptr::from_ref(PORT.sink).cast::<()>(), host, "so is the video sink");
    }
}
