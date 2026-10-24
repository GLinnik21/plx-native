//! **A server update re-reads every page of the library family**, not only the full grid.
//!
//! `reread::action` and `Bridge::held_keys` each spelled the family as `AppArg::Library`, so the
//! Collections and Recommended pages (the same `LibraryScreen` family, `AppArg::is_library_family`)
//! fell to "nothing is owed": a title changed on the server stayed stale on them until the viewer
//! left and came back. Both now ask the one predicate.
//!
//! The real-store tests put the page in a stack of its OWN: with the full grid underneath, the
//! covered grid's read-back would hold the very keys the page borrows (the Collections page borrows
//! the section's listing), and the test would pass on the buggy code.

use super::*;
use super::test_support::World;
use super::super::reread::{self, Reread, Action, COVER_FRAMES};
use plx_screens::registry::ContentArg;

fn collections() -> AppArg { AppArg::LibraryCollections { section: 0 } }
fn recommended() -> AppArg { AppArg::LibraryRecommended { section: 0 } }

#[test]
fn every_page_of_the_library_family_owes_a_read_back() {
    for (name, arg) in [("library", AppArg::Library), ("collections", collections()), ("recommended", recommended())] {
        for covered in [true, false] {
            assert_eq!(reread::action(&arg, covered), Action::ReadBack, "{name}, covered: {covered}");
        }
    }
}

#[test]
fn the_collections_and_recommended_pages_hold_the_cards_the_library_holds() {
    let r = World::new("reread-family-held-keys");
    let library = r.bridge.held_keys(&AppArg::Library);
    assert!(!library.is_empty(), "premise: the seeded library holds cards");
    assert_eq!(r.bridge.held_keys(&collections()), library, "the Collections page holds the section's listing");
    assert_eq!(r.bridge.held_keys(&recommended()), library, "the Recommended page holds the section's shelves");
}

/// `page` alone on the stack, mounted, over a store holding 120 items and six shelves.
fn alone(name: &str, page: AppArg) -> World {
    let mut r = World::new(name);
    super::show_page(&mut r.d, page.clone());
    r.frames(60);
    // The Collections page asks the store for its own listing when it mounts; its answer is what it shows.
    r.bridge.stores.browse.borrow_mut().seed_items_for_test(120);
    r.frames(10);
    assert!(r.d.top_arg() == Some(&page), "premise: the page is on top");
    assert_eq!(r.d.nav.tabs.stack.entries.len(), 1, "premise: nothing else on the stack owes the read");
    assert!(r.bridge.read_back_pending_for_test().is_empty(), "premise: nothing is owed yet");
    r
}

/// The app comes back to the foreground with the page on top: the server moved on while it was away.
#[test]
fn a_resume_with_the_collections_or_recommended_page_on_top_reads_it_back() {
    for (name, page) in [("collections", collections()), ("recommended", recommended())] {
        let mut r = alone("reread-family-resume", page);
        let mut reread = Reread::default();
        reread.resume();
        reread::run(&mut reread, &mut r.d, &mut r.bridge);
        assert!(!r.bridge.read_back_pending_for_test().is_empty(), "{name}: the resume owes the visible page its read-back");
    }
}

/// A Detail opened over the page and left open: the covered page is owed one re-read.
#[test]
fn a_collections_or_recommended_page_covered_by_a_detail_is_read_back() {
    for (name, page) in [("collections", collections()), ("recommended", recommended())] {
        let mut r = alone("reread-family-covered", page);
        super::nav_push(&mut r.d, AppArg::Content(ContentArg::Detail { sid: plx_plex::plex::ServerId::UNSET, rk: "d".into() }));
        r.frames(2);
        assert_eq!(r.d.nav.tabs.stack.entries.len(), 2, "premise: the Detail is over the page");
        let mut reread = Reread::default();
        for _ in 0..COVER_FRAMES + 2 { reread::run(&mut reread, &mut r.d, &mut r.bridge); }
        assert!(!r.bridge.read_back_pending_for_test().is_empty(), "{name}: the covered page owes its read-back");
    }
}
