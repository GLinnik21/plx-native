//! Per-application Session resources. Landing capacity bounds records, not payload bytes.
//! Neither a worker nor a fixture adapter can obtain a mutable SessionMachine through this API.

use crate::auth::owner::{SessionArrival, SessionEnvelope, SessionWorkKey};
use crate::auth::AuthProgress;
use crate::ui::landing::{AdmissionError, Landing, Lane, PublishError};
use crate::ui::machine::{Addr, MachineId, RequestId};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A worker can observe cancellation and publish facts. It has no cancellation writer or owner.
pub(crate) struct WorkerOutput {
    addr: Addr,
    key: SessionWorkKey,
    cancelled: Arc<AtomicBool>,
    landing: Arc<Landing<SessionWorkKey, AuthProgress>>,
    closed: std::cell::Cell<bool>,
}

impl WorkerOutput {
    pub(crate) fn cancelled(&self) -> bool { self.cancelled.load(Ordering::Acquire) }
    pub(crate) fn progress(&self, value: AuthProgress) -> Result<(), PublishError> {
        if self.cancelled() { return Err(PublishError::Cancelled); }
        self.landing.progress(self.addr, self.key, value)
    }
    pub(crate) fn complete(&self, value: AuthProgress) -> Result<(), PublishError> {
        self.landing.put(self.addr, self.key, value)
    }
}

impl crate::auth::owner::ObservationSink for WorkerOutput {
    fn live(&self) -> bool { !self.cancelled() && !self.closed.get() }
    fn progress(&self, value: AuthProgress) -> bool {
        if !self.live() { return false; }
        let accepted = WorkerOutput::progress(self, value).is_ok();
        if !accepted { self.closed.set(true); }
        accepted
    }
    fn terminal(&self, value: AuthProgress) -> bool {
        if !self.live() { return false; }
        self.closed.set(true);
        self.complete(value).is_ok()
    }
}

struct LaunchMetadata {
    key: SessionWorkKey,
    cancelled: Arc<AtomicBool>,
}

pub(crate) struct SessionAdapter {
    landing: Arc<Landing<SessionWorkKey, AuthProgress>>,
    launches: BTreeMap<u32, LaunchMetadata>,
    native: BTreeMap<u32, crate::auth::ClientLifecycle>,
    spawn: fn(&'static str, Box<dyn FnOnce() + Send>) -> bool,
    // Construction proves the live adapter originated on main without duplicating or moving the
    // Player adapter's exclusive token. The owned adapter remains !Send/!Sync afterwards.
    main_thread: PhantomData<Rc<()>>,
}

impl Drop for SessionAdapter {
    fn drop(&mut self) {
        // Retire interest, not the resource reservation. WorkerOutput retains Landing until
        // the running closure acknowledges cancellation through its completion guard.
        self.cancel_all();
    }
}

impl SessionAdapter {
    pub(crate) fn live(_mt: &crate::task::MainThread) -> Self {
        Self::empty(|name, job| crate::task::spawn_small(name, job))
    }
    pub(crate) fn fixture() -> Self { Self::empty(|_, _| false) }

    fn empty(spawn: fn(&'static str, Box<dyn FnOnce() + Send>) -> bool) -> Self {
        Self { landing: Arc::new(Landing::with_limits(64, 32, 32)),
            launches: BTreeMap::new(), native: BTreeMap::new(), spawn, main_thread: PhantomData }
    }

    /// Capacity refusal is synchronous, not appended to a second pending queue. It has no
    /// Landing arrival: sequence zero is used only for this never-admitted request's terminal.
    pub(crate) fn start_work(&mut self, req: RequestId, key: SessionWorkKey,
        input: crate::auth::owner::SessionWork) -> Result<(), SessionEnvelope> {
        use crate::auth::owner::SessionOp;
        let (name, stream) = match key.op {
            SessionOp::Login => ("login", true),
            SessionOp::Rediscover => ("rediscover", true),
            SessionOp::HomeRoster => ("roster", false),
            SessionOp::ServerRoster => ("roster-srv", true),
            SessionOp::ProfileSwitch => ("switch", true),
            SessionOp::Endpoint(_) => ("endpoint", false),
        };
        let spawn = self.spawn;
        self.launch(req, key, stream, |job| spawn(name, job),
            move |output| crate::auth::run_session_work(req.0, key, input, &output))
            .map_err(|_| SessionEnvelope {
                addr: Addr { to: MachineId::Session, req }, key, arrival: 0,
                terminal: true, lifecycle: None, outcome: SessionArrival::Refused,
            })
    }

    /// The owner has already decided to request this work. Resource admission is a separate
    /// result, returned synchronously on capacity rejection without an auxiliary refusal queue.
    pub(crate) fn launch(
        &mut self,
        req: RequestId,
        key: SessionWorkKey,
        stream: bool,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> bool,
        run: impl FnOnce(WorkerOutput) + Send + 'static,
    ) -> Result<(), AdmissionError> {
        let addr = Addr { to: MachineId::Session, req };
        if stream { self.landing.admit_stream(addr)?; } else { self.landing.admit(addr)?; }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.launches.insert(req.0, LaunchMetadata { key, cancelled: Arc::clone(&cancelled) });
        let output = WorkerOutput { addr, key, cancelled, landing: Arc::clone(&self.landing),
            closed: std::cell::Cell::new(false) };
        if !spawn(Box::new(move || {
            // Construct only inside a running closure. If spawning is refused the launcher owns
            // the one terminal; dropping an unstarted closure must not create a second one.
            let landing = Arc::clone(&output.landing);
            let Ok(_completion) = landing.completion_guard(addr) else { return };
            if !output.cancelled() { run(output); }
        })) {
            let _ = self.landing.refused(addr);
        }
        Ok(())
    }

    pub(crate) fn cancel(&mut self, req: RequestId) {
        self.native.remove(&req.0);
        if let Some(metadata) = self.launches.remove(&req.0) {
            metadata.cancelled.store(true, Ordering::Release);
        }
        self.landing.cancel(Addr { to: MachineId::Session, req });
    }

    pub(crate) fn cancel_all(&mut self) {
        while let Some((&req, _)) = self.launches.first_key_value() {
            self.cancel(RequestId(req));
        }
        self.native.clear();
    }

    pub(crate) fn take_results(&mut self) -> Vec<SessionEnvelope> {
        let mut records = Vec::new();
        self.landing.take_for(&|addr| addr.to == MachineId::Session, &|_| true, &mut records);
        let mut results = Vec::with_capacity(records.len());
        for record in records {
            let Some(metadata) = self.launches.get(&record.addr.req.0) else { continue };
            let key = metadata.key;
            if record.terminal { self.launches.remove(&record.addr.req.0); }
            let outcome = match record.lane {
                Lane::Data(data_key, value) => {
                    // The same launch builds both addresses/keys; reject malformed fixture input
                    // before it can be confused with a different operation's data.
                    if data_key != key { continue; }
                    let (value, native) = crate::auth::observation::Observation::from_transport(value);
                    if let Some(native) = native { self.native.insert(record.addr.req.0, native); }
                    SessionArrival::Data(Arc::new(value))
                }
                Lane::Dropped(req) => {
                    if req != record.addr.req { continue; }
                    SessionArrival::Dropped
                }
                Lane::Refused(req) => {
                    if req != record.addr.req { continue; }
                    SessionArrival::Refused
                }
            };
            let lifecycle = match key.op {
                crate::auth::owner::SessionOp::Endpoint(sid) =>
                    self.native.get(&record.addr.req.0).map(|native| native.logical(sid)),
                _ => None,
            };
            results.push(SessionEnvelope { addr: record.addr, key, arrival: record.seq, lifecycle,
                terminal: record.terminal, outcome });
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{LoginProgress, owner::SessionOp};
    use crate::auth::owner::ObservationSink;

    fn key(epoch: u64) -> SessionWorkKey { SessionWorkKey { epoch, op: SessionOp::Login } }
    fn failed(epoch: u64) -> AuthProgress {
        LoginProgress::Failed { epoch, message: "Synthetic failure".into() }.into()
    }

    #[test]
    fn instance_adapters_keep_equal_request_ids_and_full_epochs_independent() {
        let mut a = SessionAdapter::fixture();
        let mut b = SessionAdapter::fixture();
        a.launch(RequestId(1), key(1), true, |job| { job(); true }, |out| {
            out.progress(LoginProgress::Authorized { epoch: 1, token: "synthetic".into() }.into()).unwrap();
            out.complete(failed(1)).unwrap();
        }).unwrap();
        assert!(b.start_work(RequestId(1), key(0x1_0000_0001),
            crate::auth::owner::SessionWork::Login { client_id: "synthetic".into() }).is_ok());
        let a = a.take_results();
        assert_eq!(a.len(), 2);
        assert!(!a[0].terminal && a[1].terminal);
        assert!(a[0].arrival < a[1].arrival);
        let b = b.take_results();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].key.epoch, 0x1_0000_0001);
        assert!(matches!(b[0].outcome, SessionArrival::Refused));
    }

    #[test]
    fn resource_capacity_refusal_is_synchronous_and_not_an_extra_queue() {
        let mut adapter = SessionAdapter::fixture();
        for req in 1..=32 {
            assert!(adapter.start_work(RequestId(req), key(1),
                crate::auth::owner::SessionWork::Login { client_id: "synthetic".into() }).is_ok());
        }
        let refused = adapter.start_work(RequestId(33), key(1),
            crate::auth::owner::SessionWork::Login { client_id: "synthetic".into() });
        let Err(refused) = refused else { panic!("capacity should refuse synchronously") };
        assert_eq!(refused.addr.req, RequestId(33));
        assert!(refused.terminal);
        assert!(matches!(refused.outcome, SessionArrival::Refused));
        assert_eq!(adapter.launches.len(), 32);
        let results = adapter.take_results();
        assert_eq!(results.len(), 32);
        assert!(results.iter().all(|r| r.addr.req != RequestId(33)));
        assert_eq!(adapter.landing.inflight(MachineId::Session), 0);
    }

    #[test]
    fn cancellation_keeps_the_running_reservation_until_worker_acknowledges() {
        let mut adapter = SessionAdapter::fixture();
        let (tx, rx) = std::sync::mpsc::channel();
        adapter.launch(RequestId(1), key(1), true,
            |job| { tx.send(job).unwrap(); true },
            |_| panic!("cancelled unstarted worker must not perform network work")).unwrap();
        adapter.cancel(RequestId(1));
        assert_eq!(adapter.landing.inflight(MachineId::Session), 1);
        assert!(adapter.take_results().is_empty());
        assert_eq!(adapter.landing.inflight(MachineId::Session), 1);
        rx.recv().unwrap()();
        assert!(adapter.take_results().is_empty());
        assert_eq!(adapter.landing.inflight(MachineId::Session), 0);
        assert!(adapter.launches.is_empty());
    }

    #[test]
    fn adapter_teardown_cancels_worker_without_minting_an_early_terminal() {
        let mut adapter = SessionAdapter::fixture();
        let landing = Arc::clone(&adapter.landing);
        let (tx, rx) = std::sync::mpsc::channel();
        adapter.launch(RequestId(1), key(1), true,
            |job| { tx.send(job).unwrap(); true },
            |_| panic!("a departed App must not start auth network work")).unwrap();
        let cancelled = Arc::clone(&adapter.launches[&1].cancelled);
        drop(adapter);
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(landing.inflight(MachineId::Session), 1);
        rx.recv().unwrap()();
        let mut records = Vec::new();
        landing.take_for(&|_| true, &|_| true, &mut records);
        assert!(records.is_empty());
        assert_eq!(landing.inflight(MachineId::Session), 0);
    }

    #[test]
    fn stream_overflow_stops_producer_and_preserves_one_ordered_terminal() {
        let mut adapter = SessionAdapter::fixture();
        adapter.launch(RequestId(1), key(1), true, |job| { job(); true }, |out| {
            for _ in 0..64 {
                assert!(ObservationSink::progress(&out, failed(1)));
            }
            assert!(!ObservationSink::progress(&out, failed(1)));
            assert!(!out.live());
            assert!(!out.terminal(failed(1)), "overflow cannot subsequently claim success");
        }).unwrap();
        let records = adapter.take_results();
        assert_eq!(records.len(), 65);
        assert!(records.windows(2).all(|pair| pair[0].arrival < pair[1].arrival));
        assert_eq!(records.iter().filter(|r| r.terminal).count(), 1);
        assert!(matches!(records.last().unwrap().outcome, SessionArrival::Dropped));
        assert_eq!(adapter.landing.inflight(MachineId::Session), 0);
    }

    #[test]
    fn running_worker_unwind_uses_the_guard_terminal() {
        let mut adapter = SessionAdapter::fixture();
        let (tx, rx) = std::sync::mpsc::channel();
        adapter.launch(RequestId(1), key(1), true,
            |job| { tx.send(job).unwrap(); true },
            |_| panic!("synthetic auth worker unwind")).unwrap();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(rx.recv().unwrap())).is_err());
        let records = adapter.take_results();
        assert_eq!(records.len(), 1);
        assert!(records[0].terminal);
        assert!(matches!(records[0].outcome, SessionArrival::Dropped));
        assert_eq!(adapter.landing.inflight(MachineId::Session), 0);
    }
}
