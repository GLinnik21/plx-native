//! Quality-ceiling and source-probe tests: Original/Auto quality policy, remote source
//! conservation, ceiling ladder gates, and the metadata a `Plan` derives from a loaded item.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn a_partial_source_body_is_not_traced_as_a_successful_measurement() {
    use crate::player::report::TraceOutcome;
    let sample = |target_reached| crate::curlio::ThroughputSample {
        bytes: 64 * 1024,
        elapsed: std::time::Duration::from_millis(500),
        target_reached,
    };
    assert_eq!(
        source_probe_sample_outcome(sample(false)),
        TraceOutcome::Inconclusive,
        "a right-censored non-empty prefix cannot claim the requested sample completed",
    );
    assert_eq!(
        source_probe_sample_outcome(sample(true)),
        TraceOutcome::Succeeded,
    );
}


/// **GATE 1 — Original changes nothing, for any source, on any link.** It is the migration and
/// readiness fallback: a ceiling that leaked into it would change every existing install.
/// Note the unmeasured row in particular — `Ceiling::admits` fails CLOSED, and that rule must
/// not be reachable at all without a fixed rung selected.
#[test]
fn original_is_unchanged_and_auto_original_is_an_explicit_measured_state() {
    for src in [UHD_REMUX, HD_BIG, HD_SMALL, UNMEASURED] {
        assert_eq!(
            quality_policy(Quality::Original, false, src.0, src.1, src.2),
            crate::plex::LinkPolicy::UNRESTRICTED,
            "Original must restrict nothing, and {src:?} is not an exception"
        );
        assert_eq!(
            quality_policy(Quality::Auto, false, src.0, src.1, src.2),
            crate::plex::LinkPolicy {
                direct_play: false,
                remux: false
            },
            "Auto without a positive Original measurement must use HLS"
        );
        assert_eq!(
            quality_policy(Quality::Auto, true, src.0, src.1, src.2),
            crate::plex::LinkPolicy::UNRESTRICTED,
            "Auto's proven Original state must not start an encoder"
        );
        // …and composed, on every link tier, Original is exactly what the link alone said.
        for link in [
            None,
            Some(crate::plex::probe::Location::Local),
            Some(crate::plex::probe::Location::Remote),
            Some(crate::plex::probe::Location::Relay),
        ] {
            assert_eq!(
                allowed(link, Quality::Original, src),
                crate::plex::link_policy(link),
                "Original changed the answer for link {link:?} on {src:?}"
            );
        }
    }
    // Neither mode carries a fixed ceiling. The parameter half of Original's claim remains
    // the transcoder test that a `None` ceiling produces the pre-ceiling literals.
    assert_eq!(Quality::Auto.ceiling(), None);
    assert_eq!(Quality::Original.ceiling(), None);
}


#[test]
fn auto_is_available_only_on_the_positive_readiness_side() {
    assert_eq!(quality_ladder_for(false).first(), Some(&Quality::Original));
    assert!(!quality_ladder_for(false).contains(&Quality::Auto));
    assert_eq!(quality_ladder_for(true), &QUALITY_LADDER);
    assert_eq!(
        quality_ladder_for(true)[..2],
        [Quality::Auto, Quality::Original]
    );
    assert!(
        auto_quality_ready(),
        "the integrated HLS prime/swap path owns production Auto"
    );
    assert_eq!(supported_quality(Quality::Auto), Quality::Auto);
}


/// The cold-start admission rule now lives in `abr::bootstrap`, and this grades the composition
/// this file is responsible for: a curl sample turned into an observation, and the LINK CLASS
/// deciding whether the probe is consulted at all.  The boundary is conservation, not an
/// arbitrary headroom multiplier: a completed prefix is sustainable exactly when its arrival
/// rate is at least the source consumption rate.
#[test]
fn remote_original_uses_the_completed_source_conservation_test() {
    let policy = crate::abr::AbrPolicy::measured();
    let catalog = crate::abr::HlsActuatorCatalog::measured();
    let observation = |bytes: u64, ms: u64, complete: bool| crate::abr::CapacityObservation {
        kbps: u32::try_from(
            crate::curlio::ThroughputSample {
                bytes,
                elapsed: std::time::Duration::from_millis(ms),
                target_reached: complete,
            }
            .kbps(),
        )
        .unwrap(),
        bytes: bytes as u64,
        active_us: ms * 1_000,
        completed: complete,
    };
    let fast = observation(1_000_000, 500, true);
    assert_eq!(fast.kbps, 16_000);
    let go = |source, probe| {
        crate::abr::bootstrap(
            crate::abr::LinkKind::Remote,
            true,
            source,
            Some(probe),
            &catalog,
            &policy,
        )
        .original
    };
    assert!(go(10_000, fast));
    assert!(
        go(10_000, observation(1_000_000, 800, true)),
        "a completed 12.5 Mbit/s prefix sustains a 10 Mbit/s source without a hidden margin"
    );
    assert!(
        !go(10_000, observation(1_000_000, 801, true)),
        "a completed prefix just below 10 Mbit/s does not sustain that source"
    );
    assert!(
        !go(10_000, observation(1_000_000, 500, false)),
        "a truncated probe proves a floor"
    );
    assert!(
        !go(0, fast),
        "an unknown source bitrate cannot be reasoned about"
    );
    // ...and neither of the other two link classes consults a probe at all.
    for link in [crate::abr::LinkKind::Local, crate::abr::LinkKind::Relay] {
        let decision = crate::abr::bootstrap(link, true, 10_000, None, &catalog, &policy);
        assert_eq!(decision.original, link == crate::abr::LinkKind::Local);
    }
}


#[test]
fn remote_probe_samples_one_second_but_has_strict_memory_bounds() {
    assert_eq!(remote_probe_target_bytes(0), None);
    assert_eq!(
        remote_probe_target_bytes(720),
        Some(crate::abr::SOURCE_PROBE_MIN_BYTES),
    );
    assert_eq!(remote_probe_target_bytes(8_000), Some(1_000_000));
    assert_eq!(
        remote_probe_target_bytes(200_000),
        Some(crate::abr::SOURCE_PROBE_MAX_BYTES),
    );
}


/// **GATE 2 — under-ceiling content keeps the fast paths.** Picking "1080p · 8 Mbps" must not
/// send a 3 Mbit/s 720p episode to an encoder: there is nothing there for a transcode to fix,
/// and doing it anyway would cost the server a job and the picture a generation. This is the
/// assertion that stops the feature from degenerating into "a rung means always transcode".
#[test]
fn a_source_measured_under_the_ceiling_stays_direct_play_eligible() {
    let p = allowed(None, Quality::P1080, HD_SMALL);
    assert!(
        p.direct_play,
        "3 Mbps 720p is under 8 Mbps 1080p — nothing to fix"
    );
    assert!(
        p.remux,
        "…and a container remux of it is under the ceiling too"
    );
    // true right down the ladder, until the rung actually bites
    assert!(
        allowed(None, Quality::P720, HD_SMALL).direct_play,
        "3 Mbps 720p fits 4 Mbps 720p"
    );
    assert!(
        !allowed(None, Quality::P720Low, HD_SMALL).direct_play,
        "…but not 2 Mbps"
    );
}


/// **GATE 3 — over-ceiling loses DIRECT PLAY, and this is the whole point.** A 30 Mbit/s 1080p
/// file is the case a bitrate field on `TranscodeSpec` cannot touch: direct play streams the
/// file's own bytes and no encoder ever reads the number. Refusing the flavor is the only
/// thing that makes a cap mean anything.
///
/// Both axes refuse independently — over on RATE alone (the 1080p file against a 1080p rung)
/// and over on FRAME alone (a 4K source against a 1080p rung, at a rate the rung allows).
#[test]
fn a_source_over_the_ceiling_is_refused_direct_play() {
    assert!(
        !allowed(None, Quality::P1080, HD_BIG).direct_play,
        "30 Mbps is over the 8 Mbps rung"
    );
    assert!(
        !allowed(None, Quality::P1080, (4000, 3840, 2160)).direct_play,
        "4K is over a 1080p rung"
    );
    // …and the unmeasured source fails CLOSED, which is the rule that makes a rung mean
    // something on a play from a shelf that never loaded a detail page.
    assert!(!allowed(None, Quality::P1080, UNMEASURED).direct_play,
        "an unmeasured source cannot be PROVEN under the ceiling, so it takes the branch that applies one");
}


/// **GATE 4 — over-ceiling loses the REMUX too**, and this is the half a "force a transcode"
/// instinct leaves behind, because a remux *feels* like a concession already. It is not: it
/// copies the codecs and its query deliberately carries no cap, so it is the same 30 Mbit/s
/// one container down. `link_policy` states this for the relay; a user ceiling inherits it
/// unchanged, and what survives is the re-encode.
#[test]
fn a_source_over_the_ceiling_is_refused_the_remux_as_well() {
    let p = allowed(None, Quality::P1080, HD_BIG);
    assert!(
        !p.remux,
        "a remux is the same bytes at the same rate, one layer down"
    );
    assert_eq!(
        p,
        crate::plex::LinkPolicy {
            direct_play: false,
            remux: false
        }
    );
    // A 4K remux — the flavor that exists to keep 4K/HDR intact — is exactly what a low rung
    // has to refuse, or the rung buys nothing on the biggest files in the library.
    assert!(!allowed(None, Quality::P720, UHD_REMUX).remux);
}


/// **GATE 6 — the link's policy and the user's compose to the STRICTER, per flavor.** A relay
/// must not be loosened by picking a high rung (the tunnel is 2 Mbit/s whatever the user
/// thinks), and a low rung must not be loosened by a fast LAN link. Graded as a full product
/// of both axes rather than one example, because a `||` typed for a `&&` passes any single
/// case that happens to agree.
#[test]
fn a_relay_link_and_a_user_ceiling_compose_to_the_stricter_of_the_two() {
    for q in QUALITY_LADDER {
        for src in [UHD_REMUX, HD_BIG, HD_SMALL, UNMEASURED] {
            // relay denies both, and NOTHING a user can pick gives either back
            assert_eq!(
                allowed(Some(crate::plex::probe::Location::Relay), q, src),
                crate::plex::LinkPolicy {
                    direct_play: false,
                    remux: false
                },
                "a relay was loosened by rung {q:?} on {src:?}"
            );
            // and on an unrestricted link the answer is the user's policy, unchanged
            for link in [
                None,
                Some(crate::plex::probe::Location::Local),
                Some(crate::plex::probe::Location::Remote),
            ] {
                let auto_original =
                    q == Quality::Auto && link == Some(crate::plex::probe::Location::Local);
                assert_eq!(
                    allowed(link, q, src),
                    quality_policy(q, auto_original, src.0, src.1, src.2),
                    "link {link:?} altered rung {q:?} on {src:?}"
                );
            }
        }
    }
}


/// **Press Play on a SHOW page and the detail's `rk` is the show's, not the episode's.** An
/// rk-only test therefore missed on the commonest path in the app, `src_kbps` fell to 0, and
/// `Ceiling::admits` fails closed — so with any rung selected every episode in the library
/// lost direct play, while `playback_preview` (reading the same `Detail`'s numbers directly)
/// still promised Direct Play for it. Two answers to one question.
///
/// The server half is graded on both arms: a ratingKey names an item only within one server.
#[test]
fn the_loaded_detail_describes_its_own_key_and_its_on_deck_episodes() {
    let a = crate::plex::ServerId::from_raw(1);
    let b = crate::plex::ServerId::from_raw(2);
    let show = crate::metadata::Detail {
        sid: a,
        rk: "100".into(),
        on_deck: Some(crate::metadata::Episode {
            rk: "205".into(),
            ..Default::default()
        }),
        ..Default::default()
    };
    assert!(detail_describes(&show, a, "100"), "its own key");
    assert!(
        detail_describes(&show, a, "205"),
        "the episode Play would actually start"
    );
    assert!(
        !detail_describes(&show, a, "206"),
        "a different episode is not this one"
    );
    // …and neither key may match across servers, or the ceiling judges the wrong file
    assert!(!detail_describes(&show, b, "100"));
    assert!(!detail_describes(&show, b, "205"));
    // a movie has no on-deck episode and must still answer for itself
    let movie = crate::metadata::Detail {
        sid: a,
        rk: "7".into(),
        ..Default::default()
    };
    assert!(detail_describes(&movie, a, "7"));
    assert!(!detail_describes(&movie, a, "100"));
}


#[test]
fn a_trailer_play_judges_the_extra_file_not_the_parent_or_zero() {
    let a = crate::plex::ServerId::from_raw(1);
    let movie = crate::metadata::Detail {
        sid: a,
        rk: "7".into(),
        bitrate: 48_000,
        video: Some(crate::metadata::Stream {
            bitrate: 40_000,
            ..Default::default()
        }),
        extras: vec![crate::metadata::Extra {
            rk: "9".into(),
            bitrate: 2_500,
            part: "/p".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    assert_eq!(resolve_src_kbps(Some(&movie), a, "7"), 40_000);
    assert_eq!(
        resolve_src_kbps(Some(&movie), a, "9"),
        2_500,
        "the extra's own rate, not the feature's"
    );
    assert_eq!(
        resolve_src_kbps(Some(&movie), a, "8"),
        0,
        "an unrelated key is still unmeasured"
    );
    assert!(
        quality_policy(Quality::P1080, false, 2_500, 1280, 720).direct_play,
        "a small extra fits the 1080p · 8 Mbps rung"
    );
    assert!(
        !quality_policy(Quality::P1080, false, 0, 1280, 720).direct_play,
        "src_kbps=0 is the fail-closed hole this play used to hit"
    );
    assert!(
        !quality_policy(Quality::P1080, false, 40_000, 3840, 2160).direct_play,
        "the parent's 4K figure would have forced a transcode"
    );
}


/// **The ceiling is spent as `maxVideoBitrate`, so it must be judged against the VIDEO rate.**
/// `Detail::bitrate` is the whole-file figure — video plus every audio track — and comparing
/// that against a video-only cap makes each rung bite about one AC-3 track early. The video
/// stream's own number is preferred where PMS sent one; the whole-file figure is the fallback,
/// which is the conservative direction and so the right one.
#[test]
fn the_source_rate_is_the_video_streams_own_where_the_server_gave_one() {
    let with_video = crate::metadata::Detail {
        bitrate: 8540, // 7900 video + a 640 kbps AC-3 track
        video: Some(crate::metadata::Stream {
            bitrate: 7900,
            ..Default::default()
        }),
        ..Default::default()
    };
    assert_eq!(source_kbps(&with_video), 7900);
    // …which is what keeps it under an 8 Mbps rung its VIDEO does in fact fit
    assert!(
        quality_policy(Quality::P1080, false, source_kbps(&with_video), 1920, 1080).direct_play
    );
    assert!(
        !quality_policy(Quality::P1080, false, with_video.bitrate, 1920, 1080).direct_play,
        "the whole-file figure is what made the rung bite early — this is the bug, pinned"
    );

    // no video record (a show with no episode backfill, an audio-only part) → whole-file
    let bare = crate::metadata::Detail {
        bitrate: 8540,
        ..Default::default()
    };
    assert_eq!(source_kbps(&bare), 8540);
    // a video record PMS gave no bitrate for is not a measurement of 0 — fall back
    let unmeasured_stream = crate::metadata::Detail {
        bitrate: 8540,
        video: Some(crate::metadata::Stream::default()),
        ..Default::default()
    };
    assert_eq!(source_kbps(&unmeasured_stream), 8540);
    // nothing said at all stays 0, which `Ceiling::admits` fails closed on
    assert_eq!(source_kbps(&crate::metadata::Detail::default()), 0);
}

