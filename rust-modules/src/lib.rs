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
mod ff; // FFmpeg (libavformat/libavcodec/libavutil) demuxer — the TV's own FFmpeg 3.3 via the stub-.so link
mod gfx;
mod img;
mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
mod player; // buffer-feed video engine (was playback.c) — step 5
mod plex; // typed Plex API layer (rust-modules/src/plex/) — one method per PMS operation (the live READ layer; playback ops still in route.rs)
mod pms;
mod posters;
mod remote; // dev/testing remote-control channel: a FIFO the loop drains into synthetic SDL keys
mod route; // play_movie route selection (direct-play vs transcode) — step 3
mod stream;
mod svg; // runtime SVG rasterizer FFI (src/svg.c / nanosvg) — vector icon assets
mod system;
mod task; // the one spawn: a refused thread is a return value, not a panic that kills the app
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
