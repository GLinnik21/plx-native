//! plx_net: the application core's HTTP and byte-stream transports, and `ws`, the sans-IO RFC 6455
//! codec and the WebSocket transport that runs it over libcurl's `CONNECT_ONLY` socket
//! (`net::connect_only`).
//!
//! `net` is the blocking HTTPS client over the television's own libcurl (bound at run time by a
//! SONAME candidate list through `plx_base::dynlib!`, so nothing is linked here), `curlio`
//! is the interruptible HTTPS byte-range source, and `stream` is the plaintext socket stream. It is the `net` layer of `ci/module-layers.ini`: it uses `base`
//! (and `platform`, which it does not name today) and nothing else.
//!
//! `test-support` exposes the loopback PMS fixtures (`net::mint_cert`, `spawn_dual_protocol`, ...),
//! the `keypin` and `resolve` test seams and the canned-response helpers that the layers above
//! build their tests on, plus the one test-only CA-bundle override `net::request_result` honours.
//! The application crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

#[path = "curlio.rs"]
pub mod range_io; // interruptible HTTPS byte-range source
pub mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
pub mod stream;
pub mod ws;
