//! The tie test of the live list (T-live), `restore_all`, and the door's clamp-before-persist.
//!
//! T-live is the media half of plex's tie tests (`plex::session::prefs_tests`): plex declares which
//! settings are LIVE, this module declares which it applies, and a setting present in one list and
//! forgotten in the other fails here, both ways, by marker KEY.
use super::*;
use plx_plex::plex::session::{self as session, prefs::DESCRIPTORS};
use std::collections::{BTreeMap, BTreeSet};

fn declared_by_plex() -> BTreeMap<&'static str, bool> {
    DESCRIPTORS.iter().map(|d| (d.key, d.live)).collect()
}

fn declared_by_media() -> BTreeMap<&'static str, bool> {
    LIVE_ROWS.iter().map(|r| (r.key, r.live)).collect()
}

/// **T-live.** `{descriptor.key : descriptor.live}` for the LIVE descriptors equals the rows media
/// declares, in both directions. A setting declared live in plex and forgotten here (the tone and
/// enhancement shape: a panel-only write nobody restores) fails the first assertion; a row here that
/// plex does not declare live fails the second.
#[test]
fn t_live_media_rows_equal_the_settings_plex_declares_live() {
    let plex_live: Vec<_> = declared_by_plex().into_iter().filter(|(_, live)| *live).map(|(k, _)| k).collect();
    let media_rows: Vec<_> = declared_by_media().into_keys().collect();
    for key in &plex_live {
        assert!(media_rows.contains(key), "plex declares `{key}` LIVE but media has no LIVE_ROWS entry for it");
    }
    for key in &media_rows {
        assert!(plex_live.contains(key), "media applies `{key}` live but plex does not declare it LIVE");
    }
    assert_eq!(media_rows.len(), LIVE_ROWS.len(), "no key is declared twice");
    assert!(LIVE_ROWS.iter().all(|r| r.live), "every row is a LIVE setting");
}

/// The tie test must be able to fail: against a descriptor table whose `live` flag is dropped for
/// one key, the same comparison reports it.
#[test]
fn t_live_catches_a_row_forgotten_in_either_list() {
    let mut plex = declared_by_plex();
    let media = declared_by_media();
    let agrees = |plex: &BTreeMap<&str, bool>| {
        let live: Vec<_> = plex.iter().filter(|(_, l)| **l).map(|(k, _)| *k).collect();
        let rows: Vec<_> = media.keys().copied().collect();
        live == rows
    };
    assert!(agrees(&plex));
    plex.insert("subtitle_tone", false); // plex forgets it
    assert!(!agrees(&plex), "a row media applies and plex does not declare live is caught");
    plex.insert("subtitle_tone", true);
    plex.insert("a_new_live_setting", true); // plex adds one media never learns of
    assert!(!agrees(&plex), "a setting declared live in plex but missing in media is caught");
}

/// A record with the nine settings away from every default.
fn stored_prefs() -> Preferences {
    let s = session::Session::default()
        .with_pref::<QualityPref>(Quality::P1080High)
        .with_pref::<DirectPlayModePref>(DirectPlayMode::Forced)
        .with_pref::<SubtitleSizePref>(SubtitleSize::Large)
        .with_pref::<SubtitlePositionPref>(SubtitlePosition::High)
        .with_pref::<NextEpisodeModePref>(NextEpisodeMode::AfterCredits)
        .with_pref::<SkipIntervalPref>(SkipInterval::Seconds30)
        .with_pref::<DeckPressPref>(DeckPress::Play)
        .with_pref::<SubtitleTonePref>(SubtitleTone::Silver)
        .with_pref::<AudioEnhancementsPref>(AudioEnhancements { boost_dialog: true, normalize_loudness: true });
    s.prefs
}

/// A session record holding `prefs` and nothing else, for the restore.
fn record(prefs: Preferences) -> session::Session {
    session::Session { prefs, ..Default::default() }
}

#[derive(Debug, PartialEq)]
struct Cells(Quality, DirectPlayMode, SubtitleSize, SubtitlePosition, NextEpisodeMode, SkipInterval, DeckPress,
    SubtitleTone, AudioEnhancements);

fn cells() -> Cells {
    Cells(quality(), direct_play_mode(), subtitle_size(), subtitle_position(), next_episode_mode(), skip_interval(),
        deck_press(), crate::player::subtitle_tone(), crate::player::audio_enhancements())
}

/// Put the nine cells back to what they held, on drop. Declared AFTER `serial()` so it drops first,
/// while the lock `restore_*` asserts is still held.
struct Put(Option<Cells>);
impl Put {
    fn back(c: Cells) -> Put {
        Put(Some(c))
    }
}
impl Drop for Put {
    fn drop(&mut self) {
        let Some(Cells(q, dpm, size, position, next, skip, deck, tone, enh)) = self.0.take() else { return };
        restore_quality(q);
        restore_direct_play_mode(dpm);
        restore_subtitle_size(size);
        restore_subtitle_position(position);
        restore_next_episode_mode(next);
        restore_skip_interval(skip);
        restore_deck_press(deck);
        crate::player::restore_subtitle_tone(tone);
        crate::player::restore_audio_enhancements(enh);
    }
}

fn scramble() {
    restore_quality(Quality::P480);
    restore_direct_play_mode(DirectPlayMode::Disabled);
    restore_subtitle_size(SubtitleSize::ExtraLarge);
    restore_subtitle_position(SubtitlePosition::Middle);
    restore_next_episode_mode(NextEpisodeMode::Off);
    restore_skip_interval(SkipInterval::Seconds60);
    restore_deck_press(DeckPress::Play);
    crate::player::restore_subtitle_tone(SubtitleTone::Grey);
    crate::player::restore_audio_enhancements(AudioEnhancements { boost_dialog: false, normalize_loudness: true });
}

/// **`restore_all` with fixed overrides is the hand list it replaced.** Every cell lands on the
/// record's value; the quality override replaces only the quality; an absent quality reads as
/// Original while the record keeps it absent (restore writes nothing).
#[test]
fn restore_all_sets_every_cell_from_the_record_and_honours_only_the_quality_override() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("restore-all");
    let _restore = Put::back(cells());

    scramble();
    restore_all(&record(stored_prefs()), Overrides::default());
    assert_eq!(
        cells(),
        Cells(Quality::P1080High, DirectPlayMode::Forced, SubtitleSize::Large, SubtitlePosition::High,
            NextEpisodeMode::AfterCredits, SkipInterval::Seconds30, DeckPress::Play, SubtitleTone::Silver,
            AudioEnhancements { boost_dialog: true, normalize_loudness: true })
    );

    scramble();
    restore_all(&record(stored_prefs()), Overrides { quality: Some(Quality::P720) });
    assert_eq!(quality(), Quality::P720, "the override replaces the record's quality");
    assert_eq!(direct_play_mode(), DirectPlayMode::Forced, "and nothing else");
    assert_eq!(subtitle_size(), SubtitleSize::Large);

    scramble();
    restore_all(&record(Preferences::default()), Overrides::default());
    assert_eq!(
        cells(),
        Cells(Quality::Original, DirectPlayMode::Auto, SubtitleSize::Medium, SubtitlePosition::Low,
            NextEpisodeMode::Countdown, SkipInterval::Seconds10, DeckPress::Details, SubtitleTone::White,
            AudioEnhancements::NONE),
        "a record that chose nothing restores every default (an absent quality is Original)"
    );
}

/// **`restore_all` publishes each cell once** (a restore is not a pick, and the publish counter is
/// what the one-publish tests count).
#[test]
fn restore_all_publishes_each_live_cell_once() {
    let _g = plx_base::testlock::serial();
    let _restore = Put::back(cells());
    let before = (publishes_for_test::<QualityPref>(), publishes_for_test::<SubtitleTonePref>());
    restore_all(&record(stored_prefs()), Overrides::default());
    assert_eq!(publishes_for_test::<QualityPref>(), before.0 + 1);
    assert_eq!(publishes_for_test::<SubtitleTonePref>(), before.1 + 1);
}

/// What `supported_quality` would do in a build whose adaptive path is not ready: Auto is not
/// offered, so it reads as Original. (`auto_quality_ready` is a `const true` today, so the real
/// clamp is the identity; the stub stands in for the build that closes the gate.)
fn gate_closed(q: Quality) -> Quality {
    if q == Quality::Auto { Quality::Original } else { q }
}

/// **The clamp runs BEFORE persist.** A pick the build does not offer is stored as the rung it is
/// clamped to, and the cell holds that same rung: what is stored is what is live. Graded through
/// both halves of the door: the Confirm persist and the Optimistic publish-then-persist.
#[test]
fn normalize_runs_before_persist_so_the_stored_value_is_the_clamped_one() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("normalize-before-persist");
    let _restore = Put::back(cells());
    session::save(&session::Session { client_id: "cid".into(), account_token: "acct".into(), ..Default::default() });
    restore_quality(Quality::P720);

    assert!(confirm_via::<QualityPref>(Quality::Auto, gate_closed));
    assert_eq!(session::load().prefs.playback_quality, Some(Quality::Original), "stored: the clamped rung");
    assert_eq!(quality(), Quality::Original, "live: the same rung");

    restore_quality(Quality::P720);
    let (tx, rx) = std::sync::mpsc::channel();
    choose_via::<QualityPref>(Quality::Auto, How::Optimistic(Some(tx)), |q| if q == Quality::Auto { Quality::P480 } else { q });
    assert_eq!(quality(), Quality::P480, "published the clamped rung");
    assert!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
    assert_eq!(session::load().prefs.playback_quality, Some(Quality::P480), "and persisted the same one");
}

/// The Settings policy is declared once, on the row: the dispatcher asks the row.
#[test]
fn how_settings_follows_the_rows_declared_policy() {
    let (tx, _rx) = std::sync::mpsc::channel();
    assert!(matches!(How::settings::<DeckPressPref>(tx.clone()), How::Confirm(_)));
    assert!(matches!(How::settings::<QualityPref>(tx.clone()), How::Confirm(_)));
    assert!(matches!(How::settings::<SubtitleSizePref>(tx.clone()), How::Optimistic(Some(_))));
    assert!(matches!(How::settings::<SubtitlePositionPref>(tx), How::Optimistic(Some(_))));
}

/// The keys of the descriptors that have a Settings row, as plex declares them.
fn settings_rows() -> BTreeSet<&'static str> {
    DESCRIPTORS.iter().filter(|d| d.settings.is_some()).map(|d| d.key).collect()
}

/// **T-settings.** `{descriptor.key : descriptor.settings.is_some()}` equals [`Choice::KEYS`], both
/// ways. A Settings row with no `Choice` variant, or a `Choice` variant whose setting has no row
/// (its `SETTINGS` fact dropped), fails here.
#[test]
fn t_settings_rows_equal_the_choices_both_ways() {
    let choices: BTreeSet<&str> = Choice::KEYS.iter().copied().collect();
    assert_eq!(choices.len(), Choice::KEYS.len(), "no setting has two choices");
    let rows = settings_rows();
    for key in &rows {
        assert!(choices.contains(key), "plex gives `{key}` a Settings row but no Choice saves it");
    }
    for key in &choices {
        assert!(rows.contains(key), "a Choice saves `{key}` but plex gives it no Settings row");
    }
    assert_eq!(rows, choices);
}

/// The tie test must be able to fail: a row that loses its key from the choices, and a choice whose
/// key the descriptors no longer give a row, are both reported by the same comparison.
#[test]
fn t_settings_catches_a_row_or_choice_dropped() {
    let choices: BTreeSet<&str> = Choice::KEYS.iter().copied().collect();
    let agrees = |rows: &BTreeSet<&str>| *rows == choices;
    let mut rows = settings_rows();
    assert!(agrees(&rows));
    rows.remove("playback_quality"); // the Quality row's SETTINGS fact dropped
    assert!(!agrees(&rows), "a Choice whose setting has no Settings row is caught");
    rows.insert("playback_quality");
    rows.insert("subtitle_tone"); // a panel-only setting gains a row with no Choice
    assert!(!agrees(&rows), "a Settings row with no Choice is caught");
}
