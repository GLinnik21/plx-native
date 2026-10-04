//! plx_session: the session layer of the PlxNative application core.
//!
//! `auth` is the plex.tv sign-in and boot flow controller: PIN and QR sign-in, discovery of the
//! account's servers, who's-watching and the profile PIN, the install of the chosen profile's
//! credentials, and the owner machine that holds all of it (`auth::owner`). It is the `session`
//! layer of `ci/module-layers.ini`: it uses `base`, `machine`, `platform`, `net`, `plex` and
//! `telemetry` and nothing above them.
//!
//! The release it reports and the consent hand-off are the other layers' (`plx_plex::plex::identity`
//! and `plx_telemetry::telemetry`); this crate reads no `PLX_*` build variable and has no build script.
//!
//! `test-support` exposes the `cfg(test)` seams the layers above test through
//! (`auth::test_support`, `auth::SessionIdentity::of`, `auth::synthetic_incident`,
//! `auth::settled_probe_for_test`, `auth::owner::scenarios`) and the test arms of `ProfileWorkIo`.
//! The application crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

pub mod auth; // plex.tv login/boot flow controller (PIN/QR -> discovery -> who's-watching -> install)
