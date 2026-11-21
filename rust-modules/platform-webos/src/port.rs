//! The webOS port: the two tables that fill the platform, and the C entry that hands them over.
//!
//! `plx_os::tv::Services` is what the lower layers read, and `host::Platform` (the application's
//! `app::platform::Platform`) adds the shell and the frame probe the loop alone reads. Everything outside
//! this crate reaches the television through those; this is the only place that names `webos`,
//! `keymanager`, `system` and the native video sink together. The simulator and the macOS app are not on
//! these tables: they are their own platform (`plx_platform_desktop`), a different crate that never names
//! this one, so a desktop executable does not contain the television's code at all.
use plxnative_modules::host::{self, sdl_init, sdl_open, Exit, FrameProbe, InitHooks, MainThread, OpenAsk, Platform, Shell, Surface};
use std::os::raw::{c_char, c_int, c_void};

#[cfg(all(not(test), feature = "play-feed"))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &crate::webos::video_sink::StarfishSink;
#[cfg(any(test, not(feature = "play-feed")))]
const SINK: &dyn plx_os::tv::sink::VideoSink = &plx_os::tv::sink::NoSink;

// URI-only native builds retain the display vtable without the packet-feed vtable.
#[cfg(not(test))]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = &crate::webos::video_sink::StarfishSink;
#[cfg(test)]
const DISPLAY: &dyn plx_os::tv::sink::DisplaySink = SINK;

/// What the lower layers read: the webOS identity, key manager, Home key and toast, the storage helper
/// and the native playback seam. The window, the bus pump and the frame probe are the loop's own, in [`PLATFORM`].
static SERVICES: plx_os::tv::Services = plx_os::tv::Services {
    identity: plx_os::tv::IdentityHooks {
        probe_device: crate::webos::probe,
        start_capability_probe: crate::webos::caps::start_probe,
        repair_native_video: Some(crate::webos::jail_repair::execute),
        device_files: crate::webos::DEVICE_FILES,
        system_locale: crate::webos::system_locale,
        sound_output: crate::webos::sound_output,
    },
    secure: plx_os::tv::SecureHooks {
        seal: crate::keymanager::seal,
        open: crate::keymanager::open,
        remove: crate::keymanager::remove,
        mode: plx_os::tv::secure::StoreMode::Durable,
    },
    system: plx_os::tv::SystemHooks {
        go_home: crate::webos::go_home,
        poll_home: crate::webos::poll_home,
        deliver_toast: crate::webos::toast::deliver,
    },
    storage: plx_os::tv::StorageHooks { authority: plx_os::tv::authority::Authority::Helper },
    legacy_webos_media: plx_os::tv::LegacyWebosMedia {
        sink: SINK,
        display_sink: DISPLAY,
        uri_media: crate::webos::URI_MEDIA,
    },
};

/// The webOS shell: the SDL window's Wayland surface, the opaque-region hint, the LS2 bus pump and the
/// Magic Remote's pointer. The bodies stay in `crate::system` (and `webos::bind_window`); this is the
/// table over them. SDL itself, and the window it opens, are the shell crate's (`plx_shell_sdl`), which
/// this platform brings up with LG's two boot hooks around `SDL_Init`.
struct WebosShell;

impl Shell for WebosShell {
    fn init(&self, mt: &MainThread) -> Result<(), Exit> {
        // SAFETY: both hooks are LG's SDL boot steps (`crate::system`), run on the SDL main thread `mt`
        // proves, at the two points `plx_shell_sdl::init::init` calls them: the cursor hint before
        // `SDL_Init`, which reads it, and the relaunch tap after, once LG's SDL has filled its slot.
        unsafe {
            sdl_init(
                mt,
                InitHooks {
                    before: crate::system::webos3_cursor_sleep_hint,
                    after: crate::system::tap_relaunch_payloads,
                },
            )
        }
    }
    fn open(&self, mt: &MainThread, ask: &OpenAsk) -> Result<Box<dyn Surface>, Exit> {
        sdl_open(mt, ask)
    }
    fn bind_window(&self, win: *mut c_void) {
        crate::webos::bind_window(win);
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

/// The webOS platform: what `plex_main` hands `host::run`, which installs its services for the lower
/// layers and keeps it for the loop. Its media boot is the television's: the plane owner's boot, and the
/// Feed plus the product URI driver. The bodies are the application's `adapters::backends`, handed over as
/// the ready value `host::WEBOS_MEDIA`, until the webOS platform owns that file (W11a): the dev surface and
/// the tests name the URI product there, and nothing outside this crate may name `port`.
static PLATFORM: Platform = Platform::new(
    &SERVICES,
    &WebosShell,
    &WebosProbe,
    host::WEBOS_MEDIA,
);

/// The C shim's entry (`src/main.c`). The `-> c_int` is only the C signature: `plex_main` never
/// returns, so neither does this.
#[no_mangle]
pub extern "C" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    plex_main(pms_host, pms_port)
}

/// The process entry: hand the platform to the application (`host::run`), then end the process with the
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
    let _ = plx_os::storage::client::install_activator(crate::webos::activate_storage_helper);
    plx_gfx::gfx::install_clear_probe(crate::system::frame_probe_waiting, crate::system::frame_probe_acquired);
    // SAFETY: `src/main.c` hands `PMS_HOST`, a NUL-terminated string literal, so the pointer is never NULL.
    let host = unsafe { std::ffi::CStr::from_ptr(pms_host) };
    let port = u16::try_from(pms_port).expect("PMS_PORT is a TCP port");
    plx_base::process::exit_now(host::run(&PLATFORM, host, port).code)
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

    /// **The two LG hooks are the webOS shell's `init`, handed in the order SDL needs.** The shell crate
    /// pins the order inside `plx_shell_sdl::init::init` (`before`, then `SDL_Init`, then `after`) and
    /// cannot name the hooks; this pins that the webOS shell hands exactly them, the cursor hint as the
    /// step BEFORE (SDL reads it in `SDL_Init`, so a hint set after does nothing) and the relaunch tap as
    /// the step AFTER (LG's SDL fills the slot it wraps inside `SDL_Init`), once each, and that nothing
    /// else in this file names them. Reads the source, like the tests below, because no host test can
    /// run LG's SDL.
    #[test]
    fn the_webos_shell_binds_the_cursor_hint_before_and_the_relaunch_tap_after_sdl_init() {
        // The needles are split with `concat!` so this test's own text, which `include_str!` reads too,
        // can never be what they match.
        let source = include_str!("port.rs");
        let init = source
            .split_once(concat!("impl Shell for ", "WebosShell {"))
            .expect("the webOS shell")
            .1
            .split_once("    fn init(")
            .expect("its init")
            .1
            .split_once("\n    }\n")
            .expect("the end of its init")
            .0;
        let before = concat!("before: crate::system::", "webos3_cursor_sleep_hint,");
        let after = concat!("after: crate::system::", "tap_relaunch_payloads,");
        assert_eq!(init.matches(before).count(), 1, "the cursor hint is `init`'s `before`");
        assert_eq!(init.matches(after).count(), 1, "the relaunch tap is `init`'s `after`");
        assert!(init.find(before) < init.find(after), "`before` is stated first, as it runs first");
        for hook in [
            concat!("crate::system::", "webos3_cursor_sleep_hint"),
            concat!("crate::system::", "tap_relaunch_payloads"),
        ] {
            assert_eq!(source.matches(hook).count(), 1, "{hook} is named by `WebosShell::init` and nowhere else");
        }
    }
}
