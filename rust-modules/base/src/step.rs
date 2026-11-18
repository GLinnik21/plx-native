//! Test-time Sans-IO boundary. FrameScope forbids waits; this scope forbids starting IO.
use std::{marker::PhantomData, rc::Rc};
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
/// Keep this thread-bound guard around the machine call, never its runner or drain.
#[must_use = "keep the guard for the machine step"]
pub struct StepScope(PhantomData<Rc<()>>);
impl StepScope {
    #[inline(always)]
    pub fn enter() -> Self {
        #[cfg(any(test, feature = "test-support"))]
        DEPTH.with(|depth| depth.set(depth.get().checked_add(1).expect("StepScope depth")));
        Self(PhantomData)
    }
}
impl Drop for StepScope {
    #[inline(always)]
    fn drop(&mut self) {
        #[cfg(any(test, feature = "test-support"))]
        DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}
/// A frame's AllowBlocking exception cannot grant a machine IO capabilities.
#[inline(always)]
#[track_caller]
pub fn assert_io_allowed(operation: &str) {
    #[cfg(any(test, feature = "test-support"))]
    DEPTH.with(|depth| assert_eq!(depth.get(), 0, "IO during machine step: {operation}"));
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = operation;
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    #[test]
    fn step_guard_cannot_be_sent_or_shared_between_threads() {
        trait AmbiguousIfSend<A> { fn check() {} }
        impl<T: ?Sized> AmbiguousIfSend<()> for T {}
        impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
        let _ = <StepScope as AmbiguousIfSend<_>>::check;
        trait AmbiguousIfSync<A> { fn check() {} }
        impl<T: ?Sized> AmbiguousIfSync<()> for T {}
        impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
        let _ = <StepScope as AmbiguousIfSync<_>>::check;
    }
    #[test]
    fn nested_steps_restore_after_unwind_and_are_thread_local() {
        let outer = StepScope::enter();
        assert!(catch_unwind(|| { let _inner = StepScope::enter(); panic!("unwind"); }).is_err());
        assert!(catch_unwind(|| assert_io_allowed("outer still active")).is_err());
        std::thread::spawn(|| assert_io_allowed("other thread")).join().unwrap();
        drop(outer);
        assert_io_allowed("runner after step");
    }
    #[test]
    fn real_io_entries_refuse_even_under_allow_blocking() {
        let writer = crate::storage_worker::Writer::start("step-test", 1, |n: u8| n).unwrap();
        let executor = crate::storage_worker::Executor::start(1).unwrap();
        let _allowed = crate::task::allow_blocking(const { &crate::task::BlockingLabel::new("fixture") });
        let step = StepScope::enter();
        let checks: Vec<Box<dyn FnOnce() + '_>> = vec![
            Box::new(|| { let _ = crate::task::spawn("forbidden", || {}); }),
            Box::new(|| { std::thread::scope(|s| { crate::task::beside(s, "forbidden", || ()).finish(); }); }),
            Box::new(|| { let _ = crate::storage_worker::submit(|| ()); }),
            Box::new(|| { let _ = crate::storage_worker::submit_retained(|| ()); }),
            Box::new(|| { let _ = executor.submit(|| ()); }),
            Box::new(|| { let _ = writer.submit_ticket(1); }),
            Box::new(crate::storage_worker::pump_retained),
            Box::new(|| { let _ = crate::task::assert_may_block(const { &crate::task::BlockingLabel::new("PMS HTTP") }); }),
            Box::new(|| { crate::clock::vclock_ms(); }),
            Box::new(|| { crate::clock::vclock_us(); }),
            Box::new(|| { crate::clock::wall_now(); }),
        ];
        for check in checks { assert!(catch_unwind(AssertUnwindSafe(check)).is_err()); }
        drop(step);
        crate::task::spawn("runner", || {}).unwrap().join().unwrap();
        assert_eq!(executor.submit(|| 42).unwrap().wait_blocking().unwrap(), 42);
        assert_eq!(writer.submit_ticket(7).ok().unwrap().wait_blocking().unwrap(), 7);
        crate::clock::vclock_ms();
        crate::clock::wall_now();
    }
}
