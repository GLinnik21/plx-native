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
    session: Session,
    apply_pending: bool,
}

static CTL: Mutex<Option<Ctl>> = Mutex::new(None);

/// Append a line to the shared on-device event log (never a token — only ids/counts/status).
fn log(m: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("/tmp/poc-events.log") {
        let _ = writeln!(f, "{m}");
    }
}

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

/// Re-open the "who's watching" picker from an already-signed-in session (the Home profile menu's
/// "Change profile"). Reuses this session's cached roster when present; otherwise re-fetches it with
/// the stored account token. The caller routes to [`Phase::Profiles`].
pub fn start_switch() {
    let have = with_ctl(|c| {
        c.error.clear();
        c.phase = Phase::Profiles;
        !c.users.is_empty()
    });
    if have {
        return;
    }
    std::thread::spawn(|| {
        let sess = session::load();
        let ac = AccountClient::new(&sess.client_id, Some(&sess.account_token));
        let users: Vec<UserTile> = ac.home_users().unwrap_or_default().iter().map(UserTile::of).collect();
        log(&format!("auth: re-fetched roster n={}", users.len()));
        with_ctl(|c| {
            c.session = sess;
            c.users = users;
        });
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

    // 4) Plex Home roster → who's-watching, or straight in if there's a single user
    let users: Vec<UserTile> = ac.home_users().unwrap_or_default().iter().map(UserTile::of).collect();
    log(&format!("auth: home users n={}", users.len()));
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
    with_ctl(|c| c.phase = Phase::Switching);
    std::thread::spawn(move || {
        let ac = AccountClient::new(&cid, Some(&account_token));
        let u = match ac.switch_user(&tile.uuid, pin.as_deref()) {
            Some(u) if !u.auth_token.is_empty() => u,
            _ => {
                // 401 (wrong PIN) and transport errors are indistinguishable at this layer.
                log(&format!("auth: switch '{}' -> failed", tile.title));
                return with_ctl(|c| {
                    c.error = "Couldn't switch profile — check the PIN.".into();
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
