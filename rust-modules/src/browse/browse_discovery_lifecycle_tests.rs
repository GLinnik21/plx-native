//! Section/page/directory discovery landing lifecycle: repoint, retoken, profile-reset
//! staleness guards, blocking discovery, and single-flight reset.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn addressed_discovery_retry_rejects_retired_tables_and_other_sources() {
    let _guard = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    seed_sources_for_owner_test(&mut browse.state, 2, false);
    browse.state.source_mut(0).unwrap().retry_cd = 9;
    browse.state.source_mut(1).unwrap().retry_cd = 13;
    let sid = browse.state.sources()[1].sid;
    let epoch = browse.state.table_epoch();
    assert!(browse.state.retry_source(epoch, sid));
    assert_eq!(browse.state.sources()[0].retry_cd, 9);
    assert_eq!(browse.state.sources()[1].retry_cd, 0);
    browse.state.source_mut(1).unwrap().retry_cd = 17;
    assert!(!browse.state.retry_source(epoch.wrapping_add(1), sid));
    assert!(!browse.state.retry_source(epoch, ServerId::from_raw(99)));
    assert_eq!(browse.state.sources()[1].retry_cd, 17);
}
#[test]
fn a_settled_query_change_with_unknown_total_is_still_page_work() {
    let _g = crate::testlock::serial();
    let (_cleanup, browse, _, _) = registered_resident_page_source();
    let mut state = browse.state;
    let adapter = BrowseAdapter::default();
    let _ = state.sync_roster_owned();
    for source in &mut state.sources {
        source.sections_done = true;
        source.counts_done = true;
    }
    adapter.src_fetching.store(true, Ordering::SeqCst);
    assert!(!state.pump_needs_work(&adapter), "the resident query starts settled");

    state.set_unwatched(true);

    assert_eq!(state.states[state.cur()].total, -1);
    assert!(state.pump_needs_work(&adapter),
        "unknown total means page zero is owed even before a wanted window is published");
}
#[test]
fn an_equal_size_profile_roster_replaces_the_inactive_source_instead_of_appending() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let mut browse = TestBrowse::default();
    let a = crate::plex::register_for_test("browse-a", "127.0.0.1", 1, "a", "cid");
    let b = crate::plex::register_for_test("browse-b", "127.0.0.1", 2, "b", "cid");
    browse.sync_roster();
    assert_eq!(browse.state.sources().iter().map(|s| s.sid).collect::<Vec<_>>(), [a, b]);

    crate::plex::revoke_for_profile_switch();
    let c = crate::plex::register_for_test("browse-c", "127.0.0.1", 3, "c", "cid");
    assert_eq!(
        crate::plex::server_count(),
        2,
        "the replacement deliberately preserves count"
    );
    browse.sync_roster();
    assert_eq!(browse.state.sources().iter().map(|s| s.sid).collect::<Vec<_>>(), [a, c]);

    crate::plex::reset_servers_for_test();
}
#[test]
fn filling_a_source_name_refreshes_the_retained_directory() {
    let _g = crate::testlock::serial();
    let _cleanup = RegisteredCleanup;
    crate::plex::reset_servers_for_test();
    let mut browse = TestBrowse::default();
    let sid = crate::plex::register_for_test("browse-name-fill", "127.0.0.1", 1, "t", "cid");
    crate::plex::describe_server(sid, "", "", crate::plex::GrantEvidence::ours());
    browse.sync_roster();
    let mut directory = view::DirectorySnapshot::default();
    directory.capture_from(&mut browse.state);
    let old = directory.clone();
    let generation = browse.state.source_list_gen();
    crate::plex::describe_server_name(sid, "Learned");
    browse.sync_roster();
    assert_ne!(browse.state.source_list_gen(), generation);
    directory.capture_from(&mut browse.state);
    assert_eq!(directory.view().sources()[0].1.name, "Learned");
    assert_eq!(old.view().sources()[0].1.name, "");
    let generation = browse.state.source_list_gen();
    browse.sync_roster();
    assert_eq!(
        browse.state.source_list_gen(),
        generation,
        "a steady name does not republish"
    );
}
/// **A corrected credit reaches the Library panel.** `plex::servers::owner_credit` is the one
/// rule, but this table is a per-source CACHE in front of it, and it used to fill the handle
/// only while its own copy was empty — so a source that had once been captioned could never
/// lose or change that caption, whatever the registry later learned.
///
/// That is the reported bug's last hop. A session written before the rule existed publishes the
/// account holder's own handle against the household's server; the roster refresh re-grades it
/// and `describe` takes it off the registry — and the Sources panel and the Library read-out
/// went on saying "Shared by …" regardless.
///
/// The machine NAME keeps its fill-only behaviour in the same block, deliberately: it is the
/// one field here that a rename would churn under an open panel.
#[test]
fn a_source_follows_a_corrected_credit_but_not_a_renamed_machine() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let mut browse = TestBrowse::default();
    let sid = crate::plex::register_for_test("browse-credit", "127.0.0.1", 1, "t", "cid");

    // what a build without the rule left in the registry at boot
    crate::plex::describe_server(sid, "Mac mini", "admin", crate::plex::GrantEvidence::outside());
    browse.sync_roster();
    assert_eq!(
        browse.state.sources().first().map(|s| (s.handle.as_str(), s.owned)),
        Some(("admin", false))
    );

    // the roster refresh lands, re-graded: nobody is credited for the household's own server
    crate::plex::describe_server(sid, "Mac mini", "", crate::plex::GrantEvidence::outside());
    browse.sync_roster();
    assert_eq!(
        browse.state.sources().first().map(|s| s.handle.as_str()),
        Some(""),
        "the panel follows the registry off a credit, not only onto one"
    );

    // …and a rename still does not travel
    crate::plex::describe_server(sid, "nas-loft", "", crate::plex::GrantEvidence::outside());
    browse.sync_roster();
    assert_eq!(browse.state.sources().first().map(|s| s.name.as_str()), Some("Mac mini"));

    crate::plex::reset_servers_for_test();
}
#[test]
fn a_discovery_landing_from_before_a_same_slot_repoint_is_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_source();
    let old_gen = old.token_gen();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
        sid
    );
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

    queue_success_from(&mut browse, old, old_gen);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unauthorized)
    );
    assert_eq!(browse.state.sources()[0].name, "original");
    assert!(
        browse.state.sections().is_empty(),
        "old-origin rows must not enter the new lifecycle"
    );
}
#[test]
fn endpoint_outcomes_follow_only_current_failed_discovery_through_both_pumps() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_source();
    with_refused_discovery_for_test(|| {
        queue_discovery_for_owner_test(
            &mut browse.state, &browse.adapter, client, client.token_gen(), false);
        let requests = browse.discover_pump();
        assert_eq!(requests.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), [sid]);
        assert!(
            !crate::plex::write_held_for_test(),
            "intent returned after lifecycle lock release"
        );
        queue_discovery_for_owner_test(
            &mut browse.state, &browse.adapter, client, client.token_gen(), false);
        assert_eq!(browse.pump().endpoints.iter().count(), 1);
        queue_discovery_for_owner_test(
            &mut browse.state, &browse.adapter, client, client.token_gen(), true);
        assert_eq!(browse.discover_pump().endpoints.iter().count(), 0);
        let old_gen = client.token_gen();
        client.set_token("new-synthetic");
        queue_discovery_for_owner_test(
            &mut browse.state, &browse.adapter, client, old_gen, false);
        assert_eq!(browse.pump().endpoints.iter().count(), 0);
    });
}
#[test]
fn controlled_discovery_replays_refusal_retry_and_success_with_exact_identity() {
    let _serial = crate::testlock::serial();
    let (_cleanup, browse, sid, client) = registered_source();
    // Two independent executions start from this pre-request store value. No worker is
    // running: only the OS executor is substituted, below the real discovery policy.
    let initial_epoch = browse.state.table_epoch();
    let stores = crate::stores::Stores::default();
    let mt = unsafe { crate::task::MainThread::assume() };
    let mut transcript: Vec<std::collections::VecDeque<serde_json::Value>> = Vec::new();
    let mut states = Vec::new();
    let mut successful_result = None;
    let mut parsed: Option<crate::ui::rec::Recording> = None;
    for replay in [false, true] {
        // seed_sources is a reset fixture; restore the same pre-execution epoch on each
        // independent run, never to cancel or bypass a guard during either history.
        stores.browse.borrow_mut().prepare_discovery_replay_for_test(sid, initial_epoch);
        let publisher = crate::plex::session::ProfilePublisher::scoped(&mt);
        let mut io = crate::app::HomeIo {
            replay,
            preferences: Default::default(),
            requests: Vec::new(),
            admissions: Default::default(),
            failure: None,
            profile: publisher.snapshot(),
        };
        let mut attempts = 0;
        let sink = crate::ui::rec::MemSink::default();
        let bytes = sink.segments.clone();
        let header = crate::ui::rec::Header::new(17, &crate::ui::press::Press::new());
        let mut writer = crate::ui::rec::Writer::open(Box::new(sink), &header, 0).unwrap();
        for frame in 0..=602 {
            if replay {
                io.admissions = parsed.as_ref().unwrap().frames[frame]
                    .effects
                    .iter()
                    .map(|effect| effect["payload"].clone())
                    .collect();
            }
            io.discovery_owned_with(&stores, &mut |request| {
                assert!(!replay, "replay must never execute live admission");
                attempts += 1;
                if attempts == 1 { return false; }
                let descriptor = request.descriptor();
                successful_result = Some(serde_json::json!({"kind":"discovery","version":1,
                    "epoch":descriptor["epoch"],"source":descriptor["source"],"sid":descriptor["sid"],
                    "client":descriptor["client"],"token_gen":descriptor["token_gen"],
                    "name":"s00000001","what":{"Sections":[]}}));
                true
            });
            if frame == 600 {
                let result =
                    record::decode(successful_result.clone().expect("retry admitted"), |id| {
                        (id == client.instance_gen()).then_some(client)
                    })
                    .unwrap();
                assert!(stores.browse.borrow_mut()
                    .apply_discovery(&result, &io.preferences)
                    .endpoints
                    .iter()
                    .next()
                    .is_none());
            }
            let requests: std::collections::VecDeque<_> =
                std::mem::take(&mut io.requests).into();
            let state = stores.browse.borrow().discovery_policy_for_test();
            assert!(
                io.failure.is_none() && io.admissions.is_empty(),
                "frame={frame} replay={replay} failure={:?} pending={}",
                io.failure,
                io.admissions.len()
            );
            if replay {
                assert_eq!(
                    requests, transcript[frame],
                    "request/answer identity and frame must match"
                );
                assert_eq!(
                    state, states[frame],
                    "refusal/retry policy must match normal execution"
                );
            } else {
                writer.tick(
                    frame as u64,
                    crate::ui::machine::Tick {
                        ms: frame as u32 * 16,
                        dt_us: 16_000,
                    },
                );
                for request in &requests {
                    writer.effect_payload(frame as u64, "Cache", "Request", request.clone());
                }
                writer.flush_frame().unwrap();
                transcript.push(requests);
                states.push(state);
            }
        }
        assert!(
            stores.browse.borrow().discovery_policy_for_test().2,
            "admitted retry delivers its real result"
        );
        assert_eq!(attempts, if replay { 0 } else { 2 });
        writer.finish().unwrap();
        if !replay {
            let bytes = bytes.borrow();
            parsed = Some(
                crate::ui::rec::Recording::parse(
                    &header.to_json().to_string(),
                    &bytes.iter().map(Vec::as_slice).collect::<Vec<_>>(),
                    17,
                )
                .unwrap(),
            );
        }
    }
    assert_eq!(transcript[0][0]["admitted"], serde_json::json!(false));
    assert!(transcript[1..600]
        .iter()
        .all(std::collections::VecDeque::is_empty));
    assert_eq!(transcript[600][0]["admitted"], serde_json::json!(true));
}
#[test]
fn a_same_slot_repoint_rearms_section_discovery_without_erasing_known_rows() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    assert!(browse.state.sources()[0].sections_done);
    assert_eq!(browse.section_count(), 1);
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
        sid
    );

    browse.sync_roster();

    assert!(
        !browse.state.sources()[0].sections_done,
        "the new lifecycle must enumerate again"
    );
    assert_eq!(
        browse.section_count(),
        1,
        "known rows stay visible until a fresh answer lands"
    );
}
#[test]
fn a_discovery_landing_from_before_an_in_place_retoken_is_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_source();
    let old_gen = old.token_gen();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
        sid
    );
    assert!(
        std::ptr::eq(old, crate::plex::client_for(sid).unwrap()),
        "retoken stays in place"
    );
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

    queue_success_from(&mut browse, old, old_gen);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unauthorized)
    );
    assert_eq!(browse.state.sources()[0].name, "original");
    assert!(browse.state.sections().is_empty());
}
#[test]
fn a_discovery_landing_from_before_a_profile_reset_is_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_source();
    let old_gen = old.token_gen();
    crate::plex::revoke_for_profile_switch();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "profile", "cid"),
        sid
    );
    crate::plex::finish_profile_switch(&[sid]);
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);

    queue_success_from(&mut browse, old, old_gen);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unreachable)
    );
    assert_eq!(browse.state.sources()[0].name, "original");
    assert!(browse.state.sections().is_empty());
}
#[test]
fn page_failure_and_recovery_republish_directory_reachability() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_resident_page_source();
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Reachable);
    // A fully discovered, current lifecycle: neither directory discovery nor the next-page
    // scheduler has work. Only the injected page results below can change the source.
    browse.sync_roster();
    let source = browse.state.source_mut(0).unwrap();
    source.client_addr = client as *const _ as usize;
    source.token_gen = client.token_gen();
    source.sections_done = true;
    source.counts_done = true;
    let mut directory = view::DirectorySnapshot::default();
    directory.capture_from(&mut browse.state);
    let original = directory.clone();
    for (total, expected, fetch) in [
        (-1, SourceState::Unreachable, SecFetch::Failed),
        (-1, SourceState::Unreachable, SecFetch::Failed),
        (1, SourceState::Reachable, SecFetch::Ready),
    ] {
        let changed = browse.state.sources()[0].state != expected;
        let generation = browse.state.source_list_gen();
        *browse.adapter.page_result.lock().unwrap() = Some(PageResult {
            client,
            token_gen: client.token_gen(),
            gen: browse.state.query_gen(),
            sec: 0,
            start: 0,
            items: if total < 0 {
                vec![]
            } else {
                vec![PmsMovie {
                    sid,
                    ..Default::default()
                }]
            },
            total,
            sorts: None,
        });
        browse.adapter.fetching.store(true, Ordering::SeqCst);
        let _outcome = browse.pump();
        assert_eq!(browse.state.sources()[0].state, expected);
        assert_eq!(
            browse.state.source_list_gen(),
            generation.wrapping_add(u32::from(changed))
        );
        // A later roster sync cannot be relied on to supply a missing notification: its
        // registry and local states already match after the page's atomic commit.
        browse.sync_roster();
        directory.capture_from(&mut browse.state);
        assert_eq!(directory.view().sources()[0].1.state, expected);
        assert_eq!(directory.view().source_fetch(), fetch);
        assert_eq!(original.view().sources()[0].1.state, SourceState::Reachable);
    }
}
#[test]
fn a_page_landing_from_before_a_same_slot_repoint_is_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_resident_page_source();
    let old_gen = old.token_gen();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
        sid
    );
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

    queue_page_from(&mut browse, old, old_gen);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unauthorized)
    );
    assert_eq!(browse.state.sources()[0].state, SourceState::Unauthorized);
    assert_eq!(
        browse.state.states()[0].total,
        1,
        "old-origin page must not replace the new lifecycle's rows"
    );
    assert_eq!(browse.state.states()[0].items.len(), 1);
}
#[test]
fn a_page_landing_from_before_an_in_place_retoken_is_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_resident_page_source();
    let old_gen = old.token_gen();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
        sid
    );
    assert!(std::ptr::eq(old, crate::plex::client_for(sid).unwrap()));
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);

    queue_page_from(&mut browse, old, old_gen);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unauthorized)
    );
    assert_eq!(browse.state.sources()[0].state, SourceState::Unauthorized);
    assert_eq!(browse.state.states()[0].total, 1);
    assert_eq!(browse.state.states()[0].items.len(), 1);
}
#[test]
fn a_page_landing_from_before_a_profile_reset_is_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_resident_page_source();
    let old_gen = old.token_gen();
    crate::plex::revoke_for_profile_switch();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "profile", "cid"),
        sid
    );
    crate::plex::finish_profile_switch(&[sid]);
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);

    queue_page_from(&mut browse, old, old_gen);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unreachable)
    );
    assert_eq!(browse.state.sources()[0].state, SourceState::Unreachable);
    assert_eq!(browse.state.states()[0].total, 1);
    assert_eq!(browse.state.states()[0].items.len(), 1);
}
#[test]
fn a_repoint_requested_after_validation_waits_for_the_local_page_commit() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_resident_page_source();
    let token_gen = client.token_gen();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (attempt_tx, attempt_rx) = std::sync::mpsc::channel();
    let committed = std::sync::Arc::new(AtomicBool::new(false));
    let seen = committed.clone();
    let repoint = std::thread::spawn(move || {
        // This worker races the registry's own `WRITE` mutex against the main thread's
        // `commit_reachability_if_current` closure below — that IS the property under test —
        // so it is not a bystander of some other module's test; it still writes the same
        // crate-global registry `crate::testlock::serial()` protects, and it joins back into
        // the outer test (below) strictly before that guard drops. See
        // `crate::testlock::adopt_current_thread`'s doc for the exact contract.
        crate::testlock::adopt_current_thread();
        start_rx.recv().unwrap();
        attempt_tx.send(()).unwrap();
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
            sid
        );
        assert!(
            seen.load(Ordering::SeqCst),
            "repoint returned before the browse commit released WRITE"
        );
    });

    let applied = crate::plex::commit_reachability_if_current(
        sid,
        client,
        token_gen,
        true,
        None,
        |outcome| {
            assert!(
                crate::plex::write_held_for_test(),
                "local page mutation must execute under WRITE"
            );
            start_tx.send(()).unwrap();
            attempt_rx.recv().unwrap(); // the other thread is now about to take WRITE
            assert!(browse.state.apply_source_outcome(0, client, outcome));
            browse.state.state_mut(0).unwrap().total = 2;
            committed.store(true, Ordering::SeqCst);
            true
        },
    );
    assert_eq!(applied, Some(true));
    repoint.join().unwrap();
    assert_eq!(browse.state.states()[0].total, 2);
    assert!(!std::ptr::eq(client, crate::plex::client_for(sid).unwrap()));
}
#[test]
fn directory_landings_from_before_a_same_slot_repoint_are_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_directory_source();
    let old_gen = old.token_gen();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
        sid
    );
    queue_directories_from(&mut browse, old, old_gen);
    assert_new_directories_survive(&browse);
}
#[test]
fn directory_landings_from_before_an_in_place_retoken_are_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_directory_source();
    let old_gen = old.token_gen();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
        sid
    );
    queue_directories_from(&mut browse, old, old_gen);
    assert_new_directories_survive(&browse);
}
#[test]
fn directory_landings_from_before_a_profile_reset_are_inert() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_directory_source();
    let old_gen = old.token_gen();
    crate::plex::revoke_for_profile_switch();
    assert_eq!(
        crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "profile", "cid"),
        sid
    );
    crate::plex::finish_profile_switch(&[sid]);
    queue_directories_from(&mut browse, old, old_gen);
    assert_new_directories_survive(&browse);
}
#[test]
fn directory_landings_for_the_current_lifecycle_commit_both_menus() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, _, client) = registered_directory_source();
    queue_directories_from(&mut browse, client, client.token_gen());
    let state = browse.state.states().first().unwrap();
    assert_eq!(state.genres[0].id, "stale");
    assert_eq!(*state.letters, vec![("S".into(), 99)]);
    assert!(state.genres_done);
    assert!(state.letters_done);
}
#[test]
fn blocking_section_discovery_discards_a_same_slot_repoint_during_the_request() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_source();
    let count = ensure_sections_with(&mut browse.state, |client| {
        assert!(std::ptr::eq(client, old));
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.2", 32400, "new", "cid"),
            sid
        );
        Some(vec![(1, "Stale Movies".into(), SecKind::Movie)])
    });

    assert_eq!(count, 0);
    assert!(
        browse.state.sections().is_empty(),
        "the old origin's section table must not land"
    );
    assert_eq!(
        crate::plex::server_probe_result(sid),
        None,
        "the replacement lifecycle stays unprobed"
    );
}
#[test]
fn blocking_section_discovery_discards_an_in_place_retoken_during_the_request() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, old) = registered_source();
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);
    let old_gen = old.token_gen();
    let count = ensure_sections_with(&mut browse.state, |client| {
        assert!(std::ptr::eq(client, old));
        assert_eq!(
            crate::plex::register_for_test("browse-life", "10.0.0.1", 32400, "new", "cid"),
            sid
        );
        Some(vec![(1, "Stale Movies".into(), SecKind::Movie)])
    });

    assert_ne!(old.token_gen(), old_gen);
    assert_eq!(count, 0);
    assert!(browse.state.sections().is_empty());
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unauthorized)
    );
}
#[test]
fn blocking_section_failure_preserves_an_auth_401_published_during_the_request() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, sid, _) = registered_source();
    let count = ensure_sections_with(&mut browse.state, |_| {
        crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);
        None
    });

    assert_eq!(count, 0);
    assert_eq!(
        crate::plex::server_probe_result(sid),
        Some(crate::plex::probe::Outcome::Unauthorized)
    );
    assert_eq!(
        browse.state.sources()[0].state,
        SourceState::Unauthorized,
        "browse mirrors the canonical merged answer"
    );
}
/// Regression: `reset()` dropped the three result mailboxes but left the single-flight
/// flags set, and those are cleared ONLY inside a successful mailbox take. Sequence:
/// scroll Library so a page fetch spawns → BACK to Home (pump stops running) → the worker
/// lands its result → switch profile → `install_pms` calls `reset()` and nulls the mailbox
/// → the flag is now true with nothing left that can ever clear it. `maybe_spawn` returns
/// early forever and the Library is a spinner until the app is killed.
#[test]
fn reset_clears_the_single_flight_flags_with_the_mailboxes() {
    let _g = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    browse.adapter.fetching.store(true, Ordering::SeqCst);
    browse.adapter.genre_fetching.store(true, Ordering::SeqCst);
    browse.adapter.letters_fetching.store(true, Ordering::SeqCst);
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = None;

    browse.reset();

    assert!(
        !browse.adapter.fetching.load(Ordering::SeqCst),
        "page fetch stayed latched — Library wedges"
    );
    assert!(
        !browse.adapter.genre_fetching.load(Ordering::SeqCst),
        "genre fetch stayed latched"
    );
    assert!(
        !browse.adapter.letters_fetching.load(Ordering::SeqCst),
        "letters fetch stayed latched"
    );
}
/// `reset()` must also drop the retry backoff, or a profile switch inherits the previous
/// user's cooldown and stalls the first page fetch for up to ~2s.
#[test]
fn reset_clears_the_retry_backoff() {
    // Takes the crate lock for the same reason the fetch-machine tests below do — see the note
    // there. `reset()` is the most destructive call in this module, and a test that makes it
    // without the lock is not testing concurrently, it is CORRUPTING whoever is.
    let _g = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    browse.state.retry_cd = 120;
    browse.reset();
    assert_eq!(browse.state.retry_cd, 0);
}

/// **The household verdict reaches the source table, and raw `owned` stays raw.**
///
/// The two rows are the reported bug's own shape: to a Plex Home MANAGED profile, plex.tv reports
/// its household's own server exactly as it reports a stranger's share — `owned:false`, a handle
/// in `sourceTitle` — so `owned` alone cannot tell them apart and everything reasoning with it
/// calls the user's own library somebody else's. `BrowseSource::household` is the derived answer,
/// computed where the source facts are already synced; `owned` keeps saying what the wire said.
///
/// Nothing consumes `household` yet — that is a separate lane. This is the reader that proves it
/// is carried and correct.
#[test]
fn the_source_table_tells_a_household_server_from_a_share_though_both_read_unowned() {
    const ADMIN_ID: i64 = 111_111;
    const FRIEND_ID: i64 = 987_654;
    let _g = crate::testlock::serial();
    let _cleanup = RegisteredCleanup;
    let _session = TempPins::new("browse-household");
    crate::plex::session::save(&crate::plex::session::Session {
        client_id: "cid-test".into(),
        home_users: vec![crate::plex::session::HomeUserRef {
            id: ADMIN_ID,
            uuid: "u-admin".into(),
            admin: true,
            ..Default::default()
        }],
        ..Default::default()
    });
    crate::plex::reset_servers_for_test();
    let mut browse = TestBrowse::default();
    let house = crate::plex::register_for_test("browse-house", "127.0.0.1", 1, "t", "cid");
    let share = crate::plex::register_for_test("browse-share", "127.0.0.1", 2, "t", "cid");
    // what a managed profile's own `/api/v2/resources` says about each
    crate::plex::describe_server(house, "Mac mini", "", crate::plex::GrantEvidence {
        owned: false, home: true, owner_id: ADMIN_ID,
    });
    crate::plex::describe_server(share, "nas-home", "friend", crate::plex::GrantEvidence {
        owned: false, home: false, owner_id: FRIEND_ID,
    });

    browse.sync_roster();

    let rows: Vec<(bool, bool)> = browse
        .state
        .sources()
        .iter()
        .map(|source| (source.owned, source.household))
        .collect();
    assert_eq!(
        rows,
        [(false, true), (false, false)],
        "plex.tv owns neither; only one of them is this house's",
    );
}
