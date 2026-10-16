//! The Watchlist's application wiring (W45D): the strip tab and its page, the store feeding the
//! page, the remove flow, and the owner defaults the wiring decides (Q2 no token, Q8 an
//! unreachable plex.tv reverts the flip with no toast, Q11 a refused write reverts the same way).
//!
//! The store runs over a scripted [`WatchlistIo`] with an inline worker runner, so a landing is a
//! pump away and nothing here reaches a network or a real account.

use super::*;
use super::test_support::tick;
use plx_data::stores::watchlist::WatchlistStore;
use plx_data::watchlist::ledger::Intent;
use plx_data::watchlist::model::Reach;
use plx_data::watchlist::{encode_hex, WatchlistAuth, WatchlistIo};
use plx_plex::plex::watchlist::{WatchItem, WatchKind, WatchPage, WatchlistError};
use plx_plex::plex::{GuidKind, GuidLookupError, GuidMatch, ServerId};
use plx_screens::watchlist::options::{ShowFilter, WatchlistOptionsState};
use plx_screens::watchlist::page::{ListState, WatchlistScreen};
use plx_screens::watchlist::readout::Readout;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

fn guid(n: u8) -> String {
    format!("plex://movie/{}", encode_hex(&[n; 12]))
}

/// A scripted plex.tv: a fixed list, writes recorded and optionally refused (offline, or a dev
/// build without the discover stand-in: both are `Err` to the store).
struct Script {
    items: Vec<WatchItem>,
    token: bool,
    fail_writes: AtomicBool,
    fail_reads: AtomicBool,
    reads: std::sync::atomic::AtomicU32,
    server: Option<ServerId>,
    writes: Mutex<Vec<([u8; 12], Intent)>>,
}

impl Script {
    fn new(titles: &[(u8, &str)], token: bool) -> Arc<Script> {
        Arc::new(Script {
            items: titles
                .iter()
                .map(|(n, title)| WatchItem {
                    hex: encode_hex(&[*n; 12]),
                    guid: guid(*n),
                    kind: Some(WatchKind::Movie),
                    title: Some((*title).into()),
                    added_at: Some(1_000 - i64::from(*n)),
                    ..Default::default()
                })
                .collect(),
            token,
            fail_writes: AtomicBool::new(false),
            fail_reads: AtomicBool::new(false),
            reads: Default::default(),
            server: None,
            writes: Mutex::new(Vec::new()),
        })
    }
}

impl Reach for Script {
    fn reachable(&self, s: ServerId) -> bool {
        self.server == Some(s)
    }
    fn roster(&self) -> Vec<ServerId> {
        self.server.into_iter().collect()
    }
}

impl WatchlistIo for Script {
    fn capture_auth(&self) -> Option<WatchlistAuth> {
        self.token.then(|| WatchlistAuth {
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
        self.reads.fetch_add(1, Ordering::SeqCst);
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
        let hex = plx_data::watchlist::decode_hex(hex).expect("a decodable id");
        self.writes.lock().unwrap().push((hex, intent));
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

fn inline(_: &str, f: Box<dyn FnOnce() + Send + 'static>) -> bool {
    f();
    true
}

/// A rig on Home with the store over `script`, and the Watchlist tab pressed and mounted.
fn mounted(script: &Arc<Script>) -> (Dispatcher<AppHost>, Bridge) {
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    *rig.watchlist_for_test() = WatchlistStore::with_io(script.clone()).with_spawn_for_test(inline);
    super::nav_root(&mut d, AppArg::Home);
    super::frame(&mut d, &mut rig, tick(0), vec![]);
    super::nav_tab(&mut d, &mut rig, HomeTab::Watchlist, None, None);
    super::frame(&mut d, &mut rig, tick(1), vec![]);
    (d, rig)
}

/// A few loop iterations of Watchlist work: land what the workers brought, then feed the page.
fn settle(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, from: u32) {
    for i in 0..6 {
        rig.watchlist_for_test().pump();
        rig.watchlist_feed(d);
        super::frame(d, rig, tick(from + i), vec![]);
    }
}

fn page(d: &Dispatcher<AppHost>) -> &WatchlistScreen {
    d.top_screen()
        .expect("a page is up")
        .as_any()
        .and_then(|s| s.downcast_ref::<WatchlistScreen>())
        .expect("the Watchlist page is the top page")
}

fn guids(d: &Dispatcher<AppHost>) -> Vec<String> {
    page(d).tiles().iter().map(|t| t.row.guid.clone()).collect()
}

const THREE: [(u8, &str); 3] = [(1, "First"), (2, "Second"), (3, "Third")];

#[test]
fn the_strip_press_mounts_the_page_and_the_strip_carries_the_bookmark() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (d, _rig) = mounted(&script);
    assert!(d.top_arg() == Some(&AppArg::Watchlist), "the Watchlist page is on top");
    assert_eq!(super::super::words::route_word(&AppArg::Watchlist), "watchlist");
    assert_eq!(d.top_screen().unwrap().name(), "watchlist");
    let keys: Vec<u32> = d.nav.tabs.strip.iter().map(|member| member.elem).collect();
    let at = |key| keys.iter().position(|k| *k == key).unwrap_or_else(|| panic!("{key} is on the strip: {keys:?}"));
    assert!(at(plx_ui::widgets::TAB_KEY_WATCHLIST) < at(plx_ui::widgets::TAB_KEY_SEARCH),
        "the bookmark sits before Search");
}

#[test]
fn a_mounted_page_opens_the_store_and_is_fed_the_ordered_list() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    assert_eq!(page(&d).list(), ListState::Loading, "nothing has landed when the page mounts");
    settle(&mut d, &mut rig, 2);
    // Date Added, newest first: the script's `added_at` falls as the number rises.
    assert_eq!(guids(&d), vec![guid(1), guid(2), guid(3)]);
    assert_eq!(page(&d).list(), ListState::Ready { total: 3 });
    assert_eq!(page(&d).count(), Some(3), "Q7: the count beside the title");
}

#[test]
fn the_sort_and_filter_value_reaches_the_store_and_the_page_follows() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let options = WatchlistOptionsState { show: ShowFilter::OnYourServers, ..Default::default() };
    rig.watchlist_set_options(&options);
    settle(&mut d, &mut rig, 10);
    assert!(guids(&d).is_empty(), "no server holds anything, so 'on your servers' is empty");
    assert_eq!(page(&d).options(), &options);
    assert_eq!(page(&d).list(), ListState::Ready { total: 3 },
        "the list is still three: 'nothing matches', not 'empty'");
    assert_eq!(page(&d).readout(), Some(Readout::Filtered));
}

#[test]
fn the_hold_menus_remove_flips_the_tile_and_writes_the_remove() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert!(rig.watchlist_remove(&guid(2)));
    settle(&mut d, &mut rig, 10);
    assert_eq!(guids(&d), vec![guid(1), guid(3)], "the tile left the page");
    assert_eq!(*script.writes.lock().unwrap(), vec![([2u8; 12], Intent::Remove)]);
    assert_eq!(page(&d).list(), ListState::Ready { total: 2 });
}

/// Q8 and Q11: the write fails (plex.tv unreachable, or a debug build whose discover stand-in is not
/// armed refusing it). The tile went at the press and comes back when the write answers; nothing
/// else is drawn.
#[test]
fn a_write_that_fails_puts_the_tile_back_with_nothing_said() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    script.fail_writes.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert!(rig.watchlist_remove(&guid(2)));
    settle(&mut d, &mut rig, 10);
    assert_eq!(*script.writes.lock().unwrap(), vec![([2u8; 12], Intent::Remove)], "the write was tried");
    assert_eq!(guids(&d), vec![guid(1), guid(2), guid(3)], "and the flip reverted");
    assert_eq!(page(&d).list(), ListState::Ready { total: 3 });
    assert_eq!(page(&d).readout(), None, "no read-out and no toast");
}

/// Q2: a profile with no plex.tv token has no list to read.
#[test]
fn a_profile_without_a_token_reads_out_that_the_watchlist_is_unavailable() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, false);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert_eq!(page(&d).list(), ListState::Unavailable);
    assert_eq!(page(&d).readout(), Some(Readout::NoToken));
    assert!(guids(&d).is_empty());
}

#[test]
fn leaving_the_page_and_coming_back_shows_the_list_again() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert_eq!(rig.held_keys(&AppArg::Watchlist).len(), 0, "nothing matched, so nothing to read back");
    super::nav_tab(&mut d, &mut rig, HomeTab::Search, None, None);
    settle(&mut d, &mut rig, 20);
    assert!(d.top_arg() != Some(&AppArg::Watchlist), "Search replaced it");
    super::nav_tab(&mut d, &mut rig, HomeTab::Watchlist, None, None);
    settle(&mut d, &mut rig, 30);
    assert_eq!(guids(&d), vec![guid(1), guid(2), guid(3)], "the list is shown again, not blanked");
}

#[test]
fn a_hold_names_its_tile_by_guid_and_opens_the_catalog_menu() {
    use plx_data::watchlist::model::TileState;
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let entry = d.nav.top_page().unwrap().id;
    let arg = rig.watchlist_hold_arg(&mut d, entry, None, &guid(2), TileState::ComingSoon, None)
        .expect("the held tile is on the page");
    assert!(matches!(arg.kind, ItemMenuKind::Catalog { state: TileState::ComingSoon }));
    assert_eq!(arg.catalog.as_ref().map(|c| c.guid.clone()), Some(guid(2)));
    assert!(arg.rk.is_empty() && !arg.sid.is_set(), "an unmatched title names no server item");
    // A title the page no longer shows opens nothing.
    assert!(rig.watchlist_hold_arg(&mut d, entry, None, &guid(9), TileState::ComingSoon, None).is_none());
}

/// A first read that fails is a read-out with Try again, not a blank page; Try again reads again.
#[test]
fn a_failed_first_read_reads_out_and_try_again_reads_the_list() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    script.fail_reads.store(true, Ordering::SeqCst);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert_eq!(page(&d).list(), ListState::Failed);
    assert_eq!(page(&d).readout(), Some(Readout::Unreachable));
    assert!(guids(&d).is_empty());
    let before = script.reads.load(Ordering::SeqCst);
    script.fail_reads.store(false, Ordering::SeqCst);
    rig.watchlist_retry();
    settle(&mut d, &mut rig, 10);
    assert!(script.reads.load(Ordering::SeqCst) > before, "Try again read the list again");
    assert_eq!(guids(&d), vec![guid(1), guid(2), guid(3)]);
    assert_eq!(page(&d).list(), ListState::Ready { total: 3 });
}

/// An unmatched title's poster goes through a reachable server's photo proxy (Q9); with none
/// reachable it stays a skeleton.
#[test]
fn an_unmatched_title_gets_its_poster_through_a_reachable_server() {
    let _serial = plx_base::testlock::serial();
    let mut script = Script::new(&THREE, true);
    Arc::get_mut(&mut script).unwrap().items[0].thumb = Some("https://image.example/one.jpg".into());
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert_eq!(page(&d).art_of(0), Some((String::new(), 0)), "no server reachable: the skeleton");

    let mut with_server = Script::new(&THREE, true);
    {
        let w = Arc::get_mut(&mut with_server).unwrap();
        w.items[0].thumb = Some("https://image.example/one.jpg".into());
        w.server = Some(ServerId::from_raw(1));
    }
    let (mut d, mut rig) = mounted(&with_server);
    settle(&mut d, &mut rig, 2);
    assert_eq!(page(&d).art_of(0), Some(("https://image.example/one.jpg".to_string(), 1)));
}
