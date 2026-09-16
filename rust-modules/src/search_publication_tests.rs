//! Result publication readiness and per-source failure/backoff status.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// `Ready` with nothing in it is an ANSWER and `Failed` is a fault — the distinction the screen
/// draws two different sentences from. One answer is enough: a friend's server being off must
/// not tell someone their own library's search failed.
#[test]
fn one_answer_is_ready_and_only_an_all_failed_roster_is_failed() {
    let ok = || answered(0, vec![media("x")]);
    let bad = failed;
    assert_eq!(
        state_from(&[ok(), bad()], true),
        State::Ready,
        "a dead source must not fail the others"
    );
    assert_eq!(
        state_from(&[bad(), Source::EMPTY], true),
        State::Searching,
        "one is still out"
    );
    assert_eq!(state_from(&[bad(), bad()], true), State::Failed);
    assert_eq!(
        state_from(&[], true),
        State::Failed,
        "nothing can ever answer — a spinner would never end"
    );
    assert_eq!(state_from(&[Source::EMPTY], true), State::Searching);
    // a query below MIN_QUERY was never asked, so nothing about it is pending or failed
    assert_eq!(state_from(&[bad(), bad()], false), State::Idle);
    // an answer that is genuinely empty is still an answer, ONCE nobody else is still out —
    // this is the "No results for wallace" screen
    assert_eq!(
        state_from(
            &[Source {
                status: Status::Answered,
                ..Source::EMPTY
            }],
            true
        ),
        State::Ready
    );
}

/// **"No results" must not be said over a source that has not answered yet.** Our own server
/// replies empty in 20 ms on the LAN and a friend's takes a second: publishing `Ready` on the
/// first would print "No results for wallace" and then grow a shelf underneath it. An answer
/// with CONTENT is the exception — a populated screen is only ever added to, never contradicted,
/// so those results go up without waiting on the slowest server in the house.
#[test]
fn an_empty_answer_waits_for_the_stragglers_but_a_populated_one_does_not() {
    let empty = || Source {
        status: Status::Answered,
        ..Source::EMPTY
    };
    assert_eq!(
        state_from(&[empty(), Source::EMPTY], true),
        State::Searching,
        "'No results' over a server that has not answered yet"
    );
    assert_eq!(
        state_from(
            &[answered(0, vec![media("A Close Shave")]), Source::EMPTY],
            true
        ),
        State::Ready,
        "results already in hand must not wait on the slowest server"
    );
    // …and once the straggler has failed, the empty answer IS the whole truth
    assert_eq!(state_from(&[empty(), failed()], true), State::Ready);
}

/// A dead source arms its OWN backoff and contributes nothing; the source beside it still
/// answers, and the screen reads `Ready` rather than failed. The other half of "release the
/// claim on the take": a failure must not latch the source either.
#[test]
fn a_failed_source_backs_off_alone_and_the_others_still_answer() {
    let _g = fresh();
    register(2);
    set_query("wallace");
    let gen = GEN.load(Ordering::SeqCst);

    IN_FLIGHT[0].store(true, Ordering::SeqCst);
    IN_FLIGHT[1].store(true, Ordering::SeqCst);
    land(
        0,
        gen,
        Some(answered(0, vec![media("A Close Shave")]).items),
    );
    land(1, gen, None); // the friend's server is off
    hold_off();
    assert!(pump(0.0));

    assert_eq!(
        titles(&shelves()[0]),
        ["A Close Shave"],
        "a dead source must not blank a live one"
    );
    assert_eq!(state(), State::Ready, "one answer is enough");
    assert_eq!(unsafe { (*addr_of!(SRC))[1].status }, Status::Failed);
    assert_eq!(
        unsafe { (*addr_of!(SRC))[1].retry_cd },
        RETRY_FRAMES,
        "the failed source backs off before retrying"
    );
    assert!(!IN_FLIGHT[0].load(Ordering::SeqCst) && !IN_FLIGHT[1].load(Ordering::SeqCst));
    crate::plex::reset_servers_for_test();
    reset();
}

/// The duplicate-spawn race [`IN_FLIGHT`] admits to can leave two workers out for one source at
/// one generation. If the loser's failure arrived after the winner's answer, a plain
/// `Status::Failed` would drop that source's already-drawn results out of the merge and back off
/// for two seconds — over an error about a request whose answer is already on screen.
#[test]
fn a_late_failure_cannot_unsay_an_answer_this_source_already_gave() {
    let _g = fresh();
    register(1);
    set_query("wallace");
    let gen = GEN.load(Ordering::SeqCst);
    land(
        0,
        gen,
        Some(answered(0, vec![media("A Close Shave")]).items),
    );
    hold_off();
    assert!(pump(0.0));
    assert_eq!(state(), State::Ready);

    land(0, gen, None); // the duplicate worker, finishing second
    hold_off();
    pump(0.0);
    assert_eq!(
        titles(&shelves()[0]),
        ["A Close Shave"],
        "the results vanished for two seconds"
    );
    assert_eq!(state(), State::Ready);
    assert_eq!(unsafe { (*addr_of!(SRC))[0].status }, Status::Answered);
    assert_eq!(
        unsafe { (*addr_of!(SRC))[0].retry_cd },
        RETRY_FRAMES - 1,
        "no backoff — only the sentinel ticked"
    );
    crate::plex::reset_servers_for_test();
    reset();
}
