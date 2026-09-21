//! The session FILE half: save/peek/update against a real file — atomicity, secure-envelope
//! preservation, concurrent read-modify-write, and torn-write safety.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::TempSession;

#[test]
fn recording_capture_fresh_identity_has_no_persistence_before_attachment() {
    let _serial = crate::testlock::serial();
    let root = TempSession::new("capture-fresh");
    let (saved, entropy, deferred) = load_capturing_entropy();
    assert!(!saved.client_id.is_empty());
    assert!(entropy.is_some());
    assert!(!root.file().exists(), "capturing inputs must not persist before recorder attachment");
    deferred.apply().unwrap();
    assert!(root.file().exists(), "normal fresh persistence executes after attachment");
}

#[test]
fn recording_capture_plaintext_does_not_migrate_before_attachment() {
    use std::os::unix::fs::MetadataExt;
    let _serial = crate::testlock::serial();
    let root = TempSession::new("capture-plaintext");
    let before = serde_json::to_vec(&signed_in()).unwrap();
    std::fs::write(root.file(), &before).unwrap();
    let inode = std::fs::metadata(root.file()).unwrap().ino();
    let (saved, entropy, deferred) = load_capturing_entropy();
    assert!(!saved.client_id.is_empty());
    assert!(entropy.is_none());
    assert!(std::fs::read(root.file()).unwrap() == before, "capture must leave plaintext bytes unchanged");
    assert_eq!(std::fs::metadata(root.file()).unwrap().ino(), inode);
    deferred.apply().unwrap();
    assert_ne!(std::fs::metadata(root.file()).unwrap().ino(), inode, "normal atomic migration runs afterwards");
}

#[test]
fn deferred_capture_never_overwrites_a_newer_session() {
    let _serial = crate::testlock::serial();
    let root = TempSession::new("capture-superseded");
    let (_, _, deferred) = load_capturing_entropy();
    save(&signed_in());
    let before = std::fs::read(root.file()).unwrap();
    assert!(deferred.apply().is_err());
    assert!(std::fs::read(root.file()).unwrap() == before);
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

// ---- The runtime-dir fallback (2026-09-20 field report) --------------------------------------
//
// An unrooted webOS 4.4.3 set (Dev Mode, no ssh) signed in successfully and then logged
// `session: could not persist to ANY candidate path`: none of `/media/developer`,
// `/media/internal` or the app dir accepted the write on that jail. The one path known to be
// writable on that exact television was the runtime root under `/tmp` (the event log was
// reaching it). `paths::session_candidates()` now offers `in_runtime_dir("auth.json")` as the
// LAST candidate on a device install for exactly this jail. These exercise the real
// `save_legacy_fallback_locked`/`read_legacy_locked` loops `auth_paths()` feeds in production —
// not a hand-rolled stand-in for them — against a candidate list shaped exactly like
// `session_candidates()`'s new order, via `TEST_CANDIDATES` (distinct from `TempSession`'s single
// `TEST_FILE`, which cannot represent "several candidates, some unwritable").
//
// Sets its own candidates rather than going through `TempSession`/`redirect_for_test`, so
// `TEST_FILE` stays `None` throughout — RAII takes `TEST_CANDIDATES` back to `None` on drop,
// exactly as `TempSession` does for `TEST_FILE`.
struct TempCandidates {
    base: std::path::PathBuf,
}

impl TempCandidates {
    /// Two "durable" candidates (standing in for `/media/developer`, `/media/internal`), chmod'd
    /// unwritable — and one "runtime" candidate, world-writable + sticky exactly like the real
    /// `/tmp` this fallback resolves to on device (see `paths::ensure_runtime_dir`).
    fn new(tag: &str) -> (TempCandidates, Vec<std::path::PathBuf>) {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!(
            "plxnative-runtime-fallback-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let durable_a = base.join("media-developer");
        let durable_b = base.join("media-internal");
        let runtime = base.join("tmp-runtime");
        for d in [&durable_a, &durable_b, &runtime] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::set_permissions(&durable_a, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(&durable_b, std::fs::Permissions::from_mode(0o500)).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o1777)).unwrap();
        let candidates = vec![
            durable_a.join("id-auth.json"),
            durable_b.join(".id-auth.json"),
            runtime.join("auth.json"),
        ];
        (TempCandidates { base }, candidates)
    }
}

impl Drop for TempCandidates {
    fn drop(&mut self) {
        redirect_candidates_for_test(None);
        // The two "durable" dirs are unwritable (no entries were ever created inside them), so
        // removing the writable `base` they sit under does not need their own mode restored.
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// RED before the fix: with `in_runtime_dir("auth.json")` absent from `session_candidates()` on a
/// device install, this exact jail shape (every durable candidate refuses the write) had nothing
/// left to try — `save_legacy_fallback_locked` returned `None` and the field report's own log
/// line fired. Watched failing here first, against the real function, before asserting the fixed
/// behaviour below.
#[test]
fn every_durable_candidate_unwritable_and_no_fallback_offered_fails_the_save() {
    let _g = crate::testlock::serial();
    let (_guard, candidates) = TempCandidates::new("watch-fail");
    let durable_only = candidates[..2].to_vec();
    redirect_candidates_for_test(Some(durable_only));

    let outcome = save_legacy_fallback_locked(&signed_in(), false, false);
    assert!(
        outcome.is_none(),
        "setup/regression: every durable candidate must refuse the write, matching the field \
         report, when no fallback candidate is offered"
    );
}

/// The fix: with the runtime-dir candidate offered LAST, the same unwritable-durable jail now
/// persists the session — at mode 0600 — and the session reloads from disk across a simulated
/// process restart (no in-memory state carried over; `read_legacy_locked` re-resolves
/// `auth_paths()` and re-reads from disk exactly as a fresh boot would).
#[test]
fn runtime_dir_candidate_persists_and_reloads_when_every_durable_candidate_is_unwritable() {
    use std::os::unix::fs::PermissionsExt;
    let _g = crate::testlock::serial();
    let (_guard, candidates) = TempCandidates::new("persist-reload");
    redirect_candidates_for_test(Some(candidates.clone()));

    let session = signed_in();
    let outcome = save_legacy_fallback_locked(&session, false, false);
    assert!(
        outcome.is_some(),
        "the runtime-dir candidate must accept the write when every durable one refuses"
    );
    assert!(
        candidates[2].exists(),
        "the session must land on the runtime-dir candidate"
    );
    assert!(
        !candidates[0].exists() && !candidates[1].exists(),
        "the unwritable durable candidates must stay untouched"
    );

    let mode = std::fs::metadata(&candidates[2]).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "credentials at rest, even on the fallback candidate");

    match read_legacy_locked() {
        ReadState::Ready { session: reloaded, plaintext } => {
            assert_eq!(reloaded.client_id, session.client_id);
            assert_eq!(reloaded.account_token, session.account_token);
            assert!(plaintext, "no key manager on host, so this must read back plaintext");
        }
        _ => panic!(
            "the session did not reload from the runtime-dir candidate across a simulated \
             process restart"
        ),
    }
}

/// The generic write path (0600, `O_NOFOLLOW`, the pre-existing-file owner check) already applies
/// to whatever path it is handed — this pins that it also holds for the runtime-dir candidate
/// specifically, since that candidate sits under `/tmp`: a host bind mount shared across jails,
/// world-writable, where another uid can plant an entry ahead of the app.
#[test]
fn write_atomic_refuses_a_symlink_planted_at_the_runtime_dir_candidate() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let _g = crate::testlock::serial();
    let dir = std::env::temp_dir().join(format!(
        "plxnative-runtime-symlink-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // World-writable + sticky, exactly like the real runtime root (`paths::ensure_runtime_dir`).
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777)).unwrap();

    let victim = dir.join("victim.json");
    std::fs::write(&victim, b"not credentials").unwrap();
    let target = dir.join("auth.json");
    symlink(&victim, &target).unwrap();

    let result = write_atomic(&target, br#"{"account_token":"leak"}"#);
    assert!(
        result.is_err(),
        "write_atomic must refuse to write through a pre-existing symlink at the candidate path"
    );
    assert_eq!(
        std::fs::read(&victim).unwrap(),
        b"not credentials",
        "the symlink's target must be left untouched"
    );
    assert!(
        std::fs::symlink_metadata(&target).unwrap().file_type().is_symlink(),
        "the symlink itself must be left in place, not replaced"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `write_atomic` used to collapse every failure to a bare `false`, which is what left the field
/// log saying only "could not persist to ANY candidate path" — unable to tell EACCES from EROFS
/// from ENOENT. It now returns the [`WriteFailure`] the OS actually gave, and
/// `write_atomic_diagnosed` pairs it with the path and the parent directory's stat, which is what
/// a candidate's [`CandidateDiagnostic`] carries into the field log.
#[test]
fn write_atomic_reports_the_errno_and_parent_stat_per_candidate() {
    use std::os::unix::fs::MetadataExt;
    let _g = crate::testlock::serial();
    let base = std::env::temp_dir().join(format!("plxnative-write-atomic-diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();

    // ENOENT: the parent directory this candidate names does not exist at all — the create
    // itself is what fails, and there is no parent to stat.
    let missing_parent = base.join("does-not-exist").join("session.json");
    let enoent = write_atomic_diagnosed(&missing_parent, b"{}").unwrap_err();
    assert_eq!(enoent.path, missing_parent);
    assert!(enoent.parent.is_none(), "stat on a missing parent must not fabricate one");
    assert_eq!(enoent.failure, WriteFailure::CreateFailed(libc::ENOENT));
    assert_eq!(enoent.failure.errno(), Some(libc::ENOENT));

    // A destination that is already a directory is refused before any syscall could fail — no
    // errno — but its (existing) parent is still reported, uid/gid/mode.
    let as_dir = base.join("already-a-dir");
    std::fs::create_dir_all(&as_dir).unwrap();
    let not_owned = write_atomic_diagnosed(&as_dir, b"{}").unwrap_err();
    assert_eq!(not_owned.failure, WriteFailure::NotOwned);
    assert_eq!(not_owned.failure.errno(), None);
    let parent_meta = std::fs::metadata(&base).unwrap();
    let parent = not_owned.parent.expect("the base directory exists and is stat-able");
    assert_eq!(parent.uid, parent_meta.uid());
    assert_eq!(parent.gid, parent_meta.gid());
    assert_eq!(parent.mode, parent_meta.mode() & 0o7777);

    // The ordinary success case still lands a whole file — `Ok(())` rather than `true`.
    let ok_path = base.join("session.json");
    assert!(write_atomic(&ok_path, b"{\"a\":1}").is_ok());
    assert_eq!(std::fs::read(&ok_path).unwrap(), b"{\"a\":1}");

    let _ = std::fs::remove_dir_all(&base);
}

// ---- The CANONICAL half (AUTH-08/AUTH-09): `clear()` must commit a canonical Cleared record,
// and a Cleared record must present like Missing (not Locked/Blocked) while still shadowing a
// reappearing legacy file. --------------------------------------------------------------------
//
// These exercise `persistence::load`/`write_session`/`commit_cleared` for real, not the
// `TEST_FILE` legacy-file bypass `TempSession` above uses — under `#[cfg(test)]`,
// `read_live_locked` short-circuits straight to `read_legacy_locked` whenever `TEST_FILE` is
// set, which is exactly right for grading the legacy file in isolation but would make it
// impossible to ever reach the canonical authority `clear`/`ReadState::Cleared` are about.
// `redirect_persistent_state_root_for_test` instead redirects the canonical store itself.

/// Point the canonical persistence root at a directory of this test's own, and take it back on
/// drop.
struct TempCanonicalRoot {
    dir: std::path::PathBuf,
}

impl TempCanonicalRoot {
    fn new(tag: &str) -> TempCanonicalRoot {
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-canonical-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        crate::paths::redirect_persistent_state_root_for_test(Some(dir.clone()));
        TempCanonicalRoot { dir }
    }
}

impl Drop for TempCanonicalRoot {
    fn drop(&mut self) {
        crate::paths::redirect_persistent_state_root_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// AUTH-08 (RED before the fix, for the real reason — not a compile error, not a fixture bug):
/// `clear()` only ever swept the legacy on-disk candidates, never committing anything to the
/// canonical authority. `save`/`clear` both route through `persistence::write_session`/
/// `persistence::load` exactly as a live build does once `TEST_FILE` is left unset (see
/// `save_locked_with_authority`'s own `TEST_FILE`-gated branch), so this is the production
/// path, not a fixture standing in for it — it fails today because `clear()` never calls
/// `persistence::commit_cleared()`, and the account token is still there to read back.
#[test]
fn clear_commits_a_canonical_cleared_record_so_the_account_token_does_not_survive_signout() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("signout");
    redirect_for_test(None);

    save(&signed_in());
    match persistence::load() {
        persistence::CanonicalRead::Missing => {
            panic!("setup: the seed save did not reach the canonical authority")
        }
        persistence::CanonicalRead::Cleared { .. } => {
            panic!("setup: the canonical authority already reads as cleared before clear() ran")
        }
        _ => {}
    }

    let outcome = clear();
    assert!(
        matches!(outcome, ClearOutcome::Durable { .. }),
        "clear() must report the canonical clear it just committed as durable: {outcome:?}"
    );

    match persistence::load() {
        persistence::CanonicalRead::Cleared { .. } => {}
        persistence::CanonicalRead::Missing => panic!(
            "clear() left the canonical authority Missing rather than committing an explicit \
             Cleared record — AUTH-09's legacy-shadow guarantee needs a real Cleared record, \
             not mere absence"
        ),
        _ => panic!(
            "AUTH-08: clear() did not commit a canonical Cleared record — the canonical \
             authority still answers with a readable/protected tenure after sign-out, so the \
             account token and roster survive sign-out in the authority `load()` actually reads"
        ),
    }

    match read_live_locked() {
        ReadState::Ready { session, .. } => panic!(
            "the account token survived sign-out in the live read path: {:?}",
            session.account_token
        ),
        _ => {}
    }
}

/// AUTH-09 (RED before the fix, for the real reason): a canonical `Cleared` record was mapped
/// onto the same `ReadState` as `Locked`/an unreadable `Blocked` envelope, so a cleanly
/// signed-out device booted with locked/blocked UI framing instead of a plain signed-out Home.
/// `prepare_load`'s `save` output is the load path's real signal for that distinction: a fresh
/// client id is minted AND persisted for a genuinely fresh/cleared device, exactly as it is on
/// true first boot — while a truly `Locked`/`Blocked` record must refuse to, because it might
/// still hold the only copy of real credentials once whatever blocked it clears. Conflating
/// `Cleared` with that policy is what fails this test today.
#[test]
fn a_cleared_canonical_tenure_boots_clean_and_still_shadows_a_reappearing_legacy_file() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("boot");
    redirect_for_test(None);

    // A legacy file "reappears" (e.g. carried over from a pre-DB8 install) beside a canonical
    // authority that has already recorded this tenure as cleared. RAII rather than a bare
    // statement at the end of the body: a panic between the write and the old plain
    // `remove_file` call left a SIGNED-IN plaintext session at the process's default legacy
    // path for every later test in the run.
    struct RemoveFallbackFile;
    impl Drop for RemoveFallbackFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(fallback_file());
        }
    }
    let _remove_fallback = RemoveFallbackFile;
    std::fs::write(fallback_file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();

    let commit = persistence::commit_cleared();
    assert!(
        matches!(commit, persistence::CanonicalCommit::Durable { .. }),
        "setup: the canonical authority must accept the clear: {commit:?}"
    );

    // AUTH-09a: not locked/blocked UI framing.
    let read = read_locked();
    let (session, save) = prepare_load(&read, || "fresh-id".to_string());
    assert!(
        session.account_token.is_empty(),
        "a cleared tenure must not read back with an account token"
    );
    assert!(
        save,
        "a cleared tenure must mint and persist a fresh client id exactly like Missing; \
         today it is conflated with Locked/Blocked, which refuses to persist a fresh id \
         (save={save})"
    );

    // AUTH-09b (must hold both before and after the fix): the legacy shadow priority survives.
    match read_live_locked() {
        ReadState::Ready { .. } => panic!(
            "a reappearing legacy file was read back over a canonical Cleared record — \
             AUTH-09's shadow-priority guarantee was broken"
        ),
        _ => {}
    }
}

/// `clear-bypasses-test-file-guard`: a legacy-fixture test (`TEST_FILE` redirected, exactly
/// `TempSession`'s shape) must never reach the process-wide canonical authority — only the
/// scratch legacy file it was pointed at. RED before the fix: `clear()` unconditionally called
/// `persistence::commit_cleared()`, so a `TempSession`-based `clear()` call durably wrote a
/// Cleared record into whatever `persistent_state_root()` resolves to for this process (which,
/// unredirected, is the real shared instance root) — a state mutation this test can observe
/// directly by reading the canonical authority right back with `TEST_FILE` still cleared,
/// exactly as `read_live_locked` does once the fixture goes away.
#[test]
fn clear_under_a_redirected_legacy_fixture_never_touches_the_canonical_authority() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("test-file-guard");
    redirect_for_test(None);

    // Seed the canonical authority with a real signed-in record, as if some earlier, real
    // (non-fixture) save had happened in this process.
    save(&signed_in());
    assert!(
        matches!(persistence::load(), persistence::CanonicalRead::Data { .. } | persistence::CanonicalRead::Opened { .. }),
        "setup: the canonical authority must hold a real record before the fixture clear runs"
    );

    // Now redirect to a legacy-fixture file, exactly like `TempSession`, and clear it. RAII
    // rather than a bare `redirect_for_test(None)` at the end: a panic between the redirect
    // and that restore left the crate-global `TEST_FILE` pointing at this scratch path for
    // every later test in the run — the exact transplant hazard `redirect_for_test`'s own doc
    // comment warns about.
    let dir = std::env::temp_dir().join(format!(
        "plxnative-session-test-file-guard-{}",
        std::process::id()
    ));
    struct RestoreLegacyFixture(std::path::PathBuf);
    impl Drop for RestoreLegacyFixture {
        fn drop(&mut self) {
            redirect_for_test(None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _restore_legacy = RestoreLegacyFixture(dir.clone());
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    redirect_for_test(Some(dir.join("auth.json")));
    std::fs::write(dir.join("auth.json"), serde_json::to_vec(&signed_in()).unwrap()).unwrap();

    let outcome = clear();
    assert!(
        matches!(outcome, ClearOutcome::Durable { .. }),
        "a legacy-fixture clear must still report success for the file it actually cleared"
    );
    assert!(!dir.join("auth.json").exists(), "the redirected legacy fixture must be cleared");

    redirect_for_test(None);
    match persistence::load() {
        persistence::CanonicalRead::Cleared { .. } => panic!(
            "clear() under a TEST_FILE redirect reached the real canonical authority and \
             signed it out — a legacy-fixture test must never mutate the process-wide \
             canonical store"
        ),
        _ => {}
    }
}

/// `signout-leaves-token-in-unswept-candidates`: `clear()` must actually reach
/// `persistence::cleanup_after_confirmed_clear()` after a durable canonical commit — the step
/// that retires the full recognized migration-candidate set
/// (`paths::session_migration_candidates()`, plus the pre-DB8 canonical JSON wrapper on ARM),
/// which is a strict superset of the legacy `auth_paths()` list `clear()`'s own loop sweeps.
///
/// **On the honesty of this test**: `persistence::bootstrap`/`cleanup_after_confirmed_clear`
/// deliberately substitute `super::auth_paths()` for the real
/// `paths::session_migration_candidates()` under `#[cfg(test)]` (see both functions' own
/// `#[cfg(test)]`/`#[cfg(not(test))]` split), for the same test-hermeticity reason `TempSession`
/// exists — a host test must never touch `paths::in_app_dir`'s real on-device-shaped path. That
/// makes the widened candidate SET itself unreachable from a host unit test; what this test
/// verifies instead, and can only be defeated by removing the call, is that `clear()` reaches
/// `cleanup_after_confirmed_clear()` at all and faithfully reports its verdict rather than
/// assuming success. It does this by making the one candidate in scope (`auth_paths()`'s single
/// entry, whatever `clear()`'s own loop swept it to) something `clear()`'s own
/// `std::fs::remove_file` cannot remove — a directory — so `cleanup_after_confirmed_clear`'s
/// stricter regular-file check is the only thing left that can observe it, and its answer must
/// be `false`. Mutation-tested: replacing the real
/// `persistence::cleanup_after_confirmed_clear()` call with a hardcoded `true` makes this test
/// fail (expected `legacy_swept == false`, observed `true`).
#[test]
fn clear_reports_an_incomplete_sweep_when_a_recognized_candidate_cannot_be_retired() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("migration-sweep");
    redirect_for_test(None);

    // Replace the legacy candidate with a directory: `clear()`'s own `remove_file` cannot
    // remove it (it is not a regular file), so it survives that loop — exactly like a file
    // owned by another uid or otherwise un-removable would — and only
    // `cleanup_after_confirmed_clear`'s stricter check can see the residue.
    let candidate = fallback_file();
    let _ = std::fs::remove_file(&candidate);
    let _ = std::fs::remove_dir_all(&candidate);
    std::fs::create_dir(&candidate).expect("a directory standing in for an unremovable candidate");
    struct RestoreFallback(std::path::PathBuf);
    impl Drop for RestoreFallback {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _restore = RestoreFallback(candidate.clone());

    save(&signed_in());

    let outcome = clear();
    match outcome {
        ClearOutcome::Durable { legacy_swept } => assert!(
            !legacy_swept,
            "the unremovable candidate must be reported as an incomplete sweep, not silently \
             treated as fully retired — clear() may not have reached \
             `persistence::cleanup_after_confirmed_clear()` at all"
        ),
        ClearOutcome::AuthorityNotConfirmed => panic!(
            "setup: the canonical authority must confirm Cleared before this test's legacy \
             sweep assertion is meaningful"
        ),
        ClearOutcome::NotDurable => panic!("setup: the canonical clear must durably commit"),
    }
    assert!(candidate.is_dir(), "the un-removable candidate must still be present");
}

/// Finding 2: pins `clear_cleanup_outcome`'s mapping directly, arm by arm — not only through
/// `clear()`'s end-to-end path, which cannot reach `AuthorityNotConfirmed` on the host (there
/// is no seam that makes a load-side read-back disagree with a commit that just landed). No
/// isolated runtime-state root is needed: `clear_cleanup_outcome` is a pure function with no
/// I/O of its own — every side effect (the log lines) is unobservable to this test, and
/// nothing it touches is process-global or shared.
///
/// The `AuthorityNotConfirmed` arm is the one this finding is about:
/// `ClearCleanupOutcome::AuthorityNotConfirmed` must map to `ClearOutcome::
/// AuthorityNotConfirmed`, never to `ClearOutcome::Durable { legacy_swept: false }` — which
/// would silently re-commit exactly the conflation AUTH-09 Finding B existed to prevent,
/// while still passing `make check` today (nothing reaches this arm end to end).
///
/// RED: OBSERVED. Temporarily changing the `AuthorityNotConfirmed` arm in
/// `clear_cleanup_outcome` to `ClearOutcome::Durable { legacy_swept: false }` and re-running
/// only this test failed on the `assert_eq!` below (`Durable { legacy_swept: false } !=
/// AuthorityNotConfirmed`); reverting turned it back green. This test does not merely
/// duplicate `clear_reports_an_incomplete_sweep_when_a_recognized_candidate_cannot_be_retired`
/// or the other `clear()` end-to-end tests above — none of them can reach the
/// `AuthorityNotConfirmed` arm at all (the same mutation leaves every one of them passing,
/// which is exactly the gap this finding names).
#[test]
fn clear_cleanup_outcome_maps_all_three_arms_and_never_conflates_unconfirmed_with_durable() {
    assert_eq!(
        clear_cleanup_outcome(persistence::ClearCleanupOutcome::Confirmed),
        ClearOutcome::Durable { legacy_swept: true }
    );
    assert_eq!(
        clear_cleanup_outcome(persistence::ClearCleanupOutcome::LegacyRetireFailed),
        ClearOutcome::Durable { legacy_swept: false }
    );
    assert_eq!(
        clear_cleanup_outcome(persistence::ClearCleanupOutcome::AuthorityNotConfirmed),
        ClearOutcome::AuthorityNotConfirmed,
        "AuthorityNotConfirmed must never be reported as Durable{{legacy_swept: false}} — \
         that is the exact conflation AUTH-09 Finding B existed to prevent"
    );
}

/// `failed-canonical-clear-is-silent`: a canonical clear that does not durably land must be
/// observable by `clear()`'s caller, not only by an event-log line. RED before the fix:
/// `clear()` returned `()`, so this assertion could not even be expressed. A non-directory
/// canonical root makes `persistence::store()`/`commit_cleared()` fail with `StoreError::Io`
/// without touching any real path.
#[test]
fn clear_reports_a_non_durable_outcome_when_the_canonical_commit_is_refused() {
    let _serial = crate::testlock::serial();
    let dir = std::env::temp_dir().join(format!(
        "plxnative-session-canonical-not-a-dir-{}",
        std::process::id()
    ));
    // RAII rather than a bare restore at the end: this test does not use `TempCanonicalRoot`
    // at all, since its whole point is a canonical root that is a regular FILE rather than a
    // directory. A panic between the redirect and the old plain restore call left the
    // crate-global `TEST_PERSISTENT_STATE_ROOT` pointing at a non-directory for the rest of
    // the process, so every subsequent test's canonical store answered `StoreError::Io`.
    struct RestoreCanonicalRoot(std::path::PathBuf);
    impl Drop for RestoreCanonicalRoot {
        fn drop(&mut self) {
            crate::paths::redirect_persistent_state_root_for_test(None);
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _restore_root = RestoreCanonicalRoot(dir.clone());
    let _ = std::fs::remove_file(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::write(&dir, b"not a directory").unwrap();
    crate::paths::redirect_persistent_state_root_for_test(Some(dir.clone()));
    redirect_for_test(None);

    let outcome = clear();
    assert_eq!(
        outcome,
        ClearOutcome::NotDurable,
        "a refused canonical commit must be reported to the caller as non-durable, not \
         silently treated as a completed sign-out"
    );
}

/// `cleared-readstate-edits-untested` (a): a cleared canonical tenure must seed a fresh
/// playback quality exactly as a genuinely first-boot `Missing` device does — it must not be
/// treated as "a persisted session exists" the way `Locked`/`Blocked` are. Mutation-tested:
/// reverting `prepare_load`'s `persisted` to `!matches!(read, ReadState::Missing)` (dropping
/// the `| ReadState::Cleared` arm) leaves the rest of this module's suite green.
#[test]
fn prepare_load_seeds_a_fresh_playback_quality_for_cleared_exactly_as_for_missing() {
    let missing = prepare_load(&ReadState::Missing, || "id-missing".to_string()).0;
    let cleared = prepare_load(&ReadState::Cleared, || "id-cleared".to_string()).0;
    assert!(
        missing.playback_quality.is_some(),
        "setup: a Missing read must seed a fresh quality"
    );
    assert!(
        cleared.playback_quality.is_some(),
        "a Cleared read must seed a fresh playback quality exactly like Missing; today \
         `persisted` conflates Cleared with Locked/Blocked and refuses to seed one"
    );
}

/// `cleared-readstate-edits-untested` (b): `Cleared` must occupy its own identity bucket in
/// `read_identity`, distinct from both `Missing` and the `Locked`/`Blocked` pair — otherwise a
/// concurrent transition into or out of `Cleared` is invisible to `DeferredLoad::apply`'s
/// identity check. Mutation-tested: collapsing `ReadState::Cleared => vec![2]` to `vec![1]`
/// (the `Locked | Blocked` bucket) leaves the rest of this module's suite green.
#[test]
fn read_identity_gives_cleared_its_own_bucket_distinct_from_every_other_state() {
    let cleared = read_identity(&ReadState::Cleared);
    assert_ne!(cleared, read_identity(&ReadState::Missing));
    assert_ne!(cleared, read_identity(&ReadState::Locked));
    assert_ne!(cleared, read_identity(&ReadState::Blocked));
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

/// A temporary LS2/key-store failure must never turn ciphertext back into plaintext or make
/// `load` overwrite it with a newly minted, logged-out client id. The host has no Luna bus,
/// which is the exact unavailable-key condition this policy has to survive.
#[test]
fn an_unopenable_secure_session_is_preserved_without_plaintext_downgrade() {
    let _g = crate::testlock::serial();
    let t = TempSession::new("secure-locked");
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

    let (captured, entropy, deferred) = load_capturing_entropy();
    assert!(!captured.client_id.is_empty() && entropy.is_some());
    assert!(std::fs::read(t.file()).unwrap() == original);
    deferred.apply().unwrap();
    assert!(std::fs::read(t.file()).unwrap() == original, "deferred load preserves locked ciphertext too");
    let loaded = load();
    assert!(
        !loaded.client_id.is_empty(),
        "the run still gets an ephemeral id"
    );
    assert_eq!(std::fs::read(t.file()).unwrap(), original);

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
fn auto_sign_in_persists_without_replacing_other_session_state() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("auto-sign-in");
    let mut s = signed_in();
    s.user.uuid = "u-kid".into();
    s.sources.push(SourceRef {
        machine_id: "server-a".into(),
        token: "server-token".into(),
        address: "192.168.0.10".into(),
        port: 32400,
        ..Default::default()
    });
    save(&s);
    assert!(!peek().auto_sign_in());

    assert!(set_auto_sign_in(true));
    let landed = peek();
    assert!(landed.auto_sign_in());
    assert_eq!(landed.account_token, "acct");
    assert_eq!(landed.user.uuid, "u-kid");
    assert_eq!(landed.sources.len(), 1);

    assert!(
        !set_auto_sign_in(true),
        "setting the same value again must not touch the file"
    );
    assert!(set_auto_sign_in(false));
    assert!(!peek().auto_sign_in());
}

/// `take_ready` / a profile switch `save` a whole snapshot they loaded at the start of the
/// flow. That snapshot must carry the switch, or the next boot forgets it.
#[test]
fn a_full_save_of_a_switch_snapshot_keeps_auto_sign_in() {
    let _g = crate::testlock::serial();
    let _t = TempSession::new("auto-sign-in-save");
    let mut s = signed_in();
    s.user.uuid = "u-admin".into();
    save(&s);
    assert!(set_auto_sign_in(true));

    let mut snap = (*peek()).clone();
    snap.user.uuid = "u-kid".into();
    save(&snap);

    let landed = peek();
    assert!(
        landed.auto_sign_in(),
        "a whole-file replace of a loaded snapshot must not drop the switch"
    );
    assert_eq!(landed.user.uuid, "u-kid");
    assert_eq!(landed.account_token, "acct");
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

