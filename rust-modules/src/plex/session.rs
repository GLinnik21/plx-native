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
    #[serde(default)]
    pub home_users: Vec<HomeUserRef>,
    /// Which libraries feed Home — the ONE user-facing control for shared servers
    /// ([`crate::pms::pin`] owns the model; this is only where its answers are written down).
    ///
    /// **Empty means UNKNOWN, not "nothing is pinned".** Nobody has answered yet — a fresh
    /// install, or a session written by a build that predates the field — and the resolver
    /// therefore falls back to the defaults (your own libraries on, a friend's off). Reading
    /// empty as "none" would greet the first boot after an upgrade with a blank Home, which is
    /// the same class of bug [`Session::account`] documents for `home_users`.
    ///
    /// It is a decision LOG, not a set of pinned keys: an unpinned library is written down as
    /// `pinned: false` rather than omitted, so "answered no" and "never asked" stay
    /// distinguishable per library — that is what lets a share that arrives next month default
    /// to off without re-opening the first-run question about the ones already answered.
    ///
    /// `de_pins` is deliberately lenient: a malformed array here degrades to "unknown" instead
    /// of failing the whole `Session` parse. A failed parse is a SILENT SIGN-OUT — `peek` falls
    /// through to `Session::default()` and `load` then mints a fresh client id over the top of
    /// the real one — so no field this file gains may ever be able to cause one.
    #[serde(default, deserialize_with = "de_pins")]
    pub pins: Vec<LibraryPin>,
}

/// One persisted pin decision: a library, and whether it feeds Home.
///
/// Keyed by **(machine_id, section)** because a section key is only unique within one server —
/// every PMS numbers its first library `1`, so a friend's Movies and yours collide on the id
/// alone. No title is stored: a renamed library is still the same library, and this file holds
/// secrets and is never logged, so the fewer human-readable strings in it the better.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct LibraryPin {
    /// the owning server's `machineIdentifier`
    #[serde(default)]
    pub machine_id: String,
    /// the library's section key on that server (`/library/sections` → `key`)
    #[serde(default)]
    pub section: i64,
    /// feeds Home
    #[serde(default)]
    pub pinned: bool,
}

/// Lenient `pins` reader — see the field's doc for why this cannot be the derived one.
///
/// Two levels of tolerance, and they mean different things. A value that is not an array at all
/// (a `null`, an object, a string written by something else) is "unknown" — the whole array
/// degrades to empty. A single malformed ROW is dropped on its own, because the rows around it
/// are still real answers the user gave; the libraries whose rows were lost simply fall back to
/// their default, which is exactly what a library we have never seen does.
///
/// Going through `serde_json::Value` is what makes "drop a row" possible at all: on a failed
/// `Deserialize` the JSON reader is left mid-token, so every LATER field of `Session` would fail
/// too. Parsing one complete value first cannot fail on anything but broken JSON syntax, which
/// no longer distinguishes this field from the rest of the file.
fn de_pins<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<LibraryPin>, D::Error> {
    use serde_json::Value;
    let Value::Array(rows) = Value::deserialize(d)? else { return Ok(Vec::new()) };
    // ints arrive as ints (we wrote them), but accept the string form PMS uses everywhere too —
    // this file is hand-editable on a rooted TV, and a quoted number is the likeliest hand edit.
    let num = |v: Option<&Value>| match v {
        Some(Value::Number(n)) => n.as_i64(),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    };
    // `None` for anything that is not recognisably a yes or a no — and the row is then DROPPED,
    // not read as "off". A word we do not understand is not an answer, and a wrong `false` is the
    // worse of the two failures: it suppresses the library's default AND counts as a decision,
    // where a dropped row simply falls back to the default like a library we have never seen.
    let flag = |v: Option<&Value>| match v {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::Number(n)) => n.as_i64().map(|n| n != 0),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => None,
        },
        _ => None,
    };
    Ok(rows
        .iter()
        .filter_map(|r| {
            let o = r.as_object()?;
            let machine_id = o.get("machine_id")?.as_str()?.to_string();
            let section = num(o.get("section"))?;
            let pinned = flag(o.get("pinned"))?;
            // a row that names no server is not a decision about any library
            (!machine_id.is_empty()).then_some(LibraryPin { machine_id, section, pinned })
        })
        .collect())
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
    /// The recorded answer for one library, or `None` when nobody has answered for it — a new
    /// share, a library added since, or a session that predates the field. `None` is the caller's
    /// cue to apply the DEFAULT ([`crate::pms::pin::resolve`]), never to assume "off".
    pub fn pin_of(&self, machine_id: &str, section: i64) -> Option<bool> {
        self.pins
            .iter()
            .find(|p| p.machine_id == machine_id && p.section == section)
            .map(|p| p.pinned)
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
    // A `save` never writes the caller's `pins` — see [`set_pins`], the one writer of that field.
    let _ = write_out(s, false);
}

/// Record the pin table — **the only writer of [`Session::pins`]**, and the reason [`save`] leaves
/// that field alone. `false` = nothing reached disk (see below), which the first-run route relies
/// on: it must not retire its question on a write that did not happen.
///
/// The field needs a single writer because every OTHER writer here saves the WHOLE struct from a
/// snapshot of unknown age. `auth` takes one into its controller when a profile switch starts and
/// writes that clone back when the plex.tv roster lands — a plex.tv round trip later, and a
/// keypress in the Library toolbar is far quicker than a stalled one. Merging by "an empty table
/// means the writer doesn't know about pins" only covers a snapshot taken before the FIRST answer;
/// giving the field an owner covers every snapshot, of any age, forever.
///
/// Refuses to write when the session on disk is unreadable, for the reason [`peek`] exists: `save`
/// truncates in place, so writing a session assembled from a failed read is a silent sign-out, and
/// this is a path a keypress reaches.
pub fn set_pins(pins: Vec<LibraryPin>) -> bool {
    let mut s = peek();
    if s.client_id.is_empty() {
        crate::log("session: no readable session on disk — pins not saved");
        return false;
    }
    s.pins = pins;
    write_out(&s, true)
}

/// The write itself. `own_pins` = the caller owns the `pins` field (only [`set_pins`] does); every
/// other write takes the table from disk, so no stale snapshot can revert an answer.
/// `false` = no candidate path accepted the write.
fn write_out(s: &Session, own_pins: bool) -> bool {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let merged;
    let s = if own_pins {
        s
    } else {
        merged = Session { pins: pins_for_save(&s.pins, || peek().pins), ..s.clone() };
        &merged
    };
    let Ok(json) = serde_json::to_vec_pretty(s) else { return false };
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
                return true;
            }
        }
    }
    crate::log("session: could not persist to ANY candidate path — login will not survive a reboot");
    false
}

/// What a plain [`save`] puts in `pins`: **what is on disk**, not what the caller is holding.
/// Pure, so the rule is testable without a session file; `stored` is read lazily so the decision
/// costs nothing when there is nothing to protect. The caller's copy is used only when disk has
/// none — the migration case, where dropping it would lose a table for good.
fn pins_for_save(incoming: &[LibraryPin], stored: impl FnOnce() -> Vec<LibraryPin>) -> Vec<LibraryPin> {
    let disk = stored();
    if disk.is_empty() {
        incoming.to_vec()
    } else {
        disk
    }
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

// ---------------------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    /// Pure: serde only. Nothing here reads or writes the session FILE — `save`'s candidate paths
    /// are device paths, and a host test that wandered into them would be writing a real one.
    fn parse(json: &str) -> Session {
        serde_json::from_str(json).expect("the session must survive whatever `pins` holds")
    }

    /// The shape a real session has, with `pins` substituted in — so every assertion below is
    /// also an assertion that the FIELDS AROUND IT survived.
    fn with_pins(pins: &str) -> String {
        format!(r#"{{"client_id":"cid-1","account_token":"tok","pins":{pins}}}"#)
    }

    /// **Empty means unknown, not none.** A session written before this field existed, and one
    /// written with an empty table, must both come back as "nobody has answered yet" — the
    /// resolver then applies the defaults. Reading either as "nothing is pinned" is an empty Home
    /// on the first boot after an upgrade.
    #[test]
    fn an_absent_or_empty_pin_table_means_unknown() {
        assert!(parse(r#"{"client_id":"cid-1"}"#).pins.is_empty());
        assert!(parse(&with_pins("[]")).pins.is_empty());
        assert_eq!(parse(&with_pins("[]")).account_token, "tok", "and the session is intact");
    }

    /// A written table round-trips, and an answer of "off" survives as an ANSWER: the table is a
    /// decision log, not a set of pinned keys, so `Some(false)` and `None` must stay different —
    /// that difference is what lets a share arriving next month default to off without reopening
    /// the question about the libraries already answered.
    #[test]
    fn a_recorded_table_round_trips_including_the_noes() {
        let s = Session {
            client_id: "cid-1".into(),
            pins: vec![
                LibraryPin { machine_id: "aaaabbbb1111".into(), section: 1, pinned: true },
                LibraryPin { machine_id: "ccccdddd2222".into(), section: 1, pinned: false },
            ],
            ..Session::default()
        };
        let back = parse(&serde_json::to_string(&s).unwrap());
        assert_eq!(back.pin_of("aaaabbbb1111", 1), Some(true));
        assert_eq!(back.pin_of("ccccdddd2222", 1), Some(false), "a recorded 'off' is not 'unanswered'");
        assert_eq!(back.pin_of("ccccdddd2222", 2), None, "…and a library nobody ruled on is unanswered");
        assert_eq!(back.pin_of("aaaabbbb1111", 1), Some(true), "the key is per SERVER: both are library 1");
    }

    /// A corrupt table degrades to unknown — it must never fail the whole `Session` parse. That
    /// failure is a SILENT SIGN-OUT: `peek` falls through to a default session and `load` mints a
    /// fresh client id over the real one, so the user re-does the QR sign-in every boot and the
    /// cause is invisible. Every shape here is one the field can meet on a rooted TV with a
    /// hand-editable file.
    #[test]
    fn a_corrupt_pin_table_degrades_to_defaults_instead_of_signing_the_user_out() {
        for bad in [r#""nope""#, "null", "17", r#"{"aaaabbbb1111":true}"#, "[1,2,3]", r#"[null,{},[]]"#] {
            let s = parse(&with_pins(bad));
            assert!(s.pins.is_empty(), "{bad} should read as unknown");
            assert_eq!(s.client_id, "cid-1", "{bad} must not cost the client id");
            assert_eq!(s.account_token, "tok", "{bad} must not cost the account token");
        }
    }

    /// The persisted table survives a write by ANY other writer, whatever that writer is holding.
    /// `auth` saves a whole-`Session` clone it snapshotted when a profile switch started — before
    /// the user reached the Library toolbar and answered — so a table that is merely *older* has
    /// to lose to disk just as an empty one does. That is why the field has one owner
    /// ([`set_pins`]) instead of a merge rule.
    #[test]
    fn no_other_writer_can_revert_the_pin_table() {
        let disk = vec![LibraryPin { machine_id: "aaaabbbb1111".into(), section: 1, pinned: false }];
        let stale = vec![LibraryPin { machine_id: "aaaabbbb1111".into(), section: 1, pinned: true }];
        let written = pins_for_save(&stale, || disk.clone());
        assert_eq!(written.len(), 1);
        assert!(!written[0].pinned, "disk wins over a snapshot of unknown age");

        assert!(pins_for_save(&[], Vec::new).is_empty(), "nothing anywhere is still nothing");
        assert_eq!(
            pins_for_save(&stale, Vec::new).len(),
            1,
            "with nothing on disk the caller's copy is the only table there is"
        );
    }

    /// Corruption is per ROW where it can be: the rows around a broken one are still answers the
    /// user gave, and the libraries whose rows were lost simply fall back to their default —
    /// which is exactly what a library we have never seen does. The lenient number/flag reading
    /// is for the hand edit: a quoted number is the likeliest one.
    #[test]
    fn one_broken_row_costs_only_that_row() {
        let s = parse(&with_pins(
            r#"[{"machine_id":"aaaabbbb1111","section":"2","pinned":"1"},
                {"section":9,"pinned":true},
                {"machine_id":"","section":1,"pinned":true},
                {"machine_id":"ccccdddd2222","section":1,"pinned":0}]"#,
        ));
        assert_eq!(s.pins.len(), 2, "the two rows that name a library survive");
        assert_eq!(s.pin_of("aaaabbbb1111", 2), Some(true), "\"2\"/\"1\" read as 2/true");
        assert_eq!(s.pin_of("ccccdddd2222", 1), Some(false), "0 reads as false");
    }
}
