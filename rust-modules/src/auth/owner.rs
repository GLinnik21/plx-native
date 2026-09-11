//! Concrete auth decisions and immutable publications. Resource operations belong to the
//! application adapter; constructing or observing this value performs no external work.

use super::{Phase, Picker, UserTile};
use crate::plex::session::{Session as PersistedSession, UserRef};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use crate::ui::machine::{Addr, Canon, LogicalState, MachineId};

/// Delivery keeps the exact request even though generic non-instance delivery drops its outer
/// request field. The application verifies the outer address before constructing this event.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SessionEnvelope {
    #[serde(with = "super::observation::address")]
    pub addr: Addr,
    pub key: SessionWorkKey,
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
    StartLogin,
    Retry,
    RestartWait { phase: Phase, qr_generation: u64, reply: ReplyTo },
    StartSwitch(Picker),
    SelectProfile { index: usize, pin: Option<String> },
    DismissPinError,
    BackAtRoot { reply: ReplyTo },
    SignOut,
    EraseLocal,
    NoteDeleteLeftovers(usize),
    RefreshRoster,
    RequestEndpoint { sid: u16 },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) struct ReplyTo { pub instance: u32, pub correlation: u32 }

/// Ordered application/coordinator work; the owner describes it without invoking another
/// machine, platform API or global publication from inside its transition.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub(crate) enum CoordinatorAction {
    CloseTelemetry,
    SignInStarted,
    SignInCompleted,
    SignInCancelled,
    SignInFailed { phase: Phase },
    ActivateProfile,
    ShowLogin,
    ShowProfiles,
    ShowHome,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum RegistryPlan {
    Activate { source: crate::plex::session::SourceRef, ipv6: bool },
    Install { sources: Vec<crate::plex::session::SourceRef>, primary: Option<usize>, replace: bool },
    Endpoint { expected: ServerLifecycle, source: crate::plex::session::SourceRef },
    Revoke,
}

/// Every variant is data. Closures, native Clients and MainThread cannot enter logical effects.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum SessionFx {
    Capture { req: u32, epoch: u64, command: Command },
    Work { req: u32, key: SessionWorkKey, input: SessionWork },
    Cancel { requests: Vec<u32>, epoch: u64 },
    Credentials { req: u32, epoch: u64, expected: Identity, patch: CredentialPatch },
    Registry { req: u32, epoch: u64, plan: RegistryPlan },
    PublishProfile(ProfilePublication),
    Erase { req: u32, epoch: u64 },
    Coordinator(CoordinatorAction),
    RestartReply { to: ReplyTo, accepted: bool },
    BackReply { to: ReplyTo, resumed: bool },
}

pub(crate) trait SessionHost: crate::ui::machine::Host {
    fn session_effect(effect: SessionFx) -> Self::Fx;
}

pub(crate) enum SessionEvent {
    Command(Command),
    Result(SessionEnvelope),
}

impl CredentialPatch {
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
pub(crate) enum SessionOp { Login, Rediscover, HomeRoster, ServerRoster, ProfileSwitch, Endpoint(u16) }

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
}

/// Only the owner may allocate this generation; adapters publish the supplied value verbatim.
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProfileScope(pub u32);

/// Private persisted init data, not diagnostic output. No constructor consults the filesystem.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SessionInit {
    pub phase: Phase,
    pub picker: Picker,
    pub persisted: PersistedSession,
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
    pub active_profile: Option<UserRef>,
    pub profile_scope: ProfileScope,
    pub delete_leftovers: usize,
}

impl SessionInit {
    pub fn captured(persisted: PersistedSession) -> Self {
        Self { phase: Phase::Idle, picker: Picker::Boot, persisted,
            pin_code: String::new(), qr_png: Vec::new(), users: Vec::new(), error: String::new(),
            pin_denied: false, authorized_in_flow: false, signin_active: false, apply_pending: false,
            code_replaced: false, qr_gen: 0, next_qr: 0, epoch: 1, next_req: 0,
            pending: BTreeMap::new(), active_profile: None, profile_scope: ProfileScope(0),
            delete_leftovers: 0 }
    }
}

fn write_user(w: &mut Canon, user: &UserRef) {
    w.u64(user.id as u64).str(&user.uuid).str(&user.title).str(&user.thumb).str(&user.token);
}

fn write_server(w: &mut Canon, server: &crate::plex::session::ServerRef) {
    w.str(&server.name).str(&server.machine_id).str(&server.address)
        .u64(server.port as u64).str(&server.token).str(&server.origin_url);
    write_tier(w, server.tier);
}

fn write_tier(w: &mut Canon, tier: Option<crate::plex::probe::Location>) {
    use crate::plex::probe::Location;
    w.u8(match tier { None => 0, Some(Location::Local) => 1,
        Some(Location::Remote) => 2, Some(Location::Relay) => 3 });
}

fn write_sources(w: &mut Canon, sources: &[crate::plex::session::SourceRef]) {
    w.seq(sources.len());
    for s in sources {
        w.str(&s.machine_id).str(&s.name).str(&s.shared_by).bool(s.owned)
            .str(&s.address).u64(s.port as u64).str(&s.token).str(&s.origin_url);
        write_tier(w, s.tier);
    }
}

/// Explicit field encoding. Serde is only the private init round-trip, never the state hash.
fn write_persisted(w: &mut Canon, s: &PersistedSession) {
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
    for profile in &s.profiles {
        w.str(&profile.uuid);
        write_user(w, &profile.user);
        write_server(w, &profile.server);
        write_sources(w, &profile.sources);
        w.option(profile.pin.as_ref(), |w, pin| { w.str(&pin.salt).str(&pin.hash).u32(pin.iters); });
    }
    // Preferences are captured input retained by the controller, not authority for disk writes.
    // Include their exact captured values in init/canonical state even though patches never write
    // them over a newer store-owned value.
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
        w.u8(match self.phase { Phase::Idle => 0, Phase::Creating => 1, Phase::Waiting => 2,
            Phase::Discovering => 3, Phase::Profiles => 4, Phase::Switching => 5,
            Phase::Ready => 6, Phase::Error => 7, Phase::Deleted => 8 });
        w.u8(match self.picker { Picker::Boot => 0, Picker::SignedIn => 1, Picker::ChangeProfile => 2 });
        write_persisted(w, &self.persisted);
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
            match pending.key.op {
                SessionOp::Login => { w.u8(0); }
                SessionOp::Rediscover => { w.u8(1); }
                SessionOp::HomeRoster => { w.u8(2); }
                SessionOp::ServerRoster => { w.u8(3); }
                SessionOp::ProfileSwitch => { w.u8(4); }
                SessionOp::Endpoint(sid) => { w.u8(5).u32(sid as u32); }
            }
            w.str(&pending.expected.client_id).str(&pending.expected.account_token)
                .str(&pending.expected.profile_uuid);
            w.option(pending.lifecycle, |w, life| { w.u32(life.sid as u32)
                .u32(life.instance_gen).u32(life.token_gen); });
            w.option(pending.last_arrival, |w, arrival| { w.u64(arrival); });
            w.u8(match pending.phase { StreamPhase::Running => 0, StreamPhase::ProfileSeated => 1 });
        }
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
        Self { phase: state.phase, qr_generation: state.qr_gen,
            code: Arc::from(state.pin_code.as_str()), png: Arc::from(state.qr_png.as_slice()),
            code_replaced: state.code_replaced, users: Arc::from(state.users.as_slice()),
            error: Arc::from(state.error.as_str()), pin_denied: state.pin_denied,
            profile: state.active_profile.as_ref().map(|p| ProfileRead {
                uuid: p.uuid.clone(), title: p.title.clone(), thumb: p.thumb.clone(),
            }), scope: state.profile_scope, delete_leftovers: state.delete_leftovers }
    }
}

pub(crate) struct SessionMachine {
    state: SessionInit,
    publication: Arc<SessionSnapshot>,
}

impl SessionMachine {
    pub fn from_init(state: SessionInit) -> Self {
        let publication = Arc::new(SessionSnapshot::from_state(&state));
        Self { state, publication }
    }
    pub fn snapshot_init(&self) -> SessionInit { self.state.clone() }
    pub fn read(&self) -> SessionRead<'_> { self.publication.read() }
    pub fn publication(&self) -> Arc<SessionSnapshot> { Arc::clone(&self.publication) }

    /// Start/restart is one owner decision. Check both counters before cancelling anything:
    /// exhaustion must not detach the still-live operation or reuse its resource address.
    fn restart_login(&mut self, fresh_login: bool, emit: &mut impl FnMut(SessionFx)) -> bool {
        let Some(epoch) = self.state.epoch.checked_add(1) else { return false };
        if self.state.next_req.checked_add(1).is_none() { return false; }
        let discovery = !fresh_login && super::retry_kind(self.state.phase,
            self.state.authorized_in_flow) == super::RetryKind::Discovery;
        let fresh_attempt = fresh_login || super::restart_is_a_new_attempt(self.state.signin_active);
        let requests = self.state.pending.keys().copied().collect();
        self.state.pending.clear();
        self.state.epoch = epoch;
        emit(SessionFx::Cancel { requests, epoch });
        if discovery {
            self.state.phase = Phase::Discovering;
            self.state.error.clear();
        } else {
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
        emit(SessionFx::Work { req, key: SessionWorkKey { epoch, op }, input });
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
                        let Some(next) = self.state.next_qr.checked_add(1) else { return false };
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
            last_arrival: None, phase: StreamPhase::Running,
        });
        Some(req)
    }

    /// Admission to logical application, not a resource reservation. Landing owns the latter.
    /// Merely inspecting an invalid envelope must not mutate pending state or its publication.
    fn accepts(&self, envelope: &SessionEnvelope) -> bool {
        if envelope.addr.to != MachineId::Session { return false; }
        let Some(pending) = self.state.pending.get(&envelope.addr.req.0) else { return false; };
        pending.key == envelope.key && pending.key.epoch == self.state.epoch
            && pending.last_arrival.is_none_or(|last| envelope.arrival > last)
    }

    fn replace_publication(&mut self) {
        let next = SessionSnapshot::from_state(&self.state);
        if next == *self.publication { return; }
        self.publication = Arc::new(SessionSnapshot {
            code: if next.code == self.publication.code { Arc::clone(&self.publication.code) } else { next.code },
            png: if next.png == self.publication.png { Arc::clone(&self.publication.png) } else { next.png },
            users: if next.users == self.publication.users { Arc::clone(&self.publication.users) } else { next.users },
            ..next
        });
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
            SessionEvent::Command(Command::StartLogin) => self.restart_login(true, &mut emit),
            SessionEvent::Command(Command::Retry) => self.restart_login(false, &mut emit),
            SessionEvent::Command(Command::RestartWait { phase, qr_generation, reply }) => {
                let accepted = super::restart_permitted(Some((*phase, *qr_generation)),
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
            SessionEvent::Result(envelope) => self.apply_qr_observation(envelope, &mut emit),
            // Resource-dependent commands are connected with their typed commit replies.
            SessionEvent::Command(_) => false,
        };
        if !Arc::ptr_eq(&before, &self.publication) {
            fx.invalidate(crate::ui::present::Provenance::Landing(MachineId::Session));
        }
        if handled { Handled::Yes } else { Handled::No }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn qr_event(owner: &SessionMachine, req: u32, arrival: u64,
        progress: super::super::LoginProgress, terminal: bool) -> SessionEnvelope {
        SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: crate::ui::machine::RequestId(req) },
            key: owner.state.pending[&req].key,
            arrival, terminal, lifecycle: None,
            outcome: SessionArrival::Data(Arc::new(super::super::observation::Observation::Login(progress))),
        }
    }

    #[test]
    fn owned_qr_transition_retains_coherent_reads_and_ignores_duplicate_arrivals() {
        let mut owner = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
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
        let mut owner = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
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
    fn authorization_advances_same_request_identity_and_retry_preserves_account_link() {
        let mut owner = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
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
        let mut a = SessionMachine::from_init(SessionInit::captured(PersistedSession::default()));
        let mut init = SessionInit::captured(PersistedSession::default());
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
        let mut init = SessionInit::captured(PersistedSession::default());
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
    fn envelope_validation_keeps_full_epoch_and_exact_destination() {
        use crate::ui::machine::RequestId;
        let mut init = SessionInit::captured(PersistedSession::default());
        init.epoch = 0x1_0000_0001;
        let mut owner = SessionMachine::from_init(init);
        let req = owner.allocate(SessionOp::Login, None).unwrap();
        let mut envelope = SessionEnvelope {
            addr: Addr { to: MachineId::Session, req: RequestId(req) },
            key: SessionWorkKey { epoch: 1, op: SessionOp::Login },
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
