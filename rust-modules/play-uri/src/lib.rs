//! URI media protocol, ownership and diagnostic playback driver.
//!
//! The caller owns the route attempt and provides a sink, transport, clock-facing
//! main-thread token, and log function. No Plex route or buffer-feed engine is
//! referenced from this crate.

pub mod acb_bind;
mod acb_envelope;
pub mod acb_drive;
pub mod picture_gate;
pub mod uri_protocol;
pub mod uri_lifecycle;
pub mod register_ladder;
#[cfg(feature = "devtriggers")]
mod event_text;
#[cfg(feature = "devtriggers")]
mod uri_trace;
mod session_io;
pub mod backend;
#[cfg(feature = "devtriggers")]
pub mod uriprobe;
#[cfg(feature = "devtriggers")]
pub mod uriprobe_drive;
