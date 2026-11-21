//! PlxNative's webOS platform: everything that exists only because the target is an LG television
//! running webOS, written against the application's published face (`plxnative_modules::host`).
//!
//! This is the crate the ARM staticlib is built from (`cargo rustc -p plx_platform_webos --lib
//! --crate-type staticlib`), and the application is its dependency, never the reverse: the boot shim
//! `src/main.c` calls the C entries this crate exports, and they hand the application its two tables
//! and its entry. A desktop executable never links it.
//!
//! The crate is a mounted module of the `platform webos` layer (`ci/module-layers.ini`): it may name
//! every layer and the application's `host`, nothing names it, and it names no other platform
//! (`docs/platform-ports.md`, W7).
