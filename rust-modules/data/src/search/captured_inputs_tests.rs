use super::*;
use crate::captured_store_fixture::{key, roster};
use crate::stores::search::SearchCmd;
fn input<'a>(keys: &'a ClientKeys, session: &'a plx_plex::plex::session::Session, profile: &'a str) -> SearchInput<'a> {
    SearchInput { keys, recents: recents::Input { session, settled: true, profile_generation: 7,
        session_generation: 11, tenure: 99, profile_key: profile } }
}
#[test]
fn captured_roster_includes_unkeyed_slots_and_ignores_conflicting_ambient() {
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let ambient = plx_plex::plex::register_for_test("ambient-search", "127.0.0.1", 9, "ambient", "ambient");
    let keys = roster(&[(61, Some(key(1)), "captured"), (101, None, "unkeyed")], 3, 4);
    let session = Default::default();
    let input = input(&keys, &session, "alpha");
    let directory = crate::stores::browse::DirectorySnapshot::default();
    let adapter = Arc::new(SearchAdapter::default());
    let mut state = SearchState::default();
    state.run_with_directory_and_input(&adapter, SearchCmd::SetQuery("captured query".into()), directory.view(), &input);
    state.pump_with_directory_and_input_and_gate(&adapter, 0.3, directory.view(), &plx_machine::landgate::Gate::default(), &input);
    let jobs = std::mem::take(&mut state.jobs);
    assert_eq!(jobs.len(), 1, "unkeyed registered slots do not create requests");
    let crate::stores::StoreJob::Search(job, port) = jobs.into_iter().next().unwrap() else { panic!("search job") };
    assert_eq!(job.req.sid, ServerId::from_raw(61));
    let scope = state.scope_cache.snapshot_with_directory_and_input(directory.view(), &input, &[]);
    assert_eq!(scope.sources().iter().map(|s| s.sid.raw()).collect::<Vec<_>>(), [61, 101]);
    assert_eq!(scope.sources()[1].handle, "unkeyed", "partial Browse directory is not an authorization roster");
    port.refused(job.addr);
    state.pump_with_directory_and_input_and_gate(&adapter, 0.016, directory.view(), &plx_machine::landgate::Gate::default(), &input);
    assert!(state.jobs.is_empty(), "a refusal is observed rather than resent every pump");
    assert_eq!(plx_plex::plex::server_ids().collect::<Vec<_>>(), [ambient], "scripted inputs never install themselves into ambient state");
    plx_plex::plex::reset_servers_for_test();
}
#[test]
fn same_generation_full_keys_fence_late_answers_and_reset_uses_captured_roster() {
    let _serial = plx_base::testlock::serial();
    let a = roster(&[(61, Some(key(1)), "")], 3, 4);
    let b = roster(&[(61, Some(key(2)), "")], 3, 4);
    let session = Default::default();
    let directory = crate::stores::browse::DirectorySnapshot::default();
    let adapter = Arc::new(SearchAdapter::default());
    let mut state = SearchState::default();
    state.run_with_directory_and_input(&adapter, SearchCmd::SetQuery("old query".into()), directory.view(), &input(&a, &session, "alpha"));
    state.pump_with_directory_and_input_and_gate(&adapter, 0.3, directory.view(), &plx_machine::landgate::Gate::default(), &input(&a, &session, "alpha"));
    let old = state.gen;
    state.jobs.clear();
    state.pump_with_directory_and_input_and_gate(&adapter, 0.0, directory.view(), &plx_machine::landgate::Gate::default(), &input(&b, &session, "alpha"));
    assert_ne!(state.gen, old, "same generation is not the full credential key");
    land(&adapter, 61, old, Some(Default::default()));
    state.pump_with_directory_and_input_and_gate(&adapter, 0.0, directory.view(), &plx_machine::landgate::Gate::default(), &input(&b, &session, "alpha"));
    assert!(state.shelves().is_empty(), "the retired request cannot publish");
    state.run_with_directory_and_input(&adapter, SearchCmd::Reset, directory.view(), &input(&b, &session, "alpha"));
    assert!(state.query().is_empty());
    assert_eq!(state.visible, b.roster_gen());
    state.run_with_directory_and_input(&adapter, SearchCmd::SetQueryScoped { profile_generation: 6, query: "stale".into() }, directory.view(), &input(&b, &session, "beta"));
    assert!(state.query().is_empty(), "a stale draft cannot cross the captured profile epoch");
}
#[test]
fn facts_only_and_roster_only_observations_each_publish_once_at_equal_directory_generations() {
    let _serial = plx_base::testlock::serial();
    let a = roster(&[(61, None, "old credit")], 3, 4);
    let b = roster(&[(61, None, "new credit")], 3, 4);
    let c = roster(&[(101, None, "new credit")], 3, 4);
    let session = Default::default();
    let directory = crate::stores::browse::DirectorySnapshot::default();
    let cache = scope::ScopeCache::default();
    let first = cache.snapshot_with_directory_and_input(directory.view(), &input(&a, &session, "alpha"), &[]);
    let changed = cache.snapshot_with_directory_and_input(directory.view(), &input(&b, &session, "alpha"), &[]);
    assert!(!first.same_publication(&changed));
    assert_eq!(first.sources()[0].handle, "old credit", "retained facts never change under their reader");
    assert_eq!(changed.sources()[0].handle, "new credit");
    assert!(changed.same_publication(&cache.snapshot_with_directory_and_input(directory.view(), &input(&b, &session, "alpha"), &[])));
    let replaced = cache.snapshot_with_directory_and_input(directory.view(), &input(&c, &session, "alpha"), &[]);
    assert_eq!(replaced.sources()[0].sid.raw(), 101, "equal-sized same-generation roster replacement is observed");
}
