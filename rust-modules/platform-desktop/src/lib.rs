//! PlxNative's desktop platform: the simulator's and the macOS app's own tables and entry, written
//! against the application's published face (`plxnative_modules::host`), and the `plxnative-sim`
//! binary that calls it.
//!
//! Compiled only under `hostsim` (this crate's own feature, in no default set): a default-feature
//! pass that lists this package builds an empty library, and a television build never builds it.
//! The crate is a mounted module of the `platform desktop` layer (`ci/module-layers.ini`): nothing
//! names it, and it names no other platform (`docs/platform-ports.md`, W7).
#![cfg(feature = "hostsim")]
