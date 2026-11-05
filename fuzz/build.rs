//! The host link configuration the layer crates' own test binaries get, for this package's fuzz
//! binaries: `plx_appkit` draws through `plx_gfx`, which declares the GLES2, EGL, SDL2_ttf and nanosvg
//! symbols its drawing code calls, and a `cargo:rustc-link-arg` line (which carries the nanosvg
//! object) reaches only the package that prints it, so a binary here needs its own script. The lines
//! are the ones `rust-modules/appkit/build.rs` prints, from the same file, so the two cannot disagree.
//! Nothing here runs on the television; this package is never built for it.

#[path = "../rust-modules/build_support/host_link.rs"]
mod host_link;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../rust-modules/build_support/host_link.rs");
    host_link::emit(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."));
}
