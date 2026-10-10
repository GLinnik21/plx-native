//! AV1 direct-play gates (issue #593): AV1 is fed to the decoder only when the device's own
//! codec table lists an AV1 row, and then only inside that row's bound and the shared one.
use super::*;

/// The caps a set with an AV1 row in its table reports, at the A2's documented UHD bound.
fn caps_on() -> plx_platform::devcaps::Caps {
    plx_platform::devcaps::Caps { av1: true, av1_row: (3840, 2160, 60), ..plx_platform::devcaps::Caps::assumed() }
}

#[test]
fn av1_direct_plays_only_with_the_row() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    assert!(video_direct_plays("av1", 3840, 2160, dp, &caps_on()));
    assert!(
        !video_direct_plays("av1", 3840, 2160, dp, &plx_platform::devcaps::Caps::assumed()),
        "a set whose table has no AV1 decoder never direct-plays AV1",
    );
}

#[test]
fn av1_is_bounded_by_its_own_row() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    let caps = plx_platform::devcaps::Caps { av1_row: (1920, 1080, 60), ..caps_on() };
    assert!(!video_direct_plays("av1", 3840, 2160, dp, &caps), "above the AV1 row");
    assert!(video_direct_plays("av1", 1920, 1080, dp, &caps), "at the AV1 row");
    assert!(
        video_direct_plays("hevc", 3840, 2160, dp, &caps),
        "the AV1 row does not narrow HEVC, which keeps the shared bound",
    );
}

#[test]
fn av1_is_bounded_by_the_shared_bound_too() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    let caps = plx_platform::devcaps::Caps {
        av1_row: (7680, 4320, 60),
        hevc_max: (1920, 1088),
        ..caps_on()
    };
    assert!(
        !video_direct_plays("av1", 3840, 2160, dp, &caps),
        "a wider AV1 row cannot exceed the shared hevc_max bound",
    );
}

#[test]
fn forced_feed_admits_av1_only_with_the_row() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    assert!(!video_feed_supported("av1", dp, &plx_platform::devcaps::Caps::assumed()));
    assert!(video_feed_supported("av1", dp, &caps_on()));
    assert!(video_feed_supported("h264", dp, &plx_platform::devcaps::Caps::assumed()));
}

#[test]
fn a_dv_refusal_still_refuses_av1() {
    let blocked_dv = plx_data::metadata::Dovi { present: true, profile: 5, bl_compat: 0, ..plx_data::metadata::Dovi::NONE }
        .presentation(false, plx_platform::devcaps::dv::DvCapability::Unsupported, true);
    assert!(!video_direct_plays("av1", 3840, 2160, blocked_dv, &caps_on()));
    assert!(!video_feed_supported("av1", blocked_dv, &caps_on()));
}

#[test]
fn vp9_never_direct_plays() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    assert!(!video_direct_plays("vp9", 1920, 1080, dp, &caps_on()));
    assert!(!video_feed_supported("vp9", dp, &caps_on()));
}
