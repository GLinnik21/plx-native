//! Home's refresh reads the hub list in windows of hubs: no response carries more than a window,
//! the cards held while it is read stay within the held rows and one window, and every row is
//! still there to scroll to.

use super::*;
use crate::stores::paging::hub_list_tests::HubServer;
use crate::stores::paging::HUB_WINDOW;

const CARDS: usize = 12;
/// The held rows and one window of previews: what a refresh may hold at once, however long the list.
const PEAK_MAX: usize = (HOLD_ROWS + HUB_WINDOW) * CARDS;

fn sid() -> ServerId { ServerId::from_raw(0) }

fn project_from(server: &mut HubServer, held: &[(String, String)]) -> SourceBuild {
    PEAK_CARDS.with(|peak| peak.set(0));
    project_hubs(sid(), held, |req| server.answer(req)).expect("the list reads")
}

fn id_of(row: usize) -> String { format!("movie.row{row}") }
fn key_of(row: usize) -> String { format!("/hubs/sections/1/row{row}") }
fn held_rows(rows: std::ops::Range<usize>) -> Vec<(String, String)> { rows.map(|r| (id_of(r), key_of(r))).collect() }
fn cards(build: &SourceBuild) -> usize { build.shelves.iter().map(|shelf| shelf.items.len()).sum() }

#[test]
fn a_400_row_refresh_reads_windows_and_holds_no_more_than_the_held_rows_and_one_window() {
    let mut server = HubServer::new(400);
    let build = project_from(&mut server, &[]);
    assert!(server.widest <= HUB_WINDOW, "no response carries more than one window of hubs");
    assert_eq!(server.requests, 400usize.div_ceil(HUB_WINDOW));
    let peak = PEAK_CARDS.with(|peak| peak.get());
    assert!(peak <= PEAK_MAX, "peak held cards {peak} stay within {PEAK_MAX}");
    assert_eq!(build.shelves.len(), 400);
    assert_eq!(cards(&build), HOLD_ROWS * CARDS, "a cold load keeps the first rows' cards");
}

#[test]
fn every_row_is_still_reachable_and_comes_back_with_its_cards() {
    let mut server = HubServer::new(400);
    let build = project_from(&mut server, &[]);
    for (row, shelf) in build.shelves.iter().enumerate() {
        assert_eq!((shelf.hub_id.as_str(), shelf.key.as_str()), (id_of(row).as_str(), key_of(row).as_str()), "list order");
        assert_eq!(shelf.title, format!("Row {row}"));
        if row >= HOLD_ROWS {
            assert!(shelf.released(), "row {row} is a descriptor");
            assert_eq!(shelf.shown, CARDS);
            assert!(shelf.more && shelf.total == 500, "and can still be paged");
        }
    }
}

#[test]
fn a_refresh_on_row_200_keeps_that_rows_slot_and_cards() {
    let mut server = HubServer::new(400);
    let held = held_rows(192..208);
    let build = project_from(&mut server, &held);
    assert!(PEAK_CARDS.with(|peak| peak.get()) <= PEAK_MAX);
    assert_eq!(build.shelves[200].hub_id, id_of(200), "the row keeps its slot");
    assert_eq!(build.shelves[200].items.len(), CARDS, "and its cards");
    for row in 192..208 { assert!(!build.shelves[row].released(), "row {row} is held"); }
    assert!(build.shelves[0].released(), "the head is a descriptor");
    assert_eq!(cards(&build), HOLD_ROWS * CARDS, "only the held rows keep cards");
}

#[test]
fn a_refresh_carries_the_descriptors_it_had() {
    let mut server = HubServer::new(400);
    let held = held_rows(192..208);
    let mut old = project_from(&mut server, &held);
    old.shelves[5].offset = 36;
    old.shelves[5].end = 48;
    old.shelves[5].shown = CARDS;
    let mut fresh = project_from(&mut server, &held);
    carry_descriptors(&old, &mut fresh);
    assert_eq!(fresh.shelves[5].offset, 36, "a row that gave its cards up keeps the window it left");
    assert_eq!(fresh.shelves[200].items.len(), CARDS);
    assert_eq!(fresh.shelves.len(), 400);
}

#[test]
fn a_server_that_ignores_paging_still_yields_every_row_and_drops_cards_as_it_parses() {
    let mut server = HubServer::new(400);
    server.honours = false;
    server.reports = false;
    let build = project_from(&mut server, &[]);
    assert_eq!(server.requests, 1);
    assert_eq!(build.shelves.len(), 400);
    assert_eq!(cards(&build), HOLD_ROWS * CARDS);
    assert!(PEAK_CARDS.with(|peak| peak.get()) <= PEAK_MAX);
}

#[test]
fn a_list_that_changes_between_windows_ends_complete() {
    let mut server = HubServer::new(100);
    let mut calls = 0;
    PEAK_CARDS.with(|peak| peak.set(0));
    let build = project_hubs(sid(), &[], |req| {
        calls += 1;
        if calls == 3 { server.rows.remove(0); }
        server.answer(req)
    }).unwrap();
    let ids: Vec<&str> = build.shelves.iter().map(|shelf| shelf.hub_id.as_str()).collect();
    let want: Vec<String> = (1..100).map(id_of).collect();
    assert_eq!(ids, want.iter().map(String::as_str).collect::<Vec<_>>(), "the list as it ended, none twice, none missing");
}

#[test]
fn a_short_list_is_one_request_that_keeps_every_card() {
    let mut server = HubServer::new(HOLD_ROWS);
    let build = project_from(&mut server, &[]);
    assert_eq!(server.requests, 1);
    assert_eq!(cards(&build), HOLD_ROWS * CARDS);
}
