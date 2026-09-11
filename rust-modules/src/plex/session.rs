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
//!
//! **"Proven it cannot read it back" means a key manager ANSWERED and said no.** A service that
//! did not answer at all — a stalled call, a registration that never reached the bus, a hub
//! reporting no such service — has proven nothing about the key, and costing a working sealed
//! sign-in over one such boot is the failure this module used to have: see [`LOCKED_UNAVAILABLE`],
//! which keeps the envelope, tells the sign-in screen to offer *Try again*, and settles into the
//! refusal above only after [`UNAVAILABLE_MAX_LAUNCHES`] launches have ended the same way.
//!
//! ## Storage model: one file, two honest states, sealing that is EARNED
//!
//! Decided 2026-09-10 against a decompile of how Apple TV, Prime Video, Netflix and YouTube protect
//! their own stored sign-in on this same television
//! (`docs/measurements/credential-storage-native-apps-2026-09-10.md`): keymanager3 stays the sealed
//! state, there is one file path, and it is always in one of exactly two honest states — **sealed**,
//! where keymanager3 has *proven* (not merely attempted) that it round-trips across a launch, or
//! **0600 plaintext**, everywhere else. No per-install key beside the data (adds nothing a peer
//! process running as our own uid could not already read) and no address/serial-derived key (worse:
//! every process on the box can read those inputs). See [`LOCKED_RECOVERABLE`] /
//! [`write_refused_marker`] / [`plant_probe`] above for the earning mechanism itself.
//!
//! **The shared `/media/developer` namespace is not this module's threat model to solve.** It is
//! measured `drwxrwxrwx root:root` (0777, no sticky bit observed) — every Developer Mode app has
//! its own uid but shares that one directory and its one gid, so 0600 is the whole boundary a
//! *mode* can draw, and a peer app can still unlink or rename a name it cannot read
//! (`write_atomic`'s `O_NOFOLLOW` + `create_new` and `read_owned_regular`'s ownership check are
//! what turn a FOREIGN or symlinked substitution into "rejected", not "parsed as ours" — see the
//! next paragraph for what that guarantee does NOT cover). `repair_owned_mode`
//! narrows the remaining risk one step further: an OWNED regular file found with any group/other
//! bit set is fixed to 0600 in place (via `fchmod` on the already-open, already-checked fd — never
//! a `chmod` by path) rather than refused, because refusing was itself a bug — see the 2026-09-10
//! commit that closed it, and [`ModeTrust`]'s doc for what a *write*-widened mode additionally costs
//! the CONTENT, not merely the file's disclosure. SECURITY.md states the exposure for retail
//! (jailed to `mountappdir`, no shared namespace at all) against Developer Mode explicitly.
//!
//! **Parent directories, and what `O_NOFOLLOW` does and does not reach.** `O_NOFOLLOW` on the final
//! `open(2)` defeats a symlink swapped in at the LEAF name — it says nothing about who can write to
//! the directory that name lives in, and every candidate here passes through one of three parents
//! with a different owner:
//! - `/media/developer` — measured (device read, 2026-09-10) `drwxrwxrwx` **root:root**, no sticky
//!   bit. Every Developer Mode app's own uid can create, rename or unlink entries directly under
//!   it, this install's own files included.
//! - `/media/internal` — `mount ro` under THIS app's own jail profile (`jail_native_devmode.conf`),
//!   `mount rw` under the retail profile (`jail_native.conf`) that never runs on this shared
//!   namespace at all. Its owning uid/gid and mode were **not independently measured on this
//!   device** — unlike `/media/developer` there is no `ls -la` reading of it in
//!   `docs/measurements/` — so nothing beyond the jail's own mount flag is claimed about it here.
//! - This install's own directory (`paths::app_dir()`/`in_app_dir`) — on Developer Mode this
//!   directory is ITSELF a subdirectory *inside* the shared `/media/developer` tree
//!   (`/media/developer/apps/usr/palm/applications/<id>/`), so a peer able to write into its own
//!   enclosing `.../applications/` directory could in principle rename or replace this install's
//!   directory entry too — that exact node was not separately measured, but nothing about
//!   `/media/developer`'s own measured mode narrows it. On a retail install, `mountappdir` puts
//!   only this app's own directory in the mount namespace at all, so this concern does not apply
//!   there.
//!
//! **What ownership + mode checks cannot see: a replay.** A peer with rename rights in
//! `/media/developer` can move this install's own, currently-valid, correctly-owned, correctly-
//! 0600 file ASIDE, let this install write a fresh one, and later move the old bytes back. The
//! replayed copy is genuinely ours by every check this module makes — same uid, same regular file,
//! same 0600 mode, a session/consent shape this build parses fine — so it loads as though it were
//! current. **This is a known, undetected limitation, not a guarantee this module makes**; earlier
//! prose in this file and elsewhere describing a substituted name as always "rejected" was talking
//! about a FOREIGN or symlinked substitution (which the ownership/regular-file check does catch),
//! never about a stale-but-genuine replay (which it structurally cannot). See
//! `tests::a_replayed_older_valid_session_file_is_indistinguishable_from_current` (this module's
//! own tests) for a test that pins the limitation, and SECURITY.md / `docs/install-and-verify.md` for
//! the same statement outside the source tree.
//!
//! **What each shipped version actually wrote, and what this build does when it finds it:**
//!
//! | on disk | v0.6.0 / v0.6.1 wrote it as | v0.6.2 added | this build reads it as |
//! |---|---|---|---|
//! | bare `Session` JSON, 0600 | the only shape that ever existed | — | `Ready{plaintext:true}` |
//! | `SecureEnvelope{format,version:1,sealed}` that opens | wrote it opportunistically, no earning, no marker | — | `Ready{plaintext:false}`, promoted |
//! | same envelope, locked (keymanager3 REFUSED to open it — a real reply, a tag mismatch) | silently re-asked for sign-in every launch (issue #76) — no marker, no recovery | persisted `secure-storage.refused` marker + per-process lock detection | `LOCKED_RECOVERABLE`; a fresh sign-in recovers it |
//! | same envelope, and the key service simply did NOT ANSWER (timeout, failed registration, no such service) | as above — treated as a refusal, so one stalled boot cost the install its sealed storage for good | as above | `LOCKED_UNAVAILABLE`: the envelope is left byte-identical and the sign-in screen offers *Try again*. Bounded by a transient, content-only `secure-storage.unavailable` LAUNCH COUNTER (not the cross-launch refused marker) — after `UNAVAILABLE_MAX_LAUNCHES` such launches it is finally graded the refusal above. An `IdentityUnavailable` open shares none of this counter and never escalates, however many launches it recurs on — see `note_service_unavailable`'s doc. **A FRESH SIGN-IN made while the service is still unanswerable does not wait for any of that** (0.6.4): the seal is attempted, and when that is silent too the 0600 file replaces the envelope at the candidate it was found at, because a sign-in nobody can read back is the worst outcome available — issue #76's second field report, "account name shows at the top, but after a restart I must sign in again" |
//! | secure-shaped file, unrecognized format/version | n/a (format didn't exist yet) | n/a | `LOCKED_UNRECOVERABLE`; **never written over at all** — not as plaintext, and since 2026-09-10 not as a fresh envelope either, on a proven install or any other (`has_unrecognized_secure_envelope`), and since 2026-09-11 not DELETED either when it sits at a candidate this install does not read (`sweep_other_candidates`). Only `clear` removes it |
//! | `secure-storage.refused` marker, no `secure-storage.proven` | n/a | wrote the marker; nothing yet read `proven` | plaintext, durably — `plant_probe` returns immediately while a refused marker exists (see its doc), so a v0.6.2 install stays on the 0600 file forever unless the account signs out; only `clear` removes the marker and reopens the earning path |
//! | this branch's probe/proven files | n/a | n/a | the only inputs that can promote an install to sealing at all |
//!
//! A file this build cannot make sense of is never guessed at, and the two shapes of that differ
//! in exactly one respect. A secure envelope of an unrecognized format or version
//! (`LOCKED_UNRECOVERABLE`) is never replaced by anything this build writes — a fresh sign-in
//! included — because it may be a NEWER build's envelope that will read perfectly again after the
//! upgrade; `clear` (sign out / Delete all local data) is the only thing that removes it. An
//! envelope this build DID open, whose plaintext is not a session (`LOCKED_CORRUPT`), is ours and
//! proven dead, so an explicit fresh sign-in may replace it and nothing else may.
use super::origin::Origin;
use super::probe::Location;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Mutex;

#[cfg(test)]
mod migration_tests;
pub(crate) mod async_persistence;
mod persistence;

#[cfg(test)]
pub(crate) fn commit_canonical_payload_for_test(payload: String) -> bool {
    matches!(persistence::commit_data(payload), persistence::CanonicalCommit::Durable { .. })
}

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

/// Legacy session file locations, best first — see [`crate::paths::session_candidates`] for why
/// this remains a migration SEARCH ORDER rather than the canonical destination. The canonical
/// `state/session.json` record is prepended by the production adapter. The short version: webOS picks one
/// of two jail profiles by install prefix, and they disagree about which directories are writable,
/// so the one hardcoded path was correct under Developer Mode and did not exist under a Homebrew
/// Channel install — where `save()` then dropped the error and the user re-did the QR sign-in on
/// every boot, with a fresh `X-Plex-Client-Identifier` each time.
///
/// Existing external locations retain priority for compatibility. A packaged, writable
/// app-local state directory covers newer jails that deny those external writes.
#[cfg(not(test))]
fn auth_candidates() -> Vec<(std::path::PathBuf, CandidateCategory)> {
    crate::paths::session_candidates()
        .into_iter()
        .map(|(p, tier)| (p, CandidateCategory::of(tier)))
        .collect()
}

fn auth_paths() -> Vec<std::path::PathBuf> {
    auth_candidates().into_iter().map(|(p, _)| p).collect()
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
static LAST_LEGACY_SOURCE: Mutex<Option<(std::path::PathBuf, Vec<u8>)>> = Mutex::new(None);
static UNCERTAIN_LEGACY_SOURCE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// The test build's [`auth_candidates`]: a redirected candidate belongs to no jail tier at all, so
/// it is [`CandidateCategory::Other`] — the variant that exists for exactly this. Deriving the
/// category from the REDIRECT rather than from the path's prefix is what makes the pinned
/// `other:…` wire words a fact about the test seam instead of a fact about the host's
/// `std::env::temp_dir()`, which is `/tmp` on a Linux CI runner and `$TMPDIR` on a Mac.
#[cfg(test)]
fn auth_candidates() -> Vec<(std::path::PathBuf, CandidateCategory)> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(v) => v
            .into_iter()
            .map(|p| (p, CandidateCategory::Other))
            .collect(),
        None => crate::paths::session_candidates()
            .into_iter()
            .map(|(p, tier)| (p, CandidateCategory::of(tier)))
            .collect(),
    }
}

/// Test-only: the multi-candidate form of [`redirect_for_test`], for the issue #76 review's
/// recovery-targeting/sweep coverage — everything else here drives a single candidate, which
/// cannot exercise "the locked envelope is not at `auth_paths()[0]`" at all.
#[cfg(test)]
fn redirect_for_test_multi(paths: Vec<std::path::PathBuf>) {
    crate::storage_worker::drain_for_test();
    COLD_LOAD_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    async_persistence::reset_for_test();
    let mut root = paths
        .first()
        .and_then(|path| path.parent())
        .map(std::path::Path::to_path_buf);
    for path in paths.iter().skip(1) {
        while root
            .as_ref()
            .is_some_and(|candidate| !path.starts_with(candidate))
        {
            if !root.as_mut().is_some_and(std::path::PathBuf::pop) {
                root = None;
                break;
            }
        }
    }
    crate::paths::redirect_persistent_state_root_for_test(root.map(|parent| parent.join("state")));
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(paths);
    *UNCERTAIN_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    clear_cache();
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(CLASS_UNKNOWN, std::sync::atomic::Ordering::Relaxed);
    UNAVAILABLE_NOTED.store(false, std::sync::atomic::Ordering::Relaxed);
    *LAST_FRESH_READBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    FRESH_WRITE_ATTEMPTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    FRESH_SAVE_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *COLD_SESSION_FACTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *COLD_STORAGE_DIAGNOSTIC
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
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
    crate::storage_worker::drain_for_test();
    COLD_LOAD_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    async_persistence::reset_for_test();
    let state_root = p.as_ref().and_then(|path| path.parent()).map(|parent| {
        let path = p.as_ref().expect("path was present");
        if path_file_is_canonical(path) {
            parent.to_path_buf()
        } else {
            parent.join("state")
        }
    });
    crate::paths::redirect_persistent_state_root_for_test(state_root);
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p.map(|p| vec![p]);
    *UNCERTAIN_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    clear_cache();
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(CLASS_UNKNOWN, std::sync::atomic::Ordering::Relaxed);
    // …and the once-per-LAUNCH gate on the unanswered-key-service counter, for the same reason:
    // a redirect is how a test spells "a new launch against the same files", and the bounded
    // escalation that gate protects is counted in launches (see `note_service_unavailable`).
    UNAVAILABLE_NOTED.store(false, std::sync::atomic::Ordering::Relaxed);
    *LAST_FRESH_READBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    FRESH_WRITE_ATTEMPTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    FRESH_SAVE_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *COLD_SESSION_FACTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *COLD_STORAGE_DIAGNOSTIC
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(test)]
fn path_file_is_canonical(path: &std::path::Path) -> bool {
    path.file_name().is_some_and(|name| name == "session.json")
        && path
            .parent()
            .and_then(|parent| parent.file_name())
            .is_some_and(|name| name == "state")
}

/// **One in-process copy of the session.** Synchronous load/save/update paths publish explicitly;
/// asynchronous writes publish at admission, never from their later disk completion. Every writer
/// in this process goes through this module, so the snapshot cannot be overwritten by an older
/// completion. The file only ever moves under a peer process's feet if a second copy of the app is
/// running against it, which is not a case this app supports. [`peek_locked`] serves straight from
/// here once anything has been published, which is what stops the account chip, the menu and every
/// other reader from re-decrypting the file (and re-paying keymanager3's multi-second LS2 budget)
/// on every keypress. [`clear`] (sign-out) empties it.
static CACHE: std::sync::RwLock<Option<Session>> = std::sync::RwLock::new(None);
static COLD_LOAD_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

struct ColdLoadGuard;

impl Drop for ColdLoadGuard {
    fn drop(&mut self) {
        COLD_LOAD_ACTIVE.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn cached() -> Option<Session> {
    CACHE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn publish_cache(s: Session) {
    *CACHE.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

fn clear_cache() {
    *CACHE.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// **Stage B2 (issue #76 field report case 6): publish `s` for THIS RUN even though nothing was
/// persisted.** The disk-only persistence half never publishes, because a later asynchronous
/// completion must not overwrite a newer admitted snapshot. `auth::take_ready` calls this when a
/// synchronous [`save`] returns `false`, so the account chip and everything else `peek` feeds
/// answers correctly for the rest of THIS launch — the write failure is real and is reported
/// separately (see `save_locked`'s own `report_write_failed`), but it must not also make the run
/// itself lie about who is signed in.
pub(crate) fn publish_unpersisted(s: Session) {
    publish_cache(s);
}

const NOT_LOCKED: u8 = 0;
/// The recognized secure envelope (this build's own format + version) is present but this process
/// could not open it — the issue #76 shape: keymanager3 sealed something it can no longer decrypt,
/// on every read, forever. A save explicitly authorized by the successful PIN flow may safely
/// replace this file; there is nothing to lose that the reauthentication does not already re-supply.
const LOCKED_RECOVERABLE: u8 = 1;
/// A secure-shaped file in a format/version THIS BUILD DOES NOT RECOGNIZE AT ALL — never opened,
/// because it is never asked to (`read_locked` only calls `keymanager::open_checked` for its own
/// `SECURE_FORMAT`/version 1). Guarded against a same-version rewrite on the UNPROVEN branch of
/// `save_locked` — see `an_unknown_secure_envelope_version_is_locked_and_never_rewritten_as_plaintext`
/// — because it could as easily be a NEWER build's envelope as a genuinely corrupt one, and
/// overwriting either would destroy it for no reason connected to this device's key manager at
/// all. **That guard is not consulted on the PROVEN branch** — pre-existing, not introduced by
/// this range, and left as a known gap rather than "never" (review finding, 2026-09-10): a proven
/// install's fresh-sign-in save calls `keymanager::seal` unconditionally without first checking
/// `has_secure_locked()`/`LOCKED_STATE` the way the unproven branch does.
const LOCKED_UNRECOVERABLE: u8 = 2;
/// This build's OWN format/version DID open — `keymanager::open_checked` returned real plaintext —
/// but the plaintext did not parse as a `Session`: a genuine corruption of an envelope this install
/// itself once wrote, not the keymanager3 round-trip bug (see [`LOCKED_RECOVERABLE`]) and not an
/// unrecognized foreign envelope (see [`LOCKED_UNRECOVERABLE`]). Issue #76 review (blocker): unlike
/// the other two, a FRESH sign-in over this state may still recover to plaintext — there is nothing
/// left to protect once this build has already proven the envelope is its own and unreadable as a
/// session, and refusing the write is the exact endless sign-in loop the review reported (case 5).
const LOCKED_CORRUPT: u8 = 3;
/// **The key service did not ANSWER this launch, and that is not evidence about the key.** A
/// recognized own-format envelope is on disk and `keymanager::open_checked` never got far enough
/// to say anything about it: no reply inside its budget (`StorageStage::NoReply`), a registration
/// that never reached the bus (`Unreachable`), or the hub answering `-1` for a service that is not
/// on this firmware at all. See [`open_failure_is_transient`] for the whole classification.
///
/// **Deliberately NOT [`LOCKED_RECOVERABLE`]**, which is the state that costs an install its
/// sealed storage: this one writes no refused marker, so a save on the very next launch may seal
/// again, and the envelope on disk is left byte-identical for a launch that CAN read it. A
/// television whose keymanager3 stalls once during boot used to be permanently downgraded to the
/// 0600 file by that single hiccup — the marker is removed only by [`clear`] — which is exactly
/// backwards: a temporary failure must not cost a proven, sealed sign-in.
///
/// It settles rather than retrying forever: [`note_service_unavailable`] counts the LAUNCHES that
/// end here in a small marker and grades the install a real [`LOCKED_RECOVERABLE`] refusal once
/// [`UNAVAILABLE_MAX_LAUNCHES`] of them have passed, the same bounded shape [`check_probe`]
/// already uses for the probe's own unanswered opens.
const LOCKED_UNAVAILABLE: u8 = 4;
/// What the most recent [`read_locked`] in this process found — see [`LOCKED_RECOVERABLE`],
/// [`LOCKED_UNRECOVERABLE`], [`LOCKED_CORRUPT`] and [`LOCKED_UNAVAILABLE`]. Read only by
/// [`save_locked`]'s plaintext-downgrade decision.
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
        // Its own class, never folded into `SecureLocked`: nothing was refused and nothing was
        // downgraded — the sealed envelope is intact and unread. See [`LOCKED_UNAVAILABLE`].
        LOCKED_UNAVAILABLE => SessionStorageClass::SecureUnavailable,
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
#[derive(Clone, Debug)]
struct PendingReport {
    context: crate::telemetry::storage::StorageErrorContext,
    candidate_reads: Option<String>,
    candidate: Option<CandidateCategory>,
}

static PENDING_REPORTS: Mutex<Vec<PendingReport>> = Mutex::new(Vec::new());

fn queue_report(ctx: crate::telemetry::storage::StorageErrorContext) {
    PENDING_REPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(PendingReport {
            context: ctx,
            candidate_reads: None,
            candidate: None,
        });
}

fn queue_report_for_candidate(
    ctx: crate::telemetry::storage::StorageErrorContext,
    candidate: CandidateCategory,
) {
    PENDING_REPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(PendingReport {
            context: ctx,
            candidate_reads: None,
            candidate: Some(candidate),
        });
}

/// Bind the completed candidate summary to reports raised by this cold read while [`IO`] is still
/// held. A later save can queue and drain on another thread, but it can no longer steal this load's
/// evidence because the string travels with the report it describes.
fn attach_candidate_reads_to_pending(reads: &str) {
    for report in PENDING_REPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter_mut()
    {
        if report.candidate_reads.is_none() {
            report.candidate_reads = Some(reads.to_string());
        }
    }
}

/// Which [`crate::telemetry::storage::StorageStage`]s this PROCESS has already REPORTED — "exactly
/// once per process per stage", so a television whose keymanager3 answers the same refusal call
/// after call does not fill the spool with the same report on every later save. **A stage lands
/// here only once its own attempt was actually SENT or safely DEFERRED for a later replay** (Stage
/// B2, issue #76 field report case 5) — see [`report_once`]'s doc for why that is not the same
/// question as "was `report_once` called for it".
static REPORTED_STAGES: Mutex<Vec<crate::telemetry::storage::StorageStage>> =
    Mutex::new(Vec::new());

/// **Stage B2's own set, kept apart from [`REPORTED_STAGES`] on purpose.** A stage whose attempt
/// was DROPPED outright — the Errors channel's own consent question already answered "No" (or this
/// build carries no Sentry endpoint to send to at all) — must never be retried on every later
/// occurrence of the same failure (a save that fails on every roster refresh would otherwise re-run
/// the whole consent-gated attempt on every single one), but it is also not the same fact as "this
/// stage was reported": nothing was ever sent, and nothing is waiting to be. Tracking it separately
/// is what lets [`report_once`] answer "skip, already handled" for both without a caller ever
/// reading a genuinely dropped stage back as a reported one.
static DROPPED_STAGES: Mutex<Vec<crate::telemetry::storage::StorageStage>> = Mutex::new(Vec::new());

/// Route through here rather than `telemetry::storage::report_error` directly so a test can capture
/// what this module tried to report without a real Sentry endpoint compiled in — same shape as
/// `keymanager.rs`'s own `log`/`capture` seam. Both configurations now also call the real
/// consent-gated `telemetry::storage::report_error` (Stage B2): the test capture alone cannot tell
/// [`report_once`] whether an attempt was sent, deferred or dropped, and that three-way split is
/// exactly what decides which of [`REPORTED_STAGES`]/[`DROPPED_STAGES`] a stage lands in.
#[cfg(not(test))]
fn report_storage_error(
    ctx: crate::telemetry::storage::StorageErrorContext,
    candidate_reads: Option<String>,
) -> crate::telemetry::storage::ReportOutcome {
    crate::telemetry::storage::report_error_with_candidate_reads(ctx, candidate_reads)
}
#[cfg(test)]
fn report_storage_error(
    ctx: crate::telemetry::storage::StorageErrorContext,
    candidate_reads: Option<String>,
) -> crate::telemetry::storage::ReportOutcome {
    tests::capture_report(ctx);
    crate::telemetry::storage::report_error_with_candidate_reads(ctx, candidate_reads)
}

/// **Stage B2 (issue #76 field report case 5): a report cannot be burned by an attempt that never
/// actually reported anything.** The old version of this function marked a stage as `REPORTED_STAGES`
/// unconditionally, before even calling [`report_storage_error`] — so a stage found before the
/// consent question was answered, or one the answer already refused, was treated exactly like one
/// that had genuinely gone out, and no later occurrence of that same failure (this launch or, via
/// the persisted refused marker's cross-launch cousin, a much later one) could ever be attempted
/// again — even after the question got a real "Yes". `report_storage_error`'s outcome now decides
/// where a stage lands: [`crate::telemetry::storage::ReportOutcome::Sent`] and `Deferred` are both
/// "this attempt is spoken for" and go to [`REPORTED_STAGES`] (a deferred one is `telemetry::storage`'s
/// own [`crate::telemetry::storage::replay_deferred`] to resolve, not this module's job to retry);
/// `Dropped` goes to [`DROPPED_STAGES`] instead, so it is never conflated with a stage that left real
/// evidence somewhere, while still never being retried on every subsequent occurrence.
fn report_once(report: PendingReport) {
    let ctx = report.context;
    if REPORTED_STAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&ctx.stage)
    {
        return;
    }
    if DROPPED_STAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&ctx.stage)
    {
        return;
    }
    match report_storage_error(ctx, report.candidate_reads) {
        crate::telemetry::storage::ReportOutcome::Sent
        | crate::telemetry::storage::ReportOutcome::Deferred => {
            REPORTED_STAGES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(ctx.stage);
        }
        crate::telemetry::storage::ReportOutcome::Dropped => {
            DROPPED_STAGES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(ctx.stage);
        }
    }
}

/// Drain and send whatever [`queue_report`] collected — called by every public entry point
/// ([`load`], [`update`], [`save`]) AFTER its own `IO` guard has dropped.
fn take_pending_reports() -> Vec<PendingReport> {
    std::mem::take(&mut *PENDING_REPORTS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn send_pending_reports(pending: Vec<PendingReport>) {
    for report in pending {
        report_once(report);
    }
}

/// Test-only: forget every stage this process has already "reported" (see
/// [`tests::capture_report`]) — deliberately NOT folded into [`redirect_for_test`], since the
/// **Issue #76 review (should-fix): a real "Yes" gives a dropped stage its one attempt back.**
/// [`DROPPED_STAGES`] exists apart from [`REPORTED_STAGES`] precisely so a stage the Errors
/// channel's consent question genuinely refused is never conflated with one that actually went
/// out — but until this existed nothing ever read the distinction back: a "No" burned a stage for
/// the rest of the process even after the SAME process later turned Errors on in Settings, which
/// is the shape [`report_once`]'s own doc already claimed was handled and was not. Called from
/// `telemetry::record_with_receipt` from its worker cutoff exactly when a decision newly enables
/// the Errors channel — the same signal `crashreport::discard_pending_before_opt_in` already keys
/// off — so the very next occurrence of a previously-dropped stage is attempted again instead of
/// silently skipped forever.
pub(crate) fn retry_dropped_stages() {
    DROPPED_STAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// once-per-process rule is exactly what a real process never resets on a file redirect either.
///
/// `pub(crate)`: `telemetry::storage`'s own integration tests drive a real `load` through this
/// module and need the same clean slate, and these are PROCESS globals — a stage some earlier test
/// in the binary already reported is a stage `report_once` will silently skip.
#[cfg(test)]
pub(crate) fn reset_report_state_for_test() {
    PENDING_REPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    REPORTED_STAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    DROPPED_STAGES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *LAST_SESSION_WRITE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *LAST_FRESH_READBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    FRESH_WRITE_ATTEMPTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    FRESH_SAVE_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *COLD_SESSION_FACTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *COLD_STORAGE_DIAGNOSTIC
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *LAST_PERSIST.lock().unwrap_or_else(|e| e.into_inner()) = None;
    CANDIDATE_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
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
/// Every candidate `auth_paths()` entry, renamed to carry `suffix` instead of its own file name —
/// the shared shape behind [`refused_marker_paths`], [`proven_marker_paths`] and [`probe_paths`],
/// which all need one sibling-per-candidate the same way `auth_paths()` itself is a search order.
fn sibling_paths(suffix: &str) -> Vec<std::path::PathBuf> {
    auth_paths()
        .into_iter()
        .filter_map(|p| {
            let name = p.file_name()?.to_string_lossy().into_owned();
            let sibling_name = match name.strip_suffix("auth.json") {
                Some(prefix) => format!("{prefix}{suffix}"),
                None => format!("{name}.{suffix}"),
            };
            Some(p.with_file_name(sibling_name))
        })
        .collect()
}

fn refused_marker_paths() -> Vec<std::path::PathBuf> {
    sibling_paths("secure-storage.refused")
}

/// **Stage B1 (issue #76): sealed storage is EARNED, never assumed.** A launch that seals a
/// session and proves the round trip IN-PROCESS has proven nothing about whether a LATER launch —
/// a different LS2 registration, on a backend whose key can be per-boot — can read it back; that
/// is exactly the field report's shape (case 3). So `save_locked` never calls `keymanager::seal`
/// for the real session until a PRIOR launch has proven this install can reopen something it
/// sealed — recorded here, once, and never re-derived from an in-process check again. Present
/// alongside [`refused_marker_paths`] rather than instead of it: the two answer different
/// questions ("can this install trust sealing at all" vs "has it already failed to"), and both can
/// be consulted independently — see [`has_proven_marker`]/[`has_refused_marker`].
fn proven_marker_paths() -> Vec<std::path::PathBuf> {
    sibling_paths("secure-storage.proven")
}

/// The cross-launch probe envelope: a small, non-secret constant ([`PROBE_PLAINTEXT`]) sealed the
/// same way the real session would be, written whenever a save has to fall back to plaintext
/// because the install is not yet [proven](proven_marker_paths) — see [`plant_probe`]. Checked at
/// the next boot by [`check_probe`], which is what actually promotes or refuses the install.
fn probe_paths() -> Vec<std::path::PathBuf> {
    sibling_paths("secure-probe.json")
}

/// Whether a PRIOR (or this) launch has already recorded that keymanager3's envelope could not be
/// opened on this install — see [`refused_marker_paths`]. Checked the same way [`has_secure_locked`]
/// checks for a secure file: owned, regular, readable — never trusting a path some other uid could
/// have planted.
fn has_refused_marker() -> bool {
    refused_marker_paths()
        .iter()
        .any(|p| read_trusted_marker(p).is_some())
}

/// Record the cross-launch verdict: this process's own [`read_locked`] found a recognized secure
/// envelope it could not open. Idempotent — a marker already on disk is left alone, since its
/// content is a fact about the FIRST time this was seen, not something a later read should keep
/// overwriting. No key material, ciphertext or plaintext goes into it, only the version that
/// observed the refusal.
fn write_refused_marker(
    stage: crate::telemetry::storage::StorageStage,
    key_outcome: Option<crate::keymanager::KeyOutcome>,
) {
    if has_refused_marker() {
        return;
    }
    // `stage` is the same closed vocabulary the handled report sends (`no_reply`, `unreachable`,
    // `begin_decrypt`, …): it says HOW the open failed, which the log alone could not once the
    // launch that wrote this is gone. Nothing reads it back but a person with the file.
    let mut body = serde_json::json!({
        "refused_at_version": super::identity::VERSION,
        "reason": "envelope_unopenable",
        "stage": stage.code(),
    });
    // Issue #76's identity decider, carried into the marker for the same reason the stage is:
    // once this launch ends, the log is the only other witness. `None` (unknown — this refusal's
    // seal-time outcome was never recorded, or the probe file itself could not even be parsed) is
    // omitted rather than written as null.
    if let Some(outcome) = key_outcome {
        body["key_outcome"] = serde_json::json!(outcome.code());
    }
    let body = serde_json::to_vec_pretty(&body).unwrap_or_default();
    for path in refused_marker_paths() {
        if write_atomic(&path, &body) {
            return;
        }
    }
    crate::log(
        "session: could not persist the refused-storage marker to ANY candidate path — secure storage may be retried next launch",
    );
}

/// Whether a PRIOR launch has already proven this install can reopen something it sealed — see
/// [`proven_marker_paths`]'s doc. Same trusted reader [`proven_marker_identity`] itself now uses,
/// so this and the production seal gate can never disagree about what counts as proof.
fn has_proven_marker() -> bool {
    proven_marker_identity().is_some()
}

/// **Which LS2 identity the proven marker is proof ABOUT**, or `None` when no marker exists.
///
/// A proof is per identity and cannot be carried across one (issue #76's identity fix): "a probe
/// sealed as `app_id` reopened on a later launch" says nothing about whether an anonymous
/// registration can reopen an anonymous one, and the anonymous case is precisely the one that
/// fails on an affected set. A marker written before this field existed reads as
/// [`Identity::Anonymous`] — correct by construction, since anonymous is the only identity those
/// builds ever registered with.
fn proven_marker_identity() -> Option<crate::keymanager::Identity> {
    proven_marker_paths().iter().find_map(|p| {
        // Trusted, not merely owned: this is the gate `save_locked` reads to decide whether the
        // install may seal at all (`proven_for_this_launch`), so a write-widened marker must be
        // ignored and deleted here exactly as `has_refused_marker`/`has_proven_marker` already do
        // for their own markers — a forged `secure-storage.proven` must never promote an install
        // to sealing (review finding, 2026-09-10).
        let bytes = read_trusted_marker(p)?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        Some(
            value
                .get("identity")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or(crate::keymanager::Identity::Anonymous),
        )
    })
}

/// Is this install proven for the identity THIS launch would seal under?
///
/// The identity is resolved (`keymanager::ensure_identity`, one registration on the first call of
/// the process and nothing after) only once a marker exists at all, so an unproven install pays
/// nothing for the question. **`None` — no registration is possible at all right now — answers
/// `true` deliberately**: the identity question is moot on a launch where `keymanager::seal` is
/// about to fail anyway, and the seal-failure branch preserves an existing envelope where the
/// unproven branch would write plaintext beside it.
fn proven_for_this_launch() -> bool {
    let Some(marker) = proven_marker_identity() else {
        return false;
    };
    crate::keymanager::ensure_identity().is_none_or(|now| now == marker)
}

/// Record that [`check_probe`] just reopened its own probe successfully. Idempotent, same reason
/// [`write_refused_marker`] is: the fact worth keeping is that this was proven at all, not the most
/// recent time it happened to be checked again.
fn write_proven_marker(identity: crate::keymanager::Identity) {
    // Idempotent for the SAME identity; rewritten when it changes, because the marker's whole
    // content is now the claim "this install reopened something sealed as <identity>" and a stale
    // one would let a launch seal on a proof about somebody else.
    if proven_marker_identity() == Some(identity) {
        return;
    }
    let body = serde_json::to_vec_pretty(&serde_json::json!({
        "proven_at_version": super::identity::VERSION,
        "stage": "probe_opened",
        "identity": identity.code(),
    }))
    .unwrap_or_default();
    for path in proven_marker_paths() {
        if write_atomic(&path, &body) {
            return;
        }
    }
    crate::log(
        "session: could not persist the proven-storage marker to ANY candidate path — secure storage may be re-earned next launch",
    );
}

/// The bounded counter behind [`LOCKED_UNAVAILABLE`]: how many LAUNCHES in a row have found a
/// sealed envelope they could not even ask the key service about. Content only — a count, a stage
/// and the version that last wrote it — beside [`refused_marker_paths`] and
/// [`proven_marker_paths`], and unlike either of those it is TRANSIENT: the first launch that
/// actually opens the envelope deletes it (see [`clear_unavailable_marker`]).
fn unavailable_marker_paths() -> Vec<std::path::PathBuf> {
    sibling_paths("secure-storage.unavailable")
}

/// How many launches in a row may find the real session envelope unanswered before the install is
/// finally graded a genuine refusal ([`LOCKED_RECOVERABLE`] + the persisted refused marker, i.e.
/// exactly the behaviour every earlier build had on the FIRST such launch).
///
/// **Bounded for the same two reasons [`PROBE_MAX_ATTEMPTS`] is, pulling opposite ways.** One
/// stalled boot must not cost a healthy television its encryption at rest; but a key service that
/// is permanently silent must not leave the user re-signing-in every launch forever either —
/// nothing else in this file can end that loop, because a save can only fall back to the 0600 file
/// once something has recorded that the envelope is a dead end. Three launches is the same
/// allowance the probe gets, and it is a launch count rather than a retry count on purpose: the
/// question is whether the service is broken ACROSS boots, and [`note_service_unavailable`]
/// therefore counts each launch once however many times the screen's *Try again* re-asks within
/// it.
const UNAVAILABLE_MAX_LAUNCHES: u32 = 3;

/// Whether THIS process has already counted its unanswered open — see
/// [`note_service_unavailable`]. `read_locked` can run several times in one launch (a cold `peek`
/// before `load`, and every press of the sign-in screen's *Try again*), and each one would
/// otherwise spend one of the three launches the install is allowed.
static UNAVAILABLE_NOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn read_unavailable_attempts() -> u32 {
    unavailable_marker_paths()
        .iter()
        .find_map(|p| {
            // Trusted read, like every other marker in this family (review finding, 2026-09-10):
            // a write-widened counter must be ignored and deleted — read back as zero — rather
            // than honoured, or a forged high count could force the very first unanswered launch
            // straight into a permanent refused marker.
            let bytes = read_trusted_marker(p)?;
            let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            Some(v.get("launches")?.as_u64().unwrap_or(0) as u32)
        })
        .unwrap_or(0)
}

/// Delete the unanswered-launch counter.
///
/// Called from every place a run of unanswered launches ENDS, which is deliberately broader than
/// "the session was read back" (review finding, 2026-09-10): the first read that actually OPENS
/// the envelope, whatever it then makes of the plaintext (the service answered); [`not_locked`]
/// and [`not_locked_after_write`], where there is no longer an unopenable envelope on disk for a
/// count to be about; [`note_service_unavailable`] itself once the allowance runs out and the
/// question has been answered the other way; and [`clear`], where the whole install is being reset
/// anyway. The next run must start from zero rather than inherit a count from a firmware hiccup
/// two boots ago.
fn clear_unavailable_marker() {
    for p in unavailable_marker_paths() {
        remove_temp_siblings(&p);
        let _ = std::fs::remove_file(p);
    }
}

/// **Is this open failure evidence about the KEY, or only about the SERVICE?**
///
/// Pure, and the whole of the distinction this state exists for. `NoReply` (the call was made and
/// the budget ran out) and `Unreachable` (the LS2 registration never succeeded, so no call was
/// made at all) say nothing whatever about whether the stored key still opens the stored envelope
/// — they are the stalled/absent-service shape behind issues #75/#76's "slow, then try again"
/// symptom. So does a `-1`: measured on the webOS 4.10 dev set 2026-09-10, that is the HUB's own
/// `"Service does not exist: com.webos.service.keymanager3."` (see `keymanager::service_absent`),
/// and a real keymanager3 uses the same code for an unknown method — either way nothing that owns
/// a key ever looked at ours.
///
/// Everything else IS evidence: a `begin(decrypt)`/`finish(decrypt)` refused with a real service
/// error code (`-10001` "key not found", `-20030` the foreign-key tag mismatch), a decrypt that
/// handed back something unusable, or the interim AES-CFB `PalmKeymanager` envelope this build
/// refuses to open by policy. `None` — the unsupported-key-name shape `open_checked` refuses
/// before any call — is deliberately NOT transient: it is not this install's envelope at all, and
/// it took today's path before this function existed.
fn open_failure_is_transient(refusal: crate::keymanager::LastRefusal) -> bool {
    use crate::telemetry::storage::StorageStage;
    matches!(
        refusal.stage,
        StorageStage::NoReply | StorageStage::Unreachable
    ) || refusal.error_code == Some(-1)
}

/// Count this launch's unanswered open and decide whether the install has run out of patience.
///
/// `Some(state)` — the sealed envelope is preserved untouched and this launch reports
/// [`LOCKED_UNAVAILABLE`]. `None` — [`UNAVAILABLE_MAX_LAUNCHES`] launches have now ended here, so
/// the caller falls through to the refused marker and [`LOCKED_RECOVERABLE`]: a service that has
/// not answered across that many boots is no longer distinguishable from one that never will, and
/// the user has to be able to sign in again for good.
fn note_service_unavailable(
    path: &std::path::Path,
    refusal: crate::keymanager::LastRefusal,
    sealed_identity: Option<crate::keymanager::Identity>,
    category: CandidateCategory,
) -> Option<ReadState> {
    let already_counted = UNAVAILABLE_NOTED.swap(true, std::sync::atomic::Ordering::Relaxed);
    let launches = if already_counted {
        read_unavailable_attempts().max(1)
    } else {
        let next = read_unavailable_attempts().saturating_add(1);
        let body = serde_json::to_vec_pretty(&serde_json::json!({
            "launches": next,
            "stage": refusal.stage.code(),
            "noted_at_version": super::identity::VERSION,
        }))
        .unwrap_or_default();
        // A counter that cannot be written is a counter that never climbs, which would make the
        // escalation below unreachable and leave the install in the unavailable state forever.
        // Say so once rather than silently: the log is the only witness.
        if !unavailable_marker_paths()
            .iter()
            .any(|p| write_atomic(p, &body))
        {
            crate::log(
                "session: could not persist the unanswered-key-service counter — a permanently silent service will not settle",
            );
        }
        next
    };
    if launches >= UNAVAILABLE_MAX_LAUNCHES {
        crate::log(
            "session: the key service has not answered for this envelope across several launches — grading it a refusal",
        );
        clear_unavailable_marker();
        return None;
    }
    crate::log(
        "session: the key service did not answer this launch; the sealed sign-in is left untouched",
    );
    Some(locked(
        LOCKED_UNAVAILABLE,
        path,
        Some(refusal),
        sealed_identity,
        category,
    ))
}

/// The probe's own content — fixed, and carrying nothing that identifies this install or account.
/// [`plant_probe`] seals exactly these bytes; [`check_probe`] accepts only an exact match, so a
/// probe envelope that opens to anything else (corruption, or a genuine key mismatch that somehow
/// still decrypts) is graded as a refusal rather than a pass.
const PROBE_PLAINTEXT: &[u8] = b"plxnative-secure-storage-probe-v1";

/// The probe's on-disk shape: the sealed envelope plus a bounded retry counter — see
/// [`check_probe`]'s `NoReply`/`Unreachable` handling. `#[serde(flatten)]` plus a defaulted
/// `attempts` means a probe written before this counter existed (`attempts` absent) still parses,
/// read as attempt zero.
#[derive(Deserialize, Serialize)]
struct ProbeFile {
    #[serde(flatten)]
    sealed: crate::keymanager::Sealed,
    #[serde(default)]
    attempts: u32,
    /// Issue #76's identity decider: what `generateKey` told THIS launch's seal when it planted
    /// this probe — `keymanager::last_key_outcome()` read right after `keymanager::seal` succeeds
    /// below. `#[serde(default)]` so a probe written before this field existed still parses, read
    /// as `None` (unknown) rather than failing the whole file. Persisted here, not re-derived,
    /// because [`check_probe`] runs in a LATER launch, whose own live `last_key_outcome` says
    /// nothing about the seal that produced this envelope — see `KeyOutcome`'s own doc.
    #[serde(default)]
    key_outcome: Option<crate::keymanager::KeyOutcome>,
}

/// How many launches in a row may find the probe unanswered (`NoReply`/`Unreachable` — the service
/// simply did not reply in time, or the registration never reached the bus) before it is finally
/// graded as a refusal. Bounded so a install whose key manager is genuinely, permanently gone still
/// settles rather than probing forever, while a single slow boot never costs a healthy television
/// its encryption at rest.
const PROBE_MAX_ATTEMPTS: u32 = 3;

/// Ask `keymanager::seal` to protect [`PROBE_PLAINTEXT`] and, if it can, persist the envelope as
/// the cross-launch probe — called whenever [`save_locked`] falls back to plaintext because the
/// install is not yet [proven](proven_for_this_launch). A no-op once the install is already
/// refused, or already proven FOR THE IDENTITY this launch would seal under: neither question is
/// still open, so there is nothing left to earn. **Proven for a DIFFERENT identity is not a stop
/// condition** — that proof says nothing about this launch's owner ([`proven_marker_identity`]),
/// so a conflicting probe is replaced with one this launch's identity can actually answer. Reuses
/// `keymanager::seal`'s own backend cache (`SELECTED`), so a proven-*capable* backend makes this
/// cheap on every save after the first — the expensive path is only ever a backend that is truly
/// unavailable, which costs one refused LS2 registration exactly as it always has.
fn plant_probe() {
    if has_refused_marker() {
        return;
    }
    // Which identity this launch would seal under. `None` — no registration possible at all —
    // means `keymanager::seal` below could not succeed either, so there is nothing to plant.
    let Some(identity) = crate::keymanager::ensure_identity() else {
        return;
    };
    if proven_marker_identity() == Some(identity) {
        return;
    }
    // Issue #76 review (should-fix/nit): the probe's plaintext never changes, and `check_probe`
    // only ever reads the FIRST candidate it finds — so a probe already sitting on disk is all
    // this (or any) unproven launch needs. Without this, every unproven `save`/`update` (a roster
    // refresh, a pin, a quality change) re-sealed and rewrote an equivalent envelope, paying a
    // full keymanager3 `begin`/`finish` LS2 round trip — or, on a stalled service, its multi-second
    // budget — each time, for a file whose bytes never differ.
    // …and a probe already on disk is that launch's, only if it was sealed under the SAME
    // identity. One sealed as somebody else can never promote this launch's identity, so it is
    // replaced rather than waited on. An unparseable one is left exactly where it is: grading it
    // is `check_probe`'s job, and it has a branch for that. Read through the TRUSTED path — a
    // write-widened probe is not evidence of anything, per [`read_trusted_marker`]'s doc, so it is
    // ignored (and removed) exactly like an absent one rather than being trusted for its identity
    // claim.
    for path in probe_paths() {
        let Some(bytes) = read_trusted_marker(&path) else {
            continue;
        };
        let same_identity = serde_json::from_slice::<ProbeFile>(&bytes)
            .map(|probe| probe.sealed.identity == identity)
            .unwrap_or(true);
        if same_identity {
            return;
        }
        crate::log(
            "session: the planted probe was sealed under a different LS2 identity; planting a fresh one",
        );
        remove_probe_files();
        break;
    }
    let Some(sealed) = crate::keymanager::seal(PROBE_PLAINTEXT) else {
        // No usable key manager right now (absent firmware, or a genuine service refusal). There
        // is nothing to persist, and nothing to prove — the install stays on plaintext exactly as
        // it always has when no key manager is usable.
        return;
    };
    // Issue #76's identity decider, captured the only moment it is live: `seal` just called
    // `keymanager::generateKey` to produce `sealed` above, so `last_key_outcome` right now IS the
    // outcome of THIS probe's own key.
    let key_outcome = crate::keymanager::last_key_outcome();
    let Ok(bytes) = serde_json::to_vec_pretty(&ProbeFile {
        sealed,
        attempts: 0,
        key_outcome,
    }) else {
        return;
    };
    for path in probe_paths() {
        if write_atomic(&path, &bytes) {
            return;
        }
    }
}

/// The cross-launch half of [`plant_probe`]: called once, early in [`load`]'s cold path, before
/// this process has read or saved anything of its own. A probe planted by an earlier launch is
/// opened through a FRESH `keymanager::open_checked` call — a new LS2 registration, exactly the
/// boundary a same-launch round trip cannot cross — and the outcome is recorded for every later
/// save on this install to trust without re-deriving it: a match promotes the install to
/// [proven](write_proven_marker) for the identity the probe records, and any OTHER refusal arms
/// the [refused marker](write_refused_marker) with whatever stage the open reached — **except
/// `IdentityUnavailable`**, which drops the probe and reports the stage while arming nothing,
/// because a bus name this launch could not get is not a verdict on the key. Either way the probe
/// file itself is removed — it has answered the one question it existed to ask (or, for the two
/// unanswered stages, is left in place for a bounded number of further launches).
/// Delete every probe candidate — called once the probe has answered its question for good (a
/// match, a mismatch, or a refusal past [`PROBE_MAX_ATTEMPTS`]). Issue #76 review (nit): this runs
/// BEFORE the marker write in every caller below, not after — a crash in the gap used to be able to
/// leave a probe file behind carrying the marker's own verdict already recorded, so a LATER key
/// change (a firmware update, a different registration) reopened the stale probe and could arm the
/// refused marker on an install [`check_probe`] had already proven. Removing first means the worst
/// case of a crash in the gap is a leftover probe with NO marker yet — which simply gets re-read
/// (or, once nothing planted it again, re-planted) next launch, the harmless direction.
fn remove_probe_files() {
    for p in probe_paths() {
        remove_temp_siblings(&p);
        let _ = std::fs::remove_file(p);
    }
}

/// Issue #76's storage telemetry, from the PROBE side — [`check_probe`]'s own failure branches
/// used to arm the refused marker and stop there, reporting nothing: nothing else in this file
/// called `queue_report` for a probe outcome, so a probe that failed to reopen left no telemetry
/// trace at all, only the marker. `key_outcome` is the probe's own PERSISTED value (from the
/// launch that planted it), never a live read — see [`ProbeFile::key_outcome`]'s doc. Called AFTER
/// [`write_refused_marker`] wherever there is one, so `refused_marker` below reads the marker this
/// same failure just armed — and the `IdentityUnavailable` call site deliberately has none, which
/// is exactly what that report then says: the stage, with `refused_marker` still false.
/// `sealed_identity` is the identity the PROBE FILE records — the owner its key already has —
/// which is what makes a probe report answer issue #76's actual question rather than restate this
/// launch's own registration a third time (review finding, 2026-09-10). `None` only where there
/// was no parseable probe to read one from.
fn queue_probe_report(
    stage: crate::telemetry::storage::StorageStage,
    service_error_code: Option<i64>,
    key_outcome: Option<crate::keymanager::KeyOutcome>,
    sealed_identity: Option<crate::keymanager::Identity>,
) {
    queue_report(crate::telemetry::storage::StorageErrorContext {
        stage,
        service_error_code,
        class: storage_class(),
        refused_marker: has_refused_marker(),
        key_outcome,
        registered_with_app_id: crate::keymanager::registered_with_app_id(),
        registered_with_name: crate::keymanager::registered_with_name(),
        sealed_identity,
    });
}

fn check_probe() {
    for path in probe_paths() {
        let Some(bytes) = read_trusted_marker(&path) else {
            continue;
        };
        match serde_json::from_slice::<ProbeFile>(&bytes) {
            Ok(probe) => match crate::keymanager::open_checked(&probe.sealed) {
                (Some(plain), _) if plain == PROBE_PLAINTEXT => {
                    let identity = probe.sealed.identity;
                    remove_probe_files();
                    write_proven_marker(identity);
                }
                (Some(_), _) => {
                    remove_probe_files();
                    write_refused_marker(
                        crate::telemetry::storage::StorageStage::RoundtripMismatch,
                        probe.key_outcome,
                    );
                    queue_probe_report(
                        crate::telemetry::storage::StorageStage::RoundtripMismatch,
                        None,
                        probe.key_outcome,
                        Some(probe.sealed.identity),
                    );
                }
                (None, refusal) => {
                    let stage = refusal
                        .map(|r| r.stage)
                        .unwrap_or(crate::telemetry::storage::StorageStage::EnvelopeLocked);
                    // Issue #76 review (should-fix): `NoReply`/`Unreachable` proves nothing about
                    // the KEY — only that keymanager3 did not answer within its ~4s boot-race
                    // budget, or that the registration itself never reached the bus. Arming the
                    // refused marker on that evidence alone permanently downgrades a healthy
                    // television to plaintext over one transient hiccup, with no retry ever (the
                    // marker is removed only by `clear()`). Leave the probe in place instead,
                    // bounded, so a later launch gets to try again before this is graded a real
                    // refusal.
                    // **The identity this probe was sealed under is not obtainable on this
                    // launch.** That is a fact about a bus NAME, not about the key or the
                    // service, so it may never arm the refused marker — and unlike the two
                    // unanswered stages below it does not get better by trying the same envelope
                    // again either: this launch's identity is what it is. Drop the probe (with
                    // its report) so the next save plants one under the identity this install
                    // actually has.
                    if stage == crate::telemetry::storage::StorageStage::IdentityUnavailable {
                        remove_probe_files();
                        queue_probe_report(
                            stage,
                            None,
                            probe.key_outcome,
                            Some(probe.sealed.identity),
                        );
                        return;
                    }
                    if matches!(
                        stage,
                        crate::telemetry::storage::StorageStage::NoReply
                            | crate::telemetry::storage::StorageStage::Unreachable
                    ) && probe.attempts + 1 < PROBE_MAX_ATTEMPTS
                    {
                        let retried = ProbeFile {
                            sealed: probe.sealed,
                            attempts: probe.attempts + 1,
                            key_outcome: probe.key_outcome,
                        };
                        if let Ok(bytes) = serde_json::to_vec_pretty(&retried) {
                            write_atomic(&path, &bytes);
                        }
                        return;
                    }
                    let sealed_identity = Some(probe.sealed.identity);
                    remove_probe_files();
                    write_refused_marker(stage, probe.key_outcome);
                    queue_probe_report(
                        stage,
                        refusal.and_then(|r| r.error_code),
                        probe.key_outcome,
                        sealed_identity,
                    );
                }
            },
            // A probe file this build cannot even parse as a sealed envelope — corruption, or a
            // shape from a future build. There is no service reply to attach a stage to; the same
            // reasoning `read_locked`'s own unrecognized-envelope branch already applies, and there
            // is no `ProbeFile` to read a key outcome from either.
            Err(_) => {
                remove_probe_files();
                write_refused_marker(
                    crate::telemetry::storage::StorageStage::EnvelopeUnparseable,
                    None,
                );
                queue_probe_report(
                    crate::telemetry::storage::StorageStage::EnvelopeUnparseable,
                    None,
                    None,
                    // Nothing parsed, so there is no recorded owner to name.
                    None,
                );
            }
        }
        return;
    }
}

/// Whether this process has already logged that a save skipped sealing purely because of the
/// persisted marker — see [`write_refused_plaintext`]. Once per process: every `update()` on an
/// install already downgraded to plaintext takes the same branch, and repeating the line on every
/// roster refresh would drown the log in a restatement of a fact recorded once already.
static MARKER_SKIP_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn log_marker_skip_once() {
    if !MARKER_SKIP_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::log(
            "session: secure storage is marked refused on this install; keeping the 0600 file",
        );
    }
}

/// The same once-per-process courtesy as [`log_marker_skip_once`], for the OTHER reason a save
/// stays on the 0600 file: this install has not yet earned sealed storage at all (no prior launch
/// has proven a probe reopens) — see [`write_unproven_plaintext`].
static UNPROVEN_SKIP_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn log_unproven_skip_once() {
    if !UNPROVEN_SKIP_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::log(
            "session: secure storage has not yet been proven on this install; keeping the 0600 file and probing",
        );
    }
}

/// The same once-per-process courtesy again, for the THIRD reason a save writes nothing: the file
/// on disk is a secure envelope of a shape this build does not recognize, so nothing may replace
/// it. Once per process, because an install in that state takes this branch on every roster
/// refresh for as long as it runs.
static FOREIGN_SKIP_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn log_foreign_envelope_skip_once() {
    if !FOREIGN_SKIP_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::log(
            "session: the saved sign-in is a secure file this build does not recognize; leaving it untouched",
        );
    }
}

/// The same courtesy for the OTHER half of that rule — a foreign envelope at a candidate this
/// install does not read, which [`sweep_other_candidates`] steps around instead of deleting. Once
/// per process, because every save an install in that state performs sweeps past the same file.
static FOREIGN_KEPT_LOGGED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn log_foreign_envelope_kept_once() {
    if !FOREIGN_KEPT_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        crate::log(
            "session: another candidate path holds a secure file this build does not recognize; sweeping around it",
        );
    }
}

/// The pure gate behind [`save_locked`]'s plaintext-downgrade decision — whether `keymanager::seal`
/// may even be asked to try. Two DIFFERENT reasons force the plaintext branch, and this is their
/// union:
///
/// - **Per-process** (`locked_state == LOCKED_RECOVERABLE`): THIS process's own [`read_locked`]
///   already proved the on-disk envelope unopenable. Only refuses to seal once the save itself
///   carries explicit fresh-reauthentication authority (`saving_fresh_sign_in`) — an unrelated
///   writer with cached credentials must never be the thing that destroys it (see
///   `update_after_a_locked_boot_does_not_destroy_the_locked_envelope`).
/// - **Persisted** (`marker_present`): a PRIOR launch already proved it — the cross-launch half
///   issue #76's review asked for. Once true it stays true for EVERY save on this install,
///   `saving_fresh_sign_in` included: there is nothing left to protect, because this install has
///   already been downgraded to plaintext once, and the marker exists precisely to stop it being
///   re-sealed the next time an in-process round trip happens to look clean.
///
/// **[`LOCKED_UNAVAILABLE`] is deliberately in NEITHER, and that has not changed** — a transient
/// failure still costs an install nothing. Sealing stays permitted, so a save made once the
/// service has come back (the user pressed *Try again*, or simply took a minute over the QR code)
/// writes a fresh envelope in the ordinary way, and this gate is what keeps that possible: putting
/// the unavailable state in here would downgrade a healthy television to the 0600 file over one
/// stalled boot, which is the very thing the state exists to prevent.
///
/// **What changed in 0.6.4 is the other side of that save, and it is not this function's to
/// decide.** A save that reaches [`save_locked`]'s dead ends with the service *still* silent —
/// either the unproven "a secure file is present" branch or the post-seal-failure one — and that
/// carries a completed sign-in now recovers to the 0600 file at the candidate the envelope was
/// found at, instead of preserving ciphertext nobody on this install can open. The distinction
/// this gate draws is therefore between "may we ASK the key manager" (yes, always, for a
/// transient) and "what do we do once asking has failed" (the caller's, on the evidence of that
/// failure). Issue #76's second field report is what the old answer cost: the sign-in lived for
/// one run and the next launch asked for the QR code again, and an `IdentityUnavailable` open —
/// which never escalates, however many launches it recurs on — had no exit at all.
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
    /// Unknown top-level fields are retained when this build performs a read-modify-write. This
    /// keeps a forward-upgraded session's newer preferences intact without copying opaque nested
    /// objects across account/profile replacement boundaries.
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) extensions: BTreeMap<String, Value>,
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
    update_ordinary(|cur| {
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
    snapshot().last_hero_blur
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
/// Opaque future fields are retained on routine rewrites, but never formatted into logs.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct OpaqueExtensions(pub(crate) BTreeMap<String, Value>);

impl OpaqueExtensions {
    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for OpaqueExtensions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<opaque extensions>")
    }
}

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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub(crate) extensions: OpaqueExtensions,
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
                ..Default::default()
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

/// **The disk-operation lock.** Cold reads and writes take it, while initialized snapshot reads
/// use the separate short-lived [`CACHE`] RwLock. A read-modify-write held across [`update`] is atomic against every
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
/// It is held across the whole write, [`write_atomic`]'s `sync_all` included. Initialized `peek`
/// and `load` read the in-memory snapshot before this lock, so disk I/O cannot stall those reads.
/// Do not add per-frame disk readers; cache derived views by a cheap generation where possible.
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
    // The initialized snapshot is independent of the disk writer. In particular, never take IO
    // just to read an already-loaded flag while another thread is waiting for flash/keymanager.
    if let Some(s) = cached() {
        return s;
    }
    if COLD_LOAD_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return Session::default();
    }
    let (s, pending) = {
        let _io = io();
        let s = peek_locked();
        (s, take_pending_reports())
    };
    // The cold-cache fallback inside `peek_locked` can be the first `read_locked` in the process
    // (see its own doc) and can therefore queue a storage report the same way `load` can — drained
    // here for the same reason every other public entry point drains: `IO` must already be
    // released before `report_error` does its own spool I/O.
    send_pending_reports(pending);
    s
}

/// Cache-only runtime snapshot. Cold boot owns the one blocking read through [`start_load`]; UI,
/// auth refresh and preference paths use this accessor so a missing initialization cannot turn a
/// flag read into disk/keymanager work.
pub(crate) fn snapshot() -> Session {
    cached().unwrap_or_default()
}

/// [`peek`] with the lock already held — the read half every entry point here shares.
///
/// Serves [`CACHE`] once anything has been published to it in this process, and only falls back to
/// a real [`read_locked`] the first time — before any [`load`]/[`save_locked`]/[`update`] in this
/// process has run. Production cold boot owns that first read through [`start_load`] on the shared
/// persistence worker; while it is active, [`peek`] returns an empty snapshot instead of entering
/// this fallback. The fallback remains for compatibility callers and migration/security tests, not
/// as a UI-thread boot path.
///
/// **That cold-cache fallback also sets [`LOCKED_STATE`]/[`LOCKED_PATH`]**, same as any other
/// `read_locked` call. A compatibility caller that invokes [`peek`] before production has admitted
/// [`start_load`] can therefore still be the read that later authorizes [`save_locked`]'s plaintext
/// recovery write. That is intentional, not an oversight: the verdict recorded is a fact about
/// what is ON DISK, true regardless of which caller observed it first, and a `peek` alone never
/// writes. The app boot gate prevents a keypress from taking this compatibility path.
fn peek_locked() -> Session {
    if let Some(s) = cached() {
        return s;
    }
    let s = match read_locked() {
        ReadState::Ready { session, .. } => session,
        ReadState::Missing
        | ReadState::Cleared
        | ReadState::CanonicalBlocked
        | ReadState::Locked { .. } => Session::default(),
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
    Cleared,
    CanonicalBlocked,
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

/// Record what this read found in [`LOCKED_STATE`] (and, for a recoverable or own-format-corrupt
/// lock, which candidate path it was found at, in [`LOCKED_PATH`]) and hand back the same
/// [`ReadState`] — every return point in [`read_locked`] goes through one of these two so the three
/// stay in lockstep. `kind` is one of [`LOCKED_RECOVERABLE`], [`LOCKED_UNRECOVERABLE`],
/// [`LOCKED_CORRUPT`] or [`LOCKED_UNAVAILABLE`] (the match just below names the last one
/// explicitly, precisely because it is not one of the first three).
fn locked(
    kind: u8,
    path: &std::path::Path,
    refusal: Option<crate::keymanager::LastRefusal>,
    sealed_identity: Option<crate::keymanager::Identity>,
    category: CandidateCategory,
) -> ReadState {
    LOCKED_STATE.store(kind, std::sync::atomic::Ordering::Relaxed);
    // `LOCKED_PATH` is the target a fresh-sign-in recovery write replaces — meaningful for
    // `LOCKED_RECOVERABLE` (`save_locked`'s own recovery branch), `LOCKED_CORRUPT` (issue #76
    // review, blocker: the unproven branch's own recovery) and, since 0.6.4, `LOCKED_UNAVAILABLE`
    // (the same recovery, for an envelope no key service on this install would answer for — see
    // `save_locked`). Never for `LOCKED_UNRECOVERABLE`, a foreign envelope no fresh sign-in may
    // ever touch. Recording the path is not itself permission to write over it: each of the three
    // states earns that separately, in `save_locked`.
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = matches!(
        kind,
        LOCKED_RECOVERABLE | LOCKED_CORRUPT | LOCKED_UNAVAILABLE
    )
    .then(|| path.to_path_buf());
    // Issue #76 storage telemetry: a read landing Locked is one of the two triggers
    // `save_locked`'s own seal failure is the other — for a handled report, reported at most once
    // per process per stage (`report_once`, drained by the caller after `IO` is released).
    // `LOCKED_RECOVERABLE` is exactly `keymanager::open` having been attempted and failed
    // (`EnvelopeLocked`) versus a shape this build never asked a key manager to open at all (an
    // unrecognized envelope) or one that decrypted fine but did not parse as a session
    // (`EnvelopeUnparseable`), neither of which has a service reply to attach a code from.
    // When the open reached a stage of its own (`begin_decrypt` with a code, `no_reply`,
    // `unreachable`, …) the report carries THAT — it is the question a dashboard on these sets
    // needs answered, and `EnvelopeLocked` says only that the envelope did not open.
    use crate::telemetry::storage::{StorageErrorContext, StorageStage};
    let stage = match (kind, refusal) {
        // `LOCKED_UNAVAILABLE` always carries a refusal (it is classified FROM one — see
        // `open_failure_is_transient`), and the stage IS the report: `no_reply` and `unreachable`
        // are precisely what a dashboard needs to tell a stalled key service apart from a refused
        // one.
        (LOCKED_RECOVERABLE | LOCKED_UNAVAILABLE, Some(r)) => r.stage,
        (LOCKED_RECOVERABLE, None) => StorageStage::EnvelopeLocked,
        _ => StorageStage::EnvelopeUnparseable,
    };
    let service_error_code = refusal.and_then(|r| r.error_code);
    // Issue #76's identity decider: a plain READ never calls `generateKey` (only `seal` does), so
    // this process's own `last_key_outcome` is live evidence about a DIFFERENT call (an earlier
    // `seal` this same launch may have made) rather than about the seal that produced THIS
    // envelope — there is no persisted per-envelope record the way `ProbeFile::key_outcome` is for
    // a probe. Carried anyway, as the process-global "most recent" fact `LAST_REFUSAL` already is,
    // and `None` on the common cold-boot path where no `seal` has happened yet this launch.
    let key_outcome = crate::keymanager::last_key_outcome();
    queue_report_for_candidate(
        StorageErrorContext {
            stage,
            service_error_code,
            class: storage_class(),
            refused_marker: has_refused_marker(),
            key_outcome,
            registered_with_app_id: crate::keymanager::registered_with_app_id(),
            registered_with_name: crate::keymanager::registered_with_name(),
            // **The envelope's own owner, not this launch's** (review finding, 2026-09-10). The two
            // bools above say which registration this process latched; this says which one the key
            // being reported on already belongs to, and issue #76's hypothesis is precisely that they
            // differ. `None` for `LOCKED_UNRECOVERABLE`, where nothing parsed far enough to record one.
            sealed_identity,
        },
        category,
    );
    ReadState::Locked {
        recoverable: kind == LOCKED_RECOVERABLE,
    }
}

/// Publish a read that did NOT end locked — and, with it, end any run of unanswered launches.
///
/// **The counter is cleared here rather than only on the healthy-open path** (review finding,
/// 2026-09-10). Every route into this function means the same thing: this launch found no sealed
/// envelope it failed to open. It read one back (`Ready { plaintext: false }`), or the envelope is
/// no longer there at all — the file is now the 0600 plaintext one, or there is no candidate left
/// (an untrusted candidate removed above, a sign-out that raced this read). A count describes a
/// run of launches against ONE envelope, so carrying it past any of those lets a firmware hiccup
/// from two boots ago spend down a LATER genuine run's allowance and escalate an install to a
/// permanent refusal a launch early. `LOCKED_UNAVAILABLE` returns before it can ever reach here,
/// so this cannot erase the count the same read just wrote.
fn not_locked(state: ReadState) -> ReadState {
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    clear_unavailable_marker();
    LAST_CLASS.store(
        match &state {
            ReadState::Ready {
                plaintext: true, ..
            } => CLASS_PLAINTEXT,
            ReadState::Ready {
                plaintext: false, ..
            } => CLASS_SECURE,
            ReadState::Missing
            | ReadState::Cleared
            | ReadState::CanonicalBlocked
            | ReadState::Locked { .. } => CLASS_NONE,
        },
        std::sync::atomic::Ordering::Relaxed,
    );
    state
}

/// **Which jail-visible tier a session candidate sits in** — taken from the search order that
/// built it ([`crate::paths::SessionTier`], via [`CandidateCategory::of`]), never carried as the
/// path itself: see [`CandidateRead`]'s doc for why nothing here may ever reach a log line or a
/// report as a literal path. Mirrors the four real locations
/// [`crate::paths::session_candidates`] can hand back, in the same priority order that module's
/// own doc explains (`Developer`/`Internal` outside the app entirely, `AppDir` inside it, `Runtime`
/// only for a steerable build's own instance root); `Other` is the escape hatch for a test's own
/// temp-directory candidate, which matches none of the four.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CandidateCategory {
    /// `/media/developer/<id>-auth.json` — outside the app dir, survives a reinstall.
    Developer,
    /// `/media/internal/.<id>-auth.json` — the retail-jail writable fallback.
    Internal,
    /// `paths::app_dir()`-relative (packaged `state/auth.json`, or a legacy migration path).
    AppDir,
    /// `paths::runtime_dir()`-relative — a steerable build's own per-instance `auth.json`.
    Runtime,
    /// Matches none of the above — a test's own temp-directory candidate, on the host.
    Other,
}

impl CandidateCategory {
    /// Every variant, in no particular order — the exhaustiveness source for
    /// `tests::read_rejection_and_candidate_category_wire_words_round_trip`.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Developer,
        Self::Internal,
        Self::AppDir,
        Self::Runtime,
        Self::Other,
    ];

    #[cfg(test)]
    #[allow(dead_code)]
    fn _assert_all_variants_covered(v: Self) {
        match v {
            Self::Developer | Self::Internal | Self::AppDir | Self::Runtime | Self::Other => {}
        }
    }

    /// The word this category is reported as — telemetry-pinned, never renamed casually.
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Developer => "developer",
            Self::Internal => "internal",
            Self::AppDir => "app_dir",
            Self::Runtime => "runtime",
            Self::Other => "other",
        }
    }

    /// The category one real [`crate::paths::session_candidates`] entry is — a straight
    /// mapping of that module's [`crate::paths::SessionTier`], which knows what it built.
    ///
    /// **This used to be a prefix match on the path** (`starts_with(runtime_dir())`, then
    /// `app_dir()`, then the bare `/media/...` literals), which is the same question asked of a
    /// string that no longer remembers the answer. On a host test binary `runtime_dir()` is the
    /// literal `/tmp`, so a test's own `std::env::temp_dir()` candidate categorized as `Runtime`
    /// wherever `temp_dir()` is `/tmp` (every Linux CI runner) and as `Other` on a Mac, i.e. the
    /// pinned `other:…` wire words were green locally and red on the machine that gates the PR.
    /// The search order is the only thing that ever knew which tier an entry is, so that is where
    /// the answer now comes from; [`Other`](Self::Other) is reachable only through the test
    /// redirect, which belongs to no tier at all.
    pub(crate) fn of(tier: crate::paths::SessionTier) -> Self {
        match tier {
            crate::paths::SessionTier::Runtime => Self::Runtime,
            crate::paths::SessionTier::Developer => Self::Developer,
            crate::paths::SessionTier::Internal => Self::Internal,
            crate::paths::SessionTier::AppDir => Self::AppDir,
        }
    }
}

/// **One candidate's outcome for THIS launch's [`read_locked`]** — issue #76's field report gap:
/// a session file that exists but is rejected (wrong mode, wrong owner, too large, not a regular
/// file) collapsed into the same `None` as a file that was never there at all, so nothing said
/// which candidate, or why. `category` comes from the candidate's place in the search order, and
/// `rejection`/
/// `accepted_as` never carry the bytes or the path itself — only [`ReadRejection::wire`] words and
/// the fixed `plaintext`/`secure` markers — because this is what a report and, eventually, a
/// screen reads, and a path or a byte of content is exactly what neither may ever show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CandidateRead {
    pub category: CandidateCategory,
    /// `Some` for a candidate this launch declined — see [`ReadRejection`]. `None` for a candidate
    /// that was read and parsed as a recognized shape, however that shape then fared afterwards
    /// (an opened, corrupt or locked secure envelope are all "accepted as secure" here — what a
    /// key-service open then did with it is [`crate::telemetry::storage::StorageErrorContext`]'s
    /// question, not this one's).
    pub rejection: Option<ReadRejection>,
    /// `Some("plaintext")` or `Some("secure")` for a candidate whose bytes were owned, trusted and
    /// parsed as one of this build's two recognized shapes. `None` otherwise.
    pub accepted_as: Option<&'static str>,
}

/// This launch's candidate reads, in [`auth_paths`] order, replaced wholesale at the start of every
/// [`read_locked`] — never accumulated across launches or across an in-process retry, since a
/// stale entry from a previous read would misreport which candidate THIS load actually found.
static CANDIDATE_READS: Mutex<Vec<CandidateRead>> = Mutex::new(Vec::new());

fn record_candidate_read(
    category: CandidateCategory,
    rejection: Option<ReadRejection>,
    accepted_as: Option<&'static str>,
) {
    CANDIDATE_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(CandidateRead {
            category,
            rejection,
            accepted_as,
        });
}

/// This launch's per-candidate read outcomes, in priority order — see [`CandidateRead`].
pub(crate) fn last_candidate_reads() -> Vec<CandidateRead> {
    CANDIDATE_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// [`last_candidate_reads`] as the one line this goes into a log or a report as — **never a path**,
/// by construction: every piece is a fixed wire word or a bare errno. Shape:
/// `"developer:open_failed:13,internal:missing,app_dir:plaintext"`.
pub(crate) fn candidate_reads_wire() -> String {
    last_candidate_reads()
        .iter()
        .map(|c| {
            let outcome = match (c.rejection, c.accepted_as) {
                (Some(rej), _) => match rej.errno() {
                    Some(errno) => format!("{}:{errno}", rej.wire()),
                    None => rej.wire().to_string(),
                },
                (None, Some(accepted)) => accepted.to_string(),
                (None, None) => "unknown".to_string(),
            };
            format!("{}:{outcome}", c.category.wire())
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// What the cold boot read actually handed to the routing gate, captured before `load` can mint a
/// client id or migrate/reseal anything. Fixed categories and booleans only: safe for a local
/// Details panel and incapable of exposing a token, server identity, or filesystem path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ColdSessionFacts {
    pub winner: Option<CandidateCategory>,
    pub account_token_present: bool,
    pub pms_token_present: bool,
    pub server_dialable: bool,
    pub can_go_local: bool,
}

static COLD_SESSION_FACTS: Mutex<Option<ColdSessionFacts>> = Mutex::new(None);

pub(crate) fn cold_session_facts() -> Option<ColdSessionFacts> {
    *COLD_SESSION_FACTS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Immutable evidence from this process's one cold session read. The error context is copied from
/// the report queued by that exact read, before a retry or fresh save can replace KeyManager's live
/// globals. Candidate words and eligibility are bounded, path/token-free values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ColdStorageDiagnostic {
    pub errors: Vec<ColdStorageError>,
    pub candidate_reads: String,
    pub facts: ColdSessionFacts,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ColdStorageError {
    pub candidate: CandidateCategory,
    pub context: crate::telemetry::storage::StorageErrorContext,
}

/// Storage errors raised by the most recent explicitly-authorized fresh credential save. Unlike
/// [`ColdStorageError`], a save-wide failure is not always attributable to one candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FreshSaveError {
    pub candidate: Option<CandidateCategory>,
    pub context: crate::telemetry::storage::StorageErrorContext,
}

static FRESH_SAVE_ERRORS: Mutex<Vec<FreshSaveError>> = Mutex::new(Vec::new());

pub(crate) fn fresh_save_errors() -> Vec<FreshSaveError> {
    FRESH_SAVE_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

static COLD_STORAGE_DIAGNOSTIC: Mutex<Option<ColdStorageDiagnostic>> = Mutex::new(None);

pub(crate) fn cold_storage_diagnostic() -> Option<ColdStorageDiagnostic> {
    COLD_STORAGE_DIAGNOSTIC
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn capture_cold_storage_diagnostic(
    report_start: usize,
    candidate_reads: String,
    facts: ColdSessionFacts,
) {
    let errors = PENDING_REPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(report_start..)
        .unwrap_or_default()
        .iter()
        .filter_map(|report| {
            report.candidate.map(|candidate| ColdStorageError {
                candidate,
                context: report.context,
            })
        })
        .collect();
    *COLD_STORAGE_DIAGNOSTIC
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(ColdStorageDiagnostic {
        errors,
        candidate_reads,
        facts,
    });
}

fn capture_cold_session_facts(read: &ReadState) {
    let session = match read {
        ReadState::Ready { session, .. } => Some(session),
        ReadState::Missing
        | ReadState::Cleared
        | ReadState::CanonicalBlocked
        | ReadState::Locked { .. } => None,
    };
    let facts = ColdSessionFacts {
        winner: last_candidate_reads()
            .into_iter()
            .find(|candidate| candidate.accepted_as.is_some())
            .map(|candidate| candidate.category),
        account_token_present: session.is_some_and(|s| !s.account_token.is_empty()),
        pms_token_present: session.is_some_and(|s| !s.pms_token().is_empty()),
        server_dialable: session.is_some_and(Session::server_dialable),
        can_go_local: session.is_some_and(Session::can_go_local),
    };
    crate::log(&format!(
        "session: cold eligibility winner={} account={} pms={} server={} local={}",
        facts.winner.map_or("none", CandidateCategory::wire),
        u8::from(facts.account_token_present),
        u8::from(facts.pms_token_present),
        u8::from(facts.server_dialable),
        u8::from(facts.can_go_local),
    ));
    *COLD_SESSION_FACTS.lock().unwrap_or_else(|e| e.into_inner()) = Some(facts);
}

/// The first usable candidate, retaining whether an encrypted file exists but cannot be opened.
fn read_locked() -> ReadState {
    match persistence::load() {
        persistence::CanonicalRead::Missing => {
            *LAST_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let state = read_candidates(None);
            if !matches!(state, ReadState::Missing) {
                if let Some((path, bytes)) = LAST_LEGACY_SOURCE
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                {
                    // A legacy source is retired only after the domain reader selected it as a
                    // valid Session. In particular, a recognized secure envelope that is locked
                    // or unavailable remains at its old name until an explicit fresh sign-in;
                    // a newer/foreign envelope must remain recoverable by a newer binary.
                    let migratable = matches!(&state, ReadState::Ready { .. });
                    let (result, _) = if migratable {
                        persistence::migrate_exact(&path, &bytes)
                    } else {
                        (
                            persistence::CanonicalCommit::Failed(
                                crate::storage::StoreError::InvalidSchema,
                            ),
                            path.clone(),
                        )
                    };
                    if matches!(result, persistence::CanonicalCommit::Durable { .. }) {
                        let mut parents = std::collections::BTreeSet::new();
                        if !remove_cleanup_file(&path, &mut parents)
                            || !sync_cleanup_parents(parents)
                        {
                            crate::log("session: canonical migration committed but legacy cleanup failed");
                        }
                    } else if matches!(result, persistence::CanonicalCommit::Uncertain { .. }) {
                        *UNCERTAIN_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(path);
                    }
                }
            }
            state
        }
        persistence::CanonicalRead::Data { payload, .. } => {
            read_candidates(Some((persistence::path(), payload.into_bytes())))
        }
        persistence::CanonicalRead::Cleared { .. } => ReadState::Cleared,
        persistence::CanonicalRead::Blocked(_) => ReadState::CanonicalBlocked,
    }
}

fn read_candidates(canonical: Option<(std::path::PathBuf, Vec<u8>)>) -> ReadState {
    CANDIDATE_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    let canonical_present = canonical.is_some();
    let candidates = canonical
        .map(|(path, bytes)| vec![(path, CandidateCategory::AppDir, Some(bytes))])
        .unwrap_or_else(|| {
            auth_candidates()
                .into_iter()
                .map(|(path, category)| (path, category, None))
                .collect()
        });
    for (path, category, supplied) in candidates {
        let (bytes, trust) = match supplied.map_or_else(
            || read_owned_regular_checked(&path),
            |bytes| Ok((bytes, ModeTrust::Trusted)),
        ) {
            Ok(v) => v,
            Err(rejection) => {
                record_candidate_read(category, Some(rejection), None);
                continue;
            }
        };
        if !trust.content_trusted() {
            // **Write-widened, not merely readable-widened.** Another uid on the shared
            // `/media/developer` namespace could have rewritten these bytes, so a stored `usage:
            // true` would not be this person's decision and a token in here would not provably be
            // theirs — see `ModeTrust`'s doc. The mode is already repaired to 0600 by
            // `read_owned_regular_trusted` above; the CONTENT is never parsed, the file stops
            // being at this name (not merely tolerated, and not silently reused next launch), and
            // this candidate is skipped exactly like a missing one — the loop below either finds
            // an untouched candidate or this install falls through to `Missing`, i.e. the sign-in
            // screen.
            //
            // **The bytes are MOVED ASIDE rather than destroyed** (maintainer decision,
            // 2026-09-10) — `quarantine_untrusted`, which falls back to the outright delete this
            // branch always did when the move cannot be made, or when the mode repair itself did
            // not take (`mode_is_owner_only`, review finding 2026-09-11: a token-bearing file must
            // never persist at 0666 under any name). Nothing about the rule above changes: still
            // never parsed, still gone from the name the next launch reads.
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            remove_temp_siblings(&path);
            if quarantine_untrusted(&path, trust.mode_is_owner_only()) {
                crate::log(&format!(
                    "session: {name} was writable by others — content is untrusted, moved aside to {name}.untrusted"
                ));
            } else {
                crate::log(&format!(
                    "session: {name} was writable by others — content is untrusted, removing"
                ));
            }
            queue_report_for_candidate(
                crate::telemetry::storage::StorageErrorContext {
                    stage: crate::telemetry::storage::StorageStage::UntrustedMode,
                    service_error_code: None,
                    class: storage_class(),
                    refused_marker: has_refused_marker(),
                    key_outcome: crate::keymanager::last_key_outcome(),
                    registered_with_app_id: crate::keymanager::registered_with_app_id(),
                    registered_with_name: crate::keymanager::registered_with_name(),
                    // The bytes were never parsed — deliberately, since another uid could have
                    // written them — so no owner was read out of them and none is claimed.
                    sealed_identity: None,
                },
                category,
            );
            record_candidate_read(category, Some(ReadRejection::UntrustedMode), None);
            continue;
        }
        if !canonical_present {
            *LAST_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((path.clone(), bytes.clone()));
        }
        if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
            if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                // The loop always returns from inside this block (opened, corrupt or locked) —
                // never `continue`s past it — so recording the candidate as secure here, before
                // any of those verdicts, covers every one of them.
                record_candidate_read(category, None, Some("secure"));
                let (plain, refusal) = crate::keymanager::open_checked(&envelope.sealed);
                let Some(plain) = plain else {
                    crate::log("session: secure file is present but its device key is unavailable");
                    // Write the CROSS-LAUNCH marker on any failure (bar the identity one below) to open an envelope this install
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
                    // **Except for the one stage that is not a verdict on the envelope.** The
                    // file names the LS2 identity that sealed it and this launch could not
                    // register as that identity — the key was never asked about, so recording a
                    // cross-launch refusal here would downgrade an install over a bus name it may
                    // well be granted on the next launch. Keep the envelope, report the stage,
                    // write nothing.
                    if let Some(refusal) = refusal {
                        if refusal.stage
                            == crate::telemetry::storage::StorageStage::IdentityUnavailable
                        {
                            // The file names an LS2 identity this launch could not obtain — the
                            // key was never asked about, so this is transient in the same sense
                            // `NoReply`/`Unreachable` below are, but its own contract (see
                            // `StorageStage::IdentityUnavailable`'s doc) is stricter still: it
                            // must NEVER arm the cross-launch refused marker, even once the
                            // bounded counter below runs out — a set whose bus never grants this
                            // launch's identity (the webOS 4.x anonymous-forever case) would
                            // otherwise lose a perfectly good envelope for good over a name, not a
                            // key. So it shares the same bounded "keep the envelope, report
                            // `LOCKED_UNAVAILABLE`" path as a silent service, and simply never
                            // falls through to `write_refused_marker` below.
                            //
                            // This deliberately does NOT go through `note_service_unavailable` —
                            // that counter is shared with the silent-key-service case below, and
                            // borrowing from one shared budget let two identity-unavailable
                            // launches spend down the same allowance a later silent-service
                            // launch was counting on, escalating on the THIRD launch regardless
                            // of which stage contributed the count (review finding, 2026-09-10).
                            // An identity-unavailable read always reports `LOCKED_UNAVAILABLE`
                            // and never falls through to `write_refused_marker` /
                            // `LOCKED_RECOVERABLE`, however many launches in a row it recurs —
                            // the whole point of this branch is that a bus name this launch could
                            // not obtain must never cost a sealed envelope its encryption at rest.
                            crate::log(
                                "session: the secure file names an LS2 identity this launch could not obtain; keeping it unopened",
                            );
                            return locked(
                                LOCKED_UNAVAILABLE,
                                &path,
                                Some(refusal),
                                Some(envelope.sealed.identity),
                                category,
                            );
                        } else {
                            // **A service that did not ANSWER is not a service that refused.** The
                            // paragraph above is the case where the open reached a key manager and it
                            // said no; a timeout, a failed registration or the hub's own "service does
                            // not exist" reached nothing at all, and arming the cross-launch marker on
                            // that evidence permanently downgrades a healthy television over one
                            // stalled boot. Those keep the envelope and report `LOCKED_UNAVAILABLE`
                            // instead — bounded, so a service that is silent for good still settles.
                            // See `open_failure_is_transient` / `note_service_unavailable`. An install
                            // ALREADY carrying the marker is past that question and takes the old path.
                            if open_failure_is_transient(refusal) && !has_refused_marker() {
                                if let Some(state) = note_service_unavailable(
                                    &path,
                                    refusal,
                                    Some(envelope.sealed.identity),
                                    category,
                                ) {
                                    return state;
                                }
                            }
                            // Same reasoning `locked()` carries for its own report: this read
                            // never called `generateKey`, so there is no per-envelope seal-time
                            // outcome to persist here — the live value is whatever this launch's
                            // own `seal` (if any) last saw, exactly like the report `locked()`
                            // queues right below.
                            write_refused_marker(
                                refusal.stage,
                                crate::keymanager::last_key_outcome(),
                            );
                        }
                    }
                    return locked(
                        LOCKED_RECOVERABLE,
                        &path,
                        refusal,
                        Some(envelope.sealed.identity),
                        category,
                    );
                };
                // **The key service ANSWERED**, which is the one fact the unanswered-launch
                // counter is counting the absence of — so the run ends here, before anything is
                // decided about the bytes it handed back. Below this line the read can still end
                // `LOCKED_CORRUPT` (a decrypt that produced something which is not a `Session`),
                // and that is a verdict on this build's own plaintext format, not on the service.
                // Clearing only in the healthy arm left such a launch carrying a stale count into
                // a later genuine run of silent launches (review finding, 2026-09-10). Idempotent,
                // and a no-op on the overwhelming majority of reads, which never had a counter.
                clear_unavailable_marker();
                return match serde_json::from_slice(&plain) {
                    Ok(session) => {
                        // Issue #76 review (blocker): an envelope THIS process just reopened was
                        // sealed by an EARLIER launch (this one never called `keymanager::seal`
                        // to produce it) — that is precisely the cross-launch proof Stage B1's
                        // probe exists to manufacture, so there is no reason to make an already-
                        // healthy install (an upgrade from 0.6.1/0.6.2, or any install whose
                        // probe cycle hasn't happened to run yet) wait for one. Without this, an
                        // install that already holds a working secure envelope but has never
                        // earned the marker is an absorbing state: `save_locked` never seals
                        // (unproven) and never plants a promotable probe either (see the
                        // `has_secure_locked` guard below), so it can never become proven at all.
                        write_proven_marker(envelope.sealed.identity);
                        not_locked(ReadState::Ready {
                            session,
                            plaintext: false,
                        })
                    }
                    // Decrypted fine but the plaintext is not a session — a real corruption of
                    // THIS BUILD'S OWN format, not a keymanager3 round-trip refusal (so it does
                    // not get `LOCKED_RECOVERABLE`'s rewrite) and not an unrecognized foreign
                    // envelope either (so a fresh sign-in MAY still recover it — see
                    // `LOCKED_CORRUPT`'s own doc, issue #76 review blocker).
                    Err(_) => locked(
                        LOCKED_CORRUPT,
                        &path,
                        None,
                        Some(envelope.sealed.identity),
                        category,
                    ),
                };
            }
        }
        if identifies_secure_envelope(&bytes) {
            crate::log("session: unsupported or damaged secure envelope is locked");
            record_candidate_read(category, None, Some("secure"));
            // A shape this build cannot parse as its own envelope: there is no recorded owner
            // to report, which is exactly what `None` says.
            return locked(LOCKED_UNRECOVERABLE, &path, None, None, category);
        }
        if let Ok(session) = serde_json::from_slice(&bytes) {
            record_candidate_read(category, None, Some("plaintext"));
            return not_locked(ReadState::Ready {
                session,
                plaintext: true,
            });
        }
        // **The one exit that used to record nothing** (review finding, 2026-09-11). Bytes that
        // read whole, are owned, are trusted and then match none of the three shapes above fell off
        // the bottom of this loop silently — so a zero-byte or truncated `auth.json` (see
        // `write_atomic`'s doc for the historical `O_TRUNC` write that made one) vanished from
        // `candidate_reads_wire()` entirely and read, downstream, exactly like a candidate that was
        // never there. Recording it is the whole point of this lane; `candidate_reads_wire`'s and
        // `ui::login::refresh_storage_readout`'s `(None, None) => "unknown"` arms are unreachable
        // again, this time because every examined candidate really is recorded.
        record_candidate_read(category, Some(ReadRejection::Unparsable), None);
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
    match persistence::load() {
        persistence::CanonicalRead::Data { ref payload, .. } => {
            return identifies_secure_envelope(payload.as_bytes());
        }
        persistence::CanonicalRead::Blocked(_) => return true,
        persistence::CanonicalRead::Cleared { .. } => return false,
        persistence::CanonicalRead::Missing => {}
    }
    auth_paths()
        .iter()
        .any(|path| read_owned_regular(path).is_some_and(|b| identifies_secure_envelope(&b)))
}

/// Did the priority scan select a trusted plaintext Session? This is narrower than
/// `LOCKED_STATE == NOT_LOCKED`: it proves a fresh reauthentication is replacing the file this
/// launch actually read, while a recognized envelope merely survives at a lower migration tier.
fn selected_candidate_is_plaintext() -> bool {
    match persistence::load() {
        persistence::CanonicalRead::Data { ref payload, .. } => {
            return !identifies_secure_envelope(payload.as_bytes())
                && serde_json::from_str::<Session>(payload).is_ok();
        }
        persistence::CanonicalRead::Blocked(_) | persistence::CanonicalRead::Cleared { .. } => {
            return false
        }
        persistence::CanonicalRead::Missing => {}
    }
    for path in auth_paths() {
        let Some((bytes, trust)) = read_owned_regular_trusted(&path) else {
            continue;
        };
        if !trust.content_trusted() {
            continue;
        }
        return !identifies_secure_envelope(&bytes)
            && serde_json::from_slice::<Session>(&bytes).is_ok();
    }
    false
}

/// **Are these bytes a secure envelope this build cannot read** — the shape [`read_locked`] grades
/// [`LOCKED_UNRECOVERABLE`]. Asked of BYTES rather than of a path because two different callers
/// need the same verdict about two different files: [`has_unrecognized_secure_envelope`] asks it of
/// the candidate this install would read, and [`sweep_other_candidates`] asks it of every candidate
/// it is about to delete.
fn is_unrecognized_secure_envelope(bytes: &[u8]) -> bool {
    identifies_secure_envelope(bytes)
        && !serde_json::from_slice::<SecureEnvelope>(bytes)
            .is_ok_and(|e| e.format == SECURE_FORMAT && e.version == 1)
}

/// **Is the file this install would actually READ a secure envelope this build does not
/// recognize** — [`read_locked`]'s [`LOCKED_UNRECOVERABLE`] verdict, asked of the disk instead of
/// of [`LOCKED_STATE`].
///
/// It exists because `LOCKED_STATE` only answers for a launch whose own `load` has already run: a
/// `save` that happens first (or in a process that never loaded) sees the default and would have
/// no idea the file is foreign. Same first-candidate rule as `read_locked` — the first readable,
/// owned and TRUSTED candidate is the one that decides, and a stale file at a LOWER-priority path
/// shadows nothing and must not block a healthy install's save. What protects THAT file is
/// [`sweep_other_candidates`], not this: the two halves of one rule, and this function alone was
/// never enough to keep it (review finding, 2026-09-11).
///
/// **A write-widened candidate decides nothing and the scan moves past it** (the same review).
/// This used to read through the trust-blind [`read_owned_regular`], so bytes any uid in the
/// shared `/media/developer` namespace could have written were allowed to answer for the install —
/// in either direction. A planted plaintext file answers "nothing foreign here" and unblocks a
/// save whose sweep then destroys the real envelope below it; a planted `version:99` file answers
/// the other way and freezes every save this install will ever make. `read_locked` is what
/// quarantines such a file; the only thing this scan owes it is not to believe it.
fn has_unrecognized_secure_envelope() -> bool {
    match persistence::load() {
        persistence::CanonicalRead::Data { ref payload, .. } => {
            return is_unrecognized_secure_envelope(payload.as_bytes());
        }
        // A present but unreadable canonical record is terminal authority. It must not be
        // replaced by a legacy candidate or a routine save; clear() is the explicit escape.
        persistence::CanonicalRead::Blocked(_) => return true,
        persistence::CanonicalRead::Cleared { .. } => return false,
        persistence::CanonicalRead::Missing => {}
    }
    for path in auth_paths() {
        let Some((bytes, trust)) = read_owned_regular_trusted(&path) else {
            continue;
        };
        if !trust.content_trusted() {
            continue;
        }
        return is_unrecognized_secure_envelope(&bytes);
    }
    false
}

/// **Every candidate but `winner`, swept clean after a write landed** — the shared tail of the two
/// writes that replace the whole file (a successful seal, and [`write_plaintext_recovery`]). A
/// copy left behind at a lower-priority jail path is a credential another uid can read, and — for
/// a secure file — one the next boot's [`read_locked`] would find *first*, shadowing the file this
/// save just wrote.
///
/// **One candidate is exempt: a secure envelope this build does not recognize** (review finding,
/// 2026-09-11). [`has_unrecognized_secure_envelope`] deliberately lets such a file sit at a
/// lower-priority path without blocking a healthy install's saves — and until this exemption
/// existed, the very next save then DELETED it. Both halves are the same rule as
/// `a_foreign_envelope_is_never_overwritten_by_a_proven_installs_seal`: a file written by a newer
/// build is unreadable here and perfectly readable again after the upgrade, unless this launch
/// destroyed it in between. The exemption is granted only on TRUSTED bytes, for
/// `has_unrecognized_secure_envelope`'s reason: a world-writable file claiming to be a foreign
/// envelope is a peer's claim, not a build's, and it is swept like any other stale copy.
///
/// The atomic-write siblings go either way — those are OUR OWN aborted writes, plaintext or
/// envelope, and no reader of this module can tell one apart from a live file it should keep.
#[cfg(test)]
static INJECT_CLEANUP_REMOVE_FAILURE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
pub(crate) fn fail_next_cleanup_remove_for_test() {
    INJECT_CLEANUP_REMOVE_FAILURE.store(true, std::sync::atomic::Ordering::Release);
}

fn remove_cleanup_file(path: &std::path::Path, parents: &mut std::collections::BTreeSet<std::path::PathBuf>) -> bool {
    #[cfg(test)]
    if INJECT_CLEANUP_REMOVE_FAILURE.swap(false, std::sync::atomic::Ordering::AcqRel) {
        crate::log("session: injected cleanup removal failure");
        return false;
    }
    match crate::storage::remove_file_or_prove_absent(path) {
        Ok(crate::storage::RemoveDisposition::Removed) => {
            if let Some(parent) = path.parent() {
                parents.insert(parent.to_path_buf());
            }
            true
        }
        Ok(crate::storage::RemoveDisposition::Absent) => true,
        Err(error) => {
            crate::log(&format!("session: cleanup could not remove {}: {error}", path.file_name().unwrap_or_default().to_string_lossy()));
            false
        }
    }
}

fn sync_cleanup_parents(parents: std::collections::BTreeSet<std::path::PathBuf>) -> bool {
    let mut complete = true;
    for parent in parents {
        if std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .is_err()
        {
            complete = false;
        }
    }
    complete
}

fn sweep_other_candidates(winner: &std::path::Path) {
    let mut parents = std::collections::BTreeSet::new();
    let protected = UNCERTAIN_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    for stale in auth_paths().into_iter().filter(|p| p != winner) {
        if protected.as_ref() == Some(&stale) {
            continue;
        }
        remove_temp_siblings(&stale);
        if read_owned_regular_trusted(&stale).is_some_and(|(bytes, trust)| {
            trust.content_trusted() && is_unrecognized_secure_envelope(&bytes)
        }) {
            log_foreign_envelope_kept_once();
            continue;
        }
        let _ = remove_cleanup_file(&stale, &mut parents);
    }
    let _ = sync_cleanup_parents(parents);
}

/// A successful lower-tier rename is not enough: a readable old higher-tier
/// file can survive a failed sweep when its directory became read-only.
/// Check this before publishing the new session as restart-persistent. This
/// deliberately needs no key-service call and never treats unknown bytes as
/// an equivalent session. It cannot detect a later cross-app replay.
fn earlier_candidates_allow_winner(winner: &std::path::Path, bytes: &[u8]) -> bool {
    if winner == persistence::path() {
        return true;
    }
    for path in auth_paths() {
        if path == winner {
            return true;
        }
        if read_owned_regular_trusted(&path)
            .is_some_and(|(old, trust)| trust.content_trusted() && old != bytes)
        {
            return false;
        }
    }
    false
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
/// Production cold boot calls this through [`start_load`] on the persistence worker; runtime
/// readers use [`snapshot`]. The compatibility entry remains for migration/security tests. While
/// an accepted cold job owns [`COLD_LOAD_ACTIVE`], a second call refuses to cross the boundary and
/// returns an empty snapshot instead of paying keymanager/disk twice.
pub fn load() -> Session {
    if let Some(s) = cached() {
        return s;
    }
    if COLD_LOAD_ACTIVE.load(std::sync::atomic::Ordering::Acquire) {
        return Session::default();
    }
    load_cold_owned()
}

/// The one blocking cold-load body. Production reaches it only through [`start_load`]'s worker;
/// the public compatibility wrapper above remains for host migration tests and refuses to become
/// a second disk/keymanager reader while that worker owns the cold boundary.
fn load_cold_owned() -> Session {
    let (s, pending) = {
        let _io = io();
        let s = if let Some(s) = cached() {
            s
        } else {
            // Stage B1 (issue #76): resolve a PRIOR launch's cross-launch probe before anything
            // else this cold path does — see `check_probe`'s doc. It touches neither `CACHE` nor
            // `LOCKED_STATE`, so ordering against `read_locked` below only matters for the very
            // rare install that is simultaneously locked AND has an outstanding probe; either order
            // reaches the same two markers.
            check_probe();
            // Probe reports above belong to a different file. Only reports appended after this
            // boundary may describe the session candidate read captured below.
            let cold_report_start = PENDING_REPORTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len();
            let read = read_locked();
            capture_cold_session_facts(&read);
            // Issue #76 field report gap: one line naming every candidate this launch examined
            // and why each was declined or accepted — never a path, only the fixed category/
            // rejection words `candidate_reads_wire` builds. See `CandidateRead`'s doc.
            let reads = candidate_reads_wire();
            capture_cold_storage_diagnostic(
                cold_report_start,
                reads.clone(),
                cold_session_facts().unwrap_or_default(),
            );
            crate::log(&format!("session: read candidates={reads}"));
            // …and the same summary onto the report side, BEFORE anything below can queue a
            // storage-error report. `read_locked`'s own `UntrustedMode` report and the locked/
            // unavailable ones `locked` queues are bound to this completed summary here, while IO
            // is still held. They are only sent by `send_pending_reports` below, after IO is
            // released, but another thread's later save can no longer steal this read's evidence.
            let persisted = !matches!(read, ReadState::Missing | ReadState::Cleared);
            let locked = matches!(read, ReadState::Locked { .. });
            let terminal = matches!(read, ReadState::Cleared | ReadState::CanonicalBlocked);
            let plaintext = matches!(
                read,
                ReadState::Ready {
                    plaintext: true,
                    ..
                }
            );
            let mut s = match read {
                ReadState::Ready { session, .. } => session,
                ReadState::Missing
                | ReadState::Cleared
                | ReadState::CanonicalBlocked
                | ReadState::Locked { .. } => Session::default(),
            };
            seed_fresh_quality(&mut s, persisted, crate::route::auto_quality_ready());
            if s.client_id.is_empty() && !terminal {
                s.client_id = new_client_id();
                if !locked {
                    let _ = save_locked(&s, SaveAuthority::Routine);
                }
            } else if plaintext {
                // Offer every plaintext session to the Key Manager immediately. This also moves a
                // parsable legacy-path file to the preferred location; without a usable service it
                // stays an atomic mode-0600 plaintext fallback.
                let _ = save_locked(&s, SaveAuthority::Routine);
            }
            publish_identities(&s);
            // Whatever this run ends up believing the session is — even the ephemeral default
            // that comes from a Locked or Missing read — becomes the in-process truth every later
            // `peek` serves.
            publish_cache(s.clone());
            // Include reports from the read itself and from its migration/mint save, then detach
            // the batch before releasing IO so no other entry point can drain or append to it.
            attach_candidate_reads_to_pending(&reads);
            s
        };
        (s, take_pending_reports())
    };
    // Issue #76: a read landing Locked, or a save's own seal failure, may have queued a handled
    // report above — sent only now that `IO` has been released (see `PENDING_REPORTS`'s doc). The
    // cached fast path above never itself queues one (it does no `read_locked`/`save_locked`), but
    // still passes through here rather than a bare early `return`, so it cannot silently start
    // skipping this the moment that path ever changes.
    send_pending_reports(pending);
    s
}

pub(crate) struct LoadReceipt {
    ticket: Option<crate::storage_worker::TypedTicket<Session>>,
    resolved: Option<LoadPoll>,
}

#[derive(Clone)]
pub(crate) enum LoadPoll {
    Pending,
    Ready(Session),
    Failed,
}

impl LoadReceipt {
    pub(crate) fn poll(&mut self) -> LoadPoll {
        if let Some(result) = self.resolved.clone() {
            return result;
        }
        let Some(ticket) = self.ticket.as_ref() else {
            return LoadPoll::Failed;
        };
        match ticket.try_recv() {
            Ok(session) => {
                let result = LoadPoll::Ready(session);
                self.ticket = None;
                self.resolved = Some(result.clone());
                result
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => LoadPoll::Pending,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.ticket = None;
                self.resolved = Some(LoadPoll::Failed);
                LoadPoll::Failed
            }
        }
    }
}

/// Start the blocking cold Session read on the one shared persistence worker. The worker publishes
/// [`CACHE`] before the receipt becomes Ready; callers retry an explicit admission failure or a
/// disconnected receipt and never fall back to inline disk/keymanager work.
pub(crate) fn start_load(
) -> Result<LoadReceipt, crate::storage_worker::SubmitError> {
    let job = prepare_cold_load()?;
    crate::storage_worker::submit(job).map(|ticket| LoadReceipt {
        ticket: Some(ticket),
        resolved: None,
    })
}

fn prepare_cold_load(
) -> Result<Box<dyn FnOnce() -> Session + Send>, crate::storage_worker::SubmitError> {
    if COLD_LOAD_ACTIVE
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Acquire,
        )
        .is_err()
    {
        return Err(crate::storage_worker::SubmitError::Full);
    }
    let guard = ColdLoadGuard;
    Ok(Box::new(move || {
        let _guard = guard;
        load_cold_owned()
    }))
}

/// Start an explicitly authorized QR sign-in after a cleared tombstone. This creates only an
/// in-memory client id; credentials and the canonical cleared record remain untouched until the
/// successful PIN flow reaches `save_after_reauthentication`.
pub fn begin_new_signin() -> Session {
    let mut session = Session::default();
    session.client_id = new_client_id();
    publish_cache(session.clone());
    session
}

/// **Is a sealed sign-in sitting on this install that THIS LAUNCH could not read, because the key
/// service never answered?** — [`LOCKED_UNAVAILABLE`], the state the sign-in screen draws its own
/// read-out for.
///
/// A bare atomic load, deliberately: `ui::login` asks on every frame it draws, and
/// [`storage_class`] — the same verdict in the telemetry vocabulary — opens up to five candidate
/// marker paths to answer, which is a per-frame syscall storm on the SDL main thread. This is the
/// question a SCREEN asks; `storage_class` stays the one a REPORT asks.
pub(crate) fn secure_unavailable() -> bool {
    LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) == LOCKED_UNAVAILABLE
}

/// **Ask the key service again, now** — the sign-in screen's *Try again*, and the only way back
/// into a sealed session inside a launch that booted without one.
///
/// Re-runs [`read_locked`] against the same file, deliberately BYPASSING [`CACHE`]: `load` publishes
/// the ephemeral default session it fell back to, so the cached fast path would answer "still
/// nothing" without ever asking. A successful open publishes the real session, clears
/// [`LOCKED_STATE`] and writes the proven marker exactly as a healthy boot's own read does, so
/// `auth::resume_secure_session` can then enter the app the way the boot gate would have.
///
/// Returns whether the envelope opened. It does NOT report a `false` in any second vocabulary:
/// `secure_unavailable()` still answers `true` when the service is simply still silent, and
/// `false` once this open reached a real refusal — at which point the screen falls back to the
/// plain QR flow, which is the honest offer for a sign-in that genuinely cannot be recovered.
pub(crate) fn retry_secure_open() -> bool {
    let (opened, pending) = {
        let _io = io();
        let opened = match read_locked() {
            ReadState::Ready { session, .. } => {
                publish_identities(&session);
                publish_cache(session);
                true
            }
            ReadState::Missing
            | ReadState::Cleared
            | ReadState::CanonicalBlocked
            | ReadState::Locked { .. } => false,
        };
        (opened, take_pending_reports())
    };
    // The one line that says what the press achieved. `last_refusal` is the process-global
    // standing fact (see `keymanager::open_checked`'s doc) — right for a log line, which is why
    // nothing is DECIDED on it here.
    let outcome = if opened {
        "opened".to_string()
    } else {
        crate::keymanager::last_refusal()
            .map_or_else(|| "locked".to_string(), |r| r.stage.code().to_string())
    };
    crate::log(&format!("keymanager: retry -> {outcome}"));
    send_pending_reports(pending);
    opened
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
/// **Also refuses on ANY non-`NOT_LOCKED` read** — `LOCKED_RECOVERABLE`/`LOCKED_UNRECOVERABLE`
/// and equally `LOCKED_CORRUPT`/`LOCKED_UNAVAILABLE` (the guard below tests the whole
/// `LOCKED_STATE`, not an enumerated subset — read it that way rather than re-enumerating it here
/// every time a state is added). A `load` on a
/// genuinely locked boot still mints an EPHEMERAL client id (never persisted for exactly this
/// reason) so the empty-id test above no longer catches it — an unrelated writer with no
/// credentials of its own (the home-pin, recents or quality-rung `update`s) must not be the thing
/// that turns a recognized-but-unopenable secure envelope into a credential-free plaintext file;
/// only a fresh SIGN-IN, through [`save_locked`]'s own recoverable branch, may do that.
pub fn update(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    let (wrote, pending) = {
        let _io = io();
        let cur = peek_locked();
        let wrote = if cur.client_id.is_empty()
            || LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed) != NOT_LOCKED
        {
            false
        } else {
            match edit(&cur) {
                // Issue #76 review (should-fix): `save_locked` can now genuinely refuse (the
                // unproven-and-secure dead end above, or every candidate path refusing the
                // write) — propagate its real verdict instead of claiming every edit landed. When
                // it refuses, `save_locked` also never calls `publish_cache`, so — the same
                // reasoning `auth::take_ready` already applies to `session::save` — publish the
                // edit for THIS run anyway: a roster refresh or a pin that is real in memory but
                // unpersisted must not also read back as though it never happened.
                Some(next) => {
                    let wrote = save_locked(&next, SaveAuthority::Routine).persisted();
                    publish_unpersisted(next);
                    wrote
                }
                None => false,
            }
        };
        (wrote, take_pending_reports())
    };
    send_pending_reports(pending);
    wrote
}

/// Admit a short in-memory Session edit to the shared persistence FIFO. The accepted snapshot is
/// visible through [`peek`] immediately; callers poll the per-operation receipt for durability.
pub(crate) fn update_with_receipt(
    edit: impl FnOnce(&Session) -> Option<Session>,
) -> Result<Option<async_persistence::Receipt>, async_persistence::AdmissionError> {
    async_persistence::update(edit)
}

/// Admit an ordinary in-process Session edit and retain one bounded receipt for diagnostics.
/// Returns admission/no-change, never a durability claim.
pub(crate) fn update_ordinary(edit: impl FnOnce(&Session) -> Option<Session>) -> bool {
    async_persistence::update_ordinary(edit)
}

pub(crate) fn poll_ordinary_persistence() -> async_persistence::Status {
    async_persistence::poll_ordinary()
}

pub(crate) fn ordinary_persistence_revision() -> u64 {
    async_persistence::ordinary_revision()
}

pub(crate) fn retry_ordinary_persistence(expected_revision: u64) -> bool {
    async_persistence::retry_ordinary(expected_revision)
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
/// preserved through a transient failure to protect it again — **except through
/// [`save_after_reauthentication`], over a file THIS process's own read has already failed to
/// open**. That
/// is [`LOCKED_RECOVERABLE`] (keymanager3 sealed it once but cannot open it now, on this launch,
/// this firmware — issue #76), [`LOCKED_CORRUPT`] (it opened, and the plaintext is not a session)
/// and, since 0.6.4, [`LOCKED_UNAVAILABLE`] where the service is *still* unanswerable at the
/// moment of the save. In all three: a sign-in nobody could ever read back is worse than one
/// written down in plain sight, and there is nothing the locked ciphertext holds that the fresh
/// sign-in does not already re-supply. The first two do not even ask the key manager to try again
/// — a backend proven unable to open what THIS run found on disk is not asked to seal a new
/// envelope that could turn out just as unreadable next launch. The third one does: an unanswered
/// service has proven nothing about the key, so the seal is attempted first and the recovery is
/// what happens when even that comes back silent. See [`save_locked`]'s branches and
/// [`seal_permitted`]'s doc for why those two orders differ.
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
///
/// **Returns WHAT it did, not merely whether it worked** — [`PersistOutcome`], which
/// [`PersistOutcome::persisted`] narrows back to the bool this used to be (Stage B2, issue #76
/// field report case 6). Not-persisted covers a genuine total failure (every candidate path
/// refused) and several deliberate no-ops that preserve an existing secure or foreign file, and
/// those read identically in a log or a report while meaning entirely different things about the
/// install — which is how the 0.6.4 defect stayed invisible: "not persisted" was indistinguishable
/// from "nothing needed persisting". A caller that cannot afford the run to look signed out when
/// it is not (`auth::take_ready`) reads `.persisted()`; a caller that wants to SAY what happened
/// reads [`PersistOutcome::wire`].
pub fn save(s: &Session) -> PersistOutcome {
    save_with_authority(s, SaveAuthority::Routine)
}

/// Persist credentials produced by a successful Plex PIN authorization in this process.
///
/// This is intentionally a separate, conspicuous door: only that user action may replace a
/// recognized secure envelope which this launch could not open. A non-empty stored
/// [`Session::account_token`] is not evidence of reauthentication; profile switching and stored-
/// session resume carry the same token through routine saves.
pub(crate) fn save_after_reauthentication(s: &Session) -> PersistOutcome {
    save_with_authority(s, SaveAuthority::FreshReauthentication)
}

pub(crate) fn save_with_receipt(
    s: Session,
) -> Result<async_persistence::Receipt, async_persistence::AdmissionError> {
    async_persistence::replace(s, SaveAuthority::Routine)
}

pub(crate) fn save_after_reauthentication_with_receipt(
    s: Session,
) -> Result<async_persistence::Receipt, async_persistence::AdmissionError> {
    async_persistence::replace(s, SaveAuthority::FreshReauthentication)
}

pub(crate) fn latest_persistence_status() -> async_persistence::Status {
    async_persistence::status()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SaveAuthority {
    Routine,
    FreshReauthentication,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreshSaveReadbackResult {
    Match,
    Mismatch,
    Rejected,
}

impl FreshSaveReadbackResult {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FreshSaveReadback {
    pub winner: CandidateCategory,
    pub result: FreshSaveReadbackResult,
}

struct LastSessionWrite {
    path: std::path::PathBuf,
    winner: CandidateCategory,
    bytes: Vec<u8>,
}

static LAST_SESSION_WRITE: Mutex<Option<LastSessionWrite>> = Mutex::new(None);
static LAST_FRESH_READBACK: Mutex<Option<FreshSaveReadback>> = Mutex::new(None);

pub(crate) fn last_fresh_save_readback() -> Option<FreshSaveReadback> {
    *LAST_FRESH_READBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

fn verify_fresh_write_readback() {
    let written = LAST_SESSION_WRITE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take();
    let Some(written) = written else {
        return;
    };
    let result = if written.path == persistence::path() {
        match persistence::load() {
            persistence::CanonicalRead::Data { payload, .. }
                if payload.as_bytes() == written.bytes.as_slice() =>
            {
                FreshSaveReadbackResult::Match
            }
            persistence::CanonicalRead::Data { .. } => FreshSaveReadbackResult::Mismatch,
            persistence::CanonicalRead::Missing
            | persistence::CanonicalRead::Cleared { .. }
            | persistence::CanonicalRead::Blocked(_) => FreshSaveReadbackResult::Rejected,
        }
    } else {
        match read_owned_regular_checked(&written.path) {
            Ok((_, trust)) if !trust.content_trusted() => FreshSaveReadbackResult::Rejected,
            Ok((bytes, _)) if bytes == written.bytes => FreshSaveReadbackResult::Match,
            Ok(_) => FreshSaveReadbackResult::Mismatch,
            Err(_) => FreshSaveReadbackResult::Rejected,
        }
    };
    crate::log(&format!(
        "session: fresh save readback winner={} result={}",
        written.winner.wire(),
        result.wire()
    ));
    *LAST_FRESH_READBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(FreshSaveReadback {
        winner: written.winner,
        result,
    });
}

fn note_session_write(path: &std::path::Path, winner: CandidateCategory, bytes: &[u8]) {
    *LAST_SESSION_WRITE.lock().unwrap_or_else(|e| e.into_inner()) = Some(LastSessionWrite {
        path: path.to_path_buf(),
        winner,
        bytes: bytes.to_vec(),
    });
}

fn save_with_authority(s: &Session, authority: SaveAuthority) -> PersistOutcome {
    let outcome = save_with_authority_disk(s, authority);
    if outcome.persisted() {
        publish_cache(s.clone());
    }
    outcome
}

/// Blocking persistence half shared by the synchronous boot path and the asynchronous
/// coordinator. It deliberately never publishes [`CACHE`]: an asynchronous completion may be
/// older than the snapshot already admitted on the caller thread.
fn save_with_authority_disk(s: &Session, authority: SaveAuthority) -> PersistOutcome {
    let (outcome, pending) = {
        let _io = io();
        // Detach anything older at the boundary so it cannot be labeled as this save's evidence.
        let mut pending_before = take_pending_reports();
        if authority == SaveAuthority::FreshReauthentication {
            *LAST_FRESH_READBACK
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            FRESH_WRITE_ATTEMPTS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            FRESH_SAVE_ERRORS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
        let outcome = save_locked(s, authority);
        if authority == SaveAuthority::FreshReauthentication && outcome.persisted() {
            verify_fresh_write_readback();
        }
        // Success is consumed by `verify_fresh_write_readback`. Either way,
        // serialized credentials never outlive this save in the private scratch slot.
        *LAST_SESSION_WRITE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let pending_from_save = take_pending_reports();
        if authority == SaveAuthority::FreshReauthentication {
            *FRESH_SAVE_ERRORS.lock().unwrap_or_else(|e| e.into_inner()) = pending_from_save
                .iter()
                .take(8)
                .map(|report| FreshSaveError {
                    candidate: report.candidate,
                    context: report.context,
                })
                .collect();
        }
        pending_before.extend(pending_from_save);
        (outcome, pending_before)
    };
    send_pending_reports(pending);
    outcome
}

/// **What a save actually did to the file.** Every branch of [`save_locked`] ends at exactly one
/// of these, and one line of the event log says which — the only place a device log states the
/// difference between a sign-in that reached the disk and one that is alive for this run only.
///
/// The wire words ([`PersistOutcome::wire`]) are a vocabulary shared with the telemetry layer and
/// with whatever reads a log: they are part of the contract, not a debug rendering, and are not
/// renamed without renaming them there too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistOutcome {
    /// The 0600 plaintext file was written — the ordinary fallback, the recovery over a locked
    /// envelope, and every install that has not earned sealed storage yet.
    PersistedPlaintext,
    /// A fresh `keymanager::seal` envelope was written.
    PersistedSealed,
    /// Nothing was written, on purpose: a secure file this build could read is already there and
    /// this save had no business replacing it. [`PreserveReason`] says which rule kept it.
    PreservedExistingSecure(PreserveReason),
    /// Nothing was written, on purpose: the file is a secure envelope of a format or version this
    /// build does not recognize (`LOCKED_UNRECOVERABLE`), which no save may ever touch — it may be
    /// a NEWER build's envelope that reads perfectly again after the upgrade. Only [`clear`]
    /// removes it.
    BlockedUnknownEnvelope,
    /// No authoritative new session was persisted: candidate writes failed, or
    /// an older higher-priority file could not be removed and would shadow them.
    /// Nothing about the key service is claimed. Reported as `StorageStage::WriteFailed`.
    WriteFailed,
    /// The `Session` (or the envelope wrapping it) would not serialize — a bug, not a device
    /// condition, and the one outcome that says nothing about the disk at all.
    SerializationFailed,
}

/// Which rule left an existing, readable secure file in place — the reason half of
/// [`PersistOutcome::PreservedExistingSecure`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreserveReason {
    /// This install has not EARNED sealed storage (Stage B1: no prior launch's probe has reopened)
    /// and a secure file is present, so neither sealing nor a plaintext downgrade is permitted.
    /// A probe is planted for the next launch to check.
    NotProven,
    /// A prior launch recorded the cross-launch refused marker, but this save carries no fresh
    /// credentials and a secure file is present — an ordinary `update()` must never be the thing
    /// that converts a present envelope to plaintext on a stale, cross-launch fact.
    RefusedMarkerNoFreshSignIn,
    /// `keymanager::seal` failed on a save that was otherwise permitted to seal, and this save has
    /// no fresh sign-in to justify replacing the ciphertext. A transient failure must not cost an
    /// install its encryption at rest.
    SealFailed,
}

impl PersistOutcome {
    /// The bool this used to be: did anything actually reach the disk.
    pub fn persisted(self) -> bool {
        matches!(
            self,
            PersistOutcome::PersistedPlaintext | PersistOutcome::PersistedSealed
        )
    }

    /// The stable word for a log line, a report or a diagnostics row.
    pub fn wire(self) -> &'static str {
        match self {
            PersistOutcome::PersistedPlaintext => "persisted_plaintext",
            PersistOutcome::PersistedSealed => "persisted_sealed",
            PersistOutcome::PreservedExistingSecure(_) => "preserved_existing_secure",
            PersistOutcome::BlockedUnknownEnvelope => "blocked_unknown_envelope",
            PersistOutcome::WriteFailed => "write_failed",
            PersistOutcome::SerializationFailed => "serialization_failed",
        }
    }

    /// The reason word, for the one variant that carries one.
    ///
    /// `pub(crate)` since the issue #76 report lane: `auth::note_sign_in_persist` reports the
    /// outcome and its reason as two fields, and the sign-in screen's Details panel shows them as
    /// one row — neither can re-derive this from the `wire()` word, which deliberately does not
    /// carry it.
    pub(crate) fn reason_wire(self) -> Option<&'static str> {
        match self {
            PersistOutcome::PreservedExistingSecure(r) => Some(r.wire()),
            _ => None,
        }
    }
}

impl PreserveReason {
    /// The stable word for a log line, a report or a diagnostics row.
    pub fn wire(self) -> &'static str {
        match self {
            PreserveReason::NotProven => "not_proven",
            PreserveReason::RefusedMarkerNoFreshSignIn => "refused_marker_no_fresh_sign_in",
            PreserveReason::SealFailed => "seal_failed",
        }
    }
}

/// What the most recent [`save_locked`] in this process did — see [`last_persist_outcome`].
static LAST_PERSIST: Mutex<Option<PersistOutcome>> = Mutex::new(None);

/// **The most recent save's verdict, process-wide** — `None` before this process has saved at all.
///
/// A `Mutex<Option<_>>` rather than a packed atomic because the value carries a reason and is read
/// by human-paced surfaces (a diagnostics row, a report being assembled), never per frame.
pub fn last_persist_outcome() -> Option<PersistOutcome> {
    *LAST_PERSIST.lock().unwrap_or_else(|e| e.into_inner())
}

/// [`save`] with the lock already held: run the save and record what it did.
///
/// **Exactly one line per save**, whatever branch it took — `session: persist outcome=<wire>` plus
/// ` reason=<wire>` where there is one. The individual branches still log their own detail (and
/// several of them only once per process, so a repeated no-op is otherwise silent); this is the
/// line that is always there, and the one that separates "the sign-in reached the disk" from "the
/// sign-in is alive for this run only" without anybody having to know which branch is which.
fn save_locked(s: &Session, authority: SaveAuthority) -> PersistOutcome {
    *LAST_SESSION_WRITE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let outcome = persist_locked(s, authority);
    if authority == SaveAuthority::Routine {
        *LAST_SESSION_WRITE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
    *LAST_PERSIST.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
    match outcome.reason_wire() {
        Some(reason) => crate::log(&format!(
            "session: persist outcome={} reason={reason}",
            outcome.wire()
        )),
        None => crate::log(&format!("session: persist outcome={}", outcome.wire())),
    }
    outcome
}

/// [`save_locked`]'s body — every return is one [`PersistOutcome`], and the wrapper above is what
/// records and announces it.
fn persist_locked(s: &Session, authority: SaveAuthority) -> PersistOutcome {
    // Before the write, not after: a failed persist still means these names are live in THIS run,
    // and the log wants them redacted either way.
    publish_identities(s);
    let Ok(json) = serde_json::to_vec_pretty(s) else {
        return PersistOutcome::SerializationFailed;
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
    // Both halves are required. The authority proves where the save came from; the credential
    // proves it actually carries the account state which replacing the old envelope would erase.
    let saving_fresh_sign_in =
        authority == SaveAuthority::FreshReauthentication && !s.account_token.is_empty();

    // **A secure envelope this build does not recognize is never written over, by any save**
    // (review finding, 2026-09-10). Both branches below already refuse it via `has_secure_locked`
    // when the install is UNPROVEN — `a_foreign_envelope_still_never_recovers_even_unproven` is
    // that rule — but a PROVEN install fell straight through to `keymanager::seal` and replaced
    // the file with a fresh envelope of its own. Nothing here has read that file: this build never
    // even tried, because it does not know the shape. The concrete case is a downgrade from a
    // newer build, whose envelope is unreadable HERE and perfectly readable again after the
    // upgrade — unless this launch destroyed it in between. Not even a completed sign-in may:
    // the rule that "only a completed sign-in replaces the stored sign-in" governs a file this
    // build understands and has been refused by a key service, and the escape from THIS state is
    // the deliberate one, `clear()` (sign out / Delete all local data), which removes the file
    // outright. Checked from the DISK as well as from `LOCKED_STATE`, since a `save` that runs
    // before this launch's own `load` sees the state at its default.
    if locked_state == LOCKED_UNRECOVERABLE || has_unrecognized_secure_envelope() {
        log_foreign_envelope_skip_once();
        return PersistOutcome::BlockedUnknownEnvelope;
    }
    if !seal_permitted(marker_present, locked_state, saving_fresh_sign_in) {
        if locked_state == LOCKED_RECOVERABLE && saving_fresh_sign_in {
            // The more specific case: THIS launch's own read found the envelope, and knows
            // exactly which candidate to target and sweep.
            return recover_locked_session_as_plaintext(s, &json, true);
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
            return PersistOutcome::PreservedExistingSecure(
                PreserveReason::RefusedMarkerNoFreshSignIn,
            );
        }
        return write_refused_plaintext(s, &json, saving_fresh_sign_in);
    }

    // Stage B1 (issue #76): sealed storage is EARNED, never assumed. Neither `seal_permitted`
    // reason applies — this save is not blocked by a known-bad envelope — but that is not
    // permission to seal: nothing on THIS launch can prove a DIFFERENT launch will be able to read
    // it back (`keymanager::seal`'s own round trip is in-process, see its doc). Until a prior
    // launch's probe has proven that (`has_proven_marker`), every save stays on the 0600 file and
    // tries to plant a fresh probe for the NEXT launch to check.
    if !proven_for_this_launch() {
        // A secure-shaped file can already be sitting on disk here even though THIS process never
        // read it through `read_locked` (a fresh `save()` called before this launch's own `load()`,
        // or a file planted by something else entirely) — never let "not yet proven" become a
        // license to overwrite it with plaintext. Same reasoning the refused-marker branch above
        // already applies via `has_secure_locked`.
        if has_secure_locked() {
            // Issue #76 review (blocker): this branch used to be an unconditional dead end for
            // ANY unproven install that happens to have a secure file present — including one
            // THIS LAUNCH's own `read_locked` has already proved is OUR OWN format, decrypted,
            // and simply not a session (`LOCKED_CORRUPT`, the shape `seal_permitted`'s own
            // `LOCKED_RECOVERABLE` guard does not cover) — whose caller is a FRESH sign-in
            // re-supplying the exact credentials the dead ciphertext held. Mirror
            // `seal_permitted`'s own reasoning: a fresh sign-in over a file this launch has
            // independently proven both OURS and unusable may recover to plaintext — there is
            // nothing left on the ciphertext to protect. **Deliberately narrower than "any locked
            // state"**: `LOCKED_UNRECOVERABLE` (an unrecognized/foreign-version envelope this
            // build never even tried to open) must NEVER be rewritten this way — see
            // `an_unknown_secure_envelope_version_is_locked_and_never_rewritten_as_plaintext` —
            // and `LOCKED_RECOVERABLE` with a fresh sign-in never reaches here at all;
            // `seal_permitted` above already routed it to `recover_locked_session_as_plaintext`.
            // `LOCKED_UNAVAILABLE` joined this since 0.6.4, and it is the reported defect
            // (issue #76's second field report). An install upgraded from 0.6.2 carries a
            // recognized envelope and no proven marker, so a launch whose key service never
            // answers lands here on the very save that carries the fresh sign-in — and refusing it
            // meant the credentials lived for one run, the next launch found the same envelope and
            // the same silence, and the QR screen came back forever. The bounded escalation cannot
            // rescue that (an `IdentityUnavailable` open never escalates at all), and the envelope
            // being preserved is worth nothing to a person who cannot get past the sign-in screen.
            // Same reasoning as the `LOCKED_RECOVERABLE` branch above: the user has just
            // re-supplied everything the ciphertext held.
            if saving_fresh_sign_in && matches!(locked_state, LOCKED_CORRUPT | LOCKED_UNAVAILABLE) {
                return recover_locked_session_as_plaintext(s, &json, true);
            }
            if saving_fresh_sign_in
                && locked_state == NOT_LOCKED
                && selected_candidate_is_plaintext()
            {
                crate::log(
                    "session: fresh reauthentication updates selected plaintext despite another protected candidate",
                );
                return write_unproven_plaintext(s, &json, true);
            }
            crate::log(
                "session: secure storage is not yet proven on this install, but a secure file is present; leaving it untouched",
            );
            // Still worth a probe: an install that never gets a fresh sign-in over the file above
            // must not be left with no route to `has_proven_marker()` at all — a probe here is
            // what lets `check_probe` promote it on the next launch.
            plant_probe();
            return PersistOutcome::PreservedExistingSecure(PreserveReason::NotProven);
        }
        plant_probe();
        return write_unproven_plaintext(s, &json, saving_fresh_sign_in);
    }

    if let Some(sealed) = crate::keymanager::seal(&json) {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed,
        };
        let Ok(protected) = serde_json::to_vec_pretty(&envelope) else {
            return PersistOutcome::SerializationFailed;
        };
        for (winner, category) in write_candidates() {
            if write_session_candidate(&winner, category, &protected, saving_fresh_sign_in) {
                note_session_write(&winner, category, &protected);
                // A successful migration must not leave an older plaintext token file at a
                // lower-priority jail path where another uid can recover it — nor destroy the one
                // kind of file that is not ours to delete; see [`sweep_other_candidates`].
                not_locked_after_write();
                LAST_CLASS.store(CLASS_SECURE, std::sync::atomic::Ordering::Relaxed);
                return PersistOutcome::PersistedSealed;
            }
        }
        crate::log("session: key manager succeeded but the protected file could not be written");
        report_write_failed();
        return PersistOutcome::WriteFailed;
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
        // Issue #76's identity decider: this IS a seal-time report — `keymanager::seal` just ran,
        // so the live outcome is exactly the one that produced (or failed to produce) the envelope
        // this refusal is about.
        queue_report(crate::telemetry::storage::StorageErrorContext {
            stage: refusal.stage,
            service_error_code: refusal.error_code,
            class: storage_class(),
            refused_marker: has_refused_marker(),
            key_outcome: crate::keymanager::last_key_outcome(),
            registered_with_app_id: crate::keymanager::registered_with_app_id(),
            registered_with_name: crate::keymanager::registered_with_name(),
            // A seal that FAILED produced no envelope, so there is no recorded owner to report.
            // The identity this attempt registered under is this launch's own, which the two
            // bools above already publish — restating it here as a third field would make
            // `sealed_identity` mean two different things depending on the stage.
            sealed_identity: None,
        });
    }
    if has_secure_locked() {
        // …unless this launch's own read already found that envelope unanswerable and this save
        // carries a completed sign-in (0.6.4, the same rule the unproven branch above applies).
        // The seal that just failed is the second half of the evidence: the service would neither
        // open the old envelope nor produce a new one, so preserving the ciphertext costs the user
        // their sign-in at every launch and buys back nothing they have not just re-typed. A
        // service that has come back never reaches here — `keymanager::seal` succeeded above and
        // this install stayed sealed, which is why a transient failure still costs nothing.
        if saving_fresh_sign_in && locked_state == LOCKED_UNAVAILABLE {
            return recover_locked_session_as_plaintext(s, &json, true);
        }
        crate::log("session: preserving the existing secure file; refusing a plaintext downgrade");
        return PersistOutcome::PreservedExistingSecure(PreserveReason::SealFailed);
    }
    // Try each candidate; the first that accepts the write wins. A total failure is still
    // non-fatal — but it is LOGGED, because the symptom (sign in again, every boot, forever) is
    // otherwise indistinguishable from a server-side auth problem and impossible to report.
    for (path, category) in write_candidates() {
        if write_session_candidate(&path, category, &json, saving_fresh_sign_in) {
            note_session_write(&path, category, &json);
            LAST_CLASS.store(CLASS_PLAINTEXT, std::sync::atomic::Ordering::Relaxed);
            return PersistOutcome::PersistedPlaintext;
        }
    }
    crate::log(
        "session: could not persist to ANY candidate path — login will not survive a reboot",
    );
    report_write_failed();
    PersistOutcome::WriteFailed
}

/// Stage B2 (issue #76): no restart-authoritative write succeeded —
/// queued from every "could not persist to ANY candidate path" branch in this file so the failure
/// leaves a trace independent of `auth::take_ready`'s own once-per-call log line. Distinct from
/// the seal/open stages: this is not a key-service verdict, so it carries no service error code.
fn report_write_failed() {
    queue_report(crate::telemetry::storage::StorageErrorContext {
        stage: crate::telemetry::storage::StorageStage::WriteFailed,
        service_error_code: None,
        class: storage_class(),
        refused_marker: has_refused_marker(),
        key_outcome: crate::keymanager::last_key_outcome(),
        registered_with_app_id: crate::keymanager::registered_with_app_id(),
        registered_with_name: crate::keymanager::registered_with_name(),
        // No newly authoritative envelope is being claimed by this report.
        sealed_identity: None,
    });
}

/// The write-side twin of [`not_locked`]. Both callers have just REPLACED the file on disk — a
/// fresh seal, or the 0600 plaintext recovery — so whatever envelope an unanswered-launch counter
/// was accumulating against is gone, and the count goes with it for [`not_locked`]'s reason.
fn not_locked_after_write() {
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    clear_unavailable_marker();
}

/// Issue #76's recovery write: this process's own read already proved the on-disk envelope at
/// [`LOCKED_PATH`] cannot be opened — refused ([`LOCKED_RECOVERABLE`]), opened but not a session
/// ([`LOCKED_CORRUPT`]), or unanswerable for the whole of this launch, the seal attempt included
/// ([`LOCKED_UNAVAILABLE`], since 0.6.4) — and the caller carries explicit authority from a
/// successful PIN flow, so there is nothing the locked ciphertext held that this save does not
/// re-supply. Refusing it is
/// what produced the endless loop; writing the 0600 fallback in its place is what ends it.
///
/// **Reached only with fresh credentials in hand, in every one of those states.** An ordinary
/// `update()` — a roster refresh, a pinned library — never gets here (see [`update`]'s own guard
/// and [`save_locked`]'s branches), which is what keeps a firmware hiccup from converting a
/// perfectly good envelope to plaintext behind the user's back.
///
/// Targets the SAME candidate `read_locked` found the envelope at first — not merely the first
/// candidate willing to accept a write, which can be a different (lower-priority) path when the
/// locked file's own candidate is readable but not writable. Whichever path the write actually
/// lands at, every OTHER candidate is swept the same way a successful seal already does, so the
/// locked file cannot survive at a lower-priority path and keep shadowing the fresh sign-in on the
/// next boot.
fn recover_locked_session_as_plaintext(
    s: &Session,
    json: &[u8],
    record_fresh: bool,
) -> PersistOutcome {
    let previously_locked_at = LOCKED_PATH
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    match write_plaintext_recovery(s, json, previously_locked_at.as_deref(), record_fresh) {
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
            PersistOutcome::PersistedPlaintext
        }
        None => {
            crate::log(
                "session: could not persist to ANY candidate path — login will not survive a reboot",
            );
            report_write_failed();
            PersistOutcome::WriteFailed
        }
    }
}

/// Stage B1's own counterpart: this install has not yet EARNED sealed storage (no prior launch has
/// proven a probe reopens, and none has been refused either) — [`save_locked`]'s new gate before it
/// ever calls `keymanager::seal` for the real session. `plant_probe` has already been asked to
/// leave evidence for the NEXT launch to check; this just writes the plaintext file exactly like
/// the always-had-no-key-manager fallback always has.
fn write_unproven_plaintext(s: &Session, json: &[u8], record_fresh: bool) -> PersistOutcome {
    log_unproven_skip_once();
    if write_plaintext_recovery(s, json, None, record_fresh).is_none() {
        crate::log(
            "session: could not persist to ANY candidate path — login will not survive a reboot",
        );
        report_write_failed();
        return PersistOutcome::WriteFailed;
    }
    PersistOutcome::PersistedPlaintext
}

/// The marker-only counterpart to [`recover_locked_session_as_plaintext`]: a PRIOR launch —
/// possibly not this one — already proved an envelope on this install unopenable, so
/// [`has_refused_marker`] alone is enough to skip `keymanager::seal`. Unlike the per-process case
/// there is no [`LOCKED_PATH`] to target (this launch's own read may have found the file already
/// plaintext, or never touched it at all), so the write goes through the normal candidate priority
/// order.
fn write_refused_plaintext(s: &Session, json: &[u8], record_fresh: bool) -> PersistOutcome {
    log_marker_skip_once();
    if write_plaintext_recovery(s, json, None, record_fresh).is_none() {
        crate::log(
            "session: could not persist to ANY candidate path — login will not survive a reboot",
        );
        report_write_failed();
        return PersistOutcome::WriteFailed;
    }
    PersistOutcome::PersistedPlaintext
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
    _s: &Session,
    json: &[u8],
    target: Option<&std::path::Path>,
    record_fresh: bool,
) -> Option<std::path::PathBuf> {
    let _ = target;
    let candidates = write_candidates();
    for (winner, category) in candidates {
        if !write_session_candidate(&winner, category, json, record_fresh) {
            continue;
        }
        note_session_write(&winner, category, json);
        not_locked_after_write();
        LAST_CLASS.store(CLASS_PLAINTEXT, std::sync::atomic::Ordering::Relaxed);
        return Some(winner);
    }
    None
}

fn write_candidates() -> Vec<(std::path::PathBuf, CandidateCategory)> {
    vec![(persistence::path(), CandidateCategory::AppDir)]
}

fn canonical_operation(stage: crate::storage::CommitStage) -> AtomicWriteOperation {
    match stage {
        crate::storage::CommitStage::CreateTemp => AtomicWriteOperation::CreateTemp,
        crate::storage::CommitStage::Write => AtomicWriteOperation::Write,
        crate::storage::CommitStage::FileSync => AtomicWriteOperation::FileSync,
        crate::storage::CommitStage::Rename => AtomicWriteOperation::Rename,
        crate::storage::CommitStage::ParentOpen => AtomicWriteOperation::OpenParent,
        crate::storage::CommitStage::ParentSync
        | crate::storage::CommitStage::Readback
        | crate::storage::CommitStage::Cleanup => AtomicWriteOperation::SyncParent,
    }
}

fn canonical_failure(error: crate::storage::StoreError) -> AtomicWriteFailure {
    match error {
        crate::storage::StoreError::Io { stage, errno } => AtomicWriteFailure::Os {
            operation: canonical_operation(stage),
            errno,
        },
        _ => AtomicWriteFailure::Policy(AtomicWritePolicy::CanonicalFailed),
    }
}

fn write_session_candidate(
    path: &std::path::Path,
    candidate: CandidateCategory,
    bytes: &[u8],
    record_fresh: bool,
) -> bool {
    let result = if path == persistence::path() {
        let payload = match String::from_utf8(bytes.to_vec()) {
            Ok(payload) => payload,
            Err(_) => return false,
        };
        let commit = persistence::commit_data(payload);
        match &commit {
            persistence::CanonicalCommit::Durable { .. } => {
                async_persistence::note_commit_durable()
            }
            persistence::CanonicalCommit::Uncertain { stage, errno } => {
                async_persistence::note_commit_uncertain(*stage, *errno)
            }
            persistence::CanonicalCommit::Failed(error) => {
                async_persistence::note_commit_failed(*error)
            }
        }
        match commit {
            persistence::CanonicalCommit::Durable { .. } => {
                *UNCERTAIN_LEGACY_SOURCE.lock().unwrap_or_else(|e| e.into_inner()) = None;
                Ok(AtomicWriteReceipt {
                    durability: AtomicWriteDurability::Durable,
                })
            }
            persistence::CanonicalCommit::Uncertain { stage, errno } => Ok(AtomicWriteReceipt {
                durability: AtomicWriteDurability::Warning {
                    operation: canonical_operation(stage),
                    errno,
                },
            }),
            persistence::CanonicalCommit::Failed(error) => Err(canonical_failure(error)),
        }
    } else {
        write_atomic_checked(path, bytes)
    };
    // A post-rename durability warning leaves a visible record but does not prove it will survive
    // a reboot. Do not retire any legacy credential copy until the canonical commit is durable.
    let result = match result {
        Ok(
            receipt @ AtomicWriteReceipt {
                durability: AtomicWriteDurability::Warning { .. },
            },
        ) => Ok(receipt),
        Ok(receipt) => {
            sweep_other_candidates(path);
            if earlier_candidates_allow_winner(path, bytes) {
                Ok(receipt)
            } else {
                crate::log(
                    "session: candidate was written but an older higher-priority file still shadows it",
                );
                Err(AtomicWriteFailure::Policy(
                    AtomicWritePolicy::ShadowedCandidate,
                ))
            }
        }
        Err(error) => Err(error),
    };
    if record_fresh {
        let result = match result {
            Ok(receipt) => FreshWriteResult::Written {
                durability: receipt.durability,
            },
            Err(failure) => FreshWriteResult::Failed { failure },
        };
        FRESH_WRITE_ATTEMPTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(FreshWriteAttempt { candidate, result });
    }
    matches!(
        result,
        Ok(AtomicWriteReceipt {
            durability: AtomicWriteDurability::Durable
        })
    )
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicWriteOperation {
    CreateTemp,
    Write,
    FileSync,
    Rename,
    OpenParent,
    SyncParent,
}

impl AtomicWriteOperation {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::CreateTemp => "create_temp",
            Self::Write => "write",
            Self::FileSync => "file_sync",
            Self::Rename => "rename",
            Self::OpenParent => "open_parent",
            Self::SyncParent => "sync_parent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicWritePolicy {
    NoParent,
    CanonicalFailed,
    CanonicalUncertain,
    DestinationNotRegular,
    DestinationWrongOwner,
    TempNameExhausted,
    /// Bytes were written, but the normal cold-read order still selects an older file.
    ShadowedCandidate,
}

impl AtomicWritePolicy {
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::NoParent => "no_parent",
            Self::CanonicalFailed => "canonical_failed",
            Self::CanonicalUncertain => "canonical_uncertain",
            Self::DestinationNotRegular => "destination_not_regular",
            Self::DestinationWrongOwner => "destination_wrong_owner",
            Self::TempNameExhausted => "temp_name_exhausted",
            Self::ShadowedCandidate => "shadowed_candidate",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicWriteFailure {
    Policy(AtomicWritePolicy),
    Os {
        operation: AtomicWriteOperation,
        errno: i32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AtomicWriteDurability {
    Durable,
    Warning {
        operation: AtomicWriteOperation,
        errno: i32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AtomicWriteReceipt {
    pub durability: AtomicWriteDurability,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FreshWriteResult {
    Written { durability: AtomicWriteDurability },
    Failed { failure: AtomicWriteFailure },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FreshWriteAttempt {
    pub candidate: CandidateCategory,
    pub result: FreshWriteResult,
}

static FRESH_WRITE_ATTEMPTS: Mutex<Vec<FreshWriteAttempt>> = Mutex::new(Vec::new());

pub(crate) fn fresh_write_attempts() -> Vec<FreshWriteAttempt> {
    FRESH_WRITE_ATTEMPTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

pub(crate) fn write_atomic(path: &std::path::Path, json: &[u8]) -> bool {
    write_atomic_checked(path, json).is_ok()
}

#[cfg(test)]
static INJECT_WRITE_OPERATION: std::sync::atomic::AtomicU8 =
    std::sync::atomic::AtomicU8::new(u8::MAX);

#[cfg(test)]
fn injected_write_error(operation: AtomicWriteOperation) -> Option<i32> {
    let code = operation as u8;
    (INJECT_WRITE_OPERATION
        .compare_exchange(
            code,
            u8::MAX,
            std::sync::atomic::Ordering::AcqRel,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_ok())
    .then_some(libc::EIO)
}

#[cfg(not(test))]
fn injected_write_error(_operation: AtomicWriteOperation) -> Option<i32> {
    None
}

fn write_atomic_checked(
    path: &std::path::Path,
    json: &[u8],
) -> Result<AtomicWriteReceipt, AtomicWriteFailure> {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    let parent = path
        .parent()
        .ok_or(AtomicWriteFailure::Policy(AtomicWritePolicy::NoParent))?;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_file() {
            return Err(AtomicWriteFailure::Policy(
                AtomicWritePolicy::DestinationNotRegular,
            ));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(AtomicWriteFailure::Policy(
                AtomicWritePolicy::DestinationWrongOwner,
            ));
        }
    }
    if let Some(errno) = injected_write_error(AtomicWriteOperation::CreateTemp) {
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::CreateTemp,
            errno,
        });
    }
    let (tmp, mut f) = create_private_temp_checked(path)?;
    if let Some(errno) = injected_write_error(AtomicWriteOperation::Write) {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::Write,
            errno,
        });
    }
    if let Err(e) = f.write_all(json) {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::Write,
            errno: e.raw_os_error().unwrap_or(0),
        });
    }
    if let Some(errno) = injected_write_error(AtomicWriteOperation::FileSync) {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::FileSync,
            errno,
        });
    }
    if let Err(e) = f.sync_all() {
        drop(f);
        let _ = std::fs::remove_file(&tmp);
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::FileSync,
            errno: e.raw_os_error().unwrap_or(0),
        });
    }
    drop(f); // the rename must not race our own open handle on a filesystem that cares
    if let Some(errno) = injected_write_error(AtomicWriteOperation::Rename) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::Rename,
            errno,
        });
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(AtomicWriteFailure::Os {
            operation: AtomicWriteOperation::Rename,
            errno: e.raw_os_error().unwrap_or(0),
        });
    }
    let durability = if let Some(errno) = injected_write_error(AtomicWriteOperation::OpenParent) {
        AtomicWriteDurability::Warning {
            operation: AtomicWriteOperation::OpenParent,
            errno,
        }
    } else {
        match std::fs::File::open(parent) {
            Err(e) => AtomicWriteDurability::Warning {
                operation: AtomicWriteOperation::OpenParent,
                errno: e.raw_os_error().unwrap_or(0),
            },
            Ok(dir) => {
                if let Some(errno) = injected_write_error(AtomicWriteOperation::SyncParent) {
                    AtomicWriteDurability::Warning {
                        operation: AtomicWriteOperation::SyncParent,
                        errno,
                    }
                } else {
                    match dir.sync_all() {
                        Ok(()) => AtomicWriteDurability::Durable,
                        Err(e) => AtomicWriteDurability::Warning {
                            operation: AtomicWriteOperation::SyncParent,
                            errno: e.raw_os_error().unwrap_or(0),
                        },
                    }
                }
            }
        }
    };
    remove_temp_siblings(path);
    Ok(AtomicWriteReceipt { durability })
}

fn create_private_temp_checked(
    path: &std::path::Path,
) -> Result<(std::path::PathBuf, std::fs::File), AtomicWriteFailure> {
    use std::os::unix::fs::OpenOptionsExt;
    for attempt in 0..16u64 {
        let tmp = random_tmp_path(path, attempt).ok_or(AtomicWriteFailure::Policy(
            AtomicWritePolicy::TempNameExhausted,
        ))?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => {
                return Err(AtomicWriteFailure::Os {
                    operation: AtomicWriteOperation::CreateTemp,
                    errno: e.raw_os_error().unwrap_or(0),
                })
            }
        }
    }
    Err(AtomicWriteFailure::Policy(
        AtomicWritePolicy::TempNameExhausted,
    ))
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
    read_owned_regular_trusted(path).map(|(bytes, _)| bytes)
}

/// [`read_owned_regular_trusted`]'s ORIGINAL contract, kept byte-for-byte: two other modules
/// (`telemetry::spool`, `telemetry::mod`) call this directly rather than through the trust-blind
/// wrapper above, and this lane does not own those files. [`read_owned_regular_checked`] below is
/// the new typed-rejection twin — same checks, `Result` instead of folding every failure into
/// `None` — and this function is now just that one with the error discarded, so every existing
/// caller (in or out of this module) is untouched.
pub(crate) fn read_owned_regular_trusted(path: &std::path::Path) -> Option<(Vec<u8>, ModeTrust)> {
    read_owned_regular_checked(path).ok()
}

/// **Why a session file that EXISTS was not read back** — issue #76's field report gap. Every
/// variant here is a verdict [`read_owned_regular_checked`] can reach on its own open/metadata/
/// read-to-end sequence, ordered the same way the checks run. `Missing` is the one variant that
/// really does mean "nothing there" (`ENOENT`); every other variant means the opposite — a file
/// exists at this name and this launch declined it — which is exactly the distinction that used to
/// be lost the moment a caller wrote `.ok()?`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadRejection {
    /// `open(2)` failed with `ENOENT` — there really is nothing at this candidate.
    Missing,
    /// `open(2)` failed with anything but `ENOENT` (permission denied, a symlink `O_NOFOLLOW`
    /// refused, too many open files, …). Carries the raw errno.
    OpenFailed(i32),
    /// `fstat` succeeded but the file is not a regular file — a FIFO, a directory, or whatever a
    /// dangling symlink's target turned out to be.
    NotRegular,
    /// `fstat`'s `st_uid` is not this process's own euid — a peer's file at our candidate name.
    WrongOwner,
    /// `fstat` itself failed on an fd `open` already returned. Carries the raw errno.
    MetadataFailed(i32),
    /// The bytes were read whole, owned and trusted — and are none of the shapes this build knows:
    /// not a v1 `SecureEnvelope`, not envelope-shaped ([`identifies_secure_envelope`]), not a
    /// `Session`. A zero-byte or truncated `auth.json` is the concrete case (see
    /// [`write_atomic`]'s own doc for the historical `O_TRUNC` write that produced one). Unlike
    /// every other variant here this is a verdict on the CONTENT, decided by [`read_locked`] after
    /// [`read_owned_regular_checked`] has already succeeded — the same relationship
    /// [`UntrustedMode`](Self::UntrustedMode) has to that function.
    Unparsable,
    /// The file read past the size cap ([`read_owned_regular_trusted`]'s `MAX_FILE`) before EOF.
    TooLarge,
    /// `read_to_end` returned an I/O error partway through. Carries the raw errno.
    ReadFailed(i32),
    /// [`ModeTrust::content_trusted`] said no — the file was found group/other-writable, so its
    /// bytes are never parsed. Distinct from every variant above: the open/stat/read sequence all
    /// succeeded, and this is a verdict about what the mode implies for the CONTENT, decided by
    /// the caller ([`read_locked`], [`read_trusted_marker`]) rather than by this function itself.
    UntrustedMode,
}

impl ReadRejection {
    /// Every variant, in no particular order — the exhaustiveness source for
    /// `tests::read_rejection_and_candidate_category_wire_words_round_trip`. The `i32` payloads
    /// are arbitrary placeholders; only the shape matters here, not the value.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Missing,
        Self::OpenFailed(13),
        Self::NotRegular,
        Self::WrongOwner,
        Self::MetadataFailed(5),
        Self::TooLarge,
        Self::ReadFailed(9),
        Self::UntrustedMode,
        Self::Unparsable,
    ];

    #[cfg(test)]
    #[allow(dead_code)]
    fn _assert_all_variants_covered(v: Self) {
        match v {
            Self::Missing
            | Self::OpenFailed(_)
            | Self::NotRegular
            | Self::WrongOwner
            | Self::MetadataFailed(_)
            | Self::TooLarge
            | Self::ReadFailed(_)
            | Self::UntrustedMode
            | Self::Unparsable => {}
        }
    }

    /// The word this rejection is reported as — telemetry-pinned, never renamed casually. See
    /// [`candidate_reads_wire`] for the line it goes into.
    pub(crate) fn wire(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::OpenFailed(_) => "open_failed",
            Self::NotRegular => "not_regular",
            Self::WrongOwner => "wrong_owner",
            Self::MetadataFailed(_) => "metadata_failed",
            Self::TooLarge => "too_large",
            Self::ReadFailed(_) => "read_failed",
            Self::UntrustedMode => "untrusted_mode",
            Self::Unparsable => "unparsable",
        }
    }

    /// The raw errno carried by this rejection, when it has one — `Some` for exactly the three
    /// variants that ARE evidence of a failed syscall (`OpenFailed`/`MetadataFailed`/`ReadFailed`),
    /// and `None` for every other variant. `Missing`'s `ENOENT` is implied by the variant itself
    /// and not worth repeating; `NotRegular`/`WrongOwner`/`TooLarge`/`UntrustedMode`/`Unparsable`
    /// are verdicts about what a SUCCESSFUL call returned, not about a call that failed. (This
    /// sentence used to open "`None` for the three variants", which counted the wrong side of the
    /// split and was already wrong by two before `Unparsable` made it three.)
    pub(crate) fn errno(self) -> Option<i32> {
        match self {
            Self::OpenFailed(e) | Self::MetadataFailed(e) | Self::ReadFailed(e) => Some(e),
            Self::Missing
            | Self::NotRegular
            | Self::WrongOwner
            | Self::TooLarge
            | Self::UntrustedMode
            | Self::Unparsable => None,
        }
    }
}

/// [`read_owned_regular`]'s trust-aware twin — same open/ownership/regular-file/size checks, but
/// also hands back what [`repair_owned_mode`] found, so a caller for whom CONTENT (not just
/// existence) matters can refuse to trust bytes that another uid could have rewritten. Every new
/// caller that reads something more than "does this exist" should reach for this one, not the
/// trust-blind wrapper above.
///
/// Returns [`ReadRejection`] rather than folding every failure into `None` (review finding, issue
/// #76 field report): a caller that only cares about existence still gets exactly that shape
/// through [`read_owned_regular`] above, but [`read_locked`]'s candidate loop can now say WHICH
/// candidate it declined and why, rather than treating a rejected file identically to an absent
/// one.
pub(crate) fn read_owned_regular_checked(
    path: &std::path::Path,
) -> Result<(Vec<u8>, ModeTrust), ReadRejection> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        // O_NONBLOCK is load-bearing here, not decoration: without it, `open(2)` on a FIFO a peer
        // planted at this fixed name blocks BEFORE the `is_file()` check below can ever run,
        // wedging the boot path (or the frame loop, for the spool's twin). It is a no-op for a
        // genuine regular file on Linux, so nothing below needs to clear it back off (review
        // finding, 2026-09-10).
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(errno) if errno == libc::ENOENT => ReadRejection::Missing,
            Some(errno) => ReadRejection::OpenFailed(errno),
            None => ReadRejection::OpenFailed(0),
        })?;
    let meta = file
        .metadata()
        .map_err(|e| ReadRejection::MetadataFailed(e.raw_os_error().unwrap_or(0)))?;
    if !meta.file_type().is_file() {
        return Err(ReadRejection::NotRegular);
    }
    if meta.uid() != unsafe { libc::geteuid() } {
        return Err(ReadRejection::WrongOwner);
    }
    let trust = repair_owned_mode(&file, &meta, path);
    const MAX_FILE: u64 = 4 * 1024 * 1024;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_FILE + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ReadRejection::ReadFailed(e.raw_os_error().unwrap_or(0)))?;
    if bytes.len() as u64 > MAX_FILE {
        return Err(ReadRejection::TooLarge);
    }
    Ok((bytes, trust))
}

/// **A repair fixes the MODE. It says nothing about whether the CONTENT can be trusted.** Read-only
/// widening (any of `0o044`/`0o055`, i.e. group/other could only ever READ the file) is a
/// disclosure problem — the bytes on disk are still whatever this process last wrote, so a stored
/// `usage: true`, a session token, or a spool record is still OUR decision/OUR data, merely one a
/// peer in the shared namespace could also have read. Any group/other WRITE bit (`0o022`) is a
/// different claim entirely: another uid could have REWRITTEN the file between our last write and
/// this read, so a "yes" in a consent file, a token in a session file, or a record in a spool is no
/// longer provably ours. Every caller that reads more than "does this file exist" has to make that
/// distinction, which is what this type exists to carry out of [`repair_owned_mode`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModeTrust {
    /// No widening was found — the mode was already `0600` (or tighter).
    Trusted,
    /// The mode was widened. `readable_only` is `true` when the widening was read-only (no `0o022`
    /// bit) — content stays trusted — and `false` when any write bit was set — content must be
    /// treated as forged/untrusted.
    ///
    /// `fixed` is whether the `fchmod` back to `0600` actually SUCCEEDED. It is `true` on every
    /// television anyone has measured and it is not decoration (review finding, 2026-09-11): a
    /// read-only remount, or a jail/LSM that denies the operation, leaves the file exactly as
    /// found, and a caller that assumed otherwise moved a token-bearing file to a second name
    /// still writable by the peer that widened it — see [`quarantine_untrusted`].
    Repaired { readable_only: bool, fixed: bool },
}

impl ModeTrust {
    /// Whether the bytes just read may still be treated as this process's own — `false` for a
    /// `Repaired { readable_only: false, .. }` (a write-widened file), `true` for everything else.
    pub(crate) fn content_trusted(self) -> bool {
        !matches!(
            self,
            ModeTrust::Repaired {
                readable_only: false,
                ..
            }
        )
    }

    /// Whether the file is 0600 NOW — trivially true where nothing was widened, and otherwise
    /// exactly whether the repair took. The one caller that must ask is the one which keeps a
    /// widened file's bytes on disk under another name.
    pub(crate) fn mode_is_owner_only(self) -> bool {
        !matches!(self, ModeTrust::Repaired { fixed: false, .. })
    }
}

/// **Test-only: make the `fchmod` in [`repair_owned_mode`] fail.** There is no portable way to
/// make one fail on a file this process owns — that is the syscall's own contract — so the only
/// way to grade what this app does when the repair does NOT take is to refuse it here. A device
/// really can: a read-only remount, or an LSM/jail that denies the operation, leaves an owned
/// world-writable file exactly as found, which is the state [`quarantine_untrusted`] must not
/// treat as "already 0600".
///
/// A `#[cfg(test)]` global, so a shipped binary has neither the flag nor the branch.
#[cfg(test)]
static FCHMOD_REFUSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(test)]
fn fchmod_refused_for_test() -> bool {
    FCHMOD_REFUSED.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(not(test))]
fn fchmod_refused_for_test() -> bool {
    false
}

/// **Repair, never refuse.** An OWNED regular file (the caller has already checked uid + file type
/// on the same open fd this takes) found with any group/other bit set is fixed to 0600 via
/// `fchmod` on that fd — never a `chmod` by path, which would reopen the name and race whatever a
/// peer in the shared `/media/developer` namespace does between the check and the fix. A file that
/// is not ours, or not regular, is never handed to this function at all; that refusal happens
/// earlier, at the ownership check, and stays a refusal.
///
/// Logged once per repair (`perm: repaired <basename> was <octal>`, with a trailing
/// `(content untrusted)` when any WRITE bit was set) so a corrupted mode never fixes itself
/// invisibly — see `docs/measurements/credential-storage-native-apps-2026-09-10.md` for why a
/// widened mode on one of these files is worth a line: the debug install's telemetry spool was
/// found at 0777 on the device, and the pre-2026-09-10 code refused every write to it forever
/// instead of fixing what it could safely fix. **Fixing the mode is never enough on its own for a
/// write-widened file** — see [`ModeTrust`]'s doc — which is why this returns what it found rather
/// than nothing: the mode is always repaired, but the caller decides what the repair means for the
/// bytes it is about to read.
pub(crate) fn repair_owned_mode(
    file: &std::fs::File,
    meta: &std::fs::Metadata,
    path: &std::path::Path,
) -> ModeTrust {
    use std::os::unix::fs::PermissionsExt;
    let before = meta.permissions().mode() & 0o777;
    if before & 0o077 == 0 {
        return ModeTrust::Trusted;
    }
    let writable = before & 0o022 != 0;
    // A write-widened file is attacker-controlled content. Do not first make it look owner-only:
    // if its quarantine/delete then fails, a later launch would otherwise accept the same bytes.
    let fixed = !writable
        && !fchmod_refused_for_test()
        && file
            .set_permissions(std::fs::Permissions::from_mode(0o600))
            .is_ok();
    if fixed {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        if writable {
            crate::log(&format!(
                "perm: repaired {name} was {before:o} (content untrusted)"
            ));
        } else {
            crate::log(&format!("perm: repaired {name} was {before:o}"));
        }
    }
    ModeTrust::Repaired {
        readable_only: !writable,
        fixed,
    }
}

/// Read a marker/probe file, but only trust it when its mode never allowed a write from outside
/// this process. A write-widened marker is not merely repaired — it is IGNORED (treated exactly as
/// absent, the same as [`std::fs::File::open`] failing) and deleted outright, because a forged
/// `secure-storage.proven` marker sitting there must never promote this install to sealing, and a
/// forged `secure-storage.refused` marker must never talk a healthy install out of it. See
/// [`ModeTrust`]'s doc for the read-only-vs-writable distinction this rests on.
fn read_trusted_marker(path: &std::path::Path) -> Option<Vec<u8>> {
    let (bytes, trust) = read_owned_regular_trusted(path)?;
    if !trust.content_trusted() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        crate::log(&format!(
            "perm: ignored and removed {name} after an untrusted mode"
        ));
        remove_temp_siblings(path);
        let _ = std::fs::remove_file(path);
        return None;
    }
    Some(bytes)
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

/// **Move a write-widened session file ASIDE rather than destroying it** (maintainer decision,
/// 2026-09-10).
///
/// The rule that matters is unchanged and is not negotiable: bytes another uid could have written
/// are never parsed, and they must not still be sitting at the name the next launch reads. What
/// changed is what happens to them afterwards. They are the only record of what was tampered with
/// — and of what this television owner's own sign-in used to hold — so they are renamed to
/// [`untrusted_path`], 0600, in the same directory, for the owner to inspect. Nothing in this app
/// ever reads them again: the suffix is not a candidate ([`auth_paths`]) and not an atomic-write
/// sibling ([`remove_temp_siblings`]), so no later read, save or sweep touches it. [`clear`] does
/// — a sign-out that left a former account's token bytes on a rooted television would not be one.
///
/// The mode is 0600 by the time this runs *provided the repair took*:
/// [`read_owned_regular_trusted`] fixes it through `fchmod` on the fd it had already checked for
/// ownership and file type, which is the only way to fix a mode without racing a peer between the
/// check and the fix. This function therefore never `chmod`s by path — it takes the answer instead.
/// **`mode_is_owner_only` is a REQUIREMENT, not a hint** (review finding, 2026-09-11): where the
/// `fchmod` failed (a read-only remount, a jail or LSM that denies it) the file is still
/// world-writable, and moving it aside would leave a real account token at a fixed, guessable name
/// in a mode any peer can rewrite — strictly worse than the delete this branch always did, arrived
/// at while trying to preserve evidence. A file that cannot be made owner-only is destroyed.
///
/// `rename` replaces whatever is at the destination name, including a symlink a peer planted
/// (rename does not follow one) — but not a DIRECTORY, which is how it realistically fails.
/// **Any failure falls back to deleting the file, exactly as this branch did before**: keeping the
/// evidence is worth doing and worth nothing beside leaving a forged file at a name that is read
/// on the next boot. Returns whether the bytes were kept, for the log line only.
///
/// **Only the most recent quarantine is kept.** A previous `<name>.untrusted` is removed before
/// the rename (`rename` would replace a file or a symlink anyway; this also clears the one shape it
/// would not, a directory), so a second tampering overwrites the first one's evidence rather than
/// accumulating a numbered series in a jail directory this app does not police the size of.
fn quarantine_untrusted(path: &std::path::Path, mode_is_owner_only: bool) -> bool {
    let Some(aside) = untrusted_path(path) else {
        let _ = remove_cleanup_file(path, &mut std::collections::BTreeSet::new());
        return false;
    };
    // A previous quarantine (or anything a peer left at the name) goes first: `rename` would
    // replace a file or symlink anyway, and this also clears the one shape it would not.
    // A directory at the quarantine name deliberately blocks rename; do not follow it.
    let _ = std::fs::remove_file(&aside);
    if std::fs::rename(path, &aside).is_ok() {
        if !mode_is_owner_only {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
            let repaired = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(&aside)
                .ok()
                .and_then(|file| {
                    let meta = file.metadata().ok()?;
                    (meta.file_type().is_file() && meta.uid() == unsafe { libc::geteuid() })
                        .then_some(file)
                })
                .is_some_and(|file| {
                    !fchmod_refused_for_test()
                        && file
                            .set_permissions(std::fs::Permissions::from_mode(0o600))
                            .is_ok()
                });
            if !repaired {
                let _ = remove_cleanup_file(&aside, &mut std::collections::BTreeSet::new());
                return false;
            }
        }
        return true;
    }
    let _ = remove_cleanup_file(path, &mut std::collections::BTreeSet::new());
    false
}

/// Where a write-widened session file is moved aside to — see [`quarantine_untrusted`]. One
/// definition for the same reason [`tmp_path`] is one: [`clear`] has to delete exactly this file,
/// and a sign-out that spelled the suffix differently would leave a former account's token bytes
/// on the disk.
///
/// The suffix cannot collide with a real candidate ([`auth_paths`] ends `-auth.json`) or with an
/// atomic-write sibling (`.tmp.*`, which [`remove_temp_siblings`] sweeps), so a quarantined copy is
/// never read back as a session and never swept by an ordinary save.
fn untrusted_path(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut name = path.file_name()?.to_os_string();
    name.push(".untrusted");
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearDurability {
    Durable,
    Uncertain,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClearOutcome {
    pub durability: ClearDurability,
    pub cleanup_failed: bool,
}

impl ClearOutcome {
    pub fn durable(self) -> bool {
        matches!(self.durability, ClearDurability::Durable)
    }
}

pub fn clear() -> bool {
    clear_outcome().durable()
}

pub fn clear_outcome() -> ClearOutcome {
    publish_cache(Session::default());
    clear_outcome_disk()
}

/// Revoke the in-memory snapshot immediately and queue the canonical cleared record.
pub(crate) fn clear_with_receipt(
) -> Result<async_persistence::Receipt, async_persistence::AdmissionError> {
    async_persistence::clear()
}

/// Blocking revocation half used by both the synchronous sign-out path and the asynchronous
/// coordinator. Snapshot revocation happens at admission, before this function is allowed to
/// wait for disk, and this function must never republish over a newer admitted sign-in.
fn clear_outcome_disk() -> ClearOutcome {
    let _io = io();
    let canonical_key = match persistence::load() {
        persistence::CanonicalRead::Data { payload, .. } => {
            serde_json::from_str::<SecureEnvelope>(&payload)
                .ok()
                .and_then(|e| {
                    (e.format == SECURE_FORMAT && e.version == 1)
                        .then_some((e.sealed.backend, e.sealed.key))
                })
        }
        _ => None,
    };
    let canonical = persistence::commit_cleared();
    match &canonical {
        persistence::CanonicalCommit::Durable { .. } => {
            async_persistence::note_commit_durable()
        }
        persistence::CanonicalCommit::Uncertain { stage, errno } => {
            async_persistence::note_commit_uncertain(*stage, *errno)
        }
        persistence::CanonicalCommit::Failed(error) => {
            async_persistence::note_commit_failed(*error)
        }
    }
    if matches!(canonical, persistence::CanonicalCommit::Durable { .. }) {
        if let Some((backend, key)) = canonical_key {
            crate::keymanager::remove(&backend, &key);
        }
    }
    let cleanup = persistence::cleanup_temporaries();
    if let Err(error) = &cleanup {
        crate::log(&format!(
            "session: canonical temporary cleanup failed: {error:?}"
        ));
    }
    // Every candidate, not just the one we happen to write today: leaving a copy at any other
    // location would let `peek`'s search resurrect the stale session on the next boot. The `.tmp`
    // siblings go too — `peek` cannot read one, so it is not a resurrection risk, but a sign-out
    // that leaves a live account token in a file on a rooted television is not a sign-out.
    let mut cleanup_failed = cleanup.is_err();
    let mut cleanup_parents = std::collections::BTreeSet::new();
    for path in auth_paths() {
        if let Some(bytes) = read_owned_regular(&path) {
            if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
                if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                    crate::keymanager::remove(&envelope.sealed.backend, &envelope.sealed.key);
                }
            }
        }
        remove_temp_siblings(&path);
        // A quarantined copy (`quarantine_untrusted`) holds the bytes of a tampered-with file that
        // belonged to the account now signing out. It exists for the owner to inspect, not to
        // outlive them.
        if let Some(aside) = untrusted_path(&path) {
            cleanup_failed |= !remove_cleanup_file(&aside, &mut cleanup_parents);
        }
        cleanup_failed |= !remove_cleanup_file(&path, &mut cleanup_parents);
    }
    // The marker carries no credential, but leaving it behind would keep a FUTURE sign-in on this
    // same install pinned to plaintext for no reason connected to the account that just left.
    // Issue #76 review (should-fix): a refused marker can coexist with a proven one — this
    // install once proved a PROBE reopens, and separately, LATER, an envelope it actually wrote
    // failed to reopen (a key that broke, or one that was never stable across launches to begin
    // with). Dropping only the refused half here used to let the very next save skip straight
    // back to a REAL seal (`seal_permitted` sees no marker and a proven install), re-running the
    // exact failure sign-out was meant to give the install a fresh chance to avoid — so when a
    // refused marker existed, the proven one it was found alongside is stale too and goes with it;
    // the account's next envelope re-earns proven storage through the probe like any other.
    let had_refused = refused_marker_paths()
        .iter()
        .any(|p| read_owned_regular(p).is_some());
    for path in refused_marker_paths() {
        remove_temp_siblings(&path);
        cleanup_failed |= !remove_cleanup_file(&path, &mut cleanup_parents);
    }
    // The unanswered-launch counter goes too: it describes a run of launches against THIS
    // envelope, and the envelope has just been deleted.
    clear_unavailable_marker();
    // An in-flight probe belongs to the account that just signed out — a new sign-in earns its own.
    for path in probe_paths() {
        remove_temp_siblings(&path);
        cleanup_failed |= !remove_cleanup_file(&path, &mut cleanup_parents);
    }
    // **The PROVEN marker is otherwise deliberately NOT removed here.** Unlike the refused marker,
    // it is a fact about this TELEVISION's key manager — "a prior launch proved a probe reopens on
    // this firmware, on this install directory" — not about the account that is leaving. The next
    // sign-in (this account or another) gets to skip re-earning what the device has already shown
    // it can do; only `clear()`'s own directory going away (an uninstall), the underlying firmware
    // changing, or (see above) a refused marker having been recorded alongside it, makes that fact
    // stale.
    if had_refused {
        for path in proven_marker_paths() {
            remove_temp_siblings(&path);
            cleanup_failed |= !remove_cleanup_file(&path, &mut cleanup_parents);
        }
    }
    cleanup_failed |= !sync_cleanup_parents(cleanup_parents);
    // No file, so nothing left to call Locked — and no cached copy of the session that just got
    // signed out should keep answering `peek`.
    LOCKED_STATE.store(NOT_LOCKED, std::sync::atomic::Ordering::Relaxed);
    *LOCKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = None;
    LAST_CLASS.store(CLASS_UNKNOWN, std::sync::atomic::Ordering::Relaxed);
    *LAST_SESSION_WRITE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *LAST_FRESH_READBACK
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    FRESH_WRITE_ATTEMPTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    FRESH_SAVE_ERRORS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    *COLD_SESSION_FACTS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *COLD_STORAGE_DIAGNOSTIC
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = None;
    *LAST_PERSIST.lock().unwrap_or_else(|e| e.into_inner()) = None;
    CANDIDATE_READS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    ClearOutcome {
        durability: match canonical {
            persistence::CanonicalCommit::Durable { .. } => ClearDurability::Durable,
            persistence::CanonicalCommit::Uncertain { .. } => ClearDurability::Uncertain,
            persistence::CanonicalCommit::Failed(_) => ClearDurability::Failed,
        },
        cleanup_failed,
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
    use crate::storage::RecordStore;

    #[test]
    fn application_snapshot_reads_do_not_wait_for_the_disk_writer() {
        let _serial = crate::testlock::serial();
        let saved_cache = cached();
        publish_cache(Session {
            client_id: "snapshot-client".into(),
            ..Default::default()
        });
        let disk_guard = io();
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = crate::task::spawn("snapshot read test", move || {
            tx.send((peek().client_id, load().client_id)).unwrap();
        })
        .unwrap();
        let while_disk_busy = rx.recv_timeout(std::time::Duration::from_millis(250));
        drop(disk_guard);
        reader.join().unwrap();
        match saved_cache {
            Some(s) => publish_cache(s),
            None => clear_cache(),
        }
        assert_eq!(
            while_disk_busy.unwrap(),
            ("snapshot-client".into(), "snapshot-client".into())
        );
    }

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

    fn canonical_payload() -> String {
        match persistence::load() {
            persistence::CanonicalRead::Data { payload, .. } => payload,
            _ => panic!("expected canonical session payload"),
        }
    }

    fn canonical_session() -> Session {
        serde_json::from_str(&canonical_payload()).expect("canonical payload is a Session")
    }

    #[test]
    fn clear_requires_explicit_new_signin_and_never_resurrects_old_session() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("clear-new-signin");
        assert!(save_after_reauthentication(&signed_in()).persisted());
        assert_eq!(peek().account_token, "acct");

        assert!(clear());
        let fresh = begin_new_signin();
        assert!(!fresh.client_id.is_empty());
        assert_ne!(fresh.client_id, "cid-1");
        assert!(fresh.account_token.is_empty());
        assert_eq!(peek().client_id, fresh.client_id);
        assert!(peek().account_token.is_empty());

        clear_cache();
        let cold = load();
        assert!(cold.client_id.is_empty());
        assert!(cold.account_token.is_empty());
    }

    #[test]
    fn clear_reports_cleanup_failure_and_retries_leftover_quarantine() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("clear-retries-cleanup");
        assert!(save_after_reauthentication(&signed_in()).persisted());
        let aside = untrusted_path(&t.file()).unwrap();
        std::fs::write(&aside, b"former-token-bytes").unwrap();

        fail_next_cleanup_remove_for_test();
        let first = clear_outcome();
        assert_eq!(first.durability, ClearDurability::Durable);
        assert!(first.cleanup_failed, "durable tombstone and cleanup are independent");
        assert!(aside.exists(), "the injected cleanup failure leaves retryable residue");

        let second = clear_outcome();
        assert_eq!(second.durability, ClearDurability::Durable);
        assert!(!second.cleanup_failed, "a later clear retries cleanup");
        assert!(!aside.exists(), "the retry removes former credential material");
    }

    #[test]
    fn corrupt_canonical_session_is_terminal_and_does_not_fall_back_to_legacy() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("corrupt-canonical-terminal");
        std::fs::write(persistence::path(), b"not-a-record").unwrap();
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        clear_cache();
        let session = load();
        assert!(session.account_token.is_empty());
        assert!(session.client_id.is_empty());
    }

    #[test]
    fn nested_future_fields_survive_same_identity_session_rewrite() {
        let mut session = signed_in();
        session.server.machine_id = "server-1".into();
        session.sources.push(SourceRef {
            machine_id: "server-1".into(),
            ..Default::default()
        });
        session.user.uuid = "user-1".into();
        session.home_users.push(HomeUserRef {
            uuid: "home-1".into(),
            ..Default::default()
        });
        session.home_pins.push(HomePins {
            user: "home-1".into(),
            on: vec![PinnedLib {
                machine_id: "server-1".into(),
                key: 7,
                ..Default::default()
            }],
            ..Default::default()
        });
        session.recent_searches.push(RecentSearches {
            user: "home-1".into(),
            terms: vec!["old".into()],
            ..Default::default()
        });
        let mut value = serde_json::to_value(&session).unwrap();
        value["server"]["futureServerField"] = serde_json::json!(true);
        value["sources"][0]["futureSourceField"] = serde_json::json!({"v": 2});
        value["user"]["futureUserField"] = serde_json::json!("kept");
        value["home_users"][0]["futureHomeField"] = serde_json::json!(3);
        value["home_pins"][0]["futurePinsField"] = serde_json::json!(false);
        value["home_pins"][0]["on"][0]["futurePinField"] = serde_json::json!("x");
        value["recent_searches"][0]["futureSearchField"] = serde_json::json!("kept");

        let mut rewritten: Session = serde_json::from_value(value).unwrap();
        rewritten.server.address = "192.0.2.10".into();
        rewritten.sources[0].port = 32400;
        rewritten.user.title = "owner".into();
        rewritten.home_users[0].title = "member".into();
        rewritten.home_pins[0].asked = true;
        rewritten.recent_searches[0].terms.push("new".into());
        let persisted = serde_json::to_value(rewritten).unwrap();
        assert_eq!(persisted["server"]["futureServerField"], true);
        assert_eq!(persisted["sources"][0]["futureSourceField"]["v"], 2);
        assert_eq!(persisted["user"]["futureUserField"], "kept");
        assert_eq!(persisted["home_users"][0]["futureHomeField"], 3);
        assert_eq!(persisted["home_pins"][0]["futurePinsField"], false);
        assert_eq!(persisted["home_pins"][0]["on"][0]["futurePinField"], "x");
        assert_eq!(persisted["recent_searches"][0]["futureSearchField"], "kept");
    }

    #[test]
    fn app_state_fallback_survives_cold_read_and_signout_clears_it() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("app-state-fallback");
        let state = t.dir.join("state");
        assert!(state.is_dir());
        let file = persistence::path();
        redirect_for_test_multi(vec![t.dir.join("absent-developer/auth.json"), file.clone()]);
        assert!(save_after_reauthentication(&signed_in()).persisted());
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        clear_cache();
        assert_eq!(peek().account_token, "acct");
        clear();
        assert!(!file.exists());
        assert!(
            state.is_dir(),
            "sign-out must retain the packaged directory"
        );
    }

    #[test]
    fn legacy_app_root_session_migrates_to_packaged_state_on_save() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("legacy-to-state");
        let state = t.dir.join("state");
        assert!(state.is_dir());
        let new_path = persistence::path();
        let legacy = t.dir.join("auth.json");
        assert!(write_atomic(
            &legacy,
            &serde_json::to_vec(&signed_in()).unwrap()
        ));
        redirect_for_test_multi(vec![
            t.dir.join("absent/auth.json"),
            new_path.clone(),
            legacy.clone(),
        ]);
        let old = peek();
        assert_eq!(old.account_token, "acct");
        assert!(save(&old).persisted());
        assert!(new_path.is_file());
        assert!(
            !legacy.exists(),
            "the migrated legacy file must not shadow later saves"
        );
        clear_cache();
        assert_eq!(peek().account_token, "acct");
    }

    #[test]
    fn uncertain_migration_keeps_legacy_through_two_uncertain_commits_until_durable() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("uncertain-migration-retry");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::ParentSync);
        let _ = read_locked();
        assert!(t.file().exists(), "an uncertain migration never retires its only durable source");
        crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::ParentSync);
        assert!(!save(&signed_in()).persisted());
        assert!(t.file().exists(), "a second uncertainty still preserves the legacy source");
        assert!(save(&signed_in()).persisted());
        assert!(!t.file().exists(), "the later durable same-process commit retries cleanup");
    }

    #[test]
    fn unknown_envelope_in_packaged_state_is_not_replaced_by_fresh_signin() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("state-unknown-envelope");
        let state = t.dir.join("state");
        assert!(state.is_dir());
        let file = persistence::path();
        let unknown = br#"{"format":"plxnative-secure-session","version":99}"#;
        assert!(write_atomic(&file, unknown));
        redirect_for_test_multi(vec![
            t.dir.join("absent/auth.json"),
            t.dir.join("auth.json"),
        ]);
        assert_eq!(
            save_after_reauthentication(&signed_in()),
            PersistOutcome::BlockedUnknownEnvelope
        );
        assert_eq!(std::fs::read(file).unwrap(), unknown);
    }

    #[test]
    fn lower_state_write_cannot_claim_success_when_readonly_old_session_shadows_it() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        // Root bypasses directory DAC. The ordinary non-root macOS/Linux host
        // runners exercise the actual refusal, not a mocked successful sweep.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        let t = TwoCandidateSession::new("state-shadowed");
        let higher_dir = t.dir.join("readonly");
        let state = t.dir.join("state");
        std::fs::create_dir(&higher_dir).unwrap();
        assert!(state.is_dir());
        let higher = higher_dir.join("auth.json");
        let lower = persistence::path();
        assert!(write_atomic(
            &higher,
            &serde_json::to_vec(&signed_in()).unwrap()
        ));
        redirect_for_test_multi(vec![higher.clone(), lower.clone()]);
        assert_eq!(peek().account_token, "acct");
        std::fs::set_permissions(&higher_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut fresh = signed_in();
        fresh.account_token = "new-account".into();
        let outcome = save_after_reauthentication(&fresh);
        std::fs::set_permissions(&higher_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            lower.exists(),
            "a lower physical write succeeds in this fixture"
        );
        assert!(outcome.persisted(), "the canonical write is authoritative");
        clear_cache();
        assert_eq!(peek().account_token, "new-account");
    }

    /// A save lands as a WHOLE file — written to a sibling tmp and renamed over — leaving nothing
    /// behind, and the credentials are never on disk in a mode another uid can read (this box is
    /// rooted and `/media/developer` is world-readable). The tmp is where the secret exists first,
    /// so the 0600 rule has to reach it too.
    #[test]
    fn a_save_lands_whole_and_leaves_no_temporary_behind() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let _t = TempSession::new("whole");

        save(&signed_in());
        assert_eq!(peek().account_token, "acct", "and it reads back");
        assert!(
            std::fs::read_dir(persistence::root())
                .unwrap()
                .all(|entry| {
                    entry
                        .ok()
                        .and_then(|e| {
                            e.file_name()
                                .to_str()
                                .map(|n| !n.starts_with(".session.json.new-"))
                        })
                        .unwrap_or(true)
                }),
            "the temporary record file is cleaned up"
        );
        let mode = std::fs::metadata(persistence::path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "credentials at rest");

        // a sign-out takes the tmp with it: `peek` cannot read one, but a live account token left
        // in a file on a rooted television is not a sign-out
        clear();
        assert!(
            persistence::path().exists(),
            "clear retains the canonical tombstone"
        );
    }

    #[test]
    fn checked_atomic_write_distinguishes_policy_from_create_temp_errno() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("typed-atomic-errors");
        let directory = t.dir.join("directory-destination");
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(
            write_atomic_checked(&directory, b"x"),
            Err(AtomicWriteFailure::Policy(
                AtomicWritePolicy::DestinationNotRegular
            ))
        );
        let missing_parent = t.dir.join("absent").join("auth.json");
        assert!(matches!(
            write_atomic_checked(&missing_parent, b"x"),
            Err(AtomicWriteFailure::Os {
                operation: AtomicWriteOperation::CreateTemp,
                errno
            }) if errno == libc::ENOENT
        ));
    }

    #[test]
    fn checked_atomic_write_reports_each_injected_operation_and_durability_warning() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("typed-atomic-injected");
        for operation in [
            AtomicWriteOperation::CreateTemp,
            AtomicWriteOperation::Write,
            AtomicWriteOperation::FileSync,
            AtomicWriteOperation::Rename,
        ] {
            INJECT_WRITE_OPERATION.store(operation as u8, std::sync::atomic::Ordering::Release);
            assert_eq!(
                write_atomic_checked(&t.file(), b"payload"),
                Err(AtomicWriteFailure::Os {
                    operation,
                    errno: libc::EIO,
                })
            );
        }
        for operation in [
            AtomicWriteOperation::OpenParent,
            AtomicWriteOperation::SyncParent,
        ] {
            INJECT_WRITE_OPERATION.store(operation as u8, std::sync::atomic::Ordering::Release);
            assert_eq!(
                write_atomic_checked(&t.file(), b"payload"),
                Ok(AtomicWriteReceipt {
                    durability: AtomicWriteDurability::Warning {
                        operation,
                        errno: libc::EIO,
                    },
                })
            );
            assert!(
                write_atomic(&t.file(), b"next"),
                "the bool wrapper keeps warning-as-success"
            );
        }
    }

    #[test]
    fn fresh_save_records_ordered_candidate_failures_then_the_winner() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("fresh-write-attempts");
        crate::storage::inject_next_commit_failure_for_test(
            crate::storage::CommitStage::ParentSync,
        );
        assert_eq!(
            save_after_reauthentication(&signed_in()),
            PersistOutcome::WriteFailed
        );
        let attempts = fresh_write_attempts();
        assert_eq!(attempts.len(), 1);
        assert!(matches!(
            attempts[0],
            FreshWriteAttempt {
                candidate: CandidateCategory::AppDir,
                result: FreshWriteResult::Written {
                    durability: AtomicWriteDurability::Warning {
                        operation: AtomicWriteOperation::SyncParent,
                        errno: libc::EIO,
                    },
                },
            }
        ));
        let retained = attempts.clone();
        save(&signed_in());
        assert_eq!(
            fresh_write_attempts(),
            retained,
            "routine saves retain fresh diagnostics"
        );
        clear();
        assert!(fresh_write_attempts().is_empty());
    }

    #[test]
    fn fresh_save_errors_are_save_scoped_retained_and_cleared() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("fresh-save-errors");
        *COLD_STORAGE_DIAGNOSTIC
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(ColdStorageDiagnostic {
            errors: Vec::new(),
            candidate_reads: "internal:missing".into(),
            facts: ColdSessionFacts {
                winner: None,
                account_token_present: false,
                pms_token_present: false,
                server_dialable: false,
                can_go_local: false,
            },
        });
        crate::storage::inject_next_commit_failure_for_test(
            crate::storage::CommitStage::ParentSync,
        );
        assert_eq!(
            save_after_reauthentication(&signed_in()),
            PersistOutcome::WriteFailed
        );
        let errors = fresh_save_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(
            errors[0].candidate, None,
            "a save-wide failure invents no candidate"
        );
        assert_eq!(
            errors[0].context.stage,
            crate::telemetry::storage::StorageStage::WriteFailed
        );

        save(&signed_in());
        assert_eq!(
            fresh_save_errors(),
            errors,
            "routine saves retain fresh evidence"
        );
        clear();
        assert!(fresh_save_errors().is_empty());
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
        assert!(
            record_last_hero(second),
            "a genuinely different hero writes"
        );
        assert_eq!(last_hero(), Some(second), "…and replaces the stored one");
    }

    /// **Issue #76.** A pre-existing secure envelope this process cannot open (`load` never even
    /// gets a real client id out of it — `Locked` degrades to a fresh, ephemeral default, exactly
    /// the "takes longer than usual" + "sign in again" symptom the owner reported) must not shadow
    /// a FRESH sign-in forever. Once the PIN flow calls [`save_after_reauthentication`] with a real
    /// account credential, the locked ciphertext is replaced by the 0600 plaintext file — there is
    /// nothing in the old
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
                identity: crate::keymanager::Identity::Anonymous,
            },
        };
        let original = serde_json::to_vec_pretty(&envelope).unwrap();
        std::fs::write(t.file(), &original).unwrap();

        // A REFUSAL, not merely an unanswered service: `LOCKED_RECOVERABLE` (and the recovery
        // this test is about) is reached only when a key manager actually replies that it cannot
        // open the envelope — an unscripted default now means `LOCKED_UNAVAILABLE` instead, which
        // preserves the file rather than recovering it (see `open_failure_is_transient`).
        arm_refusing_keymanager();
        let loaded = load();
        crate::keymanager::disarm_for_test();
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
        save_after_reauthentication(&signed_in());
        let on_disk = canonical_session();
        assert_ne!(
            serde_json::to_vec(&on_disk).unwrap(),
            original,
            "a fresh sign-in must not be discarded to protect an envelope nobody can open"
        );
        assert_eq!(on_disk.account_token, "acct");
        let mode = std::fs::metadata(persistence::path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
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
                identity: crate::keymanager::Identity::Anonymous,
            },
        };
        std::fs::write(t.file(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();

        // The boot read fails to open the on-disk envelope — and it fails the way hypothesis 2
        // predicts, with a service that ANSWERS and refuses the key ("key not found"), rather
        // than one that never answered at all: only the former is evidence about the key, and
        // only the former reaches `LOCKED_RECOVERABLE` (see `open_failure_is_transient`).
        arm_refusing_keymanager();
        let loaded = load();
        crate::keymanager::disarm_for_test();
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

        save_after_reauthentication(&signed_in());
        crate::keymanager::disarm_for_test();

        let saved = canonical_session();
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
                identity: crate::keymanager::Identity::Anonymous,
            },
        };
        // Only the LOWER-priority candidate holds the envelope; the higher-priority one is
        // absent, so an unqualified "first writable candidate" would happily create it there.
        std::fs::write(t.lower(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        assert!(!t.higher().exists());

        arm_refusing_keymanager(); // a real refusal — see `open_failure_is_transient`
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(
            loaded.account_token.is_empty(),
            "Locked degrades to default"
        );

        save_after_reauthentication(&signed_in());

        assert!(
            !t.higher().exists(),
            "the recovery write must not land at the higher-priority candidate merely because \
             it was free — the next boot's read_locked would never reach the untouched locked \
             file at the lower-priority path if it did"
        );
        let saved = canonical_session();
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
                identity: crate::keymanager::Identity::Anonymous,
            },
        };
        std::fs::write(t.higher(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
        std::fs::write(t.lower(), b"leftover plaintext credentials").unwrap();

        arm_refusing_keymanager(); // a real refusal — see `open_failure_is_transient`
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(
            loaded.account_token.is_empty(),
            "Locked degrades to default"
        );

        save_after_reauthentication(&signed_in());

        assert!(
            !t.lower().exists(),
            "a stale copy at another candidate must not survive the recovery write"
        );
        let saved = canonical_session();
        assert_eq!(saved.account_token, "acct");
    }

    /// A historical client-id-only file at the preferred tier is valid plaintext, so it wins the
    /// cold read even when an older recognized envelope survives below it. A routine migration
    /// must preserve that envelope, but a later PIN reauthentication must be able to replace the
    /// selected plaintext and sweep the now-obsolete recognized copy instead of living for one run.
    #[test]
    fn fresh_reauthentication_supersedes_a_recognized_envelope_below_selected_plaintext() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("plaintext-above-recognized-envelope");
        std::fs::write(
            t.higher(),
            serde_json::to_vec_pretty(&Session {
                client_id: "cid-only".into(),
                ..Session::default()
            })
            .unwrap(),
        )
        .unwrap();
        std::fs::write(t.lower(), locked_envelope_bytes()).unwrap();

        let loaded = load();
        assert!(loaded.account_token.is_empty());
        assert!(
            !t.lower().exists(),
            "a durable canonical migration retires the recognized stale legacy envelope"
        );

        let mut fresh = signed_in();
        fresh.server.address = "192.0.2.10".into();
        fresh.server.port = 32400;
        fresh.server.token = "pms-token".into();
        assert!(save_after_reauthentication(&fresh).persisted());
        assert!(
            !t.lower().exists(),
            "the recognized stale envelope is swept"
        );

        super::redirect_for_test_multi(vec![t.higher(), t.lower()]);
        let next = load();
        assert_eq!(next.account_token, "acct");
        assert!(
            next.can_go_local(),
            "the next cold process passes the boot gate"
        );
    }

    #[test]
    fn selected_plaintext_probe_applies_secure_envelope_precedence_before_serde_defaults() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("selected-plaintext-shape");
        assert!(matches!(
            persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
            persistence::CanonicalCommit::Durable { .. }
        ));
        assert!(
            !selected_candidate_is_plaintext(),
            "Session defaults must not make a recognized envelope parse as plaintext"
        );
        assert!(matches!(
            persistence::commit_data(
                r#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#.into(),
            ),
            persistence::CanonicalCommit::Durable { .. }
        ));
        assert!(!selected_candidate_is_plaintext());
        assert!(matches!(
            persistence::commit_data(
                serde_json::to_string(&Session {
                    client_id: "cid-only".into(),
                    ..Session::default()
                })
                .unwrap(),
            ),
            persistence::CanonicalCommit::Durable { .. }
        ));
        assert!(selected_candidate_is_plaintext());
    }

    #[test]
    fn a_lower_unknown_envelope_survives_fresh_save_of_selected_plaintext() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("plaintext-above-unknown-envelope");
        std::fs::write(
            t.higher(),
            serde_json::to_vec_pretty(&Session {
                client_id: "cid-only".into(),
                ..Session::default()
            })
            .unwrap(),
        )
        .unwrap();
        let unknown = br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#;
        std::fs::write(t.lower(), unknown).unwrap();
        let _ = load();

        assert!(save_after_reauthentication(&signed_in()).persisted());
        assert_eq!(std::fs::read(t.lower()).unwrap(), unknown);
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
                identity: crate::keymanager::Identity::Anonymous,
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
                identity: crate::keymanager::Identity::Anonymous,
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
        let _t = TempSession::new("cache-fallback");
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
        let on_disk = canonical_session();
        assert_eq!(on_disk.account_token, "acct");
        let mode = std::fs::metadata(persistence::path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "credentials at rest even on the fallback path");
    }

    /// `clear()` (sign-out) must drop the in-process cache along with the file — a stale cached
    /// copy answering `peek` after a sign-out would mean the UI keeps showing the account that was
    /// just signed out of.
    #[test]
    fn clear_empties_the_cache_and_a_following_peek_reads_disk() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("clear-cache");
        save_after_reauthentication(&signed_in());
        assert_eq!(peek().account_token, "acct", "cached from the save above");
        assert!(last_fresh_save_readback().is_some());

        clear();
        assert_eq!(last_fresh_save_readback(), None);
        assert_eq!(last_persist_outcome(), None);
        assert_eq!(cold_session_facts(), None);
        assert_eq!(cold_storage_diagnostic(), None);
        assert!(last_candidate_reads().is_empty());
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

    /// **A session file that is ours and regular but widened to 0777 is repaired on read, not
    /// merely tolerated.** `read_owned_regular` backs the auth file, the telemetry decision file,
    /// the spool and every marker/probe — fixing it here fixes all of them at once. See
    /// `docs/measurements/credential-storage-native-apps-2026-09-10.md`: the device measurement
    /// this responds to.
    #[test]
    fn a_world_writable_owned_file_is_repaired_to_0600_on_read() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let _t = TempSession::new("repair-on-read");
        save(&signed_in());
        std::fs::set_permissions(persistence::path(), std::fs::Permissions::from_mode(0o777))
            .unwrap();

        let bytes =
            read_owned_regular(&persistence::path()).expect("owned regular file is still readable");
        assert!(!bytes.is_empty());

        let mode = std::fs::metadata(persistence::path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o777, "write-widened bytes are not repaired in place");
    }

    /// **Repairing the mode is not the same claim as trusting the content.** A write-widened
    /// session file (any of `0o022`) means another uid on the shared namespace could have rewritten
    /// the bytes — a token in there is no longer provably this account's — so it must never be
    /// parsed as a session: no session comes back, the file stops being at the name the next
    /// launch reads (moved aside since 2026-09-10, see the quarantine test below), and a
    /// storage-error report with the `UntrustedMode` stage is queued so a fleet can see this
    /// happened.
    #[test]
    fn a_write_widened_session_file_is_never_loaded_and_leaves_that_name() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("write-widened-session");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        let file = t.file();
        reset_report_state_for_test();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        // Simulate a fresh launch reading the same file — `save`'s own in-process cache must not
        // be what answers `load()` below, or this test would never touch the file at all.
        super::redirect_for_test(Some(t.file()));

        let loaded = load();
        assert!(
            loaded.account_token.is_empty(),
            "a write-widened file's token must never be trusted as this account's"
        );
        assert!(
            !untrusted_path(&file).is_none_or(|p| !p.exists()),
            "and the bytes themselves are kept aside rather than destroyed"
        );
        // `load()` mints a fresh anonymous session for the empty-`client_id` case and saves it —
        // so the file exists again afterward, but never carrying the forged token: the assertion
        // that matters is that the OLD account is gone, not that the path stays empty forever.
        if let Ok(on_disk) = std::fs::read_to_string(&file) {
            assert!(
                !on_disk.contains("acct"),
                "the untrusted file's forged token must never survive, even inside a later rewrite"
            );
        }

        let reports = captured_reports();
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert_eq!(
            reports[0].stage,
            crate::telemetry::storage::StorageStage::UntrustedMode
        );
    }

    /// **QUARANTINE, not deletion** (maintainer decision, 2026-09-10). The bytes of a
    /// write-widened session file are still evidence — of what was tampered with, and of what the
    /// owner's own sign-in used to hold — and destroying them leaves a television owner with a
    /// sign-in screen and nothing to look at. They are moved aside to `<name>.untrusted`, 0600,
    /// beside the file, and never parsed. Everything else about the branch is unchanged: no
    /// session comes back, `UntrustedMode` is reported, and the install falls to sign-in.
    #[test]
    fn a_write_widened_session_file_is_quarantined_rather_than_destroyed() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("quarantine-widened-session");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        let file = t.file();
        let forged = std::fs::read(&file).unwrap();
        reset_report_state_for_test();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        super::redirect_for_test(Some(t.file()));

        let loaded = load();
        assert!(
            loaded.account_token.is_empty(),
            "the forged token is never trusted"
        );

        let quarantine = untrusted_path(&file).unwrap();
        assert!(
            quarantine.exists(),
            "the bytes are kept for the owner to look at"
        );
        assert_eq!(
            std::fs::read(&quarantine).unwrap(),
            forged,
            "kept verbatim — the point is what was there"
        );
        assert_eq!(
            std::fs::metadata(&quarantine).unwrap().permissions().mode() & 0o777,
            0o600,
            "and no longer readable by the peer that widened it"
        );
        let reports = captured_reports();
        assert!(
            reports
                .iter()
                .any(|r| r.stage == crate::telemetry::storage::StorageStage::UntrustedMode),
            "{reports:?}"
        );
    }

    /// A quarantined copy belongs to the account that was signed in when it was made, so sign-out
    /// and Delete all local data take it with everything else. Leaving a former account's token
    /// bytes on a rooted television after a sign-out is not a sign-out.
    #[test]
    fn sign_out_removes_a_quarantined_copy() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("quarantine-cleared-on-signout");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        let file = t.file();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        super::redirect_for_test(Some(t.file()));
        let _ = load();
        let quarantine = untrusted_path(&file).unwrap();
        assert!(
            quarantine.exists(),
            "precondition: something was quarantined"
        );

        clear();

        assert!(
            !quarantine.exists(),
            "sign-out leaves no former account's bytes behind"
        );
    }

    /// **If the quarantine cannot be made, the file is destroyed exactly as before.** Keeping the
    /// evidence is worth doing and is worth nothing next to the rule it serves: a write-widened
    /// file must not still be sitting at the name the next launch reads. A peer that plants a
    /// DIRECTORY at the quarantine name is the concrete way the rename fails.
    #[test]
    fn a_quarantine_that_cannot_be_made_falls_back_to_deleting_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("quarantine-blocked");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        let file = t.file();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        let quarantine = untrusted_path(&file).unwrap();
        std::fs::create_dir_all(quarantine.join("blocker")).unwrap();
        super::redirect_for_test(Some(t.file()));

        let loaded = load();
        assert!(loaded.account_token.is_empty());
        // `load` mints and saves a fresh anonymous session, so the path may exist again — what
        // must never survive is the forged token.
        if let Ok(on_disk) = std::fs::read_to_string(&file) {
            assert!(
                !on_disk.contains("acct"),
                "the forged token must not survive"
            );
        }
        let _ = std::fs::remove_dir_all(&quarantine);
    }

    /// **A quarantine that could not be made 0600 is not a quarantine** (review finding,
    /// 2026-09-11). `quarantine_untrusted`'s doc argued the mode "is already 0600 by the time this
    /// runs" — true only where the `fchmod` SUCCEEDED, which `repair_owned_mode` never said. Where
    /// it did not (a read-only remount, a jail or LSM that denies the operation), the rename put a
    /// file still carrying a real account token, still world-WRITABLE, at a fixed, guessable name
    /// beside the session — a strictly worse outcome than the delete this branch always did,
    /// arrived at while trying to preserve evidence.
    ///
    /// The failing `fchmod` is SIMULATED (`FCHMOD_REFUSED`, a `#[cfg(test)]` hook): there is no
    /// portable way to make one fail on a file this process owns, which is the syscall's own
    /// contract. Everything else here is the real read path.
    #[test]
    fn a_write_widened_file_whose_mode_could_not_be_fixed_is_deleted_rather_than_kept() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("quarantine-unfixable-mode");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        let file = t.file();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();
        super::redirect_for_test(Some(t.file()));

        FCHMOD_REFUSED.store(true, std::sync::atomic::Ordering::Relaxed);
        let loaded = load();
        FCHMOD_REFUSED.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(
            loaded.account_token.is_empty(),
            "the forged token is never trusted"
        );

        let quarantine = untrusted_path(&file).unwrap();
        assert!(
            !quarantine.exists(),
            "a file still writable by others must not be preserved under ANY name — the evidence \
             is worth less than the token sitting in it at 0666"
        );
        if let Ok(on_disk) = std::fs::read_to_string(t.file()) {
            assert!(
                !on_disk.contains("acct"),
                "…and the forged token must not survive at the name the next launch reads either"
            );
        }
    }

    #[test]
    fn failed_untrusted_cleanup_cannot_promote_the_same_legacy_bytes_on_restart() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("untrusted-cleanup-restart");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        std::fs::set_permissions(t.file(), std::fs::Permissions::from_mode(0o666)).unwrap();
        let aside = untrusted_path(&t.file()).unwrap();
        std::fs::create_dir_all(aside.join("blocks-rename")).unwrap();
        fail_next_cleanup_remove_for_test();
        assert!(load().account_token.is_empty());
        if t.file().exists() {
            assert_ne!(
                std::fs::metadata(t.file()).unwrap().permissions().mode() & 0o022,
                0,
                "failed cleanup must leave attacker-writable bytes visibly untrusted"
            );
            let _ = std::fs::remove_file(persistence::path());
            super::redirect_for_test(Some(t.file()));
            assert!(load().account_token.is_empty(), "restart must not trust the old token");
        }
    }

    /// The read-only-widened twin of the test above: `0o644` carries no write bit, so the content
    /// is still provably this process's own and must load exactly as it always has, past the mode
    /// repair.
    #[test]
    fn a_read_only_widened_session_file_still_loads() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("readable-widened-session");
        save(&signed_in());
        std::fs::set_permissions(persistence::path(), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        super::redirect_for_test(Some(t.file()));

        let loaded = load();
        assert_eq!(
            loaded.account_token, "acct",
            "a read-only widened mode must not cost the session"
        );

        let mode = std::fs::metadata(persistence::path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");
    }

    /// **The trust verdict must be decided from the mode observed BEFORE the repair fixes it**, not
    /// from whatever the mode happens to be afterward — `repair_owned_mode` always leaves the file
    /// at 0600, so reading `meta` back off disk AFTER the call would see `0o600 & 0o077 == 0` and
    /// wrongly report `Trusted` no matter how wide the file had been. This calls the function
    /// directly with a `Metadata` snapshot taken before it runs (the same order every real caller
    /// uses — `file.metadata()` happens before `repair_owned_mode` is invoked) and checks the
    /// returned verdict, not a second stat.
    #[test]
    fn the_trust_verdict_is_decided_from_the_mode_before_repair_not_after() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-verdict-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f");
        std::fs::write(&path, b"x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        // The pre-repair snapshot: 0o666, write-widened.
        let meta = file.metadata().unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o666);

        let verdict = repair_owned_mode(&file, &meta, &path);
        assert_eq!(
            verdict,
            ModeTrust::Repaired {
                readable_only: false,
                fixed: false,
            },
            "the verdict must reflect the WRITE-widened mode captured before the fchmod, \
             even though the file is 0600 by the time this call returns"
        );
        let after = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(after, 0o666, "write-widened content is not repaired in place");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Ownership + mode checks — and the trust-vs-repair split above — can only tell a FORGED
    /// file apart from this install's own. They cannot tell a STALE-but-genuine file apart from the
    /// current one.** A peer with rename rights in the shared `/media/developer` directory (see the
    /// module doc's "Parent directories" section) can move this install's own valid, correctly-
    /// owned, correctly-0600 file aside, let a fresh sign-in write a new one, and later move the old
    /// bytes back — same owner, same mode, a session shape this build parses fine. This test PINS
    /// that as a known, undetected limitation (it is expected to pass, not to demonstrate a bug to
    /// fix): the replayed older file loads as though it were current, with no assertion in this
    /// module able to catch it.
    #[test]
    fn a_replayed_older_valid_session_file_is_indistinguishable_from_current() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("replay-limitation");

        save(&signed_in()); // account_token "acct"
        let old_payload = canonical_payload();

        // A fresh sign-in supersedes it — same install, same file, different account.
        super::redirect_for_test(Some(t.file()));
        save(&Session {
            client_id: "cid-2".into(),
            account_token: "different-acct".into(),
            ..Default::default()
        });
        assert_eq!(peek().account_token, "different-acct");

        // The replay replaces the current canonical record with an older valid record payload.
        // The RecordStore framing remains intact: this is a same-store replay, not a malformed
        // legacy JSON file planted at the canonical filename.
        assert!(matches!(
            persistence::commit_data(old_payload),
            persistence::CanonicalCommit::Durable { .. }
        ));

        super::redirect_for_test(Some(t.file()));
        let loaded = load();
        assert_eq!(
            loaded.account_token, "acct",
            "the replayed OLD file is indistinguishable from a current one — known limitation"
        );
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
                identity: crate::keymanager::Identity::Anonymous,
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
        arm_round_tripping_keymanager_bytes(&serde_json::to_vec_pretty(s).unwrap());
    }

    /// [`arm_round_tripping_keymanager`], generalized to any plaintext — Stage B1's probe seals
    /// [`PROBE_PLAINTEXT`], not a `Session`, so its own round-trip tests need the same shape of
    /// script without a `Session` to serialize.
    fn arm_round_tripping_keymanager_bytes(plain: &[u8]) {
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
                    "output": b64_encode_for_test(plain)
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
        let _t = TempSession::new("marker-write-on-locked-read");
        assert!(matches!(
            persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
            persistence::CanonicalCommit::Durable { .. }
        ));

        assert!(
            !has_refused_marker(),
            "nothing recorded before the first read"
        );
        arm_refusing_keymanager();
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(
            loaded.account_token.is_empty(),
            "Locked degrades to default"
        );
        assert!(
            has_refused_marker(),
            "read_locked's LOCKED_RECOVERABLE branch must persist the verdict"
        );
    }

    /// A locked read whose failure never reached a service REPLY — a refused LS2 registration
    /// (the unscripted default here) or a budget timeout — keeps the envelope and reports
    /// `LOCKED_UNAVAILABLE`, and the launch after it opens the same file untouched.
    ///
    /// **The middle launch asserted the opposite until 2026-09-10.** Issue #76's second review
    /// had it write the cross-launch marker, on the argument that an envelope this install wrote
    /// and cannot reopen is the failure class the marker remembers — but "cannot reopen" was
    /// being read off a service that never answered, and the cost was a healthy television
    /// downgraded to the 0600 file until sign-out over one stalled boot. The concern that review
    /// was actually defending (re-paying a dead service's budget every launch forever) is met by
    /// the bounded escalation instead — see `a_key_service_that_never_answers_is_finally_graded_a_refusal`.
    /// What is UNCHANGED, and is why this test still ends where it does: the state never gates
    /// READS, so launch 3 opens the envelope launch 1 sealed.
    #[test]
    fn an_unanswered_service_at_open_keeps_the_envelope_and_a_healthy_launch_still_reads() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-on-unreachable");
        mark_proven_for_test(); // this test is about a SEALED install, not about earning one

        // Launch 1: signs in while a keymanager3 that round-trips fine in-process seals the file.
        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            serde_json::from_str::<SecureEnvelope>(&canonical_payload()).is_ok(),
            "launch 1 ends with a genuinely sealed envelope on disk"
        );

        // Launch 2: fresh process state, same file — the unscripted default, i.e. the service
        // could not even be registered with.
        super::redirect_for_test(Some(t.file()));
        let loaded2 = load();
        assert!(
            loaded2.account_token.is_empty(),
            "this launch still can't open it"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNAVAILABLE,
            "the per-process read still protects THIS launch's file"
        );
        assert!(
            !has_refused_marker(),
            "…but nothing answered, so nothing has been proven about the key"
        );
        assert_eq!(
            unavailable_stage_for_test().as_deref(),
            Some("unreachable"),
            "the counter records WHICH way the open failed"
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

    /// **A TEMPORARY key-service failure must not cost a proven, sealed sign-in.** The stalled
    /// shape specifically (issue #75/#76's "slow, then try again" symptom): a `begin(decrypt)`
    /// that never answers inside its budget is recorded as `no_reply` — and `no_reply` is a fact
    /// about the SERVICE, not about the key.
    ///
    /// **This test asserted the opposite until 2026-09-10** (`…writes_the_marker_as_no_reply`),
    /// and the behaviour it pinned is the one under review: the marker is removed only by
    /// `clear()`, so one stalled boot permanently downgraded a healthy television to the 0600
    /// file, and the very next fresh sign-in wrote the credentials out in plain sight. What the
    /// old version was really defending — not re-paying a dead service's budget on every launch
    /// forever — is now `note_service_unavailable`'s bounded escalation, which reaches the same
    /// marker after `UNAVAILABLE_MAX_LAUNCHES` instead of on the first hiccup (see
    /// `a_key_service_that_never_answers_is_finally_graded_a_refusal`).
    #[test]
    fn a_service_that_never_answers_at_open_keeps_the_sealed_envelope() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("unavailable-on-no-reply");
        let original = locked_envelope_bytes();
        std::fs::write(t.file(), &original).unwrap();
        reset_report_state_for_test();
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let loaded = load();
        crate::keymanager::disarm_for_test();

        assert!(
            loaded.account_token.is_empty(),
            "this launch still has no session"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "the sealed sign-in is left byte-identical for a launch that CAN read it"
        );
        assert!(
            !has_refused_marker(),
            "nothing refused anything — arming the cross-launch marker here is the downgrade"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNAVAILABLE
        );
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::SecureUnavailable,
            "and it reports as its own class, not as a refusal"
        );
        // …and the launch is still REPORTED, with the stage that names the stall — the half of
        // the old behaviour that was right, and the one a dashboard reads.
        let stages: Vec<_> = captured_reports().iter().map(|c| c.stage).collect();
        assert_eq!(
            stages,
            vec![crate::telemetry::storage::StorageStage::NoReply]
        );
    }

    /// The report is owed **once per launch**, not once per open: the sign-in screen's *Try again*
    /// re-runs the whole read, and a person pressing it four times must not put four identical
    /// `StorageError`s in the spool. (`report_once`'s per-process-per-stage rule is what does it;
    /// this pins that the retry path really goes through it.)
    #[test]
    fn a_transient_failure_reports_once_however_often_try_again_is_pressed() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("unavailable-reports-once");
        assert!(matches!(
            persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
            persistence::CanonicalCommit::Durable { .. }
        ));
        reset_report_state_for_test();
        crate::keymanager::arm_for_test(vec![
            ("begin", Err(())),
            ("begin", Err(())),
            ("begin", Err(())),
        ]);
        let _ = load();
        assert!(!retry_secure_open(), "the service is still silent");
        assert!(!retry_secure_open());
        crate::keymanager::disarm_for_test();
        assert_eq!(
            captured_reports().len(),
            1,
            "one launch, one report — however many times the screen asked again"
        );
        assert_eq!(
            read_unavailable_attempts(),
            1,
            "and one launch spends exactly one of the install's three, not one per press"
        );
    }

    /// **The other half of the read-out: *Try again* that WORKS.** The service answers on the
    /// second ask, so the sealed session this launch booted without is published and the run
    /// carries on exactly as a healthy launch would — no sign-in, no rewrite of the file.
    #[test]
    fn a_later_successful_open_in_the_same_process_publishes_the_session() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("unavailable-then-opens");
        let plain = serde_json::to_vec_pretty(&signed_in()).unwrap();
        let original = locked_envelope_bytes();
        std::fs::write(t.file(), &original).unwrap();
        reset_report_state_for_test();

        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let booted = load();
        crate::keymanager::disarm_for_test();
        assert!(
            booted.account_token.is_empty(),
            "the boot read found nothing to use"
        );
        assert!(secure_unavailable(), "…so the screen offers Try again");

        // The press. `retry_secure_open` must bypass CACHE — `load` published the ephemeral
        // default above, and a cached answer would report "still nothing" without ever asking.
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
        assert!(retry_secure_open(), "the service answered this time");
        crate::keymanager::disarm_for_test();

        assert_eq!(
            peek().account_token,
            "acct",
            "the recovered session is what every later reader sees"
        );
        assert!(!secure_unavailable());
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::Secure
        );
        assert_eq!(
            canonical_payload().as_bytes(),
            original.as_slice(),
            "a successful read rewrites nothing"
        );
        assert_eq!(
            read_unavailable_attempts(),
            0,
            "the run of unanswered launches is over, so its counter goes"
        );
    }

    /// **Only a COMPLETED sign-in replaces the sealed envelope** — and, since the 0.6.3 field
    /// report, one always does. An `update()` (a roster refresh, a pin) never touches a file it
    /// could not read; a `save` carrying fresh credentials does, because the user has just
    /// re-supplied everything the ciphertext held and a sign-in nobody can read back is the worst
    /// outcome available. Which SHAPE that save takes still depends on the service: answering
    /// again means a fresh envelope (a transient failure costs the install nothing), and still
    /// silent means the 0600 recovery file rather than a sign-in that evaporates at the next
    /// launch.
    ///
    /// **The middle leg asserted the opposite until 0.6.4.** It read `!save(&signed_in())` — the
    /// ciphertext preserved, the sign-in kept in memory only — which is precisely the reporter's
    /// loop: the account name shows at the top of Home, and the next launch asks for the QR code
    /// again, against the same envelope and the same silence, forever.
    #[test]
    fn only_a_completed_sign_in_replaces_the_envelope_and_never_as_plaintext() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("unavailable-sign-in");
        mark_proven_for_test(); // a sealed install, not one earning sealing
        let original = locked_envelope_bytes();
        std::fs::write(t.file(), &original).unwrap();
        reset_report_state_for_test();

        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let _ = load();
        assert!(
            !update(|s| Some(Session {
                account_token: "roster-refresh".into(),
                ..s.clone()
            })),
            "an unrelated writer never touches a file it could not read"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "the sealed sign-in survives an unrelated writer"
        );
        assert!(
            save_after_reauthentication(&signed_in()).persisted(),
            "but a sign-in made while the service is still silent must still land somewhere a later launch can read"
        );
        crate::keymanager::disarm_for_test();
        let recovered = canonical_payload();
        assert_ne!(
            recovered.as_bytes(),
            original.as_slice(),
            "…which means the dead envelope makes way for it"
        );
        assert_eq!(
            serde_json::from_str::<Session>(&recovered)
                .unwrap()
                .account_token,
            "acct",
            "and it is the 0600 plaintext file, the only shape this launch can write"
        );

        // The service comes back — the ordinary case, since a QR sign-in takes a minute. A proven
        // install seals again from here: the recovery above is a fallback, not a downgrade that
        // sticks.
        arm_round_tripping_keymanager(&signed_in());
        assert!(
            save_after_reauthentication(&signed_in()).persisted(),
            "now the completed sign-in lands"
        );
        crate::keymanager::disarm_for_test();
        let after = canonical_payload();
        assert_ne!(
            after, recovered,
            "…replacing the file it could not seal before"
        );
        assert!(
            serde_json::from_str::<SecureEnvelope>(&after).is_ok(),
            "and it is a fresh ENVELOPE — a transient failure never costs this install its encryption at rest"
        );
    }

    /// **The 0.6.3 defect, exactly as reported: a fresh sign-in over an envelope the key service
    /// would not answer for was kept in memory only, so the next launch asked for the QR code
    /// again.**
    ///
    /// The ordinary upgrade shape — a recognized v1 envelope written by 0.6.2, an install that has
    /// never EARNED sealed storage (nothing has ever reopened an envelope on it, so there is no
    /// proven marker), and a key service that does not answer this launch. `read_locked` grades
    /// that `LOCKED_UNAVAILABLE` and keeps the ciphertext byte-identical, which is right: nothing
    /// has been proven about the key. What was not right is what happened next — `save_locked`'s
    /// "not yet proven, but a secure file is present" dead end refused the write, planted a probe
    /// and returned, so the sign-in lived for exactly one run. Same envelope, same silence, same
    /// screen, forever; the reporter's words were "account name shows at the top, but after a
    /// restart I must sign in again".
    ///
    /// The rule this pins is `recover_locked_session_as_plaintext`'s own, applied one state
    /// wider: the user has just re-supplied everything the ciphertext held, and a sign-in nobody
    /// can read back is the worst outcome available.
    #[test]
    fn a_fresh_sign_in_over_an_unanswered_envelope_survives_the_next_launch() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("unavailable-fresh-sign-in");
        let original = locked_envelope_bytes();
        std::fs::write(t.file(), &original).unwrap();
        reset_report_state_for_test();

        // Launch 1: the service never answers, so the envelope is kept and this run has no
        // session — the "takes longer than usual, then sign in again" symptom.
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let booted = load();
        assert!(
            booted.account_token.is_empty(),
            "the boot read found nothing it could use"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNAVAILABLE
        );
        assert!(
            !has_proven_marker(),
            "an install upgraded from 0.6.2 has never earned sealed storage"
        );
        let cold = cold_storage_diagnostic().expect("the cold failure is retained");
        assert_eq!(cold.errors.len(), 1);
        assert_eq!(cold.errors[0].candidate, CandidateCategory::Other);
        assert_eq!(cold.candidate_reads, "other:secure");

        // The user signs in again, and the service is STILL silent — so there is no envelope to
        // be written and the 0600 file is the only place left.
        assert_eq!(
            save_after_reauthentication(&signed_in()),
            PersistOutcome::PersistedPlaintext,
            "a completed sign-in must be persisted somewhere a later launch can read it"
        );
        assert_eq!(
            last_persist_outcome(),
            Some(PersistOutcome::PersistedPlaintext),
            "…and the process-wide record says so"
        );
        assert_eq!(
            last_fresh_save_readback(),
            Some(FreshSaveReadback {
                winner: CandidateCategory::AppDir,
                result: FreshSaveReadbackResult::Match,
            }),
            "the bytes just committed are read directly from the winning file, not CACHE"
        );
        assert_eq!(
            cold_storage_diagnostic(),
            Some(cold),
            "a fresh save adds readback evidence without replacing the original cold failure"
        );
        crate::keymanager::disarm_for_test();

        let on_disk = std::fs::read(persistence::path()).unwrap();
        assert_ne!(
            on_disk, original,
            "an envelope nobody on this install can open must not shadow a fresh sign-in"
        );
        let payload = match persistence::load() {
            persistence::CanonicalRead::Data { payload, .. } => payload,
            _ => panic!("the canonical recovery record is missing"),
        };
        assert_eq!(
            serde_json::from_str::<Session>(&payload)
                .expect("the recovery payload is plaintext, not sealed")
                .account_token,
            "acct"
        );
        assert_eq!(
            std::fs::metadata(persistence::path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the recovery write is credentials at rest too"
        );

        // Launch 2: a new process against the same files, the service no healthier than before —
        // the launch that used to land back on the QR screen.
        super::redirect_for_test(Some(t.file()));
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let next = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            next.account_token, "acct",
            "the sign-in survives a reboot, which is the whole point"
        );
    }

    /// A refused marker can outlive the service failure that produced it. When the service opens
    /// the stored envelope on a later launch, a cached account token is still only a cached token:
    /// the no-network "already active" profile choice hands that same Session through
    /// `auth::take_ready`, but nobody has authorized a new QR code. It must not be mistaken for a
    /// fresh reauthentication and used to replace the now-readable ciphertext with plaintext.
    #[test]
    fn a_routine_save_of_a_reopened_session_does_not_spend_fresh_reauthentication_authority() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("cached-token-is-not-reauthentication");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::AppId);
        write_refused_marker(crate::telemetry::storage::StorageStage::BeginDecrypt, None);
        arm_opening_keymanager_for(&serde_json::to_vec_pretty(&signed_in()).unwrap());
        crate::keymanager::arm_identity_for_test(false, true, false);

        let cached = load();
        let original = std::fs::read(persistence::path()).unwrap();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            cold_session_facts(),
            Some(ColdSessionFacts {
                winner: Some(CandidateCategory::Other),
                account_token_present: true,
                pms_token_present: false,
                server_dialable: false,
                can_go_local: false,
            }),
            "cold facts describe the reopened file before a save can rewrite the story"
        );
        assert_eq!(
            cached.account_token, "acct",
            "the prior session reopened normally"
        );
        assert_eq!(
            save(&cached),
            PersistOutcome::PreservedExistingSecure(PreserveReason::RefusedMarkerNoFreshSignIn)
        );
        assert_eq!(
            std::fs::read(persistence::path()).unwrap(),
            original,
            "a routine profile handoff must leave the readable envelope byte-identical"
        );
    }

    #[test]
    fn reauthentication_authority_without_an_account_credential_cannot_erase_an_envelope() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("empty-reauthentication-cannot-recover");
        let original = locked_envelope_bytes();
        std::fs::write(t.file(), &original).unwrap();
        reset_report_state_for_test();
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let _ = load();

        assert_eq!(
            save_after_reauthentication(&Session {
                client_id: "cid-only".into(),
                ..Session::default()
            }),
            PersistOutcome::PreservedExistingSecure(PreserveReason::NotProven)
        );
        assert_eq!(
            last_fresh_save_readback(),
            None,
            "a failed fresh save must not retain an earlier attempt's successful readback"
        );
        crate::keymanager::disarm_for_test();
        assert_eq!(std::fs::read(t.file()).unwrap(), original);
    }

    /// The same defect through its OTHER door, and the one with no exit at all before 0.6.4: the
    /// envelope names an LS2 identity this launch cannot obtain
    /// (`StorageStage::IdentityUnavailable`). That read is `LOCKED_UNAVAILABLE` like a silent
    /// service, but deliberately shares none of the bounded launch counter and therefore NEVER
    /// escalates to a refusal — see `many_identity_unavailable_launches_never_escalate_to_a_refusal`,
    /// which is the rule that must not change. So a television whose bus never grants the sealing
    /// name (the webOS 4.x anonymous-forever case) could re-sign-in every launch for the rest of
    /// the install's life: the escalation that eventually rescued the silent-service case simply
    /// never arrives here.
    #[test]
    fn a_fresh_sign_in_survives_when_the_launch_cannot_obtain_the_sealing_identity() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-unavailable-fresh-sign-in");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::AppId);
        let original = std::fs::read(t.file()).unwrap();
        reset_report_state_for_test();

        // A key manager that would open anything it was asked to — the point being that it is
        // never asked, because this launch cannot register under the name the envelope records.
        arm_opening_keymanager_for(&serde_json::to_vec_pretty(&signed_in()).unwrap());
        crate::keymanager::arm_identity_for_test(false, false, false);
        let booted = load();
        crate::keymanager::disarm_for_test();
        assert!(booted.account_token.is_empty());
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNAVAILABLE
        );
        assert!(
            captured_reports()
                .iter()
                .any(|r| r.stage == crate::telemetry::storage::StorageStage::IdentityUnavailable),
            "the read really did end on the identity door: {:?}",
            captured_reports()
                .iter()
                .map(|r| r.stage)
                .collect::<Vec<_>>()
        );

        assert!(
            save_after_reauthentication(&signed_in()).persisted(),
            "a name this launch cannot get must not cost the user their sign-in every launch"
        );
        assert_ne!(canonical_payload().as_bytes(), original.as_slice());

        super::redirect_for_test(Some(t.file()));
        assert_eq!(
            load().account_token,
            "acct",
            "and the next launch reads it back without asking anybody for a name"
        );
    }

    /// **Every [`PersistOutcome`] and [`PreserveReason`] carries a stable wire word, and no two
    /// share one.** These words are a vocabulary, not a debug rendering: the event log's
    /// `session: persist outcome=…` line is written in them, and the telemetry layer pins the same
    /// strings from its own side. A rename that looks like tidying here silently splits whatever
    /// groups by them, and nothing else in the build would notice.
    ///
    /// `wire()` is exhaustive by construction, so a NEW variant cannot be added without giving it
    /// a word; what this adds is that the words themselves do not move, and that `persisted()`
    /// agrees with them about which two variants actually reached the disk.
    #[test]
    fn every_persist_outcome_and_reason_round_trips_its_wire_word() {
        let outcomes = [
            (
                PersistOutcome::PersistedPlaintext,
                "persisted_plaintext",
                true,
            ),
            (PersistOutcome::PersistedSealed, "persisted_sealed", true),
            (
                PersistOutcome::PreservedExistingSecure(PreserveReason::NotProven),
                "preserved_existing_secure",
                false,
            ),
            (
                PersistOutcome::PreservedExistingSecure(PreserveReason::RefusedMarkerNoFreshSignIn),
                "preserved_existing_secure",
                false,
            ),
            (
                PersistOutcome::PreservedExistingSecure(PreserveReason::SealFailed),
                "preserved_existing_secure",
                false,
            ),
            (
                PersistOutcome::BlockedUnknownEnvelope,
                "blocked_unknown_envelope",
                false,
            ),
            (PersistOutcome::WriteFailed, "write_failed", false),
            (
                PersistOutcome::SerializationFailed,
                "serialization_failed",
                false,
            ),
        ];
        for (outcome, wire, persisted) in outcomes {
            assert_eq!(outcome.wire(), wire, "{outcome:?}");
            assert_eq!(
                outcome.persisted(),
                persisted,
                "{outcome:?} disagrees with its word about reaching the disk"
            );
        }
        let mut words: Vec<&str> = outcomes.iter().map(|(_, w, _)| *w).collect();
        words.sort_unstable();
        words.dedup();
        assert_eq!(
            words.len(),
            6,
            "six distinct outcome words for eight cases — the three preserve reasons share one: {words:?}"
        );

        let reasons = [
            (PreserveReason::NotProven, "not_proven"),
            (
                PreserveReason::RefusedMarkerNoFreshSignIn,
                "refused_marker_no_fresh_sign_in",
            ),
            (PreserveReason::SealFailed, "seal_failed"),
        ];
        for (reason, wire) in reasons {
            assert_eq!(reason.wire(), wire, "{reason:?}");
            assert_eq!(
                PersistOutcome::PreservedExistingSecure(reason).reason_wire(),
                Some(wire),
                "the outcome must hand back its own reason's word"
            );
        }
        let mut reason_words: Vec<&str> = reasons.iter().map(|(_, w)| *w).collect();
        reason_words.sort_unstable();
        reason_words.dedup();
        assert_eq!(
            reason_words.len(),
            reasons.len(),
            "two preserve reasons cannot share one word"
        );
        assert_eq!(
            PersistOutcome::WriteFailed.reason_wire(),
            None,
            "only the preserving variant has a reason at all"
        );
    }

    /// The three preserving branches each report their OWN reason, so a log line says which rule
    /// kept the file rather than only that something did — the distinction the old bare `false`
    /// erased, and the reason the 0.6.4 defect read as "nothing needed persisting".
    #[test]
    fn each_preserving_branch_names_the_rule_that_kept_the_file() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("preserve-reasons");

        // NotProven: a secure file is present and this install has never earned sealing. No fresh
        // sign-in is involved, so the recovery this branch now allows does not apply.
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        assert_eq!(
            save(&Session {
                client_id: "cid-1".into(),
                ..Default::default()
            }),
            PersistOutcome::PreservedExistingSecure(PreserveReason::NotProven)
        );

        // SealFailed: proven, so `keymanager::seal` is asked — and nothing is armed, so it fails.
        // Again with no fresh credentials to justify replacing the ciphertext.
        mark_proven_for_test();
        assert_eq!(
            save(&Session {
                client_id: "cid-1".into(),
                ..Default::default()
            }),
            PersistOutcome::PreservedExistingSecure(PreserveReason::SealFailed)
        );

        // RefusedMarkerNoFreshSignIn: a PRIOR launch recorded the refusal, and this save carries
        // no credentials of its own — an ordinary `update()` must never convert a present envelope.
        write_refused_marker(crate::telemetry::storage::StorageStage::BeginDecrypt, None);
        assert_eq!(
            save(&Session {
                client_id: "cid-1".into(),
                ..Default::default()
            }),
            PersistOutcome::PreservedExistingSecure(PreserveReason::RefusedMarkerNoFreshSignIn)
        );
    }

    /// **A service that is silent for good still settles.** The bounded counterpart to the tests
    /// above, and the reason keeping the envelope is not simply an endless sign-in loop by another
    /// name: after `UNAVAILABLE_MAX_LAUNCHES` launches that all end unanswered, the install is
    /// graded a genuine refusal — the old first-launch behaviour, reached on evidence instead of
    /// on a hiccup — and the next fresh sign-in recovers to the 0600 file for good.
    #[test]
    fn a_key_service_that_never_answers_is_finally_graded_a_refusal() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("unavailable-escalates");
        let original = locked_envelope_bytes();
        std::fs::write(t.file(), &original).unwrap();
        reset_report_state_for_test();

        for launch in 1..UNAVAILABLE_MAX_LAUNCHES {
            super::redirect_for_test(Some(t.file())); // a new launch against the same files
            let _ = load();
            assert!(
                !has_refused_marker(),
                "launch {launch} proved nothing about the key"
            );
            assert_eq!(read_unavailable_attempts(), launch);
        }

        super::redirect_for_test(Some(t.file()));
        let _ = load();
        assert!(
            has_refused_marker(),
            "a service unanswered across {UNAVAILABLE_MAX_LAUNCHES} launches is no longer distinguishable from one that never will answer"
        );
        assert_eq!(marker_stage_for_test().as_deref(), Some("unreachable"));
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_RECOVERABLE,
            "…and from here it is today's recoverable path"
        );
        assert_eq!(
            read_unavailable_attempts(),
            0,
            "the counter has answered its question and is removed"
        );
        assert!(
            save_after_reauthentication(&signed_in()).persisted(),
            "so a fresh sign-in can finally land"
        );
        assert_ne!(canonical_payload().as_bytes(), original.as_slice());
    }

    /// **A REFUSAL is still a refusal.** The tag mismatch a foreign key produces (`-20030`, the
    /// shape issue #76's own per-launch-key hypothesis predicts) reached a key manager and got a
    /// real answer, so it takes exactly the path it always did: the cross-launch marker, the
    /// recoverable lock, and `SecureRefused`. Nothing about the unavailable state loosens this.
    #[test]
    fn a_decrypt_the_service_refuses_still_takes_todays_locked_path() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("tag-mismatch-still-locked");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        reset_report_state_for_test();
        crate::keymanager::arm_for_test(vec![(
            "begin",
            Ok(serde_json::json!({
                "returnValue": false, "errorCode": -20030, "errorText": "verification failed"
            })),
        )]);
        let _ = load();
        crate::keymanager::disarm_for_test();

        assert!(has_refused_marker(), "the service answered, and it said no");
        assert_eq!(marker_stage_for_test().as_deref(), Some("begin_decrypt"));
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_RECOVERABLE
        );
        assert!(!secure_unavailable());
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::SecureRefused
        );
        assert_eq!(read_unavailable_attempts(), 0, "and no launch was counted");
    }

    /// The classification itself, as the pure rule the two paths above hang off.
    #[test]
    fn only_a_service_that_never_answered_is_graded_transient() {
        use crate::keymanager::LastRefusal;
        use crate::telemetry::storage::StorageStage;
        let r = |stage, error_code| LastRefusal { stage, error_code };
        // Nothing that owns a key ever looked at ours.
        assert!(open_failure_is_transient(r(StorageStage::NoReply, None)));
        assert!(open_failure_is_transient(r(
            StorageStage::Unreachable,
            None
        )));
        assert!(
            open_failure_is_transient(r(StorageStage::BeginDecrypt, Some(-1))),
            "the hub's own 'Service does not exist' — measured on the 4.10 dev set"
        );
        // …versus a key manager that answered about the key.
        assert!(!open_failure_is_transient(r(
            StorageStage::BeginDecrypt,
            Some(-10001)
        )));
        assert!(!open_failure_is_transient(r(
            StorageStage::BeginDecrypt,
            Some(-20030)
        )));
        assert!(!open_failure_is_transient(r(
            StorageStage::FinishDecrypt,
            None
        )));
        assert!(
            !open_failure_is_transient(r(StorageStage::EnvelopeLocked, None)),
            "the interim AES-CFB envelope is refused by policy, not by a stalled service"
        );
    }

    /// Stage B1 test helper: plant the proven marker directly, bypassing a real probe cycle — for
    /// tests below whose whole point is what happens ONCE an install is proven, not how it got
    /// there (that is `a_probe_that_opens_on_the_next_launch_promotes_the_install_to_sealed_storage`
    /// and its neighbours, further down). Every test here that calls this predates Stage B1 and
    /// used to reach the same state by a `seal`/`round_trips` call that has since stopped being
    /// enough on its own.
    fn mark_proven_for_test() {
        let marker = proven_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            br#"{"proven_at_version":"0.0.0-test","stage":"probe_opened"}"#,
        )
        .unwrap();
    }

    /// [`marker_stage_for_test`] for the unanswered-launch counter — the same field, on the
    /// marker that records a service that never answered rather than one that refused.
    fn unavailable_stage_for_test() -> Option<String> {
        unavailable_marker_paths().into_iter().find_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            v.get("stage")?.as_str().map(str::to_string)
        })
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

        let saved = canonical_session();
        assert_eq!(saved.account_token, "acct");
        let mode = std::fs::metadata(persistence::path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the marker-gated write is credentials at rest too"
        );
    }

    /// (c) `clear()` removes the marker along with the session, and a LATER save on the same
    /// install is free to seal again — a different account, or the same one signing back in,
    /// deserves a fresh chance rather than inheriting a stale verdict forever.
    #[test]
    fn clear_removes_the_marker_and_a_later_save_seals_again() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("marker-cleared-on-signout");
        // This install had already earned sealed storage — but on THIS launch its own envelope
        // failed to reopen, arming a refused marker beside the proven one. Issue #76 review
        // (should-fix): that specific coexistence is what makes the proven marker stale too (see
        // `clear`'s own doc) — a device that just proved it cannot trust its own real envelope is
        // not a device a fresh sign-in should re-seal onto immediately.
        mark_proven_for_test();
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
        assert!(
            !has_proven_marker(),
            "a proven marker recorded alongside a refused one is stale too — the very envelope \
             this install proved it could seal failed to reopen, so the next sign-in must re-earn \
             sealed storage through the probe rather than trusting the same fact immediately again"
        );

        // A fresh sign-in now goes through the ordinary unproven path: plaintext first, with a
        // probe planted for the NEXT launch to check — exactly like any other install that has
        // never earned sealed storage, never straight back to a real seal.
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        let on_disk = canonical_payload();
        assert_eq!(
            serde_json::from_str::<Session>(&on_disk)
                .expect("plaintext until the probe re-proves this install")
                .account_token,
            "acct"
        );
        assert!(
            probe_paths().iter().any(|p| p.exists()),
            "a probe was planted for the next launch to re-earn proven storage"
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
        // This sequence is about the REFUSED marker's cross-launch lifecycle, which Stage B1 layers
        // on top of, not about earning proven storage in the first place — see
        // `a_probe_that_opens_on_the_next_launch_promotes_the_install_to_sealed_storage` for that.
        mark_proven_for_test();

        // Launch 1: signs in while a keymanager3 that round-trips fine in-process seals the file.
        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            serde_json::from_str::<SecureEnvelope>(&canonical_payload()).is_ok(),
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
        save_after_reauthentication(&signed_in()); // the reported loop: sign in again
        let s2 = canonical_session();
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
        assert!(
            wrote,
            "an ordinary roster-refresh-shaped update still writes"
        );
        let calls = crate::keymanager::calls_for_test();
        crate::keymanager::disarm_for_test();
        assert!(
            calls.is_empty(),
            "launch 3 must not re-seal — the marker from launch 2 must still gate it"
        );
        let s3 = canonical_session();
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
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::Plaintext
        );
    }

    /// (g) A save that genuinely seals reports `Secure`.
    #[test]
    fn storage_class_reports_secure_after_a_sealing_save() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("class-secure");
        mark_proven_for_test(); // sealing at all requires an earned install — see Stage B1
        reset_report_state_for_test();
        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert_eq!(
            storage_class(),
            crate::telemetry::storage::SessionStorageClass::Secure
        );
    }

    /// (h) A locked-recoverable read always writes the marker before this function can even be
    /// asked, so the live verdict is `SecureRefused` — the more definitive of the two, per
    /// `storage_class`'s own doc — not merely `SecureLocked`.
    #[test]
    fn storage_class_reports_secure_refused_after_a_locked_recoverable_read() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("class-locked-recoverable");
        assert!(matches!(
            persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
            persistence::CanonicalCommit::Durable { .. }
        ));
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
        assert_eq!(
            reports[0].stage,
            crate::telemetry::storage::StorageStage::BeginDecrypt
        );
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

    /// **Stage B2 item 1 (issue #76 field report case 5, ported).** The pre-B2 `report_once` marked
    /// a stage `REPORTED_STAGES` unconditionally, BEFORE it even knew whether
    /// `telemetry::storage::report_error` sent, deferred or dropped it — so a stage the consent
    /// question genuinely REFUSED (a stored "No", the real terminal answer, not merely "not asked
    /// yet") was recorded exactly like one that had gone out, in the SAME set a sent-or-held report
    /// lives in. That is the shape this test pins apart: a real "No" must be tracked in its own set
    /// (`DROPPED_STAGES`) — never reported, but also never retried on every later occurrence of the
    /// identical failure — while an UNANSWERED consent state defers the report and is correctly
    /// marked reported (a deferred report is `telemetry::storage`'s own replay to resolve, not a
    /// reason for this module to ask again).
    #[test]
    fn field_5_a_dropped_report_is_tracked_apart_from_a_reported_one_and_never_retried() {
        let _g = crate::testlock::serial();
        reset_report_state_for_test();

        // A real "No": the Errors channel's own consent question, already answered.
        crate::telemetry::consent::install(crate::telemetry::consent::Consent {
            asked_version: crate::telemetry::consent::POLICY_VERSION,
            errors: false,
            ..Default::default()
        });
        let dropped_stage = crate::telemetry::storage::StorageStage::BeginDecrypt;
        let dropped_ctx = crate::telemetry::storage::StorageErrorContext {
            stage: dropped_stage,
            service_error_code: None,
            class: crate::telemetry::storage::SessionStorageClass::SecureRefused,
            refused_marker: true,
            key_outcome: None,
            registered_with_app_id: false,
            registered_with_name: false,
            sealed_identity: None,
        };
        report_once(PendingReport {
            context: dropped_ctx,
            candidate_reads: None,
            candidate: None,
        });
        assert_eq!(captured_reports().len(), 1, "one attempt was made");
        assert!(
            !REPORTED_STAGES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&dropped_stage),
            "a dropped report must never read as one that was sent or held"
        );
        assert!(
            DROPPED_STAGES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&dropped_stage),
            "a dropped report must be tracked in its own set"
        );

        // The identical failure occurs again (another save hitting the same refusal) — it must not
        // be retried every occurrence.
        report_once(PendingReport {
            context: dropped_ctx,
            candidate_reads: None,
            candidate: None,
        });
        assert_eq!(
            captured_reports().len(),
            1,
            "a dropped stage must not be re-attempted on every later occurrence"
        );

        // A DIFFERENT stage, found while the question is still open (unanswered, not refused),
        // is DEFERRED rather than dropped — and a deferred attempt IS marked reported, since
        // `telemetry::storage`'s own replay (not this module) is what resolves it later.
        crate::telemetry::consent::install(crate::telemetry::consent::Consent::default());
        let deferred_stage = crate::telemetry::storage::StorageStage::EnvelopeLocked;
        report_once(PendingReport {
            context: crate::telemetry::storage::StorageErrorContext {
                stage: deferred_stage,
                service_error_code: None,
                class: crate::telemetry::storage::SessionStorageClass::SecureRefused,
                refused_marker: true,
                key_outcome: None,
                registered_with_app_id: false,
                registered_with_name: false,
                sealed_identity: None,
            },
            candidate_reads: None,
            candidate: None,
        });
        assert_eq!(
            captured_reports().len(),
            2,
            "the second, different stage was attempted"
        );
        assert!(
            REPORTED_STAGES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&deferred_stage),
            "a deferred report is spoken for and must read as reported"
        );
        assert!(!DROPPED_STAGES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&deferred_stage));

        // Drain `telemetry::storage`'s own DEFERRED queue — a crate-global the deferred report
        // above was really pushed into (this module's `report_storage_error` calls the real
        // `telemetry::storage::report_error` in every build, Stage B2), which every later test in
        // that module's own suite (`crate::testlock::serial()`-guarded, same lock this test holds)
        // asserts a specific length of. `replay_deferred` empties it unconditionally regardless of
        // consent, since no dev build carries a Sentry endpoint to actually send through.
        crate::telemetry::storage::replay_deferred();
        crate::telemetry::consent::install(crate::telemetry::consent::Consent::default());
    }

    /// (k) A save whose `keymanager::seal` fails reports the handled error with THE KEYMANAGER'S
    /// OWN stage and code — `BeginEncrypt` here, not a generic "seal failed".
    #[test]
    fn a_seal_failure_reports_once_with_the_keymanagers_stage_and_code() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("report-seal-failure");
        mark_proven_for_test(); // a real seal attempt (not a probe) requires an earned install
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
        assert_eq!(
            reports[0].stage,
            crate::telemetry::storage::StorageStage::BeginEncrypt
        );
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
        assert_eq!(
            mode, 0o600,
            "the marker is written through the same 0600 atomic path"
        );
    }

    /// **A forged, write-widened `secure-storage.refused` marker must not disable sealing.** If
    /// another uid on the shared namespace could plant this file, it must be treated as though it
    /// were never there at all — and removed, so it cannot keep fooling every later boot either.
    #[test]
    fn a_write_widened_refused_marker_is_ignored_and_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("forged-refused-marker");
        let marker = refused_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            br#"{"refused_at_version":1,"reason":"envelope_unopenable"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();

        assert!(
            !has_refused_marker(),
            "a write-widened refused marker must be ignored, not honoured"
        );
        assert!(
            !marker.exists(),
            "an ignored, untrusted marker must also be deleted"
        );
        let _ = t;
    }

    /// The read-only twin: `0o644` never let another uid rewrite the marker, so its content is
    /// still trustworthy and must be honoured exactly as it always has been.
    #[test]
    fn a_read_only_widened_refused_marker_is_still_honoured() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("readable-refused-marker");
        let marker = refused_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            br#"{"refused_at_version":1,"reason":"envelope_unopenable"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(
            has_refused_marker(),
            "a read-only widened marker is still ours to trust"
        );
        let mode = std::fs::metadata(&marker).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");
        let _ = t;
    }

    /// **A forged, write-widened `secure-storage.proven` marker must not promote sealing.** A
    /// proven marker is what lets `save_locked` start calling `keymanager::seal` for the real
    /// session — a forged one planted by another uid must never grant that.
    #[test]
    fn a_write_widened_proven_marker_is_ignored_and_deleted() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("forged-proven-marker");
        let marker = proven_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            br#"{"proven_at_version":1,"stage":"probe_opened"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();

        assert!(
            !has_proven_marker(),
            "a write-widened proven marker must be ignored, not honoured"
        );
        assert!(
            !marker.exists(),
            "an ignored, untrusted marker must also be deleted"
        );
        let _ = t;
    }

    /// **Regression for the review finding (2026-09-10): the PRODUCTION seal gate itself, not
    /// merely `has_proven_marker`, must refuse a forged proven marker.** Before the fix,
    /// `proven_marker_identity` — what `save_locked` actually reads through
    /// `proven_for_this_launch` — read the marker trust-blind, so a write-widened marker still
    /// promoted an unproven install to sealing even though `has_proven_marker` (unreachable from
    /// production) said no.
    #[test]
    fn a_write_widened_proven_marker_does_not_promote_a_save_to_sealing() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let _t = TempSession::new("forged-proven-marker-save-gate");
        let marker = proven_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            br#"{"proven_at_version":"0.0.0-test","stage":"probe_opened","identity":"anonymous"}"#,
        )
        .unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();
        crate::keymanager::arm_identity_for_test(true, false, false);

        assert!(
            save(&signed_in()).persisted(),
            "the save itself still succeeds"
        );
        crate::keymanager::disarm_for_test();

        let on_disk = canonical_payload();
        assert!(
            serde_json::from_str::<SecureEnvelope>(&on_disk).is_err(),
            "a forged proven marker must not promote this install to sealing — expected the plaintext 0600 file"
        );
        assert_eq!(
            serde_json::from_str::<Session>(&on_disk)
                .unwrap()
                .account_token,
            "acct"
        );
    }

    /// **A storage report about an envelope names the identity THE ENVELOPE records**, not the
    /// one this launch happened to latch — that is the fact the report exists to settle, and the
    /// two are different on precisely the sets issue #76 is about. Here the launch registers as an
    /// application service while the envelope on disk was sealed anonymously by an older build.
    #[test]
    fn a_locked_read_reports_the_identity_the_envelope_records() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("report-sealed-identity");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::Anonymous);
        reset_report_state_for_test();
        // This launch can get the strongest identity there is; the envelope's is still anonymous.
        crate::keymanager::arm_identity_for_test(false, true, false);

        let _ = load();
        crate::keymanager::disarm_for_test();

        let reports = captured_reports();
        assert_eq!(reports.len(), 1, "{reports:?}");
        assert_eq!(
            reports[0].sealed_identity,
            Some(crate::keymanager::Identity::Anonymous),
            "the report is about the envelope, not about this launch"
        );
        drop(t);
    }

    /// The probe side of the same rule: a probe that fails to reopen reports the identity the
    /// PROBE FILE records. Here the hub grants the application-service form only, so the probe's
    /// own plain bus name is out of reach and `check_probe` reports `identity_unavailable` — about
    /// a probe whose recorded owner is `named`, not about the `app_id` this launch could have had.
    #[test]
    fn a_probe_report_names_the_identity_the_probe_records() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("report-probe-identity");
        write_probe_with_identity(crate::keymanager::Identity::Named);
        reset_report_state_for_test();
        arm_opening_keymanager_for(PROBE_PLAINTEXT);
        crate::keymanager::arm_identity_for_test(false, true, false);

        let _ = load();
        crate::keymanager::disarm_for_test();

        let reports = captured_reports();
        assert!(
            reports.iter().any(|r| r.stage
                == crate::telemetry::storage::StorageStage::IdentityUnavailable
                && r.sealed_identity == Some(crate::keymanager::Identity::Named)),
            "{reports:?}"
        );
        drop(t);
    }

    /// A report about no sealed thing at all carries `None` — the write that never landed. Read
    /// straight off the queue `report_write_failed` builds, since what is being graded is the
    /// context it CONSTRUCTS, not the consent-gated send that follows.
    #[test]
    fn a_write_failure_report_names_no_sealed_identity() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("report-no-identity");
        PENDING_REPORTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        report_write_failed();
        let pending = PENDING_REPORTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(
            pending[0].context.stage,
            crate::telemetry::storage::StorageStage::WriteFailed
        );
        assert_eq!(pending[0].context.sealed_identity, None);
        PENDING_REPORTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Plant an unanswered-launch counter at `launches`, as though that many prior launches had
    /// all found the key service silent.
    fn write_unavailable_counter_for_test(launches: u32) {
        let marker = unavailable_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            serde_json::to_vec_pretty(&serde_json::json!({
                "launches": launches,
                "stage": "unreachable",
                "noted_at_version": "0.0.0-test",
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// **The counter is ended by the SERVICE ANSWERING, not by the read that follows succeeding.**
    /// A decrypt that hands back bytes which are not a `Session` is `LOCKED_CORRUPT` — a verdict
    /// on this build's own plaintext format — but the key service demonstrably opened the envelope
    /// on the way there, which is exactly the evidence `UNAVAILABLE_MAX_LAUNCHES` is counting the
    /// absence of. Leaving the count behind lets a firmware hiccup from two boots ago shorten a
    /// LATER genuine run of silent launches, escalating an install to a permanent refusal one
    /// launch early (review finding, 2026-09-10).
    #[test]
    fn an_envelope_that_opens_clears_the_counter_even_when_its_plaintext_is_not_a_session() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("unavailable-cleared-on-corrupt");
        std::fs::write(t.file(), locked_envelope_bytes()).unwrap();
        write_unavailable_counter_for_test(UNAVAILABLE_MAX_LAUNCHES - 1);
        reset_report_state_for_test();

        arm_opening_keymanager_for(b"this decrypts fine and is not a session");
        let _ = load();
        crate::keymanager::disarm_for_test();

        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_CORRUPT,
            "the plaintext is not a session"
        );
        assert_eq!(
            read_unavailable_attempts(),
            0,
            "the key service answered, so the run of unanswered launches is over"
        );
        drop(t);
    }

    /// **Regression: the unanswered-launch counter is the production gate for the SAME reason —**
    /// it must be read trust-aware too, so a forged high count cannot force the very first
    /// stalled launch straight into a permanent refused marker.
    #[test]
    fn a_write_widened_unavailable_counter_does_not_force_an_immediate_refusal() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("forged-unavailable-counter");
        let bytes = locked_envelope_bytes();
        std::fs::write(t.file(), &bytes).unwrap();
        let marker = unavailable_marker_paths().into_iter().next().unwrap();
        std::fs::write(&marker, br#"{"launches":99,"stage":"unreachable"}"#).unwrap();
        std::fs::set_permissions(&marker, std::fs::Permissions::from_mode(0o666)).unwrap();
        reset_report_state_for_test();

        let _ = load();

        assert!(
            !has_refused_marker(),
            "a forged counter must not force the first genuinely unanswered launch into a refusal"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNAVAILABLE
        );
        let _ = t;
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

        let saved = canonical_session();
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
        let saved = canonical_session();
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
        let _t = TempSession::new("torn");
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
            let mut reads = 0u32;
            while !done.load(std::sync::atomic::Ordering::Acquire) {
                let store = crate::storage::open(persistence::root()).unwrap();
                let record = store
                    .load(crate::storage::RecordKey::Session)
                    .expect("the canonical record remains readable")
                    .expect("the canonical record remains present");
                let crate::storage::RecordState::Data { payload } = record.state else {
                    panic!("unexpected cleared record during writer test");
                };
                let s: Session = serde_json::from_str(&payload).unwrap_or_else(|e| {
                    panic!("torn session payload after {reads} clean reads: {e}")
                });
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
                identity: crate::keymanager::Identity::Anonymous,
            },
        };
        std::fs::write(t.file(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();

        arm_refusing_keymanager(); // a real refusal — see `open_failure_is_transient`
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(
            loaded.account_token.is_empty(),
            "Locked degrades to default"
        );

        save_after_reauthentication(&signed_in());
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

    // ---- Stage B1 (issue #76): sealed storage is EARNED by a cross-launch probe ------------------
    //
    // The field report's own case (3) is the reason none of the tests above are enough on their
    // own: `keymanager::seal`'s round-trip proof is IN-PROCESS, so a fresh sign-in on a backend
    // whose key is not usable from a DIFFERENT launch (or LS2 registration) seals cleanly, looks
    // perfectly healthy, and is unreadable the moment the television is power-cycled. These tests
    // pin the fix — a save never seals the REAL session until a PRIOR launch has proven a small,
    // non-secret probe envelope reopens; until then every save stays on the 0600 file and tries to
    // leave a probe of its own for the next launch to check (see `plant_probe`/`check_probe`).

    // ---- Issue #76, identity: a key belongs to WHOEVER SEALED IT ------------------------------
    //
    // The seal side may take the app-id identity where nothing else in the process holds that bus
    // name (`keymanager`'s "Identity" section). That fix is only sound if the identity is PINNED
    // to the envelope: an identity chosen freshly on every launch would be the very instability
    // it exists to remove. These pin the four transitions — recorded, reopened under the recorded
    // one, unobtainable (transient, never a refusal), and proven-for-one-identity-only.

    /// The envelope records the identity that sealed it, in the closed vocabulary every other
    /// surface uses.
    #[test]
    fn a_seal_under_the_app_id_identity_records_it_in_the_envelope() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("identity-recorded");
        mark_proven_for_test_as(crate::keymanager::Identity::AppId);
        arm_round_tripping_keymanager(&signed_in());
        crate::keymanager::arm_identity_for_test(false, true, false);

        assert!(save(&signed_in()).persisted());
        crate::keymanager::disarm_for_test();

        let envelope: SecureEnvelope =
            serde_json::from_str(&canonical_payload()).expect("a sealed envelope");
        assert_eq!(
            envelope.sealed.identity,
            crate::keymanager::Identity::AppId,
            "the owner of the key is a property of the envelope, not of the next launch"
        );
    }

    /// …and the NEXT launch opens it by asking for that identity, not for whichever one it could
    /// get. Two consecutive launches over the same file therefore ask for the same thing — the
    /// property that makes the identity fix a fix rather than a second source of drift.
    #[test]
    fn two_launches_open_one_envelope_under_the_identity_it_records() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-pinned-open");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::AppId);

        for _launch in 0..2 {
            super::redirect_for_test(Some(t.file()));
            arm_opening_keymanager_for(&serde_json::to_vec_pretty(&signed_in()).unwrap());
            crate::keymanager::arm_identity_for_test(false, true, false);
            assert_eq!(load().account_token, "acct");
            let asked = crate::keymanager::requested_identities_for_test();
            crate::keymanager::disarm_for_test();
            assert!(
                asked.contains(&Some(crate::keymanager::Identity::AppId)),
                "the open asked for the envelope's own identity, got {asked:?}"
            );
            assert!(
                !asked.contains(&Some(crate::keymanager::Identity::Anonymous)),
                "…and never for the other one, got {asked:?}"
            );
        }
    }

    /// An identity the hub will not grant THIS launch is a fact about a bus name, not about the
    /// key: the envelope is kept, no cross-launch refusal is recorded, and the report says
    /// `identity_unavailable` so the reason is legible off-device.
    #[test]
    fn an_unobtainable_recorded_identity_is_transient_and_never_a_refusal() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-unavailable");
        reset_report_state_for_test();
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::AppId);
        let original = std::fs::read(t.file()).unwrap();

        // A key manager that would answer perfectly well — the refusal under test is the
        // REGISTRATION, not the service: a hub that will not grant the app id, which is the shape
        // a webOS 4 set (its ACB holds the name) or a role file without the entry produces.
        arm_opening_keymanager_for(&serde_json::to_vec_pretty(&signed_in()).unwrap());
        crate::keymanager::arm_identity_for_test(false, false, false);
        let loaded = load();
        crate::keymanager::disarm_for_test();

        assert!(
            loaded.account_token.is_empty(),
            "the envelope did not open, so the run gets an ephemeral session"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            original,
            "the envelope is kept byte for byte — a later launch may well be granted the name"
        );
        assert!(
            !has_refused_marker(),
            "a name this launch could not get must never downgrade the install"
        );
        assert!(
            captured_reports()
                .iter()
                .any(|r| r.stage == crate::telemetry::storage::StorageStage::IdentityUnavailable),
            "the reason reaches a report: {:?}",
            captured_reports()
                .iter()
                .map(|r| r.stage)
                .collect::<Vec<_>>()
        );
    }

    /// **Regression for the review finding (2026-09-10): an `IdentityUnavailable` open used to
    /// fall through to `LOCKED_RECOVERABLE` once the shared unanswered-launch counter ran out,**
    /// which permitted a fresh sign-in to overwrite the still-sealed envelope with plaintext —
    /// exactly the loss the surrounding comment promises can never happen. Well past
    /// `UNAVAILABLE_MAX_LAUNCHES` identity-unavailable launches in a row, the read must still keep
    /// the envelope and report `LOCKED_UNAVAILABLE` every single time.
    #[test]
    fn many_identity_unavailable_launches_never_escalate_to_a_refusal() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-unavailable-no-escalation");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::AppId);
        let original = std::fs::read(t.file()).unwrap();

        for launch in 0..(UNAVAILABLE_MAX_LAUNCHES * 2) {
            super::redirect_for_test(Some(t.file()));
            reset_report_state_for_test();
            arm_opening_keymanager_for(&serde_json::to_vec_pretty(&signed_in()).unwrap());
            crate::keymanager::arm_identity_for_test(false, false, false);
            let _ = load();
            crate::keymanager::disarm_for_test();

            assert!(
                !has_refused_marker(),
                "launch {launch}: a bus name this launch could not get must never downgrade the install"
            );
            assert_eq!(
                LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
                LOCKED_UNAVAILABLE,
                "launch {launch}: identity-unavailable never falls through to LOCKED_RECOVERABLE"
            );
            assert_eq!(
                std::fs::read(t.file()).unwrap(),
                original,
                "launch {launch}: the sealed envelope is untouched"
            );
        }

        // And the legacy envelope is still genuinely sealed — a fresh sign-in would seal rather
        // than overwrite it with plaintext, since `LOCKED_RECOVERABLE` never occurred.
        assert!(
            serde_json::from_slice::<SecureEnvelope>(&std::fs::read(t.file()).unwrap()).is_ok(),
            "still a real envelope, not plaintext"
        );
    }

    /// **Regression: identity-unavailable launches must never spend the SILENT-SERVICE budget**
    /// (review finding, 2026-09-10). Two identity-unavailable launches followed by one genuinely
    /// silent-service launch must not push the shared counter past its limit — the two stages are
    /// evidence about different things and must never combine to trip either one's escalation.
    #[test]
    fn identity_unavailable_launches_do_not_spend_the_silent_service_budget() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-unavailable-separate-budget");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::AppId);

        for _ in 0..(UNAVAILABLE_MAX_LAUNCHES - 1) {
            super::redirect_for_test(Some(t.file()));
            reset_report_state_for_test();
            arm_opening_keymanager_for(&serde_json::to_vec_pretty(&signed_in()).unwrap());
            crate::keymanager::arm_identity_for_test(false, false, false);
            let _ = load();
            crate::keymanager::disarm_for_test();
        }
        assert!(
            !has_refused_marker(),
            "identity-unavailable launches alone never arm the marker"
        );
        assert_eq!(
            read_unavailable_attempts(),
            0,
            "identity-unavailable launches must never touch the silent-service counter"
        );

        // Now a genuinely silent service (no scripted backend, nothing registers) — its own FIRST
        // launch, and it must be graded exactly that: LOCKED_UNAVAILABLE, not an immediate refusal
        // inherited from the identity launches above.
        super::redirect_for_test(Some(t.file()));
        reset_report_state_for_test();
        let bytes = locked_envelope_bytes();
        std::fs::write(t.file(), &bytes).unwrap();
        let _ = load();
        assert!(
            !has_refused_marker(),
            "one stalled boot must not cost a sealed install its storage — its own budget starts at zero"
        );
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNAVAILABLE
        );
    }

    /// A proven marker is proof for ONE identity. An install proven as `app_id` that finds itself
    /// anonymous (or the reverse) has proven nothing about the identity it would seal under now,
    /// so the save stays on the 0600 file and plants a probe for the identity it actually has.
    #[test]
    fn a_proof_for_one_identity_does_not_permit_sealing_under_another() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("identity-proof-scope");
        mark_proven_for_test_as(crate::keymanager::Identity::AppId);
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        // This launch is anonymous — an ACB set, or a hub that refused the app id.
        crate::keymanager::arm_identity_for_test(true, false, false);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        assert_eq!(canonical_session().account_token, "acct");
        assert!(
            probe_paths().iter().any(|p| p.exists()),
            "a probe is planted under the identity this launch actually has"
        );
        assert_eq!(
            planted_probe_identity(),
            Some(crate::keymanager::Identity::Anonymous)
        );
    }

    /// A probe left by a launch with a different identity cannot promote this one, so it is
    /// replaced rather than waited on — otherwise an install whose identity changed would sit
    /// forever on a probe no launch of its own can answer.
    #[test]
    fn a_probe_sealed_under_another_identity_is_replanted() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-probe-replant");
        write_probe_with_identity(crate::keymanager::Identity::AppId);
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        crate::keymanager::arm_identity_for_test(true, false, false);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        assert_eq!(
            planted_probe_identity(),
            Some(crate::keymanager::Identity::Anonymous),
            "the stale probe was replaced by one this launch's identity can answer"
        );
        drop(t);
    }

    /// The probe side of the transient rule: a probe whose identity this launch cannot obtain is
    /// dropped (so the next save plants a usable one) and never graded as a refusal.
    #[test]
    fn a_probe_whose_identity_is_unobtainable_is_dropped_without_a_refusal() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-probe-transient");
        reset_report_state_for_test();
        write_probe_with_identity(crate::keymanager::Identity::AppId);
        arm_opening_keymanager_for(PROBE_PLAINTEXT);
        crate::keymanager::arm_identity_for_test(false, false, false);

        let _ = load();
        crate::keymanager::disarm_for_test();

        assert!(
            !has_refused_marker(),
            "an identity this launch could not get is not a verdict on the key manager"
        );
        assert!(!has_proven_marker(), "…and proves nothing either");
        assert!(
            probe_paths().iter().all(|p| !p.exists()),
            "the probe is dropped so the next save can plant one for this identity"
        );
        assert!(
            captured_reports()
                .iter()
                .any(|r| r.stage == crate::telemetry::storage::StorageStage::IdentityUnavailable),
            "the reason reaches a report"
        );
        drop(t);
    }

    /// **`named` is a first-class identity on the marker and probe surfaces too**, not only in the
    /// envelope: an install proven under the plain bus name has proven nothing about the
    /// application-service one, so a launch that gets `app_id` instead stays on the 0600 file and
    /// replants under what it actually has. Same rule as the pair above, pointed at the shape
    /// added 2026-09-10 — and it is the direction that matters most, because a set which grants
    /// the name may later be handed a role file that grants the application service.
    #[test]
    fn a_proof_under_the_plain_bus_name_does_not_permit_sealing_as_an_application_service() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("identity-proof-named");
        mark_proven_for_test_as(crate::keymanager::Identity::Named);
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        // This launch is granted the application-service form: the STRONGER identity, and still
        // not the one the proof is about.
        crate::keymanager::arm_identity_for_test(false, true, false);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        assert_eq!(canonical_session().account_token, "acct");
        assert_eq!(
            planted_probe_identity(),
            Some(crate::keymanager::Identity::AppId)
        );
    }

    /// The reverse, and the one a webOS 5+ reporter is likeliest to hit: proven as an application
    /// service, this launch can only get the plain bus name. The proof does not carry, and the
    /// probe is replanted as `named`.
    #[test]
    fn a_launch_that_can_only_get_the_bus_name_replants_under_it() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-proof-appid-to-named");
        mark_proven_for_test_as(crate::keymanager::Identity::AppId);
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        crate::keymanager::arm_identity_for_test(false, false, true);

        save(&signed_in());
        crate::keymanager::disarm_for_test();

        assert_eq!(
            planted_probe_identity(),
            Some(crate::keymanager::Identity::Named)
        );
        drop(t);
    }

    /// The proven marker's identity field parses `named` back out — it is written through
    /// `Identity::code()` and read through the same serde vocabulary, so a spelling drift here
    /// would silently downgrade every proof to `anonymous`.
    #[test]
    fn the_proven_marker_round_trips_the_named_identity() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("identity-marker-named");
        mark_proven_for_test_as(crate::keymanager::Identity::Named);
        let bytes = std::fs::read(proven_marker_paths().into_iter().next().unwrap()).unwrap();
        assert!(
            String::from_utf8_lossy(&bytes).contains(r#""identity": "named""#),
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        assert_eq!(
            proven_marker_identity(),
            Some(crate::keymanager::Identity::Named)
        );
        drop(t);
    }

    /// An envelope written before the identity field existed reads as `anonymous` — the only
    /// identity those builds ever registered with — rather than failing to parse.
    #[test]
    fn an_envelope_from_before_the_identity_field_reads_as_anonymous() {
        let envelope: SecureEnvelope = serde_json::from_str(
            r#"{"format":"plxnative-secure-session","version":1,
                "sealed":{"backend":"keymanager3","key":"plxnative.session.v1",
                          "iv":"AAAAAAAAAAAAAAAAAAAAAA==","data":"c2VjcmV0"}}"#,
        )
        .expect("an 0.6.2 envelope still parses");
        assert_eq!(
            envelope.sealed.identity,
            crate::keymanager::Identity::Anonymous
        );
    }

    /// [`mark_proven_for_test`], for an identity other than the default anonymous one.
    fn mark_proven_for_test_as(identity: crate::keymanager::Identity) {
        let marker = proven_marker_paths().into_iter().next().unwrap();
        std::fs::write(
            &marker,
            serde_json::to_vec_pretty(&serde_json::json!({
                "proven_at_version": "0.0.0-test",
                "stage": "probe_opened",
                "identity": identity.code(),
            }))
            .unwrap(),
        )
        .unwrap();
    }

    /// A secure file whose envelope names `identity`, with ciphertext no test ever decodes — the
    /// tests using it are about WHICH registration the open asks for, not about the bytes.
    fn write_envelope_with_identity(path: &std::path::Path, identity: crate::keymanager::Identity) {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
                identity,
            },
        };
        std::fs::write(path, serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();
    }

    /// The same for the cross-launch probe file.
    fn write_probe_with_identity(identity: crate::keymanager::Identity) {
        let probe = ProbeFile {
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                data: "c2VjcmV0".to_string(),
                identity,
            },
            attempts: 0,
            key_outcome: None,
        };
        let path = probe_paths().into_iter().next().unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&probe).unwrap()).unwrap();
    }

    /// Which identity the probe now on disk was sealed under.
    fn planted_probe_identity() -> Option<crate::keymanager::Identity> {
        probe_paths().iter().find_map(|p| {
            let bytes = std::fs::read(p).ok()?;
            serde_json::from_slice::<ProbeFile>(&bytes)
                .ok()
                .map(|probe| probe.sealed.identity)
        })
    }

    /// A scripted keymanager that opens one envelope back to `plain` — the read half of
    /// [`arm_round_tripping_keymanager_bytes`], for tests that start from a file on disk.
    fn arm_opening_keymanager_for(plain: &[u8]) {
        crate::keymanager::arm_for_test(vec![
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    "output": b64_encode_for_test(plain)
                })),
            ),
        ]);
    }

    /// A probe planted by an unproven launch that reopens on the very next launch promotes the
    /// install to sealed storage — and does so in time for that SAME launch's existing "offer
    /// every plaintext session to the key manager" step ([`load`]) to seal the session it just
    /// read back as plaintext, without a third launch or another sign-in.
    #[test]
    fn a_probe_that_opens_on_the_next_launch_promotes_the_install_to_sealed_storage() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("probe-promotes");

        // Launch 1: a fresh sign-in on an unproven install lands as plaintext, and plants a probe.
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            !has_proven_marker(),
            "an in-process round trip alone must never be trusted across a launch boundary"
        );
        assert_eq!(canonical_session().account_token, "acct");
        assert!(
            probe_paths().iter().any(|p| p.exists()),
            "a probe was planted for the next launch to check"
        );

        // Launch 2 — the power cycle. This launch's OWN key manager reopens the probe (a fresh
        // registration, exactly the boundary an in-process check cannot cross) and then, since
        // `load()` finds the file plaintext, gets to seal the real session in the same breath.
        super::redirect_for_test(Some(t.file()));
        let session_bytes = serde_json::to_vec_pretty(&signed_in()).unwrap();
        crate::keymanager::arm_for_test(vec![
            // `check_probe`'s own open.
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-probe"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    "output": b64_encode_for_test(PROBE_PLAINTEXT)
                })),
            ),
            // The now-proven install's own reseal of the plaintext session `load()` just read.
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
                    "output": b64_encode_for_test(&session_bytes)
                })),
            ),
        ]);
        let after = load();
        crate::keymanager::disarm_for_test();

        assert_eq!(after.account_token, "acct");
        assert!(
            has_proven_marker(),
            "a probe that reopens promotes the install"
        );
        assert!(
            probe_paths().iter().all(|p| !p.exists()),
            "the probe is consumed once it has answered"
        );
        assert!(
            serde_json::from_str::<SecureEnvelope>(&canonical_payload()).is_ok(),
            "the same boot that earns proven storage also seals the session it just read as \
             plaintext, rather than waiting for a third launch"
        );
    }

    /// Once an install is proven, it behaves exactly as 0.6.2's secure path always did: `save`
    /// seals immediately (no probe round trip in the way), and a LATER launch — a fresh LS2
    /// registration — reopens the envelope with no sign-in asked for.
    #[test]
    fn a_proven_install_seals_and_reopens_across_launches() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("proven-seals");
        mark_proven_for_test();

        arm_round_tripping_keymanager(&signed_in());
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            serde_json::from_str::<SecureEnvelope>(&canonical_payload()).is_ok(),
            "a proven install seals immediately, exactly like 0.6.2's secure path"
        );

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
        let after = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            after.account_token, "acct",
            "the proven install's own envelope reopens on the next launch"
        );
    }

    /// **The field report's own shape (case 3), ported to the probe design.** A probe that FAILS to
    /// reopen on the next launch costs nothing already won — the session was written as plaintext
    /// from the very first save, so launch 2 reads it back with no QR code, no envelope was ever
    /// written for the real session at all, and the failure is recorded as the refused marker
    /// (never spent as a wasted sign-in the way an unproven seal-and-reseal loop would).
    #[test]
    fn a_probe_that_fails_arms_the_refused_marker_without_costing_a_sign_in() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("probe-fails");

        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            probe_paths().iter().any(|p| p.exists()),
            "launch 1 planted a probe"
        );

        // Launch 2 — the power cycle. This launch's key manager genuinely refuses the decrypt.
        super::redirect_for_test(Some(t.file()));
        arm_refusing_keymanager();
        let after = load();
        crate::keymanager::disarm_for_test();

        assert_eq!(
            after.account_token, "acct",
            "the session was already plaintext — a failed probe costs nothing that was not \
             already lost"
        );
        assert!(
            has_refused_marker(),
            "a probe that fails to reopen arms the refused marker"
        );
        assert!(!has_proven_marker());
        assert!(
            probe_paths().iter().all(|p| !p.exists()),
            "the probe is consumed once it has answered, pass or fail"
        );
    }

    /// **Issue #76's identity decider.** A probe planted on a launch where `generateKey` MINTED a
    /// new key (`-> created`), that then fails to reopen on the NEXT launch with a genuine GCM tag
    /// mismatch (`finish(decrypt)` refused `-20030`) — the shape the decider is built to catch: a
    /// key that existed at seal time but a DIFFERENT launch's registration cannot open. The report
    /// this launch queues must carry `key_outcome = created`, read from the PROBE FILE (the seal
    /// that produced it), never from this launch's own `last_key_outcome` — this launch never once
    /// called `generateKey`, since `check_probe` only opens.
    #[test]
    fn a_probe_sealed_after_a_created_key_that_fails_next_launch_reports_created_and_the_tag_code()
    {
        let _g = crate::testlock::serial();
        let t = TempSession::new("probe-key-outcome-created");
        reset_report_state_for_test();

        // Launch 1: `generateKey` succeeds outright (no `-10002`) — a NEW key was minted — and the
        // probe round-trips in-process, exactly as `plant_probe` requires before it persists.
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            probe_paths().iter().any(|p| p.exists()),
            "launch 1 planted a probe"
        );

        // Launch 2 — the power cycle. This launch's OWN registration cannot open the key: `begin`
        // succeeds but `finish(decrypt)` answers the real GCM tag-mismatch code.
        super::redirect_for_test(Some(t.file()));
        crate::keymanager::arm_for_test(vec![
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": false, "errorCode": -20030, "errorText": "tag mismatch"
                })),
            ),
        ]);
        let after = load();
        crate::keymanager::disarm_for_test();

        assert_eq!(
            after.account_token, "acct",
            "the session was already plaintext — a failed probe costs nothing that was not \
             already lost"
        );
        assert!(
            has_refused_marker(),
            "a genuine tag mismatch arms the refused marker"
        );
        assert!(
            probe_paths().iter().all(|p| !p.exists()),
            "the probe is consumed"
        );

        let reports = captured_reports();
        assert_eq!(reports.len(), 1, "exactly one report for this failure");
        assert_eq!(
            reports[0].stage,
            crate::telemetry::storage::StorageStage::FinishDecrypt
        );
        assert_eq!(reports[0].service_error_code, Some(-20030));
        assert_eq!(
            reports[0].key_outcome,
            Some(crate::keymanager::KeyOutcome::Created),
            "the report must carry the PROBE's own seal-time outcome, not this launch's (which \
             never called generateKey at all)"
        );
        assert!(
            reports[0].refused_marker,
            "the marker this same failure just armed"
        );
    }

    /// The `existed` half of the same decider, at PLANT time: a save whose `generateKey` answers
    /// `-10002` ("key already exists") persists `key_outcome: existed` into the probe file — for
    /// [`check_probe`] on a later launch to read back, regardless of what that launch's own
    /// (probe-only, `generateKey`-free) open call sees.
    #[test]
    fn a_probe_sealed_after_an_existing_key_records_existed_on_disk() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("probe-key-outcome-existed");
        reset_report_state_for_test();

        crate::keymanager::arm_for_test(vec![
            (
                "generateKey",
                Ok(serde_json::json!({
                    "returnValue": false, "errorCode": -10002, "errorText": "key already exists"
                })),
            ),
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
                    "output": b64_encode_for_test(PROBE_PLAINTEXT)
                })),
            ),
        ]);
        save(&signed_in());
        crate::keymanager::disarm_for_test();

        let path = probe_paths()
            .into_iter()
            .find(|p| p.exists())
            .expect("a probe was planted");
        let probe: ProbeFile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            probe.key_outcome,
            Some(crate::keymanager::KeyOutcome::Existed),
            "the probe must persist the seal-time outcome it observed"
        );
    }

    // ---- Issue #76 review (blockers): an already-healthy install must not be an absorbing state ---

    /// **Blocker.** An install that already holds a working secure envelope — an upgrade from
    /// 0.6.1/0.6.2, which sealed unconditionally, or any install whose probe cycle simply hasn't
    /// run yet — must not be permanently stuck unproven. Reopening the envelope on an ordinary
    /// boot IS the cross-launch proof the probe exists to manufacture, so `load` promotes the
    /// install in the same breath it reads the file, and a later `update` (a roster refresh, a
    /// pin) persists normally rather than silently landing nowhere.
    #[test]
    fn an_upgraded_install_with_a_healthy_envelope_is_promoted_by_reading_it() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("upgrade-promotes-on-read");
        let plain = serde_json::to_vec_pretty(&signed_in()).unwrap();
        std::fs::write(
            t.file(),
            serde_json::to_vec_pretty(&SecureEnvelope {
                format: SECURE_FORMAT.to_string(),
                version: 1,
                sealed: crate::keymanager::Sealed {
                    backend: crate::keymanager::Backend::Keymanager3,
                    key: "plxnative.session.v1".to_string(),
                    iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                    data: "ignored-by-the-mock".to_string(),
                    identity: crate::keymanager::Identity::Anonymous,
                },
            })
            .unwrap(),
        )
        .unwrap();
        assert!(!has_proven_marker(), "a fresh install starts unproven");

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
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(loaded.account_token, "acct");
        assert!(
            has_proven_marker(),
            "reopening an envelope THIS process never sealed is the cross-launch proof the probe \
             exists to manufacture — the install must be promoted right there"
        );

        // With finding 1 unfixed, this next edit landed nowhere at all (`save_locked`'s
        // unproven-and-secure dead end): confirm an ordinary update genuinely persists now. The
        // round-trip script has to echo THIS save's own bytes (the edited session, not the
        // original) — `keymanager::seal`'s own round-trip proof would otherwise mismatch.
        let mut edited = signed_in();
        edited.playback_quality = Some(PlaybackQuality::Auto);
        arm_round_tripping_keymanager(&edited);
        let wrote = update(|s| {
            let mut next = s.clone();
            next.playback_quality = Some(PlaybackQuality::Auto);
            Some(next)
        });
        crate::keymanager::disarm_for_test();
        assert!(
            wrote,
            "an upgraded install whose envelope opens must still be able to persist"
        );
    }

    /// **Blocker.** Field report case 5, ported to `LOCKED_CORRUPT`: this build's OWN format opens
    /// fine but the plaintext is not a session — real corruption, not the keymanager3 round-trip
    /// bug — and a fresh sign-in over it must still be able to recover to plaintext. Refusing it
    /// (the pre-fix `has_secure_locked` dead end in the unproven branch) is an unbreakable sign-in
    /// loop with no `clear()` in the loop to break it.
    #[test]
    fn a_fresh_sign_in_over_a_corrupt_envelope_is_persisted() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("corrupt-envelope-recovers");
        assert!(matches!(
            persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
            persistence::CanonicalCommit::Durable { .. }
        ));

        crate::keymanager::arm_for_test(vec![
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    "output": b64_encode_for_test(b"not-a-session-at-all")
                })),
            ),
        ]);
        let _ = load();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_CORRUPT,
            "this build's own format opened but did not parse as a session"
        );

        let wrote = save_after_reauthentication(&signed_in()).persisted();
        assert!(
            wrote,
            "the fresh sign-in must be persisted - otherwise every launch asks again, forever"
        );
        assert_eq!(
            canonical_session().account_token,
            "acct",
            "plaintext payload — nothing left on a corrupt envelope to protect"
        );
    }

    /// The foreign/future-version shadow must still refuse a fresh sign-in exactly as before —
    /// `LOCKED_CORRUPT`'s recovery path must never widen to cover `LOCKED_UNRECOVERABLE`.
    /// Pinned beside the corrupt-envelope test above so the two cannot drift back together.
    #[test]
    fn a_foreign_envelope_still_never_recovers_even_unproven() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("foreign-envelope-stays-locked-unproven");
        std::fs::write(
            t.file(),
            br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#,
        )
        .unwrap();
        let _ = load();
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNRECOVERABLE
        );
        assert_eq!(
            save(&signed_in()),
            PersistOutcome::BlockedUnknownEnvelope,
            "a foreign-version envelope must never be overwritten"
        );
        let on_disk = std::fs::read(t.file()).unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&on_disk)
                .unwrap()
                .get("version")
                .is_some(),
            "the foreign envelope must still be sitting there, untouched"
        );
    }

    /// **The same rule on a PROVEN install, which is where it was missing** (review finding,
    /// 2026-09-10). Both unproven branches of `save_locked` guard a present secure file with
    /// `has_secure_locked`, but a proven install falls straight through to `keymanager::seal` and
    /// writes its fresh envelope over whatever is at the candidate path — a foreign or
    /// future-version envelope included. That file is not this build's, nothing here has ever read
    /// it, and destroying it is exactly what `a_foreign_envelope_still_never_recovers_even_unproven`
    /// forbids one branch earlier. A downgrade from a newer build is the concrete case: its
    /// envelope is unreadable HERE and perfectly readable again after the upgrade — unless this
    /// launch overwrote it.
    #[test]
    fn a_foreign_envelope_is_never_overwritten_by_a_proven_installs_seal() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("foreign-envelope-stays-locked-proven");
        let foreign = br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#;
        std::fs::write(t.file(), foreign).unwrap();
        mark_proven_for_test();
        let _ = load();
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            LOCKED_UNRECOVERABLE
        );

        // A key manager that round-trips perfectly: the seal WOULD succeed, which is the whole
        // point — what stops it must be the rule, not a broken backend.
        arm_round_tripping_keymanager(&signed_in());
        let wrote = save(&signed_in()).persisted();
        crate::keymanager::disarm_for_test();

        assert!(
            !wrote,
            "a foreign-version envelope must never be overwritten"
        );
        assert_eq!(
            std::fs::read(t.file()).unwrap(),
            foreign,
            "byte-identical: not resealed, not rewritten as plaintext"
        );
    }

    /// The cross-launch half: a save that happens before this launch's own `load` has read
    /// anything (`LOCKED_STATE` still at its default) must reach the same verdict off the DISK.
    /// Otherwise the rule holds only for the one ordering the boot path happens to take.
    #[test]
    fn a_foreign_envelope_is_not_overwritten_by_a_save_that_never_read_it() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("foreign-envelope-unread");
        let foreign = br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#;
        std::fs::write(t.file(), foreign).unwrap();
        mark_proven_for_test();
        // No `load()` at all — `LOCKED_STATE` says nothing about this file.
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            NOT_LOCKED
        );

        arm_round_tripping_keymanager(&signed_in());
        let wrote = save(&signed_in()).persisted();
        crate::keymanager::disarm_for_test();

        assert!(!wrote);
        assert_eq!(std::fs::read(t.file()).unwrap(), foreign);
    }

    /// **A foreign envelope at a LOWER-priority candidate is not the guard's business, and it is
    /// not the sweep's to delete either** (review finding, 2026-09-11). The guard above decides on
    /// the first readable candidate — deliberately, so a stale file nobody reads cannot freeze a
    /// healthy install's saves forever — and that left the file itself unprotected from the other
    /// direction: a successful seal sweeps every OTHER candidate clean, and swept a newer build's
    /// unreadable envelope with it. The two halves have to agree: this build may ignore a file it
    /// cannot read, and may not destroy it.
    #[test]
    fn a_foreign_envelope_at_another_candidate_survives_a_seals_sweep() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("foreign-survives-seal-sweep");
        let foreign = br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#;
        // The candidate this install actually reads is a healthy plaintext session; the foreign
        // envelope is at the lower-priority path, shadowing nothing.
        std::fs::write(t.higher(), serde_json::to_vec_pretty(&signed_in()).unwrap()).unwrap();
        std::fs::write(t.lower(), foreign).unwrap();
        mark_proven_for_test();

        arm_round_tripping_keymanager(&signed_in());
        let wrote = save(&signed_in()).persisted();
        crate::keymanager::disarm_for_test();

        assert!(
            wrote,
            "a readable plaintext candidate must still be resealed"
        );
        assert!(
            serde_json::from_str::<SecureEnvelope>(&canonical_payload()).is_ok(),
            "the canonical payload is the fresh envelope this save produced"
        );
        assert_eq!(
            std::fs::read(t.lower()).unwrap(),
            foreign,
            "byte-identical: a secure file this build cannot read is never the sweep's to delete"
        );
    }

    /// The same rule on the OTHER sweep — the plaintext recovery write, which sweeps exactly like
    /// a seal does. Here the locked (recognized, unopenable) envelope is at the candidate this
    /// launch reads, and the foreign one sits below it.
    #[test]
    fn a_foreign_envelope_at_another_candidate_survives_a_recovery_sweep() {
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("foreign-survives-recovery-sweep");
        let foreign = br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#;
        write_envelope_with_identity(&t.higher(), crate::keymanager::Identity::Anonymous);
        std::fs::write(t.lower(), foreign).unwrap();

        arm_refusing_keymanager(); // a real refusal — see `open_failure_is_transient`
        let loaded = load();
        crate::keymanager::disarm_for_test();
        assert!(
            loaded.account_token.is_empty(),
            "Locked degrades to default"
        );

        assert!(
            save_after_reauthentication(&signed_in()).persisted(),
            "a fresh sign-in recovers the locked candidate"
        );

        let saved = canonical_session();
        assert_eq!(saved.account_token, "acct");
        assert_eq!(
            std::fs::read(t.lower()).unwrap(),
            foreign,
            "byte-identical: the recovery sweep must skip it exactly as the seal sweep does"
        );
    }

    /// **A write-widened candidate decides nothing** (review finding, 2026-09-11): the guard read
    /// the first candidate through the trust-BLIND reader, so a file any uid in the shared
    /// `/media/developer` namespace could have written was allowed to answer "no foreign envelope
    /// here" — and the foreign envelope one candidate below it was then swept away by the very
    /// save that answer unblocked. A peer that cannot read a 0600 file can still create a
    /// world-writable one at a name this app reads, which is what makes that a real primitive
    /// rather than a tidiness point.
    #[test]
    fn a_write_widened_candidate_never_decides_the_foreign_envelope_guard() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("foreign-guard-untrusted-candidate");
        let foreign = br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#;
        std::fs::write(t.higher(), serde_json::to_vec_pretty(&signed_in()).unwrap()).unwrap();
        std::fs::set_permissions(t.higher(), std::fs::Permissions::from_mode(0o666)).unwrap();
        std::fs::write(t.lower(), foreign).unwrap();
        mark_proven_for_test();

        arm_round_tripping_keymanager(&signed_in());
        let wrote = save(&signed_in()).persisted();
        crate::keymanager::disarm_for_test();

        assert!(
            !wrote,
            "the untrusted candidate is skipped, so the foreign envelope below it is what this \
             install would read — and nothing may be written over it"
        );
        assert_eq!(std::fs::read(t.lower()).unwrap(), foreign);
    }

    /// The other side of that guard: a proven install whose file is OUR OWN, recognized envelope
    /// re-seals exactly as it always did. The rule is about a shape this build cannot read, not
    /// about the presence of ciphertext.
    #[test]
    fn a_proven_install_still_reseals_over_its_own_recognized_envelope() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("proven-reseals-own-envelope");
        write_envelope_with_identity(&t.file(), crate::keymanager::Identity::Anonymous);
        mark_proven_for_test();

        arm_round_tripping_keymanager(&signed_in());
        let wrote = save(&signed_in()).persisted();
        crate::keymanager::disarm_for_test();

        assert!(
            wrote,
            "a recognized envelope is this build's own to replace"
        );
        let on_disk = canonical_payload();
        assert_eq!(
            serde_json::from_str::<SecureEnvelope>(&on_disk)
                .expect("still an envelope")
                .sealed
                .data,
            "Y2lwaGVydGV4dA==",
            "and it is the FRESH one this save produced"
        );
    }

    // ---- Issue #76 review (should-fix): `update()` propagates a real persist failure -----------

    /// A `save_locked` refusal (the unproven-and-secure dead end, still reachable for a NON-fresh
    /// -sign-in edit) must come back out of `update()` as `false`, not `true` — and the edit must
    /// still be visible to THIS run via the cache, the same courtesy `auth::take_ready` already
    /// gives a failed `save`.
    #[test]
    fn update_reports_a_real_refusal_and_still_publishes_the_edit_in_process() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("update-reports-refusal");
        // Establish a cached, unlocked session the ordinary way (a plaintext save — no keymanager
        // script armed, so it stays on the 0600 fallback) so `update`'s cache fast path is what
        // the edit below actually exercises, rather than a fresh `read_locked` of the file this
        // test is about to plant underneath it.
        save(&signed_in());
        assert_eq!(peek().account_token, "acct");
        assert_eq!(
            LOCKED_STATE.load(std::sync::atomic::Ordering::Relaxed),
            NOT_LOCKED
        );

        // A secure-shaped file now sitting on disk that THIS process never read through
        // `read_locked` — `save_locked`'s own comment names exactly this shape ("a file planted
        // by something else entirely"). Unproven, and this launch's own `LOCKED_STATE` says
        // nothing is wrong with it (it was never read at all), so `save_locked` must leave it
        // untouched rather than overwrite it with plaintext.
        assert!(matches!(
            persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
            persistence::CanonicalCommit::Durable { .. }
        ));
        assert!(!has_proven_marker());

        let wrote = update(|s| {
            let mut next = s.clone();
            next.playback_quality = Some(PlaybackQuality::Auto);
            Some(next)
        });
        assert!(!wrote, "the edit genuinely was not persisted to disk");
        assert_eq!(
            peek().playback_quality,
            Some(PlaybackQuality::Auto),
            "but this run must still see its own edit rather than reading it back as though it \
             never happened"
        );
        assert!(
            serde_json::from_str::<SecureEnvelope>(&canonical_payload()).is_ok(),
            "the secure file this process never read must survive untouched"
        );
    }

    // ---- Issue #76 review (nit/should-fix): the probe is planted once per unproven install -----

    /// `plant_probe` must not re-seal an equivalent probe on every unproven save — one probe
    /// sitting on disk is enough for `check_probe` to consume on the next launch.
    #[test]
    fn plant_probe_does_not_reseal_once_a_probe_already_exists() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("plant-probe-latches");
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        // Two unproven saves in a row (a roster refresh right after the sign-in, say).
        save(&signed_in());
        let first_call_count = crate::keymanager::calls_for_test().len();
        assert!(first_call_count > 0, "the first save plants a probe");
        save(&signed_in());
        let second_call_count = crate::keymanager::calls_for_test().len();
        crate::keymanager::disarm_for_test();
        assert_eq!(
            first_call_count, second_call_count,
            "a probe already on disk must not be resealed by a later unproven save"
        );
    }

    // ---- Issue #76 review (should-fix): a stalled service must not permanently downgrade ---------

    /// A probe that finds `NoReply`/`Unreachable` — the service simply did not answer, proving
    /// nothing about the key — must be retried by a later launch rather than immediately arming
    /// the refused marker, and only gives up after `PROBE_MAX_ATTEMPTS` such launches.
    #[test]
    fn a_stalled_probe_is_retried_before_being_graded_a_refusal() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("stalled-probe-retries");
        arm_round_tripping_keymanager_bytes(PROBE_PLAINTEXT);
        save(&signed_in());
        crate::keymanager::disarm_for_test();
        assert!(
            probe_paths().iter().any(|p| p.exists()),
            "launch 1 planted a probe"
        );

        for attempt in 0..PROBE_MAX_ATTEMPTS - 1 {
            super::redirect_for_test(Some(t.file()));
            crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
            check_probe();
            crate::keymanager::disarm_for_test();
            assert!(
                !has_refused_marker(),
                "attempt {attempt}: a stalled service must not be graded a refusal yet"
            );
            assert!(!has_proven_marker());
            assert!(
                probe_paths().iter().any(|p| p.exists()),
                "attempt {attempt}: the probe stays for the next launch to retry"
            );
        }

        // The final attempt exhausts the budget and is graded a real refusal.
        super::redirect_for_test(Some(t.file()));
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        check_probe();
        crate::keymanager::disarm_for_test();
        assert!(
            has_refused_marker(),
            "a service that never once replies across every retry is finally graded a refusal"
        );
        assert!(
            probe_paths().iter().all(|p| !p.exists()),
            "the probe is consumed once a final verdict is reached"
        );
    }

    // ---- Issue #76 review (should-fix): a real consent "Yes" gives a dropped stage its attempt --

    /// [`retry_dropped_stages`] clears the dropped-stage set — [`telemetry::mod::record`]'s own
    /// hook when a decision newly enables the Errors channel — so the identical failure, occurring
    /// again after that "Yes", is attempted rather than skipped forever.
    #[test]
    fn retry_dropped_stages_lets_the_identical_failure_be_attempted_again() {
        let _g = crate::testlock::serial();
        reset_report_state_for_test();
        crate::telemetry::consent::install(crate::telemetry::consent::Consent {
            asked_version: crate::telemetry::consent::POLICY_VERSION,
            errors: false,
            ..Default::default()
        });
        let stage = crate::telemetry::storage::StorageStage::BeginDecrypt;
        let ctx = crate::telemetry::storage::StorageErrorContext {
            stage,
            service_error_code: None,
            class: crate::telemetry::storage::SessionStorageClass::SecureRefused,
            refused_marker: true,
            key_outcome: None,
            registered_with_app_id: false,
            registered_with_name: false,
            sealed_identity: None,
        };
        report_once(PendingReport {
            context: ctx,
            candidate_reads: None,
            candidate: None,
        });
        assert_eq!(captured_reports().len(), 1);
        report_once(PendingReport {
            context: ctx,
            candidate_reads: None,
            candidate: None,
        });
        assert_eq!(
            captured_reports().len(),
            1,
            "a dropped stage is not retried on its own"
        );

        retry_dropped_stages();
        report_once(PendingReport {
            context: ctx,
            candidate_reads: None,
            candidate: None,
        });
        assert_eq!(
            captured_reports().len(),
            2,
            "a real Yes must give the dropped stage its one attempt back"
        );
        crate::telemetry::consent::install(crate::telemetry::consent::Consent::default());
    }

    // ---- storage-file hardening: no writer under telemetry/ or this file creates anything at a
    // permissive mode -----------------------------------------------------------------------------

    /// **Every file this module, `telemetry/`, `lib.rs` or `keymanager.rs` creates fresh must name
    /// `0o600` explicitly, in the SAME STATEMENT, or be on this test's own allowlist by exact
    /// line.** A file that already exists and is merely reopened (the spool's ordinary append) is
    /// not a creation and is not what this test is about — see `repair_owned_mode` and its callers
    /// for that half instead.
    ///
    /// Walks the real source tree (`env!("CARGO_MANIFEST_DIR")`), so it cannot rot the way a
    /// transcribed list would: a new creation call added to any scanned scope fails this test the
    /// moment it lands, not the next time somebody happens to read the file by hand. Two needle
    /// sets, because two shapes both create a file: an `OpenOptions` builder chain naming
    /// `.create(true)`/`.create_new(true)` (or bare `File::create(`/`File::create_new(`), which
    /// must carry `.mode(0o600)` somewhere in its own statement — the mode is looked for in the
    /// STATEMENT the creating call belongs to (found by walking outward to the nearest `;`/`{`/`}`
    /// on each side), not a fixed line window, so an unrelated `.mode(0o600)` on a neighbouring
    /// statement can no longer exempt this one by accident; and `std::fs::write(`/`fs::write(`,
    /// which has no mode parameter at all and so is unconditionally an offence — the fix at that
    /// call site is always to switch to `write_atomic` (0600) or an explicit `OpenOptions` chain.
    #[test]
    fn every_file_creation_in_telemetry_and_session_names_mode_0600() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // (file, 1-indexed line) pairs that are deliberately exempt, each with why.
        let allowlist: &[(&str, usize)] = &[];
        let mut offences: Vec<String> = Vec::new();
        let mut files = 0usize;
        walk_tree(&src, &mut |path: &std::path::Path, text: &str| {
            let rel = path
                .strip_prefix(&src)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            // Scope: every module known to write app-owned files into the shared runtime root or
            // the session directory, not only `telemetry/` + this file — `lib.rs`'s event/panic
            // log sink and `keymanager.rs`'s dev-only key file are app-owned data too.
            if !(rel.starts_with("telemetry/")
                || rel == "plex/session.rs"
                || rel == "lib.rs"
                || rel == "keymanager.rs")
            {
                return;
            }
            files += 1;
            let lines: Vec<&str> = text.lines().collect();
            // Brace-depth skip over `mod tests { … }`: test fixtures in every one of these files
            // write throwaway files with `std::fs::write`/`OpenOptions` on purpose (there is no
            // credential and no shared namespace in a `tempdir()`), and this test is about
            // PRODUCTION creation sites, not fixture setup — a naive scan of the whole file would
            // flag the fixtures the moment `fs::write` joined the needle set.
            let mut test_mod_depth: Option<i32> = None;
            for (i, line) in lines.iter().enumerate() {
                if let Some(depth) = test_mod_depth.as_mut() {
                    *depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
                    if *depth <= 0 {
                        test_mod_depth = None;
                    }
                    continue;
                }
                let trimmed = line.trim_start();
                // Any `mod <name>test<name> {` — this file's own test module is always `mod
                // tests`, but `lib.rs` also carries `redact_tests`/`private_log_tests` as
                // separate top-level modules, so match on the substring rather than the exact
                // conventional name.
                if let Some(rest) = trimmed.strip_prefix("mod ") {
                    let name_end = rest.find(|c: char| c == '{' || c.is_whitespace());
                    let name = &rest[..name_end.unwrap_or(rest.len())];
                    if name.contains("test") && line.contains('{') {
                        let depth =
                            line.matches('{').count() as i32 - line.matches('}').count() as i32;
                        if depth > 0 {
                            test_mod_depth = Some(depth);
                        }
                        continue;
                    }
                }
                // Skip comments outright — a doc comment discussing these needles in prose (this
                // function's own doc above, for instance) must never be graded as a creation site.
                if trimmed.starts_with("//") {
                    continue;
                }
                // Skip this test's own scanning code quoting the needles as string literals.
                if line.contains("line.contains(") || line.contains("trimmed.contains(") {
                    continue;
                }
                let builder_create = line.contains(".create(true)")
                    || line.contains(".create_new(true)")
                    || line.contains("File::create(")
                    || line.contains("File::create_new(");
                let unmodeable_write = line.contains("fs::write(");
                if !builder_create && !unmodeable_write {
                    continue;
                }
                if allowlist.contains(&(rel.as_str(), i + 1)) {
                    continue;
                }
                if unmodeable_write {
                    offences.push(format!(
                        "{rel}:{} creates a file via fs::write, which has no mode parameter\n    {}",
                        i + 1,
                        line.trim()
                    ));
                    continue;
                }
                // The mode must be set somewhere in the SAME STATEMENT as the creating call —
                // walk outward to the nearest statement boundary on each side rather than using a
                // fixed line count, so a `.mode(0o600)` belonging to a different, unrelated
                // statement cannot exempt this one.
                let (start, end) = statement_span(&lines, i);
                let window = lines[start..end].join("\n");
                if !window.contains(".mode(0o600)") {
                    offences.push(format!(
                        "{rel}:{} creates a file with no explicit .mode(0o600) in its own statement\n    {}",
                        i + 1,
                        line.trim()
                    ));
                }
            }
        });
        assert!(
            files >= 8,
            "the walk found only {files} files across the scanned scope — it is not reading the tree"
        );
        assert!(
            offences.is_empty(),
            "a file creation is missing an explicit 0600 mode:\n{}",
            offences.join("\n")
        );
    }

    // ---- issue #76 field report gap: per-candidate read rejections --------------------------

    /// A candidate that truly does not exist reads as `Missing`, never anything stronger — the
    /// one verdict this whole feature must not overstate.
    #[test]
    fn candidate_reads_records_missing_for_an_absent_candidate() {
        let _g = crate::testlock::serial();
        let _t = TempSession::new("candidate-missing");
        let _ = load();
        assert_eq!(candidate_reads_wire(), "other:missing");
        let cold = cold_storage_diagnostic().expect("a missing cold read is still diagnostic");
        assert!(cold.errors.is_empty());
        assert_eq!(cold.facts, ColdSessionFacts::default());
    }

    /// `open(2)` refusing for a reason OTHER than `ENOENT` — a denied parent directory — must be
    /// distinguished from a plain absence. Root bypasses a directory's mode entirely, so this is
    /// skipped there rather than faked.
    #[test]
    fn candidate_reads_records_open_failed_for_a_denied_parent_directory() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            eprintln!(
                "SKIP candidate_reads_records_open_failed_for_a_denied_parent_directory: \
                 running as root, which ignores a directory's own mode entirely"
            );
            return;
        }
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-{}-candidate-open-failed",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let locked_parent = dir.join("locked");
        std::fs::create_dir_all(&locked_parent).expect("a writable temp dir");
        let candidate = locked_parent.join("auth.json");
        std::fs::write(&candidate, b"{}").unwrap();
        std::fs::set_permissions(&locked_parent, std::fs::Permissions::from_mode(0o000))
            .expect("this test's own directory, not a device path");

        redirect_for_test(Some(candidate.clone()));
        let _ = load();
        let wire = candidate_reads_wire();
        redirect_for_test(None);

        // Restore access before cleanup can remove the directory.
        std::fs::set_permissions(&locked_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);

        assert!(
            wire.starts_with("other:open_failed:") || wire == "other:not_regular",
            "expected a rejected candidate, got {wire}"
        );
    }

    /// A directory sitting at the candidate NAME itself opens fine (POSIX allows `open(2)` on a
    /// directory read-only) but is not a regular file — `NotRegular`, not `Missing`.
    #[test]
    fn candidate_reads_records_not_regular_for_a_directory_at_the_candidate_path() {
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-{}-candidate-not-regular",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        let candidate = dir.join("auth.json");
        std::fs::create_dir_all(&candidate).unwrap();

        redirect_for_test(Some(candidate.clone()));
        let _ = load();
        redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(candidate_reads_wire(), "other:not_regular");
    }

    /// A file past the size cap is declined as `TooLarge`, distinct from every other rejection.
    #[test]
    fn candidate_reads_records_too_large_for_a_file_over_the_size_cap() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("candidate-too-large");
        let oversized = vec![b'a'; 4 * 1024 * 1024 + 1];
        std::fs::write(t.file(), &oversized).unwrap();
        let _ = load();
        assert_eq!(candidate_reads_wire(), "other:too_large");
    }

    /// A group/other-writable candidate is quarantined by [`read_locked`] and recorded as
    /// `UntrustedMode` — the CONTENT is never parsed either way, but the rejection is now visible
    /// rather than collapsing into `Missing`.
    #[test]
    fn candidate_reads_records_untrusted_mode_for_a_0666_file() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::new("candidate-untrusted-mode");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        std::fs::set_permissions(t.file(), std::fs::Permissions::from_mode(0o666)).unwrap();
        let _ = load();
        assert_eq!(candidate_reads_wire(), "other:untrusted_mode");
    }

    #[test]
    fn cold_snapshot_preserves_every_candidate_error_in_order() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TwoCandidateSession::new("cold-multiple-errors");
        for path in [t.higher(), t.lower()] {
            std::fs::write(&path, b"{}").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        }
        let _ = load();
        let cold = cold_storage_diagnostic().unwrap();
        assert_eq!(
            cold.errors.len(),
            2,
            "no candidate failure may be collapsed away"
        );
        assert!(cold.errors.iter().all(|error| {
            error.context.stage == crate::telemetry::storage::StorageStage::UntrustedMode
                && error.candidate == CandidateCategory::Other
        }));
    }

    /// The counterpart to every rejection above: a trusted, owned, regular file that parses as
    /// this build's plain session shape is recorded as accepted, not merely as "not rejected".
    #[test]
    fn candidate_reads_records_accepted_plaintext() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("candidate-accepted-plaintext");
        std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();
        let loaded = load();
        assert_eq!(loaded.account_token, "acct");
        assert_eq!(candidate_reads_wire(), "other:plaintext");
    }

    /// Same, for a secure envelope this launch can actually open — the candidate is recorded as
    /// `secure` the moment its shape is recognized, before the key-service round trip that
    /// follows decides whether the READ itself ends up `Ready`, corrupt or locked.
    #[test]
    fn candidate_reads_records_accepted_secure_for_an_openable_envelope() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("candidate-accepted-secure");
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed: crate::keymanager::Sealed {
                backend: crate::keymanager::Backend::Keymanager3,
                key: "plxnative.session.v1".to_string(),
                iv: "AAAAAAAAAAAAAAAAAAAAAA==".to_string(),
                // Ignored by the scripted backend below — `open_checked` takes the plaintext from
                // the scripted `finish` reply's own `output`, never from this field.
                data: "aWdub3JlZA==".to_string(),
                identity: crate::keymanager::Identity::Anonymous,
            },
        };
        std::fs::write(t.file(), serde_json::to_vec_pretty(&envelope).unwrap()).unwrap();

        crate::keymanager::arm_for_test(vec![
            (
                "begin",
                Ok(serde_json::json!({"returnValue": true, "handle": "h-dec"})),
            ),
            (
                "finish",
                Ok(serde_json::json!({
                    "returnValue": true,
                    // base64("{"client_id":"cid-secure","account_token":"acct-secure"}")
                    "output": "eyJjbGllbnRfaWQiOiJjaWQtc2VjdXJlIiwiYWNjb3VudF90b2tlbiI6ImFjY3Qtc2VjdXJlIn0="
                })),
            ),
        ]);
        let loaded = load();
        crate::keymanager::disarm_for_test();

        assert_eq!(loaded.account_token, "acct-secure");
        assert_eq!(candidate_reads_wire(), "other:secure");
    }

    /// **A candidate whose bytes are neither shape this build knows must still be RECORDED.** The
    /// concrete case is the zero-byte or truncated `auth.json` `write_atomic`'s own doc records the
    /// historical `O_TRUNC` write as having produced: it opens, it is owned, it is a regular file
    /// of a trusted mode, and it then parses as neither a `SecureEnvelope`, nor an
    /// envelope-shaped object, nor a `Session`. Before `Unparsable` existed that candidate fell off
    /// the bottom of `read_locked`'s loop with nothing recorded, so the summary a triager reads
    /// against `auth_paths()` said the top-priority candidate was ABSENT while a corrupt file sat
    /// there — the exact "a file that exists but was rejected reads as one that was never there"
    /// gap this whole lane exists to close.
    #[test]
    fn candidate_reads_records_unparsable_for_bytes_that_are_neither_shape() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("candidate-unparsable");
        std::fs::write(t.file(), b"").unwrap();
        let _ = load();
        assert_eq!(candidate_reads_wire(), "other:unparsable");
    }

    /// The same for bytes that are not empty but are not JSON this build recognizes either — a
    /// truncated write, not merely a zero-length one.
    #[test]
    fn candidate_reads_records_unparsable_for_a_truncated_file() {
        let _g = crate::testlock::serial();
        let t = TempSession::new("candidate-truncated");
        std::fs::write(t.file(), b"{\"client_id\":\"ci").unwrap();
        let _ = load();
        assert_eq!(candidate_reads_wire(), "other:unparsable");
    }

    /// **`WrongOwner` is not covered by a test here.** Reproducing it needs a file this test
    /// process does not own — a second uid — which nothing in this host suite can arrange
    /// (there is no setuid helper and this is not run as root by default). The read path itself
    /// is exercised for every other rejection above; `WrongOwner`'s own branch is a two-line
    /// `st_uid` comparison identical in shape to the ones the other tests already prove correct.
    #[test]
    fn candidate_reads_wrong_owner_is_not_testable_without_a_second_uid() {}

    /// The telemetry lane pins these wire words — a rename here silently breaks whatever reads
    /// `candidate_reads_wire()` downstream. `ALL` is the exhaustiveness source (via
    /// `_assert_all_variants_covered`, which fails to COMPILE the moment a new variant is added
    /// without being added there too), so this test catches a variant that exists but was never
    /// given a pinned word here.
    #[test]
    fn read_rejection_and_candidate_category_wire_words_round_trip() {
        let rejections: &[(ReadRejection, &str)] = &[
            (ReadRejection::Missing, "missing"),
            (ReadRejection::OpenFailed(13), "open_failed"),
            (ReadRejection::NotRegular, "not_regular"),
            (ReadRejection::WrongOwner, "wrong_owner"),
            (ReadRejection::MetadataFailed(5), "metadata_failed"),
            (ReadRejection::TooLarge, "too_large"),
            (ReadRejection::ReadFailed(9), "read_failed"),
            (ReadRejection::UntrustedMode, "untrusted_mode"),
            (ReadRejection::Unparsable, "unparsable"),
        ];
        assert_eq!(
            rejections.len(),
            ReadRejection::ALL.len(),
            "a ReadRejection variant was added without a pinned wire word in this test"
        );
        for (variant, word) in rejections {
            assert_eq!(variant.wire(), *word);
        }
        for errno_variant in [
            ReadRejection::OpenFailed(13),
            ReadRejection::MetadataFailed(5),
            ReadRejection::ReadFailed(9),
        ] {
            assert!(errno_variant.errno().is_some());
        }
        for no_errno_variant in [
            ReadRejection::Missing,
            ReadRejection::NotRegular,
            ReadRejection::WrongOwner,
            ReadRejection::TooLarge,
            ReadRejection::UntrustedMode,
            ReadRejection::Unparsable,
        ] {
            assert_eq!(no_errno_variant.errno(), None);
        }

        let categories: &[(CandidateCategory, &str)] = &[
            (CandidateCategory::Developer, "developer"),
            (CandidateCategory::Internal, "internal"),
            (CandidateCategory::AppDir, "app_dir"),
            (CandidateCategory::Runtime, "runtime"),
            (CandidateCategory::Other, "other"),
        ];
        assert_eq!(
            categories.len(),
            CandidateCategory::ALL.len(),
            "a CandidateCategory variant was added without a pinned wire word in this test"
        );
        for (variant, word) in categories {
            assert_eq!(variant.wire(), *word);
        }
    }

    /// The `[start, end)` line range of the statement containing line `i`: walk backward to just
    /// after the previous line that ends a statement or block (`;`, `{` or `}`), and forward to
    /// the first line that ends one — inclusive, since a creating call's own line commonly closes
    /// its statement too. A heuristic (it does not parse Rust, so a `;` inside a string or comment
    /// could mislead it), but good enough for the house style these builder chains are written in,
    /// and it is what makes `.mode(0o600)` scoped to THIS creation rather than a neighbour's.
    fn statement_span(lines: &[&str], i: usize) -> (usize, usize) {
        fn ends_statement(line: &str) -> bool {
            let t = line.trim_end();
            t.ends_with(';') || t.ends_with('{') || t.ends_with('}')
        }
        let mut start = i;
        while start > 0 && !ends_statement(lines[start - 1]) {
            start -= 1;
        }
        let mut end = i;
        while end < lines.len() && !ends_statement(lines[end]) {
            end += 1;
        }
        (start, (end + 1).min(lines.len()))
    }

    fn walk_tree(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_tree(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    f(&p, &t);
                }
            }
        }
    }

    /// **A real process boundary, not a cache reset.** The existing issue-#76 tests exercise the
    /// same file through `redirect_for_test`, but remain in one process and therefore cannot prove
    /// that the recovered plaintext is what a genuinely new launch reads. This test invokes the
    /// current test executable as a small worker twice: the first worker performs fresh reauth
    /// over a recognized envelope while the fake key service stalls; the second worker is a new
    /// process and loads the resulting file with the service still stalled.
    #[test]
    fn a_fresh_reauth_survives_a_real_process_boundary() {
        use std::process::Command;

        let Some(action) = std::env::var_os("PLXNATIVE_SESSION_BOUNDARY_ACTION") else {
            let _g = crate::testlock::serial();
            let dir = std::env::temp_dir().join(format!(
                "plxnative-session-boundary-{}-{}",
                std::process::id(),
                crate::diag::random_hex_id().unwrap_or_else(|| "test".into())
            ));
            std::fs::create_dir_all(dir.join("state")).expect("boundary state directory");
            let file = dir.join("state/session.json");
            crate::paths::redirect_persistent_state_root_for_test(Some(dir.join("state")));
            assert!(matches!(
                persistence::commit_data(String::from_utf8(locked_envelope_bytes()).unwrap()),
                persistence::CanonicalCommit::Durable { .. }
            ));

            let run = |action: &str| {
                Command::new(std::env::current_exe().expect("test executable"))
                    .arg("plex::session::tests::a_fresh_reauth_survives_a_real_process_boundary")
                    .arg("--exact")
                    .arg("--nocapture")
                    .env("PLXNATIVE_SESSION_BOUNDARY_ACTION", action)
                    .env("PLXNATIVE_SESSION_BOUNDARY_FILE", &file)
                    .output()
                    .expect("boundary worker output")
            };

            let stalled = run("stalled");
            assert!(
                stalled.status.success(),
                "stalled worker failed: {stalled:?}"
            );
            assert!(
                String::from_utf8_lossy(&stalled.stdout).contains("1 passed"),
                "stalled worker did not execute exactly one test: {}{}",
                String::from_utf8_lossy(&stalled.stdout),
                String::from_utf8_lossy(&stalled.stderr)
            );
            let first = run("reauth");
            assert!(
                first.status.success(),
                "fresh-reauth worker failed: {first:?}"
            );
            assert!(String::from_utf8_lossy(&first.stdout).contains("1 passed"));
            let second = run("load");
            assert!(
                second.status.success(),
                "new-process load worker failed: {second:?}"
            );
            assert!(String::from_utf8_lossy(&second.stdout).contains("1 passed"));
            let payload = match persistence::load() {
                persistence::CanonicalRead::Data { payload, .. } => payload,
                _ => panic!("boundary canonical payload missing"),
            };
            assert_eq!(
                serde_json::from_str::<Session>(&payload)
                    .unwrap()
                    .account_token,
                "acct"
            );
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };

        let file =
            std::env::var_os("PLXNATIVE_SESSION_BOUNDARY_FILE").expect("boundary worker file");
        redirect_for_test(Some(file.clone().into()));
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        match action.to_string_lossy().as_ref() {
            "stalled" => {
                let before = std::fs::read(&file).expect("original envelope");
                let loaded = load();
                assert!(loaded.account_token.is_empty());
                assert_eq!(std::fs::read(&file).unwrap(), before);
            }
            "reauth" => {
                let _ = load();
                let mut s = signed_in();
                s.server.machine_id = "machine-boundary".into();
                s.server.address = "192.0.2.10".into();
                s.server.port = 32400;
                s.server.token = "pms-token".into();
                assert!(save_after_reauthentication(&s).persisted());
            }
            "load" => {
                let s = load();
                assert_eq!(s.account_token, "acct");
                assert_eq!(s.server.machine_id, "machine-boundary");
                assert_eq!(s.pms_token(), "pms-token");
                assert!(s.can_go_local());
            }
            other => panic!("unknown boundary worker action: {other}"),
        }
        crate::keymanager::disarm_for_test();
        redirect_for_test(None);
    }
}
