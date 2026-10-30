//! FinNative's sign-in: a Jellyfin server, and Quick Connect or a username and password.
//!
//! It stands in for the plex.tv QR flow in [`super::login_worker_with_output`] (through
//! [`run_session_work`], which takes the work FinNative does itself) and speaks the same
//! observations, so the sign-in screen, the session owner and what it saves are unchanged:
//!
//! 1. take the server: the one the sign-in form typed ([`backend::take_sign_in_request`]), else
//!    `FINNATIVE_SERVER`, else the first that answers a LAN broadcast;
//! 2. sign in as the form asked: with the username and password it holds, or by Quick Connect —
//!    a code on screen ([`LoginProgress::CodeReady`]) with a QR of the server's Quick Connect
//!    page, polled until the user approves it in another Jellyfin app;
//! 3. hand the server and its token to the session owner as the signed-in primary, with the
//!    server's whole base URL and the signed-in user saved beside it ([`backend::BASE_KEY`],
//!    [`backend::USER_KEY`]).
//!
//! ## A server on plain http
//!
//! A release build sends a credential over plaintext only to an origin with a live grant
//! (`plx_plex::plex::grant`). A Plex server earns one from plex.tv's verdict and the person's
//! answer; a Jellyfin server has neither, so its grant is vouched for here
//! (`PlaintextAsk::settle_vouched`): the server answered its tokenless `/System/Info/Public` at that
//! origin with its own Id, and the person chose it by signing in. That happens right after the
//! sign-in's probe, before the password or the Quick Connect secret goes out, and again by
//! [`endpoint_worker`] whenever the grant is gone and the server is asked for — at boot from the
//! saved session, and after the app returns to the foreground, when every grant dies because
//! nothing proves the television is still on the same network. That re-proof is a round trip to
//! the server, while a request refused for want of the grant fails at once, so the Jellyfin
//! backend also proves the saved server itself, on the same evidence, before a request that
//! carries its token goes out (`JfBackend::prove_saved_server`). Only a numeric private-network
//! address is granted; an `http://` name or a public address is refused with a message saying so.
//! Settings' *Unencrypted connections* lists the server once it has been granted, and turning it
//! off there is honoured until the person signs in to it again.
//!
//! The password reaches this worker through the backend's one-shot request, never through the
//! session's work description, which is serialized; nothing here logs it or the username.
//!
//! There is no plex.tv account behind a Jellyfin sign-in. The session layer still keys "signed in"
//! and every save on a non-empty account token, so it gets [`ACCOUNT_MARKER`], which is not a
//! credential: with the Jellyfin backend installed, `plex_tv()` points nowhere, so a plex.tv call
//! that token would ride (rediscovery) fails as "plex.tv unreachable", the offline case the app
//! already handles, and the server roster refresh is not asked at all (`plex::has_plex_home`). It is also the "plex.tv credential" Settings and playback
//! require before asking for the audio and subtitle preferences (`session::plex_tv_credential`),
//! which the Jellyfin backend then answers from the user's own configuration.
//!
//! ## Several users
//!
//! Every Jellyfin user signed in on the set is a profile, as a Plex Home member is: one tile in
//! the session's roster (`home_users`, its uuid the Jellyfin user id, no PIN) and one cached
//! sign-in (`profiles`: the user, that user's own server and token), so users can be on different
//! servers. *Add user* is an ordinary sign-in, which keeps the saved session; [`add_user`] then
//! lays the new user beside the saved ones, the previous user included (an install signed in
//! before there were several users has no record for it yet). *Change profile* lists them
//! ([`roster`]) and [`switch_worker`] seats one: its server proved again at its saved base, then
//! `/Users/Me` asked with its token. A token the server stopped accepting says to sign in again;
//! a server that does not answer seats the user from the cache, as an offline Plex pick does.
//! Switching reads only: it reports no playback and writes no watch state.
//!
//! A user on another server than the seated one has no avatar on the picker: the poster pipeline
//! draws a tile's image through the seated user's server (`Art::Thumb` names it), and
//! the Jellyfin backend has one client, which builds every image path on its own base.

use super::owner;
use super::{output_failed, AuthProgress, LoginProgress, RegistryProgress};
use plx_base::eventlog::log;
use plx_jellyfin::jellyfin::backend::{self, SignInMethod};
use plx_jellyfin::jellyfin::client::{normalize_base, JfClient, Session, SignInError};
use plx_jellyfin::jellyfin::dto::PublicSystemInfo;
use plx_jellyfin::jellyfin::ids::normalize;
use plx_plex::plex::grant::{self, MintRefusal, PlaintextAsk};
use plx_plex::plex::session::{PlaintextChoice, ProfileCreds, ServerRef, SourceRef, UserRef};
use plx_plex::plex::{CredentialPolicy, Origin};
use plx_telemetry::telemetry::incident::{IncidentContext, IncidentKind};
use std::time::{Duration, Instant};

/// How long one Quick Connect code is waited on before a fresh one is asked for. Jellyfin forgets
/// a pending request after ten minutes.
const CODE_WINDOW: Duration = Duration::from_secs(9 * 60);
const MAX_CODES: u32 = 4;
const POLL_EVERY: Duration = Duration::from_secs(2);

/// The session's account token for a Jellyfin sign-in (see the module doc).
const ACCOUNT_MARKER: &str = "finnative-jellyfin";

/// **The session work FinNative does itself** ([`super::run_session_work`] hands every work item
/// here first): the sign-in, the users' roster, a switch and an endpoint's re-proof, none of which
/// has a plex.tv account to ask. Anything else comes back, for PlxNative's own worker.
pub(super) fn run_session_work(epoch: u64, input: owner::SessionWork, ask: &PlaintextAsk,
    output: &dyn owner::ObservationSink) -> Option<owner::SessionWork> {
    use super::{AuthProgress as P, HomeRosterProgress, ProfileSwitchProgress, SessionIdentity};
    let identity = |v: owner::Identity| SessionIdentity { client_id: v.client_id, account_token: v.account_token,
        profile_uuid: v.profile_uuid };
    match input {
        owner::SessionWork::Login { client_id } => login_worker(epoch, &client_id, ask, output),
        // The users are the sign-ins saved on this set: there is no plex.tv roster to read.
        owner::SessionWork::HomeRoster { session, expected, .. } => {
            output.terminal(P::HomeRoster(HomeRosterProgress { epoch, expected: identity(expected),
                users: Some(roster(&session)) }));
        }
        // A saved Jellyfin sign-in is seated: its server, proved again, and its token.
        owner::SessionWork::ProfileSwitch { session, expected, tile, .. } => {
            if let Some(outcome) = switch_worker(&session, &tile, ask, output) {
                output.terminal(P::ProfileSwitch(ProfileSwitchProgress { epoch, expected: identity(expected), outcome }));
            }
        }
        // The Jellyfin server is re-proved here: there is no plex.tv resource to read.
        owner::SessionWork::Endpoint { session, expected, lifecycle, machine_id } => {
            let fresh = endpoint_worker(&session, &machine_id, ask, output);
            output.terminal(super::endpoint_work_fact(epoch, expected, lifecycle, machine_id, fresh, None));
        }
        other => return Some(other),
    }
    None
}

pub(super) fn login_worker(epoch: u64, client_id: &str, ask: &PlaintextAsk, output: &dyn owner::ObservationSink) {
    use plx_platform::i18n::msg;
    if !output.live() {
        return;
    }
    // What the sign-in form asked for, if this sign-in was started from it; otherwise Quick
    // Connect on the server the network answers with.
    let request = backend::take_sign_in_request();
    let (typed, method) = match request {
        Some(r) => (Some(r.server).filter(|s| !s.trim().is_empty()), r.method),
        None => (None, SignInMethod::QuickConnect),
    };
    let Some(base) = typed.map(|s| normalize_base(&s)).or_else(find_server) else {
        return fail(output, epoch, msg::browse_jf_no_server());
    };
    backend::set_server_hint(&backend::display_address(&base));
    log("auth: jellyfin server chosen");
    let mut jf = backend::curl_client(&base, client_id);
    let info = match jf.probe() {
        Ok(info) => info,
        Err(e) => {
            log(&format!("auth: jellyfin probe failed ({e:?})"));
            let message = match e {
                SignInError::NotJellyfin => msg::browse_jf_not_jellyfin(),
                _ => msg::browse_jf_unreachable(),
            };
            return fail(output, epoch, message);
        }
    };
    // Before any credential goes out: a plaintext server needs its grant (see the module doc).
    if let Some(origin) = Origin::parse(jf.base()) {
        let machine_id = normalize(&info.id);
        let first = vouch(ask, &machine_id, &origin, CredentialPolicy::build(), live_grant);
        let granted = settled(first, || {
            // The capture predates a network the app is still settling into: ask the server who it
            // is again, and vouch for it under what is true now.
            log("auth: the network moved while signing in; vouching again");
            match jf.probe() {
                Ok(again) if normalize(&again.id) == machine_id => {
                    vouch(&PlaintextAsk::capture(""), &machine_id, &origin, CredentialPolicy::build(), live_grant)
                }
                _ => Err(MintRefusal::Stale),
            }
        });
        if let Err(refusal) = granted {
            log(&format!("auth: jellyfin server on plain http not granted ({refusal:?})"));
            let message = match refusal {
                MintRefusal::NotPrivateLiteral => msg::browse_jf_plaintext_name(),
                _ => msg::browse_jf_sign_in_failed(),
            };
            return fail(output, epoch, message);
        }
    }
    let session = match method {
        SignInMethod::Password { username, password } => {
            match jf.sign_in(&username, &password) {
                Ok(s) => s,
                Err(e) => {
                    log(&format!("auth: jellyfin password sign-in failed ({e:?})"));
                    let message = match e {
                        SignInError::Rejected => msg::browse_jf_wrong_password(),
                        SignInError::Unreachable => msg::browse_jf_unreachable(),
                        _ => msg::browse_jf_sign_in_failed(),
                    };
                    return fail(output, epoch, message);
                }
            }
        }
        SignInMethod::QuickConnect => match quick_connect(epoch, &mut jf, output) {
            Some(s) => s,
            None => return,
        },
    };
    log("auth: jellyfin signed in");
    finish(epoch, jf, &info, session, output);
}

/// A failed sign-in, read out with the incident the sign-in screen reports it under.
fn fail(output: &dyn owner::ObservationSink, epoch: u64, message: &str) {
    output_failed(output, epoch, message, IncidentContext::new(IncidentKind::PinCreate, None), None);
}

/// Quick Connect: put a code on screen and wait for it to be approved, asking for a fresh code when
/// one runs out. `None` once the outcome has been published (a failure, or the screen going away).
fn quick_connect(epoch: u64, jf: &mut JfClient, output: &dyn owner::ObservationSink) -> Option<Session> {
    use plx_platform::i18n::msg;
    let quickconnect = format!("{}/web/#/quickconnect", jf.base());
    backend::set_link_address(&backend::display_address(jf.base()));
    for generation in 1..=MAX_CODES {
        if generation > 1 && !output.progress(LoginProgress::CodeReplacing { epoch }.into()) {
            return None;
        }
        let started = match jf.quick_connect_start() {
            Ok(r) if !r.code.is_empty() && !r.secret.is_empty() => r,
            Ok(_) | Err(SignInError::Server(_)) | Err(SignInError::NotJellyfin) | Err(SignInError::Rejected) => {
                fail(output, epoch, msg::browse_jf_quick_connect_failed());
                return None;
            }
            Err(SignInError::QuickConnectDisabled) => {
                fail(output, epoch, msg::browse_jf_quick_connect_off());
                return None;
            }
            Err(SignInError::Unreachable) => {
                fail(output, epoch, msg::browse_jf_unreachable());
                return None;
            }
        };
        log(&format!("auth: quick connect code {generation} of {MAX_CODES}"));
        let qr_png = plx_jellyfin::jellyfin::qrpng::qr_png(&quickconnect, 8).unwrap_or_default();
        if !output.live()
            || !output.progress(LoginProgress::CodeReady { epoch, code: started.code.clone(), qr_png }.into())
        {
            return None;
        }
        let minted = Instant::now();
        while minted.elapsed() < CODE_WINDOW {
            std::thread::sleep(POLL_EVERY);
            if !output.live() {
                return None;
            }
            match jf.quick_connect_poll(&started.secret) {
                Ok(Some(s)) => {
                    log("auth: quick connect approved");
                    backend::set_link_address("");
                    return Some(s);
                }
                Ok(None) | Err(SignInError::Unreachable) | Err(SignInError::Server(_)) => {}
                Err(_) => break, // the request was forgotten: ask for a new code
            }
        }
    }
    output_failed(output, epoch, msg::browse_auth_timeout(), IncidentContext::new(IncidentKind::PinExpired, None), None);
    None
}

/// Hand a signed-in server to the session owner as the primary: the same observations a plex.tv
/// sign-in ends with.
fn finish(epoch: u64, jf: JfClient, info: &PublicSystemInfo, session: Session, output: &dyn owner::ObservationSink) {
    // No plex.tv account: the account token is a marker (see the module doc).
    if !output.progress(LoginProgress::Authorized { epoch, token: ACCOUNT_MARKER.into() }.into()) {
        return;
    }
    let Some(origin) = Origin::parse(jf.base()) else {
        return fail(output, epoch, plx_platform::i18n::msg::browse_jf_bad_address());
    };
    let mut source = SourceRef {
        machine_id: session.server_id.clone(),
        name: info.server_name.clone(),
        owned: true,
        origin_url: origin.base(),
        address: origin.host().to_owned(),
        port: i64::from(origin.port()),
        token: session.token.clone(),
        tier: Some(plx_plex::plex::probe::configured_tier(origin.host())),
        ..Default::default()
    };
    // The whole base, path included: the origin above is scheme, host and port only.
    source.extensions.0.insert(backend::BASE_KEY.into(), jf.base().into());
    // Who signed in, so a launch resumes without asking the server again.
    backend::save_user(&mut source.extensions, &backend::user_record(&session));
    let server = super::server_ref(&source);
    if let Some(b) = backend::instance() {
        b.set_client(Some(jf));
    }
    if !output.progress(AuthProgress::Registry(RegistryProgress::Install {
        epoch,
        expected: None,
        sources: vec![source.clone()],
        primary: Some(0),
    })) {
        return;
    }
    output.terminal(LoginProgress::SignedIn { epoch, server, sources: vec![source], users: Vec::new() }.into());
}

/// **Re-prove a saved server** (`SessionWork::Endpoint`): ask its saved base, without a credential,
/// who it is, and hand back its saved record when it answers with `machine_id` — after granting
/// its plaintext origin again where the build needs a grant. `None` when it did not answer, is
/// another server now, or may not be granted; the record keeps whatever it had.
pub(super) fn endpoint_worker(
    session: &plx_plex::plex::session::Session,
    machine_id: &str,
    ask: &PlaintextAsk,
    output: &dyn owner::ObservationSink,
) -> Option<SourceRef> {
    if !output.live() {
        return None;
    }
    reprove(session, machine_id, |base| backend::curl_client(base, &session.client_id).probe().ok(),
        |id, origin| {
            vouch_again_when_stale(ask, || PlaintextAsk::capture(ACCOUNT_MARKER), id, origin,
                CredentialPolicy::build(), live_grant)
        })
}

/// [`endpoint_worker`] with its IO passed in: `probe` asks a base who it is, `grant` vouches for
/// the machine at its origin.
fn reprove(
    session: &plx_plex::plex::session::Session,
    machine_id: &str,
    probe: impl FnOnce(&str) -> Option<PublicSystemInfo>,
    grant: impl FnOnce(&str, &Origin) -> Result<(), MintRefusal>,
) -> Option<SourceRef> {
    let source = session.sources.iter().find(|s| s.machine_id == machine_id)?.clone();
    let origin = source.origin()?;
    let base = backend::base_of(&source.extensions, &origin);
    let Some(info) = probe(&base) else {
        log("auth: jellyfin server did not answer its saved address");
        return None;
    };
    if normalize(&info.id) != machine_id {
        log("auth: another jellyfin server answers the saved address");
        return None;
    }
    if let Err(refusal) = grant(machine_id, &origin) {
        log(&format!("auth: jellyfin server on plain http not granted ({refusal:?})"));
        return None;
    }
    Some(source)
}

/// Grant `machine_id` at `origin` when it may not carry a credential yet — `policy` refuses it and
/// no live grant admits it (see the module doc) — through `mint`. A server whose grant was never answered for has it recorded as allowed,
/// so Settings' *Unencrypted connections* lists it and can turn it off.
fn vouch(
    ask: &PlaintextAsk,
    machine_id: &str,
    origin: &Origin,
    policy: CredentialPolicy,
    mint: impl FnOnce(&PlaintextAsk, &str, &Origin) -> Result<(), MintRefusal>,
) -> Result<(), MintRefusal> {
    if grant::allowed_for(policy, machine_id, origin) {
        return Ok(());
    }
    if ask.refused(machine_id) {
        return Err(MintRefusal::NotConsented);
    }
    mint(ask, machine_id, origin)?;
    if ask.choice(machine_id) == PlaintextChoice::Undecided {
        let account = grant::account_key(ACCOUNT_MARKER);
        if grant::record(&account, machine_id, PlaintextChoice::Allowed).is_err() {
            log("auth: the allowed plaintext server was not saved");
        }
    }
    Ok(())
}

/// A grant refused as [`MintRefusal::Stale`] is asked for once more through `again`: the sign-in's
/// capture is taken when it starts, and discovery takes seconds, during which a launch's network
/// settles, so the first answer is old by the time the server is known. Any other answer stands.
fn settled(
    first: Result<(), MintRefusal>,
    again: impl FnOnce() -> Result<(), MintRefusal>,
) -> Result<(), MintRefusal> {
    match first {
        Err(MintRefusal::Stale) => again(),
        other => other,
    }
}

/// [`vouch`] for work whose capture may be old when it gets here (the boot's re-proof of a saved
/// server): a stale grant is asked for once more, under the capture `fresh` takes now.
fn vouch_again_when_stale(
    ask: &PlaintextAsk,
    fresh: impl FnOnce() -> PlaintextAsk,
    machine_id: &str,
    origin: &Origin,
    policy: CredentialPolicy,
    mint: impl Fn(&PlaintextAsk, &str, &Origin) -> Result<(), MintRefusal>,
) -> Result<(), MintRefusal> {
    settled(vouch(ask, machine_id, origin, policy, &mint), || {
        vouch(&fresh(), machine_id, origin, policy, &mint)
    })
}

/// The live mint behind [`vouch`].
fn live_grant(ask: &PlaintextAsk, machine_id: &str, origin: &Origin) -> Result<(), MintRefusal> {
    ask.settle_vouched(machine_id, origin)
}

/// The users saved on this set, as picker tiles, in the order they were added: the saved roster's
/// order, since a seat moves its user's cached sign-in to the end of `profiles`. A user on another
/// server than the seated one has no avatar (see the module doc).
pub(super) fn roster(session: &plx_plex::plex::session::Session) -> Vec<super::UserTile> {
    let mut users = session.profiles.clone();
    if let Some(seated) = signed_in(&session.server, &session.sources) {
        keep(&mut users, seated);
    }
    let place = |p: &ProfileCreds| session.home_users.iter().position(|u| u.uuid == p.uuid).unwrap_or(usize::MAX);
    users.sort_by_key(place);
    let mut tiles: Vec<_> = users.iter().filter(|p| !p.uuid.is_empty()).map(|p| tile(p, &session.server.machine_id)).collect();
    named_by(&mut tiles, &session.home_users);
    tiles
}

/// The roster work's session with each user's name and avatar taken from `stored`: the backend
/// saves a server-side edit (`UserUpdated`) there, and the owner's copy the roster is listed from
/// has not seen it. The runner calls this, since the owner reads no registry. (The deeper fix is
/// the owner re-reading the stored session when the backend saves an edit.)
pub fn adopt_stored_names(session: &mut plx_plex::plex::session::Session, stored: &plx_plex::plex::session::Session) {
    for home in &mut session.home_users {
        if let Some(u) = stored.home_users.iter().find(|u| u.uuid == home.uuid) {
            home.title.clone_from(&u.title);
            home.thumb.clone_from(&u.thumb);
        }
    }
}

/// `tiles` named as `home` names them (the roster's order and ids stay the saved users').
fn named_by(tiles: &mut [super::UserTile], home: &[plx_plex::plex::session::HomeUserRef]) {
    for tile in tiles {
        if let Some(h) = home.iter().find(|h| h.uuid == tile.uuid) {
            tile.title.clone_from(&h.title);
            if !tile.thumb.is_empty() {
                tile.thumb.clone_from(&h.thumb);
            }
        }
    }
}

/// **Lay a new sign-in beside the saved users** (the session owner's `SignedIn`, under FinNative):
/// `next` is the session the sign-in is about to save, `committed` the one saved before it. The
/// saved users are kept, the previous user is recorded from the saved server and token when it
/// has no record yet, the new user is added (or replaces its own record when it signed in again),
/// and it is the one seated.
pub(super) fn add_user(next: &mut plx_plex::plex::session::Session, committed: &owner::CredentialPatch) {
    let Some(added) = signed_in(&next.server, &next.sources) else { return };
    let mut users = committed.profiles.clone();
    if committed.account_token == ACCOUNT_MARKER {
        if let Some(previous) = signed_in(&committed.server, &committed.sources) {
            keep(&mut users, previous);
        }
    }
    keep(&mut users, added.clone());
    next.user = added.user;
    next.home_users = users.iter().map(|p| tile(p, &next.server.machine_id).to_ref()).collect();
    next.profiles = users;
}

/// **A Jellyfin sign-in's servers are the only ones live after it** (the session owner's registry
/// plan for a Login's install): *Add user* signs in beside the saved session, whose servers are
/// still registered, and they are retired as a profile switch retires them; otherwise the previous
/// user's server stays listed beside the new user's, answered by the one client the backend has.
pub(super) fn seat_signed_in_roster(registry: &mut [owner::RegistryPlan], op: owner::SessionOp) {
    if op != owner::SessionOp::Login {
        return;
    }
    for plan in registry {
        if let owner::RegistryPlan::Install { commit, .. } = plan {
            *commit = owner::RosterCommit::Switch;
        }
    }
}

/// **Seat a saved user** (`SessionWork::ProfileSwitch`): prove its server again at its saved base,
/// as [`endpoint_worker`] does, then ask the server whose the saved token is. `None` when the
/// picker went away first.
pub(super) fn switch_worker(
    session: &plx_plex::plex::session::Session,
    tile: &super::UserTile,
    ask: &PlaintextAsk,
    output: &dyn owner::ObservationSink,
) -> Option<super::ProfileSwitchOutcomeProgress> {
    if !output.live() {
        return None;
    }
    let outcome = switch_with(
        session,
        tile,
        |base| backend::curl_client(base, &session.client_id).probe().ok(),
        |id, origin| {
            vouch_again_when_stale(ask, || PlaintextAsk::capture(ACCOUNT_MARKER), id, origin,
                CredentialPolicy::build(), live_grant)
        },
        |base, token| backend::curl_client(base, &session.client_id).who_is(token),
    );
    output.live().then_some(outcome)
}

/// [`switch_worker`] with its IO passed in: `probe` and `grant` as [`reprove`] takes them, `who`
/// asks a base whose a token is.
fn switch_with(
    session: &plx_plex::plex::session::Session,
    tile: &super::UserTile,
    probe: impl FnOnce(&str) -> Option<PublicSystemInfo>,
    grant: impl FnOnce(&str, &Origin) -> Result<(), MintRefusal>,
    who: impl FnOnce(&str, &str) -> Result<Session, SignInError>,
) -> super::ProfileSwitchOutcomeProgress {
    use plx_platform::i18n::msg;
    let failed = |error: String| super::ProfileSwitchOutcomeProgress::Failed { error, pin_denied: false };
    let saved = session.profiles.iter().find(|p| p.uuid == tile.uuid).cloned()
        .or_else(|| signed_in(&session.server, &session.sources).filter(|p| p.uuid == tile.uuid));
    let Some(saved) = saved else {
        log("auth: switch -> no saved sign-in for this user");
        return failed(msg::browse_auth_sign_in_again(&tile.title));
    };
    let at = plx_plex::plex::session::Session { sources: saved.sources.clone(), ..Default::default() };
    if reprove(&at, &saved.server.machine_id, probe, grant).is_none() {
        return super::offline_switch_outcome(session, tile, None);
    }
    let user = match who(&backend::saved_base(&saved.server), &saved.user.token) {
        Ok(user) if normalize(&user.user_id) == saved.uuid => user,
        Ok(_) | Err(SignInError::Rejected) => {
            log("auth: switch -> the server no longer accepts this sign-in");
            return failed(msg::browse_auth_sign_in_again(&tile.title));
        }
        Err(SignInError::Unreachable) => return super::offline_switch_outcome(session, tile, None),
        Err(e) => {
            log(&format!("auth: switch -> failed ({e:?})"));
            return failed(msg::browse_auth_switch_failed().into());
        }
    };
    // The name and avatar as the server has them now, saved as a launch reads them.
    let record = backend::user_record(&user);
    let mut server = saved.server.clone();
    backend::save_user(&mut server.extensions, &record);
    let mut sources = saved.sources.clone();
    for source in sources.iter_mut().filter(|s| s.machine_id == server.machine_id) {
        backend::save_user(&mut source.extensions, &record);
    }
    let Some(mut cache) = signed_in(&server, &sources) else {
        return failed(msg::browse_auth_switch_failed().into());
    };
    cache.extensions = saved.extensions;
    log("auth: switch -> ok (jellyfin)");
    super::ProfileSwitchOutcomeProgress::Ready {
        delta: super::ProfileDelta { server, sources, user: cache.user.clone(), cache: Some(cache) },
        probes: Vec::new(),
    }
}

/// The user signed in on `server` (its saved [`backend::USER_KEY`] record and token) as a saved
/// sign-in; `None` for a server with no user saved beside it.
fn signed_in(server: &ServerRef, sources: &[SourceRef]) -> Option<ProfileCreds> {
    let saved = backend::SavedUser::read(&server.extensions).filter(|_| !server.token.is_empty())?;
    let thumb = saved.thumb();
    let uuid = saved.id;
    let user = UserRef {
        uuid: uuid.clone(),
        title: saved.name,
        thumb,
        token: server.token.clone(),
        // The marker, as a sign-in leaves it: what Settings and playback ask before reading the
        // user's own audio and subtitle preferences (see the module doc).
        plex_tv_token: Some(ACCOUNT_MARKER.into()),
        ..Default::default()
    };
    Some(ProfileCreds { uuid, user, server: server.clone(), sources: sources.to_vec(), ..Default::default() })
}

/// `user` in `users`: in place of its own earlier record, else after the others.
fn keep(users: &mut Vec<ProfileCreds>, user: ProfileCreds) {
    match users.iter_mut().find(|p| p.uuid == user.uuid) {
        Some(p) => *p = ProfileCreds { extensions: std::mem::take(&mut p.extensions), ..user },
        None => users.push(user),
    }
}

/// `user`'s picker tile: its avatar only when it is on `seated_server` (see the module doc).
fn tile(user: &ProfileCreds, seated_server: &str) -> super::UserTile {
    super::UserTile {
        id: 0,
        title: user.user.title.clone(),
        thumb: if user.server.machine_id == seated_server { user.user.thumb.clone() } else { String::new() },
        uuid: user.uuid.clone(),
        protected: false,
        admin: false,
    }
}

/// The server to sign in to: `FINNATIVE_SERVER` (a typed address, for the desktop simulator and
/// for a network that drops broadcasts), else the first server that answers on the LAN.
fn find_server() -> Option<String> {
    if let Some(s) = std::env::var("FINNATIVE_SERVER").ok().filter(|s| !s.trim().is_empty()) {
        return Some(normalize_base(&s));
    }
    for _ in 0..3 {
        if let Some(f) = plx_jellyfin::jellyfin::discovery::discover(Duration::from_secs(2)).into_iter().next() {
            return Some(normalize_base(&f.address));
        }
    }
    None
}


#[cfg(test)]
mod tests {
    use super::*;
    use plx_plex::plex::session::Session as Saved;

    const SERVER: &str = "0123456789abcdef0123456789abcdef";

    fn saved(origin: &str) -> Saved {
        let mut source = SourceRef { machine_id: SERVER.into(), origin_url: origin.into(), ..Default::default() };
        source.extensions.0.insert(backend::BASE_KEY.into(), format!("{origin}/jellyfin").into());
        Saved { sources: vec![source], ..Default::default() }
    }

    fn answers(id: &str) -> Option<PublicSystemInfo> {
        Some(PublicSystemInfo { id: id.into(), server_name: "Home".into() })
    }

    /// A grant refused as stale is asked for once more; every other answer is final.
    #[test]
    fn a_stale_grant_is_asked_for_again_once() {
        assert_eq!(settled(Err(MintRefusal::Stale), || Ok(())), Ok(()));
        assert_eq!(settled(Err(MintRefusal::Stale), || Err(MintRefusal::Stale)), Err(MintRefusal::Stale));
        assert_eq!(settled(Ok(()), || panic!("asked again after a grant")), Ok(()));
        for refusal in [MintRefusal::NotConsented, MintRefusal::NotPrivateLiteral, MintRefusal::NotPlaintext] {
            assert_eq!(settled(Err(refusal), || panic!("asked again after {refusal:?}")), Err(refusal));
        }
    }

    /// A saved server comes back only when its saved base answers as itself, and only once its
    /// plaintext origin is granted again.
    #[test]
    fn a_saved_server_is_reproved_at_its_base_before_it_is_granted() {
        let lan = "http://192.168.1.20:8096";
        let mut asked = String::new();
        let mut granted = None;
        let back = reprove(&saved(lan), SERVER, |base| { asked = base.into(); answers(SERVER) },
            |id, origin| { granted = Some((id.to_owned(), origin.base())); Ok(()) });
        assert_eq!(back.map(|s| s.machine_id), Some(SERVER.to_owned()));
        assert_eq!(asked, format!("{lan}/jellyfin"), "the whole saved base, path included");
        assert_eq!(granted, Some((SERVER.to_owned(), lan.to_owned())));

        // Another server at that address (a DHCP reshuffle, another network): not granted.
        let mut minted = false;
        assert!(reprove(&saved(lan), SERVER, |_| answers("fedcba9876543210fedcba9876543210"),
            |_, _| { minted = true; Ok(()) }).is_none());
        assert!(!minted, "never granted to a server that is not the saved one");
        // Nothing answered, or the grant refused: the record stays as it is.
        assert!(reprove(&saved(lan), SERVER, |_| None, |_, _| Ok(())).is_none());
        assert!(reprove(&saved(lan), SERVER, |_| answers(SERVER), |_, _| Err(MintRefusal::Stale)).is_none());
        assert!(reprove(&saved(lan), "another", |_| answers("another"), |_, _| Ok(())).is_none(), "not saved");
    }

    /// The grant is asked for only where the build's policy refuses the origin, and never over the
    /// person's own refusal.
    /// The boot's re-proof asks again, under a fresh capture, when its own was made before the
    /// network settled; a refusal that is not staleness is final and asks nothing.
    #[test]
    fn a_saved_servers_stale_grant_is_asked_for_again_under_a_fresh_capture() {
        let lan = Origin::http("192.168.1.20", 8096);
        let allowed = PlaintextAsk::undecided().with(SERVER, PlaintextChoice::Allowed);
        let minted = std::cell::Cell::new(0);
        let captured = std::cell::Cell::new(0);
        let fresh = || { captured.set(captured.get() + 1); PlaintextAsk::undecided().with(SERVER, PlaintextChoice::Allowed) };
        let stale_once = |_: &PlaintextAsk, _: &str, _: &Origin| {
            minted.set(minted.get() + 1);
            if minted.get() == 1 { Err(MintRefusal::Stale) } else { Ok(()) }
        };
        assert_eq!(vouch_again_when_stale(&allowed, fresh, SERVER, &lan, CredentialPolicy::HttpsOnly, stale_once), Ok(()));
        assert_eq!((minted.get(), captured.get()), (2, 1));
        let always = |_: &PlaintextAsk, _: &str, _: &Origin| Err(MintRefusal::Stale);
        assert_eq!(vouch_again_when_stale(&allowed, || allowed.clone(), SERVER, &lan, CredentialPolicy::HttpsOnly, always),
            Err(MintRefusal::Stale), "stale twice stays stale");
        let refusing = |_: &PlaintextAsk, _: &str, _: &Origin| Err(MintRefusal::NotPrivateLiteral);
        assert_eq!(vouch_again_when_stale(&allowed, || panic!("no second capture"), SERVER, &lan, CredentialPolicy::HttpsOnly, refusing),
            Err(MintRefusal::NotPrivateLiteral));
    }

    #[test]
    fn a_plaintext_server_is_granted_unless_the_person_turned_it_off() {
        let lan = Origin::http("192.168.1.20", 8096);
        let allowed = PlaintextAsk::undecided().with(SERVER, PlaintextChoice::Allowed);
        let minted = std::cell::Cell::new(0);
        let mint = |_: &PlaintextAsk, _: &str, _: &Origin| { minted.set(minted.get() + 1); Ok(()) };
        assert_eq!(vouch(&allowed, SERVER, &lan, CredentialPolicy::HttpsOnly, mint), Ok(()));
        assert_eq!(vouch(&allowed, SERVER, &lan, CredentialPolicy::AllowPlaintext, mint), Ok(()));
        let tls = Origin::parse("https://jellyfin.example.test").expect("an origin");
        assert_eq!(vouch(&allowed, SERVER, &tls, CredentialPolicy::HttpsOnly, mint), Ok(()));
        assert_eq!(minted.get(), 1, "only the plaintext origin under a store policy needs a grant");
        for refused in [PlaintextChoice::Revoked, PlaintextChoice::Declined] {
            let ask = PlaintextAsk::undecided().with(SERVER, refused);
            assert_eq!(vouch(&ask, SERVER, &lan, CredentialPolicy::HttpsOnly, mint),
                Err(MintRefusal::NotConsented));
        }
        assert_eq!(minted.get(), 1);
        let refusing = |_: &PlaintextAsk, _: &str, _: &Origin| Err(MintRefusal::NotPrivateLiteral);
        assert_eq!(vouch(&allowed, SERVER, &Origin::http("nas.local", 8096), CredentialPolicy::HttpsOnly, refusing),
            Err(MintRefusal::NotPrivateLiteral));
    }

    // ---- Several users ----

    /// A sign-in's install retires the servers the previous user left registered; nothing else's does.
    #[test]
    fn a_sign_in_installs_its_servers_in_place_of_the_previous_users() {
        let install = || vec![owner::RegistryPlan::Install { sources: Vec::new(), primary: Some(0),
            commit: owner::RosterCommit::Merge }];
        let commit = |plan: &[owner::RegistryPlan]| match &plan[0] {
            owner::RegistryPlan::Install { commit, .. } => *commit,
            _ => unreachable!(),
        };
        let mut login = install();
        seat_signed_in_roster(&mut login, owner::SessionOp::Login);
        assert!(commit(&login) == owner::RosterCommit::Switch);
        let mut other = install();
        seat_signed_in_roster(&mut other, owner::SessionOp::Picker);
        assert!(commit(&other) == owner::RosterCommit::Merge);
    }

    const OTHER: &str = "fedcba9876543210fedcba9876543210";

    /// A server record with `user` signed in on it with `token`, as a sign-in saves it.
    fn signed_on(machine_id: &str, user: &str, token: &str) -> (ServerRef, Vec<SourceRef>) {
        let mut source = SourceRef { machine_id: machine_id.into(), origin_url: "http://192.168.1.20:8096".into(),
            token: token.into(), ..Default::default() };
        source.extensions.0.insert(backend::USER_KEY.into(),
            serde_json::json!({ "id": user, "name": format!("name {user}"), "avatar": "tag" }));
        (super::super::server_ref(&source), vec![source])
    }

    fn session_on(machine_id: &str, user: &str, token: &str) -> Saved {
        let (server, sources) = signed_on(machine_id, user, token);
        Saved { account_token: ACCOUNT_MARKER.into(), server, sources, ..Default::default() }
    }

    fn uuids(s: &Saved) -> Vec<String> {
        s.profiles.iter().map(|p| p.uuid.clone()).collect()
    }

    /// Adding a user to an install signed in as one user keeps that user, whose record the saved
    /// server and token make, and seats the new one, on its own server.
    #[test]
    fn an_added_user_is_seated_beside_the_one_signed_in_before() {
        let committed = owner::CredentialPatch::of(&session_on(SERVER, "a1", "tok-a"));
        let mut next = session_on(OTHER, "b2", "tok-b");
        add_user(&mut next, &committed);
        assert_eq!(uuids(&next), ["a1", "b2"]);
        assert_eq!((next.user.uuid.as_str(), next.user.token.as_str()), ("b2", "tok-b"));
        assert_eq!(next.user.plex_tv_token.as_deref(), Some(ACCOUNT_MARKER));
        let a = &next.profiles[0];
        assert_eq!((a.user.token.as_str(), a.server.machine_id.as_str()), ("tok-a", SERVER), "its own server and token");
        assert!(next.cached_profile("a1").is_some() && next.cached_profile("b2").is_some(), "both seat offline");
        let tiles: Vec<(String, String)> = next.home_users.iter().map(|u| (u.uuid.clone(), u.thumb.clone())).collect();
        assert_eq!(tiles, [("a1".to_owned(), String::new()), ("b2".to_owned(), "/Users/b2/Images/Primary?quality=90&tag=tag".to_owned())],
            "no avatar for the user on another server");
        assert!(next.boot_shows_picker(false, false), "two users: the boot asks who is watching");
    }

    /// Signing in again as a saved user replaces its record where it was; a sign-in after a sign-out
    /// has nobody saved beside it.
    #[test]
    fn a_user_who_signs_in_again_keeps_its_place() {
        let mut two = session_on(OTHER, "b2", "tok-b");
        add_user(&mut two, &owner::CredentialPatch::of(&session_on(SERVER, "a1", "tok-a")));
        let mut again = session_on(SERVER, "a1", "tok-a2");
        add_user(&mut again, &owner::CredentialPatch::of(&two));
        assert_eq!(uuids(&again), ["a1", "b2"]);
        assert_eq!(again.profiles[0].user.token, "tok-a2");
        assert_eq!(again.user.uuid, "a1");

        let mut fresh = session_on(SERVER, "c3", "tok-c");
        add_user(&mut fresh, &owner::CredentialPatch::of(&Saved::default()));
        assert_eq!(uuids(&fresh), ["c3"]);
        assert!(!fresh.boot_shows_picker(false, false));
    }

    /// The picker lists the saved users, the seated one included when it has no record yet.
    #[test]
    fn the_roster_is_the_saved_users() {
        let single = session_on(SERVER, "a1", "tok-a");
        assert_eq!(roster(&single).iter().map(|t| t.uuid.as_str()).collect::<Vec<_>>(), ["a1"]);
        let mut two = session_on(OTHER, "b2", "tok-b");
        add_user(&mut two, &owner::CredentialPatch::of(&single));
        let tiles = roster(&two);
        assert_eq!(tiles.iter().map(|t| (t.uuid.as_str(), t.thumb.is_empty(), t.protected)).collect::<Vec<_>>(),
            [("a1", true, false), ("b2", false, false)]);
        // A seat moves its user's record to the end of the cache; the picker keeps its order.
        two.profiles.rotate_left(1);
        assert_eq!(roster(&two).iter().map(|t| t.uuid.as_str()).collect::<Vec<_>>(), ["a1", "b2"]);
    }

    fn two_users() -> Saved {
        let mut two = session_on(OTHER, "b2", "tok-b");
        add_user(&mut two, &owner::CredentialPatch::of(&session_on(SERVER, "a1", "tok-a")));
        two
    }

    fn tile_of(s: &Saved, uuid: &str) -> super::super::UserTile {
        roster(s).into_iter().find(|t| t.uuid == uuid).expect("a tile")
    }

    fn me(user: &str, token: &str) -> Result<Session, SignInError> {
        Ok(Session { user_id: user.into(), user_name: "renamed".into(), avatar_tag: None, token: token.into(),
            server_id: SERVER.into() })
    }

    /// A pick proves the user's own server, asks it with the user's own token, and seats the user
    /// as the server has it now.
    #[test]
    fn a_pick_seats_the_user_on_its_own_server() {
        let two = two_users();
        let asked = std::cell::RefCell::new(Vec::new());
        let outcome = switch_with(&two, &tile_of(&two, "a1"), |base| { asked.borrow_mut().push(base.to_owned()); answers(SERVER) },
            |_, _| Ok(()), |base, token| { asked.borrow_mut().push(format!("{base} {token}")); me("a1", token) });
        assert_eq!(asked.into_inner(), ["http://192.168.1.20:8096", "http://192.168.1.20:8096 tok-a"]);
        let super::super::ProfileSwitchOutcomeProgress::Ready { delta, .. } = outcome else { panic!("seated") };
        assert_eq!((delta.user.uuid.as_str(), delta.user.title.as_str(), delta.user.token.as_str()), ("a1", "renamed", "tok-a"));
        assert_eq!(delta.server.machine_id, SERVER);
        assert_eq!(delta.server.token, "tok-a", "the backend resumes from the seated server's token");
        assert_eq!(delta.server.extensions.0.get(backend::USER_KEY).and_then(|r| r.get("name")).and_then(|n| n.as_str()), Some("renamed"));
        assert!(delta.cache.is_some_and(|c| c.uuid == "a1"));
    }

    /// A token the server stopped accepting says to sign in again; a server that does not answer
    /// seats the user from what was saved.
    #[test]
    fn a_refused_token_asks_to_sign_in_again_and_no_answer_seats_offline() {
        use super::super::ProfileSwitchOutcomeProgress::{Failed, Ready};
        let two = two_users();
        let a = tile_of(&two, "a1");
        let refused = switch_with(&two, &a, |_| answers(SERVER), |_, _| Ok(()), |_, _| Err(SignInError::Rejected));
        assert!(matches!(&refused, Failed { error, pin_denied: false } if error.contains(&a.title)));
        let offline = switch_with(&two, &a, |_| answers(SERVER), |_, _| Ok(()), |_, _| Err(SignInError::Unreachable));
        assert!(matches!(offline, Ready { delta, .. } if delta.user.uuid == "a1" && delta.user.token == "tok-a"));
        let silent = switch_with(&two, &a, |_| None, |_, _| Ok(()), |_, _| panic!("no token goes out unproved"));
        assert!(matches!(silent, Ready { delta, .. } if delta.user.uuid == "a1"));
        let ungranted = switch_with(&two, &a, |_| answers(SERVER), |_, _| Err(MintRefusal::NotConsented),
            |_, _| panic!("no token goes out ungranted"));
        assert!(matches!(ungranted, Ready { .. }), "seated from the cache, as at boot");
        let another = switch_with(&two, &a, |_| answers(SERVER), |_, _| Ok(()), |_, token| me("zz", token));
        assert!(matches!(another, Failed { .. }), "the token is another user's now");
        let stranger = super::super::UserTile { uuid: "nobody".into(), title: "x".into(), ..Default::default() };
        assert!(matches!(switch_with(&two, &stranger, |_| panic!(), |_, _| panic!(), |_, _| panic!()), Failed { .. }));
    }

}
