//! `ProfileCreds` cache: PIN verifier hashing, remember/refresh-by-uuid, unusable entries,
//! and persistence across a save/load round trip.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn a_verifier_accepts_its_pin_and_nothing_else() {
    let v = PinVerifier::new("4821");
    assert!(v.verify("4821"));
    assert!(!v.verify("4822"));
    assert!(!v.verify(""));
    assert_eq!(v.salt.len(), 32, "16 random bytes, hex");
    assert_eq!(v.hash.len(), 64);
    assert_ne!(
        PinVerifier::new("4821").salt,
        v.salt,
        "two verifiers of one PIN never share a salt"
    );
}

#[test]
fn a_malformed_verifier_admits_nobody() {
    let none = PinVerifier::default();
    assert!(
        !none.verify(""),
        "an empty record must not match an empty PIN"
    );
    let mut v = PinVerifier::new("1234");
    v.iters = 0;
    assert!(!v.verify("1234"));
    let mut v = PinVerifier::new("1234");
    v.iters = u32::MAX;
    assert!(
        !v.verify("1234"),
        "an unbounded count is refused before it is run"
    );
    let mut v = PinVerifier::new("1234");
    v.iters = PinVerifier::MAX_ITERS + 1;
    assert!(!v.verify("1234"));
    let mut v = PinVerifier::new("1234");
    v.hash.pop();
    assert!(!v.verify("1234"));
    let mut v = PinVerifier::new("1234");
    v.salt = "zz".into();
    assert!(!v.verify("1234"));
}

#[test]
fn remember_replaces_by_uuid_and_the_cache_survives_a_round_trip() {
    let mut s = Session::default();
    s.remember_profile(creds("u-admin", "t1", Some("1111")));
    s.remember_profile(creds("u-kid", "t2", None));
    s.remember_profile(creds("u-admin", "t3", Some("2222")));
    s.remember_profile(creds("", "t4", None));
    assert_eq!(
        s.profiles.len(),
        2,
        "replace by uuid; an empty uuid is never cached"
    );
    assert_eq!(s.cached_profile("u-admin").unwrap().user.token, "t3");
    assert!(s
        .cached_profile("u-admin")
        .unwrap()
        .pin
        .as_ref()
        .unwrap()
        .verify("2222"));
    assert!(s.cached_profile("u-kid").unwrap().pin.is_none());
    assert!(s.cached_profile("u-nobody").is_none());
    assert!(s.cached_profile("").is_none());

    let json = serde_json::to_string(&s).unwrap();
    let back: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(back.profiles.len(), 2);
    assert!(back
        .cached_profile("u-admin")
        .unwrap()
        .pin
        .as_ref()
        .unwrap()
        .verify("2222"));
    assert_eq!(back.cached_profile("u-kid").unwrap().server.port, 32400);
}

#[test]
fn refreshing_the_active_record_follows_the_session_and_keeps_the_verifier() {
    let mut s = Session::default();
    s.remember_profile(creds("u-admin", "t1", Some("1111")));
    s.remember_profile(creds("u-kid", "t2", None));
    s.user = UserRef {
        uuid: "u-admin".into(),
        token: "t9".into(),
        ..Default::default()
    };
    s.server = ServerRef {
        machine_id: "m2".into(),
        address: "10.0.0.9".into(),
        port: 32400,
        token: "t9".into(),
        ..Default::default()
    };
    s.sources = vec![
        SourceRef {
            machine_id: "m2".into(),
            token: "t9".into(),
            address: "10.0.0.9".into(),
            port: 32400,
            ..Default::default()
        },
        SourceRef {
            machine_id: "share".into(),
            token: "s".into(),
            address: "10.0.0.7".into(),
            port: 32400,
            ..Default::default()
        },
    ];
    assert!(s.refresh_profile_record(), "a stale record changes");
    assert!(!s.refresh_profile_record(), "a current one does not");
    let c = s.cached_profile("u-admin").unwrap();
    assert_eq!(c.user.token, "t9");
    assert_eq!(c.server.machine_id, "m2");
    assert_eq!(c.sources.len(), 2, "the share found late is in the record");
    assert!(
        c.pin.as_ref().unwrap().verify("1111"),
        "the verifier survives"
    );
    assert_eq!(
        s.cached_profile("u-kid").unwrap().user.token,
        "t2",
        "other records untouched"
    );
    s.user.uuid = "u-nobody".into();
    assert!(!s.refresh_profile_record());
    assert_eq!(
        s.profiles.len(),
        2,
        "no record for the active user: nothing invented"
    );
}

#[test]
fn an_unusable_entry_is_not_offered() {
    let mut s = Session::default();
    s.remember_profile(creds("u-empty", "", None));
    assert!(
        s.cached_profile("u-empty").is_none(),
        "no token, nothing to seat"
    );
    let mut c = creds("u-noorigin", "t", None);
    c.server = ServerRef::default();
    s.remember_profile(c);
    assert!(
        s.cached_profile("u-noorigin").is_none(),
        "no primary, nothing to seat"
    );
}

/// The shapes the APP writes — a real primary with a tier and an https origin, a roster
/// entry with a credit, a verifier — survive `to_vec_pretty` → `load`'s re-parse. Written
/// after a device wiped its cache on boot (2026-09-06): `load` re-saves every plaintext
/// session it parses, so an entry the parser drops is gone after one launch.
#[test]
fn an_app_written_record_survives_the_parse_that_every_boot_re_saves() {
    let server = ServerRef {
        machine_id: "abc123".into(),
        address: "192.168.0.10".into(),
        port: 32400,
        token: "srv-tok".into(),
        tier: Some(Location::Local),
        origin_url: "https://192-168-0-10.abcdef.plex.direct:32400".into(),
        ..Default::default()
    };
    let source = SourceRef {
        machine_id: "abc123".into(),
        name: "nas".into(),
        shared_by: String::new(),
        owned: true,
        address: "192.168.0.10".into(),
        port: 32400,
        token: "srv-tok".into(),
        tier: Some(Location::Local),
        origin_url: "https://192-168-0-10.abcdef.plex.direct:32400".into(),
        ..Default::default()
    };
    let mut s = Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        server: server.clone(),
        ..Default::default()
    };
    s.user = UserRef {
        id: 7,
        uuid: "u-admin".into(),
        title: "Admin".into(),
        thumb: "https://plex.tv/users/x/avatar?c=1".into(),
        token: "user-tok".into(),
    };
    s.sources = vec![source.clone()];
    s.remember_profile(ProfileCreds {
        uuid: "u-admin".into(),
        user: s.user.clone(),
        server,
        sources: vec![source],
        pin: Some(PinVerifier::new("1234")),
    });
    let bytes = serde_json::to_vec_pretty(&s).unwrap();
    let back: Session = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        back.profiles.len(),
        1,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let c = back
        .cached_profile("u-admin")
        .expect("the record is offered back");
    assert_eq!(c.server.tier, Some(Location::Local));
    assert!(c.pin.as_ref().unwrap().verify("1234"));
}

/// A file written before the field existed parses with an empty cache, never fails.
#[test]
fn a_legacy_file_has_an_empty_cache() {
    let back: Session =
        serde_json::from_str(r#"{"client_id":"c","account_token":"a"}"#).unwrap();
    assert!(back.profiles.is_empty());
    let back: Session = serde_json::from_str(
        r#"{"client_id":"c","profiles":[{"uuid":"u","user":{"token":"t"},"server":{"address":"10.0.0.1","port":"nope"}}, 7]}"#,
    )
    .unwrap();
    assert!(
        back.profiles.is_empty(),
        "a malformed entry costs the entry, not the session"
    );
}
