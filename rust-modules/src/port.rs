//! The webOS port: the two tables that fill the platform, and the C entry that hands them over.
//!
//! `plx_os::tv::Services` is what the lower layers read, and `app::platform::Platform` adds the shell
//! and the frame probe the loop alone reads. Everything outside this module reaches the television
//! through those; this is the only place that names `webos`, `keymanager`, `system` and the native
//! video sink together. The simulator and the macOS app are not on these tables: they are their own
//! platform (`platform_desktop`), which never names this module, so a desktop build compiles the
//! television's code and the linker drops it.
use crate::app::adapters::backends;
use crate::app::platform::{FrameProbe, MediaBoot, Platform, Shell};
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

/// The webOS shell: the SDL window's Wayland surface, the opaque-region hint, the LS2 bus pump and the
/// Magic Remote's pointer. The bodies stay in `crate::system` (and `webos::bind_window`); this is the
/// table over them.
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
    fn hide_pointer(&self) {
        crate::system::hide_cursor();
    }
    fn show_pointer_after_motion(&self) {
        crate::system::show_cursor_after_motion();
    }
    fn hide_is_cursor_swap(&self) -> bool {
        crate::system::cursor_is_transparent()
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
/// for the lower layers and keeps it for the loop. Its media boot is the television's: the plane
/// owner's boot, and the Feed plus the product URI driver. The bodies are the application's
/// `adapters::backends` until the webOS platform owns that file: the dev surface and the tests name the
/// URI product there, and nothing outside this module may name `port`.
static PLATFORM: Platform = Platform::new(
    &SERVICES,
    &WebosShell,
    &WebosProbe,
    MediaBoot { early_init: backends::acb_init, compose: backends::compose_native },
);

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

    /// The webOS table states the storage helper as the canonical home of the session, the install's
    /// preferences and the consent record: on a television that is DB8, and a table that stated
    /// `Files` would say the app's own files are.
    #[test]
    fn the_webos_table_states_the_storage_helper_as_the_authority() {
        assert_eq!(SERVICES.storage.authority, Authority::Helper);
        assert_eq!(SERVICES.secure.mode, plx_os::tv::secure::StoreMode::Durable);
    }

    /// Every `.rs` file under the application crate's `src`, as text.
    fn crate_sources() -> Vec<String> {
        fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).expect("read a source directory") {
                let path = entry.expect("a directory entry").path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    out.push(std::fs::read_to_string(&path).expect("read a source file"));
                }
            }
        }
        let mut out = Vec::new();
        walk(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut out);
        out
    }

    /// `run_application` installs the services as its first statement, before anything can read
    /// them: a process that has installed none reads the host-test answers, which for the storage
    /// group is `Files`, so a television that persisted ahead of its install would be told it has no
    /// helper. One production install, and the only caller of `tv::install` is the `Platform` method
    /// it goes through. Reads the source, like `app::boot`'s `seam_order_tests`, because no host test
    /// can run the entry.
    #[test]
    fn the_services_are_installed_first_and_only_there() {
        let body = include_str!("app/mod.rs")
            .split_once("fn run_application(")
            .expect("run_application")
            .1
            .split_once('{')
            .expect("run_application's body")
            .1;
        let first = body
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            .expect("a first statement");
        let first = first.split_once("//").map_or(first, |(code, _)| code).trim();
        assert_eq!(
            first,
            concat!("platform.install_", "services();"),
            "the install must be run_application's first statement"
        );
        let sources = crate_sources();
        let count = |needle: &str| sources.iter().map(|text| text.matches(needle).count()).sum::<usize>();
        assert_eq!(
            count(concat!(".install_", "services()")),
            1,
            "one production install; a second would be refused by tv and could hide a missing first"
        );
        assert_eq!(
            count(concat!("tv::", "install(")),
            1,
            "`tv::install` is called by `Platform::install_services` and nowhere else"
        );
    }
}
