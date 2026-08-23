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
use crate::plex::probe::{self, Candidate, Outcome, ProbePlan};
use crate::plex::Origin;
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

/// **Which who's-watching picker is on screen** — the one fact [`cancel`] cannot work out for
/// itself, and the difference between an escape hatch and a privilege escalation.
///
/// It is ONE screen raised from THREE places, and BACK means something different on each. From
/// Home somebody has already identified themselves and Home is behind the picker, so backing out
/// hands them what they were already holding. Straight after a QR sign-in they have just proved
/// they hold the ACCOUNT, the credential a profile PIN hangs off. At BOOT neither is true — nobody
/// has identified themselves this run, there is nothing behind the picker but the persisted
/// session, and reinstating that is exactly the thing a PIN is there to stop.
///
/// Nothing in the state below could tell them apart (all three arrive at [`Phase::Profiles`] with
/// the same roster), so every raise site names its own kind. Two go through [`start_switch`]; the
/// third is `login_thread`, which sets that phase itself rather than calling it — which is also why
/// "they all call [`start_switch`]" is the wrong place to infer this from.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Picker {
    /// The boot gate's who's-watching, before any profile has been chosen this run.
    ///
    /// **The default, and deliberately the STRICT one.** Every picker names its own kind, so the
    /// default is only ever read where no picker is up at all: `ui::login`'s BACK, over whatever
    /// `Ctl` some other flow last reset. "We cannot say who is asking" must not resolve to "hand
    /// over the credentials" — a permissive default is the shape of the bug this enum exists to
    /// fix, and it is what left the dev-only `/tmp/plxnative-login` boot on the wrong side of it.
    #[default]
    Boot,
    /// Home's *Change profile*: already signed in as a profile, with Home behind the picker.
    ChangeProfile,
    /// The picker the QR sign-in raises when the account turns out to have a Plex Home roster —
    /// `login_thread`, not [`start_switch`]. Permissive for a stronger reason than *Change
    /// profile*: whoever is standing there completed a plex.tv sign-in seconds ago. That it is
    /// permissive AT ALL is the one open judgement here — see [`may_resume`].
    SignedIn,
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
    /// **Where the primary server is** — an [`Origin`], not a `(host, port)` pair, because the
    /// pair cannot say `https` and the host a certificate is issued for is not the address behind
    /// it (`plex::origin`). Read straight off the stored [`session::ServerRef`], which is the
    /// value discovery wrote and the one `can_go_local` gates.
    pub origin: Origin,
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
    // which picker `start_switch` raised — read by `cancel`, and by nothing else
    from: Picker,
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
///
/// **…and false at the BOOT picker when the stored profile is PIN-protected, which is a privilege
/// gate and not an ergonomic one.** The paragraph above reasons only about "carry on as the profile
/// I'm already signed in as", which is true from Home and false at boot: in the ordinary Plex Home
/// arrangement the adult profile is the protected one, so adult uses the app → child boots it →
/// picker → BACK reinstated the adult's per-user token and entered Home as them, with no code
/// entered. (Two presses did it from an open keypad, since BACK there only closes the pad.) The PIN
/// path itself was never wrong — plex.tv validates it and the no-network fast path in
/// [`switch_thread`] already excludes protected tiles — the hole was entirely in this escape hatch.
/// So a boot picker over a protected profile must be left by CHOOSING: pick a tile and enter its
/// PIN, or take the picker's own *Sign out* pill, which is focusable with ▼ whatever the roster
/// holds. The rule is [`may_resume`]; who is asking is [`Picker`].
///
/// **"Protected" also covers a session that names NO profile**, which is not a corner case but the
/// second half of the same hole: a sign-in abandoned at the who's-watching picker persists the
/// account token, the server and the roster with no profile chosen (deliberately — see
/// `login_thread`), and such a session's [`Session::pms_token`] falls back to the OWNER's server
/// token. The next boot raises a picker over exactly that, so BACK there handed out the owner's
/// credentials by a second road. [`Session::active_profile_is_protected`] is where that is decided.
///
/// The gate is a PICKER's, and `ui::login`'s BACK is left as it was, because in a shipped build it
/// cannot be this escalation: the boot gate only routes to the sign-in screen when `can_go_local()`
/// is false, which is the refusal above, and every other way onto that screen is somebody already
/// at Home. It is also the one screen with no *Sign out* pill to leave by. What it does now get is
/// the strict [`Picker`] default — no picker of its own means no `from` of its own, and the value
/// it inherits should not be the permissive one; the practical effect is confined to the dev-only
/// `/tmp/plxnative-login` boot, which is the one way to reach that screen over a live session.
pub fn cancel() -> bool {
    resume_stored(session::load())
}

/// May BACK out of the flow silently resume the stored session?
///
/// Pure, and split out from [`cancel`] so the one decision that gates a credential is gradeable on
/// the host: its caller runs inside the SDL event loop, where no test can reach it.
fn may_resume(from: Picker, stored_is_protected: bool) -> bool {
    match from {
        // Home is behind this picker and its user is already signed in as that profile: BACK hands
        // back exactly what they were holding when they opened it, PIN or no PIN.
        Picker::ChangeProfile => true,
        // The sign-in ceremony's own picker: the ACCOUNT credential was presented seconds ago and
        // the profile PIN hangs off it, so BACK still resumes. This arm is a JUDGEMENT rather than
        // a deduction, and it is the one thing here left exactly as it was found — at that moment
        // the stored session names no profile at all, so what BACK resumes on is the OWNER's server
        // token, and "the person who signed in is still the person holding the remote" is an
        // assumption about a room. Flipping it to `!stored_is_protected` would cost that flow one
        // profile pick and nothing else (the picker is fully usable, *Sign out* included).
        Picker::SignedIn => true,
        // Nobody has identified themselves yet, so resuming a protected profile IS the bypass.
        Picker::Boot => !stored_is_protected,
    }
}

/// [`cancel`] with the persisted session passed in.
fn resume_stored(sess: Session) -> bool {
    if !sess.can_go_local() {
        return false;
    }
    let from = with_ctl(|c| c.from);
    if !may_resume(from, sess.active_profile_is_protected()) {
        // No profile name: this file is the one users send us, and the line is about the flow, not
        // about who is behind the PIN. Two lines because the refusal has two distinct causes that
        // read as different bug reports — and neither names a SCREEN, because the strict default
        // means the sign-in screen's BACK can land here too.
        log(if sess.user.uuid.is_empty() {
            "auth: BACK refused — no profile has been chosen on this device yet"
        } else {
            "auth: BACK refused — the stored profile is PIN-protected"
        });
        return false;
    }
    log("auth: flow cancelled — resuming the stored session");
    // `from` rides through the reset: this is still the same flow being backed out of, and letting
    // it silently fall to the permissive default is the shape of the bug being fixed.
    with_ctl(|c| *c = Ctl { phase: Phase::Ready, session: sess, apply_pending: true, from, ..Ctl::default() });
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
                ReadyCreds { origin: c.session.server.origin(), token: c.session.pms_token().to_owned() },
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
///
/// `from` is the caller saying WHICH of those two it is, because the picker itself cannot tell and
/// [`cancel`] has to know — see [`Picker`].
pub fn start_switch(from: Picker) {
    let sess = session::load();
    if sess.account_token.is_empty() {
        return set_error("You're signed out — sign in to use profiles.");
    }
    // The boot picker reaches this before any profile is chosen, so it is the earliest point on
    // the resumed-session path where the stored roster can go back into the registry. Idempotent,
    // and it does not touch `current` — a "Change profile" from Home lands here too, by which time
    // everything is registered already and this is a no-op.
    install_stored_roster(&sess);
    // The SERVER roster's online refresh, beside the HOME-USER one spawned below. They are two
    // different rosters and only the second used to be refreshed here, despite this function's own
    // doc saying it seeded and refreshed "the persisted roster" — so a share granted after sign-in
    // never appeared on this path either.
    refresh_roster_online();
    with_ctl(|c| {
        c.error.clear();
        if c.users.is_empty() {
            c.users = sess.home_users.iter().map(UserTile::of_ref).collect();
        }
        c.session = sess;
        c.phase = Phase::Profiles;
        c.from = from;
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
                let roster = with_ctl(|c| {
                    c.session.home_users = users.iter().map(UserTile::to_ref).collect();
                    c.users = users;
                    c.session.home_users.clone()
                });
                // Only the field this worker owns, and through the one door. A whole-session save
                // from the CTL snapshot would put the stale `sources` back over the SERVER roster
                // — which `refresh_roster_online` is refreshing at this very moment, since
                // `start_switch` spawns both and neither can know which lands first.
                session::update(|s| Some(Session { home_users: roster, ..s.clone() }));
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
///
/// **The server REGISTRY has to go with the session file**, and for a long time it did not. Clearing
/// the file only stops the NEXT boot resuming: in this process every server the account was granted
/// stayed in the registry with its live per-(user, server) token, so signing into a different
/// account left both accounts' servers registered side by side — `pms::roster` merged both into
/// Home, `browse` listed both in Sources, `search` fanned every query out over both, each with the
/// departed account's credential. `plex::revoke_all` retires them and blanks their tokens; the
/// slots are not reused, so nothing the new account registers can inherit the old one's per-server
/// stores either.
pub fn sign_out() {
    session::clear();
    session::set_current(None);
    crate::plex::revoke_all();
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
    let qr_png = crate::net::https_get_public(&qr_url).filter(|r| r.ok()).map(|r| r.body).unwrap_or_default();
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
        Discovery::Refused => return set_error("Your Plex server refused the connection — check its network access settings."),
        Discovery::Silent => return set_error("Couldn't reach any Plex server — check the connection."),
    }
    // Install the PMS client now (server/owner token) so the who's-watching avatars can proxy
    // through the server's photo transcoder. The per-user token is swapped in on profile pick.
    let (origin, stok) = with_ctl(|c| (c.session.server.origin(), c.session.server.token.clone()));
    crate::plex::install(&origin, &stok);
    // `log_form`, not `base()`: byte-identical to the `{addr}:{port}` this line always printed
    // for a plaintext origin (so an archived log stays comparable), and the whole URL as soon as
    // the scheme is worth saying. See `Origin::log_form`.
    log(&format!("auth: PMS client installed {}", origin.log_form()));

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
            // The THIRD picker, and the one that does NOT go through `start_switch` — so it says
            // which it is here, rather than inheriting whatever `start_login`'s reset left behind.
            c.from = Picker::SignedIn;
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
// `172.20.x.x` — 8 s of timeout from here, and the *worse* outcome is that it succeeds against
// somebody else's box at that address on our own LAN (`docs/shared-servers.md` §2a).
//
// So the shape is: ranked candidates from `plex::probe` (pure policy — it drops that entire
// connection before any deadline can be spent), then dial them in order and **verify identity on
// the answer** before believing it.

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
    /// A 401. A TOKEN problem, not a reachability one: the `accessToken` is per (user, server) and
    /// carries the sharing grant, so every other address of this server answers identically and
    /// trying them buys nothing. Reporting "can't reach nas-home" here would send the user to look
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
fn get_identity(origin: &Origin) -> (i32, Vec<u8>) {
    match crate::http::request_capped(
        origin,
        IDENTITY,
        crate::http::Method::Get,
        &[crate::http::ACCEPT_JSON],
        64 * 1024,
    ) {
        // `/identity` is one small MediaContainer. The ceiling is enforced by each transport
        // WHILE it reads, before a machine we have not accepted can make this worker allocate an
        // unbounded body; an over-limit answer is therefore a transport failure, never a prefix
        // that might happen to contain a plausible machine id.
        Some(r) => (r.status, r.body),
        None => (0, Vec::new()),
    }
}

/// Dial one server's candidates in rank order, stopping at the first that answers **as it**.
///
/// `dial` is injected so the whole acceptance policy — identity verified before a connection is
/// accepted, a wrong machine discarded rather than retried, a 401 ending the server instead of the
/// candidate — is host-testable without a socket.
///
/// Sequential, not parallel — **and that trade got more expensive when the transport stopped
/// refusing things.** It used to be cheap because every https and hostname candidate was skipped
/// outright. [`dial_target`] admits those now, and each address costs a real deadline —
/// `stream.rs`'s `CONNECT_TIMEOUT_MS` (a whole-chain budget) on the plaintext arm, `net::API`'s
/// connect timeout on the TLS one. `probe::candidates` therefore removes a non-owned unmatched LAN
/// connection whole: the measured private address cannot be ours and must not consume an 8 s TLS
/// slot before the share's public address.
///
/// The worst case is a set on a LAN with no route to the internet: every `plex.direct` name fails
/// to resolve before the plaintext twin — which is exactly the address that answers there — gets
/// its turn. It is bounded rather than open-ended, and it is the cost of ranking TLS first, which
/// is right for every other case.
///
/// It stays sequential because this runs on the login worker behind a spinner, and because the
/// racing design is a unit of its own: parallel within a server, serial across servers with a gap,
/// and a two-phase "usable on the first reachable probe, upgraded to the best-scoring one when the
/// race completes" selection. Nothing here precludes it — `probe_server` takes its candidates in
/// rank order and its `dial` by reference, so the loop is the only thing that has to change — and
/// it is the trade `docs/shared-servers.md` §5 step 4 leaves open.
fn probe_server(plan: &ProbePlan, dial: &dyn Fn(&Origin) -> (i32, Vec<u8>)) -> Reach {
    let mut tried = 0;
    for c in plan.candidates.iter() {
        let Some(origin) = dial_target(c) else { continue };
        tried += 1;
        // The ORIGIN, whole — the same value handed back in `Reach::At`, so what answered and what
        // the roster records cannot be two different things. It is passed rather than split into
        // `(host, port)` because the SCHEME is now part of what gets dialled: splitting it here
        // would put the transport choice back at a call site.
        let (status, body) = dial(&origin);
        match classify(status, &body, &plan.machine_id) {
            Outcome::Reachable => return Reach::At(c.clone(), origin),
            Outcome::Unauthorized => {
                log(&format!("auth: '{}' answered 401 at {} — a token problem, not the network", plan.name, c.address));
                return Reach::Refused;
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
fn resolve_roster(resources: &[Resource], dial: &dyn Fn(&Origin) -> (i32, Vec<u8>)) -> Resolved {
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
        let plan = probe::plan(r);
        match probe_server(&plan, dial) {
            Reach::At(c, origin) => {
                let s = SourceRef {
                    machine_id: plan.machine_id.clone(),
                    name: plan.name.clone(),
                    shared_by: plan.source_title.clone().unwrap_or_default(),
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
                log(&format!("auth: reached {} via {}", s.describe(), origin.log_form()));
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
    let resolved = resolve_roster(&resources, &get_identity);
    let found = match resolved {
        Resolved::NoServers => return Discovery::NoServers,
        Resolved::None { refused: true } => return Discovery::Refused,
        Resolved::None { refused: false } => return Discovery::Silent,
        Resolved::Reached(f) => f,
    };

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
        // Carried across from the roster entry, so the primary and its `sources` twin can never
        // disagree about where the same server is. `reconcile_primary` keeps them together later.
        origin_url: p.origin_url.clone(),
    };
    with_ctl(|c| {
        c.session.server = server;
        c.session.sources = found;
    });
    Discovery::Ok
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
/// a boot with plex.tv unreachable still browses whatever was already known. Registration is
/// idempotent (the registry keys on `machineIdentifier`) and deliberately passes `None` as the
/// primary, so a refresh never re-points `current` at a different machine under a user who is
/// already browsing.
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
/// worse than wrong, it is a re-grant — [`retoken`] had already dropped the servers that profile
/// was not given, and this puts them back.
///
/// Re-keying the answer for the active profile afterwards is not available: the per-user tokens
/// only exist in a `/api/v2/resources` fetched with THAT user's account token, which the switch
/// obtains for one request and does not persist. So the honest answer is to skip, and the cost is
/// named: a share granted while a managed profile is signed in appears when someone next switches
/// profile (the switch re-keys the whole roster from its own response) or signs in again.
pub fn refresh_roster_online() {
    let sess = session::load();
    if sess.account_token.is_empty() {
        return; // signed out; nothing to ask plex.tv with
    }
    if !sess.active_profile_is_admin() {
        return log("auth: roster refresh skipped — the account token is the owner's, and a managed profile is active");
    }
    let _ = crate::task::spawn_small("roster-srv", move || {
        let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
        let Some(resources) = ac.resources() else {
            log("auth: roster refresh — plex.tv unreachable, keeping the stored roster");
            return;
        };
        let found = match resolve_roster(&resources, &get_identity) {
            Resolved::Reached(f) => f,
            // "no server answered" is not evidence that the grant is gone: the friend's box may
            // simply be off. Dropping the roster here would make an offline share un-browsable
            // for good rather than until it comes back.
            _ => {
                log("auth: roster refresh — nothing answered, keeping the stored roster");
                return;
            }
        };
        install_roster(&found, None);
        // The comparison, the primary reconcile and the write are ONE step under `session`'s own
        // lock. Both halves of that were races before. Reading with `load` and writing with `save`
        // let a profile pick land in between and be rewritten with the pre-switch profile, so the
        // next boot resumed as the wrong person; and grading the change against this worker's
        // spawn-time snapshot compared against the older of the two answers. `update` also refuses
        // outright when the file no longer holds a session — a sign-out during this fetch must not
        // be undone by a roster we no longer have the credentials for.
        let n = found.len();
        let persisted = session::update(move |s| {
            let roster_changed = found.len() != s.sources.len()
                || found.iter().zip(s.sources.iter()).any(|(a, b)| {
                    a.machine_id != b.machine_id || a.address != b.address || a.port != b.port || a.token != b.token
                });
            let mut next = s.clone();
            let moved = reconcile_primary(&mut next.server, &found);
            if !(roster_changed || moved) {
                return None;
            }
            next.sources = found;
            Some(next)
        });
        log(&format!("auth: roster refresh — {n} server(s){}", if persisted { ", persisted" } else { "" }));
    });
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
    let Some(s) = found.iter().find(|s| s.machine_id == server.machine_id && s.usable()) else { return false };
    if server.address == s.address && server.port == s.port && server.token == s.token && server.origin_url == s.origin_url
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
    let moved = server.address != s.address || server.port != s.port || (server.origin_url != s.origin_url && !learned_origin);
    if moved {
        // The machine name and the address, never the token and never the machine id — the same
        // line `SourceRef::describe` draws.
        log(&format!("auth: primary {:?} now answers at {}:{}", server.name, s.address, s.port));
    }
    server.address = s.address.clone();
    server.port = s.port;
    // The origin moves with the address for the same reason the token does: it came out of the
    // same answer. Leaving it behind would keep dialling the old one, which is the bug this
    // whole function exists to close, one field further in.
    server.origin_url = s.origin_url.clone();
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
fn install_roster(sources: &[SourceRef], primary: Option<usize>) -> usize {
    let order = registration_order(sources);
    for &i in &order {
        let s = &sources[i];
        // `registration_order` already filtered on `usable()`, which IS `origin().is_some()` —
        // so this `else` is unreachable today and is a `continue` rather than an `expect` because
        // a roster entry has never been allowed to cost more than itself (`de_soft_vec`).
        let Some(origin) = s.origin() else { continue };
        let id = crate::plex::register_origin(&s.machine_id, &origin, &s.token);
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
/// **Called from [`start_switch`]** (the boot picker and every later "Change profile") **and from
/// `app.rs`'s straight-to-Home boot** — a stored session with a single Plex Home user, or any
/// automated run — which installs the primary itself and never enters this module. That second call
/// site was missing until 2026-08-14, and the symptom was the whole feature being absent on the most
/// ordinary boot there is: one registered server, no shares, nothing to browse or attribute.
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
        let stok = resources
            .iter()
            .find(|r| r.is_server() && (mid.is_empty() || r.client_identifier == mid))
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
    use crate::plex::probe::Scheme;
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
            ("203-0-113-9.h.plex.direct", 200, identity_json("bbbb2222")),  // the server we asked for
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
            _ => panic!("the second address answers as the right machine: {:?}", d.seen()),
        }
        // Every https candidate is tried before any plaintext one. Rule 1 removed the private-LAN
        // connection whole, so the walk starts at the remote names.
        assert_eq!(
            d.seen(),
            vec![
                "https://media.example.internal:31234",
                "https://198-51-100-7.h.plex.direct:31234",
                "https://203-0-113-9.h.plex.direct:31234",
            ]
        );

        // …and the same body from the wrong machine is never enough on its own
        assert_eq!(classify(200, &identity_json("zzzz9999"), "bbbb2222"), Outcome::WrongServer);
        assert_eq!(classify(200, &identity_json("bbbb2222"), "bbbb2222"), Outcome::Reachable);
        // a 200 that says nothing we can check is not an acceptance either
        assert_eq!(classify(200, b"<html>router login</html>", "bbbb2222"), Outcome::WrongServer);
        // nor is a resource plex.tv sent without an identity to verify against
        assert_eq!(classify(200, &identity_json("bbbb2222"), ""), Outcome::WrongServer);
    }

    /// **401 is a token problem, never "unreachable".** The `accessToken` is per (user, server) and
    /// carries the sharing grant, so every other address of that server answers identically —
    /// trying them wastes 2 s each and then reports "can't reach nas-home", sending the user to look
    /// at their friend's router for something that lives in `/api/v2/resources`.
    #[test]
    fn a_401_ends_the_server_rather_than_being_retried_as_a_dead_address() {
        assert_eq!(classify(401, b"", "bbbb2222"), Outcome::Unauthorized);
        // and it is the ONLY status that means this: a refusal of the endpoint, a dead gateway and
        // no answer at all are all just "try the next address"
        for s in [403, 404, 500, 502, 0] {
            assert_eq!(classify(s, b"", "bbbb2222"), Outcome::Unreachable, "status {s}");
        }

        let plan = probe::plan(&a_share());
        let d = Dialled::new(vec![
            ("198-51-100-7.h.plex.direct", 401, Vec::new()),
            ("203.0.113.9", 200, identity_json("bbbb2222")),
        ]);
        assert!(matches!(probe_server(&plan, &|o| d.dial(o)), Reach::Refused));
        assert_eq!(
            d.seen(),
            vec![
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
    /// the transport did not open the ACCEPTANCE. Five candidates are dialled here, four answer
    /// nothing, and only the one whose `machineIdentifier` matches is accepted.
    #[test]
    fn every_advertised_address_is_dialable_and_only_an_impossible_port_is_not() {
        let plan = probe::plan(&a_share());
        assert_eq!(plan.candidates.len(), 6, "four connections; the share's LAN one goes whole");
        assert!(plan.candidates.iter().all(dialable), "not one of them is refused any more: {plan:#?}",
                plan = plan.candidates);

        let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        assert!(matches!(probe_server(&plan, &|o| d.dial(o)), Reach::At(..)));
        // The owner's `172.20.x.x` connection is absent whole. TLS could reject a stranger there,
        // but it cannot recover the sequential 8 s deadline spent before the public candidate.
        let seen = d.seen();
        assert!(!seen.iter().any(|s| s.contains("172.20") || s.contains("172-20-4-7")),
                "no candidate from the owner's LAN: {seen:?}");

        // The rule itself, stated on the candidates. The fixture builds `url` the way
        // `probe::candidates` does — from the SAME address and port — because that consistency is
        // the property `dial_target` relies on: it reads the origin off the URL, which is also what
        // gets recorded, so a fixture whose url and port disagree would assert nothing real.
        let cand = |scheme: Scheme, host: &str, port: i64| Candidate {
            url: format!(
                "{}://{}:{port}",
                scheme.as_str(),
                if host.contains(':') { format!("[{host}]") } else { host.to_string() }
            ),
            scheme,
            location: probe::Location::Remote,
            address: host.into(),
            port,
            ipv6: host.contains(':'),
        };
        let at = |host: &str| cand(Scheme::Http, host, 32400);
        assert!(dialable(&at("203.0.113.9")));
        assert!(dialable(&cand(Scheme::Https, "203-0-113-9.h.plex.direct", 31234)), "libcurl speaks TLS");
        assert!(dialable(&at("media.example.internal")), "stream.rs resolves names now");
        assert!(dialable(&at("2001:db8::1")), "…and dials either address family");

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
        assert_eq!(dial_target(&at("203.0.113.9")), Some(crate::plex::Origin::http("203.0.113.9", 32400)));
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
            Candidate { address: "192.0.2.55".into(), port: 4_294_999_696, ..good.clone() },
        );

        let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        assert!(matches!(probe_server(&plan, &|o| d.dial(o)), Reach::At(..)), "the good one still answers");
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
                assert_eq!(c.address, "192.168.0.10", "IPv4 leads the plaintext fallbacks");
                assert_eq!(o.base(), "http://192.168.0.10:32400", "…and the origin recorded is what was dialled");
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
        assert!(!d.seen().iter().any(|s| s.contains("fd00") || s.contains("2001:db8")), "{:?}", d.seen());
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
        let Resolved::Reached(roster) = resolve_roster(&a_two_server_account(), &|o| d.dial(o)) else {
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
            "the owner's 172.20 LAN is not ours to dial, and their hostname does not resolve"
        );
        assert_eq!(share.token, "tok-share", "a share is a separate authority: OUR token gets a 401");
        assert_eq!(share.shared_by, "friend");
        assert!(roster.iter().all(|s| s.usable()), "every entry is dialable, so every one registers");

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
                "https://media.example.internal:31234",
                "https://203-0-113-9.h.plex.direct:31234",
                "203.0.113.9:31234",
            ]
        );
        assert!(!d.seen().iter().any(|s| s.contains("plex-relay")), "a 2 Mbit/s tunnel is a last resort");
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
        let Resolved::Reached(roster) = resolve_roster(&a_two_server_account(), &|o| d.dial(o)) else {
            panic!("both servers answer over TLS: {:?}", d.seen())
        };

        assert_eq!(roster.len(), 2);
        assert_eq!(roster[0].origin_url, "https://plex-relay.example.net:8443", "ours, over the relay");
        assert_eq!(roster[1].origin_url, "https://203-0-113-9.h.plex.direct:31234", "the share, direct");
        for s in &roster {
            let o = s.origin().expect("a reached entry is dialable");
            assert!(o.is_tls(), "the connection that answered was TLS, so the stored origin must be");
            assert_eq!(o.base(), s.origin_url, "the stored string round-trips");
        }
        // The share's stored origin is the NAME and its `address` is the quad behind it. That
        // inequality is the whole reason an origin is parsed from a URL rather than rebuilt from an
        // address: rebuild it and the certificate stops matching.
        assert_eq!(roster[1].address, "203.0.113.9");
        assert_ne!(roster[1].origin().expect("dialable").host(), roster[1].address);

        // The relay is genuinely LAST: every LAN candidate of our own server was tried first, and
        // the share's walk stopped the moment its public name answered.
        let seen = d.seen();
        assert_eq!(seen.last().map(String::as_str), Some("https://203-0-113-9.h.plex.direct:31234"));
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
        let Resolved::Reached(roster) = resolve_roster(&a_two_server_account(), &|o| d.dial(o)) else {
            panic!("both servers answer")
        };

        assert_eq!(roster[0].origin_url, "http://192.168.0.10:32400");
        assert_eq!(roster[1].origin_url, "http://203.0.113.9:31234");
        // …and it is a parseable origin, so the registry gets one rather than the legacy fallback
        for s in &roster {
            let o = s.origin().expect("a reached entry is dialable");
            assert_eq!(o.base(), s.origin_url, "the stored string round-trips");
            assert!(!o.is_tls(), "these are the plaintext twins, and they answered");
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
        assert!(matches!(resolve_roster(&players, &|_| (0, Vec::new())), Resolved::NoServers));

        // servers that simply do not answer
        let silent = Dialled::new(vec![]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &|o| silent.dial(o)),
            Resolved::None { refused: false }
        ));

        // …and one that answers 401: something in front of it refuses unauthenticated requests,
        // which is not a network fault and must not be worded as one
        let refused = Dialled::new(vec![("192.168.0.10", 401, Vec::new()), ("203.0.113.9", 401, Vec::new())]);
        assert!(matches!(
            resolve_roster(&a_two_server_account(), &|o| refused.dial(o)),
            Resolved::None { refused: true }
        ));

        // a share that answers while OUR server is off still signs in — a friend's library beats
        // "no server found" — and it becomes the primary because it is the only thing there is
        let one = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
        let Resolved::Reached(roster) = resolve_roster(&a_two_server_account(), &|o| one.dial(o)) else {
            panic!("the share answered")
        };
        assert_eq!(roster.len(), 1);
        assert_eq!(primary_index(&roster), 0);
        assert!(!roster[0].owned, "the primary is a share here, and that is the point");
    }

    /// A roster entry in the **LEGACY shape** — no stored `origin`, which is what every session
    /// file on every television written before that field carries. `..Default::default()` is what
    /// leaves it empty, so these fixtures also stand as the compatibility case: everything they
    /// assert about registration and re-keying runs through `SourceRef::origin`'s fallback.
    fn source(machine_id: &str, owned: bool, token: &str) -> SourceRef {
        SourceRef {
            machine_id: machine_id.into(),
            name: machine_id.into(),
            shared_by: if owned { String::new() } else { "friend".into() },
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
        assert_eq!(next[1].shared_by, "friend", "everything but the token is carried over");
        assert_eq!(next[1].address, "10.0.0.1", "including the address discovery probed");

        // a resource that came back WITHOUT a token for this profile is not a re-key either
        let empty = vec![resource(r#"{"clientIdentifier":"ours","provides":"server","accessToken":""}"#)];
        assert!(retoken(&roster, &empty).is_empty());
        // and an entry with no identity cannot be re-keyed, and must never match by emptiness
        let anon = vec![source("", false, "old")];
        assert!(retoken(&anon, &[resource(r#"{"provides":"server","accessToken":"x"}"#)]).is_empty());
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

        assert!(reconcile_primary(&mut s, &[share.clone(), moved.clone()]), "the save is owed");
        assert_eq!((s.address.as_str(), s.port), ("192.168.0.42", 32400));
        assert_eq!(s.token, "tok-own2", "the grant came from the same answer as the address");
        assert_eq!(s.machine_id, "aaaa1111", "the identity is the KEY here, never something to rewrite");

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
            user: UserRef { uuid: uuid.into(), title: "stored".into(), token: "tok-user".into(), ..Default::default() },
            home_users: vec![
                session::HomeUserRef {
                    uuid: "u-adult".into(),
                    title: "Gleb".into(),
                    protected: true,
                    admin: true,
                    ..Default::default()
                },
                session::HomeUserRef { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() },
            ],
            ..Default::default()
        }
    }

    /// **BACK out of the BOOT picker must not hand over a PIN-protected profile**, which it did
    /// until 2026-08-21 and which is a privilege escalation rather than a rough edge: adult uses the
    /// app, child boots it, the who's-watching picker appears, BACK reinstates the adult's per-user
    /// token and enters Home as them. (From an open keypad it took two presses, the first closing
    /// the pad.) The PIN itself was always validated by plex.tv — the hole was entirely in this
    /// escape hatch, which reasons about "carry on as the profile I'm already signed in as" and is
    /// only true of the picker Home opens.
    ///
    /// So every row of the rule is graded here, and most of them are the ones that must NOT change:
    /// a boot picker over an unprotected profile still resumes (nothing is being bypassed), and
    /// *Change profile* and the sign-in picker still resume whatever they are over (you are already
    /// that profile / you just proved you hold the account).
    ///
    /// The last section is the SECOND road to the same escalation, and it survived the first fix: a
    /// sign-in abandoned at the picker persists a session that names no profile, whose token is the
    /// owner's, and the next boot raises a picker over exactly that.
    #[test]
    fn back_out_of_the_boot_picker_refuses_a_pin_protected_profile_and_nothing_else() {
        // `CTL` is a process global; hold the crate lock for the whole body and put it back after.
        let _g = crate::testlock::serial();

        // the rule itself, as a table
        assert!(!may_resume(Picker::Boot, true), "the escalation");
        assert!(may_resume(Picker::Boot, false));
        assert!(may_resume(Picker::ChangeProfile, true));
        assert!(may_resume(Picker::ChangeProfile, false));
        assert!(may_resume(Picker::SignedIn, true), "the account credential was just presented");
        assert!(may_resume(Picker::SignedIn, false));
        // and the DEFAULT is the strict one: it is read only where no picker named itself, and
        // "we cannot say who is asking" must not answer with the credentials.
        assert_eq!(Picker::default(), Picker::Boot);

        // …and that `cancel` is actually gated on it. A picker is up in each case, so the failure
        // being graded is a whole flow resolving to `Ready` with credentials armed for `take_ready`
        // — the phase alone is not the escalation, `apply_pending` is what installs them.
        let picker = |from: Picker| {
            with_ctl(|c| *c = Ctl { phase: Phase::Profiles, from, ..Ctl::default() });
        };

        picker(Picker::Boot);
        assert!(!resume_stored(signed_in_as("u-adult")), "BACK must not resume behind the PIN");
        assert_eq!(phase(), Phase::Profiles, "the picker stays up, and the key is swallowed");
        assert!(with_ctl(|c| !c.apply_pending), "no credentials are handed to the main loop");

        picker(Picker::Boot);
        assert!(resume_stored(signed_in_as("u-kid")), "an unprotected profile is not an escalation");
        assert_eq!(phase(), Phase::Ready);
        assert!(with_ctl(|c| c.apply_pending));

        picker(Picker::ChangeProfile);
        assert!(resume_stored(signed_in_as("u-adult")), "Home's picker backs out to the profile it was opened by");
        assert_eq!(phase(), Phase::Ready);
        assert!(with_ctl(|c| c.apply_pending));

        // the refusal that predates all of this: nothing usable behind the picker at all
        picker(Picker::ChangeProfile);
        assert!(!resume_stored(Session::default()));
        assert_eq!(phase(), Phase::Profiles);

        // **The second road, and the one that survived the first fix.** A sign-in ABANDONED at the
        // who's-watching picker persists the account token, the server and the roster with no
        // profile chosen (`login_thread` saves the moment they exist, so walking away does not cost
        // the sign-in). `pms_token()` on that file is the OWNER's server token and the roster is >1,
        // so the next boot raises a picker over it — where BACK was handing the owner's credentials
        // to whoever pressed it. The sign-in picker itself keeps resuming, because the account
        // credential was presented seconds ago.
        let mut unchosen = signed_in_as("u-adult");
        unchosen.user = UserRef::default();
        assert!(!unchosen.pms_token().is_empty(), "…and what it would have resumed on is the owner's");

        picker(Picker::Boot);
        assert!(!resume_stored(unchosen.clone()), "no profile chosen is not 'nothing to bypass'");
        assert_eq!(phase(), Phase::Profiles);
        assert!(with_ctl(|c| !c.apply_pending));

        picker(Picker::SignedIn);
        assert!(resume_stored(unchosen), "the sign-in's own picker is unchanged");
        assert_eq!(phase(), Phase::Ready);
        assert!(with_ctl(|c| c.apply_pending));

        with_ctl(|c| *c = Ctl::default());
    }
}
