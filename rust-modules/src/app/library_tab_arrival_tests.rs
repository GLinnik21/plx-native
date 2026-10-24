//! A Library page reached from a strip pill: how its first focused tile arrives.
//!
//! The pill press mounts the page's fade at 0. The page arrives in its first poster
//! (`PageRegions`, `Arrival::Content`), seated DORMANT: no tile is lifted while the page dissolves
//! in, where the pop would play out unseen (the viewer saw a tile that was simply there, which is
//! why the first arrival once seated the header row). The lift starts on the dissolve's last
//! frame, on the standard focus-lift spring with no extra beat, and is full by its end.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

/// The frames the standard focus-lift spring needs from its first lifted frame to rest (a
/// generous bound: the lift is at its full scale well inside it).
const LIFT_FRAMES: usize = 40;

#[test]
fn a_pill_arrival_seats_the_first_poster_dormant_and_lifts_it_when_the_dissolve_ends() {
    let _guard = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("library-tab-arrival");
    session.watching("u-library-tab-arrival");
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_plex::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    plx_plex::plex::reset_servers_for_test();
    let own = plx_plex::plex::register_for_test("arrive-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = plx_plex::plex::register_for_test("arrive-shared", "127.0.0.1", 10, "synthetic", "fixture");
    plx_plex::plex::set_current(own);
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().seed_registered_table_for_test([own, shared]);
    rig.refresh_browse_directory();
    rig.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
    rig.stores.browse.borrow_mut().seed_items_for_test(12);
    // What `nav_tab` queues for a Movies press, before the page exists.
    rig.enter_library(plx_data::browse::SecKind::Movie);
    let probe = |d: &Dispatcher<AppHost>| {
        let page = d.top_screen().unwrap().as_any().unwrap()
            .downcast_ref::<plx_screens::library::LibraryScreen>().unwrap();
        (page.probe_viewport(d.focus()).0, page.probe_arrival(d.focus()))
    };
    // (frame, region, page alpha, the focused tile's drawn scale)
    let mut timeline = Vec::new();
    for i in 0..120 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        let (region, (alpha, scale)) = probe(&d);
        timeline.push((i, region, alpha, scale));
    }
    // Frame 0 still shows the pill that was pressed ("strip"); from the first seat on it is a tile.
    let seated: Vec<_> = timeline.iter().filter(|(_, region, ..)| !matches!(*region, "none" | "strip")).collect();
    assert!(!seated.is_empty(), "nothing took focus");
    assert!(seated.iter().all(|(_, region, ..)| *region == "grid"),
        "the arrival must seat the first poster, never a header control: {seated:?}");
    // Nothing is lifted while the page is still dissolving in.
    let early: Vec<_> = timeline.iter().filter(|(_, _, alpha, scale)| *alpha < 1.0 && *scale != 1.0).collect();
    assert!(early.is_empty(), "a tile lifted under the dissolve: {early:?}");
    // The lift starts on the dissolve's last frame ...
    let began = timeline.iter().find(|(_, _, alpha, _)| *alpha < 1.0).map(|(i, ..)| *i).expect("the page never dissolved in");
    let opaque = timeline.iter().find(|(i, _, alpha, _)| *i > began && *alpha >= 1.0).map(|(i, ..)| *i).expect("the page never became opaque");
    let lifting = timeline.iter().find(|(_, _, _, scale)| *scale > 1.0).map(|(i, ..)| *i).expect("the seated tile never lifted");
    assert!(lifting <= opaque + 1, "the lift waited a beat after the dissolve: opaque at {opaque}, lifting from {lifting}");
    // ... grows monotonically from rest and is full by the end of its spring.
    let scales: Vec<f32> = timeline.iter().skip(lifting as usize).map(|(_, _, _, scale)| *scale).collect();
    assert!(scales.windows(2).all(|pair| pair[1] >= pair[0] - 1e-4), "the lift is not a monotone growth: {scales:?}");
    let first = scales[0];
    let full = scales.last().copied().unwrap();
    assert!(first > 1.0 && first < full - 0.02, "the lift starts from rest, not whole: {first} -> {full}");
    let settled = scales.get(LIFT_FRAMES).copied().expect("the timeline ends before the lift's spring does");
    assert!((settled - full).abs() < 0.002, "the lift is still filling {LIFT_FRAMES} frames after it began: {settled} vs {full}");
}
