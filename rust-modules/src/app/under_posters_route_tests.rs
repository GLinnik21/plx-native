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

/// The caption lines the focused poster of the library grid DREW (`Drawn::caption_lines`: the block
/// itself, read off the frame, not asked of the label builder).
fn caption_under_focused_poster(w: &mut World) -> Vec<String> { w.drawn().caption_lines() }

#[test]
fn the_under_posters_setting_changes_the_caption_under_the_library_grids_posters() {
    let mut w = World::movies_library("r13-caption", false);
    let _cell = Cell::default();
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.frames(30);
    assert_eq!(w.grid_cell(), Some((0, 0)), "premise: focus is on the first poster");
    // (The seeded films carry no year or length, so the default's details line says what they are.)
    assert_eq!(caption_under_focused_poster(&mut w), ["Movie"], "premise: the default wears no title under a poster");

    // Settings > Libraries > Under posters opens over the grid; the Title switch is turned on, and
    // the grid, back on top, words its captions from the new value.
    open_settings_at(&mut w.d, plx_screens::family::SettingsPage::UnderPosters);
    w.frames(40);
    assert_eq!(w.elem(), Some(1), "premise: the page is seated on the Title switch");
    plx_media::route::restore_under_posters(UnderPosters { title: true, details: UnderPostersDetails::YearLength, sort_value: false });
    w.frames(30);
    w.press(Key::Back);
    w.frames(60);
    assert_eq!(w.grid_cell(), Some((0, 0)), "premise: back on the grid, on the same poster");
    assert_eq!(caption_under_focused_poster(&mut w), ["Item 0", "Movie"], "the grid under the surface now names each poster");

    // And off again: the caption goes with the switch, in both directions.
    plx_media::route::restore_under_posters(UnderPosters::DEFAULT);
    w.frames(30);
    assert_eq!(caption_under_focused_poster(&mut w), ["Movie"], "switching the title off takes the name from under the poster");
    plx_media::route::restore_under_posters(UnderPosters { title: false, details: UnderPostersDetails::None, sort_value: false });
    w.frames(30);
    assert_eq!(caption_under_focused_poster(&mut w), Vec::<String>::new(), "Nothing opens no band and draws no line");
}

/// **A name the tile's face cuts rides the focused caption, by the ONE collection rule.** On the
/// Collections page the face sets the collection's name (a baked fan's live name, the neutral
/// tile's), and a long one ends in an ellipsis there. The Title switch governs the title line like
/// any poster's; the details line is the member count under every kind but Nothing; and a name the
/// face cut, with the title line off, is carried on the details line in front of the count (the
/// face is the only other place it is told). Nothing opens no band, so nothing is told.
#[test]
fn a_name_the_face_cuts_rides_the_caption_by_the_one_collection_rule() {
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
    w.press(Key::Down);
    w.frames(30);
    // (name, member count, does the face cut it)
    let tiles: [(&str, &str, bool); 4] = [(long, "2 films", true), ("Marvel", "3 films", false),
        (long, "4 films", false), (&longer, "5 films", true)];
    for (i, (name, count, cut)) in tiles.into_iter().enumerate() {
        for (title, details) in [(false, UnderPostersDetails::YearLength), (false, UnderPostersDetails::None),
            (true, UnderPostersDetails::YearLength), (true, UnderPostersDetails::None)] {
            plx_media::route::restore_under_posters(UnderPosters { title, details, sort_value: false });
            w.frames(60);
            let want: Vec<String> = match (title, details) {
                (_, UnderPostersDetails::None) => title.then(|| name.to_owned()).into_iter().collect(),
                (true, _) => vec![name.to_owned(), count.to_owned()],
                (false, _) if cut => vec![format!("{name} \u{b7} {count}")],
                (false, _) => vec![count.to_owned()],
            };
            assert_eq!(caption_under_focused_poster(&mut w), want, "collection {i}: title {title}, details {details:?}");
        }
        w.press(Key::Right);
        w.frames(30);
    }
}
