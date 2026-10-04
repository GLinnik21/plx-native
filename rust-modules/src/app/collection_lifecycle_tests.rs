//! Collection store lifecycle: generation isolation, terminal-state projection, and tag→rk
//! publication. These stay at the app boundary so they exercise the physically owned store the
//! Bridge exposes, matching the Person lifecycle coverage beside this file.

use plx_data::collection::{CollectionStatus, CollectionTarget};
use plx_plex::plex::ServerId;
use plx_data::stores::collection::CollectionCmd;

fn open(store: &mut plx_data::stores::collection::CollectionStore, rk: &str, tag: i64) {
    store.run(CollectionCmd::Open { target: CollectionTarget { id: plx_plex::plex::collections::CollectionRef {
        sid: ServerId::UNSET, rk: rk.into(), sec: 8, tag, name: "Fixture Collection".into() },
        want: plx_data::collection::PAGE_SIZE } });
}

#[test]
fn stale_generation_is_dropped_and_tag_resolution_publishes_the_rating_key() {
    let mut store = plx_data::stores::collection::CollectionStore::default();
    open(&mut store, "", 77);
    let old = store.generation_for_test();
    let adapter = store.adapter_for_test();
    open(&mut store, "", 88);
    adapter.land_resolved_for_test(old, "50077");
    assert!(!store.take_landing_for_test(), "a stale landing changes no visible model");
    assert_eq!(store.view().current().unwrap().id.tag, 88);
    assert!(store.view().current().unwrap().id.rk.is_empty());

    let current = store.generation_for_test();
    adapter.land_resolved_for_test(current, "50088");
    assert!(store.take_landing_for_test());
    assert_eq!(store.view().current().unwrap().id.rk, "50088",
        "the tag-only identity publishes its resolved collection ratingKey");
}

#[test]
fn denied_missing_empty_and_transport_map_to_distinct_screen_states() {
    for (name, land, want) in [
        ("denied", 0, CollectionStatus::Unavailable),
        ("missing", 1, CollectionStatus::Unavailable),
        ("empty", 2, CollectionStatus::Empty),
        ("transport", 3, CollectionStatus::Failed),
    ] {
        let mut store = plx_data::stores::collection::CollectionStore::default();
        open(&mut store, "50001", 77);
        let generation = store.generation_for_test();
        let adapter = store.adapter_for_test();
        match land {
            0 => adapter.land_status_for_test(generation, CollectionStatus::Unavailable),
            1 => adapter.land_missing_for_test(generation),
            2 => adapter.land_status_for_test(generation, CollectionStatus::Empty),
            _ => adapter.land_status_for_test(generation, CollectionStatus::Failed),
        }
        assert!(store.take_landing_for_test(), "{name} must change the model");
        assert_eq!(store.view().current().unwrap().status, want, "{name}");
    }
}

#[test]
fn a_profile_or_server_reset_drops_the_page_and_its_in_flight_answer() {
    let mut store = plx_data::stores::collection::CollectionStore::default();
    open(&mut store, "50001", 77);
    let old = store.generation_for_test();
    let old_adapter = store.adapter_for_test();
    assert!(store.run(CollectionCmd::Reset), "a reset with a page open is a change");
    assert!(store.view().current().is_none(), "the previous identity's collection is gone");
    open(&mut store, "50001", 77);
    assert!(store.generation_for_test() > old, "the reopened page is a new generation");
    old_adapter.land_resolved_for_test(old, "50001");
    assert!(!store.take_landing_for_test(),
        "an answer fetched under the previous profile never lands on the new page");
    assert!(store.view().current().unwrap().status == CollectionStatus::Loading);
}
