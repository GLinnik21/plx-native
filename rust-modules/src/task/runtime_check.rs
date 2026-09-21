//! Developer main-thread checker. The observer publishes data; only the frame thread paints.
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub(super) const HANG_FATAL_MS: u64 = 2000;
const LINGER_MS: u64 = 3000;
#[derive(Clone, Copy)]
pub(super) enum Issue { Guard, Hang(u64) }
pub(super) fn fatal(dev: bool, log_only: bool, issue: Issue) -> bool {
    dev && !log_only && match issue { Issue::Guard => true, Issue::Hang(ms) => ms >= HANG_FATAL_MS }
}

/// A separate latch from the initial >250ms report: one request per continuous stall.
#[derive(Default)]
pub(super) struct KillLatch(bool);
impl KillLatch {
    pub(super) fn poll(&mut self, elapsed: Option<u64>, log_only: bool) -> bool {
        let Some(ms) = elapsed else { self.0 = false; return false };
        if !self.0 && fatal(true, log_only, Issue::Hang(ms)) {
            self.0 = true;
            return true;
        }
        false
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Warning {
    pub(crate) kind: &'static str,
    pub(crate) ms: u64,
    pub(crate) label: &'static str,
    until: Option<u64>,
}
#[derive(Default)]
struct WarningState(Option<Warning>);
impl WarningState {
    fn reset(&mut self) { self.0 = None; }
    fn hang(&mut self, ms: u64, label: &'static str) {
        self.0 = Some(Warning { kind: "HANG", ms, label, until: None });
    }
    fn end(&mut self, now: u64, ms: u64) {
        if let Some(w) = &mut self.0 { w.ms = ms; w.until = Some(now.saturating_add(LINGER_MS)); }
    }
    fn visible(&self, now: u64) -> Option<Warning> {
        self.0.filter(|w| w.until.is_none_or(|until| now < until))
    }
}
static WARNING: Mutex<WarningState> = Mutex::new(WarningState(None));
fn now() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis().min(u64::MAX as u128) as u64
}
pub(crate) fn warning() -> Option<Warning> {
    // Never wait behind an observer on the frame thread.
    WARNING.try_lock().ok().and_then(|s| s.visible(now()))
}
pub(super) fn hang(ms: u64, label: &'static str) {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).hang(ms, label);
}
pub(super) fn reset_warning() {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).reset();
}
pub(super) fn end(ms: u64) {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).end(now(), ms);
}
#[cfg(not(test))]
pub(super) fn guard(label: &'static str) {
    let mut s = WARNING.lock().unwrap_or_else(|e| e.into_inner());
    s.0 = Some(Warning { kind: "BLOCK", ms: 0, label, until: Some(now().saturating_add(LINGER_MS)) });
}

/// Publish a warning without starting the watchdog or sleeping; restore even after an assertion.
#[cfg(test)]
pub(crate) fn with_warning_for_test(f: impl FnOnce()) {
    crate::testlock::assert_held("runtime warning fixture");
    struct Restore(Option<Warning>);
    impl Drop for Restore {
        fn drop(&mut self) { WARNING.lock().unwrap_or_else(|e| e.into_inner()).0 = self.0; }
    }
    let _restore = Restore(WARNING.lock().unwrap_or_else(|e| e.into_inner()).0);
    hang(300, "warning fixture");
    f();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_matrix() {
        for dev in [false, true] {
            for escape in [false, true] {
                assert_eq!(fatal(dev, escape, Issue::Guard), dev && !escape);
                for ms in [0, 250, 251, 1999, 2000, 2001, 5000] {
                    assert_eq!(fatal(dev, escape, Issue::Hang(ms)), dev && !escape && ms >= 2000);
                }
            }
        }
    }
    #[test]
    fn kill_request_is_once_per_stall_and_never_early() {
        let mut latch = KillLatch::default();
        for ms in [0, 250, 251, 1999] { assert!(!latch.poll(Some(ms), false)); }
        assert!(latch.poll(Some(2000), false));
        for ms in [2000, 2100, 5000] { assert!(!latch.poll(Some(ms), false)); }
        assert!(!latch.poll(None, false));
        assert!(latch.poll(Some(2000), false));
        let mut escaped = KillLatch::default();
        for ms in [251, 1999, 2000, 5000] { assert!(!escaped.poll(Some(ms), true)); }
    }
    #[test]
    fn warning_lives_through_hang_then_lingers() {
        let mut state = WarningState::default();
        assert!(state.visible(0).is_none());
        state.hang(300, "probe");
        assert_eq!(state.visible(10000).unwrap().label, "probe");
        state.end(10000, 1000);
        assert_eq!(state.visible(12999).unwrap().ms, 1000);
        assert!(state.visible(13000).is_none());
        state.hang(400, "next");
        assert_eq!(state.visible(14000).unwrap().label, "next");
    }

    #[test]
    fn a_pause_discards_the_warning_without_linger() {
        let mut state = WarningState::default();
        state.hang(300, "gl present");
        assert!(state.visible(300).is_some());
        state.reset();
        assert!(state.visible(30_300).is_none());
        state.hang(300, "frame draw");
        state.end(30_600, 400);
        state.reset();
        assert!(state.visible(30_601).is_none());
    }

}
