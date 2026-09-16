//! Prime-decision and playback-session classification tests: clip queue rows, control-
//! snapshot expiry, and prime refusal causes.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn a_clip_queue_row_does_not_arm_up_next() {
    let clip = crate::plex::QueueRow {
        kind: "clip".into(),
        rk: "9".into(),
        part: "/p".into(),
        ..Default::default()
    };
    assert!(up_next_of(&clip).is_none());
    let movie = crate::plex::QueueRow {
        kind: "movie".into(),
        rk: "1".into(),
        ..Default::default()
    };
    assert!(up_next_of(&movie).is_none());
}


#[test]
fn a_route_change_wins_over_an_expired_control_snapshot() {
    assert!(matches!(
        classify_prime_decision(false, crate::plex::JsonDeadlineOutcome::Deadline),
        Err(PrimeRefusal::Session),
    ));
}


#[test]
fn prime_refusals_follow_the_issued_cause_not_the_clock_at_return() {
    let response = |status, body: &[u8]| crate::plex::JsonDeadlineOutcome::Response {
        reply: crate::http::Reply {
            status,
            body: body.to_vec(),
        },
        parsed: None,
    };
    assert!(matches!(
        classify_prime_decision(true, response(500, b"nope")),
        Err(PrimeRefusal::Control),
    ));
    assert!(matches!(
        classify_prime_decision(true, response(200, b"not-json")),
        Err(PrimeRefusal::Control),
    ));
    assert!(matches!(
        classify_prime_decision(true, crate::plex::JsonDeadlineOutcome::Transport),
        Err(PrimeRefusal::Control),
    ));
    assert!(matches!(
        classify_prime_decision(true, crate::plex::JsonDeadlineOutcome::Deadline),
        Err(PrimeRefusal::Deadline),
    ));
    assert!(matches!(
        classify_prime_decision(false, response(500, b"nope")),
        Err(PrimeRefusal::Session),
    ));
    assert!(matches!(
        classify_prime_decision(false, crate::plex::JsonDeadlineOutcome::Transport),
        Err(PrimeRefusal::Session),
    ));
}

