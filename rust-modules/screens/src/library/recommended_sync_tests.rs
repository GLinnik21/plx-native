//! What a publication costs the Recommended page, and that the cheap way is the same page: a landing
//! on one row of a long page registers that row's keys and no other (the page's `rebuild` once
//! registered every slot of every row on each of the ~one publication per key press a long page
//! gets, 1.7 ms of a 2.4 ms frame on the host with 170 hubs), and the page built by a stream of
//! incremental publications is the page one full build of the last publication makes. The keys a
//! changed row gave up wait for a lazy sweep: the tests hold the key table to the bound the sweep
//! keeps, the per-region counts that bound is read from to a scan of the table, and the keys the
//! page held a publication ago to surviving it.
use super::*;
use super::recommended_tests::Fixture;
use plx_ui::fixture::Walk;
use plx_data::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};

const PER_ROW: usize = 24;

/// A section of `rows` hubs of `PER_ROW` cards, every hub pageable (so the store may give its cards
/// up), the first window of a listing of 500.
fn fixture_of(rows: usize) -> Fixture {
    Fixture::named_shelves(&hub_names(0..rows), PER_ROW)
}

fn hold(fixture: &mut Fixture, lo: usize, hi: usize) {
    let id = fixture.listing.view().id().expect("a section");
    fixture.stores.as_ref().unwrap().browse_run_for_test(BrowseCmd::Addressed {
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
    fixture.republish();
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
    fixture.republish();
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

/// One row of a page in the names that do not depend on how many keys were minted before it.
#[derive(Debug, PartialEq)]
struct SnapRow {
    id: String,
    /// The row's group key by identity, and the index it remembers.
    group: (LibraryIdentity, Option<usize>),
    /// The linked heading's group (identity and remembered index) and element (identity, and where it sits).
    heading: Option<((LibraryIdentity, Option<usize>), (LibraryIdentity, Option<(LibraryIdentity, usize)>))>,
    landscape: bool,
    /// Every card key by identity, and where its key remembers it sat, by the identity of that group.
    cards: Vec<(LibraryIdentity, Option<(LibraryIdentity, usize)>)>,
}

/// The rows of `page` in the names that do not depend on how many keys were minted before them:
/// every key by its identity, and where it remembers it sat (a card key by the group it sat in, a
/// group key by its row index).
fn snapshot(page: &RecommendedScreen) -> Vec<SnapRow> {
    let identity = |elem: u32| page.keys.key(elem).unwrap_or_else(|| panic!("a row names a key the registry lacks: {elem:#x}")).identity.clone();
    let place = |elem: u32| page.keys.last_place(elem).map(|(group, index)| (identity(group.0), index));
    let seat = |group: GroupId| (identity(group.0), page.keys.last_place(group.0).map(|(_, index)| index));
    page.shelves.iter().map(|row| SnapRow {
        id: row.id.clone(),
        group: seat(row.group),
        heading: row.heading.map(|(group, elem)| (seat(group), (identity(elem), place(elem)))),
        landscape: row.landscape,
        cards: row.elems.iter().map(|&elem| (identity(elem), place(elem))).collect(),
    }).collect()
}

/// The shared generator (`plx_ui::fixture::Walk`) drawing a `usize` below a bound.
struct Rng(Walk);
impl Rng {
    fn next(&mut self, below: usize) -> usize { self.0.below(below as u64) as usize }
}

/// What the random publications are drawn from.
struct Stream {
    /// Hubs on the first page, at least `rows` and below `rows + spread`.
    rows: usize,
    spread: usize,
    /// Cards a landing or a refresh gives a row, below this.
    cards: usize,
    /// Hub identifiers a refresh draws from: few and a hub recurs, many and each refresh brings new ones.
    pool: usize,
    steps: usize,
}

/// Run `stream` over `seeds`; how many times a sweep of the key table ran. After each publication
/// the page that followed them all is the page one build of the same publication makes, holds no key
/// the registry lacks, and its key table stands within the bound the sweep keeps.
fn run_random(stream: &Stream, seeds: std::ops::RangeInclusive<u64>) -> usize {
    let mut sweeps = 0;
    for seed in seeds {
        let mut rng = Rng(Walk(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x1000_0000_01b3)));
        let rows = stream.rows + rng.next(stream.spread);
        let mut per_row = stream.cards.min(PER_ROW);
        let mut fixture = Fixture::named_shelves(&hub_names(0..rows), per_row);
        let base_query = fixture.listing.view().id().expect("a section").query;
        let mut moved_query = 0u32;
        let mut page = fixture.screen();
        for step in 0..stream.steps {
            let n = page.shelves.len().max(1);
            let mut query_moves = false;
            let what = match rng.next(7) {
                0 | 1 => {
                    let row = rng.next(n);
                    let landed = fixture.stores.as_ref().unwrap().browse.borrow_mut().land_row_for_test(0, row, 1 + rng.next(stream.cards));
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
                    let count = 3 + rng.next(stream.rows + stream.spread);
                    let named = hub_names((0..count).map(|_| rng.next(stream.pool)));
                    // half the refreshes give the cards they gave before, so a hub that recurs (and
                    // has not been read since) is the row it was, moved
                    if rng.next(2) == 0 { per_row = 2 + rng.next(stream.cards.max(3) - 2); }
                    reseed(&mut fixture, &named, per_row);
                    format!("a refresh of {count} hubs, {per_row} cards each")
                }
                4 => {
                    let memory = page.page_memory();
                    page.restore(&memory);
                    "the page remounts over its memory".to_string()
                }
                5 => {
                    // the listing's query moves WITH a publication (a placeholder is keyed by the
                    // query it was minted under, and a row the store gave up is its placeholders)
                    let row = rng.next(n);
                    let landed = fixture.stores.as_ref().unwrap().browse.borrow_mut().land_row_for_test(0, row, 1 + rng.next(stream.cards));
                    query_moves = landed;
                    format!("the query moves with a read on row {row} ({landed})")
                }
                _ => "a publication that changes nothing".to_string(),
            };
            fixture.republish();
            if query_moves { moved_query += 1; }
            if moved_query > 0 { fixture.listing = fixture.listing.clone().with_query(base_query + moved_query); }
            let (cards_before, keys_before) = (page.cards_held(), page.keys.count(KeyRegion::Shelf));
            let held: Vec<u32> = page.shelves.iter().flat_map(|row| row.elems.iter().copied()).collect();
            let (publication, stale) = (page.shelf_publication, page.rows_stale);
            page.sync(&fixture.cx(None));
            let fresh = fixture.screen();
            let context = format!("seed {seed} step {step}: after {what}");
            assert_eq!(snapshot(&page), snapshot(&fresh), "{context}");
            // The table stands within what the lazy sweep keeps once a build has run (a sync that
            // builds nothing mints nothing, and stands where the last one left it): twice what the
            // page holds and held a publication ago, plus the slack, plus the batch a sweep waits for.
            let count = page.keys.count(KeyRegion::Shelf);
            let bound = key_cap(page.cards_held() + cards_before) + KEY_PRUNE_BATCH;
            if stale || page.shelf_publication != publication {
                assert!(count <= bound, "{context}: {count} keys over {bound}");
            }
            // and it still names what the page held a publication ago: a focus, or a group's
            // remembered cursor, on a card that has just left recovers through its key
            for elem in held {
                assert!(page.keys.key(elem).is_some(), "{context}: the key {elem:#x} the page held a publication ago is gone");
            }
            if count < keys_before { sweeps += 1; }
        }
    }
    sweeps
}

/// Random publications: a row lands, the hold band moves (rows give their cards up, held rows are
/// read again), a refresh brings a new list (rows inserted, dropped, reordered, one hub twice), the
/// listing's query moves, the page remounts over its own memory. After each, the page that followed
/// them all is the page one build of the same publication makes, group seats included.
#[test]
fn an_incremental_sync_is_the_page_a_full_build_makes() {
    let _guard = plx_base::testlock::serial();
    run_random(&Stream { rows: 4, spread: 30, cards: PER_ROW, pool: 40, steps: 40 }, 1..=12);
}

/// The same, on pages small enough that the lazy sweep of the key table runs (it waits for the table
/// to stand `KEY_PRUNE_BATCH` over twice what the page holds): the sweep drops the oldest keys a
/// publication left behind and never one the page or the focus can still name.
#[test]
fn an_incremental_sync_is_the_page_a_full_build_makes_across_the_sweep() {
    let _guard = plx_base::testlock::serial();
    let sweeps = run_random(&Stream { rows: 3, spread: 3, cards: 4, pool: 100_000, steps: 400 }, 1..=6);
    assert!(sweeps > 0, "premise: the stream is long enough to reach the sweep");
}

/// **The key table stays bounded over many rotating refreshes**: every refresh brings hubs the page
/// never held, so every refresh mints a key per card, and the lazy sweep takes the oldest back. The
/// table never stands over what the sweep keeps, at any step, and a sweep ran.
#[test]
fn the_key_table_stays_bounded_over_many_rotating_refreshes() {
    let _guard = plx_base::testlock::serial();
    let (rows, per_row) = (3, 12);
    let mut fixture = Fixture::named_shelves(&hub_names(0..rows), per_row);
    let mut page = fixture.screen();
    let (mut peak, mut sweeps) = (0, 0);
    for rotation in 1..=120 {
        let (cards_before, keys_before) = (page.cards_held(), page.keys.count(KeyRegion::Shelf));
        reseed(&mut fixture, &hub_names(rotation * rows..(rotation + 1) * rows), per_row);
        fixture.republish();
        page.sync(&fixture.cx(None));
        let bound = key_cap(page.cards_held() + cards_before) + KEY_PRUNE_BATCH;
        let count = page.keys.count(KeyRegion::Shelf);
        assert!(count <= bound, "rotation {rotation}: {count} item and slot keys over {bound}");
        assert_eq!(page.shelves.len(), rows, "rotation {rotation}: premise: every hub is a row");
        peak = peak.max(count);
        if count < keys_before { sweeps += 1; }
    }
    assert!(sweeps >= 2, "premise: 120 rotations of {} keys reach the sweep more than once ({sweeps})", rows * per_row);
    assert!(peak < 120 * rows * per_row, "the table did not grow with the session: peak {peak}");
    assert_eq!(snapshot(&page), snapshot(&fixture.screen()), "and the page is the one a full build makes");
}

/// **`KeyRegistry::count` is the table's count**: the per-region numbers the sweep's bound is read
/// from equal a scan of the table after any mix of minting, pruning and remounting from memory.
#[test]
fn the_registry_counts_each_region_as_a_scan_of_the_table_does() {
    use plx_plex::plex::ServerId;
    let section = |key| LibrarySectionIdentity { sid: ServerId::from_raw(1), key };
    let regions = [KeyRegion::Library, KeyRegion::Shelf, KeyRegion::Grid, KeyRegion::Rail, KeyRegion::Control];
    let scan = |keys: &KeyRegistry, region: KeyRegion| keys.keys().iter().filter(|key| keys.region(key.elem) == Some(region)).count();
    let identity = |rng: &mut Rng| -> LibraryIdentity {
        let (sid, rk, n) = (ServerId::from_raw(1 + rng.next(2) as u16), format!("rk{}", rng.next(60)), rng.next(60) as u32);
        match rng.next(9) {
            0 => LibraryIdentity::Library(section(n as i64)),
            1 => LibraryIdentity::Shelf { section: section(1), hub: format!("hub{}", n % 7), sid, rk },
            2 => LibraryIdentity::ShelfSlot { section: section(1), hub: format!("hub{}", n % 7), publication: n % 3, slot: n },
            3 => LibraryIdentity::Grid { section: section(1), sid, rk },
            4 => LibraryIdentity::GridSlot { section: section(1), query: n % 3, slot: n },
            5 => LibraryIdentity::Rail { section: section(1), label: rk },
            _ => LibraryIdentity::Control { section: section(1), kind: format!("kind{}", n % 4), key: rk },
        }
    };
    for seed in 1..=8u64 {
        let mut rng = Rng(Walk(0xd1b5_4a32_d192_ed03 ^ seed.wrapping_mul(0x1000_0000_01b3)));
        let mut keys = KeyRegistry::default();
        let mut pruned = 0;
        for step in 0..300 {
            let what = match rng.next(10) {
                0 => {
                    let region = regions[rng.next(regions.len())];
                    let (cap, kept) = (rng.next(12), 1 + rng.next(6) as u32);
                    let went = keys.prune(region, cap, |elem| elem % kept == 0);
                    pruned += usize::from(went);
                    format!("prune {region:?} to {cap} (kept every {kept}th): {went}")
                }
                1 => {
                    let memory = keys.remember(Some(section(1)), 0.0, Vec::new());
                    keys = KeyRegistry::restore(&memory);
                    "remount from memory".to_string()
                }
                _ => {
                    let (identity, index) = (identity(&mut rng), rng.next(30));
                    format!("register {:?}: {:#x}", identity, keys.register(identity.clone(), GroupId(9), index))
                }
            };
            for region in regions {
                assert_eq!(keys.count(region), scan(&keys, region), "seed {seed} step {step}: {region:?} after {what}");
            }
        }
        assert!(pruned > 0 && !keys.keys().is_empty(), "seed {seed}: premise: the mix pruned and minted");
    }
}
