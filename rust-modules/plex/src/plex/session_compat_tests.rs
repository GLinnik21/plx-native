//! Session-file backward compatibility: origins, tier, playback quality, and auto-sign-in.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **THE COMPATIBILITY GATE: a session file written by 0.4.1 must still boot.**
///
/// That build knew nothing about origins — it wrote `address` and `port` and no more — and
/// every signed-in television in the world is holding one of these files right now. If
/// `Session::server` failed to carry through, the cost is not a degraded feature: `app.rs`'s
/// boot gate runs on `can_go_local()`, so the app would land on the QR sign-in screen on
/// **every boot for every existing user**, which is a silent sign-out that no test above this
/// one can see (the roster lists are soft-parsed — `de_soft_vec` — but the primary is not a
/// disposable entry, and nothing soft-parses a MISSING field into a different meaning).
///
/// Written as literal 0.4.1-shaped JSON rather than by serialising a `Session`, because the
/// thing under test is precisely that today's struct is not what wrote those bytes.
#[test]
fn a_session_file_written_before_origins_existed_still_boots_as_plain_http() {
    // Byte-for-byte the shape 0.4.1 wrote: no `origin` on the primary, none on any source.
    let v041 = r#"{"client_id":"cid-1","account_token":"acct",
        "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                  "port":32400,"token":"tok-own"},
        "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
        "sources":[
          {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
           "address":"192.168.0.10","port":32400,"token":"tok-own"},
          {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
           "address":"203.0.113.9","port":31234,"token":"tok-share"}]}"#;
    let s: Session = serde_json::from_str(v041).expect("a 0.4.1 session file still parses");

    // the boot gate itself — this is the assertion whose failure is the silent sign-out
    assert!(
        s.can_go_local(),
        "a 0.4.1 session must still reach Home without a QR code"
    );

    // …and it boots against exactly the address it always did, as plain http
    let o = s.server.origin();
    assert_eq!(o.base(), "http://192.168.0.10:32400");
    assert_eq!((o.host(), o.port()), ("192.168.0.10", 32400));
    assert!(!o.is_tls(), "nothing in that file ever meant TLS");

    // every roster entry too, including the share on its non-default port
    assert!(
        s.sources.iter().all(|x| x.dialable()),
        "{:#?}",
        s.sources.len()
    );
    assert_eq!(
        s.owned_source().unwrap().origin().unwrap().base(),
        "http://192.168.0.10:32400"
    );
    assert_eq!(
        s.source("bbbb2222").unwrap().origin().unwrap().base(),
        "http://203.0.113.9:31234"
    );
}

#[test]
fn a_session_written_before_profile_plex_tv_credentials_loads_without_reauth() {
    let session: Session = serde_json::from_str(two_server_json()).unwrap();
    assert!(session.can_go_local());
    assert_eq!(session.user.plex_tv_token, None);
}

#[test]
fn a_malformed_or_empty_profile_plex_tv_credential_is_harmless() {
    for malformed in ["null", "7", "{}", "[]", r#""""#] {
        let json = format!(r#"{{"client_id":"c","account_token":"a","user":{{
            "uuid":"u","token":"pms","plex_tv_token":{malformed}}}}}"#);
        let session: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(session.user.token, "pms");
        assert_eq!(session.user.plex_tv_token, None, "{malformed}");
    }
}

#[test]
fn profile_plex_tv_credential_stays_only_in_the_protected_canonical_half() {
    let mut session = Session::default();
    session.user.uuid = "managed".into();
    session.user.plex_tv_token = Some("plex-tv-secret".into());
    session.remember_profile(ProfileCreds { uuid: "managed".into(),
        user: session.user.clone(), ..Default::default() });
    let (public, protected) = split_canonical(&session).unwrap();
    assert!(!serde_json::to_string(&public).unwrap().contains("plex_tv_token"));
    assert!(!serde_json::to_string(&public).unwrap().contains("plex-tv-secret"));
    assert!(protected.contains("plex_tv_token"));
    let joined = join_canonical(&public, &protected).unwrap();
    assert_eq!(joined.user.plex_tv_token.as_deref(), Some("plex-tv-secret"));
    assert_eq!(joined.profiles[0].user.plex_tv_token.as_deref(), Some("plex-tv-secret"));

    assert!(protected_matches(&session, &protected));
    session.user.plex_tv_token = Some("rotated-secret".into());
    assert!(!protected_matches(&session, &protected), "credential rotation is a protected change");
}

#[test]
fn plex_tv_credential_never_borrows_owner_or_pms_tokens_for_a_managed_profile() {
    let _serial = plx_base::testlock::serial();
    let _temp = super::test_support::TempSession::new("managed-plex-tv-credential");
    let mut stored = dialable_home(2, "u-1", false);
    stored.account_token = "owner-account".into();
    stored.user.token = "managed-pms".into();
    save(&stored);

    let mut captured = stored.user.clone();
    captured.plex_tv_token = Some("managed-plex-tv".into());
    assert_eq!(plex_tv_credential(&captured).as_deref(), Some("managed-plex-tv"));
    captured.plex_tv_token = None;
    assert_eq!(plex_tv_credential(&captured), None);

    stored.home_users.clear();
    save(&stored);
    assert_eq!(plex_tv_credential(&captured), None,
        "unknown legacy scope must not borrow the owner or PMS credential");
}

#[test]
fn legacy_owner_credential_fallback_requires_admin_scope_and_the_same_stored_user() {
    let _serial = plx_base::testlock::serial();
    let _temp = super::test_support::TempSession::new("legacy-owner-plex-tv-credential");
    let mut stored = dialable_home(2, "u-0", false);
    stored.account_token = "owner-account".into();
    stored.user.token = "owner-pms".into();
    save(&stored);
    assert_eq!(plex_tv_credential(&stored.user).as_deref(), Some("owner-account"));

    let mut mismatched = stored.user.clone();
    mismatched.uuid = "u-other".into();
    assert_eq!(plex_tv_credential(&mismatched), None);
}

#[test]
fn legacy_single_user_owner_with_no_identity_fields_uses_the_account_credential() {
    let _serial = plx_base::testlock::serial();
    let _temp = super::test_support::TempSession::new("legacy-zero-id-owner-plex-tv-credential");
    let mut stored = dialable_home(0, "", false);
    stored.account_token = "owner-account".into();
    stored.user.id = 0;
    stored.user.uuid.clear();
    stored.user.plex_tv_token = None;
    save(&stored);

    assert_eq!(plex_tv_credential(&stored.user).as_deref(), Some("owner-account"));
}

/// Tier persistence is additive: old files have no field, and a value written by a future
/// build must not make the PRIMARY fail to parse (which would route a signed-in TV to QR).
#[test]
fn a_stored_tier_round_trips_and_unknown_tiers_degrade_to_unknown() {
    let legacy: Session =
        serde_json::from_str(two_server_json()).expect("the legacy shape parses");
    assert_eq!(legacy.server.tier, None);
    assert!(legacy.sources.iter().all(|s| s.tier.is_none()));

    let json = r#"{"client_id":"c","server":{"address":"192.0.2.10","port":32400,
                  "token":"t","tier":"future-tier"},
                "sources":[{"machine_id":"m","address":"192.0.2.10","port":32400,
                  "token":"t","tier":"relay"}]}"#;
    let s: Session =
        serde_json::from_str(json).expect("an unknown primary tier is soft metadata");
    assert!(
        s.can_go_local(),
        "unknown tier metadata cannot silently sign the device out"
    );
    assert_eq!(s.server.tier, None);
    assert_eq!(
        s.sources[0].tier,
        Some(super::super::probe::Location::Relay)
    );

    let encoded = serde_json::to_value(ServerRef {
        tier: Some(super::super::probe::Location::Remote),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(
        encoded["tier"], "remote",
        "the file stays human-readable and stable"
    );
}

/// A missing quality field is an OLD install, not an invitation to adopt a new default. The
/// literal is deliberately pre-feature JSON; serialising today's `Session` would always write
/// whatever today's struct thinks and could not grade the migration boundary.
#[test]
fn a_legacy_session_with_no_quality_stays_original() {
    let s: Session = serde_json::from_str(two_server_json()).expect("the legacy file parses");
    assert_eq!(
        s.playback_quality, None,
        "absence remains distinguishable on disk"
    );
    assert_eq!(
        s.playback_quality(),
        PlaybackQuality::Original,
        "legacy playback does not become Auto"
    );
}

/// Quality is a preference beside credentials, never a reason to discard them. This is the
/// scalar counterpart of the roster/tier soft parsers: unknown future names, null and the
/// wrong JSON shape all keep the session and conservatively mean Original.
#[test]
fn invalid_or_future_quality_is_soft_and_conservative() {
    for value in [r#""future_auto_v2""#, "null", r#"{"mode":"auto"}"#, "42"] {
        let json = format!(
            r#"{{"client_id":"c","account_token":"acct",
                 "server":{{"address":"192.168.0.10","port":32400,"token":"t"}},
                 "playback_quality":{value}}}"#
        );
        let s: Session = serde_json::from_str(&json)
            .expect("bad preference metadata cannot fail credentials");
        assert_eq!(s.account_token, "acct");
        assert!(s.can_go_local());
        assert_eq!(s.playback_quality(), PlaybackQuality::Original, "{value}");
    }
}

#[test]
fn every_explicit_quality_mode_round_trips_by_stable_name() {
    let cases = [
        (PlaybackQuality::Auto, "auto"),
        (PlaybackQuality::Original, "original"),
        (PlaybackQuality::P1080High, "1080p_20_mbps"),
        (PlaybackQuality::P1080, "1080p_8_mbps"),
        (PlaybackQuality::P720, "720p_4_mbps"),
        (PlaybackQuality::P720Low, "720p_2_mbps"),
        (PlaybackQuality::P480, "480p_720_kbps"),
    ];
    for (quality, wire) in cases {
        let s = Session {
            playback_quality: Some(quality),
            ..Session::default()
        };
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["playback_quality"], wire);
        let again: Session = serde_json::from_value(json).unwrap();
        assert_eq!(again.playback_quality(), quality);
    }
}

#[test]
fn an_absent_trailer_autoplay_field_stays_on() {
    let parsed: Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
    assert!(parsed.trailer_autoplay());
    let off: Session = serde_json::from_str(r#"{"client_id":"c","trailer_autoplay":false}"#).unwrap();
    assert!(!off.trailer_autoplay());
}

const MINIMAL_PROTECTED_AUTH: &str = r#"{"format":"plxnative-session-auth","version":1,"account_token":"tok","server":{},"user":{},"home_users":[],"sources":[],"extensions":{}}"#;

/// #92's contract is "absence is on". `join_canonical` reconstructs a `Session` from a
/// canonical `PublicPayload` + a decrypted protected auth string — a null/absent
/// `preferences` blob must not silently turn the hero trailer off.
#[test]
fn join_canonical_with_null_preferences_keeps_trailer_autoplay_on() {
    let public = plx_platform::storage::state::PublicPayload::default();
    assert_eq!(public.preferences, Value::Null);
    let session = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    assert!(session.trailer_autoplay());
}

#[test]
fn join_canonical_with_explicit_false_keeps_trailer_autoplay_off() {
    let mut public = plx_platform::storage::state::PublicPayload::default();
    public.preferences = serde_json::json!({"trailer_autoplay": false});
    let session = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    assert!(!session.trailer_autoplay());
}

/// `public_session` is the Locked/protected-bundle snapshot constructor — it must surface the
/// same public preference even though it never sees the decrypted credentials.
#[test]
fn public_session_with_null_preferences_keeps_trailer_autoplay_on() {
    let public = plx_platform::storage::state::PublicPayload::default();
    assert_eq!(public.preferences, Value::Null);
    let session = public_session(&public);
    assert!(session.trailer_autoplay());
}

#[test]
fn public_session_with_explicit_false_keeps_trailer_autoplay_off() {
    let mut public = plx_platform::storage::state::PublicPayload::default();
    public.preferences = serde_json::json!({"trailer_autoplay": false});
    let session = public_session(&public);
    assert!(!session.trailer_autoplay());
}

#[test]
fn a_fresh_install_defaults_to_auto_only_after_readiness() {
    assert_eq!(
        PlaybackQuality::fresh_default(false),
        PlaybackQuality::Original
    );
    assert_eq!(PlaybackQuality::fresh_default(true), PlaybackQuality::Auto);

    let mut absent = Session::default();
    seed_fresh_quality(&mut absent, false, true);
    assert_eq!(
        absent.playback_quality,
        Some(PlaybackQuality::Auto),
        "only the no-file path may adopt a newly ready Auto default"
    );

    // Literal legacy JSON with neither field. Its empty client id will be repaired by `load`,
    // but that is not evidence of a fresh install and must not seed Auto even after readiness.
    let mut legacy: Session =
        serde_json::from_str(r#"{"account_token":"still-a-real-file"}"#).unwrap();
    seed_fresh_quality(&mut legacy, true, true);
    assert!(legacy.client_id.is_empty());
    assert_eq!(legacy.playback_quality, None);
    assert_eq!(legacy.playback_quality(), PlaybackQuality::Original);
}

/// A missing Automatically Sign In field is today's picker, not an invitation to skip it.
#[test]
fn a_legacy_session_with_no_auto_sign_in_stays_off() {
    let s: Session = serde_json::from_str(two_server_json()).expect("the legacy file parses");
    assert!(
        !s.auto_sign_in(),
        "absence is off, which is the picker every existing television already knows"
    );
}

/// Garbage on a preference switch must not sign the device out.
#[test]
fn invalid_auto_sign_in_is_soft_and_off() {
    for value in [r#""yes""#, "null", "1", r#"{"on":true}"#] {
        let json = format!(
            r#"{{"client_id":"c","account_token":"acct",
                 "server":{{"address":"192.168.0.10","port":32400,"token":"t"}},
                 "auto_sign_in":{value}}}"#
        );
        let s: Session = serde_json::from_str(&json)
            .expect("bad preference metadata cannot fail credentials");
        assert_eq!(s.account_token, "acct");
        assert!(s.can_go_local());
        assert!(!s.auto_sign_in(), "{value}");
    }
}

/// The subtitle tone's file contract, in both directions: absence is WHITE (every file written
/// before the field existed must keep drawing what it drew), a spelling this build does not know
/// is white rather than a parse failure that would cost the credentials, and every rung survives
/// the wire under its own explicit name.
#[test]
fn the_subtitle_tone_is_white_when_absent_or_unknown_and_round_trips_every_rung() {
    let parsed: Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
    assert_eq!(parsed.subtitle_tone(), SubtitleTone::White);
    for damaged in [r#""mauve""#, "7", "null", r#"{"a":1}"#] {
        let text = format!(r#"{{"client_id":"c","subtitle_tone":{damaged}}}"#);
        let parsed: Session = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.client_id, "c", "a bad tone must not cost the session: {damaged}");
        assert_eq!(parsed.subtitle_tone(), SubtitleTone::White, "{damaged}");
    }
    for (tone, wire) in [
        (SubtitleTone::White, "white"),
        (SubtitleTone::Silver, "grey_85"),
        (SubtitleTone::LightGrey, "grey_70"),
        (SubtitleTone::Grey, "grey_55"),
        (SubtitleTone::DarkGrey, "grey_40"),
        (SubtitleTone::Charcoal, "grey_28"),
    ] {
        let json = serde_json::to_value(Session::default().with_subtitle_tone(tone)).unwrap();
        assert_eq!(json["subtitle_tone"], wire);
        let again: Session = serde_json::from_value(json).unwrap();
        assert_eq!(again.subtitle_tone(), tone);
    }
    // the ladder is every rung once, lightest first, and the index is its own inverse
    assert_eq!(SubtitleTone::LADDER[0], SubtitleTone::White);
    for (i, tone) in SubtitleTone::LADDER.iter().enumerate() {
        assert_eq!(tone.index() as usize, i);
        assert_eq!(SubtitleTone::from_index(i as u8), *tone);
        assert!(!tone.label().is_empty());
    }
    assert_eq!(SubtitleTone::from_index(200), SubtitleTone::White, "out of range is white");
}

/// The tone lives in the PUBLIC preferences half of the canonical split, so it must survive
/// `split_public` → `join_canonical` and the locked-bundle `public_session` snapshot alike.
#[test]
fn the_subtitle_tone_survives_the_canonical_split() {
    let session = Session::default().with_subtitle_tone(SubtitleTone::DarkGrey);
    let public = split_public(&session).unwrap();
    assert_eq!(public.preferences["subtitle_tone"], "grey_40");
    let joined = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    assert_eq!(joined.subtitle_tone(), SubtitleTone::DarkGrey);
    assert_eq!(public_session(&public).subtitle_tone(), SubtitleTone::DarkGrey);
    // …and a null preferences blob is white, not a failure
    let empty = plx_platform::storage::state::PublicPayload::default();
    assert_eq!(public_session(&empty).subtitle_tone(), SubtitleTone::White);
}

/// Remembered library views (#278 the sort, #441 everything else the Library toolbar changes)
/// live in the PUBLIC preferences half too, so the DB8 path — not only the plaintext file the
/// host suite writes — must carry them through `split_public` → `join_canonical` and the
/// locked-bundle snapshot; a session that never changed a view must not grow the key at all; and
/// the key is still `library_sorts`, the name an older build reads back on a rollback.
#[test]
fn remembered_library_sorts_survive_the_canonical_split() {
    let mut session = Session::default();
    assert!(split_public(&session).unwrap().preferences.get("library_sorts").is_none());
    let view = LibraryView {
        machine_id: "machine".into(), key: 3, sort: "viewCount".into(), desc: true,
        unwatched: true, genre: "28".into(), listing: "episodes".into(),
        listing_sort: "addedAt".into(), listing_desc: true, ..Default::default()
    };
    session.set_view_for("u-sorter", view.clone());
    let public = split_public(&session).unwrap();
    assert!(public.preferences.get("library_views").is_none(),
        "the on-disk key keeps the name an older build reads");
    let joined = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    for restored in [&joined, &public_session(&public)] {
        assert_eq!(restored.views_for("u-sorter").and_then(|views| views.get("machine", 3)),
            Some(&view));
        assert!(restored.views_for("").is_none(), "another profile's record is its own");
    }
    // a malformed entry costs itself, never the preferences beside it
    let mut public = public;
    public.preferences["library_sorts"][0]["libs"].as_array_mut().unwrap()
        .push(serde_json::json!({"machine_id": 7}));
    public.preferences["library_sorts"].as_array_mut().unwrap().push(serde_json::json!("bad"));
    let joined = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    assert_eq!(joined.library_views.len(), 1);
    assert_eq!(joined.views_for("u-sorter").map(|views| views.libs.len()), Some(1));
}

/// A `library_sorts` entry written before #441 (the sort and nothing else) loads as a view that
/// is that sort and defaults for every field added since, and serializes back byte for byte —
/// the compatibility gate for both an upgrade and a rollback. Written as a literal blob, because
/// what is under test is that today's struct is not what wrote those bytes.
#[test]
fn a_pre_441_library_sorts_entry_loads_as_a_sort_only_view() {
    let before = r#"[{"user":"u-sorter","libs":[{"machine_id":"machine","key":3,"sort":"viewCount","desc":true}]}]"#;
    let expected = LibraryView {
        machine_id: "machine".into(), key: 3, sort: "viewCount".into(), desc: true,
        ..Default::default()
    };
    // as the canonical `preferences` blob …
    let preferences = format!(r#"{{"library_sorts":{before}}}"#);
    let public = plx_platform::storage::state::PublicPayload {
        preferences: serde_json::from_str(&preferences).unwrap(),
        ..Default::default()
    };
    let joined = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    assert_eq!(joined.views_for("u-sorter").and_then(|views| views.get("machine", 3)),
        Some(&expected));
    assert_eq!(serde_json::to_string(&joined.library_views).unwrap(), before);
    // … and as the plaintext session file
    let file = format!(r#"{{"client_id":"cid","library_sorts":{before}}}"#);
    let session: Session = serde_json::from_str(&file).unwrap();
    assert_eq!(session.views_for("u-sorter").and_then(|views| views.get("machine", 3)),
        Some(&expected));
    assert_eq!(serde_json::to_string(&session.library_views).unwrap(), before);
}

/// A malformed NEW field (`"unwatched":"yes"`) costs that one entry — the soft parse drops it —
/// never the neighbouring entries, the profile's other libraries or the credentials.
#[test]
fn a_malformed_new_view_field_costs_only_its_entry() {
    let file = r#"{"client_id":"cid","account_token":"acct","library_sorts":[{"user":"u","libs":[
        {"machine_id":"machine","key":1,"sort":"addedAt","desc":true},
        {"machine_id":"machine","key":2,"unwatched":"yes"},
        {"machine_id":"machine","key":3,"unwatched":true,"genre":"28"}]}]}"#;
    let session: Session = serde_json::from_str(file).unwrap();
    assert_eq!(session.account_token, "acct");
    let views = session.views_for("u").unwrap();
    assert!(views.get("machine", 1).is_some());
    assert!(views.get("machine", 2).is_none(), "the malformed entry is the one that is dropped");
    assert_eq!(views.get("machine", 3).map(|view| (view.unwatched, view.genre.as_str())),
        Some((true, "28")));
}

/// Remembered per-item subtitle offsets survive the canonical split the same way library sorts
/// do, and for the same reason: they live in the PUBLIC preferences half, so DB8 must carry them
/// too, and a session that never tuned one must not grow the key at all.
#[test]
fn remembered_subtitle_offsets_survive_the_canonical_split() {
    let mut session = Session::default();
    assert!(split_public(&session).unwrap().preferences.get("subtitle_offsets").is_none());
    session.set_subtitle_offset_for("u-tuner", "machine", "rk-1", Some(1_500));
    let public = split_public(&session).unwrap();
    let joined = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    for restored in [&joined, &public_session(&public)] {
        assert_eq!(restored.subtitle_offset_for("u-tuner", "machine", "rk-1"), Some(1_500));
        assert!(restored.subtitle_offset_for("", "machine", "rk-1").is_none(), "another profile's record is its own");
        assert!(restored.subtitle_offset_for("u-tuner", "machine", "rk-2").is_none(), "another item's record is its own");
    }
    // a malformed entry costs itself, never the preferences beside it
    let mut public = public;
    public.preferences["subtitle_offsets"][0]["items"].as_array_mut().unwrap()
        .push(serde_json::json!({"machine_id": 7}));
    public.preferences["subtitle_offsets"].as_array_mut().unwrap().push(serde_json::json!("bad"));
    let joined = join_canonical(&public, MINIMAL_PROTECTED_AUTH).unwrap();
    assert_eq!(joined.subtitle_offsets.len(), 1);
    assert_eq!(joined.subtitle_offsets[0].items.len(), 1);
}

/// The per-profile list is bounded (oldest evicted first), and setting the offset back to
/// Original (0) forgets the entry instead of recording a no-op correction.
#[test]
fn subtitle_offsets_are_capped_and_zero_forgets_the_entry() {
    use crate::plex::session::SubtitleOffsets;
    let mut offsets = SubtitleOffsets::default();
    for i in 0..(SubtitleOffsets::CAP + 5) {
        offsets.set("m", &format!("rk-{i}"), Some(1_000 + i as i64));
    }
    assert_eq!(offsets.items.len(), SubtitleOffsets::CAP);
    assert!(offsets.get("m", "rk-0").is_none(), "the oldest entries are evicted");
    assert!(offsets.get("m", &format!("rk-{}", SubtitleOffsets::CAP + 4)).is_some());
    // re-tuning the same item refreshes it rather than duplicating it
    offsets.set("m", "rk-10", Some(-300));
    assert_eq!(offsets.items.len(), SubtitleOffsets::CAP);
    assert_eq!(offsets.items.last().map(|e| (e.rating_key.as_str(), e.offset_ms)), Some(("rk-10", -300)));
    // Original (0) forgets rather than recording a no-op
    offsets.set("m", "rk-10", Some(0));
    assert!(offsets.get("m", "rk-10").is_none());
    // no machine id or ratingKey is ever recorded
    offsets.set("", "rk-x", Some(500));
    offsets.set("m", "", Some(500));
    assert!(offsets.get("", "rk-x").is_none());
    assert!(offsets.get("m", "").is_none());
}

/// **A session written before household evidence existed reads as TODAY's behaviour, not worse.**
///
/// `SourceRef::home`/`owner_id` are absent from every file on every television right now, and the
/// deserializer defaults them to `false`/`0`. That pair is chosen, not inherited: with no
/// evidence, [`crate::plex::is_household`] answers exactly what raw `owned` answers — which is
/// what the whole app did before the field existed — and the record self-corrects on the next
/// `/api/v2/resources`, which every boot and every profile switch performs.
///
/// It is graded rather than merely documented because the failure is silent in the wrong
/// direction: a household server defaulting to "outside" is the bug this evidence exists to
/// remove, and a future serde change that made `home` default `true`, or that dropped the
/// `#[serde(default)]`, would sign the device out or invent a household on the strength of
/// nothing.
#[test]
fn a_session_written_before_household_evidence_falls_back_to_raw_owned() {
    let legacy = r#"{"client_id":"cid-1","account_token":"acct",
        "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                  "port":32400,"token":"tok-own"},
        "user":{"id":7,"uuid":"u-7","title":"Gleb","thumb":"","token":"tok-user"},
        "home_users":[{"id":111111,"uuid":"u-admin","title":"admin","admin":true}],
        "sources":[
          {"machine_id":"aaaa1111","name":"Mac mini","shared_by":"","owned":true,
           "address":"192.168.0.10","port":32400,"token":"tok-own"},
          {"machine_id":"bbbb2222","name":"nas-home","shared_by":"friend","owned":false,
           "address":"203.0.113.9","port":31234,"token":"tok-share"}]}"#;
    let s: Session = serde_json::from_str(legacy).expect("a pre-evidence session file parses");

    assert!(
        s.sources.iter().all(|x| !x.home && x.owner_id == 0),
        "no file on disk carries either field yet"
    );
    let household = s.household_ids();
    assert_eq!(household, [111_111], "the roster is unaffected by the source shape");
    for source in &s.sources {
        assert_eq!(
            crate::plex::is_household(
                crate::plex::GrantEvidence {
                    owned: source.owned, home: source.home, owner_id: source.owner_id,
                }
                .grant(),
                &household,
            ),
            source.owned,
            "with no evidence the verdict is raw `owned` — {}", source.machine_id,
        );
    }
}

/// **The subtitle timing offset is not in the session file.** A timing error belongs to one
/// subtitle track against one media file, so the offset lives and dies with the playback
/// (`player::set_subtitle_offset`) and a stored one would put the last film's correction on the
/// next film. Neither the flat session nor the DB8 public preferences carry the key.
#[test]
fn the_session_file_carries_no_subtitle_offset() {
    let json = serde_json::to_value(Session::default()).unwrap();
    assert!(json.get("subtitle_offset_ms").is_none(), "the session file names no offset: {json}");
    let public = split_public(&Session::default()).unwrap();
    assert!(
        public.preferences.get("subtitle_offset_ms").is_none(),
        "the public preferences name no offset: {}",
        public.preferences
    );
}

// Moved from `i18n`'s tests: it grades `Session`'s `language` field, and `i18n` (a layer below
// `plex`) cannot name `Session`.
#[test]
fn preferences_soft_parse_without_losing_the_session() {
    use plx_platform::i18n::Preference;
    for raw in [
        r#"{}"#,
        r#"{"language":"future"}"#,
        r#"{"language":42}"#,
        r#"{"language":null}"#,
    ] {
        let s: crate::plex::session::Session = serde_json::from_str(raw).unwrap();
        assert_eq!(s.language, Preference::System);
    }
    let s: crate::plex::session::Session = serde_json::from_str(r#"{"language":"be"}"#).unwrap();
    assert_eq!(s.language, Preference::Be);
    assert_eq!(serde_json::to_value(&s).unwrap()["language"], "be");
}
