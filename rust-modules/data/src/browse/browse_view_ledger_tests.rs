//! A page read already in flight when a view-state edit is published lands carrying the pre-edit
//! body; the ledger re-patches it. Every test here takes its stamp from the store's REAL request
//! site (`maybe_spawn_owned`, `hubs_kick`) against a registered but unreachable server, so removing
//! the stamp from a request site fails it. The section hubs' read is a job the test takes off the
//! outbox and answers through its port, as the application's runner would.

use super::test_support::*;
use super::*;
use crate::viewstate::{ViewAt, ViewEdit};
use plx_machine::job::Port;

fn row(sid: ServerId, rk: &str) -> PmsMovie {
    let mut m = PmsMovie::default();
    m.sid = sid;
    m.rk = rk.to_string();
    m.unwatched = true;
    m.resume_ms = 90_000;
    m
}

/// A Browse page read is out, the user marks an item on that page watched, and the read lands
/// with the row as the server described it BEFORE the press. The tile must stay watched; an
/// item pressed before the request (the server has it) is left as the server said.
#[test]
fn a_page_read_in_flight_across_a_watched_edit_lands_watched() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    let ledger = browse.state.ledger.clone();
    let older = ledger.record(&ViewAt::leaf(sid, "6"), ViewEdit::Watched(true));
    ledger.landed(older); // before the request: the server has it
    browse.state.want(0, PAGE, None);
    browse.maybe_spawn(); // the page read goes out…
    assert!(browse.adapter.page.busy(), "the real request site claimed the fetch");
    assert_eq!(browse.jobs.len(), 1, "…as a job the application's runner starts");
    ledger.record(&ViewAt::leaf(sid, "7"), ViewEdit::Watched(true)); // …then the press
    let r = PageResult {
        client,
        token_gen: client.token_gen(),
        gen: browse.state.query_gen(),
        sec: 0,
        start: 0,
        items: vec![row(sid, "6"), row(sid, "7"), row(sid, "8")],
        total: 3,
        sorts: None, restored: None, genres: None, genre: None,
        resolved: Default::default(),
    };
    browse.adapter.page.post(r, |_| true); // the read's answer, as the server described it before the press
    let _ = browse.state.pump_owned(&browse.adapter);
    let items = &browse.state.states()[0].items;
    let flags = |i| items.get(i).map(|m| (m.watched, m.unwatched, m.resume_ms));
    assert_eq!(flags(0), Some((false, true, 90_000)), "pressed before the request: the server's answer stands");
    assert_eq!(flags(1), Some((true, false, 0)), "pressed while the read was out: stays watched");
    assert_eq!(flags(2), Some((false, true, 90_000)), "an item nobody pressed is as the server said");
}

/// The Library's own section shelves are read the same way: the list landing carries the press
/// made while it was out, and a card taken off the deck stays off. Through `hubs_kick`.
#[test]
fn a_section_hubs_read_in_flight_across_edits_lands_with_them_made_again() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    let ledger = browse.state.ledger.clone();
    let older = ledger.record(&ViewAt::leaf(sid, "0"), ViewEdit::Watched(true));
    ledger.landed(older); // before the request
    browse.hubs_kick(0); // the hubs read goes out…
    assert!(browse.adapter.hubs.fetch.busy(), "the real request site claimed the fetch");
    ledger.record(&ViewAt::leaf(sid, "1"), ViewEdit::Watched(true)); // …then two presses
    ledger.record(&ViewAt::leaf(sid, "2"), ViewEdit::LeftDeck);
    let (job, port) = browse.take_hubs_job();
    let card = |rk: &str| PmsMovie { sid, rk: rk.into(), unwatched: true, ..Default::default() };
    let shelves = vec![
        section_hubs::Shelf { is_continue: true, items: vec![card("1"), card("2")], ..Default::default() },
        section_hubs::Shelf { items: vec![card("0"), card("1"), card("2")], ..Default::default() },
    ];
    port.done(job.addr, section_hubs::BrowseHubsAnswer::list_for_test(&job, shelves)); // the runner's worker answers
    browse.hubs_land();
    assert!(browse.state.state_mut(0).unwrap().hubs.commit_staged(true));
    let landed = browse.state.states()[0].hubs.shelves().to_vec();
    let marks = |i: usize| landed[i].items.iter().map(|m| (m.rk.as_str(), m.watched)).collect::<Vec<_>>();
    assert_eq!(marks(0), [("1", true)], "a card taken off the deck while the read was out stays off");
    assert_eq!(marks(1), [("0", false), ("1", true), ("2", false)], "a deck removal leaves other shelves alone");
}

/// A row's page read (`hubs_spawn_row`, the second hubs request site) takes its own stamp: a press
/// made while the page was out is made again on the page that lands; one whose write had landed
/// before the request is not.
#[test]
fn a_hub_row_page_in_flight_across_a_watched_edit_lands_watched() {
    const ID: &str = "movie.row0";
    const KEY: &str = "/hubs/sections/1/row0";
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    section_hubs::seed_named_shelves_for_owner_test(&mut browse.state, 0, &[(ID, KEY, "Row")], 12);
    {
        let st = browse.state.state_mut(0).unwrap();
        let shelf = &mut Arc::make_mut(&mut st.hubs.committed)[0];
        shelf.more = true;
        shelf.total = 500;
    }
    let ledger = browse.state.ledger.clone();
    let older = ledger.record(&ViewAt::leaf(sid, "20"), ViewEdit::Watched(true));
    ledger.landed(older); // before the request
    let seen = browse.state.states()[0].hubs.revision;
    assert!(browse.hubs_page(0, seen, ID, KEY, false), "the page ask was queued");
    assert!(browse.adapter.hubs.fetch.busy(), "the real row request site claimed the fetch");
    ledger.record(&ViewAt::leaf(sid, "21"), ViewEdit::Watched(true)); // the press while the page is out
    let (job, port) = browse.take_hubs_job();
    let mut next = browse.state.states()[0].hubs.committed[0].clone();
    let card = |rk: &str| PmsMovie { sid, rk: rk.into(), unwatched: true, ..Default::default() };
    next.items = vec![card("20"), card("21")];
    next.positions = vec![12, 13];
    next.offset = 12;
    next.end = 14;
    port.done(job.addr, section_hubs::BrowseHubsAnswer::row_for_test(&job, ID, KEY, next));
    browse.hubs_land();
    let landed = &browse.state.states()[0].hubs.committed[0];
    let marks: Vec<_> = landed.items.iter().map(|m| (m.rk.as_str(), m.watched)).collect();
    assert_eq!(marks, [("20", false), ("21", true)]);
}

/// A server's created or removed items never re-list the open grid under the reader: the rows stay
/// exactly as they are, no page is read, and the listing is only marked stale (the Library page
/// re-queries it when a page covers it). The wanted window straddles a page boundary, which is
/// where a re-list that refilled one page would turn the other page's tiles into skeletons.
#[test]
fn a_library_change_leaves_the_open_grid_alone_and_marks_it_stale() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _) = loopback_page_source();
    {
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 2 * PAGE as i64;
        state.items = SecItems::from_vec((0..2 * PAGE).map(|i| Some(row(sid, &i.to_string()))).collect());
    }
    browse.state.want(PAGE - 10, PAGE + 20, Some(PAGE + 5));
    let gen = browse.state.query_gen();
    assert!(browse.state.run_owned(&browse.adapter, crate::stores::browse::BrowseCmd::ListingsChanged { sid, library_on_stack: true },
        &ClientKeys::default(), &mut Vec::new()));
    browse.maybe_spawn();
    assert!(!browse.adapter.page.busy(), "the visible grid is not listed again");
    assert!(browse.jobs.is_empty(), "…and no job asks for it: {:?}", browse.jobs);
    let state = &browse.state.states()[0];
    assert_eq!((state.fetch, state.total), (SecFetch::Ready, 2 * PAGE as i64));
    assert!((0..2 * PAGE).all(|i| state.items.get(i).is_some()), "no tile turns into a skeleton");
    assert_eq!(browse.state.query_gen(), gen, "no fetch generation moved: nothing is dropped or asked");
    assert!(browse.state.listing_snapshot().view().membership_stale(), "the listing knows it is out of date");
}
