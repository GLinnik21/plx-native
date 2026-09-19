//! **Bounded transport for an explicitly pressed one-off report.**
//!
//! The ordinary path is the durable spool, as a [`Category::OneOff`] record: it sends on the next
//! flush and survives the app being closed first. If the spool cannot be written, ONE bounded
//! background send is allowed for the same record, so a television whose storage is the very
//! thing failing can still deliver the report a person pressed Send for. The render thread never
//! performs network I/O, and a second direct send while one is in flight is refused rather than
//! queued behind it.
//!
//! What the caller sees is the report's [`DeliveryState`] in `super::delivery`, keyed by its event
//! id: `Queued` once the spool took it — the ordinary flush settles it from there — and
//! `Sending`/`Delivered`/`Failed` for the direct fallback. The fallback is fenced by the account
//! tenure (`delivery::forget`), so a stale worker cannot publish into the next account's state.
//!
//! Ported from 0.6.6's `telemetry/oneoff.rs`, with its tests.

use super::delivery::{self, DeliveryState};
use super::queue::{Category, Record};
use super::sender::Verdict;
use std::sync::Mutex;

/// The tenure a direct fallback worker was started in. Remains occupied until the worker retires,
/// even after `delivery::forget` clears its visible state, so a second direct send is refused
/// rather than queued behind it.
static FALLBACK: Mutex<Option<u64>> = Mutex::new(None);

#[cfg(test)]
type TestSend = Box<dyn Fn(&Record) -> Verdict + Send>;
#[cfg(test)]
static TEST_SEND: Mutex<Option<TestSend>> = Mutex::new(None);

fn fallback_slot() -> std::sync::MutexGuard<'static, Option<u64>> {
    FALLBACK.lock().unwrap_or_else(|e| e.into_inner())
}

fn send_one(record: &Record) -> Verdict {
    #[cfg(test)]
    {
        TEST_SEND
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(Verdict::Hopeless, |send| send(record))
    }
    #[cfg(not(test))]
    {
        super::sender::send_one(record).0
    }
}

/// Ask the ordinary sender to drain the spool. Not from a host test: a checkout configured with a
/// DSN would otherwise put the suite's fixture records on a real project.
fn kick_flush() {
    #[cfg(not(test))]
    super::flush_soon();
}

fn retire(tenure: u64) {
    let mut slot = fallback_slot();
    if *slot == Some(tenure) {
        *slot = None;
    }
}

fn fallback(record: Record, tenure: u64) {
    // `delivery::forget` ends the tenure before the durable spool is purged. Do not even enter the
    // network seam for a stale request.
    if !delivery::current(tenure) {
        retire(tenure);
        return;
    }
    let verdict = send_one(&record);
    retire(tenure);
    let state = match verdict {
        Verdict::Done => DeliveryState::Delivered,
        Verdict::Keep | Verdict::Hopeless => DeliveryState::Failed,
    };
    delivery::settle_if_current(&record.event_id, state, tenure);
}

/// Submit an explicit one-off record. `true` means the durable queue accepted it or the bounded
/// direct fallback started; it does not mean the server has replied — `delivery::state` says that.
pub(crate) fn submit(record: Record) -> bool {
    if record.category != Category::OneOff || super::queue::encode(&record).is_none() {
        return false;
    }
    let tenure = delivery::tenure();
    match super::spool::append_watched_if(&record, tenure, || true) {
        Some(true) => {
            kick_flush();
            true
        }
        Some(false) => {
            let mut slot = fallback_slot();
            if !delivery::current(tenure) || slot.is_some() {
                return false;
            }
            if !delivery::watch(&record.event_id, DeliveryState::Sending, tenure) {
                return false;
            }
            *slot = Some(tenure);
            drop(slot);
            let event_id = record.event_id.clone();
            if crate::task::spawn_small("oneoff", move || fallback(record, tenure)) {
                true
            } else {
                retire(tenure);
                delivery::settle(&event_id, DeliveryState::Failed);
                false
            }
        }
        // The tenure ended, or no delivery watch could be admitted. Neither permits a fallback.
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::queue::Dest;
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;
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
            delivery::forget();
            crate::telemetry::spool::set_test_path(None);
            let _ = std::fs::remove_dir_all(&self.0);
            *TEST_SEND.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    fn setup(name: &str) -> Reset {
        let dir =
            std::env::temp_dir().join(format!("plxnative-oneoff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::telemetry::spool::set_test_path(Some(dir.join("spool.bin")));
        delivery::forget();
        Reset(dir)
    }

    /// Wait for a worker thread's state without a fixed sleep deciding the verdict.
    fn settle(id: &str, want: DeliveryState) -> Option<DeliveryState> {
        for _ in 0..200 {
            if delivery::state(id) == Some(want) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        delivery::state(id)
    }

    #[test]
    fn completion_before_append_returns_is_not_lost() {
        let _g = crate::testlock::serial();
        let _reset = setup("append-race");
        for verdict in [Verdict::Done, Verdict::Keep, Verdict::Hopeless] {
            delivery::forget();
            super::super::spool::on_append_for_test(move || {
                let records = super::super::spool::read();
                assert_eq!(records.len(), 1);
                let (retired, _) = super::super::process_records(
                    &records, &super::super::consent::Consent::default(), || true,
                    |_| (verdict, None),
                );
                super::super::spool::commit_retiring(&retired);
            });
            assert!(submit(record("append-race", b"body")));
            let expected = match verdict {
                Verdict::Done => DeliveryState::Delivered,
                Verdict::Keep => DeliveryState::Held,
                Verdict::Hopeless => DeliveryState::Failed,
            };
            assert_eq!(delivery::state("append-race"), Some(expected),
                "the ordinary flush settled before submit resumed");
            super::super::spool::commit_retiring(&["append-race".into()]);
        }
    }

    #[test]
    fn forget_before_append_returns_does_not_resurrect_the_watch() {
        let _g = crate::testlock::serial();
        let _reset = setup("append-forget");
        let tenure = delivery::tenure();
        super::super::spool::on_append_for_test(|| {
            delivery::forget();
            super::super::spool::purge_all_local();
        });
        assert!(submit(record("forgotten", b"body")));
        assert!(super::super::spool::read().is_empty());
        assert_eq!(delivery::state("forgotten"), None);
        delivery::settle_if_current("forgotten", DeliveryState::Delivered, tenure);
        assert_eq!(delivery::state("forgotten"), None);
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
        assert_eq!(delivery::state("queued"), Some(DeliveryState::Queued));
        assert_eq!(sends.load(Ordering::Relaxed), 0);
        let spooled: Vec<_> = crate::telemetry::spool::read().into_iter().map(|r| r.event_id).collect();
        assert_eq!(spooled, vec!["queued".to_string()]);
    }

    #[test]
    fn spool_failure_uses_one_bounded_direct_fallback_and_preserves_record_id() {
        let _g = crate::testlock::serial();
        let reset = setup("fallback");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone())); // a directory: append fails
        let (tx, rx) = mpsc::channel();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |r| {
            tx.send(r.event_id.clone()).unwrap();
            Verdict::Done
        }));
        assert!(submit(record("direct-id", b"body")));
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "direct-id");
        assert_eq!(settle("direct-id", DeliveryState::Delivered), Some(DeliveryState::Delivered));
    }

    #[test]
    fn only_one_direct_fallback_can_be_inflight() {
        let _g = crate::testlock::serial();
        let reset = setup("cap");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |_| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
            Verdict::Done
        }));
        assert!(submit(record("first", b"body")));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!submit(record("second", b"body")));
        let _ = release_tx.send(());
        assert_eq!(settle("first", DeliveryState::Delivered), Some(DeliveryState::Delivered));
    }

    #[test]
    fn forget_discards_state_and_stale_worker_cannot_publish() {
        let _g = crate::testlock::serial();
        let reset = setup("forget");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel();
        *TEST_SEND.lock().unwrap() = Some(Box::new(move |_| {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
            let _ = done_tx.send(());
            Verdict::Done
        }));
        assert!(submit(record("stale", b"body")));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        delivery::forget();
        assert_eq!(delivery::state("stale"), None);
        assert!(!submit(record("new-while-old-worker-live", b"body")));
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(delivery::state("stale"), None, "a stale worker published into the next tenure");
    }

    #[test]
    fn direct_keep_is_recorded_as_failed() {
        let _g = crate::testlock::serial();
        let reset = setup("keep");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        *TEST_SEND.lock().unwrap() = Some(Box::new(|_| Verdict::Keep));
        assert!(submit(record("keep", b"body")));
        assert_eq!(settle("keep", DeliveryState::Failed), Some(DeliveryState::Failed));
    }

    #[test]
    fn direct_hopeless_is_recorded_as_failed() {
        let _g = crate::testlock::serial();
        let reset = setup("hopeless");
        crate::telemetry::spool::set_test_path(Some(reset.0.clone()));
        *TEST_SEND.lock().unwrap() = Some(Box::new(|_| Verdict::Hopeless));
        assert!(submit(record("hopeless", b"body")));
        assert_eq!(settle("hopeless", DeliveryState::Failed), Some(DeliveryState::Failed));
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
