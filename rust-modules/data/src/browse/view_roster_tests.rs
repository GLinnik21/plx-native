//! Retained directory identity is reused on a no-op fill and never mutated behind an old reader.
use super::*;
use crate::browse::{BrowseRosterInput, BrowseState};
use plx_plex::plex::ClientKeys;

#[test]
fn directory_noop_reuses_arc_and_facts_change_replaces_it_without_mutating_old_rows() {
    let sid = ServerId::from_raw(71);
    let first = Box::leak(Box::new(plx_plex::plex::ServerFacts {
        name: "Before".into(), handle: String::new(), owned: true, home: false, owner_id: 0,
    }));
    let second = Box::leak(Box::new(plx_plex::plex::ServerFacts {
        name: "Before".into(), handle: "friend".into(), owned: false, home: false, owner_id: 42,
    }));
    let mut keys = ClientKeys::default();
    keys.refill_roster(sid, 1, 1, [sid], |_| None, |_| Some(first), |_| None);
    let session = Default::default();
    let mut state = BrowseState::default();
    let mut snapshot = DirectorySnapshot::default();
    let input = BrowseRosterInput { keys: &keys, session: &session, visible_generation: 0, settled: false, profile_key: "p" };
    state.sync_roster_owned(&input);
    snapshot.capture_from(&mut state, &keys);
    let old = snapshot.clone();
    let allocation = Arc::as_ptr(&snapshot.data);
    for _ in 0..16 {
        assert!(!state.sync_roster_owned(&input).changed);
        snapshot.capture_from(&mut state, &keys);
        assert_eq!(Arc::as_ptr(&snapshot.data), allocation);
        assert!(snapshot.same_publication(&old));
    }
    keys.refill_roster(sid, 1, 1, [sid], |_| None, |_| Some(second), |_| None);
    state.sync_roster_owned(&BrowseRosterInput { keys: &keys, session: &session, visible_generation: 0, settled: false, profile_key: "p" });
    snapshot.capture_from(&mut state, &keys);
    assert_ne!(Arc::as_ptr(&snapshot.data), allocation);
    assert_eq!(old.view().sources()[0].1.name, "Before");
    assert_eq!(snapshot.view().sources()[0].1.name, "Before");
    assert_eq!(old.view().sources()[0].1.handle, "");

    assert_eq!(snapshot.view().sources()[0].1.handle, "friend");
    assert!(!state.sources()[0].owned);
    let changed = snapshot.clone();
    let generation = state.source_list_gen();
    assert!(!state.sync_roster_owned(&BrowseRosterInput { keys: &keys, session: &session, visible_generation: 0, settled: false, profile_key: "p" }).changed);
    snapshot.capture_from(&mut state, &keys);
    assert_eq!(state.source_list_gen(), generation);
    assert!(snapshot.same_publication(&changed));
}
