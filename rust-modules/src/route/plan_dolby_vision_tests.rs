//! Dolby Vision direct-play gating tests: profile 5/7/8 declarations, base-layer
//! usability, and the device-bound/dimension checks that compose with them.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **The bug this gate exists for.** Profile 5 is single-layer IPT-PQ with no HDR10 fallback,
/// so feeding its base layer to an ordinary HEVC decoder produces a picture in visibly wrong
/// colours — and nothing else in the ladder can see that: the codec is `hevc` (fine), the
/// frame size clears the dev TV's bound (fine), the container is mp4, which has direct-played
/// since 2026-08-11 (fine). Every gate passes and the user gets a broken picture.
#[test]
fn a_profile_5_source_does_not_direct_play_undeclared() {
    let caps = crate::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176), // the dev TV's own bound — this must fail on SIZE grounds nowhere
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "aac,ac3,eac3".into(),
    };
    // the live P5 item's own shape: 3840x1602 hevc, well inside the bound
    assert!(
        !video_direct_plays("hevc", 3840, 1602, p5().presentation(SILENT), &caps),
        "IPT-PQ has no HDR10 base layer"
    );
    // and it is the DV fields doing it, not the size or the codec: the same file without them
    // direct-plays, which is exactly the behaviour that shipped the wrong colours
    assert!(video_direct_plays(
        "hevc",
        3840,
        1602,
        no_dv().presentation(SILENT),
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
    let caps = crate::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "aac,ac3,eac3".into(),
    };
    let dv = p5().presentation(DECLARED);
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
    assert!(!video_direct_plays("av1", 3840, 1602, dv, &caps));
    let small = crate::devcaps::Caps {
        hevc_max: (1920, 1088),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        ..caps.clone()
    };
    assert!(!video_direct_plays("hevc", 3840, 1602, dv, &small));
}


/// Profile 7 is dual-layer: the picture is split across a base and an enhancement layer, and
/// the pipeline feeds ONE elementary stream. Caught by `el_present` alone — the live P7 item
/// reports `bl_compat = 6`, so a compatibility-id test would wave it straight through.
#[test]
fn a_dual_layer_profile_7_source_does_not_direct_play() {
    let caps = crate::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "eac3".into(),
    };
    // and it is refused in BOTH worlds: no payload key can hand the pipeline a layer we do
    // not feed it, so arming the trigger must not open this gate the way it opens P5's
    for signal in [SILENT, DECLARED] {
        let dv = p7().presentation(signal);
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
    assert_ne!(
        p7().bl_compat,
        0,
        "the fixture must keep the trap it was built to hold"
    );
}


/// **Profile 8.1 must be UNAFFECTED**, and so must every file with no DOVI record at all.
/// P8's base layer IS an HDR10 stream, so ignoring the RPU costs the dynamic metadata and
/// nothing else — the 21-case on-device suite includes a passing P8 case (`dp_hevc_eac3_dovi_p8`)
/// and this change must not move it.
#[test]
fn profile_8_and_plain_files_are_unaffected() {
    let caps = crate::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "aac,ac3,eac3".into(),
    };
    for signal in [SILENT, DECLARED] {
        assert!(
            video_direct_plays("hevc", 3840, 2160, p8().presentation(signal), &caps),
            "HDR10-compatible base layer (signal={signal})"
        );
        assert!(video_direct_plays(
            "hevc",
            3840,
            2160,
            no_dv().presentation(signal),
            &caps
        ));
        assert!(video_direct_plays(
            "h264",
            1920,
            1080,
            no_dv().presentation(signal),
            &caps
        ));
        assert_eq!(p8().presentation(signal).refusal(), None);
        assert_eq!(no_dv().presentation(signal).refusal(), None);
    }
    // A file with no Dolby Vision at all declares nothing however the trigger is set — the
    // node is a statement about the stream, not a mode the app is in.
    assert_eq!(no_dv().presentation(DECLARED).declared(), None);
    // P8 declares in BOTH settings, and that is deliberate: its base layer is HDR10 either
    // way, so the node costs nothing and adds the dynamic metadata the RPU carries. The
    // trigger reaches only the profile whose declaration is not yet free — P5, measured to
    // lose two frames every ~40 s on this set. `SILENT` here is the half that would silently
    // regress if the gate were ever rewritten as a bare `signal &&`.
    for signal in [SILENT, DECLARED] {
        assert_eq!(
            p8().presentation(signal).declared().map(|n| n.profile_id),
            Some(8),
            "a cross-compatible base layer declares without the trigger: signal={signal}"
        );
    }
    assert_eq!(
        p5().presentation(SILENT).declared(),
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
        assert_eq!(Dovi::default().presentation(signal), DvPresentation::NotDv);
        assert_eq!(
            bare.presentation(signal),
            DvPresentation::NotDv,
            "signal={signal}"
        );
        assert_eq!(contradictory.presentation(signal), DvPresentation::NotDv);
        assert_eq!(
            el_only.presentation(signal),
            DvPresentation::Refuse("dual-layer")
        );
    }
}


/// **The gate and the payload are one predicate, and this is the property that says so.**
/// Every shape the server can report, in both trigger settings: whatever the answer, direct
/// play is allowed exactly when a node will be sent or there was no Dolby Vision to declare,
/// and refused exactly when there is Dolby Vision we are not declaring. The pair that must
/// never occur is a direct play with an undeclared DV stream — that IS the wrong-colours bug —
/// and its mirror, a refusal carrying a node nobody will ever send.
#[test]
fn the_direct_play_gate_and_the_payload_node_can_never_disagree() {
    let caps = crate::devcaps::Caps {
        hevc: true,
        hevc_max: (4096, 2176),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "aac,ac3,eac3".into(),
    };
    let bare = Dovi {
        present: true,
        profile: 0,
        bl_compat: 0,
        el_present: false,
        ..Dovi::NONE
    };
    for d in [no_dv(), p5(), p7(), p8(), bare] {
        for signal in [SILENT, DECLARED] {
            let dv = d.presentation(signal);
            let plays = video_direct_plays("hevc", 3840, 1602, dv, &caps);
            assert_eq!(plays, dv.refusal().is_none(), "{d:?} signal={signal}");
            assert!(
                !(dv.refusal().is_some() && dv.declared().is_some()),
                "{d:?}"
            );
            // and a refusal always implies the COPY refusal beside it — `build_stream`'s
            // `no_video_copy` reads `base_layer_unusable`, and its log line at the refusal
            // says "(no copy)" in so many words. If a shape could be refused while a copy of
            // it stayed permitted, the item would come back byte-identical from the server.
            if dv.refusal().is_some() {
                assert!(
                    d.base_layer_unusable(),
                    "a refusal must also withdraw the copy: {d:?}"
                );
            }
            // **The one that matters, and it is now unconditional.** A direct-played Dolby
            // Vision stream is a DECLARED one — in either trigger setting, for every shape.
            // It reads as a strengthening and it is one: while the trigger gated every
            // declaration this could only be asserted as `== signal`, which quietly permitted
            // the wrong-colours pair for any profile the trigger happened to be off for. Now
            // the only undeclared DV is refused DV, so the implication holds outright.
            if plays && d.present && d.profile > 0 {
                assert!(dv.declared().is_some(), "{d:?} signal={signal}");
            }
            if let Some(n) = dv.declared() {
                assert_eq!(n.profile_id, d.profile);
                // `trackType:"dual"` with `encryptionType:"all"` is what sets the pipeline's
                // `dv-dual-svp` secure-video-path flag, which this app cannot satisfy. No
                // input may produce that pair.
                assert!(
                    !(n.track_type == "dual" && n.encryption_type == "all"),
                    "dv-dual-svp"
                );
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
    assert!(p7().base_layer_unusable());
    assert!(!p8().base_layer_unusable());
    assert_eq!(
        p5().presentation(SILENT).refusal(),
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
    let aac = [crate::metadata::Stream {
        codec: "aac".into(),
        ..Default::default()
    }];
    let part = "/library/parts/1/2/movie.mp4";
    assert_eq!(
        playback_preview_of(part, "hevc", 1920, 1080, p5().presentation(SILENT), &aac),
        Some(Preview::Converts),
        "the server must re-encode it — a container remux would copy the same wrong pixels"
    );
    // the identical item without the DV record is a plain direct play, so the preview is
    // reading the new field and not something else that happens to differ
    assert_eq!(
        playback_preview_of(part, "hevc", 1920, 1080, no_dv().presentation(SILENT), &aac),
        Some(Preview::DirectPlay)
    );
    assert_eq!(
        playback_preview_of(part, "hevc", 1920, 1080, p8().presentation(SILENT), &aac),
        Some(Preview::DirectPlay)
    );
    // and the page must follow the inversion, or the facts row promises a conversion the
    // route no longer performs — the preview reads the same predicate the gate does
    assert_eq!(
        playback_preview_of(part, "hevc", 1920, 1080, p5().presentation(DECLARED), &aac),
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
    let caps = crate::devcaps::Caps {
        hevc: true,
        hevc_max: (1920, 1088),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "aac,ac3,eac3".into(),
    };
    // the codec agrees; the frame size must still refuse — on either codec
    assert!(!video_direct_plays(
        "h264",
        3840,
        2160,
        no_dv().presentation(SILENT),
        &caps
    ));
    assert!(!video_direct_plays(
        "hevc",
        3840,
        2160,
        no_dv().presentation(SILENT),
        &caps
    ));
    // one axis over is over (per-axis bound, not an area heuristic)
    assert!(!video_direct_plays(
        "h264",
        4096,
        1080,
        no_dv().presentation(SILENT),
        &caps
    ));
    // within the bound plays, exactly at it included (1088 IS the table's number)
    assert!(video_direct_plays(
        "h264",
        1920,
        1088,
        no_dv().presentation(SILENT),
        &caps
    ));
}


/// Unknown dimensions fail OPEN (0 = PMS never measured the file — not evidence of 4K, and
/// yesterday's behavior for it), while the codec half keeps gating regardless.
#[test]
fn unknown_dimensions_fail_open_and_the_codec_half_still_gates() {
    let caps = crate::devcaps::Caps {
        hevc: false,
        hevc_max: (1920, 1088),
        h264_row: (0, 0, 0),
        hevc_row: (0, 0, 0),
        vp9: false,
        audio: "aac".into(),
    };
    assert!(video_direct_plays(
        "h264",
        0,
        0,
        no_dv().presentation(SILENT),
        &caps
    ));
    assert!(
        !video_direct_plays("hevc", 1280, 720, no_dv().presentation(SILENT), &caps),
        "no decoder row, no direct play"
    );
    assert!(
        !video_direct_plays("av1", 1280, 720, no_dv().presentation(SILENT), &caps),
        "the pipeline cannot feed it at any size"
    );
}

