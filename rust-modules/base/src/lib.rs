//! plx_base: the leaf layer of the PlxNative application core.
//!
//! The event log, file locations, the spawn discipline, the dlopen seam, the frame instruments and
//! the small leaf utilities every other layer calls. It names nothing outside itself (the
//! `base` layer of `ci/module-layers.ini` has `uses =` nothing), so it is also the crate the
//! rest of `rust-modules/src` is compiled against first and the only part a leaf edit rebuilds
//! on its own.
//!
//! `test-support` exposes the `cfg(test)` fixtures other layers' tests build on (`testlock`,
//! `testnet`, `storage_worker::drain_for_test`, ...) and keeps this crate's test-mode behaviour
//! (a disarmed watchdog, no SDL link in `diag::heartbeat`) when a dependent's tests build it.
//! The application crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

pub mod eventlog; // THE event log: `eventlog::log` is the one sink every module writes through
pub mod paths; // where the app's own files live — /proc/self/exe, not a hardcoded install prefix
pub mod task; // the one spawn: a refused thread is a return value, not a panic that kills the app
pub mod cbuf; // fixed NUL-terminated C-string buffer read/write
pub mod sha256; // SHA-256 / HMAC / PBKDF2, hand-written: the offline PIN verifier's hash
pub mod b64; // standard base64 encode/decode
pub mod spki; // a PEM certificate -> its CURLOPT_PINNEDPUBLICKEY string
pub mod dynlib; // dlopen-by-SONAME-candidate: the libraries whose major moves between webOS releases
pub mod checkpoint; // the transport-neutral "may I keep waiting?" seam every blocking media wait consults
pub mod storage_worker; // a bounded FIFO for blocking persistence work
pub mod fontcov; // which codepoints a font file can draw, read from its cmap
pub mod surface; // what we are actually drawing into: drawable vs the 1920x1080 logical canvas
pub mod tile; // `Tile`: the shelf-tile trait the data layer implements and `ui` draws through
pub mod devtrig; // the /tmp trigger PRIMITIVES (`flag`, `read`, `latched_flag!`, `no_wan`...)
pub mod diag; // the frame instruments (`heartbeat`, `spans`) and the zlib helper
#[cfg(any(test, feature = "test-support"))]
pub mod testlock; // one lock for every test that touches a process-global
#[cfg(any(test, feature = "test-support"))]
pub mod testnet; // the one accept for a loopback test server whose listener is nonblocking
