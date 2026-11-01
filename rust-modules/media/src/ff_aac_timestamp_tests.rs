//! MPEG-TS/AAC timestamp handling: in-band parameter-set recovery, ADTS
//! frame-clock backfill when the container timestamp is missing or unreliable, and the rebase of
//! a transport stream whose clock does not start near zero.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// -- MPEG-TS elementary-stream framing -----------------------------------------------

#[test]
fn a_ts_idr_keeps_in_band_parameter_sets_without_avcc_conversion() {
    let packet = [
        0, 0, 0, 1, 0x67, 0x42, 0x00, // SPS
        0, 0, 1, 0x68, 0xce, // PPS
        0, 0, 0, 1, 0x65, 0xaa, // IDR
    ];
    let mut sets = H264ParamSets::default();
    let mut out = Vec::new();
    assert_eq!(ts_h264_access_unit(&packet, &mut sets, &mut out), Ok(true));
    assert_eq!(out, packet);
    assert!(!sets.sps.is_empty());
    assert!(!sets.pps.is_empty());
}

#[test]
fn a_leading_missing_aac_timestamp_is_backfilled_from_frame_duration() {
    let mut stamps = [
        AudioStamp {
            au: 3,
            raw_ns: None,
            duration_ns: Some(21_333_333),
        },
        AudioStamp {
            au: 4,
            raw_ns: Some(900_000_000),
            duration_ns: Some(21_333_333),
        },
        AudioStamp {
            au: 5,
            raw_ns: None,
            duration_ns: Some(21_333_333),
        },
    ];
    assert_eq!(resolve_audio_stamps(&mut stamps), Ok(2));
    assert_eq!(stamps[0].raw_ns, Some(878_666_667));
    assert_eq!(stamps[2].raw_ns, Some(921_333_333));
}

#[test]
fn a_timestamp_free_aac_segment_uses_only_its_frame_clock() {
    let mut stamps = [
        AudioStamp {
            au: 0,
            raw_ns: None,
            duration_ns: Some(20_000_000),
        },
        AudioStamp {
            au: 1,
            raw_ns: None,
            duration_ns: Some(20_000_000),
        },
        AudioStamp {
            au: 2,
            raw_ns: None,
            duration_ns: Some(20_000_000),
        },
    ];
    assert_eq!(resolve_audio_stamps(&mut stamps), Ok(3));
    assert_eq!(
        stamps.iter().map(|stamp| stamp.raw_ns).collect::<Vec<_>>(),
        [Some(0), Some(20_000_000), Some(40_000_000)]
    );
}

#[test]
fn an_unanchored_aac_timestamp_hole_fails_closed() {
    let mut stamps = [
        AudioStamp {
            au: 0,
            raw_ns: Some(0),
            duration_ns: None,
        },
        AudioStamp {
            au: 1,
            raw_ns: None,
            duration_ns: None,
        },
    ];
    assert_eq!(
        resolve_audio_stamps(&mut stamps),
        Err("AAC timestamp hole has no duration anchor")
    );
}

#[test]
fn a_later_ts_idr_recovers_cached_parameter_sets() {
    let first = [0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3];
    let later = [0, 0, 1, 0x65, 4];
    let mut sets = H264ParamSets::default();
    let mut out = Vec::new();
    ts_h264_access_unit(&first, &mut sets, &mut out).unwrap();
    assert_eq!(ts_h264_access_unit(&later, &mut sets, &mut out), Ok(true));
    assert!(out.starts_with(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]));
    assert!(out.ends_with(&later));
}

#[test]
fn a_ts_idr_without_any_parameter_sets_is_rejected() {
    let mut sets = H264ParamSets::default();
    let mut out = Vec::new();
    assert_eq!(
        ts_h264_access_unit(&[0, 0, 1, 0x65, 4], &mut sets, &mut out),
        Err("IDR has no in-band or cached SPS")
    );
}

#[test]
fn adts_detection_accepts_a_real_header_but_not_arbitrary_ff_bytes() {
    let mut frame = adts_header(3, 2, 11).to_vec();
    frame.extend_from_slice(&[0; 11]);
    assert!(packet_has_adts(&frame));
    assert!(!packet_has_adts(&[0xff, 0x00, 0, 0, 0, 0, 0]));
    assert_eq!(adts_duration_ns(&frame), Some(21_333_333));
    assert_eq!(adts_duration_ns(&[0xff, 0x00, 0, 0, 0, 0, 0]), None);
}

// -- a start-code stream's clock (issue #1084) ----------------------------------------------
//
// The engine takes a fresh play's first keyframe to be near 0 (`rebase_pending` is armed only for a
// seek), and `seek_to_ns` is content time. A broadcast capture's clock starts wherever the
// transmitter's did: the report's first audio unit was fed at 45171.69 s, a pipeline 12.5 hours
// ahead of its own start. So a stream with no avcC record is moved to 0 at the demuxer, once.

#[test]
fn a_start_code_stream_is_rebased_by_its_start_time() {
    // `AVFormatContext.start_time` is in AV_TIME_BASE units (microseconds).
    assert_eq!(stream_origin_ns(45_171_690_000, NalInput::StartCodes), 45_171_690_000_000);
    assert_eq!(stream_origin_ns(1_400_000, NalInput::StartCodes), 1_400_000_000, "an ffmpeg-made TS starts at 1.4 s");
}

/// Only a stream with no config record changes: Matroska and MP4 keep the clock they have always had.
#[test]
fn a_length_prefixed_stream_keeps_its_clock() {
    assert_eq!(stream_origin_ns(45_171_690_000, NalInput::LengthPrefixed), 0);
    assert_eq!(stream_origin_ns(7_000, NalInput::LengthPrefixed), 0, "an MKV whose first block is at 7 ms");
}

#[test]
fn an_unknown_or_negative_start_time_rebases_nothing() {
    assert_eq!(stream_origin_ns(AV_NOPTS_VALUE, NalInput::StartCodes), 0);
    assert_eq!(stream_origin_ns(-40_000, NalInput::StartCodes), 0, "never moves a stream forward");
    assert_eq!(stream_origin_ns(0, NalInput::StartCodes), 0);
}

#[test]
fn a_rebased_timestamp_moves_back_and_a_missing_one_stays_missing() {
    assert_eq!(rebase_ts(4_065_452_100_000, 4_065_450_000_000), 2_100_000);
    assert_eq!(rebase_ts(AV_NOPTS_VALUE, 4_065_450_000_000), AV_NOPTS_VALUE);
    assert_eq!(rebase_ts(10, 40), -30, "a dts ahead of the stream's start stays negative; pts_ns reads pts first");
}
