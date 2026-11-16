//! S4 core input/publication contracts. These synthetic slots deliberately have NO clients.
use super::*;
use crate::stores::browse::BrowseStore;

fn description(name: &str, owned: bool, home: bool, owner_id: i64) -> &'static plx_plex::plex::ServerFacts {
    Box::leak(Box::new(plx_plex::plex::ServerFacts {
        name: name.into(), handle: if owned { String::new() } else { "friend".into() },
        owned, home, owner_id,
    }))
}
fn fill(keys: &mut ClientKeys, current: ServerId, facts: Option<&'static plx_plex::plex::ServerFacts>) {
    keys.refill_roster(current, 1, 1, [ServerId::from_raw(61), ServerId::from_raw(62)],
        |_| None, |_| facts, |_| None);
}
fn input<'a>(keys: &'a ClientKeys, session: &'a plx_plex::plex::session::Session) -> BrowseRosterInput<'a> {
    BrowseRosterInput { tenure: 0, keys, session, visible_generation: 0, settled: false, profile_key: "profile" }
}

#[test]
fn all_registered_slots_survive_without_clients_and_missing_current_has_no_fallback() {
    let mut keys = ClientKeys::default();
    fill(&mut keys, ServerId::from_raw(99), None);
    let session = Default::default();
    let mut state = BrowseState::default();
    assert!(state.sync_roster_owned(&input(&keys, &session)).changed);
    assert_eq!(state.sources().len(), 2);
    assert!(state.sources().iter().all(|source| source.owned && source.client_key.is_none()));
    assert_eq!(state.cur_source_idx(keys.current()), None);
    let adapter = Arc::new(BrowseAdapter::default());
    let mut jobs = Vec::new();
    state.controlled_discover_owned(&adapter, &keys, &mut jobs);
    assert!(jobs.is_empty(), "no client means no admission or speculative claim");
    assert!(!adapter.src_fetching.busy());
}

#[test]
fn facts_only_capture_notices_once_reuses_strings_and_retains_old_publication() {
    let mut keys = ClientKeys::default();
    let first = description("Server long enough for reuse", false, false, 42);
    fill(&mut keys, ServerId::from_raw(61), Some(first));
    let session = Default::default();
    let mut store = BrowseStore::default();
    let mut snapshot = view::DirectorySnapshot::default();
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    assert!(store.take_notice().is_some());
    let old = snapshot.clone();
    let old_name = old.view().sources()[0].1.name.clone();
    assert_eq!(old.view().sources()[0].1.handle, "friend");

    let generation = store.gen();
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    assert!(store.take_notice().is_none());
    assert_eq!(store.gen(), generation);
    assert!(snapshot.same_publication(&old));
    // Same generations on purpose: actual content, not generation hints, controls publication.
    fill(&mut keys, ServerId::from_raw(61), Some(description("Server long enough for reuse", true, false, 0)));
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    assert!(store.take_notice().is_some());
    assert!(!snapshot.same_publication(&old));
    assert_eq!(old.view().sources()[0].1.name, old_name);
    assert_eq!(snapshot.view().sources()[0].1.name, old_name);
    assert_eq!(old.view().sources()[0].1.handle, "friend");
    assert_eq!(snapshot.view().sources()[0].1.handle, "");
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    assert!(store.take_notice().is_none());
}

#[test]
fn current_only_capture_is_a_publication_and_unchanged_capture_is_quiet() {
    let mut keys = ClientKeys::default();
    fill(&mut keys, ServerId::from_raw(61), None);
    let session = Default::default();
    let mut store = BrowseStore::default();
    let mut snapshot = view::DirectorySnapshot::default();
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    let _ = store.take_notice();
    let old = snapshot.clone();
    fill(&mut keys, ServerId::from_raw(62), None);
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    assert!(store.take_notice().is_some());
    assert!(!snapshot.same_publication(&old));
    assert_eq!(old.view().source().unwrap().0, ServerId::from_raw(61));
    assert_eq!(snapshot.view().source().unwrap().0, ServerId::from_raw(62));
    store.capture_directory(&mut snapshot, &input(&keys, &session));
    assert!(store.take_notice().is_none());
}

#[test]
fn settled_is_not_inferred_from_an_empty_session_and_description_removal_restores_browse_fallback() {
    let mut keys = ClientKeys::default();
    fill(&mut keys, ServerId::from_raw(61), Some(description("Shared", false, false, 42)));
    let session = Default::default();
    let mut state = BrowseState::default();
    let mut supplied = input(&keys, &session);
    state.sync_roster_owned(&supplied);
    assert!(!state.session_settled);
    supplied.settled = true;
    assert!(state.discovery_needs_pump(&BrowseAdapter::default(), &supplied));
    state.sync_roster_owned(&supplied);
    assert!(state.session_settled);
    fill(&mut keys, ServerId::from_raw(61), None);
    state.sync_roster_owned(&input(&keys, &session));
    assert!(state.sources().iter().all(|source| source.owned && source.household && !source.home));
}

#[test]
fn retained_source_string_capacities_do_not_move_on_unchanged_sync() {
    let mut keys = ClientKeys::default();
    fill(&mut keys, ServerId::from_raw(61), Some(description("Retained", false, false, 42)));
    let session = Default::default();
    let mut state = BrowseState::default();
    let supplied = input(&keys, &session);
    state.sync_roster_owned(&supplied);
    let source = &state.sources()[0];
    let pointers = (source.name.as_ptr(), source.handle.as_ptr(), state.sources.as_ptr());
    for _ in 0..16 {
        assert!(!state.sync_roster_owned(&supplied).changed);
        let source = &state.sources()[0];
        assert_eq!((source.name.as_ptr(), source.handle.as_ptr(), state.sources.as_ptr()), pointers);
    }
}

#[test]
fn late_household_evidence_without_fact_generation_move_regrades_the_whole_pin_table() {
    let mut keys = ClientKeys::default();
    let family = description("Family", false, false, 42);
    let outsider = description("Outsider", false, false, 99);
    keys.refill_roster(ServerId::from_raw(61), 1, 1,
        [ServerId::from_raw(61), ServerId::from_raw(62)], |_| None,
        |sid| Some(if sid == ServerId::from_raw(61) { family } else { outsider }), |_| None);
    let mut session = plx_plex::plex::session::Session::default();
    let mut state = BrowseState::default();
    state.sync_roster_owned(&input(&keys, &session));
    state.append_sections_with(0, vec![(1, "Family films".into(), SecKind::Movie)], &session, "profile");
    state.append_sections_with(1, vec![(1, "Shared films".into(), SecKind::Movie)], &session, "profile");
    assert!(!state.sources()[0].household);
    session.home_users.push(serde_json::from_value(serde_json::json!({
        "id": 42, "uuid": "household", "title": "Household", "thumb": "",
        "admin": true, "restricted": false, "protected": false
    })).unwrap());
    let mut supplied = input(&keys, &session);
    supplied.settled = true;
    assert!(state.discovery_needs_pump(&BrowseAdapter::default(), &supplied));
    assert!(state.sync_roster_owned(&supplied).changed);
    assert!(state.sources()[0].household);
    assert!(!state.sources()[1].household);
    assert!(state.sections()[0].pinned);
    assert!(!state.sections()[1].pinned, "whole-table default follows late household evidence");
    assert!(!state.sync_roster_owned(&supplied).changed);
}
