//! The webOS port: the one table that fills the `tv` interfaces, and the C entry that installs it.
//!
//! Everything outside this module reaches the television through `plx_platform::tv`; this is the only
//! place that names `webos`, `keymanager`, `system` and `player::ffi` together. Its functions keep
//! their own `cfg(any(hostsim, test))` arms, so the simulator installs this same table and behaves
//! as it always did.
use std::os::raw::{c_char, c_int};

#[cfg(all(not(feature = "hostsim"), not(test)))]
const SINK: &dyn plx_platform::tv::sink::VideoSink = &crate::player::ffi::StarfishSink;
#[cfg(feature = "hostsim")]
const SINK: &dyn plx_platform::tv::sink::VideoSink = &crate::player::ffi_host::HostSink;
#[cfg(all(not(feature = "hostsim"), test))]
const SINK: &dyn plx_platform::tv::sink::VideoSink = &plx_platform::tv::sink::NoSink;

static PORT: plx_platform::tv::Port = plx_platform::tv::Port {
    probe_device: plx_platform::webos::probe,
    start_capability_probe: plx_platform::webos::caps::start_probe,
    repair_sandbox: plx_platform::webos::jail_repair::execute,
    seal: plx_platform::keymanager::seal,
    open: plx_platform::keymanager::open,
    remove: plx_platform::keymanager::remove,
    system_locale: plx_platform::webos::system_locale,
    go_home: plx_platform::webos::go_home,
    poll_home: plx_platform::webos::poll_home,
    deliver_toast: plx_platform::webos::toast::deliver,
    bind_window: plx_platform::webos::bind_window,
    grab_surface: crate::system::sys_grab_wayland,
    release_surface: crate::system::sys_release_wayland,
    arm_opaque_region: crate::system::opaque_region_init,
    opaque_route: crate::system::opaque_route,
    clear_opaque_region: crate::system::clear_opaque_region,
    pump_bus: crate::system::ls2_pump,
    frame_probe_request: crate::system::frame_probe_request,
    frame_probe_waiting: crate::system::frame_probe_waiting,
    frame_probe_acquired: crate::system::frame_probe_acquired,
    frame_probe_fields: crate::system::frame_probe_fields,
    sink: SINK,
};

/// The C shim's entry (`src/main.c`) and the simulator's (`src/bin/sim.rs`): install the port, then
/// hand over to `app::run_application`.
#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    let _ = plx_platform::tv::install(&PORT);
    #[cfg(target_os = "linux")]
    let _ = plx_platform::storage::client::install_activator(plx_platform::webos::activate_storage_helper);
    crate::app::run_application(pms_host, pms_port)
}
