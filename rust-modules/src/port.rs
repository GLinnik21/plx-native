//! The webOS port: the one table that fills the `tv` interfaces, and the C entry that installs it.
//!
//! Everything outside this module reaches the television through `crate::tv`; this is the only
//! place that names `webos`, `keymanager`, `system` and `player::ffi` together. Its functions keep
//! their own `cfg(any(hostsim, test))` arms, so the simulator installs this same table and behaves
//! as it always did.
use std::os::raw::{c_char, c_int};

#[cfg(all(not(feature = "hostsim"), not(test)))]
const SINK: &dyn crate::tv::sink::VideoSink = &crate::player::ffi::StarfishSink;
#[cfg(feature = "hostsim")]
const SINK: &dyn crate::tv::sink::VideoSink = &crate::player::ffi_host::HostSink;
#[cfg(all(not(feature = "hostsim"), test))]
const SINK: &dyn crate::tv::sink::VideoSink = &crate::tv::sink::NoSink;

static PORT: crate::tv::Port = crate::tv::Port {
    probe_device: crate::webos::probe,
    start_capability_probe: crate::webos::caps::start_probe,
    repair_sandbox: crate::webos::jail_repair::execute,
    seal: crate::keymanager::seal,
    open: crate::keymanager::open,
    remove: crate::keymanager::remove,
    system_locale: crate::webos::system_locale,
    go_home: crate::webos::go_home,
    poll_home: crate::webos::poll_home,
    deliver_toast: crate::webos::toast::deliver,
    bind_window: crate::webos::bind_window,
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
    let _ = crate::tv::install(&PORT);
    #[cfg(target_os = "linux")]
    let _ = crate::storage::client::install_activator(crate::webos::activate_storage_helper);
    crate::app::run_application(pms_host, pms_port)
}
