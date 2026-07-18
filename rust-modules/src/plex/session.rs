//! Persisted login session — what makes the client **offline-first**. After the one-time online
//! login (account token → server discovery → profile switch), the chosen server's *local* address
//! and the profile's token are written here, so every later boot connects straight to the LAN
//! server over plain HTTP with no internet needed. Lives in the writable app dir (device-only;
//! never in the repo). The token fields are secrets — this file's contents are never logged.
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// The signed-in profile, in-memory for the UI (the Home profile chip reads this). Set by the boot
/// gate (from the stored session) and on every profile switch, so it survives an offline boot.
static CURRENT: Mutex<Option<UserRef>> = Mutex::new(None);
/// Bumped on every [`set_current`]; per-frame readers (the Home profile chip) snapshot by
/// generation instead of re-cloning the UserRef every frame.
static CURRENT_GEN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Install the active profile for the UI (or clear it on sign-out with `None`).
pub fn set_current(u: Option<UserRef>) {
    if let Ok(mut g) = CURRENT.lock() {
        *g = u;
    }
    CURRENT_GEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}
/// The active profile (name + avatar), if any. Empty title = the owner with no Plex Home selection.
pub fn current() -> Option<UserRef> {
    CURRENT.lock().ok().and_then(|g| g.clone())
}
/// The profile generation (see [`set_current`]).
pub fn current_gen() -> u32 {
    CURRENT_GEN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Session file — on the dev partition but OUTSIDE the app install dir: appinstalld replaces
/// `applications/com.beb.plxnative/` wholesale on every ipk (re)install, which silently signed the
/// user out when the file lived there. `/media/developer/` itself survives reinstalls.
const AUTH_PATH: &str = "/media/developer/com.beb.plxnative-auth.json";
/// Pre-relocation path (inside the app dir) — read once as a migration fallback.
const AUTH_PATH_OLD: &str = "/media/developer/apps/usr/palm/applications/com.beb.plxnative/auth.json";

/// The full persisted session. Empty fields mean "not logged in yet" for that stage.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    /// Stable `X-Plex-Client-Identifier` — generated once, reused forever (plex.tv binds the pin
    /// and the authorized-device entry to it).
    pub client_id: String,
    /// plex.tv account token (for online: re-discovery, home-users, switch). Not used for PMS.
    #[serde(default)]
    pub account_token: String,
    #[serde(default)]
    pub server: ServerRef,
    #[serde(default)]
    pub user: UserRef,
    /// The Plex Home roster as of the last successful fetch — lets the who's-watching picker
    /// render instantly on every boot (and offline) instead of waiting on a plex.tv round-trip.
    #[serde(default)]
    pub home_users: Vec<HomeUserRef>,
}

/// One persisted who's-watching tile (avatar + PIN flag; no tokens live here).
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct HomeUserRef {
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    #[serde(default)]
    pub protected: bool,
    #[serde(default)]
    pub admin: bool,
}

/// The chosen server's LAN coordinates. `address`:`port` is reached over plain HTTP by the existing
/// PMS socket (offline-capable); `token` is the owner's server access token (fallback when no
/// managed-user token is set).
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct ServerRef {
    pub name: String,
    pub machine_id: String,
    pub address: String,
    pub port: i64,
    pub token: String,
}

/// The last-selected Plex Home user. `token` is the per-user token PMS scopes watch state by — it
/// keeps working against the LAN server offline once cached here.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct UserRef {
    pub id: i64,
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    pub token: String,
}

impl Session {
    /// True once we have a LAN server + a usable PMS token — i.e. we can run offline.
    pub fn can_go_local(&self) -> bool {
        !self.server.address.is_empty() && !self.pms_token().is_empty()
    }
    /// The token PMS calls use: the switched managed-user token if we have one, else the server
    /// access token (owner).
    pub fn pms_token(&self) -> &str {
        if !self.user.token.is_empty() {
            &self.user.token
        } else {
            &self.server.token
        }
    }
}

/// Load the persisted session, ensuring a stable `client_id` exists (generated + saved on first
/// boot). Never returns an error — a missing/corrupt file degrades to a fresh, logged-out session.
/// Falls back to the pre-relocation path once and re-saves at the new one (migration).
pub fn load() -> Session {
    let mut s: Session = std::fs::read(AUTH_PATH)
        .ok()
        .or_else(|| std::fs::read(AUTH_PATH_OLD).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    if s.client_id.is_empty() {
        s.client_id = new_client_id();
        save(&s);
    }
    s
}

/// Persist the session (best-effort; a write failure is non-fatal — we just re-login next boot).
pub fn save(s: &Session) {
    if let Ok(json) = serde_json::to_vec_pretty(s) {
        let _ = std::fs::write(AUTH_PATH, json);
    }
}

/// Clear the persisted session (sign-out) — removes the file; a fresh `client_id` is minted next
/// load. The old-path copy goes too, or the migration fallback would resurrect the stale session.
pub fn clear() {
    let _ = std::fs::remove_file(AUTH_PATH);
    let _ = std::fs::remove_file(AUTH_PATH_OLD);
}

/// A v4-ish UUID from `/dev/urandom` (no `uuid` crate). Only uniqueness/stability matter — plex.tv
/// just needs a value it can key the device on.
fn new_client_id() -> String {
    use std::io::Read;
    let mut b = [0u8; 16];
    // bounded read — /dev/urandom is a char device with no EOF, so read_exact (not fs::read).
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b[6] = (b[6] & 0x0f) | 0x40; // version 4
    b[8] = (b[8] & 0x3f) | 0x80; // variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}
