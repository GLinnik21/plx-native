//! The memory door's plex half (`prefs::ProfileList`, `MemoryEntry`, `ProfileMemory::upsert_with`,
//! `tenure`), graded on a synthetic two-half tenant as well as on the real one (subtitle
//! offsets), so the generic is held to its own contract and not only to the one tenant that exists.
//!
//! * **T3** — a half-entry merges against the list as it is NOW (a diverged file), not against the
//!   caller's earlier copy.
//! * **T4** — the cap drains the oldest, and an emptied profile record is dropped.
//! * the wire shape: `{user, items}` is byte-equal as a `Value`, an `account` stamp round-trips,
//!   and an unreadable one costs the stamp, never the record.
use super::prefs::{MemoryEntry, ProfileList, ProfileMemory};
use super::test_support::STAMP;
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// A tenant with two independently picked halves, like track memory's audio and subtitle.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
struct Memo {
    show: String,
    audio: Option<String>,
    subtitle: Option<String>,
}

impl MemoryEntry for Memo {
    type Key = String;
    const CAP: usize = 3;
    fn key(&self) -> String {
        self.show.clone()
    }
    fn forgets(&self) -> bool {
        self.show.is_empty() || (self.audio.is_none() && self.subtitle.is_none())
    }
    fn slot(_: &Session) -> &Vec<ProfileList<Self>> {
        unreachable!("the synthetic tenant lives in no session")
    }
    fn slot_mut(_: &mut Session) -> &mut Vec<ProfileList<Self>> {
        unreachable!("the synthetic tenant lives in no session")
    }
}

fn memo(show: &str, audio: Option<&str>, subtitle: Option<&str>) -> Memo {
    Memo { show: show.into(), audio: audio.map(Into::into), subtitle: subtitle.map(Into::into) }
}

fn record(user: &str, items: Vec<Memo>) -> ProfileList<Memo> {
    ProfileList { user: user.into(), items, ..Default::default() }
}

/// **T3.** The mirror holds a stale entry, the file moved on (another pick landed meanwhile), and
/// the delta is "the subtitle half is s2". Applied to the FILE it keeps the file's audio half;
/// applied as a whole-entry overwrite built from the mirror's copy it would have put the stale
/// audio back. `upsert_with` hands the closure the list's CURRENT entry, which is what makes the
/// first possible.
#[test]
fn t3_upsert_with_merges_a_half_entry_against_a_diverged_file() {
    let mirror_copy = memo("show", Some("audio-old"), Some("sub-1"));
    let mut file = vec![record("u", vec![memo("show", Some("audio-from-file"), Some("sub-1"))])];

    let changed = file.upsert_with("u", "", &"show".to_string(), |current| {
        let mut entry = current.cloned().unwrap_or_else(|| memo("show", None, None));
        entry.subtitle = Some("sub-2".into());
        Some(entry)
    });

    assert!(changed);
    assert_eq!(
        file[0].items,
        vec![memo("show", Some("audio-from-file"), Some("sub-2"))],
        "the subtitle half is the delta's and the audio half is the file's"
    );
    assert_ne!(
        file[0].items[0].audio, mirror_copy.audio,
        "control: merging against the stale copy would have restored its audio half"
    );
}

/// A delta for an entry the file has never seen starts from nothing, and the closure sees `None`.
#[test]
fn a_delta_for_an_unknown_entry_sees_none_and_creates_the_profile_record() {
    let mut list: Vec<ProfileList<Memo>> = Vec::new();
    let mut saw = Some(memo("x", None, None));
    assert!(list.upsert_with("u", "", &"show".to_string(), |current| {
        saw = current.cloned();
        Some(memo("show", Some("a"), None))
    }));
    assert_eq!(saw, None);
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].user.as_str(), list[0].account.as_str()), ("u", ""));
}

/// **T4.** The cap drains the oldest first, a re-recorded entry moves to the end (and so survives),
/// and a profile whose last entry is forgotten loses its record entirely.
#[test]
fn t4_pruning_caps_per_profile_drains_the_oldest_and_drops_an_emptied_record() {
    let mut list: Vec<ProfileList<Memo>> = Vec::new();
    for show in ["a", "b", "c"] {
        list.upsert_with("u", "", &show.to_string(), |_| Some(memo(show, Some("x"), None)));
    }
    list.upsert_with("other", "", &"a".to_string(), |_| Some(memo("a", Some("y"), None)));
    // `a` is re-recorded with a new value: it moves to the end, so the next insert drains `b`.
    list.upsert_with("u", "", &"a".to_string(), |_| Some(memo("a", Some("changed"), None)));
    list.upsert_with("u", "", &"d".to_string(), |_| Some(memo("d", Some("x"), None)));

    let shows: Vec<&str> = list[0].items.iter().map(|m| m.show.as_str()).collect();
    assert_eq!(shows, ["c", "a", "d"], "capped at {}: the oldest (`b`) went", Memo::CAP);
    assert_eq!(list[1].items.len(), 1, "another profile's record is untouched by the cap");

    for show in ["c", "a", "d"] {
        list.upsert_with("u", "", &show.to_string(), |_| None);
    }
    assert_eq!(list.len(), 1, "the emptied profile's record is gone");
    assert_eq!(list[0].user, "other");
}

/// An entry equal to what the list holds is no change: nothing is moved, and a forget of an entry
/// that is not there does not even create a record. (This is what lets a queued write skip a
/// no-op, as `persist_subtitle_offset` always did.)
#[test]
fn an_unchanged_entry_is_not_a_change_and_does_not_reorder() {
    let mut list = vec![record("u", vec![memo("a", Some("x"), None), memo("b", Some("y"), None)])];
    assert!(!list.upsert_with("u", "", &"a".to_string(), |c| c.cloned()), "same entry, same place");
    assert_eq!(list[0].items[0].show, "a", "`a` was not moved to the end");
    assert!(!list.upsert_with("nobody", "", &"a".to_string(), |_| None), "forgetting what was never there");
    assert_eq!(list.len(), 1, "and it created no record");
    assert!(!list.upsert_with("u", "", &"z".to_string(), |_| Some(memo("z", None, None))),
        "an entry that forgets records nothing, so there is nothing to change");
}

/// The record is identified by `(user, account)`: the same profile under another stamp is another
/// record, so a stamped list never reads or overwrites an unstamped one.
#[test]
fn a_record_is_identified_by_user_and_account() {
    let mut list: Vec<ProfileList<Memo>> = Vec::new();
    list.upsert_with("u", "", &"a".to_string(), |_| Some(memo("a", Some("unstamped"), None)));
    list.upsert_with("u", "acct", &"a".to_string(), |_| Some(memo("a", Some("stamped"), None)));
    assert_eq!(list.len(), 2);
    assert_eq!(list.get("u", "", &"a".to_string()).unwrap().audio.as_deref(), Some("unstamped"));
    assert_eq!(list.get("u", "acct", &"a".to_string()).unwrap().audio.as_deref(), Some("stamped"));
    assert!(list.get("u", "other", &"a".to_string()).is_none());
}

/// **The real tenant.** Offsets keep their cap (24), key, forget rule and empty-ids rule through the
/// generic, and the typed `get` still answers.
#[test]
fn subtitle_offsets_run_through_the_generic_with_their_own_cap_and_key() {
    assert_eq!(<SubtitleOffsetEntry as MemoryEntry>::CAP, 24);
    assert_eq!(SubtitleOffsets::CAP, 24);
    let mut session = Session::default();
    session.set_subtitle_offset_for("u", "m", "rk-1", Some(500));
    session.set_subtitle_offset_for("u", "", "rk-1", Some(500));
    session.set_subtitle_offset_for("u", "m", "", Some(500));
    session.set_subtitle_offset_for("u", "m", "rk-2", Some(0));
    assert_eq!(session.prefs.subtitle_offsets.len(), 1);
    assert_eq!(session.prefs.subtitle_offsets[0].items.len(), 1, "empty ids and an offset of 0 record nothing");
    assert_eq!(session.subtitle_offset_for("u", "m", "rk-1"), Some(500));
    let key = ("m".to_string(), "rk-1".to_string());
    assert_eq!(session.prefs.subtitle_offsets.get("u", "", &key).map(|e| e.offset_ms), Some(500));
}

/// `{user, items}` is exactly the shape the field had before the door existed: no `account` key is
/// written while the stamp is empty, so the file is byte-equal (as a `Value`) to the old one.
#[test]
fn an_unstamped_list_writes_exactly_the_old_shape() {
    let old = json!([{"user":"u","items":[{"machine_id":"m","rating_key":"1","offset_ms":-250}]}]);
    let list: Vec<SubtitleOffsets> = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(serde_json::to_value(&list).unwrap(), old);
}

/// A stamp that is present round-trips, and an unreadable one (not a string) reads as no stamp and
/// costs only the stamp: the record and its entries survive.
#[test]
fn an_account_stamp_round_trips_and_an_unreadable_one_costs_only_the_stamp() {
    let stamped = json!({"user":"u","account":"abcd","items":[{"machine_id":"m","rating_key":"1","offset_ms":5}]});
    let list: SubtitleOffsets = serde_json::from_value(stamped.clone()).unwrap();
    assert_eq!(list.account, "abcd");
    assert_eq!(serde_json::to_value(&list).unwrap(), stamped);

    let junk = json!({"user":"u","account":{"fp":[1,2]},"items":[{"machine_id":"m","rating_key":"1","offset_ms":5}]});
    let list: SubtitleOffsets = serde_json::from_value(junk).unwrap();
    assert_eq!(list.account, "", "an unreadable stamp binds to no account");
    assert_eq!(list.items.len(), 1, "and the record is kept");
}

/// `with_memory` is the write closure's door: `None` (no write queued) when nothing changed, the
/// edited session otherwise, with every other field intact.
#[test]
fn with_memory_returns_none_when_unchanged_and_the_edited_session_otherwise() {
    let session = Session { client_id: "cid".into(), ..Default::default() };
    let key = ("m".to_string(), "rk".to_string());
    let entry = |ms| SubtitleOffsetEntry { machine_id: "m".into(), rating_key: "rk".into(), offset_ms: ms, extensions: Default::default() };

    assert!(session.with_memory::<SubtitleOffsetEntry>(|m| m.upsert_with("u", "", &key, |_| None)).is_none());
    let next = session
        .with_memory::<SubtitleOffsetEntry>(|m| m.upsert_with("u", "", &key, |_| Some(entry(700))))
        .expect("a new entry is a change");
    assert_eq!(next.client_id, "cid");
    assert_eq!(next.subtitle_offset_for("u", "m", "rk"), Some(700));
    assert!(next.with_memory::<SubtitleOffsetEntry>(|m| m.upsert_with("u", "", &key, |_| Some(entry(700)))).is_none());
}

/// **T3 through the real queue.** The delta is applied to the FILE under the storage lock: an edit
/// queued against one view of the list lands against whatever the file holds when it runs.
#[test]
fn a_queued_memory_edit_merges_against_the_file_it_finds() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("memory-queued-merge");
    save(&Session { client_id: "cid-1".into(), account_token: "acct".into(), ..Default::default() });
    let _ = peek();

    // Another writer lands a different item first; the queued edit must keep it.
    assert!(update(|cur| {
        let mut next = cur.clone();
        next.set_subtitle_offset_for("u", "m", "rk-first", Some(100));
        Some(next)
    }));
    let key = ("m".to_string(), "rk-second".to_string());
    let ticket = queue_update_ticket(move |cur| {
        let stamp = crate::plex::grant::account_key(&cur.account_token);
        cur.with_memory::<SubtitleOffsetEntry>(|m| {
            m.upsert_with("u", &stamp, &key, |_| {
                Some(SubtitleOffsetEntry {
                    machine_id: "m".into(), rating_key: "rk-second".into(), offset_ms: 200, extensions: Default::default(),
                })
            })
        })
    })
    .unwrap();
    assert!(ticket.wait_blocking().unwrap());
    let after = peek();
    assert_eq!(after.subtitle_offset_for("u", "m", "rk-first"), Some(100));
    assert_eq!(after.subtitle_offset_for("u", "m", "rk-second"), Some(200));
}

/// The tenure moves on revocation and on nothing else a reader would call ordinary.
#[test]
fn the_tenure_moves_when_the_session_is_revoked() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("memory-tenure");
    let before = tenure();
    assert_eq!(tenure(), before, "stable while nothing is revoked");
    revoke_cached_session();
    assert_ne!(tenure(), before);
}

// ---- SO: offsets are account-bound, with legacy unstamped records adopted -----------------------

fn offset(rating_key: &str, offset_ms: i64) -> SubtitleOffsetEntry {
    SubtitleOffsetEntry { machine_id: "m".into(), rating_key: rating_key.into(), offset_ms, extensions: Default::default() }
}

fn offset_key(rating_key: &str) -> (String, String) {
    ("m".to_string(), rating_key.to_string())
}

/// A file written before offsets were account-bound holds records with no stamp. The signed-in
/// account sees them exactly as it did (nothing a viewer sees changes before sign-out), and the
/// first write to the list stamps them as that account's own, so the sign-out that follows can
/// tell whose they are.
#[test]
fn unstamped_offsets_are_adopted_and_stamped_on_write() {
    let mut list: Vec<SubtitleOffsets> = serde_json::from_value(json!([
        {"user": "u", "items": [{"machine_id": "m", "rating_key": "rk-1", "offset_ms": 100},
                                {"machine_id": "m", "rating_key": "rk-2", "offset_ms": 200}]},
        {"user": "v", "items": [{"machine_id": "m", "rating_key": "rk-1", "offset_ms": 300}]},
    ]))
    .unwrap();
    assert_eq!(list.get("u", STAMP, &offset_key("rk-1")).map(|e| e.offset_ms), Some(100), "visible before any write");
    assert_eq!(list.get("v", STAMP, &offset_key("rk-1")).map(|e| e.offset_ms), Some(300));

    assert!(list.upsert_with("u", STAMP, &offset_key("rk-3"), |_| Some(offset("rk-3", 400))));

    assert_eq!(list.len(), 2, "adopted in place, not duplicated beside an unstamped copy");
    assert!(list.iter().all(|r| r.account == STAMP), "every legacy record is now the account's own: {list:?}");
    assert_eq!(list.get("u", STAMP, &offset_key("rk-1")).map(|e| e.offset_ms), Some(100));
    assert_eq!(list.get("u", STAMP, &offset_key("rk-2")).map(|e| e.offset_ms), Some(200));
    assert_eq!(list.get("u", STAMP, &offset_key("rk-3")).map(|e| e.offset_ms), Some(400));
    assert_eq!(list.get("v", STAMP, &offset_key("rk-1")).map(|e| e.offset_ms), Some(300), "another profile's too");
    assert_eq!(serde_json::to_value(&list[0]).unwrap()["account"], json!(STAMP), "and the stamp is written");
}

/// An unchanged entry is not a write, but a legacy record that WOULD be stamped is a change: the
/// list reports it, so the queued update writes the stamp instead of dropping the edit.
#[test]
fn adopting_an_unstamped_record_is_a_change_even_when_the_entry_is_unchanged() {
    let mut list: Vec<SubtitleOffsets> = serde_json::from_value(json!([
        {"user": "u", "items": [{"machine_id": "m", "rating_key": "rk-1", "offset_ms": 100}]}])).unwrap();
    assert!(list.upsert_with("u", STAMP, &offset_key("rk-1"), |current| current.cloned()));
    assert_eq!(list[0].account, STAMP);
    assert!(!list.upsert_with("u", STAMP, &offset_key("rk-1"), |current| current.cloned()), "and then it is settled");
}

/// A record stamped for another account reads as nothing to this one, and the next write drops it
/// (the previous sign-in's offsets must not outlive the first write of the next). A tenant that
/// adopts does not read an unstamped record as foreign.
#[test]
fn a_foreign_stamp_reads_as_nothing_and_the_next_write_prunes_it() {
    let mut list: Vec<SubtitleOffsets> = serde_json::from_value(json!([
        {"user": "u", "account": "ffffffffffffffff", "items": [{"machine_id": "m", "rating_key": "rk-1", "offset_ms": 100}]},
        {"user": "w", "items": [{"machine_id": "m", "rating_key": "rk-9", "offset_ms": 900}]},
    ]))
    .unwrap();
    assert!(list.get("u", STAMP, &offset_key("rk-1")).is_none(), "another account's record is not ours");
    assert_eq!(list.get("w", STAMP, &offset_key("rk-9")).map(|e| e.offset_ms), Some(900));

    assert!(list.upsert_with("u", STAMP, &offset_key("rk-2"), |_| Some(offset("rk-2", 200))));
    assert!(list.get("u", "ffffffffffffffff", &offset_key("rk-1")).is_none(), "pruned");
    assert_eq!(list.len(), 2, "the foreign record is gone, ours and the adopted one remain: {list:?}");
    assert!(list.iter().all(|r| r.account == STAMP));
}

/// Both an unstamped record and a stamped one for the same profile (a build that predates the stamp
/// wrote beside a stamped record): adoption merges them into the one record, the stamped record's
/// entries winning, rather than leaving two records for one `(user, account)`.
#[test]
fn an_unstamped_record_merges_into_the_accounts_own_record_of_the_same_profile() {
    let mut list: Vec<SubtitleOffsets> = serde_json::from_value(json!([
        {"user": "u", "items": [{"machine_id": "m", "rating_key": "rk-1", "offset_ms": 100},
                                {"machine_id": "m", "rating_key": "rk-2", "offset_ms": 200}]},
        {"user": "u", "account": STAMP, "items": [{"machine_id": "m", "rating_key": "rk-2", "offset_ms": 999}]},
    ]))
    .unwrap();
    assert!(list.upsert_with("u", STAMP, &offset_key("rk-3"), |_| Some(offset("rk-3", 300))));
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list.get("u", STAMP, &offset_key("rk-1")).map(|e| e.offset_ms), Some(100));
    assert_eq!(list.get("u", STAMP, &offset_key("rk-2")).map(|e| e.offset_ms), Some(999), "the stamped entry wins");
    assert_eq!(list.get("u", STAMP, &offset_key("rk-3")).map(|e| e.offset_ms), Some(300));
}

/// A session with no account token has no stamp to bind to; offsets are not refused there (they
/// never were), they are written unstamped and a later account adopts them. Nothing is pruned
/// without a stamp to compare against.
#[test]
fn a_write_without_a_stamp_is_kept_unstamped_and_prunes_nothing() {
    let mut list: Vec<SubtitleOffsets> = serde_json::from_value(json!([
        {"user": "u", "account": "ffffffffffffffff", "items": [{"machine_id": "m", "rating_key": "rk-1", "offset_ms": 100}]}])).unwrap();
    assert!(list.upsert_with("u", "", &offset_key("rk-2"), |_| Some(offset("rk-2", 200))));
    assert_eq!(list.len(), 2, "the stamped record is left alone: {list:?}");
    assert_eq!(list.get("u", "", &offset_key("rk-2")).map(|e| e.offset_ms), Some(200));
    assert!(list.get("u", "", &offset_key("rk-1")).is_none(), "and still reads as nothing without its account");
}

/// The in-memory half of the door stamps with the session's OWN account.
#[test]
fn a_session_stamps_the_offsets_it_records_with_its_own_account() {
    let mut session = Session { account_token: "acct".into(), ..Default::default() };
    session.set_subtitle_offset_for("u", "m", "rk-1", Some(500));
    let stamp = crate::plex::grant::account_key("acct");
    assert_eq!(session.prefs.subtitle_offsets[0].account, stamp);
    assert_eq!(session.prefs.subtitle_offsets.get("u", &stamp, &offset_key("rk-1")).map(|e| e.offset_ms), Some(500));
}

/// **N7: a login over a stored session of another account does not inherit its library views.**
/// `library_sorts` and `last_library` are per profile and carry no account stamp, so sign-out clears
/// them (`ACCOUNT_BOUND_PREFERENCES`). The one login that is not a sign-out, a QR sign-in started
/// with a stored session that could not be activated still on disk, replaces the credentials in place
/// (`replace_after_reauthentication_guarded_with_outcome`), and the new account used to open on the
/// previous one's last library and sorts. A different account clears them; the same one keeps them.
#[test]
fn a_login_over_another_accounts_session_does_not_inherit_its_library_views() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("relogin-library-views");
    let mut first = Session { client_id: "cid-1".into(), account_token: "acct-1".into(),
        home_users: roster_of(7), ..Default::default() };
    first.set_view_for("u", LibraryView {
        machine_id: "m".into(), key: 1, sort: "addedAt".into(), desc: true, ..Default::default()
    });
    first.prefs.last_library.push(LastLibrary {
        user: "u".into(),
        libs: vec![TypedLib { kind: "movie".into(), machine_id: "m".into(), key: 1, extensions: Default::default() }],
        extensions: Default::default(),
    });
    save(&first);
    let views = |s: &Session| (s.prefs.library_views.len(), s.prefs.last_library.len());
    assert_eq!(views(&load()), (1, 1), "control: both lists are stored");

    let login_as = |token: &str, owner: i64| login_with_roster(token, roster_of(owner));
    login_as("acct-1", 7);
    assert_eq!(views(&load()), (1, 1), "the same account logging in again keeps its views");
    login_as("acct-2", 8);
    assert_eq!(views(&load()), (0, 0), "another account does not inherit them");
}

/// **The same login does not inherit the previous account's Home pins or search history either.**
/// Both are keyed by profile `uuid` with `""` for the account owner, carry no account stamp, and go
/// with the file at sign-out; a login over another account's stored session replaces the credentials
/// in place, so the next account's owner (key `""` again) opened on the previous owner's favourite
/// libraries and was offered back what they had searched for. Same rule as the library views: a
/// different account clears them, the same one keeps them.
#[test]
fn a_login_over_another_accounts_session_does_not_inherit_its_pins_or_searches() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("relogin-pins-searches");
    let first = Session {
        client_id: "cid-1".into(),
        account_token: "acct-1".into(), home_users: roster_of(7),
        home_pins: vec![HomePins { user: String::new(), asked: true, ..Default::default() }],
        recent_searches: vec![RecentSearches { user: String::new(), terms: vec!["synthetic term".into()],
            ..Default::default() }],
        ..Default::default()
    };
    save(&first);
    let personal = |s: &Session| (s.home_pins.len(), s.recent_searches.len());
    assert_eq!(personal(&load()), (1, 1), "control: both lists are stored");

    let login_as = |token: &str, owner: i64| login_with_roster(token, roster_of(owner));
    login_as("acct-1", 7);
    assert_eq!(personal(&load()), (1, 1), "the same account logging in again keeps them");
    login_as("acct-2", 8);
    assert_eq!(personal(&load()), (0, 0), "another account does not inherit them");
}

/// The roster a sign-in's discovery save writes: one admin (the account owner) with this plex.tv id.
fn roster_of(owner: i64) -> Vec<HomeUserRef> {
    vec![HomeUserRef { id: owner, uuid: format!("synthetic-owner-{owner}"), title: "Synthetic".into(),
        admin: true, ..Default::default() }]
}

/// A login over the stored session: the new token and the roster the discovery save carries.
fn login_with_roster(token: &str, roster: Vec<HomeUserRef>) {
    let token = token.to_string();
    replace_after_reauthentication_with_outcome(|_| true,
        move |cur| Session { account_token: token, home_users: roster, ..cur.clone() })
        .expect("not fenced")
        .expect("written");
}

/// **A fresh QR sign-in always mints a new account token, so the token cannot say "same account".**
/// The same person signing in again (a new token, the same owner identity in the roster) keeps their
/// pins, searches and library memory; a different owner, or a roster either side cannot name (empty
/// on the disk or in the new save), loses them: unknown identity fails safe to clearing.
#[test]
fn a_login_keeps_what_belongs_to_the_same_account_identity_whatever_its_token() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("relogin-identity");
    let mut first = Session {
        client_id: "cid-1".into(), account_token: "token-a".into(), home_users: roster_of(7),
        home_pins: vec![HomePins { user: String::new(), asked: true, ..Default::default() }],
        recent_searches: vec![RecentSearches { user: String::new(), terms: vec!["synthetic term".into()],
            ..Default::default() }],
        ..Default::default()
    };
    first.set_view_for("u", LibraryView {
        machine_id: "m".into(), key: 1, sort: "addedAt".into(), desc: true, ..Default::default()
    });
    first.prefs.last_library.push(LastLibrary {
        user: "u".into(),
        libs: vec![TypedLib { kind: "movie".into(), machine_id: "m".into(), key: 1, extensions: Default::default() }],
        extensions: Default::default(),
    });
    let kept = |s: &Session| (s.home_pins.len(), s.recent_searches.len(),
        s.prefs.library_views.len(), s.prefs.last_library.len());
    save(&first);
    assert_eq!(kept(&load()), (1, 1, 1, 1), "control: all four are stored");

    login_with_roster("token-b", roster_of(7));
    assert_eq!(load().account_token, "token-b", "control: the new token was written");
    assert_eq!(kept(&load()), (1, 1, 1, 1), "same owner identity, different token: kept");

    login_with_roster("token-c", roster_of(8));
    assert_eq!(kept(&load()), (0, 0, 0, 0), "a different owner identity clears all four");

    // Unknown on the new side: the discovery save carried no roster.
    save(&Session { home_users: roster_of(8), ..first.clone() });
    login_with_roster("token-d", Vec::new());
    assert_eq!(kept(&load()), (0, 0, 0, 0), "an unnamed new identity clears");

    // Unknown on the disk side: the stored record never fetched a roster.
    save(&Session { home_users: Vec::new(), ..first.clone() });
    assert_eq!(kept(&load()), (1, 1, 1, 1), "control: stored again");
    login_with_roster("token-e", roster_of(7));
    assert_eq!(kept(&load()), (0, 0, 0, 0), "an unnamed stored identity clears");
}

// ---- #830: per-account memory follows the OWNER, not the sign-in token ---------------------------
//
// A sign-in mints a new plex.tv token every time, so a stamp derived from the token orphaned the
// memory of a person who signed in again. The stamp of a record is now the OWNER's
// (`account_stamp`), and these tests hold that to the three stamped kinds (`profile_prefs`,
// `track_memory`, `subtitle_offsets`) through the real doors and the real read/write path.
//
// The records a test needs "as an owner-stamped build wrote them" are written under the GOLDEN
// stamps of `stamp_tests` rather than under `account_stamp(..)`, so a test cannot pass by agreeing
// with the implementation it grades.
use super::prefs::{AudioMemo, TrackMemoryEntry};
use super::stamp_tests::{OWNER_7, OWNER_7_UUID, OWNER_8};

fn tm_key(show: &str) -> (String, String) {
    ("m".to_string(), show.to_string())
}

fn track(show: &str) -> TrackMemoryEntry {
    TrackMemoryEntry::new("m", show, Some(AudioMemo { lang: "eng".into(), ..Default::default() }), None)
}

/// One record of each of the three stamped kinds for `user`, under `stamp`, tagged `tag`: the file
/// as some build wrote it. Pushed directly, not through the doors (which prune every other stamp),
/// so a fixture can hold records of several stamps at once.
fn remember_all(session: &mut Session, user: &str, stamp: &str, tag: &str) {
    fn record_of<'a, E>(list: &'a mut Vec<ProfileList<E>>, user: &str, stamp: &str) -> &'a mut ProfileList<E> {
        let at = list.iter().position(|r| r.user == user && r.account == stamp).unwrap_or_else(|| {
            list.push(ProfileList { user: user.into(), account: stamp.into(), ..Default::default() });
            list.len() - 1
        });
        &mut list[at]
    }
    record_of(&mut session.prefs.track_memory, user, stamp).items.push(track(tag));
    record_of(&mut session.prefs.subtitle_offsets, user, stamp).items.push(offset(tag, 250));
    if !session.prefs.profile_prefs.iter().any(|r| r.user == user && r.account == stamp) {
        session.prefs.profile_prefs.push(ProfilePrefsRecord { user: user.into(), account: stamp.into(),
            prefs: ProfilePrefs { subtitle_size: Some(SubtitleSize::Large), ..Default::default() } });
    }
}

/// (track memory, subtitle offset, profile pick) as `user` reads them under `stamp`.
fn reads(session: &Session, user: &str, stamp: &str, tag: &str) -> (bool, bool, bool) {
    (
        session.prefs.track_memory.get(user, stamp, &tm_key(tag)).is_some(),
        session.prefs.subtitle_offsets.get(user, stamp, &offset_key(tag)).is_some(),
        own_prefs(&session.prefs.profile_prefs, user, stamp).is_some_and(|p| p.subtitle_size == Some(SubtitleSize::Large)),
    )
}

const ALL: (bool, bool, bool) = (true, true, true);
const NONE: (bool, bool, bool) = (false, false, false);

/// A stored session of `token` with `roster`, holding `tag`'s three kinds for profile `u` under `stamp`.
fn stored_with_memory(token: &str, roster: Vec<HomeUserRef>, stamp: &str, tag: &str) -> Session {
    let mut session = Session { client_id: "cid-1".into(), account_token: token.into(), home_users: roster, ..Default::default() };
    remember_all(&mut session, "u", stamp, tag);
    session
}

/// **#830. The same person signing in again keeps their memory.** A QR sign-in mints a new token,
/// so a stamp of the token read the person's own track memory, subtitle offsets and player settings
/// as another account's and the next write pruned them. The roster's admin names the owner and does
/// not change with the token.
#[test]
fn a_resignin_by_the_same_owner_keeps_track_memory_offsets_and_profile_prefs() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-same-owner");
    let mut first = stored_with_memory("token-a", roster_of(7), OWNER_7, "k1");
    first.home_pins = vec![HomePins { user: String::new(), asked: true, ..Default::default() }];
    let first = first.with_plaintext_choice(&crate::plex::grant::account_key("token-a"), "machine", PlaintextChoice::Allowed);
    save(&first);

    let before = load();
    assert_eq!(account_stamp(&before), OWNER_7, "the owner is named by the roster, not by the token");
    assert_eq!(reads(&before, "u", &account_stamp(&before), "k1"), ALL, "control: all three are readable before the sign-in");

    login_with_roster("token-b", roster_of(7));
    let after = load();
    assert_eq!(after.account_token, "token-b", "control: the new token was written");
    assert_eq!(account_stamp(&after), OWNER_7, "the same owner under a new token is the same stamp");
    assert_eq!(reads(&after, "u", &account_stamp(&after), "k1"), ALL, "all three survive the sign-in");
    assert_eq!(after.home_pins.len(), 1, "control: the unstamped kinds already followed the owner");
    assert_eq!(after.plaintext_choice(&crate::plex::grant::account_key("token-b"), "machine"), PlaintextChoice::Undecided,
        "plaintext consent stays bound to the sign-in by design: the new token is asked again");
}

/// **A different owner never reads or keeps another's memory.** Bob signing in over Alice's stored
/// session (no sign-out) reads none of it, his first write prunes it, and Alice coming back reads
/// nothing either (the file keeps only the live account's).
#[test]
fn a_sign_in_by_a_different_owner_reads_none_of_it() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-other-owner");
    save(&stored_with_memory("token-a", roster_of(7), OWNER_7, "k1"));
    let alice = account_stamp(&load());

    login_with_roster("token-c", roster_of(8));
    let bob = load();
    assert_eq!(account_stamp(&bob), OWNER_8);
    assert_eq!(reads(&bob, "u", &account_stamp(&bob), "k1"), NONE, "Bob does not read Alice's");
    assert_eq!(reads(&bob, "u", &alice, "k1"), ALL, "control: her records are still on disk, under her stamp, unreadable to him");

    assert!(update(|cur| {
        let mut next = cur.clone();
        let stamp = account_stamp(&next);
        next.prefs.track_memory.upsert_with("u", &stamp, &tm_key("bobs"), |_| Some(track("bobs")));
        put_own::<SubtitleSizePref>(&mut next.prefs.profile_prefs, "u", &stamp, SubtitleSize::Small);
        next.prefs.subtitle_offsets.upsert_with("u", &stamp, &offset_key("bobs"), |_| Some(offset("bobs", 90)));
        Some(next)
    }));
    let pruned = load();
    for (name, accounts) in [
        ("track_memory", pruned.prefs.track_memory.iter().map(|r| r.account.clone()).collect::<Vec<_>>()),
        ("subtitle_offsets", pruned.prefs.subtitle_offsets.iter().map(|r| r.account.clone()).collect()),
        ("profile_prefs", pruned.prefs.profile_prefs.iter().map(|r| r.account.clone()).collect()),
    ] {
        assert_eq!(accounts, vec![OWNER_8.to_string()], "{name}: Bob's first write pruned everything that is not his");
    }

    login_with_roster("token-a2", roster_of(7));
    let alice_again = load();
    assert_eq!(reads(&alice_again, "u", &account_stamp(&alice_again), "k1"), NONE, "A, B, A with a write by B: nothing comes back");
}

/// **An older build's records are adopted into the owner stamp, once.** The previous build stamped
/// every record with the token fingerprint. The first read by this build (roster known) re-stamps
/// the records whose stamp is THIS token's fingerprint; nothing re-stamps them twice; and a later
/// sign-in by the same owner keeps them while a different owner's does not.
#[test]
fn a_record_stamped_by_an_older_build_is_adopted_once() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-adopt-once");
    let legacy = crate::plex::grant::account_key("token-a");
    // The file as the OLDER build wrote it: raw bytes, not `save`, which adopts on the way out.
    std::fs::write(_root.path(), serde_json::to_vec(&stored_with_memory("token-a", roster_of(7), &legacy, "old")).unwrap()).unwrap();
    invalidate_for_test();
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(_root.path()).unwrap()).unwrap();
    assert_eq!(raw["track_memory"][0]["account"], json!(legacy), "setup: the file carries the token fingerprint");

    let adopted = load();
    for (name, account) in [
        ("track_memory", &adopted.prefs.track_memory[0].account),
        ("subtitle_offsets", &adopted.prefs.subtitle_offsets[0].account),
        ("profile_prefs", &adopted.prefs.profile_prefs[0].account),
    ] {
        assert_eq!(account, OWNER_7, "{name}: adopted into the owner stamp");
    }
    assert_eq!(reads(&adopted, "u", &account_stamp(&adopted), "old"), ALL);

    let again = load();
    assert_eq!(serde_json::to_value(&again).unwrap(), serde_json::to_value(&adopted).unwrap(), "a second read changes nothing");
    let twice = adopted.clone().adopt_stamps();
    assert_eq!(serde_json::to_value(&twice).unwrap(), serde_json::to_value(&adopted).unwrap(), "the pass is idempotent");

    login_with_roster("token-b", roster_of(7));
    let same = load();
    assert_eq!(reads(&same, "u", &account_stamp(&same), "old"), ALL, "the same owner under a new token keeps them");
    login_with_roster("token-c", roster_of(8));
    let other = load();
    assert_eq!(reads(&other, "u", &account_stamp(&other), "old"), NONE, "a different owner does not");
}

/// The pass over a freshly read session, directly: a session of THIS token with the token
/// fingerprint adopts, a session with another token's fingerprint does not (a stamp can only be
/// told apart from a stranger's by the token that wrote it), and running it twice is a no-op.
#[test]
fn adoption_takes_only_this_tokens_fingerprint_and_is_idempotent() {
    let mine = crate::plex::grant::account_key("token-a");
    let theirs = crate::plex::grant::account_key("token-z");
    let mut session = stored_with_memory("token-a", roster_of(7), &mine, "mine");
    remember_all(&mut session, "other", &theirs, "theirs");
    let adopted = session.adopt_stamps();
    let accounts = |s: &Session| {
        let mut accounts: Vec<(String, String)> =
            s.prefs.track_memory.iter().map(|r| (r.user.clone(), r.account.clone())).collect();
        accounts.sort();
        accounts
    };
    assert_eq!(accounts(&adopted), [("other".to_string(), theirs.clone()), ("u".to_string(), OWNER_7.to_string())],
        "this token's record is adopted; another token's is left alone, still foreign");
    let twice = adopted.clone().adopt_stamps();
    assert_eq!(serde_json::to_value(&twice).unwrap(), serde_json::to_value(&adopted).unwrap());
}

/// **A record stamped for another token is not adopted**, and neither is the owner family of
/// another owner: nobody's memory becomes the signed-in person's by being stamped something else.
#[test]
fn a_record_stamped_for_another_token_or_owner_is_not_adopted() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-not-adopted");
    let mut session = Session { client_id: "cid-1".into(), account_token: "token-a".into(), home_users: roster_of(7), ..Default::default() };
    for (user, stamp) in [("u-z", crate::plex::grant::account_key("token-z")), ("u-8", OWNER_8.to_string()),
        ("u-junk", "o1:zzzz".to_string()), ("u-cased", "O1:19afc47bcea99412".to_string())] {
        remember_all(&mut session, user, &stamp, "k1");
    }
    save(&session);
    let loaded = load();
    let mut accounts: Vec<String> = loaded.prefs.track_memory.iter().map(|r| r.account.clone()).collect();
    accounts.sort();
    let mut expected = vec![crate::plex::grant::account_key("token-z"), OWNER_8.to_string(), "o1:zzzz".to_string(),
        "O1:19afc47bcea99412".to_string()];
    expected.sort();
    assert_eq!(accounts, expected, "no record changed hands");
    for user in ["u-z", "u-8", "u-junk", "u-cased"] {
        assert_eq!(reads(&loaded, user, &account_stamp(&loaded), "k1"), NONE, "{user}: not readable by the owner of token-a");
    }
}

/// **The owner-unknown window keeps the owner's records.** A sign-in whose roster has not landed
/// yet (`home/users` failed, or not fetched yet) stamps with the token fingerprint. It must not
/// delete the records of the owner it cannot name yet (they may be exactly this person's), and when
/// the roster lands the next read adopts what was written in the window, the stamped record
/// winning a clash.
#[test]
fn an_owner_unknown_window_neither_adopts_nor_prunes_owner_records() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-unknown-window");
    save(&stored_with_memory("token-a", roster_of(7), OWNER_7, "k1"));
    login_with_roster("token-b", Vec::new());
    let window = load();
    let token_form = crate::plex::grant::account_key("token-b");
    assert_eq!(account_stamp(&window), token_form, "the owner is unknown: the stamp is the token's");
    assert_eq!(reads(&window, "u", &token_form, "k1"), NONE, "and nothing of the owner's is readable meanwhile");

    // A pick in the window is stamped with the token and prunes nothing of the owner's.
    assert!(update(|cur| {
        let mut next = cur.clone();
        let stamp = account_stamp(&next);
        next.prefs.track_memory.upsert_with("u", &stamp, &tm_key("k2"), |_| Some(track("k2")));
        next.prefs.track_memory.upsert_with("u", &stamp, &tm_key("k1"), |_| Some(TrackMemoryEntry::new("m", "k1",
            Some(AudioMemo { lang: "deu".into(), ..Default::default() }), None)));
        put_own::<SubtitlePositionPref>(&mut next.prefs.profile_prefs, "u", &stamp, SubtitlePosition::High);
        Some(next)
    }));
    let written = load();
    let accounts: Vec<&str> = written.prefs.track_memory.iter().map(|r| r.account.as_str()).collect();
    assert!(accounts.contains(&OWNER_7), "the owner's record survived a write by the unknown-owner window: {accounts:?}");
    assert!(written.prefs.profile_prefs.iter().any(|r| r.account == OWNER_7), "so did the owner's picks");
    assert!(written.prefs.subtitle_offsets.iter().any(|r| r.account == OWNER_7), "and offsets");

    // The roster lands (the picker's refresh, or the next rediscovery).
    assert!(update(|cur| Some(Session { home_users: roster_of(7), ..cur.clone() })));
    let landed = load();
    let stamp = account_stamp(&landed);
    assert_eq!(stamp, OWNER_7);
    assert_eq!(landed.prefs.track_memory.len(), 1, "adopted and merged into the one record: {:?}", landed.prefs.track_memory);
    assert!(landed.prefs.track_memory.get("u", &stamp, &tm_key("k2")).is_some(), "the window's pick is now the owner's");
    assert_eq!(landed.prefs.track_memory.get("u", &stamp, &tm_key("k1")).and_then(|e| e.audio.as_ref()).map(|a| a.lang.as_str()),
        Some("eng"), "on a clash the entry the owner's stamped record already held wins (the merge rule of `adopt_unstamped`)");
    assert_eq!(own_prefs(&landed.prefs.profile_prefs, "u", &stamp).map(|p| (p.subtitle_size, p.subtitle_position)),
        Some((Some(SubtitleSize::Large), Some(SubtitlePosition::High))), "profile picks merge field by field");
}

/// **The admin's id arriving late keeps the uuid-stamped records.** A roster written before it
/// carried ids names the owner by uuid; a later refresh brings the id of the SAME admin, and the
/// stamp moves from the uuid spelling to the id spelling. Without the alias rule a roster refresh
/// would orphan the owner's memory.
#[test]
fn the_admin_id_arriving_late_keeps_the_uuid_stamped_records() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-late-id");
    let id_less = vec![HomeUserRef { id: 0, uuid: "synthetic-owner-7".into(), title: "Synthetic".into(), admin: true, ..Default::default() }];
    save(&stored_with_memory("token-a", id_less, OWNER_7_UUID, "k1"));
    let before = load();
    assert_eq!(account_stamp(&before), OWNER_7_UUID, "control: an id-less roster is named by its uuid");
    assert_eq!(reads(&before, "u", OWNER_7_UUID, "k1"), ALL);

    assert!(update(|cur| Some(Session { home_users: roster_of(7), ..cur.clone() })), "the refresh brings the id");
    let after = load();
    assert_eq!(account_stamp(&after), OWNER_7, "the primary spelling is the id");
    assert_eq!(reads(&after, "u", &account_stamp(&after), "k1"), ALL, "the uuid-stamped records were re-spelled, not orphaned");
    assert_eq!(after.prefs.track_memory[0].account, OWNER_7);

    // A different admin with the same uuid-less shape never gets them.
    assert!(update(|cur| Some(Session { home_users: roster_of(8), ..cur.clone() })));
    let other = load();
    assert_eq!(reads(&other, "u", &account_stamp(&other), "k1"), NONE);
}

/// A roster without a nameable admin, and a session without a token, fall back to the stamps they
/// always had: the token fingerprint, and no stamp at all.
#[test]
fn a_roster_without_an_admin_falls_back_to_the_token_stamp() {
    let member_only = vec![HomeUserRef { id: 9, uuid: "member".into(), admin: false, ..Default::default() }];
    let session = Session { account_token: "token-a".into(), home_users: member_only, ..Default::default() };
    assert_eq!(account_stamp(&session), crate::plex::grant::account_key("token-a"));
    assert_eq!(account_stamp(&Session { home_users: roster_of(7), ..Default::default() }), "", "no token, no stamp");
    // Writing through the doors with the empty stamp is refused, as it always was.
    let mut list: Vec<ProfileList<super::prefs::TrackMemoryEntry>> = Vec::new();
    assert!(!list.upsert_with("u", &account_stamp(&Session::default()), &tm_key("k"), |_| Some(track("k"))));
}

/// **Two profiles of one owner stay separate across a re-sign-in.** Records are keyed by
/// `(profile uuid, stamp)`; the stamp is shared by the household's profiles, the uuid is not.
#[test]
fn two_profiles_of_one_owner_keep_separate_records_across_a_resignin() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("stamp-two-profiles");
    let mut first = stored_with_memory("token-a", roster_of(7), OWNER_7, "kid-show");
    first.prefs.track_memory.upsert_with("parent", OWNER_7, &tm_key("parent-show"), |_| Some(track("parent-show")));
    save(&first);
    login_with_roster("token-b", roster_of(7));
    let after = load();
    let stamp = account_stamp(&after);
    assert!(after.prefs.track_memory.get("u", &stamp, &tm_key("kid-show")).is_some());
    assert!(after.prefs.track_memory.get("parent", &stamp, &tm_key("parent-show")).is_some());
    assert!(after.prefs.track_memory.get("u", &stamp, &tm_key("parent-show")).is_none(), "one profile's pick is not the other's");
    assert!(after.prefs.track_memory.get("parent", &stamp, &tm_key("kid-show")).is_none());
}

/// **The previous build, modelled.** It compares the stamp with the fingerprint of its own token,
/// so an owner-stamped record is a foreign account's to it: nothing is read, nothing panics, its
/// first write prunes the record (its `prune_foreign` was `retain(account == own stamp)`, written
/// out here because this build's own prune now keeps owner stamps while the owner is unknown), and
/// what is left still parses. (Accepted rollback loss.)
#[test]
fn the_old_reader_treats_an_owner_stamp_as_foreign() {
    let old_stamp = crate::plex::grant::account_key("token-a");
    let session = stored_with_memory("token-a", roster_of(7), OWNER_7, "k1");
    assert_eq!(reads(&session, "u", &old_stamp, "k1"), NONE, "the old reader finds nothing under its stamp");
    let mut old = session.clone();
    old.prefs.track_memory.retain(|r| r.account == old_stamp);
    old.prefs.track_memory.push(ProfileList { user: "u".into(), account: old_stamp.clone(), items: vec![track("k2")], ..Default::default() });
    assert!(old.prefs.track_memory.iter().all(|r| r.account == old_stamp), "its first write pruned the owner-stamped record");
    let reparsed: Session = serde_json::from_value(serde_json::to_value(&old).unwrap()).unwrap();
    assert_eq!(reparsed.prefs.track_memory.len(), 1, "and the bytes it leaves parse");
}

/// A stamp that is not a string, is empty, or is a near-miss of the owner form is never adopted,
/// never a parse failure and never a panic; an unstamped record is nobody's (and, for the offsets
/// that adopt unstamped records, stays unstamped until a write).
#[test]
fn junk_and_empty_stamps_never_panic_and_never_adopt() {
    let session: Session = serde_json::from_value(json!({
        "client_id": "cid-1", "account_token": "token-a",
        "home_users": [{"id": 7, "uuid": "synthetic-owner-7", "admin": true}],
        "track_memory": [
            {"user": "a", "account": 5, "items": [{"machine_id": "m", "show_rk": "1", "audio": {"lang": "eng"}}]},
            {"user": "b", "account": ["o1:x"], "items": [{"machine_id": "m", "show_rk": "1", "audio": {"lang": "eng"}}]},
            {"user": "c", "account": "", "items": [{"machine_id": "m", "show_rk": "1", "audio": {"lang": "eng"}}]},
            {"user": "d", "account": "o1:", "items": [{"machine_id": "m", "show_rk": "1", "audio": {"lang": "eng"}}]},
            {"user": "e", "items": [{"machine_id": "m", "show_rk": "1", "audio": {"lang": "eng"}}]}],
        "subtitle_offsets": [
            {"user": "a", "account": null, "items": [{"machine_id": "m", "rating_key": "1", "offset_ms": 5}]},
            {"user": "b", "items": [{"machine_id": "m", "rating_key": "1", "offset_ms": 5}]}],
        "profile_prefs": [
            {"user": "a", "account": {"x": 1}, "prefs": {"subtitle_size": "large"}},
            {"user": "b", "prefs": {"subtitle_size": "large"}}]
    })).unwrap();
    let adopted = session.clone().adopt_stamps().adopt_stamps();
    assert_eq!(serde_json::to_value(&adopted).unwrap(), serde_json::to_value(&session).unwrap(), "nothing was touched");
    let owner = account_stamp(&adopted);
    assert_eq!(owner, OWNER_7);
    assert!(adopted.prefs.track_memory.iter().all(|r| r.account != owner));
    assert!(adopted.prefs.subtitle_offsets.iter().all(|r| r.account.is_empty()), "offsets stay unstamped until they are written");
    assert!(adopted.prefs.profile_prefs.iter().all(|r| r.account.is_empty()));
}

/// **A Plex Home member who signs in with their own account** carries the HOME's roster, so the
/// stamp is the admin's: the admin's per-profile records are readable by the member's session
/// (the trust boundary `same_owner` already draws for pins, searches and library memory), while the
/// per-uuid keys still keep each person's own records apart. A member whose roster did not load is
/// stamped by their own token, not by the admin's identity.
#[test]
fn a_home_member_over_the_admin_reads_the_admins_records_but_per_uuid_records_stay_separate() {
    let home = vec![
        HomeUserRef { id: 7, uuid: "admin-uuid".into(), title: "Admin".into(), admin: true, ..Default::default() },
        HomeUserRef { id: 9, uuid: "member-uuid".into(), title: "Member".into(), admin: false, ..Default::default() },
    ];
    let mut member = Session { client_id: "cid-1".into(), account_token: "member-token".into(), home_users: home, ..Default::default() };
    remember_all(&mut member, "admin-uuid", OWNER_7, "admins");
    member.prefs.track_memory.upsert_with("member-uuid", OWNER_7, &tm_key("members"), |_| Some(track("members")));
    let stamp = account_stamp(&member);
    assert_eq!(stamp, OWNER_7, "the member's session carries the Home admin's identity");
    assert_eq!(reads(&member, "admin-uuid", &stamp, "admins").0, true, "the admin's profile record is visible to the member's session");
    assert!(member.prefs.track_memory.get("member-uuid", &stamp, &tm_key("members")).is_some());
    assert!(member.prefs.track_memory.get("member-uuid", &stamp, &tm_key("admins")).is_none(), "per-uuid records stay apart");
    assert!(member.prefs.track_memory.get("admin-uuid", &stamp, &tm_key("members")).is_none());

    let degraded = Session { home_users: Vec::new(), ..member };
    assert_eq!(account_stamp(&degraded), crate::plex::grant::account_key("member-token"),
        "without the roster the member is stamped by their own token");
    assert_eq!(reads(&degraded, "admin-uuid", &account_stamp(&degraded), "admins"), NONE);
}

/// A write with an owner still unknown prunes the foreign records it always pruned, but keeps the
/// owner-stamped ones: a transient roster failure must not delete a person's memory. (A write with
/// the owner KNOWN prunes every other stamp, token form included, as before.)
#[test]
fn a_write_while_the_owner_is_unknown_keeps_owner_stamped_records_for_every_kind() {
    let old = crate::plex::grant::account_key("token-old");
    let token_form = crate::plex::grant::account_key("token-new");
    let tracks = |user: &str, account: &str| ProfileList { user: user.into(), account: account.into(), items: vec![track("k1")], ..Default::default() };
    let offsets = |user: &str, account: &str| ProfileList { user: user.into(), account: account.into(), items: vec![offset("k1", 5)], ..Default::default() };
    let picks = |user: &str, account: &str| ProfilePrefsRecord { user: user.into(), account: account.into(),
        prefs: ProfilePrefs { subtitle_size: Some(SubtitleSize::Small), ..Default::default() } };
    let mut session = Session::default();
    session.prefs.track_memory = vec![tracks("u", OWNER_7), tracks("ghost", &old)];
    session.prefs.subtitle_offsets = vec![offsets("u", OWNER_7), offsets("ghost", &old)];
    session.prefs.profile_prefs = vec![picks("u", OWNER_7), picks("ghost", &old)];

    session.prefs.track_memory.upsert_with("u", &token_form, &tm_key("fresh"), |_| Some(track("fresh")));
    put_own::<SubtitlePositionPref>(&mut session.prefs.profile_prefs, "u", &token_form, SubtitlePosition::High);
    session.prefs.subtitle_offsets.upsert_with("u", &token_form, &offset_key("fresh"), |_| Some(offset("fresh", 7)));

    for (name, accounts) in [
        ("track_memory", session.prefs.track_memory.iter().map(|r| r.account.clone()).collect::<Vec<_>>()),
        ("subtitle_offsets", session.prefs.subtitle_offsets.iter().map(|r| r.account.clone()).collect()),
        ("profile_prefs", session.prefs.profile_prefs.iter().map(|r| r.account.clone()).collect()),
    ] {
        assert!(accounts.iter().any(|a| a == OWNER_7), "{name}: the owner's record was kept: {accounts:?}");
        assert!(accounts.iter().any(|a| a == &token_form), "{name}: the write landed under the token stamp: {accounts:?}");
        assert!(!accounts.iter().any(|a| a == &old), "{name}: another token's record was still pruned: {accounts:?}");
    }

    // The owner known: everything that is not the owner's own goes, the token form included.
    session.prefs.track_memory.upsert_with("u", OWNER_7, &tm_key("again"), |_| Some(track("again")));
    put_own::<SubtitlePositionPref>(&mut session.prefs.profile_prefs, "u", OWNER_7, SubtitlePosition::Low);
    session.prefs.subtitle_offsets.upsert_with("u", OWNER_7, &offset_key("again"), |_| Some(offset("again", 9)));
    assert!(session.prefs.track_memory.iter().all(|r| r.account == OWNER_7), "{:?}", session.prefs.track_memory);
    assert!(session.prefs.subtitle_offsets.iter().all(|r| r.account == OWNER_7));
    assert!(session.prefs.profile_prefs.iter().all(|r| r.account == OWNER_7));
}

/// The in-memory half of the offsets door stamps with the session's OWN stamp: the owner's when the
/// roster names one.
#[test]
fn a_session_stamps_the_offsets_it_records_with_its_owner_stamp() {
    let mut session = Session { account_token: "acct".into(), home_users: roster_of(7), ..Default::default() };
    session.set_subtitle_offset_for("u", "m", "rk-1", Some(500));
    assert_eq!(session.prefs.subtitle_offsets[0].account, OWNER_7);
    assert_eq!(session.prefs.subtitle_offsets.get("u", OWNER_7, &offset_key("rk-1")).map(|e| e.offset_ms), Some(500));
}

// ---- #830 (separate commit): the login-over-another-account check honours both spellings --------

fn admin(id: i64, uuid: &str) -> Vec<HomeUserRef> {
    vec![HomeUserRef { id, uuid: uuid.into(), title: "Synthetic".into(), admin: true, ..Default::default() }]
}

/// `same_owner` is the check the login door asks ("is this the account that was signed in?"). A
/// roster written before ids existed names its admin by uuid only; a refresh later brings the id of
/// the SAME admin. Compared by the primary spelling alone, that read as another account and cleared
/// the pins, searches and library memory.
#[test]
fn the_same_admin_is_the_same_owner_in_either_spelling() {
    assert!(same_owner(&admin(0, "synthetic-owner-7"), &admin(7, "synthetic-owner-7")), "id-less, then the id arrives");
    assert!(same_owner(&admin(7, "synthetic-owner-7"), &admin(0, "synthetic-owner-7")), "and the other way round");
    assert!(same_owner(&admin(7, "synthetic-owner-7"), &admin(7, "synthetic-owner-7")), "control: identical");
    assert!(same_owner(&admin(7, ""), &admin(7, "synthetic-owner-7")), "the id alone is the account");
    assert!(!same_owner(&admin(7, "synthetic-owner-7"), &admin(8, "synthetic-owner-8")), "another admin");
    assert!(!same_owner(&admin(0, "synthetic-owner-7"), &admin(8, "synthetic-owner-8")), "another admin, one spelling");
    assert!(!same_owner(&admin(0, ""), &admin(0, "")), "an admin that names nothing is unknown, never equal");
    assert!(!same_owner(&Vec::new(), &admin(7, "synthetic-owner-7")), "an empty roster is unknown");
    assert!(!same_owner(&admin(7, "synthetic-owner-7"), &Vec::new()));
}

/// The same check through the login door: pins, searches and library memory survive a sign-in whose
/// roster respells the SAME admin, and still go for another admin.
#[test]
fn a_login_that_respells_the_same_admin_keeps_pins_searches_and_library_memory() {
    let _serial = plx_base::testlock::serial();
    let _root = TempSession::new("relogin-respelled-admin");
    let mut first = Session {
        client_id: "cid-1".into(), account_token: "token-a".into(), home_users: admin(0, "synthetic-owner-7"),
        home_pins: vec![HomePins { user: String::new(), asked: true, ..Default::default() }],
        recent_searches: vec![RecentSearches { user: String::new(), terms: vec!["synthetic term".into()], ..Default::default() }],
        ..Default::default()
    };
    first.set_view_for("u", LibraryView { machine_id: "m".into(), key: 1, sort: "addedAt".into(), desc: true, ..Default::default() });
    let kept = |s: &Session| (s.home_pins.len(), s.recent_searches.len(), s.prefs.library_views.len());
    save(&first);
    assert_eq!(kept(&load()), (1, 1, 1), "control: all three are stored");
    login_with_roster("token-b", admin(7, "synthetic-owner-7"));
    assert_eq!(kept(&load()), (1, 1, 1), "the same admin, now with its id: kept");
    login_with_roster("token-c", admin(0, "synthetic-owner-7"));
    assert_eq!(kept(&load()), (1, 1, 1), "and respelled back: kept");
    login_with_roster("token-d", admin(8, "synthetic-owner-8"));
    assert_eq!(kept(&load()), (0, 0, 0), "another admin clears them");
}
