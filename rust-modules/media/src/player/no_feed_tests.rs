//! **The doors the app calls whether or not a Feed is composed, answered for a session that
//! composed none.** Media holds no cargo feature for the Feed: a URI-only build is a session whose
//! `backends()` carry no Feed (`PlaybackSession::install_backends`, from the adapter's
//! `composed_backends()`), and each door that has a Feed arm answers what a URI-only build always
//! answered when that set is installed -- refused, idle, empty, "—" -- while the same door over a
//! Feed-composed session runs its Feed arm.

use super::*;
use crate::route::PlaybackSession;
use plx_playback_contract::BackendSet;
use readout::ReadoutEvent;

fn session(backends: BackendSet) -> PlaybackSession {
    let mut ps = PlaybackSession::IDLE;
    ps.install_backends(backends);
    crate::route::reset_player_control_for_test(&ps);
    ps
}

fn adapter() -> adapter::PlayerAdapter {
    adapter::PlayerAdapter::new(unsafe { MainThread::assume() })
}

#[test]
fn the_adapter_doors_refuse_when_no_feed_is_composed() {
    let _serial = plx_base::testlock::serial();
    let mut ps = session(BackendSet::URI);
    let mut pa = adapter();
    let audio = crate::route::CarriedAudio { sid: 9, ordinal: 0, codec: "aac".into(), channels: 2,
        can_normalize_loudness: false, immersive: false, sample_rate: 0, bit_depth: 0 };
    assert!(!pa.request_seek(&mut ps, 5_000_000_000), "no Feed, no URI attempt: no seek executor");
    assert!(!pa.set_play_rate(&mut ps, 1500));
    assert!(!pa.commit_audio_track(&mut ps, audio.clone(), None));
    assert!(!pa.commit_subtitle_track(&mut ps, 0, 0, None, String::new(), None));
    assert!(!pa.commit_forced_only_track(&mut ps, 0, 0, "eng", false));
    assert!(!pa.commit_audio_enhancement(&mut ps, plx_plex::plex::AudioEnhancements::NONE));
    // The same doors over a Feed-composed session run their Feed arm.
    let mut ps = session(BackendSet::FEED);
    assert!(pa.request_seek(&mut ps, 5_000_000_000));
    assert!(pa.commit_subtitle_track(&mut ps, 0, 0, None, String::new(), None));
    assert!(pa.commit_forced_only_track(&mut ps, 0, 0, "eng", false));
    assert!(pa.commit_audio_enhancement(&mut ps, plx_plex::plex::AudioEnhancements::NONE));
}

#[test]
fn the_derived_state_and_the_transport_are_idle_when_no_feed_is_composed() {
    let _serial = plx_base::testlock::serial();
    let mut ps = session(BackendSet::URI);
    let pa = adapter();
    ps.step_readout(ReadoutEvent::StateDerived(PlaybackState::Playing));
    ps.step_readout(ReadoutEvent::ReleaseQueue { queued: true });
    ps.step_readout(ReadoutEvent::CommitPaused { paused: true });
    assert_eq!(state(&ps), PlaybackState::Idle, "a queued start and a Feed state are no URI-only facts");
    assert!(!transport::viewer_paused_for(&ps, &pa), "the Feed's pause is not the viewer's");
    let mut ps_set = session(BackendSet::URI);
    let mut pa = adapter();
    assert!(!transport::set_transport_paused(&mut ps_set, &mut pa, false), "no attempt is Starting or Active");
    // Composed with a Feed, the same readout speaks.
    ps.install_backends(BackendSet::FEED);
    assert_eq!(state(&ps), PlaybackState::Resolving, "the queued start is the Feed's release queue");
    assert!(transport::viewer_paused_for(&ps, &pa));
    let mut ps_fed = session(BackendSet::FEED);
    assert!(transport::set_transport_paused(&mut ps_fed, &mut pa, false), "already playing: satisfied");
}

#[test]
fn the_readouts_wording_and_offers_follow_the_composition() {
    let _serial = plx_base::testlock::serial();
    let mut ps = session(BackendSet::URI);
    let pa = adapter();
    ps.native_start_refused = Some(crate::route::StartRefusal::LoadTimeout);
    assert_eq!(live_runtime_failure(&ps), RuntimeFailure::Unknown, "a parked Load is the Feed's");
    assert_eq!(diag(&ps, &pa).vp_mode_str(), "—");
    assert!(speed_offer(&ps).is_empty());
    assert_eq!(route_fx::side_reader_dev_armed_ordinal(&ps), None);
    ps.install_backends(BackendSet::FEED);
    assert_ne!(live_runtime_failure(&ps), RuntimeFailure::Unknown);
    assert_eq!(diag(&ps, &pa).vp_mode_str(), "NONE — no video path");
}

#[test]
fn the_dev_trigger_arms_the_side_reader_only_for_a_composed_feed() {
    let _serial = plx_base::testlock::serial();
    plx_base::devtrig::with_private_triggers(|| {
        std::fs::write(plx_base::devtrig::path("subside"), "2\n").unwrap();
        assert_eq!(route_fx::side_reader_dev_armed_ordinal(&session(BackendSet::URI)), None);
        assert_eq!(route_fx::side_reader_dev_armed_ordinal(&session(BackendSet::FEED)), Some(2));
    });
}
