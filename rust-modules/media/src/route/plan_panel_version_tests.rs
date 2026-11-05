use super::*;
use plx_data::pms::MediaVersion;
use plx_platform::devcaps::panel::Panel;
use plx_platform::devcaps::Caps;
use plx_plex::plex::MediaId;

fn version(id: i64, resolution: &str, width: i64, height: i64, vcodec: &str, kbps: i64) -> MediaVersion {
    MediaVersion {
        id: MediaId(id),
        kbps,
        resolution: resolution.into(),
        width,
        height,
        vcodec: vcodec.into(),
        ..Default::default()
    }
}

fn uhd() -> MediaVersion {
    version(1, "4k", 3840, 2160, "hevc", 65121)
}

fn full_hd() -> MediaVersion {
    version(2, "1080", 1920, 800, "hevc", 2535)
}

fn hd() -> MediaVersion {
    version(3, "720", 1280, 720, "h264", 3000)
}

fn picked(versions: &[MediaVersion], panel: Panel) -> Option<i64> {
    panel_version(versions, panel, &Caps::assumed()).map(|v| v.id.0)
}

#[test]
fn a_panel_that_is_not_uhd_plays_the_1080p_version_listed_after_the_4k() {
    assert_eq!(picked(&[uhd(), full_hd()], Panel::NotUhd), Some(2));
}

#[test]
fn without_a_1080p_version_it_plays_the_720p_rather_than_the_4k() {
    assert_eq!(picked(&[uhd(), hd()], Panel::NotUhd), Some(3));
}

#[test]
fn it_plays_the_highest_version_that_fits_the_panel() {
    assert_eq!(picked(&[uhd(), hd(), full_hd()], Panel::NotUhd), Some(2));
}

#[test]
fn with_every_version_above_the_panel_it_plays_the_smallest_one() {
    let qhd = version(4, "1440", 2560, 1440, "hevc", 20000);
    assert_eq!(picked(&[uhd(), qhd], Panel::NotUhd), Some(4));
}

#[test]
fn a_uhd_panel_keeps_the_4k_version_listed_first() {
    assert_eq!(picked(&[uhd(), full_hd()], Panel::Uhd), None);
}

#[test]
fn a_uhd_panel_plays_the_4k_version_listed_second() {
    assert_eq!(picked(&[full_hd(), uhd()], Panel::Uhd), Some(1));
}

#[test]
fn a_first_version_that_already_fits_changes_nothing() {
    assert_eq!(picked(&[full_hd(), uhd()], Panel::NotUhd), None);
    assert_eq!(picked(&[uhd(), uhd()], Panel::NotUhd), None);
}

#[test]
fn an_unknown_panel_or_no_versions_play_the_first_version_as_before() {
    assert_eq!(picked(&[uhd(), full_hd()], Panel::Unknown), None);
    assert_eq!(picked(&[], Panel::NotUhd), None);
}

#[test]
fn within_one_class_the_version_the_set_decodes_wins_then_the_higher_rate() {
    let undecodable = version(5, "1080", 1920, 1080, "mpeg2video", 30000);
    let low = version(6, "1080", 1920, 1080, "h264", 4000);
    let high = version(7, "1080", 1920, 1080, "h264", 8000);
    assert_eq!(picked(&[uhd(), undecodable, low, high], Panel::NotUhd), Some(7));
}

#[test]
fn without_a_resolution_class_the_frame_size_decides() {
    let cropped = version(8, "", 1920, 800, "h264", 5000);
    let scope = version(9, "", 4096, 1716, "hevc", 50000);
    let unmeasured = version(10, "", 0, 0, "h264", 0);
    assert_eq!(resolution_class(&cropped), Some(1080));
    assert_eq!(resolution_class(&scope), Some(2160));
    assert_eq!(resolution_class(&unmeasured), None);
    assert_eq!(resolution_class(&version(11, "sd", 720, 576, "h264", 0)), Some(480));
    assert_eq!(resolution_class(&version(12, "576", 720, 576, "h264", 0)), Some(576));
    assert_eq!(picked(&[scope, unmeasured, cropped], Panel::NotUhd), Some(8));
}
