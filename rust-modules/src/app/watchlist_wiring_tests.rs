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
use plx_screens::watchlist::header::Count;
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
    fn page(&self, _: &WatchlistAuth, start: u32, size: u32, _: Option<&str>) -> Result<WatchPage, WatchlistError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
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

/// **The tab's arrival seats in the page, not on the strip.** The page mounts empty (its rows are
/// fed after it), so the mint's group 0 has nothing to seat; the fallback must take the page's own
/// header, never the strip's left end (the account chip, where it used to land).
#[test]
fn the_tab_arrival_seats_in_the_page_not_on_the_strip() {
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let page = d.nav.top_page().map(|e| e.id);
    let seat = d.focus();
    assert!(seat.is_some_and(|k| Some(k.entry) == page && k.elem < plx_ui::dispatch::STRIP_BASE),
        "the arrival seats in the page: {seat:?}");
}

/// **The arrival seat follows the tiles in, while it is still the page's.** Before the rows land the
/// header's pill holds it; the first landing moves it to the first tile, as the tab mint asked.
#[test]
fn the_arrival_seat_moves_to_the_first_tile_when_the_rows_land() {
    use plx_screens::watchlist::page::PILL_ELEM;
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    assert_eq!(d.focus().map(|k| k.elem), Some(PILL_ELEM), "nothing landed yet: the pill holds the seat");
    settle(&mut d, &mut rig, 2);
    assert_eq!(d.focus().map(|k| k.elem), page(&d).elem_at(0), "the rows landed: the first tile holds it");
}

/// **A move made before the rows land is the reader's.** The landing that follows does not take
/// focus back to the grid.
#[test]
fn a_key_before_the_rows_land_keeps_the_readers_choice() {
    use plx_machine::machine::Key;
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    super::frame(&mut d, &mut rig, tick(2), super::script_key(Key::Up, tick(2)));
    super::frame(&mut d, &mut rig, tick(3), vec![]);
    let chosen = d.focus();
    assert!(chosen.is_some_and(|k| k.elem >= plx_ui::dispatch::STRIP_BASE), "UP from the pill reaches the strip: {chosen:?}");
    settle(&mut d, &mut rig, 4);
    assert!(!guids(&d).is_empty(), "the rows landed");
    assert_eq!(d.focus(), chosen, "the landing left the reader's choice alone");
}

/// **A key that gets in before the page's own seat `Enter` keeps its move.** The page seats its
/// arrival with a fresh `Enter` addressed to itself (`follow_seat`), queued at the FIFO tail when a
/// Tick finds the rows landed. When the drain's budget carries that Tick over a frame, the reader's
/// key of the next frame is queued behind the Tick but AHEAD of the Enter the Tick emits: the key
/// claims the seat, and the Enter that follows used to seat the first tile anyway, taking the
/// reader's move back.
#[test]
fn a_key_ahead_of_the_pages_deferred_seat_enter_keeps_the_readers_move() {
    use plx_machine::machine::{Delivery, Fx, Key, MachineId, Stamped};
    use plx_ui::dispatch::{MAX_STEPS_POST, MAX_STEPS_PRE};
    use plx_ui::screen::ScreenEvent;
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    assert_eq!(d.focus().map(|k| k.elem), Some(plx_screens::watchlist::page::PILL_ELEM),
        "control: nothing landed yet, the pill holds the page's seat");
    let inst = d.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id).expect("the page is mounted");
    // Land the rows (the page's cells are fed), then spend both budgets of the next frame on
    // no-op deliveries queued AHEAD of its Tick, so that Tick is carried to the frame after.
    for _ in 0..3 {
        rig.watchlist_feed(&mut d);
        rig.watchlist_for_test().pump();
    }
    rig.watchlist_feed(&mut d);
    assert!(!guids(&d).is_empty(), "control: the rows are in the page before its next Tick");
    d.absorb((0..MAX_STEPS_PRE + MAX_STEPS_POST + 10).map(|_| Stamped {
        from: MachineId::Nav,
        fx: Fx::Deliver(MachineId::Instance(inst), Delivery::Screen(ScreenEvent::Uncover)),
    }).collect());
    let (_, r) = super::frame(&mut d, &mut rig, tick(2), vec![]);
    assert!(r.carried > 0, "control: the frame's Tick is carried over");
    // The reader's key meets the carried Tick, which now emits the seat Enter behind the key.
    super::frame(&mut d, &mut rig, tick(3), super::script_key(Key::Up, tick(3)));
    for i in 4..8 {
        super::frame(&mut d, &mut rig, tick(i), vec![]);
    }
    assert!(!guids(&d).is_empty(), "the rows landed");
    let held = d.focus();
    assert!(held.is_some_and(|k| k.elem >= plx_ui::dispatch::STRIP_BASE),
        "UP from the pill reached the strip and the page's late seat Enter left it there: {held:?}");
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
    assert_eq!(page(&d).count(), Some(Count::All(3)), "Q7: the count beside the pill");
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
    assert!(rig.watchlist_remove(&guid(2), true));
    settle(&mut d, &mut rig, 10);
    assert_eq!(guids(&d), vec![guid(1), guid(3)], "the tile left the page");
    assert_eq!(*script.writes.lock().unwrap(), vec![([2u8; 12], Intent::Remove)]);
    assert_eq!(page(&d).list(), ListState::Ready { total: 2 });
}

/// **The page does not move while a menu is up.** A Remove the hold menu chose is in the store at
/// once, but the page is handed nothing (no tile gone, no count, no invalidation to redraw the host
/// snapshot) for as long as the menu veils it, closing tail included; the first feed after the
/// menu has let go commits the Remove.
#[test]
fn the_feed_hands_the_page_nothing_while_a_menu_holds_it() {
    use plx_ui::containers::modal::{set_veil, CLOSING_GONE};
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert!(rig.watchlist_remove(&guid(2), true));
    for veil in [1.0, 0.5, CLOSING_GONE * 1.5] {
        set_veil(veil);
        rig.watchlist_for_test().pump();
        let _ = plx_machine::idle::take_local_damage();
        rig.watchlist_feed(&mut d);
        assert_eq!(guids(&d), vec![guid(1), guid(2), guid(3)], "veil {veil}: the page is as the menu found it");
        assert_eq!(page(&d).count(), Some(Count::All(3)), "veil {veil}: and so is its count");
        assert_eq!(plx_machine::idle::take_local_damage(), 0, "veil {veil}: nothing invalidated the host");
    }
    set_veil(CLOSING_GONE * 0.5);
    rig.watchlist_for_test().pump();
    rig.watchlist_feed(&mut d);
    assert_eq!(guids(&d), vec![guid(1), guid(3)], "the first feed after the menu let go commits the Remove");
    set_veil(0.0);
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
    assert!(rig.watchlist_remove(&guid(2), true));
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

/// Plan §1.3 / §1.11 B: a resume while the Watchlist is live reads the plex.tv list again (which
/// re-matches it), where before only the 2 s write timer and Try again did.
#[test]
fn a_resume_with_the_page_live_reads_the_list_again() {
    use super::super::reread::{self, Reread};
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let mut reread = Reread::default();
    let before = script.reads.load(Ordering::SeqCst);
    for _ in 0..3 {
        reread::run(&mut reread, &mut d, &mut rig);
        settle(&mut d, &mut rig, 10);
    }
    assert_eq!(script.reads.load(Ordering::SeqCst), before, "nothing is read before a resume");
    reread.resume();
    reread::run(&mut reread, &mut d, &mut rig);
    settle(&mut d, &mut rig, 20);
    assert!(script.reads.load(Ordering::SeqCst) > before, "the resume read the list again");
    assert_eq!(guids(&d), vec![guid(1), guid(2), guid(3)], "and the page still shows it");
}

/// The hold window follows the page's cursor, a reserved cell past the loaded tail included: the
/// focus read there is no title, and reading it as "none" sent the window back to the head.
#[test]
fn the_hold_window_follows_the_cursor_onto_a_reserved_cell() {
    let _serial = plx_base::testlock::serial();
    let titles: Vec<(u8, String)> = (1..=12).map(|n| (n, format!("Title {n}"))).collect();
    let refs: Vec<(u8, &str)> = titles.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let script = Script::new(&refs, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    assert_eq!(page(&d).tiles().len(), 12);
    // The service says more are coming than have landed: cells 12.. are reserved.
    {
        let id = d.nav.tabs.stack.entries.iter().find(|e| e.arg == AppArg::Watchlist).unwrap().id;
        let screen = d.nav.entry_mut(id).unwrap().inst.as_mut().unwrap().screen.as_any_mut().unwrap()
            .downcast_mut::<WatchlistScreen>().unwrap();
        screen.input(plx_screens::watchlist::page::WatchlistIn::List(ListState::Ready { total: 250 }));
    }
    let mut t = 20;
    while page(&d).cursor() < 12 && t < 60 {
        super::frame(&mut d, &mut rig, tick(t), super::script_key(Key::Down, tick(t)));
        super::frame(&mut d, &mut rig, tick(t + 1), vec![]);
        t += 2;
    }
    let cursor = page(&d).cursor();
    assert!(cursor >= 12, "DOWN reached a reserved cell (cursor {cursor})");
    assert_eq!(page(&d).focused(), None, "a reserved cell is no title");
    rig.watchlist_feed(&mut d);
    assert_eq!(rig.watchlist_window_for_test(), Some(cursor.saturating_sub(18)..cursor + 36),
        "the window is centred on the cursor, not the head");
}

/// Plan §1.9: a playable title's hold menu is the PMS card menu of its copy (sid, rk) plus Remove;
/// a title with no reachable copy keeps the catalog rows.
#[test]
fn a_playable_titles_hold_is_the_card_menu_of_its_copy() {
    use plx_data::watchlist::model::{ServerMatch, TileState};
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let entry = d.nav.top_page().unwrap().id;
    let copy = ServerMatch { server_id: ServerId::from_raw(1), rating_key: 42, blur: None };
    for state in [TileState::Available, TileState::ComingSoonMatched] {
        let arg = rig.watchlist_hold_arg(&mut d, entry, None, &guid(2), state, Some(copy)).expect("on the page");
        match &arg.kind {
            ItemMenuKind::Card { row, from_deck } => {
                assert!(!from_deck);
                assert_eq!((row.sid, row.rk.as_str(), row.kind), (ServerId::from_raw(1), "42", 0), "{state:?}");
            }
            _ => panic!("{state:?}: a playable title is a card of its copy"),
        }
        assert_eq!(arg.catalog.as_ref().map(|c| c.guid.clone()), Some(guid(2)), "Remove names the guid");
    }
    let arg = rig.watchlist_hold_arg(&mut d, entry, None, &guid(2), TileState::NotOnAnyServer, None).unwrap();
    assert!(matches!(arg.kind, ItemMenuKind::Catalog { .. }), "no copy: the catalog rows");
}

/// Plan §1.9: a Catalog hold menu has sid UNSET and an empty rk, even when the tile still knows a
/// (cached) copy, as a ServerOffline tile does.
#[test]
fn a_catalog_hold_carries_no_pms_identity_even_with_a_cached_copy() {
    use plx_data::watchlist::model::{ServerMatch, TileState};
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let entry = d.nav.top_page().unwrap().id;
    let copy = ServerMatch { server_id: ServerId::from_raw(1), rating_key: 42, blur: None };
    for state in [TileState::ServerOffline, TileState::Unknown, TileState::ComingSoon, TileState::NotOnAnyServer] {
        let arg = rig.watchlist_hold_arg(&mut d, entry, None, &guid(2), state, Some(copy)).expect("on the page");
        assert!(matches!(arg.kind, ItemMenuKind::Catalog { .. }), "{state:?}: the catalog rows");
        assert_eq!(arg.sid, ServerId::UNSET, "{state:?}: no server");
        assert!(arg.rk.is_empty(), "{state:?}: no rating key, got {:?}", arg.rk);
    }
    let arg = rig.watchlist_hold_arg(&mut d, entry, None, &guid(2), TileState::Available, Some(copy)).unwrap();
    assert_eq!((arg.sid, arg.rk.as_str()), (ServerId::from_raw(1), "42"), "a card keeps its copy");
}

/// **The remote's OK on a strip pill leaves the Watchlist** (owner, on the TV: "I can't leave
/// Watchlist"). Real key edges through the dispatcher: the strip is a `Control` group, so OK arms a
/// press and the page hears `PressCommit` with the pill focused. The page used to answer only a
/// card's commit, so every strip press died in it and only BACK left.
#[test]
fn ok_on_a_strip_pill_asks_to_leave_the_page() {
    use plx_machine::machine::Key;
    use plx_screens::watchlist::page::WatchlistOut;
    let _serial = plx_base::testlock::serial();
    let script = Script::new(&THREE, true);
    let (mut d, mut rig) = mounted(&script);
    settle(&mut d, &mut rig, 2);
    let mut i = 40;
    let mut press = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, key: Key| {
        super::frame(d, rig, tick(i), super::script_key(key, tick(i)));
        // Long enough for the press to commit (its dip plays first).
        for n in 1..30 { super::frame(d, rig, tick(i + n), vec![]); }
        i += 30;
    };
    let on_strip = |d: &Dispatcher<AppHost>| d.focus().is_some_and(|k| k.elem >= plx_ui::dispatch::STRIP_BASE);
    for _ in 0..4 { if !on_strip(&d) { press(&mut d, &mut rig, Key::Up); } }
    assert!(on_strip(&d), "UP reaches the strip: {:?}", d.focus());
    let strip = d.nav.tabs.strip.len();
    for _ in 0..strip { press(&mut d, &mut rig, Key::Left); }
    while d.focus().is_some_and(|k| k.elem != plx_ui::widgets::TAB_KEY_HOME) {
        let before = d.focus();
        press(&mut d, &mut rig, Key::Right);
        assert_ne!(d.focus(), before, "the Home pill is on the strip");
    }
    rig.take_watchlist_reqs();
    press(&mut d, &mut rig, Key::Ok);
    let asked: Vec<WatchlistOut> = rig.take_watchlist_reqs().into_iter().map(|(_, out, _)| out).collect();
    assert_eq!(asked, vec![WatchlistOut::Tab(HomeTab::Home)], "OK on the Home pill asks for Home");
}
