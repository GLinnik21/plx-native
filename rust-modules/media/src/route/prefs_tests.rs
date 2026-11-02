//! The tie test of the live list (T-live), the seat's restore, and the door's clamp-before-persist.
//!
//! T-live is the media half of plex's tie tests (`plex::session::prefs_tests`): plex declares which
//! settings are LIVE, this module declares which it applies, and a setting present in one list and
//! forgotten in the other fails here, both ways, by marker KEY. Every live row is also a `Profile`
//! row (the partition by `SCOPE`): a live setting that stayed `Install` would be restored from
//! nobody's seat.
use super::*;
use plx_plex::plex::session::{self as session, prefs::DESCRIPTORS, stored_effective, UnderPostersDetails};
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

/// **T-live, the partition by `SCOPE`.** Every setting plex declares live is a `Profile` row, and the
/// rows media applies carry that scope: the seat restores them all, and a live `Install` row would
/// be restored from nobody's seat. The descriptors that are NOT live are the ones that stay out.
#[test]
fn t_live_every_live_row_is_a_profile_row() {
    use plx_plex::plex::session::prefs::Scope;
    for d in DESCRIPTORS.iter().filter(|d| d.live) {
        assert_eq!(d.scope, Scope::Profile, "`{}` is live, so the seat restores it: it must be a Profile row", d.key);
    }
    assert!(LIVE_ROWS.iter().all(|r| r.scope == Scope::Profile));
    let profile_live: BTreeSet<_> =
        DESCRIPTORS.iter().filter(|d| d.scope == Scope::Profile && d.live).map(|d| d.key).collect();
    let media: BTreeSet<_> = LIVE_ROWS.iter().map(|r| r.key).collect();
    assert_eq!(profile_live, media, "the live Profile rows are exactly the rows the seat restores");
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

/// The legacy install-wide keys of a record with the eleven settings away from every default.
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
        .with_pref::<HdrOverlayBrightnessPref>(HdrOverlayBrightness::Half)
        .with_pref::<AudioEnhancementsPref>(AudioEnhancements { boost_dialog: true, normalize_loudness: true })
        .with_pref::<TrailerAutoplayPref>(false);
    s.prefs
}

/// The account the fixtures sign in with, and its stamp: a record is visible only under it.
const TOKEN: &str = "acct-token";

fn stamp() -> String {
    plx_plex::plex::grant::account_key(TOKEN)
}

fn seat(user: &str) -> Seat {
    Seat::Seated { user: user.into(), stamp: stamp() }
}

/// What `user` picked, as the file would hold it (parsed from JSON, so the soft parse is in play).
fn picked(user: &str, under: &str, prefs: serde_json::Value) -> ProfilePrefsRecord {
    serde_json::from_value(serde_json::json!({"user": user, "account": under, "prefs": prefs})).unwrap()
}

/// A session record holding `legacy` as its top-level keys and `own` as the profiles' picks.
fn record(legacy: Preferences, own: Vec<ProfilePrefsRecord>) -> session::Session {
    session::Session {
        prefs: Preferences { profile_prefs: own, ..legacy },
        account_token: TOKEN.into(),
        ..Default::default()
    }
}

/// [`restore_profile_rows`] for a process that has restored nothing yet: the mirror is seeded from
/// `record` itself. (A restore that finds a mirror already seeded keeps it: see
/// `restore_profile_rows_reads_the_mirror_because_the_file_is_write_after`.)
fn restore(record: &session::Session, seat: &Seat, ov: Overrides) {
    unseed_profile_prefs_for_test();
    restore_profile_rows(record, seat, ov);
}

#[derive(Debug, PartialEq)]
struct Cells(Quality, DirectPlayMode, SubtitleSize, SubtitlePosition, NextEpisodeMode, SkipInterval, DeckPress,
    SubtitleTone, AudioEnhancements, HdrOverlayBrightness, bool);

fn cells() -> Cells {
    Cells(quality(), direct_play_mode(), subtitle_size(), subtitle_position(), next_episode_mode(), skip_interval(),
        deck_press(), crate::player::subtitle_tone(), crate::player::audio_enhancements(),
        hdr_overlay_brightness(), crate::player::preview::TRAILER_AUTOPLAY.get())
}

/// The eleven legacy values of [`stored_prefs`], as the cells hold them once they are restored.
fn legacy_cells() -> Cells {
    Cells(Quality::P1080High, DirectPlayMode::Forced, SubtitleSize::Large, SubtitlePosition::High,
        NextEpisodeMode::AfterCredits, SkipInterval::Seconds30, DeckPress::Play, SubtitleTone::Silver,
        AudioEnhancements { boost_dialog: true, normalize_loudness: true }, HdrOverlayBrightness::Half, false)
}

/// Put the eleven cells back to what they held, and the mirrors to never-seeded, on drop. Declared
/// AFTER `serial()` so it drops first, while the lock `restore_*` asserts is still held.
struct Put(Option<Cells>);
impl Put {
    fn back(c: Cells) -> Put {
        Put(Some(c))
    }
}
impl Drop for Put {
    fn drop(&mut self) {
        unseed_profile_prefs_for_test();
        let Some(Cells(q, dpm, size, position, next, skip, deck, tone, enh, hdr, trailer)) = self.0.take() else { return };
        restore_quality(q);
        restore_direct_play_mode(dpm);
        restore_subtitle_size(size);
        restore_subtitle_position(position);
        restore_next_episode_mode(next);
        restore_skip_interval(skip);
        restore_deck_press(deck);
        crate::player::restore_subtitle_tone(tone);
        crate::player::restore_audio_enhancements(enh);
        restore_hdr_overlay_brightness(hdr);
        crate::player::preview::restore_trailer_autoplay(trailer);
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
    restore_hdr_overlay_brightness(HdrOverlayBrightness::Full);
    crate::player::preview::restore_trailer_autoplay(true);
}

/// **Unseated restores the legacy keys, and never an own value.** Nobody is seated, so nobody owns
/// anything: every cell lands on the record's top-level key even when some profile has a pick of
/// its own; the quality override replaces only the quality; an absent quality reads as Original while
/// the record keeps it absent (restore writes nothing).
#[test]
fn restore_profile_rows_unseated_reads_the_legacy_keys_and_never_an_own_value() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("restore-unseated");
    let _restore = Put::back(cells());
    let own = vec![picked("u1", &stamp(), serde_json::json!({"subtitle_size": "extra_large", "skip_interval": "60s"}))];

    scramble();
    restore(&record(stored_prefs(), own.clone()), &Seat::Unseated, Overrides::default());
    assert_eq!(cells(), legacy_cells());

    scramble();
    restore(&record(stored_prefs(), own), &Seat::Unseated, Overrides { quality: Some(Quality::P720) });
    assert_eq!(quality(), Quality::P720, "the override replaces the record's quality");
    assert_eq!(direct_play_mode(), DirectPlayMode::Forced, "and nothing else");
    assert_eq!(subtitle_size(), SubtitleSize::Large);

    scramble();
    restore(&record(Preferences::default(), vec![]), &Seat::Unseated, Overrides::default());
    assert_eq!(
        cells(),
        Cells(Quality::Original, DirectPlayMode::Auto, SubtitleSize::Medium, SubtitlePosition::Low,
            NextEpisodeMode::Countdown, SkipInterval::Seconds10, DeckPress::Details, SubtitleTone::White,
            AudioEnhancements::NONE, HdrOverlayBrightness::ThreeQuarters, false),
        "a record that chose nothing restores every default (an absent quality is Original; the derived \
         `Preferences::default()` has trailers off: a PARSED record reads them on, see below)"
    );
}

/// **A seated profile reads its own value, else the legacy key, per setting.** u1 picked three of the
/// ten (subtitle size, skip interval and the trailer switch); the other seven read the TV's. Another
/// profile with no record reads the TV's for all ten,
/// the owner (`""`) has a record of its own, and a record under another sign-in's stamp is not read.
#[test]
fn restore_profile_rows_seated_reads_the_own_value_and_falls_back_per_setting() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("restore-seated");
    let _restore = Put::back(cells());
    let own = vec![
        picked("u1", &stamp(), serde_json::json!({"subtitle_size": "extra_large", "skip_interval": "60s",
            "trailer_autoplay": true})),
        picked("", &stamp(), serde_json::json!({"playback_quality": "480p_720_kbps"})),
        picked("u3", "ffffffffffffffff", serde_json::json!({"subtitle_size": "small"})),
    ];
    let rec = record(stored_prefs(), own);

    scramble();
    restore(&rec, &seat("u1"), Overrides::default());
    let Cells(q, dpm, _, position, next, _, deck, tone, enh, hdr, _) = legacy_cells();
    assert_eq!(
        cells(),
        Cells(q, dpm, SubtitleSize::ExtraLarge, position, next, SkipInterval::Seconds60, deck, tone, enh, hdr, true),
        "u1: its three picks (trailers ON over the TV's off) over the TV's seven"
    );

    scramble();
    restore(&rec, &seat("u2"), Overrides::default());
    assert_eq!(cells(), legacy_cells(), "u2 picked nothing: all ten are the TV's");

    scramble();
    restore(&rec, &seat(""), Overrides::default());
    assert_eq!(quality(), Quality::P480, "the owner's own record");
    assert_eq!(subtitle_size(), SubtitleSize::Large, "and the TV's for what it did not pick");

    scramble();
    restore(&rec, &seat("u3"), Overrides::default());
    assert_eq!(subtitle_size(), SubtitleSize::Large, "a record of another sign-in is invisible");

    scramble();
    restore(&rec, &seat("u1"), Overrides { quality: Some(Quality::P720) });
    assert_eq!(quality(), Quality::P720, "the override is forced over the profile's value too");
    assert_eq!(subtitle_size(), SubtitleSize::ExtraLarge);
}

/// **Hero trailer autoplay (P1c) follows the seat through the same restore.** The TV's key says off
/// (the trailer switch a household turned off before profiles had their own): a profile that never
/// picked reads off, one that picked on reads on, nobody seated reads the TV's. On a PARSED record
/// that never had the key, everybody reads on (the product default); the cell is what
/// `preview::enabled` reads, so these are the per-profile answers the detail hero gets.
#[test]
fn restore_profile_rows_trailer_autoplay_follows_the_seat_and_falls_back_to_the_tv() {
    use crate::player::preview::enabled;
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("restore-trailer");
    let _restore = Put::back(cells());
    let tv_off: session::Session = serde_json::from_value(serde_json::json!({
        "account_token": TOKEN, "trailer_autoplay": false,
        "profile_prefs": [{"user": "on", "account": stamp(), "prefs": {"trailer_autoplay": true}},
                          {"user": "junk", "account": stamp(), "prefs": {"trailer_autoplay": "maybe"}}]
    })).unwrap();

    restore(&tv_off, &Seat::Unseated, Overrides::default());
    assert!(!enabled(), "nobody seated: the TV's off");
    restore(&tv_off, &seat("on"), Overrides::default());
    assert!(enabled(), "its own pick, on, over the TV's off");
    restore(&tv_off, &seat("other"), Overrides::default());
    assert!(!enabled(), "a profile that never picked: the TV's off");
    restore(&tv_off, &seat("junk"), Overrides::default());
    assert!(!enabled(), "an unreadable own value reads the TV's, not ON");

    let fresh: session::Session = serde_json::from_value(serde_json::json!({"account_token": TOKEN})).unwrap();
    restore_profile_rows(&fresh, &Seat::Unseated, Overrides::default());
    assert!(enabled(), "a record that never had the key: on");
    restore_profile_rows(&fresh, &seat("anyone"), Overrides::default());
    assert!(enabled());
}

/// **The trailer pick is the seated profile's own, and the TV's key is untouched.** An off pick as `a`
/// publishes off for `a` only: after the switch `b` reads the TV's on, and `a`, seated again, finds its
/// off. Through the Confirm door the Settings toggle uses.
#[test]
fn trailer_pick_is_the_seated_profiles_own() {
    use crate::player::preview::enabled;
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("trailer-pick");
    let _restore = Put::back(cells());
    let mut base: session::Session = serde_json::from_value(serde_json::json!({"account_token": TOKEN})).unwrap();
    base.client_id = "cid-test".into();
    session.seated("a", TOKEN);
    session::save(&base);
    seed_profile_prefs_for_test(&session::peek());
    restore_profile_rows(&session::peek(), &seat("a"), Overrides::default());
    assert!(enabled(), "setup: on for a");

    assert!(confirm::<TrailerAutoplayPref>(false));
    assert!(!enabled(), "published for a");
    let top_level = |s: &session::Session| {
        let mut v = serde_json::to_value(&s.prefs).unwrap();
        v.as_object_mut().unwrap().remove("profile_prefs");
        v
    };
    assert_eq!(top_level(&session::load()), top_level(&base), "the TV's key was not written");
    let stored = session::load();
    assert_eq!(own_of_trailer(&stored.prefs.profile_prefs, "a"), Some(false));

    session.seated("b", TOKEN);
    restore_profile_rows(&session::peek(), &seat("b"), Overrides::default());
    assert!(enabled(), "b picked nothing: the TV's on");
    assert_eq!(own_of_trailer(&profile_prefs_mirror_for_test(), "b"), None);
    session.seated("a", TOKEN);
    restore_profile_rows(&session::peek(), &seat("a"), Overrides::default());
    assert!(!enabled(), "a finds its off (the mirror, not a stale file)");
}

fn own_of_trailer(records: &[ProfilePrefsRecord], user: &str) -> Option<bool> {
    own_of::<TrailerAutoplayPref>(records, user)
}

/// **A restore publishes each cell once** (a restore is not a pick, and the publish counter is what
/// the one-publish tests count).
#[test]
fn restore_profile_rows_publishes_each_live_cell_once() {
    let _g = plx_base::testlock::serial();
    let _restore = Put::back(cells());
    let before = (publishes_for_test::<QualityPref>(), publishes_for_test::<SubtitleTonePref>(),
        publishes_for_test::<SubtitleSizePref>());
    restore(&record(stored_prefs(), vec![]), &seat("u1"), Overrides::default());
    assert_eq!(publishes_for_test::<QualityPref>(), before.0 + 1);
    assert_eq!(publishes_for_test::<SubtitleTonePref>(), before.1 + 1);
    assert_eq!(publishes_for_test::<SubtitleSizePref>(), before.2 + 1);
}

/// **A restore reads the mirror, not the record it is handed.** `session::peek()` is write-after: a
/// pick is in the mirror the moment it is made and in the file when the worker lands it. A restore
/// that read the record would show a switch back to the profile its pre-pick value. The record here
/// is the stale one a restore would be handed while the write is still queued; the mirror, already
/// seeded under the same tenure and stamp, is kept.
#[test]
fn restore_profile_rows_reads_the_mirror_because_the_file_is_write_after() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("restore-mirror");
    let _restore = Put::back(cells());
    let stale = record(Preferences::default(), vec![]);
    assert_eq!(session.seated("u1", TOKEN), stamp());
    seed_profile_prefs_for_test(&stale);

    assert!(confirm::<SubtitleSizePref>(SubtitleSize::ExtraLarge));
    restore_subtitle_size(SubtitleSize::Small);
    restore_profile_rows(&stale, &seat("u1"), Overrides::default());
    assert_eq!(subtitle_size(), SubtitleSize::ExtraLarge, "the pick, though `stale` does not have it");
    restore_profile_rows(&stale, &seat("u2"), Overrides::default());
    assert_eq!(subtitle_size(), SubtitleSize::Medium, "and it is u1's, not u2's");
}

/// **Every live row records its pick under the seated profile, and never in the TV's key.** The ten
/// settings picked through the Confirm door land in the profile's record (the mirror has the same),
/// the frozen top-level keys are byte for byte what they were, and the cell holds the pick.
#[test]
fn every_live_setting_is_picked_into_the_seated_profiles_record() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("pick-every-row");
    let _restore = Put::back(cells());
    session.seated("u1", TOKEN);
    let mut legacy = record(stored_prefs(), vec![]);
    legacy.client_id = "cid-test".into();
    session::save(&legacy);
    seed_profile_prefs_for_test(&session::peek());
    let top_level = |s: &session::Session| {
        let mut v = serde_json::to_value(&s.prefs).unwrap();
        v.as_object_mut().unwrap().remove("profile_prefs");
        v
    };
    let before = top_level(&session::load());

    assert!(confirm::<QualityPref>(Quality::P480));
    assert!(confirm::<DirectPlayModePref>(DirectPlayMode::Disabled));
    assert!(confirm::<SubtitleSizePref>(SubtitleSize::ExtraLarge));
    assert!(confirm::<SubtitlePositionPref>(SubtitlePosition::Middle));
    assert!(confirm::<NextEpisodeModePref>(NextEpisodeMode::Off));
    assert!(confirm::<SkipIntervalPref>(SkipInterval::Seconds60));
    assert!(confirm::<DeckPressPref>(DeckPress::Details));
    assert!(confirm::<SubtitleTonePref>(SubtitleTone::Grey));
    assert!(confirm::<AudioEnhancementsPref>(AudioEnhancements::NONE));
    assert!(confirm::<TrailerAutoplayPref>(true), "over the TV's off: an own value, on");

    let after = session::load();
    assert_eq!(top_level(&after), before, "the TV's keys are untouched: no pick writes a legacy key");
    let own = after.prefs.profile_prefs.iter().find(|r| r.user == "u1").expect("u1's record");
    assert_eq!(own.account, stamp());
    assert_eq!(after.prefs.profile_prefs.len(), 1);
    assert_eq!(profile_prefs_mirror_for_test(), after.prefs.profile_prefs, "the mirror holds exactly what landed");
    assert_eq!(
        cells(),
        Cells(Quality::P480, DirectPlayMode::Disabled, SubtitleSize::ExtraLarge, SubtitlePosition::Middle,
            NextEpisodeMode::Off, SkipInterval::Seconds60, DeckPress::Details, SubtitleTone::Grey,
            AudioEnhancements::NONE, HdrOverlayBrightness::ThreeQuarters, true)
    );
}

/// **P-d10: a pick while nobody is seated is refused.** Confirm replies false and publishes nothing,
/// Optimistic shows the value for this seat only and replies false; neither writes the file.
#[test]
fn profile_write_while_unseated_is_refused() {
    let _g = plx_base::testlock::serial();
    let _session = session::TempSession::new("pick-unseated");
    let _restore = Put::back(cells());
    restore_subtitle_size(SubtitleSize::Medium);
    restore_deck_press(DeckPress::Details);
    assert_eq!(session::current_seat(), Seat::Unseated, "setup");
    let before = serde_json::to_value(session::load()).unwrap();

    assert!(!confirm::<DeckPressPref>(DeckPress::Play), "Confirm replies false");
    assert_eq!(deck_press(), DeckPress::Details, "and publishes nothing");
    let (tx, rx) = std::sync::mpsc::channel();
    choose::<DeckPressPref>(DeckPress::Play, How::Confirm(tx));
    assert!(!rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), "so does the queued door");
    assert_eq!(deck_press(), DeckPress::Details);

    let (tx, rx) = std::sync::mpsc::channel();
    choose::<SubtitleSizePref>(SubtitleSize::Large, How::Optimistic(Some(tx)));
    assert_eq!(subtitle_size(), SubtitleSize::Large, "an optimistic pick still shows, for this seat only");
    assert!(!rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(), "but it was not saved");

    plx_base::storage_worker::drain_for_test();
    assert_eq!(serde_json::to_value(session::load()).unwrap(), before, "and the file is untouched");
}

/// What `user` has picked for `S`, according to `records` under the fixture account's stamp.
fn own_of<S: session::ProfileSetting>(records: &[ProfilePrefsRecord], user: &str) -> Option<S::Value> {
    session::own_prefs(records, user, &stamp()).and_then(S::own)
}

/// Profile `u1` seated and reading the legacy values, its Confirm pick captured, and then the switch
/// to `u2` (seated and restored) before the pick's write runs: the order a queued Confirm can meet.
fn confirm_pick_then_switch(session: &session::TempSession) -> ProfileWrite {
    let mut legacy = record(stored_prefs(), vec![]);
    legacy.client_id = "cid-test".into();
    session.seated("u1", TOKEN);
    session::save(&legacy);
    seed_profile_prefs_for_test(&session::peek());
    restore_profile_rows(&session::peek(), &seat("u1"), Overrides::default());
    assert_eq!(deck_press(), DeckPress::Play, "setup: u1 reads the legacy value");
    let write = ProfileWrite::capture().expect("u1 is seated");
    session.seated("u2", TOKEN);
    restore_profile_rows(&session::peek(), &seat("u2"), Overrides::default());
    assert_eq!(deck_press(), DeckPress::Play, "setup: u2 reads the legacy value too");
    write
}

/// **P-d5: a Confirm reply after a switch does not publish into the new profile's cells.** u1's pick
/// is captured on the main thread and runs on the worker after the switch seated u2. The write is
/// durable (the reply is true) and lands in u1's record, but the cell u2 is reading is not touched:
/// the cell is published only while the profile that picked is still the seated one.
#[test]
fn confirm_reply_after_switch_publishes_nothing() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("confirm-after-switch");
    let _restore = Put::back(cells());
    let write = confirm_pick_then_switch(&session);
    let before = publishes_for_test::<DeckPressPref>();

    assert!(confirm_with::<DeckPressPref>(&write, DeckPress::Details), "the write was durable");
    assert_eq!(deck_press(), DeckPress::Play, "and u2's cell is not u1's pick");
    assert_eq!(publishes_for_test::<DeckPressPref>(), before, "nothing was published");
    let records = session::load().prefs.profile_prefs;
    assert_eq!(own_of::<DeckPressPref>(&records, "u1"), Some(DeckPress::Details), "it landed in u1's record");
    assert_eq!(own_of::<DeckPressPref>(&records, "u2"), None, "and in nobody else's");
}

/// **N3: the mirror is updated for the profile that picked, whoever is seated when the write returns.**
/// The same late Confirm as above: the cell is left alone, but the mirror holds u1's pick, so a switch
/// back to u1 shows it (a restore reads the mirror, never the possibly older file).
#[test]
fn confirm_after_switch_updates_the_mirror_of_the_picking_profile() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("confirm-after-switch-mirror");
    let _restore = Put::back(cells());
    let write = confirm_pick_then_switch(&session);

    assert!(confirm_with::<DeckPressPref>(&write, DeckPress::Details));
    let mirror = profile_prefs_mirror_for_test();
    assert_eq!(own_of::<DeckPressPref>(&mirror, "u1"), Some(DeckPress::Details), "the mirror has u1's pick");
    assert_eq!(own_of::<DeckPressPref>(&mirror, "u2"), None, "and u2 has none");

    session.seated("u1", TOKEN);
    let stale = record(stored_prefs(), vec![]);
    restore_profile_rows(&stale, &seat("u1"), Overrides::default());
    assert_eq!(deck_press(), DeckPress::Details, "u1, seated again, finds its pick (a stale record cannot hide it)");
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
    session::save(&session::Session { client_id: "cid".into(), account_token: TOKEN.into(), ..Default::default() });
    _session.seated("u1", TOKEN);
    let seat = session::current_seat();
    let stored = || stored_effective::<QualityPref>(&session::load(), &seat);
    restore_quality(Quality::P720);

    assert!(confirm_via::<QualityPref>(Quality::Auto, gate_closed));
    assert_eq!(stored(), Quality::Original, "stored: the clamped rung");
    assert_eq!(quality(), Quality::Original, "live: the same rung");

    restore_quality(Quality::P720);
    let (tx, rx) = std::sync::mpsc::channel();
    choose_via::<QualityPref>(Quality::Auto, How::Optimistic(Some(tx)), |q| if q == Quality::Auto { Quality::P480 } else { q });
    assert_eq!(quality(), Quality::P480, "published the clamped rung");
    assert!(rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap());
    assert_eq!(stored(), Quality::P480, "and persisted the same one");
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

/// The overlay dim rule: nothing changes for SDR or at 100%; HDR at 75% / 50% scales by exactly
/// that; a server re-encode is an SDR picture whatever the source was, a remux keeps it HDR.
#[test]
fn overlay_gain_is_one_for_sdr_and_full_and_the_rung_for_hdr() {
    use HdrOverlayBrightness::{Full, Half, ThreeQuarters};
    for pick in [Full, ThreeQuarters, Half] {
        assert_eq!(overlay_gain_for(false, false, false, pick), 1.0, "SDR direct play, {pick:?}");
        assert_eq!(overlay_gain_for(false, true, true, pick), 1.0, "SDR remux, {pick:?}");
    }
    assert_eq!(overlay_gain_for(true, false, false, Full), 1.0);
    assert_eq!(overlay_gain_for(true, false, false, ThreeQuarters), 0.75);
    assert_eq!(overlay_gain_for(true, false, false, Half), 0.5);
    assert_eq!(overlay_gain_for(true, true, true, Half), 0.5, "a remux keeps the HDR stream");
    assert_eq!(overlay_gain_for(true, true, false, Half), 1.0, "a re-encode is tone-mapped to SDR");
}

#[test]
fn the_default_pick_dims_an_hdr_picture_to_75_percent_and_never_an_sdr_one() {
    assert_eq!(HdrOverlayBrightness::default(), HdrOverlayBrightness::ThreeQuarters);
    assert_eq!(HdrOverlayBrightness::from_index(99), HdrOverlayBrightness::ThreeQuarters);
    assert_eq!(overlay_gain_for(true, false, false, HdrOverlayBrightness::default()), 0.75);
    assert_eq!(overlay_gain_for(false, false, false, HdrOverlayBrightness::default()), 1.0);
    assert_eq!(overlay_gain_for(true, false, false, HdrOverlayBrightness::Full), 1.0, "a chosen 100% stays off");
}

// ---- Under posters (Settings > Libraries) ------------------------------------------------------

/// Puts the Under posters cell back to the default on drop (before the test lock is released).
struct UnderPostersBack;
impl Drop for UnderPostersBack {
    fn drop(&mut self) {
        restore_under_posters(UnderPosters::DEFAULT);
    }
}

/// **The Under posters cell follows the seat, and a Confirm pick lands in the profile's record.**
/// Nothing picked reads the default (no title, year and length); a pick is the seated profile's
/// own and never a legacy key; another profile keeps reading the default; the cell holds the pick.
#[test]
fn under_posters_is_picked_into_the_seated_profiles_record_and_follows_the_seat() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("under-posters");
    let _restore = Put::back(cells());
    let _back = UnderPostersBack;
    session.seated("u1", TOKEN);
    let mut legacy = record(Preferences::default(), vec![]);
    legacy.client_id = "cid-test".into();
    session::save(&legacy);
    seed_profile_prefs_for_test(&session::peek());
    restore_profile_rows(&session::peek(), &seat("u1"), Overrides::default());
    assert_eq!(under_posters(), UnderPosters::DEFAULT, "nothing picked: the default");

    let pick = UnderPosters { title: true, details: UnderPostersDetails::AgeRatingScore, sort_value: false };
    assert!(confirm::<UnderPostersPref>(pick));
    assert_eq!(under_posters(), pick, "the cell holds the pick");
    let after = session::load();
    assert_eq!(after.prefs.under_posters, UnderPosters::DEFAULT, "the TV's key is untouched");
    assert_eq!(own_of::<UnderPostersPref>(&after.prefs.profile_prefs, "u1"), Some(pick));
    assert!(confirm::<UnderPostersPref>(pick), "the same pick again is a no-op that still reports saved");

    session.seated("u2", TOKEN);
    restore_profile_rows(&session::peek(), &seat("u2"), Overrides::default());
    assert_eq!(under_posters(), UnderPosters::DEFAULT, "another profile reads the default");
    session.seated("u1", TOKEN);
    restore_profile_rows(&session::peek(), &seat("u1"), Overrides::default());
    assert_eq!(under_posters(), pick, "and u1, seated again, finds its pick");
    assert!(Choice::KEYS.contains(&"under_posters"), "a Settings row saves it through the door");
    assert!(matches!(How::settings::<UnderPostersPref>(std::sync::mpsc::channel().0), How::Confirm(_)));
}

// ---- Dolby Vision override (Settings > Playback) -----------------------------------------------

struct DolbyVisionBack;
impl Drop for DolbyVisionBack {
    fn drop(&mut self) {
        restore_dolby_vision_mode(DolbyVisionMode::DEFAULT);
    }
}

#[test]
fn dolby_vision_mode_is_picked_into_the_seated_profiles_record_and_follows_the_seat() {
    let _g = plx_base::testlock::serial();
    let session = session::TempSession::new("dolby-vision-mode");
    let _restore = Put::back(cells());
    let _back = DolbyVisionBack;
    session.seated("u1", TOKEN);
    let mut legacy = record(Preferences::default(), vec![]);
    legacy.client_id = "cid-test".into();
    session::save(&legacy);
    seed_profile_prefs_for_test(&session::peek());
    restore_profile_rows(&session::peek(), &seat("u1"), Overrides::default());
    assert_eq!(dolby_vision_mode(), DolbyVisionMode::Auto);

    assert!(confirm::<DolbyVisionModePref>(DolbyVisionMode::Off));
    assert_eq!(dolby_vision_mode(), DolbyVisionMode::Off);
    let after = session::load();
    assert_eq!(after.prefs.dolby_vision_mode, DolbyVisionMode::Auto, "the TV's key is untouched");
    assert_eq!(own_of::<DolbyVisionModePref>(&after.prefs.profile_prefs, "u1"), Some(DolbyVisionMode::Off));

    session.seated("u2", TOKEN);
    restore_profile_rows(&session::peek(), &seat("u2"), Overrides::default());
    assert_eq!(dolby_vision_mode(), DolbyVisionMode::Auto, "another profile reads the default");

    session.seated("u1", TOKEN);
    restore_profile_rows(&session::peek(), &seat("u1"), Overrides::default());
    assert_eq!(dolby_vision_mode(), DolbyVisionMode::Off, "u1 seated again restores Off");
}
