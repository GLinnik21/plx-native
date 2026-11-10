//! The readers that ask the session's readout for a Feed fact instead of the feed's mailbox
//! (L3a-2 step 4b, part 2a). Each test delivers a sample through the session's real ingestion
//! while the mailbox holds its defaults, so a reader that still looked at the mailbox says 1x /
//! no report, and one that looked at another attempt's sample says that attempt's value.

use plx_playback_contract::feed::FeedFacts;
use plx_playback_contract::RATE_NORMAL;

use super::attempt_view::Backend;
use crate::route::{PlaybackSession, RouteStartAttempt};

fn sample(rate: i32, audio: Option<bool>) -> FeedFacts {
    let mut facts = FeedFacts::default();
    facts.raw.applied_rate_permille = rate;
    facts.raw.speed_audio_ok = audio;
    facts
}

/// The speed the pipeline runs at is the last sample's, addressed by its attempt: a late sample of a
/// replaced attempt does not change it, and a URI attempt still has none.
#[test]
fn the_applied_rate_is_the_ingested_samples_for_the_addressed_attempt() {
    let _serial = plx_base::testlock::serial();
    let (a, b) = (RouteStartAttempt::fixture(), RouteStartAttempt::fixture_n(2));
    let mut ps = PlaybackSession::IDLE;
    ps.begin_attempt_view(a, Backend::Feed);
    assert_eq!(super::applied_rate(&ps), Some(RATE_NORMAL), "nothing sampled yet: 1x");
    assert!(ps.ingest_feed_facts(a, sample(1500, None)));
    assert_eq!(super::applied_rate(&ps), Some(1500), "the reader is the readout's sample, not the mailbox");
    assert_eq!(super::effective_playback_rate(&ps), Some(1500));

    ps.begin_attempt_view(b, Backend::Feed);
    assert!(!ps.ingest_feed_facts(a, sample(750, None)), "a replaced attempt's sample is refused");
    assert_eq!(super::applied_rate(&ps), Some(1500), "so it cannot move the reader");
    assert!(ps.ingest_feed_facts(b, sample(750, None)));
    assert_eq!(super::applied_rate(&ps), Some(750));

    ps.begin_attempt_view(RouteStartAttempt::fixture_n(3), Backend::Uri);
    assert_eq!(super::applied_rate(&ps), None, "a URI attempt has no Feed speed evidence");
    assert_eq!(super::effective_playback_rate(&ps), Some(RATE_NORMAL), "URI only admits normal speed");
}

/// What the pipeline said about a speed session's audio is the last sample's: `None` before any
/// report, then the verdict as it was ingested.
#[test]
fn the_speed_audio_report_is_the_ingested_samples() {
    let _serial = plx_base::testlock::serial();
    let a = RouteStartAttempt::fixture();
    let mut ps = PlaybackSession::IDLE;
    ps.begin_attempt_view(a, Backend::Feed);
    assert_eq!(super::speed_audio_report(&ps), None, "no report yet");
    assert!(ps.ingest_feed_facts(a, sample(1500, Some(true))));
    assert_eq!(super::speed_audio_report(&ps), Some(true));
    assert!(ps.ingest_feed_facts(a, sample(1500, Some(false))));
    assert_eq!(super::speed_audio_report(&ps), Some(false), "the declared 48 kHz stereo was not what it set up");
}
