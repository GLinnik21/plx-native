//! Host link configuration for the gfx layer's own test binary, and nothing else.
//!
//! `plx_gfx` declares the GLES2, EGL, SDL2_ttf and nanosvg symbols its drawing code calls. Its
//! `--test` binary is a real executable that reaches them (the drawing tests make `gfx` and `text`
//! live), so on the host it needs the same SDL, GL and nanosvg object the application crate's build
//! script supplies to the application's binaries. The lines are the same ones, from the same file
//! (`../build_support/host_link.rs`), so the two cannot disagree.
//!
//! On the television this script prints no `rustc-link-*` line at all: the ARM build is a
//! staticlib the Makefile links, and `host_link::emit` returns before emitting anything for
//! `target_arch = "arm"`. No environment variable is read except cargo's own target description,
//! so an ordinary rebuild never re-runs it.

#[path = "../build_support/host_link.rs"]
mod host_link;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../build_support/host_link.rs");
    host_link::emit(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
}
