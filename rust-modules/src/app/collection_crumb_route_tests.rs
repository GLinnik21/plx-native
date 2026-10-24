//! **The Collection screen's caption names the page BACK actually returns to** (S21c), on a real
//! `Dispatcher<AppHost>` over a real store (`World`, the app tier's one rig).
//!
//! The screen is opened from the Collections page, a Home hub, a library page, Recommended,
//! Search and an item's Detail, so a caption that was one constant named the wrong place whenever
//! BACK went elsewhere. The caption is the title of the entry beneath it on the navigation stack,
//! read from the container once (`Host::views_over`), and is read here the way the page pass reads
//! it: through the context the dispatcher builds for that page (`Dispatcher::page_cx`). A host
//! cannot DRAW a page (no GL context, no font), so the caption is read as the screen's declaration
//! (`Screen::crumb`), which shares its one source with the draw (`CollectionLike::back_to`).

use super::*;
use super::test_support::{tick, World};
use plx_plex::plex::collections::CollectionRef;
use plx_screens::registry::ContentArg;

/// A collection to open: the identity is all the push needs.
fn collection(w: &World) -> AppArg {
    AppArg::Content(ContentArg::Collection(CollectionRef::by_rk(w.own, "50007", 1, "Synthetic set")))
}

/// Push the Collection screen over whatever page is on top, settled.
fn open_over_top(w: &mut World) {
    let arg = collection(w);
    nav_push(&mut w.d, arg);
    w.frames(60);
    assert!(matches!(w.d.top_arg(), Some(AppArg::Content(ContentArg::Collection(_)))),
        "premise: the Collection screen is on top");
}

/// What the Collection screen on top declares as the place BACK returns to, read through the
/// context the dispatcher builds for it.
fn caption(w: &World) -> Option<String> {
    let entry = w.d.nav.top_page().expect("a page is on top").id;
    let screen = &*w.d.nav.entry(entry).and_then(|e| e.inst.as_ref()).expect("it is mounted").screen;
    let cx = w.d.page_cx(tick(w.frame), entry, w.bridge.views(), &w.bridge.measure);
    screen.crumb(&cx).map(std::borrow::Cow::into_owned)
}

/// From the Collections page it names "Collections" (the guard that held while the caption was a
/// constant, and must keep holding).
#[test]
fn opened_from_the_collections_page_it_names_collections() {
    let w = super::arrival_seat_tests::collection_screen("crumb-from-collections");
    assert_eq!(caption(&w).as_deref(), Some(plx_platform::i18n::msg::browse_kind_collections()));
}

/// From a Home hub it names Home.
#[test]
fn opened_from_a_home_hub_it_names_home() {
    let mut w = World::home_with_disc("crumb-from-home", [7; 12]);
    open_over_top(&mut w);
    assert_eq!(caption(&w).as_deref(), Some(plx_platform::i18n::msg::browse_chrome_home()));
}

/// From the Recommended page it names Recommended.
#[test]
fn opened_from_recommended_it_names_recommended() {
    let mut w = World::recommended("crumb-from-recommended");
    open_over_top(&mut w);
    assert_eq!(caption(&w).as_deref(), Some(plx_platform::i18n::msg::library_header_recommended()));
}

/// From a library's grid it names the library, as that page names itself.
#[test]
fn opened_from_a_library_it_names_the_library() {
    let mut w = World::movies_library("crumb-from-library", false);
    open_over_top(&mut w);
    assert_eq!(caption(&w).as_deref(), Some("Movies"));
}

/// From Search it names Search.
#[test]
fn opened_from_search_it_names_search() {
    let mut w = World::new("crumb-from-search");
    super::show_page(&mut w.d, AppArg::Search);
    w.frames(60);
    assert!(w.on(&AppArg::Search), "premise: Search is on top");
    open_over_top(&mut w);
    assert_eq!(caption(&w).as_deref(), Some(plx_platform::i18n::msg::browse_chrome_search()));
}

/// From an item's Detail it names the item, as that page titles itself.
#[test]
fn opened_from_an_item_detail_it_names_the_item() {
    let mut w = World::detail_with_disc("crumb-from-detail", [7; 12]);
    plx_data::metadata::set_current_for_test(w.bridge.stores.metadata.state_mut(), Some(plx_data::metadata::Detail {
        sid: plx_plex::plex::ServerId::UNSET, rk: "movie".into(), kind: "movie".into(),
        title: "Synthetic film".into(), ..Default::default()
    }));
    open_over_top(&mut w);
    assert_eq!(caption(&w).as_deref(), Some("Synthetic film"));
}

/// Rooted (the `--screen collection` boot trigger), BACK leaves the page stack: no caption.
#[test]
fn rooted_it_draws_no_caption() {
    let mut w = World::new("crumb-rooted");
    let arg = collection(&w);
    nav_root(&mut w.d, arg);
    w.frames(60);
    assert!(matches!(w.d.top_arg(), Some(AppArg::Content(ContentArg::Collection(_)))), "premise: it is the root");
    assert_eq!(caption(&w), None);
}
