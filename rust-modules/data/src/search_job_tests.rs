//! The job lane of the Search store (`search.rs`'s [`SearchReq`] / [`SearchPort`]): what a pump
//! emits and when, what the job says, how its answer or refusal comes back, and what a refusal does
//! to the pump that sees it. The runner side (a launch, a refused launch, a parked run) is pinned
//! where the runner lives, `app/adapters/stores.rs`.

use super::tail_tests::Fake;
use super::test_support::*;
use super::*;
use crate::stores::search::{SearchCmd, SearchStore};
use crate::stores::StoreJob;
use plx_machine::job::Port;

/// Io whose every call panics: the transport blew up under the worker.
struct Panics;

impl tail::Io for Panics {
    fn listing(&mut self, _: plx_plex::plex::SearchKind, _: plx_plex::plex::PageReq)
        -> Option<plx_plex::plex::MediaContainer> {
        panic!("the transport blew up")
    }
    fn hubs(&mut self, _: usize) -> Option<Projection> {
        panic!("the transport blew up")
    }
}

/// A settled query over `n` registered servers, nothing asked yet.
fn settled(n: usize) -> (Fresh, Owner) {
    let fresh = fresh();
    let mut owner = Owner::default();
    register(&mut owner, n);
    owner.set_query("wallace");
    (fresh, owner)
}

/// The search jobs the last step left on the outbox, in order.
fn taken(owner: &mut Owner) -> Vec<(Job<SearchReq>, SearchPort)> {
    std::mem::take(&mut owner.state.jobs).into_iter().map(|job| match job {
        StoreJob::Search(job, port) => (job, port),
        _ => panic!("not a search job"),
    }).collect()
}

fn claim_held(owner: &Owner, i: usize) -> bool {
    owner.adapter.mailbox(i).busy()
}

/// A settled query emits ONE request per source and starts nothing: the claim is raised, the job
/// names the client the step decided on and the generation it was asked under, and the pump after
/// it asks nobody again while the requests are out.
#[test]
fn a_settled_query_emits_one_request_per_slot_naming_the_client_it_decided_on() {
    let (_fresh, mut owner) = settled(2);
    assert!(owner.state.jobs.is_empty(), "typing alone emits nothing");

    owner.pump(0.3);
    let keys = crate::pms::registry_keys_for_test();
    let jobs = taken(&mut owner);
    assert_eq!(jobs.len(), 2, "one request per live slot");
    for ((job, port), i) in jobs.iter().zip(slots()) {
        let sid = ServerId::from_raw(i as u16);
        assert_eq!(job.req.server(), (sid, keys.key(sid).expect("registered")));
        assert_eq!(job.addr, search_addr(owner.state.gen), "addressed by the query's generation");
        assert_eq!(*port, SearchPort::to(&owner.adapter, i), "answering into its own slot");
        assert!(matches!(job.req.work, SearchWork::First), "the first answer is the whole preview");
        assert!(claim_held(&owner, i), "the claim is raised at the emit");
    }

    owner.pump(0.016);
    assert!(owner.state.jobs.is_empty(), "requests out are not asked again");
}

/// A server that holds no client has nothing to ask: no job, and the slot fails the way a read of
/// the missing client always did, so the status and the backoff are the failure's.
#[test]
fn a_slot_without_a_client_emits_nothing_and_lands_as_a_failure() {
    let (_fresh, mut owner) = settled(1);
    let none = ClientKeys::default();

    pump_with_optional_directory(&mut owner.state, &owner.adapter, 0.3, None,
        plx_machine::landgate::fixture_gate(), &none);
    assert!(owner.state.jobs.is_empty(), "no client, no request");
    assert!(owner.adapter.mailbox(0).has_mail(), "the failure is posted through the mailbox");

    owner.pump(0.016);
    assert_eq!(source(&owner.state, 0).status, Status::Failed);
    assert_eq!(owner.state.state(), State::Failed);
    assert!(!claim_held(&owner, 0), "the take of the failure released the claim");
    assert!(owner.state.jobs.is_empty(), "and it is not asked again until the backoff has run");
}

/// A refusal hands the source's claim back in the call that refuses it. The pump after it reads the
/// refusal BEFORE it decides to ask and backs off: with only the release it would find the claim
/// free and emit the same request again, every pump.
#[test]
fn a_refused_request_is_asked_again_only_after_the_backoff() {
    let (_fresh, mut owner) = settled(1);
    owner.pump(0.3);
    let (job, port) = taken(&mut owner).remove(0);
    assert!(claim_held(&owner, 0));

    port.refused(job.addr);
    assert!(!claim_held(&owner, 0), "the refusal released the claim in the same call");
    assert!(!owner.adapter.mailbox(0).has_mail(), "a refusal answers nothing");

    // The refusal is seen one pump after it is made, and holds the full wait.
    owner.pump(0.016);
    assert!(owner.state.jobs.is_empty());
    assert_eq!(source(&owner.state, 0).retry_cd, RETRY_FRAMES, "the pump that saw it backed off");
    for pumps in 1..RETRY_FRAMES {
        owner.pump(0.016);
        assert!(owner.state.jobs.is_empty(), "still backed off after {pumps} more pumps");
    }
    owner.pump(0.016);
    let (again, _) = taken(&mut owner).remove(0);
    assert_eq!(again, job, "the same request is asked again once the wait is over");
    assert!(claim_held(&owner, 0), "...and it claims again");
}

/// A refusal is observed once: a request refused, asked again and answered is not backed off a
/// second time by the memory of the first.
#[test]
fn a_refusal_backs_off_one_request_and_not_the_one_after() {
    let (_fresh, mut owner) = settled(1);
    owner.pump(0.3);
    let (job, port) = taken(&mut owner).remove(0);
    port.refused(job.addr);
    for _ in 0..=RETRY_FRAMES { owner.pump(0.016); }
    let (second, port) = taken(&mut owner).remove(0);

    let mut fake = Fake::new().movies(1);
    finish_with_io(&port, second.addr, &mut fake, second.req);
    owner.pump(0.016);
    assert_eq!(source(&owner.state, 0).status, Status::Answered);
    assert_eq!(source(&owner.state, 0).retry_cd, 0, "the answered request left no backoff behind");
}

/// A refusal that arrives after the query has moved on leaves the newer claim alone, and the newer
/// request is not backed off for it: the runner refuses a job on its own schedule, and by then the
/// viewer may have typed something else.
#[test]
fn a_refusal_for_a_superseded_request_leaves_the_newer_one_alone() {
    let (_fresh, mut owner) = settled(1);
    owner.pump(0.3);
    let (old, old_port) = taken(&mut owner).remove(0);

    owner.set_query("wallace shawn"); // supersedes the first query, clearing its claim
    owner.pump(0.3);
    let (current, _) = taken(&mut owner).remove(0);
    assert_ne!(old.addr, current.addr);
    assert!(claim_held(&owner, 0));

    old_port.refused(old.addr);
    assert!(claim_held(&owner, 0), "the old request's refusal did not release the claim the new one holds");
    owner.pump(0.016);
    assert_eq!(source(&owner.state, 0).retry_cd, 0, "and the new request was not backed off for it");
}

/// A job's port is captured when the job is emitted. A `Reset` between the request and the runner
/// rotates the store's adapter, and the job still answers into the retired one, where nothing is
/// pumped, never into the replacement's mailbox.
#[test]
fn a_job_emitted_before_a_reset_answers_into_the_retired_adapter() {
    let _fresh = fresh();
    register(&mut Owner::default(), 1);
    let mut store = SearchStore::default();
    store.pump(0.0); // the first pump observes the roster
    assert!(store.run(SearchCmd::SetQuery("wallace".into())));
    assert!(!store.has_jobs(), "a command emits no request");
    store.pump(0.3);
    let mut jobs = store.take_jobs();
    assert!(!store.has_jobs(), "taking the jobs empties the outbox");
    assert_eq!(jobs.len(), 1);
    let StoreJob::Search(job, port) = jobs.remove(0) else { panic!("not a search job") };
    let retired = store.adapter_for_test();
    assert_eq!(port, SearchPort::to(&retired, 0));

    store.run(SearchCmd::Reset);
    let replacement = store.adapter_for_test();
    assert!(!Arc::ptr_eq(&retired, &replacement), "reset rotated the adapter");
    assert_ne!(port, SearchPort::to(&replacement, 0), "the job holds the retired adapter's port");
    let mut fake = Fake::new().movies(1);
    finish_with_io(&port, job.addr, &mut fake, job.req);
    assert!(retired.mailbox(0).has_mail(), "the answer reached the adapter the request was made from");
    assert!(!replacement.mailbox(0).has_mail(), "and never the replacement's");
}

/// A fetch whose IO panics still answers, as a FAILURE: not as "this server has nothing" and not
/// as silence, so the take of it releases the claim and the source backs off instead of staying
/// out for ever. (The guard is the one the worker body always had; this pins that it moved with it.)
#[test]
fn a_panicking_fetch_lands_as_a_failure_and_releases_its_claim() {
    let (_fresh, mut owner) = settled(1);
    owner.pump(0.3);
    let (job, port) = taken(&mut owner).remove(0);

    finish_with_io(&port, job.addr, &mut Panics, job.req);

    assert!(owner.adapter.mailbox(0).has_mail(), "the panicked fetch still answered");
    owner.pump(0.016);
    assert!(!claim_held(&owner, 0), "the take released the claim");
    assert_eq!(source(&owner.state, 0).status, Status::Failed, "a failure, not an empty answer");
    assert_eq!(source(&owner.state, 0).retry_cd, RETRY_FRAMES);
}

/// ViewState's `ApplyView` fan-out steps Search, and ViewState drains only the stores its fill list
/// names (`VIEWSTATE_FILLS`), Search not among them because a view edit emits no job. This pins the
/// premise, so an `ApplyView` that one day emits fails here and not on the late backstop.
#[test]
fn a_view_edit_stepped_through_the_fan_out_emits_no_job() {
    use crate::viewstate::{ViewAt, ViewEdit};
    let (_fresh, mut owner) = settled(1);
    owner.state.run(&owner.adapter, SearchCmd::ApplyView {
        at: ViewAt::leaf(ServerId::from_raw(0), "synthetic-rk"), edit: ViewEdit::Watched(true),
    });
    assert!(owner.state.jobs.is_empty(), "a view edit starts nothing");
}

/// A lane read rides the same lane: an answered source owed a window asks for it as a `Tail` job
/// carrying a COPY of its lane, and the read that panics lands as a failed read, which leaves the
/// slide owed and the claim released.
#[test]
fn a_lane_read_is_a_job_and_a_panicking_one_lands_as_a_failed_read() {
    let (_fresh, mut owner) = settled(1);
    let _fake = owner.serve(Fake::new().movies(300));
    for _ in 0..4 { owner.pump(0.3); }
    assert_eq!(source(&owner.state, 0).status, Status::Answered);
    owner.io = None; // from here a request waits for the test to run it

    let k = KINDS.iter().position(|x| *x == Kind::Movie).unwrap();
    let shelf = owner.state.shelves().iter().find(|s| s.kind == Kind::Movie).expect("the row is there");
    let seen = (shelf.window.start, shelf.items.len());
    owner.state.run(&owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen });
    owner.pump(0.3);
    let (job, port) = taken(&mut owner).remove(0);
    let SearchWork::Tail { k: asked, ref range, .. } = job.req.work else { panic!("an answered source reads a lane") };
    assert_eq!(asked, k, "the movie lane");
    assert!(range.end > range.start);

    finish_with_io(&port, job.addr, &mut Panics, job.req);

    owner.pump(0.3);
    assert!(owner.state.wins[k].pending.is_some(), "the slide stays owed");
    assert_eq!(source(&owner.state, 0).status, Status::Answered, "a failed read is not a failed source");
    assert!(!claim_held(&owner, 0), "the failed read's take released the claim");
}
