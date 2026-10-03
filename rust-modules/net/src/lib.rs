//! plx_net: the two transports of the PlxNative application core.
//!
//! `net` is the blocking HTTPS client over the television's own libcurl (bound at run time by a
//! SONAME candidate list through `plx_base::dynlib!`, so nothing is linked here) and `stream` is
//! the plaintext socket stream. It is the `net` layer of `ci/module-layers.ini`: it uses `base`
//! (and `platform`, which it does not name today) and nothing else.
//!
//! `test-support` exposes the loopback PMS fixtures (`net::mint_cert`, `spawn_dual_protocol`, ...),
//! the `keypin` and `resolve` test seams and the canned-response helpers that the layers above
//! build their tests on, plus the one test-only CA-bundle override `net::request_result` honours.
//! The application crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

pub mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
pub mod stream;
