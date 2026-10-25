//! The tie tests of `prefs.rs`: every list of settings held to every other, in BOTH directions, so
//! a field added to one and forgotten in the other fails here instead of drifting.
//!
//! * **T-fields** — `Preferences`' serde field list equals `DESCRIPTORS`' keys.
//! * **T-account** — `DESCRIPTORS`' account-bound keys equal platform's
//!   `ACCOUNT_BOUND_PREFERENCES`, and `auto_sign_in` is never account-bound.
//! * **T-parse** — each descriptor's declared parse policy is the one its field really has.
//!
//! T-live and T-settings sit in media (`route/prefs_tests.rs`), where the lists they hold to these
//! descriptors are declared.
use super::prefs::{Descriptor, Fallback, ListRow, Parse, ProfileMemory, Row, Scope, Setting, SettingsWrite, DESCRIPTORS};
use super::prefs::{AudioEnhancementsPref, AutoSignInPref, DeckPressPref, UnderPostersPref, DirectPlayModePref, LastHeroBlurPref,
    LastLibraryRow, LibraryViewsRow, NextEpisodeModePref, PlaintextConsentRow, QualityPref, ServerKeyPinsRow,
    HdrOverlayBrightnessPref, SkipIntervalPref, SubtitleOffsetsPref, SubtitlePositionPref, SubtitleSizePref, SubtitleTonePref,
    HoldHintTaughtPref, TrailerAutoplayPref, UiResolutionPref};
use super::{
    CanonicalSessionPreferences, DeckPress, UnderPosters, UnderPostersDetails, DirectPlayMode, LastLibrary, LibraryViews, NextEpisodeMode,
    PlaintextChoice, PlaintextConsent, PlaybackQuality, Preferences, ServerKeyPin, Session, SkipInterval,
    HdrOverlayBrightness, HoldHintTaught, SubtitleOffsetEntry, SubtitlePosition, SubtitleSize, SubtitleTone, UiResolution,
};
use serde::de::{self, Deserializer, Visitor};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// A `Deserializer` that does nothing but record the field list `#[derive(Deserialize)]` hands to
/// `deserialize_struct`. `Preferences` has no inner flatten, so derive takes that path, and the
/// list is serde's own account of which keys the struct reads.
struct CaptureFields<'a>(&'a mut Vec<&'static str>);

impl<'de, 'a> Deserializer<'de> for CaptureFields<'a> {
    type Error = de::value::Error;
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.0.extend_from_slice(fields);
        Err(de::Error::custom("captured"))
    }
    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(de::Error::custom("only deserialize_struct is expected"))
    }
    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option
        unit unit_struct newtype_struct seq tuple tuple_struct map enum identifier ignored_any
    }
}

fn preferences_fields() -> Vec<&'static str> {
    let mut fields = Vec::new();
    let _ = <Preferences as serde::Deserialize>::deserialize(CaptureFields(&mut fields));
    assert!(!fields.is_empty(), "the capture saw no field list: derive no longer calls deserialize_struct");
    fields
}

fn keys() -> BTreeSet<&'static str> {
    DESCRIPTORS.iter().map(|d| d.key).collect()
}

/// **T-fields.** A field without a `Setting`, or a `Setting` without a field, fails.
#[test]
fn t_fields_every_field_is_described_by_exactly_one_setting() {
    let fields = preferences_fields();
    let field_set: BTreeSet<&str> = fields.iter().copied().collect();
    assert_eq!(field_set.len(), fields.len(), "a field is listed twice: {fields:?}");
    let described: Vec<&str> = DESCRIPTORS.iter().map(|d| d.key).collect();
    assert_eq!(keys().len(), described.len(), "two settings share a key: {described:?}");
    let described_set = keys();
    let missing: Vec<_> = field_set.difference(&described_set).collect();
    let orphan: Vec<_> = described_set.difference(&field_set).collect();
    assert!(missing.is_empty(), "Preferences fields with no Setting row: {missing:?}");
    assert!(orphan.is_empty(), "Setting rows with no Preferences field: {orphan:?}");
}

/// Each marker's `KEY` is the key its field really serializes under, and `put`/`get` round-trip
/// through the field. A marker that names the wrong field (two settings of one value type are the
/// case a type-keyed trait could not tell apart) fails here.
fn check_row<S: Setting>(non_default: S::Value)
where
    S::Value: std::fmt::Debug,
{
    let mut p = Preferences::default();
    assert_ne!(S::get(&p), non_default, "{}: the fixture must differ from the default", S::KEY);
    S::put(&mut p, non_default);
    assert_eq!(S::get(&p), non_default, "{}", S::KEY);
    let json = serde_json::to_value(&p).unwrap();
    let object = json.as_object().unwrap();
    assert!(object.contains_key(S::KEY), "{}: writing the row must write its own key: {json}", S::KEY);
    // and only its own: every other row is still at its default, so none of them moved.
    let mut others = Preferences::default();
    S::put(&mut others, S::get(&Preferences::default()));
    let default_json = serde_json::to_value(&others).unwrap();
    for (key, value) in object {
        if key != S::KEY {
            assert_eq!(default_json.get(key), Some(value), "{}: moved the unrelated key {key}", S::KEY);
        }
    }
    let back: Preferences = serde_json::from_value(json).unwrap();
    assert_eq!(S::get(&back), non_default, "{}: the written key must read back", S::KEY);
}

#[test]
fn every_marker_names_its_own_field() {
    check_row::<QualityPref>(PlaybackQuality::P720);
    check_row::<DirectPlayModePref>(DirectPlayMode::Forced);
    check_row::<SubtitleTonePref>(SubtitleTone::Grey);
    check_row::<SubtitleSizePref>(SubtitleSize::Large);
    check_row::<SubtitlePositionPref>(SubtitlePosition::High);
    check_row::<HdrOverlayBrightnessPref>(HdrOverlayBrightness::Half);
    check_row::<NextEpisodeModePref>(NextEpisodeMode::Off);
    check_row::<SkipIntervalPref>(SkipInterval::Seconds30);
    check_row::<DeckPressPref>(DeckPress::Play);
    check_row::<UnderPostersPref>(UnderPosters { title: true, details: UnderPostersDetails::Genre });
    check_row::<AudioEnhancementsPref>(crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false });
    // A5: the remaining `Copy` rows.
    check_row::<AutoSignInPref>(true);
    check_row::<TrailerAutoplayPref>(true);
    check_row::<LastHeroBlurPref>(Some([[0.5, 0.25, 0.125]; 4]));
    check_row::<UiResolutionPref>(UiResolution::Hd720);
    check_row::<HoldHintTaughtPref>(HoldHintTaught { card: true, pill: false });
    // The table lists exactly these sixteen `Setting` rows, the five list rows below, the two
    // memory-door rows (`TrackMemoryPref`, whose field check is
    // `track_memory_tests::the_list_row_names_its_own_field`, and `SubtitleOffsetsPref`, checked by
    // `the_subtitle_offsets_row_names_its_own_field`) and the per-profile record list
    // (`ProfilePrefsPref`, `profile_prefs_tests::nothing_picked_serializes_to_no_key`): a row needs
    // its line in one of the four places.
    assert_eq!(DESCRIPTORS.len(), 16 + 5 + 2 + 1);
}

/// The list rows do what `check_row` proves for the `Copy` ones: the marker names its own field
/// (writing the list writes its key and no other, and the key reads back).
fn check_list_row<R: ListRow>(item: R::Item)
where
    R::Item: Clone + PartialEq + std::fmt::Debug,
{
    let default_json = serde_json::to_value(Preferences::default()).unwrap();
    let mut p = Preferences::default();
    assert!(R::items(&p).is_empty(), "{}: a list row starts empty", R::KEY);
    R::items_mut(&mut p).push(item.clone());
    let json = serde_json::to_value(&p).unwrap();
    let object = json.as_object().unwrap();
    assert!(object.contains_key(R::KEY), "{}: writing the row must write its own key: {json}", R::KEY);
    for (key, value) in object {
        if key != R::KEY {
            assert_eq!(default_json.get(key), Some(value), "{}: moved the unrelated key {key}", R::KEY);
        }
    }
    let back: Preferences = serde_json::from_value(json).unwrap();
    assert_eq!(R::items(&back), &vec![item], "{}: the written key must read back", R::KEY);
}

#[test]
fn every_list_marker_names_its_own_field() {
    check_list_row::<PlaintextConsentRow>(PlaintextConsent {
        machine_id: "m".into(), account: "a".into(), choice: PlaintextChoice::Allowed, extensions: Default::default(),
    });
    check_list_row::<ServerKeyPinsRow>(ServerKeyPin {
        machine_id: "m".into(), pin: "sha256//x".into(), extensions: Default::default(),
    });
    check_list_row::<LastLibraryRow>(serde_json::from_value::<LastLibrary>(
        json!({"user":"u1","libs":[{"kind":"movie","machine_id":"m","key":1}]})).unwrap());
    check_list_row::<LibraryViewsRow>(serde_json::from_value::<LibraryViews>(
        json!({"user":"u1","libs":[{"machine_id":"m","key":1,"sort":"plays"}]})).unwrap());
    // `library_views` is stored as `library_sorts`: the row's key is the wire key.
    assert_eq!(LibraryViewsRow::KEY, "library_sorts");
}

/// Quality is the one row whose stored and spoken types differ: absent reads as Original, and an
/// explicit pick stores `Some` (even `Some(Original)`).
#[test]
fn quality_is_stored_optional_and_read_as_original() {
    let mut p = Preferences::default();
    assert_eq!(*QualityPref::stored(&p), None);
    assert_eq!(QualityPref::get(&p), PlaybackQuality::Original);
    QualityPref::put(&mut p, PlaybackQuality::Original);
    assert_eq!(*QualityPref::stored(&p), Some(PlaybackQuality::Original));
}

/// **T-account.** Platform names the preference keys sign-out clears; this crate declares which
/// rows are account-bound. The two lists are held to each other both ways: every key platform
/// clears has a row that says so, and no row says so that platform does not clear.
#[test]
fn t_account_the_bound_rows_are_the_platform_bound_keys() {
    let platform: BTreeSet<&str> = plx_platform::storage::state::ACCOUNT_BOUND_PREFERENCES.iter().copied().collect();
    let declared: BTreeSet<&str> = DESCRIPTORS.iter().filter(|d| d.account_bound).map(|d| d.key).collect();
    assert_eq!(declared, platform, "account-bound rows and platform's ACCOUNT_BOUND_PREFERENCES drifted");
    // Every key platform clears is a row at all (a bound key with no row would be unlisted here).
    for key in &platform {
        assert!(keys().contains(key), "{key}: platform clears it at sign-out but no row describes it");
    }
    // `auto_sign_in` is read by `boot_shows_picker` BEFORE any profile is seated, so it can never be
    // per-profile and never account-bound (sign-out keeps the television's switch).
    let auto = DESCRIPTORS.iter().find(|d| d.key == "auto_sign_in").expect("auto_sign_in has a row");
    assert_eq!(auto.scope, Scope::Install);
    assert!(!auto.account_bound && !platform.contains("auto_sign_in"));
    for d in DESCRIPTORS {
        match d.scope {
            Scope::Install => assert!(!d.account_bound, "{}: an Install row is never account-bound", d.key),
            Scope::Account => assert!(d.account_bound, "{}: an Account row is always account-bound", d.key),
            _ => {}
        }
    }
}

/// The facts a row declares that a later PR reads (`LIVE` by media's T-live, `SETTINGS` by the
/// Settings dispatcher's T-settings), pinned here per key so a change is a visible edit. The nine
/// are `Profile` rows with the legacy install key as their fallback since the per-profile work (P1a),
/// and so is `trailer_autoplay` since P1c, which also gained its live cell and its Confirm Settings
/// row then; every other fact, of every other row, is what `main` declared.
#[test]
fn the_declared_live_and_settings_facts_are_the_ones_main_has() {
    let row = |key: &str| -> &Descriptor { DESCRIPTORS.iter().find(|d| d.key == key).unwrap() };
    use SettingsWrite::{Confirm, Optimistic};
    for (key, settings) in [
        ("playback_quality", Some(Confirm)),
        ("direct_play_mode", Some(Confirm)),
        ("next_episode_mode", Some(Confirm)),
        ("skip_interval", Some(Confirm)),
        ("deck_press", Some(Confirm)),
        ("under_posters", Some(Confirm)),
        ("subtitle_size", Some(Optimistic)),
        ("subtitle_position", Some(Optimistic)),
        ("hdr_overlay_brightness", Some(Optimistic)),
        ("subtitle_tone", None),
        ("audio_enhancements", None),
        ("trailer_autoplay", Some(Confirm)),
    ] {
        let d = row(key);
        assert_eq!(d.settings, settings, "{key}");
        assert!(d.live, "{key}: every one of the twelve has a live cell in media");
        assert_eq!(d.scope, Scope::Profile, "{key}: DECLARED FLIP (P1a, and P1c for trailer_autoplay, where live and settings flipped too): it was Install on main");
        assert_eq!(d.fallback, Fallback::LegacyInstallKey, "{key}: a profile with no value of its own reads the TV's");
        assert!(!d.account_bound, "{key}: the top-level key is the TV's, the profile's own value is `profile_prefs`");
    }
}

/// The facts the A5 rows declare, pinned per key like the ten above: none is live (no media cell
/// applies them) and none has a `choose` Settings row (Settings writes `auto_sign_in` through its
/// own ticketed update). (`trailer_autoplay` was one of these until P1c.)
#[test]
fn the_a5_rows_declare_the_scope_and_binding_main_has() {
    let row = |key: &str| -> &Descriptor { DESCRIPTORS.iter().find(|d| d.key == key).unwrap() };
    for (key, scope, bound, parse) in [
        ("auto_sign_in", Scope::Install, false, Parse::BoolOff),
        ("last_hero_blur", Scope::Install, false, Parse::OptionalSoft),
        ("ui_resolution", Scope::Install, false, Parse::Soft),
        ("hold_hint_taught", Scope::Install, false, Parse::Soft),
        ("plaintext_consent", Scope::Account, true, Parse::SoftVec),
        ("server_key_pins", Scope::Account, true, Parse::SoftVec),
        ("last_library", Scope::Profile, true, Parse::SoftVec),
        ("library_sorts", Scope::Profile, true, Parse::SoftVec),
    ] {
        let d = row(key);
        assert_eq!((d.scope, d.account_bound, d.parse), (scope, bound, parse), "{key}");
        assert!(!d.live && d.settings.is_none(), "{key}");
    }
}

/// **The taught hold hints** (`plx_ui::hold_hint`): an absent key is "not taught" and writes no
/// key, each flag round-trips through both on-disk hosts, a damaged flag costs only itself, and the
/// row is the television's (Install, never account-bound, so sign-out keeps it:
/// `platform::storage::state::clear_tenure_keeps_the_taught_hold_hints`).
#[test]
fn the_taught_hold_hints_default_absent_round_trip_and_are_install_wide() {
    let default_json = serde_json::to_value(Preferences::default()).unwrap();
    assert!(default_json.get("hold_hint_taught").is_none(), "nothing taught writes no key: {default_json}");
    assert!(!serde_json::to_string(&Session::default()).unwrap().contains("hold_hint_taught"));
    let absent: Preferences = serde_json::from_value(json!({})).unwrap();
    assert_eq!(absent.hold_hint_taught, HoldHintTaught::NONE);

    for taught in [
        HoldHintTaught { card: true, pill: false },
        HoldHintTaught { card: false, pill: true },
        HoldHintTaught { card: true, pill: true },
    ] {
        let session = Session::default().with_pref::<HoldHintTaughtPref>(taught);
        let legacy: Session = serde_json::from_slice(&serde_json::to_vec(&session).unwrap()).unwrap();
        assert_eq!(legacy.hold_hint_taught(), taught);
        let (public, protected) = super::split_canonical(&session).unwrap();
        assert_eq!(super::join_canonical(&public, &protected).unwrap().hold_hint_taught(), taught);
        assert_eq!(super::public_session(&public).hold_hint_taught(), taught);
    }
    let one = serde_json::to_value(Session::default().with_pref::<HoldHintTaughtPref>(HoldHintTaught { card: true, pill: false })).unwrap();
    assert_eq!(one["hold_hint_taught"], json!({"card": true}), "only the flag that is set is written");

    let damaged: Preferences = serde_json::from_value(json!({"hold_hint_taught": {"card": "yes", "pill": true}})).unwrap();
    assert_eq!(damaged.hold_hint_taught, HoldHintTaught { card: false, pill: true }, "a damaged flag costs only itself");

    let row = DESCRIPTORS.iter().find(|d| d.key == "hold_hint_taught").expect("the row is in the table");
    assert_eq!((row.scope, row.account_bound, row.live, row.settings), (Scope::Install, false, false, None));
    assert!(!plx_platform::storage::state::ACCOUNT_BOUND_PREFERENCES.contains(&"hold_hint_taught"));
}

/// The `subtitle_offsets` row is a `Preferences` field under its own key, written only while
/// non-empty, and its descriptor is the memory-door tenant it was before A5 (per item, account-bound
/// from SO on, no live cell, no Settings row). It is a `Row` and not a `ListRow`: the
/// whole-list accessor is exactly what the memory door exists to withhold.
#[test]
fn the_subtitle_offsets_row_names_its_own_field() {
    let default_json = serde_json::to_value(Preferences::default()).unwrap();
    assert!(default_json.get("subtitle_offsets").is_none(), "an empty list writes no key: {default_json}");
    let mut p = Preferences::default();
    assert!(p.subtitle_offsets.upsert_with("u1", "", &("m".to_string(), "42".to_string()), |_| {
        Some(SubtitleOffsetEntry { machine_id: "m".into(), rating_key: "42".into(), offset_ms: 1_500, extensions: Default::default() })
    }));
    let body = serde_json::to_value(&p).unwrap();
    assert!(body["subtitle_offsets"].is_array(), "{body}");
    for (key, value) in body.as_object().unwrap() {
        if key != SubtitleOffsetsPref::KEY {
            assert_eq!(default_json.get(key), Some(value), "writing the row moved the unrelated key {key}");
        }
    }
    let back: Preferences = serde_json::from_value(body).unwrap();
    assert_eq!(back, p);
    let row = DESCRIPTORS.iter().find(|d| d.key == SubtitleOffsetsPref::KEY).expect("the row is in the table");
    assert_eq!((row.scope, row.account_bound, row.live, row.settings, row.parse), (Scope::PerItem, true, false, None, Parse::SoftVec));
}

/// What garbage a key is fed, per descriptor key. A descriptor with no entry here fails
/// `t_parse`, and so does an entry without a descriptor.
fn garbage_fixtures() -> Vec<(&'static str, Vec<Value>)> {
    let generic = || vec![Value::Null, json!("zzz-not-a-spelling"), json!(17), json!(true), json!([1, "x"]), json!({"unexpected": 1})];
    vec![
        ("playback_quality", generic()),
        ("direct_play_mode", generic()),
        ("subtitle_tone", generic()),
        ("subtitle_size", generic()),
        ("subtitle_position", generic()),
        ("hdr_overlay_brightness", generic()),
        ("next_episode_mode", generic()),
        ("skip_interval", generic()),
        ("deck_press", generic()),
        ("under_posters", generic()),
        ("audio_enhancements", generic()),
        // A5. A switch's garbage excludes the booleans, which are VALID values for it.
        ("auto_sign_in", not_a_bool()),
        ("ui_resolution", generic()),
        ("hold_hint_taught", generic()),
        ("trailer_autoplay", not_a_bool()),
        ("last_hero_blur", vec![Value::Null, json!("zzz"), json!(17), json!([1, "x"]), json!([[1, 2, 3]]), json!({"unexpected": 1})]),
        // A list's garbage is anything that is not an array; a mixed array is driven separately.
        ("plaintext_consent", not_a_list()),
        ("server_key_pins", not_a_list()),
        ("native_audio_refused", not_a_list()),
        ("last_library", not_a_list()),
        ("library_sorts", not_a_list()),
        ("track_memory", not_a_list()),
        ("subtitle_offsets", not_a_list()),
        ("profile_prefs", not_a_list()),
    ]
}

fn not_a_bool() -> Vec<Value> {
    vec![Value::Null, json!("zzz-not-a-spelling"), json!("true"), json!(17), json!([1, "x"]), json!({"unexpected": 1})]
}

fn not_a_list() -> Vec<Value> {
    vec![Value::Null, json!("zzz-not-a-list"), json!(17), json!(true), json!({"unexpected": 1})]
}

/// One valid element per list key: the mixed array around it must keep exactly these.
fn good_element(key: &str) -> Value {
    match key {
        "plaintext_consent" => json!({"machine_id":"m","account":"a","choice":"allowed"}),
        "server_key_pins" => json!({"machine_id":"m","pin":"sha256//x"}),
        "native_audio_refused" => json!({"firmware":"kisscurl-koli","codec":"vorbis","channels":2}),
        "last_library" => json!({"user":"u1","libs":[{"kind":"movie","machine_id":"m","key":1}]}),
        "library_sorts" => json!({"user":"u1","libs":[{"machine_id":"m","key":1,"sort":"plays"}]}),
        "subtitle_offsets" => json!({"user":"u1","items":[{"machine_id":"m","rating_key":"42","offset_ms":1500}]}),
        "track_memory" => json!({"user":"u1","account":"a","items":[{"machine_id":"m","show_rk":"42",
            "audio":{"lang":"eng","tag":"","commentary":false,"ad":false,"codec":"eac3","channels":6,"title":""}}]}),
        "profile_prefs" => json!({"user":"u1","account":"a","prefs":{"subtitle_size":"large"}}),
        other => panic!("{other}: a SoftVec row needs a valid element in `good_element`"),
    }
}

/// **T-parse.** Each descriptor's `PARSE` is the policy its field really has: the garbage that
/// policy absorbs reads as the fallback, in the struct on its own AND inside both of its on-disk
/// hosts (the legacy `Session` and the canonical preference record).
#[test]
fn t_parse_each_row_keeps_its_declared_policy() {
    let fixtures = garbage_fixtures();
    let fixture_keys: BTreeSet<&str> = fixtures.iter().map(|(k, _)| *k).collect();
    assert_eq!(fixture_keys, keys(), "a descriptor needs a garbage fixture, and a fixture needs a descriptor");
    // An ABSENT key is the baseline: `trailer_autoplay` reads on there although the derived
    // `Preferences::default()` has it off, so the baseline is a parse, never `default()`.
    let baseline: Preferences = serde_json::from_value(json!({})).unwrap();
    assert!(baseline.trailer_autoplay, "an absent trailer_autoplay key reads on");
    assert_ne!(baseline, Preferences::default(), "the one absent-key read that is not the derived default");
    for d in DESCRIPTORS {
        let (_, garbage) = fixtures.iter().find(|(k, _)| *k == d.key).unwrap();
        for value in garbage {
            let object = json!({ d.key: value });
            let alone: Preferences = serde_json::from_value(object.clone())
                .unwrap_or_else(|e| panic!("{}: {value} must not fail the struct: {e}", d.key));
            let legacy: Session = serde_json::from_value(object.clone())
                .unwrap_or_else(|e| panic!("{}: {value} must not fail the session: {e}", d.key));
            let canonical: CanonicalSessionPreferences = serde_json::from_value(object)
                .unwrap_or_else(|e| panic!("{}: {value} must not fail the canonical record: {e}", d.key));
            for (host, parsed) in [("struct", &alone), ("legacy", &legacy.prefs), ("canonical", &canonical.prefs)] {
                // Every policy's garbage reads as an absent key, whatever that key's own fallback is:
                // the type's default (`Soft`), `None` (`OptionalSoft`), no elements (`SoftVec`), off
                // for a switch whose product default is off (`BoolOff`) and ON for the one whose
                // product default is on (`BoolOn`, where on is also what an absent key reads).
                assert_eq!(parsed, &baseline, "{} in {host}: {value}", d.key);
                match d.parse {
                    Parse::Soft | Parse::SoftVec => {}
                    Parse::OptionalSoft => {
                        assert_eq!(parsed.playback_quality, None, "{host}: {value}");
                        assert_eq!(parsed.last_hero_blur, None, "{host}: {value}");
                    }
                    Parse::BoolOn => assert!(parsed.trailer_autoplay, "{host}: garbage reads ON: {value}"),
                    Parse::BoolOff => assert!(!parsed.auto_sign_in, "{host}: garbage reads OFF: {value}"),
                }
            }
        }
        if d.parse == Parse::SoftVec {
            // SoftVec keeps the good elements of a mixed array and drops the bad ones around them.
            let good = good_element(d.key);
            let mixed = json!([7, good, "x", Value::Null, {"user": 5}, good]);
            let object = json!({ d.key: mixed });
            let alone: Preferences = serde_json::from_value(object.clone()).unwrap();
            let legacy: Session = serde_json::from_value(object.clone()).unwrap();
            let canonical: CanonicalSessionPreferences = serde_json::from_value(object).unwrap();
            let want = serde_json::to_value(serde_json::from_value::<Preferences>(json!({ d.key: [good, good] })).unwrap()).unwrap();
            for (host, parsed) in [("struct", &alone), ("legacy", &legacy.prefs), ("canonical", &canonical.prefs)] {
                assert_eq!(serde_json::to_value(parsed).unwrap(), want, "{} in {host}: both good elements, no bad one", d.key);
                assert_ne!(parsed, &baseline, "{} in {host}: the good elements are kept", d.key);
            }
        }
    }
}

/// The lists that are NOT rows (`home_pins`, `recent_searches`) keep the per-element soft parse
/// they were written with; A5 did not touch them.
#[test]
fn the_lists_outside_the_preferences_keep_their_soft_parse() {
    let session: Session = serde_json::from_value(json!({
        "home_pins": [{"user":"u1","asked":true,"on":[{"machine_id":"m1","key":2}],"off":[]}, 3, "x"],
        "recent_searches": [{"user":"u1","terms":["a"]}, 3, "x"],
    }))
    .unwrap();
    assert_eq!(session.home_pins.len(), 1, "SoftVec keeps the good element and drops the bad ones");
    assert_eq!(session.recent_searches.len(), 1);
}

/// A flattened struct takes its keys first: the modelled ones are in `Preferences`, never in the unknown-key
/// map beside them, and a key nobody models still is.
#[test]
fn modelled_keys_never_reach_extensions_and_future_keys_still_do() {
    let canonical: CanonicalSessionPreferences = serde_json::from_value(json!({
        "deck_press": "play", "subtitle_size": "large", "playback_quality": "auto", "a_future_key": {"n": 1},
    }))
    .unwrap();
    assert_eq!(canonical.prefs.deck_press, DeckPress::Play);
    assert_eq!(canonical.prefs.subtitle_size, SubtitleSize::Large);
    assert_eq!(canonical.prefs.playback_quality, Some(PlaybackQuality::Auto));
    assert_eq!(canonical.extensions.keys().map(String::as_str).collect::<Vec<_>>(), ["a_future_key"]);
    let legacy: Session = serde_json::from_value(json!({
        "deck_press": "play", "audio_enhancements": {"boost_dialog": true}, "a_future_key": 2,
    }))
    .unwrap();
    assert_eq!(legacy.prefs.deck_press, DeckPress::Play);
    assert!(legacy.prefs.audio_enhancements.boost_dialog);
    assert!(legacy.extensions.0.contains_key("a_future_key"));
    assert!(!legacy.extensions.0.contains_key("deck_press"));
    assert!(!legacy.extensions.0.contains_key("audio_enhancements"));
}

// ---- Under posters (Settings > Libraries) ------------------------------------------------------

fn under_posters_session(value: Value) -> Session {
    serde_json::from_value(json!({ "under_posters": value })).expect("a session with the key parses")
}

/// Soft parse, omit-at-default, and a field's garbage costing only that field.
#[test]
fn under_posters_is_soft_per_field_and_omitted_at_the_default() {
    for garbage in [Value::Null, json!("zzz"), json!(3), json!(true), json!([1, "x"])] {
        assert_eq!(under_posters_session(garbage.clone()).under_posters(), UnderPosters::default(), "{garbage}");
    }
    // A later build's spelling of `details` keeps the title the viewer chose.
    let kept = under_posters_session(json!({ "title": true, "details": "future_kind" }));
    assert_eq!(kept.under_posters(), UnderPosters { title: true, details: UnderPostersDetails::YearLength });
    // And a bad title keeps the details.
    let kept = under_posters_session(json!({ "title": "yes", "details": "genre" }));
    assert_eq!(kept.under_posters(), UnderPosters { title: false, details: UnderPostersDetails::Genre });
    // The default is not written, so a session predating the field serializes as it did.
    let json = serde_json::to_string(&Session::default()).unwrap();
    assert!(!json.contains("under_posters"), "{json}");
    assert_eq!(UnderPosters::default(), UnderPosters::DEFAULT);
}

#[test]
fn under_posters_round_trips_and_packs_into_one_byte() {
    for details in UnderPostersDetails::LADDER {
        for title in [false, true] {
            let v = UnderPosters { title, details };
            let session = Session::default().with_pref::<UnderPostersPref>(v);
            assert_eq!(serde_json::to_string(&session).unwrap().contains("under_posters"), v != UnderPosters::DEFAULT);
            let back: Session = serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();
            assert_eq!(back.under_posters(), v);
            assert_eq!(UnderPosters::unpack(v.pack()), v, "{v:?}");
        }
    }
    assert_eq!(UnderPosters::DEFAULT.pack(), 0, "the live cell's seed is the default");
    assert_eq!(UnderPosters::unpack(0xFE).details, UnderPostersDetails::YearLength, "out of range reads the default");
    for d in UnderPostersDetails::LADDER {
        assert_eq!(UnderPostersDetails::from_index(d.index()), d);
    }
}
