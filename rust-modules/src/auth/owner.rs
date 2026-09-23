//! Concrete auth decisions and immutable publications. Resource operations belong to the
//! application adapter; constructing or observing this value performs no external work.

use super::{Phase, Picker, UserTile};
use crate::plex::session::async_persistence::{PersistencePurpose, RejectionKind};
use crate::plex::session::{Session as PersistedSession, UserRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use crate::ui::machine::{Addr, Canon, LogicalState, MachineId};

mod incident;
use crate::telemetry::incident::{IncidentContext, InternalClass};
pub(crate) use incident::{IncidentDelivery, IncidentFlow, IncidentKey, IncidentLane, IncidentOffer,
    IncidentReport, IncidentState};

pub(crate) const SESSION_DATA_RECORDS: usize = 64;
pub(crate) const SESSION_OWNER_RESERVATIONS: u32 = 32;
pub(crate) const SESSION_TOTAL_RESERVATIONS: u32 = 32;
pub(crate) const SESSION_TRANSFER_RECORDS: usize = SESSION_DATA_RECORDS + SESSION_TOTAL_RESERVATIONS as usize;

/// A credit for one distinct transferred record, not a worker-completion acknowledgement.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Receipt {
    pub arrival: u64,
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub key: SessionWorkKey,
}

impl Receipt {
    pub fn of(envelope: &SessionEnvelope) -> Self {
        Self { arrival: envelope.arrival, addr: envelope.addr, key: envelope.key }
    }
    fn write(&self, w: &mut Canon) {
        self.addr.to.write_canon(w);
        w.u32(self.addr.req.0).u64(self.arrival).u64(self.key.epoch);
        write_op(w, self.key.op);
    }
}

pub(super) fn write_purpose(w: &mut Canon, purpose: PersistencePurpose) {
    w.u8(match purpose {
        PersistencePurpose::Discovery => 0,
        PersistencePurpose::Final => 1,
        PersistencePurpose::Profile => 2,
        PersistencePurpose::Background => 3,
    });
}

fn write_op(w: &mut Canon, op: SessionOp) {
    match op {
        SessionOp::Login => { w.u8(0); }
        SessionOp::Rediscover => { w.u8(1); }
        SessionOp::HomeRoster => { w.u8(2); }
        SessionOp::ServerRoster => { w.u8(3); }
        SessionOp::ProfileSwitch => { w.u8(4); }
        SessionOp::Endpoint(sid) => { w.u8(5).u32(u32::from(sid)); }
        SessionOp::Ready => { w.u8(6); }
        SessionOp::Picker => { w.u8(7); }
        SessionOp::DevBoundary => { w.u8(8); }
    }
}

/// Delivery keeps the exact request even though generic non-instance delivery drops its outer
/// request field. The application verifies the outer address before constructing this event.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SessionEnvelope {
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub key: SessionWorkKey,
    pub admission: AdmissionId,
    pub arrival: u64,
    pub terminal: bool,
    pub lifecycle: Option<ServerLifecycle>,
    pub outcome: SessionArrival,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum SessionArrival {
    Data(#[serde(with = "super::observation::arc")] Arc<super::observation::Observation>),
    Refused,
    Dropped,
}

impl SessionEnvelope {
    fn write(&self, w: &mut Canon) {
        Receipt::of(self).write(w);
        w.u32(self.admission.0).bool(self.terminal);
        w.option(self.lifecycle, |w, life| {
            w.u32(u32::from(life.sid)).u32(life.instance_gen).u32(life.token_gen);
        });
        match &self.outcome {
            SessionArrival::Data(data) => { w.u8(0); data.write(w); }
            SessionArrival::Refused => { w.u8(1); }
            SessionArrival::Dropped => { w.u8(2); }
        }
    }
}

/// Resource-side producer contract. Auth workers name this domain trait, never app/ or a
/// controller. Returning false means the stream is closed and no further work may be published.
pub(crate) trait ObservationSink {
    fn live(&self) -> bool;
    fn progress(&self, value: super::AuthProgress) -> bool;
    fn terminal(&self, value: super::AuthProgress) -> bool;
}

/// Fully captured worker input. No native pointer or closure can be serialized into work.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum SessionWork {
    Login { client_id: String },
    Rediscover { client_id: String, account_token: String },
    HomeRoster { client_id: String, account_token: String, expected: Identity },
    ServerRoster { session: PersistedSession, expected: Identity },
    ProfileSwitch { session: PersistedSession, expected: Identity, tile: UserTile,
        pin: Option<String>, recently_unreachable: bool },
    Endpoint { session: PersistedSession, expected: Identity, lifecycle: ServerLifecycle,
        machine_id: String },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct ProfilePublication {
    pub epoch: u64,
    pub profile: Option<UserRef>,
    pub scope: ProfileScope,
}

/// Credentials-owned patch data. The adapter merges it into the current disk value, retaining
/// newer favourite/search/quality/ambient fields owned by other machines.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CredentialPatch {
    pub client_id: String,
    pub account_token: String,
    pub server: crate::plex::session::ServerRef,
    pub user: UserRef,
    pub home_users: Vec<crate::plex::session::HomeUserRef>,
    pub sources: Vec<crate::plex::session::SourceRef>,
    pub profiles: Vec<crate::plex::session::ProfileCreds>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Command {
    ActivateDevBootstrap,
    /// Restore the captured single-user boot authority without re-saving its credentials.
    ResumeStored,
    StartLogin,
    Retry,
    RestartWait { phase: Phase, qr_generation: u64, reply: ReplyTo },
    StartSwitch(Picker),
    SelectProfile { index: usize, pin: Option<String> },
    SelectProfileWithReply { index: usize, pin: Option<String>, reply: ReplyTo },
    DismissPinError,
    BackAtRoot { reply: ReplyTo },
    SignOut,
    EraseLocal,
    NoteDeleteLeftovers(usize),
    RefreshRoster,
    RequestEndpoint { #[serde(with = "super::observation::server_id")] sid: crate::plex::ServerId },
    TakeReady,
    /// Answer the currently shown [`PersistenceWarning`]. A key that does not match the warning
    /// currently held is inert — it may be stale (a newer warning replaced it).
    AcknowledgePersistenceWarning { key: PersistenceWarningKey },
    /// The consent decision for the held incident, as the presenting screen derived it at
    /// `revision` (`telemetry::consent::revision`). Stale ids and unchanged revisions are inert.
    ResolveIncident { id: u32, permission: crate::telemetry::consent::Permission, revision: u32 },
    /// Send report — from the incident alert or from Details. A person's press, and the whole of
    /// the one-off report's consent.
    ReportIncident { id: u32 },
    /// Not now: the offer is answered for this launch.
    DeclineIncident { id: u32 },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct ReplyTo { pub instance: u32, pub correlation: u32 }

/// Ordered application/coordinator work; the owner describes it without invoking another
/// machine, platform API or global publication from inside its transition.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) enum CoordinatorAction {
    CloseTelemetry,
    LocalDataErased,
    SignInStarted,
    SignInCompleted,
    SignInCancelled,
    SignInFailed { phase: Phase },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum RegistryPlan {
    DevInstall { primary: crate::plex::session::ServerRef, extras: Vec<crate::plex::session::SourceRef>, client_id: String },
    /// Boot picker's avatar client, before any profile is permitted to enter Home.
    Primary { server: crate::plex::session::ServerRef, token: String },
    Activate { source: crate::plex::session::SourceRef, ipv6: bool },
    Install { sources: Vec<crate::plex::session::SourceRef>, primary: Option<usize>, replace: bool },
    Endpoint { expected: ServerLifecycle, source: crate::plex::session::SourceRef },
    Probe(super::SettledProbe),
    Revoke,
}

/// Which site's fresh save produced the warning being shown to the user — the discovery write
/// that ran right after sign-in, or the final write `take_ready` issues once discovery settles.
/// Both are `FreshReauthentication` writes; only the site differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PersistenceWarningSite { Discovery, Final }

/// Identity of the fresh write a `PersistenceWarning` is reporting on, so an acknowledgement can
/// be checked against the exact warning it is answering rather than any warning currently shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistenceWarningKey { pub epoch: u64, pub req: u32 }

/// A fresh-reauthentication write that did NOT confirm durable, surfaced to the owner/UI and held
/// until the user explicitly acknowledges it — the AUTH-03 gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistenceWarning {
    pub key: PersistenceWarningKey,
    pub site: PersistenceWarningSite,
    #[serde(default)]
    pub helper: Option<crate::storage::wire::failure::HelperFailure>,
    #[serde(default)]
    pub candidate_errnos: [Option<i32>; 8],
    /// Closed evidence for the warning; retained independently of the consent-gated report.
    #[serde(default)]
    pub persistence: Option<PersistenceEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PersistenceEvidence {
    class: crate::telemetry::incident::PersistenceFailure,
    keymanager_stage: Option<crate::storage::wire::KeymanagerStage>,
    service_error_code: Option<i32>,
}

impl PersistenceWarning {
    pub(crate) fn from_outcome(key: PersistenceWarningKey, site: PersistenceWarningSite,
        outcome: &crate::plex::session::async_persistence::CompletionOutcome) -> Self {
        let context = IncidentContext {
            kind: crate::telemetry::incident::IncidentKind::SaveFailed,
            ..IncidentContext::internal(InternalClass::CommitRefused)
        }.with_persistence(outcome);
        Self {
            key, site, helper: context.helper, candidate_errnos: context.candidate_errnos,
            persistence: context.persistence.map(|class| PersistenceEvidence {
                class, keymanager_stage: context.keymanager_stage, service_error_code: context.service_error_code,
            }),
        }
    }

    /// Both the original offer and a later explicit one-off use this same closed snapshot.
    pub(super) fn incident_context(self) -> Option<IncidentContext> {
        let evidence = self.persistence?;
        Some(IncidentContext {
            kind: crate::telemetry::incident::IncidentKind::SaveFailed,
            persistence: Some(evidence.class), helper: self.helper, candidate_errnos: self.candidate_errnos,
            keymanager_stage: evidence.keymanager_stage, service_error_code: evidence.service_error_code,
            ..IncidentContext::internal(InternalClass::CommitRefused)
        })
    }
}

/// A Ready handoff whose fresh final write has been admitted but not yet confirmed durable, or
/// confirmed NOT durable and awaiting the user's acknowledgement. Nothing is emitted for it until
/// `release_held_handoff` runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct HeldHandoff { pub epoch: u64, pub req: u32 }

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CommitPlan {
    /// Identity captured for this login, independent of the old disk identity used for OCC.
    pub registry_client_id: String,
    pub expected_disk: Identity,
    pub credentials: Option<CredentialPatch>,
    pub registry: Vec<RegistryPlan>,
    pub lifecycle: Option<ServerLifecycle>,
    /// What a durable write would be FOR. The caller uses this to refuse treating a background
    /// refresh as proof of a saved login, and the completion echoes it back.
    pub purpose: PersistencePurpose,
    /// Whether this commit asks storage for a durable write. Registry-only commits do not.
    pub writes_durable: bool,
    /// `Routine` for every ordinary write; `FreshReauthentication` only for a write the owner
    /// issues on the strength of THIS flow's own PIN authorization (a completed sign-in or
    /// device-code exchange) — never on a discovery/rediscovery retry that merely reuses it.
    pub authority: crate::plex::session::SaveAuthority,
}

/// Only the changes whose side effects are awaiting acknowledgement. This is not a second
/// controller snapshot: unchanged fields remain solely in SessionInit.
#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct CommitDelta {
    pub dev: Option<DevCommitDelta>,
    pub credentials: Option<CredentialPatch>,
    pub phase: Option<Phase>,
    pub picker: Option<Picker>,
    pub users: Option<Vec<UserTile>>,
    pub clear_error: bool,
    pub activate_profile: bool,
    pub complete_signin: bool,
    pub profile_seated: bool,
    pub ready: Option<bool>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum DevCommitDelta { Activated, StartAccount { login_req: u32 } }

/// The picker's read-outs when it has no tiles and cannot get any. Neutral wording, drawn as a
/// failed read-out on the picker itself (`screens/profiles.rs`), never as a sign-in failure.
pub(crate) const ROSTER_UNREACHABLE: &str = "Couldn\u{2019}t load profiles — check the connection.";
pub(crate) const ROSTER_REFUSED: &str = "Switching profiles isn\u{2019}t available from this profile.";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum BootstrapAuthority {
    Account { extras: Vec<crate::plex::session::SourceRef> },
    DevPms { primary: crate::plex::session::ServerRef, extras: Vec<crate::plex::session::SourceRef> },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum ReadyInstall {
    PrimaryAndExtras(Vec<crate::plex::session::SourceRef>),
    AlreadyInstalled,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PendingCommit {
    pub req: u32,
    pub epoch: u64,
    pub arrival: u64,
    pub terminal: bool,
    pub writes_credentials: bool,
    pub receipt: Option<Receipt>,
    pub delta: CommitDelta,
    /// The revision an admitted durable write consumed, if any. A completion must repeat it
    /// exactly; a completion naming any other revision belongs to a different operation.
    pub admitted_revision: Option<u64>,
    /// The purpose that admitted write was for. A background refresh is never saved-login
    /// evidence, so this is what the completion is graded against.
    pub purpose: Option<PersistencePurpose>,
    /// Whether this commit's write is on `SaveAuthority::FreshReauthentication` — a durable
    /// confirmation for one of these is held rather than announced until acknowledged.
    pub fresh: bool,
}

/// Owner-side durability state of one commit consumption. Replaces the old boolean that advertised
/// `accepted` for a registry-only commit that never asked storage for anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CommitPhase {
    /// Authority was current; a durable write was admitted and enqueued, not yet durable.
    pub(crate) admitted: bool,
    /// Storage CONFIRMED durability for the admitted revision. Still false while merely enqueued,
    /// and still false for a completion that was fenced out.
    pub(crate) durable: bool,
    /// That confirmed durable write had a purpose which may stand as evidence a login was saved.
    /// A background refresh leaves this false even when it becomes durable.
    pub(crate) proves_saved_login: bool,
}

/// The durable write this owner is waiting on, identified exactly. A completion settles only this
/// identity; anything else is a stale or superseded verdict and must change nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AdmittedPersistence {
    pub req: u32,
    pub epoch: u64,
    pub arrival: u64,
    pub revision: u64,
    /// Purpose of the admitted write, when the plan named one.
    pub purpose: Option<PersistencePurpose>,
    /// Whether the admitted write is on `SaveAuthority::FreshReauthentication`. A non-durable
    /// completion for a fresh write raises a `PersistenceWarning`; a routine one does not.
    pub fresh: bool,
    /// Which site (`Discovery` or `Final`) the fresh write belongs to, for the warning it may
    /// raise. Meaningless when `fresh` is false.
    pub site: PersistenceWarningSite,
}

/// Typed result of consuming a commit permit. The four cases are deliberately distinct: treating
/// "the authority was current" as "durable" is the defect this type exists to prevent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CommitAdmission {
    /// The permit was not current (wrong request/epoch/arrival/endpoint lifecycle). Nothing ran.
    StaleAuthority,
    /// Authority was current and the commit asked for no durable write (registry-only).
    RegistryOnly,
    /// Authority was current and a durable write was admitted and enqueued. NOT yet durable.
    Admitted { revision: u64, purpose: PersistencePurpose },
    /// Authority was current but capacity/duplicate/lock/public-only refused the write.
    Rejected { revision: Option<u64>, rejection: RejectionKind },
}

impl CommitAdmission {
    pub(crate) fn accepted(self) -> bool {
        !matches!(self, Self::StaleAuthority | Self::Rejected { .. })
    }
    pub(crate) fn admitted_revision(self) -> Option<u64> {
        match self {
            Self::Admitted { revision, .. } => Some(revision),
            _ => None,
        }
    }
    /// The purpose storage was actually asked to write for. This is the authoritative purpose for
    /// fencing a completion: the plan's intent and the admission can disagree, and only what was
    /// enqueued can be answered.
    pub(crate) fn admitted_purpose(self) -> Option<PersistencePurpose> {
        match self {
            Self::Admitted { purpose, .. } => Some(purpose),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct CommitReply {
    pub req: u32,
    pub epoch: u64,
    pub arrival: u64,
    pub admission: CommitAdmission,
}

/// Execution-time permission borrowed from the sole owner. Not serialized state or an effect:
/// keeping it alive prevents changing the owner while its separate adapter commits resources.
pub(crate) struct CommitPermit<'a> {
    req: u32,
    epoch: u64,
    arrival: u64,
    owner: std::marker::PhantomData<&'a SessionMachine>,
}

impl CommitPermit<'_> {
    pub fn request(&self) -> u32 { self.req }
    /// The epoch and arrival this permit was issued for. The adapter needs them so the durability
    /// verdict it reports can be fenced against the exact operation that produced it.
    pub fn epoch(&self) -> u64 { self.epoch }
    pub fn arrival(&self) -> u64 { self.arrival }
    pub fn reply(self, admission: CommitAdmission) -> CommitReply {
        CommitReply { req: self.req, epoch: self.epoch, arrival: self.arrival, admission }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum CaptureIntent {
    Login,
    Profile { tile: UserTile, pin: Option<String> },
    Endpoint { sid: u16 },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) enum SessionReadRequest {
    LoginClientId,
    ProfilePolicy,
    Endpoint { sid: u16 },
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum SessionReadValue {
    LoginClientId(String),
    ProfilePolicy { recently_unreachable: bool },
    Endpoint(Option<EndpointCapture>),
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct EndpointCapture {
    pub lifecycle: ServerLifecycle,
    pub machine_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SessionReadReply {
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub epoch: u64,
    pub value: SessionReadValue,
}

/// One launch per checked request. The explicit result state distinguishes "accepted, no
/// observations yet" from a never-admitted request; an arrival watermark cannot do that.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AdmissionId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum AdmissionState {
    NotRequested,
    Awaiting(AdmissionId),
    Accepted(AdmissionId),
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct AdmissionReply {
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub key: SessionWorkKey,
    pub correlation: AdmissionId,
    pub accepted: bool,
}

/// Every variant is data. Closures, native Clients and MainThread cannot enter logical effects.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum SessionFx {
    Commit { req: u32, epoch: u64, arrival: u64, plan: CommitPlan },
    Pump,
    Acknowledge(Vec<Receipt>),
    Retire { req: u32 },
    Ready { epoch: u64, scope: ProfileScope, server: crate::plex::session::ServerRef, token: String, install: ReadyInstall },
    Capture { req: u32, epoch: u64, request: SessionReadRequest },
    Work { req: u32, key: SessionWorkKey, admission: AdmissionId, input: SessionWork },
    Cancel { requests: Vec<u32>, epoch: u64 },
    PublishProfile(ProfilePublication),
    Erase { req: u32, epoch: u64, all_local: bool },
    Coordinator(CoordinatorAction),
    RestartReply { to: ReplyTo, accepted: bool },
    /// Queue one incident report on `lane`. The adapter answers with
    /// [`SessionEvent::IncidentReported`] carrying the same `id`.
    Incident { id: u32, lane: IncidentLane, report: IncidentReport },
    SelectionReply { to: ReplyTo, accepted: bool, flow_epoch: u64 },
    BackReply { to: ReplyTo, resumed: bool },
}

pub(crate) trait SessionHost: crate::ui::machine::Host {
    fn session_effect(effect: SessionFx) -> Self::Fx;
}

pub(crate) enum SessionEvent {
    Command(Command),
    Result(SessionEnvelope),
    Commit(CommitReply),
    /// Resource evidence is delivered even when its original authority permit is gone.
    DiskWrite { before: Identity, after: Identity,
        outcome: crate::plex::session::async_persistence::CompletionOutcome },
    Read(SessionReadReply),
    Pump,
    Admission(AdmissionReply),
    /// A typed durability verdict from the persistence worker, delivered through the ordinary
    /// effect/FIFO path. It is fenced by request/epoch/arrival/revision before it settles anything.
    Persistence(crate::plex::session::async_persistence::PersistenceCompletion),
    Erased { epoch: u64, leftovers: usize },
    /// What became of a [`SessionFx::Incident`]; fenced by the offer's id.
    IncidentReported { id: u32, delivery: IncidentDelivery },
}

impl CredentialPatch {
    pub fn identity(&self) -> Identity {
        Identity { client_id: self.client_id.clone(), account_token: self.account_token.clone(),
            profile_uuid: self.user.uuid.clone() }
    }
    pub fn of(s: &PersistedSession) -> Self {
        Self { client_id: s.client_id.clone(), account_token: s.account_token.clone(),
            server: s.server.clone(), user: s.user.clone(), home_users: s.home_users.clone(),
            sources: s.sources.clone(), profiles: s.profiles.clone() }
    }
    pub fn merge_into(&self, disk: &PersistedSession) -> PersistedSession {
        PersistedSession { client_id: self.client_id.clone(), account_token: self.account_token.clone(),
            server: self.server.clone(), user: self.user.clone(), home_users: self.home_users.clone(),
            sources: self.sources.clone(), profiles: self.profiles.clone(), ..disk.clone() }
    }
}

/// Serializable identity of a registry resource, never the native Client pointer.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServerLifecycle {
    pub sid: u16,
    pub instance_gen: u32,
    pub token_gen: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum SessionOp { Login, Rediscover, HomeRoster, ServerRoster, ProfileSwitch, Endpoint(u16), Ready, Picker, DevBoundary }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionWorkKey {
    pub epoch: u64,
    pub op: SessionOp,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    pub client_id: String,
    pub account_token: String,
    pub profile_uuid: String,
}

impl Identity {
    pub fn of(session: &PersistedSession) -> Self {
        Self { client_id: session.client_id.clone(), account_token: session.account_token.clone(),
            profile_uuid: session.user.uuid.clone() }
    }
    pub fn matches(&self, session: &PersistedSession) -> bool {
        self.client_id == session.client_id && self.account_token == session.account_token
            && self.profile_uuid == session.user.uuid
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum StreamPhase { Running, ProfileSeated }

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub key: SessionWorkKey,
    pub expected: Identity,
    pub lifecycle: Option<ServerLifecycle>,
    pub last_arrival: Option<u64>,
    pub phase: StreamPhase,
    pub capture: Option<CaptureIntent>,
    pub admission: AdmissionState,
}

/// Only the owner may allocate this generation; adapters publish the supplied value verbatim.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProfileScope(pub u32);

/// Private persisted init data, not diagnostic output. No constructor consults the filesystem.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SessionInit {
    pub authority: BootstrapAuthority,
    /// Comparison-only disk baseline; NEVER an input to workers, profile selection or BACK.
    pub disk_identity: Identity,
    pub phase: Phase,
    pub picker: Picker,
    pub persisted: PersistedSession,
    /// Last accepted credential commit, distinct from a QR token or profile candidate which has
    /// not been saved yet. BACK may resume this version; it never resumes a provisional token.
    pub committed_credentials: CredentialPatch,
    pub pin_code: String,
    pub qr_png: Vec<u8>,
    pub users: Vec<UserTile>,
    pub error: String,
    pub pin_denied: bool,
    pub authorized_in_flow: bool,
    pub signin_active: bool,
    pub apply_pending: bool,
    pub code_replaced: bool,
    pub qr_gen: u64,
    pub next_qr: u64,
    pub epoch: u64,
    pub next_req: u32,
    pub pending: BTreeMap<u32, Pending>,
    pub pending_commit: Option<PendingCommit>,
    /// Purpose of the commit currently being resolved, if it asked for durability.
    pub persistence_purpose: Option<PersistencePurpose>,
    /// Durability state of the last consumed commit.
    pub commit_phase: CommitPhase,
    /// Identity of the durable write this owner is waiting to hear about. Cleared once a matching
    /// completion settles it, or superseded by a newer admission. Only this identity can settle it.
    pub admitted_persistence: Option<AdmittedPersistence>,
    /// A fresh write that did not confirm durable, held for acknowledgement (AUTH-03). While
    /// present, `take_ready` refuses to issue a second fresh write over it.
    pub persistence_warning: Option<PersistenceWarning>,
    /// A Ready handoff whose fresh write is admitted but not yet released to the owner/UI.
    pub held_handoff: Option<HeldHandoff>,
    /// Whether THIS authorization has already had one unsaved-login (AUTH-03) warning answered.
    /// Authorization succeeding and credentials being savable are different conditions: once the
    /// user has answered for this authorization, a later storage failure (Discovery then Final,
    /// or a retry) must release its own held handoff rather than raise the same question again.
    /// Per-authorization, so it is cleared everywhere `persistence_warning`/`held_handoff` reset
    /// together — a new sign-in must start with a fresh, unanswered question.
    pub persistence_warning_answered: bool,
    /// The disk identity a fresh credential write replaced at admission, until that write proves
    /// durable. A definite failure restores it: the record never changed, and fencing the next
    /// fresh write on the identity that never landed would refuse every retry this run.
    #[serde(default)]
    pub unconfirmed_fresh_prior: Option<Identity>,
    /// One ordered erase awaiting resource completion: existing epoch and whether to sign in.
    pub pending_erase: Option<(u64, bool)>,
    pub inbox: VecDeque<SessionEnvelope>,
    /// Covers both the queued Fx::Pump and its eventual typed delivery. Cancellation does not
    /// forget a marker already in the dispatcher; it can safely pump a newer current inbox.
    pub pump_pending: bool,
    pub active_profile: Option<UserRef>,
    pub profile_scope: ProfileScope,
    pub delete_leftovers: usize,
    /// The onboarding incident being offered or reported, if any — see `owner::incident`.
    #[serde(default)]
    pub incident: Option<IncidentOffer>,
    /// Every incident key already resolved this launch: none of them is raised again.
    #[serde(default)]
    pub incidents_seen: Vec<IncidentKey>,
    /// The last incident id handed out. Never reused within a launch.
    #[serde(default)]
    pub next_incident: u32,
    /// plex.tv has left at least two consecutive polls of the code on screen unanswered.
    #[serde(default)]
    pub link_trouble: bool,
}

impl SessionInit {
    pub fn captured(persisted: PersistedSession) -> Self {
        let committed_credentials = CredentialPatch::of(&persisted);
        let disk_identity = Identity::of(&persisted);
        Self { authority: BootstrapAuthority::Account { extras: Vec::new() }, disk_identity,
            phase: Phase::Idle, picker: Picker::Boot, persisted, committed_credentials,
            pin_code: String::new(), qr_png: Vec::new(), users: Vec::new(), error: String::new(),
            pin_denied: false, authorized_in_flow: false, signin_active: false, apply_pending: false,
            code_replaced: false, qr_gen: 0, next_qr: 0, epoch: 1, next_req: 0,
            pending: BTreeMap::new(), pending_commit: None, persistence_purpose: None,
            commit_phase: CommitPhase { admitted: false, durable: false, proves_saved_login: false }, admitted_persistence: None,
            persistence_warning: None, held_handoff: None, persistence_warning_answered: false,
            unconfirmed_fresh_prior: None, pending_erase: None, inbox: VecDeque::new(), pump_pending: false,
            active_profile: None, profile_scope: ProfileScope(0),
            delete_leftovers: 0, incident: None, incidents_seen: Vec::new(), next_incident: 0,
            link_trouble: false }
    }

    pub fn captured_boot(saved: PersistedSession, primary: Option<crate::plex::session::ServerRef>,
        extras: Vec<crate::plex::session::SourceRef>) -> Self {
        let mut init = Self::captured(saved);
        if let Some(primary) = primary {
            let clean = CredentialPatch::of(&PersistedSession {
                client_id: init.persisted.client_id.clone(), ..Default::default()
            });
            init.persisted = clean.merge_into(&init.persisted);
            init.committed_credentials = clean;
            init.authority = BootstrapAuthority::DevPms { primary, extras };
        } else { init.authority = BootstrapAuthority::Account { extras }; }
        init
    }
}

pub(super) fn write_incident_context(w: &mut Canon, context: &crate::telemetry::incident::IncidentContext) {
    incident::write_context(w, context);
}

pub(super) fn write_user(w: &mut Canon, user: &UserRef) {
    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb).str(&user.token);
    w.option(user.plex_tv_token.as_ref(), |w, t| { w.str(t); });
}

pub(super) fn write_tile(w: &mut Canon, user: &UserTile) {
    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb)
        .bool(user.protected).bool(user.admin);
}

pub(super) fn write_profile(w: &mut Canon, profile: &crate::plex::session::ProfileCreds) {
    w.str(&profile.uuid);
    write_user(w, &profile.user);
    write_server(w, &profile.server);
    write_sources(w, &profile.sources);
    w.option(profile.pin.as_ref(), |w, pin| { w.str(&pin.salt).str(&pin.hash).u32(pin.iters); });
}

pub(super) fn write_server(w: &mut Canon, server: &crate::plex::session::ServerRef) {
    w.str(&server.name).str(&server.machine_id).str(&server.address)
        .u64(server.port as u64).str(&server.token).str(&server.origin_url);
    write_tier(w, server.tier);
}

pub(super) fn write_tier(w: &mut Canon, tier: Option<crate::plex::probe::Location>) {
    use crate::plex::probe::Location;
    w.u8(match tier { None => 0, Some(Location::Local) => 1,
        Some(Location::Remote) => 2, Some(Location::Relay) => 3 });
}

pub(super) fn write_sources(w: &mut Canon, sources: &[crate::plex::session::SourceRef]) {
    w.seq(sources.len());
    for s in sources {
        // The carried household EVIDENCE joins the canonical digest beside raw `owned`. It has to:
        // a state whose only difference is that plex.tv now names the grant's owner is a different
        // state, and a digest blind to it would grade the correction as no change at all.
        w.str(&s.machine_id).str(&s.name).str(&s.shared_by).bool(s.owned)
            .bool(s.home).u64(s.owner_id as u64)
            .str(&s.address).u64(s.port as u64).str(&s.token).str(&s.origin_url);
        write_tier(w, s.tier);
    }
}

/// Explicit field encoding. Serde is only the private init round-trip, never the state hash.
pub(super) fn write_persisted(w: &mut Canon, s: &PersistedSession) {
    w.str(&s.client_id).str(&s.account_token);
    write_server(w, &s.server);
    write_user(w, &s.user);
    write_sources(w, &s.sources);
    w.seq(s.home_users.len());
    for user in &s.home_users {
        w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb)
            .bool(user.protected).bool(user.admin);
    }
    w.seq(s.profiles.len());
    for profile in &s.profiles { write_profile(w, profile); }
    // Preferences are captured input retained by the controller, not authority for disk writes.
    // Include their exact captured values in init/canonical state even though patches never write
    // them over a newer store-owned value.
    w.bool(s.auto_sign_in());
    w.seq(s.recent_searches.len());
    for recent in &s.recent_searches {
        w.str(&recent.user).seq(recent.terms.len());
        for term in &recent.terms { w.str(term); }
    }
    w.seq(s.last_library.len());
    for last in &s.last_library {
        w.str(&last.user).seq(last.libs.len());
        for lib in &last.libs { w.str(&lib.kind).str(&lib.machine_id).u64(lib.key as u64); }
    }
    w.seq(s.home_pins.len());
    for pins in &s.home_pins {
        w.str(&pins.user).bool(pins.asked);
        for list in [&pins.on, &pins.off] {
            w.seq(list.len());
            for lib in list { w.str(&lib.machine_id).u64(lib.key as u64); }
        }
    }
    w.option(s.last_hero_blur, |w, blur| {
        for corner in blur { for channel in corner { w.f32(channel); } }
    });
    w.option(s.playback_quality, |w, quality| {
        use crate::plex::session::PlaybackQuality;
        w.u8(match quality { PlaybackQuality::Auto => 0, PlaybackQuality::Original => 1,
            PlaybackQuality::P1080High => 2, PlaybackQuality::P1080 => 3,
            PlaybackQuality::P720 => 4, PlaybackQuality::P720Low => 5, PlaybackQuality::P480 => 6 });
    });
}

impl LogicalState for SessionInit {
    fn write(&self, w: &mut Canon) {
        w.str(&self.disk_identity.client_id).str(&self.disk_identity.account_token)
            .str(&self.disk_identity.profile_uuid);
        match &self.authority {
            BootstrapAuthority::Account { extras } => { w.u8(0); write_sources(w, extras); }
            BootstrapAuthority::DevPms { primary, extras } => {
                w.u8(1); write_server(w, primary); write_sources(w, extras);
            }
        }
        w.u8(match self.phase { Phase::Idle => 0, Phase::Creating => 1, Phase::Waiting => 2,
            Phase::Discovering => 3, Phase::Profiles => 4, Phase::Switching => 5,
            Phase::Ready => 6, Phase::Error => 7, Phase::Deleted => 8 });
        w.u8(match self.picker { Picker::Boot => 0, Picker::SignedIn => 1, Picker::ChangeProfile => 2 });
        write_persisted(w, &self.persisted);
        write_persisted(w, &self.committed_credentials.merge_into(&PersistedSession::default()));
        w.str(&self.pin_code).seq(self.qr_png.len());
        for byte in &self.qr_png { w.u8(*byte); }
        w.seq(self.users.len());
        for user in &self.users {
            w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb)
                .bool(user.protected).bool(user.admin);
        }
        w.str(&self.error).bool(self.pin_denied).bool(self.authorized_in_flow)
            .bool(self.signin_active).bool(self.apply_pending).bool(self.code_replaced)
            .u64(self.qr_gen).u64(self.next_qr).u64(self.epoch).u32(self.next_req)
            .u64(self.delete_leftovers as u64).u32(self.profile_scope.0);
        w.option(self.active_profile.as_ref(), write_user);
        w.seq(self.pending.len());
        for (&req, pending) in &self.pending {
            w.u32(req).u64(pending.key.epoch);
            write_op(w, pending.key.op);
            w.str(&pending.expected.client_id).str(&pending.expected.account_token)
                .str(&pending.expected.profile_uuid);
            w.option(pending.lifecycle, |w, life| { w.u32(life.sid as u32)
                .u32(life.instance_gen).u32(life.token_gen); });
            w.option(pending.last_arrival, |w, arrival| { w.u64(arrival); });
            w.u8(match pending.phase { StreamPhase::Running => 0, StreamPhase::ProfileSeated => 1 });
            match pending.admission {
                AdmissionState::NotRequested => { w.u8(0); }
                AdmissionState::Awaiting(id) => { w.u8(1).u32(id.0); }
                AdmissionState::Accepted(id) => { w.u8(2).u32(id.0); }
            }
            w.option(pending.capture.as_ref(), |w, capture| match capture {
                CaptureIntent::Login => { w.u8(0); }
                CaptureIntent::Profile { tile, pin } => {
                    w.u8(1).u64(tile.id as u64).str(&tile.uuid).str(&tile.title).str(&tile.thumb)
                        .bool(tile.protected).bool(tile.admin);
                    w.option(pin.as_ref(), |w, pin| { w.str(pin); });
                }
                CaptureIntent::Endpoint { sid } => { w.u8(2).u32(u32::from(*sid)); }
            });
        }
        w.option(self.persistence_purpose, |w, purpose| { write_purpose(w, purpose); });
        w.bool(self.commit_phase.admitted);
        w.bool(self.commit_phase.durable);
        w.bool(self.commit_phase.proves_saved_login);
        w.option(self.persistence_warning.as_ref(), |w, warning| {
            w.u64(warning.key.epoch).u32(warning.key.req).u8(match warning.site {
                PersistenceWarningSite::Discovery => 0,
                PersistenceWarningSite::Final => 1,
            });
            w.option(warning.persistence, |w, evidence| { w.str(&serde_json::to_string(&evidence).unwrap()); });
            w.option(warning.helper, |w, helper| { w.str(&serde_json::to_string(&helper).unwrap()); });
            for errno in warning.candidate_errnos { w.option(errno, |w, n| { w.u32(n as u32); }); }
        });
        w.option(self.held_handoff.as_ref(), |w, held| { w.u64(held.epoch).u32(held.req); });
        w.bool(self.persistence_warning_answered);
        w.option(self.pending_commit.as_ref(), |w, commit| {
            w.u32(commit.req).u64(commit.epoch).u64(commit.arrival).bool(commit.terminal)
                .bool(commit.writes_credentials);
            w.option(commit.receipt.as_ref(), |w, receipt| receipt.write(w));
            let delta = &commit.delta;
            w.option(delta.dev.as_ref(), |w, dev| { match dev {
                DevCommitDelta::Activated => { w.u8(0); }
                DevCommitDelta::StartAccount { login_req } => { w.u8(1).u32(*login_req); }
            } });
            w.option(delta.credentials.as_ref(), |w, patch| {
                write_persisted(w, &patch.merge_into(&PersistedSession::default()));
            });
            w.option(delta.phase, |w, phase| { w.u8(match phase {
                Phase::Idle => 0, Phase::Creating => 1, Phase::Waiting => 2,
                Phase::Discovering => 3, Phase::Profiles => 4, Phase::Switching => 5,
                Phase::Ready => 6, Phase::Error => 7, Phase::Deleted => 8,
            }); });
            w.option(delta.picker, |w, picker| { w.u8(match picker {
                Picker::Boot => 0, Picker::SignedIn => 1, Picker::ChangeProfile => 2,
            }); });
            w.option(delta.users.as_ref(), |w, users| {
                w.seq(users.len());
                for user in users {
                    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb)
                        .bool(user.protected).bool(user.admin);
                }
            });
            w.bool(delta.clear_error).bool(delta.activate_profile).bool(delta.complete_signin)
                .bool(delta.profile_seated);
            w.option(delta.ready, |w, ready| { w.bool(ready); });
        });
        w.option(self.pending_erase, |w, (epoch, sign_in)| { w.u64(epoch).bool(sign_in); });
        w.bool(self.pump_pending).seq(self.inbox.len());
        for envelope in &self.inbox { envelope.write(w); }
        w.option(self.incident.as_ref(), incident::write_offer);
        w.seq(self.incidents_seen.len());
        for key in &self.incidents_seen { incident::write_key(w, key); }
        w.u32(self.next_incident).bool(self.link_trouble);
    }
    fn probe(&self, out: &mut String) {
        use std::fmt::Write;
        let _ = write!(out, "session phase={:?} requests={} scope={}",
            self.phase, self.pending.len(), self.profile_scope.0);
    }
}

impl LogicalState for SessionMachine {
    fn write(&self, w: &mut Canon) { self.state.write(w); }
    fn probe(&self, out: &mut String) { self.state.probe(out); }
}

/// Derived UI facts only. Credentials and writable controller state are not part of the view.
#[derive(PartialEq, Eq)]
pub(crate) struct SessionSnapshot {
    pub flow_epoch: u64,
    pub phase: Phase,
    pub qr_generation: u64,
    pub code: Arc<str>,
    pub png: Arc<[u8]>,
    pub code_replaced: bool,
    pub users: Arc<[UserTile]>,
    pub error: Arc<str>,
    pub pin_denied: bool,
    pub profile: Option<ProfileRead>,
    pub scope: ProfileScope,
    pub delete_leftovers: usize,
    pub persistence_warning: Option<PersistenceWarning>,
    /// The onboarding incident being offered or reported — see `owner::incident`.
    pub incident: Option<IncidentOffer>,
    /// plex.tv is not answering the polls of the code on screen.
    pub link_trouble: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProfileRead {
    pub uuid: String,
    pub title: String,
    pub thumb: String,
}

#[derive(Clone, Copy)]
pub(crate) struct SessionRead<'a>(pub &'a SessionSnapshot);

/// [`SessionSnapshot::roster_readout`]'s condition, for a reader that holds the fields rather
/// than the snapshot (the Profiles screen keeps its own copies between frames).
pub(crate) fn is_roster_readout(phase: Phase, users: &[UserTile], error: &str) -> bool {
    phase == Phase::Profiles && users.is_empty() && !error.is_empty()
}

/// The log line owed when one session step moves the publication INTO the roster read-out
/// (#132), or `None`. The owner logs nothing itself; the bridge, which runs every session step,
/// writes this between the publication before and after that step. It is decided HERE — at the
/// publication boundary — and not by the Profiles screen, because the screen only ever sees the
/// state it was mounted on: a dev-token Change profile is refused synchronously, so the read-out
/// already exists at mount and a screen-side "it just changed" gate never fires (TV, PR #212).
/// A worker's failure line carries the HTTP status; this one carries what the person is told,
/// and fires on every path in, including the ones that made no request at all.
pub(crate) fn roster_readout_entered(before: &SessionSnapshot, after: &SessionSnapshot) -> Option<String> {
    let reason = after.roster_readout()?;
    if before.roster_readout() == Some(reason) { return None; }
    Some(format!("profiles: no profiles to offer — {reason} (BACK returns)"))
}

impl SessionSnapshot {
    pub fn read(&self) -> SessionRead<'_> { SessionRead(self) }
    /// **#132's read-out**: a picker with nobody to offer and a reason why. The ONE predicate —
    /// the Profiles screen draws it, stops its spinner on it, and [`roster_readout_entered`]
    /// announces it; three copies of the condition are how a screen ends up still spinning under
    /// a read-out it drew.
    pub fn roster_readout(&self) -> Option<&str> {
        is_roster_readout(self.phase, &self.users, &self.error).then_some(&*self.error)
    }
    fn from_state(state: &SessionInit) -> Self {
        Self::from_state_counted(state, None, &mut |_| {})
    }
    fn matches_state(&self, state: &SessionInit) -> bool {
        self.flow_epoch == state.epoch && self.phase == state.phase && self.qr_generation == state.qr_gen
            && &*self.code == state.pin_code.as_str() && &*self.png == state.qr_png.as_slice()
            && &*self.users == state.users.as_slice() && &*self.error == state.error.as_str()
            && self.code_replaced == state.code_replaced && self.pin_denied == state.pin_denied
            && self.scope == state.profile_scope && self.delete_leftovers == state.delete_leftovers
            && self.persistence_warning == state.persistence_warning
            && self.incident == state.incident && self.link_trouble == state.link_trouble
            && match (&self.profile, &state.active_profile) {
                (None, None) => true,
                (Some(read), Some(profile)) => read.uuid == profile.uuid
                    && read.title == profile.title && read.thumb == profile.thumb,
                _ => false,
            }
    }
    fn from_state_counted(state: &SessionInit, previous: Option<&Self>, built: &mut impl FnMut(usize)) -> Self {
        let code = previous.filter(|old| &*old.code == state.pin_code.as_str())
            .map(|old| Arc::clone(&old.code)).unwrap_or_else(|| {
                built(0); Arc::from(state.pin_code.as_str())
            });
        let png = previous.filter(|old| &*old.png == state.qr_png.as_slice())
            .map(|old| Arc::clone(&old.png)).unwrap_or_else(|| {
                built(1); Arc::from(state.qr_png.as_slice())
            });
        let users = previous.filter(|old| &*old.users == state.users.as_slice())
            .map(|old| Arc::clone(&old.users)).unwrap_or_else(|| {
                built(2); Arc::from(state.users.as_slice())
            });
        let error = previous.filter(|old| &*old.error == state.error.as_str())
            .map(|old| Arc::clone(&old.error)).unwrap_or_else(|| Arc::from(state.error.as_str()));
        Self { flow_epoch: state.epoch, phase: state.phase, qr_generation: state.qr_gen,
            code, png, code_replaced: state.code_replaced, users,
            error, pin_denied: state.pin_denied,
            profile: state.active_profile.as_ref().map(|p| ProfileRead {
                uuid: p.uuid.clone(), title: p.title.clone(), thumb: p.thumb.clone(),
            }), scope: state.profile_scope, delete_leftovers: state.delete_leftovers,
            persistence_warning: state.persistence_warning, incident: state.incident.clone(),
            link_trouble: state.link_trouble }
    }
}

pub(crate) struct SessionMachine {
    state: SessionInit,
    publication: Arc<SessionSnapshot>,
    subhash: u64,
    logical_dirty: bool,
    #[cfg(test)]
    publication_payload_allocations: [usize; 3],
}

impl SessionMachine {
    pub fn from_init(state: SessionInit) -> Self {
        assert!(state.inbox.len() + usize::from(state.pending_commit.as_ref().is_some_and(|c| c.receipt.is_some()))
            <= SESSION_TRANSFER_RECORDS, "invalid Session transfer state");
        let mut canon = Canon::new();
        state.write(&mut canon);
        let subhash = canon.finish();
        let publication = Arc::new(SessionSnapshot::from_state(&state));
        Self { state, publication, subhash, logical_dirty: true,
            #[cfg(test)] publication_payload_allocations: [0; 3] }
    }
    // Required controlled-init boundary; full AppInit capture remains a following stage.
    pub fn snapshot_init(&self) -> SessionInit { self.state.clone() }
    pub fn read(&self) -> SessionRead<'_> { self.publication.read() }
    pub fn publication(&self) -> Arc<SessionSnapshot> { Arc::clone(&self.publication) }
    pub fn subhash(&self) -> u64 { self.subhash }
    // Cached hashes already change on logical transitions; aggregate dirty consumption is
    // separate from UI Arc damage and remains a following-stage App recording integration.
    #[cfg_attr(not(test), expect(dead_code, reason = "aggregate App logical-dirty consumer is not integrated yet"))]
    pub fn take_logical_dirty(&mut self) -> bool { std::mem::take(&mut self.logical_dirty) }

    fn refresh_subhash(&mut self) {
        let mut canon = Canon::new();
        self.state.write(&mut canon);
        let next = canon.finish();
        if next != self.subhash {
            self.subhash = next;
            self.logical_dirty = true;
        }
    }

    pub fn ready_is_current(&self, epoch: u64, scope: ProfileScope) -> bool {
        self.state.epoch == epoch && self.state.profile_scope == scope
            && self.state.phase == Phase::Ready && !self.state.apply_pending
    }

    pub fn needs_ready_commit(&self) -> bool {
        self.state.phase == Phase::Ready && self.state.apply_pending && self.state.pending_commit.is_none()
            && self.state.persistence_warning.is_none()
    }

    pub fn publication_is_current(&self, publication: &ProfilePublication) -> bool {
        publication.epoch == self.state.epoch && publication.scope == self.state.profile_scope
    }

    pub fn read_is_current(&self, req: u32, epoch: u64, request: SessionReadRequest) -> bool {
        if epoch != self.state.epoch { return false; }
        let Some(pending) = self.state.pending.get(&req) else { return false };
        if pending.key.epoch != epoch || !pending.expected.matches(&self.state.persisted) { return false; }
        match (&pending.capture, request) {
            (Some(CaptureIntent::Login), SessionReadRequest::LoginClientId)
            | (Some(CaptureIntent::Profile { .. }), SessionReadRequest::ProfilePolicy) => true,
            (Some(CaptureIntent::Endpoint { sid: a }), SessionReadRequest::Endpoint { sid: b }) => *a == b,
            _ => false,
        }
    }

    /// Physical completion remains evidence even after its authority permit was superseded.
    /// Advance only the comparison fence; never publish obsolete credentials as trusted state.
    fn observe_disk_write(&mut self, before: &Identity, after: &Identity,
        outcome: crate::plex::session::async_persistence::CompletionOutcome) -> bool {
        use crate::plex::session::async_persistence::{CompletionOutcome, Operation};
        if self.state.disk_identity != *before || before == after
            || !matches!(outcome, CompletionOutcome::Durable(Operation::Write { .. })) { return false; }
        self.state.disk_identity = after.clone();
        true
    }

    /// Read-only authorization at effect execution, after any carried cancellation command.
    /// The bridge checks before admission and again when the worker completes. Supersession
    /// cancels pending work; a write already completed still reports its physical disk evidence.
    pub fn commit_is_current(&self, req: u32, epoch: u64, arrival: u64) -> bool {
        self.state.epoch == epoch
            && self.state.pending.contains_key(&req)
            && self.state.pending_commit.as_ref().is_some_and(|commit|
                commit.req == req && commit.epoch == epoch && commit.arrival == arrival)
    }

    pub fn commit_permit(&self, req: u32, epoch: u64, arrival: u64) -> Option<CommitPermit<'_>> {
        self.commit_is_current(req, epoch, arrival).then_some(CommitPermit {
            req, epoch, arrival, owner: std::marker::PhantomData,
        })
    }

    /// Bridge wiring hook for Stage B: the caller that owns the decision can name the purpose
    /// explicitly instead of inheriting a default. The adapter reaches the plan's own purpose
    /// through [`CommitPlan`], so this is not yet called inside this crate.
    #[allow(dead_code)]
    pub fn commit_permit_for(&self, req: u32, epoch: u64, arrival: u64) -> Option<CommitPermit<'_>> {
        self.commit_is_current(req, epoch, arrival).then_some(CommitPermit {
            req, epoch, arrival, owner: std::marker::PhantomData,
        })
    }

    fn begin_commit(&mut self, req: u32, arrival: u64, terminal: bool,
        plan: CommitPlan, delta: CommitDelta, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.pending_commit.is_some() { return false; }
        let Some(pending) = self.state.pending.get_mut(&req) else { return false };
        if pending.key.epoch != self.state.epoch { return false; }
        pending.last_arrival = Some(arrival);
        let epoch = pending.key.epoch;
        let fresh = plan.writes_durable && plan.credentials.is_some()
            && plan.authority == crate::plex::session::SaveAuthority::FreshReauthentication;
        self.state.pending_commit = Some(PendingCommit { req, epoch, arrival, terminal,
            writes_credentials: plan.writes_durable && plan.credentials.is_some(),
            receipt: None, delta,
            admitted_revision: None,
            purpose: plan.writes_durable.then_some(plan.purpose),
            fresh });
        self.state.persistence_purpose = Some(plan.purpose);
        emit(SessionFx::Commit { req, epoch, arrival, plan });
        true
    }

    fn take_ready(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.phase != Phase::Ready || !self.state.apply_pending
            || self.state.pending_commit.is_some() { return false; }
        // A discovery failure left a warning unacknowledged: the final fresh write must not run
        // over it (AUTH-03) — acknowledging is what re-admits `needs_ready_commit`.
        if self.state.persistence_warning.is_some() { return false; }
        let Some(req) = self.allocate(SessionOp::Ready, None) else {
            self.state.phase = Phase::Profiles;
            self.state.apply_pending = false;
            self.state.error = "Couldn't switch profile. Try again.".into();
            self.replace_publication();
            return true;
        };
        let mut next = self.state.persisted.clone();
        super::remember_unprotected_active(&mut next);
        let patch = CredentialPatch::of(&next);
        // Consumed exactly once: a discovery/rediscovery retry only PEEKS this flag
        // (`apply_resource_observation`), so it cannot spend the authority the final write needs.
        let authority = if std::mem::take(&mut self.state.authorized_in_flow) {
            crate::plex::session::SaveAuthority::FreshReauthentication
        } else { crate::plex::session::SaveAuthority::Routine };
        let plan = CommitPlan { registry_client_id: self.state.persisted.client_id.clone(),
            expected_disk: self.state.disk_identity.clone(),
            credentials: Some(patch.clone()), lifecycle: None,
            registry: vec![RegistryPlan::Install { sources: next.sources.clone(), primary: None, replace: false }],
            purpose: PersistencePurpose::Final, writes_durable: true, authority };
        self.begin_commit(req, 0, true, plan, CommitDelta {
            credentials: Some(patch), activate_profile: true, ready: Some(false),
            ..Default::default()
        }, emit)
    }

    fn activate_dev(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.phase != Phase::Idle || self.state.pending_commit.is_some()
            || self.state.pending_erase.is_some() { return false; }
        let BootstrapAuthority::DevPms { primary, extras } = &self.state.authority else { return false };
        let registry = vec![RegistryPlan::DevInstall { primary: primary.clone(), extras: extras.clone(),
            client_id: self.state.persisted.client_id.clone() }];
        let Some(req) = self.allocate(SessionOp::DevBoundary, None) else { return false };
        self.begin_commit(req, 0, true, CommitPlan { registry_client_id: self.state.persisted.client_id.clone(),
            expected_disk: self.state.disk_identity.clone(),
            credentials: None, lifecycle: None, registry,
            purpose: PersistencePurpose::Background, writes_durable: false,
            authority: crate::plex::session::SaveAuthority::Routine }, CommitDelta {
                dev: Some(DevCommitDelta::Activated), ..Default::default()
            }, emit)
    }

    fn begin_dev_account(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.pending_erase.is_some() || self.state.epoch.checked_add(1).is_none()
            || self.state.next_req.checked_add(2).is_none()
            || self.state.pending_commit.as_ref().is_some_and(|c|
                matches!(c.delta.dev, Some(DevCommitDelta::StartAccount { .. }))) { return false; }
        self.advance_epoch(emit).expect("dev login epoch preflight");
        self.publish_profile(None, emit);
        self.state.phase = Phase::Creating;
        self.state.apply_pending = false;
        self.state.error.clear();
        let req = self.allocate(SessionOp::DevBoundary, None).expect("two-slot preflight");
        let login_req = self.allocate(SessionOp::Login, None).expect("two-slot preflight");
        self.begin_commit(req, 0, true, CommitPlan { registry_client_id: self.state.persisted.client_id.clone(),
            expected_disk: self.state.disk_identity.clone(),
            credentials: None, lifecycle: None, registry: vec![RegistryPlan::Revoke],
            purpose: PersistencePurpose::Background, writes_durable: false,
            authority: crate::plex::session::SaveAuthority::Routine }, CommitDelta {
                dev: Some(DevCommitDelta::StartAccount { login_req }), ..Default::default()
            }, emit);
        self.replace_publication();
        true
    }

    fn start_reserved_login(&mut self, req: u32, emit: &mut impl FnMut(SessionFx)) {
        let client_id = self.state.persisted.client_id.clone();
        if client_id.is_empty() {
            self.state.pending.get_mut(&req).expect("reserved login").capture = Some(CaptureIntent::Login);
            emit(SessionFx::Capture { req, epoch: self.state.epoch, request: SessionReadRequest::LoginClientId });
        } else { self.emit_work(req, SessionWork::Login { client_id }, emit); }
    }

    fn resume_stored(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool {
        if !matches!(self.state.authority, BootstrapAuthority::Account { .. }) { return false; }
        if self.state.phase != Phase::Idle || !self.state.persisted.can_go_local()
            || self.state.pending_commit.is_some() { return false; }
        let Some(req) = self.allocate(SessionOp::Ready, None) else { return false };
        let plan = CommitPlan { registry_client_id: self.state.persisted.client_id.clone(),
            expected_disk: self.state.disk_identity.clone(),
            credentials: None, lifecycle: None,
            registry: vec![RegistryPlan::Install { sources: self.state.persisted.sources.clone(),
                primary: None, replace: false }],
            purpose: PersistencePurpose::Background, writes_durable: false,
            authority: crate::plex::session::SaveAuthority::Routine };
        self.begin_commit(req, 0, true, plan, CommitDelta {
            phase: Some(Phase::Ready), activate_profile: true, ready: Some(false),
            ..Default::default()
        }, emit)
    }

    /// Settle a durability verdict ONLY if it still describes the write this owner is waiting on.
    ///
    /// Fenced by request, epoch, arrival and revision together. A completion that fails the fence
    /// is inert: it must not activate registry or profile, must not clear a newer error, and must
    /// not release admission credit that belongs to a different operation. The owner keeps only
    /// this logical identity; channels and receipts stay in the adapter.
    fn apply_persistence_completion(
        &mut self,
        completion: crate::plex::session::async_persistence::PersistenceCompletion,
        emit: &mut impl FnMut(SessionFx),
    ) -> bool {
        use crate::plex::session::async_persistence::CompletionOutcome;
        let Some(admitted) = self.state.admitted_persistence else { return false };
        let correlation = crate::plex::session::async_persistence::PersistenceCorrelation {
            req: admitted.req, epoch: admitted.epoch, arrival: admitted.arrival,
        };
        if !completion.acts_on(correlation, admitted.revision) { return false; }
        // A verdict the worker resolved for a DIFFERENT purpose than the one admitted is not this
        // operation's verdict either.
        if admitted.purpose.is_some_and(|purpose| purpose != completion.purpose) { return false; }
        let durable = matches!(completion.outcome, CompletionOutcome::Durable(_));
        self.state.admitted_persistence = None;
        let prior = self.state.unconfirmed_fresh_prior.take();
        if let (Some(prior), CompletionOutcome::Failed(_)) = (prior, &completion.outcome) {
            if admitted.fresh { self.state.disk_identity = prior; }
        }
        self.state.commit_phase.durable = durable;
        self.state.commit_phase.proves_saved_login =
            durable && completion.purpose.proves_saved_login();
        let correlation_key = PersistenceWarningKey { epoch: completion.epoch, req: completion.req };
        if durable {
            // 0.6.6 parity: a later FRESH write landing durably supersedes any warning still
            // showing about an earlier fresh attempt — the failure it was reporting on no longer
            // describes the session's current state, so holding it would strand an acknowledgement
            // over a problem that already resolved itself.
            let superseded = admitted.fresh && self.state.persistence_warning.is_some();
            if superseded {
                self.state.persistence_warning = None;
                self.retire_save_incident();
            }
            // Release only the handoff THIS completion is for — a routine completion arriving
            // while an unrelated fresh handoff is held must not free it.
            let released = self.state.held_handoff == Some(HeldHandoff { epoch: completion.epoch, req: completion.req });
            if released { self.release_held_handoff(emit); }
            if superseded || released { self.replace_publication(); }
        } else if admitted.fresh {
            if self.state.persistence_warning_answered {
                // This authorization already had its one unsaved-login question answered
                // (AUTH-03's field bug: a device unwritable on BOTH Discovery and Final used to
                // ask it twice). Storage failing again must not ask it a second time — release
                // whatever handoff this completion was holding so entry proceeds regardless.
                let released = self.state.held_handoff == Some(HeldHandoff { epoch: completion.epoch, req: completion.req });
                if released { self.release_held_handoff(emit); self.replace_publication(); }
            } else {
                // The final write's own non-durable completion replaces a Discovery warning still
                // showing (`Discovery` and `Final` never coexist: an unacknowledged Discovery warning
                // blocks `take_ready`), so the newer one wins by direct overwrite.
                let warning = PersistenceWarning::from_outcome(correlation_key, admitted.site, &completion.outcome);
                self.retire_save_incident();
                self.state.persistence_warning = Some(warning);
                if warning.helper.is_some() {
                    // The reducer reads no clock. Both report paths use this fenced completion's
                    // local warning evidence, not later process-global diagnostics.
                    if let Some(context) = warning.incident_context() {
                        self.raise_incident(IncidentFlow::SignIn, context);
                    }
                }
                self.replace_publication();
            }
        }
        true
    }

    fn apply_commit_reply(&mut self, reply: CommitReply, emit: &mut impl FnMut(SessionFx)) -> bool {
        if !self.commit_is_current(reply.req, reply.epoch, reply.arrival) { return false; }
        // `take()` is what makes a duplicate reply inert. The admitted revision and purpose are
        // copied into `admitted_persistence` first, because that is the identity a later
        // completion must repeat exactly to be allowed to settle anything.
        let admit_revision = reply.admission.admitted_revision();
        // The admission names what storage was actually asked for; the plan only records intent.
        let admit_purpose = reply.admission.admitted_purpose()
            .or_else(|| self.state.pending_commit.as_ref().and_then(|c| c.purpose));
        let commit = self.state.pending_commit.take().unwrap();
        self.state.admitted_persistence = admit_revision.map(|revision| AdmittedPersistence {
            req: commit.req, epoch: commit.epoch, arrival: commit.arrival, revision,
            purpose: admit_purpose, fresh: commit.fresh,
            site: match admit_purpose {
                Some(PersistencePurpose::Discovery) => PersistenceWarningSite::Discovery,
                _ => PersistenceWarningSite::Final,
            } });
        self.state.commit_phase = CommitPhase {
            admitted: admit_revision.is_some(),
            durable: false,
            proves_saved_login: false,
        };
        self.state.persistence_purpose = None;
        if !reply.admission.accepted() {
            if let Some(DevCommitDelta::StartAccount { login_req }) = commit.delta.dev {
                self.retire(login_req, emit);
            }
            let op = self.state.pending.remove(&reply.req).unwrap().key.op;
            match op {
                SessionOp::Login | SessionOp::Rediscover => self.fail_login("Couldn't finish sign-in. Try again.",
                    Some(IncidentContext::internal(InternalClass::CommitRefused)), emit),
                SessionOp::ProfileSwitch | SessionOp::Ready => {
                    self.state.phase = Phase::Profiles;
                    self.state.apply_pending = false;
                    self.state.error = "Couldn't switch profile — check the connection.".into();
                }
                SessionOp::HomeRoster | SessionOp::ServerRoster | SessionOp::Endpoint(_) | SessionOp::Picker => {}
                SessionOp::DevBoundary => {
                    self.state.phase = Phase::Error;
                    self.state.apply_pending = false;
                    self.state.error = "Couldn't change session authority. Try again.".into();
                }
            }
            emit(SessionFx::Retire { req: reply.req });
            if let Some(receipt) = commit.receipt { emit(SessionFx::Acknowledge(vec![receipt])); }
            self.schedule_pump(emit);
            self.replace_publication();
            return true;
        }
        let delta = commit.delta;
        if let Some(dev) = &delta.dev {
            match dev {
                DevCommitDelta::Activated => {
                    if let BootstrapAuthority::DevPms { primary, .. } = &self.state.authority {
                        let primary = primary.clone();
                        self.state.phase = Phase::Ready;
                        self.state.apply_pending = false;
                        self.publish_profile(None, emit);
                        emit(SessionFx::Ready { epoch: self.state.epoch, scope: self.state.profile_scope,
                            token: primary.token.clone(), server: primary, install: ReadyInstall::AlreadyInstalled });
                    }
                }
                DevCommitDelta::StartAccount { login_req } => {
                    self.state.authority = BootstrapAuthority::Account { extras: Vec::new() };
                    self.state.signin_active = true;
                    emit(SessionFx::Coordinator(CoordinatorAction::SignInStarted));
                    self.start_reserved_login(*login_req, emit);
                }
            }
        }
        if let Some(patch) = delta.credentials {
            self.state.persisted = patch.merge_into(&self.state.persisted);
            if commit.writes_credentials {
                let prior = std::mem::replace(&mut self.state.disk_identity, patch.identity());
                self.state.unconfirmed_fresh_prior =
                    (commit.fresh && admit_revision.is_some()).then_some(prior);
                self.state.committed_credentials = patch;
            }
        }
        if let Some(phase) = delta.phase { self.state.phase = phase; }
        if let Some(picker) = delta.picker { self.state.picker = picker; }
        if let Some(users) = delta.users { self.state.users = users; }
        if let Some(ready) = delta.ready { self.state.apply_pending = ready; }
        if delta.clear_error {
            self.state.error.clear();
            self.state.pin_denied = false;
        }
        if delta.profile_seated {
            let pending = self.state.pending.get_mut(&reply.req).unwrap();
            pending.expected = Identity::of(&self.state.persisted);
            pending.phase = StreamPhase::ProfileSeated;
            self.cancel_obsolete_interests(reply.req, emit);
        }
        if delta.complete_signin && std::mem::take(&mut self.state.signin_active) {
            emit(SessionFx::Coordinator(CoordinatorAction::SignInCompleted));
        }
        if delta.activate_profile {
            if commit.fresh && admit_revision.is_some() && !self.state.persistence_warning_answered {
                // The fresh write is admitted but not yet confirmed durable: hold the handoff
                // rather than announce Ready/publish the profile until a completion (or an
                // acknowledged warning) releases it — the AUTH-03 gate.
                self.state.held_handoff = Some(HeldHandoff { epoch: commit.epoch, req: commit.req });
            } else if self.state.persistence_warning.is_none() {
                // `persistence_warning_answered` true means this authorization already spent its
                // one Continue: nothing is left to wait for, so a fresh write lands here exactly
                // like a routine one — publish immediately rather than hold a handoff whose
                // completion (or loss) would otherwise be the only way out.
                // 0.6.6 parity (`discovery-warning-not-cleared-on-retry-or-fresh-success`): a
                // ROUTINE `activate_profile` commit (StartSwitch, resume_stored, …) must not free
                // a handoff still held behind an unacknowledged warning — that release is
                // `acknowledge_persistence_warning`'s alone. An unrelated held handoff with NO live
                // warning (already superseded above, or never one to begin with) is unaffected.
                self.release_held_handoff(emit);
            }
        }
        if commit.terminal {
            self.state.pending.remove(&reply.req);
            emit(SessionFx::Retire { req: reply.req });
        }
        if let Some(receipt) = commit.receipt { emit(SessionFx::Acknowledge(vec![receipt])); }
        self.schedule_pump(emit);
        self.replace_publication();
        true
    }

    fn retire(&mut self, req: u32, emit: &mut impl FnMut(SessionFx)) {
        self.state.pending.remove(&req);
        emit(SessionFx::Retire { req });
    }

    /// The single site that publishes a profile and announces `SessionFx::Ready`. A fresh
    /// handoff reaches it only once its write is released — durably confirmed, or its warning
    /// acknowledged; a non-fresh (`Routine`) commit reaches it immediately, exactly as before.
    fn release_held_handoff(&mut self, emit: &mut impl FnMut(SessionFx)) {
        self.state.held_handoff = None;
        self.publish_profile(Some(self.state.persisted.user.clone()), emit);
        if self.state.phase == Phase::Ready {
            emit(SessionFx::Ready { epoch: self.state.epoch, scope: self.state.profile_scope,
                server: self.state.persisted.server.clone(), token: self.state.persisted.pms_token().into(),
                install: ReadyInstall::PrimaryAndExtras(match &self.state.authority {
                    BootstrapAuthority::Account { extras } => extras.clone(),
                    BootstrapAuthority::DevPms { .. } => Vec::new(),
                }) });
        }
    }

    fn retire_save_incident(&mut self) {
        if self.state.incident.as_ref().is_some_and(|offer|
            offer.key.kind == crate::telemetry::incident::IncidentKind::SaveFailed) {
            self.state.incident = None;
        }
    }

    /// The AUTH-03 acknowledgement door: clears the warning and, if a handoff is still held for
    /// it, releases it — exactly ONE `SessionFx::Ready` for the flow, through
    /// [`Self::release_held_handoff`], never a second one from here.
    fn acknowledge_persistence_warning(&mut self, key: PersistenceWarningKey,
        emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.persistence_warning.map(|warning| warning.key) != Some(key) { return false; }
        self.state.persistence_warning = None;
        self.retire_save_incident();
        // This authorization's unsaved-login question is answered now: storage may never gate
        // entry a second time for it (AUTH-03's field bug). A later failure — Discovery then
        // Final, or a retry — releases the handoff itself instead of asking again.
        self.state.persistence_warning_answered = true;
        if self.state.held_handoff.is_some() {
            self.release_held_handoff(emit);
        }
        self.replace_publication();
        true
    }

    fn emit_work(&mut self, req: u32, input: SessionWork, emit: &mut impl FnMut(SessionFx)) {
        let pending = self.state.pending.get_mut(&req).expect("owned work request");
        let admission = AdmissionId(req);
        pending.admission = AdmissionState::Awaiting(admission);
        emit(SessionFx::Work { req, key: pending.key, admission, input });
    }

    pub fn work_is_current(&self, req: u32, key: SessionWorkKey, admission: AdmissionId) -> bool {
        key.epoch == self.state.epoch && self.state.pending.get(&req).is_some_and(|pending|
            pending.key == key && pending.capture.is_none()
                && pending.admission == AdmissionState::Awaiting(admission)
                && pending.expected.matches(&self.state.persisted))
    }

    fn apply_admission(&mut self, reply: AdmissionReply, emit: &mut impl FnMut(SessionFx)) -> bool {
        if reply.addr.to != MachineId::Session || reply.key.epoch != self.state.epoch { return false; }
        let req = reply.addr.req.0;
        let Some(pending) = self.state.pending.get_mut(&req) else { return false };
        if pending.key != reply.key || pending.admission != AdmissionState::Awaiting(reply.correlation) { return false; }
        if reply.accepted {
            pending.admission = AdmissionState::Accepted(reply.correlation);
        } else {
            // This is an unsequenced, never-admitted refusal. Accepted requests (including ones
            // without a first observation) cannot enter this branch.
            match pending.key.op {
                SessionOp::Login => self.fail_login("Couldn't start sign-in. Try again.",
                    Some(IncidentContext::internal(InternalClass::AdmissionRefused)), emit),
                SessionOp::Rediscover => self.fail_login("Couldn't restart server discovery. Try again.",
                    Some(IncidentContext::internal(InternalClass::AdmissionRefused)), emit),
                SessionOp::ProfileSwitch => {
                    self.state.phase = Phase::Profiles;
                    self.state.error = "Couldn't switch profile. Try again.".into();
                }
                SessionOp::HomeRoster => self.fail_empty_home_roster(ROSTER_UNREACHABLE),
                _ => {}
            }
            self.retire(req, emit);
            self.replace_publication();
        }
        self.schedule_pump(emit);
        true
    }

    fn advance_epoch(&mut self, emit: &mut impl FnMut(SessionFx)) -> Option<u64> {
        let epoch = self.state.epoch.checked_add(1)?;
        let requests = self.state.pending.keys().copied().collect();
        self.discard_owned_envelopes(emit);
        self.state.pending.clear();
        self.state.epoch = epoch;
        emit(SessionFx::Cancel { requests, epoch });
        Some(epoch)
    }

    fn owns_receipt(&self, receipt: Receipt) -> bool {
        self.state.inbox.iter().any(|record| Receipt::of(record) == receipt)
            || self.state.pending_commit.as_ref().and_then(|commit| commit.receipt) == Some(receipt)
    }

    fn schedule_pump(&mut self, emit: &mut impl FnMut(SessionFx)) {
        if !self.state.inbox.is_empty() && self.state.pending_commit.is_none() && !self.state.pump_pending {
            self.state.pump_pending = true;
            emit(SessionFx::Pump);
        }
    }

    fn discard_owned_envelopes(&mut self, emit: &mut impl FnMut(SessionFx)) {
        let mut receipts: Vec<_> = self.state.inbox.drain(..).map(|record| Receipt::of(&record)).collect();
        if let Some(commit) = self.state.pending_commit.take() {
            emit(SessionFx::Cancel { requests: vec![commit.req], epoch: self.state.epoch });
            if let Some(receipt) = commit.receipt { receipts.push(receipt); }
        }
        // Keep pump_pending: the marker may still be an App effect or a carried typed event.
        // Carried envelope receipts are not ours yet and therefore are NOT acknowledged here.
        if !receipts.is_empty() { emit(SessionFx::Acknowledge(receipts)); }
    }

    fn ingest(&mut self, envelope: &SessionEnvelope, emit: &mut impl FnMut(SessionFx)) -> bool {
        let receipt = Receipt::of(envelope);
        if self.owns_receipt(receipt) { return true; }
        if !self.accepts_header(envelope) {
            emit(SessionFx::Acknowledge(vec![receipt]));
            return true;
        }
        // The adapter admits only receipt-bearing records from an accepted resource. This can
        // also confirm acceptance if its small admission reply is still carried in the queue.
        self.state.pending.get_mut(&envelope.addr.req.0).unwrap().admission = AdmissionState::Accepted(envelope.admission);
        let active = usize::from(self.state.pending_commit.as_ref().is_some_and(|commit| commit.receipt.is_some()));
        assert!(self.state.inbox.len() + active < SESSION_TRANSFER_RECORDS, "Session transfer credit invariant");
        // Do not advance last_arrival here: that is the processing watermark, not receipt
        // admission. Advancing it would make the FIFO head reject its own first delivery.
        self.state.inbox.push_back(envelope.clone());
        if self.state.pending_commit.is_none() && !self.state.pump_pending { self.pump_one(emit); }
        true
    }

    fn pump_one(&mut self, emit: &mut impl FnMut(SessionFx)) {
        if self.state.pending_commit.is_some() { return; }
        let Some(mut envelope) = self.state.inbox.pop_front() else { return };
        let receipt = Receipt::of(&envelope);
        // Completion authority is independent of payload authority. Recheck at the FIFO
        // head (a preceding commit may have changed this request), never at ingress.
        // A trusted terminal with rejected data still ends its own work, using the same
        // failure policy as a dropped producer; none of its payload reaches resource IO.
        if !self.accepts(&envelope) && envelope.terminal
            && matches!(envelope.outcome, SessionArrival::Data(_))
            && self.accepts_header(&envelope)
            && self.state.pending.get(&envelope.addr.req.0).is_some_and(|pending|
                !matches!(pending.key.op, SessionOp::Endpoint(_))
                    || pending.lifecycle == envelope.lifecycle)
        {
            envelope.outcome = SessionArrival::Dropped;
        }
        if self.accepts(&envelope) {
            if !self.apply_qr_observation(&envelope, emit) {
                self.apply_resource_observation(&envelope, emit);
            }
        }
        if let Some(commit) = &mut self.state.pending_commit {
            assert!(commit.req == receipt.addr.req.0 && commit.epoch == receipt.key.epoch
                && commit.arrival == receipt.arrival, "Session commit receipt mismatch");
            commit.receipt = Some(receipt);
        } else {
            emit(SessionFx::Acknowledge(vec![receipt]));
        }
        self.schedule_pump(emit);
    }

    fn publish_profile(&mut self, profile: Option<UserRef>, emit: &mut impl FnMut(SessionFx)) {
        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.state.profile_scope.0 = self.state.profile_scope.0.wrapping_add(1);
        self.state.active_profile = profile;
        emit(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.state.epoch, profile: self.state.active_profile.clone(), scope: self.state.profile_scope,
        }));
    }

    fn cancel_obsolete_interests(&mut self, retained: u32, emit: &mut impl FnMut(SessionFx)) {
        let requests: Vec<_> = self.state.pending.iter().filter_map(|(&req, pending)|
            (req != retained && !pending.expected.matches(&self.state.persisted)).then_some(req)).collect();
        if requests.is_empty() { return; }
        for req in &requests { self.state.pending.remove(req); }
        let mut receipts = Vec::new();
        self.state.inbox.retain(|record| {
            if requests.contains(&record.addr.req.0) {
                receipts.push(Receipt::of(record));
                false
            } else { true }
        });
        emit(SessionFx::Cancel { requests, epoch: self.state.epoch });
        if !receipts.is_empty() { emit(SessionFx::Acknowledge(receipts)); }
    }

    fn refresh_roster(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.persisted.account_token.is_empty() { return false; }
        let Some(req) = self.allocate(SessionOp::ServerRoster, None) else { return false };
        self.emit_work(req, SessionWork::ServerRoster { session: self.state.persisted.clone(),
            expected: Identity::of(&self.state.persisted) }, emit);
        true
    }

    fn start_switch(&mut self, picker: Picker, emit: &mut impl FnMut(SessionFx)) -> bool {
        if matches!(self.state.authority, BootstrapAuthority::DevPms { .. }) {
            // A dev-token session has no account behind it to list a roster with. The account
            // menu still offers *Change profile* (it reads the on-disk sign-in), and the app has
            // already routed to the picker by the time this runs — so refusing SILENTLY left that
            // route with nothing behind it: a spinner that never ends and a BACK that could not
            // leave (#132, TV session 8). Say so on the picker instead; `back` returns.
            // `picker` is deliberately not recorded: nothing is detached here, so there is no
            // picker-kind rule for `back` to apply, and BACK restores this session state exactly.
            if self.state.phase != Phase::Ready || self.state.pending_erase.is_some() { return false; }
            let _ = picker;
            self.state.phase = Phase::Profiles;
            self.state.users.clear();
            self.state.error = ROSTER_REFUSED.into();
            self.replace_publication();
            return true;
        }
        if self.state.pending_erase.is_some() { return false; }
        if self.advance_epoch(emit).is_none() { return false; }
        if self.state.persisted.account_token.is_empty() {
            self.fail_login("You're signed out — sign in to use profiles.", None, emit);
            self.replace_publication();
            return true;
        }
        if super::detaches_active_profile(picker) { self.publish_profile(None, emit); }
        self.state.error.clear();
        if self.state.users.is_empty() {
            self.state.users = self.state.persisted.home_users.iter().map(UserTile::of_ref).collect();
        }
        self.state.phase = Phase::Profiles;
        self.state.picker = picker;
        if let Some(req) = self.allocate(SessionOp::Picker, None) {
            let initial_profile = picker == Picker::Boot && self.state.active_profile.is_none()
                && self.state.persisted.can_go_local();
            let mut registry = Vec::new();
            if initial_profile {
                registry.push(RegistryPlan::Primary { server: self.state.persisted.server.clone(),
                    token: self.state.persisted.pms_token().into() });
            }
            registry.push(RegistryPlan::Install {
                sources: self.state.persisted.sources.clone(), primary: None, replace: false,
            });
            let plan = CommitPlan { registry_client_id: self.state.persisted.client_id.clone(),
                expected_disk: self.state.disk_identity.clone(),
                credentials: None, lifecycle: None, registry,
                purpose: PersistencePurpose::Background, writes_durable: false,
                authority: crate::plex::session::SaveAuthority::Routine };
            self.begin_commit(req, 0, true, plan, CommitDelta {
                activate_profile: initial_profile, ..Default::default()
            }, emit);
        }
        self.refresh_roster(emit);
        if let Some(req) = self.allocate(SessionOp::HomeRoster, None) {
            self.emit_work(req, SessionWork::HomeRoster { client_id: self.state.persisted.client_id.clone(),
                account_token: self.state.persisted.account_token.clone(),
                expected: Identity::of(&self.state.persisted) }, emit);
        }
        self.replace_publication();
        true
    }

    fn select_profile(&mut self, index: usize, pin: Option<String>, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.pending_erase.is_some() { return false; }
        let Some(tile) = self.state.users.get(index).cloned() else { return false };
        if self.state.next_req.checked_add(1).is_none() { return false; }
        if self.advance_epoch(emit).is_none() { return false; }
        let same_user = pin.is_none() && !tile.protected && !self.state.persisted.user.uuid.is_empty()
            && tile.uuid == self.state.persisted.user.uuid && !self.state.persisted.pms_token().is_empty();
        if same_user {
            self.state.error.clear();
            self.state.phase = Phase::Ready;
            self.state.apply_pending = true;
        } else {
            self.state.phase = Phase::Switching;
            self.state.pin_denied = false;
            let req = self.allocate(SessionOp::ProfileSwitch, None).expect("request preflight");
            self.state.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Profile { tile, pin });
            emit(SessionFx::Capture { req, epoch: self.state.epoch, request: SessionReadRequest::ProfilePolicy });
        }
        self.replace_publication();
        true
    }

    fn request_endpoint(&mut self, sid: crate::plex::ServerId, emit: &mut impl FnMut(SessionFx)) -> bool {
        let sid = sid.raw();
        if usize::from(sid) >= crate::plex::MAX_SERVERS
            || self.state.persisted.account_token.is_empty()
            || self.state.pending.values().any(|pending| pending.key.op == SessionOp::Endpoint(sid)) {
            return false;
        }
        let Some(req) = self.allocate(SessionOp::Endpoint(sid), None) else { return false };
        self.state.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Endpoint { sid });
        emit(SessionFx::Capture { req, epoch: self.state.epoch, request: SessionReadRequest::Endpoint { sid } });
        true
    }

    fn back(&mut self, reply: ReplyTo, emit: &mut impl FnMut(SessionFx)) -> bool {
        // AUTH-03: while an unacknowledged persistence warning is showing, BACK must not clear it
        // or resume — clearing it here would silently re-admit `needs_ready_commit`/`take_ready`
        // on the next frame (a second, routine save over the still-unconfirmed fresh one) and, for
        // a held Final handoff, would release it without the explicit acknowledgement the warning
        // exists to require. Only the labelled Continue (`acknowledge_persistence_warning`) may
        // clear it; BACK here is the platform root press only.
        if self.state.persistence_warning.is_some() {
            emit(SessionFx::BackReply { to: reply, resumed: false });
            return true;
        }
        let dead_end = self.roster_dead_end();
        if dead_end {
            if let BootstrapAuthority::DevPms { primary, .. } = &self.state.authority {
                // Nothing was detached or re-pointed (see `start_switch`): re-announce the dev
                // session exactly as its activation did.
                let primary = primary.clone();
                self.state.phase = Phase::Ready;
                self.state.error.clear();
                emit(SessionFx::Ready { epoch: self.state.epoch, scope: self.state.profile_scope,
                    token: primary.token.clone(), server: primary, install: ReadyInstall::AlreadyInstalled });
                emit(SessionFx::BackReply { to: reply, resumed: true });
                self.replace_publication();
                return true;
            }
        }
        let stored = self.state.committed_credentials.merge_into(&self.state.persisted);
        // A dead-end picker offered nobody, so the picker-kind rule (`may_resume`: the
        // Change-profile picker is a root, so BACK cannot skip a PIN) has no boundary to guard.
        // Only "is there a session to go back to" remains.
        let resumable = if dead_end { stored.can_go_local() } else { super::resumable(&stored, self.state.picker) };
        if !resumable || self.state.epoch.checked_add(1).is_none() {
            emit(SessionFx::BackReply { to: reply, resumed: false });
            return true;
        }
        self.advance_epoch(emit).expect("epoch preflight");
        if std::mem::take(&mut self.state.signin_active) {
            emit(SessionFx::Coordinator(CoordinatorAction::SignInCancelled));
        }
        self.state.persisted = stored;
        self.state.phase = Phase::Ready;
        self.state.apply_pending = true;
        self.state.pin_code.clear();
        self.state.qr_png.clear();
        self.state.qr_gen = 0;
        self.state.users.clear();
        self.state.error.clear();
        self.state.pin_denied = false;
        self.state.authorized_in_flow = false;
        self.state.persistence_warning = None;
        self.state.held_handoff = None;
        self.state.persistence_warning_answered = false;
        self.state.code_replaced = false;
        emit(SessionFx::BackReply { to: reply, resumed: true });
        self.replace_publication();
        true
    }

    fn erase(&mut self, sign_in: bool, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.pending_erase.is_some() || self.state.epoch.checked_add(1).is_none() { return false; }
        emit(SessionFx::Coordinator(CoordinatorAction::CloseTelemetry));
        self.advance_epoch(emit).expect("epoch preflight");
        self.state.persisted = PersistedSession::default();
        self.state.committed_credentials = CredentialPatch::of(&self.state.persisted);
        self.state.pending_erase = Some((self.state.epoch, sign_in));
        self.state.authority = BootstrapAuthority::Account { extras: Vec::new() };
        self.state.picker = Picker::Boot;
        self.state.pin_code.clear();
        self.state.qr_png.clear();
        self.state.qr_gen = 0;
        self.state.users.clear();
        self.state.error.clear();
        self.state.pin_denied = false;
        self.state.authorized_in_flow = false;
        self.state.persistence_warning = None;
        self.state.held_handoff = None;
        self.state.persistence_warning_answered = false;
        self.state.signin_active = false;
        self.state.apply_pending = false;
        self.state.code_replaced = false;
        self.forget_incidents();
        self.publish_profile(None, emit);
        emit(SessionFx::Erase { req: self.state.next_req, epoch: self.state.epoch, all_local: !sign_in });
        self.replace_publication();
        true
    }

    fn erased(&mut self, epoch: u64, leftovers: usize, emit: &mut impl FnMut(SessionFx)) -> bool {
        let Some((expected, sign_in)) = self.state.pending_erase else { return false };
        if expected != epoch || self.state.epoch != epoch { return false; }
        self.state.pending_erase = None;
        self.state.disk_identity = Identity::of(&self.state.persisted);
        self.state.delete_leftovers = leftovers;
        self.state.phase = Phase::Deleted;
        if sign_in { self.restart_login(true, emit); }
        else { emit(SessionFx::Coordinator(CoordinatorAction::LocalDataErased)); }
        self.replace_publication();
        true
    }

    fn apply_read(&mut self, reply: &SessionReadReply, emit: &mut impl FnMut(SessionFx)) -> bool {
        if reply.addr.to != MachineId::Session || reply.epoch != self.state.epoch { return false; }
        let req = reply.addr.req.0;
        let Some(pending) = self.state.pending.get(&req) else { return false };
        if pending.key.epoch != reply.epoch || !pending.expected.matches(&self.state.persisted) { return false; }
        let Some(intent) = pending.capture.clone() else { return false };
        let input = match (intent, &reply.value) {
            (CaptureIntent::Login, SessionReadValue::LoginClientId(client_id)) if !client_id.is_empty() => {
                if self.state.disk_identity.client_id.is_empty() {
                    self.state.disk_identity.client_id = client_id.clone();
                }
                self.state.persisted.client_id = client_id.clone();
                self.state.committed_credentials.client_id = client_id.clone();
                self.state.pending.get_mut(&req).unwrap().expected = Identity::of(&self.state.persisted);
                SessionWork::Login { client_id: client_id.clone() }
            }
            (CaptureIntent::Profile { tile, pin }, SessionReadValue::ProfilePolicy { recently_unreachable }) =>
                SessionWork::ProfileSwitch { session: self.state.persisted.clone(),
                    expected: Identity::of(&self.state.persisted), tile, pin, recently_unreachable: *recently_unreachable },
            (CaptureIntent::Endpoint { sid }, SessionReadValue::Endpoint(Some(captured)))
                if captured.lifecycle.sid == sid && self.state.persisted.sources.iter().any(|source|
                    source.machine_id == captured.machine_id && source.dialable()) => {
                self.state.pending.get_mut(&req).unwrap().lifecycle = Some(captured.lifecycle);
                SessionWork::Endpoint { session: self.state.persisted.clone(), expected: Identity::of(&self.state.persisted),
                    lifecycle: captured.lifecycle, machine_id: captured.machine_id.clone() }
            }
            (CaptureIntent::Endpoint { .. }, SessionReadValue::Endpoint(_)) => {
                self.retire(req, emit);
                return true;
            }
            (CaptureIntent::Login, SessionReadValue::LoginClientId(_)) => {
                self.fail_login("Couldn't start sign-in. Try again.",
                    Some(IncidentContext::internal(InternalClass::ClientIdUnavailable)), emit);
                self.retire(req, emit);
                self.replace_publication();
                return true;
            }
            _ => return false,
        };
        let pending = self.state.pending.get_mut(&req).unwrap();
        pending.capture = None;
        self.emit_work(req, input, emit);
        true
    }

    fn roster_plan(next: &PersistedSession, probes: &[super::SettledProbe]) -> Vec<RegistryPlan> {
        let primary = next.sources.iter().position(|source| source.machine_id == next.server.machine_id);
        let mut plans = vec![RegistryPlan::Install { sources: next.sources.clone(), primary, replace: true }];
        plans.extend(probes.iter().cloned().map(RegistryPlan::Probe));
        plans
    }

    /// A roster request ended without tiles. With cached tiles on screen nothing changes — they
    /// are still the household, and offline switching reads them. With NONE, the picker says so
    /// on its own route: `Phase::Profiles` with the reason in `error` and no tiles is the
    /// read-out `screens/profiles.rs` draws, and [`Self::roster_dead_end`] is what lets BACK leave
    /// it. It used to be `fail_login`, i.e. `Phase::Error` — which the app routes to the SIGN-IN
    /// screen ("Couldn't sign in", whose Try again starts a new QR sign-in) for a person who is
    /// signed in, and whose BACK the Change-profile picker's root rule then refused (#132).
    fn fail_empty_home_roster(&mut self, reason: &str) {
        if self.state.users.is_empty() && self.state.phase == Phase::Profiles {
            self.state.error = reason.to_owned();
        }
    }

    /// Is the picker on screen one that has nobody to offer and is no longer waiting to? Then no
    /// profile boundary can be crossed from it — nobody can be handed the remote — and BACK
    /// resumes whatever session is behind it rather than stranding the person on Sign out
    /// (#132). A picker with tiles, or with a roster request still in flight, is not this.
    fn roster_dead_end(&self) -> bool {
        self.state.phase == Phase::Profiles && self.state.users.is_empty()
            && !self.state.pending.values().any(|pending| pending.key.op == SessionOp::HomeRoster)
    }

    fn apply_resource_observation(&mut self, envelope: &SessionEnvelope,
        emit: &mut impl FnMut(SessionFx)) -> bool {
        use super::observation::Observation;
        if !self.accepts(envelope) { return false; }
        let req = envelope.addr.req.0;
        let pending = &self.state.pending[&req];
        if !pending.expected.matches(&self.state.persisted) { return false; }
        let SessionArrival::Data(data) = &envelope.outcome else {
            if pending.key.op == SessionOp::ProfileSwitch && pending.phase == StreamPhase::Running {
                self.state.phase = Phase::Profiles;
                self.state.pin_denied = false;
                self.state.error = "Couldn't switch profile. Try again.".into();
            } else if pending.key.op == SessionOp::HomeRoster {
                self.fail_empty_home_roster(ROSTER_UNREACHABLE);
            }
            self.retire(req, emit);
            self.replace_publication();
            return true;
        };
        let mut delta = CommitDelta::default();
        // Purpose is decided by the op, not by whether the write happens to be a credential
        // patch: a background authorisation refresh must never read as proof of a saved login.
        // Login/Rediscover's SignedIn write is the DISCOVERY save — the FINAL one is take_ready's,
        // once the picker/Ready decision has been made.
        let purpose = match pending.key.op {
            SessionOp::Login | SessionOp::Rediscover => PersistencePurpose::Discovery,
            SessionOp::ProfileSwitch => PersistencePurpose::Profile,
            _ => PersistencePurpose::Background,
        };
        // A PEEK, never a consume: a discovery/rediscovery write must not spend the fresh
        // authority the FINAL write (take_ready) needs (AUTH-04). Only take_ready consumes it.
        let authority = if matches!(pending.key.op, SessionOp::Login | SessionOp::Rediscover)
            && self.state.authorized_in_flow {
            crate::plex::session::SaveAuthority::FreshReauthentication
        } else { crate::plex::session::SaveAuthority::Routine };
        let mut plan = CommitPlan { registry_client_id: self.state.persisted.client_id.clone(),
            expected_disk: self.state.disk_identity.clone(),
            credentials: None, registry: Vec::new(), lifecycle: pending.lifecycle,
            purpose, writes_durable: false, authority };
        match &**data {
            Observation::Login(super::LoginProgress::SignedIn { server, sources, users, .. }) => {
                let mut next = self.state.persisted.clone();
                next.server = server.clone();
                next.sources = sources.clone();
                next.home_users = users.iter().map(super::UserTile::to_ref).collect();
                // A fresh QR sign-in's account token is the owner's plex.tv credential. Keep it
                // on the owner profile too; rediscovery can belong to a managed active profile
                // and must not overwrite that profile's switch credential with the owner's.
                if pending.key.op == SessionOp::Login && !next.account_token.is_empty() {
                    next.user.plex_tv_token = Some(next.account_token.clone());
                }
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                plan.credentials = Some(patch);
                if users.len() > 1 {
                    delta.phase = Some(Phase::Profiles);
                    delta.picker = Some(Picker::SignedIn);
                    delta.users = Some(users.clone());
                } else {
                    delta.phase = Some(Phase::Ready);
                    delta.ready = Some(true);
                }
                delta.complete_signin = true;
            }
            Observation::Login(_) => return false,
            Observation::Registry(progress) => {
                plan.registry.push(match progress {
                    super::RegistryProgress::Activate { candidate, .. } => RegistryPlan::Activate {
                        source: crate::plex::session::SourceRef {
                            machine_id: candidate.machine_id.clone(), token: candidate.token.clone(),
                            name: candidate.name.clone(), shared_by: candidate.credit.clone(), owned: candidate.owned,
                            home: candidate.home, owner_id: candidate.owner_id,
                            origin_url: candidate.origin.base(), address: candidate.address.clone(),
                            port: i64::from(candidate.origin.port()), tier: Some(candidate.location),
                            extensions: Default::default(),
                        }, ipv6: candidate.ipv6,
                    },
                    super::RegistryProgress::Settled { probe, .. } => RegistryPlan::Probe(probe.clone()),
                    super::RegistryProgress::Install { sources, primary, .. } =>
                        RegistryPlan::Install { sources: sources.clone(), primary: *primary, replace: false },
                });
            }
            Observation::HomeRoster(progress) => {
                // `None`: no verdict (no answer, a 5xx, a body that would not read). `Some([])`:
                // plex.tv's verdict that this account has no roster to offer — it answered with
                // nobody, or refused the identity (a managed profile's token gets 401 from
                // `/api/v2/home/users`; see `auth::grade_roster`). Neither is committed: an empty
                // answer written over a cached roster would erase what offline switching reads.
                let users = match &progress.users {
                    Some(users) if !users.is_empty() => users,
                    graded => {
                        self.fail_empty_home_roster(if graded.is_none() { ROSTER_UNREACHABLE } else { ROSTER_REFUSED });
                        self.retire(req, emit);
                        self.replace_publication();
                        return true;
                    }
                };
                let mut next = self.state.persisted.clone();
                next.home_users = users.iter().map(super::UserTile::to_ref).collect();
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                delta.users = Some(users.clone());
                plan.credentials = Some(patch);
            }
            Observation::ServerRoster(progress) => match &progress.outcome {
                super::ServerRosterOutcome::Unreachable => {
                    self.retire(req, emit);
                    return true;
                }
                super::ServerRosterOutcome::NoReachable { settled } => {
                    // R2/A5: nothing was found to REGISTER, but a probe may still have verified
                    // something worth recording (an InsecureOnly answer, most of all) — a
                    // registry-only commit, the same shape `start_switch`'s initial-primary plan
                    // already uses: no credential patch, no lifecycle, just the probe facts.
                    if settled.is_empty() {
                        self.retire(req, emit);
                        return true;
                    }
                    plan.registry = settled.iter().cloned().map(RegistryPlan::Probe).collect();
                }
                super::ServerRosterOutcome::Reconcile { resources, found, household, settled } => {
                    let mut next = self.state.persisted.clone();
                    let refreshed = super::refreshed_sources(&next.sources, found, resources, household);
                    let usable = !refreshed.is_empty();
                    let sources = if usable { refreshed } else { next.sources.clone() };
                    let roster_changed = !super::same_sources(&sources, &next.sources);
                    let moved = usable && super::reconcile_refresh_session(&mut next, &sources);
                    next.sources = sources;
                    let repaired = next.refresh_profile_record();
                    if !(roster_changed || moved || repaired) {
                        self.retire(req, emit);
                        return true;
                    }
                    let patch = CredentialPatch::of(&next);
                    delta.credentials = Some(patch.clone());
                    plan.credentials = Some(patch);
                    plan.registry = Self::roster_plan(&next, settled);
                }
            },
            Observation::ProfileSwitch(progress) => match &progress.outcome {
                super::ProfileSwitchOutcomeProgress::Failed { error, pin_denied } => {
                    self.state.error = error.clone();
                    self.state.pin_denied = *pin_denied;
                    self.state.phase = Phase::Profiles;
                    self.retire(req, emit);
                    self.replace_publication();
                    return true;
                }
                super::ProfileSwitchOutcomeProgress::Ready { delta: profile, probes } => {
                    let mut next = self.state.persisted.clone();
                    super::merge_profile_delta(&mut next, profile.clone());
                    delta.credentials = Some(CredentialPatch::of(&next));
                    delta.phase = Some(Phase::Ready);
                    delta.ready = Some(true);
                    delta.clear_error = true;
                    delta.profile_seated = true;
                    plan.registry = Self::roster_plan(&next, probes);
                }
            },
            Observation::ProfileRoster(progress) => {
                let mut next = self.state.persisted.clone();
                let sources = super::profile_sources(&next.sources, &progress.reached,
                    &progress.resources, &next.household_ids());
                let Some(primary) = sources.iter().find(|s| s.machine_id == next.server.machine_id)
                    .or_else(|| sources.get(super::primary_index(&sources))).cloned() else {
                    self.retire(req, emit);
                    return true;
                };
                next.sources = sources;
                next.server = super::server_ref(&primary);
                next.user.token = primary.token;
                next.refresh_profile_record();
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                if !self.state.apply_pending { plan.credentials = Some(patch); }
                plan.registry = Self::roster_plan(&next, &progress.probes);
            }
            Observation::Endpoint(progress) => {
                let Some(lifecycle) = pending.lifecycle else { return false; };
                match &progress.fresh {
                    None => match &progress.probe {
                        // Nothing to INSTALL, but the probe itself is evidence — an InsecureOnly
                        // verdict most of all — so publish it rather than retiring silently.
                        Some(probe) => plan.registry.push(RegistryPlan::Probe(probe.clone())),
                        // The worker exited early (plex.tv itself unreachable, or the machine no
                        // longer among its resources): nothing was dialled, so there is nothing
                        // to publish and no verdict to widen — retire exactly as an empty
                        // `ServerRosterOutcome::NoReachable` does.
                        None => {
                            self.retire(req, emit);
                            return true;
                        }
                    },
                    Some(fresh) => {
                        let mut next = self.state.persisted.clone();
                        let Some((source, changed)) = super::apply_refreshed_endpoint(&mut next, &progress.machine_id, fresh) else {
                            self.retire(req, emit);
                            return true;
                        };
                        // Issue #95 step 6: this endpoint repair changes exactly the fields
                        // `refresh_profile_record` copies into the active profile's cached
                        // record (server/sources) — miss this and a plaintext-to-https repair
                        // (or any other endpoint move) never reaches `Session::profiles`, so an
                        // offline reseat of this same profile keeps dialling the stale origin
                        // forever. `ProfileRoster`'s commit above makes the identical call.
                        // The ServerRoster Reconcile arm above already plans credentials on
                        // `changed || repaired`; this commit used to gate on `changed` alone,
                        // silently dropping a repaired-but-otherwise-unchanged active
                        // `ProfileCreds` record on the floor.
                        let repaired = next.refresh_profile_record();
                        let patch = CredentialPatch::of(&next);
                        delta.credentials = Some(patch.clone());
                        if (changed || repaired) && !self.state.apply_pending { plan.credentials = Some(patch); }
                        plan.registry.push(RegistryPlan::Endpoint { expected: lifecycle, source });
                    }
                }
            }
        }
        plan.writes_durable = plan.credentials.is_some();
        self.begin_commit(req, envelope.arrival, envelope.terminal, plan, delta, emit)
    }

    /// Start/restart is one owner decision. Check both counters before cancelling anything:
    /// exhaustion must not detach the still-live operation or reuse its resource address.
    fn restart_login(&mut self, fresh_login: bool, emit: &mut impl FnMut(SessionFx)) -> bool {
        if matches!(self.state.authority, BootstrapAuthority::DevPms { .. }) {
            return (fresh_login || self.state.phase == Phase::Error) && self.begin_dev_account(emit);
        }
        if self.state.pending_erase.is_some() { return false; }
        let Some(epoch) = self.state.epoch.checked_add(1) else { return false };
        if self.state.next_req.checked_add(1).is_none() { return false; }
        let discovery = !fresh_login && super::retry_kind(self.state.phase,
            self.state.authorized_in_flow) == super::RetryKind::Discovery;
        let fresh_attempt = fresh_login || super::restart_is_a_new_attempt(self.state.signin_active);
        let requests = self.state.pending.keys().copied().collect();
        self.discard_owned_envelopes(emit);
        self.state.pending.clear();
        self.state.epoch = epoch;
        emit(SessionFx::Cancel { requests, epoch });
        if discovery {
            self.state.phase = Phase::Discovering;
            self.state.error.clear();
            // 0.6.6 parity (`discovery-warning-not-cleared-on-retry-or-fresh-success`): a restart
            // begins a fresh discovery attempt under a NEW epoch, so a warning (or a held handoff)
            // left over from the attempt being retried can never be released or acknowledged by
            // anything this new epoch does — it would sit stale forever. A rediscovery restart
            // clears both, the same way the fresh-attempt branch below already does.
            self.state.persistence_warning = None;
            self.state.held_handoff = None;
            self.state.persistence_warning_answered = false;
        } else {
            self.state.persisted = self.state.committed_credentials.merge_into(&self.state.persisted);
            self.state.phase = Phase::Creating;
            self.state.picker = Picker::Boot;
            self.state.pin_code.clear();
            self.state.qr_png.clear();
            self.state.users.clear();
            self.state.error.clear();
            self.state.pin_denied = false;
            self.state.authorized_in_flow = false;
            self.state.persistence_warning = None;
            self.state.held_handoff = None;
            self.state.persistence_warning_answered = false;
            self.state.apply_pending = false;
            self.state.code_replaced = false;
            self.state.qr_gen = 0;
        }
        self.state.signin_active = true;
        self.state.link_trouble = false;
        if fresh_attempt { emit(SessionFx::Coordinator(CoordinatorAction::SignInStarted)); }
        let op = if discovery { SessionOp::Rediscover } else { SessionOp::Login };
        let req = self.allocate(op, None).expect("request exhaustion checked before transition");
        let client_id = self.state.persisted.client_id.clone();
        let input = if discovery {
            SessionWork::Rediscover { client_id, account_token: self.state.persisted.account_token.clone() }
        } else { SessionWork::Login { client_id } };
        if !discovery && self.state.persisted.client_id.is_empty() {
            self.state.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Login);
            emit(SessionFx::Capture { req, epoch, request: SessionReadRequest::LoginClientId });
        } else {
            self.emit_work(req, input, emit);
        }
        self.replace_publication();
        true
    }

    /// End the flow on the error read-out. **Every ending names its incident**: `Some` is the
    /// failure's own closed evidence and is raised here, so the read-out's Details and Send report
    /// are about THIS failure; `None` is an ending the onboarding report does not cover, and it
    /// retires whatever is held, which explains an earlier failure and not this one.
    fn fail_login(&mut self, message: &str, incident: Option<IncidentContext>, emit: &mut impl FnMut(SessionFx)) {
        if std::mem::take(&mut self.state.signin_active) {
            emit(SessionFx::Coordinator(CoordinatorAction::SignInFailed { phase: self.state.phase }));
        }
        self.state.error = message.to_owned();
        self.state.phase = Phase::Error;
        self.state.persistence_warning = None;
        self.state.held_handoff = None;
        self.state.persistence_warning_answered = false;
        self.state.link_trouble = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        if incident.is_some_and(|context| context.kind.refuses_the_account_token()) {
            self.state.authorized_in_flow = false;
        }
        match incident {
            Some(context) => self.raise_incident(IncidentFlow::SignIn, context),
            None => self.state.incident = None,
        }
    }

    /// QR observations need no external commit. SignedIn and registry/profile facts go through
    /// the separate resource commit protocol, rather than treating their side effects as reads.
    fn apply_qr_observation(&mut self, envelope: &SessionEnvelope,
        emit: &mut impl FnMut(SessionFx)) -> bool {
        if !self.accepts(envelope)
            || !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
            return false;
        }
        let req = envelope.addr.req.0;
        if !self.state.pending[&req].expected.matches(&self.state.persisted) { return false; }
        match &envelope.outcome {
            SessionArrival::Refused | SessionArrival::Dropped => {
                if !envelope.terminal { return false; }
                let (message, class) = match (envelope.key.op, &envelope.outcome) {
                    (SessionOp::Rediscover, SessionArrival::Refused) =>
                        ("Couldn't restart server discovery. Try again.", InternalClass::WorkerRefused),
                    (_, SessionArrival::Refused) => ("Couldn't start sign-in. Try again.", InternalClass::WorkerRefused),
                    _ => ("Couldn't finish sign-in. Try again.", InternalClass::WorkerDropped),
                };
                self.fail_login(message, Some(IncidentContext::internal(class)), emit);
            }
            SessionArrival::Data(data) => {
                let super::observation::Observation::Login(progress) = &**data else { return false };
                use super::LoginProgress;
                let (epoch, terminal) = match progress {
                    LoginProgress::CodeReplacing { epoch }
                    | LoginProgress::CodeReady { epoch, .. }
                    | LoginProgress::Authorized { epoch, .. }
                    | LoginProgress::LinkTrouble { epoch, .. } => (*epoch, false),
                    LoginProgress::Failed { epoch, .. } => (*epoch, true),
                    LoginProgress::SignedIn { .. } => return false,
                };
                if epoch != envelope.key.epoch || terminal != envelope.terminal { return false; }
                match progress {
                    LoginProgress::CodeReplacing { .. } => {
                        if envelope.key.op != SessionOp::Login { return false; }
                        self.state.phase = Phase::Creating;
                        self.state.pin_code.clear();
                        self.state.qr_png.clear();
                        self.state.code_replaced = true;
                        // The worker's miss count is per code: a fresh code starts clean.
                        self.state.link_trouble = false;
                    }
                    LoginProgress::CodeReady { code, qr_png, .. } => {
                        if envelope.key.op != SessionOp::Login { return false; }
                        let Some(next) = self.state.next_qr.checked_add(1) else {
                            self.fail_login("Couldn't start sign-in. Try again.",
                                Some(IncidentContext::internal(InternalClass::Exhausted)), emit);
                            self.state.pending.remove(&req);
                            emit(SessionFx::Cancel { requests: vec![req], epoch: self.state.epoch });
                            emit(SessionFx::Retire { req });
                            self.replace_publication();
                            return true;
                        };
                        self.state.next_qr = next;
                        self.state.qr_gen = next;
                        self.state.pin_code = code.clone();
                        self.state.qr_png = qr_png.clone();
                        self.state.phase = Phase::Waiting;
                    }
                    LoginProgress::Authorized { token, .. } => {
                        if envelope.key.op != SessionOp::Login { return false; }
                        self.state.persisted.account_token = token.clone();
                        self.state.authorized_in_flow = true;
                        self.state.link_trouble = false;
                        self.state.phase = Phase::Discovering;
                        self.state.pending.get_mut(&req).unwrap().expected = Identity::of(&self.state.persisted);
                    }
                    LoginProgress::LinkTrouble { trouble, .. } => {
                        if envelope.key.op != SessionOp::Login { return false; }
                        self.state.link_trouble = trouble.is_some();
                        if let Some(context) = trouble {
                            self.raise_incident(IncidentFlow::SignIn, *context);
                        }
                    }
                    LoginProgress::Failed { message, incident, .. } => {
                        self.fail_login(message, Some(*incident), emit);
                    }
                    LoginProgress::SignedIn { .. } => unreachable!(),
                }
            }
        }
        if envelope.terminal {
            self.state.pending.remove(&req);
        } else {
            self.state.pending.get_mut(&req).unwrap().last_arrival = Some(envelope.arrival);
        }
        self.replace_publication();
        true
    }

    /// The checked allocator is independent from the full-width auth epoch. Exhaustion cannot
    /// reuse an address, including when a cancelled worker still owns a Landing reservation.
    fn allocate(&mut self, op: SessionOp, lifecycle: Option<ServerLifecycle>) -> Option<u32> {
        let req = self.state.next_req.checked_add(1)?;
        self.state.next_req = req;
        self.state.pending.insert(req, Pending {
            key: SessionWorkKey { epoch: self.state.epoch, op },
            expected: Identity::of(&self.state.persisted), lifecycle,
            last_arrival: None, phase: StreamPhase::Running, capture: None, admission: AdmissionState::NotRequested,
        });
        Some(req)
    }

    /// Admission to logical application, not a resource reservation. Landing owns the latter.
    /// Merely inspecting an invalid envelope must not mutate pending state or its publication.
    fn accepts_header(&self, envelope: &SessionEnvelope) -> bool {
        if envelope.addr.to != MachineId::Session { return false; }
        let Some(pending) = self.state.pending.get(&envelope.addr.req.0) else { return false; };
        pending.key == envelope.key && pending.key.epoch == self.state.epoch
            && pending.capture.is_none()
            && matches!(pending.admission, AdmissionState::Awaiting(id) | AdmissionState::Accepted(id) if id == envelope.admission)
            && pending.last_arrival.is_none_or(|last| envelope.arrival > last)
    }

    fn accepts(&self, envelope: &SessionEnvelope) -> bool {
        if !self.accepts_header(envelope) { return false; }
        let pending = &self.state.pending[&envelope.addr.req.0];
        match &envelope.outcome {
                SessionArrival::Data(data) => data.matches_request(pending, envelope.terminal)
                    && (!matches!(pending.key.op, SessionOp::Endpoint(_))
                        || pending.lifecycle == envelope.lifecycle),
                SessionArrival::Dropped | SessionArrival::Refused => envelope.terminal,
        }
    }

    fn replace_publication(&mut self) {
        // Compare borrowed fields first: constructing a throwaway snapshot here would copy
        // QR/roster payloads even if equality then retained all of the old handles.
        if self.publication.matches_state(&self.state) { return; }
        let next = SessionSnapshot::from_state_counted(&self.state, Some(&self.publication), &mut |field| {
            #[cfg(test)]
            { self.publication_payload_allocations[field] += 1; }
            #[cfg(not(test))]
            { let _ = field; }
        });
        self.publication = Arc::new(next);
    }
}

impl<H: SessionHost> crate::ui::machine::Machine<H> for SessionMachine {
    type Ev = SessionEvent;

    fn step(&mut self, ev: &SessionEvent, _cx: &crate::ui::machine::Cx<'_, H>,
        fx: &mut crate::ui::machine::Effects<'_, H>) -> crate::ui::machine::Handled {
        use crate::ui::machine::{Fx, Handled};
        let before = self.publication();
        let mut emit = |effect| fx.push(Fx::App(H::session_effect(effect)));
        let handled = match ev {
            SessionEvent::Command(Command::ActivateDevBootstrap) => self.activate_dev(&mut emit),
            SessionEvent::Command(Command::ResumeStored) => self.resume_stored(&mut emit),
            SessionEvent::Command(Command::StartLogin) => self.restart_login(true, &mut emit),
            SessionEvent::Command(Command::Retry) => self.restart_login(false, &mut emit),
            SessionEvent::Command(Command::TakeReady) => self.take_ready(&mut emit),
            SessionEvent::Command(Command::AcknowledgePersistenceWarning { key }) =>
                self.acknowledge_persistence_warning(*key, &mut emit),
            SessionEvent::Command(command @ (Command::ResolveIncident { .. }
                | Command::ReportIncident { .. } | Command::DeclineIncident { .. })) => {
                let handled = self.step_incident_command(command, &mut emit);
                if handled { self.replace_publication(); }
                handled
            }
            SessionEvent::Command(Command::StartSwitch(picker)) => self.start_switch(*picker, &mut emit),
            SessionEvent::Command(Command::SelectProfile { index, pin }) => self.select_profile(*index, pin.clone(), &mut emit),
            SessionEvent::Command(Command::SelectProfileWithReply { index, pin, reply }) => {
                let accepted = self.select_profile(*index, pin.clone(), &mut emit);
                emit(SessionFx::SelectionReply { to: *reply, accepted, flow_epoch: self.state.epoch });
                true
            }
            SessionEvent::Command(Command::BackAtRoot { reply }) => self.back(*reply, &mut emit),
            SessionEvent::Command(Command::SignOut) => self.erase(true, &mut emit),
            SessionEvent::Command(Command::EraseLocal) => self.erase(false, &mut emit),
            SessionEvent::Command(Command::RefreshRoster) => self.refresh_roster(&mut emit),
            SessionEvent::Command(Command::RequestEndpoint { sid }) => self.request_endpoint(*sid, &mut emit),
            SessionEvent::Command(Command::RestartWait { phase, qr_generation, reply }) => {
                let accepted = matches!(self.state.authority, BootstrapAuthority::Account { .. })
                    && super::restart_permitted(Some((*phase, *qr_generation)),
                    (self.state.phase, self.state.qr_gen)) && self.restart_login(false, &mut emit);
                emit(SessionFx::RestartReply { to: *reply, accepted });
                true
            }
            SessionEvent::Command(Command::DismissPinError) => {
                self.state.pin_denied = false;
                self.replace_publication();
                true
            }
            SessionEvent::Command(Command::NoteDeleteLeftovers(count)) => {
                self.state.delete_leftovers = *count;
                self.replace_publication();
                true
            }
            SessionEvent::Result(envelope) => self.ingest(envelope, &mut emit),
            SessionEvent::Commit(reply) => self.apply_commit_reply(*reply, &mut emit),
            SessionEvent::DiskWrite { before, after, outcome } => self.observe_disk_write(before, after, *outcome),
            SessionEvent::Persistence(completion) => self.apply_persistence_completion(*completion, &mut emit),
            SessionEvent::Read(reply) => self.apply_read(reply, &mut emit),
            SessionEvent::Admission(reply) => self.apply_admission(*reply, &mut emit),
            SessionEvent::Erased { epoch, leftovers } => self.erased(*epoch, *leftovers, &mut emit),
            SessionEvent::IncidentReported { id, delivery } => {
                let handled = self.incident_reported(*id, delivery);
                if handled { self.replace_publication(); }
                handled
            }
            SessionEvent::Pump => {
                self.state.pump_pending = false;
                self.pump_one(&mut emit);
                true
            }
        };
        if !Arc::ptr_eq(&before, &self.publication) {
            fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
        }
        self.refresh_subhash();
        if handled { Handled::Yes } else { Handled::No }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_leftovers_is_recorded_and_reread_without_being_consumed() {
        let mut a = SessionMachine::from_init(captured_session());
        let b = SessionMachine::from_init(captured_session());
        step(&mut a, SessionEvent::Command(Command::NoteDeleteLeftovers(3)));
        let retained = a.publication();
        assert_eq!(a.read().0.delete_leftovers, 3);
        assert_eq!(a.read().0.delete_leftovers, 3, "the read is not a consuming queue");
        assert_eq!(b.read().0.delete_leftovers, 0, "another owner cannot inherit the sweep count");
        step(&mut a, SessionEvent::Command(Command::NoteDeleteLeftovers(0)));
        assert_eq!(a.read().0.delete_leftovers, 0, "a clean sweep replaces the earlier count");
        assert_eq!(retained.read().0.delete_leftovers, 3, "retained reads stay coherent");
    }

    #[test]
    fn scalar_and_noop_transitions_do_not_temporarily_rebuild_shared_payloads() {
        let mut init = captured_session();
        init.phase = Phase::Waiting;
        init.pin_code = "AAAA".into();
        init.qr_png = vec![1, 2, 3];
        init.qr_gen = 1;
        init.next_qr = 1;
        init.users = vec![UserTile { title: "Synthetic user".into(), ..Default::default() }];
        init.pending.insert(1, Pending { key: SessionWorkKey { epoch: 1, op: SessionOp::Login },
            expected: Identity::of(&init.persisted), lifecycle: None, last_arrival: None,
            phase: StreamPhase::Running, capture: None, admission: AdmissionState::Accepted(AdmissionId(1)) });
        let mut owner = SessionMachine::from_init(init);
        let old = owner.publication();
        step(&mut owner, SessionEvent::Command(Command::NoteDeleteLeftovers(2)));
        assert_eq!(owner.publication_payload_allocations, [0; 3],
            "scalar publication changes must not allocate then discard copies of code/PNG/users");
        assert!(Arc::ptr_eq(&old.code, &owner.publication.code));
        assert!(Arc::ptr_eq(&old.png, &owner.publication.png));
        assert!(Arc::ptr_eq(&old.users, &owner.publication.users));
        let scalar = owner.publication();
        step(&mut owner, SessionEvent::Command(Command::NoteDeleteLeftovers(2)));
        step(&mut owner, SessionEvent::Command(Command::DismissPinError));
        assert_eq!(owner.publication_payload_allocations, [0; 3]);
        assert!(Arc::ptr_eq(&scalar, &owner.publication()));
        let code = qr_event(&owner, 1, 1, super::super::LoginProgress::CodeReady {
            epoch: 1, code: "BBBB".into(), qr_png: vec![4, 5, 6],
        }, false);
        step(&mut owner, SessionEvent::Result(code));
        assert_eq!(owner.publication_payload_allocations, [1, 1, 0], "only changed payloads allocate");
        assert_eq!(&*old.code, "AAAA");
        assert_eq!(&*old.png, &[1, 2, 3]);
        assert_eq!(old.qr_generation, 1);
        assert_eq!(&*owner.publication.code, "BBBB");
        assert_eq!(&*owner.publication.png, &[4, 5, 6]);
        assert_eq!(owner.publication.qr_generation, 2);
        assert!(Arc::ptr_eq(&old.users, &owner.publication.users));
    }

    struct OwnerHost;
    impl crate::ui::machine::Host for OwnerHost {
        type Arg = crate::ui::fixture::FixtureArg;
        type Fx = SessionFx;
        type Msg = SessionEvent;
        type Elem = u32;
        type Views<'a> = SessionRead<'a>;
        type Init = SessionInit;
        type Memory = ();
    }
    impl SessionHost for OwnerHost {
        fn session_effect(effect: SessionFx) -> SessionFx { effect }
    }

    fn step(owner: &mut SessionMachine, event: SessionEvent) -> Vec<SessionFx> {
        use crate::ui::machine::{Cx, Effects, Fx, InputOwner, EntryId, Machine, Tick};
        let publication = owner.publication();
        let cx = Cx::<OwnerHost> { views: publication.read(), tick: Tick::default(),
            measure: &crate::ui::fixture::FixtureMeasure, press: Default::default(),
            focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
        let mut present = crate::ui::present::Present::new();
        let mut effects = Vec::new();
        owner.step(&event, &cx, &mut Effects::new(&mut effects, MachineId::Session, &mut present));
        effects.into_iter().map(|effect| match effect.fx {
            Fx::App(effect) => effect,
            _ => panic!("Session emitted a non-domain effect"),
        }).collect()
    }

    fn captured_session() -> SessionInit {
        SessionInit::captured(PersistedSession { client_id: "synthetic-client".into(), ..Default::default() })
    }

    /// A session that can actually go local, so `resume_stored` can reach its registry-only
    /// commit. Synthetic values only.
    fn local_session() -> SessionInit {
        let mut persisted = PersistedSession { client_id: "synthetic-client".into(),
            account_token: "synthetic-account".into(), ..Default::default() };
        persisted.server.address = "127.0.0.1".into();
        persisted.server.port = 32400;
        persisted.server.token = "synthetic-token".into();
        persisted.user.token = "synthetic-token".into();
        SessionInit::captured(persisted)
    }

    /// A dialable `ServerRef` matching [`local_session`]'s own — for a `SignedIn` observation that
    /// must make `can_go_local()` true afterwards (a `Default::default()` server has no address,
    /// so `server_dialable()` refuses it regardless of the token).
    fn local_server() -> crate::plex::session::ServerRef {
        crate::plex::session::ServerRef { address: "127.0.0.1".into(), port: 32400,
            token: "synthetic-token".into(), ..Default::default() }
    }

    /// AUTH-03/AUTH-04 rig: a session mid-flow, discovering with a completed PIN authorization
    /// already recorded (`authorized_in_flow`) and one live `Login` request awaiting its
    /// `SignedIn` observation — the shape `restart_login`+`Authorized` would have produced, built
    /// directly so the test owns its own state root with no disk, no fixture and no Bridge.
    fn discovering_after_authorization() -> SessionInit {
        let mut init = local_session();
        let req = init.next_req.checked_add(1).unwrap();
        init.next_req = req;
        init.phase = Phase::Discovering;
        init.authorized_in_flow = true;
        init.pending.insert(req, Pending {
            key: SessionWorkKey { epoch: init.epoch, op: SessionOp::Login },
            expected: Identity::of(&init.persisted), lifecycle: None, last_arrival: None,
            phase: StreamPhase::Running, capture: None,
            admission: AdmissionState::Awaiting(AdmissionId(req)),
        });
        init
    }

    /// Same rig as [`discovering_after_authorization`], but seeded with an already-established,
    /// UNPROTECTED profile identity (`user.uuid`/`committed_credentials`) — the "reopened session"
    /// shape `back-bypasses-persistence-warning-ack`/AUTH-04 both describe (a Rediscover of a
    /// session that is already fully signed in as a specific, unprotected profile), which is what
    /// makes `super::resumable` actually answer TRUE rather than being refused on `Picker::Boot`'s
    /// "nobody has said who they are" default (an empty `user.uuid` reads as protected
    /// unconditionally — see [`crate::plex::session::Session::active_profile_is_protected`]).
    fn reopened_after_authorization() -> SessionInit {
        let mut init = discovering_after_authorization();
        init.persisted.user.uuid = "u-1".into();
        init.persisted.home_users = vec![crate::plex::session::HomeUserRef {
            uuid: "u-1".into(), protected: false, ..Default::default() }];
        init.committed_credentials = CredentialPatch::of(&init.persisted);
        // The pending Login request's `expected` identity was captured before this mutation —
        // recompute it, or `apply_resource_observation`'s `pending.expected.matches(&persisted)`
        // fence silently drops the SignedIn observation this rig exists to deliver.
        for pending in init.pending.values_mut() {
            pending.expected = Identity::of(&init.persisted);
        }
        init
    }

    /// Drives a fresh sign-in all the way to a held Final handoff with an unacknowledged warning
    /// showing (the discovery write lands durably, the final write does not) — the shared setup
    /// behind every `discovery-warning-not-cleared-on-retry-or-fresh-success` regression below.
    fn owner_with_held_final_warning() -> SessionMachine {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, Operation, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { uuid: "u-1".into(), protected: false,
                title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        step(&mut owner, SessionEvent::Commit(discovery_reply));
        let discovery_durable = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) };
        step(&mut owner, SessionEvent::Persistence(discovery_durable));
        step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = owner.state.next_req;
        let final_reply = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        step(&mut owner, SessionEvent::Commit(final_reply));
        let final_failed = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(final_failed));
        assert!(owner.state.held_handoff.is_some(), "rig: the Ready handoff is held");
        assert!(owner.state.persistence_warning.is_some(), "rig: a Final warning is showing");
        owner
    }

    /// ACCEPTANCE SPEC 1/5. Consuming a commit returns a TYPED admission separating authority
    /// currency from durability, and a stale completion settles nothing.
    #[test]
    fn commit_consumption_is_typed_and_a_stale_reply_settles_nothing() {
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        assert!(owner.state.pending_commit.is_some());
        // A registry-only commit asked storage for nothing and must not claim a revision.
        let registry_only = CommitReply { req, epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly };
        let effects = step(&mut owner, SessionEvent::Commit(registry_only));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Retire { .. })));
        assert_eq!(owner.state.commit_phase, CommitPhase { admitted: false, durable: false, proves_saved_login: false });
        assert!(owner.state.pending_commit.is_none());
        assert_eq!(owner.state.persistence_purpose, None,
            "a registry-only commit must not claim a persistence purpose");

        // A stale completion (wrong epoch) must not settle or retire anything.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let req = owner.state.next_req;
        let before = owner.snapshot_init().hash();
        let stale = CommitReply { req, epoch: owner.state.epoch + 9, arrival: 0,
            admission: CommitAdmission::StaleAuthority };
        assert!(step(&mut owner, SessionEvent::Commit(stale)).is_empty());
        assert_eq!(owner.snapshot_init().hash(), before,
            "a stale completion must change nothing");
        assert!(owner.state.pending_commit.is_some());
        assert_eq!(owner.state.commit_phase, CommitPhase { admitted: false, durable: false, proves_saved_login: false },
            "a stale completion must not report a durable admission");

        // The committed purpose survives into the owner's state so the caller can refuse to
        // treat a background refresh as proof of a saved login.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let req = owner.state.next_req;
        let durable = CommitReply { req, epoch: owner.state.epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 3, purpose: PersistencePurpose::Final } };
        assert!(owner.apply_commit_reply(durable, &mut |_| {}));
        assert_eq!(owner.state.commit_phase, CommitPhase { admitted: true, durable: false, proves_saved_login: false });
        assert_eq!(owner.state.persistence_purpose, None,
            "the purpose is cleared once the commit settles");
    }

    /// ACCEPTANCE SPEC 5 / Stage B bridge. A durability verdict is fenced by request, epoch,
    /// arrival AND revision, and only a saved-login-proving purpose may stand as saved-login
    /// evidence. Before this wiring the owner had no way to receive such a verdict at all, so a
    /// stale or background verdict could not even be told apart from the durable one.
    #[test]
    fn persistence_completion_is_fenced_and_background_never_proves_a_saved_login() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Operation, PersistOutcome, PersistenceCompletion, PersistenceCorrelation,
            PersistencePurpose,
        };
        let admit = |owner: &mut SessionMachine, purpose: PersistencePurpose| {
            let req = owner.state.next_req;
            let epoch = owner.state.epoch;
            let durable = CommitReply { req, epoch, arrival: 0,
                admission: CommitAdmission::Admitted { revision: 1, purpose } };
            assert!(owner.apply_commit_reply(durable, &mut |_| {}));
            PersistenceCorrelation { req, epoch, arrival: 0 }
        };
        let durable_completion = |correlation: PersistenceCorrelation, purpose: PersistencePurpose,
                                  revision: u64, epoch: u64| PersistenceCompletion {
            req: correlation.req, epoch, arrival: correlation.arrival, revision, purpose,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }),
        };
        // Deliver through the production event route (not the handler directly) and read the
        // resulting owner state: a settled verdict emits no effects, so state is the oracle.
        let deliver = |owner: &mut SessionMachine, completion: PersistenceCompletion| {
            let _ = step(owner, SessionEvent::Persistence(completion));
        };

        // (a) A stale epoch settles nothing: not durable, not saved-login evidence.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let correlation = admit(&mut owner, PersistencePurpose::Final);
        assert!(owner.state.admitted_persistence.is_some(),
            "an admitted write must leave a fenced identity behind");
        let stale_epoch = durable_completion(correlation, PersistencePurpose::Final, 1, correlation.epoch + 9);
        deliver(&mut owner, stale_epoch);
        assert!(!owner.state.commit_phase.durable,
            "a completion from a retired epoch must be inert");
        assert!(!owner.state.commit_phase.proves_saved_login);
        assert!(owner.state.admitted_persistence.is_some(),
            "a fenced-out completion must not consume the admitted identity");

        // (b) A completion naming another revision is a different operation's verdict.
        let wrong_revision = durable_completion(correlation, PersistencePurpose::Final, 99, correlation.epoch);
        deliver(&mut owner, wrong_revision);
        assert!(!owner.state.commit_phase.durable,
            "a completion naming another revision is a different operation's verdict");

        // (c) The matching completion for a Final purpose DOES settle as saved-login evidence.
        let matching = durable_completion(correlation, PersistencePurpose::Final, 1, correlation.epoch);
        deliver(&mut owner, matching);
        assert!(owner.state.commit_phase.durable, "storage confirmed durability");
        assert!(owner.state.commit_phase.proves_saved_login,
            "a Final write is evidence a login was saved");
        assert!(owner.state.admitted_persistence.is_none(), "the identity is consumed once settled");

        // (d) The SAME matching completion for a Background purpose is durable but NOT evidence.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let correlation = admit(&mut owner, PersistencePurpose::Background);
        let background = durable_completion(correlation, PersistencePurpose::Background, 1, correlation.epoch);
        deliver(&mut owner, background);
        assert!(owner.state.commit_phase.durable, "a background refresh does become durable");
        assert!(!owner.state.commit_phase.proves_saved_login,
            "a background refresh must never be offered as proof of a saved login");

        // (e) A purpose mismatch is fenced even when the correlation matches exactly.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let correlation = admit(&mut owner, PersistencePurpose::Background);
        let mismatched = durable_completion(correlation, PersistencePurpose::Final, 1, correlation.epoch);
        deliver(&mut owner, mismatched);
        assert!(!owner.state.commit_phase.durable,
            "a verdict resolved for a different purpose is not this operation's verdict");
    }

    /// AUTH-03. Port of 0.6.6's
    /// `a_fresh_sign_in_over_an_unanswered_envelope_survives_the_next_launch`
    /// (`plex/session.rs:8662`) into the 0.7 owner: the DISCOVERY write (the SignedIn observation)
    /// lands durably here, so it raises no warning — it is the FINAL write, `take_ready`'s, that
    /// fails to confirm durable, and THAT is what must be held behind an acknowledgement rather
    /// than announced as `SessionFx::Ready`.
    #[test]
    fn fresh_save_warning_requires_acknowledgement_before_the_handoff() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, Operation, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        assert!(owner.state.authorized_in_flow, "the rig starts with a completed PIN authorization");

        // The SignedIn observation begins a DISCOVERY commit, on a PEEKED fresh authority (not
        // consumed yet — AUTH-04's own guarantee).
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: Default::default(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        let effects = step(&mut owner, SessionEvent::Result(signed_in));
        let Some(SessionFx::Commit { plan, .. }) = effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("SignedIn must begin a commit");
        };
        assert_eq!(plan.purpose, PersistencePurpose::Discovery);
        assert_eq!(plan.authority, crate::plex::session::SaveAuthority::FreshReauthentication);
        assert!(owner.state.authorized_in_flow, "a discovery commit only PEEKS the authority");

        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        let effects = step(&mut owner, SessionEvent::Commit(discovery_reply));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "a discovery admission never announces Ready");
        let discovery_durable = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) };
        step(&mut owner, SessionEvent::Persistence(discovery_durable));
        assert!(owner.state.persistence_warning.is_none(), "the discovery write DID land durably");

        // take_ready now issues the FINAL commit, consuming the authority exactly once.
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let Some(SessionFx::Commit { plan: final_plan, .. }) =
            effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("TakeReady must begin the final commit");
        };
        assert_eq!(final_plan.purpose, PersistencePurpose::Final);
        assert_eq!(final_plan.authority, crate::plex::session::SaveAuthority::FreshReauthentication);
        assert!(!owner.state.authorized_in_flow, "the final commit consumes the authority");

        let final_req = owner.state.next_req;
        let final_reply = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        let effects = step(&mut owner, SessionEvent::Commit(final_reply));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "a FRESH admission is held, not announced, until its durability is known");
        assert!(owner.state.held_handoff.is_some(), "the Ready handoff is held for this commit");

        // The final write's OWN completion fails to confirm durable: this is the AUTH-03 gate.
        let final_failed = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        let effects = step(&mut owner, SessionEvent::Persistence(final_failed));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "MUTATION M1 TARGET: skipping the gate would announce Ready right here");
        assert_eq!(owner.state.persistence_warning.map(|w| w.site), Some(PersistenceWarningSite::Final));
        assert_eq!(owner.publication().persistence_warning.map(|w| w.site), Some(PersistenceWarningSite::Final),
            "the warning must reach the UI-facing publication, not only internal state");
        assert!(!owner.state.commit_phase.proves_saved_login,
            "no saved/final claim before the user acknowledges");

        let warning_key = owner.state.persistence_warning.unwrap().key;
        let wrong = PersistenceWarningKey { epoch: warning_key.epoch, req: warning_key.req + 1 };
        assert!(!step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: wrong }))
            .iter().any(|fx| matches!(fx, SessionFx::Ready { .. })));
        assert!(owner.state.persistence_warning.is_some(), "a mismatched key must not clear the warning");

        let effects = step(&mut owner,
            SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: warning_key }));
        assert_eq!(effects.iter().filter(|fx| matches!(fx, SessionFx::Ready { .. })).count(), 1,
            "acknowledging releases exactly the ONE held handoff, exactly once");
        assert!(owner.state.persistence_warning.is_none());
        assert!(owner.state.held_handoff.is_none());
    }

    #[test]
    fn declined_warning_reconstructs_every_persistence_class_and_its_evidence() {
        use crate::plex::session::async_persistence::{CompletionOutcome as O, Failure as F, Operation, PersistOutcome};
        use crate::plex::session::persistence::ProtectionFailure;
        use crate::storage::wire::{AuthPreservation, ErrorCode, KeymanagerFailure, KeymanagerFailureCategory,
            KeymanagerOperation, KeymanagerStage};
        use crate::storage::wire::failure::{HelperFailure, Stage};
        use crate::telemetry::incident::{IncidentKind, PersistenceFailure as P};
        let protection = ProtectionFailure {
            failure: KeymanagerFailure { operation: KeymanagerOperation::Seal, stage: KeymanagerStage::Finish,
                code: ErrorCode::Unavailable, category: KeymanagerFailureCategory::ServiceRejected, service_code: Some(-3961) },
            preservation: AuthPreservation::Unchanged, db8_commit_verified: false,
        };
        let helper = HelperFailure::new(Stage::Db8, Some(-3963));
        let mut errnos = [None; 8];
        errnos[0] = Some(libc::EACCES);
        for (outcome, class) in [
            (O::Failed(F::Admission(crate::storage_worker::SubmitError::Full)), P::Admission),
            (O::Failed(F::Persistence(PersistOutcome::WriteFailed)), P::WriteFailed),
            (O::Failed(F::Storage(crate::storage::StoreError::HelperUnavailable)), P::Storage),
            (O::Failed(F::Helper(helper, errnos)), P::Storage),
            (O::Uncertain { stage: crate::storage::CommitStage::Readback, errno: 0, helper: Some((helper, errnos)) }, P::CommitUncertain),
            (O::Uncertain { stage: crate::storage::CommitStage::ParentSync, errno: 5, helper: None }, P::CommitUncertain),
            (O::Failed(F::Protection(protection)), P::Protection),
            (O::ProtectionUncertain(ProtectionFailure { preservation: AuthPreservation::Uncertain, ..protection }), P::ProtectionUncertain),
            (O::Failed(F::WorkerDropped), P::WorkerDropped),
        ] {
            let mut owner = SessionMachine::from_init(discovering_after_authorization());
            let expected = IncidentContext { kind: IncidentKind::SaveFailed,
                ..IncidentContext::internal(InternalClass::CommitRefused) }.with_persistence(&outcome);
            assert_eq!(expected.persistence, Some(class));
            let warning = PersistenceWarning::from_outcome(
                PersistenceWarningKey { epoch: owner.state.epoch, req: 1 }, PersistenceWarningSite::Final, &outcome);
            // Replay/serialization must preserve the same local evidence as the live warning.
            owner.state.persistence_warning = Some(serde_json::from_value(serde_json::to_value(warning).unwrap()).unwrap());
            owner.raise_incident(IncidentFlow::SignIn, expected);
            let id = owner.state.incident.as_ref().unwrap().id;
            step(&mut owner, SessionEvent::Command(Command::ResolveIncident {
                id, permission: crate::telemetry::consent::Permission::Declined, revision: 1 }));
            assert!(owner.state.incident.as_ref().unwrap().context.is_none());
            let effects = step(&mut owner, SessionEvent::Command(Command::ReportIncident { id }));
            let rebuilt = effects.iter().find_map(|effect| match effect {
                SessionFx::Incident { lane: IncidentLane::OneOff, report: IncidentReport::Retained(context), .. } => Some(*context),
                _ => None,
            }).expect("classified warning must reconstruct its report");
            assert_eq!(rebuilt, expected, "class {class:?}");
        }
        for outcome in [O::Superseded, O::Failed(F::Superseded), O::Durable(Operation::Clear { cleanup_failed: false })] {
            assert!(PersistenceWarning::from_outcome(PersistenceWarningKey { epoch: 1, req: 1 },
                PersistenceWarningSite::Final, &outcome).incident_context().is_none());
        }
    }

    #[test]
    fn fresh_owner_sign_in_records_the_account_token_as_its_plex_tv_credential() {
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        let effects = step(&mut owner, SessionEvent::Result(signed_in));
        let plan = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => Some(plan),
            _ => None,
        }).expect("SignedIn must begin its discovery commit");
        let credentials = plan.credentials.as_ref().expect("fresh sign-in commits credentials");
        assert!(!credentials.account_token.is_empty());
        assert_eq!(credentials.user.plex_tv_token.as_deref(),
            Some(credentials.account_token.as_str()));
    }

    #[test]
    fn uncertain_db8_reply_reaches_the_warning_and_incident_report() {
        use crate::plex::session::{persistence, async_persistence::{CompletionOutcome, PersistenceCompletion}};
        use crate::storage::wire::failure::{Detail, Stage};
        for reconcile in [false, true] {
            let mut owner = SessionMachine::from_init(discovering_after_authorization());
            let req = owner.state.next_req;
            let epoch = owner.state.epoch;
            let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
                epoch, server: local_server(), sources: Vec::new(),
                users: vec![UserTile { title: "Synthetic user".into(), ..Default::default() }],
            }, true);
            step(&mut owner, SessionEvent::Result(signed_in));
            step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
                admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } }));
            let outcome = persistence::uncertain_helper_reply_for_test(reconcile);
            assert!(matches!(outcome, CompletionOutcome::Uncertain { .. }));
            step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req, epoch, arrival: 1,
                revision: 1, purpose: PersistencePurpose::Discovery, outcome }));
            let warning = owner.publication().persistence_warning.unwrap();
            let helper = warning.helper.expect("uncertain helper reply lost DB8 evidence");
            assert_eq!(helper.helper, Some(Detail::new(Stage::Db8, Some(-3963))));
            assert_eq!(helper.line(), "storage: helper · db8 (-3963)");
            assert_eq!(owner.state.incident.as_ref().unwrap().context.as_ref().unwrap().helper, Some(helper));
            let context = crate::telemetry::incident::IncidentContext::new(
                crate::telemetry::incident::IncidentKind::SaveFailed, None).with_persistence(&outcome);
            let body = crate::telemetry::incident::event_body(&"a".repeat(32), "", None, context,
                crate::telemetry::incident::ConsentKind::OneOff);
            assert_eq!(body["contexts"]["incident"]["persistence"], "commit_uncertain");
            assert_eq!(body["contexts"]["incident"]["helper"]["helper"]["stage"], "db8");
            assert_eq!(body["contexts"]["incident"]["helper"]["helper"]["code"], -3963);
            let offer = owner.state.incident.clone().unwrap();
            let retained = offer.context.unwrap();
            step(&mut owner, SessionEvent::Command(Command::ResolveIncident {
                id: offer.id, permission: crate::telemetry::consent::Permission::Declined, revision: 1 }));
            assert!(owner.state.incident.as_ref().unwrap().context.is_none());
            let effects = step(&mut owner, SessionEvent::Command(Command::ReportIncident { id: offer.id }));
            let rebuilt = effects.iter().find_map(|effect| match effect {
                SessionFx::Incident { lane: IncidentLane::OneOff, report: IncidentReport::Retained(context), .. } => Some(*context),
                _ => None,
            }).expect("one-off save report must use the visible warning evidence");
            assert_eq!(rebuilt.persistence, retained.persistence);
            assert_eq!(rebuilt.helper, retained.helper);
            assert_eq!(rebuilt, retained);
        }
    }

    #[test]
    fn helper_failure_warning_and_one_off_are_bound_to_the_completion() {
        use crate::plex::session::async_persistence::{CompletionOutcome, Failure, PersistenceCompletion};
        use crate::storage::wire::failure::{HelperFailure, Stage};
        for permission in [crate::telemetry::consent::Permission::NotDetermined,
            crate::telemetry::consent::Permission::Declined] {
            let mut owner = SessionMachine::from_init(discovering_after_authorization());
            let req = owner.state.next_req;
            let epoch = owner.state.epoch;
            let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
                epoch, server: local_server(), sources: Vec::new(),
                users: vec![UserTile { title: "Synthetic user".into(), ..Default::default() }],
            }, true);
            step(&mut owner, SessionEvent::Result(signed_in));
            step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
                admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } }));
            let failure = HelperFailure::new(Stage::Connect, Some(libc::ECONNREFUSED));
            let mut errnos = [None; 8];
            errnos[0] = Some(libc::EACCES);
            let completion = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
                purpose: PersistencePurpose::Discovery,
                outcome: CompletionOutcome::Failed(Failure::Helper(failure, errnos)) };
            step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req: req + 99, ..completion }));
            assert!(owner.state.persistence_warning.is_none());
            step(&mut owner, SessionEvent::Persistence(completion));
            let warning = owner.publication().persistence_warning.unwrap();
            assert_eq!(warning.helper, Some(failure));
            let offer = owner.state.incident.clone().unwrap();
            assert_eq!(offer.context.unwrap().helper, Some(failure));
            let effects = step(&mut owner, SessionEvent::Command(Command::ResolveIncident {
                id: offer.id, permission, revision: 1 }));
            assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Incident { .. })));
            assert!(matches!(owner.state.incident.as_ref().unwrap().state,
                IncidentState::Offered { .. } | IncidentState::Dropped));
            let effects = step(&mut owner, SessionEvent::Command(Command::ReportIncident { id: offer.id }));
            assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Incident {
                lane: IncidentLane::OneOff, report: IncidentReport::Retained(context), ..
            } if context.helper == Some(failure) && context.candidate_errnos == errnos)));
            step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: warning.key }));
            assert!(owner.state.incident.is_none());
        }
    }

    // Field regression: an unrooted webOS 4.4.3 device unwritable on BOTH the Discovery and
    // Final layers used to demand two separate "Couldn't save your sign-in" acknowledgements.
    // One Continue must suffice for the whole authorization.
    #[test]
    fn one_continue_enters_when_discovery_and_final_storage_are_unavailable() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Synthetic user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted {
                revision: 1, purpose: PersistencePurpose::Discovery,
            } }));
        let failed = CompletionOutcome::Failed(Failure::Storage(
            crate::storage::StoreError::HelperUnavailable));
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion {
            req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery, outcome: failed,
        }));
        let discovery_warning = owner.publication().persistence_warning.unwrap();
        assert_eq!(discovery_warning.site, PersistenceWarningSite::Discovery);
        assert!(owner.state.held_handoff.is_none());
        step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning {
            key: discovery_warning.key,
        }));
        assert!(owner.needs_ready_commit());
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, plan, .. }
                if plan.purpose == PersistencePurpose::Final => Some(*req),
            _ => None,
        }).expect("accepted Continue must reach the final commit");
        // The authorization already answered its one Continue, so the Final commit's own reply
        // must publish immediately rather than hold a handoff behind a completion that may never
        // arrive — see `one_continue_survives_a_lost_final_completion` for that failure mode alone.
        let commit_effects = step(&mut owner, SessionEvent::Commit(CommitReply {
            req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted {
                revision: 2, purpose: PersistencePurpose::Final,
            },
        }));
        assert!(owner.state.held_handoff.is_none(),
            "an already-answered authorization must not hold a handoff on the Final commit");
        let effects = step(&mut owner, SessionEvent::Persistence(PersistenceCompletion {
            req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final, outcome: failed,
        }));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "Ready already fired at the commit reply; a later completion must not repeat it");
        let entered_after_one_continue = commit_effects.iter().chain(effects.iter())
            .any(|fx| matches!(fx, SessionFx::Ready { .. }));
        let second_warning = owner.publication().persistence_warning;
        // Distinguish the duplicate-warning trap from an infinite owner-level dead end.
        if let Some(warning) = second_warning {
            assert_eq!(warning.site, PersistenceWarningSite::Final);
            assert_ne!(warning.key, discovery_warning.key);
            let effects = step(&mut owner, SessionEvent::Command(
                Command::AcknowledgePersistenceWarning { key: warning.key }));
            assert_eq!(effects.iter().filter(|fx| matches!(fx, SessionFx::Ready { .. })).count(), 1);
        }
        assert!(entered_after_one_continue && second_warning.is_none(),
            "one Continue must enter the app without asking the same unsaved-login question again");
    }

    // Field regression, the OTHER half of the fix above: holding the Final handoff until its own
    // completion arrives means a completion that is lost (worker died, channel dropped, app
    // backgrounded mid-write) strands an already-answered authorization forever — no warning to
    // acknowledge (there is none) and no handoff ever released. Once the one Continue is spent,
    // the Final commit's own reply is where entry must happen; nothing downstream may be load-
    // bearing for it.
    #[test]
    fn one_continue_survives_a_lost_final_completion() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Synthetic user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted {
                revision: 1, purpose: PersistencePurpose::Discovery,
            } }));
        let failed = CompletionOutcome::Failed(Failure::Storage(
            crate::storage::StoreError::HelperUnavailable));
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion {
            req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery, outcome: failed,
        }));
        let discovery_warning = owner.publication().persistence_warning.unwrap();
        step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning {
            key: discovery_warning.key,
        }));
        assert!(owner.state.persistence_warning_answered,
            "acknowledging must record that this authorization already answered");
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, plan, .. }
                if plan.purpose == PersistencePurpose::Final => Some(*req),
            _ => None,
        }).expect("accepted Continue must reach the final commit");
        // No `SessionEvent::Persistence` follows this reply at all — the completion is LOST.
        let effects = step(&mut owner, SessionEvent::Commit(CommitReply {
            req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted {
                revision: 2, purpose: PersistencePurpose::Final,
            },
        }));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "an already-answered authorization must enter on the Final commit reply itself, \
             not wait on a completion that may never arrive");
        assert!(owner.state.held_handoff.is_none(),
            "nothing may still be held once Ready has already been announced");
    }

    /// AUTH-04. Port of 0.6.6's
    /// `a_routine_save_of_a_reopened_session_does_not_spend_fresh_reauthentication_authority`
    /// (`plex/session.rs:8761`): a DISCOVERY failure must not spend the fresh authority the FINAL
    /// write still needs, and must not let `take_ready` run at all until acknowledged.
    #[test]
    fn a_discovery_failure_cannot_spend_or_authorize_the_final_fresh_save() {
        use crate::plex::session::async_persistence::{CompletionOutcome, Failure, Operation,
            PersistOutcome, PersistenceCompletion};
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;

        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: Default::default(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        step(&mut owner, SessionEvent::Commit(discovery_reply));
        assert!(owner.state.authorized_in_flow, "the discovery admission only PEEKED the authority");

        let discovery_failed = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(discovery_failed));
        assert_eq!(owner.state.persistence_warning.map(|w| w.site), Some(PersistenceWarningSite::Discovery));
        assert!(owner.state.authorized_in_flow,
            "MUTATION M3 TARGET: a discovery failure must not have spent the fresh authority");

        // Blocked: an unacknowledged warning must refuse TakeReady outright.
        assert!(!owner.needs_ready_commit(), "a live warning must suppress the ready-commit signal");
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })),
            "MUTATION M4 TARGET: take_ready must not begin a commit over an unacknowledged warning");

        let warning_key = owner.state.persistence_warning.unwrap().key;
        step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: warning_key }));
        assert!(owner.state.persistence_warning.is_none());

        // Acknowledging a DISCOVERY warning (no held handoff behind it) re-admits TakeReady, and
        // the authority the discovery failure did NOT spend is still there for the real final write.
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let Some(SessionFx::Commit { plan, .. }) = effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("the acknowledged flow must be able to reach its final commit");
        };
        assert_eq!(plan.purpose, PersistencePurpose::Final);
        assert_eq!(plan.authority, crate::plex::session::SaveAuthority::FreshReauthentication,
            "the authority a discovery failure never spent is still available to the final write");
        assert!(!owner.state.authorized_in_flow, "this final commit consumes it now");

        let final_req = owner.state.next_req;
        let admitted = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        // This authorization already answered its one Continue (the discovery warning above), so
        // the final commit's own reply publishes immediately rather than hold a handoff behind a
        // completion that may never arrive (`one_continue_survives_a_lost_final_completion`).
        let effects = step(&mut owner, SessionEvent::Commit(admitted));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "an already-answered authorization releases Ready on the final commit reply itself");
        let durable = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) };
        let effects = step(&mut owner, SessionEvent::Persistence(durable));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "Ready already fired at the commit reply; the later durable completion must not repeat it");

        // A LATER, ordinary Ready-op commit (a ready profile re-applying itself) must never carry
        // fresh authority again — the flag was spent once and stays spent.
        owner.state.apply_pending = true;
        owner.state.phase = Phase::Ready;
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let Some(SessionFx::Commit { plan: routine_plan, .. }) =
            effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("the second Ready-op commit must still be reachable");
        };
        assert_eq!(routine_plan.authority, crate::plex::session::SaveAuthority::Routine,
            "the authority was spent once; a later Ready-op commit is Routine");
    }

    /// Regression for `back-bypasses-persistence-warning-ack`: `back()` used to treat a fresh
    /// account as `resumable` and unconditionally clear `persistence_warning`/`held_handoff` and
    /// zero `authorized_in_flow`, which silently re-admitted `needs_ready_commit`/`take_ready` on
    /// the very next frame (a second, ROUTINE save over a fresh write that never confirmed durable)
    /// and, for a held Final handoff, released it without the acknowledgement the warning exists
    /// to require. BACK while a warning is showing must be inert except for the platform root
    /// press: `resumed: false`, warning/handoff/authority untouched, no `Commit`/`Ready` emitted.
    /// MUTATION for `back-bypasses-persistence-warning-ack`: delete the
    /// `if self.state.persistence_warning.is_some() { .. }` early-return this test guards at the
    /// top of `back()` — this test must then fail (the warning clears and, in the Final-site half,
    /// a `Ready` is emitted with no acknowledgement).
    #[test]
    fn back_at_root_does_not_bypass_an_unacknowledged_persistence_warning() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, Operation, PersistOutcome, PersistenceCompletion,
        };
        let reply = ReplyTo { instance: 0, correlation: 0 };

        // --- Discovery-site warning: the authority is only PEEKED so far (AUTH-04's guarantee),
        // and BACK must not spend it or clear the warning either.
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { uuid: "u-1".into(), protected: false, title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        step(&mut owner, SessionEvent::Commit(discovery_reply));
        let discovery_failed = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(discovery_failed));
        assert!(owner.state.persistence_warning.is_some(), "rig: a Discovery warning is showing");
        assert!(owner.state.authorized_in_flow, "rig: the authority is still unspent");
        assert!(owner.state.committed_credentials.merge_into(&owner.state.persisted).can_go_local(),
            "rig: the discovery write's own admission already published the signed-in credentials \
             (`apply_commit_reply`'s `writes_credentials` branch runs on ADMISSION, not completion), \
             so BACK's `resumable()` check really is exercised here rather than short-circuited");

        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert!(owner.state.persistence_warning.is_some(),
            "MUTATION TARGET: BACK must not clear an unacknowledged warning");
        assert!(owner.state.authorized_in_flow,
            "BACK must not spend the fresh authority the final write still needs");
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. } | SessionFx::Commit { .. })),
            "BACK over a live warning must not begin or announce any save");
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: false, .. })),
            "BACK is refused (root press only), not a resume");

        // --- Final-site warning with a held handoff: acknowledging is the ONLY door that may
        // release it (`acknowledge_persistence_warning` / `release_held_handoff`); BACK must not.
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { uuid: "u-1".into(), protected: false, title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        step(&mut owner, SessionEvent::Commit(discovery_reply));
        let discovery_durable = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) };
        step(&mut owner, SessionEvent::Persistence(discovery_durable));
        step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = owner.state.next_req;
        let final_reply = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        step(&mut owner, SessionEvent::Commit(final_reply));
        assert!(owner.state.held_handoff.is_some(), "rig: the Ready handoff is held for this commit");
        let final_failed = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(final_failed));
        assert!(owner.state.persistence_warning.is_some(), "rig: a Final warning is showing");

        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert!(owner.state.persistence_warning.is_some(),
            "MUTATION TARGET: BACK must not clear an unacknowledged Final-site warning");
        assert!(owner.state.held_handoff.is_some(),
            "MUTATION TARGET: BACK must not release a held handoff without acknowledgement");
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. } | SessionFx::Commit { .. })),
            "BACK over a held Final handoff must not announce Ready or begin a second save");
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: false, .. })));
    }

    /// Regression for `discovery-warning-not-cleared-on-retry-or-fresh-success` (0.6.6's
    /// `persistence_warning_generation_and_attempt_bound_every_ack_and_report`), first half: a
    /// discovery RETRY (`restart_login`'s `Retry` path) begins a brand-new attempt under a new
    /// epoch, so a Discovery-site warning left over from the attempt being retried can never be
    /// acknowledged by anything the new attempt does — it must be cleared at the restart, not left
    /// to strand `take_ready` (`needs_ready_commit` refuses while any warning is live) once the
    /// retry itself succeeds.
    /// MUTATION TARGET: drop the `self.state.persistence_warning = None;` this test guards in
    /// `restart_login`'s `if discovery { .. }` arm.
    #[test]
    fn a_discovery_retry_clears_a_stale_warning_from_the_attempt_it_replaces() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        step(&mut owner, SessionEvent::Commit(discovery_reply));
        let discovery_failed = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(discovery_failed));
        assert!(owner.state.persistence_warning.is_some(), "rig: a Discovery warning is showing");
        // `retry_kind` needs a phase that reads as Discovery-retriable; the rig is already
        // `Phase::Discovering` from `discovering_after_authorization`.
        // `retry_kind` reads `(phase, authorized_in_flow)`; a single-user `SignedIn` already
        // advances phase to `Ready` by the time its OWN write's completion can raise a warning
        // (`delta.phase` applies at commit ADMISSION, before any completion exists), so nothing
        // in this crate can reach a live Discovery warning with the phase still `Discovering` —
        // this rig sets it back deliberately to isolate `restart_login`'s DISCOVERY arm, exactly
        // as the retry decision would see mid-flight for a *multi-worker* discovery (a resource
        // fetch retried while the earlier attempt's OWN persistence write is still unacknowledged)
        // rather than depending on today's one call sequence to happen to produce that phase.
        owner.state.phase = Phase::Discovering;
        assert_eq!(super::super::retry_kind(owner.state.phase, owner.state.authorized_in_flow),
            super::super::RetryKind::Discovery, "rig: Retry must resolve to the DISCOVERY arm here");
        step(&mut owner, SessionEvent::Command(Command::Retry));
        assert!(owner.state.persistence_warning.is_none(),
            "MUTATION TARGET: a discovery restart must clear the stale warning it is replacing");
        assert!(owner.state.held_handoff.is_none());
    }

    /// A fresh write admitted over a READABLE record whose write then definitely failed leaves
    /// that record on disk, so the next fresh write must be fenced on it rather than on the
    /// identity that never landed (otherwise every retry this run is `StaleAuthority`).
    #[test]
    fn a_durable_superseded_write_advances_only_the_disk_fence() {
        use crate::plex::session::async_persistence::{CompletionOutcome, Operation, PersistOutcome};
        let mut owner = SessionMachine::from_init(captured_session());
        let before = owner.state.disk_identity.clone();
        let mut after = before.clone();
        after.account_token = "superseded-durable-token".into();
        let trusted = Identity::of(&owner.state.persisted);
        let outcome = CompletionOutcome::Durable(Operation::Write {
            outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None });
        assert!(owner.observe_disk_write(&before, &after, outcome));
        assert!(owner.state.disk_identity == after);
        assert!(Identity::of(&owner.state.persisted) == trusted, "disk evidence is not login authority");
        assert!(!owner.observe_disk_write(&before, &after, outcome), "duplicate receipt is inert");
    }

    #[test]
    fn a_failed_fresh_write_restores_the_disk_identity_it_never_replaced() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        owner.state.disk_identity = Identity { client_id: "synthetic-client".into(),
            account_token: "synthetic-revoked-account".into(), profile_uuid: "old".into() };
        let before = owner.state.disk_identity.clone();
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } }));
        assert!(owner.state.disk_identity != before, "rig: admission re-bases on the new identity");
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req, epoch, arrival: 1,
            revision: 1, purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) }));
        assert!(owner.state.disk_identity == before,
            "MUTATION TARGET: a definite failure must restore the identity still on disk");
        assert!(owner.state.persistence_warning.is_some());
    }

    /// Second half of `discovery-warning-not-cleared-on-retry-or-fresh-success`: a LATER fresh
    /// discovery write landing durably supersedes an earlier failure warning outright (0.6.6's
    /// "a later fresh success supersedes a failure warning"), even without going through
    /// `restart_login` at all — pinning `apply_persistence_completion`'s own `superseded` branch
    /// rather than only the `restart_login` door the test above exercises.
    /// MUTATION TARGET: drop the `let superseded = …` clearing in `apply_persistence_completion`'s
    /// `if durable { .. }` arm.
    #[test]
    fn a_later_durable_fresh_completion_supersedes_a_showing_warning_directly() {
        use crate::plex::session::async_persistence::{
            CompletionOutcome, Operation, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        // Manufacture a showing warning directly (no restart), keeping `admitted_persistence`
        // fenced so the very next completion below is accepted as this same operation's verdict.
        owner.state.persistence_warning = Some(PersistenceWarning {
            key: PersistenceWarningKey { epoch, req }, site: PersistenceWarningSite::Discovery, helper: None, candidate_errnos: [None; 8], persistence: None });
        owner.state.admitted_persistence = Some(AdmittedPersistence {
            req, epoch, arrival: 0, revision: 1, purpose: Some(PersistencePurpose::Discovery),
            fresh: true, site: PersistenceWarningSite::Discovery });
        let durable = PersistenceCompletion { req, epoch, arrival: 0, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) };
        step(&mut owner, SessionEvent::Persistence(durable));
        assert!(owner.state.persistence_warning.is_none(),
            "MUTATION TARGET: a later durable fresh write must supersede the earlier warning");
    }

    /// Third half of `discovery-warning-not-cleared-on-retry-or-fresh-success`: `StartSwitch` (and
    /// by the same code path `resume_stored`) issues a ROUTINE `activate_profile` commit, whose
    /// `apply_commit_reply` arm must not release a handoff still held behind a DIFFERENT,
    /// unacknowledged warning — that release belongs to `acknowledge_persistence_warning` alone.
    /// MUTATION TARGET: drop the `self.state.persistence_warning.is_none()` guard this test pins
    /// in `apply_commit_reply`'s non-fresh `activate_profile` arm (reverting to an unconditional
    /// `self.release_held_handoff(emit)`).
    #[test]
    fn start_switch_does_not_release_a_handoff_held_behind_an_unacknowledged_warning() {
        let mut owner = owner_with_held_final_warning();
        let held_before = owner.state.held_handoff;
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::Boot)));
        let Some((switch_req, switch_epoch)) = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, epoch, .. } => Some((*req, *epoch)),
            _ => None,
        }) else {
            panic!("rig: StartSwitch(Boot) must begin its own (registry-only) commit");
        };
        let switch_reply = CommitReply { req: switch_req, epoch: switch_epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly };
        let effects = step(&mut owner, SessionEvent::Commit(switch_reply));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "MUTATION TARGET: no Ready may be announced for the unrelated held handoff before \
             its own warning is acknowledged");
        assert_eq!(owner.state.held_handoff, held_before,
            "the handoff held behind the still-showing warning must be untouched");
        assert!(owner.state.persistence_warning.is_some(), "the warning itself is still unanswered");
    }

    #[test]
    fn disk_comparison_identity_is_not_a_worker_or_back_input() {
        let src = include_str!("owner.rs");
        let production = src.split("\n#[cfg(test)]\nmod tests").next().unwrap();
        // Explicit function-body list, not a transitive call-graph claim. apply_read may update
        // an empty comparison client-id from a resource reply; it never reads its token for work.
        for name in ["start_reserved_login", "restart_login", "start_switch", "select_profile",
            "back", "request_endpoint", "refresh_roster", "emit_work", "work_is_current"] {
            let marker = format!("fn {name}(");
            let start = production.find(&marker).unwrap();
            let rest = &production[start..];
            let end = rest.find("\n    }").unwrap();
            // start_switch also emits a no-save registry commit. Its exact comparison-field
            // projection is allowed; no other use in these worker/BACK constructors is.
            let body = rest[..end].replace("expected_disk: self.state.disk_identity.clone(),", "");
            assert!(!body.contains("disk_identity"), "comparison identity reached {name}");
        }
    }

    /// Issue #95 step 6, the owner-dedup half: a stored plaintext session's repair loop is
    /// `pms::landed_fail` (backoff 2s, 4s, 8s, 16s, then 30s — see
    /// `pms::the_backoff_doubles_then_holds_at_the_ceiling`) → `RequestEndpoint` on every step that
    /// comes due. `pms.rs`'s own retry gate (`kick_with`'s `s.fetching` check) already stops a
    /// second FETCH from starting before the backoff elapses, so this is the seam that matters
    /// here: however many times a heartbeat's worth of `landed_fail`s re-fires `RequestEndpoint`
    /// for the SAME sid while the previous probe is still outstanding, the owner must admit at
    /// most one `SessionOp::Endpoint(sid)` at a time. There is no single deterministic seam that
    /// drives both the backoff ladder and this admission gate together (the ladder lives in
    /// `pms.rs`, on a different clock than the owner's `pending` map), so — as the plan allows —
    /// this tests the dedup half directly; the ladder's own shape is `pms.rs`'s test above.
    #[test]
    fn request_endpoint_admits_at_most_one_per_sid_until_the_previous_attempt_resolves() {
        let mut init = captured_session();
        init.persisted.account_token = "synthetic-account".into();
        let mut owner = SessionMachine::from_init(init);
        let sid: u16 = 3;

        // First backoff step: a fresh RequestEndpoint is admitted.
        let fx = step(&mut owner, SessionEvent::Command(Command::RequestEndpoint {
            sid: crate::plex::ServerId::from_raw(sid) }));
        assert!(fx.iter().any(|f| matches!(f,
            SessionFx::Capture { request: SessionReadRequest::Endpoint { sid: s }, .. } if *s == sid)),
            "the first RequestEndpoint for an idle sid must be admitted");
        assert_eq!(owner.snapshot_init().pending.values()
            .filter(|p| p.key.op == SessionOp::Endpoint(sid)).count(), 1);

        // Every re-fire while that attempt is still outstanding — exactly what a `landed_fail`
        // during the same backoff wait produces — must be refused, not open a second probe.
        for _ in 0..3 {
            let fx = step(&mut owner, SessionEvent::Command(Command::RequestEndpoint {
                sid: crate::plex::ServerId::from_raw(sid) }));
            assert!(fx.is_empty(), "a RequestEndpoint for a sid already in flight must be a no-op");
        }
        assert_eq!(owner.snapshot_init().pending.values()
            .filter(|p| p.key.op == SessionOp::Endpoint(sid)).count(), 1,
            "still exactly one outstanding attempt for this sid — one per backoff step, not per fire");
    }

    #[test]
    fn auto_sign_in_only_changes_init_and_cached_owner_hash_and_round_trips() {
        let off = captured_session();
        let mut on = off.clone();
        on.persisted = on.persisted.with_auto_sign_in(true);
        assert_ne!(off.hash(), on.hash(), "captured auto-sign-in preference is canonical input");
        let a = SessionMachine::from_init(off);
        let b = SessionMachine::from_init(on.clone());
        assert_ne!(a.subhash(), b.subhash(), "cached owner hash must include the preference");
        let restored: SessionInit = serde_json::from_slice(&serde_json::to_vec(&on).unwrap()).unwrap();
        assert!(restored.persisted.auto_sign_in());
        assert_eq!(restored.hash(), on.hash());
        assert_eq!(SessionMachine::from_init(restored).subhash(), b.subhash());
    }

    #[test]
    fn plex_tv_token_changes_user_and_cached_owner_hashes() {
        let none = captured_session();
        let mut first = none.clone();
        first.persisted.user.plex_tv_token = Some("synthetic-plex-tv-token-a".into());
        let mut second = first.clone();
        second.persisted.user.plex_tv_token = Some("synthetic-plex-tv-token-b".into());

        assert_ne!(none.hash(), first.hash(), "None and Some plex.tv credentials are distinct canonical state");
        assert_ne!(first.hash(), second.hash(), "users differing only by plex.tv credential are distinct canonical state");
        assert_ne!(SessionMachine::from_init(none).subhash(), SessionMachine::from_init(first).subhash(),
            "cached owner hash must include the optional plex.tv credential");
    }

    #[test]
    fn busy_commit_retains_second_valid_result_in_canonical_owner_state() {
        let mut owner = SessionMachine::from_init(captured_session());
        let a = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        let b = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        for req in [a, b] {
            owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        }
        let make = |req, arrival| SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: SessionWorkKey { epoch: 1, op: SessionOp::ServerRoster }, arrival,
            admission: AdmissionId(req),
            terminal: false, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Registry(
                super::super::RegistryProgress::Install { epoch: 1,
                    expected: Some(super::super::SessionIdentity::of(&captured_session().persisted)),
                    sources: Vec::new(), primary: None }))),
        };
        let first = step(&mut owner, SessionEvent::Result(make(a, 1)));
        assert!(first.iter().any(|effect| matches!(effect, SessionFx::Commit { req, .. } if *req == a)));
        let publication = owner.publication();
        let mut canon = Canon::new();
        owner.write(&mut canon);
        let before = canon.finish();
        assert_eq!(owner.subhash(), before);
        assert!(owner.take_logical_dirty());
        assert!(!owner.take_logical_dirty());
        let second = make(b, 2);
        step(&mut owner, SessionEvent::Result(second.clone()));
        let mut canon = Canon::new();
        owner.write(&mut canon);
        let retained_hash = canon.finish();
        assert_ne!(before, retained_hash, "valid busy work must be retained in canonical owner state, not self-redelivered");
        assert_eq!(owner.subhash(), retained_hash, "the cached subhash must include retained work");
        assert!(owner.take_logical_dirty(), "logical dirty is independent of UI publication damage");
        assert!(Arc::ptr_eq(&publication, &owner.publication()));

        let duplicate = step(&mut owner, SessionEvent::Result(second));
        assert!(duplicate.is_empty(), "a duplicate must not ACK the active original's receipt");
        assert_eq!(owner.subhash(), retained_hash);
        assert!(!owner.take_logical_dirty(), "ignored duplicate must not dirty the cached state");
        assert!(Arc::ptr_eq(&publication, &owner.publication()));

        let restored = SessionMachine::from_init(owner.snapshot_init());
        assert_eq!(restored.subhash(), retained_hash, "init must retain the busy FIFO and commit receipt");
    }

    fn qr_event(owner: &SessionMachine, req: u32, arrival: u64,
        progress: super::super::LoginProgress, terminal: bool) -> SessionEnvelope {
        SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: owner.state.pending[&req].key,
            admission: AdmissionId(req),
            arrival, terminal, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Login(progress))),
        }
    }

    #[test]
    fn owned_qr_transition_retains_coherent_reads_and_ignores_duplicate_arrivals() {
        let mut owner = SessionMachine::from_init(captured_session());
        let mut effects = Vec::new();
        assert!(owner.restart_login(true, &mut |fx| effects.push(fx)));
        let epoch = owner.state.epoch;
        let req = owner.state.next_req;
        let code = qr_event(&owner, req, 1, super::super::LoginProgress::CodeReady {
            epoch, code: "synthetic-code-a".into(), qr_png: vec![1, 2, 3],
        }, false);
        assert!(owner.apply_qr_observation(&code, &mut |fx| effects.push(fx)));
        let retained = owner.publication();
        assert!(!owner.apply_qr_observation(&code, &mut |fx| effects.push(fx)));
        assert!(Arc::ptr_eq(&retained, &owner.publication()));
        let replacement = qr_event(&owner, req, 2, super::super::LoginProgress::CodeReady {
            epoch, code: "synthetic-code-b".into(), qr_png: vec![4, 5],
        }, false);
        assert!(owner.apply_qr_observation(&replacement, &mut |fx| effects.push(fx)));
        assert_eq!(&*retained.code, "synthetic-code-a");
        assert_eq!(&*retained.png, &[1, 2, 3]);
        assert_eq!(retained.qr_generation, 1);
        assert_eq!(&*owner.read().0.code, "synthetic-code-b");
        assert_eq!(&*owner.read().0.png, &[4, 5]);
        assert_eq!(owner.read().0.qr_generation, 2);
    }

    #[test]
    fn request_exhaustion_cannot_cancel_the_live_qr_operation() {
        let mut owner = SessionMachine::from_init(captured_session());
        assert!(owner.restart_login(true, &mut |_| {}));
        owner.state.next_req = u32::MAX;
        let before = owner.publication();
        let mut canon = Canon::new();
        owner.write(&mut canon);
        let hash = canon.finish();
        assert!(!owner.restart_login(true, &mut |_| panic!("exhaustion must not emit cancellation")));
        let mut canon = Canon::new();
        owner.write(&mut canon);
        assert_eq!(hash, canon.finish());
        assert!(Arc::ptr_eq(&before, &owner.publication()));
        assert_eq!(owner.state.pending.len(), 1);
    }

    #[test]
    fn qr_allocator_exhaustion_fails_and_retires_the_admitted_request() {
        let mut owner = SessionMachine::from_init(captured_session());
        assert!(owner.restart_login(true, &mut |_| {}));
        let req = owner.state.next_req;
        let epoch = owner.state.epoch;
        owner.state.next_qr = u64::MAX;
        let code = qr_event(&owner, req, 1, super::super::LoginProgress::CodeReady {
            epoch, code: "synthetic-code".into(), qr_png: vec![1, 2, 3],
        }, false);
        let mut effects = Vec::new();
        let handled = owner.apply_qr_observation(&code, &mut |fx| effects.push(fx));
        assert_eq!(owner.state.phase, Phase::Error, "exhaustion must settle the spinner as failure");
        assert!(handled);
        assert_eq!(owner.state.next_qr, u64::MAX);
        assert_eq!(owner.state.qr_gen, 0, "no generation is reused or published");
        assert!(owner.state.pending.is_empty());
        assert!(!owner.state.signin_active);
        assert!(effects.iter().any(|fx| matches!(fx,
            SessionFx::Cancel { requests, .. } if requests == &[req])));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Retire { req: retired } if *retired == req)));
        assert!(!owner.apply_qr_observation(&code, &mut |_| panic!("retired request emitted again")));
    }

    #[test]
    fn request_only_changes_affect_canonical_state_without_ui_damage() {
        let mut owner = SessionMachine::from_init(captured_session());
        let publication = owner.publication();
        let mut canon = Canon::new();
        owner.write(&mut canon);
        let before = canon.finish();
        owner.allocate(SessionOp::HomeRoster, None).unwrap();
        let mut canon = Canon::new();
        owner.write(&mut canon);
        assert_ne!(before, canon.finish());
        assert!(Arc::ptr_eq(&publication, &owner.publication()));
    }

    #[test]
    fn authorization_advances_same_request_identity_and_retry_preserves_account_link() {
        let mut owner = SessionMachine::from_init(captured_session());
        assert!(owner.restart_login(true, &mut |_| {}));
        let epoch = owner.state.epoch;
        let req = owner.state.next_req;
        let authorized = qr_event(&owner, req, 1, super::super::LoginProgress::Authorized {
            epoch, token: "synthetic-token".into(),
        }, false);
        assert!(owner.apply_qr_observation(&authorized, &mut |_| {}));
        assert!(owner.state.pending[&req].expected.matches(&owner.state.persisted));
        let failed = qr_event(&owner, req, 2, super::super::LoginProgress::Failed {
            epoch, message: "synthetic discovery failure".into(), incident: crate::auth::synthetic_incident()
        }, true);
        assert!(owner.apply_qr_observation(&failed, &mut |_| {}));
        let retained = owner.publication();
        assert!(!owner.apply_qr_observation(&failed, &mut |_| panic!("duplicate terminal")));
        assert!(Arc::ptr_eq(&retained, &owner.publication()));
        let mut effects = Vec::new();
        assert!(owner.restart_login(false, &mut |fx| effects.push(fx)));
        assert_eq!(owner.state.phase, Phase::Discovering);
        assert!(matches!(effects.last(), Some(SessionFx::Work {
            input: SessionWork::Rediscover { account_token, .. }, ..
        }) if account_token == "synthetic-token"));
    }

    #[test]
    fn independent_owners_construct_without_io_or_a_global_lock() {
        let mut a = SessionMachine::from_init(captured_session());
        let mut init = captured_session();
        init.phase = Phase::Profiles;
        init.epoch = u32::MAX as u64 + 7;
        let mut b = SessionMachine::from_init(init);
        assert_eq!(a.allocate(SessionOp::Login, None), Some(1));
        assert_eq!(b.allocate(SessionOp::HomeRoster, None), Some(1));
        assert_eq!(a.read().0.phase, Phase::Idle);
        assert_eq!(b.read().0.phase, Phase::Profiles);
        assert_eq!(b.state.pending[&1].key.epoch, u32::MAX as u64 + 7);
        a.state.next_req = u32::MAX;
        assert!(a.allocate(SessionOp::Login, None).is_none());
        assert_eq!(b.allocate(SessionOp::ServerRoster, None), Some(2));
    }

    #[test]
    fn private_init_round_trip_preserves_owned_decisions_and_probe_omits_secrets() {
        let mut init = captured_session();
        init.pin_code = "synthetic-code".into();
        init.qr_png = vec![1, 2, 3];
        init.persisted.account_token = "synthetic-token".into();
        init.next_qr = 91;
        init.qr_gen = 90;
        init.epoch = u32::MAX as u64 + 2;
        let a = SessionMachine::from_init(init);
        let encoded = serde_json::to_vec(&a.snapshot_init()).unwrap();
        let b = SessionMachine::from_init(serde_json::from_slice(&encoded).unwrap());
        let (mut ca, mut cb) = (Canon::new(), Canon::new());
        a.write(&mut ca); b.write(&mut cb);
        assert_eq!(ca.finish(), cb.finish());
        assert_eq!(&*b.read().0.png, &[1, 2, 3]);
        let mut probe = String::new();
        b.probe(&mut probe);
        assert!(!probe.contains("synthetic"));
    }

    #[test]
    fn queued_running_terminal_is_revalidated_after_ready_ack_seats_new_profile() {
        let mut init = captured_session();
        init.phase = Phase::Switching;
        init.persisted.account_token = "synthetic-account".into();
        init.persisted.user.uuid = "old-profile".into();
        let mut owner = SessionMachine::from_init(init);
        let req = owner.allocate(SessionOp::ProfileSwitch, None).unwrap();
        owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.state.epoch;
        let expected = super::super::SessionIdentity::of(&owner.state.persisted);
        let make = |arrival, terminal, outcome| SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ProfileSwitch },
            admission: AdmissionId(req), arrival, terminal, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ProfileSwitch(
                super::super::ProfileSwitchProgress { epoch, expected: expected.clone(), outcome }))),
        };
        let ready = make(1, false, super::super::ProfileSwitchOutcomeProgress::Ready {
            delta: super::super::ProfileDelta {
                user: UserRef { uuid: "new-profile".into(), token: "new-token".into(), ..Default::default() },
                server: Default::default(), sources: Vec::new(), cache: None,
            }, probes: Vec::new(),
        });
        let terminal = make(2, true, super::super::ProfileSwitchOutcomeProgress::Failed {
            error: "stale-running-failure".into(), pin_denied: true,
        });
        assert!(owner.accepts(&terminal), "terminal body genuinely valid in old Running state");
        let effects = step(&mut owner, SessionEvent::Result(ready.clone()));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })));
        assert!(step(&mut owner, SessionEvent::Result(terminal.clone())).is_empty());
        assert_eq!(owner.state.inbox.len(), 1);
        let acked = step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Final } }));
        assert_eq!(owner.state.phase, Phase::Ready);
        assert_eq!(owner.state.persisted.user.uuid, "new-profile");
        assert!(owner.state.pending[&req].phase == StreamPhase::ProfileSeated);
        assert!(!owner.accepts(&terminal), "same body is invalid at the changed FIFO head");
        let seated = owner.publication();
        let effects = step(&mut owner, SessionEvent::Pump);
        assert!(owner.state.pending.is_empty());
        assert!(owner.state.pending_commit.is_none());
        assert_eq!(owner.state.phase, Phase::Ready);
        assert_eq!(owner.state.persisted.user.uuid, "new-profile");
        assert!(!owner.state.pin_denied);
        assert!(owner.state.error.is_empty());
        assert!(Arc::ptr_eq(&seated, &owner.publication()));
        assert!(effects.iter().all(|fx| matches!(fx, SessionFx::Retire { .. } | SessionFx::Acknowledge(_))),
            "rejected terminal must not emit another commit/profile publication/Ready");
        let receipts: Vec<_> = acked.iter().chain(&effects).filter_map(|fx| match fx {
            SessionFx::Acknowledge(receipts) => Some(receipts.as_slice()), _ => None,
        }).flatten().copied().collect();
        assert!(receipts == [Receipt::of(&ready), Receipt::of(&terminal)]);
    }

    #[test]
    fn rejected_terminal_uses_existing_login_and_profile_drop_policy() {
        for (op, seated) in [(SessionOp::Login, false), (SessionOp::ProfileSwitch, false),
            (SessionOp::ProfileSwitch, true)] {
            let mut init = captured_session();
            init.phase = if op == SessionOp::Login { Phase::Waiting }
                else if seated { Phase::Ready } else { Phase::Switching };
            let mut owner = SessionMachine::from_init(init);
            let req = owner.allocate(op, None).unwrap();
            let pending = owner.state.pending.get_mut(&req).unwrap();
            pending.admission = AdmissionState::Accepted(AdmissionId(req));
            if seated { pending.phase = StreamPhase::ProfileSeated; }
            // Wrong inner epoch; the outer header still identifies this admitted terminal.
            let record = qr_event(&owner, req, 1, super::super::LoginProgress::Failed {
                epoch: owner.state.epoch + 1, message: "rejected payload text".into(), incident: crate::auth::synthetic_incident()
            }, true);
            step(&mut owner, SessionEvent::Result(record));
            assert!(owner.state.pending.is_empty());
            assert!(owner.state.pending_commit.is_none());
            assert_eq!(owner.state.phase, if op == SessionOp::Login { Phase::Error }
                else if seated { Phase::Ready } else { Phase::Profiles });
            assert_ne!(owner.state.error, "rejected payload text");
        }
    }

    #[test]
    fn rejected_terminal_cannot_bypass_processing_watermark_or_capture() {
        for captured in [false, true] {
            let mut owner = SessionMachine::from_init(captured_session());
            let req = owner.allocate(SessionOp::Login, None).unwrap();
            let pending = owner.state.pending.get_mut(&req).unwrap();
            pending.admission = AdmissionState::Accepted(AdmissionId(req));
            if captured { pending.capture = Some(CaptureIntent::Login); }
            else { pending.last_arrival = Some(2); }
            let record = qr_event(&owner, req, 1, super::super::LoginProgress::Failed {
                epoch: owner.state.epoch + 1, message: "rejected payload text".into(), incident: crate::auth::synthetic_incident()
            }, true);
            let before = owner.snapshot_init().hash();
            step(&mut owner, SessionEvent::Result(record));
            assert_eq!(owner.snapshot_init().hash(), before);
            assert!(owner.state.pending.contains_key(&req));
        }
    }

    #[test]
    fn envelope_validation_keeps_full_epoch_and_exact_destination() {
        use crate::ui::machine::RequestId;
        let mut init = captured_session();
        init.epoch = 0x1_0000_0001;
        let mut owner = SessionMachine::from_init(init);
        let req = owner.allocate(SessionOp::Login, None).unwrap();
        owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let mut envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: RequestId(req) },
            key: SessionWorkKey { epoch: 1, op: SessionOp::Login },
            admission: AdmissionId(req),
            arrival: 0, terminal: true, lifecycle: None, outcome: SessionArrival::Dropped,
        };
        assert!(!owner.accepts(&envelope), "low epoch bits are not identity");
        envelope.key.epoch = 0x1_0000_0001;
        assert!(owner.accepts(&envelope));
        envelope.addr.to = MachineId::Player;
        assert!(!owner.accepts(&envelope));
        envelope.addr.to = MachineId::Session;
        envelope.addr.req = RequestId(req + 1);
        assert!(!owner.accepts(&envelope));
    }

    // ---- issue #95, step 5: R2/A5 — a worker outcome with nothing to REGISTER still PUBLISHES ----

    /// Endpoint worker, `fresh: None` (plan §4): nothing to INSTALL, but the probe itself is
    /// evidence — an `InsecureOnly` verdict most of all — so the owner commits it as a
    /// registry-only [`RegistryPlan::Probe`] rather than silently retiring the request.
    #[test]
    fn endpoint_worker_with_no_fresh_source_still_publishes_its_probe() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("insecure-mach", "10.0.0.9", 32400, "tok", "cid");
        let client = crate::plex::client_for(sid).unwrap();
        let lifecycle = super::super::ClientLifecycle::capture(client);

        let mut owner = SessionMachine::from_init(captured_session());
        let req = owner.allocate(SessionOp::Endpoint(sid.raw()), Some(lifecycle.logical(sid.raw()))).unwrap();
        owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.state.epoch;
        let expected = super::super::SessionIdentity::of(&owner.state.persisted);
        let probe = super::super::settled_probe_for_test(
            "insecure-mach", crate::plex::probe::Outcome::InsecureOnly, Some(crate::plex::probe::Location::Local), Some("10.0.0.9".into()));
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::Endpoint(sid.raw()) },
            admission: AdmissionId(req), arrival: 1, terminal: true, lifecycle: Some(lifecycle.logical(sid.raw())),
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Endpoint(
                super::super::observation::EndpointFact {
                    epoch, expected, sid: sid.raw(), machine_id: "insecure-mach".into(),
                    fresh: None, probe: Some(probe.clone()),
                }))),
        };
        assert!(owner.accepts(&envelope), "a None-fresh endpoint reply is still a valid terminal arrival");
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        let plan = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => Some(plan),
            _ => None,
        }).expect("a None-fresh endpoint reply must still commit its probe");
        assert_eq!(plan.registry.len(), 1, "nothing to INSTALL — the probe is the whole plan");
        assert!(
            matches!(&plan.registry[0], RegistryPlan::Probe(p)
                if p.machine_id == "insecure-mach" && p.outcome == crate::plex::probe::Outcome::InsecureOnly),
            "the InsecureOnly verdict must reach the registry even with no source to install"
        );
        assert!(plan.credentials.is_none(), "a registry-only probe writes no credentials");
    }

    /// PR #104 review: the endpoint commit used to gate `plan.credentials` on `changed` alone,
    /// discarding `refresh_profile_record`'s own return — unlike the `ServerRosterOutcome::
    /// Reconcile` arm a few lines above, which already includes `repaired`. So an endpoint reply
    /// whose route facts exactly match what is already stored (`changed == false`) but whose
    /// active profile's cached record (`Session::profiles`) is stale never got repaired on disk,
    /// even though `delta.credentials` (the in-memory/UI side) was updated regardless. A later
    /// offline reseat of that profile would then read the stale cached server/sources forever.
    #[test]
    fn endpoint_commit_plans_credentials_when_only_the_profile_record_needed_repair() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let sid = crate::plex::register_for_test("ours", "10.0.0.9", 32400, "profile-token", "cid");
        let client = crate::plex::client_for(sid).unwrap();
        let lifecycle = super::super::ClientLifecycle::capture(client);

        // The route facts the endpoint reply carries are IDENTICAL to what is already stored, so
        // `apply_refreshed_endpoint` reports `changed == false`.
        let current = crate::plex::session::SourceRef {
            machine_id: "ours".into(),
            owned: true,
            token: "profile-token".into(),
            address: "10.0.0.9".into(),
            port: 32400,
            origin_url: "http://10.0.0.9:32400".into(),
            ..Default::default()
        };
        let mut persisted = PersistedSession {
            client_id: "synthetic-client".into(),
            user: UserRef { uuid: "u1".into(), ..Default::default() },
            server: super::super::server_ref(&current),
            sources: vec![current.clone()],
            ..Default::default()
        };
        // The active profile's own cached record is stale/blank — `refresh_profile_record` must
        // overwrite it and report `repaired == true`, independently of `changed`.
        persisted.profiles.push(crate::plex::session::ProfileCreds {
            uuid: "u1".into(),
            user: UserRef::default(),
            server: crate::plex::session::ServerRef::default(),
            sources: Vec::new(),
            pin: None,
            extensions: Default::default(),
        });

        let mut owner = SessionMachine::from_init(SessionInit::captured(persisted));
        let req = owner.allocate(SessionOp::Endpoint(sid.raw()), Some(lifecycle.logical(sid.raw()))).unwrap();
        owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.state.epoch;
        let expected = super::super::SessionIdentity::of(&owner.state.persisted);
        let probe = super::super::settled_probe_for_test(
            "ours", crate::plex::probe::Outcome::Reachable, Some(crate::plex::probe::Location::Local), Some("10.0.0.9".into()));
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::Endpoint(sid.raw()) },
            admission: AdmissionId(req), arrival: 1, terminal: true, lifecycle: Some(lifecycle.logical(sid.raw())),
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Endpoint(
                super::super::observation::EndpointFact {
                    epoch, expected, sid: sid.raw(), machine_id: "ours".into(),
                    fresh: Some(current.clone()), probe: Some(probe),
                }))),
        };
        assert!(owner.accepts(&envelope));
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        let plan = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => Some(plan),
            _ => None,
        }).expect("an endpoint reply that repairs the profile record must still commit");
        assert!(
            plan.credentials.is_some(),
            "route facts were unchanged but the active profile's cached record needed repair — \
             the commit must still write credentials so the repair reaches disk"
        );
    }

    /// `ServerRosterOutcome::NoReachable` (R2/A5): the roster itself found nothing to REGISTER,
    /// but every probe that ran is still carried to the owner and committed as registry-only
    /// [`RegistryPlan::Probe`]s — never as a credential or a roster write, since nothing about the
    /// disk roster changed.
    #[test]
    fn no_reachable_server_roster_commits_registry_only_probes_and_no_credentials() {
        let _g = crate::testlock::serial();
        let mut owner = SessionMachine::from_init(captured_session());
        let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.state.epoch;
        let expected = super::super::SessionIdentity::of(&owner.state.persisted);
        let settled = vec![
            super::super::settled_probe_for_test("a", crate::plex::probe::Outcome::InsecureOnly,
                Some(crate::plex::probe::Location::Local), Some("10.0.0.1".into())),
            super::super::settled_probe_for_test("b", crate::plex::probe::Outcome::Unreachable, None, None),
        ];
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ServerRoster },
            admission: AdmissionId(req), arrival: 1, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
                super::super::ServerRosterProgress { epoch, expected,
                    outcome: super::super::ServerRosterOutcome::NoReachable { settled: settled.clone() } }))),
        };
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        let plan = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => Some(plan),
            _ => None,
        }).expect("NoReachable with a non-empty settled list must still commit");
        assert_eq!(plan.registry.len(), settled.len(), "one RegistryPlan::Probe per settled probe, no roster Install");
        assert!(plan.registry.iter().all(|p| matches!(p, RegistryPlan::Probe(_))),
            "NoReachable must never write RegistryPlan::Install — the roster itself is unchanged");
        assert!(plan.credentials.is_none(), "a registry-only commit writes no credentials");
    }

    /// Issue #132's production half: a signed-in account whose cached roster is EMPTY (a failed
    /// fetch at sign-in persists an empty vec) presses *Change profile*, and the roster fetch fails
    /// again — plex.tv refusing the token (401, answered as `Some(vec![])`) or never answering
    /// (`None`). The picker must read out a failure on ITS OWN route (phase stays `Profiles`, with
    /// the reason in `error`), not flip to `Phase::Error`, which the profiles screen does not draw
    /// — that left an endless spinner. And BACK must hand back the still-valid session: no picker
    /// was ever offered, so no profile boundary was crossed.
    /// PR #212 review (P2, "the last spinner frame stays visible"): the step that lands a failed
    /// roster must itself request a frame. Once the read-out replaces the spinner nothing on the
    /// Profiles screen is moving, so if this step did not damage Present the spinner's final
    /// frame would sit there until the keepalive redraw.
    #[test]
    fn landing_a_failed_roster_requests_the_frame_that_draws_the_readout() {
        use crate::ui::machine::{Cx, Effects, InputOwner, EntryId, Machine, Tick};
        let _g = crate::testlock::serial();
        for users in [None, Some(Vec::new())] {
            let mut owner = SessionMachine::from_init(local_session());
            let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
            let (req, key) = effects.iter().find_map(|fx| match fx {
                SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
                _ => None,
            }).expect("Change profile fetches the Home roster");
            settle_picker_commit(&mut owner, &effects);
            let envelope = roster_envelope(&mut owner, req, key, users);

            let publication = owner.publication();
            let cx = Cx::<OwnerHost> { views: publication.read(), tick: Tick::default(),
                measure: &crate::ui::fixture::FixtureMeasure, press: Default::default(),
                focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
            let mut present = crate::ui::present::Present::new();
            let _ = present.take(0); // the spinner's last frame has been presented
            assert!(!present.peek(0), "rig: nothing is owed before the result lands");
            let mut sink = Vec::new();
            owner.step(&SessionEvent::Result(envelope), &cx,
                &mut Effects::new(&mut sink, MachineId::Session, &mut present));
            assert!(owner.read().0.roster_readout().is_some(), "rig: the read-out is up");
            assert!(present.peek(0), "the step that raises the read-out must request its frame");
        }
    }

    fn empty_roster_change_profile(users: Option<Vec<UserTile>>) -> (SessionMachine, Vec<SessionFx>) {
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.state.persisted.home_users.is_empty(), "rig: nothing cached to show");
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let (req, key) = effects.iter().find_map(|fx| match fx {
            SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
            _ => None,
        }).expect("Change profile fetches the Home roster");
        assert_eq!(owner.state.phase, Phase::Profiles);
        settle_picker_commit(&mut owner, &effects);
        let envelope = roster_envelope(&mut owner, req, key, users);
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        (owner, effects)
    }

    /// The picker's own registry-only commit is in flight until answered, and a roster result
    /// arriving behind it is QUEUED rather than applied — settle it the way the adapter would.
    fn settle_picker_commit(owner: &mut SessionMachine, effects: &[SessionFx]) {
        let (req, epoch) = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, epoch, .. } => Some((*req, *epoch)),
            _ => None,
        }).expect("rig: the picker commits its registry plan");
        step(owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly }));
        assert!(owner.state.pending_commit.is_none(), "rig: nothing is left in flight");
    }

    fn roster_envelope(owner: &mut SessionMachine, req: u32, key: SessionWorkKey,
        users: Option<Vec<UserTile>>) -> SessionEnvelope {
        owner.state.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let expected = super::super::SessionIdentity::of(&owner.state.persisted);
        SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key, admission: AdmissionId(req), arrival: 1, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::HomeRoster(
                super::super::HomeRosterProgress { epoch: key.epoch, expected, users }))),
        }
    }

    #[test]
    fn change_profile_with_no_roster_reads_out_the_failure_and_back_resumes_the_session() {
        let _g = crate::testlock::serial();
        for (users, reason) in [
            (None, ROSTER_UNREACHABLE),
            (Some(Vec::new()), ROSTER_REFUSED),
        ] {
            let (mut owner, _) = empty_roster_change_profile(users);
            assert_eq!(owner.state.phase, Phase::Profiles,
                "the failure is read out on the picker's own route, not as a sign-in Error");
            assert!(owner.state.users.is_empty());
            assert_eq!(owner.state.error, reason);
            assert!(owner.state.persisted.home_users.is_empty(), "no roster is invented or committed");
            let reply = ReplyTo { instance: 0, correlation: 7 };
            let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
            assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: true, .. })),
                "BACK from a picker that never offered a profile resumes the session behind it");
            assert_eq!(owner.state.phase, Phase::Ready);
            assert!(owner.state.apply_pending, "the resume goes through the ordinary Ready handoff");
            assert!(owner.state.error.is_empty());
        }
    }

    /// A refused roster must not wipe a CACHED one (it is what offline switching reads), and a
    /// picker that HAS tiles is still a root: BACK stays refused there.
    #[test]
    fn a_refused_roster_keeps_cached_tiles_and_the_picker_stays_a_root() {
        let _g = crate::testlock::serial();
        let mut init = local_session();
        init.persisted.home_users = vec![crate::plex::session::HomeUserRef {
            uuid: "cached-user".into(), title: "Cached".into(), ..Default::default() }];
        let mut owner = SessionMachine::from_init(init);
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let (req, key) = effects.iter().find_map(|fx| match fx {
            SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
            _ => None,
        }).unwrap();
        settle_picker_commit(&mut owner, &effects);
        let envelope = roster_envelope(&mut owner, req, key, Some(Vec::new()));
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })),
            "an answer with nobody in it is not committed over the cached roster");
        assert_eq!(owner.state.users.len(), 1);
        assert_eq!(owner.state.persisted.home_users.len(), 1);
        assert!(owner.state.error.is_empty());
        let reply = ReplyTo { instance: 0, correlation: 7 };
        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: false, .. })),
            "with tiles on screen the Change-profile picker is still the root it always was");
    }

    /// Issue #132 as TV session 8 actually hit it: a dev-token (`DevPms`) boot, where the account
    /// menu still offers *Change profile* from the on-disk sign-in. The owner used to refuse the
    /// switch silently — no roster worker, no log line — while the app routed to the picker
    /// anyway: an endless spinner with an inert BACK. The refusal is now the same read-out, and
    /// BACK re-announces the dev session exactly as its activation did.
    #[test]
    fn a_dev_session_change_profile_says_it_cannot_switch_and_back_returns() {
        let _g = crate::testlock::serial();
        let saved = PersistedSession { client_id: "synthetic-client".into(), ..Default::default() };
        let mut owner = SessionMachine::from_init(SessionInit::captured_boot(saved, Some(local_server()), Vec::new()));
        let effects = step(&mut owner, SessionEvent::Command(Command::ActivateDevBootstrap));
        let (req, epoch) = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, epoch, .. } => Some((*req, *epoch)),
            _ => None,
        }).expect("rig: the dev boundary commits");
        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly }));
        assert_eq!(owner.state.phase, Phase::Ready, "rig: the dev session is up");

        let before = owner.publication();
        step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        assert_eq!(owner.state.phase, Phase::Profiles,
            "the picker the app just routed to must have a state behind it");
        assert_eq!(owner.state.error, ROSTER_REFUSED);
        assert!(owner.state.users.is_empty());
        // TV, PR #212: this read-out exists BEFORE the Profiles screen mounts, so only the step
        // that entered it can announce it — and it must, once.
        let entered = owner.publication();
        assert_eq!(roster_readout_entered(&before, &entered).as_deref(),
            Some(format!("profiles: no profiles to offer — {ROSTER_REFUSED} (BACK returns)").as_str()));
        assert_eq!(roster_readout_entered(&entered, &entered), None, "announced once, not per step");

        let reply = ReplyTo { instance: 0, correlation: 7 };
        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert_eq!(roster_readout_entered(&entered, &owner.publication()), None, "leaving is not entering");
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: true, .. })));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Ready {
            install: ReadyInstall::AlreadyInstalled, .. })), "BACK hands the dev session back");
        assert_eq!(owner.state.phase, Phase::Ready);
        assert!(owner.state.error.is_empty());
    }
}
