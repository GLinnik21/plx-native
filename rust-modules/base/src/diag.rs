//! The frame instruments and the zlib helper that used to live under the application's `diag`.
//! They are leaves, so they live in this crate; the typed usage schema, the scrub and the event
//! door stay in the application crate's own `diag` module, which these do not name.

pub mod heartbeat; // the frame's own instruments: the eight phase stamps, FRAMEDROP, worstframe=/worstprep=
pub mod spans; // named sub-spans of one frame (results, update, draw), printed on its FRAMEDROP line
#[cfg(feature = "lab-diagnostics")]
pub mod zlib;
