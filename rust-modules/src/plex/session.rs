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

/// Read the persisted session and nothing else — **no minting, no write.** For readers that merely
/// want to know what the session says (the account surfaces): [`load`]'s client-id minting means a
/// read can turn into a `save`, and `save` truncates in place, so a file that momentarily fails to
/// parse would be overwritten with a bare client_id — a silent sign-out. That is an acceptable
/// trade on the boot path, which must end up with an id; it is not one on a path a keypress can
/// reach. Falls back to the pre-relocation path (migration), same as `load`.
pub fn peek() -> Session {
    std::fs::read(AUTH_PATH)
        .ok()
        .or_else(|| std::fs::read(AUTH_PATH_OLD).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Load the persisted session, ensuring a stable `client_id` exists (generated + saved on first
/// boot). Never returns an error — a missing/corrupt file degrades to a fresh, logged-out session.
/// Falls back to the pre-relocation path once and re-saves at the new one (migration).
pub fn load() -> Session {
    let mut s: Session = peek();
    if s.client_id.is_empty() {
        s.client_id = new_client_id();
        save(&s);
    }
    s
}

/// Persist the session (best-effort; a write failure is non-fatal — we just re-login next boot).
///
/// **Credentials at rest: 0600, set in `open(2)`'s own mode argument — never create-then-chmod.**
/// This file holds the plex.tv account token, every per-user PMS access token and the Plex Home
/// roster, and it lives on a ROOTED TV whose `/media/developer` is world-readable — the same box
/// where `/tmp` is a dumping ground for dev triggers. `fs::write` creates with `0666 & !umask`
/// (0644 here), so the tokens were readable by every other uid on the device from the instant they
/// hit the disk. Passing the mode through `OpenOptionsExt` means the file never *exists* in a
/// permissive mode, which a chmod after the write cannot promise: the window between create and
/// chmod is exactly when the secret is already on disk.
///
/// `truncate(true)` keeps a shorter re-save from leaving the tail of the previous one behind, and
/// it also empties any pre-existing file *before* the `set_permissions` below — so tightening a
/// legacy 0644 session written by an older build (the mode above only applies on creation) can
/// never expose content either: at that moment the file is zero bytes.
pub fn save(s: &Session) {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let Ok(json) = serde_json::to_vec_pretty(s) else { return };
    let opened = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(AUTH_PATH);
    if let Ok(mut f) = opened {
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        let _ = f.write_all(&json);
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

// ---- What the account surfaces are allowed to SAY about the user ----

/// The account facts the UI may state — see [`Session::account`]. An account surface must word
/// itself from THIS, never from [`current`] alone: that profile is a bare `UserRef::default()` —
/// empty title, empty thumb — for every account **without Plex Home**, because auth's single-user
/// path enters Home without ever writing one. Reading that emptiness as "signed out" is how a
/// signed-in owner ends up being offered "Sign in".
///
/// Converted so far: `ui/account_menu.rs`. **Not yet: the Home profile chip** (`ui/widgets.rs`
/// `profile_chip`), which still labels itself off `title.is_empty()` and so still says "Sign in"
/// to that same owner — the remaining half of the bug, and a one-block change once this lands.
pub struct Account {
    /// **This device** holds a session: a plex.tv account token, or at least a server + PMS token
    /// it can stream on. The opposite of "offer them Sign in". Note it describes the session ON
    /// DISK, not the identity currently in use: an automated boot on `/tmp/plxnative-token` streams
    /// on an injected token yet still reports the stored account here — deliberately, because the
    /// stored account is exactly what a "Sign out" would clear.
    pub signed_in: bool,
    /// Profile switching is possible. It needs the **plex.tv account token**: both the Plex Home
    /// roster and the per-user tokens come from plex.tv, so a server-only session cannot switch
    /// (`auth::start_switch` refuses one outright). Deliberately NOT gated on the roster length —
    /// see `home_users`' note on why an empty roster means "unknown", not "there are none".
    pub can_switch: bool,
    /// Who we may say the user is: the active managed profile, else the account owner off the
    /// persisted roster. `None` = signed in but nameless (no roster has ever landed), which is a
    /// missing name and not a missing user — say "Account", never "Sign in".
    pub name: Option<String>,
}

impl Session {
    /// The account facts for the UI, from the persisted session plus the in-memory active profile
    /// (`active`, i.e. [`current`]). The profile is the better name once a managed user has been
    /// picked; the persisted roster's `admin` entry is what names an owner who has no Plex Home
    /// and therefore never got a profile written at all.
    ///
    /// **`home_users` being empty means "unknown", not "none".** It is only ever filled by a
    /// sign-in or a "Change profile", and a *failed* fetch persists an empty vec
    /// (`auth.rs`'s `home_users().unwrap_or_default()`), so "never fetched", "fetch failed" and
    /// "genuinely empty" are one value. Anything deciding on it must treat empty as "ask" — which
    /// is why [`Account::can_switch`] keeps the switch row: that row is what re-fetches the roster,
    /// and hiding it on an empty one would be a one-way door out of a Plex Home created later.
    pub fn account(&self, active: Option<&UserRef>) -> Account {
        let named = |t: &str| Some(t.to_string()).filter(|t| !t.is_empty());
        // the roster hop searches for a NAMED admin, then any named entry — a `find(admin)` whose
        // hit happens to carry an empty title must not swallow the answer sitting behind it, which
        // is the same shape of bug this whole function exists to fix.
        let roster = || {
            let named_admin = self.home_users.iter().find(|u| u.admin && !u.title.is_empty());
            named_admin.or_else(|| self.home_users.iter().find(|u| !u.title.is_empty())).map(|u| u.title.clone())
        };
        let name = active.and_then(|u| named(&u.title)).or_else(|| named(&self.user.title)).or_else(roster);
        Account {
            signed_in: !self.account_token.is_empty() || self.can_go_local(),
            can_switch: !self.account_token.is_empty(),
            name,
        }
    }
}
