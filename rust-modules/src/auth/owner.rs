//! Concrete auth decisions and immutable publications. Resource operations belong to the
//! application adapter; constructing or observing this value performs no external work.

use super::{Phase, Picker, UserTile};
use crate::plex::session::{Session as PersistedSession, UserRef};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use crate::ui::machine::{Addr, Canon, LogicalState, MachineId};

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

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct CommitPlan {
    pub expected_disk: Identity,
    pub credentials: Option<CredentialPatch>,
    pub registry: Vec<RegistryPlan>,
    pub lifecycle: Option<ServerLifecycle>,
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
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct CommitReply {
    pub req: u32,
    pub epoch: u64,
    pub arrival: u64,
    pub accepted: bool,
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
    pub fn reply(self, accepted: bool) -> CommitReply {
        CommitReply { req: self.req, epoch: self.epoch, arrival: self.arrival, accepted }
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
    Read(SessionReadReply),
    Pump,
    Admission(AdmissionReply),
    Erased { epoch: u64, leftovers: usize },
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
    /// One ordered erase awaiting resource completion: existing epoch and whether to sign in.
    pub pending_erase: Option<(u64, bool)>,
    pub inbox: VecDeque<SessionEnvelope>,
    /// Covers both the queued Fx::Pump and its eventual typed delivery. Cancellation does not
    /// forget a marker already in the dispatcher; it can safely pump a newer current inbox.
    pub pump_pending: bool,
    pub active_profile: Option<UserRef>,
    pub profile_scope: ProfileScope,
    pub delete_leftovers: usize,
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
            pending: BTreeMap::new(), pending_commit: None, pending_erase: None, inbox: VecDeque::new(), pump_pending: false,
            active_profile: None, profile_scope: ProfileScope(0),
            delete_leftovers: 0 }
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

pub(super) fn write_user(w: &mut Canon, user: &UserRef) {
    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb).str(&user.token);
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
        w.str(&s.machine_id).str(&s.name).str(&s.shared_by).bool(s.owned)
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
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProfileRead {
    pub uuid: String,
    pub title: String,
    pub thumb: String,
}

#[derive(Clone, Copy)]
pub(crate) struct SessionRead<'a>(pub &'a SessionSnapshot);

impl SessionSnapshot {
    pub fn read(&self) -> SessionRead<'_> { SessionRead(self) }
    fn from_state(state: &SessionInit) -> Self {
        Self::from_state_counted(state, None, &mut |_| {})
    }
    fn matches_state(&self, state: &SessionInit) -> bool {
        self.flow_epoch == state.epoch && self.phase == state.phase && self.qr_generation == state.qr_gen
            && &*self.code == state.pin_code.as_str() && &*self.png == state.qr_png.as_slice()
            && &*self.users == state.users.as_slice() && &*self.error == state.error.as_str()
            && self.code_replaced == state.code_replaced && self.pin_denied == state.pin_denied
            && self.scope == state.profile_scope && self.delete_leftovers == state.delete_leftovers
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
            }), scope: state.profile_scope, delete_leftovers: state.delete_leftovers }
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

    /// Read-only authorization at effect execution, after any carried cancellation command.
    /// The Bridge borrows this owner while the adapter performs the synchronous commit; no
    /// second epoch/decision counter is copied into that adapter.
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

    fn begin_commit(&mut self, req: u32, arrival: u64, terminal: bool,
        plan: CommitPlan, delta: CommitDelta, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.pending_commit.is_some() { return false; }
        let Some(pending) = self.state.pending.get_mut(&req) else { return false };
        if pending.key.epoch != self.state.epoch { return false; }
        pending.last_arrival = Some(arrival);
        let epoch = pending.key.epoch;
        self.state.pending_commit = Some(PendingCommit { req, epoch, arrival, terminal,
            writes_credentials: plan.credentials.is_some(), receipt: None, delta });
        emit(SessionFx::Commit { req, epoch, arrival, plan });
        true
    }

    fn take_ready(&mut self, emit: &mut impl FnMut(SessionFx)) -> bool {
        if self.state.phase != Phase::Ready || !self.state.apply_pending
            || self.state.pending_commit.is_some() { return false; }
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
        let plan = CommitPlan { expected_disk: self.state.disk_identity.clone(),
            credentials: Some(patch.clone()), lifecycle: None,
            registry: vec![RegistryPlan::Install { sources: next.sources.clone(), primary: None, replace: false }] };
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
        self.begin_commit(req, 0, true, CommitPlan { expected_disk: self.state.disk_identity.clone(),
            credentials: None, lifecycle: None, registry }, CommitDelta {
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
        self.begin_commit(req, 0, true, CommitPlan { expected_disk: self.state.disk_identity.clone(),
            credentials: None, lifecycle: None, registry: vec![RegistryPlan::Revoke] }, CommitDelta {
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
        let plan = CommitPlan { expected_disk: self.state.disk_identity.clone(),
            credentials: None, lifecycle: None,
            registry: vec![RegistryPlan::Install { sources: self.state.persisted.sources.clone(),
                primary: None, replace: false }] };
        self.begin_commit(req, 0, true, plan, CommitDelta {
            phase: Some(Phase::Ready), activate_profile: true, ready: Some(false),
            ..Default::default()
        }, emit)
    }

    fn apply_commit_reply(&mut self, reply: CommitReply, emit: &mut impl FnMut(SessionFx)) -> bool {
        if !self.commit_is_current(reply.req, reply.epoch, reply.arrival) { return false; }
        let commit = self.state.pending_commit.take().unwrap();
        if !reply.accepted {
            if let Some(DevCommitDelta::StartAccount { login_req }) = commit.delta.dev {
                self.retire(login_req, emit);
            }
            let op = self.state.pending.remove(&reply.req).unwrap().key.op;
            match op {
                SessionOp::Login | SessionOp::Rediscover =>
                    self.fail_login("Couldn't finish sign-in. Try again.", emit),
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
                self.state.disk_identity = patch.identity();
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
                SessionOp::Login => self.fail_login("Couldn't start sign-in. Try again.", emit),
                SessionOp::Rediscover => self.fail_login("Couldn't restart server discovery. Try again.", emit),
                SessionOp::ProfileSwitch => {
                    self.state.phase = Phase::Profiles;
                    self.state.error = "Couldn't switch profile. Try again.".into();
                }
                SessionOp::HomeRoster => self.fail_empty_home_roster(emit),
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
        if let Some(receipt) = self.state.pending_commit.take().and_then(|commit| commit.receipt) {
            receipts.push(receipt);
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
        if !matches!(self.state.authority, BootstrapAuthority::Account { .. }) { return false; }
        if self.state.pending_erase.is_some() { return false; }
        if self.advance_epoch(emit).is_none() { return false; }
        if self.state.persisted.account_token.is_empty() {
            self.fail_login("You're signed out — sign in to use profiles.", emit);
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
            let plan = CommitPlan { expected_disk: self.state.disk_identity.clone(),
                credentials: None, lifecycle: None, registry };
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
        let stored = self.state.committed_credentials.merge_into(&self.state.persisted);
        if !super::resumable(&stored, self.state.picker) || self.state.epoch.checked_add(1).is_none() {
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
        self.state.signin_active = false;
        self.state.apply_pending = false;
        self.state.code_replaced = false;
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
                    source.machine_id == captured.machine_id && source.usable()) => {
                self.state.pending.get_mut(&req).unwrap().lifecycle = Some(captured.lifecycle);
                SessionWork::Endpoint { session: self.state.persisted.clone(), expected: Identity::of(&self.state.persisted),
                    lifecycle: captured.lifecycle, machine_id: captured.machine_id.clone() }
            }
            (CaptureIntent::Endpoint { .. }, SessionReadValue::Endpoint(_)) => {
                self.retire(req, emit);
                return true;
            }
            (CaptureIntent::Login, SessionReadValue::LoginClientId(_)) => {
                self.fail_login("Couldn't start sign-in. Try again.", emit);
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

    fn fail_empty_home_roster(&mut self, emit: &mut impl FnMut(SessionFx)) {
        if self.state.users.is_empty() && self.state.phase == Phase::Profiles {
            self.fail_login("Couldn't load profiles — check the connection.", emit);
        }
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
                self.fail_empty_home_roster(emit);
            }
            self.retire(req, emit);
            self.replace_publication();
            return true;
        };
        let mut delta = CommitDelta::default();
        let mut plan = CommitPlan { expected_disk: self.state.disk_identity.clone(),
            credentials: None, registry: Vec::new(), lifecycle: pending.lifecycle };
        match &**data {
            Observation::Login(super::LoginProgress::SignedIn { server, sources, users, .. }) => {
                let mut next = self.state.persisted.clone();
                next.server = server.clone();
                next.sources = sources.clone();
                next.home_users = users.iter().map(super::UserTile::to_ref).collect();
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
                            origin_url: candidate.origin.base(), address: candidate.address.clone(),
                            port: i64::from(candidate.origin.port()), tier: Some(candidate.location),
                        }, ipv6: candidate.ipv6,
                    },
                    super::RegistryProgress::Settled { probe, .. } => RegistryPlan::Probe(probe.clone()),
                    super::RegistryProgress::Install { sources, primary, .. } =>
                        RegistryPlan::Install { sources: sources.clone(), primary: *primary, replace: false },
                });
            }
            Observation::HomeRoster(progress) => {
                let Some(users) = &progress.users else {
                    self.fail_empty_home_roster(emit);
                    self.retire(req, emit);
                    self.replace_publication();
                    return true;
                };
                let mut next = self.state.persisted.clone();
                next.home_users = users.iter().map(super::UserTile::to_ref).collect();
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                delta.users = Some(users.clone());
                plan.credentials = Some(patch);
            }
            Observation::ServerRoster(progress) => {
                let super::ServerRosterOutcome::Reconcile { resources, found, household, settled } = &progress.outcome else {
                    self.retire(req, emit);
                    return true;
                };
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
                let Some(fresh) = &progress.fresh else { self.retire(req, emit); return true; };
                let Some(lifecycle) = pending.lifecycle else { return false; };
                let mut next = self.state.persisted.clone();
                let Some((source, changed)) = super::apply_refreshed_endpoint(&mut next, &progress.machine_id, fresh) else {
                    self.retire(req, emit);
                    return true;
                };
                let patch = CredentialPatch::of(&next);
                delta.credentials = Some(patch.clone());
                if changed && !self.state.apply_pending { plan.credentials = Some(patch); }
                plan.registry.push(RegistryPlan::Endpoint { expected: lifecycle, source });
            }
        }
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
            self.state.apply_pending = false;
            self.state.code_replaced = false;
            self.state.qr_gen = 0;
        }
        self.state.signin_active = true;
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

    fn fail_login(&mut self, message: &str, emit: &mut impl FnMut(SessionFx)) {
        if std::mem::take(&mut self.state.signin_active) {
            emit(SessionFx::Coordinator(CoordinatorAction::SignInFailed { phase: self.state.phase }));
        }
        self.state.error = message.to_owned();
        self.state.phase = Phase::Error;
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
                let message = match (envelope.key.op, &envelope.outcome) {
                    (SessionOp::Rediscover, SessionArrival::Refused) => "Couldn't restart server discovery. Try again.",
                    (_, SessionArrival::Refused) => "Couldn't start sign-in. Try again.",
                    _ => "Couldn't finish sign-in. Try again.",
                };
                self.fail_login(message, emit);
            }
            SessionArrival::Data(data) => {
                let super::observation::Observation::Login(progress) = &**data else { return false };
                use super::LoginProgress;
                let (epoch, terminal) = match progress {
                    LoginProgress::CodeReplacing { epoch }
                    | LoginProgress::CodeReady { epoch, .. }
                    | LoginProgress::Authorized { epoch, .. } => (*epoch, false),
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
                    }
                    LoginProgress::CodeReady { code, qr_png, .. } => {
                        if envelope.key.op != SessionOp::Login { return false; }
                        let Some(next) = self.state.next_qr.checked_add(1) else {
                            self.fail_login("Couldn't start sign-in. Try again.", emit);
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
                        self.state.phase = Phase::Discovering;
                        self.state.pending.get_mut(&req).unwrap().expected = Identity::of(&self.state.persisted);
                    }
                    LoginProgress::Failed { message, .. } => self.fail_login(message, emit),
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
            SessionEvent::Read(reply) => self.apply_read(reply, &mut emit),
            SessionEvent::Admission(reply) => self.apply_admission(*reply, &mut emit),
            SessionEvent::Erased { epoch, leftovers } => self.erased(*epoch, *leftovers, &mut emit),
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
            epoch, message: "synthetic discovery failure".into(),
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
        let acked = step(&mut owner, SessionEvent::Commit(CommitReply { req, epoch, arrival: 1, accepted: true }));
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
                epoch: owner.state.epoch + 1, message: "rejected payload text".into(),
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
                epoch: owner.state.epoch + 1, message: "rejected payload text".into(),
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
}
