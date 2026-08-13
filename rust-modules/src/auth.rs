//! Login + boot orchestration for the plex.tv account flow. Owns the flow state machine the Login
//! and Profiles screens render, drives the background network threads (pin create/poll → server
//! discovery → home-users → user switch), and hands the resolved credentials to the main loop via
//! [`take_ready`], which installs them on the main thread (never the worker) and enters Home.
//!
//! Offline-first: this flow only runs when there's no usable stored session — [`crate::plex::session`]
//! + the boot gate in `app.rs` short-circuit straight to the LAN server when we already have creds.
//! All network happens on spawned threads; the UI only reads snapshots through the accessors here.
//! Tokens live in the working [`Session`] and are never logged.
#![allow(dead_code)]
use crate::plex::account::{AccountClient, HomeUser, Resource};
use crate::plex::probe::{self, Candidate, Outcome, ProbePlan, Scheme};
use crate::plex::session::{self, ServerRef, Session, SourceRef, UserRef};
use std::sync::Mutex;

/// Which stage the flow is in — the Login/Profiles screens switch on this each frame.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
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
}

/// One "who's watching" tile.
#[derive(Clone, Default)]
pub struct UserTile {
    pub title: String,
    pub thumb: String,
    pub uuid: String,
    pub protected: bool, // needs a PIN
    pub admin: bool,
}
impl UserTile {
    fn of(u: &HomeUser) -> UserTile {
        UserTile {
            title: u.title.clone(),
            thumb: u.thumb.clone(),
            uuid: u.uuid.clone(),
            protected: u.protected,
            admin: u.admin,
        }
    }
    fn of_ref(u: &session::HomeUserRef) -> UserTile {
        UserTile {
            title: u.title.clone(),
            thumb: u.thumb.clone(),
            uuid: u.uuid.clone(),
            protected: u.protected,
            admin: u.admin,
        }
    }
    fn to_ref(&self) -> session::HomeUserRef {
        session::HomeUserRef {
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
    pub host: String,
    pub port: i32,
    pub token: String,
}

#[derive(Default)]
struct Ctl {
    phase: Phase,
    pin_id: i64,
    pin_code: String,
    qr_png: Vec<u8>, // Plex's server-rendered QR PNG bytes (decoded + shown by the login screen)
    users: Vec<UserTile>,
    error: String,
    // the last switch failure blames the submitted PIN (the 401/keypad case) — the PIN pad flashes
    // its dots red for this one and shows the picker's error banner for everything else ("no
    // access to this server", offline), which a red wrong-PIN flash would misrepresent.
    pin_denied: bool,
    session: Session,
    apply_pending: bool,
}

static CTL: Mutex<Option<Ctl>> = Mutex::new(None);

/// Append a line to the shared on-device event log (never a token — only ids/counts/status).
use crate::log;

fn with_ctl<R>(f: impl FnOnce(&mut Ctl) -> R) -> R {
    let mut g = CTL.lock().unwrap_or_else(|e| e.into_inner());
    let c = g.get_or_insert_with(Ctl::default);
    f(c)
}

// ---- accessors the UI reads each frame ----

pub fn phase() -> Phase {
    with_ctl(|c| c.phase)
}
pub fn pin_code() -> String {
    with_ctl(|c| c.pin_code.clone())
}
/// Plex's QR PNG bytes for the current pin (empty until fetched) — the login screen decodes + shows.
pub fn qr_png() -> Vec<u8> {
    with_ctl(|c| c.qr_png.clone())
}
pub fn error() -> String {
    with_ctl(|c| c.error.clone())
}
/// Did the last profile-switch failure blame the submitted PIN? Drives the PIN pad's red-flash
/// (vs closing so the picker's error banner can show a non-PIN failure).
pub fn pin_denied() -> bool {
    with_ctl(|c| c.pin_denied)
}
pub fn users() -> Vec<UserTile> {
    with_ctl(|c| c.users.clone())
}

// ---- flow control ----

/// Begin the QR login: reset state, load the persisted `client_id`, and kick off the pin thread.
pub fn start_login() {
    with_ctl(|c| {
        *c = Ctl { phase: Phase::Creating, session: session::load(), ..Ctl::default() };
    });
    if !crate::task::spawn_small("login", login_thread) {
        // Phase::Creating is a spinner with a worker behind it. Without the worker it never ends,
        // and the login screen has no other way out — Error at least offers the retry.
        set_error("Couldn't start sign-in. Try again.");
    }
}

/// Retry after an [`Phase::Error`] — same as a fresh login.
pub fn retry() {
    start_login();
}

/// Back out of the flow (BACK on the Login or Profiles screen) → **resume the stored session** and
/// let the main loop take us Home. Returns whether there was anything to back out to.
///
/// It deliberately does NOT drop to [`Phase::Idle`]. Nothing routes on Idle: `app.rs`'s
/// phase→route follower runs every frame while the route is Login/Profiles and maps every phase it
/// doesn't recognise back to `Route::Login`, so an Idle cancel would park the user on the sign-in
/// screen showing "Connecting to Plex…" forever — strictly worse than no escape hatch at all.
///
/// Instead it re-arms the resolved-credentials handoff with the session already on disk, which is
/// bit-for-bit the state [`switch_thread`]'s "already-active profile" fast path produces:
/// [`take_ready`] picks it up on the next frame, installs the stored server + token on the main
/// thread and enters Home. So BACK means "carry on as the profile I'm already signed in as" —
/// identical to picking your own tile in the picker, which is the only sensible thing behind these
/// two screens.
///
/// **False (and no state change) when there is no usable stored session** — a first-ever sign-in,
/// or the picker straight after a sign-out. There is genuinely nothing behind those, so the callers
/// swallow BACK rather than stranding the user on a screen with no server.
pub fn cancel() -> bool {
    let sess = session::load();
    if !sess.can_go_local() {
        return false;
    }
    log("auth: flow cancelled — resuming the stored session");
    with_ctl(|c| *c = Ctl { phase: Phase::Ready, session: sess, apply_pending: true, ..Ctl::default() });
    true
}

/// Choose a profile with no PIN (or after the keypad, via [`submit_pin`]).
pub fn select_profile(index: usize) {
    switch_thread(index, None);
}

/// Submit the entered PIN for a protected profile.
pub fn submit_pin(index: usize, pin: &str) {
    switch_thread(index, Some(pin.to_owned()));
}

/// Main-loop hook: when the flow has resolved credentials, return them ONCE (and persist the
/// session) so the caller installs them on the main thread. `None` on every other frame.
///
/// The returned creds are the PRIMARY server's, unchanged. The rest of the roster is registered
/// here too — every path that resolves credentials passes through this one function (sign-in, a
/// profile pick, and `cancel`'s resume of the stored session), and a share that is not in the
/// registry is a share nothing can browse.
pub fn take_ready() -> Option<ReadyCreds> {
    let (sources, creds) = with_ctl(|c| {
        if c.phase == Phase::Ready && c.apply_pending {
            c.apply_pending = false;
            session::save(&c.session);
            session::set_current(Some(c.session.user.clone())); // drives the Home profile chip
            Some((
                c.session.sources.clone(),
                ReadyCreds {
                    host: c.session.server.address.clone(),
                    port: c.session.server.port as i32,
                    token: c.session.pms_token().to_owned(),
                },
            ))
        } else {
            None
        }
    })?;
    // Outside the CTL lock: registering touches the server registry (and, on a cold slot, reads
    // the session file for the device id), and nothing here needs the flow state held while it
    // does. `None` for the primary — the caller's own `plex::install` of these creds is what
    // retargets `current`, and an owned entry registers first regardless.
    install_roster(&sources, None);
    Some(creds)
}

/// Open the "who's watching" picker: the boot gate (picker-at-start) and the Home profile menu's
/// "Change profile" both land here. Seeds the roster from the persisted session (instant + offline)
/// and refreshes it from plex.tv in the background — a successful refresh is persisted, a failed
/// one keeps the cache. Only an *empty* roster that also fails to fetch becomes an error; being
/// signed out is an error immediately (an empty picker is a dead end). The caller routes on phase.
pub fn start_switch() {
    let sess = session::load();
    if sess.account_token.is_empty() {
        return set_error("You're signed out — sign in to use profiles.");
    }
    // The boot picker reaches this before any profile is chosen, so it is the earliest point on
    // the resumed-session path where the stored roster can go back into the registry. Idempotent,
    // and it does not touch `current` — a "Change profile" from Home lands here too, by which time
    // everything is registered already and this is a no-op.
    install_stored_roster(&sess);
    with_ctl(|c| {
        c.error.clear();
        if c.users.is_empty() {
            c.users = sess.home_users.iter().map(UserTile::of_ref).collect();
        }
        c.session = sess;
        c.phase = Phase::Profiles;
    });
    // best-effort: a refused spawn just leaves the persisted roster on screen (already installed
    // above), so there is no flag to release and nothing to tell the user
    let _ = crate::task::spawn_small("roster", || {
        let (cid, tok) = with_ctl(|c| (c.session.client_id.clone(), c.session.account_token.clone()));
        let ac = AccountClient::new(&cid, Some(&tok));
        match ac.home_users() {
            Some(us) if !us.is_empty() => {
                let users: Vec<UserTile> = us.iter().map(UserTile::of).collect();
                log(&format!("auth: roster refreshed n={}", users.len()));
                let persist = with_ctl(|c| {
                    c.session.home_users = users.iter().map(UserTile::to_ref).collect();
                    c.users = users;
                    c.session.clone()
                });
                session::save(&persist);
            }
            _ => {
                log("auth: roster refresh failed — keeping cached roster");
                if with_ctl(|c| c.users.is_empty() && c.phase == Phase::Profiles) {
                    set_error("Couldn't load profiles — check the connection.");
                }
            }
        }
    });
}

/// Sign out: forget the persisted session + roster and start a fresh login. The caller routes to
/// [`Phase::Login`]-era screens (Route::Login).
pub fn sign_out() {
    session::clear();
    session::set_current(None);
    with_ctl(|c| *c = Ctl::default());
    start_login();
}

// ---- worker threads ----

fn login_thread() {
    let cid = with_ctl(|c| c.session.client_id.clone());
    let ac = AccountClient::new(&cid, None);

    // 1) create a pin
    let pin = match ac.create_pin() {
        Some(p) if p.id != 0 && !p.code.is_empty() => p,
        _ => return set_error("Couldn't reach Plex — check the connection."),
    };
    // Neither the id nor the code may be logged. `GET /api/v2/pins/{id}` is what RETURNS the
    // account token once the user authorizes (plex/account.rs `poll_pin`), so the id is a handle
    // that redeems a credential, and the code is what authorizes it — and this file is the one we
    // ask users to send us when something goes wrong. Log that we got here, not what we got.
    log("auth: pin created (waiting for authorization)");
    // fetch the server-rendered QR PNG (the exact QR the official apps display); public, no token.
    let qr_url = if pin.qr.is_empty() {
        format!("https://plex.tv/api/v2/pins/qr/{}", pin.code)
    } else {
        pin.qr.clone()
    };
    let qr_png = crate::net::https_get(&qr_url, &[]).filter(|r| r.ok()).map(|r| r.body).unwrap_or_default();
    log(&format!("auth: qr png {} bytes", qr_png.len()));
    with_ctl(|c| {
        c.pin_id = pin.id;
        c.pin_code = pin.code.clone();
        c.qr_png = qr_png;
        c.phase = Phase::Waiting;
    });

    // 2) poll until authorized (or the pin expires / the user cancels)
    let token = match poll_for_token(&ac, pin.id, pin.expires_in) {
        Some(t) => t,
        None => return, // cancelled (silent) or timed out (poll set the error)
    };
    log("auth: authorized — discovering server");

    // 3) discover the LAN server. GUARDED, because [`cancel`] is just a phase flip and the
    // `poll_pin` above is a network round trip: a BACK pressed while that request was in flight
    // would otherwise be undone a second later, and — far worse — the `plex::install` below would
    // swap the PMS client out from under the Home the user had already gone back to. Anything but
    // Waiting means this worker is no longer the live flow, so it drops its token and exits.
    let still_ours = with_ctl(|c| {
        if c.phase != Phase::Waiting {
            return false;
        }
        c.session.account_token = token.clone();
        c.phase = Phase::Discovering;
        true
    });
    if !still_ours {
        return log("auth: sign-in was cancelled while the pin poll was in flight — token dropped");
    }
    let ac = AccountClient::new(&cid, Some(&token));
    // The failure copy is per outcome, and it used to be one line — "No local Plex server found on
    // this network." — for every one of them. That sentence was the discovery POLICY talking: a
    // server reached over the internet was a failure by construction, so the message named the LAN.
    // It now describes what actually happened, and none of the three sends the user to the wrong
    // place: a token refusal is not a router problem, and an account with no server is not an
    // outage.
    match discover_and_store(&ac) {
        Discovery::Ok => {}
        Discovery::NoServers => return set_error("This Plex account has no server yet."),
        // "A Plex server", not "your": `refused` is raised by ANY server's addresses all answering
        // 401, which includes a friend's share while our own was merely silent. Naming the wrong
        // machine is the whole failure mode this per-outcome copy exists to avoid.
        Discovery::Refused => return set_error("A Plex server refused the connection — check its network access settings."),
        Discovery::Silent => return set_error("Couldn't reach any Plex server — check the connection."),
        // Silent on purpose: the user pressed BACK and is already on the screen `cancel` resumed.
        Discovery::Cancelled => return log("auth: sign-in was cancelled while discovery was dialling"),
    }
    // Install the PMS client now (server/owner token) so the who's-watching avatars can proxy
    // through the server's photo transcoder. The per-user token is swapped in on profile pick.
    let (addr, port, stok) =
        with_ctl(|c| (c.session.server.address.clone(), c.session.server.port, c.session.server.token.clone()));
    crate::plex::install(&addr, port as i32, &stok);
    log(&format!("auth: PMS client installed {addr}:{port}"));

    // 4) Plex Home roster → who's-watching, or straight in if there's a single user. The roster is
    // kept on the session so it persists with the creds — the boot picker and every later
    // "Change profile" render from it instantly, online or not.
    let users: Vec<UserTile> = ac.home_users().unwrap_or_default().iter().map(UserTile::of).collect();
    log(&format!("auth: home users n={}", users.len()));
    with_ctl(|c| c.session.home_users = users.iter().map(UserTile::to_ref).collect());
    // Persist NOW — the account token + server + roster are durable the moment they exist.
    // Waiting for take_ready() (a completed profile pick) meant abandoning the app at the
    // picker lost the whole sign-in; next boot resumes at the picker instead.
    let snap = with_ctl(|c| c.session.clone());
    session::save(&snap);
    if users.len() > 1 {
        log("auth: showing who's-watching");
        with_ctl(|c| {
            c.users = users;
            c.phase = Phase::Profiles;
        });
    } else {
        // no Plex Home (or a single user): use the owner's server token as-is.
        log("auth: single user — ready, entering Home");
        with_ctl(|c| {
            c.phase = Phase::Ready;
            c.apply_pending = true;
        });
    }
}

/// Poll `/pins/{id}` every 2s until `auth_token` appears, the pin expires, or the flow leaves
/// [`Phase::Waiting`] (user pressed BACK).
fn poll_for_token(ac: &AccountClient, id: i64, expires_in: i64) -> Option<String> {
    let iters = (expires_in.max(60) / 2).min(900); // cap ~30 min
    for _ in 0..iters {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if with_ctl(|c| c.phase != Phase::Waiting) {
            return None; // cancelled
        }
        if let Some(p) = ac.poll_pin(id) {
            if let Some(t) = p.auth_token {
                if !t.is_empty() {
                    return Some(t);
                }
            }
        }
    }
    set_error("Login timed out — try again.");
    None
}

// ---- server discovery ----
//
// **Every server the account can reach, not the first one that looks local.** What this replaced
// filtered both of its passes on `c.local && !c.relay`, kept exactly one server, and threw the rest
// of the account away. Against a real share that filter is worse than useless: `Connection.local`
// means "this address is RFC1918", not "you are on that LAN", so it selected the OWNER's
// `10.9.x.x` — 8 s of timeout from here, and the *worse* outcome is that it succeeds against
// somebody else's box at that address on our own LAN (`docs/shared-servers.md` §2a).
//
// So the shape is: ranked candidates from `plex::probe` (pure policy — it is what drops that
// address), then dial them in order and **verify identity on the answer** before believing it.

/// How far one server got. Only [`Reach::At`] is a server we can use; the other two are the
/// distinction `probe.rs`'s module doc refuses to let a caller collapse, because they send the
/// user to two different places.
enum Reach {
    /// This address answered `/identity` **as the server we asked for**.
    At(Candidate),
    /// A 401. A TOKEN problem, not a reachability one: the `accessToken` is per (user, server) and
    /// carries the sharing grant, so every other address of this server answers identically and
    /// trying them buys nothing. Reporting "can't reach friend-nas" here would send the user to look
    /// at their friend's router for a problem that lives in `/api/v2/resources`.
    Refused,
    /// Nothing answered as this server.
    No,
}

/// What discovery concluded. Three outcomes rather than a bool, because "this account owns no
/// server", "your servers are silent" and "a server answered and refused us" are three different
/// things to tell a user, and only the middle one is about the network.
enum Discovery {
    Ok,
    /// The user left the flow while this was dialling — nothing was registered, nothing stored,
    /// and nothing must be SAID either: an error banner would land on a screen they had already
    /// moved on from.
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

/// Can this app's transport dial that candidate **today**? `stream.rs` speaks plain HTTP to a
/// dotted quad: no TLS, and no name resolution of any kind (`stream.rs`'s `http_open` builds the
/// `sockaddr_in` by hand from four decimal octets). An https `plex.direct` origin and the owner's
/// custom hostname are therefore not "unreachable" — they are unspoken, and saying otherwise would
/// blame a server for a gap in this client. They come back when the curl control plane lands
/// (`docs/shared-servers.md` §5 step 6), which is why `probe::candidates` already emits them.
fn dialable(c: &Candidate) -> bool {
    c.scheme == Scheme::Http && is_ipv4_literal(&c.address)
}

/// Four decimal octets and nothing else — the exact address shape `stream.rs` can turn into a
/// `sockaddr_in`. Anything else (a hostname, a v6 literal, a five-part typo) would be handed to
/// `http_open` only to be rejected there after a `CString` round trip.
fn is_ipv4_literal(a: &str) -> bool {
    let mut parts = 0;
    for p in a.split('.') {
        if p.is_empty() || p.len() > 3 || !p.bytes().all(|b| b.is_ascii_digit()) || p.parse::<u8>().is_err() {
            return false;
        }
        parts += 1;
    }
    parts == 4
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
    let start = rest.iter().position(|b| !matches!(b, b'"' | b':' | b'=' | b' ' | b'\t' | b'\r' | b'\n'))?;
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

/// One unauthenticated `GET http://host:port/identity`, as (status, body).
///
/// Uses the raw stream primitives rather than `stream::http_get` because the STATUS is half the
/// answer: `http_get` folds every non-2xx into `None`, which is precisely the collapse of 401 into
/// "unreachable" that this module exists to avoid. `http_open` already closed the socket on a
/// non-2xx (and `http_close` is a no-op the second time, by `take_fd`'s swap), so the status
/// survives on the stream while the body does not.
fn get_identity(host: &str, port: i32) -> (i32, Vec<u8>) {
    let (Ok(h), Ok(p)) = (std::ffi::CString::new(host), std::ffi::CString::new(IDENTITY)) else {
        return (0, Vec::new());
    };
    let accept = c"Accept: application/json\r\n";
    let mut hs = crate::stream::http_stream_boxed();
    let opened = crate::stream::http_open(&mut *hs, h.as_ptr(), port, p.as_ptr(), accept.as_ptr(), "GET");
    let status = crate::stream::hs_status(&*hs);
    let mut body = Vec::new();
    if opened == 0 {
        let mut chunk = vec![0u8; 8192];
        loop {
            let n = crate::stream::http_read(&mut *hs, chunk.as_mut_ptr(), chunk.len() as i32);
            if n <= 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n as usize]);
            // `/identity` is one empty MediaContainer. Anything past this is not an answer to the
            // question, and a probe must not be a way to make us read an unbounded body.
            if body.len() >= 64 * 1024 {
                break;
            }
        }
    }
    crate::stream::http_close(&mut *hs);
    (status, body)
}

/// Dial one server's candidates in rank order, stopping at the first that answers **as it**.
///
/// `dial` is injected so the whole acceptance policy — identity verified before a connection is
/// accepted, a wrong machine discarded rather than retried, a 401 ending the server instead of the
/// candidate — is host-testable without a socket.
///
/// Sequential, not parallel. `stream.rs` bounds a handshake at 2 s (`CONNECT_TIMEOUT_MS`), the
/// dialable list is short (policy has already dropped the owner's LAN address and this client
/// cannot speak to the https/hostname ones), and this runs on the login worker behind a spinner —
/// so racing would buy a couple of seconds at the price of N threads that each have to capture
/// their inputs at the spawn site. It is the trade `docs/shared-servers.md` §5 step 4 leaves open.
///
/// **A 401 does not end the server, it ends the candidate.** The probe is unauthenticated, so a 401
/// is not this server refusing our credential — it is whatever is listening at that address refusing
/// an anonymous request, and that need not be the server at all. Abandoning the whole server on one
/// would mean this: DHCP moves the PMS, a NAS answers 401 at its old address, and discovery drops
/// the user's own server without trying the second address plex.tv advertised for it. So it is
/// carried like [`Outcome::WrongServer`] — next candidate — and only reported as
/// [`Reach::Refused`] when EVERY address that could be dialled answered 401, which is the shape
/// that really does mean "reaching this server is not the problem".
fn probe_server(plan: &ProbePlan, dial: &dyn Fn(&str, i32) -> (i32, Vec<u8>)) -> Reach {
    let (mut tried, mut refusals) = (0, 0);
    for c in plan.candidates.iter().filter(|c| dialable(c)) {
        tried += 1;
        let (status, body) = dial(&c.address, c.port as i32);
        match classify(status, &body, &plan.machine_id) {
            Outcome::Reachable => return Reach::At(c.clone()),
            Outcome::Unauthorized => {
                refusals += 1;
                log(&format!("auth: '{}' — {}:{} refused an unauthenticated request (401)", plan.name, c.address, c.port));
            }
            Outcome::WrongServer => {
                // Rule 1, live: something answered and it is not this server. Discarded, never
                // retried — and never registered, which is the point of verifying at all.
                log(&format!("auth: '{}' — {}:{} answered as a DIFFERENT machine", plan.name, c.address, c.port));
            }
            Outcome::Unreachable => {}
        }
    }
    let skipped = plan.candidates.len() - tried;
    log(&format!("auth: '{}' did not answer ({tried} address(es) tried, {skipped} not dialable)", plan.name));
    if tried > 0 && refusals == tried {
        Reach::Refused
    } else {
        Reach::No
    }
}

/// Has the login flow moved on from [`Phase::Discovering`] — i.e. is this worker no longer the live
/// flow? [`cancel`] is a phase flip and nothing more, so this is the only way a worker holding a
/// socket can find out that BACK was pressed and the stored session has already been resumed.
fn flow_left_discovery() -> bool {
    with_ctl(|c| c.phase != Phase::Discovering)
}

/// What probing a whole `/api/v2/resources` response came to.
enum Resolved {
    /// The response named no server at all — nothing was dialled, and this is a fact about the
    /// account rather than about the network.
    NoServers,
    /// Servers were probed and none was accepted. `refused` distinguishes "a server's every
    /// dialable address answered 401" from "silence" — two different things to tell the user.
    None { refused: bool },
    /// The flow moved on (BACK) while this was dialling. Not an outcome to report: nothing may be
    /// registered, stored or said, because the user is already somewhere else.
    Cancelled,
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
/// `abort` is polled between servers. Discovery is no longer a filter over a response — it holds
/// sockets, and `stream.rs` will wait 2 s on a handshake and 15 s on a peer that connects and then
/// says nothing. That is a long time for the user to be looking at a spinner they can leave, so the
/// worker has to be able to notice that it is no longer the live flow.
fn resolve_roster(
    resources: &[Resource],
    dial: &dyn Fn(&str, i32) -> (i32, Vec<u8>),
    abort: &dyn Fn() -> bool,
) -> Resolved {
    let mut servers: Vec<&Resource> = resources.iter().filter(|r| r.is_server()).collect();
    if servers.is_empty() {
        return Resolved::NoServers;
    }
    // Ours first — it is what becomes `current`, what Home is built from, and the one whose silence
    // the user can actually do something about. `sort_by_key` is stable, so plex.tv's own order
    // survives inside each group.
    servers.sort_by_key(|r| !r.owned);

    let mut found: Vec<SourceRef> = Vec::new();
    let mut refused = false;
    for r in servers {
        if abort() {
            return Resolved::Cancelled;
        }
        let plan = probe::plan(r);
        match probe_server(&plan, dial) {
            Reach::At(c) => {
                let s = SourceRef {
                    machine_id: plan.machine_id.clone(),
                    name: plan.name.clone(),
                    shared_by: plan.source_title.clone().unwrap_or_default(),
                    owned: plan.owned,
                    // The address that ANSWERED, never the first advertised — and it got here only
                    // by being dialable, which is what keeps a v6 literal or a hostname out of the
                    // session file.
                    address: c.address,
                    port: c.port,
                    // That server's OWN grant. Our own server's token gets a 401 from a share, so
                    // there is no such thing as one token for the roster.
                    token: plan.token.clone(),
                };
                log(&format!("auth: reached {}", s.describe()));
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

/// Discover **every** server this identity can use — ours and each share — and store the roster.
///
/// Each resource that `provides` a server is turned into ranked candidates by `plex::probe`, dialled
/// in order, and accepted only when the answer's `machineIdentifier` matches. Each one that answers
/// is registered with the [server registry](crate::plex::register) under its **real machine id** and
/// its **own** per-(user, server) `accessToken` — a share is a separate authority and answers 401 to
/// our own server's token. Our own server stays `current`: a share is browsable, never the default.
///
/// The primary [`ServerRef`] is written exactly as before, so a single-server account produces the
/// same session file it always did (plus a one-entry roster beside it).
fn discover_and_store(ac: &AccountClient) -> Discovery {
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
    let found = match resolve_roster(&resources, &get_identity, &flow_left_discovery) {
        Resolved::NoServers => return Discovery::NoServers,
        Resolved::None { refused: true } => return Discovery::Refused,
        Resolved::None { refused: false } => return Discovery::Silent,
        Resolved::Cancelled => return Discovery::Cancelled,
        Resolved::Reached(f) => f,
    };

    // LAST CHANCE TO STOP, and the one that matters most: everything below is global. The guard
    // above `discover_and_store` is checked before any of this runs, and dialling can now take
    // seconds — long enough for BACK to have resumed the stored session, installed it and routed
    // the user to Home. Registering here would then retarget `client()` out from under that Home,
    // and the session write would clobber the session `cancel` had just restored.
    if flow_left_discovery() {
        return Discovery::Cancelled;
    }

    let primary = primary_index(&found);
    log(&format!("auth: {} server(s) reached, primary '{}'", found.len(), found[primary].name));
    install_roster(&found, Some(primary));
    let p = &found[primary];
    let server = ServerRef {
        name: p.name.clone(),
        machine_id: p.machine_id.clone(),
        address: p.address.clone(),
        port: if p.port != 0 { p.port } else { 32400 },
        token: p.token.clone(),
    };
    with_ctl(|c| {
        c.session.server = server;
        c.session.sources = found;
    });
    Discovery::Ok
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
fn install_roster(sources: &[SourceRef], primary: Option<usize>) -> usize {
    let order = registration_order(sources);
    for &i in &order {
        let s = &sources[i];
        let id = crate::plex::register(&s.machine_id, &s.address, s.port as i32, &s.token);
        if primary == Some(i) {
            crate::plex::set_current(id);
        }
    }
    order.len()
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
    let mut order: Vec<usize> = (0..sources.len()).filter(|&i| sources[i].usable()).collect();
    order.sort_by_key(|&i| !sources[i].owned);
    order
}

/// Which reached server is the primary: ours if it answered, else the first that did. A friend's
/// library is a better app than "no server found" when our own box is off.
fn primary_index(sources: &[SourceRef]) -> usize {
    sources.iter().position(|s| s.owned).unwrap_or(0)
}

/// Register the persisted roster — the BOOT twin of discovery, for the path that resumes a stored
/// session instead of signing in. Only the primary server comes back through
/// [`take_ready`]/`plex::install`; without this the shares stay unregistered until the next
/// sign-in, and a boot that resumed a session could browse only our own server.
///
/// Leaves `current` alone (an owned entry sorts first, so the registry's own "first registration
/// wins" already points at ours); the caller's `plex::install` of the primary is what retargets.
///
/// **Called from [`start_switch`], which covers the boot picker and every later "Change profile".
/// The one path it does NOT cover is `app.rs`'s straight-to-Home boot** — a stored session with a
/// single Plex Home user, or any automated run — which installs the primary itself and never
/// enters this module. That boot needs one line beside its `install_pms`:
/// `crate::auth::install_stored_roster(&session);`.
pub fn install_stored_roster(sess: &Session) -> usize {
    let n = install_roster(&sess.sources, None);
    if n > 0 {
        log(&format!("auth: roster restored — {n} server(s) registered"));
    }
    n
}

/// Re-key a stored roster to a newly switched profile.
///
/// `accessToken` is per **(user, server)**, so switching profile invalidates every stored token at
/// once, not only the primary's — a share left on the previous profile's token answers 401 to
/// everything. The switch already fetches `/api/v2/resources` as the new user to find the primary's
/// token, so this re-keys the whole roster from that same response: no extra round trip.
///
/// A source the response no longer names is DROPPED — that profile has not been granted it. A brand
/// new share is not added here: it has no probed address yet, and inventing one is what discovery
/// is for.
/// An entry with no machine id is dropped too: it cannot be identified, so it cannot be re-keyed,
/// and an empty id must never be allowed to match a resource that also happens to have none.
fn retoken(sources: &[SourceRef], resources: &[Resource]) -> Vec<SourceRef> {
    sources
        .iter()
        .filter(|s| !s.machine_id.is_empty())
        .filter_map(|s| {
            let r = resources.iter().find(|r| r.is_server() && r.client_identifier == s.machine_id)?;
            (!r.access_token.is_empty()).then(|| SourceRef { token: r.access_token.clone(), ..s.clone() })
        })
        .collect()
}

fn switch_thread(index: usize, pin: Option<String>) {
    let (cid, account_token, tile) = with_ctl(|c| {
        (c.session.client_id.clone(), c.session.account_token.clone(), c.users.get(index).cloned())
    });
    let tile = match tile {
        Some(t) => t,
        None => return,
    };
    // Picking the already-active, PIN-free profile needs no network — the stored per-user creds
    // still apply. This is what lets the boot picker proceed offline for the signed-in profile.
    // (A protected tile always goes through switch_user so the PIN is actually validated.)
    let same_user = pin.is_none()
        && !tile.protected
        && with_ctl(|c| !c.session.user.uuid.is_empty() && tile.uuid == c.session.user.uuid && !c.session.pms_token().is_empty());
    if same_user {
        log(&format!("auth: '{}' already active — no switch needed", tile.title));
        return with_ctl(|c| {
            c.error.clear();
            c.phase = Phase::Ready;
            c.apply_pending = true;
        });
    }
    with_ctl(|c| {
        c.phase = Phase::Switching;
        c.pin_denied = false;
    });
    let spawned = crate::task::spawn_small("switch", move || {
        let ac = AccountClient::new(&cid, Some(&account_token));
        let u = match ac.switch_user(&tile.uuid, pin.as_deref()) {
            Some(u) if !u.auth_token.is_empty() => u,
            _ => {
                // 401 (wrong PIN) and transport errors are indistinguishable at this layer;
                // only blame the PIN when one was actually submitted.
                log(&format!("auth: switch '{}' -> failed", tile.title));
                return with_ctl(|c| {
                    c.error = if pin.is_some() {
                        "Couldn't switch profile — check the PIN.".into()
                    } else {
                        "Couldn't switch profile — check the connection.".into()
                    };
                    c.pin_denied = pin.is_some();
                    c.phase = Phase::Profiles;
                });
            }
        };
        // The /switch token is an ACCOUNT token, NOT a PMS access token — using it directly 401s for
        // managed users (the admin's happens to double as one). Re-discover with the switched user's
        // token to get THIS user's per-user server access token (the /resources `accessToken` the PMS
        // accepts), scoped to what that profile is allowed to see.
        let (mid, roster) = with_ctl(|c| (c.session.server.machine_id.clone(), c.session.sources.clone()));
        let resources = AccountClient::new(&cid, Some(&u.auth_token)).resources().unwrap_or_default();
        // The fallback when we have no machine id used to take the FIRST server in the response,
        // which is only safe while one server exists: plex.tv listed the SHARE first in the
        // measured capture, so an empty `session.server.machine_id` would persist a friend's
        // accessToken as this profile's primary token — and 401 everything. Ours first, then
        // anything, and only the exact machine when we know it.
        let by_id = |r: &&Resource| r.is_server() && !mid.is_empty() && r.client_identifier == mid;
        let stok = resources
            .iter()
            .find(by_id)
            .or_else(|| mid.is_empty().then(|| resources.iter().find(|r| r.is_server() && r.owned)).flatten())
            .or_else(|| mid.is_empty().then(|| resources.iter().find(|r| r.is_server())).flatten())
            .map(|r| r.access_token.clone())
            .filter(|t| !t.is_empty());
        // The per-(user, server) token is per server, so EVERY entry of the roster has just gone
        // stale, not only the primary's — a share left on the previous profile's token answers 401
        // to everything. This response is the one already being fetched, so re-keying costs nothing.
        //
        // COMPUTED here, APPLIED only on the success branch below. `retoken` drops what the new
        // profile was not granted, and a switch that then fails leaves the user on the picker with
        // their old profile still signed in — persisting a roster re-keyed for a profile we did not
        // switch to would delete their shares from disk the next time anything saved the session.
        // Guarded on the response naming at least one server, so a failed fetch cannot read as
        // "this profile has been un-shared everything" either.
        let rekeyed = resources.iter().any(|r| r.is_server()).then(|| retoken(&roster, &resources));
        match stok {
            Some(token) => {
                log(&format!("auth: switch '{}' -> ok (per-user server token)", tile.title));
                if let Some(next) = rekeyed {
                    with_ctl(|c| c.session.sources = next.clone());
                    install_roster(&next, None);
                }
                with_ctl(|c| {
                    c.session.user = UserRef {
                        id: u.id,
                        uuid: u.uuid.clone(),
                        title: u.title.clone(),
                        thumb: tile.thumb.clone(),
                        token,
                    };
                    c.error.clear();
                    c.phase = Phase::Ready;
                    c.apply_pending = true;
                });
            }
            None => {
                log(&format!("auth: switch '{}' -> no server access", tile.title));
                with_ctl(|c| {
                    c.error = format!("{} has no access to this server", tile.title);
                    c.phase = Phase::Profiles;
                });
            }
        }
    });
    if !spawned {
        // Phase::Switching is a spinner with nothing behind it now — drop back to the roster the
        // same way the transport failure above does, so the tile can simply be picked again.
        with_ctl(|c| {
            c.error = "Couldn't switch profile. Try again.".into();
            c.phase = Phase::Profiles;
        });
    }
}

// ---- helpers ----

fn set_error(msg: &str) {
    log(&format!("auth: ERROR {msg}"));
    with_ctl(|c| {
        c.error = msg.to_owned();
        c.phase = Phase::Error;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn resource(json: &str) -> Resource {
        serde_json::from_str(json).expect("fixture parses")
    }

    /// A share with FOUR advertised addresses, which between them cover every case the probe loop
    /// has to get right: the owner's LAN address (policy drops it), a hostname this transport
    /// cannot resolve, and two public IPv4s so "the first one answered as somebody else" has a
    /// second one to fall through to. Shaped on the live capture of 2026-08-11
    /// (`docs/shared-servers.md` §2); the addresses are stand-ins, the arrangement is not.
    fn a_share() -> Resource {
        resource(
            r#"{"name":"friend-nas","clientIdentifier":"bbbb2222","provides":"server","owned":false,
                "sourceTitle":"afriend","publicAddressMatches":false,"httpsRequired":false,
                "accessToken":"tok-share","connections":[
                  {"protocol":"https","address":"10.9.9.7","port":32400,
                   "uri":"https://10-9-9-7.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
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
        format!(r#"{{"MediaContainer":{{"size":0,"machineIdentifier":"{mid}","version":"1.43.3"}}}}"#).into_bytes()
    }

    /// A recording dial. Returns whatever the script says for an address, and remembers the order
    /// it was asked — which is how "it stopped" and "it never tried that one" become assertions.
    struct Dialled {
        seen: RefCell<Vec<String>>,
        answers: Vec<(&'static str, i32, Vec<u8>)>,
    }
    impl Dialled {
        fn new(answers: Vec<(&'static str, i32, Vec<u8>)>) -> Dialled {
            Dialled { seen: RefCell::new(Vec::new()), answers }
        }
        fn dial(&self, host: &str, port: i32) -> (i32, Vec<u8>) {
            self.seen.borrow_mut().push(format!("{host}:{port}"));
            match self.answers.iter().find(|(h, _, _)| *h == host) {
                Some((_, s, b)) => (*s, b.clone()),
                None => (0, Vec::new()), // nothing answered at that address
            }
        }
        fn seen(&self) -> Vec<String> {
            self.seen.borrow().clone()
        }
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
            ("198.51.100.7", 200, identity_json("zzzz9999")), // someone else entirely
            ("203.0.113.9", 200, identity_json("bbbb2222")),  // the server we asked for
        ]);

        match probe_server(&plan, &|h, p| d.dial(h, p)) {
            Reach::At(c) => assert_eq!((c.address.as_str(), c.port), ("203.0.113.9", 31234)),
            _ => panic!("the second address answers as the right machine: {:?}", d.seen()),
        }
        assert_eq!(d.seen(), vec!["198.51.100.7:31234", "203.0.113.9:31234"]);

        // …and the same body from the wrong machine is never enough on its own
        assert_eq!(classify(200, &identity_json("zzzz9999"), "bbbb2222"), Outcome::WrongServer);
        assert_eq!(classify(200, &identity_json("bbbb2222"), "bbbb2222"), Outcome::Reachable);
        // a 200 that says nothing we can check is not an acceptance either
        assert_eq!(classify(200, b"<html>router login</html>", "bbbb2222"), Outcome::WrongServer);
        // nor is a resource plex.tv sent without an identity to verify against
        assert_eq!(classify(200, &identity_json("bbbb2222"), ""), Outcome::WrongServer);
    }

    /// **A 401 is its own state, never folded into "unreachable" — but it ends the CANDIDATE, not
    /// the server.** The probe is unauthenticated, so a 401 is not this server refusing our
    /// credential; it is whatever is listening refusing an anonymous request, and that need not be
    /// the server at all. The scenario that decides it: DHCP moves the PMS, a NAS answers 401 at its
    /// old address, and abandoning the server there would drop the user's OWN server without ever
    /// trying the second address plex.tv advertised for it.
    #[test]
    fn a_401_moves_to_the_next_address_and_only_a_clean_sweep_is_a_refusal() {
        assert_eq!(classify(401, b"", "bbbb2222"), Outcome::Unauthorized);
        // and 401 is the only status that means it: an endpoint refusal, a dead gateway and no
        // answer at all are all just "try the next address"
        for s in [403, 404, 500, 502, 0] {
            assert_eq!(classify(s, b"", "bbbb2222"), Outcome::Unreachable, "status {s}");
        }

        // one address 401s, the next answers properly → the server is REACHED, not abandoned
        let plan = probe::plan(&a_share());
        let d = Dialled::new(vec![("198.51.100.7", 401, Vec::new()), ("203.0.113.9", 200, identity_json("bbbb2222"))]);
        match probe_server(&plan, &|h, p| d.dial(h, p)) {
            Reach::At(c) => assert_eq!(c.address, "203.0.113.9"),
            _ => panic!("a 401 on one address must not cost the server: {:?}", d.seen()),
        }
        assert_eq!(d.seen().len(), 2, "the 401 did not stop the sweep");

        // every dialable address 401s → THAT is a refusal, and it is not "unreachable"
        let all = Dialled::new(vec![("198.51.100.7", 401, Vec::new()), ("203.0.113.9", 401, Vec::new())]);
        assert!(matches!(probe_server(&plan, &|h, p| all.dial(h, p)), Reach::Refused));

        // a 401 mixed with silence is NOT a refusal — the silent address is the unexplained one,
        // and reporting "refused" would send the user to the wrong setting entirely
        let mixed = Dialled::new(vec![("198.51.100.7", 401, Vec::new())]);
        assert!(matches!(probe_server(&plan, &|h, p| mixed.dial(h, p)), Reach::No));
    }

    /// BACK during discovery must leave the process alone. `cancel` is a phase flip, so by the time
    /// a dial returns the stored session may already be installed and the user on Home — and
    /// everything discovery does next is global: it registers servers, retargets `client()`, and
    /// overwrites the session. It gets to do none of that.
    #[test]
    fn a_flow_that_moved_on_mid_dial_registers_nothing_and_stores_nothing() {
        let d = Dialled::new(vec![
            ("192.168.0.10", 200, identity_json("aaaa1111")),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        let out = resolve_roster(&a_two_server_account(), &|h, p| d.dial(h, p), &|| true);
        assert!(matches!(out, Resolved::Cancelled), "cancelled before anything was dialled");
        assert!(d.seen().is_empty(), "not one socket after the flow moved on: {:?}", d.seen());

        // and the abort is polled BETWEEN servers, so a cancel landing after the first still stops
        let hits = std::cell::Cell::new(0);
        let after_one = || {
            hits.set(hits.get() + 1);
            hits.get() > 1
        };
        let d2 = Dialled::new(vec![
            ("192.168.0.10", 200, identity_json("aaaa1111")),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &|h, p| d2.dial(h, p), &after_one),
            Resolved::Cancelled
        ));
        assert_eq!(d2.seen(), vec!["192.168.0.10:32400"], "the second server is never dialled");
    }

    /// The 8-second trap, and the half of it the transport adds. Policy has already dropped the
    /// share's `local` address (the OWNER's LAN); this asserts the probe loop never dials it — and
    /// never dials the two candidates this client cannot speak to at all, which are unspoken rather
    /// than unreachable and must not be counted as the server failing.
    #[test]
    fn only_addresses_this_transport_can_dial_are_ever_probed() {
        let plan = probe::plan(&a_share());
        assert_eq!(plan.candidates.len(), 6, "policy kept three connections, two candidates each");

        let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        assert!(matches!(probe_server(&plan, &|h, p| d.dial(h, p)), Reach::At(_)));
        let seen = d.seen();
        assert!(!seen.iter().any(|s| s.starts_with("10.9.x.x")), "the owner's LAN address: {seen:?}");
        assert!(!seen.iter().any(|s| s.contains("example.internal")), "no DNS in this transport: {seen:?}");
        assert!(!seen.iter().any(|s| s.contains("plex.direct")), "no TLS in this transport: {seen:?}");

        // the rule itself, stated on the candidates
        let http_v4 = |a: &str| Candidate {
            url: format!("http://{a}:32400"),
            scheme: Scheme::Http,
            location: probe::Location::Remote,
            address: a.into(),
            port: 32400,
            ipv6: false,
        };
        assert!(dialable(&http_v4("203.0.113.9")));
        assert!(!dialable(&Candidate { scheme: Scheme::Https, ..http_v4("203.0.113.9") }));
        assert!(!dialable(&http_v4("media.example.internal")));
        assert!(!dialable(&http_v4("2001:db8::1")));
        assert!(!dialable(&http_v4("203.0.113")), "three octets is not an address");
        assert!(!dialable(&http_v4("203.0.113.999")), "999 is not an octet");
    }

    /// **The address that reaches the session file is one this transport can dial — never an IPv6
    /// literal, never a hostname.** This is the property that replaced `choose_local_connection`'s
    /// v6 guard: the foundation's `includeIPv6=1` made plex.tv offer v6 connections, and the chooser
    /// this file used to end in took the FIRST `local` match and PERSISTED it, so one v6 address
    /// wrote an undialable server to disk and broke every later boot, not just that one.
    ///
    /// Here the guard is structural rather than a filter that can be forgotten: nothing but a
    /// `dialable` candidate is ever dialled, and only a candidate that ANSWERED becomes a
    /// `SourceRef`. So an address can only be persisted after it has been connected to.
    #[test]
    fn only_an_address_this_transport_can_dial_is_ever_chosen_and_stored() {
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
            // every one of them answers, correctly — so nothing but the ORDER and the dialable
            // filter can keep the v6 addresses out
            ("2001:db8::1", 200, identity_json("aaaa1111")),
            ("fd00::5", 200, identity_json("aaaa1111")),
            ("192.168.0.10", 200, identity_json("aaaa1111")),
        ]);

        match probe_server(&plan, &|h, p| d.dial(h, p)) {
            Reach::At(c) => assert_eq!(c.address, "192.168.0.10", "a v6 literal is not dialable here"),
            _ => panic!("the LAN IPv4 answers: {:?}", d.seen()),
        }
        assert_eq!(d.seen(), vec!["192.168.0.10:32400"], "the v6 addresses are never even tried");
        // …and the flag being false does not make a colon-bearing address dialable
        assert!(!is_ipv4_literal("fd00::5") && !is_ipv4_literal("2001:db8::1"));
    }

    /// The one field that decides whether we trust a connection is scanned for, not deserialized:
    /// PMS answers XML unless an explicit JSON Accept survives to it, and a probe is the request
    /// most likely to meet a proxy that rewrites headers.
    #[test]
    fn the_machine_identifier_is_read_from_json_and_from_xml_alike() {
        assert_eq!(machine_id_in(&identity_json("abc123")).as_deref(), Some("abc123"));
        assert_eq!(
            machine_id_in(br#"<MediaContainer size="0" machineIdentifier="abc123" version="1.43.3"/>"#).as_deref(),
            Some("abc123")
        );
        assert_eq!(machine_id_in(br#"{"MediaContainer":{"machineIdentifier" : "abc123"}}"#).as_deref(), Some("abc123"));
        // an empty value is no value — it must not read as "the next field"
        assert_eq!(machine_id_in(br#"{"machineIdentifier":"","size":0}"#), None);
        assert_eq!(machine_id_in(b"nothing here"), None);
        assert_eq!(machine_id_in(b""), None);
    }

    /// The account this feature exists for, as `/api/v2/resources` really returns it: OUR server
    /// (owned, LAN + public + relay) and the SHARE (not owned, the owner's 10.9.x.x LAN, an internal
    /// hostname, and one public IPv4). Shaped on the live capture of 2026-08-11
    /// (`docs/shared-servers.md` §2) — the addresses are stand-ins, the arrangement is not, and the
    /// share is listed FIRST because plex.tv's order is not ours to rely on.
    fn a_two_server_account() -> Vec<Resource> {
        serde_json::from_str(
            r#"[
              {"name":"friend-nas","clientIdentifier":"bbbb2222","provides":"server","owned":false,
               "sourceTitle":"afriend","ownerId":987654,"publicAddressMatches":false,
               "httpsRequired":false,"accessToken":"tok-share","connections":[
                 {"protocol":"https","address":"10.9.9.7","port":32400,
                  "uri":"https://10-9-9-7.h.plex.direct:32400","local":true,"relay":false,"IPv6":false},
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
    /// 10.9.x.x LAN. Each carries its own grant, and the non-server resource is not in the roster.
    #[test]
    fn a_sign_in_to_a_two_server_account_settles_on_one_address_each_ours_first() {
        let d = Dialled::new(vec![
            ("192.168.0.10", 200, identity_json("aaaa1111")),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        let Resolved::Reached(roster) = resolve_roster(&a_two_server_account(), &|h, p| d.dial(h, p), &|| false) else {
            panic!("both servers answer: {:?}", d.seen())
        };

        assert_eq!(roster.len(), 2, "a player resource is not a server");
        assert_eq!(primary_index(&roster), 0, "ours is the primary and becomes `current`");

        let own = &roster[0];
        assert!(own.owned && own.machine_id == "aaaa1111");
        assert_eq!((own.address.as_str(), own.port), ("192.168.0.10", 32400), "the LAN v4, not the v6");
        assert_eq!(own.token, "tok-own");
        assert!(own.shared_by.is_empty(), "an owned server has no owner to name");

        let share = &roster[1];
        assert!(!share.owned && share.machine_id == "bbbb2222");
        assert_eq!(
            (share.address.as_str(), share.port),
            ("203.0.113.9", 31234),
            "the owner's 10.9.x.x LAN is not ours to dial, and their hostname does not resolve"
        );
        assert_eq!(share.token, "tok-share", "a share is a separate authority: OUR token gets a 401");
        assert_eq!(share.shared_by, "afriend");
        assert!(roster.iter().all(|s| s.usable()), "every entry is dialable, so every one registers");

        // Exactly two dials: the relay is never tried (a 2 Mbit/s https-only tunnel), nor the
        // owner's LAN, nor the internal hostname, nor the v6 literal — none of which this transport
        // can speak to. And OURS is probed first, though plex.tv listed the share first.
        assert_eq!(d.seen(), vec!["192.168.0.10:32400", "203.0.113.9:31234"]);
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
        assert!(matches!(resolve_roster(&players, &|_, _| (0, Vec::new()), &|| false), Resolved::NoServers));

        // servers that simply do not answer
        let silent = Dialled::new(vec![]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &|h, p| silent.dial(h, p), &|| false),
            Resolved::None { refused: false }
        ));

        // …and one that answers 401: something in front of it refuses unauthenticated requests,
        // which is not a network fault and must not be worded as one
        let refused = Dialled::new(vec![("192.168.0.10", 401, Vec::new()), ("203.0.113.9", 401, Vec::new())]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &|h, p| refused.dial(h, p), &|| false),
            Resolved::None { refused: true }
        ));

        // a share that answers while OUR server is off still signs in — a friend's library beats
        // "no server found" — and it becomes the primary because it is the only thing there is
        let one = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        let Resolved::Reached(roster) = resolve_roster(&a_two_server_account(), &|h, p| one.dial(h, p), &|| false) else {
            panic!("the share answered")
        };
        assert_eq!(roster.len(), 1);
        assert_eq!(primary_index(&roster), 0);
        assert!(!roster[0].owned, "the primary is a share here, and that is the point");
    }

    fn source(machine_id: &str, owned: bool, token: &str) -> SourceRef {
        SourceRef {
            machine_id: machine_id.into(),
            name: machine_id.into(),
            shared_by: if owned { String::new() } else { "afriend".into() },
            owned,
            address: "10.0.0.1".into(),
            port: 32400,
            token: token.into(),
        }
    }

    /// Our own server registers first and is the primary, whatever order plex.tv listed the account
    /// in — because the registry makes the first registration `current` when nothing is yet, so the
    /// ordering is what stops a boot coming up pointed at a friend's server and building Home from
    /// their library.
    #[test]
    fn our_own_server_leads_the_roster_however_plex_tv_ordered_it() {
        let roster = vec![source("share-1", false, "t1"), source("ours", true, "t2"), source("share-2", false, "t3")];
        assert_eq!(registration_order(&roster), vec![1, 0, 2], "ours first, then plex.tv's own order");
        assert_eq!(primary_index(&roster), 1);

        // an entry with no credential (or no address) cannot be dialled, so it is not registered —
        // registering it would put a `Client` in the table that 401s everything asked of it
        let mut half = roster.clone();
        half[0].token.clear();
        half[2].address.clear();
        assert_eq!(registration_order(&half), vec![1]);

        // a shares-only roster (our own box is off) still yields a primary rather than nothing:
        // a friend's library is a better app than "no server found"
        let shares = vec![source("share-1", false, "t1"), source("share-2", false, "t3")];
        assert_eq!(primary_index(&shares), 0);
        assert_eq!(registration_order(&shares), vec![0, 1]);
    }

    /// A profile switch re-keys the WHOLE roster, not just the primary. `accessToken` is per
    /// (user, server), so the other profile's token on a share is a 401 waiting to happen — and a
    /// server this profile has not been granted leaves the roster rather than lingering with a
    /// credential that no longer works.
    #[test]
    fn switching_profile_re_keys_every_source_and_drops_the_ones_not_granted() {
        let roster =
            vec![source("ours", true, "old-own"), source("share-1", false, "old-share"), source("gone", false, "old-gone")];
        let rs = vec![
            resource(r#"{"clientIdentifier":"ours","provides":"server","owned":true,"accessToken":"new-own"}"#),
            resource(r#"{"clientIdentifier":"share-1","provides":"server","owned":false,"accessToken":"new-share"}"#),
        ];

        let next = retoken(&roster, &rs);
        assert_eq!(next.len(), 2, "the un-granted server is gone, not left on a stale token");
        assert_eq!(next[0].token, "new-own");
        assert_eq!((next[1].machine_id.as_str(), next[1].token.as_str()), ("share-1", "new-share"));
        assert_eq!(next[1].shared_by, "afriend", "everything but the token is carried over");
        assert_eq!(next[1].address, "10.0.0.1", "including the address discovery probed");

        // a resource that came back WITHOUT a token for this profile is not a re-key either
        let empty = vec![resource(r#"{"clientIdentifier":"ours","provides":"server","accessToken":""}"#)];
        assert!(retoken(&roster, &empty).is_empty());
        // and an entry with no identity cannot be re-keyed, and must never match by emptiness
        let anon = vec![source("", false, "old")];
        assert!(retoken(&anon, &[resource(r#"{"provides":"server","accessToken":"x"}"#)]).is_empty());
    }
}
