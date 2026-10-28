//! The Home pins take the session as an input (docs/sans-io.md S5): the Bridge reads it, after the
//! store observed its pending pin write, and hands it to the step. Every test here seeds the registry
//! so that it DISAGREES with the session it hands, so a read of the registry shows as a wrong answer.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

use plx_plex::plex::session::{peek, Session};

fn without_pins(session: &Session) -> Session {
    let mut handed = session.clone();
    handed.home_pins.clear();
    handed
}

/// A saved pin write does not re-read the registry: the record comes from the session handed in.
#[test]
fn a_saved_pin_write_resolves_from_the_handed_session() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-saved");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    browse.state.apply_pins(&[(2, true)]);
    plx_base::storage_worker::drain_for_test();
    browse.observe_pin_write();
    assert!(peek().pins_for("u-owner").is_some(), "the registry holds the saved record");
    let handed = without_pins(&peek());
    browse.state.resolve_pins_from(&handed, "u-owner");
    assert!(!browse.pinned(2), "the handed session holds no record, so the friend's row is back to its default");
}

/// The first-run gate is judged on the session it is handed, not on the registry.
#[test]
fn the_first_run_gate_is_judged_on_the_handed_session() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-gate");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    browse.state.apply_pins(&[]);
    plx_base::storage_worker::drain_for_test();
    assert!(
        browse.first_run_asks_with(&Session::default()),
        "a session with no record for the profile asks, whatever the registry says"
    );
    assert!(!browse.first_run_asks_with(&peek()), "the registry's record says it was asked");
}

/// A session with no client id refuses the write, so nothing is queued and nothing is recorded.
#[test]
fn a_commit_handed_a_signed_out_session_writes_nothing() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-signed-out");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    browse.apply_pins_with(&[(2, true)], &Session::default());
    assert!(browse.state.pending_pins.is_none(), "no write was queued");
    plx_base::storage_worker::drain_for_test();
    assert!(peek().pins_for("u-owner").is_none(), "nothing reached the registry");
    assert!(browse.pinned(2), "the visible selection stands for this run");
}

/// A live discovery landing resolves its pins from the session the step was handed.
#[test]
fn a_live_discovery_landing_resolves_from_the_handed_session() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-landing");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    browse.state.apply_pins(&[(2, true)]);
    plx_base::storage_worker::drain_for_test();
    browse.observe_pin_write();
    assert!(browse.pinned(2));
    let handed = without_pins(&peek());
    browse.append_sections_live_with(1, vec![(7, "Documentaries".into(), SecKind::Movie)], &handed);
    assert_eq!(browse.section_count(), 4, "the landing added a row");
    assert!(!browse.pinned(2), "the table was rebuilt from the handed session, which holds no record");
}

/// **F1.** A receipt that lands after the snapshot was taken is never paired with that snapshot: the
/// poll comes before the read, and an unobserved write answers with its own local record.
#[test]
fn a_receipt_landing_after_the_snapshot_is_never_paired_with_it() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-stale");
    t.watching("u-owner");
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let _blocker = plx_base::storage_worker::submit(move || {
        entered_tx.send(()).unwrap();
        let _ = release_rx.recv();
    }).unwrap();
    entered_rx.recv().unwrap();
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    let stale;
    {
        let _frame = plx_base::task::FrameScope::enter();
        browse.state.apply_pins(&[(2, true)]);
        stale = peek();
    }
    assert!(stale.pins_for("u-owner").is_none(), "the snapshot predates the write");
    release_tx.send(()).unwrap();
    plx_base::storage_worker::drain_for_test();
    browse.state.resolve_pins_from(&stale, "u-owner");
    assert!(browse.pinned(2), "the older snapshot must not undo the viewer's saved choice");
}
