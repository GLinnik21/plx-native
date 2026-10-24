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

/// A Live TV recording's later segment carries no SPS/PPS (2026-10-21): the sets the previous
/// segment of the SAME playlist left are carried over, and another playlist never inherits them.
#[test]
fn a_live_segment_without_parameter_sets_inherits_them_from_its_own_playlist_only() {
    let at = |path: &str| crate::hls::Segment {
        sequence: 1,
        duration: std::time::Duration::from_secs(1),
        resource: crate::hls::Resource::new(plx_plex::plex::Origin::http("192.0.2.10", 32400), path).unwrap(),
    };
    let session = "/livetv/sessions/9b9fe727/d459eaef";
    assert_eq!(carry_scope(&at(&format!("{session}/00000.ts"))), session);
    assert_eq!(carry_scope(&at(&format!("{session}/00001.ts?X-Plex-Token=t"))), session);
    assert_ne!(carry_scope(&at("/video/:/transcode/universal/session/other/base/00001.ts")), session);

    // the previous segment's sets, cloned into the next one, go in front of its bare IDR
    let mut previous = H264ParamSets::default();
    let mut out = Vec::new();
    ts_h264_access_unit(&[0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3], &mut previous, &mut out).unwrap();
    let mut next = previous.clone();
    assert_eq!(ts_h264_access_unit(&[0, 0, 1, 0x65, 4], &mut next, &mut out), Ok(true));
    assert!(out.starts_with(&[0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2]));
}

/// A live recording starts three seconds behind its edge and is polled twice a second (measured
/// on the TV 2026-10-21: started on its first segment it stalled at every late one); a PMS
/// transcode's playlist keeps the VOD cadence and starts at once.
#[test]
fn a_live_playlist_starts_behind_its_edge_and_is_polled_faster() {
    assert!(!live_start_ready(true, false, 760_000_000), "one segment is no cushion");
    assert!(!live_start_ready(true, false, 2_760_000_000));
    assert!(live_start_ready(true, false, 3_760_000_000));
    assert!(live_start_ready(true, true, 0), "once started, every segment goes as it lands");
    assert!(live_start_ready(false, false, 0), "a PMS transcode is never held");
    assert_eq!(playlist_poll(true, 3), std::time::Duration::from_millis(500));
    assert_eq!(playlist_poll(false, 3), std::time::Duration::from_millis(1_000));
    assert_eq!(playlist_poll(false, 0), std::time::Duration::from_millis(250));
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
