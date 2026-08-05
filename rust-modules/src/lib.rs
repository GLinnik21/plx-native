//! PlxNative — an unofficial native Plex client for LG webOS.
//! Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository
//! root, and THIRD-PARTY-NOTICES.md for the components this links or redistributes.
//! Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.
//!
//! plxnative-modules — the Rust app core, built as a staticlib and linked into the C
//! boot shim. The crate's C surface is tiny: C calls `plex_run` (app.rs) and forwards
//! the two starfish callbacks (`sf_on_event`/`acb_on_event`, player/mod.rs); everything
//! else is Rust-internal (the per-module `repr(C)` shapes are migration legacy, not ABI).
mod app; // plex_run — the Rust app core / event loop (the entry inverted from main.c)
mod aq;
mod cbuf; // fixed NUL-terminated C-string buffer read/write (shared by pms/route/posters)
mod auth; // plex.tv login/boot flow controller (PIN/QR → discovery → who's-watching → install)
mod browse; // Library browse: per-section paged catalog (sparse store + off-thread page fetches)
mod capture; // dev live UI capture stream: own-GLES-frame grab → MPEG1/TS or JPEG → TCP (UI plane only)
mod dev; // the /tmp/plxnative-* trigger surface, behind one `devtriggers` feature — read it before adding a trigger
#[macro_use]
mod dynlib; // dlopen-by-SONAME-candidate: the libraries whose major moves between webOS releases
mod ff; // FFmpeg (libavformat/libavcodec/libavutil) demuxer — the TV's own FFmpeg 3.3 via the stub-.so link
mod gfx;
mod img;
mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
mod person; // person/actor page data layer: the header handed in by the cast row + /library/people/{id}/media
mod player; // buffer-feed video engine (was playback.c) — step 5
mod plex; // typed Plex API layer (rust-modules/src/plex/) — one method per PMS operation (the live READ layer; playback ops still in route.rs)
mod paths; // where the app's own files live — /proc/self/exe, not a hardcoded install prefix
mod pms;
mod posters;
mod remote; // dev/testing remote-control channel: a FIFO the loop drains into synthetic SDL keys
mod route; // play_movie route selection (direct-play vs transcode) — step 3
mod stream;
mod surface; // what we are actually drawing into — drawable vs the 1920x1080 logical canvas
mod svg; // runtime SVG rasterizer FFI (src/svg.c / nanosvg) — vector icon assets
mod system;
mod task; // the one spawn: a refused thread is a return value, not a panic that kills the app

#[cfg(test)]
pub(crate) mod testlock {
    //! One lock for every test that touches a process-global.
    //!
    //! The app's async seams are process-wide by construction — `static mut CURRENT`, route's play
    //! mailbox, the player's SHARED block — so tests in DIFFERENT modules contend on the same
    //! state and `cargo test` threads them. A per-module mutex cannot see that: the season and
    //! detail mailboxes are two test functions in one file, but the season generation also moves
    //! under `pump_detail` (which calls `supersede_season`).
    //!
    //! Hold the guard for the whole test. Poison is stepped over so a failing test reports ITS
    //! assertion instead of dragging every later one down with a poison panic.
    static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        GLOBALS.lock().unwrap_or_else(|e| e.into_inner())
    }
}
mod text;
mod ui; // retui — retained UI framework; ui/home.rs now owns the home-screen C ABI

/// Append one line to the on-device event log (`/tmp/plxnative-events.log`) — the primary debugging
/// surface (`make run` fetches it). The ONE shared sink; modules bring it in as `use crate::log;`.
pub(crate) fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/plxnative-events.log") {
        let _ = writeln!(f, "{m}");
    }
}
