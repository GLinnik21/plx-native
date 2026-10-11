//! `Ledger::note` against the algorithm it replaced: same ledger for every window, and an index
//! that still answers every position.

use super::*;

fn window(keys: &[u32]) -> Vec<Row> {
    keys.iter().map(|key| (0, Arc::new(PmsMovie { rk: key.to_string(), ..PmsMovie::default() }))).collect()
}

fn ledger(keys: &[u32]) -> Ledger {
    Ledger::from_rows(&window(keys), 0)
}

fn names(ledger: &Ledger) -> Vec<LedgerKey> { ledger.keys.to_vec() }

/// The algorithm `Ledger::note` replaced: the whole list copied and its index rebuilt, with a
/// linear search for every lookup.
fn noted_by_scan(held: &[u32], rows: &[u32]) -> Vec<LedgerKey> {
    let mut ledger: Vec<LedgerKey> = held.iter().map(|key| LedgerKey::new(&key.to_string())).collect();
    let keys: Vec<LedgerKey> = rows.iter().map(|key| LedgerKey::new(&key.to_string())).collect();
    let Some(anchor) = keys.iter().position(|key| ledger.contains(key)) else {
        ledger.extend(keys);
        return ledger;
    };
    let at = ledger.iter().position(|k| *k == keys[anchor]).unwrap_or(0);
    for key in keys[..anchor].iter().rev() { ledger.insert(at, key.clone()); }
    let mut cursor = at + anchor;
    for key in &keys[anchor..] {
        match ledger.iter().position(|k| k == key) {
            Some(index) => cursor = index + 1,
            None => { ledger.insert(cursor, key.clone()); cursor += 1; }
        }
    }
    ledger
}

#[test]
fn note_matches_the_scanning_algorithm() {
    let held = [10, 11, 12, 13, 14, 15];
    let windows: [&[u32]; 12] = [
        &[], &[12, 13, 14], &[13, 12], &[1, 2, 3], &[1, 12, 13], &[11, 12, 20, 21, 14],
        &[20, 10, 11], &[14, 15, 16, 17], &[10, 12, 15, 30], &[40, 13, 41, 11, 42],
        &[15, 14, 13, 99], &[10, 11, 12, 13, 14, 15],
    ];
    for rows in windows {
        let mut noted = ledger(&held);
        noted.note(&window(rows));
        assert_eq!(names(&noted), noted_by_scan(&held, rows), "window {rows:?}");
        for (at, key) in noted.keys.iter().enumerate() {
            assert_eq!(noted.keys.position_of(key), Some(at), "index after {rows:?}");
        }
        assert_eq!(noted.keys.index_len(), noted.keys.len());
    }
}

#[test]
fn note_of_a_stable_listing_leaves_the_storage_shared() {
    let mut noted = ledger(&[1, 2, 3, 4]);
    let before = noted.keys.clone();
    noted.note(&window(&[2, 3]));
    noted.note(&window(&[4, 1]));
    assert!(noted.keys.shares(&before));
}

#[test]
fn note_inserted_in_the_middle_and_removed_keys() {
    let mut noted = ledger(&[1, 2, 5, 6]);
    noted.note(&window(&[2, 3, 4, 5]));
    assert_eq!(names(&noted), noted_by_scan(&[1, 2, 5, 6], &[2, 3, 4, 5]));
    noted.keys.retain(|key| *key != LedgerKey::new("3"));
    noted.note(&window(&[2, 3, 4]));
    assert_eq!(names(&noted), noted_by_scan(&[1, 2, 4, 5, 6], &[2, 3, 4]));
}

#[test]
fn note_records_a_repeated_key_once_and_keeps_the_index_true() {
    for (held, rows) in [
        (&[10u32, 11, 12][..], &[7u32, 7, 11, 12][..]),
        (&[10, 11, 12][..], &[7, 8, 7, 11][..]),
        (&[10, 11, 12][..], &[7, 7, 8][..]),
        (&[10, 11, 12][..], &[11, 20, 20, 12][..]),
    ] {
        let mut noted = ledger(held);
        noted.note(&window(rows));
        let list = names(&noted);
        let unique: HashSet<_> = list.iter().cloned().collect();
        assert_eq!(unique.len(), list.len(), "a key twice in the ledger after {rows:?}");
        for (at, key) in list.iter().enumerate() {
            assert_eq!(noted.keys.position_of(key), Some(at), "index after {rows:?}");
        }
        assert_eq!(noted.keys.index_len(), list.len());
    }
}
