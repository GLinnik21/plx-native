//! **A viewer Play inside a held prime reaches the clock.** The Feed driver answered a Play
//! `Queued` and enqueued nothing while `Engine::prime_play` was armed, so a viewer Pause that had
//! landed on a held prime (`ClockHold::Initial`/`Seek`/`Rebuffer`/`ResumePrime`: the Pause only
//! marks the clock `user_held`) was never undone: the contract recorded `viewer_paused = false`, the
//! clock stayed user-held, `reserve_hls_prime_play` refused the prime for good and every later Play
//! was a no-op until a seek or a stop cleared the hold (seen on the TV during a progressive-rebuffer
//! hold, `run-e-diag`).
//!
//! Every row enters at the viewer's door, `transport::set_transport_paused`, over an Engine the
//! common runner started (`startup_controlled`, an attempt Active in the runner): a fixture Engine
//! installed with `PlayerAdapter::install` has no attempt, the door falls back to `player::resume`
//! and the driver's dispatch never runs, which is how a test that called `player::pause` /
//! `resume` directly passed over this bug.
//!
//! Each hold is graded with and without a preceding viewer Pause. A Play with no Pause behind it
//! never reaches the driver through the viewer's door (the door answers it satisfied), so its rows
//! pin that nothing moves; the claim hold's give-back (`claim_hold::release`) is the caller that
//! sends a Play through `backend_transport_control` whatever the viewer's goal reads, and its row
//! pins that a Play with nothing to resume is not refused and does not disturb the prime.

use super::flight_tests::{assert_scrub_reload_plays, fill_both_lanes, hls_rig, Rig};
use plx_play_feed::feed_state::Engine;
use plx_play_feed::ffi_host::{clock_state_for_test, play_calls_for_test};
use plx_play_feed::shared::{HlsPauseCompletion, HlsPrimeKind, HlsSeekPause, FEED_SHARED};
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

/// The viewer's Pause (`true`) or Play (`false`) key, through the door every key takes.
fn press(rig: &mut Rig, paused: bool) -> bool {
    crate::player::transport::set_transport_paused(&mut rig.ps, &mut rig.pa, paused)
}

fn eng(rig: &mut Rig) -> std::cell::RefMut<'_, Engine> { rig.pa.engine().expect("engine") }

fn user_held() -> bool { FEED_SHARED.clock_user_held_for_test() }

fn prime_kind() -> Option<HlsPrimeKind> { FEED_SHARED.hls_prime_kind() }

/// The contract's answer to "is the viewer paused" (what the next key is compared with).
fn contract_paused(rig: &Rig) -> bool {
    crate::player::transport::viewer_paused_for(&rig.ps, &rig.pa)
}

/// Frames until `done`, bounded (the Load, the worker's landing and the runner's poll are real
/// threads and ticks).
fn frames_until_ok(rig: &mut Rig, what: &str, done: impl Fn(&mut Rig) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done(rig) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(2));
        rig.frame();
    }
}

/// What every row asserts about the prime BEFORE it presses a key, so a row cannot pass on main
/// for the wrong reason: the runner owns an Active Feed attempt over a startup-controlled Engine
/// whose Play has been commanded, a prime is armed, and the clock is held by `kind`.
fn assert_in_prime(rig: &mut Rig, kind: HlsPrimeKind, what: &str) {
    assert_eq!(rig.pa.backend_snapshot().phase, plx_playback_contract::Phase::Active,
        "{what}: PRECONDITION: the common runner owns an Active attempt");
    let (controlled, commanded, prime) = {
        let e = eng(rig);
        (e.startup_controlled, e.startup_play_commanded, e.prime_play)
    };
    assert!(controlled, "{what}: PRECONDITION: the Engine is startup-controlled (a runner start)");
    assert!(commanded, "{what}: PRECONDITION: the startup Play was commanded (the driver's \
        `else` arm is the one a viewer Play takes)");
    let facts = {
        let e = eng(rig);
        format!("immediate={} user_held_flag={} stage={:?} flushed={} rebase={} clock_user_held={} kind={:?} plays={} vbuf_ms={} abuf_ms={} feed_allowed={}",
            e.startup_immediate_play, e.startup_user_held, e.stage, e.flushed, e.rebase_pending,
            user_held(), prime_kind(), play_calls_for_test(),
            (e.max_fed_video_pts - e.seek_base_pts) / 1_000_000,
            (e.max_fed_audio_pts - e.seek_base_pts) / 1_000_000,
            plx_play_feed::shared::FEED_TX.feed_allowed())
    };
    assert!(prime, "{what}: PRECONDITION: a prime is armed ({facts})");
    assert_eq!(prime_kind(), Some(kind), "{what}: PRECONDITION: the clock is held by this prime");
}

/// The state a refused or fenced first Play leaves (`SMP Play fenced; priming before retry`): the
/// runner's startup Play is commanded, the clock is held by `hold`, the lanes hold nothing and the
/// prime owes the sink's Play. A real reloaded start plays at once in this rig (the landing's resume
/// fires `play_initial_now` on the host sink), so the hold is put back with the hooks the engine
/// tests use for the same states, over the runner-started Engine. Returns the sink's Play count.
fn held_prime(rig: &mut Rig, hold: Hold) -> u64 {
    let plays = running_stream(rig);
    match hold {
        Hold::Initial => FEED_SHARED.ensure_initial_clock_hold_for_test(),
        Hold::Seek => {
            let Some(HlsSeekPause::Issue(token)) = FEED_SHARED.prepare_seek_pause() else {
                panic!("PRECONDITION: a running clock issues the seek's pause");
            };
            assert_eq!(FEED_SHARED.complete_seek_pause(token, true), HlsPauseCompletion::Accepted);
        }
    }
    let mut e = eng(rig);
    e.prime_play = true;
    e.max_fed_video_pts = e.seek_base_pts;
    e.max_fed_audio_pts = e.seek_base_pts;
    drop(e);
    plays
}

#[derive(Clone, Copy)]
enum Hold { Initial, Seek }

/// A running clock on the primed start: Play reached the sink, the clock advances.
fn running_stream(rig: &mut Rig) -> u64 {
    assert_scrub_reload_plays(rig, true);
    play_calls_for_test()
}

/// A Play was swallowed: the clock is still user-held and nothing played.
fn assert_play_reached_the_clock(rig: &mut Rig, plays_before: u64, what: &str) {
    assert!(!user_held(), "{what}: the viewer's Play did not reach the held clock: it is still \
        user-held, so `reserve_hls_prime_play` refuses the prime for good");
    assert_eq!(contract_paused(rig), user_held(),
        "{what}: the contract's viewer_paused disagrees with the clock's user hold");
    assert!(!contract_paused(rig), "{what}: the viewer pressed Play");
    assert_eq!(play_calls_for_test(), plays_before,
        "{what}: the sink played before the prime's media was ready (the prime owns the Play)");
    assert!(eng(rig).prime_play, "{what}: the prime stays armed: it, not the key, issues the Play");
}

/// The prime then releases once its media is there, with exactly one physical Play.
fn assert_prime_then_plays_once(rig: &mut Rig, plays_before: u64, what: &str) {
    frames_until_ok(rig, &format!("{what}: the prime to release"), |_| {
        play_calls_for_test() > plays_before
    });
    for _ in 0..5 { rig.frame(); }
    assert_eq!(play_calls_for_test(), plays_before + 1, "{what}: exactly one physical Play");
    assert!(!eng(rig).prime_play, "{what}: the prime released");
    assert!(!user_held(), "{what}: the clock runs for the viewer");
    assert!(clock_state_for_test().1, "{what}: the sink clock runs");
}

// ---------------------------------------------------------------------------------------------
// The initial prime (startup).
// ---------------------------------------------------------------------------------------------

#[test]
fn held_prime_play_initial_prime_with_a_viewer_pause_then_play() {
    let mut rig = hls_rig(60);
    let plays = held_prime(&mut rig, Hold::Initial);
    assert_in_prime(&mut rig, HlsPrimeKind::Fresh, "initial prime");

    assert!(press(&mut rig, true), "the viewer's Pause is accepted");
    assert!(user_held(), "the Pause landed on the held prime: the clock is user-held");
    assert!(press(&mut rig, false), "the viewer's Play is accepted");

    assert_play_reached_the_clock(&mut rig, plays, "initial prime");
    fill_both_lanes(&mut rig);
    assert_prime_then_plays_once(&mut rig, plays, "initial prime");
}

#[test]
fn held_prime_play_initial_prime_without_a_pause_is_inert() {
    let mut rig = hls_rig(60);
    let plays = held_prime(&mut rig, Hold::Initial);
    assert_in_prime(&mut rig, HlsPrimeKind::Fresh, "initial prime, no Pause");

    assert!(press(&mut rig, false), "a Play on a playing viewer is already satisfied");
    assert!(!user_held());
    assert_eq!(play_calls_for_test(), plays, "no physical Play");
    assert!(eng(&mut rig).prime_play, "the prime is untouched");
    fill_both_lanes(&mut rig);
    assert_prime_then_plays_once(&mut rig, plays, "initial prime, no Pause");
}

/// The claim hold's give-back sends a Play through the backend control with no regard to the
/// viewer's goal. With a prime armed and nothing user-held there is nothing to resume: the Play is
/// not refused (`claim_hold::release` would log "could not give play back") and the prime is not
/// disturbed.
#[test]
fn held_prime_play_a_backend_play_with_nothing_held_is_not_refused() {
    let mut rig = hls_rig(60);
    let plays = held_prime(&mut rig, Hold::Initial);
    assert_in_prime(&mut rig, HlsPrimeKind::Fresh, "backend Play, nothing held");

    let accepted = rig.pa.backend_transport_control(&mut rig.ps, false);
    assert_eq!(accepted, Some(true), "a Play with nothing to resume is answered, not refused");
    assert!(!user_held());
    assert_eq!(play_calls_for_test(), plays, "no physical Play");
    assert!(eng(&mut rig).prime_play, "the prime is untouched");
    fill_both_lanes(&mut rig);
    assert_prime_then_plays_once(&mut rig, plays, "backend Play, nothing held");
}

// ---------------------------------------------------------------------------------------------
// The HLS internal hold (`ClockHold::Rebuffer`).
// ---------------------------------------------------------------------------------------------

/// A running primed stream whose clock the internal controller has paused (the engine tests'
/// recipe: the hostsim recovery entry, the mirror, and the Engine's armed prime).
fn internal_rebuffer(rig: &mut Rig) -> u64 {
    let plays = running_stream(rig);
    FEED_SHARED.hls_prime_runway_ms.store(500, Relaxed);
    FEED_SHARED.begin_hls_recovery();
    FEED_SHARED.hls_rebuffering.store(true, Relaxed);
    eng(rig).prime_play = true;
    plays
}

fn release_rebuffer_media() {
    FEED_SHARED.observe_hls_recovery(500_000, Duration::from_secs(2));
}

#[test]
fn held_prime_play_internal_rebuffer_with_a_viewer_pause_then_play() {
    let mut rig = hls_rig(60);
    let plays = internal_rebuffer(&mut rig);
    assert_in_prime(&mut rig, HlsPrimeKind::Rebuffer, "internal rebuffer");

    assert!(press(&mut rig, true), "the viewer's Pause is accepted");
    assert!(user_held(), "the Pause landed on the internal hold: the clock is user-held");
    assert!(press(&mut rig, false), "the viewer's Play is accepted");

    assert_play_reached_the_clock(&mut rig, plays, "internal rebuffer");
    assert_eq!(prime_kind(), Some(HlsPrimeKind::Rebuffer), "the internal hold keeps its certificate");
    release_rebuffer_media();
    assert_prime_then_plays_once(&mut rig, plays, "internal rebuffer");
}

#[test]
fn held_prime_play_internal_rebuffer_without_a_pause_is_inert() {
    let mut rig = hls_rig(60);
    let plays = internal_rebuffer(&mut rig);
    assert_in_prime(&mut rig, HlsPrimeKind::Rebuffer, "internal rebuffer, no Pause");

    assert!(press(&mut rig, false), "a Play on a playing viewer is already satisfied");
    assert!(!user_held());
    assert_eq!(play_calls_for_test(), plays);
    assert!(eng(&mut rig).prime_play, "the prime is untouched");
    release_rebuffer_media();
    assert_prime_then_plays_once(&mut rig, plays, "internal rebuffer, no Pause");
}

// ---------------------------------------------------------------------------------------------
// The viewer's own resume prime (`ClockHold::ResumePrime`).
// ---------------------------------------------------------------------------------------------

/// Pause, then Play: the Play arms the resume prime (nothing is pressed between, so no frame has
/// released it).
fn resume_prime(rig: &mut Rig) -> u64 {
    let plays = running_stream(rig);
    assert!(press(rig, true), "the viewer's Pause is accepted");
    assert!(user_held());
    assert!(press(rig, false), "the viewer's Play is accepted");
    assert!(!user_held(), "the Play moved the clock's ownership to the resume prime");
    plays
}

#[test]
fn held_prime_play_resume_prime_with_a_second_pause_then_play() {
    let mut rig = hls_rig(60);
    let plays = resume_prime(&mut rig);
    // `assert_in_prime` wants a commanded startup Play; the running stream's was, then released.
    assert!(eng(&mut rig).prime_play, "PRECONDITION: the resume prime is armed");
    assert_eq!(prime_kind(), Some(HlsPrimeKind::Resume), "PRECONDITION: the clock is held by the resume prime");

    assert!(press(&mut rig, true), "the viewer's second Pause is accepted");
    assert!(user_held(), "the Pause landed on the resume prime: the clock is user-held");
    assert!(press(&mut rig, false), "the viewer's second Play is accepted");

    assert_play_reached_the_clock(&mut rig, plays, "resume prime");
    assert_prime_then_plays_once(&mut rig, plays, "resume prime");
}

#[test]
fn held_prime_play_resume_prime_without_a_second_pause_is_inert() {
    let mut rig = hls_rig(60);
    let plays = resume_prime(&mut rig);
    assert!(eng(&mut rig).prime_play, "PRECONDITION: the resume prime is armed");
    assert_eq!(prime_kind(), Some(HlsPrimeKind::Resume));

    assert!(press(&mut rig, false), "a Play on a playing viewer is already satisfied");
    assert!(!user_held());
    assert_eq!(play_calls_for_test(), plays);
    assert_prime_then_plays_once(&mut rig, plays, "resume prime, no second Pause");
}

// ---------------------------------------------------------------------------------------------
// The seek prime (`ClockHold::Seek`).
// ---------------------------------------------------------------------------------------------

#[test]
fn held_prime_play_seek_prime_with_a_pause_after_the_seek_then_play() {
    let mut rig = hls_rig(60);
    let plays = held_prime(&mut rig, Hold::Seek);
    assert_in_prime(&mut rig, HlsPrimeKind::Fresh, "seek prime, Pause after the seek");

    assert!(press(&mut rig, true), "the viewer's Pause is accepted");
    assert!(user_held(), "the Pause landed on the seek prime: the clock is user-held");
    assert!(press(&mut rig, false), "the viewer's Play is accepted");

    assert_play_reached_the_clock(&mut rig, plays, "seek prime, Pause after the seek");
    fill_both_lanes(&mut rig);
    assert_prime_then_plays_once(&mut rig, plays, "seek prime, Pause after the seek");
}

/// The viewer pauses, THEN seeks: the seek takes the hold over from the user (`user_held` is
/// cleared and the prime owns the clock), and the viewer's Play must still open the feed gate.
#[test]
fn held_prime_play_seek_prime_with_a_pause_before_the_seek_then_play() {
    let mut rig = hls_rig(60);
    let plays = running_stream(&mut rig);
    assert!(press(&mut rig, true), "the viewer's Pause is accepted");
    assert!(user_held());
    assert!(matches!(FEED_SHARED.prepare_seek_pause(), Some(HlsSeekPause::AlreadyHeld)),
        "the seek reuses the user's physical Pause");
    {
        let mut e = eng(&mut rig);
        e.prime_play = true;
        e.max_fed_video_pts = e.seek_base_pts;
        e.max_fed_audio_pts = e.seek_base_pts;
    }
    // What `paused_seek_step` does for a seek dispatched under a viewer Pause: the one-frame
    // preroll opens (the transport gate stays shut for the viewer, the keyframe may pass).
    assert!(plx_play_feed::shared::FEED_TX.own_paused_seek(905_000_000_000), "the preroll opens");
    assert_in_prime(&mut rig, HlsPrimeKind::Fresh, "seek prime, Pause before the seek");
    assert!(!user_held(), "the seek moved the hold from the viewer to the prime");
    assert!(contract_paused(&rig), "the viewer is still paused: the seek did not unpause");
    assert!(plx_play_feed::shared::FEED_TX.paused.load(std::sync::atomic::Ordering::Acquire),
        "the transport gate is shut for the viewer's Pause");

    assert!(press(&mut rig, false), "the viewer's Play is accepted");

    assert!(!contract_paused(&rig), "the viewer pressed Play");
    assert!(!plx_play_feed::shared::FEED_TX.paused.load(std::sync::atomic::Ordering::Acquire),
        "the viewer's Play did not open the transport gate: the feed never runs and the prime \
         never completes while the contract says the film plays");
    assert_eq!(play_calls_for_test(), plays, "the prime owns the physical Play");
    fill_both_lanes(&mut rig);
    assert_prime_then_plays_once(&mut rig, plays, "seek prime, Pause before the seek");
}

#[test]
fn held_prime_play_seek_prime_without_a_pause_is_inert() {
    let mut rig = hls_rig(60);
    let plays = held_prime(&mut rig, Hold::Seek);
    assert_in_prime(&mut rig, HlsPrimeKind::Fresh, "seek prime, no Pause");

    assert!(press(&mut rig, false), "a Play on a playing viewer is already satisfied");
    assert!(!user_held());
    assert_eq!(play_calls_for_test(), plays);
    assert!(eng(&mut rig).prime_play, "the prime is untouched");
    fill_both_lanes(&mut rig);
    assert_prime_then_plays_once(&mut rig, plays, "seek prime, no Pause");
}
