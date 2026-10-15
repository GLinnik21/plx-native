//! The notification mapping: which of a stranger's play-state hints become a read of OUR state, and
//! that none of them is ever treated as playback (S5). Observed through the reconcile queue and the
//! effects `Stores::notify_map` reports having spent.

use super::browse::DirectorySnapshot;
use super::notify::{NotifyOut, NotifyPlays, NOTIFY_ENABLED};
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
    let dir = DirectorySnapshot::default();
    stores.notify_map(out, pages, dir.view())
}

fn pending(stores: &Stores) -> Vec<String> {
    stores.reconcile.pending_for_test().into_iter().map(|(_, k)| k).collect()
}

fn detail(rk: &str) -> ContentArg {
    ContentArg::Detail { sid: SID, rk: rk.into() }
}

#[test]
fn notifications_ship_off() {
    assert!(!NOTIFY_ENABLED, "S6: flipping the constant is the owner's decision after the TV checks");
}

#[test]
fn holds_covers_the_visible_and_the_covered_pages() {
    let _g = plx_base::testlock::serial();
    let stores = holding(&["s1"]);
    assert!(stores.holds(SID, "s1", &[]), "a list page's window");
    assert!(!stores.holds(SID, "other", &[]));
    assert!(stores.holds(SID, "d1", &[detail("d1")]), "a Detail page on the stack, top or covered");
    assert!(!stores.holds(SID, "d1", &[]), "a popped page holds nothing");
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
    assert!(!spent.invalidated, "the held item is read back, no whole-section invalidate");
}

#[test]
fn stopped_unheld_coalesced_source_refetch_and_invalidate() {
    let _g = plx_base::testlock::serial();
    let mut stores = holding(&["s1"]);
    let out = plays(vec![session("1", "a", PlayState::Stopped), session("2", "b", PlayState::Stopped)]);
    let spent = map(&mut stores, &out, &[]);
    assert!(pending(&stores).is_empty(), "no per-item read of an item nothing shows");
    assert_eq!(spent.refetched, [SID], "two stops on one server are one refetch");
    assert!(spent.invalidated);
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
