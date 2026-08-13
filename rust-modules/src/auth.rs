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

/// An OWNED server's `local` connection, else any server's — the live sign-in rule, pulled out of
/// [`discover_and_store`] so it can be graded on the host. `plex::probe` is the richer version of
/// this and supersedes it at step 4 of `docs/shared-servers.md`; until something calls that, THIS
/// is what every sign-in runs, so its two limits are the app's limits.
///
/// **A dialable address here means an IPv4 LITERAL on our own LAN**, and both halves are transport
/// facts rather than preferences:
///
/// * `stream.rs::http_open` parses the host as a dotted quad by hand (`AF_INET`, no DNS), so a v6
///   literal is not slower — it cannot be dialled at all. Since the roster query started sending
///   `includeIPv6=1`, plex.tv will happily offer one, and this chooser takes the FIRST match and
///   PERSISTS it: without the guard, a server advertising its LAN v6 ahead of its v4 signs in
///   "successfully" to an empty Home *and* writes that address to the session file, so every later
///   boot starts there too. `probe.rs` ranks v6 last for the same reason.
/// * `local` is required because a remote address needs DNS/TLS this socket does not have. Note
///   what §2(a) of the shared-servers note proves about that flag on a SHARED server: it means
///   "RFC1918", not "your LAN", so this rule reaching a share would pick the owner's `172.20.x.x`
///   and hang for 8 s. That is the trap `probe.rs` exists to close, and it is still open here.
fn choose_local_connection(
    resources: &[crate::plex::account::Resource],
) -> Option<(&crate::plex::account::Resource, &crate::plex::account::Connection)> {
    let find = |owned_only: bool| {
        resources
            .iter()
            .filter(|r| r.is_server() && (!owned_only || r.owned))
            .find_map(|r| {
                r.connections
                    .iter()
                    .find(|c| {
                        c.local && !c.relay && !c.address.is_empty() && !c.ipv6 && !c.address.contains(':')
                    })
                    .map(|c| (r, c))
            })
    };
    find(true).or_else(|| find(false))
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
    let (r, conn) = match choose_local_connection(&resources) {
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
    use crate::plex::account::Resource;

    fn roster(json: &str) -> Vec<Resource> {
        serde_json::from_str(json).expect("fixture parses")
    }

    /// The address this picks is PERSISTED, so picking an undialable one is not a slow sign-in — it
    /// is a permanently empty Home, on every boot after it too. `stream.rs` speaks IPv4 literals
    /// only, and the roster query asks plex.tv for `includeIPv6=1`, so a server listing its LAN v6
    /// FIRST is the shape that breaks: this chooser takes the first match. It must step past the v6
    /// to the v4 behind it.
    #[test]
    fn a_lan_ipv6_is_stepped_over_for_the_ipv4_behind_it() {
        let rs = roster(
            r#"[{"name":"mine","clientIdentifier":"aaaa1111","provides":"server","owned":true,
                 "connections":[
                   {"address":"2001:db8::1","port":32400,"local":true,"relay":false,"IPv6":true},
                   {"address":"192.168.0.10","port":32400,"local":true,"relay":false,"IPv6":false}]}]"#,
        );
        let (_, c) = super::choose_local_connection(&rs).expect("a dialable connection");
        assert_eq!(c.address, "192.168.0.10", "the v6 literal cannot be dialled by stream.rs at all");

        // …and a v6 whose flag is absent is caught by the address itself. The roster flags are
        // null-tolerant now, so `IPv6:false` on a colon-bearing address must not be believed.
        let rs = roster(
            r#"[{"name":"mine","clientIdentifier":"aaaa1111","provides":"server","owned":true,
                 "connections":[{"address":"fd00::5","port":32400,"local":true,"relay":false,"IPv6":false}]}]"#,
        );
        assert!(super::choose_local_connection(&rs).is_none(), "nothing dialable is None, not a v6");
    }

    /// The two passes, unchanged by the guard above: an owned server wins even when a shared one is
    /// listed first and looks perfectly good, and a relay is never a "local" connection. (What this
    /// rule still gets wrong on a SHARE — `local` meaning RFC1918 rather than "your LAN", §2(a) of
    /// `docs/shared-servers.md` — is `probe.rs`'s job and is not asserted here, because nothing
    /// calls probe yet and this is a pin of the LIVE behaviour.)
    #[test]
    fn an_owned_server_outranks_a_shared_one_and_a_relay_is_never_chosen() {
        let rs = roster(
            r#"[{"name":"theirs","clientIdentifier":"bbbb2222","provides":"server","owned":false,
                 "connections":[{"address":"172.20.4.7","port":32400,"local":true,"relay":false,"IPv6":false}]},
                {"name":"mine","clientIdentifier":"aaaa1111","provides":"server","owned":true,
                 "connections":[{"address":"192.168.0.10","port":32400,"local":true,"relay":false,"IPv6":false}]}]"#,
        );
        let (r, c) = super::choose_local_connection(&rs).expect("the owned server");
        assert_eq!((r.name.as_str(), c.address.as_str()), ("mine", "192.168.0.10"));

        let rs = roster(
            r#"[{"name":"relayed","clientIdentifier":"cccc3333","provides":"server","owned":true,
                 "connections":[{"address":"10.0.0.9","port":8443,"local":true,"relay":true,"IPv6":false}]}]"#,
        );
        assert!(super::choose_local_connection(&rs).is_none(), "a relay is not a local connection");
    }
}
