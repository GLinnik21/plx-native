//! The webOS port: the two tables that fill the platform, and the C entry that hands them over.
//!
//! `plx_os::tv::Services` is what the lower layers read, and `app::platform::Platform` adds the shell
//! and the frame probe the loop alone reads. Everything outside this module reaches the television
//! through those; this is the only place that names `webos`, `keymanager`, `system` and the native
//! video sink together. The simulator and the macOS app are not on these tables: they are their own
//! platform (`platform_desktop`), which never names this module, so a desktop build compiles the
//! television's code and the linker drops it.
use crate::app::platform::{FrameProbe, Platform, Shell};
use std::os::raw::{c_char, c_int, c_void};

#[cfg(all(not(test), feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::webos::video_sink::StarfishSink;
#[cfg(any(test, not(feature = "play-feed")))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::tv::sink::NoSink;

// URI-only native builds retain the display vtable without the packet-feed vtable.
#[cfg(not(test))]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = &plx_os::webos::video_sink::StarfishSink;
#[cfg(test)]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = SINK;

/// What the lower layers read: the webOS identity, key manager, Home key and toast, the storage helper
/// and the native playback seam. The window, the bus pump and the frame probe are the loop's own, in [`PLATFORM`].
static SERVICES: plx_os::tv::Services = plx_os::tv::Services {
    identity: plx_os::tv::IdentityHooks {
        probe_device: plx_os::webos::probe,
        start_capability_probe: plx_os::webos::caps::start_probe,
        repair_native_video: Some(plx_os::webos::jail_repair::execute),
        device_files: plx_os::webos::DEVICE_FILES,
        system_locale: plx_os::webos::system_locale,
        sound_output: plx_os::webos::sound_output,
    },
    secure: plx_os::tv::SecureHooks {
        seal: plx_os::keymanager::seal,
        open: plx_os::keymanager::open,
        remove: plx_os::keymanager::remove,
        mode: plx_os::tv::secure::StoreMode::Durable,
    },
    system: plx_os::tv::SystemHooks {
        go_home: plx_os::webos::go_home,
        poll_home: plx_os::webos::poll_home,
        deliver_toast: plx_os::webos::toast::deliver,
    },
    storage: plx_os::tv::StorageHooks { authority: plx_os::tv::authority::Authority::Helper },
    legacy_webos_media: plx_os::tv::LegacyWebosMedia {
        sink: SINK,
        display_sink: DISPLAY,
        uri_media: plx_os::webos::URI_MEDIA,
    },
};

/// The webOS shell: the SDL window's Wayland surface, the opaque-region hint and the LS2 bus pump.
/// The bodies stay in `crate::system` (and `webos::bind_window`); this is the table over them.
struct WebosShell;

impl Shell for WebosShell {
    fn bind_window(&self, win: *mut c_void) {
        plx_os::webos::bind_window(win);
    }
    fn grab_surface(&self, win: *mut c_void) {
        crate::system::sys_grab_wayland(win);
    }
    fn release_surface(&self) -> bool {
        crate::system::sys_release_wayland()
    }
    fn arm_opaque_region(&self) {
        crate::system::opaque_region_init();
    }
    fn opaque_route(&self, player: bool) {
        crate::system::opaque_route(player);
    }
    fn clear_opaque_region(&self) {
        crate::system::clear_opaque_region();
    }
    fn pump_bus(&self) {
        crate::system::ls2_pump();
    }
}

/// The webOS frame probe, over `crate::system`'s compositor-callback clock.
struct WebosProbe;

impl FrameProbe for WebosProbe {
    fn request(&self) {
        crate::system::frame_probe_request();
    }
    fn prepare_begin(&self) {
        crate::system::frame_probe_prepare_begin();
    }
    fn prepare_end(&self) {
        crate::system::frame_probe_prepare_end();
    }
    fn waiting(&self) {
        crate::system::frame_probe_waiting();
    }
    fn acquired(&self) {
        crate::system::frame_probe_acquired();
    }
    fn fields(&self) -> String {
        crate::system::frame_probe_fields()
    }
}

/// The webOS platform: what `plex_main` hands `app::run_application`, which installs its services
/// for the lower layers and keeps it for the loop.
static PLATFORM: Platform = Platform::new(&SERVICES, &WebosShell, &WebosProbe);

/// The C shim's entry (`src/main.c`). The `-> c_int` is only the C signature: `plex_main` never
/// returns, so neither does this.
#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    plex_main(pms_host, pms_port)
}

/// The process entry: hand the platform to `app::run_application`, then end the process with the
/// status it returned. The renderer's clear hook (`gfx::install_clear_probe`) takes plain functions, so
/// it is installed here, with the webOS frame probe's own, on the thread the app boots and draws on.
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
    #[cfg(target_os = "linux")]
    let _ = plx_os::storage::client::install_activator(plx_os::webos::activate_storage_helper);
    plx_gfx::gfx::install_clear_probe(crate::system::frame_probe_waiting, crate::system::frame_probe_acquired);
    plx_base::process::exit_now(crate::app::run_application(&PLATFORM, pms_host, pms_port).code)
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

#[cfg(test)]
mod tests {
    use super::SERVICES;
    use plx_os::tv::authority::Authority;

    /// The webOS table states the storage helper as the home of the session, the install's preferences
    /// and the consent record. The persistence layers choose their store from this value, so a table
    /// that stated anything else would move a television's account out of DB8.
    #[test]
    fn the_webos_table_states_the_storage_helper_as_the_authority() {
        assert_eq!(SERVICES.storage.authority, Authority::Helper);
        assert_eq!(SERVICES.secure.mode, plx_os::tv::secure::StoreMode::Durable);
    }

    /// `run_application` installs the services before anything else runs. The authority is read by
    /// whatever persists first, and an uninstalled television reads `Files` and would write a
    /// plaintext session file, so the install has to come before `exit::entry` runs `enter_application`.
    /// Reads the source, like `app::boot`'s `seam_order_tests`, because no host test can run the entry.
    #[test]
    fn the_services_are_installed_before_the_application_enters() {
        let source = include_str!("app/mod.rs");
        let body = source
            .split_once("fn run_application(")
            .expect("run_application")
            .1
            .split_once("\n}\n")
            .expect("end of run_application")
            .0;
        let install = body.find("platform.install_services();").expect("run_application installs the services");
        let enter = body.find("exit::entry(").expect("run_application enters through exit::entry");
        assert!(install < enter, "install_services must run before exit::entry");
        assert_eq!(
            source.matches("install_services()").count(),
            1,
            "one production install; a second would be refused by tv and could hide a missing first"
        );
    }
}
