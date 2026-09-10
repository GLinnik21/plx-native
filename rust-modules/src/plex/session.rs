//! Persisted login session — what makes the client **offline-first**. After the one-time online
//! login (account token → server discovery → profile switch), the chosen server's verified
//! [`Origin`] and the profile's token are written here. A stable build can therefore resume a
//! stored HTTPS origin without plex.tv when it remains reachable; an explicit developer-trigger
//! build may also resume a plaintext HTTP origin for lab use. Lives in the writable app dir (device-only; never in the
//! repo). The token fields are secrets — this file's contents are never logged.
//!
//! ## One server, and then the ROSTER
//!
//! [`Session::server`] is still the primary — the one address `can_go_local` runs on and the one
//! `app.rs` boots against — and refresh keeps its origin/tier aligned with the roster. Beside it,
//! [`Session::sources`] records **every**
//! server discovery reached, ours and every share, each with its own address and its own
//! per-(user, server) token, because a shared server is a separate authority that answers 401 to
//! anybody else's credential (`docs/shared-servers.md` §2b). A single-server account writes one
//! entry there and behaves exactly as it always has.
//!
//! **Nothing here carries a timestamp**, deliberately: this TV's wall clock runs ~3 h skewed
//! (`docs/agent-reference.md`), so a stored "last seen" would be a number that cannot be compared with
//! anything and would invite an expiry rule built on it.
//!
//! ## Storage: encrypted when it can be, 0600 always, and STAYS 0600 once refused
//!
//! [`save`] asks `keymanager::seal` to device-key-encrypt the file and falls back to a mode-0600
//! plaintext file when no usable Key Manager is available. **Once an install has proven it cannot
//! read its own sealed envelope back — [`LOCKED_RECOVERABLE`], issue #76 — that install keeps the
//! 0600 file until sign-out or erase**, never only for the one launch that found it: the verdict is
//! recorded in a small on-disk marker (see [`write_refused_marker`]) precisely because a backend
//! that round-trips fine WITHIN one launch can still be the same one that sealed the now-unreadable
//! envelope, and re-sealing on that evidence alone reproduces the loop one launch later.
//! [`clear`] removes the marker with the session, so a different account or a future firmware gets
//! a fresh chance.
use super::origin::Origin;
use super::probe::Location;
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
static TEST_FILE: Mutex<Option<Vec<std::path::PathBuf>>> = Mutex::new(None);

#[cfg(test)]
fn auth_paths() -> Vec<std::path::PathBuf> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(v) => v,
        None => crate::paths::session_candidates(),
    }
}

/// Test-only: the multi-candidate form of [`redirect_for_test`], for the issue #76 review's
/// recovery-targeting/sweep coverage — everything else here drives a single candidate, which
/// cannot exercise "the locked envelope is not at `auth_paths()[0]`" at all.
#[cfg(test)]
fn redirect_for_test_multi(paths: Vec<std::path::PathBuf>) {
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(paths);
    clear_cache();
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(CLASS_UNKNOWN, std::sync::atomic::Ordering::Relaxed);
}

/// Point this module's file at `p`, or back at the real search order with `None`.
///
/// `pub(crate)` because the writing half is no longer only this module's business: `browse`'s
/// per-profile Home selection round-trips through this file, and grading THAT end to end is the
/// only way to catch the shape of bug it exists to prevent (one profile's answer overwriting
/// another's), which no in-memory fixture can see.
///
/// The caller owes the same discipline `tests::TempSession` documents: hold
/// [`crate::testlock::serial`] for the whole test, because this is a crate global and several
/// modules reach `session::load` indirectly.
///
/// Also resets [`CACHE`] and [`LOCKED_STATE`] — both process globals, and without this a leftover
/// cache from one test would answer `peek()` in the next one before it has written anything of its
/// own.
#[cfg(test)]
pub(crate) fn redirect_for_test(p: Option<std::path::PathBuf>) {
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p.map(|p| vec![p]);
    clear_cache();
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(CLASS_UNKNOWN, std::sync::atomic::Ordering::Relaxed);
}

/// **One in-process copy of the session.** Published by every successful [`load`], [`save_locked`]
/// (after a write actually lands) and [`update`] — every writer in this process goes through this
/// module under [`IO`], so this cache can never go stale relative to what THIS process itself last
/// wrote; the file only ever moves under a peer process's feet if a second copy of the app is
/// running against it, which is not a case this app supports. [`peek_locked`] serves straight from
/// here once anything has been published, which is what stops the account chip, the menu and every
/// other reader from re-decrypting the file (and re-paying keymanager3's multi-second LS2 budget)
/// on every keypress. [`clear`] (sign-out) empties it.
static CACHE: Mutex<Option<Session>> = Mutex::new(None);

fn cached() -> Option<Session> {
    CACHE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn publish_cache(s: Session) {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

fn clear_cache() {
    *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

const NOT_LOCKED: u8 = 0;
/// The recognized secure envelope (this build's own format + version) is present but this process
/// could not open it — the issue #76 shape: keymanager3 sealed something it can no longer decrypt,
/// on every read, forever. A fresh sign-in (a save carrying an `account_token`) may safely replace
/// this file; there is nothing to lose that the sign-in itself does not already re-supply.
const LOCKED_RECOVERABLE: u8 = 1;
/// A secure-shaped file this build does not understand at all — an unrecognized format/version, or
/// bytes that decrypted but did not parse as a `Session`. Never blindly overwritten: unlike the
/// recoverable case this is not necessarily the keymanager3 round-trip bug, and could as easily be
/// a newer build's envelope or a genuinely corrupt one, either of which a same-version rewrite would
/// destroy for no reason connected to this device's key manager at all.
const LOCKED_UNRECOVERABLE: u8 = 2;
/// What the most recent [`read_locked`] in this process found — see [`LOCKED_RECOVERABLE`] /
/// [`LOCKED_UNRECOVERABLE`]. Read only by [`save_locked`]'s plaintext-downgrade decision.
static LOCKED_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(NOT_LOCKED);
/// The candidate path [`read_locked`] actually found the [`LOCKED_RECOVERABLE`] envelope at, kept
/// in lockstep with [`LOCKED_STATE`] by [`locked`]/[`not_locked`]. `read_locked` tries candidates
/// in priority order and stops at the first one that exists — so the recoverable envelope is not
/// necessarily at `auth_paths()[0]`, and a recovery write must target the SAME candidate `read_locked`
/// found it at rather than "whichever candidate happens to accept a write first": those can differ
/// when a lower-priority candidate is writable but the one actually holding the locked file is not,
/// which would otherwise leave the locked file in place — still shadowing everything below it —
/// while a stray plaintext copy accumulates at another path.
static LOCKED_PATH: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// This process has read or saved nothing yet — the transient state before the very first
/// `read_locked`/`save_locked` in a process, distinct from [`CLASS_NONE`] (a real "no file exists")
/// so [`storage_class`] can tell "unknown, ask again" apart from "known and empty" if a future
/// caller ever needs to.
const CLASS_UNKNOWN: u8 = 0;
/// The last non-locked read found no file, or (unreachably in practice — a fresh install always
/// gets a save right behind its first `Missing` read) a save has yet to happen.
const CLASS_NONE: u8 = 1;
const CLASS_PLAINTEXT: u8 = 2;
const CLASS_SECURE: u8 = 3;
/// What the most recent NON-LOCKED [`read_locked`]/[`save_locked`] in this process actually did —
/// opened or sealed a real secure envelope, read or wrote the 0600 plaintext fallback, or found
/// nothing at all. [`storage_class`] only consults this once the live locked/refused checks below
/// have both come back negative; a locked or refused verdict always outranks whatever this last
/// says, since those are facts about the file RIGHT NOW rather than about the last successful step.
static LAST_CLASS: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(CLASS_UNKNOWN);

/// Issue #76 storage telemetry: this process's public, live verdict on how the session file is
/// protected right now — [`crate::telemetry::storage::SessionStorageClass`], the one vocabulary
/// this module, `keymanager.rs` and `diag::schema::UsageContext::session_storage` all share.
///
/// **Ordered by how outranking a fact is, not by how recently it was learned.** A persisted refused
/// marker or this process's own live [`LOCKED_STATE`] both describe the file as it stands RIGHT NOW
/// and must win over [`LAST_CLASS`], which only remembers the last NON-locked step — otherwise a
/// process whose most recent successful read was plaintext (launch 3 of the four-launch sequence
/// `save_locked`'s doc walks through) would report `Plaintext` even while sitting on a marker that
/// says this install has already been downgraded for good, or — the narrower per-process case —
/// while `LOCKED_STATE` says the file this process just tried to read is the one it could not open.
pub(crate) fn storage_class() -> crate::telemetry::storage::SessionStorageClass {
    use crate::telemetry::storage::SessionStorageClass;
    if has_refused_marker() {
        return SessionStorageClass::SecureRefused;
    }
    match LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) {
        LOCKED_RECOVERABLE | LOCKED_UNRECOVERABLE => SessionStorageClass::SecureLocked,
        _ => match LAST_CLASS.load(std::sync::atomic::Ordering::Relaxed) {
            CLASS_SECURE => SessionStorageClass::Secure,
            CLASS_PLAINTEXT => SessionStorageClass::Plaintext,
            CLASS_NONE => SessionStorageClass::None,
            // CLASS_UNKNOWN: this process has not read or saved anything yet — reachable from
            // `app.launch` (the highest-volume usage event), sent before the boot path's own
            // first `load()`. Reporting `None` here would claim "no session file exists" about an
            // install this process has simply never looked at, including a secure or locked one.
            _ => SessionStorageClass::Unknown,
        },
    }
}

/// Storage-error reports discovered while [`IO`] was held, drained and sent once the lock is
/// released — the same reason `auth.rs`'s `set_error` collects under `Ctl`'s lock and reports after:
/// `telemetry::storage::report_error` does spool I/O (its own lock, a disk read/write, possibly a
/// log line), and `IO` here is held across a synchronous save on the SDL main thread. A `Vec` rather
/// than a single slot only because a read that lands Locked and a save's own seal failure could in
/// principle both queue within the same locked step.
static PENDING_REPORTS: Mutex<Vec<crate::telemetry::storage::StorageErrorContext>> =
    Mutex::new(Vec::new());

fn queue_report(ctx: crate::telemetry::storage::StorageErrorContext) {
    PENDING_REPORTS.lock().unwrap_or_else(|e| e.into_inner()).push(ctx);
}

/// Which [`crate::telemetry::storage::StorageStage`]s this PROCESS has already reported — "exactly
/// once per process per stage", so a television whose keymanager3 answers the same refusal call
/// after call does not fill the spool with the same report on every later save.
static REPORTED_STAGES: Mutex<Vec<crate::telemetry::storage::StorageStage>> = Mutex::new(Vec::new());

/// Route through here rather than `telemetry::storage::report_error` directly so a test can capture
/// what this module tried to report without a real Sentry endpoint compiled in — same shape as
/// `keymanager.rs`'s own `log`/`capture` seam.
#[cfg(not(test))]
fn report_storage_error(ctx: crate::telemetry::storage::StorageErrorContext) {
    crate::telemetry::storage::report_error(ctx);
}
#[cfg(test)]
fn report_storage_error(ctx: crate::telemetry::storage::StorageErrorContext) {
    tests::capture_report(ctx);
}

fn report_once(ctx: crate::telemetry::storage::StorageErrorContext) {
    let mut reported = REPORTED_STAGES.lock().unwrap_or_else(|e| e.into_inner());
    if reported.contains(&ctx.stage) {
        return;
    }
    reported.push(ctx.stage);
    drop(reported);
    report_storage_error(ctx);
}

/// Drain and send whatever [`queue_report`] collected — called by every public entry point
/// ([`load`], [`update`], [`save`]) AFTER its own `IO` guard has dropped.
fn drain_pending_reports() {
    let pending: Vec<_> =
        std::mem::take(&mut *PENDING_REPORTS.lock().unwrap_or_else(|e| e.into_inner()));
    for ctx in pending {
        report_once(ctx);
    }
}

/// Test-only: forget every stage this process has already "reported" (see
/// [`tests::capture_report`]) — deliberately NOT folded into [`redirect_for_test`], since the
/// once-per-process rule is exactly what a real process never resets on a file redirect either.
#[cfg(test)]
fn reset_report_state_for_test() {
    PENDING_REPORTS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    REPORTED_STAGES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    tests::CAPTURED_REPORTS.with(|c| c.borrow_mut().clear());
}

/// **The cross-launch half of issue #76.** [`LOCKED_STATE`] is a *process* global — it answers
/// nothing about what a PREVIOUS launch found, so a backend whose key differs per
/// launch/registration (issue #76's hypothesis 2) can round-trip cleanly on launch 3, look
/// perfectly healthy to [`seal_permitted`]'s per-process half, and re-seal — reproducing the
/// exact loop the fix was meant to end, just one launch later. This marker is what makes the
/// verdict persist ON THE INSTALL rather than resetting at every `exec()`: once [`read_locked`]
/// proves an envelope unopenable, [`write_refused_marker`] records that fact on disk, and every
/// later [`save_locked`] — this launch's or any other's — consults [`has_refused_marker`] before
/// ever calling `keymanager::seal` again. Removed only by [`clear`] (sign-out/erase): a different
/// account, or a future firmware, gets a fresh chance.
fn refused_marker_paths() -> Vec<std::path::PathBuf> {
    auth_paths()
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let marker_name = match name.strip_suffix("auth.json") {
                Some(prefix) => format!("{prefix}secure-storage.refused"),
                None => format!("{name}.secure-storage.refused"),
            };
            Some(p.with_file_name(marker_name))
        })
        .collect()
}

/// Whether a PRIOR (or this) launch has already recorded that keymanager3's envelope could not be
/// opened on this install — see [`refused_marker_paths`]. Checked the same way [`has_secure_locked`]
/// checks for a secure file: owned, regular, readable — never trusting a path some other uid could
/// have planted.
fn has_refused_marker() -> bool {
    refused_marker_paths()
        .iter()
        .any(|p| read_owned_regular(p).is_some())
}

/// Record the cross-launch verdict: this process's own [`read_locked`] found a recognized secure
/// envelope it could not open. Idempotent — a marker already on disk is left alone, since its
/// content is a fact about the FIRST time this was seen, not something a later read should keep
/// overwriting. No key material, ciphertext or plaintext goes into it, only the version that
/// observed the refusal.
fn write_refused_marker(stage: crate::telemetry::storage::StorageStage) {
    if has_refused_marker() {
        return;
    }
    // `stage` is the same closed vocabulary the handled report sends (`no_reply`, `unreachable`,
    // `begin_decrypt`, …): it says HOW the open failed, which the log alone could not once the
    // launch that wrote this is gone. Nothing reads it back but a person with the file.
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "refused_at_version": super::identity::VERSION,
        "reason": "envelope_unopenable",
        "stage": stage.code(),
    }))
    .unwrap_or_default();
    for path in refused_marker_paths() {
        if write_atomic(&path, &body) {
            return;
        }
    }
    crate::log(
        "session: could not persist the refused-storage marker to ANY candidate path — secure storage may be retried next launch",
    );
}

/// Whether this process has already logged that a save skipped sealing purely because of the
/// persisted marker — see [`write_refused_plaintext`]. Once per process: every `update()` on an
/// install already downgraded to plaintext takes the same branch, and repeating the line on every
/// roster refresh would drown the log in a restatement of a fact recorded once already.
static MARKER_SKIP_LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn log_marker_skip_once() {
    if !MARKER_SKIP_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::log(
            "session: secure storage is marked refused on this install; keeping the 0600 file",
        );
    }
}

/// The pure gate behind [`save_locked`]'s plaintext-downgrade decision — whether `keymanager::seal`
/// may even be asked to try. Two DIFFERENT reasons force the plaintext branch, and this is their
/// union:
///
/// - **Per-process** (`locked_state == LOCKED_RECOVERABLE`): THIS process's own [`read_locked`]
///   already proved the on-disk envelope unopenable. Only refuses to seal once the save itself
///   carries real credentials (`saving_fresh_sign_in`) — an unrelated writer with nothing of its
///   own to replace the locked file with must never be the thing that destroys it (see
///   `update_after_a_locked_boot_does_not_destroy_the_locked_envelope`).
/// - **Persisted** (`marker_present`): a PRIOR launch already proved it — the cross-launch half
///   issue #76's review asked for. Once true it stays true for EVERY save on this install,
///   `saving_fresh_sign_in` included: there is nothing left to protect, because this install has
///   already been downgraded to plaintext once, and the marker exists precisely to stop it being
///   re-sealed the next time an in-process round trip happens to look clean.
fn seal_permitted(marker_present: bool, locked_state: u8, saving_fresh_sign_in: bool) -> bool {
    if marker_present {
        return false;
    }
    !(locked_state == LOCKED_RECOVERABLE && saving_fresh_sign_in)
}

/// The full persisted session. Empty fields mean "not logged in yet" for that stage.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    /// Stable `X-Plex-Client-Identifier` — generated once, reused forever (plex.tv binds the pin
    /// and the authorized-device entry to it).
    #[serde(default)]
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
    /// Which libraries each PROFILE chose to see **on Home**. Browsing is governed by the grant,
    /// not by this: pinning is the only *setting* of the three states a source has (granted /
    /// pinned / reachable — `docs/shared-servers.md` §6).
    ///
    /// **Keyed by profile, and that is the whole point of the shape** — the same lesson
    /// [`Session::recent_searches`] beside it records, learned the same way. It was a bare
    /// `Vec<PinnedLib>` hanging off the `Session`, which is one per INSTALL: a household where one
    /// person wants a friend's films on their front door and another does not could not express it,
    /// and switching profile left the previous person's shelves in place. The owner's ruling
    /// (2026-08-21) is explicit — "it is separate for each profile" — and a shared television is
    /// exactly where that matters.
    ///
    /// **An absent entry means "never asked", not "nothing pinned"** — the same trap `home_users`
    /// documents, and why [`HomePins`] records both sides of the answer rather than one list.
    ///
    /// Soft-parsed (see [`de_soft_vec`]) like every list in this struct: one hand-edited entry
    /// costs that entry, never the credentials.
    #[serde(default, deserialize_with = "de_soft_vec")]
    pub home_pins: Vec<HomePins>,
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
    /// The install's playback-quality preference. `None` is deliberately distinct from an
    /// explicit value: every session written before this field existed lands there and must keep
    /// the old **Original** behaviour rather than being migrated onto automatic playback.
    ///
    /// A newly-created session writes an explicit default through [`PlaybackQuality::fresh_default`].
    /// That default may become Auto only when the playback owner exposes a positive readiness
    /// gate. The integrated HLS prime/swap path opens it for fresh installs; old files remain
    /// Original because their absent field is not reinterpreted. Unknown or malformed future
    /// values soften to `None`, and therefore Original,
    /// instead of making the credentials file fail to parse.
    #[serde(default, deserialize_with = "de_soft_playback_quality")]
    pub(crate) playback_quality: Option<PlaybackQuality>,
    /// **Device-wide ambient memory**: the last hero `UltraBlurColors` envelope Home actually
    /// rendered on this television, so a route in the Settings/first-run family that opens
    /// BEFORE Home has fetched anything this boot — first-run consent moved ahead of the
    /// profile picker is the case that motivated this — can still seed its frozen ground from
    /// real light instead of falling all the way to the design system's authored atmosphere
    /// (`theme::ROUTE_GROUND_FALLBACK`). See [`crate::ui::route_screen::RouteGround::draw_home`],
    /// the only reader, and [`record_last_hero`], its one writer.
    ///
    /// Not keyed by profile: it says nothing about content history, only about what colour light
    /// this SET last showed, which is why it lives beside `client_id` rather than in a per-profile
    /// section like [`Session::home_pins`].
    #[serde(default)]
    pub(crate) last_hero_blur: Option<[[f32; 3]; 4]>,
}

/// Remember the hero envelope Home is showing right now, best-effort, for [`Session::last_hero_blur`].
///
/// Cheap to call on every route-ground latch: [`update`] is a single read-modify-write, and this
/// skips the write entirely when the stored envelope already matches, so parking on the same hero
/// for minutes costs nothing beyond the initial read. A session with no `client_id` yet (nothing
/// signed in) is a deliberate no-op — see [`update`]'s doc — which is fine here: there is no
/// pre-Home route to seed before an account exists.
///
/// Returns whether the file was actually rewritten — `false` both when nothing is signed in yet
/// ([`update`]'s own no-op rule) and when the stored envelope already matches, which is how a test
/// can grade the skip without inspecting file bytes.
pub(crate) fn record_last_hero(blur: [[f32; 3]; 4]) -> bool {
    update(|cur| {
        if cur.last_hero_blur == Some(blur) {
            return None;
        }
        let mut next = cur.clone();
        next.last_hero_blur = Some(blur);
        Some(next)
    })
}

/// The last hero envelope recorded by [`record_last_hero`], or `None` on a fresh device that has
/// never rendered one.
pub(crate) fn last_hero() -> Option<[[f32; 3]; 4]> {
    load().last_hero_blur
}

/// The persisted playback-quality modes. The spelling on disk is explicit rather than derived
/// from Rust variant names: these strings are a file-format contract and must survive refactors.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlaybackQuality {
    /// Automatic adaptation. It is offered only after the playback readiness gate opens.
    #[serde(rename = "auto")]
    Auto,
    /// No ceiling: the source's original quality and the legacy playback behaviour.
    #[default]
    #[serde(rename = "original")]
    Original,
    /// 1080p at 20 Mbps — cap large 4K sources while preserving high-rate HD.
    #[serde(rename = "1080p_20_mbps")]
    P1080High,
    /// 1080p at 8 Mbps.
    #[serde(rename = "1080p_8_mbps")]
    P1080,
    /// 720p at 4 Mbps.
    #[serde(rename = "720p_4_mbps")]
    P720,
    /// 720p at 2 Mbps.
    #[serde(rename = "720p_2_mbps")]
    P720Low,
    /// 480p at 720 kbps.
    #[serde(rename = "480p_720_kbps")]
    P480,
}

impl PlaybackQuality {
    /// A missing field in an OLD file is handled by [`Session::playback_quality`] and is always
    /// Original. This is only for a genuinely NEW file, where Auto is allowed to become the
    /// default after (and only after) its whole playback path declares itself ready.
    pub(crate) fn fresh_default(auto_ready: bool) -> Self {
        if auto_ready {
            Self::Auto
        } else {
            Self::Original
        }
    }
}

/// One profile's search history.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct RecentSearches {
    /// The Plex Home user's `uuid`, or **empty for the account owner** with no Home selection.
    /// `uuid` and not `id`, because it is the identity that survives a roster refetch.
    ///
    /// This used to cite [`SourceRef`]'s handle as the same "empty means the owner" convention. It
    /// is not one any more and never quite was: an empty [`SourceRef::shared_by`] is *nobody to
    /// credit*, which covers the household's server and an unnamed share as well as our own.
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
    /// This member's plex.tv **account id** (`/api/v2/home/users[].id`) — the identity
    /// [`Session::household_ids`] hands the "Shared by …" rule, and the reason it is here at all.
    /// It is the SAME id space as `account::Resource::owner_id`: the admin's row carries the id
    /// `/api/v2/user` reports for the account itself (measured 2026-09-03 on the dev account), so
    /// "does this server's owner live in this house" is an integer comparison rather than a
    /// comparison of two differently-sourced display names. `0` in every file written before this
    /// field existed, and `0` never matches — see [`super::servers::is_household`].
    pub id: i64,
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    pub protected: bool,
    pub admin: bool,
}

/// The PRIMARY server's coordinates — the one `can_go_local` boots on. `origin` is the verified
/// HTTP(S) authority; `address`:`port` remains its diagnostic/legacy fallback. `token` is that
/// server's access token (fallback when no managed-user token is set). Every server, including
/// this one, is also in [`Session::sources`].
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)] // a missing field costs that field, never the session — see [`HomeUserRef`]
pub struct ServerRef {
    pub name: String,
    pub machine_id: String,
    /// The dotted quad (or v6 literal, or hostname) discovery recorded. **Diagnostic, and the
    /// LEGACY fallback** — see [`ServerRef::origin`], which is what anything dialling reads.
    pub address: String,
    pub port: i64,
    pub token: String,
    /// The connection tier that won the last completed probe. `None` in legacy files and whenever
    /// an address was restored without being re-probed. Lenient on disk: an unknown future tier is
    /// lost as metadata, never allowed to make the primary session fail to parse.
    #[serde(default, deserialize_with = "de_soft_location")]
    pub tier: Option<Location>,
    /// **Where this server is, as a URL** — `"http://192.0.2.10:32400"`. Written since the origin
    /// model landed; **empty in every file written before it**, which is the whole reason
    /// [`ServerRef::origin`] has a fallback rather than an `Option`.
    ///
    /// It is a serialized [`Origin`] and not a `scheme` beside `address` because the two are not
    /// interchangeable: the host a TLS certificate is issued for is the `plex.direct` NAME, which
    /// `address` never holds (`origin.rs`). Storing the URL keeps the file legible to a human
    /// editing it on the television, which the struct-shaped alternative does not.
    ///
    /// **The `_url` suffix is not decoration**: this is the raw string, [`ServerRef::origin`] is
    /// the parsed value, and naming both `origin` would put a silent mix-up two characters away at
    /// every use. The FILE's key stays `origin`, which is what a human editing it reads.
    #[serde(default, rename = "origin")]
    pub origin_url: String,
}

impl ServerRef {
    /// **Where the primary server is.** [`ServerRef::origin`] when the file has one, else the
    /// legacy `http://{address}:{port}` — which is exactly what a file written before that field
    /// existed meant, and what every reader of this struct did with those two fields by hand.
    ///
    /// **TOTAL, unlike [`SourceRef::origin`].** The asymmetry is deliberate. A roster entry has
    /// [`SourceRef::usable`] in front of every caller, so `None` there costs one entry. This is
    /// the PRIMARY: `app.rs`'s boot gate and `auth::cancel` read it unconditionally, gated only by
    /// [`Session::can_go_local`], so a `None` here would be a NEW refusal on a path that has never
    /// had one — a silent sign-out at boot, which is the failure this whole field exists to avoid.
    /// The gate stays where it is, and the `port as i32` below is the same cast those readers were
    /// already doing, kept in one documented place instead of three.
    pub fn origin(&self) -> Origin {
        Origin::parse(&self.origin_url)
            .unwrap_or_else(|| Origin::http(&self.address, self.port as i32))
    }
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
    /// **The CREDIT** — whom to name, empty when there is nobody to name. It is
    /// [`super::servers::owner_credit`]'s answer, decided once at ingest (`auth::credit_of`), and
    /// deliberately NOT the raw `sourceTitle` it used to be: plex.tv puts the Plex Home ADMIN's
    /// handle here for a managed profile's own household server, so the raw field named the person
    /// watching. Empty therefore covers three cases and the UI treats them alike — our own server,
    /// the household's, and a share plex.tv did not name.
    ///
    /// The one string the browsing UI ever says about a source: "Shared by friend".
    pub shared_by: String,
    /// False ⇒ shared with us. A preference (ours sorts first, ours is `current`), never a wall.
    pub owned: bool,
    /// The address that answered `/identity` with the right `machineIdentifier` — not the first
    /// one advertised. An unmatched share's advertised local address may be this only through its
    /// TLS URI, after certificate and machine-identity verification; its plaintext form is gated.
    ///
    /// **Diagnostic metadata, and the LEGACY fallback.** It is what [`SourceRef::describe`] prints
    /// and what the Sources panel says; it is *not* what a connection is built from — that is
    /// [`SourceRef::origin`], and for an https server the two genuinely differ (`origin.rs`).
    pub address: String,
    pub port: i64,
    /// This identity's per-(user, server) `accessToken` for THIS server. A secret — never logged.
    /// Our own server's token gets a 401 from a share, which is why one token cannot serve both.
    pub token: String,
    /// The winning connection tier. It is restored onto `Client::link` only after registration,
    /// because re-pointing publishes a fresh client whose link starts unknown.
    #[serde(default, deserialize_with = "de_soft_location")]
    pub tier: Option<Location>,
    /// **Where this server is, as a URL** — the [`Origin`] the probe accepted, serialized. Empty
    /// in every file written before the field existed; [`SourceRef::origin`] falls back to
    /// `http://{address}:{port}` for those, which is what they meant. See [`ServerRef::origin`]
    /// for why that fallback exists at all, and [`ServerRef::origin_url`] for the `_url` suffix.
    #[serde(default, rename = "origin")]
    pub origin_url: String,
}

impl SourceRef {
    /// Everything about this source except the token, for the event log. The machine id is left
    /// out entirely — it is a permanent household fingerprint (`ui::stats`), and the event log is
    /// the file we ask users to send us.
    pub fn describe(&self) -> String {
        // Three states, not two: `owned` is plex.tv's flag about this ACCOUNT, and a source that is
        // not ours may still credit nobody — the household's own server seen by a managed profile,
        // or a share plex.tv never named. That case used to print the dangling `shared by ` with
        // the name missing, which reads as a bug in the logger rather than as the fact it is.
        let who = if self.owned {
            "ours".to_string()
        } else if self.shared_by.is_empty() {
            "not owned, uncredited".to_string()
        } else {
            format!("shared by {}", self.shared_by)
        };
        format!("{:?} {}:{} ({who})", self.name, self.address, self.port)
    }
    /// Enough to dial: an address, a **dialable** port, and the credential that server accepts.
    ///
    /// The port goes through [`probe::dial_port`](super::probe::dial_port) rather than a bare
    /// `> 0`, because this is the gate `auth::install_roster` filters on before `register(…,
    /// s.port as i32, …)` — and the session file is not a trusted input: it is JSON on disk that a
    /// hand edit, a truncated write or an older build can leave holding anything an `i64` can hold.
    /// An out-of-range port wraps in that cast; here it costs the entry instead, and `de_soft_vec`
    /// already establishes that one bad roster entry costs that entry and never the session.
    pub fn usable(&self) -> bool {
        self.origin().is_some() && !self.token.is_empty()
    }

    /// **Where to dial this source**, `None` when there is nothing dialable written down.
    ///
    /// [`SourceRef::origin`] when the file has one, else the legacy `http://{address}:{port}` — an
    /// entry written before the field existed, which is every entry in every session file on every
    /// television today. The port still goes through
    /// [`probe::dial_port`](super::probe::dial_port) on that path, for the reason
    /// [`SourceRef::usable`] gives: this file is JSON on disk that a hand edit or an older build
    /// can leave holding anything an `i64` can hold, and `port as i32` WRAPS.
    ///
    /// `Option`, unlike [`ServerRef::origin`], because every caller here is already behind
    /// [`SourceRef::usable`] — so `None` costs one roster entry, which is the rule `de_soft_vec`
    /// establishes for this whole struct.
    pub fn origin(&self) -> Option<Origin> {
        if !self.origin_url.is_empty() {
            return Origin::parse(&self.origin_url);
        }
        if self.address.is_empty() {
            return None;
        }
        super::probe::dial_port(self.port).map(|p| Origin::http(&self.address, p))
    }
}

/// One library the user answered about, named the only way a library CAN be named across two
/// servers: the server's machine id plus that server's own section key. Section keys are
/// server-local integers starting at 1 — both servers in the measured pair have a section `1`
/// (`docs/shared-servers.md` §2), so a bare key identifies nothing.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct PinnedLib {
    pub machine_id: String,
    pub key: i64,
}

/// **One profile's answer to "what goes on your Home?"** — the first-run route's record
/// (`Shared Sources.dc.html` deliverable F), and what the Library's Sources panel writes back
/// every time a switch is flipped.
///
/// **Both sides are recorded, and that is the field this type exists for.** A single "these are
/// pinned" list cannot tell *turned off* from *not answered about*, and the two must not be one
/// value: libraries arrive over time — a share whose server was slow to answer, a library the
/// owner created last week — and one that lands after the question was put has to fall on its own
/// DEFAULT (yours On, a friend's Off), not silently Off because it was absent from a list written
/// before it existed. That is also exactly what makes the design's "a share arriving later does
/// not reopen this screen" honest: it appears, unpinned, and the user finds it in the Sources
/// panel rather than being asked again.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct HomePins {
    /// The Plex Home user's `uuid`, or **empty for the account owner** with no Home selection —
    /// the same convention [`RecentSearches`] uses, and for the same reason: `uuid` and not `id`,
    /// because it is the identity that survives a roster refetch. A profile is keyed by something
    /// durable, never by its position in the roster, which reshuffles.
    pub user: String,
    /// The first-run question has been PUT to this profile. Separate from the two lists because a
    /// profile can be asked and answer with the defaults untouched, which writes nothing new —
    /// and being asked twice is precisely what a first-run screen must never do.
    pub asked: bool,
    /// libraries this profile turned ON …
    pub on: Vec<PinnedLib>,
    /// … and the ones it turned OFF. See the type doc: absent from both is "never answered for".
    pub off: Vec<PinnedLib>,
}

impl HomePins {
    /// This profile's recorded answer for one library: `Some(on)`, or `None` when the question was
    /// never put about *this* library and the caller owes it a default.
    pub fn answer(&self, machine_id: &str, key: i64) -> Option<bool> {
        let names =
            |v: &Vec<PinnedLib>| v.iter().any(|p| p.machine_id == machine_id && p.key == key);
        if machine_id.is_empty() {
            // An unknown machine id must not match the entries that have none either — the same
            // guard [`Session::source`] carries, and the same failure it avoids: one library
            // answering for every library on every server nobody has identified yet.
            return None;
        }
        match (names(&self.on), names(&self.off)) {
            (true, _) => Some(true),
            (false, true) => Some(false),
            (false, false) => None,
        }
    }
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
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(Vec::new());
    };
    Ok(match v {
        serde_json::Value::Array(items) => items
            .into_iter()
            .filter_map(|it| serde_json::from_value::<T>(it).ok())
            .collect(),
        // a null, an object, a string: not a list, so there is no list. Not an error.
        _ => Vec::new(),
    })
}

/// A persisted tier is diagnostic/policy metadata, not a credential gate. Missing, null,
/// malformed, or from a newer build therefore means "unknown" rather than failing the enclosing
/// `ServerRef` (which would turn one hand edit into a silent sign-out).
fn de_soft_location<'de, D>(d: D) -> Result<Option<Location>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<Option<Location>>(v).unwrap_or(None))
}

/// Playback quality is a preference, not a credential gate. A value written by a newer build or
/// damaged by a hand edit therefore degrades to the legacy-safe Original mode rather than making
/// the enclosing [`Session`] disappear.
fn de_soft_playback_quality<'de, D>(d: D) -> Result<Option<PlaybackQuality>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(None);
    };
    Ok(serde_json::from_value::<Option<PlaybackQuality>>(v).unwrap_or(None))
}

impl Session {
    /// The effective persisted playback quality. Absence is the literal legacy migration rule:
    /// builds that predate the field played Original, so they continue to play Original.
    pub(crate) fn playback_quality(&self) -> PlaybackQuality {
        self.playback_quality.unwrap_or(PlaybackQuality::Original)
    }

    /// Record an explicit user choice while leaving every unrelated session field intact.
    pub(crate) fn with_playback_quality(&self, quality: PlaybackQuality) -> Self {
        let mut next = self.clone();
        next.playback_quality = Some(quality);
        next
    }

    /// True once we have a LAN server + a usable PMS token — i.e. we can run offline.
    ///
    /// The PORT is part of "we have a server", and this is the only gate in front of it: every
    /// resume path (`app.rs`'s boot gate, `auth::cancel`) reads `server.port as i32` straight into
    /// `plex::install` on the strength of this answer. A port outside `1..=65535` wraps in that
    /// cast into a plausible one, so it is refused here — the app lands on sign-in, which is the
    /// honest report for a session it cannot dial, rather than talking to a port nobody named. It
    /// also covers `port` simply being ABSENT from an older file (`#[serde(default)]` = 0), which
    /// could never have connected either.
    pub fn can_go_local(&self) -> bool {
        self.server_dialable() && !self.pms_token().is_empty()
    }

    /// Is the primary's address one this app could actually open a socket to?
    ///
    /// Split out of [`Session::can_go_local`] because [`ServerRef::origin`] is deliberately total
    /// (see its doc) — so the refusal that used to be implicit in reading `address`/`port` has to
    /// be stated somewhere, and this is it. A stored ORIGIN is judged by whether it parses at all
    /// (`Origin::parse` refuses an undialable port and a scheme this app does not speak); a legacy
    /// file with no origin is judged exactly as before.
    ///
    /// **It asks "is there a supported address written down", not whether the network answers
    /// now.** `Origin::parse` accepts the two schemes the control and media transports implement,
    /// plus hostname/IPv4/IPv6 authorities with dialable ports. Reachability is measured after
    /// restore by the ordinary request/probe paths; refusing an offline but well-formed session
    /// here would wrongly send its user back to the QR flow.
    fn server_dialable(&self) -> bool {
        if !self.server.origin_url.is_empty() {
            return Origin::parse(&self.server.origin_url).is_some();
        }
        !self.server.address.is_empty() && super::probe::dial_port(self.server.port).is_some()
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

    /// **Is the profile currently watching the one [`Session::account_token`] belongs to?**
    ///
    /// That token is the account OWNER's (the Plex Home admin's). It is written once, by the QR
    /// sign-in, and a profile switch never replaces it — the switched user's own account token is
    /// fetched, used for one `/api/v2/resources`, and dropped. So anything asked of plex.tv with it
    /// is answered ABOUT THE OWNER: every `accessToken` that comes back is the owner's
    /// per-(user, server) grant, and a restricted profile's answer would have been a shorter list.
    /// A caller that installs those tokens while somebody else is watching has swapped identities
    /// under them, which is why this exists as a gate rather than as a display fact.
    ///
    /// `true` for the owner with or without Plex Home; `false` for a managed profile — **and false
    /// when the roster cannot say.** "We cannot prove this is the owner" and "this is the owner"
    /// must not be one value on a question whose wrong answer is another identity's credentials:
    /// `home_users` is empty for "never fetched" as well as for "no Plex Home"
    /// (see [`Session::account`]), so the two are only told apart by a uuid actually being set.
    pub fn active_profile_is_admin(&self) -> bool {
        if self.user.uuid.is_empty() {
            // No Plex Home selection was ever made, so there is no managed profile to be: auth's
            // single-user path enters Home on the owner's own server token without writing one.
            return true;
        }
        self.home_users
            .iter()
            .find(|u| u.uuid == self.user.uuid)
            .map(|u| u.admin)
            .unwrap_or(false)
    }

    /// **Everyone in this house, as plex.tv account ids** — the input to
    /// [`super::servers::is_household`] and so to the one "Shared by …" rule: a server whose
    /// `ownerId` is in here belongs to the household and credits nobody.
    ///
    /// **The Plex Home ROSTER and nothing else, so that an empty answer means exactly one thing:
    /// the roster could not answer.** The rule leans on that — it falls back to plex.tv's
    /// undocumented `home` flag precisely when this list is empty — so anything else in here would
    /// be an id that silences the fallback without being able to replace it.
    ///
    /// [`UserRef::id`], the WATCHING profile's own id, is therefore deliberately NOT included, and
    /// it took a review to see why it must not be. It looks free (a server owned by the person
    /// watching is already `owned:true` to them, so it can never be the id that decides a case) and
    /// it is not: a session upgraded from a build without [`HomeUserRef::id`] has a whole roster of
    /// zeroes and a `user.id` the `/switch` wrote long ago, which made this return
    /// `[the-managed-profile]` — non-empty, so `home` was ignored — while the id that would have
    /// mattered, the ADMIN's, was one of the zeroes that got filtered. That is the exact legacy
    /// session the fallback exists for, and it was the only one it could not reach.
    ///
    /// **An empty answer means "we cannot enumerate the house", never "the house is empty"** —
    /// exactly the trap [`Session::active_profile_is_admin`] documents one function up, and it
    /// falls the same way: an id we do not hold matches nothing, so the rule degrades to plex.tv's
    /// own `owned`/`home` flags rather than to a confident wrong answer about somebody's server.
    /// `0` is filtered because it is the "no id" value on both sides of the comparison — an entry
    /// from a file written before [`HomeUserRef::id`] existed, and `Resource::ownerId` on our own
    /// server — and letting those two zeroes meet would credit-suppress by accident.
    ///
    /// In practice the roster is rewritten WHOLESALE from one `/api/v2/home/users` response, so it
    /// is all zeroes (an old build wrote it) or none. That is the writer's behaviour and not an
    /// invariant anything enforces — `HomeUser::id` is individually `#[serde(default)]`, so a wire
    /// shape omitting one member's id would yield a mixed roster. A mixed roster degrades the right
    /// way regardless: the ids present still decide their own cases, and the ones missing fall
    /// through to the same "outside the house" default an un-enumerable roster gets, one member at
    /// a time instead of all at once.
    pub fn household_ids(&self) -> Vec<i64> {
        self.home_users
            .iter()
            .map(|u| u.id)
            .filter(|&id| id != 0)
            .collect()
    }

    /// **Is the profile this session would resume as behind a PIN?**
    ///
    /// The other flag on the same roster row as [`Session::active_profile_is_admin`], read for the
    /// one question the boot who's-watching picker has to answer: may BACK out of it silently
    /// reinstate what is on disk? A PIN-protected profile is one plex.tv validates a code for on
    /// every switch (`auth::submit_pin` → `AccountClient::switch_user`), so resuming it without
    /// one hands out precisely the session the PIN exists to gate — see [`crate::auth::cancel`].
    ///
    /// It answers the OPPOSITE way to `active_profile_is_admin` when the roster cannot say, and
    /// for the same reason: on each question, "we cannot prove it" must land on the side whose
    /// wrong answer costs nothing. There it is somebody else's credentials, so an unknown uuid is
    /// not the owner; here it is a bypassed PIN, so an unknown uuid is treated as protected. The
    /// cost of being wrong is one profile pick — the picker is still fully usable, and its
    /// *Sign out* pill is reachable with the roster empty.
    ///
    /// **An EMPTY uuid answers TRUE**, and it is the case worth spelling out, because it reads as
    /// the harmless one ("no profile chosen, so no PIN to be behind") and is the opposite. A
    /// sign-in ABANDONED at the who's-watching picker persists exactly that shape: `auth`'s
    /// `login_thread` saves the account token, the server and the roster the moment they exist —
    /// deliberately, so that walking away does not cost the whole sign-in — and no profile has been
    /// picked. Such a session's [`Session::pms_token`] falls back to the OWNER's server token, and
    /// the next boot raises a picker over it (the gate needs a roster of more than one user, which
    /// that file has). So "no profile chosen" is not "no PIN": it is *nobody has said who they
    /// are*, and the picker is that question — which is why it belongs on the same side as an
    /// unknown uuid rather than opposite it.
    pub fn active_profile_is_protected(&self) -> bool {
        if self.user.uuid.is_empty() {
            return true; // see above — nobody has said who they are
        }
        self.home_users
            .iter()
            .find(|u| u.uuid == self.user.uuid)
            .map(|u| u.protected)
            .unwrap_or(true)
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
    /// One profile's Home selection, or `None` for a profile that has never been asked. The
    /// difference is load-bearing — see [`Session::home_pins`].
    pub fn pins_for(&self, user: &str) -> Option<&HomePins> {
        self.home_pins.iter().find(|p| p.user == user)
    }

    /// Replace one profile's answer, leaving every OTHER profile's alone. A method rather than a
    /// field assignment at the call site for [`Session::set_recents_for`]'s reason: the writer
    /// holds a whole `Session`, and the obvious `Session { home_pins: mine, ..s }` would silently
    /// delete everybody else's selection.
    pub fn set_pins_for(&mut self, user: &str, pins: HomePins) {
        match self.home_pins.iter_mut().find(|p| p.user == user) {
            Some(slot) => *slot = pins,
            None => self.home_pins.push(pins),
        }
    }

    /// One profile's search terms — empty for a profile that has never searched, which is the same
    /// answer as "never chosen" and needs no distinction here.
    pub fn recents_for(&self, user: &str) -> &[String] {
        self.recent_searches
            .iter()
            .find(|r| r.user == user)
            .map(|r| &r.terms[..])
            .unwrap_or(&[])
    }

    /// Replace one profile's terms, leaving every OTHER profile's alone. That last part is the
    /// reason this is a method rather than a field assignment at the call site: the writer holds a
    /// whole `Session` and the obvious `Session { recent_searches: mine, ..s }` would silently
    /// delete everybody else's history.
    pub fn set_recents_for(&mut self, user: &str, terms: Vec<String>) {
        if let Some(r) = self.recent_searches.iter_mut().find(|r| r.user == user) {
            r.terms = terms;
        } else if !terms.is_empty() {
            self.recent_searches.push(RecentSearches {
                user: user.to_string(),
                terms,
            });
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
/// other writer there is: the server-roster worker (`auth::refresh_roster`), the
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
    let s = {
        let _io = io();
        peek_locked()
    };
    // The cold-cache fallback inside `peek_locked` can be the first `read_locked` in the process
    // (see its own doc) and can therefore queue a storage report the same way `load` can — drained
    // here for the same reason every other public entry point drains: `IO` must already be
    // released before `report_error` does its own spool I/O.
    drain_pending_reports();
    s
}

/// [`peek`] with the lock already held — the read half every entry point here shares.
///
/// Serves [`CACHE`] once anything has been published to it in this process, and only falls back to
/// a real [`read_locked`] the first time — before any [`load`]/[`save_locked`]/[`update`] in this
/// process has run. In production that first read is always [`load`]'s own, at boot; this fallback
/// exists so [`peek`] is never wrong in that narrow window rather than to be the common path.
///
/// **That cold-cache fallback also sets [`LOCKED_STATE`]/[`LOCKED_PATH`]**, same as any other
/// `read_locked` call — so in the (narrow, boot-only) window before the first `load`, a `peek`
/// reachable from a keypress can be the read that later authorizes [`save_locked`]'s plaintext
/// recovery write. That is intentional, not an oversight: the verdict recorded is a fact about
/// what is ON DISK, true regardless of which caller's read happened to observe it first, and a
/// recovery write still only fires on an actual fresh sign-in later — a `peek` alone never writes.
fn peek_locked() -> Session {
    if let Some(s) = cached() {
        return s;
    }
    let s = match read_locked() {
        ReadState::Ready { session, .. } => session,
        ReadState::Missing | ReadState::Locked { .. } => Session::default(),
    };
    publish_cache(s.clone());
    s
}

const SECURE_FORMAT: &str = "plxnative-secure-session";

#[derive(Deserialize, Serialize)]
struct SecureEnvelope {
    format: String,
    version: u8,
    sealed: crate::keymanager::Sealed,
}

enum ReadState {
    Missing,
    Ready {
        session: Session,
        plaintext: bool,
    },
    /// A recognized encrypted file whose device key is temporarily or permanently unavailable.
    /// It must shadow every lower-priority candidate: treating it as corrupt and then writing a
    /// fresh client id would destroy the only copy of the credentials. `recoverable` says whether
    /// [`save_locked`] may replace this file on a fresh sign-in — see [`LOCKED_RECOVERABLE`].
    Locked {
        recoverable: bool,
    },
}

/// Record what this read found in [`LOCKED_STATE`] (and, for a recoverable lock, which candidate
/// path it was found at, in [`LOCKED_PATH`]) and hand back the same [`ReadState`] — every return
/// point in [`read_locked`] goes through one of these two so the three stay in lockstep.
fn locked(
    recoverable: bool,
    path: &std::path::Path,
    refusal: Option<crate::keymanager::LastRefusal>,
) -> ReadState {
    LOCKED_STATE.store(
        if recoverable {
            LOCKED_RECOVERABLE
        } else {
            LOCKED_UNRECOVERABLE
        },
        std::sync::atomic::Ordering::Relaxed,
    );
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) =
        recoverable.then(|| path.to_path_buf());
    // Issue #76 storage telemetry: a read landing Locked is one of the two triggers
    // `save_locked`'s own seal failure is the other — for a handled report, reported at most once
    // per process per stage (`report_once`, drained by the caller after `IO` is released).
    // `recoverable` is exactly `keymanager::open` having been attempted and failed
    // (`EnvelopeLocked`) versus a shape this build never asked a key manager to open at all — an
    // unrecognized envelope, or one that decrypted fine but did not parse as a session
    // (`EnvelopeUnparseable`), where there is no service reply to attach a code from.
    // When the open reached a stage of its own (`begin_decrypt` with a code, `no_reply`,
    // `unreachable`, …) the report carries THAT — it is the question a dashboard on these sets
    // needs answered, and `EnvelopeLocked` says only that the envelope did not open.
    use crate::telemetry::storage::{StorageErrorContext, StorageStage};
    let stage = match (recoverable, refusal) {
        (true, Some(r)) => r.stage,
        (true, None) => StorageStage::EnvelopeLocked,
        (false, _) => StorageStage::EnvelopeUnparseable,
    };
    let service_error_code = refusal.and_then(|r| r.error_code);
    queue_report(StorageErrorContext {
        stage,
        service_error_code,
        class: storage_class(),
        refused_marker: has_refused_marker(),
    });
    ReadState::Locked { recoverable }
}

fn not_locked(state: ReadState) -> ReadState {
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(
        match &state {
            ReadState::Ready { plaintext: true, .. } => CLASS_PLAINTEXT,
            ReadState::Ready { plaintext: false, .. } => CLASS_SECURE,
            ReadState::Missing | ReadState::Locked { .. } => CLASS_NONE,
        },
        std::sync::atomic::Ordering::Relaxed,
    );
    state
}

/// The first usable candidate, retaining whether an encrypted file exists but cannot be opened.
fn read_locked() -> ReadState {
    for path in auth_paths() {
        let Some(bytes) = read_owned_regular(&path) else {
            continue;
        };
        if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
            if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                let (plain, refusal) = crate::keymanager::open_checked(&envelope.sealed);
                let Some(plain) = plain else {
                    crate::log("session: secure file is present but its device key is unavailable");
                    // Write the CROSS-LAUNCH marker on ANY failure to open an envelope this install
                    // wrote, with the stage that failed recorded in it. `refusal` comes straight
                    // back from THIS `open_checked` call (not the racy global), and since the
                    // second issue #76 review it is `Some` for a timeout (`no_reply`) and a failed
                    // registration (`unreachable`) as well as for a service reply — the first
                    // review's "reply-only" gate left a STALLED keymanager3 (the shape both
                    // reporters' "slow, then try again" symptom points at) re-paying its 4 s
                    // budget on every later launch and never reporting, because no marker and no
                    // stage were ever recorded. The envelope's existence proves this install once
                    // sealed; failing to reopen it is exactly the class the marker remembers. A
                    // healthy set that hiccuped once pays for the wrong marker only at its next
                    // FRESH sign-in (a 0600 file instead of an envelope, until sign-out) — an
                    // ordinary `update()` never converts a present envelope (`save_locked`'s
                    // guard), and reads are never gated on it, so the same envelope still opens on
                    // the next launch that can. `None` here is only the unsupported-key-name
                    // shape `open_checked` refuses before any call, which is not this install's.
                    if let Some(refusal) = refusal {
                        write_refused_marker(refusal.stage);
                    }
                    return locked(true, &path, refusal);
                };
                return match serde_json::from_slice(&plain) {
                    Ok(session) => not_locked(ReadState::Ready {
                        session,
                        plaintext: false,
                    }),
                    // Decrypted fine but the plaintext is not a session — a real corruption, not a
                    // keymanager3 refusal, so it does not get the recoverable rewrite.
                    Err(_) => locked(false, &path, None),
                };
            }
        }
        if identifies_secure_envelope(&bytes) {
            crate::log("session: unsupported or damaged secure envelope is locked");
            return locked(false, &path, None);
        }
        if let Ok(session) = serde_json::from_slice(&bytes) {
            return not_locked(ReadState::Ready {
                session,
                plaintext: true,
            });
        }
    }
    not_locked(ReadState::Missing)
}

fn identifies_secure_envelope(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .is_some_and(|o| {
            o.get("format").and_then(serde_json::Value::as_str) == Some(SECURE_FORMAT)
                || (o.contains_key("sealed") && o.contains_key("version"))
        })
}

fn has_secure_locked() -> bool {
    auth_paths()
        .iter()
        .any(|path| read_owned_regular(path).is_some_and(|b| identifies_secure_envelope(&b)))
}

/// Seed a quality only for a genuinely absent file. A parsable legacy file remains distinguishable
/// even when it omitted `client_id`; otherwise opening the Auto gate in a future build would turn
/// that old install into a fresh one merely because its identifier also needed repair.
fn seed_fresh_quality(s: &mut Session, persisted: bool, auto_ready: bool) {
    if !persisted && s.playback_quality.is_none() {
        s.playback_quality = Some(PlaybackQuality::fresh_default(auto_ready));
    }
}

/// **Hand the scrubber this household's names**, so `crate::log` can redact them without ever
/// touching this module.
///
/// The scrubber used to call [`peek`] per line, which took [`IO`] and read the file — a deadlock
/// against every writer here (`save_locked` logs while holding the lock) and a syscall storm on
/// the log path besides. Ownership is inverted now: the session layer PUSHES on every change and
/// `diag::scrub` keeps a cached snapshot.
///
/// Called on load, on save and on a successful `update`, i.e. everywhere the set of names can
/// move — including a user switch and a roster refresh, both of which land through `update`.
fn publish_identities(s: &Session) {
    let mut v: Vec<String> = vec![
        s.server.name.clone(),
        s.server.machine_id.clone(),
        s.user.title.clone(),
    ];
    for u in &s.home_users {
        v.push(u.title.clone());
        v.push(u.uuid.clone());
    }
    for src in &s.sources {
        v.push(src.name.clone());
        v.push(src.machine_id.clone());
        v.push(src.shared_by.clone());
    }
    crate::diag::scrub::set_identities(v);
}

/// Load the persisted session, ensuring a stable `client_id` exists (generated + saved on first
/// boot). Never returns an error — a missing/corrupt file degrades to a fresh, logged-out session.
/// Falls back to the pre-relocation path once and re-saves at the new one (migration).
///
/// **Served from [`CACHE`] once anything has been published to it in this process** — the same
/// fast path [`peek_locked`] already takes, extended to cover `load`'s own ~9 mid-run callers
/// (the account chip's `signed_in()`, a Settings rebuild, `plex::servers`'s per-registration device
/// id, an auth cancel/restart). This process's own writers keep `CACHE` in lockstep with the file
/// (see its doc), so a repeat `load` gains nothing by re-reading — except paying keymanager3's
/// multi-second LS2 budget a second time, and letting a transient mid-run decrypt hiccup on some
/// unrelated file access overwrite [`LOCKED_STATE`]/[`LOCKED_PATH`] with a verdict about a file
/// this run already read successfully once, which a LATER save's recovery decision then trusts.
/// Only the FIRST `load` in a process — genuinely the boot path — does the full read/mint/reseal
/// work below.
pub fn load() -> Session {
    let s = {
        let _io = io();
        if let Some(s) = cached() {
            s
        } else {
            let read = read_locked();
            let persisted = !matches!(read, ReadState::Missing);
            let locked = matches!(read, ReadState::Locked { .. });
            let plaintext = matches!(
                read,
                ReadState::Ready {
                    plaintext: true,
                    ..
                }
            );
            let mut s = match read {
                ReadState::Ready { session, .. } => session,
                ReadState::Missing | ReadState::Locked { .. } => Session::default(),
            };
            seed_fresh_quality(&mut s, persisted, crate::route::auto_quality_ready());
            if s.client_id.is_empty() {
                s.client_id = new_client_id();
                if !locked {
                    save_locked(&s);
                }
            } else if plaintext {
                // Offer every plaintext session to the Key Manager immediately. This also moves a
                // parsable legacy-path file to the preferred location; without a usable service it
                // stays an atomic mode-0600 plaintext fallback.
                save_locked(&s);
            }
            publish_identities(&s);
            // Whatever this run ends up believing the session is — even the ephemeral default
            // that comes from a Locked or Missing read — becomes the in-process truth every later
            // `peek` serves.
            publish_cache(s.clone());
            s
        }
    };
    // Issue #76: a read landing Locked, or a save's own seal failure, may have queued a handled
    // report above — sent only now that `IO` has been released (see `PENDING_REPORTS`'s doc). The
    // cached fast path above never itself queues one (it does no `read_locked`/`save_locked`), but
    // still passes through here rather than a bare early `return`, so it cannot silently start
    // skipping this the moment that path ever changes.
    drain_pending_reports();
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
///
/// **Also refuses on a [`LOCKED_RECOVERABLE`]/[`LOCKED_UNRECOVERABLE`] read.** A `load` on a
/// genuinely locked boot still mints an EPHEMERAL client id (never persisted for exactly this
/// reason) so the empty-id test above no longer catches it — an unrelated writer with no
/// credentials of its own (the home-pin, recents or quality-rung `update`s) must not be the thing
/// that turns a recognized-but-unopenable secure envelope into a credential-free plaintext file;
/// only a fresh SIGN-IN, through [`save_locked`]'s own recoverable branch, may do that.
pub fn update(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    let wrote = {
        let _io = io();
        let cur = peek_locked();
        if cur.client_id.is_empty()
            || LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) != NOT_LOCKED
        {
            false
        } else {
            match edit(&cur) {
                Some(next) => {
                    save_locked(&next);
                    true
                }
                None => false,
            }
        }
    };
    drain_pending_reports();
    wrote
}

/// Persist the session (best-effort; a write failure is non-fatal — we just re-login next boot).
///
/// **A whole-file REPLACE.** Use it only where the caller genuinely owns the entire file — the
/// sign-in flow and the profile switch, which built their `Session` from this same file moments
/// earlier. Anything changing one field of a file somebody else also writes must go through
/// [`update`], or it overwrites their change with whatever it last read.
///
/// **Credentials at rest: device-key encryption when an authenticated public Key Manager is
/// available, and 0600 in every case.** The probe uses TV 24+'s
/// `com.webos.service.keymanager3`. The legacy `com.palm.keymanager` service is not used because
/// its AES-CFB interface cannot authenticate ciphertext. A firmware that does not expose or permit
/// keymanager3 keeps the compatible 0600 plaintext fallback. An existing encrypted file is
/// preserved through a transient failure to protect it again — **except** when THIS process itself
/// read that exact file as [`LOCKED_RECOVERABLE`] (keymanager3 sealed it once but cannot open it
/// now, on this launch, this firmware — issue #76) and the save carries a fresh `account_token`: a
/// sign-in nobody could ever read back is worse than one written down in plain sight, and there is
/// nothing the locked ciphertext holds that the fresh sign-in does not already re-supply. That save
/// does not even ask the key manager to try again — a backend proven unable to open what THIS run
/// found on disk is not asked to seal a new envelope that could turn out just as unreadable next
/// launch; see [`save_locked`]'s recoverable branch.
///
/// **The verdict outlives the launch that found it.** A per-process refusal alone would only ever
/// interrupt the loop for one boot — issue #76's own robustness review measured the sequence
/// (`docs/…` — see the module doc's cross-launch paragraph): launch 2 recovers to plaintext, but
/// launch 3 reads that plaintext cleanly, so ITS OWN [`LOCKED_STATE`] never becomes
/// [`LOCKED_RECOVERABLE`] — and if the key manager happens to round-trip fine within launch 3 (the
/// exact "works per-launch, not across launches" shape the bug reports describe), an ordinary
/// `update()` (a roster refresh, a pin) re-seals it, and launch 4 is locked again. So once
/// [`read_locked`] proves an envelope unopenable, that fact is ALSO written to a small 0600 marker
/// beside the session file (see [`write_refused_marker`]) — content only, never key material —
/// and every later save on this install, this launch's or any other's, checks it before ever
/// calling `keymanager::seal`. An install that has once failed to read its own envelope back stays
/// on the 0600 file until [`clear`] (sign-out or erase), which removes the marker together with
/// the session — a different account, or a future firmware, gets a fresh chance.
///
/// The mode is set in `open(2)`'s own argument — never create-then-chmod. `fs::write` creates with
/// `0666 & !umask` (0644 here), so a fallback token file would be readable by every other uid from
/// the instant it hit the disk. Passing the mode through `OpenOptionsExt` means it never *exists*
/// in a permissive mode, which a chmod after the write cannot promise.
pub fn save(s: &Session) {
    {
        let _io = io();
        save_locked(s);
    }
    drain_pending_reports();
}

/// [`save`] with the lock already held.
fn save_locked(s: &Session) {
    // Before the write, not after: a failed persist still means these names are live in THIS run,
    // and the log wants them redacted either way.
    publish_identities(s);
    let Ok(json) = serde_json::to_vec_pretty(s) else {
        return;
    };

    // **Consulted BEFORE calling `keymanager::seal`, not only after it fails** — both halves of
    // `seal_permitted`. `LOCKED_STATE` records whether THIS process's own `read_locked` found the
    // on-disk envelope unopenable; the marker records whether ANY process ever did. Either way,
    // `keymanager::seal`'s round trip proves only an IN-PROCESS, same-launch decrypt — a backend
    // whose key is not usable by a DIFFERENT launch (or LS2 registration) than the one that sealed
    // it would round-trip perfectly right here and hand back a fresh envelope in exactly the same
    // unreadable shape, so a save landing here does not even ask the key manager to try again.
    let marker_present = has_refused_marker();
    let locked_state = LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed);
    let saving_fresh_sign_in = !s.account_token.is_empty();
    if !seal_permitted(marker_present, locked_state, saving_fresh_sign_in) {
        if locked_state == LOCKED_RECOVERABLE && saving_fresh_sign_in {
            // The more specific case: THIS launch's own read found the envelope, and knows
            // exactly which candidate to target and sweep.
            return recover_locked_session_as_plaintext(s, &json);
        }
        // The marker-only case: a PRIOR launch found it, and this one may never have seen the
        // secure file at all (it could already be plaintext, or `LOCKED_STATE` could still be
        // sitting at its default for an unrelated reason).
        //
        // **But never on THIS save's own authority alone when it carries no fresh credentials.**
        // The marker is a stale, cross-launch fact; a secure-shaped file could still be sitting on
        // disk from a firmware that has since started working again (the same reasoning the seal-
        // failure branch below already applies via `has_secure_locked`). An ordinary `update()` —
        // a roster refresh, a pinned library — must not be what silently converts a currently
        // present secure envelope to plaintext; only a fresh sign-in (which re-supplies the
        // credentials the marker's own downgrade would otherwise discard) may do that.
        if !saving_fresh_sign_in && has_secure_locked() {
            crate::log(
                "session: secure storage is marked refused, but a secure file is present; leaving it untouched",
            );
            return;
        }
        return write_refused_plaintext(s, &json);
    }

    if let Some(sealed) = crate::keymanager::seal(&json) {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed,
        };
        let Ok(protected) = serde_json::to_vec_pretty(&envelope) else {
            return;
        };
        for winner in auth_paths() {
            if write_atomic(&winner, &protected) {
                // A successful migration must not leave an older plaintext token file at a
                // lower-priority jail path where another uid can recover it.
                for stale in auth_paths().into_iter().filter(|p| p != &winner) {
                    remove_temp_siblings(&stale);
                    let _ = std::fs::remove_file(stale);
                }
                not_locked_after_write();
                LAST_CLASS.store(CLASS_SECURE, std::sync::atomic::Ordering::Relaxed);
                publish_cache(s.clone());
                return;
            }
        }
        crate::log("session: key manager succeeded but the protected file could not be written");
        return;
    }
    // `seal` failed for a reason unrelated to a locked-boot read (that case returned above).
    // Never turn an already protected session back into plaintext because a service was
    // temporarily unavailable during a save. Preserve the previous ciphertext instead.
    //
    // Issue #76 storage telemetry: this IS "a seal round trip fails" — `keymanager::seal` already
    // logged its own refusal (or the round-trip mismatch) and published it as `last_refusal`, which
    // is the stage and code a handled report needs. A backend that is simply ABSENT (no keymanager3
    // on this firmware at all) never reaches a service call and leaves `last_refusal` at `None`, so
    // an ordinary plaintext-only install reports nothing here.
    // A `no_reply`/`unreachable` here is NOT reported: on an install that has never sealed, a
    // service that does not answer is indistinguishable from a firmware that has no keymanager3
    // at all (every set before webOS 24), and reporting it would send one StorageError from every
    // such install's first save. Those two stages are evidence only on the READ side, where the
    // envelope's existence proves the service once worked (`read_locked`).
    if let Some(refusal) = crate::keymanager::last_refusal().filter(|r| {
        r.error_code.is_some()
            || r.stage == crate::telemetry::storage::StorageStage::RoundtripMismatch
    }) {
        queue_report(crate::telemetry::storage::StorageErrorContext {
            stage: refusal.stage,
            service_error_code: refusal.error_code,
            class: storage_class(),
            refused_marker: has_refused_marker(),
        });
    }
    if has_secure_locked() {
        crate::log("session: preserving the existing secure file; refusing a plaintext downgrade");
        return;
    }
    // Try each candidate; the first that accepts the write wins. A total failure is still
    // non-fatal — but it is LOGGED, because the symptom (sign in again, every boot, forever) is
    // otherwise indistinguishable from a server-side auth problem and impossible to report.
    for path in auth_paths() {
        if write_atomic(&path, &json) {
            LAST_CLASS.store(CLASS_PLAINTEXT, std::sync::atomic::Ordering::Relaxed);
            publish_cache(s.clone());
            return;
        }
    }
    crate::log(
        "session: could not persist to ANY candidate path — login will not survive a reboot",
    );
}

fn not_locked_after_write() {
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Issue #76's recovery write: this process's own read already proved the on-disk envelope at
/// [`LOCKED_PATH`] cannot be opened, and `s` carries a fresh sign-in's own credentials — nothing
/// the locked ciphertext held that this save does not re-supply. Refusing it is what produced the
/// endless loop; writing the 0600 fallback in its place is what ends it.
///
/// Targets the SAME candidate `read_locked` found the envelope at first — not merely the first
/// candidate willing to accept a write, which can be a different (lower-priority) path when the
/// locked file's own candidate is readable but not writable. Whichever path the write actually
/// lands at, every OTHER candidate is swept the same way a successful seal already does, so the
/// locked file cannot survive at a lower-priority path and keep shadowing the fresh sign-in on the
/// next boot.
fn recover_locked_session_as_plaintext(s: &Session, json: &[u8]) {
    let previously_locked_at = LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match write_plaintext_recovery(s, json, previously_locked_at.as_deref()) {
        Some(winner) => {
            crate::log(
                "session: the secure file could not be opened on this firmware; replaced by the 0600 file so the sign-in survives a reboot",
            );
            if let Some(locked_at) = &previously_locked_at {
                if locked_at != &winner && locked_at.exists() {
                    crate::log(
                        "session: a locked file at a lower-priority path could not be removed after recovery — it may still shadow the fresh sign-in",
                    );
                }
            }
        }
        None => crate::log(
            "session: could not persist to ANY candidate path — login will not survive a reboot",
        ),
    }
}

/// The marker-only counterpart to [`recover_locked_session_as_plaintext`]: a PRIOR launch —
/// possibly not this one — already proved an envelope on this install unopenable, so
/// [`has_refused_marker`] alone is enough to skip `keymanager::seal`. Unlike the per-process case
/// there is no [`LOCKED_PATH`] to target (this launch's own read may have found the file already
/// plaintext, or never touched it at all), so the write goes through the normal candidate priority
/// order.
fn write_refused_plaintext(s: &Session, json: &[u8]) {
    log_marker_skip_once();
    if write_plaintext_recovery(s, json, None).is_none() {
        crate::log(
            "session: could not persist to ANY candidate path — login will not survive a reboot",
        );
    }
}

/// Write `s` as the 0600 plaintext file, replacing any secure envelope, and sweep every other
/// candidate clean — the shared mechanics behind both [`recover_locked_session_as_plaintext`] and
/// [`write_refused_plaintext`]. `target`, when known, is the SAME candidate the locked envelope was
/// found at (`recover_locked_session_as_plaintext`'s case) rather than merely the first candidate
/// willing to accept a write, which can differ when a lower-priority candidate is writable but the
/// one actually holding the locked file is not — that would leave the locked file in place, still
/// shadowing everything below it, while a stray plaintext copy accumulates elsewhere. Returns the
/// path actually written, or `None` if every candidate refused.
fn write_plaintext_recovery(
    s: &Session,
    json: &[u8],
    target: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let mut candidates = auth_paths();
    if let Some(first) = target {
        candidates.retain(|p| p != first);
        candidates.insert(0, first.to_path_buf());
    }
    for winner in candidates {
        if !write_atomic(&winner, json) {
            continue;
        }
        for stale in auth_paths().into_iter().filter(|p| p != &winner) {
            remove_temp_siblings(&stale);
            let _ = std::fs::remove_file(&stale);
        }
        not_locked_after_write();
        LAST_CLASS.store(CLASS_PLAINTEXT, std::sync::atomic::Ordering::Relaxed);
        publish_cache(s.clone());
        return Some(winner);
    }
    None
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
/// The tmp file is a **sibling**, named off the resolved path. `rename(2)` is
/// only atomic within one filesystem, and the webOS jail's writable directories are separate mounts
/// (`/media/developer`, `/media/internal`, the app dir — see [`auth_paths`]); a tmp under `/tmp`
/// would demote this to a cross-device copy, i.e. exactly the truncate-in-place it replaces. Its
/// suffix is random and opened with `create_new` + `O_NOFOLLOW`: the module lock serializes our
/// writers, but it does not serialize another uid able to create a sibling entry.
///
/// `sync_all` before the rename and on the parent after it is what makes the promise survive the
/// plug being pulled, which on a
/// television is an ordinary way to end a session: without it the rename can be visible while the
/// data behind it is not, and the file that comes back is the empty one. It costs a flush of a
/// couple of kilobytes on a path that runs at sign-in, at a profile switch, at a roster change and
/// at a committed search term — never per frame.
///
/// The 0600 mode is [`save`]'s rule applied one file earlier: the secret must never *exist* in a
/// permissive mode, and the tmp file is where it exists first.
/// `pub(crate)` since 2026-08-29 so `crate::telemetry` writes its file the same way rather than
/// growing a second implementation of this. It is a generic 0600 atomic write that happens to live
/// beside its first caller; the alternative was two copies of a routine whose whole value is that
/// its failure modes have already been found once, on the file holding the credentials.
pub(crate) fn write_atomic(path: &std::path::Path, json: &[u8]) -> bool {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
            return false;
        }
    }
    let Some((tmp, mut f)) = create_private_temp(path) else {
        return false;
    };
    let written = f.write_all(json).is_ok() && f.sync_all().is_ok();
    drop(f); // the rename must not race our own open handle on a filesystem that cares
    if written && std::fs::rename(&tmp, path).is_ok() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        remove_temp_siblings(path);
        return true;
    }
    // Leave no half-written credentials behind under a name the next writer would overwrite
    // anyway — and none at all if this candidate turned out to be unwritable.
    let _ = std::fs::remove_file(&tmp);
    false
}

fn create_private_temp(path: &std::path::Path) -> Option<(std::path::PathBuf, std::fs::File)> {
    use std::os::unix::fs::OpenOptionsExt;
    for attempt in 0..16u64 {
        let tmp = random_tmp_path(path, attempt)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
        {
            Ok(file) => return Some((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return None,
        }
    }
    None
}

fn random_tmp_path(path: &std::path::Path, attempt: u64) -> Option<std::path::PathBuf> {
    use std::io::Read;
    static FALLBACK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let mut nonce = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut nonce))
        .is_err()
    {
        nonce = FALLBACK
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(attempt)
            .to_ne_bytes();
    }
    let mut name = path.file_name()?.to_os_string();
    name.push(format!(".tmp.{:016x}", u64::from_ne_bytes(nonce)));
    Some(path.with_file_name(name))
}

pub(crate) fn read_owned_regular(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    const MAX_FILE: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= MAX_FILE).then_some(bytes)
}

fn remove_temp_siblings(path: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    if let Some(legacy) = tmp_path(path) {
        let _ = std::fs::remove_file(legacy);
    }
    let (Some(parent), Some(file_name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let prefix = format!("{}.tmp.", file_name.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        if let Ok(meta) = std::fs::symlink_metadata(entry.path()) {
            if meta.file_type().is_symlink() || meta.uid() == unsafe { libc::geteuid() } {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
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
/// **Also removes the persisted refused-storage marker** (see [`write_refused_marker`]) — a
/// different account, or a future firmware, gets a fresh chance at keymanager3 rather than
/// inheriting a previous account's verdict about this install's key.
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
        if let Some(bytes) = read_owned_regular(&path) {
            if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
                if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                    crate::keymanager::remove(&envelope.sealed.backend, &envelope.sealed.key);
                }
            }
        }
        remove_temp_siblings(&path);
        let _ = std::fs::remove_file(path);
    }
    // The marker carries no credential, but leaving it behind would keep a FUTURE sign-in on this
    // same install pinned to plaintext for no reason connected to the account that just left.
    for path in refused_marker_paths() {
        remove_temp_siblings(&path);
        let _ = std::fs::remove_file(path);
    }
    // No file, so nothing left to call Locked — and no cached copy of the session that just got
    // signed out should keep answering `peek`.
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(CLASS_UNKNOWN, std::sync::atomic::Ordering::Relaxed);
    clear_cache();
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
/// Converted: `ui/account_menu.rs`, and — since 2026-08-23 — the shared top bar's profile chip
/// (`ui/widgets.rs` `profile_chip`), which was the remaining half of the bug. Both now word
/// themselves through ONE resolver, `ui::account_menu::chip_label`, so the chip and the menu it
/// opens cannot disagree about the same account again.
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
            let named_admin = self
                .home_users
                .iter()
                .find(|u| u.admin && !u.title.is_empty());
            named_admin
                .or_else(|| self.home_users.iter().find(|u| !u.title.is_empty()))
                .map(|u| u.title.clone())
        };
        let name = active
            .and_then(|u| named(&u.title))
            .or_else(|| named(&self.user.title))
            .or_else(roster);
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

    thread_local! {
        /// What [`super::report_storage_error`]'s test double captured instead of a real send —
        /// same seam `keymanager.rs`'s `log`/`capture` uses, since a dev checkout has no Sentry
        /// endpoint compiled in and `telemetry::storage::report_error` would always refuse.
        pub(super) static CAPTURED_REPORTS: std::cell::RefCell<Vec<crate::telemetry::storage::StorageErrorContext>> =
            std::cell::RefCell::new(Vec::new());
    }

    pub(super) fn capture_report(ctx: crate::telemetry::storage::StorageErrorContext) {
        CAPTURED_REPORTS.with(|c| c.borrow_mut().push(ctx));
    }

    fn captured_reports() -> Vec<crate::telemetry::storage::StorageErrorContext> {
        CAPTURED_REPORTS.with(|c| c.borrow().clone())
    }

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
            "home_pins":[{"user":"u-7","asked":true,
                          "on":[{"machine_id":"bbbb2222","key":1}],
                          "off":[{"machine_id":"aaaa1111","key":1}]}]}"#
    }

    /// **THE COMPATIBILITY GATE: a session file written by 0.4.1 must still boot.**
    ///
    /// That build knew nothing about origins — it wrote `address` and `port` and no more — and
    /// every signed-in television in the world is holding one of these files right now. If
    /// `Session::server` failed to carry through, the cost is not a degraded feature: `app.rs`'s
    /// boot gate runs on `can_go_local()`, so the app would land on the QR sign-in screen on
    /// **every boot for every existing user**, which is a silent sign-out that no test above this
    /// one can see (the roster lists are soft-parsed — `de_soft_vec` — but the primary is not a
    /// disposable entry, and nothing soft-parses a MISSING field into a different meaning).
    ///
    /// Written as literal 0.4.1-shaped JSON rather than by serialising a `Session`, because the
    /// thing under test is precisely that today's struct is not what wrote those bytes.
    #[test]
    fn a_session_file_written_before_origins_existed_still_boots_as_plain_http() {
        // Byte-for-byte the shape 0.4.1 wrote: no `origin` on the primary, none on any source.
        let v041 = r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                      "port":32400,"token":"tok-own"},
            "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
            "sources":[
              {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
               "address":"192.168.0.10","port":32400,"token":"tok-own"},
              {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
               "address":"203.0.113.9","port":31234,"token":"tok-share"}]}"#;
        let s: Session = serde_json::from_str(v041).expect("a 0.4.1 session file still parses");

        // the boot gate itself — this is the assertion whose failure is the silent sign-out
        assert!(
            s.can_go_local(),
            "a 0.4.1 session must still reach Home without a QR code"
        );

        // …and it boots against exactly the address it always did, as plain http
        let o = s.server.origin();
        assert_eq!(o.base(), "http://192.168.0.10:32400");
        assert_eq!((o.host(), o.port()), ("192.168.0.10", 32400));
        assert!(!o.is_tls(), "nothing in that file ever meant TLS");

        // every roster entry too, including the share on its non-default port
        assert!(
            s.sources.iter().all(|x| x.usable()),
            "{:#?}",
            s.sources.len()
        );
        assert_eq!(
            s.owned_source().unwrap().origin().unwrap().base(),
            "http://192.168.0.10:32400"
        );
        assert_eq!(
            s.source("bbbb2222").unwrap().origin().unwrap().base(),
            "http://203.0.113.9:31234"
        );
    }

    /// Tier persistence is additive: old files have no field, and a value written by a future
    /// build must not make the PRIMARY fail to parse (which would route a signed-in TV to QR).
    #[test]
    fn a_stored_tier_round_trips_and_unknown_tiers_degrade_to_unknown() {
        let legacy: Session =
            serde_json::from_str(two_server_json()).expect("the legacy shape parses");
        assert_eq!(legacy.server.tier, None);
        assert!(legacy.sources.iter().all(|s| s.tier.is_none()));

        let json = r#"{"client_id":"c","server":{"address":"192.0.2.10","port":32400,
                      "token":"t","tier":"future-tier"},
                    "sources":[{"machine_id":"m","address":"192.0.2.10","port":32400,
                      "token":"t","tier":"relay"}]}"#;
        let s: Session =
            serde_json::from_str(json).expect("an unknown primary tier is soft metadata");
        assert!(
            s.can_go_local(),
            "unknown tier metadata cannot silently sign the device out"
        );
        assert_eq!(s.server.tier, None);
        assert_eq!(
            s.sources[0].tier,
            Some(super::super::probe::Location::Relay)
        );

        let encoded = serde_json::to_value(ServerRef {
            tier: Some(super::super::probe::Location::Remote),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            encoded["tier"], "remote",
            "the file stays human-readable and stable"
        );
    }

    /// A missing quality field is an OLD install, not an invitation to adopt a new default. The
    /// literal is deliberately pre-feature JSON; serialising today's `Session` would always write
    /// whatever today's struct thinks and could not grade the migration boundary.
    #[test]
    fn a_legacy_session_with_no_quality_stays_original() {
        let s: Session = serde_json::from_str(two_server_json()).expect("the legacy file parses");
        assert_eq!(
            s.playback_quality, None,
            "absence remains distinguishable on disk"
        );
        assert_eq!(
            s.playback_quality(),
            PlaybackQuality::Original,
            "legacy playback does not become Auto"
        );
    }

    /// Quality is a preference beside credentials, never a reason to discard them. This is the
    /// scalar counterpart of the roster/tier soft parsers: unknown future names, null and the
    /// wrong JSON shape all keep the session and conservatively mean Original.
    #[test]
    fn invalid_or_future_quality_is_soft_and_conservative() {
        for value in [r#""future_auto_v2""#, "null", r#"{"mode":"auto"}"#, "42"] {
            let json = format!(
                r#"{{"client_id":"c","account_token":"acct",
                     "server":{{"address":"192.168.0.10","port":32400,"token":"t"}},
                     "playback_quality":{value}}}"#
            );
            let s: Session = serde_json::from_str(&json)
                .expect("bad preference metadata cannot fail credentials");
            assert_eq!(s.account_token, "acct");
            assert!(s.can_go_local());
            assert_eq!(s.playback_quality(), PlaybackQuality::Original, "{value}");
        }
    }

    #[test]
    fn every_explicit_quality_mode_round_trips_by_stable_name() {
        let cases = [
            (PlaybackQuality::Auto, "auto"),
            (PlaybackQuality::Original, "original"),
            (PlaybackQuality::P1080High, "1080p_20_mbps"),
            (PlaybackQuality::P1080, "1080p_8_mbps"),
            (PlaybackQuality::P720, "720p_4_mbps"),
            (PlaybackQuality::P720Low, "720p_2_mbps"),
            (PlaybackQuality::P480, "480p_720_kbps"),
        ];
        for (quality, wire) in cases {
            let s = Session {
                playback_quality: Some(quality),
                ..Session::default()
            };
            let json = serde_json::to_value(&s).unwrap();
            assert_eq!(json["playback_quality"], wire);
            let again: Session = serde_json::from_value(json).unwrap();
            assert_eq!(again.playback_quality(), quality);
        }
    }

    #[test]
    fn a_fresh_install_defaults_to_auto_only_after_readiness() {
        assert_eq!(
            PlaybackQuality::fresh_default(false),
            PlaybackQuality::Original
        );
        assert_eq!(PlaybackQuality::fresh_default(true), PlaybackQuality::Auto);

        let mut absent = Session::default();
        seed_fresh_quality(&mut absent, false, true);
        assert_eq!(
            absent.playback_quality,
            Some(PlaybackQuality::Auto),
            "only the no-file path may adopt a newly ready Auto default"
        );

        // Literal legacy JSON with neither field. Its empty client id will be repaired by `load`,
        // but that is not evidence of a fresh install and must not seed Auto even after readiness.
        let mut legacy: Session =
            serde_json::from_str(r#"{"account_token":"still-a-real-file"}"#).unwrap();
        seed_fresh_quality(&mut legacy, true, true);
        assert!(legacy.client_id.is_empty());
        assert_eq!(legacy.playback_quality, None);
        assert_eq!(legacy.playback_quality(), PlaybackQuality::Original);
    }

    /// The other side of the gate: once an origin IS written down it is what gets dialled, and it
    /// beats the address pair beside it. That is not a tie-break for its own sake — for an https
    /// server the two genuinely differ (the certificate is issued for the `plex.direct` NAME, not
    /// for the quad), so reading the pair would connect and then fail validation.
    #[test]
    fn a_stored_origin_beats_the_address_pair_beside_it() {
        let json = r#"{"client_id":"c","account_token":"a",
            "server":{"machine_id":"aaaa1111","address":"203.0.113.9","port":31234,"token":"t",
                      "origin":"https://203-0-113-9.hash.plex.direct:31234"},
            "sources":[{"machine_id":"aaaa1111","owned":true,"address":"203.0.113.9","port":31234,
                        "token":"t","origin":"https://203-0-113-9.hash.plex.direct:31234"}]}"#;
        let s: Session = serde_json::from_str(json).expect("parses");

        let o = s.server.origin();
        assert_eq!(
            o.host(),
            "203-0-113-9.hash.plex.direct",
            "the name TLS validates against"
        );
        assert!(o.is_tls());
        assert_eq!(
            s.server.address, "203.0.113.9",
            "…and the quad survives as the diagnostic half"
        );
        assert!(
            s.can_go_local(),
            "an https primary is still a session this device holds"
        );
        assert_eq!(
            s.sources[0].origin().unwrap(),
            o,
            "the roster entry says the same thing"
        );

        // and it round-trips: what we write back is what we would read next boot
        let again: Session =
            serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
        assert_eq!(again.server.origin(), o);
    }

    /// A stored origin that cannot be dialled is refused rather than silently repaired. The port
    /// is the case that really arrives — the session file is JSON on disk that a hand edit or an
    /// older build can leave holding anything an `i64` can hold, and `4_294_999_696 as i32` is
    /// **32400**, so "repair it to the default" means dialling a port nobody wrote down.
    #[test]
    fn an_undialable_stored_origin_is_refused_not_repaired() {
        let bad = |origin: &str| {
            let json = format!(
                r#"{{"client_id":"c","account_token":"a",
                     "server":{{"address":"192.168.0.10","port":32400,"token":"t","origin":"{origin}"}},
                     "sources":[{{"machine_id":"m","address":"192.168.0.10","port":32400,"token":"t",
                                  "origin":"{origin}"}}]}}"#
            );
            serde_json::from_str::<Session>(&json).expect("the file still parses")
        };
        for origin in [
            "http://192.168.0.10:4294999696",
            "ftp://192.168.0.10:21",
            "http://",
        ] {
            let s = bad(origin);
            assert!(!s.can_go_local(), "{origin} is not something to boot on");
            assert!(
                !s.sources[0].usable(),
                "{origin} is not something to register"
            );
        }
    }

    /// The roster survives a write/read cycle intact — including the two facts that make a share
    /// usable at all: its OWN address (never the owner's LAN one) and its OWN token.
    #[test]
    fn the_roster_round_trips_through_the_session_file_format() {
        let s: Session = serde_json::from_str(two_server_json()).expect("a normal session parses");
        let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");

        assert_eq!(s.sources.len(), 2);
        let own = s.owned_source().expect("our own server is in the roster");
        assert_eq!(
            (own.machine_id.as_str(), own.address.as_str()),
            ("aaaa1111", "192.168.0.10")
        );
        assert!(
            own.shared_by.is_empty(),
            "an owned server has no owner to name"
        );

        let share = s
            .source("bbbb2222")
            .expect("keyed by machineIdentifier, not by index");
        assert_eq!((share.address.as_str(), share.port), ("203.0.113.9", 31234));
        assert_eq!(
            share.token, "tok-share",
            "the sharing grant, not the account token"
        );
        assert_eq!(share.shared_by, "friend");
        assert!(!share.owned && share.usable());
        assert_eq!(s.shared_sources().count(), 1);

        let mine = s
            .pins_for("u-7")
            .expect("the Home selection is keyed by PROFILE");
        assert!(mine.asked);
        assert_eq!(mine.answer("bbbb2222", 1), Some(true));
        // section keys are server-local: both servers have a section 1, so the key alone matches
        // nothing on its own
        assert_eq!(
            mine.answer("aaaa1111", 1),
            Some(false),
            "an answer names a server AND a key"
        );
        assert_eq!(
            mine.answer("bbbb2222", 9),
            None,
            "a library nobody was asked about"
        );
        assert!(
            s.pins_for("u-9").is_none(),
            "another profile has an answer of its own, or none"
        );
        assert!(s.source("").is_none() && s.source("nope").is_none());

        // and the token is not printable by accident — `describe` is the only formatter there is
        assert!(
            !share.describe().contains("tok-share"),
            "{}",
            share.describe()
        );
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
            "home_pins":"not a list"}"#;
        let s: Session = serde_json::from_str(mixed).expect("a bad entry must not fail the file");
        assert_eq!(s.account_token, "acct", "the credentials are still here");
        assert!(s.can_go_local(), "and the device can still stream");
        assert_eq!(
            s.sources.len(),
            1,
            "the malformed entry dropped, the good one landed"
        );
        assert_eq!(s.sources[0].machine_id, "bbbb2222");
        assert!(
            s.home_pins.is_empty(),
            "a string where a list belongs is no list, not an error"
        );

        // the whole field as an explicit null, and the whole field missing (every session file
        // written before this landed) — both are simply a session with no roster yet
        for json in [
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"},"sources":null}"#,
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"}}"#,
        ] {
            let s: Session = serde_json::from_str(json).expect("null and absent both parse");
            assert!(s.sources.is_empty() && s.home_pins.is_empty());
            assert!(
                s.can_go_local(),
                "the primary server is what boot runs on, roster or not"
            );
        }
    }

    /// **A port is `i64` on disk and `i32` at the socket, and the narrowing used to be a bare
    /// cast.** `4_294_999_696 as i32` is **32400** — the most ordinary port there is — so a session
    /// file holding a number no port can be would have had the app quietly dial a server nobody
    /// wrote down. `#[serde(default)]` cannot catch it either: the field parses fine, it is the
    /// value that is impossible.
    ///
    /// Both gates the value reaches are stated here, because they fail differently and one does not
    /// imply the other: a bad ROSTER entry costs that entry (`usable`, which
    /// `auth::install_roster` filters on before registering), while a bad PRIMARY costs the resume
    /// (`can_go_local`, the one gate in front of `plex::install`) and lands the app on sign-in.
    #[test]
    fn a_port_no_socket_could_take_is_refused_rather_than_wrapped() {
        let s: Session = serde_json::from_str(
            r#"{"client_id":"cid-1","account_token":"acct",
                "server":{"machine_id":"aaaa1111","address":"192.168.0.10","port":32400,"token":"t"},
                "sources":[{"machine_id":"aaaa1111","owned":true,"address":"192.168.0.10",
                            "port":4294999696,"token":"tok-own"},
                           {"machine_id":"bbbb2222","owned":false,"address":"203.0.113.9",
                            "port":31234,"token":"tok-share"}]}"#,
        )
        .unwrap();
        assert!(
            !s.sources[0].usable(),
            "32400 is what that number wraps to — it must not be dialled"
        );
        assert!(
            s.sources[1].usable(),
            "…and the entry beside it is untouched"
        );
        assert!(
            s.can_go_local(),
            "the PRIMARY is fine, so boot still resumes"
        );

        // …and the same number on the primary costs the resume instead, rather than dialling 32400
        let bad: Session = serde_json::from_str(
            r#"{"client_id":"c","server":{"address":"192.168.0.10","port":4294999696,"token":"t"}}"#,
        )
        .unwrap();
        assert!(
            !bad.can_go_local(),
            "an undialable primary sends the user to sign-in, honestly"
        );
        // an absent port is the same answer for the same reason: it could never have connected
        let none: Session = serde_json::from_str(
            r#"{"client_id":"c","server":{"address":"192.168.0.10","token":"t"}}"#,
        )
        .unwrap();
        assert!(!none.can_go_local());
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
        assert_eq!(
            s.pms_token(),
            "tok-own",
            "no managed user picked yet → the server token"
        );
        s.user.token = "tok-user".into();
        assert_eq!(
            s.pms_token(),
            "tok-user",
            "a switched profile's token wins, as before"
        );
        // the roster agrees with the primary rather than competing with it
        assert_eq!(
            s.owned_source().map(|x| x.address.as_str()),
            Some(s.server.address.as_str())
        );
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
        assert_eq!(
            s.recents_for("uu-1"),
            ["wallace", "Гладиатор", "the curse"],
            "most recent first, in order"
        );

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
        let mut s = Session {
            client_id: "cid".into(),
            ..Default::default()
        };
        s.set_recents_for("uu-a", vec!["gromit".into()]);
        s.set_recents_for("uu-b", vec!["эдем".into()]);

        assert_eq!(s.recents_for("uu-a"), ["gromit"]);
        assert_eq!(s.recents_for("uu-b"), ["эдем"]);
        assert!(
            s.recents_for("uu-never-searched").is_empty(),
            "an unknown profile reads empty, not someone else's"
        );
        // the owner with no Plex Home selection keys on "" and is nobody else
        assert!(s.recents_for("").is_empty());

        // …and a write for one leaves the others intact — the bug `set_recents_for` exists to make
        // unwriteable, since the obvious `Session { recent_searches: mine, ..s }` deletes everybody.
        s.set_recents_for("uu-a", vec!["wallace".into(), "gromit".into()]);
        assert_eq!(s.recents_for("uu-a"), ["wallace", "gromit"]);
        assert_eq!(
            s.recents_for("uu-b"),
            ["эдем"],
            "the other profile's history survived the write"
        );
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
        assert_eq!(
            s.recents_for("u"),
            ["wallace", "gromit"],
            "the three bad entries dropped"
        );
        assert_eq!(s.account_token, "acct");
        assert!(s.can_go_local(), "and the device can still stream");

        // the whole field the wrong type is no list, not an error
        let s: Session = serde_json::from_str(r#"{"client_id":"c","recent_searches":"wallace"}"#)
            .expect("a string where a list belongs parses");
        assert!(s.recent_searches.is_empty());
    }

    /// **Whose token is `account_token`, and is that who is watching?** It is the account OWNER's,
    /// written once by the QR sign-in and never replaced by a profile switch — so a roster refresh
    /// made with it answers about the owner, and installing those per-server tokens while a managed
    /// profile is signed in swaps identities under them. For a RESTRICTED profile it also re-adds
    /// the shares `auth::retoken` had correctly made tokenless, which is a re-grant and not a refresh.
    #[test]
    fn only_the_account_owners_own_profile_may_refresh_the_roster_with_the_account_token() {
        // Holds keymanager global state (`LAST_REFUSAL`) exposed through `clear()` -> `keymanager::remove()`
        // below; without this a concurrent `keymanager.rs` test asserting on that value can race it.
        let _g = crate::testlock::serial();
        let home = |uuid: &str| Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            user: UserRef {
                uuid: uuid.into(),
                ..Default::default()
            },
            home_users: vec![
                HomeUserRef {
                    uuid: "u-owner".into(),
                    title: "Gleb".into(),
                    admin: true,
                    ..Default::default()
                },
                HomeUserRef {
                    uuid: "u-kid".into(),
                    title: "Kid".into(),
                    admin: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(
            home("u-owner").active_profile_is_admin(),
            "the owner's own tile"
        );
        assert!(
            !home("u-kid").active_profile_is_admin(),
            "a managed profile is not the account"
        );

        // An account with no Plex Home never writes a profile at all — auth's single-user path
        // enters Home on the owner's server token — so an empty uuid IS the owner.
        let solo = Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        assert!(solo.active_profile_is_admin());

        // …but an unknown uuid is NOT the owner. `home_users` is empty for "never fetched" as much
        // as for "no Plex Home" (see `Session::account`), and on a question whose wrong answer is
        // somebody else's credentials, "cannot prove it" must not read as "yes".
        let mut unknown = home("u-kid");
        unknown.home_users.clear();
        assert!(!unknown.active_profile_is_admin());
        assert!(!home("u-nobody").active_profile_is_admin());
    }

    /// **Who lives in this house** — the ids the "Shared by …" rule asks
    /// `plex::servers::is_household` with, which is the Plex Home ROSTER and nothing else.
    ///
    /// The rule falls back to plex.tv's undocumented `home` flag exactly when this list is empty,
    /// so emptiness has to mean one thing — *the roster could not answer* — and every case below
    /// is about keeping it meaning that.
    #[test]
    fn the_household_is_the_home_roster_and_emptiness_means_it_could_not_answer() {
        let s = Session {
            user: UserRef {
                id: 333_333,
                uuid: "u-kid".into(),
                ..Default::default()
            },
            home_users: vec![
                HomeUserRef {
                    id: 111_111,
                    uuid: "u-owner".into(),
                    admin: true,
                    ..Default::default()
                },
                HomeUserRef {
                    id: 222_222,
                    uuid: "u-guest".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            s.household_ids(),
            vec![111_111, 222_222],
            "the roster, and NOT `user.id` — see the case below and the function's own doc"
        );

        // **`0` is filtered, and that is the compatibility case rather than a tidy-up.** A roster
        // read off a file written before `HomeUserRef::id` existed is all zeroes, and our own
        // server's `ownerId` is `0` too — letting those two meet would suppress a credit by
        // accident, on evidence that is only the absence of evidence.
        let legacy = Session {
            home_users: vec![HomeUserRef {
                uuid: "u-owner".into(),
                admin: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(
            legacy.household_ids().is_empty(),
            "an un-enumerable house is empty, not a house containing nobody-id-zero"
        );

        // **The upgraded managed session, and the reason `user.id` is not in this list.** Every
        // roster id is still the legacy `0`, and the `/switch` that chose this profile wrote a real
        // `user.id` long ago. Including it made the answer NON-empty — which
        // `plex::servers::is_household` reads as "the house can speak for itself" and uses to
        // silence the `home` fallback — while the one id that could have decided the case, the
        // ADMIN's, was among the zeroes that get filtered. The result was the reported bug
        // surviving on exactly the sessions the fallback was added for.
        let upgraded = Session {
            user: UserRef {
                id: 333_333,
                uuid: "u-kid".into(),
                ..Default::default()
            },
            home_users: vec![
                HomeUserRef {
                    uuid: "u-owner".into(),
                    admin: true,
                    ..Default::default()
                },
                HomeUserRef {
                    uuid: "u-kid".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(
            upgraded.household_ids().is_empty(),
            "a roster of zeroes cannot enumerate the house, whoever is watching"
        );
    }

    /// **The stored profile's PIN flag — what the boot picker's BACK is gated on.** The escalation
    /// it exists to close: the adult profile carries the PIN, the app is signed in as them, a child
    /// boots it, and BACK out of the who's-watching picker reinstated that session with no code
    /// entered at all (`auth::cancel`).
    ///
    /// The two "the roster cannot say" answers deliberately disagree with the test above's. An
    /// unknown uuid is NOT the owner, because that question's wrong answer is somebody else's
    /// credentials; the same uuid IS treated as protected, because this question's wrong answer is
    /// a bypassed PIN and being wrong the other way costs one profile pick.
    #[test]
    fn a_stored_profile_behind_a_pin_is_reported_as_protected() {
        // Same reason as the sibling test above: `clear()` reaches `keymanager::remove()`, which
        // touches process-global keymanager state a `keymanager.rs` test can be asserting on.
        let _g = crate::testlock::serial();
        let home = |uuid: &str| Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            user: UserRef {
                uuid: uuid.into(),
                ..Default::default()
            },
            home_users: vec![
                HomeUserRef {
                    uuid: "u-owner".into(),
                    title: "Gleb".into(),
                    admin: true,
                    protected: true,
                    ..Default::default()
                },
                HomeUserRef {
                    uuid: "u-kid".into(),
                    title: "Kid".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(
            home("u-owner").active_profile_is_protected(),
            "the adult tile carries the PIN"
        );
        assert!(
            !home("u-kid").active_profile_is_protected(),
            "a managed profile with no PIN"
        );

        // **A session that names NO profile answers protected too**, which is the half that reads
        // as harmless and is not: it is what a sign-in abandoned at the picker leaves on disk (the
        // account token, the server and the roster are persisted the moment they exist; the pick
        // never happened), and `pms_token()` on it is the OWNER's server token. The very next boot
        // raises a picker over that file — a roster of >1 is exactly what it has — so answering
        // "not protected" here put the owner's credentials behind BACK by a second road.
        let mut abandoned = home("u-owner");
        abandoned.user = UserRef::default();
        assert!(
            abandoned.active_profile_is_protected(),
            "no profile chosen is not 'no PIN to be behind'"
        );
        let solo = Session {
            client_id: "cid".into(),
            account_token: "acct".into(),
            ..Default::default()
        };
        assert!(solo.active_profile_is_protected());

        // …and a uuid the roster does not name is treated as protected.
        let mut unknown = home("u-owner");
        unknown.home_users.clear();
        assert!(unknown.active_profile_is_protected());
        assert!(home("u-nobody").active_profile_is_protected());
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
            let dir = std::env::temp_dir()
                .join(format!("plxnative-session-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir); // a previous run that died mid-test
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            super::redirect_for_test(Some(dir.join("auth.json")));
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
            super::redirect_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Two priority-ordered candidates of this test's own — for the issue #76 review coverage
    /// that `TempSession`'s single path structurally cannot exercise: a locked envelope that is
    /// NOT at `auth_paths()[0]`.
    struct TwoCandidateSession {
        dir: std::path::PathBuf,
    }

    impl TwoCandidateSession {
        fn new(tag: &str) -> TwoCandidateSession {
            let dir = std::env::temp_dir()
                .join(format!("plxnative-session-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            super::redirect_for_test_multi(vec![dir.join("a.json"), dir.join("b.json")]);
            TwoCandidateSession { dir }
        }
        fn higher(&self) -> std::path::PathBuf {
            self.dir.join("a.json")
        }
        fn lower(&self) -> std::path::PathBuf {
            self.dir.join("b.json")
        }
    }

    impl Drop for TwoCandidateSession {
        fn drop(&mut self) {
            super::redirect_for_test(None);
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn signed_in() -> Session {
        Session {
            client_id: "cid-1".into(),
            account_token: "acct".into(),
            ..Default::default()
        }
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
        assert!(
            !t.tmp().exists(),
            "the tmp file is renamed, not left beside the session"
        );
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials at rest");

        // a sign-out takes the tmp with it: `peek` cannot read one, but a live account token left
        // in a file on a rooted television is not a sign-out
        std::fs::write(t.tmp(), b"{}").unwrap();
        clear();
        assert!(!t.file().exists() && !t.tmp().exists());
    }

    /// **The route ground's one persisted seed.** A fresh device has recorded nothing, a real
    /// hero is remembered across the read-modify-write cycle `update` uses everywhere else, and
    /// recording the SAME envelope again is a no-op rather than a second disk write.
    #[test]
    fn last_hero_blur_round_trips_and_skips_a_redundant_write() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("last-hero");
        save(&signed_in());
        assert_eq!(last_hero(), None, "a fresh device has shown no hero yet");

        let envelope = [[0.1, 0.2, 0.3]; 4];
        assert!(record_last_hero(envelope), "a new envelope is a real write");
        assert_eq!(last_hero(), Some(envelope));

        assert!(
            !record_last_hero(envelope),
            "recording the same envelope again must not touch the file"
        );

        let second = [[0.9, 0.8, 0.7]; 4];
        assert!(record_last_hero(second), "a genuinely different hero writes");
        assert_eq!(last_hero(), Some(second), "…and replaces the stored one");
    }

    /// **Issue #76.** A pre-existing secure envelope this process cannot open (`load` never even
    /// gets a real client id out of it — `Locked` degrades to a fresh, ephemeral default, exactly
    /// the "takes longer than usual" + "sign in again" symptom the owner reported) must not shadow
    /// a FRESH sign-in forever. Once a caller saves a session that carries its own `account_token`,
    /// the locked ciphertext is replaced by the 0600 plaintext file — there is nothing in the old
    /// envelope the new sign-in does not already re-supply, and refusing the write is exactly what
    /// produced the endless loop: seal → unreadable envelope → every `peek` defaults → sign in
    /// again → seal into the same unreadable shape.
    #[test]
    fn a_locked_secure_session_is_replaced_by_plaintext_on_a_fresh_sign_in() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("secure-locked-recovery");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        let original = serde_json::to_vec_pretty(&envelope).unwrap();
        std::fs::write(t.file(), &original).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the run still gets an ephemeral id"
        );
        assert!(
            loaded.account_token.is_empty(),
            "the locked envelope's real session never came back — Locked degrades to default"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "a locked file is never rewritten just for a fresh client id"
        );

        // The user signs in again, exactly as the reported loop describes.
        save(&signed_in());
        let on_disk = std::fs::read(t.file()).unwrap();
        assert_ne!(
            on_disk, original,
            "a fresh sign-in must not be discarded to protect an envelope nobody can open"
        );
        let saved: Session =
            serde_json::from_slice(&on_disk).expect("the recovery file is plaintext, not sealed");
        assert_eq!(saved.account_token, "acct");
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the recovery write is credentials at rest too");

        // A later boot in a NEW process (no cache) reads the recovered session back.
        clear_cache();
        LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            load().account_token,
            "acct",
            "the sign-in survives a reboot, which is the whole point"
        );
    }

    /// **Issue #76, hypothesis 2** (the review's blocker): a keymanager3 that can encrypt AND
    /// decrypt fine within THIS launch/registration — so `keymanager::seal`'s own in-process
    /// round trip would pass — but whose key is not the one that sealed the envelope already on
    /// disk (a different launch, a different registration, a rotated/lost key — the shape LG's
    /// "a key can be used only by the owner of the key" most naturally describes). Before this
    /// fix, `save_locked` called `seal` unconditionally and trusted whatever it returned, so a
    /// fresh sign-in here would be RE-SEALED into another envelope in exactly the same unreadable
    /// shape — the endless loop, unbroken, with only a misleading "replaced by the 0600 file" log
    /// line to show for it. The fix is to consult this run's OWN read verdict (`LOCKED_STATE`)
    /// before ever calling `seal` again.
    #[test]
    fn a_backend_that_would_round_trip_right_now_is_never_asked_after_this_run_already_found_the_file_locked(
    ) {
        let _g = crate::testlock::serial();
        let t = TempSession::new("hypothesis-2");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        std::fs::write(t.file(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();

        // The boot read fails to open the on-disk envelope — a script-free `open` always refuses,
        // matching the dev set's "no keymanager3 at all" as well as "this launch cannot open a key
        // sealed elsewhere". `LOCKED_STATE` is now `LOCKED_RECOVERABLE`.
        let loaded = load();
        assert!(loaded.account_token.is_empty());

        // NOW arm a keymanager double that would round-trip PERFECTLY if asked — modelling the
        // part of hypothesis 2 that fooled the old code: this launch's own key manager genuinely
        // works. If `save_locked` called `seal` again here, it would succeed and hand back a new
        // sealed envelope.
        crate::keymanager::arm_for_test(vec![
            ("generateKey", Ok(serde_json::json!({"returnValue": true}))),
            (
                "begin",
                Ok(serde_json::json!({
                    "returnValue": true, "handle": "h-enc", "iv": "MDEyMzQ1Njc4OWFi"
                })),
            ),
            (
                "finish",
                Ok(serde_json::json!({"returnValue": true, "output": "Y2lwaGVydGV4dA=="})),
            ),
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    "output": "aXNzdWUtNzYgcGxhaW50ZXh0" // an arbitrary plaintext seal() would accept
                })),
            ),
        ]);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        let on_disk = std::fs::read(t.file()).unwrap();
        let saved: Session = serde_json::from_slice(&on_disk).expect(
            "a working-right-now backend must still be bypassed — the file must be the plaintext \
             recovery write, never a freshly sealed envelope this launch alone could open",
        );
        assert_eq!(saved.account_token, "acct");
    }

    /// **Recovery targets the SAME candidate the locked envelope was found at**, not merely the
    /// first candidate willing to accept a write. `TempSession` is one path; this needs two, with
    /// the envelope at the LOWER-priority one and the higher-priority one free — the shape that
    /// made the old "first writable wins" loop write a fresh plaintext file the next boot's
    /// `read_locked` would never even reach, because the untouched locked envelope at the
    /// higher-priority candidate kept shadowing it.
    #[test]
    fn recovery_targets_the_candidate_the_locked_envelope_was_actually_found_at() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("recovery-targeting");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        // Only the LOWER-priority candidate holds the envelope; the higher-priority one is
        // absent, so an unqualified "first writable candidate" would happily create it there.
        std::fs::write(t.lower(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        assert!(!t.higher().exists());

        let loaded = load();
        assert!(loaded.account_token.is_empty(), "Locked degrades to default");

        save(&signed_in());

        assert!(
            !t.higher().exists(),
            "the recovery write must not land at the higher-priority candidate merely because \
             it was free — the next boot's read_locked would never reach the untouched locked \
             file at the lower-priority path if it did"
        );
        let saved: Session = serde_json::from_slice(&std::fs::read(t.lower()).unwrap())
            .expect("the recovery write lands at the SAME candidate the envelope was found at");
        assert_eq!(saved.account_token, "acct");
    }

    /// **Recovery sweeps every OTHER candidate**, exactly like a successful seal already does —
    /// a stale copy left behind at a lower-priority jail path is a plaintext credential another
    /// uid can read, whether it got there from an old fallback write or from anything else.
    #[test]
    fn recovery_sweeps_a_stale_copy_at_another_candidate() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("recovery-sweep");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        std::fs::write(t.higher(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        std::fs::write(t.lower(), b"leftover plaintext credentials").unwrap();

        let loaded = load();
        assert!(loaded.account_token.is_empty(), "Locked degrades to default");

        save(&signed_in());

        assert!(
            !t.lower().exists(),
            "a stale copy at another candidate must not survive the recovery write"
        );
        let saved: Session = serde_json::from_slice(&std::fs::read(t.higher()).unwrap()).unwrap();
        assert_eq!(saved.account_token, "acct");
    }

    /// **Issue #76 review:** an unrelated writer (home pins, recents, the quality rung — none of
    /// which carries fresh credentials) must never be the thing that replaces a recognized
    /// secure-but-unopenable envelope with a credential-free plaintext file. Before this fix,
    /// `load`'s own ephemeral (never-persisted) client id — minted even on a Locked read — made
    /// `update`'s empty-client-id guard pass, so the very next `update` from anywhere destroyed
    /// the locked envelope.
    #[test]
    fn update_after_a_locked_boot_does_not_destroy_the_locked_envelope() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("update-vs-locked");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        let original = serde_json::to_vec_pretty(&envelope).unwrap();
        std::fs::write(t.file(), &original).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the run still gets an ephemeral id — the exact thing that used to fool `update`"
        );

        let wrote = update(|s| {
            Some(Session {
                client_id: s.client_id.clone(),
                ..Default::default()
            })
        });
        assert!(
            !wrote,
            "an unrelated writer with no credentials of its own must not touch a locked file"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "the locked envelope must survive untouched"
        );
    }

    /// A temporary LS2/key-store failure must still refuse the plaintext downgrade when THIS
    /// process never actually found the on-disk file Locked — as opposed to the recovery case
    /// above, where the whole point is that a boot read failed. `LOCKED_STATE` only ever becomes
    /// [`LOCKED_RECOVERABLE`] through [`read_locked`] observing exactly that; reaching into it
    /// directly (this test's own module, via `super::*`) is the cheapest way to pin the OTHER side
    /// of that branch without reconstructing a byte-exact keymanager3 encrypt/decrypt round trip
    /// that has nothing to do with what this test is about.
    #[test]
    fn a_transient_failure_with_no_locked_read_this_run_still_refuses_the_plaintext_downgrade() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("secure-transient");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        let original = serde_json::to_vec_pretty(&envelope).unwrap();
        std::fs::write(t.file(), &original).unwrap();
        // No `load()`/`read_locked()` ran against this file in this process — `LOCKED_STATE` sits
        // at its default, never having been told this file is the recoverable shape.
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            NOT_LOCKED
        );

        // `seal` fails (no keymanager script armed, the default every unscripted test relies on).
        save(&signed_in());
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "an unavailable service cannot leak the replacement session as plaintext"
        );
    }

    #[test]
    fn an_unknown_secure_envelope_version_is_locked_and_never_rewritten_as_plaintext() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("secure-future-version");
        let original = br#"{
  "format": "plxnative-secure-session",
  "version": 2,
  "sealed": {
    "backend": "keymanager3",
    "key": "plxnative.session.v2",
    "iv": "future-iv",
    "data": "future-ciphertext"
  }
}"#;
        std::fs::write(t.file(), original).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the run still gets an ephemeral id"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "rollback must preserve an envelope it does not understand"
        );

        save(&signed_in());
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "a future secure envelope must shadow every plaintext replacement"
        );
    }

    /// A backend that answers `encrypt` but not `decrypt` never gets to persist ciphertext at all
    /// (stage K's own `seal` round-trip check catches it) — from `save_locked`'s side this looks
    /// exactly like "no usable key manager", so the write falls straight through to the 0600
    /// plaintext file. What this test is actually pinning is the CACHE half: `peek` afterwards must
    /// not re-decrypt anything — there is nothing left to decrypt, since the file is plaintext, and
    /// serving it from the in-process copy rather than re-reading disk is what stops the account
    /// chip from paying a multi-second LS2 round trip on every open.
    #[test]
    fn a_backend_that_cannot_open_its_own_envelope_falls_back_to_plaintext_and_peek_serves_the_cache(
    ) {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("cache-fallback");
        crate::keymanager::arm_for_test(vec![
            ("generateKey", Ok(serde_json::json!({"returnValue": true}))),
            (
                "begin",
                Ok(serde_json::json!({
                    "returnValue": true, "handle": "h-enc",
                    "iv": "MDEyMzQ1Njc4OWFi"
                })),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true, "output": "Y2lwaGVydGV4dA=="
                })),
            ),
            (
                "begin",
                Ok(serde_json::json!({
                    "returnValue": false, "errorCode": -10001, "errorText": "key not found"
                })),
            ),
        ]);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        assert_eq!(
            peek().account_token,
            "acct",
            "served from the in-process cache, not a re-decrypt of the file"
        );
        let raw = std::fs::read(t.file()).unwrap();
        let on_disk: Session =
            serde_json::from_slice(&raw).expect("the fallback file is plaintext, not sealed");
        assert_eq!(on_disk.account_token, "acct");
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "credentials at rest even on the fallback path");
    }

    /// `clear()` (sign-out) must drop the in-process cache along with the file — a stale cached
    /// copy answering `peek` after a sign-out would mean the UI keeps showing the account that was
    /// just signed out of.
    #[test]
    fn clear_empties_the_cache_and_a_following_peek_reads_disk() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("clear-cache");
        save(&signed_in());
        assert_eq!(peek().account_token, "acct", "cached from the save above");

        clear();
        assert_eq!(
            peek().account_token,
            "",
            "signed out — nothing cached, nothing on disk"
        );

        // And the cache is genuinely gone, not merely holding a signed-out value: a session
        // written straight to disk (as another process/boot would) is what `peek` now reads.
        save(&signed_in());
        assert_eq!(peek().account_token, "acct");
    }

    #[test]
    fn a_precreated_tmp_symlink_cannot_redirect_session_bytes() {
        use std::os::unix::fs::symlink;
        let _g = crate::testlock::serial();
        let t = TempSession::new("tmp-symlink");
        let victim = t.dir.join("attacker-readable");
        std::fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, t.tmp()).unwrap();

        save(&signed_in());

        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        assert_eq!(peek().account_token, "acct");
    }

    // ---- Issue #76 review: the CROSS-LAUNCH marker ----------------------------------------------
    //
    // `LOCKED_STATE` is a process global: it answers nothing about what a PRIOR launch found. The
    // robustness review's gap is that a per-process-only fix breaks the loop for exactly one boot —
    // a later launch that reads the recovered plaintext cleanly, or whose own key manager happens
    // to round-trip within itself, re-seals into the same unopenable shape. These tests pin the
    // persisted marker that makes the verdict survive past the launch that found it.

    fn locked_envelope_bytes() -> Vec<u8> {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        serde_json::to_vec_pretty(&envelope).unwrap()
    }

    /// Minimal standard base64 (RFC 4648), matching `keymanager::b64::encode` — which is
    /// `pub(super)` and unreachable from here — just enough to script a `finish(decrypt)` reply
    /// that `keymanager::open`'s `b64::decode` will actually turn back into `plain`.
    fn b64_encode_for_test(bytes: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
        for chunk in bytes.chunks(3) {
            let n = (chunk[0] as u32) << 16
                | (*chunk.get(1).unwrap_or(&0) as u32) << 8
                | *chunk.get(2).unwrap_or(&0) as u32;
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - i * 6)) as usize & 63] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// A round-tripping seal script, for a launch whose OWN key manager works fine in-process —
    /// the shape that must still be bypassed once the marker is present. The `finish(decrypt)`
    /// reply carries the base64 of `s`'s OWN serialized bytes so `seal`'s internal round-trip
    /// check (and, in tests that let a save genuinely succeed, `keymanager::open` itself) sees a
    /// real match rather than an arbitrary placeholder.
    fn arm_round_tripping_keymanager(s: &Session) {
        let plain = serde_json::to_vec_pretty(s).unwrap();
        crate::keymanager::arm_for_test(vec![
            ("generateKey", Ok(serde_json::json!({"returnValue": true}))),
            (
                "begin",
                Ok(serde_json::json!({
                    "returnValue": true, "handle": "h-enc", "iv": "MDEyMzQ1Njc4OWFi"
                })),
            ),
            (
                "finish",
                Ok(serde_json::json!({"returnValue": true, "output": "Y2lwaGVydGV4dA=="})),
            ),
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    "output": b64_encode_for_test(&plain)
                })),
            ),
        ]);
    }

    /// A genuine, repeatable keymanager3 REFUSAL on the decrypt half — a real `returnValue:false`
    /// reply with an `errorCode`, as opposed to the unscripted default (`Client::new` refusing
    /// outright, standing in for a registration that never even reached the bus). Every test below
    /// that plants a [`locked_envelope_bytes`] file and wants `read_locked` to persist the
    /// cross-launch marker arms this first — [`write_refused_marker`]'s gate is evidence-based
    /// (`keymanager::last_refusal().is_some()`, review issue #76): only a real service reply counts
    /// as proof the envelope is unopenable, never a bare "nothing answered".
    fn arm_refusing_keymanager() {
        crate::keymanager::arm_for_test(vec![(
            "begin",
            Ok(serde_json::json!({
                "returnValue": false, "errorCode": -10001, "errorText": "key not found"
            })),
        )]);
    }

    /// (a) A read that finds the recognized-but-unopenable envelope writes the marker — before
    /// this fix, nothing on disk recorded that fact at all, so a later launch had no way to know.
    #[test]
    fn a_locked_read_persists_the_refused_marker() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-write-on-locked-read");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();

        assert!(
            !has_refused_marker(),
            "nothing recorded before the first read"
        );
        arm_refusing_keymanager();
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(loaded.account_token.is_empty(), "Locked degrades to default");
        assert!(
            has_refused_marker(),
            "read_locked's LOCKED_RECOVERABLE branch must persist the verdict"
        );
    }

    /// Issue #76, second review: a locked read whose failure never reached a service REPLY — a
    /// refused LS2 registration (the unscripted default here) or a budget timeout — DOES persist
    /// the cross-launch marker, with the stage that failed recorded in it. The first review gated
    /// the marker on a `returnValue:false` reply, and the trace under a STALLED service showed
    /// what that costs: no marker is ever written, so every later launch asks keymanager3 again
    /// and pays the 4 s budget again, on the SDL thread, for the life of the install — and the
    /// handled report never fires either, because nothing recorded a stage. The envelope's own
    /// existence proves this install sealed once; failing to open it, for whatever reason, is the
    /// failure class the marker exists to remember. A healthy set pays for a wrong marker only
    /// at its next fresh sign-in (a 0600 file instead of an envelope, until sign-out), and an
    /// ordinary `update()` never converts a present envelope (see `save_locked`'s guard) — so a
    /// launch that CAN open the envelope still does, marker or not.
    #[test]
    fn an_unreachable_service_at_open_writes_the_marker_and_a_healthy_launch_still_reads() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-on-unreachable");

        // Launch 1: signs in while a keymanager3 that round-trips fine in-process seals the file.
        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            serde_json::from_slice::<SecureEnvelope>(&std::fs::read(t.file()).unwrap()).is_ok(),
            "launch 1 ends with a genuinely sealed envelope on disk"
        );

        // Launch 2: fresh process state, same file — the unscripted default, i.e. the service
        // could not even be registered with.
        super::redirect_for_test(Some(t.file()));
        let loaded2 = load();
        assert!(loaded2.account_token.is_empty(), "this launch still can't open it");
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_RECOVERABLE,
            "the per-process read still protects THIS launch's file"
        );
        assert!(
            has_refused_marker(),
            "an envelope this install wrote could not be opened — that is the marker's whole case"
        );
        assert_eq!(
            marker_stage_for_test().as_deref(),
            Some("unreachable"),
            "the marker records WHICH way the open failed"
        );

        // Launch 3: fresh process state, and this time the key manager genuinely reopens the
        // envelope launch 1 sealed — the marker gates SEALING, never reading.
        super::redirect_for_test(Some(t.file()));
        let plain = serde_json::to_vec_pretty(&signed_in()).unwrap();
        crate::keymanager::arm_for_test(vec![
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    "output": b64_encode_for_test(&plain)
                })),
            ),
        ]);
        let loaded3 = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            loaded3.account_token, "acct",
            "a healthy launch must still be able to reopen its own envelope"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            NOT_LOCKED,
            "the marker does not gate reads"
        );
    }

    /// The stalled-service shape specifically (issue #75/#76's "slow, then try again" symptom): a
    /// `begin(decrypt)` that never answers within its budget is recorded as `no_reply`, and the
    /// marker is written so the next launch does not pay the budget again.
    #[test]
    fn a_service_that_never_answers_at_open_writes_the_marker_as_no_reply() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-on-no-reply");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(loaded.account_token.is_empty(), "Locked degrades to default");
        assert!(has_refused_marker(), "a timeout on an envelope we wrote is evidence enough");
        assert_eq!(marker_stage_for_test().as_deref(), Some("no_reply"));
    }

    /// Reads the `stage` field back out of whichever candidate holds the marker.
    fn marker_stage_for_test() -> Option<String> {
        refused_marker_paths().into_iter().find_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            v.get("stage")?.as_str().map(str::to_string)
        })
    }

    /// (b) With the marker present, `save` never even reaches the key manager — checked with
    /// `calls_for_test`, not merely "no error", because a backend that round-trips PERFECTLY would
    /// otherwise look identical to one correctly bypassed.
    #[test]
    fn a_present_marker_stops_save_before_it_asks_the_key_manager() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-skips-seal");
        // Plant the marker directly, bypassing a real locked read: this test is about `save_locked`
        // consulting it, not about how it got there (that is test (a) above).
        let marker = refused_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            br#"{"refused_at_version":"0.0.0-test","reason":"envelope_unopenable"}"#,
        )
        .unwrap();
        // This launch's OWN read is clean — plaintext, no lock at all — so only the marker can be
        // gating the save that follows.
        std::fs::write(t.file(), serde_json::to_vec_pretty(&signed_in()).unwrap()).unwrap();
        let _ = load();
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            NOT_LOCKED,
            "this launch's own read found nothing wrong"
        );

        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        let calls = crate::keymanager::calls_for_test();
        crate::keymanager::disarm_for_test();
        assert!(
            calls.is_empty(),
            "the marker must stop save_locked before it ever calls the scripted backend, got {calls:?}"
        );

        let on_disk = std::fs::read(t.file()).unwrap();
        let saved: Session = serde_json::from_slice(&on_disk)
            .expect("plaintext, never a freshly sealed envelope from a backend that would round-trip");
        assert_eq!(saved.account_token, "acct");
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the marker-gated write is credentials at rest too");
    }

    /// (c) `clear()` removes the marker along with the session, and a LATER save on the same
    /// install is free to seal again — a different account, or the same one signing back in,
    /// deserves a fresh chance rather than inheriting a stale verdict forever.
    #[test]
    fn clear_removes_the_marker_and_a_later_save_seals_again() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-cleared-on-signout");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        arm_refusing_keymanager();
        let _ = load();
        crate::keymanager::disarm_for_test();
        assert!(has_refused_marker());

        clear();
        assert!(
            !has_refused_marker(),
            "sign-out must take the marker with the session"
        );

        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        let calls = crate::keymanager::calls_for_test();
        crate::keymanager::disarm_for_test();
        assert!(
            !calls.is_empty(),
            "with no marker left, a working key manager must be asked to seal again"
        );
        let on_disk = std::fs::read(t.file()).unwrap();
        assert!(
            serde_json::from_slice::<SecureEnvelope>(&on_disk).is_ok(),
            "a fresh sign-in after clear() gets a real sealed envelope, not the plaintext fallback"
        );
    }

    /// (d) The four-launch sequence the robustness review described end to end: launch 1 seals,
    /// launch 2 finds it locked and recovers to plaintext (as the pre-existing per-process fix
    /// already did), launch 3 reads that plaintext cleanly and — the gap this fix closes — must NOT
    /// re-seal even though ITS OWN key manager would round-trip perfectly, and launch 4 reads the
    /// session back with no third sign-in asked for. `redirect_for_test` is the "new launch" — it
    /// resets `CACHE`/`LOCKED_STATE`/`LOCKED_PATH` while keeping the same on-disk file.
    #[test]
    fn the_four_launch_loop_is_broken_by_the_persisted_marker() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("four-launch");

        // Launch 1: signs in while a keymanager3 that round-trips fine in-process seals the file.
        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            serde_json::from_slice::<SecureEnvelope>(&std::fs::read(t.file()).unwrap()).is_ok(),
            "launch 1 ends with a genuinely sealed envelope on disk"
        );

        // Launch 2: fresh process state, same file, and this time the key manager gives a real
        // (repeatable) refusal on the decrypt — not merely an unreachable registration, since
        // review confirmed the marker must persist only on genuine evidence a service answered
        // (`write_refused_marker`'s gate below).
        super::redirect_for_test(Some(t.file()));
        crate::keymanager::arm_for_test(vec![(
            "begin",
            Ok(serde_json::json!({
                "returnValue": false, "errorCode": -10001, "errorText": "key not found"
            })),
        )]);
        let loaded2 = load();
        crate::keymanager::disarm_for_test();
        assert!(
            loaded2.account_token.is_empty(),
            "launch 2 cannot open what launch 1 sealed"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_RECOVERABLE
        );
        assert!(
            has_refused_marker(),
            "launch 2's locked read must persist the verdict for launch 3"
        );
        save(&signed_in()); // the reported loop: sign in again
        let s2: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap())
            .expect("launch 2 recovers to the plaintext fallback");
        assert_eq!(s2.account_token, "acct");

        // Launch 3: fresh process state again. This launch's OWN read is clean (the file is
        // plaintext now), and a key manager that would round-trip perfectly if asked is armed —
        // exactly the shape that fooled a per-process-only fix into re-sealing.
        super::redirect_for_test(Some(t.file()));
        let loaded3 = load();
        assert_eq!(loaded3.account_token, "acct");
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            NOT_LOCKED,
            "launch 3's own read succeeded — only the marker can still be gating anything"
        );
        arm_round_tripping_keymanager(&signed_in());
        let wrote = update(|cur| {
            Some(Session {
                account_token: cur.account_token.clone(),
                client_id: cur.client_id.clone(),
                ..cur.clone()
            })
        });
        assert!(wrote, "an ordinary roster-refresh-shaped update still writes");
        let calls = crate::keymanager::calls_for_test();
        crate::keymanager::disarm_for_test();
        assert!(
            calls.is_empty(),
            "launch 3 must not re-seal — the marker from launch 2 must still gate it"
        );
        let s3: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap())
            .expect("launch 3's update stays plaintext");
        assert_eq!(s3.account_token, "acct");

        // Launch 4: fresh process state — reads the session back, no third sign-in needed.
        super::redirect_for_test(Some(t.file()));
        let loaded4 = load();
        assert_eq!(
            loaded4.account_token, "acct",
            "the loop is broken: no third sign-in is asked for"
        );
    }

    // ---- issue #76 storage telemetry: `storage_class` and the handled-report wiring ----

    /// (f) An ordinary save with no key manager at all lands as plaintext, and `storage_class`
    /// reports exactly that — no marker, no lock, nothing sitting behind it.
    #[test]
    fn storage_class_reports_plaintext_after_an_ordinary_save() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("class-plaintext");
        reset_report_state_for_test();
        save(&signed_in());
        assert_eq!(storage_class(), crate::telemetry::storage::SessionStorageClass::Plaintext);
    }

    /// (g) A save that genuinely seals reports `Secure`.
    #[test]
    fn storage_class_reports_secure_after_a_sealing_save() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("class-secure");
        reset_report_state_for_test();
        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert_eq!(storage_class(), crate::telemetry::storage::SessionStorageClass::Secure);
    }

    /// (h) A locked-recoverable read always writes the marker before this function can even be
    /// asked, so the live verdict is `SecureRefused` — the more definitive of the two, per
    /// `storage_class`'s own doc — not merely `SecureLocked`.
    #[test]
    fn storage_class_reports_secure_refused_after_a_locked_recoverable_read() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("class-locked-recoverable");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        reset_report_state_for_test();
        arm_refusing_keymanager();
        let _ = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::SecureRefused
        );
    }

    /// (i) An UNRECOVERABLE locked read — a secure-shaped file this build does not recognize —
    /// never writes the marker (see `ReadState::Locked`'s own doc), so `storage_class` reports the
    /// narrower `SecureLocked` instead of `SecureRefused`.
    #[test]
    fn storage_class_reports_secure_locked_for_an_unrecoverable_unrecognized_envelope() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("class-unrecoverable");
        std::fs::write(t.file(), br#"{"format":"plxnative-secure-session"}"#).unwrap();
        reset_report_state_for_test();
        let _ = load();
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNRECOVERABLE
        );
        assert!(
            !has_refused_marker(),
            "an unrecoverable read must not write the cross-launch marker"
        );
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::SecureLocked
        );
    }

    /// (j) A read that lands `LOCKED_RECOVERABLE` reports the handled error exactly once — with
    /// `EnvelopeLocked`, the live class and the marker fact at the time of the report — and a LATER
    /// launch against the same still-locked file must not report the identical stage again.
    #[test]
    fn a_locked_read_reports_once_per_process_with_stage_envelope_locked() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("report-envelope-locked");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        reset_report_state_for_test();

        arm_refusing_keymanager();
        let _ = load();
        crate::keymanager::disarm_for_test();
        let reports = captured_reports();
        assert_eq!(reports.len(), 1, "{reports:?}");
        // The open failed at `begin(decrypt)` with a real code, so the report says THAT rather
        // than the bare `envelope_locked` a failure with no stage of its own would carry.
        assert_eq!(reports[0].stage, crate::telemetry::storage::StorageStage::BeginDecrypt);
        assert_eq!(
            reports[0].class,
            crate::telemetry::storage::SessionStorageClass::SecureRefused
        );
        assert!(reports[0].refused_marker);

        // A later launch against the SAME still-locked file — `redirect_for_test` is the "new
        // launch" the four-launch test above uses (fresh `LOCKED_STATE`/`CACHE`, same file); the
        // once-per-process dedup is deliberately NOT reset by it.
        super::redirect_for_test(Some(t.file()));
        arm_refusing_keymanager();
        let _ = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            captured_reports().len(),
            1,
            "the same stage must not be reported twice in one process"
        );
    }

    /// (k) A save whose `keymanager::seal` fails reports the handled error with THE KEYMANAGER'S
    /// OWN stage and code — `BeginEncrypt` here, not a generic "seal failed".
    #[test]
    fn a_seal_failure_reports_once_with_the_keymanagers_stage_and_code() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("report-seal-failure");
        reset_report_state_for_test();
        crate::keymanager::arm_for_test(vec![
            ("generateKey", Ok(serde_json::json!({"returnValue": true}))),
            (
                "begin",
                Ok(serde_json::json!({
                    "returnValue": false, "errorCode": -10001, "errorText": "key not found"
                })),
            ),
        ]);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        let reports = captured_reports();
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert_eq!(reports[0].stage, crate::telemetry::storage::StorageStage::BeginEncrypt);
        assert_eq!(reports[0].service_error_code, Some(-10001));
    }

    /// (l) An install with no key manager at all — the ordinary case on today's dev set — gets no
    /// answer from any service (`keymanager` records that as `unreachable`/`no_reply`), and the
    /// seal-failure path deliberately does not report those two stages: with no envelope on disk
    /// they are indistinguishable from a firmware that simply has no keymanager3.
    ///
    /// **Disarms first rather than relying on nothing else in the process having touched
    /// `keymanager::LAST_REFUSAL`** (issue #76 review, `keymanager::seal` has no `SELECTED`-keyed
    /// fast path clearing that value on every call — see `keymanager::open`'s doc for why, and why
    /// `seal` deliberately does not). A test that ran earlier in this binary and left a genuine
    /// refusal published (e.g. `last_refusal_publishes_the_stage_and_code_of_a_begin_decrypt_refusal`)
    /// would otherwise be indistinguishable here from this save's own `keymanager::seal` call
    /// finding one, since an install that has already settled on `UNAVAILABLE` takes `seal`'s fast
    /// path without asking a service anything. The premise this test states in its name —
    /// "reaches no service call" — is made true here rather than assumed.
    #[test]
    fn a_healthy_plaintext_install_reports_nothing() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("report-healthy-plaintext");
        crate::keymanager::disarm_for_test();
        reset_report_state_for_test();
        save(&signed_in());
        assert!(captured_reports().is_empty(), "{:?}", captured_reports());
    }

    /// (e) The marker file itself is credentials-adjacent evidence about this device's key manager
    /// and is written through the same 0600 path everything else here uses.
    #[test]
    fn the_refused_marker_is_written_0600() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-mode");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        arm_refusing_keymanager();
        let _ = load();
        crate::keymanager::disarm_for_test();

        let marker = refused_marker_paths()
            .into_iter()
            .find(|p| p.exists())
            .expect("the locked read wrote a marker somewhere");
        let mode = std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the marker is written through the same 0600 atomic path");
    }

    #[test]
    fn a_quality_choice_persists_without_replacing_other_session_state() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("quality");
        let mut s = signed_in();
        s.sources.push(SourceRef {
            machine_id: "server-a".into(),
            token: "server-token".into(),
            address: "192.168.0.10".into(),
            port: 32400,
            ..Default::default()
        });
        save(&s);

        assert!(update(|cur| Some(
            cur.with_playback_quality(PlaybackQuality::P720)
        )));
        let landed = peek();
        assert_eq!(landed.playback_quality(), PlaybackQuality::P720);
        assert_eq!(landed.account_token, "acct");
        assert_eq!(landed.sources.len(), 1);
        assert_eq!(landed.sources[0].machine_id, "server-a");
    }

    #[test]
    fn loading_legacy_json_without_an_id_repairs_only_the_id_not_the_quality() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("legacy-no-id");
        std::fs::write(t.file(), br#"{"account_token":"legacy-account"}"#).unwrap();

        let loaded = load();
        assert!(
            !loaded.client_id.is_empty(),
            "the ordinary identifier repair still happens"
        );
        assert_eq!(loaded.account_token, "legacy-account");
        assert_eq!(loaded.playback_quality(), PlaybackQuality::Original);
        assert_eq!(
            loaded.playback_quality, None,
            "a parsable old file is not fresh and must not acquire a default choice"
        );

        let saved: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap()).unwrap();
        assert_eq!(saved.playback_quality(), PlaybackQuality::Original);
        assert_eq!(saved.playback_quality, None);
    }

    #[test]
    fn loading_with_no_file_records_the_gated_fresh_default() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("fresh-quality");
        assert!(!t.file().exists());

        let loaded = load();
        assert_eq!(
            loaded.playback_quality,
            Some(PlaybackQuality::Auto),
            "the production readiness gate gives only a genuinely fresh install Auto"
        );
        let saved: Session = serde_json::from_slice(&std::fs::read(t.file()).unwrap()).unwrap();
        assert_eq!(
            saved.playback_quality,
            Some(PlaybackQuality::Auto),
            "freshness is decided once and stored explicitly"
        );
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
        assert_eq!(
            s.sources.len(),
            N,
            "a roster entry was overwritten by the other writer"
        );
        assert_eq!(
            s.recents_for("uu-1").len(),
            N,
            "a search term was overwritten by the other writer"
        );
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
                assert_eq!(
                    s.client_id, "cid-1",
                    "a partial read is a signed-out device"
                );
                reads += 1;
            }
        });
        assert_eq!(peek().home_users.len(), 20);
    }

    /// Once a fresh sign-in has recovered a Locked boot (the scenario above), `update` must treat
    /// the run as signed in — not keep refusing the way it would right after `load()` alone, which
    /// left `client_id` empty in memory (an ephemeral id is never persisted for a Locked read; see
    /// [`load`]). Before the cache, `update`'s own `peek_locked` would have re-run `read_locked`
    /// and found the file STILL the old locked envelope (a lower-priority reader never sees this
    /// process's own writes go by), reproducing the empty-client-id refusal on every attempted
    /// change for the rest of the run — a second shape of the same loop.
    #[test]
    fn update_no_longer_refuses_after_a_locked_boot_once_a_fresh_sign_in_has_landed() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("update-after-recovery");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
            },
        };
        std::fs::write(t.file(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();

        let loaded = load();
        assert!(loaded.account_token.is_empty(), "Locked degrades to default");

        save(&signed_in());
        assert!(
            update(|s| Some(Session {
                account_token: s.account_token.clone(),
                client_id: s.client_id.clone(),
                ..Default::default()
            })),
            "the freshly signed-in session must be visible to `update` in the same run"
        );
        assert_eq!(peek().account_token, "acct");
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
        assert!(!update(|s| Some(Session {
            account_token: "acct".into(),
            ..s.clone()
        })));
        assert!(
            !t.file().exists(),
            "a refused cycle must not create the file it refused to write"
        );

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
