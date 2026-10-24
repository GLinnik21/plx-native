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
//! ## The live read cache
//!
//! [`peek`] (and [`load`] on a hit) answers from an in-memory cache rather than [`IO`] — see
//! [`CACHE`] for the mechanism. The invariants that make it safe to trust:
//!
//! - A cached record is backed by a completed read under [`IO`] or a `Durable` write under `IO`.
//!   The separate `Revoked` arm is a local sign-out, never a claim about durable storage.
//!   Reads, refused updates, and routine preference writes cannot replace it; only an explicit
//!   proven credential write through `install_proven_locked` may restore a session.
//! - Only this module writes the session domain of the persisted record; every
//!   `persistence::commit_*`/`write_session`/`commit_cleared` caller here ends by calling exactly
//!   a read install, proven-write install, cache drop, or local revocation.
//! - `peek` is a pure read of memory: it takes no `IO`, submits nothing and reads no clock. A miss
//!   returns the last snapshot (or the empty session). The refill is the session owner's job:
//!   `refresh_need` says what the cache lacks, the app's frame step hands it to the owner, and the
//!   owner's `RefreshCache` effect runs `refresh_cache_job` on the bounded persistence FIFO. The
//!   job reads and installs under `IO`; `CACHE` is never held across `IO`. Queued and in-flight
//!   reads cannot overwrite a later revocation (`CACHE_GENERATION` and `replace_cache`'s Revoked
//!   guard). Cached views poll `VisibleSessionWatch` on Tick; `peek_settled` distinguishes recovery
//!   in progress from an authoritative empty session, so pending preferences need not flash defaults.
//! - Writers never read from the cache; they read the authority under `IO` (fence correctness),
//!   then install their own outcome over it.
//! - A `Locked`/`Blocked` read or fallback `Ready` during helper unavailability is transient: it
//!   is served as the last known state, and the owner re-reads it no sooner than `LOCKED_RETRY_MS`
//!   of frame time later (per generation), rather than latching it forever.
//! - [`clear`] (sign-out) drops the cached `Arc` immediately — it holds the very account/server
//!   tokens sign-out means to get rid of.
//! - Other domains of the same record (consent) are not in this cache and their writes never
//!   touch it. `async_persistence`'s own `CACHE`/`LOCKED_STATE` are a separate, currently-unwired
//!   engine (see its doc) — when it is wired, its coordinator must install into and drop THIS
//!   cache rather than keep a second copy of the session live.
use super::origin::Origin;
use super::probe::Location;
pub use prefs::{HoldHintTaughtPref, effective, own_prefs, persist_profile_pref, put_own, stored_effective, tenure, AudioEnhancementsPref,
    DeckPressPref, DirectPlayModePref, HdrOverlayBrightnessPref, NextEpisodeModePref, Preferences, ProfilePrefs, ProfilePrefsPref,
    ProfilePrefsRecord, ProfileSetting, ProfileWrite, QualityPref, Row, Setting, SkipIntervalPref,
    SubtitlePositionPref, SubtitleSizePref, SubtitleTonePref, TrailerAutoplayPref, UiResolutionPref, UnderPostersPref};
use prefs::ProfileMemory;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use std::sync::Mutex;
use std::collections::BTreeMap;
use serde_json::Value;

/// The signed-in profile, in-memory for the UI (the Home profile chip reads this). Set by the boot
/// gate (from the stored session) and on every profile switch, so it survives an offline boot.
static CURRENT: Mutex<Option<std::sync::Arc<CurrentProfile>>> = Mutex::new(None);

/// Immutable worker publication. Identity and its explicit owner-assigned generation are one
/// record, so a reader that needs both can retain one snapshot across subsequent publications.
pub struct CurrentProfile {
    pub user: Option<UserRef>,
    pub generation: u32,
    /// `grant::account_key` of the account token the owner published this profile under: the half of
    /// the [`Seat`] that says WHICH sign-in the profile belongs to. Empty before a sign-in and in a
    /// publication recorded before the field existed.
    pub stamp: String,
}

impl CurrentProfile {
    /// Who this publication seats. A publication with no profile seats nobody, and so does one with
    /// no stamp (a dev token, an older recording): without a stamp a stored per-profile value cannot
    /// be told from another sign-in's, so nothing may be read from or written to a record.
    pub fn seat(&self) -> Seat {
        Seat::of(self.user.as_ref(), &self.stamp)
    }
}

/// **Whose per-profile settings the TV is showing**, as a value that can be compared, captured on the
/// main thread and carried to a worker. `current_profile_key()` cannot say this: it is `""` for the
/// account owner AND for "no profile published yet", and it carries no sign-in.
///
/// * [`Seat::Unseated`] is the process before a profile is chosen, the boot picker until a choice, a
///   change-profile, a sign-out, a dev token and a controlled replay. Nothing is restored for it and
///   nothing may be written for it.
/// * [`Seat::Seated`] names the Plex Home user's `uuid` (`""` for the account owner) and the
///   account stamp. Two seats are the same seat only when both match, so a re-login as the same
///   person with a different token is a new seat.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum Seat {
    #[default]
    Unseated,
    Seated { user: String, stamp: String },
}

impl Seat {
    /// The seat a publication of `user` under the account `stamp` occupies. Nobody published seats
    /// nobody, and so does a profile with no stamp (a dev token, an older recording): without a stamp
    /// a stored per-profile value cannot be told from another sign-in's, so nothing may be read from
    /// or written to a record. The unstamped case is logged once, so a field report would show it.
    pub fn of(user: Option<&UserRef>, stamp: &str) -> Seat {
        match user {
            Some(user) if !stamp.is_empty() => Seat::Seated { user: user.uuid.clone(), stamp: stamp.to_string() },
            Some(_) => {
                static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    plx_base::eventlog::log("session: a profile was published without an account stamp; unseated");
                }
                Seat::Unseated
            }
            None => Seat::Unseated,
        }
    }

    /// The seated profile's key (`""` for the owner), or `None` while unseated.
    pub fn user(&self) -> Option<&str> {
        match self {
            Seat::Seated { user, .. } => Some(user),
            Seat::Unseated => None,
        }
    }
}

pub fn current_snapshot() -> std::sync::Arc<CurrentProfile> {
    CURRENT.lock().unwrap_or_else(|e| e.into_inner()).as_ref().cloned()
        .unwrap_or_else(|| std::sync::Arc::new(CurrentProfile { user: None, generation: 0, stamp: String::new() }))
}

/// The seat the published profile occupies — [`CurrentProfile::seat`] of [`current_snapshot`].
pub fn current_seat() -> Seat {
    current_snapshot().seat()
}

/// Whether the profile `user` (`""` for the account owner) is the one seated now: the allocation-free
/// question the memory door asks on every recall.
pub fn is_seated_as(user: &str) -> bool {
    let profile = current_snapshot();
    !profile.stamp.is_empty() && profile.user.as_ref().is_some_and(|u| u.uuid == user)
}

/// Resource-side capability; constructing it borrows, but does not duplicate or retain, the
/// engine's MainThread token. The Session adapter holds it and supplies the owner's generation.
pub struct ProfilePublisher {
    scoped: Option<std::sync::Arc<CurrentProfile>>,
    _main_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl ProfilePublisher {
    pub fn new(_mt: &plx_base::task::MainThread) -> Self {
        Self { scoped: None, _main_thread: std::marker::PhantomData }
    }
    pub fn scoped(_mt: &plx_base::task::MainThread) -> Self {
        Self { scoped: Some(std::sync::Arc::new(CurrentProfile { user: None, generation: 0, stamp: String::new() })),
            _main_thread: std::marker::PhantomData }
    }
    pub fn snapshot(&self) -> std::sync::Arc<CurrentProfile> {
        self.scoped.clone().unwrap_or_else(current_snapshot)
    }
    /// Whether this publisher keeps its publications to itself (a controlled replay): they never
    /// reach the process global, so they seat nobody in the process.
    pub fn is_scoped(&self) -> bool {
        self.scoped.is_some()
    }
    /// A user-confirmed exit from recording resumes ordinary live publication, preserving
    /// the last owner-supplied scope. Replay never receives this transition capability.
    pub fn resume_live(&mut self) {
        if let Some(scoped) = self.scoped.take() {
            self.publish(scoped.user.clone(), scoped.generation, scoped.stamp.clone());
        }
    }
    /// Publish `user` as the active profile under the owner's `generation` and the account `stamp`
    /// (`grant::account_key` of the account token) the owner published it under.
    pub fn publish(&mut self, user: Option<UserRef>, generation: u32, stamp: String) {
        #[cfg(any(test, feature = "test-support"))]
        self.publish_with(user, generation, stamp, |_, _| {});
        #[cfg(not(any(test, feature = "test-support")))]
        self.publish_with(user, generation, stamp, |user, generation| {
            super::account::publish_audio_preferences_profile(user.as_ref(), generation);
            let Some(user) = user else { return; };
            let client_id = peek().client_id.clone();
            let Some(credential) = plex_tv_credential(&user) else { return; };
            super::account::warm_audio_preferences(client_id, credential, user, generation);
        });
    }
    fn publish_with<W>(&mut self, user: Option<UserRef>, generation: u32, stamp: String, warm: W)
    where W: FnOnce(Option<UserRef>, u32) {
        if let Some(scoped) = &mut self.scoped {
            *scoped = std::sync::Arc::new(CurrentProfile { user, generation, stamp });
            return;
        }
        {
            *CURRENT.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(
                CurrentProfile { user: user.clone(), generation, stamp }));
        }
        warm(user, generation);
    }
    #[cfg(any(test, feature = "test-support"))]
    fn publish_with_warmer_for_test<W>(&mut self, user: Option<UserRef>, generation: u32, warm: W)
    where W: FnOnce(Option<UserRef>, u32) {
        self.publish_with(user, generation, String::new(), warm);
    }
}

/// Resource fixtures supply their generation explicitly and use the real publication writer.
/// This is not a controller or a process-global scope allocator. Caller owns teardown/serial guard.
#[cfg(any(test, feature = "test-support"))]
pub fn publish_profile_for_test(user: Option<UserRef>, generation: u32) {
    publish_seat_for_test(user, generation, String::new());
}

/// [`publish_profile_for_test`] with the account `stamp`, so the published profile occupies a
/// [`Seat`]. The plain fixture publishes none (an unstamped publication seats nobody), which keeps
/// every test that does not care about a seat unseated.
#[cfg(any(test, feature = "test-support"))]
pub fn publish_seat_for_test(user: Option<UserRef>, generation: u32, stamp: String) {
    plx_base::testlock::assert_held("profile publication fixture");
    let mt = unsafe { plx_base::task::MainThread::assume() };
    ProfilePublisher::new(&mt).publish(user, generation, stamp);
}
/// The active profile (name + avatar), if any. Empty title = the owner with no Plex Home selection.
pub fn current() -> Option<UserRef> {
    current_snapshot().user.clone()
}
/// The generation assigned by the Session owner and published with this profile.
pub fn current_gen() -> u32 {
    current_snapshot().generation
}

/// Credential for a plex.tv call made on behalf of one captured active-profile snapshot.
///
/// New sessions carry the profile's own account-service token explicitly. The only fallback is
/// for legacy owner sessions written before that field existed: the stored and captured profile
/// must be the same identity, and the persisted roster must prove owner scope. A managed or
/// unknown legacy profile therefore skips the optional call instead of borrowing either the
/// owner's account token or its own unrelated PMS token.
pub fn plex_tv_credential(snapshot_user: &UserRef) -> Option<String> {
    if let Some(token) = snapshot_user.plex_tv_token.as_ref()
        .filter(|token| !token.trim().is_empty())
    {
        return Some(token.clone());
    }
    let stored = peek();
    let same_profile = if snapshot_user.uuid.is_empty() {
        stored.user.uuid.is_empty() && snapshot_user.id == stored.user.id
    } else {
        snapshot_user.uuid == stored.user.uuid
    };
    (same_profile && stored.active_profile_is_admin())
        .then(|| stored.account_token.clone()).filter(|token| !token.is_empty())
}

/// Session file locations, best first — see [`plx_base::paths::session_candidates`] for why this is a
/// SEARCH ORDER rather than the single constant it used to be. The short version: webOS picks one
/// of two jail profiles by install prefix, and they disagree about which directories are writable,
/// so the one hardcoded path was correct under Developer Mode and did not exist under a Homebrew
/// Channel install — where `save()` then dropped the error and the user re-did the QR sign-in on
/// every boot, with a fresh `X-Plex-Client-Identifier` each time.
///
/// The first entry is still deliberately OUTSIDE the app install dir: appinstalld replaces
/// `applications/com.beb.plxnative/` wholesale on every ipk (re)install, which silently signed the
/// user out when the file lived there.
#[cfg(not(any(test, feature = "test-support")))]
fn auth_paths() -> Vec<std::path::PathBuf> {
    plx_base::paths::session_candidates()
}

/// The test build's [`auth_paths`]: the scratch file a test redirected to (see
/// `tests::TempSession`), else this PROCESS's own scratch file. A `#[cfg(test)]` global, so a
/// shipped binary has neither the static nor the branch — the file this module writes on a
/// television is decided by `paths.rs` and by nothing else.
///
/// It exists because there is no other way to exercise the writing half at all: every candidate
/// `paths.rs` offers is either a device path that does not exist on the dev Mac or — for
/// `in_app_dir` — the directory the test binary itself is running from, which is a real writable
/// path, so a careless test would leave a credentials-shaped file in `target/`.
#[cfg(any(test, feature = "test-support"))]
static TEST_FILE: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// **The fallback is a scratch file, NEVER [`plx_base::paths::session_candidates`].**
///
/// That fall-through was the whole bug. Off a television the real search order ends at
/// `paths::in_app_dir("auth.json")`, and for a test binary `in_app_dir` resolves through
/// `current_exe()` to the directory the binary runs from — `rust-modules/target/<profile>/deps/`.
/// So the host suite read and wrote a real session file that no test owned, one per checkout,
/// surviving every run. Three consequences — the first MEASURED, the second and third read off
/// the code that produced it:
///
/// * **A test's answer was decided by that file.** `browse::append_sections` calls `resolve_pins`,
///   which reads this module for the current profile's `home_pins` — so EVERY use of
///   `browse::seed_two_source_table_for_test` resolved its favourite libraries against whatever
///   record happened to be on that developer's disk. A record for the empty profile key naming
///   the fixture's own machines (`mac-mini`, `nas-home`) with Movies switched off deletes the
///   Movies pill, and `app::bridge`'s `library_publishes_the_actual_container_strip` and
///   `app::chrome`'s `four_libraries_on_two_servers_publish_two_type_destinations` then failed
///   ALONE, single-threaded, in one checkout while passing in another built from the same commit.
/// * **The suite WROTE it.** [`update`] resolves `auth_paths()` once for its read and again for
///   its write, and `redirect_for_test` used to move `TEST_FILE` without holding [`IO`] — so a
///   redirect landing between the two halves of somebody else's read-modify-write put a scratch
///   session's contents, `home_pins` and all, at the persistent path. That is the ONLY route to
///   the record above this module offers — no test records pins with the real path in play, and
///   the whole suite single-threaded leaves `home_pins` empty — and it fits its one odd feature,
///   the EMPTY profile key, which is what a `TempSession` with no `watching` has. Inferred, not
///   caught in the act: the interleaving is narrow, which is also why the failure arrived as an
///   occasional red rather than all at once. `redirect_for_test` takes `IO` now.
/// * **`save_locked` DELETES the losing candidates.** With the real order in play that is a test
///   binary reaching for `pkg/auth.json` and the two `/media/…` paths.
///
/// Per process rather than per test: `TempSession` is how a test gets a file of its own, and this
/// is only the neutral floor beneath it — empty at every start, so nothing an earlier RUN left
/// behind can be read, and nothing this run writes can outlive it.
#[cfg(any(test, feature = "test-support"))]
fn fallback_file() -> std::path::PathBuf {
    static PATH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    // Removed when the test process exits (`plx_base::testscratch`).
    PATH.get_or_init(|| plx_base::testscratch::process_dir("session-fallback").join("auth.json"))
        .clone()
}

/// The same process-global scratch path [`fallback_file`] resolves to, exposed to other modules'
/// test code (the adapter regression test for the Finding 1 canonical-verdict path) so it can
/// snapshot and restore the file rather than leaving residue for whichever other test falls
/// through to it next.
#[cfg(any(test, feature = "test-support"))]
pub fn fallback_file_for_test() -> std::path::PathBuf {
    fallback_file()
}

/// A whole CANDIDATE LIST a test wants `auth_paths()` to answer, distinct from [`TEST_FILE`]'s
/// single scratch path. [`TEST_FILE`] can only ever stand for the ONE file a fixture like
/// [`TempSession`] owns; it cannot represent "several candidates, some unwritable, in a specific
/// order" — exactly the shape [`plx_base::paths::session_candidates`] itself has, and exactly what
/// the runtime-dir fallback regression needs to exercise the real search-and-fall-through loops
/// in [`save_legacy_fallback_locked`]/[`read_legacy_locked`] rather than a hand-rolled stand-in
/// for them.
#[cfg(any(test, feature = "test-support"))]
static TEST_CANDIDATES: Mutex<Option<Vec<std::path::PathBuf>>> = Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
fn auth_paths() -> Vec<std::path::PathBuf> {
    if let Some(list) = TEST_CANDIDATES.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return list;
    }
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(p) => vec![p],
        None => vec![fallback_file()],
    }
}

/// Point [`auth_paths`] at a whole candidate LIST of a test's own, or back at the ordinary
/// [`TEST_FILE`]/[`fallback_file`] resolution with `None` — the multi-candidate sibling of
/// [`redirect_for_test`], for a fixture that needs several paths (some unwritable) in a specific
/// order rather than one scratch file.
#[cfg(any(test, feature = "test-support"))]
fn redirect_candidates_for_test(v: Option<Vec<std::path::PathBuf>>) {
    *TEST_CANDIDATES.lock().unwrap_or_else(|e| e.into_inner()) = v;
}

/// Point this module's file at `p`, or back at [`fallback_file`] with `None`.
///
/// `pub` because the writing half is no longer only this module's business: `browse`'s
/// per-profile Home selection round-trips through this file, and grading THAT end to end is the
/// only way to catch the shape of bug it exists to prevent (one profile's answer overwriting
/// another's), which no in-memory fixture can see.
///
/// The caller owes the same discipline `tests::TempSession` documents: hold
/// [`plx_base::testlock::serial`] for the whole test, because this is a crate global and several
/// modules reach `session::load` indirectly.
///
/// **It takes [`IO`] to make the swap, and that is not tidiness.** [`update`] is a read-modify-write
/// that resolves [`auth_paths`] TWICE — once for `peek_locked`, once inside `save_locked` — so a
/// redirect moving between the two makes it read one file and write another. That is a transplant:
/// a scratch session's contents, `home_pins` and all, land at whatever path the second resolution
/// answers. Taking `IO` here means a redirect can only ever move between complete cycles, so both
/// halves of every read-modify-write see one file. (Callers hold `testlock::serial`, which
/// serializes the TESTS — but a test writing the session does not have to be the test that moved
/// the redirect, and the crate lock cannot see that pairing.)
#[cfg(any(test, feature = "test-support"))]
pub fn redirect_for_test(p: Option<std::path::PathBuf>) {
    plx_base::storage_worker::drain_for_test();
    let _io = io();
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p;
    // A test swapping the scratch file changes what `peek` answers just as surely as a write
    // does — a cache primed against the old file must not survive the swap. See `CACHE`.
    replace_cache(Cached::Unloaded, None, true);
}

/// Snapshot the current [`TEST_FILE`] redirect so a caller can restore it exactly with
/// `redirect_for_test`, rather than assuming `None` is always the value to go back to.
#[cfg(any(test, feature = "test-support"))]
pub fn redirect_snapshot_for_test() -> Option<std::path::PathBuf> {
    TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// **A signed-in session at a scratch path, taken back on drop — THE guard, not one of several.**
///
/// Any test that reads or writes a per-profile decision (the favourite libraries above all, since
/// the tab strip is a projection of them now) is otherwise graded against whatever `auth.json`
/// happens to be on the developer's own machine — and worse, WRITES its fixtures there: `make
/// check` was seeding this household's real session file with machines called `mac-mini` and
/// `nas-home` and then reading them back one test later, which is how a strip that resolves
/// `[Movie, Show]` on the maintainer's Mac resolves something else on a runner.
///
/// Three near-identical copies of this existed — `browse`'s `TempPins`, `ui::onboard`'s
/// `TempSession` and an inline one in `auth` — and `onboard`'s own doc comment already said this
/// module owned the original, which it did not. It does now. A screen with its own globals to put
/// back wraps this one and adds its teardown to the wrapper's `Drop` (see `screens::onboard`,
/// which is where that screen and its `TempSession` wrapper live since phase 5b), rather
/// than forking the redirect a fourth time.
///
/// **The caller must hold [`plx_base::testlock::serial`] for its whole body** — the redirected path is
/// a crate global that several modules reach indirectly, so two of these at once is one test
/// reading the other's fixtures.
#[cfg(any(test, feature = "test-support"))]
pub struct TempSession {
    dir: std::path::PathBuf,
}

#[cfg(any(test, feature = "test-support"))]
impl TempSession {
    /// A session with a `client_id` and **no current profile**. An empty `client_id` makes
    /// [`update`] a silent no-op by design, so seeding one is what makes a per-profile write
    /// observable at all; leaving the profile unset is the neutral start, since a test that cares
    /// which profile it is says so with [`TempSession::watching`].
    pub fn new(tag: &str) -> TempSession {
        let dir =
            std::env::temp_dir().join(format!("plxnative-session-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        redirect_for_test(Some(dir.join("auth.json")));
        save(&Session {
            client_id: "cid-test".into(),
            ..Default::default()
        });
        TempSession { dir }
    }

    /// **The scratch file itself** — for a test that grades whether something WROTE it.
    ///
    /// [`load`] re-persists a plaintext session on every call, so "did this code path write the
    /// session file" is a real, gradeable question about a screen, and the only honest way to ask
    /// it is against the bytes on disk. `write_atomic` renames a fresh temp file into place, so an
    /// unchanged inode is the discriminator that cannot depend on a filesystem's mtime resolution.
    pub fn path(&self) -> std::path::PathBuf {
        self.dir.join("auth.json")
    }

    /// Resource tests assert the actual read/write/clear candidate list before touching it.
    pub fn assert_only_target(&self) {
        plx_base::testlock::assert_held("Session scratch resource target");
        assert_eq!(auth_paths(), vec![self.path()]);
    }

    /// Publish a fixture profile through the resource writer; the test supplies the next scope.
    /// Every per-profile decision keys on this publication ([`current_profile_key`]).
    pub fn watching(&self, uuid: &str) {
        publish_profile_for_test(Some(UserRef {
            uuid: uuid.into(),
            ..Default::default()
        }), current_gen().wrapping_add(1));
    }

    /// **Sign the fixture in and seat `uuid`**: store `account_token` in the scratch record, then
    /// publish the profile with the stamp of that token, so [`current_seat`] is
    /// [`Seat::Seated`] and a per-profile write has somewhere to land. [`TempSession::watching`]
    /// publishes no stamp (an unstamped publication seats nobody), which is what every test that
    /// does not care about a seat wants. Returns the stamp.
    pub fn seated(&self, uuid: &str, account_token: &str) -> String {
        assert!(update(|cur| {
            let mut next = cur.clone();
            next.account_token = account_token.into();
            Some(next)
        }), "the fixture session takes the account token");
        let stamp = super::grant::account_key(account_token);
        publish_seat_for_test(Some(UserRef { uuid: uuid.into(), ..Default::default() }),
            current_gen().wrapping_add(1), stamp.clone());
        stamp
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TempSession {
    fn drop(&mut self) {
        // A new fixture must not reuse the scope that recents/directory resources cached.
        // This test-owned generation is supplied explicitly; production only uses Session's.
        publish_profile_for_test(None, current_gen().wrapping_add(1));
        redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OpaqueExtensions(pub BTreeMap<String, Value>);

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


/// The full persisted session. Empty fields mean "not logged in yet" for that stage.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Session {
    /// Install-wide UI language, applied on the next process launch. Absent means System, and
    /// System is not written, so a session that never chose a language serializes exactly as it
    /// did before localization — committed replay initials and the owner digest included.
    #[serde(default, skip_serializing_if = "plx_platform::i18n::Preference::is_system")]
    pub language: plx_platform::i18n::Preference,
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
    /// empty-query state offers back (`crate::search::recents` owns the cap, the
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
    /// **Every profile this television has switched to, with the credentials that switch
    /// resolved** — so the who's-watching picker can seat a household member with plex.tv
    /// unreachable. Written by the profile switch on every ONLINE success (replace-by-uuid), read
    /// by `crate::auth`'s offline fallback, and gone with the file on sign-out.
    ///
    /// It exists because the offline design's first cut let only the already-active, PIN-free
    /// profile through without a network, and the first real outage (2026-09-06) showed what that
    /// is worth in a house whose active profile is the PIN-protected admin: nothing. A protected
    /// entry carries a [`PinVerifier`]; an unprotected one carries `None` and is seated on a pick.
    ///
    /// **Several profiles' server tokens in one file is not a new exposure.** The same file holds
    /// `account_token`, which mints every one of them online (`/api/v2/home/users/{uuid}/switch`),
    /// and it is 0600 or sealed by the key manager either way. What a reader of this file could
    /// NOT do before is walk past a PIN, which is why the PIN itself is never here — see
    /// [`PinVerifier`] for exactly what is.
    ///
    /// Soft-parsed like every list in this struct: an entry costs itself, never the credentials.
    #[serde(default, deserialize_with = "de_profile_cache")]
    pub profiles: Vec<ProfileCreds>,
    /// Every preference key but `language` (`ui_resolution`, `hold_hint_taught`, `profile_prefs`, the per-profile home of the ten player
    /// settings and of hero trailer autoplay, and those settings' frozen top-level keys every profile
    /// falls back to (`trailer_autoplay` among them), `auto_sign_in`,
    /// `last_hero_blur`, `plaintext_consent`, `server_key_pins`, `last_library`,
    /// `library_sorts`, `track_memory`, `subtitle_offsets`), flattened so the keys sit at the top level of the file under
    /// the same keys they always had. Declared before [`Session::extensions`]:
    /// a flattened struct takes its keys first, so the unknown-key map sees only keys nobody models.
    #[serde(flatten)]
    pub prefs: Preferences,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CanonicalSessionAuth {
    format: String,
    version: u32,
    #[serde(default)]
    profiles: Option<Value>,
    account_token: String,
    server: ServerRef,
    user: UserRef,
    home_users: Vec<HomeUserRef>,
    sources: Vec<SourceRef>,
    /// Unknown top-level fields may contain credentials introduced by a newer client.  Protect
    /// them by default instead of guessing that an unfamiliar value is a harmless preference.
    extensions: OpaqueExtensions,
}

#[derive(Serialize, Deserialize)]
struct CanonicalSessionPreferences {
    #[serde(default)]
    language: plx_platform::i18n::Preference,
    /// Every flattened preference key (the ten player settings, hero trailer autoplay, the A5 rows and the two memory-door
    /// lists), flattened ahead of `extensions` so a flattened struct takes its keys first and
    /// `extensions` still sees only unknown ones.
    #[serde(flatten)]
    prefs: Preferences,
    /// Parsed only so a future preference does not make the known fields disappear. The shipping
    /// adapter merges these opaque keys from the current DB8 public payload before every rewrite;
    /// they are not promoted into the Session domain object.
    #[serde(flatten)]
    extensions: BTreeMap<String, Value>,
}

/// `#[derive(Default)]` would give `trailer_autoplay: false` (the plain bool default), which is
/// what `.unwrap_or_default()` falls back to when `preferences` isn't even an object (null,
/// absent, or corrupt) — silently contradicting #92's "absence is on" contract, since a per-field
/// `#[serde(default = "default_true", ...)]` only fires for a missing KEY inside an object being
/// deserialized, never for the whole-value fallback used here. Every other field's honest
/// "unknown" value happens to coincide with a bare derived default, which is why only this one
/// needed a manual impl.
impl Default for CanonicalSessionPreferences {
    fn default() -> Self {
        Self {
            language: plx_platform::i18n::Preference::System,
            prefs: Preferences { trailer_autoplay: true, ..Preferences::default() },
            extensions: BTreeMap::new(),
        }
    }
}

/// Split a typed session at the encryption boundary used by the DB8 helper.
///
/// The returned public object is still protected by the helper-owned private DB8 kind, but it is
/// deliberately readable while Keymanager is unavailable.  The returned string contains every
/// credential and all unknown extensions and must only cross the authenticated helper socket.
#[allow(dead_code)] // Connected by the Stage B Session adapter.
pub fn split_canonical(
    session: &Session,
) -> Result<(plx_platform::storage::state::PublicPayload, String), ()> {
    // `profiles` in a v1 extension is opaque, never an active credential cache. Refuse an
    // ambiguous v2 write; only the typed field is permitted to carry active credentials.
    if session.extensions.0.contains_key("profiles") { return Err(()); }
    let auth = serde_json::to_string(&CanonicalSessionAuth {
        format: "plxnative-session-auth".into(),
        version: 2,
        profiles: Some(serde_json::to_value(valid_profiles(session.profiles.clone())).map_err(|_| ())?),
        account_token: session.account_token.clone(),
        server: session.server.clone(),
        user: session.user.clone(),
        home_users: session.home_users.clone(),
        sources: session.sources.clone(),
        extensions: session.extensions.clone(),
    })
    .map_err(|_| ())?;
    Ok((split_public(session)?, auth))
}

fn split_public(session: &Session) -> Result<plx_platform::storage::state::PublicPayload, ()> {
    let mut preferences = serde_json::to_value(CanonicalSessionPreferences {
        language: session.language,
        prefs: session.prefs.clone(),
        extensions: BTreeMap::new(),
    })
    .map_err(|_| ())?;
    // `plaintext_consent` is written as `[]` in the legacy file (its row has no skip attribute) but
    // the canonical record has always omitted it while empty: one flattened field carries one
    // attribute set, so the canonical form drops the empty key here (pinned by
    // `prefs_characterisation_tests`, the default session's canonical record).
    if session.prefs.plaintext_consent.is_empty() {
        if let Some(object) = preferences.as_object_mut() {
            object.remove("plaintext_consent");
        }
    }
    let pins = serde_json::to_value(&session.home_pins).map_err(|_| ())?;
    let recents = serde_json::to_value(&session.recent_searches).map_err(|_| ())?;
    Ok(plx_platform::storage::state::PublicPayload {
            preferences,
            client_id: (!session.client_id.is_empty()).then(|| session.client_id.clone()),
            // Profile/server bootstrap metadata is personal and only useful together with its
            // token, so it stays in CanonicalSessionAuth rather than being duplicated here.
            profile: Value::Null,
            pins,
            recents,
            consent: Value::Null,
            scopes: Value::Null,
            ids: Value::Null,
            account_extensions: Value::Null,
        })
}

/// Reassemble the domain type after the helper has opened the protected auth payload.
///
/// Public preferences degrade independently: one malformed optional setting must not discard a
/// valid token bundle.  The protected half is strict because accepting the wrong auth schema as a
/// session would turn corruption into an authenticated state.
#[allow(dead_code)] // Connected by the Stage B Session adapter.
pub fn join_canonical(
    public: &plx_platform::storage::state::PublicPayload,
    protected: &str,
) -> Result<Session, ()> {
    let auth: CanonicalSessionAuth = serde_json::from_str(protected).map_err(|_| ())?;
    if auth.format != "plxnative-session-auth" || !matches!(auth.version, 1 | 2) {
        return Err(());
    }
    let profiles = match (auth.version, auth.profiles) {
        (1, None) => Vec::new(),
        (2, Some(Value::Array(entries))) if !auth.extensions.0.contains_key("profiles") => {
            parse_profiles(entries)
        }
        _ => return Err(()),
    };
    let preferences = serde_json::from_value::<CanonicalSessionPreferences>(
        public.preferences.clone(),
    )
    .unwrap_or_default();
    let home_pins = serde_json::from_value(public.pins.clone()).unwrap_or_default();
    let recent_searches = serde_json::from_value(public.recents.clone()).unwrap_or_default();
    Ok(Session {
        client_id: public.client_id.clone().unwrap_or_default(),
        account_token: auth.account_token,
        server: auth.server,
        user: auth.user,
        home_users: auth.home_users,
        sources: auth.sources,
        home_pins,
        recent_searches,
        language: preferences.language,
        prefs: preferences.prefs,
        profiles,
        extensions: auth.extensions,
    })
}

/// Public snapshot for a locked protected bundle. It deliberately contains no offline credentials.
fn public_session(public: &plx_platform::storage::state::PublicPayload) -> Session {
    let preferences = serde_json::from_value::<CanonicalSessionPreferences>(
        public.preferences.clone(),
    )
    .unwrap_or_default();
    let home_pins = serde_json::from_value(public.pins.clone()).unwrap_or_default();
    let recent_searches = serde_json::from_value(public.recents.clone()).unwrap_or_default();
    Session {
        client_id: public.client_id.clone().unwrap_or_default(),
        language: preferences.language,
        prefs: preferences.prefs,
        home_pins, recent_searches,
        ..Default::default()
    }
}

fn de_soft_hero_blur<'de, D: Deserializer<'de>>(d: D) -> Result<Option<[[f32; 3]; 4]>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(serde_json::from_value(value).ok())
}

fn de_profile_cache<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ProfileCreds>, D::Error> {
    let value = Value::deserialize(d)?;
    Ok(match value { Value::Array(entries) => parse_profiles(entries), _ => Vec::new() })
}

fn parse_profiles(entries: Vec<Value>) -> Vec<ProfileCreds> {
    let mut counts = BTreeMap::new();
    for entry in &entries {
        if let Some(uuid) = entry.get("uuid").and_then(Value::as_str) {
            *counts.entry(uuid.to_owned()).or_insert(0usize) += 1;
        }
    }
    valid_profiles(entries.into_iter().filter(|entry| {
        entry.get("uuid").and_then(Value::as_str).is_some_and(|uuid| counts.get(uuid) == Some(&1))
    }).filter_map(|entry| serde_json::from_value(entry).ok()).collect())
}

/// Compare protected domain data across v1/v2 encodings without manufacturing an auth write.
/// Public-only edits preserve the original protected bytes, including v1 opaque extensions.
fn protected_fields(session: &Session) -> Result<Value, serde_json::Error> {
    serde_json::to_value((&session.account_token, &session.server, &session.user,
        &session.home_users, &session.sources, &session.profiles, &session.extensions))
}
fn protected_fields_equal(left: &Session, right: &Session) -> bool {
    matches!((protected_fields(left), protected_fields(right)), (Ok(left), Ok(right)) if left == right)
}
fn protected_matches(session: &Session, protected: &str) -> bool {
    join_canonical(&plx_platform::storage::state::PublicPayload::default(), protected)
        .is_ok_and(|previous| protected_fields_equal(&previous, session))
}

/// Invalid credentials cost only their offline entry. Duplicate identities invalidate every
/// matching entry, so input order can never choose which token/PIN becomes authoritative.
fn valid_profiles(profiles: Vec<ProfileCreds>) -> Vec<ProfileCreds> {
    let mut counts = BTreeMap::new();
    for profile in &profiles { *counts.entry(profile.uuid.clone()).or_insert(0usize) += 1; }
    profiles.into_iter().filter(|profile| {
        !profile.uuid.trim().is_empty() && profile.uuid == profile.user.uuid
            && counts.get(&profile.uuid) == Some(&1)
            && profile.pin.as_ref().is_none_or(PinVerifier::valid_shape)
    }).collect()
}

/// Persist Automatically Sign In through [`update`], so a concurrent roster/recents write cannot
/// lose the switch. Returns whether the file was rewritten.
#[cfg(any(test, feature = "test-support"))]
pub fn set_auto_sign_in(on: bool) -> bool {
    update(|cur| {
        if cur.prefs.auto_sign_in == on {
            return None;
        }
        Some(cur.with_auto_sign_in(on))
    })
}

/// Original-stream routing policy. A forced route may never create a compatible fallback.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DirectPlayMode {
    #[default]
    Auto,
    Forced,
    Disabled,
}

/// Persisted quality names are a file-format contract and must survive refactors.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackQuality {
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
    /// The bound this rung imposes. Original has none; Auto also has no fixed ceiling because its
    /// adaptive owner selects one dynamically once that path is ready.
    pub fn ceiling(self) -> Option<super::Ceiling> {
        let (max_kbps, max_w, max_h) = match self {
            PlaybackQuality::Auto | PlaybackQuality::Original => return None,
            PlaybackQuality::P1080High => (20000, 1920, 1080),
            PlaybackQuality::P1080 => (8000, 1920, 1080),
            PlaybackQuality::P720 => (4000, 1280, 720),
            PlaybackQuality::P720Low => (2000, 1280, 720),
            PlaybackQuality::P480 => (720, 854, 480),
        };
        Some(super::Ceiling {
            max_kbps,
            max_w,
            max_h,
        })
    }

    /// The picker's row text. ONE string per rung rather than a label plus a trailing value,
    /// because the row already carries the picker's leading checkmark — `ui/table.rs`'s rule is
    /// that a mark says where you are and a word says what is set, and no row says both.
    ///
    /// Display text in the UI language with its regional digits; a log line names the rung by
    /// `{:?}` instead, so the log does not depend on the viewer's language.
    pub fn label(self) -> String {
        use plx_platform::i18n::{current, msg};
        let (lines, kbps): (i64, i64) = match self {
            PlaybackQuality::Auto => return msg::settings_playback_auto().to_owned(),
            PlaybackQuality::Original => return msg::widgets_quality_original().to_owned(),
            PlaybackQuality::P1080High => (1080, 20_000),
            PlaybackQuality::P1080 => (1080, 8_000),
            PlaybackQuality::P720 => (720, 4_000),
            PlaybackQuality::P720Low => (720, 2_000),
            PlaybackQuality::P480 => (480, 720),
        };
        let rate = if kbps >= 1000 {
            msg::widgets_tracks_mbps(&current().number(kbps / 1000))
        } else {
            msg::widgets_tracks_kbps(&current().number(kbps))
        };
        // `1080p` is the format's own conventional name in every shipped language.
        format!("{lines}p \u{b7} {rate}")
    }

    /// The rung's SHORT name for a row that has another part to read beside it ("720p"): the
    /// picture height alone, which is the format's own conventional name in every shipped language.
    /// Auto and Original name themselves ([`Self::label`]).
    pub fn short_label(self) -> String {
        match self {
            PlaybackQuality::Auto | PlaybackQuality::Original => self.label(),
            PlaybackQuality::P1080High | PlaybackQuality::P1080 => "1080p".to_owned(),
            PlaybackQuality::P720 | PlaybackQuality::P720Low => "720p".to_owned(),
            PlaybackQuality::P480 => "480p".to_owned(),
        }
    }

    /// A missing field in an OLD file is handled by [`Session::playback_quality`] and is always
    /// Original. This is only for a genuinely NEW file, where Auto is allowed to become the
    /// default after (and only after) its whole playback path declares itself ready.
    pub fn fresh_default(auto_ready: bool) -> Self {
        if auto_ready {
            Self::Auto
        } else {
            Self::Original
        }
    }
}

/// The persisted subtitle tones, lightest first. Like [`PlaybackQuality`] the spelling on disk is
/// explicit: these strings are a file-format contract and must survive a variant rename.
///
/// A ladder of GREYS rather than a colour wheel, because the job is brightness: over an HDR
/// picture the panel maps graphics white far above where it sits in SDR, and the only thing that
/// makes a caption comfortable there is less light. What each rung looks like is `ui::theme`'s
/// (`subtitle_ink`); this type only names them.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitleTone {
    #[default]
    #[serde(rename = "white")]
    White,
    #[serde(rename = "grey_85")]
    Silver,
    #[serde(rename = "grey_70")]
    LightGrey,
    #[serde(rename = "grey_55")]
    Grey,
    #[serde(rename = "grey_40")]
    DarkGrey,
    #[serde(rename = "grey_28")]
    Charcoal,
}

impl SubtitleTone {
    /// Every rung, lightest first — the order the picker draws them in.
    pub const LADDER: [SubtitleTone; 6] = [
        SubtitleTone::White,
        SubtitleTone::Silver,
        SubtitleTone::LightGrey,
        SubtitleTone::Grey,
        SubtitleTone::DarkGrey,
        SubtitleTone::Charcoal,
    ];

    /// Canonical English tone label for diagnostics and compatibility callers.
    /// Localized picker rows map this typed tone through the UI catalog.
    pub fn label(self) -> &'static str {
        match self {
            SubtitleTone::White => "White",
            SubtitleTone::Silver => "Silver",
            SubtitleTone::LightGrey => "Light gray",
            SubtitleTone::Grey => "Gray",
            SubtitleTone::DarkGrey => "Dark gray",
            SubtitleTone::Charcoal => "Charcoal",
        }
    }

    /// An in-memory index back to a rung — out of range is `White`, never a neighbouring rung,
    /// for the reason `Quality::from_index` gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> SubtitleTone {
        Self::LADDER.get(i as usize).copied().unwrap_or(SubtitleTone::White)
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&t| t == self).unwrap_or(0) as u8
    }
}

/// The persisted subtitle text sizes, smallest first — one per PROFILE, like [`SubtitleTone`]: a
/// legible caption size is as much about the viewer's eyes as the panel and the couch distance, and
/// two people on one couch differ. A profile that has not picked reads the install's older key.
/// Absence is `Medium`, which is the caption face every build before this
/// preference drew (`appkit::player_hud`'s hardcoded `sz = 36`). Spelling on disk is explicit, like
/// every other persisted ladder here.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitleSize {
    #[serde(rename = "small")]
    Small,
    #[default]
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "large")]
    Large,
    #[serde(rename = "extra_large")]
    ExtraLarge,
    /// Added on top of the ladder rather than by moving the other rungs up, so no saved pick
    /// changes size. A build from before it reads `"huge"` as a value it cannot parse: the
    /// per-profile field falls back to the install's older value and the install key to `Medium`
    /// (both are parsed softly), never a failed load.
    #[serde(rename = "huge")]
    Huge,
}

impl SubtitleSize {
    /// Every rung, smallest first — the order the picker lists them in.
    pub const LADDER: [SubtitleSize; 5] = [
        SubtitleSize::Small,
        SubtitleSize::Medium,
        SubtitleSize::Large,
        SubtitleSize::ExtraLarge,
        SubtitleSize::Huge,
    ];

    /// An in-memory index back to a rung — out of range is `Medium`, never a neighbouring rung,
    /// for the reason [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> SubtitleSize {
        Self::LADDER.get(i as usize).copied().unwrap_or(SubtitleSize::Medium)
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&s| s == self).unwrap_or(1) as u8
    }
}

/// The persisted subtitle vertical placements, lowest first — one per profile, like [`SubtitleTone`].
/// Absence is `Low`, which is where every build before this preference drew the caption
/// (`media::player::text_style`'s fixed `SUB_BASE_Y`/`SUB_CEIL_Y` baseline). Only the plain-text caption draw
/// moves with this; image (PGS/VobSub) captions and native ASS/SSA keep their own placement
/// (`docs/ass-subtitles.md`).
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitlePosition {
    #[default]
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "middle")]
    Middle,
    #[serde(rename = "high")]
    High,
}

impl SubtitlePosition {
    /// Every rung, lowest first — the order the picker lists them in.
    pub const LADDER: [SubtitlePosition; 3] = [
        SubtitlePosition::Low,
        SubtitlePosition::Middle,
        SubtitlePosition::High,
    ];

    /// An in-memory index back to a rung — out of range is `Low`, never a neighbouring rung, for
    /// the reason [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> SubtitlePosition {
        Self::LADDER.get(i as usize).copied().unwrap_or(SubtitlePosition::Low)
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&p| p == self).unwrap_or(0) as u8
    }
}

/// What the player does when an episode that has a successor reaches its credits — one per profile,
/// like [`SubtitleTone`]. Absence is `Countdown`, the Up Next tile and its 10 s countdown every
/// build before this preference showed. `AfterCredits` shows nothing during the credits and
/// starts the next episode only when the stream ends; `Off` shows nothing and, at the end of the
/// stream, leaves the player as a movie does. A show's last episode and a movie are unaffected.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum NextEpisodeMode {
    #[default]
    #[serde(rename = "countdown")]
    Countdown,
    #[serde(rename = "after_credits")]
    AfterCredits,
    #[serde(rename = "off")]
    Off,
}

impl NextEpisodeMode {
    /// Every mode, the order the picker lists them in.
    pub const LADDER: [NextEpisodeMode; 3] = [
        NextEpisodeMode::Countdown,
        NextEpisodeMode::AfterCredits,
        NextEpisodeMode::Off,
    ];

    /// An in-memory index back to a mode — out of range is `Countdown`, for the reason
    /// [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> NextEpisodeMode {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&m| m == self).unwrap_or(0) as u8
    }
}

/// How far one Left/Right press jumps in the player (and the trailer transport) — one per profile
/// like [`SubtitleTone`], the same for both directions. Absence is `Seconds10`, the fixed hop
/// every build before this preference made. A closed set rather than a free number so the
/// telemetry code for a pick is a fixed string and a hand-edited file cannot ask for a
/// zero-length or hour-long hop. Holding a key still ramps by its own curve; this is only the tap.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipInterval {
    #[serde(rename = "5s")]
    Seconds5,
    #[default]
    #[serde(rename = "10s")]
    Seconds10,
    #[serde(rename = "15s")]
    Seconds15,
    #[serde(rename = "30s")]
    Seconds30,
    #[serde(rename = "60s")]
    Seconds60,
}

impl SkipInterval {
    /// Every option, shortest first — the order the picker lists them in.
    pub const LADDER: [SkipInterval; 5] = [
        SkipInterval::Seconds5,
        SkipInterval::Seconds10,
        SkipInterval::Seconds15,
        SkipInterval::Seconds30,
        SkipInterval::Seconds60,
    ];

    /// An in-memory index back to an option — out of range is the default, never a neighbouring
    /// option, for the reason [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> SkipInterval {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&o| o == self).unwrap_or(1) as u8
    }

    pub fn seconds(self) -> i64 {
        match self {
            SkipInterval::Seconds5 => 5,
            SkipInterval::Seconds10 => 10,
            SkipInterval::Seconds15 => 15,
            SkipInterval::Seconds30 => 30,
            SkipInterval::Seconds60 => 60,
        }
    }

    /// The hop in nanoseconds, the unit every scrub position is in.
    pub fn ns(self) -> i64 {
        self.seconds() * 1_000_000_000
    }
}

/// What OK does on a Continue Watching card — one per profile, like [`SubtitleTone`]. Absence is
/// `Details`: OK opens the movie page, exactly as on any other shelf, or for an episode the SHOW's
/// page on the card's season; the card's menu leads with Play (and the card draws no amber ▶, which
/// promises a press that plays).
/// `Play` is what every build before this preference did: OK plays or resumes at once.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeckPress {
    #[default]
    #[serde(rename = "details")]
    Details,
    #[serde(rename = "play")]
    Play,
}

impl DeckPress {
    /// Every option, the order the picker lists them in.
    pub const LADDER: [DeckPress; 2] = [DeckPress::Details, DeckPress::Play];

    /// An in-memory index back to an option — out of range is the default, for the reason
    /// [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> DeckPress {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&m| m == self).unwrap_or(0) as u8
    }

    /// Whether a press on a Continue Watching card starts the video — the one fact the deck's
    /// triangle, its press and its menu's Play row are all derived from.
    pub fn press_plays(self) -> bool {
        self == DeckPress::Play
    }
}

/// Which fact the details line under a focused poster carries (Settings > Libraries > Under
/// posters > Details). plex's own enum, because plex does not name `ui`: `screens` maps it to the
/// caption policy's `DetailsKind`. Absence, and any spelling this build does not know, is
/// `YearLength`.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnderPostersDetails {
    /// "2019 · 2h 14m" (the default).
    #[default]
    #[serde(rename = "year_length")]
    YearLength,
    #[serde(rename = "genre")]
    Genre,
    /// "4K · HDR".
    #[serde(rename = "video")]
    Video,
    /// "PG-13 · 7.8".
    #[serde(rename = "age_rating_score")]
    AgeRatingScore,
    /// No details line.
    #[serde(rename = "none")]
    None,
}

impl UnderPostersDetails {
    /// Every option, the order the picker lists them in.
    pub const LADDER: [UnderPostersDetails; 5] = [
        UnderPostersDetails::YearLength,
        UnderPostersDetails::Genre,
        UnderPostersDetails::Video,
        UnderPostersDetails::AgeRatingScore,
        UnderPostersDetails::None,
    ];

    /// An in-memory index back to an option — out of range is the default, for the reason
    /// [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> UnderPostersDetails {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&m| m == self).unwrap_or(0) as u8
    }
}

/// **What sits under the focused poster** — one setting for every poster grid and row (Library,
/// Collections, Home, Search, Watchlist): whether the title shows, and which fact the details
/// line carries. Per profile, like [`DeckPress`]. The default is no title and the year and length.
///
/// Each field is soft-parsed on its own, so a spelling a later build wrote for `details` costs the
/// details alone and never the title.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct UnderPosters {
    #[serde(deserialize_with = "de_soft_under_posters_title")]
    pub title: bool,
    #[serde(deserialize_with = "de_soft_under_posters_details")]
    pub details: UnderPostersDetails,
}

fn de_soft_under_posters_title<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    let Ok(v) = Value::deserialize(d) else { return Ok(false) };
    Ok(v.as_bool().unwrap_or(false))
}

fn de_soft_under_posters_details<'de, D: Deserializer<'de>>(d: D) -> Result<UnderPostersDetails, D::Error> {
    let Ok(v) = Value::deserialize(d) else { return Ok(UnderPostersDetails::default()) };
    Ok(serde_json::from_value::<UnderPostersDetails>(v).unwrap_or_default())
}

impl UnderPosters {
    /// The defaults, as a `const` for callers that compare against them.
    pub const DEFAULT: UnderPosters = UnderPosters { title: false, details: UnderPostersDetails::YearLength };

    /// One byte for the media layer's live cell: the title in bit 0, the details' index in bits 1-3.
    pub fn pack(self) -> u8 {
        u8::from(self.title) | (self.details.index() << 1)
    }

    /// The inverse of [`UnderPosters::pack`]; an out-of-range details index is the default.
    pub fn unpack(b: u8) -> UnderPosters {
        UnderPosters { title: b & 1 != 0, details: UnderPostersDetails::from_index((b >> 1) & 0b111) }
    }
}

/// **Which "Hold [OK] for options" hints this install has been taught** (`plx_ui::hold_hint`): one
/// flag per surface, `card` for any media card and `pill` for the Detail season tabs. Each is set
/// once a read showing happened on that surface (or an item menu was opened, which sets both) and
/// is never cleared, so the self-appearing hint stands once per INSTALL and survives relaunches.
/// Install-wide: kept across sign-out, shared by every profile. Absence is "not taught yet";
/// each flag is soft-parsed on its own, so garbage costs only that flag.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldHintTaught {
    #[serde(default, deserialize_with = "de_soft_bool", skip_serializing_if = "std::ops::Not::not")]
    pub card: bool,
    #[serde(default, deserialize_with = "de_soft_bool", skip_serializing_if = "std::ops::Not::not")]
    pub pill: bool,
}

impl HoldHintTaught {
    /// Nothing taught: the default, and what an absent key reads.
    pub const NONE: HoldHintTaught = HoldHintTaught { card: false, pill: false };

    /// Both sets joined: a surface taught in either stays taught.
    pub fn union(self, other: HoldHintTaught) -> HoldHintTaught {
        HoldHintTaught { card: self.card || other.card, pill: self.pill || other.pill }
    }
}

/// **The window the interface asks for at launch** (Settings > System > Interface resolution).
///
/// The UI is authored at 1920x1080 and `plx_base::surface` fits that canvas into whatever drawable
/// webOS grants; a smaller one fills fewer pixels and the television's compositor upscales it.
/// `Auto` is the native 1920x1080 canvas on EVERY set: a smaller window is always the owner's
/// choice, because it trades text sharpness for smoother frames and only the owner can judge that trade
/// on their own panel. Install-wide (not per profile) because the window is created once, before
/// anyone is seated. Absence, and any spelling this build does not know, is `Auto`. The setting is
/// offered, and the stored value honoured at boot, on webOS 1-3 only
/// (`plx_platform::tv::device::offers_ui_resolution`); elsewhere the stored value is kept but unused.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum UiResolution {
    #[default]
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "900p")]
    Hd900,
    #[serde(rename = "720p")]
    Hd720,
    /// Half the canvas in each direction: the one scale at which every texel maps to a whole
    /// number of canvas pixels (see `plx_base::surface`).
    #[serde(rename = "540p")]
    Sd540,
}

impl UiResolution {
    /// Every option, the order the picker lists them in. There is no 1920x1080 entry: Automatic
    /// already is the native canvas, and an explicit one would be the same choice twice.
    pub const LADDER: [UiResolution; 4] =
        [UiResolution::Auto, UiResolution::Hd900, UiResolution::Hd720, UiResolution::Sd540];

    /// The option's position in [`UiResolution::LADDER`], the row a picker highlights.
    pub fn index(self) -> usize {
        Self::LADDER.iter().position(|&m| m == self).unwrap_or(0)
    }

    /// The option at a picker row; an index past the end is `Auto`.
    pub fn from_index(index: usize) -> UiResolution {
        Self::LADDER.get(index).copied().unwrap_or_default()
    }

    /// The serialized spelling, for an identity comparison that must not depend on the language.
    pub fn tag(self) -> &'static str {
        match self {
            UiResolution::Auto => "auto",
            UiResolution::Hd900 => "900p",
            UiResolution::Hd720 => "720p",
            UiResolution::Sd540 => "540p",
        }
    }

    /// The window size this option asks for. `Auto` is the native canvas, so it names no
    /// smaller window: `None`.
    pub fn size(self) -> Option<(i32, i32)> {
        match self {
            UiResolution::Auto => None,
                        UiResolution::Hd900 => Some((1600, 900)),
            UiResolution::Hd720 => Some((1280, 720)),
            UiResolution::Sd540 => Some((960, 540)),
        }
    }
}

/// **How bright the player's overlay and captions are drawn while the picture is HDR** — one per
/// profile, like [`SubtitleTone`]. A television maps graphics white in an HDR picture to a far
/// brighter level than the same white in SDR (the HUD and the captions are graphics; the panel
/// treats them as part of the HDR scene), so a viewer in a dark room is dazzled by the transport
/// and the subtitles. This is how far RGB is scaled down then; it never applies to an SDR picture.
/// Absence is `ThreeQuarters`: the overlay is dimmed to 75% in an HDR picture unless the viewer
/// chose otherwise (`Full` turns the dim off). The factor is a plain linear multiplier on the sRGB-encoded values (alpha is untouched), which is
/// the same multiply the painter's `rgb` cascade already is.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HdrOverlayBrightness {
    #[serde(rename = "full")]
    Full,
    #[default]
    #[serde(rename = "three_quarters")]
    ThreeQuarters,
    #[serde(rename = "half")]
    Half,
}

impl HdrOverlayBrightness {
    /// Every rung, brightest first — the order the picker lists them in.
    pub const LADDER: [HdrOverlayBrightness; 3] = [
        HdrOverlayBrightness::Full,
        HdrOverlayBrightness::ThreeQuarters,
        HdrOverlayBrightness::Half,
    ];

    /// An in-memory index back to a rung — out of range is the default, never a neighbouring rung, for
    /// the reason [`SubtitleTone::from_index`] gives: the ladder can grow or shrink.
    pub fn from_index(i: u8) -> HdrOverlayBrightness {
        Self::LADDER.get(i as usize).copied().unwrap_or_default()
    }

    pub fn index(self) -> u8 {
        Self::LADDER.iter().position(|&b| b == self).unwrap_or(0) as u8
    }

    /// The RGB multiplier this rung applies to an HDR picture's overlay, `1.0` for `Full`.
    pub fn factor(self) -> f32 {
        match self {
            HdrOverlayBrightness::Full => 1.0,
            HdrOverlayBrightness::ThreeQuarters => 0.75,
            HdrOverlayBrightness::Half => 0.5,
        }
    }

    /// The rung's percentage, for its label.
    pub fn percent(self) -> i64 {
        (self.factor() * 100.0).round() as i64
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
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

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
    pub extensions: OpaqueExtensions,

}

/// One entry of [`Session::profiles`]: what a successful online switch to this profile resolved,
/// kept so the same profile can be seated offline. `user`, `server` and `sources` are exactly
/// what the switch wrote into the session when it was the active profile.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct ProfileCreds {
    pub uuid: String,
    pub user: UserRef,
    pub server: ServerRef,
    #[serde(deserialize_with = "de_soft_vec")]
    pub sources: Vec<SourceRef>,
    /// Present for a protected profile: the PIN plex.tv accepted at the last online switch, as a
    /// verifier. Absent for an unprotected profile — and absent for a protected one whose last
    /// switch predates this field, which [`Session::cached_profile`] treats as "cannot verify",
    /// never as "no PIN".
    pub pin: Option<PinVerifier>,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

}

/// A Plex Home PIN as something a PIN can be checked against, never the PIN: PBKDF2-HMAC-SHA-256
/// over a random 16-byte salt ([`plx_base::sha256`]).
///
/// **What it does and does not protect.** A four-digit PIN has ten thousand values, so nothing
/// stored can stop somebody who can read this file from grinding it — and that somebody already
/// holds the account token in the same file, which switches to any profile online, so there is no
/// new door. What the salt and the iteration count DO buy is the number itself: household PINs
/// are reused for phones and cards, and a leaked session file must not hand one over in clear.
/// The count is the highest a Cortex-A9 verifies in well under a second on the switch worker.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct PinVerifier {
    /// Lower-case hex, 16 random bytes.
    pub salt: String,
    /// Lower-case hex, the 32-byte PBKDF2 output.
    pub hash: String,
    pub iters: u32,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

}

impl PinVerifier {
    fn valid_shape(&self) -> bool {
        self.salt.len() == 32 && unhex(&self.salt).is_some_and(|v| v.len() == 16)
            && self.hash.len() == 64 && unhex(&self.hash).is_some_and(|v| v.len() == 32)
            && (1..=Self::MAX_ITERS).contains(&self.iters)
    }

    pub const ITERS: u32 = 20_000;
    /// The largest count [`PinVerifier::verify`] will run. A record is this app's own writing,
    /// so anything past a few times [`PinVerifier::ITERS`] is a hand edit or a newer build's
    /// value, and either must fail the check rather than park the switch worker in PBKDF2 for
    /// as long as a `u32` can count.
    pub const MAX_ITERS: u32 = 4 * Self::ITERS;

    /// A fresh verifier for `pin`, under a salt read from `/dev/urandom`.
    pub fn new(pin: &str) -> PinVerifier {
        Self::with_salt(pin, &random_bytes::<16>())
    }

    fn with_salt(pin: &str, salt: &[u8]) -> PinVerifier {
        let hash = plx_base::sha256::pbkdf2_hmac_sha256(pin.as_bytes(), salt, Self::ITERS);
        PinVerifier {
            salt: hex(salt),
            hash: hex(&hash),
            iters: Self::ITERS,
        extensions: Default::default(),
        }
    }

    /// Does `pin` reproduce this verifier? A malformed record (no salt, an un-hex hash, a zero
    /// count) verifies NOTHING rather than everything — the failure direction a lock must have.
    pub fn verify(&self, pin: &str) -> bool {
        let (Some(salt), Some(hash)) = (unhex(&self.salt), unhex(&self.hash)) else {
            return false;
        };
        if salt.len() != 16 || hash.len() != 32 || self.iters == 0 || self.iters > Self::MAX_ITERS {
            return false;
        }
        let got = plx_base::sha256::pbkdf2_hmac_sha256(pin.as_bytes(), &salt, self.iters);
        plx_base::sha256::ct_eq(&got, &hash)
    }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// `N` bytes from `/dev/urandom`, the same source [`new_client_id`] draws from. A short read
/// leaves zeros, which for a SALT costs uniqueness and nothing else.
fn random_bytes<const N: usize>() -> [u8; N] {
    use std::io::Read;
    let mut b = [0u8; N];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    b
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
    /// The dotted quad (or v6 literal, or hostname) discovery recorded. The LEGACY fallback for a
    /// file with no `origin` — see [`ServerRef::origin`] — and, since 2026-09-05, **the DNS
    /// answer for a `plex.direct` origin**: [`ServerRef::resolve_pin`] dials the name at this
    /// address when the name encodes it, which is what lets a stored session reach the household's
    /// own server with the internet down.
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
    pub extensions: OpaqueExtensions,

}

impl ServerRef {
    /// **Where the primary server is.** [`ServerRef::origin`] when the file has one, else the
    /// legacy `http://{address}:{port}` — which is exactly what a file written before that field
    /// existed meant, and what every reader of this struct did with those two fields by hand.
    ///
    /// **TOTAL, unlike [`SourceRef::origin`].** The asymmetry is deliberate. A roster entry has
    /// [`SourceRef::dialable`] in front of every caller, so `None` there costs one entry. This is
    /// the PRIMARY: `app.rs`'s boot gate and `auth::cancel` read it unconditionally, gated only by
    /// [`Session::can_go_local`], so a `None` here would be a NEW refusal on a path that has never
    /// had one — a silent sign-out at boot, which is the failure this whole field exists to avoid.
    /// The gate stays where it is, and the `port as i32` below is the same cast those readers were
    /// already doing, kept in one documented place instead of three.
    pub fn origin(&self) -> Origin {
        Origin::parse(&self.origin_url)
            .unwrap_or_else(|| Origin::http(&self.address, self.port as i32))
    }

    /// The DNS answer this file already holds for its origin: `address` beside a `plex.direct`
    /// name that encodes it. `None` for a legacy plaintext record or any origin whose address the
    /// name does not vouch for — see [`super::origin::ResolvePin`]. It is what lets a stored
    /// session boot against the household's own server with no resolver at all.
    pub fn resolve_pin(&self) -> Option<super::origin::ResolvePin> {
        super::origin::ResolvePin::for_origin(&self.origin(), &self.address)
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
    ///
    /// **It is plex.tv's wire flag, not "is this our household's server"** — those are different
    /// questions and `false` answers both of them for a Plex Home managed profile's own household
    /// server. [`home`](Self::home) and [`owner_id`](Self::owner_id) are carried beside it so the
    /// second question can be asked; see [`super::servers::is_household`].
    pub owned: bool,
    /// plex.tv's `home` on the grant, carried verbatim — see [`super::account::Resource::home`].
    ///
    /// **Absent from every file written before this field existed, and `false` is what those
    /// files mean.** Read the note on [`owner_id`](Self::owner_id) for why that default is safe:
    /// the two fields are one decision.
    #[serde(default)]
    pub home: bool,
    /// plex.tv's `ownerId` on the grant, carried verbatim: the account that owns the server, `0`
    /// on our own and `0` when plex.tv named nobody.
    ///
    /// **The legacy default is the whole reason this doc sentence exists.** A `SourceRef`
    /// deserialized from a file written before these two fields came back `home:false,
    /// owner_id:0` — and with `owned` also `false` (a share, or a managed profile's own household
    /// server) [`super::servers::is_household`] then answers exactly what raw `owned` answers,
    /// which is TODAY's behaviour and not a new one. That is deliberate and it is the intended
    /// reading: the record self-corrects on the next `/api/v2/resources` fetch, which every boot
    /// and every profile switch performs. It is written down here because the failure it can
    /// cause is silent — a household server read as an outside share — and a reader of the
    /// deserializer must be able to see that the default was chosen rather than defaulted into.
    #[serde(rename = "ownerId", default)]
    pub owner_id: i64,
    /// The address that answered `/identity` with the right `machineIdentifier` — not the first
    /// one advertised. An unmatched share's advertised local address may be this only through its
    /// TLS URI, after certificate and machine-identity verification; its plaintext form is gated.
    ///
    /// **What [`SourceRef::describe`] prints, the LEGACY fallback, and the resolve pin's address.**
    /// A connection is built from [`SourceRef::origin`], and for an https server the two genuinely
    /// differ (`origin.rs`) — but when the origin's `plex.direct` name encodes this very address,
    /// [`SourceRef::resolve_pin`] hands it to the transport as the name's resolution, so the
    /// origin is dialled with no resolver at all (the offline case).
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
    pub extensions: OpaqueExtensions,

}

impl SourceRef {
    /// Everything about this source except the token, for the event log. The machine id is left
    /// out entirely — it is a permanent household fingerprint (`app::diagnostics`), and the event log is
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
    ///
    /// **This is a well-formedness check, not a credential-eligibility one.** A `true` answer says
    /// only that there is an address, a dialable port and a non-empty token written down — it says
    /// nothing about whether THIS BUILD may put that token on THIS origin's transport. Issue #95:
    /// a stored `http://` entry is fully `dialable`. Issue #107 closed the gap that used to sit
    /// between here and that answer — a plaintext credential over it is refused or not by the one
    /// authority, `super::grant::credential_allowed` (the build's
    /// [`CredentialPolicy`](super::CredentialPolicy), or a live consented grant — which a stored
    /// entry can never supply by itself: grants are not persisted), asked again at registration
    /// (`servers::register_origin` and its sibling entry points, before the entry can ever become
    /// the current client) and again at the point of the actual request — never here.
    pub fn dialable(&self) -> bool {
        self.origin().is_some() && !self.token.is_empty()
    }

    /// **Where to dial this source**, `None` when there is nothing dialable written down.
    ///
    /// [`SourceRef::origin`] when the file has one, else the legacy `http://{address}:{port}` — an
    /// entry written before the field existed, which is every entry in every session file on every
    /// television today. The port still goes through
    /// [`probe::dial_port`](super::probe::dial_port) on that path, for the reason
    /// [`SourceRef::dialable`] gives: this file is JSON on disk that a hand edit or an older build
    /// can leave holding anything an `i64` can hold, and `port as i32` WRAPS.
    ///
    /// `Option`, unlike [`ServerRef::origin`], because every caller here is already behind
    /// [`SourceRef::dialable`] — so `None` costs one roster entry, which is the rule `de_soft_vec`
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

    /// [`ServerRef::resolve_pin`] for a roster entry: the answer plex.tv gave beside this
    /// origin, when the origin's name encodes it. A share on the internet gets none in practice
    /// (its name is not a LAN literal's); a second household server on the LAN gets the same
    /// treatment as the primary.
    pub fn resolve_pin(&self) -> Option<super::origin::ResolvePin> {
        let origin = self.origin()?;
        super::origin::ResolvePin::for_origin(&origin, &self.address)
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
    pub extensions: OpaqueExtensions,

}

/// The person's answer to "Connect without encryption?" for one server. See
/// [`Preferences::plaintext_consent`].
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlaintextChoice {
    /// Never asked — the only value an absent or unreadable entry can mean.
    #[default]
    Undecided,
    /// Connect is allowed while the server is eligible (`plex::probe::PlaintextEligibility`).
    Allowed,
    /// *Not now* on the question.
    Declined,
    /// Turned off in Settings after it had been allowed. Distinct from `Declined` only so the
    /// read-out and the report can say which: both refuse the same way.
    Revoked,
}

impl PlaintextChoice {
    pub fn allows(self) -> bool {
        self == Self::Allowed
    }
}

/// One server's recorded [`PlaintextChoice`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct PlaintextConsent {
    pub machine_id: String,
    /// The account that answered — `plex::grant::account_key`, a one-way fingerprint of its
    /// plex.tv token. An entry is honoured only while that account is signed in; one with no
    /// account (or another's) reads as never asked.
    #[serde(default)]
    pub account: String,
    pub choice: PlaintextChoice,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,
}

/// One server's remembered leaf public key. See [`Preferences::server_key_pins`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ServerKeyPin {
    pub machine_id: String,
    /// `sha256//<base64>`, the exact `CURLOPT_PINNEDPUBLICKEY` string (`plx_base::spki`).
    pub pin: String,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,
}

/// Is `pin` shaped like `plx_base::spki::pin_from_pem`'s answer: `sha256//` and the 44 base64
/// characters of a 32-byte digest. The file is hand-editable and the value goes to libcurl, so a
/// string that is not one is treated as absent rather than handed on.
fn is_key_pin(pin: &str) -> bool {
    pin.strip_prefix("sha256//").is_some_and(|b64| {
        b64.len() == 44
            && b64.ends_with('=')
            && b64.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'/' | b'='))
    })
}

/// One profile's last-browsed library per content type. See [`Preferences::last_library`].
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LastLibrary {
    /// The Plex Home user's `uuid`, or **empty for the account owner** with no Home selection —
    /// the same convention [`HomePins`] and [`RecentSearches`] use, and for the same reason.
    pub user: String,
    pub libs: Vec<TypedLib>,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

}

/// One remembered library, tagged with the TYPE whose tab it answers for.
///
/// `kind` is the wire's own `Directory.type` string (`movie` / `show`) rather than an enum
/// discriminant, so a reordered `SecKind` cannot silently repoint an entry written by an older
/// build — the same reason [`PinnedLib`] keys on a machine id rather than a roster position.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct TypedLib {
    pub kind: String,
    pub machine_id: String,
    pub key: i64,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

}

impl LastLibrary {
    /// This profile's remembered library for `kind`, as `(machine_id, key)`.
    pub fn get(&self, kind: &str) -> Option<(&str, i64)> {
        self.libs
            .iter()
            .find(|l| l.kind == kind)
            .map(|l| (l.machine_id.as_str(), l.key))
    }
    /// Record a choice, replacing this type's entry rather than appending beside it.
    ///
    /// **A library with no machine id is not recorded and CLEARS the entry**, rather than being
    /// written with an empty one: `""` would match every nameless library on every server nobody
    /// has identified yet, which is the trap [`HomePins::answer`] carries its own guard for — and
    /// here it would point a tab at an arbitrary one of them on the next boot.
    pub fn set(&mut self, kind: &str, machine_id: &str, key: i64) {
        self.libs.retain(|l| l.kind != kind);
        if machine_id.is_empty() {
            return;
        }
        self.libs.push(TypedLib {
            kind: kind.to_string(),
            machine_id: machine_id.to_string(),
            key,
        extensions: Default::default(),
        });
    }
}

/// One profile's remembered library views. See [`Preferences::library_views`].
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LibraryViews {
    /// The Plex Home user's `uuid`, or **empty for the account owner** — [`LastLibrary`]'s
    /// convention, and for the same reason.
    pub user: String,
    /// Oldest first: [`LibraryViews::set`] moves a re-viewed library to the end, and the cap
    /// drops from the front.
    #[serde(deserialize_with = "de_soft_vec")]
    pub libs: Vec<LibraryView>,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,
}

/// Everything one person chose in one library's view: the section, and what the Library screen's
/// toolbar changed about it.
///
/// `machine_id`, `key`, `sort` and `desc` are the entry's ORIGINAL fields, always written, so an
/// older build reads the sort back from a file this one wrote; every field added since is omitted
/// while it is at its default, which makes a sort-only entry serialize byte for byte as it did
/// before this record held more than the sort.
///
/// The library has two sorts to keep because it has two listings: `sort`/`desc` belong to its OWN
/// listing (Movies, or TV Shows) and `listing_sort`/`listing_desc` to the listing named by
/// `listing` when that is not the library's own. They are separate so the library's own sort
/// survives a visit to Collections — returning to the own listing within a session re-applies it.
/// A genre is a filter of the library's own listing only (it names one metadata type's menu), so
/// it is never recorded for another listing.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct LibraryView {
    pub machine_id: String,
    pub key: i64,
    /// The own listing's sort KEY (`titleSort`, `addedAt`, the client-side `viewCount`); empty =
    /// the menu's first entry. Always serialized: it is the legacy field.
    pub sort: String,
    /// The own listing's direction. Always serialized, like `sort`.
    pub desc: bool,
    /// The Unwatched switch.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unwatched: bool,
    /// The genre filter's tag id (the server's `genre` directory key), own listing only; empty =
    /// no genre.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub genre: String,
    /// The listing type shown, by the wire word `browse` gives it: empty = the library's own
    /// listing, else `seasons`, `episodes` or `collections` ([`LibraryView::LISTINGS`]).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub listing: String,
    /// The sort key of the listing `listing` names; empty = that listing's default order.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub listing_sort: String,
    /// The direction of `listing_sort`.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub listing_desc: bool,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,
}

impl LibraryView {
    /// The `listing` words a record may carry. Anything else is not a listing this build knows.
    const LISTINGS: [&'static str; 3] = ["seasons", "episodes", "collections"];

    /// The own listing's remembered sort as `(sort key, descending)`, `None` when it is the
    /// default order.
    pub fn primary_sort(&self) -> Option<(&str, bool)> {
        (!self.sort.is_empty()).then_some((self.sort.as_str(), self.desc))
    }
    /// The named listing's remembered sort as `(sort key, descending)`, `None` when it is that
    /// listing's default order.
    pub fn listing_sort(&self) -> Option<(&str, bool)> {
        (!self.listing_sort.is_empty()).then_some((self.listing_sort.as_str(), self.listing_desc))
    }
    /// Does this view say nothing a fresh library would not? Such a view is forgotten rather than
    /// recorded. Judges the CONTENT: which library it is says nothing, and an entry carrying
    /// fields only a newer build understands is never "nothing".
    pub fn is_default(&self) -> bool {
        self.sort.is_empty() && !self.desc && !self.unwatched && self.genre.is_empty()
            && self.listing.is_empty() && self.listing_sort.is_empty() && !self.listing_desc
            && self.extensions.is_empty()
    }
    /// This view with every field a record may not carry dropped back to its default: a sort
    /// longer than [`LibraryViews::MAX_SORT`], a genre longer than [`LibraryViews::MAX_GENRE`], a
    /// listing that is not one of [`LibraryView::LISTINGS`].
    pub fn sanitized(mut self) -> Self {
        if self.sort.len() > LibraryViews::MAX_SORT {
            self.sort.clear();
            self.desc = false;
        }
        if self.listing_sort.len() > LibraryViews::MAX_SORT {
            self.listing_sort.clear();
            self.listing_desc = false;
        }
        if self.genre.len() > LibraryViews::MAX_GENRE {
            self.genre.clear();
        }
        if !self.listing.is_empty() && !Self::LISTINGS.contains(&self.listing.as_str()) {
            self.listing.clear();
            self.listing_sort.clear();
            self.listing_desc = false;
        }
        self
    }
}

impl LibraryViews {
    /// Libraries remembered per profile. A household changes a handful; the cap exists so a
    /// television that has browsed many shares over the years cannot grow the public payload
    /// (256 KiB for everything, `storage::state`) without bound — ~200 bytes an entry at its
    /// fullest, so a full list is ~5 KiB per profile. It bounds ONE profile: past 256 KiB for the
    /// whole document the document guard (`persistence::fit_document`, `prefs::shed_memory`) may
    /// also evict this list's oldest entries from the largest record.
    pub const CAP: usize = 24;
    /// Longest sort key recorded. Real keys are a few words (`show.titleSort,episode.index` is
    /// the longest PMS advertises); anything longer is not a key worth carrying across restarts.
    const MAX_SORT: usize = 96;
    /// Longest genre id recorded. A genre's `key` is a tag id — a number or a short slug.
    const MAX_GENRE: usize = 64;

    /// This profile's remembered view of one library, if it ever changed it. `None` for a library
    /// with no machine id, which is never recorded.
    pub fn get(&self, machine_id: &str, key: i64) -> Option<&LibraryView> {
        if machine_id.is_empty() {
            return None;
        }
        self.libs.iter().find(|lib| lib.machine_id == machine_id && lib.key == key)
    }
    /// Record a library's view as the most recent. The library's previous entry is replaced; a
    /// view that is the DEFAULT once sanitized records nothing, which is how choosing everything
    /// back FORGETS the entry. Evicts the oldest entries past [`CAP`](Self::CAP). A library with
    /// no machine id is never recorded — for [`LastLibrary::set`]'s reason.
    pub fn set(&mut self, view: LibraryView) {
        self.libs.retain(|lib| !(lib.machine_id == view.machine_id && lib.key == view.key));
        let view = view.sanitized();
        if view.machine_id.is_empty() || view.is_default() {
            return;
        }
        self.libs.push(view);
        let excess = self.libs.len().saturating_sub(Self::CAP);
        self.libs.drain(..excess);
    }
}

/// One profile's remembered subtitle sync-timing corrections. See [`Preferences::subtitle_offsets`].
///
/// The first tenant of the memory door ([`prefs::ProfileList`]): `{user, items, extensions}` on
/// disk exactly as it was before the door existed (`account` is skipped while empty; the tenant is
/// account-bound, so a record carries its account's stamp and sign-out drops the list). Oldest
/// first: a re-tuned item moves to the end and the cap drops from the front ([`SubtitleOffsetEntry`]'s [`MemoryEntry`] impl names the cap and the key).
pub type SubtitleOffsets = prefs::ProfileList<SubtitleOffsetEntry>;

/// One item's remembered subtitle sync-timing correction, in milliseconds
/// (`player::SUBTITLE_OFFSET_LATEST_MS`'s own unit and clamp).
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct SubtitleOffsetEntry {
    pub machine_id: String,
    pub rating_key: String,
    pub offset_ms: i64,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,
}

/// **One profile's FAVOURITE libraries** — the first-run route's record (`Shared Sources.dc.html`
/// deliverable F), and what the Favorite libraries editor writes back when its one action commits.
///
/// **Not a write per switch**, which this said until the editor grew a draft: a flip edits the
/// draft and nothing reaches this record until `Done`/`Start watching` sends one `ApplyPins`. Nor
/// is it a write of the whole table any more — only the rows the viewer ANSWERED become entries
/// (`pins::answers`), so a default nobody chose stays absent from both lists below and keeps
/// re-deriving. Which rows those are rides on the `ApplyPins` command itself: an answer is not
/// recognised by disagreeing with the live pin, or one given just as a roster correction moved
/// the pin onto the same value would be read as a default and lost.
///
/// It recorded the answer to "what goes on your Home?" until 2026-09-05 and the persisted key is
/// still `home_pins`, deliberately: renaming it would break ROLLBACK rather than upgrade, since the
/// next whole-`Session` write under an older build would emit only the new name and that build
/// would silently apply defaults. The SCOPE is what widened — see `browse::BrowseSection::pinned`.
///
/// **Both sides are recorded, and that is the field this type exists for.** A single "these are
/// pinned" list cannot tell *turned off* from *not answered about*, and the two must not be one
/// value: libraries arrive over time — a share whose server was slow to answer, a library the
/// owner created last week — and one that lands after the question was put has to fall on its own
/// DEFAULT (the household's On, a friend's Off), not silently Off because it was absent from a
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
    pub extensions: OpaqueExtensions,

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

/// The last-selected Plex Home user. `token` is strictly the per-user PMS token that scopes
/// server access and watch state; `plex_tv_token` is the distinct credential returned by the
/// profile switch for account-service calls. Both keep their authority when cached offline.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)] // a missing field costs that field, never the session — see [`HomeUserRef`]
pub struct UserRef {
    pub id: i64,
    pub uuid: String,
    pub title: String,
    pub thumb: String,
    /// Per-(profile, server) PMS credential. Never send this to plex.tv.
    pub token: String,
    /// The switched profile's plex.tv credential. Old, damaged, or empty values simply disable
    /// optional account-service reads until the next successful online switch.
    #[serde(default, deserialize_with = "de_soft_nonempty_string", skip_serializing_if = "Option::is_none")]
    pub plex_tv_token: Option<String>,
    #[serde(flatten, default, skip_serializing_if = "OpaqueExtensions::is_empty")]
    pub extensions: OpaqueExtensions,

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

fn de_soft_nonempty_string<'de, D>(d: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(value) = Value::deserialize(d) else { return Ok(None) };
    Ok(value.as_str().map(str::to_owned).filter(|value| !value.trim().is_empty()))
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

/// A preference switch: garbage degrades to off rather than failing the enclosing [`Session`].
fn de_soft_bool<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(false);
    };
    Ok(v.as_bool().unwrap_or(false))
}

fn default_true() -> bool {
    true
}

/// Same soft parse as [`de_soft_bool`], but garbage and a missing value stay on. Used where the
/// product default is on ([`Session::trailer_autoplay`], the frozen install-wide fallback of the
/// per-profile hero trailer switch).
fn de_soft_bool_on<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let Ok(v) = serde_json::Value::deserialize(d) else {
        return Ok(true);
    };
    Ok(v.as_bool().unwrap_or(true))
}

impl Session {
    /// Record (or replace) the cached credentials for one profile — the online switch's write.
    pub fn remember_profile(&mut self, creds: ProfileCreds) {
        if creds.uuid.is_empty() {
            return;
        }
        self.profiles.retain(|p| p.uuid != creds.uuid);
        self.profiles.push(creds);
    }

    /// Bring the ACTIVE profile's cached record up to date with the session's own user, primary
    /// and roster — the late half of a switch (`merge_profile_roster`) and a roster refresh both
    /// change those after the record was first written, and an offline seat from the old copy
    /// would restore a roster missing the shares found since. The verifier is kept; nothing here
    /// knows a PIN. No record, no write: an unprotected profile's first record comes from
    /// `auth::remember_unprotected_active`, a protected one's from the switch that saw its PIN.
    /// Returns whether the record CHANGED, so a writer that persists only on change (the roster
    /// refresh) also persists a stale record's repair when the roster itself did not move.
    pub fn refresh_profile_record(&mut self) -> bool {
        let uuid = self.user.uuid.clone();
        if uuid.is_empty() {
            return false;
        }
        let Some(p) = self.profiles.iter_mut().find(|p| p.uuid == uuid) else {
            return false;
        };
        let before = serde_json::to_string(&(&p.user, &p.server, &p.sources)).unwrap_or_default();
        p.user = self.user.clone();
        p.server = self.server.clone();
        p.sources = self.sources.clone();
        serde_json::to_string(&(&p.user, &p.server, &p.sources)).unwrap_or_default() != before
    }

    /// The cached credentials for `uuid`, if this television has switched to it online before
    /// and the entry still names a usable primary. Nothing about a PIN is decided here — the
    /// caller reads [`ProfileCreds::pin`] against the tile's `protected` flag.
    pub fn cached_profile(&self, uuid: &str) -> Option<&ProfileCreds> {
        if uuid.is_empty() {
            return None;
        }
        self.profiles.iter().find(|p| {
            p.uuid == uuid && !p.user.token.is_empty() && !p.server.origin().host().is_empty()
        })
    }

    /// The effective persisted playback quality. Absence is the literal legacy migration rule:
    /// builds that predate the field played Original, so they continue to play Original.
    #[allow(dead_code)]
    pub fn direct_play_mode(&self) -> DirectPlayMode { self.prefs.direct_play_mode }

    pub fn playback_quality(&self) -> PlaybackQuality {
        self.prefs.playback_quality.unwrap_or(PlaybackQuality::Original)
    }

    pub fn auto_sign_in(&self) -> bool {
        self.prefs.auto_sign_in
    }

    /// The Interface resolution the NEXT launch draws at; see [`UiResolution`].
    pub fn ui_resolution(&self) -> UiResolution {
        self.prefs.ui_resolution
    }

    /// Which hold hints this install has been taught; see [`HoldHintTaught`].
    pub fn hold_hint_taught(&self) -> HoldHintTaught {
        self.prefs.hold_hint_taught
    }

    /// This session with `taught` joined into what it already holds (a lesson is never unlearned).
    pub fn with_hold_hint_taught(&self, taught: HoldHintTaught) -> Self {
        let mut next = self.clone();
        next.prefs.hold_hint_taught = next.prefs.hold_hint_taught.union(taught);
        next
    }

    pub fn with_ui_resolution(&self, mode: UiResolution) -> Self {
        let mut next = self.clone();
        next.prefs.ui_resolution = mode;
        next
    }

    /// Record the Settings switch while leaving every unrelated session field intact.
    pub fn with_auto_sign_in(&self, on: bool) -> Self {
        let mut next = self.clone();
        next.prefs.auto_sign_in = on;
        next
    }

    /// The install-wide hero trailer autoplay key: the FALLBACK every profile reads until it picks
    /// its own (`prefs::stored_effective::<TrailerAutoplayPref>` is what a profile sees). No pick
    /// writes it; session creation sets it on.
    pub fn trailer_autoplay(&self) -> bool {
        self.prefs.trailer_autoplay
    }

    /// The answer `account` (`plex::grant::account_key`) recorded for one server —
    /// [`PlaintextChoice::Undecided`] when it was never asked (see [`Preferences::plaintext_consent`]).
    pub fn plaintext_choice(&self, account: &str, machine_id: &str) -> PlaintextChoice {
        self.prefs.plaintext_consent
            .iter()
            .find(|c| c.account == account && c.machine_id == machine_id)
            .map_or(PlaintextChoice::Undecided, |c| c.choice)
    }

    /// Record one server's answer for `account`, leaving that account's other servers alone.
    /// `Undecided` forgets it. Another account's answers are dropped on the way: they could never
    /// be honoured again while this one answers, and the file keeps only the live account's.
    pub fn with_plaintext_choice(&self, account: &str, machine_id: &str, choice: PlaintextChoice) -> Self {
        let mut next = self.clone();
        next.prefs.plaintext_consent.retain(|c| c.account == account && c.machine_id != machine_id);
        if choice != PlaintextChoice::Undecided && !machine_id.is_empty() && !account.is_empty() {
            next.prefs.plaintext_consent.push(PlaintextConsent {
                machine_id: machine_id.to_owned(),
                account: account.to_owned(),
                choice,
                extensions: Default::default(),
            });
        }
        next
    }

    /// The pin remembered for `machine_id`, or `None` when none was learned (or the stored string
    /// is not a pin). Read by [`project_server_keys`], which hands it to `net::keypin` (issue #378).
    pub fn server_key_pin(&self, machine_id: &str) -> Option<&str> {
        self.prefs.server_key_pins
            .iter()
            .find(|k| k.machine_id == machine_id)
            .map(|k| k.pin.as_str())
            .filter(|pin| is_key_pin(pin))
    }

    /// This session with `machine_id`'s remembered pin set to `pin`, or `None` when that changes
    /// nothing — the same pin already stored, or an empty machine id or a string that is not a
    /// pin — so the caller skips the write. One entry per machine: a different key replaces it.
    pub fn with_server_key_pin(&self, machine_id: &str, pin: &str) -> Option<Self> {
        if machine_id.is_empty() || !is_key_pin(pin) || self.server_key_pin(machine_id) == Some(pin) {
            return None;
        }
        let mut next = self.clone();
        next.prefs.server_key_pins.retain(|k| k.machine_id != machine_id);
        next.prefs.server_key_pins.push(ServerKeyPin {
            machine_id: machine_id.to_owned(),
            pin: pin.to_owned(),
            extensions: Default::default(),
        });
        Some(next)
    }

    pub fn subtitle_tone(&self) -> SubtitleTone {
        self.prefs.subtitle_tone
    }

    pub fn subtitle_size(&self) -> SubtitleSize {
        self.prefs.subtitle_size
    }

    pub fn subtitle_position(&self) -> SubtitlePosition {
        self.prefs.subtitle_position
    }

    pub fn next_episode_mode(&self) -> NextEpisodeMode {
        self.prefs.next_episode_mode
    }

    pub fn skip_interval(&self) -> SkipInterval {
        self.prefs.skip_interval
    }

    pub fn deck_press(&self) -> DeckPress {
        self.prefs.deck_press
    }

    pub fn under_posters(&self) -> UnderPosters {
        self.prefs.under_posters
    }

    pub fn hdr_overlay_brightness(&self) -> HdrOverlayBrightness {
        self.prefs.hdr_overlay_brightness
    }

    pub fn audio_enhancements(&self) -> crate::plex::AudioEnhancements {
        self.prefs.audio_enhancements
    }

    /// Interactive boot with a multi-user Plex Home shows the who's-watching picker unless
    /// Automatically Sign In is on and a profile is already seated.
    ///
    /// `force_pick` is `/tmp/plxnative-pickuser`: it wins even on an automated boot. An empty
    /// `user.uuid` still raises the picker when the switch is on — that is the abandoned-at-picker
    /// session whose PMS token falls back to the owner. A uuid that is no longer in
    /// [`Session::home_users`] (removed Home user, stale roster) raises it too: the switch is an
    /// opt-in to skip the list for someone still on it, not a licence to boot leftover tokens.
    pub fn boot_shows_picker(&self, automated: bool, force_pick: bool) -> bool {
        if !self.can_go_local() || self.home_users.len() <= 1 {
            return false;
        }
        if force_pick {
            return true;
        }
        if automated {
            return false;
        }
        !(self.prefs.auto_sign_in && self.seated_in_roster())
    }

    /// True when [`Session::user`]'s uuid names someone still on the cached Plex Home roster.
    pub fn seated_in_roster(&self) -> bool {
        !self.user.uuid.is_empty()
            && self.home_users.iter().any(|u| u.uuid == self.user.uuid)
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
    /// sign-in, and a profile switch never replaces it. The switched user's plex.tv credential is
    /// retained separately on [`UserRef::plex_tv_token`]. Anything asked of plex.tv with the
    /// session account token is answered ABOUT THE OWNER: every `accessToken` that comes back is the owner's
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
    /// one hands out precisely the session the PIN exists to gate — see `crate::auth::cancel`.
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
            extensions: Default::default(),
            });
        }
    }

    /// One profile's remembered library views — `None` for a profile that never changed one.
    pub fn views_for(&self, user: &str) -> Option<&LibraryViews> {
        self.prefs.library_views.iter().find(|views| views.user == user)
    }

    /// Record one library's view for one profile (a view that is the default forgets the entry —
    /// [`LibraryViews::set`]), leaving every other profile's alone — a method for
    /// [`Session::set_recents_for`]'s reason. A profile whose last entry is forgotten loses its
    /// record entirely, so the list never carries empties.
    pub fn set_view_for(&mut self, user: &str, view: LibraryView) {
        match self.prefs.library_views.iter_mut().find(|views| views.user == user) {
            Some(slot) => slot.set(view),
            None => {
                let mut fresh = LibraryViews { user: user.to_string(), ..Default::default() };
                fresh.set(view);
                self.prefs.library_views.push(fresh);
            }
        }
        self.prefs.library_views.retain(|views| !views.libs.is_empty());
    }

    /// One profile's remembered subtitle sync-timing correction for one item, `None` for a
    /// profile that never tuned one (or a different item/server). Reads the profile's record
    /// whatever its account stamp: the player's read is [`prefs::ProfileMemory::get`] under the
    /// signed-in account's stamp, which is what hides another account's record.
    pub fn subtitle_offset_for(&self, user: &str, machine_id: &str, rating_key: &str) -> Option<i64> {
        self.prefs.subtitle_offsets.iter().find(|o| o.user == user)
            .and_then(|o| o.get(machine_id, rating_key))
    }

    /// Record (or, with `None`, forget) one item's subtitle offset for one profile, leaving every
    /// other profile's alone — [`Session::set_view_for`]'s reason. A profile whose last entry is
    /// forgotten loses its record entirely, so the list never carries empties. The in-memory
    /// half of the memory door's `upsert_with` ([`prefs::ProfileMemory`]), stamping the record with
    /// this session's own account (`grant::account_key`; none without an account token).
    pub fn set_subtitle_offset_for(&mut self, user: &str, machine_id: &str, rating_key: &str, offset_ms: Option<i64>) {
        let entry = offset_ms.map(|offset_ms| SubtitleOffsetEntry {
            machine_id: machine_id.to_string(),
            rating_key: rating_key.to_string(),
            offset_ms,
            extensions: Default::default(),
        });
        let key = (machine_id.to_string(), rating_key.to_string());
        let account = super::grant::account_key(&self.account_token);
        self.prefs.subtitle_offsets.upsert_with(user, &account, &key, |_| entry);
    }
}

/// Which profile's history is in play: the active Plex Home user's `uuid`, or `""` for the owner
/// with no Home selection AND for "no profile published yet" (the boot picker, a sign-out), which
/// this cannot tell apart from the owner. It does not say whether anyone is SEATED either: a profile
/// published without a sign-in stamp (a dev token) has a key here and seats nobody. Who is seated,
/// as a comparable value that also names the sign-in, is [`current_seat`] / [`Seat`]; the ten
/// per-profile settings (the player's nine and trailer autoplay) key on that, never on this. One accessor, so the reader and the
/// writer cannot key on different things — which would look exactly like the leak this scoping
/// exists to prevent.
pub fn current_profile_key() -> String {
    current().map(|u| u.uuid).unwrap_or_default()
}

/// **The I/O lock.** Blocking loads and writers take it; per-frame [`peek`] never does.
/// It serializes boot loads, credential commits on the persistence worker, roster/profile
/// workers, and preference/search-recents updates against the same authority.
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
/// It is held across the whole write, including `sync_all`, so only boot and background work
/// may take it. Frame readers use [`peek`] on both hits and misses; the frame guard rejects
/// blocking entry points in tests and names violations in debug/release logs. [`CACHE`] is
/// taken briefly inside IO, never the reverse; content comparison runs outside CACHE.
static IO: Mutex<()> = Mutex::new(());

struct IoGuard {
    _lock: std::sync::MutexGuard<'static, ()>,
    _block: plx_base::task::BlockingGuard,
}

fn io() -> IoGuard {
    let block = plx_base::task::assert_may_block(const { &plx_base::task::BlockingLabel::new("session storage I/O") });
    // Poison is stepped over: a panic in one writer must not turn every later save into a panic of
    // its own, which on this path would mean losing the credentials rather than a stale file.
    IoGuard { _lock: IO.lock().unwrap_or_else(|e| e.into_inner()), _block: block }
}

#[cfg(any(test, feature = "test-support"))]
pub fn with_io_for_test<R>(f: impl FnOnce() -> R) -> R {
    let _io = io();
    f()
}

/// One cached answer this process has proved about the persisted session — the arm a
/// [`ReadState`] lands in is [`refresh_locked`]'s doc.
#[derive(Clone)]
enum Cached {
    /// Locally signed out while the queued clear is pending or failed; never restore credentials.
    Revoked,
    /// Nothing has been read or written yet this process.
    Unloaded,
    /// Canonical/migration [`ReadState::Ready`], `Missing` or `Cleared`: a settled fact about the file. Good until the
    /// next write installs or drops it.
    Settled(std::sync::Arc<ReadState>),
    /// [`ReadState::Locked`], `Blocked`, or fallback `Ready`: a transient failure (a helper timeout, a keymanager
    /// hiccup), not a fact about the file — served as the last known state, never latched forever:
    /// [`refresh_need`] asks for another read and the session owner decides when (its backoff clock is
    /// the frame tick, [`LOCKED_RETRY_MS`]).
    Transient {
        state: std::sync::Arc<ReadState>,
    },
}

/// **The live read cache** — what [`peek`] answers from memory instead of taking [`IO`]. A hit is
/// one short `Mutex` lock and an `Arc` clone; a miss serves the previous snapshot (or the empty
/// session) and leaves the refill to the session owner's `RefreshCache` effect ([`refresh_need`]). See the module doc for the full invariant list;
/// the two that matter for reasoning about a deadlock: nobody takes `IO` while holding this
/// lock, and a writer always re-reads the authority under `IO` rather than trusting
/// whatever is cached, so a miss can never overwrite a newer write. `player::preview`'s own
/// `MACHINE` mutex is always taken before this one, and nothing reachable while holding this lock
/// calls back into `preview` or takes `IO` — the same rule [`IO`]'s own doc states for `update`'s
/// closure, unchanged by the cache.
static CACHE: Mutex<Cached> = Mutex::new(Cached::Unloaded);

/// How long (in frame-tick milliseconds) the session owner holds back another refresh of the
/// generation whose read came back Locked/Blocked or as a fallback Ready, and of every generation
/// after a refused or unwound job. These are transient by definition (see [`ReadState`]'s doc), so
/// the interval only needs to be short enough that a real recovery is felt quickly, and long
/// enough to limit background retries to about once a second. Per-frame callers never pay for
/// these reads, and nothing here reads a clock: the owner measures it on `cx.tick.ms`.
pub const LOCKED_RETRY_MS: u32 = 1000;

/// The `Session` every non-`Ready` read answers with, shared so a Locked/Blocked retry window does
/// not allocate a fresh default every time.
fn empty_session() -> std::sync::Arc<Session> {
    static EMPTY: std::sync::OnceLock<std::sync::Arc<Session>> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| std::sync::Arc::new(Session::default())).clone()
}

/// The `Arc<Session>` a [`ReadState`] answers with — [`empty_session`] for anything but `Ready`.
fn session_of(state: &ReadState) -> std::sync::Arc<Session> {
    match state {
        ReadState::Ready { session, .. } => session.clone(),
        ReadState::Cleared { kept } | ReadState::Locked { kept } => std::sync::Arc::new(kept.into_session()),
        ReadState::Missing | ReadState::Blocked => empty_session(),
    }
}

/// Whether a [`ReadState`] is a transient failure to read rather than a fact about the file.
fn is_transient(state: &ReadState) -> bool {
    match state {
        ReadState::Locked { .. } | ReadState::Blocked | ReadState::Ready { retry_canonical: true, .. } => true,
        ReadState::Ready { .. } | ReadState::Missing | ReadState::Cleared { .. } => false,
    }
}

/// Which [`Cached`] arm a fresh [`ReadState`] belongs in — shared by [`refresh_locked`] (a read)
/// and [`install_locked`] (a write's proven outcome).
fn cached_arm(state: std::sync::Arc<ReadState>) -> Cached {
    if is_transient(&state) { Cached::Transient { state } } else { Cached::Settled(state) }
}

/// A settled cache hit, if one exists. Never takes [`IO`]. A transient entry is a miss here: the
/// boot and background callers that ask ([`load_with_id`]) want a fact about the file.
fn cached_settled() -> Option<std::sync::Arc<ReadState>> {
    match &*CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        Cached::Settled(state) => Some(state.clone()),
        Cached::Unloaded | Cached::Revoked | Cached::Transient { .. } => None,
    }
}

/// Read the authority (the caller must already hold [`IO`]), install the result into [`CACHE`],
/// and return it. Publishes identities on a `Ready` read — the same hook [`load`] and every write
/// already carry, so a cache-filling read is covered by the scrubber exactly as an uncached one
/// always was.
fn refresh_locked() -> std::sync::Arc<ReadState> {
    read_into_cache_locked(None).0
}

/// [`refresh_locked`] that installs only while the cache generation is still `expected`, and says
/// whether it did (`false`: a write, a drop or a revocation got in first, and the read was discarded).
fn read_into_cache_locked(expected: Option<u64>) -> (std::sync::Arc<ReadState>, bool) {
    #[cfg(not(any(test, feature = "test-support")))]
    let state = std::sync::Arc::new(read_live_locked());
    #[cfg(any(test, feature = "test-support"))]
    let state = std::sync::Arc::new(CACHE_READ_FOR_TEST.with(|read| {
        read.get().unwrap_or(read_live_locked)()
    }));
    #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
    plx_base::eventlog::log("session: authority read reason=miss");
    if !replace_cache(cached_arm(state.clone()), expected, false) { return (state, false); }
    if let ReadState::Ready { session, .. } = &*state { publish_identities(session); }
    (state, true)
}

/// Install a proven [`ReadState`] — a completed read, or the outcome of a write that IS provably
/// the record (`Durable`, or on the `TEST_FILE` path a legacy persist that returned `Some(_)`) —
/// as the new cache content. The caller must already hold [`IO`]; see the module doc's invariants.
fn install_locked(state: std::sync::Arc<ReadState>) {
    set_cache_locked(cached_arm(state));
}

/// Drop whatever is cached: a write whose outcome is not provably the record (`Uncertain`,
/// `Failed`, `ProtectionFailed`, or a failed legacy fallback), a sign-out, or a test fixture
/// redirecting the file out from under the cache. The caller must already hold [`IO`].
fn drop_cache_locked() {
    set_cache_locked(Cached::Unloaded);
}

/// Queue a preference edit against the captured account. The worker still reads and merges
/// under IO; a sign-out/account replacement before execution discards the obsolete edit.
/// The return value means admitted to the queue, not durably saved.
pub fn queue_update(edit: impl FnOnce(&Session) -> Option<Session> + Send + 'static) -> bool {
    queue_update_ticket(edit).is_ok()
}

/// Remember `pin` as `machine_id`'s public key (issue #380), best-effort and off the caller's
/// thread. Called by the identity probe on every verified answer, which is often, so it asks the
/// cached session first and queues nothing when the stored pin already matches; the queued edit
/// re-checks against the file under IO and writes only a change. Returns whether an edit was
/// queued — not whether it was saved.
pub fn learn_server_key(machine_id: &str, pin: &str) -> bool {
    if machine_id.is_empty() || peek().server_key_pin(machine_id) == Some(pin) {
        return false;
    }
    // The key reaches `net::keypin` ONLY through the queued write's own cache replacement
    // ([`project_server_keys`], the table's single production writer). Key mode cannot be needed
    // right after a strict success, and a direct write here would let an intervening
    // `replace_cache` revert it until the write lands, and let a probe that finishes after
    // sign-out repopulate the table that sign-out just emptied.
    let (machine_id, pin) = (machine_id.to_owned(), pin.to_owned());
    queue_update(move |cur| cur.with_server_key_pin(&machine_id, &pin))
}

/// Remember that this install has been taught the hold hints in `taught` (`plx_ui::hold_hint`),
/// off the caller's thread and best-effort: the frame thread calls it on the rising edge, the
/// cached session says whether anything is new (nothing is queued when the stored flags already
/// cover `taught`), and the queued edit re-checks under IO and writes only a change. Returns
/// whether an edit was queued, not whether it was saved. With no stored session the edit finds
/// nothing to write and the hint teaches once more on the next install of a session.
pub fn learn_hold_hint_taught(taught: HoldHintTaught) -> bool {
    let stored = peek().hold_hint_taught();
    if stored.union(taught) == stored {
        return false;
    }
    queue_update(move |cur| {
        let next = cur.with_hold_hint_taught(taught);
        (next.prefs.hold_hint_taught != cur.prefs.hold_hint_taught).then_some(next)
    })
}

/// The UI may retain the receipt while showing its pending preference locally.
pub fn queue_update_ticket(edit: impl FnOnce(&Session) -> Option<Session> + Send + 'static)
    -> Result<plx_base::storage_worker::TypedTicket<bool>, plx_base::storage_worker::SubmitError> {
    let expected = peek();
    let tenure = REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    Ok(plx_base::storage_worker::submit_retained(move || {
        update(|current| {
            if REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire) != tenure
                || (!expected.client_id.is_empty() && (current.client_id != expected.client_id
                    || current.account_token != expected.account_token)) {
                return None;
            }
            edit(current)
        })
    }))
}

/// [`queue_update_ticket`] for an edit whose LANDING matters — a consent refusal
/// (`plex::grant::record`), which must not be lost to a failed write. `settled` runs on the
/// storage worker once the attempt is over: `true` when the file holds the edit afterwards (it
/// already did, or the write persisted) or the edit no longer applies (a sign-out or another
/// account replaced the one it was queued for), `false` when the store could not be read or the
/// write failed — the caller's cue to try again.
pub fn queue_update_settled(
    edit: impl FnOnce(&Session) -> Option<Session> + Send + 'static,
    settled: impl FnOnce(bool) + Send + 'static,
) {
    let expected = peek();
    let tenure = REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    drop(plx_base::storage_worker::submit_retained(move || {
        let (mut read, mut moot) = (false, false);
        let write = update_with_outcome(|current| {
            if REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire) != tenure
                || (!expected.client_id.is_empty() && (current.client_id != expected.client_id
                    || current.account_token != expected.account_token)) {
                moot = true;
                return None;
            }
            read = true;
            edit(current)
        });
        settled(moot || write.map_or(read, |w| w.outcome.persisted()));
    }));
}

/// Read the persisted session without minting or preference edits. A worker may migrate a marked
/// fallback into recovered canonical storage. For readers that merely want to know what the
/// session says (the account surfaces, and now every per-frame reader too):
/// [`load`]'s client-id minting means a read can turn into a `save`, so a file that momentarily
/// fails to parse would be overwritten with a bare client_id — a silent sign-out. That is an
/// acceptable trade on the boot path, which must end up with an id; it is not one on a path a
/// keypress (or a frame) can reach. Falls back to the pre-relocation path (migration), same as
/// [`load`].
///
/// **Cached** — see [`CACHE`]. Returns the same `Arc<Session>` across repeated calls as long as
/// nothing in this process has written since the last one. **A pure read of memory:** it takes no
/// [`IO`], submits nothing and reads no clock, so it answers the last known state (or the empty
/// session before the first read) on a miss. Refilling a missing or transient cache is the session
/// owner's job: [`refresh_need`] says what is missing, the owner's `RefreshCache` effect runs
/// [`refresh_cache_job`], and the frame thread observes the visible generation move before
/// invalidating; unchanged retries leave a settled UI alone.
pub fn peek() -> std::sync::Arc<Session> {
    match &*CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        Cached::Unloaded | Cached::Revoked => empty_session(),
        Cached::Settled(state) | Cached::Transient { state } => session_of(state),
    }
}

/// What the cache is missing, for the session owner's refresh decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RefreshNeed {
    /// The cache generation the need was observed at; the refresh installs only while it still holds.
    pub generation: u64,
    /// The cache holds a transient failure to read (a retry), not an `Unloaded` cache.
    pub transient: bool,
}

/// Whether the cache needs a read, and at which generation. Pure: it takes no [`IO`] and reads no
/// clock. `Settled` and `Revoked` need nothing; `Unloaded` and `Transient` do. Only the app's
/// frame step asks, and answers with a `RefreshCache` effect once the owner's backoff allows it.
pub fn refresh_need() -> Option<RefreshNeed> {
    let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let transient = match &*cache {
        Cached::Unloaded => false,
        Cached::Transient { .. } => true,
        Cached::Settled(_) | Cached::Revoked => return None,
    };
    Some(RefreshNeed { generation: CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed), transient })
}

/// How a refresh job ended. `Refused` is never produced by [`refresh_cache_job`]: it is reported
/// when the job never ran to completion. The adapter reports it when the queue refused the job or
/// the job panicked; the app's bridge reports it, from its frame-step reconcile, when the adapter
/// that began the job was replaced or dropped and so lost it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CacheRefreshOutcome {
    /// A write, a drop or a revocation moved the cache before the read could land; nothing installed.
    Superseded,
    /// The read installed a fact about the file.
    Settled,
    /// The read installed a transient failure (Locked, Blocked, fallback Ready). `generation` is
    /// the cache generation that entry stands at: the install itself moved it, so the owner holds
    /// back only observations of THIS generation, and a drop or a write (which move it again)
    /// refresh at once.
    Transient { generation: u64 },
    /// The job did not complete.
    Refused,
}

/// The body of one cache refresh, for the storage worker: take [`IO`], quit if the cache generation
/// moved since `generation` was observed, otherwise read the authority and install it (still
/// fenced by `generation`, so a sign-out or write that lands mid-read wins). The two sign-out
/// fences, [`CACHE_GENERATION`] and [`replace_cache`]'s Revoked guard, are exactly the ones the old
/// scheduled refresh used. The test read substitute is captured here, on the submitting thread.
pub fn refresh_cache_job(generation: u64) -> impl FnOnce() -> CacheRefreshOutcome + Send + 'static {
    #[cfg(any(test, feature = "test-support"))]
    let read = CACHE_READ_FOR_TEST.with(|read| read.get());
    move || {
        let _io = io();
        if CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed) != generation {
            return CacheRefreshOutcome::Superseded;
        }
        #[cfg(any(test, feature = "test-support"))]
        CACHE_READ_FOR_TEST.with(|slot| slot.set(read));
        let (state, installed) = read_into_cache_locked(Some(generation));
        #[cfg(any(test, feature = "test-support"))]
        CACHE_READ_FOR_TEST.with(|slot| slot.set(None));
        match (installed, is_transient(&state)) {
            (false, _) => CacheRefreshOutcome::Superseded,
            (true, true) => CacheRefreshOutcome::Transient {
                generation: CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed),
            },
            (true, false) => CacheRefreshOutcome::Settled,
        }
    }
}

/// Fill the cache from the authority now, on the calling thread, and return what it says: for tests
/// that need a filled cache without driving the owner's `RefreshCache` effect.
#[cfg(any(test, feature = "test-support"))]
pub fn refresh_blocking_for_test() -> std::sync::Arc<Session> {
    let _io = io();
    session_of(&refresh_locked())
}

/// A queued refresh is invalidated by any intervening write or cache drop. The worker checks
/// this generation under IO before reading, so an old queue entry cannot resurrect a sign-out.
static CACHE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn set_cache_locked(value: Cached) {
    replace_cache(value, None, false);
}

/// Only an explicit, proven credential write may end local revocation.
fn install_proven_locked(state: std::sync::Arc<ReadState>, generation: u64) {
    replace_cache(cached_arm(state), Some(generation), true);
}

fn install_write_locked(state: std::sync::Arc<ReadState>, authority: SaveAuthority, generation: u64) {
    if authority == SaveAuthority::FreshReauthentication {
        install_proven_locked(state, generation);
    } else {
        replace_cache(cached_arm(state), Some(generation), false);
    }
}

pub fn cache_revoked() -> bool {
    matches!(*CACHE.lock().unwrap_or_else(|e| e.into_inner()), Cached::Revoked)
}

/// Separate from the I/O fence: Locked/Blocked/Missing all serve the same empty session.
/// Bridges and cached session-derived views observe this counter independently on the frame thread.
static VISIBLE_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(any(test, feature = "test-support"))]
pub fn install_transient_for_test(locked: bool) {
    plx_base::testlock::assert_held("session read fixture");
    let _io = io();
    install_locked(std::sync::Arc::new(if locked { ReadState::Locked { kept: install_preferences::Retained { language: plx_platform::i18n::saved_preference(), ..Default::default() } } } else { ReadState::Blocked }));
}

pub fn visible_generation() -> u64 {
    VISIBLE_GENERATION.load(std::sync::atomic::Ordering::Acquire)
}

/// Per-consumer cursor for cached session-derived views. Poll on Tick, including while storage
/// is unavailable: the owner's `RefreshCache` effect runs the bounded retry, so the frame never blocks.
#[derive(Default)]
pub struct VisibleSessionWatch(Option<(u64, bool)>);
impl VisibleSessionWatch {
    pub fn changed(&mut self) -> bool {
        let generation = visible_generation();
        let key = (generation, peek_settled().is_some());
        let changed = self.0 != Some(key);
        self.0 = Some(key);
        changed
    }
}

/// A settled visible authority, or None while a read is unloaded/Locked/Blocked. Local revocation
/// is settled for UI purposes: retaining a pre-sign-out account would be misleading.
pub fn peek_settled() -> Option<std::sync::Arc<Session>> {
    match &*CACHE.lock().unwrap_or_else(|e| e.into_inner()) {
        Cached::Settled(state) => Some(session_of(state)),
        Cached::Revoked => Some(empty_session()),
        Cached::Unloaded | Cached::Transient { .. } => None,
    }
}

fn same_visible_session(previous: &Cached, next: &Cached) -> bool {
    let ready = |cache: &Cached| match cache {
        Cached::Settled(state) | Cached::Transient { state } => match &**state {
            ReadState::Ready { session, .. } => Some(session.clone()),
            _ => None,
        },
        _ => None,
    };
    match (ready(previous), ready(next)) {
        (None, None) => true,
        (Some(a), Some(b)) if std::sync::Arc::ptr_eq(&a, &b) => true,
        (a, b) => {
            // Only Ready content needs serialization. This runs outside CACHE; malformed
            // future serializers conservatively count as changed rather than panicking.
            let a = a.unwrap_or_else(empty_session);
            let b = b.unwrap_or_else(empty_session);
            matches!((serde_json::to_value(&*a), serde_json::to_value(&*b)),
                (Ok(a), Ok(b)) if a == b)
        }
    }
}

/// **The session → `net::keypin` projection, spelled once** (issue #378). Replaces the table's
/// remembered keys with `session`'s and binds every stored server that has a `ResolvePin` — the
/// primary and the roster — to its `host:port`, so a boot that has not registered anything yet is
/// already covered. The other half of the binding is `servers::register_lazy`, which binds the
/// server whose `ResolvePin` it installs. A signed-out session is empty, so sign-out empties the
/// table. Called from [`replace_cache`] (every read, write and revocation) and by boot for a
/// session it was handed rather than read. `signed_out` is [`key_projection`]'s statement that the
/// session is over (signed out, cleared, revoked), which also ends what `net::keypin` published
/// about the servers it knew; a live session that merely holds no key is NOT, and boot hands over
/// a session that is live.
pub fn project_server_keys(session: &Session, signed_out: bool) {
    let pins: Vec<(String, String)> = session
        .prefs
        .server_key_pins
        .iter()
        .filter_map(|k| session.server_key_pin(&k.machine_id).map(|p| (k.machine_id.clone(), p.to_owned())))
        .collect();
    let mut stored: Vec<(String, String, i32)> = Vec::new();
    let mut add = |machine: &str, pin: Option<super::origin::ResolvePin>| {
        if let Some(pin) = pin {
            stored.push((machine.to_owned(), pin.host().to_owned(), pin.port()));
        }
    };
    add(&session.server.machine_id, session.server.resolve_pin());
    for source in &session.sources {
        add(&source.machine_id, source.resolve_pin());
    }
    plx_net::net::keypin::project(pins, &stored, signed_out);
}

/// The session a cache value tells us about, for [`project_server_keys`], and whether it is the end
/// of one: a read or a write that proved the record (live), a signed-out record or none (empty,
/// over), a local revocation (empty, over). A transient failure to read (`Locked`, `Blocked`) and
/// `Unloaded` say nothing and leave the table alone.
fn key_projection(value: &Cached) -> Option<(std::sync::Arc<Session>, bool)> {
    match value {
        Cached::Revoked => Some((empty_session(), true)),
        Cached::Unloaded => None,
        Cached::Settled(state) | Cached::Transient { state } => match &**state {
            ReadState::Ready { session, .. } => Some((session.clone(), false)),
            ReadState::Missing | ReadState::Cleared { .. } => Some((empty_session(), true)),
            ReadState::Locked { .. } | ReadState::Blocked => None,
        },
    }
}

fn replace_cache(value: Cached, expected: Option<u64>, proven: bool) -> bool {
    let projection = key_projection(&value);
    loop {
        let (previous, generation) = {
            let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
            let generation = CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
            if expected.is_some_and(|expected| expected != generation)
                || (!proven && matches!(*cache, Cached::Revoked) && !matches!(value, Cached::Revoked)) {
                return false;
            }
            (cache.clone(), generation)
        };
        let changed = !same_visible_session(&previous, &value);
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed) != generation { continue; }
        *cache = value;
        CACHE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Under the cache lock, so two replacements project in the order they landed.
        // `net::keypin` takes its own lock and never calls back into this module.
        if let Some((session, signed_out)) = &projection { project_server_keys(session, *signed_out); }
        if changed { VISIBLE_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release); }
        return true;
    }
}

/// Revoke the UI's credentials immediately without waiting for IO. This is a local revocation,
/// not a claim that the canonical clear is durable. Only a subsequent proven write can replace it.
static REVOCATION_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn revoke_cached_session() {
    REVOCATION_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Release);
    set_cache_locked(Cached::Revoked);
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Substitute a slow authority read without changing the storage or legacy-file paths.
    static CACHE_READ_FOR_TEST: std::cell::Cell<Option<fn() -> ReadState>> = const {
        std::cell::Cell::new(None)
    };
}

#[cfg(test)]
mod cache_timing_tests {
    use super::*;

    // These cases advance the process-wide present clock. Restore it before releasing the
    // serial fixture: a later test's time zero would otherwise wrap the keepalive arithmetic.
    struct ResetIdle;
    impl Drop for ResetIdle {
        fn drop(&mut self) { plx_machine::idle::reset_for_test(); }
    }

    /// What a frame thread's landing step reads of the cache: whether the visible generation moved
    /// since it last looked. The app's `Bridge::land_session_cache` compares the same counter and
    /// invalidates the frame once per move; that half is graded beside `Bridge`
    /// (`app::bridge::plex_session_app_tests`), so this layer's tests need no application type.
    struct FrameCursor(u64);
    impl FrameCursor {
        fn new() -> Self { Self(visible_generation()) }
        fn lands(&mut self) -> bool {
            let seen = visible_generation();
            std::mem::replace(&mut self.0, seen) != seen
        }
    }

    /// `peek` is a read of memory and nothing else: it never submits a refresh, so an invalidated
    /// cache stays invalidated until the session owner's `RefreshCache` effect runs
    /// ([`refresh_need`]).
    #[test]
    fn peek_never_reads_storage() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static READS: AtomicUsize = AtomicUsize::new(0);
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("peek-never-reads");
        { let _io = io(); install_locked(std::sync::Arc::new(ReadState::Blocked)); }
        invalidate_for_test();
        READS.store(0, Ordering::SeqCst);
        CACHE_READ_FOR_TEST.with(|slot| slot.set(Some(|| {
            READS.fetch_add(1, Ordering::SeqCst);
            ReadState::Missing
        })));
        for _ in 0..30 { assert!(peek().client_id.is_empty()); }
        plx_base::storage_worker::drain_for_test();
        CACHE_READ_FOR_TEST.with(|slot| slot.set(None));
        assert_eq!(READS.load(Ordering::SeqCst), 0, "peek must not schedule a storage read");
    }

    #[test]
    fn a_slow_refresh_job_never_blocks_peek_and_lands_on_the_frame_step() {
        use std::sync::mpsc::{self, Receiver, Sender};
        static CHANNELS: Mutex<Option<(Sender<()>, Receiver<()>)>> = Mutex::new(None);
        // Set by the read once its release (or its 5 s fallback) came: the clockless witness that a
        // peek taking the read's `IO` would have had to wait for it.
        static READ_RETURNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        READ_RETURNED.store(false, std::sync::atomic::Ordering::SeqCst);
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("slow-production-landing");
        let _idle = ResetIdle;
        { let _io = io(); install_locked(std::sync::Arc::new(ReadState::Blocked)); }
        let mut frame_step = FrameCursor::new();
        invalidate_for_test();
        let need = refresh_need().expect("an invalidated cache needs a read");
        assert!(!need.transient);
        let (started, entered) = mpsc::channel();
        let (release, held) = mpsc::channel();
        *CHANNELS.lock().unwrap() = Some((started, held));
        CACHE_READ_FOR_TEST.with(|slot| slot.set(Some(|| {
            let (started, held) = CHANNELS.lock().unwrap().take().unwrap();
            started.send(()).unwrap();
            let _ = held.recv_timeout(std::time::Duration::from_secs(5));
            READ_RETURNED.store(true, std::sync::atomic::Ordering::SeqCst);
            ReadState::Ready { session: std::sync::Arc::new(test_support::signed_in()), plaintext: false, retry_canonical: false }
        })));
        plx_machine::idle::reset_for_test();
        let ticket = plx_base::storage_worker::submit_retained(refresh_cache_job(need.generation));
        CACHE_READ_FOR_TEST.with(|slot| slot.set(None));
        entered.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        for _ in 0..30 { assert!(peek().client_id.is_empty()); }
        assert!(
            !READ_RETURNED.load(std::sync::atomic::Ordering::SeqCst),
            "peek must not wait for the read: the peeks finished only after the parked read had returned"
        );
        release.send(()).unwrap();
        assert_eq!(ticket.wait_blocking().unwrap(), CacheRefreshOutcome::Settled);
        plx_base::storage_worker::drain_for_test();
        assert!(!plx_machine::idle::should_present(0), "the worker only publishes data");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "the peeks raise no frame damage of their own");
        let _frame = plx_base::task::FrameScope::enter();
        assert!(frame_step.lands(), "the frame step observes the published session");
        assert_eq!(peek().client_id, "cid-1");
        assert!(!frame_step.lands(), "once");
    }

    #[test]
    fn a_preference_edit_survives_a_full_worker_queue() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("full-edit-queue");
        save(&test_support::signed_in());
        let (release, held) = std::sync::mpsc::channel();
        let (started, entered) = std::sync::mpsc::channel();
        let _block = plx_base::storage_worker::submit(move || {
            started.send(()).unwrap();
            let _ = held.recv();
        }).unwrap();
        entered.recv().unwrap();
        for _ in 0..plx_base::storage_worker::CAPACITY {
            let _ = plx_base::storage_worker::submit(|| ()).unwrap();
        }
        let edit = queue_update_ticket(|s| Some(s.with_auto_sign_in(true)));
        release.send(()).unwrap();
        plx_base::storage_worker::drain_for_test();
        assert!(edit.unwrap().wait_blocking().unwrap(), "the edit must survive admission backpressure");
        assert!(peek().auto_sign_in());
    }

    #[test]
    fn registering_a_new_server_inside_a_frame_never_loads_storage() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("register-frame-no-load");
        let _frame = plx_base::task::FrameScope::enter();
        crate::plex::register_origin("frame-client", &crate::plex::Origin::http("127.0.0.1", 32400),
            "", None, crate::plex::ConnectionFacts::default());
    }

    #[test]
    fn reads_and_preference_updates_cannot_resurrect_a_revoked_session() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("revoked-read");
        save(&test_support::signed_in());
        revoke_cached_session(); // A non-durable clear leaves this old record on disk.
        let ticket = queue_update_ticket(|current| Some(current.with_auto_sign_in(true))).unwrap();
        let _ = ticket.wait_blocking();
        assert!(peek().account_token.is_empty(), "a preference edit cannot end local revocation");
        let _ = load();
        assert!(peek().account_token.is_empty(), "a synchronous read cannot end local revocation");
    }

    #[test]
    fn an_edit_admitted_before_the_first_cache_read_reaches_the_valid_disk_session() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("unloaded-edit");
        save(&test_support::signed_in());
        invalidate_for_test();
        let ticket = queue_update_ticket(|current| Some(current.with_auto_sign_in(true))).unwrap();
        assert!(ticket.wait_blocking().unwrap(), "unknown cache identity is not an account mismatch");
        assert!(peek().auto_sign_in());
    }

    #[test]
    fn a_queued_refresh_cannot_overwrite_a_newer_write() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("cache-queued-write");
        let need = refresh_need().expect("an unloaded cache needs a read");
        let held = io();
        let ticket = plx_base::storage_worker::submit_retained(refresh_cache_job(need.generation));
        install_locked(std::sync::Arc::new(ReadState::Ready {
            session: std::sync::Arc::new(test_support::signed_in()), plaintext: false, retry_canonical: false,
        }));
        drop(held);
        assert_eq!(ticket.wait_blocking().unwrap(), CacheRefreshOutcome::Superseded,
            "a write that landed first fences the queued read out");
        assert_eq!(peek().client_id, "cid-1");
    }

    #[test]
    fn an_inflight_refresh_cannot_undo_local_revocation() {
        use std::sync::mpsc::{self, Receiver, Sender};
        static CHANNELS: Mutex<Option<(Sender<()>, Receiver<()>)>> = Mutex::new(None);
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("cache-revoked-inflight");
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *CHANNELS.lock().unwrap() = Some((entered_tx, release_rx));
        CACHE_READ_FOR_TEST.with(|read| read.set(Some(|| {
            let (entered, release) = CHANNELS.lock().unwrap().take().unwrap();
            entered.send(()).unwrap();
            let _ = release.recv_timeout(std::time::Duration::from_secs(5));
            ReadState::Ready { session: std::sync::Arc::new(test_support::signed_in()), plaintext: false, retry_canonical: false }
        })));
        let need = refresh_need().expect("an unloaded cache needs a read");
        let ticket = plx_base::storage_worker::submit_retained(refresh_cache_job(need.generation));
        CACHE_READ_FOR_TEST.with(|read| read.set(None));
        entered_rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        revoke_cached_session();
        assert!(peek().account_token.is_empty());
        release_tx.send(()).unwrap();
        assert_eq!(ticket.wait_blocking().unwrap(), CacheRefreshOutcome::Superseded);
        assert!(peek().account_token.is_empty(), "an old read must not republish revoked credentials");
        assert!(matches!(*CACHE.lock().unwrap(), Cached::Revoked));
    }

    #[test]
    fn session_landings_invalidate_once_on_the_frame_thread_only_when_content_changes() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("cache-visible-landing");
        let _idle = ResetIdle;
        {
            let _io = io();
            install_locked(std::sync::Arc::new(ReadState::Blocked));
        }
        let mut frame_step = FrameCursor::new();
        plx_base::storage_worker::drain_for_test();
        frame_step.lands();
        for (read, landed) in [
            ((|| ReadState::Blocked) as fn() -> ReadState, false),
            ((|| ReadState::Locked { kept: Default::default() }) as fn() -> ReadState, false),
            ((|| ReadState::Ready {
                session: std::sync::Arc::new(test_support::signed_in()), plaintext: false, retry_canonical: false,
            }) as fn() -> ReadState, true),
            // A different allocation and ReadState metadata, but the same served content.
            ((|| ReadState::Ready {
                session: std::sync::Arc::new(test_support::signed_in()), plaintext: true, retry_canonical: false,
            }) as fn() -> ReadState, false),
        ] {
            plx_machine::idle::reset_for_test();
            plx_machine::idle::note_present(10_000);
            CACHE_READ_FOR_TEST.with(|slot| slot.set(Some(read)));
            let job = refresh_cache_job(CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed));
            CACHE_READ_FOR_TEST.with(|slot| slot.set(None));
            drop(plx_base::storage_worker::submit_retained(job));
            plx_base::storage_worker::drain_for_test();
            assert!(!plx_machine::idle::should_present(10_001), "workers cannot wake the UI");
            assert_eq!(plx_machine::idle::take_local_damage(), 0);
            let _frame = plx_base::task::FrameScope::enter();
            assert_eq!(frame_step.lands(), landed);
            assert!(!frame_step.lands(), "one landing per change");
            assert_eq!(plx_machine::idle::take_local_damage(), 0, "landing the cache raises no frame damage of its own");
        }
    }

    #[test]
    fn refresh_need_reports_what_the_cache_lacks() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("refresh-need");
        invalidate_for_test();
        let unloaded = refresh_need().expect("Unloaded needs a read");
        assert!(!unloaded.transient);
        { let _io = io(); install_locked(std::sync::Arc::new(ReadState::Blocked)); }
        let transient = refresh_need().expect("Transient needs a retry");
        assert!(transient.transient);
        assert_ne!(transient.generation, unloaded.generation, "the install moved the generation");
        { let _io = io(); install_locked(std::sync::Arc::new(ReadState::Missing)); }
        assert_eq!(refresh_need(), None, "Settled needs nothing");
        revoke_cached_session();
        assert_eq!(refresh_need(), None, "a local revocation is never refilled");
    }

    #[test]
    fn a_refresh_job_reports_how_it_ended() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("refresh-outcomes");
        let run = |read: fn() -> ReadState, generation: u64| {
            CACHE_READ_FOR_TEST.with(|slot| slot.set(Some(read)));
            let job = refresh_cache_job(generation);
            CACHE_READ_FOR_TEST.with(|slot| slot.set(None));
            plx_base::storage_worker::submit_retained(job).wait_blocking().unwrap()
        };
        invalidate_for_test();
        let g = refresh_need().unwrap().generation;
        assert_eq!(run(|| ReadState::Blocked, g), CacheRefreshOutcome::Transient { generation: g + 1 },
            "the install moved the generation; the outcome names the one the entry stands at");
        let g = refresh_need().unwrap().generation;
        assert_eq!(run(|| ReadState::Missing, g), CacheRefreshOutcome::Settled);
        assert_eq!(run(|| ReadState::Missing, g), CacheRefreshOutcome::Superseded,
            "the generation the job was given has moved on");
        assert_eq!(refresh_need(), None);
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn cache_is_empty_for_test() -> bool {
    matches!(&*CACHE.lock().unwrap_or_else(|e| e.into_inner()), Cached::Unloaded | Cached::Revoked)
}

/// Drop [`CACHE`] from a test, outside any `IO`-holding call — for a fixture that changes what
/// [`peek`] ought to answer without going through [`save`]/[`update`]/[`clear`] (writing bytes
/// directly to a redirected scratch or canonical file). See the module doc's test-isolation note.
#[cfg(any(test, feature = "test-support"))]
pub fn invalidate_for_test() {
    let _io = io();
    drop_cache_locked();
}

/// **Forget one profile's recorded favourite libraries.**
///
/// For a fixture that must resolve against the HOUSEHOLD DEFAULTS rather than against an answer
/// somebody recorded earlier — `browse::seed_two_source_table_for_test` and its registered twin,
/// whose whole contract ("four libraries projecting to two library-type pills") is a statement
/// about a table with no recorded pins behind it.
///
/// Not [`update`]: that refuses a file with no `client_id`, which is exactly the state a scratch
/// session is in before anything has signed in, so the clear would silently not happen — the
/// failure mode this exists to remove.
#[cfg(any(test, feature = "test-support"))]
pub fn forget_pins_for_test(user: &str) {
    let _io = io();
    let mut s = peek_locked();
    let before = s.home_pins.len();
    s.home_pins.retain(|p| p.user != user);
    if s.home_pins.len() != before {
        save_locked(&s);
    }
}

/// [`peek`] with the lock already held — the read half every entry point here shares. Used only
/// where an owned, mutable `Session` is genuinely needed ([`forget_pins_for_test`]); everything
/// else wants the cached, `Arc`-shared [`peek`].
fn peek_locked() -> Session {
    session_from_read(&read_live_locked())
}

/// An owned clone out of a [`ReadState`] — the one place that pays a full `Session` clone rather
/// than an `Arc` bump, for a caller that needs to mutate or mint into it.
fn session_from_read(read: &ReadState) -> Session {
    match read {
        ReadState::Ready { session, .. } => (**session).clone(),
        ReadState::Cleared { kept } | ReadState::Locked { kept } => kept.into_session(),
        ReadState::Missing | ReadState::Blocked => Session::default(),
    }
}

const SECURE_FORMAT: &str = "plxnative-secure-session";

#[derive(Deserialize, Serialize)]
struct SecureEnvelope {
    format: String,
    version: u8,
    sealed: plx_platform::tv::secure::Sealed,
}

enum ReadState {
    Missing,
    Ready {
        session: std::sync::Arc<Session>,
        plaintext: bool,
        /// A fallback served during helper unavailability must keep retrying canonical storage.
        retry_canonical: bool,
    },
    /// A recognized encrypted file whose device key is temporarily or permanently unavailable.
    /// It must shadow every lower-priority candidate: treating it as corrupt and then writing a
    /// fresh client id would destroy the only copy of the credentials.
    Locked { kept: install_preferences::Retained },
    /// The canonical authority could not answer safely. It shadows legacy candidates exactly as
    /// `Locked` does, so a fresh client id can never overwrite the only copy of the credentials.
    Blocked,
    /// The canonical authority answered with an explicit cleared/signed-out tenure record — this
    /// device really did sign out, and the authority durably recorded that. For load/lock
    /// semantics it must behave exactly like [`Missing`](ReadState::Missing): no locked/blocked UI
    /// framing, and a fresh client id is minted and persisted normally. It is still its own
    /// variant rather than `Missing` itself for the one property it does NOT share with `Missing`:
    /// it must still shadow a reappearing legacy file, exactly as `Locked`/`Blocked` do, so a
    /// stale pre-DB8 `auth.json` can never resurrect a tenure this device already cleared.
    Cleared { kept: install_preferences::Retained },
}

/// The canonical authority's answer, retaining whether protected data exists but cannot be opened.
fn read_locked(canonical: persistence::CanonicalRead) -> ReadState {
    match canonical {
        persistence::CanonicalRead::Opened { session, .. } => ReadState::Ready {
            session: std::sync::Arc::new(session),
            plaintext: false,
            retry_canonical: false,
        },
        persistence::CanonicalRead::Data { payload, .. } => {
            match serde_json::from_str::<Session>(&payload) {
                Ok(session) => ReadState::Ready {
                    session: std::sync::Arc::new(session),
                    plaintext: true,
                    retry_canonical: false,
                },
                Err(error) => {
                    plx_base::eventlog::log(&format!("session: canonical record is invalid: {error}"));
                    ReadState::Blocked
                }
            }
        }
        persistence::CanonicalRead::Missing => ReadState::Missing,
        // A cleared tenure is deliberately not Missing: it must shadow a reappearing legacy file.
        // It is also deliberately not Blocked/Locked: those carry locked/blocked UI framing that a
        // cleanly signed-out device must not present. `ReadState::Cleared` is its own variant so
        // downstream `match`es are forced to decide, rather than silently inheriting either policy.
        persistence::CanonicalRead::Cleared { kept, .. } => ReadState::Cleared { kept },
        persistence::CanonicalRead::Locked { public, .. } => ReadState::Locked { kept: install_preferences::Retained::of(&public) },
        persistence::CanonicalRead::Pending { .. } => ReadState::Blocked,
        persistence::CanonicalRead::Blocked(_) => ReadState::Blocked,
    }
}

/// Prefer any present canonical record, including Locked, Pending, invalid and Cleared.
/// Only transport unavailability permits a fallback-written file; ordinary legacy files are
/// migration inputs only when canonical is Missing. A recovered canonical record always wins.
fn read_live_locked() -> ReadState {
    #[cfg(any(test, feature = "test-support"))]
    READS_FOR_TEST.with(|c| c.set(c.get() + 1));
    #[cfg(any(test, feature = "test-support"))]
    if TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        // A redirected scratch path is an explicit host fixture. It is also deliberately not
        // behind the process-wide canonical root: dozens of existing tests grade the exact
        // scratch bytes, including recovery from states the canonical store cannot represent.
        return read_legacy_locked();
    }
    match read_canonical_locked() {
        persistence::CanonicalRead::Missing => migrate_missing_fallback_locked(read_legacy_locked()),
        persistence::CanonicalRead::Blocked(plx_platform::storage::StoreError::HelperUnavailable) => {
            match read_legacy_filtered_locked(true) {
                ReadState::Ready {
                    session, plaintext, ..
                } => ReadState::Ready {
                    session,
                    plaintext,
                    retry_canonical: true,
                },
                ReadState::Locked { kept } => ReadState::Locked { kept },
                ReadState::Cleared { kept } => ReadState::Locked { kept },
                _ => ReadState::Blocked,
            }
        }
        canonical => read_locked(canonical),
    }
}

/// A marked file is still fallback storage even if encrypted. Promote it when canonical is
/// Missing; failed migration keeps the readable snapshot transient so the worker retries it.
fn migrate_missing_fallback_locked(read: ReadState) -> ReadState {
    if let ReadState::Ready { session, retry_canonical: true, .. } = &read {
        if !cache_revoked() {
            match persistence::migrate_session(session) {
                persistence::CanonicalCommit::Durable { protection, .. } => {
                    retire_marked_fallbacks_locked();
                    return ReadState::Ready {
                        session: session.clone(), plaintext: protection.is_none(), retry_canonical: false,
                    };
                }
                _ => plx_base::eventlog::log("session: fallback migration did not complete; retaining snapshot for retry"),
            }
        }
    }
    read
}

/// Canonical reads happen under IO, on boot/storage workers, never through a frame-thread peek.
fn read_canonical_locked() -> persistence::CanonicalRead {
    let read = persistence::load();
    retire_after_canonical_read_locked(&read);
    read
}

fn retire_after_canonical_read_locked(read: &persistence::CanonicalRead) {
    if matches!(read, persistence::CanonicalRead::Data { .. }
        | persistence::CanonicalRead::Opened { .. } | persistence::CanonicalRead::Cleared { .. }) {
        retire_marked_fallbacks_locked();
    }
}

/// Read migration inputs and fallback-written files on every target, preserving sealed versus
/// plaintext handling. Missing canonical storage accepts both, so the normal bootstrap can
/// migrate a fallback too. Marked reads remain transient until canonical migration succeeds,
/// including sealed snapshots. An unavailable helper accepts only our explicit fallback marker.
/// A neutralized file (JSON null) is absent in either mode.
fn read_legacy_locked() -> ReadState {
    read_legacy_filtered_locked(false)
}

const FALLBACK_MARKER: &str = "_plxnative_session_fallback";
/// An in-place sign-out tombstone, distinct from Session's permissive empty object.
const SESSION_TOMBSTONE: &[u8] = b"null\n";

fn read_legacy_filtered_locked(fallback_only: bool) -> ReadState {
    let paths = auth_paths();
    let revoked = fallback_revoked_at(&paths);
    for path in paths {
        let Some(bytes) = read_owned_regular(&path) else {
            continue;
        };
        let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let marked = value
            .get(FALLBACK_MARKER)
            .and_then(serde_json::Value::as_u64)
            == Some(1);
        if (fallback_only && !marked) || (marked && revoked) {
            continue;
        }
        if let Some(object) = value.as_object_mut() {
            object.remove(FALLBACK_MARKER);
        }
        if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
            if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                let Some(plain) = plx_platform::tv::secure::open(&envelope.sealed) else {
                    plx_base::eventlog::log("session: secure file is present but its device key is unavailable");
                    return ReadState::Locked { kept: install_preferences::load().unwrap_or_default() };
                };
                return serde_json::from_slice::<Session>(&plain)
                    .map(|session| ReadState::Ready {
                        session: std::sync::Arc::new(session),
                        plaintext: false,
                        retry_canonical: marked,
                    })
                    .unwrap_or(ReadState::Locked { kept: install_preferences::load().unwrap_or_default() });
            }
        }
        if identifies_secure_envelope(&bytes) {
            plx_base::eventlog::log("session: unsupported or damaged secure envelope is locked");
            return ReadState::Locked { kept: install_preferences::load().unwrap_or_default() };
        }
        if let Ok(session) = serde_json::from_value::<Session>(value) {
            return ReadState::Ready {
                session: std::sync::Arc::new(session),
                plaintext: true,
                retry_canonical: marked,
            };
        }
    }
    install_preferences::load().map_or(ReadState::Missing, |kept| ReadState::Cleared { kept })
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

fn has_unmarked_secure_locked() -> bool {
    auth_paths().iter().any(|path| read_owned_regular(path)
        .is_some_and(|bytes| identifies_secure_envelope(&bytes) && !marked_fallback(&bytes)))
}

/// Seed a quality only for a genuinely absent file. A parsable legacy file remains distinguishable
/// even when it omitted `client_id`; otherwise opening the Auto gate in a future build would turn
/// that old install into a fresh one merely because its identifier also needed repair.
fn seed_fresh_quality(s: &mut Session, persisted: bool, auto_ready: bool) {
    if !persisted && s.prefs.playback_quality.is_none() {
        s.prefs.playback_quality = Some(PlaybackQuality::fresh_default(auto_ready));
    }
}

/// **Whether a fresh record is seeded with Auto quality** is `route::auto_quality_ready()`'s answer,
/// and `route` sits above `plex`, so the app installs that function as a hook
/// ([`install_auto_quality_ready`]; `app::boot::install_plex_seams`, first thing in
/// `app::enter_application`, before the first [`load`]). Unset it reads `true` — what the gate has
/// returned since the adaptive path was completed, and what every host test that loads the session
/// without booting the app has always seen.
static AUTO_QUALITY_READY: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();

/// Install the Auto-quality gate [`prepare_load`] seeds a fresh record from. Once; later calls are
/// ignored.
pub fn install_auto_quality_ready(ready: fn() -> bool) {
    let _ = AUTO_QUALITY_READY.set(ready);
}

fn auto_quality_ready() -> bool {
    AUTO_QUALITY_READY.get().is_none_or(|ready| ready())
}

/// **The telemetry/consent legacy-file sweep that follows a durable canonical clear**
/// ([`clear_for_erase`]'s ARM arm): `telemetry` owns the files and sits above `plex`, so the app
/// installs its `cleanup_after_account_clear` as a hook ([`install_account_clear_cleanup`];
/// `app::boot::install_plex_seams`). It answers whether every candidate was retired. Unset there is
/// nothing registered to retire, which reads as retired, so the "could not be retired" branch is
/// never taken without a sweep that failed. Compiled where the sweep is: ARM, not the simulator,
/// not a test build.
#[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
static ACCOUNT_CLEAR_CLEANUP: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();

/// Install the sweep [`ACCOUNT_CLEAR_CLEANUP`] runs. Once; later calls are ignored.
#[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
pub fn install_account_clear_cleanup(sweep: fn() -> bool) {
    let _ = ACCOUNT_CLEAR_CLEANUP.set(sweep);
}

/// **Hand the scrubber this household's names**, so `plx_base::eventlog::log` can redact them without ever
/// touching this module.
///
/// The scrubber used to call [`peek`] per line, which took [`IO`] and read the file — a deadlock
/// against every writer here (`save_locked` logs while holding the lock) and a syscall storm on
/// the log path besides. Ownership is inverted now: the session layer PUSHES on every change and
/// `eventlog::scrub` keeps a cached snapshot.
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
        // The origin's HOST too: a share reached through a custom access URL carries the friend's
        // own domain, which is neither a `plex.direct` label nor a bare address — the two shapes
        // the scrubber recognises on its own — and it surfaced verbatim in a `stream: … DNS
        // FAILED host=…` line on 2026-09-06.
        if let Some(o) = src.origin() {
            v.push(o.host().to_string());
        }
    }
    v.push(s.server.origin().host().to_string());
    plx_base::eventlog::scrub::set_identities(v);
}

/// Load the persisted session, ensuring a stable `client_id` exists (generated + saved on first
/// boot). Never returns an error — a missing/corrupt file degrades to a fresh, logged-out session.
/// Falls back to the pre-relocation path once and re-saves at the new one (migration).
pub fn load() -> Session {
    load_with_id(new_client_id)
}

/// Resolve the install identifier on the persistence worker before starting a QR attempt.
/// A revoked cache stays revoked: reading its old install id never republishes its credentials.
/// If storage is unavailable, reuse one process-local id; persist only over Missing/Cleared.
pub fn load_login_client_id() -> String {
    let _io = io();
    let read = read_live_locked();
    if let ReadState::Ready { session, .. } = &read {
        if !session.client_id.is_empty() { return session.client_id.clone(); }
    }
    static FALLBACK: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let id = FALLBACK.get_or_init(new_client_id).clone();
    if matches!(read, ReadState::Missing | ReadState::Cleared { .. }) {
        let fresh = Session { client_id: id.clone(), ..session_from_read(&read) };
        save_locked(&fresh);
    }
    id
}

/// A captured loader resource action, not serialized initialization. Dropping it has no effect.
pub struct DeferredLoad {
    session: Session,
    expected: Vec<u8>,
    save: bool,
}
fn read_identity(read: &ReadState) -> Vec<u8> {
    match read {
        ReadState::Missing => vec![0],
        ReadState::Blocked => vec![1],
        ReadState::Locked { kept } => [vec![1], kept.language.tag().as_bytes().to_vec(), kept.ui_resolution.tag().as_bytes().to_vec()].concat(),
        // Its own bucket, distinct from both Missing and Locked/Blocked: a concurrent transition
        // into or out of Cleared must be detectable by `DeferredLoad::apply`'s identity check, not
        // silently matched against whichever of those two buckets it happens to share a vec! with.
        ReadState::Cleared { kept } => [vec![2], kept.language.tag().as_bytes().to_vec(), kept.ui_resolution.tag().as_bytes().to_vec()].concat(),
        ReadState::Ready { session, .. } => serde_json::to_vec(&**session).expect("Session serialization"),
    }
}
impl DeferredLoad {
    /// Only after validation and recorder attachment. Recheck under the normal file lock so
    /// a writer between capture and attachment is not overwritten by an obsolete snapshot.
    pub fn apply(self) -> Result<(), &'static str> {
        let _io = io();
        let read = std::sync::Arc::new(read_live_locked());
        if read_identity(&read) != self.expected {
            // Refused, exactly like `update_with_outcome`'s own refusal path: install the record
            // this capture lost the race against, rather than leaving the cache empty for the
            // session owner to refill with a second read of what this call just established
            // under `IO`.
            install_locked(read);
            return Err("session changed during capture");
        }
        if self.save {
            // `save_locked` installs (or drops) the cache itself, from the write's own proven
            // outcome — see its module doc.
            save_locked(&self.session);
        } else {
            // No write happens on this path, but the read above IS the verified record: install
            // it so the cache is not left empty for the session owner to refill with a second
            // read of what this call just established under `IO`.
            install_locked(read);
        }
        publish_identities(&self.session);
        Ok(())
    }
}

/// Capture read/mint inputs; ordinary saves and identity publication remain deferred.
/// A marked fallback can migrate during the authority read when canonical storage recovers.
pub fn load_capturing_entropy() -> (Session, Option<[u8; 16]>, DeferredLoad) {
    let _io = io();
    let read = read_live_locked();
    let expected = read_identity(&read);
    let mut captured = None;
    let (session, save) = prepare_load(&read, || {
        let bytes = random_bytes();
        captured = Some(bytes);
        client_id_from_entropy(bytes)
    });
    let deferred = DeferredLoad { session: session.clone(), expected, save };
    (session, captured, deferred)
}

/// Whether a cached [`ReadState`] answers [`load_with_id`] with no `IO` at all: an established,
/// non-empty `client_id` that is not sitting in a plaintext file — the two things `prepare_load`
/// would otherwise decide to re-save over. Anything else must fall through to the ordinary
/// read-modify-write, so a save is never built from a read that was not taken in the same `IO`
/// critical section as the write it might cause — the no-lost-update invariant `IO`'s own doc
/// states, which caching must not weaken.
fn established(read: &ReadState) -> bool {
    matches!(
        read,
        ReadState::Ready { session, plaintext: false, .. } if !session.client_id.is_empty()
    )
}

fn load_with_id(mint: impl FnOnce() -> String) -> Session {
    if cache_revoked() { return Session::default(); }
    if let Some(read) = cached_settled() {
        if established(&read) {
            let (s, save) = prepare_load(&read, mint);
            debug_assert!(!save, "an established, protected record must never need a resave");
            publish_identities(&s);
            return s;
        }
    }
    let _io = io();
    // The same race a synchronous cache fill guards against: another caller may have installed an established
    // record while this one waited for `IO`, in which case re-reading storage here would be a
    // second, needless read of a record already proved.
    if let Some(read) = cached_settled() {
        if established(&read) {
            let (s, save) = prepare_load(&read, mint);
            debug_assert!(!save, "an established, protected record must never need a resave");
            publish_identities(&s);
            return s;
        }
    }
    let read = refresh_locked();
    if cache_revoked() { return Session::default(); }
    let (s, save) = prepare_load(&read, mint);
    if save { save_locked(&s); }
    publish_identities(&s);
    s
}

fn prepare_load(read: &ReadState, mint: impl FnOnce() -> String) -> (Session, bool) {
    // A cleared tenure is grouped with Missing here, deliberately not with Locked/Blocked: it is
    // not "a persisted session exists" (there is nothing to preserve), and — the actual fix this
    // exists for — it must not be `locked`, which is what drives locked/blocked UI/boot framing.
    let persisted = !matches!(read, ReadState::Missing | ReadState::Cleared { .. });
    let locked = matches!(read, ReadState::Locked { .. } | ReadState::Blocked);
    let plaintext = matches!(
        read,
        ReadState::Ready {
            plaintext: true,
            retry_canonical: false,
            ..
        }
    );
    let mut s = match read {
        ReadState::Ready { session, .. } => (**session).clone(),
        ReadState::Missing | ReadState::Locked { .. } | ReadState::Blocked | ReadState::Cleared { .. } => {
            let mut fresh = session_from_read(read);
            // Product default is on. `Default` for a bool is off, and this is the path that
            // writes the first file, so set it before that save. Like `fresh_default` quality, it
            // sets the install-wide default the profiles with no value of their own fall back to.
            fresh.prefs.trailer_autoplay = true;
            fresh
        }
    };
    seed_fresh_quality(&mut s, persisted, auto_quality_ready());
    let fresh = s.client_id.is_empty();
    if fresh {
        s.client_id = mint();
    }
    // Preserve ordinary fresh/locked/plaintext policy; only its execution boundary is deferred.
    (s, (fresh && !locked) || (!fresh && plaintext))
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
    update_with_outcome(edit).is_some()
}

/// Blocking persistence seam; Language settings dispatches it on the storage worker.
/// A next-launch promise requires the same confirmed durability as playback preferences.
pub fn set_language(language: plx_platform::i18n::Preference) -> bool {
    let saved = update_with_outcome(|current| {
        let mut next = current.clone();
        next.language = language;
        Some(next)
    }).is_some_and(|write| matches!(write.classify(),
        async_persistence::CompletionOutcome::Durable(_)));
    if saved { plx_platform::i18n::set_saved_preference(language); }
    saved
}

/// Blocking persistence seam for the Interface resolution picker, dispatched on the storage
/// worker. The choice applies at the NEXT launch, so a pick is only claimed once it is durable.
pub fn set_ui_resolution(mode: UiResolution) -> bool {
    let mut already = false;
    let write = update_with_outcome(|current| {
        already = current.ui_resolution() == mode;
        (!already).then(|| current.with_ui_resolution(mode))
    });
    // Already the stored choice is a pick that landed; an unreadable or refused record is not.
    already || write.is_some_and(|write| matches!(write.classify(), async_persistence::CompletionOutcome::Durable(_)))
}


/// [`update`], but reporting what the durable write actually did.
///
/// The persistence worker uses this blocking operation; the frame thread polls its receipt.
/// Callers that must not conflate "the write was attempted" with "the write reached disk"
/// use this; the typed persistence completion is built from this real outcome rather than assumed.
///
/// It hands back the whole [`async_persistence::LiveWrite`] — the canonical verdict as well as the
/// legacy write's result — because collapsing the two into one "persisted" bool is exactly how an
/// `Uncertain` canonical commit used to be reported as a durable login.
pub fn update_with_outcome(
    edit: impl FnOnce(&Session) -> Option<Session>,
) -> Option<async_persistence::LiveWrite> {
    update_guarded_with_outcome(edit, || true)
}

pub fn update_guarded_with_outcome(
    edit: impl FnOnce(&Session) -> Option<Session>,
    may_write: impl Fn() -> bool,
) -> Option<async_persistence::LiveWrite> {
    let _io = io();
    if cache_revoked() { return None; }
    let read = std::sync::Arc::new(read_live_locked());
    let cur = session_of(&read);
    if cache_revoked() { return None; }
    if cur.client_id.is_empty() {
        // Nothing readable to modify. Install this fresh truth anyway — it is what makes `peek()`
        // show a concurrent external change afterwards, exactly as an uncached re-read always did.
        install_locked(read);
        return None;
    }
    match edit(&cur) {
        Some(next) if may_write() && !cache_revoked() => Some(save_locked_outcome(&next)),
        Some(_) => None,
        None => {
            // Refused by the caller's own policy: install the record it refused OVER, not
            // whatever used to be cached — a stale hit here would show a change that never
            // happened.
            install_locked(read);
            None
        }
    }
}

/// The whole-record write only a completed PIN authorization may perform (the 0.6.6
/// `save_after_reauthentication` door). Unlike [`update_with_outcome`] it does NOT refuse when the
/// disk reads as Locked/Blocked/Missing (those read as a default `Session`, whose empty `client_id`
/// makes the read-modify-write a silent no-op): the user has just re-supplied everything the
/// ciphertext held, and a sign-in nobody can read back next launch is the worst outcome available.
///
/// A disk that DOES hold a **readable** record (non-empty `client_id`) is still fenced against
/// `fence`, exactly like an ordinary write — a fresh sign-in must still lose to a *readable*
/// record a concurrent actor already replaced, the same OCC protection `update_with_outcome`'s
/// `Routine` callers get. Only the unreadable case is deliberately left unfenced, since a
/// Locked/Blocked/Missing read can never match anything the owner minted and refusing there is
/// exactly the 0.6.3 symptom AUTH-03 exists to end. `fence` returning `false` refuses the write
/// entirely (`Err`), before anything reaches disk.
///
/// Fresh authority without an account credential writes nothing (mirrors
/// `async_persistence::Coordinator::admit_with`'s `account_token.is_empty()` refusal).
pub fn replace_after_reauthentication_with_outcome(
    fence: impl FnOnce(&Session) -> bool,
    edit: impl FnOnce(&Session) -> Session,
) -> Result<Option<async_persistence::LiveWrite>, ()> {
    replace_after_reauthentication_guarded_with_outcome(fence, edit, || true)
}

pub fn replace_after_reauthentication_guarded_with_outcome(
    fence: impl FnOnce(&Session) -> bool,
    edit: impl FnOnce(&Session) -> Session,
    may_write: impl Fn() -> bool,
) -> Result<Option<async_persistence::LiveWrite>, ()> {
    let _io = io();
    let read = std::sync::Arc::new(read_live_locked());
    let cur = session_of(&read);
    if !cur.client_id.is_empty() && !fence(&cur) {
        install_locked(read);
        return Err(());
    }
    let mut next = edit(&cur);
    if next.account_token.is_empty() {
        install_locked(read);
        return Ok(None);
    }
    // The one login that is not a sign-out: the stored session is still on disk, so no `ClearTenure`
    // ran. A different account must not open on the previous one's library views, which carry no
    // account stamp to read as nothing under (`ACCOUNT_BOUND_PREFERENCES` is what sign-out clears),
    // nor on its Home pins and search history: those are keyed by profile `uuid` with `""` for the
    // owner of EVERY account, and sign-out drops them with the file.
    //
    // "Different account" is the OWNER'S IDENTITY (`owner_identity` of the roster each side holds),
    // never the token: a fresh QR sign-in always mints a new plex.tv token, so the same person
    // signing in again differs by token and must keep their pins, searches and library memory.
    // Unknown on either side (a roster not yet fetched, or one without a nameable admin) clears.
    if !cur.account_token.is_empty() && !same_owner(&cur.home_users, &next.home_users) {
        next.prefs.last_library.clear();
        next.prefs.library_views.clear();
        next.home_pins.clear();
        next.recent_searches.clear();
    }
    if !may_write() { return Err(()); }
    Ok(Some(save_locked_with_authority(&next, SaveAuthority::FreshReauthentication)))
}

/// The stable identity of the account a roster belongs to: its admin entry's plex.tv account id
/// (the same id space as `account::Resource::owner_id`), else that entry's uuid when an old file
/// carries no id. `None` when the roster is empty ("unknown", not "nobody"), has no admin, or the
/// admin names neither. A token is NOT an identity: every sign-in mints a new one.
pub fn owner_identity(roster: &[HomeUserRef]) -> Option<String> {
    let admin = roster.iter().find(|u| u.admin)?;
    if admin.id != 0 { Some(format!("id:{}", admin.id)) }
    else if !admin.uuid.is_empty() { Some(format!("uuid:{}", admin.uuid)) }
    else { None }
}

/// Do two rosters name the SAME account? Fails safe: an identity either side cannot name is "no".
pub fn same_owner(a: &[HomeUserRef], b: &[HomeUserRef]) -> bool {
    matches!((owner_identity(a), owner_identity(b)), (Some(x), Some(y)) if x == y)
}

/// What the routine-authority write actually did — the canonical verdict beside the
/// sealed/plaintext attempt's own result, without changing any caller's behavior.
fn save_locked_outcome(s: &Session) -> async_persistence::LiveWrite {
    save_locked_with_authority(s, SaveAuthority::Routine)
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
/// keymanager3 keeps the compatible 0600 plaintext fallback. An existing encrypted file is never
/// downgraded merely because its service is temporarily unavailable.
///
/// The mode is set in `open(2)`'s own argument — never create-then-chmod. `fs::write` creates with
/// `0666 & !umask` (0644 here), so a fallback token file would be readable by every other uid from
/// the instant it hit the disk. Passing the mode through `OpenOptionsExt` means it never *exists*
/// in a permissive mode, which a chmod after the write cannot promise.
pub fn save(s: &Session) {
    let _io = io();
    let _ = save_locked_with_authority(s, SaveAuthority::FreshReauthentication);
}

pub fn save_fresh_reauthentication(s: &Session) {
    let _io = io();
    let _ = save_locked_with_authority(s, SaveAuthority::FreshReauthentication);
}

/// [`save`] with the lock already held. Ordinary read-modify-writes never ask for fresh login
/// authority; only [`save`] and its confirmed-auth adapter may do so.
fn save_locked(s: &Session) {
    let _ = save_locked_with_authority(s, SaveAuthority::Routine);
}

/// Write the session, reporting BOTH verdicts the write produced.
///
/// The canonical authority's [`persistence::CanonicalCommit`] is carried out of here rather than
/// reduced to "sealed / plaintext / nothing" on the way: a non-durable canonical commit with no
/// protected authority falls through to the legacy write below, that write succeeds, and a caller
/// holding only the bool cannot tell that apart from a commit the store confirmed. The typed
/// completion the live adapter publishes is built from the pair by
/// [`async_persistence::LiveWrite::classify`].
fn save_locked_with_authority(
    s: &Session,
    authority: SaveAuthority,
) -> async_persistence::LiveWrite {
    // A sign-out can revoke the cache while disk I/O is in flight. Even a durable write must
    // not overwrite that newer local decision when it returns; its owner still gets the receipt.
    let cache_generation = CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed);
    // Every caller of this function holds `IO` already (it is private and reached only through
    // the entry points that took it) — which is what makes installing this write's outcome into
    // `CACHE` below safe. See the module doc's cache invariants.
    #[cfg(any(test, feature = "test-support"))]
    {
        *LAST_WRITE_AUTHORITY.lock().unwrap_or_else(|e| e.into_inner()) = Some(authority);
    }
    // Before the write, not after: a failed persist still means these names are live in THIS run,
    // and the log wants them redacted either way.
    publish_identities(s);
    #[cfg(any(test, feature = "test-support"))]
    if TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        let legacy = save_legacy_locked(s);
        install_or_drop_after_write(s, legacy, authority, cache_generation);
        return async_persistence::LiveWrite::legacy(legacy);
    }
    plx_platform::storage::wire::failure::clear();
    CANDIDATE_ERRNOS.with(|slot| slot.set([None; 8]));
    let protected_before = has_protected_authority();
    let commit = persistence::write_session_timed(s, authority);
    // Uncertain helper replies already own their evidence in CanonicalCommit. Only the older
    // StoreError-only failure variants still need this immediate thread-local snapshot.
    let helper_failure = match &commit {
        persistence::CanonicalCommit::Failed(plx_platform::storage::StoreError::HelperUnavailable
            | plx_platform::storage::StoreError::HelperAuthentication | plx_platform::storage::StoreError::HelperProtocol) =>
            Some(plx_platform::storage::wire::failure::last().unwrap_or_else(||
                plx_platform::storage::wire::failure::HelperFailure::new(plx_platform::storage::wire::failure::Stage::Unknown, None))),
        _ => None,
    };
    let durable = matches!(commit, persistence::CanonicalCommit::Durable { .. });
    if !durable {
        match &commit {
            persistence::CanonicalCommit::Durable { .. } => unreachable!(),
            persistence::CanonicalCommit::Uncertain { stage, errno, helper } => {
                plx_base::eventlog::log(&format!("session: canonical write is uncertain stage={stage:?} errno={errno} helper={helper:?}"));
            }
            persistence::CanonicalCommit::Failed(error) => {
                plx_base::eventlog::log(&format!("session: canonical write failed: {error:?} helper={helper_failure:?}"));
            }
            persistence::CanonicalCommit::ProtectionFailed(failure) => {
                plx_base::eventlog::log(&format!(
                    "session: canonical protection failed: {:?}, commit_verified={}",
                    failure.failure, failure.db8_commit_verified
                ));
            }
        }
    }
    let protected_after = has_protected_authority();
    if durable {
        canonical_write_completed_locked(s, authority, cache_generation);
        let protection = match &commit {
            persistence::CanonicalCommit::Durable { protection, .. } => *protection,
            _ => unreachable!(),
        };
        // A `Durable` commit IS the record now — install it rather than drop it, so the very next
        // `peek()` (even the caller's own, right after this returns) is served from memory instead
        // of finding the cache left `Unloaded` until the session owner refills it.
        install_write_locked(std::sync::Arc::new(ReadState::Ready {
            session: std::sync::Arc::new(s.clone()),
            plaintext: protection.is_none(),
            retry_canonical: false,
        }), authority, cache_generation);
        return async_persistence::LiveWrite::canonical(
            commit,
            Some(protected_before || protected_after),
        );
    }
    let legacy = save_legacy_fallback_locked(s, protected_before, protected_after);
    if legacy.is_some() {
        end_fallback_revocation_locked(s, authority, cache_generation, true);
    }
    // Re-read canonical before trusting the marked fallback: an uncertain write may actually
    // have landed, or a present record may outrank it. If the helper is still unavailable, the
    // next read serves the fallback as Transient, never as settled canonical state. Read installs
    // preserve local Revoked even when this fallback write succeeded.
    drop_cache_locked();
    async_persistence::LiveWrite::canonical(commit, legacy).with_helper_failure(helper_failure)
}

/// The `#[cfg(test)]` `TEST_FILE` path's write outcome, where the legacy file IS the only record
/// (there is no canonical store to outrank it): `Some(sealed)` is what was actually written
/// (sealed or plaintext), which IS provably the record, so it is installed; `None` means nothing
/// landed anywhere, so whatever was cached before is no longer trustworthy and must be dropped
/// rather than risk serving a value the disk does not hold. **Not** used on the canonical path —
/// see the comment at its one non-durable call site for why a legacy write there can't be trusted
/// as the record either way.
fn install_or_drop_after_write(s: &Session, legacy: Option<bool>, authority: SaveAuthority, generation: u64) {
    match legacy {
        Some(sealed) => {
            end_fallback_revocation_locked(s, authority, generation, false);
            install_write_locked(std::sync::Arc::new(ReadState::Ready {
                session: std::sync::Arc::new(s.clone()),
                plaintext: !sealed,
                retry_canonical: false,
            }), authority, generation);
        },
        None => drop_cache_locked(),
    }
}

/// Tag the existing file representation without changing its encryption or candidate paths.
/// Readers strip this storage metadata before decoding Session's flattened extension fields.
fn fallback_bytes(value: &impl Serialize) -> Result<Vec<u8>, serde_json::Error> {
    let mut value = serde_json::to_value(value)?;
    value
        .as_object_mut()
        .expect("session/envelope object")
        .insert(FALLBACK_MARKER.into(), 1.into());
    serde_json::to_vec_pretty(&value)
}

/// The pre-canonical sealed/plaintext write, run only where the canonical commit did NOT land.
///
/// Split out of [`save_locked_with_authority`] so that function can return the canonical verdict
/// alongside this one. Files are marked for unavailable-helper reads; every refusal to downgrade
/// a protected record is retained. `Some(true)` sealed, `Some(false)` plaintext, `None` nothing was written.
fn save_legacy_fallback_locked(
    s: &Session,
    protected_before: bool,
    protected_after: bool,
) -> Option<bool> {
    // Only our marked fallback envelopes may be resealed here. Unmarked legacy secure files
    // still refuse the entire fallback write, as before; the plaintext arm never downgrades either.
    if protected_before || protected_after || has_unmarked_secure_locked() {
        plx_base::eventlog::log("session: preserving the existing protected record; refusing an unprotected downgrade");
        return None;
    }
    if let Some(sealed) = plx_platform::tv::secure::seal(&serde_json::to_vec_pretty(s).ok()?) {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed,
        };
        let Ok(protected) = fallback_bytes(&envelope) else {
            return None;
        };
        let mut failures = Vec::new();
        for winner in auth_paths() {
            match write_atomic_diagnosed(&winner, &protected) {
                Ok(()) => {
                    retire_other_fallback_candidates_locked(&winner, true);
                    if !failures.is_empty() {
                        plx_base::eventlog::log(
                            "session: protected write succeeded on a later candidate; earlier ones refused",
                        );
                        log_candidate_diagnostics("protected write refused before the later success", &failures);
                    }
                    return Some(true);
                }
                Err(diagnostic) => failures.push(diagnostic),
            }
        }
        plx_base::eventlog::log("session: key manager succeeded but the protected file could not be written");
        log_candidate_diagnostics("protected write refused", &failures);
        return None;
    }
    // Never turn an already protected session back into plaintext because a service was
    // temporarily unavailable during a save. Preserve the previous ciphertext instead.
    if has_secure_locked() {
        plx_base::eventlog::log("session: preserving the existing secure file; refusing a plaintext downgrade");
        return None;
    }
    let Ok(json) = fallback_bytes(s) else {
        return None;
    };
    // Try each candidate; the first that accepts the write wins. A total failure is still
    // non-fatal — but it is LOGGED, because the symptom (sign in again, every boot, forever) is
    // otherwise indistinguishable from a server-side auth problem and impossible to report.
    // `auth_paths()` — i.e. `paths::session_candidates()` — decides how many candidates that is;
    // this loop makes no assumption about the count.
    let mut failures = Vec::new();
    for path in auth_paths() {
        match write_atomic_diagnosed(&path, &json) {
            Ok(()) => {
                retire_other_fallback_candidates_locked(&path, false);
                if !failures.is_empty() {
                    plx_base::eventlog::log(
                        "session: plaintext write succeeded on a later candidate; earlier ones refused",
                    );
                    log_candidate_diagnostics("plaintext write refused before the later success", &failures);
                }
                return Some(false);
            }
            Err(diagnostic) => failures.push(diagnostic),
        }
    }
    plx_base::eventlog::log(
        "session: could not persist to ANY candidate path — login will not survive a reboot",
    );
    log_candidate_diagnostics("plaintext write refused", &failures);
    None
}

/// Sealed writes retain the existing sweep of other plaintext migration inputs, so credentials
/// are not left unprotected elsewhere. Plaintext writes retire only other marked snapshots.
fn retire_other_fallback_candidates_locked(winner: &std::path::Path, sealed: bool) {
    let mut complete = retry_pending_retirements_locked();
    for stale in auth_paths().into_iter().filter(|path| path != winner) {
        if sealed || read_owned_regular(&stale).is_some_and(|bytes| marked_fallback(&bytes)) {
            remove_temp_siblings(&stale);
            complete &= retire_session_candidate(&stale);
        }
    }
    if complete { retry_pending_revocation_removals_locked(); }
}

#[cfg(any(test, feature = "test-support"))]
fn save_legacy_locked(s: &Session) -> Option<bool> {
    let Ok(json) = serde_json::to_vec_pretty(s) else {
        return None;
    };
    if let Some(sealed) = plx_platform::tv::secure::seal(&json) {
        let envelope = SecureEnvelope {
            format: SECURE_FORMAT.to_string(),
            version: 1,
            sealed,
        };
        let Ok(protected) = serde_json::to_vec_pretty(&envelope) else {
            return None;
        };
        for winner in auth_paths() {
            if write_atomic(&winner, &protected).is_ok() {
                for stale in auth_paths().into_iter().filter(|p| p != &winner) {
                    remove_temp_siblings(&stale);
                    let _ = std::fs::remove_file(stale);
                }
                return Some(true);
            }
        }
        return None;
    }
    if has_secure_locked() {
        return None;
    }
    for path in auth_paths() {
        if write_atomic(&path, &json).is_ok() {
            return Some(false);
        }
    }
    None
}

fn has_protected_authority() -> bool {
    match read_canonical_locked() {
        persistence::CanonicalRead::Locked { protection, .. } => protection.is_some_and(|outcome| {
            !matches!(outcome.class, plx_platform::storage::wire::ProtectionClass::Db8AclOnly)
        }),
        _ => false,
    }
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
/// `pub` since 2026-08-29 so `crate::telemetry` writes its file the same way rather than
/// growing a second implementation of this. It is a generic 0600 atomic write that happens to live
/// beside its first caller; the alternative was two copies of a routine whose whole value is that
/// its failure modes have already been found once, on the file holding the credentials.
///
/// Returns the [`WriteFailure`] the OS actually gave, rather than a bare no: a caller trying
/// several candidates (session save's own fallback, the crash watermark, the telemetry spool) used
/// to be left with "none of them took it" and nothing that could tell EACCES (a jail whose
/// permissions changed) from EROFS (a mount gone read-only) from ENOENT (a directory a factory
/// reset removed) apart — see [`save_legacy_fallback_locked`], the one caller that now reports the
/// difference.
pub fn write_atomic(path: &std::path::Path, json: &[u8]) -> Result<(), WriteFailure> {
    use std::io::Write;
    use std::os::unix::fs::MetadataExt;
    let Some(parent) = path.parent() else {
        return Err(WriteFailure::InvalidPath);
    };
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(WriteFailure::NotOwned);
        }
    }
    let (tmp, mut f) = create_private_temp(path)?;
    let write_result = f.write_all(json).and_then(|()| f.sync_all());
    drop(f); // the rename must not race our own open handle on a filesystem that cares
    if let Err(e) = write_result {
        let _ = std::fs::remove_file(&tmp);
        return Err(WriteFailure::WriteFailed(e.raw_os_error().unwrap_or(0)));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        // Leave no half-written credentials behind under a name the next writer would overwrite
        // anyway — and none at all if this candidate turned out to be unwritable.
        let _ = std::fs::remove_file(&tmp);
        return Err(WriteFailure::RenameFailed(e.raw_os_error().unwrap_or(0)));
    }
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    remove_temp_siblings(path);
    Ok(())
}

/// Why one candidate refused an atomic write. Errno-bearing where the OS actually returned one —
/// [`Self::errno`] — so a reader (the field log today; the failure read-out's Details card,
/// `screens::login::support_line`) can tell EACCES from EROFS from ENOENT instead of a bare no.
/// Never sent over the network: no path, uid, gid or mode belongs in `telemetry::incident`'s closed
/// vocabulary (see that module's doc), so this type stays local to the write path and its callers'
/// own logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteFailure {
    /// The path has no parent directory, or no file name, to build a sibling temp name from — a
    /// degenerate candidate, not a syscall failure.
    InvalidPath,
    /// A file already exists at the destination under a uid this process does not own, or is not a
    /// regular file. Refused before any write was attempted, so there is no errno.
    NotOwned,
    /// The private temp file could not be created; the errno from the `open(2)` that failed.
    CreateFailed(i32),
    /// Every random temp suffix this attempt tried already existed on disk.
    Exhausted,
    /// The write or its `fsync` failed; the errno from whichever failed.
    WriteFailed(i32),
    /// The rename into place failed; the errno from `rename(2)`.
    RenameFailed(i32),
}

impl WriteFailure {
    /// The OS errno this failure carries, when it carries one — `None` for a refusal this process
    /// made itself before any syscall had a chance to fail.
    pub fn errno(self) -> Option<i32> {
        match self {
            Self::CreateFailed(e) | Self::WriteFailed(e) | Self::RenameFailed(e) => Some(e),
            Self::InvalidPath | Self::NotOwned | Self::Exhausted => None,
        }
    }
}

/// The parent directory's owner and mode at the moment a candidate was tried — the other half of
/// what makes a "could not persist" line actionable: an errno alone does not say whether the
/// directory is simply not this process's, or is not writable by anyone, or does not exist.
#[derive(Debug, Clone, Copy)]
pub struct ParentStat {
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
}

/// One candidate [`write_atomic`] tried: where, what its parent directory looked like, and why it
/// refused. Built for the field log ([`save_legacy_fallback_locked`]) — local diagnostic evidence
/// only, never folded into `telemetry::incident::IncidentContext`.
#[derive(Debug, Clone)]
pub struct CandidateDiagnostic {
    pub path: std::path::PathBuf,
    /// `None` when even `stat` on the parent failed (it does not exist, or a component above it
    /// is not searchable).
    pub parent: Option<ParentStat>,
    pub failure: WriteFailure,
}

fn parent_stat(path: &std::path::Path) -> Option<ParentStat> {
    use std::os::unix::fs::MetadataExt;
    let parent = path.parent()?;
    let meta = std::fs::metadata(parent).ok()?;
    Some(ParentStat { uid: meta.uid(), gid: meta.gid(), mode: meta.mode() & 0o7777 })
}

thread_local! {
    // This write's bounded errno projection only: paths, identities and modes stay local.
    static CANDIDATE_ERRNOS: std::cell::Cell<[Option<i32>; 8]> = const { std::cell::Cell::new([None; 8]) };
}
pub fn candidate_errnos() -> [Option<i32>; 8] { CANDIDATE_ERRNOS.with(|slot| slot.get()) }

/// [`write_atomic`], plus the local diagnostic and its numeric-only report projection.
fn write_atomic_diagnosed(path: &std::path::Path, json: &[u8]) -> Result<(), CandidateDiagnostic> {
    write_atomic(path, json).map_err(|failure| {
        if let Some(errno) = failure.errno() {
            CANDIDATE_ERRNOS.with(|slot| {
                let mut values = slot.get();
                if let Some(empty) = values.iter_mut().find(|n| n.is_none()) { *empty = Some(errno); }
                slot.set(values);
            });
        }
        CandidateDiagnostic { path: path.to_path_buf(), parent: parent_stat(path), failure }
    })
}

/// One line per failed candidate — path, parent uid/gid/mode (octal) or `unknown` when `stat`
/// itself failed, and the errno or the refusal class when there is no errno. The format a person
/// can read off a log, or a future on-screen diagnostic, without guessing what each number means.
fn log_candidate_diagnostics(context: &str, failures: &[CandidateDiagnostic]) {
    for d in failures {
        let parent = d.parent.map_or_else(
            || "parent=unknown".to_string(),
            |p| format!("parent_uid={} parent_gid={} parent_mode={:03o}", p.uid, p.gid, p.mode),
        );
        let class = match d.failure {
            WriteFailure::InvalidPath => "invalid_path",
            WriteFailure::NotOwned => "not_owned",
            WriteFailure::CreateFailed(_) => "create_failed",
            WriteFailure::Exhausted => "exhausted",
            WriteFailure::WriteFailed(_) => "write_failed",
            WriteFailure::RenameFailed(_) => "rename_failed",
        };
        let errno = d.failure.errno().map_or_else(|| "none".to_string(), |e| e.to_string());
        plx_base::eventlog::log(&format!(
            "session: {context} path={} {parent} cause={class} errno={errno}",
            d.path.display()
        ));
    }
}

fn create_private_temp(
    path: &std::path::Path,
) -> Result<(std::path::PathBuf, std::fs::File), WriteFailure> {
    use std::os::unix::fs::OpenOptionsExt;
    for attempt in 0..16u64 {
        let Some(tmp) = random_tmp_path(path, attempt) else {
            return Err(WriteFailure::InvalidPath);
        };
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&tmp)
        {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(WriteFailure::CreateFailed(e.raw_os_error().unwrap_or(0))),
        }
    }
    Err(WriteFailure::Exhausted)
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

/// Open `path` for reading without following a symlink at its name, and only if it is a regular
/// file this process owns. The metadata comes from the SAME descriptor the bytes will be read
/// from, so a caller that records the file's identity describes what it read. A missing file is
/// `NotFound`; anything else refused is `PermissionDenied`.
pub fn open_owned_regular(
    path: &std::path::Path,
) -> std::io::Result<(std::fs::File, std::fs::Metadata)> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    // O_NONBLOCK: a FIFO planted at the name must not block the open (it is refused just below);
    // it changes nothing for a regular file.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok((file, meta))
}

/// The most [`read_owned_regular`] allocates for one file.
pub const MAX_OWNED_FILE: u64 = 4 * 1024 * 1024;

pub fn read_owned_regular(path: &std::path::Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let (mut file, _) = open_owned_regular(path).ok()?;
    const MAX_FILE: u64 = MAX_OWNED_FILE;
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

/// What [`clear`] found out about the canonical authority. Distinct from a bare `()` return
/// because a sign-out that fails to durably reach the canonical authority is a real
/// security-relevant outcome — the account token may still be readable on the next boot — and a
/// caller that cannot see that has no way to react to it (finding `failed-canonical-clear-is-silent`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearOutcome {
    /// The canonical authority committed a durable Cleared record. `legacy_swept` is false only
    /// when the post-clear legacy-candidate sweep ([`persistence::cleanup_after_confirmed_clear`])
    /// could not retire every recognized migration candidate — the tenure is still durably
    /// cleared, so a stale candidate is a residue to retry, never a reason to reopen it.
    Durable { legacy_swept: bool },
    /// The canonical commit itself reported durable, but the immediate authority read-back
    /// (`persistence::cleanup_after_confirmed_clear`'s own `load()`) did NOT confirm `Cleared` —
    /// distinct from `Durable { legacy_swept: false }`, which means the authority DID confirm
    /// `Cleared` and only a legacy residue file survived the sweep. This variant exists so the two
    /// failure modes AUTH-09 Finding B conflated cannot be matched as the same thing: nothing here
    /// may treat this as a completed durable sign-out. The account token may still be readable
    /// from the canonical authority on the next boot.
    AuthorityNotConfirmed,
    /// The canonical clear did not durably land (uncertain, failed, or a protection failure). The
    /// account token may still be readable from the canonical authority on the next boot; the
    /// caller must not present this as a completed sign-out.
    NotDurable,
}

/// Map [`persistence::ClearCleanupOutcome`] onto the [`ClearOutcome`] `clear()` reports for a
/// canonical commit that already landed `Durable` — pulled out of `clear()`'s body (behavior
/// unchanged, log lines and all) so the mapping itself can be pinned directly by a unit test
/// rather than only through `clear()`'s end-to-end path, which cannot reach every arm on the
/// host. **`AuthorityNotConfirmed` must never map to `Durable { legacy_swept: false }`** — that is
/// exactly the conflation an earlier finding (AUTH-09 Finding B) existed to prevent:
/// `AuthorityNotConfirmed` means the immediate authority read-back did NOT confirm `Cleared`, so
/// the account token may still be readable from the canonical authority, which is a materially
/// different — and worse — outcome than "cleared, but one legacy residue file survived the
/// sweep".
fn clear_cleanup_outcome(outcome: persistence::ClearCleanupOutcome) -> ClearOutcome {
    match outcome {
        persistence::ClearCleanupOutcome::Confirmed => ClearOutcome::Durable { legacy_swept: true },
        persistence::ClearCleanupOutcome::LegacyRetireFailed => {
            plx_base::eventlog::log(
                "session: canonical clear is durable but a recognized legacy migration \
                 candidate could not be retired — it remains on disk and will be swept \
                 again on the next sign-out or bootstrap",
            );
            ClearOutcome::Durable { legacy_swept: false }
        }
        persistence::ClearCleanupOutcome::AuthorityNotConfirmed => {
            plx_base::eventlog::log(
                "session: canonical clear reported durable but the immediate authority \
                 read-back did not confirm Cleared — the legacy sweep was skipped and the \
                 account token may still be readable from the canonical authority",
            );
            ClearOutcome::AuthorityNotConfirmed
        }
    }
}

/// Clear the persisted session (sign-out) — removes the file; a fresh `client_id` is minted next
/// load. The old-path copy goes too, or the migration fallback would resurrect the stale session.
/// **And commits an explicit Cleared record to the canonical authority** — the legacy-file sweep
/// below only ever touches pre-DB8 candidates; on a build where `persistence::load`/`write_session`
/// actually read/write the canonical store (DB8 or its host/ARM equivalent), that store is a
/// SEPARATE copy of the account token and roster, and clearing only the legacy files would leave a
/// clean-looking sign-out that the canonical authority still hands back on the next boot.
///
/// Takes [`IO`] like every other entry point, and that is not tidiness: a sign-out racing an
/// in-flight worker's read-modify-write would otherwise delete the file and have the worker put it
/// straight back, account token and all.
///
/// This is an ordinary sign-out: the install-wide language survives it. The erase queue calls
/// [`clear_for_erase`] directly, which also serves "Delete all local data" and its retries.
pub fn clear() -> ClearOutcome {
    let report = clear_for_erase(false, None);
    if report.preference_failures.is_empty() { report.outcome } else { ClearOutcome::NotDurable }
}

/// Worker receipt: retaining/resetting a public preference never republishes credentials.
pub struct Erasure {
    pub outcome: ClearOutcome,
    /// What the next launch reads: kept by sign-out; after delete-all, the language is System and
    /// (on the file backend, where the whole retained file goes) the resolution is Automatic.
    pub retained: Retained,
    pub preference_failures: Vec<String>,
}

/// Clear the credentials for a sign-out (`all_local == false`), which retains the install-wide
/// language and Interface resolution, or for "Delete all local data", which then resets the
/// language to System.
///
/// A retry carries the first worker's confirmed choices, so a failed auxiliary write cannot
/// replace them with the defaults after the credentials themselves have already been cleared.
pub fn clear_for_erase(all_local: bool, retry: Option<Retained>) -> Erasure {
    let _io = io();
    let kept = retry.unwrap_or_else(|| Retained::of(&session_from_read(&read_live_locked())));
    let native = cfg!(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))));
    // File backends need a credential-free resource before the credential file disappears.
    let mut preference_failures = Vec::new();
    if !all_local && !native && !install_preferences::save(kept) {
        preference_failures.push("install preferences could not be retained".into());
    }
    // Signing out changes what `peek` answers exactly as durably as a save does — and it must drop
    // the cached `Arc<Session>` immediately rather than merely marking it stale: that `Arc` holds
    // the very account/server tokens sign-out means to get rid of, and leaving it cached would
    // keep it reachable from `peek()` until some unrelated later write happens to overwrite it.
    // See the module doc's cache invariants.
    revoke_cached_session();
    // Persist revocation before any clear can fail or the process can exit midway through it.
    // Best effort: the sweep below still requires every candidate to be retired, marker or not.
    persist_fallback_revocation_locked();

    // A redirected legacy-fixture test (`TempSession`/`redirect_for_test`) must never reach the
    // real canonical authority — exactly the guard `save_locked_with_authority` and
    // `read_live_locked` already carry for the same fixture. Without it, a test that only means to
    // grade the scratch legacy file instead signs this PROCESS'S real canonical store out from
    // under whatever else is reading it (e.g. a `make sim` simulator sharing the same instance
    // root under `make check`), which is silent because the whole suite still passes.
    #[cfg(any(test, feature = "test-support"))]
    let bypass_canonical = TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).is_some();
    #[cfg(not(any(test, feature = "test-support")))]
    let bypass_canonical = false;

    // Commit the canonical Cleared record BEFORE sweeping the legacy files, not after: a
    // canonical Cleared record already outranks any legacy file unconditionally (AUTH-09), so
    // committing it first means the sign-out has already taken effect in the authority `load()`
    // actually reads even if the legacy sweep below then fails partway through. The previous
    // order did the opposite — remove the local copy, then attempt the canonical commit — so a
    // commit that came back non-durable left the account token readable from the canonical
    // authority with the local trace already gone and nothing on disk to show for it.
    //
    // The canonical clear is best-effort in the sense that sign-out must still remove the legacy
    // files below even when it does not durably land — losing the local files on report of a
    // canonical failure would leave BOTH copies of the credentials reachable. But "best-effort"
    // must never mean "silent": anything short of a verified Durable commit is a real
    // security-relevant failure (the account token may still be readable from the canonical
    // authority on next boot), so it is always logged, matching this module's existing `save`-side
    // logging idiom, and it is reported back to the caller as [`ClearOutcome::NotDurable`] rather
    // than discarded.
    let canonical_outcome = if bypass_canonical {
        None
    } else {
        Some(match persistence::commit_cleared() {
        persistence::CanonicalCommit::Durable { .. } => {
            retire_marked_fallbacks_locked();
            // `auth_paths()` above only ever covered `paths::session_candidates()` — the legacy
            // sign-in file and its pre-relocation predecessor. The recognized migration source set
            // is bigger (`paths::session_migration_candidates()`, plus the pre-DB8 canonical JSON
            // wrapper on ARM), and a candidate this sweep never visits is a live account token left
            // on a rooted, world-readable install prefix after a sign-out that otherwise looked
            // clean. `cleanup_after_confirmed_clear` re-reads the authority to confirm it really is
            // Cleared before retiring anything, so this can only ever remove residue, never data a
            // concurrent re-login just wrote.
            #[allow(unused_mut)] // only mutated on the ARM cfg arm below
            let mut outcome = clear_cleanup_outcome(persistence::cleanup_after_confirmed_clear());
            // Telemetry/consent's own legacy files are a SEPARATE candidate set from the session
            // auth-token sweep above (`persistence::cleanup_after_confirmed_clear` never touches
            // them — see `paths::telemetry_candidates` vs `paths::session_migration_candidates`).
            // On ARM, `telemetry::persistence::forget_at` defers their removal to exactly this
            // moment, once this canonical commit is confirmed durable (Copilot review on PR #105,
            // finding 7; ported from `release/v0.6`'s `telemetry::cleanup_after_account_clear`,
            // called here in that release).
            #[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
            if !ACCOUNT_CLEAR_CLEANUP.get().is_none_or(|sweep| sweep()) {
                plx_base::eventlog::log(
                    "session: canonical clear is durable but a telemetry/consent legacy \
                     candidate could not be retired — it remains on disk and will be swept \
                     again on the next sign-out",
                );
                if let ClearOutcome::Durable { legacy_swept } = &mut outcome {
                    *legacy_swept = false;
                }
            }
            outcome
        }
        persistence::CanonicalCommit::Uncertain { stage, errno, helper } => {
            plx_base::eventlog::log(&format!(
                "session: canonical clear is uncertain stage={stage:?} errno={errno} helper={helper:?} — the \
                 account token may still be readable from the canonical authority"
            ));
            ClearOutcome::NotDurable
        }
        persistence::CanonicalCommit::Failed(error) => {
            plx_base::eventlog::log(&format!(
                "session: canonical clear failed: {error:?} — the account token may still be \
                 readable from the canonical authority"
            ));
            ClearOutcome::NotDurable
        }
        persistence::CanonicalCommit::ProtectionFailed(failure) => {
            plx_base::eventlog::log(&format!(
                "session: canonical clear protection failed: {:?}, commit_verified={} — the \
                 account token may still be readable from the canonical authority",
                failure.failure, failure.db8_commit_verified
            ));
            ClearOutcome::NotDurable
        }
        })
    };

    // Every candidate, not just the one we happen to write today: leaving a copy at any other
    // location would let `peek`'s search resurrect the stale session on the next boot. The `.tmp`
    // siblings go too — `peek` cannot read one, so it is not a resurrection risk, but a sign-out
    // that leaves a live account token in a file on a rooted television is not a sign-out.
    let mut legacy_swept = retry_pending_retirements_locked();
    for path in auth_paths() {
        if let Some(bytes) = read_owned_regular(&path) {
            if let Ok(envelope) = serde_json::from_slice::<SecureEnvelope>(&bytes) {
                if envelope.format == SECURE_FORMAT && envelope.version == 1 {
                    plx_platform::tv::secure::remove(&envelope.sealed.backend, &envelope.sealed.key);
                }
            }
        }
        remove_temp_siblings(&path);
        legacy_swept &= retire_session_candidate(&path);
    }

    let mut outcome = canonical_outcome.unwrap_or(ClearOutcome::Durable { legacy_swept: true });
    if let ClearOutcome::Durable { legacy_swept: complete } = &mut outcome {
        *complete &= legacy_swept;
    }
    let mut retained = kept;
    if all_local {
        // Native ClearTenure deliberately retains preferences. Remove only language with a
        // public-only, same-generation CAS after confirmed clearing, never ReplaceAuth.
        let reset = !native || matches!(persistence::reset_cleared_language(),
            persistence::CanonicalCommit::Durable { .. });
        if !reset { preference_failures.push("stored language preference could not be reset".into()); }
        preference_failures.extend(install_preferences::erase());
        if reset && preference_failures.is_empty() {
            retained.language = plx_platform::i18n::Preference::System;
            // The file backend's retained file is gone whole. Native ClearTenure keeps every other
            // public preference, and its Cleared state accepts the removal of `language` alone.
            if !native { retained.ui_resolution = UiResolution::Auto; }
        }
    } else if native && !matches!(outcome, ClearOutcome::Durable { .. })
        && !install_preferences::save(kept) {
        // A failed helper clear can still leave only an explicitly supported file fallback.
        preference_failures.push("fallback install preferences could not be retained".into());
    }
    Erasure { outcome, retained, preference_failures }
}

/// Sibling markers are independent of auth.json: a read-only credential inode/directory can
/// still be revoked through another writable candidate (including the runtime directory).
/// Every fallback read scans ALL markers before choosing a file, and Missing migration also
/// skips marked snapshots while revoked. Only a later successful fresh credential write may
/// remove the markers, after attempting to retire obsolete marked snapshots. Canonical reads and
/// preference writes never remove them. This protects process restarts, not loss of every
/// writable directory: if no marker can be synced we cannot protect a surviving snapshot;
/// the incomplete sweep is logged/reported and remains retryable. A marker stored only in
/// /tmp cannot survive a device reboot that clears /tmp. A crash before the storage worker
/// persists the intent is likewise not covered. This barrier does not override canonical data.
fn fallback_revocation_path(path: &std::path::Path) -> std::path::PathBuf {
    path.with_extension("session-revoked")
}

fn fallback_revoked_at(paths: &[std::path::PathBuf]) -> bool {
    paths.iter().any(|path| match std::fs::symlink_metadata(fallback_revocation_path(path)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        // Even an unreadable/damaged marker fails closed. Its contents are not credentials.
        _ => true,
    })
}

fn persist_fallback_revocation_locked() -> bool {
    // A new sign-out supersedes any earlier credential write's permission to remove markers.
    PENDING_REVOCATION_REMOVALS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    for path in auth_paths() {
        let marker = fallback_revocation_path(&path);
        let result = write_atomic(&marker, b"revoked\n").and_then(|()| {
            // write_atomic swallows parent-sync errors; revocation requires this flush to succeed.
            std::fs::File::open(marker.parent().expect("candidate parent"))
                .and_then(|dir| dir.sync_all())
                .map_err(|error| WriteFailure::WriteFailed(error.raw_os_error().unwrap_or(0)))
        });
        match result {
            Ok(()) => return true,
            Err(failure) => log_candidate_diagnostics("revocation marker refused", &[
                CandidateDiagnostic { parent: parent_stat(&marker), path: marker, failure },
            ]),
        }
    }
    plx_base::eventlog::log("session: no durable fallback revocation marker; sign-out cannot survive a restart until cleanup succeeds");
    false
}

fn marked_fallback(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).ok()
        .is_some_and(|value| value.get(FALLBACK_MARKER).and_then(serde_json::Value::as_u64) == Some(1))
}

/// Best effort after canonical recovery. No cache mutation and no change to the canonical
/// verdict: an unsuccessful retirement is logged, while canonical still serves its proven data.
fn retire_marked_fallbacks_locked() -> bool {
    let mut complete = retry_pending_retirements_locked();
    for path in auth_paths() {
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                plx_base::eventlog::log(&format!("session: fallback retirement stat failed path={} errno={}",
                    path.display(), error.raw_os_error().unwrap_or(0)));
                complete = false;
                continue;
            }
            Ok(meta) if !meta.is_file() => continue,
            Ok(_) => {}
        }
        match read_owned_regular(&path) {
            Some(bytes) if marked_fallback(&bytes) => complete &= retire_session_candidate(&path),
            Some(_) => {}
            None => {
                plx_base::eventlog::log(&format!("session: fallback retirement could not read candidate path={}", path.display()));
                complete = false;
            }
        }
    }
    complete && retry_pending_revocation_removals_locked()
}

/// Retry authorization comes from a proven fresh credential write, never a file mtime (the
/// device clock can jump). IO serializes this set; a newer sign-out invalidates its tenure and
/// cancels it before persisting another marker. Across process restart, markers stay fail-closed
/// until another proven fresh sign-in because this authorization is deliberately process-local.
static PENDING_REVOCATION_REMOVALS: Mutex<Vec<(std::path::PathBuf, u64)>> = Mutex::new(Vec::new());

fn retry_pending_revocation_removals_locked() -> bool {
    let pending = PENDING_REVOCATION_REMOVALS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut complete = true;
    for (marker, tenure) in pending {
        let current = REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire);
        let retired = if tenure != current {
            // Discard obsolete permission without touching the marker for a newer sign-out.
            true
        } else {
            match plx_platform::storage::unlink(&marker) {
                Ok(()) => sync_retired_candidate_parent(&marker),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => sync_retired_candidate_parent(&marker),
                // The same rule as `retire_session_candidate`: absence proven behind a refusal.
                Err(error) => match plx_platform::storage::prove_absent_after_refused_unlink(&marker, error) {
                    Ok(()) => true,
                    Err(error) => {
                        plx_base::eventlog::log(&format!("session: revocation retirement failed errno={}",
                            error.raw_os_error().unwrap_or(0)));
                        false
                    }
                },
            }
        };
        if retired {
            PENDING_REVOCATION_REMOVALS.lock().unwrap_or_else(|e| e.into_inner())
                .retain(|entry| entry != &(marker.clone(), tenure));
        } else {
            complete = false;
        }
    }
    complete
}

fn end_fallback_revocation_locked(s: &Session, authority: SaveAuthority, generation: u64, fallback_written: bool) {
    let tenure = REVOCATION_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    if authority != SaveAuthority::FreshReauthentication || s.account_token.is_empty()
        || CACHE_GENERATION.load(std::sync::atomic::Ordering::Relaxed) != generation { return; }
    for path in auth_paths() {
        let marker = fallback_revocation_path(&path);
        if matches!(std::fs::symlink_metadata(&marker),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound) { continue; }
        let mut pending = PENDING_REVOCATION_REMOVALS.lock().unwrap_or_else(|e| e.into_inner());
        pending.retain(|(path, _)| path != &marker);
        pending.push((marker, tenure));
    }
    if fallback_written {
        // The outage writer already attempted to retire OTHER candidates; keep its new snapshot.
        retry_pending_revocation_removals_locked();
    } else {
        // Canonical sign-in retires all old snapshots before removing the barrier.
        retire_marked_fallbacks_locked();
    }
}

fn canonical_write_completed_locked(s: &Session, authority: SaveAuthority, generation: u64) {
    retire_marked_fallbacks_locked();
    end_fallback_revocation_locked(s, authority, generation, false);
}

/// Sign-out must retire credentials even when a directory refuses unlink but its file is
/// writable. Only an owned regular file with no other hard links may be changed in place;
/// validate the opened fd before truncation, and never follow a symlink or create a new file.
fn neutralize_session_candidate(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
    }
    file.set_len(0)?;
    let written = file.write_all(SESSION_TOMBSTONE);
    // Sync even after a short/failed write: truncation may already have removed credentials.
    let synced = file.sync_all();
    written.and(synced)
}

#[cfg(any(test, feature = "test-support"))]
std::thread_local! {
    static RETIRE_PARENT_SYNC_FOR_TEST: std::cell::Cell<Option<fn() -> Option<i32>>> =
        const { std::cell::Cell::new(None) };
}

fn sync_retirement_directory(dir: &std::fs::File) -> std::io::Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(errno) = RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.get().and_then(|inject| inject())) {
        return Err(std::io::Error::from_raw_os_error(errno));
    }
    dir.sync_all()
}

/// IO serializes retirement and this pending set. Only failed parent flushes are queued, so
/// ordinary canonical reads do not fsync absent candidates. Process-local state is enough for
/// this retry: after restart, a directory entry restored by power loss is visible as a marked
/// file again and is retired on the next authoritative read, rather than silently skipped.
static PENDING_RETIREMENTS: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

fn retry_pending_retirements_locked() -> bool {
    // Do not hold the set's mutex across sync: the helper updates its entry on completion.
    let pending = PENDING_RETIREMENTS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut complete = true;
    for path in pending {
        complete &= sync_retired_candidate_parent(&path);
    }
    complete
}

/// NotFound may be a retry of an unlink whose directory sync failed, not durable absence.
/// A missing parent is already retired; every other open/sync failure remains retryable.
fn sync_retired_candidate_parent(path: &std::path::Path) -> bool {
    let result = path.parent().ok_or_else(|| std::io::Error::from_raw_os_error(libc::EINVAL))
        .and_then(|parent| match std::fs::File::open(parent) {
            Ok(dir) => sync_retirement_directory(&dir),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        });
    if let Err(error) = result {
        let mut pending = PENDING_RETIREMENTS.lock().unwrap_or_else(|e| e.into_inner());
        if !pending.iter().any(|candidate| candidate == path) { pending.push(path.to_path_buf()); }
        drop(pending);
        plx_base::eventlog::log(&format!("session: sign-out unlink sync failed path={} errno={}",
            path.display(), error.raw_os_error().unwrap_or(0)));
        return false;
    }
    PENDING_RETIREMENTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|candidate| candidate != path);
    true
}

/// Shared by both sign-out sweeps. Successful retirement includes a synced tombstone;
/// failures remain visible, especially when no reachable canonical Cleared record protects us.
///
/// Retired means no credential remains at `path`: removed (parent synced), neutralized, or
/// PROVEN absent. A refused unlink is not evidence of presence — Linux answers EROFS from the
/// parent's mount before it looks the child up, which is how every legacy candidate on a mount
/// the jail sees read-only answered on webOS 4.10.2. So a refusal is followed by a no-follow
/// lookup, and a neutralize open (no O_CREAT) answering ENOENT is the same proof. Only a name
/// that exists, or cannot be looked at, stays a failure for the caller to retry.
fn retire_session_candidate(path: &std::path::Path) -> bool {
    let unlink = match plx_platform::storage::unlink(path) {
        Ok(()) => return sync_retired_candidate_parent(path),
        // May be the retry of our own unlink whose parent sync failed, so it syncs too.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return sync_retired_candidate_parent(path),
        Err(error) => error,
    };
    // A refusal changed nothing on disk, so absence proven behind it has nothing to flush.
    let Err(unlink) = plx_platform::storage::prove_absent_after_refused_unlink(path, unlink) else { return true };
    match neutralize_session_candidate(path) {
        Ok(()) => true,
        // Opened without O_CREAT: the name vanished after the lookup above.
        Err(overwrite) if overwrite.kind() == std::io::ErrorKind::NotFound => true,
        Err(overwrite) => {
            plx_base::eventlog::log(&format!(
                "session: sign-out candidate retirement failed path={} unlink_errno={} neutralize_errno={}",
                path.display(), unlink.raw_os_error().unwrap_or(0),
                overwrite.raw_os_error().unwrap_or(0)
            ));
            false
        }
    }
}

/// A v4-ish UUID from `/dev/urandom` (no `uuid` crate). Only uniqueness/stability matter — plex.tv
/// just needs a value it can key the device on.
pub fn new_client_id() -> String {
    client_id_from_entropy(random_bytes())
}

pub fn client_id_from_entropy(mut b: [u8; 16]) -> String {
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
/// Converted: `screens/account_menu.rs`, and — since 2026-08-23 — the shared top bar's profile chip
/// (`ui/widgets.rs` `profile_chip`), which was the remaining half of the bug. Both now word
/// themselves through ONE resolver, `screens::account_menu::chip_label`, so the chip and the menu it
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
    /// sign-in or a "Change profile", and a *failed* fetch at sign-in persists an empty vec
    /// (`auth.rs`'s `finish_sign_in`, which logs the failure's grade), so "never fetched", "fetch
    /// failed" and "genuinely empty" are one value. Anything deciding on it must treat empty as
    /// "ask" — which is why [`Account::can_switch`] keeps the switch row: that row is what
    /// re-fetches the roster, and hiding it on an empty one would be a one-way door out of a Plex
    /// Home created later. When the re-fetch fails too, the picker reads out why and BACK leaves
    /// it (#132); a REFUSAL over nothing cached is then remembered for that identity
    /// (`auth::owner::SessionSnapshot::switch_refused`), and it is `screens::account_menu`, not
    /// this, that hides the row on it.
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
#[path = "session_test_support.rs"]
mod test_support;

#[cfg(test)]
#[path = "session_publication_tests.rs"]
mod publication_tests;

#[cfg(test)]
#[path = "session_compat_tests.rs"]
mod compat_tests;

#[cfg(test)]
#[path = "session_roster_tests.rs"]
mod roster_tests;

#[cfg(test)]
#[path = "session_persistence_tests.rs"]
mod persistence_tests;

#[cfg(test)]
#[path = "session_profile_cache_tests.rs"]
mod profile_cache_tests;

#[cfg(test)]
#[path = "session_cache_tests.rs"]
mod cache_tests;

// Storage-facing capability only. Session owner admission is integrated in Stage B.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
pub enum SaveAuthority { PublicOnly, Routine, FreshReauthentication }

/// Test-only witness of the authority the last [`save_locked_with_authority`] call actually used —
/// what a fixture cannot observe any other way, since the adapter's `LiveWrite` carries the
/// canonical verdict but not which door produced it.
#[cfg(any(test, feature = "test-support"))]
static LAST_WRITE_AUTHORITY: Mutex<Option<SaveAuthority>> = Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
pub fn last_write_authority_for_test() -> Option<SaveAuthority> {
    *LAST_WRITE_AUTHORITY.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_last_write_authority_for_test() {
    *LAST_WRITE_AUTHORITY.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    /// Test-only witness of how many times [`read_live_locked`] actually ran, on THIS thread — the
    /// number [`CACHE`] exists to keep flat across repeated per-frame [`peek`] calls with no
    /// intervening write. Every real read (`peek`, `update`, `clear`'s own re-read, …) funnels
    /// through `read_live_locked`, so this counts the thing a per-frame caller must not cause.
    ///
    /// `thread_local!`, not a process-wide atomic: the host test runner puts every `#[test]` on
    /// its own thread and runs many concurrently, and a test asserting an exact count wants to
    /// know what ITS OWN reads did, not what some unrelated test running in parallel on another
    /// thread also caused — a shared atomic made this counter's answer depend on scheduling.
    static READS_FOR_TEST: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn reads_for_test() -> u32 {
    READS_FOR_TEST.with(|c| c.get())
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_reads_for_test() {
    READS_FOR_TEST.with(|c| c.set(0));
}

#[allow(dead_code)]
pub mod persistence;

#[cfg(test)]
mod migration_tests;

#[cfg(test)]
mod prefs_characterisation_tests;

#[allow(dead_code)] // Stage B connects typed owner admission/completions.
pub mod async_persistence;
mod install_preferences;
pub use install_preferences::Retained;
pub mod prefs;
#[cfg(test)]
mod prefs_tests;
#[cfg(test)]
mod memory_tests;

#[cfg(test)]
mod v0;
#[cfg(test)]
mod tier1_characterisation_tests;

#[cfg(test)]
mod direct_play_mode_tests {
    use super::*;
    #[test]
    fn direct_play_mode_defaults_and_round_trips_through_both_storage_formats() {
        for value in [serde_json::json!({}), serde_json::json!({"direct_play_mode":"future"}), serde_json::json!({"direct_play_mode":17})] {
            let session: Session = serde_json::from_value(value).unwrap();
            assert_eq!(session.direct_play_mode(), DirectPlayMode::Auto);
        }
        for mode in [DirectPlayMode::Auto, DirectPlayMode::Forced, DirectPlayMode::Disabled] {
            let session = Session::default().with_pref::<DirectPlayModePref>(mode);
            let round: Session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
            assert_eq!(round.direct_play_mode(), mode);
            let prefs: CanonicalSessionPreferences = serde_json::from_value(split_public(&session).unwrap().preferences).unwrap();
            assert_eq!(prefs.prefs.direct_play_mode, mode);
        }
    }
}

/// `Session::next_episode_mode`: soft-parse, omit-at-default (so committed replay fixtures and
/// every session written before the field existed serialize unchanged) and round-trip.
#[cfg(test)]
mod next_episode_mode_tests {
    use super::*;

    #[test]
    fn absent_or_malformed_is_countdown() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"next_episode_mode": null}),
            serde_json::json!({"next_episode_mode": "future"}),
            serde_json::json!({"next_episode_mode": 3}),
        ] {
            let session: Session = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("{value}: a bad preference must not fail the session: {e}"));
            assert_eq!(session.next_episode_mode(), NextEpisodeMode::Countdown, "{value}");
        }
    }

    #[test]
    fn the_default_is_not_serialized() {
        let session = Session::default();
        assert_eq!(session.next_episode_mode(), NextEpisodeMode::Countdown);
        assert!(!serde_json::to_string(&session).unwrap().contains("next_episode_mode"));
        let prefs = serde_json::to_string(&split_public(&session).unwrap().preferences).unwrap();
        assert!(!prefs.contains("next_episode_mode"), "{prefs}");
    }

    #[test]
    fn a_pick_round_trips_through_both_formats() {
        for mode in [NextEpisodeMode::AfterCredits, NextEpisodeMode::Off] {
            let session = Session::default().with_pref::<NextEpisodeModePref>(mode);
            let json = serde_json::to_string(&session).unwrap();
            assert!(json.contains("next_episode_mode"), "{json}");
            let round: Session = serde_json::from_str(&json).unwrap();
            assert_eq!(round.next_episode_mode(), mode);

            let (public, protected) = split_canonical(&session).unwrap();
            assert_eq!(join_canonical(&public, &protected).unwrap().next_episode_mode(), mode);
            assert_eq!(public_session(&public).next_episode_mode(), mode);
        }
    }
}

/// `Session::deck_press`: soft-parse, omit-at-default (so committed replay fixtures and every
/// session written before the field existed serialize unchanged) and round-trip.
#[cfg(test)]
mod deck_press_tests {
    use super::*;

    #[test]
    fn absent_or_malformed_is_details() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"deck_press": null}),
            serde_json::json!({"deck_press": "future"}),
            serde_json::json!({"deck_press": 3}),
        ] {
            let session: Session = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("{value}: a bad preference must not fail the session: {e}"));
            assert_eq!(session.deck_press(), DeckPress::Details, "{value}");
        }
    }

    #[test]
    fn the_default_is_not_serialized() {
        let session = Session::default();
        assert_eq!(session.deck_press(), DeckPress::Details);
        assert!(!serde_json::to_string(&session).unwrap().contains("deck_press"));
        let prefs = serde_json::to_string(&split_public(&session).unwrap().preferences).unwrap();
        assert!(!prefs.contains("deck_press"), "{prefs}");
    }

    #[test]
    fn a_pick_round_trips_through_both_formats() {
        let session = Session::default().with_pref::<DeckPressPref>(DeckPress::Play);
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("deck_press"), "{json}");
        let round: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(round.deck_press(), DeckPress::Play);

        let (public, protected) = split_canonical(&session).unwrap();
        assert_eq!(join_canonical(&public, &protected).unwrap().deck_press(), DeckPress::Play);
        assert_eq!(public_session(&public).deck_press(), DeckPress::Play);
    }

    #[test]
    fn only_play_presses_play() {
        assert!(!DeckPress::Details.press_plays());
        assert!(DeckPress::Play.press_plays());
        for mode in DeckPress::LADDER {
            assert_eq!(DeckPress::from_index(mode.index()), mode);
        }
    }
}

/// `Session::ui_resolution`: absent or unknown is Automatic, the default is not serialized (so
/// committed replay fixtures and every session written before the field serialize unchanged),
/// a pick round-trips through both formats, and the option maps to the window it asks for.
#[cfg(test)]
mod ui_resolution_tests {
    use super::*;

    #[test]
    fn absent_or_unknown_is_automatic() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"ui_resolution": null}),
            serde_json::json!({"ui_resolution": "8k"}),
            serde_json::json!({"ui_resolution": "576p"}),
            serde_json::json!({"ui_resolution": 720}),
        ] {
            let session: Session = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("{value}: a bad preference must not fail the session: {e}"));
            assert_eq!(session.ui_resolution(), UiResolution::Auto, "{value}");
        }
    }

    #[test]
    fn the_default_is_not_serialized() {
        let session = Session::default();
        assert_eq!(session.ui_resolution(), UiResolution::Auto);
        assert!(!serde_json::to_string(&session).unwrap().contains("ui_resolution"));
        let prefs = serde_json::to_string(&split_public(&session).unwrap().preferences).unwrap();
        assert!(!prefs.contains("ui_resolution"), "{prefs}");
    }

    #[test]
    fn a_pick_round_trips_through_both_formats() {
        let session = Session::default().with_ui_resolution(UiResolution::Hd720);
        let json = serde_json::to_string(&session).unwrap();
        assert!(json.contains("\"ui_resolution\":\"720p\""), "{json}");
        let round: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(round.ui_resolution(), UiResolution::Hd720);
        let (public, protected) = split_canonical(&session).unwrap();
        assert_eq!(join_canonical(&public, &protected).unwrap().ui_resolution(), UiResolution::Hd720);
        assert_eq!(public_session(&public).ui_resolution(), UiResolution::Hd720);
    }

    #[test]
    fn the_ladder_lists_the_options_and_automatic_is_the_native_canvas() {
        assert_eq!(UiResolution::Auto.size(), None, "Automatic asks for no smaller window on any set");
        assert_eq!(UiResolution::Hd900.size(), Some((1600, 900)));
        assert_eq!(UiResolution::Hd720.size(), Some((1280, 720)));
        assert_eq!(UiResolution::Sd540.size(), Some((960, 540)));
        assert_eq!(UiResolution::LADDER, [UiResolution::Auto, UiResolution::Hd900, UiResolution::Hd720, UiResolution::Sd540]);
        for mode in UiResolution::LADDER {
            assert_eq!(UiResolution::from_index(mode.index()), mode);
            assert_eq!(serde_json::to_value(mode).unwrap(), serde_json::json!(mode.tag()), "tag is the serialized spelling");
        }
        assert_eq!(UiResolution::from_index(UiResolution::LADDER.len()), UiResolution::Auto);
    }

    /// An earlier build offered an explicit 1080p, which was the native canvas spelled out. It is no
    /// option now, and a stored one reads as Automatic (which is the same window).
    #[test]
    fn a_stored_1080p_is_automatic() {
        let session: Session = serde_json::from_value(serde_json::json!({"ui_resolution": "1080p"})).unwrap();
        assert_eq!(session.ui_resolution(), UiResolution::Auto);
        let (public, protected) = split_canonical(&Session::default()).unwrap();
        let mut public = serde_json::to_value(&public).unwrap();
        public["preferences"]["ui_resolution"] = serde_json::json!("1080p");
        let public: plx_platform::storage::state::PublicPayload = serde_json::from_value(public).unwrap();
        assert_eq!(join_canonical(&public, &protected).unwrap().ui_resolution(), UiResolution::Auto);
    }

    /// A signed-out read (Cleared or Locked) carries the resolution exactly as it carries the
    /// language, in the cached session `peek` serves and in the owned one a writer starts from.
    #[test]
    fn a_signed_out_read_carries_the_resolution_like_the_language() {
        let kept = Retained { language: plx_platform::i18n::Preference::Be, ui_resolution: UiResolution::Sd540 };
        for read in [ReadState::Cleared { kept }, ReadState::Locked { kept }] {
            for session in [session_from_read(&read), (*session_of(&read)).clone()] {
                assert_eq!(session.language, plx_platform::i18n::Preference::Be);
                assert_eq!(session.ui_resolution(), UiResolution::Sd540);
                assert!(session.account_token.is_empty());
            }
        }
        let other = Retained { ui_resolution: UiResolution::Hd720, ..kept };
        assert_ne!(read_identity(&ReadState::Cleared { kept }), read_identity(&ReadState::Cleared { kept: other }),
            "a change of resolution is a change of identity for a deferred load");
    }

    /// The retained file holds the resolution beside the language, a spelling this build does not
    /// know (or the retired 1080p) costs the resolution and never the language, and Automatic is not written.
    #[test]
    fn the_retained_file_is_soft_about_the_resolution() {
        let _serial = plx_base::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plx-retained-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        redirect_candidates_for_test(Some(vec![dir.join("auth.json")]));
        let path = dir.join("auth.language.json");
        for (body, language, resolution) in [
            (r#"{"version":1,"language":"be","ui_resolution":"8k"}"#, plx_platform::i18n::Preference::Be, UiResolution::Auto),
            (r#"{"version":1,"language":"be","ui_resolution":"1080p"}"#, plx_platform::i18n::Preference::Be, UiResolution::Auto),
            (r#"{"version":1,"language":"be"}"#, plx_platform::i18n::Preference::Be, UiResolution::Auto),
            (r#"{"version":1,"language":"be","ui_resolution":"720p"}"#, plx_platform::i18n::Preference::Be, UiResolution::Hd720),
        ] {
            std::fs::write(&path, body).unwrap();
            let loaded = install_preferences::load();
            assert_eq!(loaded, Some(Retained { language, ui_resolution: resolution }), "{body}");
        }
        assert!(install_preferences::save(Retained { language: plx_platform::i18n::Preference::Be, ui_resolution: UiResolution::Auto }));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("ui_resolution"), "Automatic is not written");
        redirect_candidates_for_test(None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// `Session::skip_interval`: soft-parse, omit-at-default (so committed replay fixtures and every
/// session written before the field existed serialize unchanged) and round-trip.
#[cfg(test)]
mod skip_interval_tests {
    use super::*;

    #[test]
    fn absent_or_malformed_is_ten_seconds() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"skip_interval": null}),
            serde_json::json!({"skip_interval": "future"}),
            serde_json::json!({"skip_interval": 30}),
        ] {
            let session: Session = serde_json::from_value(value.clone())
                .unwrap_or_else(|e| panic!("{value}: a bad preference must not fail the session: {e}"));
            assert_eq!(session.skip_interval(), SkipInterval::Seconds10, "{value}");
        }
    }

    #[test]
    fn the_default_is_not_serialized() {
        let session = Session::default();
        assert_eq!(session.skip_interval(), SkipInterval::Seconds10);
        assert!(!serde_json::to_string(&session).unwrap().contains("skip_interval"));
        let prefs = serde_json::to_string(&split_public(&session).unwrap().preferences).unwrap();
        assert!(!prefs.contains("skip_interval"), "{prefs}");
    }

    #[test]
    fn a_pick_round_trips_through_both_formats() {
        for interval in SkipInterval::LADDER.into_iter().filter(|&i| i != SkipInterval::default()) {
            let session = Session::default().with_pref::<SkipIntervalPref>(interval);
            let json = serde_json::to_string(&session).unwrap();
            assert!(json.contains("skip_interval"), "{json}");
            let round: Session = serde_json::from_str(&json).unwrap();
            assert_eq!(round.skip_interval(), interval);

            let (public, protected) = split_canonical(&session).unwrap();
            assert_eq!(join_canonical(&public, &protected).unwrap().skip_interval(), interval);
            assert_eq!(public_session(&public).skip_interval(), interval);
        }
    }

    #[test]
    fn the_ladder_is_ascending_and_indexes_back_to_itself() {
        let seconds: Vec<i64> = SkipInterval::LADDER.iter().map(|i| i.seconds()).collect();
        assert_eq!(seconds, [5, 10, 15, 30, 60]);
        for interval in SkipInterval::LADDER {
            assert_eq!(SkipInterval::from_index(interval.index()), interval);
        }
        assert_eq!(SkipInterval::from_index(200), SkipInterval::Seconds10);
        assert_eq!(SkipInterval::Seconds10.ns(), 10_000_000_000);
    }
}

/// Issue #266 (PR1): `Session::audio_enhancements` — the persisted Plex Pass DSP preference.
/// This PR never sets a toggle on from any production code path; these tests establish the
/// field's own contract in isolation (soft-parse, omit-when-NONE, round-trip) so a later PR's
/// offering policy has a settled place to write into.
#[cfg(test)]
mod audio_enhancements_tests {
    use super::*;

    #[test]
    fn audio_enhancements_absent_key_is_none() {
        let session: Session = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(session.audio_enhancements(), crate::plex::AudioEnhancements::NONE);
    }

    #[test]
    fn audio_enhancements_garbage_is_none() {
        for value in [
            serde_json::json!({"audio_enhancements": null}),
            serde_json::json!({"audio_enhancements": "future"}),
            serde_json::json!({"audio_enhancements": 17}),
            serde_json::json!({"audio_enhancements": {"boost_dialog": "yes"}}),
            serde_json::json!({"audio_enhancements": []}),
        ] {
            let session: Session = serde_json::from_value(value.clone()).unwrap_or_else(|e| {
                panic!("{value}: a malformed audio_enhancements value must not fail the whole session: {e}")
            });
            assert_eq!(
                session.audio_enhancements(),
                crate::plex::AudioEnhancements::NONE,
                "{value}"
            );
        }
    }

    #[test]
    fn audio_enhancements_none_not_serialized() {
        let session = Session::default();
        assert_eq!(session.audio_enhancements(), crate::plex::AudioEnhancements::NONE);
        let json = serde_json::to_string(&session).unwrap();
        assert!(
            !json.contains("audio_enhancements"),
            "a NONE preference must stay omitted, exactly like every other preference in this \
             struct, so a session written before this field existed serializes unchanged: {json}"
        );
        let prefs_json =
            serde_json::to_string(&split_public(&session).unwrap().preferences).unwrap();
        assert!(
            !prefs_json.contains("audio_enhancements"),
            "the canonical public preferences payload must omit it too: {prefs_json}"
        );
    }

    #[test]
    fn audio_enhancements_round_trip() {
        for enh in [
            crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false },
            crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true },
            crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: true },
        ] {
            let session = Session::default().with_pref::<AudioEnhancementsPref>(enh);
            assert_eq!(session.audio_enhancements(), enh);

            // Legacy fallback format: the whole `Session` serialized directly.
            let round: Session =
                serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
            assert_eq!(round.audio_enhancements(), enh);
            let json = serde_json::to_string(&session).unwrap();
            assert!(json.contains("audio_enhancements"), "{json}");

            // Canonical split/join format: the public-preferences half.
            let (public, protected) = split_canonical(&session).unwrap();
            let joined = join_canonical(&public, &protected).unwrap();
            assert_eq!(joined.audio_enhancements(), enh);
        }
    }

    /// Real fixture-shaped session credentials predate #266 (`committed_credentials` in every
    /// `tests/fixtures/replay/*/manifest.json` carries exactly the auth-half fields
    /// `CanonicalSessionAuth` expects: `account_token`, `client_id`, `home_users`, `server`,
    /// `sources`, `user`). Building a `Session` from one and re-serializing it must never grow
    /// an `audio_enhancements` key — the whole point of `skip_serializing_if` — so replaying a
    /// fixture recorded before this PR stays byte-stable rather than drifting the moment this
    /// field is read back in.
    #[test]
    fn replay_manifest_session_is_byte_stable() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        // `plex/` is one level below `rust-modules/`, which is one below the repository root.
        let repo = manifest_dir
            .parent()
            .and_then(std::path::Path::parent)
            .expect("rust-modules/plex has a repository root two levels up");
        let replay_dir = repo.join("tests/fixtures/replay");
        let entries = std::fs::read_dir(&replay_dir)
            .unwrap_or_else(|e| panic!("{}: {e}", replay_dir.display()));
        let mut checked = 0;
        for entry in entries {
            let entry = entry.expect("readable fixture directory entry");
            let manifest_path = entry.path().join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let bytes = std::fs::read(&manifest_path)
                .unwrap_or_else(|e| panic!("{}: {e}", manifest_path.display()));
            let manifest: serde_json::Value = serde_json::from_slice(&bytes)
                .unwrap_or_else(|e| panic!("{}: {e}", manifest_path.display()));
            let Some(committed) = manifest.pointer("/init/data/session/committed_credentials")
            else {
                continue;
            };
            let session: Session = serde_json::from_value(committed.clone())
                .unwrap_or_else(|e| panic!("{}: {e}", manifest_path.display()));
            assert_eq!(
                session.audio_enhancements(),
                crate::plex::AudioEnhancements::NONE,
                "{}: a fixture predating #266 must default both toggles off",
                manifest_path.display()
            );
            let reserialized = serde_json::to_string(&session)
                .unwrap_or_else(|e| panic!("{}: {e}", manifest_path.display()));
            assert!(
                !reserialized.contains("audio_enhancements"),
                "{}: NONE must stay omitted, not just default-valued",
                manifest_path.display()
            );
            checked += 1;
        }
        assert!(checked > 0, "expected at least one replay fixture manifest to exercise");
    }
}

/// Issue #380: `Preferences::server_key_pins` — each server's remembered leaf public key. The reader
/// is [`project_server_keys`] (issue #378); these pin the storage contract it relies on.
#[cfg(test)]
mod server_key_pin_tests {
    use super::*;

    fn pin(seed: u8) -> String {
        plx_base::spki::pin_from_spki_der(&[seed; 8])
    }

    fn stored(machine: &str, pin: &str) -> Session {
        Session::default().with_server_key_pin(machine, pin).expect("a new entry")
    }

    /// `replace_cache` projects on every session read and write. A LIVE session that holds no
    /// learned key and no stored resolve pin looks empty to the projection, but it is not a
    /// sign-out: what `net::keypin` published about a bound host must survive it. A sign-out, a
    /// cleared record and a local revocation end it.
    #[test]
    fn only_a_sign_out_ends_what_keypin_published_not_a_live_session_with_no_key() {
        use plx_net::net::keypin;
        let _serial = plx_base::testlock::serial();
        let key = keypin::key_of("session-signed-in.invalid", 32400);
        let _scoped = keypin::Scoped::watch_machine("m-session-live", &key);
        let language = plx_platform::i18n::Preference::En;
        let live = || Cached::Settled(std::sync::Arc::new(ReadState::Ready {
            session: std::sync::Arc::new(Session::default()), plaintext: false, retry_canonical: false,
        }));
        for (what, ended) in [
            ("a cleared record", Cached::Settled(std::sync::Arc::new(ReadState::Cleared { kept: install_preferences::Retained { language, ..Default::default() } }))),
            ("a missing record", Cached::Settled(std::sync::Arc::new(ReadState::Missing))),
            ("a local revocation", Cached::Revoked),
        ] {
            keypin::strict_failure(&key, 60, Some(10));
            assert!(replace_cache(live(), None, true));
            assert_eq!(keypin::blocked_for("m-session-live"), Some(keypin::Blocked::NoKey),
                "a live session with no key is not a sign-out ({what})");
            assert!(replace_cache(ended, None, true));
            assert_eq!(keypin::blocked_for("m-session-live"), None, "{what} is a sign-out");
        }
        replace_cache(Cached::Unloaded, None, true);
    }

    #[test]
    fn an_absent_key_is_no_pins_and_an_empty_list_is_not_written() {
        let old: Session = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(old.prefs.server_key_pins.is_empty());
        assert_eq!(old.server_key_pin("m"), None);
        let session = Session::default();
        assert!(!serde_json::to_string(&session).unwrap().contains("server_key_pins"));
        let prefs = serde_json::to_string(&split_public(&session).unwrap().preferences).unwrap();
        assert!(!prefs.contains("server_key_pins"), "{prefs}");
    }

    #[test]
    fn a_pin_round_trips_through_both_storage_formats() {
        let session = stored("m1", &pin(1)).with_server_key_pin("m2", &pin(2)).unwrap();
        // Legacy: the whole `Session` serialized directly.
        let round: Session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(round.prefs.server_key_pins, session.prefs.server_key_pins);
        assert_eq!(round.server_key_pin("m1"), Some(pin(1).as_str()));
        // Canonical split/join, and the locked-bundle public snapshot.
        let (public, protected) = split_canonical(&session).unwrap();
        let joined = join_canonical(&public, &protected).unwrap();
        assert_eq!(joined.prefs.server_key_pins, session.prefs.server_key_pins);
        assert_eq!(public_session(&public).prefs.server_key_pins, session.prefs.server_key_pins);
    }

    #[test]
    fn a_damaged_entry_costs_only_that_entry() {
        let good = pin(3);
        let session: Session = serde_json::from_value(serde_json::json!({
            "server_key_pins": [
                {"machine_id": "m1", "pin": good},
                {"machine_id": 7, "pin": "x"},
                "garbage",
                {"machine_id": "m2"},
                {"machine_id": "m3", "pin": good},
            ]
        }))
        .unwrap();
        let machines: Vec<&str> = session.prefs.server_key_pins.iter().map(|k| k.machine_id.as_str()).collect();
        assert_eq!(machines, ["m1", "m3"]);
        let not_a_list: Session =
            serde_json::from_value(serde_json::json!({"server_key_pins": {"m1": "x"}})).unwrap();
        assert!(not_a_list.prefs.server_key_pins.is_empty());
    }

    #[test]
    fn a_stored_string_that_is_not_a_pin_is_never_handed_on() {
        for bad in ["", "sha256//", "sha256//short=", "md5//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA;", "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="] {
            let session: Session = serde_json::from_value(
                serde_json::json!({"server_key_pins": [{"machine_id": "m", "pin": bad}]}),
            )
            .unwrap();
            assert_eq!(session.server_key_pin("m"), None, "{bad:?}");
            assert!(Session::default().with_server_key_pin("m", bad).is_none(), "{bad:?}");
        }
        assert!(Session::default().with_server_key_pin("", &pin(1)).is_none());
    }

    #[test]
    fn one_entry_per_machine_a_different_key_replaces_and_the_same_key_changes_nothing() {
        let one = stored("m", &pin(1));
        assert!(one.with_server_key_pin("m", &pin(1)).is_none(), "same pin: nothing to write");
        let replaced = one.with_server_key_pin("m", &pin(2)).unwrap();
        assert_eq!(replaced.prefs.server_key_pins.len(), 1);
        assert_eq!(replaced.server_key_pin("m"), Some(pin(2).as_str()));
        let two = replaced.with_server_key_pin("n", &pin(3)).unwrap();
        assert_eq!(two.server_key_pin("m"), Some(pin(2).as_str()), "another machine's entry is left alone");
        assert_eq!(two.server_key_pin("n"), Some(pin(3).as_str()));
    }

    /// The probe runs often: learning queues nothing once the stored pin matches, and the legacy
    /// file store's sign-out takes the entry with the file. (`TempSession` bypasses the canonical
    /// store, so this does NOT cover the native one; `migration_tests::
    /// helper_signout_forgets_the_learned_server_keys_and_the_next_account_inherits_none` does.)
    #[test]
    fn learning_writes_a_change_once_and_the_file_sign_out_forgets_it() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("server-key-learn");
        save(&test_support::signed_in());
        assert!(learn_server_key("m", &pin(1)), "a new pin is queued");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(peek().server_key_pin("m"), Some(pin(1).as_str()));
        assert!(!learn_server_key("m", &pin(1)), "the same pin is not");
        assert!(!learn_server_key("", &pin(1)));
        assert!(learn_server_key("m", &pin(2)), "a different one replaces it");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(peek().server_key_pin("m"), Some(pin(2).as_str()));
        assert_eq!(peek().account_token, "acct", "the credentials are untouched");

        clear();
        assert!(peek().prefs.server_key_pins.is_empty(), "cleared with the credentials");
    }

    /// `plx_ui::hold_hint` hands the application what an install was taught: the learn door queues a
    /// write only for news, never un-teaches, and writes through the same ticketed door every
    /// install-wide preference uses.
    #[test]
    fn learning_a_taught_hold_hint_writes_once_and_never_unlearns() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("hold-hint-learn");
        save(&test_support::signed_in());
        let card = HoldHintTaught { card: true, pill: false };
        assert!(learn_hold_hint_taught(card), "news is queued");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(peek().hold_hint_taught(), card);
        assert!(!learn_hold_hint_taught(card), "the same lesson is not");
        assert!(!learn_hold_hint_taught(HoldHintTaught::NONE), "nothing taught is not news");
        assert!(learn_hold_hint_taught(HoldHintTaught { card: false, pill: true }), "another surface is");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(peek().hold_hint_taught(), HoldHintTaught { card: true, pill: true }, "joined, never replaced");
        assert_eq!(peek().account_token, "acct", "the credentials are untouched");
    }

    // ---- Issue #378: the session → `net::keypin` projection. Each test uses its own machine id and
    // its own port, so the process-wide key table never carries one test's entry into another's;
    // all hold `testlock::serial()` (the session cache is a crate global too). ----

    const HASH: &str = "0123456789abcdef0123456789abcdef";

    fn plex_direct_origin(port: i32) -> String {
        format!("https://127-0-0-1.{HASH}.plex.direct:{port}")
    }

    fn table_key(port: i32) -> String {
        plx_net::net::keypin::key_of(&format!("127-0-0-1.{HASH}.plex.direct"), port)
    }

    /// A signed-in session whose primary server is the `plex.direct` origin of `machine` and which
    /// remembers `pin` for it.
    fn remembering(machine: &str, port: i32, pin: &str) -> Session {
        let mut session = test_support::signed_in();
        session.server = ServerRef {
            machine_id: machine.into(),
            address: "127.0.0.1".into(),
            port: i64::from(port),
            origin_url: plex_direct_origin(port),
            ..Default::default()
        };
        session.with_server_key_pin(machine, pin).expect("a new entry")
    }

    /// Restoring a stored session fills the table WITHOUT any registration having happened (the
    /// offline boot: the stored primary is what knows the host), and sign-out empties it — the
    /// table and the latch that stood on it.
    #[test]
    fn a_restored_session_fills_the_key_table_and_sign_out_empties_it() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("keys-restore");
        let (machine, port) = ("m-keys-restore", 41_001);
        let key = table_key(port);
        let _scoped = plx_net::net::keypin::Scoped::new(key.clone(), &pin(9));
        plx_net::net::keypin::forget_for_test(&key);
        save(&remembering(machine, port, &pin(1)));
        let _ = load();
        plx_base::storage_worker::drain_for_test();
        let _ = peek();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), Some(pin(1)), "stored server + stored key");

        plx_net::net::keypin::key_established(&key, &pin(1), Some(10));
        assert!(plx_net::net::keypin::is_latched(&key));
        revoke_cached_session();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), None, "sign-out empties the table");
        assert!(!plx_net::net::keypin::is_latched(&key), "…and ends key mode");
    }

    /// A new or changed key reaches the table when the write `learn_server_key` queued is applied
    /// (the session projection is the only production source of a key), and a changed key ends key
    /// mode for the host.
    #[test]
    fn learning_a_new_or_changed_key_updates_the_table_and_clears_the_latch() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("keys-learn");
        let (machine, port) = ("m-keys-learn", 41_002);
        let key = table_key(port);
        let _scoped = plx_net::net::keypin::Scoped::new(key.clone(), &pin(9));
        plx_net::net::keypin::forget_for_test(&key);
        let mut stored = remembering(machine, port, &pin(1));
        stored.account_token = "acct".into();
        save(&stored);
        let _ = load();
        plx_base::storage_worker::drain_for_test();
        let _ = peek();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), Some(pin(1)));

        plx_net::net::keypin::key_established(&key, &pin(1), Some(10));
        assert!(learn_server_key(machine, &pin(2)), "a different key is queued");
        plx_base::storage_worker::drain_for_test();
        let _ = peek();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), Some(pin(2)), "the applied write projects it");
        assert!(!plx_net::net::keypin::is_latched(&key), "a pin change ends key mode for the host");
    }

    /// A probe that finishes after sign-out must not put its key back into the table sign-out just
    /// emptied: nothing but the session projection writes the table, and a signed-out session
    /// projects nothing.
    #[test]
    fn a_learn_that_completes_after_sign_out_leaves_the_table_empty() {
        let _serial = plx_base::testlock::serial();
        let _session = test_support::TempSession::new("keys-learn-signout");
        let (machine, port) = ("m-keys-learn-out", 41_003);
        let key = table_key(port);
        let _scoped = plx_net::net::keypin::Scoped::new(key.clone(), &pin(9));
        plx_net::net::keypin::forget_for_test(&key);
        let mut stored = remembering(machine, port, &pin(1));
        stored.account_token = "acct".into();
        save(&stored);
        let _ = load();
        plx_base::storage_worker::drain_for_test();
        let _ = peek();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), Some(pin(1)));

        revoke_cached_session();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), None, "sign-out empties the table");
        let _ = learn_server_key(machine, &pin(2));
        plx_base::storage_worker::drain_for_test();
        let _ = peek();
        assert_eq!(plx_net::net::keypin::pin_for_test(&key), None, "the late probe did not repopulate it");
    }
}
