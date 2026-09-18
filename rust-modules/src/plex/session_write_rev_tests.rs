//! [`snapshot`]'s `WRITE_REV`-keyed cache: the detail page's per-frame preview tick calls it every
//! frame (`player::preview::enabled`), and repeated calls with no intervening write must not
//! re-read the session file — that re-read is a `recv(2)` round trip to the storage helper on the
//! television, measured at ~27 ms/frame and the whole gap between 60 fps and the 26 fps the detail
//! page actually drew (2026-09-18). A write must still be observed on the very next call, and a
//! read that came back non-`Ready` (a storage-helper glitch, not a real persisted state) must never
//! be the thing that gets cached.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::TempSession;

#[test]
fn snapshot_caches_across_calls_and_observes_a_write() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("snapshot-cache");
    save(&signed_in());
    // `Session` derives `Default` (a plain `false`), unlike the on-missing-key JSON default the
    // `trailer_autoplay` field otherwise carries — so pin a known starting value explicitly
    // rather than lean on either default.
    set_trailer_autoplay(true);

    reset_reads_for_test();
    assert!(snapshot().trailer_autoplay(), "the value just written");
    let after_first = reads_for_test();
    assert!(after_first >= 1, "the first call must actually read the session");

    for _ in 0..20 {
        assert!(snapshot().trailer_autoplay());
    }
    assert_eq!(
        reads_for_test(),
        after_first,
        "twenty more calls with no write in between must not re-read the session file"
    );

    // `set_trailer_autoplay` (via `update`) does its own internal read to compare against the
    // current value before deciding to write, so the read count moves before `snapshot` is even
    // called again — that internal read is not what this test is about, so it is captured here
    // rather than folded into the assertion below.
    set_trailer_autoplay(false);
    let after_write = reads_for_test();
    assert!(
        !snapshot().trailer_autoplay(),
        "a write is observed on the very next call, not silently cached past it"
    );
    assert_eq!(
        reads_for_test(),
        after_write + 1,
        "the write bumped WRITE_REV, so the stale cache had to re-read on this exact next call — \
         one more read than right after the write, no more and no less"
    );

    let after_write = reads_for_test();
    for _ in 0..20 {
        assert!(!snapshot().trailer_autoplay());
    }
    assert_eq!(
        reads_for_test(),
        after_write,
        "the cache settles again after the one re-read the write forced"
    );
}

/// The fix for the review finding this file used to guard: the old per-field cache mapped a
/// failed read (`ReadState::Missing | Locked | Blocked | Cleared`) to a cached `false` with no way
/// to ever un-latch it short of an unrelated write — so a single storage-helper glitch could
/// disable trailer autoplay until whenever that next write happened to land, arbitrarily far away
/// or never. The CURRENT behaviour (2026-09-18 review) still caches a `Locked`/`Blocked` answer —
/// re-reading it every frame would reintroduce the ~27 ms/frame cost `snapshot` exists to remove —
/// but only for `SNAPSHOT_RETRY`: at most one re-read per second, so a real recovery is still felt
/// quickly and a per-frame caller never pays for the retry itself. This test drives that with
/// `snapshot_at` rather than `snapshot`, so it can simulate the retry window elapsing without an
/// actual one-second sleep.
#[test]
fn snapshot_never_caches_a_non_ready_read() {
    let _serial = crate::testlock::serial();

    // A genuine Ready baseline, just far enough to have something for the Locked case below to
    // differ from (the ordinary cache-hit behaviour itself is
    // `snapshot_caches_across_calls_and_observes_a_write`'s job).
    let _t1 = TempSession::new("snapshot-non-ready-baseline");
    save(&signed_in());
    set_trailer_autoplay(true);
    reset_reads_for_test();
    assert!(snapshot().trailer_autoplay());
    let after_first = reads_for_test();
    assert!(snapshot().trailer_autoplay());
    assert_eq!(
        reads_for_test(),
        after_first,
        "a second call against an unchanged Ready session must be served from cache"
    );

    // Redirect to a second scratch file and write a secure envelope this build cannot open
    // directly, bypassing `save`/`update` — `ReadState::Locked` is one of the two transient states
    // `snapshot` caches with a retry deadline rather than forever; `Blocked` shares the exact same
    // arm, so this one stands for both. `TempSession::new`'s own `redirect_for_test` call bumps
    // `WRITE_REV`, so the calls below are a genuine cache miss rather than a leftover hit against
    // the previous fixture's cached value.
    let t2 = TempSession::new("snapshot-non-ready-locked");
    std::fs::write(
        t2.file(),
        br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#,
    )
    .expect("write the locked fixture");

    reset_reads_for_test();
    let t0 = std::time::Instant::now();
    assert!(
        !snapshot_at(t0).trailer_autoplay(),
        "a Locked read must fall back to the default session"
    );
    assert_eq!(reads_for_test(), 1, "the first call must actually read the session");

    assert!(
        !snapshot_at(t0).trailer_autoplay(),
        "still the default within the retry window"
    );
    assert_eq!(
        reads_for_test(),
        1,
        "a Locked answer within SNAPSHOT_RETRY must be served from cache, not re-read every call \
         — that is exactly the per-frame cost `snapshot` exists to remove"
    );

    // Advance past the retry deadline (no sleep — `snapshot_at` takes "now" as a parameter) and
    // confirm the next call retries the disk exactly once, then settles into a new cache entry.
    let past_retry = t0 + SNAPSHOT_RETRY;
    assert!(!snapshot_at(past_retry).trailer_autoplay());
    assert_eq!(
        reads_for_test(),
        2,
        "a transient answer must be re-read after its retry deadline, so a real recovery (or a \
         still-Locked file) is observed within about a second rather than latched forever"
    );
    assert!(!snapshot_at(past_retry).trailer_autoplay());
    assert_eq!(
        reads_for_test(),
        2,
        "the re-read establishes a fresh retry window, which the very next call must hit from \
         cache rather than reading a third time"
    );
}

/// The other half of the same review finding: bumping `WRITE_REV` alone is not enough for
/// `clear()` (sign-out) — the previous `Arc<Session>`, which holds the account/server tokens, would
/// otherwise sit in `SNAPSHOT_CACHE` until the next `Ready` read happens to overwrite it, so a
/// `snapshot()` caller in between would still see the signed-out credentials. `invalidate_snapshot`
/// exists so the bump and the drop can never drift apart; this pins the drop half directly, since
/// nothing about `WRITE_REV` moving proves the cache slot is actually `None`.
#[test]
fn clear_drops_the_cached_snapshot() {
    let _serial = crate::testlock::serial();
    let _t = TempSession::new("snapshot-clear-drops-cache");
    save(&signed_in());
    set_trailer_autoplay(true);

    assert!(snapshot().trailer_autoplay(), "prime the cache with a Ready read");
    assert!(
        !snapshot_cache_is_empty_for_test(),
        "the priming call above must have populated the cache"
    );

    clear();

    assert!(
        snapshot_cache_is_empty_for_test(),
        "clear() must drop the cached snapshot immediately, not merely bump WRITE_REV — a stale \
         entry would keep serving the just-cleared account/server tokens to any snapshot() caller \
         until some unrelated later write happened to overwrite it"
    );
}
