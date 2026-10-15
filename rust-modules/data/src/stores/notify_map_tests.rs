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
    NotifyOut { plays: vec![NotifyPlays { sid: SID, sessions }], resynced: Vec::new() }
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
    stores.notify_map(out, pages, shown, dir.view())
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
    NotifyOut { plays: vec![NotifyPlays { sid, sessions: vec![session(key, rk, state)] }], resynced: Vec::new() }
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
