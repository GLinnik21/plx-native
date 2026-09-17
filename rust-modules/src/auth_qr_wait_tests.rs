//! QR/PIN sign-in wait tests: activation settlement, and the poll loop for issue #30
//! (deadline-safe backoff, superseded flows, and pin-lifetime clamping).

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn only_a_live_qr_attempt_can_settle_as_an_activation() {
    let mut qr_attempt = true;
    assert!(settle_signin(&mut qr_attempt));
    assert!(!qr_attempt);
    assert!(
        !settle_signin(&mut qr_attempt),
        "a retry or duplicate completion is not activation"
    );

    let mut stored_session_discovery = false;
    assert!(!settle_signin(&mut stored_session_discovery));
}

// ---- the QR sign-in that could not end (issue #30) ----

/// **A press may only act on the wait the screen actually timed**, and both halves of that
/// identity are a defect that was live for one review round.
///
/// The PHASE: the sign-in screen offers its escape in the DRAW and takes it on the next key,
/// and between those the poll can return a token and walk the flow to `Ready` — where a
/// restart mints a fresh pin over a sign-in that had just succeeded. Worse, the two escapes
/// share one clock, so a wait that moved `Waiting → Discovering` made the QR predicate false
/// and the STALLED-SPINNER one true, on the dead code's timer, down what used to be an
/// unguarded path.
///
/// The CODE: a wait is now replaced automatically without the phase changing, so a press timed
/// against the code that expired would throw away the one that replaced it a moment ago.
#[test]
fn a_restart_acts_only_on_the_wait_that_earned_it() {
    // the rule, as a table
    let timed = (Phase::Waiting, 7u64);
    assert!(restart_permitted(Some(timed), timed));
    assert!(
        !restart_permitted(Some(timed), (Phase::Ready, 7)),
        "the sign-in succeeded between the draw and the key"
    );
    assert!(
        !restart_permitted(Some(timed), (Phase::Discovering, 7)),
        "…or merely moved on, which the OTHER escape would have accepted on this same clock"
    );
    assert!(
        !restart_permitted(Some(timed), (Phase::Waiting, 8)),
        "the code was replaced automatically while the key was in flight"
    );
    assert!(
        restart_permitted(None, (Phase::Ready, 99)),
        "the settled read-out's own control has no live wait to be wrong about"
    );
}

/// **One `SignInStarted` per attempt**, which `diag::schema` states as a contract: a start is
/// bracketed by exactly one completed/failed/cancelled. Both of the sign-in screen's timed
/// escapes restart a wait that is still UNSETTLED, so reporting a second start against the one
/// settle that eventually follows would leave every stalled sign-in over-counted.
#[test]
fn restarting_a_live_wait_is_the_same_attempt_carrying_on() {
    assert!(
        !restart_is_a_new_attempt(true),
        "a stalled spinner or an unscanned code is already being counted"
    );
    assert!(
        restart_is_a_new_attempt(false),
        "…while an error read-out has reported its failure and the next press opens a new \
         bracket"
    );
}

/// A scripted pin: answers from a list, and a clock that moves only when the loop waits or
/// polls. A fifteen-minute pin therefore runs to its death in microseconds.
struct ScriptedPin {
    answers: std::collections::VecDeque<PinPoll>,
    clock: Duration,
    waits: Vec<Duration>,
    /// The wait (by index) at which a newer flow takes the screen.
    superseded_at: Option<usize>,
    polls: usize,
    /// What one request costs. Settable because it is the axis the old iteration count was
    /// blind to, and because `net::API` lets one poll cost 25 s.
    poll_cost: Duration,
}

impl ScriptedPin {
    fn new(answers: Vec<PinPoll>) -> ScriptedPin {
        ScriptedPin {
            answers: answers.into(),
            clock: Duration::ZERO,
            waits: Vec::new(),
            superseded_at: None,
            polls: 0,
            poll_cost: Duration::from_millis(300),
        }
    }
}

impl PinWatch for ScriptedPin {
    fn poll(&mut self) -> PinPoll {
        self.polls += 1;
        // A poll costs a round trip. That cost is the whole of the second defect: the old loop
        // counted ITERATIONS and paid this on top of every one of them, so its window was
        // always longer than the pin it was watching — by minutes on a healthy link and by
        // hours against `net::API`'s 25 s deadline.
        self.clock += self.poll_cost;
        self.answers.pop_front().unwrap_or(PinPoll::Unreachable)
    }
    fn wait(&mut self, d: Duration) -> bool {
        if self.superseded_at == Some(self.waits.len()) {
            return false;
        }
        self.waits.push(d);
        self.clock += d;
        true
    }
    fn elapsed(&self) -> Duration {
        self.clock
    }
}

/// **The wait ends with the pin, not some multiple of it.**
///
/// plex.tv mints a code with `expiresIn: 900` and answers a poll of a dead one with
/// `404 {"code":1020,"message":"Code not found or expired"}` (both measured against the live
/// service, 2026-09-03). The loop this replaced was bounded at 450 ITERATIONS, each costing a
/// 2 s sleep plus a round trip — 1035 s at a fast 300 ms RTT, and 12150 s if every poll ran to
/// `net::API`'s 25 s deadline. All of that time was spent on a screen that said "Waiting for
/// you to sign in…" over a code nothing could ever authorize.
#[test]
fn the_wait_for_one_code_cannot_outlive_that_code() {
    let window = pin_window(900);
    assert_eq!(window, Duration::from_secs(900), "plex.tv's own expiresIn");

    // nothing ever answers: the pathological case, and the one that used to run for hours
    let mut w = ScriptedPin::new(Vec::new());
    assert_eq!(poll_for_token(&mut w, window), PollEnd::Expired);
    assert!(
        w.clock >= window,
        "it did wait out the code it was given, rather than giving up early"
    );
    assert!(
        w.clock <= window + w.poll_cost,
        "…and overran it by at most the ONE request that was in flight when the deadline \
         passed — the clamped pauses land the last poll exactly on it — not by a whole \
         backoff, and certainly not by the 1035s the iteration count allowed"
    );
}

/// **Exactly one poll may cross the deadline.** The request that began before expiry is always
/// allowed to answer — that is the token-losing bug above — but a flag computed BEFORE the
/// poll cannot see what the poll itself cost, so a 25 s request starting at 899 s left the
/// loop believing it was still inside the window and issuing a second one. The replacement
/// code is then another 25 s late, on a screen whose whole complaint is waiting.
#[test]
fn a_poll_that_itself_crosses_the_deadline_is_the_last_one() {
    let mut w = ScriptedPin::new(vec![PinPoll::Unreachable]);
    w.poll_cost = Duration::from_secs(25); // `net::API`'s whole-transfer deadline
    assert_eq!(
        poll_for_token(&mut w, Duration::from_secs(20)),
        PollEnd::Expired
    );
    assert_eq!(
        w.polls, 1,
        "the request in flight answered, and nothing was asked after it"
    );

    // …and the same crossing poll still hands over a token it was carrying.
    let mut w = ScriptedPin::new(vec![PinPoll::Authorized("account-token".into())]);
    w.poll_cost = Duration::from_secs(25);
    assert_eq!(
        poll_for_token(&mut w, Duration::from_secs(20)),
        PollEnd::Token("account-token".into())
    );
}

/// A pin plex.tv has forgotten ends the wait AT ONCE. There is nothing left to poll for, and
/// the code on screen is unscannable — every second spent on it is a second the user is being
/// asked to try something that cannot work.
#[test]
fn a_code_plex_tv_no_longer_knows_ends_the_wait_at_once() {
    let mut w = ScriptedPin::new(vec![PinPoll::Pending, PinPoll::Pending, PinPoll::Gone]);
    assert_eq!(poll_for_token(&mut w, pin_window(900)), PollEnd::Expired);
    assert_eq!(w.polls, 3);
    assert!(
        w.clock < Duration::from_secs(30),
        "the 404 is an ending, not another two seconds of hope"
    );
}

/// **A transport failure is not an ending, and it is not a reason to hammer the network.**
/// The old loop carried on too — but at a flat 2 s and with nothing in the log, so a poll that
/// had stopped being answered and a poller that had stopped existing produced identical
/// evidence on the one screen where they are the whole question.
#[test]
fn a_run_of_unanswered_polls_backs_off_and_keeps_going() {
    let mut w = ScriptedPin::new(vec![
        PinPoll::Unreachable,
        PinPoll::Unreachable,
        PinPoll::Unreachable,
        PinPoll::Unreachable,
        PinPoll::Authorized("account-token".into()),
    ]);
    assert_eq!(
        poll_for_token(&mut w, pin_window(900)),
        PollEnd::Token("account-token".into()),
        "the token still arrives — backing off never abandons the pin"
    );
    assert_eq!(
        w.waits,
        vec![
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(8),
            Duration::from_secs(16),
            Duration::from_secs(16),
        ],
        "2s while healthy, doubling per consecutive miss, capped so the backoff cannot \
         swallow what is left of the pin"
    );
}

/// **The deadline may not cancel a poll, and this is the review finding that mattered.**
///
/// Reachable, and it is the reported symptom exactly: at 889 s a miss has pushed the backoff
/// to 16 s; the user authorizes at 895 s and their phone says *Account linked*; the wait ends
/// at 905 s. A loop that consults its clock BEFORE polling declares expiry there and throws
/// away a token that was sitting in the very next response. So the pause is clamped to what is
/// left of the code and the poll after it always happens — only plex.tv gets to say a pin is
/// finished before we have asked once more.
#[test]
fn an_authorization_that_lands_during_the_last_backoff_is_still_collected() {
    let window = Duration::from_secs(20);
    let mut w = ScriptedPin::new(vec![
        PinPoll::Unreachable, // t=2.0  -> 2.3, backoff 4
        PinPoll::Unreachable, // t=6.3  -> 6.6, backoff 8
        PinPoll::Unreachable, // t=14.6 -> 14.9, backoff 16 — which would end at 30.9
        PinPoll::Authorized("account-token".into()),
    ]);
    assert_eq!(
        poll_for_token(&mut w, window),
        PollEnd::Token("account-token".into()),
        "the uncapped 16s backoff would have overrun the window and reported Expired \
         WITHOUT asking, dropping a token the user had already authorized"
    );
    assert_eq!(
        w.waits.last(),
        Some(&Duration::from_secs_f64(20.0 - 14.9)),
        "the last pause is exactly what was left of the code, not the full backoff"
    );
    assert_eq!(w.polls, 4, "and the poll at the deadline really happened");
}

/// The ceiling on automatic replacement, and the fact that reaching it is not a dead end: the
/// flow lands on `Error`, which is the phase the sign-in screen has always drawn a retry on.
#[test]
fn automatic_replacement_is_bounded_and_ends_somewhere_with_a_way_out() {
    assert!(another_code_allowed(1), "the code a sign-in opens with");
    assert!(another_code_allowed(MAX_PIN_GENERATIONS - 1));
    assert!(
        !another_code_allowed(MAX_PIN_GENERATIONS),
        "a television left on this screen must stop polling plex.tv eventually"
    );
    assert!(
        MAX_PIN_GENERATIONS >= 2,
        "one code is the behaviour being fixed"
    );
}

/// One answer puts the cadence back. A phone tap is judged at 2 s, and a single bad moment
/// half an hour ago must not still be costing sixteen seconds of it.
#[test]
fn an_answer_restores_the_two_second_cadence() {
    let mut w = ScriptedPin::new(vec![
        PinPoll::Unreachable,
        PinPoll::Pending,
        PinPoll::Authorized("t".into()),
    ]);
    assert_eq!(
        poll_for_token(&mut w, pin_window(900)),
        PollEnd::Token("t".into())
    );
    assert_eq!(
        w.waits,
        vec![
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(2)
        ]
    );
}

/// A superseded flow stops without polling and without a word: the successor owns the screen,
/// and two workers narrating one sign-in is how a log stops being readable.
#[test]
fn a_superseded_flow_stops_silently_and_immediately() {
    let mut w = ScriptedPin::new(vec![PinPoll::Authorized("never-read".into())]);
    w.superseded_at = Some(0);
    assert_eq!(poll_for_token(&mut w, pin_window(900)), PollEnd::Superseded);
    assert_eq!(w.polls, 0);
}

/// The window is the pin's own lifetime, floored against a plex.tv that omits the field and
/// ceilinged by this app's patience for one code.
#[test]
fn a_codes_lifetime_is_read_from_the_pin_and_clamped_at_both_ends() {
    assert_eq!(pin_window(900), Duration::from_secs(900));
    assert_eq!(
        pin_window(0),
        Duration::from_secs(60),
        "a missing expiresIn"
    );
    assert_eq!(pin_window(-7), Duration::from_secs(60), "or a nonsense one");
    assert_eq!(pin_window(86_400), Duration::from_secs(1800));
}
