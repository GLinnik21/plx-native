//! A page read already in flight when a view-state edit is published lands carrying the pre-edit
//! body; the ledger re-patches it.

use super::test_support::*;
use super::*;

/// A Browse page read is out, the user marks an item on that page watched, and the read lands
/// with the row as the server described it BEFORE the press. The tile must stay watched.
#[test]
fn a_page_read_in_flight_across_a_watched_edit_lands_watched() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_page_source();
    let row = |rk: &str| {
        let mut m = PmsMovie::default();
        m.sid = sid;
        m.rk = rk.to_string();
        m.unwatched = true;
        m.resume_ms = 90_000;
        m
    };
    // The edit is published while the read is out: nothing is on screen to patch yet.
    let at = crate::viewstate::ViewAt::leaf(sid, "7");
    browse.state.ledger.record(&at, crate::viewstate::ViewEdit::Watched(true));
    assert!(!browse.state.set_watched_local(&at, true));
    let r = PageResult {
        client,
        token_gen: client.token_gen(),
        gen: browse.state.query_gen(),
        sec: 0,
        start: 0,
        items: vec![row("7"), row("8")],
        total: 2,
        sorts: None, restored: None, genres: None, genre: None,
        resolved: Default::default(),
    };
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    let _ = browse.pump();
    let items = &browse.state.states()[0].items;
    let flags = |i| items.get(i).map(|m| (m.watched, m.unwatched, m.resume_ms));
    assert_eq!(flags(0), Some((true, false, 0)), "the pressed item stays watched across the landing");
    assert_eq!(flags(1), Some((false, true, 90_000)), "an item nobody pressed is as the server said");
}

/// A read REQUESTED after the press already carries it: the server's answer stands, whatever it
/// says (a later read-back is the one that corrects an optimistic edit that was wrong).
#[test]
fn a_page_read_requested_after_the_edit_is_not_overwritten() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_page_source();
    let at = crate::viewstate::ViewAt::leaf(sid, "7");
    browse.state.ledger.record(&at, crate::viewstate::ViewEdit::Watched(true));
    browse.state.page_stamp = browse.state.ledger.stamp();
    let mut m = PmsMovie::default();
    m.sid = sid;
    m.rk = "7".into();
    m.unwatched = true;
    let r = PageResult {
        client,
        token_gen: client.token_gen(),
        gen: browse.state.query_gen(),
        sec: 0,
        start: 0,
        items: vec![m],
        total: 1,
        sorts: None, restored: None, genres: None, genre: None,
        resolved: Default::default(),
    };
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    let _ = browse.pump();
    assert!(browse.state.states()[0].items.get(0).is_some_and(|m| m.unwatched && !m.watched));
}

/// The Library's own Continue Watching/section shelves are read the same way: a list landing
/// carries the press made while it was out, and a card taken off the deck stays off.
#[test]
fn a_section_hubs_read_in_flight_across_edits_lands_with_them_made_again() {
    let sid = plx_plex::plex::ServerId::UNSET;
    let ledger = crate::viewledger::ViewLedger::default();
    let stamp = ledger.stamp();
    ledger.record(&crate::viewstate::ViewAt::leaf(sid, "1"), crate::viewstate::ViewEdit::Watched(true));
    ledger.record(&crate::viewstate::ViewAt::leaf(sid, "2"), crate::viewstate::ViewEdit::LeftDeck);
    let card = |rk: &str| PmsMovie { sid, rk: rk.into(), unwatched: true, ..Default::default() };
    let mut shelves = vec![
        section_hubs::Shelf { is_continue: true, items: vec![card("1"), card("2")], ..Default::default() },
        section_hubs::Shelf { items: vec![card("1"), card("2")], ..Default::default() },
    ];
    section_hubs::replay_since_request(&ledger, stamp, &mut shelves);
    assert_eq!(shelves[0].items.iter().map(|m| (m.rk.as_str(), m.watched)).collect::<Vec<_>>(), [("1", true)]);
    assert_eq!(shelves[1].items.iter().map(|m| (m.rk.as_str(), m.watched)).collect::<Vec<_>>(),
        [("1", true), ("2", false)], "a deck removal leaves other shelves alone");
}
