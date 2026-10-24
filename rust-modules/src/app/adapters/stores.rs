//! The runner for the jobs the data layer's stores emit (`plx_data::stores::StoreJob`).
//!
//! A store's step asks for IO by pushing a typed job on its outbox; it starts no thread and reads
//! no clock. The `Bridge` takes the jobs right after the step that emitted them
//! (`Bridge::metadata_run`, `Bridge::collection_pump`, or `Bridge::start_store_jobs` after a
//! command run on the bare store) and hands each to [`StoreRunner::start`], which owns everything
//! the data layer must not: the tape's admission record, the worker thread and the process-global
//! client the request reads. The detail pump keeps a counted backstop for a sender that did
//! neither (`Bridge::start_late_store_jobs`).
//!
//! **One runner per `Bridge`, chosen when the Bridge is built**, as the consent adapter is: `Live`
//! in production, and in a host test the `Held` runner, which parks each job until the test runs
//! it on the test thread (`Bridge::run_held_store_jobs_for_test`). Holding is what lets a test
//! choose the moment a request is "in flight" and the moment it "settles", where a worker thread
//! made both depend on the scheduler: a fetch for an unknown server returns in microseconds, so a
//! fast run settled a request before an "in flight" assertion while a loaded CI runner had not
//! settled it after a hundred `yield_now`s.
//!
//! **The registry is read here, not in a step.** A step that decides from "which client does this
//! server slot hold" is handed the answer as an input (`plx_plex::plex::ClientKeys`), which the
//! Bridge fills with [`refill_client_keys`] immediately before the step, in the same call. A job
//! carries the key its step decided on, and the runner starts it only if the registry still holds
//! that client at the moment of the launch.

use plx_data::collection::{finish_collection_job, CollectionPort, CollectionReq};
use plx_data::metadata::{fetch_full, finish_detail_fetch, DetailPort, DetailReq};
use plx_data::stores::StoreJob;
use plx_machine::job::{Job, Port};
use plx_plex::plex::{Client, ClientKey, ClientKeys};

/// Fill `keys` with the registry's answer for every active server slot, in place (a pump that runs
/// every frame allocates nothing for it). The only loop over `client_for` that feeds a step: an
/// active slot can still hold no client, so the walk asks each slot for its client and keeps the
/// ones that answer, which makes `keys.key(sid)` equal `client_for(sid).map(ClientKey::of)` for
/// every `sid`, a retired or unregistered one included.
pub(crate) fn refill_client_keys(keys: &mut ClientKeys) {
    keys.refill(plx_plex::plex::server_ids().filter_map(|sid| {
        plx_plex::plex::client_for(sid).map(|client| (sid, ClientKey::of(client)))
    }));
}

/// The client a job's step decided on, if the registry still holds it: `client_for(sid)` accepted
/// only when it is the very client (and credential) the step's [`ClientKeys`] named. A job the
/// registry has moved on from (a sign-out, a re-registration, a retoken) is not started.
fn resolve(sid: plx_plex::plex::ServerId, key: ClientKey) -> Option<&'static Client> {
    plx_plex::plex::client_for(sid).filter(|client| ClientKey::of(client) == key)
}

pub(crate) enum StoreRunner {
    /// Start every job on a worker thread.
    Live,
    /// TEST ONLY: park every job (after its tape admission) until [`StoreRunner::run_held`].
    #[cfg(test)]
    Held(Vec<StoreJob>),
}

impl StoreRunner {
    pub(crate) fn live() -> Self {
        Self::Live
    }

    #[cfg(test)]
    pub(crate) fn held() -> Self {
        Self::Held(Vec::new())
    }

    /// Admit `job` through the tape and start it. Exactly one answer reaches its port: the fetch's
    /// own, or a refusal when nothing was started.
    pub(crate) fn start(&mut self, job: StoreJob) {
        match job {
            StoreJob::Detail(job, port) => self.start_detail(job, port),
            StoreJob::Collection(job, port) => self.start_collection(job, port),
        }
    }

    fn start_detail(&mut self, job: Job<DetailReq>, port: DetailPort) {
        // A request supersedes the one before it, so a fetch still parked here belongs to a
        // request that is already retired. Answer it, as the thread it stands for would have,
        // so its landing reservation can be handed back; the answer is discarded by the pump.
        #[cfg(test)]
        if let Self::Held(held) = self {
            Self::retire_parked_details(held);
        }
        let addr = job.addr;
        let (sid, gen) = (job.req.sid, addr.req.0);
        let request = serde_json::json!({"store":"metadata",
            "sid":sid.raw(),"rk":job.req.rk,"gen":gen,
            "client":plx_plex::plex::client_for(sid).map(|c| c.instance_gen())});
        let admission = port.clone();
        if !plx_data::stores::tape::admit(request, || self.launch_detail(job, port)) {
            // Nothing was started, so nothing will ever answer on its own: the refusal is what
            // settles the spinner, exactly one event for the request.
            admission.refused(addr);
        }
    }

    /// TEST ONLY: answer the detail fetches still parked as superseded (see [`Self::start_detail`]),
    /// and keep every other store's parked job: a newer detail request retires only the requests it
    /// supersedes. A `match` over every job kind, so a store that joins the lane must say what a
    /// newer detail request does to its parked jobs.
    #[cfg(test)]
    fn retire_parked_details(held: &mut Vec<StoreJob>) {
        for parked in std::mem::take(held) {
            match parked {
                StoreJob::Detail(old, old_port) => {
                    old_port.done(old.addr, ((old.req.sid, old.req.rk), None));
                }
                kept @ StoreJob::Collection(..) => held.push(kept),
            }
        }
    }

    fn launch_detail(&mut self, job: Job<DetailReq>, port: DetailPort) -> bool {
        match self {
            Self::Live => {
                let Job { addr, req: DetailReq { sid, rk, prefer } } = job;
                plx_base::task::spawn_small("detail", move || {
                    finish_detail_fetch(&port, addr, (sid, rk.clone()), || fetch_full(sid, &rk, prefer));
                })
            }
            #[cfg(test)]
            Self::Held(held) => {
                held.push(StoreJob::Detail(job, port));
                true
            }
        }
    }

    fn start_collection(&mut self, job: Job<CollectionReq>, port: CollectionPort) {
        let addr = job.addr;
        let request = job.req.admission(addr);
        let admission = port.clone();
        if !plx_data::stores::tape::admit(request, || self.launch_collection(job, port)) {
            // Nothing was started, so nothing will ever answer on its own: the refusal hands the
            // page's claim back, and the next pump asks again.
            admission.refused(addr);
        }
    }

    /// Start the request on a worker, if the registry still holds the client the step decided on.
    /// The check is made HERE, at the launch the tape records, so a replay (which never launches)
    /// and a recording agree on the one boolean they both hold.
    fn launch_collection(&mut self, job: Job<CollectionReq>, port: CollectionPort) -> bool {
        let (sid, key) = job.req.server();
        let Some(client) = resolve(sid, key) else { return false };
        match self {
            Self::Live => {
                let Job { addr, req } = job;
                plx_base::task::spawn_small("collection", move || {
                    finish_collection_job(&port, addr, client, req);
                })
            }
            #[cfg(test)]
            Self::Held(held) => {
                held.push(StoreJob::Collection(job, port));
                true
            }
        }
    }

    /// TEST ONLY: run every parked job on THIS thread through the production completion path
    /// (`finish_detail_fetch` over `fetch_full`, `finish_collection_job`) and return how many ran. Their landings are
    /// queued for the next pump. Under `Live` there is nothing parked.
    #[cfg(test)]
    pub(crate) fn run_held(&mut self) -> usize {
        let Self::Held(held) = self else { return 0 };
        let run = std::mem::take(held);
        let n = run.len();
        for job in run {
            match job {
                StoreJob::Detail(job, port) => {
                    let DetailReq { sid, rk, prefer } = job.req;
                    finish_detail_fetch(&port, job.addr, (sid, rk.clone()), || fetch_full(sid, &rk, prefer));
                }
                StoreJob::Collection(job, port) => {
                    // The registry may have moved on since the job was parked: a request whose
                    // client is gone is refused, as the launch would have refused it.
                    let (sid, key) = job.req.server();
                    match resolve(sid, key) {
                        Some(client) => finish_collection_job(&port, job.addr, client, job.req),
                        None => port.refused(job.addr),
                    }
                }
            }
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_data::stores::collection::{CollectionCmd, CollectionStore};
    use plx_data::stores::metadata::{MetadataCmd, MetadataStore};
    use plx_data::stores::tape;
    use plx_plex::plex::ServerId;

    use crate::app::bridge::Bridge;

    /// Puts the tape back to its inactive state when a test ends, however it ends.
    struct TapeReset;
    impl Drop for TapeReset {
        fn drop(&mut self) { tape::reset_for_test(); }
    }

    fn request(store: &mut MetadataStore, sid: ServerId, rk: &str) -> StoreJob {
        assert!(store.run(MetadataCmd::RequestDetail { sid, rk: rk.to_string() }));
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one accepted request emits one job");
        jobs.remove(0)
    }

    /// A fresh store with its first detail request out (generation 1) and the tape in replay
    /// holding the one admission record a recording of that request would hold, `admitted` as
    /// given. Returns the store, the job the request emitted and that record.
    fn replaying(admitted: bool) -> (MetadataStore, StoreJob, serde_json::Value) {
        let sid = ServerId::UNSET;
        tape::init(None, true);
        let mut store = MetadataStore::default();
        let job = request(&mut store, sid, "42");
        let admission = serde_json::json!({"content_resource":true, "admitted":admitted,
            "request":{"store":"metadata","sid":sid.raw(),"rk":"42","gen":1,"client":null}});
        tape::begin([admission.clone()].into(), Default::default());
        (store, job, admission)
    }

    /// What the live landing holds for the request, read the way a live run's pump reads it. A
    /// replaying pump takes its landings from the tape's recorded replies and never looks at the
    /// live landing, so the tape is retired first, after its verdict has been taken.
    fn pump_the_live_landing(store: &mut MetadataStore) {
        tape::reset_for_test();
        store.pump_detail();
    }

    /// Replay with `"admitted":false`: the tape says the recorded run's launch was refused, so
    /// nothing may start and the runner itself settles the request, with one refusal. The tape's
    /// own verdict has to read clean too: a mismatched admission also answers "not admitted", so a
    /// request whose bytes drifted from the record would pass every other assertion here.
    #[test]
    fn a_job_the_tape_does_not_admit_is_refused_once_and_the_request_settles() {
        let _serial = plx_base::testlock::serial();
        let _tape = TapeReset;
        let (mut store, job, admission) = replaying(false);
        let sid = ServerId::UNSET;

        StoreRunner::live().start(job);
        // Had a worker been started it would have answered by the time this returns.
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        assert!(store.view().detail_loading(), "the refusal is queued, and lands on the pump");
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 1);

        pump_the_live_landing(&mut store);
        assert!(!store.view().detail_loading(), "the refusal settled the spinner");
        assert_eq!(store.view().detail_request_status(sid, "42"), Some(false));
        assert!(store.view().current().is_none(), "a refusal installs nothing");
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 0,
            "one refusal handed the one reservation back");
    }

    /// Replay with `"admitted":true`: the recorded run launched a worker, and replay never does, so
    /// the runner answers nothing. The request stays owed, to be settled by the replayed landing.
    #[test]
    fn a_job_the_tape_admits_launches_nothing_under_replay_and_stays_owed() {
        let _serial = plx_base::testlock::serial();
        let _tape = TapeReset;
        let (mut store, job, admission) = replaying(true);
        let sid = ServerId::UNSET;

        StoreRunner::live().start(job);
        // Had a worker been started it would have answered by the time this returns.
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        pump_the_live_landing(&mut store);
        assert!(store.view().detail_loading(), "no worker and no refusal answered, so the request is still out");
        assert_eq!(store.view().detail_request_status(sid, "42"), Some(true));
        assert!(store.view().current().is_none());
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 1,
            "an admitted replay launch holds its reservation until the recorded landing");
    }

    /// `Held` parks each admitted job, and a newer request retires the one parked before it: that
    /// one is answered as the thread it stands for would have answered, so its reservation comes
    /// back on the next pump, and only the newest job is left to run.
    #[test]
    fn a_held_job_is_answered_as_superseded_when_the_next_request_starts() {
        let _serial = plx_base::testlock::serial();
        let sid = ServerId::UNSET;
        let mut store = MetadataStore::default();
        let mut runner = StoreRunner::held();
        let (old, new) = (request(&mut store, sid, "old"), request(&mut store, sid, "new"));

        runner.start(old);
        runner.start(new);
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 2);
        store.pump_detail();
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 1,
            "the superseded job's answer handed its slot back");
        assert!(store.view().detail_loading(), "the newest request is still out");

        assert_eq!(runner.run_held(), 1, "only the newest job was still parked");
        store.pump_detail();
        assert!(!store.view().detail_loading());
        assert_eq!(plx_data::metadata::detail_inflight_for_test(store.adapter_ref()), 0);
    }

    // ---- the Collection lane: each outcome of a collection job reaches its port once ----

    /// Puts the tape, the registry and the workers back to their inactive state when a test ends,
    /// however it ends. Declare it AFTER the serial guard, so it drops first.
    struct CollectionRig;
    impl Drop for CollectionRig {
        fn drop(&mut self) {
            plx_base::task::drain_workers_for_test();
            tape::reset_for_test();
            plx_plex::plex::reset_servers_for_test();
        }
    }

    /// A server registered at a port nothing listens on (a read that starts fails at once) and a
    /// collection store whose first request, the header of `50001`, is out: the job its pump
    /// emitted, keyed to the client the registry held when it did.
    fn collection_request() -> (ServerId, CollectionStore, StoreJob) {
        use plx_data::collection::{CollectionTarget, PAGE_SIZE};
        use plx_plex::plex::collections::CollectionRef;
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("runner", "127.0.0.1", 9, "t", "cid");
        let keys = registry_keys();
        let mut store = CollectionStore::default();
        let id = CollectionRef { sid, rk: "50001".into(), sec: 1, tag: 7, name: "Set".into() };
        assert!(store.run(CollectionCmd::Open { target: CollectionTarget { id, want: PAGE_SIZE } }, &keys));
        store.pump(plx_machine::landgate::fixture_gate(), &keys);
        let mut jobs = store.take_jobs();
        assert_eq!(jobs.len(), 1, "one open page, one request");
        (sid, store, jobs.remove(0))
    }

    /// The registry's answer as of now, as the Bridge fills it before a step.
    fn registry_keys() -> ClientKeys {
        let mut keys = ClientKeys::default();
        refill_client_keys(&mut keys);
        keys
    }

    /// The admission record of that request, `admitted` as given.
    fn collection_admission(sid: ServerId, admitted: bool) -> serde_json::Value {
        let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
        serde_json::json!({"content_resource":true,"admitted":admitted,
            "request":{"store":"collection","slot":0,"gen":1,"sid":sid.raw(),"client":client,
                "job":{"Header":{"rk":"50001"}}}})
    }

    fn collection_claim_held(store: &CollectionStore) -> bool { store.adapter_for_test().busy_for_test() }

    /// Recording, the registry still holding the client the step decided on: the worker starts,
    /// reads, and posts its ONE answer; taking it releases the claim.
    #[test]
    fn a_collection_job_whose_client_is_still_held_is_admitted_and_answered_once() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = collection_request();
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![collection_admission(sid, true)], None),
            "the request the runner admitted is the recorded shape, admitted");
        assert!(store.adapter_for_test().has_mail_for_test(), "the worker answered");
        tape::reset_for_test();
        let generation = store.generation_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert_eq!(store.generation_for_test(), generation, "the same listing took the answer: it was not rebuilt");
        assert!(!collection_claim_held(&store), "the one answer released the claim");
        assert_eq!(store.view().current().expect("open").status, plx_data::collection::CollectionStatus::Failed,
            "the unreachable server's failure is what landed");
        assert!(!store.has_jobs(), "a failed read is retried after its hold-off, not at once");
    }

    /// Replay with `"admitted":false`: the recorded run's launch was refused, so nothing starts and
    /// the runner settles the request with one refusal, handing the claim back. The tape's own
    /// verdict has to read clean too: a request whose bytes drifted from the record also answers
    /// "not admitted".
    #[test]
    fn a_collection_job_the_tape_does_not_admit_is_refused_once_and_its_claim_comes_back() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, mut store, job) = collection_request();
        let admission = collection_admission(sid, false);
        tape::init(None, true);
        tape::begin([admission.clone()].into(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None),
            "the request the runner admitted is byte-for-byte the recorded one");
        assert!(!collection_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(), "a refusal answers nothing");
        tape::reset_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert_eq!(store.take_jobs().len(), 1, "the next pump asks again");
    }

    /// Replay with `"admitted":true`: the recorded run launched a worker and replay never does, so
    /// the runner answers nothing. The request stays owed, to be settled by the replayed landing.
    #[test]
    fn a_collection_job_the_tape_admits_launches_nothing_under_replay_and_stays_owed() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job) = collection_request();
        let admission = collection_admission(sid, true);
        tape::init(None, true);
        tape::begin([admission.clone()].into(), Default::default());

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![admission], None));
        assert!(collection_claim_held(&store), "no worker and no refusal answered: the claim is held");
        assert!(!store.adapter_for_test().has_mail_for_test());
    }

    /// The registry moved on between the step and the launch (a retoken here): the runner does not
    /// start a request through a credential the step never asked with. The tape records the launch
    /// as not admitted, and the refusal hands the claim back.
    #[test]
    fn a_collection_job_whose_client_the_registry_has_moved_on_from_is_not_started() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (sid, store, job) = collection_request();
        plx_plex::plex::client_for(sid).expect("registered").set_token("rotated");
        tape::init(None, false);

        StoreRunner::live().start(job);
        plx_base::task::drain_workers_for_test();

        assert_eq!(tape::finish(), (vec![collection_admission(sid, false)], None),
            "recorded as a launch that did not happen");
        assert!(!collection_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test(), "nothing was read, so nothing answered");
    }

    /// `Held` parks each admitted job, and a newer DETAIL request retires only the detail fetches
    /// parked before it: another store's parked job is not a request that detail request supersedes.
    #[test]
    fn a_newer_detail_request_leaves_a_parked_collection_job_to_run() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, mut store, job) = collection_request();
        let mut runner = StoreRunner::held();
        let mut metadata = MetadataStore::default();

        runner.start(job);
        runner.start(request(&mut metadata, ServerId::UNSET, "d1"));
        assert_eq!(runner.run_held(), 2, "the collection job was kept, and the detail job parked after it");

        assert!(store.adapter_for_test().has_mail_for_test(), "the kept job ran and answered");
        let generation = store.generation_for_test();
        store.pump(plx_machine::landgate::fixture_gate(), &registry_keys());
        assert_eq!(store.generation_for_test(), generation, "the same listing took the answer");
        assert!(!collection_claim_held(&store), "the answer released the claim");
    }

    /// A parked job runs later, and the registry may have moved on by then: the held runner
    /// refuses it, as the launch would have, so the claim is handed back and nothing answers.
    #[test]
    fn a_parked_collection_job_whose_client_is_gone_when_it_runs_is_refused() {
        let _serial = plx_base::testlock::serial();
        let _rig = CollectionRig;
        let (_, store, job) = collection_request();
        let mut runner = StoreRunner::held();

        runner.start(job);
        assert!(collection_claim_held(&store), "parked, still out");
        plx_plex::plex::reset_servers_for_test();
        assert_eq!(runner.run_held(), 1);

        assert!(!collection_claim_held(&store), "the refusal handed the claim back");
        assert!(!store.adapter_for_test().has_mail_for_test());
    }

    // ---- the Bridge's drain, which feeds the runner ----

    /// What `playback::describe_movie` does: a command run on the bare store, which emits a job.
    fn run_on_the_bare_store(bridge: &mut Bridge) {
        let (sid, rk) = (ServerId::UNSET, "42".to_string());
        assert!(bridge.metadata_mut().run(MetadataCmd::RequestDetail { sid, rk }));
    }

    fn run_the_detail_pump(bridge: &mut Bridge) {
        let _frame = plx_base::task::FrameScope::enter();
        bridge.metadata_pump_detail();
    }

    /// The two ways a sender keeps its job out of the backstop: through `Bridge::metadata_run`, or
    /// by draining right after a command on the bare store. Both leave the frame check green after
    /// the pump has run.
    #[test]
    fn a_sender_that_drains_its_own_job_leaves_nothing_for_the_backstop() {
        let _serial = plx_base::testlock::serial();
        let mut bridge = Bridge::for_test(|| 0);

        assert!(bridge.metadata_run(MetadataCmd::RequestDetail { sid: ServerId::UNSET, rk: "42".into() }));
        run_the_detail_pump(&mut bridge);
        bridge.assert_no_store_jobs_left_for_test();

        run_on_the_bare_store(&mut bridge);
        assert_eq!(bridge.start_store_jobs(), 1, "the drain started the job the bare command emitted");
        run_the_detail_pump(&mut bridge);
        bridge.assert_no_store_jobs_left_for_test();
    }

    /// A sender that does neither is caught at the frame's end while its job is still on the outbox.
    #[test]
    #[should_panic(expected = "sits on an outbox")]
    fn a_job_left_on_an_outbox_fails_the_frame_check() {
        let _serial = plx_base::testlock::serial();
        let mut bridge = Bridge::for_test(|| 0);
        run_on_the_bare_store(&mut bridge);
        bridge.assert_no_store_jobs_left_for_test();
    }

    /// ...and still caught once the detail pump has started it. The outbox is empty by then, so a
    /// check that read only the outbox would pass, which is the frame `update()` ends with: the
    /// backstop rescues the request, and the count is what keeps the bypass from being silent.
    #[test]
    #[should_panic(expected = "backstop had to start")]
    fn a_job_the_pump_had_to_start_late_fails_the_frame_check_though_the_outbox_is_empty() {
        let _serial = plx_base::testlock::serial();
        let mut bridge = Bridge::for_test(|| 0);
        run_on_the_bare_store(&mut bridge);
        run_the_detail_pump(&mut bridge);
        assert!(!bridge.metadata_mut().has_jobs(), "the backstop started it: the outbox is empty");
        bridge.assert_no_store_jobs_left_for_test();
    }
}
