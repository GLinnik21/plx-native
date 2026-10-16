//! The Watchlist press paths (plan §1.8, §1.9, W6): every route into a write, the §1.9 state ×
//! press table, a plex.tv that cannot be reached, and the tab pulse.
//!
//! Like the wiring tests, the store runs over a scripted [`WatchlistIo`] with an inline worker
//! runner and the catalog store over a scripted [`CatalogIo`]: a landing is a pump away and nothing
//! here reaches a network or a real account.

use super::*;
use super::test_support::tick;
use plx_data::catalog::{CatalogIo, Load, Membership};
use plx_data::stores::catalog::CatalogStore;
use plx_data::stores::watchlist::WatchlistStore;
use plx_data::watchlist::ledger::Intent;
use plx_data::watchlist::model::{Kind, Reach, ServerMatch, TileState};
use plx_data::watchlist::{decode_hex, encode_hex, WatchlistAuth, WatchlistIo};
use plx_plex::plex::watchlist::{CatalogItem, WatchItem, WatchKind, WatchPage, WatchlistError};
use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch, ServerId};
use plx_screens::catalog::CatalogScreen;
use plx_screens::registry::ContentArg;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn guid(n: u8) -> String {
    format!("plex://movie/{}", encode_hex(&[n; 12]))
}

/// A scripted plex.tv: a fixed list; reads and writes can be refused (offline).
struct Script {
    items: Vec<WatchItem>,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
    writes: Mutex<Vec<([u8; 12], Intent)>>,
}

impl Script {
    fn new(titles: &[u8]) -> Arc<Script> {
        Arc::new(Script {
            items: titles
                .iter()
                .map(|n| WatchItem {
                    hex: encode_hex(&[*n; 12]),
                    guid: guid(*n),
                    kind: Some(WatchKind::Movie),
                    title: Some(format!("Title {n}")),
                    added_at: Some(1_000 - i64::from(*n)),
                    ..Default::default()
                })
                .collect(),
            fail_writes: AtomicBool::new(false),
            fail_reads: AtomicBool::new(false),
            writes: Mutex::new(Vec::new()),
        })
    }
}

impl Reach for Script {
    fn reachable(&self, _: ServerId) -> bool {
        false
    }
    fn roster(&self) -> Vec<ServerId> {
        Vec::new()
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
    fn page(&self, _: &WatchlistAuth, start: u32, size: u32) -> Result<WatchPage, WatchlistError> {
        if self.fail_reads.load(Ordering::SeqCst) {
            return Err(WatchlistError::Transport);
        }
        let items: Vec<WatchItem> = self.items.iter().skip(start as usize).take(size as usize).cloned().collect();
        let raw = items.len() as u32;
        Ok(WatchPage { items, total: Some(self.items.len() as u32), start, raw })
    }
    fn released(&self, _: &WatchlistAuth) -> Result<Vec<WatchItem>, WatchlistError> {
        Ok(self.items.clone())
    }
    fn write(&self, _: &WatchlistAuth, hex: &str, intent: Intent) -> Result<(), WatchlistError> {
        self.writes.lock().unwrap().push((decode_hex(hex).expect("a decodable id"), intent));
        if self.fail_writes.load(Ordering::SeqCst) { Err(WatchlistError::Transport) } else { Ok(()) }
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

/// A rig on Home, the Watchlist store over `script` and the catalog store over [`Discover`].
fn rig_over(script: &Arc<Script>) -> (Dispatcher<AppHost>, Bridge) {
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    *rig.watchlist_for_test() = WatchlistStore::with_io(script.clone()).with_spawn_for_test(inline);
    rig.stores.catalog = CatalogStore::with_io(Arc::new(Discover)).with_spawn_for_test(inline);
    super::nav_root(&mut d, AppArg::Home);
    super::frame(&mut d, &mut rig, tick(0), vec![]);
    (d, rig)
}

/// Land what the workers brought and feed the pages, a few frames.
fn settle(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, from: u32) {
    for i in 0..6 {
        rig.watchlist_for_test().pump();
        rig.watchlist_feed(d);
        rig.catalog_feed(d);
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
