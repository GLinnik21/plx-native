//! The calendar families' bands on the data side: the date families (`addedAt`, `lastViewedAt`,
//! `episode.addedAt`) and the release date, counted by the production body over a MODEL of the server
//! (`bands_job_tests::Server`) that holds generated dates, answers `>=` range reads and the
//! `:asc:nullsLast` extent read the way the measured rules say, and records what it was asked. The
//! screens draw all of these families (`BandFamily::rail_ready`; `library/tests.rs` drives the rail over
//! the bands this body lands), so the body, the key and the landing are driven directly here with the
//! family as a parameter.
//!
//! Edges and expected starts are computed here from an independent calendar (a day counter, not
//! `bands.rs`'s arithmetic), so an off-by-one edge, a wrong clock, one zone offset for every edge or a
//! band that lands under the wrong label fails against the generated list.

use super::bands_tests::{banded_source, terms, Item, Server};
use super::tests::{tick_directory, the_directory_job};
use super::*;
use crate::browse::bands::Zone;
use crate::browse::SecKind;
use crate::stores::browse::LibraryWork;
use plx_plex::plex::Oldest;
use std::cell::RefCell;

const TODAY: Day = [2026, 10, 31];
const HOUR: i64 = 3600;

fn leap(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since 1970-01-01 by counting, for any year this file uses.
fn days(date: Day) -> i64 {
    let [year, month, day] = date;
    let year_len = |y: i32| if leap(y) { 366 } else { 365 };
    let mut n: i64 = if year >= 1970 { (1970..year).map(year_len).sum() } else { -(year..1970).map(year_len).sum::<i64>() };
    let lengths = [31, if leap(year) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    n += lengths[..month as usize - 1].iter().sum::<i64>() + i64::from(day) - 1;
    n
}

fn utc(date: Day) -> i64 {
    days(date) * 86_400
}

/// A viewer five hours behind UTC all year: local midnight is 05:00 UTC.
fn flat_shift(_: Day) -> i64 {
    5 * HOUR
}

/// A viewer whose clocks went forward on 9 March 2026: five hours behind UTC before, four after, so
/// every local midnight from then on is an hour earlier in UTC.
fn dst_shift(date: Day) -> i64 {
    if date >= [2026, 3, 9] { 4 * HOUR } else { 5 * HOUR }
}

fn zone_of(midnight: fn(Day) -> Option<i64>) -> Zone {
    // a stamp's civil date is read in the zone's standing offset, which the extent's year never tests
    Zone { local_midnight: midnight, local_date: |epoch| (bands::utc_zone().local_date)(epoch - 5 * HOUR) }
}

fn flat_zone() -> Zone {
    zone_of(|date| Some(utc(date) + flat_shift(date)))
}

fn dst_zone() -> Zone {
    zone_of(|date| Some(utc(date) + dst_shift(date)))
}

/// Where a family's bands begin, highest first, written out from the board: independent of `plan`.
fn edges(family: BandFamily, oldest_year: i32, shift: fn(Day) -> i64) -> Vec<i64> {
    let local = |date: Day| utc(date) + shift(date);
    if family == BandFamily::Release {
        let mut edges = vec![utc([2026, 1, 1])];
        edges.extend((oldest_year.div_euclid(10)..=202).rev().map(|decade| utc([decade * 10, 1, 1])));
        return edges;
    }
    let mut edges = vec![local(TODAY)];
    edges.extend((1..=10).rev().map(|month| local([2026, month, 1])));
    edges.extend((oldest_year..2026).rev().map(|year| local([year, 1, 1])));
    edges
}

/// The edges of the ladder the body draws for `filters`: its extent is the oldest value the listing holds
/// (the section's oldest episode for the last episode added, whatever the filters), read in the family's
/// own clock.
fn edges_for(family: BandFamily, items: &[Item], filters: &[(String, String)], shift: fn(Day) -> i64) -> Vec<i64> {
    let scoped = if family == BandFamily::LastEpisodeAdded { &[][..] } else { filters };
    let oldest = items.iter().filter(|item| Server::passes(item, scoped)).filter_map(|item| item.value).min().unwrap();
    let year = if family == BandFamily::Release { year_of(oldest) } else { year_of(oldest - 5 * HOUR) };
    edges(family, year, shift)
}

fn field(family: BandFamily) -> &'static str {
    family.field()
}

/// Generated items: a value on each edge and a second either side of it (none below the lowest, which
/// is the extent), a few with no value, and the rest random; a third unwatched, two genres.
fn items(edges: &[i64], len: usize, seed: u64) -> Vec<Item> {
    let mut palette = vec![None, None];
    for (i, &edge) in edges.iter().enumerate() {
        if i + 1 < edges.len() {
            palette.push(Some(edge - 1));
        }
        palette.extend([Some(edge), Some(edge + 1)]);
    }
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize
    };
    // every palette value at least once, so the lowest edge is always the extent
    let mut items: Vec<Item> = palette.iter().map(|&value| Item { value, unwatched: next() % 3 == 0, genre: 7 + (next() % 2) as u8 * 2 }).collect();
    while items.len() < len {
        let value = palette[next() % palette.len()];
        items.push(Item { value, unwatched: next() % 3 == 0, genre: if next() % 2 == 0 { 7 } else { 9 } });
    }
    items
}

/// The band (0 = highest) of a value, or none: no value, or below the lowest edge.
fn band_of(edges: &[i64], value: Option<i64>) -> Option<usize> {
    let value = value?;
    edges.iter().position(|&edge| value >= edge)
}

/// The grid's rows for `filters`, as the measured server orders them (no value first in `:asc`, last in
/// `:desc`), each as its band.
fn rows(edges: &[i64], items: &[Item], filters: &[(String, String)], descending: bool) -> Vec<Option<usize>> {
    let mut values: Vec<Option<i64>> =
        items.iter().filter(|item| Server::passes(item, filters)).map(|item| item.value).collect();
    values.sort_by_key(|value| value.map_or(i64::MIN, |v| v));
    if descending {
        values.reverse();
    }
    values.into_iter().map(|value| band_of(edges, value)).collect()
}

#[track_caller]
fn assert_layout(bands: usize, rows: &[Option<usize>], layout: &bands::Offsets, descending: bool, what: &str) {
    assert_eq!(layout.bands.len(), bands, "{what}");
    let order: Vec<usize> = if descending { (0..bands).collect() } else { (0..bands).rev().collect() };
    for (start, band) in layout.bands.iter().zip(order) {
        let size = rows.iter().filter(|&&row| row == Some(band)).count() as u32;
        assert_eq!(start.size, size, "{what}: band {band} size");
        if size > 0 {
            let first = rows.iter().position(|&row| row == Some(band)).unwrap() as u32;
            assert_eq!(start.start, first, "{what}: band {band} starts at its first item (descending {descending})");
        }
    }
    let unbanded = rows.iter().filter(|row| row.is_none()).count() as u32;
    let (lead, tail) = if descending { (0, unbanded) } else { (unbanded, 0) };
    assert_eq!((layout.lead, layout.tail), (lead, tail), "{what}: the items with no value");
}

fn year_of(epoch: i64) -> i32 {
    (bands::utc_zone().local_date)(epoch).unwrap()[0]
}

/// What the model answers an extent read: the first non-null value of the listing (the section's oldest
/// episode, for the episodes read), as the instant or the year the family's field is.
fn oldest_of(family: BandFamily, server: &Server, terms: &[(String, String)]) -> Oldest {
    let rows = server.items.borrow();
    let episodes = terms == [("type".to_owned(), "4".to_owned())];
    let min = rows.iter().filter(|item| episodes || Server::passes(item, terms)).filter_map(|item| item.value).min();
    match min {
        None => Oldest::Nothing,
        Some(value) if family == BandFamily::Release => Oldest::Year(year_of(value)),
        Some(value) => Oldest::Instant(value),
    }
}

const FAMILIES: [(BandFamily, i32); 4] = [
    (BandFamily::DateAdded, 2003), (BandFamily::DateViewed, 2011), (BandFamily::LastEpisodeAdded, 2008),
    (BandFamily::Release, 1965),
];

fn count_read(family: BandFamily, today: Option<Day>, zone: Zone, filters: &[(String, String)], server: &Server)
    -> BandsRead {
    read_bands(Some(family), today, zone, filters, |terms| server.count(terms),
        |terms, _| Some(oldest_of(family, server, terms)))
}

/// The acceptance: for every calendar family, every filter combination and both directions (the last
/// episode added only descending: ascending it orders by the OLDEST episode), the starts the counts give
/// are the first index of each band in the list the server would sort, with a no-value lead or tail; once
/// with a viewer whose clocks changed in March, so a single zone offset for every edge fails too.
#[test]
fn the_counts_of_a_calendar_family_become_the_starts_of_its_bands() {
    let filters_cases: [Vec<(String, String)>; 4] = [
        Vec::new(), terms(&[("unwatched", "1")]), terms(&[("genre", "7")]),
        terms(&[("type", "2"), ("unwatchedLeaves", "1"), ("genre", "9")]),
    ];
    for (shift, zone) in [(flat_shift as fn(Day) -> i64, flat_zone()), (dst_shift, dst_zone())] {
        for (family, oldest_year) in FAMILIES {
            let shift = if family == BandFamily::Release { flat_shift } else { shift };
            for seed in [1, 2, 3] {
                let items = items(&edges(family, oldest_year, shift), 120, seed);
                for filters in &filters_cases {
                    let edges = edges_for(family, &items, filters, shift);
                    let what = format!("{family:?} seed {seed} {filters:?}");
                    let server = Server::new(items.clone(), field(family), 1);
                    let rail = match count_read(family, Some(TODAY), zone, filters, &server) {
                        BandsRead::Counted(Some(rail)) => rail,
                        _ => panic!("{what}: a rail"),
                    };
                    for descending in [true, false] {
                        if family == BandFamily::LastEpisodeAdded && !descending {
                            continue;
                        }
                        let rows = rows(&edges, &items, filters, descending);
                        assert_layout(edges.len(), &rows, rail.listed(descending), descending, &what);
                    }
                }
            }
        }
    }
}

/// The reads, in order and spelled as the server reads them: the listing's size, ONE extent read (the
/// first row of `<field>:asc:nullsLast`, the page's own terms; the section's episodes for the last
/// episode added), one `>=` range read per band at its exact epoch with the page's terms before it, the
/// size again. An empty listing makes no extent read.
#[test]
fn the_reads_are_the_size_the_extent_the_edges_and_the_size_again() {
    let page = terms(&[("type", "2"), ("unwatchedLeaves", "1"), ("genre", "7")]);
    let wire = |pairs: &[(String, String)]| pairs.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    for (family, oldest_year) in FAMILIES {
        let items = items(&edges(family, oldest_year, flat_shift), 90, 4);
        let edges = edges_for(family, &items, &page, flat_shift);
        let server = Server::new(items, field(family), 1);
        let log = RefCell::new(Vec::<String>::new());
        let read = read_bands(Some(family), Some(TODAY), flat_zone(), &page,
            |terms| { log.borrow_mut().push(format!("count {}", wire(terms))); server.count(terms) },
            |terms, field| { log.borrow_mut().push(format!("oldest {field} {}", wire(terms))); Some(oldest_of(family, &server, terms)) });
        assert!(matches!(read, BandsRead::Counted(Some(_))), "{family:?}");
        let (extent_field, extent_terms) = match family {
            BandFamily::LastEpisodeAdded => ("addedAt", "type=4".to_owned()),
            other => (other.field(), wire(&page)),
        };
        let mut expected = vec![format!("count {}", wire(&page)), format!("oldest {extent_field} {extent_terms}")];
        expected.extend(edges.iter().map(|edge| format!("count {}&{}>={edge}", wire(&page), field(family))));
        expected.push(format!("count {}", wire(&page)));
        assert_eq!(*log.borrow(), expected, "{family:?}");

        let empty = Server::new(Vec::new(), field(family), 1);
        let log = RefCell::new(0);
        let read = read_bands(Some(family), Some(TODAY), flat_zone(), &page,
            |terms| { *log.borrow_mut() += 1; empty.count(terms) }, |_, _| panic!("an empty listing has no extent"));
        assert!(matches!(read, BandsRead::Counted(None)));
        assert_eq!(*log.borrow(), 1, "{family:?}: the size and nothing else");
    }
}

/// Everything that leaves a calendar family without a rail, and what is a failure to ask again.
#[test]
fn a_calendar_read_with_no_day_no_value_no_zone_or_no_believable_extent_has_no_rail() {
    let family = BandFamily::DateAdded;
    let edges = edges(family, 2003, flat_shift);
    let server = Server::new(items(&edges, 60, 2), field(family), 1);
    let read = |today: Option<Day>, zone: Zone, oldest: Option<Oldest>| {
        let counted = RefCell::new(0);
        let out = read_bands(Some(family), today, zone, &[], |terms| { *counted.borrow_mut() += 1; server.count(terms) },
            |_, _| oldest);
        (out, counted.into_inner())
    };
    let instant = Some(Oldest::Instant(utc([2003, 1, 1]) + 5 * HOUR));
    assert!(matches!(read(Some(TODAY), flat_zone(), instant), (BandsRead::Counted(Some(_)), _)), "premise");
    // no day: the size and nothing more
    assert!(matches!(read(None, flat_zone(), instant), (BandsRead::Counted(None), 1)));
    // no item has a value in the field (every row of a never-watched library)
    assert!(matches!(read(Some(TODAY), flat_zone(), Some(Oldest::Nothing)), (BandsRead::Counted(None), 1)));
    // the extent read did not answer: ask again, with the back-off
    assert!(matches!(read(Some(TODAY), flat_zone(), None), (BandsRead::Failed, 1)));
    // an instant the zone cannot place
    let unplaceable = Zone { local_date: |_| None, ..flat_zone() };
    assert!(matches!(read(Some(TODAY), unplaceable, instant), (BandsRead::Counted(None), 1)));
    // a zone that cannot convert ONE edge: no rail, not a rail of the other bands
    let no_march = zone_of(|date| (date != [2026, 3, 1]).then(|| utc(date) + 5 * HOUR));
    assert!(matches!(read(Some(TODAY), no_march, instant), (BandsRead::Counted(None), 1)),
        "the size alone is counted: no edge is probed");
    // the extent year is judged: after this year there is no rail, and before 1900 the bands start at
    // 1900 (a probe per year since the year 1 would be absurd): Today, ten months, 1900..=2025, and
    // the size twice
    let local_year = |year: i32| Some(Oldest::Instant(utc([year, 6, 1])));
    assert!(matches!(read(Some(TODAY), flat_zone(), local_year(1970)), (BandsRead::Counted(_), n) if n > 40));
    assert!(matches!(read(Some(TODAY), flat_zone(), local_year(2026)), (BandsRead::Counted(_), n) if n >= 13));
    let absurd = Zone { local_date: |epoch| Some([i32::try_from(epoch).unwrap_or(1), 1, 1]), ..flat_zone() };
    for epoch in [1, 1899] {
        assert!(matches!(read(Some(TODAY), absurd, Some(Oldest::Instant(epoch))), (BandsRead::Counted(_), n) if n == 2 + 1 + 10 + 126), "{epoch}");
    }
    for epoch in [2027, 5000] {
        assert!(matches!(read(Some(TODAY), absurd, Some(Oldest::Instant(epoch))), (BandsRead::Counted(None), 1)), "{epoch}");
    }
    let release = |year: i32| {
        let server = Server::new(Vec::new(), "originallyAvailableAt", 1);
        let counted = RefCell::new(0);
        let items = vec![Item { value: Some(utc([2020, 1, 1])), unwatched: false, genre: 7 }; 3];
        *server.items.borrow_mut() = items;
        let out = read_bands(Some(BandFamily::Release), Some(TODAY), flat_zone(), &[],
            |terms| { *counted.borrow_mut() += 1; server.count(terms) }, |_, _| Some(Oldest::Year(year)));
        (out, counted.into_inner())
    };
    assert!(matches!(release(1899), (BandsRead::Counted(_), n) if n == 2 + 1 + 13), "before 1900 starts at 1900: 2026, the 2020s down to the 1900s");
    assert!(matches!(release(2027), (BandsRead::Counted(None), 1)), "after this year");
    assert!(matches!(release(1900), (_, n) if n > 12), "1900 is believable");
    assert!(matches!(release(2026), (_, 3)), "this year alone is one band: the size, the edge, the size");
}

/// A scan that lands while the calendar is counted fails the answer (asked again), as for the fixed
/// ladders, and a server that ignores the field is a verdict.
#[test]
fn a_calendar_listing_that_moved_fails_and_one_that_ignores_the_field_is_a_verdict() {
    let family = BandFamily::DateAdded;
    let edges = edges(family, 2003, flat_shift);
    let mut server = Server::new(items(&edges, 60, 5), field(family), 1);
    server.scan.set(Some((9, Item { value: Some(edges[0] + 7), unwatched: true, genre: 7 })));
    assert!(matches!(count_read(family, Some(TODAY), flat_zone(), &[], &server), BandsRead::Failed));
    assert!(matches!(count_read(family, Some(TODAY), flat_zone(), &[], &server), BandsRead::Counted(Some(_))),
        "the library holds still now");
    server.ignores_the_field = true;
    assert!(matches!(count_read(family, Some(TODAY), flat_zone(), &[], &server), BandsRead::Counted(None)));
    server.ignores_the_window = true;
    assert!(matches!(count_read(family, Some(TODAY), flat_zone(), &[], &server), BandsRead::Counted(None)));
    let failing = Server::new(items(&edges, 60, 5), field(family), 1);
    failing.fail_from.set(Some(5));
    assert!(matches!(count_read(family, Some(TODAY), flat_zone(), &[], &failing), BandsRead::Failed));
}

fn a_rail(family: BandFamily) -> bands::BandRail {
    let ladder = bands::plan(family, TODAY, 2025);
    let counts: Vec<u32> = (0..ladder.len() as u32).collect();
    bands::BandRail::new(&ladder, &counts, 40).expect("a trusted rail")
}

/// The rail of `family` planned for `today`, 1 to n films in each band.
fn a_rail_on(family: BandFamily, today: bands::Day) -> bands::BandRail {
    let ladder = bands::plan(family, today, 2025);
    let counts: Vec<u32> = (1..=ladder.len() as u32).collect();
    bands::BandRail::new(&ladder, &counts, 400).expect("a trusted rail")
}

/// Midnight re-keys a calendar rail and asks it once more; until that read lands the rail on screen is
/// the previous day's (a focus seated in it must not lose it to a second of nothing), and landing the new
/// day's drops yesterday's instead of keeping it. Another family or other terms never get a stale rail.
#[test]
fn a_day_turn_keeps_the_previous_days_rail_on_screen_until_the_new_days_lands() {
    use crate::browse::test_support::TestBrowse;
    use std::sync::Arc;
    let _g = plx_base::testlock::serial();
    let kind = SecKind::Movie;
    let (_cleanup, mut browse, _sid, _client) = banded_source(kind, &["rating", "addedAt"]);
    assert!(browse.state.set_sort_by_key("addedAt", true));
    let on = |browse: &mut TestBrowse, today| browse.state.state_mut(0).unwrap().today = Some(today);
    on(&mut browse, TODAY);
    let key = browse.state.states()[0].bands_key(BandFamily::DateAdded);
    browse.state.state_mut(0).unwrap().land_bands(key, Some(a_rail_on(BandFamily::DateAdded, TODAY)));
    let yesterday = browse.state.states()[0].shown_bands(kind).expect("the day's rail").clone();
    assert!(!browse.state.states()[0].bands_owed(kind));

    on(&mut browse, [2026, 11, 1]);
    let state = &browse.state.states()[0];
    assert!(state.bands_owed(kind) && state.bands_verdict_for(BandFamily::DateAdded).is_none(), "the new day's bands are asked for");
    assert!(Arc::ptr_eq(state.shown_bands(kind).expect("the rail stays up while they are read"), &yesterday));
    assert!(state.bands_current(kind), "…and it is on screen, so a watch that moves it is noticed");

    // other terms are no rail of the day before: Unwatched, and then another sort
    browse.state.state_mut(0).unwrap().unwatched = true;
    assert!(browse.state.states()[0].shown_bands(kind).is_none(), "counted without Unwatched");
    browse.state.state_mut(0).unwrap().unwatched = false;
    assert!(browse.state.states()[0].shown_bands(kind).is_some());

    // the new day's read lands: its rail is shown, yesterday's is gone and not kept
    let key = browse.state.states()[0].bands_key(BandFamily::DateAdded);
    browse.state.state_mut(0).unwrap().land_bands(key, Some(a_rail_on(BandFamily::DateAdded, [2026, 11, 1])));
    let state = &browse.state.states()[0];
    assert!(!state.bands_owed(kind));
    assert!(!Arc::ptr_eq(state.shown_bands(kind).expect("today's rail"), &yesterday));
    assert!(state.bands_kept.is_none(), "yesterday's list is not kept beside today's");

    // a sort of another family has no use for it either
    assert!(browse.state.set_sort_by_key("rating", true));
    assert!(browse.state.states()[0].shown_bands(kind).is_none(), "the rating's bands are not read yet");

    // A round trip through another sort moves the day's list to the kept slot, and the day turning while
    // it stands there keeps it on screen just the same: sort by Rating (its read lands), back to Date
    // added (nothing is asked, the kept list answers), midnight.
    let key = browse.state.states()[0].bands_key(BandFamily::Rating);
    browse.state.state_mut(0).unwrap().land_bands(key, Some(a_rail(BandFamily::Rating)));
    assert!(browse.state.set_sort_by_key("addedAt", true));
    let state = &browse.state.states()[0];
    assert!(!state.bands_owed(kind), "the kept list answers, nothing is read");
    assert!(state.bands.as_ref().is_some_and(|(key, _)| key.family == Some(BandFamily::Rating)), "the rating's list is the landed one");
    let kept = state.shown_bands(kind).expect("the day's rail, from the kept slot").clone();
    on(&mut browse, [2026, 11, 2]);
    let state = &browse.state.states()[0];
    assert!(state.bands_owed(kind) && state.bands_verdict_for(BandFamily::DateAdded).is_none(), "the new day's bands are asked for");
    assert!(Arc::ptr_eq(state.shown_bands(kind).expect("the rail stays up from the kept slot"), &kept));
    assert!(state.bands_current(kind), "…and it is on screen");

    // the same through an Unwatched round trip: this day's list lands, Unwatched goes on (its list is
    // another terms' and lands, the day's moves to the kept slot) and off again
    let key = browse.state.states()[0].bands_key(BandFamily::DateAdded);
    browse.state.state_mut(0).unwrap().land_bands(key, Some(a_rail_on(BandFamily::DateAdded, [2026, 11, 2])));
    browse.state.state_mut(0).unwrap().unwatched = true;
    let key = browse.state.states()[0].bands_key(BandFamily::DateAdded);
    browse.state.state_mut(0).unwrap().land_bands(key, Some(a_rail_on(BandFamily::DateAdded, [2026, 11, 2])));
    browse.state.state_mut(0).unwrap().unwatched = false;
    let state = &browse.state.states()[0];
    assert!(!state.bands_owed(kind), "the kept list answers, nothing is read");
    let kept = state.shown_bands(kind).expect("the day's rail, from the kept slot").clone();
    on(&mut browse, [2026, 11, 3]);
    let state = &browse.state.states()[0];
    assert!(state.bands_owed(kind));
    assert!(Arc::ptr_eq(state.shown_bands(kind).expect("the rail stays up from the kept slot"), &kept));

    // landing the new day's drops the previous day's list from the kept slot too: it stands beside no
    // list of its own terms (here a stale one is planted next to nothing, which no landing leaves)
    let key = browse.state.states()[0].bands_key(BandFamily::DateAdded);
    let stale = {
        let mut stale = key.clone();
        stale.today = Some([2026, 11, 2]);
        (stale, Some(Arc::new(a_rail_on(BandFamily::DateAdded, [2026, 11, 2]))))
    };
    let state = browse.state.state_mut(0).unwrap();
    state.bands = None;
    state.bands_kept = Some(stale);
    state.land_bands(key, Some(a_rail_on(BandFamily::DateAdded, [2026, 11, 3])));
    assert!(state.bands_kept.is_none(), "a previous day's list is not kept beside this day's");
}

/// The key: a calendar family's carries the day (a day turn asks once more), no other does; the
/// direction is in none; a watch moves the date viewed and no other date.
#[test]
fn a_calendar_key_carries_the_day_and_a_watch_moves_only_the_date_viewed() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = banded_source(SecKind::Movie, &["rating", "addedAt", "lastViewedAt"]);
    use BandFamily::*;
    {
        let state = browse.state.state_mut(0).unwrap();
        state.today = Some(TODAY);
        for family in [DateAdded, DateViewed, LastEpisodeAdded, Release] {
            assert_eq!(state.bands_key(family).today, Some(TODAY), "{family:?}");
        }
        for family in [Rating, Duration, Unplayed, Plays] {
            assert_eq!(state.bands_key(family).today, None, "{family:?}");
        }
        let (added, rating) = (state.bands_key(DateAdded), state.bands_key(Rating));
        state.sort_desc = !state.sort_desc;
        assert_eq!((state.bands_key(DateAdded), state.bands_key(Rating)), (added.clone(), rating.clone()), "no direction");
        state.land_bands(added, Some(a_rail(DateAdded)));
        state.land_bands(rating, Some(a_rail(Rating)));
        assert!(!state.bands_owed_for(DateAdded) && !state.bands_owed_for(Rating), "both on hand");
        // the day turns: the calendar's bands were that day's, the rating's are not
        state.today = Some([2026, 11, 1]);
        assert!(state.bands_verdict_for(DateAdded).is_none() && state.bands_owed_for(DateAdded), "a day turn asks once more");
        assert!(state.bands_verdict_for(Rating).is_some() && !state.bands_owed_for(Rating), "the rating stands");
        state.today = Some(TODAY);
        assert!(state.bands_verdict_for(DateAdded).is_some(), "the same day finds the kept list");
    }
    let before: Vec<u32> = [DateAdded, DateViewed, LastEpisodeAdded, Release]
        .map(|family| browse.state.states()[0].bands_key(family).gen).to_vec();
    browse.state.apply_view_local(&crate::viewstate::ViewAt::leaf(sid, "7"),
        crate::viewstate::ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0, sort: None });
    let after: Vec<u32> = [DateAdded, DateViewed, LastEpisodeAdded, Release]
        .map(|family| browse.state.states()[0].bands_key(family).gen).to_vec();
    assert_eq!(before[0], after[0], "a watch leaves the date added");
    assert_ne!(before[1], after[1], "…and moves the date viewed");
    assert_eq!((before[2], before[3]), (after[2], after[3]), "…and the last episode and the release date");
}

/// The day reaches the key from the Library's ask. A calendar sort is asked for its bands only once the
/// application has handed over the day (`rail_family`), and then its key carries it; a fixed family's
/// key never does.
#[test]
fn the_ask_hands_the_day_to_the_state_and_a_calendar_sort_asks_for_its_bands_once_it_has_it() {
    let _g = plx_base::testlock::serial();
    for family in BandFamily::ALL {
        assert!(family.rail_ready(), "{family:?}: the rail draws every family");
        assert_eq!(family.extent_read().is_some(), family.basis().is_some(), "{family:?}");
    }
    let (_cleanup, mut browse, sid, _client) = banded_source(SecKind::Movie, &["rating", "addedAt"]);
    assert!(browse.state.set_sort_by_key("addedAt", true));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty(), "a calendar sort asks nothing without a day: {:?}", browse.jobs);
    assert!(browse.state.states()[0].rail_family(SecKind::Movie).is_none());
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: Some(TODAY) });
    assert_eq!(browse.state.states()[0].today, Some(TODAY));
    assert_eq!(browse.state.states()[0].rail_family(SecKind::Movie), Some(BandFamily::DateAdded));
    let (job, _port) = the_directory_job(&mut browse);
    assert_eq!(job.req.letters.as_ref().map(|key| (key.family, key.today)), Some((Some(BandFamily::DateAdded), Some(TODAY))),
        "a calendar family's key carries the day");
}

/// Last episode added counts the newest episode of a show, which only a DESCENDING sort orders by
/// (`episode.addedAt:asc` orders by the oldest), so the rail's bands exist for a descending sort only:
/// the screens draw whatever bands the data layer lands, and this is where an ascending one is kept off.
#[test]
fn last_episode_added_asks_for_its_bands_descending_with_the_day_and_never_ascending() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = banded_source(SecKind::Show, &["episode.addedAt"]);
    assert!(browse.state.set_sort_by_key("episode.addedAt", true));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty() && browse.state.states()[0].rail_family(SecKind::Show).is_none(), "no day, no bands");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: Some(TODAY) });
    assert_eq!(browse.state.states()[0].rail_family(SecKind::Show), Some(BandFamily::LastEpisodeAdded));
    let (job, _port) = the_directory_job(&mut browse);
    assert_eq!(job.req.letters.as_ref().map(|key| (key.family, key.today)), Some((Some(BandFamily::LastEpisodeAdded), Some(TODAY))));

    let (_cleanup, mut browse, sid, _client) = banded_source(SecKind::Show, &["episode.addedAt"]);
    assert!(browse.state.set_sort_by_key("episode.addedAt", false));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: Some(TODAY) });
    assert!(browse.state.states()[0].rail_family(SecKind::Show).is_none(), "ascending has no bands");
    assert!(browse.jobs.is_empty(), "…and asks for none: {:?}", browse.jobs);
}

/// …and a fixed family's key does not carry it, whatever day the ask hands over.
#[test]
fn a_fixed_familys_key_does_not_carry_the_day_the_ask_hands_over() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = banded_source(SecKind::Movie, &["rating", "addedAt"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: Some([2026, 11, 1]) });
    let (job, _port) = the_directory_job(&mut browse);
    assert_eq!(job.req.letters.as_ref().map(|key| (key.family, key.today)), Some((Some(BandFamily::Rating), None)),
        "a fixed family's key does not carry the day");
}

/// The production body for a calendar request: `today` comes from the key the step asked with, the
/// answer lands under that key, and the state shows it for that day and no other.
#[test]
fn a_calendar_request_lands_under_the_key_it_was_asked_with() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["addedAt"]);
    browse.state.state_mut(0).unwrap().today = Some(TODAY);
    let family = BandFamily::DateAdded;
    let key = browse.state.states()[0].bands_key(family);
    let edges = edges(family, 2003, flat_shift);
    let server = Server::new(items(&edges, 50, 6), field(family), 1);
    Directory::Bands.claim(&browse.adapter, 1);
    let req = BrowseDirectoryReq::new(sid, plx_plex::plex::ClientKey::of(client), browse.state.table_epoch(), 0,
        browse.state.sections[0].key, LibraryType::Primary, Vec::new(), Some(key), Directory::Bands);
    let port = BrowseDirectoryPort::of(&browse.adapter, Directory::Bands);
    finish_bands_with(&port, listing_addr(1), client, req, flat_zone(), |terms| server.count(terms),
        |terms, _| Some(oldest_of(family, &server, terms)));
    let _ = browse.pump();
    browse.jobs.clear();
    let state = &browse.state.states()[0];
    let rail = state.bands_verdict_for(family).expect("landed").as_ref().expect("a rail").clone();
    assert_eq!(rail.listed(true).bands.len(), edges.len());
    assert!(!Directory::Bands.busy(&browse.adapter));
}

/// An extent year of 1 (a bogus `0001-01-01` release date, an 1895 film) does not take the rail away: the
/// bands start at 1900, the probes are bounded, and the items older than that sit in the unlabelled
/// region at the low end (the lead of an ascending listing, the tail of a descending one) with the
/// items that have no value, each band at the first index of its items.
#[test]
fn an_extent_before_1900_starts_the_release_bands_at_1900_and_leaves_the_older_items_unlabelled() {
    let family = BandFamily::Release;
    let edges = edges(family, 1900, flat_shift);
    let values = [None, None, Some(utc([1, 1, 1])), Some(utc([1850, 6, 1])), Some(utc([1899, 12, 31])), Some(utc([1900, 1, 1])),
        Some(utc([1931, 4, 2])), Some(utc([1969, 12, 31])), Some(utc([1970, 1, 1])), Some(utc([2005, 5, 5])),
        Some(utc([2020, 1, 1])), Some(utc([2026, 3, 3]))];
    let items: Vec<Item> = values.iter().map(|&value| Item { value, unwatched: false, genre: 7 }).collect();
    let server = Server::new(items.clone(), field(family), 1);
    let counted = RefCell::new(0);
    let read = read_bands(Some(family), Some(TODAY), flat_zone(), &[],
        |terms| { *counted.borrow_mut() += 1; server.count(terms) }, |_, _| Some(Oldest::Year(1)));
    let BandsRead::Counted(Some(rail)) = read else { panic!("a rail") };
    assert_eq!(*counted.borrow(), 2 + edges.len(), "the size, one read per band from 1900, the size");
    assert_eq!(edges.len(), 1 + 13, "2026 and the decades 2020s down to 1900s");
    for descending in [true, false] {
        let rows = rows(&edges, &items, &[], descending);
        assert_layout(edges.len(), &rows, rail.listed(descending), descending, "year 1");
        let layout = rail.listed(descending);
        let unlabelled = if descending { layout.tail } else { layout.lead };
        assert_eq!(unlabelled, 2 + 3, "no value, 1, 1850 and 1899 are in no band");
    }
}

/// **The Episodes listing of a show library has date bands** (`BandFamily::of_sort`): sorted by date added or
/// by release date, the step asks for the family's bands with the page's own terms (`type=4`, which every read
/// carries, the extent read included), the production body counts them over the model of the server, the answer
/// lands under the key it was asked with, and the view lays the bands out in the grid's order in both directions.
/// The letters are not this sort's rail.
#[test]
fn an_episodes_listing_counts_its_date_bands_over_type_4() {
    use crate::browse::SortEntry;
    use std::sync::Arc;
    for (family, oldest_year, sort_key) in [
        (BandFamily::DateAdded, 2003, "addedAt"), (BandFamily::Release, 1965, "originallyAvailableAt"),
    ] {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Show, &[]);
        assert!(browse.state.set_library_type(LibraryType::Episodes));
        let entry = |key: &str, default_desc| SortEntry { key: key.into(), desc_key: format!("{key}:desc"), title: key.into(), default_desc };
        let state = browse.state.state_mut(0).unwrap();
        state.sorts = Arc::new(vec![
            entry("show.titleSort,season.index:nullsLast,episode.index:nullsLast,episode.id", false),
            entry(sort_key, true),
        ]);
        let letters = state.letters_key();
        state.land_letters(letters, vec![("A".into(), 1), ("B".into(), 1)]);
        assert!(browse.state.set_sort_by_key(sort_key, true));
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: Some(TODAY) });
        let (job, port) = the_directory_job(&mut browse);
        assert_eq!(job.req.library_type, LibraryType::Episodes);
        assert_eq!(job.req.letters.as_ref().and_then(|key| key.family), Some(family), "{family:?}: the sort's own family");
        let page = vec![("type".to_owned(), "4".to_owned())];
        assert_eq!(job.req.filters, page, "{family:?}: the bands are counted over the Episodes listing");

        let items = items(&edges(family, oldest_year, flat_shift), 120, 7);
        let edges = edges_for(family, &items, &page, flat_shift);
        let server = Server::new(items.clone(), field(family), 1);
        let extents = RefCell::new(Vec::<Vec<(String, String)>>::new());
        finish_bands_with(&port, job.addr, client, job.req, flat_zone(), |terms| server.count(terms),
            |terms, _| { extents.borrow_mut().push(terms.to_vec()); Some(oldest_of(family, &server, terms)) });
        let _ = browse.pump();
        browse.jobs.clear();

        let reads = server.seen.borrow();
        assert_eq!(reads.len(), 2 + edges.len(), "{family:?}: the size, one read per band, the size");
        assert!(reads.iter().all(|read| read[0] == page[0]), "{family:?}: every count carries type=4: {reads:?}");
        assert_eq!(*extents.borrow(), vec![page.clone()], "{family:?}: one extent read, over the same listing");
        let snapshot = browse.state.listing_snapshot();
        assert!(snapshot.view().bands_available() && !snapshot.view().rail_available(), "{family:?}: bands, not letters");
        for descending in [true, false] {
            assert!(browse.state.set_sort_by_key(sort_key, descending));
            let snapshot = browse.state.listing_snapshot();
            let shown = snapshot.view().bands().expect("the counted bands, in this direction");
            let rows = rows(&edges, &items, &page, descending);
            assert_layout(edges.len(), &rows, shown, descending, &format!("{family:?} descending {descending}"));
        }
    }
}
