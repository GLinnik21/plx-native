//! The tie tests of `prefs.rs`: every list of settings held to every other, in BOTH directions, so
//! a field added to one and forgotten in the other fails here instead of drifting.
//!
//! * **T-fields** — `Preferences`' serde field list equals `DESCRIPTORS`' keys.
//! * **T-account** — `DESCRIPTORS`' account-bound keys equal platform's
//!   `ACCOUNT_BOUND_PREFERENCES` (minus the rows that do not exist yet).
//! * **T-parse** — each descriptor's declared parse policy is the one its field really has.
//!
//! T-live and T-settings sit in media (`route/prefs_tests.rs`), where the lists they hold to these
//! descriptors are declared.
use super::prefs::{Descriptor, Parse, Scope, Setting, SettingsWrite, DESCRIPTORS};
use super::prefs::{AudioEnhancementsPref, DeckPressPref, DirectPlayModePref, NextEpisodeModePref,
    QualityPref, SkipIntervalPref, SubtitlePositionPref, SubtitleSizePref, SubtitleTonePref};
use super::{
    CanonicalSessionPreferences, DeckPress, DirectPlayMode, NextEpisodeMode, PlaybackQuality,
    Preferences, Session, SkipInterval, SubtitlePosition, SubtitleSize, SubtitleTone,
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
    check_row::<NextEpisodeModePref>(NextEpisodeMode::Off);
    check_row::<SkipIntervalPref>(SkipInterval::Seconds30);
    check_row::<DeckPressPref>(DeckPress::Play);
    check_row::<AudioEnhancementsPref>(crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false });
    // The table lists exactly these nine: a tenth row needs its line here too.
    assert_eq!(DESCRIPTORS.len(), 9);
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
/// rows are account-bound. The two lists are held to each other both ways. The rows that are not
/// `Preferences` fields yet (`NOT_YET_ROWS`) are the only exception, and the last row to land
/// deletes the list.
#[test]
fn t_account_the_bound_rows_are_the_platform_bound_keys() {
    const NOT_YET_ROWS: &[&str] = &["server_key_pins", "plaintext_consent"];
    let platform: BTreeSet<&str> = plx_platform::storage::state::ACCOUNT_BOUND_PREFERENCES.iter().copied().collect();
    for key in NOT_YET_ROWS {
        assert!(platform.contains(key), "NOT_YET_ROWS names {key}, which platform no longer clears");
        assert!(!keys().contains(key), "{key} has a row now: delete it from NOT_YET_ROWS");
    }
    let expected: BTreeSet<&str> = platform.iter().copied().filter(|k| !NOT_YET_ROWS.contains(k)).collect();
    let declared: BTreeSet<&str> = DESCRIPTORS.iter().filter(|d| d.account_bound).map(|d| d.key).collect();
    assert_eq!(declared, expected, "account-bound rows and platform's ACCOUNT_BOUND_PREFERENCES drifted");
    for d in DESCRIPTORS {
        match d.scope {
            Scope::Install => assert!(!d.account_bound, "{}: an Install row is never account-bound", d.key),
            Scope::Account => assert!(d.account_bound, "{}: an Account row is always account-bound", d.key),
            _ => {}
        }
    }
}

/// The facts a row declares that a later PR reads (`LIVE` by media's T-live, `SETTINGS` by the
/// Settings dispatcher's T-settings), pinned here per key so a change is a visible edit.
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
        ("subtitle_size", Some(Optimistic)),
        ("subtitle_position", Some(Optimistic)),
        ("subtitle_tone", None),
        ("audio_enhancements", None),
    ] {
        let d = row(key);
        assert_eq!(d.settings, settings, "{key}");
        assert!(d.live, "{key}: every one of the nine has a live cell in media");
        assert_eq!(d.scope, Scope::Install, "{key}");
        assert!(!d.account_bound, "{key}");
    }
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
        ("next_episode_mode", generic()),
        ("skip_interval", generic()),
        ("deck_press", generic()),
        ("audio_enhancements", generic()),
    ]
}

/// **T-parse.** Each descriptor's `PARSE` is the policy its field really has: the garbage that
/// policy absorbs reads as the fallback, in the struct on its own AND inside both of its on-disk
/// hosts (the legacy `Session` and the canonical preference record).
#[test]
fn t_parse_each_row_keeps_its_declared_policy() {
    let fixtures = garbage_fixtures();
    let fixture_keys: BTreeSet<&str> = fixtures.iter().map(|(k, _)| *k).collect();
    assert_eq!(fixture_keys, keys(), "a descriptor needs a garbage fixture, and a fixture needs a descriptor");
    let default_prefs = Preferences::default();
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
                match d.parse {
                    // garbage reads as the type's default, i.e. as an absent key
                    Parse::Soft => assert_eq!(parsed, &default_prefs, "{} in {host}: {value}", d.key),
                    // garbage reads as None, i.e. as an absent key
                    Parse::OptionalSoft => {
                        assert_eq!(parsed, &default_prefs, "{} in {host}: {value}", d.key);
                        assert_eq!(parsed.playback_quality, None, "{host}: {value}");
                    }
                    Parse::SoftVec | Parse::BoolOn | Parse::BoolOff => {
                        panic!("{}: no Preferences row has this policy yet; add its fixture with the row", d.key)
                    }
                }
            }
        }
    }
}

/// The three policies no `Preferences` row uses yet are still the ones the rest of `Session` has
/// (S1): the per-element list, the switch that defaults on, the switch that defaults off. They
/// join `t_parse` as rows when their fields do; until then this pins that A1 did not unify them.
#[test]
fn the_other_parse_policies_survive_the_move() {
    let session: Session = serde_json::from_value(json!({
        "trailer_autoplay": "garbage",
        "auto_sign_in": "garbage",
        "home_pins": [{"user":"u1","asked":true,"on":[{"machine_id":"m1","key":2}],"off":[]}, 3, "x"],
    }))
    .unwrap();
    assert!(session.trailer_autoplay, "BoolOn: garbage reads true");
    assert!(!session.auto_sign_in, "BoolOff: garbage reads false");
    assert_eq!(session.home_pins.len(), 1, "SoftVec keeps the good element and drops the bad ones");
}

/// A flattened struct takes its keys first: the nine are in `Preferences`, never in the unknown-key
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
