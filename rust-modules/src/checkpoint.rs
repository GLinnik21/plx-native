//! The seam through which a blocking media transport asks its caller "may I keep waiting?".
//!
//! A transport ([`crate::stream`], [`crate::curlio`]) knows how to wait for bytes; it does not
//! know why its caller might stop wanting them. The ABR acquisition that owns a segment fetch does
//! know — a playhead-funded reserve can run out, or the main thread can ask for a hold — but the
//! only instant it could hand a transport used to be one absolute deadline, fixed when the
//! operation began. A wait already blocked in `poll`/`curl_multi_wait` could not learn that the
//! answer had changed until that deadline, or bytes, arrived.
//!
//! This module is the transport-neutral contract that closes that gap by **bounded polling**, not
//! by a new wake descriptor: every blocking wait caps its slice at the earlier of its own deadline
//! and the caller's `next_check`, and when a slice (not the deadline) expires it asks again.
//!
//! * A slice expiry is a recheck and nothing else. It is never a transport timeout, never
//!   progress, and it never renews an inactivity bound; the operation resumes IN PLACE with its
//!   partial header, chunk and body state intact.
//! * [`Flow::Stop`] ends the operation with a result distinct from every failure the transport
//!   can report on its own (`HttpOpenError::Stopped`, `HTTP_READ_STOPPED`, `OpenErr::Stopped`,
//!   `READ_STOPPED`), so it can never be mistaken for a dead keep-alive to redial, a fallback to
//!   dial, a deadline, or a teardown.
//! * `next_check: None` means "nothing to check": the transport behaves exactly as it did before
//!   this seam existed, with no extra wakeups. [`NoCheckpoint`] is that answer for every caller.
use std::time::Instant;

/// The caller's answer at a checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Flow {
    /// Keep waiting. `next_check` is the latest instant at which the transport must ask again;
    /// `None` means it never needs to for the rest of this operation.
    Continue { next_check: Option<Instant> },
    /// Abandon the operation now, with the transport's controlled-stop result.
    Stop,
}

/// Consulted by a transport before it blocks, and again whenever a checkpoint slice expires.
pub(crate) trait Checkpoint {
    fn check(&mut self) -> Flow;
}

/// The checkpoint of a caller that has nothing to check: today's behaviour, unchanged.
pub(crate) struct NoCheckpoint;

impl Checkpoint for NoCheckpoint {
    fn check(&mut self) -> Flow {
        Flow::Continue { next_check: None }
    }
}

/// The caller asked the transport to stop at a checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stopped;

/// One transport operation's view of its [`Checkpoint`]: asks only when a check is DUE, so a
/// read that never blocks and bytes that keep arriving cost no extra calls, and a wait that does
/// block wakes at most once per `next_check` rather than once per syscall.
pub(crate) struct Pacer<'a> {
    checkpoint: &'a mut dyn Checkpoint,
    /// `None` until the first ask; then the last answer's `next_check`.
    due: Option<Option<Instant>>,
}

impl<'a> Pacer<'a> {
    pub(crate) fn new(checkpoint: &'a mut dyn Checkpoint) -> Pacer<'a> {
        Pacer {
            checkpoint,
            due: None,
        }
    }

    /// Call immediately before blocking. Consults the checkpoint when it has never been asked or
    /// its `next_check` has passed, and returns the instant the coming wait must wake by to ask
    /// again (`None`: wait as long as the transport's own bounds allow).
    pub(crate) fn before_wait(&mut self) -> Result<Option<Instant>, Stopped> {
        let ask = match self.due {
            None => true,
            Some(None) => false,
            Some(Some(at)) => Instant::now() >= at,
        };
        if ask {
            match self.checkpoint.check() {
                Flow::Stop => return Err(Stopped),
                Flow::Continue { next_check } => self.due = Some(next_check),
            }
        }
        Ok(self.due.flatten())
    }
}

/// Milliseconds a `poll`-style wait may block to reach `at`, rounded UP (a wait that returns a
/// hair early would only spin) and at least 1 (0 is a non-blocking poll, a busy loop in a caller
/// whose `next_check` is already in the past).
pub(crate) fn wait_ms_until(at: Instant, cap_ms: i32) -> i32 {
    let left_us = at.saturating_duration_since(Instant::now()).as_micros();
    (left_us.saturating_add(999) / 1_000).clamp(1, cap_ms.max(1) as u128) as i32
}

/// A host-suite checkpoint: counts its calls, asks again `slice` after each, and stops once
/// `stop` is set or after `stop_after` calls. The transport tests synchronise on `calls`.
#[cfg(test)]
pub(crate) struct TestCheckpoint {
    pub(crate) calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) stop_after: Option<usize>,
    pub(crate) slice: std::time::Duration,
}

#[cfg(test)]
impl TestCheckpoint {
    pub(crate) fn every(slice: std::time::Duration) -> TestCheckpoint {
        TestCheckpoint {
            calls: Default::default(),
            stop: Default::default(),
            stop_after: None,
            slice,
        }
    }
    pub(crate) fn stopping_after(calls: usize, slice: std::time::Duration) -> TestCheckpoint {
        TestCheckpoint {
            stop_after: Some(calls),
            ..TestCheckpoint::every(slice)
        }
    }
    pub(crate) fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[cfg(test)]
impl Checkpoint for TestCheckpoint {
    fn check(&mut self) -> Flow {
        use std::sync::atomic::Ordering;
        let n = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
        if self.stop.load(Ordering::Acquire) || self.stop_after.is_some_and(|k| n > k) {
            Flow::Stop
        } else {
            Flow::Continue {
                next_check: Some(Instant::now() + self.slice),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Scripted(Vec<Flow>, usize);
    impl Checkpoint for Scripted {
        fn check(&mut self) -> Flow {
            self.1 += 1;
            self.0.remove(0)
        }
    }

    #[test]
    fn checkpoint_pacer_asks_once_when_unarmed() {
        let mut cp = Scripted(vec![Flow::Continue { next_check: None }], 0);
        let mut pacer = Pacer::new(&mut cp);
        for _ in 0..5 {
            assert_eq!(pacer.before_wait(), Ok(None));
        }
        drop(pacer);
        assert_eq!(cp.1, 1);
    }

    #[test]
    fn checkpoint_pacer_asks_again_only_once_due() {
        let far = Instant::now() + Duration::from_secs(60);
        let past = Instant::now();
        let mut cp = Scripted(
            vec![
                Flow::Continue {
                    next_check: Some(past),
                },
                Flow::Continue {
                    next_check: Some(far),
                },
                Flow::Stop,
            ],
            0,
        );
        let mut pacer = Pacer::new(&mut cp);
        assert_eq!(pacer.before_wait(), Ok(Some(past)));
        assert_eq!(pacer.before_wait(), Ok(Some(far))); // past was due: asked again
        assert_eq!(pacer.before_wait(), Ok(Some(far))); // far is not due: not asked
        drop(pacer);
        assert_eq!(cp.1, 2);
    }

    #[test]
    fn checkpoint_stop_is_reported() {
        let mut cp = Scripted(vec![Flow::Stop], 0);
        assert_eq!(Pacer::new(&mut cp).before_wait(), Err(Stopped));
    }

    #[test]
    fn checkpoint_wait_ms_rounds_up_and_never_polls_zero() {
        let a = Instant::now();
        assert_eq!(wait_ms_until(a, 200), 1);
        assert_eq!(
            wait_ms_until(Instant::now() + Duration::from_secs(9), 200),
            200
        );
    }
}
