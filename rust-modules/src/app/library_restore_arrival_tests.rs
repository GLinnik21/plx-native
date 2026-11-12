//! **A remembered Library page restored from the top strip** (`nav_tab` -> `NavOp::SelectTabWithChild`)
//! and the way back to its grid.
//!
//! The restore mounts the grid and the remembered child in one navigation commit. Two things the
//! reader sees follow from how those pages share the section's one store listing (the Collections
//! page BORROWS it, `LibraryScreen::follow_listing_type`):
//!
//! * the child arrives in its first tile, through the same seat a pill arrival gives a page
//!   (`PageRegions`, `Arrival::Content`), never leaving focus where the press left it (the strip);
//! * the grid, uncovered by BACK while the store still lists the collections, draws none of the
//!   child's tiles in its own layout before its films answer (the forward direction is
//!   `library_collections_arrival_tests`).

use super::*;
use super::test_support::{World, DipImage, ImageHeld};
use plx_plex::plex::session::LibraryPage;
use plx_screens::registry::HomeTab;
use plx_ui::containers::transition::PageDip;

fn page(w: &World) -> &plx_screens::library::LibraryScreen {
    w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref()).expect("a Library page is on top")
}

/// Home, with the Collections page remembered for the Movies library, then the Movies pill.
fn restore_from_home(name: &str) -> World { restore_page_from_home(name, LibraryPage::Collections) }

fn restore_page_from_home(name: &str, remembered: LibraryPage) -> World {
    let mut w = World::movies_library(name, false);
    w.d.nav.tabs.stack.transition = Box::new(PageDip::new());
    let key = w.bridge.directory.view().sections()[0].key;
    w.bridge.remember_library_page("route-own", key, remembered);
    super::nav_tab(&mut w.d, &mut w.bridge, HomeTab::Home, None, None);
    w.frames(60);
    assert!(w.on(&AppArg::Home), "premise: Home is on top");
    super::nav_tab(&mut w.d, &mut w.bridge, HomeTab::Movies, None, None);
    w
}

/// **The restored page takes the focus.** The Movies press leaves focus on the strip; the Collections
/// page that comes up over the grid seats its first collection as a pill arrival seats a page's first
/// poster. The reader's first Down must not have to find the grid from the top bar.
#[test]
fn a_restored_collections_page_seats_its_first_tile() {
    let mut w = restore_from_home("restore-seats-first-tile");
    let _held = ImageHeld;
    let mut dip = DipImage::default();
    for i in 0..90 {
        // the section's collections, as the page's own request answers
        if i == 12 { w.bridge.stores.browse.borrow_mut().seed_items_for_test(5); }
        w.step_dip(&mut dip, vec![]);
    }
    assert!(matches!(w.d.top_arg(), Some(AppArg::LibraryCollections { .. })), "premise: the Collections page is on top");
    assert_eq!(page(&w).probe_content().4, 5, "premise: the collections landed");
    assert_eq!(w.grid_cell(), Some((0, 0)), "focus is on the first collection, not on {:?}", w.focus());
}

/// The Recommended page restored the same way seats its first shelf card.
#[test]
fn a_restored_recommended_page_seats_its_first_card() {
    let mut w = restore_page_from_home("restore-recommended-seats", LibraryPage::Recommended);
    let _held = ImageHeld;
    let mut dip = DipImage::default();
    for _ in 0..90 { w.step_dip(&mut dip, vec![]); }
    assert!(matches!(w.d.top_arg(), Some(AppArg::LibraryRecommended { .. })), "premise: the Recommended page is on top");
    let recommended = w.d.top_screen().and_then(|s| s.as_any()).and_then(|a| a.downcast_ref::<plx_screens::library::RecommendedScreen>())
        .expect("the Recommended screen is on top");
    assert_eq!(recommended.probe_viewport(w.focus()).0, "shelf", "focus is on a shelf card, not on {:?}", w.focus());
}

/// **Back to the grid never shows the loan.** The Collections page borrowed the section's listing, so
/// the store still lists the collections when BACK uncovers the grid; until the films are asked for
/// and answer, the grid shows none of them in its own layout.
#[test]
fn the_grid_uncovered_by_back_draws_none_of_the_collections() {
    let mut w = World::collections("back-draws-no-loan", false);
    let _held = ImageHeld;
    let mut dip = DipImage::default();
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(5);
    w.frames(30);
    assert!(matches!(w.d.top_arg(), Some(AppArg::LibraryCollections { .. })), "premise: the Collections page is on top");
    assert_eq!(page(&w).probe_content().4, 5, "premise: the page lists its five collections");
    let back = super::script_key(Key::Back, super::test_support::tick(w.frame + 1));
    w.step_dip(&mut dip, back);
    let mut shown = Vec::new();
    for i in 0..40 {
        // the films answer a few frames after the repaying request
        if i == 20 { w.bridge.stores.browse.borrow_mut().seed_items_for_test(120); }
        w.step_dip(&mut dip, vec![]);
        if !w.on(&AppArg::Library) { continue; }
        let (page_alpha, grid_alpha, _, _, tiles) = page(&w).probe_content();
        if tiles == 5 && page_alpha * grid_alpha > 0.0 {
            shown.push(format!("frame {i}: {tiles} collections at alpha {:.2}", page_alpha * grid_alpha));
        }
    }
    assert!(shown.is_empty(), "the grid drew the collections it was lent: {shown:#?}");
}
