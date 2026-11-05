//! URI media protocol, ownership and diagnostic playback driver.
//!
//! The caller owns the route attempt and provides a sink, transport, clock-facing
//! main-thread token, and log function. No Plex route or buffer-feed engine is
//! referenced from this crate.

pub mod acb_bind;
pub mod acb_drive;
pub mod picture_gate;
pub mod uri_protocol;
pub mod uri_lifecycle;
mod session_io;
pub mod backend;
#[cfg(feature = "devtriggers")]
pub mod uriprobe;
#[cfg(feature = "devtriggers")]
pub mod uriprobe_drive;
