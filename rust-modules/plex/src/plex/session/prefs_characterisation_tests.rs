//! **Characterisation of the session file's preference JSON and its parsing, on `main` as of
//! `cab5d041`.** These tests pin what the code does today; none of them is a wish. Every expected
//! value below was read from `main` by running it. A change that moves one of them is a change to
//! the on-disk contract an older build must still read, and must say so (track-memory plan, A0).
//!
//! Scope of this file: the legacy-file JSON and the canonical public JSON (item 1), the per-key
//! parse policies (item 2), the #582 non-resurrection of preferences set back to their default
//! and the unknown-key rule (item 3), the per-profile offset record pruning (item 4) and what the
//! file backend keeps at sign-out (item 15).
use super::*;
use plx_platform::storage::state::PublicPayload;
use plx_platform::storage::{client, state, wire::*};
use serde_json::json;

// ---- fixtures ----------------------------------------------------------------------------------

/// A session with EVERY persisted preference away from its default, written as the literal JSON a
/// file holds so that the parse is part of what is graded.
fn all_non_default_json() -> Value {
    json!({
        "language":"be",
        "client_id":"cid","account_token":"acct",
        "server":{"name":"S","machine_id":"m1","address":"10.0.0.2","port":32400,"token":"t"},
        "user":{"id":1,"uuid":"u1","title":"T","thumb":"","token":"ut"},
        "home_pins":[{"user":"u1","asked":true,"on":[{"machine_id":"m1","key":2}],"off":[]}],
        "recent_searches":[{"user":"u1","terms":["a","b"]}],
        "last_library":[{"user":"u1","libs":[{"kind":"movie","machine_id":"m1","key":3}]}],
        "library_sorts":[{"user":"u1","libs":[{"machine_id":"m1","key":3,"sort":"plays","desc":true,
            "unwatched":true,"genre":"g","listing":"seasons","listing_sort":"ls","listing_desc":true}]}],
        "subtitle_offsets":[{"user":"u1","items":[{"machine_id":"m1","rating_key":"9","offset_ms":-250}]}],
        "playback_quality":"720p_4_mbps","direct_play_mode":"forced","auto_sign_in":true,
        "trailer_autoplay":false,"subtitle_tone":"grey_55","subtitle_size":"large",
        "subtitle_position":"high","next_episode_mode":"off","skip_interval":"30s",
        "deck_press":"play",
        "last_hero_blur":[[0.5,0.25,0.125],[0.5,0.25,0.125],[0.5,0.25,0.125],[0.5,0.25,0.125]],
        "plaintext_consent":[{"machine_id":"m1","account":"a","choice":"allowed"}],
        "server_key_pins":[{"machine_id":"m1","pin":PIN}],
        "audio_enhancements":{"boost_dialog":true,"normalize_loudness":true}
    })
}

/// A syntactically valid SPKI pin (`sha256//` + 44 base64 characters ending in `=`).
const PIN: &str = "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

fn all_non_default() -> Session {
    serde_json::from_value(all_non_default_json()).unwrap()
}

/// The legacy file's JSON: `serde_json::to_value(&Session)`.
fn legacy(session: &Session) -> Value {
    serde_json::to_value(session).unwrap()
}

/// The canonical public payload as the DB8 record holds it, as a `Value`.
fn public(session: &Session) -> Value {
    serde_json::to_value(split_public(session).unwrap()).unwrap()
}

/// Parse a legacy file whose body is exactly `body`.
fn legacy_from(body: Value) -> Session {
    serde_json::from_value(body).expect("a legacy file with soft keys always parses")
}

/// Parse a canonical record whose `public.preferences` is exactly `preferences`, through the same
/// door `join_canonical` and the locked-bundle snapshot use.
fn canonical_from(preferences: Value) -> Session {
    public_session(&PublicPayload { preferences, ..Default::default() })
}

/// What a key looks like in the legacy JSON of `session`, `None` when the key is not written.
fn legacy_key(session: &Session, key: &str) -> Option<Value> {
    legacy(session).get(key).cloned()
}

/// What a key looks like in the canonical `public.preferences` of `session`.
fn canonical_key(session: &Session, key: &str) -> Option<Value> {
    public(session)["preferences"].get(key).cloned()
}

// ---- item 1: the JSON ----------------------------------------------------------------------------

/// A default session, legacy file. NOTE the derived `Session::default()` has
/// `trailer_autoplay: false` (the plain bool default) although the product default is on; the
/// next test pins both. NOTE also `"playback_quality": null` (no skip attribute), the tone written
/// at its default, and `plaintext_consent: []` written while empty (no skip attribute).
#[test]
fn legacy_json_of_a_default_session() {
    assert_eq!(
        legacy(&Session::default()),
        json!({
            "client_id":"","account_token":"",
            "server":{"name":"","machine_id":"","address":"","port":0,"token":"","tier":null,"origin":""},
            "user":{"id":0,"uuid":"","title":"","thumb":"","token":""},
            "home_users":[],"sources":[],"home_pins":[],"recent_searches":[],"last_library":[],
            "profiles":[],"playback_quality":null,"direct_play_mode":"auto","auto_sign_in":false,
            "trailer_autoplay":false,"subtitle_tone":"white","last_hero_blur":null,
            "plaintext_consent":[]
        })
    );
}

#[test]
fn canonical_public_json_of_a_default_session() {
    assert_eq!(
        public(&Session::default()),
        json!({
            "preferences":{"auto_sign_in":false,"direct_play_mode":"auto","language":"system",
                "last_hero_blur":null,"last_library":[],"playback_quality":null,
                "subtitle_tone":"white","trailer_autoplay":false},
            "client_id":null,"profile":null,"pins":[],"recents":[],"consent":null,"scopes":null,
            "ids":null,"account_extensions":null
        })
    );
}

#[test]
fn legacy_json_of_an_all_non_default_session() {
    assert_eq!(
        legacy(&all_non_default()),
        json!({
            "language":"be","client_id":"cid","account_token":"acct",
            "server":{"name":"S","machine_id":"m1","address":"10.0.0.2","port":32400,"token":"t",
                "tier":null,"origin":""},
            "user":{"id":1,"uuid":"u1","title":"T","thumb":"","token":"ut"},
            "home_users":[],"sources":[],
            "home_pins":[{"user":"u1","asked":true,"on":[{"machine_id":"m1","key":2}],"off":[]}],
            "recent_searches":[{"user":"u1","terms":["a","b"]}],
            "last_library":[{"user":"u1","libs":[{"kind":"movie","machine_id":"m1","key":3}]}],
            "library_sorts":[{"user":"u1","libs":[{"machine_id":"m1","key":3,"sort":"plays",
                "desc":true,"unwatched":true,"genre":"g","listing":"seasons","listing_sort":"ls",
                "listing_desc":true}]}],
            "subtitle_offsets":[{"user":"u1","items":[{"machine_id":"m1","rating_key":"9","offset_ms":-250}]}],
            "profiles":[],"playback_quality":"720p_4_mbps","direct_play_mode":"forced",
            "auto_sign_in":true,"trailer_autoplay":false,"subtitle_tone":"grey_55",
            "subtitle_size":"large","subtitle_position":"high","next_episode_mode":"off",
            "skip_interval":"30s","deck_press":"play",
            "last_hero_blur":[[0.5,0.25,0.125],[0.5,0.25,0.125],[0.5,0.25,0.125],[0.5,0.25,0.125]],
            "plaintext_consent":[{"machine_id":"m1","account":"a","choice":"allowed"}],
            "server_key_pins":[{"machine_id":"m1","pin":PIN}],
            "audio_enhancements":{"boost_dialog":true,"normalize_loudness":true}
        })
    );
}

#[test]
fn canonical_public_json_of_an_all_non_default_session() {
    assert_eq!(
        public(&all_non_default()),
        json!({
            "preferences":{
                "audio_enhancements":{"boost_dialog":true,"normalize_loudness":true},
                "auto_sign_in":true,"deck_press":"play","direct_play_mode":"forced","language":"be",
                "last_hero_blur":[[0.5,0.25,0.125],[0.5,0.25,0.125],[0.5,0.25,0.125],[0.5,0.25,0.125]],
                "last_library":[{"libs":[{"key":3,"kind":"movie","machine_id":"m1"}],"user":"u1"}],
                "library_sorts":[{"libs":[{"desc":true,"genre":"g","key":3,"listing":"seasons",
                    "listing_desc":true,"listing_sort":"ls","machine_id":"m1","sort":"plays",
                    "unwatched":true}],"user":"u1"}],
                "next_episode_mode":"off",
                "plaintext_consent":[{"account":"a","choice":"allowed","machine_id":"m1"}],
                "playback_quality":"720p_4_mbps",
                "server_key_pins":[{"machine_id":"m1","pin":PIN}],
                "skip_interval":"30s",
                "subtitle_offsets":[{"items":[{"machine_id":"m1","offset_ms":-250,"rating_key":"9"}],"user":"u1"}],
                "subtitle_position":"high","subtitle_size":"large","subtitle_tone":"grey_55",
                "trailer_autoplay":false
            },
            "client_id":"cid","profile":null,
            "pins":[{"asked":true,"off":[],"on":[{"key":2,"machine_id":"m1"}],"user":"u1"}],
            "recents":[{"terms":["a","b"],"user":"u1"}],
            "consent":null,"scopes":null,"ids":null,"account_extensions":null
        })
    );
}

/// `trailer_autoplay` is the one key whose default is not the derived one. Four doors, and they do
/// NOT agree: the derived `Session::default()` is off, a file without the key is on, the manual
/// `CanonicalSessionPreferences::default()` (the whole-value fallback) is on, and an explicit
/// `true` is written out. A refactor that "unifies" these changes someone's boot.
#[test]
fn trailer_autoplay_has_a_derived_off_default_and_an_absent_key_that_reads_on() {
    assert!(!Session::default().trailer_autoplay, "the derived Default is off");
    assert!(legacy_from(json!({})).trailer_autoplay, "a file without the key reads on");
    assert!(CanonicalSessionPreferences::default().trailer_autoplay, "the manual Default is on");
    assert!(canonical_from(json!({})).trailer_autoplay, "an empty preferences object reads on");
    assert!(canonical_from(Value::Null).trailer_autoplay, "absent preferences fall back to the manual Default");

    let on = legacy_from(json!({"trailer_autoplay":true}));
    assert_eq!(legacy_key(&on, "trailer_autoplay"), Some(json!(true)), "an on value is written, not skipped");
    assert_eq!(canonical_key(&on, "trailer_autoplay"), Some(json!(true)));
}

// ---- item 2: per-key parse fixtures ---------------------------------------------------------------

/// One scalar key: what it reads as, once serialised again, for garbage, `null` and a spelling a
/// future build might write. `None` means "the key is skipped at its default".
struct Scalar {
    key: &'static str,
    future: Value,
    fallback: Option<Value>,
}

fn scalars() -> Vec<Scalar> {
    vec![
        Scalar { key: "playback_quality", future: json!("8k_ultra"), fallback: Some(Value::Null) },
        Scalar { key: "direct_play_mode", future: json!("sometimes"), fallback: Some(json!("auto")) },
        Scalar { key: "auto_sign_in", future: json!("yes"), fallback: Some(json!(false)) },
        Scalar { key: "trailer_autoplay", future: json!("yes"), fallback: Some(json!(true)) },
        Scalar { key: "subtitle_tone", future: json!("grey_10"), fallback: Some(json!("white")) },
        Scalar { key: "subtitle_size", future: json!("huge"), fallback: None },
        Scalar { key: "subtitle_position", future: json!("top"), fallback: None },
        Scalar { key: "next_episode_mode", future: json!("sometimes"), fallback: None },
        Scalar { key: "skip_interval", future: json!("45s"), fallback: None },
        Scalar { key: "deck_press", future: json!("hold"), fallback: None },
        Scalar { key: "audio_enhancements", future: json!({"boost_dialog":"loud"}), fallback: None },
        Scalar { key: "last_hero_blur", future: json!({"future":"shape"}), fallback: Some(Value::Null) },
    ]
}

/// Scalar garbage, `null` and a future spelling read today's fallback, in BOTH on-disk forms.
/// `trailer_autoplay` garbage reads ON; `auto_sign_in` garbage reads OFF; quality garbage reads
/// `None`. The keys that skip at their default are absent from the re-serialised JSON.
#[test]
fn scalar_garbage_null_and_future_spellings_read_todays_fallback() {
    for scalar in scalars() {
        let garbage = [json!("garbage"), json!(42), Value::Null, json!([]), json!({"x":1}), scalar.future.clone()];
        for value in garbage {
            let legacy_session = legacy_from(json!({ scalar.key: value.clone() }));
            assert_eq!(legacy_key(&legacy_session, scalar.key), scalar.fallback,
                "legacy: {} = {value}", scalar.key);
            let canonical_session = canonical_from(json!({ scalar.key: value.clone() }));
            assert_eq!(canonical_key(&canonical_session, scalar.key), scalar.fallback,
                "canonical: {} = {value}", scalar.key);
        }
    }
}

/// The two explicit corners the prose names: a bad `trailer_autoplay` stays on, a bad
/// `auto_sign_in` stays off, bad quality is `None`; and a valid value still wins.
#[test]
fn the_three_named_garbage_corners_and_a_valid_value_that_still_wins() {
    let s = legacy_from(json!({"trailer_autoplay":"garbage","auto_sign_in":"garbage","playback_quality":"garbage"}));
    assert!(s.trailer_autoplay);
    assert!(!s.auto_sign_in);
    assert_eq!(s.prefs.playback_quality, None);
    let s = legacy_from(json!({"trailer_autoplay":false,"auto_sign_in":true,"playback_quality":"auto"}));
    assert!(!s.trailer_autoplay);
    assert!(s.auto_sign_in);
    assert_eq!(s.prefs.playback_quality, Some(PlaybackQuality::Auto));
}

/// One list key: a good element, a bad one, and the key's slot in each form.
struct ListKey {
    key: &'static str,
    good: Value,
    bad: Vec<Value>,
}

fn list_keys() -> Vec<ListKey> {
    vec![
        ListKey {
            key: "subtitle_offsets",
            good: json!({"user":"u1","items":[{"machine_id":"m","rating_key":"1","offset_ms":100}]}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"user":5})],
        },
        ListKey {
            key: "last_library",
            good: json!({"user":"u1","libs":[{"kind":"movie","machine_id":"m","key":1}]}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"user":5})],
        },
        ListKey {
            key: "library_sorts",
            good: json!({"user":"u1","libs":[{"machine_id":"m","key":1,"sort":"plays"}]}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"user":5})],
        },
        ListKey {
            key: "plaintext_consent",
            good: json!({"machine_id":"m","account":"a","choice":"allowed"}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"choice":"allowed"})],
        },
        ListKey {
            key: "server_key_pins",
            good: json!({"machine_id":"m","pin":PIN}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"machine_id":"m"})],
        },
        ListKey {
            key: "home_pins",
            good: json!({"user":"u1","asked":true,"on":[{"machine_id":"m","key":1}],"off":[]}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"user":5})],
        },
        ListKey {
            key: "recent_searches",
            good: json!({"user":"u1","terms":["a"]}),
            bad: vec![json!(7), json!("x"), Value::Null, json!({"user":5})],
        },
    ]
}

/// The mixed array: good, bad, good, bad... A bad element costs itself and nothing else.
fn mixed(list: &ListKey) -> Value {
    let mut out = vec![list.good.clone()];
    for bad in &list.bad {
        out.push(bad.clone());
        out.push(list.good.clone());
    }
    Value::Array(out)
}

/// A mixed good/bad array keeps the good elements, per list key, on the legacy file. The count of
/// good elements is the number of `good` entries in `mixed`.
#[test]
fn a_mixed_list_in_the_legacy_file_keeps_the_good_elements() {
    for list in list_keys() {
        let kept = list.bad.len() + 1;
        let session = legacy_from(json!({ list.key: mixed(&list) }));
        let written = legacy_key(&session, list.key).expect(list.key);
        let written = written.as_array().expect(list.key);
        assert_eq!(written.len(), kept, "{}: {written:?}", list.key);
        assert!(written.iter().all(|e| e == &written[0]), "{}: only the good element survives, {kept} times", list.key);
    }
}

/// On the canonical record the six `preferences` lists are soft per element; the two lists that ride
/// in their own `pins` / `recents` slots are NOT: one bad element costs the whole slot. That is
/// today's behaviour (`serde_json::from_value(..).unwrap_or_default()`), pinned as found.
#[test]
fn a_mixed_list_in_the_canonical_record_keeps_good_elements_except_in_the_pins_and_recents_slots() {
    for list in list_keys() {
        let kept = list.bad.len() + 1;
        let array = mixed(&list);
        match list.key {
            "home_pins" => {
                let s = public_session(&PublicPayload { pins: array, ..Default::default() });
                assert_eq!(s.home_pins.len(), 0, "pins slot is parsed whole");
            }
            "recent_searches" => {
                let s = public_session(&PublicPayload { recents: array, ..Default::default() });
                assert_eq!(s.recent_searches.len(), 0, "recents slot is parsed whole");
            }
            key => {
                let session = canonical_from(json!({ key: array }));
                let written = canonical_key(&session, key).expect(key);
                let written = written.as_array().expect(key);
                assert_eq!(written.len(), kept, "{key}");
                assert!(written.iter().all(|e| e == &written[0]), "{key}: only the good element survives");
            }
        }
    }
}

/// A list that is not a list (a string, an object, `null`) is no list, in both forms.
#[test]
fn a_list_key_that_is_not_a_list_reads_empty() {
    for list in list_keys() {
        for not_a_list in [json!("x"), json!({"a":1}), Value::Null, json!(3)] {
            let s = legacy_from(json!({ list.key: not_a_list.clone() }));
            let written = legacy_key(&s, list.key);
            assert!(
                written.as_ref().is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)),
                "{} = {not_a_list}: {written:?}", list.key
            );
        }
    }
}

/// The nested lists. `items` and `libs` of the offset and view records are soft per element; a
/// `LastLibrary` record's `libs` is not, so one bad library costs that profile's record.
#[test]
fn nested_lists_are_soft_for_offsets_and_views_and_strict_for_last_library() {
    let s = legacy_from(json!({
        "subtitle_offsets":[{"user":"u1","items":[
            {"machine_id":"m","rating_key":"1","offset_ms":100}, 7,
            {"machine_id":"m","rating_key":"2","offset_ms":"x"},
            {"machine_id":"m","rating_key":"3","offset_ms":-5}]}],
        "library_sorts":[{"user":"u1","libs":[{"machine_id":"m","key":1,"sort":"plays"}, "x", {"key":"y"}]}],
        "last_library":[{"user":"u1","libs":[{"kind":"movie","machine_id":"m","key":1}, 9]},
                        {"user":"u2","libs":[{"kind":"show","machine_id":"m","key":2}]}]
    }));
    assert_eq!(s.subtitle_offsets.len(), 1);
    assert_eq!(s.subtitle_offsets[0].items.len(), 2, "two of the four items survive");
    assert_eq!(s.library_views[0].libs.len(), 1, "one of the three views survives");
    assert_eq!(s.last_library.len(), 1, "the profile with a bad library is dropped whole");
    assert_eq!(s.last_library[0].user, "u2");
}

/// `language` is not soft-parsed with a custom function, but a bad spelling still reads System.
#[test]
fn a_bad_language_reads_system_in_the_legacy_file() {
    for bad in [json!("future"), json!(42), Value::Null] {
        let s = legacy_from(json!({"language": bad}));
        assert_eq!(s.language, plx_platform::i18n::Preference::System);
    }
}

/// A canonical `preferences` that is not an object at all costs ALL the preferences (the manual
/// `Default` is the fallback), never the credentials. One bad key inside an object costs only
/// itself.
#[test]
fn a_canonical_preferences_value_that_is_not_an_object_falls_back_to_the_manual_default() {
    for not_an_object in [json!("x"), json!(3), json!([]), Value::Null] {
        let s = canonical_from(not_an_object.clone());
        assert!(s.trailer_autoplay, "{not_an_object}");
        assert_eq!(s.prefs.direct_play_mode, DirectPlayMode::Auto);
        assert_eq!(s.prefs.playback_quality, None);
    }
    let s = canonical_from(json!({"subtitle_size":"huge","auto_sign_in":true,"skip_interval":"60s"}));
    assert!(s.auto_sign_in, "a sibling of a bad key is kept");
    assert_eq!(s.prefs.skip_interval, SkipInterval::Seconds60);
    assert_eq!(s.prefs.subtitle_size, SubtitleSize::Medium);
}

// ---- item 3: unknown keys, and #582 -----------------------------------------------------------------

/// In the legacy file a future top-level key is kept in `extensions` and written back; no key this
/// build models ever lands there.
#[test]
fn legacy_file_keeps_a_future_key_and_never_routes_a_modelled_key_into_extensions() {
    let mut body = all_non_default_json();
    body["future_pref"] = json!({"nested":[1,2]});
    let s = legacy_from(body);
    let keys: Vec<&String> = s.extensions.0.keys().collect();
    assert_eq!(keys, ["future_pref"], "only the future key lands in extensions");
    assert_eq!(legacy_key(&s, "future_pref"), Some(json!({"nested":[1,2]})), "and it is written back");

    let modelled = all_non_default();
    assert!(modelled.extensions.0.is_empty(), "{:?}", modelled.extensions.0.keys().collect::<Vec<_>>());
}

/// The canonical preferences struct routes exactly the unknown keys to its `extensions` map: that
/// map is what `preserve_unknown_preferences` consults, so a modelled key landing in it would be
/// copied back over a deliberate reset.
#[test]
fn canonical_preferences_route_only_unknown_keys_to_extensions() {
    let mut stored = public(&all_non_default())["preferences"].clone();
    let parsed: CanonicalSessionPreferences = serde_json::from_value(stored.clone()).unwrap();
    assert!(parsed.extensions.is_empty(), "{:?}", parsed.extensions.keys().collect::<Vec<_>>());
    stored["future_pref"] = json!("keep");
    let parsed: CanonicalSessionPreferences = serde_json::from_value(stored).unwrap();
    assert_eq!(parsed.extensions.keys().collect::<Vec<_>>(), ["future_pref"]);
}

/// `split_public` writes no extensions of its own: a future preference does not survive a trip
/// through the `Session` domain object; the adapter re-merges it from the stored record.
#[test]
fn a_future_canonical_preference_does_not_ride_through_the_session_object() {
    let mut stored = public(&all_non_default())["preferences"].clone();
    stored["future_pref"] = json!("keep");
    let s = canonical_from(stored);
    assert_eq!(canonical_key(&s, "future_pref"), None);
}

/// The helper-backed store, the way `migration_tests` builds it. A private copy: that module's
/// `Db8` is not visible here, and this lane touches no other file.
#[derive(Default)]
struct Db8 {
    record: Option<Value>,
}
impl plx_platform::storage::keymanager::Rpc for Db8 {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        match uri {
            "luna://com.palm.db/get" => {
                Ok(json!({"returnValue":true,"results":self.record.iter().cloned().collect::<Vec<_>>()}))
            }
            "luna://com.palm.db/put" => {
                let mut next = payload["objects"][0].clone();
                if let Some(current) = &self.record {
                    assert_eq!(next["_rev"], current["_rev"], "real coordinator must CAS the loaded revision");
                }
                next["_rev"] = json!(1 + self.record.as_ref().map_or(0, |r| r["_rev"].as_u64().unwrap_or(0)));
                let rev = next["_rev"].clone();
                self.record = Some(next);
                Ok(json!({"returnValue":true,"results":[{"id":"plxstate.stable","rev":rev}]}))
            }
            _ => panic!("the ACL-tier scenario must not invoke a Keymanager RPC"),
        }
    }
}

type Store = plx_platform::storage::backend::Backend<Db8>;

fn store() -> Store {
    plx_platform::storage::backend::Backend::new(
        Db8::default(), state::Flavor::Stable, "com.beb.plxnative.storage".into())
}

fn commit(store: &mut Store, session: &Session, authority: SaveAuthority) {
    let mut transport = |request: Request| Ok(store.dispatch(request));
    assert!(matches!(
        persistence::commit_session_with(session, false, authority, 4, &mut transport),
        persistence::CanonicalCommit::Durable { .. }));
}

fn stored_preferences(store: &mut Store) -> Value {
    let mut transport = |request: Request| Ok(store.dispatch(request));
    let client::Load::Present(snapshot) = client::load_with(&mut transport).unwrap() else {
        panic!("helper snapshot")
    };
    snapshot.state.public.preferences
}

fn opened(store: &mut Store) -> Session {
    let mut transport = |request: Request| Ok(store.dispatch(request));
    let persistence::CanonicalRead::Opened { session, .. } = persistence::load_helper_with(&mut transport) else {
        panic!("the session opens")
    };
    session
}

/// Put a key this build does not model into the stored public preferences, the way a newer build's
/// record would carry one.
fn seed_future_preference(store: &mut Store) {
    let record = store.rpc.record.as_mut().expect("a record to seed");
    let mut state: Value = serde_json::from_str(record["state"].as_str().unwrap()).unwrap();
    state["public"]["preferences"]["future_pref"] = json!("keep");
    record["state"] = json!(state.to_string());
}

/// One row that is skipped at its default spelling.
struct SkipRow {
    key: &'static str,
    set: fn(&mut Session),
    reset: fn(&mut Session),
}

fn skip_rows() -> Vec<SkipRow> {
    vec![
        SkipRow { key: "subtitle_size", set: |s| s.prefs.subtitle_size = SubtitleSize::Large,
            reset: |s| s.prefs.subtitle_size = SubtitleSize::default() },
        SkipRow { key: "subtitle_position", set: |s| s.prefs.subtitle_position = SubtitlePosition::High,
            reset: |s| s.prefs.subtitle_position = SubtitlePosition::default() },
        SkipRow { key: "next_episode_mode", set: |s| s.prefs.next_episode_mode = NextEpisodeMode::Off,
            reset: |s| s.prefs.next_episode_mode = NextEpisodeMode::default() },
        SkipRow { key: "skip_interval", set: |s| s.prefs.skip_interval = SkipInterval::Seconds30,
            reset: |s| s.prefs.skip_interval = SkipInterval::default() },
        SkipRow { key: "deck_press", set: |s| s.prefs.deck_press = DeckPress::Play,
            reset: |s| s.prefs.deck_press = DeckPress::default() },
        SkipRow { key: "audio_enhancements",
            set: |s| s.prefs.audio_enhancements = crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: true },
            reset: |s| s.prefs.audio_enhancements = crate::plex::AudioEnhancements::NONE },
        SkipRow { key: "subtitle_offsets",
            set: |s| s.set_subtitle_offset_for("", "m", "1", Some(500)),
            reset: |s| s.set_subtitle_offset_for("", "m", "1", None) },
        SkipRow { key: "library_sorts",
            set: |s| s.library_views = vec![LibraryViews { user: String::new(),
                libs: vec![LibraryView { machine_id: "m".into(), key: 1, sort: "plays".into(), ..Default::default() }],
                extensions: Default::default() }],
            reset: |s| s.library_views.clear() },
        SkipRow { key: "server_key_pins",
            set: |s| s.server_key_pins = vec![ServerKeyPin { machine_id: "m".into(), pin: PIN.into(), extensions: Default::default() }],
            reset: |s| s.server_key_pins.clear() },
        SkipRow { key: "plaintext_consent",
            set: |s| s.plaintext_consent = vec![PlaintextConsent { machine_id: "m".into(), account: "a".into(),
                choice: PlaintextChoice::Allowed, extensions: Default::default() }],
            reset: |s| s.plaintext_consent.clear() },
    ]
}

fn signed_in_session() -> Session {
    Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        server: ServerRef { machine_id: "machine".into(), address: "10.0.0.2".into(), port: 32400,
            token: "tok".into(), ..Default::default() },
        user: UserRef { uuid: "owner".into(), token: "ut".into(), ..Default::default() },
        ..Default::default()
    }
}

/// **Issue #582, for every row that is skipped at its default.** Through the real helper backend
/// and the production coordinator: the row is stored non-default, a public-only write sets it back
/// to the default, and neither the stored record nor the next boot's session has it back. The
/// future key seeded into the record survives the same write.
#[test]
fn a_skip_at_default_row_set_back_to_default_is_not_resurrected_and_a_future_key_is_kept() {
    for row in skip_rows() {
        let mut store = store();
        let mut session = signed_in_session();
        (row.set)(&mut session);
        commit(&mut store, &session, SaveAuthority::FreshReauthentication);
        assert!(stored_preferences(&mut store).get(row.key).is_some(), "{}: stored non-default", row.key);
        seed_future_preference(&mut store);

        (row.reset)(&mut session);
        commit(&mut store, &session, SaveAuthority::PublicOnly);

        let stored = stored_preferences(&mut store);
        assert!(stored.get(row.key).is_none(), "{}: resurrected in the record: {stored}", row.key);
        assert_eq!(stored["future_pref"], "keep", "{}: the future key is preserved", row.key);
        let reopened = opened(&mut store);
        // `plaintext_consent` has no skip attribute in the legacy file, so it reads `[]` there.
        assert!(
            legacy_key(&reopened, row.key).is_none_or(|v| v == json!([])),
            "{}: resurrected on the next boot: {:?}", row.key, legacy_key(&reopened, row.key)
        );
        // and a modelled key never lands in extensions
        let parsed: CanonicalSessionPreferences = serde_json::from_value(stored).unwrap();
        assert_eq!(parsed.extensions.keys().collect::<Vec<_>>(), ["future_pref"], "{}", row.key);
    }
}

/// The one deliberate exception in `preserve_unknown_preferences`: `playback_quality` is written
/// even at the default (`null`), so a reset to `None` is stored as `null` and read back as `None`.
#[test]
fn playback_quality_is_stored_as_null_when_reset_and_reads_back_none() {
    let mut store = store();
    let mut session = Session {
        prefs: Preferences { playback_quality: Some(PlaybackQuality::P720), ..Preferences::default() },
        ..signed_in_session()
    };
    commit(&mut store, &session, SaveAuthority::FreshReauthentication);
    assert_eq!(stored_preferences(&mut store)["playback_quality"], "720p_4_mbps");
    session.prefs.playback_quality = None;
    commit(&mut store, &session, SaveAuthority::PublicOnly);
    assert_eq!(stored_preferences(&mut store).get("playback_quality"), Some(&Value::Null), "present and null");
    assert_eq!(opened(&mut store).prefs.playback_quality, None);
}

// ---- item 4: forgetting an offset prunes the profile record -----------------------------------------

/// Forgetting a profile's LAST offset removes that profile's record rather than leaving an empty
/// one; another profile's record, and a profile's remaining items, are untouched. Setting 0 is a
/// forget; forgetting for a profile that never had a record creates none.
#[test]
fn forgetting_a_profiles_last_offset_removes_the_profile_record() {
    let mut s = Session::default();
    s.set_subtitle_offset_for("u1", "m", "1", Some(100));
    s.set_subtitle_offset_for("u1", "m", "2", Some(200));
    s.set_subtitle_offset_for("u2", "m", "1", Some(300));
    assert_eq!(s.subtitle_offsets.len(), 2);

    s.set_subtitle_offset_for("u1", "m", "1", None);
    assert_eq!(s.subtitle_offsets.len(), 2, "u1 still has an item");
    assert_eq!(s.subtitle_offset_for("u1", "m", "2"), Some(200));

    s.set_subtitle_offset_for("u1", "m", "2", None);
    assert_eq!(s.subtitle_offsets.len(), 1, "u1's record is gone with its last item");
    assert_eq!(s.subtitle_offsets[0].user, "u2");
    assert_eq!(s.subtitle_offset_for("u2", "m", "1"), Some(300), "u2 is untouched");

    s.set_subtitle_offset_for("u2", "m", "1", Some(0));
    assert!(s.subtitle_offsets.is_empty(), "an offset of 0 is a forget");

    s.set_subtitle_offset_for("nobody", "m", "1", None);
    assert!(s.subtitle_offsets.is_empty(), "forgetting for an unknown profile creates no record");
    assert_eq!(legacy_key(&s, "subtitle_offsets"), None, "and the key is skipped while empty");
}

// ---- item 15: what the file backend keeps at sign-out -------------------------------------------------

/// Point the canonical persistence root at a directory of this test's own.
struct Root {
    dir: std::path::PathBuf,
}
impl Root {
    fn new(tag: &str) -> Root {
        let dir = std::env::temp_dir().join(format!("plxnative-prefs-char-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        plx_base::paths::redirect_persistent_state_root_for_test(Some(dir.clone()));
        Root { dir }
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        plx_base::paths::redirect_persistent_state_root_for_test(None);
        redirect_candidates_for_test(None);
        redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// **The file backend keeps only `language` at sign-out** (session.rs `clear_for_erase`: the
/// credential-free `install_preferences::save(language)` before the credential file goes). Every
/// other preference, the per-profile lists and the install-wide scalars alike, is gone on the next
/// boot: that is today's fact, and it is what the per-profile move must not silently widen or
/// narrow.
#[test]
fn the_file_backend_keeps_only_the_language_at_sign_out() {
    use plx_platform::i18n::Preference;
    let _serial = plx_base::testlock::serial();
    let root = Root::new("signout-keeps-language");
    let saved = plx_platform::i18n::saved_preference();
    redirect_for_test(None);
    redirect_candidates_for_test(Some(vec![root.dir.join("legacy-auth.json")]));

    save(&all_non_default());
    assert_eq!(peek().prefs.subtitle_size, SubtitleSize::Large, "setup: the non-default preferences are stored");
    assert_eq!(peek().language, Preference::Be);
    assert!(matches!(clear(), ClearOutcome::Durable { .. }));

    // A new process has neither the session cache nor the confirmed-preference atomic.
    redirect_for_test(None);
    plx_platform::i18n::set_saved_preference(Preference::System);
    let fresh = load_with_id(|| "new-install-id".into());
    plx_platform::i18n::set_saved_preference(saved);

    assert_eq!(fresh.language, Preference::Be, "language is the one thing kept");
    // A session minted after sign-out is a FRESH one, not `Session::default()`: it writes the
    // explicit fresh quality (`auto`) and has trailer autoplay on. Neither is a kept preference.
    let mut expected = legacy(&Session::default());
    expected["client_id"] = json!("new-install-id");
    expected["language"] = json!("be");
    expected["playback_quality"] = json!("auto");
    expected["trailer_autoplay"] = json!(true);
    assert_eq!(legacy(&fresh), expected, "everything else is the fresh-install session");
}
