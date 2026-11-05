//! Pure derivations the Feed backend publishes into a readout: which subsystem stopped, and how
//! much runway the queues hold. Both read worker signals and queue tails the caller has already
//! loaded (`FEED_SHARED`), so they own no state and perform no IO. The common player names them
//! for its own readout (`live_runtime_failure`); `feed_pump::native_attempt_facts` names them for
//! the addressed attempt's facts.

use plx_playback_contract::readout::RuntimeFailure;

/// PURE: turn the four terminal worker signals into one cause. More specific downstream evidence
/// wins over the generic producer flag when concurrent teardown makes more than one bit visible.
/// `load_timed_out` is checked FIRST and independently of `load_failed`, even though `pump.rs`
/// always sets both together on a budget expiry: the distinction this function exists to make is
/// "did the pipeline refuse this, or did it never answer at all", and only the timed-out signal
/// can tell the two apart.
pub fn runtime_failure(
    demux_failed: bool,
    io_failed: bool,
    load_failed: bool,
    load_timed_out: bool,
) -> RuntimeFailure {
    if load_timed_out {
        RuntimeFailure::LoadTimeout
    } else if load_failed {
        RuntimeFailure::TvPipeline
    } else if io_failed {
        RuntimeFailure::PlaybackInterrupted
    } else if demux_failed {
        RuntimeFailure::MediaSource
    } else {
        RuntimeFailure::Unknown
    }
}

/// One physical reserve definition for every delivery mode. Direct-file tails already use movie
/// time and have `display_base_ns == 0`; segmented HLS and offset progressive transcodes publish a
/// zero-based tail and carry the movie offset in `display_base_ns`. Adding that base universally
/// makes all three shapes land in the same timeline without a route-specific heuristic.
pub fn playable_buffer_ms(
    video_tail_ns: i64,
    audio_tail_ns: i64,
    audio_expected: bool,
    display_base_ns: i64,
    playpos_ns: i64,
) -> Option<i64> {
    if video_tail_ns < 0 {
        return None;
    }
    let tail_ns = if audio_expected {
        if audio_tail_ns < 0 {
            return None;
        }
        video_tail_ns.min(audio_tail_ns)
    } else {
        video_tail_ns
    };
    Some(
        tail_ns
            .saturating_add(display_base_ns.max(0))
            .saturating_sub(playpos_ns.max(0))
            .max(0)
            / 1_000_000,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playable_buffer_uses_one_movie_timeline_for_every_transport() {
        assert_eq!(
            playable_buffer_ms(70_000_000_000, 69_500_000_000, true, 0, 60_000_000_000,),
            Some(9_500),
            "a direct file publishes absolute movie timestamps",
        );
        assert_eq!(
            playable_buffer_ms(
                4_000_000_000,
                3_500_000_000,
                true,
                120_000_000_000,
                122_000_000_000,
            ),
            Some(1_500),
            "an HLS or offset-transcode tail is translated by its display base",
        );
        assert_eq!(
            playable_buffer_ms(4_000_000_000, -1, true, 0, 1_000_000_000),
            None,
            "an A/V stream cannot claim reserve before its audio lane arrives",
        );
        assert_eq!(
            playable_buffer_ms(4_000_000_000, -1, false, 0, 1_000_000_000),
            Some(3_000),
            "a declared video-only stream uses its video tail",
        );
    }

    #[test]
    fn the_most_specific_signal_wins() {
        assert_eq!(runtime_failure(true, true, true, true), RuntimeFailure::LoadTimeout);
        assert_eq!(runtime_failure(true, true, true, false), RuntimeFailure::TvPipeline);
        assert_eq!(runtime_failure(true, true, false, false), RuntimeFailure::PlaybackInterrupted);
        assert_eq!(runtime_failure(true, false, false, false), RuntimeFailure::MediaSource);
        assert_eq!(runtime_failure(false, false, false, false), RuntimeFailure::Unknown);
    }
}
