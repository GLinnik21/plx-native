//! Concrete auth decisions and immutable publications. Resource operations belong to the
//! application adapter; constructing or observing this value performs no external work.
#![deny(clippy::wildcard_enum_match_arm)]

use super::{Phase, Picker, UserTile};
use plx_plex::plex::session::async_persistence::{PersistencePurpose, RejectionKind};
use plx_plex::plex::session::{Session as PersistedSession, UserRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use plx_machine::machine::{Addr, Canon, LogicalState, MachineId};

mod attempt;
mod incident;
use plx_telemetry::telemetry::incident::{IncidentContext, InternalClass, IncidentKind};
use plx_telemetry::telemetry::consent::Permission;
use incident::{is_settled, SEEN_CAP};
pub use incident::{IncidentDelivery, IncidentFlow, IncidentKey, IncidentLane, IncidentOffer,
    IncidentReport, IncidentState};

pub const SESSION_DATA_RECORDS: usize = 64;
pub const SESSION_OWNER_RESERVATIONS: u32 = 32;
pub const SESSION_TOTAL_RESERVATIONS: u32 = 32;
pub const SESSION_TRANSFER_RECORDS: usize = SESSION_DATA_RECORDS + SESSION_TOTAL_RESERVATIONS as usize;

/// A credit for one distinct transferred record, not a worker-completion acknowledgement.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
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

pub fn write_purpose(w: &mut Canon, purpose: PersistencePurpose) {
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
pub struct SessionEnvelope {
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
pub enum SessionArrival {
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
pub trait ObservationSink {
    fn live(&self) -> bool;
    fn progress(&self, value: super::AuthProgress) -> bool;
    fn terminal(&self, value: super::AuthProgress) -> bool;
}

/// Fully captured worker input. No native pointer or closure can be serialized into work.
#[derive(Clone, Serialize, Deserialize)]
pub enum SessionWork {
    Login { client_id: String },
    Rediscover { client_id: String, account_token: String },
    HomeRoster { client_id: String, account_token: String, expected: Identity },
    ServerRoster { session: PersistedSession, expected: Identity },
    ProfileSwitch { session: PersistedSession, expected: Identity, tile: UserTile,
        pin: Option<String>, recently_unreachable: bool },
    Endpoint { session: PersistedSession, expected: Identity, lifecycle: ServerLifecycle,
        machine_id: String },
}

impl SessionWork {
    /// The plex.tv account token this work runs for — empty for a sign-in, which has none yet.
    /// What `plex::grant::PlaintextAsk::capture` keys the person's answers by.
    pub fn account_token(&self) -> &str {
        match self {
            Self::Login { .. } => "",
            Self::Rediscover { account_token, .. } | Self::HomeRoster { account_token, .. } => account_token,
            Self::ServerRoster { session, .. } | Self::ProfileSwitch { session, .. }
                | Self::Endpoint { session, .. } => &session.account_token,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ProfilePublication {
    pub epoch: u64,
    pub profile: Option<UserRef>,
    pub scope: ProfileScope,
    /// `grant::account_key` of the account token the profile is published under: the half of the
    /// seat (`plex::session::Seat`) that says which sign-in the profile belongs to. Empty with no
    /// account token, and in a recording made before the field existed. An empty stamp is left off
    /// the wire: a controlled replay grades the regenerated effect as complete canonical JSON
    /// against the committed recordings, which hold `{epoch, profile, scope}` alone.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stamp: String,
}

/// Credentials-owned patch data. The adapter merges it into the current disk value, retaining
/// newer favourite/search/quality/ambient fields owned by other machines.
#[derive(Clone, Serialize, Deserialize)]
pub struct CredentialPatch {
    pub client_id: String,
    pub account_token: String,
    pub server: plx_plex::plex::session::ServerRef,
    pub user: UserRef,
    pub home_users: Vec<plx_plex::plex::session::HomeUserRef>,
    pub sources: Vec<plx_plex::plex::session::SourceRef>,
    pub profiles: Vec<plx_plex::plex::session::ProfileCreds>,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum Command {
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
    RequestEndpoint { #[serde(with = "super::observation::server_id")] sid: plx_plex::plex::ServerId },
    TakeReady,
    /// Answer the currently shown [`PersistenceWarning`]. A key that does not match the warning
    /// currently held is inert — it may be stale (a newer warning replaced it).
    AcknowledgePersistenceWarning { key: PersistenceWarningKey },
    /// The consent decision for the held incident, as the presenting screen derived it at
    /// `revision` (`telemetry::consent::revision`). Stale ids and unchanged revisions are inert.
    ResolveIncident { id: u32, permission: plx_telemetry::telemetry::consent::Permission, revision: u32 },
    /// Send report — from the incident alert or from Details. A person's press, and the whole of
    /// the one-off report's consent.
    ReportIncident { id: u32 },
    /// Not now: the offer is answered for this launch.
    DeclineIncident { id: u32 },
    /// The person's answer to "Connect without encryption?" (or Settings' switch) for one server
    /// — every consent surface sends this one command. On the sign-in read-out it answers the
    /// offer shown there ([`SessionInit::plaintext`]; another machine is inert) and an *Allowed*
    /// retries the discovery. Signed in, it answers a server Home, the Library or Settings showed
    /// (`plex::grant::offers`, or an earlier answer), and an *Allowed* re-finds `sid`'s endpoint —
    /// or the whole roster when the server has no slot — so a fresh probe can mint the grant.
    AnswerPlaintext {
        machine_id: String,
        choice: plx_plex::plex::session::PlaintextChoice,
        #[serde(with = "super::observation::optional_server_id")]
        sid: Option<plx_plex::plex::ServerId>,
    },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct ReplyTo { pub instance: u32, pub correlation: u32 }

/// Ordered application/coordinator work; the owner describes it without invoking another
/// machine, platform API or global publication from inside its transition.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum CoordinatorAction {
    CloseTelemetry,
    LocalDataErased,
    SignInStarted,
    SignInCompleted,
    SignInCancelled,
    SignInFailed { phase: Phase },
}

/// What a [`RegistryPlan::Install`] does to the slots already live. The line that matters is
/// between the last two: whether the identity asking the servers CHANGED.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RosterCommit {
    /// Register the roster beside whatever is live — a boot, a picker's cached roster, discovery
    /// progress. Nothing is retired.
    Merge,
    /// The seated profile's roster re-read from plex.tv (the admin's boot reconcile): install in
    /// place and retire only what it no longer lists (`plex::finish_roster_refresh`). With
    /// `same_identity` — the seated profile is provably the account holder whose token listed the
    /// roster (`auth::seated_is_account_holder`) — no live token is revoked, so a server whose
    /// grant merely rotated keeps its resident art. Without it, each live token the roster would
    /// REPLACE is revoked first (`plex::revoke_before_foreign_retoken`): another account's grant
    /// never inherits what the seated identity claimed.
    Refresh { same_identity: bool },
    /// A profile switch's `Ready` seat: every live token is revoked before the new identity's
    /// grants land (`plex::revoke_for_profile_switch`), then the installed set becomes the roster.
    /// The same request's late `ProfileRoster` is a `Refresh { same_identity: true }` of that seat.
    Switch,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum RegistryPlan {
    DevInstall { primary: plx_plex::plex::session::ServerRef, extras: Vec<plx_plex::plex::session::SourceRef>, client_id: String },
    /// Boot picker's avatar client, before any profile is permitted to enter Home.
    Primary { server: plx_plex::plex::session::ServerRef, token: String },
    /// `same_identity`: the grant speaks for the identity already seated, so it may replace a live
    /// token in place. `false` revokes a DIFFERENT live token first — see [`RosterCommit::Refresh`].
    Activate { source: plx_plex::plex::session::SourceRef, ipv6: bool, same_identity: bool },
    Install { sources: Vec<plx_plex::plex::session::SourceRef>, primary: Option<usize>, commit: RosterCommit },
    Endpoint { expected: ServerLifecycle, source: plx_plex::plex::session::SourceRef },
    Probe(super::SettledProbe),
    Revoke,
}

/// Which site's fresh save produced the warning being shown to the user — the discovery write
/// that ran right after sign-in, or the final write `take_ready` issues once discovery settles.
/// Both are `FreshReauthentication` writes; only the site differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistenceWarningSite { Discovery, Final }

/// Identity of the fresh write a `PersistenceWarning` is reporting on, so an acknowledgement can
/// be checked against the exact warning it is answering rather than any warning currently shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceWarningKey { pub epoch: u64, pub req: u32 }

/// A fresh-reauthentication write that did NOT confirm durable, surfaced to the owner/UI and held
/// until the user explicitly acknowledges it — the AUTH-03 gate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceWarning {
    pub key: PersistenceWarningKey,
    pub site: PersistenceWarningSite,
    #[serde(default)]
    pub helper: Option<plx_platform::storage::wire::failure::HelperFailure>,
    #[serde(default)]
    pub candidate_errnos: [Option<i32>; 8],
    /// Closed evidence for the warning; retained independently of the consent-gated report.
    #[serde(default)]
    pub persistence: Option<PersistenceEvidence>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistenceEvidence {
    class: plx_telemetry::telemetry::incident::PersistenceFailure,
    keymanager_stage: Option<plx_platform::storage::wire::KeymanagerStage>,
    service_error_code: Option<i32>,
}

impl PersistenceWarning {
    pub fn from_outcome(key: PersistenceWarningKey, site: PersistenceWarningSite,
        outcome: &plx_plex::plex::session::async_persistence::CompletionOutcome) -> Self {
        let context = IncidentContext {
            kind: plx_telemetry::telemetry::incident::IncidentKind::SaveFailed,
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
    pub fn incident_context(self) -> Option<IncidentContext> {
        let evidence = self.persistence?;
        Some(IncidentContext {
            kind: plx_telemetry::telemetry::incident::IncidentKind::SaveFailed,
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
pub struct HeldHandoff { pub epoch: u64, pub req: u32 }

#[derive(Clone, Serialize, Deserialize)]
pub struct CommitPlan {
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
    pub authority: plx_plex::plex::session::SaveAuthority,
}

#[derive(Clone, Copy, Default)]
enum CommitDestination {
    #[default]
    Unchanged,
    Roster,
    ReadyCommitted,
    SignInProfiles,
    SignInReady,
    ProfileReady,
    StoredReady,
}

impl CommitDestination {
    // The legacy patch is decoded once, at the compatibility boundary, never by a live transition.
    fn from_init(delta: &CommitDelta) -> Self {
        match delta.phase {
            Some(Phase::Profiles) => Self::SignInProfiles,
            Some(Phase::Ready) if delta.profile_seated => Self::ProfileReady,
            Some(Phase::Ready) if delta.complete_signin => Self::SignInReady,
            Some(Phase::Ready) => Self::StoredReady,
            Some(Phase::Idle | Phase::Creating | Phase::Waiting | Phase::Discovering
                | Phase::Switching | Phase::Error | Phase::Deleted) | None => {
                if delta.ready == Some(false) { Self::ReadyCommitted }
                else if delta.users.is_some() { Self::Roster }
                else { Self::Unchanged }
            }
        }
    }
}

/// Only the changes whose side effects are awaiting acknowledgement. This is not a second
/// controller snapshot: unchanged fields remain solely in SessionInit.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct CommitDelta {
    #[serde(skip)]
    destination: CommitDestination,
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
    /// plex.tv answered this identity with a roster (a sign-in's own, or the picker's refresh):
    /// the question [`SessionInit::switch_refused_for`] recorded a refusal to is answered again,
    /// so the verdict goes with the commit that carries the answer.
    #[serde(default)]
    pub roster_answered: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub enum DevCommitDelta { Activated, StartAccount { login_req: u32 } }

/// The picker's read-outs when it has no tiles and cannot get any. Neutral wording, drawn as a
/// failed read-out on the picker itself (`screens/profiles.rs`), never as a sign-in failure.
pub fn roster_unreachable() -> &'static str {
    plx_platform::i18n::msg::browse_auth_roster_unreachable()
}
pub fn roster_refused() -> &'static str {
    plx_platform::i18n::msg::browse_auth_roster_refused()
}

#[derive(Clone, Serialize, Deserialize)]
pub enum BootstrapAuthority {
    Account { extras: Vec<plx_plex::plex::session::SourceRef> },
    DevPms { primary: plx_plex::plex::session::ServerRef, extras: Vec<plx_plex::plex::session::SourceRef> },
}

#[derive(Clone, Serialize, Deserialize)]
pub enum ReadyInstall {
    PrimaryAndExtras(Vec<plx_plex::plex::session::SourceRef>),
    AlreadyInstalled,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PendingCommit {
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
pub struct CommitPhase {
    /// Authority was current; a durable write was admitted and enqueued, not yet durable.
    pub admitted: bool,
    /// Storage CONFIRMED durability for the admitted revision. Still false while merely enqueued,
    /// and still false for a completion that was fenced out.
    pub durable: bool,
    /// That confirmed durable write had a purpose which may stand as evidence a login was saved.
    /// A background refresh leaves this false even when it becomes durable.
    pub proves_saved_login: bool,
}

/// The durable write this owner is waiting on, identified exactly. A completion settles only this
/// identity; anything else is a stale or superseded verdict and must change nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmittedPersistence {
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
pub enum CommitAdmission {
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
    pub fn accepted(self) -> bool {
        !matches!(self, Self::StaleAuthority | Self::Rejected { .. })
    }
    pub fn admitted_revision(self) -> Option<u64> {
        match self {
            Self::Admitted { revision, .. } => Some(revision),
            Self::StaleAuthority | Self::RegistryOnly | Self::Rejected { .. } => None,
        }
    }
    /// The purpose storage was actually asked to write for. This is the authoritative purpose for
    /// fencing a completion: the plan's intent and the admission can disagree, and only what was
    /// enqueued can be answered.
    pub fn admitted_purpose(self) -> Option<PersistencePurpose> {
        match self {
            Self::Admitted { purpose, .. } => Some(purpose),
            Self::StaleAuthority | Self::RegistryOnly | Self::Rejected { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct CommitReply {
    pub req: u32,
    pub epoch: u64,
    pub arrival: u64,
    pub admission: CommitAdmission,
}

/// Execution-time permission borrowed from the sole owner. Not serialized state or an effect:
/// keeping it alive prevents changing the owner while its separate adapter commits resources.
pub struct CommitPermit<'a> {
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
pub enum CaptureIntent {
    Login,
    Profile { tile: UserTile, pin: Option<String> },
    Endpoint { sid: u16 },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum SessionReadRequest {
    LoginClientId,
    ProfilePolicy,
    Endpoint { sid: u16 },
}

#[derive(Clone, Serialize, Deserialize)]
pub enum SessionReadValue {
    LoginClientId(String),
    ProfilePolicy { recently_unreachable: bool },
    Endpoint(Option<EndpointCapture>),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct EndpointCapture {
    pub lifecycle: ServerLifecycle,
    pub machine_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionReadReply {
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub epoch: u64,
    pub value: SessionReadValue,
}

/// One launch per checked request. The explicit result state distinguishes "accepted, no
/// observations yet" from a never-admitted request; an arrival watermark cannot do that.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionId(pub u32);

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AdmissionState {
    NotRequested,
    Awaiting(AdmissionId),
    Accepted(AdmissionId),
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct AdmissionReply {
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub key: SessionWorkKey,
    pub correlation: AdmissionId,
    pub accepted: bool,
}

/// Every variant is data. Closures, native Clients and MainThread cannot enter logical effects.
#[derive(Clone, Serialize, Deserialize)]
pub enum SessionFx {
    Commit { req: u32, epoch: u64, arrival: u64, plan: CommitPlan },
    Pump,
    Acknowledge(Vec<Receipt>),
    Retire { req: u32 },
    Ready { epoch: u64, scope: ProfileScope, server: plx_plex::plex::session::ServerRef, token: String, install: ReadyInstall },
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
    /// Record the person's plaintext answer for one server: this launch's grant authority
    /// (`plex::grant::answer`) and the persisted choice (`Preferences::plaintext_consent`), under
    /// `account` — `plex::grant::account_key` of the account signed in when it was given.
    PlaintextAnswer { machine_id: String, choice: plx_plex::plex::session::PlaintextChoice, account: String },
    /// Read the session cache's authority once and install it (`plex::session::refresh_cache_job`),
    /// installing only while the cache is still at `generation`. The adapter answers EVERY one of
    /// these with at most one [`SessionEvent::CacheRefreshed`] carrying the same `req`: the
    /// adapter when the queue refuses the job or the job ends, the app's frame step when the
    /// adapter that began it was replaced. A controlled run (it has no cache to refill) answers
    /// with none and fails loudly instead. The owner takes only the answer for the request in
    /// flight and drops any other. `req` comes from the owner's own counter, not
    /// [`SessionInit::next_req`]: it names no pending work and never enters the recorded state.
    RefreshCache { req: u32, generation: u64 },
}

pub trait SessionHost: plx_machine::machine::Host {
    fn session_effect(effect: SessionFx) -> Self::Fx;
}

pub enum SessionEvent {
    /// Consume the aggregate dirty marker through the same owner-event boundary.
    ConsumeLogicalDirty,
    /// Legacy incident producers enter with closed evidence, never a mutable owner escape.
    RaiseIncident { flow: IncidentFlow, context: IncidentContext },
    ForgetIncidents,
    #[cfg(any(test, feature = "test-support"))] ReserveWork { op: SessionOp, lifecycle: Option<ServerLifecycle> },
    #[cfg(test)] RebuildPublication,
    #[cfg(test)] RestartLogin { fresh_login: bool },
    #[cfg(test)] ObserveQr(SessionEnvelope),
    #[cfg(test)] FailLogin { message: String, incident: Option<IncidentContext> },
    Command(Command),
    Result(SessionEnvelope),
    Commit(CommitReply),
    /// Resource evidence is delivered even when its original authority permit is gone.
    DiskWrite { before: Identity, after: Identity,
        outcome: plx_plex::plex::session::async_persistence::CompletionOutcome },
    Read(SessionReadReply),
    Pump,
    Admission(AdmissionReply),
    /// A typed durability verdict from the persistence worker, delivered through the ordinary
    /// effect/FIFO path. It is fenced by request/epoch/arrival/revision before it settles anything.
    Persistence(plx_plex::plex::session::async_persistence::PersistenceCompletion),
    Erased { epoch: u64, leftovers: usize },
    /// What became of a [`SessionFx::Incident`]; fenced by the offer's id.
    IncidentReported { id: u32, delivery: IncidentDelivery },
    /// The frame step saw the session cache lacking a read (`plex::session::refresh_need`). Decided
    /// on the frame tick: one [`SessionFx::RefreshCache`], or nothing while one is in flight or its
    /// backoff holds. Changes no recorded state and no publication.
    CacheObserved(plx_plex::plex::session::RefreshNeed),
    /// How a [`SessionFx::RefreshCache`] ended; fenced only by its own `req`, so a sign-out that
    /// advanced the epoch while it ran still clears the in-flight marker.
    CacheRefreshed { req: u32, outcome: plx_plex::plex::session::CacheRefreshOutcome },
}

/// The owner's private bookkeeping for refilling the session cache (`plex::session`'s [`peek`] is a
/// pure read; this is who asks for the read). Lives beside [`SessionInit`], never in it: it is not
/// logical session state, so it stays out of the snapshot, the replay shape and the subhash.
/// Changed only by [`SessionMachine::step`]; time enters only as `cx.tick.ms`.
///
/// [`peek`]: plx_plex::plex::session::peek
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CacheRefresh {
    /// Nothing asked for and nothing held back.
    Idle,
    /// One [`SessionFx::RefreshCache`] is out, waiting for its [`SessionEvent::CacheRefreshed`].
    InFlight { req: u32, generation: u64 },
    /// Hold the next refresh back for [`LOCKED_RETRY_MS`] after the frame tick `armed_ms` it was
    /// armed at: held while `now.wrapping_sub(armed_ms) < LOCKED_RETRY_MS`. That is the unsigned
    /// time since arming, so a backoff nothing ever observed again (its need went away inside the
    /// second) cannot come back to hold a later need after the tick has run 2^31 ms past it, as a
    /// signed compare against a deadline would; it can only alias inside one second of every
    /// 2^32 ms (49.7 days), at the cost of one extra second of hold.
    /// `generation: Some(g)` holds back only observations of cache generation `g` (a transient
    /// read that is still standing: a drop to `Unloaded` bumps the generation and refreshes at
    /// once); `None` holds back every generation (the job was refused or never completed).
    ///
    /// [`LOCKED_RETRY_MS`]: plx_plex::plex::session::LOCKED_RETRY_MS
    Backoff { armed_ms: u32, generation: Option<u64> },
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
pub struct ServerLifecycle {
    pub sid: u16,
    pub instance_gen: u32,
    pub token_gen: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionOp { Login, Rediscover, HomeRoster, ServerRoster, ProfileSwitch, Endpoint(u16), Ready, Picker, DevBoundary }

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWorkKey {
    pub epoch: u64,
    pub op: SessionOp,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
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
pub enum StreamPhase { Running, ProfileSeated }

#[derive(Clone, Serialize, Deserialize)]
pub struct Pending {
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
pub struct ProfileScope(pub u32);

/// Private persisted init data, not diagnostic output. No constructor consults the filesystem.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionInit {
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
    /// The one account retry run currently outstanding during discovery. Its elapsed value is
    /// sampled at the last miss; the screen advances from that anchor with its frame clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_retry: Option<super::DiscoveryRetryProgress>,
    /// The server the failure read-out may offer a consented plaintext connection to, with the
    /// person's answer so far (`auth::PlaintextVerdict`). Set only by an insecure-only discovery
    /// failure whose server is eligible; cleared by every other ending and by a new attempt.
    #[serde(default)]
    pub plaintext: Option<super::PlaintextVerdict>,
    /// **The name plex.tv gave the account that just signed in to find no server** — for the one
    /// read-out that says who signed in (`browse.auth.no_servers_signed_in_as`), composed and
    /// measured by the screen. Personal data, so UI state ONLY: never serialized (a recording's
    /// initial state must not carry it), never hashed into [`Canon`] (it would reach a replay
    /// digest), never logged, never part of an incident. Set by a failed sign-in that carries one,
    /// cleared by every other ending and by a new attempt; read through the private account readout.
    #[serde(skip)]
    pub signin_account: Option<String>,
    /// The identity plex.tv REFUSED a roster to while nothing was cached to switch from — the
    /// verdict behind [`roster_refused`]'s read-out, kept so the account menu stops offering
    /// *Change profile* into that dead end ([`SessionSnapshot::switch_refused`]). Keyed by the
    /// identity, so a profile switch to anyone else reads it as unrefused. The identity's
    /// lifecycle retires it outright: `erase` (sign-out; the same account signed back in is the
    /// same `Identity`), and any commit carrying a roster for it
    /// ([`CommitDelta::roster_answered`] — a sign-in's own or the picker's refresh). In memory
    /// only: a relaunch asks plex.tv again.
    #[serde(default)]
    pub switch_refused_for: Option<Identity>,
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
            link_trouble: false, discovery_retry: None, plaintext: None, signin_account: None,
            switch_refused_for: None }
    }

    pub fn captured_boot(saved: PersistedSession, primary: Option<plx_plex::plex::session::ServerRef>,
        extras: Vec<plx_plex::plex::session::SourceRef>) -> Self {
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

pub fn write_incident_context(w: &mut Canon, context: &plx_telemetry::telemetry::incident::IncidentContext) {
    incident::write_context(w, context);
}

pub fn write_plaintext_verdict(w: &mut Canon, v: &super::PlaintextVerdict) {
    use plx_plex::plex::session::PlaintextChoice as C;
    w.str(&v.machine_id).str(&v.name).str(&v.shared_by).str(v.eligibility.code())
        .u8(match v.choice { C::Undecided => 0, C::Allowed => 1, C::Declined => 2, C::Revoked => 3 });
}

pub fn write_user(w: &mut Canon, user: &UserRef) {
    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb).str(&user.token);
    w.option(user.plex_tv_token.as_ref(), |w, t| { w.str(t); });
}

pub fn write_tile(w: &mut Canon, user: &UserTile) {
    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb)
        .bool(user.protected).bool(user.admin);
}

pub fn write_profile(w: &mut Canon, profile: &plx_plex::plex::session::ProfileCreds) {
    w.str(&profile.uuid);
    write_user(w, &profile.user);
    write_server(w, &profile.server);
    write_sources(w, &profile.sources);
    w.option(profile.pin.as_ref(), |w, pin| { w.str(&pin.salt).str(&pin.hash).u32(pin.iters); });
}

pub fn write_server(w: &mut Canon, server: &plx_plex::plex::session::ServerRef) {
    w.str(&server.name).str(&server.machine_id).str(&server.address)
        .u64(server.port as u64).str(&server.token).str(&server.origin_url);
    write_tier(w, server.tier);
}

pub fn write_tier(w: &mut Canon, tier: Option<plx_plex::plex::probe::Location>) {
    use plx_plex::plex::probe::Location;
    w.u8(match tier { None => 0, Some(Location::Local) => 1,
        Some(Location::Remote) => 2, Some(Location::Relay) => 3 });
}

pub fn write_sources(w: &mut Canon, sources: &[plx_plex::plex::session::SourceRef]) {
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
pub fn write_persisted(w: &mut Canon, s: &PersistedSession) {
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
    if s.language != plx_platform::i18n::Preference::System {
        w.str("language").str(s.language.tag());
    }
    w.seq(s.recent_searches.len());
    for recent in &s.recent_searches {
        w.str(&recent.user).seq(recent.terms.len());
        for term in &recent.terms { w.str(term); }
    }
    w.seq(s.prefs.last_library.len());
    for last in &s.prefs.last_library {
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
    w.option(s.prefs.last_hero_blur, |w, blur| {
        for corner in blur { for channel in corner { w.f32(channel); } }
    });
    w.option(s.prefs.playback_quality, |w, quality| {
        use plx_plex::plex::session::PlaybackQuality;
        w.u8(match quality { PlaybackQuality::Auto => 0, PlaybackQuality::Original => 1,
            PlaybackQuality::P1080High => 2, PlaybackQuality::P1080 => 3,
            PlaybackQuality::P720 => 4, PlaybackQuality::P720Low => 5, PlaybackQuality::P480 => 6 });
    });
}

/// Borrowed V5 logical fields. Omitted wire-only and private values are intentionally absent.
struct LegacyV5Projection<'a> {
    disk_identity: &'a Identity,
    authority: &'a BootstrapAuthority,
    phase: Phase,
    picker: Picker,
    persisted: &'a PersistedSession,
    committed_credentials: &'a CredentialPatch,
    pin_code: &'a String,
    qr_png: &'a Vec<u8>,
    users: &'a Vec<UserTile>,
    error: &'a String,
    pin_denied: bool,
    authorized_in_flow: bool,
    signin_active: bool,
    apply_pending: bool,
    code_replaced: bool,
    qr_gen: u64,
    next_qr: u64,
    epoch: u64,
    next_req: u32,
    delete_leftovers: usize,
    profile_scope: ProfileScope,
    active_profile: &'a Option<UserRef>,
    pending: &'a BTreeMap<u32, Pending>,
    persistence_purpose: Option<PersistencePurpose>,
    commit_phase: &'a CommitPhase,
    persistence_warning: &'a Option<PersistenceWarning>,
    held_handoff: &'a Option<HeldHandoff>,
    persistence_warning_answered: bool,
    pending_commit: &'a Option<PendingCommit>,
    pending_erase: Option<(u64, bool)>,
    pump_pending: bool,
    inbox: &'a VecDeque<SessionEnvelope>,
    incident: &'a Option<IncidentOffer>,
    incidents_seen: &'a Vec<IncidentKey>,
    next_incident: u32,
    link_trouble: bool,
    switch_refused_for: &'a Option<Identity>,
    plaintext: &'a Option<super::PlaintextVerdict>,
    discovery_retry: Option<super::DiscoveryRetryProgress>,
}

impl<'a> LegacyV5Projection<'a> {
    fn from_init(init: &'a SessionInit) -> Self {
        Self {
            disk_identity: &init.disk_identity,
            authority: &init.authority,
            phase: init.phase,
            picker: init.picker,
            persisted: &init.persisted,
            committed_credentials: &init.committed_credentials,
            pin_code: &init.pin_code,
            qr_png: &init.qr_png,
            users: &init.users,
            error: &init.error,
            pin_denied: init.pin_denied,
            authorized_in_flow: init.authorized_in_flow,
            signin_active: init.signin_active,
            apply_pending: init.apply_pending,
            code_replaced: init.code_replaced,
            qr_gen: init.qr_gen,
            next_qr: init.next_qr,
            epoch: init.epoch,
            next_req: init.next_req,
            delete_leftovers: init.delete_leftovers,
            profile_scope: init.profile_scope,
            active_profile: &init.active_profile,
            pending: &init.pending,
            persistence_purpose: init.persistence_purpose,
            commit_phase: &init.commit_phase,
            persistence_warning: &init.persistence_warning,
            held_handoff: &init.held_handoff,
            persistence_warning_answered: init.persistence_warning_answered,
            pending_commit: &init.pending_commit,
            pending_erase: init.pending_erase,
            pump_pending: init.pump_pending,
            inbox: &init.inbox,
            incident: &init.incident,
            incidents_seen: &init.incidents_seen,
            next_incident: init.next_incident,
            link_trouble: init.link_trouble,
            switch_refused_for: &init.switch_refused_for,
            plaintext: &init.plaintext,
            discovery_retry: init.discovery_retry,
        }
    }

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
        for byte in self.qr_png { w.u8(*byte); }
        w.seq(self.users.len());
        for user in self.users {
            w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb)
                .bool(user.protected).bool(user.admin);
        }
        w.str(&self.error).bool(self.pin_denied).bool(self.authorized_in_flow)
            .bool(self.signin_active).bool(self.apply_pending).bool(self.code_replaced)
            .u64(self.qr_gen).u64(self.next_qr).u64(self.epoch).u32(self.next_req)
            .u64(self.delete_leftovers as u64).u32(self.profile_scope.0);
        w.option(self.active_profile.as_ref(), write_user);
        w.seq(self.pending.len());
        for (&req, pending) in self.pending {
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
                .bool(delta.profile_seated).bool(delta.roster_answered);
            w.option(delta.ready, |w, ready| { w.bool(ready); });
        });
        w.option(self.pending_erase, |w, (epoch, sign_in)| { w.u64(epoch).bool(sign_in); });
        w.bool(self.pump_pending).seq(self.inbox.len());
        for envelope in self.inbox { envelope.write(w); }
        w.option(self.incident.as_ref(), incident::write_offer);
        w.seq(self.incidents_seen.len());
        for key in self.incidents_seen { incident::write_key(w, key); }
        w.u32(self.next_incident).bool(self.link_trouble);
        w.option(self.switch_refused_for.as_ref(), |w, id| {
            w.str(&id.client_id).str(&id.account_token).str(&id.profile_uuid);
        });
        // Appended only when present, so every state without an offer keeps its digest.
        if let Some(verdict) = &self.plaintext {
            write_plaintext_verdict(w, verdict);
        }
        if let Some(retry) = self.discovery_retry {
            w.u8(match retry.run {
                super::DiscoveryRetryRun::Resources => 0,
                super::DiscoveryRetryRun::HomeUsers => 1,
            }).u32(retry.misses).u32(retry.elapsed_ms);
        }
    }
}

impl LogicalState for SessionInit {
    fn write(&self, w: &mut Canon) { LegacyV5Projection::from_init(self).write(w); }
    fn probe(&self, out: &mut String) {
        use std::fmt::Write;
        let _ = write!(out, "session phase={:?} requests={} scope={}",
            self.phase, self.pending.len(), self.profile_scope.0);
    }
}

impl LogicalState for SessionMachine {
    fn write(&self, w: &mut Canon) { LegacyV5Projection::from_partition(self.view()).write(w); }
    fn probe(&self, out: &mut String) { use std::fmt::Write; let p = LegacyV5Projection::from_partition(self.view()); let _ = write!(out, "session phase={:?} requests={} scope={}", p.phase, p.pending.len(), p.profile_scope.0); }
}

/// Derived UI facts only. Credentials and writable controller state are not part of the view.
#[derive(PartialEq, Eq)]
pub struct SessionSnapshot {
    pub flow_epoch: u64,
    pub phase: Phase,
    pub qr_generation: u64,
    pub code: Arc<str>,
    pub png: Arc<[u8]>,
    pub code_replaced: bool,
    pub users: Arc<[UserTile]>,
    /// The uuid of the user the saved session seats (empty for none), which the roster opens on:
    /// unlike `profile`, it is still named while the picker is up.
    pub seated: Arc<str>,
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
    pub discovery_retry: Option<super::DiscoveryRetryProgress>,
    /// The failure read-out's plaintext offer — see [`SessionInit::plaintext`]. The screen reads
    /// the server's name and owner from it; nothing here leaves the device.
    pub plaintext: Option<super::PlaintextVerdict>,
    /// Who signed in, for the no-server read-out alone — see [`SessionInit::signin_account`]. The
    /// screen composes and measures the sentence; nothing here leaves the device.
    pub account: Option<Arc<str>>,
    /// **Switching profiles is known to be unavailable for the identity in use** — its roster
    /// was refused with nothing cached (plex.tv's verdict, or a dev-token session that has no
    /// account to ask with). The account menu hides *Change profile* on it; no verdict
    /// (unreachable, never asked) keeps the row, because that row is what asks.
    pub switch_refused: bool,
    /// **BACK from the roster read-out resumes a session inside the app** — the one fact the
    /// read-out needs to offer a *Back* control whose press does what the BACK key does. When
    /// it is `false` the key hands the screen to the television, and a *Back* pill saying
    /// otherwise would lie.
    pub readout_back_resumes: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ProfileRead {
    pub uuid: String,
    pub title: String,
    pub thumb: String,
}

#[derive(Clone, Copy)]
pub struct SessionRead<'a>(pub &'a SessionSnapshot);

/// [`SessionSnapshot::roster_readout`]'s condition, for a reader that holds the fields rather
/// than the snapshot (the Profiles screen keeps its own copies between frames).
pub fn is_roster_readout(phase: Phase, users: &[UserTile], error: &str) -> bool {
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
pub fn roster_readout_entered(before: &SessionSnapshot, after: &SessionSnapshot) -> Option<String> {
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
    fn from_state(state: attempt::PartitionView<'_>) -> Self {
        Self::from_state_counted(state, None, &mut |_| {})
    }
    fn matches_state(&self, state: attempt::PartitionView<'_>) -> bool {
        let partition = state;
        let state = LegacyV5Projection::from_partition(partition);
        self.flow_epoch == state.epoch && self.phase == state.phase && self.qr_generation == state.qr_gen
            && &*self.code == state.pin_code.as_str() && &*self.png == state.qr_png.as_slice()
            && &*self.users == state.users.as_slice() && &*self.error == state.error.as_str()
            && &*self.seated == state.persisted.user.uuid.as_str()
            && self.code_replaced == state.code_replaced && self.pin_denied == state.pin_denied
            && self.scope == state.profile_scope && self.delete_leftovers == state.delete_leftovers
            && self.persistence_warning == *state.persistence_warning
            && self.incident == *state.incident && self.link_trouble == state.link_trouble
            && self.discovery_retry == state.discovery_retry
            && self.plaintext == *state.plaintext
            && self.account.as_deref() == partition.readout_account()
            && self.switch_refused == partition.switch_refused()
            && self.readout_back_resumes == (partition.roster_readout_up() && partition.roster_dead_end()
                && partition.dead_end_resumes())
            && match (&self.profile, state.active_profile) {
                (None, None) => true,
                (Some(read), Some(profile)) => read.uuid == profile.uuid
                    && read.title == profile.title && read.thumb == profile.thumb,
                _ => false,
            }
    }
    fn from_state_counted(state: attempt::PartitionView<'_>, previous: Option<&Self>, built: &mut impl FnMut(usize)) -> Self {
        let partition = state;
        let state = LegacyV5Projection::from_partition(partition);
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
            seated: previous.filter(|old| &*old.seated == state.persisted.user.uuid.as_str())
                .map(|old| Arc::clone(&old.seated)).unwrap_or_else(|| Arc::from(state.persisted.user.uuid.as_str())),
            error, pin_denied: state.pin_denied,
            profile: state.active_profile.as_ref().map(|p| ProfileRead {
                uuid: p.uuid.clone(), title: p.title.clone(), thumb: p.thumb.clone(),
            }), scope: state.profile_scope, delete_leftovers: state.delete_leftovers,
            persistence_warning: *state.persistence_warning, incident: (*state.incident).clone(),
            link_trouble: state.link_trouble, discovery_retry: state.discovery_retry,
            plaintext: (*state.plaintext).clone(),
            account: partition.readout_account().map(Arc::from),
            switch_refused: partition.switch_refused(),
            readout_back_resumes: partition.roster_readout_up() && partition.roster_dead_end()
                && partition.dead_end_resumes() }
    }
}

struct CancelBatch {
    commit: Option<(u32, u64)>,
    requests: Vec<u32>,
    epoch: u64,
}

struct ExitActions {
    cancel: Option<CancelBatch>,
    acknowledge: Vec<Receipt>,
    coordinator: Option<CoordinatorAction>,
}

enum ExitFx {
    Idle(ExitActions),
    Creating(ExitActions),
    Waiting(ExitActions),
    Discovering(ExitActions),
    Profiles(ExitActions),
    Switching(ExitActions),
    Ready(ExitActions),
    Error(ExitActions),
    Deleted(ExitActions),
    Erasing(ExitActions),
}

impl ExitFx {
    fn for_state(state: &attempt::Attempt, actions: ExitActions) -> Self {
        match state {
            attempt::Attempt::Idle(_) => Self::Idle(actions),
            attempt::Attempt::Creating(_) => Self::Creating(actions),
            attempt::Attempt::Waiting(_) => Self::Waiting(actions),
            attempt::Attempt::Discovering(_) => Self::Discovering(actions),
            attempt::Attempt::Profiles(_) => Self::Profiles(actions),
            attempt::Attempt::Switching(_) => Self::Switching(actions),
            attempt::Attempt::Ready(_) => Self::Ready(actions),
            attempt::Attempt::Error(_) => Self::Error(actions),
            attempt::Attempt::Deleted(_) => Self::Deleted(actions),
            attempt::Attempt::Erasing(_) => Self::Erasing(actions),
        }
    }
    fn attempt(state: attempt::PartitionView<'_>, event: &AttemptEvent) -> Self {
        let coordinator = if !state.common.signin_notice.owed() { None }
            else if matches!(event, AttemptEvent::LoginFailed { .. }) { Some(CoordinatorAction::SignInFailed { phase: state.attempt.phase() }) }
            else if matches!(event, AttemptEvent::ResumeBack) { Some(CoordinatorAction::SignInCancelled) }
            else if matches!(event, AttemptEvent::SigninProfiles { .. } | AttemptEvent::SigninReady) { Some(CoordinatorAction::SignInCompleted) }
            else { None };
        Self::for_state(state.attempt, ExitActions { cancel: None, acknowledge: Vec::new(), coordinator })
    }
    fn epoch(state: attempt::PartitionView<'_>) -> Option<Self> {
        let common = state.common;
        let epoch = common.epoch.checked_add(1)?;
        let mut acknowledge: Vec<_> = common.inbox.iter().map(Receipt::of).collect();
        let commit = common.pending_commit.as_ref().map(|commit| {
            if let Some(receipt) = commit.receipt { acknowledge.push(receipt); }
            (commit.req, common.epoch)
        });
        Some(Self::for_state(state.attempt, ExitActions {
            cancel: Some(CancelBatch { commit, requests: common.pending.keys().copied().collect(), epoch }),
            acknowledge,
            coordinator: None,
        }))
    }

    fn epoch_number(&self) -> u64 {
        let actions = match self {
            Self::Idle(actions) => actions,
            Self::Creating(actions) => actions,
            Self::Waiting(actions) => actions,
            Self::Discovering(actions) => actions,
            Self::Profiles(actions) => actions,
            Self::Switching(actions) => actions,
            Self::Ready(actions) => actions,
            Self::Error(actions) => actions,
            Self::Deleted(actions) => actions,
            Self::Erasing(actions) => actions,
        };
        actions.cancel.as_ref().expect("epoch exit has a cancellation").epoch
    }
    fn emit(self, emit: &mut impl FnMut(SessionFx)) {
        let actions = match self {
            Self::Idle(actions) => actions,
            Self::Creating(actions) => actions,
            Self::Waiting(actions) => actions,
            Self::Discovering(actions) => actions,
            Self::Profiles(actions) => actions,
            Self::Switching(actions) => actions,
            Self::Ready(actions) => actions,
            Self::Error(actions) => actions,
            Self::Deleted(actions) => actions,
            Self::Erasing(actions) => actions,
        };
        let ExitActions { cancel, acknowledge, coordinator } = actions;
        if let Some(batch) = cancel {
            if let Some((req, epoch)) = batch.commit {
                emit(SessionFx::Cancel { requests: vec![req], epoch });
            }
            if !acknowledge.is_empty() { emit(SessionFx::Acknowledge(acknowledge)); }
            emit(SessionFx::Cancel { requests: batch.requests, epoch: batch.epoch });
        } else if !acknowledge.is_empty() {
            emit(SessionFx::Acknowledge(acknowledge));
        }
        if let Some(action) = coordinator { emit(SessionFx::Coordinator(action)); }
    }
}

enum EntryWork {
    Capture { req: u32, epoch: u64 },
    Work { req: u32, key: SessionWorkKey, admission: AdmissionId, input: SessionWork },
}

struct CommitEntry { req: u32, epoch: u64, arrival: u64, plan: CommitPlan }
impl CommitEntry { fn emit(self, emit: &mut impl FnMut(SessionFx)) { let Self { req, epoch, arrival, plan } = self; emit(SessionFx::Commit { req, epoch, arrival, plan }); } }
struct PickerSetup { initial: Option<CommitEntry>, server_roster: Option<EntryWork>, home_roster: Option<EntryWork> }
impl EntryWork {
    fn emit(self, emit: &mut impl FnMut(SessionFx)) {
        match self {
            Self::Capture { req, epoch } => emit(SessionFx::Capture { req, epoch, request: SessionReadRequest::LoginClientId }),
            Self::Work { req, key, admission, input } => emit(SessionFx::Work { req, key, admission, input }),
        }
    }
}
struct ReadyEntry { epoch: u64, scope: ProfileScope, server: plx_plex::plex::session::ServerRef, token: String, install: ReadyInstall }
struct EraseEntry { req: u32, epoch: u64, all_local: bool }

// Entry obligations are values of the destination state, not calls into another machine.
enum EntryFx {
    Idle,
    Creating { work: Option<EntryWork>, started: bool },
    Waiting,
    Discovering { work: Option<EntryWork>, started: bool },
    Profiles { setup: Option<PickerSetup> },
    Switching { capture: Option<(u32, u64)> },
    Ready { announcement: Option<ReadyEntry> },
    Error,
    Deleted { announce_erased: bool },
    Erasing { erase: Option<EraseEntry> },
}

impl EntryFx {
    fn retained(state: &attempt::Attempt) -> Self {
        match state {
            attempt::Attempt::Idle(_) => Self::Idle,
            attempt::Attempt::Creating(_) => Self::Creating { work: None, started: false },
            attempt::Attempt::Waiting(_) => Self::Waiting,
            attempt::Attempt::Discovering(_) => Self::Discovering { work: None, started: false },
            attempt::Attempt::Profiles(_) => Self::Profiles { setup: None },
            attempt::Attempt::Switching(_) => Self::Switching { capture: None },
            attempt::Attempt::Ready(_) => Self::Ready { announcement: None },
            attempt::Attempt::Error(_) => Self::Error,
            attempt::Attempt::Deleted(_) => Self::Deleted { announce_erased: false },
            attempt::Attempt::Erasing(_) => Self::Erasing { erase: None },
        }
    }
    fn attempt(state: attempt::PartitionView<'_>, event: &AttemptEvent) -> Self {
        let common = state.common;
        // Erasing retains imported read-out changes, but owes no second erase job.
        if matches!(state.attempt, attempt::Attempt::Erasing(_))
            && !matches!(event, AttemptEvent::BeginErase { .. } | AttemptEvent::FinishErase) {
            return Self::Erasing { erase: None };
        }
        match event {
            AttemptEvent::LoginFailed { .. } => Self::Error,
            AttemptEvent::CodeReplacing => Self::Creating { work: None, started: false },
            AttemptEvent::CodeReady { .. } => Self::Waiting,
            AttemptEvent::Authorized => Self::Discovering { work: None, started: false },
            AttemptEvent::RestartDiscovery { .. } => Self::Discovering { work: None, started: false },
            AttemptEvent::RestartLogin { .. } => Self::Creating { work: None, started: false },
            AttemptEvent::ResumeBack => Self::Ready { announcement: None },
            AttemptEvent::ResumeDevBack => {
                let announcement = if let BootstrapAuthority::DevPms { primary, .. } = &common.authority {
                    Some(ReadyEntry { epoch: common.epoch, scope: common.profile_scope, server: primary.clone(), token: primary.token.clone(), install: ReadyInstall::AlreadyInstalled })
                } else { None };
                Self::Ready { announcement }
            },
            AttemptEvent::BeginDevAccount { .. } => Self::Creating { work: None, started: false },
            AttemptEvent::ActivateDev => {
                let announcement = if let BootstrapAuthority::DevPms { primary, .. } = &common.authority {
                    Some(ReadyEntry { epoch: common.epoch, scope: common.profile_scope, server: primary.clone(), token: primary.token.clone(), install: ReadyInstall::AlreadyInstalled })
                } else { None };
                Self::Ready { announcement }
            },
            AttemptEvent::ReadyExhausted => Self::Profiles { setup: None },
            AttemptEvent::SwitchCommitRefused => Self::Profiles { setup: None },
            AttemptEvent::DevCommitRefused => Self::Error,
            AttemptEvent::SwitchAdmissionRefused => Self::Profiles { setup: None },
            AttemptEvent::OpenDevPicker => Self::Profiles { setup: None },
            AttemptEvent::OpenPicker { .. } => Self::Profiles { setup: None },
            AttemptEvent::ReuseProfile => Self::Ready { announcement: None },
            AttemptEvent::SelectProfile { req } => Self::Switching { capture: Some((*req, common.epoch)) },
            AttemptEvent::SwitchDropped => Self::Profiles { setup: None },
            AttemptEvent::SwitchFailed { .. } => Self::Profiles { setup: None },
            AttemptEvent::SigninProfiles { .. } => Self::Profiles { setup: None },
            AttemptEvent::SigninReady => Self::Ready { announcement: None },
            AttemptEvent::ProfileReady { .. } => Self::Ready { announcement: None },
            AttemptEvent::StoredReady => Self::Ready { announcement: None },
            AttemptEvent::LinkTrouble { .. } => Self::retained(state.attempt),
            AttemptEvent::RosterFailed { .. } => Self::retained(state.attempt),
            AttemptEvent::BeginSignin { .. } => Self::Creating { work: None, started: true },
            AttemptEvent::DiscoveryTrouble { .. } => Self::retained(state.attempt),
            AttemptEvent::DiscoverySettled { .. } => Self::retained(state.attempt),
            AttemptEvent::DiscoveryFailed { .. } => Self::retained(state.attempt),
            AttemptEvent::PlaintextDeclined { .. } => Self::retained(state.attempt),
            AttemptEvent::RosterArrived { .. } => Self::retained(state.attempt),
            AttemptEvent::ReadyCommitted => Self::retained(state.attempt),
            AttemptEvent::EraseAuthority => Self::retained(state.attempt),
            AttemptEvent::EpochAdvanced | AttemptEvent::WorkRetired { .. }
                | AttemptEvent::BeginDevActivation { .. } | AttemptEvent::BeginStoredRestore { .. }
                | AttemptEvent::InstallPicker { .. } | AttemptEvent::BeginReadyCommit { .. } => Self::retained(state.attempt),
            AttemptEvent::BeginErase { epoch, sign_in } => Self::Erasing { erase: Some(EraseEntry { req: common.next_req, epoch: *epoch, all_local: !*sign_in }) },
            AttemptEvent::FinishErase => Self::Deleted { announce_erased: state.attempt.pending_erase().is_some_and(|(_, sign_in)| !sign_in) },
        }
    }

    fn login(state: attempt::PartitionView<'_>, req: u32, discovery: bool, started: bool) -> Self {
        let common = state.common;
        let key = common.pending[&req].key;
        let client_id = &common.persisted.client_id;
        if !discovery && client_id.is_empty() {
            return Self::Creating { work: Some(EntryWork::Capture { req, epoch: key.epoch }), started };
        }
        let input = if discovery {
            SessionWork::Rediscover { client_id: client_id.clone(), account_token: common.persisted.account_token.clone() }
        } else { SessionWork::Login { client_id: client_id.clone() } };
        let work = Some(EntryWork::Work { req, key, admission: AdmissionId(req), input });
        if discovery { Self::Discovering { work, started } } else { Self::Creating { work, started } }
    }
    fn work(&self) -> &EntryWork {
        match self {
            Self::Creating { work, .. } | Self::Discovering { work, .. } => work.as_ref().expect("new sign-in entry owns work"),
            Self::Idle | Self::Waiting | Self::Profiles { .. } | Self::Switching { .. } | Self::Ready { .. }
                | Self::Error | Self::Deleted { .. } | Self::Erasing { .. } => panic!("not a sign-in entry"),
        }
    }
    fn emit(self, emit: &mut impl FnMut(SessionFx)) {
        match self {
            Self::Creating { work, started } | Self::Discovering { work, started } => {
                if started { emit(SessionFx::Coordinator(CoordinatorAction::SignInStarted)); }
                if let Some(work) = work {
                    work.emit(emit);
                }
            }
            Self::Profiles { setup } => {
                if let Some(PickerSetup { initial, server_roster, home_roster }) = setup {
                    if let Some(commit) = initial { commit.emit(emit); }
                    if let Some(work) = server_roster { work.emit(emit); }
                    if let Some(work) = home_roster { work.emit(emit); }
                }
            }
            Self::Switching { capture } => {
                if let Some((req, epoch)) = capture { emit(SessionFx::Capture { req, epoch, request: SessionReadRequest::ProfilePolicy }); }
            }
            Self::Ready { announcement } => {
                if let Some(ReadyEntry { epoch, scope, server, token, install }) = announcement {
                    emit(SessionFx::Ready { epoch, scope, server, token, install });
                }
            }
            Self::Deleted { announce_erased } => {
                if announce_erased { emit(SessionFx::Coordinator(CoordinatorAction::LocalDataErased)); }
            }
            Self::Erasing { erase } => {
                if let Some(EraseEntry { req, epoch, all_local }) = erase { emit(SessionFx::Erase { req, epoch, all_local }); }
            }
            Self::Idle | Self::Waiting | Self::Error => {}
        }
    }
}

enum AttemptEvent {
    LoginFailed { message: String, revoke: bool },
    CodeReplacing,
    CodeReady { generation: u64, code: String, png: Vec<u8> },
    Authorized,
    RestartDiscovery { req: u32 },
    RestartLogin { req: u32 },
    ResumeBack,
    ResumeDevBack,
    BeginDevAccount { boundary_req: u32, login_req: u32 },
    ActivateDev,
    ReadyExhausted,
    SwitchCommitRefused,
    DevCommitRefused,
    SwitchAdmissionRefused,
    OpenDevPicker,
    OpenPicker { kind: Picker, fallback: Vec<UserTile> },
    ReuseProfile,
    SelectProfile { req: u32 },
    SwitchDropped,
    SwitchFailed { message: String, denied: bool },
    SigninProfiles { users: Vec<UserTile> },
    SigninReady,
    ProfileReady { req: u32 },
    StoredReady,
    LinkTrouble { present: bool },
    RosterFailed { message: String },
    BeginSignin { req: u32 },
    DiscoveryTrouble { progress: super::DiscoveryRetryProgress },
    DiscoverySettled { run: super::DiscoveryRetryRun },
    DiscoveryFailed { plaintext: Option<super::PlaintextVerdict>, account: Option<String> },
    PlaintextDeclined { choice: plx_plex::plex::session::PlaintextChoice },
    RosterArrived { users: Vec<UserTile> },
    ReadyCommitted,
    EraseAuthority,
    EpochAdvanced,
    WorkRetired { req: u32 },
    BeginDevActivation { req: u32 },
    BeginStoredRestore { req: u32 },
    InstallPicker { req: u32 },
    BeginReadyCommit { req: u32 },
    BeginErase { epoch: u64, sign_in: bool },
    FinishErase,
}

fn next_attempt(attempt: attempt::Attempt, event: AttemptEvent) -> attempt::Attempt {
match event {
                    AttemptEvent::LoginFailed { message, revoke } => attempt.login_failed(message, revoke),
                    AttemptEvent::CodeReplacing => attempt.code_replacing(),
                    AttemptEvent::CodeReady { generation, code, png } => attempt.code_ready(generation, code, png),
                    AttemptEvent::Authorized => attempt.authorized(),
                    AttemptEvent::RestartDiscovery { req } => attempt.restart_discovery(req),
                    AttemptEvent::RestartLogin { req } => attempt.restart_login(req),
                    AttemptEvent::ResumeBack => attempt.resume_back(),
                    AttemptEvent::ResumeDevBack => attempt.resume_dev_back(),
                    AttemptEvent::BeginDevAccount { boundary_req, login_req } => attempt.begin_dev_account(boundary_req, login_req),
                    AttemptEvent::ActivateDev => attempt.activate_dev(),
                    AttemptEvent::ReadyExhausted => attempt.ready_exhausted(),
                    AttemptEvent::SwitchCommitRefused => attempt.switch_commit_refused(),
                    AttemptEvent::DevCommitRefused => attempt.dev_commit_refused(),
                    AttemptEvent::SwitchAdmissionRefused => attempt.switch_admission_refused(),
                    AttemptEvent::OpenDevPicker => attempt.open_dev_picker(),
                    AttemptEvent::OpenPicker { kind, fallback } => attempt.open_picker(kind, fallback),
                    AttemptEvent::ReuseProfile => attempt.reuse_profile(),
                    AttemptEvent::SelectProfile { req } => attempt.select_profile(req),
                    AttemptEvent::SwitchDropped => attempt.switch_dropped(),
                    AttemptEvent::SwitchFailed { message, denied } => attempt.switch_failed(message, denied),
                    AttemptEvent::SigninProfiles { users } => attempt.signin_profiles(users),
                    AttemptEvent::SigninReady => attempt.signin_ready(),
                    AttemptEvent::ProfileReady { req } => attempt.profile_ready(req),
                    AttemptEvent::StoredReady => attempt.stored_ready(),
                    AttemptEvent::LinkTrouble { present } => attempt.link_trouble(present),
                    AttemptEvent::RosterFailed { message } => attempt.roster_failed(message),
                    AttemptEvent::BeginSignin { req } => attempt.begin_signin(req),
                    AttemptEvent::DiscoveryTrouble { progress } => attempt.discovery_trouble(progress),
                    AttemptEvent::DiscoverySettled { run } => attempt.discovery_settled(run),
                    AttemptEvent::DiscoveryFailed { plaintext, account } => attempt.discovery_failed(plaintext, account),
                    AttemptEvent::PlaintextDeclined { choice } => attempt.plaintext_declined(choice),
                    AttemptEvent::RosterArrived { users } => attempt.roster_arrived(users),
                    AttemptEvent::ReadyCommitted => attempt.ready_committed(),
                    AttemptEvent::EraseAuthority => attempt.erase_authority(),
                    AttemptEvent::EpochAdvanced => attempt.epoch_advanced(),
                    AttemptEvent::WorkRetired { req } => attempt.work_retired(req),
                    AttemptEvent::BeginDevActivation { req } => attempt.begin_dev_activation(req),
                    AttemptEvent::BeginStoredRestore { req } => attempt.begin_stored_restore(req),
                    AttemptEvent::InstallPicker { req } => attempt.install_picker(req),
                    AttemptEvent::BeginReadyCommit { req } => attempt.begin_ready_commit(req),
                    AttemptEvent::BeginErase { epoch, sign_in } => attempt.begin_erase(epoch, sign_in),
                    AttemptEvent::FinishErase => attempt.finish_erase(),
                }
}

fn signin_notice(event: &AttemptEvent) -> Option<attempt::SignInNotice> {
match event {
                    AttemptEvent::RestartLogin { .. } | AttemptEvent::RestartDiscovery { .. } | AttemptEvent::BeginSignin { .. } => Some(attempt::SignInNotice::Owed),
                    AttemptEvent::LoginFailed { .. } | AttemptEvent::ResumeBack | AttemptEvent::SigninProfiles { .. }
                        | AttemptEvent::SigninReady | AttemptEvent::EraseAuthority => Some(attempt::SignInNotice::Settled),
                    AttemptEvent::CodeReplacing | AttemptEvent::CodeReady { .. } | AttemptEvent::Authorized | AttemptEvent::ResumeDevBack
                        | AttemptEvent::BeginDevAccount { .. } | AttemptEvent::ActivateDev | AttemptEvent::ReadyExhausted | AttemptEvent::SwitchCommitRefused
                        | AttemptEvent::DevCommitRefused | AttemptEvent::SwitchAdmissionRefused | AttemptEvent::OpenDevPicker | AttemptEvent::OpenPicker { .. }
                        | AttemptEvent::ReuseProfile | AttemptEvent::SelectProfile { .. } | AttemptEvent::SwitchDropped | AttemptEvent::SwitchFailed { .. }
                        | AttemptEvent::ProfileReady { .. } | AttemptEvent::StoredReady | AttemptEvent::LinkTrouble { .. }
                        | AttemptEvent::RosterFailed { .. } | AttemptEvent::DiscoveryTrouble { .. }
                        | AttemptEvent::DiscoverySettled { .. } | AttemptEvent::DiscoveryFailed { .. } | AttemptEvent::PlaintextDeclined { .. }
                        | AttemptEvent::RosterArrived { .. } | AttemptEvent::ReadyCommitted | AttemptEvent::EpochAdvanced | AttemptEvent::WorkRetired { .. }
                        | AttemptEvent::BeginDevActivation { .. } | AttemptEvent::BeginStoredRestore { .. }
                        | AttemptEvent::InstallPicker { .. } | AttemptEvent::BeginReadyCommit { .. }
                        | AttemptEvent::BeginErase { .. } | AttemptEvent::FinishErase => None
                }
}

struct WorkReservation { req: u32, pending: Pending }
struct CommitPreparation { pending: PendingCommit, entry: CommitEntry }
enum LoginRestart { Fresh, Retry }

pub struct SessionMachine {
    state: attempt::Attempt,
    common: attempt::Common,
    publication: Arc<SessionSnapshot>,
    subhash: u64,
    logical_dirty: bool,
    /// See [`CacheRefresh`]. Not in `state`, `snapshot_init` or `subhash`.
    cache_refresh: CacheRefresh,
    next_cache_req: u32,
    #[cfg(test)]
    publication_payload_allocations: [usize; 3],
}

impl SessionMachine {
    pub fn from_init(state: SessionInit) -> Self {
        assert!(state.inbox.len() + usize::from(state.pending_commit.as_ref().is_some_and(|c| c.receipt.is_some()))
            <= SESSION_TRANSFER_RECORDS, "invalid Session transfer state");
        let attempt::Partition { common, attempt: state } = attempt::Partition::from_init(state);
        let view = attempt::PartitionView { common: &common, attempt: &state };
        let mut canon = Canon::new();
        LegacyV5Projection::from_partition(view).write(&mut canon);
        let subhash = canon.finish();
        let publication = Arc::new(SessionSnapshot::from_state(view));
        Self { state, common, publication, subhash, logical_dirty: true,
            cache_refresh: CacheRefresh::Idle, next_cache_req: 0,
            #[cfg(test)] publication_payload_allocations: [0; 3] }
    }
fn plan_reservation(&self, op: SessionOp, lifecycle: Option<ServerLifecycle>) -> Option<WorkReservation> {
    let req = self.common.next_req.checked_add(1)?;
    Some(WorkReservation { req, pending: Pending {
        key: SessionWorkKey { epoch: self.common.epoch, op },
        expected: Identity::of(&self.common.persisted), lifecycle,
        last_arrival: None, phase: StreamPhase::Running, capture: None, admission: AdmissionState::NotRequested,
    } })
}

fn plan_commit(&self, req: u32, arrival: u64, terminal: bool, plan: CommitPlan, delta: CommitDelta) -> Option<CommitPreparation> {
    if self.common.pending_commit.is_some() { return None; }
    let pending = self.common.pending.get(&req)?;
    if pending.key.epoch != self.common.epoch { return None; }
    let epoch = pending.key.epoch;
    let fresh = plan.writes_durable && plan.credentials.is_some()
        && plan.authority == plx_plex::plex::session::SaveAuthority::FreshReauthentication;
    Some(CommitPreparation { pending: PendingCommit { req, epoch, arrival, terminal,
        writes_credentials: plan.writes_durable && plan.credentials.is_some(),
        receipt: None, delta, admitted_revision: None,
        purpose: plan.writes_durable.then_some(plan.purpose), fresh,
    }, entry: CommitEntry { req, epoch, arrival, plan } })
}

fn publication_update(&self) -> Option<(Arc<SessionSnapshot>, [usize; 3])> {
        if self.publication.matches_state(self.view()) { return None; }
        let mut allocations = [0; 3];
        let next = SessionSnapshot::from_state_counted(self.view(), Some(&self.publication), &mut |field| allocations[field] += 1);
        Some((Arc::new(next), allocations))
    }
    #[cfg(test)] fn fail_login(&mut self, message: &str, incident: Option<IncidentContext>, emit: &mut impl FnMut(SessionFx)) { let mut effects = Vec::new(); self.transition(&SessionEvent::FailLogin { message: message.into(), incident }, 0, &mut effects); effects.into_iter().for_each(emit); }
    // Required controlled-init boundary; full AppInit capture remains a following stage.
    pub fn snapshot_init(&self) -> SessionInit { self.view().snapshot_init() }
    fn view(&self) -> attempt::PartitionView<'_> { attempt::PartitionView { common: &self.common, attempt: &self.state } }
    pub fn read(&self) -> SessionRead<'_> { self.publication.read() }
    pub fn publication(&self) -> Arc<SessionSnapshot> { Arc::clone(&self.publication) }
    /// The request of the [`SessionFx::RefreshCache`] still awaiting its answer, if any. The app's
    /// frame step compares it with the adapter's own slot to reconcile a job the adapter began and
    /// then lost.
    pub fn cache_refresh_in_flight(&self) -> Option<u32> {
        match self.cache_refresh { CacheRefresh::InFlight { req, .. } => Some(req), CacheRefresh::Idle | CacheRefresh::Backoff { .. } => None }
    }
    pub fn subhash(&self) -> u64 { self.subhash }
    // Cached hashes already change on logical transitions; aggregate dirty consumption is
    // separate from UI Arc damage and remains a following-stage App recording integration.
    pub fn take_logical_dirty(&mut self) -> bool { self.transition(&SessionEvent::ConsumeLogicalDirty, 0, &mut Vec::new()) }



    pub fn ready_is_current(&self, epoch: u64, scope: ProfileScope) -> bool {
        self.common.epoch == epoch && self.common.profile_scope == scope
            && self.state.phase() == Phase::Ready && !self.state.handoff_pending()
    }

    pub fn needs_ready_commit(&self) -> bool {
        self.state.phase() == Phase::Ready && self.state.handoff_pending() && self.common.pending_commit.is_none()
            && self.common.persistence_warning.is_none()
    }

    pub fn publication_is_current(&self, publication: &ProfilePublication) -> bool {
        publication.epoch == self.common.epoch && publication.scope == self.common.profile_scope
    }

    pub fn read_is_current(&self, req: u32, epoch: u64, request: SessionReadRequest) -> bool {
        if epoch != self.common.epoch { return false; }
        let Some(pending) = self.common.pending.get(&req) else { return false };
        if pending.key.epoch != epoch || !pending.expected.matches(&self.common.persisted)
            || !self.state.owns_work(req, pending) { return false; }
        match (&pending.capture, request) {
            (Some(CaptureIntent::Login), SessionReadRequest::LoginClientId)
            | (Some(CaptureIntent::Profile { .. }), SessionReadRequest::ProfilePolicy) => true,
            (Some(CaptureIntent::Endpoint { sid: a }), SessionReadRequest::Endpoint { sid: b }) => *a == b,
            _ => false,
        }
    }

    /// Physical completion remains evidence even after its authority permit was superseded.
    /// Advance only the comparison fence; never publish obsolete credentials as trusted state.
    #[cfg(test)] fn observe_disk_write(&mut self, before: &Identity, after: &Identity, outcome: plx_plex::plex::session::async_persistence::CompletionOutcome) -> bool { self.transition(&SessionEvent::DiskWrite { before: before.clone(), after: after.clone(), outcome }, 0, &mut Vec::new()) }

    /// Read-only authorization at effect execution, after any carried cancellation command.
    /// The bridge checks before admission and again when the worker completes. Supersession
    /// cancels pending work; a write already completed still reports its physical disk evidence.
    pub fn commit_is_current(&self, req: u32, epoch: u64, arrival: u64) -> bool {
        self.common.epoch == epoch
            && self.common.pending.get(&req).is_some_and(|pending| self.state.owns_work(req, pending))
            && self.common.pending_commit.as_ref().is_some_and(|commit|
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
    pub fn commit_permit_for(&self, req: u32, epoch: u64, arrival: u64) -> Option<CommitPermit<'_>> {
        self.commit_is_current(req, epoch, arrival).then_some(CommitPermit {
            req, epoch, arrival, owner: std::marker::PhantomData,
        })
    }













    #[cfg(test)] fn resume_stored(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool { let mut effects = Vec::new(); let handled = self.transition(&SessionEvent::Command(Command::ResumeStored), 0, &mut effects); effects.into_iter().for_each(emit); handled }

    /// Settle a durability verdict ONLY if it still describes the write this owner is waiting on.
    ///
    /// Fenced by request, epoch, arrival and revision together. A completion that fails the fence
    /// is inert: it must not activate registry or profile, must not clear a newer error, and must
    /// not release admission credit that belongs to a different operation. The owner keeps only
    /// this logical identity; channels and receipts stay in the adapter.


    #[cfg(test)] fn apply_commit_reply(&mut self, reply: CommitReply, emit: &mut impl FnMut(SessionFx)) -> bool { let mut effects = Vec::new(); let handled = self.transition(&SessionEvent::Commit(reply), 0, &mut effects); effects.into_iter().for_each(emit); handled }



    /// The single site that publishes a profile and announces `SessionFx::Ready`. A fresh
    /// handoff reaches it only once its write is released — durably confirmed, or its warning
    /// acknowledged; a non-fresh (`Routine`) commit reaches it immediately, exactly as before.




    /// The AUTH-03 acknowledgement door: clears the warning and, if a handoff is still held for
    /// it, releases it — exactly ONE `SessionFx::Ready` for the flow, through
    /// [`Self::release_held_handoff`], never a second one from here.





    pub fn work_is_current(&self, req: u32, key: SessionWorkKey, admission: AdmissionId) -> bool {
        key.epoch == self.common.epoch && self.common.pending.get(&req).is_some_and(|pending|
            pending.key == key && self.state.owns_work(req, pending) && pending.capture.is_none()
                && pending.admission == AdmissionState::Awaiting(admission)
                && pending.expected.matches(&self.common.persisted))
    }





    fn owns_receipt(&self, receipt: Receipt) -> bool {
        self.common.inbox.iter().any(|record| Receipt::of(record) == receipt)
            || self.common.pending_commit.as_ref().and_then(|commit| commit.receipt) == Some(receipt)
    }





























    fn roster_plan(next: &PersistedSession, probes: &[super::SettledProbe], commit: RosterCommit)
        -> Vec<RegistryPlan> {
        let primary = next.sources.iter().position(|source| source.machine_id == next.server.machine_id);
        let mut plans = vec![RegistryPlan::Install { sources: next.sources.clone(), primary, commit }];
        plans.extend(probes.iter().cloned().map(RegistryPlan::Probe));
        plans
    }

    /// A roster request ended without tiles. With cached tiles on screen nothing changes — they
    /// are still the household, and offline switching reads them. With NONE, the picker says so
    /// on its own route: `Phase::Profiles` with the reason in `error` and no tiles is the
    /// read-out `screens/profiles.rs` draws, and [`SessionInit::roster_dead_end`] is what lets BACK leave
    /// it. It used to be `fail_login`, i.e. `Phase::Error` — which the app routes to the SIGN-IN
    /// screen ("Couldn't sign in", whose Try again starts a new QR sign-in) for a person who is
    /// signed in, and whose BACK the Change-profile picker's root rule then refused (#132).


    /// [`Self::fail_empty_home_roster`] for plex.tv's VERDICT: over nothing cached it is the dead
    /// end itself, so it is also remembered for this identity and the account menu stops offering
    /// the way in (`SessionInit::refuse_switch`). Over cached tiles it changes nothing, like any
    /// other empty answer.




    /// Start/restart is one owner decision. Check both counters before cancelling anything:
    /// exhaustion must not detach the still-live operation or reuse its resource address.
    #[cfg(test)] fn restart_login(&mut self, fresh_login: bool, emit: &mut impl FnMut(SessionFx)) -> bool { let mut effects = Vec::new(); let handled = self.transition(&SessionEvent::RestartLogin { fresh_login }, 0, &mut effects); effects.into_iter().for_each(emit); handled }

    /// **The sign-in read-out's *Try again*.** Over an eligible server the person said *Not now*
    /// to (or turned off), it is also how they are asked again — Settings, where a signed-in person
    /// changes that answer, is out of reach before sign-in — so the answer is withdrawn (recorded
    /// `Undecided`, through the same [`SessionFx::PlaintextAnswer`]) BEFORE the retry's work
    /// captures the answers, and the retry that follows asks.


    /// **The answer to "Connect without encryption?"** — recorded through
    /// [`SessionFx::PlaintextAnswer`] first, so the work an *Allowed* starts captures it
    /// (`plex::grant::PlaintextAsk`).
    ///
    /// On the sign-in read-out it must answer the server shown there: *Connect* retries the
    /// discovery, *Not now* re-words the read-out (its primary becomes *Try again*, which
    /// withdraws the answer and asks again — [`Self::retry`]) and starts nothing. Signed in (Home's or the Library's read-out, Settings), any server may be
    /// answered; an *Allowed* re-finds `sid`'s endpoint, or the whole roster without one. Nothing
    /// is recorded without an account to bind it to.
    #[cfg(test)] fn answer_plaintext(&mut self, machine_id: &str, choice: plx_plex::plex::session::PlaintextChoice, sid: Option<plx_plex::plex::ServerId>, emit: &mut impl FnMut(SessionFx)) -> bool { let mut effects = Vec::new(); let handled = self.transition(&SessionEvent::Command(Command::AnswerPlaintext { machine_id: machine_id.into(), choice, sid }), 0, &mut effects); effects.into_iter().for_each(emit); handled }

    /// End the flow on the error read-out. **Every ending names its incident**: `Some` is the
    /// failure's own closed evidence and is raised here, so the read-out's Details and Send report
    /// are about THIS failure; `None` is an ending the onboarding report does not cover, and it
    /// retires whatever is held, which explains an earlier failure and not this one.


    /// QR observations need no external commit. SignedIn and registry/profile facts go through
    /// the separate resource commit protocol, rather than treating their side effects as reads.
    #[cfg(test)] fn apply_qr_observation(&mut self, envelope: &SessionEnvelope, emit: &mut impl FnMut(SessionFx)) -> bool { let mut effects = Vec::new(); let handled = self.transition(&SessionEvent::ObserveQr(envelope.clone()), 0, &mut effects); effects.into_iter().for_each(emit); handled }

    /// The checked allocator is independent from the full-width auth epoch. Exhaustion cannot
    /// reuse an address, including when a cancelled worker still owns a Landing reservation.
    #[cfg(any(test, feature = "test-support"))] fn allocate(&mut self, op: SessionOp, lifecycle: Option<ServerLifecycle>) -> Option<u32> { self.transition(&SessionEvent::ReserveWork { op, lifecycle }, 0, &mut Vec::new()).then_some(self.common.next_req) }

    /// Admission to logical application, not a resource reservation. Landing owns the latter.
    /// Merely inspecting an invalid envelope must not mutate pending state or its publication.
    fn accepts_header(&self, envelope: &SessionEnvelope) -> bool {
        if envelope.addr.to != MachineId::Session { return false; }
        let Some(pending) = self.common.pending.get(&envelope.addr.req.0) else { return false; };
        pending.key == envelope.key && pending.key.epoch == self.common.epoch
            && self.state.owns_work(envelope.addr.req.0, pending)
            && pending.capture.is_none()
            && matches!(pending.admission, AdmissionState::Awaiting(id) | AdmissionState::Accepted(id) if id == envelope.admission)
            && pending.last_arrival.is_none_or(|last| envelope.arrival > last)
    }

    fn accepts(&self, envelope: &SessionEnvelope) -> bool {
        if !self.accepts_header(envelope) { return false; }
        let pending = &self.common.pending[&envelope.addr.req.0];
        match &envelope.outcome {
                SessionArrival::Data(data) => data.matches_request(pending, envelope.terminal)
                    && (!matches!(pending.key.op, SessionOp::Endpoint(_))
                        || pending.lifecycle == envelope.lifecycle),
                SessionArrival::Dropped | SessionArrival::Refused => envelope.terminal,
        }
    }

    /// The frame step reports what the cache lacks. At most one refresh is out at a time; a backoff
    /// holds the next one back on the frame clock; and the first transient observation of a cache
    /// entry arms the backoff instead of reading, so a boot with the storage helper down retries
    /// about a second later, as the in-line retry timer did, not on frame one.


    /// A refresh ended. Only the request in flight can end it: a duplicate or stale answer is dropped.


    /// Apply one addressed event, preserving the owner's effect order.
fn transition(&mut self, ev: &SessionEvent, tick_ms: u32, fx: &mut Vec<SessionFx>) -> bool {
        let mut publish = false;
let mut pump = false;
let mut restart = None;
let mut restart_reply = None;
let mut handled = match ev {
SessionEvent::ConsumeLogicalDirty => {
 std::mem::take(&mut self.logical_dirty)
},
SessionEvent::RaiseIncident { flow, context } => { publish = true; 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (*flow, *context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

}; true },
SessionEvent::ForgetIncidents => { publish = true; {

        self.common.incident = None;
        self.common.incidents_seen.clear();
        { let event = AttemptEvent::LinkTrouble { present: false }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |_| {}); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |_| {}); true };

}; true },
#[cfg(any(test, feature = "test-support"))] SessionEvent::ReserveWork { op, lifecycle } => (('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (*op, *lifecycle); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) })).is_some(),
#[cfg(test)] SessionEvent::RebuildPublication => { { publish = true; }; true },
#[cfg(test)] SessionEvent::RestartLogin { fresh_login } => { restart = Some(if *fresh_login { LoginRestart::Fresh } else { LoginRestart::Retry }); true },
#[cfg(test)] SessionEvent::ObserveQr(envelope) => 'apply_qr_observation: {
let (envelope,): (&SessionEnvelope,) = (envelope,);

        if !self.accepts(envelope)
            || !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
            break 'apply_qr_observation false;
        }
        let req = envelope.addr.req.0;
        if !self.common.pending[&req].expected.matches(&self.common.persisted) { break 'apply_qr_observation false; }
        match &envelope.outcome {
            SessionArrival::Refused | SessionArrival::Dropped => {
                if !envelope.terminal { break 'apply_qr_observation false; }
                let (message, class) = match (envelope.key.op, &envelope.outcome) {
                    (SessionOp::Rediscover, SessionArrival::Refused) =>
                        (plx_platform::i18n::msg::browse_auth_rediscover_failed(), InternalClass::WorkerRefused),
                    (_, SessionArrival::Refused) => (plx_platform::i18n::msg::browse_auth_start_failed(), InternalClass::WorkerRefused),
                    _ => (plx_platform::i18n::msg::browse_auth_finish_failed(), InternalClass::WorkerDropped),
                };
                {
let (message, incident,): (&str, Option<IncidentContext>,) = (message, Some(IncidentContext::internal(class)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
            }
            SessionArrival::Data(data) => {
                let super::observation::Observation::Login(progress) = &**data else { break 'apply_qr_observation false };
                use super::LoginProgress;
                let (epoch, terminal) = match progress {
                    LoginProgress::CodeReplacing { epoch }
                    | LoginProgress::CodeReady { epoch, .. }
                    | LoginProgress::Authorized { epoch, .. }
                    | LoginProgress::DiscoveryTrouble { epoch, .. }
                    | LoginProgress::DiscoveryRetrySettled { epoch, .. }
                    | LoginProgress::LinkTrouble { epoch, .. } => (*epoch, false),
                    LoginProgress::Failed { epoch, .. } => (*epoch, true),
                    LoginProgress::SignedIn { .. } => break 'apply_qr_observation false,
                };
                if epoch != envelope.key.epoch || terminal != envelope.terminal { break 'apply_qr_observation false; }
                match progress {
                    LoginProgress::CodeReplacing { .. } => {
                        if envelope.key.op != SessionOp::Login || !matches!(self.state.phase(), Phase::Creating | Phase::Waiting) { break 'apply_qr_observation false; }
                        { let event = AttemptEvent::CodeReplacing; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::CodeReady { code, qr_png, .. } => {
                        if envelope.key.op != SessionOp::Login { break 'apply_qr_observation false; }
                        let Some(next) = self.common.next_qr.checked_add(1) else {
                            {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_start_failed(), Some(IncidentContext::internal(InternalClass::Exhausted)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
                            self.common.pending.remove(&req);
                            fx.push(SessionFx::Cancel { requests: vec![req], epoch: self.common.epoch });
                            fx.push(SessionFx::Retire { req });
                            { publish = true; };
                            break 'apply_qr_observation true;
                        };
                        self.common.next_qr = next;
                        { let event = AttemptEvent::CodeReady {
                            generation: next, code: code.clone(), png: qr_png.clone(),
                        }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::Authorized { token, .. } => {
                        if envelope.key.op != SessionOp::Login { break 'apply_qr_observation false; }
                        // A sign-in that is not a sign-out (a stored session that could not be
                        // activated, *Use another account*, the dev login trigger) starts from the
                        // stored record. Whoever that record seated, its roster and its offline
                        // profile cache belong to the authorization this token replaces, so none of
                        // them may ride into the new account: `SignedIn` writes the roster and
                        // `Session::account` names a single-user account from it; a Home account
                        // seats through the picker. Reset BEFORE `expected` is recomputed below.
                        // The new token cannot say whose cache this is (every sign-in mints one),
                        // so the cache is emptied while the identity is unknown and `SignedIn`
                        // restores the STORED record's when its roster names the same owner.
                        self.common.persisted.user = UserRef::default();
                        self.common.persisted.home_users.clear();
                        self.common.persisted.profiles.clear();
                        self.common.persisted.account_token = token.clone();
                        { let event = AttemptEvent::Authorized; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                        self.common.pending.get_mut(&req).unwrap().expected = Identity::of(&self.common.persisted);
                    }
                    LoginProgress::DiscoveryTrouble { progress, .. } => {
                        if !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
                            break 'apply_qr_observation false;
                        }
                        { let event = AttemptEvent::DiscoveryTrouble { progress: *progress }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::DiscoveryRetrySettled { run, .. } => {
                        if !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
                            break 'apply_qr_observation false;
                        }
                        { let event = AttemptEvent::DiscoverySettled { run: *run }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::LinkTrouble { trouble, .. } => {
                        if envelope.key.op != SessionOp::Login { break 'apply_qr_observation false; }
                        { let event = AttemptEvent::LinkTrouble {
                            present: trouble.is_some(),
                        }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                        if let Some(context) = trouble {
                            'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, *context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

};
                        }
                    }
                    LoginProgress::Failed { message, incident, plaintext, account, .. } => {
                        {
let (message, incident,): (&str, Option<IncidentContext>,) = (message, Some(*incident),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
                        { let event = AttemptEvent::DiscoveryFailed { plaintext: plaintext.clone(), account: account.clone() }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::SignedIn { .. } => unreachable!(),
                }
            }
        }
        if envelope.terminal {
            self.common.pending.remove(&req);
        } else {
            self.common.pending.get_mut(&req).unwrap().last_arrival = Some(envelope.arrival);
        }
        { publish = true; };
        true

},
#[cfg(test)] SessionEvent::FailLogin { message, incident } => { {
let (message, incident,): (&str, Option<IncidentContext>,) = (message, *incident,);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

}; true },

            SessionEvent::Command(Command::ActivateDevBootstrap) => 'activate_dev: {

        if self.state.phase() != Phase::Idle || self.common.pending_commit.is_some()
            || self.state.pending_erase().is_some() { break 'activate_dev false; }
        let BootstrapAuthority::DevPms { primary, extras } = &self.common.authority else { break 'activate_dev false };
        let registry = vec![RegistryPlan::DevInstall { primary: primary.clone(), extras: extras.clone(),
            client_id: self.common.persisted.client_id.clone() }];
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::DevBoundary, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else { break 'activate_dev false };
        { let event = AttemptEvent::BeginDevActivation { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        'begin_commit: {
let (req, arrival, terminal, plan, delta,): (u32, u64, bool, CommitPlan, CommitDelta,) = (req, 0, true, CommitPlan { registry_client_id: self.common.persisted.client_id.clone(),
            expected_disk: self.common.disk_identity.clone(),
            credentials: None, lifecycle: None, registry,
            purpose: PersistencePurpose::Background, writes_durable: false,
            authority: plx_plex::plex::session::SaveAuthority::Routine }, CommitDelta {
                dev: Some(DevCommitDelta::Activated), ..Default::default()
            },);

        let Some(entry) = ('commit: { let (req, arrival, terminal, plan, delta): (u32, u64, bool, CommitPlan, CommitDelta) = (req, arrival, terminal, plan, delta); let Some(CommitPreparation { pending, entry }) = self.plan_commit(req, arrival, terminal, plan, delta) else { break 'commit None }; self.common.pending.get_mut(&req).unwrap().last_arrival = Some(arrival); self.common.pending_commit = Some(pending); self.common.persistence_purpose = Some(entry.plan.purpose); Some(entry) }) else { break 'begin_commit false };
        entry.emit(&mut |effect| fx.push(effect));
        true

}

},
            SessionEvent::Command(Command::ResumeStored) => 'resume_stored: {

        if !matches!(self.common.authority, BootstrapAuthority::Account { .. }) { break 'resume_stored false; }
        if self.state.phase() != Phase::Idle || !self.common.persisted.can_go_local()
            || self.common.pending_commit.is_some() { break 'resume_stored false; }
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::Ready, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else { break 'resume_stored false };
        let plan = CommitPlan { registry_client_id: self.common.persisted.client_id.clone(),
            expected_disk: self.common.disk_identity.clone(),
            credentials: None, lifecycle: None,
            registry: vec![RegistryPlan::Install { sources: self.common.persisted.sources.clone(),
                primary: None, commit: RosterCommit::Merge }],
            purpose: PersistencePurpose::Background, writes_durable: false,
            authority: plx_plex::plex::session::SaveAuthority::Routine };
        { let event = AttemptEvent::BeginStoredRestore { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        'begin_commit: {
let (req, arrival, terminal, plan, delta,): (u32, u64, bool, CommitPlan, CommitDelta,) = (req, 0, true, plan, CommitDelta {
            destination: CommitDestination::StoredReady,
            phase: Some(Phase::Ready), activate_profile: true, ready: Some(false),
            ..Default::default()
        },);

        let Some(entry) = ('commit: { let (req, arrival, terminal, plan, delta): (u32, u64, bool, CommitPlan, CommitDelta) = (req, arrival, terminal, plan, delta); let Some(CommitPreparation { pending, entry }) = self.plan_commit(req, arrival, terminal, plan, delta) else { break 'commit None }; self.common.pending.get_mut(&req).unwrap().last_arrival = Some(arrival); self.common.pending_commit = Some(pending); self.common.persistence_purpose = Some(entry.plan.purpose); Some(entry) }) else { break 'begin_commit false };
        entry.emit(&mut |effect| fx.push(effect));
        true

}

},
            SessionEvent::Command(Command::StartLogin) => { restart = Some(if true { LoginRestart::Fresh } else { LoginRestart::Retry }); true },
            SessionEvent::Command(Command::Retry) => {

        use plx_plex::plex::session::PlaintextChoice;
        if self.state.phase() == Phase::Error {
            let account = plx_plex::plex::grant::account_key(&self.common.persisted.account_token);
            let answered = self.state.plaintext().filter(|v| v.offers()
                && matches!(v.choice, PlaintextChoice::Declined | PlaintextChoice::Revoked));
            if let (Some(v), false) = (answered, account.is_empty()) {
                fx.push(SessionFx::PlaintextAnswer {
                    machine_id: v.machine_id.clone(), choice: PlaintextChoice::Undecided, account,
                });
            }
        }
        { restart = Some(if false { LoginRestart::Fresh } else { LoginRestart::Retry }); true }

},
            SessionEvent::Command(Command::TakeReady) => 'take_ready: {

        if self.state
            .phase() != Phase::Ready || !self.state
            .handoff_pending()
            || self.common.pending_commit.is_some() { break 'take_ready false; }
        // A discovery failure left a warning unacknowledged: the final fresh write must not run
        // over it (AUTH-03) — acknowledging is what re-admits `needs_ready_commit`.
        if self.common.persistence_warning.is_some() { break 'take_ready false; }
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::Ready, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else {
            { let event = AttemptEvent::ReadyExhausted; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
            { publish = true; };
            break 'take_ready true;
        };
        let mut next = self.common.persisted.clone();
        super::remember_unprotected_active(&mut next);
        let patch = CredentialPatch::of(&next);
        // Consumed exactly once: a discovery/rediscovery retry only PEEKS this flag
        // (`apply_resource_observation`), so it cannot spend the authority the final write needs.
        let fresh = self.state.fresh_authority();
        { let event = AttemptEvent::BeginReadyCommit { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        let authority = if fresh {
            plx_plex::plex::session::SaveAuthority::FreshReauthentication
        } else { plx_plex::plex::session::SaveAuthority::Routine };
        let plan = CommitPlan { registry_client_id: self.common.persisted.client_id.clone(),
            expected_disk: self.common.disk_identity.clone(),
            credentials: Some(patch.clone()), lifecycle: None,
            registry: vec![RegistryPlan::Install { sources: next.sources.clone(), primary: None, commit: RosterCommit::Merge }],
            purpose: PersistencePurpose::Final, writes_durable: true, authority };
        'begin_commit: {
let (req, arrival, terminal, plan, delta,): (u32, u64, bool, CommitPlan, CommitDelta,) = (req, 0, true, plan, CommitDelta {
            destination: CommitDestination::ReadyCommitted,
            credentials: Some(patch), activate_profile: true, ready: Some(false),
            ..Default::default()
        },);

        let Some(entry) = ('commit: { let (req, arrival, terminal, plan, delta): (u32, u64, bool, CommitPlan, CommitDelta) = (req, arrival, terminal, plan, delta); let Some(CommitPreparation { pending, entry }) = self.plan_commit(req, arrival, terminal, plan, delta) else { break 'commit None }; self.common.pending.get_mut(&req).unwrap().last_arrival = Some(arrival); self.common.pending_commit = Some(pending); self.common.persistence_purpose = Some(entry.plan.purpose); Some(entry) }) else { break 'begin_commit false };
        entry.emit(&mut |effect| fx.push(effect));
        true

}

},
            SessionEvent::Command(Command::AcknowledgePersistenceWarning { key }) =>
                'acknowledge_persistence_warning: {
let (key,): (PersistenceWarningKey,) = (*key,);

        if self.common.persistence_warning.map(|warning| warning.key) != Some(key) { break 'acknowledge_persistence_warning false; }
        self.common.persistence_warning = None;
        {

        if self.common.incident.as_ref().is_some_and(|offer|
            offer.key.kind == plx_telemetry::telemetry::incident::IncidentKind::SaveFailed) {
            self.common.incident = None;
        }

};
        // This authorization's unsaved-login question is answered now: storage may never gate
        // entry a second time for it (AUTH-03's field bug). A later failure — Discovery then
        // Final, or a retry — releases the handoff itself instead of asking again.
        self.common.persistence_warning_answered = true;
        if self.common.held_handoff.is_some() {
            {

        self.common.held_handoff = None;
        {
let (profile,): (Option<UserRef>,) = (Some(self.common.persisted.user.clone()),);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        if self.state.phase() == Phase::Ready {
            EntryFx::Ready { announcement: Some(ReadyEntry { epoch: self.common.epoch, scope: self.common.profile_scope,
                server: self.common.persisted.server.clone(), token: self.common.persisted.pms_token().into(),
                install: ReadyInstall::PrimaryAndExtras(match &self.common.authority {
                    BootstrapAuthority::Account { extras } => extras.clone(),
                    BootstrapAuthority::DevPms { .. } => Vec::new(),
                }) }) }.emit(&mut |effect| fx.push(effect));
        }

};
        }
        { publish = true; };
        true

},
            SessionEvent::Command(command @ (Command::ResolveIncident { .. }
                | Command::ReportIncident { .. } | Command::DeclineIncident { .. })) => {
                let handled = {
let (command,): (&Command,) = (command,);

        match command {
            Command::ResolveIncident { id, permission, revision } => {
                'resolve_incident: {
let (id, permission, revision,): (u32, Permission, u32,) = (*id, *permission, *revision,);

        let seen = &self.common.incidents_seen;
        let Some(held) = self.common.incident.as_mut().filter(|held| held.id == id) else { break 'resolve_incident false };
        match held.state {
            IncidentState::Pending => {}
            IncidentState::Offered { revision: at } | IncidentState::OnRequest { revision: at }
                if at != revision => {}
            IncidentState::Offered { .. } | IncidentState::OnRequest { .. } | IncidentState::AutoSending
                | IncidentState::Sending | IncidentState::Queued { .. } | IncidentState::Saved { .. }
                | IncidentState::Delivered { .. } | IncidentState::Failed | IncidentState::NotNow | IncidentState::Dropped => break 'resolve_incident false,
        }
        let key = held.key;
        // Offered behind Details only: a kind the alert never covers, a key this launch has
        // already asked about (the raise that brought it back is Pending over a seen key), and
        // an offer already resolved that way — a later decision may drop it, never escalate it.
        let quiet = !key.kind.alert_eligible()
            || matches!(held.state, IncidentState::OnRequest { .. })
            || (held.state == IncidentState::Pending && seen.contains(&key));
        match permission {
            Permission::Declined => {
                held.state = IncidentState::Dropped;
                held.context = None;
            }
            Permission::Granted | Permission::NotDetermined if quiet => {
                held.state = IncidentState::OnRequest { revision };
            }
            Permission::Granted if key.kind.standing_eligible() => {
                let Some(context) = held.context else { break 'resolve_incident false };
                held.state = IncidentState::AutoSending;
                fx.push(SessionFx::Incident { id, lane: IncidentLane::Standing, report: IncidentReport::Retained(context) });
            }
            Permission::Granted | Permission::NotDetermined => {
                held.state = IncidentState::Offered { revision };
            }
        }
        'remember_seen: {
let (key,): (IncidentKey,) = (key,);

        if self.common.incidents_seen.contains(&key) {
            break 'remember_seen;
        }
        if self.common.incidents_seen.len() >= SEEN_CAP {
            self.common.incidents_seen.remove(0);
        }
        self.common.incidents_seen.push(key);

};
        true

}
            }
            Command::ReportIncident { id } => 'report_incident: {
let (id,): (u32,) = (*id,);

        let Some(held) = self.common.incident.as_mut().filter(|held| held.id == id && held.sendable()) else {
            break 'report_incident false;
        };
        let report = match held.context {
            Some(context) => IncidentReport::Retained(context),
            None => {
                self.common.persistence_warning
                    .filter(|_| held.key.kind == IncidentKind::SaveFailed)
                    .and_then(|warning| warning.incident_context())
                    .map(IncidentReport::Retained)
                    .unwrap_or(IncidentReport::AtPress(held.key))
            },
        };
        let key = held.key;
        held.state = IncidentState::Sending;
        fx.push(SessionFx::Incident { id, lane: IncidentLane::OneOff, report });
        'remember_seen: {
let (key,): (IncidentKey,) = (key,);

        if self.common.incidents_seen.contains(&key) {
            break 'remember_seen;
        }
        if self.common.incidents_seen.len() >= SEEN_CAP {
            self.common.incidents_seen.remove(0);
        }
        self.common.incidents_seen.push(key);

};
        true

},
            Command::DeclineIncident { id } => 'decline_incident: {
let (id,): (u32,) = (*id,);

        let Some(held) = self.common.incident.as_mut().filter(|held| held.id == id) else { break 'decline_incident false };
        if !matches!(held.state, IncidentState::Offered { .. }) {
            break 'decline_incident false;
        }
        held.state = IncidentState::NotNow;
        true

},
            Command::ActivateDevBootstrap | Command::ResumeStored | Command::StartLogin | Command::Retry
                | Command::RestartWait { .. } | Command::StartSwitch(_) | Command::SelectProfile { .. }
                | Command::SelectProfileWithReply { .. } | Command::DismissPinError | Command::BackAtRoot { .. }
                | Command::SignOut | Command::EraseLocal | Command::NoteDeleteLeftovers(_) | Command::RefreshRoster
                | Command::RequestEndpoint { .. } | Command::TakeReady | Command::AcknowledgePersistenceWarning { .. }
                | Command::AnswerPlaintext { .. } => false,
        }

};
                if handled { { publish = true; }; }
                handled
            }
            SessionEvent::Command(Command::StartSwitch(picker)) => 'start_switch: {
let (picker,): (Picker,) = (*picker,);

        if matches!(self.common.authority, BootstrapAuthority::DevPms { .. }) {
            // A dev-token session has no account behind it to list a roster with. The account
            // menu still offers *Change profile* (it reads the on-disk sign-in), and the app has
            // already routed to the picker by the time this runs — so refusing SILENTLY left that
            // route with nothing behind it: a spinner that never ends and a BACK that could not
            // leave (#132, TV session 8). Say so on the picker instead; `back` returns.
            // `picker` is deliberately not recorded: nothing is detached here, so there is no
            // picker-kind rule for `back` to apply, and BACK restores this session state exactly.
            if self.state.phase() != Phase::Ready || self.state.pending_erase().is_some() { break 'start_switch false; }
            let _ = picker;
            { let event = AttemptEvent::OpenDevPicker; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
            self.common.switch_refused_for = Some(Identity::of(&self.common.persisted));
            { publish = true; };
            break 'start_switch true;
        }
        if self.state.pending_erase().is_some() { break 'start_switch false; }
        if ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).is_none() { break 'start_switch false; }
        if self.common.persisted.account_token.is_empty() {
            {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_signed_out(), None,);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
            { publish = true; };
            break 'start_switch true;
        }
        if super::detaches_active_profile(picker) { {
let (profile,): (Option<UserRef>,) = (None,);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

}; }
        { let event = AttemptEvent::OpenPicker { kind: picker, fallback: if self.state.picker_data().users.is_empty() { self.common.persisted.home_users.iter().map(UserTile::of_ref).collect() } else { Vec::new() } }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        let mut initial = None;
        if let Some(req) = 'reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::Picker, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) } {
            let initial_profile = picker == Picker::Boot && self.common.active_profile.is_none()
                && self.common.persisted.can_go_local();
            let mut registry = Vec::new();
            if initial_profile {
                registry.push(RegistryPlan::Primary { server: self.common.persisted.server.clone(),
                    token: self.common.persisted.pms_token().into() });
            }
            registry.push(RegistryPlan::Install {
                sources: self.common.persisted.sources.clone(), primary: None, commit: RosterCommit::Merge,
            });
            let plan = CommitPlan { registry_client_id: self.common.persisted.client_id.clone(),
                expected_disk: self.common.disk_identity.clone(),
                credentials: None, lifecycle: None, registry,
                purpose: PersistencePurpose::Background, writes_durable: false,
                authority: plx_plex::plex::session::SaveAuthority::Routine };
            { let event = AttemptEvent::InstallPicker { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
            initial = 'commit: { let (req, arrival, terminal, plan, delta): (u32, u64, bool, CommitPlan, CommitDelta) = (req, 0, true, plan, CommitDelta {
                activate_profile: initial_profile, ..Default::default()
            }); let Some(CommitPreparation { pending, entry }) = self.plan_commit(req, arrival, terminal, plan, delta) else { break 'commit None }; self.common.pending.get_mut(&req).unwrap().last_arrival = Some(arrival); self.common.pending_commit = Some(pending); self.common.persistence_purpose = Some(entry.plan.purpose); Some(entry) };
        }
        let server_roster = match 'reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::ServerRoster, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) } { Some(req) => Some(
            {
let (req, input,): (u32, SessionWork,) = (req, SessionWork::ServerRoster { session: self.common.persisted.clone(), expected: Identity::of(&self.common.persisted) },);

        let pending = self.common.pending.get_mut(&req).expect("owned work request");
        let admission = AdmissionId(req);
        pending.admission = AdmissionState::Awaiting(admission);
        EntryWork::Work { req, key: pending.key, admission, input }

}), None => None };
        let home_roster = match 'reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::HomeRoster, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) } { Some(req) => Some(
            {
let (req, input,): (u32, SessionWork,) = (req, SessionWork::HomeRoster { client_id: self.common.persisted.client_id.clone(), account_token: self.common.persisted.account_token.clone(), expected: Identity::of(&self.common.persisted) },);

        let pending = self.common.pending.get_mut(&req).expect("owned work request");
        let admission = AdmissionId(req);
        pending.admission = AdmissionState::Awaiting(admission);
        EntryWork::Work { req, key: pending.key, admission, input }

}), None => None };
        EntryFx::Profiles { setup: Some(PickerSetup { initial, server_roster, home_roster }) }.emit(&mut |effect| fx.push(effect));
        { publish = true; };
        true

},
            SessionEvent::Command(Command::SelectProfile { index, pin }) => 'select_profile: {
let (index, pin,): (usize, Option<String>,) = (*index, pin.clone(),);

        if self.state.pending_erase().is_some() || !matches!(self.state, attempt::Attempt::Profiles(_) | attempt::Attempt::Switching(_)) { break 'select_profile false; }
        let Some(tile) = self.state.picker_data().users.get(index).cloned() else { break 'select_profile false };
        if self.common.next_req.checked_add(1).is_none() { break 'select_profile false; }
        if ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).is_none() { break 'select_profile false; }
        let same_user = super::may_reuse_seated_profile(&self.common.persisted, &tile, pin.as_deref());
        if same_user {
            { let event = AttemptEvent::ReuseProfile; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        } else {
            let req = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::ProfileSwitch, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }).expect("request preflight");
            self.common.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Profile { tile, pin });
            { let event = AttemptEvent::SelectProfile { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        { publish = true; };
        true

},
            SessionEvent::Command(Command::SelectProfileWithReply { index, pin, reply }) => {
                let accepted = 'select_profile: {
let (index, pin,): (usize, Option<String>,) = (*index, pin.clone(),);

        if self.state.pending_erase().is_some() || !matches!(self.state, attempt::Attempt::Profiles(_) | attempt::Attempt::Switching(_)) { break 'select_profile false; }
        let Some(tile) = self.state.picker_data().users.get(index).cloned() else { break 'select_profile false };
        if self.common.next_req.checked_add(1).is_none() { break 'select_profile false; }
        if ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).is_none() { break 'select_profile false; }
        let same_user = super::may_reuse_seated_profile(&self.common.persisted, &tile, pin.as_deref());
        if same_user {
            { let event = AttemptEvent::ReuseProfile; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        } else {
            let req = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::ProfileSwitch, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }).expect("request preflight");
            self.common.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Profile { tile, pin });
            { let event = AttemptEvent::SelectProfile { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        { publish = true; };
        true

};
                fx.push(SessionFx::SelectionReply { to: *reply, accepted, flow_epoch: self.common.epoch });
                true
            }
            SessionEvent::Command(Command::BackAtRoot { reply }) => 'back: {
let (reply,): (ReplyTo,) = (*reply,);

        // AUTH-03: while an unacknowledged persistence warning is showing, BACK must not clear it
        // or resume — clearing it here would silently re-admit `needs_ready_commit`/`take_ready`
        // on the next frame (a second, routine save over the still-unconfirmed fresh one) and, for
        // a held Final handoff, would release it without the explicit acknowledgement the warning
        // exists to require. Only the labelled Continue (`acknowledge_persistence_warning`) may
        // clear it; BACK here is the platform root press only.
        if self.common.persistence_warning.is_some() {
            fx.push(SessionFx::BackReply { to: reply, resumed: false });
            break 'back true;
        }
        let dead_end = self.view().roster_dead_end();
        if dead_end {
            if matches!(self.common.authority, BootstrapAuthority::DevPms { .. }) {
                // Nothing was detached or re-pointed (see `start_switch`): re-announce the dev
                // session exactly as its activation did.
                { let event = AttemptEvent::ResumeDevBack; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                fx.push(SessionFx::BackReply { to: reply, resumed: true });
                { publish = true; };
                break 'back true;
            }
        }
        let stored = self.common.committed_credentials.merge_into(&self.common.persisted);
        // A dead-end picker offered nobody, so the picker-kind rule (`may_resume`: the
        // Change-profile picker is a root, so BACK cannot skip a PIN) has no boundary to guard.
        // Only "is there a session to go back to" remains — `dead_end_resumes`, the same answer
        // the read-out publishes so its *Back* control is offered exactly when this resumes.
        let resumable = if dead_end { self.view().dead_end_resumes() } else { super::resumable(&stored, self.state
            .picker_data().picker) };
        if !resumable || self.common.epoch.checked_add(1).is_none() {
            fx.push(SessionFx::BackReply { to: reply, resumed: false });
            break 'back true;
        }
        ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).expect("epoch preflight");
        self.common.persisted = stored;
        { let event = AttemptEvent::ResumeBack; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        fx.push(SessionFx::BackReply { to: reply, resumed: true });
        { publish = true; };
        true

},
            SessionEvent::Command(Command::SignOut) => 'erase: {
let (sign_in,): (bool,) = (true,);

        if self.state  .pending_erase().is_some() || self.common.epoch.checked_add(1).is_none() { break 'erase false; }
        ExitFx::for_state(&self.state, ExitActions { cancel: None, acknowledge: Vec::new(), coordinator: Some(CoordinatorAction::CloseTelemetry) }).emit(&mut |effect| fx.push(effect));
        ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).expect("epoch preflight");
        self.common.persisted = PersistedSession::default();
        self.common.committed_credentials = CredentialPatch::of(&self.common.persisted);
        // The identity ends here, and its verdicts with it: the same account signed back in is
        // the same `Identity`, and nothing has refused it anything in that sign-in.
        self.common.switch_refused_for = None;
        self.common.authority = BootstrapAuthority::Account { extras: Vec::new() };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        { let event = AttemptEvent::EraseAuthority; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        {

        self.common.incident = None;
        self.common.incidents_seen.clear();
        { let event = AttemptEvent::LinkTrouble { present: false }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |_| {}); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |_| {}); true };

};
        {
let (profile,): (Option<UserRef>,) = (None,);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        { let event = AttemptEvent::BeginErase { epoch: self.common.epoch, sign_in }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        { publish = true; };
        true

},
            SessionEvent::Command(Command::EraseLocal) => 'erase: {
let (sign_in,): (bool,) = (false,);

        if self.state  .pending_erase().is_some() || self.common.epoch.checked_add(1).is_none() { break 'erase false; }
        ExitFx::for_state(&self.state, ExitActions { cancel: None, acknowledge: Vec::new(), coordinator: Some(CoordinatorAction::CloseTelemetry) }).emit(&mut |effect| fx.push(effect));
        ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).expect("epoch preflight");
        self.common.persisted = PersistedSession::default();
        self.common.committed_credentials = CredentialPatch::of(&self.common.persisted);
        // The identity ends here, and its verdicts with it: the same account signed back in is
        // the same `Identity`, and nothing has refused it anything in that sign-in.
        self.common.switch_refused_for = None;
        self.common.authority = BootstrapAuthority::Account { extras: Vec::new() };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        { let event = AttemptEvent::EraseAuthority; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        {

        self.common.incident = None;
        self.common.incidents_seen.clear();
        { let event = AttemptEvent::LinkTrouble { present: false }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |_| {}); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |_| {}); true };

};
        {
let (profile,): (Option<UserRef>,) = (None,);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        { let event = AttemptEvent::BeginErase { epoch: self.common.epoch, sign_in }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        { publish = true; };
        true

},
            SessionEvent::Command(Command::RefreshRoster) => 'refresh_roster: {

        if self.common.persisted.account_token.is_empty() { break 'refresh_roster false; }
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::ServerRoster, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else { break 'refresh_roster false };
        {
let (req, input,): (u32, SessionWork,) = (req, SessionWork::ServerRoster { session: self.common.persisted.clone(),
            expected: Identity::of(&self.common.persisted) },);

        ({
let (req, input,): (u32, SessionWork,) = (req, input,);

        let pending = self.common.pending.get_mut(&req).expect("owned work request");
        let admission = AdmissionId(req);
        pending.admission = AdmissionState::Awaiting(admission);
        EntryWork::Work { req, key: pending.key, admission, input }

}).emit(&mut |effect| fx.push(effect));

};
        true

},
            SessionEvent::Command(Command::RequestEndpoint { sid }) => 'request_endpoint: {
let (sid,): (plx_plex::plex::ServerId,) = (*sid,);

        // The registry's own validity rule: every id but the reserved UNSET names a slot.
        if !sid.is_set()
            || self.common.persisted.account_token.is_empty()
            || self.common.pending.values().any(|pending| pending.key.op == SessionOp::Endpoint(sid.raw())) {
            break 'request_endpoint false;
        }
        let sid = sid.raw();
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::Endpoint(sid), None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else { break 'request_endpoint false };
        self.common.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Endpoint { sid });
        fx.push(SessionFx::Capture { req, epoch: self.common.epoch, request: SessionReadRequest::Endpoint { sid } });
        true

},
            SessionEvent::Command(Command::AnswerPlaintext { machine_id, choice, sid }) =>
                'answer_plaintext: {
let (machine_id, choice, sid,): (&str, plx_plex::plex::session::PlaintextChoice, Option<plx_plex::plex::ServerId>,) = (machine_id, *choice, *sid,);

        use plx_plex::plex::session::PlaintextChoice;
        let account = plx_plex::plex::grant::account_key(&self.common.persisted.account_token);
        if account.is_empty() || machine_id.is_empty() || choice == PlaintextChoice::Undecided {
            break 'answer_plaintext false;
        }
        let on_sign_in = self.state.phase() == Phase::Error;
        if on_sign_in && !self.state.plaintext().is_some_and(|v| v.machine_id == machine_id) {
            break 'answer_plaintext false;
        }
        fx.push(SessionFx::PlaintextAnswer { machine_id: machine_id.to_owned(), choice, account });
        if on_sign_in {
            if choice.allows() {
                break 'answer_plaintext ({ restart = Some(if false { LoginRestart::Fresh } else { LoginRestart::Retry }); true });
            }
            { let event = AttemptEvent::PlaintextDeclined { choice }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
            { publish = true; };
            break 'answer_plaintext true;
        }
        if choice.allows() {
            let _ = match sid {
                Some(sid) => 'request_endpoint: {
let (sid,): (plx_plex::plex::ServerId,) = (sid,);

        // The registry's own validity rule: every id but the reserved UNSET names a slot.
        if !sid.is_set()
            || self.common.persisted.account_token.is_empty()
            || self.common.pending.values().any(|pending| pending.key.op == SessionOp::Endpoint(sid.raw())) {
            break 'request_endpoint false;
        }
        let sid = sid.raw();
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::Endpoint(sid), None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else { break 'request_endpoint false };
        self.common.pending.get_mut(&req).unwrap().capture = Some(CaptureIntent::Endpoint { sid });
        fx.push(SessionFx::Capture { req, epoch: self.common.epoch, request: SessionReadRequest::Endpoint { sid } });
        true

},
                None => 'refresh_roster: {

        if self.common.persisted.account_token.is_empty() { break 'refresh_roster false; }
        let Some(req) = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::ServerRoster, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }) else { break 'refresh_roster false };
        {
let (req, input,): (u32, SessionWork,) = (req, SessionWork::ServerRoster { session: self.common.persisted.clone(),
            expected: Identity::of(&self.common.persisted) },);

        ({
let (req, input,): (u32, SessionWork,) = (req, input,);

        let pending = self.common.pending.get_mut(&req).expect("owned work request");
        let admission = AdmissionId(req);
        pending.admission = AdmissionState::Awaiting(admission);
        EntryWork::Work { req, key: pending.key, admission, input }

}).emit(&mut |effect| fx.push(effect));

};
        true

},
            };
        }
        true

},
            SessionEvent::Command(Command::RestartWait { phase, qr_generation, reply }) => {
                let accepted = matches!(self.common.authority, BootstrapAuthority::Account { .. })
                    && super::restart_permitted(Some((*phase, *qr_generation)),
                    (self.state.phase(), self.state.qr().generation)) && { restart = Some(if false { LoginRestart::Fresh } else { LoginRestart::Retry }); true };
                { restart_reply = Some((reply, accepted)); };
                true
            }
            SessionEvent::Command(Command::DismissPinError) => {
                let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder());
                self.state = attempt.dismiss_pin();
                { publish = true; };
                true
            }
            SessionEvent::Command(Command::NoteDeleteLeftovers(count)) => {
                self.common.delete_leftovers = *count;
                { publish = true; };
                true
            }
            SessionEvent::Result(envelope) => 'ingest: {
let (envelope,): (&SessionEnvelope,) = (envelope,);

        let receipt = Receipt::of(envelope);
        if self.owns_receipt(receipt) { break 'ingest true; }
        if !self.accepts_header(envelope) {
            fx.push(SessionFx::Acknowledge(vec![receipt]));
            break 'ingest true;
        }
        // The adapter admits only receipt-bearing records from an accepted resource. This can
        // also confirm acceptance if its small admission reply is still carried in the queue.
        self.common.pending.get_mut(&envelope.addr.req.0).unwrap().admission = AdmissionState::Accepted(envelope.admission);
        let active = usize::from(self.common.pending_commit.as_ref().is_some_and(|commit| commit.receipt.is_some()));
        assert!(self.common.inbox.len() + active < SESSION_TRANSFER_RECORDS, "Session transfer credit invariant");
        // Do not advance last_arrival here: that is the processing watermark, not receipt
        // admission. Advancing it would make the FIFO head reject its own first delivery.
        self.common.inbox.push_back(envelope.clone());
        if self.common.pending_commit.is_none() && !self.common.pump_pending { { pump = true; }; }
        true

},
            SessionEvent::Commit(reply) => 'apply_commit_reply: {
let (reply,): (CommitReply,) = (*reply,);

        if !self.commit_is_current(reply.req, reply.epoch, reply.arrival) { break 'apply_commit_reply false; }
        // `take()` is what makes a duplicate reply inert. The admitted revision and purpose are
        // copied into `admitted_persistence` first, because that is the identity a later
        // completion must repeat exactly to be allowed to settle anything.
        let admit_revision = reply.admission.admitted_revision();
        // The admission names what storage was actually asked for; the plan only records intent.
        let admit_purpose = reply.admission.admitted_purpose()
            .or_else(|| self.common.pending_commit.as_ref().and_then(|c| c.purpose));
        let commit = self.common.pending_commit.take().unwrap();
        self.common.admitted_persistence = admit_revision.map(|revision| AdmittedPersistence {
            req: commit.req, epoch: commit.epoch, arrival: commit.arrival, revision,
            purpose: admit_purpose, fresh: commit.fresh,
            site: match admit_purpose {
                Some(PersistencePurpose::Discovery) => PersistenceWarningSite::Discovery,
                _ => PersistenceWarningSite::Final,
            } });
        self.common.commit_phase = CommitPhase {
            admitted: admit_revision.is_some(),
            durable: false,
            proves_saved_login: false,
        };
        self.common.persistence_purpose = None;
        if !reply.admission.accepted() {
            if let Some(DevCommitDelta::StartAccount { login_req }) = commit.delta.dev {
                {
let (req,): (u32,) = (login_req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
            }
            let op = self.common.pending[&reply.req].key.op;
            match op {
                SessionOp::Login | SessionOp::Rediscover => {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_finish_failed(), Some(IncidentContext::internal(InternalClass::CommitRefused)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

},
                SessionOp::ProfileSwitch | SessionOp::Ready => {
                    { let event = AttemptEvent::SwitchCommitRefused; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                }
                SessionOp::HomeRoster | SessionOp::ServerRoster | SessionOp::Endpoint(_) | SessionOp::Picker => {}
                SessionOp::DevBoundary => {
                    { let event = AttemptEvent::DevCommitRefused; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                }
            }
            {
let (req,): (u32,) = (reply.req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
            if let Some(receipt) = commit.receipt { fx.push(SessionFx::Acknowledge(vec![receipt])); }
            {

        if !self.common.inbox.is_empty() && self.common.pending_commit.is_none() && !self.common.pump_pending {
            self.common.pump_pending = true;
            fx.push(SessionFx::Pump);
        }

};
            { publish = true; };
            break 'apply_commit_reply true;
        }
        let delta = commit.delta;
        if let Some(dev) = &delta.dev {
            match dev {
                DevCommitDelta::Activated => {
                    if matches!(self.common.authority, BootstrapAuthority::DevPms { .. }) {
                        {
let (profile,): (Option<UserRef>,) = (None,);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
                        { let event = AttemptEvent::ActivateDev; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                }
                DevCommitDelta::StartAccount { login_req } => {
                    self.common.authority = BootstrapAuthority::Account { extras: Vec::new() };
                    { let event = AttemptEvent::BeginSignin { req: *login_req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    {
let (req,): (u32,) = (*login_req,);

        let entry = EntryFx::login(self.view(), req, false, false);
        let pending = self.common.pending.get_mut(&req).expect("reserved login");
        match entry.work() {
            EntryWork::Capture { .. } => pending.capture = Some(CaptureIntent::Login),
            EntryWork::Work { admission, .. } => pending.admission = AdmissionState::Awaiting(*admission),
        }
        entry.emit(&mut |effect| fx.push(effect));

};
                }
            }
        }
        if let Some(patch) = delta.credentials {
            self.common.persisted = patch.merge_into(&self.common.persisted);
            if commit.writes_credentials {
                let prior = std::mem::replace(&mut self.common.disk_identity, patch.identity());
                self.common.unconfirmed_fresh_prior =
                    (commit.fresh && admit_revision.is_some()).then_some(prior);
                self.common.committed_credentials = patch;
            }
        }
        match delta.destination {
            CommitDestination::Unchanged => {}
            CommitDestination::Roster => { if let Some(users) = delta.users { { let event = AttemptEvent::RosterArrived { users }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true }; } }
            CommitDestination::ReadyCommitted => { { let event = AttemptEvent::ReadyCommitted; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true }; }
            CommitDestination::SignInProfiles => { { let event = AttemptEvent::SigninProfiles { users: delta.users.expect("sign-in roster") }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true }; }
            CommitDestination::SignInReady => { { let event = AttemptEvent::SigninReady; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true }; }
            CommitDestination::ProfileReady => { { let event = AttemptEvent::ProfileReady { req: reply.req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true }; }
            CommitDestination::StoredReady => { { let event = AttemptEvent::StoredReady; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true }; }
        }
        if delta.roster_answered { self.common.switch_refused_for = None; }
        if matches!(delta.destination, CommitDestination::ProfileReady) {
            let pending = self.common.pending.get_mut(&reply.req).unwrap();
            pending.expected = Identity::of(&self.common.persisted);
            pending.phase = StreamPhase::ProfileSeated;
            'cancel_obsolete_interests: {
let (retained,): (u32,) = (reply.req,);

        let requests: Vec<_> = self.common.pending.iter().filter_map(|(&req, pending)|
            (req != retained && !pending.expected.matches(&self.common.persisted)).then_some(req)).collect();
        if requests.is_empty() { break 'cancel_obsolete_interests; }
        for req in &requests { self.common.pending.remove(req); }
        let mut receipts = Vec::new();
        self.common.inbox.retain(|record| {
            if requests.contains(&record.addr.req.0) {
                receipts.push(Receipt::of(record));
                false
            } else { true }
        });
        fx.push(SessionFx::Cancel { requests, epoch: self.common.epoch });
        if !receipts.is_empty() { fx.push(SessionFx::Acknowledge(receipts)); }

};
        }
        if delta.activate_profile {
            if commit.fresh && admit_revision.is_some() && !self.common.persistence_warning_answered {
                // The fresh write is admitted but not yet confirmed durable: hold the handoff
                // rather than announce Ready/publish the profile until a completion (or an
                // acknowledged warning) releases it — the AUTH-03 gate.
                self.common.held_handoff = Some(HeldHandoff { epoch: commit.epoch, req: commit.req });
            } else if self.common.persistence_warning.is_none() {
                // `persistence_warning_answered` true means this authorization already spent its
                // one Continue: nothing is left to wait for, so a fresh write lands here exactly
                // like a routine one — publish immediately rather than hold a handoff whose
                // completion (or loss) would otherwise be the only way out.
                // 0.6.6 parity (`discovery-warning-not-cleared-on-retry-or-fresh-success`): a
                // ROUTINE `activate_profile` commit (StartSwitch, resume_stored, …) must not free
                // a handoff still held behind an unacknowledged warning — that release is
                // `acknowledge_persistence_warning`'s alone. An unrelated held handoff with NO live
                // warning (already superseded above, or never one to begin with) is unaffected.
                {

        self.common.held_handoff = None;
        {
let (profile,): (Option<UserRef>,) = (Some(self.common.persisted.user.clone()),);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        if self.state.phase() == Phase::Ready {
            EntryFx::Ready { announcement: Some(ReadyEntry { epoch: self.common.epoch, scope: self.common.profile_scope,
                server: self.common.persisted.server.clone(), token: self.common.persisted.pms_token().into(),
                install: ReadyInstall::PrimaryAndExtras(match &self.common.authority {
                    BootstrapAuthority::Account { extras } => extras.clone(),
                    BootstrapAuthority::DevPms { .. } => Vec::new(),
                }) }) }.emit(&mut |effect| fx.push(effect));
        }

};
            }
        }
        if commit.terminal { {
let (req,): (u32,) = (reply.req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

}; }
        if let Some(receipt) = commit.receipt { fx.push(SessionFx::Acknowledge(vec![receipt])); }
        {

        if !self.common.inbox.is_empty() && self.common.pending_commit.is_none() && !self.common.pump_pending {
            self.common.pump_pending = true;
            fx.push(SessionFx::Pump);
        }

};
        { publish = true; };
        true

},
            SessionEvent::DiskWrite { before, after, outcome } => 'observe_disk_write: {
let (before, after, outcome,): (&Identity, &Identity, plx_plex::plex::session::async_persistence::CompletionOutcome,) = (before, after, *outcome,);

        use plx_plex::plex::session::async_persistence::{CompletionOutcome, Operation};
        if self.common.disk_identity != *before || before == after
            || !matches!(outcome, CompletionOutcome::Durable(Operation::Write { .. })) { break 'observe_disk_write false; }
        self.common.disk_identity = after.clone();
        true

},
            SessionEvent::Persistence(completion) => 'apply_persistence_completion: {
let (completion,): (plx_plex::plex::session::async_persistence::PersistenceCompletion,) = (*completion,);

        use plx_plex::plex::session::async_persistence::CompletionOutcome;
        let Some(admitted) = self.common.admitted_persistence else { break 'apply_persistence_completion false };
        let correlation = plx_plex::plex::session::async_persistence::PersistenceCorrelation {
            req: admitted.req, epoch: admitted.epoch, arrival: admitted.arrival,
        };
        if !completion.acts_on(correlation, admitted.revision) { break 'apply_persistence_completion false; }
        // A verdict the worker resolved for a DIFFERENT purpose than the one admitted is not this
        // operation's verdict either.
        if admitted.purpose.is_some_and(|purpose| purpose != completion.purpose) { break 'apply_persistence_completion false; }
        let durable = matches!(completion.outcome, CompletionOutcome::Durable(_));
        self.common.admitted_persistence = None;
        let prior = self.common.unconfirmed_fresh_prior.take();
        if let (Some(prior), CompletionOutcome::Failed(_)) = (prior, &completion.outcome) {
            if admitted.fresh { self.common.disk_identity = prior; }
        }
        self.common.commit_phase.durable = durable;
        self.common.commit_phase.proves_saved_login =
            durable && completion.purpose.proves_saved_login();
        let correlation_key = PersistenceWarningKey { epoch: completion.epoch, req: completion.req };
        if durable {
            // 0.6.6 parity: a later FRESH write landing durably supersedes any warning still
            // showing about an earlier fresh attempt — the failure it was reporting on no longer
            // describes the session's current state, so holding it would strand an acknowledgement
            // over a problem that already resolved itself.
            let superseded = admitted.fresh && self.common.persistence_warning.is_some();
            if superseded {
                self.common.persistence_warning = None;
                {

        if self.common.incident.as_ref().is_some_and(|offer|
            offer.key.kind == plx_telemetry::telemetry::incident::IncidentKind::SaveFailed) {
            self.common.incident = None;
        }

};
            }
            // Release only the handoff THIS completion is for — a routine completion arriving
            // while an unrelated fresh handoff is held must not free it.
            let released = self.common.held_handoff == Some(HeldHandoff { epoch: completion.epoch, req: completion.req });
            if released { {

        self.common.held_handoff = None;
        {
let (profile,): (Option<UserRef>,) = (Some(self.common.persisted.user.clone()),);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        if self.state.phase() == Phase::Ready {
            EntryFx::Ready { announcement: Some(ReadyEntry { epoch: self.common.epoch, scope: self.common.profile_scope,
                server: self.common.persisted.server.clone(), token: self.common.persisted.pms_token().into(),
                install: ReadyInstall::PrimaryAndExtras(match &self.common.authority {
                    BootstrapAuthority::Account { extras } => extras.clone(),
                    BootstrapAuthority::DevPms { .. } => Vec::new(),
                }) }) }.emit(&mut |effect| fx.push(effect));
        }

}; }
            if superseded || released { { publish = true; }; }
        } else if admitted.fresh {
            if self.common.persistence_warning_answered {
                // This authorization already had its one unsaved-login question answered
                // (AUTH-03's field bug: a device unwritable on BOTH Discovery and Final used to
                // ask it twice). Storage failing again must not ask it a second time — release
                // whatever handoff this completion was holding so entry proceeds regardless.
                let released = self.common.held_handoff == Some(HeldHandoff { epoch: completion.epoch, req: completion.req });
                if released { {

        self.common.held_handoff = None;
        {
let (profile,): (Option<UserRef>,) = (Some(self.common.persisted.user.clone()),);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        if self.state.phase() == Phase::Ready {
            EntryFx::Ready { announcement: Some(ReadyEntry { epoch: self.common.epoch, scope: self.common.profile_scope,
                server: self.common.persisted.server.clone(), token: self.common.persisted.pms_token().into(),
                install: ReadyInstall::PrimaryAndExtras(match &self.common.authority {
                    BootstrapAuthority::Account { extras } => extras.clone(),
                    BootstrapAuthority::DevPms { .. } => Vec::new(),
                }) }) }.emit(&mut |effect| fx.push(effect));
        }

}; { publish = true; }; }
            } else {
                // The final write's own non-durable completion replaces a Discovery warning still
                // showing (`Discovery` and `Final` never coexist: an unacknowledged Discovery warning
                // blocks `take_ready`), so the newer one wins by direct overwrite.
                let warning = PersistenceWarning::from_outcome(correlation_key, admitted.site, &completion.outcome);
                {

        if self.common.incident.as_ref().is_some_and(|offer|
            offer.key.kind == plx_telemetry::telemetry::incident::IncidentKind::SaveFailed) {
            self.common.incident = None;
        }

};
                self.common.persistence_warning = Some(warning);
                if warning.helper.is_some() {
                    // The reducer reads no clock. Both report paths use this fenced completion's
                    // local warning evidence, not later process-global diagnostics.
                    if let Some(context) = warning.incident_context() {
                        'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

};
                    }
                }
                { publish = true; };
            }
        }
        true

},
            SessionEvent::Read(reply) => 'apply_read: {
let (reply,): (&SessionReadReply,) = (reply,);

        if reply.addr.to != MachineId::Session || reply.epoch != self.common.epoch { break 'apply_read false; }
        let req = reply.addr.req.0;
        let Some(pending) = self.common.pending.get(&req) else { break 'apply_read false };
        if pending.key.epoch != reply.epoch || !pending.expected.matches(&self.common.persisted) || !self.state.owns_work(req, pending) { break 'apply_read false; }
        let Some(intent) = pending.capture.clone() else { break 'apply_read false };
        let input = match (intent, &reply.value) {
            (CaptureIntent::Login, SessionReadValue::LoginClientId(client_id)) if !client_id.is_empty() => {
                if self.common.disk_identity.client_id.is_empty() {
                    self.common.disk_identity.client_id = client_id.clone();
                }
                self.common.persisted.client_id = client_id.clone();
                self.common.committed_credentials.client_id = client_id.clone();
                self.common.pending.get_mut(&req).unwrap().expected = Identity::of(&self.common.persisted);
                SessionWork::Login { client_id: client_id.clone() }
            }
            (CaptureIntent::Profile { tile, pin }, SessionReadValue::ProfilePolicy { recently_unreachable }) =>
                SessionWork::ProfileSwitch { session: self.common.persisted.clone(),
                    expected: Identity::of(&self.common.persisted), tile, pin, recently_unreachable: *recently_unreachable },
            (CaptureIntent::Endpoint { sid }, SessionReadValue::Endpoint(Some(captured)))
                if captured.lifecycle.sid == sid && self.common.persisted.sources.iter().any(|source|
                    source.machine_id == captured.machine_id && source.dialable()) => {
                self.common.pending.get_mut(&req).unwrap().lifecycle = Some(captured.lifecycle);
                SessionWork::Endpoint { session: self.common.persisted.clone(), expected: Identity::of(&self.common.persisted),
                    lifecycle: captured.lifecycle, machine_id: captured.machine_id.clone() }
            }
            (CaptureIntent::Endpoint { .. }, SessionReadValue::Endpoint(_)) => {
                {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                break 'apply_read true;
            }
            (CaptureIntent::Login, SessionReadValue::LoginClientId(_)) => {
                {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_start_failed(), Some(IncidentContext::internal(InternalClass::ClientIdUnavailable)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
                {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                { publish = true; };
                break 'apply_read true;
            }
            _ => break 'apply_read false,
        };
        let pending = self.common.pending.get_mut(&req).unwrap();
        pending.capture = None;
        {
let (req, input,): (u32, SessionWork,) = (req, input,);

        ({
let (req, input,): (u32, SessionWork,) = (req, input,);

        let pending = self.common.pending.get_mut(&req).expect("owned work request");
        let admission = AdmissionId(req);
        pending.admission = AdmissionState::Awaiting(admission);
        EntryWork::Work { req, key: pending.key, admission, input }

}).emit(&mut |effect| fx.push(effect));

};
        true

},
            SessionEvent::Admission(reply) => 'apply_admission: {
let (reply,): (AdmissionReply,) = (*reply,);

        if reply.addr.to != MachineId::Session || reply.key.epoch != self.common.epoch { break 'apply_admission false; }
        let req = reply.addr.req.0;
        if !self.common.pending.get(&req).is_some_and(|pending| self.state.owns_work(req, pending)) { break 'apply_admission false; }
        let Some(pending) = self.common.pending.get_mut(&req) else { break 'apply_admission false };
        if pending.key != reply.key || pending.admission != AdmissionState::Awaiting(reply.correlation) { break 'apply_admission false; }
        if reply.accepted {
            pending.admission = AdmissionState::Accepted(reply.correlation);
        } else {
            // This is an unsequenced, never-admitted refusal. Accepted requests (including ones
            // without a first observation) cannot enter this branch.
            match pending.key.op {
                SessionOp::Login => {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_start_failed(), Some(IncidentContext::internal(InternalClass::AdmissionRefused)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

},
                SessionOp::Rediscover => {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_rediscover_failed(), Some(IncidentContext::internal(InternalClass::AdmissionRefused)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

},
                SessionOp::ProfileSwitch => {
                    { let event = AttemptEvent::SwitchAdmissionRefused; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                }
                SessionOp::HomeRoster => {
let (reason,): (&str,) = (roster_unreachable(),);

        if self.state.picker_data().users.is_empty() && self.state.phase() == Phase::Profiles {
            { let event = AttemptEvent::RosterFailed { message: reason.to_owned() }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }

},
                SessionOp::ServerRoster | SessionOp::Endpoint(_) | SessionOp::Ready | SessionOp::Picker | SessionOp::DevBoundary => {}
            }
            {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
            { publish = true; };
        }
        {

        if !self.common.inbox.is_empty() && self.common.pending_commit.is_none() && !self.common.pump_pending {
            self.common.pump_pending = true;
            fx.push(SessionFx::Pump);
        }

};
        true

},
            SessionEvent::Erased { epoch, leftovers } => 'erased: {
let (epoch, leftovers,): (u64, usize,) = (*epoch, *leftovers,);

        let Some((expected, sign_in)) = self.state.pending_erase() else { break 'erased false };
        if expected != epoch || self.common.epoch != epoch { break 'erased false; }
        { let event = AttemptEvent::FinishErase; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.disk_identity = Identity::of(&self.common.persisted);
        self.common.delete_leftovers = leftovers;
        if sign_in { { restart = Some(if true { LoginRestart::Fresh } else { LoginRestart::Retry }); true }; }
        { publish = true; };
        true

},
            SessionEvent::IncidentReported { id, delivery } => {
                let handled = 'incident_reported: {
let (id, delivery,): (u32, &IncidentDelivery,) = (*id, delivery,);

        let Some(held) = self.common.incident.as_mut().filter(|held| held.id == id) else { break 'incident_reported false };
        let shown = match &held.state {
            IncidentState::Queued { receipt } | IncidentState::Saved { receipt } => Some(receipt.as_str()),
            IncidentState::Pending | IncidentState::Offered { .. } | IncidentState::OnRequest { .. }
                | IncidentState::AutoSending | IncidentState::Sending | IncidentState::Delivered { .. }
                | IncidentState::Failed | IncidentState::NotNow | IncidentState::Dropped => None,
        };
        held.state = match (&held.state, delivery) {
            (IncidentState::AutoSending, IncidentDelivery::Standing { receipt: Some(receipt) })
            | (IncidentState::Sending, IncidentDelivery::OneOff { receipt: Some(receipt) }) => {
                IncidentState::Queued { receipt: receipt.clone() }
            }
            (IncidentState::AutoSending, IncidentDelivery::Standing { receipt: None })
            | (IncidentState::Sending, IncidentDelivery::OneOff { receipt: None }) => IncidentState::Failed,
            // Tried and not through yet, still queued durably: said once, from Queued.
            (IncidentState::Queued { .. }, IncidentDelivery::Held { receipt }) if shown == Some(receipt) => {
                IncidentState::Saved { receipt: receipt.clone() }
            }
            (_, IncidentDelivery::Delivered { receipt }) if shown == Some(receipt) => {
                IncidentState::Delivered { receipt: receipt.clone() }
            }
            // The report did not get through after all: Send report is accepted again.
            (_, IncidentDelivery::Undelivered { receipt }) if shown == Some(receipt) => IncidentState::Failed,
            _ => break 'incident_reported false,
        };
        true

};
                if handled { { publish = true; }; }
                handled
            }
            SessionEvent::Pump => {
                self.common.pump_pending = false;
                { pump = true; };
                true
            }
            SessionEvent::CacheObserved(need) => {
                'observe_cache: {
let (need, now_ms,): (plx_plex::plex::session::RefreshNeed, u32,) = (*need, tick_ms,);

        use plx_plex::plex::session::LOCKED_RETRY_MS;
        let tracked = match self.cache_refresh {
            CacheRefresh::InFlight { .. } => break 'observe_cache,
            CacheRefresh::Backoff { armed_ms, generation } => {
                let covers = generation.is_none_or(|g| g == need.generation);
                if covers && now_ms.wrapping_sub(armed_ms) < LOCKED_RETRY_MS { break 'observe_cache; }
                covers
            }
            CacheRefresh::Idle => false,
        };
        if need.transient && !tracked {
            self.cache_refresh = CacheRefresh::Backoff { armed_ms: now_ms, generation: Some(need.generation) };
            break 'observe_cache;
        }
        self.next_cache_req = self.next_cache_req.wrapping_add(1);
        let req = self.next_cache_req;
        self.cache_refresh = CacheRefresh::InFlight { req, generation: need.generation };
        fx.push(SessionFx::RefreshCache { req, generation: need.generation });

};
                true
            }
            SessionEvent::CacheRefreshed { req, outcome } => 'cache_refreshed: {
let (req, outcome, now_ms,): (u32, plx_plex::plex::session::CacheRefreshOutcome, u32,) = (*req, *outcome, tick_ms,);

        use plx_plex::plex::session::CacheRefreshOutcome;
        if self.cache_refresh_in_flight() != Some(req) { break 'cache_refreshed false; }
        let armed_ms = now_ms;
        self.cache_refresh = match outcome {
            CacheRefreshOutcome::Settled | CacheRefreshOutcome::Superseded => CacheRefresh::Idle,
            CacheRefreshOutcome::Transient { generation } => CacheRefresh::Backoff { armed_ms, generation: Some(generation) },
            CacheRefreshOutcome::Refused => CacheRefresh::Backoff { armed_ms, generation: None },
        };
        true

},
        };
// Result ingress and Pump share one receipt-ordered observation drain.
if pump { 'pump_one: {

        if self.common.pending_commit.is_some() { break 'pump_one; }
        let Some(mut envelope) = self.common.inbox.pop_front() else { break 'pump_one };
        let receipt = Receipt::of(&envelope);
        // Completion authority is independent of payload authority. Recheck at the FIFO
        // head (a preceding commit may have changed this request), never at ingress.
        // A trusted terminal with rejected data still ends its own work, using the same
        // failure policy as a dropped producer; none of its payload reaches resource IO.
        if !self.accepts(&envelope) && envelope.terminal
            && matches!(envelope.outcome, SessionArrival::Data(_))
            && self.accepts_header(&envelope)
            && self.common.pending.get(&envelope.addr.req.0).is_some_and(|pending|
                !matches!(pending.key.op, SessionOp::Endpoint(_))
                    || pending.lifecycle == envelope.lifecycle)
        {
            envelope.outcome = SessionArrival::Dropped;
        }
        if self.accepts(&envelope) {
            if !('apply_qr_observation: {
let (envelope,): (&SessionEnvelope,) = (&envelope,);

        if !self.accepts(envelope)
            || !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
            break 'apply_qr_observation false;
        }
        let req = envelope.addr.req.0;
        if !self.common.pending[&req].expected.matches(&self.common.persisted) { break 'apply_qr_observation false; }
        match &envelope.outcome {
            SessionArrival::Refused | SessionArrival::Dropped => {
                if !envelope.terminal { break 'apply_qr_observation false; }
                let (message, class) = match (envelope.key.op, &envelope.outcome) {
                    (SessionOp::Rediscover, SessionArrival::Refused) =>
                        (plx_platform::i18n::msg::browse_auth_rediscover_failed(), InternalClass::WorkerRefused),
                    (_, SessionArrival::Refused) => (plx_platform::i18n::msg::browse_auth_start_failed(), InternalClass::WorkerRefused),
                    _ => (plx_platform::i18n::msg::browse_auth_finish_failed(), InternalClass::WorkerDropped),
                };
                {
let (message, incident,): (&str, Option<IncidentContext>,) = (message, Some(IncidentContext::internal(class)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
            }
            SessionArrival::Data(data) => {
                let super::observation::Observation::Login(progress) = &**data else { break 'apply_qr_observation false };
                use super::LoginProgress;
                let (epoch, terminal) = match progress {
                    LoginProgress::CodeReplacing { epoch }
                    | LoginProgress::CodeReady { epoch, .. }
                    | LoginProgress::Authorized { epoch, .. }
                    | LoginProgress::DiscoveryTrouble { epoch, .. }
                    | LoginProgress::DiscoveryRetrySettled { epoch, .. }
                    | LoginProgress::LinkTrouble { epoch, .. } => (*epoch, false),
                    LoginProgress::Failed { epoch, .. } => (*epoch, true),
                    LoginProgress::SignedIn { .. } => break 'apply_qr_observation false,
                };
                if epoch != envelope.key.epoch || terminal != envelope.terminal { break 'apply_qr_observation false; }
                match progress {
                    LoginProgress::CodeReplacing { .. } => {
                        if envelope.key.op != SessionOp::Login || !matches!(self.state.phase(), Phase::Creating | Phase::Waiting) { break 'apply_qr_observation false; }
                        { let event = AttemptEvent::CodeReplacing; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::CodeReady { code, qr_png, .. } => {
                        if envelope.key.op != SessionOp::Login { break 'apply_qr_observation false; }
                        let Some(next) = self.common.next_qr.checked_add(1) else {
                            {
let (message, incident,): (&str, Option<IncidentContext>,) = (plx_platform::i18n::msg::browse_auth_start_failed(), Some(IncidentContext::internal(InternalClass::Exhausted)),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
                            self.common.pending.remove(&req);
                            fx.push(SessionFx::Cancel { requests: vec![req], epoch: self.common.epoch });
                            fx.push(SessionFx::Retire { req });
                            { publish = true; };
                            break 'apply_qr_observation true;
                        };
                        self.common.next_qr = next;
                        { let event = AttemptEvent::CodeReady {
                            generation: next, code: code.clone(), png: qr_png.clone(),
                        }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::Authorized { token, .. } => {
                        if envelope.key.op != SessionOp::Login { break 'apply_qr_observation false; }
                        // A sign-in that is not a sign-out (a stored session that could not be
                        // activated, *Use another account*, the dev login trigger) starts from the
                        // stored record. Whoever that record seated, its roster and its offline
                        // profile cache belong to the authorization this token replaces, so none of
                        // them may ride into the new account: `SignedIn` writes the roster and
                        // `Session::account` names a single-user account from it; a Home account
                        // seats through the picker. Reset BEFORE `expected` is recomputed below.
                        // The new token cannot say whose cache this is (every sign-in mints one),
                        // so the cache is emptied while the identity is unknown and `SignedIn`
                        // restores the STORED record's when its roster names the same owner.
                        self.common.persisted.user = UserRef::default();
                        self.common.persisted.home_users.clear();
                        self.common.persisted.profiles.clear();
                        self.common.persisted.account_token = token.clone();
                        { let event = AttemptEvent::Authorized; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                        self.common.pending.get_mut(&req).unwrap().expected = Identity::of(&self.common.persisted);
                    }
                    LoginProgress::DiscoveryTrouble { progress, .. } => {
                        if !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
                            break 'apply_qr_observation false;
                        }
                        { let event = AttemptEvent::DiscoveryTrouble { progress: *progress }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::DiscoveryRetrySettled { run, .. } => {
                        if !matches!(envelope.key.op, SessionOp::Login | SessionOp::Rediscover) {
                            break 'apply_qr_observation false;
                        }
                        { let event = AttemptEvent::DiscoverySettled { run: *run }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::LinkTrouble { trouble, .. } => {
                        if envelope.key.op != SessionOp::Login { break 'apply_qr_observation false; }
                        { let event = AttemptEvent::LinkTrouble {
                            present: trouble.is_some(),
                        }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                        if let Some(context) = trouble {
                            'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, *context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

};
                        }
                    }
                    LoginProgress::Failed { message, incident, plaintext, account, .. } => {
                        {
let (message, incident,): (&str, Option<IncidentContext>,) = (message, Some(*incident),);

        { let event = AttemptEvent::LoginFailed { message: message.to_owned(), revoke: incident.is_some_and(|context| context.kind.refuses_the_account_token()) }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        self.common.persistence_warning = None;
        self.common.held_handoff = None;
        self.common.persistence_warning_answered = false;
        // A token plex.tv refused is not an authorization in flight any more: without this, the
        // retry decision (`retry_kind`) kept offering a discovery-only pass that presents the same
        // refused token again, and Try again could never recover. Dropping it makes the retry a
        // new QR sign-in; the incident below is raised exactly as before, so its dedup holds.
        match incident {
            Some(context) => 'raise_incident: {
let (flow, context,): (IncidentFlow, IncidentContext,) = (IncidentFlow::SignIn, context,);

        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.common.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher report evidence. Resolved: keep the answer and report
            // evidence it already has. Details always reads the current failure facts.
            held.readout_context = Some(context);
            if !is_settled(held) {
                held.context = Some(context);
            }
            break 'raise_incident;
        }
        self.common.next_incident = self.common.next_incident.wrapping_add(1).max(1);
        self.common.incident = Some(IncidentOffer {
            id: self.common.next_incident,
            key,
            context: Some(context),
            readout_context: Some(context),
            state: IncidentState::Pending,
        });

},
            None => self.common.incident = None,
        }

};
                        { let event = AttemptEvent::DiscoveryFailed { plaintext: plaintext.clone(), account: account.clone() }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    }
                    LoginProgress::SignedIn { .. } => unreachable!(),
                }
            }
        }
        if envelope.terminal {
            self.common.pending.remove(&req);
        } else {
            self.common.pending.get_mut(&req).unwrap().last_arrival = Some(envelope.arrival);
        }
        { publish = true; };
        true

}) {
                'apply_resource_observation: {
let (envelope,): (&SessionEnvelope,) = (&envelope,);

        use super::observation::Observation;
        if !self.accepts(envelope) { break 'apply_resource_observation false; }
        let req = envelope.addr.req.0;
        let pending = &self.common.pending[&req];
        if !pending.expected.matches(&self.common.persisted) { break 'apply_resource_observation false; }
        let SessionArrival::Data(data) = &envelope.outcome else {
            if pending.key.op == SessionOp::ProfileSwitch && pending.phase == StreamPhase::Running {
                { let event = AttemptEvent::SwitchDropped; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
            } else if pending.key.op == SessionOp::HomeRoster {
                {
let (reason,): (&str,) = (roster_unreachable(),);

        if self.state.picker_data().users.is_empty() && self.state.phase() == Phase::Profiles {
            { let event = AttemptEvent::RosterFailed { message: reason.to_owned() }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }

};
            }
            {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
            { publish = true; };
            break 'apply_resource_observation true;
        };
        let mut delta = CommitDelta::default();
        // Purpose is decided by the op, not by whether the write happens to be a credential
        // patch: a background authorisation refresh must never read as proof of a saved login.
        // Login/Rediscover's SignedIn write is the DISCOVERY save — the FINAL one is take_ready's,
        // once the picker/Ready decision has been made.
        let purpose = match pending.key.op {
            SessionOp::Login | SessionOp::Rediscover => PersistencePurpose::Discovery,
            SessionOp::ProfileSwitch => PersistencePurpose::Profile,
            SessionOp::HomeRoster | SessionOp::ServerRoster | SessionOp::Endpoint(_)
                | SessionOp::Ready | SessionOp::Picker | SessionOp::DevBoundary => PersistencePurpose::Background,
        };
        // A PEEK, never a consume: a discovery/rediscovery write must not spend the fresh
        // authority the FINAL write (take_ready) needs (AUTH-04). Only take_ready consumes it.
        let authority = if matches!(pending.key.op, SessionOp::Login | SessionOp::Rediscover)
            && self.state.fresh_authority() {
            plx_plex::plex::session::SaveAuthority::FreshReauthentication
        } else { plx_plex::plex::session::SaveAuthority::Routine };
        let mut plan = CommitPlan { registry_client_id: self.common.persisted.client_id.clone(),
            expected_disk: self.common.disk_identity.clone(),
            credentials: None, registry: Vec::new(), lifecycle: pending.lifecycle,
            purpose, writes_durable: false, authority };
        match &**data {
            Observation::Login(super::LoginProgress::SignedIn { server, sources, users, .. }) => {
                let mut next = self.common.persisted.clone();
                next.server = server.clone();
                next.sources = sources.clone();
                next.home_users = users.iter().map(super::UserTile::to_ref).collect();
                // `Authorized` emptied the offline profile cache because the new token names no
                // account. The roster does: the same owner as the stored record gets that
                // record's cache back (nothing of another account's, nor when either is unknown).
                if pending.key.op == SessionOp::Login && self.state.fresh_authority()
                    && next.profiles.is_empty()
                    && plx_plex::plex::session::same_owner(&self.common.committed_credentials.home_users,
                        &next.home_users) {
                    next.profiles = self.common.committed_credentials.profiles.clone();
                }
                // A fresh QR sign-in's account token is the owner's plex.tv credential. Keep it
                // on the owner profile too; rediscovery can belong to a managed active profile
                // and must not overwrite that profile's switch credential with the owner's.
                if pending.key.op == SessionOp::Login && !next.account_token.is_empty() {
                    next.user.plex_tv_token = Some(next.account_token.clone());
                }
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                plan.credentials = Some(patch);
                delta.roster_answered = !users.is_empty();
                if users.len() > 1 {
                    delta.destination = CommitDestination::SignInProfiles;
                    delta.phase = Some(Phase::Profiles);
                    delta.picker = Some(Picker::SignedIn);
                    delta.users = Some(users.clone());
                } else {
                    delta.destination = CommitDestination::SignInReady;
                    delta.phase = Some(Phase::Ready);
                    delta.ready = Some(true);
                }
                delta.complete_signin = true;
            }
            Observation::Login(_) => break 'apply_resource_observation false,
            Observation::Registry(progress) => {
                if pending.key.op == SessionOp::ServerRoster
                    && !self.common.persisted.active_profile_is_admin() {
                    // This operation fetched `/resources` with the ACCOUNT token. Its Activate
                    // and Install observations therefore carry the account holder's grants, not
                    // the seated profile's. The identity/epoch checks above prove which request
                    // this is; the profile predicate decides whether its credentials may cross
                    // the owner's commit boundary. Settled is credential-free and remains useful.
                    if let super::RegistryProgress::Settled { probe, .. } = progress {
                        plan.registry.push(RegistryPlan::Probe(probe.clone()));
                    }
                } else {
                    plan.registry.push(match progress {
                        super::RegistryProgress::Activate { candidate, .. } => RegistryPlan::Activate {
                            source: plx_plex::plex::session::SourceRef {
                                machine_id: candidate.machine_id.clone(), token: candidate.token.clone(),
                                name: candidate.name.clone(), shared_by: candidate.credit.clone(), owned: candidate.owned,
                                home: candidate.home, owner_id: candidate.owner_id,
                                origin_url: candidate.origin.base(), address: candidate.address.clone(),
                                port: i64::from(candidate.origin.port()), tier: Some(candidate.location),
                                extensions: Default::default(),
                            }, ipv6: candidate.ipv6,
                            // A roster refresh lists `/resources` with the ACCOUNT token; the seated
                            // profile is admin (gate above) but not necessarily that account. Only
                            // a server both call OWNED proves them one identity. Every other
                            // operation's grants are the identity it is establishing.
                            same_identity: pending.key.op != SessionOp::ServerRoster
                                || super::seated_is_account_holder(&self.common.persisted,
                                    candidate.owned.then_some(candidate.machine_id.as_str())),
                        },
                        super::RegistryProgress::Settled { probe, .. } => RegistryPlan::Probe(probe.clone()),
                        super::RegistryProgress::Install { sources, primary, .. } =>
                            RegistryPlan::Install { sources: sources.clone(), primary: *primary, commit: RosterCommit::Merge },
                    });
                }
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
                        if graded.is_none() {
                            {
let (reason,): (&str,) = (roster_unreachable(),);

        if self.state.picker_data().users.is_empty() && self.state.phase() == Phase::Profiles {
            { let event = AttemptEvent::RosterFailed { message: reason.to_owned() }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }

};
                        } else {
                            {

        if self.state.picker_data().users.is_empty() && self.state.phase() == Phase::Profiles {
            { let event = AttemptEvent::RosterFailed { message: roster_refused().into() }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
            self.common.switch_refused_for = Some(Identity::of(&self.common.persisted));
        }

};
                        }
                        {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                        { publish = true; };
                        break 'apply_resource_observation true;
                    }
                };
                let mut next = self.common.persisted.clone();
                next.home_users = users.iter().map(super::UserTile::to_ref).collect();
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                delta.destination = CommitDestination::Roster;
                delta.users = Some(users.clone());
                delta.roster_answered = true;
                plan.credentials = Some(patch);
            }
            Observation::ServerRoster(progress) => match &progress.outcome {
                super::ServerRosterOutcome::Unreachable => {
                    {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                    break 'apply_resource_observation true;
                }
                super::ServerRosterOutcome::NoReachable { settled } => {
                    // R2/A5: nothing was found to REGISTER, but a probe may still have verified
                    // something worth recording (an InsecureOnly answer, most of all) — a
                    // registry-only commit, the same shape `start_switch`'s initial-primary plan
                    // already uses: no credential patch, no lifecycle, just the probe facts.
                    if settled.is_empty() {
                        {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                        break 'apply_resource_observation true;
                    }
                    plan.registry = settled.iter().cloned().map(RegistryPlan::Probe).collect();
                }
                super::ServerRosterOutcome::Reconcile {
                    resources, found, admitted_machine_id, household, settled,
                } => {
                    if !self.common.persisted.active_profile_is_admin() {
                        if settled.is_empty() {
                            {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                            break 'apply_resource_observation true;
                        }
                        plan.registry = settled.iter().cloned().map(RegistryPlan::Probe).collect();
                    } else {
                        let mut next = self.common.persisted.clone();
                        let refreshed = super::refreshed_sources(&next.sources, found, resources, household);
                        let usable = !refreshed.is_empty();
                        let sources = if usable { refreshed } else { next.sources.clone() };
                        let roster_changed = !super::same_sources(&sources, &next.sources);
                        let moved = super::reconcile_refresh_session(
                            &mut next, &sources, admitted_machine_id);
                        next.sources = sources.clone();
                        let repaired = next.refresh_profile_record();
                        if !(roster_changed || moved || repaired) {
                            if settled.is_empty() {
                                {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                                break 'apply_resource_observation true;
                            }
                            plan.registry = settled.iter().cloned().map(RegistryPlan::Probe).collect();
                        } else {
                            let patch = CredentialPatch::of(&next);
                            delta.credentials = Some(patch.clone());
                            plan.credentials = Some(patch);
                            let primary = Some(sources.iter()
                                .position(|source| source.machine_id == *admitted_machine_id)
                                .expect("the admitted primary is part of the reconciled roster"));
                            // The seated identity is unchanged — only admins reach this arm, and
                            // the answer was fetched with this very account's token — so this is
                            // a REFRESH, never a switch: a rotated grant for the same user must
                            // not revoke every live client (owner trace 2026-09-30).
                            let same_identity = super::seated_is_account_holder(
                                &self.common.persisted,
                                resources.iter().filter(|r| r.is_server() && r.owned)
                                    .map(|r| r.client_identifier.as_str()));
                            plan.registry = vec![RegistryPlan::Install {
                                sources: sources.clone(), primary,
                                commit: RosterCommit::Refresh { same_identity },
                            }];
                            plan.registry.extend(settled.iter().cloned().map(RegistryPlan::Probe));
                        }
                    }
                }
            },
            Observation::ProfileSwitch(progress) => match &progress.outcome {
                super::ProfileSwitchOutcomeProgress::Failed { error, pin_denied } => {
                    { let event = AttemptEvent::SwitchFailed { message: error.clone(), denied: *pin_denied }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
                    {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                    { publish = true; };
                    break 'apply_resource_observation true;
                }
                super::ProfileSwitchOutcomeProgress::Ready { delta: profile, probes } => {
                    let mut next = self.common.persisted.clone();
                    super::merge_profile_delta(&mut next, profile.clone());
                    delta.credentials = Some(CredentialPatch::of(&next));
                    delta.destination = CommitDestination::ProfileReady;
                    delta.phase = Some(Phase::Ready);
                    delta.ready = Some(true);
                    delta.clear_error = true;
                    delta.profile_seated = true;
                    plan.registry = Self::roster_plan(&next, probes, RosterCommit::Switch);
                }
            },
            Observation::ProfileRoster(progress) => {
                let mut next = self.common.persisted.clone();
                let sources = super::profile_sources(&next.sources, &progress.reached,
                    &progress.resources, &next.household_ids());
                let Some(primary) = sources.iter()
                    .find(|s| s.machine_id == next.server.machine_id && s.dialable())
                    .or_else(|| sources.iter().find(|s| s.dialable())).cloned() else {
                    {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                    break 'apply_resource_observation true;
                };
                next.sources = sources;
                next.server = super::server_ref(&primary);
                next.user.token = primary.token;
                next.refresh_profile_record();
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                if !self.state.handoff_pending() { plan.credentials = Some(patch); }
                // This request's `Ready` already seated this profile and committed its roster as
                // the `Switch` — the one revoke that change of identity owes. This late roster is
                // for that SAME seat, and that is proven, not assumed: `matches_request` admits it
                // only in `ProfileSeated` and only for `progress.expected == pending.expected`;
                // the `Ready` commit stamped `pending.expected` from the seat it installed; and
                // `observe` dropped it above unless that identity is still the persisted one. So
                // it is a same-identity refresh — retoken in place, retire what is no longer
                // granted — never a second switch that blanks Home's live clients seconds after
                // they drew (owner trace 2026-09-30, who's-watching picker path).
                debug_assert!(pending.phase == StreamPhase::ProfileSeated
                    && pending.expected.matches(&self.common.persisted));
                let commit = RosterCommit::Refresh { same_identity: true };
                plan.registry = Self::roster_plan(&next, &progress.probes, commit);
            }
            Observation::Endpoint(progress) => {
                let Some(lifecycle) = pending.lifecycle else { break 'apply_resource_observation false; };
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
                            {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                            break 'apply_resource_observation true;
                        }
                    },
                    Some(fresh) => {
                        let mut next = self.common.persisted.clone();
                        let Some((source, changed)) = super::apply_refreshed_endpoint(&mut next, &progress.machine_id, fresh) else {
                            {
let (req,): (u32,) = (req,);

        if self.common.pending.contains_key(&req) {
            { let event = AttemptEvent::WorkRetired { req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        }
        self.common.pending.remove(&req);
        fx.push(SessionFx::Retire { req });

};
                            break 'apply_resource_observation true;
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
                        if (changed || repaired) && !self.state.handoff_pending() { plan.credentials = Some(patch); }
                        plan.registry.push(RegistryPlan::Endpoint { expected: lifecycle, source });
                    }
                }
            }
        }
        plan.writes_durable = plan.credentials.is_some();
        'begin_commit: {
let (req, arrival, terminal, plan, delta,): (u32, u64, bool, CommitPlan, CommitDelta,) = (req, envelope.arrival, envelope.terminal, plan, delta,);

        let Some(entry) = ('commit: { let (req, arrival, terminal, plan, delta): (u32, u64, bool, CommitPlan, CommitDelta) = (req, arrival, terminal, plan, delta); let Some(CommitPreparation { pending, entry }) = self.plan_commit(req, arrival, terminal, plan, delta) else { break 'commit None }; self.common.pending.get_mut(&req).unwrap().last_arrival = Some(arrival); self.common.pending_commit = Some(pending); self.common.persistence_purpose = Some(entry.plan.purpose); Some(entry) }) else { break 'begin_commit false };
        entry.emit(&mut |effect| fx.push(effect));
        true

}

};
            }
        }
        if let Some(commit) = &self.common.pending_commit {
            assert!(commit.req == receipt.addr.req.0 && commit.epoch == receipt.key.epoch
                && commit.arrival == receipt.arrival, "Session commit receipt mismatch");
            self.common.pending_commit.as_mut().unwrap().receipt = Some(receipt);
        } else {
            fx.push(SessionFx::Acknowledge(vec![receipt]));
        }
        {

        if !self.common.inbox.is_empty() && self.common.pending_commit.is_none() && !self.common.pump_pending {
            self.common.pump_pending = true;
            fx.push(SessionFx::Pump);
        }

};

}; }
// All login entry paths converge here; the reply follows its ordered exit and entry effects.
let mut restarted = false;
if let Some(restart) = restart { restarted = 'restart_login: {
let (fresh_login,): (bool,) = (matches!(restart, LoginRestart::Fresh),);

        if matches!(self.common.authority, BootstrapAuthority::DevPms { .. }) {
            break 'restart_login (fresh_login || self.state   .phase() == Phase::Error) && 'begin_dev_account: {

        if self.state .pending_erase().is_some() || self.common.epoch.checked_add(1).is_none()
            || self.common.next_req.checked_add(2).is_none()
            || self.common.pending_commit.as_ref().is_some_and(|c|
                matches!(c.delta.dev, Some(DevCommitDelta::StartAccount { .. }))) { break 'begin_dev_account false; }
        ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).expect("dev login epoch preflight");
        {
let (profile,): (Option<UserRef>,) = (None,);

        // This is the sole scope allocator. Resource publication will store this explicit value
        // rather than independently incrementing another generation.
        self.common.profile_scope.0 = self.common.profile_scope.0.wrapping_add(1);
        self.common.active_profile = profile;
        fx.push(SessionFx::PublishProfile(ProfilePublication {
            epoch: self.common.epoch, profile: self.common.active_profile.clone(), scope: self.common.profile_scope,
            stamp: plx_plex::plex::grant::account_key(&self.common.persisted.account_token),
        }));

};
        let req = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::DevBoundary, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }).expect("two-slot preflight");
        let login_req = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (SessionOp::Login, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }).expect("two-slot preflight");
        { let event = AttemptEvent::BeginDevAccount { boundary_req: req, login_req }; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        'begin_commit: {
let (req, arrival, terminal, plan, delta,): (u32, u64, bool, CommitPlan, CommitDelta,) = (req, 0, true, CommitPlan { registry_client_id: self.common.persisted.client_id.clone(),
            expected_disk: self.common.disk_identity.clone(),
            credentials: None, lifecycle: None, registry: vec![RegistryPlan::Revoke],
            purpose: PersistencePurpose::Background, writes_durable: false,
            authority: plx_plex::plex::session::SaveAuthority::Routine }, CommitDelta {
                dev: Some(DevCommitDelta::StartAccount { login_req }), ..Default::default()
            },);

        let Some(entry) = ('commit: { let (req, arrival, terminal, plan, delta): (u32, u64, bool, CommitPlan, CommitDelta) = (req, arrival, terminal, plan, delta); let Some(CommitPreparation { pending, entry }) = self.plan_commit(req, arrival, terminal, plan, delta) else { break 'commit None }; self.common.pending.get_mut(&req).unwrap().last_arrival = Some(arrival); self.common.pending_commit = Some(pending); self.common.persistence_purpose = Some(entry.plan.purpose); Some(entry) }) else { break 'begin_commit false };
        entry.emit(&mut |effect| fx.push(effect));
        true

};
        { publish = true; };
        true

};
        }
        if self.state   .pending_erase().is_some() { break 'restart_login false; }
        if self.common.epoch.checked_add(1).is_none() { break 'restart_login false; }
        if self.common.next_req.checked_add(1).is_none() { break 'restart_login false; }
        let discovery = !fresh_login && super::retry_kind(self.state   .phase(),
            self.state.fresh_authority()) == super::RetryKind::Discovery;
        let fresh_attempt = fresh_login || super::restart_is_a_new_attempt(self.common.signin_notice.owed());
        ('advance_epoch: {

        let exit = match ExitFx::epoch(self.view()) { Some(value) => value, None => break 'advance_epoch None };
        let epoch = exit.epoch_number();
        self.common.inbox.clear();
        self.common.pending_commit = None;
        self.common.pending.clear();
        self.common.epoch = epoch;
        { let event = AttemptEvent::EpochAdvanced; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        // Keep pump_pending: a marker may still be carried outside the owner.
        exit.emit(&mut |effect| fx.push(effect));
        Some(epoch)

}).expect("epoch exhaustion checked before transition");
        if discovery {
            // 0.6.6 parity (`discovery-warning-not-cleared-on-retry-or-fresh-success`): a restart
            // begins a fresh discovery attempt under a NEW epoch, so a warning (or a held handoff)
            // left over from the attempt being retried can never be released or acknowledged by
            // anything this new epoch does — it would sit stale forever. A rediscovery restart
            // clears both, the same way the fresh-attempt branch below already does.
            self.common.persistence_warning = None;
            self.common.held_handoff = None;
            self.common.persistence_warning_answered = false;
        } else {
            self.common.persisted = self.common.committed_credentials.merge_into(&self.common.persisted);
            self.common.persistence_warning = None;
            self.common.held_handoff = None;
            self.common.persistence_warning_answered = false;
        }
        let op = if discovery { SessionOp::Rediscover } else { SessionOp::Login };
        let req = ('reservation: { let (op, lifecycle): (SessionOp, Option<ServerLifecycle>) = (op, None); let Some(WorkReservation { req, pending }) = self.plan_reservation(op, lifecycle) else { break 'reservation None }; self.common.next_req = req; self.common.pending.insert(req, pending); Some(req) }).expect("request exhaustion checked before transition");
        let event = if discovery { AttemptEvent::RestartDiscovery { req } } else { AttemptEvent::RestartLogin { req } };
        { let event = event; let exit = ExitFx::attempt(self.view(), &event); let entry = EntryFx::attempt(self.view(), &event); if let Some(notice) = signin_notice(&event) { self.common.signin_notice = notice; } exit.emit(&mut |effect| fx.push(effect)); let attempt = std::mem::replace(&mut self.state, attempt::Attempt::placeholder()); self.state = next_attempt(attempt, event); entry.emit(&mut |effect| fx.push(effect)); true };
        let entry = EntryFx::login(self.view(), req, discovery, fresh_attempt);
        let pending = self.common.pending.get_mut(&req).expect("allocated login request");
        match entry.work() {
            EntryWork::Capture { .. } => pending.capture = Some(CaptureIntent::Login),
            EntryWork::Work { admission, .. } => pending.admission = AdmissionState::Awaiting(*admission),
        }
        entry.emit(&mut |effect| fx.push(effect));
        { publish = true; };
        true

}; if !matches!(ev, SessionEvent::Erased { .. } | SessionEvent::Command(Command::RestartWait { .. })) { handled = restarted; } }
if let Some((reply, permitted)) = restart_reply { fx.push(SessionFx::RestartReply { to: *reply, accepted: permitted && restarted }); }
// Publications are observable only after this atomic event has finished.
if publish { if let Some((next, allocations)) = self.publication_update() { self.publication = next; #[cfg(test)] for (total, count) in self.publication_payload_allocations.iter_mut().zip(allocations) { *total += count; } #[cfg(not(test))] let _ = allocations; } }
if !matches!(ev, SessionEvent::CacheObserved(_) | SessionEvent::CacheRefreshed { .. } | SessionEvent::ConsumeLogicalDirty) { {

        let mut canon = Canon::new();
        LegacyV5Projection::from_partition(self.view()).write(&mut canon);
        let next = canon.finish();
        if next != self.subhash {
            self.subhash = next;
            self.logical_dirty = true;
        }

}; }
handled
}
    #[cfg(test)] fn replace_publication(&mut self) { self.transition(&SessionEvent::RebuildPublication, 0, &mut Vec::new()); }
}

impl<H: SessionHost> plx_machine::machine::Machine<H> for SessionMachine {
    type Ev = SessionEvent;

    fn step(&mut self, ev: &SessionEvent, cx: &plx_machine::machine::Cx<'_, H>, fx: &mut plx_machine::machine::Effects<'_, H>) -> plx_machine::machine::Handled {
 use plx_machine::machine::{Fx, Handled};
 let before = self.publication();
 let mut effects = Vec::new();
 let handled = self.transition(ev, cx.tick.ms, &mut effects);
 for effect in effects { fx.push(Fx::App(H::session_effect(effect))); }
 if !Arc::ptr_eq(&before, &self.publication) { fx.invalidate(plx_machine::present::Provenance::Landing(MachineId::Session)); }
 if handled { Handled::Yes } else { Handled::No }
}
}

#[cfg(any(test, feature = "test-support"))]
pub mod scenarios;

#[cfg(test)]
mod tests {
    use super::*;
    use super::scenarios::{roster_refresh_fixture, step, step_at, OwnerHost};

    #[test]
    fn logical_dirty_drain_is_an_owner_event_without_effects_or_publication_damage() {
        let mut owner = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
        let publication = owner.publication();
        let subhash = owner.subhash();
        let mut effects = Vec::new();
        assert!(owner.transition(&SessionEvent::ConsumeLogicalDirty, 0, &mut effects));
        assert!(!owner.transition(&SessionEvent::ConsumeLogicalDirty, 0, &mut effects));
        assert!(effects.is_empty());
        assert_eq!(owner.subhash(), subhash);
        assert!(Arc::ptr_eq(&publication, &owner.publication()));
    }

    #[test]
    fn legacy_incident_callback_forwards_the_typed_event_effect_drain() {
        let mut legacy = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
        let context = IncidentContext::new(IncidentKind::PinCreate, None);
        legacy.raise_incident(IncidentFlow::SignIn, context);
        let id = legacy.common.incident.as_ref().unwrap().id;
        let mut typed = SessionMachine::from_init(legacy.snapshot_init());
        let mut observed = Vec::new();
        assert!(legacy.resolve_incident(id, Permission::Granted, 1, &mut |effect| observed.push(effect)));
        let mut expected = Vec::new();
        assert!(typed.transition(&SessionEvent::Command(Command::ResolveIncident {
            id, permission: Permission::Granted, revision: 1,
        }), 0, &mut expected));
        assert_eq!(observed.len(), 1);
        assert_eq!(expected.len(), 1);
        for effect in observed.iter().chain(expected.iter()) {
            assert!(matches!(effect, SessionFx::Incident { id: seen, lane: IncidentLane::Standing,
                report: IncidentReport::Retained(evidence) } if *seen == id && *evidence == context));
        }
        assert_eq!(legacy.snapshot_init().incident, typed.snapshot_init().incident);
        assert_eq!(legacy.subhash(), typed.subhash());
    }

    #[test]
    fn standalone_incident_events_publish_the_owned_readout() {
        let mut owner = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
        let context = IncidentContext::new(IncidentKind::PinCreate, None);
        let before = owner.publication();
        let mut effects = Vec::new();
        assert!(owner.transition(&SessionEvent::RaiseIncident { flow: IncidentFlow::SignIn, context }, 0, &mut effects));
        assert!(owner.common.incident.is_some());
        assert_eq!(owner.read().0.incident, owner.snapshot_init().incident);
        assert!(!Arc::ptr_eq(&before, &owner.publication()));
        let before = owner.publication();
        assert!(owner.transition(&SessionEvent::ForgetIncidents, 0, &mut effects));
        assert!(owner.common.incident.is_none());
        assert!(owner.read().0.incident.is_none());
        assert!(!Arc::ptr_eq(&before, &owner.publication()));
        assert!(effects.is_empty());
    }

    /// The session-cache refresh bookkeeping (`CacheRefresh`): driven by tick values, no sleeping.
    mod cache_refresh {
        use super::*;
        use plx_plex::plex::session::{CacheRefreshOutcome, RefreshNeed, LOCKED_RETRY_MS};

        fn need(generation: u64, transient: bool) -> SessionEvent {
            SessionEvent::CacheObserved(RefreshNeed { generation, transient })
        }
        fn refreshed(req: u32, outcome: CacheRefreshOutcome) -> SessionEvent {
            SessionEvent::CacheRefreshed { req, outcome }
        }
        fn refreshes(effects: &[SessionFx]) -> Vec<(u32, u64)> {
            effects.iter().filter_map(|fx| match fx {
                SessionFx::RefreshCache { req, generation } => Some((*req, *generation)),
                SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
                | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
                | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
                | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
                | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
                | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. } => None,
            }).collect()
        }
        fn owner() -> SessionMachine { SessionMachine::from_init(captured_session()) }

        #[test]
        fn one_refresh_per_need_and_nothing_while_one_is_in_flight() {
            let mut owner = owner();
            assert_eq!(refreshes(&step_at(&mut owner, need(4, false), 0)), [(1, 4)]);
            assert_eq!(owner.cache_refresh_in_flight(), Some(1));
            for ms in [1, 16, 5_000] {
                assert!(step_at(&mut owner, need(4, false), ms).is_empty(), "in flight at {ms}");
                assert!(step_at(&mut owner, need(9, true), ms).is_empty(), "any need, in flight at {ms}");
            }
            assert_eq!(owner.cache_refresh_in_flight(), Some(1));
            step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Settled), 20);
            assert_eq!(owner.cache_refresh_in_flight(), None);
            assert_eq!(refreshes(&step_at(&mut owner, need(5, false), 21)), [(2, 5)], "a new request id");
        }

        #[test]
        fn a_transient_result_holds_back_only_its_own_generation() {
            let mut owner = owner();
            step_at(&mut owner, need(4, false), 0);
            step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Transient { generation: 5 }), 10);
            assert_eq!(owner.cache_refresh_in_flight(), None);
            assert!(step_at(&mut owner, need(5, true), 500).is_empty(), "the standing transient waits");
            assert!(step_at(&mut owner, need(5, true), 10 + LOCKED_RETRY_MS - 1).is_empty());
            assert_eq!(refreshes(&step_at(&mut owner, need(6, false), 500)), [(2, 6)],
                "a drop to Unloaded inside the window still refreshes at once");
            step_at(&mut owner, refreshed(2, CacheRefreshOutcome::Transient { generation: 7 }), 600);
            assert_eq!(refreshes(&step_at(&mut owner, need(7, true), 600 + LOCKED_RETRY_MS)), [(3, 7)],
                "the deadline is on the tick the result arrived, and it reads at the deadline");
        }

        #[test]
        fn a_refused_result_holds_back_every_generation() {
            let mut owner = owner();
            step_at(&mut owner, need(4, false), 0);
            step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Refused), 10);
            for generation in [4, 5, 99] {
                assert!(step_at(&mut owner, need(generation, false), 500).is_empty(), "generation {generation}");
            }
            assert_eq!(refreshes(&step_at(&mut owner, need(5, false), 10 + LOCKED_RETRY_MS)), [(2, 5)]);
        }

        #[test]
        fn settled_and_superseded_clear_the_backoff() {
            for outcome in [CacheRefreshOutcome::Settled, CacheRefreshOutcome::Superseded] {
                let mut owner = owner();
                step_at(&mut owner, need(4, false), 0);
                step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Refused), 10);
                step_at(&mut owner, need(4, false), 10 + LOCKED_RETRY_MS);
                step_at(&mut owner, refreshed(2, outcome), 2_000);
                assert_eq!(refreshes(&step_at(&mut owner, need(8, false), 2_001)), [(3, 8)], "{outcome:?}");
            }
        }

        #[test]
        fn the_first_transient_observation_arms_a_backoff_instead_of_reading() {
            // A boot that read Locked/Blocked (the storage helper down) keeps the old first retry
            // about a second later, not a read on frame one.
            let mut owner = owner();
            assert!(step_at(&mut owner, need(3, true), 100).is_empty());
            assert_eq!(owner.cache_refresh_in_flight(), None);
            assert!(step_at(&mut owner, need(3, true), 100 + LOCKED_RETRY_MS - 1).is_empty());
            assert_eq!(refreshes(&step_at(&mut owner, need(3, true), 100 + LOCKED_RETRY_MS)), [(1, 3)]);
        }

        #[test]
        fn backoff_deadlines_survive_the_tick_wrapping() {
            let mut owner = owner();
            step_at(&mut owner, need(4, false), u32::MAX - 20);
            step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Refused), u32::MAX - 5);
            assert!(step_at(&mut owner, need(4, false), 10).is_empty(), "16 ms into the window");
            assert!(step_at(&mut owner, need(4, false), LOCKED_RETRY_MS - 7).is_empty());
            assert_eq!(refreshes(&step_at(&mut owner, need(4, false), LOCKED_RETRY_MS - 6)), [(2, 4)]);
        }

        /// A backoff that outlives its need (a write filled the cache inside its second, so no
        /// observation ever cleared it) must not come back to hold a much later need: a deadline
        /// compared as a signed wrapping difference flips to "not over" once the process has run
        /// 2^31 ms (24.8 days) past it. The hold is the unsigned time since it was armed.
        #[test]
        fn a_backoff_left_behind_does_not_hold_a_need_a_long_time_later() {
            // `10 + 2^31 + 1` is the review's tick; the signed deadline compare first flips at
            // `10 + LOCKED_RETRY_MS + 2^31`, so both are asked.
            for later in [10u32.wrapping_add(1 << 31).wrapping_add(1),
                10u32.wrapping_add(LOCKED_RETRY_MS).wrapping_add(1 << 31)] {
                let mut owner = owner();
                step_at(&mut owner, need(4, false), 0);
                step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Refused), 10);
                assert_eq!(refreshes(&step_at(&mut owner, need(4, false), later)), [(2, 4)], "at tick {later}");
            }
            // The transient arm reads the same clock: armed on the first observation, asked again
            // a long gap later.
            let mut owner = owner();
            assert!(step_at(&mut owner, need(3, true), 10).is_empty());
            let later = 10u32.wrapping_add(LOCKED_RETRY_MS).wrapping_add(1 << 31);
            assert_eq!(refreshes(&step_at(&mut owner, need(3, true), later)), [(1, 3)], "at tick {later}");
        }

        #[test]
        fn a_stale_or_unsolicited_answer_changes_nothing() {
            let mut owner = owner();
            step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Refused), 5);
            assert_eq!(refreshes(&step_at(&mut owner, need(4, false), 6)), [(1, 4)],
                "an answer with nothing in flight does not start a backoff");
            step_at(&mut owner, refreshed(77, CacheRefreshOutcome::Settled), 7);
            assert_eq!(owner.cache_refresh_in_flight(), Some(1), "another request's answer is not ours");
        }

        #[test]
        fn a_sign_out_while_a_refresh_is_in_flight_still_clears_the_flag() {
            let mut owner = owner();
            step_at(&mut owner, need(4, false), 0);
            let epoch = owner.snapshot_init().epoch;
            step(&mut owner, SessionEvent::Command(Command::SignOut));
            assert_ne!(owner.snapshot_init().epoch, epoch, "the sign-out advanced the epoch");
            assert_eq!(owner.cache_refresh_in_flight(), Some(1), "the sign-out does not answer for the job");
            step_at(&mut owner, refreshed(1, CacheRefreshOutcome::Superseded), 50);
            assert_eq!(owner.cache_refresh_in_flight(), None);
            assert_eq!(refreshes(&step_at(&mut owner, need(9, false), 51)), [(2, 9)]);
        }

        #[test]
        fn the_bookkeeping_is_outside_the_recorded_state() {
            let mut owner = owner();
            let (hash, publication, init) = (owner.subhash(), owner.publication(), owner.snapshot_init().hash());
            for (ev, ms) in [(need(4, false), 0), (refreshed(1, CacheRefreshOutcome::Refused), 10),
                (need(4, false), 2_000), (refreshed(2, CacheRefreshOutcome::Transient { generation: 5 }), 2_010)] {
                step_at(&mut owner, ev, ms);
                assert_eq!(owner.subhash(), hash);
                assert!(Arc::ptr_eq(&owner.publication(), &publication), "no publication, so no invalidation");
                assert_eq!(owner.snapshot_init().hash(), init);
                assert!(!owner.take_logical_dirty() || ms == 0, "no logical change");
            }
        }
    }

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

    // Moved from `i18n`'s tests: it grades `SessionInit`'s recorded hash, and `i18n` (a layer
    // below `auth`) cannot name `SessionInit`.
    #[test]
    fn captured_language_changes_the_recorded_session_hash() {
        use plx_machine::machine::LogicalState;
        let a = SessionInit::captured(PersistedSession::default());
        let mut b = a.clone();
        b.persisted.language = plx_platform::i18n::Preference::Be;
        assert_ne!(a.hash(), b.hash());
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

    fn captured_session() -> SessionInit {
        SessionInit::captured(PersistedSession { client_id: "synthetic-client".into(), ..Default::default() })
    }

    #[test]
    fn epoch_exit_plan_is_pure_and_preserves_cancel_receipt_fifo() {
        let mut owner = SessionMachine::from_init(captured_session());
        let first = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        let second = owner.allocate(SessionOp::HomeRoster, None).unwrap();
        let envelope = |req, arrival, op| SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch: 1, op }, admission: AdmissionId(req), arrival,
            terminal: true, lifecycle: None, outcome: SessionArrival::Refused,
        };
        let queued = envelope(second, 2, SessionOp::HomeRoster);
        let committed = envelope(first, 1, SessionOp::ServerRoster);
        let receipts = vec![Receipt::of(&queued), Receipt::of(&committed)];
        owner.common.inbox.push_back(queued);
        owner.common.pending_commit = Some(PendingCommit {
            req: first, epoch: 1, arrival: 1, terminal: true, writes_credentials: false,
            receipt: Some(Receipt::of(&committed)), delta: CommitDelta::default(),
            admitted_revision: None, purpose: None, fresh: false,
        });
        owner.common.pump_pending = true;
        let before = serde_json::to_value(owner.snapshot_init()).unwrap();
        let exit = ExitFx::epoch(owner.view()).unwrap();
        assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), before);
        let mut planned = Vec::new();
        exit.emit(&mut |fx| planned.push(fx));
        assert!(matches!(planned.as_slice(), [
            SessionFx::Cancel { requests: a, epoch: 1 },
            SessionFx::Acknowledge(r),
            SessionFx::Cancel { requests: b, epoch: 2 },
        ] if a == &vec![first] && r == &receipts && b == &vec![first, second]));
        let actual = step(&mut owner, SessionEvent::Command(Command::StartLogin));
        assert!(matches!(actual.as_slice(), [
            SessionFx::Cancel { requests: a, epoch: 1 },
            SessionFx::Acknowledge(r),
            SessionFx::Cancel { requests: b, epoch: 2 },
            SessionFx::Coordinator(CoordinatorAction::SignInStarted),
            SessionFx::Work { req: 3, .. },
        ] if a == &vec![first] && r == &receipts && b == &vec![first, second]));
        assert!(owner.common.pump_pending);
    }

    #[test]
    fn epoch_exit_plan_refuses_exhaustion_without_effects_or_mutation() {
        let mut init = captured_session();
        init.epoch = u64::MAX;
        let mut owner = SessionMachine::from_init(init);
        let before = serde_json::to_value(owner.snapshot_init()).unwrap();
        assert!(ExitFx::epoch(owner.view()).is_none());
        assert!(step(&mut owner, SessionEvent::Command(Command::StartLogin)).is_empty());
        assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), before);
    }

    #[test]
    fn login_entry_plans_capture_or_work_without_mutating_the_ledger() {
        for (client, discovery) in [("", false), ("synthetic-client", false), ("", true), ("synthetic-client", true)] {
            let mut init = captured_session();
            init.persisted.client_id = client.into();
            init.persisted.account_token = "synthetic-account".into();
            let mut owner = SessionMachine::from_init(init);
            let op = if discovery { SessionOp::Rediscover } else { SessionOp::Login };
            let req = owner.allocate(op, None).unwrap();
            let before = serde_json::to_value(owner.snapshot_init()).unwrap();
            let entry = EntryFx::login(owner.view(), req, discovery, false);
            assert_eq!(serde_json::to_value(owner.snapshot_init()).unwrap(), before);
            let mut effects = Vec::new();
            entry.emit(&mut |fx| effects.push(fx));
            if discovery {
                assert!(matches!(effects.as_slice(), [SessionFx::Work { req: 1, key,
                    admission: AdmissionId(1), input: SessionWork::Rediscover { client_id, account_token } }]
                    if key.epoch == 1 && client_id == client && account_token == "synthetic-account"));
            } else if client.is_empty() {
                assert!(matches!(effects.as_slice(), [SessionFx::Capture { req: 1, epoch: 1,
                    request: SessionReadRequest::LoginClientId }]));
            } else {
                assert!(matches!(effects.as_slice(), [SessionFx::Work { req: 1, key,
                    admission: AdmissionId(1), input: SessionWork::Login { client_id } }]
                    if key.epoch == 1 && client_id == client));
            }
        }
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
    fn local_server() -> plx_plex::plex::session::ServerRef {
        plx_plex::plex::session::ServerRef { address: "127.0.0.1".into(), port: 32400,
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
    /// unconditionally — see [`plx_plex::plex::session::Session::active_profile_is_protected`]).
    fn reopened_after_authorization() -> SessionInit {
        let mut init = discovering_after_authorization();
        init.persisted.user.uuid = "u-1".into();
        init.persisted.home_users = vec![plx_plex::plex::session::HomeUserRef {
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
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, Operation, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
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
        let final_req = owner.common.next_req;
        let final_reply = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        step(&mut owner, SessionEvent::Commit(final_reply));
        let final_failed = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(final_failed));
        assert!(owner.common.held_handoff.is_some(), "rig: the Ready handoff is held");
        assert!(owner.common.persistence_warning.is_some(), "rig: a Final warning is showing");
        owner
    }

    /// ACCEPTANCE SPEC 1/5. Consuming a commit returns a TYPED admission separating authority
    /// currency from durability, and a stale completion settles nothing.
    #[test]
    fn commit_consumption_is_typed_and_a_stale_reply_settles_nothing() {
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        assert!(owner.common.pending_commit.is_some());
        // A registry-only commit asked storage for nothing and must not claim a revision.
        let registry_only = CommitReply { req, epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly };
        let effects = step(&mut owner, SessionEvent::Commit(registry_only));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Retire { .. })));
        assert_eq!(owner.common.commit_phase, CommitPhase { admitted: false, durable: false, proves_saved_login: false });
        assert!(owner.common.pending_commit.is_none());
        assert_eq!(owner.common.persistence_purpose, None,
            "a registry-only commit must not claim a persistence purpose");

        // A stale completion (wrong epoch) must not settle or retire anything.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let req = owner.common.next_req;
        let before = owner.snapshot_init().hash();
        let stale = CommitReply { req, epoch: owner.common.epoch + 9, arrival: 0,
            admission: CommitAdmission::StaleAuthority };
        assert!(step(&mut owner, SessionEvent::Commit(stale)).is_empty());
        assert_eq!(owner.snapshot_init().hash(), before,
            "a stale completion must change nothing");
        assert!(owner.common.pending_commit.is_some());
        assert_eq!(owner.common.commit_phase, CommitPhase { admitted: false, durable: false, proves_saved_login: false },
            "a stale completion must not report a durable admission");

        // The committed purpose survives into the owner's state so the caller can refuse to
        // treat a background refresh as proof of a saved login.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let req = owner.common.next_req;
        let durable = CommitReply { req, epoch: owner.common.epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 3, purpose: PersistencePurpose::Final } };
        assert!(owner.apply_commit_reply(durable, &mut |_| {}));
        assert_eq!(owner.common.commit_phase, CommitPhase { admitted: true, durable: false, proves_saved_login: false });
        assert_eq!(owner.common.persistence_purpose, None,
            "the purpose is cleared once the commit settles");
    }

    /// ACCEPTANCE SPEC 5 / Stage B bridge. A durability verdict is fenced by request, epoch,
    /// arrival AND revision, and only a saved-login-proving purpose may stand as saved-login
    /// evidence. Before this wiring the owner had no way to receive such a verdict at all, so a
    /// stale or background verdict could not even be told apart from the durable one.
    #[test]
    fn persistence_completion_is_fenced_and_background_never_proves_a_saved_login() {
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Operation, PersistOutcome, PersistenceCompletion, PersistenceCorrelation,
            PersistencePurpose,
        };
        let admit = |owner: &mut SessionMachine, purpose: PersistencePurpose| {
            let req = owner.common.next_req;
            let epoch = owner.common.epoch;
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
        assert!(owner.common.admitted_persistence.is_some(),
            "an admitted write must leave a fenced identity behind");
        let stale_epoch = durable_completion(correlation, PersistencePurpose::Final, 1, correlation.epoch + 9);
        deliver(&mut owner, stale_epoch);
        assert!(!owner.common.commit_phase.durable,
            "a completion from a retired epoch must be inert");
        assert!(!owner.common.commit_phase.proves_saved_login);
        assert!(owner.common.admitted_persistence.is_some(),
            "a fenced-out completion must not consume the admitted identity");

        // (b) A completion naming another revision is a different operation's verdict.
        let wrong_revision = durable_completion(correlation, PersistencePurpose::Final, 99, correlation.epoch);
        deliver(&mut owner, wrong_revision);
        assert!(!owner.common.commit_phase.durable,
            "a completion naming another revision is a different operation's verdict");

        // (c) The matching completion for a Final purpose DOES settle as saved-login evidence.
        let matching = durable_completion(correlation, PersistencePurpose::Final, 1, correlation.epoch);
        deliver(&mut owner, matching);
        assert!(owner.common.commit_phase.durable, "storage confirmed durability");
        assert!(owner.common.commit_phase.proves_saved_login,
            "a Final write is evidence a login was saved");
        assert!(owner.common.admitted_persistence.is_none(), "the identity is consumed once settled");

        // (d) The SAME matching completion for a Background purpose is durable but NOT evidence.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let correlation = admit(&mut owner, PersistencePurpose::Background);
        let background = durable_completion(correlation, PersistencePurpose::Background, 1, correlation.epoch);
        deliver(&mut owner, background);
        assert!(owner.common.commit_phase.durable, "a background refresh does become durable");
        assert!(!owner.common.commit_phase.proves_saved_login,
            "a background refresh must never be offered as proof of a saved login");

        // (e) A purpose mismatch is fenced even when the correlation matches exactly.
        let mut owner = SessionMachine::from_init(local_session());
        assert!(owner.resume_stored(&mut |_| {}));
        let correlation = admit(&mut owner, PersistencePurpose::Background);
        let mismatched = durable_completion(correlation, PersistencePurpose::Final, 1, correlation.epoch);
        deliver(&mut owner, mismatched);
        assert!(!owner.common.commit_phase.durable,
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
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, Operation, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        assert!(owner.state.projected_authorization(), "the rig starts with a completed PIN authorization");

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
        assert_eq!(plan.authority, plx_plex::plex::session::SaveAuthority::FreshReauthentication);
        assert!(owner.state.projected_authorization(), "a discovery commit only PEEKS the authority");

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
        assert!(owner.common.persistence_warning.is_none(), "the discovery write DID land durably");

        // take_ready now issues the FINAL commit, consuming the authority exactly once.
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let Some(SessionFx::Commit { plan: final_plan, .. }) =
            effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("TakeReady must begin the final commit");
        };
        assert_eq!(final_plan.purpose, PersistencePurpose::Final);
        assert_eq!(final_plan.authority, plx_plex::plex::session::SaveAuthority::FreshReauthentication);
        assert!(!owner.state.projected_authorization(), "the final commit consumes the authority");

        let final_req = owner.common.next_req;
        let final_reply = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        let effects = step(&mut owner, SessionEvent::Commit(final_reply));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "a FRESH admission is held, not announced, until its durability is known");
        assert!(owner.common.held_handoff.is_some(), "the Ready handoff is held for this commit");

        // The final write's OWN completion fails to confirm durable: this is the AUTH-03 gate.
        let final_failed = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        let effects = step(&mut owner, SessionEvent::Persistence(final_failed));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "MUTATION M1 TARGET: skipping the gate would announce Ready right here");
        assert_eq!(owner.common.persistence_warning.map(|w| w.site), Some(PersistenceWarningSite::Final));
        assert_eq!(owner.publication().persistence_warning.map(|w| w.site), Some(PersistenceWarningSite::Final),
            "the warning must reach the UI-facing publication, not only internal state");
        assert!(!owner.common.commit_phase.proves_saved_login,
            "no saved/final claim before the user acknowledges");

        let warning_key = owner.common.persistence_warning.unwrap().key;
        let wrong = PersistenceWarningKey { epoch: warning_key.epoch, req: warning_key.req + 1 };
        assert!(!step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: wrong }))
            .iter().any(|fx| matches!(fx, SessionFx::Ready { .. })));
        assert!(owner.common.persistence_warning.is_some(), "a mismatched key must not clear the warning");

        let effects = step(&mut owner,
            SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: warning_key }));
        assert_eq!(effects.iter().filter(|fx| matches!(fx, SessionFx::Ready { .. })).count(), 1,
            "acknowledging releases exactly the ONE held handoff, exactly once");
        assert!(owner.common.persistence_warning.is_none());
        assert!(owner.common.held_handoff.is_none());
    }

    #[test]
    fn declined_warning_reconstructs_every_persistence_class_and_its_evidence() {
        use plx_plex::plex::session::async_persistence::{CompletionOutcome as O, Failure as F, Operation, PersistOutcome};
        use plx_plex::plex::session::persistence::ProtectionFailure;
        use plx_platform::storage::wire::{AuthPreservation, ErrorCode, KeymanagerFailure, KeymanagerFailureCategory,
            KeymanagerOperation, KeymanagerStage};
        use plx_platform::storage::wire::failure::{HelperFailure, Stage};
        use plx_telemetry::telemetry::incident::{IncidentKind, PersistenceFailure as P};
        let protection = ProtectionFailure {
            failure: KeymanagerFailure { operation: KeymanagerOperation::Seal, stage: KeymanagerStage::Finish,
                code: ErrorCode::Unavailable, category: KeymanagerFailureCategory::ServiceRejected, service_code: Some(-3961) },
            preservation: AuthPreservation::Unchanged, db8_commit_verified: false,
        };
        let helper = HelperFailure::new(Stage::Db8, Some(-3963));
        let mut errnos = [None; 8];
        errnos[0] = Some(libc::EACCES);
        for (outcome, class) in [
            (O::Failed(F::Admission(plx_base::storage_worker::SubmitError::Full)), P::Admission),
            (O::Failed(F::Persistence(PersistOutcome::WriteFailed)), P::WriteFailed),
            (O::Failed(F::Storage(plx_platform::storage::StoreError::HelperUnavailable)), P::Storage),
            (O::Failed(F::Helper(helper, errnos)), P::Storage),
            (O::Uncertain { stage: plx_platform::storage::CommitStage::Readback, errno: 0, helper: Some((helper, errnos)) }, P::CommitUncertain),
            (O::Uncertain { stage: plx_platform::storage::CommitStage::ParentSync, errno: 5, helper: None }, P::CommitUncertain),
            (O::Failed(F::Protection(protection)), P::Protection),
            (O::ProtectionUncertain(ProtectionFailure { preservation: AuthPreservation::Uncertain, ..protection }), P::ProtectionUncertain),
            (O::Failed(F::WorkerDropped), P::WorkerDropped),
        ] {
            let mut owner = SessionMachine::from_init(discovering_after_authorization());
            let expected = IncidentContext { kind: IncidentKind::SaveFailed,
                ..IncidentContext::internal(InternalClass::CommitRefused) }.with_persistence(&outcome);
            assert_eq!(expected.persistence, Some(class));
            let warning = PersistenceWarning::from_outcome(
                PersistenceWarningKey { epoch: owner.common.epoch, req: 1 }, PersistenceWarningSite::Final, &outcome);
            // Replay/serialization must preserve the same local evidence as the live warning.
            owner.common.persistence_warning = Some(serde_json::from_value(serde_json::to_value(warning).unwrap()).unwrap());
            owner.raise_incident(IncidentFlow::SignIn, expected);
            let id = owner.common.incident.as_ref().unwrap().id;
            step(&mut owner, SessionEvent::Command(Command::ResolveIncident {
                id, permission: plx_telemetry::telemetry::consent::Permission::Declined, revision: 1 }));
            assert!(owner.common.incident.as_ref().unwrap().context.is_none());
            let effects = step(&mut owner, SessionEvent::Command(Command::ReportIncident { id }));
            let rebuilt = effects.iter().find_map(|effect| match effect {
                SessionFx::Incident { lane: IncidentLane::OneOff, report: IncidentReport::Retained(context), .. } => Some(*context),
                SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
                | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
                | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
                | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
                | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
                | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
                | SessionFx::RefreshCache { .. } => None,
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
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        let effects = step(&mut owner, SessionEvent::Result(signed_in));
        let plan = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => Some(plan),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("SignedIn must begin its discovery commit");
        let credentials = plan.credentials.as_ref().expect("fresh sign-in commits credentials");
        assert!(!credentials.account_token.is_empty());
        assert_eq!(credentials.user.plex_tv_token.as_deref(),
            Some(credentials.account_token.as_str()));
    }

    #[test]
    fn uncertain_db8_reply_reaches_the_warning_and_incident_report() {
        use plx_plex::plex::session::{persistence, async_persistence::{CompletionOutcome, PersistenceCompletion}};
        use plx_platform::storage::wire::failure::{Detail, Stage};
        for reconcile in [false, true] {
            let mut owner = SessionMachine::from_init(discovering_after_authorization());
            let req = owner.common.next_req;
            let epoch = owner.common.epoch;
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
            assert_eq!(owner.common.incident.as_ref().unwrap().context.as_ref().unwrap().helper, Some(helper));
            let context = plx_telemetry::telemetry::incident::IncidentContext::new(
                plx_telemetry::telemetry::incident::IncidentKind::SaveFailed, None).with_persistence(&outcome);
            let body = plx_telemetry::telemetry::incident::event_body(&"a".repeat(32), "", None, context,
                plx_telemetry::telemetry::incident::ConsentKind::OneOff);
            assert_eq!(body["contexts"]["incident"]["persistence"], "commit_uncertain");
            assert_eq!(body["contexts"]["incident"]["helper"]["helper"]["stage"], "db8");
            assert_eq!(body["contexts"]["incident"]["helper"]["helper"]["code"], -3963);
            let offer = owner.common.incident.clone().unwrap();
            let retained = offer.context.unwrap();
            step(&mut owner, SessionEvent::Command(Command::ResolveIncident {
                id: offer.id, permission: plx_telemetry::telemetry::consent::Permission::Declined, revision: 1 }));
            assert!(owner.common.incident.as_ref().unwrap().context.is_none());
            let effects = step(&mut owner, SessionEvent::Command(Command::ReportIncident { id: offer.id }));
            let rebuilt = effects.iter().find_map(|effect| match effect {
                SessionFx::Incident { lane: IncidentLane::OneOff, report: IncidentReport::Retained(context), .. } => Some(*context),
                SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
                | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
                | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
                | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
                | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
                | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
                | SessionFx::RefreshCache { .. } => None,
            }).expect("one-off save report must use the visible warning evidence");
            assert_eq!(rebuilt.persistence, retained.persistence);
            assert_eq!(rebuilt.helper, retained.helper);
            assert_eq!(rebuilt, retained);
        }
    }

    #[test]
    fn helper_failure_warning_and_one_off_are_bound_to_the_completion() {
        use plx_plex::plex::session::async_persistence::{CompletionOutcome, Failure, PersistenceCompletion};
        use plx_platform::storage::wire::failure::{HelperFailure, Stage};
        for permission in [plx_telemetry::telemetry::consent::Permission::NotDetermined,
            plx_telemetry::telemetry::consent::Permission::Declined] {
            let mut owner = SessionMachine::from_init(discovering_after_authorization());
            let req = owner.common.next_req;
            let epoch = owner.common.epoch;
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
            assert!(owner.common.persistence_warning.is_none());
            step(&mut owner, SessionEvent::Persistence(completion));
            let warning = owner.publication().persistence_warning.unwrap();
            assert_eq!(warning.helper, Some(failure));
            let offer = owner.common.incident.clone().unwrap();
            assert_eq!(offer.context.unwrap().helper, Some(failure));
            let effects = step(&mut owner, SessionEvent::Command(Command::ResolveIncident {
                id: offer.id, permission, revision: 1 }));
            assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Incident { .. })));
            assert!(matches!(owner.common.incident.as_ref().unwrap().state,
                IncidentState::Offered { .. } | IncidentState::Dropped));
            let effects = step(&mut owner, SessionEvent::Command(Command::ReportIncident { id: offer.id }));
            assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Incident {
                lane: IncidentLane::OneOff, report: IncidentReport::Retained(context), ..
            } if context.helper == Some(failure) && context.candidate_errnos == errnos)));
            step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: warning.key }));
            assert!(owner.common.incident.is_none());
        }
    }

    // Field regression: an unrooted webOS 4.4.3 device unwritable on BOTH the Discovery and
    // Final layers used to demand two separate "Couldn't save your sign-in" acknowledgements.
    // One Continue must suffice for the whole authorization.
    #[test]
    fn one_continue_enters_when_discovery_and_final_storage_are_unavailable() {
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
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
            plx_platform::storage::StoreError::HelperUnavailable));
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion {
            req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery, outcome: failed,
        }));
        let discovery_warning = owner.publication().persistence_warning.unwrap();
        assert_eq!(discovery_warning.site, PersistenceWarningSite::Discovery);
        assert!(owner.common.held_handoff.is_none());
        step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning {
            key: discovery_warning.key,
        }));
        assert!(owner.needs_ready_commit());
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, plan, .. }
                if plan.purpose == PersistencePurpose::Final => Some(*req),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
            | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
            | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
            | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => None,
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
        assert!(owner.common.held_handoff.is_none(),
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
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
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
            plx_platform::storage::StoreError::HelperUnavailable));
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion {
            req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery, outcome: failed,
        }));
        let discovery_warning = owner.publication().persistence_warning.unwrap();
        step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning {
            key: discovery_warning.key,
        }));
        assert!(owner.common.persistence_warning_answered,
            "acknowledging must record that this authorization already answered");
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, plan, .. }
                if plan.purpose == PersistencePurpose::Final => Some(*req),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
            | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
            | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
            | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => None,
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
        assert!(owner.common.held_handoff.is_none(),
            "nothing may still be held once Ready has already been announced");
    }

    /// AUTH-04. Port of 0.6.6's
    /// `a_routine_save_of_a_reopened_session_does_not_spend_fresh_reauthentication_authority`
    /// (`plex/session.rs:8761`): a DISCOVERY failure must not spend the fresh authority the FINAL
    /// write still needs, and must not let `take_ready` run at all until acknowledged.
    #[test]
    fn a_discovery_failure_cannot_spend_or_authorize_the_final_fresh_save() {
        use plx_plex::plex::session::async_persistence::{CompletionOutcome, Failure, Operation,
            PersistOutcome, PersistenceCompletion};
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;

        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { uuid: "u-1".into(), title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        let discovery_reply = CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } };
        step(&mut owner, SessionEvent::Commit(discovery_reply));
        assert!(owner.state.projected_authorization(), "the discovery admission only PEEKED the authority");

        let discovery_failed = PersistenceCompletion { req, epoch, arrival: 1, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(discovery_failed));
        assert_eq!(owner.common.persistence_warning.map(|w| w.site), Some(PersistenceWarningSite::Discovery));
        assert!(owner.state.projected_authorization(),
            "MUTATION M3 TARGET: a discovery failure must not have spent the fresh authority");

        // Blocked: an unacknowledged warning must refuse TakeReady outright.
        assert!(!owner.needs_ready_commit(), "a live warning must suppress the ready-commit signal");
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })),
            "MUTATION M4 TARGET: take_ready must not begin a commit over an unacknowledged warning");

        let warning_key = owner.common.persistence_warning.unwrap().key;
        step(&mut owner, SessionEvent::Command(Command::AcknowledgePersistenceWarning { key: warning_key }));
        assert!(owner.common.persistence_warning.is_none());

        // Acknowledging a DISCOVERY warning (no held handoff behind it) re-admits TakeReady, and
        // the authority the discovery failure did NOT spend is still there for the real final write.
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let Some(SessionFx::Commit { plan, .. }) = effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("the acknowledged flow must be able to reach its final commit");
        };
        assert_eq!(plan.purpose, PersistencePurpose::Final);
        assert_eq!(plan.authority, plx_plex::plex::session::SaveAuthority::FreshReauthentication,
            "the authority a discovery failure never spent is still available to the final write");
        assert!(!owner.state.projected_authorization(), "this final commit consumes it now");

        let final_req = owner.common.next_req;
        assert!(owner.state.owns_work(final_req, &owner.common.pending[&final_req]));
        assert!(owner.commit_is_current(final_req, epoch, 0));
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
        step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        step(&mut owner, SessionEvent::Command(Command::SelectProfile { index: 0, pin: None }));
        assert!(owner.needs_ready_commit());
        assert!(!owner.state.fresh_authority());
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let Some(SessionFx::Commit { plan: routine_plan, .. }) =
            effects.iter().find(|fx| matches!(fx, SessionFx::Commit { .. })) else {
            panic!("the second Ready-op commit must still be reachable");
        };
        assert!(owner.commit_is_current(owner.common.next_req, owner.common.epoch, 0));
        assert_eq!(routine_plan.authority, plx_plex::plex::session::SaveAuthority::Routine,
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
    /// `if self.common.persistence_warning.is_some() { .. }` early-return this test guards at the
    /// top of `back()` — this test must then fail (the warning clears and, in the Final-site half,
    /// a `Ready` is emitted with no acknowledgement).
    #[test]
    fn back_at_root_does_not_bypass_an_unacknowledged_persistence_warning() {
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, Operation, PersistOutcome, PersistenceCompletion,
        };
        let reply = ReplyTo { instance: 0, correlation: 0 };

        // --- Discovery-site warning: the authority is only PEEKED so far (AUTH-04's guarantee),
        // and BACK must not spend it or clear the warning either.
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
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
        assert!(owner.common.persistence_warning.is_some(), "rig: a Discovery warning is showing");
        assert!(owner.state.projected_authorization(), "rig: the authority is still unspent");
        assert!(owner.common.committed_credentials.merge_into(&owner.common.persisted).can_go_local(),
            "rig: the discovery write's own admission already published the signed-in credentials \
             (`apply_commit_reply`'s `writes_credentials` branch runs on ADMISSION, not completion), \
             so BACK's `resumable()` check really is exercised here rather than short-circuited");

        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert!(owner.common.persistence_warning.is_some(),
            "MUTATION TARGET: BACK must not clear an unacknowledged warning");
        assert!(owner.state.projected_authorization(),
            "BACK must not spend the fresh authority the final write still needs");
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. } | SessionFx::Commit { .. })),
            "BACK over a live warning must not begin or announce any save");
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: false, .. })),
            "BACK is refused (root press only), not a resume");

        // --- Final-site warning with a held handoff: acknowledging is the ONLY door that may
        // release it (`acknowledge_persistence_warning` / `release_held_handoff`); BACK must not.
        let mut owner = SessionMachine::from_init(reopened_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
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
        let final_req = owner.common.next_req;
        let final_reply = CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } };
        step(&mut owner, SessionEvent::Commit(final_reply));
        assert!(owner.common.held_handoff.is_some(), "rig: the Ready handoff is held for this commit");
        let final_failed = PersistenceCompletion { req: final_req, epoch, arrival: 0, revision: 2,
            purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) };
        step(&mut owner, SessionEvent::Persistence(final_failed));
        assert!(owner.common.persistence_warning.is_some(), "rig: a Final warning is showing");

        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert!(owner.common.persistence_warning.is_some(),
            "MUTATION TARGET: BACK must not clear an unacknowledged Final-site warning");
        assert!(owner.common.held_handoff.is_some(),
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
    /// MUTATION TARGET: drop the `self.common.persistence_warning = None;` this test guards in
    /// `restart_login`'s `if discovery { .. }` arm.
    #[test]
    fn a_discovery_retry_clears_a_stale_warning_from_the_attempt_it_replaces() {
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
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
        assert!(owner.common.persistence_warning.is_some(), "rig: a Discovery warning is showing");
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
        let mut init = owner.snapshot_init();
        init.phase = Phase::Discovering;
        owner = SessionMachine::from_init(init);
        assert_eq!(super::super::retry_kind(owner.state.phase(), owner.state.projected_authorization()),
            super::super::RetryKind::Discovery, "rig: Retry must resolve to the DISCOVERY arm here");
        step(&mut owner, SessionEvent::Command(Command::Retry));
        assert!(owner.common.persistence_warning.is_none(),
            "MUTATION TARGET: a discovery restart must clear the stale warning it is replacing");
        assert!(owner.common.held_handoff.is_none());
    }

    /// A fresh write admitted over a READABLE record whose write then definitely failed leaves
    /// that record on disk, so the next fresh write must be fenced on it rather than on the
    /// identity that never landed (otherwise every retry this run is `StaleAuthority`).
    #[test]
    fn a_durable_superseded_write_advances_only_the_disk_fence() {
        use plx_plex::plex::session::async_persistence::{CompletionOutcome, Operation, PersistOutcome};
        let mut owner = SessionMachine::from_init(captured_session());
        let before = owner.common.disk_identity.clone();
        let mut after = before.clone();
        after.account_token = "superseded-durable-token".into();
        let trusted = Identity::of(&owner.common.persisted);
        let outcome = CompletionOutcome::Durable(Operation::Write {
            outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None });
        assert!(owner.observe_disk_write(&before, &after, outcome));
        assert!(owner.common.disk_identity == after);
        assert!(Identity::of(&owner.common.persisted) == trusted, "disk evidence is not login authority");
        assert!(!owner.observe_disk_write(&before, &after, outcome), "duplicate receipt is inert");
    }

    #[test]
    fn a_failed_fresh_write_restores_the_disk_identity_it_never_replaced() {
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Failure, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        owner.common.disk_identity = Identity { client_id: "synthetic-client".into(),
            account_token: "synthetic-revoked-account".into(), profile_uuid: "old".into() };
        let before = owner.common.disk_identity.clone();
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { title: "Only user".into(), ..Default::default() }],
        }, true);
        step(&mut owner, SessionEvent::Result(signed_in));
        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } }));
        assert!(owner.common.disk_identity != before, "rig: admission re-bases on the new identity");
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req, epoch, arrival: 1,
            revision: 1, purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Failed(Failure::Persistence(PersistOutcome::WriteFailed)) }));
        assert!(owner.common.disk_identity == before,
            "MUTATION TARGET: a definite failure must restore the identity still on disk");
        assert!(owner.common.persistence_warning.is_some());
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
        use plx_plex::plex::session::async_persistence::{
            CompletionOutcome, Operation, PersistOutcome, PersistenceCompletion,
        };
        let mut owner = SessionMachine::from_init(discovering_after_authorization());
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        // Manufacture a showing warning directly (no restart), keeping `admitted_persistence`
        // fenced so the very next completion below is accepted as this same operation's verdict.
        owner.common.persistence_warning = Some(PersistenceWarning {
            key: PersistenceWarningKey { epoch, req }, site: PersistenceWarningSite::Discovery, helper: None, candidate_errnos: [None; 8], persistence: None });
        owner.common.admitted_persistence = Some(AdmittedPersistence {
            req, epoch, arrival: 0, revision: 1, purpose: Some(PersistencePurpose::Discovery),
            fresh: true, site: PersistenceWarningSite::Discovery });
        let durable = PersistenceCompletion { req, epoch, arrival: 0, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) };
        step(&mut owner, SessionEvent::Persistence(durable));
        assert!(owner.common.persistence_warning.is_none(),
            "MUTATION TARGET: a later durable fresh write must supersede the earlier warning");
    }

    /// Third half of `discovery-warning-not-cleared-on-retry-or-fresh-success`: `StartSwitch` (and
    /// by the same code path `resume_stored`) issues a ROUTINE `activate_profile` commit, whose
    /// `apply_commit_reply` arm must not release a handoff still held behind a DIFFERENT,
    /// unacknowledged warning — that release belongs to `acknowledge_persistence_warning` alone.
    /// MUTATION TARGET: drop the `self.common.persistence_warning.is_none()` guard this test pins
    /// in `apply_commit_reply`'s non-fresh `activate_profile` arm (reverting to an unconditional
    /// `self.release_held_handoff(emit)`).
    #[test]
    fn start_switch_does_not_release_a_handoff_held_behind_an_unacknowledged_warning() {
        let mut owner = owner_with_held_final_warning();
        let held_before = owner.common.held_handoff;
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::Boot)));
        let Some((switch_req, switch_epoch)) = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, epoch, .. } => Some((*req, *epoch)),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }) else {
            panic!("rig: StartSwitch(Boot) must begin its own (registry-only) commit");
        };
        let switch_reply = CommitReply { req: switch_req, epoch: switch_epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly };
        let effects = step(&mut owner, SessionEvent::Commit(switch_reply));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Ready { .. })),
            "MUTATION TARGET: no Ready may be announced for the unrelated held handoff before \
             its own warning is acknowledged");
        assert_eq!(owner.common.held_handoff, held_before,
            "the handoff held behind the still-showing warning must be untouched");
        assert!(owner.common.persistence_warning.is_some(), "the warning itself is still unanswered");
    }

    #[test]
    fn disk_comparison_identity_is_not_a_worker_or_back_input() {
        let src = include_str!("owner.rs");
        let production = src.split("\n#[cfg(test)]\nmod tests").next().unwrap();
        // The former helper bodies now live in these canonical event arms and entry plans.
        // This is still an explicit source-scope check, not a transitive call-graph claim.
        for (name, marker, boundary) in [
            ("start_reserved_login/emit_work", "fn login(state:", "enum AttemptEvent {"),
            ("restart_login", "restarted = 'restart_login: {", "}; if !matches!(ev, SessionEvent::Erased"),
            ("start_switch", "SessionEvent::Command(Command::StartSwitch(picker))", "SessionEvent::Command(Command::SelectProfile { index, pin })"),
            ("select_profile", "SessionEvent::Command(Command::SelectProfile { index, pin })", "SessionEvent::Command(Command::BackAtRoot"),
            ("back", "SessionEvent::Command(Command::BackAtRoot", "SessionEvent::Command(Command::SignOut)"),
            ("request_endpoint", "SessionEvent::Command(Command::RequestEndpoint { sid })", "SessionEvent::Command(Command::AnswerPlaintext"),
            ("refresh_roster", "SessionEvent::Command(Command::RefreshRoster)", "SessionEvent::Command(Command::RequestEndpoint { sid })"),
            ("work_is_current", "pub fn work_is_current(", "\n    }"),
        ] {
            let start = production.find(marker).unwrap_or_else(|| panic!("missing owner scope {name}"));
            let rest = &production[start + marker.len()..];
            let end = rest.find(boundary).unwrap_or_else(|| panic!("missing end of owner scope {name}"));
            // A no-save registry commit may carry the exact comparison projection,
            // never use it to construct worker credentials or BACK authority.
            let body = rest[..end].replace("expected_disk: self.common.disk_identity.clone(),", "");
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
            sid: plx_plex::plex::ServerId::from_raw(sid) }));
        assert!(fx.iter().any(|f| matches!(f,
            SessionFx::Capture { request: SessionReadRequest::Endpoint { sid: s }, .. } if *s == sid)),
            "the first RequestEndpoint for an idle sid must be admitted");
        assert_eq!(owner.snapshot_init().pending.values()
            .filter(|p| p.key.op == SessionOp::Endpoint(sid)).count(), 1);

        // Every re-fire while that attempt is still outstanding — exactly what a `landed_fail`
        // during the same backoff wait produces — must be refused, not open a second probe.
        for _ in 0..3 {
            let fx = step(&mut owner, SessionEvent::Command(Command::RequestEndpoint {
                sid: plx_plex::plex::ServerId::from_raw(sid) }));
            assert!(fx.is_empty(), "a RequestEndpoint for a sid already in flight must be a no-op");
        }
        assert_eq!(owner.snapshot_init().pending.values()
            .filter(|p| p.key.op == SessionOp::Endpoint(sid)).count(), 1,
            "still exactly one outstanding attempt for this sid — one per backoff step, not per fire");
    }

    #[test]
    fn request_endpoint_admits_a_server_past_the_old_sixteen_slot_table() {
        let mut init = captured_session();
        init.persisted.account_token = "synthetic-account".into();
        let mut owner = SessionMachine::from_init(init);
        let sid: u16 = 40;
        let fx = step(&mut owner, SessionEvent::Command(Command::RequestEndpoint {
            sid: plx_plex::plex::ServerId::from_raw(sid) }));
        assert!(fx.iter().any(|f| matches!(f,
            SessionFx::Capture { request: SessionReadRequest::Endpoint { sid: s }, .. } if *s == sid)),
            "a server the registry issued past slot 15 must reach its endpoint probe");
        // The reserved UNSET id names no server, so it is still refused.
        let fx = step(&mut owner, SessionEvent::Command(Command::RequestEndpoint {
            sid: plx_plex::plex::ServerId::UNSET }));
        assert!(fx.is_empty());
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
            owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        }
        let make = |req, arrival| SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
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
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: owner.common.pending[&req].key,
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
        let epoch = owner.common.epoch;
        let req = owner.common.next_req;
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
        owner.common.next_req = u32::MAX;
        let before = owner.publication();
        let mut canon = Canon::new();
        owner.write(&mut canon);
        let hash = canon.finish();
        assert!(!owner.restart_login(true, &mut |_| panic!("exhaustion must not emit cancellation")));
        let mut canon = Canon::new();
        owner.write(&mut canon);
        assert_eq!(hash, canon.finish());
        assert!(Arc::ptr_eq(&before, &owner.publication()));
        assert_eq!(owner.common.pending.len(), 1);
    }

    #[test]
    fn qr_allocator_exhaustion_fails_and_retires_the_admitted_request() {
        let mut owner = SessionMachine::from_init(captured_session());
        assert!(owner.restart_login(true, &mut |_| {}));
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        owner.common.next_qr = u64::MAX;
        let code = qr_event(&owner, req, 1, super::super::LoginProgress::CodeReady {
            epoch, code: "synthetic-code".into(), qr_png: vec![1, 2, 3],
        }, false);
        let mut effects = Vec::new();
        let handled = owner.apply_qr_observation(&code, &mut |fx| effects.push(fx));
        assert_eq!(owner.state.phase(), Phase::Error, "exhaustion must settle the spinner as failure");
        assert!(handled);
        assert_eq!(owner.common.next_qr, u64::MAX);
        assert_eq!(owner.state.qr().generation, 0, "no generation is reused or published");
        assert!(owner.common.pending.is_empty());
        assert!(!owner.common.signin_notice.owed());
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
        let epoch = owner.common.epoch;
        let req = owner.common.next_req;
        let authorized = qr_event(&owner, req, 1, super::super::LoginProgress::Authorized {
            epoch, token: "synthetic-token".into(),
        }, false);
        assert!(owner.apply_qr_observation(&authorized, &mut |_| {}));
        assert!(owner.common.pending[&req].expected.matches(&owner.common.persisted));
        let failed = qr_event(&owner, req, 2, super::super::LoginProgress::Failed {
            epoch, message: "synthetic discovery failure".into(), incident: crate::auth::synthetic_incident(), plaintext: None, account: None
        }, true);
        assert!(owner.apply_qr_observation(&failed, &mut |_| {}));
        let retained = owner.publication();
        assert!(!owner.apply_qr_observation(&failed, &mut |_| panic!("duplicate terminal")));
        assert!(Arc::ptr_eq(&retained, &owner.publication()));
        let mut effects = Vec::new();
        assert!(owner.restart_login(false, &mut |fx| effects.push(fx)));
        assert_eq!(owner.state.phase(), Phase::Discovering);
        assert!(matches!(effects.last(), Some(SessionFx::Work {
            input: SessionWork::Rediscover { account_token, .. }, ..
        }) if account_token == "synthetic-token"));
    }

    /// **A QR sign-in over a stored session seats nobody from that session.** A sign-in that is not
    /// a sign-out (a stored session that could not be activated, *Use another account*, the dev
    /// `/tmp/plxnative-login` trigger) starts from the stored record, and `Authorized` used to
    /// replace only the account token: the previous account's `user` rode through `SignedIn` (which
    /// even stamped the new account's plex.tv token onto it) into the published profile, so a
    /// single-user account that signed in next opened with the previous account's name on the
    /// profile chip, and the previous account's offline profile cache was saved beside it.
    #[test]
    fn a_sign_in_over_another_accounts_session_publishes_the_new_accounts_name() {
        use plx_plex::plex::session::{HomeUserRef, ProfileCreds};
        use plx_plex::plex::session::async_persistence::{CompletionOutcome, Operation, PersistOutcome,
            PersistenceCompletion};
        let mut init = local_session();
        init.persisted.account_token = "synthetic-account-a".into();
        init.persisted.user = UserRef { id: 7, uuid: "previous-owner".into(), title: "Previous Name".into(),
            thumb: "previous-thumb".into(), token: "synthetic-token".into(),
            plex_tv_token: Some("synthetic-account-a".into()), ..Default::default() };
        init.persisted.home_users = vec![HomeUserRef { id: 7, uuid: "previous-owner".into(),
            title: "Previous Name".into(), admin: true, ..Default::default() }];
        init.persisted.profiles = vec![ProfileCreds { uuid: "previous-owner".into(),
            user: init.persisted.user.clone(), ..Default::default() }];
        init.committed_credentials = CredentialPatch::of(&init.persisted);
        let mut owner = SessionMachine::from_init(init);

        assert!(owner.restart_login(true, &mut |_| {}));
        let epoch = owner.common.epoch;
        let req = owner.common.next_req;
        let authorized = qr_event(&owner, req, 1, super::super::LoginProgress::Authorized {
            epoch, token: "synthetic-account-b".into(),
        }, false);
        assert!(owner.apply_qr_observation(&authorized, &mut |_| {}));
        assert!(owner.common.pending[&req].expected.matches(&owner.common.persisted));
        let signed_in = qr_event(&owner, req, 2, super::super::LoginProgress::SignedIn {
            epoch, server: local_server(), sources: Vec::new(),
            users: vec![UserTile { id: 1, uuid: "demo-user".into(), title: "Demo".into(), admin: true,
                ..Default::default() }],
        }, true);
        let effects = step(&mut owner, SessionEvent::Result(signed_in));
        let saved = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => plan.credentials.clone(),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the sign-in saves its credentials");
        assert_ne!(saved.user.uuid, "previous-owner", "the discovery save must not keep the previous profile");
        assert_ne!(saved.user.title, "Previous Name");
        assert!(saved.profiles.is_empty(), "the previous account's offline profile cache is not saved");

        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 2,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } }));
        step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req, epoch, arrival: 2, revision: 1,
            purpose: PersistencePurpose::Discovery,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) }));
        step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_req = owner.common.next_req;
        step(&mut owner, SessionEvent::Commit(CommitReply { req: final_req, epoch, arrival: 0,
            admission: CommitAdmission::Admitted { revision: 2, purpose: PersistencePurpose::Final } }));
        let effects = step(&mut owner, SessionEvent::Persistence(PersistenceCompletion { req: final_req, epoch,
            arrival: 0, revision: 2, purpose: PersistencePurpose::Final,
            outcome: CompletionOutcome::Durable(Operation::Write {
                outcome: PersistOutcome::PersistedPlaintext, verified: true, protection: None }) }));
        let published = effects.iter().find_map(|fx| match fx {
            SessionFx::PublishProfile(p) => Some(p.profile.clone()),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the sign-in publishes who is watching");
        let account = owner.common.persisted.account(published.as_ref());
        assert_eq!(account.name.as_deref(), Some("Demo"), "the chip names the account that signed in");
        assert!(published.as_ref().is_none_or(|u| u.uuid != "previous-owner" && u.thumb.is_empty()));
        assert!(owner.common.persisted.profiles.is_empty());
    }

    /// **The same person signing in again keeps the offline profile cache.** A fresh QR sign-in
    /// mints a new account token, so the token cannot say whose cache it is: `Authorized` resets it
    /// (identity unknown then) and the `SignedIn` roster decides. A roster naming the same owner as
    /// the stored record restores that record's `profiles`; a roster naming another owner, or none
    /// at all, leaves it empty (`a_sign_in_over_another_accounts_session_publishes_the_new_accounts_name`).
    #[test]
    fn a_sign_in_over_the_same_accounts_session_keeps_its_offline_profile_cache() {
        use plx_plex::plex::session::{HomeUserRef, ProfileCreds};
        let saved_profiles = |users: Vec<UserTile>| {
            let mut init = local_session();
            init.persisted.account_token = "synthetic-token-before".into();
            init.persisted.user = UserRef { id: 7, uuid: "stored-owner".into(), title: "Stored Name".into(),
                token: "synthetic-token".into(), ..Default::default() };
            init.persisted.home_users = vec![HomeUserRef { id: 7, uuid: "stored-owner".into(),
                title: "Stored Name".into(), admin: true, ..Default::default() }];
            init.persisted.profiles = vec![ProfileCreds { uuid: "stored-owner".into(),
                user: init.persisted.user.clone(), ..Default::default() }];
            init.committed_credentials = CredentialPatch::of(&init.persisted);
            let mut owner = SessionMachine::from_init(init);
            assert!(owner.restart_login(true, &mut |_| {}));
            let epoch = owner.common.epoch;
            let req = owner.common.next_req;
            let authorized = qr_event(&owner, req, 1, super::super::LoginProgress::Authorized {
                epoch, token: "synthetic-token-after".into(),
            }, false);
            assert!(owner.apply_qr_observation(&authorized, &mut |_| {}));
            assert!(owner.common.persisted.profiles.is_empty(), "identity is unknown at Authorized");
            let signed_in = qr_event(&owner, req, 2, super::super::LoginProgress::SignedIn {
                epoch, server: local_server(), sources: Vec::new(), users,
            }, true);
            let effects = step(&mut owner, SessionEvent::Result(signed_in));
            effects.iter().find_map(|fx| match fx {
                SessionFx::Commit { plan, .. } => plan.credentials.clone(),
                SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
                | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
                | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
                | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
                | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
                | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
                | SessionFx::RefreshCache { .. } => None,
            }).expect("the sign-in saves its credentials")
        };
        let tile = |id: i64| UserTile { id, uuid: format!("owner-{id}"), title: "Synthetic".into(),
            admin: true, ..Default::default() };

        let same = saved_profiles(vec![tile(7)]);
        assert_ne!(same.account_token, "synthetic-token-before", "control: a new token was minted");
        assert_eq!(same.profiles.len(), 1, "the same owner keeps the offline profile cache");
        assert_eq!(same.profiles[0].uuid, "stored-owner");
        assert!(saved_profiles(vec![tile(8)]).profiles.is_empty(), "another owner gets none");
        assert!(saved_profiles(Vec::new()).profiles.is_empty(), "an unnamed owner gets none");
    }

    /// **The consent answer**: the read-out's verdict reaches the publication; *Connect* records
    /// Allowed and rediscovers with the same account token at once; *Not now* records Declined and
    /// re-words the read-out to say how to allow it. An answer about another server, or with no
    /// failure on screen, is refused.
    #[test]
    fn answering_the_plaintext_question_records_the_choice_and_allow_rediscovers() {
        use plx_plex::plex::probe::PlaintextEligibility;
        use plx_plex::plex::session::PlaintextChoice;
        for allow in [true, false] {
            let mut owner = SessionMachine::from_init(captured_session());
            assert!(owner.restart_login(true, &mut |_| {}));
            let epoch = owner.common.epoch;
            let req = owner.common.next_req;
            let authorized = qr_event(&owner, req, 1, super::super::LoginProgress::Authorized {
                epoch, token: "synthetic-token".into(),
            }, false);
            assert!(owner.apply_qr_observation(&authorized, &mut |_| {}));
            let verdict = super::super::PlaintextVerdict {
                machine_id: "lan-machine".into(), name: "Home".into(), shared_by: String::new(),
                eligibility: PlaintextEligibility::Eligible, choice: PlaintextChoice::Undecided,
            };
            let failed = qr_event(&owner, req, 2, super::super::LoginProgress::Failed {
                epoch, message: super::super::insecure_only_copy(Some(&verdict)).into_owned(),
                incident: crate::auth::synthetic_incident(), plaintext: Some(verdict), account: None,
            }, true);
            assert!(owner.apply_qr_observation(&failed, &mut |_| {}));
            assert_eq!(owner.read().0.plaintext.as_ref().map(|v| v.machine_id.as_str()), Some("lan-machine"));

            let want = if allow { PlaintextChoice::Allowed } else { PlaintextChoice::Declined };
            assert!(!owner.answer_plaintext("another-machine", want, None, &mut |_| panic!("not this server")));
            let mut effects = Vec::new();
            assert!(owner.answer_plaintext("lan-machine", want, None, &mut |fx| effects.push(fx)));
            let key = plx_plex::plex::grant::account_key("synthetic-token");
            assert!(matches!(effects.first(), Some(SessionFx::PlaintextAnswer { machine_id, choice, account })
                if machine_id == "lan-machine" && *choice == want && *account == key), "allow={allow}");
            if allow {
                assert_eq!(owner.state.phase(), Phase::Discovering);
                assert!(owner.read().0.plaintext.is_none(), "the question is answered");
                assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Work {
                    input: SessionWork::Rediscover { account_token, .. }, ..
                } if account_token == "synthetic-token")));
            } else {
                assert_eq!(owner.state.phase(), Phase::Error);
                let shown = owner.read().0;
                assert_eq!(shown.plaintext.as_ref().map(|v| v.choice), Some(PlaintextChoice::Declined));
                assert!(shown.error.contains("Select Try again to be asked again."), "{}", shown.error);
                // Settings is out of reach before sign-in, so *Try again* is how the person is
                // asked again: the answer is withdrawn (recorded Undecided) BEFORE the retry's
                // work captures the answers, and the retry runs.
                let retried = step(&mut owner, SessionEvent::Command(Command::Retry));
                let withdrawn = retried.iter().position(|fx| matches!(fx, SessionFx::PlaintextAnswer {
                    machine_id, choice: PlaintextChoice::Undecided, account } if machine_id == "lan-machine" && *account == key));
                let work = retried.iter().position(|fx| matches!(fx, SessionFx::Work { .. }));
                assert!(withdrawn.is_some() && withdrawn < work, "the declined answer is withdrawn before the retry: {:?}",
                    retried.iter().map(|fx| std::mem::discriminant(fx)).collect::<Vec<_>>());
                assert_eq!(owner.state.phase(), Phase::Discovering);
            }
        }
    }

    /// **A signed-in person has a consent path too.** Outside the sign-in read-out the answer is
    /// recorded under the signed-in account's key, and an *Allowed* re-finds that server's
    /// endpoint (or, with no slot, the roster) so a fresh probe can mint; a *Revoked* or *Declined*
    /// starts nothing. With no account signed in nothing is recorded at all.
    #[test]
    fn a_signed_in_answer_is_recorded_for_the_account_and_re_finds_the_server() {
        use plx_plex::plex::session::PlaintextChoice;
        let key = plx_plex::plex::grant::account_key("synthetic-account");
        let sid = plx_plex::plex::ServerId::from_raw(3);
        let mut owner = SessionMachine::from_init(local_session());
        let fx = step(&mut owner, SessionEvent::Command(Command::AnswerPlaintext {
            machine_id: "lan-machine".into(), choice: PlaintextChoice::Allowed, sid: Some(sid) }));
        assert!(matches!(fx.first(), Some(SessionFx::PlaintextAnswer { machine_id, choice, account })
            if machine_id == "lan-machine" && *choice == PlaintextChoice::Allowed && *account == key));
        assert!(fx.iter().any(|f| matches!(f,
            SessionFx::Capture { request: SessionReadRequest::Endpoint { sid: 3 }, .. })));

        let mut owner = SessionMachine::from_init(local_session());
        let fx = step(&mut owner, SessionEvent::Command(Command::AnswerPlaintext {
            machine_id: "lan-machine".into(), choice: PlaintextChoice::Allowed, sid: None }));
        assert!(fx.iter().any(|f| matches!(f, SessionFx::Work { input: SessionWork::ServerRoster { .. }, .. })));

        let mut owner = SessionMachine::from_init(local_session());
        let fx = step(&mut owner, SessionEvent::Command(Command::AnswerPlaintext {
            machine_id: "lan-machine".into(), choice: PlaintextChoice::Revoked, sid: Some(sid) }));
        assert_eq!(fx.len(), 1, "a revocation records and starts nothing");

        let mut owner = SessionMachine::from_init(captured_session());
        let fx = step(&mut owner, SessionEvent::Command(Command::AnswerPlaintext {
            machine_id: "lan-machine".into(), choice: PlaintextChoice::Allowed, sid: Some(sid) }));
        assert!(fx.is_empty(), "no account, nothing to bind the answer to");
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
        assert_eq!(b.common.pending[&1].key.epoch, u32::MAX as u64 + 7);
        a.common.next_req = u32::MAX;
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

    fn admit_profile_selection(owner: &mut SessionMachine) -> u32 {
        let effects = step(owner, SessionEvent::Command(Command::SelectProfile { index: 0, pin: None }));
        let req = owner.common.next_req;
        let epoch = owner.common.epoch;
        assert!(matches!(effects.last(), Some(SessionFx::Capture {
            req: captured, epoch: captured_epoch, request: SessionReadRequest::ProfilePolicy,
        }) if *captured == req && *captured_epoch == epoch));
        assert!(owner.state.owns_work(req, &owner.common.pending[&req]));
        assert!(owner.read_is_current(req, epoch, SessionReadRequest::ProfilePolicy));
        let effects = step(owner, SessionEvent::Read(SessionReadReply {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            epoch, value: SessionReadValue::ProfilePolicy { recently_unreachable: false },
        }));
        assert!(matches!(effects.as_slice(), [SessionFx::Work {
            req: launched, input: SessionWork::ProfileSwitch { .. }, ..
        }] if *launched == req));
        let key = owner.common.pending[&req].key;
        assert!(owner.work_is_current(req, key, AdmissionId(req)));
        step(owner, SessionEvent::Admission(AdmissionReply {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key, correlation: AdmissionId(req), accepted: true,
        }));
        assert!(owner.common.pending[&req].admission == AdmissionState::Accepted(AdmissionId(req)));
        assert!(owner.state.owns_work(req, &owner.common.pending[&req]));
        req
    }

    #[test]
    fn queued_running_terminal_is_revalidated_after_ready_ack_seats_new_profile() {
        let mut init = captured_session();
        init.phase = Phase::Profiles;
        init.users = vec![UserTile { uuid: "new-profile".into(), ..Default::default() }];
        init.persisted.account_token = "synthetic-account".into();
        init.persisted.user.uuid = "old-profile".into();
        let mut owner = SessionMachine::from_init(init);
        let req = admit_profile_selection(&mut owner);
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let make = |arrival, terminal, outcome| SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
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
        assert_eq!(owner.common.inbox.len(), 1);
        let acked = step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
            admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Final } }));
        assert_eq!(owner.state.phase(), Phase::Ready);
        assert_eq!(owner.common.persisted.user.uuid, "new-profile");
        assert!(owner.common.pending[&req].phase == StreamPhase::ProfileSeated);
        assert!(owner.state.owns_work(req, &owner.common.pending[&req]));
        assert!(!owner.accepts(&terminal), "same body is invalid at the changed FIFO head");
        let seated = owner.publication();
        let effects = step(&mut owner, SessionEvent::Pump);
        assert!(owner.common.pending.is_empty());
        assert!(owner.common.pending_commit.is_none());
        assert_eq!(owner.state.phase(), Phase::Ready);
        assert_eq!(owner.common.persisted.user.uuid, "new-profile");
        assert!(!owner.state.picker_data().pin_denied);
        assert!(owner.state.picker_data().error.is_empty());
        assert!(Arc::ptr_eq(&seated, &owner.publication()));
        assert!(effects.iter().all(|fx| matches!(fx, SessionFx::Retire { .. } | SessionFx::Acknowledge(_))),
            "rejected terminal must not emit another commit/profile publication/Ready");
        let receipts: Vec<_> = acked.iter().chain(&effects).filter_map(|fx| match fx {
            SessionFx::Acknowledge(receipts) => Some(receipts.as_slice()),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).flatten().copied().collect();
        assert!(receipts == [Receipt::of(&ready), Receipt::of(&terminal)]);
    }

    #[test]
    fn rejected_terminal_uses_existing_login_and_profile_drop_policy() {
        for (op, seated) in [(SessionOp::Login, false), (SessionOp::ProfileSwitch, false),
            (SessionOp::ProfileSwitch, true)] {
            let mut init = captured_session();
            init.persisted.account_token = "synthetic-account".into();
            init.persisted.user.uuid = "old-profile".into();
            if op == SessionOp::ProfileSwitch {
                init.phase = Phase::Profiles;
                init.users = vec![UserTile { uuid: "new-profile".into(), ..Default::default() }];
            }
            let mut owner = SessionMachine::from_init(init);
            let req = if op == SessionOp::Login {
                step(&mut owner, SessionEvent::Command(Command::StartLogin));
                let req = owner.common.next_req;
                let key = owner.common.pending[&req].key;
                assert!(owner.work_is_current(req, key, AdmissionId(req)));
                step(&mut owner, SessionEvent::Admission(AdmissionReply {
                    addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
                    key, correlation: AdmissionId(req), accepted: true,
                }));
                let code = qr_event(&owner, req, 1, super::super::LoginProgress::CodeReady {
                    epoch: owner.common.epoch, code: "ABCD".into(), qr_png: vec![1],
                }, false);
                assert!(owner.accepts(&code));
                step(&mut owner, SessionEvent::Result(code));
                assert_eq!(owner.state.phase(), Phase::Waiting);
                req
            } else { admit_profile_selection(&mut owner) };
            if seated {
                let epoch = owner.common.epoch;
                let ready = SessionEnvelope {
                    addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
                    key: owner.common.pending[&req].key, admission: AdmissionId(req),
                    arrival: 1, terminal: false, lifecycle: None,
                    outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ProfileSwitch(
                        super::super::ProfileSwitchProgress {
                            epoch, expected: super::super::SessionIdentity::of(&owner.common.persisted),
                            outcome: super::super::ProfileSwitchOutcomeProgress::Ready {
                                delta: super::super::ProfileDelta {
                                    user: UserRef { uuid: "new-profile".into(), token: "new-token".into(), ..Default::default() },
                                    server: local_server(), sources: Vec::new(), cache: None,
                                }, probes: Vec::new(),
                            },
                        }))),
                };
                assert!(owner.accepts(&ready));
                let effects = step(&mut owner, SessionEvent::Result(ready));
                assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })));
                assert!(owner.commit_is_current(req, epoch, 1));
                step(&mut owner, SessionEvent::Commit(CommitReply {
                    req, epoch, arrival: 1, admission: CommitAdmission::RegistryOnly,
                }));
                assert_eq!(owner.state.phase(), Phase::Ready);
                assert!(owner.common.pending[&req].phase == StreamPhase::ProfileSeated);
            }
            assert!(owner.state.owns_work(req, &owner.common.pending[&req]));
            assert!(owner.common.pending[&req].admission == AdmissionState::Accepted(AdmissionId(req)));
            // Wrong inner epoch; the outer header still identifies this admitted terminal.
            let record = qr_event(&owner, req, 2, super::super::LoginProgress::Failed {
                epoch: owner.common.epoch + 1, message: "rejected payload text".into(), incident: crate::auth::synthetic_incident(), plaintext: None, account: None
            }, true);
            assert!(owner.accepts_header(&record));
            assert!(!owner.accepts(&record));
            step(&mut owner, SessionEvent::Result(record));
            assert!(owner.common.pending.is_empty());
            assert!(owner.common.pending_commit.is_none());
            assert_eq!(owner.state.phase(), if op == SessionOp::Login { Phase::Error }
                else if seated { Phase::Ready } else { Phase::Profiles });
            assert_ne!(owner.state.picker_data().error, "rejected payload text");
        }
    }

    #[test]
    fn rejected_terminal_cannot_bypass_processing_watermark_or_capture() {
        for captured in [false, true] {
            let mut owner = SessionMachine::from_init(captured_session());
            let req = owner.allocate(SessionOp::Login, None).unwrap();
            let pending = owner.common.pending.get_mut(&req).unwrap();
            pending.admission = AdmissionState::Accepted(AdmissionId(req));
            if captured { pending.capture = Some(CaptureIntent::Login); }
            else { pending.last_arrival = Some(2); }
            let record = qr_event(&owner, req, 1, super::super::LoginProgress::Failed {
                epoch: owner.common.epoch + 1, message: "rejected payload text".into(), incident: crate::auth::synthetic_incident(), plaintext: None, account: None
            }, true);
            let before = owner.snapshot_init().hash();
            step(&mut owner, SessionEvent::Result(record));
            assert_eq!(owner.snapshot_init().hash(), before);
            assert!(owner.common.pending.contains_key(&req));
        }
    }

    #[test]
    fn envelope_validation_keeps_full_epoch_and_exact_destination() {
        use plx_machine::machine::RequestId;
        let mut init = captured_session();
        init.epoch = 0x1_0000_0000;
        let mut owner = SessionMachine::from_init(init);
        step(&mut owner, SessionEvent::Command(Command::StartLogin));
        let req = owner.common.next_req;
        let key = owner.common.pending[&req].key;
        assert_eq!(owner.common.epoch, 0x1_0000_0001);
        assert!(owner.work_is_current(req, key, AdmissionId(req)));
        step(&mut owner, SessionEvent::Admission(AdmissionReply {
            addr: Addr { to: MachineId::Session, req: RequestId(req) },
            key, correlation: AdmissionId(req), accepted: true,
        }));
        assert!(owner.state.owns_work(req, &owner.common.pending[&req]));
        assert!(owner.common.pending[&req].admission == AdmissionState::Accepted(AdmissionId(req)));
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
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("insecure-mach", "10.0.0.9", 32400, "tok", "cid");
        let client = plx_plex::plex::client_for(sid).unwrap();
        let lifecycle = super::super::ClientLifecycle::capture(client);

        let mut owner = SessionMachine::from_init(captured_session());
        let req = owner.allocate(SessionOp::Endpoint(sid.raw()), Some(lifecycle.logical(sid.raw()))).unwrap();
        owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let probe = super::super::settled_probe_for_test(
            "insecure-mach", plx_plex::plex::probe::Outcome::InsecureOnly, Some(plx_plex::plex::probe::Location::Local), Some("10.0.0.9".into()));
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
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
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("a None-fresh endpoint reply must still commit its probe");
        assert_eq!(plan.registry.len(), 1, "nothing to INSTALL — the probe is the whole plan");
        assert!(
            matches!(&plan.registry[0], RegistryPlan::Probe(p)
                if p.machine_id == "insecure-mach" && p.outcome == plx_plex::plex::probe::Outcome::InsecureOnly),
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
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let sid = plx_plex::plex::register_for_test("ours", "10.0.0.9", 32400, "profile-token", "cid");
        let client = plx_plex::plex::client_for(sid).unwrap();
        let lifecycle = super::super::ClientLifecycle::capture(client);

        // The route facts the endpoint reply carries are IDENTICAL to what is already stored, so
        // `apply_refreshed_endpoint` reports `changed == false`.
        let current = plx_plex::plex::session::SourceRef {
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
        persisted.profiles.push(plx_plex::plex::session::ProfileCreds {
            uuid: "u1".into(),
            user: UserRef::default(),
            server: plx_plex::plex::session::ServerRef::default(),
            sources: Vec::new(),
            pin: None,
            extensions: Default::default(),
        });

        let mut owner = SessionMachine::from_init(SessionInit::captured(persisted));
        let req = owner.allocate(SessionOp::Endpoint(sid.raw()), Some(lifecycle.logical(sid.raw()))).unwrap();
        owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let probe = super::super::settled_probe_for_test(
            "ours", plx_plex::plex::probe::Outcome::Reachable, Some(plx_plex::plex::probe::Location::Local), Some("10.0.0.9".into()));
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
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
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
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
        let _g = plx_base::testlock::serial();
        let mut owner = SessionMachine::from_init(captured_session());
        let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let settled = vec![
            super::super::settled_probe_for_test("a", plx_plex::plex::probe::Outcome::InsecureOnly,
                Some(plx_plex::plex::probe::Location::Local), Some("10.0.0.1".into())),
            super::super::settled_probe_for_test("b", plx_plex::plex::probe::Outcome::Unreachable, None, None),
        ];
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ServerRoster },
            admission: AdmissionId(req), arrival: 1, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
                super::super::ServerRosterProgress { epoch, expected,
                    outcome: super::super::ServerRosterOutcome::NoReachable { settled: settled.clone() } }))),
        };
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        let plan = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { plan, .. } => Some(plan),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("NoReachable with a non-empty settled list must still commit");
        assert_eq!(plan.registry.len(), settled.len(), "one RegistryPlan::Probe per settled probe, no roster Install");
        assert!(plan.registry.iter().all(|p| matches!(p, RegistryPlan::Probe(_))),
            "NoReachable must never write RegistryPlan::Install — the roster itself is unchanged");
        assert!(plan.credentials.is_none(), "a registry-only commit writes no credentials");
    }

    fn account_refresh_source() -> plx_plex::plex::session::SourceRef {
        plx_plex::plex::session::SourceRef {
            machine_id: "account-machine".into(), name: "Account server".into(), owned: true,
            token: "account-server-token".into(), address: "10.0.0.9".into(), port: 32400,
            origin_url: "https://10-0-0-9.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default()
        }
    }

    fn land_account_roster_refresh(owner: &mut SessionMachine) -> (CommitPlan, CommitPlan) {
        let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        owner.common.pending.get_mut(&req).unwrap().admission =
            AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let fresh = account_refresh_source();
        let activate = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ServerRoster },
            admission: AdmissionId(req), arrival: 1, terminal: false, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Registry(
                super::super::RegistryProgress::Activate {
                    epoch, expected: Some(expected.clone()),
                    candidate: super::super::CandidateActivation {
                        machine_id: fresh.machine_id.clone(), token: fresh.token.clone(),
                        name: fresh.name.clone(), credit: String::new(), owned: true,
                        home: false, owner_id: 0, origin: fresh.origin().unwrap(),
                        address: fresh.address.clone(), location: plx_plex::plex::probe::Location::Local,
                        ipv6: false,
                    },
                }))),
        };
        let activate_effects = step(owner, SessionEvent::Result(activate));
        let activate_plan = activate_effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { plan, .. } => Some(plan.clone()),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the progress observation reaches the commit boundary");
        step(owner, SessionEvent::Commit(CommitReply {
            req, epoch, arrival: 1, admission: CommitAdmission::RegistryOnly,
        }));

        let probe = super::super::settled_probe_for_test(&fresh.machine_id,
            plx_plex::plex::probe::Outcome::Reachable,
            Some(plx_plex::plex::probe::Location::Local), Some(fresh.address.clone()));
        let resource = plx_plex::plex::account::Resource {
            name: fresh.name.clone(), client_identifier: fresh.machine_id.clone(),
            provides: "server".into(), owned: true, access_token: fresh.token.clone(),
            ..Default::default()
        };
        let reconcile = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ServerRoster },
            admission: AdmissionId(req), arrival: 2, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
                super::super::ServerRosterProgress { epoch, expected,
                    outcome: super::super::ServerRosterOutcome::Reconcile {
                        resources: vec![resource], found: vec![fresh.clone()],
                        admitted_machine_id: fresh.machine_id, household: Vec::new(),
                        settled: vec![probe],
                    },
                }))),
        };
        let reconcile_effects = step(owner, SessionEvent::Result(reconcile));
        let reconcile_plan = reconcile_effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { plan, .. } => Some(plan.clone()),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the terminal observation reaches the commit boundary");
        step(owner, SessionEvent::Commit(CommitReply {
            req, epoch, arrival: 2, admission: CommitAdmission::RegistryOnly,
        }));
        (activate_plan, reconcile_plan)
    }

    fn assert_non_admin_roster_refresh_is_probe_only(mut owner: SessionMachine) {
        let before = CredentialPatch::of(&owner.common.persisted);
        let (activate, reconcile) = land_account_roster_refresh(&mut owner);
        assert!(activate.credentials.is_none());
        assert!(activate.registry.is_empty(),
            "an account-token activation must not add a grant for the seated profile");
        assert!(reconcile.credentials.is_none(),
            "the account holder's roster must not replace seated-profile credentials");
        assert_eq!(reconcile.registry.len(), 1);
        assert!(matches!(&reconcile.registry[0], RegistryPlan::Probe(probe)
            if probe.machine_id == "account-machine"
                && probe.outcome == plx_plex::plex::probe::Outcome::Reachable));
        assert_eq!(owner.common.persisted.server.machine_id, before.server.machine_id);
        assert_eq!(owner.common.persisted.server.token, before.server.token);
        assert_eq!(owner.common.persisted.user.token, before.user.token);
        assert_eq!(owner.common.persisted.sources[0].token, before.sources[0].token);
        assert_eq!(owner.common.persisted.profiles[0].server.token, before.profiles[0].server.token);
        assert_eq!(owner.common.persisted.profiles[0].user.token, before.profiles[0].user.token);
        assert_eq!(owner.common.persisted.profiles[0].sources[0].token,
            before.profiles[0].sources[0].token);
    }

    #[test]
    fn seated_managed_profile_roster_refresh_keeps_profile_credentials_and_publishes_probes() {
        let _g = plx_base::testlock::serial();
        assert_non_admin_roster_refresh_is_probe_only(roster_refresh_fixture("u-managed", vec![
            plx_plex::plex::session::HomeUserRef { id: 2, uuid: "u-managed".into(),
                title: "Managed".into(), admin: false, ..Default::default() },
        ]));
    }

    #[test]
    fn seated_home_member_roster_refresh_keeps_profile_credentials_and_publishes_probes() {
        let _g = plx_base::testlock::serial();
        assert_non_admin_roster_refresh_is_probe_only(roster_refresh_fixture("u-member", vec![
            plx_plex::plex::session::HomeUserRef { id: 3, uuid: "u-member".into(),
                title: "Home member".into(), protected: true, admin: false, ..Default::default() },
        ]));
    }

    #[test]
    fn same_user_take_ready_keeps_an_unavailable_secondary_live_and_cached() {
        let _g = plx_base::testlock::serial();
        let primary = plx_plex::plex::session::SourceRef {
            machine_id: "a".into(), name: "Primary A".into(), owned: true,
            token: "kid-a-token".into(), address: "10.0.0.8".into(), port: 32400,
            origin_url: "https://10-0-0-8.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default()
        };
        let secondary = plx_plex::plex::session::SourceRef {
            machine_id: "b".into(), name: "Secondary B".into(),
            token: "kid-b-token".into(), address: "10.0.0.9".into(), port: 32400,
            origin_url: "https://10-0-0-9.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Remote), ..Default::default()
        };
        let user = UserRef { uuid: "u-kid".into(), title: "Kid".into(),
            token: primary.token.clone(), ..Default::default() };
        let mut persisted = PersistedSession {
            client_id: "synthetic-client".into(), account_token: "account-token".into(),
            server: super::super::server_ref(&primary), user: UserRef { uuid: "u-admin".into(),
                token: "admin-token".into(), ..Default::default() },
            home_users: vec![plx_plex::plex::session::HomeUserRef { uuid: "u-kid".into(),
                title: "Kid".into(), admin: false, ..Default::default() }],
            sources: vec![primary.clone()], ..Default::default()
        };
        persisted.remember_profile(plx_plex::plex::session::ProfileCreds {
            uuid: user.uuid.clone(), user: user.clone(), server: super::super::server_ref(&primary),
            sources: vec![primary.clone(), secondary.clone()], pin: None,
            extensions: Default::default(),
        });
        let mut init = SessionInit::captured(persisted);
        init.phase = Phase::Profiles;
        init.users = vec![UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() }];
        let mut owner = SessionMachine::from_init(init);
        let req = admit_profile_selection(&mut owner);
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let mut tokenless_secondary = secondary.clone();
        tokenless_secondary.token.clear();
        let ready = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ProfileSwitch }, admission: AdmissionId(req),
            arrival: 1, terminal: false, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ProfileSwitch(
                super::super::ProfileSwitchProgress { epoch, expected,
                    outcome: super::super::ProfileSwitchOutcomeProgress::Ready {
                        delta: super::super::ProfileDelta {
                            server: super::super::server_ref(&primary),
                            sources: vec![primary.clone(), tokenless_secondary], user: user.clone(),
                            cache: Some(plx_plex::plex::session::ProfileCreds {
                                uuid: user.uuid.clone(), user: user.clone(),
                                server: super::super::server_ref(&primary),
                                sources: vec![primary.clone(), secondary.clone()], pin: None,
                                extensions: Default::default(),
                            }),
                        },
                        probes: Vec::new(),
                    },
                }))),
        };
        let effects = step(&mut owner, SessionEvent::Result(ready));
        let reply = effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { req, epoch, arrival, .. } => Some(CommitReply {
                req: *req, epoch: *epoch, arrival: *arrival, admission: CommitAdmission::RegistryOnly,
            }),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the profile switch must commit");
        step(&mut owner, SessionEvent::Commit(reply));

        let next_identity = super::super::SessionIdentity::of(&owner.common.persisted);
        let resources = vec![
            plx_plex::plex::account::Resource { name: primary.name.clone(),
                client_identifier: primary.machine_id.clone(), provides: "server".into(),
                owned: true, access_token: primary.token.clone(), ..Default::default() },
            plx_plex::plex::account::Resource { name: secondary.name.clone(),
                client_identifier: secondary.machine_id.clone(), provides: "server".into(),
                access_token: secondary.token.clone(), ..Default::default() },
        ];
        let roster = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ProfileSwitch }, admission: AdmissionId(req),
            arrival: 2, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ProfileRoster(
                super::super::ProfileRosterProgress { epoch, expected: next_identity,
                    resources, reached: vec![primary.clone()], probes: Vec::new() }))),
        };
        let effects = step(&mut owner, SessionEvent::Result(roster));
        let reply = effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { req, epoch, arrival, .. } => Some(CommitReply {
                req: *req, epoch: *epoch, arrival: *arrival, admission: CommitAdmission::RegistryOnly,
            }),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the unavailable-secondary roster must commit");
        step(&mut owner, SessionEvent::Commit(reply));

        step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let effects = step(&mut owner, SessionEvent::Command(Command::SelectProfile { index: 0, pin: None }));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Capture { .. } | SessionFx::Work { .. })));
        assert!(owner.needs_ready_commit());
        let effects = step(&mut owner, SessionEvent::Command(Command::TakeReady));
        let final_plan = effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { plan, .. } => Some(plan),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the same-user shortcut must pass through take_ready");
        assert!(owner.commit_is_current(owner.common.next_req, owner.common.epoch, 0));
        let patch = final_plan.credentials.as_ref().expect("take_ready saves the live session");
        assert_eq!(patch.sources.iter().find(|source| source.machine_id == "b").unwrap().token,
            "kid-b-token", "B remains live even though its identity probe was unavailable");
        assert_eq!(patch.profiles.iter().find(|profile| profile.uuid == "u-kid").unwrap()
            .sources.iter().find(|source| source.machine_id == "b").unwrap().token,
            "kid-b-token", "take_ready must not overwrite the cached profile with a tokenless B");
    }

    #[test]
    fn unknown_roster_profile_refresh_keeps_profile_credentials_and_publishes_probes() {
        let _g = plx_base::testlock::serial();
        assert_non_admin_roster_refresh_is_probe_only(roster_refresh_fixture("u-unknown", vec![
            plx_plex::plex::session::HomeUserRef { id: 1, uuid: "u-owner".into(),
                title: "Owner".into(), admin: true, ..Default::default() },
        ]));
    }

    fn assert_admin_roster_refresh_accepts_credentials(mut owner: SessionMachine) {
        let (activate, reconcile) = land_account_roster_refresh(&mut owner);
        assert!(matches!(&activate.registry[..], [RegistryPlan::Activate { source, .. }]
            if source.token == "account-server-token"));
        let patch = reconcile.credentials.as_ref()
            .expect("an account-owner refresh persists its new grant");
        assert_eq!(patch.server.machine_id, "account-machine");
        assert_eq!(patch.server.token, "account-server-token");
        assert!(patch.user.token.is_empty() || patch.user.token == "account-server-token");
        assert!(reconcile.registry.iter().any(|plan|
            matches!(plan, RegistryPlan::Install { sources, .. }
                if sources.iter().any(|source| source.token == "account-server-token"))));
    }

    #[test]
    fn seated_admin_profile_roster_refresh_accepts_refreshed_tokens() {
        let _g = plx_base::testlock::serial();
        assert_admin_roster_refresh_accepts_credentials(roster_refresh_fixture("u-owner", vec![
            plx_plex::plex::session::HomeUserRef { id: 1, uuid: "u-owner".into(),
                title: "Owner".into(), admin: true, ..Default::default() },
        ]));
    }

    #[test]
    fn no_home_account_roster_refresh_accepts_refreshed_tokens() {
        let _g = plx_base::testlock::serial();
        assert_admin_roster_refresh_accepts_credentials(roster_refresh_fixture("", Vec::new()));
    }

    #[test]
    fn admin_refresh_never_keeps_an_identity_only_cached_primary_current() {
        let _g = plx_base::testlock::serial();
        let users = vec![plx_plex::plex::session::HomeUserRef { id: 1, uuid: "u-owner".into(),
            title: "Owner".into(), admin: true, ..Default::default() }];
        let mut owner = roster_refresh_fixture("u-owner", users);
        let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let admitted = plx_plex::plex::session::SourceRef { machine_id: "preferred-machine".into(),
            name: "Preferred server".into(), owned: true, token: "preferred-token".into(),
            address: "10.0.0.9".into(),
            port: 32400, origin_url: "https://10-0-0-9.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Remote), ..Default::default() };
        let identity_only = plx_plex::plex::session::SourceRef {
            machine_id: "profile-machine".into(), name: "Cached primary".into(), owned: false,
            token: "fresh-primary-token".into(), address: "10.0.0.8".into(), port: 32400,
            origin_url: "https://10-0-0-8.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default()
        };
        let resources = vec![
            plx_plex::plex::account::Resource { name: "Cached primary".into(),
                client_identifier: "profile-machine".into(), provides: "server".into(),
                access_token: "fresh-primary-token".into(), ..Default::default() },
            plx_plex::plex::account::Resource { name: admitted.name.clone(),
                client_identifier: admitted.machine_id.clone(), provides: "server".into(),
                owned: true, access_token: admitted.token.clone(), ..Default::default() },
        ];
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ServerRoster }, admission: AdmissionId(req),
            arrival: 1, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
                super::super::ServerRosterProgress { epoch, expected,
                    outcome: super::super::ServerRosterOutcome::Reconcile {
                        resources, found: vec![admitted.clone(), identity_only],
                        admitted_machine_id: admitted.machine_id.clone(), household: vec![1],
                        settled: Vec::new(),
                    },
                }))),
        };
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        let plan = effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { plan, .. } => Some(plan),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the admitted endpoint changes the roster");
        let patch = plan.credentials.as_ref().unwrap();
        assert_eq!(patch.server.machine_id, admitted.machine_id);
        let install = plan.registry.iter().find_map(|plan| match plan {
            RegistryPlan::Install { sources, primary, commit } => Some((sources, primary, commit)),
            RegistryPlan::DevInstall { .. } | RegistryPlan::Primary { .. }
            | RegistryPlan::Activate { .. } | RegistryPlan::Endpoint { .. } | RegistryPlan::Probe(_)
            | RegistryPlan::Revoke => None,
        }).expect("refresh installs an admitted roster");
        assert_eq!(install.1.and_then(|index| install.0.get(index))
            .map(|source| source.machine_id.as_str()), Some("preferred-machine"));
        // A refresh, not a switch. Whether it is the SAME identity is not this test's question: here
        // the holder's answer does not call the seated profile's owned server its own, so it is
        // unproven (`seated_is_account_holder`) and the executor revokes before re-tokening.
        assert!(matches!(install.2, RosterCommit::Refresh { .. }), "an admin refresh is not a profile switch");
    }

    #[test]
    fn admin_refresh_keeps_a_granted_cached_secondary_live_when_its_probe_misses() {
        let _g = plx_base::testlock::serial();
        let users = vec![plx_plex::plex::session::HomeUserRef { id: 1, uuid: "u-owner".into(),
            title: "Owner".into(), admin: true, ..Default::default() }];
        let mut owner = roster_refresh_fixture("u-owner", users);
        let secondary = plx_plex::plex::session::SourceRef { machine_id: "secondary".into(),
            name: "Secondary".into(), token: "old-secondary-token".into(),
            address: "10.0.0.7".into(), port: 32400,
            origin_url: "https://10-0-0-7.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Remote), ..Default::default() };
        owner.common.persisted.sources.push(secondary.clone());
        owner.common.persisted.refresh_profile_record();

        let req = owner.allocate(SessionOp::ServerRoster, None).unwrap();
        owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let epoch = owner.common.epoch;
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        let admitted = plx_plex::plex::session::SourceRef { machine_id: "profile-machine".into(),
            name: "Profile server".into(), owned: true, token: "fresh-primary-token".into(),
            address: "10.0.0.18".into(), port: 32400,
            origin_url: "https://10-0-0-18.example.plex.direct:32400".into(),
            tier: Some(plx_plex::plex::probe::Location::Local), ..Default::default() };
        let resources = vec![
            plx_plex::plex::account::Resource { name: admitted.name.clone(),
                client_identifier: admitted.machine_id.clone(), provides: "server".into(), owned: true,
                access_token: admitted.token.clone(), ..Default::default() },
            plx_plex::plex::account::Resource { name: secondary.name.clone(),
                client_identifier: secondary.machine_id.clone(), provides: "server".into(),
                access_token: "fresh-secondary-token".into(), ..Default::default() },
        ];
        let envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key: SessionWorkKey { epoch, op: SessionOp::ServerRoster }, admission: AdmissionId(req),
            arrival: 1, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::ServerRoster(
                super::super::ServerRosterProgress { epoch, expected,
                    outcome: super::super::ServerRosterOutcome::Reconcile {
                        resources, found: vec![admitted.clone()],
                        admitted_machine_id: admitted.machine_id, household: vec![1],
                        settled: Vec::new(),
                    },
                }))),
        };
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        let plan = effects.iter().find_map(|effect| match effect {
            SessionFx::Commit { plan, .. } => Some(plan),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("the primary address change commits the refreshed roster");
        let install = plan.registry.iter().find_map(|plan| match plan {
            RegistryPlan::Install { sources, primary, commit } => Some((sources, primary, commit)),
            RegistryPlan::DevInstall { .. } | RegistryPlan::Primary { .. }
            | RegistryPlan::Activate { .. } | RegistryPlan::Endpoint { .. } | RegistryPlan::Probe(_)
            | RegistryPlan::Revoke => None,
        }).expect("the address change replaces the live registry");
        assert!(install.0.iter().any(|source| source.machine_id == "secondary"
            && source.token == "fresh-secondary-token"),
            "a still-granted cached secondary must survive a missed identity probe");
        assert_eq!(*install.2, RosterCommit::Refresh { same_identity: true }, "an admin refresh is not a profile switch");
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
        use plx_machine::machine::{Cx, Effects, InputOwner, EntryId, Machine, Tick};
        let _g = plx_base::testlock::serial();
        for users in [None, Some(Vec::new())] {
            let mut owner = SessionMachine::from_init(local_session());
            let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
            let (req, key) = effects.iter().find_map(|fx| match fx {
                SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
                SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
                | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
                | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
                | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
                | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
                | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
                | SessionFx::RefreshCache { .. } => None,
            }).expect("Change profile fetches the Home roster");
            settle_picker_commit(&mut owner, &effects);
            let envelope = roster_envelope(&mut owner, req, key, users);

            let publication = owner.publication();
            let cx = Cx::<OwnerHost> { views: publication.read(), tick: Tick::default(),
                measure: &plx_machine::machine::BareMeasure, press: Default::default(),
                focus: Default::default(), owner: InputOwner::Entry(EntryId(0)) };
            let mut present = plx_machine::present::Present::new();
            assert!(present.take(0)); // admit the spinner's last frame
            present.acknowledge(0); // its simulated swap has completed
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
        assert!(owner.common.persisted.home_users.is_empty(), "rig: nothing cached to show");
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let (req, key) = effects.iter().find_map(|fx| match fx {
            SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
            | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
            | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
            | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => None,
        }).expect("Change profile fetches the Home roster");
        assert_eq!(owner.state.phase(), Phase::Profiles);
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
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("rig: the picker commits its registry plan");
        step(owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly }));
        assert!(owner.common.pending_commit.is_none(), "rig: nothing is left in flight");
    }

    fn roster_envelope(owner: &mut SessionMachine, req: u32, key: SessionWorkKey,
        users: Option<Vec<UserTile>>) -> SessionEnvelope {
        owner.common.pending.get_mut(&req).unwrap().admission = AdmissionState::Accepted(AdmissionId(req));
        let expected = super::super::SessionIdentity::of(&owner.common.persisted);
        SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: plx_machine::machine::RequestId(req) },
            key, admission: AdmissionId(req), arrival: 1, terminal: true, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::HomeRoster(
                super::super::HomeRosterProgress { epoch: key.epoch, expected, users }))),
        }
    }

    #[test]
    fn change_profile_with_no_roster_reads_out_the_failure_and_back_resumes_the_session() {
        let _g = plx_base::testlock::serial();
        for (users, reason) in [
            (None, roster_unreachable()),
            (Some(Vec::new()), roster_refused()),
        ] {
            let (mut owner, _) = empty_roster_change_profile(users);
            assert_eq!(owner.state.phase(), Phase::Profiles,
                "the failure is read out on the picker's own route, not as a sign-in Error");
            assert!(owner.state.picker_data().users.is_empty());
            assert_eq!(owner.state.picker_data().error, reason);
            assert!(owner.common.persisted.home_users.is_empty(), "no roster is invented or committed");
            let reply = ReplyTo { instance: 0, correlation: 7 };
            let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
            assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: true, .. })),
                "BACK from a picker that never offered a profile resumes the session behind it");
            assert_eq!(owner.state.phase(), Phase::Ready);
            assert!(owner.state.handoff_pending(), "the resume goes through the ordinary Ready handoff");
            assert!(owner.state.picker_data().error.is_empty());
        }
    }

    /// A refused roster must not wipe a CACHED one (it is what offline switching reads), and a
    /// picker that HAS tiles is still a root: BACK stays refused there.
    #[test]
    fn a_refused_roster_keeps_cached_tiles_and_the_picker_stays_a_root() {
        let _g = plx_base::testlock::serial();
        let mut init = local_session();
        init.persisted.home_users = vec![plx_plex::plex::session::HomeUserRef {
            uuid: "cached-user".into(), title: "Cached".into(), ..Default::default() }];
        let mut owner = SessionMachine::from_init(init);
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let (req, key) = effects.iter().find_map(|fx| match fx {
            SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
            | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
            | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
            | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => None,
        }).unwrap();
        settle_picker_commit(&mut owner, &effects);
        let envelope = roster_envelope(&mut owner, req, key, Some(Vec::new()));
        let effects = step(&mut owner, SessionEvent::Result(envelope));
        assert!(!effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })),
            "an answer with nobody in it is not committed over the cached roster");
        assert_eq!(owner.state.picker_data().users.len(), 1);
        assert_eq!(owner.common.persisted.home_users.len(), 1);
        assert!(owner.state.picker_data().error.is_empty());
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
        let _g = plx_base::testlock::serial();
        let saved = PersistedSession { client_id: "synthetic-client".into(), ..Default::default() };
        let mut owner = SessionMachine::from_init(SessionInit::captured_boot(saved, Some(local_server()), Vec::new()));
        let effects = step(&mut owner, SessionEvent::Command(Command::ActivateDevBootstrap));
        let (req, epoch) = effects.iter().find_map(|fx| match fx {
            SessionFx::Commit { req, epoch, .. } => Some((*req, *epoch)),
            SessionFx::Pump | SessionFx::Acknowledge(_) | SessionFx::Retire { .. }
            | SessionFx::Ready { .. } | SessionFx::Capture { .. } | SessionFx::Work { .. }
            | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_) | SessionFx::Erase { .. }
            | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. } | SessionFx::Incident { .. }
            | SessionFx::SelectionReply { .. } | SessionFx::BackReply { .. }
            | SessionFx::PlaintextAnswer { .. } | SessionFx::RefreshCache { .. } => None,
        }).expect("rig: the dev boundary commits");
        step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 0,
            admission: CommitAdmission::RegistryOnly }));
        assert_eq!(owner.state.phase(), Phase::Ready, "rig: the dev session is up");

        let before = owner.publication();
        step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        assert_eq!(owner.state.phase(), Phase::Profiles,
            "the picker the app just routed to must have a state behind it");
        assert_eq!(owner.state.picker_data().error, roster_refused());
        assert!(owner.state.picker_data().users.is_empty());
        // TV, PR #212: this read-out exists BEFORE the Profiles screen mounts, so only the step
        // that entered it can announce it — and it must, once.
        let entered = owner.publication();
        assert_eq!(roster_readout_entered(&before, &entered).as_deref(),
            Some(format!("profiles: no profiles to offer — {} (BACK returns)", roster_refused()).as_str()));
        assert_eq!(roster_readout_entered(&entered, &entered), None, "announced once, not per step");

        let reply = ReplyTo { instance: 0, correlation: 7 };
        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert_eq!(roster_readout_entered(&entered, &owner.publication()), None, "leaving is not entering");
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: true, .. })));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::Ready {
            install: ReadyInstall::AlreadyInstalled, .. })), "BACK hands the dev session back");
        assert_eq!(owner.state.phase(), Phase::Ready);
        assert!(owner.state.picker_data().error.is_empty());
        assert!(!before.switch_refused, "nothing has been refused before anybody asked");
        assert!(owner.publication().switch_refused,
            "the refusal outlives the read-out, so the menu stops offering it");
    }

    /// The account menu must not lead into #132's dead end once the app KNOWS it is one: plex.tv
    /// refused this identity a roster and nothing is cached to switch from. The verdict outlives
    /// the picker (BACK resumes the same identity), while "no verdict" — plex.tv unreachable —
    /// publishes none, because the menu row is what asks again.
    #[test]
    fn a_refused_roster_with_nothing_cached_is_published_as_the_identitys_switch_verdict() {
        let _g = plx_base::testlock::serial();
        for (users, refused) in [(None, false), (Some(Vec::new()), true)] {
            let (mut owner, _) = empty_roster_change_profile(users);
            let read = owner.publication();
            assert_eq!(read.switch_refused, refused, "verdict on the read-out itself");
            assert!(read.readout_back_resumes,
                "a stored, dialable session is behind the picker: BACK resumes it");
            let reply = ReplyTo { instance: 0, correlation: 7 };
            step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
            assert_eq!(owner.state.phase(), Phase::Ready, "rig: BACK resumed");
            assert_eq!(owner.publication().switch_refused, refused,
                "the same identity is still refused once the picker has gone");
            owner.common.persisted.user.uuid = "another-profile".into();
            owner.replace_publication();
            assert!(!owner.publication().switch_refused,
                "a different identity has not been refused anything");
        }
    }

    /// The verdict belongs to the identity's LIFECYCLE, not to one call site. Signing out ends
    /// the identity, so it forgets the verdict: signing back into the same account in the same
    /// process yields the same `Identity`, and a surviving verdict would hide *Change profile*
    /// until restart even after that login brought a roster.
    #[test]
    fn signing_out_forgets_the_switch_verdict() {
        let _g = plx_base::testlock::serial();
        let (mut owner, _) = empty_roster_change_profile(Some(Vec::new()));
        assert!(owner.publication().switch_refused, "rig: the verdict is recorded");
        let account = owner.common.persisted.clone();
        step(&mut owner, SessionEvent::Command(Command::SignOut));
        assert!(owner.common.switch_refused_for.is_none(), "sign-out ends the identity and its verdict");
        // The same account, signed back in: nothing refused it in this sign-in.
        owner.common.persisted = account;
        owner.replace_publication();
        assert!(!owner.publication().switch_refused);
    }

    /// Any AUTHORITATIVE roster clears the verdict — a sign-in's own roster as much as the
    /// picker's refresh — because both answer the question the verdict recorded. A sign-in that
    /// brought no roster answered nothing, and keeps it.
    #[test]
    fn a_sign_in_that_brings_a_roster_clears_the_switch_verdict() {
        let _g = plx_base::testlock::serial();
        for (users, cleared) in [
            (vec![UserTile { uuid: "u-1".into(), title: "Only user".into(), ..Default::default() }], true),
            (Vec::new(), false),
        ] {
            let mut init = reopened_after_authorization();
            init.switch_refused_for = Some(Identity::of(&init.persisted));
            let mut owner = SessionMachine::from_init(init);
            assert!(owner.publication().switch_refused, "rig: the verdict is recorded");
            let req = owner.common.next_req;
            let epoch = owner.common.epoch;
            let signed_in = qr_event(&owner, req, 1, super::super::LoginProgress::SignedIn {
                epoch, server: local_server(), sources: Vec::new(), users }, true);
            step(&mut owner, SessionEvent::Result(signed_in));
            step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1,
                admission: CommitAdmission::Admitted { revision: 1, purpose: PersistencePurpose::Discovery } }));
            assert!(owner.common.pending_commit.is_none(), "rig: the sign-in committed");
            assert_eq!(owner.publication().switch_refused, !cleared);
            assert_eq!(owner.common.switch_refused_for.is_none(), cleared);
        }
    }

    /// A refusal over a CACHED roster is no dead end — offline switching reads the cache (#132,
    /// #164) — so it is neither committed over the cache nor published as a verdict.
    #[test]
    fn a_refusal_over_a_cached_roster_is_not_a_switch_verdict() {
        let _g = plx_base::testlock::serial();
        let mut init = local_session();
        init.persisted.home_users = vec![plx_plex::plex::session::HomeUserRef {
            uuid: "cached-user".into(), title: "Cached".into(), ..Default::default() }];
        let mut owner = SessionMachine::from_init(init);
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let (req, key) = effects.iter().find_map(|fx| match fx {
            SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
            | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
            | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
            | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => None,
        }).unwrap();
        settle_picker_commit(&mut owner, &effects);
        let envelope = roster_envelope(&mut owner, req, key, Some(Vec::new()));
        step(&mut owner, SessionEvent::Result(envelope));
        assert_eq!(owner.common.persisted.home_users.len(), 1, "the cache survives the refusal");
        assert!(!owner.publication().switch_refused);
    }

    /// With no dialable session stored, BACK from the read-out hands the screen to the television
    /// — so the read-out must not offer a *Back* that claims to stay in the app.
    #[test]
    fn a_readout_with_no_session_behind_it_says_back_does_not_resume() {
        let _g = plx_base::testlock::serial();
        let mut persisted = local_session().persisted;
        persisted.server = Default::default();
        let mut owner = SessionMachine::from_init(SessionInit::captured(persisted));
        let effects = step(&mut owner, SessionEvent::Command(Command::StartSwitch(Picker::ChangeProfile)));
        let (req, key) = effects.iter().find_map(|fx| match fx {
            SessionFx::Work { req, key, input: SessionWork::HomeRoster { .. }, .. } => Some((*req, *key)),
            SessionFx::Commit { .. } | SessionFx::Pump | SessionFx::Acknowledge(_)
            | SessionFx::Retire { .. } | SessionFx::Ready { .. } | SessionFx::Capture { .. }
            | SessionFx::Work { .. } | SessionFx::Cancel { .. } | SessionFx::PublishProfile(_)
            | SessionFx::Erase { .. } | SessionFx::Coordinator(_) | SessionFx::RestartReply { .. }
            | SessionFx::Incident { .. } | SessionFx::SelectionReply { .. }
            | SessionFx::BackReply { .. } | SessionFx::PlaintextAnswer { .. }
            | SessionFx::RefreshCache { .. } => None,
        }).expect("rig: an account token is enough to ask for a roster");
        if effects.iter().any(|fx| matches!(fx, SessionFx::Commit { .. })) {
            settle_picker_commit(&mut owner, &effects);
        }
        let envelope = roster_envelope(&mut owner, req, key, Some(Vec::new()));
        step(&mut owner, SessionEvent::Result(envelope));
        let read = owner.publication();
        assert_eq!(read.roster_readout(), Some(roster_refused()), "rig: the read-out is up");
        assert!(!read.readout_back_resumes);
        let reply = ReplyTo { instance: 0, correlation: 7 };
        let effects = step(&mut owner, SessionEvent::Command(Command::BackAtRoot { reply }));
        assert!(effects.iter().any(|fx| matches!(fx, SessionFx::BackReply { resumed: false, .. })),
            "the published fact and BACK's own decision are one answer");
    }
}

#[cfg(test)]
#[path = "owner/attempt_characterization_tests.rs"]
mod attempt_characterization;
