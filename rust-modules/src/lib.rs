//! plexpoc-modules — C modules ported to Rust, linked into the C app (hybrid
//! migration). Each module exposes the same C ABI its src/*.h declares, so the
//! remaining C code calls it unchanged. Ported: img, stream, aq, mkv, pms,
//! posters, text, gfx, system, ui_home.
mod app; // plex_run — the Rust app core / event loop (the entry inverted from main.c)
mod aq;
mod cbuf; // fixed NUL-terminated C-string buffer read/write (shared by pms/route/posters)
mod auth; // plex.tv login/boot flow controller (PIN/QR → discovery → who's-watching → install)
mod ff; // FFmpeg (libavformat/libavcodec/libavutil) demuxer — replaces mkv.rs (TV ships FFmpeg 3.4)
mod gfx;
mod img;
mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
mod mkv;
mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
mod player; // buffer-feed video engine (was playback.c) — step 5
mod plex; // typed Plex API layer (rust-modules/src/plex/) — one method per PMS operation (unused; call sites migrate later)
mod pms;
mod posters;
mod route; // play_movie route selection (direct-play vs transcode) — step 3
mod stream;
mod svg; // runtime SVG rasterizer FFI (src/svg.c / nanosvg) — vector icon assets
mod system;
mod text;
mod ui; // retui — retained UI framework; ui/home.rs now owns the home-screen C ABI

/// Append one line to the on-device event log (`/tmp/poc-events.log`) — the primary debugging
/// surface (`make run` fetches it). The ONE shared sink; modules bring it in as `use crate::log;`.
pub(crate) fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}
