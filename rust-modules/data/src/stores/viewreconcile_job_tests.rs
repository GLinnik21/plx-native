//! A read-back batch is a typed job the store emits, not a thread it starts (`docs/sans-io.md`),
//! and which client a server holds is an input to the step, not a read of the registry: what a pump
//! emits and when, what the job says, how its answer or refusal comes back, and what a refusal does
//! to the flight that was waiting for it.

use super::*;
use plx_machine::landgate::Gate;

const KEY: ClientKey = ClientKey { instance: 7, token: 3, grant: 3 };

fn sid(n: u16) -> ServerId { ServerId::from_raw(n) }

/// The registry's answer when each of `sids` holds the client `KEY` names.
fn holding(sids: &[u16]) -> ClientKeys { ClientKeys::from_keys(sids.iter().map(|&n| (sid(n), KEY))) }

fn item(n: u16, rk: &str) -> Key { (sid(n), rk.to_string()) }

fn owe(store: &mut ViewReconcileStore, n: u16, rks: &[&str]) {
    store.enqueue(rks.iter().map(|rk| item(n, rk)));
}

/// One pump on a plain gate with the registry's answer `clients`; whether it changed anything and
/// the rows it published.
fn pump(store: &mut ViewReconcileStore, may_send: bool, clients: &ClientKeys) -> (bool, Vec<String>) {
    let mut published = Vec::new();
    let changed = store.pump_with_gate(&Gate::default(), may_send, clients, &mut |c| published.push(c.rk));
    (changed, published)
}

/// The job the pump emits for the flight `id`, as the step builds it.
fn job(store: &ViewReconcileStore, id: u32, n: u16, keys: &[&str]) -> StoreJob {
    StoreJob::ViewReconcile(
        Job { addr: reconcile_addr(id), req: ReconcileReq {
            sid: sid(n), key: KEY, keys: keys.iter().map(|k| k.to_string()).collect() } },
        ReconcilePort::of(&store.adapter))
}

/// The one read the pump emitted, taken apart.
fn one(store: &mut ViewReconcileStore) -> (Job<ReconcileReq>, ReconcilePort) {
    let mut jobs = store.take_jobs();
    assert_eq!(jobs.len(), 1, "one batch is out for the one server");
    let StoreJob::ViewReconcile(job, port) = jobs.remove(0) else { panic!("not a view-reconcile job") };
    (job, port)
}

fn landings(store: &ViewReconcileStore) -> usize { store.adapter.mail.lock().unwrap().len() }

fn watched_row(n: u16, rk: &str) -> ReadRow {
    ReadRow {
        movie: crate::pms::PmsMovie { sid: sid(n), rk: rk.into(), kind: 1, watched: true, ..Default::default() },
        parent: String::new(),
        grandparent: String::new(),
    }
}

/// A pump sends one batch per server that has queued items and none out, each naming the client the
/// step decided on, in the order the servers were first queued; a second pump sends nothing while
/// those are out; and the step starts nothing itself.
#[test]
fn a_pump_sends_one_read_per_server_naming_the_client_it_decided_on() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1", "2"]);
    owe(&mut store, 1, &["9"]);

    pump(&mut store, true, &holding(&[0, 1]));
    plx_machine::assert_fx!(store.take_jobs(), [job(&store, 1, 0, &["1", "2"]), job(&store, 2, 1, &["9"])]);
    assert_eq!(store.flights_for_test(), 2);
    assert!(store.pending_for_test().is_empty(), "the queued items left with their batches");

    owe(&mut store, 0, &["1"]); // asked for while its read is out: dirty, not a second read
    assert_eq!(store.dirty_for_test(), [item(0, "1")]);
    pump(&mut store, true, &holding(&[0, 1]));
    plx_machine::assert_fx!(store.take_jobs(), []);
}

/// The seasons and shows a leaf's answer named ride in the same batch as the leaves, after them:
/// one request per server however many parents there are.
#[test]
fn parents_ride_in_the_leaves_batch_after_the_leaves() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["3"]);
    store.pending_parents.extend([item(0, "2"), item(0, "1")]);

    pump(&mut store, true, &holding(&[0]));
    plx_machine::assert_fx!(store.take_jobs(), [job(&store, 1, 0, &["3", "2", "1"])]);
}

/// A write queued or out holds every read in the queue: a read's stamp is taken when it is sent.
#[test]
fn a_write_in_flight_sends_nothing() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1"]);

    pump(&mut store, false, &holding(&[0]));
    plx_machine::assert_fx!(store.take_jobs(), []);
    assert_eq!(store.pending_for_test(), [item(0, "1")], "the item waits");

    pump(&mut store, true, &holding(&[0]));
    plx_machine::assert_fx!(store.take_jobs(), [job(&store, 1, 0, &["1"])]);
}

/// A server that holds no client has nowhere to read from: its batch is dropped, no
/// flight is made and no wait begins; another server's batch is still sent.
#[test]
fn a_server_without_a_client_drops_its_batch_and_the_others_are_still_sent() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1"]);
    owe(&mut store, 1, &["9"]);

    pump(&mut store, true, &holding(&[1]));
    plx_machine::assert_fx!(store.take_jobs(), [job(&store, 1, 1, &["9"])]);
    assert_eq!(store.flights_for_test(), 1, "no flight for the dropped batch");
    assert!(store.pending_for_test().is_empty(), "the dropped batch is not kept");
    assert_eq!(store.retry_cd, 0, "dropping a batch starts no wait");
}

/// A refused read answers: the refusal is a landing under the flight's id, the flight stays owed
/// until the pump takes it, and the pump that takes it puts the flight's items back (leaves, then
/// parents) and waits out the full retry. It does not send the same read again on every pump.
#[test]
fn a_refused_flight_posts_not_started_and_the_next_pump_requeues_it() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["3", "4"]);
    store.pending_parents.extend([item(0, "2"), item(0, "1")]);
    pump(&mut store, true, &holding(&[0]));
    let (sent, port) = one(&mut store);
    assert!(store.is_busy(), "the flight is out");

    port.refused(sent.addr);
    assert_eq!(landings(&store), 1, "a refusal is an answer");
    assert_eq!(store.flights_for_test(), 1, "the flight is owed until the pump takes its answer");

    let (changed, published) = pump(&mut store, true, &holding(&[0]));
    assert!(!changed && published.is_empty(), "nothing was read, so nothing changed and nothing is published");
    assert_eq!(store.flights_for_test(), 0, "the refusal settled the flight");
    assert_eq!(store.pending_for_test(), [item(0, "3"), item(0, "4")]);
    assert_eq!(store.pending_parents, [item(0, "2"), item(0, "1")], "the parents go back where they came from");
    plx_machine::assert_fx!(store.take_jobs(), []);
    assert_eq!(store.retry_cd, RETRY_FRAMES - 1, "the pump that took the refusal counted its own frame of the wait");

    // The wait runs its course with the registry unchanged; then the same batch goes out again,
    // as a new flight.
    for _ in 1..RETRY_FRAMES {
        pump(&mut store, true, &holding(&[0]));
        plx_machine::assert_fx!(store.take_jobs(), []);
    }
    pump(&mut store, true, &holding(&[0]));
    plx_machine::assert_fx!(store.take_jobs(), [job(&store, 2, 0, &["3", "4", "2", "1"])]);
}

/// An item asked for again while its read was out is already coming back with that read's items:
/// the refusal drops its dirty mark instead of queueing it a second time.
#[test]
fn an_item_asked_for_again_while_its_read_was_out_is_queued_once_after_a_refusal() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1"]);
    pump(&mut store, true, &holding(&[0]));
    let (sent, port) = one(&mut store);
    owe(&mut store, 0, &["1"]);
    assert_eq!(store.dirty_for_test(), [item(0, "1")]);

    port.refused(sent.addr);
    pump(&mut store, false, &holding(&[0]));

    assert_eq!(store.pending_for_test(), [item(0, "1")], "queued once");
    assert!(store.dirty_for_test().is_empty(), "the dirty mark was spent with the requeue");
}

/// Nothing was asked of the server, so a refusal settles nothing that depends on an answer: the
/// stop candidate survives to be decided by the read that does run, the quiet mark stays, no page is
/// owed and no notice is raised.
#[test]
fn a_refused_flight_settles_nothing_and_a_later_flight_still_decides_the_candidate() {
    let mut store = ViewReconcileStore::default();
    let stopped = item(0, "1");
    store.enqueue_stop(stopped.clone(), false);
    store.enqueue_reread([item(0, "5")]);
    pump(&mut store, true, &holding(&[0]));
    let (sent, port) = one(&mut store);
    let notices = store.gen();

    port.refused(sent.addr);
    pump(&mut store, false, &holding(&[0]));

    assert_eq!(store.candidates_for_test(), [stopped.clone()], "no answer came, so the stop is still to be decided");
    assert!(store.is_quiet_for_test(&item(0, "5")), "its quiet mark is its own until a read of it lands");
    assert!(store.take_landed().is_empty(), "no row was published, so no page is owed");
    assert_eq!(store.gen(), notices, "no notice");

    // The retry goes out as a later flight, and its answer finds the title watched.
    for _ in 0..=RETRY_FRAMES {
        pump(&mut store, true, &holding(&[0]));
    }
    let (retry, retry_port) = one(&mut store);
    assert!(retry.addr.req.0 > sent.addr.req.0);
    retry_port.done(retry.addr, ReconcileAnswer(Some(vec![watched_row(0, "1"), watched_row(0, "5")])));
    pump(&mut store, true, &holding(&[0]));
    assert_eq!(store.take_watched_ends(), [stopped], "the later flight decided it");
    assert!(store.candidates_for_test().is_empty());
}

/// A refusal for a flight the store has since reset is a landing in the retired adapter, where
/// nothing is pumped: it never reaches the replacement's flights.
#[test]
fn a_job_emitted_before_a_reset_answers_into_the_retired_adapter() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1"]);
    pump(&mut store, true, &holding(&[0]));
    let (sent, port) = one(&mut store);
    assert_eq!(port, ReconcilePort::of(&store.adapter));

    store.reset();
    assert_ne!(port, ReconcilePort::of(&store.adapter), "the job holds the retired adapter's port");
    port.refused(sent.addr);

    assert_eq!(landings(&store), 0, "the replacement's mailbox never sees the answer");
    owe(&mut store, 0, &["1"]);
    pump(&mut store, true, &holding(&[0]));
    plx_machine::assert_fx!(store.take_jobs(), [job(&store, 2, 0, &["1"])]);
}

/// A job the step sent and nobody has taken yet survives the reset on the outbox, as the other
/// stores' jobs do: the drain still sees it (a bypass is counted by the backstop, not erased), and
/// it answers into the adapter it was made for.
#[test]
fn a_job_still_on_the_outbox_at_a_reset_is_kept_and_answers_into_the_retired_adapter() {
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1"]);
    pump(&mut store, true, &holding(&[0]));
    assert!(store.has_jobs(), "premise: the read is on the outbox");

    store.reset();
    assert!(store.has_jobs(), "the reset does not drop an unanswered job");
    let (sent, port) = one(&mut store);
    assert_ne!(port, ReconcilePort::of(&store.adapter), "it holds the retired adapter's port");
    port.refused(sent.addr);
    assert_eq!(landings(&store), 0, "the replacement's mailbox never sees the answer");
}

/// **The owed predicate never starves.** In the paced dump the landing is owed while a flight it has
/// not received is out, and holds the picture back; a refused flight still answers, so the take is
/// satisfied by the NotStarted landing and nothing is left starved.
#[test]
fn a_refused_flight_does_not_starve_the_dump_mode_landing() {
    let gate = Gate::default();
    gate.arm_dump_paced(0);
    let mut store = ViewReconcileStore::default();
    owe(&mut store, 0, &["1"]);
    let take = |store: &mut ViewReconcileStore| {
        store.pump_with_gate(&gate, true, &holding(&[0]), &mut |_| {})
    };
    take(&mut store);
    let (sent, port) = one(&mut store);

    take(&mut store);
    assert_eq!(gate.take_starved(), ["viewreconcile"], "an unanswered flight is owed: the take waits for it");

    port.refused(sent.addr);
    take(&mut store);
    assert!(gate.take_starved().is_empty(), "the refusal answered, so nothing is owed any more");
    assert_eq!(store.flights_for_test(), 0);
    assert_eq!(store.pending_for_test(), [item(0, "1")]);
    gate.disarm();
}

/// A registry that moved on between the step and the launch (a retoken here) refuses the job, and
/// the retry asks the NEW client: the runner's mismatch is a refusal like any other, and the next
/// step reads the new answer.
#[test]
fn a_key_mismatch_at_launch_requeues_and_the_next_kick_asks_the_new_client() {
    let _serial = plx_base::testlock::serial();
    struct Registry;
    impl Drop for Registry {
        fn drop(&mut self) {
            plx_base::task::drain_workers_for_test();
            plx_plex::plex::reset_servers_for_test();
        }
    }
    let _registry = Registry;
    plx_plex::plex::reset_servers_for_test();
    let id = plx_plex::plex::register_for_test("rig", "127.0.0.1", 9, "t", "cid");
    let mut store = ViewReconcileStore::default();
    store.enqueue([(id, "1".to_string())]);
    let clients = super::rig::registry_keys();
    pump(&mut store, true, &clients);
    let (sent, port) = one(&mut store);
    let asked_with = sent.req.server().1;
    assert_eq!(Some(asked_with), clients.key(id));

    plx_plex::plex::client_for(id).expect("registered").set_token("rotated");
    super::rig::start(sent, port);
    plx_base::task::drain_workers_for_test();
    assert_eq!(landings(&store), 1, "the refusal is the flight's answer; no read ran");

    let clients = super::rig::registry_keys();
    assert_ne!(clients.key(id), Some(asked_with), "the registry's answer changed with the token");
    pump(&mut store, true, &clients);
    assert_eq!(store.flights_for_test(), 0);
    for _ in 0..RETRY_FRAMES {
        pump(&mut store, true, &clients);
    }
    let (retry, _) = one(&mut store);
    assert_eq!(retry.req.server(), (id, clients.key(id).expect("held")), "the retry names the new client");
}
