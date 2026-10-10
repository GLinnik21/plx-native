//! A Library section's Recommended rows on the sliding window and the row ring: every row and card
//! reachable, held cards bounded, a row returning at the offset it left. The server is a closure.

use super::*;

type Container = plx_plex::plex::MediaContainer;

const ID: &str = "movie.genre";
const KEY: &str = "/hubs/sections/1/genre/5";

fn sid() -> ServerId { ServerId::from_raw(0) }

fn item(key: usize) -> serde_json::Value {
    serde_json::json!({"ratingKey": key.to_string(), "type": "movie", "title": "Movie", "thumb": "/poster"})
}

fn container(offset: usize, total: usize, keys: impl Iterator<Item = usize>) -> Container {
    serde_json::from_value(serde_json::json!({"offset": offset, "totalSize": total,
        "Metadata": keys.map(item).collect::<Vec<_>>()})).unwrap()
}

/// A hub list: `rows` hubs of twelve cards each, row `r` card `i` keyed `r * 1000 + i`.
fn hub_list(rows: usize) -> Container {
    let hubs: Vec<_> = (0..rows).map(|r| serde_json::json!({
        "hubIdentifier": format!("movie.row{r}"), "title": format!("Row {r}"), "type": "movie",
        "key": format!("/hubs/sections/1/row{r}"), "more": true, "size": 12, "totalSize": 500,
        "Metadata": (0..12).map(|i| item(r * 1000 + i)).collect::<Vec<_>>()})).collect();
    serde_json::from_value(serde_json::json!({"Hub": hubs})).unwrap()
}

/// One row whose preview is `preview` (rating keys) of a listing of `len`.
fn one_row(preview: &[usize], len: usize) -> Container {
    serde_json::from_value(serde_json::json!({"Hub": [{
        "hubIdentifier": ID, "title": "Genre", "type": "movie", "key": KEY, "more": true, "size": preview.len(),
        "totalSize": len, "Metadata": preview.iter().copied().map(item).collect::<Vec<_>>()}]})).unwrap()
}

fn published(mc: &Container) -> SecHubs {
    let mut hubs = SecHubs { armed: true, ..Default::default() };
    hubs.land_ok(parse_hubs(mc, sid(), 1).shelves);
    assert!(hubs.commit_staged(true));
    hubs
}

fn keys(shelf: &Shelf) -> Vec<String> { shelf.items.iter().map(|item| item.rk.clone()).collect() }

/// The batch read the ledger and a sample row's preview go through: one row per key, in order.
fn many(keys: &[String]) -> Option<Container> {
    Some(serde_json::from_value(serde_json::json!({
        "Metadata": keys.iter().map(|key| item(key.parse().unwrap())).collect::<Vec<_>>()})).unwrap())
}

/// One page the way the worker does it, landed the way the store does.
fn step(hubs: &mut SecHubs, ask: &RowAsk, list: impl FnMut(usize, usize) -> Option<Container>) -> bool {
    let window = hubs.committed.iter().find(|shelf| shelf.is(&ask.id, &ask.key)).cloned();
    let read = window.and_then(|window| read_shelf(sid(), &window, ask, list, many));
    hubs.land_row(ask, read)
}

fn walk(hubs: &mut SecHubs, before: bool, mut list: impl FnMut(usize, usize) -> Option<Container>) -> Vec<Vec<String>> {
    let mut windows = vec![keys(&hubs.committed[0])];
    for _ in 0..400 {
        if !hubs.want_page(ID, KEY, before) { break; }
        let ask = hubs.next_ask().unwrap();
        step(hubs, &ask, &mut list);
        assert!(hubs.committed[0].items.len() <= MAX_SHELF_ITEMS, "the window is at most 24 cards");
        windows.push(keys(&hubs.committed[0]));
    }
    windows
}

fn held_cards(hubs: &SecHubs) -> usize { hubs.committed.iter().map(|shelf| shelf.items.len()).sum() }

#[test]
fn a_500_card_section_row_that_is_not_recently_added_pages_to_its_end_and_back() {
    let len = 500;
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    let out = walk(&mut hubs, false, listing);
    let seen: std::collections::BTreeSet<usize> = out.iter().flatten().map(|key| key.parse().unwrap()).collect();
    assert_eq!(seen.len(), len, "every card of the row was reached");
    assert_eq!(hubs.committed[0].items.last().unwrap().rk, "499");
    assert!(!hubs.want_page(ID, KEY, false), "the end of the listing ends the row");
    let back = walk(&mut hubs, true, listing);
    assert_eq!(hubs.committed[0].offset, 0);
    assert_eq!(keys(&hubs.committed[0])[0], "0", "and the way back reaches the head");
    assert!(back.len() > 10);
}

#[test]
fn a_random_section_row_shows_every_item_once() {
    let len = 90;
    // the preview is a shuffled sample of a stable listing
    let preview = [41, 7, 63, 2, 88, 19, 50, 33, 71, 5, 26, 80];
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&preview, len));
    let out = walk(&mut hubs, false, listing);
    let mut first_seen = Vec::new();
    for window in &out {
        for key in window {
            if !first_seen.contains(key) { first_seen.push(key.clone()); }
        }
    }
    let mut sorted: Vec<usize> = first_seen.iter().map(|key| key.parse().unwrap()).collect();
    assert_eq!(first_seen[..12], preview.iter().map(usize::to_string).collect::<Vec<_>>()[..], "the preview leads");
    sorted.sort_unstable();
    assert_eq!(sorted, (0..len).collect::<Vec<_>>(), "every item of the listing, none twice");
    assert!(out.iter().all(|window| window.iter().collect::<std::collections::HashSet<_>>().len() == window.len()));
}

#[test]
fn a_listing_that_reshuffles_between_requests_still_shows_every_item_once() {
    let len = 80;
    let mut request = 0;
    let listing = move |start: usize, size: usize| {
        request += 1;
        // a stable head for the probe and the first page, then a listing that moves on every read
        let key = |p: usize| if request <= 2 { p } else { (p * 7 + request * 13) % len };
        Some(container(start, len, (start..(start + size).min(len)).map(key)))
    };
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    let out = walk(&mut hubs, false, listing);
    let mut seen: Vec<String> = Vec::new();
    for window in &out {
        for key in window {
            if !seen.contains(key) { seen.push(key.clone()); }
        }
    }
    assert_eq!(seen.len(), len, "the ledger reaches every item");
}

#[test]
fn a_row_with_a_key_the_pager_does_not_admit_keeps_its_preview() {
    let mc: Container = serde_json::from_value(serde_json::json!({"Hub": [{
        "hubIdentifier": "test.section.unadmitted", "title": "Playlist", "type": "movie",
        "key": "/playlists/9/items", "more": true, "size": 12, "totalSize": 40,
        "Metadata": (0..12).map(item).collect::<Vec<_>>()}]})).unwrap();
    let mut hubs = published(&mc);
    assert_eq!(hubs.committed[0].items.len(), 12, "the preview stays");
    assert!(!hubs.committed[0].more, "and there is nothing to ask for");
    assert!(!hubs.want_page("test.section.unadmitted", "/playlists/9/items", false));
    hubs.set_hold(300, 310);
    assert_eq!(hubs.committed[0].items.len(), 12, "a row that cannot be read again is never released");
}

#[test]
fn a_400_row_section_is_all_there_and_holds_at_most_16_rows_of_cards() {
    let rows = 400;
    let mut hubs = published(&hub_list(rows));
    assert_eq!(hubs.committed.len(), rows, "no row is left off");
    assert!(held_cards(&hubs) <= HOLD_ROWS * MAX_SHELF_ITEMS);
    let mut reached = vec![false; rows];
    let listing = |key: &str, start: usize, size: usize| {
        let row: usize = key.trim_start_matches("/hubs/sections/1/row").parse().unwrap();
        Some(container(start, 500, (start..(start + size).min(500)).map(move |i| row * 1000 + i)))
    };
    let mut hold = |hubs: &mut SecHubs, top: usize| {
        hubs.set_hold(top.saturating_sub(2), (top + 4).min(rows - 1));
        for _ in 0..40 {
            let Some(ask) = hubs.next_ask() else { break };
            let key = ask.key.clone();
            step(hubs, &ask, |start, size| listing(&key, start, size));
            assert!(held_cards(hubs) <= HOLD_ROWS * MAX_SHELF_ITEMS, "held cards stay within the ring");
        }
        let (lo, hi) = hubs.held();
        for row in lo..=hi.min(rows - 1) {
            assert!(!hubs.committed[row].released(), "row {row} is held, so it has its cards");
            reached[row] = true;
        }
    };
    for top in (0..rows).step_by(3) { hold(&mut hubs, top); }
    for top in (0..rows).step_by(3).rev() { hold(&mut hubs, top); }
    assert!(reached.iter().all(|&reached| reached), "every row was reached");
    assert_eq!(hubs.committed.len(), rows);
}

#[test]
fn a_row_the_ring_took_comes_back_at_the_offset_it_left() {
    let mut hubs = published(&hub_list(40));
    let listing = |key: &str, start: usize, size: usize| {
        let row: usize = key.trim_start_matches("/hubs/sections/1/row").parse().unwrap();
        Some(container(start, 500, (start..(start + size).min(500)).map(move |i| row * 1000 + i)))
    };
    let key = "/hubs/sections/1/row2";
    for _ in 0..4 {
        assert!(hubs.want_page("movie.row2", key, false));
        let ask = hubs.next_ask().unwrap();
        step(&mut hubs, &ask, |start, size| listing(key, start, size));
    }
    let (offset, first) = (hubs.committed[2].offset, hubs.committed[2].items[0].rk.clone());
    assert!(offset > 0);
    hubs.set_hold(20, 30);
    assert!(hubs.committed[2].released(), "row 2 gave its cards up");
    assert_eq!(hubs.committed[2].slots(), hubs.committed[2].shown, "and keeps its card count as slots");
    hubs.set_hold(0, 6);
    // every held row is read again, one at a time, in order
    while let Some(ask) = hubs.next_ask() {
        assert!(ask.reload);
        let row_key = ask.key.clone();
        step(&mut hubs, &ask, |start, size| listing(&row_key, start, size));
    }
    assert_eq!((hubs.committed[2].offset, hubs.committed[2].items[0].rk.clone()), (offset, first));
}

#[test]
fn a_refresh_keeps_the_window_a_row_was_walked_to() {
    let mut hubs = published(&hub_list(5));
    let key = "/hubs/sections/1/row1";
    let listing = |start: usize, size: usize| Some(container(start, 500, (start..(start + size).min(500)).map(|i| 1000 + i)));
    assert!(hubs.want_page("movie.row1", key, false));
    let ask = hubs.next_ask().unwrap();
    step(&mut hubs, &ask, listing);
    let walked = (hubs.committed[1].offset, keys(&hubs.committed[1]));
    hubs.land_ok(parse_hubs(&hub_list(5), sid(), 1).shelves);
    assert!(hubs.commit_staged(true));
    assert_eq!((hubs.committed[1].offset, keys(&hubs.committed[1])), walked);
}
