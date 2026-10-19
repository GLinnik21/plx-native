//! **R13 at the app tier: the Under posters setting reaches the Library's posters** (`routes.md`),
//! on a real `Dispatcher<AppHost>` over a real store.
//!
//! The Settings page's walk (`screens/src/settings_nav_structure_tests.rs`) is key-driven up to the
//! write door: OK on the Title switch goes out as a `PreferenceCmd::Choose`. The door's confirmed
//! write publishes the live cell from the storage worker, which the test lock refuses to let touch
//! a process global (`plx_base::testlock`), so this test publishes the way the door ends
//! (`restore_under_posters`, the cell's own door) and grades the other half: the grid under the
//! Settings surface words its captions from the new value.

use super::*;
use super::test_support::World;
use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};

/// Holds the live setting on its default for the test and puts it back after it.
struct Cell;
impl Cell {
    fn default() -> Self {
        plx_media::route::restore_under_posters(UnderPosters::DEFAULT);
        Cell
    }
}
impl Drop for Cell {
    fn drop(&mut self) { plx_media::route::restore_under_posters(UnderPosters::DEFAULT); }
}

/// The two caption lines (title, details) under the first poster of the library grid.
fn caption_under_first_poster(w: &World) -> [Option<String>; 2] {
    let snapshot = w.bridge.stores.browse.borrow_mut().listing_snapshot();
    w.library().probe_caption(snapshot.view(), 0)
}

#[test]
fn the_under_posters_setting_changes_the_caption_under_the_library_grids_posters() {
    let mut w = World::movies_library("r13-caption", false);
    let _cell = Cell::default();
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.frames(30);
    // (The seeded films carry no year or length, so the details line is empty whatever it is set to.)
    assert_eq!(caption_under_first_poster(&w)[0], None, "premise: the default wears no title under a poster");

    // Settings > Libraries > Under posters opens over the grid; the Title switch is turned on.
    open_settings_at(&mut w.d, plx_screens::family::SettingsPage::UnderPosters);
    w.frames(40);
    assert_eq!(w.elem(), Some(1), "premise: the page is seated on the Title switch");
    plx_media::route::restore_under_posters(UnderPosters { title: true, details: UnderPostersDetails::YearLength });
    w.frames(30);
    assert_eq!(caption_under_first_poster(&w)[0].as_deref(), Some("Item 0"), "the grid under the surface now names each poster");

    // And off again: the caption goes with the switch, in both directions.
    plx_media::route::restore_under_posters(UnderPosters::DEFAULT);
    w.frames(30);
    assert_eq!(caption_under_first_poster(&w)[0], None, "switching the title off takes the name from under the poster");
}
