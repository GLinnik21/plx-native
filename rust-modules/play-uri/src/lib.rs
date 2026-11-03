//! URI media protocol, ownership and diagnostic playback driver.
//!
//! The caller owns the route attempt and provides a sink, transport, clock-facing
//! main-thread token, and log function. No Plex route or buffer-feed engine is
//! referenced from this crate.

#[cfg(feature = "devtriggers")]
pub mod uri_protocol;
#[cfg(feature = "devtriggers")]
pub mod uri_lifecycle;
#[cfg(feature = "devtriggers")]
pub mod uriprobe;
#[cfg(feature = "devtriggers")]
pub mod uriprobe_drive;
