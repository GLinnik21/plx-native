//! plexpoc-modules — C modules ported to Rust, linked into the C app (hybrid
//! migration). Each module exposes the same C ABI its src/*.h declares, so the
//! remaining C code calls it unchanged. Ported: img, stream, aq, mkv, pms,
//! posters, text, gfx, system, ui_home.
mod app; // plex_run — the Rust app core / event loop (the entry inverted from main.c)
mod aq;
mod ff; // FFmpeg (libavformat/libavcodec/libavutil) demuxer — replaces mkv.rs (TV ships FFmpeg 3.4)
mod gfx;
mod img;
mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
mod mkv;
mod player; // buffer-feed video engine (was playback.c) — step 5
mod plex; // typed Plex API layer (rust-modules/src/plex/) — one method per PMS operation (unused; call sites migrate later)
mod pms;
mod posters;
mod route; // play_movie route selection (direct-play vs transcode) — step 3
mod stream;
mod system;
mod text;
mod ui; // retui — retained UI framework; ui/home.rs now owns the home-screen C ABI
mod webvtt; // streaming WebVTT parser (soft subtitles during transcode)
