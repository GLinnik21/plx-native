//! A server that lists one item twice in a page: the row shows it once, at its first position, and
//! the offsets the next request uses are the listing's, not the row's.

use super::*;

/// A page of `keys` from `start`, one movie per key; a repeated key is a repeated item.
fn page(start: usize, keys: &[u32], total: usize) -> MediaContainer {
    let items: Vec<_> = keys.iter().map(|key| serde_json::json!({
        "ratingKey": key.to_string(), "type": "movie", "title": "Movie", "thumb": "/poster"})).collect();
    serde_json::from_value(serde_json::json!({"offset": start, "totalSize": total, "Metadata": items})).unwrap()
}

fn sid() -> ServerId { ServerId::from_raw(0) }

fn held(keys: std::ops::Range<usize>) -> Vec<Row> {
    keys.map(|i| (i, Arc::new(PmsMovie { rk: i.to_string(), title: "Movie".into(), thumb: "/poster".into(), ..PmsMovie::default() }))).collect()
}

fn keys_of(rows: &[Row]) -> Vec<String> { rows.iter().map(|(_, item)| item.rk.clone()).collect() }

#[test]
fn a_backward_page_that_repeats_a_key_shows_it_once() {
    // The listing, by position: 0..5, then 5 again at position 6, then 7..10. The window holds 10..14.
    let listing: Vec<u32> = vec![0, 1, 2, 3, 4, 5, 5, 7, 8, 9, 10, 11, 12, 13];
    let current = held(10..14);
    let info = PageInfo { offset: 10, end: 14, total: 14, more: true, unstable: false };
    let mut asked = Vec::new();
    let (rows, info) = fetch_window(sid(), &Ask { start: 10, before: true, hidden: &[] },
        Some((&current, info)), 0, |start, size| {
            asked.push((start, size));
            Some(page(start, &listing[start..start + size], 14))
        }).expect("reads");
    assert_eq!(keys_of(&rows), ["0", "1", "2", "3", "4", "5", "7", "8", "9", "10", "11", "12", "13"]);
    let at: Vec<usize> = rows.iter().map(|(position, _)| *position).collect();
    assert_eq!(at[5], 5, "the first occurrence keeps its listing position");
    assert_eq!(asked, [(0, 11)], "one read, from the listing's own offsets");
    assert_eq!((info.offset, info.end, info.more), (0, 14, false), "the next ask starts from the listing's start");
}

#[test]
fn a_forward_page_that_repeats_a_key_shows_it_once_and_the_next_ask_starts_at_the_listing_end() {
    let listing: Vec<u32> = vec![0, 1, 2, 2, 4, 0, 6, 7];
    let mut asked = Vec::new();
    let (rows, info) = fetch_window(sid(), &Ask { start: 0, before: false, hidden: &[] }, None, 0, |start, size| {
        asked.push((start, size));
        let end = (start + size).min(listing.len());
        Some(page(start, &listing[start..end], listing.len()))
    }).expect("reads");
    assert_eq!(keys_of(&rows), ["0", "1", "2", "4", "6", "7"]);
    assert_eq!((info.end, info.more), (8, false), "the end is the listing's, not the row's length");
    assert_eq!(asked, [(0, MAX_SHELF_ITEMS)]);
}
