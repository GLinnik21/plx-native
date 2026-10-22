//! A page read already in flight when a view-state edit is published lands carrying the pre-edit
//! body; the ledger re-patches it. Every test here takes its stamp from the store's REAL request
//! site (`maybe_spawn_owned`, `hubs_kick`) against a registered but unreachable server, so removing
//! the stamp from a request site fails it.

use super::test_support::*;
use super::*;
use crate::viewstate::{ViewAt, ViewEdit};

/// `registered_page_source`, but on a loopback port nothing listens on: the request path really
/// spawns its worker, which is refused at once instead of dialling 10.0.0.1.
fn loopback_page_source() -> (RegisteredCleanup, TestBrowse, ServerId, &'static plx_plex::plex::Client) {
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("browse-ledger", "127.0.0.1", 9, "old", "cid");
    assert!(plx_plex::plex::set_current(sid));
    let mut source = a_source("original", "", true);
    source.sid = sid;
    source.sections_done = true;
    source.counts_done = true;
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![source]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    (RegisteredCleanup, browse, sid, plx_plex::plex::client_for(sid).unwrap())
}

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
    browse.state.maybe_spawn_owned(&browse.adapter); // the page read goes out…
    assert!(browse.adapter.fetching.load(Ordering::SeqCst), "the real request site claimed the fetch");
    ledger.record(&ViewAt::leaf(sid, "7"), ViewEdit::Watched(true)); // …then the press
    plx_base::task::drain_workers_for_test();
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
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r); // the refused worker's answer, replaced
    let _ = browse.state.pump_owned(&browse.adapter);
    plx_base::task::drain_workers_for_test();
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
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    let ledger = browse.state.ledger.clone();
    let older = ledger.record(&ViewAt::leaf(sid, "0"), ViewEdit::Watched(true));
    ledger.landed(older); // before the request
    browse.state.hubs_kick(0, &browse.adapter); // the hubs read goes out…
    assert!(browse.adapter.hubs.fetching.load(Ordering::SeqCst), "the real request site claimed the fetch");
    ledger.record(&ViewAt::leaf(sid, "1"), ViewEdit::Watched(true)); // …then two presses
    ledger.record(&ViewAt::leaf(sid, "2"), ViewEdit::LeftDeck);
    plx_base::task::drain_workers_for_test();
    let card = |rk: &str| PmsMovie { sid, rk: rk.into(), unwatched: true, ..Default::default() };
    let shelves = vec![
        section_hubs::Shelf { is_continue: true, items: vec![card("1"), card("2")], ..Default::default() },
        section_hubs::Shelf { items: vec![card("0"), card("1"), card("2")], ..Default::default() },
    ];
    *browse.adapter.hubs.result.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(section_hubs::HubResult::list_for_test(browse.state.table_epoch(), 0, client, client.token_gen(), shelves));
    browse.state.hubs_land(&browse.adapter, plx_machine::landgate::fixture_gate());
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
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
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
    assert!(browse.state.hubs_page(0, seen, ID, KEY, false, &browse.adapter), "the page ask was queued");
    assert!(browse.adapter.hubs.fetching.load(Ordering::SeqCst), "the real row request site claimed the fetch");
    ledger.record(&ViewAt::leaf(sid, "21"), ViewEdit::Watched(true)); // the press while the page is out
    plx_base::task::drain_workers_for_test();
    let mut next = browse.state.states()[0].hubs.committed[0].clone();
    let card = |rk: &str| PmsMovie { sid, rk: rk.into(), unwatched: true, ..Default::default() };
    next.items = vec![card("20"), card("21")];
    next.positions = vec![12, 13];
    next.offset = 12;
    next.end = 14;
    *browse.adapter.hubs.result.lock().unwrap_or_else(|e| e.into_inner()) =
        Some(section_hubs::HubResult::row_for_test(browse.state.table_epoch(), 0, client, client.token_gen(), ID, KEY, next));
    browse.state.hubs_land(&browse.adapter, plx_machine::landgate::fixture_gate());
    let landed = &browse.state.states()[0].hubs.committed[0];
    let marks: Vec<_> = landed.items.iter().map(|m| (m.rk.as_str(), m.watched)).collect();
    assert_eq!(marks, [("20", false), ("21", true)]);
}

/// A server's created or removed items re-list the open grid: its rows stay on screen while the
/// re-list is out, and its first page then replaces them whole, at the new total, in place.
#[test]
fn a_re_list_keeps_the_rows_until_its_first_page_replaces_them() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    {
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 3;
        state.items = SecItems::from_vec(vec![Some(row(sid, "6")), Some(row(sid, "7")), Some(row(sid, "8"))]);
    }
    browse.state.want(0, PAGE, None);
    assert!(browse.state.run_owned(&browse.adapter, crate::stores::browse::BrowseCmd::ListingsChanged { sid }));
    browse.state.maybe_spawn_owned(&browse.adapter);
    assert!(browse.adapter.fetching.load(Ordering::SeqCst), "the re-list asks though no page is missing");
    assert_eq!(browse.state.states()[0].items.get(1).map(|m| m.rk.as_str()), Some("7"), "the old rows stay meanwhile");
    plx_base::task::drain_workers_for_test();
    let r = PageResult {
        client,
        token_gen: client.token_gen(),
        gen: browse.state.query_gen(),
        sec: 0,
        start: 0,
        items: vec![row(sid, "6"), row(sid, "8")],
        total: 2,
        sorts: None, restored: None, genres: None, genre: None,
        resolved: Default::default(),
    };
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    let _ = browse.state.pump_owned(&browse.adapter);
    plx_base::task::drain_workers_for_test();
    let state = &browse.state.states()[0];
    let rks: Vec<_> = (0..3).map(|i| state.items.get(i).map(|m| m.rk.clone())).collect();
    assert_eq!(rks, [Some("6".into()), Some("8".into()), None], "the removed row is gone and nothing is left past the end");
    assert_eq!(state.total, 2);
    assert!(!state.relist && !browse.state.requery_owed(), "spent");
}

/// A long scan reports change after change: a re-list already out is kept (never dropped for the
/// next ask, which could starve the grid), and exactly one more follows it.
#[test]
fn a_change_while_a_re_list_is_out_keeps_it_and_owes_one_more() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    {
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 3;
        state.items = SecItems::from_vec(vec![Some(row(sid, "6")), Some(row(sid, "7")), Some(row(sid, "8"))]);
    }
    browse.state.want(0, PAGE, None);
    let relist = |browse: &mut TestBrowse| browse.state.run_owned(&browse.adapter, crate::stores::browse::BrowseCmd::ListingsChanged { sid });
    relist(&mut browse);
    browse.state.maybe_spawn_owned(&browse.adapter);
    let gen = browse.state.query_gen();
    relist(&mut browse);
    relist(&mut browse);
    assert_eq!(browse.state.query_gen(), gen, "the re-list out is not dropped");
    plx_base::task::drain_workers_for_test();
    let land = |browse: &mut TestBrowse, rks: &[&str]| {
        let r = PageResult {
            client, token_gen: client.token_gen(), gen: browse.state.query_gen(), sec: 0, start: 0,
            items: rks.iter().map(|rk| row(sid, rk)).collect(), total: rks.len() as i64,
            sorts: None, restored: None, genres: None, genre: None, resolved: Default::default(),
        };
        *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        let _ = browse.state.pump_owned(&browse.adapter);
        plx_base::task::drain_workers_for_test();
    };
    land(&mut browse, &["6", "8"]);
    assert_eq!(browse.state.states()[0].total, 2, "its answer lands");
    assert!(browse.state.states()[0].relist && browse.state.requery_owed(), "one more re-list is owed");
    browse.adapter.fetching.store(false, Ordering::SeqCst);
    browse.state.maybe_spawn_owned(&browse.adapter);
    plx_base::task::drain_workers_for_test();
    land(&mut browse, &["8"]);
    assert_eq!(browse.state.states()[0].total, 1);
    assert!(!browse.state.states()[0].relist, "and no more after it");
}
