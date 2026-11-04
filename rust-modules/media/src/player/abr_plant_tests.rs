//! The closed-loop ABR plant models the Feed pipeline's queues and leads; these pin it to them.
//! They live with the Feed side because the constants they read are the Feed executor's.

/// MATHEMATICAL INVARIANT: the plant's constants are still the pipeline's.
///
/// **All FOUR of them.** `B_max = lead + queue_bytes/rate`, and this test used to pin only the
/// two byte caps — so either FEED LEAD could move in `player::engine` and this plant would go
/// on modelling a pipeline that no longer exists, silently. That is not a hypothetical failure
/// mode here: the operating-point table one level up was hand-transcribed, the fixture pack was
/// rebuilt under it, and two of its three points described a television that had stopped
/// existing for a month without a single test going red.
#[test]
fn the_plant_constants_still_match_the_pipeline() {
    let (video_bytes, audio_bytes, video_lead_ms, audio_lead_ms) = crate::abr::pipeline_constants();
    let (video, audio) = crate::player::aq_caps();
    assert_eq!(video as u64, video_bytes);
    assert_eq!(audio as u64, audio_bytes);
    let (video_lead, audio_lead) = crate::player::feed_leads_ms();
    assert_eq!(video_lead, video_lead_ms);
    assert_eq!(audio_lead, audio_lead_ms);
}
