//! PlxNative — an unofficial native Plex client for LG webOS.
//! Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository
//! root, and THIRD-PARTY-NOTICES.md for the components this links or redistributes.
//! Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.
//!
//! plxnative-modules — the Rust app core, an rlib the platform crates build on. The staticlib the C
//! boot shim links is the webOS platform crate's (`plx_platform_webos`, `cargo rustc -p
//! plx_platform_webos --crate-type staticlib`), which bundles this crate with the rest of its graph. The
//! C surface of that archive is tiny: C calls `plex_run` and `plx_launch_args` (the platform crate's
//! port.rs), writes the fallback image marker through `plx_crash_write_image_marker`, re-enters the
//! native-crash spool through `plx_sentry_spool_external`, and forwards the two library-thread callbacks
//! (`sf_on_event`, `plx_play_feed`'s sf_event.rs; `acb_on_event`, the platform crate's `webos::video_sink`).
//! Everything else is Rust-internal (the per-module `repr(C)` shapes are migration legacy, not ABI).
// Host checks may build without an executor; a native television artifact must select one.
#[cfg(all(target_arch = "arm", target_os = "linux", not(feature = "hostsim"),
    not(any(feature = "play-uri", feature = "play-feed"))))]
compile_error!("native playback requires play-uri or play-feed");
mod app; // run_application — the Rust app core / event loop (the entry inverted from main.c; the webOS platform crate's `plex_run` hands over to it)
mod capture; // dev live UI capture stream: own-GLES-frame grab → MPEG1/TS or JPEG → TCP (UI plane only)
mod coldstart; // retires old last-page bookmarks; authenticated cold boots now stay on Home
mod dev; // the /tmp/plxnative-* trigger surface, behind one `devtriggers` feature — read it before adding a trigger
mod focusprobe; // dev: one diffable line naming everything app.rs's key ladder can move, logged when it changes
/// The one narrow public face of the application for a platform crate: the types it builds and implements
/// (`Platform`, `Shell`, `Surface`, `EventSink`, ...), the SDL helpers, the entry `run` and the ready media values
/// (`WEBOS_MEDIA`, and under `hostsim` `FEED_MEDIA` and `HOST_SINK`), as re-exports. Every other module of
/// the application stays private to a television build (`docs/platform-ports.md`, Target shape 8; a `hostsim`
/// build also exposes the simulator binary's own doors below).
pub mod host;
// The simulator's HUD tools (`--hud-lint`, `--hud-schema`, `--hud-export`): a library function the thin
// `platform-desktop/src/bin/sim.rs` calls before it reads a PMS host. `pub` because that binary is a separate crate; absent
// from the television build, which has no use for a linter and no argv to read it from.
#[cfg(any(test, feature = "hostsim"))]
pub mod hud_cli;
#[cfg(any(test, feature = "hostsim", feature = "devtriggers"))]
mod hud_tools;
mod lab; // Cloud Lab bridge: pinned diagnostic uploads + optional outbound command long-poll
// Pure RELEASE_LINE-parsing helpers, `include!`d verbatim by build.rs so `cargo test --lib`
// actually runs their unit tests (see the module for why). Nothing in the app itself calls
// them at runtime — the version rule they implement is applied once, at compile time, by
// build.rs — so they exist in THIS crate only for the test build; `#[cfg(test)]` here, not on
// the functions themselves, because build.rs's own separate compilation is never built with
// `--test` and needs them unconditionally.
#[cfg(test)]
mod release_line;
mod remote; // dev/testing remote-control channel: a FIFO the loop drains into synthetic SDL keys
#[cfg(feature = "hostsim")]
mod shot; // simulator screenshots: read the frame back and write a PNG (see the module doc)
mod textinput; // the TV's own on-screen keyboard, via plain SDL_StartTextInput (see the module doc)

/// The instance root, for the simulator binary.
///
/// `platform-desktop/src/bin/sim.rs` is a separate crate and cannot see `pub(crate)` items, but it must create the
/// directory and truncate the event log inside it before the app starts. Exposing the resolver
/// keeps ONE definition of where that is — a second `env::var` read in the binary would be a
/// second answer waiting to drift from this one.
#[cfg(feature = "hostsim")]
pub fn sim_runtime_dir() -> std::path::PathBuf {
    plx_base::paths::runtime_dir().to_path_buf()
}

/// Publish a webOS major version for this simulator run (`PLXNATIVE_SIM_WEBOS_MAJOR`, read by the
/// binary): the simulator has no firmware file, so it otherwise reports "unknown" and the
/// webOS-1-3-only Interface resolution row stays hidden.
#[cfg(feature = "hostsim")]
pub fn publish_sim_webos_major(major: u32) {
    plx_os::tv::device::publish_info(plx_os::tv::device::Info { major, ..Default::default() });
}

/// The event log's path, built by the ONE expression [`plx_base::eventlog::log`] uses.
///
/// `platform-desktop/src/bin/sim.rs` truncates this file at startup. Spelling the name a second time over there
/// would mean a rename could leave the binary truncating a file the app never appends to — the
/// simulator's log would silently start non-empty, which is exactly the state `tests/run.py` dates
/// its first line from.
#[cfg(feature = "hostsim")]
pub fn sim_events_log() -> std::path::PathBuf {
    plx_base::eventlog::events_log()
}

#[cfg(feature = "hostsim")]
pub use app::synthetic_home_initial;

