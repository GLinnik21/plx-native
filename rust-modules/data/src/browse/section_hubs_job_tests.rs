//! The section hubs' reads leave the store as store jobs (`StoreJob::BrowseHubs`) and start no
//! worker of the store's own; the identity compare of a landing is a compare of keys.

use super::*;
use crate::browse::test_support::*;
use std::sync::atomic::Ordering;

/// A section-hubs step starts no worker. The loopback server refuses a connection at once, so a
/// worker the step started would have posted its (failed) answer by the time the pool has drained.
#[test]
fn a_section_hubs_step_starts_no_worker_of_its_own() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = loopback_page_source();
    browse.state.hubs_kick(0, &browse.adapter);
    assert!(browse.adapter.hubs.fetching.load(Ordering::SeqCst), "the step claimed the one shared fetch");
    plx_base::task::drain_workers_for_test();
    assert!(browse.adapter.hubs.result.lock().unwrap_or_else(|e| e.into_inner()).is_none(),
        "no worker of the store's own answered: the read is the application's to start");
}

/// An answer that arrives for a client the section's server no longer holds is not a landing: the
/// shelves it carried are dropped and the section is still owed the read it asked for.
#[test]
fn a_landing_whose_client_moved_is_not_ours_and_the_section_is_still_owed() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = loopback_page_source();
    browse.state.hubs_kick(0, &browse.adapter);
    plx_base::task::drain_workers_for_test();
    assert!(!browse.state.states()[0].hubs.owed, "premise: the claim took the section's owed read");
    let asked_with = client.token_gen();
    client.set_token("new");
    assert_ne!(client.token_gen(), asked_with, "premise: the credential moved while the read was out");
    let shelves = vec![Shelf::default()];
    *browse.adapter.hubs.result.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(HubResult::list_for_test(browse.state.table_epoch(), 0, client, asked_with, shelves));
    assert!(!browse.state.hubs_land(&browse.adapter, plx_machine::landgate::fixture_gate()), "nothing landed");
    assert!(browse.state.states()[0].hubs.staged.is_none(), "the shelves of the old client were dropped");
    assert!(browse.state.states()[0].hubs.owed, "the section is still owed the read");
    assert!(!browse.adapter.hubs.fetching.load(Ordering::SeqCst), "the take gave the claim back");
}
