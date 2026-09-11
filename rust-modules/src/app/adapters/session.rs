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
use crate::auth::owner::{CommitPermit, CommitPlan, CommitReply, EndpointCapture,
    AdmissionId, AdmissionReply, Receipt, ServerLifecycle, SessionReadReply, SessionReadRequest, SessionReadValue,
    SESSION_DATA_RECORDS, SESSION_OWNER_RESERVATIONS, SESSION_TOTAL_RESERVATIONS, SESSION_TRANSFER_RECORDS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionIngressError { BatchTooLarge, AddressMismatch, Unadmitted }

enum NativeEndpoint {
    Live(crate::auth::ClientLifecycle),
    #[cfg(test)]
    Fixture(EndpointCapture),
}

impl NativeEndpoint {
    fn logical(&self, sid: u16) -> ServerLifecycle {
        match self {
            Self::Live(native) => native.logical(sid),
            #[cfg(test)]
            Self::Fixture(native) => native.lifecycle,
        }
    }
}

enum Resources {
    Live { publisher: crate::plex::session::ProfilePublisher },
    #[cfg(test)]
    Fixture(FixtureResources),
}

/// Test resource boundary, not a decision machine. Writes use the same CredentialPatch merge
/// as live disk; native registry operations are recorded, never sent to the global registry.
#[cfg(test)]
pub(crate) struct FixtureResources {
    pub disk: crate::plex::session::Session,
    pub endpoints: BTreeMap<u16, EndpointCapture>,
    pub registry_writes: Vec<crate::auth::owner::RegistryPlan>,
    pub profile: Option<crate::auth::owner::ProfilePublication>,
    pub recently_unreachable: bool,
    pub minted_client_id: String,
    pub coordinator_events: Vec<crate::auth::owner::CoordinatorAction>,
    pub root_press_available: bool,
    pub back_results: Vec<bool>,
}

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
    admission: AdmissionId,
    cancelled: Arc<AtomicBool>,
}

struct TransferMetadata {
    receipt: Receipt,
    admission: AdmissionId,
}

pub(crate) struct SessionAdapter {
    landing: Arc<Landing<SessionWorkKey, AuthProgress>>,
    launches: BTreeMap<u32, LaunchMetadata>,
    native: BTreeMap<u32, NativeEndpoint>,
    resources: Resources,
    /// Metadata only. These credits cover owner-held AND dispatcher-carried envelopes, so
    /// cancelling a request must not clear them before those unique records are discarded.
    receipts: BTreeMap<u64, TransferMetadata>,
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
        Self::empty(|name, job| crate::task::spawn_small(name, job), Resources::Live {
            publisher: crate::plex::session::ProfilePublisher::new(_mt),
        })
    }
    #[cfg(test)]
    pub(crate) fn fixture() -> Self { Self::fixture_with(crate::plex::session::Session::default()) }

    #[cfg(test)]
    pub(crate) fn fixture_with(disk: crate::plex::session::Session) -> Self {
        Self::empty(|_, _| false, Resources::Fixture(FixtureResources {
            disk, endpoints: BTreeMap::new(), registry_writes: Vec::new(), profile: None,
            recently_unreachable: false, minted_client_id: "synthetic-client".into(),
            coordinator_events: Vec::new(), root_press_available: true, back_results: Vec::new(),
        }))
    }

    #[cfg(test)]
    pub(crate) fn fixture_resources(&mut self) -> &mut FixtureResources {
        let Resources::Fixture(resources) = &mut self.resources else { panic!("not a fixture adapter") };
        resources
    }

    fn empty(spawn: fn(&'static str, Box<dyn FnOnce() + Send>) -> bool, resources: Resources) -> Self {
        Self { landing: Arc::new(Landing::with_limits(SESSION_DATA_RECORDS,
                SESSION_OWNER_RESERVATIONS, SESSION_TOTAL_RESERVATIONS)),
            launches: BTreeMap::new(), native: BTreeMap::new(), resources,
            receipts: BTreeMap::new(), spawn, main_thread: PhantomData }
    }

    pub(crate) fn capture(&mut self, req: u32, epoch: u64, request: SessionReadRequest) -> SessionReadReply {
        let value = match &mut self.resources {
            Resources::Live { .. } => match request {
                SessionReadRequest::LoginClientId => SessionReadValue::LoginClientId(crate::plex::session::load().client_id),
                SessionReadRequest::ProfilePolicy => SessionReadValue::ProfilePolicy {
                    recently_unreachable: crate::plex::account::plex_tv_recently_unreachable(),
                },
                SessionReadRequest::Endpoint { sid } => {
                    let endpoint = crate::plex::client_for(crate::plex::ServerId::from_raw(sid))
                        .filter(|client| !client.machine_id().is_empty()).map(|client| {
                            let native = crate::auth::ClientLifecycle::capture(client);
                            let captured = EndpointCapture { lifecycle: native.logical(sid), machine_id: client.machine_id().into() };
                            self.native.insert(req, NativeEndpoint::Live(native));
                            captured
                        });
                    SessionReadValue::Endpoint(endpoint)
                }
            },
            #[cfg(test)]
            Resources::Fixture(resources) => match request {
                SessionReadRequest::LoginClientId => {
                    if resources.disk.client_id.is_empty() { resources.disk.client_id = resources.minted_client_id.clone(); }
                    SessionReadValue::LoginClientId(resources.disk.client_id.clone())
                }
                SessionReadRequest::ProfilePolicy => SessionReadValue::ProfilePolicy {
                    recently_unreachable: resources.recently_unreachable,
                },
                SessionReadRequest::Endpoint { sid } => {
                    let endpoint = resources.endpoints.get(&sid).cloned();
                    if let Some(captured) = &endpoint {
                        self.native.insert(req, NativeEndpoint::Fixture(captured.clone()));
                    }
                    SessionReadValue::Endpoint(endpoint)
                }
            },
        };
        SessionReadReply { addr: Addr { to: MachineId::Session, req: RequestId(req) }, epoch, value }
    }

    fn lifecycle_current(&self, req: u32, expected: ServerLifecycle) -> bool {
        match (&self.resources, self.native.get(&req)) {
            (Resources::Live { .. }, Some(NativeEndpoint::Live(native))) => native.is_current(expected),
            #[cfg(test)]
            (Resources::Fixture(resources), Some(NativeEndpoint::Fixture(captured))) =>
                captured.lifecycle == expected && resources.endpoints.get(&expected.sid)
                    .is_some_and(|current| current.lifecycle == expected && current.machine_id == captured.machine_id),
            _ => false,
        }
    }

    fn endpoint_machine_matches(&self, req: u32, machine_id: &str) -> bool {
        if machine_id.is_empty() { return false; }
        match self.native.get(&req) {
            Some(NativeEndpoint::Live(native)) => native.machine_id() == machine_id,
            #[cfg(test)]
            Some(NativeEndpoint::Fixture(captured)) => captured.machine_id == machine_id,
            None => false,
        }
    }

    /// `accepted` means this credential/lifecycle authority was current, not that best-effort
    /// storage has become durable. File write failures retain the existing in-memory behavior.
    pub(crate) fn commit(&mut self, permit: CommitPermit<'_>, plan: &CommitPlan) -> CommitReply {
        use crate::auth::owner::RegistryPlan;
        if plan.lifecycle.is_some_and(|expected| !self.lifecycle_current(permit.request(), expected)) {
            return permit.reply(false);
        }
        if plan.registry.iter().any(|operation| match operation {
            RegistryPlan::Activate { source, .. } => source.origin().is_none() || source.tier.is_none(),
            RegistryPlan::Endpoint { expected, source } => plan.lifecycle != Some(*expected)
                || source.origin().is_none() || !self.endpoint_machine_matches(permit.request(), &source.machine_id),
            _ => false,
        }) { return permit.reply(false); }
        match &mut self.resources {
            Resources::Live { .. } => {
                if let Some(patch) = &plan.credentials {
                    let mut examined = false;
                    let mut matches = false;
                    let _ = crate::plex::session::update(|disk| {
                        examined = true;
                        matches = plan.expected_disk.matches(disk);
                        matches.then(|| patch.merge_into(disk))
                    });
                    if examined && !matches { return permit.reply(false); }
                }
                for operation in &plan.registry {
                    if !crate::auth::execute_session_registry(operation) {
                        return permit.reply(false);
                    }
                }
            }
            #[cfg(test)]
            Resources::Fixture(resources) => {
                if let Some(patch) = &plan.credentials {
                    if !resources.disk.client_id.is_empty() {
                        if !plan.expected_disk.matches(&resources.disk) { return permit.reply(false); }
                        resources.disk = patch.merge_into(&resources.disk);
                    }
                }
                resources.registry_writes.extend(plan.registry.iter().cloned());
            }
        }
        permit.reply(true)
    }

    pub(crate) fn publish_profile(&mut self, publication: crate::auth::owner::ProfilePublication) {
        match &mut self.resources {
            Resources::Live { publisher } => publisher.publish(publication.profile, publication.scope.0),
            #[cfg(test)]
            Resources::Fixture(resources) => resources.profile = Some(publication),
        }
    }

    pub(crate) fn erase(&mut self) {
        match &mut self.resources {
            Resources::Live { .. } => {
                crate::plex::session::clear();
                crate::plex::revoke_all();
                crate::imgcache::clear();
            }
            #[cfg(test)]
            Resources::Fixture(resources) => {
                resources.disk = Default::default();
                resources.endpoints.clear();
                resources.registry_writes.push(crate::auth::owner::RegistryPlan::Revoke);
            }
        }
    }

    pub(crate) fn coordinator(&mut self, action: crate::auth::owner::CoordinatorAction) {
        use crate::auth::owner::CoordinatorAction;
        #[cfg(test)]
        if let Resources::Fixture(resources) = &mut self.resources {
            resources.coordinator_events.push(action);
            return;
        }
        use crate::diag::schema::{DiagEvent, SignInFailure};
        match action {
            CoordinatorAction::CloseTelemetry => crate::telemetry::forget(),
            CoordinatorAction::SignInStarted => crate::diag::event(DiagEvent::SignInStarted),
            CoordinatorAction::SignInCompleted => crate::diag::event(DiagEvent::SignInCompleted),
            CoordinatorAction::SignInCancelled => crate::diag::event(DiagEvent::SignInCancelled),
            CoordinatorAction::SignInFailed { phase } => crate::diag::event(DiagEvent::SignInFailed { kind: match phase {
                crate::auth::Phase::Creating => SignInFailure::PinCreate,
                crate::auth::Phase::Waiting => SignInFailure::Authorization,
                crate::auth::Phase::Discovering => SignInFailure::Discovery,
                _ => SignInFailure::Other,
            } }),
        }
    }

    pub(crate) fn claim_root_press(&mut self) -> bool {
        match &mut self.resources {
            Resources::Live { .. } => crate::webos::take_root_press(),
            #[cfg(test)]
            Resources::Fixture(resources) => std::mem::replace(&mut resources.root_press_available, false),
        }
    }

    pub(crate) fn finish_back(&mut self, resumed: bool) {
        match &mut self.resources {
            Resources::Live { .. } => {
                if resumed { crate::webos::release_root_press(); } else { crate::webos::go_home(); }
            }
            #[cfg(test)]
            Resources::Fixture(resources) => {
                resources.back_results.push(resumed);
                if resumed { resources.root_press_available = true; }
            }
        }
    }

    /// Capacity refusal is synchronous and unsequenced, not appended to a second refusal queue.
    pub(crate) fn start_work(&mut self, req: RequestId, key: SessionWorkKey, admission: AdmissionId,
        input: crate::auth::owner::SessionWork) -> Result<(), AdmissionReply> {
        use crate::auth::owner::SessionOp;
        let (name, stream) = match key.op {
            SessionOp::Login => ("login", true),
            SessionOp::Rediscover => ("rediscover", true),
            SessionOp::HomeRoster => ("roster", false),
            SessionOp::ServerRoster => ("roster-srv", true),
            SessionOp::ProfileSwitch => ("switch", true),
            SessionOp::Endpoint(_) => ("endpoint", false),
            SessionOp::Ready | SessionOp::Picker => return Err(AdmissionReply {
                addr: Addr { to: MachineId::Session, req }, key, correlation: admission, accepted: false,
            }),
        };
        let spawn = self.spawn;
        match self.launch_correlated(req, key, admission, stream, |job| spawn(name, job),
            move |output| crate::auth::run_session_work(req.0, key, input, &output)) {
            Ok(()) | Err(AdmissionError::Duplicate) => Ok(()),
            Err(AdmissionError::Capacity) => Err(AdmissionReply {
                addr: Addr { to: MachineId::Session, req }, key, correlation: admission, accepted: false,
            }),
        }
    }

    /// The owner has already decided to request this work. Resource admission is a separate
    /// result, returned synchronously on capacity rejection without an auxiliary refusal queue.
    #[cfg(test)]
    pub(crate) fn launch(
        &mut self,
        req: RequestId,
        key: SessionWorkKey,
        stream: bool,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> bool,
        run: impl FnOnce(WorkerOutput) + Send + 'static,
    ) -> Result<(), AdmissionError> {
        self.launch_correlated(req, key, AdmissionId(req.0), stream, spawn, run)
    }

    fn launch_correlated(
        &mut self, req: RequestId, key: SessionWorkKey, admission: AdmissionId, stream: bool,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> bool,
        run: impl FnOnce(WorkerOutput) + Send + 'static,
    ) -> Result<(), AdmissionError> {
        let addr = Addr { to: MachineId::Session, req };
        if stream { self.landing.admit_stream(addr)?; } else { self.landing.admit(addr)?; }
        let cancelled = Arc::new(AtomicBool::new(false));
        self.launches.insert(req.0, LaunchMetadata { key, admission, cancelled: Arc::clone(&cancelled) });
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

    pub(crate) fn acknowledge(&mut self, receipts: &[Receipt]) {
        for receipt in receipts {
            if self.receipts.get(&receipt.arrival).is_some_and(|metadata| metadata.receipt == *receipt) {
                self.receipts.remove(&receipt.arrival);
            }
        }
    }

    pub(crate) fn admitted(&self, envelope: &SessionEnvelope) -> bool {
        self.receipts.get(&envelope.arrival).is_some_and(|metadata|
            metadata.receipt == Receipt::of(envelope) && metadata.admission == envelope.admission)
    }

    /// A queued negative report cannot deny this same correlated launch after resource admission,
    /// even if the owner has not received the positive report or first observation yet.
    pub(crate) fn resource_admitted(&self, reply: &AdmissionReply) -> bool {
        self.launches.get(&reply.addr.req.0).is_some_and(|metadata|
            metadata.key == reply.key && metadata.admission == reply.correlation)
            || self.receipts.values().any(|metadata| metadata.receipt.addr == reply.addr
                && metadata.receipt.key == reply.key && metadata.admission == reply.correlation)
    }

    /// Supplied Session fixtures use envelopes obtained through this adapter's explicit
    /// admission/transfer path. Validation polls no live mailbox and never truncates a batch.
    pub(crate) fn validate_supplied(&self, records: &[(Addr, SessionEnvelope)]) -> Result<(), SessionIngressError> {
        if records.len() > SESSION_TRANSFER_RECORDS { return Err(SessionIngressError::BatchTooLarge); }
        for (outer, envelope) in records {
            if *outer != envelope.addr { return Err(SessionIngressError::AddressMismatch); }
            if !self.admitted(envelope) { return Err(SessionIngressError::Unadmitted); }
        }
        Ok(())
    }

    pub(crate) fn take_results(&mut self) -> Vec<SessionEnvelope> {
        // One whole transferred batch at a time, not one record per frame. Landing can refill
        // independently while this batch is carried/committing: up to 96 + 96 distinct records.
        if !self.receipts.is_empty() { return Vec::new(); }
        let mut records = Vec::new();
        self.landing.take_for(&|addr| addr.to == MachineId::Session, &|_| true, &mut records);
        let mut results = Vec::with_capacity(records.len());
        for record in records {
            let Some(metadata) = self.launches.get(&record.addr.req.0) else { continue };
            let key = metadata.key;
            let admission = metadata.admission;
            if record.terminal { self.launches.remove(&record.addr.req.0); }
            let outcome = match record.lane {
                Lane::Data(data_key, value) => {
                    // The same launch builds both addresses/keys; reject malformed fixture input
                    // before it can be confused with a different operation's data.
                    if data_key != key { continue; }
                    let (value, native) = crate::auth::observation::Observation::from_transport(value);
                    if let Some(native) = native {
                        self.native.entry(record.addr.req.0).or_insert(NativeEndpoint::Live(native));
                    }
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
            results.push(SessionEnvelope { addr: record.addr, key, admission, arrival: record.seq, lifecycle,
                terminal: record.terminal, outcome });
        }
        assert!(results.len() <= SESSION_TRANSFER_RECORDS, "Session Landing transfer bound");
        for result in &results {
            let receipt = Receipt::of(result);
            assert!(self.receipts.insert(receipt.arrival,
                TransferMetadata { receipt, admission: result.admission }).is_none(), "duplicate Landing arrival");
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
    fn fixture_commit_uses_latest_preferences_and_never_writes_another_adapter() {
        use crate::auth::owner::{CommitDelta, CredentialPatch, Identity, Pending, PendingCommit,
            SessionInit, SessionMachine, StreamPhase};
        let disk = crate::plex::session::Session { client_id: "synthetic-client".into(), ..Default::default() };
        let mut init = SessionInit::captured(disk.clone());
        init.pending.insert(1, Pending { key: SessionWorkKey { epoch: 1, op: SessionOp::Ready },
            expected: Identity::of(&disk), lifecycle: None, last_arrival: Some(0),
            phase: StreamPhase::Running, capture: None, admission: crate::auth::owner::AdmissionState::NotRequested });
        init.pending_commit = Some(PendingCommit { req: 1, epoch: 1, arrival: 0, terminal: true,
            writes_credentials: true, receipt: None, delta: CommitDelta::default() });
        let owner = SessionMachine::from_init(init);
        let mut a = SessionAdapter::fixture_with(disk.clone());
        let mut b = SessionAdapter::fixture_with(disk.clone());
        a.fixture_resources().disk.playback_quality = Some(crate::plex::session::PlaybackQuality::Original);
        let mut next = disk.clone();
        next.account_token = "synthetic-new-token".into();
        let plan = CommitPlan { expected_disk: Identity::of(&disk),
            credentials: Some(CredentialPatch::of(&next)), registry: Vec::new(), lifecycle: None };
        assert!(a.commit(owner.commit_permit(1, 1, 0).unwrap(), &plan).accepted);
        assert_eq!(a.fixture_resources().disk.account_token, "synthetic-new-token");
        assert_eq!(a.fixture_resources().disk.playback_quality, Some(crate::plex::session::PlaybackQuality::Original));
        assert!(b.fixture_resources().disk.account_token.is_empty());
        assert!(a.fixture_resources().profile.is_none());
        assert!(a.fixture_resources().registry_writes.is_empty());
        let captured = a.capture(2, 1, SessionReadRequest::ProfilePolicy);
        assert!(matches!(captured.value, SessionReadValue::ProfilePolicy { recently_unreachable: false }));
    }

    #[test]
    fn endpoint_commit_rejects_another_machine_before_patching_disk() {
        use crate::auth::owner::{CommitDelta, CredentialPatch, Identity, Pending, PendingCommit,
            RegistryPlan, SessionInit, SessionMachine, StreamPhase};
        let disk = crate::plex::session::Session { client_id: "synthetic-client".into(), ..Default::default() };
        let lifecycle = ServerLifecycle { sid: 0, instance_gen: 11, token_gen: 12 };
        let mut init = SessionInit::captured(disk.clone());
        init.pending.insert(1, Pending { key: SessionWorkKey { epoch: 1, op: SessionOp::Endpoint(0) },
            expected: Identity::of(&disk), lifecycle: Some(lifecycle), last_arrival: Some(1),
            phase: StreamPhase::Running, capture: None, admission: crate::auth::owner::AdmissionState::Accepted(AdmissionId(1)) });
        init.pending_commit = Some(PendingCommit { req: 1, epoch: 1, arrival: 1, terminal: true,
            writes_credentials: true, receipt: None, delta: CommitDelta::default() });
        let owner = SessionMachine::from_init(init);
        let mut adapter = SessionAdapter::fixture_with(disk.clone());
        adapter.fixture_resources().endpoints.insert(0, EndpointCapture {
            lifecycle, machine_id: "machine-a".into(),
        });
        adapter.capture(1, 1, SessionReadRequest::Endpoint { sid: 0 });
        let mut changed = disk.clone();
        changed.account_token = "synthetic-new-token".into();
        let plan = CommitPlan { expected_disk: Identity::of(&disk),
            credentials: Some(CredentialPatch::of(&changed)), lifecycle: Some(lifecycle),
            registry: vec![RegistryPlan::Endpoint { expected: lifecycle,
                source: crate::plex::session::SourceRef { machine_id: "machine-b".into(),
                    origin_url: "http://192.0.2.2:32400".into(), ..Default::default() } }],
        };
        assert!(!adapter.commit(owner.commit_permit(1, 1, 1).unwrap(), &plan).accepted,
            "matching lifecycle cannot authorize repointing another machine");
        assert!(adapter.fixture_resources().disk.account_token.is_empty());
        assert!(adapter.fixture_resources().registry_writes.is_empty());
    }

    fn fill_batch(adapter: &mut SessionAdapter, first_req: u32, epoch: u64) {
        for offset in 0..SESSION_TOTAL_RESERVATIONS {
            adapter.launch(RequestId(first_req + offset), key(epoch), true,
                |job| { job(); true }, move |output| {
                    if offset == 0 {
                        for _ in 0..SESSION_DATA_RECORDS {
                            assert!(ObservationSink::progress(&output,
                                LoginProgress::CodeReplacing { epoch }.into()));
                        }
                    }
                    assert!(output.terminal(failed(epoch)));
                }).unwrap();
        }
    }

    #[test]
    fn transfer_credits_survive_cancel_and_gate_a_full_refilled_landing() {
        let mut adapter = SessionAdapter::fixture();
        fill_batch(&mut adapter, 1, 1);
        let first = adapter.take_results();
        assert_eq!(first.len(), SESSION_TRANSFER_RECORDS);
        let supplied: Vec<_> = first.iter().cloned().map(|record| (record.addr, record)).collect();
        assert!(adapter.validate_supplied(&supplied).is_ok());
        let first_receipt = Receipt::of(&first[0]);
        adapter.acknowledge(&[first_receipt, first_receipt]);
        assert_eq!(adapter.receipts.len(), SESSION_TRANSFER_RECORDS - 1);
        adapter.cancel(RequestId(1));
        assert_eq!(adapter.receipts.len(), SESSION_TRANSFER_RECORDS - 1, "cancel cannot discard carried credits");
        fill_batch(&mut adapter, 100, 2);
        assert_eq!(adapter.landing.len(), SESSION_TRANSFER_RECORDS);
        assert!(adapter.take_results().is_empty(), "no second transfer while any unique first-batch credit remains");
        let receipts: Vec<_> = first.iter().skip(1).map(Receipt::of).collect();
        adapter.acknowledge(&receipts);
        let second = adapter.take_results();
        assert_eq!(second.len(), SESSION_TRANSFER_RECORDS);
        assert!(second[0].arrival > first.last().unwrap().arrival);
        adapter.acknowledge(&[first_receipt]);
        assert_eq!(adapter.receipts.len(), SESSION_TRANSFER_RECORDS, "an old ACK cannot free a new credit");
        let mut wrong = Receipt::of(&second[0]);
        wrong.addr.req = RequestId(9999);
        adapter.acknowledge(&[wrong]);
        assert_eq!(adapter.receipts.len(), SESSION_TRANSFER_RECORDS);
        let wrong_outer = vec![(wrong.addr, second[0].clone())];
        assert_eq!(adapter.validate_supplied(&wrong_outer), Err(SessionIngressError::AddressMismatch));
        let oversized = vec![(second[0].addr, second[0].clone()); SESSION_TRANSFER_RECORDS + 1];
        assert_eq!(adapter.validate_supplied(&oversized), Err(SessionIngressError::BatchTooLarge));
        adapter.acknowledge(&second.iter().map(Receipt::of).collect::<Vec<_>>());
        assert_eq!(adapter.validate_supplied(&[(second[0].addr, second[0].clone())]), Err(SessionIngressError::Unadmitted));
    }

    #[test]
    fn instance_adapters_keep_equal_request_ids_and_full_epochs_independent() {
        let mut a = SessionAdapter::fixture();
        let mut b = SessionAdapter::fixture();
        a.launch(RequestId(1), key(1), true, |job| { job(); true }, |out| {
            out.progress(LoginProgress::Authorized { epoch: 1, token: "synthetic".into() }.into()).unwrap();
            out.complete(failed(1)).unwrap();
        }).unwrap();
        assert!(b.start_work(RequestId(1), key(0x1_0000_0001), AdmissionId(1),
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
            assert!(adapter.start_work(RequestId(req), key(1), AdmissionId(req),
                crate::auth::owner::SessionWork::Login { client_id: "synthetic".into() }).is_ok());
        }
        let refused = adapter.start_work(RequestId(33), key(1), AdmissionId(33),
            crate::auth::owner::SessionWork::Login { client_id: "synthetic".into() });
        let Err(refused) = refused else { panic!("capacity should refuse synchronously") };
        assert_eq!(refused.addr.req, RequestId(33));
        assert!(!refused.accepted);
        assert_eq!(adapter.launches.len(), 32);
        let results = adapter.take_results();
        assert_eq!(results.len(), 32);
        assert!(results.iter().all(|r| r.addr.req != RequestId(33)));
        assert_eq!(adapter.landing.inflight(MachineId::Session), 0);
    }

    #[test]
    fn duplicate_work_effect_cannot_refuse_the_original_running_request() {
        let mut adapter = SessionAdapter::fixture();
        let (tx, rx) = std::sync::mpsc::channel();
        adapter.launch(RequestId(1), key(1), true,
            |job| { tx.send(job).unwrap(); true },
            |output| { assert!(output.terminal(failed(1))); }).unwrap();
        let cancelled = Arc::clone(&adapter.launches[&1].cancelled);
        assert!(adapter.start_work(RequestId(1), key(1), AdmissionId(1),
            crate::auth::owner::SessionWork::Login { client_id: "synthetic".into() }).is_ok(),
            "a duplicate is not a refusal of the original admitted worker");
        assert_eq!(adapter.landing.inflight(MachineId::Session), 1);
        assert!(Arc::ptr_eq(&cancelled, &adapter.launches[&1].cancelled));
        assert!(adapter.resource_admitted(&AdmissionReply {
            addr: Addr { to: MachineId::Session, req: RequestId(1) }, key: key(1),
            correlation: AdmissionId(1), accepted: false,
        }), "accepted before the first observation");
        assert!(adapter.take_results().is_empty());
        rx.recv().unwrap()();
        let results = adapter.take_results();
        assert_eq!(results.len(), 1);
        assert!(results[0].terminal);
        assert!(matches!(results[0].outcome, SessionArrival::Data(_)));
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
