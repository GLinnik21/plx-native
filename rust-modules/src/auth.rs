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
use crate::plex::account::{AccountClient, CallEvidence, HomeUser, PinPoll, Resource, SwitchOutcome};
use crate::telemetry::incident::{DiscoveryClass, IncidentContext, IncidentKind};
use crate::plex::probe::{self, Candidate, Outcome, ProbePlan};
use crate::plex::session::{self, ProfileCreds, ServerRef, Session, SourceRef, UserRef};
use crate::plex::{CredentialPolicy, Origin, ServerId};
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
    resources: impl FnOnce(&AccountClient) -> Result<Vec<Resource>, CallEvidence>,
    probe: impl FnOnce(&Resource, &[i64]) -> (Option<SourceRef>, SettledProbe)) {
    let (fresh, probe) = probe_endpoint_work(ServerId::from_raw(lifecycle.sid), &machine_id, &session,
        resources, probe, &|| output.live());
    output.terminal(endpoint_work_fact(epoch, expected, lifecycle, machine_id, fresh, probe));
}

/// Endpoint transport projection shared by the real worker and injected network-result tests.
/// Admission, interest and native lifecycle validation remain in the adapter/owner protocol.
pub(crate) fn endpoint_work_fact(epoch: u64, expected: owner::Identity,
    lifecycle: owner::ServerLifecycle, machine_id: String, fresh: Option<SourceRef>,
    probe: Option<SettledProbe>) -> AuthProgress {
    AuthProgress::Endpoint(EndpointProgress { epoch,
        expected: SessionIdentity { client_id: expected.client_id, account_token: expected.account_token,
            profile_uuid: expected.profile_uuid },
        id: ServerId::from_raw(lifecycle.sid), machine_id, lifecycle: None, fresh, probe })
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
            extensions: Default::default(),
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
    /// The advertised address BEHIND `origin` — a dotted quad or v6 literal, never a
    /// `plex.direct` hostname. `plex::IpVersion::of_host` can classify this even when `origin`'s
    /// own host is a certificate NAME it cannot parse as an address (#95 step 8 / R3(a)).
    pub address: String,
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
/// registry) and *Sign out* (which revokes). One exception, and it is not a PIN bypass: a picker
/// with NO tiles whose roster request has ended (`owner`'s `roster_dead_end`) offered nobody to
/// hand the remote to, so BACK resumes the stored session from it rather than strand the person
/// on *Sign out* (#132).
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

fn may_reuse_seated_profile(sess: &Session, tile: &UserTile, pin: Option<&str>) -> bool {
    pin.is_none() && !tile.protected && !sess.user.uuid.is_empty()
        && tile.uuid == sess.user.uuid && !sess.pms_token().is_empty()
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
        extensions: Default::default(),
    });
}

/// A roster answer, graded for the owner and logged with what the request observed.
///
/// - `Some(tiles)` — a roster.
/// - `Some(vec![])` — plex.tv's VERDICT that this identity has no roster to offer: it answered
///   with nobody, or refused the identity (401/403 — a managed Plex Home profile's token, #132).
///   The owner never commits it over a cached roster; with none cached, the picker says
///   switching isn't available from this profile.
/// - `None` — no verdict: no answer, a 5xx, a body that would not read. The owner keeps whatever
///   is cached; with none, the picker says to check the connection.
///
/// Both failure lines carry the status (`describe_evidence`): a failure that leaves no line is
/// how #132's first report came to read "nothing in the log".
fn grade_roster(answer: Result<Vec<HomeUser>, CallEvidence>) -> Option<Vec<UserTile>> {
    match answer {
        Ok(users) if !users.is_empty() => {
            let users: Vec<UserTile> = users.iter().map(UserTile::of).collect();
            log(&format!("auth: roster refreshed n={}", users.len()));
            Some(users)
        }
        Ok(_) => {
            log("auth: roster answered with no profiles — keeping cached roster");
            Some(Vec::new())
        }
        Err(evidence) => {
            // "roster refresh failed — keeping cached roster" is matched verbatim by
            // `tests/run.py`'s offline check: the evidence goes AFTER it.
            let refused = crate::plex::account::refused_identity(&evidence).is_some();
            log(&format!("auth: roster refresh {} — keeping cached roster ({}){}",
                if refused { "refused" } else { "failed" },
                crate::plex::account::describe_evidence(&evidence),
                if refused { ": this account cannot list Home profiles" } else { "" }));
            refused.then(Vec::new)
        }
    }
}

fn home_roster_worker_with_output(epoch: u64, expected: SessionIdentity, cid: String,
    token: String, output: &dyn owner::ObservationSink) {
    if !output.live() { return; }
    let ac = AccountClient::new(&cid, Some(&token));
    let users = grade_roster(ac.home_users());
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
    /// The grant EVIDENCE beside the credit — plex.tv's `home` and `ownerId`, carried so the
    /// registry slot this activation publishes can answer "is this our household's server?"
    /// rather than only "does this account own it?". Defaulted on a legacy observation for the
    /// same reason, and with the same self-correction, as `SourceRef::owner_id` documents.
    #[serde(default)]
    home: bool,
    #[serde(default)]
    owner_id: i64,
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
    /// Nothing was found to REGISTER, but at least the probes themselves ran (R2/A5) — `settled`
    /// carries every one, empty only when there was truly nothing to probe (no server named at
    /// all). The owner commits these as registry-only [`RegistryPlan::Probe`]s: the roster itself
    /// is unchanged, so there is nothing to write to disk.
    NoReachable { settled: Vec<SettledProbe> },
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
    probe: Option<SettledProbe>,
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
pub(crate) fn execute_session_registry(plan: &owner::RegistryPlan, client_id: &str) -> bool {
    match plan {
        owner::RegistryPlan::DevInstall { primary, extras, client_id } => {
            install_captured_registry(&primary.origin(), &primary.address, &primary.token,
                primary.tier, primary.resolve_pin().as_ref(), extras, Some(client_id));
        }
        owner::RegistryPlan::Primary { server, token } => {
            // #95 step 8 / A2: carry the tier + address-derived IP into the same registration
            // write, rather than restoring them in a later separate call the boot picker's avatar
            // client used to skip.
            let connection = crate::plex::ConnectionFacts::new(
                server.tier,
                crate::plex::IpVersion::of_host(&server.address),
            );
            let id = register_observed_origin("", &server.origin(), token, server.resolve_pin().as_ref(), connection, client_id);
            crate::plex::set_current(id);
        }
        owner::RegistryPlan::Activate { source, ipv6 } => {
            let Some(origin) = source.origin() else { return false };
            let Some(location) = source.tier else { return false };
            apply_candidate_activation(CandidateActivation {
                machine_id: source.machine_id.clone(), token: source.token.clone(),
                name: source.name.clone(), credit: source.shared_by.clone(), owned: source.owned,
                home: source.home, owner_id: source.owner_id,
                origin, address: source.address.clone(),
                location, ipv6: *ipv6,
            }, client_id);
        }
        owner::RegistryPlan::Install { sources, primary, replace } => {
            if *replace { crate::plex::revoke_for_profile_switch(); }
            let installed = install_roster(sources, *primary, client_id);
            if *replace { crate::plex::finish_profile_switch(&installed); }
        }
        owner::RegistryPlan::Endpoint { expected, source } => {
            let Some(origin) = source.origin() else { return false };
            let connection = crate::plex::ConnectionFacts::new(
                source.tier,
                crate::plex::IpVersion::of_host(&source.address),
            );
            let id = register_observed_origin(&source.machine_id, &origin, &source.token,
                source.resolve_pin().as_ref(), connection, client_id);
            if id.raw() != expected.sid { return false; }
            crate::plex::describe_server(id, &source.name, &source.shared_by, grant_of(source));
            crate::plex::publish_probe_result(id, Outcome::Reachable);
        }
        owner::RegistryPlan::Probe(probe) => publish_settled_probe(probe),
        owner::RegistryPlan::Revoke => crate::plex::revoke_all(),
    }
    true
}

/// Shared resource installer. Dev boot supplies its captured device identity so registration
/// cannot mint/read a session file; legacy boot callers can use the already-loaded session cache.
///
/// `address` is the advertised address BEHIND `origin` (#95 step 8 / R3(a)): `origin.host()` is
/// usually a `plex.direct` certificate NAME that `IpVersion::of_host` cannot parse, so deriving
/// the IP family from it silently produced `None` on every real boot. The stored/advertised
/// address is the one value that is actually a literal.
pub(crate) fn install_captured_registry(origin: &Origin, address: &str, token: &str,
    tier: Option<probe::Location>, pin: Option<&crate::plex::ResolvePin>, extras: &[SourceRef],
    client_id: Option<&str>) {
    // Applies `connection` atomically, inside the same registration write, on whichever branch
    // runs (#95 step 8) — the primary and every extra used to register first and set the
    // connection facts in a SEPARATE call afterward, which is exactly the gap `ConnectionFacts`
    // exists to close.
    let register = |machine: &str, origin: &Origin, token: &str,
        pin: Option<&crate::plex::ResolvePin>, connection: crate::plex::ConnectionFacts| {
        if let Some(cid) = client_id {
            crate::plex::register_captured_origin_with_connection(machine, origin, token, pin, cid,
                connection)
        } else {
            register_observed_origin(machine, origin, token, pin, connection, &session::peek().client_id)
        }
    };
    let primary_connection =
        crate::plex::ConnectionFacts::new(tier, crate::plex::IpVersion::of_host(address));
    let id = register("", origin, token, pin, primary_connection);
    crate::plex::set_current(id);
    for source in extras {
        let Some(origin) = source.origin() else { continue };
        if source.token.is_empty() { continue; }
        let connection = crate::plex::ConnectionFacts::new(
            source.tier,
            crate::plex::IpVersion::of_host(&source.address),
        );
        let id = register(&source.machine_id, &origin, &source.token,
            source.resolve_pin().as_ref(), connection);
        crate::plex::describe_server(id, &source.name, &source.shared_by, grant_of(source));
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
    ///
    /// `incident` is the same failure as closed evidence for the onboarding report — a kind, the
    /// class of the last network call and its counters, never `message`'s text.
    Failed { epoch: u64, message: String, incident: crate::telemetry::incident::IncidentContext },
    /// plex.tv stopped answering the polls of the code on screen (`Some`, once, when the run of
    /// unanswered polls reaches [`LINK_TROUBLE_AFTER`]) or answered again (`None`). Non-terminal:
    /// the wait goes on, and the owner raises a `LinkStalled` incident from the evidence.
    LinkTrouble { epoch: u64, trouble: Option<crate::telemetry::incident::IncidentContext> },
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

/// Register with the identity captured by the owner, including before a fresh login is durable.
fn register_observed_origin(
    machine_id: &str,
    origin: &Origin,
    token: &str,
    pin: Option<&crate::plex::ResolvePin>,
    connection: crate::plex::ConnectionFacts,
    client_id: &str,
) -> ServerId {
    crate::plex::register_captured_origin_with_connection(
        machine_id, origin, token, pin, client_id, connection)
}

fn apply_candidate_activation(candidate: CandidateActivation, client_id: &str) {
    let pin = crate::plex::ResolvePin::for_origin(&candidate.origin, &candidate.address);
    let connection = crate::plex::ConnectionFacts::new(
        Some(candidate.location),
        Some(if candidate.ipv6 {
            crate::plex::IpVersion::V6
        } else {
            crate::plex::IpVersion::V4
        }),
    );
    let id = register_observed_origin(
        &candidate.machine_id,
        &candidate.origin,
        &candidate.token,
        pin.as_ref(),
        connection,
        client_id,
    );
    if crate::plex::client_for(id).is_some() {
        crate::plex::publish_probe_result(id, Outcome::Reachable);
    }
    crate::plex::describe_server(id, &candidate.name, &candidate.credit, crate::plex::GrantEvidence {
        owned: candidate.owned, home: candidate.home, owner_id: candidate.owner_id,
    });
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

/// A failure's evidence for a test that is not about the incident offer.
#[cfg(test)]
pub(crate) fn synthetic_incident() -> IncidentContext {
    IncidentContext::new(IncidentKind::PinCreate, None)
}

/// End a sign-in on the error read-out. `incident` is the same failure as closed evidence — the
/// caption is for the person, the context is what an onboarding report may carry.
fn output_failed(output: &dyn owner::ObservationSink, epoch: u64, message: &str,
    incident: IncidentContext) {
    output.terminal(LoginProgress::Failed { epoch, message: message.into(), incident }.into());
}

/// The caption and the incident for a discovery that found nothing usable. One table for the
/// sign-in and the rediscovery paths, so the two cannot word — or report — the same verdict
/// differently: the rediscovery worker is reached only through *Try again* after a discovery
/// failure (`retry_kind`), and reporting that failure under a kind of its own made the retry ask
/// the question the person had just answered. `None` for the outcomes that are not failures.
///
/// **plex.tv refusing the account token is not silence.** `/resources` answering 401 or 403 is an
/// [`IncidentKind::Authorization`] failure, with a caption that does not send the person to a
/// network that is working.
fn discovery_failure(d: &Discovery) -> Option<(&'static str, IncidentContext)> {
    if let Discovery::Silent(Some(last)) = d {
        let status = match last {
            Ok(status) => Some(*status),
            Err(failure) => failure.status,
        };
        if matches!(status, Some(401 | 403)) {
            return Some((
                "Plex didn't accept this sign-in. Try again.",
                IncidentContext::new(IncidentKind::Authorization, Some(*last)),
            ));
        }
    }
    let (message, class, last) = match d {
        Discovery::Ok { .. } | Discovery::Cancelled => return None,
        Discovery::NoServers => ("This Plex account has no server yet.", DiscoveryClass::NoServers, None),
        Discovery::Refused => (
            "Your Plex server refused the connection — check its network access settings.",
            DiscoveryClass::Refused,
            None,
        ),
        Discovery::Silent(last) => (
            "Couldn't reach any Plex server — check the connection.",
            DiscoveryClass::Silent,
            *last,
        ),
        Discovery::InsecureOnly => (DISCOVERY_INSECURE_ONLY_MESSAGE, DiscoveryClass::InsecureOnly, None),
    };
    Some((message, IncidentContext::new(IncidentKind::Discovery(class), last)))
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
            epoch,
            generation,
        };
        match poll_for_token(&mut watch, pin_window(code.expires_in)) {
            PollEnd::Token(t) => break t,
            PollEnd::Superseded => return, // cancelled — whoever superseded us owns the screen
            PollEnd::Expired(_) if another_code_allowed(generation) => {
                log("auth: the sign-in code ran out — minting a fresh one");
            }
            PollEnd::Expired(tail) => {
                log("auth: out of automatic sign-in codes — asking the user to start again");
                let incident = expired_incident(&tail, generation);
                return output_failed(output, epoch, "Sign-in timed out — try again.", incident);
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
    let discovery = discover_and_store(&ac, &cid, epoch, output);
    if let Some((message, incident)) = discovery_failure(&discovery) {
        return output_failed(output, epoch, message, incident);
    }
    let Discovery::Ok { server, sources } = discovery else { return };
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
    let created = crate::dev::scenarios::signin_trouble_create().unwrap_or_else(|| ac.create_pin());
    let pin = match created {
        Ok(p) if p.id != 0 && !p.code.is_empty() => p,
        failed => {
            // A 2xx that decoded into a pin with no id or code is still an answer: classed by
            // its status, which is the 2xx it was.
            let last = match failed { Err(evidence) => evidence, Ok(_) => Ok(200) };
            // Says what the internet is FOR here — signing in — because the one time this screen
            // appears with the link deliberately down is the first boot of a set that has never
            // signed in. Drawn as the reason under "Couldn't sign in" (`screens/login.rs`).
            output_failed(output,
                epoch,
                "Couldn\u{2019}t reach Plex. Check your internet connection. An internet connection is \
                 needed to sign in.",
                IncidentContext::new(IncidentKind::PinCreate, Some(last))
                    .with_link_state(0, None, generation),
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
    //
    // A failed fetch still signs in (as a single user) and persists an empty roster, which the
    // session reads as "unknown" (`Session::account`) — but it now says WHY in the log, in the
    // same grading the Change-profile refresh uses, rather than a bare `n=0`.
    let users = match ac.home_users() {
        Ok(users) => users.iter().map(UserTile::of).collect(),
        Err(evidence) => {
            log(&format!("auth: home users {} ({})",
                if crate::plex::account::refused_identity(&evidence).is_some() { "refused" } else { "unavailable" },
                crate::plex::account::describe_evidence(&evidence)));
            Vec::<UserTile>::new()
        }
    };
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
    let discovery = discover_and_store(&ac, &cid, epoch, output);
    if let Some((message, incident)) = discovery_failure(&discovery) {
        // The same caption AND the same incident as sign-in: this is the retry of that failure.
        return output_failed(output, epoch, message, incident);
    }
    if let Discovery::Ok { server, sources } = discovery {
        finish_sign_in(&ac, epoch, server, sources, output);
    }
}

/// What the polls of a code that ran out last observed — the evidence its expiry report carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct PollTail {
    /// The latest poll's evidence: `Ok(200)` for an answer that was still pending, the failed
    /// call's own evidence for an unanswered one. `None` when no poll produced any.
    last: Option<CallEvidence>,
    /// Consecutive unanswered polls at the end.
    unanswered: u32,
    /// How long that run of misses lasted, on the code's clock. `None` without one.
    failing_for: Option<Duration>,
}

impl PollTail {
    /// The tail at `elapsed` on the code's clock, with a run of `misses` that began at `since`.
    fn at(elapsed: Duration, misses: u32, since: Duration, last: Option<CallEvidence>) -> Self {
        Self { last, unanswered: misses, failing_for: (misses > 0).then(|| elapsed.saturating_sub(since)) }
    }
}

/// The report for the last code of a flow running out, from how its polls last went.
fn expired_incident(tail: &PollTail, generation: u32) -> IncidentContext {
    IncidentContext::new(IncidentKind::PinExpired, tail.last)
        .with_link_state(tail.unanswered, tail.failing_for, generation)
}

/// How one publication of a QR code ended.
#[derive(Debug, PartialEq, Eq)]
enum PollEnd {
    /// The user authorized on their phone and plex.tv handed over the account token.
    Token(String),
    /// This code is finished — plex.tv says so, or its own lifetime ran out. There is nothing
    /// left to wait for and the caller must mint another. Carries how its polls last went.
    Expired(PollTail),
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
    /// plex.tv has stopped answering (`Some`, once per run of misses, when it reaches
    /// [`LINK_TROUBLE_AFTER`]) or answers again (`None`, only after a `Some`).
    fn link_trouble(&mut self, stall: Option<Stall>);
}

/// Consecutive unanswered polls before the wait is called stalled — 0.6.6's rule, and the
/// shortest run that is not one bad moment: a single miss already backs off and says so in the
/// log, a second in a row is a link that has gone.
const LINK_TROUBLE_AFTER: u32 = 2;

/// Evidence of a stalled wait, handed to [`PinWatch::link_trouble`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stall {
    /// Consecutive unanswered polls.
    unanswered: u32,
    /// Since the first of them, on the code's clock.
    failing_for: Duration,
    /// What the latest of them observed.
    last: CallEvidence,
}

/// The real one: a live pin, the wall clock, and the flow's epoch.
struct LivePin<'a> {
    ac: &'a AccountClient,
    id: i64,
    output: &'a dyn owner::ObservationSink,
    started: Instant,
    epoch: u64,
    /// Which code of the flow this is, 1-based — the report's code generation.
    generation: u32,
}

impl PinWatch for LivePin<'_> {
    fn poll(&mut self) -> PinPoll {
        crate::dev::scenarios::signin_trouble_poll().unwrap_or_else(|| self.ac.poll_pin(self.id))
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
    fn link_trouble(&mut self, stall: Option<Stall>) {
        let trouble = stall.map(|s| {
            IncidentContext::new(IncidentKind::LinkStalled, Some(s.last))
                .with_link_state(s.unanswered, Some(s.failing_for), self.generation)
        });
        self.output.progress(LoginProgress::LinkTrouble { epoch: self.epoch, trouble }.into());
    }
}

/// Poll `/pins/{id}` until the user authorizes, the code dies, or the flow is superseded.
///
/// **A transport failure is not an ending.** It never was — the loop this replaced also carried on
/// — but it was also never SAID, in the log or in the cadence, so "the app stopped polling" and
/// "plex.tv stopped answering" produced identical evidence. Now a miss backs off, says so once,
/// and says when the answers come back; and the one answer that really is an ending, a pin plex.tv
/// no longer knows, ends the wait immediately instead of being retried for the rest of the window.
///
/// A run of [`LINK_TROUBLE_AFTER`] misses is also REPORTED ([`PinWatch::link_trouble`]), once per
/// run, and the first answer after it clears the report — the screen says the link is down, and the
/// onboarding report may offer the stall.
fn poll_for_token(w: &mut impl PinWatch, window: Duration) -> PollEnd {
    let mut misses: u32 = 0;
    // When the current run of misses began, on the code's clock.
    let mut failing_since = Duration::ZERO;
    // What the latest poll observed — the evidence an expiry is reported with.
    let mut last_seen: Option<CallEvidence> = None;
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
                // `poll_response` decodes a pending pin only out of a 200.
                last_seen = Some(Ok(200));
                if misses > 0 {
                    log("auth: plex.tv is answering again — still waiting for authorization");
                }
                if misses >= LINK_TROUBLE_AFTER {
                    w.link_trouble(None);
                }
                misses = 0;
            }
            PinPoll::Gone => {
                log("auth: plex.tv no longer knows this sign-in code — expired or already used");
                return PollEnd::Expired(PollTail::at(w.elapsed(), misses, failing_since, last_seen));
            }
            PinPoll::Unreachable(last) => {
                last_seen = Some(last);
                if misses == 0 {
                    failing_since = w.elapsed();
                }
                misses = misses.saturating_add(1);
                if misses == LINK_TROUBLE_AFTER {
                    let failing_for = w.elapsed().saturating_sub(failing_since);
                    w.link_trouble(Some(Stall { unanswered: misses, failing_for, last }));
                }
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
            return PollEnd::Expired(PollTail::at(w.elapsed(), misses, failing_since, last_seen));
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

/// How far one server got. Only [`Reach::At`] is a server we can use; the others are the
/// distinction `probe.rs`'s module doc refuses to let a caller collapse, because they send the
/// user to different places.
///
/// **Precedence, best first: `At` > `InsecureOnly` > `Refused` > `No`.** A verified identity —
/// even one this build cannot put a credential on — beats a 401 from a different, parallel or
/// proxied candidate; silence is the weakest signal of all.
enum Reach {
    /// This address answered `/identity` **as the server we asked for**, over a transport that
    /// can carry a credential.
    ///
    /// Two values, and the split is the point: the [`Origin`] is **what was actually dialled**, and
    /// so the only thing the roster may record as this server's address; the [`Candidate`] is kept
    /// beside it for the DIAGNOSTIC fields (`address`, `port`) that the log and the Sources panel
    /// say. Deriving the record from `Candidate::url` while the dial came from `Candidate::address`
    /// left exactly one gap — a plex.tv `uri` whose port disagrees with `port` would be verified at
    /// one and written down as the other — and this pairing closes it by construction.
    At(Candidate, Origin),
    /// This address answered `/identity` **as the server we asked for**, but
    /// [`Candidate::credential_eligible`] is `false` — a plaintext answer in a store build.
    /// **Alive, and provably the right server, but nothing may register it or put a token on it.**
    /// It is not [`Reach::At`], because the whole point of this app's credential guard is that a
    /// verified-but-ineligible candidate must not read as success (`probe.rs`'s module doc); it is
    /// not [`Reach::No`] either, because "the server did not answer" and "the server answered and
    /// this build cannot use it" are two different facts to hand the user.
    InsecureOnly(Candidate),
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
    /// Servers exist; none of them answered (or plex.tv itself did not). Carries the failed
    /// `/api/v2/resources` call's evidence when that is what went silent; `None` when the servers
    /// themselves did.
    Silent(Option<CallEvidence>),
    /// At least one answered **401**, and none was reachable. Something in front of that server
    /// refuses unauthenticated requests — an auth proxy, or `allowedNetworks` excluding this
    /// subnet. It is not a network fault and not a dead server, so it must not be worded as one.
    Refused,
    /// At least one server verified — the identity matched — but only [`Reach::InsecureOnly`]:
    /// over a transport this build can never put a credential on. Takes precedence over
    /// [`Self::Refused`] (plan §4): a verified plaintext answer is a more useful fact than a
    /// parallel/proxy 401, and points at a fixable cause (HTTPS to the server) rather than a
    /// credential one.
    InsecureOnly,
}

/// Copy for [`Discovery::InsecureOnly`], shared by sign-in and rediscovery so the two paths
/// cannot say two different things about the same verdict (plan §4).
///
/// Owner-approved wording; keep byte-identical.
const DISCOVERY_INSECURE_ONLY_MESSAGE: &str =
    "Found your Plex server, but couldn't connect to it securely (HTTPS). Check that your server \
     allows secure connections, then try again.";

/// The probe path. **Unauthenticated on purpose** — `/identity` answers 200 to anybody, which
/// makes it useless as a token test and perfect as a reachability + identity one.
///
/// The token is deliberately NOT sent. A probe can land on a *different machine* (that is rule 1
/// of `probe.rs`, and the reason identity is verified at all), and a request that carried the
/// per-(user, server) token would hand that stranger a live credential before we had any reason to
/// believe who they are.
///
/// **So this identity stage does not, and cannot, prove the token works.** A per-(user, server)
/// grant revoked between the `/api/v2/resources` fetch and now still probes as
/// [`Outcome::Reachable`] here — the server really is reachable; it is the credential that is dead.
/// Online admission follows this proof with authenticated `GET /library/sections` before selecting
/// or activating the endpoint, so that request classifies the 401 without exposing the token to an
/// unverified machine. The
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
///
/// `pin`, when [`race_batch`] built one for this candidate, is forwarded to
/// [`crate::http::request_probe`] exactly as `apply_candidate_activation` forwards one to
/// `register_origin` — the same [`crate::plex::ResolvePin`], used one step earlier: at the DIAL
/// that decides the winner, not only at the registration of one already decided.
fn get_identity(
    origin: &Origin,
    pin: Option<&crate::plex::ResolvePin>,
    budget: Duration,
) -> (i32, Vec<u8>) {
    match crate::http::request_probe(
        origin,
        IDENTITY,
        crate::http::Method::Get,
        &[crate::http::ACCEPT_JSON],
        64 * 1024,
        budget.as_secs().max(1) as i32,
        pin,
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
// Authenticated admission starts only after identity probing has produced a candidate. Endpoint
// fallbacks share this ceiling, while each sections request keeps its local/remote attempt cap.
const ADMISSION_BUDGET: Duration = Duration::from_secs(20);

type ProbeDial = Arc<
    dyn Fn(&Origin, Option<&crate::plex::ResolvePin>, Duration) -> (i32, Vec<u8>) + Send + Sync + 'static,
>;
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
    /// The best-scored verified-but-ineligible answer in this batch — set instead of `first`/
    /// `best` when [`Candidate::credential_eligible`] is `false`. Never activated; kept as
    /// evidence for [`Reach::InsecureOnly`] when nothing eligible verifies.
    insecure: Option<Winner>,
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
            if !c.credential_eligible {
                // Verified — this really is the server we asked for — but over a transport this
                // build can never put a credential on (a plaintext twin in a store build). It
                // must not become `first`/`best`: those are what `activate` acts on, and
                // activating this origin would re-point the live server to something every
                // credentialed request then fails on (device, 2026-09-06: `security: refused
                // plaintext PMS credentials`, a hub fetch and the picker's first avatar lost in
                // the gap while the LAN plaintext twin answered before the TLS winner did). It
                // is still evidence the server is alive here, so it is kept — best-scored — as
                // `insecure`, and `probe_server_racing` decides what that means once nothing
                // eligible has verified.
                log(&format!(
                    "auth: '{}' answered only over plaintext at {}",
                    plan.name,
                    winner.origin.log_form()
                ));
                if result.insecure.as_ref().is_none_or(|old| better(&winner, old)) {
                    result.insecure = Some(winner);
                }
                return;
            }
            if result.first.is_none() {
                // "First usable immediately" — every candidate that reaches this branch is
                // already `credential_eligible`, so nothing here needs to ask again whether the
                // build can put a token on it.
                activate(plan, &winner.candidate, &winner.origin);
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
        // `classify` never produces this — it is the whole-server AGGREGATE verdict this
        // function's own caller derives from `result.insecure`, not a per-candidate answer.
        Outcome::InsecureOnly => {}
    }
}

/// Race one phase of a server's candidates. The spawner is injected because refusal is a result
/// the coordinator must settle, not an exceptional path a unit test can reach through real OS
/// exhaustion. Only a successful spawn creates a pending entry. Each entry owns an absolute
/// deadline; expiring one local worker never settles a still-live remote worker. Direct and relay
/// are separate phases, so each phase receives the full per-candidate budget appropriate to it.
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
        let budget = probe_deadline(c, policy);
        let deadline = started + budget;
        let state = Arc::new(AtomicU8::new(PROBE_PENDING));
        let tx = tx.clone();
        let dial = Arc::clone(&dial);
        let worker_state = Arc::clone(&state);
        let machine_id = plan.machine_id.clone();
        // Built here, at the dial that decides a winner — not only at `apply_candidate_activation`,
        // which pins the same way after one already has. `None` for a plaintext candidate (a pin
        // belongs to a TLS name only) or an unmatched/undecodable label; the request then resolves
        // through DNS exactly as before.
        let pin = crate::plex::ResolvePin::for_origin(&origin, &c.address);
        let job = Box::new(move || {
            let (status, body) = dial(&origin, pin.as_ref(), budget);
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
    // Relay is the reachability fallback whenever nothing eligible verified directly — `first` is
    // only ever set for a `credential_eligible` winner now (`settle_probe_message`), so a
    // plaintext-only direct answer still starves nothing here — including when a proxy on one
    // direct origin answered 401. Preserve that refusal, and the direct-only insecure answer, only
    // as the final reason if relay also produces no eligible winner; a verified identity always
    // beats a parallel/proxy 401.
    if batch.first.is_none() && !relay.is_empty() {
        let direct_refused = batch.refused;
        let direct_insecure = batch.insecure.take();
        batch = race_batch(plan, &relay, dial, spawn, policy, activate);
        batch.refused |= direct_refused;
        if batch.insecure.is_none() {
            batch.insecure = direct_insecure;
        }
    }

    let Some(best) = batch.best else {
        return if let Some(insecure) = batch.insecure {
            Reach::InsecureOnly(insecure.candidate)
        } else if batch.refused {
            Reach::Refused
        } else {
            Reach::No
        };
    };
    let first = batch
        .first
        .as_ref()
        .expect("a best winner is also a first winner");
    // The final re-point to the best score. Both `first` and `best` are `credential_eligible` by
    // construction (`settle_probe_message` never lets an ineligible winner become either), so the
    // activation question this used to re-ask here is already settled.
    if first.index != best.index {
        activate(plan, &best.candidate, &best.origin);
    }
    Reach::At(best.candidate, best.origin)
}


fn candidate_activation(
    plan: &ProbePlan,
    c: &Candidate,
    origin: &Origin,
    credit: &str,
    evidence: crate::plex::GrantEvidence,
) -> CandidateActivation {
    CandidateActivation {
        machine_id: plan.machine_id.clone(),
        token: plan.token.clone(),
        name: plan.name.clone(),
        credit: credit.to_owned(),
        // `owned` still comes from the PLAN — it is what the plan was built to dial under — while
        // `home`/`ownerId` come from the paired wire row, exactly as the credit does. A plan
        // deliberately carries only what is needed to DIAL, and the pairing by `clientIdentifier`
        // is the one identity that cannot drift.
        owned: plan.owned,
        home: evidence.home,
        owner_id: evidence.owner_id,
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
    /// The candidate address that answered, kept ONLY so a background application of this
    /// verdict (`publish_settled_probe`) can derive the connection's IP generation without ever
    /// reading `origin.host()` (R3/A3) — a `plex.direct` NAME there, not the dotted quad. `None`
    /// whenever `tier` is: nothing verified, so there is nothing to derive from.
    #[serde(default)]
    address: Option<String>,
}

pub(crate) fn settled_probe(
    plan: &ProbePlan,
    outcome: Outcome,
    tier: Option<probe::Location>,
    address: Option<String>,
) -> SettledProbe {
    SettledProbe {
        machine_id: plan.machine_id.clone(),
        outcome,
        tier,
        address,
    }
}

/// Test-only convenience: build a [`SettledProbe`] directly by machine id, for fixtures that have
/// a `SourceRef`/machine id in hand but no [`ProbePlan`] worth constructing just to read one field
/// off it.
#[cfg(test)]
pub(crate) fn settled_probe_for_test(
    machine_id: &str,
    outcome: Outcome,
    tier: Option<probe::Location>,
    address: Option<String>,
) -> SettledProbe {
    SettledProbe { machine_id: machine_id.to_owned(), outcome, tier, address }
}

/// Turn one settled race into what gets published and, when something verified, recorded —
/// `resolve_roster_using` and `probe_profile_resource_live` each needed this same mapping once per
/// server (plan §4). [`Reach::InsecureOnly`] keeps its candidate's tier and address (S9): the
/// state itself already carries the verdict, so a diagnostic tier is not a claim of usability.
fn probe_verdict(reach: &Reach) -> (Outcome, Option<probe::Location>, Option<String>) {
    match reach {
        Reach::At(c, _) => (Outcome::Reachable, Some(c.location), Some(c.address.clone())),
        Reach::InsecureOnly(c) => (Outcome::InsecureOnly, Some(c.location), Some(c.address.clone())),
        Reach::Refused => (Outcome::Unauthorized, None, None),
        Reach::No => (Outcome::Unreachable, None, None),
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
        // `apply_connection`, not the bare `set_link` this used to call: the same rule (R3) — the
        // IP generation comes from the candidate's own ADDRESS, never from `origin.host()`, which
        // is a `plex.direct` NAME for exactly the connections this fix is about — and `None` from
        // an unparseable address LEAVES a previously known ip alone (A1) rather than forcing it
        // back to unknown, since this function updates an ALREADY-registered client rather than
        // applying facts inside a fresh registration write.
        let ip = probe.address.as_deref().and_then(crate::plex::IpVersion::of_host);
        client.apply_connection(crate::plex::ConnectionFacts::new(Some(link), ip));
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
    // The same eligibility rule the racing coordinator applies (`settle_probe_message`): a
    // verified-but-ineligible answer is kept as evidence, never returned as `Reach::At`, and the
    // search continues past it — the next candidate may still verify AND be usable.
    let mut insecure: Option<Candidate> = None;
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
            Outcome::Reachable => {
                if !c.credential_eligible {
                    log(&format!(
                        "auth: '{}' answered only over plaintext at {}",
                        plan.name,
                        origin.log_form()
                    ));
                    if insecure.is_none() {
                        insecure = Some(c.clone());
                    }
                    continue;
                }
                return Reach::At(c.clone(), origin);
            }
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
            // `classify` never produces this; see `settle_probe_message`'s identical arm.
            Outcome::InsecureOnly => {}
        }
    }
    if let Some(c) = insecure {
        return Reach::InsecureOnly(c);
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
    /// 401" from "silence", and `insecure` marks that at least one server answered
    /// [`Reach::InsecureOnly`] — verified alive, but over a transport this build can never put a
    /// credential on. Three different things to tell the user.
    None { refused: bool, insecure: bool },
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
/// `household` is [`session::Session::household_ids`] — see [`credit_of`]. `policy` is passed
/// explicitly, as it is to every pure function in this file's discovery path — this function has
/// no live edge of its own and must not re-derive the build's policy on its own.
fn resolve_roster_using_admission(
    resources: &[Resource],
    household: &[i64],
    policy: CredentialPolicy,
    probe_one: &mut dyn FnMut(&ProbePlan, &[String]) -> Reach,
    admit: &mut dyn FnMut(&SourceRef) -> crate::plex::EndpointAdmission,
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
    observe: &mut dyn FnMut(&ProbePlan, Outcome, Option<probe::Location>, Option<String>),
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
    let mut insecure = false;
    for r in servers {
        let plan = probe::plan(r, policy);
        let mut rejected_origins = Vec::new();
        let reach = loop {
            let reach = probe_one(&plan, &rejected_origins);
            let Some(s) = source_from_reach(r, &plan, &reach, household) else { break reach };
            // Admission chooses the primary. Once one source is seated, every secondary keeps the
            // profile-specific grant plex.tv returned; its identity probe still decides endpoint
            // and reachability facts, but it does not have to browse before becoming live.
            if !found.is_empty() {
                if let Reach::At(c, origin) = &reach { activate(&plan, c, origin); }
                break reach;
            }
            match admit(&s) {
                crate::plex::EndpointAdmission::Usable => {
                    if let Reach::At(c, origin) = &reach { activate(&plan, c, origin); }
                    break reach;
                }
                crate::plex::EndpointAdmission::Refused(status) => {
                    refused = true;
                    log(&format!("auth: {:?} refused its per-profile grant (HTTP {status})",
                        plan.name));
                }
                crate::plex::EndpointAdmission::InsecureOnly => insecure = true,
                evidence => log(&format!("auth: {:?} did not admit its grant ({evidence:?})",
                    plan.name)),
            }
            let origin = s.origin_url;
            if rejected_origins.contains(&origin) { break Reach::No; }
            rejected_origins.push(origin);
        };
        let (outcome, tier, address) = probe_verdict(&reach);
        // Publish one aggregate result per server, after all of its direct/relay candidates have
        // settled. In particular a 401 remains distinct from silence, while wrong-machine-only
        // races fold to Unreachable because no address verified this server.
        observe(&plan, outcome, tier, address);
        match reach {
            Reach::At(c, origin) => {
                let s = source_from_reach(r, &plan, &Reach::At(c, origin.clone()), household)
                    .expect("matched reachable source");
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
            // The verified-but-ineligible probe itself already reached the registry through
            // `observe` above (one `RegistryProgress::Settled`/`RegistryPlan::Probe` per server,
            // published before this match runs) — R2/A5's registry-only commit besides. This arm
            // only records the fact for `Resolved`'s own aggregate, which decides the user-facing
            // `Discovery`/`ServerRosterOutcome` rather than the registry write.
            Reach::InsecureOnly(c) => {
                log(&format!(
                    "auth: '{}' verified at {}:{} but only over plaintext — not recorded",
                    plan.name, c.address, c.port
                ));
                insecure = true;
            }
            Reach::Refused => refused = true,
            Reach::No => {}
        }
    }
    if found.is_empty() {
        Resolved::None { refused, insecure }
    } else {
        Resolved::Reached(found)
    }
}

#[cfg(test)]
fn resolve_roster_using(
    resources: &[Resource],
    household: &[i64],
    policy: CredentialPolicy,
    probe_one: &mut dyn FnMut(&ProbePlan) -> Reach,
    observe: &mut dyn FnMut(&ProbePlan, Outcome, Option<probe::Location>, Option<String>),
) -> Resolved {
    resolve_roster_using_admission(resources, household, policy, &mut |plan, _| probe_one(plan),
        &mut |_| crate::plex::EndpointAdmission::Usable, &mut |_, _, _| {},
        observe)
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

/// [`credit_for_machine`]'s sibling for the grant EVIDENCE — plex.tv's `home` and `ownerId` for
/// the machine a [`ProbePlan`] names, paired out of the same response by the same
/// `clientIdentifier`.
///
/// It is separate from `credit_for_machine` rather than folded into it because the two answers
/// have different lifetimes at the call site: a credit is graded against the household roster and
/// is a decided STRING, while this is raw wire evidence that outlives any particular roster and is
/// re-graded downstream. An id that names no row carries no evidence, which degrades to exactly
/// what raw `owned` already said — the same "absence is the safe direction" the credit rule takes.
fn evidence_for_machine(resources: &[Resource], machine_id: &str) -> crate::plex::GrantEvidence {
    if machine_id.is_empty() {
        return crate::plex::GrantEvidence::default();
    }
    resources
        .iter()
        .find(|r| r.is_server() && r.client_identifier == machine_id)
        .map(|r| crate::plex::GrantEvidence::of(r.grant()))
        .unwrap_or_default()
}

/// The grant evidence a persisted [`SourceRef`] carries, for the registry describers.
fn grant_of(source: &SourceRef) -> crate::plex::GrantEvidence {
    crate::plex::GrantEvidence {
        owned: source.owned,
        home: source.home,
        owner_id: source.owner_id,
    }
}

/// Test seam for the pre-racing acceptance fixtures. The injected dial runs synchronously; the
/// racing coordinator has its own focused tests for completion order/refusal.
/// `policy` is explicit, like every other pure call in this path — most callers want `HttpsOnly`
/// (the store policy), and a fixture whose dial answers only over a plaintext twin passes
/// `AllowPlaintext` instead of asking this seam to guess.
#[cfg(test)]
fn resolve_roster(
    resources: &[Resource],
    household: &[i64],
    policy: CredentialPolicy,
    dial: &dyn Fn(&Origin) -> (i32, Vec<u8>),
) -> Resolved {
    let mut probe_one = |plan: &ProbePlan| probe_server(plan, dial);
    resolve_roster_using(
        resources,
        household,
        policy,
        &mut probe_one,
        &mut |_, _, _, _| {},
    )
}

fn resolve_roster_live_while(
    resources: &[Resource],
    household: &[i64],
    client_id: &str,
    activate: &mut dyn FnMut(&ProbePlan, &Candidate, &Origin),
    observe: &mut dyn FnMut(&ProbePlan, Outcome, Option<probe::Location>, Option<String>),
    live: &dyn Fn() -> bool,
) -> Resolved {
    let policy = CredentialPolicy::build();
    let dial: ProbeDial = Arc::new(get_identity);
    let spawn = |_index: usize, job: ProbeJob| crate::task::spawn_small("probe", job);
    let mut probe_one = |plan: &ProbePlan, rejected: &[String]| {
        if !live() { return Reach::No; }
        let plan = ProbePlan {
            machine_id: plan.machine_id.clone(), token: plan.token.clone(), owned: plan.owned,
            name: plan.name.clone(), source_title: plan.source_title.clone(), policy: plan.policy,
            candidates: plan.candidates.iter().filter(|candidate|
                dial_target(candidate).is_some_and(|origin|
                    !rejected.iter().any(|rejected| rejected == &origin.base())))
                .cloned().collect(),
        };
        probe_server_racing(&plan, Arc::clone(&dial), &spawn, PROBE_DEADLINES,
            &mut |_, _, _| {})
    };
    let mut admission_deadline = None;
    let mut admit = |source: &SourceRef| {
        if !live() { return crate::plex::EndpointAdmission::Transport; }
        let deadline = *admission_deadline.get_or_insert_with(||
            Instant::now().checked_add(ADMISSION_BUDGET).unwrap_or_else(Instant::now));
        crate::plex::admit_source_until(source, client_id, deadline)
    };
    resolve_roster_using_admission(
        resources,
        household,
        policy,
        &mut probe_one,
        &mut admit,
        activate,
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
        home: res.home,
        owner_id: res.owner_id,
        origin_url: origin.base(),
        address: c.address.clone(),
        port: c.port,
        token: plan.token.clone(),
        tier: Some(c.location),
        extensions: Default::default(),
    })
}

/// Probe exactly one resource, with no live-registry side effect. This is the bounded critical
/// path of a profile choice; whole-roster discovery deliberately remains a different operation.
fn probe_profile_resource_live(
    resource: &Resource,
    household: &[i64],
) -> (Option<SourceRef>, SettledProbe) {
    probe_profile_resource_live_after(resource, household, &[])
}

fn probe_profile_resource_live_after(
    resource: &Resource,
    household: &[i64],
    rejected_origins: &[String],
) -> (Option<SourceRef>, SettledProbe) {
    let mut plan = probe::plan(resource, CredentialPolicy::build());
    plan.candidates.retain(|candidate| dial_target(candidate).is_some_and(|origin|
        !rejected_origins.iter().any(|rejected| rejected == &origin.base())));
    let dial: ProbeDial = Arc::new(get_identity);
    let spawn = |_index: usize, job: ProbeJob| crate::task::spawn_small("probe", job);
    let reach = probe_server_racing(&plan, dial, &spawn, PROBE_DEADLINES, &mut |_, _, _| {});
    let (outcome, tier, address) = probe_verdict(&reach);
    let source = source_from_reach(resource, &plan, &reach, household);
    (source, settled_probe(&plan, outcome, tier, address))
}

/// Fold [`Resolved`]'s three empty-roster shapes into the [`Discovery`] outcome they map to, and
/// leave a real roster (`Resolved::Reached`) for the caller to keep processing. Pure and pulled out
/// of [`discover_and_store`] so the precedence rule (plan §4: `At` > `InsecureOnly` > `Refused` >
/// `No`) is itself gradeable on the dev Mac rather than only reachable through a live worker.
fn resolved_without_roster(resolved: Resolved) -> Result<Vec<SourceRef>, Discovery> {
    match resolved {
        Resolved::NoServers => Err(Discovery::NoServers),
        // A verified-but-plaintext answer is worth more to the user than a parallel/proxy 401,
        // because it names a fixable cause (HTTPS to the server) rather than a credential one.
        Resolved::None { insecure: true, .. } => {
            log("auth: at least one server verified only over plaintext (insecure-only), unusable in this build");
            Err(Discovery::InsecureOnly)
        }
        Resolved::None { refused: true, insecure: false } => Err(Discovery::Refused),
        Resolved::None { refused: false, insecure: false } => Err(Discovery::Silent(None)),
        Resolved::Reached(found) => Ok(found),
    }
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
fn discover_and_store(ac: &AccountClient, client_id: &str, epoch: u64,
    output: &dyn owner::ObservationSink) -> Discovery {
    if !output.live() { return Discovery::Cancelled; }
    let resources = match ac.resources() {
        Ok(r) => r,
        Err(last) => {
            // No response, or one that would not deserialize: plex.tv is unreachable from here.
            // NOT `NoServers` — that copy tells the user their account owns no server, which is a
            // statement about their account made on the strength of never having heard from it.
            log("auth: resources request FAILED (no response/deser)");
            return Discovery::Silent(Some(last));
        }
    };
    log(&format!(
        "auth: resources n={} servers={}",
        resources.len(),
        resources.iter().filter(|r| r.is_server()).count()
    ));
    let mut activate = |plan: &ProbePlan, c: &Candidate, origin: &Origin| {
        let credit = credit_for_machine(&resources, &plan.machine_id, &[]);
        let evidence = evidence_for_machine(&resources, &plan.machine_id);
        output.progress(AuthProgress::Registry(RegistryProgress::Activate {
            epoch,
            expected: None,
            candidate: candidate_activation(plan, c, origin, &credit, evidence),
        }));
    };
    let mut observe = |plan: &ProbePlan, outcome: Outcome, tier: Option<probe::Location>, address: Option<String>| {
        output.progress(AuthProgress::Registry(RegistryProgress::Settled {
            epoch,
            expected: None,
            probe: settled_probe(plan, outcome, tier, address),
        }));
    };
    // **No household ids here, and that is a fact about the ORDER rather than an omission**: the
    // Plex Home roster is fetched by `finish_sign_in`, *after* this runs, so at sign-in there is
    // nothing to enumerate the house with — and the CTL session at this moment may still be the
    // account that just signed out. Discovery is always performed with the ACCOUNT OWNER's token
    // (the QR flow authorizes the account, never a managed profile), so plex.tv's own `owned`
    // answers for their server and `home`/`ownerId` for the rest; the household refinement lands
    // with `refresh_roster` or the first profile switch, both of which pass the real roster.
    let resolved = resolve_roster_live_while(&resources, &[], client_id,
        &mut activate, &mut observe, &|| output.live());
    if !output.live() { return Discovery::Cancelled; }
    let found = match resolved_without_roster(resolved) {
        Ok(found) => found,
        Err(discovery) => return discovery,
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
        extensions: Default::default(),
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
/// replaces the credential cache with the authoritative granted roster. Fresh authenticated
/// admission selects the primary; secondary grants remain live on identity-verified or cached
/// endpoints. It preserves the current primary only when that endpoint was admitted; otherwise it
/// promotes the preferred admitted survivor so `current` cannot be stranded on cached metadata.
///
/// Persists only when the roster actually CHANGED, because the session file is on flash and a
/// rewrite per boot buys nothing.
///
/// **Its credential-bearing result is committed for the account OWNER only, and that is a
/// correctness gate rather than a policy.** The worker still probes on every stored boot so fresh,
/// credential-free reachability facts survive; the session owner rejects Activate/Install and the
/// Reconcile credential patch when [`Session::active_profile_is_admin`] is false.
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
        // Assigned, not merged, for the same reason the credit is: this is the path a Plex Home
        // profile switch takes, and the stored evidence belongs to whichever profile wrote it.
        cached.home = r.home;
        cached.owner_id = r.owner_id;
        if cached.dialable() {
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
                // The carried household evidence, or a roster whose ONLY change is that plex.tv
                // now names an owner would compare equal and never be republished.
                && a.home == b.home
                && a.owner_id == b.owner_id
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
        extensions: source.extensions.clone(),
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
    let resources = match ac.resources() {
        Ok(resources) => resources,
        Err(evidence) => {
            log(&format!("auth: server roster refresh could not list resources ({})",
                crate::plex::account::describe_evidence(&evidence)));
            output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
                epoch,
                expected,
                outcome: ServerRosterOutcome::Unreachable,
            }));
            return;
        }
    };
    let mut activate = |plan: &ProbePlan, c: &Candidate, origin: &Origin| {
        let credit = credit_for_machine(&resources, &plan.machine_id, &household);
        let evidence = evidence_for_machine(&resources, &plan.machine_id);
        output.progress(AuthProgress::Registry(RegistryProgress::Activate {
            epoch,
            expected: Some(expected.clone()),
            candidate: candidate_activation(plan, c, origin, &credit, evidence),
        }));
    };
    let mut settled = Vec::new();
    // Unlike `activate` above, this does NOT also `output.progress(RegistryProgress::Settled)`
    // per server as the race runs. It used to, and the owner committed BOTH: the live progress
    // arm (`Observation::Registry` in `owner.rs`) planned one `RegistryPlan::Probe` per server as
    // it settled, and the terminal `ServerRosterOutcome` below planned the identical set again
    // through `settled`/`roster_plan` — every probe this worker ever runs was published twice.
    // This worker is a BACKGROUND refresh (`refresh_roster`) with no live race screen watching
    // it — unlike `discover_and_store`'s sign-in flow, which really does drive a Sources/QR
    // screen off `RegistryProgress::Activate` while candidates settle, and keeps publishing both
    // — so there is nothing here for the extra progress arrival to reach before the terminal
    // commit does the same work. `settled` alone, folded into the terminal outcome, is authoritative.
    let found = match resolve_roster_live_while(
        &resources,
        &household,
        &sess.client_id,
        &mut activate,
        &mut |plan, outcome, tier, address| {
            settled.push(settled_probe(plan, outcome, tier, address));
        },
        &|| output.live(),
    ) {
        Resolved::Reached(found) => found,
        _ => {
            // R2/A5: a background refresh that found nothing to REGISTER may still have PROBED
            // something worth recording — carry it rather than throwing `settled` away, so the
            // owner can commit it as a registry-only `RegistryPlan::Probe` (`owner.rs`).
            output.terminal(AuthProgress::ServerRoster(ServerRosterProgress {
                epoch,
                expected,
                outcome: ServerRosterOutcome::NoReachable { settled },
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
        home: source.home,
        owner_id: source.owner_id,
        extensions: source.extensions.clone(),
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
    resources: impl FnOnce(&AccountClient) -> Result<Vec<Resource>, CallEvidence>,
    probe: impl FnOnce(&Resource, &[i64]) -> (Option<SourceRef>, SettledProbe),
    live: &dyn Fn() -> bool,
) -> (Option<SourceRef>, Option<SettledProbe>) {
    // Nothing was actually probed on any of the early exits below — plex.tv never answered, or
    // this machine is no longer among its resources — so there is no verdict to report at all:
    // fabricating `Unreachable` here would widen a real, more specific probe result
    // (`InsecureOnly`/`Unauthorized`) into "Not reachable" once it reached the registry.
    if !live() { return (None, None); }
    let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
    let resources = match resources(&ac) {
        Ok(resources) => resources,
        Err(evidence) => {
            log(&format!(
                "auth: endpoint refresh for source {} could not list resources ({})",
                id.raw(),
                crate::plex::account::describe_evidence(&evidence)
            ));
            return (None, None);
        }
    };
    let Some(resource) = resources
        .iter()
        .find(|resource| resource.is_server() && resource.client_identifier == machine_id)
    else {
        log(&format!(
            "auth: endpoint refresh for source {} found no matching resource",
            id.raw()
        ));
        return (None, None);
    };
    if !live() { return (None, None); }
    let (fresh, probe) = probe(resource, &sess.household_ids());
    (fresh, Some(probe))
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
        .find(|s| s.machine_id == server.machine_id && s.dialable())
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
fn install_roster(sources: &[SourceRef], primary: Option<usize>, client_id: &str) -> Vec<ServerId> {
    let order = registration_order(sources);
    let mut installed = Vec::with_capacity(order.len());
    for &i in &order {
        let s = &sources[i];
        // `registration_order` already filtered on `usable()`, which IS `origin().is_some()` —
        // so this `else` is unreachable today and is a `continue` rather than an `expect` because
        // a roster entry has never been allowed to cost more than itself (`de_soft_vec`).
        let Some(origin) = s.origin() else { continue };
        // Applied atomically inside the same write (#95 step 8) — a re-pointed slot's fresh
        // `Client` gets its connection facts from THIS call rather than a separate one after.
        let connection =
            crate::plex::ConnectionFacts::new(s.tier, crate::plex::IpVersion::of_host(&s.address));
        let id = register_observed_origin(&s.machine_id, &origin, &s.token,
            s.resolve_pin().as_ref(), connection, client_id);
        if !id.is_set() {
            continue;
        }
        installed.push(id);
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
        crate::plex::describe_server(id, &s.name, &s.shared_by, grant_of(s));
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
        .filter(|&i| sources[i].dialable())
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

/// Keep the profile's own grants while preferring endpoints verified during this switch.
/// Authenticated admission chooses the primary; secondary grants remain live on their cached
/// endpoints while identity probes refresh their reachability and connection facts.
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
    fn resources(&mut self, account: &AccountClient) -> Result<Vec<Resource>, CallEvidence>;
    fn probe(&mut self, resource: &Resource, household: &[i64]) -> (Option<SourceRef>, SettledProbe);
    fn probe_after(&mut self, resource: &Resource, household: &[i64], _: &[String])
        -> (Option<SourceRef>, SettledProbe) {
        self.probe(resource, household)
    }
    fn probe_cached(&mut self, _: &Resource, _: &SourceRef, _: &[i64], _: &[String])
        -> Option<(Option<SourceRef>, SettledProbe)> {
        None
    }
    #[cfg(not(test))]
    fn admit(&mut self, source: &SourceRef, client_id: &str) -> crate::plex::EndpointAdmission;
    #[cfg(test)]
    fn admit(&mut self, _: &SourceRef, _: &str) -> crate::plex::EndpointAdmission {
        crate::plex::EndpointAdmission::Usable
    }
    fn admit_until(&mut self, source: &SourceRef, client_id: &str, _: Instant)
        -> crate::plex::EndpointAdmission {
        self.admit(source, client_id)
    }
    fn admission_budget(&self) -> Duration { ADMISSION_BUDGET }
}

struct LiveProfileWorkIo<S> { switch: Option<S> }

impl<S: FnOnce(&AccountClient, &str, Option<&str>) -> SwitchOutcome> ProfileWorkIo for LiveProfileWorkIo<S> {
    fn switch(&mut self, account: &AccountClient, uuid: &str, pin: Option<&str>) -> SwitchOutcome {
        self.switch.take().expect("one switch request per profile worker")(account, uuid, pin)
    }
    fn resources(&mut self, account: &AccountClient) -> Result<Vec<Resource>, CallEvidence> { account.resources() }
    fn probe(&mut self, resource: &Resource, household: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        probe_profile_resource_live(resource, household)
    }
    fn probe_after(&mut self, resource: &Resource, household: &[i64], rejected: &[String])
        -> (Option<SourceRef>, SettledProbe) {
        probe_profile_resource_live_after(resource, household, rejected)
    }
    fn probe_cached(&mut self, resource: &Resource, cached: &SourceRef, household: &[i64],
        rejected: &[String]) -> Option<(Option<SourceRef>, SettledProbe)> {
        let origin = cached.origin()?;
        if rejected.iter().any(|rejected| rejected == &origin.base()) { return None; }
        let plan = probe::plan(resource, CredentialPolicy::build());
        let pin = cached.resolve_pin();
        let budget = if cached.tier == Some(probe::Location::Local) {
            PROBE_DEADLINES.local
        } else {
            PROBE_DEADLINES.remote
        };
        let (status, body) = get_identity(&origin, pin.as_ref(), budget);
        let outcome = classify(status, &body, &plan.machine_id);
        let source = (outcome == Outcome::Reachable).then(|| {
            let mut fresh = cached.clone();
            fresh.token = resource.access_token.clone();
            fresh.name = resource.name.clone();
            fresh.owned = resource.owned;
            fresh.shared_by = credit_of(resource, household);
            fresh.home = resource.home;
            fresh.owner_id = resource.owner_id;
            fresh
        });
        Some((source, settled_probe(&plan, outcome, cached.tier,
            (outcome == Outcome::Reachable).then(|| cached.address.clone()))))
    }
    fn admit(&mut self, source: &SourceRef, client_id: &str) -> crate::plex::EndpointAdmission {
        crate::plex::admit_source(source, client_id)
    }
    fn admit_until(&mut self, source: &SourceRef, client_id: &str, deadline: Instant)
        -> crate::plex::EndpointAdmission {
        crate::plex::admit_source_until(source, client_id, deadline)
    }
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
    let resources = match io.resources(&AccountClient::new(&cid, Some(&user.auth_token))) {
        Ok(resources) => resources,
        Err(evidence) => {
            log(&format!("auth: profile resources request failed ({})",
                crate::plex::account::describe_evidence(&evidence)));
            // A refusal is an answer, not a dead link: "check the connection" would send the
            // person to a router that is fine. The PIN was already accepted by this point, so
            // this is never the PIN flash either.
            let error = if crate::plex::account::refused_identity(&evidence).is_some() {
                format!("plex.tv refused {}\u{2019}s sign-in. Try again.", tile.title)
            } else {
                "Couldn't switch profile — check the connection.".into()
            };
            output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
                epoch,
                expected,
                outcome: ProfileSwitchOutcomeProgress::Failed {
                    error,
                    pin_denied: false,
                },
            }));
            return;
        }
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
    let mut admission_deadline = None;
    let mut order = grants.clone();
    if let Some(pos) = order
        .iter()
        .position(|&i| resources[i].client_identifier == stored.server.machine_id)
    {
        order.swap(0, pos);
    }
    let mut reached = Vec::new();
    let mut probes: Vec<SettledProbe> = Vec::new();
    let mut probed = vec![false; resources.len()];
    let mut admission_failures = Vec::new();
    let mut selected_mid = None;
    for &i in &order {
        probed[i] = true;
        let mut rejected_origins = Vec::new();
        loop {
            if !output.live() { return; }
            if admission_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                admission_failures.push((resources[i].name.clone(),
                    crate::plex::EndpointAdmission::Timeout));
                break;
            }
            let (mut winner, mut settled) =
                io.probe_after(&resources[i], &household, &rejected_origins);
            if !output.live() { return; }
            if winner.is_none() {
                if let Some(cached) = stored.sources.iter()
                    .find(|source| source.machine_id == resources[i].client_identifier) {
                    if let Some((cached_winner, cached_settled)) =
                        io.probe_cached(&resources[i], cached, &household, &rejected_origins) {
                        winner = cached_winner;
                        settled = cached_settled;
                    }
                }
            }
            if !output.live() { return; }
            if let Some(previous) = probes.iter_mut()
                .find(|probe| probe.machine_id == resources[i].client_identifier) {
                if previous.outcome != Outcome::Reachable || settled.outcome == Outcome::Reachable {
                    *previous = settled;
                }
            } else {
                probes.push(settled);
            }
            let Some(winner) = winner else { break };
            let origin = winner.origin_url.clone();
            if rejected_origins.contains(&origin) { break; }
            let deadline = *admission_deadline.get_or_insert_with(|| Instant::now()
                .checked_add(io.admission_budget()).unwrap_or_else(Instant::now));
            let admission = if Instant::now() >= deadline {
                crate::plex::EndpointAdmission::Timeout
            } else {
                io.admit_until(&winner, &cid, deadline)
            };
            if admission == crate::plex::EndpointAdmission::Usable {
                selected_mid = Some(winner.machine_id.clone());
                reached.push(winner);
                break;
            }
            log(&format!("auth: switch '{}' — {:?} did not admit this profile ({admission:?})",
                tile.title, resources[i].name));
            admission_failures.push((resources[i].name.clone(), admission));
            rejected_origins.push(origin);
        }
        if selected_mid.is_some() {
            break;
        }
    }
    let cached_sources = profile_sources(&stored.sources, &reached, &resources, &household);
    let initial = cached_sources.clone();
    let Some(primary) =
        selected_mid.and_then(|mid| initial.iter().find(|s| s.machine_id == mid).cloned())
    else {
        // S7: a grant that verified but only over plaintext is not the same failure as "you were
        // never given this server" — it names a fixable cause (HTTPS to the server), and the
        // ordinary copy sends the user to ask their friend for access they already have.
        let insecure_only = probes.iter().any(|p| p.outcome == Outcome::InsecureOnly)
            || admission_failures.iter().any(|(_, evidence)|
                *evidence == crate::plex::EndpointAdmission::InsecureOnly);
        let refusal = admission_failures.iter().find_map(|(name, evidence)| match evidence {
            crate::plex::EndpointAdmission::Refused(status) => Some((name, *status)),
            _ => None,
        });
        let malformed = admission_failures.iter().any(|(_, evidence)|
            *evidence == crate::plex::EndpointAdmission::Malformed);
        log(&format!(
            "auth: switch '{}' -> {}",
            tile.title,
            if insecure_only { "verified only over plaintext" }
            else if refusal.is_some() { "server refused the profile credential" }
            else if malformed { "server returned a malformed authenticated response" }
            else { "no server access" },
        ));
        output.terminal(AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            epoch,
            expected,
            outcome: ProfileSwitchOutcomeProgress::Failed {
                error: if insecure_only {
                    DISCOVERY_INSECURE_ONLY_MESSAGE.to_owned()
                } else if let Some((name, _)) = refusal {
                    format!("{name} refused {}. Try again.", tile.title)
                } else if malformed {
                    "Couldn't switch profile — the server sent an invalid response.".into()
                } else if !admission_failures.is_empty() {
                    "Couldn't switch profile — check the connection.".into()
                } else {
                    format!("{} has no access to this server", tile.title)
                },
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
        extensions: Default::default(),
    };
    if !output.live() { return; }
    // Carry forward whatever this uuid's PRIOR seating cached in `extensions` — the serde-flatten
    // catch-all for fields this build does not model (forward compatibility with a newer build
    // that wrote this session). `remember_profile` below replaces that uuid's whole cache entry,
    // so building a fresh one with `Default::default()` here silently erased it (Copilot review on
    // PR #105, finding 3).
    let carried_extensions = stored
        .profiles
        .iter()
        .find(|p| p.uuid == user.uuid)
        .map(|p| p.extensions.clone())
        .unwrap_or_default();
    let cache = ProfileCreds {
        uuid: user.uuid.clone(),
        user: user.clone(),
        server: server.clone(),
        sources: cached_sources,
        pin: pin
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(session::PinVerifier::new),
        extensions: carried_extensions,
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
        let (mut winner, mut settled) = io.probe_after(&resources[i], &household, &[]);
        if !output.live() { return; }
        if winner.is_none() {
            if let Some(cached) = stored.sources.iter()
                .find(|source| source.machine_id == resources[i].client_identifier) {
                if let Some((cached_winner, cached_settled)) =
                    io.probe_cached(&resources[i], cached, &household, &[]) {
                    winner = cached_winner;
                    settled = cached_settled;
                }
            }
        }
        if !output.live() { return; }
        if let Some(previous) = probes.iter_mut()
            .find(|probe| probe.machine_id == resources[i].client_identifier) {
            if previous.outcome != Outcome::Reachable || settled.outcome == Outcome::Reachable {
                *previous = settled;
            }
        } else {
            probes.push(settled);
        }
        if let Some(winner) = winner { reached.push(winner); }
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
#[path = "auth_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "auth_discovery_tests.rs"]
mod discovery_tests;

#[cfg(test)]
#[path = "auth_profile_seat_tests.rs"]
mod profile_seat_tests;

#[cfg(test)]
#[path = "auth_registry_tests.rs"]
mod registry_tests;

#[cfg(test)]
#[path = "auth_qr_wait_tests.rs"]
mod qr_wait_tests;

#[cfg(test)]
#[path = "auth_session_worker_tests.rs"]
mod session_worker_tests;

#[cfg(test)]
#[path = "auth_storage_extension_tests.rs"]
mod storage_extension_tests;
