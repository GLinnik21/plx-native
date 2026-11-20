//! The desktop platform: what `plxnative-sim` and the macOS app hand the application.
//!
//! A desktop has no television's services, so this is a platform in its own right and not the webOS
//! port with its answers faked: [`PLATFORM`] states every hook the application reads, from the
//! shared `NONE` groups of `plx_os::tv` wherever a host has nothing to say (the key manager, the
//! settings service, the device files, the URI media pipe) and with its own body where it has
//! something to do:
//!
//! - the capability probe publishes the host's answer, and honours the same `dvcaps0` / `dvcaps1`
//!   dev triggers the television's does (`devcaps::dv`, which both probes end in);
//! - the root press is a log line, the one thing a Mac window has to say about a launcher that does
//!   not exist;
//! - the system toast says it was not sent;
//! - the video sink is the Feed backend's host sink (`HOST_SINK`) when the build has one, else the
//!   refusing `NoSink`.
//!
//! The shell and the frame probe are plain no-ops: SDL owns the window and its event loop, there is
//! no compositor to hint and no bus to pump. The secure store is `SecureHooks::NONE`: nothing seals,
//! so a session is the 0600 plaintext file the session layer writes itself, which the simulator's
//! harnesses plant and read.
//!
//! This module never names `port`, `system`, `webos` or `keymanager`, so a desktop executable
//! references none of the television's code. It still COMPILES it (the television arms of those
//! files have no `hostsim` gate any more), and the linker drops what nothing reaches: a build that
//! made one of them reachable would fail to link for want of libwayland, glib or luna-service2, on
//! a Mac and on Linux alike. `simulators.yml` links the shapes the simulator ships in to guard that
//! until the platform crates split (`docs/platform-ports.md`, W7).
use crate::app::platform::{FrameProbe, Platform, Shell};
use plx_os::devcaps::dv::{self, DvCapability, DvProbe, ProbeSource};
use plx_os::tv::toast::{Identity, Outcome, Sent};
use plx_os::tv::{IdentityHooks, LegacyWebosMedia, SecureHooks, Services, SystemHooks};
use std::ffi::CStr;
use std::os::raw::{c_int, c_void};

/// The Feed backend's host sink, built with the simulator's decoded picture.
#[cfg(feature = "play-feed")]
const SINK: &dyn plx_os::tv::sink::VideoSink = &crate::app::adapters::hostsim::HOST_SINK;
/// A build without the Feed backend has no picture to show: every verb is refused.
#[cfg(not(feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::tv::sink::NoSink;

/// The display binding the player's plane boot resolves: the same sink, so the plane is the host's.
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = SINK;

/// What the lower layers read from the desktop platform. Every hook is stated, as `tv` requires: a
/// hook a host has nothing to say for is the shared `NONE` group's own, named, so a hook added to a
/// group is a compile error here until this platform states it.
static SERVICES: Services = Services {
    identity: IdentityHooks {
        probe_device: IdentityHooks::NONE.probe_device,
        start_capability_probe,
        repair_native_video: IdentityHooks::NONE.repair_native_video,
        device_files: IdentityHooks::NONE.device_files,
        system_locale: IdentityHooks::NONE.system_locale,
        sound_output: IdentityHooks::NONE.sound_output,
    },
    secure: SecureHooks::NONE,
    system: SystemHooks { go_home, poll_home: SystemHooks::NONE.poll_home, deliver_toast },
    legacy_webos_media: LegacyWebosMedia {
        sink: SINK,
        display_sink: DISPLAY,
        uri_media: LegacyWebosMedia::NONE.uri_media,
    },
};

/// The boot capability probe: a host has no configd to ask, so it answers `unknown`, from the host,
/// unless a dev trigger forces the answer. Synchronous, with no worker: the answer needs no bus,
/// and the app reads it only after boot.
fn start_capability_probe() {
    let started = std::time::Instant::now();
    if dv::publish_forced(started) {
        return;
    }
    dv::publish_logged(
        DvProbe { capability: DvCapability::Unknown, source: ProbeSource::Host, reason: "host" },
        started,
        None,
        None,
    );
}

/// The root press: there is no launcher to hand a desktop window to, so the screen does not change
/// and the press is a log line, the one the simulator's skill (`ui-sim`) tells a reader to look for.
fn go_home() {
    plx_base::eventlog::log("gohome: request mode=auto");
    plx_base::eventlog::log(
        "gohome: no LS2 bus off-device — the root press would launch com.webos.app.home on a television",
    );
}

/// The system toast: a host has no notification service this app may call, so nothing is sent.
fn deliver_toast(_message: &str, identity: Identity) -> Sent {
    plx_base::eventlog::log(&format!("toast: no system notifications on this host — {identity:?} toast not sent"));
    Sent { reply: None, outcome: Outcome::NoBus }
}

/// The desktop shell: SDL owns the window and its event loop, so nothing is bound, grabbed, hinted
/// or pumped, and no surface is ever held.
struct DesktopShell;

impl Shell for DesktopShell {
    fn bind_window(&self, _win: *mut c_void) {}
    fn grab_surface(&self, _win: *mut c_void) {}
    fn release_surface(&self) -> bool {
        false
    }
    fn arm_opaque_region(&self) {}
    fn opaque_route(&self, _player: bool) {}
    fn clear_opaque_region(&self) {}
    fn pump_bus(&self) {}
}

/// The desktop frame probe: always unarmed, there is no compositor callback to clock.
struct DesktopProbe;

impl FrameProbe for DesktopProbe {
    fn request(&self) {}
    fn prepare_begin(&self) {}
    fn prepare_end(&self) {}
    fn waiting(&self) {}
    fn acquired(&self) {}
    fn fields(&self) -> String {
        String::new()
    }
}

/// The desktop platform: what [`run`] hands `app::run_application`.
static PLATFORM: Platform = Platform::new(&SERVICES, &DesktopShell, &DesktopProbe);

/// The desktop entry: boot the application on [`PLATFORM`] and return the status it ended with.
/// Ending the process is the executable's (`src/bin/sim.rs`), which owns it, like the television's
/// entry does (`port::plex_main`): a process that hosts the application never does.
///
/// Nothing is logged before `run_application`: a boot it refuses early must leave the log as the
/// harnesses expect (`tests/replay_fixtures.py`).
pub fn run(pms_host: &CStr, pms_port: u16) -> c_int {
    crate::app::run_application(&PLATFORM, pms_host.as_ptr(), c_int::from(pms_port)).code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's plane boot resolves the display through `display_installed()` (this table's
    /// `display_sink`), where media's old `acb_init` resolved it through `player::display()`. In
    /// the simulator both must be the host sink, never `NoSink`: a boot that read `NoSink` would
    /// report no plane to every backend and nothing in a simulator run would say so.
    #[cfg(feature = "play-feed")]
    #[test]
    fn the_simulator_port_display_is_the_host_sink_the_acb_init_reads() {
        let host = std::ptr::from_ref(&crate::app::adapters::hostsim::HOST_SINK).cast::<()>();
        let media = &SERVICES.legacy_webos_media;
        assert_eq!(std::ptr::from_ref(media.display_sink).cast::<()>(), host,
            "display_installed() is the host sink");
        assert_eq!(std::ptr::from_ref(media.sink).cast::<()>(), host, "so is the video sink");
    }
}
