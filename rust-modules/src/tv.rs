//! The television, as everything outside the port sees it (step L15, docs/module-layers.md).
pub(crate) mod device;
pub(crate) mod home;
pub(crate) mod sandbox;
pub(crate) mod secure;
pub(crate) mod sink;
pub(crate) mod toast;
pub(crate) mod window;

use std::os::raw::c_void;
use std::sync::OnceLock;

/// What the platform's settings service said about the locale. i18n owns the parse and the log lines.
#[cfg_attr(any(feature = "hostsim", test), allow(dead_code))] // Reply/Unavailable: TV arm only
pub(crate) enum LocaleReply { NoPlatform, Unavailable, Reply(String) }

pub(crate) struct Port {
    pub(crate) probe_device: fn(),
    pub(crate) start_capability_probe: fn(),
    pub(crate) repair_sandbox: fn() -> Result<(), sandbox::Failure>,
    pub(crate) seal: fn(&[u8]) -> Option<secure::Sealed>,
    pub(crate) open: fn(&secure::Sealed) -> Option<Vec<u8>>,
    pub(crate) remove: fn(&secure::Backend, &str),
    #[cfg_attr(test, allow(dead_code))] // read only by i18n's non-test platform_locale
    pub(crate) system_locale: fn() -> LocaleReply,
    pub(crate) go_home: fn(),
    pub(crate) poll_home: fn(),
    pub(crate) deliver_toast: fn(&str, toast::Identity) -> toast::Sent,
    pub(crate) bind_window: fn(*mut c_void),
    pub(crate) grab_surface: fn(*mut c_void),
    pub(crate) release_surface: fn(),
    pub(crate) arm_opaque_region: fn(),
    pub(crate) opaque_route: fn(bool),
    pub(crate) clear_opaque_region: fn(),
    pub(crate) pump_bus: fn(),
    pub(crate) frame_probe_request: fn(),
    pub(crate) frame_probe_waiting: fn(),
    pub(crate) frame_probe_acquired: fn(),
    pub(crate) frame_probe_fields: fn() -> String,
    /// The Starfish-shaped verb sink (see `sink::VideoSink`); the OS-neutral sink is step L15b.
    /// Read only by [`sink::installed`], the one way out of `tv` for it.
    pub(crate) sink: &'static dyn sink::VideoSink,
}

fn nothing() {}

/// No port installed: host unit tests (nothing calls `plex_run`). Every entry is what today's
/// `cfg(any(hostsim, test))` arm answered, minus the off-device log lines the other entries would
/// write (`deliver_toast` keeps its one).
static ABSENT: Port = Port {
    probe_device: nothing,
    start_capability_probe: nothing,
    repair_sandbox: || Err(sandbox::Failure::Unsupported),
    seal: |_| None,
    open: |_| None,
    remove: |_, _| {},
    system_locale: || LocaleReply::NoPlatform,
    go_home: nothing,
    poll_home: nothing,
    deliver_toast: toast::deliver_without_port,
    bind_window: |_| {},
    grab_surface: |_| {},
    release_surface: nothing,
    arm_opaque_region: nothing,
    opaque_route: |_| {},
    clear_opaque_region: nothing,
    pump_bus: nothing,
    frame_probe_request: nothing,
    frame_probe_waiting: nothing,
    frame_probe_acquired: nothing,
    frame_probe_fields: String::new,
    sink: &sink::NoSink,
};

static INSTALLED: OnceLock<&'static Port> = OnceLock::new();

/// Once, first thing in `plex_run`. A second call is refused and changes nothing.
pub(crate) fn install(port: &'static Port) -> Result<(), &'static Port> { INSTALLED.set(port) }

/// The installed port. Only `tv`'s own modules read the table, so no other module can issue a
/// verb the interfaces below do not offer; the sink leaves through [`sink::installed`].
pub(in crate::tv) fn port() -> &'static Port { INSTALLED.get().copied().unwrap_or_else(absent) }

/// Nothing installed. A host test expects that; the shipping build never should, and a silent
/// fall back to the no-port defaults would read as a television that does nothing, so say it once.
fn absent() -> &'static Port {
    #[cfg(not(test))]
    {
        static SAID: std::sync::Once = std::sync::Once::new();
        SAID.call_once(|| plx_base::eventlog::log("tv: port not installed - using the no-port defaults"));
    }
    &ABSENT
}

pub(crate) fn probe_device() { (port().probe_device)() }
pub(crate) fn start_capability_probe() { (port().start_capability_probe)() }
#[cfg(not(test))]
pub(crate) fn system_locale() -> LocaleReply { (port().system_locale)() }
