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
        ReadState::Ready { session: reloaded, plaintext, .. } => {
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

/// The success-path defect this change fixes: `save_legacy_fallback_locked` accumulates a
/// [`CandidateDiagnostic`] per refused candidate but used to reach [`log_candidate_diagnostics`]
/// only on the total-failure paths — both success arms `return`ed before it ran. That threw the
/// refusal evidence away on exactly the shape
/// `runtime_dir_candidate_persists_and_reloads_when_every_durable_candidate_is_unwritable` above
/// exercises: durable candidates refuse, the runtime-dir one accepts — which is the common case on
/// the jail the 2026-09-20 field report came from, and the one case where "why did the durable
/// ones refuse" is the whole question worth answering.
///
/// This suite has no facility to capture `crate::log`'s own output (it goes to a shared, on-disk
/// event log via `scrub_local` and `lab::record`; building a capture for that under time pressure
/// is out of scope here). Instead this proves what has to be true for the fixed call to say
/// anything real: replaying `write_atomic_diagnosed` against the same two candidates the real loop
/// tries (and is refused by) first shows each produces a genuine, non-empty [`CandidateDiagnostic`]
/// — a real errno, a real path — i.e. there IS evidence, not an empty vector, for the success arm
/// to log; and the real `save_legacy_fallback_locked`, run against the identical jail shape, still
/// succeeds via the later candidate. It does NOT prove `log_candidate_diagnostics` was actually
/// invoked on that success arm, nor the resulting log line's wording — that wiring is only checked
/// by reading the call site this change added, not by this test.
#[test]
fn refused_candidates_produce_evidence_when_a_later_one_still_succeeds() {
    let _g = crate::testlock::serial();
    let (_guard, candidates) = TempCandidates::new("evidence-survives-success");
    redirect_candidates_for_test(Some(candidates.clone()));

    let session = signed_in();
    let json = serde_json::to_vec_pretty(&session).unwrap();

    // The same two durable candidates the real loop tries (and is refused by) first: prove each
    // produces a genuine CandidateDiagnostic, not a bare bool, so there is real evidence to log.
    let first = write_atomic_diagnosed(&candidates[0], &json).unwrap_err();
    let second = write_atomic_diagnosed(&candidates[1], &json).unwrap_err();
    assert_eq!(first.path, candidates[0]);
    assert_eq!(second.path, candidates[1]);
    assert!(
        first.failure.errno().is_some(),
        "a refusal must carry a real errno to be worth logging"
    );
    assert!(second.failure.errno().is_some());

    // The real function, in the identical jail shape, still succeeds via the runtime-dir
    // candidate — this is the success arm whose early `return` used to discard the evidence just
    // shown to exist above.
    let outcome = save_legacy_fallback_locked(&session, false, false);
    assert!(outcome.is_some(), "the runtime-dir candidate must still accept the write");
    assert!(candidates[2].exists(), "the session must land on the runtime-dir candidate");
    assert!(!candidates[0].exists() && !candidates[1].exists());
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
    let read = read_locked(persistence::load());
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

// The canonical seam supplies the same typed failure as a dead native helper. It does not
// bypass the production save, fallback writer, candidate reader, or cache.
struct CanonicalReadOverride;
impl CanonicalReadOverride {
    fn unavailable() -> Self {
        persistence::READ_FOR_TEST.with(|hook| {
            hook.set(Some(|| {
                persistence::CanonicalRead::Blocked(crate::storage::StoreError::HelperUnavailable)
            }))
        });
        Self
    }
}
impl Drop for CanonicalReadOverride {
    fn drop(&mut self) {
        persistence::READ_FOR_TEST.with(|hook| hook.set(None));
        invalidate_for_test();
    }
}

#[test]
fn unavailable_helper_fallback_survives_cache_drop_and_restart() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("blocked-restart");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    let expected = signed_in();
    let outcome = {
        let _io = io();
        save_locked_with_authority(&expected, SaveAuthority::FreshReauthentication)
    };
    assert!(candidates[2].exists(), "real fallback write must land");
    assert!(matches!(
        outcome.commit,
        Some(persistence::CanonicalCommit::Failed(
            crate::storage::StoreError::HelperUnavailable
        ))
    ));
    for _ in 0..2 {
        // No cache/Session survives: read the on-disk candidates as a new process would.
        invalidate_for_test();
        let actual = peek_at(std::time::Instant::now());
        assert_eq!(actual.account_token, expected.account_token);
        assert_eq!(actual.client_id, expected.client_id);
    }
}

#[test]
fn fallback_never_outranks_present_or_untrusted_canonical_state() {
    use crate::storage::StoreError as E;
    use persistence::CanonicalRead as C;
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("blocked-priority");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates));
    let _helper = CanonicalReadOverride::unavailable();
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    for read in [
        (|| C::Pending {
            revision: 1,
            envelope: "opaque".into(),
        }) as fn() -> C,
        || C::Locked {
            revision: 1,
            public: Session::default(),
            protection: None,
        },
        || C::Cleared { revision: 1 },
        || C::Data {
            revision: 1,
            payload: "not json".into(),
        },
        || C::Blocked(E::InvalidSchema),
        || C::Blocked(E::UnknownFormat),
        || C::Blocked(E::UnsupportedVersion),
        || C::Blocked(E::AuthLocked),
        || C::Blocked(E::HelperAuthentication),
        || C::Blocked(E::HelperProtocol),
        || {
            C::Blocked(E::Io {
                stage: crate::storage::CommitStage::Readback,
                errno: 5,
            })
        },
    ] {
        persistence::READ_FOR_TEST.with(|hook| hook.set(Some(read)));
        assert!(!matches!(read_live_locked(), ReadState::Ready { .. }));
    }
    persistence::READ_FOR_TEST.with(|hook| {
        hook.set(Some(|| C::Opened {
            revision: 1,
            session: Session {
                account_token: "older-canonical".into(),
                ..signed_in()
            },
        }))
    });
    assert_eq!(
        session_from_read(&read_live_locked()).account_token,
        "older-canonical"
    );
    persistence::READ_FOR_TEST.with(|hook| hook.set(Some(|| C::Missing)));
    assert!(matches!(read_live_locked(), ReadState::Missing),
        "the authoritative read retired the old fallback before canonical disappeared");
}

#[test]
fn legacy_unmarked_file_requires_missing_canonical_and_marked_sealed_stays_locked() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("blocked-legacy");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    write_atomic(&candidates[2], &serde_json::to_vec(&signed_in()).unwrap()).unwrap();
    assert!(matches!(read_live_locked(), ReadState::Blocked));
    persistence::READ_FOR_TEST.with(|hook| hook.set(Some(|| persistence::CanonicalRead::Missing)));
    assert_eq!(session_from_read(&read_live_locked()).account_token, "acct");
    let _helper_again = CanonicalReadOverride::unavailable();
    let envelope =
        serde_json::json!({"format":SECURE_FORMAT,"version":99,"sealed":{}, FALLBACK_MARKER:1});
    write_atomic(&candidates[2], &serde_json::to_vec(&envelope).unwrap()).unwrap();
    assert!(matches!(read_live_locked(), ReadState::Locked));
}

#[test]
fn fallback_cache_retries_and_observes_recovery_without_reviving_revocation() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("blocked-cache");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates));
    let _helper = CanonicalReadOverride::unavailable();
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    reset_reads_for_test();
    let now = std::time::Instant::now();
    assert_eq!(peek_at(now).account_token, "acct");
    assert!(peek_settled().is_none());
    let mut watch = VisibleSessionWatch::default();
    assert!(watch.changed());
    let generation = visible_generation();
    assert_eq!(peek_at(now + LOCKED_RETRY / 2).account_token, "acct");
    assert_eq!(reads_for_test(), 1);
    assert_eq!(peek_at(now + LOCKED_RETRY).account_token, "acct");
    assert_eq!(reads_for_test(), 2);
    assert_eq!(visible_generation(), generation);
    assert!(!watch.changed());
    persistence::READ_FOR_TEST.with(|hook| {
        hook.set(Some(|| persistence::CanonicalRead::Opened {
            revision: 1,
            session: signed_in(),
        }))
    });
    assert_eq!(peek_at(now + LOCKED_RETRY * 2).account_token, "acct");
    assert!(peek_settled().is_some());
    assert!(
        watch.changed(),
        "settling matters even when content is unchanged"
    );
    let _helper_again = CanonicalReadOverride::unavailable();
    revoke_cached_session();
    {
        let _io = io();
        refresh_locked(now + LOCKED_RETRY * 3);
    }
    assert!(cache_revoked());
    assert!(!update(|s| Some(s.with_auto_sign_in(true))));
    assert!(load().account_token.is_empty());
    assert!(peek().account_token.is_empty());
    assert!(cache_revoked());
    // Model a new process for fixture teardown (ordinary cache drops preserve revocation).
    redirect_for_test(None);
}

#[test]
fn signout_with_unavailable_helper_removes_all_fallback_candidates() {
    let _serial = crate::testlock::serial();
    let root = TempCanonicalRoot::new("blocked-clear");
    // A non-directory canonical root ensures the host clear fails too, as the dead helper does.
    std::fs::remove_dir_all(&root.dir).unwrap();
    std::fs::write(&root.dir, b"unavailable").unwrap();
    let (_paths, candidates) = TempCandidates::new("blocked-clear");
    redirect_for_test(None);
    // Use two writable candidates to prove the sweep removes every copy.
    let files = vec![
        candidates[2].clone(),
        candidates[2].with_file_name("second-auth.json"),
    ];
    redirect_candidates_for_test(Some(files.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    std::fs::copy(&files[0], &files[1]).unwrap();
    assert_eq!(clear(), ClearOutcome::NotDurable);
    assert!(files.iter().all(|p| !p.exists()));
    redirect_for_test(None); // new process: no local Revoked barrier
    assert!(matches!(read_live_locked(), ReadState::Blocked));
    assert!(peek_at(std::time::Instant::now()).account_token.is_empty());
    std::fs::remove_file(&root.dir).unwrap();
}

/// Preserve permissions even when the regression assertion panics.
struct RestorePermissions(Vec<(std::path::PathBuf, std::fs::Permissions)>);
impl RestorePermissions {
    fn set(paths: &[(&std::path::Path, u32)]) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let saved = Self(paths.iter().map(|(path, _)| {
            (path.to_path_buf(), std::fs::metadata(path).unwrap().permissions())
        }).collect());
        for (path, mode) in paths {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode)).unwrap();
        }
        saved
    }
}
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        for (path, permissions) in &self.0 {
            let _ = std::fs::set_permissions(path, permissions.clone());
        }
    }
}

#[test]
fn signout_neutralizes_fallback_when_parent_refuses_unlink() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("unlink-refused");
    let (_paths, candidates) = TempCandidates::new("unlink-refused");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    let file = &candidates[2];
    let _permissions = RestorePermissions::set(&[(file.parent().unwrap(), 0o500)]);
    assert_eq!(std::fs::remove_file(file).unwrap_err().raw_os_error(), Some(libc::EACCES));
    // Canonical readback cannot confirm the clear while the helper is unavailable.
    let _ = clear();
    assert!(file.exists(), "the directory still refuses unlink");
    redirect_for_test(None); // simulate launch with no local Revoked barrier
    assert!(session_from_read(&read_live_locked()).account_token.is_empty(),
        "an undeletable fallback must not resurrect after sign-out");
    assert_eq!(std::fs::read(file).unwrap(), SESSION_TOMBSTONE);
    assert!(matches!(read_legacy_filtered_locked(true), ReadState::Missing));
    assert!(matches!(read_legacy_filtered_locked(false), ReadState::Missing));
    persistence::READ_FOR_TEST.with(|hook| hook.set(Some(|| persistence::CanonicalRead::Missing)));
    assert!(matches!(read_live_locked(), ReadState::Missing));
}

#[test]
fn signout_reports_candidate_that_cannot_be_unlinked_or_neutralized() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("unlink-and-overwrite-refused");
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    let _permissions = RestorePermissions::set(&[
        (file.file().parent().unwrap(), 0o500), (file.file().as_path(), 0o400),
    ]);
    assert_eq!(std::fs::remove_file(file.file()).unwrap_err().raw_os_error(), Some(libc::EACCES));
    assert_eq!(std::fs::OpenOptions::new().write(true).open(file.file())
        .unwrap_err().raw_os_error(), Some(libc::EACCES));
    assert!(!persist_fallback_revocation_locked(), "no candidate can persist a marker");
    // This fixture bypasses canonical, so only the legacy sweep can downgrade the outcome.
    assert_eq!(clear(), ClearOutcome::Durable { legacy_swept: false });
    assert!(!fallback_revoked_at(&auth_paths()), "failure is reported even without a marker");
    assert_eq!(session_from_read(&read_legacy_locked()).account_token, "acct",
        "the persisted credentials remain; callers must be told retirement failed");
    assert!(peek().account_token.is_empty(), "this process remains locally revoked");
}

#[test]
fn confirmed_clear_neutralizes_an_undeletable_migration_candidate() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("confirmed-neutralization");
    let (_paths, candidates) = TempCandidates::new("confirmed-neutralization");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    let file = &candidates[2];
    let _permissions = RestorePermissions::set(&[(file.parent().unwrap(), 0o500)]);
    assert_eq!(clear(), ClearOutcome::Durable { legacy_swept: true });
    assert_eq!(std::fs::read(file).unwrap(), SESSION_TOMBSTONE);
    assert_eq!(persistence::cleanup_after_confirmed_clear(),
        persistence::ClearCleanupOutcome::Confirmed);
    redirect_for_test(None);
}

#[test]
fn neutralization_refuses_symlinks_and_shared_inodes_without_truncating_them() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("neutralization-owned-fd");
    let bytes = fallback_bytes(&signed_in()).unwrap();
    std::fs::write(file.file(), &bytes).unwrap();
    let alias = file.dir.join("alias");
    std::os::unix::fs::symlink(file.file(), &alias).unwrap();
    assert!(neutralize_session_candidate(&alias).is_err());
    assert_eq!(std::fs::read(file.file()).unwrap(), bytes);
    std::fs::remove_file(&alias).unwrap();
    std::fs::hard_link(file.file(), &alias).unwrap();
    assert_eq!(neutralize_session_candidate(&alias).unwrap_err().raw_os_error(), Some(libc::EPERM));
    assert_eq!(std::fs::read(file.file()).unwrap(), bytes);
}

#[test]
fn p1_recovery_write_retires_the_previous_outage_snapshot() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("p1-recovery");
    let (_paths, candidates) = TempCandidates::new("p1-recovery");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let helper = CanonicalReadOverride::unavailable();
    save(&signed_in());
    assert!(candidates[2].exists());
    drop(helper);
    let current = Session { account_token: "new-account".into(), ..signed_in() };
    save(&current);
    let _helper = CanonicalReadOverride::unavailable();
    invalidate_for_test();
    assert!(session_from_read(&read_live_locked()).account_token.is_empty(),
        "a later outage must not restore the pre-recovery snapshot");
}

#[test]
fn p1_failed_signout_cannot_reopen_fallback_after_restart() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("p1-revocation");
    let (_paths, candidates) = TempCandidates::new("p1-revocation");
    redirect_for_test(None);
    let stuck = candidates[2].clone();
    let other_dir = stuck.parent().unwrap().parent().unwrap().join("other-runtime");
    std::fs::create_dir(&other_dir).unwrap();
    redirect_candidates_for_test(Some(vec![stuck.clone(), other_dir.join("auth.json")]));
    let _helper = CanonicalReadOverride::unavailable();
    save(&signed_in());
    let _permissions = RestorePermissions::set(&[(stuck.parent().unwrap(), 0o500), (&stuck, 0o400)]);
    let _ = clear();
    assert!(cache_revoked());
    assert_eq!(serde_json::from_slice::<Session>(&std::fs::read(&stuck).unwrap()).unwrap().account_token, "acct");
    redirect_for_test(None); // new process, no in-memory revocation
    assert!(session_from_read(&read_live_locked()).account_token.is_empty(),
        "a durable marker in the other directory must suppress the stuck fallback");
    persistence::READ_FOR_TEST.with(|hook| hook.set(Some(|| persistence::CanonicalRead::Missing)));
    assert!(matches!(read_live_locked(), ReadState::Missing), "Missing cannot import revoked fallback");
    drop(_permissions);
    drop(_helper);
    let current = Session { account_token: "fresh-account".into(), ..signed_in() };
    {
        let _io = io();
        let _ = save_locked_with_authority(&current, SaveAuthority::Routine);
    }
    assert!(fallback_revoked_at(&auth_paths()), "routine writes do not end revocation");
    save(&current);
    assert!(!fallback_revoked_at(&auth_paths()), "proven fresh credentials end revocation");
    let _helper = CanonicalReadOverride::unavailable();
    save(&current);
    assert_eq!(session_from_read(&read_live_locked()).account_token, "fresh-account");
}

#[test]
fn p1_adapter_does_not_complete_an_erase_with_an_incomplete_sweep() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("p1-adapter-erase");
    save(&signed_in());
    let _permissions = RestorePermissions::set(&[
        (file.file().parent().unwrap(), 0o500), (file.file().as_path(), 0o400),
    ]);
    let mt = unsafe { crate::task::MainThread::assume() };
    let mut adapter = crate::app::adapters::session::SessionAdapter::live_resources_for_test(&mt, false);
    let mut meta = crate::stores::metadata::MetadataStore::default();
    assert!(adapter.begin_erase(1, false, &mut meta).is_none());
    crate::storage_worker::drain_for_test();
    assert!(adapter.take_erased(&mut meta).is_none(), "incomplete erase must remain retryable");
    assert!(cache_revoked());
    drop(_permissions);
    struct ResetClock;
    impl Drop for ResetClock { fn drop(&mut self) { crate::app::clock::set_replay(0); } }
    let _clock = ResetClock;
    crate::app::clock::set_replay(crate::app::clock::now().wrapping_add(1_000));
    let completed = adapter.take_erased(&mut meta);
    crate::storage_worker::drain_for_test();
    let completed = completed.or_else(|| adapter.take_erased(&mut meta));
    assert!(matches!(completed, Some(crate::auth::owner::SessionEvent::Erased { epoch: 1, .. })),
        "the same pending erase completes only after a successful retry");
    assert!(cache_revoked());
}

#[test]
fn p1_authoritative_reads_retire_only_marked_files() {
    use persistence::CanonicalRead as C;
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("p1-read-retirement");
    redirect_for_test(None);
    let marked = candidates[2].clone();
    let legacy = marked.with_file_name("legacy-auth.json");
    redirect_candidates_for_test(Some(vec![marked.clone(), legacy.clone()]));
    let _helper = CanonicalReadOverride::unavailable();
    let legacy_bytes = serde_json::to_vec(&signed_in()).unwrap();
    write_atomic(&legacy, &legacy_bytes).unwrap();
    for read in [
        (|| C::Opened { revision: 1, session: signed_in() }) as fn() -> C,
        || C::Data { revision: 1, payload: serde_json::to_string(&signed_in()).unwrap() },
        || C::Cleared { revision: 1 },
    ] {
        assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
        persistence::READ_FOR_TEST.with(|hook| hook.set(Some(read)));
        { let _io = io(); let _ = read_live_locked(); }
        assert!(!marked.exists());
        assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
        persistence::READ_FOR_TEST.with(|hook| hook.set(Some(|| C::Blocked(crate::storage::StoreError::HelperUnavailable))));
        assert!(matches!(read_live_locked(), ReadState::Blocked));
    }
}

#[test]
fn p1_retirement_failure_does_not_fail_or_replace_canonical_data() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("p1-retirement-failure");
    let (_paths, candidates) = TempCandidates::new("p1-retirement-failure");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    assert!(save_legacy_fallback_locked(&signed_in(), false, false).is_some());
    let file = &candidates[2];
    let _permissions = RestorePermissions::set(&[(file.parent().unwrap(), 0o500), (file, 0o400)]);
    let current = Session { account_token: "canonical-account".into(), ..signed_in() };
    let result = { let _io = io(); save_locked_with_authority(&current, SaveAuthority::Routine) };
    assert!(matches!(result.commit, Some(persistence::CanonicalCommit::Durable { .. })));
    assert_eq!(session_from_read(&read_live_locked()).account_token, "canonical-account");
    assert!(file.exists());
}

#[test]
fn retirement_retries_parent_sync_even_after_the_file_is_gone() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("retirement-parent-sync-retry");
    std::fs::write(file.file(), fallback_bytes(&signed_in()).unwrap()).unwrap();
    std::thread_local! {
        static SYNCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    struct ResetSync;
    impl Drop for ResetSync {
        fn drop(&mut self) { RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None)); }
    }
    let _reset = ResetSync;
    SYNCS.with(|count| count.set(0));
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(Some(|| {
        SYNCS.with(|count| {
            count.set(count.get() + 1);
            (count.get() <= 2).then_some(libc::EIO)
        })
    })));
    assert!(!retire_session_candidate(&file.file()));
    assert!(!file.file().exists(), "unlink succeeded, but its parent sync did not");
    assert!(!retire_session_candidate(&file.file()),
        "NotFound must not turn the second failed parent sync into durable retirement");
    assert_eq!(SYNCS.with(|count| count.get()), 2);
    assert!(retire_session_candidate(&file.file()), "the next real parent sync succeeds");
    assert_eq!(SYNCS.with(|count| count.get()), 3);
}

#[test]
fn retirement_accepts_a_missing_parent_directory_without_a_sync() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("retirement-missing-parent");
    struct ResetSync;
    impl Drop for ResetSync {
        fn drop(&mut self) { RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None)); }
    }
    let _reset = ResetSync;
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(Some(|| {
        panic!("there is no parent directory to sync");
    })));
    assert!(retire_session_candidate(&file.dir.join("missing").join("auth.json")));
}

#[test]
fn recovery_retirement_retries_pending_parent_sync_before_skipping_absent_files() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("recovery-parent-sync-retry");
    std::fs::write(file.file(), fallback_bytes(&signed_in()).unwrap()).unwrap();
    std::thread_local! {
        static SYNCS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }
    struct ResetSync;
    impl Drop for ResetSync {
        fn drop(&mut self) {
            RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None));
            with_io_for_test(retire_marked_fallbacks_locked);
        }
    }
    let _reset = ResetSync;
    SYNCS.with(|count| count.set(0));
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(Some(|| {
        SYNCS.with(|count| {
            count.set(count.get() + 1);
            (count.get() <= 2).then_some(libc::EIO)
        })
    })));
    assert!(!with_io_for_test(retire_marked_fallbacks_locked));
    assert!(!file.file().exists(), "the first recovery pass unlinked the marked file");
    assert!(!with_io_for_test(retire_marked_fallbacks_locked),
        "the next recovery pass must retry the failed flush, even though stat says NotFound");
    assert_eq!(SYNCS.with(|count| count.get()), 2);
    assert!(with_io_for_test(retire_marked_fallbacks_locked));
    assert_eq!(SYNCS.with(|count| count.get()), 3);
    assert!(with_io_for_test(retire_marked_fallbacks_locked));
    assert_eq!(SYNCS.with(|count| count.get()), 3,
        "an ordinary recovery pass with no pending flush must not fsync absent candidates");
}

#[test]
fn signout_sweeps_cannot_complete_while_a_recovery_flush_is_pending() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("signout-pending-recovery-sync");
    std::fs::write(file.file(), fallback_bytes(&signed_in()).unwrap()).unwrap();
    struct ResetHooks;
    impl Drop for ResetHooks {
        fn drop(&mut self) {
            RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None));
            persistence::READ_FOR_TEST.with(|hook| hook.set(None));
            with_io_for_test(retry_pending_retirements_locked);
        }
    }
    let _reset = ResetHooks;
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(Some(|| Some(libc::EIO))));
    assert!(!with_io_for_test(retire_marked_fallbacks_locked));
    assert!(!file.file().exists());
    persistence::READ_FOR_TEST.with(|hook| hook.set(Some(|| persistence::CanonicalRead::Cleared { revision: 1 })));
    assert_eq!(with_io_for_test(persistence::cleanup_after_confirmed_clear),
        persistence::ClearCleanupOutcome::LegacyRetireFailed);
    assert_eq!(clear(), ClearOutcome::Durable { legacy_swept: false });
    assert_eq!(PENDING_RETIREMENTS.lock().unwrap().as_slice(), &[file.file()],
        "repeated failures keep exactly one pending entry for this path");
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None));
    assert_eq!(with_io_for_test(persistence::cleanup_after_confirmed_clear),
        persistence::ClearCleanupOutcome::Confirmed);
    assert!(PENDING_RETIREMENTS.lock().unwrap().is_empty());
    assert_eq!(clear(), ClearOutcome::Durable { legacy_swept: true });
}

// Exercises the real seal/open envelope paths, with identity transformation at the LS2 seam.
// This synthetic transport does not claim to test cryptography or firmware availability.
struct SyntheticKeymanager;
impl SyntheticKeymanager {
    fn new() -> Self {
        crate::keymanager::reset_for_test();
        crate::keymanager::RPC_FOR_TEST.with(|hook| hook.set(Some(|uri, payload| {
            let payload: serde_json::Value = serde_json::from_str(payload).unwrap();
            let response = if uri.ends_with("/generateKey") {
                serde_json::json!({"returnValue":true})
            } else if uri.ends_with("/begin") {
                serde_json::json!({"returnValue":true,"handle":"synthetic","iv":"synthetic-iv"})
            } else if uri.ends_with("/finish") {
                serde_json::json!({"returnValue":true,"output":payload["data"]})
            } else { panic!("unexpected synthetic keymanager operation"); };
            Ok(response.to_string())
        })));
        Self
    }
}
impl Drop for SyntheticKeymanager {
    fn drop(&mut self) {
        crate::keymanager::RPC_FOR_TEST.with(|hook| hook.set(None));
        crate::keymanager::reset_for_test();
    }
}

#[test]
fn p2_outage_reseals_the_latest_snapshot_over_a_marked_secure_fallback() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("p2-reseal");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    let _keymanager = SyntheticKeymanager::new();
    save(&signed_in());
    assert!(identifies_secure_envelope(&std::fs::read(&candidates[2]).unwrap()));
    let newer = Session { account_token: "newer-account".into(), ..signed_in() };
    save(&newer);
    invalidate_for_test();
    let read = with_io_for_test(read_live_locked);
    assert_eq!(session_from_read(&read).account_token, "newer-account",
        "restarting during the outage must restore the latest sealed snapshot");
    assert!(matches!(read, ReadState::Ready { plaintext: false, .. }));
}

#[test]
fn p2_plaintext_winner_retires_older_lower_priority_marked_candidates() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("p2-plaintext-stale");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    save(&signed_in());
    assert!(candidates[2].exists());
    let _writable = RestorePermissions::set(&[(candidates[0].parent().unwrap(), 0o700)]);
    save(&Session { account_token: "newer-account".into(), ..signed_in() });
    assert!(candidates[0].exists());
    let _unreadable = RestorePermissions::set(&[(&candidates[0], 0o000)]);
    invalidate_for_test();
    assert!(session_from_read(&with_io_for_test(read_live_locked)).account_token.is_empty(),
        "an unreadable winner must not reveal a stale lower-priority marked snapshot");
}

#[test]
fn p2_proven_signin_retries_failed_revocation_marker_removal() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("p2-marker-retry");
    let (_paths, candidates) = TempCandidates::new("p2-marker-retry");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let marker = fallback_revocation_path(&candidates[2]);
    write_atomic(&marker, b"revoked\n").unwrap();
    let permissions = RestorePermissions::set(&[(marker.parent().unwrap(), 0o500)]);
    let newer = Session { account_token: "new-account".into(), ..signed_in() };
    save(&newer);
    assert!(marker.exists(), "the proven write cannot yet unlink the old marker");
    drop(permissions);
    assert!(with_io_for_test(retire_marked_fallbacks_locked));
    assert!(!marker.exists(), "the next retirement pass must retry the authorized removal");
    let _helper = CanonicalReadOverride::unavailable();
    save(&newer);
    assert_eq!(session_from_read(&with_io_for_test(read_live_locked)).account_token, "new-account");
}

#[test]
fn marked_secure_fallback_never_downgrades_and_unmarked_secure_input_is_preserved() {
    let _serial = crate::testlock::serial();
    let file = TempSession::new("marked-secure-guards");
    let keymanager = SyntheticKeymanager::new();
    assert_eq!(with_io_for_test(|| save_legacy_fallback_locked(&signed_in(), false, false)), Some(true));
    let sealed = std::fs::read(file.file()).unwrap();
    drop(keymanager);
    let newer = Session { account_token: "newer-account".into(), ..signed_in() };
    assert_eq!(with_io_for_test(|| save_legacy_fallback_locked(&newer, false, false)), None);
    assert_eq!(std::fs::read(file.file()).unwrap(), sealed);
    let _keymanager = SyntheticKeymanager::new();
    let mut legacy: serde_json::Value = serde_json::from_slice(&sealed).unwrap();
    legacy.as_object_mut().unwrap().remove(FALLBACK_MARKER);
    let legacy = serde_json::to_vec(&legacy).unwrap();
    write_atomic(&file.file(), &legacy).unwrap();
    assert_eq!(with_io_for_test(|| save_legacy_fallback_locked(&newer, false, false)), None);
    assert_eq!(std::fs::read(file.file()).unwrap(), legacy,
        "available keymanager does not grant permission to overwrite unmarked secure input");
}

#[test]
fn sealed_fallback_retires_a_stale_candidate_even_when_it_cannot_be_unlinked() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("sealed-stale-neutralization");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _helper = CanonicalReadOverride::unavailable();
    let _keymanager = SyntheticKeymanager::new();
    save(&signed_in());
    let _writable = RestorePermissions::set(&[(candidates[0].parent().unwrap(), 0o700)]);
    let _no_unlink = RestorePermissions::set(&[(candidates[2].parent().unwrap(), 0o500)]);
    save(&Session { account_token: "newer-account".into(), ..signed_in() });
    assert_eq!(std::fs::read(&candidates[2]).unwrap(), SESSION_TOMBSTONE);
    assert_eq!(session_from_read(&with_io_for_test(read_live_locked)).account_token, "newer-account");
}

#[test]
fn plaintext_cleanup_preserves_unmarked_input_and_does_not_fail_a_successful_save() {
    let _serial = crate::testlock::serial();
    let (_paths, candidates) = TempCandidates::new("plaintext-cleanup-policy");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _writable = RestorePermissions::set(&[(candidates[0].parent().unwrap(), 0o700)]);
    let legacy = serde_json::to_vec(&signed_in()).unwrap();
    write_atomic(&candidates[2], &legacy).unwrap();
    let newer = Session { account_token: "newer-account".into(), ..signed_in() };
    assert_eq!(with_io_for_test(|| save_legacy_fallback_locked(&newer, false, false)), Some(false));
    assert_eq!(std::fs::read(&candidates[2]).unwrap(), legacy);
    let stale = fallback_bytes(&signed_in()).unwrap();
    write_atomic(&candidates[2], &stale).unwrap();
    let _stuck = RestorePermissions::set(&[
        (candidates[2].parent().unwrap(), 0o500), (&candidates[2], 0o400),
    ]);
    assert_eq!(with_io_for_test(|| save_legacy_fallback_locked(&newer, false, false)), Some(false));
    assert_eq!(std::fs::read(&candidates[2]).unwrap(), stale,
        "cleanup failure is best effort and does not fail the winning write");
    assert_eq!(session_from_read(&with_io_for_test(read_legacy_locked)).account_token, "newer-account");
}

#[test]
fn new_signout_cancels_an_older_signins_pending_marker_removal() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("new-signout-marker-retry");
    let (_paths, candidates) = TempCandidates::new("new-signout-marker-retry");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let marker = fallback_revocation_path(&candidates[2]);
    write_atomic(&marker, b"revoked\n").unwrap();
    let permissions = RestorePermissions::set(&[(marker.parent().unwrap(), 0o500)]);
    save(&signed_in());
    assert!(!PENDING_REVOCATION_REMOVALS.lock().unwrap().is_empty());
    revoke_cached_session(); // the frame can revoke before the queued clear persists its marker
    drop(permissions);
    assert!(with_io_for_test(retire_marked_fallbacks_locked));
    assert!(marker.exists(), "an old sign-in cannot revoke a newer sign-out barrier");
    assert!(PENDING_REVOCATION_REMOVALS.lock().unwrap().is_empty());
    assert_eq!(clear(), ClearOutcome::Durable { legacy_swept: true });
    assert!(marker.exists());
    redirect_for_test(None);
}

#[test]
fn pending_marker_removal_retries_its_parent_sync_after_unlink() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("marker-sync-retry");
    let (_paths, candidates) = TempCandidates::new("marker-sync-retry");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let marker = fallback_revocation_path(&candidates[2]);
    write_atomic(&marker, b"revoked\n").unwrap();
    struct ResetSync;
    impl Drop for ResetSync {
        fn drop(&mut self) {
            RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None));
            with_io_for_test(retire_marked_fallbacks_locked);
        }
    }
    let _reset = ResetSync;
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(Some(|| Some(libc::EIO))));
    save(&signed_in());
    assert!(!marker.exists());
    assert!(!with_io_for_test(retire_marked_fallbacks_locked));
    assert!(!PENDING_REVOCATION_REMOVALS.lock().unwrap().is_empty());
    RETIRE_PARENT_SYNC_FOR_TEST.with(|hook| hook.set(None));
    assert!(with_io_for_test(retire_marked_fallbacks_locked));
    assert!(PENDING_REVOCATION_REMOVALS.lock().unwrap().is_empty());
}

#[test]
fn fresh_outage_signin_after_signout_survives_restart() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("fresh-outage-after-signout");
    let (_paths, candidates) = TempCandidates::new("fresh-outage-after-signout");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates));
    let _helper = CanonicalReadOverride::unavailable();
    save(&signed_in());
    let _ = clear();
    assert!(fallback_revoked_at(&auth_paths()));
    let newer = Session { account_token: "new-signin".into(), ..signed_in() };
    save_fresh_reauthentication(&newer);
    redirect_for_test(None); // restart: no in-memory revocation or cache
    assert_eq!(session_from_read(&with_io_for_test(read_live_locked)).account_token, "new-signin");
}

#[test]
fn sealed_marked_fallback_migrates_when_canonical_recovers_missing() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("sealed-fallback-recovered-missing");
    let (_paths, candidates) = TempCandidates::new("sealed-fallback-recovered-missing");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _keymanager = SyntheticKeymanager::new();
    let helper = CanonicalReadOverride::unavailable();
    save(&signed_in());
    assert!(identifies_secure_envelope(&std::fs::read(&candidates[2]).unwrap()));
    drop(helper);
    assert!(matches!(persistence::load(), persistence::CanonicalRead::Missing));
    assert_eq!(load().account_token, "acct");
    let persistence::CanonicalRead::Data { payload, .. } = persistence::load() else {
        panic!("the recovered canonical store must receive the sealed marked fallback");
    };
    assert_eq!(serde_json::from_str::<Session>(&payload).unwrap().account_token, "acct");
    assert!(!candidates[2].exists());
}

#[test]
fn marked_missing_migration_failure_stays_transient_and_retries() {
    let _serial = crate::testlock::serial();
    for sealed in [false, true] {
        let _root = TempCanonicalRoot::new("marked-migration-retry");
        let (_paths, candidates) = TempCandidates::new("marked-migration-retry");
        redirect_for_test(None);
        redirect_candidates_for_test(Some(candidates.clone()));
        let _keymanager = sealed.then(SyntheticKeymanager::new);
        let helper = CanonicalReadOverride::unavailable();
        save(&signed_in());
        drop(helper);
        struct ResetFailure;
        impl Drop for ResetFailure {
            fn drop(&mut self) { crate::storage::clear_injected_commit_failure_for_test(); }
        }
        let _reset = ResetFailure;
        crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::CreateTemp);
        assert_eq!(load().account_token, "acct");
        assert!(peek_settled().is_none(), "a failed marked migration must remain retryable");
        assert!(matches!(*CACHE.lock().unwrap(), Cached::Transient { .. }));
        let now = std::time::Instant::now();
        assert!(candidates[2].exists());
        assert_eq!(peek_at(now + LOCKED_RETRY).account_token, "acct");
        assert!(peek_settled().is_some());
        assert!(matches!(persistence::load(), persistence::CanonicalRead::Data { .. }));
        assert!(!candidates[2].exists());
    }
}

#[test]
fn unmarked_sealed_missing_record_keeps_its_established_legacy_behavior() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("unmarked-sealed-missing");
    let (_paths, candidates) = TempCandidates::new("unmarked-sealed-missing");
    redirect_for_test(None);
    redirect_candidates_for_test(Some(candidates.clone()));
    let _keymanager = SyntheticKeymanager::new();
    assert_eq!(with_io_for_test(|| save_legacy_fallback_locked(&signed_in(), false, false)), Some(true));
    let mut envelope: serde_json::Value = serde_json::from_slice(&std::fs::read(&candidates[2]).unwrap()).unwrap();
    envelope.as_object_mut().unwrap().remove(FALLBACK_MARKER);
    let original = serde_json::to_vec(&envelope).unwrap();
    write_atomic(&candidates[2], &original).unwrap();
    assert_eq!(load().account_token, "acct");
    assert!(peek_settled().is_some());
    assert!(matches!(persistence::load(), persistence::CanonicalRead::Missing));
    assert_eq!(std::fs::read(&candidates[2]).unwrap(), original);
}
