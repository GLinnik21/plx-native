//! The held-key enumerators of the single-current stores (search, person, collection): each names
//! the items its page holds cards for and only for the page it is asked about.

use super::collection::{CollectionCmd, CollectionStore};
use super::content_arg::ContentArg;
use super::person::{PersonCmd, PersonStore};
use super::search::SearchStore;
use crate::collection::{CollectionStatus, CollectionTarget};
use crate::pms::PmsMovie;
use crate::search::{Item, Kind, Shelf};
use plx_plex::plex::collections::CollectionRef;
use plx_plex::plex::ServerId;

fn movie(rk: &str) -> PmsMovie {
    PmsMovie { sid: ServerId::UNSET, rk: rk.into(), ..Default::default() }
}

fn rks(keys: &super::HeldKeys) -> Vec<&str> {
    keys.iter().map(|(_, k)| k.as_str()).collect()
}

#[test]
fn search_holds_its_media_cards_only() {
    let _g = plx_base::testlock::serial();
    let mut store = SearchStore::default();
    assert!(store.held_keys().is_empty(), "no result shelves hold nothing");
    store.publish_shelves_for_test(vec![
        Shelf { epoch: 0, window: Default::default(), kind: Kind::Movie,
            items: vec![Item::Media(movie("1")), Item::Media(movie("2")), Item::Media(movie("1"))] },
    ]);
    assert_eq!(rks(&store.held_keys()), ["1", "2"], "deduplicated, in shelf order");
}

#[test]
fn person_holds_its_shelves_for_that_person_only() {
    let _g = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let mut store = PersonStore::default();
    let arg = ContentArg::Person {
        sid: ServerId::UNSET, key: "42".into(), guid: String::new(), name: "N".into(), thumb: String::new() };
    assert!(store.held_keys(&arg).is_empty(), "no person mounted");
    store.run(PersonCmd::Open { sid: ServerId::UNSET, key: "42".into(), guid: String::new(), name: "N".into(), thumb: String::new() });
    store.install_for_test(vec![movie("a"), movie("b")], vec![movie("c")]);
    assert_eq!(rks(&store.held_keys(&arg)), ["a", "b", "c"]);
    let other = ContentArg::Person {
        sid: ServerId::UNSET, key: "43".into(), guid: String::new(), name: "M".into(), thumb: String::new() };
    assert!(store.held_keys(&other).is_empty(), "another person's page holds nothing of this one");
    assert!(store.held_keys(&ContentArg::Detail { sid: ServerId::UNSET, rk: "42".into() }).is_empty());
}

#[test]
fn collection_holds_its_wanted_window_for_that_collection_only() {
    let _g = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let mut store = CollectionStore::default();
    let id = CollectionRef::by_rk(ServerId::UNSET, "col", 1, "C");
    let arg = ContentArg::Collection(id.clone());
    assert!(store.held_keys(&arg).is_empty());
    store.run(CollectionCmd::Open { target: CollectionTarget { id, want: 4 } }, &plx_plex::plex::ClientKeys::default());
    store.install_for_test((0..10).map(|i| movie(&format!("m{i}"))).collect(), CollectionStatus::Ready);
    store.run(CollectionCmd::Window { wanted: 2..5, focus: None, restore: None }, &plx_plex::plex::ClientKeys::default());
    assert_eq!(rks(&store.held_keys(&arg)), ["m2", "m3", "m4"]);
    let other = ContentArg::Collection(CollectionRef::by_rk(ServerId::UNSET, "other", 1, "O"));
    assert!(store.held_keys(&other).is_empty());
}
