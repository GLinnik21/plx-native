//! Run one test body on several threads at once, and wait for them with a deadline.
//!
//! The regression test for a process-wide cache that owns heap (`ci/check-statics-heap.py`) needs
//! the same four pieces every time: N threads released together by a barrier so their loops
//! overlap, a panic caught per thread so the failure names the thread and its own assertion, no
//! `join`, and a deadline. The deadline is the part a hand-rolled `thread::scope` leaves out and
//! the part that matters: a `HashMap` that two threads corrupted can spin forever, and a test that
//! joins it hangs the whole suite instead of failing (the unfixed `elide` memo held a core at 320%
//! for 13 minutes). So the workers are DETACHED and the caller waits on a channel; a worker that
//! never reports is a test failure, and the process exit reaps it.
//!
//! Workers start through [`crate::task::spawn`], the tree's one spawn.
//!
//! The body is `'static` because the workers outlive the call when they hang: it cannot borrow the
//! test's locals, so build per-thread fixtures inside it, from the thread number.

use std::sync::{mpsc, Arc, Barrier};
use std::time::Duration;

/// How long [`race`] waits for every worker. A healthy test is a few loops of a cache lookup and
/// finishes in well under a second; this only has to outlast a loaded Mac running two `make
/// check`s, and a corrupted table does not come back at any length.
const DEADLINE: Duration = Duration::from_secs(60);

/// Run `body(t)` for `t` in `0..threads` on separate threads, all released by one barrier, and
/// return when every one has finished. Panics if any body panicked (naming the thread and its
/// message) or if one has not finished within [`DEADLINE`].
pub fn race<F>(threads: usize, body: F)
where
    F: Fn(usize) + Send + Sync + 'static,
{
    race_within(threads, DEADLINE, body);
}

/// [`race`] with an explicit deadline, for the test of the deadline itself.
pub fn race_within<F>(threads: usize, deadline: Duration, body: F)
where
    F: Fn(usize) + Send + Sync + 'static,
{
    let body = Arc::new(body);
    let start = Arc::new(Barrier::new(threads));
    let (done, finished) = mpsc::channel();
    for t in 0..threads {
        let (body, start, done) = (Arc::clone(&body), Arc::clone(&start), done.clone());
        crate::task::spawn("race worker", move || {
            start.wait();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| body(t)));
            let _ = done.send((t, outcome.map_err(|payload| panic_message(payload.as_ref()))));
        })
        .expect("the OS refused a race worker thread");
    }
    drop(done);
    for _ in 0..threads {
        match finished.recv_timeout(deadline) {
            Ok((_, Ok(()))) => {}
            Ok((t, Err(message))) => panic!("thread {t} panicked: {message}"),
            Err(_) => panic!(
                "a worker never finished within {deadline:?}: the state the threads share is \
                 corrupted or deadlocked"
            ),
        }
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    match payload.downcast_ref::<&str>() {
        Some(message) => (*message).to_string(),
        None => payload.downcast_ref::<String>().cloned().unwrap_or_else(|| "(non-string panic)".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn every_thread_runs_the_body_once_with_its_own_number() {
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        race(5, |t| {
            SEEN.fetch_add(1 << t, Ordering::SeqCst);
        });
        assert_eq!(SEEN.load(Ordering::SeqCst), 0b11111);
    }

    #[test]
    #[should_panic(expected = "thread 2 panicked: its own assertion")]
    fn a_panicking_body_fails_the_race_and_names_the_thread() {
        race(4, |t| assert!(t != 2, "its own assertion"));
    }

    #[test]
    #[should_panic(expected = "a worker never finished")]
    fn a_body_that_never_returns_fails_the_race_at_the_deadline() {
        race_within(2, Duration::from_millis(200), |t| {
            if t == 1 {
                std::thread::sleep(Duration::from_secs(30));
            }
        });
    }
}
