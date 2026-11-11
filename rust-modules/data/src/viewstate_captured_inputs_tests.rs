use super::*;
use crate::captured_store_fixture::{key, roster};
#[test]
fn origin_fanout_counts_registered_captured_slots_not_eligible_or_ambient_slots() {
    let _serial = plx_base::testlock::serial();
    let sid = ServerId::from_raw(61);
    let one = roster(&[(61, Some(key(1)), "")], 3, 4);
    let two = roster(&[(61, Some(key(1)), ""), (101, None, "unkeyed")], 3, 4);
    let adapter = Arc::new(ViewStateAdapter::default());
    let mut state = ViewStateState::default();
    assert!(!state.fan_out_origin_watched(&adapter, sid, "4", &one, 7));
    assert!(state.fan_out_origin_watched(&adapter, sid, "4", &two, 7), "a registered unkeyed destination still makes this a multi-source observation");
    let StoreJob::ViewWrite(job, _) = state.jobs.pop().unwrap() else { panic!("view write") };
    assert!(job.req.origin_done);
    assert_eq!(job.req.sid, sid);
    assert_eq!(job.req.keys.slots().map(|s| s.raw()).collect::<Vec<_>>(), [61, 101]);
    assert_eq!(job.req.keys.iter().count(), 1);
    let missing = roster(&[(61, None, ""), (101, Some(key(2)), "friend")], 3, 4);
    let mut refused = ViewStateState::default();
    assert!(!refused.fan_out_origin_watched(&adapter, sid, "4", &missing, 7));
    assert!(refused.jobs.is_empty(), "missing origin input is not filled from the registry");
}
#[test]
fn worker_one_captured_slot_never_queries_even_when_ambient_has_two() {
    fn forbidden(_: ServerId) -> Option<&'static Client> { panic!("the captured one-server job must not look up any destination") }
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let origin = plx_plex::plex::register_for_test("ambient-origin", "127.0.0.1", 9, "token", "cid");
    plx_plex::plex::register_for_test("ambient-other", "127.0.0.1", 9, "token", "cid");
    let client = plx_plex::plex::client_for(origin).unwrap();
    let keys = roster(&[(61, Some(key(1)), "")], 3, 4);
    let result = fan_out(client, KeyedClients::new(&keys, forbidden), &keys,
        ServerId::from_raw(61), "4", "", Write::Watched);
    assert!(result.is_empty());
    assert_eq!(plx_plex::plex::server_ids().count(), 2, "scripted observations never rewrite ambient state");
    plx_plex::plex::reset_servers_for_test();
}
