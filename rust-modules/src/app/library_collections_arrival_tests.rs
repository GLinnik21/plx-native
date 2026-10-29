//! The Collections page arriving from a warm library: what it can put on screen between the
//! press and its own listing landing.
//!
//! The page borrows the section's one store listing (`LibraryScreen::follow_listing_type`), so on
//! the frame it mounts the store still holds the page BENEATH it: the library's films. The page
//! asks the store for type 18 and the answer takes a round trip. Until it lands the page has no
//! content of its own, and it must say so (an empty grid with the loading read-out) instead of
//! wearing the parent's tiles under its own crumb. The route dip makes this visible for as long
//! as it runs: it captures the destination page ONCE at the floor and holds that image, so the
//! parent's tiles in that capture were on screen for the whole dip and then swapped for the
//! collections in one frame ("the grid blinks, and only then loads the collections").

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// One frame of the trace: what the top Library page would put on screen, and what the dip did.
/// `on_top`: the top page is the Collections page (the outgoing library page is top until the
/// dip's floor, and what it shows is its own).
/// `seat`: the grid cell the page reports focus on.
struct Row { on_top: bool, captured: bool, content: (f32, f32, bool, i64, usize), seat: Option<(usize, usize)> }

fn library_content(d: &Dispatcher<AppHost>) -> Option<(f32, f32, bool, i64, usize)> {
    let entry = d.nav.top_page()?;
    let page = entry.inst.as_ref()?.screen.as_any()?.downcast_ref::<plx_screens::library::LibraryScreen>()?;
    Some(page.probe_content())
}

/// The reader stands on the warm Movies library (120 films in the store) and presses the
/// Collections link; `land` is `(frame, rows)`: the server's answer, `rows` collection rows put
/// in the store `frame` frames into the trace, `None` for an answer that has not arrived by its end.
/// `command` is `(frame, cmd)`: a library command sent to the top page that frame, the way the
/// dev triggers and the scripted scenarios send one.
fn trace(frames: usize, land: Option<(usize, usize)>, command: Option<(usize, plx_screens::registry::LibraryCmd)>)
    -> (Vec<Row>, usize) {
    struct Cleanup;
    impl Drop for Cleanup { fn drop(&mut self) { plx_plex::plex::reset_servers_for_test(); } }
    let _cleanup = Cleanup;
    let session = plx_plex::plex::session::TempSession::new("library-collections-arrival");
    session.watching("u-library-collections-arrival");
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("coll-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared = plx_plex::plex::register_for_test("coll-shared", "127.0.0.1", 10, "synthetic", "fixture");
    plx_plex::plex::set_current(sid);
    let mut d = Dispatcher::<AppHost>::with_transition(Box::new(plx_ui::containers::transition::PageDip::new()));
    let mut rig = Bridge::for_test(|| 0);
    rig.stores.browse.borrow_mut().seed_registered_table_for_test([sid, shared]);
    rig.refresh_browse_directory();
    rig.browse_run(plx_data::stores::browse::BrowseCmd::SetCur(0));
    rig.stores.browse.borrow_mut().seed_items_for_test(120);
    rig.stores.browse.borrow_mut().seed_shelves_for_test(0, &[], 4);
    let _held = ImageHeld;
    let mut dip = DipImage::default();
    let mut ms = 0u32;
    let mut step = |d: &mut Dispatcher<AppHost>, rig: &mut Bridge, rows: Option<&mut Vec<Row>>| {
        ms += 16;
        plx_machine::idle::frame_begin(0.016);
        super::frame(d, rig, Tick { ms, dt_us: 16_000 }, vec![]);
        let Some(paint) = dip.plan(d, ms) else { return };
        if let (Some(rows), Some(content)) = (rows, library_content(d)) {
            rows.push(Row { on_top: d.top_arg() == Some(&AppArg::LibraryCollections { section: 0 }), captured: paint.captures_page(), content,
                seat: Bridge::library_grid_position(d) });
        }
    };
    super::show_page(&mut d, AppArg::Home);
    for _ in 0..60 { step(&mut d, &mut rig, None); }
    nav_tab(&mut d, &mut rig, HomeTab::Movies, None, None);
    for _ in 0..90 { step(&mut d, &mut rig, None); }
    assert!(d.top_arg() == Some(&AppArg::Library), "premise: the library is on top");
    let (_, _, _, _, films) = library_content(&d).expect("the library page");
    assert_eq!(films, 120, "premise: the grid lists the warm films");
    nav_push(&mut d, AppArg::LibraryCollections { section: 0 });
    let mut rows = Vec::new();
    let mut command = command;
    for i in 0..frames {
        if let Some((at, rows_landed)) = land {
            if i == at { rig.stores.browse.borrow_mut().seed_items_for_test(rows_landed); }
        }
        if command.as_ref().is_some_and(|(at, _)| *at == i) {
            Bridge::library_command(&mut d, command.take().expect("checked above").1);
        }
        step(&mut d, &mut rig, Some(&mut rows));
    }
    assert!(d.top_arg() == Some(&AppArg::LibraryCollections { section: 0 }), "premise: the push committed");
    (rows, films)
}

/// **Until its own listing lands, the page draws none of the page beneath it.** Every frame from
/// the press to the answer, the visible grid (page fade x grid fade) holds no tile, and neither
/// does the floor image the dip captures.
#[test]
fn the_collections_page_never_draws_the_parent_listings_tiles_while_its_own_is_in_flight() {
    let _guard = plx_base::testlock::serial();
    let (rows, films) = trace(40, None, None);
    let stale: Vec<_> = rows.iter().enumerate()
        .filter(|(_, r)| r.on_top && r.content.0 * r.content.1 > 0.0 && r.content.4 > 0)
        .map(|(i, r)| format!("frame {i}: {} tiles at alpha {:.2}{}", r.content.4, r.content.0 * r.content.1,
            if r.captured { " (the floor image)" } else { "" }))
        .collect();
    assert!(stale.is_empty(), "the page drew the parent's {films} films before its own listing landed: {stale:#?}");
}

/// ...and it swaps ONCE: the first frame with tiles is the collections themselves.
#[test]
fn the_collections_arrive_in_one_swap_from_the_loading_read_out() {
    let _guard = plx_base::testlock::serial();
    let (rows, films) = trace(80, Some((30, 3)), None);
    let shown: Vec<usize> = rows.iter().filter(|r| r.on_top && r.content.0 * r.content.1 > 0.0)
        .map(|r| r.content.4).filter(|n| *n > 0).collect();
    assert!(!shown.is_empty(), "the collections never became visible");
    assert!(shown.iter().all(|n| *n == 3), "only the collections were ever drawn, never {films} films: {shown:?}");
    let last = rows.last().unwrap().content;
    assert!(last.0 * last.1 >= 0.99 && !last.2 && last.4 == 3, "the page settled on its three collections: {last:?}");
}

/// **A library command reaches the Collections page.** The dev triggers (`libgrid`, `libshelf`) and
/// the scripted scenarios address "the library" through `Bridge::library_command`, which answered
/// only the full grid's route, so a seat sent to the Collections page was dropped and the trigger
/// gave up. The page is the same screen: the cell is seated.
#[test]
fn a_grid_seat_command_reaches_the_collections_page() {
    let _guard = plx_base::testlock::serial();
    let (rows, _) = trace(90, Some((20, 5)), Some((50, plx_screens::registry::LibraryCmd::FocusGrid { row: 0, col: 2 })));
    let last = rows.last().expect("frames were traced");
    assert!(last.on_top, "premise: the Collections page is on top");
    assert_eq!(last.seat, Some((0, 2)), "the page reports focus on the cell the command named");
}
