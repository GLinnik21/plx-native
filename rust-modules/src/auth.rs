//! Auth policy, immutable worker observations, and main-thread resource operations.
//!
//! The application's Bridge contains the concrete [`SessionMachine`] and a separate Session
//! resource adapter. Commands and addressed observations enter that owner; the UI borrows its
//! coherent publication. No module-global controller, epoch allocator or progress queue remains.
//! Workers receive captured inputs and an observation sink, never session/registry write authority.
//! Resource commits and Ready handoff are accepted on main with exact request/epoch/lifecycle
//! checks. Credential patches preserve newer disk preferences. Tokens are never logged.
//!
//! Stored Home, picker and explicit developer bootstrap use the same owner, with distinct typed
//! authority. Network/PIN derivation remain worker operations; offline policy is retained below.
use crate::plex::account::{AccountClient, HomeUser, PinPoll, Resource, SwitchOutcome};
use crate::plex::probe::{self, Candidate, Outcome, ProbePlan};
use crate::plex::session::{self, ProfileCreds, ServerRef, Session, SourceRef, UserRef};
use crate::plex::{Origin, ServerId};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

pub(crate) mod owner;
pub(crate) mod observation;
pub(crate) use owner::{SessionInit, SessionMachine, SessionRead};

/// Resource executor entry. Every credential and network-policy input is captured by the
/// requesting owner/adapter; workers can only observe cancellation and publish stream facts.
pub(crate) fn run_session_work(key: owner::SessionWorkKey,
    input: owner::SessionWork, output: &dyn owner::ObservationSink) {
    fn identity(value: owner::Identity) -> SessionIdentity {
        SessionIdentity { client_id: value.client_id, account_token: value.account_token,
            profile_uuid: value.profile_uuid }
    }
    let epoch = key.epoch;
    match input {
        owner::SessionWork::Login { client_id } => login_worker_with_output(epoch, client_id, output),
        owner::SessionWork::Rediscover { client_id, account_token } =>
            rediscovery_worker_with_output(client_id, account_token, epoch, output),
        owner::SessionWork::HomeRoster { client_id, account_token, expected } =>
            home_roster_worker_with_output(epoch, identity(expected), client_id, account_token, output),
        owner::SessionWork::ServerRoster { session, expected } => {
            let household = session.household_ids();
            server_roster_worker_with_output(session, epoch, identity(expected), household, output);
        }
        owner::SessionWork::ProfileSwitch { session, expected, tile, pin, recently_unreachable } =>
            profile_switch_worker_with_output(epoch, identity(expected), session, tile, pin,
                recently_unreachable, output, |ac, uuid, pin| ac.switch_user(uuid, pin)),
        owner::SessionWork::Endpoint { session, expected, lifecycle, machine_id } => {
            endpoint_worker_with_io(epoch, session, expected, lifecycle, machine_id, output,
                |ac| ac.resources(), probe_profile_resource_live);
        }
    }
}

/// Shared endpoint worker body; only account/probe IO is injectable. Native lifecycle remains
/// adapter metadata and only main can apply the terminal observation.
pub(crate) fn endpoint_worker_with_io(epoch: u64, session: Session, expected: owner::Identity,
    lifecycle: owner::ServerLifecycle, machine_id: String, output: &dyn owner::ObservationSink,
    resources: impl FnOnce(&AccountClient) -> Option<Vec<Resource>>,
    probe: impl FnOnce(&Resource, &[i64]) -> (Option<SourceRef>, SettledProbe)) {
    let fresh = probe_endpoint_work(ServerId::from_raw(lifecycle.sid), &machine_id, &session,
        resources, probe, &|| output.live());
    output.terminal(endpoint_work_fact(epoch, expected, lifecycle, machine_id, fresh));
}

/// Endpoint transport projection shared by the real worker and injected network-result tests.
/// Admission, interest and native lifecycle validation remain in the adapter/owner protocol.
pub(crate) fn endpoint_work_fact(epoch: u64, expected: owner::Identity,
    lifecycle: owner::ServerLifecycle, machine_id: String, fresh: Option<SourceRef>) -> AuthProgress {
    AuthProgress::Endpoint(EndpointProgress { epoch,
        expected: SessionIdentity { client_id: expected.client_id, account_token: expected.account_token,
            profile_uuid: expected.profile_uuid },
        id: ServerId::from_raw(lifecycle.sid), machine_id, lifecycle: None, fresh })
}

/// Application commands are the concrete owner's domain vocabulary, not global operations.
pub(crate) use owner::Command as SessionCmd;

/// Which stage the flow is in — the Login/Profiles screens switch on this each frame.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    /// Not in the login flow (offline / dev-token path handles startup instead).
    #[default]
    Idle,
    /// Requesting a pin from plex.tv (brief spinner before the QR appears).
    Creating,
    /// Showing the QR + code, polling until the user authorizes on their phone.
    Waiting,
    /// Got the account token; discovering the server (spinner).
    Discovering,
    /// Showing the "who's watching" roster.
    Profiles,
    /// Switching to the chosen profile (spinner).
    Switching,
    /// Credentials resolved — the main loop should install them and go Home.
    Ready,
    /// A step failed; show the message and allow a retry.
    Error,
    /// All local state was erased. No worker runs until the user explicitly starts sign-in.
    Deleted,
}

/// Does an error retry need a new account sign-in, or only another server-discovery pass?
/// Keeping this decision pure makes the UI contract gradeable without spawning a network worker.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RetryKind {
    Login,
    Discovery,
}

fn retry_kind(phase: Phase, authorized_in_flow: bool) -> RetryKind {
    // **`Discovering` is here because a retry no longer only follows an error.** The sign-in
    // screen offers a `Try again` once a working phase has stalled (`ui::login`'s escape), and the
    // phase most likely to stall is discovery itself — reached only after the pin has already
    // yielded an account credential. Keying solely on `Error` sent that press down the `Login`
    // arm and minted a fresh QR, throwing away a sign-in the user had already completed on their
    // phone. `authorized_in_flow` is the fact that actually matters; the phase list only keeps a
    // retry from a state where no worker is owed anything.
    if authorized_in_flow && matches!(phase, Phase::Error | Phase::Discovering) {
        RetryKind::Discovery
    } else {
        RetryKind::Login
    }
}

/// **Which who's-watching picker is on screen** — the one fact [`cancel`] cannot work out for
/// itself, and the difference between an escape hatch and a privilege escalation.
///
/// It is ONE screen raised from THREE places, and BACK means something different on each. At BOOT
/// nobody has identified themselves this run: there is nothing behind the picker but the persisted
/// session, and reinstating that silently is exactly the thing a PIN is there to stop — so BACK
/// there resumes only an UNPROTECTED stored profile. The other two resume nothing at all, for two
/// different reasons. After a QR sign-in the standing person has proved they hold the ACCOUNT, but
/// an account credential is not a household PIN and no profile has been chosen yet. And *Change
/// profile* DETACHES what was behind it ([`detaches_active_profile`]), which is what makes its
/// picker a root — the paragraph that used to sit here said Home is behind it and backing out hands
/// the user what they were already holding, and that reasoned about the person who PRESSED the
/// control rather than the one now holding the remote.
///
/// Nothing in the state below could tell them apart (all three arrive at [`Phase::Profiles`] with
/// the same roster), so every raise site names its own kind. Boot/change-profile use the owner's
/// StartSwitch command; accepted QR completion selects SignedIn after its resource commit ACK.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum Picker {
    /// The boot gate's who's-watching, before any profile has been chosen this run.
    ///
    /// **The default, and deliberately the STRICT one.** Every picker names its own kind, so the
    /// default also covers Login's BACK before a picker is raised. "We cannot say who is asking"
    /// must not resolve to "hand
    /// over the credentials" — a permissive default is the shape of the bug this enum exists to
    /// fix, and it is what left the dev-only `/tmp/plxnative-login` boot on the wrong side of it.
    #[default]
    Boot,
    /// Home's *Change profile*: a profile WAS active, and raising this picker detaches it.
    ///
    /// **The strictest of the three, despite being raised from the most authenticated place.** Home
    /// is no longer behind it, so there is nothing to back out to and BACK restores nothing at all
    /// — not even an unprotected previous profile. The whole argument is on
    /// [`detaches_active_profile`] and [`may_resume`].
    ChangeProfile,
    /// The picker the QR sign-in raises when the account turns out to have a Plex Home roster —
    /// accepted QR completion, not StartSwitch. Whoever is standing there completed a plex.tv sign-in
    /// seconds ago, but an account credential is not a household PIN and no profile has been chosen
    /// yet, so BACK resumes nothing here either — [`may_resume`].
    SignedIn,
}

/// One "who's watching" tile.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserTile {
    /// This member's plex.tv account id. Nothing on screen reads it — it rides through so that
    /// [`session::Session::household_ids`] is filled on the one path that writes the persisted
    /// roster, which is what lets the "Shared by …" rule tell the household's own server from a
    /// friend's share (`plex::servers::owner_credit`).
    pub id: i64,
    pub title: String,
    pub thumb: String,
    pub uuid: String,
    pub protected: bool, // needs a PIN
    pub admin: bool,
}
impl UserTile {
    fn of(u: &HomeUser) -> UserTile {
        UserTile {
            id: u.id,
            title: u.title.clone(),
            thumb: u.thumb.clone(),
            uuid: u.uuid.clone(),
            protected: u.protected,
            admin: u.admin,
        }
    }
    fn of_ref(u: &session::HomeUserRef) -> UserTile {
        UserTile {
            id: u.id,
            title: u.title.clone(),
            thumb: u.thumb.clone(),
            uuid: u.uuid.clone(),
            protected: u.protected,
            admin: u.admin,
        }
    }
    fn to_ref(&self) -> session::HomeUserRef {
        session::HomeUserRef {
            id: self.id,
            uuid: self.uuid.clone(),
            title: self.title.clone(),
            thumb: self.thumb.clone(),
            protected: self.protected,
            admin: self.admin,
        }
    }
}

/// PMS credentials the main loop installs once the flow resolves.
pub struct ReadyCreds {
    pub(crate) install: owner::ReadyInstall,
    /// **Where the primary server is** — an [`Origin`], not a `(host, port)` pair, because the
    /// pair cannot say `https` and the host a certificate is issued for is not the address behind
    /// it (`plex::origin`). Read straight off the stored [`session::ServerRef`], which is the
    /// value discovery wrote and the one `can_go_local` gates.
    pub origin: Origin,
    pub token: String,
    /// The tier that won discovery, restored only after the main thread installs/re-points the
    /// client because a fresh client deliberately starts with an unknown link.
    pub tier: Option<probe::Location>,
    /// The origin's resolve pin, read off the same stored record ([`session::ServerRef::resolve_pin`])
    /// so the install this hands off dials the LAN name with no resolver, exactly as the boot
    /// gate does.
    pub pin: Option<crate::plex::ResolvePin>,
}

/// Append a line to the shared on-device event log (never a token — only ids/counts/status).
use crate::log;

/// Does restarting this flow begin a NEW sign-in attempt, as the diagnostics count them?
///
/// One line, named, because the schema states a contract that a boolean inversion here would break
/// silently: a `SignInStarted` is bracketed by exactly one completed/failed/cancelled. An attempt
/// still marked active has already reported its start and has not yet reported a settle, so
/// restarting it is that attempt carrying on — which is what BOTH of the sign-in screen's timed
/// escapes do. Only a restart from a settled read-out, whose `set_error` already reported the
/// failure, opens a new bracket.
fn restart_is_a_new_attempt(signin_active: bool) -> bool {
    !signin_active
}

/// May a restart act on the flow that is live right now?
///
/// Pure, and separate, because it is the whole of the check that closes the two races above and
/// the alternative is proving it against plex.tv. `None` is the settled read-out's own control,
/// which has no live wait to be wrong about.
fn restart_permitted(expected: Option<(Phase, u64)>, live: (Phase, u64)) -> bool {
    match expected {
        Some(e) => e == live,
        None => true,
    }
}

/// May BACK out of the flow silently resume the stored session?
///
/// Pure, and split out from [`cancel`] so the one decision that gates a credential is gradeable on
/// the host: its caller runs inside the SDL event loop, where no test can reach it.
fn may_resume(from: Picker, stored_is_protected: bool) -> bool {
    match from {
        // **Nothing is behind this picker any more** — see [`detaches_active_profile`]. It used to
        // answer `true` on the reasoning that Home sits behind it and its user is already signed
        // in as that profile, so BACK hands back exactly what they were holding. That is true of
        // the person who pressed *Change profile* and false of the next person, which is the whole
        // point of the control: you press it when you are about to hand the remote over.
        Picker::ChangeProfile => false,
        // The account was authorized, but nobody selected a household profile. Resuming here uses
        // the owner's server token and bypasses the profile PIN boundary entirely.
        Picker::SignedIn => false,
        // Nobody has identified themselves yet, so resuming a protected profile IS the bypass.
        Picker::Boot => !stored_is_protected,
    }
}

/// Does raising this picker DETACH whatever profile was active behind it?
///
/// **Only *Change profile*, and it is the whole of the fix for "BACK bypasses the PIN".** The other
/// two have nothing to detach: at BOOT nobody has been attached this run, and the picker a QR
/// sign-in raises has authorized an ACCOUNT and never a profile.
///
/// Detaching is two things happening together, and neither is sufficient alone. [`may_resume`]
/// stops BACK reinstating the credentials, and the owner's explicit None profile publication
/// stops the process still ANSWERING with the profile that was active — the Home chip, the account
/// menu's rows and `search::recents`' per-profile store all read it, and a picker that has
/// announced a profile boundary must not be standing over a process that still knows who was
/// watching. The generation bump is what makes the second half take effect; see `session::current`.
///
/// **Two things are deliberately NOT detached, and the honest statement of this rule needs both.**
///
/// The SESSION FILE's `user` stays: that is disk state — who this device was last signed in as —
/// and blanking it would cost `switch_thread`'s offline fast path (picking your own unprotected
/// tile with no network), which is not a credential boundary, since that path refuses a tile CACHED
/// as protected and every such PIN still goes to plex.tv.
///
/// So a RESTART re-attaches through the BOOT GATE rather than through this one, and what happens
/// there is that gate's policy, not this one's: with a roster of more than one it raises a picker
/// unless Automatically Sign In is on and a profile is already seated
/// ([`crate::plex::session::Session::boot_shows_picker`]), and with a roster of one or none
/// `app.rs` installs the stored profile directly, PIN or no PIN. Two known staleness/policy gaps
/// sit behind that sentence and are deliberately NOT closed here — the single-user boot restore,
/// and the fact that `protected` is read from a CACHED roster that plex.tv may have moved on from.
/// Both are older than this rule, both are one owner decision about what to do with no network,
/// and the obvious fix for the first (gating boot on [`Session::active_profile_is_protected`]) is
/// worse than the bug: that predicate answers TRUE for an empty or unknown roster by design, so it
/// would put a PIN screen in front of every single-account user who has no PIN at all.
/// Automatically Sign In is the explicit opt-in that extends the single-user restore to a
/// multi-user roster, PIN included.
///
/// The previous profile's per-user PMS token also stays installed in the server registry. It has to
/// — the roster's own avatars are fetched through it (`ui::profiles`'s `Art::Thumb`), so revoking
/// here would blank the faces on the screen doing the asking. So "detached" means *no route and no
/// identity*, not *no credential in the process*: **no picker action routes into catalog content**
/// — which is the precise claim, since background pumps and those avatar requests do still consume
/// the retained client — and the only ways off the screen are choosing a tile (which re-points the
/// registry) and *Sign out* (which revokes).
fn detaches_active_profile(from: Picker) -> bool {
    match from {
        Picker::ChangeProfile => true,
        Picker::Boot | Picker::SignedIn => false,
    }
}

/// Is there something behind this screen that BACK may silently resume?
///
/// The whole of [`cancel`]'s decision, as one pure question, so that the caller can ask it BEFORE
/// invalidating the flow rather than after — which is the difference between a swallowed key press
/// and a dead sign-in.
fn resumable(sess: &Session, from: Picker) -> bool {
    sess.can_go_local() && may_resume(from, sess.active_profile_is_protected())
}

/// Why a resume was refused, for the event log — the file users send us.
///
/// **No profile NAME**, deliberately: the line is about the flow, not about who is behind the PIN.
/// And no SCREEN either, because the strict [`Picker`] default means the sign-in screen's BACK can
/// land here too. Four causes, because they read as four different bug reports — and the first
/// exists because the *Change-profile* refusal is not about a PIN at all: the profile behind that
/// picker is commonly UNPROTECTED, so reporting "the stored profile is PIN-protected" there sends
/// whoever reads the log looking for a PIN that was never involved.
#[cfg(test)]
fn refusal_reason(from: Picker, sess: &Session) -> &'static str {
    match from {
        Picker::ChangeProfile => "auth: BACK refused — the Change-profile picker is a root",
        _ if !sess.can_go_local() => {
            "auth: BACK refused — there is no stored session to go back to"
        }
        _ if sess.user.uuid.is_empty() => {
            "auth: BACK refused — no profile has been chosen on this device yet"
        }
        _ => "auth: BACK refused — the stored profile is PIN-protected",
    }
}

/// Every seating of a PIN-free profile also writes its cache record, whichever path seated it —
/// the switch worker writes protected ones because only it holds a PIN to verify against, but an
/// unprotected profile's record is the session itself, so a stored sign-in that predates the cache
/// becomes seatable offline the first time it is used, with no online switch required.
fn remember_unprotected_active(sess: &mut Session) {
    // An existing record (protected or not) follows the session; only a MISSING record for an
    // unprotected profile is created here.
    sess.refresh_profile_record();
    if sess.user.uuid.is_empty()
        || sess.pms_token().is_empty()
        || sess.active_profile_is_protected()
        || sess.cached_profile(&sess.user.uuid).is_some()
    {
        return;
    }
    sess.remember_profile(ProfileCreds {
        uuid: sess.user.uuid.clone(),
        user: sess.user.clone(),
        server: sess.server.clone(),
        sources: sess.sources.clone(),
        pin: None,
    });
}

fn home_roster_worker_with_output(epoch: u64, expected: SessionIdentity, cid: String,
    token: String, output: &dyn owner::ObservationSink) {
    if !output.live() { return; }
    let ac = AccountClient::new(&cid, Some(&token));
    let users = match ac.home_users() {
        Some(users) if !users.is_empty() => {
            let users: Vec<UserTile> = users.iter().map(UserTile::of).collect();
            log(&format!("auth: roster refreshed n={}", users.len()));
            Some(users)
        }
        _ => {
            log("auth: roster refresh failed — keeping cached roster");
            None
        }
    };
    output.terminal(AuthProgress::HomeRoster(HomeRosterProgress {
        epoch,
        expected,
        users,
    }));
}

// QR, profile, roster and endpoint workers share the adapter's addressed observation stream.
// The owner serializes application through commit acknowledgments; resource effects never run
// on the producer. Cancellation, receipt return and physical producer completion are distinct.

/// The credential identity a worker captured at its spawn. It is validation only: accepted
/// observations patch the latest controller/disk value field-by-field and never write this stale
/// snapshot back over preferences that changed while the request was in flight.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SessionIdentity {
    client_id: String,
    account_token: String,
    profile_uuid: String,
}

impl SessionIdentity {
    #[cfg(test)]
    pub(crate) fn of(s: &Session) -> Self {
        Self {
            client_id: s.client_id.clone(),
            account_token: s.account_token.clone(),
            profile_uuid: s.user.uuid.clone(),
        }
    }

}

/// One candidate activation observed by a probe coordinator. This carries the exact origin,
/// credential and link facts the old worker-side `activate_candidate` call used; applying them is
/// delayed until the main thread accepts the epoch/session identity.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct CandidateActivation {
    machine_id: String,
    token: String,
    name: String,
    credit: String,
    owned: bool,
    #[serde(with = "observation::origin")]
    origin: Origin,
    address: String,
    location: probe::Location,
    ipv6: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum RegistryProgress {
    Activate {
        epoch: u64,
        expected: Option<SessionIdentity>,
        candidate: CandidateActivation,
    },
    Settled {
        epoch: u64,
        expected: Option<SessionIdentity>,
        probe: SettledProbe,
    },
    Install {
        epoch: u64,
        expected: Option<SessionIdentity>,
        sources: Vec<SourceRef>,
        primary: Option<usize>,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct HomeRosterProgress {
    epoch: u64,
    expected: SessionIdentity,
    users: Option<Vec<UserTile>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ServerRosterProgress {
    epoch: u64,
    expected: SessionIdentity,
    outcome: ServerRosterOutcome,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum ServerRosterOutcome {
    Unreachable,
    NoReachable,
    Reconcile {
        #[serde(with = "observation::resources")]
        resources: Vec<Resource>,
        found: Vec<SourceRef>,
        household: Vec<i64>,
        settled: Vec<SettledProbe>,
    },
}

pub(crate) struct EndpointProgress {
    epoch: u64,
    expected: SessionIdentity,
    id: ServerId,
    machine_id: String,
    lifecycle: Option<ClientLifecycle>,
    fresh: Option<SourceRef>,
}

/// The exact registry incarnation an endpoint request was issued through. `ServerId` and
/// `machine_id` survive a re-point and a profile retoken, so neither can prove that a late route
/// result still belongs to the client/token that launched it.
#[derive(Clone, Copy)]
pub(crate) struct ClientLifecycle {
    client: &'static crate::plex::Client,
    token_gen: u32,
}

impl ClientLifecycle {
    pub(crate) fn machine_id(self) -> &'static str { self.client.machine_id() }

    pub(crate) fn capture(client: &'static crate::plex::Client) -> Self {
        Self { client, token_gen: client.token_gen() }
    }
    pub(crate) fn logical(self, sid: u16) -> owner::ServerLifecycle {
        owner::ServerLifecycle { sid, instance_gen: self.client.instance_gen(), token_gen: self.token_gen }
    }
    pub(crate) fn is_current(self, expected: owner::ServerLifecycle) -> bool {
        self.logical(expected.sid) == expected && crate::plex::commit_if_current(
            ServerId::from_raw(expected.sid), self.client, self.token_gen, || ()).is_some()
    }
}

/// Native registry effects executed only by the Session resource adapter, after its borrowed
/// owner permit and (for an endpoint) exact captured Client lifecycle have been validated.
pub(crate) fn execute_session_registry(plan: &owner::RegistryPlan) -> bool {
    match plan {
        owner::RegistryPlan::DevInstall { primary, extras, client_id } => {
            install_captured_registry(&primary.origin(), &primary.token, primary.tier,
                primary.resolve_pin().as_ref(), extras, Some(client_id));
        }
        owner::RegistryPlan::Primary { server, token } => {
            crate::plex::install(&server.origin(), token, server.resolve_pin().as_ref());
        }
        owner::RegistryPlan::Activate { source, ipv6 } => {
            let Some(origin) = source.origin() else { return false };
            let Some(location) = source.tier else { return false };
            apply_candidate_activation(CandidateActivation {
                machine_id: source.machine_id.clone(), token: source.token.clone(),
                name: source.name.clone(), credit: source.shared_by.clone(), owned: source.owned,
                origin, address: source.address.clone(),
                location, ipv6: *ipv6,
            });
        }
        owner::RegistryPlan::Install { sources, primary, replace } => {
            if *replace { crate::plex::revoke_for_profile_switch(); }
            let installed = install_roster(sources, *primary);
            if *replace { crate::plex::finish_profile_switch(&installed); }
        }
        owner::RegistryPlan::Endpoint { expected, source } => {
            let Some(origin) = source.origin() else { return false };
            let id = register_observed_origin(&source.machine_id, &origin, &source.token, source.resolve_pin().as_ref());
            if id.raw() != expected.sid { return false; }
            if let (Some(tier), Some(client)) = (source.tier, crate::plex::client_for(id)) {
                client.set_connection(tier, crate::plex::IpVersion::of_host(&source.address));
            }
            crate::plex::describe_server(id, &source.name, &source.shared_by, source.owned);
            crate::plex::publish_probe_result(id, Outcome::Reachable);
        }
        owner::RegistryPlan::Probe(probe) => publish_settled_probe(probe),
        owner::RegistryPlan::Revoke => crate::plex::revoke_all(),
    }
    true
}

/// Shared resource installer. Dev boot supplies its captured device identity so registration
/// cannot mint/read a session file; Account installation retains the existing lazy-ID behavior.
pub(crate) fn install_captured_registry(origin: &Origin, token: &str, tier: Option<probe::Location>,
    pin: Option<&crate::plex::ResolvePin>, extras: &[SourceRef], client_id: Option<&str>) {
    let register = |machine: &str, origin: &Origin, token: &str, pin: Option<&crate::plex::ResolvePin>| {
        if let Some(cid) = client_id { crate::plex::register_captured_origin(machine, origin, token, pin, cid) }
        else { register_observed_origin(machine, origin, token, pin) }
    };
    let id = register("", origin, token, pin);
    crate::plex::set_current(id);
    if let Some(link) = tier {
        if let Some(client) = crate::plex::client_for(id) {
            client.set_connection(link, crate::plex::IpVersion::of_host(origin.host()));
        }
    }
    for source in extras {
        let Some(origin) = source.origin() else { continue };
        if source.token.is_empty() { continue; }
        let id = register(&source.machine_id, &origin, &source.token, source.resolve_pin().as_ref());
        if let Some(link) = source.tier {
            if let Some(client) = crate::plex::client_for(id) { client.set_link(link); }
        }
        crate::plex::describe_server(id, &source.name, &source.shared_by, source.owned);
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ProfileDelta {
    server: ServerRef,
    sources: Vec<SourceRef>,
    user: UserRef,
    cache: Option<ProfileCreds>,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum ProfileSwitchOutcomeProgress {
    Failed {
        error: String,
        pin_denied: bool,
    },
    Ready {
        delta: ProfileDelta,
        probes: Vec<SettledProbe>,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ProfileSwitchProgress {
    epoch: u64,
    expected: SessionIdentity,
    outcome: ProfileSwitchOutcomeProgress,
}

#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct ProfileRosterProgress {
    epoch: u64,
    expected: SessionIdentity,
    #[serde(with = "observation::resources")]
    resources: Vec<Resource>,
    reached: Vec<SourceRef>,
    probes: Vec<SettledProbe>,
}

/// The one ordered auth stream. `Login` is the already-shipped multi-observation QR protocol;
/// R2A adds the remaining immutable worker observations beside it without changing its variants or
/// terminal ordering.
pub(crate) enum AuthProgress {
    Login(LoginProgress),
    Registry(RegistryProgress),
    HomeRoster(HomeRosterProgress),
    ServerRoster(ServerRosterProgress),
    Endpoint(EndpointProgress),
    ProfileSwitch(ProfileSwitchProgress),
    ProfileRoster(ProfileRosterProgress),
}

impl From<LoginProgress> for AuthProgress {
    fn from(value: LoginProgress) -> Self {
        Self::Login(value)
    }
}

/// One sign-in/discovery fact. Its full epoch travels beside the adapter's exact addressed
/// request and admission identity. The owner revalidates at FIFO head after prior commit ACKs;
/// the worker's cancellation read is only a courtesy, never permission to mutate resources.
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) enum LoginProgress {
    /// The code on screen just died (its own lifetime, or plex.tv answering [`PinPoll::Gone`]) and
    /// [`mint_pin`] is about to replace it. Mirrors the write `mint_pin` used to make directly for
    /// every generation after the first: clear the dead code and flag it replaced before the
    /// successor lands, so no frame can draw digits that no longer authorize anything.
    CodeReplacing { epoch: u64 },
    /// A pin was created and its QR fetched. The owner allocates its checked QR generation on
    /// acceptance, not on the worker, and publishes code/bitmap/generation together.
    CodeReady { epoch: u64, code: String, qr_png: Vec<u8> },
    /// The user authorized on their phone; discovery is starting.
    Authorized { epoch: u64, token: String },
    /// The whole attempt failed for the stated, already-user-facing reason — no server on the
    /// account, discovery unreachable/refused, the pin ran out of automatic replacements, or pin
    /// creation itself could not reach plex.tv. Only the current owner may publish that failure.
    Failed { epoch: u64, message: String },
    /// Discovery and the account's Home-user fetch both finished. Carries everything
    /// the owner's resource commit needs to update the session coherently: the winning
    /// server, the reachable roster, and the Home users (empty for a single-user account, in which
    /// case the flow goes straight to [`Phase::Ready`] instead of raising the picker).
    SignedIn {
        epoch: u64,
        server: ServerRef,
        sources: Vec<SourceRef>,
        users: Vec<UserTile>,
    },
}

/// Register one accepted observed origin. Host tests use the registry's explicit no-I/O seam;
/// shipping builds retain `register_origin`'s server-info refresh and persisted client identity.
fn register_observed_origin(
    machine_id: &str,
    origin: &Origin,
    token: &str,
    pin: Option<&crate::plex::ResolvePin>,
) -> ServerId {
    #[cfg(not(test))]
    {
        crate::plex::register_origin(machine_id, origin, token, pin)
    }
    #[cfg(test)]
    {
        crate::plex::register_pinned_with_client_id(
            machine_id,
            origin,
            token,
            pin,
            "auth-observation-test",
        )
    }
}

fn apply_candidate_activation(candidate: CandidateActivation) {
    let pin = crate::plex::ResolvePin::for_origin(&candidate.origin, &candidate.address);
    let id = register_observed_origin(
        &candidate.machine_id,
        &candidate.origin,
        &candidate.token,
        pin.as_ref(),
    );
    if let Some(client) = crate::plex::client_for(id) {
        client.set_connection(
            candidate.location,
            Some(if candidate.ipv6 {
                crate::plex::IpVersion::V6
            } else {
                crate::plex::IpVersion::V4
            }),
        );
        crate::plex::publish_probe_result(id, Outcome::Reachable);
    }
    crate::plex::describe_server(id, &candidate.name, &candidate.credit, candidate.owned);
}

fn merge_profile_delta(session: &mut Session, delta: ProfileDelta) {
    session.server = delta.server;
    session.sources = delta.sources;
    session.user = delta.user;
    if let Some(cache) = delta.cache {
        session.remember_profile(cache);
    } else {
        session.refresh_profile_record();
    }
}

// ---- worker threads ----

fn output_failed(output: &dyn owner::ObservationSink, epoch: u64, message: &str) {
    output.terminal(LoginProgress::Failed { epoch, message: message.into() }.into());
}

fn login_worker_with_output(epoch: u64, cid: String, output: &dyn owner::ObservationSink) {
    if !output.live() { return; }
    let ac = AccountClient::new(&cid, None);

    // 1) create a pin, and KEEP creating one for as long as this screen is up and the last one
    //    ran out. A pin lives 15 minutes (plex.tv's `expiresIn: 900`); a television left on the
    //    sign-in screen for longer than that used to sit over a code plex.tv had forgotten,
    //    saying "Waiting for you to sign in…" at it. See [`pin_window`].
    let mut generation: u32 = 0;
    let token = loop {
        generation += 1;
        let Some(code) = mint_pin(&ac, epoch, generation, output) else {
            return; // the flow was superseded, or pin creation failed and said so
        };
        // 2) poll until authorized (or the pin dies / the user cancels)
        let mut watch = LivePin {
            ac: &ac,
            id: code.id,
            output,
            started: code.minted,
        };
        match poll_for_token(&mut watch, pin_window(code.expires_in)) {
            PollEnd::Token(t) => break t,
            PollEnd::Superseded => return, // cancelled — whoever superseded us owns the screen
            PollEnd::Expired if another_code_allowed(generation) => {
                log("auth: the sign-in code ran out — minting a fresh one");
            }
            PollEnd::Expired => {
                log("auth: out of automatic sign-in codes — asking the user to start again");
                return output_failed(output, epoch, "Sign-in timed out — try again.");
            }
        }
    };
    log("auth: authorized — discovering server");

    // 3) discover the LAN server. The owner applies Authorized on main; this worker only
    // observes adapter cancellation to avoid wasted IO. It never reads the owner's phase, which
    // may legitimately lag this producer until the admitted observations reach the FIFO head.
    if !output.live() {
        return log("auth: a newer sign-in superseded this one while the pin poll was in flight — token dropped");
    }
    if !output.progress(LoginProgress::Authorized {
        epoch,
        token: token.clone(),
    }.into()) { return; }
    let ac = AccountClient::new(&cid, Some(&token));
    // The failure copy is per outcome, and it used to be one line — "No local Plex server found on
    // this network." — for every one of them. That sentence was the discovery POLICY talking: a
    // server reached over the internet was a failure by construction, so the message named the LAN.
    // It now describes what actually happened, and none of the three sends the user to the wrong
    // place: a token refusal is not a router problem, and an account with no server is not an
    // outage.
    let (server, sources) = match discover_and_store(&ac, epoch, output) {
        Discovery::Ok { server, sources } => (server, sources),
        Discovery::Cancelled => return,
        Discovery::NoServers => return output_failed(output, epoch, "This Plex account has no server yet."),
        Discovery::Refused => {
            return output_failed(output,
                epoch,
                "Your Plex server refused the connection — check its network access settings.",
            )
        }
        Discovery::Silent => {
            return output_failed(output,
                epoch,
                "Couldn't reach any Plex server — check the connection.",
            )
        }
    };
    finish_sign_in(&ac, epoch, server, sources, output);
}

/// How many codes ONE visit to the sign-in screen may burn through before it gives up and offers
/// its own *Try again*.
///
/// Four codes is an hour at plex.tv's 15-minute pins — long enough that walking away mid-sign-in
/// and coming back is not punished, short enough that a television left on this screen overnight
/// does not poll plex.tv until somebody notices. The cap is on CODES rather than on wall-clock
/// time because the pin's own lifetime is the unit the user experiences: what runs out is the
/// thing on screen.
const MAX_PIN_GENERATIONS: u32 = 4;

/// May a flow that has just watched its `generation`-th code run out mint another?
///
/// Pure, because "how many times may this happen automatically" is a policy and the alternative
/// to grading it here is grading it against plex.tv four times. The answer at the ceiling is not a
/// dead end: the flow lands on [`Phase::Error`], which is the one phase the sign-in screen has
/// always drawn a *Try again* on.
fn another_code_allowed(generation: u32) -> bool {
    generation < MAX_PIN_GENERATIONS
}

/// Create a pin, fetch its QR, and publish both as the code on screen.
///
/// `generation` is 1 for the code a fresh sign-in opens with and climbs by one for each
/// replacement. A replacement goes through [`Phase::Creating`] on its way, which is not
/// decoration: that phase is what the login screen already keys "Connecting to Plex…" on, and it
/// is where the dead code is cleared so no frame can draw it while its successor is being minted.
///
/// `None` means "stop": either the flow was superseded (silent — the successor owns the screen) or
/// creation failed and has already said so on the error read-out.
fn mint_pin(ac: &AccountClient, epoch: u64, generation: u32,
    output: &dyn owner::ObservationSink) -> Option<MintedCode> {
    if !output.live() { return None; }
    if generation > 1 {
        // Liveness only, no write — see the section doc above [`login_thread`]. This is the same
        // "stop wasting plex.tv calls on a dead flow" courtesy the old synchronous check made:
        // without it, `ac.create_pin()` below would still burn a network round trip minting a code
        // nobody is left to scan.
        if !output.live() {
            return None;
        }
        if !output.progress(LoginProgress::CodeReplacing { epoch }.into()) { return None; }
    }
    let pin = match ac.create_pin() {
        Some(p) if p.id != 0 && !p.code.is_empty() => p,
        _ => {
            // Says what the internet is FOR here, because the one time this screen appears
            // with the link deliberately down is the first boot of a set that has never signed
            // in — and that person needs to know the app works offline once it has.
            output_failed(output,
                epoch,
                "Couldn't reach Plex — check the connection. Signing in needs the internet once.",
            );
            return None;
        }
    };
    // **The lease starts HERE, not where the polling does.** plex.tv began counting the moment it
    // answered, and the QR fetch below is another request on `net::API`'s 25 s deadline — so a
    // clock started after it would let the poll run that much past the code's real death, which is
    // the same over-run in miniature that this whole change is about.
    let minted = Instant::now();
    // Neither the id nor the code may be logged. `GET /api/v2/pins/{id}` is what RETURNS the
    // account token once the user authorizes (plex/account.rs `poll_pin`), so the id is a handle
    // that redeems a credential, and the code is what authorizes it — and this file is the one we
    // ask users to send us when something goes wrong. Log that we got here, not what we got.
    log(&format!(
        "auth: pin created (code {generation} of {MAX_PIN_GENERATIONS}, {}s to authorize)",
        pin_window(pin.expires_in).as_secs()
    ));
    // fetch the server-rendered QR PNG (the exact QR the official apps display); public, no token.
    let qr_url = if pin.qr.is_empty() {
        format!("https://plex.tv/api/v2/pins/qr/{}", pin.code)
    } else {
        pin.qr.clone()
    };
    if !output.live() { return None; }
    let qr_png = crate::net::https_get_public(&qr_url)
        .filter(|r| r.ok())
        .map(|r| r.body)
        .unwrap_or_default();
    log(&format!("auth: qr png {} bytes", qr_png.len()));
    // Same liveness-only check as above, and the same reason: no point publishing a code the flow
    // this worker belongs to no longer exists to show. The owner's checked QR allocator runs
    // only on accepted CodeReady, not here on the producer.
    if !output.live() {
        return None;
    }
    if !output.progress(LoginProgress::CodeReady {
        epoch,
        code: pin.code.clone(),
        qr_png,
    }.into()) { return None; }
    Some(MintedCode {
        id: pin.id,
        expires_in: pin.expires_in,
        minted,
    })
}

/// What [`login_thread`] needs to know about the code it just put on screen: the handle to poll,
/// how long plex.tv will honour it, and **when that clock started**.
struct MintedCode {
    id: i64,
    expires_in: i64,
    minted: Instant,
}

/// Finish a successful discovery. Shared by the QR flow and the discovery-only Retry path.
///
/// `server`/`sources` are discovery's own result, passed directly rather than reread from Session.
fn finish_sign_in(ac: &AccountClient, epoch: u64, server: ServerRef, sources: Vec<SourceRef>,
    output: &dyn owner::ObservationSink) {
    if !output.live() { return; }
    // Discovery already queued its activation observations before this SignedIn observation.
    // Only owner-accepted resource effects install clients; the historical "installed" log
    // label below describes the observed result, not proof of main-thread commit completion.
    // `log_form`, not `base()`: byte-identical to the `{addr}:{port}` this line always printed
    // for a plaintext origin (so an archived log stays comparable), and the whole URL as soon as
    // the scheme is worth saying. See `Origin::log_form`.
    log(&format!(
        "auth: PMS client installed {}",
        server.origin().log_form()
    ));

    // 4) Plex Home roster → who's-watching, or straight in if there's a single user. The roster is
    // kept on the session so it persists with the creds — the boot picker and every later
    // "Change profile" render from it instantly, online or not.
    let users: Vec<UserTile> = ac
        .home_users()
        .unwrap_or_default()
        .iter()
        .map(UserTile::of)
        .collect();
    log(&format!("auth: home users n={}", users.len()));
    // One observation carrying everything the owner needs to commit the session at once —
    // see [`LoginProgress::SignedIn`] for why this used to be three separate `with_ctl` writes and
    // is now one.
    output.terminal(LoginProgress::SignedIn {
        epoch,
        server,
        sources,
        users,
    }.into());
}

fn rediscovery_worker_with_output(cid: String, token: String, epoch: u64,
    output: &dyn owner::ObservationSink) {
    if !output.live() { return; }
    let ac = AccountClient::new(&cid, Some(&token));
    match discover_and_store(&ac, epoch, output) {
        Discovery::Ok { server, sources } => finish_sign_in(&ac, epoch, server, sources, output),
        Discovery::Cancelled => {}
        Discovery::NoServers => output_failed(output, epoch, "This Plex account has no server yet."),
        Discovery::Refused => output_failed(output,
            epoch,
            "Your Plex server refused the connection — check its network access settings.",
        ),
        Discovery::Silent => output_failed(output,
            epoch,
            "Couldn't reach any Plex server — check the connection.",
        ),
    }
}

/// How one publication of a QR code ended.
#[derive(Debug, PartialEq, Eq)]
enum PollEnd {
    /// The user authorized on their phone and plex.tv handed over the account token.
    Token(String),
    /// This code is finished — plex.tv says so, or its own lifetime ran out. There is nothing
    /// left to wait for and the caller must mint another.
    Expired,
    /// A newer flow owns the sign-in, or the screen left [`Phase::Waiting`]. Say nothing.
    Superseded,
}

/// How long one QR code may be waited on, from the pin's own `expiresIn`.
///
/// **A WALL-CLOCK bound, and that is the half of issue #30 no log could show.** What this replaced
/// counted ITERATIONS — 450 of them for the `expiresIn: 900` plex.tv actually answers with — while
/// each iteration cost a 2 s sleep PLUS one HTTPS round trip whose own deadline is `net::API`'s
/// 25 s. So the screen said "Waiting for you to sign in…" for somewhere between 17 minutes and
/// three and a half hours over a pin that had stopped existing after fifteen, and every poll in
/// that tail was answered `404 {"code":1020,"message":"Code not found or expired"}` — which the
/// old `Option<Pin>` return could not express, so it read as "not authorized yet". Counting the
/// wait in seconds makes the window mean what its name says.
///
/// The floor covers a plex.tv that omits the field (or sends a nonsense one); the ceiling is this
/// app's own patience for a single code.
fn pin_window(expires_in: i64) -> Duration {
    Duration::from_secs(expires_in.clamp(60, 1800) as u64)
}

/// The pause before the next poll, after `misses` consecutive answers that told us nothing.
///
/// A steady 2 s while plex.tv is answering — the cadence this flow has always had, and the one the
/// user's phone tap is judged by, so a healthy sign-in is not made slower by any of this. A
/// transport failure is a different matter: retrying it at the same rate hammers a network that
/// has already said it is unhappy, so consecutive misses back off geometrically. The ceiling is
/// low on purpose — the pin has a deadline, and a backoff that grew past it would spend the
/// window asleep and miss an authorization that did arrive.
fn poll_delay(misses: u32) -> Duration {
    const BASE_MS: u64 = 2_000;
    const CEILING_MS: u64 = 16_000;
    Duration::from_millis((BASE_MS << misses.min(8)).min(CEILING_MS))
}

/// Everything [`poll_for_token`] needs from the world: one network answer, one interruptible
/// wait, and a clock.
///
/// It is a trait for one reason — the loop underneath is the part of the sign-in that went wrong,
/// and a loop built out of `thread::sleep` and `Instant::now` can only be graded by a test that
/// waits in real time, which is to say it is never graded. A scripted implementation lets a host
/// test run a fifteen-minute pin to its death in microseconds.
trait PinWatch {
    /// Ask plex.tv about this pin.
    fn poll(&mut self) -> PinPoll;
    /// Wait up to `d`. `false` means the flow was superseded meanwhile — stop, say nothing.
    fn wait(&mut self, d: Duration) -> bool;
    /// How long this code has been on screen.
    fn elapsed(&self) -> Duration;
}

/// The real one: a live pin, the wall clock, and the flow's epoch.
struct LivePin<'a> {
    ac: &'a AccountClient,
    id: i64,
    output: &'a dyn owner::ObservationSink,
    started: Instant,
}

impl PinWatch for LivePin<'_> {
    fn poll(&mut self) -> PinPoll {
        self.ac.poll_pin(self.id)
    }
    fn wait(&mut self, d: Duration) -> bool {
        // SLICED, so a cancel is noticed within a slice however far the backoff has grown. The
        // worker's answer would be discarded anyway, but a thread that lingers for the whole of a
        // 16 s backoff after the user has left the screen is a thread the next flow shares the
        // device with.
        const SLICE: Duration = Duration::from_secs(1);
        let deadline = Instant::now() + d;
        loop {
            if !self.output.live() {
                return false;
            }
            let now = Instant::now();
            if now >= deadline {
                return true;
            }
            std::thread::sleep((deadline - now).min(SLICE));
        }
    }
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Poll `/pins/{id}` until the user authorizes, the code dies, or the flow is superseded.
///
/// **A transport failure is not an ending.** It never was — the loop this replaced also carried on
/// — but it was also never SAID, in the log or in the cadence, so "the app stopped polling" and
/// "plex.tv stopped answering" produced identical evidence. Now a miss backs off, says so once,
/// and says when the answers come back; and the one answer that really is an ending, a pin plex.tv
/// no longer knows, ends the wait immediately instead of being retried for the rest of the window.
fn poll_for_token(w: &mut impl PinWatch, window: Duration) -> PollEnd {
    let mut misses: u32 = 0;
    loop {
        // **The wait never runs past the deadline, and the deadline never cancels a poll.** Both
        // halves are one bug found in review, and it is the bug this whole change exists to stop:
        // at t=889s a miss sets the backoff to 16s, the user authorizes at 895s and their phone
        // says *Account linked* — and a loop that checked the clock before polling would declare
        // expiry at 905s and throw away a token that was sitting there. So the pause is clamped to
        // what is left of the code, and the poll after it always happens. Only plex.tv gets to say
        // a pin is finished before we have asked it once more.
        let pause = poll_delay(misses).min(window.saturating_sub(w.elapsed()));
        if !w.wait(pause) {
            return PollEnd::Superseded;
        }
        match w.poll() {
            PinPoll::Authorized(t) => return PollEnd::Token(t),
            PinPoll::Pending => {
                if misses > 0 {
                    log("auth: plex.tv is answering again — still waiting for authorization");
                }
                misses = 0;
            }
            PinPoll::Gone => {
                log("auth: plex.tv no longer knows this sign-in code — expired or already used");
                return PollEnd::Expired;
            }
            PinPoll::Unreachable => {
                misses = misses.saturating_add(1);
                // Once when it starts, and rarely after, because this line is written every two
                // seconds by an app whose event log is truncated at every launch.
                if misses == 1 || misses % 15 == 0 {
                    log(&format!(
                        "auth: sign-in poll unanswered n={misses} — still waiting, backing off"
                    ));
                }
            }
        }
        // **After the poll, never before it, and exactly once.** Before it, a wait that crossed
        // the deadline would cancel a request the code was still alive for — the token-losing bug
        // above. Asked after it, the clock also accounts for what the REQUEST cost: a poll that
        // starts at 899 s and runs to `net::API`'s 25 s deadline has taken us past the end, and
        // issuing a second one (which a flag computed before the poll would have done) only delays
        // the replacement code by another 25 s. Every pause is clamped to what is left, so exactly
        // one poll can ever begin before the deadline and finish after it, and that one is always
        // allowed to answer.
        if w.elapsed() >= window {
            log("auth: the sign-in code reached the end of its life unused");
            return PollEnd::Expired;
        }
    }
}

// ---- server discovery ----
//
// **Every server the account can reach, not the first one that looks local.** What this replaced
// filtered both of its passes on `c.local && !c.relay`, kept exactly one server, and threw the rest
// of the account away. Against a real share that filter is worse than useless: `Connection.local`
// means "this address is RFC1918", not "you are on that LAN", so it selected the OWNER's
// `172.20.x.x` — 8 s of timeout from here, and the *worse* outcome is that it succeeds against
// somebody else's box at that address on our own LAN (`docs/shared-servers.md` §2a).
//
// So the shape is: ranked candidates from `plex::probe` (pure policy keeps only identity-safe
// forms of an unmatched shared-LAN address), race one server's direct candidates, and **verify
// identity on the answer** before believing it. Servers remain serial, with relay as a second phase.

/// How far one server got. Only [`Reach::At`] is a server we can use; the other two are the
/// distinction `probe.rs`'s module doc refuses to let a caller collapse, because they send the
/// user to two different places.
enum Reach {
    /// This address answered `/identity` **as the server we asked for**.
    ///
    /// Two values, and the split is the point: the [`Origin`] is **what was actually dialled**, and
    /// so the only thing the roster may record as this server's address; the [`Candidate`] is kept
    /// beside it for the DIAGNOSTIC fields (`address`, `port`) that the log and the Sources panel
    /// say. Deriving the record from `Candidate::url` while the dial came from `Candidate::address`
    /// left exactly one gap — a plex.tv `uri` whose port disagrees with `port` would be verified at
    /// one and written down as the other — and this pairing closes it by construction.
    At(Candidate, Origin),
    /// One or more candidates answered 401 and no candidate verified the server. A proxy-specific
    /// 401 does not cancel parallel direct probes or the relay fallback; it survives only as the
    /// final reason when none of those proves reachability. Reporting that as generic silence would
    /// send the user to the router for an authorization/access-policy problem.
    Refused,
    /// Nothing answered as this server.
    No,
}

/// What discovery concluded. Three outcomes rather than a bool, because "this account owns no
/// server", "your servers are silent" and "a server answered and refused us" are three different
/// things to tell a user, and only the middle one is about the network.
enum Discovery {
    /// The winning primary and the reachable roster. Carried on the variant rather than left for
    /// the caller to re-read out of `Ctl` — since phase 6, `discover_and_store` no longer writes
    /// `Ctl` at all (see [`LoginProgress`]), so this is now the ONLY way `finish_sign_in` learns
    /// what discovery found.
    Ok {
        server: ServerRef,
        sources: Vec<SourceRef>,
    },
    /// Superseded while network work was in flight. Silent: the newer flow owns the UI/session.
    Cancelled,
    /// `/api/v2/resources` named no server at all. NOT the case where it could not be fetched —
    /// that is [`Discovery::Silent`], because a request that never arrived says nothing about what
    /// the account owns.
    NoServers,
    /// Servers exist; none of them answered (or plex.tv itself did not).
    Silent,
    /// At least one answered **401**, and none was reachable. Something in front of that server
    /// refuses unauthenticated requests — an auth proxy, or `allowedNetworks` excluding this
    /// subnet. It is not a network fault and not a dead server, so it must not be worded as one.
    Refused,
}

/// The probe path. **Unauthenticated on purpose** — `/identity` answers 200 to anybody, which
/// makes it useless as a token test and perfect as a reachability + identity one.
///
/// The token is deliberately NOT sent. A probe can land on a *different machine* (that is rule 1
/// of `probe.rs`, and the reason identity is verified at all), and a request that carried the
/// per-(user, server) token would hand that stranger a live credential before we had any reason to
/// believe who they are.
///
/// **So discovery does not, and cannot, prove the token works.** A per-(user, server) grant revoked
/// between the `/api/v2/resources` fetch and now still probes as [`Outcome::Reachable`] here — the
/// server really is reachable; it is the credential that is dead, and this request never shows it
/// one. That 401 surfaces on the first AUTHENTICATED request instead, where the answer is to refetch
/// `/api/v2/resources` (`probe.rs`'s module doc) rather than to look for a network fault. The
/// [`Outcome::Unauthorized`] arm below is not dead code for that: a PMS behind an auth proxy, or one
/// whose `allowedNetworks` refuses this subnet, answers 401 to the probe itself, and *that* must not
/// be reported as an unreachable address.
const IDENTITY: &str = "/identity";

/// Can this app's transport dial that candidate? **Every one of them, now** — see [`dial_target`],
/// which this is the boolean face of.
///
/// It used to be the narrowest predicate in the app: plain HTTP at a dotted quad and nothing else,
/// because `stream.rs` was the only transport there was. Every https `plex.direct` origin and every
/// hostname was "unspoken" rather than unreachable — a true distinction, and no comfort at all to
/// an account signed in from anywhere but the server's own LAN, which had nothing left to dial.
/// That was the dead end this one predicate was responsible for.
#[cfg(test)]
fn dialable(c: &Candidate) -> bool {
    dial_target(c).is_some()
}

/// [`dialable`] and the ORIGIN to dial, from one expression — so the predicate that admits a
/// candidate and the value handed to the transport can never disagree.
///
/// **It is [`Candidate::origin`] and nothing else now**, and the emptiness is the achievement. Two
/// separate narrowings used to live in this function, one per gap in the transport, and they were
/// closed by two different pieces of work:
///
/// * *No TLS* — every `https://` candidate was skipped, which is every `plex.direct` uri plex.tv
///   advertises. `crate::http` closed that one by routing an https origin through libcurl.
/// * *No resolver, no IPv6* — a plaintext candidate had to be four decimal octets, because
///   `http_open` built a `sockaddr_in` by hand. `stream.rs` closed that one with `getaddrinfo` and
///   a walk down the whole resolved chain, so a name and a v6 literal are both ordinary now.
///
/// What survives is the port narrowing, and it survives *inside* [`Origin::parse`]: an out-of-range
/// `i64` from plex.tv is refused by [`probe::dial_port`] rather than wrapped by `as i32` into a
/// plausible-looking 32400 (that function's doc has the arithmetic). A candidate refused there is
/// a connection this client cannot open, not a server that failed to answer, so it is skipped and
/// the next address gets its turn.
fn dial_target(c: &Candidate) -> Option<Origin> {
    c.origin()
}

/// The `machineIdentifier` in an `/identity` body, read out of **either** encoding.
///
/// PMS answers JSON only for an explicit `Accept: application/json` and XML for anything else
/// (`plex/CLAUDE.md`), and a probe is exactly the request most likely to meet a proxy, a cache or
/// an older build that ignores the header — so the one field that decides whether we trust the
/// connection is scanned for rather than deserialized. The two forms differ only in the
/// punctuation between the name and the value: `"machineIdentifier":"abc"` and
/// `machineIdentifier="abc"`.
fn machine_id_in(body: &[u8]) -> Option<String> {
    const NAME: &[u8] = b"machineIdentifier";
    let after = body.windows(NAME.len()).position(|w| w == NAME)? + NAME.len();
    let rest = &body[after..];
    let start = rest
        .iter()
        .position(|b| !matches!(b, b'"' | b':' | b'=' | b' ' | b'\t' | b'\r' | b'\n'))?;
    let rest = &rest[start..];
    let end = rest
        .iter()
        .position(|b| matches!(b, b'"' | b'\'' | b'<' | b',' | b'}' | b' '))
        .unwrap_or(rest.len());
    let v = &rest[..end];
    (!v.is_empty()).then(|| String::from_utf8_lossy(v).into_owned())
}

/// Turn one probe response into the outcome the caller must not collapse. Pure, so the acceptance
/// policy is gradeable on the dev Mac — which is the only tier that can grade it, since the
/// failures it prevents are "a stranger's server answered" and "a token problem reported as a dead
/// router".
fn classify(status: i32, body: &[u8], want_machine_id: &str) -> Outcome {
    if status == 401 {
        // 401 ONLY. PMS refuses a credential with 401; a 403 is an endpoint saying "not for you"
        // (the owner-only surfaces), which is not something re-fetching `/resources` can fix.
        return Outcome::Unauthorized;
    }
    if !(200..300).contains(&status) {
        return Outcome::Unreachable;
    }
    if want_machine_id.is_empty() {
        // Nothing to verify against, so nothing is verified. plex.tv sent a resource with no
        // `clientIdentifier`; accepting whatever answered would be accepting an unnamed machine.
        return Outcome::WrongServer;
    }
    match machine_id_in(body) {
        Some(id) if id == want_machine_id => Outcome::Reachable,
        _ => Outcome::WrongServer,
    }
}

/// One unauthenticated `GET {origin}/identity`, as (status, body).
///
/// Goes through [`crate::http`], which is what makes this ONE function able to probe both a
/// plaintext LAN address and an `https://…plex.direct` name: the dispatch is on the origin's
/// scheme, and every candidate `dial_target` admits carries the transport it needs in that field.
/// It hand-rolled the socket before, which is also why it could only ever probe the first kind.
///
/// The STATUS is half the answer, which is why this cannot be a `stream::http_get`: that wrapper
/// folds every non-2xx into `None`, and folding is precisely the collapse of 401 into "unreachable"
/// that this module exists to avoid. [`crate::http::Reply`] carries both halves over either
/// transport.
///
/// A transport failure — nothing answered, DNS said no, the certificate would not validate — comes
/// back as `(0, [])`, and `classify` reads that as [`Outcome::Unreachable`]. `0` is not a status any
/// server can send, so it cannot be confused with one.
fn get_identity(origin: &Origin, budget: Duration) -> (i32, Vec<u8>) {
    match crate::http::request_probe(
        origin,
        IDENTITY,
        crate::http::Method::Get,
        &[crate::http::ACCEPT_JSON],
        64 * 1024,
        budget.as_secs().max(1) as i32,
    ) {
        // `/identity` is one small MediaContainer. The ceiling is enforced by each transport
        // WHILE it reads, before a machine we have not accepted can make this worker allocate an
        // unbounded body; an over-limit answer is therefore a transport failure, never a prefix
        // that might happen to contain a plausible machine id.
        Some(r) => (r.status, r.body),
        None => (0, Vec::new()),
    }
}

/// Candidate probing deadlines belong here, where the connection tier is known. They are
/// deliberately not transport settings: ordinary PMS requests and media reads have different
/// timeout contracts, while discovery alone distinguishes a local path from a remote one.
#[derive(Clone, Copy)]
struct ProbeDeadlines {
    local: Duration,
    remote: Duration,
}

const PROBE_DEADLINES: ProbeDeadlines = ProbeDeadlines {
    local: Duration::from_secs(5),
    remote: Duration::from_secs(10),
};
const SERVER_GAP: Duration = Duration::from_secs(4);

type ProbeDial = Arc<dyn Fn(&Origin, Duration) -> (i32, Vec<u8>) + Send + Sync + 'static>;
type ProbeJob = Box<dyn FnOnce() + Send + 'static>;

#[derive(Clone)]
struct Winner {
    index: usize,
    candidate: Candidate,
    origin: Origin,
    score: i32,
}

struct ProbeMessage {
    index: usize,
    on_time: bool,
    outcome: Outcome,
}

const PROBE_PENDING: u8 = 0;
const PROBE_COMPLETED: u8 = 1;
const PROBE_EXPIRED: u8 = 2;

#[derive(Clone)]
struct PendingProbe {
    deadline: Instant,
    state: Arc<AtomicU8>,
}

#[derive(Default)]
struct BatchResult {
    first: Option<Winner>,
    best: Option<Winner>,
    refused: bool,
}

fn probe_deadline(c: &Candidate, policy: ProbeDeadlines) -> Duration {
    if c.location == probe::Location::Local {
        policy.local
    } else {
        policy.remote
    }
}

fn loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|a| a.is_loopback())
}

/// The official client's additive candidate score. `+6 reachable` is included here even though
/// this function is called only for a reachable answer, so the code remains a literal rendering
/// of the contract rather than a relative shorthand that can drift when another term is added.
fn candidate_score(c: &Candidate, origin: &Origin) -> i32 {
    6 + if loopback_host(&c.address) || loopback_host(origin.host()) {
        3
    } else {
        0
    } + if c.location == probe::Location::Local {
        2
    } else {
        0
    } + if c.scheme == probe::Scheme::Https {
        1
    } else {
        0
    } - if c.location == probe::Location::Relay {
        1
    } else {
        0
    }
}

fn better(a: &Winner, b: &Winner) -> bool {
    a.score > b.score || (a.score == b.score && a.index < b.index)
}

fn settle_probe_message(
    plan: &ProbePlan,
    message: ProbeMessage,
    pending: &mut [Option<PendingProbe>],
    live: &mut usize,
    result: &mut BatchResult,
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
) {
    let Some(_pending) = pending.get_mut(message.index).and_then(Option::take) else {
        return; // expired or already settled: late/duplicate messages are inert
    };
    *live -= 1;
    if !message.on_time {
        return;
    }
    let c = &plan.candidates[message.index];
    match message.outcome {
        Outcome::Reachable => {
            let Some(origin) = dial_target(c) else { return };
            let winner = Winner {
                index: message.index,
                score: candidate_score(c, &origin),
                candidate: c.clone(),
                origin,
            };
            if result.first.is_none() {
                // "First usable immediately" — and USABLE is the word: on a LAN the plaintext
                // twin answers before the TLS handshake completes, and a store build refuses
                // to put a token on it, so activating it re-pointed the live server to an
                // origin every request then failed on until the https winner landed ~100 ms
                // later (device, 2026-09-06: `security: refused plaintext PMS credentials`,
                // a hub fetch and the picker's first avatar lost in the gap). The first answer
                // still counts as reached; it just does not become the live origin unless this
                // build can dial it with a credential.
                if activation_allowed(&winner.origin) {
                    activate(plan, &winner.candidate, &winner.origin);
                }
                result.first = Some(winner.clone());
            }
            if result.best.as_ref().is_none_or(|old| better(&winner, old)) {
                result.best = Some(winner);
            }
        }
        Outcome::Unauthorized => {
            result.refused = true;
            log(&format!(
                "auth: '{}' answered 401 at {} — a token problem, not the network",
                plan.name, c.address
            ));
        }
        Outcome::WrongServer => log(&format!(
            "auth: '{}' — {}:{} answered as a DIFFERENT machine",
            plan.name, c.address, c.port
        )),
        Outcome::Unreachable => {}
    }
}

/// Race one phase of a server's candidates. The spawner is injected because refusal is a result
/// the coordinator must settle, not an exceptional path a unit test can reach through real OS
/// exhaustion. Only a successful spawn creates a pending entry. Each entry owns an absolute
/// deadline; expiring one local worker never settles a still-live remote worker.
fn race_batch(
    plan: &ProbePlan,
    indices: &[usize],
    dial: ProbeDial,
    spawn: &dyn Fn(usize, ProbeJob) -> bool,
    policy: ProbeDeadlines,
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
) -> BatchResult {
    let (tx, rx) = mpsc::channel::<ProbeMessage>();
    let mut pending = vec![None; plan.candidates.len()];
    let mut live = 0usize;

    for &index in indices {
        let c = &plan.candidates[index];
        let Some(origin) = dial_target(c) else {
            continue;
        };
        let started = Instant::now();
        let deadline = started + probe_deadline(c, policy);
        let state = Arc::new(AtomicU8::new(PROBE_PENDING));
        let tx = tx.clone();
        let dial = Arc::clone(&dial);
        let worker_state = Arc::clone(&state);
        let machine_id = plan.machine_id.clone();
        let budget = probe_deadline(c, policy);
        let job = Box::new(move || {
            let (status, body) = dial(&origin, budget);
            let outcome = classify(status, &body, &machine_id);
            let on_time = Instant::now() <= deadline;
            // Claim completion before publishing the message. If the coordinator expires first,
            // this result is inert. If this claim wins and the worker is descheduled before send,
            // the coordinator sees COMPLETED and waits for the already-decided result rather than
            // erasing it on its own later wall-clock sample.
            if worker_state
                .compare_exchange(
                    PROBE_PENDING,
                    PROBE_COMPLETED,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                let _ = tx.send(ProbeMessage {
                    index,
                    on_time,
                    outcome,
                });
            }
        });
        if spawn(index, job) {
            pending[index] = Some(PendingProbe { deadline, state });
            live += 1;
        }
    }
    // Only worker-held senders remain. If one panics (or an injected spawner accepts then drops its
    // job), disconnect settles the remaining pending set instead of parking the coordinator.
    drop(tx);

    let mut result = BatchResult::default();
    while live > 0 {
        // Drain results that completed on time BEFORE expiring by the coordinator's current clock.
        // Spawn setup and queue backlog are allowed to delay observation; `finished` is the fact
        // that decides whether the candidate met its own absolute deadline.
        loop {
            match rx.try_recv() {
                Ok(message) => settle_probe_message(
                    plan,
                    message,
                    &mut pending,
                    &mut live,
                    &mut result,
                    activate,
                ),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if live > 0 {
                        live = 0;
                        pending.fill(None);
                    }
                    break;
                }
            }
        }
        if live == 0 {
            break;
        }
        let now = Instant::now();
        for &index in indices {
            let expired = pending[index].as_ref().is_some_and(|p| {
                p.deadline <= now
                    && p.state
                        .compare_exchange(
                            PROBE_PENDING,
                            PROBE_EXPIRED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
            });
            if expired {
                pending[index] = None;
                live -= 1;
                let c = &plan.candidates[index];
                log(&format!(
                    "auth: '{}' probe timed out at {}:{}",
                    plan.name, c.address, c.port
                ));
            }
        }
        if live == 0 {
            break;
        }
        let next = indices
            .iter()
            .filter_map(|&i| pending[i].as_ref())
            .filter(|p| p.state.load(Ordering::Acquire) == PROBE_PENDING)
            .map(|p| p.deadline)
            .min();
        let received = match next {
            Some(next) => rx.recv_timeout(next.saturating_duration_since(Instant::now())),
            // Every live worker has already claimed completion and owes exactly one message.
            // Blocking here avoids a zero-timeout spin in the tiny claim-before-send window.
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        match received {
            Ok(message) => settle_probe_message(
                plan,
                message,
                &mut pending,
                &mut live,
                &mut result,
                activate,
            ),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Every sender is gone, so no pending candidate can ever report. This includes a
                // worker panic and an injected accepted-but-dropped job.
                live = 0;
                for slot in pending.iter_mut() {
                    *slot = None;
                }
            }
        }
    }
    result
}

/// Parallel within one server, with relay held out until every direct candidate has settled.
/// The coordinator alone activates: first usable immediately, then at most one re-point to the
/// final best score. Workers only dial, classify and send a message.
fn probe_server_racing(
    plan: &ProbePlan,
    dial: ProbeDial,
    spawn: &dyn Fn(usize, ProbeJob) -> bool,
    policy: ProbeDeadlines,
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
) -> Reach {
    let direct: Vec<usize> = plan
        .candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| (c.location != probe::Location::Relay).then_some(i))
        .collect();
    let relay: Vec<usize> = plan
        .candidates
        .iter()
        .enumerate()
        .filter_map(|(i, c)| (c.location == probe::Location::Relay).then_some(i))
        .collect();

    let mut batch = race_batch(plan, &direct, Arc::clone(&dial), spawn, policy, activate);
    // Relay is the reachability fallback whenever no direct origin verified, including when a
    // proxy on one direct origin answered 401. Preserve that refusal only as the final reason if
    // relay also produces no winner; a verified identity always beats a parallel/proxy 401.
    if batch.first.is_none() && !relay.is_empty() {
        let direct_refused = batch.refused;
        batch = race_batch(plan, &relay, dial, spawn, policy, activate);
        batch.refused |= direct_refused;
    }

    let Some(best) = batch.best else {
        return if batch.refused {
            Reach::Refused
        } else {
            Reach::No
        };
    };
    let first = batch
        .first
        .as_ref()
        .expect("a best winner is also a first winner");
    // The final re-point to the best score — or the first activation of the best, when the
    // first answer was one this build could not make live (see `settle_probe_message`).
    if (first.index != best.index || !activation_allowed(&first.origin))
        && activation_allowed(&best.origin)
    {
        activate(plan, &best.candidate, &best.origin);
    }
    Reach::At(best.candidate, best.origin)
}

/// May this origin become the LIVE one — can the app put a credential on it in this build?
/// TLS always; plaintext only in a developer build (`http::credential_transport_allowed`'s rule,
/// asked before a registration instead of after a refused request).
fn activation_allowed(origin: &Origin) -> bool {
    activation_allowed_by_policy(origin, cfg!(feature = "devtriggers"))
}

fn activation_allowed_by_policy(origin: &Origin, allow_plaintext_credentials: bool) -> bool {
    crate::http::credential_transport_allowed_by_policy(
        origin,
        "/",
        &["X-Plex-Token: any"],
        allow_plaintext_credentials,
    )
}

fn candidate_activation(
    plan: &ProbePlan,
    c: &Candidate,
    origin: &Origin,
    credit: &str,
) -> CandidateActivation {
    CandidateActivation {
        machine_id: plan.machine_id.clone(),
        token: plan.token.clone(),
        name: plan.name.clone(),
        credit: credit.to_owned(),
        owned: plan.owned,
        origin: origin.clone(),
        address: c.address.clone(),
        location: c.location,
        ipv6: c.ipv6,
    }
}

/// Publish a completed server race onto the already-registered slot for that machine. A newly
/// granted server that never verified an address has no slot yet and is deliberately ignored:
/// probe failure is not authority to register an unverified endpoint. A retained/offline source,
/// however, is already registered from its cached verified origin and receives the new state.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct SettledProbe {
    machine_id: String,
    #[serde(with = "observation::outcome")]
    outcome: Outcome,
    tier: Option<probe::Location>,
}

pub(crate) fn settled_probe(
    plan: &ProbePlan,
    outcome: Outcome,
    tier: Option<probe::Location>,
) -> SettledProbe {
    SettledProbe {
        machine_id: plan.machine_id.clone(),
        outcome,
        tier,
    }
}

fn publish_settled_probe(probe: &SettledProbe) {
    let Some((id, client)) = crate::plex::server_ids()
        .filter_map(|id| crate::plex::client_for(id).map(|client| (id, client)))
        .find(|(_, client)| client.machine_id() == probe.machine_id)
    else {
        return;
    };
    if let Some(link) = probe.tier {
        client.set_link(link);
    }
    crate::plex::publish_probe_result(id, probe.outcome);
}

#[cfg(test)]
fn publish_settled_probes(probes: &[SettledProbe]) {
    for probe in probes {
        publish_settled_probe(probe);
    }
}

/// Legacy synchronous seam for the older acceptance fixtures. Production uses
/// [`probe_server_racing`]; these tests still exercise identity mismatch, 401 and roster-recording
/// semantics without timing or worker scheduling in their assertions.
#[cfg(test)]
fn probe_server(plan: &ProbePlan, dial: &dyn Fn(&Origin) -> (i32, Vec<u8>)) -> Reach {
    let mut tried = 0;
    for c in plan.candidates.iter() {
        let Some(origin) = dial_target(c) else {
            continue;
        };
        tried += 1;
        // The ORIGIN, whole — the same value handed back in `Reach::At`, so what answered and what
        // the roster records cannot be two different things. It is passed rather than split into
        // `(host, port)` because the SCHEME is now part of what gets dialled: splitting it here
        // would put the transport choice back at a call site.
        let (status, body) = dial(&origin);
        match classify(status, &body, &plan.machine_id) {
            Outcome::Reachable => return Reach::At(c.clone(), origin),
            Outcome::Unauthorized => {
                log(&format!(
                    "auth: '{}' answered 401 at {} — a token problem, not the network",
                    plan.name, c.address
                ));
                return Reach::Refused;
            }
            Outcome::WrongServer => {
                // Rule 1, live: something answered and it is not this server. Discarded, never
                // retried — and never registered, which is the point of verifying at all.
                log(&format!(
                    "auth: '{}' — {}:{} answered as a DIFFERENT machine",
                    plan.name, c.address, c.port
                ));
            }
            Outcome::Unreachable => {}
        }
    }
    let skipped = plan.candidates.len() - tried;
    log(&format!(
        "auth: '{}' did not answer ({tried} address(es) tried, {skipped} not dialable)",
        plan.name
    ));
    Reach::No
}

/// What probing a whole `/api/v2/resources` response came to.
enum Resolved {
    /// The response named no server at all — nothing was dialled, and this is a fact about the
    /// account rather than about the network.
    NoServers,
    /// Servers were probed and none was accepted. `refused` distinguishes "at least one answered
    /// 401" from "silence", which are two different things to tell the user.
    None { refused: bool },
    /// The roster, **ours first**, each entry carrying the address that actually answered.
    Reached(Vec<SourceRef>),
}

/// The whole of discovery except the two impure edges — fetching `/resources` and holding a socket.
///
/// Everything that decides what the app ends up talking to lives here: which servers are tried and
/// in what order, which of a server's addresses is accepted, and what is written down about it. It
/// takes the response and a `dial`, so a full sign-in against a two-server account is a host test
/// rather than a screenshot — which matters because this function is the gate on the whole feature:
/// register the wrong connection and no other unit's work is reachable, however correct it is.
///
/// `household` is [`session::Session::household_ids`] — see [`credit_of`].
fn resolve_roster_using(
    resources: &[Resource],
    household: &[i64],
    probe_one: &mut dyn FnMut(&ProbePlan) -> Reach,
    between_servers: &mut dyn FnMut(),
    observe: &mut dyn FnMut(&ProbePlan, Outcome, Option<probe::Location>),
) -> Resolved {
    let mut servers: Vec<&Resource> = resources.iter().filter(|r| r.is_server()).collect();
    if servers.is_empty() {
        return Resolved::NoServers;
    }
    // Ours first, then shared servers whose publicAddressMatches says we share the server's NAT.
    // `sort_by_key` is stable, so plex.tv's own order survives inside each group.
    servers.sort_by_key(|r| (!r.owned, !r.public_address_matches));

    let mut found: Vec<SourceRef> = Vec::new();
    let mut refused = false;
    for (server_index, r) in servers.into_iter().enumerate() {
        if server_index != 0 {
            between_servers();
        }
        let plan = probe::plan(r);
        let reach = probe_one(&plan);
        let (outcome, tier) = match &reach {
            Reach::At(c, _) => (Outcome::Reachable, Some(c.location)),
            Reach::Refused => (Outcome::Unauthorized, None),
            Reach::No => (Outcome::Unreachable, None),
        };
        // Publish one aggregate result per server, after all of its direct/relay candidates have
        // settled. In particular a 401 remains distinct from silence, while wrong-machine-only
        // races fold to Unreachable because no address verified this server.
        observe(&plan, outcome, tier);
        match reach {
            Reach::At(c, origin) => {
                let s = SourceRef {
                    machine_id: plan.machine_id.clone(),
                    name: plan.name.clone(),
                    // The CREDIT, not `sourceTitle` — `r` rather than `plan` because the rule reads
                    // two fields (`home`, `ownerId`) that a probe plan has no business carrying.
                    shared_by: credit_of(r, household),
                    owned: plan.owned,
                    // **The origin that ANSWERED** — `probe_server` hands back the very value it
                    // dialled, so what is written down here has been verified and not merely
                    // derived. It comes from the candidate's URL (`dial_target` → `Candidate::origin`)
                    // and never from `Candidate::address`: plex.tv advertises the `plex.direct` NAME
                    // in `uri` while `address` stays the quad behind it, and the certificate is
                    // issued for the name, so a session file that stored the address would fail TLS
                    // validation on every real server (`plex::origin`). The two deliberately do
                    // not agree for a TLS `plex.direct` candidate: its URL names the certificate,
                    // while `address` remains diagnostic metadata about the endpoint behind it.
                    origin_url: origin.base(),
                    // The address that ANSWERED, never the first advertised. Kept as the
                    // DIAGNOSTIC half — what `describe` prints and the Sources panel says.
                    address: c.address,
                    port: c.port,
                    // That server's OWN grant. Our own server's token gets a 401 from a share, so
                    // there is no such thing as one token for the roster.
                    token: plan.token.clone(),
                    // The tier of the candidate that actually answered, persisted beside its
                    // origin so boot can restore the same playback policy without guessing from
                    // an address.
                    tier: Some(c.location),
                };
                // **`origin.log_form()`, not just `describe()`.** `SourceRef::describe` prints the
                // diagnostic `address:port`, and both candidates of one connection carry the SAME
                // address — plex.tv advertises `192.168.0.10` alongside a
                // `192-168-0-10.<hash>.plex.direct` uri — so that line alone cannot say which of
                // the two answered, i.e. whether this run reached the server over TLS at all. That
                // is the `[[silent-instrument-trap]]` exactly: an instrument that cannot see the
                // one thing the change was made to do. `log_form` is byte-identical to the old
                // half for a plaintext origin (the bare authority), so an archived log stays
                // comparable, and says the whole URL the moment it is anything else.
                log(&format!(
                    "auth: reached {} via {}",
                    s.describe(),
                    origin.log_form()
                ));
                found.push(s);
            }
            Reach::Refused => refused = true,
            Reach::No => {}
        }
    }
    if found.is_empty() {
        Resolved::None { refused }
    } else {
        Resolved::Reached(found)
    }
}

/// **Whom to CREDIT for one `/api/v2/resources` row** — the app's single "Shared by …" decision,
/// applied at the boundary where a plex.tv row becomes a persisted [`SourceRef`].
///
/// The rule and its evidence are `plex::servers::owner_credit`; this is only the place discovery
/// calls it, and the reason it is a named function rather than three inline expressions is that
/// there ARE three ingest sites ([`resolve_roster_using`], [`source_from_reach`],
/// [`refreshed_sources`]) and one of them disagreeing is exactly how the raw `sourceTitle` got onto
/// the household's own server in the first place.
///
/// `household` is [`session::Session::household_ids`], captured by the caller from the live session
/// rather than read here: these functions are pure so the whole of discovery is host-gradeable, and
/// a worker that read the session file mid-probe would be reading it under whoever switched profile
/// meanwhile ([`crate::plex`]'s "capture the server at the spawn site" rule, one identity up).
fn credit_of(res: &Resource, household: &[i64]) -> String {
    crate::plex::owner_credit(res.grant(), household).to_string()
}

/// [`credit_of`] for a machine named by a [`ProbePlan`] rather than by the row itself — the early
/// per-candidate publication ([`activate_candidate`]) has the plan and the response, but not the
/// pairing, and a plan deliberately carries only what is needed to DIAL.
///
/// An id that names no row in this response credits nobody. That is the same "absence is the safe
/// direction" the rule itself states: the alternative is attributing a server to whoever plex.tv
/// last mentioned, and the pairing is by `clientIdentifier`, the one identity that cannot drift.
fn credit_for_machine(resources: &[Resource], machine_id: &str, household: &[i64]) -> String {
    if machine_id.is_empty() {
        return String::new();
    }
    resources
        .iter()
        .find(|r| r.is_server() && r.client_identifier == machine_id)
        .map(|r| credit_of(r, household))
        .unwrap_or_default()
}

/// Test seam for the pre-racing acceptance fixtures. The injected dial runs synchronously and the
/// gap is elided; the racing coordinator has its own focused tests for completion order/refusal.
#[cfg(test)]
fn resolve_roster(
    resources: &[Resource],
    household: &[i64],
    dial: &dyn Fn(&Origin) -> (i32, Vec<u8>),
) -> Resolved {
    let mut probe_one = |plan: &ProbePlan| probe_server(plan, dial);
    resolve_roster_using(
        resources,
        household,
        &mut probe_one,
        &mut || {},
        &mut |_, _, _| {},
    )
}

fn resolve_roster_live_while(
    resources: &[Resource],
    household: &[i64],
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
    observe: &mut dyn FnMut(&ProbePlan, Outcome, Option<probe::Location>),
    live: &dyn Fn() -> bool,
) -> Resolved {
    let dial: ProbeDial = Arc::new(get_identity);
    let spawn = |_index: usize, job: ProbeJob| crate::task::spawn_small("probe", job);
    let mut probe_one = |plan: &ProbePlan| {
        if !live() { return Reach::No; }
        probe_server_racing(plan, Arc::clone(&dial), &spawn, PROBE_DEADLINES, activate)
    };
    resolve_roster_using(
        resources,
        household,
        &mut probe_one,
        &mut || { if live() { std::thread::sleep(SERVER_GAP); } },
        observe,
    )
}

/// Project one verified probe winner into the persisted roster shape. A profile switch uses this
/// without registering anything: credentials and endpoints become visible only at its atomic
/// activation commit, never candidate-by-candidate while the previous profile is still live.
///
/// It takes the `Resource` as well as the plan because [`credit_of`] reads two fields off the wire
/// row that the plan does not carry; the plan remains the source of everything about the CONNECTION.
fn source_from_reach(
    res: &Resource,
    plan: &ProbePlan,
    reach: &Reach,
    household: &[i64],
) -> Option<SourceRef> {
    let Reach::At(c, origin) = reach else {
        return None;
    };
    Some(SourceRef {
        machine_id: plan.machine_id.clone(),
        name: plan.name.clone(),
        shared_by: credit_of(res, household),
        owned: plan.owned,
        origin_url: origin.base(),
        address: c.address.clone(),
        port: c.port,
        token: plan.token.clone(),
        tier: Some(c.location),
    })
}

/// Probe exactly one resource, with no live-registry side effect. This is the bounded critical
/// path of a profile choice; whole-roster discovery deliberately remains a different operation.
fn probe_profile_resource_live(
    resource: &Resource,
    household: &[i64],
) -> (Option<SourceRef>, SettledProbe) {
    let plan = probe::plan(resource);
    let dial: ProbeDial = Arc::new(get_identity);
    let spawn = |_index: usize, job: ProbeJob| crate::task::spawn_small("probe", job);
    let reach = probe_server_racing(&plan, dial, &spawn, PROBE_DEADLINES, &mut |_, _, _| {});
    let (outcome, tier) = match &reach {
        Reach::At(c, _) => (Outcome::Reachable, Some(c.location)),
        Reach::Refused => (Outcome::Unauthorized, None),
        Reach::No => (Outcome::Unreachable, None),
    };
    let source = source_from_reach(resource, &plan, &reach, household);
    (source, settled_probe(&plan, outcome, tier))
}

/// Discover **every** server this identity can use — ours and each share — and store the roster.
///
/// Each resource that `provides` a server is turned into ranked candidates by `plex::probe`, raced
/// within that server, and accepted only when the answer's `machineIdentifier` matches. Each winner
/// is registered with the [server registry](crate::plex::register) under its **real machine id** and
/// its **own** per-(user, server) `accessToken` — a share is a separate authority and answers 401 to
/// our own server's token. Our own server stays `current`: a share is browsable, never the default.
///
/// The primary [`ServerRef`] is written exactly as before, so a single-server account produces the
/// same session file it always did (plus a one-entry roster beside it).
fn discover_and_store(ac: &AccountClient, epoch: u64, output: &dyn owner::ObservationSink) -> Discovery {
    if !output.live() { return Discovery::Cancelled; }
    let resources = match ac.resources() {
        Some(r) => r,
        None => {
            // No response, or one that would not deserialize: plex.tv is unreachable from here.
            // NOT `NoServers` — that copy tells the user their account owns no server, which is a
            // statement about their account made on the strength of never having heard from it.
            log("auth: resources request FAILED (no response/deser)");
            return Discovery::Silent;
        }
    };
    log(&format!(
        "auth: resources n={} servers={}",
        resources.len(),
        resources.iter().filter(|r| r.is_server()).count()
    ));
    let mut activate = |plan: &ProbePlan, c: &Candidate, origin: &Origin| {
        let credit = credit_for_machine(&resources, &plan.machine_id, &[]);
        output.progress(AuthProgress::Registry(RegistryProgress::Activate {
            epoch,
            expected: None,
            candidate: candidate_activation(plan, c, origin, &credit),
        }));
    };
    let mut observe = |plan: &ProbePlan, outcome: Outcome, tier: Option<probe::Location>| {
        output.progress(AuthProgress::Registry(RegistryProgress::Settled {
            epoch,
            expected: None,
            probe: settled_probe(plan, outcome, tier),
        }));
    };
    // **No household ids here, and that is a fact about the ORDER rather than an omission**: the
    // Plex Home roster is fetched by `finish_sign_in`, *after* this runs, so at sign-in there is
    // nothing to enumerate the house with — and the CTL session at this moment may still be the
    // account that just signed out. Discovery is always performed with the ACCOUNT OWNER's token
    // (the QR flow authorizes the account, never a managed profile), so plex.tv's own `owned`
    // answers for their server and `home`/`ownerId` for the rest; the household refinement lands
    // with `refresh_roster` or the first profile switch, both of which pass the real roster.
    let resolved = resolve_roster_live_while(&resources, &[], &mut activate, &mut observe, &|| output.live());
    if !output.live() { return Discovery::Cancelled; }
    let found = match resolved {
        Resolved::NoServers => return Discovery::NoServers,
        Resolved::None { refused: true } => return Discovery::Refused,
        Resolved::None { refused: false } => return Discovery::Silent,
        Resolved::Reached(f) => f,
    };

    let primary = primary_index(&found);
    let p = &found[primary];
    let server = ServerRef {
        name: p.name.clone(),
        machine_id: p.machine_id.clone(),
        address: p.address.clone(),
        port: if p.port != 0 { p.port } else { 32400 },
        token: p.token.clone(),
        tier: p.tier,
        // Carried across from the roster entry, so the primary and its `sources` twin can never
        // disagree about where the same server is. `reconcile_primary` keeps them together later.
        origin_url: p.origin_url.clone(),
    };
    log(&format!(
        "auth: {} server(s) reached, primary '{}'",
        found.len(),
        found[primary].name
    ));
    if !output.progress(AuthProgress::Registry(RegistryProgress::Install {
        epoch,
        expected: None,
        sources: found.clone(),
        primary: Some(primary),
    })) { return Discovery::Cancelled; }
    Discovery::Ok {
        server,
        sources: found,
    }
}

/// **Re-learn the roster from plex.tv on a resumed session, in the background.**
///
/// `discover_and_store` above is the only other writer of `Session::sources`, and it runs on ONE
/// path: the QR sign-in. So before this existed the roster was learned exactly once, at sign-in,
/// and never again — which meant:
///
/// * an account signed in before shared servers shipped had `sources: []` forever, and every share
///   was invisible on every boot no matter how many times the app was relaunched (owner-reported,
///   2026-08-14: the libraries were there under the dev credential trigger and gone on a real
///   launch — the persisted roster on the device was an empty array);
/// * and a friend sharing a library TOMORROW would never appear either, because nobody signs in
///   again. A grant is not a one-time fact, so neither is discovery of it.
///
/// Best-effort and non-destructive: on any failure the persisted roster stays exactly as it was, so
/// a boot with plex.tv unreachable still browses whatever was already known. A successful refresh
/// replaces the live registry with the authoritative granted roster. It preserves the current
/// primary while that machine remains granted; if the grant disappeared, it promotes the preferred
/// surviving server so `current` cannot be stranded on a tokenless shell.
///
/// Persists only when the roster actually CHANGED, because the session file is on flash and a
/// rewrite per boot buys nothing.
///
/// **It runs for the account OWNER only, and that is a correctness gate rather than a policy.**
/// The one credential this can ask plex.tv with is [`Session::account_token`], which belongs to the
/// admin and is never replaced by a Plex Home switch — so every `accessToken` in the answer is the
/// ADMIN's per-(user, server) grant. Installing those while a managed profile is watching swaps the
/// wrong identity's token into every registered `Client` in place (that swap is what the ~30 call
/// sites holding a `&'static Client` are built to follow) and then persists it: browsing and
/// scrobbling as the account owner from someone else's profile. For a RESTRICTED profile it is
/// worse than wrong, it is a re-grant — [`retoken`] had already blanked and hidden the servers that
/// profile was not given, and this puts them back.
///
/// Re-keying the answer for the active profile afterwards is not available: the per-user tokens
/// only exist in a `/api/v2/resources` fetched with THAT user's account token, which the switch
/// obtains for one request and does not persist. So the honest answer is to skip, and the cost is
/// named: a share granted while a managed profile is signed in appears when someone next switches
/// profile (the switch re-keys the whole roster from its own response) or signs in again.
fn refreshed_sources(
    stored: &[SourceRef],
    reached: &[SourceRef],
    resources: &[Resource],
    household: &[i64],
) -> Vec<SourceRef> {
    let mut grants: Vec<&Resource> = resources
        .iter()
        .filter(|r| r.is_server() && !r.client_identifier.is_empty() && !r.access_token.is_empty())
        .collect();
    grants.sort_by_key(|r| (!r.owned, !r.public_address_matches));

    let mut out = Vec::new();
    for r in grants {
        if out
            .iter()
            .any(|s: &SourceRef| s.machine_id == r.client_identifier)
        {
            continue;
        }
        if let Some(s) = reached.iter().find(|s| s.machine_id == r.client_identifier) {
            out.push(s.clone());
            continue;
        }
        let Some(mut cached) = stored
            .iter()
            .find(|s| s.machine_id == r.client_identifier)
            .cloned()
        else {
            // A newly granted but unreachable server has no verified address to preserve yet.
            continue;
        };
        cached.token = r.access_token.clone();
        cached.owned = r.owned;
        if !r.name.is_empty() {
            cached.name = r.name.clone();
        }
        // **Assigned, not merged.** The credit follows the CURRENT grant unconditionally, because
        // "no credit" is a positive answer here and not a missing one: this is the exact path a
        // Plex Home profile switch takes, and a stored entry that already names somebody (the
        // admin, from a build that wrote the raw `sourceTitle`) has to lose that name rather than
        // keep it for want of a fresher one. A share whose handle plex.tv stops sending likewise
        // stops being credited — see `plex::servers::owner_credit` on why absence is the safe way
        // to be wrong.
        cached.shared_by = credit_of(r, household);
        if cached.usable() {
            out.push(cached);
        }
    }
    out
}

fn same_sources(a: &[SourceRef], b: &[SourceRef]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b.iter()).all(|(a, b)| {
            a.machine_id == b.machine_id
                && a.name == b.name
                && a.shared_by == b.shared_by
                && a.owned == b.owned
                && a.address == b.address
                && a.port == b.port
                && a.token == b.token
                && a.origin_url == b.origin_url
                && a.tier == b.tier
        })
}

fn server_ref(source: &SourceRef) -> ServerRef {
    ServerRef {
        name: source.name.clone(),
        machine_id: source.machine_id.clone(),
        address: source.address.clone(),
        port: if source.port != 0 { source.port } else { 32400 },
        token: source.token.clone(),
        tier: source.tier,
        origin_url: source.origin_url.clone(),
    }
}

/// Follow the stored primary when it still exists; otherwise promote the preferred surviving
/// grant. Leaving a removed primary in place strands `current` as a tokenless shell after registry
/// replacement and makes the newly reached servers unusable despite a successful refresh.
fn reconcile_refresh_primary(server: &mut ServerRef, sources: &[SourceRef]) -> bool {
    if sources.is_empty() {
        return false;
    }
    if sources.iter().any(|s| s.machine_id == server.machine_id) {
        return reconcile_primary(server, sources);
    }
    let next = &sources[primary_index(sources)];
    log(&format!(
        "auth: primary grant removed — using {:?} at {}:{}",
        next.name, next.address, next.port
    ));
    *server = server_ref(next);
    true
}

/// Reconcile both records of the active owner's primary credential.
///
/// `Session::user.token` is the selected Plex Home user's token for the PRIMARY, so a refresh that
/// rotates that grant—or promotes another machine—must move it together with `Session::server`.
/// Owner sessions without a Home user token already fall back to `server.token` and need no copy.
fn reconcile_refresh_session(s: &mut Session, sources: &[SourceRef]) -> bool {
    let mut changed = reconcile_refresh_primary(&mut s.server, sources);
    if !s.user.token.is_empty() && s.user.token != s.server.token {
        s.user.token = s.server.token.clone();
        changed = true;
    }
    changed
}

fn server_roster_worker_with_output(sess: Session, epoch: u64, expected: SessionIdentity,
    household: Vec<i64>, output: &dyn owner::ObservationSink) {
    if !output.live() { return; }
    let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
    let Some(resources) = ac.resources() else {
        output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
            epoch,
            expected,
            outcome: ServerRosterOutcome::Unreachable,
        }));
        return;
    };
    let mut activate = |plan: &ProbePlan, c: &Candidate, origin: &Origin| {
        let credit = credit_for_machine(&resources, &plan.machine_id, &household);
        output.progress(AuthProgress::Registry(RegistryProgress::Activate {
            epoch,
            expected: Some(expected.clone()),
            candidate: candidate_activation(plan, c, origin, &credit),
        }));
    };
    let mut settled = Vec::new();
    let found = match resolve_roster_live_while(
        &resources,
        &household,
        &mut activate,
        &mut |plan, outcome, tier| {
            let probe = settled_probe(plan, outcome, tier);
            settled.push(probe.clone());
            output.progress(AuthProgress::Registry(RegistryProgress::Settled {
                epoch,
                expected: Some(expected.clone()),
                probe,
            }));
        },
        &|| output.live(),
    ) {
        Resolved::Reached(found) => found,
        _ => {
            output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
                epoch,
                expected,
                outcome: ServerRosterOutcome::NoReachable,
            }));
            return;
        }
    };
    output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
        epoch,
        expected,
        outcome: ServerRosterOutcome::Reconcile {
            resources,
            found,
            household,
            settled,
        },
    }));
}

/// Replace only the route facts of one already-granted source.
///
/// This is deliberately narrower than [`refreshed_sources`]. A recovery probe may use the
/// install owner's account token merely to obtain the current connection list after the network
/// topology changes, while the live Plex Home profile owns a different per-server PMS token and a
/// smaller grant set. Therefore it may neither add/remove a source nor copy the Resource token:
/// it updates the verified origin, address and tier of the exact machine already in the profile.
fn apply_refreshed_endpoint(
    session: &mut Session,
    machine_id: &str,
    fresh: &SourceRef,
) -> Option<(SourceRef, bool)> {
    let source = session
        .sources
        .iter_mut()
        .find(|source| source.machine_id == machine_id)?;
    let next = SourceRef {
        address: fresh.address.clone(),
        port: fresh.port,
        origin_url: fresh.origin_url.clone(),
        tier: fresh.tier,
        // Grant/profile facts remain exactly the active profile's. In particular, `fresh.token`
        // may be the account owner's token when this recovery follows a managed-profile switch.
        token: source.token.clone(),
        machine_id: source.machine_id.clone(),
        name: source.name.clone(),
        shared_by: source.shared_by.clone(),
        owned: source.owned,
    };
    let changed = source.address != next.address
        || source.port != next.port
        || source.origin_url != next.origin_url
        || source.tier != next.tier;
    *source = next.clone();
    if session.server.machine_id == machine_id {
        reconcile_primary(&mut session.server, std::slice::from_ref(&next));
    }
    Some((next, changed))
}

fn probe_endpoint_work(
    id: ServerId,
    machine_id: &str,
    sess: &Session,
    resources: impl FnOnce(&AccountClient) -> Option<Vec<Resource>>,
    probe: impl FnOnce(&Resource, &[i64]) -> (Option<SourceRef>, SettledProbe),
    live: &dyn Fn() -> bool,
) -> Option<SourceRef> {
    if !live() { return None; }
    let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
    let Some(resources) = resources(&ac) else {
        log(&format!(
            "auth: endpoint refresh for source {} could not reach plex.tv",
            id.raw()
        ));
        return None;
    };
    let Some(resource) = resources
        .iter()
        .find(|resource| resource.is_server() && resource.client_identifier == machine_id)
    else {
        log(&format!(
            "auth: endpoint refresh for source {} found no matching resource",
            id.raw()
        ));
        return None;
    };
    if !live() { return None; }
    let (fresh, _) = probe(resource, &sess.household_ids());
    fresh
}

/// Point the persisted PRIMARY at wherever the refreshed roster says that machine now answers.
/// Returns whether anything moved, so the caller knows the save is owed.
///
/// [`Session::server`] and [`Session::sources`] are two records of the same servers and only the
/// second was being rewritten here, so the moment the primary PMS changed LAN address the two
/// disagreed permanently. Two symptoms, both durable and neither self-healing:
///
/// * `app.rs`'s boot gate dials `session.server`, so every boot went to the dead address first;
/// * and `plex::install` of that address registers a SECOND slot for a machine already in the table
///   — `servers::same_server` can only match on the address when the legacy `install` supplies no
///   machine id — with the dead copy made `current`. The house's own server, listed twice, the
///   working one not the one being used.
///
/// It cannot be fixed by re-running discovery either: the refresh persists `sources` only when they
/// changed, so the very first boot after the move wrote the new address into the roster and left
/// `server` stale, and every boot after that found the roster already correct and saved nothing.
/// That is why the reconcile is part of the CHANGED decision and not a rider on it.
///
/// Matched on `machine_id` and nothing else — the identity that survives an address moving is the
/// only thing that can decide this — and an empty id matches nothing, [`retoken`]'s rule: an entry
/// that cannot be identified must never match a resource that also happens to have no id.
fn reconcile_primary(server: &mut ServerRef, found: &[SourceRef]) -> bool {
    if server.machine_id.is_empty() {
        return false;
    }
    let Some(s) = found
        .iter()
        .find(|s| s.machine_id == server.machine_id && s.usable())
    else {
        return false;
    };
    if server.address == s.address
        && server.port == s.port
        && server.token == s.token
        && server.origin_url == s.origin_url
        && server.tier == s.tier
    {
        return false;
    }
    // The line says the server MOVED, so it must not fire when only the stored origin was
    // LEARNED. A primary written before that field existed carries an empty one, so the first boot
    // after the upgrade populates it beside an identical address, port and token — a write, and not
    // news. Logging it would read as DHCP churn in the file this project treats as its primary
    // evidence surface, on every existing install, exactly once, which is the worst kind of false
    // positive: unreproducible afterwards.
    let learned_origin = server.origin_url.is_empty() && !s.origin_url.is_empty();
    let moved = server.address != s.address
        || server.port != s.port
        || (server.origin_url != s.origin_url && !learned_origin);
    if moved {
        // The machine name and the address, never the token and never the machine id — the same
        // line `SourceRef::describe` draws.
        log(&format!(
            "auth: primary {:?} now answers at {}:{}",
            server.name, s.address, s.port
        ));
    }
    server.address = s.address.clone();
    server.port = s.port;
    // The origin moves with the address for the same reason the token does: it came out of the
    // same answer. Leaving it behind would keep dialling the old one, which is the bug this
    // whole function exists to close, one field further in.
    server.origin_url = s.origin_url.clone();
    server.tier = s.tier;
    // The token moves with the address because it came from the same answer: this is the OWNER's
    // per-(user, server) grant, which is exactly what `ServerRef::token` means (and the refresh
    // above only runs for the owner). `pms_token()` still prefers a switched profile's own token.
    server.token = s.token.clone();
    true
}

/// Register a roster with the [server registry](crate::plex::register), optionally naming which
/// entry is the current server.
///
/// The registry is keyed on `machineIdentifier`, so this is idempotent: re-running discovery
/// re-points a server that moved rather than adding a second slot for it, and a re-registration at
/// the same address just swaps the token in place — which is what the ~30 call sites holding a
/// `&'static Client` rely on.
///
/// Owned entries are registered FIRST even when `primary` is `None` — see [`registration_order`].
fn install_roster(sources: &[SourceRef], primary: Option<usize>) -> Vec<ServerId> {
    let order = registration_order(sources);
    let mut installed = Vec::with_capacity(order.len());
    for &i in &order {
        let s = &sources[i];
        // `registration_order` already filtered on `usable()`, which IS `origin().is_some()` —
        // so this `else` is unreachable today and is a `continue` rather than an `expect` because
        // a roster entry has never been allowed to cost more than itself (`de_soft_vec`).
        let Some(origin) = s.origin() else { continue };
        let id =
            register_observed_origin(&s.machine_id, &origin, &s.token, s.resolve_pin().as_ref());
        if !id.is_set() {
            continue;
        }
        installed.push(id);
        // Registration may have re-pointed the slot by publishing a fresh Client, whose link is
        // deliberately unknown. Restore the winner only AFTER that publication, every time.
        if let (Some(link), Some(client)) = (s.tier, crate::plex::client_for(id)) {
            client.set_connection(link, crate::plex::IpVersion::of_host(&s.address));
        }
        // …and say WHOSE it is. Registering without this was the bug that made the whole shared-
        // source feature invisible on the only path a real user takes: `ServerFacts` stayed unset,
        // so every source read as owned with no handle, and each surface then correctly drew
        // nothing — no "Shared by" on a detail page, no handle on a shelf heading or the Source
        // chip, no owner on a failure read-out, and a friend's library pinned to Home by the
        // ownership default. It looked like five separate features not working. The one
        // `describe_server` call that existed was in `app.rs`'s DEV-TRIGGER path, which is exactly
        // why a headless capture showed the handle and a signed-in television did not.
        //
        // `owned` comes from the roster rather than from an empty handle: a share whose
        // `sourceTitle` plex.tv did not send is still a share.
        crate::plex::describe_server(id, &s.name, &s.shared_by, s.owned);
        if primary == Some(i) {
            crate::plex::set_current(id);
        }
    }
    installed
}

/// Which roster entries to register, and in what order: the ones that can actually be dialled,
/// **ours first**.
///
/// The order is load-bearing, not tidiness. The registry makes the FIRST registration current when
/// nothing is current yet (`servers.rs`), which is exactly the state a boot is in — so a roster
/// that happens to list a share first would silently come up pointed at the friend's server, and
/// Home would be built from their library. Stable, so plex.tv's own order survives inside each
/// group.
fn registration_order(sources: &[SourceRef]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sources.len())
        .filter(|&i| sources[i].usable())
        .collect();
    order.sort_by_key(|&i| !sources[i].owned);
    order
}

/// Which reached server is the primary: ours if it answered, else the first that did. A friend's
/// library is a better app than "no server found" when our own box is off.
fn primary_index(sources: &[SourceRef]) -> usize {
    sources.iter().position(|s| s.owned).unwrap_or(0)
}

/// Re-key a stored roster to a newly switched profile.
///
/// `accessToken` is per **(user, server)**, so switching profile invalidates every stored token at
/// once, not only the primary's — a share left on the previous profile's token answers 401 to
/// everything. The switch already fetches `/api/v2/resources` as the new user to find the primary's
/// token, so this re-keys the whole roster from that same response: no extra round trip.
///
/// A source the response no longer names is retained only as TOKENLESS connection metadata. It is
/// therefore unusable and omitted by every registry/install walk, but a later switch back to a
/// profile that is granted it can restore the new token without having forgotten the verified
/// address while it was hidden. A brand new share is not added here: it has no probed address yet,
/// and inventing one is what discovery is for. An entry with no machine id is dropped entirely: it
/// cannot be identified, and emptiness must never match another empty id.
#[cfg(test)]
fn retoken(sources: &[SourceRef], resources: &[Resource]) -> Vec<SourceRef> {
    sources
        .iter()
        .filter(|s| !s.machine_id.is_empty())
        .map(|s| {
            let token = resources
                .iter()
                .find(|r| r.is_server() && r.client_identifier == s.machine_id)
                .map(|r| r.access_token.clone())
                .unwrap_or_default();
            SourceRef { token, ..s.clone() }
        })
        .collect()
}

/// Reconcile a profile switch's grants with endpoints verified using that profile's transient
/// account token. Kept separate from [`retoken`] while the switch flow is migrated so the
/// regression test can pin the missing half: changing credentials must not throw away a fresher
/// verified origin.
fn profile_sources(
    stored: &[SourceRef],
    reached: &[SourceRef],
    resources: &[Resource],
    household: &[i64],
) -> Vec<SourceRef> {
    refreshed_sources(stored, reached, resources, household)
}

fn ordered_profile_grants(resources: &[Resource]) -> Vec<usize> {
    let mut grants: Vec<usize> = resources
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.is_server() && !r.client_identifier.is_empty() && !r.access_token.is_empty()
        })
        .map(|(i, _)| i)
        .collect();
    grants.sort_by_key(|&i| (!resources[i].owned, !resources[i].public_address_matches));
    let mut seen = Vec::<String>::new();
    grants.retain(|&i| {
        let mid = &resources[i].client_identifier;
        if seen.contains(mid) {
            false
        } else {
            seen.push(mid.clone());
            true
        }
    });
    grants
}

/// Where a failed `switch_user` is SHOWN — `(roster banner, blame the PIN)`.
///
/// Pure, and split out of [`switch_thread`] for the reason [`may_resume`] is: its only caller runs
/// inside a spawned worker behind a plex.tv round trip, which no host test can reach.
/// **A PIN-blaming failure leaves the roster's band EMPTY.** The keypad already answered it — the
/// dots flash red and the entry restarts on the same pad — and `ui::profiles::draw` paints
/// [`error`] under the AVATAR ROW the moment the pad is closed, so a banner here reappeared under
/// the faces as soon as BACK dismissed the keypad, blaming a PIN nobody was being asked for any
/// more. Two surfaces, one of them asking about a PIN; the answer belongs on that one.
///
/// Every other failure keeps its banner, and that asymmetry is the point rather than an oversight:
/// "no access to this server" and "check the connection" close the pad (`ui::profiles::update`)
/// precisely so the roster can say WHY, and a picker that swallowed the choice with no read-out at
/// all is the failure the banner was added for.
fn switch_failure(pin_submitted: bool) -> (String, bool) {
    if pin_submitted {
        (String::new(), true)
    } else {
        (
            "Couldn't switch profile — check the connection.".into(),
            false,
        )
    }
}

/// The uuid a seated profile is recorded under: the ROSTER's, which is what every later
/// comparison uses — the tile the next pick names, [`Session::cached_profile`]'s key, and the
/// same-user shortcut in [`switch_thread`]. The `/switch` response's own `uuid` is taken only when
/// it agrees or the roster has none. The first device run of the cache (2026-09-06) wrote NO
/// record for a primed profile: the response's `uuid` came back empty, `remember_profile` refuses
/// an empty key, and the pick that followed offline found "no cached credentials".
fn seated_uuid(u: &crate::plex::account::SwitchedUser, tile: &UserTile) -> String {
    if tile.uuid.is_empty() {
        u.uuid.clone()
    } else {
        tile.uuid.clone()
    }
}

/// What a profile pick resolves to with plex.tv out of reach — decided from the stored session
/// alone, so it can be graded on the host.
pub(crate) enum OfflineSwitch {
    /// Seat this session: the cached credentials, under the stored account and roster.
    Seat(Box<Session>),
    /// A protected profile whose PIN does not match this television's record.
    PinDenied,
    /// Nothing cached for this profile — or a protected one cached without a verifier, which
    /// cannot be checked and is therefore the same as nothing.
    NoCache,
}

/// [`OfflineSwitch`] for `tile`, from `stored`'s [`Session::profiles`].
///
/// The rule is the online one with the network removed: a PIN-protected profile is seated only
/// on its PIN, an unprotected one on the pick alone. What changes is who checks the PIN — the
/// verifier the last online switch wrote — and that a profile this television has never seated
/// online cannot be seated at all, because there is nothing to seat it with.
fn offline_activation(stored: &Session, tile: &UserTile, pin: Option<&str>) -> OfflineSwitch {
    let Some(cached) = stored.cached_profile(&tile.uuid) else {
        return OfflineSwitch::NoCache;
    };
    if tile.protected {
        let Some(verifier) = &cached.pin else {
            return OfflineSwitch::NoCache;
        };
        let Some(pin) = pin.filter(|p| !p.is_empty()) else {
            return OfflineSwitch::PinDenied;
        };
        if !verifier.verify(pin) {
            return OfflineSwitch::PinDenied;
        }
    }
    let mut next = stored.clone();
    next.server = cached.server.clone();
    next.sources = cached.sources.clone();
    next.user = cached.user.clone();
    OfflineSwitch::Seat(Box::new(next))
}

/// The switch worker's offline arm: verify/derive against captured data and return facts only.
///
/// The seat is the online success arm with the probes removed — the same revoke / install /
/// finish sequence, so the registry ends in the same state a network switch leaves it in. The
/// cached tokens are the ones that were valid when the profile was last seated online; a server
/// that has since revoked them answers 401 on Home exactly as it would after a stale boot, and
/// the next online pick rewrites the record.
fn offline_switch_outcome(
    stored: &Session,
    tile: &UserTile,
    pin: Option<&str>,
) -> ProfileSwitchOutcomeProgress {
    match offline_activation(stored, tile, pin) {
        OfflineSwitch::Seat(next) => {
            log(&format!(
                "auth: switch '{}' -> ok (offline, cached credentials)",
                tile.title
            ));
            ProfileSwitchOutcomeProgress::Ready {
                delta: ProfileDelta {
                    server: next.server.clone(),
                    sources: next.sources.clone(),
                    user: next.user.clone(),
                    cache: None,
                },
                probes: Vec::new(),
            }
        }
        OfflineSwitch::PinDenied => {
            log(&format!(
                "auth: switch '{}' -> offline, the PIN did not match this television's record",
                tile.title
            ));
            ProfileSwitchOutcomeProgress::Failed {
                error: String::new(),
                pin_denied: true,
            }
        }
        OfflineSwitch::NoCache => {
            log(&format!(
                "auth: switch '{}' -> failed (plex.tv unreachable, and no cached credentials for this profile)",
                tile.title
            ));
            // The banner, never the PIN flash: a PIN that could not be checked was not refused —
            // and it says what would fix it, because "check the connection" reads as a fault
            // when the connection is down on purpose (owner, 2026-09-06: state that one online
            // pick is needed first).
            ProfileSwitchOutcomeProgress::Failed {
                error: String::from(
                    "No internet connection. Pick this profile once while online, and it will work offline.",
                ),
                pin_denied: false,
            }
        }
    }
}

/// Transport boundary for the profile worker. Implementations supply account/probe observations
/// and pacing only; cache/PIN/grant/Ready/late-roster decisions stay in the shared worker body.
pub(crate) trait ProfileWorkIo {
    fn switch(&mut self, account: &AccountClient, uuid: &str, pin: Option<&str>) -> SwitchOutcome;
    fn resources(&mut self, account: &AccountClient) -> Option<Vec<Resource>>;
    fn probe(&mut self, resource: &Resource, household: &[i64]) -> (Option<SourceRef>, SettledProbe);
    fn gap(&mut self);
}

struct LiveProfileWorkIo<S> { switch: Option<S> }

impl<S: FnOnce(&AccountClient, &str, Option<&str>) -> SwitchOutcome> ProfileWorkIo for LiveProfileWorkIo<S> {
    fn switch(&mut self, account: &AccountClient, uuid: &str, pin: Option<&str>) -> SwitchOutcome {
        self.switch.take().expect("one switch request per profile worker")(account, uuid, pin)
    }
    fn resources(&mut self, account: &AccountClient) -> Option<Vec<Resource>> { account.resources() }
    fn probe(&mut self, resource: &Resource, household: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        probe_profile_resource_live(resource, household)
    }
    fn gap(&mut self) { std::thread::sleep(SERVER_GAP); }
}

/// Both the live resource executor and preserved worker-policy tests enter this same body.
fn profile_switch_worker_with_output(
    epoch: u64,
    expected: SessionIdentity,
    stored: Session,
    tile: UserTile,
    pin: Option<String>,
    recently_unreachable: bool,
    output: &dyn owner::ObservationSink,
    switch: impl FnOnce(&AccountClient, &str, Option<&str>) -> SwitchOutcome,
) {
    profile_switch_worker_with_io(epoch, expected, stored, tile, pin, recently_unreachable,
        output, &mut LiveProfileWorkIo { switch: Some(switch) });
}

pub(crate) fn profile_switch_worker_with_io(
    epoch: u64,
    expected: SessionIdentity,
    stored: Session,
    tile: UserTile,
    pin: Option<String>,
    recently_unreachable: bool,
    output: &dyn owner::ObservationSink,
    io: &mut impl ProfileWorkIo,
) {
    if !output.live() { return; }
    let cid = stored.client_id.clone();
    let account_token = stored.account_token.clone();
    let ac = AccountClient::new(&cid, Some(&account_token));
    let cache_first = stored.cached_profile(&tile.uuid).is_some() && recently_unreachable;
    let outcome = if cache_first {
        log("auth: switch — plex.tv was unreachable moments ago, trying the cached credentials first");
        SwitchOutcome::Unreachable
    } else {
        io.switch(&ac, &tile.uuid, pin.as_deref())
    };
    let user = match outcome {
        SwitchOutcome::Switched(user) => user,
        SwitchOutcome::Refused(status) => {
            log(&format!(
                "auth: switch '{}' -> refused (HTTP {status})",
                tile.title
            ));
            let (error, pin_denied) = switch_failure(pin.is_some());
            output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
                epoch,
                expected,
                outcome: ProfileSwitchOutcomeProgress::Failed { error, pin_denied },
            }));
            return;
        }
        SwitchOutcome::Unreachable => {
            let outcome = offline_switch_outcome(&stored, &tile, pin.as_deref());
            output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
                epoch,
                expected,
                outcome,
            }));
            return;
        }
    };
    if !output.live() { return; }
    let Some(resources) = io.resources(&AccountClient::new(&cid, Some(&user.auth_token))) else {
        log("auth: profile resources request failed");
        output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            epoch,
            expected,
            outcome: ProfileSwitchOutcomeProgress::Failed {
                error: "Couldn't switch profile — check the connection.".into(),
                pin_denied: false,
            },
        }));
        return;
    };
    let grants = ordered_profile_grants(&resources);
    if grants.is_empty() {
        output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            epoch,
            expected,
            outcome: ProfileSwitchOutcomeProgress::Failed {
                error: format!("{} has no server access", tile.title),
                pin_denied: false,
            },
        }));
        return;
    }

    let household = stored.household_ids();
    let mut order = grants.clone();
    if let Some(pos) = order
        .iter()
        .position(|&i| resources[i].client_identifier == stored.server.machine_id)
    {
        order.swap(0, pos);
    }
    let mut reached = Vec::new();
    let mut probes = Vec::new();
    let mut probed = vec![false; resources.len()];
    let mut selected_mid = None;
    for &i in &order {
        if !output.live() { return; }
        let (winner, settled) = io.probe(&resources[i], &household);
        probed[i] = true;
        probes.push(settled);
        if let Some(winner) = winner {
            reached.push(winner);
        }
        let roster = profile_sources(&stored.sources, &reached, &resources, &household);
        if roster
            .iter()
            .any(|s| s.machine_id == resources[i].client_identifier && s.usable())
        {
            selected_mid = Some(resources[i].client_identifier.clone());
            break;
        }
    }
    let initial = profile_sources(&stored.sources, &reached, &resources, &household);
    let Some(primary) =
        selected_mid.and_then(|mid| initial.iter().find(|s| s.machine_id == mid).cloned())
    else {
        log(&format!(
            "auth: switch '{}' -> no server access",
            tile.title
        ));
        output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            epoch,
            expected,
            outcome: ProfileSwitchOutcomeProgress::Failed {
                error: format!("{} has no access to this server", tile.title),
                pin_denied: false,
            },
        }));
        return;
    };

    log(&format!(
        "auth: switch '{}' -> ok (per-user server token)",
        tile.title
    ));
    let server = server_ref(&primary);
    let user = UserRef {
        id: user.id,
        uuid: seated_uuid(&user, &tile),
        title: user.title,
        thumb: tile.thumb,
        token: primary.token.clone(),
    };
    if !output.live() { return; }
    let cache = ProfileCreds {
        uuid: user.uuid.clone(),
        user: user.clone(),
        server: server.clone(),
        sources: initial.clone(),
        pin: pin
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(session::PinVerifier::new),
    };
    let next_identity = SessionIdentity {
        client_id: expected.client_id.clone(),
        account_token: expected.account_token.clone(),
        profile_uuid: user.uuid.clone(),
    };
    if !output.progress(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
        epoch,
        expected,
        outcome: ProfileSwitchOutcomeProgress::Ready {
            delta: ProfileDelta {
                server,
                sources: initial,
                user,
                cache: Some(cache),
            },
            probes: probes.clone(),
        },
    })) { return; }

    for &i in &grants {
        if probed[i] {
            continue;
        }
        if !output.live() { return; }
        io.gap();
        if !output.live() { return; }
        let (winner, settled) = io.probe(&resources[i], &household);
        probes.push(settled);
        if let Some(winner) = winner {
            reached.push(winner);
        }
    }
    output.terminal(AuthProgress::ProfileRoster(ProfileRosterProgress {
        epoch,
        expected: next_identity,
        resources,
        reached,
        probes,
    }));
}

// ---- helpers ----

#[cfg(test)]
fn settle_signin(active: &mut bool) -> bool {
    std::mem::take(active)
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_controller_has_no_process_global_owner() {
        let source = include_str!("auth.rs");
        let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
        for declaration in ["static CTL:", "static QR_GENERATION:", "static ENDPOINT_ADMISSION:",
            "static DELETE_LEFTOVERS:", "static PROGRESS:", "static AUTH_EPOCH:", "static ACTIVATION_GATE:"] {
            assert!(!production.contains(declaration), "global decision/queue remains: {declaration}");
        }
    }
    use super::*;
    use crate::plex::probe::Scheme;
    use std::cell::RefCell;
    use std::sync::Mutex;

    /// **The next account to sign in must be asked afresh.** The maintainer's scenario (2026-09-04):
    /// account A consents to both channels, signs out, account B signs in through the QR flow — and
    /// B was never asked, while B's usage went out under A's consent and A's identifiers. Consent
    /// belongs to the person who gave it, so signing out ends it: the decision returns to
    /// *unanswered*, both identifiers are destroyed and the file is gone, exactly as a withdrawal
    /// plus a fresh install would leave it. This resource test grades the live Session adapter's
    /// CloseTelemetry effect. The Bridge erasure test separately proves that the owner emits it
    /// before resource deletion; no network work is launched here.
    #[test]
    fn signing_out_leaves_no_consent_and_no_identifier_for_the_next_account() {
        use crate::telemetry::consent;
        /// Every crate-global redirect this test takes, handed back on drop — so a failed
        /// assertion cannot leave the next test writing into this one's directory.
        struct Redirects {
            dir: std::path::PathBuf,
            saved: Option<consent::Consent>,
        }
        impl Drop for Redirects {
            fn drop(&mut self) {
                crate::telemetry::spool::set_test_path(None);
                crate::telemetry::redirect_for_test(None);
                crate::plex::session::redirect_for_test(None);
                if let Some(c) = self.saved.take() {
                    consent::install(c);
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-signout-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        let _redirects = Redirects {
            dir: dir.clone(),
            saved: consent::current(),
        };
        crate::plex::session::redirect_for_test(Some(dir.join("auth.json")));
        let consent_file = dir.join("telemetry.json");
        crate::telemetry::redirect_for_test(Some(consent_file.clone()));
        crate::telemetry::spool::set_test_path(Some(dir.join("spool.jsonl")));

        // Account A answers yes to both, which mints both identifiers and persists the decision.
        crate::telemetry::record(consent::apply(
            &consent::Consent::default(),
            true,
            true,
            || Some("a".repeat(32)),
        ));
        assert!(consent::allows_usage() && consent::errors_id().is_some());
        assert!(
            consent_file.exists(),
            "the decision was persisted for account A"
        );

        let mut bridge = crate::app::bridge::Bridge::for_consent_resource_test(
            consent::current().expect("account A decision is published"),
        );
        let mut dispatcher =
            crate::ui::dispatch::Dispatcher::<crate::app::bridge::AppHost>::new();
        dispatcher.emit(
            crate::ui::machine::MachineId::Session,
            crate::ui::machine::Fx::App(
                crate::screens::registry::AppFx::SessionEffect(
                    owner::SessionFx::Coordinator(owner::CoordinatorAction::CloseTelemetry),
                ),
            ),
        );
        dispatcher.frame_with(
            &mut bridge,
            crate::ui::machine::Tick::default(),
            Vec::new(),
            Vec::new(),
            &mut crate::ui::dispatch::NoTap,
            false,
        );

        let after = consent::current().expect("a decision is always published");
        assert!(
            !after.answered(),
            "account B would never be asked: A's answer survived the sign-out"
        );
        assert!(
            after.install_id.is_none() && after.errors_id.is_none(),
            "an identifier survived the sign-out and would tag B's reports as A"
        );
        assert!(!consent::allows_usage() && !consent::allows_errors());
        assert!(consent::errors_id().is_none());
        assert!(
            consent::should_ask(&after, false),
            "the next authorized sign-in must put the question on screen again"
        );
        assert!(
            !consent_file.exists(),
            "the consent file outlived the sign-out and would resume A's decision at the next boot"
        );
    }

    #[test]
    fn only_a_live_qr_attempt_can_settle_as_an_activation() {
        let mut qr_attempt = true;
        assert!(settle_signin(&mut qr_attempt));
        assert!(!qr_attempt);
        assert!(
            !settle_signin(&mut qr_attempt),
            "a retry or duplicate completion is not activation"
        );

        let mut stored_session_discovery = false;
        assert!(!settle_signin(&mut stored_session_discovery));
    }

    fn resource(json: &str) -> Resource {
        serde_json::from_str(json).expect("fixture parses")
    }

    /// A share with FOUR advertised addresses, which between them cover every case the probe loop
    /// has to get right: the owner's LAN address (policy keeps only its TLS URI), a hostname the
    /// transport resolves, and two public IPv4s so "the first one answered as somebody else" has a
    /// second one to fall through to. Shaped on the live capture of 2026-08-11
    /// (`docs/shared-servers.md` §2); the addresses are stand-ins, the arrangement is not.
    fn a_share() -> Resource {
        resource(
            r#"{"name":"nas-home","clientIdentifier":"bbbb2222","provides":"server","owned":false,
                "sourceTitle":"friend","publicAddressMatches":false,"httpsRequired":false,
                "accessToken":"tok-share","connections":[
                  {"protocol":"https","address":"10.9.9.7","port":32400,
                   "uri":"https://172-20-4-7.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"media.example.internal","port":31234,
                   "uri":"https://media.example.internal:31234","local":false,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"198.51.100.7","port":31234,
                   "uri":"https://198-51-100-7.h.plex.direct:31234","local":false,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"203.0.113.9","port":31234,
                   "uri":"https://203-0-113-9.h.plex.direct:31234","local":false,"relay":false,"IPv6":false}]}"#,
        )
    }

    /// A JSON `/identity` body naming `mid` — what a PMS answers a probe with.
    fn identity_json(mid: &str) -> Vec<u8> {
        format!(
            r#"{{"MediaContainer":{{"size":0,"machineIdentifier":"{mid}","version":"1.43.3"}}}}"#
        )
        .into_bytes()
    }

    /// A recording dial. Returns whatever the script says for an address, and remembers the order
    /// it was asked — which is how "it stopped" and "it never tried that one" become assertions.
    struct Dialled {
        seen: RefCell<Vec<String>>,
        answers: Vec<(&'static str, i32, Vec<u8>)>,
    }
    impl Dialled {
        fn new(answers: Vec<(&'static str, i32, Vec<u8>)>) -> Dialled {
            Dialled {
                seen: RefCell::new(Vec::new()),
                answers,
            }
        }
        /// Answers are keyed on the origin's HOST, which is the field that tells the two candidates
        /// of one connection apart: `203-0-113-9.h.plex.direct` is the advertised uri and
        /// `203.0.113.9` is the plaintext twin synthesized from the address behind it. A fixture can
        /// therefore say "the name answers and the address does not" (a reviewer over the internet)
        /// or the reverse (a LAN with no DNS), which is the axis this whole unit turns on.
        ///
        /// `seen` records `Origin::log_form` — the bare authority for plaintext, the whole URL for
        /// TLS — so a probe order that reads plausibly cannot hide which transport each step took.
        fn dial(&self, o: &Origin) -> (i32, Vec<u8>) {
            self.seen.borrow_mut().push(o.log_form());
            match self.answers.iter().find(|(h, _, _)| *h == o.host()) {
                Some((s, st, b)) => {
                    let _ = s;
                    (*st, b.clone())
                }
                None => (0, Vec::new()), // nothing answered at that address
            }
        }
        fn seen(&self) -> Vec<String> {
            self.seen.borrow().clone()
        }
    }

    fn race_plan() -> ProbePlan {
        let candidate = |url: &str, address: &str, location: probe::Location| Candidate {
            url: url.into(),
            scheme: if url.starts_with("https://") {
                Scheme::Https
            } else {
                Scheme::Http
            },
            location,
            address: address.into(),
            port: 32400,
            ipv6: false,
        };
        ProbePlan {
            machine_id: "race-machine".into(),
            token: "race-token".into(),
            owned: true,
            name: "race-server".into(),
            source_title: None,
            candidates: vec![
                candidate(
                    "https://192-0-2-10.h.plex.direct:32400",
                    "192.0.2.10",
                    probe::Location::Local,
                ),
                candidate(
                    "https://203-0-113-9.h.plex.direct:32400",
                    "203.0.113.9",
                    probe::Location::Remote,
                ),
            ],
        }
    }

    fn test_policy() -> ProbeDeadlines {
        ProbeDeadlines {
            local: Duration::from_secs(1),
            remote: Duration::from_secs(1),
        }
    }

    fn threaded_spawn(_: usize, job: ProbeJob) -> bool {
        std::thread::spawn(job);
        true
    }

    #[test]
    fn retry_reuses_an_authorized_account_only_for_discovery_errors() {
        let mut old = owner::SessionInit::captured(Session {
            account_token: "persisted-but-not-authorized-now".into(),
            ..Session::default()
        });
        old.phase = Phase::Error;
        assert_eq!(
            retry_kind(old.phase, old.authorized_in_flow),
            RetryKind::Login
        );

        let mut current = old;
        current.authorized_in_flow = true;
        assert_eq!(
            retry_kind(current.phase, current.authorized_in_flow),
            RetryKind::Discovery
        );
        assert_eq!(retry_kind(Phase::Waiting, true), RetryKind::Login);
    }

    /// **A stalled DISCOVERY retries discovery, not the whole sign-in.** `ui::login` grows a
    /// `Try again` once a working phase has run long enough to look wedged, and discovery is the
    /// phase that reaches — it only runs after the pin has already yielded an account credential.
    /// Routing that press through `RetryKind::Login` minted a fresh QR and made the user
    /// authorize on their phone a second time for what is usually one unreachable server.
    #[test]
    fn a_stalled_discovery_retries_discovery_rather_than_minting_a_new_qr() {
        assert_eq!(retry_kind(Phase::Discovering, true), RetryKind::Discovery);
        assert_eq!(
            retry_kind(Phase::Discovering, false),
            RetryKind::Login,
            "…but discovery reached without an authorization in THIS flow has no token to reuse"
        );
        assert_eq!(
            retry_kind(Phase::Creating, true),
            RetryKind::Login,
            "and a stall before the pin exists can only start over"
        );
    }

    /// Completion order is responsiveness, never preference. A lower-scoring remote candidate
    /// finishing last cannot replace the local winner that already activated.
    #[test]
    fn a_worse_candidate_finishing_last_never_downgrades_the_winner() {
        let plan = race_plan();
        let dial: ProbeDial = Arc::new(|origin, _| {
            if origin.host().starts_with("203-") {
                std::thread::sleep(Duration::from_millis(30));
            }
            (200, identity_json("race-machine"))
        });
        let mut activated = Vec::new();
        let reach = probe_server_racing(
            &plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, _, origin| activated.push(origin.base()),
        );

        let Reach::At(candidate, _) = reach else {
            panic!("the local candidate must win")
        };
        assert_eq!(candidate.location, probe::Location::Local);
        assert_eq!(
            activated.len(),
            1,
            "the worse last result must not cause a re-point"
        );
        assert!(activated[0].contains("192-0-2-10"));
    }

    #[test]
    fn a_better_candidate_finishing_last_causes_exactly_one_final_repoint() {
        let mut plan = race_plan();
        plan.candidates.swap(0, 1); // remote launches first; local remains the better score
        let dial: ProbeDial = Arc::new(|origin, _| {
            if origin.host().starts_with("192-") {
                std::thread::sleep(Duration::from_millis(30));
            }
            (200, identity_json("race-machine"))
        });
        let mut activated = Vec::new();
        let reach = probe_server_racing(
            &plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, c, _| activated.push(c.location),
        );
        assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Local));
        assert_eq!(
            activated,
            [probe::Location::Remote, probe::Location::Local],
            "first usable, then one final best-score re-point"
        );
    }

    /// Pending means a worker really exists. Refusing one launch cannot leave the coordinator
    /// awaiting a message that can never be sent.
    #[test]
    fn one_refused_spawn_still_settles_on_the_worker_that_exists() {
        let plan = race_plan();
        let dial: ProbeDial = Arc::new(|_, _| (200, identity_json("race-machine")));
        let spawn = |index: usize, job: ProbeJob| {
            if index == 0 {
                false
            } else {
                std::thread::spawn(job);
                true
            }
        };
        let mut activated = Vec::new();
        let reach = probe_server_racing(&plan, dial, &spawn, test_policy(), &mut |_, c, _| {
            activated.push(c.location)
        });
        assert!(matches!(reach, Reach::At(..)));
        assert_eq!(activated, vec![probe::Location::Remote]);
    }

    #[test]
    fn all_refused_spawns_terminate_as_failure() {
        let plan = race_plan();
        let dial: ProbeDial = Arc::new(|_, _| panic!("a refused job must never run"));
        let mut activations = 0;
        let reach =
            probe_server_racing(&plan, dial, &|_, _| false, test_policy(), &mut |_, _, _| {
                activations += 1
            });
        assert!(matches!(reach, Reach::No));
        assert_eq!(activations, 0);
    }

    /// Relay is a second phase, not one more concurrent candidate. It is launched only after the
    /// non-relay set has settled without a winner.
    #[test]
    fn relay_is_dialled_only_after_every_nonrelay_candidate_settles() {
        let mut plan = race_plan();
        plan.candidates.truncate(1);
        plan.candidates.push(Candidate {
            url: "https://relay.example.test:443".into(),
            scheme: Scheme::Https,
            location: probe::Location::Relay,
            address: "relay.example.test".into(),
            port: 443,
            ipv6: false,
        });
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_by_dial = Arc::clone(&seen);
        let dial: ProbeDial = Arc::new(move |origin, _| {
            seen_by_dial.lock().unwrap().push(origin.host().to_string());
            if origin.host() == "relay.example.test" {
                (200, identity_json("race-machine"))
            } else {
                (0, Vec::new())
            }
        });
        let reach = probe_server_racing(
            &plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        );
        assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Relay));
        assert_eq!(
            seen.lock().unwrap().as_slice(),
            ["192-0-2-10.h.plex.direct", "relay.example.test"]
        );
    }

    #[test]
    fn a_reachable_relay_beats_a_direct_proxy_401() {
        let mut plan = race_plan();
        plan.candidates.truncate(1);
        plan.candidates.push(Candidate {
            url: "https://relay.example.test:443".into(),
            scheme: Scheme::Https,
            location: probe::Location::Relay,
            address: "relay.example.test".into(),
            port: 443,
            ipv6: false,
        });
        let dial: ProbeDial = Arc::new(|origin, _| {
            if origin.host() == "relay.example.test" {
                (200, identity_json("race-machine"))
            } else {
                (401, Vec::new())
            }
        });
        let reach = probe_server_racing(
            &plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        );
        assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Relay));
    }

    #[test]
    fn a_direct_401_remains_the_reason_when_relay_is_silent() {
        let mut plan = race_plan();
        plan.candidates.truncate(1);
        plan.candidates.push(Candidate {
            url: "https://relay.example.test:443".into(),
            scheme: Scheme::Https,
            location: probe::Location::Relay,
            address: "relay.example.test".into(),
            port: 443,
            ipv6: false,
        });
        let dial: ProbeDial = Arc::new(|origin, _| {
            if origin.host() == "relay.example.test" {
                (0, Vec::new())
            } else {
                (401, Vec::new())
            }
        });
        let reach = probe_server_racing(
            &plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        );
        assert!(matches!(reach, Reach::Refused));
    }

    /// A result's completion timestamp, not a delayed coordinator observation, decides whether it
    /// met the deadline. The injected spawn holds the coordinator after the job has already sent.
    #[test]
    fn an_on_time_result_queued_before_the_deadline_survives_coordinator_delay() {
        let mut plan = race_plan();
        plan.candidates.truncate(1);
        let dial: ProbeDial = Arc::new(|_, _| (200, identity_json("race-machine")));
        let spawn = |_: usize, job: ProbeJob| {
            job();
            std::thread::sleep(Duration::from_millis(20));
            true
        };
        let policy = ProbeDeadlines {
            local: Duration::from_millis(5),
            remote: Duration::from_millis(5),
        };
        let reach = probe_server_racing(&plan, dial, &spawn, policy, &mut |_, _, _| {});
        assert!(matches!(reach, Reach::At(..)));
    }

    #[test]
    fn a_late_local_result_is_ignored_while_a_remote_deadline_remains_live() {
        let plan = race_plan();
        let dial: ProbeDial = Arc::new(|origin, _| {
            if origin.host().starts_with("192-") {
                std::thread::sleep(Duration::from_millis(25));
            } else {
                std::thread::sleep(Duration::from_millis(35));
            }
            (200, identity_json("race-machine"))
        });
        let policy = ProbeDeadlines {
            local: Duration::from_millis(5),
            remote: Duration::from_millis(100),
        };
        let mut activated = Vec::new();
        let reach = probe_server_racing(&plan, dial, &threaded_spawn, policy, &mut |_, c, _| {
            activated.push(c.location)
        });
        assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Remote));
        assert_eq!(activated, [probe::Location::Remote]);
    }

    /// A proxy-specific 401 can race a verified answer on another origin. Reachability wins when
    /// identity was actually proved; 401 is the final reason only when no candidate reaches.
    #[test]
    fn a_verified_reachable_candidate_wins_over_a_parallel_401() {
        let plan = race_plan();
        let dial: ProbeDial = Arc::new(|origin, _| {
            if origin.host().starts_with("192-") {
                (401, Vec::new())
            } else {
                (200, identity_json("race-machine"))
            }
        });
        let reach = probe_server_racing(
            &plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        );
        assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Remote));
    }

    #[test]
    fn servers_are_serial_owned_then_public_match_with_one_gap_between_each() {
        let resources = vec![
            resource(
                r#"{"name":"unmatched","clientIdentifier":"shared-u","provides":"server",
                         "owned":false,"publicAddressMatches":false}"#,
            ),
            resource(
                r#"{"name":"owned","clientIdentifier":"owned","provides":"server",
                         "owned":true,"publicAddressMatches":false}"#,
            ),
            resource(
                r#"{"name":"matched","clientIdentifier":"shared-m","provides":"server",
                         "owned":false,"publicAddressMatches":true}"#,
            ),
        ];
        let mut order = Vec::new();
        let mut gaps = 0;
        let resolved = resolve_roster_using(
            &resources,
            &[],
            &mut |plan| {
                order.push(plan.machine_id.clone());
                Reach::No
            },
            &mut || gaps += 1,
            &mut |_, _, _| {},
        );
        assert!(matches!(resolved, Resolved::None { refused: false }));
        assert_eq!(order, ["owned", "shared-m", "shared-u"]);
        assert_eq!(
            gaps, 2,
            "three serial servers have exactly two inter-server gaps"
        );
    }

    #[test]
    fn every_server_settlement_publishes_its_specific_state_and_winning_tier() {
        let resources = vec![
            resource(r#"{"name":"yes","clientIdentifier":"yes","provides":"server","owned":true}"#),
            resource(
                r#"{"name":"denied","clientIdentifier":"denied","provides":"server","owned":false}"#,
            ),
            resource(
                r#"{"name":"off","clientIdentifier":"off","provides":"server","owned":false}"#,
            ),
        ];
        let winner = Candidate {
            url: "https://remote.example.test:32400".into(),
            scheme: Scheme::Https,
            location: probe::Location::Remote,
            address: "203.0.113.9".into(),
            port: 32400,
            ipv6: false,
        };
        let origin = winner.origin().expect("fixture origin");
        let mut observed = Vec::new();
        let resolved = resolve_roster_using(
            &resources,
            &[],
            &mut |plan| match plan.machine_id.as_str() {
                "yes" => Reach::At(winner.clone(), origin.clone()),
                "denied" => Reach::Refused,
                _ => Reach::No,
            },
            &mut || {},
            &mut |plan, outcome, tier| observed.push((plan.machine_id.clone(), outcome, tier)),
        );

        assert!(matches!(resolved, Resolved::Reached(ref roster) if roster.len() == 1));
        assert_eq!(
            observed,
            vec![
                (
                    "yes".into(),
                    Outcome::Reachable,
                    Some(probe::Location::Remote)
                ),
                ("denied".into(), Outcome::Unauthorized, None),
                ("off".into(), Outcome::Unreachable, None),
            ]
        );
    }

    #[test]
    fn a_changed_refresh_republishes_reached_unauthorized_and_offline_after_registry_replacement() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        let old = [
            crate::plex::register_for_test("yes", "10.0.0.1", 32400, "old", "cid"),
            crate::plex::register_for_test("denied", "10.0.0.2", 32400, "old", "cid"),
            crate::plex::register_for_test("off", "10.0.0.3", 32400, "old", "cid"),
        ];
        for id in old {
            crate::plex::publish_probe_result(id, Outcome::Reachable);
        }

        // The changed=true refresh path resets every old profile fact before installing the final
        // roster. These registrations stand in for install_roster without its network side effect.
        crate::plex::revoke_for_profile_switch();
        let installed = [
            crate::plex::register_for_test("yes", "10.0.0.1", 32400, "new", "cid"),
            crate::plex::register_for_test("denied", "10.0.0.2", 32400, "new", "cid"),
            crate::plex::register_for_test("off", "10.0.0.3", 32400, "new", "cid"),
        ];
        crate::plex::client_for(installed[0])
            .unwrap()
            .set_link(probe::Location::Remote);
        crate::plex::client_for(installed[1])
            .unwrap()
            .set_link(probe::Location::Local);
        crate::plex::client_for(installed[2])
            .unwrap()
            .set_link(probe::Location::Relay);
        crate::plex::finish_profile_switch(&installed);
        assert!(installed
            .iter()
            .all(|&id| crate::plex::server_probe_result(id).is_none()));

        publish_settled_probes(&[
            SettledProbe {
                machine_id: "yes".into(),
                outcome: Outcome::Reachable,
                tier: Some(probe::Location::Remote),
            },
            SettledProbe {
                machine_id: "denied".into(),
                outcome: Outcome::Unauthorized,
                tier: None,
            },
            SettledProbe {
                machine_id: "off".into(),
                outcome: Outcome::Unreachable,
                tier: None,
            },
        ]);

        assert_eq!(
            crate::plex::server_probe_result(installed[0]),
            Some(Outcome::Reachable)
        );
        assert_eq!(
            crate::plex::server_probe_result(installed[1]),
            Some(Outcome::Unauthorized)
        );
        assert_eq!(
            crate::plex::server_probe_result(installed[2]),
            Some(Outcome::Unreachable)
        );
        assert_eq!(
            crate::plex::client_for(installed[0]).unwrap().link(),
            Some(probe::Location::Remote)
        );
        assert_eq!(
            crate::plex::client_for(installed[1]).unwrap().link(),
            Some(probe::Location::Local)
        );
        assert_eq!(
            crate::plex::client_for(installed[2]).unwrap().link(),
            Some(probe::Location::Relay)
        );
        crate::plex::reset_servers_for_test();
    }

    /// **Identity is verified before a connection is accepted.** A candidate that answers is not
    /// the server we asked for: rule 1 of `probe.rs` is a live account of how a stranger's box on
    /// our own LAN answers a probe, and accepting it would register their machine under our
    /// friend's name and browse it.
    ///
    /// The wrong machine is discarded and the NEXT candidate is tried — a mismatch is a fact about
    /// that address, not about the server.
    #[test]
    fn a_response_from_the_wrong_machine_is_rejected_and_the_next_address_is_tried() {
        let plan = probe::plan(&a_share());
        let d = Dialled::new(vec![
            ("198-51-100-7.h.plex.direct", 200, identity_json("zzzz9999")), // someone else entirely
            ("203-0-113-9.h.plex.direct", 200, identity_json("bbbb2222")), // the server we asked for
        ]);

        match probe_server(&plan, &|o| d.dial(o)) {
            Reach::At(c, o) => {
                // The DIAGNOSTIC half is still the address plex.tv sent…
                assert_eq!((c.address.as_str(), c.port), ("203.0.113.9", 31234));
                // …and the origin is the NAME the certificate is issued for, which is the whole
                // reason `Reach::At` carries both. A roster rebuilt from `address` would store an
                // https origin no certificate matches.
                assert_eq!(o.base(), "https://203-0-113-9.h.plex.direct:31234");
            }
            _ => panic!(
                "the second address answers as the right machine: {:?}",
                d.seen()
            ),
        }
        // Every https candidate is tried before any plaintext one. Rule 1 keeps the guarded TLS
        // URI from the owner's LAN, but never its plaintext twin.
        assert_eq!(
            d.seen(),
            vec![
                "https://172-20-4-7.h.plex.direct:32400",
                "https://media.example.internal:31234",
                "https://198-51-100-7.h.plex.direct:31234",
                "https://203-0-113-9.h.plex.direct:31234",
            ]
        );

        // …and the same body from the wrong machine is never enough on its own
        assert_eq!(
            classify(200, &identity_json("zzzz9999"), "bbbb2222"),
            Outcome::WrongServer
        );
        assert_eq!(
            classify(200, &identity_json("bbbb2222"), "bbbb2222"),
            Outcome::Reachable
        );
        // a 200 that says nothing we can check is not an acceptance either
        assert_eq!(
            classify(200, b"<html>router login</html>", "bbbb2222"),
            Outcome::WrongServer
        );
        // nor is a resource plex.tv sent without an identity to verify against
        assert_eq!(
            classify(200, &identity_json("bbbb2222"), ""),
            Outcome::WrongServer
        );
    }

    /// The legacy synchronous seam stops at 401. Production races every direct candidate and lets
    /// relay follow a direct proxy 401; the coordinator tests above grade those semantics. This
    /// fixture remains only to pin the older one-at-a-time acceptance harness.
    #[test]
    fn the_legacy_sequential_seam_stops_at_401_instead_of_calling_it_a_dead_address() {
        assert_eq!(classify(401, b"", "bbbb2222"), Outcome::Unauthorized);
        // and it is the ONLY status that means this: a refusal of the endpoint, a dead gateway and
        // no answer at all are all just "try the next address"
        for s in [403, 404, 500, 502, 0] {
            assert_eq!(
                classify(s, b"", "bbbb2222"),
                Outcome::Unreachable,
                "status {s}"
            );
        }

        let plan = probe::plan(&a_share());
        let d = Dialled::new(vec![
            ("198-51-100-7.h.plex.direct", 401, Vec::new()),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        assert!(matches!(
            probe_server(&plan, &|o| d.dial(o)),
            Reach::Refused
        ));
        assert_eq!(
            d.seen(),
            vec![
                "https://172-20-4-7.h.plex.direct:32400",
                "https://media.example.internal:31234",
                "https://198-51-100-7.h.plex.direct:31234",
            ],
            "the 401 ends the SERVER: the address that would have answered is never even tried"
        );
    }

    /// **Every advertised address is dialable now, and the only thing that can still refuse one is
    /// a port no socket could take.** This test asserted the opposite for four shapes — an https
    /// origin, a hostname, a v6 literal, and by implication the whole `plex.direct` fleet — and
    /// each of those was true of a transport that no longer exists: `crate::http` routes TLS
    /// through libcurl, and `stream.rs` resolves names and dials either address family.
    ///
    /// The `probe_server` leg is the one that matters more than the table: it proves that opening
    /// the transport did not open the ACCEPTANCE. Candidates are dialled here until only the one
    /// nothing, and only the one whose `machineIdentifier` matches is accepted.
    #[test]
    fn every_advertised_address_is_dialable_and_only_an_impossible_port_is_not() {
        let plan = probe::plan(&a_share());
        assert_eq!(
            plan.candidates.len(),
            7,
            "guarded LAN TLS plus three remote uri/twin pairs"
        );
        assert!(
            plan.candidates.iter().all(dialable),
            "not one of them is refused any more: {plan:#?}",
            plan = plan.candidates
        );

        let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        assert!(matches!(probe_server(&plan, &|o| d.dial(o)), Reach::At(..)));
        // The owner's `172.20.x.x` connection keeps only the advertised TLS URI. Identity and the
        // certificate can reject a stranger there; the unsafe plaintext twin is never emitted.
        let seen = d.seen();
        assert!(
            seen.iter().any(|s| s.contains("172-20-4-7")),
            "the guarded TLS URI survives: {seen:?}"
        );
        assert!(
            !seen.iter().any(|s| s == "10.9.9.7:32400"),
            "the plaintext twin is absent: {seen:?}"
        );

        // The rule itself, stated on the candidates. The fixture builds `url` the way
        // `probe::candidates` does — from the SAME address and port — because that consistency is
        // the property `dial_target` relies on: it reads the origin off the URL, which is also what
        // gets recorded, so a fixture whose url and port disagree would assert nothing real.
        let cand = |scheme: Scheme, host: &str, port: i64| Candidate {
            url: format!(
                "{}://{}:{port}",
                scheme.as_str(),
                if host.contains(':') {
                    format!("[{host}]")
                } else {
                    host.to_string()
                }
            ),
            scheme,
            location: probe::Location::Remote,
            address: host.into(),
            port,
            ipv6: host.contains(':'),
        };
        let at = |host: &str| cand(Scheme::Http, host, 32400);
        assert!(dialable(&at("203.0.113.9")));
        assert!(
            dialable(&cand(Scheme::Https, "203-0-113-9.h.plex.direct", 31234)),
            "libcurl speaks TLS"
        );
        assert!(
            dialable(&at("media.example.internal")),
            "stream.rs resolves names now"
        );
        assert!(
            dialable(&at("2001:db8::1")),
            "…and dials either address family"
        );

        // …and the PORT is the one narrowing left. `4_294_999_696 as i32` is 32400, so without the
        // range check `probe::dial_port` applies — inside `Origin::parse` now, one layer down from
        // where it used to be — a nonsense answer from plex.tv would have been dialled at the most
        // ordinary port there is.
        assert!(!dialable(&cand(Scheme::Http, "203.0.113.9", 4_294_999_696)));
        assert!(!dialable(&cand(Scheme::Http, "203.0.113.9", 0)));
        assert!(!dialable(&cand(Scheme::Http, "203.0.113.9", 70_000)));

        // **The predicate hands back the ORIGIN, and it is the one `probe_server` dials and
        // `resolve_roster` records.** One value, so the address that answered and the address
        // written down cannot be two different things — and for an https candidate the two really
        // do differ, which is why this is a value rather than a bool.
        assert_eq!(
            dial_target(&at("203.0.113.9")),
            Some(crate::plex::Origin::http("203.0.113.9", 32400))
        );
        assert_eq!(
            dial_target(&cand(Scheme::Https, "203-0-113-9.h.plex.direct", 31234)).map(|o| o.base()),
            Some("https://203-0-113-9.h.plex.direct:31234".to_string())
        );
    }

    /// A candidate whose port cannot be dialled is SKIPPED, exactly as a hostname is — the next
    /// address gets its turn, and the server is not written off for one broken connection.
    ///
    /// The failure this prevents is silent in both directions: with a wrapping `as i32` the app
    /// dials port 32400 at that address, and whatever answers there is accepted the moment its
    /// `machineIdentifier` matches — which, on a server that really is at 32400, it does.
    #[test]
    fn an_undialable_port_costs_that_candidate_and_not_the_server() {
        let mut plan = probe::plan(&a_share());
        let good = plan
            .candidates
            .iter()
            .find(|c| dialable(c))
            .cloned()
            .expect("the share has one dialable candidate");
        // ahead of it, the same server at another address, advertised on a port that wraps
        plan.candidates.insert(
            0,
            Candidate {
                address: "192.0.2.55".into(),
                port: 4_294_999_696,
                ..good.clone()
            },
        );

        let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        assert!(
            matches!(probe_server(&plan, &|o| d.dial(o)), Reach::At(..)),
            "the good one still answers"
        );
        assert!(
            !d.seen().iter().any(|s| s.starts_with("192.0.2.55")),
            "the wrapping candidate was never dialled: {:?}",
            d.seen()
        );
    }

    /// **Only an address that ANSWERED is ever stored** — the guard that replaced
    /// `choose_local_connection`, which took the first `local` match and persisted it sight unseen,
    /// so one v6 address wrote an undialable server to disk and broke every later boot.
    ///
    /// The guard was once "this transport can only dial a dotted quad" and is now structural
    /// instead, which is strictly stronger: every advertised address is dialable, nothing but a
    /// candidate that answered as the right machine becomes a `SourceRef`, and the origin recorded
    /// is the very value that was dialled.
    ///
    /// The scenario is **a LAN with no route to the internet**, which is the case ranking TLS first
    /// costs something: every `plex.direct` name is probed and none resolves, and the plaintext
    /// twin — the address that works there — is what answers. That is the whole trade, priced.
    #[test]
    fn only_an_address_that_answered_is_ever_chosen_and_stored() {
        // our own server, v6 first — and the second v6 lies about its flag, which is why the shape
        // of the address is what decides rather than `IPv6`
        let res = resource(
            r#"{"name":"Mac mini","clientIdentifier":"aaaa1111","provides":"server","owned":true,
                "publicAddressMatches":false,"httpsRequired":false,"accessToken":"tok-own",
                "connections":[
                  {"protocol":"https","address":"2001:db8::1","port":32400,
                   "uri":"https://2001-db8--1.h.plex.direct:32400","local":true,"relay":false,"IPv6":true},
                  {"protocol":"https","address":"fd00::5","port":32400,"uri":"","local":true,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"192.168.0.10","port":32400,
                   "uri":"https://192-168-0-10.h.plex.direct:32400","local":true,"relay":false,"IPv6":false}]}"#,
        );
        let plan = probe::plan(&res);
        let d = Dialled::new(vec![
            // No plex.direct name resolves on an isolated LAN, so only the plaintext twins are
            // reachable — and both v6 ones answer too, so nothing but the ORDER decides.
            ("2001:db8::1", 200, identity_json("aaaa1111")),
            ("fd00::5", 200, identity_json("aaaa1111")),
            ("192.168.0.10", 200, identity_json("aaaa1111")),
        ]);

        match probe_server(&plan, &|o| d.dial(o)) {
            Reach::At(c, o) => {
                assert_eq!(
                    c.address, "192.168.0.10",
                    "IPv4 leads the plaintext fallbacks"
                );
                assert_eq!(
                    o.base(),
                    "http://192.168.0.10:32400",
                    "…and the origin recorded is what was dialled"
                );
            }
            _ => panic!("the LAN IPv4 answers: {:?}", d.seen()),
        }
        assert_eq!(
            d.seen(),
            vec![
                "https://192-168-0-10.h.plex.direct:32400",
                "https://2001-db8--1.h.plex.direct:32400",
                "192.168.0.10:32400",
            ],
            "TLS is tried first and costs two probes here; the twin is the fallback that answers"
        );
        // …and the v6 addresses are never reached, because a candidate that answers ends the walk
        assert!(
            !d.seen()
                .iter()
                .any(|s| s.contains("fd00") || s.contains("2001:db8")),
            "{:?}",
            d.seen()
        );
    }

    /// The one field that decides whether we trust a connection is scanned for, not deserialized:
    /// PMS answers XML unless an explicit JSON Accept survives to it, and a probe is the request
    /// most likely to meet a proxy that rewrites headers.
    #[test]
    fn the_machine_identifier_is_read_from_json_and_from_xml_alike() {
        assert_eq!(
            machine_id_in(&identity_json("abc123")).as_deref(),
            Some("abc123")
        );
        assert_eq!(
            machine_id_in(
                br#"<MediaContainer size="0" machineIdentifier="abc123" version="1.43.3"/>"#
            )
            .as_deref(),
            Some("abc123")
        );
        assert_eq!(
            machine_id_in(br#"{"MediaContainer":{"machineIdentifier" : "abc123"}}"#).as_deref(),
            Some("abc123")
        );
        // an empty value is no value — it must not read as "the next field"
        assert_eq!(machine_id_in(br#"{"machineIdentifier":"","size":0}"#), None);
        assert_eq!(machine_id_in(b"nothing here"), None);
        assert_eq!(machine_id_in(b""), None);
    }

    /// The account this feature exists for, as `/api/v2/resources` really returns it: OUR server
    /// (owned, LAN + public + relay) and the SHARE (not owned, the owner's 172.20 LAN, an internal
    /// hostname, and one public IPv4). Shaped on the live capture of 2026-08-11
    /// (`docs/shared-servers.md` §2) — the addresses are stand-ins, the arrangement is not, and the
    /// share is listed FIRST because plex.tv's order is not ours to rely on.
    fn a_two_server_account() -> Vec<Resource> {
        serde_json::from_str(
            r#"[
              {"name":"nas-home","clientIdentifier":"bbbb2222","provides":"server","owned":false,
               "sourceTitle":"friend","ownerId":987654,"publicAddressMatches":false,
               "httpsRequired":false,"accessToken":"tok-share","connections":[
                 {"protocol":"https","address":"10.9.9.7","port":32400,
                  "uri":"https://172-20-4-7.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
                 {"protocol":"https","address":"media.example.internal","port":31234,
                  "uri":"https://media.example.internal:31234","local":false,"relay":false,"IPv6":false},
                 {"protocol":"https","address":"203.0.113.9","port":31234,
                  "uri":"https://203-0-113-9.h.plex.direct:31234","local":false,"relay":false,"IPv6":false}]},
              {"name":"Mac mini","clientIdentifier":"aaaa1111","provides":"server","owned":true,
               "sourceTitle":null,"ownerId":null,"publicAddressMatches":false,"httpsRequired":false,
               "accessToken":"tok-own","connections":[
                 {"protocol":"https","address":"2001:db8::1","port":32400,
                  "uri":"https://2001-db8--1.h.plex.direct:32400","local":true,"relay":false,"IPv6":true},
                 {"protocol":"https","address":"192.168.0.10","port":32400,
                  "uri":"https://192-168-0-10.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
                 {"protocol":"https","address":"plex-relay.example.net","port":8443,
                  "uri":"https://plex-relay.example.net:8443","local":false,"relay":true,"IPv6":false}]},
              {"name":"someone's iPad","clientIdentifier":"cccc3333","provides":"player,controller",
               "accessToken":"tok-pad","connections":[]}
            ]"#,
        )
        .expect("fixture parses")
    }

    /// **What a real sign-in must produce.** The whole of discovery over the measured two-server
    /// account, with only the socket faked: this is the assertion that stands in for a device run,
    /// because everything downstream — Home, the library grid, playback — talks to whatever this
    /// function decided.
    ///
    /// Two servers, OURS FIRST (plex.tv listed the share first), each settled on the one address
    /// that answers from this TV: our LAN IPv4, and the share's PUBLIC IPv4 rather than the owner's
    /// 172.20 LAN. Each carries its own grant, and the non-server resource is not in the roster.
    #[test]
    fn a_sign_in_to_a_two_server_account_settles_on_one_address_each_ours_first() {
        let d = Dialled::new(vec![
            ("192.168.0.10", 200, identity_json("aaaa1111")),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        let Resolved::Reached(roster) =
            resolve_roster(&a_two_server_account(), &[], &|o| d.dial(o))
        else {
            panic!("both servers answer: {:?}", d.seen())
        };

        assert_eq!(roster.len(), 2, "a player resource is not a server");
        assert_eq!(
            primary_index(&roster),
            0,
            "ours is the primary and becomes `current`"
        );

        let own = &roster[0];
        assert!(own.owned && own.machine_id == "aaaa1111");
        assert_eq!(
            (own.address.as_str(), own.port),
            ("192.168.0.10", 32400),
            "the LAN v4, not the v6"
        );
        assert_eq!(own.token, "tok-own");
        assert!(
            own.shared_by.is_empty(),
            "an owned server has no owner to name"
        );

        let share = &roster[1];
        assert!(!share.owned && share.machine_id == "bbbb2222");
        assert_eq!(
            (share.address.as_str(), share.port),
            ("203.0.113.9", 31234),
            "the owner's 172.20 LAN is not ours to dial, and their hostname does not resolve"
        );
        assert_eq!(
            share.token, "tok-share",
            "a share is a separate authority: OUR token gets a 401"
        );
        assert_eq!(share.shared_by, "friend");
        assert!(
            roster.iter().all(|s| s.usable()),
            "every entry is dialable, so every one registers"
        );

        // OURS is probed first, though plex.tv listed the share first — that ordering is what
        // decides which library Home is built from. Within each server, TLS leads and the plaintext
        // twin is the fallback that answers on this (internet-less) LAN, and the walk STOPS at the
        // first acceptance: the relay is never reached, and neither is the share's plain hostname.
        assert_eq!(
            d.seen(),
            vec![
                "https://192-168-0-10.h.plex.direct:32400",
                "https://2001-db8--1.h.plex.direct:32400",
                "192.168.0.10:32400",
                "https://172-20-4-7.h.plex.direct:32400",
                "https://media.example.internal:31234",
                "https://203-0-113-9.h.plex.direct:31234",
                "203.0.113.9:31234",
            ]
        );
        assert!(
            !d.seen().iter().any(|s| s.contains("plex-relay")),
            "a 2 Mbit/s tunnel is a last resort"
        );
    }

    /// **The case this whole unit exists for: an account signed in from OUTSIDE the servers' LAN.**
    /// It is the shape an LG QA reviewer has — no PMS on their network, an account we supply — and
    /// before the TLS control plane it produced an empty roster and "Couldn't reach any Plex
    /// server", because every candidate that can work from there is an https `plex.direct` name and
    /// not one of them was dialable.
    ///
    /// Here nothing on either LAN answers. The share is reached at its public `plex.direct` name,
    /// and OUR server — which this fixture advertises no public direct address for, the ordinary
    /// shape when nobody has forwarded a port — is reached at its **relay**, the last candidate
    /// there is. What must come out is a roster whose origins are the NAMES a certificate is issued
    /// for, while `address`, the diagnostic half, still reads as whatever plex.tv sent.
    #[test]
    fn an_account_reached_only_over_the_public_internet_settles_on_its_https_origins() {
        let d = Dialled::new(vec![
            ("plex-relay.example.net", 200, identity_json("aaaa1111")),
            ("203-0-113-9.h.plex.direct", 200, identity_json("bbbb2222")),
        ]);
        let Resolved::Reached(roster) =
            resolve_roster(&a_two_server_account(), &[], &|o| d.dial(o))
        else {
            panic!("both servers answer over TLS: {:?}", d.seen())
        };

        assert_eq!(roster.len(), 2);
        assert_eq!(
            roster[0].origin_url, "https://plex-relay.example.net:8443",
            "ours, over the relay"
        );
        assert_eq!(
            roster[1].origin_url, "https://203-0-113-9.h.plex.direct:31234",
            "the share, direct"
        );
        for s in &roster {
            let o = s.origin().expect("a reached entry is dialable");
            assert!(
                o.is_tls(),
                "the connection that answered was TLS, so the stored origin must be"
            );
            assert_eq!(o.base(), s.origin_url, "the stored string round-trips");
        }
        // The share's stored origin is the NAME and its `address` is the quad behind it. That
        // inequality is the whole reason an origin is parsed from a URL rather than rebuilt from an
        // address: rebuild it and the certificate stops matching.
        assert_eq!(roster[1].address, "203.0.113.9");
        assert_ne!(
            roster[1].origin().expect("dialable").host(),
            roster[1].address
        );

        // The relay is genuinely LAST: every LAN candidate of our own server was tried first, and
        // the share's walk stopped the moment its public name answered.
        let seen = d.seen();
        assert_eq!(
            seen.last().map(String::as_str),
            Some("https://203-0-113-9.h.plex.direct:31234")
        );
        assert!(
            seen.iter().position(|x| x.contains("plex-relay")).unwrap() == 4,
            "four LAN candidates of ours precede the relay: {seen:?}"
        );
    }

    /// **Each roster entry's ORIGIN comes from the candidate's URL, not from its address.**
    ///
    /// A plaintext twin has the same host as `address`; an accepted TLS candidate deliberately
    /// does not. plex.tv advertises the `plex.direct` NAME in `uri` while `address` stays the quad
    /// behind it, so a roster rebuilt from `address` would store an origin no certificate matches.
    #[test]
    fn each_reached_entry_records_the_origin_its_url_named() {
        let d = Dialled::new(vec![
            ("192.168.0.10", 200, identity_json("aaaa1111")),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        let Resolved::Reached(roster) =
            resolve_roster(&a_two_server_account(), &[], &|o| d.dial(o))
        else {
            panic!("both servers answer")
        };

        assert_eq!(roster[0].origin_url, "http://192.168.0.10:32400");
        assert_eq!(roster[1].origin_url, "http://203.0.113.9:31234");
        // …and it is a parseable origin, so the registry gets one rather than the legacy fallback
        for s in &roster {
            let o = s.origin().expect("a reached entry is dialable");
            assert_eq!(o.base(), s.origin_url, "the stored string round-trips");
            assert!(
                !o.is_tls(),
                "these are the plaintext twins, and they answered"
            );
            // On a plaintext twin the URL's host IS the address, which is what makes this leg the
            // control for the https one above: there the two differ, and only the URL is right.
            assert_eq!((o.host(), o.port() as i64), (s.address.as_str(), s.port));
        }
    }

    /// The three ways discovery can come to nothing are three different things to say, and the one
    /// that used to be said for all of them ("No local Plex server found on this network") was the
    /// old policy talking rather than a description of what happened.
    #[test]
    fn the_three_empty_outcomes_are_distinguished() {
        let players = serde_json::from_str::<Vec<Resource>>(
            r#"[{"name":"iPad","clientIdentifier":"cccc3333","provides":"player","connections":[]}]"#,
        )
        .unwrap();
        assert!(matches!(
            resolve_roster(&players, &[], &|_| (0, Vec::new())),
            Resolved::NoServers
        ));

        // servers that simply do not answer
        let silent = Dialled::new(vec![]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &[], &|o| silent.dial(o)),
            Resolved::None { refused: false }
        ));

        // …and one that answers 401: something in front of it refuses unauthenticated requests,
        // which is not a network fault and must not be worded as one
        let refused = Dialled::new(vec![
            ("192.168.0.10", 401, Vec::new()),
            ("203.0.113.9", 401, Vec::new()),
        ]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &[], &|o| refused.dial(o)),
            Resolved::None { refused: true }
        ));

        // a share that answers while OUR server is off still signs in — a friend's library beats
        // "no server found" — and it becomes the primary because it is the only thing there is
        let one = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        let Resolved::Reached(roster) =
            resolve_roster(&a_two_server_account(), &[], &|o| one.dial(o))
        else {
            panic!("the share answered")
        };
        assert_eq!(roster.len(), 1);
        assert_eq!(primary_index(&roster), 0);
        assert!(
            !roster[0].owned,
            "the primary is a share here, and that is the point"
        );
    }

    /// A roster entry in the **LEGACY shape** — no stored `origin`, which is what every session
    /// file on every television written before that field carries. `..Default::default()` is what
    /// leaves it empty, so these fixtures also stand as the compatibility case: everything they
    /// assert about registration and re-keying runs through `SourceRef::origin`'s fallback.
    // ---- the offline profile seat, decided from the stored session alone ----

    fn cached_session(protected_pin: Option<&str>) -> Session {
        let mut s = Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        s.user = UserRef {
            uuid: "u-admin".into(),
            token: "admin-token".into(),
            ..Default::default()
        };
        s.server = ServerRef {
            machine_id: "ours".into(),
            address: "10.0.0.1".into(),
            port: 32400,
            token: "admin-token".into(),
            ..Default::default()
        };
        s.sources = vec![source("ours", true, "admin-token")];
        s.remember_profile(ProfileCreds {
            uuid: "u-admin".into(),
            user: s.user.clone(),
            server: s.server.clone(),
            sources: s.sources.clone(),
            pin: protected_pin.map(session::PinVerifier::new),
        });
        s.remember_profile(ProfileCreds {
            uuid: "u-kid".into(),
            user: UserRef {
                uuid: "u-kid".into(),
                token: "kid-token".into(),
                ..Default::default()
            },
            server: ServerRef {
                machine_id: "ours".into(),
                address: "10.0.0.1".into(),
                port: 32400,
                token: "kid-token".into(),
                ..Default::default()
            },
            sources: vec![source("ours", true, "kid-token")],
            pin: None,
        });
        s
    }

    /// The roster's uuid keys the record, whatever the `/switch` body says — an empty or
    /// differing response uuid must not produce an entry the next pick cannot find.
    #[test]
    fn a_seated_profile_is_recorded_under_the_roster_uuid() {
        let u = crate::plex::account::SwitchedUser {
            uuid: String::new(),
            ..Default::default()
        };
        assert_eq!(seated_uuid(&u, &tile("u-kid", false)), "u-kid");
        let u = crate::plex::account::SwitchedUser {
            uuid: "u-other".into(),
            ..Default::default()
        };
        assert_eq!(seated_uuid(&u, &tile("u-kid", false)), "u-kid");
        assert_eq!(
            seated_uuid(&u, &tile("", false)),
            "u-other",
            "no roster uuid: the response's"
        );
    }

    /// The plaintext twin may answer first, but a store build cannot make it live: only an
    /// https origin is activated there, while a developer build keeps its lab plaintext.
    #[test]
    fn a_store_build_never_makes_a_plaintext_origin_live() {
        let plain = Origin::http("192.168.0.10", 32400);
        let tls = Origin::parse("https://192-168-0-10.abc.plex.direct:32400").unwrap();
        assert!(!activation_allowed_by_policy(&plain, false));
        assert!(activation_allowed_by_policy(&tls, false));
        assert!(
            activation_allowed_by_policy(&plain, true),
            "a developer build keeps its lab server"
        );
    }

    fn tile(uuid: &str, protected: bool) -> UserTile {
        UserTile {
            uuid: uuid.into(),
            title: uuid.into(),
            protected,
            ..Default::default()
        }
    }

    /// The outage that motivated the cache (2026-09-06): the active profile is the PIN-protected
    /// admin, plex.tv is unreachable, and the PIN has to be checked by this television.
    #[test]
    fn a_protected_profile_is_seated_offline_on_its_pin_and_refused_on_any_other() {
        let stored = cached_session(Some("4821"));
        match offline_activation(&stored, &tile("u-admin", true), Some("4821")) {
            OfflineSwitch::Seat(next) => {
                assert_eq!(next.user.token, "admin-token");
                assert_eq!(
                    next.client_id, "cid",
                    "the account and its roster ride through"
                );
                assert_eq!(next.account_token, "acct");
            }
            _ => panic!("the right PIN seats the cached profile"),
        }
        assert!(matches!(
            offline_activation(&stored, &tile("u-admin", true), Some("0000")),
            OfflineSwitch::PinDenied
        ));
        assert!(matches!(
            offline_activation(&stored, &tile("u-admin", true), None),
            OfflineSwitch::PinDenied
        ));
        assert!(matches!(
            offline_activation(&stored, &tile("u-admin", true), Some("")),
            OfflineSwitch::PinDenied
        ));
    }

    /// A protected profile whose record predates the verifier cannot be checked, so it is not
    /// seated — "no cache", never "no PIN".
    #[test]
    fn a_protected_profile_cached_without_a_verifier_is_not_seated() {
        let stored = cached_session(None);
        assert!(matches!(
            offline_activation(&stored, &tile("u-admin", true), Some("4821")),
            OfflineSwitch::NoCache
        ));
    }

    #[test]
    fn an_unprotected_cached_profile_is_seated_on_the_pick_alone() {
        let stored = cached_session(Some("4821"));
        match offline_activation(&stored, &tile("u-kid", false), None) {
            OfflineSwitch::Seat(next) => {
                assert_eq!(next.user.uuid, "u-kid");
                assert_eq!(next.user.token, "kid-token");
                assert_eq!(next.server.token, "kid-token");
                assert_eq!(next.sources[0].token, "kid-token");
                assert_eq!(
                    next.profiles.len(),
                    2,
                    "the cache itself is kept for the next pick"
                );
            }
            _ => panic!("an unprotected cached profile seats without a network"),
        }
    }

    #[test]
    fn a_profile_this_television_never_seated_online_has_nothing_to_seat() {
        let stored = cached_session(Some("4821"));
        assert!(matches!(
            offline_activation(&stored, &tile("u-guest", false), None),
            OfflineSwitch::NoCache
        ));
        assert!(matches!(
            offline_activation(&Session::default(), &tile("u-admin", true), Some("4821")),
            OfflineSwitch::NoCache
        ));
    }

    /// The seating paths that never see a PIN still write the record for a PIN-free profile,
    /// so a session stored before the cache existed becomes seatable offline on first use.
    #[test]
    fn seating_an_unprotected_active_profile_records_it_and_a_protected_one_is_left_to_the_switch()
    {
        let mut s = cached_session(None);
        s.profiles.clear();
        s.home_users = vec![session::HomeUserRef {
            uuid: "u-admin".into(),
            protected: false,
            ..Default::default()
        }];
        remember_unprotected_active(&mut s);
        assert_eq!(s.profiles.len(), 1);
        assert_eq!(
            s.cached_profile("u-admin").unwrap().user.token,
            "admin-token"
        );

        let mut p = cached_session(None);
        p.profiles.clear();
        p.home_users = vec![session::HomeUserRef {
            uuid: "u-admin".into(),
            protected: true,
            ..Default::default()
        }];
        remember_unprotected_active(&mut p);
        assert!(
            p.profiles.is_empty(),
            "no PIN in hand, no verifier to write"
        );

        let mut none = Session::default();
        remember_unprotected_active(&mut none);
        assert!(
            none.profiles.is_empty(),
            "an account without Plex Home names no profile"
        );
    }

    fn source(machine_id: &str, owned: bool, token: &str) -> SourceRef {
        SourceRef {
            machine_id: machine_id.into(),
            name: machine_id.into(),
            shared_by: if owned {
                String::new()
            } else {
                "friend".into()
            },
            owned,
            address: "10.0.0.1".into(),
            port: 32400,
            token: token.into(),
            ..Default::default()
        }
    }

    /// Our own server registers first and is the primary, whatever order plex.tv listed the account
    /// in — because the registry makes the first registration `current` when nothing is yet, so the
    /// ordering is what stops a boot coming up pointed at a friend's server and building Home from
    /// their library.
    #[test]
    fn our_own_server_leads_the_roster_however_plex_tv_ordered_it() {
        let roster = vec![
            source("share-1", false, "t1"),
            source("ours", true, "t2"),
            source("share-2", false, "t3"),
        ];
        assert_eq!(
            registration_order(&roster),
            vec![1, 0, 2],
            "ours first, then plex.tv's own order"
        );
        assert_eq!(primary_index(&roster), 1);

        // an entry with no credential (or no address) cannot be dialled, so it is not registered —
        // registering it would put a `Client` in the table that 401s everything asked of it
        let mut half = roster.clone();
        half[0].token.clear();
        half[2].address.clear();
        assert_eq!(registration_order(&half), vec![1]);

        // a shares-only roster (our own box is off) still yields a primary rather than nothing:
        // a friend's library is a better app than "no server found"
        let shares = vec![
            source("share-1", false, "t1"),
            source("share-2", false, "t3"),
        ];
        assert_eq!(primary_index(&shares), 0);
        assert_eq!(registration_order(&shares), vec![0, 1]);
    }

    /// A profile switch re-keys the WHOLE roster, not just the primary. `accessToken` is per
    /// (user, server), so the other profile's token on a share is a 401 waiting to happen — and a
    /// server this profile has not been granted becomes an inert, tokenless cache entry rather
    /// than lingering with a credential that works or losing the verified address forever.
    #[test]
    fn switching_profile_re_keys_every_source_and_drops_the_ones_not_granted() {
        let roster = vec![
            source("ours", true, "old-own"),
            source("share-1", false, "old-share"),
            source("gone", false, "old-gone"),
        ];
        let rs = vec![
            resource(
                r#"{"clientIdentifier":"ours","provides":"server","owned":true,"accessToken":"new-own"}"#,
            ),
            resource(
                r#"{"clientIdentifier":"share-1","provides":"server","owned":false,"accessToken":"new-share"}"#,
            ),
        ];

        let next = retoken(&roster, &rs);
        assert_eq!(
            next.len(),
            3,
            "the un-granted server remains only as address metadata"
        );
        assert_eq!(next[0].token, "new-own");
        assert_eq!(
            (next[1].machine_id.as_str(), next[1].token.as_str()),
            ("share-1", "new-share")
        );
        assert_eq!(
            next[1].shared_by, "friend",
            "everything but the token is carried over"
        );
        assert_eq!(
            next[1].address, "10.0.0.1",
            "including the address discovery probed"
        );

        assert_eq!(next[2].machine_id, "gone");
        assert!(
            next[2].token.is_empty() && !next[2].usable(),
            "the old profile credential is gone"
        );

        // Switching back can restore that cached machine without rediscovering its address.
        let restored = retoken(
            &next,
            &[resource(
                r#"{"clientIdentifier":"gone","provides":"server","accessToken":"back"}"#,
            )],
        );
        assert_eq!(restored[2].token, "back");
        assert!(restored[2].usable());

        // a resource that came back WITHOUT a token for this profile remains inert
        let empty = vec![resource(
            r#"{"clientIdentifier":"ours","provides":"server","accessToken":""}"#,
        )];
        let without = retoken(&roster, &empty);
        assert_eq!(without.len(), 3);
        assert!(without.iter().all(|s| s.token.is_empty()));
        // and an entry with no identity cannot be re-keyed, and must never match by emptiness
        let anon = vec![source("", false, "old")];
        assert!(retoken(
            &anon,
            &[resource(r#"{"provides":"server","accessToken":"x"}"#)]
        )
        .is_empty());
    }

    /// The incident this change fixes: an owner refresh found the public HTTPS route while the
    /// protected-profile switch was in flight, then the switch re-keyed the old LAN snapshot and
    /// discarded that winner. The selected profile owns both halves of the answer — its grant
    /// token and the endpoint verified with that token — so they must land together.
    #[test]
    fn profile_activation_keeps_a_fresh_wan_winner_instead_of_the_cached_lan_origin() {
        let mut cached = source("ours", true, "owner-token");
        cached.address = "192.0.2.10".into();
        cached.origin_url = "http://192.0.2.10:32400".into();

        let mut wan = source("ours", true, "profile-token");
        wan.address = "203.0.113.9".into();
        wan.origin_url = "https://203-0-113-9.example.test:32400".into();
        wan.tier = Some(probe::Location::Remote);

        let resources = vec![resource(
            r#"{"name":"ours","clientIdentifier":"ours","provides":"server","owned":true,
                "accessToken":"profile-token"}"#,
        )];
        let next = profile_sources(&[cached], &[wan], &resources, &[]);

        assert_eq!(next.len(), 1);
        assert_eq!(next[0].token, "profile-token");
        assert_eq!(next[0].address, "203.0.113.9");
        assert_eq!(next[0].origin_url, "https://203-0-113-9.example.test:32400");
        assert_eq!(next[0].tier, Some(probe::Location::Remote));
    }

    /// Network recovery may fetch the connection list with the install owner's account token,
    /// even though the active managed profile has its own PMS token. Only route facts may cross
    /// that seam: copying the Resource credential would make the next request run as the owner.
    #[test]
    fn endpoint_recovery_repoints_an_existing_source_without_replacing_profile_grants() {
        let mut cached = source("ours", true, "managed-profile-token");
        cached.address = "203.0.113.9".into();
        cached.origin_url = "https://public.example.test:32400".into();
        cached.tier = Some(probe::Location::Remote);
        let mut session = Session {
            server: server_ref(&cached),
            sources: vec![cached],
            ..Default::default()
        };

        let mut lan = source("ours", true, "owner-resource-token");
        lan.address = "192.0.2.10".into();
        lan.origin_url = "https://lan.example.test:32400".into();
        lan.tier = Some(probe::Location::Local);
        let (landed, changed) = apply_refreshed_endpoint(&mut session, "ours", &lan).unwrap();

        assert!(changed);
        assert_eq!(landed.address, "192.0.2.10");
        assert_eq!(landed.origin_url, "https://lan.example.test:32400");
        assert_eq!(landed.tier, Some(probe::Location::Local));
        assert_eq!(landed.token, "managed-profile-token");
        assert_eq!(session.server.token, "managed-profile-token");
        assert_eq!(session.server.origin_url, "https://lan.example.test:32400");
        assert_eq!(session.sources.len(), 1, "recovery cannot add a grant");
    }

    #[test]
    fn endpoint_recovery_cannot_introduce_a_server_outside_the_profile_roster() {
        let cached = source("ours", true, "profile-token");
        let mut session = Session {
            server: server_ref(&cached),
            sources: vec![cached],
            ..Default::default()
        };
        let fresh_share = source("owner-only-share", false, "owner-token");

        assert!(apply_refreshed_endpoint(&mut session, "owner-only-share", &fresh_share).is_none());
        assert_eq!(session.sources.len(), 1);
        assert_eq!(session.sources[0].machine_id, "ours");
    }

    #[test]
    fn profile_activation_promotes_a_surviving_share_when_primary_is_revoked() {
        let stored = vec![
            source("revoked-primary", true, "old-owner"),
            source("surviving-share", false, "old-share"),
        ];
        let resources = vec![resource(
            r#"{"name":"club","clientIdentifier":"surviving-share","provides":"server",
                "owned":false,"sourceTitle":"friend","accessToken":"profile-share"}"#,
        )];

        let next = profile_sources(&stored, &[], &resources, &[]);

        assert_eq!(next.len(), 1);
        assert_eq!(next[0].machine_id, "surviving-share");
        assert_eq!(next[0].token, "profile-share");
        assert_eq!(primary_index(&next), 0);
    }

    /// **A Plex Home managed user's own household server must not be credited to the admin.**
    ///
    /// This is the reported bug ("Shared by Gleb" on the user's OWN server), reproduced at the one
    /// layer that decides it: a profile switch re-fetches `/api/v2/resources` with the SWITCHED
    /// user's token (`switch_thread`), and plex.tv answers about that user — so the household's
    /// own server comes back `owned:false` with the admin's handle in `sourceTitle`. Fed straight
    /// into `SourceRef::shared_by` that is a credit naming the person watching.
    ///
    /// The shape is the live 2026-09-03 `/api/v2/resources` shape with stand-in identities: an
    /// owned server carries `sourceTitle:null`/`ownerId:null`, a share carries a handle and the
    /// owner's plex.tv id, and `ownerId` is in the same id space as `/api/v2/home/users[].id`
    /// (measured: the admin row's `id` equals `/api/v2/user`'s `id`).
    #[test]
    fn a_home_admins_server_seen_by_a_managed_profile_credits_nobody() {
        const ADMIN_ID: i64 = 111_111;
        const MANAGED_ID: i64 = 222_222;
        const FRIEND_ID: i64 = 987_654;
        let household = [ADMIN_ID, MANAGED_ID];

        // What the admin's own sign-in wrote down: the household server is ours, the share is not.
        let stored = vec![
            source("aaaa1111", true, "own-tok"),
            source("bbbb2222", false, "share-tok"),
        ];
        // What plex.tv says to the MANAGED user's token: nothing is owned, and the household
        // server now names the admin.
        let resources = vec![
            resource(
                r#"{"name":"Mac mini","clientIdentifier":"aaaa1111","provides":"server",
                    "owned":false,"home":true,"sourceTitle":"admin","ownerId":111111,
                    "accessToken":"kid-own"}"#,
            ),
            resource(
                r#"{"name":"nas-home","clientIdentifier":"bbbb2222","provides":"server",
                    "owned":false,"home":false,"sourceTitle":"friend","ownerId":987654,
                    "accessToken":"kid-share"}"#,
            ),
        ];

        let next = refreshed_sources(&stored, &[], &resources, &household);

        assert!(
            next[0].shared_by.is_empty(),
            "the household's own server credits nobody, whichever profile is watching — got {:?}",
            next[0].shared_by
        );
        assert_eq!(
            next[1].shared_by, "friend",
            "a person outside the household is still credited"
        );
        let _ = FRIEND_ID;
    }

    #[test]
    fn refresh_keeps_a_still_granted_offline_share_and_drops_only_a_revoked_grant() {
        let stored = vec![
            source("ours", true, "old-own"),
            source("offline-share", false, "old-share"),
            source("revoked", false, "old-revoked"),
        ];
        let mut reached_own = source("ours", true, "new-own");
        reached_own.address = "10.0.0.42".into();
        let reached = vec![reached_own];
        let resources = vec![
            resource(
                r#"{"name":"ours-now","clientIdentifier":"ours","provides":"server","owned":true,
                    "accessToken":"new-own","publicAddressMatches":true}"#,
            ),
            resource(
                r#"{"name":"friend-box","clientIdentifier":"offline-share","provides":"server","owned":false,
                    "sourceTitle":"friend","accessToken":"new-share"}"#,
            ),
            resource(
                r#"{"name":"brand-new-but-offline","clientIdentifier":"new-share","provides":"server",
                    "owned":false,"sourceTitle":"other","accessToken":"new-token"}"#,
            ),
        ];

        let next = refreshed_sources(&stored, &reached, &resources, &[]);
        assert_eq!(
            next.iter()
                .map(|s| s.machine_id.as_str())
                .collect::<Vec<_>>(),
            ["ours", "offline-share"]
        );
        assert_eq!(
            next[0].address, "10.0.0.42",
            "a reached server takes its freshly verified origin"
        );
        assert_eq!(
            next[1].address, "10.0.0.1",
            "an offline but still-granted share keeps its verified address"
        );
        assert_eq!(
            next[1].token, "new-share",
            "but follows the current grant's credential"
        );
        assert!(
            !next.iter().any(|s| s.machine_id == "revoked"),
            "absence from resources is authoritative"
        );
        assert!(
            !next.iter().any(|s| s.machine_id == "new-share"),
            "no address is invented for an unseen server"
        );
    }

    /// The primary in the **LEGACY shape** — see [`source`] above.
    fn primary(machine_id: &str, address: &str, port: i64, token: &str) -> ServerRef {
        ServerRef {
            name: "Mac mini".into(),
            machine_id: machine_id.into(),
            address: address.into(),
            port,
            token: token.into(),
            ..Default::default()
        }
    }

    /// **The two records of the same server must not drift.** `Session::server` is what `app.rs`
    /// boots on and `Session::sources` is what everything else reads, and the online roster refresh
    /// only ever rewrote the second — so the day the house's PMS took a new LAN address, every boot
    /// went on dialling the dead one, and `plex::install` of that address registered a SECOND slot
    /// for a machine already in the table (the legacy install has no id to match on) with the dead
    /// copy made current.
    #[test]
    fn a_primary_that_moved_is_followed_by_the_roster_refresh() {
        let mut s = primary("aaaa1111", "192.168.0.10", 32400, "tok-own");
        let mut moved = source("aaaa1111", true, "tok-own2");
        moved.address = "192.168.0.42".into();
        moved.port = 32400;
        let share = source("bbbb2222", false, "tok-share");

        assert!(
            reconcile_primary(&mut s, &[share.clone(), moved.clone()]),
            "the save is owed"
        );
        assert_eq!((s.address.as_str(), s.port), ("192.168.0.42", 32400));
        assert_eq!(
            s.token, "tok-own2",
            "the grant came from the same answer as the address"
        );
        assert_eq!(
            s.machine_id, "aaaa1111",
            "the identity is the KEY here, never something to rewrite"
        );

        // idempotent — a refresh that learns nothing new must not force a flash write every boot
        assert!(!reconcile_primary(&mut s, &[share.clone(), moved.clone()]));

        // a roster that does not name this machine says nothing about it: our own box being off
        // must not blank the address the next boot needs
        let mut off = primary("aaaa1111", "192.168.0.10", 32400, "tok-own");
        assert!(!reconcile_primary(&mut off, &[share.clone()]));
        assert_eq!(off.address, "192.168.0.10");

        // an entry with nothing to dial is not an address to adopt…
        let mut half = moved.clone();
        half.token.clear();
        let mut s2 = primary("aaaa1111", "192.168.0.10", 32400, "tok-own");
        assert!(!reconcile_primary(&mut s2, &[half]));
        assert_eq!(s2.address, "192.168.0.10");

        // …and a primary with no machine id cannot be matched at all — `retoken`'s rule, because an
        // empty id must never match a roster entry that also happens to have none
        let mut anon = primary("", "192.168.0.10", 32400, "tok-own");
        let mut anon_src = source("", true, "tok-x");
        anon_src.address = "10.9.9.9".into();
        assert!(!reconcile_primary(&mut anon, &[anon_src]));
        assert_eq!(anon.address, "192.168.0.10");
    }

    #[test]
    fn a_removed_primary_promotes_the_preferred_surviving_grant_but_an_empty_answer_erases_nothing()
    {
        let mut old = primary("gone", "10.0.0.1", 32400, "old");
        let share = source("share", false, "share-token");
        assert!(reconcile_refresh_primary(&mut old, &[share.clone()]));
        assert_eq!(old.machine_id, "share");
        assert_eq!(old.token, "share-token");

        let before = old.clone();
        assert!(!reconcile_refresh_primary(&mut old, &[]));
        assert_eq!(old.machine_id, before.machine_id);
        assert_eq!(old.address, before.address);
        assert_eq!(old.token, before.token);
    }

    #[test]
    fn a_refresh_moves_the_active_home_users_token_with_same_or_replaced_primary() {
        let mut sess = Session {
            server: primary("ours", "10.0.0.1", 32400, "old-server"),
            user: UserRef {
                uuid: "owner".into(),
                token: "old-user".into(),
                ..UserRef::default()
            },
            ..Session::default()
        };

        let fresh_ours = source("ours", true, "fresh-own");
        assert!(reconcile_refresh_session(&mut sess, &[fresh_ours]));
        assert_eq!(sess.server.token, "fresh-own");
        assert_eq!(
            sess.pms_token(),
            "fresh-own",
            "a same-primary token rotation reaches the next boot"
        );

        let survivor = source("share", false, "fresh-share");
        assert!(reconcile_refresh_session(&mut sess, &[survivor]));
        assert_eq!(sess.server.machine_id, "share");
        assert_eq!(
            sess.pms_token(),
            "fresh-share",
            "a promoted primary never inherits the removed PMS's token"
        );
    }

    /// A signed-in device in the ordinary Plex Home arrangement: the adult profile carries the PIN,
    /// the child's does not, and `uuid` picks which of them the stored session would resume as.
    fn signed_in_as(uuid: &str) -> Session {
        Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            server: ServerRef {
                name: "nas".into(),
                machine_id: "aaaa1111".into(),
                address: "192.168.0.10".into(),
                port: 32400,
                token: "tok-own".into(),
                ..Default::default()
            },
            user: UserRef {
                uuid: uuid.into(),
                title: "stored".into(),
                token: "tok-user".into(),
                ..Default::default()
            },
            home_users: vec![
                session::HomeUserRef {
                    uuid: "u-adult".into(),
                    title: "Gleb".into(),
                    protected: true,
                    admin: true,
                    ..Default::default()
                },
                session::HomeUserRef {
                    uuid: "u-kid".into(),
                    title: "Kid".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }
    }

    #[test]
    fn picker_policy_defaults_detachment_and_refusal_reasons_remain_exact() {
        for (picker, protected, allowed) in [
            (Picker::Boot, true, false), (Picker::Boot, false, true),
            (Picker::ChangeProfile, true, false), (Picker::ChangeProfile, false, false),
            (Picker::SignedIn, true, false), (Picker::SignedIn, false, false),
        ] { assert_eq!(may_resume(picker, protected), allowed); }
        assert_eq!(Picker::default(), Picker::Boot);
        assert!(detaches_active_profile(Picker::ChangeProfile));
        assert!(!detaches_active_profile(Picker::Boot));
        assert!(!detaches_active_profile(Picker::SignedIn));
        let adult = signed_in_as("u-adult");
        let kid = signed_in_as("u-kid");
        assert!(adult.active_profile_is_protected());
        assert!(!kid.active_profile_is_protected());
        assert_eq!(refusal_reason(Picker::ChangeProfile, &kid),
            "auth: BACK refused — the Change-profile picker is a root");
        assert_eq!(refusal_reason(Picker::Boot, &adult),
            "auth: BACK refused — the stored profile is PIN-protected");
        let unchosen = Session { user: UserRef::default(), ..adult };
        assert_eq!(refusal_reason(Picker::SignedIn, &unchosen),
            "auth: BACK refused — no profile has been chosen on this device yet");
    }

    /// **A wrong PIN must not follow the user back to the roster.** Reported as a *"strange 'Switch
    /// Profile — Check the PIN' element"* appearing on Who's Watching after a rejected PIN.
    ///
    /// It is `switch_thread`'s failure banner. The pad and the roster are two surfaces and only one
    /// of them is asking about a PIN: `ui::profiles::draw` paints `auth::error()` under the avatar
    /// row whenever the pad is closed, so the moment BACK dismissed the keypad the string the pad
    /// had already answered with a red flash reappeared under the faces — blaming a PIN nobody was
    /// being asked for any more, on the one screen where every profile is a candidate.
    ///
    /// So a PIN-blaming failure leaves NO roster banner. Everything else keeps one, because the
    /// roster is exactly where "no access to this server" or "check the connection" belongs — the
    /// pad closes for those (`ui::profiles::update`), and a screen that swallowed the choice with
    /// no read-out at all is the failure this banner was added for.
    #[test]
    fn a_rejected_pin_leaves_no_error_on_the_who_s_watching_roster() {
        let (banner, denied) = switch_failure(true);
        assert!(
            banner.is_empty(),
            "a PIN-blaming failure must leave the roster's error band EMPTY — got {banner:?}"
        );
        assert!(denied, "…and must still flash the pad's dots");

        let (banner, denied) = switch_failure(false);
        assert!(
            !banner.is_empty(),
            "a switch that failed for any other reason still owes the roster a read-out"
        );
        assert!(
            !denied,
            "…and must not flash the pad red, which reads as a typo to retry forever"
        );
    }

    // ---- the QR sign-in that could not end (issue #30) ----

    /// **A press may only act on the wait the screen actually timed**, and both halves of that
    /// identity are a defect that was live for one review round.
    ///
    /// The PHASE: the sign-in screen offers its escape in the DRAW and takes it on the next key,
    /// and between those the poll can return a token and walk the flow to `Ready` — where a
    /// restart mints a fresh pin over a sign-in that had just succeeded. Worse, the two escapes
    /// share one clock, so a wait that moved `Waiting → Discovering` made the QR predicate false
    /// and the STALLED-SPINNER one true, on the dead code's timer, down what used to be an
    /// unguarded path.
    ///
    /// The CODE: a wait is now replaced automatically without the phase changing, so a press timed
    /// against the code that expired would throw away the one that replaced it a moment ago.
    #[test]
    fn a_restart_acts_only_on_the_wait_that_earned_it() {
        // the rule, as a table
        let timed = (Phase::Waiting, 7u64);
        assert!(restart_permitted(Some(timed), timed));
        assert!(
            !restart_permitted(Some(timed), (Phase::Ready, 7)),
            "the sign-in succeeded between the draw and the key"
        );
        assert!(
            !restart_permitted(Some(timed), (Phase::Discovering, 7)),
            "…or merely moved on, which the OTHER escape would have accepted on this same clock"
        );
        assert!(
            !restart_permitted(Some(timed), (Phase::Waiting, 8)),
            "the code was replaced automatically while the key was in flight"
        );
        assert!(
            restart_permitted(None, (Phase::Ready, 99)),
            "the settled read-out's own control has no live wait to be wrong about"
        );
    }

    /// **One `SignInStarted` per attempt**, which `diag::schema` states as a contract: a start is
    /// bracketed by exactly one completed/failed/cancelled. Both of the sign-in screen's timed
    /// escapes restart a wait that is still UNSETTLED, so reporting a second start against the one
    /// settle that eventually follows would leave every stalled sign-in over-counted.
    #[test]
    fn restarting_a_live_wait_is_the_same_attempt_carrying_on() {
        assert!(
            !restart_is_a_new_attempt(true),
            "a stalled spinner or an unscanned code is already being counted"
        );
        assert!(
            restart_is_a_new_attempt(false),
            "…while an error read-out has reported its failure and the next press opens a new \
             bracket"
        );
    }

    /// A scripted pin: answers from a list, and a clock that moves only when the loop waits or
    /// polls. A fifteen-minute pin therefore runs to its death in microseconds.
    struct ScriptedPin {
        answers: std::collections::VecDeque<PinPoll>,
        clock: Duration,
        waits: Vec<Duration>,
        /// The wait (by index) at which a newer flow takes the screen.
        superseded_at: Option<usize>,
        polls: usize,
        /// What one request costs. Settable because it is the axis the old iteration count was
        /// blind to, and because `net::API` lets one poll cost 25 s.
        poll_cost: Duration,
    }

    impl ScriptedPin {
        fn new(answers: Vec<PinPoll>) -> ScriptedPin {
            ScriptedPin {
                answers: answers.into(),
                clock: Duration::ZERO,
                waits: Vec::new(),
                superseded_at: None,
                polls: 0,
                poll_cost: Duration::from_millis(300),
            }
        }
    }

    impl PinWatch for ScriptedPin {
        fn poll(&mut self) -> PinPoll {
            self.polls += 1;
            // A poll costs a round trip. That cost is the whole of the second defect: the old loop
            // counted ITERATIONS and paid this on top of every one of them, so its window was
            // always longer than the pin it was watching — by minutes on a healthy link and by
            // hours against `net::API`'s 25 s deadline.
            self.clock += self.poll_cost;
            self.answers.pop_front().unwrap_or(PinPoll::Unreachable)
        }
        fn wait(&mut self, d: Duration) -> bool {
            if self.superseded_at == Some(self.waits.len()) {
                return false;
            }
            self.waits.push(d);
            self.clock += d;
            true
        }
        fn elapsed(&self) -> Duration {
            self.clock
        }
    }

    /// **The wait ends with the pin, not some multiple of it.**
    ///
    /// plex.tv mints a code with `expiresIn: 900` and answers a poll of a dead one with
    /// `404 {"code":1020,"message":"Code not found or expired"}` (both measured against the live
    /// service, 2026-09-03). The loop this replaced was bounded at 450 ITERATIONS, each costing a
    /// 2 s sleep plus a round trip — 1035 s at a fast 300 ms RTT, and 12150 s if every poll ran to
    /// `net::API`'s 25 s deadline. All of that time was spent on a screen that said "Waiting for
    /// you to sign in…" over a code nothing could ever authorize.
    #[test]
    fn the_wait_for_one_code_cannot_outlive_that_code() {
        let window = pin_window(900);
        assert_eq!(window, Duration::from_secs(900), "plex.tv's own expiresIn");

        // nothing ever answers: the pathological case, and the one that used to run for hours
        let mut w = ScriptedPin::new(Vec::new());
        assert_eq!(poll_for_token(&mut w, window), PollEnd::Expired);
        assert!(
            w.clock >= window,
            "it did wait out the code it was given, rather than giving up early"
        );
        assert!(
            w.clock <= window + w.poll_cost,
            "…and overran it by at most the ONE request that was in flight when the deadline \
             passed — the clamped pauses land the last poll exactly on it — not by a whole \
             backoff, and certainly not by the 1035s the iteration count allowed"
        );
    }

    /// **Exactly one poll may cross the deadline.** The request that began before expiry is always
    /// allowed to answer — that is the token-losing bug above — but a flag computed BEFORE the
    /// poll cannot see what the poll itself cost, so a 25 s request starting at 899 s left the
    /// loop believing it was still inside the window and issuing a second one. The replacement
    /// code is then another 25 s late, on a screen whose whole complaint is waiting.
    #[test]
    fn a_poll_that_itself_crosses_the_deadline_is_the_last_one() {
        let mut w = ScriptedPin::new(vec![PinPoll::Unreachable]);
        w.poll_cost = Duration::from_secs(25); // `net::API`'s whole-transfer deadline
        assert_eq!(
            poll_for_token(&mut w, Duration::from_secs(20)),
            PollEnd::Expired
        );
        assert_eq!(
            w.polls, 1,
            "the request in flight answered, and nothing was asked after it"
        );

        // …and the same crossing poll still hands over a token it was carrying.
        let mut w = ScriptedPin::new(vec![PinPoll::Authorized("account-token".into())]);
        w.poll_cost = Duration::from_secs(25);
        assert_eq!(
            poll_for_token(&mut w, Duration::from_secs(20)),
            PollEnd::Token("account-token".into())
        );
    }

    /// A pin plex.tv has forgotten ends the wait AT ONCE. There is nothing left to poll for, and
    /// the code on screen is unscannable — every second spent on it is a second the user is being
    /// asked to try something that cannot work.
    #[test]
    fn a_code_plex_tv_no_longer_knows_ends_the_wait_at_once() {
        let mut w = ScriptedPin::new(vec![PinPoll::Pending, PinPoll::Pending, PinPoll::Gone]);
        assert_eq!(poll_for_token(&mut w, pin_window(900)), PollEnd::Expired);
        assert_eq!(w.polls, 3);
        assert!(
            w.clock < Duration::from_secs(30),
            "the 404 is an ending, not another two seconds of hope"
        );
    }

    /// **A transport failure is not an ending, and it is not a reason to hammer the network.**
    /// The old loop carried on too — but at a flat 2 s and with nothing in the log, so a poll that
    /// had stopped being answered and a poller that had stopped existing produced identical
    /// evidence on the one screen where they are the whole question.
    #[test]
    fn a_run_of_unanswered_polls_backs_off_and_keeps_going() {
        let mut w = ScriptedPin::new(vec![
            PinPoll::Unreachable,
            PinPoll::Unreachable,
            PinPoll::Unreachable,
            PinPoll::Unreachable,
            PinPoll::Authorized("account-token".into()),
        ]);
        assert_eq!(
            poll_for_token(&mut w, pin_window(900)),
            PollEnd::Token("account-token".into()),
            "the token still arrives — backing off never abandons the pin"
        );
        assert_eq!(
            w.waits,
            vec![
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(16),
            ],
            "2s while healthy, doubling per consecutive miss, capped so the backoff cannot \
             swallow what is left of the pin"
        );
    }

    /// **The deadline may not cancel a poll, and this is the review finding that mattered.**
    ///
    /// Reachable, and it is the reported symptom exactly: at 889 s a miss has pushed the backoff
    /// to 16 s; the user authorizes at 895 s and their phone says *Account linked*; the wait ends
    /// at 905 s. A loop that consults its clock BEFORE polling declares expiry there and throws
    /// away a token that was sitting in the very next response. So the pause is clamped to what is
    /// left of the code and the poll after it always happens — only plex.tv gets to say a pin is
    /// finished before we have asked once more.
    #[test]
    fn an_authorization_that_lands_during_the_last_backoff_is_still_collected() {
        let window = Duration::from_secs(20);
        let mut w = ScriptedPin::new(vec![
            PinPoll::Unreachable, // t=2.0  -> 2.3, backoff 4
            PinPoll::Unreachable, // t=6.3  -> 6.6, backoff 8
            PinPoll::Unreachable, // t=14.6 -> 14.9, backoff 16 — which would end at 30.9
            PinPoll::Authorized("account-token".into()),
        ]);
        assert_eq!(
            poll_for_token(&mut w, window),
            PollEnd::Token("account-token".into()),
            "the uncapped 16s backoff would have overrun the window and reported Expired \
             WITHOUT asking, dropping a token the user had already authorized"
        );
        assert_eq!(
            w.waits.last(),
            Some(&Duration::from_secs_f64(20.0 - 14.9)),
            "the last pause is exactly what was left of the code, not the full backoff"
        );
        assert_eq!(w.polls, 4, "and the poll at the deadline really happened");
    }

    /// The ceiling on automatic replacement, and the fact that reaching it is not a dead end: the
    /// flow lands on `Error`, which is the phase the sign-in screen has always drawn a retry on.
    #[test]
    fn automatic_replacement_is_bounded_and_ends_somewhere_with_a_way_out() {
        assert!(another_code_allowed(1), "the code a sign-in opens with");
        assert!(another_code_allowed(MAX_PIN_GENERATIONS - 1));
        assert!(
            !another_code_allowed(MAX_PIN_GENERATIONS),
            "a television left on this screen must stop polling plex.tv eventually"
        );
        assert!(
            MAX_PIN_GENERATIONS >= 2,
            "one code is the behaviour being fixed"
        );
    }

    /// One answer puts the cadence back. A phone tap is judged at 2 s, and a single bad moment
    /// half an hour ago must not still be costing sixteen seconds of it.
    #[test]
    fn an_answer_restores_the_two_second_cadence() {
        let mut w = ScriptedPin::new(vec![
            PinPoll::Unreachable,
            PinPoll::Pending,
            PinPoll::Authorized("t".into()),
        ]);
        assert_eq!(
            poll_for_token(&mut w, pin_window(900)),
            PollEnd::Token("t".into())
        );
        assert_eq!(
            w.waits,
            vec![
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(2)
            ]
        );
    }

    /// A superseded flow stops without polling and without a word: the successor owns the screen,
    /// and two workers narrating one sign-in is how a log stops being readable.
    #[test]
    fn a_superseded_flow_stops_silently_and_immediately() {
        let mut w = ScriptedPin::new(vec![PinPoll::Authorized("never-read".into())]);
        w.superseded_at = Some(0);
        assert_eq!(poll_for_token(&mut w, pin_window(900)), PollEnd::Superseded);
        assert_eq!(w.polls, 0);
    }

    /// The window is the pin's own lifetime, floored against a plex.tv that omits the field and
    /// ceilinged by this app's patience for one code.
    #[test]
    fn a_codes_lifetime_is_read_from_the_pin_and_clamped_at_both_ends() {
        assert_eq!(pin_window(900), Duration::from_secs(900));
        assert_eq!(
            pin_window(0),
            Duration::from_secs(60),
            "a missing expiresIn"
        );
        assert_eq!(pin_window(-7), Duration::from_secs(60), "or a nonsense one");
        assert_eq!(pin_window(86_400), Duration::from_secs(1800));
    }

    // ---- phase 6: LoginProgress / apply_progress ----
    //
    // `login_thread` used to be a writer of `Ctl` with the same authority as `start_login`/
    // `cancel`/`restart`, from a thread none of those three synchronize with except by convention.
    // These three tests pin the replacement: a stale observation is refused, a cancel cannot be
    // overtaken by a success that was already in flight when it happened, and the worker functions
    // themselves no longer contain the write at all.

    #[test]
    fn a_profile_delta_preserves_unrelated_newer_session_preferences() {
        let mut current = signed_in_as("u-adult");
        current.recent_searches.push(session::RecentSearches {
            user: "u-adult".into(),
            terms: vec!["newer preference".into()],
        });
        let next = signed_in_as("u-kid");
        merge_profile_delta(
            &mut current,
            ProfileDelta {
                server: next.server,
                sources: next.sources,
                user: next.user,
                cache: None,
            },
        );
        assert_eq!(current.user.uuid, "u-kid");
        assert_eq!(current.recent_searches.len(), 1);
        assert_eq!(current.recent_searches[0].terms, ["newer preference"]);
    }

    #[test]
    fn instance_profile_worker_completes_offline_policy_on_its_own_landing() {
        use crate::auth::owner::{SessionArrival, SessionOp, SessionWorkKey};
        use crate::app::adapters::session::SessionAdapter;
        use crate::ui::machine::RequestId;
        // No serial lock: both input credentials and both output transports are instance-local.
        let mut a = SessionAdapter::fixture();
        let mut b = SessionAdapter::fixture();
        for (adapter, epoch, uuid) in [(&mut a, 0x1_0000_0001, "u-kid"),
            (&mut b, 7, "not-cached")] {
            let stored = cached_session(None);
            let expected = SessionIdentity::of(&stored);
            let tile = UserTile { uuid: uuid.into(), title: "Synthetic profile".into(),
                ..Default::default() };
            adapter.launch(RequestId(1), SessionWorkKey { epoch, op: SessionOp::ProfileSwitch },
                true, |job| { job(); true }, move |output| {
                    profile_switch_worker_with_output(epoch, expected, stored, tile, None,
                        false, &output, |_, _, _| SwitchOutcome::Unreachable);
                }).unwrap();
        }
        let a = a.take_results();
        let b = b.take_results();
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert!(a[0].terminal && b[0].terminal);
        let SessionArrival::Data(a) = &a[0].outcome else { panic!("missing offline result") };
        let SessionArrival::Data(b) = &b[0].outcome else { panic!("missing failure result") };
        assert!(matches!(&**a, observation::Observation::ProfileSwitch(ProfileSwitchProgress {
            epoch: 0x1_0000_0001, outcome: ProfileSwitchOutcomeProgress::Ready { delta, .. }, ..
        }) if delta.user.uuid == "u-kid"));
        assert!(matches!(&**b, observation::Observation::ProfileSwitch(ProfileSwitchProgress {
            epoch: 7, outcome: ProfileSwitchOutcomeProgress::Failed { pin_denied: false, .. }, ..
        })));
    }

    /// Extract one `fn NAME(` … `}` body, verbatim, from this file's OWN source. A tiny lexer —
    /// tracking only whether it is inside a `"…"` string literal or a `//` line comment — rather
    /// than a bare brace count, because several of these functions log a `format!("…{x}…")` whose
    /// placeholder braces are balanced on their own and would otherwise silently agree with a real
    /// count by coincidence; skipping string contents removes the coincidence instead of relying on
    /// it.
    fn extract_fn_body<'a>(src: &'a str, name: &str) -> &'a str {
        let needle = format!("fn {name}(");
        let start = src
            .find(&needle)
            .unwrap_or_else(|| panic!("no `{needle}` in auth.rs — did it get renamed?"));
        let open = src[start..]
            .find('{')
            .map(|i| start + i)
            .unwrap_or_else(|| panic!("`{needle}` has no body"));
        let bytes = src.as_bytes();
        let (mut depth, mut i, mut in_string, mut in_comment) = (0i32, open, false, false);
        while i < bytes.len() {
            let c = bytes[i] as char;
            if in_comment {
                in_comment = c != '\n';
                i += 1;
                continue;
            }
            if in_string {
                if c == '\\' {
                    i += 2;
                    continue;
                }
                in_string = c != '"';
                i += 1;
                continue;
            }
            match c {
                '"' => in_string = true,
                '/' if bytes.get(i + 1) == Some(&b'/') => in_comment = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[open..=i];
                    }
                }
                _ => {}
            }
            i += 1;
        }
        panic!("unterminated body for `{needle}`");
    }

    /// Does `body`'s raw text call some function whose name starts with `prefix` — an
    /// `identifier(` whose identifier begins with `prefix`, found by a plain byte scan (this crate
    /// carries no regex dependency)?
    ///
    /// Used below to catch not one exact bypass spelling but the whole NAMING FAMILY it belongs
    /// to: this file's `Ctl`-writing setters — `set_error`, `set_error_if_live`,
    /// `set_pin_denied_for_test` — all share a `set_` prefix, so a FOURTH one sharing the
    /// convention (a `set_phase`, say) is refused here by the family it belongs to, on the commit
    /// that adds it, rather than only once someone remembers to type its exact name into a list.
    fn calls_a_function_prefixed(body: &str, prefix: &str) -> bool {
        let bytes = body.as_bytes();
        let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut i = 0;
        while let Some(rel) = body[i..].find(prefix) {
            let at = i + rel;
            // The match must START an identifier — the byte before it, if any, is not itself part
            // of one — so this cannot fire on `unset_error(` or on some longer name that merely
            // CONTAINS the prefix (`offset_ms(`, say, for a `set_` search).
            let starts_ident = at == 0 || !is_ident(bytes[at - 1]);
            if starts_ident {
                let mut j = at + prefix.len();
                while j < bytes.len() && is_ident(bytes[j]) {
                    j += 1;
                }
                let mut k = j;
                while k < bytes.len() && (bytes[k] as char).is_whitespace() {
                    k += 1;
                }
                if bytes.get(k) == Some(&b'(') {
                    return true;
                }
            }
            i = at + prefix.len();
        }
        false
    }

    /// **The invariant phase 6 exists to establish, pinned by reading the source.** Nothing else
    /// can see a regression here: a `with_ctl(|c| c.foo = …)` spliced back into `login_thread`
    /// compiles cleanly, passes every OTHER test in this file (none of them spin up a real worker
    /// thread against a real plex.tv — see the module doc), and only misbehaves on a device, under
    /// contention nobody happened to be watching for. Modelled on `diag::scrub`'s
    /// `no_log_call_site_interpolates_viewing_content`, which pins its own "the mechanism is that
    /// nobody writes it" claim the same way.
    ///
    /// **What this test actually checks, restated after a reviewer found the gap in the stronger
    /// sentence this comment used to make ("no `with_ctl` call at all", full stop).** That claim
    /// was true of the two DIRECT spellings this test greped for and false of a WRAPPED one: the
    /// reviewer added `set_error_if_live(epoch, "…")` to `login_thread` — the exact pre-phase-6
    /// call [`LoginProgress::Failed`]'s doc says is retired — and it reached `with_ctl` two hops
    /// down (`set_error_if_live` → `set_error` → `with_ctl`) while this test kept reporting green,
    /// because a two-hop wrapper call is neither `with_ctl(` nor `CTL.lock(` in the CALLER's own
    /// text. Three checks run per function now, not two: the original direct-spelling pair, PLUS
    /// [`calls_a_function_prefixed`] against `set_`, the naming family every `Ctl`-writing setter
    /// in this file already belongs to — which catches `set_error_if_live(` (and `set_error(` on
    /// its own) by the family, not by a name typed into this test.
    ///
    /// **What is still NOT proven, and this says so rather than overclaiming again:** a wrapper
    /// that reached `Ctl` under a name outside the `set_` convention would still slip past a
    /// textual scan — closing that fully needs a real call-graph walk (or moving `Ctl` behind an
    /// interface a worker's module cannot name at all), not a longer prefix list. This is a
    /// materially stronger gate than the one it replaces, scoped to say exactly that.
    ///
    /// This original QR-specific guard stays beside the broader R2A boundary below because it also
    /// scans the helper chain called by login discovery. Profile, roster and endpoint workers are
    /// covered by [`all_auth_worker_bodies_are_observation_only`].
    #[test]
    fn login_worker_functions_never_touch_ctl_directly() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
        )
        .expect("auth.rs must be readable from its own test");
        for name in [
            "login_worker_with_output",
            "mint_pin",
            "finish_sign_in",
            "discover_and_store",
            "rediscovery_worker_with_output",
        ] {
            let body = extract_fn_body(&src, name);
            assert!(
                !body.contains("with_ctl("),
                "`{name}` calls `with_ctl` — a phase-6 worker must observe and push a \
                 `LoginProgress` instead of touching `Ctl` directly (read OR write):\n{body}"
            );
            assert!(
                !body.contains("CTL.lock("),
                "`{name}` locks `CTL` directly, bypassing `with_ctl` but not the rule it exists \
                 to enforce"
            );
            for wrapper in ["set_error(", "set_error_if_live("] {
                assert!(
                    !body.contains(wrapper),
                    "`{name}` calls `{wrapper}` — a `Ctl`-writing wrapper this test used to be \
                     blind to, because it only greped for `with_ctl(`/`CTL.lock(` directly and \
                     this reaches `with_ctl` two calls down. A worker must push a `LoginProgress` \
                     and let `apply_progress` make the write on the main thread instead."
                );
            }
            assert!(
                !calls_a_function_prefixed(body, "set_"),
                "`{name}` calls a `set_`-prefixed function — this file's naming convention for \
                 every `Ctl`-writing setter it has (`set_error`, `set_error_if_live`, \
                 `set_pin_denied_for_test`). A NEW setter sharing that prefix is refused here by \
                 the family it belongs to; see the doc above this test for the mutation that made \
                 the narrower direct-spelling check insufficient:\n{body}"
            );
        }
    }

    /// Textual worker boundary: the actual instance-worker entry points AND their shared policy
    /// bodies. Scanning a thin forwarding wrapper alone cannot constrain its callee. This is
    /// an explicit list, not automatic call-graph coverage; new worker helpers must be added.
    /// Workers may perform network/probe/PBKDF2 work and publish immutable observations;
    /// application mutation belongs to the main-thread owner/resource acceptance path.
    #[test]
    fn all_auth_worker_bodies_are_observation_only() {
        let src = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
        )
        .expect("auth.rs must be readable from its own test");
        let forbidden = [
            "with_ctl(",
            "CTL.lock(",
            "with_live_epoch(",
            "session::load(",
            "session::peek(",
            "session::save(",
            "session::update(",
            "session::clear(",
            "session::set_current(",
            "activate_candidate(",
            "install_roster(",
            "publish_settled_probe(",
            "publish_settled_probes(",
            "crate::plex::register_origin(",
            "crate::plex::revoke_",
            "crate::plex::finish_profile_switch(",
            "crate::plex::publish_probe_result(",
            "crate::plex::describe_server(",
        ];
        for name in [
            "discover_and_store",
            "login_worker_with_output",
            "rediscovery_worker_with_output",
            "home_roster_worker_with_output",
            "server_roster_worker_with_output",
            "profile_switch_worker_with_output",
            "profile_switch_worker_with_io",
            "run_session_work",
            "endpoint_work_fact",
            "endpoint_worker_with_io",
            "probe_endpoint_work",
            "offline_switch_outcome",
            "candidate_activation",
            "probe_profile_resource_live",
        ] {
            let body = extract_fn_body(&src, name);
            for call in forbidden {
                assert!(
                    !body.contains(call),
                    "auth worker `{name}` crosses the observation boundary through `{call}`:\n{body}"
                );
            }
        }
    }
}
