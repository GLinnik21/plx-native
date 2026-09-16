//! Server registry and roster reconciliation tests: registration order, profile-switch
//! re-keying, endpoint recovery, revoked/surviving grants, and primary drift repair.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// Our own server registers first and is the primary, whatever order plex.tv listed the account
/// in — because the registry makes the first registration `current` when nothing is yet, so the
/// ordering is what stops a boot coming up pointed at a friend's server and building Home from
/// their library.
#[test]
fn our_own_server_leads_the_roster_however_plex_tv_ordered_it() {
    let roster = vec![
        source("share-1", false, "t1"),
        source("ours", true, "t2"),
        source("share-2", false, "t3"),
    ];
    assert_eq!(
        registration_order(&roster),
        vec![1, 0, 2],
        "ours first, then plex.tv's own order"
    );
    assert_eq!(primary_index(&roster), 1);

    // an entry with no credential (or no address) cannot be dialled, so it is not registered —
    // registering it would put a `Client` in the table that 401s everything asked of it
    let mut half = roster.clone();
    half[0].token.clear();
    half[2].address.clear();
    assert_eq!(registration_order(&half), vec![1]);

    // a shares-only roster (our own box is off) still yields a primary rather than nothing:
    // a friend's library is a better app than "no server found"
    let shares = vec![
        source("share-1", false, "t1"),
        source("share-2", false, "t3"),
    ];
    assert_eq!(primary_index(&shares), 0);
    assert_eq!(registration_order(&shares), vec![0, 1]);
}

/// A profile switch re-keys the WHOLE roster, not just the primary. `accessToken` is per
/// (user, server), so the other profile's token on a share is a 401 waiting to happen — and a
/// server this profile has not been granted becomes an inert, tokenless cache entry rather
/// than lingering with a credential that works or losing the verified address forever.
#[test]
fn switching_profile_re_keys_every_source_and_drops_the_ones_not_granted() {
    let roster = vec![
        source("ours", true, "old-own"),
        source("share-1", false, "old-share"),
        source("gone", false, "old-gone"),
    ];
    let rs = vec![
        resource(
            r#"{"clientIdentifier":"ours","provides":"server","owned":true,"accessToken":"new-own"}"#,
        ),
        resource(
            r#"{"clientIdentifier":"share-1","provides":"server","owned":false,"accessToken":"new-share"}"#,
        ),
    ];

    let next = retoken(&roster, &rs);
    assert_eq!(
        next.len(),
        3,
        "the un-granted server remains only as address metadata"
    );
    assert_eq!(next[0].token, "new-own");
    assert_eq!(
        (next[1].machine_id.as_str(), next[1].token.as_str()),
        ("share-1", "new-share")
    );
    assert_eq!(
        next[1].shared_by, "friend",
        "everything but the token is carried over"
    );
    assert_eq!(
        next[1].address, "10.0.0.1",
        "including the address discovery probed"
    );

    assert_eq!(next[2].machine_id, "gone");
    assert!(
        next[2].token.is_empty() && !next[2].usable(),
        "the old profile credential is gone"
    );

    // Switching back can restore that cached machine without rediscovering its address.
    let restored = retoken(
        &next,
        &[resource(
            r#"{"clientIdentifier":"gone","provides":"server","accessToken":"back"}"#,
        )],
    );
    assert_eq!(restored[2].token, "back");
    assert!(restored[2].usable());

    // a resource that came back WITHOUT a token for this profile remains inert
    let empty = vec![resource(
        r#"{"clientIdentifier":"ours","provides":"server","accessToken":""}"#,
    )];
    let without = retoken(&roster, &empty);
    assert_eq!(without.len(), 3);
    assert!(without.iter().all(|s| s.token.is_empty()));
    // and an entry with no identity cannot be re-keyed, and must never match by emptiness
    let anon = vec![source("", false, "old")];
    assert!(retoken(
        &anon,
        &[resource(r#"{"provides":"server","accessToken":"x"}"#)]
    )
    .is_empty());
}

/// The incident this change fixes: an owner refresh found the public HTTPS route while the
/// protected-profile switch was in flight, then the switch re-keyed the old LAN snapshot and
/// discarded that winner. The selected profile owns both halves of the answer — its grant
/// token and the endpoint verified with that token — so they must land together.
#[test]
fn profile_activation_keeps_a_fresh_wan_winner_instead_of_the_cached_lan_origin() {
    let mut cached = source("ours", true, "owner-token");
    cached.address = "192.0.2.10".into();
    cached.origin_url = "http://192.0.2.10:32400".into();

    let mut wan = source("ours", true, "profile-token");
    wan.address = "203.0.113.9".into();
    wan.origin_url = "https://203-0-113-9.example.test:32400".into();
    wan.tier = Some(probe::Location::Remote);

    let resources = vec![resource(
        r#"{"name":"ours","clientIdentifier":"ours","provides":"server","owned":true,
            "accessToken":"profile-token"}"#,
    )];
    let next = profile_sources(&[cached], &[wan], &resources, &[]);

    assert_eq!(next.len(), 1);
    assert_eq!(next[0].token, "profile-token");
    assert_eq!(next[0].address, "203.0.113.9");
    assert_eq!(next[0].origin_url, "https://203-0-113-9.example.test:32400");
    assert_eq!(next[0].tier, Some(probe::Location::Remote));
}

/// Network recovery may fetch the connection list with the install owner's account token,
/// even though the active managed profile has its own PMS token. Only route facts may cross
/// that seam: copying the Resource credential would make the next request run as the owner.
#[test]
fn endpoint_recovery_repoints_an_existing_source_without_replacing_profile_grants() {
    let mut cached = source("ours", true, "managed-profile-token");
    cached.address = "203.0.113.9".into();
    cached.origin_url = "https://public.example.test:32400".into();
    cached.tier = Some(probe::Location::Remote);
    let mut session = Session {
        server: server_ref(&cached),
        sources: vec![cached],
        ..Default::default()
    };

    let mut lan = source("ours", true, "owner-resource-token");
    lan.address = "192.0.2.10".into();
    lan.origin_url = "https://lan.example.test:32400".into();
    lan.tier = Some(probe::Location::Local);
    let (landed, changed) = apply_refreshed_endpoint(&mut session, "ours", &lan).unwrap();

    assert!(changed);
    assert_eq!(landed.address, "192.0.2.10");
    assert_eq!(landed.origin_url, "https://lan.example.test:32400");
    assert_eq!(landed.tier, Some(probe::Location::Local));
    assert_eq!(landed.token, "managed-profile-token");
    assert_eq!(session.server.token, "managed-profile-token");
    assert_eq!(session.server.origin_url, "https://lan.example.test:32400");
    assert_eq!(session.sources.len(), 1, "recovery cannot add a grant");
}

#[test]
fn endpoint_recovery_cannot_introduce_a_server_outside_the_profile_roster() {
    let cached = source("ours", true, "profile-token");
    let mut session = Session {
        server: server_ref(&cached),
        sources: vec![cached],
        ..Default::default()
    };
    let fresh_share = source("owner-only-share", false, "owner-token");

    assert!(apply_refreshed_endpoint(&mut session, "owner-only-share", &fresh_share).is_none());
    assert_eq!(session.sources.len(), 1);
    assert_eq!(session.sources[0].machine_id, "ours");
}

#[test]
fn profile_activation_promotes_a_surviving_share_when_primary_is_revoked() {
    let stored = vec![
        source("revoked-primary", true, "old-owner"),
        source("surviving-share", false, "old-share"),
    ];
    let resources = vec![resource(
        r#"{"name":"club","clientIdentifier":"surviving-share","provides":"server",
            "owned":false,"sourceTitle":"friend","accessToken":"profile-share"}"#,
    )];

    let next = profile_sources(&stored, &[], &resources, &[]);

    assert_eq!(next.len(), 1);
    assert_eq!(next[0].machine_id, "surviving-share");
    assert_eq!(next[0].token, "profile-share");
    assert_eq!(primary_index(&next), 0);
}

/// **A Plex Home managed user's own household server must not be credited to the admin.**
///
/// This is the reported bug ("Shared by Gleb" on the user's OWN server), reproduced at the one
/// layer that decides it: a profile switch re-fetches `/api/v2/resources` with the SWITCHED
/// user's token (`switch_thread`), and plex.tv answers about that user — so the household's
/// own server comes back `owned:false` with the admin's handle in `sourceTitle`. Fed straight
/// into `SourceRef::shared_by` that is a credit naming the person watching.
///
/// The shape is the live 2026-09-03 `/api/v2/resources` shape with stand-in identities: an
/// owned server carries `sourceTitle:null`/`ownerId:null`, a share carries a handle and the
/// owner's plex.tv id, and `ownerId` is in the same id space as `/api/v2/home/users[].id`
/// (measured: the admin row's `id` equals `/api/v2/user`'s `id`).
#[test]
fn a_home_admins_server_seen_by_a_managed_profile_credits_nobody() {
    const ADMIN_ID: i64 = 111_111;
    const MANAGED_ID: i64 = 222_222;
    const FRIEND_ID: i64 = 987_654;
    let household = [ADMIN_ID, MANAGED_ID];

    // What the admin's own sign-in wrote down: the household server is ours, the share is not.
    let stored = vec![
        source("aaaa1111", true, "own-tok"),
        source("bbbb2222", false, "share-tok"),
    ];
    // What plex.tv says to the MANAGED user's token: nothing is owned, and the household
    // server now names the admin.
    let resources = vec![
        resource(
            r#"{"name":"Mac mini","clientIdentifier":"aaaa1111","provides":"server",
                "owned":false,"home":true,"sourceTitle":"admin","ownerId":111111,
                "accessToken":"kid-own"}"#,
        ),
        resource(
            r#"{"name":"nas-home","clientIdentifier":"bbbb2222","provides":"server",
                "owned":false,"home":false,"sourceTitle":"friend","ownerId":987654,
                "accessToken":"kid-share"}"#,
        ),
    ];

    let next = refreshed_sources(&stored, &[], &resources, &household);

    assert!(
        next[0].shared_by.is_empty(),
        "the household's own server credits nobody, whichever profile is watching — got {:?}",
        next[0].shared_by
    );
    assert_eq!(
        next[1].shared_by, "friend",
        "a person outside the household is still credited"
    );
    let _ = FRIEND_ID;
}

#[test]
fn refresh_keeps_a_still_granted_offline_share_and_drops_only_a_revoked_grant() {
    let stored = vec![
        source("ours", true, "old-own"),
        source("offline-share", false, "old-share"),
        source("revoked", false, "old-revoked"),
    ];
    let mut reached_own = source("ours", true, "new-own");
    reached_own.address = "10.0.0.42".into();
    let reached = vec![reached_own];
    let resources = vec![
        resource(
            r#"{"name":"ours-now","clientIdentifier":"ours","provides":"server","owned":true,
                "accessToken":"new-own","publicAddressMatches":true}"#,
        ),
        resource(
            r#"{"name":"friend-box","clientIdentifier":"offline-share","provides":"server","owned":false,
                "sourceTitle":"friend","accessToken":"new-share"}"#,
        ),
        resource(
            r#"{"name":"brand-new-but-offline","clientIdentifier":"new-share","provides":"server",
                "owned":false,"sourceTitle":"other","accessToken":"new-token"}"#,
        ),
    ];

    let next = refreshed_sources(&stored, &reached, &resources, &[]);
    assert_eq!(
        next.iter()
            .map(|s| s.machine_id.as_str())
            .collect::<Vec<_>>(),
        ["ours", "offline-share"]
    );
    assert_eq!(
        next[0].address, "10.0.0.42",
        "a reached server takes its freshly verified origin"
    );
    assert_eq!(
        next[1].address, "10.0.0.1",
        "an offline but still-granted share keeps its verified address"
    );
    assert_eq!(
        next[1].token, "new-share",
        "but follows the current grant's credential"
    );
    assert!(
        !next.iter().any(|s| s.machine_id == "revoked"),
        "absence from resources is authoritative"
    );
    assert!(
        !next.iter().any(|s| s.machine_id == "new-share"),
        "no address is invented for an unseen server"
    );
}

/// **The two records of the same server must not drift.** `Session::server` is what `app.rs`
/// boots on and `Session::sources` is what everything else reads, and the online roster refresh
/// only ever rewrote the second — so the day the house's PMS took a new LAN address, every boot
/// went on dialling the dead one, and `plex::install` of that address registered a SECOND slot
/// for a machine already in the table (the legacy install has no id to match on) with the dead
/// copy made current.
#[test]
fn a_primary_that_moved_is_followed_by_the_roster_refresh() {
    let mut s = primary("aaaa1111", "192.168.0.10", 32400, "tok-own");
    let mut moved = source("aaaa1111", true, "tok-own2");
    moved.address = "192.168.0.42".into();
    moved.port = 32400;
    let share = source("bbbb2222", false, "tok-share");

    assert!(
        reconcile_primary(&mut s, &[share.clone(), moved.clone()]),
        "the save is owed"
    );
    assert_eq!((s.address.as_str(), s.port), ("192.168.0.42", 32400));
    assert_eq!(
        s.token, "tok-own2",
        "the grant came from the same answer as the address"
    );
    assert_eq!(
        s.machine_id, "aaaa1111",
        "the identity is the KEY here, never something to rewrite"
    );

    // idempotent — a refresh that learns nothing new must not force a flash write every boot
    assert!(!reconcile_primary(&mut s, &[share.clone(), moved.clone()]));

    // a roster that does not name this machine says nothing about it: our own box being off
    // must not blank the address the next boot needs
    let mut off = primary("aaaa1111", "192.168.0.10", 32400, "tok-own");
    assert!(!reconcile_primary(&mut off, &[share.clone()]));
    assert_eq!(off.address, "192.168.0.10");

    // an entry with nothing to dial is not an address to adopt…
    let mut half = moved.clone();
    half.token.clear();
    let mut s2 = primary("aaaa1111", "192.168.0.10", 32400, "tok-own");
    assert!(!reconcile_primary(&mut s2, &[half]));
    assert_eq!(s2.address, "192.168.0.10");

    // …and a primary with no machine id cannot be matched at all — `retoken`'s rule, because an
    // empty id must never match a roster entry that also happens to have none
    let mut anon = primary("", "192.168.0.10", 32400, "tok-own");
    let mut anon_src = source("", true, "tok-x");
    anon_src.address = "10.9.9.9".into();
    assert!(!reconcile_primary(&mut anon, &[anon_src]));
    assert_eq!(anon.address, "192.168.0.10");
}

#[test]
fn a_removed_primary_promotes_the_preferred_surviving_grant_but_an_empty_answer_erases_nothing()
{
    let mut old = primary("gone", "10.0.0.1", 32400, "old");
    let share = source("share", false, "share-token");
    assert!(reconcile_refresh_primary(&mut old, &[share.clone()]));
    assert_eq!(old.machine_id, "share");
    assert_eq!(old.token, "share-token");

    let before = old.clone();
    assert!(!reconcile_refresh_primary(&mut old, &[]));
    assert_eq!(old.machine_id, before.machine_id);
    assert_eq!(old.address, before.address);
    assert_eq!(old.token, before.token);
}

#[test]
fn a_refresh_moves_the_active_home_users_token_with_same_or_replaced_primary() {
    let mut sess = Session {
        server: primary("ours", "10.0.0.1", 32400, "old-server"),
        user: UserRef {
            uuid: "owner".into(),
            token: "old-user".into(),
            ..UserRef::default()
        },
        ..Session::default()
    };

    let fresh_ours = source("ours", true, "fresh-own");
    assert!(reconcile_refresh_session(&mut sess, &[fresh_ours]));
    assert_eq!(sess.server.token, "fresh-own");
    assert_eq!(
        sess.pms_token(),
        "fresh-own",
        "a same-primary token rotation reaches the next boot"
    );

    let survivor = source("share", false, "fresh-share");
    assert!(reconcile_refresh_session(&mut sess, &[survivor]));
    assert_eq!(sess.server.machine_id, "share");
    assert_eq!(
        sess.pms_token(),
        "fresh-share",
        "a promoted primary never inherits the removed PMS's token"
    );
}

