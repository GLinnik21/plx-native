//! Issue #266: `CarriedAudio` — the frozen audio-track snapshot that replaced the four
//! independent `audio_sid`/`audio_ordinal`/`acodec`/`immersive` fields it now carries together.
//! The offering policy has its own file (`plan_audio_enhancement_tests.rs`); these tests pin the
//! surrounding mechanics: what
//! `from_stream` actually reads off a `metadata::Stream`, and that `None` — "server default,
//! facts unknown" — survives every projection/pending/candidate round trip unchanged rather than
//! being turned into a confident `Some` by a struct-literal default somewhere along the way.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::apply_plan;

/// `has_atmos` (and therefore `CarriedAudio::immersive`) reads `Stream.profile` alone — Dolby's
/// own spec says the channel layout cannot tell Atmos apart from plain surround, and PMS composes
/// `"dolby digital plus + dolby atmos"` for a JOC track regardless of its ordinary 5.1 layout.
#[test]
fn from_stream_eac3_joc_is_immersive() {
    let s = crate::metadata::Stream {
        id: 7,
        codec: "eac3".into(),
        channels: 6,
        profile: "dolby digital plus + dolby atmos".into(),
        can_normalize_loudness: true,
        ..Default::default()
    };
    let carried = CarriedAudio::from_stream(&s, 2);
    assert_eq!(carried.sid, 7);
    assert_eq!(carried.ordinal, 2);
    assert_eq!(carried.codec, "eac3");
    assert_eq!(carried.channels, 6);
    assert!(carried.can_normalize_loudness);
    assert!(carried.immersive, "a JOC profile must read as immersive");
}

/// Plain AC-3 (Dolby Digital, no JOC layer) never carries Atmos — its `profile` names no such
/// thing, and `has_atmos` is deliberately a substring test on that field alone.
#[test]
fn from_stream_ac3_not_immersive() {
    let s = crate::metadata::Stream {
        id: 9,
        codec: "ac3".into(),
        channels: 6,
        profile: String::new(),
        can_normalize_loudness: false,
        ..Default::default()
    };
    let carried = CarriedAudio::from_stream(&s, 0);
    assert_eq!(carried.codec, "ac3");
    assert!(!carried.can_normalize_loudness);
    assert!(!carried.immersive, "plain AC-3 is never Atmos");
}

/// `None` — "server default, facts unknown" — must survive every mechanical round trip this PR's
/// refactor introduced unchanged: the per-frame [`PlaybackSession::publication`] copy, the
/// [`route_projection`]/[`install_route_projection`] commit pair, and a [`PendingOriginal`]
/// snapshot/restore transaction. A stray struct-literal default turning `None` into `Some` at any
/// of these seams would silently invent capability facts for a track nobody read.
#[test]
fn none_round_trips_publication_projection_pending() {
    let ps = PlaybackSession::IDLE;
    assert!(ps.cur_audio.is_none());

    // The per-frame screen copy.
    let published = ps.publication();
    assert!(published.cur_audio.is_none());

    // The route-projection commit pair: installing a `None` projection must overwrite a stale
    // `Some` left over from a previous route, not leave it behind.
    let projection = route_projection(&ps);
    assert!(projection.audio.is_none());
    let mut installed = PlaybackSession::IDLE;
    installed.cur_audio = Some(CarriedAudio {
        sid: 99,
        ordinal: 0,
        codec: "aac".into(),
        channels: 2,
        can_normalize_loudness: false,
        immersive: false,
    });
    install_route_projection(&mut installed, &projection);
    assert!(
        installed.cur_audio.is_none(),
        "a None projection must overwrite a stale Some"
    );

    // A native-Original recovery trial's snapshot/restore path.
    let mut pending = snapshot_route(&ps, "enc-1".into(), 0);
    assert!(pending.cur_audio.is_none());
    assert!(pending.deferred_audio.is_none());
    let effects = DeferredOriginalEffects::from_pending(&mut pending);
    assert!(effects.is_empty());
    assert!(pending.deferred_audio.is_none());
}

/// [`auto_original_features`] is what an HLS controller consults for the "would going back to
/// Original recover something the current re-encode can't give back" prior — Dolby Vision and
/// Atmos. A candidate whose `audio` is `None` (server default, facts unknown) must fail CLOSED
/// exactly the way the pre-refactor flat `immersive: bool` field always defaulted to `false`:
/// nothing here may read an unknown track as immersive just because nobody said otherwise.
#[test]
fn candidate_audio_none_matches_legacy_payload() {
    let mut ps = PlaybackSession::IDLE;
    let mut candidate = test_original_candidate(None);
    candidate.audio = None;
    ps.auto_original = Some(candidate);
    let features = auto_original_features(&ps);
    assert!(
        !features.atmos,
        "an unknown audio track must never read as immersive"
    );

    // The known-track candidate this same helper builds by default does carry Atmos, which is
    // the contrast that makes the `None` case above meaningful rather than vacuous.
    let mut ps_known = PlaybackSession::IDLE;
    ps_known.auto_original = Some(test_original_candidate(None));
    assert!(auto_original_features(&ps_known).atmos);

    // …and through the recovery that actually INSTALLS the candidate. The legacy flat fields this
    // replaced gave a server-default candidate the file's own default codec (`acodec` = the
    // resolve's source argument), ordinal -1 (leave the demuxer on the default track) and no
    // Atmos. `None` must reproduce that payload exactly: an empty codec here is a Load payload
    // describing no audio at all.
    let mut ps = PlaybackSession::IDLE;
    let _g = fresh_registry(&mut ps);
    restore_quality(Quality::Auto);
    let mut candidate = test_original_candidate(None);
    candidate.audio = None;
    apply_plan(
        &mut ps,
        Plan {
            url: "https://example.invalid/hls/master.m3u8".into(),
            tsession: "encoder-1".into(),
            vcodec: "h264".into(),
            acodec: "aac".into(),
            src_vcodec: "hevc".into(),
            src_acodec: "eac3".into(),
            contract: crate::plex::EncodeContract {
                delivery: crate::plex::TranscodeDelivery::FixedHls { seconds_per_segment: 2 },
                ceiling: Some(crate::abr::Rung::P1080High.ceiling()),
                ..Default::default()
            },
            transport_kbps: 28_000,
            auto_original: Some(candidate),
            ..Default::default()
        },
        "rk-auto",
    );
    crate::player::set_audio_track(3);
    assert_eq!(recover_auto_to_original(&mut ps, 120), Some(AutoOriginalReload::Direct));
    assert_eq!(stream_acodec(&ps), "eac3", "the file's own default codec, as the legacy payload");
    assert_eq!(
        crate::player::SHARED.desired_audio_idx.load(std::sync::atomic::Ordering::Relaxed),
        -1,
        "no known ordinal: the demuxer feeds the file's default track",
    );
    assert!(!stream_immersive(&ps), "an unknown track never declares Atmos");
    restore_quality(Quality::Original);
    reset_session(&mut ps);
    install_active_encoder("");
    crate::player::reset_audio_track();
    crate::player::reset_subtitle();
}
