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
#[cfg(not(test))]
fn auth_paths() -> Vec<std::path::PathBuf> {
    crate::paths::session_candidates()
}

/// The test build's [`auth_paths`]: the real search order until a test redirects it to a file of
/// its own (see `tests::TempSession`). A `#[cfg(test)]` global, so a shipped binary has neither the
/// static nor the branch — the file this module writes on a television is decided by `paths.rs` and
/// by nothing else.
///
/// It exists because there is no other way to exercise the writing half at all: every candidate
/// `paths.rs` offers is either a device path that does not exist on the dev Mac or — for
/// `in_app_dir` — the directory the test binary itself is running from, which is a real writable
/// path, so a careless test would leave a credentials-shaped file in `target/`.
#[cfg(test)]
static TEST_FILE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

#[cfg(test)]
fn auth_paths() -> Vec<std::path::PathBuf> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(p) => vec![p],
        None => crate::paths::session_candidates(),
    }
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
    /// The search terms actually searched, most recent first — what the Search screen's
    /// empty-query state offers back (`crate::ui::search::recents` owns the cap, the
    /// de-duplication and the ordering; this is only where they rest).
    ///
    /// **Keyed by PROFILE, and that is the whole point of the shape.** They lived here as a bare
    /// `Vec<String>` for one commit, which made them the account's rather than the person's — so
    /// after a Plex Home switch the next person's empty search screen offered back what the
    /// previous one had looked for. A search history is about as personal as watch state, which
    /// this product already scopes per user, and a shared television is exactly where that
    /// matters.
    ///
    /// Clearing on a switch would also have fixed the leak, and is the wrong fix: it costs you
    /// your own history every time you hand the remote over and take it back.
    ///
    /// They live in this file rather than one of their own because it is the file cleared on
    /// sign-out, so they go with the credentials they belong to instead of being left for whoever
    /// signs in next.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) for the reason every list in this struct is: a hand-edited
    /// or half-written entry must cost that entry and nothing more. Failing the `Session` over a
    /// search term would sign the device out on every boot.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub recent_searches: Vec<RecentSearches>,
}

/// One profile's search history.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct RecentSearches {
    /// The Plex Home user's `uuid`, or **empty for the account owner** with no Home selection —
    /// the same "empty means the owner" convention [`SourceRef`] uses for a handle. `uuid` and not
    /// `id`, because it is the identity that survives a roster refetch.
    pub user: String,
    pub terms: Vec<String>,
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

    /// One profile's search terms — empty for a profile that has never searched, which is the same
    /// answer as "never chosen" and needs no distinction here.
    pub fn recents_for(&self, user: &str) -> &[String] {
        self.recent_searches.iter().find(|r| r.user == user).map(|r| &r.terms[..]).unwrap_or(&[])
    }

    /// Replace one profile's terms, leaving every OTHER profile's alone. That last part is the
    /// reason this is a method rather than a field assignment at the call site: the writer holds a
    /// whole `Session` and the obvious `Session { recent_searches: mine, ..s }` would silently
    /// delete everybody else's history.
    pub fn set_recents_for(&mut self, user: &str, terms: Vec<String>) {
        if let Some(r) = self.recent_searches.iter_mut().find(|r| r.user == user) {
            r.terms = terms;
        } else if !terms.is_empty() {
            self.recent_searches.push(RecentSearches { user: user.to_string(), terms });
        }
    }
}

/// Which profile's history is in play: the active Plex Home user's `uuid`, or `""` for the owner
/// with no Home selection. One accessor, so the reader and the writer cannot key on different
/// things — which would look exactly like the leak this scoping exists to prevent.
pub fn current_profile_key() -> String {
    current().map(|u| u.uuid).unwrap_or_default()
}

/// **The one lock this file has**, and the only authority over it. Every public entry point in
/// this module takes it, so a read-modify-write held across [`update`] is atomic against every
/// other writer there is: the server-roster worker (`auth::refresh_roster_online`), the
/// who's-watching roster worker (`auth::start_switch`), the profile-switch and sign-in saves on
/// the main thread (`auth::take_ready`, `auth`'s login thread), and the search-recents flush
/// worker (`ui::search::recents`).
///
/// They were all unsynchronized — `recents` kept a `WRITING` mutex, which serialized recents
/// against recents and against nothing else, and no `auth` writer took anything at all. Two
/// failures came of it, both silent and both read by the user as something else entirely:
///
/// * a **lost update**. The roster worker re-reads the file ("a profile pick may have landed
///   meanwhile" — its own comment), the pick lands *after* that read, and the worker's save puts
///   the pre-switch profile back. The next boot resumes as the wrong person, with that person's
///   watch state, which reads as a server problem.
/// * a **torn file**. `save` truncated in place, so two interleaved writes produced JSON that
///   [`peek`] cannot parse — and an unparseable session file is not "a stale roster", it is no
///   `client_id`, no token and a QR code on the next boot. A silent sign-out, caused by a search
///   term landing at the same moment as a roster refresh.
///
/// The lock closes the second only together with the atomic write in [`write_atomic`]: one
/// process's threads are serialized here, but a reader outside this module (or a crash mid-write)
/// still sees whatever is on disk, and only a rename can promise that is a whole file.
///
/// **Not reentrant** — a plain `Mutex`. Nothing called from inside [`update`]'s closure may call
/// back into this module.
///
/// It is held across the whole write, [`write_atomic`]'s `sync_all` included, so a reader that
/// takes it can be parked for as long as the flash takes. That is affordable because of who the
/// readers are — a keypress (`ui::account_menu::open`), a boot, and one read-out that was already
/// doing an `fs::read` per frame (`ui::library`'s failed-source labels). **Do not add a per-frame
/// reader of this file**; the answer for that is a snapshot keyed on something cheap, the way
/// `ui::search::recents` caches by [`current_gen`].
static IO: Mutex<()> = Mutex::new(());

fn io() -> std::sync::MutexGuard<'static, ()> {
    // Poison is stepped over: a panic in one writer must not turn every later save into a panic of
    // its own, which on this path would mean losing the credentials rather than a stale file.
    IO.lock().unwrap_or_else(|e| e.into_inner())
}

/// Read the persisted session and nothing else — **no minting, no write.** For readers that merely
/// want to know what the session says (the account surfaces): [`load`]'s client-id minting means a
/// read can turn into a `save`, so a file that momentarily fails to parse would be overwritten with
/// a bare client_id — a silent sign-out. That is an acceptable trade on the boot path, which must
/// end up with an id; it is not one on a path a keypress can reach. Falls back to the
/// pre-relocation path (migration), same as `load`.
pub fn peek() -> Session {
    let _io = io();
    peek_locked()
}

/// [`peek`] with the lock already held — the read half every entry point here shares.
fn peek_locked() -> Session {
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
    let _io = io();
    let mut s: Session = peek_locked();
    if s.client_id.is_empty() {
        s.client_id = new_client_id();
        save_locked(&s);
    }
    s
}

/// **One read-modify-write of the session file, under [`IO`], as a single atomic step.** This is
/// the door for anything that changes PART of the file — the roster, the search terms — and the
/// only way to write one without racing the other writers.
///
/// `edit` is handed what is on disk *right now* and answers with what should replace it, or `None`
/// to leave the file exactly as it is. Returns whether anything was written. The closure runs with
/// the lock held, so it must be quick and it must not call back into this module (see [`IO`]).
///
/// **A file with no `client_id` refuses the cycle before `edit` ever runs.** [`peek_locked`] hands
/// back a default `Session` both for "no file yet" and for "the file did not parse", and writing
/// one field onto that default would truncate a live session — the silent sign-out again, this
/// time caused by the fix for it. `client_id` is minted once by [`load`] on the boot path and is
/// never empty afterwards, so it is exactly the test for "something real came back". A caller with
/// no session on disk simply keeps its change in memory for the run, which is what both of today's
/// callers already wanted.
pub fn update(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    let _io = io();
    let cur = peek_locked();
    if cur.client_id.is_empty() {
        return false;
    }
    match edit(&cur) {
        Some(next) => {
            save_locked(&next);
            true
        }
        None => false,
    }
}

/// Persist the session (best-effort; a write failure is non-fatal — we just re-login next boot).
///
/// **A whole-file REPLACE.** Use it only where the caller genuinely owns the entire file — the
/// sign-in flow and the profile switch, which built their `Session` from this same file moments
/// earlier. Anything changing one field of a file somebody else also writes must go through
/// [`update`], or it overwrites their change with whatever it last read.
///
/// **Credentials at rest: 0600, set in `open(2)`'s own mode argument — never create-then-chmod.**
/// This file holds the plex.tv account token, every per-user PMS access token and the Plex Home
/// roster, and it lives on a ROOTED TV whose `/media/developer` is world-readable — the same box
/// where `/tmp` is a dumping ground for dev triggers. `fs::write` creates with `0666 & !umask`
/// (0644 here), so the tokens were readable by every other uid on the device from the instant they
/// hit the disk. Passing the mode through `OpenOptionsExt` means the file never *exists* in a
/// permissive mode, which a chmod after the write cannot promise: the window between create and
/// chmod is exactly when the secret is already on disk.
pub fn save(s: &Session) {
    let _io = io();
    save_locked(s);
}

/// [`save`] with the lock already held.
fn save_locked(s: &Session) {
    let Ok(json) = serde_json::to_vec_pretty(s) else { return };
    // Try each candidate; the first that accepts the write wins. A total failure is still
    // non-fatal — but it is LOGGED, because the symptom (sign in again, every boot, forever) is
    // otherwise indistinguishable from a server-side auth problem and impossible to report.
    for path in auth_paths() {
        if write_atomic(&path, &json) {
            return;
        }
    }
    crate::log("session: could not persist to ANY candidate path — login will not survive a reboot");
}

/// Write `json` to `path` so that whatever reads it sees the WHOLE previous file or the WHOLE new
/// one — never a truncated one, and never bytes of both. The `plxnative.new` → `mv` dance the
/// Makefile's deploy does, for the same reason and against a worse loss: the file being replaced
/// here is the credentials.
///
/// The old `O_TRUNC` in place had two windows, and the second is the one that took the file. A
/// reader between the truncate and the `write_all` sees zero bytes; a power cut or a kill in that
/// same gap leaves zero bytes *on disk*, and `peek` reads both as "no session" — sign in again.
///
/// The tmp file is a **sibling**, named off the resolved path rather than picked. `rename(2)` is
/// only atomic within one filesystem, and the webOS jail's writable directories are separate mounts
/// (`/media/developer`, `/media/internal`, the app dir — see [`auth_paths`]); a tmp under `/tmp`
/// would demote this to a cross-device copy, i.e. exactly the truncate-in-place it replaces. One
/// fixed `.tmp` suffix is enough because [`IO`] means only one writer is ever in here, and a stale
/// one left by a crash is overwritten rather than read — it is not one of [`auth_paths`]'s
/// candidates, so `peek`'s search cannot pick it up.
///
/// `sync_all` before the rename is what makes the promise survive the plug being pulled, which on a
/// television is an ordinary way to end a session: without it the rename can be visible while the
/// data behind it is not, and the file that comes back is the empty one. It costs a flush of a
/// couple of kilobytes on a path that runs at sign-in, at a profile switch, at a roster change and
/// at a committed search term — never per frame.
///
/// The 0600 mode is [`save`]'s rule applied one file earlier: the secret must never *exist* in a
/// permissive mode, and the tmp file is where it exists first. `set_permissions` covers a stale tmp
/// left by an older build, whose mode `open` would not touch — and the `truncate` above means it is
/// zero bytes while we do it.
fn write_atomic(path: &std::path::Path, json: &[u8]) -> bool {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let Some(tmp) = tmp_path(path) else { return false };
    let opened =
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp);
    let Ok(mut f) = opened else { return false };
    let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
    let written = f.write_all(json).is_ok() && f.sync_all().is_ok();
    drop(f); // the rename must not race our own open handle on a filesystem that cares
    if written && std::fs::rename(&tmp, path).is_ok() {
        return true;
    }
    // Leave no half-written credentials behind under a name the next writer would overwrite
    // anyway — and none at all if this candidate turned out to be unwritable.
    let _ = std::fs::remove_file(&tmp);
    false
}

/// The sibling [`write_atomic`] writes through, for a resolved candidate path. One definition
/// because [`clear`] has to delete the same file, and a sign-out that missed it by spelling the
/// suffix differently would leave a live account token on the disk.
fn tmp_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(".tmp");
    Some(path.with_file_name(name))
}

/// Clear the persisted session (sign-out) — removes the file; a fresh `client_id` is minted next
/// load. The old-path copy goes too, or the migration fallback would resurrect the stale session.
///
/// Takes [`IO`] like every other entry point, and that is not tidiness: a sign-out racing an
/// in-flight worker's read-modify-write would otherwise delete the file and have the worker put it
/// straight back, account token and all.
pub fn clear() {
    let _io = io();
    // Every candidate, not just the one we happen to write today: leaving a copy at any other
    // location would let `peek`'s search resurrect the stale session on the next boot. The `.tmp`
    // siblings go too — `peek` cannot read one, so it is not a resurrection risk, but a sign-out
    // that leaves a live account token in a file on a rooted television is not a sign-out.
    for path in auth_paths() {
        if let Some(tmp) = tmp_path(&path) {
            let _ = std::fs::remove_file(tmp);
        }
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
            r#"{"client_id":"cid-1","recent_searches":[
                 {"user":"uu-1","terms":["wallace","Гладиатор","the curse"]}]}"#,
        )
        .expect("a session carrying terms parses");
        let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
        assert_eq!(s.recents_for("uu-1"), ["wallace", "Гладиатор", "the curse"], "most recent first, in order");

        // absent entirely — every session file written before this landed
        let s: Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
        assert!(s.recent_searches.is_empty());
    }

    /// **One profile cannot read another's history, and cannot delete it either.** A search
    /// history is as personal as watch state, and a television is the one place several people
    /// share an install — so this is scoped rather than cleared on a switch, which would have
    /// stopped the leak at the price of losing your own list every time you handed the remote over.
    #[test]
    fn a_profiles_search_history_is_its_own() {
        let mut s = Session { client_id: "cid".into(), ..Default::default() };
        s.set_recents_for("uu-a", vec!["gromit".into()]);
        s.set_recents_for("uu-b", vec!["эдем".into()]);

        assert_eq!(s.recents_for("uu-a"), ["gromit"]);
        assert_eq!(s.recents_for("uu-b"), ["эдем"]);
        assert!(s.recents_for("uu-never-searched").is_empty(), "an unknown profile reads empty, not someone else's");
        // the owner with no Plex Home selection keys on "" and is nobody else
        assert!(s.recents_for("").is_empty());

        // …and a write for one leaves the others intact — the bug `set_recents_for` exists to make
        // unwriteable, since the obvious `Session { recent_searches: mine, ..s }` deletes everybody.
        s.set_recents_for("uu-a", vec!["wallace".into(), "gromit".into()]);
        assert_eq!(s.recents_for("uu-a"), ["wallace", "gromit"]);
        assert_eq!(s.recents_for("uu-b"), ["эдем"], "the other profile's history survived the write");
    }

    /// And they degrade the same way every other list here does: one malformed term costs that
    /// term, never the credentials sitting beside it. A search term must never be able to sign the
    /// device out.
    #[test]
    fn a_corrupt_search_term_costs_that_term_and_not_the_session() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"address":"192.168.0.10","port":32400,"token":"t"},
                "recent_searches":[{"user":"u","terms":["wallace","gromit"]},null,42,"nope"]}"#,
        )
        .expect("a bad term must not fail the file");
        assert_eq!(s.recents_for("u"), ["wallace", "gromit"], "the three bad entries dropped");
        assert_eq!(s.account_token, "acct");
        assert!(s.can_go_local(), "and the device can still stream");

        // the whole field the wrong type is no list, not an error
        let s: Session = serde_json::from_str(r#"{"client_id":"c","recent_searches":"wallace"}"#)
            .expect("a string where a list belongs parses");
        assert!(s.recent_searches.is_empty());
    }

    // ---- The FILE half: one writer at a time, and a whole file or none of it -------------------
    //
    // Everything below drives the real `save`/`peek`/`update` against a real file, so it needs a
    // file it may have. `TempSession` redirects [`TEST_FILE`] — a crate global, which is why every
    // test here holds `crate::testlock::serial()` for its whole body (`src/lib.rs`): several
    // modules call `session::load` indirectly, and one running in parallel would read and WRITE
    // the file being graded.

    /// Point this module's file at a directory of this test's own, and take it back on drop.
    struct TempSession {
        dir: std::path::PathBuf,
    }

    impl TempSession {
        fn new(tag: &str) -> TempSession {
            // `env::temp_dir()` is right HERE and wrong in `dev.rs` (whose test warns against it):
            // there a literal path stops meeting a read that resolves its own root, while this
            // test is choosing the path that BOTH halves resolve to.
            let dir =
                std::env::temp_dir().join(format!("plxnative-session-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir); // a previous run that died mid-test
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(dir.join("auth.json"));
            TempSession { dir }
        }
        fn file(&self) -> std::path::PathBuf {
            self.dir.join("auth.json")
        }
        fn tmp(&self) -> std::path::PathBuf {
            self.dir.join("auth.json.tmp")
        }
    }

    impl Drop for TempSession {
        fn drop(&mut self) {
            *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn signed_in() -> Session {
        Session { client_id: "cid-1".into(), account_token: "acct".into(), ..Default::default() }
    }

    /// A save lands as a WHOLE file — written to a sibling tmp and renamed over — leaving nothing
    /// behind, and the credentials are never on disk in a mode another uid can read (this box is
    /// rooted and `/media/developer` is world-readable). The tmp is where the secret exists first,
    /// so the 0600 rule has to reach it too.
    #[test]
    fn a_save_lands_whole_and_leaves_no_temporary_behind() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("whole");

        save(&signed_in());
        assert_eq!(peek().account_token, "acct", "and it reads back");
        assert!(!t.tmp().exists(), "the tmp file is renamed, not left beside the session");
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials at rest");

        // a sign-out takes the tmp with it: `peek` cannot read one, but a live account token left
        // in a file on a rooted television is not a sign-out
        std::fs::write(t.tmp(), b"{}").unwrap();
        clear();
        assert!(!t.file().exists() && !t.tmp().exists());
    }

    /// **Two writers, one file, and neither may lose the other's work.** Each thread runs exactly
    /// the read-modify-write cycle the two real writers run — `auth`'s roster refresh growing
    /// `sources`, the search-recents worker growing one profile's terms — and when they are done
    /// every update from both must be in the file.
    ///
    /// This is the bug in its own shape: the roster worker re-read the file, a profile pick landed
    /// after that read, and its save put the pre-switch profile back — the next boot resuming as
    /// the wrong person. `update` makes the read and the write one step under one lock, so the
    /// interleaving that loses an update cannot be constructed.
    #[test]
    fn concurrent_read_modify_writes_never_lose_an_update() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("lost-update");
        save(&signed_in());

        // A dozen each is plenty and is deliberately not more: every cycle ends in the `sync_all`
        // that makes the rename mean something, and on this host that is an `F_FULLFSYNC` — the
        // whole host suite is meant to cost well under a second.
        const N: usize = 12;
        std::thread::scope(|sc| {
            sc.spawn(|| {
                for i in 0..N {
                    update(|s| {
                        let mut next = s.clone();
                        next.sources.push(SourceRef {
                            machine_id: format!("m{i}"),
                            address: "192.168.0.10".into(),
                            port: 32400,
                            token: "tok".into(),
                            ..Default::default()
                        });
                        Some(next)
                    });
                }
            });
            sc.spawn(|| {
                for i in 0..N {
                    update(|s| {
                        let mut next = s.clone();
                        let mut terms = next.recents_for("uu-1").to_vec();
                        terms.push(format!("term-{i}"));
                        next.set_recents_for("uu-1", terms);
                        Some(next)
                    });
                }
            });
        });

        let s = peek();
        assert_eq!(s.client_id, "cid-1", "the credentials survived every cycle");
        assert_eq!(s.account_token, "acct");
        assert_eq!(s.sources.len(), N, "a roster entry was overwritten by the other writer");
        assert_eq!(s.recents_for("uu-1").len(), N, "a search term was overwritten by the other writer");
    }

    /// **A reader outside the lock never sees half a session.** The reader here deliberately does
    /// NOT go through `peek` — that takes the same lock, so it could not observe a torn file even
    /// if `save` still truncated in place. It reads the path the way everything else on the device
    /// does, which is also the window a crash or a power cut reads through: with `O_TRUNC` the
    /// bytes at that path are empty for as long as the write takes, and an unparseable session
    /// file is a QR code on the next boot, not a stale roster.
    #[test]
    fn a_reader_outside_the_lock_never_sees_half_a_session() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("torn");
        save(&signed_in());

        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                for i in 0..20 {
                    update(|s| {
                        let mut next = s.clone();
                        // a payload big enough that one `write_all` is several pages — a torn read
                        // must not depend on the file happening to be tiny
                        next.home_users.push(HomeUserRef {
                            uuid: format!("uuid-{i}"),
                            title: format!("A profile with a long enough name to be worth {i} bytes"),
                            thumb: format!("https://plex.direct/photo/:/transcode?url=library%2Fmetadata%2F{i}"),
                            ..Default::default()
                        });
                        Some(next)
                    });
                }
                done.store(true, std::sync::atomic::Ordering::Release);
            });
            let file = t.file();
            let mut reads = 0u32;
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let bytes = std::fs::read(&file).expect("the path always names a complete file");
                let s: Session = serde_json::from_slice(&bytes)
                    .unwrap_or_else(|e| panic!("torn session file after {reads} clean reads: {e}"));
                assert_eq!(s.client_id, "cid-1", "a partial read is a signed-out device");
                reads += 1;
            }
        });
        assert_eq!(peek().home_users.len(), 20);
    }

    /// `update` must never CREATE a session. A missing or unparseable file reads back as a default
    /// `Session`, and writing one field onto that leaves a `client_id`-less file where a live
    /// session used to be — the silent sign-out every list in this struct is soft-parsed to
    /// prevent, arriving instead by the door built to fix it. It is also what a sign-out racing a
    /// background worker would otherwise produce: `clear()` removes the file, and the worker in
    /// flight puts a roster back with no credentials under it.
    #[test]
    fn update_refuses_a_file_that_holds_no_session() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("refuse");

        // no file at all — the state straight after `clear()`
        assert!(!update(|s| Some(Session { account_token: "acct".into(), ..s.clone() })));
        assert!(!t.file().exists(), "a refused cycle must not create the file it refused to write");

        // a file that does not parse: the same answer, and the bytes are left alone rather than
        // replaced with a freshly minted session
        std::fs::write(t.file(), b"{ not json").unwrap();
        assert!(!update(|_| Some(signed_in())));
        assert_eq!(std::fs::read(t.file()).unwrap(), b"{ not json");
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
