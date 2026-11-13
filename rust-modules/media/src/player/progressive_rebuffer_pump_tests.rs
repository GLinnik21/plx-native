//! A starved direct-play (progressive) Feed stream, driven through the real `pump()` over the host
//! sink: the producer stops delivering, the sink clock plays out what it was fed, and the viewer
//! must be told "Buffering" instead of watching the sink drop late frames.
//!
//! Every case runs the whole chain, the way `end_of_film_tests` does: the feed lanes, the pump's own
//! decision to hold, the sink's Pause, the position callback, the readout. Nothing here calls the
//! hold's decision function or sets `hls_rebuffering` itself, so a test that passes has seen the
//! pump do it. The numbers are literals, not the crate's constants: a test that names the
//! threshold it grades cannot fail when the threshold is wrong.
//!
//! The host clock is a plant, and its shape bounds what these can see. It clamps the position to the
//! highest video pts it was fed and keeps reporting while it plays, so the native-clock-silence
//! arm of the begin rule cannot fire here (`native_clock_stopped`'s own tests cover that arm). What
//! the plant does reproduce is the reserve running out: the clock reaches the tail of what was fed
//! and parks there.

use super::end_of_film_tests::Rig;
use super::*;
use plx_play_feed::ffi_host;
use plx_play_feed::feed_state::PRES_NONE;
use plx_play_feed::shared::FEED_TX;
use std::sync::atomic::Ordering::Relaxed;

const S: i64 = 1_000_000_000;
const V_STEP_NS: i64 = 41_666_666; // 24 fps
const A_STEP_NS: i64 = 21_333_333; // AAC: 1024 samples at 48 kHz
/// One native position period (the callback's measured cadence): a reserve at or under it may
/// already be spent by the next callback.
const LOW_WATER_NS: i64 = 201_000_000;
/// The reserve a Play must have reached before a starvation hold is credible.
const PRIME_NS: i64 = 700_000_000;
/// Steps a run may take.
const STEP_BUDGET: usize = 120;

/// How many Pause verbs the host sink saw since the probe was reset.
fn pauses() -> usize {
    ffi_host::verb_order_for_test().iter().filter(|v| v.as_str() == "pause").count()
}

/// The pts of the last access unit a lane of `end_ns` holds, built the way `Rig` builds it.
fn last_pts(end_ns: i64, step_ns: i64) -> i64 {
    (end_ns / step_ns - 1) * step_ns
}

/// **A starved progressive stream is held and reads Buffering, through the pump.**
///
/// Both lanes hold six seconds, no EOF, and nothing more is coming: a link that stopped delivering.
/// The clock plays the six seconds out. While more than the prime depth is buffered nothing may
/// hold; once the reserve reaches one native position period the pump must have paused the sink,
/// within two pump periods, and the readout must say Buffering.
///
/// On a build whose pump returns from the hold at `!segmented_hls` no Pause is ever sent: the clock
/// parks at the tail, the sink would drop every later frame, and the viewer sees a frozen picture.
#[test]
fn a_starved_progressive_stream_pauses_the_sink_and_reads_buffering() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Rig::with_media(6 * S, 6 * S);
    ffi_host::reset_volume_probe_for_test();

    // PRECONDITIONS, asserted rather than assumed: they say a red run is the defect and not a dead
    // instrument. The route is direct play; the furthest lane's tail is within one period of the
    // tail the clock will clamp at (otherwise the hold could never begin in this plant).
    assert!(!crate::route::is_segmented_hls(&rig.ps), "PRECONDITION FAILED, not the defect: the route is HLS");
    let video_tail = last_pts(6 * S, V_STEP_NS);
    let audio_tail = last_pts(6 * S, A_STEP_NS);
    assert!(
        audio_tail - video_tail <= LOW_WATER_NS,
        "PRECONDITION FAILED, not the defect: audio ends {}ms past the video the sink clock clamps to",
        (audio_tail - video_tail) / 1_000_000
    );
    let tail = video_tail.max(audio_tail);

    let mut saw_healthy_reserve = false;
    let mut low_water_at: Option<usize> = None;
    let mut paused_at: Option<usize> = None;
    for n in 0..STEP_BUDGET {
        rig.step();
        let position = FEED_SHARED.pres_fed.load(Relaxed);
        if position == PRES_NONE {
            continue;
        }
        let reserve = tail - position;
        if reserve > PRIME_NS {
            saw_healthy_reserve = true;
            assert_eq!(pauses(), 0, "a healthy reserve ({}ms) was held: {}", reserve / 1_000_000, rig.facts());
        }
        if low_water_at.is_none() && saw_healthy_reserve && reserve <= LOW_WATER_NS {
            low_water_at = Some(n);
        }
        if paused_at.is_none() && pauses() > 0 {
            paused_at = Some(n);
        }
        if low_water_at.is_some_and(|at| n >= at + 2) {
            break;
        }
    }

    assert!(
        saw_healthy_reserve,
        "PRECONDITION FAILED, not the defect: the clock never played with a reserve: {}",
        rig.facts()
    );
    let low_water_at = low_water_at.unwrap_or_else(|| {
        panic!("PRECONDITION FAILED, not the defect: the clock never reached the tail: {}", rig.facts())
    });
    assert!(
        paused_at.is_some_and(|at| at <= low_water_at + 2),
        "the reserve reached one native period at step {low_water_at} and the sink was not paused within two \
         pump periods (first pause at step {paused_at:?}): the stream is left to drop frames. {}",
        rig.facts()
    );
    assert!(FEED_SHARED.hls_rebuffering.load(Relaxed), "the hold did not raise the Buffering mirror");
    assert_eq!(
        crate::player::rebuffering(&rig.ps),
        Some(true),
        "the attempt's facts do not say Buffering: {}",
        rig.facts()
    );
}

// ---- the rest of the contract, on the same rig: every hold below is begun by the pump's own decision ----

/// The producer delivers `from_ns..to_ns` more media on both lanes, cut the way `Rig::with_media`
/// cut the first stretch (`from_ns` is the length it was built with, a whole second).
fn deliver(rig: &mut Rig, from_ns: i64, to_ns: i64) {
    let au = [0u8; 64];
    let mut guard = rig.eng();
    let eng = &mut *guard;
    let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
    for i in from_ns / V_STEP_NS..to_ns / V_STEP_NS {
        plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as std::os::raw::c_int, i * V_STEP_NS, 0, 1);
    }
    let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
    for i in from_ns / A_STEP_NS..to_ns / A_STEP_NS {
        plx_play_feed::aq::aq_push(qa, au.as_ptr(), au.len() as std::os::raw::c_int, i * A_STEP_NS, 1, 2);
    }
}

/// `run`'s result and the event-log text it wrote.
fn logged<R>(run: impl FnOnce() -> R) -> (R, String) {
    plx_base::eventlog::with_private_log(|| {
        let result = run();
        (result, std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default())
    })
}

/// Play the first stretch out until the pump has held the stream.
fn run_until_held(rig: &mut Rig) {
    ffi_host::reset_volume_probe_for_test();
    assert!(
        rig.run_until(|_| FEED_SHARED.hls_rebuffering.load(Relaxed)),
        "PRECONDITION FAILED, not the defect: the starved stream was never held: {}",
        rig.facts()
    );
    assert_eq!(pauses(), 1, "the hold is exactly one Pause");
}

fn held_by_the_progressive_rule() -> bool {
    matches!(
        FEED_SHARED.hls_prime_kind(),
        Some(plx_play_feed::shared::HlsPrimeKind::ProgressiveRebuffer { .. })
    )
}

/// **The hold's way out (t2, and the deadlock guard).** A progressive hold has no HLS segment to
/// credit, so a certificate borrowed from the HLS hold would show Buffering for good. The producer
/// delivers again: a runway under the target keeps holding, the target releases, and the Play
/// clears Buffering and starts the clock.
#[test]
fn a_hold_releases_when_the_producer_delivers_the_runway() {
    let _serial = plx_base::testlock::serial();
    let ((), text) = logged(|| {
        let mut rig = Rig::with_media(6 * S, 6 * S);
        run_until_held(&mut rig);
        assert!(held_by_the_progressive_rule());
        let plays = ffi_host::play_calls_for_test();

        deliver(&mut rig, 6 * S, 7 * S);
        for _ in 0..8 {
            rig.step();
        }
        assert!(FEED_SHARED.hls_rebuffering.load(Relaxed), "about a second is under the runway: still holding");
        assert_eq!(ffi_host::play_calls_for_test(), plays, "no Play before the runway");

        deliver(&mut rig, 7 * S, 12 * S);
        assert!(
            rig.run_until(|_| !FEED_SHARED.hls_rebuffering.load(Relaxed)),
            "the runway arrived and the hold never released: {}",
            rig.facts()
        );
        assert_eq!(ffi_host::play_calls_for_test(), plays + 1, "one Play ends the hold");
        assert!(!held_by_the_progressive_rule());
        assert_ne!(crate::player::rebuffering(&rig.ps), Some(true), "Buffering is still up after the Play");
        let before = FEED_SHARED.pres_fed.load(Relaxed);
        for _ in 0..5 {
            rig.step();
        }
        assert!(FEED_SHARED.pres_fed.load(Relaxed) > before, "the clock runs again after the Play");
        assert_eq!(pauses(), 1, "playing the new media out did not hold again");
    });
    assert!(text.contains("progressive rebuffer hold pos="), "no hold line: {text}");
    for field in ["vtail=", "atail=", "qv=", "qa=", "reserve="] {
        assert!(text.contains(field), "the hold line lacks {field}: {text}");
    }
    assert!(text.contains("progressive rebuffer resume reason=target held="), "no resume line: {text}");
}

/// **The end of the stream during a hold (t4).** The producer finishing releases the hold without
/// the decoder prime (nothing more will arrive), and the release is not the end of it: the video
/// lane drains and the pipeline is told the stream is over, or the film hangs on its last frame.
#[test]
fn the_end_of_the_stream_releases_a_hold_and_the_eos_follows() {
    let _serial = plx_base::testlock::serial();
    let ((), text) = logged(|| {
        let mut rig = Rig::with_media(6 * S, 6 * S);
        run_until_held(&mut rig);
        assert!(!rig.eng().eos_pushed, "PRECONDITION FAILED, not the defect: the EOS went out under the hold");
        rig.producer_done();
        assert!(
            rig.run_until(|_| !FEED_SHARED.hls_rebuffering.load(Relaxed)),
            "the stream ended and the hold never released: {}",
            rig.facts()
        );
        assert!(
            rig.run_until(|r| r.eng().eos_pushed),
            "the hold released but pushEOS never followed: {}",
            rig.facts()
        );
    });
    assert!(text.contains("progressive rebuffer resume reason=eof"), "no eof resume line: {text}");
}

/// **The viewer's Pause and Resume inside a hold (t6).** The Resume must not start the clock under
/// the hold (the media it waits for has not arrived): it stays held, still reading Buffering, and
/// the hold's own certificate plays it once the runway is there.
#[test]
fn a_user_pause_and_resume_inside_a_hold_stay_held_until_the_runway() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Rig::with_media(6 * S, 6 * S);
    run_until_held(&mut rig);
    let plays = ffi_host::play_calls_for_test();

    assert!(crate::player::pause(&mut rig.ps, &mut rig.pa), "the viewer's Pause is accepted inside the hold");
    for _ in 0..3 {
        rig.step();
    }
    assert!(held_by_the_progressive_rule(), "a user Pause does not end the hold");
    assert!(crate::player::resume(&mut rig.ps, &mut rig.pa), "the viewer's Resume is accepted");
    for _ in 0..3 {
        rig.step();
    }
    assert_eq!(ffi_host::play_calls_for_test(), plays, "Resume started the clock under a hold with no runway");
    assert!(held_by_the_progressive_rule(), "Resume returned to the bare resume path, not the hold");
    assert!(FEED_SHARED.hls_rebuffering.load(Relaxed), "still Buffering");

    deliver(&mut rig, 6 * S, 12 * S);
    assert!(
        rig.run_until(|_| !FEED_SHARED.hls_rebuffering.load(Relaxed)),
        "the runway arrived and the hold never released: {}",
        rig.facts()
    );
    assert_eq!(ffi_host::play_calls_for_test(), plays + 1);
    assert!(!FEED_TX.paused.load(Relaxed), "the viewer asked for Play");
}

/// **What never holds (t5).** Each is a stream whose clock is stopped on purpose, still starting,
/// or not a stream a link can starve: zero Pauses of the pump's own.
mod never_held {
    use super::*;

    /// Run `steps` periods and return how many Pause verbs the sink saw.
    fn pauses_over(rig: &mut Rig, steps: usize) -> usize {
        for _ in 0..steps {
            rig.step();
        }
        pauses()
    }

    /// A start that never reached the prime depth stays in the existing startup handling: its first
    /// Play is still owed, so there is no clock to stall and nothing for the hold to wait on. (The
    /// arming latch itself, a Play whose reserve never rose to the prime depth, is graded in
    /// `feed_pump`'s `StallWatch` tests: the pump never Plays below the prime depth, so this rig
    /// cannot reach it.)
    #[test]
    fn a_start_that_never_reached_the_prime_depth_is_not_held() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::with_media(S / 2, S / 2);
        ffi_host::reset_volume_probe_for_test();
        assert_eq!(pauses_over(&mut rig, 40), 0, "a start that never reached the prime depth was held");
        assert!(!FEED_SHARED.hls_rebuffering.load(Relaxed), "the startup wait reads as a rebuffer: {}", rig.facts());
        assert!(!held_by_the_progressive_rule(), "the startup wait took the progressive hold: {}", rig.facts());
        assert!(
            !ffi_host::clock_state_for_test().1,
            "PRECONDITION FAILED, not the defect: the clock started on half a second: {}",
            rig.facts()
        );
    }

    /// A link that keeps up (the producer is always ahead of the clock) is never held.
    #[test]
    fn a_full_rate_stream_is_never_held() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::with_media(30 * S, 30 * S);
        ffi_host::reset_volume_probe_for_test();
        assert_eq!(pauses_over(&mut rig, 80), 0, "a stream with a full reserve was held");
        assert!(
            ffi_host::clock_state_for_test().0 > 10 * S,
            "PRECONDITION FAILED, not the defect: the clock never played: {}",
            rig.facts()
        );
    }

    /// A trailer preview is direct play too, and its stall ends the preview, never the readout.
    #[test]
    fn a_trailer_preview_is_never_held() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::with_media(6 * S, 6 * S);
        crate::route::mark_preview_for_test(&mut rig.ps);
        ffi_host::reset_volume_probe_for_test();
        assert_eq!(pauses_over(&mut rig, 45), 0, "a preview was held");
        assert!(
            ffi_host::clock_state_for_test().0 >= 5 * S,
            "PRECONDITION FAILED, not the defect: the clock never reached the tail: {}",
            rig.facts()
        );
        assert!(!FEED_SHARED.hls_rebuffering.load(Relaxed));
    }

    /// The viewer's own Pause is the only Pause a paused clock gets: running dry under it is not a
    /// starvation to hold, and the viewer who pressed Pause is not made to read Buffering.
    #[test]
    fn a_user_pause_is_not_a_starvation() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::with_media(6 * S, 6 * S);
        for _ in 0..8 {
            rig.step();
        }
        ffi_host::reset_volume_probe_for_test();
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa), "the viewer's Pause is accepted");
        let at_pause = pauses();
        assert_eq!(pauses_over(&mut rig, 40), at_pause, "a paused stream was held a second time");
        assert!(!FEED_SHARED.hls_rebuffering.load(Relaxed), "a paused stream reads Buffering");
    }
}

/// **The hold is invisible to the Auto watchdog's inputs (t7).** The progressive Original watchdog
/// (`adaptive::ProgressiveOriginalSession::observe`, fed by the demux worker) decides a fallback to
/// HLS from the bytes delivered, the demuxed reserve, the remaining playback and the VIEWER's pause
/// (`FEED_TX.pause_state_sample`, the pause lane). A hold sets none of them: it is not a viewer
/// Pause, so the watchdog neither skips its quanta nor restarts its resume window, and a sustained
/// deficit is still read as one. This pins the inputs the hold could have disturbed; the decision
/// itself is graded where it lives (`route::decision`, `abr`).
#[test]
fn a_hold_does_not_read_as_a_viewer_pause_to_the_auto_watchdog() {
    let _serial = plx_base::testlock::serial();
    let mut rig = Rig::with_media(6 * S, 6 * S);
    let before = FEED_TX.pause_state_sample();
    run_until_held(&mut rig);
    let during = FEED_TX.pause_state_sample();
    assert_eq!(
        (during.paused, during.sequence),
        (before.paused, before.sequence),
        "the internal hold is visible on the viewer's pause lane"
    );
    assert!(!FEED_SHARED.seeking.load(Relaxed), "the hold reads as a seek in flight");
}
