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

/// Proves the claim deterministically rather than by racing a timeout against a sleep: the
/// `update(...)` closure below runs only once `update_with_outcome` already holds `IO` (that
/// happens before `edit` is ever called), so a `peek()` spawned from inside it and blocked
/// waiting to report back over `tx` can only mean one thing — it needed `IO` too, and `IO` is
/// held by this very thread until the closure returns. There is no way for that spawned `peek()`
/// to finish while the closure is still waiting on `rx`, so `recv_timeout` either sees the
/// cache-served answer almost immediately, or the closure times out and fails the test outright.
/// It fails; it does not hang.
#[test]
fn peek_from_another_thread_does_not_take_io() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-peek-other-thread");
    save(&signed_in()); // primes the cache before the point under test

    let (tx, rx) = std::sync::mpsc::channel();
    let mut worker: Option<std::thread::JoinHandle<()>> = None;

    assert!(update(|s| {
        // Spawned here, so it starts strictly after this closure already holds `IO`.
        worker = Some(std::thread::spawn(move || {
            let client_id = peek().client_id.clone();
            let _ = tx.send(client_id);
        }));
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(client_id) => {
                assert_eq!(client_id, "cid-1", "peek() from the other thread must see the cache");
                Some(s.clone())
            }
            Err(_) => panic!(
                "peek() from another thread did not report back within 2s while this closure \
                 held IO — on unmodified code that means it blocked on the same lock instead of \
                 being served from the cache"
            ),
        }
    }));

    worker
        .expect("the worker thread must have been spawned inside the closure")
        .join()
        .expect("the other thread must not panic");
}

/// Two callers can both find `CACHE` empty and then take turns on `IO`; the one that gets there
/// second must not blindly re-read storage once it finally has the lock — the caller ahead of it
/// may have already installed the answer. Modelled with the same `update(...)`-holds-`IO` trick
/// as the test above: the worker's own miss check runs (and is confirmed a real miss) BEFORE this
/// closure — which already holds `IO` — returns and installs the write's outcome, so the worker's
/// later `io()` call is guaranteed to block until after that install lands. `READS_FOR_TEST` is
/// thread-local (see its own doc), so it is reset and read on the worker's thread, the one that
/// actually matters here.
#[test]
fn a_peek_that_waited_for_io_sees_the_fill_and_does_not_reread() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-double-check-no-reread");
    save(&signed_in());
    invalidate_for_test(); // back to Unloaded: the next peek() must be a genuine miss

    let (checkpoint_tx, checkpoint_rx) = std::sync::mpsc::channel::<()>();
    let (reads_tx, reads_rx) = std::sync::mpsc::channel::<u32>();
    let mut worker: Option<std::thread::JoinHandle<()>> = None;

    assert!(update(|s| {
        // `update` already holds `IO` by the time this closure runs, so the worker's own `io()`
        // call below is guaranteed to block until this closure returns and its write installs —
        // it must NOT be joined in here, which would deadlock against that same lock.
        worker = Some(std::thread::spawn(move || {
            reset_reads_for_test();
            let now = std::time::Instant::now();
            assert!(
                cached_at(now).is_none(),
                "the cache must still read empty here — the behaviour under test is the SECOND \
                 check, taken once IO is finally held, not this first one"
            );
            checkpoint_tx.send(()).unwrap();
            let _ = peek_at(now);
            let _ = reads_tx.send(reads_for_test());
        }));
        checkpoint_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the worker must observe the miss before this closure returns");
        Some(s.clone())
    }));

    worker
        .expect("the worker thread must have been spawned inside the closure")
        .join()
        .expect("the worker must not panic");
    assert_eq!(
        reads_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(),
        0,
        "a miss that only reached the front of the IO queue after another caller already filled \
         CACHE must be served from that fill, not pay for a second, needless read of storage"
    );
}

/// A no-save `DeferredLoad::apply()` still took a real, verified read of the authority under
/// `IO` — an established, already-protected record needs no write, but the read itself is a fact
/// the cache must not throw away. Constructs the `DeferredLoad` directly (its fields are
/// module-private, visible here) rather than through the real boot path, to isolate exactly the
/// no-save branch under test.
#[test]
fn apply_installs_the_verified_read_when_it_does_not_save() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-deferred-apply-installs");
    save(&signed_in());

    let read = read_live_locked();
    let expected = read_identity(&read);
    let deferred = DeferredLoad { session: signed_in(), expected, save: false };

    invalidate_for_test(); // drop what `save` above installed, so `apply()` is what's under test
    assert!(cache_is_empty_for_test(), "the fixture must start empty for this to prove anything");

    deferred.apply().expect("the identity must still match: nothing wrote to the file meanwhile");

    assert!(
        !cache_is_empty_for_test(),
        "a no-save apply() must install the verified read it just took under IO — otherwise the \
         boot path's first peek() pays a third storage read for a fact this call already proved"
    );
    reset_reads_for_test();
    assert_eq!(peek().client_id, "cid-1");
    assert_eq!(
        reads_for_test(),
        0,
        "apply() already proved this record under IO; the next peek() must not re-read it"
    );
}

/// The other half of the same rule, on `apply()`'s refusal path: a capture that lost the race
/// (the file changed between capture and attachment) still took a real read under `IO` to notice
/// that — install the record it refused over, the same rule `update_with_outcome`'s own refusal
/// follows (see `a_refused_write_installs_the_record_it_refused_over` above).
#[test]
fn apply_installs_the_fresh_read_when_the_identity_check_fails() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("cache-deferred-apply-mismatch-installs");
    save(&signed_in());
    let stale_expected = read_identity(&read_live_locked());

    let mut changed = signed_in();
    changed.client_id = "cid-2".into();
    save(&changed);

    invalidate_for_test(); // drop what the second save() installed, so apply() is what's under test

    let deferred = DeferredLoad { session: signed_in(), expected: stale_expected, save: false };
    let result = deferred.apply();
    assert_eq!(result, Err("session changed during capture"));

    assert!(
        !cache_is_empty_for_test(),
        "a refused apply() must still install the fresh read it just verified under IO, not \
         leave the cache empty for the next peek() to pay for a read this call already took"
    );
    reset_reads_for_test();
    assert_eq!(peek().client_id, "cid-2", "the fresh on-disk record must be what peek() now sees");
    assert_eq!(
        reads_for_test(),
        0,
        "apply()'s own read already proved this; peek() must not redo it"
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
