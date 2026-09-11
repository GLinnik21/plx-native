//! Bounded transport for an explicitly pressed one-off report.
//!
//! The ordinary path is the durable spool.  If that path cannot be written, one bounded
//! background send is allowed for the same record; the render thread never performs network I/O.

use super::queue::{Category, Record};
use super::sender::Verdict;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

const MAX_STATES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliveryState {
    Queued,
    Sending,
    Sent,
    Failed,
}

#[derive(Default)]
struct Inner {
    states: Vec<(String, DeliveryState)>,
    /// Remains occupied until the worker retires, even after `forget` clears its visible state.
    fallback_generation: Option<u64>,
}

static INNER: Mutex<Inner> = Mutex::new(Inner {
    states: Vec::new(),
    fallback_generation: None,
});
static GENERATION: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
static TEST_SEND: Mutex<Option<Box<dyn Fn(&Record) -> Verdict + Send>>> = Mutex::new(None);

fn generation_current(generation: u64) -> bool {
    GENERATION.load(Ordering::Acquire) == generation
}

fn remember_if_current(event_id: &str, state: DeliveryState, generation: u64) -> bool {
    let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
    if !generation_current(generation) {
        return false;
    }
    if let Some((_, existing)) = inner.states.iter_mut().find(|(id, _)| id == event_id) {
        *existing = state;
        return true;
    }
    if inner.states.len() >= MAX_STATES {
        if let Some(index) = inner
            .states
            .iter()
            .position(|(_, state)| *state != DeliveryState::Sending)
        {
            inner.states.remove(index);
        } else {
            return false;
        }
    }
    inner.states.push((event_id.to_string(), state));
    true
}

pub(crate) fn delivery_state(event_id: &str) -> Option<DeliveryState> {
    INNER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .states
        .iter()
        .find(|(id, _)| id == event_id)
        .map(|(_, state)| *state)
}

fn send_one(record: &Record) -> Verdict {
    #[cfg(test)]
    {
        return TEST_SEND
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(Verdict::Hopeless, |send| send(record));
    }
    #[cfg(not(test))]
    {
        super::sender::send_one(record).0
    }
}

fn retire(generation: u64) {
    let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
    if inner.fallback_generation == Some(generation) {
        inner.fallback_generation = None;
    }
}

fn fallback(record: Record, generation: u64) {
    // Forget increments GENERATION before purging the durable spool.  Do not even enter the
    // network seam for a stale request.
    if !generation_current(generation) {
        retire(generation);
        return;
    }
    let verdict = send_one(&record);
    let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
    if inner.fallback_generation == Some(generation) {
        inner.fallback_generation = None;
    }
    if generation_current(generation) {
        let state = match verdict {
            Verdict::Done => DeliveryState::Sent,
            Verdict::Keep | Verdict::Hopeless => DeliveryState::Failed,
        };
        if let Some((_, existing)) = inner
            .states
            .iter_mut()
            .find(|(id, _)| id == &record.event_id)
        {
            *existing = state;
        }
    }
}

/// Submit an explicit one-off record. `true` means durable queue acceptance or a bounded direct
/// fallback was accepted; it does not mean the server has replied.
pub(crate) fn submit(record: Record) -> bool {
    if record.category != Category::OneOff || super::queue::encode(&record).is_none() {
        return false;
    }
    let generation = GENERATION.load(Ordering::Acquire);
    let allowed_generation = || generation_current(generation);
    match super::spool::append_if(&record, allowed_generation) {
        Some(true) => {
            let _ = remember_if_current(&record.event_id, DeliveryState::Queued, generation);
            super::flush_soon();
            true
        }
        Some(false) => {
            let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
            if !generation_current(generation) || inner.fallback_generation.is_some() {
                return false;
            }
            if inner.states.len() >= MAX_STATES {
                if let Some(index) = inner
                    .states
                    .iter()
                    .position(|(_, state)| *state != DeliveryState::Sending)
                {
                    inner.states.remove(index);
                } else {
                    return false;
                }
            }
            inner.fallback_generation = Some(generation);
            inner
                .states
                .push((record.event_id.clone(), DeliveryState::Sending));
            drop(inner);
            let event_id = record.event_id.clone();
            let spawned = crate::task::spawn_small("oneoff", move || fallback(record, generation));
            if spawned {
                true
            } else {
                retire(generation);
                let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((_, state)) = inner.states.iter_mut().find(|(id, _)| id == &event_id) {
                    *state = DeliveryState::Failed;
                }
                false
            }
        }
        None => false,
    }
}

/// End the account's ownership of all in-flight and retained one-off delivery state.
pub(crate) fn forget() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    let mut inner = INNER.lock().unwrap_or_else(|e| e.into_inner());
    inner.states.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::queue::Dest;
    use std::path::PathBuf;
    use std::sync::mpsc;
    use std::time::Duration;

    fn record(id: &str, body: &[u8]) -> Record {
        Record {
            category: Category::OneOff,
            dest: Dest::Sentry,
            event_id: id.into(),
            body: body.to_vec(),
        }
    }

    struct Reset(PathBuf);
    impl Drop for Reset {
        fn drop(&mut self) {
            super::forget();
            crate::telemetry::spool::set_test_path(None);
            let _ = std::fs::remove_dir_all(&self.0);
            *TEST_SEND.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    fn setup(name: &str) -> Reset {
        let dir = std::env::temp_dir().join(format!("plxnative-oneoff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::telemetry::spool::set_test_path(Some(dir.join("spool.bin")));
        super::forget();
        Reset(dir)
    }

    #[test]
    fn spool_success_is_queued_without_direct_send() {
        let _g = crate::testlock::serial();
        let _reset = setup("queued");
        let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sends2 = sends.clone();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |_| {
            sends2.fetch_add(1, Ordering::Relaxed);
            Verdict::Done
        }));
        assert!(submit(record("queued", b"body")));
        assert_eq!(delivery_state("queued"), Some(DeliveryState::Queued));
        assert_eq!(sends.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn spool_failure_uses_one_bounded_direct_fallback_and_preserves_record_id() {
        let _g = crate::testlock::serial();
        let reset = setup("fallback");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone())); // directory: append fails
        let (tx, rx) = mpsc::channel();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |r| {
            tx.send(r.event_id.clone()).unwrap();
            Verdict::Done
        }));
        assert!(submit(record("direct-id", b"body")));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "direct-id");
        for _ in 0..20 {
            if delivery_state("direct-id") == Some(DeliveryState::Sent) {
                return;
            }
            std::thread::yield_now();
        }
        assert_eq!(delivery_state("direct-id"), Some(DeliveryState::Sent));
    }

    #[test]
    fn only_one_direct_fallback_can_be_inflight() {
        let _g = crate::testlock::serial();
        let reset = setup("cap");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |_| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
            Verdict::Done
        }));
        assert!(submit(record("first", b"body")));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!submit(record("second", b"body")));
        let _ = release_tx.send(());
    }

    #[test]
    fn forget_discards_state_and_stale_worker_cannot_publish() {
        let _g = crate::testlock::serial();
        let reset = setup("forget");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |_| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
            Verdict::Done
        }));
        assert!(submit(record("stale", b"body")));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        forget();
        assert_eq!(delivery_state("stale"), None);
        assert!(!submit(record("new-while-old-worker-live", b"body")));
        release_tx.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(delivery_state("stale"), None);
    }

    #[test]
    fn direct_keep_is_recorded_as_failed() {
        let _g = crate::testlock::serial();
        let reset = setup("keep");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        *TEST_SEND.lock().unwrap() = Some(Box::new(|_| Verdict::Keep));
        assert!(submit(record("keep", b"body")));
        for _ in 0..100 {
            if delivery_state("keep") == Some(DeliveryState::Failed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(delivery_state("keep"), Some(DeliveryState::Failed));
    }

    #[test]
    fn direct_hopeless_is_recorded_as_failed() {
        let _g = crate::testlock::serial();
        let reset = setup("hopeless");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        *TEST_SEND.lock().unwrap() = Some(Box::new(|_| Verdict::Hopeless));
        assert!(submit(record("hopeless", b"body")));
        for _ in 0..100 {
            if delivery_state("hopeless") == Some(DeliveryState::Failed) {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(delivery_state("hopeless"), Some(DeliveryState::Failed));
    }

    #[test]
    fn non_oneoff_and_oversized_records_never_use_direct_fallback() {
        let _g = crate::testlock::serial();
        let reset = setup("reject");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        let sends = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sends2 = sends.clone();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |_| {
            sends2.fetch_add(1, Ordering::Relaxed);
            Verdict::Done
        }));
        let mut wrong = record("wrong", b"body");
        wrong.category = Category::Errors;
        assert!(!submit(wrong));
        assert!(!submit(record("huge", &vec![0u8; crate::telemetry::queue::MAX_RECORD])));
        assert_eq!(sends.load(Ordering::Relaxed), 0);
    }
}
