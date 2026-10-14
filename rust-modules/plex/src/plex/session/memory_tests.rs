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
    let mut first = Session { client_id: "cid-1".into(), account_token: "acct-1".into(), ..Default::default() };
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

    let login_as = |token: &str| {
        let token = token.to_string();
        replace_after_reauthentication_with_outcome(|_| true, move |cur| Session { account_token: token, ..cur.clone() })
            .expect("not fenced")
            .expect("written");
    };
    login_as("acct-1");
    assert_eq!(views(&load()), (1, 1), "the same account logging in again keeps its views");
    login_as("acct-2");
    assert_eq!(views(&load()), (0, 0), "another account does not inherit them");
}
