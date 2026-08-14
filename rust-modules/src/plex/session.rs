//! Persisted login session — what makes the client **offline-first**. After the one-time online
//! login (account token → server discovery → profile switch), the chosen server's *local* address
//! and the profile's token are written here, so every later boot connects straight to the LAN
//! server over plain HTTP with no internet needed. Lives in the writable app dir (device-only;
//! never in the repo). The token fields are secrets — this file's contents are never logged.
//!
//! ## One server, and then the ROSTER
//!
//! [`Session::server`] is still the primary — the one address `can_go_local` runs on and the one
//! `app.rs` boots against — and it is untouched. Beside it, [`Session::sources`] records **every**
//! server discovery reached, ours and every share, each with its own address and its own
//! per-(user, server) token, because a shared server is a separate authority that answers 401 to
//! anybody else's credential (`docs/shared-servers.md` §2b). A single-server account writes one
//! entry there and behaves exactly as it always has.
//!
//! **Nothing here carries a timestamp**, deliberately: this TV's wall clock runs ~3 h skewed
//! (root `CLAUDE.md`), so a stored "last seen" would be a number that cannot be compared with
//! anything and would invite an expiry rule built on it.
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
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

/// Session file locations, best first — see [`crate::paths::session_candidates`] for why this is a
/// SEARCH ORDER rather than the single constant it used to be. The short version: webOS picks one
/// of two jail profiles by install prefix, and they disagree about which directories are writable,
/// so the one hardcoded path was correct under Developer Mode and did not exist under a Homebrew
/// Channel install — where `save()` then dropped the error and the user re-did the QR sign-in on
/// every boot, with a fresh `X-Plex-Client-Identifier` each time.
///
/// The first entry is still deliberately OUTSIDE the app install dir: appinstalld replaces
/// `applications/com.beb.plxnative/` wholesale on every ipk (re)install, which silently signed the
/// user out when the file lived there.
fn auth_paths() -> Vec<std::path::PathBuf> {
    crate::paths::session_candidates()
}

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
    ///
    /// Soft-parsed for the same reason [`Session::sources`] is: one managed user whose stored
    /// `thumb` came back as a JSON `null` would otherwise fail the whole `Session` and sign the
    /// device out on every boot, to fix an avatar.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub home_users: Vec<HomeUserRef>,
    /// **Every server this identity can browse**, as of the last successful discovery — ours and
    /// each share, best-address-first per entry. Additive: [`Session::server`] stays the primary,
    /// and this list holds it too (as the `owned` entry) so a reader needs only one surface.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) — a corrupt or unreadable entry costs that entry, never
    /// the `Session`, because failing the whole file here is a silent sign-out at every boot for
    /// a feature nobody has used yet.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub sources: Vec<SourceRef>,
    /// Which libraries the user chose to see **on Home**. Browsing is governed by the grant, not
    /// by this: pinning is the only *setting* of the three states a source has (granted / pinned /
    /// reachable — `docs/shared-servers.md` §6).
    ///
    /// **Empty means "never chosen", not "none pinned"** — the same trap `home_users` documents.
    /// A reader that finds it empty must fall back to its own default (our own server's
    /// libraries), never draw an empty Home.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub pinned: Vec<PinnedLib>,
    /// The search terms the user actually searched, **most recent first** — what the Search
    /// screen's empty-query state offers back (`crate::ui::search::recents`, which owns the cap,
    /// the de-duplication and the ordering; this is only where they rest).
    ///
    /// They live here rather than in a file of their own for one reason: they are the ACCOUNT's,
    /// and this is the file that is cleared on sign-out, so they are dropped with the credentials
    /// they belong to instead of being left behind for whoever signs in next.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) for the reason every list in this struct is: a hand-edited
    /// or half-written entry must cost that term and nothing more. Failing the `Session` over a
    /// search term would sign the device out on every boot.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub recent_searches: Vec<String>,
}

/// One persisted who's-watching tile (avatar + PIN flag; no tokens live here).
///
/// `#[serde(default)]` on the CONTAINER, so a missing field costs that field. Per-field it covered
/// only the two flags, which meant a tile written by a build that did not have `thumb` yet — or one
/// hand-edited on the TV — failed the whole `Session`, i.e. signed the device out. The same
/// reasoning applies to every struct in this file: it is a file we read on the boot path, and the
/// cost of one unexpected shape must never be the credentials.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct HomeUserRef {
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    pub protected: bool,
    pub admin: bool,
}

/// The PRIMARY server's coordinates — the one `can_go_local` boots on. `address`:`port` is reached
/// over plain HTTP by the existing PMS socket (offline-capable); `token` is that server's access
/// token (fallback when no managed-user token is set). Every server, including this one, is also
/// in [`Session::sources`].
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)] // a missing field costs that field, never the session — see [`HomeUserRef`]
pub struct ServerRef {
    pub name: String,
    pub machine_id: String,
    pub address: String,
    pub port: i64,
    pub token: String,
}

/// One server this identity can browse — our own or a friend's share. What discovery resolved:
/// the identity to key it on, the address that actually **answered**, and the credential that
/// server accepts.
///
/// Deliberately NOT `Debug`: `token` is a live per-(user, server) PMS access token, and a derived
/// `Debug` is exactly how a secret reaches a log by accident (`dev::DevServer` says the same).
/// [`SourceRef::describe`] is the only formatter, and it prints everything but the token.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct SourceRef {
    /// `machineIdentifier` — the ONLY stable identity, and the registry key. An address moves
    /// (LAN ↔ remote, DHCP, relay); this does not.
    pub machine_id: String,
    /// The machine name ("nas-home"). Settings surfaces only — a person is named by `shared_by`.
    pub name: String,
    /// The OWNER's plex.tv handle (`sourceTitle`), empty on our own server. The one string the
    /// browsing UI ever says about a share: "Shared by friend".
    pub shared_by: String,
    /// False ⇒ shared with us. A preference (ours sorts first, ours is `current`), never a wall.
    pub owned: bool,
    /// The address that answered `/identity` with the right `machineIdentifier` — not the first
    /// one advertised. A share's `local` address is the OWNER's LAN and is never this.
    pub address: String,
    pub port: i64,
    /// This identity's per-(user, server) `accessToken` for THIS server. A secret — never logged.
    /// Our own server's token gets a 401 from a share, which is why one token cannot serve both.
    pub token: String,
}

impl SourceRef {
    /// Everything about this source except the token, for the event log. The machine id is left
    /// out entirely — it is a permanent household fingerprint (`ui::stats`), and the event log is
    /// the file we ask users to send us.
    pub fn describe(&self) -> String {
        let who = if self.owned { "ours".to_string() } else { format!("shared by {}", self.shared_by) };
        format!("{:?} {}:{} ({who})", self.name, self.address, self.port)
    }
    /// Enough to dial: an address, a port, and the credential that server accepts.
    pub fn usable(&self) -> bool {
        !self.address.is_empty() && self.port > 0 && !self.token.is_empty()
    }
}

/// One library the user pinned to Home, named the only way a library CAN be named across two
/// servers: the server's machine id plus that server's own section key. Section keys are
/// server-local integers starting at 1 — both servers in the measured pair have a section `1`
/// (`docs/shared-servers.md` §2), so a bare key identifies nothing.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct PinnedLib {
    pub machine_id: String,
    pub key: i64,
}

/// The last-selected Plex Home user. `token` is the per-user token PMS scopes watch state by — it
/// keeps working against the LAN server offline once cached here.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)] // a missing field costs that field, never the session — see [`HomeUserRef`]
pub struct UserRef {
    pub id: i64,
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    pub token: String,
}

/// A list that degrades **element by element** instead of taking the whole [`Session`] with it.
///
/// `#[serde(default)]` covers a field that is ABSENT. It does not cover one that is present and
/// the wrong shape — a `null`, a string where an array belongs, one entry whose `port` was
/// hand-edited to `"32400"` — and any of those fails the enclosing struct. For a `Session` that
/// failure is not "the roster is empty": [`peek`] then finds no candidate that parses, `load`
/// mints a fresh `client_id`, and the user is signed out and re-scanning a QR code on every boot,
/// for a stale list nothing had read yet.
///
/// So: decode to a `Value` (which for JSON can only fail on input the whole file would fail on),
/// keep the entries that are the right shape, and drop the ones that are not.
fn de_soft_vec<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let Ok(v) = serde_json::Value::deserialize(d) else { return Ok(Vec::new()) };
    Ok(match v {
        serde_json::Value::Array(items) => {
            items.into_iter().filter_map(|it| serde_json::from_value::<T>(it).ok()).collect()
        }
        // a null, an object, a string: not a list, so there is no list. Not an error.
        _ => Vec::new(),
    })
}

impl Session {
    /// True once we have a LAN server + a usable PMS token — i.e. we can run offline.
    pub fn can_go_local(&self) -> bool {
        !self.server.address.is_empty() && !self.pms_token().is_empty()
    }
    /// The token PMS calls use: the switched managed-user token if we have one, else the server
    /// access token (owner).
    ///
    /// **This is the PRIMARY server's token and no other's.** A share is a separate authority and
    /// answers 401 to it; its own credential is [`SourceRef::token`], keyed by machine id.
    pub fn pms_token(&self) -> &str {
        if !self.user.token.is_empty() {
            &self.user.token
        } else {
            &self.server.token
        }
    }

    /// One source by `machineIdentifier` — the only key that identifies a server.
    pub fn source(&self, machine_id: &str) -> Option<&SourceRef> {
        if machine_id.is_empty() {
            return None; // an unknown id must not match the entries that have none either
        }
        self.sources.iter().find(|s| s.machine_id == machine_id)
    }
    /// Our own server's entry in the roster, if discovery reached one.
    pub fn owned_source(&self) -> Option<&SourceRef> {
        self.sources.iter().find(|s| s.owned)
    }
    /// The shares — every source that is not ours, in discovery order.
    pub fn shared_sources(&self) -> impl Iterator<Item = &SourceRef> {
        self.sources.iter().filter(|s| !s.owned)
    }
    /// Has the user pinned this library to Home? See [`Session::pinned`] on why an EMPTY list is
    /// "never chosen" and must not be read as "nothing is pinned".
    pub fn is_pinned(&self, machine_id: &str, key: i64) -> bool {
        self.pinned.iter().any(|p| p.machine_id == machine_id && p.key == key)
    }
}

/// Read the persisted session and nothing else — **no minting, no write.** For readers that merely
/// want to know what the session says (the account surfaces): [`load`]'s client-id minting means a
/// read can turn into a `save`, and `save` truncates in place, so a file that momentarily fails to
/// parse would be overwritten with a bare client_id — a silent sign-out. That is an acceptable
/// trade on the boot path, which must end up with an id; it is not one on a path a keypress can
/// reach. Falls back to the pre-relocation path (migration), same as `load`.
pub fn peek() -> Session {
    // First candidate that both EXISTS and PARSES wins. Parse is part of the test on purpose: a
    // half-written file at a preferred location must not shadow a good one further down the list.
    auth_paths()
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .find_map(|b| serde_json::from_slice(&b).ok())
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
    // Try each candidate; the first that accepts the write wins. A total failure is still
    // non-fatal — but it is LOGGED, because the symptom (sign in again, every boot, forever) is
    // otherwise indistinguishable from a server-side auth problem and impossible to report.
    for path in auth_paths() {
        let opened = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path);
        if let Ok(mut f) = opened {
            let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
            if f.write_all(&json).is_ok() {
                return;
            }
        }
    }
    crate::log("session: could not persist to ANY candidate path — login will not survive a reboot");
}

/// Clear the persisted session (sign-out) — removes the file; a fresh `client_id` is minted next
/// load. The old-path copy goes too, or the migration fallback would resurrect the stale session.
pub fn clear() {
    // Every candidate, not just the one we happen to write today: leaving a copy at any other
    // location would let `peek`'s search resurrect the stale session on the next boot.
    for path in auth_paths() {
        let _ = std::fs::remove_file(path);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The file a signed-in device holds today, once discovery has reached two servers. Written
    /// as literal JSON rather than by serialising a `Session`, because the thing under test is
    /// what happens when the bytes on disk are not what this build expects.
    fn two_server_json() -> &'static str {
        r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                      "port":32400,"token":"tok-own"},
            "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
            "home_users":[{"uuid":"u-7","title":"Gleb","thumb":"","protected":false,"admin":true}],
            "sources":[
              {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
               "address":"192.168.0.10","port":32400,"token":"tok-own"},
              {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
               "address":"203.0.113.9","port":31234,"token":"tok-share"}],
            "pinned":[{"machine_id":"bbbb2222","key":1}]}"#
    }

    /// The roster survives a write/read cycle intact — including the two facts that make a share
    /// usable at all: its OWN address (never the owner's LAN one) and its OWN token.
    #[test]
    fn the_roster_round_trips_through_the_session_file_format() {
        let s: Session = serde_json::from_str(two_server_json()).expect("a normal session parses");
        let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");

        assert_eq!(s.sources.len(), 2);
        let own = s.owned_source().expect("our own server is in the roster");
        assert_eq!((own.machine_id.as_str(), own.address.as_str()), ("aaaa1111", "192.168.0.10"));
        assert!(own.shared_by.is_empty(), "an owned server has no owner to name");

        let share = s.source("bbbb2222").expect("keyed by machineIdentifier, not by index");
        assert_eq!((share.address.as_str(), share.port), ("203.0.113.9", 31234));
        assert_eq!(share.token, "tok-share", "the sharing grant, not the account token");
        assert_eq!(share.shared_by, "friend");
        assert!(!share.owned && share.usable());
        assert_eq!(s.shared_sources().count(), 1);

        assert!(s.is_pinned("bbbb2222", 1));
        // section keys are server-local: both servers have a section 1, so the key alone matches
        // nothing on its own
        assert!(!s.is_pinned("aaaa1111", 1), "a pin names a server AND a key");
        assert!(s.source("").is_none() && s.source("nope").is_none());

        // and the token is not printable by accident — `describe` is the only formatter there is
        assert!(!share.describe().contains("tok-share"), "{}", share.describe());
        assert!(share.describe().contains("friend") && share.describe().contains("203.0.113.9"));
    }

    /// **The sign-out bug this list is shaped to avoid.** A `sources` array that is corrupt, the
    /// wrong type, or absent entirely must cost the roster and nothing else — `#[serde(default)]`
    /// alone does not do that, because it covers an ABSENT field and not a present, malformed one,
    /// and the failure mode is not "an empty roster" but a `Session` that will not parse: no
    /// account token, no server, a freshly minted client id, and a QR code to scan on every boot.
    #[test]
    fn a_corrupt_or_absent_roster_never_costs_the_session() {
        // one entry with a hand-mangled port, beside a perfectly good one
        let mixed = r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"m","machine_id":"aaaa1111","address":"192.168.0.10","port":32400,"token":"t"},
            "sources":[{"machine_id":"aaaa1111","port":{"oops":true}},
                       {"machine_id":"bbbb2222","name":"nas-home","owned":false,
                        "address":"203.0.113.9","port":31234,"token":"tok-share"}],
            "pinned":"not a list"}"#;
        let s: Session = serde_json::from_str(mixed).expect("a bad entry must not fail the file");
        assert_eq!(s.account_token, "acct", "the credentials are still here");
        assert!(s.can_go_local(), "and the device can still stream");
        assert_eq!(s.sources.len(), 1, "the malformed entry dropped, the good one landed");
        assert_eq!(s.sources[0].machine_id, "bbbb2222");
        assert!(s.pinned.is_empty(), "a string where a list belongs is no list, not an error");

        // the whole field as an explicit null, and the whole field missing (every session file
        // written before this landed) — both are simply a session with no roster yet
        for json in [
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"},"sources":null}"#,
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"}}"#,
        ] {
            let s: Session = serde_json::from_str(json).expect("null and absent both parse");
            assert!(s.sources.is_empty() && s.pinned.is_empty());
            assert!(s.can_go_local(), "the primary server is what boot runs on, roster or not");
        }
    }

    /// One server must behave exactly as it did before the roster existed: the primary
    /// `server`/`user` pair is what `can_go_local` and `pms_token` read, and the roster is a
    /// record beside it, never a second source of truth that could disagree.
    #[test]
    fn a_single_server_session_behaves_as_it_always_has() {
        let mut s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                          "port":32400,"token":"tok-own"},
                "sources":[{"machine_id":"aaaa1111","name":"Mac mini","owned":true,
                            "address":"192.168.0.10","port":32400,"token":"tok-own"}]}"#,
        )
        .unwrap();
        assert!(s.can_go_local());
        assert_eq!(s.pms_token(), "tok-own", "no managed user picked yet → the server token");
        s.user.token = "tok-user".into();
        assert_eq!(s.pms_token(), "tok-user", "a switched profile's token wins, as before");
        // the roster agrees with the primary rather than competing with it
        assert_eq!(s.owned_source().map(|x| x.address.as_str()), Some(s.server.address.as_str()));
        assert_eq!(s.shared_sources().count(), 0);
        assert!(s.account(None).signed_in && s.account(None).can_switch);
    }

    /// The Search screen's recent terms are ordinary session content: they survive a write/read
    /// cycle in order, including the non-ASCII ones this household actually searches.
    #[test]
    fn the_recent_search_terms_round_trip_through_the_session_file_format() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","recent_searches":["wallace","Гладиатор","the curse"]}"#,
        )
        .expect("a session carrying terms parses");
        let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
        assert_eq!(s.recent_searches, ["wallace", "Гладиатор", "the curse"], "most recent first, in order");

        // absent entirely — every session file written before this landed
        let s: Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
        assert!(s.recent_searches.is_empty());
    }

    /// And they degrade the same way every other list here does: one malformed term costs that
    /// term, never the credentials sitting beside it. A search term must never be able to sign the
    /// device out.
    #[test]
    fn a_corrupt_search_term_costs_that_term_and_not_the_session() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"address":"192.168.0.10","port":32400,"token":"t"},
                "recent_searches":["wallace",null,42,{"oops":true},"gromit"]}"#,
        )
        .expect("a bad term must not fail the file");
        assert_eq!(s.recent_searches, ["wallace", "gromit"], "the three bad entries dropped");
        assert_eq!(s.account_token, "acct");
        assert!(s.can_go_local(), "and the device can still stream");

        // the whole field the wrong type is no list, not an error
        let s: Session = serde_json::from_str(r#"{"client_id":"c","recent_searches":"wallace"}"#)
            .expect("a string where a list belongs parses");
        assert!(s.recent_searches.is_empty());
    }

    /// The roster's own leniency must not weaken the roster the picker draws from: a managed user
    /// whose stored `thumb` is a `null` costs that user, not the session.
    #[test]
    fn a_malformed_home_user_costs_that_tile_and_not_the_session() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"c","home_users":[{"uuid":"a","title":"A","thumb":null},
                                              {"uuid":"b","title":"B","thumb":"","admin":true}]}"#,
        )
        .expect("one bad tile must not fail the file");
        assert_eq!(s.home_users.len(), 1);
        assert_eq!(s.account(None).name.as_deref(), Some("B"));
    }
}
