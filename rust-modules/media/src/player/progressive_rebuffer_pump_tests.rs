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
