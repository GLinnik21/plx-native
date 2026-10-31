//! Dolby Vision direct-play gating tests: profile 5/7/8 declarations, base-layer
//! usability, and the device-bound/dimension checks that compose with them.

#[allow(unused_imports)]
use super::test_support::*;
use super::*;
use plx_data::metadata::{Dovi, DvDecision};
use plx_platform::devcaps::dv::DvCapability;

#[test]
fn p8_without_confirmed_dv_support_uses_base_layer() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    for bl_compat in [1, 2, 4] {
        let dovi = Dovi {
            present: true,
            profile: 8,
            bl_compat,
            el_present: false,
            ..Dovi::NONE
        };
        for capability in [
            plx_platform::devcaps::dv::DvCapability::Unknown,
            plx_platform::devcaps::dv::DvCapability::Unsupported,
        ] {
            for signal in [false, true] {
                let presentation = dovi.presentation(signal, capability, true);
                assert_eq!(presentation, DvPresentation::NotDv);
                assert!(presentation.declared().is_none());
                assert!(
                    video_direct_plays("hevc", 3840, 2160, presentation, &caps),
                    "ccid={bl_compat} capability={capability:?} signal={signal}",
                );
            }
        }
    }
}

#[test]
fn p5_without_confirmed_dv_support_requires_video_encode() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    for capability in [
        plx_platform::devcaps::dv::DvCapability::Unknown,
        plx_platform::devcaps::dv::DvCapability::Unsupported,
    ] {
        for signal in [false, true] {
            let presentation = p5().presentation(signal, capability, true);
            assert!(presentation.refusal().is_some());
            assert!(presentation.declared().is_none());
            assert!(!video_direct_plays("hevc", 3840, 1602, presentation, &caps));
            assert!(
                p5().base_layer_unusable(),
                "the remux/directStream video-copy permission must remain withdrawn",
            );
        }
    }
}

#[test]
fn supported_dv_preserves_signal_and_layer_rules() {
    use plx_platform::devcaps::dv::DvCapability::Supported;

    assert!(p5()
        .presentation(false, Supported, true)
        .refusal()
        .is_some());
    assert_eq!(
        p5().presentation(true, Supported, true)
            .declared()
            .map(|n| n.profile_id),
        Some(5),
    );
    for signal in [false, true] {
        for compat in [1, 2, 4] {
            let p8 = Dovi {
                bl_compat: compat,
                ..p8()
            };
            assert_eq!(
                p8.presentation(signal, Supported, true)
                    .declared()
                    .map(|n| n.profile_id),
                Some(8),
            );
        }
        // A Profile 7 with a cross-compatible base layer is fed as the 8.1 `ff::packet_to_annexb`
        // makes of it and declared as such; only a dual layer without one is still refused.
        assert_eq!(
            p7().presentation(signal, Supported, true)
                .declared()
                .map(|n| (n.profile_id, n.track_type)),
            Some((8, "single")),
        );
        assert_eq!(
            Dovi { bl_compat: 0, ..p7() }.presentation(signal, Supported, true),
            DvPresentation::Refuse("dual-layer"),
        );
    }
}

#[test]
fn non_hevc_never_declares_dolby_vision() {
    use plx_platform::devcaps::dv::DvCapability::Supported;

    // HEVC and AV1 carry a declaration; an AVC Profile 9 and VP9 never do.
    for (vcodec, carries) in [("hevc", true), ("HEVC", true), ("av1", true), ("h264", false), ("vp9", false)] {
        assert_eq!(plx_data::metadata::dv_video_codec(vcodec), carries, "{vcodec}");
    }
    let p10 = Dovi {
        present: true,
        profile: 10,
        bl_compat: 1,
        el_present: false,
        ..Dovi::NONE
    };
    assert_eq!(
        p10.presentation(true, Supported, plx_data::metadata::dv_video_codec("av1"))
            .declared()
            .map(|n| n.profile_id),
        Some(10),
    );

    let p9 = Dovi {
        present: true,
        profile: 9,
        bl_compat: 2,
        el_present: false,
        ..Dovi::NONE
    };
    let fallback = p9.presentation(true, Supported, false);
    assert_eq!(fallback, DvPresentation::NotDv);
    assert!(fallback.declared().is_none());

    let unusable = p5().presentation(true, Supported, false);
    assert!(unusable.refusal().is_some());
    assert!(unusable.declared().is_none());
}

/// **The bug this gate exists for.** Profile 5 is single-layer IPT-PQ with no HDR10 fallback,
/// so feeding its base layer to an ordinary HEVC decoder produces a picture in visibly wrong
/// colours — and nothing else in the ladder can see that: the codec is `hevc` (fine), the
/// frame size clears the dev TV's bound (fine), the container is mp4, which has direct-played
/// since 2026-08-11 (fine). Every gate passes and the user gets a broken picture.
#[test]
fn a_profile_5_source_does_not_direct_play_undeclared() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176), // the dev TV's own bound — this must fail on SIZE grounds nowhere
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    // the live P5 item's own shape: 3840x1602 hevc, well inside the bound
    assert!(
        !video_direct_plays(
            "hevc",
            3840,
            1602,
            p5().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &caps
        ),
        "IPT-PQ has no HDR10 base layer"
    );
    // and it is the DV fields doing it, not the size or the codec: the same file without them
    // direct-plays, which is exactly the behaviour that shipped the wrong colours
    assert!(video_direct_plays(
        "hevc",
        3840,
        1602,
        no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
        &caps
    ));
}

/// **The inversion, and the reason the refusal above is now conditional.** Declaring the
/// stream — one `DolbyHdrInfo` node in the Load payload — is what makes the pipeline set
/// `dolby-vision=TRUE` on the caps it builds, and a Profile 5 shown in Dolby Vision mode is
/// the correct picture rather than the wrong one. So the same file, same size, same codec,
/// direct-plays once we are willing to say what it is; the refusal was never about the
/// decoder, only about our own silence.
#[test]
fn declaring_dolby_vision_inverts_the_profile_5_refusal() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    let dv = p5().presentation(DECLARED, plx_platform::devcaps::dv::DvCapability::Supported, true);
    assert!(
        video_direct_plays("hevc", 3840, 1602, dv, &caps),
        "a declared P5 is displayable"
    );
    let n = dv
        .declared()
        .expect("the payload must carry the node the gate was opened for");
    assert_eq!(
        n.profile_id, 5,
        "getInt, and the pipeline's -1 sentinel means no profile hint"
    );
    assert_eq!(n.track_type, "single");
    assert_eq!(n.encryption_type, "clear");
    // ...and the size and codec halves of the gate are untouched by any of it
    // Holds because these caps carry no AV1 row (av1: false), not because of the Dolby gate.
    assert!(!video_direct_plays("av1", 3840, 1602, dv, &caps));
    let small = plx_platform::devcaps::Caps {
        hevc_max: (1920, 1088),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        ..caps.clone()
    };
    assert!(!video_direct_plays("hevc", 3840, 1602, dv, &small));
}

/// **A Profile 7 with a cross-compatible base layer direct-plays, declared as the 8.1 the feed
/// path makes of it.** The pipeline feeds ONE elementary stream, and `ff::packet_to_annexb` makes
/// it one: the enhancement layer is dropped and the RPUs are rewritten, so the gate and the
/// payload both see a single-layer Profile 8.1 — in both `nodv` signal states, since a
/// cross-compatible base layer declares on a supported set exactly as P8 does. A dual layer
/// without a cross-compatible base layer is the one still refused, and it is caught by
/// `el_present` alone: the live P7 item reports `bl_compat = 6`, so a compatibility-id test would
/// wave the other kind straight through.
#[test]
fn a_cross_compatible_profile_7_source_direct_plays_as_profile_8() {
    use plx_platform::devcaps::dv::DvCapability::{Supported, Unknown, Unsupported};
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    for signal in [SILENT, DECLARED] {
        let dv = p7().presentation(signal, Supported, true);
        assert!(
            video_direct_plays("hevc", 3840, 2160, dv, &caps),
            "signal={signal}"
        );
        assert_eq!(dv.refusal(), None);
        let n = dv.declared().expect("a converted P7 declares");
        assert_eq!(n.profile_id, 8, "declared as the 8.1 that is actually fed");
        assert_eq!(n.track_type, "single");
        assert_eq!(n.encryption_type, "clear");
        // Without a confirmed Dolby Vision panel nothing is declared, and the HDR10 base layer
        // the conversion leaves plays as the ordinary HEVC it is.
        for capability in [Unknown, Unsupported] {
            let dv = p7().presentation(signal, capability, true);
            assert_eq!(dv, DvPresentation::NotDv, "{capability:?} signal={signal}");
            assert!(video_direct_plays("hevc", 3840, 2160, dv, &caps));
        }
    }
    assert!(p7().converts_to_p81());
    assert!(
        !p7().base_layer_unusable(),
        "a copy of its base layer is a copy of a displayable HDR10 stream"
    );
    assert_ne!(
        p7().bl_compat,
        0,
        "the fixture must keep the trap it was built to hold"
    );
    // and it is refused in BOTH worlds: no payload key can hand the pipeline a layer we do
    // not feed it, so arming the trigger must not open this gate the way it opens P5's
    let unconvertible = Dovi {
        bl_compat: 0,
        ..p7()
    };
    assert!(!unconvertible.converts_to_p81());
    assert!(unconvertible.base_layer_unusable());
    for signal in [SILENT, DECLARED] {
        let dv = unconvertible.presentation(signal, Supported, true);
        assert!(
            !video_direct_plays("hevc", 3840, 2160, dv, &caps),
            "signal={signal}"
        );
        assert_eq!(dv.refusal(), Some("dual-layer"));
        assert_eq!(
            dv.declared(),
            None,
            "a layer we cannot feed must never be declared"
        );
    }
}

/// On a supported set Profile 8.1 declares in either `nodv` signal state, while every file with no
/// DOVI record remains ordinary video. The unsupported/unknown base-layer case is the dedicated
/// matrix above.
#[test]
fn profile_8_and_plain_files_are_unaffected() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    for signal in [SILENT, DECLARED] {
        assert!(
            video_direct_plays(
                "hevc",
                3840,
                2160,
                p8().presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
                &caps
            ),
            "HDR10-compatible base layer (signal={signal})"
        );
        assert!(video_direct_plays(
            "hevc",
            3840,
            2160,
            no_dv().presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &caps
        ));
        assert!(video_direct_plays(
            "h264",
            1920,
            1080,
            no_dv().presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &caps
        ));
        assert_eq!(
            p8().presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true)
                .refusal(),
            None
        );
        assert_eq!(
            no_dv()
                .presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true)
                .refusal(),
            None
        );
    }
    // A file with no Dolby Vision at all declares nothing however the trigger is set — the
    // node is a statement about the stream, not a mode the app is in.
    assert_eq!(
        no_dv()
            .presentation(DECLARED, plx_platform::devcaps::dv::DvCapability::Supported, true)
            .declared(),
        None
    );
    // P8 declares in BOTH settings, and that is deliberate: its base layer is HDR10 either
    // way, so the node costs nothing and adds the dynamic metadata the RPU carries. The
    // trigger reaches only the profile whose declaration is not yet free — P5, measured to
    // lose two frames every ~40 s on this set. `SILENT` here is the half that would silently
    // regress if the gate were ever rewritten as a bare `signal &&`.
    for signal in [SILENT, DECLARED] {
        assert_eq!(
            p8().presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true)
                .declared()
                .map(|n| n.profile_id),
            Some(8),
            "a cross-compatible base layer declares without the trigger: signal={signal}"
        );
    }
    assert_eq!(
        p5().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true)
            .declared(),
        None,
        "P5 stays behind the trigger"
    );
}

/// **Silence must not convict.** Every field of `Dovi` is 0 both when the server omits it and
/// when the file simply is not Dolby Vision, so a bare `bl_compat == 0` test would refuse
/// direct play for the entire library. Two guards keep that from happening, and this drives
/// both: `present` gates the whole question, and a KNOWN profile gates the compat-id test.
/// The direction is deliberate — a false refusal costs 4K and HDR10 on a file that played
/// perfectly, and on a Pass-less server (issue #22) it costs playback outright.
#[test]
fn an_unreported_dolby_vision_record_refuses_nothing() {
    // the shape every ordinary SDR file has: no DV at all, so bl_compat 0 means nothing
    assert!(!Dovi::default().base_layer_unusable());
    // `DOVIPresent` and nothing else — an older or quieter server. Not enough to convict.
    let bare = Dovi {
        present: true,
        profile: 0,
        bl_compat: 0,
        el_present: false,
        ..Dovi::NONE
    };
    assert!(
        !bare.base_layer_unusable(),
        "a compat id of 0 read out of a silent field is not a 0"
    );
    // but an explicit enhancement layer is disqualifying even with no profile reported,
    // because that field says what it says regardless of what sits beside it
    let el_only = Dovi {
        present: true,
        profile: 0,
        bl_compat: 0,
        el_present: true,
        ..Dovi::NONE
    };
    assert!(el_only.base_layer_unusable());
    // and `present: false` overrides everything — no DV means no DV, whatever noise follows
    let contradictory = Dovi {
        present: false,
        profile: 5,
        bl_compat: 0,
        el_present: true,
        ..Dovi::NONE
    };
    assert!(!contradictory.base_layer_unusable());
    // The rule survives the declaration, in both settings: a bare `present` names no profile,
    // `getInt` has nothing to be given, and a node we cannot fill is not a reason to convict a
    // file that plays. It falls through to `NotDv` — plays as it always has, declares nothing.
    for signal in [SILENT, DECLARED] {
        assert_eq!(
            Dovi::default().presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
            DvPresentation::NotDv
        );
        assert_eq!(
            bare.presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
            DvPresentation::NotDv,
            "signal={signal}"
        );
        assert_eq!(
            contradictory.presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
            DvPresentation::NotDv
        );
        assert_eq!(
            el_only.presentation(signal, plx_platform::devcaps::dv::DvCapability::Supported, true),
            DvPresentation::Refuse("dual-layer")
        );
    }
}

/// **The gate and the payload are one predicate, and this is the property that says so.**
/// Every capability, evidenced compatible CCID and trigger setting. Direct play and payload agree
/// on the same frozen presentation; a compatible base-layer play is intentionally allowed without
/// a node when support is absent or unknown.
#[test]
fn the_direct_play_gate_and_the_payload_node_can_never_disagree() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    for capability in [
        plx_platform::devcaps::dv::DvCapability::Unknown,
        plx_platform::devcaps::dv::DvCapability::Unsupported,
        plx_platform::devcaps::dv::DvCapability::Supported,
    ] {
        for compat in [0, 1, 2, 4, 6] {
            for signal in [false, true] {
                let d = Dovi {
                    present: true,
                    profile: if compat == 0 { 5 } else { 8 },
                    bl_compat: compat,
                    el_present: compat == 6,
                    ..Dovi::NONE
                };
                let presentation = d.presentation(signal, capability, true);
                let plays = video_direct_plays("hevc", 3840, 1602, presentation, &caps);
                assert_eq!(plays, presentation.refusal().is_none());
                assert!(!(presentation.refusal().is_some() && presentation.declared().is_some()));
                if presentation.refusal().is_some() {
                    assert!(d.base_layer_unusable());
                }
                if let Some(node) = presentation.declared() {
                    assert_eq!(capability, plx_platform::devcaps::dv::DvCapability::Supported);
                    assert_eq!(node.profile_id, d.profile);
                }
                if plays && capability != plx_platform::devcaps::dv::DvCapability::Supported {
                    assert_eq!(presentation, DvPresentation::NotDv);
                    assert!(presentation.declared().is_none());
                }
            }
        }
    }
}

/// The three profiles, through the predicate itself rather than the gate, including the
/// 8.2 (SDR base) and 8.4 (HLG base) variants: their base layers are ordinary displayable
/// pictures, so they direct-play like 8.1 and only the compat id tells them apart.
#[test]
fn base_layer_usability_by_profile() {
    assert!(p5().base_layer_unusable());
    // converted on the feed path: its base layer is the HDR10 picture it always was
    assert!(!p7().base_layer_unusable());
    assert!(Dovi { bl_compat: 0, ..p7() }.base_layer_unusable());
    assert!(!p8().base_layer_unusable());
    assert_eq!(
        p5().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true)
            .refusal(),
        Some("no cross-compatible base layer")
    );
    for compat in [1, 2, 4] {
        let d = Dovi {
            present: true,
            profile: 8,
            bl_compat: compat,
            el_present: false,
            ..Dovi::NONE
        };
        assert!(
            !d.base_layer_unusable(),
            "P8 with a cross-compatible base layer (id {compat})"
        );
    }
}

/// The detail page's preview must agree with what Play will do, or the facts row promises a
/// direct play the route then refuses. A P5 item reads `Converts` — which is the honest
/// answer, since a real re-encode is exactly what the server has to do to make it displayable.
///
/// It is a client-side PREDICTION and stops there: `Preview` has no "this server cannot do it"
/// state, and on the dev PMS a Profile 5 conversion is exactly what comes back refused. The
/// page says what the route will ASK for; whether the server can answer is the read-out's
/// question, not this one's.
#[test]
fn the_preview_calls_a_profile_5_item_a_conversion() {
    let aac = [plx_data::metadata::Stream {
        codec: "aac".into(),
        ..Default::default()
    }];
    let part = "/library/parts/1/2/movie.mp4";
    assert_eq!(
        playback_preview_of(
            &plx_platform::devcaps::caps(),
            part,
            "hevc",
            1920,
            1080,
            p5().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &aac
        ),
        Some(Preview::Converts),
        "the server must re-encode it — a container remux would copy the same wrong pixels"
    );
    // the identical item without the DV record is a plain direct play, so the preview is
    // reading the new field and not something else that happens to differ
    assert_eq!(
        playback_preview_of(
            &plx_platform::devcaps::caps(),
            part,
            "hevc",
            1920,
            1080,
            no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &aac
        ),
        Some(Preview::DirectPlay)
    );
    assert_eq!(
        playback_preview_of(
            &plx_platform::devcaps::caps(),
            part,
            "hevc",
            1920,
            1080,
            p8().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &aac
        ),
        Some(Preview::DirectPlay)
    );
    // and the page must follow the inversion, or the facts row promises a conversion the
    // route no longer performs — the preview reads the same predicate the gate does
    assert_eq!(
        playback_preview_of(
            &plx_platform::devcaps::caps(),
            part,
            "hevc",
            1920,
            1080,
            p5().presentation(DECLARED, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &aac
        ),
        Some(Preview::DirectPlay)
    );
}

/// The RESOLUTION half of the gate (issue #22's over-claim class): when `/decision` is
/// unreachable the fallback never asks PMS, so the profile's `*`-scoped width/height limitation
/// cannot save a 4K source from direct-playing onto a 1080p-bounded decoder — the client must
/// refuse it locally. Invisible on the dev TV (bound 4096x2176); this drives the gate with the
/// reviewer-class caps.
#[test]
fn a_source_beyond_the_device_bound_does_not_direct_play() {
    let caps = plx_platform::devcaps::Caps {
        hevc: true,
        hevc_max: (1920, 1088),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac,ac3,eac3".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    // the codec agrees; the frame size must still refuse — on either codec
    assert!(!video_direct_plays(
        "h264",
        3840,
        2160,
        no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
        &caps
    ));
    assert!(!video_direct_plays(
        "hevc",
        3840,
        2160,
        no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
        &caps
    ));
    // one axis over is over (per-axis bound, not an area heuristic)
    assert!(!video_direct_plays(
        "h264",
        4096,
        1080,
        no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
        &caps
    ));
    // within the bound plays, exactly at it included (1088 IS the table's number)
    assert!(video_direct_plays(
        "h264",
        1920,
        1088,
        no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
        &caps
    ));
}

/// Unknown dimensions fail OPEN (0 = PMS never measured the file — not evidence of 4K, and
/// yesterday's behavior for it), while the codec half keeps gating regardless.
#[test]
fn unknown_dimensions_fail_open_and_the_codec_half_still_gates() {
    let caps = plx_platform::devcaps::Caps {
        hevc: false,
        hevc_max: (1920, 1088),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        vp9_row: (0, 0, 0),
        vp9_bit_depth: 8,
        av1: false,
        av1_row: (0, 0, 0),
        audio: "aac".into(),
        audio_channels: Default::default(),
        native_ceiling: Default::default(),
        video_ceiling: Default::default(),
        app_lossy: plx_platform::devcaps::AppLossy::Off,
        app_lossy_in_force: Default::default(),
    };
    assert!(video_direct_plays(
        "h264",
        0,
        0,
        no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
        &caps
    ));
    assert!(
        !video_direct_plays(
            "hevc",
            1280,
            720,
            no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &caps
        ),
        "no decoder row, no direct play"
    );
    assert!(
        !video_direct_plays(
            "av1",
            1280,
            720,
            no_dv().presentation(SILENT, plx_platform::devcaps::dv::DvCapability::Supported, true),
            &caps
        ),
        "the pipeline cannot feed it at any size"
    );
}

/// A `/decision` body whose video lane carries this `decision` text (empty = the lane is present
/// but silent), or no video lane at all.
fn decision_body(video_lane: Option<&str>) -> plx_plex::plex::MediaContainer {
    let stream = match video_lane {
        Some(d) => format!(r#"[{{"streamType":1,"decision":"{d}"}},{{"streamType":2,"decision":"transcode"}}]"#),
        None => r#"[{"streamType":2,"decision":"transcode"}]"#.to_string(),
    };
    serde_json::from_str(&format!(
        r#"{{"Metadata":[{{"Media":[{{"Part":[{{"decision":"transcode","Stream":{stream}}}]}}]}}]}}"#
    ))
    .expect("a decision body")
}

fn contract(remux: bool, no_video_copy: bool) -> plx_plex::plex::EncodeContract {
    plx_plex::plex::EncodeContract { remux, no_video_copy, ..Default::default() }
}

/// The lane's own word wins; only a lane that says nothing follows the contract.
#[test]
fn decision_copies_video_follows_the_lane_then_the_contract() {
    assert!(decision_copies_video(&decision_body(Some("copy")), false), "copy is a copy under any contract");
    assert!(!decision_copies_video(&decision_body(Some("transcode")), true), "transcode re-encodes even for a remux");
    for lane in [Some(""), None] {
        assert!(decision_copies_video(&decision_body(lane), true), "a silent lane follows a remux");
        assert!(!decision_copies_video(&decision_body(lane), false), "a silent lane follows a re-encode");
    }
}

/// **`decision_lanes` is each lane's own word and never the contract.** The recovery ladder's proof
/// rule reads it, so a body that states nothing is `Unstated` on both lanes (where
/// [`decision_copies_video`] would follow the contract), and `copy`, `transcode` and `directplay`
/// map to their lanes; the FIRST stream of each type speaks for its lane.
#[test]
fn decision_lanes_reads_each_lane_and_never_the_contract() {
    use crate::player::recovery::{Lane, Lanes};
    let body = |streams: &str| -> plx_plex::plex::MediaContainer {
        serde_json::from_str(&format!(
            r#"{{"Metadata":[{{"Media":[{{"Part":[{{"decision":"transcode","Stream":{streams}}}]}}]}}]}}"#
        ))
        .expect("a decision body")
    };
    let lanes = |video, audio| Lanes { video, audio };
    let silent = lanes(Lane::Unstated, Lane::Unstated);
    let nothing: plx_plex::plex::MediaContainer = serde_json::from_str("{}").expect("an empty body");
    assert_eq!(decision_lanes(&nothing), silent, "no Metadata");
    assert_eq!(decision_lanes(&body("[]")), silent, "no streams");
    assert_eq!(decision_lanes(&body(r#"[{"streamType":1},{"streamType":2}]"#)), silent, "lanes without a decision");
    assert_eq!(
        decision_lanes(&body(r#"[{"streamType":1,"decision":""},{"streamType":2,"decision":"ignore"}]"#)),
        silent,
        "an empty or unknown word states nothing"
    );
    for (video, audio, want) in [
        ("copy", "transcode", lanes(Lane::Copy, Lane::Transcode)),
        ("directplay", "copy", lanes(Lane::DirectPlay, Lane::Copy)),
        ("transcode", "directplay", lanes(Lane::Transcode, Lane::DirectPlay)),
    ] {
        let streams = format!(r#"[{{"streamType":1,"decision":"{video}"}},{{"streamType":2,"decision":"{audio}"}}]"#);
        assert_eq!(decision_lanes(&body(&streams)), want, "{video} / {audio}");
    }
    assert_eq!(
        decision_lanes(&body(r#"[{"streamType":2,"decision":"copy"}]"#)),
        lanes(Lane::Unstated, Lane::Copy),
        "a lane with no stream is unstated beside one that speaks"
    );
    assert_eq!(
        decision_lanes(&body(
            r#"[{"streamType":1,"decision":"copy"},{"streamType":1,"decision":"transcode"},{"streamType":2,"decision":"transcode"},{"streamType":2,"decision":"copy"}]"#
        )),
        lanes(Lane::Copy, Lane::Transcode),
        "the first stream of each type speaks for its lane"
    );
}

/// The one rule every server-built route asks: the file's declaration rides a COPY of a usable
/// base layer, and nothing else. A missing `/decision` follows the contract, so a burn re-encode
/// whose decision failed never declares Dolby Vision over a stream the server re-encoded.
#[test]
fn copied_video_dv_declares_only_a_copy_of_a_usable_base_layer() {
    let none = (Dovi::NONE, DvDecision::NONE);
    let sentinel = DvDecision { capability: DvCapability::Supported, ..DvDecision::NONE };
    let src = (p8(), sentinel);
    let copy = decision_body(Some("copy"));
    let reencode = decision_body(Some("transcode"));

    assert_eq!(copied_video_dv(Some(&copy), &contract(true, false), src.0, src.1), src, "a copy keeps the declaration");
    assert_eq!(copied_video_dv(Some(&reencode), &contract(true, false), src.0, src.1), none, "a re-encode declares nothing");
    assert_eq!(copied_video_dv(Some(&copy), &contract(true, true), src.0, src.1), none, "no_video_copy overrides a copy answer");
    assert_eq!(copied_video_dv(Some(&copy), &contract(true, false), p5(), sentinel), none, "a Profile 5 copy is never declared");

    // No decision: the contract alone decides.
    assert_eq!(copied_video_dv(None, &contract(true, false), src.0, src.1), src, "a remux contract copies");
    assert_eq!(
        copied_video_dv(None, &contract(false, false), src.0, src.1),
        none,
        "a burn re-encode with no decision body must not declare (prepare_original_remux used to assume remux=true)",
    );
    assert_eq!(copied_video_dv(None, &contract(true, true), src.0, src.1), none);
}

/// The decision a route freezes stores the SET's answer as its `capability` and applies the
/// profile's Dolby Vision setting to the `presentation` only. Diagnostics and the `dv:` log lines
/// print that capability, so an Off profile on a Supported set must not read `unsupported`: a
/// quoted line has to tell it from a failed probe and from a set without Dolby Vision.
#[test]
fn off_on_a_supported_set_keeps_the_probe_answer_and_plays_the_base_layer() {
    use plx_plex::plex::session::DolbyVisionMode;

    let off = frozen_dv_decision(DolbyVisionMode::Off, DvCapability::Supported, p8(), "hevc");
    assert_eq!(off.capability, DvCapability::Supported, "the probe's answer, whatever the setting");
    assert_eq!(off.presentation, DvPresentation::NotDv, "Off plays the base layer");

    let auto = frozen_dv_decision(DolbyVisionMode::Auto, DvCapability::Supported, p8(), "hevc");
    assert_eq!(auto.capability, DvCapability::Supported);
    assert!(auto.presentation.declared().is_some(), "Auto trusts the probe and declares");

    for probed in [DvCapability::Unknown, DvCapability::Unsupported, DvCapability::Supported] {
        for mode in DolbyVisionMode::LADDER {
            assert_eq!(
                frozen_dv_decision(mode, probed, p8(), "hevc").capability,
                probed,
                "{mode:?} must not rewrite what the set said",
            );
        }
        // Off derives its presentation as an unsupported set would, on every answer the set gave.
        assert_eq!(
            frozen_dv_decision(DolbyVisionMode::Off, probed, p5(), "hevc").presentation,
            frozen_dv_decision(DolbyVisionMode::Auto, DvCapability::Unsupported, p5(), "hevc").presentation,
            "{probed:?}",
        );
    }
}
