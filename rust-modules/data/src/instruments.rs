//! Diagnostic timers only; these readings never enter machine state.
use std::time::{Duration, Instant};
pub(crate) fn start() -> Instant { Instant::now() }
pub(crate) fn elapsed(started: Instant) -> Duration { started.elapsed() }
