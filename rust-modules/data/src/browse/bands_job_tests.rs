//! The value sorts' rail, end to end on the data side: the bands read asked on the rail's tick with
//! the page's own terms, its counts turned into starts that match the listing the server would
//! sort, and the key that decides when they are shown, kept, asked again or backed off.
//!
//! The server is a MODEL ([`Server`]) that answers a count-only read the way the measured rules say
//! (`docs/pms-api.md`, "Count probes at band edges") over a generated list: `>>=` strictly greater,
//! `>=` inclusive, no-value items left out of every range, and the unfiltered size for a read with no
//! range. It is passed to the body as its `probe` parameter, the seam the production body takes.

use super::tests::{directory_refusal_pending, the_directory_job, tick_directory, tick_directory_without_keys};
use super::*;
use crate::browse::bands::Offsets;
use crate::browse::test_support::*;
use crate::browse::{SecKind, SortEntry};
use crate::stores::browse::LibraryWork;
use crate::stores::StoreJob;
use crate::viewstate::{ViewAt, ViewEdit};
use plx_plex::plex::SectionTotal;
use std::cell::{Cell, RefCell};

const WATCHED_FACTS: ViewEdit = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0, sort: None };

// ---- the server model --------------------------------------------------------------------------

#[derive(Clone, Copy, Debug)]
pub(super) struct Item {
    /// The value in the sort's field, in the wire's smallest unit; `None` = no value at all.
    pub(super) value: Option<i64>,
    pub(super) unwatched: bool,
    pub(super) genre: u8,
}

/// A server holding `items`, answering count reads, and recording every read it was asked.
pub(super) struct Server {
    pub(super) items: RefCell<Vec<Item>>,
    /// The sort field the model answers a range on.
    field: &'static str,
    /// Wire units in one whole of the probe's decimal (`100` for a rating written `9.49`).
    scale: i64,
    pub(super) seen: RefCell<Vec<Vec<(String, String)>>>,
    /// Answer no read from this one on (0-based): a server that stops answering.
    pub(super) fail_from: Cell<Option<usize>>,
    /// Answer every range read with the unfiltered size: a server that ignores the field.
    pub(super) ignores_the_field: bool,
    /// Answer every read with rows and no count: a server that ignores the `Size=0` window.
    pub(super) ignores_the_window: bool,
    /// A library scan landing: this item joins the library just before the read at this index.
    pub(super) scan: Cell<Option<(usize, Item)>>,
}

impl Server {
    pub(super) fn new(items: Vec<Item>, field: &'static str, scale: i64) -> Self {
        Self {
            items: RefCell::new(items), field, scale, seen: RefCell::default(), fail_from: Cell::new(None),
            ignores_the_field: false, ignores_the_window: false, scan: Cell::new(None),
        }
    }

    pub(super) fn passes(item: &Item, filters: &[(String, String)]) -> bool {
        filters.iter().all(|(key, value)| match key.as_str() {
            "unwatched" | "unwatchedLeaves" => value != "1" || item.unwatched,
            "genre" => value.parse::<u8>().is_ok_and(|genre| genre == item.genre),
            "type" => true,
            other => panic!("the model knows no filter {other}"),
        })
    }

    /// A decimal as the wire spells it, in wire units: `9.49` at scale 100 is 949. Panics on a
    /// spelling finer than the scale, which a real server would round and the model must not.
    fn units(&self, text: &str) -> i64 {
        let digits = self.scale.ilog10() as usize;
        let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
        assert!(fraction.len() <= digits, "{text} is finer than the model's scale");
        let fraction = format!("{fraction:0<digits$}");
        whole.parse::<i64>().unwrap() * self.scale + if digits == 0 { 0 } else { fraction.parse::<i64>().unwrap() }
    }

    pub(super) fn count(&self, terms: &[(String, String)]) -> Option<SectionTotal> {
        let read = self.seen.borrow().len();
        self.seen.borrow_mut().push(terms.to_vec());
        if let Some((at, item)) = self.scan.get() {
            if read == at {
                self.items.borrow_mut().push(item);
                self.scan.set(None);
            }
        }
        if self.fail_from.get().is_some_and(|from| read >= from) {
            return None;
        }
        if self.ignores_the_window {
            return Some(SectionTotal::Ignored);
        }
        self.matching(terms).map(SectionTotal::Count)
    }

    fn matching(&self, terms: &[(String, String)]) -> Option<i64> {
        let (ranges, filters): (Vec<_>, Vec<_>) = terms.iter().cloned().partition(|(key, _)| key.contains('>'));
        let items = self.items.borrow();
        let rows = items.iter().filter(|item| Self::passes(item, &filters));
        match ranges.as_slice() {
            [] => Some(rows.count() as i64),
            _ if self.ignores_the_field => Some(rows.count() as i64),
            [(key, value)] => {
                let (field, op) = key.split_at(key.find('>').unwrap());
                assert_eq!(field, self.field, "the probe is on the sort's own field");
                let edge = self.units(value);
                let count = rows.filter(|item| match item.value {
                    Some(v) if op == ">>" => v > edge,
                    Some(v) if op == ">" => v >= edge,
                    Some(_) => panic!("the model knows no operator {op}"),
                    None => false,
                });
                Some(count.count() as i64)
            }
            more => panic!("one range per read, saw {more:?}"),
        }
    }
}

// ---- the ladders, written out independently of the model under test ------------------------------

struct Ladder {
    family: BandFamily,
    field: &'static str,
    scale: i64,
    /// Where each band starts, in wire units, highest first.
    edges: Vec<i64>,
    /// Is there a last band for everything above zero (`<75%`, `<1h`)?
    floor: bool,
    /// Values the generated list draws from: the edges and one unit either side of each.
    palette: Vec<Option<i64>>,
}

impl Ladder {
    fn new(family: BandFamily, field: &'static str, scale: i64, edges: Vec<i64>, floor: bool) -> Self {
        let mut palette = vec![None, Some(0), Some(1)];
        for &edge in &edges {
            palette.extend([Some(edge - 1), Some(edge), Some(edge + 1)]);
        }
        palette.push(Some(edges[0] * 2));
        Self { family, field, scale, edges, floor, palette }
    }

    fn rating() -> Self {
        Self::new(BandFamily::Rating, "rating", 100, vec![950, 900, 850, 800, 750], true)
    }

    fn duration() -> Self {
        let minutes = [180, 150, 120, 90, 60];
        Self::new(BandFamily::Duration, "duration", 1, minutes.iter().map(|m| m * 60_000).collect(), true)
    }

    fn unplayed() -> Self {
        Self::new(BandFamily::Unplayed, "unviewedLeafCount", 1, vec![12, 10, 8, 6, 4, 2, 1], false)
    }

    fn plays() -> Self {
        Self::new(BandFamily::Plays, "viewCount", 1, vec![10, 5, 3, 2, 1], false)
    }

    fn bands(&self) -> usize {
        self.edges.len() + usize::from(self.floor)
    }

    /// The band (0 = highest) a value is in, or `None` for the unlabelled: no value, or a stored zero.
    fn band_of(&self, value: Option<i64>) -> Option<usize> {
        let v = value.filter(|&v| v > 0)?;
        match self.edges.iter().position(|&edge| v >= edge) {
            Some(band) => Some(band),
            None => self.floor.then_some(self.edges.len()),
        }
    }

    /// A list of `len` items drawn from the palette (deterministic), a third of them unwatched and
    /// split between two genres.
    fn items(&self, len: usize, seed: u64) -> Vec<Item> {
        let mut state = seed;
        let mut next = move || {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (state >> 33) as usize
        };
        (0..len)
            .map(|_| Item {
                value: self.palette[next() % self.palette.len()],
                unwatched: next() % 3 == 0,
                genre: if next() % 2 == 0 { 7 } else { 9 },
            })
            .collect()
    }

    fn server(&self, items: Vec<Item>) -> Server {
        Server::new(items, self.field, self.scale)
    }
}

/// A bands read of a family with fixed edges: no day, no extent.
fn read_fixed(family: BandFamily, filters: &[(String, String)],
    probe: impl Fn(&[(String, String)]) -> Option<SectionTotal>) -> BandsRead {
    read_bands(Some(family), None, bands::utc_zone(), filters, probe, |_, _| None)
}

/// [`finish_bands_with`] for a family with fixed edges.
fn finish_fixed(port: &BrowseDirectoryPort, addr: Addr, client: &'static plx_plex::plex::Client,
    req: BrowseDirectoryReq, probe: impl Fn(&[(String, String)]) -> Option<SectionTotal>) {
    finish_bands_with(port, addr, client, req, bands::utc_zone(), probe, |_, _| None);
}

pub(super) fn terms(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

/// What the grid shows for `filters` sorted by value, as the measured server orders it: no value
/// FIRST in `:asc` and LAST in `:desc`, a stored zero beside them on the value side.
fn listing(ladder: &Ladder, items: &[Item], filters: &[(String, String)], descending: bool) -> Vec<Option<usize>> {
    let mut rows: Vec<Option<i64>> =
        items.iter().filter(|item| Server::passes(item, filters)).map(|item| item.value).collect();
    rows.sort_by_key(|value| value.map_or(i64::MIN, |v| v));
    if descending {
        rows.reverse();
    }
    rows.into_iter().map(|value| ladder.band_of(value)).collect()
}

/// The rail must put every populated band at the first index of its band in `rows`, with its size,
/// and the unlabelled items where they sit.
#[track_caller]
fn assert_layout(ladder: &Ladder, rows: &[Option<usize>], layout: &Offsets, descending: bool, what: &str) {
    assert_eq!(layout.bands.len(), ladder.bands(), "{what}");
    // the bands appear in the listing's order: highest first when descending
    let order: Vec<usize> = if descending { (0..ladder.bands()).collect() } else { (0..ladder.bands()).rev().collect() };
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
    assert_eq!((layout.lead, layout.tail), (lead, tail), "{what}: the unlabelled items");
    assert_eq!(layout.lead + layout.bands.iter().map(|b| b.size).sum::<u32>() + layout.tail, rows.len() as u32, "{what}");
}

fn rail_of(family: BandFamily, filters: &[(String, String)], server: &Server) -> Option<BandRail> {
    match read_fixed(family, filters, |terms| server.count(terms)) {
        BandsRead::Counted(rail) => rail,
        BandsRead::Failed => panic!("the model answers every read"),
    }
}

// ---- counts to starts ---------------------------------------------------------------------------

/// The acceptance: for every fixed ladder, every filter combination and both directions, the starts
/// the counts give are the first index of each band in the list the server would sort, the sizes are
/// the band's, and the unlabelled items (no value, stored zeros) are the lead or the tail. The
/// generated lists put values on each edge and a unit either side, so an off-by-one edge, a wrong
/// operator, a total taken from the wrong listing or a flipped direction fails here.
#[test]
fn counts_become_the_starts_of_the_bands_for_every_ladder_filter_and_direction() {
    let filters_cases: [Vec<(String, String)>; 4] = [
        Vec::new(),
        terms(&[("unwatched", "1")]),
        terms(&[("genre", "7")]),
        terms(&[("type", "2"), ("unwatchedLeaves", "1"), ("genre", "9")]),
    ];
    for ladder in [Ladder::rating(), Ladder::duration(), Ladder::unplayed(), Ladder::plays()] {
        for seed in [1, 2, 3] {
            let items = ladder.items(90, seed);
            for filters in &filters_cases {
                let what = format!("{:?} seed {seed} {filters:?}", ladder.family);
                let server = ladder.server(items.clone());
                let rail = rail_of(ladder.family, filters, &server).unwrap_or_else(|| panic!("{what}: a rail"));
                for descending in [true, false] {
                    let rows = listing(&ladder, &items, filters, descending);
                    assert_layout(&ladder, &rows, rail.listed(descending), descending, &what);
                }
            }
        }
    }
}

/// Every read carries the grid's terms, in the page's order: the first is the listing alone (its
/// size), each other is those terms and ONE range term on the sort's own field, spelled the way the
/// measured server reads it.
#[test]
fn every_read_carries_the_grids_terms_and_one_range_on_the_sorts_field() {
    let filters = terms(&[("type", "2"), ("unwatchedLeaves", "1"), ("genre", "7")]);
    for (ladder, spelled) in [
        (Ladder::rating(), ["rating>>=9.49", "rating>>=8.99", "rating>>=8.49", "rating>>=7.99", "rating>>=7.49", "rating>>=0"]),
        (Ladder::duration(), ["duration>>=10799999", "duration>>=8999999", "duration>>=7199999",
            "duration>>=5399999", "duration>>=3599999", "duration>>=0"]),
    ] {
        let server = ladder.server(ladder.items(40, 5));
        rail_of(ladder.family, &filters, &server).expect("a rail");
        let seen = server.seen.borrow();
        assert_eq!(seen.len(), 2 + ladder.bands(), "the size, one read per band, the size again");
        assert_eq!(seen[0], filters, "the size is the listing's own");
        assert_eq!(seen[seen.len() - 1], filters, "…and so is the check at the end");
        for (read, spelled) in seen[1..=ladder.bands()].iter().zip(spelled) {
            assert_eq!(read[..filters.len()], filters[..], "the page's terms, in the page's order");
            assert_eq!(read.len(), filters.len() + 1);
            let (key, value) = read.last().unwrap();
            assert_eq!(format!("{key}={value}"), spelled);
        }
    }
}

/// The edge case the ladders are built around: only the rating tail is probed at zero, so a stored
/// 0.0 joins the unlabelled items instead of the `<75%` band, and a no-value item never counts.
#[test]
fn a_stored_zero_and_a_missing_value_are_unlabelled_not_the_lowest_band() {
    let ladder = Ladder::rating();
    let items = [None, None, Some(0), Some(0), Some(100), Some(760), Some(960)]
        .map(|value| Item { value, unwatched: true, genre: 7 }).to_vec();
    let rail = rail_of(ladder.family, &[], &ladder.server(items)).expect("a rail");
    let desc = rail.listed(true);
    assert_eq!((desc.lead, desc.tail), (0, 4));
    assert_eq!(desc.bands.iter().map(|b| b.size).collect::<Vec<_>>(), [1, 0, 0, 0, 1, 1]);
    let asc = rail.listed(false);
    assert_eq!((asc.lead, asc.tail), (4, 0));
    assert_eq!(asc.bands.iter().map(|b| (b.start, b.size)).collect::<Vec<_>>(),
        [(4, 1), (5, 1), (6, 0), (6, 0), (6, 0), (6, 1)]);
}

// ---- failing closed -----------------------------------------------------------------------------

/// A read that does not answer fails the whole answer, wherever in the sequence it falls: a rail
/// built from some of its counts would point at the wrong rows.
#[test]
fn one_read_that_fails_fails_the_answer_wherever_it_falls() {
    let ladder = Ladder::rating();
    for from in 0..2 + ladder.bands() {
        let server = ladder.server(ladder.items(30, 7));
        server.fail_from.set(Some(from));
        assert!(matches!(read_fixed(ladder.family, &[], |t| server.count(t)), BandsRead::Failed),
            "reads from the {from}th fail");
    }
    // …and so does a body that panics
    assert!(matches!(
        catch_unwind(AssertUnwindSafe(|| read_fixed(BandFamily::Rating, &[], |_| panic!("the body panicked"))))
            .unwrap_or(BandsRead::Failed),
        BandsRead::Failed));
}

/// Counts that cannot be trusted give no rail and are NOT a failure while the listing held still: a
/// server that ignores the field answers the whole listing to every probe; a count that rises as the
/// edge falls is some other list; a server that answers rows to a count-only read does so every time.
#[test]
fn counts_that_are_not_trusted_give_no_rail_and_are_not_retried() {
    let ladder = Ladder::rating();
    let mut ignoring = ladder.server(ladder.items(30, 9));
    ignoring.ignores_the_field = true;
    assert!(rail_of(ladder.family, &[], &ignoring).is_none(), "every count is the total: the filter was ignored");
    assert_eq!(ignoring.seen.borrow().len(), 2 + ladder.bands(), "…read in full, and verified");

    let calls = Cell::new(0);
    let falling = |terms: &[(String, String)]| {
        calls.set(calls.get() + 1);
        Some(SectionTotal::Count(if terms.is_empty() { 30 } else { 20 - 2 * calls.get() as i64 }))
    };
    assert!(matches!(read_fixed(BandFamily::Rating, &[], falling), BandsRead::Counted(None)),
        "counts that fall as the edge falls, in a listing that did not move");

    let empty = ladder.server(Vec::new());
    assert!(matches!(read_fixed(ladder.family, &[], |t| empty.count(t)), BandsRead::Counted(None)));
    assert_eq!(empty.seen.borrow().len(), 1, "an empty listing is not read band by band");

    // rows to a count-only read: the window is ignored, and will be again. One read, a verdict.
    let mut rows = ladder.server(ladder.items(30, 9));
    rows.ignores_the_window = true;
    assert!(matches!(read_fixed(ladder.family, &[], |t| rows.count(t)), BandsRead::Counted(None)));
    assert_eq!(rows.seen.borrow().len(), 1);

    // a size that is no count
    let count = |n: i64| move |_: &[(String, String)]| Some(SectionTotal::Count(n));
    assert!(matches!(read_fixed(BandFamily::Rating, &[], count(-1)), BandsRead::Failed));
    assert!(matches!(read_fixed(BandFamily::Rating, &[], count(i64::from(u32::MAX) + 1)), BandsRead::Failed));
}

/// A library scan lands between the sequential reads: counts above the total, or falling as the edge
/// falls, are then the scan's doing and not the server's word. The size is read again at the end, and
/// a size that moved fails the answer (the back-off asks again) instead of leaving a rail-less verdict
/// that nothing would ever revisit (a server one does not own has no feed to move the key).
#[test]
fn a_scan_landing_between_the_reads_fails_the_answer_and_a_clean_read_after_it_succeeds() {
    let _g = plx_base::testlock::serial();
    let ladder = Ladder::rating();
    // ten rated items; one more rated item arrives while the bands are counted (the 4th read), so the
    // lowest edge counts eleven of a listing the size read said had ten
    let rated = |value| Item { value: Some(value), unwatched: true, genre: 7 };
    let items: Vec<Item> = [960, 920, 870, 810, 770, 760, 700, 650, 600, 500].map(rated).to_vec();
    let server = ladder.server(items);
    server.scan.set(Some((4, rated(990))));
    assert!(matches!(read_fixed(ladder.family, &[], |t| server.count(t)), BandsRead::Failed),
        "the size moved from 10 to 11 under the counts");
    assert_eq!(server.seen.borrow().len(), 2 + ladder.bands(), "every read was made, the last one is the check");
    // …asked again (the library holds still now), the same server gives a rail
    let rail = rail_of(ladder.family, &[], &server).expect("a clean read");
    assert_eq!(rail.listed(true).bands.iter().map(|b| b.size).sum::<u32>() + rail.listed(true).tail, 11);

    // the same through the step: a failure with the back-off armed, then a clean read after it
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let items: Vec<Item> = [960, 920, 870, 810, 770, 760, 700, 650, 600, 500].map(rated).to_vec();
    let server = ladder.server(items);
    server.scan.set(Some((4, rated(990))));
    ask_bands(&mut browse, sid, client, &server);
    assert!(!bands_shown(&browse));
    assert!(browse.state.states()[0].bands_waiting(), "a failure: the back-off is armed");
    no_job_is_owed(&mut browse, sid, "inside the back-off");
    for _ in 0..crate::browse::letters::LETTERS_RETRY_STEPS {
        let _ = browse.pump();
        browse.jobs.clear();
    }
    ask_bands(&mut browse, sid, client, &server);
    assert!(bands_shown(&browse), "asked again after the back-off, over a library that held still");
    assert!(!browse.state.states()[0].bands_waiting());
}

// ---- the step, the key, the landing and the view -------------------------------------------------

fn sort(key: &str, default_desc: bool) -> SortEntry {
    SortEntry { key: key.into(), desc_key: String::new(), title: key.into(), default_desc }
}

/// A source of `kind` with the sorts a server lists and the title letters already landed, so the
/// only job a tick leaves is the bands'. Ordered by title, ascending.
pub(super) fn banded_source(kind: SecKind, keys: &[&str])
    -> (RegisteredCleanup, TestBrowse, plx_plex::plex::ServerId, &'static plx_plex::plex::Client) {
    let (cleanup, mut browse, sid, client) = loopback_source_at(9, kind);
    let state = browse.state.state_mut(0).unwrap();
    state.sorts = Arc::new(std::iter::once("titleSort").chain(keys.iter().copied()).map(|key| sort(key, key != "titleSort")).collect());
    let key = state.letters_key();
    state.land_letters(key, vec![("A".into(), 1), ("B".into(), 1)]);
    (cleanup, browse, sid, client)
}

/// The title letters of the listing as it is now, landed: an edit of the terms owes them again, and a
/// test about the bands wants the bands' job to be the only one a tick leaves.
fn land_title_letters(browse: &mut TestBrowse) {
    let state = browse.state.state_mut(0).unwrap();
    let key = state.letters_key();
    state.land_letters(key, vec![("A".into(), 1), ("B".into(), 1)]);
}

/// A genre menu with Drama (7), so a genre can be chosen.
fn a_genre_menu(browse: &mut TestBrowse) {
    browse.state.state_mut(0).unwrap().genres = Arc::new(vec![crate::browse::GenreEntry { id: "7".into(), title: "Drama".into() }]);
}

/// The bands job the rail's tick leaves (the only one), taken off the outbox.
#[track_caller]
fn bands_job(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId) -> (plx_machine::job::Job<BrowseDirectoryReq>, BrowseDirectoryPort) {
    tick_directory(browse, sid, LibraryWork::Letters { today: None });
    let (job, port) = the_directory_job(browse);
    assert_eq!(job.req.which, Directory::Bands, "the value sort's tick asks for its bands");
    (job, port)
}

/// Tick, run the production body over `server`, land the answer.
#[track_caller]
fn ask_bands(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId, client: &'static plx_plex::plex::Client, server: &Server) {
    let (job, port) = bands_job(browse, sid);
    finish_fixed(&port, job.addr, client, job.req, |terms| server.count(terms));
    let _ = browse.pump();
    browse.jobs.clear();
}

fn snapshot_bands(browse: &TestBrowse) -> Option<Offsets> {
    browse.state.listing_snapshot().view().bands().cloned()
}

/// Is a rail of bands offered for the sort the grid is ordered by?
fn bands_shown(browse: &TestBrowse) -> bool {
    browse.state.listing_snapshot().view().bands_available()
}

/// Is the letters rail offered (the title sort's, which the screen draws from `letters()`)?
fn letters_shown(browse: &TestBrowse) -> bool {
    browse.state.listing_snapshot().view().rail_available()
}

fn no_job_is_owed(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId, what: &str) {
    tick_directory(browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty(), "{what}: nothing is owed, saw {:?}", browse.jobs);
}

/// The whole path on a rating sort: the tick asks with the page's own terms, the answer lands, the
/// view lays the bands out in the listing's order, and the rail is offered.
#[test]
fn a_value_sort_asks_with_the_pages_terms_and_the_view_lays_the_bands_out() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating", "duration"]);
    assert!(browse.state.set_unwatched(true));
    land_title_letters(&mut browse);
    assert!(browse.state.set_sort_by_key("rating", true));
    assert!(!bands_shown(&browse), "premise: nothing counted yet");

    let ladder = Ladder::rating();
    let items = ladder.items(80, 11);
    let server = ladder.server(items.clone());
    let (job, port) = bands_job(&mut browse, sid);
    // the page's own terms: the page job of this very state sends the same
    browse.maybe_spawn();
    let page_terms = match browse.jobs.remove(0) {
        StoreJob::BrowsePage(page, _) => page.req.filters,
        other => panic!("a page job, not {other:?}"),
    };
    assert_eq!(job.req.filters, page_terms, "the bands are counted over the listing the page asks for");
    assert_eq!(job.req.letters.as_ref().and_then(|key| key.family), Some(BandFamily::Rating));
    finish_fixed(&port, job.addr, client, job.req, |terms| server.count(terms));
    for read in server.seen.borrow().iter() {
        assert_eq!(read[..page_terms.len()], page_terms[..], "every read carries them");
    }
    let _ = browse.pump();
    browse.jobs.clear();

    let filters = terms(&[("unwatched", "1")]);
    assert_eq!(page_terms, filters, "premise");
    let shown = snapshot_bands(&browse).expect("the bands of the sort the grid is ordered by");
    assert_layout(&ladder, &listing(&ladder, &items, &filters, true), &shown, true, "descending");
    assert!(bands_shown(&browse));
    assert!(!letters_shown(&browse), "the letters are landed too, and are not this sort's rail");

    // the direction is not part of what was counted: a flip lays the same counts out the other way
    assert!(browse.state.set_sort_by_key("rating", false));
    no_job_is_owed(&mut browse, sid, "a flip of the direction");
    let shown = snapshot_bands(&browse).expect("still counted");
    assert_layout(&ladder, &listing(&ladder, &items, &filters, false), &shown, false, "ascending");
    assert!(bands_shown(&browse));
}

/// The letters rail is the title sort's alone: the screen draws `letters()` for whatever
/// `rail_available` allows, and the letters are landed on a value sort too (their key has no sort), so
/// a jump from one would land on an arbitrary row. A value sort offers its bands, never the letters.
#[test]
fn the_letters_rail_is_offered_for_the_title_sort_and_never_for_a_value_sort() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating", "addedAt"]);
    assert!(letters_shown(&browse), "the title sort, ascending, with two letters: as before");
    assert!(!bands_shown(&browse));
    assert!(browse.state.set_sort_by_key("titleSort", true));
    assert!(!letters_shown(&browse), "a descending title sort has none");
    assert!(browse.state.set_sort_by_key("titleSort", false));

    let ladder = Ladder::rating();
    assert!(browse.state.set_sort_by_key("rating", true));
    assert_eq!(browse.state.listing_snapshot().view().letters().len(), 2, "premise: letters are on hand");
    assert!(!letters_shown(&browse), "a value sort without its bands counted");
    ask_bands(&mut browse, sid, client, &ladder.server(ladder.items(60, 3)));
    assert!(bands_shown(&browse), "premise: the bands are offered");
    assert!(!letters_shown(&browse), "…and the letters still are not, whatever the bands");
    assert_eq!(browse.state.listing_snapshot().view().letters().len(), 2, "…though they are on hand");
    assert!(browse.state.set_sort_by_key("rating", false));
    assert!(!letters_shown(&browse) && bands_shown(&browse), "ascending too");

    // a calendar sort whose ask has handed no day to count by (`today: None`) has neither
    assert!(browse.state.set_sort_by_key("addedAt", true));
    assert!(!letters_shown(&browse) && !bands_shown(&browse));
    assert_eq!(letters_shown(&browse), browse.state.rail_available());
    assert_eq!(bands_shown(&browse), browse.state.bands_available());
}

/// A sort of another family counts again; the title sort shows no bands and asks for none.
#[test]
fn a_sort_change_asks_again_and_a_title_sort_asks_for_no_bands() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating", "duration"]);
    let rating = Ladder::rating();
    assert!(browse.state.set_sort_by_key("rating", true));
    ask_bands(&mut browse, sid, client, &rating.server(rating.items(60, 3)));
    assert!(bands_shown(&browse));

    let duration = Ladder::duration();
    assert!(browse.state.set_sort_by_key("duration", true));
    assert!(snapshot_bands(&browse).is_none(), "the rating's counts are not the duration's");
    assert!(!bands_shown(&browse));
    let (job, _port) = bands_job(&mut browse, sid);
    assert_eq!(job.req.letters.as_ref().and_then(|key| key.family), Some(BandFamily::Duration));
    browse.jobs.clear();

    // back to the title sort: the letters (landed) are the rail, no bands read is owed
    assert!(browse.state.set_sort_by_key("titleSort", false));
    assert!(snapshot_bands(&browse).is_none());
    assert!(letters_shown(&browse), "the letters of the title sort");
    assert!(!bands_shown(&browse), "…and no bands beside them");
    browse.adapter.bands.clear();
    no_job_is_owed(&mut browse, sid, "a title sort");
    let _ = duration;
}

/// The title sort's own tick is the letters' alone, as it was: one read, of the letters.
#[test]
fn a_title_sort_still_asks_for_the_letters_and_only_the_letters() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_source_at(9, SecKind::Movie);
    browse.state.state_mut(0).unwrap().sorts = Arc::new(vec![sort("titleSort", false), sort("rating", true)]);
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    let (job, _port) = the_directory_job(&mut browse);
    assert_eq!(job.req.which, Directory::Letters);
    assert!(job.req.letters.as_ref().is_some_and(|key| key.family.is_none()));
}

/// A calendar sort asks nothing and shows nothing while the ask has handed it no day (`no_job_is_owed`
/// ticks with `Letters { today: None }`): no read, no bands, no rail. With the day it asks, which
/// `bands_calendar_tests` drives.
#[test]
fn a_calendar_sort_asks_nothing_and_shows_nothing_without_a_day() {
    let _g = plx_base::testlock::serial();
    for (kind, key, desc) in [
        (SecKind::Movie, "addedAt", true), (SecKind::Movie, "addedAt", false), (SecKind::Movie, "lastViewedAt", true),
        (SecKind::Movie, "originallyAvailableAt", true), (SecKind::Show, "episode.addedAt", true),
        (SecKind::Show, "originallyAvailableAt", false),
    ] {
        let (_cleanup, mut browse, sid, _client) = banded_source(kind, &[key]);
        assert!(browse.state.set_sort_by_key(key, desc));
        no_job_is_owed(&mut browse, sid, key);
        assert!(snapshot_bands(&browse).is_none(), "{key}");
        assert!(!bands_shown(&browse), "{key}");
    }
}

/// A listing whose items sit in one band has nothing to jump between; no rail.
#[test]
fn a_rail_with_fewer_than_two_populated_bands_is_not_offered() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let ladder = Ladder::rating();
    let items = [Some(960), Some(970), Some(990), None, None].map(|value| Item { value, unwatched: true, genre: 7 }).to_vec();
    ask_bands(&mut browse, sid, client, &ladder.server(items));
    let shown = snapshot_bands(&browse).expect("the counts are trustworthy");
    assert_eq!(shown.populated(), 1, "premise: every rated item is in the top band");
    assert!(!bands_shown(&browse), "one band is not a rail");
}

/// Counts that cannot be trusted leave the rail hidden and the claim down, and nothing is asked
/// again until the key moves.
#[test]
fn untrusted_counts_hide_the_rail_without_a_retry() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let ladder = Ladder::rating();
    let mut server = ladder.server(ladder.items(40, 4));
    server.ignores_the_field = true;
    ask_bands(&mut browse, sid, client, &server);
    assert!(snapshot_bands(&browse).is_none());
    assert!(!bands_shown(&browse));
    assert!(!Directory::Bands.busy(&browse.adapter));
    assert!(!browse.state.states()[0].bands_waiting(), "a verdict, not a failure: no back-off");
    for _ in 0..3 {
        let _ = browse.pump();
        browse.jobs.clear();
        no_job_is_owed(&mut browse, sid, "the same key");
    }
    // a moved key asks again
    assert!(browse.state.set_unwatched(true));
    land_title_letters(&mut browse);
    let _ = bands_job(&mut browse, sid);
}

/// A read that did not answer backs off as the letters do: nothing is shown, the same key waits
/// [`LETTERS_RETRY_STEPS`] steps, and an edit of the terms asks at once.
#[test]
fn a_failed_read_backs_off_like_the_letters() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let ladder = Ladder::rating();
    let server = ladder.server(ladder.items(40, 4));
    server.fail_from.set(Some(2));
    ask_bands(&mut browse, sid, client, &server);
    assert!(!bands_shown(&browse));
    assert!(browse.state.states()[0].bands_waiting(), "the back-off is armed");
    no_job_is_owed(&mut browse, sid, "inside the back-off");

    for step in 0..crate::browse::letters::LETTERS_RETRY_STEPS - 1 {
        let _ = browse.pump();
        browse.jobs.clear();
        assert!(!Directory::Bands.busy(&browse.adapter));
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        assert!(browse.jobs.is_empty(), "step {step}: still backing off");
    }
    let _ = browse.pump();
    browse.jobs.clear();
    // the back-off is spent: the same key is asked again (and fails again)
    ask_bands(&mut browse, sid, client, &server);
    assert!(browse.state.states()[0].bands_waiting(), "…which arms it again");

    // an edit of the terms is asked at once, back-off or not
    assert!(browse.state.set_unwatched(true));
    land_title_letters(&mut browse);
    let _ = bands_job(&mut browse, sid);
}

/// An answer asked under other terms than the grid now has is never shown.
#[test]
fn an_answer_asked_under_older_terms_is_not_shown() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let ladder = Ladder::rating();
    let server = ladder.server(ladder.items(60, 2));
    let (job, port) = bands_job(&mut browse, sid);
    assert!(browse.state.set_unwatched(true), "the viewer narrows the listing while the read is out");
    finish_fixed(&port, job.addr, client, job.req, |terms| server.count(terms));
    let _ = browse.pump();
    browse.jobs.clear();
    assert!(snapshot_bands(&browse).is_none(), "counts of the unfiltered listing over a filtered grid");
    assert!(!bands_shown(&browse));
    land_title_letters(&mut browse);
    let (job, _port) = bands_job(&mut browse, sid);
    assert_eq!(job.req.filters, terms(&[("unwatched", "1")]), "and asked again with the new terms");
}

/// The one kept slot: turning Unwatched on and off again finds the unfiltered counts where they were.
#[test]
fn the_list_a_filter_replaced_is_shown_again_when_the_filter_goes() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let ladder = Ladder::rating();
    let items = ladder.items(60, 6);
    ask_bands(&mut browse, sid, client, &ladder.server(items.clone()));
    let unfiltered = snapshot_bands(&browse).expect("counted");
    a_genre_menu(&mut browse);
    assert!(browse.state.set_genre_by_id(Some("7")));
    land_title_letters(&mut browse);
    ask_bands(&mut browse, sid, client, &ladder.server(items));
    assert_ne!(snapshot_bands(&browse), Some(unfiltered.clone()), "the genre's counts");
    assert!(browse.state.set_genre_by_id(None));
    land_title_letters(&mut browse);
    no_job_is_owed(&mut browse, sid, "the unfiltered list is kept");
    assert_eq!(snapshot_bands(&browse), Some(unfiltered));
}

/// A watch moves the plays and the unplayed counts and nothing else's: the rating's bands stand, the
/// plays' are owed again.
#[test]
fn a_watch_stales_the_counts_that_move_with_it_and_only_those() {
    let _g = plx_base::testlock::serial();
    for (key, ladder, moved) in [("rating", Ladder::rating(), false), ("duration", Ladder::duration(), false),
        ("viewCount", Ladder::plays(), true)] {
        let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &[key]);
        assert!(browse.state.set_sort_by_key(key, true));
        ask_bands(&mut browse, sid, client, &ladder.server(ladder.items(50, 8)));
        assert!(bands_shown(&browse), "{key}: premise");
        browse.state.apply_view_local(&ViewAt::leaf(sid, "7"), WATCHED_FACTS);
        assert_eq!(!bands_shown(&browse), moved, "{key}: a watch hides the counts it moves");
        if moved {
            let _ = bands_job(&mut browse, sid);
        } else {
            no_job_is_owed(&mut browse, sid, key);
        }
    }
    // the unplayed count of a show likewise
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Show, &["unviewedLeafCount"]);
    assert!(browse.state.set_sort_by_key("unviewedLeafCount", true));
    let ladder = Ladder::unplayed();
    ask_bands(&mut browse, sid, client, &ladder.server(ladder.items(50, 8)));
    assert!(bands_shown(&browse));
    browse.state.apply_view_local(&ViewAt::leaf(sid, "7"), WATCHED_FACTS);
    assert!(!bands_shown(&browse));
    let (job, _port) = bands_job(&mut browse, sid);
    assert_eq!(job.req.filters, terms(&[("type", "2")]), "a show's listing is asked for as the page asks");
}

/// Counts describe one metadata type: leaving the Primary listing and coming back asks again.
#[test]
fn a_library_type_change_forgets_the_bands() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let ladder = Ladder::rating();
    ask_bands(&mut browse, sid, client, &ladder.server(ladder.items(50, 1)));
    assert!(bands_shown(&browse));
    assert!(browse.state.set_library_type(LibraryType::Collections));
    assert!(!bands_shown(&browse), "Collections have no value bands");
    assert!(browse.state.set_library_type(LibraryType::Primary));
    assert!(snapshot_bands(&browse).is_none());
    land_title_letters(&mut browse);
    let _ = bands_job(&mut browse, sid);
}

/// A job the runner refused gives its claim back, the step after settles it, and the one after asks
/// again with the registry's key as of that step.
#[test]
fn a_refused_bands_job_is_settled_and_asked_again() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let first = plx_plex::plex::ClientKey::of(client);
    let (job, port) = bands_job(&mut browse, sid);
    assert_eq!(job.req.server(), (sid, first));
    assert!(Directory::Bands.busy(&browse.adapter), "premise: the step raised the claim");
    port.refused(job.addr);
    assert!(!Directory::Bands.busy(&browse.adapter), "the refusal gave the claim back");
    // a claim that is down while the letters' stays up does not release the other kinds
    let jobs = tick_directory_without_keys(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(jobs.is_empty(), "no client, no read");
    assert!(!directory_refusal_pending(&browse, Directory::Bands, job.addr.req.0), "the refusal is consumed");
    assert!(browse.state.states()[0].bands_owed(SecKind::Movie), "still owed");
    client.set_token("new");
    let (job, _port) = bands_job(&mut browse, sid);
    assert_eq!(job.req.server(), (sid, plx_plex::plex::ClientKey::of(client)), "asked with the current key");
}

/// The body posts its answer outside the guard: a body that panics lands as a failed read.
#[test]
fn a_panicking_bands_body_lands_as_a_failed_read() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = banded_source(SecKind::Movie, &["rating"]);
    assert!(browse.state.set_sort_by_key("rating", true));
    let (job, port) = bands_job(&mut browse, sid);
    finish_fixed(&port, job.addr, client, job.req, |_| panic!("the body panicked"));
    assert!(browse.adapter.bands.has_mail(), "the answer was posted");
    let _ = browse.pump();
    assert!(browse.state.states()[0].bands_waiting(), "a failure, not an empty rail");
    assert!(!Directory::Bands.busy(&browse.adapter), "the take lowered the claim");
}
