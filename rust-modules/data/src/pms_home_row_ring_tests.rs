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

/// A Home of `rows` pageable rows from one registered server, with the ring applied as a landing
/// would apply it.
fn ring_home(rows: usize) -> (Owner, ServerId) {
    plx_plex::plex::reset_servers_for_test();
    let id = plx_plex::plex::register_for_test("ring", "127.0.0.1", 9, "synthetic", "cid");
    let shelves = (0..rows).map(|r| pageable_shelf(id.raw(), r)).collect();
    let mut o = Owner::default();
    let mut source = src(id.raw(), "", HubState::Ready, Some(built(id.raw(), &[], shelves)));
    source.client = plx_plex::plex::client_for(id);
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
    let keys: Vec<String> = (page.start..page.start + CARDS).map(|c| format!("{row}-{c}")).collect();
    let mut answer = shelf(request.sid.raw(), "", &page.id, &keys.iter().map(String::as_str).collect::<Vec<_>>());
    answer.key = page.key.clone();
    answer.positions = (page.start..page.start + CARDS).collect();
    answer.offset = page.start;
    answer.end = page.start + CARDS;
    answer.more = true;
    answer.total = 1000;
    let build = SourceBuild { shelves: vec![answer], ..Default::default() };
    request.complete(Some(build))
}

struct Ring { o: Owner, launched: Vec<(String, usize, usize)>, pending: Vec<Landing> }

impl Ring {
    fn new(rows: usize) -> Self {
        let (o, _) = ring_home(rows);
        Self { o, launched: Vec::new(), pending: Vec::new() }
    }
    fn hold(&mut self, lo: usize, hi: usize) {
        let scope = BrowseScope::standalone();
        let (pending, launched) = (&mut self.pending, &mut self.launched);
        let mut launch = |request: HubRequest| {
            let page = request.page.clone().unwrap();
            launched.push((page.id.clone(), page.start, request.window.as_ref().map_or(0, |w| w.end)));
            pending.push(serve(request));
            true
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
            true
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
    assert_eq!(row.items[0].rk, "5-12", "the first card is the one at the window offset");
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
