//! The section hubs' reads leave the store as store jobs (`StoreJob::BrowseHubs`) and start no
//! worker of the store's own; the identity compare of a landing is a compare of keys, and a read the
//! runner refused is settled by the store's next step to the state a read that never started leaves.

use super::*;
use crate::browse::test_support::*;
use plx_machine::job::Port;
use plx_machine::machine::MachineId;

/// A section-hubs step starts no worker. The loopback server refuses a connection at once, so a
/// worker the step started would have posted its (failed) answer by the time the pool has drained.
/// The read is a job on the outbox, naming the server and the client the step decided on.
#[test]
fn a_section_hubs_step_emits_a_job_and_starts_no_worker_of_its_own() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    browse.hubs_kick(0);
    assert!(browse.adapter.hubs.fetch.busy(), "the step claimed the one shared fetch");
    plx_base::task::drain_workers_for_test();
    assert!(!browse.adapter.hubs.fetch.has_mail(),
        "no worker of the store's own answered: the read is the application's to start");
    let (job, _port) = browse.take_hubs_job();
    assert_eq!(job.addr.to, MachineId::Store(crate::stores::StoreId::Browse.ord()), "addressed to Browse");
    assert_eq!(job.req.server(), (sid, ClientKey::of(client)), "the server and the client the step decided on");
    assert!(!browse.state.states()[0].hubs.owed, "the emitted read took the section's owed read");
}

/// A job whose key moved by the time the runner launched it is refused. Nothing is recorded: the
/// store's next step settles it to the state of a read that never started (the section is owed
/// again, the claim is down), and asks nothing while the registry holds no client for it. The step
/// after, with the client back under a new key, asks again with THAT key.
#[test]
fn a_refused_job_is_settled_by_the_next_step_and_the_one_after_asks_again_with_the_new_key() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    let first = ClientKey::of(client);
    browse.hubs_kick(0);
    let (job, port) = browse.take_hubs_job();
    assert!(!browse.state.states()[0].hubs.owed, "premise: the emitted read took the owed read");
    port.refused(job.addr); // the runner found the registry's client moved at the launch
    assert!(!browse.adapter.hubs.fetch.busy(), "the refusal gave the claim back in the call that refused");
    assert!(!browse.state.states()[0].hubs.owed, "…and nothing is settled until the store's next step");

    // The next step finds the registry holding no client for the server: it settles the refusal and
    // has nothing to ask with.
    let (keys, mut jobs) = (ClientKeys::default(), Vec::new());
    browse.state.hubs_kick(0, &browse.adapter, &keys, &mut jobs);
    assert!(jobs.is_empty(), "no client, no read: {jobs:?}");
    assert!(browse.state.states()[0].hubs.owed,
        "the section is owed its read again, as after a read that never started");
    assert!(!browse.adapter.hubs.fetch.busy(), "no claim is left up without a job in flight");
    assert!(browse.state.hubs_flight.is_none(), "the refusal is consumed");

    // The client is back, under a key that is not the first.
    client.set_token("new");
    assert_ne!(ClientKey::of(client), first, "premise: the key moved");
    browse.hubs_kick(0);
    let (job, _port) = browse.take_hubs_job();
    assert_eq!(job.req.server(), (sid, ClientKey::of(client)), "the read asks with the current key");
    assert!(browse.adapter.hubs.fetch.busy(), "and holds the claim");
}

/// A refused ROW read puts the row back to wanting: its `inflight` is cleared by the settle, so the
/// page the reader asked for is read again by a later step, not lost.
#[test]
fn a_refused_row_read_is_settled_to_a_row_that_is_not_in_flight() {
    const ID: &str = "movie.row0";
    const KEY: &str = "/hubs/sections/1/row0";
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = loopback_page_source();
    seed_named_shelves_for_owner_test(&mut browse.state, 0, &[(ID, KEY, "Row")], 12);
    {
        let st = browse.state.state_mut(0).unwrap();
        let shelf = &mut Arc::make_mut(&mut st.hubs.committed)[0];
        shelf.more = true;
        shelf.total = 500;
    }
    let seen = browse.state.states()[0].hubs.revision;
    assert!(browse.hubs_page(0, seen, ID, KEY, false), "the page ask was queued");
    let (job, port) = browse.take_hubs_job();
    assert!(browse.state.states()[0].hubs.inflight.is_some(), "premise: the row read is in flight");

    port.refused(job.addr);
    let (keys, mut jobs) = (ClientKeys::default(), Vec::new());
    browse.state.hubs_kick(0, &browse.adapter, &keys, &mut jobs);
    assert!(jobs.is_empty(), "no client, no read");
    assert!(browse.state.states()[0].hubs.inflight.is_none(), "the refused row read is no longer in flight");
    assert!(browse.state.states()[0].hubs.ask.is_some(), "the page the reader asked for is still wanted");
}

/// An adapter the store has since replaced (a roster retire) takes the record of the read it was
/// out on with it: the next step drops it and settles nothing, since the tables it described are gone.
#[test]
fn a_read_out_on_a_retired_adapter_is_dropped_by_the_next_step_not_settled() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = loopback_page_source();
    browse.hubs_kick(0);
    let (_job, _port) = browse.take_hubs_job();
    assert!(browse.state.hubs_flight.is_some(), "premise: a read is out");
    browse.adapter = Arc::new(crate::browse::BrowseAdapter::default()); // the claim stays with the retired adapter
    let (keys, mut jobs) = (ClientKeys::default(), Vec::new());
    browse.state.hubs_kick(0, &browse.adapter, &keys, &mut jobs);
    assert!(browse.state.hubs_flight.is_none(), "the orphaned record was dropped");
    assert!(!browse.state.states()[0].hubs.owed,
        "nothing was settled: the old adapter's refusal, if any, is not ours");
}

/// An answer that arrives for a client the section's server no longer holds is not a landing: the
/// shelves it carried are dropped and the section is still owed the read it asked for.
#[test]
fn a_landing_whose_client_moved_is_not_ours_and_the_section_is_still_owed() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = loopback_page_source();
    browse.hubs_kick(0);
    let (job, port) = browse.take_hubs_job();
    assert!(!browse.state.states()[0].hubs.owed, "premise: the claim took the section's owed read");
    let asked_with = ClientKey::of(client);
    client.set_token("new");
    assert_ne!(ClientKey::of(client), asked_with, "premise: the credential moved while the read was out");
    port.done(job.addr, BrowseHubsAnswer::list_for_test(&job, vec![Shelf::default()]));
    assert!(!browse.hubs_land(), "nothing landed");
    assert!(browse.state.states()[0].hubs.staged.is_none(), "the shelves of the old client were dropped");
    assert!(browse.state.states()[0].hubs.owed, "the section is still owed the read");
    assert!(!browse.adapter.hubs.fetch.busy(), "the take gave the claim back");
    assert!(browse.state.hubs_flight.is_none(), "and the record of the read is gone");
}

/// The compare is on the whole key, not on the credential alone: an answer made by another client
/// OBJECT (the slot was re-pointed while the read was out, so the registry's key has a different
/// `instance`) is not ours even when the token generation agrees.
#[test]
fn a_landing_from_another_client_instance_is_not_ours_even_with_the_same_token() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = loopback_page_source();
    browse.hubs_kick(0);
    let (job, port) = browse.take_hubs_job();
    let current = ClientKey::of(client);
    let other = ClientKey { instance: current.instance.wrapping_add(1), token: current.token, grant: current.grant };
    port.done(job.addr, BrowseHubsAnswer::list_for_test(&job, vec![Shelf::default()]).from_client_for_test(other));
    assert!(!browse.hubs_land(), "nothing landed");
    assert!(browse.state.states()[0].hubs.staged.is_none(), "the other client's shelves were dropped");
    assert!(browse.state.states()[0].hubs.owed, "the section is still owed the read");
    assert!(!browse.adapter.hubs.fetch.busy(), "the take gave the claim back");
}

/// A server that holds no client by the time the answer lands (a sign-out retired its slot) has no
/// key to equal the answer's: the shelves are dropped and the section is owed its read again.
#[test]
fn a_landing_for_a_server_that_holds_no_client_any_more_is_not_ours() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    browse.hubs_kick(0);
    let (job, port) = browse.take_hubs_job();
    plx_plex::plex::reset_servers_for_test();
    assert!(plx_plex::plex::client_for(sid).is_none(), "premise: the slot holds no client at the landing");
    port.done(job.addr, BrowseHubsAnswer::list_for_test(&job, vec![Shelf::default()]));
    assert!(!browse.hubs_land(), "nothing landed");
    assert!(browse.state.states()[0].hubs.staged.is_none(), "the signed-out client's shelves were dropped");
    assert!(browse.state.states()[0].hubs.owed, "the section is still owed the read");
    assert!(!browse.adapter.hubs.fetch.busy(), "the take gave the claim back");
}

/// The answer to a read the store has since superseded (here: the mailbox was cleared under it and a
/// newer read took the claim) is dropped and leaves the newer read's claim alone.
#[test]
fn a_superseded_reads_late_answer_leaves_the_newer_claim_up() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = loopback_page_source();
    browse.hubs_kick(0);
    let (old_job, old_port) = browse.take_hubs_job();
    browse.adapter.hubs.fetch.clear(); // the mailbox was cleared under the first read
    browse.state.state_mut(0).unwrap().hubs.owed = true;
    browse.hubs_kick(0);
    let (new_job, _new_port) = browse.take_hubs_job();
    assert_ne!(old_job.addr, new_job.addr, "premise: each read has its own address");
    old_port.done(old_job.addr, BrowseHubsAnswer::list_for_test(&old_job, vec![Shelf::default()]));
    assert!(!browse.hubs_land(), "the superseded read's answer landed nothing");
    assert!(browse.adapter.hubs.fetch.busy(), "the newer read still holds the claim");
    assert!(browse.state.hubs_flight.is_some_and(|out| out.gen == new_job.addr.req.0), "and its record");
}

/// The pump is woken for the one thing nothing else would wake it for: a read that is out with no
/// claim behind it, i.e. one the runner refused (or whose adapter was replaced), whose record the
/// next step settles. While the claim is up the read is merely out, and the pump sleeps.
#[test]
fn a_read_out_with_no_claim_wakes_the_pump_that_settles_it() {
    let _g = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let (mut state, adapter) = (crate::browse::BrowseState::default(), crate::browse::BrowseAdapter::default());
    assert!(!state.pump_needs_work(&adapter), "premise: an empty store has nothing to do");
    state.hubs_flight = Some(HubsFlight { gen: 1, epoch: state.table_epoch(), sec: 0, flight: Flight::List });
    adapter.hubs.fetch.claim(1);
    assert!(!state.pump_needs_work(&adapter), "the read is out and holds its claim: nothing to settle yet");
    adapter.hubs.fetch.refuse_for(1);
    assert!(state.pump_needs_work(&adapter), "refused: the claim is down and the record is left to settle");
    adapter.hubs.fetch.take_refused(1);
    state.hubs_flight = None;
    assert!(!state.pump_needs_work(&adapter), "settled: the pump sleeps again");
}
