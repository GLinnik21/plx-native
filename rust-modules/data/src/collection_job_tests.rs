//! A collection request is a typed job the store emits, not a thread it starts
//! (`docs/sans-io.md`), and which client a server holds is an input to the step, not a read of
//! the registry: what a pump emits and when, what the job says, how its answer or refusal comes
//! back, and what a different answer from the registry does to the page.

use super::*;
use crate::stores::collection::{CollectionCmd, CollectionStore};
use crate::stores::StoreJob;
use plx_machine::landgate::fixture_gate;

const KEY: ClientKey = ClientKey { instance: 7, token: 3 };

fn sid() -> ServerId { ServerId::from_raw(3) }

/// The registry's answer when the server holds the client `KEY` names.
fn holding() -> ClientKeys { ClientKeys::from_keys([(sid(), KEY)]) }

fn target(rk: &str, tag: i64, name: &str, want: usize) -> CollectionTarget {
    CollectionTarget { id: CollectionRef { sid: sid(), rk: rk.into(), sec: 1, tag, name: name.into() }, want }
}

fn open(store: &mut CollectionStore, rk: &str, keys: &ClientKeys) {
    assert!(store.run(CollectionCmd::Open { target: target(rk, 7, "Set", PAGE_SIZE) }, keys));
}

/// The job the store emits for `work` at `generation`, as the step builds it.
fn job_of(store: &CollectionStore, generation: u32, work: Work) -> StoreJob {
    StoreJob::Collection(
        Job { addr: collection_addr(generation), req: CollectionReq { sid: sid(), key: KEY, work } },
        store.collection_port(),
    )
}

fn header_work(rk: &str) -> Work { Work::Header { rk: rk.into() } }

fn children_work(rk: &str, start: usize) -> Work { Work::Children { rk: rk.into(), start } }

/// The one request the store has emitted, taken apart.
fn one(store: &mut CollectionStore) -> (Job<CollectionReq>, CollectionPort) {
    let mut jobs = store.take_jobs();
    assert_eq!(jobs.len(), 1, "one request is out at a time");
    let StoreJob::Collection(job, port) = jobs.remove(0) else { panic!("not a collection job") };
    (job, port)
}

/// Answer a job the way its worker would.
fn answer(job: StoreJob, what: Landing) {
    let StoreJob::Collection(job, port) = job else { panic!("not a collection job") };
    port.done(job.addr, CollectionAnswer(what));
}

fn members(n: usize) -> Vec<PmsMovie> {
    (0..n).map(|i| PmsMovie { sid: sid(), rk: format!("m{i}"), ..Default::default() }).collect()
}

/// A pump over an opened page emits the next request and says which client it decided on; a
/// command emits nothing; and one request is out at a time, so a second pump emits nothing until
/// the first is answered.
#[test]
fn an_open_page_emits_one_request_naming_the_client_it_decided_on() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    assert!(!store.has_jobs());

    open(&mut store, "50001", &holding());
    assert!(!store.has_jobs(), "a command emits no job: the request is the pump's");

    store.pump(fixture_gate(), &holding());
    assert!(store.has_jobs());
    plx_machine::assert_fx!(store.take_jobs(), [job_of(&store, 1, header_work("50001"))]);
    assert!(!store.has_jobs(), "taking the jobs empties the outbox");
    assert!(store.adapter_for_test().busy_for_test(), "the request holds the claim while it is out");

    store.pump(fixture_gate(), &holding());
    plx_machine::assert_fx!(store.take_jobs(), []);
}

/// The request is a pure function of the page, the window and the registry's answer: a tag route
/// resolves first, then the header, then pages in order, each as the answer to the one before.
#[test]
fn each_answer_makes_the_next_pump_emit_the_next_request() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    assert!(store.run(CollectionCmd::Open { target: target("", 7, "Set", PAGE_SIZE) }, &holding()));

    store.pump(fixture_gate(), &holding());
    plx_machine::assert_fx!(store.take_jobs(), [job_of(&store, 1,
        Work::Resolve { sec: 1, tag: 7, name: "Set".into() })]);
    // the resolution's row IS the header: no second request for it
    store.adapter_for_test().land_resolved_for_test(1, "50007");
    store.pump(fixture_gate(), &holding());
    plx_machine::assert_fx!(store.take_jobs(), [job_of(&store, 1, children_work("50007", 0))]);

    store.adapter_for_test().land(1, Landing::Page {
        start: 0, got: PAGE_SIZE, pages: vec![members(PAGE_SIZE)], total: 3 * PAGE_SIZE });
    store.pump(fixture_gate(), &holding());
    // the window is full: nothing more is asked for
    plx_machine::assert_fx!(store.take_jobs(), []);

    assert!(store.run(CollectionCmd::Open { target: target("50007", 7, "Set", 2 * PAGE_SIZE) }, &holding()));
    store.pump(fixture_gate(), &holding());
    plx_machine::assert_fx!(store.take_jobs(), [job_of(&store, 1, children_work("50007", PAGE_SIZE))]);
}

/// A server that holds no client has nothing to ask: no job, the page reads Failed when it shows
/// nothing, and the next attempt is held off for a stretch of frames.
#[test]
fn a_server_without_a_client_emits_nothing_reads_failed_and_holds_off() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    let none = ClientKeys::default();
    open(&mut store, "50001", &none);

    store.pump(fixture_gate(), &none);
    plx_machine::assert_fx!(store.take_jobs(), []);
    assert_eq!(store.view().current().expect("open").status, CollectionStatus::Failed);
    assert!(!store.adapter_for_test().busy_for_test(), "no claim was made");

    // Still no client. For RETRY_FRAMES - 1 pumps the step does not look for one again, so it does
    // not mark the page Failed again, and the view's revision (what the screen repaints on) stays
    // where the first pump left it. Without the hold-off every frame re-enters the no-client
    // branch and moves it.
    let held_at = store.view().revision();
    for _ in 1..RETRY_FRAMES {
        store.pump(fixture_gate(), &none);
        plx_machine::assert_fx!(store.take_jobs(), []);
        assert_eq!(store.view().revision(), held_at, "held off: the page is not marked Failed again");
    }
    // The hold-off ends: the ask is made again, finds no client again, and holds off again.
    store.pump(fixture_gate(), &none);
    plx_machine::assert_fx!(store.take_jobs(), []);
    assert_ne!(store.view().revision(), held_at, "the hold-off ended and the step asked again");

    // The server comes up: the listing was read through no client, so it is rebuilt against the
    // one the registry now holds and asks at once, as a new listing.
    store.pump(fixture_gate(), &holding());
    plx_machine::assert_fx!(store.take_jobs(), [job_of(&store, 2, header_work("50001"))]);
}

/// A listing opened against one client is rebuilt when the registry answers with another (a
/// retoken, a re-registration): the step compares the answer it was handed with the one the
/// listing was read through, and reads nothing from the registry itself.
#[test]
fn a_different_client_answer_rebuilds_the_page_and_asks_again() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    open(&mut store, "50001", &holding());
    store.pump(fixture_gate(), &holding());
    let _ = one(&mut store);
    let generation = store.generation_for_test();

    let retoken = ClientKeys::from_keys([(sid(), ClientKey { token: KEY.token + 1, ..KEY })]);
    assert!(store.pump(fixture_gate(), &retoken), "a listing read through another credential is rebuilt");
    assert_ne!(store.generation_for_test(), generation, "…as a new listing");
    let (job, _) = one(&mut store);
    assert_eq!(job.req.server(), (sid(), ClientKey { token: KEY.token + 1, ..KEY }),
        "the new request names the client the registry now holds");

    // The same answer again changes nothing.
    assert!(!store.pump(fixture_gate(), &retoken));
}

/// A refusal hands the request's claim back, so the next pump asks again, once. The runner
/// refuses a job whose admission the tape declined or whose client the registry no longer holds.
#[test]
fn a_refused_request_releases_its_claim_and_is_asked_again() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    open(&mut store, "50001", &holding());
    store.pump(fixture_gate(), &holding());
    let (job, port) = one(&mut store);
    assert!(store.adapter_for_test().busy_for_test());

    port.refused(job.addr);
    assert!(!store.adapter_for_test().busy_for_test(), "the refusal released the claim");
    assert!(!store.adapter_for_test().has_mail_for_test(), "a refusal answers nothing");

    store.pump(fixture_gate(), &holding());
    plx_machine::assert_fx!(store.take_jobs(), [job_of(&store, 1, header_work("50001"))]);
}

/// A refusal that arrives after a newer request has claimed leaves the newer claim alone: the
/// runner refuses a job on its own thread, and by then the page may have moved on.
#[test]
fn a_refusal_for_a_superseded_request_leaves_the_newer_claim() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    open(&mut store, "50001", &holding());
    store.pump(fixture_gate(), &holding());
    let (old, old_port) = one(&mut store);

    open(&mut store, "50002", &holding()); // another collection supersedes the first
    store.pump(fixture_gate(), &holding());
    let _ = one(&mut store); // the new listing asked for its header
    assert_ne!(store.generation_for_test(), old.addr.req.0);

    old_port.refused(old.addr);
    assert!(store.adapter_for_test().busy_for_test(),
        "the old request's refusal did not release the claim the new one holds");
}

/// An answer is stamped with its request's generation, so the pump applies the current listing's
/// answer and discards one for a listing it has since superseded.
#[test]
fn an_answer_applies_to_its_own_listing_and_a_superseded_one_is_dropped() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    open(&mut store, "50001", &holding());
    store.pump(fixture_gate(), &holding());
    let stale = store.take_jobs().remove(0);

    open(&mut store, "50002", &holding());
    store.pump(fixture_gate(), &holding());
    let current = store.take_jobs().remove(0);

    answer(stale, Landing::Header { rk: None, head: header("Stale", 5) });
    store.pump(fixture_gate(), &holding());
    assert!(!store.view().current().unwrap().header_ready, "a superseded listing's answer lands nowhere");

    answer(current, Landing::Header { rk: None, head: header("Fresh", 5) });
    assert!(store.pump(fixture_gate(), &holding()));
    let c = store.view().current().unwrap();
    assert!(c.header_ready);
    assert_eq!(c.title, "Fresh");
}

/// A job's port is captured when the job is emitted. A `Reset` between the request and the
/// runner rotates the store's adapter, and the job still answers into the retired one, where
/// nothing is pumped, never into the replacement's landing.
#[test]
fn a_job_emitted_before_a_reset_answers_into_the_retired_adapter() {
    let _serial = plx_base::testlock::serial();
    let mut store = CollectionStore::default();
    open(&mut store, "50001", &holding());
    store.pump(fixture_gate(), &holding());
    let (job, port) = one(&mut store);
    assert_eq!(port, store.collection_port());

    store.run(CollectionCmd::Reset, &holding());
    assert_ne!(port, store.collection_port(), "the job holds the retired adapter's port");
    open(&mut store, "50001", &holding());

    port.done(job.addr, CollectionAnswer(Landing::Header { rk: None, head: header("Old", 1) }));
    assert!(!store.adapter_for_test().has_mail_for_test(), "the replacement's landing never sees the answer");
}

/// The admission record is a pure function of the job: the bytes a replay compares are the
/// request's store, slot, generation, server, client instance and work, by name. The shape is
/// pinned here as literals; the application's Bridge test pins the same bytes from outside.
#[test]
fn the_admission_record_names_the_request_in_the_recorded_shape() {
    let req = |work| CollectionReq { sid: sid(), key: KEY, work };
    let addr = collection_addr(4);
    let sid = sid().raw();
    assert_eq!(req(header_work("50001")).admission(addr), serde_json::json!({
        "store":"collection","slot":0,"gen":4,"sid":sid,"client":7,"job":{"Header":{"rk":"50001"}}}));
    assert_eq!(req(children_work("50001", 60)).admission(addr), serde_json::json!({
        "store":"collection","slot":0,"gen":4,"sid":sid,"client":7,"job":{"Children":{"rk":"50001","start":60}}}));
    assert_eq!(req(Work::Resolve { sec: 4, tag: 77, name: "Set".into() }).admission(addr), serde_json::json!({
        "store":"collection","slot":0,"gen":4,"sid":sid,"client":7,
        "job":{"Resolve":{"sec":4,"tag":77,"name":"Set"}}}));
}

/// A members page read while a Mark Watched was pressed lands with the member as the server
/// described it before the press; the ledger makes the press again on it, and only the ones
/// published after the read's own request. The stamp is taken by the REAL request (`maybe_spawn`,
/// the job it emits), not assigned.
#[test]
fn a_members_page_in_flight_across_a_watched_edit_lands_watched() {
    use crate::viewstate::{ViewAt, ViewEdit};
    let _serial = plx_base::testlock::serial();
    let adapter = Arc::new(CollectionAdapter::default());
    let mut state = CollectionState::default();
    let keys = holding();
    state.run(&adapter, CollectionCmd::Open { target: target("50003", 9, "Set", PAGE_SIZE) }, &keys);
    let generation = state.generation();
    // the header request is emitted and answered with a canned header
    let _ = state.pump_with_gate(&adapter, fixture_gate(), &keys);
    assert_eq!(state.jobs.len(), 1);
    state.jobs.clear();
    adapter.fetch.release();
    adapter.land(generation, Landing::Header { rk: None, head: header("Set", 3) });
    assert!(state.take_landing_for_test(&adapter));

    let older = state.ledger.record(&ViewAt::leaf(sid(), "m0"), ViewEdit::Watched(true));
    state.ledger.landed(older); // before the page request: the server has it
    state.maybe_spawn(&adapter, &keys); // the members page request is emitted
    assert_eq!(state.jobs.len(), 1);
    state.ledger.record(&ViewAt::leaf(sid(), "m1"), ViewEdit::Watched(true)); // …then the press
    let member = |rk: &str| PmsMovie { sid: sid(), rk: rk.into(), unwatched: true, resume_ms: 7, ..Default::default() };
    let _ = adapter.fetch.take();
    adapter.land(generation, Landing::Page { start: 0, got: 3, pages: vec![vec![member("m0"), member("m1"), member("m2")]], total: 3 });
    assert!(state.take_landing_for_test(&adapter));
    let c = state.current.as_ref().unwrap();
    let marks: Vec<_> = (0..c.shown()).filter_map(|i| c.pages.get(i)).map(|m| (m.rk.clone(), m.watched, m.resume_ms)).collect();
    assert_eq!(marks, vec![("m0".to_string(), false, 7), ("m1".to_string(), true, 0), ("m2".to_string(), false, 7)]);
}
