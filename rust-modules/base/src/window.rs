//! A span of time that opens when it is started and closes `len` later: the one stamp a memo or a
//! cache entry holds when it must stop counting after a while ("Discover failed to answer in the
//! last 45 s", "this miss expires with that memo").
//!
//! It lives in `base`, which owns the monotonic clock, so the layers that hold such a stamp (the
//! app's poster adapter, the plex.tv reachability memos) keep a VALUE and ask it, and read no clock
//! themselves.

use std::time::{Duration, Instant};

/// A window that was started at some moment and ends `len` after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    ends: Instant,
}

impl Window {
    /// Start a window of `len` now.
    pub fn start(len: Duration) -> Window {
        Window { ends: Instant::now() + len }
    }

    /// Is it still open? A window of zero length is never open.
    pub fn is_open(&self) -> bool {
        Instant::now() < self.ends
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_is_open_for_its_length_and_a_zero_one_never() {
        assert!(Window::start(Duration::from_secs(3600)).is_open());
        assert!(!Window::start(Duration::ZERO).is_open());
        let w = Window::start(Duration::from_millis(20));
        assert!(w.is_open());
        std::thread::sleep(Duration::from_millis(40));
        assert!(!w.is_open(), "it closes once its length has passed");
    }
}
