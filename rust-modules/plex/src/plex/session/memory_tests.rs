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
    assert_eq!(session.subtitle_offsets.len(), 1);
    assert_eq!(session.subtitle_offsets[0].items.len(), 1, "empty ids and an offset of 0 record nothing");
    assert_eq!(session.subtitle_offset_for("u", "m", "rk-1"), Some(500));
    let key = ("m".to_string(), "rk-1".to_string());
    assert_eq!(session.subtitle_offsets.get("u", "", &key).map(|e| e.offset_ms), Some(500));
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
        cur.with_memory::<SubtitleOffsetEntry>(|m| {
            m.upsert_with("u", "", &key, |_| {
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
