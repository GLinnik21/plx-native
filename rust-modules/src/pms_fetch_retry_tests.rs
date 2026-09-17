//! Worker request/response plumbing: addressed requests, endpoint outcomes, retry backoff,
//! and landing arrival handling.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::land;

#[test]
fn request_addresses_do_not_alias_across_sources_or_profile_resets() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    crate::plex::reset_servers_for_test();
    let a_sid = crate::plex::register_for_test("request-a", "a.invalid", 32400, "test", "cid");
    let b_sid = crate::plex::register_for_test("request-b", "b.invalid", 32400, "test", "cid");
    let ca = crate::plex::client_for(a_sid).unwrap();
    let cb = crate::plex::client_for(b_sid).unwrap();
    let mut first = Src::new(a_sid, String::new());
    let mut second = Src::new(b_sid, String::new());
    let mint = || o.adapter.next_request.fetch_add(1, Ordering::Relaxed);
    let mut held = None;
    kick_with(o.state.hub_gen, &o.adapter, &mut first, |request| { held = Some(request); true });
    let a = held.unwrap().seq;
    kick_with(o.state.hub_gen, &o.adapter, &mut first, |_| panic!("an in-flight source must not invoke the adapter again"));
    assert!(first.begin_request(ca, 1, || panic!("single flight must not mint again")).is_none());
    let b = second.begin_request(cb, 1, mint).unwrap().seq;
    first.fetching = false; // the prior attempt completed
    let retry = first.begin_request(ca, 1, mint).unwrap().seq;
    reset(&mut o.state, &o.adapter);
    let next_profile = Src::new(a_sid, String::new()).begin_request(ca, 2, mint).unwrap().seq;
    let mut ids = vec![a, b, retry, next_profile];
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    crate::plex::reset_servers_for_test();
}

#[test]
fn endpoint_outcomes_cover_missing_client_refusal_and_failed_arrival() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    crate::plex::reset_servers_for_test();
    reset(&mut o.state, &o.adapter);
    let mut missing = Src::new(sid(0), String::new());
    assert_eq!(kick_with(o.state.hub_gen, &o.adapter, &mut missing, |_| panic!("missing client cannot spawn")).unwrap().sid, sid(0));
    assert_eq!(missing.state, HubState::Failed);
    assert_eq!(missing.retry_s, RETRY_MIN_S);

    let id = crate::plex::register_for_test("endpoint-hub", "127.0.0.1", 9, "synthetic", "cid");
    let mut s = Src::new(id, String::new());
    assert_eq!(kick_with(o.state.hub_gen, &o.adapter, &mut s, |_| false).unwrap().sid, id);
    assert!(!s.fetching);
    assert_eq!(s.retry_s, RETRY_MIN_S);
    seed(&mut o.state, vec![s]);
    land(&o.state, &o.adapter, id.raw(), None);
    let result = take_landings(&o.adapter).pop().unwrap();
    let outcome = super::land(&mut o.state, &o.adapter, &result);
    assert_eq!(outcome.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), [id]);
    assert_eq!(o.state.srcs[0].retry_n, 2);
    reset(&mut o.state, &o.adapter);
    crate::plex::reset_servers_for_test();
}

#[test]
fn a_prepared_request_keeps_its_original_context_without_running_an_adapter() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("request", "old.invalid", 32400, "old", "cid");
    let client = crate::plex::client_for(sid).unwrap();
    let token_gen = client.token_gen();
    let mut source = Src::new(sid, String::new());
    source.last = Some(build_test(2));
    source.state = HubState::Failed;
    source.retry_n = 3;
    source.retry_s = 8.0;
    let generation = o.state.hub_gen;
    let request = source.begin_request(client, generation, || 23).unwrap();
    assert!(source.fetching);
    assert_eq!(source.state, HubState::Loading);
    assert_eq!(source.retry_s, 0.0);
    assert_eq!(source.retry_n, 3, "admission is not a successful fetch");
    assert_eq!(source.last.as_ref().unwrap().shelves[0].items.len(), 2);
    assert_eq!(source.seq, 23);
    client.set_token("new");
    assert_eq!(crate::plex::register_for_test("request", "new.invalid", 32400, "new", "cid"), sid);
    reset(&mut o.state, &o.adapter); // a different account epoch must not retag the request already handed out
    assert_ne!(o.state.hub_gen, generation);
    let result = request.complete(None);
    assert_eq!((result.gen, result.seq, result.sid, result.token_gen), (generation, 23, sid, token_gen));
    assert!(std::ptr::eq(result.client.unwrap().resource, client));
    assert!(result.build.is_none(), "a failed request is not an empty success");
    crate::plex::reset_servers_for_test();
}

#[test]
fn addressed_arrivals_and_retry_ticks_do_not_drain_the_mailbox() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(2)))]);
    land(&o.state, &o.adapter, 0, None);
    let failed = take_landings(&o.adapter).pop().unwrap();
    land(&o.state, &o.adapter, 0, Some(build_test(5)));
    let _outcome = apply_landing(&mut o.state, &o.adapter, &failed);
    assert_eq!(hub_len(&o.state, 0), 2, "a failure retains the last successful catalog");
    assert_eq!(hub_state(&o.state), HubState::Failed);
    assert_eq!(o.state.srcs[0].retry_s, RETRY_MIN_S, "delivery spends no retry time");
    let _outcome = tick(&mut o.state, &o.adapter, 0.5);
    assert_eq!(o.state.srcs[0].retry_s, RETRY_MIN_S - 0.5);
    assert_eq!(hub_len(&o.state, 0), 2, "the tick cannot consume the later success");
    let success = take_landings(&o.adapter).pop().unwrap();
    let _outcome = apply_landing(&mut o.state, &o.adapter, &success);
    assert_eq!(hub_len(&o.state, 0), 5);
    assert_eq!(hub_state(&o.state), HubState::Ready);
    reset(&mut o.state, &o.adapter);
}

#[test]
fn a_captured_batch_does_not_consume_later_worker_arrivals() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(2)))]);
    land(&o.state, &o.adapter, 0, Some(build_test(3)));
    let batch = take_landings(&o.adapter);
    assert_eq!(batch.len(), 1);
    assert!(take_landings(&o.adapter).is_empty(), "a drain transfers ownership once");
    assert_eq!(hub_len(&o.state, 0), 2, "capture alone must not apply an arrival");

    // The next worker can post while the captured batch is being observed. Applying that
    // batch must neither hold the mailbox lock nor silently pick up this later arrival.
    land(&o.state, &o.adapter, 0, Some(build_test(5)));
    let _outcome = pump_with_landings(&mut o.state, &o.adapter, 0.0, || batch);
    assert_eq!(hub_len(&o.state, 0), 3);
    let _outcome = pump_with_landings(&mut o.state, &o.adapter, 0.0, Vec::new);
    assert_eq!(hub_len(&o.state, 0), 3, "an empty supplied batch does not drain live work");
    pump(&mut o.state, &o.adapter, 0.0);
    assert_eq!(hub_len(&o.state, 0), 5, "the later arrival belongs to the next live drain");
    let generation = o.state.catalog_gen;
    pump(&mut o.state, &o.adapter, 0.0);
    assert_eq!(o.state.catalog_gen, generation, "an arrival is not applied twice");
    reset(&mut o.state, &o.adapter);
}

#[test]
fn a_captured_batch_is_still_rejected_after_an_identity_reset() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(2)))]);
    land(&o.state, &o.adapter, 0, Some(build_test(9)));
    let batch = take_landings(&o.adapter);
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(3)))]);
    let _outcome = pump_with_landings(&mut o.state, &o.adapter, 0.0, || batch);
    assert_eq!(hub_len(&o.state, 0), 3);
    assert_eq!(hub_state(&o.state), HubState::Ready);
    reset(&mut o.state, &o.adapter);
}

#[test]
fn a_same_slot_repoint_drops_the_old_flight_and_rearms_home() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("home-life", "10.0.0.1", 32400, "old", "cid");
    let old = crate::plex::client_for(sid).unwrap();
    let old_gen = old.token_gen();
    let mut s = Src::new(sid, String::new());
    s.state = HubState::Ready;
    s.fetching = true;
    s.last = Some(build_test(2));
    assert_eq!(
        crate::plex::register_for_test("home-life", "10.0.0.2", 32400, "new", "cid"),
        sid
    );

    assert!(refresh_src_lifecycle(&mut s));
    assert_eq!(s.state, HubState::Loading);
    assert!(!s.fetching);
    assert!(
        s.last.is_some(),
        "old shelves remain until the fresh lifecycle answers"
    );
    assert!(!crate::plex::client_for(sid)
        .is_some_and(|now| { std::ptr::eq(now, old) && now.token_gen() == old_gen }));
    crate::plex::reset_servers_for_test();
}

#[test]
fn the_backoff_doubles_then_holds_at_the_ceiling() {
    assert_eq!(
        backoff_secs(1),
        RETRY_MIN_S,
        "the first retry is the shortest wait"
    );
    assert_eq!(backoff_secs(2), 4.0);
    assert_eq!(backoff_secs(3), 8.0);
    assert_eq!(backoff_secs(4), 16.0);
    assert_eq!(backoff_secs(5), RETRY_MAX_S, "32s is past the ceiling");
    assert_eq!(
        backoff_secs(99),
        RETRY_MAX_S,
        "and it never grows past it (nor overflows)"
    );
}

/// The bug the fetch state machine exists for: a failed fetch used to commit an EMPTY catalog,
/// so one unreachable moment blanked a populated Home for good. A failure must leave every one
/// of the three statics exactly as it found them.
#[test]
fn a_failed_landing_never_blanks_a_populated_home() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(3)))]);
    assert_eq!(hub_count(&o.state), 1);
    assert_eq!(hub_len(&o.state, 0), 3);

    land(&o.state, &o.adapter, 0, None);
    pump(&mut o.state, &o.adapter, 0.0);

    assert_eq!(
        hub_state(&o.state),
        HubState::Failed,
        "the failure must be distinguishable"
    );
    assert_eq!(hub_count(&o.state), 1, "the shelves survive a failed refetch");
    assert_eq!(hub_len(&o.state, 0), 3);
    assert!(
        o.state.srcs[0].retry_s > 0.0,
        "and the next attempt is armed"
    );
    reset(&mut o.state, &o.adapter);
}

/// A landing that carries a build commits it, and a success retires that source's backoff so
/// its next failure starts at the bottom of the ladder instead of inheriting a 30s wait.
#[test]
fn a_successful_landing_commits_and_retires_the_backoff() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Loading, None)]);
    {
        landed_fail(&mut o.state.srcs[0]);
        landed_fail(&mut o.state.srcs[0]);
    }
    assert_eq!(hub_state(&o.state), HubState::Failed);

    land(&o.state, &o.adapter, 0, Some(build_test(2)));
    pump(&mut o.state, &o.adapter, 0.0);

    assert_eq!(hub_state(&o.state), HubState::Ready);
    assert_eq!(hub_len(&o.state, 0), 2);
    assert_eq!(o.state.srcs[0].retry_n, 0);
    assert_eq!(o.state.srcs[0].retry_s, 0.0);
    reset(&mut o.state, &o.adapter);
}

/// An answer of "nothing" is an ANSWER: it must land as Ready (Home's empty state), not as a
/// failure, or the screen would apologise for a server that is simply empty — and retry it
/// forever.
#[test]
fn a_server_with_no_hubs_is_ready_and_empty_not_failed() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![src(0, "", HubState::Loading, None)]);
    land(&o.state, &o.adapter, 0, Some(SourceBuild::default()));
    pump(&mut o.state, &o.adapter, 0.0);
    assert_eq!(hub_state(&o.state), HubState::Ready);
    assert_eq!(hub_count(&o.state), 0);
    reset(&mut o.state, &o.adapter);
}

/// The countdown is real time (seconds of `dt`), not frames like `browse.rs`'s — a device
/// that drops to 30fps must still retry on the same wall clock. NB `retry_due` keeps
/// reporting due once it is spent; what makes an attempt happen only once is `kick`
/// re-latching that source's single flight, not this.
#[test]
fn the_retry_countdown_fires_when_the_backoff_elapses() {
    let mut s = src(0, "", HubState::Loading, None);
    landed_fail(&mut s); // arms RETRY_MIN_S
    for _ in 0..3 {
        assert!(
            !retry_due(&mut s, 0.5),
            "0.5s at a time must not fire before the 2s wait is spent"
        );
    }
    assert!(retry_due(&mut s, 0.5), "the fourth half-second spends it");
}

/// A retry still in flight when the account changes must not commit the PREVIOUS identity's
/// hubs over the new one's: its landing carries the old generation and is dropped whole —
/// neither committed nor blamed on the current fetch. The same drop catches a SUPERSEDED
/// attempt at the same source within one generation, which is what the per-source seq is for:
/// an authoritative fetch releases every single-flight latch, so without it the worker that
/// call abandoned could still land on top of the one that replaced it.
#[test]
fn a_stale_or_superseded_landing_is_dropped_whole() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let stale = o.state.hub_gen;
    reset(&mut o.state, &o.adapter); // the identity change
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(2)))]); // …and the new identity's catalog
    let s0 = sid(0);
    let cur = o.state.hub_gen;
    {
        let mut r = o.adapter.results.lock().unwrap_or_else(|e| e.into_inner());
        r.push(Landing {
            gen: stale,
            seq: 0,
            sid: s0,
            client: None,
            token_gen: 0,
            build: Some(build_test(9)),
        });
        r.push(Landing {
            gen: cur,
            seq: 99,
            sid: s0,
            client: None,
            token_gen: 0,
            build: Some(build_test(7)),
        });
    }
    pump(&mut o.state, &o.adapter, 0.0);
    assert_eq!(hub_len(&o.state, 0), 2, "neither may replace the current catalog");
    assert_eq!(
        hub_state(&o.state),
        HubState::Ready,
        "nor be counted as a failure of the current one"
    );
    reset(&mut o.state, &o.adapter);
}
