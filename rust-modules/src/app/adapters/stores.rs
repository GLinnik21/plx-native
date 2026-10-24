//! The runner for the jobs the data layer's stores emit (`plx_data::stores::StoreJob`).
//!
//! A store's step asks for IO by pushing a typed job on its outbox; it starts no thread and reads
//! no clock. The `Bridge` takes the jobs right after the step that emitted them
//! (`Bridge::metadata_run`, and a backstop before the detail pump takes its landing) and hands each
//! to [`StoreRunner::start`], which owns everything the data layer must not: the tape's admission
//! record, the worker thread and the process-global client the request reads.
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
