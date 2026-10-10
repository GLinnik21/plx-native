//! The windows over a result row's hits (`search.rs`, `search/tail.rs`): the store pages each row
//! through a fake server, and every hit is reachable while the cards held stay a window.

use std::sync::{Arc, Mutex};

use super::tail_tests::Fake;
use super::test_support::*;
use super::*;
use crate::stores::search::SearchCmd;

/// Points every worker at `fake` (and runs it inline, so a pump is deterministic) until dropped.
struct Hook;

impl Hook {
    fn set(fake: Fake) -> (Hook, Arc<Mutex<Fake>>) {
        let fake = Arc::new(Mutex::new(fake));
        *TEST_IO.lock().unwrap() = Some(fake.clone());
        (Hook, fake)
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        *TEST_IO.lock().unwrap() = None;
    }
}

struct Rig {
    owner: Owner,
    fake: Arc<Mutex<Fake>>,
    peak: usize,
    _hook: Hook,
    _fresh: Fresh,
}

impl Rig {
    fn new(servers: usize, fake: Fake) -> Rig {
        let fresh = fresh();
        let mut owner = Owner::default();
        register(&mut owner, servers);
        let (hook, fake) = Hook::set(fake);
        owner.set_query("wallace");
        let mut rig = Rig { owner, fake, peak: 0, _hook: hook, _fresh: fresh };
        rig.drain(6 * servers + 6);
        rig
    }

    fn drain(&mut self, pumps: usize) {
        for _ in 0..pumps {
            self.owner.pump(0.3);
            let busy = slots().iter().filter(|&&i| self.owner.adapter.mailbox(i).busy()).count();
            self.peak = self.peak.max(busy);
        }
    }

    fn page(&mut self, kind: Kind, before: bool) {
        let seen = self.seen(kind);
        self.owner.state.run(&self.owner.adapter, SearchCmd::Page { kind, before, seen });
        let n = 6 * nsrc() + 12;
        self.drain(n);
    }

    /// A page that waits out failed reads: pump until the slide has committed or `frames` ran out.
    fn page_settled(&mut self, kind: Kind, frames: usize) -> bool {
        let k = KINDS.iter().position(|x| *x == kind).unwrap();
        let seen = self.seen(kind);
        self.owner.state.run(&self.owner.adapter, SearchCmd::Page { kind, before: false, seen });
        for _ in 0..frames {
            if self.owner.state.wins[k].pending.is_none() { return true; }
            self.drain(1);
        }
        self.owner.state.wins[k].pending.is_none()
    }

    /// The row's window as a screen reading this publication names it in an ask.
    fn seen(&self, kind: Kind) -> (usize, usize) {
        (self.shelf(kind).window.start, self.shelf(kind).items.len())
    }

    fn shelf(&self, kind: Kind) -> &Shelf {
        self.owner.state.shelves().iter().find(|s| s.kind == kind).expect("the row is there")
    }

    fn keys(&self, kind: Kind) -> Vec<String> {
        self.shelf(kind).items.iter().map(tail::key_of).collect()
    }

    /// Page a row to its last card and collect every key drawn on the way, asserting the window.
    fn reach(&mut self, kind: Kind) -> Vec<String> {
        let k = KINDS.iter().position(|x| *x == kind).unwrap();
        let mut seen = self.keys(kind);
        for _ in 0..200 {
            assert!(self.shelf(kind).items.len() <= 24, "a window of one source is at most 24 cards");
            let held: usize = self.owner.state.src.iter().map(|s| s.tails[k].held()).sum();
            assert!(held <= 24 + 2 * tail::PAGE, "{held} rows held");
            if !self.shelf(kind).window.after { break; }
            let before = (self.shelf(kind).window.start, self.shelf(kind).items.len());
            self.page(kind, false);
            let now = (self.shelf(kind).window.start, self.shelf(kind).items.len());
            assert!(now.0 > before.0 || (now.0 == before.0 && now.1 > before.1),
                "the window slid, or filled where it stood: {before:?} -> {now:?}");
            seen.extend(self.keys(kind));
        }
        assert!(!self.shelf(kind).window.after, "the last card was reached");
        seen.sort_by_key(|k| k.parse::<usize>().unwrap());
        seen.dedup();
        seen
    }
}

fn numbers(r: std::ops::Range<usize>) -> Vec<String> { r.map(|i| i.to_string()).collect() }

#[test]
fn three_hundred_movies_are_reached_to_the_last_and_back_within_a_window() {
    let mut rig = Rig::new(1, Fake::new().movies(300));
    assert_eq!(rig.keys(Kind::Movie).len(), 12, "the first paint is the preview");
    assert_eq!(rig.reach(Kind::Movie), numbers(0..300), "every hit of the row");
    for _ in 0..100 {
        if rig.shelf(Kind::Movie).window.start == 0 { break; }
        rig.page(Kind::Movie, true);
        assert!(rig.shelf(Kind::Movie).items.len() <= 24);
    }
    assert!(!rig.shelf(Kind::Movie).window.before);
    assert_eq!(rig.keys(Kind::Movie), numbers(0..24), "back at the head, the same cards");
}

/// The first paint is the preview, half of a one-source window, and the row asks to go on with
/// focus in the preview's second half. Going on reads the rest of the window that stands: sliding
/// it instead would drop the whole preview, the focused card with it.
#[test]
fn going_on_from_the_preview_fills_the_window_before_it_slides() {
    let mut rig = Rig::new(1, Fake::new().movies(300));
    assert_eq!(rig.keys(Kind::Movie), numbers(0..12), "the first paint is the preview");
    rig.page(Kind::Movie, false);
    assert_eq!(rig.shelf(Kind::Movie).window.start, 0, "the window stays where the reader is");
    assert_eq!(rig.keys(Kind::Movie), numbers(0..24), "and every card of the preview keeps its place");
    rig.page(Kind::Movie, false);
    assert_eq!(rig.keys(Kind::Movie), numbers(12..36), "a full window slides by half");
}

#[test]
fn a_mock_empty_for_every_typed_listing_still_reaches_every_hit_of_every_row() {
    let mut fake = Fake { typed: false, ..Fake::default() }.movies(80).tv(30, 30);
    fake.collections = (0..40).map(|i| ("collection", 500 + i)).collect();
    fake.people = (0..50).collect();
    let mut rig = Rig::new(1, fake);
    assert_eq!(rig.reach(Kind::Movie), numbers(0..80));
    assert_eq!(rig.reach(Kind::Show), numbers(0..30));
    assert_eq!(rig.reach(Kind::Collection).len(), 40);
}

#[test]
fn shows_come_first_and_the_episodes_are_split_off_at_the_show_count() {
    let mut rig = Rig::new(1, Fake::new().tv(30, 40));
    assert_eq!(rig.reach(Kind::Show), numbers(0..30));
    let eps: Vec<String> = (0..40).map(|i| (1000 + i).to_string()).collect();
    assert_eq!(rig.reach(Kind::Episode), eps);
}

#[test]
fn a_new_query_releases_every_lane_window_and_pending_ask() {
    let mut rig = Rig::new(1, Fake::new().movies(100));
    rig.page(Kind::Movie, false);
    rig.page(Kind::Movie, false);
    assert!(rig.owner.state.wins[0].lo > 0);
    // a slide that cannot finish: its source keeps failing, so the ask stays pending
    rig.fake.lock().unwrap().fail_listings = usize::MAX;
    rig.page(Kind::Movie, false);
    rig.page(Kind::Movie, false);
    assert!(rig.owner.state.wins[0].pending.is_some(), "the slide is owed");
    rig.owner.set_query("wallace shawn");
    assert!(rig.owner.state.wins.iter().all(|w| w.lo == 0), "every window is back at the head");
    assert!(rig.owner.state.wins.iter().all(|w| w.pending.is_none()), "no ask outlives the query");
    assert!(rig.owner.state.src.iter().all(|s| !s.tails[0].inited), "no lane survives the query");
}

/// Focus left the edge a slide was asked from while its sources were still reading: the screen
/// withdraws the ask, and the reads that answer afterwards must not commit a window the reader has
/// walked away from. A withdrawal for a window that has since moved is a no-op.
#[test]
fn a_withdrawn_slide_is_not_committed_when_its_sources_answer() {
    let mut rig = Rig::new(1, Fake::new().movies(100));
    rig.page(Kind::Movie, false);
    rig.page(Kind::Movie, false);
    let lo = rig.owner.state.wins[0].lo;
    assert!(lo > 0);
    // a slide that cannot finish yet: its source fails, so the ask stays pending
    rig.fake.lock().unwrap().fail_listings = usize::MAX;
    rig.page(Kind::Movie, false);
    assert!(rig.owner.state.wins[0].pending.is_some(), "the slide is owed");
    rig.owner.state.run(&rig.owner.adapter, SearchCmd::PageCancel { kind: Kind::Movie, seen: lo + 1000 });
    assert!(rig.owner.state.wins[0].pending.is_some(), "a withdrawal naming another window withdraws nothing");
    rig.owner.state.run(&rig.owner.adapter, SearchCmd::PageCancel { kind: Kind::Movie, seen: lo });
    assert!(rig.owner.state.wins[0].pending.is_none(), "the slide is withdrawn");
    rig.fake.lock().unwrap().fail_listings = 0;
    rig.drain(60);
    assert_eq!(rig.owner.state.wins[0].lo, lo, "nothing commits once the sources answer");
    assert!(rig.page_settled(Kind::Movie, 800), "asking again finishes");
    assert!(rig.owner.state.wins[0].lo > lo, "and asking again slides the window");
}

#[test]
fn forty_servers_never_have_more_than_four_requests_in_flight() {
    let mut rig = Rig::new(40, Fake::new().movies(60));
    for _ in 0..30 { rig.page(Kind::Movie, false); }
    assert!(rig.owner.state.wins[0].lo >= 12, "the row moved past every preview");
    assert!(rig.peak <= 4, "peak {}", rig.peak);
    assert!(rig.peak > 0);
    assert!(rig.owner.state.src.iter().filter(|s| s.status == Status::Answered).all(|s| s.tails[0].covered() > 12));
}

#[test]
fn favourites_rank_only_within_the_first_twelve_cards() {
    let sid = ServerId::from_raw(0);
    let item = |i: usize| Item::Media(crate::pms::PmsMovie { sid, rk: i.to_string(), title: i.to_string(),
        sec: if i % 2 == 0 { 9 } else { 1 }, ..Default::default() });
    let favs = [(sid, 1i64, true), (sid, 9i64, false)];
    let sh = merge(&[answered(0, (0..24).map(item).collect())], &favs);
    let got = titles(&sh[0]);
    let mut want: Vec<String> = [1, 3, 5, 7, 9, 11, 0, 2, 4, 6, 8, 10].iter().map(|i| i.to_string()).collect();
    want.extend((12..24).map(|i| i.to_string()));
    assert_eq!(got, want.iter().map(String::as_str).collect::<Vec<_>>());
}

/// Every card of `kind` the row draws now, as (server, key).
fn drawn(rig: &Rig, kind: Kind) -> Vec<(u16, String)> {
    rig.shelf(kind).items.iter().map(|it| (it.sid().raw(), tail::key_of(it))).collect()
}

#[test]
fn a_lane_read_that_fails_loses_no_hit_and_does_not_move_the_focused_card() {
    let mut rig = Rig::new(3, Fake::new().movies(100));
    rig.fake.lock().unwrap().fail_listings = 6;
    let mut seen: std::collections::BTreeSet<(u16, String)> = drawn(&rig, Kind::Movie).into_iter().collect();
    for _ in 0..200 {
        if !rig.shelf(Kind::Movie).window.after { break; }
        let held = drawn(&rig, Kind::Movie);
        let before = rig.shelf(Kind::Movie).window.start;
        let asked = rig.seen(Kind::Movie);
        rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen: asked });
        let mut frames = 0;
        // while a source still owes its read the cards stay where they are
        while rig.owner.state.wins[0].pending.is_some() && frames < 800 {
            assert_eq!(drawn(&rig, Kind::Movie), held, "the focused card keeps its place while a read is owed");
            rig.drain(1);
            frames += 1;
        }
        assert!(rig.owner.state.wins[0].pending.is_none(), "the slide committed");
        assert!(rig.shelf(Kind::Movie).window.start > before);
        seen.extend(drawn(&rig, Kind::Movie));
    }
    let left = rig.fake.lock().unwrap().fail_listings;
    assert_eq!(left, 0, "both injected failures were met");
    let want: std::collections::BTreeSet<(u16, String)> = (0..3u16)
        .flat_map(|s| (0..100).map(move |i| (s, i.to_string()))).collect();
    let missing: Vec<_> = want.difference(&seen).collect();
    assert!(missing.is_empty(), "{} hits never drawn, first {:?}", missing.len(), missing.first());
}

#[test]
fn a_slide_waiting_on_a_source_that_left_the_roster_does_not_wait_for_ever() {
    let mut rig = Rig::new(2, Fake::new().movies(100));
    let lo = rig.owner.state.wins[0].lo;
    rig.owner.state.wins[0].pending = Some(Pending { lo: lo + 4, hi: lo + 16, todo: vec![57], ..Default::default() });
    assert!(prune_pending(&mut rig.owner.state, &slots()), "the slide committed without it");
    assert_eq!(rig.owner.state.wins[0].lo, lo + 4);
    assert!(rig.owner.state.wins[0].pending.is_none());
}

/// Page `kind` forward to its end with server 2 failing, asserting every slide commits within the
/// bounded number of tries; returns every (server, key) drawn on the way.
fn walk_forward_bounded(rig: &mut Rig, kind: Kind) -> std::collections::BTreeSet<(u16, String)> {
    let bound = (LANDING_TRIES as usize + 1) * RETRY_FRAMES as usize;
    let mut seen: std::collections::BTreeSet<(u16, String)> = drawn(rig, kind).into_iter().collect();
    for _ in 0..200 {
        if !rig.shelf(kind).window.after { break; }
        assert!(rig.page_settled(kind, bound), "the slide did not wait for a server that keeps failing");
        seen.extend(drawn(rig, kind));
    }
    assert!(!rig.shelf(kind).window.after, "the end of the row was reached");
    seen
}

fn hits_of(servers: std::ops::Range<u16>) -> std::collections::BTreeSet<(u16, String)> {
    servers.flat_map(|s| (0..100).map(move |i| (s, i.to_string()))).collect()
}

#[test]
fn a_server_that_never_recovers_does_not_stall_the_others() {
    let mut rig = Rig::new(3, Fake::new().movies(100));
    { let mut f = rig.fake.lock().unwrap(); f.fail_for = Some(2); f.fail_listings = usize::MAX; }
    let seen = walk_forward_bounded(&mut rig, Kind::Movie);
    let missing: Vec<_> = hits_of(0..2).difference(&seen).cloned().collect();
    assert!(missing.is_empty(), "{} hits of the healthy servers never drawn, first {:?}", missing.len(), missing.first());
}

#[test]
fn a_server_that_fails_fifty_times_then_recovers_is_merged_in_and_loses_no_hit() {
    let mut rig = Rig::new(3, Fake::new().movies(100));
    { let mut f = rig.fake.lock().unwrap(); f.fail_for = Some(2); f.fail_listings = 50; }
    let mut seen = walk_forward_bounded(&mut rig, Kind::Movie);
    let missing: Vec<_> = hits_of(0..2).difference(&seen).cloned().collect();
    assert!(missing.is_empty(), "the others were walked to the end without it");
    // the server comes back: it is asked again on its own and its hits come in at the window it is on
    for _ in 0..60 * RETRY_FRAMES {
        if rig.fake.lock().unwrap().fail_listings == 0 && !rig.owner.state.src[2].skipped[0] { break; }
        rig.drain(1);
    }
    assert!(!rig.owner.state.src[2].skipped[0], "the recovered server's hits for the window are in");
    assert!(drawn(&rig, Kind::Movie).iter().any(|(s, _)| *s == 2), "its cards joined the row");
    seen.extend(drawn(&rig, Kind::Movie));
    // and the depths already passed are read back as the row is paged back
    for _ in 0..100 {
        if rig.shelf(Kind::Movie).window.start == 0 { break; }
        let from = rig.seen(Kind::Movie);
        rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: true, seen: from });
        for _ in 0..4 * RETRY_FRAMES {
            if rig.owner.state.wins[0].pending.is_none() && !rig.owner.state.src[2].skipped[0] { break; }
            rig.drain(1);
        }
        seen.extend(drawn(&rig, Kind::Movie));
    }
    let missing: Vec<_> = hits_of(0..3).difference(&seen).cloned().collect();
    assert!(missing.is_empty(), "{} hits never drawn, first {:?}", missing.len(), missing.first());
}

/// A frame captures its views before its landings are delivered, so a tick after a slide's landing
/// can ask from the window the slide replaced. Honoured, it would slide the NEW window again, with
/// focus in the middle of it. The ask names the window it was read from and the store refuses one
/// for a window it has since slid; from the window that stands, the same ask is honoured.
#[test]
fn an_ask_computed_from_a_window_the_row_has_since_slid_is_refused() {
    let mut rig = Rig::new(1, Fake::new().movies(300));
    // the preview fills where it stands: an ask computed from its twelve cards is stale too
    let preview = rig.seen(Kind::Movie);
    rig.page(Kind::Movie, false);
    let filled = rig.seen(Kind::Movie);
    assert_eq!((preview, filled), ((0, 12), (0, 24)), "the fill landed");
    rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen: preview });
    rig.drain(6 * nsrc() + 12);
    assert_eq!(rig.seen(Kind::Movie), filled, "the filled window did not slide");
    let first = filled.0;
    rig.page(Kind::Movie, false);
    let slid = rig.shelf(Kind::Movie).window.start;
    assert!(slid > first, "the slide landed");
    let keys = rig.keys(Kind::Movie);
    rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen: filled });
    rig.drain(6 * nsrc() + 12);
    assert_eq!((rig.shelf(Kind::Movie).window.start, rig.keys(Kind::Movie)), (slid, keys), "the window did not move");
    assert!(rig.owner.state.wins[0].pending.is_none());
    // the way back, from the window that stands
    rig.page(Kind::Movie, true);
    assert_eq!(rig.shelf(Kind::Movie).window.start, first);
}

/// The shelf repeats an ask nobody answered and the page sends each repeat on: an identical ask
/// while the slide is pending is neither a second slide nor a second read of any source, and one
/// refused for naming a replaced window (`seen`) is taken when sent again from the window that
/// stands.
#[test]
fn a_repeated_slide_ask_reads_and_slides_once_and_a_refused_one_is_taken_when_resent() {
    let reads = |repeats: usize| {
        let mut rig = Rig::new(2, Fake::new().movies(300));
        rig.fake.lock().unwrap().log.clear();
        let seen = rig.seen(Kind::Movie);
        for _ in 0..repeats {
            rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen });
        }
        rig.drain(6 * nsrc() + 12);
        let log = rig.fake.lock().unwrap().log.len();
        (log, rig.seen(Kind::Movie), rig.keys(Kind::Movie))
    };
    let once = reads(1);
    assert!(once.0 > 0 && once.1 .1 > 12, "one ask reads and the window moves: {once:?}");
    assert_eq!(reads(5), once, "five identical asks read and slide exactly as one does");
    let mut rig = Rig::new(2, Fake::new().movies(300));
    let stale = rig.seen(Kind::Movie);
    rig.page(Kind::Movie, false);
    let moved = rig.seen(Kind::Movie);
    assert_ne!(moved, stale);
    rig.fake.lock().unwrap().log.clear();
    rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen: stale });
    rig.drain(6 * nsrc() + 12);
    assert_eq!((rig.seen(Kind::Movie), rig.fake.lock().unwrap().log.len()), (moved, 0), "refused: no slide, no read");
    rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen: moved });
    rig.drain(6 * nsrc() + 12);
    assert_ne!(rig.seen(Kind::Movie), moved, "sent again from the window that stands, it is taken");
}

/// What tells the shelf an ask was answered when the window it sees did not move: the row's epoch
/// moves on every slide that commits and does not on a read that failed, and a new query starts
/// the count over with the row.
#[test]
fn every_committed_slide_moves_the_rows_epoch_and_a_failed_read_does_not() {
    let mut rig = Rig::new(1, Fake::new().movies(300));
    let epoch = |rig: &Rig| rig.shelf(Kind::Movie).epoch;
    let first = epoch(&rig);
    rig.fake.lock().unwrap().fail_listings = usize::MAX;
    rig.page(Kind::Movie, false);
    assert!(rig.owner.state.wins[0].pending.is_some(), "the slide is owed");
    assert_eq!(epoch(&rig), first, "a read that failed answered nothing");
    rig.fake.lock().unwrap().fail_listings = 0;
    assert!(rig.page_settled(Kind::Movie, 800));
    assert_eq!(epoch(&rig), first + 1, "the slide that committed is one answer");
    let second = epoch(&rig);
    rig.page(Kind::Movie, false);
    assert_eq!(epoch(&rig), second + 1, "and so is the next");
}

/// Reach survives the guard: every slide's landing is met by a stale ask from the window it
/// replaced (the worst frame order), and the row still reaches its last card.
#[test]
fn a_row_whose_every_landing_meets_a_stale_ask_still_reaches_its_last_card() {
    let mut rig = Rig::new(1, Fake::new().movies(300));
    let mut seen = rig.keys(Kind::Movie);
    for _ in 0..200 {
        if !rig.shelf(Kind::Movie).window.after { break; }
        let stale = rig.seen(Kind::Movie);
        rig.page(Kind::Movie, false);
        rig.owner.state.run(&rig.owner.adapter, SearchCmd::Page { kind: Kind::Movie, before: false, seen: stale });
        rig.drain(6 * nsrc() + 12);
        seen.extend(rig.keys(Kind::Movie));
    }
    assert!(!rig.shelf(Kind::Movie).window.after, "the last card was reached");
    seen.sort_by_key(|k| k.parse::<usize>().unwrap());
    seen.dedup();
    assert_eq!(seen, numbers(0..300));
}
