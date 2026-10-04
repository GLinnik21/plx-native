//! ABR candidate reject-cause classification: mapping HLS exits and candidate
//! results onto `crate::abr::RejectCause`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn an_expired_candidate_is_a_common_censored_budget_not_an_ordinal_rung_failure() {
    assert_eq!(
        abr_reject_for_hls_exit(HlsExit::PrimeExpired),
        crate::abr::RejectCause::Censored,
    );
    assert_eq!(
        abr_reject_for_hls_exit(HlsExit::NotReady),
        crate::abr::RejectCause::Circumstance,
        "an ordinary PMS not-ready reply has no exhausted deadline and says nothing about a rung",
    );
}

#[test]
fn a_completed_underfilled_response_is_common_evidence_not_a_structural_rung_failure() {
    assert_eq!(
        abr_reject_for_candidate_result(true, false, crate::abr::CandidateVerdict::Ready,),
        crate::abr::RejectCause::ResponseUnchanged,
        "PMS response geometry, not the different requested actuator, determines the evidence",
    );
}

#[test]
fn an_incomplete_prefix_is_unknown_on_the_unqualified_debug_panel() {
    let sample = crate::abr::SegmentSample::new(
        212_992,
        258_000,
        300_000,
        2_000,
        crate::abr::BufferSnapshot {
            playback: crate::abr::MediaTimeMs(10_000),
            video_tail: crate::abr::MediaTimeMs(16_000),
            audio_tail: Some(crate::abr::MediaTimeMs(16_000)),
            audio_expected: true,
        },
    )
    .expect("valid transfer")
    .abandoned();
    assert_eq!(
        hls_abr_rate_readout(sample),
        (-1, -1, -1),
        "without a complete= column, a right-censored prefix must not look measured",
    );
}

#[test]
fn an_abr_candidate_must_decode_within_the_proposed_raster() {
    assert!(hls_raster_within(1_280, 536, crate::abr::Rung::P720));
    assert!(hls_raster_within(1_280, 720, crate::abr::Rung::P720));
    assert!(!hls_raster_within(1_920, 804, crate::abr::Rung::P720));
    assert!(!hls_raster_within(0, 720, crate::abr::Rung::P720));
}

#[test]
fn an_upshift_commits_only_an_observed_quality_improvement() {
    assert!(hls_upshift_strictly_improves(
        720, 404, 896_000, 720, 404, 992_000
    ));
    assert!(hls_upshift_strictly_improves(
        720, 404, 896_000, 1_920, 1_080, 16_150_000
    ));
    assert!(!hls_upshift_strictly_improves(
        720, 404, 896_000, 720, 404, 896_000
    ));
    assert!(
        !hls_upshift_strictly_improves(720, 404, 992_000, 720, 404, 923_000),
        "the live trace's 12→16 Mbps request returned the same raster at a lower declaration",
    );
    assert!(
        !hls_upshift_strictly_improves(720, 404, 992_000, 3_840, 2_160, 896_000),
        "pixels traded for fewer declared bits are not objectively ordered without a heuristic",
    );
    assert!(!hls_upshift_strictly_improves(
        0, 0, 0, 3_840, 2_160, 20_895_000
    ));
}

#[test]
fn a_short_http_body_is_not_a_completed_hls_segment() {
    assert!(!hls_body_complete(400_000, 212_992));
    assert!(hls_body_complete(400_000, 400_000));
    assert!(
        hls_body_complete(-1, 212_992),
        "a close-delimited response has no byte total to compare; transport errors are separate",
    );
}
