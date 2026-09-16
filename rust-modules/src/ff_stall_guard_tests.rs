//! `StallGuard` arming and abort decisions for a server-paced HLS transfer.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// A completed PMS HLS object is not necessarily delivered at a stationary rate.  The server
/// reports `segmentWait` inside downloads while its JIT encoder catches up, then sends the
/// already-sized remainder as a burst.  On the television the first 214 440-byte prefix of a
/// roughly 6 MB 4K object took about 258 ms, while the adjacent complete objects landed in
/// 1.2--1.4 s.  Extrapolating that prefix over the unseen remainder predicts more than the
/// available 6.4 s reserve and aborts a stream whose complete acquisitions are sustainable.
///
/// The prefix is right-censored evidence: without a model of PMS's future production it can
/// prove only what has already been spent, not how long unseen bytes will take.  Keeping the
/// playhead fixed makes this differential: the broken attained-rate forecast fires; the
/// physical reserve has spent nothing and therefore cannot.
#[test]
fn a_server_paced_prefix_cannot_forecast_its_unseen_remainder() {
    let _serial = crate::testlock::serial();
    let pos = 5_000_000_000i64;
    crate::player::SHARED
        .playpos_ns
        .store(pos, std::sync::atomic::Ordering::Relaxed);
    let guard = StallGuard::arm(6_376).expect("a live reserve arms");
    assert!(
        !guard.should_abort(5_785_560, false),
        "a censored JIT prefix cannot prove that the complete response will miss the reserve",
    );
}

/// **But a reserve that was empty when the fetch STARTED never arms the guard at all**, which
/// is a different question and is the one that matters: that is every session's first segment,
/// and arming there aborts the fetch that would have created the picture. Device-measured
/// without this gate: `stall abort seq=0 ... of 0ms reserve`, playback never started, the video
/// plane never bound.
#[test]
fn an_empty_reserve_at_the_start_of_a_fetch_arms_nothing() {
    assert!(
        StallGuard::arm(0).is_none(),
        "so the guard must refuse to be armed"
    );
    assert!(StallGuard::arm(-1).is_none());
    assert!(
        StallGuard::arm(1).is_some(),
        "and must arm as soon as there is a picture to keep"
    );
}

#[test]
fn the_floor_guard_holds_the_clock_without_abandoning_the_only_rung() {
    let hold = StallGuard::arm_clock_hold(2_000).expect("a live floor reserve arms");
    assert!(
        !hold.aborts_fetch(),
        "the floor has no cheaper object to re-fetch"
    );
    assert!(
        StallGuard::arm(2_000)
            .expect("a live non-floor reserve arms")
            .aborts_fetch(),
        "above the floor the same signal must still release the controller to downshift",
    );
}

#[test]
fn an_existing_terminal_hold_arms_no_second_abort() {
    assert!(
        arm_active_stall_guard(Some(2_000), false, true).is_none(),
        "the response rebuilding an empty reserve must be allowed to complete",
    );
    assert!(
        arm_active_stall_guard(Some(2_000), false, false).is_some(),
        "a running clock still needs its terminal boundary",
    );
}

/// The reserve is spent by the PLAYHEAD, not by wall time. A server may wait arbitrarily while
/// playback is paused or re-priming without consuming one millisecond of queued media.
#[test]
fn a_fetch_the_playhead_is_not_consuming_never_aborts() {
    let _serial = crate::testlock::serial();
    let pos = 5_000_000_000i64;
    crate::player::SHARED
        .playpos_ns
        .store(pos, std::sync::atomic::Ordering::Relaxed);
    let guard = StallGuard {
        reserve_ms_at_start: 2_000,
        playhead_at_start_ns: pos,
        action: super::StallAction::AbortFetch,
    };
    assert!(
        !guard.should_abort(400_000, false),
        "the guard abandoned a fetch while the picture was not consuming its reserve"
    );
}

/// The other half, so the fix cannot be "never abort": a playhead that IS advancing spends the
/// reserve exactly as before, and a fetch that provably cannot land still aborts.
#[test]
fn a_fetch_the_playhead_is_consuming_still_aborts() {
    let _serial = crate::testlock::serial();
    let pos = 5_000_000_000i64;
    let guard = StallGuard {
        reserve_ms_at_start: 2_000,
        playhead_at_start_ns: pos,
        action: super::StallAction::AbortFetch,
    };
    crate::player::SHARED
        .playpos_ns
        .store(pos + 1_999_000_000, std::sync::atomic::Ordering::Relaxed);
    assert!(
        !guard.should_abort(400_000, false),
        "one millisecond of reserve remains"
    );
    // Exactly the two seconds of media present at the boundary have now been consumed.
    crate::player::SHARED
        .playpos_ns
        .store(pos + 2_000_000_000, std::sync::atomic::Ordering::Relaxed);
    assert!(
        guard.should_abort(400_000, false),
        "a genuinely exhausted reserve must still abort"
    );
}

/// The main thread's B=0 hold and the worker's playhead sample are the same physical boundary.
/// The explicit signal covers millisecond quantisation and a callback that resumes just after
/// Starfish was paused.
#[test]
fn a_terminal_hold_aborts_only_an_incomplete_response() {
    let _serial = crate::testlock::serial();
    crate::player::SHARED
        .playpos_ns
        .store(5_000_000_000, std::sync::atomic::Ordering::Relaxed);
    let guard = StallGuard::arm(6_000).expect("a live reserve arms");
    assert!(
        guard.should_abort(1, true),
        "B=0 with bytes remaining is terminal"
    );
    assert!(
        !guard.should_abort(0, true),
        "a complete sized response must be credited"
    );
    assert!(
        !guard.should_abort(-1, true),
        "past the declared end is complete too"
    );
}
