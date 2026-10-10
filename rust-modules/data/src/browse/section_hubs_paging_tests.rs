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
        if !hubs.want_page(hubs.revision, ID, KEY, before) { break; }
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
    assert!(!hubs.want_page(hubs.revision, ID, KEY, false), "the end of the listing ends the row");
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
fn a_rescan_through_more_than_a_window_of_known_keys_does_not_end_the_row() {
    let (len, known) = (1000, 600);
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    // The row has shown 600 keys through its ledger and is on a rescan of the listing from 0.
    let shelf = &mut Arc::make_mut(&mut hubs.committed)[0];
    let window: Vec<usize> = (known - 12..known).collect();
    shelf.items = window.iter().map(|k| PmsMovie { rk: k.to_string(), sid: sid(), ..Default::default() }).collect();
    shelf.positions = window.clone();
    (shelf.offset, shelf.end, shelf.more) = (known - 12, known, true);
    shelf.row.ledger = Some(paging::Ledger { keys: (0..known).map(|k| paging::LedgerKey::new(&k.to_string())).collect(),
        active: true, next: 0, rescans: 1, ..Default::default() });
    let mut reached = std::collections::HashSet::new();
    for _ in 0..400 {
        if !hubs.want_page(hubs.revision, ID, KEY, false) { break; }
        let ask = hubs.next_ask().unwrap();
        step(&mut hubs, &ask, listing);
        reached.extend(keys(&hubs.committed[0]));
    }
    assert!(reached.contains("999"), "the rescan reached the last item");
    assert_eq!(reached.len(), len - known + 12, "every unseen item, none twice");
    assert!(!hubs.committed[0].more);
}

/// The shelf's repeat ladder reads a landing that moved nothing it sees as an ask nobody answered
/// unless the row says a page landed: the count moves on every landing, fruitful or not, and not on
/// a failure, and the 1000-item row with 600 known keys ends after one ask per landing.
#[test]
fn every_landing_on_a_section_row_moves_its_epoch_and_a_failure_does_not_and_the_walk_ends() {
    let (len, known) = (1000, 600);
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    let shelf = &mut Arc::make_mut(&mut hubs.committed)[0];
    let window: Vec<usize> = (known - 12..known).collect();
    shelf.items = window.iter().map(|k| PmsMovie { rk: k.to_string(), sid: sid(), ..Default::default() }).collect();
    shelf.positions = window.clone();
    (shelf.offset, shelf.end, shelf.more) = (known - 12, known, true);
    shelf.row.ledger = Some(paging::Ledger { keys: (0..known).map(|k| paging::LedgerKey::new(&k.to_string())).collect(),
        active: true, next: 0, rescans: 1, ..Default::default() });
    let epoch = |hubs: &SecHubs| hubs.committed[0].epoch;
    assert!(hubs.want_page(hubs.revision, ID, KEY, false));
    let ask = hubs.next_ask().unwrap();
    let before = epoch(&hubs);
    step(&mut hubs, &ask, |_, _| None);
    assert_eq!(epoch(&hubs), before, "a failure is not a landing");
    let (mut asks, mut fruitless) = (0, 0);
    // the failed ask stands and the store's backoff gives it back; after that, one ask per landing
    for _ in 0..100_000 { if hubs.next_ask().is_some() { break; } hubs.tick(); }
    loop {
        let ask = match hubs.next_ask() {
            Some(ask) => ask,
            None if hubs.want_page(hubs.revision, ID, KEY, false) => hubs.next_ask().unwrap(),
            None => break,
        };
        let (e, items, end) = (epoch(&hubs), keys(&hubs.committed[0]), hubs.committed[0].end);
        step(&mut hubs, &ask, listing);
        asks += 1;
        assert_eq!(epoch(&hubs), e + 1, "every landing moves the count");
        if keys(&hubs.committed[0]) == items && hubs.committed[0].end == end { fruitless += 1; }
        assert!(asks < 400, "the walk must end");
    }
    assert!(!hubs.committed[0].more, "the row ended at the listing's end");
    assert!(fruitless > 0, "the rescan has landings that move nothing the shelf sees");
    assert!(fruitless < 100, "and they are bounded: {fruitless}");
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
    assert!(!hubs.want_page(hubs.revision, "test.section.unadmitted", "/playlists/9/items", false));
    hubs.set_hold(300, 310);
    assert!(hubs.committed[0].released(), "a row with no key to page by gives its cards up all the same");
    assert_eq!(hubs.committed[0].row.kept.len(), 12, "and keeps the rating keys it showed");
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
        assert!(hubs.want_page(hubs.revision, "movie.row2", key, false));
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
    assert!(hubs.want_page(hubs.revision, "movie.row1", key, false));
    let ask = hubs.next_ask().unwrap();
    step(&mut hubs, &ask, listing);
    let walked = (hubs.committed[1].offset, keys(&hubs.committed[1]));
    hubs.land_ok(parse_hubs(&hub_list(5), sid(), 1).shelves);
    assert!(hubs.commit_staged(true));
    assert_eq!((hubs.committed[1].offset, keys(&hubs.committed[1])), walked);
}

/// A hub list of rows whose key the pager does not admit: row `r` card `i` keyed `r * 1000 + i`.
fn unpaged_hub_list(rows: usize) -> Container {
    let hubs: Vec<_> = (0..rows).map(|r| serde_json::json!({
        "hubIdentifier": format!("movie.list{r}"), "title": format!("List {r}"), "type": "movie",
        "key": format!("/playlists/{r}/items"), "more": false, "size": 12, "totalSize": 12,
        "Metadata": (0..12).map(|i| item(r * 1000 + i)).collect::<Vec<_>>()})).collect();
    serde_json::from_value(serde_json::json!({"Hub": hubs})).unwrap()
}

#[test]
fn a_400_row_section_of_unpaged_rows_holds_at_most_16_rows_and_each_row_returns_with_its_cards() {
    let rows = 400;
    let mut hubs = published(&unpaged_hub_list(rows));
    assert_eq!(hubs.committed.len(), rows, "no row is left off");
    let shown: Vec<Vec<String>> = hubs.committed.iter().map(|shelf| (0..12).map(|i| {
        let row: usize = shelf.id.trim_start_matches("movie.list").parse().unwrap();
        (row * 1000 + i).to_string()
    }).collect()).collect();
    let hold = |hubs: &mut SecHubs, top: usize| {
        hubs.set_hold(top.saturating_sub(2), (top + 4).min(rows - 1));
        while let Some(ask) = hubs.next_ask() {
            assert!(ask.reload, "an unpaged row is only ever read again");
            step(hubs, &ask, |_, _| panic!("a row with no key to page by never reads a listing"));
            assert!(held_cards(hubs) <= HOLD_ROWS * MAX_SHELF_ITEMS, "held cards stay within the ring");
        }
        assert!(held_cards(hubs) <= HOLD_ROWS * 12, "at most 16 rows of cards are held");
        let (lo, hi) = hubs.held();
        for row in lo..=hi.min(rows - 1) {
            assert_eq!(keys(&hubs.committed[row]), shown[row], "row {row} shows the cards it showed");
        }
    };
    for top in (0..rows).step_by(3) { hold(&mut hubs, top); }
    for top in (0..rows).step_by(3).rev() { hold(&mut hubs, top); }
    assert!(hubs.committed.iter().filter(|shelf| shelf.released()).count() >= rows - HOLD_ROWS);
}

#[test]
fn a_random_row_the_ring_took_comes_back_with_the_cards_it_showed() {
    let preview = [41, 7, 63, 2, 88, 19, 50, 33, 71, 5, 26, 80];
    let mut hubs = published(&one_row(&preview, 90));
    hubs.set_hold(20, 30);
    assert!(hubs.committed[0].released(), "an unprobed row gives its cards up");
    hubs.set_hold(0, 6);
    let ask = hubs.next_ask().unwrap();
    step(&mut hubs, &ask, |_, _| panic!("a sample is read by its keys, never by its listing"));
    assert_eq!(keys(&hubs.committed[0]), preview.iter().map(usize::to_string).collect::<Vec<_>>());
}

#[test]
fn a_refresh_gives_an_unpaged_row_the_cards_it_now_shows() {
    let mut hubs = published(&unpaged_hub_list(30));
    hubs.set_hold(20, 26);
    assert!(hubs.committed[0].released());
    let mut fresh = parse_hubs(&unpaged_hub_list(30), sid(), 1).shelves;
    fresh[0].items.truncate(5);
    hubs.land_ok(fresh);
    assert!(hubs.commit_staged(true));
    assert_eq!(hubs.committed[0].row.kept.len(), 5, "the refresh's own preview is what the row shows now");
}

/// A frame captures its views before its landings are delivered, so the tick that follows a
/// landing can ask from the window the landing replaced. The store has just cleared the row's
/// in-flight ask, so without the revision the asker read it would accept this one and slide the
/// NEW window, with focus in the middle of it.
#[test]
fn an_ask_computed_from_a_revision_the_row_has_since_left_is_refused() {
    let len = 500;
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    let seen = hubs.revision;
    assert!(hubs.want_page(seen, ID, KEY, false));
    let ask = hubs.next_ask().unwrap();
    step(&mut hubs, &ask, listing);
    assert_ne!(seen, hubs.revision, "the landing published a new window");
    let window = keys(&hubs.committed[0]);
    assert!(!hubs.want_page(seen, ID, KEY, false), "the ask names a window the row no longer holds");
    assert!(hubs.next_ask().is_none());
    assert_eq!(keys(&hubs.committed[0]), window);
    assert!(hubs.want_page(hubs.revision, ID, KEY, false), "from the window that stands it is due again");
}

/// A page read that fails is asked for again by the store's own backoff, on either side, with no new
/// ask from the page: a reader held at the edge of a row presses into it and is not re-asking, so
/// the retry is the store's. (The ask used to be dropped on landing, failed or not, which left the
/// backoff with nothing to spawn: the row stayed short of its first card while the key was held.)
#[test]
fn a_failed_row_page_is_asked_for_again_without_a_new_ask_from_the_page() {
    let len = 500;
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    for _ in 0..4 {
        assert!(hubs.want_page(hubs.revision, ID, KEY, false));
        let ask = hubs.next_ask().unwrap();
        step(&mut hubs, &ask, listing);
    }
    let offset = hubs.committed[0].offset;
    assert!(offset > 0, "the window has moved on from the head");
    for before in [true, false] {
        let from = hubs.committed[0].offset;
        assert!(hubs.want_page(hubs.revision, ID, KEY, before));
        let ask = hubs.next_ask().unwrap();
        step(&mut hubs, &ask, |_, _| None);
        assert_eq!(hubs.committed[0].offset, from, "a failed read moves nothing");
        let mut frames = 0;
        while hubs.next_ask().is_none() && frames < 100_000 {
            hubs.tick();
            frames += 1;
        }
        let retry = hubs.next_ask().expect("the failed page is due again once its backoff has run");
        assert_eq!((retry.before, retry.reload), (before, false), "the same page, not another");
        step(&mut hubs, &retry, listing);
        assert_ne!(hubs.committed[0].offset, from, "and it lands");
        assert!(hubs.next_ask().is_none(), "a landed page is not asked for again");
    }
}

/// Reach survives the guard: when every landing meets a stale ask (the worst frame order), the
/// ask the missed publication re-arms comes from the window that stands, and the row still
/// reaches its last card.
#[test]
fn a_row_whose_every_landing_meets_a_stale_ask_still_reaches_its_last_card() {
    let len = 500;
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    let mut reached = std::collections::BTreeSet::new();
    for _ in 0..400 {
        reached.extend(keys(&hubs.committed[0]));
        let seen = hubs.revision;
        if !hubs.want_page(seen, ID, KEY, false) { break; }
        let ask = hubs.next_ask().unwrap();
        step(&mut hubs, &ask, listing);
        assert!(!hubs.want_page(seen, ID, KEY, false), "the stale ask of the landing's own frame");
    }
    assert_eq!(reached.len(), len);
    assert_eq!(hubs.committed[0].items.last().unwrap().rk, "499");
}

/// `HubHold` names rows `lo..=hi` of a catalog that a page ask does not reorder, so unlike a page
/// ask it is absolute: the last one said stands, and saying it again changes nothing. A hold read
/// from a view the frame's landing has since replaced is overwritten by the next tick's.
#[test]
fn a_hold_ask_is_absolute_so_a_stale_one_is_simply_overwritten() {
    let mut hubs = published(&hub_list(40));
    hubs.set_hold(20, 26);
    hubs.set_hold(0, 6);
    assert_eq!(hubs.held(), (0, 6), "the last ask stands whatever came before it");
    let revision = hubs.revision;
    assert!(!hubs.set_hold(0, 6), "saying it again changes nothing");
    assert_eq!(revision, hubs.revision);
}

// ---- the hub list read in windows of hubs ----------------------------------------------------

mod windows {
    use super::*;
    use crate::stores::paging::hub_list_tests::HubServer;
    use crate::stores::paging::HUB_WINDOW;

    /// The held rows and one window of previews: what a read may hold at once, however long the list.
    const PEAK_MAX: usize = (HOLD_ROWS + HUB_WINDOW) * 12;

    fn read(server: &mut HubServer, hold: (usize, usize)) -> Vec<Shelf> {
        PEAK_CARDS.with(|peak| peak.set(0));
        parse_hub_list(sid(), 1, hold, |req| server.answer(req)).expect("the list reads").shelves
    }

    fn peak() -> usize { PEAK_CARDS.with(|peak| peak.get()) }

    fn cards_of(shelves: &[Shelf]) -> usize { shelves.iter().map(|shelf| shelf.items.len()).sum() }

    #[test]
    fn a_400_row_section_is_read_in_windows_and_holds_only_the_ring_and_one_window() {
        let mut server = HubServer::new(400);
        let shelves = read(&mut server, (0, HOLD_ROWS - 1));
        assert!(server.widest <= HUB_WINDOW, "no response carries more than one window of hubs");
        assert_eq!(server.requests, 400usize.div_ceil(HUB_WINDOW));
        assert!(peak() <= PEAK_MAX, "peak held cards {} stay within {PEAK_MAX}", peak());
        assert_eq!(shelves.len(), 400);
        assert_eq!(cards_of(&shelves), HOLD_ROWS * 12);
    }

    #[test]
    fn every_row_is_reachable_and_a_released_row_keeps_what_it_needs_to_come_back() {
        let mut server = HubServer::new(400);
        let shelves = read(&mut server, (0, HOLD_ROWS - 1));
        for (row, shelf) in shelves.iter().enumerate() {
            assert_eq!(shelf.id, format!("movie.row{row}"));
            assert_eq!(shelf.key, format!("/hubs/sections/1/row{row}"));
            assert_eq!(shelf.released(), row >= HOLD_ROWS, "row {row}");
        }
        let last = &shelves[399];
        assert_eq!((last.shown, last.total, last.more), (12, 500, true), "a descriptor keeps what it showed and its listing");
    }

    #[test]
    fn a_refresh_while_the_user_is_on_row_200_keeps_that_rows_slot_window_and_cards() {
        let mut server = HubServer::new(400);
        let mut hubs = SecHubs { armed: true, ..Default::default() };
        hubs.land_ok(read(&mut server, (192, 208)));
        hubs.set_hold(192, 208);
        assert!(hubs.commit_staged(true));
        // the user has paged row 200 forward
        Arc::make_mut(&mut hubs.committed)[200].offset = 24;
        Arc::make_mut(&mut hubs.committed)[200].end = 48;
        hubs.land_ok(read(&mut server, (192, 208)));
        assert!(hubs.commit_staged(true));
        assert_eq!(hubs.committed[200].id, "movie.row200");
        assert_eq!((hubs.committed[200].offset, hubs.committed[200].end), (24, 48), "the window stays where it was");
        assert!(!hubs.committed[200].released());
        assert!(hubs.committed[0].released());
        assert!(held_cards(&hubs) <= HOLD_ROWS * MAX_SHELF_ITEMS);
    }

    #[test]
    fn a_server_that_ignores_paging_still_yields_every_row() {
        let mut server = HubServer::new(400);
        server.honours = false;
        server.reports = false;
        let shelves = read(&mut server, (0, HOLD_ROWS - 1));
        assert_eq!((server.requests, shelves.len()), (1, 400));
        assert_eq!(cards_of(&shelves), HOLD_ROWS * 12);
        assert!(peak() <= PEAK_MAX);
    }

    #[test]
    fn a_list_that_changes_between_windows_ends_complete_with_no_row_twice() {
        let mut server = HubServer::new(100);
        let mut calls = 0;
        let shelves = parse_hub_list(sid(), 1, (0, HOLD_ROWS - 1), |req| {
            calls += 1;
            if calls == 3 { server.rows.insert(5, 900); }
            server.answer(req)
        }).unwrap().shelves;
        let ids: Vec<String> = shelves.iter().map(|shelf| shelf.id.clone()).collect();
        let want: Vec<String> = server.rows.iter().map(|r| format!("movie.row{r}")).collect();
        assert_eq!(ids, want);
        assert_eq!(cards_of(&shelves), HOLD_ROWS * 12, "and the restart left no cards from the first read");
    }

    #[test]
    fn a_short_section_list_is_one_request() {
        let mut server = HubServer::new(7);
        let shelves = read(&mut server, (0, HOLD_ROWS - 1));
        assert_eq!((server.requests, shelves.len()), (1, 7));
    }
}

/// Focus left the edge a page was asked from while the read was out: the page withdraws the ask, and
/// the read that lands afterwards must not slide the window from under it. An ask not yet claimed
/// is simply dropped, and asking again afterwards works.
#[test]
fn a_withdrawn_ask_is_discarded_when_its_read_lands() {
    let len = 500;
    let listing = move |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut hubs = published(&one_row(&(0..12).collect::<Vec<_>>(), len));
    let window = keys(&hubs.committed[0]);
    let revision = hubs.revision;
    // not yet claimed
    assert!(hubs.want_page(revision, ID, KEY, false));
    hubs.cancel_page(ID, KEY);
    assert!(hubs.next_ask().is_none(), "an unclaimed ask is dropped");
    // claimed by a worker, then withdrawn
    assert!(hubs.want_page(revision, ID, KEY, false));
    let ask = hubs.next_ask().unwrap();
    hubs.inflight = Some((ask.clone(), false));
    hubs.cancel_page(ID, KEY);
    assert!(!step(&mut hubs, &ask, listing), "the landing changes nothing");
    assert_eq!(keys(&hubs.committed[0]), window);
    assert_eq!(hubs.revision, revision);
    // and the row can be asked again, and that one lands
    assert!(hubs.want_page(revision, ID, KEY, false));
    let ask = hubs.next_ask().unwrap();
    hubs.inflight = Some((ask.clone(), false));
    assert!(step(&mut hubs, &ask, listing));
    assert_ne!(keys(&hubs.committed[0]), window);
}
