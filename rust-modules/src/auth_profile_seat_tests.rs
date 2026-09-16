//! Who's-watching profile seating: offline PIN activation from the cached roster, the
//! picker's resume/detach policy, and profile-switch UI banner behavior.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The roster's uuid keys the record, whatever the `/switch` body says — an empty or
/// differing response uuid must not produce an entry the next pick cannot find.
#[test]
fn a_seated_profile_is_recorded_under_the_roster_uuid() {
    let u = crate::plex::account::SwitchedUser {
        uuid: String::new(),
        ..Default::default()
    };
    assert_eq!(seated_uuid(&u, &tile("u-kid", false)), "u-kid");
    let u = crate::plex::account::SwitchedUser {
        uuid: "u-other".into(),
        ..Default::default()
    };
    assert_eq!(seated_uuid(&u, &tile("u-kid", false)), "u-kid");
    assert_eq!(
        seated_uuid(&u, &tile("", false)),
        "u-other",
        "no roster uuid: the response's"
    );
}

/// The plaintext twin may answer first, but a store build cannot make it live: only an
/// https origin is activated there, while a developer build keeps its lab plaintext.
#[test]
fn a_store_build_never_makes_a_plaintext_origin_live() {
    let plain = Origin::http("192.168.0.10", 32400);
    let tls = Origin::parse("https://192-168-0-10.abc.plex.direct:32400").unwrap();
    assert!(!activation_allowed_by_policy(&plain, false));
    assert!(activation_allowed_by_policy(&tls, false));
    assert!(
        activation_allowed_by_policy(&plain, true),
        "a developer build keeps its lab server"
    );
}

/// The outage that motivated the cache (2026-09-06): the active profile is the PIN-protected
/// admin, plex.tv is unreachable, and the PIN has to be checked by this television.
#[test]
fn a_protected_profile_is_seated_offline_on_its_pin_and_refused_on_any_other() {
    let stored = cached_session(Some("4821"));
    match offline_activation(&stored, &tile("u-admin", true), Some("4821")) {
        OfflineSwitch::Seat(next) => {
            assert_eq!(next.user.token, "admin-token");
            assert_eq!(
                next.client_id, "cid",
                "the account and its roster ride through"
            );
            assert_eq!(next.account_token, "acct");
        }
        _ => panic!("the right PIN seats the cached profile"),
    }
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), Some("0000")),
        OfflineSwitch::PinDenied
    ));
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), None),
        OfflineSwitch::PinDenied
    ));
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), Some("")),
        OfflineSwitch::PinDenied
    ));
}

/// A protected profile whose record predates the verifier cannot be checked, so it is not
/// seated — "no cache", never "no PIN".
#[test]
fn a_protected_profile_cached_without_a_verifier_is_not_seated() {
    let stored = cached_session(None);
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), Some("4821")),
        OfflineSwitch::NoCache
    ));
}

#[test]
fn an_unprotected_cached_profile_is_seated_on_the_pick_alone() {
    let stored = cached_session(Some("4821"));
    match offline_activation(&stored, &tile("u-kid", false), None) {
        OfflineSwitch::Seat(next) => {
            assert_eq!(next.user.uuid, "u-kid");
            assert_eq!(next.user.token, "kid-token");
            assert_eq!(next.server.token, "kid-token");
            assert_eq!(next.sources[0].token, "kid-token");
            assert_eq!(
                next.profiles.len(),
                2,
                "the cache itself is kept for the next pick"
            );
        }
        _ => panic!("an unprotected cached profile seats without a network"),
    }
}

#[test]
fn a_profile_this_television_never_seated_online_has_nothing_to_seat() {
    let stored = cached_session(Some("4821"));
    assert!(matches!(
        offline_activation(&stored, &tile("u-guest", false), None),
        OfflineSwitch::NoCache
    ));
    assert!(matches!(
        offline_activation(&Session::default(), &tile("u-admin", true), Some("4821")),
        OfflineSwitch::NoCache
    ));
}

/// The seating paths that never see a PIN still write the record for a PIN-free profile,
/// so a session stored before the cache existed becomes seatable offline on first use.
#[test]
fn seating_an_unprotected_active_profile_records_it_and_a_protected_one_is_left_to_the_switch()
{
    let mut s = cached_session(None);
    s.profiles.clear();
    s.home_users = vec![session::HomeUserRef {
        uuid: "u-admin".into(),
        protected: false,
        ..Default::default()
    }];
    remember_unprotected_active(&mut s);
    assert_eq!(s.profiles.len(), 1);
    assert_eq!(
        s.cached_profile("u-admin").unwrap().user.token,
        "admin-token"
    );

    let mut p = cached_session(None);
    p.profiles.clear();
    p.home_users = vec![session::HomeUserRef {
        uuid: "u-admin".into(),
        protected: true,
        ..Default::default()
    }];
    remember_unprotected_active(&mut p);
    assert!(
        p.profiles.is_empty(),
        "no PIN in hand, no verifier to write"
    );

    let mut none = Session::default();
    remember_unprotected_active(&mut none);
    assert!(
        none.profiles.is_empty(),
        "an account without Plex Home names no profile"
    );
}

#[test]
fn picker_policy_defaults_detachment_and_refusal_reasons_remain_exact() {
    for (picker, protected, allowed) in [
        (Picker::Boot, true, false), (Picker::Boot, false, true),
        (Picker::ChangeProfile, true, false), (Picker::ChangeProfile, false, false),
        (Picker::SignedIn, true, false), (Picker::SignedIn, false, false),
    ] { assert_eq!(may_resume(picker, protected), allowed); }
    assert_eq!(Picker::default(), Picker::Boot);
    assert!(detaches_active_profile(Picker::ChangeProfile));
    assert!(!detaches_active_profile(Picker::Boot));
    assert!(!detaches_active_profile(Picker::SignedIn));
    let adult = signed_in_as("u-adult");
    let kid = signed_in_as("u-kid");
    assert!(adult.active_profile_is_protected());
    assert!(!kid.active_profile_is_protected());
    assert_eq!(refusal_reason(Picker::ChangeProfile, &kid),
        "auth: BACK refused — the Change-profile picker is a root");
    assert_eq!(refusal_reason(Picker::Boot, &adult),
        "auth: BACK refused — the stored profile is PIN-protected");
    let unchosen = Session { user: UserRef::default(), ..adult };
    assert_eq!(refusal_reason(Picker::SignedIn, &unchosen),
        "auth: BACK refused — no profile has been chosen on this device yet");
}

/// **A wrong PIN must not follow the user back to the roster.** Reported as a *"strange 'Switch
/// Profile — Check the PIN' element"* appearing on Who's Watching after a rejected PIN.
///
/// It is `switch_thread`'s failure banner. The pad and the roster are two surfaces and only one
/// of them is asking about a PIN: `ui::profiles::draw` paints `auth::error()` under the avatar
/// row whenever the pad is closed, so the moment BACK dismissed the keypad the string the pad
/// had already answered with a red flash reappeared under the faces — blaming a PIN nobody was
/// being asked for any more, on the one screen where every profile is a candidate.
///
/// So a PIN-blaming failure leaves NO roster banner. Everything else keeps one, because the
/// roster is exactly where "no access to this server" or "check the connection" belongs — the
/// pad closes for those (`ui::profiles::update`), and a screen that swallowed the choice with
/// no read-out at all is the failure this banner was added for.
#[test]
fn a_rejected_pin_leaves_no_error_on_the_who_s_watching_roster() {
    let (banner, denied) = switch_failure(true);
    assert!(
        banner.is_empty(),
        "a PIN-blaming failure must leave the roster's error band EMPTY — got {banner:?}"
    );
    assert!(denied, "…and must still flash the pad's dots");

    let (banner, denied) = switch_failure(false);
    assert!(
        !banner.is_empty(),
        "a switch that failed for any other reason still owes the roster a read-out"
    );
    assert!(
        !denied,
        "…and must not flash the pad red, which reads as a typo to retry forever"
    );
}

