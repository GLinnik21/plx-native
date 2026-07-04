//! plexpoc-modules — C modules ported to Rust, linked into the C app (hybrid
//! migration). Each module exposes the same C ABI its src/*.h declares, so the
//! remaining C code calls it unchanged. Ported: img, stream, aq, mkv, pms,
//! posters, text, gfx, system, ui_home.
mod aq;
mod gfx;
mod img;
mod mkv;
mod pms;
mod posters;
mod stream;
mod system;
mod text;
mod ui_home;
