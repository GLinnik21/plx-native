//! The three places that each say "this television can play codec X" must agree, because only the
//! first is the server's answer and the other two are what runs when it is not:
//!
//! 1. the capability profile sent to PMS (`plex::direct_play_video_codecs`, the list its
//!    `videoCodec=` is built from),
//! 2. the local direct-play gate (`video_direct_plays`, which stands in for `/decision` when it is
//!    unreachable and decides whether "Original" is offered),
//! 3. the Load refusal (`engine::load_video_refused`, the guard against a server that ignored the
//!    profile).
//!
//! Checked over every combination of the table-gated capabilities, at a size that clears every bound.
use super::*;
use crate::player::engine::{load_video_refused, LoadVideo};

const CODECS: [&str; 4] = ["h264", "hevc", "vp9", "av1"];

fn caps(hevc: bool, vp9: bool, av1: bool) -> plx_platform::devcaps::Caps {
    plx_platform::devcaps::Caps { hevc, vp9, av1, ..plx_platform::devcaps::Caps::assumed() }
}

#[test]
fn the_profile_and_the_local_gate_name_the_same_codecs_for_every_capability_set() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    for bits in 0..8u8 {
        let c = caps(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        let listed = plx_plex::plex::direct_play_video_codecs(&c);
        for codec in CODECS {
            assert_eq!(
                listed.contains(&codec),
                video_direct_plays(codec, 1280, 720, dp, &c),
                "{codec} under hevc={} vp9={} av1={}: profile list {listed:?} vs the local gate",
                c.hevc, c.vp9, c.av1,
            );
        }
    }
}

#[test]
fn a_refused_load_is_never_a_codec_the_profile_asked_for() {
    for bits in 0..8u8 {
        let c = caps(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        let listed = plx_plex::plex::direct_play_video_codecs(&c);
        for codec in CODECS {
            let lv = LoadVideo::of_stream(codec).expect("a fed codec");
            if load_video_refused(lv, &c) {
                assert!(
                    !listed.contains(&codec),
                    "{codec} is refused at Load but listed in the profile (hevc={} vp9={} av1={})",
                    c.hevc, c.vp9, c.av1,
                );
            }
        }
    }
}

#[test]
fn forced_mode_feeds_exactly_what_the_load_does_not_refuse() {
    let dp = plx_data::metadata::DvPresentation::NotDv;
    for bits in 0..8u8 {
        let c = caps(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0);
        for codec in CODECS {
            let lv = LoadVideo::of_stream(codec).unwrap();
            if video_feed_supported(codec, dp, &c) {
                assert!(!load_video_refused(lv, &c), "{codec}: forced mode feeds it but the Load refuses it");
            }
        }
    }
}
