//! Every Home hub row on the sliding window: the ledger a listing that will not hold falls back to,
//! and (further down) the preview rule that puts a non-recent row on the listing behind its key.

use super::*;
use super::test_support::*;

type Container = plx_plex::plex::MediaContainer;

fn item(key: usize) -> serde_json::Value {
    serde_json::json!({"ratingKey": key.to_string(), "type": "movie", "title": "Movie", "thumb": "/poster"})
}

fn container(offset: usize, total: usize, keys: impl Iterator<Item = usize>) -> Container {
    serde_json::from_value(serde_json::json!({"offset": offset, "totalSize": total,
        "Metadata": keys.map(item).collect::<Vec<_>>()})).unwrap()
}

/// A listing of `len` rows that is a different permutation on every request, so no position holds.
fn reshuffling(len: usize) -> impl FnMut(usize, usize) -> Option<Container> {
    let mut request = 0;
    move |start, size| {
        request += 1;
        let key = |p: usize| (p * 7 + request * 13) % len;
        Some(container(start, len, (start..(start + size).min(len)).map(key)))
    }
}

/// The batch read the ledger re-materialises through: one row per key asked for, in request order.
fn batch(known: impl Fn(&str) -> bool + 'static) -> impl FnMut(&[String]) -> Option<Container> {
    move |keys| {
        let rows = keys.iter().filter(|key| known(key)).map(|key| item(key.parse().unwrap())).collect::<Vec<_>>();
        Some(serde_json::from_value(serde_json::json!({"Metadata": rows})).unwrap())
    }
}


fn rows_of(keys: impl Iterator<Item = usize>) -> Vec<paging::Row> {
    keys.enumerate().map(|(position, key)| (position, row(0, &key.to_string()))).collect()
}

fn keys_of(rows: &[paging::Row]) -> Vec<String> {
    rows.iter().map(|(_, item)| item.rk.clone()).collect()
}

fn ask(before: bool) -> paging::Ask<'static> {
    paging::Ask { start: 0, before, hidden: &[] }
}

fn active(rows: &[paging::Row], next: usize) -> paging::Ledger {
    paging::Ledger { active: true, ..paging::Ledger::from_rows(rows, next) }
}

fn info_of(rows: &[paging::Row], total: usize) -> paging::PageInfo {
    paging::PageInfo { offset: 0, end: rows.len(), total, more: true, unstable: false }
}

#[test]
fn ledger_keys_are_integers_unless_they_are_not() {
    assert_eq!(paging::LedgerKey::new("4096"), paging::LedgerKey::Num(4096));
    assert_eq!(paging::LedgerKey::new("4096").text(), "4096");
    assert_eq!(paging::LedgerKey::new("007").text(), "007", "a key that is not canonical stays text");
    assert_eq!(paging::LedgerKey::new("abc-1").text(), "abc-1");
}

#[test]
fn a_listing_that_reshuffles_on_every_request_shows_every_item_exactly_once() {
    let _guard = plx_base::testlock::serial();
    let len = 100;
    let mut listing = reshuffling(len);
    let first = listing(0, 24).unwrap();
    let mut rows: Vec<paging::Row> = first.metadata.iter().enumerate()
        .map(|(position, m)| (position, std::sync::Arc::new(parse_item(m, sid(0))))).collect();
    let mut ledger = active(&rows, 24);
    let mut info = info_of(&rows, len);
    let mut seen: std::collections::HashSet<String> = keys_of(&rows).into_iter().collect();
    assert_eq!(seen.len(), 24);
    let mut previous: std::collections::HashSet<String> = seen.clone();
    for _ in 0..200 {
        if !info.more { break; }
        let (next, next_info) = paging::ledger_window(sid(0), &ask(false), (&rows, info), &mut ledger,
            &mut listing, batch(|_| true)).unwrap();
        assert!(next.len() <= 24, "the row never holds more than its window");
        for key in keys_of(&next) {
            if !previous.contains(&key) { assert!(seen.insert(key.clone()), "{key} was shown a second time"); }
        }
        previous = keys_of(&next).into_iter().collect();
        rows = next;
        info = next_info;
    }
    assert!(!info.more, "the walk ends");
    assert_eq!(seen.len(), len, "every item was reached");
}

#[test]
fn moving_back_from_the_ledger_returns_the_same_cards_in_the_same_order() {
    let _guard = plx_base::testlock::serial();
    let len = 100;
    let listing = |start: usize, size: usize| Some(container(start, len, start..(start + size).min(len)));
    let mut rows = rows_of(0..24);
    let mut ledger = active(&rows, 24);
    let mut info = info_of(&rows, len);
    let mut forward = vec![keys_of(&rows)];
    while info.more {
        let (next, next_info) = paging::ledger_window(sid(0), &ask(false), (&rows, info), &mut ledger,
            listing, batch(|_| true)).unwrap();
        forward.push(keys_of(&next));
        rows = next;
        info = next_info;
    }
    assert_eq!(forward.last().unwrap().last().unwrap(), "99");
    let mut batches = 0;
    let mut counting = batch(|_| true);
    for expected in forward.iter().rev().skip(1) {
        let (previous, previous_info) = paging::ledger_window(sid(0), &ask(true), (&rows, info), &mut ledger,
            |_, _| panic!("moving back reads the ledger, not the listing"),
            |keys| { batches += 1; counting(keys) }).unwrap();
        assert_eq!(&keys_of(&previous), expected);
        assert_eq!(previous.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            (previous_info.offset..previous_info.end).collect::<Vec<_>>(), "positions are ledger indices");
        rows = previous;
        info = previous_info;
    }
    assert!(batches > 0);
    assert_eq!(info.offset, 0);
}

#[test]
fn a_server_that_ignores_paging_is_read_once_and_only_the_window_is_kept() {
    let _guard = plx_base::testlock::serial();
    let len = 60;
    let mut reads = 0;
    // Every read answers with the whole listing from its first row, whatever was asked.
    let mut ignoring = |_: usize, _: usize| { reads += 1; Some(container(0, len, 0..len)) };
    let mut rows = rows_of(0..24);
    let mut ledger = active(&rows, 24);
    let mut info = info_of(&rows, len);
    let mut shown = std::collections::BTreeSet::new();
    shown.extend(keys_of(&rows));
    while info.more {
        let (next, next_info) = paging::ledger_window(sid(0), &ask(false), (&rows, info), &mut ledger,
            &mut ignoring, batch(|_| true)).unwrap();
        assert!(next.len() <= 24);
        shown.extend(keys_of(&next));
        rows = next;
        info = next_info;
    }
    assert_eq!(shown.len(), len);
    assert_eq!(reads, 1, "its keys came from that one response");
    assert_eq!(ledger.keys.len(), len);
}

#[test]
fn a_key_the_server_no_longer_returns_is_dropped_and_the_window_closes_over_it() {
    let _guard = plx_base::testlock::serial();
    let all = rows_of(0..48);
    let window: Vec<paging::Row> = all[24..48].iter().cloned().collect();
    let mut ledger = active(&all, 48);
    let info = paging::PageInfo { offset: 24, end: 48, total: 48, more: false, unstable: false };
    let (rows, _) = paging::ledger_window(sid(0), &ask(true), (&window, info), &mut ledger,
        |_, _| panic!("moving back reads the ledger"), batch(|key| key != "20")).unwrap();
    let expected: Vec<String> = [11usize].into_iter().chain(12..20).chain(21..36).map(|k| k.to_string()).collect();
    assert_eq!(keys_of(&rows), expected, "key 20 is gone and one more is read to fill the window");
    assert!(ledger.position("20").is_none());
    let focused = rows.iter().find(|(_, item)| item.rk == "30").unwrap();
    assert_eq!(Some(focused.0), ledger.position("30"), "the card the user is on keeps its identity and its index");
    assert_eq!(ledger.keys.len(), 47);
}

#[test]
fn noting_a_window_keeps_the_ledger_in_row_order() {
    let mut ledger = paging::Ledger::default();
    ledger.note(&rows_of(10..14));
    ledger.note(&rows_of(12..16));
    ledger.note(&rows_of(8..12));
    let keys: Vec<String> = ledger.keys.iter().map(paging::LedgerKey::text).collect();
    assert_eq!(keys, (8..16).map(|k| k.to_string()).collect::<Vec<_>>());
}

// ---- a row asked through the store -------------------------------------------------------------

const RECENT: &str = "/hubs/home/recentlyAdded?type=1";

/// One server whose Home holds a single row, `id` over `key`, showing keys 0..12 of a listing of `len`.
fn owner_with(id: &str, key: &str, len: usize) -> Owner {
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("rows", "127.0.0.1", 9, "synthetic", "fixture");
    let keys: Vec<String> = (0..12).map(|k| k.to_string()).collect();
    let mut shelf = shelf(0, "Row", id, &keys.iter().map(String::as_str).collect::<Vec<_>>());
    shelf.key = key.into();
    shelf.end = 12;
    shelf.total = len;
    shelf.more = true;
    let mut owner = Owner::default();
    seed(&mut owner.state, vec![src(sid.raw(), "", HubState::Ready, Some(built(sid.raw(), &[], vec![shelf])))]);
    owner
}

fn ask_row(owner: &mut Owner, id: &str, key: &str, before: bool) -> Option<HubRequest> {
    let mut held = None;
    let _ = request_page(&mut owner.state, &owner.adapter, sid(0), id, key, before, &BrowseScope::standalone(),
        &mut |request| { held = Some(request); true });
    held
}

/// Answers a page request from `list`/`many` the way `HubRequest::fetch` would from a server, then
/// lands it through the same encode/decode the recorder uses.
fn answer(owner: &mut Owner, request: HubRequest, list: impl FnMut(usize, usize) -> Option<Container>,
    many: impl FnMut(&[String]) -> Option<Container>) {
    let page = request.page.clone().unwrap();
    let build = fetch_row(request.sid, &page, request.window.as_ref(), 0, list, many);
    let landing = request.complete(build);
    let landing = record::decode(record::encode(&landing), |_| plx_plex::plex::client_for(sid(0))).unwrap();
    let _ = super::land(&mut owner.state, &owner.adapter, &landing);
}

fn shown(owner: &Owner) -> Vec<String> {
    rks(&owner.state, 0)
}

#[test]
fn a_recent_row_whose_listing_stops_holding_moves_to_its_ledger_and_still_reaches_everything() {
    let _guard = plx_base::testlock::serial();
    let len = 100;
    let mut owner = owner_with("home.movies.recent", RECENT, len);
    let mut requests = 0;
    // The first two reads see the listing as it was; after that every read is another order.
    let mut listing = {
        let mut reshuffled = reshuffling(len);
        move |start: usize, size: usize| {
            requests += 1;
            if requests <= 2 { Some(container(start, len, start..(start + size).min(len))) } else { reshuffled(start, size) }
        }
    };
    let mut ever: std::collections::BTreeSet<String> = shown(&owner).into_iter().collect();
    let mut previous: std::collections::HashSet<String> = ever.iter().cloned().collect();
    for _ in 0..200 {
        let Some(request) = ask_row(&mut owner, "home.movies.recent", RECENT, false) else { break };
        answer(&mut owner, request, &mut listing, batch(|_| true));
        let window = shown(&owner);
        assert!(window.len() <= 24);
        for key in &window {
            if !previous.contains(key) { assert!(ever.insert(key.clone()), "{key} appeared a second time"); }
        }
        previous = window.into_iter().collect();
    }
    assert_eq!(ever.len(), len, "an unstable listing no longer stops the row");
    assert!(ask_row(&mut owner, "home.movies.recent", RECENT, false).is_none());
    reset(&mut owner.state, &owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}
