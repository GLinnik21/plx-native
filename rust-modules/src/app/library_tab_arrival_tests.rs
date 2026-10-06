//! A Library page reached from a strip pill: how its first focused tile arrives.
//!
//! The pill press mounts the page's fade at 0 and reveals it once its shelves land. The head
//! tile is seated while the page is still dark, so a pop that starts with the seat plays out
//! under the dissolve: the tile is most of the way up by the time anything is opaque and the
//! viewer sees a tile that was simply there. The selection has to play out where it can be seen.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::{frame};

#[test]
fn a_pill_arrival_pops_its_head_tile_after_the_page_is_opaque_not_under_the_dissolve() {
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
    let arrival = |d: &Dispatcher<AppHost>| {
        d.top_screen().unwrap().as_any().unwrap()
            .downcast_ref::<plx_screens::library::LibraryScreen>().unwrap()
            .probe_arrival(d.focus())
    };
    // The shelves are a separate fetch: the page waits dark for them, as it does on the set.
    let mut timeline = Vec::new();
    for i in 0..90 {
        if i == 20 {
            rig.stores.browse.borrow_mut()
                .seed_shelves_for_test(0, &["movie.inprogress.1", "tv.recentlyreleased.1"], 3);
        }
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        if let (alpha, Some(pop)) = arrival(&d) { timeline.push((i, alpha, pop)); }
    }
    assert!(!timeline.is_empty(), "the head tile never took focus");
    let seen_growing = timeline.iter().filter(|(_, alpha, pop)| *alpha < 1.0 && *pop > 1.0).count();
    assert_eq!(seen_growing, 0,
        "the tile grew while the page was still dissolving in, so the selection played under the fade: {timeline:?}");
    let opaque = timeline.iter().position(|(_, alpha, _)| *alpha >= 1.0).expect("the page never became opaque");
    let after = &timeline[opaque..];
    assert!(after[0].2 < 1.01, "the tile was already up when the page turned opaque: {:?}", after[0]);
    assert!(after.iter().any(|(_, _, pop)| *pop > 1.05),
        "the tile never grew after the page turned opaque: {after:?}");
    let ramp: Vec<f32> = after.iter().map(|(_, _, pop)| *pop).collect();
    assert!(ramp.windows(2).all(|w| w[1] >= w[0] - 1.0e-4), "the pop must grow monotonically: {ramp:?}");
}
