use super::*;
use plx_data::pms::MediaVersion;
use plx_platform::devcaps::{Caps, Facet};
use plx_plex::plex::MediaId;

fn version(id: i64, resolution: &str, width: i64, height: i64, vcodec: &str) -> MediaVersion {
    MediaVersion {
        id: MediaId(id),
        resolution: resolution.into(),
        width,
        height,
        vcodec: vcodec.into(),
        ..Default::default()
    }
}

fn uhd() -> MediaVersion {
    version(1, "4k", 3840, 2160, "hevc")
}

fn full_hd() -> MediaVersion {
    version(2, "1080", 1920, 800, "hevc")
}

fn hd() -> MediaVersion {
    version(3, "720", 1280, 720, "h264")
}

fn full_hd_set() -> Caps {
    Caps { hevc_max: (1920, 1088), ..Caps::assumed() }
}

fn picked(versions: &[MediaVersion], caps: &Caps) -> Option<i64> {
    decodable_version(versions, caps).map(|v| v.id.0)
}

#[test]
fn a_1080p_decoder_plays_the_1080p_version_listed_after_the_4k() {
    assert_eq!(picked(&[uhd(), full_hd()], &full_hd_set()), Some(2));
}

#[test]
fn without_a_1080p_version_it_plays_the_720p_rather_than_transcoding_the_4k() {
    assert_eq!(picked(&[uhd(), hd()], &full_hd_set()), Some(3));
}

#[test]
fn it_plays_the_highest_version_the_decoder_takes() {
    assert_eq!(picked(&[uhd(), hd(), full_hd()], &full_hd_set()), Some(2));
}

#[test]
fn with_no_version_the_decoder_takes_it_keeps_the_smallest_one() {
    let qhd = version(4, "1440", 2560, 1440, "hevc");
    assert_eq!(picked(&[uhd(), qhd], &full_hd_set()), Some(4));
}

#[test]
fn a_first_version_that_plays_directly_changes_nothing() {
    assert_eq!(picked(&[uhd(), full_hd()], &Caps::assumed()), None);
    assert_eq!(picked(&[full_hd(), uhd()], &Caps::assumed()), None);
    assert_eq!(picked(&[full_hd(), uhd()], &full_hd_set()), None);
}

#[test]
fn versions_of_one_class_above_the_decoder_keep_the_first_and_no_versions_are_no_choice() {
    assert_eq!(picked(&[uhd(), uhd()], &full_hd_set()), None);
    assert_eq!(picked(&[], &full_hd_set()), None);
}

#[test]
fn a_first_version_in_a_codec_the_set_lacks_gives_way_to_one_it_decodes() {
    let undecodable = version(5, "1080", 1920, 1080, "mpeg2video");
    assert_eq!(picked(&[undecodable, hd()], &Caps::assumed()), Some(3));
}

#[test]
fn within_one_class_the_first_listed_version_the_set_decodes_wins() {
    let undecodable = version(5, "1080", 1920, 1080, "mpeg2video");
    let first = version(6, "1080", 1920, 1080, "h264");
    let second = version(7, "1080", 1920, 1080, "h264");
    assert_eq!(picked(&[uhd(), undecodable, first, second], &full_hd_set()), Some(6));
}

#[test]
fn a_4k_the_attempt_has_refused_gives_way_to_the_1080p() {
    let refused = Facet::Video { codec: "hevc".into(), width: 3840, height: 2160 };
    assert_eq!(picked(&[uhd(), full_hd()], &Caps::assumed().without(&refused)), Some(2));
}

#[test]
fn without_a_resolution_class_the_frame_size_decides() {
    let cropped = version(8, "", 1920, 800, "h264");
    let scope = version(9, "", 4096, 1716, "hevc");
    let unmeasured = version(10, "", 0, 0, "h264");
    assert_eq!(resolution_class(&cropped), Some(1080));
    assert_eq!(resolution_class(&scope), Some(2160));
    assert_eq!(resolution_class(&unmeasured), None);
    assert_eq!(resolution_class(&version(11, "sd", 720, 576, "h264")), Some(480));
    assert_eq!(resolution_class(&version(12, "576", 720, 576, "h264")), Some(576));
    assert_eq!(resolution_class(&version(13, "", 0, 1080, "h264")), Some(1080));
    assert_eq!(picked(&[scope, unmeasured, cropped], &full_hd_set()), Some(8));
}

#[test]
fn a_garbage_frame_size_is_a_large_class_not_an_overflow() {
    assert_eq!(resolution_class(&version(14, "", i64::MAX, 0, "hevc")), Some(2160));
    assert_eq!(resolution_class(&version(15, "", 0, i64::MAX, "hevc")), Some(2160));
    assert_eq!(resolution_class(&version(16, "", -1, i64::MIN, "hevc")), None);
}
