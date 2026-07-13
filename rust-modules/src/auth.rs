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
use crate::plex::account::{AccountClient, HomeUser};
use crate::plex::session::{self, ServerRef, Session, UserRef};
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
    std::thread::spawn(login_thread);
}

/// Retry after an [`Phase::Error`] — same as a fresh login.
pub fn retry() {
    start_login();
}

/// Cancel the flow (BACK on the login screen) → back to [`Phase::Idle`].
pub fn cancel() {
    with_ctl(|c| *c = Ctl::default());
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
pub fn take_ready() -> Option<ReadyCreds> {
    with_ctl(|c| {
        if c.phase == Phase::Ready && c.apply_pending {
            c.apply_pending = false;
            session::save(&c.session);
            session::set_current(Some(c.session.user.clone())); // drives the Home profile chip
            Some(ReadyCreds {
                host: c.session.server.address.clone(),
                port: c.session.server.port as i32,
                token: c.session.pms_token().to_owned(),
            })
        } else {
            None
        }
    })
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
    with_ctl(|c| {
        c.error.clear();
        if c.users.is_empty() {
            c.users = sess.home_users.iter().map(UserTile::of_ref).collect();
        }
        c.session = sess;
        c.phase = Phase::Profiles;
    });
    std::thread::spawn(|| {
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
    log(&format!("auth: pin created id={} code={} (waiting for authorization)", pin.id, pin.code));
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

    // 3) discover the LAN server
    with_ctl(|c| {
        c.session.account_token = token.clone();
        c.phase = Phase::Discovering;
    });
    let ac = AccountClient::new(&cid, Some(&token));
    if !discover_and_store(&ac) {
        return set_error("No local Plex server found on this network.");
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

/// Pick an OWNED server with a `local` connection (offline-capable, numeric address the plain-HTTP
/// PMS socket can reach), else any server with one, and store it in the working session. A
/// remote-only server is unusable here — the PMS socket does no DNS/TLS — so we require a local one.
fn discover_and_store(ac: &AccountClient) -> bool {
    let resources = match ac.resources() {
        Some(r) => r,
        None => {
            log("auth: resources request FAILED (no response/deser)");
            return false;
        }
    };
    let servers = resources.iter().filter(|r| r.is_server()).count();
    log(&format!("auth: resources n={} servers={}", resources.len(), servers));
    let find = |owned_only: bool| {
        resources
            .iter()
            .filter(|r| r.is_server() && (!owned_only || r.owned))
            .find_map(|r| {
                r.connections
                    .iter()
                    .find(|c| c.local && !c.relay && !c.address.is_empty())
                    .map(|c| (r, c))
            })
    };
    let (r, conn) = match find(true).or_else(|| find(false)) {
        Some(x) => x,
        None => {
            log("auth: no server with a local connection (remote-only can't be reached)");
            return false;
        }
    };
    log(&format!("auth: chose server '{}' {}:{}", r.name, conn.address, conn.port));
    with_ctl(|c| {
        c.session.server = ServerRef {
            name: r.name.clone(),
            machine_id: r.client_identifier.clone(),
            address: conn.address.clone(),
            port: if conn.port != 0 { conn.port } else { 32400 },
            token: r.access_token.clone(),
        };
    });
    true
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
    std::thread::spawn(move || {
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
        let mid = with_ctl(|c| c.session.server.machine_id.clone());
        let stok = AccountClient::new(&cid, Some(&u.auth_token))
            .resources()
            .and_then(|rs| {
                rs.into_iter()
                    .find(|r| r.is_server() && (mid.is_empty() || r.client_identifier == mid))
                    .map(|r| r.access_token)
            })
            .filter(|t| !t.is_empty());
        match stok {
            Some(token) => {
                log(&format!("auth: switch '{}' -> ok (per-user server token)", tile.title));
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
}

// ---- helpers ----

fn set_error(msg: &str) {
    log(&format!("auth: ERROR {msg}"));
    with_ctl(|c| {
        c.error = msg.to_owned();
        c.phase = Phase::Error;
    });
}
