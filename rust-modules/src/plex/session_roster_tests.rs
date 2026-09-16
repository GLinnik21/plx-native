//! Multi-server roster: boot picker gating, stored origins, corrupt/malformed roster entries,
//! search history scoping, roster-refresh authorization, and household/PIN identity.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn boot_shows_picker_table() {
    let multi_off = dialable_home(2, "u-0", false);
    assert!(
        multi_off.boot_shows_picker(false, false),
        "multi-user interactive still raises the picker when the switch is off"
    );
    assert!(
        !multi_off.boot_shows_picker(true, false),
        "an automated boot still skips the picker"
    );
    assert!(
        multi_off.boot_shows_picker(true, true),
        "pickuser forces the picker even on an automated boot"
    );

    let multi_on = dialable_home(2, "u-0", true);
    assert!(
        multi_on.home_users[0].protected,
        "u-0 is the PIN-protected admin in this fixture"
    );
    assert!(
        !multi_on.boot_shows_picker(false, false),
        "the switch skips the picker when a profile is seated, PIN included"
    );
    assert!(
        multi_on.boot_shows_picker(false, true),
        "pickuser still forces the picker when the switch is on"
    );

    let abandoned = dialable_home(2, "", true);
    assert!(
        abandoned.boot_shows_picker(false, false),
        "an empty uuid still raises the picker so the owner's token is not handed out"
    );

    let gone = dialable_home(2, "u-gone", true);
    assert!(
        gone.boot_shows_picker(false, false),
        "a uuid no longer on the roster still raises the picker — leftover tokens are not a seat"
    );

    let solo = dialable_home(1, "u-0", false);
    assert!(
        !solo.boot_shows_picker(false, false),
        "a one-person account already skips the picker"
    );
    assert!(!dialable_home(1, "u-0", true).boot_shows_picker(false, false));

    let mut undialable = dialable_home(2, "u-0", false);
    undialable.server = ServerRef::default();
    assert!(
        !undialable.boot_shows_picker(false, false),
        "this helper is not the QR path: no local session, no picker"
    );
}

/// The other side of the gate: once an origin IS written down it is what gets dialled, and it
/// beats the address pair beside it. That is not a tie-break for its own sake — for an https
/// server the two genuinely differ (the certificate is issued for the `plex.direct` NAME, not
/// for the quad), so reading the pair would connect and then fail validation.
#[test]
fn a_stored_origin_beats_the_address_pair_beside_it() {
    let json = r#"{"client_id":"c","account_token":"a",
        "server":{"machine_id":"aaaa1111","address":"203.0.113.9","port":31234,"token":"t",
                  "origin":"https://203-0-113-9.hash.plex.direct:31234"},
        "sources":[{"machine_id":"aaaa1111","owned":true,"address":"203.0.113.9","port":31234,
                    "token":"t","origin":"https://203-0-113-9.hash.plex.direct:31234"}]}"#;
    let s: Session = serde_json::from_str(json).expect("parses");

    let o = s.server.origin();
    assert_eq!(
        o.host(),
        "203-0-113-9.hash.plex.direct",
        "the name TLS validates against"
    );
    assert!(o.is_tls());
    assert_eq!(
        s.server.address, "203.0.113.9",
        "…and the quad survives as the diagnostic half"
    );
    assert!(
        s.can_go_local(),
        "an https primary is still a session this device holds"
    );
    assert_eq!(
        s.sources[0].origin().unwrap(),
        o,
        "the roster entry says the same thing"
    );

    // and it round-trips: what we write back is what we would read next boot
    let again: Session =
        serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
    assert_eq!(again.server.origin(), o);
}

/// A stored origin that cannot be dialled is refused rather than silently repaired. The port
/// is the case that really arrives — the session file is JSON on disk that a hand edit or an
/// older build can leave holding anything an `i64` can hold, and `4_294_999_696 as i32` is
/// **32400**, so "repair it to the default" means dialling a port nobody wrote down.
#[test]
fn an_undialable_stored_origin_is_refused_not_repaired() {
    let bad = |origin: &str| {
        let json = format!(
            r#"{{"client_id":"c","account_token":"a",
                 "server":{{"address":"192.168.0.10","port":32400,"token":"t","origin":"{origin}"}},
                 "sources":[{{"machine_id":"m","address":"192.168.0.10","port":32400,"token":"t",
                              "origin":"{origin}"}}]}}"#
        );
        serde_json::from_str::<Session>(&json).expect("the file still parses")
    };
    for origin in [
        "http://192.168.0.10:4294999696",
        "ftp://192.168.0.10:21",
        "http://",
    ] {
        let s = bad(origin);
        assert!(!s.can_go_local(), "{origin} is not something to boot on");
        assert!(
            !s.sources[0].usable(),
            "{origin} is not something to register"
        );
    }
}

/// The roster survives a write/read cycle intact — including the two facts that make a share
/// usable at all: its OWN address (never the owner's LAN one) and its OWN token.
#[test]
fn the_roster_round_trips_through_the_session_file_format() {
    let s: Session = serde_json::from_str(two_server_json()).expect("a normal session parses");
    let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");

    assert_eq!(s.sources.len(), 2);
    let own = s.owned_source().expect("our own server is in the roster");
    assert_eq!(
        (own.machine_id.as_str(), own.address.as_str()),
        ("aaaa1111", "192.168.0.10")
    );
    assert!(
        own.shared_by.is_empty(),
        "an owned server has no owner to name"
    );

    let share = s
        .source("bbbb2222")
        .expect("keyed by machineIdentifier, not by index");
    assert_eq!((share.address.as_str(), share.port), ("203.0.113.9", 31234));
    assert_eq!(
        share.token, "tok-share",
        "the sharing grant, not the account token"
    );
    assert_eq!(share.shared_by, "friend");
    assert!(!share.owned && share.usable());
    assert_eq!(s.shared_sources().count(), 1);

    let mine = s
        .pins_for("u-7")
        .expect("the Home selection is keyed by PROFILE");
    assert!(mine.asked);
    assert_eq!(mine.answer("bbbb2222", 1), Some(true));
    // section keys are server-local: both servers have a section 1, so the key alone matches
    // nothing on its own
    assert_eq!(
        mine.answer("aaaa1111", 1),
        Some(false),
        "an answer names a server AND a key"
    );
    assert_eq!(
        mine.answer("bbbb2222", 9),
        None,
        "a library nobody was asked about"
    );
    assert!(
        s.pins_for("u-9").is_none(),
        "another profile has an answer of its own, or none"
    );
    assert!(s.source("").is_none() && s.source("nope").is_none());

    // and the token is not printable by accident — `describe` is the only formatter there is
    assert!(
        !share.describe().contains("tok-share"),
        "{}",
        share.describe()
    );
    assert!(share.describe().contains("friend") && share.describe().contains("203.0.113.9"));
}

/// **The sign-out bug this list is shaped to avoid.** A `sources` array that is corrupt, the
/// wrong type, or absent entirely must cost the roster and nothing else — `#[serde(default)]`
/// alone does not do that, because it covers an ABSENT field and not a present, malformed one,
/// and the failure mode is not "an empty roster" but a `Session` that will not parse: no
/// account token, no server, a freshly minted client id, and a QR code to scan on every boot.
#[test]
fn a_corrupt_or_absent_roster_never_costs_the_session() {
    // one entry with a hand-mangled port, beside a perfectly good one
    let mixed = r#"{"client_id":"cid-1","account_token":"acct",
        "server":{"name":"m","machine_id":"aaaa1111","address":"192.168.0.10","port":32400,"token":"t"},
        "sources":[{"machine_id":"aaaa1111","port":{"oops":true}},
                   {"machine_id":"bbbb2222","name":"nas-home","owned":false,
                    "address":"203.0.113.9","port":31234,"token":"tok-share"}],
        "home_pins":"not a list"}"#;
    let s: Session = serde_json::from_str(mixed).expect("a bad entry must not fail the file");
    assert_eq!(s.account_token, "acct", "the credentials are still here");
    assert!(s.can_go_local(), "and the device can still stream");
    assert_eq!(
        s.sources.len(),
        1,
        "the malformed entry dropped, the good one landed"
    );
    assert_eq!(s.sources[0].machine_id, "bbbb2222");
    assert!(
        s.home_pins.is_empty(),
        "a string where a list belongs is no list, not an error"
    );

    // the whole field as an explicit null, and the whole field missing (every session file
    // written before this landed) — both are simply a session with no roster yet
    for json in [
        r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"},"sources":null}"#,
        r#"{"client_id":"c","server":{"address":"192.168.0.10","port":32400,"token":"t"}}"#,
    ] {
        let s: Session = serde_json::from_str(json).expect("null and absent both parse");
        assert!(s.sources.is_empty() && s.home_pins.is_empty());
        assert!(
            s.can_go_local(),
            "the primary server is what boot runs on, roster or not"
        );
    }
}

/// **A port is `i64` on disk and `i32` at the socket, and the narrowing used to be a bare
/// cast.** `4_294_999_696 as i32` is **32400** — the most ordinary port there is — so a session
/// file holding a number no port can be would have had the app quietly dial a server nobody
/// wrote down. `#[serde(default)]` cannot catch it either: the field parses fine, it is the
/// value that is impossible.
///
/// Both gates the value reaches are stated here, because they fail differently and one does not
/// imply the other: a bad ROSTER entry costs that entry (`usable`, which
/// `auth::install_roster` filters on before registering), while a bad PRIMARY costs the resume
/// (`can_go_local`, the one gate in front of `plex::install`) and lands the app on sign-in.
#[test]
fn a_port_no_socket_could_take_is_refused_rather_than_wrapped() {
    let s: Session = serde_json::from_str(
        r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"machine_id":"aaaa1111","address":"192.168.0.10","port":32400,"token":"t"},
            "sources":[{"machine_id":"aaaa1111","owned":true,"address":"192.168.0.10",
                        "port":4294999696,"token":"tok-own"},
                       {"machine_id":"bbbb2222","owned":false,"address":"203.0.113.9",
                        "port":31234,"token":"tok-share"}]}"#,
    )
    .unwrap();
    assert!(
        !s.sources[0].usable(),
        "32400 is what that number wraps to — it must not be dialled"
    );
    assert!(
        s.sources[1].usable(),
        "…and the entry beside it is untouched"
    );
    assert!(
        s.can_go_local(),
        "the PRIMARY is fine, so boot still resumes"
    );

    // …and the same number on the primary costs the resume instead, rather than dialling 32400
    let bad: Session = serde_json::from_str(
        r#"{"client_id":"c","server":{"address":"192.168.0.10","port":4294999696,"token":"t"}}"#,
    )
    .unwrap();
    assert!(
        !bad.can_go_local(),
        "an undialable primary sends the user to sign-in, honestly"
    );
    // an absent port is the same answer for the same reason: it could never have connected
    let none: Session = serde_json::from_str(
        r#"{"client_id":"c","server":{"address":"192.168.0.10","token":"t"}}"#,
    )
    .unwrap();
    assert!(!none.can_go_local());
}

/// One server must behave exactly as it did before the roster existed: the primary
/// `server`/`user` pair is what `can_go_local` and `pms_token` read, and the roster is a
/// record beside it, never a second source of truth that could disagree.
#[test]
fn a_single_server_session_behaves_as_it_always_has() {
    let mut s: Session = serde_json::from_str(
        r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"name":"Mac mini","machine_id":"aaaa1111","address":"192.168.0.10",
                      "port":32400,"token":"tok-own"},
            "sources":[{"machine_id":"aaaa1111","name":"Mac mini","owned":true,
                        "address":"192.168.0.10","port":32400,"token":"tok-own"}]}"#,
    )
    .unwrap();
    assert!(s.can_go_local());
    assert_eq!(
        s.pms_token(),
        "tok-own",
        "no managed user picked yet → the server token"
    );
    s.user.token = "tok-user".into();
    assert_eq!(
        s.pms_token(),
        "tok-user",
        "a switched profile's token wins, as before"
    );
    // the roster agrees with the primary rather than competing with it
    assert_eq!(
        s.owned_source().map(|x| x.address.as_str()),
        Some(s.server.address.as_str())
    );
    assert_eq!(s.shared_sources().count(), 0);
    assert!(s.account(None).signed_in && s.account(None).can_switch);
}

/// The Search screen's recent terms are ordinary session content: they survive a write/read
/// cycle in order, including the non-ASCII ones this household actually searches.
#[test]
fn the_recent_search_terms_round_trip_through_the_session_file_format() {
    let s: Session = serde_json::from_str(
        r#"{"client_id":"cid-1","recent_searches":[
             {"user":"uu-1","terms":["wallace","Гладиатор","the curse"]}]}"#,
    )
    .expect("a session carrying terms parses");
    let s: Session = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).expect("re-read");
    assert_eq!(
        s.recents_for("uu-1"),
        ["wallace", "Гладиатор", "the curse"],
        "most recent first, in order"
    );

    // absent entirely — every session file written before this landed
    let s: Session = serde_json::from_str(r#"{"client_id":"c"}"#).unwrap();
    assert!(s.recent_searches.is_empty());
}

/// **One profile cannot read another's history, and cannot delete it either.** A search
/// history is as personal as watch state, and a television is the one place several people
/// share an install — so this is scoped rather than cleared on a switch, which would have
/// stopped the leak at the price of losing your own list every time you handed the remote over.
#[test]
fn a_profiles_search_history_is_its_own() {
    let mut s = Session {
        client_id: "cid".into(),
        ..Default::default()
    };
    s.set_recents_for("uu-a", vec!["gromit".into()]);
    s.set_recents_for("uu-b", vec!["эдем".into()]);

    assert_eq!(s.recents_for("uu-a"), ["gromit"]);
    assert_eq!(s.recents_for("uu-b"), ["эдем"]);
    assert!(
        s.recents_for("uu-never-searched").is_empty(),
        "an unknown profile reads empty, not someone else's"
    );
    // the owner with no Plex Home selection keys on "" and is nobody else
    assert!(s.recents_for("").is_empty());

    // …and a write for one leaves the others intact — the bug `set_recents_for` exists to make
    // unwriteable, since the obvious `Session { recent_searches: mine, ..s }` deletes everybody.
    s.set_recents_for("uu-a", vec!["wallace".into(), "gromit".into()]);
    assert_eq!(s.recents_for("uu-a"), ["wallace", "gromit"]);
    assert_eq!(
        s.recents_for("uu-b"),
        ["эдем"],
        "the other profile's history survived the write"
    );
}

/// And they degrade the same way every other list here does: one malformed term costs that
/// term, never the credentials sitting beside it. A search term must never be able to sign the
/// device out.
#[test]
fn a_corrupt_search_term_costs_that_term_and_not_the_session() {
    let s: Session = serde_json::from_str(
        r#"{"client_id":"cid-1","account_token":"acct",
            "server":{"address":"192.168.0.10","port":32400,"token":"t"},
            "recent_searches":[{"user":"u","terms":["wallace","gromit"]},null,42,"nope"]}"#,
    )
    .expect("a bad term must not fail the file");
    assert_eq!(
        s.recents_for("u"),
        ["wallace", "gromit"],
        "the three bad entries dropped"
    );
    assert_eq!(s.account_token, "acct");
    assert!(s.can_go_local(), "and the device can still stream");

    // the whole field the wrong type is no list, not an error
    let s: Session = serde_json::from_str(r#"{"client_id":"c","recent_searches":"wallace"}"#)
        .expect("a string where a list belongs parses");
    assert!(s.recent_searches.is_empty());
}

/// **Whose token is `account_token`, and is that who is watching?** It is the account OWNER's,
/// written once by the QR sign-in and never replaced by a profile switch — so a roster refresh
/// made with it answers about the owner, and installing those per-server tokens while a managed
/// profile is signed in swaps identities under them. For a RESTRICTED profile it also re-adds
/// the shares `auth::retoken` had correctly made tokenless, which is a re-grant and not a refresh.
#[test]
fn only_the_account_owners_own_profile_may_refresh_the_roster_with_the_account_token() {
    let home = |uuid: &str| Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        user: UserRef {
            uuid: uuid.into(),
            ..Default::default()
        },
        home_users: vec![
            HomeUserRef {
                uuid: "u-owner".into(),
                title: "Gleb".into(),
                admin: true,
                ..Default::default()
            },
            HomeUserRef {
                uuid: "u-kid".into(),
                title: "Kid".into(),
                admin: false,
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert!(
        home("u-owner").active_profile_is_admin(),
        "the owner's own tile"
    );
    assert!(
        !home("u-kid").active_profile_is_admin(),
        "a managed profile is not the account"
    );

    // An account with no Plex Home never writes a profile at all — auth's single-user path
    // enters Home on the owner's server token — so an empty uuid IS the owner.
    let solo = Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        ..Default::default()
    };
    assert!(solo.active_profile_is_admin());

    // …but an unknown uuid is NOT the owner. `home_users` is empty for "never fetched" as much
    // as for "no Plex Home" (see `Session::account`), and on a question whose wrong answer is
    // somebody else's credentials, "cannot prove it" must not read as "yes".
    let mut unknown = home("u-kid");
    unknown.home_users.clear();
    assert!(!unknown.active_profile_is_admin());
    assert!(!home("u-nobody").active_profile_is_admin());
}

/// **Who lives in this house** — the ids the "Shared by …" rule asks
/// `plex::servers::is_household` with, which is the Plex Home ROSTER and nothing else.
///
/// The rule falls back to plex.tv's undocumented `home` flag exactly when this list is empty,
/// so emptiness has to mean one thing — *the roster could not answer* — and every case below
/// is about keeping it meaning that.
#[test]
fn the_household_is_the_home_roster_and_emptiness_means_it_could_not_answer() {
    let s = Session {
        user: UserRef {
            id: 333_333,
            uuid: "u-kid".into(),
            ..Default::default()
        },
        home_users: vec![
            HomeUserRef {
                id: 111_111,
                uuid: "u-owner".into(),
                admin: true,
                ..Default::default()
            },
            HomeUserRef {
                id: 222_222,
                uuid: "u-guest".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert_eq!(
        s.household_ids(),
        vec![111_111, 222_222],
        "the roster, and NOT `user.id` — see the case below and the function's own doc"
    );

    // **`0` is filtered, and that is the compatibility case rather than a tidy-up.** A roster
    // read off a file written before `HomeUserRef::id` existed is all zeroes, and our own
    // server's `ownerId` is `0` too — letting those two meet would suppress a credit by
    // accident, on evidence that is only the absence of evidence.
    let legacy = Session {
        home_users: vec![HomeUserRef {
            uuid: "u-owner".into(),
            admin: true,
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(
        legacy.household_ids().is_empty(),
        "an un-enumerable house is empty, not a house containing nobody-id-zero"
    );

    // **The upgraded managed session, and the reason `user.id` is not in this list.** Every
    // roster id is still the legacy `0`, and the `/switch` that chose this profile wrote a real
    // `user.id` long ago. Including it made the answer NON-empty — which
    // `plex::servers::is_household` reads as "the house can speak for itself" and uses to
    // silence the `home` fallback — while the one id that could have decided the case, the
    // ADMIN's, was among the zeroes that get filtered. The result was the reported bug
    // surviving on exactly the sessions the fallback was added for.
    let upgraded = Session {
        user: UserRef {
            id: 333_333,
            uuid: "u-kid".into(),
            ..Default::default()
        },
        home_users: vec![
            HomeUserRef {
                uuid: "u-owner".into(),
                admin: true,
                ..Default::default()
            },
            HomeUserRef {
                uuid: "u-kid".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert!(
        upgraded.household_ids().is_empty(),
        "a roster of zeroes cannot enumerate the house, whoever is watching"
    );
}

/// **The stored profile's PIN flag — what the boot picker's BACK is gated on.** The escalation
/// it exists to close: the adult profile carries the PIN, the app is signed in as them, a child
/// boots it, and BACK out of the who's-watching picker reinstated that session with no code
/// entered at all (`auth::cancel`).
///
/// The two "the roster cannot say" answers deliberately disagree with the test above's. An
/// unknown uuid is NOT the owner, because that question's wrong answer is somebody else's
/// credentials; the same uuid IS treated as protected, because this question's wrong answer is
/// a bypassed PIN and being wrong the other way costs one profile pick.
#[test]
fn a_stored_profile_behind_a_pin_is_reported_as_protected() {
    let home = |uuid: &str| Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        user: UserRef {
            uuid: uuid.into(),
            ..Default::default()
        },
        home_users: vec![
            HomeUserRef {
                uuid: "u-owner".into(),
                title: "Gleb".into(),
                admin: true,
                protected: true,
                ..Default::default()
            },
            HomeUserRef {
                uuid: "u-kid".into(),
                title: "Kid".into(),
                ..Default::default()
            },
        ],
        ..Default::default()
    };
    assert!(
        home("u-owner").active_profile_is_protected(),
        "the adult tile carries the PIN"
    );
    assert!(
        !home("u-kid").active_profile_is_protected(),
        "a managed profile with no PIN"
    );

    // **A session that names NO profile answers protected too**, which is the half that reads
    // as harmless and is not: it is what a sign-in abandoned at the picker leaves on disk (the
    // account token, the server and the roster are persisted the moment they exist; the pick
    // never happened), and `pms_token()` on it is the OWNER's server token. The very next boot
    // raises a picker over that file — a roster of >1 is exactly what it has — so answering
    // "not protected" here put the owner's credentials behind BACK by a second road.
    let mut abandoned = home("u-owner");
    abandoned.user = UserRef::default();
    assert!(
        abandoned.active_profile_is_protected(),
        "no profile chosen is not 'no PIN to be behind'"
    );
    let solo = Session {
        client_id: "cid".into(),
        account_token: "acct".into(),
        ..Default::default()
    };
    assert!(solo.active_profile_is_protected());

    // …and a uuid the roster does not name is treated as protected.
    let mut unknown = home("u-owner");
    unknown.home_users.clear();
    assert!(unknown.active_profile_is_protected());
    assert!(home("u-nobody").active_profile_is_protected());
}
