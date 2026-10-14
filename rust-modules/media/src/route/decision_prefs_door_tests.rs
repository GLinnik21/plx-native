//! **The declared deltas of the one write door** (`track-memory-plan.md` §3.7, deltas 2, 4 and 6;
//! §3.8.7, P-d3), each a test written against the old code first and watched fail there. The
//! characterisation file beside this one pins what `main` did; this one pins what the door does
//! differently, on purpose, and nothing else. Delta 2 was reversed by the per-profile work: a
//! player quality pick is a profile's own value now, so it is account data again (P-d3).
use super::prefs_characterisation_tests::{
    defaults, picked, read, recv, sign_in, stored, writes, LogMark, Restore, Unwritable, TOKEN, WRITE_FAILED,
};
use super::*;
use plx_plex::plex::session::{
    self as session, DeckPress, NextEpisodeMode, QualityPref, SkipInterval, SkipIntervalPref, SubtitlePosition,
    SubtitleSize, SubtitleTone, SubtitleTonePref,
};
use plx_plex::plex::AudioEnhancements;
use std::sync::mpsc;

/// **Delta 6.** Picking the value the record already holds writes nothing, and says it is saved
/// (`read && unchanged`), even when the disk would refuse a write: there is nothing to refuse.
/// The Confirm rows publish and report on it; size and position reply `true`.
#[test]
fn confirm_repick_of_the_current_value_writes_nothing_and_reports_saved() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("door-repick");
    sign_in(&session);
    let _restore = Restore::take();
    defaults();

    // Land one explicit value per row, so a re-pick is a re-pick of what the file holds.
    assert!(set_direct_play_mode(DirectPlayMode::Forced));
    assert!(set_next_episode_mode(NextEpisodeMode::AfterCredits));
    assert!(set_skip_interval(SkipInterval::Seconds30));
    assert!(set_deck_press(DeckPress::Play));
    assert!(set_default_quality(Quality::P720));
    let (tx, rx) = mpsc::channel();
    select_subtitle_size(SubtitleSize::Large, Some(tx));
    assert!(recv(&rx));
    let (tx, rx) = mpsc::channel();
    select_subtitle_position(SubtitlePosition::High, Some(tx));
    assert!(recv(&rx));

    // The disk now refuses every write; none is attempted, so none fails.
    let fixture = Unwritable::new(&session);
    assert!(!writes(|| {
        assert!(set_direct_play_mode(DirectPlayMode::Forced), "direct play");
        assert!(set_next_episode_mode(NextEpisodeMode::AfterCredits), "next episode");
        assert!(set_skip_interval(SkipInterval::Seconds30), "skip interval");
        assert!(set_deck_press(DeckPress::Play), "deck press");
        assert!(set_default_quality(Quality::P720), "quality");
        let (tx, rx) = mpsc::channel();
        select_subtitle_size(SubtitleSize::Large, Some(tx));
        assert!(recv(&rx), "size reports saved");
        let (tx, rx) = mpsc::channel();
        select_subtitle_position(SubtitlePosition::High, Some(tx));
        assert!(recv(&rx), "position reports saved");
    }), "an unchanged pick writes nothing");

    // ...while a CHANGED pick still reaches the refusing disk and still claims nothing.
    assert!(!set_skip_interval(SkipInterval::Seconds60));
    assert_eq!(skip_interval(), SkipInterval::Seconds30);
    drop(fixture);
    assert_eq!(read::<SkipIntervalPref>(), SkipInterval::Seconds30);
}

/// **Delta 4.** A failed tone or enhancement write now logs the door's line and keeps the live
/// value; before, `update` swallowed the refusal and nothing was said.
#[test]
fn tone_write_failure_logs_and_keeps_the_live_value() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("door-tone-failure");
    sign_in(&session);
    let _restore = Restore::take();
    defaults();
    let boost = AudioEnhancements { boost_dialog: true, normalize_loudness: false };
    let fixture = Unwritable::new(&session);
    let mark = LogMark::now();

    crate::player::set_subtitle_tone(SubtitleTone::Grey);
    crate::player::set_audio_enhancements(boost);
    plx_base::storage_worker::drain_for_test();

    assert_eq!(crate::player::subtitle_tone(), SubtitleTone::Grey, "the pick stays live");
    assert_eq!(crate::player::audio_enhancements(), boost);
    let log = mark.since();
    assert!(log.contains(&format!("subtitle tone: {WRITE_FAILED}")), "{log}");
    assert!(log.contains(&format!("audio enhancements: {WRITE_FAILED}")), "{log}");
    drop(fixture);
    assert_eq!(picked::<SubtitleTonePref>(), None, "and nothing landed");
}

/// Hold the storage worker on a job, so an edit queued behind it runs only after the test has
/// changed the world.
pub(super) struct Parked(mpsc::Sender<()>);
impl Parked {
    pub(super) fn new() -> Parked {
        let (release, wait) = mpsc::channel::<()>();
        let (started, running) = mpsc::channel::<()>();
        drop(plx_base::storage_worker::submit_retained(move || {
            let _ = started.send(());
            let _ = wait.recv();
        }));
        running.recv().expect("the worker picked the parking job up");
        Parked(release)
    }
    pub(super) fn release(self) {
        let _ = self.0.send(());
    }
}

/// **P-d3, reversing delta 2.** The player's quality pick used to ride the install door, so a
/// sign-out followed by a completed sign-in while the pick waited on the worker did not lose it. A
/// quality pick is a PROFILE's value now, and a profile's values are account data: the write is
/// dropped when the tenure moved or the credentials changed between pick and run, so it can never
/// land in the next sign-in's record. The pick stays live for this session, as every failed write's
/// does.
#[test]
fn profile_write_queued_across_sign_out_is_dropped() {
    let _g = plx_base::testlock::serial();
    let temp = session::TempSession::new("door-quality-tenure");
    let _restore = Restore::take();
    defaults();
    let signed_in = session::Session { client_id: "cid-1".into(), account_token: TOKEN.into(), ..Default::default() };
    session::save(&signed_in);
    assert_eq!(session::peek().client_id, "cid-1");
    sign_in(&temp);

    let parked = Parked::new();
    set_quality_for_retry(Quality::P720);
    session::revoke_cached_session();
    session::save(&signed_in);
    assert!(!session::cache_revoked(), "the proven sign-in ended the revocation");
    parked.release();
    plx_base::storage_worker::drain_for_test();

    assert_eq!(picked::<QualityPref>(), None, "the pick was dropped: it belonged to the sign-in that ended");
    assert_eq!(stored().prefs.playback_quality, None, "and no top-level key took it either");
    assert_eq!(quality(), Quality::P720, "the pick stays live for this session");
}
