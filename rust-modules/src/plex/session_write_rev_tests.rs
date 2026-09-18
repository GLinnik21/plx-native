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

/// The fix for the review finding this file exists to guard: the old per-field cache mapped a
/// failed read (`ReadState::Missing | Locked | Blocked | Cleared`) to a cached `false`, so a single
/// storage-helper glitch would disable trailer autoplay until the next in-process write happened to
/// bump `WRITE_REV` — which could be arbitrarily far away, or never. `snapshot` must instead cache
/// ONLY a `Ready` read, and hand back the uncached default for anything else, so the very next call
/// retries the disk.
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
    // directly, bypassing `save`/`update` — `ReadState::Locked` is one of the four non-Ready
    // states `snapshot` must never cache; `Missing`/`Blocked`/`Cleared` share the exact same
    // fallthrough arm, so this one stands for all of them. `TempSession::new`'s own
    // `redirect_for_test` call bumps `WRITE_REV`, so the calls below are a genuine cache miss
    // rather than a leftover hit against the previous fixture's cached value.
    let t2 = TempSession::new("snapshot-non-ready-locked");
    std::fs::write(
        t2.file(),
        br#"{"format":"plxnative-secure-session","version":99,"sealed":{}}"#,
    )
    .expect("write the locked fixture");

    reset_reads_for_test();
    assert!(
        !snapshot().trailer_autoplay(),
        "a Locked read must fall back to the default session, not whatever was cached before"
    );
    assert!(
        !snapshot().trailer_autoplay(),
        "still Locked on the second call too"
    );
    assert_eq!(
        reads_for_test(),
        2,
        "a non-Ready read must never be cached: WRITE_REV never moved between these two calls, so \
         a cache that latched the first Locked answer would have skipped the second file read \
         entirely — exactly the bug (one storage-helper glitch disabling trailer autoplay until \
         the next unrelated write) this test exists to catch"
    );
}
