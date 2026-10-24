//! The catalog store against a scripted fetch: loaded, not found, unreachable, the stale-landing
//! rules, and membership read through the watchlist store. No network: both seams are fakes.

use super::*;
use crate::stores::catalog::{finish_catalog_job, CatalogPort, CatalogReq, CatalogStore};
use crate::stores::{StoreId, StoreJob};
use crate::stores::watchlist::{WatchlistCmd, WatchlistStore};
use crate::watchlist::ledger::Intent;
use crate::watchlist::model::Reach;
use crate::watchlist::{decode_hex, WatchlistIo};
use plx_machine::job::{Job, Port};
use plx_machine::machine::{Addr, MachineId, RequestId};
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
    /// The read panics instead of answering.
    panics: AtomicBool,
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
            panics: AtomicBool::new(false),
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
        assert!(!self.panics.load(Ordering::SeqCst), "scripted panic in the catalog read");
        self.answer.lock().unwrap().clone()
    }
    fn today(&self) -> Option<String> {
        Some("2026-10-15".into())
    }
}

/// The Watchlist store's worker, run in place (that store still starts its own).
fn inline(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    f();
    true
}

fn store(fake: &Arc<Fake>) -> CatalogStore {
    CatalogStore::with_io(fake.clone())
}

/// The address of the catalog read for request `req`.
fn addr(req: u32) -> Addr {
    Addr { to: MachineId::Store(StoreId::Catalog.ord()), req: RequestId(req) }
}

/// The one job the store has emitted, taken apart.
fn one(s: &mut CatalogStore) -> (Job<CatalogReq>, CatalogPort) {
    let mut jobs = s.take_jobs();
    assert_eq!(jobs.len(), 1, "one read is out at a time");
    let StoreJob::Catalog(job, port) = jobs.remove(0) else { panic!("not a catalog job") };
    (job, port)
}

/// Run a job the way the runner's worker does: through the production completion path, on this
/// thread. Its answer is in the store's mailbox for the next pump.
fn run(job: StoreJob) {
    let StoreJob::Catalog(job, port) = job else { panic!("not a catalog job") };
    finish_catalog_job(&port, job.addr, job.req);
}

/// Run everything the store has emitted, as an inline runner would, and return how many ran.
fn run_emitted(s: &mut CatalogStore) -> usize {
    let jobs = s.take_jobs();
    let n = jobs.len();
    jobs.into_iter().for_each(run);
    n
}

/// `open` as the Bridge does it: the step, then every job it emitted started in the same call.
fn open(s: &mut CatalogStore, hex: [u8; 12]) {
    s.open(hex);
    run_emitted(s);
}

#[test]
fn a_found_title_loads_with_its_record() {
    let fake = Fake::new(Ok(item("Coming Film", Some("2027-03-12"))));
    let mut s = store(&fake);
    open(&mut s, hex());
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
    open(&mut s, hex());
    s.pump();
    assert_eq!(s.load, Load::NotFound);
}

#[test]
fn an_unanswered_or_refused_read_is_unreachable() {
    for why in [WatchlistError::Transport, WatchlistError::Unauthorized, WatchlistError::Unreadable, WatchlistError::Other(503)] {
        let fake = Fake::new(Err(why.clone()));
        let mut s = store(&fake);
        open(&mut s, hex());
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
    assert!(!s.has_jobs(), "no credential, no job");
    assert!(fake.fetched.lock().unwrap().is_empty());
}

#[test]
fn try_again_reads_again_and_a_landing_raises_one_notice() {
    let fake = Fake::new(Err(WatchlistError::Transport));
    let mut s = store(&fake);
    open(&mut s, hex());
    s.pump();
    assert_eq!(s.load, Load::Unreachable);
    assert!(s.take_notice().is_some());
    *fake.answer.lock().unwrap() = Ok(item("Back", None));
    open(&mut s, hex());
    assert_eq!(s.load, Load::Loading, "a fresh read shows Loading at once");
    s.pump();
    assert!(matches!(s.load, Load::Loaded(_)));
    assert!(s.take_notice().is_some());
    assert!(s.take_notice().is_none());
    assert_eq!(fake.captures.load(Ordering::SeqCst), 1, "the identity is captured once while it is still seated");
}

/// A read that panics on its worker must still end the page's wait. The job's body is guarded:
/// the unwind stops there and the answer is posted outside the guard, so the page reads as
/// unreachable instead of waiting on a landing that would never come.
#[test]
fn a_read_that_panics_leaves_loading_for_the_unreachable_page() {
    let fake = Fake::new(Ok(item("X", None)));
    fake.panics.store(true, Ordering::SeqCst);
    let mut s = store(&fake);
    open(&mut s, hex());
    assert!(s.pump());
    assert_eq!(s.load, Load::Unreachable, "a read that panicked answers as a failed one does");
    assert!(!s.has_jobs(), "...and is not retried");
}

/// `open` emits exactly one job, addressed by the request id and carrying the identity and the
/// title; the port reaches this store's mailbox. Nothing else is emitted, on the next pump either.
#[test]
fn an_open_emits_one_job_naming_the_title_and_the_identity() {
    let fake = Fake::new(Ok(item("A", None)));
    let mut s = store(&fake);
    assert!(!s.has_jobs());
    s.open(hex());
    assert!(s.has_jobs());
    plx_machine::assert_fx!(s.take_jobs(), [StoreJob::Catalog(
        Job { addr: addr(1), req: CatalogReq::for_test(auth(1), hex()) },
        s.catalog_port(),
    )]);
    assert!(!s.has_jobs(), "taking the jobs empties the outbox");
    assert!(fake.fetched.lock().unwrap().is_empty(), "the step reads nothing: the runner does");
    s.pump();
    plx_machine::assert_fx!(s.take_jobs(), []);
    // Another open is another request, and one job again.
    s.open(other_hex());
    plx_machine::assert_fx!(s.take_jobs(), [StoreJob::Catalog(
        Job { addr: addr(2), req: CatalogReq::for_test(auth(1), other_hex()) },
        s.catalog_port(),
    )]);
}

/// A job not yet taken belongs to the request it was emitted for: the next `open`, or a reset,
/// takes it back, so a runner never starts a read for a title the page has left.
#[test]
fn an_untaken_job_goes_with_the_request_that_superseded_it() {
    let fake = Fake::new(Ok(item("A", None)));
    let mut s = store(&fake);
    s.open(hex());
    s.open(other_hex());
    let (job, _) = one(&mut s);
    assert_eq!((job.addr, job.req), (addr(2), CatalogReq::for_test(auth(1), other_hex())));
    s.open(hex());
    s.reset();
    assert!(!s.has_jobs(), "a reset forgets the title, and the credential in its job");
}

#[test]
fn a_late_landing_for_an_earlier_title_is_dropped() {
    let fake = Fake::new(Ok(item("First", None)));
    let mut s = store(&fake);
    s.open(hex());
    let first = s.take_jobs();
    s.open(other_hex());
    let second = s.take_jobs();
    assert_eq!((first.len(), second.len()), (1, 1), "each open emitted its one read");
    // The first request lands after the second was asked for: the page keeps waiting.
    first.into_iter().for_each(run);
    assert!(!s.pump());
    assert_eq!(s.load, Load::Loading);
    assert_eq!(s.hex, Some(other_hex()));
    second.into_iter().for_each(run);
    assert!(s.pump());
    assert!(matches!(s.load, Load::Loaded(_)));
}

#[test]
fn a_landing_for_a_switched_profile_asks_again_under_the_new_one() {
    let fake = Fake::new(Ok(item("Mine", None)));
    let mut s = store(&fake);
    s.open(hex());
    let old = s.take_jobs();
    fake.generation.store(2, Ordering::SeqCst);
    old.into_iter().for_each(run);
    assert!(s.pump());
    assert_eq!(s.load, Load::Loading, "the old profile's answer is not shown");
    assert_eq!(fake.captures.load(Ordering::SeqCst), 2, "captured again for the new profile");
    // The pump's re-open emitted the new profile's read: it is on the outbox for the Bridge's drain.
    let (job, port) = one(&mut s);
    assert_eq!(job.req, CatalogReq::for_test(auth(2), hex()), "asked as the new identity");
    run(StoreJob::Catalog(job, port));
    s.pump();
    assert!(matches!(s.load, Load::Loaded(_)));
}

/// The runner could not start the job. Its refusal is the job's one answer: the next pump takes it
/// as a landing that shows nothing, owes the request, and asks again only after the same 120 frames
/// the failed spawn waited, without a job in between (no respawn spin).
#[test]
fn a_refused_job_posts_not_started_and_asks_again_after_n_frames_and_never_blocks() {
    let fake = Fake::new(Ok(item("Late", None)));
    let mut s = store(&fake);
    s.open(hex());
    let (job, port) = one(&mut s);
    port.refused(job.addr);
    assert!(!s.pump(), "a refusal changes nothing the page shows");
    assert_eq!(s.load, Load::Loading);
    assert!(!s.has_jobs(), "...and does not respawn at once");
    assert!(fake.fetched.lock().unwrap().is_empty());
    for frame in 1..RETRY_FRAMES_FOR_TEST {
        assert!(!s.pump());
        assert!(!s.has_jobs(), "frame {frame}: still waiting");
    }
    assert!(!s.pump());
    let (again, port) = one(&mut s);
    assert_eq!(again, job, "the same read, asked again");
    finish_catalog_job(&port, again.addr, again.req);
    assert!(s.pump());
    assert!(matches!(s.load, Load::Loaded(_)), "then it goes");
    assert_eq!(*fake.fetched.lock().unwrap(), [HEX], "asked once");
}

/// A refusal that comes after the page has moved on lands in a retired mailbox: it owes nothing.
#[test]
fn a_refusal_for_a_retired_request_owes_nothing() {
    let fake = Fake::new(Ok(item("Gone", None)));
    let mut s = store(&fake);
    s.open(hex());
    let (job, port) = one(&mut s);
    s.open(other_hex());
    let _ = s.take_jobs();
    port.refused(job.addr);
    for _ in 0..(RETRY_FRAMES_FOR_TEST + 2) {
        s.pump();
    }
    assert!(!s.has_jobs(), "the new request was never refused");
}

const RETRY_FRAMES_FOR_TEST: u32 = 120;

#[test]
fn reset_forgets_the_title_and_drops_what_was_in_flight() {
    let fake = Fake::new(Ok(item("Gone", None)));
    let mut s = store(&fake);
    s.open(hex());
    let jobs = s.take_jobs();
    s.reset();
    jobs.into_iter().for_each(run);
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
