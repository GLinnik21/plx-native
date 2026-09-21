//! The frame thread must not wait on storage or synchronous platform calls. Boot runs outside
//! the scope; the application loop and dispatcher enter it, including when driven by host tests.

use std::{cell::Cell, marker::PhantomData, rc::Rc, time::Instant};

/// An immutable static descriptor lets the watchdog publish a whole label with one pointer.
/// `const { &BlockingLabel::new("literal") }` is evaluated into static storage; no scope
/// allocates or interns strings.
pub(crate) struct BlockingLabel {
    pub(super) text: &'static str,
}
impl BlockingLabel {
    pub(crate) const fn new(text: &'static str) -> Self { Self { text } }
}

thread_local! {
    static FRAMES: Cell<u32> = const { Cell::new(0) };
    static ALLOWED: Cell<u32> = const { Cell::new(0) };
}

pub(crate) struct FrameScope(PhantomData<Rc<()>>);
impl FrameScope {
    pub(crate) fn enter() -> Self {
        FRAMES.with(|depth| depth.set(depth.get() + 1));
        Self(PhantomData)
    }
}
impl Drop for FrameScope {
    fn drop(&mut self) { FRAMES.with(|depth| depth.set(depth.get() - 1)); }
}

#[allow(dead_code)] // Explicit escape hatch; no production call site currently needs it.
pub(crate) struct AllowBlocking { _label: super::watchdog::LabelScope }
/// Explicit exceptions belong at user actions, with a reason and follow-up at the call site.
#[allow(dead_code)] // Kept available for a justified, greppable exception.
pub(crate) fn allow_blocking(reason: &'static BlockingLabel) -> AllowBlocking {
    assert!(!reason.text.is_empty());
    ALLOWED.with(|depth| depth.set(depth.get() + 1));
    AllowBlocking { _label: super::watchdog::enter_label(reason) }
}
impl Drop for AllowBlocking {
    fn drop(&mut self) { ALLOWED.with(|depth| depth.set(depth.get() - 1)); }
}

#[must_use = "keep the guard for the duration of the blocking operation"]
pub(crate) struct BlockingGuard {
    label: &'static str,
    started: Option<Instant>,
    _label: super::watchdog::LabelScope,
    _thread: PhantomData<Rc<()>>,
}

pub(crate) fn assert_may_block(label: &'static BlockingLabel) -> BlockingGuard {
    let in_frame = FRAMES.with(|depth| depth.get() != 0);
    if cfg!(test) && in_frame && !ALLOWED.with(|depth| depth.get() != 0) {
        panic!("main-thread block: {}", label.text);
    }
    BlockingGuard { label: label.text, started: in_frame.then(Instant::now),
        _label: super::watchdog::enter_label(label), _thread: PhantomData }
}

impl Drop for BlockingGuard {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            static REPORTED: std::sync::Mutex<std::collections::BTreeSet<&'static str>> =
                std::sync::Mutex::new(std::collections::BTreeSet::new());
            if REPORTED.lock().unwrap_or_else(|e| e.into_inner()).insert(self.label) {
                crate::log(&format!("main-thread block: {} {}ms", self.label, started.elapsed().as_millis()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[should_panic(expected = "main-thread block: storage helper transact")]
    fn a_helper_call_inside_a_frame_is_rejected() {
        let _frame = FrameScope::enter();
        let _ = crate::storage::client::load();
    }

    #[test]
    fn nested_scopes_and_exceptions_restore_after_unwind() {
        let frame = FrameScope::enter();
        let _ = std::panic::catch_unwind(|| {
            let _nested = FrameScope::enter();
            let _allowed = allow_blocking(const { &BlockingLabel::new("test explicit user action") });
            let _call = assert_may_block(const { &BlockingLabel::new("allowed fixture") });
            panic!("unwind the scopes");
        });
        assert!(std::panic::catch_unwind(|| assert_may_block(const { &BlockingLabel::new("after exception") })).is_err());
        drop(frame);
        let _boot = assert_may_block(const { &BlockingLabel::new("boot outside frame") });
    }
}
