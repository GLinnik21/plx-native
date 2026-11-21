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
//! The shell's hooks and the frame probe are plain no-ops, bar the two that bring SDL up and open its
//! window (`init` and `open`, the shell crate's, with nothing around `SDL_Init`): SDL owns the window
//! and its event loop, there is no compositor to hint, no bus to pump and no remote pointer to hide.
//! The secure store is
//! `SecureHooks::NONE`: nothing seals, so a session is the 0600 plaintext file the session layer
//! writes itself, which the simulator's harnesses plant and read.
//! The storage group is `StorageHooks::NONE`: there is no storage helper, so the account's records
//! live in files under the persistent state root.
//!
//! This crate names no other platform and reaches the application through `plxnative_modules::host`
//! alone, so a desktop executable contains none of the television's code: the
//! webOS port (the Wayland handles, glib, luna-service2) is `plx_platform_webos`, which no desktop crate
//! depends on and the application never names. The simulator links without a television library to drop.
//!
//! The crate is compiled only under `hostsim` (its own feature, in no default set), so a default-feature
//! pass that lists this package builds an empty library, and a television build never builds it. It is a
//! mounted module of the `platform desktop` layer (`ci/module-layers.ini`): nothing names it, and it names
//! no other platform (`docs/platform-ports.md`, W7). The `plxnative-sim` binary beside it
//! (`src/bin/sim.rs`) is its executable.
#![cfg(feature = "hostsim")]
use plxnative_modules::host::{self, sdl_init, sdl_open, Exit, FrameProbe, InitHooks, MediaBoot, OpenAsk, Platform, Shell, Surface};
use plx_os::devcaps::dv::{self, DvCapability, DvProbe, ProbeSource};
use plx_os::tv::toast::{Identity, Outcome, Sent};
use plx_os::tv::{IdentityHooks, LegacyWebosMedia, SecureHooks, Services, StorageHooks, SystemHooks};
use std::ffi::CStr;
use std::os::raw::{c_int, c_void};

/// The Feed backend's host sink, built with the simulator's decoded picture.
#[cfg(feature = "play-feed")]
const SINK: &dyn plx_os::tv::sink::VideoSink = &host::HOST_SINK;
/// A build without the Feed backend has no picture to show: every verb is refused.
#[cfg(not(feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::tv::sink::NoSink;

/// The display binding the table must state. The desktop runs no plane boot, so nothing reads it in
/// production; it is the host sink only so that the hook is never `NoSink`.
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
    storage: StorageHooks::NONE,
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

/// The desktop shell: SDL owns the window and its event loop (`init` and `open` are the shell crate's,
/// with no hook around `SDL_Init`), so nothing is bound, grabbed, hinted or pumped, and no surface is
/// ever held. The pointer is a mouse the application does not hide, so hiding and showing it do
/// nothing, and a click is always a real one (the hide is not a cursor swap).
struct DesktopShell;

impl Shell for DesktopShell {
    fn init(&self, mt: &host::MainThread) -> Result<(), Exit> {
        // SAFETY: `InitHooks::NONE` calls nothing.
        unsafe { sdl_init(mt, InitHooks::NONE) }
    }
    fn open(&self, mt: &host::MainThread, ask: &OpenAsk) -> Result<Box<dyn Surface>, Exit> {
        sdl_open(mt, ask)
    }
    fn bind_window(&self, _win: *mut c_void) {}
    fn grab_surface(&self, _win: *mut c_void) {}
    fn release_surface(&self) -> bool {
        false
    }
    fn arm_opaque_region(&self) {}
    fn opaque_route(&self, _player: bool) {}
    fn clear_opaque_region(&self) {}
    fn pump_bus(&self) {}
    fn hide_pointer(&self) {}
    fn show_pointer_after_motion(&self) {}
    fn hide_is_cursor_swap(&self) -> bool {
        false
    }
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

/// The desktop's media boot, the ready value the application publishes (`host::FEED_MEDIA`): nothing to
/// initialize before the first adapter (the plane owner's default facts are the absent ones the host
/// sink's `VP_EXPORTED`/`VP_NONE` boot would state), and the Feed over the host sink as its only driver.
/// It composes no URI driver (that is the television's ACB driver), so it returns no resolver and the
/// session is published the Feed alone.
const MEDIA: MediaBoot = host::FEED_MEDIA;

/// The desktop platform: what [`run`] hands `host::run`.
static PLATFORM: Platform = Platform::new(
    &SERVICES,
    &DesktopShell,
    &DesktopProbe,
    MEDIA,
);

/// The desktop entry: boot the application on [`PLATFORM`] and return the status it ended with.
/// Ending the process is the executable's (`src/bin/sim.rs`), which owns it, like the television's
/// entry does (`port::plex_main` in `plx_platform_webos`): a process that hosts the application never does.
///
/// Nothing is logged before the application's entry: a boot it refuses early must leave the log as the
/// harnesses expect (`tests/replay_fixtures.py`).
pub fn run(pms_host: &CStr, pms_port: u16) -> c_int {
    host::run(&PLATFORM, pms_host, pms_port).code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The desktop hides no pointer and says so**: its mouse is not the application's to hide, so a
    /// hide and a show do nothing, and the hide is not a cursor swap, which is what makes a click
    /// always a real one (`input::click_is_ok`). The simulator's pointer behaviour is exactly this.
    #[test]
    fn the_desktop_shell_hides_no_pointer_and_its_clicks_are_real() {
        let shell: &dyn Shell = &DesktopShell;
        shell.hide_pointer();
        shell.show_pointer_after_motion();
        assert!(!shell.hide_is_cursor_swap());
    }

    /// A desktop has no storage helper: the session, the install's preferences and the consent record
    /// are files. The same value a host test reads from `Services::HOST_TEST`.
    #[test]
    fn the_desktop_table_keeps_the_account_in_files() {
        assert_eq!(SERVICES.storage.authority, plx_os::tv::authority::Authority::Files);
    }

    /// **The desktop declares no SDL function.** The window, the GL context, the queue and the text-input
    /// panel are `plx_shell_sdl`'s (`init` and `open` above reach them through `host::sdl_init` and
    /// `host::sdl_open`), and the television's LG half is the webOS crate's; a declaration here, in the
    /// library or the `plxnative-sim` binary, would be a second SDL seam beside the shell crate's. Reads the
    /// sources, because no host test can see a declaration. (The application crate holds itself to
    /// `clock.rs`'s `SDL_GetTicks` the same way, and the webOS crate to the four it keeps.)
    #[test]
    fn the_desktop_declares_no_sdl_function() {
        fn walk(dir: &std::path::Path, src: &std::path::Path, hits: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("read a source directory") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    walk(&path, src, hits);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    let rel = path.strip_prefix(src).expect("under src").to_string_lossy().replace('\\', "/");
                    let text = std::fs::read_to_string(&path).expect("read a source file");
                    // Split with `concat!`, so this file's own text is never what it matches.
                    if text.contains(concat!("fn ", "SDL_")) {
                        hits.push(rel);
                    }
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut hits = Vec::new();
        walk(&src, &src, &mut hits);
        assert!(hits.is_empty(), "an SDL function is declared in the desktop platform: {hits:?}");
    }

    /// The video sink the Feed runs over and the display binding the table states are the one host
    /// sink in the simulator, never `NoSink`: a Feed that ran over `NoSink` would show no picture
    /// and nothing in a simulator run would say so.
    #[cfg(feature = "play-feed")]
    #[test]
    fn the_simulator_port_display_is_the_host_sink() {
        let sink = std::ptr::from_ref(&host::HOST_SINK).cast::<()>();
        let media = &SERVICES.legacy_webos_media;
        assert_eq!(std::ptr::from_ref(media.display_sink).cast::<()>(), sink,
            "display_installed() is the host sink");
        assert_eq!(std::ptr::from_ref(media.sink).cast::<()>(), sink, "so is the video sink");
    }

    /// The desktop's own media boot, the one its `PLATFORM` states, composes the Feed and no URI
    /// driver: the adapter's installed drivers form a set with no `Uri` in it, and the composition
    /// carries no resolver. A resolver here would describe a driver this platform never installed. (What
    /// the application then publishes to the session from a composition is `publish_composition`'s, and
    /// is the application's own to test: a platform cannot reach it.)
    #[test]
    fn the_desktop_platform_composes_the_feed_and_no_resolver() {
        use plx_media::player::adapter::PlayerAdapter;
        let mut adapter = PlayerAdapter::new(unsafe { host::MainThread::assume() });
        let composed = (MEDIA.compose)(&mut adapter, false);
        assert!(composed.resolver.is_none());
        assert!(!adapter.composed_backends().uri);
        #[cfg(feature = "play-feed")]
        assert!(adapter.composed_backends().feed, "the Feed is the desktop's driver");
    }
}
