//! The notification mapping: which of a stranger's play-state hints become a read of OUR state, and
//! that none of them is ever treated as playback (S5). Observed through the reconcile queue and the
//! effects `Stores::notify_map` reports having spent.

use super::browse::DirectorySnapshot;
use super::notify::{NotifyOut, NotifyPlays, NOTIFY_ENABLED};
use super::notify_map::Shown;
use super::{ContentArg, Stores};
use crate::pms::PmsMovie;
use crate::search::{Item, Kind, Shelf};
use plx_plex::plex::notify::{PlaySession, PlayState};
use plx_plex::plex::ServerId;

const SID: ServerId = ServerId::UNSET;

fn session(key: &str, rk: &str, state: PlayState) -> PlaySession {
    PlaySession {
        session_key: Some(key.into()),
        client_identifier: Some("other".into()),
        rating_key: Some(rk.into()),
        key: None,
        view_offset_ms: Some(1000),
        state,
    }
}

fn plays(sessions: Vec<PlaySession>) -> NotifyOut {
    NotifyOut { plays: vec![NotifyPlays { sid: SID, sessions }], resynced: Vec::new(), ..Default::default() }
}

fn one(key: &str, rk: &str, state: PlayState) -> NotifyOut {
    plays(vec![session(key, rk, state)])
}

/// A store aggregate whose Search page holds `rks`.
fn holding(rks: &[&str]) -> Stores {
    let mut stores = Stores::default();
    stores.search.publish_shelves_for_test(vec![Shelf {
        epoch: 0,
        window: Default::default(),
        kind: Kind::Movie,
        items: rks.iter().map(|rk| Item::Media(PmsMovie { sid: SID, rk: (*rk).into(), ..Default::default() })).collect(),
    }]);
    stores
}

fn map(stores: &mut Stores, out: &NotifyOut, pages: &[ContentArg]) -> super::notify_map::Spent {
    map_shown(stores, out, pages, Shown::ALL)
}

fn map_shown(stores: &mut Stores, out: &NotifyOut, pages: &[ContentArg], shown: Shown) -> super::notify_map::Spent {
    let dir = DirectorySnapshot::default();
    let spent = stores.notify_map(out, pages, shown, dir.view(), &crate::pms::registry_keys_for_test());
    stores.watchlist.dispatch_jobs_for_test();
    spent
}

fn pending(stores: &Stores) -> Vec<String> {
    stores.reconcile.pending_for_test().into_iter().map(|(_, k)| k).collect()
}

fn detail(rk: &str) -> ContentArg {
    ContentArg::Detail { sid: SID, rk: rk.into() }
}

#[test]
fn notifications_ship_on() {
    assert!(NOTIFY_ENABLED, "notifications ship ON; the owned-only rule (S6) is what keeps others dark");
}

#[test]
fn holds_covers_the_visible_and_the_covered_pages() {
    let _g = plx_base::testlock::serial();
    let stores = holding(&["s1"]);
    assert!(stores.holds(SID, "s1", &[], Shown::ALL), "a list page's window");
    assert!(!stores.holds(SID, "other", &[], Shown::ALL));
    assert!(stores.holds(SID, "d1", &[detail("d1")], Shown::ALL), "a Detail page on the stack, top or covered");
    assert!(!stores.holds(SID, "d1", &[], Shown::ALL), "a popped page holds nothing");
}

#[test]
fn unheld_item_no_reread() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let spent = map(&mut stores, &one("1", "zzz", PlayState::Playing), &[]);
    assert!(pending(&stores).is_empty());
    assert_eq!(spent, Default::default());
    let spent = map(&mut stores, &one("1", "zzz", PlayState::Paused), &[]);
    assert!(pending(&stores).is_empty(), "a state change of an item nothing shows");
    assert_eq!(spent, Default::default());
}

#[test]
fn steady_playing_no_reread() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
    assert_eq!(pending(&stores), ["s1"], "a new session on a held item");
    stores.reconcile.reset();
    for _ in 0..5 {
        map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
    }
    assert!(pending(&stores).is_empty(), "the same state again is no news");
}

#[test]
fn state_change_enqueues_once_then_dirty() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
    map(&mut stores, &one("1", "s1", PlayState::Paused), &[]);
    map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
    assert_eq!(pending(&stores), ["s1"], "one queue entry however many changes (a read in flight marks it dirty)");
}

#[test]
fn stopped_held_enqueues_and_quiet_refetch() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let spent = map(&mut stores, &one("1", "s1", PlayState::Stopped), &[]);
    assert_eq!(pending(&stores), ["s1"]);
    assert_eq!(spent.refetched, [SID], "Home's source is refetched quietly");
    assert!(spent.invalidated.is_empty(), "the held item is read back, no whole-section invalidate");
}

#[test]
fn stopped_unheld_coalesced_source_refetch_and_invalidate() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let out = plays(vec![session("1", "a", PlayState::Stopped), session("2", "b", PlayState::Stopped)]);
    let spent = map(&mut stores, &out, &[]);
    assert!(pending(&stores).is_empty(), "no per-item read of an item nothing shows");
    assert_eq!(spent.refetched, [SID], "two stops on one server are one refetch");
    assert_eq!(spent.invalidated, [SID], "that server's section shelves are invalidated, once");
}

#[test]
fn created_or_removed_items_owe_the_servers_grids_one_re_list() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    use crate::stores::notify::NotifyLibrary;
    use plx_plex::plex::notify::{LibraryEntry, LibraryKind, LibraryState};
    let entry = |id: &str, state| LibraryEntry { item_id: id.into(), kind: LibraryKind::Movie, state, section: None };
    let out = NotifyOut {
        library: vec![
            NotifyLibrary { sid: SID, entries: vec![entry("a", LibraryState::Created)] },
            NotifyLibrary { sid: SID, entries: vec![entry("b", LibraryState::Deleted)] },
        ],
        ..Default::default()
    };
    assert_eq!(map(&mut stores, &out, &[]), Default::default(), "nothing is read while the feed may still be busy");
    let spent = after_the_quiet(&mut stores);
    assert_eq!(spent.relisted, [SID], "two reports in one pump are one re-list");
    assert_eq!(spent.refetched, [SID], "and one quiet Home refetch");
    assert_eq!(spent.invalidated, [SID], "and one section-shelf invalidate");
    assert!(map(&mut stores, &NotifyOut::default(), &[]).relisted.is_empty(), "spent once");
}

#[test]
fn owed_work_waits_for_network_release() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    stores.hold_network(true);
    let spent = map(&mut stores, &one("1", "s1", PlayState::Stopped), &[]);
    assert_eq!(spent, Default::default(), "nothing is spent under the player");
    assert!(pending(&stores).is_empty());
    assert!(!stores.notify_owed_for_test().is_empty(), "queued, not dropped");
    map(&mut stores, &NotifyOut::default(), &[]);
    stores.hold_network(false);
    let spent = map(&mut stores, &NotifyOut::default(), &[]);
    assert_eq!(spent.refetched, [SID]);
    assert_eq!(pending(&stores), ["s1"]);
    assert!(stores.notify_owed_for_test().is_empty());
    let again = map(&mut stores, &NotifyOut::default(), &[]);
    assert_eq!(again, Default::default(), "spent once");
}

#[test]
fn owed_work_waits_for_the_landing_gate() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    stores.landgate.arm_recording();
    let spent = map(&mut stores, &one("1", "s1", PlayState::Stopped), &[]);
    assert_eq!(spent, Default::default(), "a recording owns the run: no read it cannot contain");
    stores.landgate.disarm();
    assert!(!map(&mut stores, &NotifyOut::default(), &[]).refetched.is_empty());
}

#[test]
fn notification_never_enqueue_stop_or_candidates_or_offset() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    for state in [PlayState::Playing, PlayState::Paused, PlayState::Stopped] {
        map(&mut stores, &one("1", "s1", state), &[detail("s1")]);
    }
    assert!(stores.reconcile.candidates_for_test().is_empty(), "no end-of-stream candidate");
    assert!(stores.reconcile.take_watched_ends().is_empty(), "nothing to fan out to other servers");
    assert!(!stores.viewstate.borrow().is_busy(), "no view-state write, so no scrobble");
    assert!(stores.viewstate.borrow_mut().take_reread().is_empty());
    assert!(stores.reconcile.take_landed().is_empty(), "and no Offset patch or page debt of its own");
}

#[test]
fn epoch_move_clears_last_state() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
    stores.reconcile.reset();
    let mut out = one("1", "s1", PlayState::Playing);
    map(&mut stores, &out, &[]);
    assert!(pending(&stores).is_empty(), "steady");
    out.resynced.push((SID, 1));
    map(&mut stores, &out, &[]);
    assert_eq!(pending(&stores), ["s1"], "after a gap the same state is news again");
}

#[test]
fn last_state_is_bounded() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    for i in 0..(super::notify_map::MAX_SESSIONS * 3) {
        map(&mut stores, &one(&format!("k{i}"), "zzz", PlayState::Playing), &[]);
    }
    assert!(stores.notify_tracked_for_test() <= super::notify_map::MAX_SESSIONS);
}

#[test]
fn detail_item_stop_routes_via_detail_reconcile() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let pages = [detail("d1")];
    map(&mut stores, &plays(vec![session("1", "d1", PlayState::Stopped), session("2", "s1", PlayState::Stopped)]), &pages);
    assert_eq!(pending(&stores), ["d1", "s1"]);
    assert!(!stores.reconcile.is_quiet_for_test(&(SID, "d1".into())),
        "the Detail's item owes its page (take_landed -> reconcile_details -> DetailReconcile)");
    assert!(stores.reconcile.is_quiet_for_test(&(SID, "s1".into())),
        "a list page's card is patched in place and owes no Detail a re-fetch");
}

#[test]
fn notify_never_mutates_detail_directly() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    let gen = stores.metadata.gen();
    map(&mut stores, &one("1", "d1", PlayState::Stopped), &[detail("d1")]);
    assert_eq!(stores.metadata.gen(), gen, "the Metadata store (the Detail's slot) is untouched");
    assert!(stores.take_landed_views().is_empty(), "only a landed read-back owes the page");
}

#[test]
fn buffering_is_playing_and_other_is_ignored() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
    assert_eq!(pending(&stores), ["s1"], "the first report is news");
    stores.reconcile.reset();
    let mut reads = 0;
    for i in 0..10 {
        let state = if i % 2 == 0 { PlayState::Buffering } else { PlayState::Playing };
        map(&mut stores, &one("1", "s1", state), &[]);
        if i % 3 == 0 {
            map(&mut stores, &one("1", "s1", PlayState::Other), &[]);
        }
        reads += pending(&stores).len();
        stores.reconcile.reset();
    }
    assert_eq!(reads, 0, "a stalled stream flipping playing/buffering/other is the same state: no further read");
    map(&mut stores, &one("1", "s1", PlayState::Paused), &[]);
    assert_eq!(pending(&stores), ["s1"], "a real change after the flips is still news");
}

#[test]
fn a_new_session_reporting_other_first_is_not_tracked() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    map(&mut stores, &one("1", "s1", PlayState::Other), &[]);
    assert!(pending(&stores).is_empty(), "an unknown state names nothing to read");
    assert_eq!(stores.notify_tracked_for_test(), 0);
}

fn n_live_sessions(stores: &mut Stores, from: usize, to: usize) {
    for i in from..to {
        map(stores, &one(&format!("k{i}"), "zzz", PlayState::Playing), &[]);
    }
}

#[test]
fn an_updated_session_moves_to_the_back_and_survives_the_bound() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let max = super::notify_map::MAX_SESSIONS;
    map(&mut stores, &one("k0", "s1", PlayState::Playing), &[]);
    n_live_sessions(&mut stores, 1, max);
    map(&mut stores, &one("k0", "s1", PlayState::Paused), &[]); // the oldest session speaks again
    n_live_sessions(&mut stores, max, max + 1); // one more than fits
    stores.reconcile.reset();
    map(&mut stores, &one("k0", "s1", PlayState::Paused), &[]);
    assert!(pending(&stores).is_empty(), "the session that just spoke was not the one evicted");
}

#[test]
fn a_stopped_session_is_evicted_before_a_live_one() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let max = super::notify_map::MAX_SESSIONS;
    map(&mut stores, &one("live", "s1", PlayState::Playing), &[]);
    map(&mut stores, &one("gone", "s1", PlayState::Stopped), &[]);
    n_live_sessions(&mut stores, 0, max - 2); // exactly full
    assert_eq!(stores.notify_tracked_for_test(), max);
    n_live_sessions(&mut stores, max, max + 1); // one more than fits
    stores.reconcile.reset();
    map(&mut stores, &one("live", "s1", PlayState::Playing), &[]);
    assert!(pending(&stores).is_empty(), "the oldest LIVE session is still known: the stopped one made room");
}

fn sessionless(client: &str, rk: &str, state: PlayState) -> PlaySession {
    PlaySession {
        session_key: None,
        client_identifier: Some(client.into()),
        rating_key: Some(rk.into()),
        key: None,
        view_offset_ms: None,
        state,
    }
}

#[test]
fn two_sessionless_clients_on_one_item_do_not_collide() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let round = || plays(vec![sessionless("a", "s1", PlayState::Paused), sessionless("b", "s1", PlayState::Playing)]);
    map(&mut stores, &round(), &[]);
    assert_eq!(pending(&stores), ["s1"]);
    stores.reconcile.reset();
    for _ in 0..4 {
        map(&mut stores, &round(), &[]);
    }
    assert!(pending(&stores).is_empty(), "each client's steady state is no news, not a flip-flop between them");
}

fn session_lines() -> usize {
    std::fs::read_to_string(plx_base::eventlog::events_log())
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("notify: sessions"))
        .count()
}

#[test]
fn a_steady_playing_stream_logs_its_first_frame_only() {
    let _g = plx_base::testlock::serial();
    plx_base::eventlog::with_private_log(|| {
        let mut stores = holding(&["s1"]);
        for _ in 0..50 {
            map(&mut stores, &one("1", "s1", PlayState::Playing), &[]);
        }
        assert_eq!(session_lines(), 1, "the steady frames after the first are no transition and log nothing");
        map(&mut stores, &one("1", "s1", PlayState::Stopped), &[]);
        assert_eq!(session_lines(), 2, "a transition logs once");
    });
}

#[test]
fn a_popped_list_page_holds_nothing() {
    let _g = plx_base::testlock::serial();
    let stores = holding(&["s1"]);
    let search_only = Shown { search: true, ..Default::default() };
    assert!(stores.holds(SID, "s1", &[], search_only), "Search is on the stack");
    assert!(!stores.holds(SID, "s1", &[], Shown::default()), "Search was popped: its last rows are not held");
    let mut stores = holding(&["s1"]);
    map_shown(&mut stores, &one("1", "s1", PlayState::Stopped), &[], Shown::default());
    assert!(pending(&stores).is_empty(), "no read-back for a page that is gone");
}

const OTHER: ServerId = ServerId::from_raw(7);

fn on(sid: ServerId, key: &str, rk: &str, state: PlayState) -> NotifyOut {
    NotifyOut { plays: vec![NotifyPlays { sid, sessions: vec![session(key, rk, state)] }], resynced: Vec::new(), ..Default::default() }
}

/// A Search page holding `rk` on each of two servers.
fn holding_on_both(rk: &str) -> Stores {
    let mut stores = Stores::default();
    stores.search.publish_shelves_for_test(vec![Shelf {
        epoch: 0,
        window: Default::default(),
        kind: Kind::Movie,
        items: [SID, OTHER].iter().map(|sid| Item::Media(PmsMovie { sid: *sid, rk: rk.into(), ..Default::default() })).collect(),
    }]);
    stores
}

#[test]
fn a_feed_for_one_server_never_reads_or_invalidates_another() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding_on_both("s1");
    let spent = map(&mut stores, &on(OTHER, "1", "s1", PlayState::Stopped), &[]);
    assert_eq!(stores.reconcile.pending_for_test(), [(OTHER, "s1".to_string())], "the key is read on the notifying server only");
    assert_eq!(spent.refetched, [OTHER]);
    assert!(spent.invalidated.is_empty());
    let spent = map(&mut stores, &on(OTHER, "2", "unheld", PlayState::Stopped), &[]);
    assert_eq!(spent.invalidated, [OTHER], "an unheld stop invalidates its own server's shelves and no other");
    assert_eq!(spent.refetched, [OTHER]);
}

#[test]
fn a_resync_forgets_only_that_servers_sessions() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding_on_both("s1");
    map(&mut stores, &on(SID, "1", "s1", PlayState::Playing), &[]);
    map(&mut stores, &on(OTHER, "1", "s1", PlayState::Playing), &[]);
    stores.reconcile.reset();
    let mut out = NotifyOut::default();
    out.resynced.push((SID, 1));
    map(&mut stores, &out, &[]);
    map(&mut stores, &on(OTHER, "1", "s1", PlayState::Playing), &[]);
    assert!(pending(&stores).is_empty(), "the other server's session is still known: steady is no news");
    map(&mut stores, &on(SID, "1", "s1", PlayState::Playing), &[]);
    assert_eq!(stores.reconcile.pending_for_test(), [(SID, "s1".to_string())], "the resynced server's is news again");
    assert_eq!(stores.notify_tracked_for_test(), 2);
}

/// Owed notification work is kept, not dropped, for as long as it cannot be spent, and a page
/// owed a re-read keeps its debt (`take_landed`) until the application takes it.
#[test]
fn deferred_preserved_when_detail_busy() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    let pages = [detail("d1")];
    stores.hold_network(true); // the player is on top: nothing may be read
    for i in 0..5 {
        let spent = map(&mut stores, &on(SID, &format!("k{i}"), "d1", PlayState::Stopped), &pages);
        assert_eq!(spent, Default::default());
        assert!(pending(&stores).is_empty());
    }
    assert!(stores.notify_owed_for_test().contains(&(SID, "d1".to_string())), "the Detail's read is still owed");
    // a landed read the application has not taken yet is not lost either
    stores.reconcile.owe_page((SID, "d1".into()));
    stores.hold_network(false);
    map(&mut stores, &NotifyOut::default(), &pages);
    assert_eq!(pending(&stores), ["d1"], "released and queued once, the owing way");
    assert!(!stores.reconcile.is_quiet_for_test(&(SID, "d1".into())));
    assert_eq!(stores.take_landed_views(), [(SID, "d1".to_string())], "the page's debt waited for the application");
    assert!(stores.take_landed_views().is_empty(), "…and is paid once");
}

#[test]
fn a_loaded_details_related_tiles_are_read_quiet_and_only_while_it_is_the_page() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    crate::metadata::set_current_for_test(stores.metadata.state_mut(), Some(crate::metadata::Detail {
        sid: SID, rk: "d1".into(),
        related: vec![PmsMovie { sid: SID, rk: "r1".into(), ..Default::default() }],
        ..Default::default()
    }));
    map(&mut stores, &one("1", "r1", PlayState::Playing), &[]);
    assert!(pending(&stores).is_empty(), "no Detail on the stack: its tiles are not held");
    map(&mut stores, &one("2", "r1", PlayState::Playing), &[detail("other")]);
    assert!(pending(&stores).is_empty(), "another Detail is the page: the loaded one's tiles are not held");
    map(&mut stores, &one("3", "r1", PlayState::Playing), &[detail("d1")]);
    assert_eq!(pending(&stores), ["r1"]);
    assert!(stores.reconcile.is_quiet_for_test(&(SID, "r1".into())), "a tile is patched in place, no Detail re-fetch");
}

/// A store aggregate whose Watchlist page holds a title matched as `rk` on the first server.
fn watchlist_holding(rk: &str) -> (Stores, ServerId) {
    let mut stores = Stores::default();
    stores.watchlist = super::watchlist::tests::holding_for_test(ServerId::from_raw(0), rk);
    (stores, ServerId::from_raw(0))
}

fn pending_on(stores: &Stores) -> Vec<(ServerId, String)> {
    stores.reconcile.pending_for_test()
}

fn on_watchlist(sid: ServerId, key: &str, rk: &str, state: PlayState) -> NotifyOut {
    NotifyOut {
        plays: vec![NotifyPlays { sid, sessions: vec![session(key, rk, state)] }],
        resynced: Vec::new(),
        ..Default::default()
    }
}

/// A title the Watchlist page shows (matched on a reachable server) owes a read-back of our state
/// when someone else's session on it changes, as any held card does.
#[test]
fn a_watchlist_held_key_owes_a_quiet_read() {
    let _g = plx_base::testlock::serial();
    let (mut stores, sid) = watchlist_holding("11");
    assert!(stores.holds(sid, "11", &[], Shown::ALL), "the page holds its matched copy");
    map(&mut stores, &on_watchlist(sid, "1", "11", PlayState::Stopped), &[]);
    assert_eq!(pending_on(&stores), [(sid, "11".to_string())]);
}

/// With the Watchlist page off the stack its store's last rows hold nothing and owe nothing.
#[test]
fn a_popped_watchlist_holds_nothing() {
    let _g = plx_base::testlock::serial();
    let (mut stores, sid) = watchlist_holding("11");
    let not_watchlist = Shown { watchlist: false, ..Shown::ALL };
    assert!(stores.holds(sid, "11", &[], Shown::ALL), "held while the page is on the stack");
    assert!(!stores.holds(sid, "11", &[], not_watchlist));
    map_shown(&mut stores, &on_watchlist(sid, "1", "11", PlayState::Stopped), &[], not_watchlist);
    assert!(pending_on(&stores).is_empty(), "Shown.watchlist = false owes nothing");
}

/// The Watchlist read-back waits for the network like any other: `!reread_allowed` keeps it queued.
#[test]
fn a_watchlist_read_waits_for_the_network() {
    let _g = plx_base::testlock::serial();
    let (mut stores, sid) = watchlist_holding("11");
    stores.hold_network(true);
    map(&mut stores, &on_watchlist(sid, "1", "11", PlayState::Stopped), &[]);
    assert!(pending_on(&stores).is_empty(), "nothing is spent under the player");
    assert!(!stores.notify_owed_for_test().is_empty(), "queued, not dropped");
    stores.hold_network(false);
    map(&mut stores, &NotifyOut::default(), &[]);
    assert_eq!(pending_on(&stores), [(sid, "11".to_string())]);
}

// ---- a library change re-matches the Watchlist (W-notify) ------------------------------------

use super::notify::NotifyLibrary;
use super::notify_map::{LIBRARY_QUIET_FRAMES, LIBRARY_SPACING_FRAMES};
use super::watchlist::tests::{scripted_for_test, scripted_with_shows_for_test, Script};
use crate::watchlist::model::TileState;
use plx_plex::plex::notify::{LibraryEntry, LibraryKind, LibraryState};

fn change(kind: LibraryKind, state: LibraryState, rk: &str) -> LibraryEntry {
    LibraryEntry { item_id: rk.into(), kind, state, section: Some("1".into()) }
}

fn created(rk: &str) -> LibraryEntry {
    change(LibraryKind::Movie, LibraryState::Created, rk)
}

fn deleted(rk: &str) -> LibraryEntry {
    change(LibraryKind::Movie, LibraryState::Deleted, rk)
}

fn library(sid: ServerId, entries: Vec<LibraryEntry>) -> NotifyOut {
    NotifyOut { library: vec![NotifyLibrary { sid, entries }], ..Default::default() }
}

/// Three movies on the Watchlist (numbered 1-3); the first server holds title 1 as rk `11`.
fn watchlist_of_three() -> (Stores, Script, ServerId) {
    let sid = ServerId::from_raw(0);
    let (watchlist, script) = scripted_for_test(sid, 3, &[(1, "11")]);
    let mut stores = Stores::default();
    stores.watchlist = watchlist;
    script.forget_asked();
    (stores, script, sid)
}

fn tile(stores: &Stores, n: u8) -> Option<TileState> {
    let v = stores.watchlist.view();
    let guid = Script::guid(n);
    (0..v.len()).find(|&i| v.row(i).is_some_and(|r| r.guid == guid)).and_then(|i| v.state(i))
}

fn quiet_frames(stores: &mut Stores, n: u64) {
    for _ in 0..n {
        map(stores, &NotifyOut::default(), &[]);
    }
}

/// The pumps after one that delivered a library entry, up to the one that ends its wait: nothing is
/// spent before it, and what it spends is returned.
fn after_the_quiet(stores: &mut Stores) -> super::notify_map::Spent {
    for _ in 1..LIBRARY_QUIET_FRAMES {
        assert_eq!(map(stores, &NotifyOut::default(), &[]), Default::default(), "the feed has not been quiet for 10 s yet");
    }
    map(stores, &NotifyOut::default(), &[])
}

#[test]
fn a_deleted_item_a_tile_is_matched_to_re_asks_that_guid_and_the_tile_flips() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    assert_eq!(tile(&stores, 1), Some(TileState::Available));
    script.drop_hold(sid, 1);
    map(&mut stores, &library(sid, vec![deleted("11")]), &[]);
    assert!(script.asked().is_empty(), "not while the feed may still be busy");
    after_the_quiet(&mut stores);
    assert_eq!(script.asked(), [[Script::guid(1)]], "that guid only, one request");
    stores.watchlist.pump();
    assert_ne!(tile(&stores, 1), Some(TileState::Available), "the tile left 'on your servers' at the landing");
    assert!(stores.notify_owed_for_test().is_empty());
}

#[test]
fn a_deleted_item_no_tile_is_matched_to_owes_nothing() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    map(&mut stores, &library(sid, vec![deleted("999"), deleted("not a key")]), &[]);
    after_the_quiet(&mut stores);
    assert!(script.asked().is_empty());
    assert!(stores.notify_owed_for_test().is_empty());
    // The same key on another server is not the matched copy either.
    map(&mut stores, &library(ServerId::from_raw(3), vec![deleted("11")]), &[]);
    after_the_quiet(&mut stores);
    assert!(script.asked().is_empty());
}

#[test]
fn a_created_item_re_asks_only_the_titles_the_server_is_not_seen_to_hold() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    script.hold(sid, 2, "12");
    map(&mut stores, &library(sid, vec![created("12")]), &[]);
    after_the_quiet(&mut stores);
    let asked = script.asked();
    assert_eq!(asked.len(), 1, "{asked:?}");
    let mut guids = asked[0].clone();
    guids.sort();
    assert_eq!(guids, [Script::guid(2), Script::guid(3)], "title 1 is held already and is not asked again");
    stores.watchlist.pump();
    assert_eq!(tile(&stores, 2), Some(TileState::Available));
    assert_eq!(tile(&stores, 1), Some(TileState::Available));
}

#[test]
fn an_add_of_a_kind_the_list_has_no_rows_of_asks_nothing() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    map(&mut stores, &library(sid, vec![change(LibraryKind::Show, LibraryState::Created, "5")]), &[]);
    after_the_quiet(&mut stores);
    assert!(script.asked().is_empty(), "the three rows are movies");
    assert!(stores.notify_owed_for_test().is_empty(), "and nothing is left owed");
}

#[test]
fn a_burst_of_changes_is_one_re_match_per_server_and_kind() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    script.drop_hold(sid, 1);
    let burst = NotifyOut {
        library: vec![
            NotifyLibrary { sid, entries: vec![created("20"), created("21"), deleted("11"), deleted("11")] },
            NotifyLibrary { sid, entries: vec![created("22")] },
        ],
        ..Default::default()
    };
    map(&mut stores, &burst, &[]);
    after_the_quiet(&mut stores);
    let asked = script.asked();
    assert_eq!(asked.len(), 1, "one request for the burst: {asked:?}");
    let mut guids = asked[0].clone();
    guids.sort();
    assert_eq!(guids, [Script::guid(1), Script::guid(2), Script::guid(3)], "each guid once");
}

#[test]
fn a_change_after_a_re_match_starts_a_new_wait() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    map(&mut stores, &library(sid, vec![created("20")]), &[]);
    after_the_quiet(&mut stores);
    assert_eq!(script.asked().len(), 1);
    stores.watchlist.pump();
    map(&mut stores, &library(sid, vec![created("21")]), &[]);
    quiet_frames(&mut stores, LIBRARY_QUIET_FRAMES - 1);
    assert_eq!(script.asked().len(), 1, "the new entry waits for its own quiet");
    assert!(!stores.notify_owed_for_test().is_empty(), "queued, not dropped");
    quiet_frames(&mut stores, 1);
    assert_eq!(script.asked().len(), 2, "made once the feed has been quiet for 10 s");
    assert!(stores.notify_owed_for_test().is_empty());
}

/// The spacing between two re-matches of one `(server, kind)` is not the wait's: a feed gap's sweep
/// is owed without an entry, and one right after a re-match still holds off for the spacing.
#[test]
fn a_feed_gap_right_after_a_re_match_waits_for_the_spacing() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    map(&mut stores, &library(sid, vec![created("20")]), &[]);
    after_the_quiet(&mut stores);
    assert_eq!(script.asked().len(), 1);
    stores.watchlist.pump();
    map(&mut stores, &NotifyOut { resynced: vec![(sid, 1)], ..Default::default() }, &[]);
    quiet_frames(&mut stores, LIBRARY_SPACING_FRAMES - 3);
    assert_eq!(script.asked().len(), 1, "still inside the spacing");
    assert!(!stores.notify_owed_for_test().is_empty(), "queued, not dropped");
    quiet_frames(&mut stores, 3);
    assert_eq!(script.asked().len(), 2, "made once the spacing has passed, with no wait of its own");
    assert!(stores.notify_owed_for_test().is_empty());
}

/// A feed gap's sweep is not the entries' owe: it is made at once, even with entries still waiting,
/// and it covers what they would have asked, so the wait's end asks the Watchlist nothing more.
#[test]
fn a_feed_gap_sweeps_at_once_while_entries_wait() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    map(&mut stores, &library(sid, vec![created("20")]), &[]);
    assert!(script.asked().is_empty(), "the entry waits");
    map(&mut stores, &NotifyOut { resynced: vec![(sid, 1)], ..Default::default() }, &[]);
    assert_eq!(script.asked().len(), 1, "the sweep does not");
    assert_eq!(script.asked()[0].len(), 3, "it asks about every title");
    stores.watchlist.pump();
    // The entry came two pumps ago.
    quiet_frames(&mut stores, LIBRARY_QUIET_FRAMES - 2);
    let at_the_end = map(&mut stores, &NotifyOut::default(), &[]);
    assert_eq!(at_the_end.relisted, [sid], "the entry's lists are still read when its feed is quiet");
    assert_eq!(script.asked().len(), 1, "and the Watchlist is not asked a second time for it");
}

#[test]
fn a_re_match_waits_for_the_network_like_every_owed_read() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    stores.hold_network(true);
    map(&mut stores, &library(sid, vec![created("20")]), &[]);
    quiet_frames(&mut stores, LIBRARY_QUIET_FRAMES);
    assert!(script.asked().is_empty(), "nothing is spent under the player");
    assert!(!stores.notify_owed_for_test().is_empty(), "queued, not dropped");
    stores.hold_network(false);
    quiet_frames(&mut stores, 1);
    assert_eq!(script.asked().len(), 1);
    assert!(stores.notify_owed_for_test().is_empty());
}

#[test]
fn a_change_noted_before_the_first_load_is_not_asked_again_after_it() {
    let _g = plx_base::testlock::serial();
    let mut stores = Stores::default();
    let sid = ServerId::from_raw(0);
    // A create and a delete arrive while the page has never been read and no read is out.
    map(&mut stores, &library(sid, vec![created("20"), deleted("11")]), &[]);
    quiet_frames(&mut stores, LIBRARY_QUIET_FRAMES);
    assert!(stores.notify_owed_for_test().is_empty(), "the read to come already reflects both");
    let (watchlist, script) = scripted_for_test(sid, 3, &[(1, "11")]);
    stores.watchlist = watchlist;
    script.forget_asked();
    quiet_frames(&mut stores, 1);
    assert!(script.asked().is_empty(), "no redundant re-match right after the first load: {:?}", script.asked());
}

#[test]
fn a_change_that_arrives_while_the_first_read_is_out_is_kept_and_asked_after_it() {
    let _g = plx_base::testlock::serial();
    let sid = ServerId::from_raw(0);
    let mut stores = Stores::default();
    stores.watchlist = super::watchlist::tests::reading_for_test(sid, 3);
    map(&mut stores, &library(sid, vec![created("20")]), &[]);
    quiet_frames(&mut stores, LIBRARY_QUIET_FRAMES + 5);
    assert!(!stores.notify_owed_for_test().is_empty(), "that read's matching may have asked before the create");
    let (watchlist, script) = scripted_for_test(sid, 3, &[(1, "11")]);
    stores.watchlist = watchlist;
    script.forget_asked();
    quiet_frames(&mut stores, 1);
    assert_eq!(script.asked().len(), 1, "{:?}", script.asked());
    assert!(stores.notify_owed_for_test().is_empty());
}

/// A delete that arrives while the first read is out cannot be followed to a tile (the list is not
/// ready), so it asks after every title of the kind. That is not a feed gap's sweep: it waits for its
/// server's quiet like any other change, and the burst it belongs to is asked once, not twice.
#[test]
fn a_delete_during_the_first_read_waits_for_the_servers_quiet_and_asks_once() {
    let _g = plx_base::testlock::serial();
    let sid = ServerId::from_raw(0);
    let mut stores = Stores::default();
    stores.watchlist = super::watchlist::tests::reading_for_test(sid, 3);
    map(&mut stores, &library(sid, vec![deleted("11")]), &[]);
    // The first read lands while the server's feed is still settling.
    let (watchlist, script) = scripted_for_test(sid, 3, &[(1, "11")]);
    stores.watchlist = watchlist;
    script.forget_asked();
    for _ in 1..LIBRARY_QUIET_FRAMES {
        map(&mut stores, &NotifyOut::default(), &[]);
        assert!(script.asked().is_empty(), "not while the feed may still be busy: {:?}", script.asked());
    }
    map(&mut stores, &NotifyOut::default(), &[]);
    assert_eq!(script.asked().len(), 1, "the burst is asked once, at the quiet: {:?}", script.asked());
    assert_eq!(script.asked()[0].len(), 3, "every title of the kind: the delete was not followed to a tile");
    quiet_frames(&mut stores, LIBRARY_QUIET_FRAMES);
    assert_eq!(script.asked().len(), 1, "and not again: {:?}", script.asked());
    assert!(stores.notify_owed_for_test().is_empty());
}

#[test]
fn a_feed_that_came_back_after_a_gap_sweeps_the_titles_of_that_server() {
    let _g = plx_base::testlock::serial();
    let (mut stores, script, sid) = watchlist_of_three();
    script.hold(sid, 3, "13");
    let gap = NotifyOut { resynced: vec![(sid, 1)], ..Default::default() };
    map(&mut stores, &gap, &[]);
    assert_eq!(script.asked().len(), 1);
    assert_eq!(script.asked()[0].len(), 3, "title 1 is asked again too: a delete may have been missed");
    stores.watchlist.pump();
    assert_eq!(tile(&stores, 3), Some(TileState::Available));
}

#[test]
fn a_library_change_moves_the_servers_lists_and_reads_no_item() {
    let _g = plx_base::testlock::serial();
    let (mut stores, _script, sid) = watchlist_of_three();
    map(&mut stores, &library(sid, vec![created("20"), deleted("11")]), &[]);
    let spent = after_the_quiet(&mut stores);
    assert_eq!(spent.refetched, [sid], "one quiet Home refetch");
    assert_eq!(spent.invalidated, [sid], "one section-shelf invalidate");
    assert_eq!(spent.relisted, [sid], "one re-list of its grids");
    assert!(pending(&stores).is_empty(), "no per-item read-back");
}

/// The quiet refetch is a Hubs step, and a Hubs step settles the launches the runner refused since the
/// last one: its refreshes are part of what the pass spent, not dropped.
#[test]
fn a_refusal_the_quiet_refetch_step_settles_is_carried_out_of_the_pass() {
    let _g = plx_base::testlock::serial();
    let _session = plx_plex::plex::session::TempSession::new("notify-refusal");
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("notify-refusal", "127.0.0.1", 9, "synthetic", "cid");
    let mut stores = Stores::default();
    let keys = crate::pms::registry_keys_for_test();
    let dir = DirectorySnapshot::default();
    let first = stores.hubs.run_with_directory(crate::stores::hubs::HubsCmd::RefetchHubs, dir.view(), &keys);
    assert!(first.endpoints.is_empty(), "the refusal is the runner's answer, not the kick's");
    stores.hubs.refuse_jobs_for_test();
    map(&mut stores, &library(sid, vec![created("20")]), &[]);
    let spent = after_the_quiet(&mut stores);
    assert_eq!(spent.refetched, [sid]);
    assert_eq!(spent.endpoints.iter().map(|r| r.sid).collect::<Vec<_>>(), [sid]);
    plx_plex::plex::reset_servers_for_test();
}

fn owes_lines() -> usize {
    std::fs::read_to_string(plx_base::eventlog::events_log())
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("notify: owes"))
        .count()
}

fn settled_lines() -> Vec<String> {
    std::fs::read_to_string(plx_base::eventlog::events_log())
        .unwrap_or_default()
        .lines()
        .filter(|l| l.contains("notify: library settled"))
        .map(|l| l.rsplit("notify: ").next().unwrap_or_default().to_owned())
        .collect()
}

/// A scan reports a create every few seconds: the server's lists are read once when the feed goes
/// quiet, and the one line that says so counts the entries and the wait; the frames spent waiting
/// log nothing.
#[test]
fn a_waited_for_refresh_logs_once_when_it_is_spent_and_never_per_waiting_frame() {
    let _g = plx_base::testlock::serial();
    plx_base::eventlog::with_private_log(|| {
        let mut stores = holding(&[]);
        let change = library(SID, vec![created("20"), created("21")]);
        let none = NotifyOut::default();
        for second in 0..20 {
            map(&mut stores, if second % 4 == 0 { &change } else { &none }, &[]);
            quiet_frames(&mut stores, FRAMES_PER_S - 1);
        }
        assert_eq!(owes_lines(), 0, "twenty seconds of waiting spent nothing and logged no owe");
        assert!(settled_lines().is_empty(), "nor a settle");
        quiet_frames(&mut stores, 2 * LIBRARY_QUIET_FRAMES);
        assert_eq!(owes_lines(), 1, "one spend");
        assert_eq!(settled_lines(), [format!("library settled sid={} entries=10 waited=26s", SID.raw())]);
    });
}

// ---- library entries wait for the feed to go quiet (#1458) ----------------------------------

use super::notify::FRAMES_PER_S;
use super::notify_map::{Spent, LIBRARY_LONGEST_WAIT_FRAMES};

/// How many times a server's lists were read over a run of pumps.
#[derive(Default, Debug, PartialEq, Clone, Copy)]
struct Tally {
    refetched: usize,
    invalidated: usize,
    relisted: usize,
}

impl Tally {
    const ONE_OF_EACH: Self = Self { refetched: 1, invalidated: 1, relisted: 1 };

    fn add(&mut self, sid: ServerId, spent: &Spent) {
        let of = |servers: &[ServerId]| servers.iter().filter(|s| **s == sid).count();
        self.refetched += of(&spent.refetched);
        self.invalidated += of(&spent.invalidated);
        self.relisted += of(&spent.relisted);
    }

    fn at_most_one_of_each(&self) -> bool {
        self.refetched <= 1 && self.invalidated <= 1 && self.relisted <= 1
    }
}

/// One pump of `out` on `SID`'s feed, reported as what it spent.
fn pump_tally(stores: &mut Stores, out: &NotifyOut) -> Tally {
    let mut spent = Tally::default();
    spent.add(SID, &map(stores, out, &[]));
    spent
}

/// Pump `n` empty frames and sum what they spent.
fn quiet_tally(stores: &mut Stores, n: u64) -> Tally {
    let mut sum = Tally::default();
    for _ in 0..n {
        sum.add(SID, &map(stores, &NotifyOut::default(), &[]));
    }
    sum
}

/// A feed that delivers one finished item a second for a minute used to re-read the server's Home,
/// its shelves and its grids every 5 s; over a remote link that kept every read the viewer scrolled
/// to stuck in the queue. They are made once the feed goes quiet, or once a minute when it never does.
#[test]
fn a_feed_that_streams_library_entries_refreshes_the_servers_lists_once_it_goes_quiet() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    let entry = library(SID, vec![created("20")]);
    let mut run = Tally::default();
    for second in 0..65 {
        run.add(SID, &map(&mut stores, &entry, &[]));
        for _ in 1..FRAMES_PER_S {
            run.add(SID, &map(&mut stores, &NotifyOut::default(), &[]));
        }
        if second == 59 {
            assert!(run.at_most_one_of_each(), "a minute of steady entries: {run:?}");
        }
    }
    assert_eq!(run, Tally::ONE_OF_EACH, "once, the pump the oldest entry had waited a minute");
    // The last entry was FRAMES_PER_S - 1 pumps ago. The stream stops: nothing until 10 s after it,
    // then the one refresh the entries since then owe, and then nothing more.
    let before = quiet_tally(&mut stores, LIBRARY_QUIET_FRAMES - FRAMES_PER_S);
    assert_eq!(before, Tally::default(), "not before the feed has been quiet for 10 s");
    assert_eq!(quiet_tally(&mut stores, 1), Tally::ONE_OF_EACH, "once it has");
    assert_eq!(quiet_tally(&mut stores, 2 * LIBRARY_LONGEST_WAIT_FRAMES), Tally::default(), "and once only");
    assert!(stores.notify_owed_for_test().is_empty());
}

#[test]
fn a_lone_library_entry_is_spent_once_the_feed_has_been_quiet_for_ten_seconds() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    assert_eq!(pump_tally(&mut stores, &library(SID, vec![created("20")])), Tally::default(), "not at once");
    assert_eq!(quiet_tally(&mut stores, LIBRARY_QUIET_FRAMES - 1), Tally::default(), "not on the next pump, nor for 10 s");
    assert!(!stores.notify_owed_for_test().is_empty(), "waiting, not dropped");
    assert_eq!(quiet_tally(&mut stores, 1), Tally::ONE_OF_EACH, "the pump that makes it 10 s");
    assert_eq!(quiet_tally(&mut stores, 2 * LIBRARY_QUIET_FRAMES), Tally::default(), "spent once");
    assert!(stores.notify_owed_for_test().is_empty());
}

#[test]
fn a_feed_that_never_goes_quiet_still_refreshes_about_once_a_minute() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    let entry = library(SID, vec![created("20")]);
    let none = NotifyOut::default();
    let mut released_at = Vec::new();
    for pump in 1..=190 * FRAMES_PER_S {
        let out = if (pump - 1) % FRAMES_PER_S == 0 { &entry } else { &none };
        let spent = pump_tally(&mut stores, out);
        if spent != Tally::default() {
            assert_eq!(spent, Tally::ONE_OF_EACH, "pump {pump}");
            released_at.push(pump);
        }
    }
    assert_eq!(released_at.len(), 3, "190 s of entries, a refresh a minute: {released_at:?}");
    assert_eq!(released_at[0], 1 + LIBRARY_LONGEST_WAIT_FRAMES, "the oldest entry has waited a minute");
    for pair in released_at.windows(2) {
        assert!(pair[1] - pair[0] >= LIBRARY_LONGEST_WAIT_FRAMES, "after a refresh the next entry starts a new wait: {released_at:?}");
    }
}

/// The store re-matches one thing at a time, so a server whose movies and shows both changed has one
/// re-match refused at the pump its wait ends. That one stays queued and its wait is over: it starts
/// when the one out finishes, not after the new wait the feed's next entry began a second later
/// (#1458). Held again each time, a feed that never goes quiet re-matched each kind about every two
/// minutes, not once a minute.
#[test]
fn a_re_match_refused_when_the_wait_ends_starts_when_the_one_out_finishes_not_a_minute_later() {
    let _g = plx_base::testlock::serial();
    let sid = ServerId::from_raw(0);
    let (watchlist, script) = scripted_with_shows_for_test(sid, 3, 2, &[(1, "11")]);
    let mut stores = Stores::default();
    stores.watchlist = watchlist;
    script.forget_asked();
    // A movie and a show finish every second, for as long as the test runs.
    let entry = library(sid, vec![created("20"), change(LibraryKind::Show, LibraryState::Created, "21")]);
    let none = NotifyOut::default();
    let mut pumps = 0;
    let mut step = |stores: &mut Stores| {
        pumps += 1;
        map(stores, if (pumps - 1) % FRAMES_PER_S == 0 { &entry } else { &none }, &[]);
    };
    for _ in 0..LIBRARY_LONGEST_WAIT_FRAMES {
        step(&mut stores);
    }
    assert!(script.asked().is_empty(), "the oldest entry has not waited a minute");
    step(&mut stores);
    let out = script.asked();
    assert_eq!(out.len(), 1, "one kind's re-match starts the pump the wait ends: {out:?}");
    assert!(out[0].contains(&Script::guid(2)), "the movies, which were owed first");
    // The re-match takes longer than a second (a remote link): the next entry arrives while it is out.
    for _ in 0..FRAMES_PER_S {
        step(&mut stores);
    }
    assert_eq!(script.asked().len(), 1, "the shows wait for the re-match that is out");
    stores.watchlist.pump();
    let mut waited = 0;
    while script.asked().len() < 2 && waited < LIBRARY_LONGEST_WAIT_FRAMES {
        step(&mut stores);
        waited += 1;
    }
    assert!(waited <= FRAMES_PER_S, "the shows start {waited} pumps after the re-match ahead of them finished");
    let asked = script.asked();
    assert!(asked[1].contains(&Script::show_guid(4)), "and they ask about the shows: {asked:?}");
}

/// The wait belongs to the library entries: a `stopped` session's refetch and invalidate are spent
/// the pump it arrives, and the library's own refresh still comes when its feed is quiet.
#[test]
fn a_stop_is_never_held_by_the_library_wait() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    assert_eq!(pump_tally(&mut stores, &library(SID, vec![created("20")])), Tally::default());
    let stop = pump_tally(&mut stores, &one("1", "zzz", PlayState::Stopped));
    assert_eq!(stop, Tally { refetched: 1, invalidated: 1, relisted: 0 }, "a stop refetches and invalidates at once, and re-lists nothing");
    assert_eq!(quiet_tally(&mut stores, LIBRARY_QUIET_FRAMES - 2), Tally::default());
    assert_eq!(quiet_tally(&mut stores, 1), Tally::ONE_OF_EACH, "the entry's refresh, once its feed has been quiet");
}

#[test]
fn each_servers_library_wait_is_its_own() {
    let _g = plx_base::testlock::serial();
    let other = ServerId::from_raw(1);
    let mid = 1 + LIBRARY_QUIET_FRAMES / 2;
    let mut stores = holding(&[]);
    let (mut mine, mut theirs) = (Vec::new(), Vec::new());
    for pump in 1..=3 * LIBRARY_QUIET_FRAMES {
        // The first server's entry, then the other's halfway through its wait.
        let out = match pump {
            1 => library(SID, vec![created("20")]),
            p if p == mid => library(other, vec![created("30")]),
            _ => NotifyOut::default(),
        };
        let spent = map(&mut stores, &out, &[]);
        if spent.relisted.contains(&SID) {
            mine.push(pump);
        }
        if spent.relisted.contains(&other) {
            theirs.push(pump);
        }
    }
    assert_eq!(mine, [1 + LIBRARY_QUIET_FRAMES], "10 s after its own entry, not pushed back by the other's");
    assert_eq!(theirs, [mid + LIBRARY_QUIET_FRAMES], "10 s after its own");
}

#[test]
fn a_library_wait_that_ends_under_the_player_is_spent_when_the_network_is_released() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&[]);
    stores.hold_network(true);
    map(&mut stores, &library(SID, vec![created("20")]), &[]);
    assert_eq!(quiet_tally(&mut stores, 2 * LIBRARY_QUIET_FRAMES), Tally::default(), "nothing is spent under the player");
    assert!(!stores.notify_owed_for_test().is_empty(), "queued, not dropped");
    stores.hold_network(false);
    assert_eq!(quiet_tally(&mut stores, 1), Tally::ONE_OF_EACH, "the pump the network frees up");
    assert!(stores.notify_owed_for_test().is_empty());
}
