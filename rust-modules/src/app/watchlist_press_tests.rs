//! The Watchlist press paths (plan §1.8, §1.9, W6): every route into a write, the §1.9 state ×
//! press table, a plex.tv that cannot be reached, and the tab pulse.
//!
//! Like the wiring tests, the store runs over a scripted [`WatchlistIo`] with an inline worker
//! runner and the catalog store over a scripted [`CatalogIo`] whose reads the rig runs in the frame
//! they were emitted (`feed_catalog`): a landing is a pump away and nothing here reaches a network
//! or a real account.

use super::*;
use super::test_support::tick;
use plx_data::catalog::{CatalogIo, Load, Membership};
use plx_data::stores::catalog::CatalogStore;
use plx_data::stores::watchlist::WatchlistStore;
use plx_data::watchlist::ledger::Intent;
use plx_data::watchlist::model::{Kind, Reach, ServerMatch, TileState};
use plx_data::watchlist::{decode_hex, encode_hex, WatchlistAuth, WatchlistIo};
use plx_plex::plex::watchlist::{CatalogItem, WatchItem, WatchKind, WatchPage, WatchlistError};
use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch, ListingFacts, ServerId};
use plx_screens::catalog::CatalogScreen;
use plx_screens::registry::ContentArg;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

fn guid(n: u8) -> String {
    format!("plex://movie/{}", encode_hex(&[n; 12]))
}

/// A scripted plex.tv: a fixed list; reads and writes can be refused (offline).
pub(super) struct Script {
    items: Vec<WatchItem>,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
    writes: Mutex<Vec<([u8; 12], Intent)>>,
    /// The servers on the roster; empty = no server asked.
    roster: Mutex<Vec<ServerId>>,
    /// Are they reachable?
    up: AtomicBool,
    /// Does every roster server hold every title?
    holds: AtomicBool,
    /// What the lookup row of a held title says of it for a caption (genre, video, ratings, length).
    copy_facts: Mutex<ListingFacts>,
    facts: AtomicU32,
    /// Each server record's own `guid`, by `(server, ratingKey)`: what `client.metadata` would say.
    records: Mutex<HashMap<(ServerId, String), String>>,
    /// Record reads made (a hex the row already carried costs none).
    record_reads: AtomicU32,
}

impl Script {
    /// The writes plex.tv was sent, in order.
    pub(super) fn writes(&self) -> Vec<([u8; 12], Intent)> {
        self.writes.lock().unwrap().clone()
    }

    pub(super) fn new(titles: &[u8]) -> Arc<Script> {
        Self::with_kinds(titles, |_| WatchKind::Movie)
    }

    /// `titles` with the kind `kind_of` names for each number.
    pub(super) fn with_kinds(titles: &[u8], kind_of: impl Fn(u8) -> WatchKind) -> Arc<Script> {
        Arc::new(Script {
            items: titles
                .iter()
                .map(|n| WatchItem {
                    hex: encode_hex(&[*n; 12]),
                    guid: guid(*n),
                    kind: Some(kind_of(*n)),
                    title: Some(format!("Title {n}")),
                    added_at: Some(1_000 - i64::from(*n)),
                    ..Default::default()
                })
                .collect(),
            fail_writes: AtomicBool::new(false),
            fail_reads: AtomicBool::new(false),
            writes: Mutex::new(Vec::new()),
            roster: Mutex::new(Vec::new()),
            up: AtomicBool::new(false),
            holds: AtomicBool::new(false),
            copy_facts: Mutex::new(ListingFacts::default()),
            facts: AtomicU32::new(1),
            records: Mutex::new(HashMap::new()),
            record_reads: AtomicU32::new(0),
        })
    }
}

impl Reach for Script {
    fn reachable(&self, _: ServerId) -> bool {
        self.up.load(Ordering::SeqCst)
    }
    fn roster(&self) -> Vec<ServerId> {
        self.roster.lock().unwrap().clone()
    }
}

impl WatchlistIo for Script {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        Some(WatchlistAuth {
            client_id: "synthetic-client".into(),
            credential: "synthetic-credential".into(),
            user: Default::default(),
            generation: 1,
        })
    }
    fn is_current(&self, auth: &WatchlistAuth) -> bool {
        auth.generation == 1
    }
    fn page(&self, _: &WatchlistAuth, start: u32, size: u32, _: Option<&str>) -> Result<WatchPage, WatchlistError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(WatchlistError::Transport);
        }
        let items: Vec<WatchItem> = self.items.iter().skip(start as usize).take(size as usize).cloned().collect();
        let raw = items.len() as u32;
        Ok(WatchPage { items, total: Some(self.items.len() as u32), start, raw, etag: None })
    }
    fn released(&self, _: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
        Ok(self.items.clone())
    }
    fn write(&self, _: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError> {
        self.writes.lock().unwrap().push((decode_hex(hex).expect("a decodable id"), intent));
        if self.fail_writes.load(Ordering::SeqCst) { Err(WatchlistError::Transport) } else { Ok(()) }
    }
    fn find(&self, _: ServerId, _: GuidKind, guids: &[String]) -> Result<Vec<GuidMatch>, GuidLookupError> {
        if !self.holds.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        Ok(guids
            .iter()
            .map(|g| GuidMatch { guid: g.clone(), rating_key: "42".into(), library_section_id: None, blur: None, facts: self.copy_facts.lock().unwrap().clone() })
            .collect())
    }
    fn roster_gen(&self) -> u32 {
        1
    }
    fn facts_gen(&self) -> u32 {
        self.facts.load(Ordering::SeqCst)
    }
    fn guid_of(&self, server: ServerId, rk: &str) -> Option<String> {
        self.record_reads.fetch_add(1, Ordering::SeqCst);
        self.records.lock().unwrap().get(&(server, rk.to_owned())).cloned()
    }
    fn today(&self) -> Option<String> {
        Some("2026-10-15".into())
    }
}

/// A scripted discover service for the catalog page: one title, released later than "today".
struct Discover;

impl CatalogIo for Discover {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        Some(WatchlistAuth {
            client_id: "synthetic-client".into(),
            credential: "synthetic-credential".into(),
            user: Default::default(),
            generation: 1,
        })
    }
    fn is_current(&self, auth: &WatchlistAuth) -> bool {
        auth.generation == 1
    }
    fn fetch(&self, _: &WatchlistAuth, hex: &str) -> Result<CatalogItem, WatchlistError> {
        Ok(CatalogItem {
            item: WatchItem {
                hex: hex.into(),
                kind: Some(WatchKind::Movie),
                title: Some("Not Yet Released".into()),
                originally_available: Some("2026-12-31".into()),
                ..Default::default()
            },
            genres: vec!["Drama".into()],
            content_rating: None,
            duration_ms: None,
        })
    }
    fn today(&self) -> Option<String> {
        Some("2026-10-15".into())
    }
}

fn inline(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    f();
    true
}

/// A spawn that refuses every job: the store's worker never starts, so its first read stays ON ITS
/// WAY (the store retries on its frame counter and is refused again; it never blocks).
fn refuse(_: &str, _: Box<dyn FnOnce() + Send + 'static>) -> bool {
    false
}

/// Hold the Watchlist's reads: until [`release_reads`] the list is `Loading`, the answer a real
/// plex.tv has not given yet (a scripted one answers inside the call).
pub(super) fn hold_reads(rig: &mut Bridge) {
    let store = std::mem::take(rig.watchlist_for_test());
    *rig.watchlist_for_test() = store.with_spawn_for_test(refuse);
}

/// Let the held read run: the store reads the list again and the next frames land the rows.
pub(super) fn release_reads(rig: &mut Bridge) {
    let store = std::mem::take(rig.watchlist_for_test());
    *rig.watchlist_for_test() = store.with_spawn_for_test(inline);
    rig.watchlist_for_test().run(plx_data::stores::watchlist::WatchlistCmd::Open);
}

/// A rig on Home, the Watchlist store over `script` and the catalog store over [`Discover`].
pub(super) fn rig_over(script: &Arc<Script>) -> (Dispatcher<AppHost>, Bridge) {
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    *rig.watchlist_for_test() = WatchlistStore::with_io(script.clone()).with_spawn_for_test(inline);
    rig.stores.catalog = CatalogStore::with_io(Arc::new(Discover));
    super::nav_root(&mut d, AppArg::Home);
    super::frame(&mut d, &mut rig, tick(0), vec![]);
    (d, rig)
}

/// The catalog page's feed, then its read (if the feed emitted one) run on this thread, so the
/// answer is in the mailbox for the next frame's pass, as an inline worker's was. Only the catalog's
/// jobs run: a Detail fetch another test is holding stays parked.
pub(super) fn feed_catalog(rig: &mut Bridge, d: &mut Dispatcher<AppHost>) {
    rig.catalog_feed(d);
    rig.run_held_store_jobs_of_for_test(plx_data::stores::StoreId::Catalog);
}

/// Land what the workers brought and feed the pages, a few frames.
pub(super) fn settle(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, from: u32) {
    for i in 0..6 {
        rig.watchlist_for_test().pump();
        rig.watchlist_feed(d);
        feed_catalog(rig, d);
        super::frame(d, rig, tick(from + i), vec![]);
    }
}

/// Read the list once, as the tab or a page that needs membership does.
fn read_list(d: &mut Dispatcher<AppHost>, rig: &mut Bridge) {
    rig.watchlist_for_test().run(plx_data::stores::watchlist::WatchlistCmd::Open);
    settle(d, rig, 1);
}

fn member(rig: &mut Bridge, n: u8) -> bool {
    rig.watchlist_member(&[n; 12])
}

fn catalog_page(d: &Dispatcher<AppHost>) -> &CatalogScreen {
    d.top_screen()
        .expect("a page is up")
        .as_any()
        .and_then(|s| s.downcast_ref::<CatalogScreen>())
        .expect("the catalog page is the top page")
}

/// §1.9, every state's OK and hold: a playable title (and a coming-soon one with a reachable copy,
/// Q6) holds as a card of that copy plus Remove; every other state holds as the catalog rows with no
/// PMS identity and a Remove face (the tile IS on the list), and OK opens the catalog page of its id.
#[test]
fn the_state_by_press_table_holds_and_opens_the_right_thing_in_all_six_states() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1, 2, 3]);
    let (mut d, mut rig) = rig_over(&script);
    super::nav_tab(&mut d, &mut rig, HomeTab::Watchlist, None, None);
    super::frame(&mut d, &mut rig, tick(1), vec![]);
    settle(&mut d, &mut rig, 2);
    let entry = d.nav.top_page().unwrap().id;
    let copy = ServerMatch { server_id: ServerId::from_raw(1), rating_key: 42, blur: None };
    let hex = [2u8; 12];
    // (state, has a copy, is a card of that copy)
    let table = [
        (TileState::Available, true, true),
        (TileState::ComingSoonMatched, true, true),
        (TileState::ComingSoon, false, false),
        (TileState::NotOnAnyServer, false, false),
        (TileState::ServerOffline, true, false),
        (TileState::Unknown, false, false),
    ];
    for (state, has_copy, card) in table {
        let arg = rig.watchlist_hold_arg(&mut d, entry, None, &guid(2), state, has_copy.then_some(copy))
            .unwrap_or_else(|| panic!("{state:?}: the tile is on the page"));
        if card {
            assert!(matches!(arg.kind, ItemMenuKind::Card { .. }), "{state:?}: a card of the copy");
            assert_eq!((arg.sid, arg.rk.as_str()), (copy.server_id, "42"), "{state:?}");
            assert!(arg.watchlist.is_none(), "{state:?}: the card keeps its single Remove row, no second face");
        } else {
            assert!(matches!(arg.kind, ItemMenuKind::Catalog { .. }), "{state:?}: the catalog rows");
            assert_eq!((arg.sid, arg.rk.as_str()), (ServerId::UNSET, ""), "{state:?}: no PMS identity");
            let face = arg.watchlist.unwrap_or_else(|| panic!("{state:?}: the Remove face"));
            assert_eq!((face.hex, face.member), (hex, Membership::On), "{state:?}: the tile is on the list");
            assert_eq!(arg.catalog.as_ref().map(|c| c.guid.clone()), Some(guid(2)), "{state:?}");
            // OK on such a tile pushes the catalog page of the same id.
            super::open_catalog(&mut d, hex, None);
            for i in 0..40 {
                super::frame(&mut d, &mut rig, tick(100 + i), vec![]);
            }
            assert!(d.top_arg() == Some(&AppArg::Content(ContentArg::Catalog { hex })), "{state:?}: OK opens the page");
            super::nav_pop(&mut d);
            for i in 0..40 {
                super::frame(&mut d, &mut rig, tick(200 + i), vec![]);
            }
            assert!(d.top_arg() == Some(&AppArg::Watchlist), "{state:?}: BACK returns to the list");
        }
    }
}

/// The catalog page: pushed by hex, opens the catalog store for it, is fed the record and the
/// membership, and its pill's press is the same write as every other route (a flip, then a pulse).
#[test]
fn the_catalog_page_loads_its_record_and_its_pill_flips_the_membership() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    let (mut d, mut rig) = rig_over(&script);
    let hex = [9u8; 12];
    super::open_catalog(&mut d, hex, None);
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(10 + i), vec![]);
    }
    settle(&mut d, &mut rig, 60);
    let vm = catalog_page(&d).view_model().clone();
    assert!(matches!(vm.load, plx_screens::catalog::Load::Loaded { .. }), "the record landed: {:?}", vm.load);
    assert_eq!(vm.membership, plx_screens::catalog::Membership::Off, "the list was read and the title is absent");
    // The pill's press, as the app receives it.
    rig.catalog_effect(plx_screens::catalog::CatalogOut::Add { hex });
    assert!(member(&mut rig, 9), "the face flipped at the press");
    settle(&mut d, &mut rig, 80);
    assert_eq!(catalog_page(&d).view_model().membership, plx_screens::catalog::Membership::On);
    assert_eq!(*script.writes.lock().unwrap(), vec![(hex, Intent::Add)]);
    // …and the page going away forgets the title.
    super::nav_pop(&mut d);
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(100 + i), vec![]);
    }
    settle(&mut d, &mut rig, 200);
    assert_eq!(rig.stores.catalog.hex, None, "the store let go of the title");
    assert!(matches!(rig.stores.catalog.load, Load::Loading));
}

/// plex.tv refuses the write (offline, or a dev build without the stand-in): the face flips at the
/// press and reverts when the answer comes. Nothing is said.
#[test]
fn a_press_flips_then_reverts_when_the_write_is_refused() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    script.fail_writes.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = rig_over(&script);
    read_list(&mut d, &mut rig);
    assert!(!member(&mut rig, 5), "not on the list to begin with");
    rig.watchlist_add([5; 12]);
    assert!(member(&mut rig, 5), "flipped at the press");
    settle(&mut d, &mut rig, 10);
    assert_eq!(*script.writes.lock().unwrap(), vec![([5u8; 12], Intent::Add)], "the write was tried");
    assert!(!member(&mut rig, 5), "and the flip reverted");
}

/// The membership the hero discs are fed across an Add and a Remove on a list that never landed:
/// `Unknown` -> `On` at the press (the shape the discs' flip must treat as the user's own change,
/// `watchlist_disc::Disc::set`), and `On` -> `Unknown` at the Remove.
#[test]
fn an_add_on_an_unread_list_feeds_unknown_then_on() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    script.fail_reads.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = rig_over(&script);
    read_list(&mut d, &mut rig);
    assert_eq!(super::watchlist::membership_in(&rig.stores.watchlist.view(), &[5; 12]), Membership::Unknown);
    rig.watchlist_add([5; 12]);
    assert_eq!(super::watchlist::membership_in(&rig.stores.watchlist.view(), &[5; 12]), Membership::On);
    assert!(rig.watchlist_remove(&guid(5), false));
    assert_eq!(super::watchlist::membership_in(&rig.stores.watchlist.view(), &[5; 12]), Membership::Unknown);
}

/// plex.tv cannot be read at all: the membership is unknown, so the disc and the pill wear the Add
/// (outline) face and a menu offers no Watchlist row; a press still flips, then reverts.
#[test]
fn with_plex_tv_unreachable_the_face_is_outline_the_menu_row_is_omitted_and_a_press_reverts() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    script.fail_reads.store(true, Ordering::SeqCst);
    script.fail_writes.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = rig_over(&script);
    read_list(&mut d, &mut rig);
    let card = plx_data::pms::PmsMovie { kind: 0, guid: Some([4; 12]), ..Default::default() };
    assert!(rig.watchlist_card_face(&card).is_none(), "the menu row is omitted (no face)");
    let hex = [4u8; 12];
    assert!(matches!(rig.stores.watchlist.view().status(), plx_data::stores::watchlist::Status::Failed(_)),
        "the first read failed");
    // (The disc's and the pill's Unknown -> outline/Add face and the Add press are the pure tables in
    // `detail::hero_watchlist_tests` and `catalog::tests`; here the store is what is unreachable.)
    assert_eq!(rig.stores.catalog.membership(&rig.stores.watchlist), Membership::Unknown);
    rig.watchlist_add(hex);
    assert!(member(&mut rig, 4), "flipped");
    settle(&mut d, &mut rig, 10);
    assert!(!member(&mut rig, 4), "reverted");
}

/// The menu face by membership and kind: a movie or show acts on itself; an episode or a season on
/// its show, with the "Show" labels; an unread list wears Add (Unknown); a read one says On / Off.
#[test]
fn the_menu_face_carries_the_title_identity_and_the_membership_read_at_open() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1, 2]);
    let (mut d, mut rig) = rig_over(&script);
    let movie = plx_data::pms::PmsMovie { kind: 0, guid: Some([1; 12]), ..Default::default() };
    let episode = plx_data::pms::PmsMovie { kind: 3, guid: Some([8; 12]), show_guid: Some([2; 12]), ..Default::default() };
    let no_guid = plx_data::pms::PmsMovie { kind: 0, ..Default::default() };
    let unread = rig.watchlist_card_face(&movie).expect("an unread list is Unknown, not unreachable");
    assert_eq!(unread.member, Membership::Unknown, "wears the Add face");
    read_list(&mut d, &mut rig);
    let on = rig.watchlist_card_face(&movie).unwrap();
    assert_eq!((on.hex, on.member, on.kind, on.from_episode), ([1; 12], Membership::On, Kind::Movie, false));
    let ep = rig.watchlist_card_face(&episode).unwrap();
    assert_eq!((ep.hex, ep.member, ep.kind, ep.from_episode), ([2; 12], Membership::On, Kind::Show, true),
        "an episode acts on its SHOW");
    let off = rig.watchlist_card_face(&plx_data::pms::PmsMovie { kind: 1, guid: Some([6; 12]), ..Default::default() }).unwrap();
    assert_eq!(off.member, Membership::Off);
    assert!(rig.watchlist_card_face(&no_guid).is_none(), "no catalog id, no row");
}

/// §1.8: an Add from any route pulses the Watchlist mark on the strip; a removal does not, and a
/// strip without the mark drops the request.
#[test]
fn an_add_pulses_the_watchlist_tab_and_a_removal_does_not() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    let (mut d, mut rig) = rig_over(&script);
    super::nav_tab(&mut d, &mut rig, HomeTab::Watchlist, None, None);
    super::frame(&mut d, &mut rig, tick(1), vec![]);
    settle(&mut d, &mut rig, 2);
    let key = plx_ui::widgets::TAB_KEY_WATCHLIST;
    let at = rig.chrome.keys.iter().position(|k| *k == key).expect("the mark is on the strip");
    rig.watchlist_press([1; 12], Intent::Remove);
    assert_eq!(rig.chrome.take_pulse(), None, "a removal does not pulse");
    rig.catalog_effect(plx_screens::catalog::CatalogOut::Remove { hex: [1; 12] });
    assert_eq!(rig.chrome.take_pulse(), None, "nor does the catalog pill's removal");
    rig.watchlist_add([7; 12]);
    assert_eq!(rig.chrome.take_pulse(), Some(at), "an add pulses the Watchlist mark");
    assert_eq!(rig.chrome.take_pulse(), None, "and the request is spent once handed on");
    // Handed to the strip's renderer it runs, and ends at rest.
    rig.strip.pulse(at);
    assert_eq!(rig.strip.pulse_read().map(|(i, _)| i), Some(at));
    // A pulse asked for while no bar is on screen is dropped, not played late.
    rig.watchlist_add([7; 12]);
    rig.chrome.drop_pulse();
    assert_eq!(rig.chrome.take_pulse(), None);
}

/// The catalog page's status line follows the title's TILE, not the release date alone: an
/// `Unknown` tile (a silent server) never reads "on no server", a `ServerOffline` one says the
/// server is offline, and a flip of the tile (a server going away) moves the line while the page is
/// open. The scripted record is dated ahead of "today", so the date rule alone would say coming soon.
#[test]
fn the_catalog_status_follows_the_tile_state_and_moves_when_it_does() {
    use plx_screens::catalog::{Load as ScreenLoad, Status};
    let status = |d: &Dispatcher<AppHost>| match &catalog_page(d).view_model().load {
        ScreenLoad::Loaded { status, .. } => *status,
        other => panic!("the record should have landed: {other:?}"),
    };
    let _serial = plx_base::testlock::serial();
    // No server on the roster: the title is Unknown, which says nothing (never "on no server").
    let silent = Script::new(&[1]);
    let (mut d, mut rig) = rig_over(&silent);
    super::open_catalog(&mut d, [1; 12], None);
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(10 + i), vec![]);
    }
    settle(&mut d, &mut rig, 60);
    assert_eq!(rig.watchlist_for_test().view().tile_state_of(&[1; 12]), Some(TileState::Unknown));
    assert_eq!(status(&d), Status::None, "a silent server is not 'on no server'");

    // A server that answered "absent": released and held nowhere.
    let absent = Script::new(&[1]);
    *absent.roster.lock().unwrap() = vec![ServerId::from_raw(1)];
    absent.up.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = rig_over(&absent);
    super::open_catalog(&mut d, [1; 12], None);
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(10 + i), vec![]);
    }
    settle(&mut d, &mut rig, 60);
    assert_eq!(status(&d), Status::NotOnAnyServer);

    // A copy is held, then its server goes away: the open page's line moves to "Server offline".
    let held = Script::new(&[1]);
    *held.roster.lock().unwrap() = vec![ServerId::from_raw(1)];
    held.up.store(true, Ordering::SeqCst);
    held.holds.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = rig_over(&held);
    super::open_catalog(&mut d, [1; 12], None);
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(10 + i), vec![]);
    }
    settle(&mut d, &mut rig, 60);
    assert_eq!(rig.watchlist_for_test().view().tile_state_of(&[1; 12]), Some(TileState::Available));
    assert_eq!(status(&d), Status::None, "a reachable copy: nothing to say, whatever the date");
    held.up.store(false, Ordering::SeqCst);
    held.facts.fetch_add(1, Ordering::SeqCst);
    settle(&mut d, &mut rig, 100);
    assert_eq!(rig.watchlist_for_test().view().tile_state_of(&[1; 12]), Some(TileState::ServerOffline));
    assert_eq!(status(&d), Status::ServerOffline, "the match landing re-derived the status line");
    assert_eq!(plx_screens::catalog::status_icon(Status::ServerOffline, None), Some(plx_ui::icons::Icon::ServerSlash));
}

/// A card row that carries no catalog id is resolved through its own server record
/// (`client.metadata(rk or show_rk).guid`): the menu opened first omits the row and asks, the next
/// one offers it, and its press ends in the write for the RESOLVED id.
#[test]
fn a_card_row_with_no_guid_resolves_through_its_record_and_the_press_writes_it() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    let sid = ServerId::from_raw(1);
    let hex = [7u8; 12];
    let show_hex = [8u8; 12];
    script.records.lock().unwrap().insert((sid, "11".into()), format!("plex://movie/{}", encode_hex(&hex)));
    script.records.lock().unwrap().insert((sid, "5".into()), format!("plex://show/{}", encode_hex(&show_hex)));
    script.records.lock().unwrap().insert((sid, "12".into()), "local://12".into());
    let (mut d, mut rig) = rig_over(&script);
    read_list(&mut d, &mut rig);
    let movie = plx_data::pms::PmsMovie { sid, rk: "11".into(), kind: 0, ..Default::default() };
    let episode = plx_data::pms::PmsMovie { sid, rk: "99".into(), show_rk: "5".into(), kind: 3, ..Default::default() };
    let local = plx_data::pms::PmsMovie { sid, rk: "12".into(), kind: 0, ..Default::default() };
    for item in [&movie, &episode, &local] {
        assert!(rig.watchlist_card_face(item).is_none(), "nothing is known until the record is read");
    }
    settle(&mut d, &mut rig, 10);
    assert_eq!(script.record_reads.load(Ordering::SeqCst), 3, "one record read per title, the episode's SHOW record");
    let face = rig.watchlist_card_face(&movie).expect("the movie now offers the row");
    assert_eq!((face.hex, face.kind, face.from_episode), (hex, Kind::Movie, false));
    let face_ep = rig.watchlist_card_face(&episode).expect("the episode offers its show");
    assert_eq!((face_ep.hex, face_ep.kind, face_ep.from_episode), (show_hex, Kind::Show, true));
    assert!(rig.watchlist_card_face(&local).is_none(), "a record with no portable id offers no row");
    settle(&mut d, &mut rig, 20);
    assert_eq!(script.record_reads.load(Ordering::SeqCst), 3, "an answered title is not asked again");
    // The press: the menu's row is Add for the resolved id, and the write goes out for it.
    let (_, action, _) = plx_screens::item_menu::watchlist::face_row(&face);
    assert_eq!(action, plx_screens::item_menu::Action::WatchlistAdd { hex });
    rig.watchlist_add(hex);
    settle(&mut d, &mut rig, 30);
    assert_eq!(*script.writes.lock().unwrap(), vec![(hex, Intent::Add)]);
    // A row that already carries its id costs no record read.
    let carried = plx_data::pms::PmsMovie { sid, rk: "11".into(), kind: 0, guid: Some([3; 12]), ..Default::default() };
    assert_eq!(rig.watchlist_card_face(&carried).map(|f| f.hex), Some([3; 12]));
    assert_eq!(script.record_reads.load(Ordering::SeqCst), 3);
}

/// Only a Remove made ON the Watchlist page asks it to play its removal landing. From a Detail
/// pushed over a retained page it does not, so a title added back before the page is uncovered is
/// never faded out as removed.
#[test]
fn a_remove_from_another_page_leaves_the_watchlist_page_no_removal_to_replay() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1, 2]);
    let (mut d, mut rig) = rig_over(&script);
    super::nav_tab(&mut d, &mut rig, HomeTab::Watchlist, None, None);
    super::frame(&mut d, &mut rig, tick(1), vec![]);
    settle(&mut d, &mut rig, 2);
    assert!(Bridge::watchlist_page_on_top(&d), "the page is what the user is on");
    assert!(rig.watchlist_remove(&guid(2), Bridge::watchlist_page_on_top(&d)));
    assert_eq!(rig.removal_owed_for_test(), Some(guid(2).as_str()), "on the page: its landing is owed");
    settle(&mut d, &mut rig, 10);
    super::open_catalog(&mut d, [2; 12], None);
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(30 + i), vec![]);
    }
    assert!(!Bridge::watchlist_page_on_top(&d), "a page over the retained list is not the list");
    rig.watchlist_add([2; 12]);
    assert!(rig.watchlist_remove(&guid(2), Bridge::watchlist_page_on_top(&d)));
    assert_eq!(rig.removal_owed_for_test(), None, "from over the page: it is told nothing");
}

/// The item menu's Watchlist rows, as the app receives them (`input::apply_item_action`): Add
/// queues the write and pulses the tab, Remove queues the removal without a pulse, and Go to opens
/// the catalog page by the identity the menu captured (a guid with no catalog id opens nothing).
#[test]
fn the_item_menu_watchlist_arms_queue_the_write_pulse_and_route() {
    use plx_screens::item_menu::Action;
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&[1]);
    let (mut d, mut rig) = rig_over(&script);
    super::nav_tab(&mut d, &mut rig, HomeTab::Watchlist, None, None);
    super::frame(&mut d, &mut rig, tick(1), vec![]);
    settle(&mut d, &mut rig, 2);
    let key = plx_ui::widgets::TAB_KEY_WATCHLIST;
    let at = rig.chrome.keys.iter().position(|k| *k == key).expect("the mark is on the strip");
    let mut ps = plx_media::route::PlaybackSession::default();
    let mt = unsafe { plx_base::task::MainThread::assume() };
    let mut pa = plx_media::player::adapter::PlayerAdapter::new(mt);
    let mut await_play = None;
    let mut apply = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, act: Action| unsafe {
        crate::app::input::apply_item_action(
            &mut ps, &mut pa,
            plx_screens::registry::ItemMenuReq { act, sid: ServerId::UNSET, item: None, season: None,
                loaded_episode: false, from_home: false },
            d, rig, &mut crate::app::playback::LivePlaybackResources, &mut await_play, 0,
        );
    };
    apply(&mut d, &mut rig, Action::WatchlistAdd { hex: [7; 12] });
    assert!(member(&mut rig, 7), "the Add row queued the write (the face flipped at the press)");
    assert_eq!(rig.chrome.take_pulse(), Some(at), "and the Watchlist mark pulsed");
    settle(&mut d, &mut rig, 10);
    assert!(script.writes.lock().unwrap().contains(&([7u8; 12], Intent::Add)));
    apply(&mut d, &mut rig, Action::WatchlistRemove(guid(7)));
    assert!(!member(&mut rig, 7), "the Remove row flipped the tile off");
    assert_eq!(rig.chrome.take_pulse(), None, "a removal does not pulse");
    settle(&mut d, &mut rig, 20);
    assert!(script.writes.lock().unwrap().contains(&([7u8; 12], Intent::Remove)));
    apply(&mut d, &mut rig, Action::GoToCatalog("plex://movie/not-a-hex".into()));
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(100 + i), vec![]);
    }
    assert!(d.top_arg() == Some(&AppArg::Watchlist), "a guid with no catalog id opens nothing");
    apply(&mut d, &mut rig, Action::GoToCatalog(guid(3)));
    for i in 0..40 {
        super::frame(&mut d, &mut rig, tick(200 + i), vec![]);
    }
    assert!(d.top_arg() == Some(&AppArg::Content(ContentArg::Catalog { hex: [3; 12] })), "Go to opens the catalog page");
}

/// **The lookup row's facts reach the focused caption** (audit N5): a title a reachable server holds
/// is worded by its COPY under the Under posters setting, as every poster is, through the whole
/// chain on a real `Dispatcher<AppHost>`: the guid lookup's row, the store's match table, the feed,
/// the page's cell, read off the frame the page DRAWS. The scripted plex.tv sends no year, so the
/// default reads the length alone.
#[test]
fn a_matched_titles_caption_reads_its_copys_facts_under_every_details_pick() {
    use super::test_support::World;
    use super::under_posters_shelf_route_tests::{set, Cell};
    use plx_plex::plex::session::UnderPostersDetails as Details;
    let script = Script::new(&[1]);
    *script.roster.lock().unwrap() = vec![ServerId::from_raw(1)];
    script.up.store(true, Ordering::SeqCst);
    script.holds.store(true, Ordering::SeqCst);
    *script.copy_facts.lock().unwrap() = ListingFacts {
        genre: Some("Drama".into()),
        video_resolution: Some("1080".into()),
        hdr: true,
        content_rating: Some("PG-13".into()),
        audience_rating: Some(7.8),
        duration_ms: Some(100 * 60_000),
        ..Default::default()
    };
    let mut w = World::watchlist_over("watchlist-matched-facts", script);
    let _cell = Cell::default(); // after the world, which holds the process-global lock the setting needs
    assert_eq!(w.bridge.watchlist_for_test().view().tile_state_of(&[1; 12]), Some(TileState::Available),
        "premise: a reachable server holds the title");
    // the focused poster's block as the frame DREW it (not the production label): Down brings focus onto
    // the one title, then each pick words its details line alone (the Title switch is off)
    set(&mut w, false, Details::YearLength);
    for _ in 0..4 {
        if !w.drawn().caption_lines().is_empty() { break; }
        w.press(Key::Down);
    }
    for (details, want) in [
        (Details::YearLength, "1h 40m"),
        (Details::Genre, "Drama"),
        (Details::Video, "1080p \u{b7} HDR"),
        (Details::AgeRatingScore, "PG-13 \u{b7} 7.8"),
    ] {
        set(&mut w, false, details);
        assert_eq!(w.drawn().caption_lines(), [want.to_string()], "{details:?}");
    }
}
