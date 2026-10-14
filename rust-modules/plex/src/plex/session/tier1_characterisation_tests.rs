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
use super::test_support::{signed_in, STAMP};
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
    session.prefs.last_library.push(LastLibrary {
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

/// **Sign-out on DB8 keeps every INSTALL key and drops every key named in
/// `ACCOUNT_BOUND_PREFERENCES`.** `ClearTenure` keeps `public.preferences` whole except
/// `server_key_pins`, `plaintext_consent`, `track_memory`, `subtitle_offsets`, `library_sorts` and
/// `last_library`, and resets every other slot of the public payload (`client_id`, `pins`,
/// `recents`, ...). DECLARED FLIP (SO, QR3): the last three used to survive a sign-out.
#[test]
fn db8_signout_keeps_install_keys_but_not_pins_recents_or_account_bound_keys() {
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
    // The account-bound keys do not (SO: the three per-profile keys joined the list).
    for key in ["server_key_pins", "plaintext_consent", "subtitle_offsets", "library_sorts", "last_library"] {
        assert!(after.preferences.get(key).is_none(), "`{key}` is dropped by sign-out");
    }
    // The other public slots are reset: the account's identity, home pins and recent searches go.
    assert_eq!(after.client_id, None);
    assert!(after.pins.is_null(), "home pins do not survive sign-out");
    assert!(after.recents.is_null(), "recent searches do not survive sign-out");
    // Exactly the stored keys, minus the five: nothing else appeared or vanished.
    let mut expected = before.preferences.clone();
    for key in ["server_key_pins", "plaintext_consent", "subtitle_offsets", "library_sorts", "last_library"] {
        expected.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(after.preferences, expected);
}

/// **P-d2, the DB8 path: after a sign-out and a sign-in every profile reads the frozen legacy values.**
/// `profile_prefs` is account-bound, so `ClearTenure` drops every profile's own picks with the
/// credentials; the top-level keys they fell back to are install keys and survive. The profile that
/// had picked its own values reads the legacy ones in the next sign-in (a new token, so a new stamp,
/// would hide the records anyway: this is the tenure half). Decided item 17: their own picks are gone.
#[test]
fn sign_out_then_in_reads_the_legacy_values() {
    let mut backend = helper();
    let mut transport = |request: Request| Ok(backend.dispatch(request));
    let mut session = rich();
    let stamp = crate::plex::grant::account_key(&session.account_token);
    assert!(put_own::<SubtitleSizePref>(&mut session.prefs.profile_prefs, "u-7", &stamp, SubtitleSize::Small));
    assert!(put_own::<SkipIntervalPref>(&mut session.prefs.profile_prefs, "u-7", &stamp, SkipInterval::Seconds5));
    assert!(matches!(
        persistence::commit_session_with(&session, false, SaveAuthority::FreshReauthentication, 4, &mut transport),
        persistence::CanonicalCommit::Durable { .. }));
    let before = stored_public(&mut transport);
    assert!(before.preferences.get("profile_prefs").is_some(), "control: the picks are stored");

    assert!(matches!(persistence::commit_clear_with(&mut transport), persistence::CanonicalCommit::Durable { .. }));
    let after = stored_public(&mut transport);
    assert!(after.preferences.get("profile_prefs").is_none(), "a sign-out drops every profile's own picks");
    for key in ["subtitle_size", "skip_interval"] {
        assert_eq!(after.preferences.get(key), before.preferences.get(key), "but the frozen legacy key `{key}` stays");
    }

    // The next sign-in builds its session from what survived.
    let kept = serde_json::from_value::<CanonicalSessionPreferences>(after.preferences.clone()).unwrap();
    let next = Session { account_token: "tier1-account-2".into(), prefs: kept.prefs, ..Default::default() };
    assert!(next.prefs.profile_prefs.is_empty());
    let seat = Seat::Seated { user: "u-7".into(), stamp: crate::plex::grant::account_key("tier1-account-2") };
    assert_eq!(stored_effective::<SubtitleSizePref>(&next, &seat), SubtitleSize::Large,
        "the profile that picked Small reads the legacy Large");
    assert_eq!(stored_effective::<SkipIntervalPref>(&next, &seat), SkipInterval::Seconds30);
}

// ---- item 10: queued edits across a tenure / identity change; revoked writes ----------------

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
        assert_eq!(joined.prefs.trailer_autoplay, original.prefs.trailer_autoplay, "{case}");
        assert_eq!(joined.language, original.language, "{case}");
        assert_eq!(joined.subtitle_offset_for("u-7", MACHINE, "rk-1"), Some(1_500), "{case}");
        assert_eq!(joined.prefs.last_library.len(), 1, "{case}");
        assert_eq!(joined.prefs.library_views.len(), 1, "{case}");
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
        assert!(joined.prefs.trailer_autoplay, "{case}: the whole-value fallback keeps trailer autoplay on");
        assert!(joined.prefs.subtitle_offsets.is_empty(), "{case}");
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
        assert_eq!(parsed.trailer_autoplay, original.prefs.trailer_autoplay, "{case}");
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
        // DECLARED FLIP (C1): `track_memory` is a modelled `Preferences` field, so this build reads
        // it (a stored value of another shape is parsed softly) instead of carrying it opaque; the
        // frozen `v0` parse above still sees both keys as extensions.
        // DECLARED FLIP (P1a): so is `profile_prefs`. The shapes above are ones nothing writes (an
        // object keyed by user, a string), which the soft list parse reads as no records; the shape
        // this build writes rides through the previous build in
        // `tier1_the_real_profile_prefs_shape_rides_through_the_previous_build`.
        assert!(live.extensions.0.is_empty(), "{case}: {:?}", live.extensions.0.keys().collect::<Vec<_>>());
        assert!(live.prefs.profile_prefs.is_empty(), "{case}: a shape nothing writes reads as no records");
    }
}

/// **A0 item 16, the real shape of `profile_prefs`.** The cases above feed tier 1 shapes nothing
/// writes. This is the shape this build writes (an array of records, each with the profile, the
/// account stamp and the values it picked), through the platform codec and the PREVIOUS build's
/// frozen parse: credentials and today's keys intact, the key opaque, and the previous build's own
/// rewrite of it byte-identical; this build then reads back exactly what it wrote. It also proves the
/// frozen top-level keys are still the previous build's to read: a profile's pick never rewrote them.
#[test]
fn tier1_the_real_profile_prefs_shape_rides_through_the_previous_build() {
    let mut original = rich();
    assert!(put_own::<SubtitleSizePref>(&mut original.prefs.profile_prefs, "", STAMP, SubtitleSize::Small));
    assert!(put_own::<SkipIntervalPref>(&mut original.prefs.profile_prefs, "u-7", STAMP, SkipInterval::Seconds10));
    assert!(put_own::<AudioEnhancementsPref>(&mut original.prefs.profile_prefs, "u-7", STAMP,
        crate::plex::AudioEnhancements { boost_dialog: false, normalize_loudness: true }));
    // P1c: the hero trailer switch is a field of the same record (an ON pick over the TV's off), so the
    // shape below holds it too. `rich()`'s legacy top-level key is off, and the pick must not write it.
    assert!(put_own::<TrailerAutoplayPref>(&mut original.prefs.profile_prefs, "u-7", STAMP, true));
    let (public, auth) = split_canonical(&original).unwrap();
    let real = public.preferences["profile_prefs"].clone();
    assert_eq!(real.as_array().map(Vec::len), Some(2), "{real}");
    assert_eq!(real[0]["user"], serde_json::json!(""), "the owner is the empty user");
    assert_eq!(real[1]["account"], serde_json::json!(STAMP));
    assert_eq!(real[1]["prefs"], serde_json::json!({
        "skip_interval": "10s", "audio_enhancements": {"boost_dialog": false, "normalize_loudness": true},
        "trailer_autoplay": true}));
    // The top-level keys stay what they were: no pick wrote them.
    assert_eq!(public.preferences["subtitle_size"], serde_json::to_value(SubtitleSize::Large).unwrap());
    assert_eq!(public.preferences["skip_interval"], serde_json::to_value(SkipInterval::Seconds30).unwrap());
    assert_eq!(public.preferences["trailer_autoplay"], serde_json::json!(original.prefs.trailer_autoplay),
        "and so is the TV's trailer key (off): the pick did not write it");

    let bytes = canonical_state(&public, public.preferences.clone()).encode().expect("encode validates");
    let decoded = state::CanonicalState::decode(&bytes, state::Flavor::Stable).expect("decode validates");
    assert_eq!(decoded.public.preferences, public.preferences, "the codec keeps it byte for byte");

    // The previous build.
    let baseline = todays_keys(&v0::CanonicalSessionPreferences::parse(&split_canonical(&rich()).unwrap().0.preferences));
    let old = v0::CanonicalSessionPreferences::parse(&decoded.public.preferences);
    assert_eq!(todays_keys(&old), baseline, "today's keys parse as before");
    assert_eq!(old.extensions.keys().collect::<Vec<_>>(), ["profile_prefs"], "exactly the new key is opaque");
    assert_eq!(old.extensions.get("profile_prefs"), Some(&real));
    assert_eq!(serde_json::to_value(&old).unwrap()["profile_prefs"], real, "its own rewrite keeps the value");

    // This build.
    let joined = join_canonical(&decoded.public, &auth).expect("the credentials still open");
    assert_credentials_intact(&joined, &original, "real shape");
    assert_eq!(joined.prefs.profile_prefs, original.prefs.profile_prefs);
    assert_eq!(joined.prefs.subtitle_size, SubtitleSize::Large, "the TV-wide key is untouched");

    // The legacy file: the previous build's frozen `Session` carries it opaque and keeps the credentials.
    let file = serde_json::to_value(&original).unwrap();
    assert_eq!(file["profile_prefs"], real);
    let old: v0::Session = serde_json::from_value(file.clone()).expect("the previous build still reads the file");
    assert_eq!(old.extensions.0.get("profile_prefs"), Some(&real));
    assert_eq!(old.account_token, original.account_token);
    assert_eq!(serde_json::to_value(&old).unwrap()["profile_prefs"], real);
    let back: Session = serde_json::from_value(file).unwrap();
    assert_eq!(back.prefs.profile_prefs, original.prefs.profile_prefs);
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

// ---- SO: the real shape of stamped offsets, and the one login that is not a sign-out ------------

/// **Item 16, the real-shape case for stamped offsets (SO).** A's cases above use guessed shapes;
/// this one serializes a REAL new-build session, whose offset record carries the account stamp it
/// wrote itself, and feeds it to the old build's parse and to the platform codec. The stamp is a
/// per-record key the old build keeps opaquely (`extensions`); credentials and today's keys are
/// intact on both stores.
#[test]
fn tier1_real_shape_stamped_offsets_are_read_by_the_old_build_and_the_codec() {
    let original = rich();
    let stamp = crate::plex::grant::account_key("tier1-account");
    assert!(!stamp.is_empty());
    let (public, auth) = split_canonical(&original).unwrap();
    assert_eq!(public.preferences["subtitle_offsets"][0]["account"], serde_json::json!(stamp),
        "the real record is stamped with its own account");
    assert!(public.preferences["subtitle_offsets"][0]["items"][0].get("account").is_none(), "the entries are not");

    // The DB8 path: the platform codec, then the old build's parse and join.
    let bytes = canonical_state(&public, public.preferences.clone()).encode().expect("encode validates");
    let decoded = state::CanonicalState::decode(&bytes, state::Flavor::Stable).expect("decode validates");
    assert_eq!(decoded.public.preferences, public.preferences, "the codec keeps the preferences byte for byte");
    let parsed = v0::CanonicalSessionPreferences::parse(&decoded.public.preferences);
    assert_eq!(parsed.subtitle_offsets.len(), 1);
    assert_eq!(parsed.subtitle_offsets[0].user, "u-7");
    assert_eq!(parsed.subtitle_offsets[0].items[0].offset_ms, 1_500);
    assert_eq!(parsed.subtitle_offsets[0].extensions.0.get("account"), Some(&serde_json::json!(stamp)),
        "the old build keeps the stamp opaque");
    let joined = join_canonical(&decoded.public, &auth).expect("the credentials still open");
    assert_credentials_intact(&joined, &original, "db8");
    assert_eq!(joined.subtitle_offset_for("u-7", MACHINE, "rk-1"), Some(1_500));

    // The legacy-file path: the same record in the file's top level key, parsed by the frozen `v0`.
    let file = serde_json::to_value(&original).unwrap();
    assert_eq!(file["subtitle_offsets"][0]["account"], serde_json::json!(stamp));
    let parsed: v0::Session = serde_json::from_value(file).expect("the old build still reads the file");
    assert_eq!(parsed.account_token, original.account_token);
    assert_eq!(parsed.subtitle_offsets[0].items[0].offset_ms, 1_500);
    assert_eq!(parsed.subtitle_offsets[0].extensions.0.get("account"), Some(&serde_json::json!(stamp)));
}

/// **N7, at the persistence layer: a commit with a different account token carries the stored
/// preferences through.** boot.rs :1105-1107 starts a QR sign-in with the stored session still on
/// disk, so the new token replaces the old without a `ClearTenure`. `library_sorts` and
/// `last_library` are cleared by sign-out (SO) but are not stamped, so THIS layer, which only writes
/// what it is handed, keeps them. The login's own door clears them before it gets here
/// (`memory_tests::a_login_over_another_accounts_session_does_not_inherit_its_library_views`). The
/// stamped offsets do not pass either way: they stay in the file under the previous account's stamp,
/// and read as nothing to the new one.
#[test]
fn relogin_without_clear_keeps_library_views() {
    use super::prefs::ProfileMemory;
    let mut backend = helper();
    let mut transport = |request: Request| Ok(backend.dispatch(request));
    let first = rich();
    assert!(matches!(
        persistence::commit_session_with(&first, false, SaveAuthority::FreshReauthentication, 4, &mut transport),
        persistence::CanonicalCommit::Durable { .. }));
    let before = stored_public(&mut transport);

    let mut second = first.clone();
    second.account_token = "tier1-account-2".into();
    assert!(matches!(
        persistence::commit_session_with(&second, false, SaveAuthority::FreshReauthentication, 4, &mut transport),
        persistence::CanonicalCommit::Durable { .. }));
    let after = stored_public(&mut transport);

    for key in ["library_sorts", "last_library"] {
        assert!(before.preferences.get(key).is_some(), "control: `{key}` was stored");
        assert_eq!(after.preferences.get(key), before.preferences.get(key), "`{key}` is inherited by the next login");
    }
    let offsets: Vec<SubtitleOffsets> = serde_json::from_value(after.preferences["subtitle_offsets"].clone()).unwrap();
    let key = (MACHINE.to_string(), "rk-1".to_string());
    assert_eq!(offsets.get("u-7", &crate::plex::grant::account_key("tier1-account"), &key).map(|e| e.offset_ms), Some(1_500),
        "the previous account's record is still in the file, under its own stamp");
    assert!(offsets.get("u-7", &crate::plex::grant::account_key("tier1-account-2"), &key).is_none(),
        "and reads as nothing to the account that logged in over it");

    // The new account's first writes: the view it inherited is still there beside its own, and the
    // previous account's offsets are pruned by the write to the list.
    second.set_view_for("u-7", LibraryView {
        machine_id: MACHINE.into(), key: 2, sort: "titleSort".into(), desc: false, ..Default::default()
    });
    second.set_subtitle_offset_for("u-7", MACHINE, "rk-2", Some(900));
    assert!(matches!(
        persistence::commit_session_with(&second, false, SaveAuthority::Routine, 4, &mut transport),
        persistence::CanonicalCommit::Durable { .. }));
    let written = stored_public(&mut transport);
    let views = &written.preferences["library_sorts"][0]["libs"];
    assert_eq!(views.as_array().map(Vec::len), Some(2), "the inherited view stays beside the new one: {views}");
    assert_eq!(views[0]["sort"], serde_json::json!("addedAt"), "the previous account's view is still there");
    let offsets: Vec<SubtitleOffsets> = serde_json::from_value(written.preferences["subtitle_offsets"].clone()).unwrap();
    assert_eq!(offsets.len(), 1, "the previous account's offsets are pruned: {offsets:?}");
    assert_eq!(offsets[0].account, crate::plex::grant::account_key("tier1-account-2"));
    assert_eq!(offsets[0].items.len(), 1);
    assert_eq!(offsets[0].items[0].rating_key, "rk-2");
}
