//! The quiet per-source refetch and the visibility-gated commit of what it brought (layer-2 L2-3):
//! `HubsCmd::RefetchSourceQuiet` stages a source's answer on the source, `HubsCmd::Publish`
//! decides in data when it may land.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::land;
use crate::stores::hubs::HubsCmd;

/// One owner with `sources` registered servers that have each landed a first read.
struct Rig {
    o: Owner,
    slots: Vec<u16>,
}

/// Source 0's first answer: three Continue Watching cards, then two shelves. Catalog rows are
/// `0` Continue Watching, `1` Recently Added, `2` Top (source 1, when there is one, follows).
fn first_build(slot: u16) -> SourceBuild {
    built(slot, &[(30, "c1"), (20, "c2"), (10, "c3")], vec![
        shelf(slot, "Recently Added", "home.movies.recent", &["a1", "a2", "a3"]),
        shelf(slot, "Top", "home.top", &["t1", "t2"]),
    ])
}

/// The next read of source 0 when only Continue Watching moved: a new card first.
fn cw_moved(slot: u16) -> SourceBuild {
    built(slot, &[(40, "c0"), (30, "c1"), (20, "c2")], vec![
        shelf(slot, "Recently Added", "home.movies.recent", &["a1", "a2", "a3"]),
        shelf(slot, "Top", "home.top", &["t1", "t2"]),
    ])
}

/// …when only the Top shelf (row 2) changed.
fn top_moved(slot: u16) -> SourceBuild {
    built(slot, &[(30, "c1"), (20, "c2"), (10, "c3")], vec![
        shelf(slot, "Recently Added", "home.movies.recent", &["a1", "a2", "a3"]),
        shelf(slot, "Top", "home.top", &["t1", "t3"]),
    ])
}

/// …when a shelf is gone (a row removed).
fn shelf_removed(slot: u16) -> SourceBuild {
    built(slot, &[(30, "c1"), (20, "c2"), (10, "c3")], vec![
        shelf(slot, "Recently Added", "home.movies.recent", &["a1", "a2", "a3"]),
    ])
}

/// …when the two shelves swapped places.
fn shelves_swapped(slot: u16) -> SourceBuild {
    built(slot, &[(30, "c1"), (20, "c2"), (10, "c3")], vec![
        shelf(slot, "Top", "home.top", &["t1", "t2"]),
        shelf(slot, "Recently Added", "home.movies.recent", &["a1", "a2", "a3"]),
    ])
}

fn other_build(slot: u16) -> SourceBuild {
    built(slot, &[], vec![shelf(slot, "Theirs", "home.theirs", &["x1", "x2"])])
}

fn rig(sources: usize) -> Rig {
    rig_of((0..sources).map(|i| Box::new(move |slot| if i == 0 { first_build(slot) } else { other_build(slot) })
        as Box<dyn Fn(u16) -> SourceBuild>).collect())
}

/// [`rig`] with each source's first answer chosen: `builds[i](slot)`.
fn rig_of(builds: Vec<Box<dyn Fn(u16) -> SourceBuild>>) -> Rig {
    let mut o = Owner::default();
    plx_plex::plex::reset_servers_for_test();
    let slots: Vec<u16> = (0..builds.len())
        .map(|i| plx_plex::plex::register_for_test(&format!("staged-{i}"), "127.0.0.1", 9 + i as i32,
            "synthetic", "cid").raw())
        .collect();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, slots.iter().map(|&slot| src(slot, "", HubState::Loading, None)).collect());
    for (&slot, build) in slots.iter().zip(&builds) {
        land(&o.state, &o.adapter, slot, Some(build(slot)));
    }
    pump(&mut o.state, &o.adapter, 0.0);
    assert!(o.state.srcs.iter().all(|s| s.state == HubState::Ready && s.staged.is_none()),
        "premise: every source answered");
    Rig { o, slots }
}

impl Rig {
    fn sid(&self, i: usize) -> ServerId { sid(self.slots[i]) }

    /// Run `cmd` through the controlled path; the requests it launched are returned.
    fn run(&mut self, cmd: Option<HubsCmd>, dt: f32) -> (crate::stores::StoreOutcome, Vec<HubRequest>) {
        let mut held = Vec::new();
        let outcome = controlled_work(&mut self.o.state, &self.o.adapter, cmd, dt, &mut |r| { held.push(r); true });
        (outcome, held)
    }

    fn quiet(&mut self, i: usize) -> Vec<HubRequest> {
        let sid = self.sid(i);
        self.run(Some(HubsCmd::RefetchSourceQuiet { sid }), 0.0).1
    }

    fn apply(&mut self, landing: &Landing) -> crate::stores::StoreOutcome {
        super::land(&mut self.o.state, &self.o.adapter, landing)
    }

    /// Ask quietly, answer with `build`, and take the answer in.
    fn stage(&mut self, i: usize, build: SourceBuild) {
        let mut asked = self.quiet(i);
        assert_eq!(asked.len(), 1, "premise: the quiet ask launched one read");
        let landing = asked.remove(0).complete_for_test(Some(build));
        let _ = self.apply(&landing);
        assert!(self.o.state.srcs[i].staged.is_some(), "premise: the answer is staged");
    }

    fn publish(&mut self, visible: bool, focus: Option<usize>, press: bool) -> crate::stores::StoreOutcome {
        self.run(Some(HubsCmd::Publish { home_top_visible: visible, focused_hub: focus, press_live: press }), 0.0).0
    }

    fn rows(&self) -> Vec<Vec<String>> { (0..hub_count(&self.o.state)).map(|h| rks(&self.o.state, h)).collect() }
}

impl Drop for Rig {
    fn drop(&mut self) {
        reset(&mut self.o.state, &self.o.adapter);
        plx_plex::plex::reset_servers_for_test();
    }
}

#[test]
fn quiet_refetch_stages_without_commit() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let (gen, published) = (r.o.state.catalog_gen, r.rows());
    let mut asked = r.quiet(0);
    assert_eq!(asked.len(), 1, "a quiet ask is a read");
    assert!(r.o.state.srcs[0].fetching);
    assert_eq!(r.o.state.srcs[0].state, HubState::Ready, "and not a spinner: the source stays Ready");
    let outcome = r.apply(&asked.remove(0).complete_for_test(Some(cw_moved(r.slots[0]))));
    assert!(!outcome.changed, "staging publishes nothing");
    assert_eq!(r.o.state.catalog_gen, gen, "catalog_gen does not move");
    assert_eq!(r.rows(), published, "the published rows are as they were");
    let s = &r.o.state.srcs[0];
    assert!(!s.fetching && !s.quiet, "the flight is over");
    let staged = s.staged.as_ref().expect("the answer is held on the source");
    assert_eq!(staged.cw[0].m.rk, "c0");
    assert_eq!(s.last.as_ref().unwrap().cw[0].m.rk, "c1", "and `last` is untouched");
}

#[test]
fn quiet_refetch_never_bumps_hub_gen() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(2);
    let hub_gen = r.o.state.hub_gen;
    let other = (r.o.state.srcs[1].seq, r.o.state.srcs[1].fetching, r.o.state.srcs[1].state);
    let mut asked = r.quiet(0);
    assert_eq!(r.o.state.hub_gen, hub_gen, "the ask");
    assert_eq!(asked.len(), 1, "one read, for the one source");
    assert_eq!(asked[0].descriptor().2, r.slots[0]);
    assert_eq!(asked[0].descriptor().0, hub_gen, "stamped with the generation that stands");
    let _ = r.apply(&asked.remove(0).complete_for_test(Some(cw_moved(r.slots[0]))));
    assert_eq!(r.o.state.hub_gen, hub_gen, "the landing");
    assert_eq!((r.o.state.srcs[1].seq, r.o.state.srcs[1].fetching, r.o.state.srcs[1].state), other,
        "no other source was touched");
    assert!(r.o.state.srcs[1].staged.is_none());
}

#[test]
fn quiet_refetch_moves_the_sequence_so_a_superseded_landing_is_dropped() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let before = r.o.state.srcs[0].seq;
    let mut asked = r.quiet(0);
    assert_ne!(r.o.state.srcs[0].seq, before, "`kick_gated` admits the read through `begin_request`, which mints a sequence");
    assert_eq!(r.o.state.srcs[0].seq, asked[0].descriptor().1);
    let landing = asked.remove(0).complete_for_test(Some(cw_moved(r.slots[0])));
    r.o.state.srcs[0].seq = r.o.state.srcs[0].seq.wrapping_add(1); // a newer attempt took the source over
    let _ = r.apply(&landing);
    assert!(r.o.state.srcs[0].staged.is_none(), "a superseded quiet landing is dropped whole");
    assert_eq!(r.o.state.srcs[0].last.as_ref().unwrap().cw[0].m.rk, "c1");
}

#[test]
fn quiet_refetch_skipped_while_source_fetching_or_paging() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(2);
    r.o.state.srcs[0].fetching = true;
    assert!(r.quiet(0).is_empty(), "the source is out already");
    r.o.state.srcs[0].fetching = false;
    r.o.state.srcs[0].page = Some(PageQuery { id: "home.movies.recent".into(), key: "k".into(), start: 0,
        before: false, hidden: Vec::new(), reload: false });
    assert!(r.quiet(0).is_empty(), "a page ask for the source is pending");
    r.o.state.srcs[0].page = None;
    r.o.state.srcs[1].fetching = true; // an ordinary read of ANOTHER source is out
    assert!(r.quiet(0).is_empty(), "a prompted read is in flight: it will publish what this would have staged");
    r.o.state.srcs[1].fetching = false;
    r.o.state.srcs[0].state = HubState::Failed;
    assert!(r.quiet(0).is_empty(), "a source that is not Ready has its own retry");
    assert!(!r.o.state.srcs[0].quiet, "and nothing is left marked quiet");
    r.o.state.srcs[0].state = HubState::Ready;
    assert_eq!(r.quiet(0).len(), 1, "premise: it asks when nothing is in the way");
}

#[test]
fn a_quiet_ask_the_gate_deferred_is_still_quiet_when_a_tick_launches_it() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    {
        let s = &mut r.o.state.srcs[0];
        s.quiet = true;
        s.deferred = true; // the fan-out gate had no room when it was asked
    }
    let (_, mut asked) = r.run(None, 0.0);
    assert_eq!(asked.len(), 1, "the tick launches it");
    let (gen, published) = (r.o.state.catalog_gen, r.rows());
    let landing = asked.remove(0).complete_for_test(Some(cw_moved(r.slots[0])));
    let _ = r.apply(&landing);
    assert_eq!(r.o.state.catalog_gen, gen, "and it is staged, not published");
    assert_eq!(r.rows(), published);
    assert!(r.o.state.srcs[0].staged.is_some());
}

#[test]
fn prompted_refetch_supersedes_and_clears_staged() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let mut quiet = r.quiet(0);
    assert_eq!(quiet.len(), 1, "premise: a quiet read is out");
    r.o.state.srcs[0].staged = Some(cw_moved(r.slots[0])); // …and an older answer is held
    let hub_gen = r.o.state.hub_gen;
    // the prompted refetch: every flight is released and the staged answer is dropped
    let (outcome, prompted) = r.run(Some(HubsCmd::RefetchHubs), 0.0);
    assert!(outcome.changed);
    assert_eq!(prompted.len(), 1);
    assert_ne!(r.o.state.hub_gen, hub_gen, "an authoritative fetch bumps the generation");
    assert!(r.o.state.srcs[0].staged.is_none(), "a prompted refetch clears what was staged");
    assert!(!r.o.state.srcs[0].quiet, "and the read it started is not a quiet one");
    // …and the quiet read that was out lands into a generation that has gone
    let (gen, published) = (r.o.state.catalog_gen, r.rows());
    let _ = r.apply(&quiet.remove(0).complete_for_test(Some(cw_moved(r.slots[0]))));
    assert!(r.o.state.srcs[0].staged.is_none(), "the stale quiet landing is dropped");
    assert_eq!((r.o.state.catalog_gen, r.rows()), (gen, published));
    assert!(r.o.state.srcs[0].fetching, "the prompted read is still the one in flight");
}

#[test]
fn a_reset_clears_staged() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, cw_moved(r.slots[0]));
    reset(&mut r.o.state, &r.o.adapter);
    assert!(r.o.state.srcs.iter().all(|s| s.staged.is_none()), "no source, so nothing staged");
}

#[test]
fn edit_during_staging_survives_commit() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, top_moved(r.slots[0]));
    let sid0 = r.sid(0);
    let at = ViewAt::leaf(sid0, "a2");
    let outcome = run(&mut r.o.state, &r.o.adapter, HubsCmd::ApplyView { at, edit: ViewEdit::Watched(true) });
    assert!(outcome.changed);
    assert!(r.o.state.srcs[0].staged.as_ref().unwrap().shelves[0].items[1].watched,
        "the edit reached the staged copy as well");
    let _ = r.publish(false, None, false);
    assert!(hub_item(&r.o.state, 1, 1).unwrap().watched, "the committed read still carries the edit");
    assert_eq!(rks(&r.o.state, 2), ["t1", "t3"], "and what the read brought");
}

#[test]
fn edit_item_commit_leaves_staged_alone() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, top_moved(r.slots[0]));
    let sid0 = r.sid(0);
    let _ = run(&mut r.o.state, &r.o.adapter, HubsCmd::ApplyView { at: ViewAt::leaf(sid0, "a2"), edit: ViewEdit::Watched(true) });
    assert!(hub_item(&r.o.state, 1, 1).unwrap().watched, "the edit published through `last`");
    assert_eq!(rks(&r.o.state, 2), ["t1", "t2"], "…without the staged read riding along");
    let staged = r.o.state.srcs[0].staged.as_ref().expect("the staged copy is still held");
    assert_eq!(staged.shelves[1].items.iter().map(|m| m.rk.as_str()).collect::<Vec<_>>(), ["t1", "t3"]);
}

#[test]
fn publish_not_visible_commits_at_once() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, cw_moved(r.slots[0]));
    let gen = r.o.state.catalog_gen;
    // the worst case for a visible Home: a structural change with the focus on the changed row
    r.o.state.srcs[0].staged = Some(shelf_removed(r.slots[0]));
    let outcome = r.publish(false, Some(0), true);
    assert!(outcome.changed, "the outcome says the catalog moved");
    assert_eq!(r.o.state.catalog_gen, gen.wrapping_add(1), "committed once");
    assert_eq!(hub_count(&r.o.state), 2);
    assert!(r.o.state.srcs[0].staged.is_none(), "and nothing is left staged");
    assert_eq!(r.o.state.srcs[0].last.as_ref().unwrap().shelves.len(), 1);
}

#[test]
fn publish_visible_holds_focused_row() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, top_moved(r.slots[0]));
    let (gen, published) = (r.o.state.catalog_gen, r.rows());
    for focus in [Some(2), Some(1), Some(0), None] {
        let outcome = r.publish(true, focus, false);
        assert!(!outcome.changed, "focus {focus:?}: a held change publishes nothing");
        assert_eq!(r.o.state.catalog_gen, gen);
    }
    assert_eq!(r.rows(), published);
    assert!(r.o.state.srcs[0].staged.is_some(), "it waits");
    // …until Home is covered
    let _ = r.publish(false, None, false);
    assert_eq!(rks(&r.o.state, 2), ["t1", "t3"]);
}

#[test]
fn publish_visible_commits_unfocused_cw() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, cw_moved(r.slots[0]));
    let (gen, published) = (r.o.state.catalog_gen, r.rows());
    assert!(!r.publish(true, Some(0), false).changed, "the focus is in Continue Watching");
    assert!(!r.publish(true, Some(1), true).changed, "a press is armed");
    assert_eq!((r.o.state.catalog_gen, r.rows()), (gen, published));
    assert!(r.publish(true, Some(1), false).changed, "the focus is on another row, nothing is pressed");
    assert_eq!(rks(&r.o.state, 0), ["c0", "c1", "c2"], "Continue Watching took the new card");
    assert!(r.o.state.srcs[0].staged.is_none());
}

#[test]
fn publish_visible_commits_cw_when_the_focus_is_not_in_a_row() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, cw_moved(r.slots[0]));
    assert!(r.publish(true, None, false).changed, "the hero or the top bar holds the focus");
}

#[test]
fn publish_visible_holds_structural() {
    let _g = plx_base::testlock::serial();
    for staged in [shelf_removed as fn(u16) -> SourceBuild, shelves_swapped] {
        let mut r = rig(1);
        r.o.state.hold = (0, 0); // every changed row is off screen, and it still waits
        r.stage(0, staged(r.slots[0]));
        let (gen, published) = (r.o.state.catalog_gen, r.rows());
        assert!(!r.publish(true, Some(0), false).changed, "a row added, removed or moved shifts the ground");
        assert_eq!((r.o.state.catalog_gen, r.rows()), (gen, published));
        assert!(r.o.state.srcs[0].staged.is_some());
        assert!(r.publish(false, None, false).changed, "and lands when Home is covered");
    }
}

#[test]
fn publish_visible_commits_offscreen_rows() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, top_moved(r.slots[0]));
    assert!(!r.publish(true, Some(0), false).changed, "row 2 is inside the held window (0..=15)");
    r.o.state.hold = (0, 1); // the screen holds Continue Watching and Recently Added only
    assert!(r.publish(true, Some(0), false).changed, "row 2 is outside it: nothing the person can see moves");
    assert!(r.o.state.srcs[0].staged.is_none());
}

#[test]
fn unprompted_visible_commit_preserves_heroes() {
    let _g = plx_base::testlock::serial();
    let mut visible = rig(1);
    assert_eq!(hero_pool_item(&visible.o.state, 0).unwrap().rk, "c1", "premise: the billboard opens on c1");
    visible.stage(0, cw_moved(visible.slots[0]));
    assert!(visible.publish(true, Some(1), false).changed);
    assert_eq!(hero_pool_item(&visible.o.state, 0).unwrap().rk, "c1",
        "a commit under the person's eyes does not turn the billboard");
    drop(visible);
    let mut hidden = rig(1);
    hidden.stage(0, cw_moved(hidden.slots[0]));
    assert!(hidden.publish(false, None, false).changed);
    assert_eq!(hero_pool_item(&hidden.o.state, 0).unwrap().rk, "c0",
        "a commit while Home is covered takes the new pool");
}

#[test]
fn a_staged_commit_is_the_catalog_a_prompted_landing_would_have_made() {
    let _g = plx_base::testlock::serial();
    let mut prompted = rig(1);
    let mut asked = prompted.run(Some(HubsCmd::RefetchHubs), 0.0).1;
    let landing = asked.remove(0).complete_for_test(Some(cw_moved(prompted.slots[0])));
    let _ = prompted.apply(&landing);
    let want = (prompted.rows(), hero_pool_item(&prompted.o.state, 0).map(|m| m.rk.clone()));
    drop(prompted);
    let mut staged = rig(1);
    staged.stage(0, cw_moved(staged.slots[0]));
    let _ = staged.publish(false, None, false);
    let got = (staged.rows(), hero_pool_item(&staged.o.state, 0).map(|m| m.rk.clone()));
    assert_eq!(got, want, "deck placement and merge are the landing's");
    assert!(staged.o.state.srcs[0].last.as_ref().unwrap().lane.placed, "the deck was placed on the lane");
}

#[test]
fn failed_quiet_fetch_leaves_published_and_generation() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let (gen, hub_gen, published) = (r.o.state.catalog_gen, r.o.state.hub_gen, r.rows());
    let mut asked = r.quiet(0);
    let outcome = r.apply(&asked.remove(0).complete_for_test(None));
    assert!(!outcome.changed);
    assert_eq!((r.o.state.catalog_gen, r.o.state.hub_gen, r.rows()), (gen, hub_gen, published));
    let s = &r.o.state.srcs[0];
    assert_eq!(s.state, HubState::Ready, "a quiet failure is not a failed source");
    assert!(!s.fetching && !s.quiet && s.staged.is_none());
    assert_eq!((s.retry_n, s.retry_s), (0, 0.0), "no backoff is armed");
    assert!(outcome.endpoints.iter().next().is_none(),
        "a quiet failure asks no rediscovery: the prompted retry owns that");
    let (_, asked) = r.run(None, 60.0);
    assert!(asked.is_empty(), "and no later tick retries it as a prompted read");
}

#[test]
fn a_quiet_launch_the_system_refused_leaves_the_source_alone() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let sid0 = r.sid(0);
    let outcome = controlled_work(&mut r.o.state, &r.o.adapter, Some(HubsCmd::RefetchSourceQuiet { sid: sid0 }), 0.0,
        &mut |_| false);
    let s = &r.o.state.srcs[0];
    assert_eq!((s.state, s.fetching, s.quiet, s.retry_n), (HubState::Ready, true, true, 0),
        "the refused read is out until the next step settles it");
    assert!(outcome.endpoints.iter().next().is_none());
    let outcome = controlled_work(&mut r.o.state, &r.o.adapter, None, 0.0, &mut |_| false);
    let s = &r.o.state.srcs[0];
    assert_eq!((s.state, s.fetching, s.quiet, s.retry_n), (HubState::Ready, false, false, 0));
    assert!(outcome.endpoints.iter().next().is_none(), "and asks no rediscovery");
}

#[test]
fn a_page_landing_drops_what_was_staged_for_its_source() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, top_moved(r.slots[0]));
    r.o.state.srcs[0].staged = Some(top_moved(r.slots[0]));
    let page = PageQuery { id: "home.movies.recent".into(), key: "k".into(), start: 3, before: false,
        hidden: Vec::new(), reload: false };
    {
        let s = &mut r.o.state.srcs[0];
        s.page = Some(page);
        s.fetching = true;
    }
    land(&r.o.state, &r.o.adapter, r.slots[0], Some(built(r.slots[0], &[], vec![
        shelf(r.slots[0], "Recently Added", "home.movies.recent", &["a4", "a5"])])));
    pump(&mut r.o.state, &r.o.adapter, 0.0);
    assert!(r.o.state.srcs[0].staged.is_none(),
        "the window moved under the staged copy, whose rows would put it back");
}

#[test]
fn a_quiet_read_in_flight_does_not_unsettle_the_source() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let sid0 = r.sid(0);
    assert!(source_settled(&r.o.state, sid0), "premise: nothing is out");
    let mut quiet = r.quiet(0);
    assert!(r.o.state.srcs[0].fetching, "premise: the quiet read is out");
    assert!(source_settled(&r.o.state, sid0),
        "the return hold waits for a prompted read; a quiet one is not what it is waiting for");
    // …and a prompted read for the same source is
    let _prompted = r.run(Some(HubsCmd::RefetchHubs), 0.0);
    assert!(!source_settled(&r.o.state, sid0));
    let _ = r.apply(&quiet.remove(0).complete_for_test(None)); // (dropped: its generation has gone)
}

fn many_cw(prefix: &'static str, newest: i64, n: usize) -> impl Fn(u16) -> SourceBuild {
    move |slot| {
        let rks: Vec<String> = (0..n).map(|i| format!("{prefix}{i}")).collect();
        let cw: Vec<(i64, &str)> = rks.iter().enumerate().map(|(i, rk)| (newest - i as i64, rk.as_str())).collect();
        built(slot, &cw, vec![shelf(slot, "Recently Added", "home.movies.recent", &["a1", "a2", "a3"])])
    }
}

fn lane_json(r: &Rig, i: usize) -> String {
    serde_json::to_string(&r.o.state.srcs[i].last.as_ref().unwrap().lane).unwrap()
}

#[test]
fn a_waiting_publish_leaves_every_other_sources_deck_lane_alone() {
    let _g = plx_base::testlock::serial();
    let mut r = rig_of(vec![Box::new(many_cw("p", 1000, 14)), Box::new(many_cw("q", 500, 14))]);
    let (lane0, lane1) = (lane_json(&r, 0), lane_json(&r, 1));
    // source 0 now has more, newer cards than its share of the bound: fitting the deck shrinks
    // source 1's window
    r.stage(0, many_cw("n", 2000, 20)(r.slots[0]));
    let gen = r.o.state.catalog_gen;
    assert!(!r.publish(true, Some(0), false).changed, "premise: the focus is in Continue Watching, so it waits");
    assert_eq!(r.o.state.catalog_gen, gen);
    assert_eq!((lane_json(&r, 0), lane_json(&r, 1)), (lane0, lane1),
        "a change that waits changes no source's published deck lane");
}

#[test]
fn an_unrelated_edit_after_a_waiting_publish_leaves_the_published_deck_alone() {
    let _g = plx_base::testlock::serial();
    let mut r = rig_of(vec![Box::new(many_cw("p", 1000, 14)), Box::new(many_cw("q", 500, 14))]);
    let deck = rks(&r.o.state, 0);
    r.stage(0, many_cw("n", 2000, 20)(r.slots[0]));
    assert!(!r.publish(true, Some(0), false).changed);
    let sid0 = r.sid(0);
    let _ = run(&mut r.o.state, &r.o.adapter, HubsCmd::ApplyView { at: ViewAt::leaf(sid0, "a2"), edit: ViewEdit::Watched(true) });
    assert_eq!(rks(&r.o.state, 0), deck, "the edit republishes the deck it had, not the waiting read's share");
}

#[test]
fn a_page_ask_supersedes_a_deferred_quiet_read() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    {
        let s = &mut r.o.state.srcs[0];
        s.quiet = true;
        s.deferred = true; // the gate had no room for the quiet ask
        s.page = Some(PageQuery { id: "home.movies.recent".into(), key: String::new(), start: 3, before: false,
            hidden: Vec::new(), reload: false }); // …and then a page ask came for the same source
    }
    let (_, mut asked) = r.run(None, 0.0);
    assert_eq!(asked.len(), 1);
    assert!(asked[0].page_descriptor().is_some(), "the read that went out is the page read");
    let landing = asked.remove(0).complete_for_test(Some(built(r.slots[0], &[], vec![
        shelf(r.slots[0], "Recently Added", "home.movies.recent", &["a4", "a5"])])));
    let _ = r.apply(&landing);
    let s = &r.o.state.srcs[0];
    assert!(s.staged.is_none(), "a page's one-shelf answer is not a source's whole answer to stage");
    assert!(!s.quiet);
}

#[test]
fn a_page_landing_that_dropped_the_staging_asks_the_quiet_read_again() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, top_moved(r.slots[0]));
    {
        let s = &mut r.o.state.srcs[0];
        s.page = Some(PageQuery { id: "home.movies.recent".into(), key: "k".into(), start: 3, before: false,
            hidden: Vec::new(), reload: false });
        s.fetching = true;
    }
    land(&r.o.state, &r.o.adapter, r.slots[0], Some(built(r.slots[0], &[], vec![
        shelf(r.slots[0], "Recently Added", "home.movies.recent", &["a4", "a5"])])));
    pump(&mut r.o.state, &r.o.adapter, 0.0);
    assert!(r.o.state.srcs[0].staged.is_none());
    let (_, asked) = r.run(None, 120.0);
    assert_eq!(asked.len(), 1, "the update the page landing dropped is asked for again");
    assert!(asked[0].page_descriptor().is_none(), "as a whole-source read");
    assert!(r.o.state.srcs[0].quiet);
}

#[test]
fn a_quiet_ask_refused_for_the_sources_own_page_flight_is_owed() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    {
        let s = &mut r.o.state.srcs[0];
        s.fetching = true;
        s.page = Some(PageQuery { id: "home.movies.recent".into(), key: "k".into(), start: 3, before: false,
            hidden: Vec::new(), reload: false });
    }
    assert!(r.quiet(0).is_empty(), "refused while the page read is out");
    {
        let s = &mut r.o.state.srcs[0];
        s.fetching = false; // the page read landed
        s.page = None;
    }
    let (_, asked) = r.run(None, 120.0);
    assert_eq!(asked.len(), 1, "the refused ask is made again once the source is idle");
}

#[test]
fn another_sources_page_flight_does_not_block_a_quiet_ask() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(2);
    r.o.state.srcs[1].fetching = true;
    r.o.state.srcs[1].page = Some(PageQuery { id: "home.theirs".into(), key: "k".into(), start: 3, before: false,
        hidden: Vec::new(), reload: false });
    assert_eq!(r.quiet(0).len(), 1, "scrolling a row of one server must not silence the other's update");
}

#[test]
fn a_deck_advance_owes_the_quiet_read_for_what_it_dropped() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, cw_moved(r.slots[0]));
    advance_deck(&mut r.o.state.srcs, false, 1);
    assert!(r.o.state.srcs[0].staged.is_none(), "the lane moved under the staged copy");
    let (_, asked) = r.run(None, 120.0);
    assert_eq!(asked.len(), 1, "and its update is asked for again");
}

#[test]
fn quiet_asks_inside_the_spacing_coalesce_into_one_later_read() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    r.stage(0, cw_moved(r.slots[0]));
    assert!(r.quiet(0).is_empty(), "a second ask straight after the first read is coalesced");
    assert!(r.quiet(0).is_empty());
    let (_, asked) = r.run(None, 120.0);
    assert_eq!(asked.len(), 1, "and made once the spacing has run out");
}

#[test]
fn a_failed_quiet_read_backs_off_before_the_next() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let mut asked = r.quiet(0);
    let _ = r.apply(&asked.remove(0).complete_for_test(None));
    assert!(r.quiet(0).is_empty(), "no new read straight after a failure");
    let (_, again) = r.run(None, 120.0);
    assert_eq!(again.len(), 1, "the ask that came in meanwhile is made once the back-off ran out");
}

#[test]
fn a_metadata_only_change_commits_even_under_a_visible_home() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    let mut build = first_build(r.slots[0]);
    build.shelves[1].total = 90; // "Top" now reports a longer listing; its cards are the same
    r.stage(0, build);
    let gen = r.o.state.catalog_gen;
    assert!(r.publish(true, Some(2), false).changed, "nothing on screen moves, so nothing waits");
    assert_eq!(r.o.state.catalog_gen, gen.wrapping_add(1));
    assert!(r.o.state.srcs[0].staged.is_none());
}

#[test]
fn nothing_staged_is_known_without_a_scope() {
    let _g = plx_base::testlock::serial();
    let mut r = rig(1);
    assert!(!has_staged(&r.o.state));
    r.stage(0, cw_moved(r.slots[0]));
    assert!(has_staged(&r.o.state));
}


/// A source whose deck is one episode of show 4 (`e`) after a movie that moves (`lead`).
fn show_build(slot: u16, lead: Option<&str>, episode: Option<&str>) -> SourceBuild {
    let mut cw: Vec<(i64, &str)> = Vec::new();
    if let Some(lead) = lead { cw.push((40, lead)); }
    if let Some(episode) = episode { cw.push((30, episode)); }
    let mut build = built(slot, &cw, Vec::new());
    for item in &mut build.cw {
        if Some(item.m.rk.as_str()) == episode {
            let m = Arc::make_mut(&mut item.m);
            (m.kind, m.show_guid, m.part) = (3, Some([4; 12]), "/library/parts/1/file.mkv".into());
        }
    }
    build
}

/// The publish gate's trial merge settles the lanes and feeds the copies; when the commit waits, the
/// facts it fed must go back with the lanes.
#[test]
fn a_trial_merge_that_waits_leaves_nothing_behind() {
    let _g = plx_base::testlock::serial();
    let mut r = rig_of(vec![
        Box::new(|slot| show_build(slot, None, Some("a-e"))),
        Box::new(|slot| show_build(slot, None, Some("b-e"))),
    ]);
    let held = |r: &Rig| r.o.state.srcs.iter().map(|s| s.copies.rks()).collect::<Vec<_>>();
    assert_eq!(held(&r), [vec!["a-e".to_string()], vec!["b-e".to_string()]], "premise: both copies are held");
    let (gen, published) = (r.o.state.catalog_gen, r.rows());
    // source 0's next answer lists a new first card and no longer lists its copy
    r.stage(0, show_build(r.slots[0], Some("lead"), None));
    let outcome = r.publish(true, Some(0), false);
    assert!(!outcome.changed && r.o.state.catalog_gen == gen, "premise: the focused deck row holds the change");
    assert!(r.o.state.srcs[0].staged.is_some(), "premise: it waits");
    assert_eq!(r.rows(), published);
    assert_eq!(held(&r), [vec!["a-e".to_string()], vec!["b-e".to_string()]], "the trial left no feed behind");
}
