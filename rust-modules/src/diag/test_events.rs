//! Scoped, thread-local observation of typed event producers, never telemetry delivery.
use super::schema::DiagEvent;
use std::cell::RefCell;

thread_local! {
    static EVENTS: RefCell<Option<Vec<DiagEvent>>> = const { RefCell::new(None) };
}

pub(super) fn intercept(event: DiagEvent) -> bool {
    EVENTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(events) = slot.as_mut() else { return false };
        events.push(event);
        true
    })
}

/// Capture calls on this thread only. The scope bypasses the telemetry adapter entirely;
/// these observations prove producer behavior, not consent, persistence, or transport.
pub(crate) fn capture<T>(run: impl FnOnce() -> T) -> (T, Vec<DiagEvent>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) { EVENTS.with(|slot| *slot.borrow_mut() = None); }
    }
    EVENTS.with(|slot| {
        let mut slot = slot.borrow_mut();
        assert!(slot.is_none(), "nested event capture would hide the outer producer's events");
        *slot = Some(Vec::new());
    });
    let _reset = Reset;
    let result = run();
    let events = EVENTS.with(|slot| slot.borrow_mut().take().unwrap());
    (result, events)
}

#[test]
fn capture_observes_the_real_event_doors_and_restores_after_unwind() {
    use super::schema::Feature;
    let event = DiagEvent::FeatureUsed { feature: Feature::LibrarySwitch };
    let (answer, events) = capture(|| {
        super::event(event);
        super::event_for_server(event, crate::plex::ServerId::UNSET);
        7
    });
    assert_eq!(answer, 7);
    assert_eq!(events, [event, event]);
    assert!(!intercept(event), "the capture must not remain installed");
    let result = std::panic::catch_unwind(|| capture(|| panic!("synthetic producer failure")));
    assert!(result.is_err());
    assert!(!intercept(event), "unwinding must also release the capture");
    let (_, events) = capture(|| super::event(event));
    assert_eq!(events, [event]);
}
