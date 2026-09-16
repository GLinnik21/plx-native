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
