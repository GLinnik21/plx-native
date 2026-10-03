//! Generates the `Flavor` enum `platform/src/storage/state.rs` includes, through the same generator
//! the platform crate's own build script uses (`build_support/install_identities.rs`), so the helper
//! and the client cannot disagree about which installs exist. Nothing else happens at build time here.

use std::path::Path;

#[path = "../build_support/install_identities.rs"]
mod install_identities;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../build_support/install_identities.rs");
    install_identities::emit(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/install-identities.json"));
}
