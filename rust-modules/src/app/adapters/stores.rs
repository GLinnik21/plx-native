//! The runner for the jobs the data layer's stores emit (`plx_data::stores::StoreJob`).
//!
//! A store's step asks for IO by pushing a typed job on its outbox; it starts no thread and reads
//! no clock. The `Bridge` takes the jobs right after the step that emitted them
//! (`Bridge::metadata_run`, or `Bridge::start_store_jobs` after a command run on the bare store)
//! and hands each to [`StoreRunner::start`], which owns everything the data layer must not: the
//! tape's admission record, the worker thread and the process-global client the request reads.
//! The detail pump keeps a counted backstop for a sender that did neither
//! (`Bridge::start_late_store_jobs`).
//!
//! **One runner per `Bridge`, chosen when the Bridge is built**, as the consent adapter is: `Live`
//! in production, and in a host test the `Held` runner, which parks each job until the test runs
//! it on the test thread (`Bridge::run_held_store_jobs_for_test`). Holding is what lets a test
//! choose the moment a request is "in flight" and the moment it "settles", where a worker thread
//! made both depend on the scheduler: a fetch for an unknown server returns in microseconds, so a
//! fast run settled a request before an "in flight" assertion while a loaded CI runner had not
//! settled it after a hundred `yield_now`s.

use plx_data::metadata::{fetch_full, finish_detail_fetch, DetailPort, DetailReq};
use plx_data::stores::StoreJob;
use plx_machine::job::{Job, Port};

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
        }
    }

    fn start_detail(&mut self, job: Job<DetailReq>, port: DetailPort) {
        // A request supersedes the one before it, so a fetch still parked here belongs to a
        // request that is already retired. Answer it, as the thread it stands for would have,
        // so its landing reservation can be handed back; the answer is discarded by the pump.
        #[cfg(test)]
        if let Self::Held(held) = self {
            for superseded in std::mem::take(held) {
                let StoreJob::Detail(old, old_port) = superseded;
                old_port.done(old.addr, ((old.req.sid, old.req.rk), None));
            }
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

    /// TEST ONLY: run every parked job on THIS thread through the production completion path
    /// (`finish_detail_fetch` over `fetch_full`) and return how many ran. Their landings are
    /// queued for the next pump. Under `Live` there is nothing parked.
    #[cfg(test)]
    pub(crate) fn run_held(&mut self) -> usize {
        let Self::Held(held) = self else { return 0 };
        let run = std::mem::take(held);
        let n = run.len();
        for job in run {
            let StoreJob::Detail(job, port) = job;
            let DetailReq { sid, rk, prefer } = job.req;
            finish_detail_fetch(&port, job.addr, (sid, rk.clone()), || fetch_full(sid, &rk, prefer));
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
