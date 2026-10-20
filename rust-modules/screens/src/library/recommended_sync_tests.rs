//! What a publication costs the Recommended page, and that the cheap way is the same page: a landing
//! on one row of a long page registers that row's keys and no other (the page's `rebuild` once
//! registered every slot of every row on each of the ~one publication per key press a long page
//! gets, 1.7 ms of a 2.4 ms frame on the host with 170 hubs), and the page built by a stream of
//! incremental publications is the page one full build of the last publication makes.
use super::*;
use super::recommended_tests::Fixture;
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};

const PER_ROW: usize = 24;

/// A section of `rows` hubs of `PER_ROW` cards, every hub pageable (so the store may give its cards
/// up), the first window of a listing of 500.
fn fixture_of(rows: usize) -> Fixture {
    publish_named(&hub_names(0..rows), PER_ROW)
}

fn publish_named(rows: &[(String, String, String)], per_row: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let stores = plx_data::stores::Stores::default();
    stores.browse.borrow_mut().seed_two_source_table_for_test();
    stores.capture_browse(&mut fixture.directory);
    stores.browse_run(BrowseCmd::SetCur(0));
    {
        let mut browse = stores.browse.borrow_mut();
        browse.seed_items_for_test(120);
        let named: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
        browse.seed_named_shelves_for_test(0, &named, per_row);
        browse.seed_paging_for_test(0, 500);
    }
    fixture.stores = Some(stores);
    republish(&mut fixture);
    fixture
}

/// Capture what the store publishes now, as the dispatcher does between frames.
fn republish(fixture: &mut Fixture) {
    let stores = fixture.stores.take().expect("a stored fixture");
    let publication = stores.capture_browse(&mut fixture.directory);
    fixture.listing = publication.listing;
    fixture.hubs = publication.section_hubs;
    fixture.stores = Some(stores);
}

fn hold(fixture: &mut Fixture, lo: usize, hi: usize) {
    let id = fixture.listing.view().id().expect("a section");
    fixture.stores.as_ref().unwrap().browse_run(BrowseCmd::Addressed {
        target: SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section },
        work: LibraryWork::HubHold { lo, hi },
    });
}

/// The server answers a refresh: the hubs it now lists, `per_row` cards each.
fn reseed(fixture: &mut Fixture, rows: &[(String, String, String)], per_row: usize) {
    let mut browse = fixture.stores.as_ref().unwrap().browse.borrow_mut();
    let refs: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
    browse.seed_named_shelves_for_test(0, &refs, per_row);
    browse.seed_paging_for_test(0, 500);
}

fn hub_names(ids: impl Iterator<Item = usize>) -> Vec<(String, String, String)> {
    ids.map(|id| (format!("movie.hub{id}"), format!("/hubs/sections/1/hub{id}"), format!("Hub {id}"))).collect()
}

/// The keys registered by the one publication that lands row 3.
fn probes_for_one_landing(rows: usize) -> usize {
    let _guard = plx_base::testlock::serial();
    let mut fixture = fixture_of(rows);
    let mut page = fixture.screen();
    assert_eq!(page.shelves.len(), rows, "premise: every hub is a row");
    let before = page.shelves[3].elems.clone();
    page.keys.reset_register_probes();
    assert!(fixture.stores.as_ref().unwrap().browse.borrow_mut().land_row_for_test(0, 3, PER_ROW), "premise: the row took the landing");
    republish(&mut fixture);
    page.sync(&fixture.cx(None));
    assert_ne!(page.shelves[3].elems, before, "the landed row holds its new cards' keys");
    page.keys.register_probes()
}

/// **A landing on one row of a long page costs that row.** With 170 hubs the page re-registered all
/// 170 x 25 keys on every publication; the cost is the row's slots and its group, for 10 hubs and
/// for 170 alike.
#[test]
fn landing_one_row_registers_that_rows_keys_whatever_the_page_holds() {
    let (small, large) = (probes_for_one_landing(10), probes_for_one_landing(170));
    assert_eq!(small, large, "a landing costs the same on 10 hubs and on 170");
    assert!(large <= PER_ROW + 2, "the row's slots, its group and a heading at most: {large}");
}

/// **A hub inserted above the page moves every row below it, and costs them their seats, not their
/// cards**: each moved row's group key remembers its index, the 24 card keys do not.
#[test]
fn a_hub_inserted_above_seats_the_rows_it_moved_without_registering_their_cards() {
    let _guard = plx_base::testlock::serial();
    let rows = 40;
    let mut fixture = fixture_of(rows);
    let mut page = fixture.screen();
    page.keys.reset_register_probes();
    reseed(&mut fixture, &hub_names(std::iter::once(900).chain(0..rows)), PER_ROW);
    republish(&mut fixture);
    page.sync(&fixture.cx(None));
    assert_eq!(page.shelves.len(), rows + 1);
    // the new row's group and slots, a seat (the group) for each moved row, and the one row the
    // hold band pushed out (it was the sixteenth held; the store gives its cards up and its
    // placeholders are keyed again): never the cards of every row.
    assert!(page.keys.register_probes() <= (rows + 1) + 2 * (PER_ROW + 1),
        "a seat for each moved row, the rows that changed, not every card of every row: {}", page.keys.register_probes());
    assert_eq!(snapshot(&page), snapshot(&fixture.screen()), "the page is the one a full build makes");
    // and the moved rows' group keys remember where they stand now
    for (index, row) in page.shelves.iter().enumerate() {
        assert_eq!(page.keys.last_place(row.group.0).map(|(_, at)| at), Some(index), "row {index}'s group key");
    }
}

/// What a row's keys were minted from is what decides whether a publication changed it.
#[test]
fn a_rows_basis_tells_a_changed_row_from_the_one_it_was_built_from() {
    use plx_data::browse::section_hubs::Shelf;
    let cards = |rks: &[&str]| rks.iter().map(|rk| plx_data::pms::PmsMovie { rk: (*rk).into(), ..Default::default() }).collect::<Vec<_>>();
    let shelf = |rks: &[&str]| Shelf { id: "hub".into(), items: cards(rks), ..Default::default() };
    let held = shelf(&["a", "b", "c"]);
    let basis = Basis::of(&held, 7);
    assert!(basis.matches(&held, 7) && basis.matches(&held, 8), "a row of cards does not depend on the query");
    assert!(!basis.matches(&shelf(&["a", "b", "x"]), 7), "a card with another ratingKey");
    assert!(!basis.matches(&shelf(&["a", "b"]), 7), "a shorter window");
    let mut other_server = held.clone();
    other_server.items[1].sid = plx_plex::plex::ServerId::from_raw(3);
    assert!(!basis.matches(&other_server, 7), "the same ratingKey on another server is another card");
    let mut landscape = held.clone();
    landscape.landscape = true;
    assert!(!basis.matches(&landscape, 7), "a row drawn another way");
    // a row the store gave its cards up to: its placeholders carry the query they were minted under
    let released = Shelf { id: "hub".into(), shown: 3, ..Default::default() };
    assert_eq!(released.slots(), 3, "premise: a released row is its placeholders");
    let basis = Basis::of(&released, 7);
    assert!(basis.matches(&released, 7));
    assert!(!basis.matches(&released, 8), "placeholders are minted per query");
    assert!(!basis.matches(&held, 7), "a row that got its cards back");
    assert!(!Basis::of(&held, 7).matches(&released, 7), "a row that gave them up");
}

// ---- the incremental page is the page a full build makes --------------------------------------------

type Snapshot = Vec<(String, LibraryIdentity, Option<(LibraryIdentity, LibraryIdentity)>, bool, Vec<(LibraryIdentity, Option<(LibraryIdentity, usize)>)>)>;

/// The rows of `page` in the names that do not depend on how many keys were minted before them:
/// every key by its identity, and where its key remembers it sat by the identity of that group.
fn snapshot(page: &RecommendedScreen) -> Snapshot {
    let identity = |elem: u32| page.keys.key(elem).unwrap_or_else(|| panic!("a row names a key the registry lacks: {elem:#x}")).identity.clone();
    let place = |elem: u32| page.keys.last_place(elem).map(|(group, index)| (identity(group.0), index));
    page.shelves.iter().map(|row| (
        row.id.clone(), identity(row.group.0),
        row.heading.map(|(group, elem)| (identity(group.0), identity(elem))), row.landscape,
        row.elems.iter().map(|&elem| (identity(elem), place(elem))).collect(),
    )).collect()
}

struct Rng(u64);
impl Rng {
    fn next(&mut self, below: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % below as u64) as usize
    }
}

/// Random publications: a row lands, the hold band moves (rows give their cards up, held rows are
/// read again), a refresh brings a new list (rows inserted, dropped, reordered, one hub twice), the
/// page remounts over its own memory. After each, the page that followed them all is the page one
/// build of the same publication makes, and it holds no key the registry lacks.
#[test]
fn an_incremental_sync_is_the_page_a_full_build_makes() {
    let _guard = plx_base::testlock::serial();
    for seed in 1..=12u64 {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x1000_0000_01b3));
        let rows = 4 + rng.next(30);
        let mut fixture = fixture_of(rows);
        let mut page = fixture.screen();
        for step in 0..40 {
            let n = page.shelves.len().max(1);
            let what = match rng.next(6) {
                0 | 1 => {
                    let row = rng.next(n);
                    let landed = fixture.stores.as_ref().unwrap().browse.borrow_mut().land_row_for_test(0, row, 1 + rng.next(PER_ROW));
                    format!("a read lands on row {row} ({landed})")
                }
                2 => {
                    let lo = rng.next(n);
                    let hi = lo + rng.next(20);
                    hold(&mut fixture, lo, hi);
                    if rng.next(2) == 0 { fixture.stores.as_ref().unwrap().browse.borrow_mut().land_held_rows_for_test(0); }
                    format!("the hold moves to {lo}..{hi}")
                }
                3 => {
                    // a refresh: ids drawn from a pool (a hub can recur, and one can come twice), in any order
                    let count = 3 + rng.next(30);
                    let named = hub_names((0..count).map(|_| rng.next(40)));
                    let per_row = 2 + rng.next(PER_ROW - 2);
                    reseed(&mut fixture, &named, per_row);
                    format!("a refresh of {count} hubs, {per_row} cards each")
                }
                4 => {
                    let memory = page.page_memory();
                    page.restore(&memory);
                    "the page remounts over its memory".to_string()
                }
                _ => "a publication that changes nothing".to_string(),
            };
            republish(&mut fixture);
            page.sync(&fixture.cx(None));
            let fresh = fixture.screen();
            assert_eq!(snapshot(&page), snapshot(&fresh), "seed {seed} step {step}: after {what}");
        }
    }
}
