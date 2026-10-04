//! plx_telemetry: the telemetry layer of the PlxNative application core.
//!
//! `telemetry` is consent, the opt-in crash and usage channels (Sentry and PostHog), the spool and
//! the worker that drains it; `diag` is the typed usage-event schema they carry. It is the
//! `telemetry` layer of `ci/module-layers.ini`: it uses `base`, `machine`, `platform`, `net` and
//! `plex` and nothing above them.
//!
//! The release it reports (`plxnative@<version>`) is what the application hands to
//! `telemetry::set_release` right after `plex::identity::set_version`, because `PLX_VERSION` is a
//! `cargo:rustc-env` of the application crate alone. The Sentry and PostHog credentials are read with
//! `option_env!` from the build environment, never from a file, so an unconfigured build carries
//! no endpoint at all.
//!
//! `test-support` exposes the consent and queue test seams (`telemetry::redirect_for_test`,
//! `spool::set_test_path`, `diag::test_events`, ...) and switches the `cfg(not(test))` arms the
//! layers above need in their test form. The application crate enables it in `[dev-dependencies]`
//! only, so no shipped build sees it.

pub mod diag; // typed usage schema plus log/lab scrub, ring and zlib; native crashes have a separate allowlist
pub mod telemetry; // the opt-in crash + usage channels: consent, the spool, the worker, the two wire formats
