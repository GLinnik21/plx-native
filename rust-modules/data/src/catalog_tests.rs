//! The catalog store against a scripted fetch: loaded, not found, unreachable, the stale-landing
//! rules, and membership read through the watchlist store. No network: both seams are fakes.

use super::*;
use crate::stores::catalog::CatalogStore;
use crate::stores::watchlist::{WatchlistCmd, WatchlistStore};
use crate::watchlist::ledger::Intent;
use crate::watchlist::model::Reach;
use crate::watchlist::{decode_hex, WatchlistIo};
use plx_plex::plex::watchlist::{WatchItem, WatchPage};
use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch, ServerId};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const HEX: &str = "0123456789abcdef01234567";

fn hex() -> [u8; 12] {
    decode_hex(HEX).unwrap()
}

fn other_hex() -> [u8; 12] {
    decode_hex("ffffffffffffffffffffff01").unwrap()
}

fn auth(generation: u32) -> WatchlistAuth {
    WatchlistAuth { client_id: "test-client".into(), credential: "test-credential".into(), user: Default::default(), generation }
}

fn item(title: &str, date: Option<&str>) -> CatalogItem {
    CatalogItem {
        item: WatchItem {
            hex: HEX.into(),
            guid: format!("plex://movie/{HEX}"),
            kind: Some(WatchKind::Movie),
            title: Some(title.into()),
            year: Some(2027),
            originally_available: date.map(str::to_owned),
            summary: Some("A summary.".into()),
            thumb: Some("https://example.invalid/poster.jpg".into()),
            ..Default::default()
        },
        genres: vec!["Drama".into(), "Thriller".into()],
        content_rating: None,
        duration_ms: None,
    }
}

struct Fake {
    answer: Mutex<Result<CatalogItem, WatchlistError>>,
    has_auth: AtomicBool,
    current: AtomicBool,
    generation: AtomicU32,
    fetched: Mutex<Vec<String>>,
    captures: AtomicUsize,
}

impl Fake {
    fn new(answer: Result<CatalogItem, WatchlistError>) -> Arc<Fake> {
        Arc::new(Fake {
            answer: Mutex::new(answer),
            has_auth: AtomicBool::new(true),
            current: AtomicBool::new(true),
            generation: AtomicU32::new(1),
            fetched: Mutex::new(Vec::new()),
            captures: AtomicUsize::new(0),
        })
    }
}

impl CatalogIo for Fake {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        self.captures.fetch_add(1, Ordering::SeqCst);
        self.has_auth.load(Ordering::SeqCst).then(|| auth(self.generation.load(Ordering::SeqCst)))
    }
    fn is_current(&self, a: &WatchlistAuth) -> bool {
        self.current.load(Ordering::SeqCst) && a.generation == self.generation.load(Ordering::SeqCst)
    }
    fn fetch(&self, _: &WatchlistAuth, hex: &str) -> Result<CatalogItem, WatchlistError> {
        self.fetched.lock().unwrap().push(hex.to_owned());
        self.answer.lock().unwrap().clone()
    }
    fn today(&self) -> Option<String> {
        Some("2026-10-15".into())
    }
}

fn inline(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    f();
    true
}

fn refuse(_: &str, _: Box<dyn FnOnce() + Send + 'static>) -> bool {
    false
}

/// A spawn that parks the job so a test can land it late.
fn park(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    PARKED.with(|p| p.borrow_mut().push(f));
    true
}

thread_local! {
    static PARKED: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send + 'static>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn store(fake: &Arc<Fake>) -> CatalogStore {
    CatalogStore::with_io(fake.clone()).with_spawn_for_test(inline)
}

#[test]
fn a_found_title_loads_with_its_record() {
    let fake = Fake::new(Ok(item("Coming Film", Some("2027-03-12"))));
    let mut s = store(&fake);
    s.open(hex());
    assert_eq!(s.load, Load::Loading, "nothing has landed before the first pump");
    assert!(s.pump());
    let Load::Loaded(rec) = &s.load else { panic!("loaded expected, got {:?}", s.load) };
    assert_eq!(rec.title, "Coming Film");
    assert_eq!(rec.year, Some(2027));
    assert_eq!(rec.kind, Kind::Movie);
    assert_eq!(rec.summary, "A summary.");
    assert_eq!(rec.genres, ["Drama", "Thriller"]);
    assert_eq!(rec.art_url, "https://example.invalid/poster.jpg");
    assert_eq!(rec.release_date.as_deref(), Some("2027-03-12"));
    assert_eq!(rec.status, CatalogStatus::ComingSoon);
    assert_eq!(*fake.fetched.lock().unwrap(), [HEX], "the 24-digit id the service takes");
}

#[test]
fn the_status_follows_the_release_date() {
    let released = record_of(&item("Old", Some("2020-01-01")), Some("2026-10-15"));
    assert_eq!(released.status, CatalogStatus::NotOnAnyServer);
    let today = record_of(&item("Today", Some("2026-10-15")), Some("2026-10-15"));
    assert_eq!(today.status, CatalogStatus::NotOnAnyServer, "released on the day");
    let undated = record_of(&item("Undated", None), Some("2026-10-15"));
    assert_eq!(undated.status, CatalogStatus::None);
    assert_eq!(undated.release_date, None);
    let no_clock = record_of(&item("Dated", Some("2099-01-01")), None);
    assert_eq!(no_clock.status, CatalogStatus::NotOnAnyServer, "no clock, no claim that it is ahead");
    let mut show = item("Show", None);
    show.item.kind = Some(WatchKind::Show);
    assert_eq!(record_of(&show, None).kind, Kind::Show);
}

#[test]
fn a_title_the_service_does_not_know_is_not_found() {
    let fake = Fake::new(Err(WatchlistError::NotFound));
    let mut s = store(&fake);
    s.open(hex());
    s.pump();
    assert_eq!(s.load, Load::NotFound);
}

#[test]
fn an_unanswered_or_refused_read_is_unreachable() {
    for why in [WatchlistError::Transport, WatchlistError::Unauthorized, WatchlistError::Unreadable, WatchlistError::Other(503)] {
        let fake = Fake::new(Err(why.clone()));
        let mut s = store(&fake);
        s.open(hex());
        s.pump();
        assert_eq!(s.load, Load::Unreachable, "{why:?}");
    }
}

#[test]
fn no_account_credential_is_unreachable_without_a_fetch() {
    let fake = Fake::new(Ok(item("X", None)));
    fake.has_auth.store(false, Ordering::SeqCst);
    let mut s = store(&fake);
    s.open(hex());
    assert_eq!(s.load, Load::Unreachable);
    assert!(fake.fetched.lock().unwrap().is_empty());
}

#[test]
fn try_again_reads_again_and_a_landing_raises_one_notice() {
    let fake = Fake::new(Err(WatchlistError::Transport));
    let mut s = store(&fake);
    s.open(hex());
    s.pump();
    assert_eq!(s.load, Load::Unreachable);
    assert!(s.take_notice().is_some());
    *fake.answer.lock().unwrap() = Ok(item("Back", None));
    s.open(hex());
    assert_eq!(s.load, Load::Loading, "a fresh read shows Loading at once");
    s.pump();
    assert!(matches!(s.load, Load::Loaded(_)));
    assert!(s.take_notice().is_some());
    assert!(s.take_notice().is_none());
    assert_eq!(fake.captures.load(Ordering::SeqCst), 1, "the identity is captured once while it is still seated");
}

#[test]
fn a_late_landing_for_an_earlier_title_is_dropped() {
    let fake = Fake::new(Ok(item("First", None)));
    let mut s = CatalogStore::with_io(fake.clone()).with_spawn_for_test(park);
    s.open(hex());
    s.open(other_hex());
    let jobs: Vec<_> = PARKED.with(|p| p.borrow_mut().drain(..).collect());
    assert_eq!(jobs.len(), 2);
    // The first request lands after the second was asked for: the page keeps waiting.
    let mut jobs = jobs.into_iter();
    jobs.next().unwrap()();
    assert!(!s.pump());
    assert_eq!(s.load, Load::Loading);
    assert_eq!(s.hex, Some(other_hex()));
    jobs.next().unwrap()();
    assert!(s.pump());
    assert!(matches!(s.load, Load::Loaded(_)));
}

#[test]
fn a_landing_for_a_switched_profile_asks_again_under_the_new_one() {
    let fake = Fake::new(Ok(item("Mine", None)));
    let mut s = CatalogStore::with_io(fake.clone()).with_spawn_for_test(park);
    s.open(hex());
    let job = PARKED.with(|p| p.borrow_mut().pop()).unwrap();
    fake.generation.store(2, Ordering::SeqCst);
    job();
    assert!(s.pump());
    assert_eq!(s.load, Load::Loading, "the old profile's answer is not shown");
    assert_eq!(fake.captures.load(Ordering::SeqCst), 2, "captured again for the new profile");
    let again = PARKED.with(|p| p.borrow_mut().pop()).unwrap();
    again();
    s.pump();
    assert!(matches!(s.load, Load::Loaded(_)));
}

#[test]
fn a_refused_spawn_retries_after_n_frames_and_never_blocks() {
    let fake = Fake::new(Ok(item("Late", None)));
    let mut s = CatalogStore::with_io(fake.clone()).with_spawn_for_test(refuse);
    s.open(hex());
    s.pump();
    assert!(fake.fetched.lock().unwrap().is_empty());
    s = s.with_spawn_for_test(inline);
    for _ in 0..(RETRY_FRAMES_FOR_TEST - 2) {
        s.pump();
    }
    assert!(fake.fetched.lock().unwrap().is_empty(), "not before N frames");
    for _ in 0..4 {
        s.pump();
    }
    assert!(matches!(s.load, Load::Loaded(_)), "then it goes");
}

const RETRY_FRAMES_FOR_TEST: u32 = 120;

#[test]
fn reset_forgets_the_title_and_drops_what_was_in_flight() {
    let fake = Fake::new(Ok(item("Gone", None)));
    let mut s = CatalogStore::with_io(fake.clone()).with_spawn_for_test(park);
    s.open(hex());
    let job = PARKED.with(|p| p.borrow_mut().pop()).unwrap();
    s.reset();
    job();
    assert!(!s.pump());
    assert_eq!(s.hex, None);
    assert_eq!(s.load, Load::Loading);
}

// ---- membership: read through the watchlist store, never kept here -------------------------

struct Wl {
    members: Vec<[u8; 12]>,
    list_fails: bool,
}

impl Reach for Wl {
    fn reachable(&self, _: ServerId) -> bool {
        false
    }
    fn roster(&self) -> Vec<ServerId> {
        Vec::new()
    }
}

impl WatchlistIo for Wl {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        Some(auth(1))
    }
    fn is_current(&self, a: &WatchlistAuth) -> bool {
        a.generation == 1
    }
    fn page(&self, _: &WatchlistAuth, start: u32, _: u32, _: Option<&str>) -> Result<WatchPage, WatchlistError> {
        if self.list_fails {
            return Err(WatchlistError::Transport);
        }
        let items: Vec<WatchItem> = self
            .members
            .iter()
            .skip(start as usize)
            .map(|h| {
                let hex = crate::watchlist::encode_hex(h);
                WatchItem { guid: format!("plex://movie/{hex}"), hex, kind: Some(WatchKind::Movie), title: Some("T".into()), ..Default::default() }
            })
            .collect();
        let raw = items.len() as u32;
        Ok(WatchPage { items, total: Some(self.members.len() as u32), start, raw, etag: None })
    }
    fn released(&self, _: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
        Ok(Vec::new())
    }
    fn write(&self, _: &WatchlistAuth, _: &str, _: Intent) -> Result<(), WatchlistError> {
        Ok(())
    }
    fn find(&self, _: ServerId, _: GuidKind, _: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
        Ok(Vec::new())
    }
    fn roster_gen(&self) -> u32 {
        1
    }
    fn facts_gen(&self) -> u32 {
        1
    }
    fn today(&self) -> Option<String> {
        None
    }
}

fn watchlist(io: Wl) -> WatchlistStore {
    let mut w = WatchlistStore::with_io(Arc::new(io)).with_spawn_for_test(inline);
    w.run(WatchlistCmd::Open);
    for _ in 0..4 {
        w.pump();
    }
    w
}

fn opened(fake: &Arc<Fake>) -> CatalogStore {
    let mut s = store(fake);
    s.open(hex());
    s
}

#[test]
fn membership_is_read_from_the_watchlist_store() {
    let fake = Fake::new(Ok(item("M", None)));
    let s = opened(&fake);
    let on = watchlist(Wl { members: vec![hex()], list_fails: false });
    assert_eq!(s.membership(&on), Membership::On);
    let off = watchlist(Wl { members: vec![other_hex()], list_fails: false });
    assert_eq!(s.membership(&off), Membership::Off);
}

#[test]
fn membership_follows_a_press_at_once_and_a_revert() {
    let fake = Fake::new(Ok(item("M", None)));
    let s = opened(&fake);
    let mut w = watchlist(Wl { members: vec![other_hex()], list_fails: false });
    assert_eq!(s.membership(&w), Membership::Off);
    w.run(WatchlistCmd::Press { hex: hex(), intent: Intent::Add });
    assert_eq!(s.membership(&w), Membership::On, "the flip is immediate");
    w.run(WatchlistCmd::Press { hex: hex(), intent: Intent::Remove });
    assert_eq!(s.membership(&w), Membership::Off);
}

#[test]
fn membership_is_unknown_when_the_list_never_landed_and_a_press_still_flips() {
    let fake = Fake::new(Ok(item("M", None)));
    let s = opened(&fake);
    let mut w = watchlist(Wl { members: vec![], list_fails: true });
    assert_eq!(s.membership(&w), Membership::Unknown, "plex.tv unreachable: the Add face");
    w.run(WatchlistCmd::Press { hex: hex(), intent: Intent::Add });
    assert_eq!(s.membership(&w), Membership::On, "an intent in flight is known even without a list");
    let idle = WatchlistStore::with_io(Arc::new(Wl { members: vec![], list_fails: false }));
    assert_eq!(s.membership(&idle), Membership::Unknown, "before the list is asked");
}

#[test]
fn membership_without_a_title_is_unknown() {
    let fake = Fake::new(Ok(item("M", None)));
    let s = store(&fake);
    let w = watchlist(Wl { members: vec![hex()], list_fails: false });
    assert_eq!(s.membership(&w), Membership::Unknown);
}

#[test]
fn a_tile_state_dictates_the_status_line_and_a_silent_server_never_says_on_no_server() {
    use crate::watchlist::model::TileState as T;
    assert_eq!(CatalogStatus::of_tile(T::ComingSoon), CatalogStatus::ComingSoon);
    assert_eq!(CatalogStatus::of_tile(T::ComingSoonMatched), CatalogStatus::ComingSoon);
    assert_eq!(CatalogStatus::of_tile(T::NotOnAnyServer), CatalogStatus::NotOnAnyServer);
    assert_eq!(CatalogStatus::of_tile(T::ServerOffline), CatalogStatus::ServerOffline);
    assert_eq!(CatalogStatus::of_tile(T::Unknown), CatalogStatus::None);
    assert_eq!(CatalogStatus::of_tile(T::Available), CatalogStatus::None);
}
