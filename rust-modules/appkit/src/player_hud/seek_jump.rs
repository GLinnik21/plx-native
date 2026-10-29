//! Requested-target accumulation for immediate relative seeks.
use super::scrub_clamp_target;

/// Fresh presses this close together belong to one relative-seek gesture. Keyframe rounding
/// must not erase its requested travel; after a pause, the published playhead is authoritative.
pub const BURST_GAP_MS: u32 = 1_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeekBurst {
    last: Option<(i64, u32)>,
}

impl SeekBurst {
    pub const IDLE: Self = Self { last: None };

    pub fn reset(&mut self) {
        *self = Self::IDLE;
    }

    /// Logical gesture state, including the expiry clock, for the screen's recorder.
    pub fn snapshot(&self) -> Option<(i64, u32)> {
        self.last
    }

    pub fn jump(&mut self, now: u32, position: i64, step: i64, duration: i64) -> i64 {
        let base = self
            .last
            .filter(|&(_, at)| now.wrapping_sub(at) <= BURST_GAP_MS)
            .map_or(position, |(target, _)| target);
        let target = scrub_clamp_target(base.saturating_add(step), duration);
        self.last = Some((target, now));
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seek_jump_burst_reverses_clamps_and_handles_tick_wrap() {
        let mut burst = SeekBurst::IDLE;
        let second = 1_000_000_000;
        assert_eq!(
            burst.jump(u32::MAX - 100, 40 * second, 60 * second, 100 * second),
            97 * second
        );
        assert_eq!(
            burst.jump(100, 40 * second, -60 * second, 100 * second),
            37 * second
        );
        assert_eq!(burst.jump(200, 40 * second, -60 * second, 100 * second), 0);
        burst.reset();
        assert_eq!(
            burst.jump(300, 40 * second, 10 * second, 100 * second),
            50 * second
        );
        assert_eq!(
            burst.jump(1_801, 43 * second, 10 * second, 100 * second),
            53 * second
        );
    }

}
