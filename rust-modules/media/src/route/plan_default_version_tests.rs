use super::*;
use plx_data::metadata::Dovi;
use plx_data::pms::MediaVersion;
use plx_os::devcaps::dv::DvCapability;
use plx_os::devcaps::{Caps, Facet};
use plx_plex::plex::session::DolbyVisionMode;
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
    picked_with_dolby_vision(versions, caps, DolbyVisionMode::Auto, DvCapability::Unknown)
}

fn picked_with_dolby_vision(
    versions: &[MediaVersion],
    caps: &Caps,
    dv_mode: DolbyVisionMode,
    dv_probed: DvCapability,
) -> Option<i64> {
    decodable_version(versions, caps, dv_mode, dv_probed).map(|v| v.id.0)
}

fn uhd_with(id: i64, dovi: Dovi) -> MediaVersion {
    MediaVersion { dovi, ..version(id, "4k", 3840, 2160, "hevc") }
}

fn profile_5() -> Dovi {
    Dovi { present: true, profile: 5, bl_compat: 0, ..Dovi::NONE }
}

fn profile_8() -> Dovi {
    Dovi { present: true, profile: 8, bl_compat: 1, ..Dovi::NONE }
}

#[test]
fn played_video_uses_the_chosen_versions_range_and_resolution_together() {
    use plx_base::detail_parts::DynamicRange;
    let d = plx_data::metadata::Detail {
        part: "/primary".into(), video_resolution: "4k".into(), vcodec: "hevc".into(),
        dovi: profile_5(),
        versions: vec![uhd_with(17, profile_5()), MediaVersion { hdr: true, ..full_hd() }],
        ..Default::default()
    };
    let facts = |mode, capability| played_video(&d, mode, &Caps::assumed(), DolbyVisionMode::Auto, capability).unwrap();
    let alternate = facts(DirectPlayMode::Auto, DvCapability::Unsupported);
    assert_eq!((alternate.version.unwrap().id, alternate.resolution, alternate.range()),
               (MediaId(2), "1080", DynamicRange::Hdr));
    let primary = facts(DirectPlayMode::Auto, DvCapability::Supported);
    assert!(primary.version.is_none());
    assert_eq!((primary.resolution, primary.range()), ("4k", DynamicRange::DolbyVision));
    let forced = facts(DirectPlayMode::Forced, DvCapability::Unsupported);
    assert!(forced.version.is_none());
    assert_eq!((forced.resolution, forced.range()), ("4k", DynamicRange::Sdr));
}

#[test]
fn played_video_reports_the_on_deck_episodes_base_layer_when_dv_is_unavailable() {
    use plx_base::detail_parts::DynamicRange;
    use plx_data::metadata::{Detail, Episode, EpisodeStreams};
    let caps = Caps::assumed();
    let mut d = Detail { hdr: true, dovi: profile_8(), ..Default::default() };
    assert!(played_video(&d, DirectPlayMode::Auto, &caps, DolbyVisionMode::Auto, DvCapability::Supported).is_none());
    d.on_deck = Some(Episode { rk: "episode".into(), resume_ms: 5000, part: "/episode".into(), vcodec: "hevc".into(), acodec: "aac".into(), ..Default::default() });
    assert!(played_video(&d, DirectPlayMode::Auto, &caps, DolbyVisionMode::Auto, DvCapability::Supported).is_none(),
            "an on-deck episode does not make an unsuccessful backfill available");
    d.episode_streams = EpisodeStreams::Loaded("episode".into());
    for (mode, capability, range) in [
        (DolbyVisionMode::Auto, DvCapability::Supported, DynamicRange::DolbyVision),
        (DolbyVisionMode::Off, DvCapability::Supported, DynamicRange::Hdr),
        (DolbyVisionMode::Auto, DvCapability::Unsupported, DynamicRange::Hdr),
    ] {
        let file = played_video(&d, DirectPlayMode::Auto, &caps, mode, capability).unwrap();
        assert_eq!((file.primary_part, file.vcodec, file.acodec), ("/episode", "hevc", "aac"));
        assert!(file.dovi.present);
        assert_eq!(file.range(), range);
    }
}

#[test]
fn a_set_without_dolby_vision_plays_the_hdr10_version_listed_after_a_profile_5() {
    let versions = [uhd_with(17, profile_5()), uhd_with(18, Dovi::NONE)];
    for probed in [DvCapability::Unsupported, DvCapability::Unknown] {
        assert_eq!(picked_with_dolby_vision(&versions, &Caps::assumed(), DolbyVisionMode::Auto, probed), Some(18), "{probed:?}");
    }
}

#[test]
fn a_set_that_shows_dolby_vision_keeps_the_profile_5_listed_first() {
    let versions = [uhd_with(17, profile_5()), uhd_with(18, Dovi::NONE)];
    assert_eq!(picked_with_dolby_vision(&versions, &Caps::assumed(), DolbyVisionMode::Auto, DvCapability::Supported), None);
}

#[test]
fn dolby_vision_off_passes_over_a_profile_5_even_on_a_set_that_shows_it() {
    let versions = [uhd_with(17, profile_5()), uhd_with(18, Dovi::NONE)];
    assert_eq!(picked_with_dolby_vision(&versions, &Caps::assumed(), DolbyVisionMode::Off, DvCapability::Supported), Some(18));
}

#[test]
fn a_profile_8_listed_first_plays_its_base_layer_on_a_set_without_dolby_vision() {
    let versions = [uhd_with(19, profile_8()), uhd_with(20, Dovi::NONE)];
    assert_eq!(picked_with_dolby_vision(&versions, &Caps::assumed(), DolbyVisionMode::Auto, DvCapability::Unsupported), None);
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

#[test]
fn played_video_follows_the_first_episode_of_an_unstarted_show() {
    use plx_data::metadata::{Detail, Episode, EpisodeStreams};
    let first = Episode { rk: "first".into(), part: "/first".into(), vcodec: "h264".into(), ..Default::default() };
    let mut d = Detail {
        is_show: true, episodes: vec![first].into(), episode_streams: EpisodeStreams::Loaded("first".into()),
        video_resolution: "720".into(), ..Default::default()
    };
    fn facts(d: &Detail) -> Option<PlayedVideo<'_>> {
        played_video(d, DirectPlayMode::Auto, &Caps::assumed(), DolbyVisionMode::Auto, DvCapability::Unknown)
    }
    assert_eq!(facts(&d).unwrap().resolution, "720");
    d.episode_streams = EpisodeStreams::LegacyOnDeck;
    assert!(facts(&d).is_none(), "a legacy recording cannot identify its season's stream owner");
    d.episode_streams = EpisodeStreams::Loaded("first".into());
    d.on_deck = Some(Episode { rk: "other".into(), part: "/other".into(), ..Default::default() });
    assert_eq!(facts(&d).unwrap().primary_part, "/first");
    d.on_deck.as_mut().unwrap().resume_ms = 5000;
    assert!(facts(&d).is_none(), "the backfill belongs to a different episode");
}
