//! Host link configuration for the shell-sdl layer's own test binary, and nothing else.
//!
//! `plx_shell_sdl` declares the SDL2 symbols its window, event and text code call. Its `--test`
//! binary is a real executable that can reach them (the event-queue round trips push and poll
//! real SDL events), so on the host it needs the same SDL object the application crate's build
//! script supplies to the application's binaries. The lines are the same ones, from the same file
//! (`../build_support/host_link.rs`), so the crates cannot disagree.
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
