//! Per-profile playback and display settings: the record and its soft parse, the fallback read, the
//! write that always records an own value and prunes foreign stamps, and the seat a publication
//! seats. The live half (cells, the door, the restore) is `media::route::prefs_tests`; the seat
//! trigger is the app's `restore_characterisation_tests`.
use super::super::{
    DolbyVisionMode, InstantDpadSeeking, current_seat, AudioEnhancementsPref, CurrentProfile, DeckPress, DeckPressPref, UnderPosters, UnderPostersDetails, DirectPlayMode, DirectPlayModePref,
    HdrOverlayBrightness, HdrOverlayBrightnessPref, NextEpisodeMode, NextEpisodeModePref, PlaybackQuality, QualityPref, Seat, Session, SkipInterval, SkipIntervalPref,
    SubtitlePosition, SubtitlePositionPref, SubtitleSize, SubtitleSizePref, SubtitleTone, SubtitleTonePref, TempSession,
    TrailerAutoplayPref, UserRef,
};
use super::profile_prefs::*;
use super::{Fallback, Parse, Preferences, Scope, Setting, DESCRIPTORS};
use crate::plex::session::test_support::STAMP;
use crate::plex::AudioEnhancements;
use serde_json::{json, Value};
use std::collections::BTreeSet;

const OTHER_STAMP: &str = "ffeeddccbbaa9988";

fn record(user: &str, stamp: &str, prefs: Value) -> Value {
    json!({ "user": user, "account": stamp, "prefs": prefs })
}

/// A `Preferences` whose `profile_prefs` is parsed from `records`, exactly as the file would be.
fn prefs_with(records: Value) -> Preferences {
    serde_json::from_value(json!({ "profile_prefs": records })).unwrap()
}

// ---- the row and the record -------------------------------------------------------------------

/// The row says what the plan says: its own key, a profile list that sign-out takes with the
/// credentials, not live (the cells are restored from it, it is not itself a cell), not a Settings
/// row (its value is a list of records), and soft-parsed per element.
#[test]
fn the_row_declares_a_profile_scoped_account_bound_soft_list() {
    let d = DESCRIPTORS.iter().find(|d| d.key == "profile_prefs").expect("a descriptor for profile_prefs");
    assert_eq!(d.scope, Scope::Profile);
    assert!(d.account_bound, "a record carries the stamp of the account that picked it");
    assert!(!d.live);
    assert!(d.settings.is_none());
    assert_eq!(d.parse, Parse::SoftVec);
    assert_eq!(d.fallback, Fallback::None);
}

/// **T-fields.** `ProfilePrefs` has a field for exactly the settings whose `FALLBACK` is the legacy
/// install key, under the same name. A Profile row without a field (or a field without its row)
/// fails here.
#[test]
fn the_record_has_one_field_for_each_setting_with_a_legacy_fallback() {
    let full = ProfilePrefs {
        playback_quality: Some(PlaybackQuality::P720),
        direct_play_mode: Some(DirectPlayMode::Forced),
        subtitle_tone: Some(SubtitleTone::Silver),
        subtitle_size: Some(SubtitleSize::Large),
        subtitle_position: Some(SubtitlePosition::High),
        hdr_overlay_brightness: Some(HdrOverlayBrightness::Half),
        next_episode_mode: Some(NextEpisodeMode::AfterCredits),
        skip_interval: Some(SkipInterval::Seconds30),
        instant_dpad_seeking: Some(InstantDpadSeeking::On),
        dolby_vision_mode: Some(DolbyVisionMode::Off),
        deck_press: Some(DeckPress::Play),
        under_posters: Some(UnderPosters { title: true, details: UnderPostersDetails::Video, sort_value: false }),
        audio_enhancements: Some(AudioEnhancements { boost_dialog: true, normalize_loudness: true }),
        trailer_autoplay: Some(false),
    };
    let fields: BTreeSet<String> = serde_json::to_value(&full).unwrap().as_object().unwrap().keys().cloned().collect();
    let rows: BTreeSet<String> = DESCRIPTORS
        .iter()
        .filter(|d| d.fallback == Fallback::LegacyInstallKey)
        .map(|d| d.key.to_string())
        .collect();
    assert_eq!(rows.len(), 14, "every per-profile setting has a legacy fallback");
    assert_eq!(fields, rows, "ProfilePrefs and the rows with a legacy fallback drifted");
    assert!(DESCRIPTORS.iter().filter(|d| d.fallback == Fallback::LegacyInstallKey).all(|d| d.scope == Scope::Profile));
}

/// A record that picked nothing serializes to nothing, and a session that never picked anything per
/// profile serializes WITHOUT the key, byte for byte as it did before the field existed.
#[test]
fn nothing_picked_serializes_to_no_key() {
    assert!(ProfilePrefs::default().is_empty());
    assert_eq!(serde_json::to_value(ProfilePrefs::default()).unwrap(), json!({}));
    let v = serde_json::to_value(Preferences::default()).unwrap();
    assert!(v.get("profile_prefs").is_none(), "an empty list is not written");
    let p = prefs_with(json!([record("kid", STAMP, json!({ "subtitle_size": "large" }))]));
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["profile_prefs"], json!([{ "user": "kid", "account": STAMP, "prefs": { "subtitle_size": "large" } }]));
}

/// **No `extensions`**: a field a later build adds is not carried by this one. A modelled field
/// round-trips; an unknown one is dropped (the tier-2 case the plan does not promise).
#[test]
fn an_unknown_field_of_a_record_is_not_carried() {
    let p = prefs_with(json!([record("kid", STAMP, json!({ "subtitle_size": "large", "future_setting": 7 }))]));
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["profile_prefs"][0]["prefs"], json!({ "subtitle_size": "large" }));
}

/// **P-d4.** A garbage own value is "no value of its own", which reads the LEGACY value, not the
/// type's default: every field is parsed by `de_soft_opt`, so one bad field costs only itself.
#[test]
fn garbage_own_value_reads_the_legacy_value() {
    let mut legacy = Preferences::default();
    SubtitleSizePref::put(&mut legacy, SubtitleSize::Large);
    QualityPref::put(&mut legacy, PlaybackQuality::P480);
    AudioEnhancementsPref::put(&mut legacy, AudioEnhancements { boost_dialog: true, normalize_loudness: false });
    let p = prefs_with(json!([record("kid", STAMP, json!({
        "subtitle_size": 7,
        "playback_quality": "garbage",
        "audio_enhancements": "garbage",
        "skip_interval": null,
        "subtitle_position": "high",
    }))]));
    let own = own_prefs(&p.profile_prefs, "kid", STAMP);
    assert!(own.is_some(), "one bad field costs only itself, never the record");
    assert_eq!(effective::<SubtitleSizePref>(&legacy, own), SubtitleSize::Large, "garbage reads legacy, not Medium");
    assert_eq!(effective::<QualityPref>(&legacy, own), PlaybackQuality::P480);
    assert_eq!(effective::<AudioEnhancementsPref>(&legacy, own),
        AudioEnhancements { boost_dialog: true, normalize_loudness: false });
    assert_eq!(effective::<SkipIntervalPref>(&legacy, own), SkipInterval::Seconds10, "null reads legacy, which is the default");
    assert_eq!(effective::<SubtitlePositionPref>(&legacy, own), SubtitlePosition::High, "a good neighbour survives");
}

/// **A size spelling this build does not know is read like a build before `Huge` reads `"huge"`**:
/// the profile's own value falls back to the install's older one and the install key to `Medium`;
/// neither fails the load. `"huge"` itself is this build's largest rung, in both places.
#[test]
fn an_unknown_size_falls_back_and_huge_round_trips() {
    let mut legacy = Preferences::default();
    SubtitleSizePref::put(&mut legacy, SubtitleSize::Large);
    let p = prefs_with(json!([record("kid", STAMP, json!({ "subtitle_size": "gigantic" }))]));
    assert_eq!(effective::<SubtitleSizePref>(&legacy, own_prefs(&p.profile_prefs, "kid", STAMP)), SubtitleSize::Large);
    let install: Preferences = serde_json::from_value(json!({ "subtitle_size": "gigantic" })).unwrap();
    assert_eq!(SubtitleSizePref::get(&install), SubtitleSize::Medium);

    let p = prefs_with(json!([record("kid", STAMP, json!({ "subtitle_size": "huge" }))]));
    assert_eq!(effective::<SubtitleSizePref>(&legacy, own_prefs(&p.profile_prefs, "kid", STAMP)), SubtitleSize::Huge);
    assert_eq!(serde_json::to_value(&p).unwrap()["profile_prefs"][0]["prefs"], json!({ "subtitle_size": "huge" }));
    let install: Preferences = serde_json::from_value(json!({ "subtitle_size": "huge" })).unwrap();
    assert_eq!(SubtitleSizePref::get(&install), SubtitleSize::Huge);
}

/// A record or a stamp that is the wrong shape costs only itself.
#[test]
fn a_malformed_record_costs_only_itself() {
    let p = prefs_with(json!([
        7,
        { "user": "kid", "account": 5, "prefs": { "subtitle_size": "large" } },
        { "user": "kid2", "account": STAMP, "prefs": "junk" },
        record("kid3", STAMP, json!({ "subtitle_size": "small" })),
    ]));
    assert!(own_prefs(&p.profile_prefs, "kid3", STAMP).is_some_and(|o| o.subtitle_size == Some(SubtitleSize::Small)));
    assert!(own_prefs(&p.profile_prefs, "kid", STAMP).is_none(), "a non-string stamp binds to no account");
    assert!(p.profile_prefs.iter().all(|r| r.user != "kid" || r.account.is_empty()));
}

// ---- reading ----------------------------------------------------------------------------------

/// The migration is a FALLBACK: the profile's own value, else the legacy top-level key, else the
/// type's default. A fresh install reads the defaults; a profile that never picked reads what the
/// TV had; a profile that picked reads its own, whatever the TV had.
#[test]
fn own_value_then_legacy_then_default() {
    let fresh = Session::default();
    let seat = Seat::Seated { user: "kid".into(), stamp: STAMP.into() };
    assert_eq!(stored_effective::<SubtitleSizePref>(&fresh, &seat), SubtitleSize::Medium, "a fresh install reads the default");
    assert_eq!(stored_effective::<QualityPref>(&fresh, &seat), PlaybackQuality::Original, "an absent quality is Original");

    let mut tv = Session::default();
    SubtitleSizePref::put(&mut tv.prefs, SubtitleSize::Large);
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &seat), SubtitleSize::Large, "a profile with no record reads the TV's value");

    tv.prefs.profile_prefs = prefs_with(json!([record("kid", STAMP, json!({ "subtitle_size": "small" }))])).profile_prefs;
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &seat), SubtitleSize::Small, "its own value wins");
    assert_eq!(stored_effective::<SubtitlePositionPref>(&tv, &seat), SubtitlePosition::Low, "an unpicked setting still falls back");
    let other = Seat::Seated { user: "sibling".into(), stamp: STAMP.into() };
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &other), SubtitleSize::Large, "another profile's pick is not seen");
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &Seat::Unseated), SubtitleSize::Large,
        "nobody seated: nobody's own value, the legacy one");
}

/// **A stale stamp is invisible**, an empty one matches nothing, and the owner's `""` records of two
/// accounts never mix.
#[test]
fn a_record_is_visible_only_under_its_own_non_empty_stamp() {
    let mut tv = Session::default();
    tv.prefs.profile_prefs = prefs_with(json!([
        record("", STAMP, json!({ "subtitle_size": "small" })),
        record("", OTHER_STAMP, json!({ "subtitle_size": "extra_large" })),
        record("kid", "", json!({ "subtitle_size": "large" })),
    ]))
    .profile_prefs;
    let owner = |stamp: &str| Seat::Seated { user: String::new(), stamp: stamp.into() };
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &owner(STAMP)), SubtitleSize::Small);
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &owner(OTHER_STAMP)), SubtitleSize::ExtraLarge);
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &owner("0000000000000000")), SubtitleSize::Medium,
        "a stamp no record carries sees nothing");
    let kid = Seat::Seated { user: "kid".into(), stamp: String::new() };
    assert_eq!(stored_effective::<SubtitleSizePref>(&tv, &kid), SubtitleSize::Medium, "an unstamped seat reads no record");
}

// ---- writing ----------------------------------------------------------------------------------

/// `put_own` records an own value even when it equals the fallback (the first pick), skips an equal
/// own value, refuses an empty stamp, prunes records of other stamps, and drops nothing else.
#[test]
fn put_own_records_prunes_and_skips() {
    let mut records: Vec<ProfilePrefsRecord> = Vec::new();
    assert!(!put_own::<SubtitleSizePref>(&mut records, "kid", "", SubtitleSize::Large), "an empty stamp is refused");
    assert!(records.is_empty());

    assert!(put_own::<SubtitleSizePref>(&mut records, "kid", STAMP, SubtitleSize::Medium),
        "the first pick records an own value even when it equals the default");
    assert_eq!(records.len(), 1);
    assert!(!put_own::<SubtitleSizePref>(&mut records, "kid", STAMP, SubtitleSize::Medium), "equal: unchanged");
    assert!(put_own::<SubtitlePositionPref>(&mut records, "kid", STAMP, SubtitlePosition::High), "another setting of the same record");
    assert!(put_own::<SubtitleSizePref>(&mut records, "sibling", STAMP, SubtitleSize::Small));
    assert_eq!(records.len(), 2, "two profiles, two records");

    records.push(ProfilePrefsRecord { user: "ghost".into(), account: OTHER_STAMP.into(),
        prefs: ProfilePrefs { deck_press: Some(DeckPress::Play), ..Default::default() } });
    assert!(put_own::<SubtitleSizePref>(&mut records, "kid", STAMP, SubtitleSize::Large));
    assert!(records.iter().all(|r| r.account == STAMP), "the write pruned the previous sign-in's record");
    assert_eq!(records.len(), 2);
    assert_eq!(own_prefs(&records, "kid", STAMP).unwrap().subtitle_position, Some(SubtitlePosition::High), "and kept the rest of its own");
}

// ---- the seat ---------------------------------------------------------------------------------

fn profile(user: Option<&str>, stamp: &str) -> CurrentProfile {
    CurrentProfile { user: user.map(|u| UserRef { uuid: u.into(), ..Default::default() }), generation: 1, stamp: stamp.into() }
}

/// Nothing published seats nobody, a profile with no stamp seats nobody (a dev token, an older
/// recording), the owner (`uuid` empty) is seated like anyone else, and two seats are the same seat
/// only when user AND stamp match.
#[test]
fn a_publication_seats_only_a_stamped_profile() {
    assert_eq!(profile(None, STAMP).seat(), Seat::Unseated);
    assert_eq!(profile(Some("kid"), "").seat(), Seat::Unseated);
    assert_eq!(profile(Some(""), "").seat(), Seat::Unseated, "the owner of an unstamped sign-in is unseated too");
    assert_eq!(profile(Some("kid"), STAMP).seat(), Seat::Seated { user: "kid".into(), stamp: STAMP.into() });
    assert_eq!(profile(Some(""), STAMP).seat(), Seat::Seated { user: String::new(), stamp: STAMP.into() });
    assert_ne!(profile(Some("kid"), STAMP).seat(), profile(Some("kid"), OTHER_STAMP).seat(), "a re-login is a new seat");
    assert_eq!(profile(Some("kid"), STAMP).seat().user(), Some("kid"));
    assert_eq!(Seat::Unseated.user(), None);
}

/// The published profile carries the stamp; `current_seat()` and `is_seated_as` read it.
#[test]
fn the_published_seat_follows_the_publication() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("seat-follows-publication");
    assert_eq!(current_seat(), Seat::Unseated, "the neutral start");
    session.watching("kid");
    assert_eq!(current_seat(), Seat::Unseated, "an unstamped publication seats nobody");
    assert!(!super::super::is_seated_as("kid"));
    let stamp = session.seated("kid", "account-token");
    assert_eq!(current_seat(), Seat::Seated { user: "kid".into(), stamp: stamp.clone() });
    assert!(super::super::is_seated_as("kid"));
    assert!(!super::super::is_seated_as("sibling"));
    assert!(!super::super::is_seated_as(""), "the owner is not the kid");
    assert_eq!(stamp, crate::plex::grant::account_key("account-token"));
}

// ---- hero trailer autoplay (P1c) --------------------------------------------------------------

/// **An absent own value reads the legacy key, else ON.** A profile with no record on a file that
/// never had the key (on, the product default); on a file whose legacy key says off (off: the TV's
/// old choice is what a profile that never picked sees); a profile with its own pick over a
/// contrary legacy key; and nobody seated (the legacy key).
#[test]
fn trailer_autoplay_own_value_then_legacy_key_then_on() {
    let seat = Seat::Seated { user: "kid".into(), stamp: STAMP.into() };
    let sibling = Seat::Seated { user: "sibling".into(), stamp: STAMP.into() };
    let mut s: Session = serde_json::from_value(json!({})).unwrap();
    assert!(stored_effective::<TrailerAutoplayPref>(&s, &seat), "a file without the key: on");
    assert!(stored_effective::<TrailerAutoplayPref>(&s, &Seat::Unseated));

    let mut s_off: Session = serde_json::from_value(json!({"trailer_autoplay": false})).unwrap();
    assert!(!stored_effective::<TrailerAutoplayPref>(&s_off, &seat), "a profile that never picked reads the TV's off");
    assert!(!stored_effective::<TrailerAutoplayPref>(&s_off, &Seat::Unseated));

    s_off.prefs.profile_prefs = prefs_with(json!([record("kid", STAMP, json!({ "trailer_autoplay": true }))])).profile_prefs;
    assert!(stored_effective::<TrailerAutoplayPref>(&s_off, &seat), "its own on beats the TV's off");
    assert!(!stored_effective::<TrailerAutoplayPref>(&s_off, &sibling), "and the sibling still reads off");
    assert!(!stored_effective::<TrailerAutoplayPref>(&s_off, &Seat::Unseated), "nobody seated: nobody's pick");

    s.prefs.profile_prefs = prefs_with(json!([record("kid", STAMP, json!({ "trailer_autoplay": false }))])).profile_prefs;
    assert!(!stored_effective::<TrailerAutoplayPref>(&s, &seat), "its own off beats the TV's (absent, so on)");
    assert!(stored_effective::<TrailerAutoplayPref>(&s, &sibling), "and the sibling reads on");
}

/// **A garbage own trailer value reads the legacy key**, not ON: `de_soft_opt` makes it "no value of
/// its own". The legacy key's own garbage still reads ON (A0, unchanged: `de_soft_bool_on`).
#[test]
fn trailer_autoplay_garbage_own_value_reads_the_legacy_key() {
    let off: Preferences = serde_json::from_value(json!({"trailer_autoplay": false})).unwrap();
    for garbage in [json!("yes"), json!(7), json!(null), json!([true]), json!({"on": false})] {
        let p = prefs_with(json!([record("kid", STAMP, json!({ "trailer_autoplay": garbage, "deck_press": "play" }))]));
        let own = own_prefs(&p.profile_prefs, "kid", STAMP);
        assert!(own.is_some_and(|o| o.deck_press == Some(DeckPress::Play)), "{garbage}: a good neighbour survives");
        assert!(!effective::<TrailerAutoplayPref>(&off, own), "{garbage}: reads the legacy off, not ON");
    }
    let garbage_legacy: Preferences = serde_json::from_value(json!({"trailer_autoplay": "garbage"})).unwrap();
    assert!(effective::<TrailerAutoplayPref>(&garbage_legacy, None), "A0 unchanged: a garbage legacy key is ON");
}

/// **An off pick on profile A is not seen by B, and the TV's key is never written.**
#[test]
fn trailer_off_on_profile_a_is_not_seen_by_b_in_the_record() {
    let mut records: Vec<ProfilePrefsRecord> = Vec::new();
    assert!(put_own::<TrailerAutoplayPref>(&mut records, "a", STAMP, false));
    assert!(!put_own::<TrailerAutoplayPref>(&mut records, "a", STAMP, false), "equal: unchanged");
    let mut tv: Session = serde_json::from_value(json!({})).unwrap();
    tv.prefs.profile_prefs = records;
    let a = Seat::Seated { user: "a".into(), stamp: STAMP.into() };
    let b = Seat::Seated { user: "b".into(), stamp: STAMP.into() };
    assert!(!stored_effective::<TrailerAutoplayPref>(&tv, &a));
    assert!(stored_effective::<TrailerAutoplayPref>(&tv, &b), "b picked nothing: the TV's on");
    assert!(tv.prefs.trailer_autoplay, "and the TV's key is what it was");
}

// ---- persisting -------------------------------------------------------------------------------

fn stored() -> Session {
    crate::plex::session::load()
}

fn without_profile_prefs(session: &Session) -> Value {
    let mut v = serde_json::to_value(session).unwrap();
    v.as_object_mut().unwrap().remove("profile_prefs");
    v
}

/// The write lands on the captured profile's record, records an own value, and NEVER writes a
/// top-level key: the legacy keys are frozen as the fallback.
#[test]
fn profile_choose_never_writes_the_legacy_key() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("never-writes-legacy");
    let stamp = session.seated("kid", "account-token");
    let before = stored();
    let w = ProfileWrite::capture().expect("a seated profile");
    assert_eq!((w.user(), w.stamp()), ("kid", stamp.as_str()));

    assert!(persist_profile_pref::<SubtitleSizePref>(&w, SubtitleSize::Large));
    assert!(persist_profile_pref::<QualityPref>(&w, PlaybackQuality::Original), "Original over an absent key still records Some");
    assert!(persist_profile_pref::<DirectPlayModePref>(&w, DirectPlayMode::Forced));
    assert!(persist_profile_pref::<NextEpisodeModePref>(&w, NextEpisodeMode::Off));
    assert!(persist_profile_pref::<SkipIntervalPref>(&w, SkipInterval::Seconds30));
    assert!(persist_profile_pref::<DeckPressPref>(&w, DeckPress::Play));
    assert!(persist_profile_pref::<SubtitleTonePref>(&w, SubtitleTone::Silver));
    assert!(persist_profile_pref::<SubtitlePositionPref>(&w, SubtitlePosition::High));
    assert!(persist_profile_pref::<AudioEnhancementsPref>(&w, AudioEnhancements { boost_dialog: true, normalize_loudness: false }));
    assert!(persist_profile_pref::<TrailerAutoplayPref>(&w, false), "turning trailers off records an own value");

    let after = stored();
    assert_eq!(without_profile_prefs(&before), without_profile_prefs(&after),
        "every top-level key, the frozen fallbacks included, is byte-unchanged by every pick");
    let own = own_prefs(&after.prefs.profile_prefs, "kid", &stamp).expect("the profile's record");
    assert_eq!(own.subtitle_size, Some(SubtitleSize::Large));
    assert_eq!(own.playback_quality, Some(PlaybackQuality::Original));
    assert_eq!(own.audio_enhancements, Some(AudioEnhancements { boost_dialog: true, normalize_loudness: false }));
    assert_eq!(own.trailer_autoplay, Some(false));
    assert_eq!(after.prefs.trailer_autoplay, before.prefs.trailer_autoplay, "and the TV's key is what it was: the pick did not write it");
    assert!(!ProfilePrefs { ..own.clone() }.is_empty());
}

/// **#741: a picked 50% is on disk and survives the default moving from 100% to 75%.** The pick is
/// written as the profile's OWN value (`"half"`), so the build that reads it back, whichever
/// default it carries, shows Half; and the write is also made for 100% (no longer the skipped
/// default) so a chosen Full is not reinterpreted as the new 75% default.
#[test]
fn a_picked_hdr_rung_is_on_disk_and_survives_the_default_changing() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("hdr-rung-on-disk");
    let stamp = session.seated("kid", "account-token");
    let w = ProfileWrite::capture().expect("a seated profile");
    let seat = Seat::Seated { user: "kid".into(), stamp: stamp.clone() };

    assert!(persist_profile_pref::<HdrOverlayBrightnessPref>(&w, HdrOverlayBrightness::Half));
    // Read back from the file, as a relaunch does, not from the live cache.
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(session.path()).unwrap()).unwrap();
    assert_eq!(raw["profile_prefs"][0]["prefs"]["hdr_overlay_brightness"], json!("half"));
    assert_eq!(stored_effective::<HdrOverlayBrightnessPref>(&stored(), &seat), HdrOverlayBrightness::Half);

    assert!(persist_profile_pref::<HdrOverlayBrightnessPref>(&w, HdrOverlayBrightness::Full));
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(session.path()).unwrap()).unwrap();
    assert_eq!(raw["profile_prefs"][0]["prefs"]["hdr_overlay_brightness"], json!("full"), "Full is written, not skipped as the default");
    assert_eq!(stored_effective::<HdrOverlayBrightnessPref>(&stored(), &seat), HdrOverlayBrightness::Full);

    // A file an older build (default Full, key absent for a profile that never picked) wrote.
    let old: Preferences = serde_json::from_value(json!({
        "profile_prefs": [record("kid", STAMP, json!({ "hdr_overlay_brightness": "half" })), record("pal", STAMP, json!({}))]
    })).unwrap();
    let mut tv: Session = serde_json::from_value(json!({})).unwrap();
    tv.prefs = old;
    let kid = Seat::Seated { user: "kid".into(), stamp: STAMP.into() };
    let pal = Seat::Seated { user: "pal".into(), stamp: STAMP.into() };
    assert_eq!(stored_effective::<HdrOverlayBrightnessPref>(&tv, &kid), HdrOverlayBrightness::Half);
    assert_eq!(stored_effective::<HdrOverlayBrightnessPref>(&tv, &pal), HdrOverlayBrightness::ThreeQuarters, "no own value: the new default");
}

/// An equal own value is no write (and is saved, nothing was refused); a different one is.
#[test]
fn an_unchanged_own_value_is_saved_without_a_write() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("unchanged-own");
    session.seated("kid", "account-token");
    let w = ProfileWrite::capture().unwrap();
    assert!(persist_profile_pref::<SubtitleSizePref>(&w, SubtitleSize::Large));
    let path = session.path();
    let inode = std::fs::metadata(&path).map(|m| (m.len(), m.modified().ok())).ok();
    assert!(persist_profile_pref::<SubtitleSizePref>(&w, SubtitleSize::Large), "unchanged is saved");
    assert_eq!(std::fs::metadata(&path).map(|m| (m.len(), m.modified().ok())).ok(), inode, "and nothing was written");
}

/// **P-d3.** A write captured under one credential or client lands nowhere once either moved: the
/// account-data guard of the persist half. (The tenure half, a sign-out between the pick and the
/// write, is `media::route::prefs_tests::profile_write_queued_across_sign_out_is_dropped`.)
#[test]
fn a_pick_is_refused_across_a_credential_or_client_change() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("refused-across");
    let stamp = session.seated("kid", "account-token");

    // The account token moved (a different login over the same file).
    let w = ProfileWrite::capture().unwrap();
    assert!(crate::plex::session::update(|cur| Some(Session { account_token: "another-token".into(), ..cur.clone() })));
    assert!(!persist_profile_pref::<SubtitleSizePref>(&w, SubtitleSize::Large), "the credential moved");
    assert!(stored().prefs.profile_prefs.is_empty());
    assert!(crate::plex::session::update(|cur| Some(Session { account_token: "account-token".into(), ..cur.clone() })));
    assert!(persist_profile_pref::<SubtitleSizePref>(&w, SubtitleSize::Large), "back under the captured credential it lands");

    // The client id moved.
    assert!(crate::plex::session::update(|cur| Some(Session { client_id: "another-client".into(), ..cur.clone() })));
    assert!(!persist_profile_pref::<SubtitlePositionPref>(&w, SubtitlePosition::High), "the client id moved");
    assert!(crate::plex::session::update(|cur| Some(Session { client_id: "cid-test".into(), ..cur.clone() })));

    assert_eq!(own_prefs(&stored().prefs.profile_prefs, "kid", &stamp).and_then(|o| o.subtitle_position), None,
        "neither refused pick left a trace");
}

/// **P-d9: a re-login without `ClearTenure` leaves the previous sign-in's picks unreachable, and the
/// next write prunes them.** The boot path that replaces the account token over a stored record
/// without a sign-out keeps the file's `profile_prefs`, which are stamped with the OLD token. Under
/// the new stamp the old records are invisible (the profile reads the fallback, not a stranger's
/// pick), and the first write under the new stamp drops them, so they cannot linger forever.
#[test]
fn relogin_without_clear_hides_and_prunes_profile_data() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("relogin-without-clear");
    let old_stamp = session.seated("kid", "account-token");
    let w = ProfileWrite::capture().expect("a seated profile");
    assert!(persist_profile_pref::<SubtitleSizePref>(&w, SubtitleSize::Large));
    assert_eq!(stored_effective::<SubtitleSizePref>(&stored(), &current_seat()), SubtitleSize::Large, "setup: the pick reads back");

    // The re-login: a new token over the stored record, nothing cleared.
    let new_stamp = session.seated("kid", "next-token");
    assert_ne!(new_stamp, old_stamp);
    let now = stored();
    assert_eq!(now.prefs.profile_prefs.len(), 1, "the old sign-in's record is still on disk");
    assert_eq!(now.prefs.profile_prefs[0].account, old_stamp);
    assert_eq!(stored_effective::<SubtitleSizePref>(&now, &current_seat()), SubtitleSize::Medium,
        "and invisible: the new sign-in reads the fallback, not the old pick");

    // The first write under the new stamp prunes it.
    let w = ProfileWrite::capture().expect("seated under the new stamp");
    assert!(persist_profile_pref::<SubtitlePositionPref>(&w, SubtitlePosition::High));
    let after = stored().prefs.profile_prefs;
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].account, new_stamp, "only the new sign-in's record is left");
    assert_eq!(own_prefs(&after, "kid", &old_stamp), None);
    assert_eq!(stored_effective::<SubtitleSizePref>(&stored(), &current_seat()), SubtitleSize::Medium,
        "and the old size did not come back with the new record");
}

/// A write while nobody is seated has no `ProfileWrite` at all: the door refuses before it starts.
#[test]
fn nobody_seated_captures_no_write() {
    let _g = plx_base::testlock::serial();
    let session = TempSession::new("capture-unseated");
    assert!(ProfileWrite::capture().is_none());
    session.watching("kid");
    assert!(ProfileWrite::capture().is_none(), "an unstamped publication seats nobody");
    session.seated("kid", "account-token");
    assert!(ProfileWrite::capture().is_some());
}

/// **The HDR overlay brightness rides the same per-profile plumbing as every other rung**: it
/// defaults to ThreeQuarters (75%, the dim on out of the box), a profile's own pick reads back and round-trips its
/// stored spelling, a spelling this build does not know falls back to the install's value, and the
/// factors are the plain 1.0 / 0.75 / 0.5 multipliers the picker's labels say.
#[test]
fn hdr_overlay_brightness_defaults_to_three_quarters_and_round_trips() {
    assert_eq!(HdrOverlayBrightness::default(), HdrOverlayBrightness::ThreeQuarters);
    assert_eq!(HdrOverlayBrightnessPref::get(&Preferences::default()), HdrOverlayBrightness::ThreeQuarters);
    assert!(
        serde_json::to_value(Preferences::default()).unwrap().get("hdr_overlay_brightness").is_none(),
        "the default is not written, so a session that never chose serializes as it always did"
    );
    let factors: Vec<f32> = HdrOverlayBrightness::LADDER.iter().map(|b| b.factor()).collect();
    assert_eq!(factors, [1.0, 0.75, 0.5]);
    let percents: Vec<i64> = HdrOverlayBrightness::LADDER.iter().map(|b| b.percent()).collect();
    assert_eq!(percents, [100, 75, 50]);
    for b in HdrOverlayBrightness::LADDER {
        assert_eq!(HdrOverlayBrightness::from_index(b.index()), b);
    }
    assert_eq!(HdrOverlayBrightness::from_index(200), HdrOverlayBrightness::ThreeQuarters);

    let legacy = Preferences::default();
    let p = prefs_with(json!([record("kid", STAMP, json!({ "hdr_overlay_brightness": "half" }))]));
    assert_eq!(
        effective::<HdrOverlayBrightnessPref>(&legacy, own_prefs(&p.profile_prefs, "kid", STAMP)),
        HdrOverlayBrightness::Half
    );
    assert_eq!(serde_json::to_value(&p).unwrap()["profile_prefs"][0]["prefs"], json!({ "hdr_overlay_brightness": "half" }));
    let p = prefs_with(json!([record("kid", STAMP, json!({ "hdr_overlay_brightness": "full" }))]));
    assert_eq!(
        effective::<HdrOverlayBrightnessPref>(&legacy, own_prefs(&p.profile_prefs, "kid", STAMP)),
        HdrOverlayBrightness::Full,
        "a profile that chose 100% keeps it (it is written: the default is 75%)"
    );
    assert_eq!(serde_json::to_value(&p).unwrap()["profile_prefs"][0]["prefs"], json!({ "hdr_overlay_brightness": "full" }));
    let p = prefs_with(json!([record("kid", STAMP, json!({ "hdr_overlay_brightness": "a_quarter" }))]));
    assert_eq!(
        effective::<HdrOverlayBrightnessPref>(&legacy, own_prefs(&p.profile_prefs, "kid", STAMP)),
        HdrOverlayBrightness::ThreeQuarters,
        "an unknown spelling reads the install's value"
    );
    let install: Preferences = serde_json::from_value(json!({ "hdr_overlay_brightness": "half" })).unwrap();
    assert_eq!(HdrOverlayBrightnessPref::get(&install), HdrOverlayBrightness::Half);
}
