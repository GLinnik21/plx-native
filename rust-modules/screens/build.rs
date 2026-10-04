//! Host link configuration for the screens layer's own test binary, and nothing else.
//!
//! `plx_screens` draws through `plx_ui` and `plx_gfx`, which declare the GLES2, EGL, SDL2_ttf and
//! nanosvg symbols their drawing code calls. This crate's `--test` binary is a real executable that
//! reaches them (the screens' draw tests measure text and rasterize icons), and a
//! `cargo:rustc-link-arg` line, which carries the nanosvg object, reaches only the package that
//! prints it, so `plx_gfx`'s own script does not cover this binary. The lines are the same ones,
//! from the same file (`../build_support/host_link.rs`), so the crates cannot disagree.
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
