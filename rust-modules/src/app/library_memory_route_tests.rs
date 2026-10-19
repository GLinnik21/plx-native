//! **R15, what the Library remembers and what it lends** (`routes.md`), key-driven on a real
//! `Dispatcher<AppHost>` over a real store.
//!
//! The Collections page is a LOAN of the section's one store query (the Browse store holds one query
//! per section): it asks for the collections when it opens, the Library repays the loan when it is
//! the top page again, and a switch to another library finds the query back on films, so the
//! Collections page of the library switched to asks again for ITS collections.
//!
//! **Not covered here, and a known open problem** (`library_bookmark_tests`, last block, "the P4
//! open problems"): A, scrolled deep, switched to B and back to A does NOT land the grid on the tile
//! it left. The page stays at the top with focus on the library's name (by design: a library chosen
//! with OK keeps the header on screen) and the DOWN that enters the grid lands on its first row
//! instead of the remembered tile, whose cursor `LibraryScreen::seed_cursor` is meant to remember.
//! Driven with `FocusGrid` (a key-only walk cannot build a deep tile) and two `LibrarySelect`s: scroll
//! 3320 -> 0, tile (8, 4) -> (0, 0).

use super::*;
use super::test_support::{World, FILM_CLUB};
use plx_data::browse::LibraryType;

/// What the store's one query is listing now: its type, and the section key it belongs to.
fn listed(w: &World) -> (LibraryType, Option<i64>) {
    let snapshot = w.bridge.stores.browse.borrow_mut().listing_snapshot();
    let view = snapshot.view();
    (view.library_type(), view.id().map(|id| id.section))
}

#[test]
fn the_collections_page_repays_its_loan_on_back_and_asks_again_for_the_library_switched_to() {
    let mut w = World::collections("r15-collections-loan", true);
    let movies = w.bridge.directory.view().sections()[0].key;
    let film_club = w.bridge.directory.view().sections()[FILM_CLUB].key;
    assert_eq!(listed(&w), (LibraryType::Collections, Some(movies)), "premise: the page asked for Movies' collections");

    // BACK leaves the page (a first press returns its scroll to the top, a second pops it).
    for _ in 0..3 {
        if w.on(&AppArg::Library) { break; }
        w.press(Key::Back);
    }
    w.frames(60);
    assert!(w.on(&AppArg::Library), "premise: BACK returned to the grid");
    // The repaid loan is a requery of the section's films; the server's answer lands.
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.frames(60);
    assert_eq!(listed(&w), (LibraryType::Primary, Some(movies)), "the grid on top again puts the query back on films");

    // Switch to the second library with the keys: its name, then OK.
    let name = w.library().probe_libraries().iter().find(|(_, section)| *section == FILM_CLUB).expect("premise: Film Club is named").0;
    w.walk_header_to(name);
    w.press(Key::Ok);
    w.frames(60);
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.frames(60);
    assert_eq!(listed(&w), (LibraryType::Primary, Some(film_club)), "premise: the switch landed on Film Club's films");

    // Its Collections page asks for ITS collections, not the ones the first library was lent.
    w.open_from_header(8, AppArg::LibraryCollections { section: FILM_CLUB as u32 });
    w.frames(60);
    assert_eq!(listed(&w), (LibraryType::Collections, Some(film_club)), "the page asked again, for the library switched to");
}

/// OK on the library at `index` of the directory, as the header's name delivers it: the page that
/// asked hears `LibrarySelect` addressed to the section.
fn select(w: &mut World, index: usize) {
    let view = w.bridge.directory.view();
    let section = &view.sections()[index];
    let target = plx_data::stores::browse::SectionAddress {
        epoch: view.epoch().unwrap(), sid: section.sid.unwrap(), section: section.key,
    };
    let instance = w.d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    w.d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(instance),
        Delivery::Screen(ScreenEvent::App(AppMsg::LibrarySelect(target)))));
}

/// The server answers the library just switched to: its films and its (no) shelves, then the
/// page's fades play out.
fn answer(w: &mut World, index: usize) {
    w.frames(60);
    w.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    w.bridge.stores.browse.borrow_mut().seed_shelves_for_test(index, &[], 4);
    w.frames(90);
}

#[test]
fn a_library_switched_back_to_lands_down_on_the_tile_it_was_left_at_and_scrolls_to_it() {
    let mut w = World::movies_library("r15-switch-memory", true);
    w.frames(30);
    Bridge::library_command(&mut w.d, plx_screens::registry::LibraryCmd::FocusGrid { row: 8, col: 4 });
    w.frames(80);
    assert_eq!(w.grid_cell(), Some((8, 4)), "premise: focus is on a deep tile");
    let deep = w.scroll_target();
    assert!(deep > 1000.0, "premise: the page is scrolled deep ({deep})");

    // A -> B: the page shows the new library from its top, under a header focus stays on.
    select(&mut w, FILM_CLUB);
    answer(&mut w, FILM_CLUB);
    assert_eq!(listed(&w).1, Some(w.bridge.directory.view().sections()[FILM_CLUB].key), "premise: Film Club is listed");
    assert!(w.on_header(), "focus stays on the header after a switch");
    assert_eq!(w.scroll_target(), 0.0, "a switch keeps the header on screen");
    // First visit of a library: the Down that enters the grid lands on its first row.
    w.press(Key::Down);
    assert_eq!(w.grid_cell(), Some((0, 0)), "a library not visited before opens on its first row");
    w.press(Key::Up);
    assert!(w.on_header(), "premise: Up returned to the header");

    // B -> A: A comes back at the top with focus on the name; Down enters the grid on the remembered tile.
    select(&mut w, 0);
    answer(&mut w, 0);
    assert!(w.on_header(), "focus stays on the header after switching back");
    assert_eq!(w.scroll_target(), 0.0, "the header is on screen after switching back");
    w.press(Key::Down);
    assert_eq!(w.grid_cell(), Some((8, 4)), "Down lands on the tile the library was left at");
    assert!(w.scroll_target() > 1000.0, "and the page scrolls to it ({})", w.scroll_target());
}
