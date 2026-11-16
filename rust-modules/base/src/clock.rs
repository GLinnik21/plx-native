//! The process's monotonic millisecond clock for stamps that are only ever differenced.
//!
//! Not SDL ticks: it is read on the pipeline's own callback thread, and a private origin owes SDL
//! nothing. Every reader (the pipeline callback's presentation stamp, the pump's silence watchdog,
//! the player's audio-pick window, the Feed executor's slice clock) reads this one clock, so a
//! stamp one of them stores is comparable by any other. The origin is fixed at the first call.
//! It lives here, in `base`, because the Feed that stamps with it and the player that differences
//! the stamps are separate layers and neither may name the other.

use std::sync::OnceLock;
use std::time::Instant;

/// Monotonic milliseconds from the origin fixed at the first call (wrapping at `u32`).
pub fn vclock_ms() -> u32 {
    crate::step::assert_io_allowed("clock::vclock_ms");
    origin().elapsed().as_millis() as u32
}

/// [`vclock_ms`] in microseconds, on the same origin.
pub fn vclock_us() -> u64 {
    crate::step::assert_io_allowed("clock::vclock_us");
    origin().elapsed().as_micros() as u64
}

/// Wall time is an IO port too; machine inputs carry already-sampled time.
#[inline]
pub fn wall_now() -> std::time::SystemTime {
    crate::step::assert_io_allowed("clock::wall_now");
    std::time::SystemTime::now()
}
fn origin() -> &'static Instant {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_millisecond_and_microsecond_readings_share_one_origin_and_never_run_back() {
        let a = vclock_us();
        let ms = u64::from(vclock_ms());
        let b = vclock_us();
        assert!(a / 1000 <= ms && ms <= b / 1000, "{a} us, {ms} ms, {b} us");
        assert!(b >= a);
    }
}
