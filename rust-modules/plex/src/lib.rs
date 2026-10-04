//! plx_plex: the Plex layer of the PlxNative application core.
//!
//! `plex` is the typed Plex API (PMS and plex.tv), the session store and the grant/probe logic that
//! decide which origin may be asked for what; `http` is the one door out of the control plane,
//! dispatching a request on its origin's scheme (`plx_net::stream` for http, `plx_net::net` for
//! https). It is the `plex` layer of `ci/module-layers.ini`: it uses `base`, `machine`, `platform`
//! and `net` and nothing above them.
//!
//! `test-support` exposes the session fixtures (`plex::session::TempSession`, the `*_for_test`
//! seams) and switches the few `cfg(not(test))` arms the layers above need in their test form. The
//! application crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

pub mod http; // the ONE door out of the control plane: dispatch a Plex REST request on its origin's scheme
pub mod plex; // typed Plex API layer — one method per PMS operation (the live READ layer)
