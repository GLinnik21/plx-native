//! Home's row ring: every row keeps a descriptor, cards are held for the rows the screen holds,
//! and a row coming back is read again where its window began.

use super::*;
use super::test_support::*;

const ROWS: usize = 400;
const CARDS: usize = 12;
/// 16 held rows of 24 cards: the bound the ring states.
const HELD_CARDS_MAX: usize = HOLD_ROWS * MAX_SHELF_ITEMS;

fn pageable_shelf(slot: u16, row: usize) -> Shelf {
    let keys: Vec<String> = (0..CARDS).map(|c| format!("{row}-{c}")).collect();
    let mut sh = shelf(slot, &format!("Row {row}"), &format!("x.{row}"), &keys.iter().map(String::as_str).collect::<Vec<_>>());
    sh.key = format!("/hubs/sections/1/{row}");
    sh.end = CARDS;
    sh.more = true;
    sh
}

/// A row whose key the pager does not read: a playlist's items.
fn opaque_shelf(slot: u16, row: usize) -> Shelf {
    let mut sh = pageable_shelf(slot, row);
    sh.key = format!("/playlists/{row}/items");
    sh.end = 0;
    sh.more = false;
    sh
}

/// A pageable row that has not been compared with its listing: its preview may be a random sample.
fn unprobed_shelf(slot: u16, row: usize) -> Shelf {
    let mut sh = pageable_shelf(slot, row);
    sh.row.mode = paging::RowMode::Unprobed;
    sh
}

/// A Home of `rows` pageable rows from one registered server, with the ring applied as a landing
/// would apply it.
fn ring_home(rows: usize, make: fn(u16, usize) -> Shelf) -> (Owner, ServerId) {
    plx_plex::plex::reset_servers_for_test();
    let id = plx_plex::plex::register_for_test("ring", "127.0.0.1", 9, "synthetic", "cid");
    let shelves = (0..rows).map(|r| make(id.raw(), r)).collect();
    let mut o = Owner::default();
    let mut source = src(id.raw(), "", HubState::Ready, Some(built(id.raw(), &[], shelves)));
    source.key = plx_plex::plex::client_for(id).map(plx_plex::plex::ClientKey::of);
    o.state.srcs = vec![source];
    let scope = BrowseScope::standalone();
    let mut srcs = std::mem::take(&mut o.state.srcs);
    let (build, _) = merge_held(&mut srcs, &scope, o.state.hold);
    o.state.srcs = srcs;
    remember_roster(&mut o.state, &scope);
    o.state.seen_facts = facts_key();
    commit(&mut o.state, build);
    (o, id)
}

/// The cards the catalog holds for real: placeholders have no rating key.
fn held_cards(state: &PmsState) -> usize {
    catalog(state).iter().filter(|m| !m.rk.is_empty()).count()
}

fn row_has_cards(state: &PmsState, row: usize) -> bool {
    let h = &hubs(state)[row];
    h.len > 0 && (h.start..h.start + h.len).all(|i| !catalog(state)[i].rk.is_empty())
}

/// What the server answers a row's reload with: `CARDS` cards from the offset asked for.
fn serve(request: HubRequest) -> Landing {
    let page = request.page.clone().expect("a reload is a page request");
    assert!(page.reload, "only reloads are asked in these tests");
    let row: usize = page.id.trim_start_matches("x.").parse().unwrap();
    if let Some(window) = request.window.as_ref().filter(|window| !window.row.kept.is_empty()) {
        // the batch read: every card asked for, in the reverse of the order asked
        let mut many = |keys: &[String]| {
            let items: Vec<serde_json::Value> = keys.iter().rev().map(|key| serde_json::json!({
                "ratingKey": key, "type": "movie", "title": "Movie", "thumb": "/poster"})).collect();
            Some(serde_json::from_value(serde_json::json!({"Metadata": items})).unwrap())
        };
        let build = fetch_kept(request.sid, &page, window, &mut many);
        return request.complete_for_test(build);
    }
    // a listing that is a different sample every time it is read
    let keys: Vec<String> = (page.start..page.start + CARDS).map(|c| format!("fresh-{row}-{c}")).collect();
    let mut answer = shelf(request.sid.raw(), "", &page.id, &keys.iter().map(String::as_str).collect::<Vec<_>>());
    answer.key = page.key.clone();
    answer.positions = (page.start..page.start + CARDS).collect();
    answer.offset = page.start;
    answer.end = page.start + CARDS;
    answer.more = true;
    answer.total = 1000;
    let build = SourceBuild { shelves: vec![answer], ..Default::default() };
    request.complete_for_test(Some(build))
}

struct Ring { o: Owner, launched: Vec<(String, usize, usize)>, pending: Vec<Landing> }

impl Ring {
    fn new(rows: usize) -> Self { Self::of(rows, pageable_shelf) }
    fn of(rows: usize, make: fn(u16, usize) -> Shelf) -> Self {
        let (o, _) = ring_home(rows, make);
        Self { o, launched: Vec::new(), pending: Vec::new() }
    }
    fn hold(&mut self, lo: usize, hi: usize) {
        let scope = BrowseScope::standalone();
        let (pending, launched) = (&mut self.pending, &mut self.launched);
        let mut launch = |request: HubRequest| {
            let page = request.page.clone().unwrap();
            launched.push((page.id.clone(), page.start, request.window.as_ref().map_or(0, |w| w.end)));
            pending.push(serve(request));
        };
        let _ = run_hold(&mut self.o.state, &self.o.adapter, &scope, lo, hi, &mut launch);
    }
    /// Lands what the "server" has answered so far (one tick), launching what that frees.
    fn land(&mut self) -> usize {
        let scope = BrowseScope::standalone();
        let landed = std::mem::take(&mut self.pending);
        let n = landed.len();
        let (pending, launched) = (&mut self.pending, &mut self.launched);
        let mut launch = |request: HubRequest| {
            let page = request.page.clone().unwrap();
            launched.push((page.id.clone(), page.start, request.window.as_ref().map_or(0, |w| w.end)));
            pending.push(serve(request));
        };
        let _ = step_landings_with_scope(&mut self.o.state, &self.o.adapter, Some(0.0), move || landed, &scope, &mut launch);
        n
    }
    fn settle(&mut self) { while self.land() > 0 {} }
}

#[test]
fn a_400_row_home_holds_few_cards_at_every_step_and_leaves_no_row_off() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::new(ROWS);
    assert_eq!(hub_count(&ring.o.state), ROWS, "no row is left off");
    assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX, "the first publication holds the ring only");
    let walk: Vec<usize> = (0..ROWS).chain((0..ROWS).rev()).collect();
    for &row in &walk {
        ring.hold(row.saturating_sub(2), (row + 2).min(ROWS - 1));
        assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX, "row {row}: {} cards held", held_cards(&ring.o.state));
        ring.settle();
        assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX, "row {row} after landings: {} cards held", held_cards(&ring.o.state));
        assert_eq!(hub_count(&ring.o.state), ROWS);
        assert!(row_has_cards(&ring.o.state, row), "row {row} has its cards once the reads landed");
    }
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_row_that_left_the_ring_returns_at_its_window_offset() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::new(ROWS);
    {
        let shelf = &mut ring.o.state.srcs[0].last.as_mut().unwrap().shelves[5];
        shelf.offset = 12;
        shelf.end = 24;
    }
    ring.hold(100, 110);
    ring.settle();
    let hub = hubs_snapshot(&ring.o.state);
    assert!(hub.view().hub(5).unwrap().items.iter().all(|m| m.rk.is_empty()), "row 5 gave its cards up");
    assert_eq!(hub.view().hub(5).unwrap().offset, 12, "and kept its window offset");
    ring.launched.clear();
    ring.hold(3, 8);
    ring.settle();
    let asked = ring.launched.iter().find(|(id, _, _)| id == "x.5").expect("row 5 was read again");
    assert_eq!((asked.1, asked.2), (12, 12), "from the offset its window began at");
    let hub = hubs_snapshot(&ring.o.state);
    let row = hub.view().hub(5).unwrap();
    assert_eq!(row.offset, 12);
    assert_eq!(row.items[0].rk, "fresh-5-12", "the first card is the one at the window offset");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn reads_are_bounded_and_none_is_asked_for_a_row_no_longer_held() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::new(ROWS);
    // held Down through every row without a single landing in between
    for row in 0..ROWS { ring.hold(row.saturating_sub(2), (row + 2).min(ROWS - 1)); }
    assert_eq!(ring.launched.len(), 1, "one source, one read out: not {ROWS} queued");
    ring.settle();
    let asked: Vec<usize> = ring.launched.iter().map(|(id, _, _)| id.trim_start_matches("x.").parse().unwrap()).collect();
    assert!(asked.iter().all(|&r| r < 5 || r >= ROWS - 5), "rows the key passed were never asked for: {asked:?}");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn hold_that_does_not_move_does_nothing() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::new(40);
    ring.hold(10, 14);
    let gen = ring.o.state.catalog_gen;
    let asked = ring.launched.len();
    ring.hold(10, 14);
    assert_eq!((ring.o.state.catalog_gen, ring.launched.len()), (gen, asked));
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_queued_read_for_a_row_that_left_the_ring_is_dropped_before_it_starts() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::new(ROWS);
    // the fan-out gate had no room for row 5's read, so it waits in the queue, deferred
    let source = &mut ring.o.state.srcs[0];
    source.page = Some(PageQuery { id: "x.5".into(), key: "/hubs/sections/1/5".into(), start: 0, before: false, hidden: Vec::new(), reload: true });
    source.deferred = true;
    ring.hold(100, 104);
    ring.settle();
    let asked: Vec<&str> = ring.launched.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(!asked.contains(&"x.5"), "row 5 left the ring before its read started: {asked:?}");
    assert!(asked.iter().any(|id| *id == "x.100"), "the rows the key stopped on were read: {asked:?}");
    plx_plex::plex::reset_servers_for_test();
}

/// The cards of a row, by rating key, as the catalog holds them.
fn card_keys(state: &PmsState, row: usize) -> Vec<String> {
    let h = &hubs(state)[row];
    (h.start..h.start + h.len).map(|i| catalog(state)[i].rk.clone()).collect()
}

fn the_cards_every_row_showed(rows: usize) -> impl Fn(usize) -> Vec<String> {
    move |row| { assert!(row < rows); (0..CARDS).map(|c| format!("{row}-{c}")).collect() }
}

#[test]
fn rows_with_no_pageable_key_are_released_and_return_with_the_same_cards_in_order() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::of(ROWS, opaque_shelf);
    assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX, "{} cards held by the first publication", held_cards(&ring.o.state));
    let showed = the_cards_every_row_showed(ROWS);
    let walk: Vec<usize> = (0..ROWS).chain((0..ROWS).rev()).collect();
    for &row in &walk {
        ring.hold(row.saturating_sub(2), (row + 2).min(ROWS - 1));
        assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX, "row {row}: {} cards held", held_cards(&ring.o.state));
        ring.settle();
        assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX, "row {row} after landings: {} cards held", held_cards(&ring.o.state));
        assert_eq!(card_keys(&ring.o.state, row), showed(row), "row {row} came back with the cards it showed, in order");
    }
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_released_random_row_returns_with_the_cards_the_user_saw_not_a_fresh_sample() {
    let _g = plx_base::testlock::serial();
    let mut ring = Ring::of(ROWS, unprobed_shelf);
    let showed = the_cards_every_row_showed(ROWS);
    ring.hold(100, 104);
    ring.settle();
    assert!(held_cards(&ring.o.state) <= HELD_CARDS_MAX);
    ring.hold(1, 5);
    ring.settle();
    for row in 1..=5 {
        assert_eq!(card_keys(&ring.o.state, row), showed(row), "row {row}");
    }
    plx_plex::plex::reset_servers_for_test();
}

/// A pageable row that has walked a long way: its ledger holds `WALKED` keys, and every fifth row
/// has moved onto it because its listing could not be addressed by offset.
const WALKED: usize = 200;

fn walked_shelf(slot: u16, row: usize) -> Shelf {
    let mut sh = pageable_shelf(slot, row);
    sh.row.mode = paging::RowMode::Head;
    sh.row.ledger = Some(paging::Ledger {
        keys: (0..WALKED).map(|k| paging::LedgerKey::new(&(row * 1000 + k).to_string())).collect(),
        active: row % 5 == 0,
        ..Default::default()
    });
    sh
}

#[test]
fn a_released_row_keeps_a_ledger_only_when_its_window_cannot_be_read_without_it() {
    let _guard = plx_base::testlock::serial();
    let (o, _) = ring_home(ROWS, walked_shelf);
    let build = o.state.srcs[0].last.as_ref().unwrap();
    let (mut released, mut kept) = (0, 0);
    for (row, shelf) in build.shelves.iter().enumerate().filter(|(_, shelf)| shelf.released()) {
        released += 1;
        match &shelf.row.ledger {
            Some(ledger) => {
                assert!(ledger.active, "row {row}: an idle ledger was kept on a released row");
                assert_eq!(ledger.keys.len(), WALKED, "an active ledger is kept whole: positions index into it");
                kept += 1;
            }
            None => assert_ne!(row % 5, 0, "row {row}: the ledger its window is read through was dropped"),
        }
    }
    assert!(released > ROWS / 2, "most rows are outside the held range");
    assert_eq!(kept, (0..ROWS).filter(|row| row % 5 == 0 && build.shelves[*row].released()).count());
    plx_plex::plex::reset_servers_for_test();
}

/// A Hold moves the band and the rows that left it give their cards up. The catalog is PATCHED for
/// them (their slices become placeholders, the shape is the same), not built a second time; the
/// result must be the catalog a second merge of the settled ring produces, hero pool included - a
/// hero in a row that left the band is kept as a card the catalog still holds.
#[test]
fn merge_held_equals_a_merge_of_the_settled_ring() {
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let id = plx_plex::plex::register_for_test("ring", "127.0.0.1", 9, "synthetic", "cid");
    let hero_rows = [2, 60, 61, 300];
    let shelves = (0..ROWS).map(|row| {
        let mut sh = pageable_shelf(id.raw(), row);
        if hero_rows.contains(&row) {
            sh.hub_id = format!("x.recent.{row}");
            for item in &mut sh.items { Arc::make_mut(item).art = format!("/art/{row}"); }
        }
        sh
    }).collect();
    let mut srcs = vec![src(id.raw(), "", HubState::Ready, Some(built(id.raw(), &[], shelves)))];
    let scope = BrowseScope::standalone();
    let full = merge_with_scope(&srcs, &scope);
    assert!(full.2.len() >= 3, "the fixture pools heroes from several rows");
    let (patched, changed) = merge_held(&mut srcs, &scope, (0, 15));
    assert!(changed, "most rows are outside the band");
    // the old way: the same sources, merged again once the ring has settled
    let mut rebuilt = merge_with_scope(&srcs, &scope);
    keep_heroes(&full.0, &full.2, &mut rebuilt);
    let keys = |build: &HubBuild| build.0.iter().map(|m| (m.sid, m.rk.clone(), m.art.clone())).collect::<Vec<_>>();
    assert_eq!(keys(&patched), keys(&rebuilt), "the patched catalog holds the cards a second merge holds");
    let rows = |build: &HubBuild| build.1.iter().map(|h| (h.title.clone(), h.hub_id.clone(), h.key.clone(),
        h.source.clone(), h.total, h.start, h.len, h.offset, h.more, h.epoch)).collect::<Vec<_>>();
    assert_eq!(rows(&patched), rows(&rebuilt));
    let pool = |build: &HubBuild| build.2.iter().map(|h| (build.0[h.idx].rk.clone(), h.source.clone())).collect::<Vec<_>>();
    assert_eq!(pool(&patched), pool(&rebuilt));
    assert!(patched.2.iter().all(|h| !patched.0[h.idx].rk.is_empty()), "no hero is a placeholder");
    assert!(patched.1.iter().skip(16).all(|h| patched.0[h.start..h.start + h.len].iter().all(|m| m.rk.is_empty())),
        "the rows outside the band are placeholders");
    plx_plex::plex::reset_servers_for_test();
}

/// The same equivalence over what the one-source fixture above leaves out: two sources (the pool
/// spans them and the placeholder slot is per source), the placed Continue Watching deck (it leads
/// the catalog, so the held band counts it), a pin filter that withholds a row, and rows that
/// cannot give their cards up (a card with no rating key has no way back) and so stay whole outside
/// the band. For several bands, the later ones over rows that earlier ones released.
#[test]
fn merge_held_equals_a_merge_of_the_settled_ring_across_sources_the_deck_pins_and_rows_that_keep_their_cards() {
    let _guard = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let a = plx_plex::plex::register_for_test("ring-a", "127.0.0.1", 9, "synthetic", "cid-a");
    let b = plx_plex::plex::register_for_test("ring-b", "127.0.0.2", 9, "synthetic", "cid-b");
    let (withheld_row, whole_row, hero_rows) = (7, 30, [2, 45, 50]);
    let shelves = |id: ServerId| -> Vec<Shelf> {
        (0..90).map(|row| {
            let mut sh = pageable_shelf(id.raw(), row);
            if hero_rows.contains(&row) {
                sh.hub_id = format!("x.recent.{row}");
                for item in &mut sh.items { Arc::make_mut(item).art = format!("/art/{}/{row}", id.raw()); }
            }
            if row == withheld_row {
                // every card of the row belongs to a library the pins turn off
                for item in &mut sh.items { Arc::make_mut(item).sec = 5; }
            }
            if row == whole_row {
                // a row that cannot be read again: its key is no pager's and one card has no rating key
                sh = opaque_shelf(id.raw(), row);
                Arc::make_mut(&mut sh.items[3]).rk.clear();
            }
            sh
        }).collect()
    };
    let deck: Vec<(i64, String)> = (0..8).map(|i| (1000 - i, format!("cw{i}"))).collect();
    let deck: Vec<(i64, &str)> = deck.iter().map(|(at, rk)| (*at, rk.as_str())).collect();
    let mut srcs = vec![
        src(a.raw(), "", HubState::Ready, Some(built(a.raw(), &deck, shelves(a)))),
        src(b.raw(), "", HubState::Ready, Some(built(b.raw(), &deck[..3], shelves(b)))),
    ];
    settle_deck(&mut srcs, true);
    let scope = BrowseScope { sections_gen: 1, pins: vec![(a, 5, false)], clients: Default::default() };
    let keys = |build: &HubBuild| build.0.iter().map(|m| (m.sid, m.rk.clone(), m.art.clone())).collect::<Vec<_>>();
    let rows = |build: &HubBuild| build.1.iter().map(|h| (h.title.clone(), h.hub_id.clone(), h.key.clone(),
        h.source.clone(), h.total, h.start, h.len, h.offset, h.more, h.epoch)).collect::<Vec<_>>();
    let pool = |build: &HubBuild| build.2.iter().map(|h| (build.0[h.idx].sid, build.0[h.idx].rk.clone(), h.source.clone())).collect::<Vec<_>>();
    let full = merge_with_scope(&srcs, &scope);
    assert!(full.2.len() >= 3, "the fixture pools heroes from several rows");
    assert!(full.1.iter().any(|h| h.hub_id == "home.continue"), "premise: the deck leads the catalog");
    let rows_of = |hub_key: String| full.1.iter().filter(|h| h.key == hub_key).count();
    assert_eq!(rows_of(format!("/hubs/sections/1/{withheld_row}")), 1, "premise: the pins withhold the row of source a only");
    // the first band, then bands that move on, and back over rows released before
    let mut patches = Vec::new();
    for band in [(0, 15), (20, 35), (60, 75), (0, 15), (40, 55)] {
        let (patched, changed) = merge_held(&mut srcs, &scope, band);
        patches.push(changed);
        let mut rebuilt = merge_with_scope(&srcs, &scope);
        keep_heroes(&full.0, &full.2, &mut rebuilt);
        assert_eq!(keys(&patched), keys(&rebuilt), "band {band:?}: the patched catalog holds the cards a second merge holds");
        assert_eq!(rows(&patched), rows(&rebuilt), "band {band:?}");
        assert_eq!(pool(&patched), pool(&rebuilt), "band {band:?}");
        assert!(patched.2.iter().all(|h| !patched.0[h.idx].rk.is_empty()), "band {band:?}: no hero is a placeholder");
        // the row that cannot release keeps its cards wherever the band is, for both sources
        let whole: Vec<_> = patched.1.iter().filter(|h| h.key == format!("/playlists/{whole_row}/items")).collect();
        assert_eq!(whole.len(), 2, "band {band:?}: the row of each source is there");
        for h in whole {
            let cards = &patched.0[h.start..h.start + h.len];
            assert_eq!(cards.iter().filter(|m| m.rk.is_empty()).count(), 1, "band {band:?}: its cards are the row's own");
        }
    }
    // (no reads land here, so a row released by one band stays released: only the first two bands
    // hold cards to give up, and the later ones check that a patch with nothing to do is the merge too)
    assert_eq!(&patches[..2], &[true; 2], "the first bands release rows, so each is a patch and not a no-op: {patches:?}");
    plx_plex::plex::reset_servers_for_test();
}
