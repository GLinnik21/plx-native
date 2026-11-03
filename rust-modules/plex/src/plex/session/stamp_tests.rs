//! The account stamp (`account_stamp`, `is_owner_stamp`, `adoptable_stamps`): the pure rules.
//!
//! The golden values below are computed with the exact rule of the format (`o1:` + the first eight
//! bytes of SHA-256 over `"plx-owner:" + owner_identity`, hex) and pinned, because a drift in the
//! format orphans every record a device has written: nothing re-derives an old stamp from anything.
//! The behaviour of the stamp across a re-sign-in is `memory_tests`; the owner's publication of it
//! is `plx_session`'s.
use super::stamp::adoptable_stamps;
use super::*;
use crate::plex::grant;

/// `owner_identity = id:7`.
pub(super) const OWNER_7: &str = "o1:19afc47bcea99412";
/// `owner_identity = id:8`.
pub(super) const OWNER_8: &str = "o1:38e53b36e68382ab";
/// `owner_identity = uuid:synthetic-owner-7` (the spelling of a roster written before it carried ids).
pub(super) const OWNER_7_UUID: &str = "o1:82eac2ef2031e0d2";

fn roster(id: i64, uuid: &str) -> Vec<HomeUserRef> {
    vec![HomeUserRef { id, uuid: uuid.into(), title: "Synthetic".into(), admin: true, ..Default::default() }]
}

fn session(token: &str, roster: Vec<HomeUserRef>) -> Session {
    Session { client_id: "cid-1".into(), account_token: token.into(), home_users: roster, ..Default::default() }
}

/// The three forms, one per situation: no token, a token and no nameable owner, a token and a
/// roster whose admin is named.
#[test]
fn the_stamp_is_empty_without_a_token_the_token_form_without_an_owner_and_the_owner_form_with_one() {
    let token_form = grant::account_key("token-a");
    assert_eq!(token_form.len(), 16, "control: the legacy form is 16 hex");

    assert_eq!(account_stamp(&session("", roster(7, "synthetic-owner-7"))), "", "no token: no stamp, whatever the roster says");
    assert_eq!(account_stamp(&session("", Vec::new())), "");

    assert_eq!(account_stamp(&session("token-a", Vec::new())), token_form, "no roster: the owner is unknown");
    let no_admin = vec![HomeUserRef { id: 7, uuid: "synthetic-owner-7".into(), admin: false, ..Default::default() }];
    assert_eq!(account_stamp(&session("token-a", no_admin)), token_form, "a roster without an admin entry names nobody");
    assert_eq!(account_stamp(&session("token-a", roster(0, ""))), token_form, "an admin that names neither id nor uuid");

    assert_eq!(account_stamp(&session("token-a", roster(7, "synthetic-owner-7"))), OWNER_7);
}

/// The format itself: golden values for the id form and for the uuid spelling of an old roster.
#[test]
fn the_owner_form_is_pinned_by_golden_values() {
    assert_eq!(account_stamp(&session("token-a", roster(7, ""))), OWNER_7);
    assert_eq!(account_stamp(&session("token-a", roster(8, "any-uuid"))), OWNER_8);
    assert_eq!(account_stamp(&session("token-a", roster(0, "synthetic-owner-7"))), OWNER_7_UUID);
    assert_eq!(OWNER_7.len(), 19, "`o1:` and 16 hex");
}

/// The point of the owner form: the same admin under any number of tokens is one stamp, and two
/// admins never share one.
#[test]
fn the_owner_form_ignores_the_token_and_tells_two_owners_apart() {
    let a = account_stamp(&session("token-a", roster(7, "synthetic-owner-7")));
    let b = account_stamp(&session("token-b", roster(7, "synthetic-owner-7")));
    let other = account_stamp(&session("token-a", roster(8, "synthetic-owner-8")));
    assert_eq!(a, b, "a new sign-in of the same owner is the same stamp");
    assert_ne!(a, other, "another admin is another stamp, whichever token it signs in with");
    assert_eq!(other, account_stamp(&session("token-z", roster(8, "synthetic-owner-8"))));
}

/// The owner family is `o<digits>:`; the token form, the empty stamp and near-misses are not in it.
/// A later `o2:` is foreign to this build but still the family.
#[test]
fn the_owner_family_is_a_versioned_prefix_and_nothing_else() {
    for stamp in [OWNER_7, OWNER_8, OWNER_7_UUID, "o2:0123456789abcdef", "o10:ab"] {
        assert!(is_owner_stamp(stamp), "{stamp}");
    }
    for stamp in ["", &grant::account_key("token-a"), "o:abcd", "o1", "o1abcd", "ox:abcd", "O1:abcd", " o1:abcd", "01:abcd"] {
        assert!(!is_owner_stamp(stamp), "{stamp:?}");
    }
}

/// A stamp of the owner form can never be mistaken for a token fingerprint (length and prefix), so
/// the two families cannot collide in a comparison.
#[test]
fn an_owner_stamp_never_equals_a_token_fingerprint() {
    for token in ["token-a", "token-b", "", "o1:19afc47bcea99412"] {
        let legacy = grant::account_key(token);
        assert_ne!(legacy, OWNER_7);
        assert!(legacy.len() == 16 || legacy.is_empty());
        assert!(!is_owner_stamp(&legacy));
    }
}

/// What a session may adopt: the token fingerprint of ITS token, and the other spelling of ITS
/// roster's admin, and only while its own stamp is the owner form. Nothing else, ever.
#[test]
fn adoptable_stamps_are_this_tokens_fingerprint_and_this_admins_other_spelling() {
    let both = HomeUserRef { id: 7, uuid: "synthetic-owner-7".into(), admin: true, ..Default::default() };
    let s = session("token-a", vec![both]);
    let mut adoptable = adoptable_stamps(&s);
    adoptable.sort();
    let mut expected = vec![grant::account_key("token-a"), OWNER_7_UUID.to_string()];
    expected.sort();
    assert_eq!(adoptable, expected, "the id form is the stamp; the token and the uuid spelling are adopted into it");
    assert!(!adoptable.contains(&account_stamp(&s)), "never the stamp itself");

    let id_only = session("token-a", roster(7, ""));
    assert_eq!(adoptable_stamps(&id_only), vec![grant::account_key("token-a")], "no uuid, no second spelling");

    let uuid_only = session("token-a", roster(0, "synthetic-owner-7"));
    assert_eq!(adoptable_stamps(&uuid_only), vec![grant::account_key("token-a")], "an id-less roster has one spelling");

    assert!(adoptable_stamps(&session("token-a", Vec::new())).is_empty(), "owner unknown: nothing to migrate toward");
    assert!(adoptable_stamps(&session("", roster(7, "synthetic-owner-7"))).is_empty(), "no token: no stamp, nothing to adopt");
    let other_owner = adoptable_stamps(&session("token-a", roster(8, "synthetic-owner-8")));
    assert!(!other_owner.iter().any(|stamp| stamp == OWNER_7 || stamp == OWNER_7_UUID),
        "another owner's stamps are never adoptable: {other_owner:?}");
}
