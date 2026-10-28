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

fn pending_write_saved(browse: &TestBrowse) -> Option<bool> {
    browse.state.pending_pins.as_ref()
        .map(|(_, write)| write.lock().unwrap_or_else(|e| e.into_inner()).saved())
}

/// Park the storage worker until the returned sender fires, so a write queued meanwhile is
/// unsaved and its receipt cannot land.
fn parked_worker() -> (std::sync::mpsc::Sender<()>, plx_base::storage_worker::TypedTicket<()>) {
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let blocker = plx_base::storage_worker::submit(move || {
        entered_tx.send(()).unwrap();
        let _ = release_rx.recv();
    }).unwrap();
    entered_rx.recv().unwrap();
    (release_tx, blocker)
}

/// **F2.** A latch belongs to the write that set it: observing write A says nothing about write B.
#[test]
fn a_latch_belongs_to_its_own_write() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-latch");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    browse.state.apply_pins(&[(2, true)]);
    plx_base::storage_worker::drain_for_test();
    browse.observe_pin_write();
    assert_eq!(pending_write_saved(&browse), Some(true), "A is saved and latched");

    let (release, _blocker) = parked_worker();
    {
        let _frame = plx_base::task::FrameScope::enter();
        browse.state.apply_pins(&[(0, false)]);
    }
    assert_eq!(pending_write_saved(&browse), Some(false), "B is a new write, and unsaved");
    browse.state.resolve_pins_from(&peek(), "u-owner");
    assert!(!browse.pinned(0) && browse.pinned(2), "B's local record stands while its receipt is out");
    assert_eq!(pending_write_saved(&browse), Some(false), "B is still the pending write");

    release.send(()).unwrap();
    plx_base::storage_worker::drain_for_test();
    browse.observe_pin_write();
    browse.state.resolve_pins_from(&peek(), "u-owner");
    assert!(!browse.pinned(0) && browse.pinned(2), "B's saved record, read from the registry");
    assert_eq!(pending_write_saved(&browse), None, "nothing pending once B was seen saved");
}

/// **F3.** A receipt is consumed only once it was observed: a resolve on a session read before
/// the observation leaves the write pending.
#[test]
fn a_receipt_is_consumed_only_once_observed() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-consume");
    t.watching("u-owner");
    let (release, _blocker) = parked_worker();
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    let stale;
    {
        let _frame = plx_base::task::FrameScope::enter();
        browse.state.apply_pins(&[(2, true)]);
        stale = peek();
    }
    release.send(()).unwrap();
    plx_base::storage_worker::drain_for_test();
    browse.state.resolve_pins_from(&stale, "u-owner");
    assert!(pending_write_saved(&browse).is_some(), "the receipt was not observed, so the write is still pending");

    browse.observe_pin_write();
    let fresh = peek();
    browse.state.resolve_pins_from(&fresh, "u-owner");
    assert_eq!(pending_write_saved(&browse), None, "observed, then consumed");
    assert_eq!(browse.state.recorded, fresh.pins_for("u-owner").cloned(), "the record now comes from the registry");
}

/// **G1.** The gate asks for the profile that is watching, on the record the handed session holds
/// for that profile and no other.
#[test]
fn the_gate_judges_the_published_profile() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-profile");
    t.watching("u-owner");
    let mut browse = TestBrowse::default();
    seed_two_servers(&mut browse);
    let asked = |user: &str| plx_plex::plex::session::HomePins {
        user: user.into(), asked: true, ..Default::default()
    };
    let mut other = Session::default();
    other.set_pins_for("u-other", asked("u-other"));
    assert!(browse.first_run_asks_with(&other), "a record for another profile does not answer for this one");
    let mut owner = Session::default();
    owner.set_pins_for("u-owner", asked("u-owner"));
    assert!(!browse.first_run_asks_with(&owner), "this profile's own record answers it");
}

/// `BrowseStore::run` is the fan-outs' entry and holds no session: a pin commit sent through it
/// is refused loudly (a release build drops it with a log line) instead of reading the registry.
#[test]
#[should_panic(expected = "handed no session")]
fn a_pin_command_sent_through_the_sessionless_entry_is_refused() {
    let _serial = plx_base::testlock::serial();
    let mut store = crate::stores::browse::BrowseStore::default();
    store.run(crate::stores::browse::BrowseCmd::ApplyPins(vec![(0, true)]), &ClientKeys::default());
}

/// The harness pump observes the write BEFORE its roster sync, as `Bridge::browse_session` does
/// before `BrowseStore::pump_with_gate`: the roster reconcile then sees a saved write as saved.
#[test]
fn the_harness_pump_observes_the_write_before_the_roster_sync() {
    let _serial = plx_base::testlock::serial();
    let t = TempPins::new("pins-input-pump-order");
    t.watching("u-owner");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    let sid = plx_plex::plex::register_for_test("pins-order", "127.0.0.1", 9, "tok", "cid");
    assert!(plx_plex::plex::set_current(sid));
    let mut source = a_source("mac-mini", "", true);
    source.sid = sid;
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![source]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "TV Shows".into(), SecKind::Show)]);
    browse.state.apply_pins(&[(1, false)]);
    plx_base::storage_worker::drain_for_test();
    t.watching("u-owner");
    let _ = browse.pump();
    assert_eq!(pending_write_saved(&browse), None, "the roster sync consumed the saved write");
}
