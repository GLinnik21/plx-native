//! Host link configuration for the webOS platform crate's own test binary, and nothing else.
//!
//! `plx_platform_webos` holds `plex_run`, a `#[no_mangle]` C entry, so its `--test` binary links the
//! whole application and reaches the GLES2, EGL, SDL2_ttf and nanosvg symbols that `plx_gfx`
//! declares. A `cargo:rustc-link-arg` line, which carries the nanosvg object, reaches only the
//! package that prints it, so the application crate's own script does not cover an executable of this
//! package. The lines are the same ones, from the same file (`../build_support/host_link.rs`), so the
//! crates cannot disagree.
//!
//! On the television this script prints no `rustc-link-*` line at all: the ARM build is a staticlib
//! the Makefile links, and `host_link::emit` returns before emitting anything for `target_arch =
//! "arm"`. No environment variable is read except cargo's own target description, so an ordinary
//! rebuild never re-runs it.
//!
//! It publishes no `PLX_VERSION` or `PLX_BUILD_SHA`: `cargo:rustc-env` reaches only the package that
//! prints it, those two are the application crate's, and nothing in this crate reads them.

#[path = "../build_support/host_link.rs"]
mod host_link;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../build_support/host_link.rs");
    host_link::emit(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
}
