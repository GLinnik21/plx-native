//! The data layer of per-show track memory: the entry and its bounds, the soft parse, the account
//! stamp, the per-profile cap and the household budget. The document it lives in (the DB8 cap, the
//! guard, sign-out) is `track_memory_document_tests`.
use super::super::{CanonicalSessionPreferences, Session};
use super::track_memory::*;
use super::{
    evict_by_rule, list_size, logical, MemoryEntry, Parse, Preferences, ProfileList, ProfileMemory, Scope, DESCRIPTORS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(super) const MACHINE: &str = "0123456789abcdef0123456789abcdef01234567";
/// A Plex Home user's `uuid` is shorter than this; the worst case is a UUID-length one.
pub(super) const LONG_USER: &str = "01234567-89ab-cdef-0123-456789abcdef";
pub(super) use crate::plex::session::test_support::STAMP;

pub(super) fn show(n: u32) -> String {
    format!("{n:016}")
}

pub(super) fn audio_pick() -> AudioMemo {
    AudioMemo { lang: "eng".into(), tag: "en-US".into(), codec: "eac3".into(), channels: 6, title: "English".into(), ..Default::default() }
}

pub(super) fn subtitle_pick() -> SubtitleMemo {
    SubtitleMemo::Track(SubtitleTrackMemo { lang: "eng".into(), kind: "text".into(), title: "English (SDH)".into(), ..Default::default() })
}

/// A typical entry (a 40-hex server, an episodic show key).
pub(super) fn typical(n: u32) -> TrackMemoryEntry {
    TrackMemoryEntry::new(MACHINE, &n.to_string(), Some(audio_pick()), Some(subtitle_pick()))
}

/// An entry with EVERY string at its bound: the largest an entry can be (`TrackMemoryEntry::worst_case`).
pub(super) fn worst(n: u32) -> TrackMemoryEntry {
    let entry = TrackMemoryEntry::worst_case(n);
    assert_eq!(entry.show_rk, show(n), "the fixture's show key is the sixteen-digit one");
    assert_eq!(entry.show_rk.len(), SHOW_RK_MAX, "the fixture's show key is at its bound");
    entry
}

fn key_of(e: &TrackMemoryEntry) -> (String, String) {
    e.key()
}

/// Write `entry` for (`user`, `STAMP`) through the door.
pub(super) fn put(list: &mut Vec<ProfileList<TrackMemoryEntry>>, user: &str, entry: TrackMemoryEntry) -> bool {
    list.upsert_with(user, STAMP, &key_of(&entry), |_| Some(entry.clone()))
}

fn shows_of(list: &[ProfileList<TrackMemoryEntry>], user: &str) -> Vec<String> {
    list.iter().find(|r| r.user == user).map(|r| r.items.iter().map(|e| e.show_rk.clone()).collect()).unwrap_or_default()
}

// ---- the row ---------------------------------------------------------------------------------

/// The list row is a `Preferences` field under its own key, written only while non-empty, and its
/// descriptor says what the plan says.
#[test]
fn the_list_row_names_its_own_field() {
    let default_json = serde_json::to_value(Preferences::default()).unwrap();
    assert!(default_json.get("track_memory").is_none(), "an empty list writes no key: {default_json}");
    let mut p = Preferences::default();
    let mut list = Vec::new();
    assert!(put(&mut list, "", typical(1)));
    p.track_memory = list;
    let body = serde_json::to_value(&p).unwrap();
    assert!(body["track_memory"].is_array(), "{body}");
    let back: Preferences = serde_json::from_value(body).unwrap();
    assert_eq!(back, p);
    let row = DESCRIPTORS.iter().find(|d| d.key == "track_memory").expect("the row is in the table");
    assert_eq!(row.scope, Scope::PerShow);
    assert!(row.account_bound && !row.live && row.settings.is_none());
    assert_eq!(row.parse, Parse::SoftVec);
}

// ---- the wire shape and the soft parse --------------------------------------------------------

#[test]
fn the_entry_serializes_the_documented_shape() {
    let entry = TrackMemoryEntry::new("m1", "42", Some(audio_pick()), Some(subtitle_pick()));
    assert_eq!(
        serde_json::to_value(&entry).unwrap(),
        json!({
            "machine_id": "m1", "show_rk": "42",
            "audio": {"lang": "eng", "tag": "en-US", "commentary": false, "ad": false, "codec": "eac3", "channels": 6, "title": "English"},
            "subtitle": {"mode": "track", "lang": "eng", "tag": "", "forced": false, "sdh": false, "kind": "text", "external": false, "title": "English (SDH)"},
        })
    );
    let audio_only = TrackMemoryEntry::new("m1", "42", Some(audio_pick()), None);
    assert!(serde_json::to_value(&audio_only).unwrap().get("subtitle").is_none(), "an unpicked half writes no key");
    let subtitle_only = TrackMemoryEntry::new("m1", "42", None, Some(subtitle_pick()));
    assert!(serde_json::to_value(&subtitle_only).unwrap().get("audio").is_none(), "nor does the other half");
}

/// `SubtitleMemo`'s three shapes round-trip, and a mode this build does not know drops ONLY that
/// half: the audio half and the entry survive, and an entry whose only half it was is dropped.
#[test]
fn subtitle_memo_has_three_shapes_and_an_unknown_mode_drops_only_that_half() {
    let shapes = [
        (
            SubtitleMemo::Track(SubtitleTrackMemo { lang: "eng".into(), forced: true, kind: "text".into(), ..Default::default() }),
            json!({"mode": "track", "lang": "eng", "tag": "", "forced": true, "sdh": false, "kind": "text", "external": false, "title": ""}),
        ),
        (SubtitleMemo::Off, json!({"mode": "off"})),
        (SubtitleMemo::ForcedOnly { lang: "eng".into() }, json!({"mode": "forced_only", "lang": "eng"})),
    ];
    for (memo, wire) in shapes {
        assert_eq!(serde_json::to_value(&memo).unwrap(), wire);
        assert_eq!(serde_json::from_value::<SubtitleMemo>(wire.clone()).unwrap(), memo);
        let entry = TrackMemoryEntry::new("m1", "42", None, Some(memo.clone()));
        let back: TrackMemoryEntry = serde_json::from_value(serde_json::to_value(&entry).unwrap()).unwrap();
        assert_eq!(back, entry, "{wire}");
    }
    // forced-only without a language is the empty language, not a failure
    assert_eq!(
        serde_json::from_value::<SubtitleMemo>(json!({"mode": "forced_only"})).unwrap(),
        SubtitleMemo::ForcedOnly { lang: String::new() }
    );

    let element = |subtitle: Value| json!({"machine_id": "m1", "show_rk": "42", "audio": {"lang": "eng"}, "subtitle": subtitle});
    let parsed: Vec<TrackMemoryEntry> = [
        element(json!({"mode": "sparkly", "lang": "eng"})), // a newer build's mode
        element(json!({"mode": "track", "forced": "yes"})), // a known mode with a field of the wrong type
        element(json!("off")),                              // not an object at all
        element(json!({"mode": "off"})),
    ]
    .into_iter()
    .map(|v| serde_json::from_value(v).expect("the entry survives with its audio half"))
    .collect();
    for unreadable in &parsed[..3] {
        assert_eq!(unreadable.subtitle, None, "the unreadable half reads as never picked");
        assert_eq!(unreadable.audio.as_ref().map(|a| a.lang.as_str()), Some("eng"), "the other half is untouched");
    }
    assert_eq!(parsed[3].subtitle, Some(SubtitleMemo::Off));

    // an entry whose ONLY half is unreadable remembers nothing, so it is dropped (no half-entry)
    let only = json!({"machine_id": "m1", "show_rk": "42", "subtitle": {"mode": "sparkly"}});
    assert!(serde_json::from_value::<TrackMemoryEntry>(only.clone()).is_err());
    let preferences: Preferences = serde_json::from_value(json!({"track_memory": [
        {"user": "", "account": STAMP, "items": [only, element(json!({"mode": "off"}))]}]})).unwrap();
    assert_eq!(preferences.track_memory[0].items.len(), 1, "only the readable element is kept");
}

/// **T-parse for the list row.** Junk is dropped per element, at both levels (a record, an entry),
/// in the struct on its own AND inside both of its on-disk hosts; the good elements survive.
#[test]
fn junk_elements_are_dropped_per_element() {
    let good = |rk: &str| json!({"machine_id": MACHINE, "show_rk": rk, "audio": {"lang": "eng"}});
    let body = json!({"track_memory": [
        "junk", 7, null, {"user": "u1", "account": STAMP, "items": 3}, // records that are not records
        {"user": "u2", "account": STAMP, "items": [
            good("1"),
            7, "x", null,                                                                  // not objects
            {"machine_id": MACHINE, "show_rk": "2"},                                        // remembers nothing
            {"machine_id": "", "show_rk": "3", "audio": {"lang": "eng"}},                   // no server
            {"machine_id": MACHINE, "show_rk": "", "audio": {"lang": "eng"}},               // no show
            {"machine_id": MACHINE, "show_rk": "r".repeat(SHOW_RK_MAX + 1), "audio": {}},   // key over its bound
            {"machine_id": "m".repeat(MACHINE_ID_MAX + 1), "show_rk": "4", "audio": {}},    // key over its bound
            {"machine_id": 5, "show_rk": "5", "audio": {}},                                 // wrong type
            {"show_rk": "6", "audio": {}},                                                  // key missing
            good("7"),
        ]},
    ]});
    let alone: Preferences = serde_json::from_value(body.clone()).unwrap();
    let legacy: Session = serde_json::from_value(body.clone()).unwrap();
    let canonical: CanonicalSessionPreferences = serde_json::from_value(body).unwrap();
    for (host, list) in [("struct", &alone.track_memory), ("legacy", &legacy.prefs.track_memory), ("canonical", &canonical.prefs.track_memory)] {
        let u2 = list.iter().find(|r| r.user == "u2").unwrap_or_else(|| panic!("{host}: the good record survives"));
        assert_eq!(u2.items.iter().map(|e| e.show_rk.as_str()).collect::<Vec<_>>(), ["1", "7"], "{host}");
        assert!(u2.account == STAMP, "{host}");
        // the records that were not records are gone; a record with no readable items is the only
        // other survivor (an emptied record is pruned by the next write, never by a read)
        assert!(list.iter().all(|r| r.user == "u2" || r.items.is_empty()), "{host}: {list:?}");
    }
    assert!(alone.track_memory.len() <= 2);
}

// ---- the bounds -------------------------------------------------------------------------------

/// A title is CUT at a character boundary, never in the middle of a character: a 2-, 3- and 4-byte
/// character straddling byte 48 leaves the longest prefix that is whole characters.
#[test]
fn a_title_is_truncated_at_a_char_boundary() {
    for ch in ['\u{e9}', '\u{20ac}', '\u{1f3ac}'] {
        let width = ch.len_utf8();
        assert!((2..=4).contains(&width));
        // `a`s up to 47 bytes short of the char's start so the char straddles byte 48 for every width
        let pad = TITLE_MAX - 1;
        let title = format!("{}{}tail", "a".repeat(pad), ch);
        assert!(title.len() > TITLE_MAX && !title.is_char_boundary(TITLE_MAX), "the fixture straddles byte {TITLE_MAX}");
        let audio = AudioMemo { title: title.clone(), ..audio_pick() };
        let entry = TrackMemoryEntry::new("m1", "1", Some(audio), None);
        let kept = &entry.audio.as_ref().unwrap().title;
        assert!(kept.len() <= TITLE_MAX, "{width}-byte char: {} bytes", kept.len());
        assert_eq!(*kept, "a".repeat(pad), "{width}-byte char: the straddling character is cut whole, the rest kept");
        // and the same bound holds when the entry is READ
        let parsed: TrackMemoryEntry = serde_json::from_value(json!({
            "machine_id": "m1", "show_rk": "1", "audio": {"title": title},
            "subtitle": {"mode": "track", "title": title}})).unwrap();
        assert_eq!(parsed.audio.unwrap().title, "a".repeat(pad));
        let SubtitleMemo::Track(track) = parsed.subtitle.unwrap() else { panic!("a track") };
        assert_eq!(track.title, "a".repeat(pad));
    }
    // a title already within its bound is untouched, multi-byte or not
    let exact = "\u{e9}".repeat(TITLE_MAX / 2);
    assert_eq!(exact.len(), TITLE_MAX);
    let entry = TrackMemoryEntry::new("m1", "1", Some(AudioMemo { title: exact.clone(), ..audio_pick() }), None);
    assert_eq!(entry.audio.unwrap().title, exact);
}

/// Every other string is DROPPED when over its bound (it reads as absent), kept when at it, and a
/// key string over its bound leaves the entry keyless, which is forgotten. The same on the read
/// path and the write path (one normaliser).
#[test]
fn an_over_long_string_is_dropped_not_truncated_and_the_bounds_hold_on_read_and_write() {
    let over = |n: usize| "x".repeat(n + 1);
    let at = |n: usize| "x".repeat(n);
    let audio = AudioMemo {
        lang: over(LANG_MAX), tag: over(TAG_MAX), codec: over(CODEC_MAX), channels: CHANNELS_MAX + 1, ..audio_pick()
    };
    let subtitle = SubtitleMemo::Track(SubtitleTrackMemo { lang: over(LANG_MAX), tag: over(TAG_MAX), kind: over(KIND_MAX), ..Default::default() });
    let built = TrackMemoryEntry::new("m1", "1", Some(audio.clone()), Some(subtitle.clone()));
    let read: TrackMemoryEntry = serde_json::from_value(json!({
        "machine_id": "m1", "show_rk": "1", "audio": audio, "subtitle": subtitle})).unwrap();
    assert_eq!(built, read, "building and reading an entry apply the same bounds");
    let a = built.audio.as_ref().unwrap();
    assert_eq!((a.lang.as_str(), a.tag.as_str(), a.codec.as_str(), a.channels), ("", "", "", 0));
    assert_eq!(a.title, "English", "the title within its bound is kept");
    let SubtitleMemo::Track(t) = built.subtitle.as_ref().unwrap() else { panic!("a track") };
    assert_eq!((t.lang.as_str(), t.tag.as_str(), t.kind.as_str()), ("", "", ""));
    let forced = TrackMemoryEntry::new("m1", "1", None, Some(SubtitleMemo::ForcedOnly { lang: over(LANG_MAX) }));
    assert_eq!(forced.subtitle, Some(SubtitleMemo::ForcedOnly { lang: String::new() }));

    // at the bound: kept whole
    let exact = AudioMemo { lang: at(LANG_MAX), tag: at(TAG_MAX), codec: at(CODEC_MAX), channels: CHANNELS_MAX, ..audio_pick() };
    let kept = TrackMemoryEntry::new(&at(MACHINE_ID_MAX), &at(SHOW_RK_MAX), Some(exact.clone()), None);
    assert_eq!(kept.audio, Some(exact));
    assert_eq!((kept.machine_id.len(), kept.show_rk.len()), (MACHINE_ID_MAX, SHOW_RK_MAX));

    // a key over its bound: no key, so the door refuses it (nothing to forget, nothing recorded)
    for (machine, rk) in [(over(MACHINE_ID_MAX), "1".to_string()), (MACHINE.to_string(), over(SHOW_RK_MAX))] {
        let mut list: Vec<ProfileList<TrackMemoryEntry>> = Vec::new();
        let entry = TrackMemoryEntry { machine_id: machine, show_rk: rk, audio: Some(audio_pick()), subtitle: None };
        assert!(entry.clone().normalized().forgets());
        assert!(!list.upsert_with("", STAMP, &key_of(&entry), |_| Some(entry.clone())), "an unkeyable entry is not recorded");
        assert!(list.is_empty());
    }

    // an over-long value handed to the WRITE door is stored bounded
    let mut list = Vec::new();
    let long_title = AudioMemo { title: "t".repeat(TITLE_MAX * 3), ..audio_pick() };
    assert!(put(&mut list, "", TrackMemoryEntry { machine_id: "m1".into(), show_rk: "1".into(), audio: Some(long_title), subtitle: None }));
    assert_eq!(list[0].items[0].audio.as_ref().unwrap().title.len(), TITLE_MAX);
}

// ---- the account stamp ------------------------------------------------------------------------

/// A record stamped for another account reads as nothing, a write PRUNES it (and an unstamped
/// record nobody can read), and a write without a stamp is refused outright.
#[test]
fn a_stale_stamp_is_ignored_and_a_write_prunes_it() {
    let stale = "ffffffffffffffff";
    let mut list: Vec<ProfileList<TrackMemoryEntry>> = vec![
        ProfileList { user: "".into(), account: stale.into(), items: vec![typical(1)], ..Default::default() },
        ProfileList { user: "u2".into(), account: String::new(), items: vec![typical(2)], ..Default::default() },
    ];
    let key = key_of(&typical(1));
    assert!(list.get("", STAMP, &key).is_none(), "the other account's record is not this account's");
    assert!(list.get("", "", &key).is_none(), "an unstamped read finds nothing");
    assert!(list.get("u2", "", &key_of(&typical(2))).is_none(), "even an unstamped record is unreadable");
    assert!(list.get("", stale, &key).is_some(), "control: its own account reads it");

    assert!(put(&mut list, "", typical(3)));
    assert_eq!(list.len(), 1, "the foreign and the unstamped record are pruned: {list:?}");
    assert_eq!(list[0].account, STAMP);
    assert_eq!(shows_of(&list, ""), ["3"]);

    // pruning is a change on its own, even when the entry written is already held
    let mut list: Vec<ProfileList<TrackMemoryEntry>> = vec![
        ProfileList { user: "".into(), account: STAMP.into(), items: vec![typical(3)], ..Default::default() },
        ProfileList { user: "".into(), account: stale.into(), items: vec![typical(1)], ..Default::default() },
    ];
    assert!(list.upsert_with("", STAMP, &key_of(&typical(3)), |cur| cur.cloned()), "the prune is reported as a change");
    assert_eq!(list.len(), 1);
    assert!(!list.upsert_with("", STAMP, &key_of(&typical(3)), |cur| cur.cloned()), "nothing left to change");

    // a write without a stamp is refused: nothing recorded, the closure is not even asked
    let mut list: Vec<ProfileList<TrackMemoryEntry>> = Vec::new();
    let mut asked = false;
    assert!(!list.upsert_with("", "", &key, |_| { asked = true; Some(typical(1)) }));
    assert!(list.is_empty() && !asked);
}

// ---- the per-profile cap ----------------------------------------------------------------------

#[test]
fn the_cap_of_64_drains_the_oldest() {
    assert_eq!(<TrackMemoryEntry as MemoryEntry>::CAP, 64);
    let mut list = Vec::new();
    for n in 0..64 {
        assert!(put(&mut list, "", typical(n)));
    }
    assert_eq!(list[0].items.len(), 64);
    // re-recording the oldest moves it to the end instead of growing the list
    assert!(put(&mut list, "", TrackMemoryEntry::new(MACHINE, "0", Some(audio_pick()), None)));
    assert_eq!(list[0].items.len(), 64);
    assert_eq!(list[0].items.last().unwrap().show_rk, "0");
    // a 65th show drains the OLDEST (now show 1), not the one just re-recorded
    assert!(put(&mut list, "", typical(64)));
    let shows = shows_of(&list, "");
    assert_eq!(shows.len(), 64);
    assert!(!shows.contains(&"1".to_string()) && shows.contains(&"0".to_string()) && shows.contains(&"64".to_string()), "{shows:?}");
}

// ---- the household budget ---------------------------------------------------------------------

/// **One profile keeps 64 worst-case entries**, every string at its bound, with a UUID-length
/// profile key and a stamp: the arithmetic the bounds exist for.
#[test]
fn one_profile_keeps_64_worst_case_entries() {
    let mut list = Vec::new();
    for n in 0..64 {
        assert!(put(&mut list, LONG_USER, worst(n)));
    }
    let size = list_size(&list);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].items.len(), 64, "the budget evicted a lone viewer's entries ({size} of {BUDGET_BYTES} bytes)");
    assert!(size <= BUDGET_BYTES, "{size} > {BUDGET_BYTES}");
    // the figures the bounds are chosen against, so a bound that grows shows here first
    assert_eq!(logical(&worst(0)), WORST_ENTRY_BYTES);
    assert_eq!(size, BUDGET_BYTES - WORST_MARGIN_BYTES, "64 worst-case entries leave {WORST_MARGIN_BYTES} bytes free");
    // the 65th is drained by the cap, not by the budget, and the list still fits
    assert!(put(&mut list, LONG_USER, worst(64)));
    assert_eq!(list[0].items.len(), 64);
    assert_eq!(list[0].items[0].show_rk, show(1));
    assert!(list_size(&list) <= BUDGET_BYTES);
}

/// The worst-case entry's logical size, and what 64 of them leave of the budget under a
/// UUID-length profile key: re-derived whenever a bound moves.
const WORST_ENTRY_BYTES: usize = 509;
const WORST_MARGIN_BYTES: usize = 40;

/// The scripted case: a heavy viewer (40), a medium one (20), then a light one writing on. The
/// heavy record gives up its OLDEST entries, in order, while the medium and the light viewer's
/// stay; and once the light viewer is itself the largest it drains its OWN oldest, never the entry
/// it just wrote.
#[test]
fn track_memory_budget_evicts_the_largest_records_oldest() {
    let mut list = Vec::new();
    for n in 0..40 {
        assert!(put(&mut list, "heavy", worst(n)));
    }
    for n in 100..120 {
        assert!(put(&mut list, "medium", worst(n)));
    }
    assert!(list_size(&list) <= BUDGET_BYTES, "60 worst-case entries fit");
    let medium_before = shows_of(&list, "medium");
    assert_eq!(medium_before.len(), 20);

    let mut light_written = 0u32;
    for n in 200..215 {
        assert!(put(&mut list, "light", worst(n)));
        light_written += 1;
        let size = list_size(&list);
        assert!(size <= BUDGET_BYTES, "the list is brought back under the budget on every write: {size}");
        let light = shows_of(&list, "light");
        assert_eq!(light.last(), Some(&show(n)), "the writer's newest pick always stays");
        assert_eq!(light.len() as u32, light_written, "the light viewer lost nothing while a heavier one existed");
        assert_eq!(shows_of(&list, "medium"), medium_before, "the medium viewer is untouched while the heavy one is larger");
        // the heavy viewer lost from the FRONT only: what remains is the newest of what it wrote
        let heavy = shows_of(&list, "heavy");
        let expected: Vec<String> = (40 - heavy.len() as u32..40).map(show).collect();
        assert_eq!(heavy, expected, "oldest first");
        // and it lost nothing it did not have to: one more entry would not have fit
        if heavy.len() < 40 {
            assert!(size + logical(&worst(0)) + 1 > BUDGET_BYTES, "over-evicted: {size} bytes held, heavy has {}", heavy.len());
        }
    }
    let heavy_after_phase_one = shows_of(&list, "heavy").len();
    assert!(heavy_after_phase_one < 40, "the scenario must actually run into the budget");

    // keep writing until the light viewer is the largest: it then gives up ITS oldest
    for n in 215..260 {
        assert!(put(&mut list, "light", worst(n)));
        assert!(list_size(&list) <= BUDGET_BYTES);
        assert_eq!(shows_of(&list, "light").last(), Some(&show(n)), "the writer's newest pick always stays");
    }
    let light = shows_of(&list, "light");
    let (heavy, medium) = (shows_of(&list, "heavy").len(), shows_of(&list, "medium").len());
    assert!(light.len() < 60, "the light viewer stopped growing once it was the largest ({} held)", light.len());
    assert!(light.len() >= heavy && light.len() >= medium, "light {}, heavy {heavy}, medium {medium}", light.len());
    let newest_run: Vec<String> = (260 - light.len() as u32..260).map(show).collect();
    assert_eq!(light, newest_run, "what it keeps is its own newest run: it drained its OLDEST");
}

/// A synthetic tenant with a tiny budget, to grade the eviction RULE on exact counts. Each
/// one-letter record costs `51 + 24 * entries` logical bytes.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
struct Tiny {
    k: String,
    v: String,
}
impl MemoryEntry for Tiny {
    type Key = String;
    const CAP: usize = 100;
    const ACCOUNT_BOUND: bool = true;
    const BUDGET: Option<usize> = Some(TINY_BUDGET);
    fn key(&self) -> String {
        self.k.clone()
    }
    fn forgets(&self) -> bool {
        self.k.is_empty()
    }
    fn slot(_: &Session) -> &Vec<ProfileList<Self>> {
        unreachable!("the synthetic tenant lives in no session")
    }
    fn slot_mut(_: &mut Session) -> &mut Vec<ProfileList<Self>> {
        unreachable!("the synthetic tenant lives in no session")
    }
}
/// Three one-letter profiles are `157 + 24 * entries` bytes: 17 entries fit (565), 18 do not (589).
const TINY_BUDGET: usize = 580;

fn tiny(k: u32, v: &str) -> Tiny {
    Tiny { k: format!("{k:03}"), v: v.to_string() }
}
fn tiny_record(user: &str, ks: std::ops::Range<u32>) -> ProfileList<Tiny> {
    ProfileList { user: user.into(), account: STAMP.into(), items: ks.map(|k| tiny(k, "abcde")).collect(), ..Default::default() }
}
fn tiny_put(list: &mut Vec<ProfileList<Tiny>>, user: &str, k: u32) -> bool {
    let entry = tiny(k, "abcde");
    list.upsert_with(user, STAMP, &entry.k.clone(), |_| Some(entry))
}
fn tiny_counts(list: &[ProfileList<Tiny>]) -> Vec<(&str, usize)> {
    list.iter().map(|r| (r.user.as_str(), r.items.len())).collect()
}
fn tiny_keys(list: &[ProfileList<Tiny>], user: &str) -> Vec<String> {
    list.iter().find(|r| r.user == user).map(|r| r.items.iter().map(|e| e.k.clone()).collect()).unwrap_or_default()
}

/// The tie rules, on exact counts: the MOST entries goes first; on a tie a record other than the
/// writer's; among those the earlier record; and the writer's lone newest pick is never taken.
#[test]
fn the_eviction_rule_on_exact_counts() {
    // a6, b6, c5: 17 entries, just under the budget
    let mut list = vec![tiny_record("a", 1..7), tiny_record("b", 11..17), tiny_record("c", 21..26)];
    assert_eq!(list_size(&list), 565);
    assert!(list_size(&list) <= TINY_BUDGET);

    // c writes: a 3-way tie at 6. A record other than the writer's goes first, the earlier of them
    assert!(tiny_put(&mut list, "c", 900));
    assert_eq!(tiny_counts(&list), [("a", 5), ("b", 6), ("c", 6)]);
    assert_eq!(tiny_keys(&list, "a"), ["002", "003", "004", "005", "006"], "and it is that record's OLDEST entry that went");
    assert_eq!(tiny_keys(&list, "b").len(), 6);

    // c writes again: c is now the largest (7) although it is the writer, so its OLDEST goes
    assert!(tiny_put(&mut list, "c", 901));
    assert_eq!(tiny_counts(&list), [("a", 5), ("b", 6), ("c", 6)]);
    assert_eq!(tiny_keys(&list, "c"), ["022", "023", "024", "025", "900", "901"], "the newest pick stays, the oldest went");
    assert_eq!(tiny_keys(&list, "a").len(), 5, "and nobody else lost anything");

    // a light writer is never evicted by a heavy one: "a" (22 entries, 581 bytes) is over the budget
    // alone, "z" arrives with one entry, and it is "a" that pays, its four oldest entries (657 -> 561, 24 a piece)
    let mut list = vec![tiny_record("a", 1..23)];
    assert!(list_size(&list) > TINY_BUDGET);
    assert!(tiny_put(&mut list, "z", 500));
    assert_eq!(tiny_keys(&list, "z"), ["500"]);
    assert_eq!(tiny_keys(&list, "a"), (5..23).map(|k| format!("{k:03}")).collect::<Vec<_>>());
    assert_eq!(list_size(&list), 561);

    // a lone writer with a single entry is never evicted, even if it alone is over the budget
    let mut lone = vec![tiny_record("solo", 1..2)];
    let (evicted, freed) = evict_by_rule(&mut lone, Some(("solo", STAMP)), |_| true);
    assert_eq!((evicted, freed), (0, 0));
    assert_eq!(lone[0].items.len(), 1);
    // the same record with NO writer named (the document guard) is evicted, and the list empties
    let (evicted, _) = evict_by_rule(&mut lone, None, |_| true);
    assert_eq!(evicted, 1);
    assert!(lone.is_empty());
}

/// **The byte account is exact.** `evict_by_rule` counts what it frees without re-serializing the
/// list; after EVERY number of evictions the list's fresh measure equals the starting size minus
/// that count, through records of every length and the removal of whole records.
#[test]
fn evict_exactness() {
    let mut list: Vec<ProfileList<TrackMemoryEntry>> = Vec::new();
    for (user, count) in [("a", 5u32), ("b", 3), ("c", 1), ("d", 4)] {
        for n in 0..count {
            assert!(put(&mut list, user, if n % 2 == 0 { worst(n) } else { typical(n) }));
        }
    }
    let start = list_size(&list);
    let total: usize = list.iter().map(|r| r.items.len()).sum();
    assert_eq!(total, 13);
    for take in 1..=total {
        let mut probe = list.clone();
        let mut asked = 0;
        let (evicted, freed) = evict_by_rule(&mut probe, None, |_| {
            asked += 1;
            asked <= take
        });
        assert_eq!(evicted, take);
        assert_eq!(list_size(&probe), start - freed, "after {take} evictions");
    }
    let mut all = list.clone();
    let (evicted, freed) = evict_by_rule(&mut all, None, |_| true);
    assert_eq!((evicted, freed), (13, start - 2), "everything out leaves the empty list's two bytes");
    assert!(all.is_empty());
}

/// The rule against a naive reference written from the spec: a random walk of writes from four
/// profiles, the reference re-measuring the whole list per step and picking by scanning counts.
#[test]
fn eviction_matches_a_naive_reference_over_a_random_walk() {
    fn reference(list: &mut Vec<ProfileList<Tiny>>, writer: &str) -> usize {
        let mut evicted = 0;
        while logical(&*list) > TINY_BUDGET {
            let max = list.iter().map(|r| r.items.len()).max().unwrap();
            let tied: Vec<usize> = (0..list.len()).filter(|&i| list[i].items.len() == max).collect();
            let pick = tied.iter().copied().find(|&i| list[i].user != writer).unwrap_or(tied[0]);
            if list[pick].user == writer && list[pick].items.len() == 1 {
                break;
            }
            list[pick].items.remove(0);
            evicted += 1;
            if list[pick].items.is_empty() {
                list.remove(pick);
            }
        }
        evicted
    }
    let (mut door, mut naive): (Vec<ProfileList<Tiny>>, Vec<ProfileList<Tiny>>) = (Vec::new(), Vec::new());
    let mut seed = 0x2545_f491_u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) as u32
    };
    let mut evictions = 0;
    for step in 0..400u32 {
        let user = ["a", "b", "c", "d"][(next() % 4) as usize];
        // few distinct keys, so re-recording an existing show is exercised as well as growth
        let entry = tiny(next() % 24 + 1, &format!("{step:05}"));
        door.upsert_with(user, STAMP, &entry.k.clone(), |_| Some(entry.clone()));
        match naive.iter().position(|r| r.user == user) {
            Some(i) => {
                naive[i].items.retain(|e| e.k != entry.k);
                naive[i].items.push(entry.clone());
            }
            None => naive.push(ProfileList { user: user.into(), account: STAMP.into(), items: vec![entry.clone()], ..Default::default() }),
        }
        evictions += reference(&mut naive, user);
        assert_eq!(door, naive, "step {step}: {user} wrote {}", entry.k);
        assert!(logical(&door) <= TINY_BUDGET);
    }
    assert!(evictions > 20, "the walk must keep running into the budget ({evictions})");
}

