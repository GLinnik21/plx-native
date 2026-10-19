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

/// **S23: a name the tile's face cuts rides the focused caption, whatever the setting.** On the
/// Collections page the face sets the collection's name (a baked fan's live name, the neutral
/// tile's), and a long one ends in an ellipsis there. With the Title switch off and the details
/// line off, the caption used to be nothing at all; now the cut name is the caption, while a name
/// the face sets whole stays out of it (the setting is still the reader's for those).
#[test]
fn a_name_the_face_cuts_is_in_the_caption_under_every_setting() {
    const FAN: &str = "/library/collections/9/composite/1700000000";
    let long = "The Complete Extended Director's Cut Anniversary Edition Saga Collection Box Set";
    let mut w = World::collections("s23-caption", false);
    let _cell = Cell::default();
    let longer = [long; 3].join(" ");
    w.bridge.stores.browse.borrow_mut().seed_named_collections_for_test(&[
        (long, FAN),
        ("Marvel", FAN),
        (long, "/library/metadata/5/thumb/1"),
        (&longer, ""),
    ]);
    w.frames(30);
    let caption = |w: &World, i: usize| {
        let snapshot = w.bridge.stores.browse.borrow_mut().listing_snapshot();
        w.library().probe_caption(snapshot.view(), i)
    };
    for (title, details) in [(false, UnderPostersDetails::YearLength), (false, UnderPostersDetails::None),
        (true, UnderPostersDetails::YearLength), (true, UnderPostersDetails::None)] {
        plx_media::route::restore_under_posters(UnderPosters { title, details });
        w.frames(10);
        let setting = format!("title {title}, details {details:?}");
        assert_eq!(caption(&w, 0)[0].as_deref(), Some(long), "the cut name is the caption's title line ({setting})");
        assert_eq!(caption(&w, 1)[0].is_some(), title, "a name the face sets whole follows the Title switch ({setting})");
        assert_eq!(caption(&w, 2)[0].is_some(), title, "a custom poster carries no name to cut ({setting})");
        assert_eq!(caption(&w, 3)[0].as_deref(), Some(longer.as_str()), "the neutral tile's cut name too ({setting})");
    }
}
