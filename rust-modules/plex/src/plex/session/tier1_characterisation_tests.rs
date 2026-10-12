//! **A0d: characterisation of three things `main` does today, so the track-memory refactor can
//! change the preference types without changing them by accident.** Every expected value below was
//! read from `main` (commit cab5d041) by running it, not derived. Nothing here changes behaviour.
//!
//! * Item 5: sign-out on the DB8 path. Which stored keys survive `ClearTenure`.
//! * Item 10: a queued account-data edit is dropped across a tenure change and across a
//!   `client_id` / `account_token` change; an install write is refused while revoked.
//! * Item 16: tier 1. A new build's extra preference keys, fed through the platform state codec
//!   and the OLD build's parse (`super::v0`), leave credentials and today's keys intact; and the
//!   two structural traps (a new `PublicPayload` slot, a new top-level state field) are refused by
//!   `decode`, which is why the plan forbids them. These tests do NOT assert that a new key
//!   survives a save: that is tier 2.
use super::*;
use super::v0;
use plx_platform::storage::{client, state, wire::*};

// ---- a signed-in session with every preference at a non-default value -----------------------

const MACHINE: &str = "aaaa1111";

fn pin() -> String {
    plx_base::spki::pin_from_spki_der(b"synthetic spki")
}

/// Every credential and every preference key `main` models, none at its default, so a key that
/// goes missing or is rewritten shows.
fn rich() -> Session {
    let mut session: Session = serde_json::from_value(serde_json::json!({
        "client_id": "tier1-client", "account_token": "tier1-account",
        "server": {"name": "Mac mini", "machine_id": MACHINE, "address": "192.168.0.10",
                   "port": 32400, "token": "tier1-server"},
        "user": {"id": 7, "uuid": "u-7", "title": "Gleb", "thumb": "", "token": "tier1-user"},
        "home_users": [{"uuid": "u-7", "title": "Gleb", "thumb": "", "protected": false, "admin": true}],
        "sources": [
            {"machine_id": MACHINE, "name": "Mac mini", "shared_by": "", "owned": true,
             "address": "192.168.0.10", "port": 32400, "token": "tier1-server"},
            {"machine_id": "bbbb2222", "name": "nas-home", "shared_by": "friend", "owned": false,
             "address": "203.0.113.9", "port": 31234, "token": "tier1-share"}],
        "home_pins": [{"user": "u-7", "asked": true,
                       "on": [{"machine_id": "bbbb2222", "key": 1}],
                       "off": [{"machine_id": MACHINE, "key": 1}]}],
        "recent_searches": [{"user": "u-7", "terms": ["alien", "heat"]}],
        "auto_sign_in": true, "trailer_autoplay": false,
        "last_hero_blur": [[0.1, 0.2, 0.3], [0.1, 0.2, 0.3], [0.1, 0.2, 0.3], [0.1, 0.2, 0.3]]
    })).unwrap();
    session.language = plx_platform::i18n::Preference::Be;
    session.prefs.playback_quality = Some(PlaybackQuality::P1080High);
    session.prefs.direct_play_mode = DirectPlayMode::Forced;
    session.prefs.subtitle_tone = SubtitleTone::Grey;
    session.prefs.subtitle_size = SubtitleSize::Large;
    session.prefs.subtitle_position = SubtitlePosition::High;
    session.prefs.next_episode_mode = NextEpisodeMode::AfterCredits;
    session.prefs.skip_interval = SkipInterval::Seconds30;
    session.prefs.deck_press = DeckPress::Play;
    session.prefs.audio_enhancements = crate::plex::AudioEnhancements { boost_dialog: true, normalize_loudness: false };
    session.set_subtitle_offset_for("u-7", MACHINE, "rk-1", Some(1_500));
    session.set_view_for("u-7", LibraryView {
        machine_id: MACHINE.into(), key: 1, sort: "addedAt".into(), desc: true, ..Default::default()
    });
    session.last_library.push(LastLibrary {
        user: "u-7".into(),
        libs: vec![TypedLib { kind: "movie".into(), machine_id: MACHINE.into(), key: 1, extensions: Default::default() }],
        extensions: Default::default(),
    });
    session.with_plaintext_choice("acct-fp", MACHINE, PlaintextChoice::Allowed)
        .with_server_key_pin(MACHINE, &pin()).expect("a well-formed pin")
}

// ---- item 5: sign-out on the DB8 path -------------------------------------------------------

#[derive(Default)]
struct Db8 {
    record: Option<Value>,
    puts: usize,
}
impl plx_platform::storage::keymanager::Rpc for Db8 {
    fn call(&mut self, uri: &str, payload: &Value) -> Result<Value, ErrorCode> {
        match uri {
            "luna://com.palm.db/get" => Ok(
                serde_json::json!({"returnValue": true, "results": self.record.iter().cloned().collect::<Vec<_>>()})),
            "luna://com.palm.db/put" => {
                let mut next = payload["objects"][0].clone();
                if let Some(current) = &self.record {
                    assert_eq!(next["_rev"], current["_rev"], "the coordinator must CAS the loaded revision");
                }
                self.puts += 1;
                next["_rev"] = serde_json::json!(self.puts);
                self.record = Some(next);
                Ok(serde_json::json!({"returnValue": true, "results": [{"id": "plxstate.stable", "rev": self.puts}]}))
            }
            _ => panic!("the DB8 scenario must not invoke any other RPC"),
        }
    }
}

fn helper() -> plx_platform::storage::backend::Backend<Db8> {
    plx_platform::storage::backend::Backend::new(
        Db8::default(), state::Flavor::Stable, "com.beb.plxnative.storage".into())
}

fn stored_public(transport: &mut dyn client::Transport) -> state::PublicPayload {
    let client::Load::Present(snapshot) = client::load_with(transport).unwrap() else {
        panic!("helper snapshot")
    };
    snapshot.state.public
}

/// **Sign-out on DB8 keeps every INSTALL key and, today, also the per-profile keys that are not
/// named in `ACCOUNT_BOUND_PREFERENCES`.** `ClearTenure` keeps `public.preferences` whole except
/// `server_key_pins` and `plaintext_consent`, and resets every other slot of the public payload
/// (`client_id`, `pins`, `recents`, ...). So `subtitle_offsets`, `library_sorts` and
/// `last_library` DO survive a sign-out today. Pinned as it is; the SO PR flips these three.
#[test]
fn db8_signout_keeps_install_keys_and_per_profile_preferences_but_not_pins_recents_or_account_bound_keys() {
    let mut backend = helper();
    let mut transport = |request: Request| Ok(backend.dispatch(request));
    let session = rich();
    assert!(matches!(
        persistence::commit_session_with(&session, false, SaveAuthority::FreshReauthentication, 4, &mut transport),
        persistence::CanonicalCommit::Durable { .. }));

    // Control: before sign-out every key is stored.
    let before = stored_public(&mut transport);
    let stored = before.preferences.as_object().expect("preferences object");
    for key in ["language", "playback_quality", "direct_play_mode", "auto_sign_in", "trailer_autoplay",
        "subtitle_tone", "subtitle_size", "subtitle_position", "next_episode_mode", "skip_interval",
        "deck_press", "audio_enhancements", "last_hero_blur", "last_library", "library_sorts",
        "subtitle_offsets", "plaintext_consent", "server_key_pins"] {
        assert!(stored.contains_key(key), "before sign-out `{key}` is stored: {stored:?}");
    }
    assert_eq!(before.client_id.as_deref(), Some("tier1-client"));
    assert!(!before.pins.is_null() && !before.recents.is_null(), "home pins and recents are stored");

    assert!(matches!(persistence::commit_clear_with(&mut transport), persistence::CanonicalCommit::Durable { .. }));
    let after = stored_public(&mut transport);

    // Install keys survive, value for value.
    for key in ["language", "playback_quality", "direct_play_mode", "auto_sign_in", "trailer_autoplay",
        "subtitle_tone", "subtitle_size", "subtitle_position", "next_episode_mode", "skip_interval",
        "deck_press", "audio_enhancements", "last_hero_blur"] {
        assert_eq!(after.preferences.get(key), before.preferences.get(key), "install key `{key}` survives sign-out");
    }
    // The per-profile keys that ClearTenure does not name survive too (SO flips these).
    for key in ["subtitle_offsets", "library_sorts", "last_library"] {
        assert_eq!(after.preferences.get(key), before.preferences.get(key), "`{key}` survives sign-out today");
    }
    // The two account-bound keys do not.
    for key in ["server_key_pins", "plaintext_consent"] {
        assert!(after.preferences.get(key).is_none(), "`{key}` is dropped by sign-out");
    }
    // The other public slots are reset: the account's identity, home pins and recent searches go.
    assert_eq!(after.client_id, None);
    assert!(after.pins.is_null(), "home pins do not survive sign-out");
    assert!(after.recents.is_null(), "recent searches do not survive sign-out");
    // Exactly the stored keys, minus the two: nothing else appeared or vanished.
    let mut expected = before.preferences.clone();
    for key in ["server_key_pins", "plaintext_consent"] {
        expected.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(after.preferences, expected);
}

// ---- item 10: queued edits across a tenure / identity change; revoked writes ----------------

fn signed_in() -> Session {
    Session { client_id: "cid-1".into(), account_token: "acct".into(), ..Default::default() }
}

/// Hold the persistence worker on a job so that an edit queued behind it runs only after the
/// test has changed the world, then release it. Returns the pieces; `release` frees the worker.
struct Parked {
    release: std::sync::mpsc::Sender<()>,
}
impl Parked {
    fn new() -> Parked {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started, running) = std::sync::mpsc::channel::<()>();
        drop(plx_base::storage_worker::submit_retained(move || {
            let _ = started.send(());
            let _ = wait.recv();
        }));
        running.recv().expect("the worker picked the parking job up");
        Parked { release }
    }
    fn release(self) {
        let _ = self.release.send(());
    }
}

fn queue_offset_edit() -> plx_base::storage_worker::TypedTicket<bool> {
    queue_update_ticket(|current| {
        let mut next = current.clone();
        next.set_subtitle_offset_for("", "machine", "rk-queued", Some(2_000));
        Some(next)
    }).unwrap()
}

fn has_queued_offset() -> bool {
    peek().subtitle_offset_for("", "machine", "rk-queued") == Some(2_000)
}

/// Control for the three drops below: with nothing changed between queue time and run time, the
/// queued offset edit IS written. Without this the drops could be a fixture that never writes.
#[test]
fn a_queued_offset_edit_is_written_when_nothing_changed() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("tier1-queue-control");
    save(&signed_in());
    assert_eq!(peek().client_id, "cid-1");
    let parked = Parked::new();
    let ticket = queue_offset_edit();
    parked.release();
    assert!(ticket.wait_blocking().unwrap(), "the edit ran and wrote");
    assert!(has_queued_offset());
}

/// A sign-out followed by a completed sign-in of the SAME account between queue and run: the
/// `client_id` and `account_token` match again, so only the tenure counter can drop the edit.
#[test]
fn a_queued_offset_edit_is_dropped_across_a_tenure_change() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("tier1-queue-tenure");
    save(&signed_in());
    assert_eq!(peek().client_id, "cid-1");
    let parked = Parked::new();
    let ticket = queue_offset_edit();
    revoke_cached_session();
    save(&signed_in());
    assert!(!cache_revoked(), "the proven sign-in ended the revocation, so only the tenure differs");
    assert_eq!(peek().client_id, "cid-1");
    assert_eq!(peek().account_token, "acct");
    parked.release();
    assert!(!ticket.wait_blocking().unwrap(), "the obsolete edit reports not written");
    assert!(!has_queued_offset(), "and wrote nothing");
}

/// The account changed under the queue without a revocation: a different `client_id`, then a
/// different `account_token` with the same `client_id`.
#[test]
fn a_queued_offset_edit_is_dropped_across_a_client_id_or_account_token_change() {
    let _serial = plx_base::testlock::serial();
    for (tag, replacement) in [
        ("client", Session { client_id: "cid-2".into(), account_token: "acct".into(), ..Default::default() }),
        ("token", Session { client_id: "cid-1".into(), account_token: "acct-2".into(), ..Default::default() }),
    ] {
        let _root = TempSession::new(&format!("tier1-queue-{tag}"));
        save(&signed_in());
        assert_eq!(peek().client_id, "cid-1");
        let parked = Parked::new();
        let ticket = queue_offset_edit();
        save(&replacement);
        parked.release();
        assert!(!ticket.wait_blocking().unwrap(), "{tag}: the obsolete edit reports not written");
        assert!(!has_queued_offset(), "{tag}: and wrote nothing");
        assert_eq!(peek().client_id, replacement.client_id);
        assert_eq!(peek().account_token, replacement.account_token);
    }
}

/// An INSTALL write (an unconditional read-modify-write of a player preference) is refused while
/// the cache is revoked, and the file is not touched.
#[test]
fn an_install_write_is_refused_while_the_session_is_revoked() {
    let _serial = plx_base::testlock::serial();
    let root = TempSession::new("tier1-revoked");
    save(&signed_in());
    let on_disk = std::fs::read(root.path()).unwrap();
    revoke_cached_session();
    assert!(cache_revoked());
    assert!(!update(|current| Some(current.with_auto_sign_in(true))), "the edit never ran to a write");
    assert!(update_with_outcome(|current| Some(current.with_auto_sign_in(true))).is_none());
    assert!(cache_revoked(), "a refused write does not end the revocation");
    assert_eq!(std::fs::read(root.path()).unwrap(), on_disk, "the file is untouched");
    // A queued edit behind it is refused the same way.
    let ticket = queue_offset_edit();
    assert!(!ticket.wait_blocking().unwrap());
    assert_eq!(std::fs::read(root.path()).unwrap(), on_disk);
}

// ---- item 16: tier 1 ------------------------------------------------------------------------

/// The extra keys a new build puts in the public preferences, with a malformed variant of each.
/// `account` rides INSIDE the offset records (a per-record key a new build stamps).
fn new_build_preferences(base: &Value, malformed: bool) -> Value {
    let mut preferences = base.clone();
    let object = preferences.as_object_mut().unwrap();
    if malformed {
        object.insert("profile_prefs".into(), serde_json::json!("not-an-object"));
        object.insert("track_memory".into(), serde_json::json!([1, "two", null]));
    } else {
        object.insert("profile_prefs".into(), serde_json::json!({
            "u-7": {"subtitle_size": "large", "skip_interval": "30s"}}));
        object.insert("track_memory".into(), serde_json::json!({
            "version": 1, "profiles": {"u-7": {"show:rk-9": {"audio": "eng", "subtitle": "off"}}}}));
    }
    let offsets = object.get_mut("subtitle_offsets").and_then(Value::as_array_mut).expect("offsets are stored");
    for record in offsets {
        let account = if malformed { serde_json::json!({"fp": [1, 2]}) } else { serde_json::json!("acct-fp") };
        record["account"] = account.clone();
        record["items"][0]["account"] = account;
    }
    preferences
}

/// Today's keys, as the old build's types hold them, without the opaque extensions.
fn todays_keys(p: &v0::CanonicalSessionPreferences) -> Value {
    serde_json::json!({
        "language": p.language, "playback_quality": p.playback_quality,
        "direct_play_mode": p.direct_play_mode, "auto_sign_in": p.auto_sign_in,
        "trailer_autoplay": p.trailer_autoplay, "subtitle_tone": p.subtitle_tone,
        "subtitle_size": p.subtitle_size, "subtitle_position": p.subtitle_position,
        "next_episode_mode": p.next_episode_mode, "skip_interval": p.skip_interval,
        "deck_press": p.deck_press, "audio_enhancements": p.audio_enhancements,
        "last_hero_blur": p.last_hero_blur,
        "last_library": p.last_library.iter().map(|l| serde_json::json!({
            "user": l.user, "libs": l.libs.iter().map(|t| (&t.kind, &t.machine_id, t.key)).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "library_views": p.library_views.iter().map(|l| serde_json::json!({
            "user": l.user, "libs": l.libs.iter().map(|v| (&v.machine_id, v.key, &v.sort, v.desc)).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "subtitle_offsets": p.subtitle_offsets.iter().map(|o| serde_json::json!({
            "user": o.user, "items": o.items.iter().map(|e| (&e.machine_id, &e.rating_key, e.offset_ms)).collect::<Vec<_>>()})).collect::<Vec<_>>(),
        "plaintext_consent": p.plaintext_consent.iter().map(|c| (&c.machine_id, &c.account, c.choice)).collect::<Vec<_>>(),
        "server_key_pins": p.server_key_pins.iter().map(|k| (&k.machine_id, &k.pin)).collect::<Vec<_>>(),
    })
}

/// A real `CanonicalState` as the helper would hold it: the public half from `split_canonical`
/// plus `extra` preference keys, a protected envelope slot filled (the helper-owned ciphertext is
/// opaque to the codec), the session migration complete.
fn canonical_state(public: &state::PublicPayload, preferences: Value) -> state::CanonicalState {
    let mut canonical = state::CanonicalState::new(state::Flavor::Stable, state::Generation([1; 16]));
    canonical.public = state::PublicPayload { preferences, ..public.clone() };
    canonical.auth_envelope = Some("opaque-protected-envelope".into());
    canonical.migrations.session.progress = state::MigrationProgress::Complete;
    canonical.migrations.consent.progress = state::MigrationProgress::Complete;
    canonical
}

fn assert_credentials_intact(joined: &Session, original: &Session, case: &str) {
    assert_eq!(joined.client_id, original.client_id, "{case}: client_id");
    assert_eq!(joined.account_token, original.account_token, "{case}: account_token");
    assert_eq!(serde_json::to_value(&joined.server).unwrap(), serde_json::to_value(&original.server).unwrap(), "{case}: server");
    assert_eq!(serde_json::to_value(&joined.sources).unwrap(), serde_json::to_value(&original.sources).unwrap(), "{case}: server list");
    assert_eq!(serde_json::to_value(&joined.user).unwrap(), serde_json::to_value(&original.user).unwrap(), "{case}: user");
    assert_eq!(serde_json::to_value(&joined.home_users).unwrap(), serde_json::to_value(&original.home_users).unwrap(), "{case}: home users");
}

/// **Tier 1, DB8 path.** New-build extra keys in the public preferences (and a per-record
/// `account` inside the offset records), well formed and malformed, through the platform codec
/// (`encode` then `decode`, both validating), then the old build's preference parse and today's
/// `join_canonical`: credentials intact, today's keys parse as before, extras land in opaque
/// extensions and nowhere else.
#[test]
fn tier1_db8_extra_preference_keys_leave_credentials_and_todays_keys_intact() {
    let original = rich();
    let (public, auth) = split_canonical(&original).unwrap();
    let baseline = todays_keys(&v0::CanonicalSessionPreferences::parse(&public.preferences));

    for (case, malformed) in [("well-formed", false), ("malformed", true)] {
        let preferences = new_build_preferences(&public.preferences, malformed);
        let bytes = canonical_state(&public, preferences.clone()).encode().expect("encode validates");
        let decoded = state::CanonicalState::decode(&bytes, state::Flavor::Stable).expect("decode validates");
        assert_eq!(decoded.public.preferences, preferences, "{case}: the codec keeps the preferences byte for byte");

        // The old build's preference parse.
        let parsed = v0::CanonicalSessionPreferences::parse(&decoded.public.preferences);
        assert_eq!(todays_keys(&parsed), baseline, "{case}: today's keys parse as before");
        assert_eq!(parsed.extensions.get("profile_prefs"), preferences.get("profile_prefs"), "{case}");
        assert_eq!(parsed.extensions.get("track_memory"), preferences.get("track_memory"), "{case}");
        assert_eq!(parsed.extensions.len(), 2, "{case}: exactly the two new keys are opaque");
        for record in &parsed.subtitle_offsets {
            assert_eq!(record.extensions.0.get("account"), preferences["subtitle_offsets"][0].get("account"), "{case}");
            assert_eq!(record.items[0].extensions.0.get("account"), preferences["subtitle_offsets"][0]["items"][0].get("account"), "{case}");
        }
        assert_eq!(parsed.subtitle_offsets.len(), 1, "{case}: the offset record itself survives");

        // Today's join.
        let joined = join_canonical(&decoded.public, &auth).expect("the credentials still open");
        assert_credentials_intact(&joined, &original, case);
        assert_eq!(joined.prefs.subtitle_size, original.prefs.subtitle_size, "{case}");
        assert_eq!(joined.prefs.skip_interval, original.prefs.skip_interval, "{case}");
        assert_eq!(joined.trailer_autoplay, original.trailer_autoplay, "{case}");
        assert_eq!(joined.language, original.language, "{case}");
        assert_eq!(joined.subtitle_offset_for("u-7", MACHINE, "rk-1"), Some(1_500), "{case}");
        assert_eq!(joined.last_library.len(), 1, "{case}");
        assert_eq!(joined.library_views.len(), 1, "{case}");
        assert_eq!(joined.plaintext_choice("acct-fp", MACHINE), PlaintextChoice::Allowed, "{case}");
        assert_eq!(joined.server_key_pin(MACHINE), Some(pin().as_str()), "{case}");
        assert!(joined.extensions.0.get("profile_prefs").is_none() && joined.extensions.0.get("track_memory").is_none(),
            "{case}: a public preference extension is not promoted into the session's protected extensions");
        // The public-only snapshot (a locked protected bundle) reads the same keys.
        let public_only = public_session(&decoded.public);
        assert_eq!(public_only.client_id, original.client_id, "{case}");
        assert!(public_only.account_token.is_empty(), "{case}: no credential in the public snapshot");
        assert_eq!(public_only.prefs.subtitle_size, original.prefs.subtitle_size, "{case}");
    }
}

/// **Tier 1, DB8 path, hostile preferences.** A preferences value that is not an object, or
/// whose known keys are the wrong shape, costs the preferences and never the credentials.
#[test]
fn tier1_db8_unreadable_preferences_cost_only_the_preferences() {
    let original = rich();
    let (public, auth) = split_canonical(&original).unwrap();
    for (case, preferences) in [
        ("array", serde_json::json!(["profile_prefs"])),
        ("string", serde_json::json!("track_memory")),
        ("null", Value::Null),
    ] {
        let bytes = canonical_state(&public, preferences.clone()).encode().expect("encode validates");
        let decoded = state::CanonicalState::decode(&bytes, state::Flavor::Stable).expect("decode validates");
        let parsed = v0::CanonicalSessionPreferences::parse(&decoded.public.preferences);
        assert!(parsed.trailer_autoplay, "{case}: absence is on");
        assert!(parsed.subtitle_offsets.is_empty(), "{case}");
        let joined = join_canonical(&decoded.public, &auth).expect("the credentials still open");
        assert_credentials_intact(&joined, &original, case);
        assert!(joined.trailer_autoplay, "{case}: the whole-value fallback keeps trailer autoplay on");
        assert!(joined.subtitle_offsets.is_empty(), "{case}");
    }
}

/// **Tier 1, legacy-file path.** The same keys at the file's top level, parsed by the frozen
/// `v0::Session`: credentials intact, today's keys as before, the extras opaque.
#[test]
fn tier1_legacy_file_extra_top_level_keys_leave_credentials_and_todays_keys_intact() {
    let original = rich();
    let base = serde_json::to_value(&original).unwrap();
    let baseline: v0::Session = serde_json::from_value(base.clone()).unwrap();

    for (case, malformed) in [("well-formed", false), ("malformed", true)] {
        let mut file = base.clone();
        let extras = new_build_preferences(&serde_json::json!({"subtitle_offsets": base["subtitle_offsets"]}), malformed);
        let object = file.as_object_mut().unwrap();
        for key in ["profile_prefs", "track_memory", "subtitle_offsets"] {
            object.insert(key.into(), extras[key].clone());
        }
        let parsed: v0::Session = serde_json::from_value(file.clone()).expect("the old build still reads the file");
        assert_eq!(parsed.client_id, original.client_id, "{case}");
        assert_eq!(parsed.account_token, original.account_token, "{case}");
        assert_eq!(serde_json::to_value(&parsed.server).unwrap(), serde_json::to_value(&original.server).unwrap(), "{case}");
        assert_eq!(serde_json::to_value(&parsed.sources).unwrap(), serde_json::to_value(&original.sources).unwrap(), "{case}");
        assert_eq!(serde_json::to_value(&parsed.user).unwrap(), serde_json::to_value(&original.user).unwrap(), "{case}");
        assert_eq!(parsed.home_pins, baseline.home_pins, "{case}");
        assert_eq!(parsed.recent_searches, baseline.recent_searches, "{case}");
        assert_eq!(parsed.language, original.language, "{case}");
        assert_eq!(parsed.subtitle_size, original.prefs.subtitle_size, "{case}");
        assert_eq!(parsed.skip_interval, original.prefs.skip_interval, "{case}");
        assert_eq!(parsed.trailer_autoplay, original.trailer_autoplay, "{case}");
        assert_eq!(parsed.plaintext_consent, baseline.plaintext_consent, "{case}");
        assert_eq!(parsed.server_key_pins, baseline.server_key_pins, "{case}");
        assert_eq!(parsed.last_library, baseline.last_library, "{case}");
        assert_eq!(parsed.library_views, baseline.library_views, "{case}");
        assert_eq!(parsed.subtitle_offsets.len(), 1, "{case}");
        assert_eq!(parsed.subtitle_offsets[0].items[0].offset_ms, 1_500, "{case}");
        assert_eq!(parsed.extensions.0.get("profile_prefs"), file.get("profile_prefs"), "{case}");
        assert_eq!(parsed.extensions.0.get("track_memory"), file.get("track_memory"), "{case}");
        assert_eq!(parsed.extensions.0.len(), 2, "{case}: exactly the two new keys are opaque");
        assert_eq!(parsed.subtitle_offsets[0].extensions.0.get("account"), file["subtitle_offsets"][0].get("account"), "{case}");

        // Main's own Session reads the same file the same way (what a restart of this build sees).
        let live: Session = serde_json::from_value(file).expect("main reads it too");
        assert_eq!(live.account_token, original.account_token, "{case}");
        assert_eq!(live.subtitle_offset_for("u-7", MACHINE, "rk-1"), Some(1_500), "{case}");
        assert_eq!(live.extensions.0.len(), 2, "{case}");
    }
}

/// **Negative control.** The two structural traps the tier-1 rule forbids both FAIL `decode`: an
/// extra field in `public` (a new `PublicPayload` slot) and an extra top-level field of the
/// state. A new build must therefore carry new data inside `public.preferences`, never beside it.
#[test]
fn tier1_negative_control_a_new_public_slot_or_state_field_fails_decode() {
    let original = rich();
    let (public, _) = split_canonical(&original).unwrap();
    let bytes = canonical_state(&public, public.preferences.clone()).encode().unwrap();
    assert!(state::CanonicalState::decode(&bytes, state::Flavor::Stable).is_ok(), "control: the unmodified bytes decode");

    let mut in_public: Value = serde_json::from_slice(&bytes).unwrap();
    in_public["public"]["profile_prefs"] = serde_json::json!({"u-7": {}});
    assert_eq!(
        state::CanonicalState::decode(&serde_json::to_vec(&in_public).unwrap(), state::Flavor::Stable).unwrap_err(),
        state::StateError::Invalid, "an extra field in `public` is refused");

    let mut top_level: Value = serde_json::from_slice(&bytes).unwrap();
    top_level["track_memory"] = serde_json::json!({"version": 1});
    assert_eq!(
        state::CanonicalState::decode(&serde_json::to_vec(&top_level).unwrap(), state::Flavor::Stable).unwrap_err(),
        state::StateError::Invalid, "an extra top-level field is refused");
}
