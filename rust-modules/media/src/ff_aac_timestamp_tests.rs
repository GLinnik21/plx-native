//! MPEG-TS/AAC timestamp handling: in-band parameter-set recovery and ADTS
//! frame-clock backfill when the container timestamp is missing or unreliable.

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
