//! plexpoc-modules — C modules ported to Rust, linked into the C app (hybrid
//! migration). Each module exposes the same C ABI its src/*.h declares, so the
//! remaining C code calls it unchanged. Ported so far: img, stream, aq, mkv.
mod aq;
mod img;
mod mkv;
mod stream;
