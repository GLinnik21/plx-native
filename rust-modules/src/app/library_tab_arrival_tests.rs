//! A Library page reached from a strip pill: how its first focused tile arrives.
//!
//! The pill press mounts the page's fade at 0. Since the header restructure the page leads with
//! its header row, so the arrival seats focus THERE: no tile is selected, so no pop can play out
//! under the dissolve (the old head-tile arrival did, and the viewer saw a tile that was simply
//! there).

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

#[test]
fn a_pill_arrival_seats_the_header_row_and_never_pops_a_tile_under_the_dissolve() {
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
        (page.probe_viewport(d.focus()).0, page.probe_content().0)
    };
    let mut timeline = Vec::new();
    for i in 0..90 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        timeline.push((i, probe(&d)));
    }
    // Frame 0 still shows the pill that was pressed ("strip"); from the first seat on it is the header.
    let seated: Vec<_> = timeline.iter().filter(|(_, (region, _))| !matches!(*region, "none" | "strip")).collect();
    assert!(!seated.is_empty(), "nothing took focus");
    assert!(seated.iter().all(|(_, (region, _))| *region == "toolbar"),
        "the arrival must seat the header row, never a tile: {seated:?}");
    assert!(timeline.iter().any(|(_, (_, alpha))| *alpha >= 1.0), "the page never became opaque");
}
