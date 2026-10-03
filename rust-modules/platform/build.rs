//! Code generation for the platform layer: the `i18n::msg` localization catalog and the
//! install-identity `Flavor` enum `storage::state` includes. Nothing else happens at build time
//! here, and nothing here links anything: the host link configuration (SDL, GL, the nanosvg object)
//! belongs to the application crate's build script, which owns the final binary.
//!
//! Both generators write into this crate's `OUT_DIR` and both are included only from this crate
//! (`i18n/mod.rs`, `storage/state.rs`). The storage helper (`plxnative-storage`) includes
//! `storage/state.rs` by `#[path]` and runs `install_identities` from its own build script, so the
//! client and the helper cannot disagree about which installs exist.
//!
//! The inputs are the only things this script re-runs for: the catalog sources under `locales/`,
//! the packaging manifest of installs, and the two generator files. No environment variable is
//! read, so an ordinary rebuild never re-runs it.

use std::path::{Path, PathBuf};

#[path = "build_support/catalog.rs"]
mod catalog;
#[path = "../build_support/install_identities.rs"]
mod install_identities;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=build_support/catalog.rs");
    println!("cargo:rerun-if-changed=../build_support/install_identities.rs");
    println!("cargo:rerun-if-changed=../../locales");
    catalog::build(Path::new("../../locales"), &PathBuf::from(std::env::var_os("OUT_DIR").unwrap()))
        .expect("valid, complete localization catalogs");

    install_identities::emit(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/install-identities.json"));
}
