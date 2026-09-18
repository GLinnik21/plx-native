//! [`peek`]'s live [`CACHE`]: the detail page's per-frame preview tick calls it every frame
//! (`player::preview::enabled`), and repeated calls with no intervening write must not re-read the
//! session file — that re-read is a `recv(2)` round trip to the storage helper on the television,
//! measured at ~27 ms/frame and the whole gap between 60 fps and the 26 fps the detail page
//! actually drew (2026-09-18). A write must still be observed on the very next call, a refused or
//! non-durable write must never be mistaken for a fact about the file, and a Locked/Blocked read
//! must be retried rather than latched forever. Replaces `session_write_rev_tests.rs`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::TempSession;

#[test]
fn a_durable_write_is_served_to_every_later_peek_from_memory() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-durable-served");
    save(&signed_in());

    reset_reads_for_test();
    for _ in 0..30 {
        assert_eq!(peek().client_id, "cid-1");
    }
    assert_eq!(
        reads_for_test(),
        0,
        "a durable write already proved the record; every later peek must be served from memory, \
         not re-read the file"
    );
}

#[test]
fn update_reads_the_authority_and_peek_reads_the_result() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-update-then-peek");
    save(&signed_in());

    reset_reads_for_test();
    assert!(update(|s| {
        let mut next = s.clone();
        next.trailer_autoplay = true;
        Some(next)
    }));
    assert_eq!(
        reads_for_test(),
        1,
        "update's own fence read against the authority is the only read a write should ever cost"
    );
    assert!(peek().trailer_autoplay(), "peek must see the edit");
    assert_eq!(
        reads_for_test(),
        1,
        "update installs its own outcome, so the very next peek must not re-read the file"
    );
}

/// The fix for the review finding `session_write_rev_tests.rs` used to guard, now against the
/// real cache: an old per-field design mapped a failed read (`ReadState::Missing | Locked |
/// Blocked | Cleared`) to a cached `false` with no way to un-latch it short of an unrelated write
/// — so a single storage-helper glitch could disable trailer autoplay indefinitely. The current
/// behaviour still caches a `Locked`/`Blocked` answer (re-reading it every frame would reintroduce
/// the ~27 ms/frame cost the cache exists to remove) but only for [`LOCKED_RETRY`], so a real
/// recovery is still felt quickly. Uses `peek_at` rather than `peek`, so it can simulate the retry
/// window elapsing without an actual one-second sleep.
#[test]
fn a_locked_record_is_retried_not_latched() {
    let _serial = crate::testlock::serial();
    let t = TempSession::new("cache-locked-retried");
    std::fs::write(
        t.file(),
        br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#,
    )
    .expect("write the locked fixture");
    invalidate_for_test();

    reset_reads_for_test();
    let t0 = std::time::Instant::now();
    assert!(
        peek_at(t0).client_id.is_empty(),
        "a Locked read must fall back to the default session"
    );
    assert_eq!(reads_for_test(), 1, "the first call must actually read the session");

    assert!(
        peek_at(t0 + std::time::Duration::from_millis(500)).client_id.is_empty(),
        "still the default within the retry window"
    );
    assert_eq!(
        reads_for_test(),
        1,
        "a Locked answer within LOCKED_RETRY must be served from cache, not re-read every call \
         — that is exactly the per-frame cost the cache exists to remove"
    );

    // A real recovery, simulated by writing a plaintext session directly — bypassing save/update,
    // the way an external migration or a keymanager coming back online would change what the next
    // read sees without this process itself writing anything.
    std::fs::write(t.file(), serde_json::to_vec(&signed_in()).unwrap()).unwrap();

    assert_eq!(
        peek_at(t0 + LOCKED_RETRY).client_id,
        "cid-1",
        "past the retry deadline, a real recovery must be observed within about a second rather \
         than staying latched at the earlier Locked answer"
    );
    assert_eq!(
        reads_for_test(),
        2,
        "the retry must cost exactly one more read, settling into a fresh cache entry"
    );
}

/// Point the canonical persistence root at a directory of this test's own, and take it back on
/// drop. A second, deliberate copy of `session_persistence_tests.rs`'s private fixture of the same
/// name — the plan that added this file flagged the duplication (three copies crate-wide) as worth
/// collapsing separately; this test needs the real canonical write path, not the `TEST_FILE`
/// legacy-file bypass every other test in this file uses.
struct TempCanonicalRoot {
    dir: std::path::PathBuf,
}

impl TempCanonicalRoot {
    fn new(tag: &str) -> TempCanonicalRoot {
        let dir = std::env::temp_dir().join(format!(
            "plxnative-session-cache-canonical-{}-{tag}",
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

#[test]
fn a_non_durable_write_drops_the_cache() {
    let _serial = crate::testlock::serial();
    let _root = TempCanonicalRoot::new("cache-non-durable-drops");
    redirect_for_test(None);

    save(&signed_in());
    assert!(
        !cache_is_empty_for_test(),
        "the first save's Durable commit must have installed a Ready read"
    );

    // Force the canonical commit under test to come back `Uncertain` at `ParentSync` — after the
    // record has actually been renamed into place (the real production seam; see
    // `storage::JsonStore::commit`), so `save_locked_with_authority` takes the `!durable` branch.
    crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::ParentSync);
    let mut next = signed_in();
    next.trailer_autoplay = true;
    save(&next);
    crate::storage::clear_injected_commit_failure_for_test();

    assert!(
        cache_is_empty_for_test(),
        "a write whose canonical commit came back non-durable must never be trusted as the \
         record — the disk state after an Uncertain commit is genuinely unknown, and the legacy \
         fallback here has nothing protected to preserve so it writes nothing either"
    );

    reset_reads_for_test();
    let _ = peek();
    assert_eq!(
        reads_for_test(),
        1,
        "after the cache was dropped, the next peek must read the authority exactly once"
    );
}

/// Port of `session_write_rev_tests.rs`'s `clear_drops_the_cached_snapshot`: bumping a revision
/// counter alone is not enough for `clear()` (sign-out) — the previous `Arc<Session>`, which holds
/// the account/server tokens, would otherwise sit in the cache until an unrelated later `Ready`
/// read happens to overwrite it, so a `peek()` caller in between would still see the signed-out
/// credentials.
#[test]
fn clear_drops_the_cached_session() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-clear-drops");
    save(&signed_in());

    assert_eq!(peek().client_id, "cid-1", "prime the cache with a Ready read");
    assert!(!cache_is_empty_for_test(), "the priming call above must have populated the cache");

    clear();

    assert!(
        cache_is_empty_for_test(),
        "clear() must drop the cached session immediately, not merely leave it to be overwritten \
         — a stale entry would keep serving the just-cleared account/server tokens to any peek() \
         caller until some unrelated later write happened to replace it"
    );
    reset_reads_for_test();
    assert!(peek().client_id.is_empty(), "signed out: peek must answer the default session");
    assert_eq!(
        reads_for_test(),
        1,
        "the post-clear peek must read the (now-cleared) authority exactly once"
    );
}

/// `update_with_outcome` only ever gates on the CURRENT record's `client_id` — there is no guard
/// on the CANDIDATE's `client_id`. This models "a refused write" the only way the real door
/// supports one: the `edit` closure notices its own candidate would be invalid and declines by
/// returning `None`, exactly as a real caller's policy would. It guards step 4's "install the
/// fence read" behaviour on the OTHER refusal path `update` itself can take.
#[test]
fn a_refused_write_installs_the_record_it_refused_over() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-refused-write");
    save(&signed_in());

    reset_reads_for_test();
    let wrote = update(|cur| {
        let mut candidate = cur.clone();
        candidate.client_id = String::new();
        if candidate.client_id.is_empty() {
            None
        } else {
            Some(candidate)
        }
    });
    assert!(!wrote, "a candidate with an empty client_id must be declined, not written");
    assert_eq!(peek().client_id, "cid-1", "the refused write must not have touched the record");
    assert_eq!(
        reads_for_test(),
        1,
        "the refusal's own fence read must be installed as the answer, not thrown away — the \
         next peek must not pay for a second read of the record it already just proved"
    );
}

/// Measures the SPAWNED thread's own elapsed time inside `peek()`, rather than racing a timeout
/// against the whole test — a `Barrier` guarantees the spawned thread only calls `peek()` once
/// `update`'s closure below is definitely running (which is definitely after `update_with_outcome`
/// took `IO`, since that happens before `edit` is ever called), so what gets measured is really
/// "how long did a concurrent `peek()` take while a write held `IO`", not scheduling luck.
#[test]
fn peek_from_another_thread_does_not_take_io() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-peek-other-thread");
    save(&signed_in());

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let worker_barrier = barrier.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        worker_barrier.wait();
        let start = std::time::Instant::now();
        let client_id = peek().client_id.clone();
        let _ = tx.send((client_id, start.elapsed()));
    });

    assert!(update(|s| {
        barrier.wait();
        // Held long enough that a `peek()` blocked on `IO` could not possibly finish inside it —
        // on unmodified code that is exactly what happens, since `peek` always takes this same
        // lock; a cached `peek()` never needs it and returns in microseconds.
        std::thread::sleep(std::time::Duration::from_millis(200));
        Some(s.clone())
    }));

    let (client_id, elapsed) = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("the other thread's peek() must complete");
    worker.join().expect("the other thread must not panic");
    assert_eq!(client_id, "cid-1");
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "peek() from another thread took {elapsed:?} while a concurrent update held IO — it must \
         be served from the cache, not block on the same lock the writer holds"
    );
}

/// Guard: passes already, on unmodified code too — a fixture that redirects to a different file
/// must never let a cached answer from the old one leak into the new tenure.
#[test]
fn redirecting_the_fixture_drops_the_cache() {
    let _serial = crate::testlock::serial();
    let a = TempSession::new("cache-redirect-a");
    save(&signed_in());
    assert_eq!(peek().client_id, "cid-1");
    drop(a);

    let _b = TempSession::new("cache-redirect-b");
    let mut other = signed_in();
    other.client_id = "cid-2".into();
    save(&other);
    assert_eq!(
        peek().client_id,
        "cid-2",
        "redirecting to a fresh fixture must not leave a stale cached answer from the old one"
    );
}
