//! The discovery job's request, its port and its body, and the store's side of its refusal: a step
//! with no key asks for nothing, the descriptor a controlled run records is the literal the replay
//! fixtures hold, an answer never lowers the claim (the apply does), a refusal never reaches the
//! mailbox, a read that panics lands as the failure of the job it was doing, and a refusal the
//! store settled across a `Reset` backs nothing off.

use super::*;
use crate::browse::test_support::*;
use crate::browse::{discovery_answer_waiting_for_test, discovery_owed, SecKind};
use crate::stores::StoreJob;
use plx_machine::job::Port;
use plx_plex::plex::ClientKeys;

/// The one discovery job the steps have left, taken off the outbox.
#[track_caller]
fn the_discovery_job(browse: &mut TestBrowse) -> (plx_machine::job::Job<BrowseDiscoveryReq>, BrowseDiscoveryPort) {
    let mut jobs = browse.take_discovery_jobs();
    assert_eq!(jobs.len(), 1, "exactly one discovery job is on the outbox");
    assert!(browse.jobs.is_empty(), "nothing else is on the outbox: {:?}", browse.jobs);
    jobs.remove(0)
}

/// A server whose slot holds no client in the keys the step was handed is not asked: no job, and no
/// claim raised, which is what reading `client_for(sid)` and finding none did.
#[test]
fn a_step_whose_keys_hold_no_client_for_the_server_asks_for_nothing() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_source();
    browse.state.maybe_discover_owned(&browse.adapter, &ClientKeys::default(), &mut browse.jobs);
    assert!(browse.jobs.is_empty(), "no job: {:?}", browse.jobs);
    assert!(!discovery_owed(&browse.adapter), "no claim");
    assert!(browse.state.src_flight.is_none(), "nothing is in flight");
}

/// A step with a key leaves exactly one job, raises the claim for it, and starts nothing.
#[test]
fn a_step_leaves_one_job_and_raises_the_claim() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (job, _port) = the_discovery_job(&mut browse);
    assert_eq!(job.req.server(), (sid, plx_plex::plex::ClientKey::of(client)));
    assert!(discovery_owed(&browse.adapter), "the claim is raised by the step");
    // a second step before the drain emits no duplicate
    browse.maybe_discover();
    assert!(browse.jobs.is_empty(), "the claim holds the second step off");
}

/// The descriptor a controlled run records is the literal the replay fixtures hold: the same keys
/// in the same order, the values the step's own inputs say, and no field more.
#[test]
fn the_admission_is_the_recorded_literal() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.state.source_mut(0).unwrap().name = String::new();
    browse.maybe_discover();
    let (job, _port) = the_discovery_job(&mut browse);
    let epoch = browse.state.table_epoch();
    assert_eq!(job.req.admission().to_string(), serde_json::json!({
        "epoch": epoch, "source": 0, "sid": sid.raw(), "client": client.instance_gen(),
        "token_gen": client.token_gen(), "name": true, "sections": true, "counts": [],
    }).to_string(), "byte for byte, key order included");

    // the counts request: the keys of the landed sections, and no name to ask for once it is known
    browse.state.source_mut(0).unwrap().name = "home".into();
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie), (2, "Shows".into(), SecKind::Show)]);
    let source = browse.state.source_mut(0).unwrap();
    source.sections_done = true;
    source.counts_done = false;
    browse.adapter.src_fetching.clear();
    browse.maybe_discover();
    let (job, _port) = the_discovery_job(&mut browse);
    assert_eq!(job.req.admission().to_string(), serde_json::json!({
        "epoch": epoch, "source": 0, "sid": sid.raw(), "client": client.instance_gen(),
        "token_gen": client.token_gen(), "name": false, "sections": false, "counts": [1, 2],
    }).to_string());
}

/// The body's answer lands in the mailbox and does NOT lower the claim: the apply does, after its
/// epoch check, so a take that finds the answer there still owes the release.
#[test]
fn an_answer_waits_in_the_mailbox_and_the_apply_lowers_the_claim() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (job, port) = the_discovery_job(&mut browse);

    finish_discovery_with(&port, job.addr, client, job.req, |_, _, _| {
        (String::new(), SrcWhat::Sections(Some(vec![(1, "Movies".into(), SecKind::Movie)])))
    });

    assert!(discovery_answer_waiting_for_test(&browse.adapter), "the answer is in the mailbox");
    assert!(discovery_owed(&browse.adapter), "…and the claim is still up: the apply lowers it");
    let outcome = browse.discover_pump();
    assert!(outcome.changed, "the landing applied the sections");
    assert_eq!(browse.section_count(), 1);
    assert!(!discovery_owed(&browse.adapter), "the apply released the claim");
}

/// A refusal lowers the claim in its own call and never touches the mailbox, so it cannot be
/// recorded as a result.
#[test]
fn a_refusal_lowers_the_claim_and_never_reaches_the_mailbox() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (job, port) = the_discovery_job(&mut browse);

    port.refused(job.addr);

    assert!(!discovery_owed(&browse.adapter), "the claim is down in the refusing call");
    assert!(!discovery_answer_waiting_for_test(&browse.adapter), "the mailbox was never written");
    assert!(crate::browse::record::take_from(&browse.adapter).is_none(), "nothing a run could record");
}

/// A refusal of a request the store has since superseded finds the claim is a newer request's (or
/// down) and does nothing: it cannot lower the claim of the request that is current.
#[test]
fn a_late_refusal_of_a_superseded_request_leaves_the_current_claim_alone() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (old, old_port) = the_discovery_job(&mut browse);
    // the claim is released without a refusal (an answer applied elsewhere), and the next step asks again
    browse.adapter.src_fetching.release();
    browse.maybe_discover();
    let (_current, _port) = the_discovery_job(&mut browse);
    assert!(discovery_owed(&browse.adapter));

    old_port.refused(old.addr);

    assert!(discovery_owed(&browse.adapter), "the older request's refusal leaves the newer claim up");
}

/// The settle runs on the step after the refusal: the source backs off, nothing else about it
/// moves, and a refusal across a `Reset` backs nothing off (the flight and the refusal are gone).
#[test]
fn a_refusal_across_a_reset_backs_nothing_off() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (job, port) = the_discovery_job(&mut browse);

    browse.reset();
    port.refused(job.addr);

    assert!(!discovery_owed(&browse.adapter), "the reset cleared the claim");
    assert!(browse.state.src_flight.is_none(), "…and the flight");
    browse.sync_roster();
    browse.maybe_discover();
    assert!(browse.state.sources().iter().all(|source| source.retry_cd == 0), "no backoff was armed");
}

/// A refusal the store settles after the table was reset under it (the epoch moved) backs off no
/// source: the source index it names belongs to a table that is gone.
#[test]
fn a_settled_refusal_of_an_older_epoch_backs_nothing_off() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (job, port) = the_discovery_job(&mut browse);
    port.refused(job.addr);
    browse.state.epoch = browse.state.epoch.wrapping_add(1);

    browse.maybe_discover();

    assert_eq!(browse.state.sources()[0].retry_cd, 0, "the refusal was for another table");
}

/// A Sections body that panics lands as the FAILURE of its job (`Sections(None)`), never as a
/// section list that is empty: the source is marked unreachable and its rows are left where they
/// were. The claim is lowered by the landing's apply.
#[test]
fn a_panicking_sections_read_lands_as_a_failed_section_list() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let (job, port) = the_discovery_job(&mut browse);

    finish_discovery_with(&port, job.addr, client, job.req, |_, _, _| panic!("the body panicked"));

    let landed = crate::browse::record::take_from(&browse.adapter).expect("the answer was posted, outside the guard");
    match crate::browse::record::encode(&landed)["what"].clone() {
        what if what == serde_json::json!({"Sections": null}) => {}
        other => panic!("a failed section list, not {other}"),
    }
}

/// A Counts body that panics lands as an EMPTY count list (a failure the landing does not latch:
/// the probe stays armed), never as a failed section list.
#[test]
fn a_panicking_counts_read_lands_as_an_empty_count_list() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_source();
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    let source = browse.state.source_mut(0).unwrap();
    source.sections_done = true;
    source.counts_done = false;
    browse.maybe_discover();
    let (job, port) = the_discovery_job(&mut browse);
    assert_eq!(job.req.admission()["counts"], serde_json::json!([1]), "premise: a counts job");

    finish_discovery_with(&port, job.addr, client, job.req, |_, _, _| panic!("the body panicked"));

    assert!(discovery_answer_waiting_for_test(&browse.adapter), "the answer was posted, outside the guard");
    let _ = browse.discover_pump();
    assert!(!browse.state.sources()[0].counts_done, "an empty answer leaves the probe armed");
    assert!(!discovery_owed(&browse.adapter), "the apply released the claim");
    assert!(browse.state.sources()[0].sections_done, "the source's section list is not blamed");
}

/// The outbox holds only the discovery kind from this step.
#[test]
fn the_job_is_a_browse_store_job_admitted_home() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_source();
    browse.state.source_mut(0).unwrap().counts_done = true;
    browse.maybe_discover();
    let job = browse.jobs.remove(0);
    assert!(matches!(job, StoreJob::BrowseDiscovery(..)));
    assert_eq!(job.store(), crate::stores::StoreId::Browse);
    assert_eq!(job.admission(), crate::stores::Admission::Home);
}
