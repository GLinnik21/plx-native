//! Catalog publication lifecycle: commit generations, retained publication, reset, and
//! row lookup by server+key.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
#[should_panic(expected = "requires its server in the retained Browse directory")]
fn a_directory_scoped_hubs_fixture_refuses_an_empty_browse_publication() {
    let _guard = crate::testlock::serial();
    let mut o = Owner::default();
    let directory = crate::stores::browse::DirectorySnapshot::default();
    seed_for_directory_test(
        &mut o.state, &o.adapter, ServerId::from_raw(0), 1, HubState::Ready, directory.view());
}

#[test]
fn every_catalog_commit_advances_the_published_generation() {
    let _guard = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let before = o.state.catalog_gen;
    commit(&mut o.state, (vec![row(0, "new")], Vec::new(), Vec::new()));
    assert_ne!(o.state.catalog_gen, before, "an optimistic or roster commit must invalidate cached views too");
    reset(&mut o.state, &o.adapter);
}

#[test]
fn retained_home_publication_survives_commit_and_reset_without_copying_items() {
    let _guard = crate::testlock::serial();
    let mut o = Owner::default();
    seed_for_test(&mut o.state, &o.adapter, 3, HubState::Ready);
    let first = hubs_snapshot(&o.state);
    let same = hubs_snapshot(&o.state);
    assert!(Arc::ptr_eq(&first.data, &same.data), "snapshot acquisition must not clone media");
    let view = first.view();
    let title = view.hub(0).unwrap().title;
    let item = view.hero(0).unwrap().item;
    assert_eq!(view.hub_count(), 1);
    assert_eq!(view.hero_count(), 3);
    assert_eq!(view.state, HubState::Ready);
    assert!(std::ptr::eq(view.hub(0).unwrap().items.first().unwrap(), item));
    assert_eq!(view.hub(0).unwrap().identity, Some(HubIdentity::ContinueWatching));
    assert_eq!(view.hub(0).unwrap().source, "");
    assert_eq!(view.hero(0).unwrap().source, "");
    reset(&mut o.state, &o.adapter);
    let empty = hubs_snapshot(&o.state);
    assert!(!Arc::ptr_eq(&first.data, &empty.data));
    assert_ne!(view.generation, empty.view().generation);
    assert_eq!(empty.view().hub_count(), 0);
    assert_eq!(empty.view().state, HubState::Loading);
    assert_eq!(title, "Continue Watching");
    assert_eq!(item.rk, "1");
    assert_eq!(view.hub(0).unwrap().items.len(), 3);
    assert!(view.hub(1).is_none());
    assert!(view.hero(3).is_none());
}

#[test]
fn home_group_falls_back_to_provider_key_not_label_or_position() {
    let items = vec![row(0, "a"), row(1, "b")];
    let mut hub = HubRow { title: "Display title".into(), hub_id: String::new(),
        key: "/library/collections/7/children".into(), source: String::new(), start: 0, len: 1 };
    let key = "/library/collections/7/children";
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::Key { sid: sid(0), key }));
    hub.title = "Other locale".into();
    hub.start = 1;
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::Key { sid: sid(1), key }));
    hub.hub_id = key.into();
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::Identifier { sid: sid(1), id: key }));
    hub.hub_id.clear();
    hub.key.clear();
    assert_eq!(stable_hub_identity(&hub, &items), None);
}

/// **An episode keeps its OWN still even when its show has no poster.** `still` used to be
/// populated only when `grandparentThumb` was present — a proxy for "this is an episode" that
/// fails in the one direction that matters. With the show poster absent, the episode's own 16:9
/// frame survived only in `thumb`, and `widgets::still_key` prefers `art` over `thumb`, so a
/// landscape tile drew the show's shared backdrop while the episode's own still sat unused:
/// exactly the "every episode is the same picture" symptom the landscape row was built to end,
/// reappearing through a different door.
#[test]
fn an_episode_keeps_its_own_still_without_a_show_poster() {
    let ep = |gp: &str| {
        let it = crate::plex::Metadata {
            kind: "episode".into(),
            rating_key: "9".into(),
            title: "The Meeting".into(),
            thumb: "/ep/still".into(),
            art: "/show/art".into(),
            grandparent_thumb: gp.into(),
            grandparent_title: "The Office".into(),
            ..Default::default()
        };
        parse_item(&it, sid(0))
    };

    // the show HAS a poster: the poster substitution stands, and the still is kept beside it
    let with = ep("/show/poster");
    assert_eq!(with.thumb, "/show/poster", "a portrait card wants the poster");
    assert_eq!(with.still, "/ep/still");
    assert_eq!(crate::ui::widgets::still_key(&with), "/ep/still");

    // …and with no show poster the still is STILL the episode's own frame, not the backdrop
    let without = ep("");
    assert_eq!(without.thumb, "/ep/still", "nothing to substitute");
    assert_eq!(
        without.still, "/ep/still",
        "the episode's own frame, which the landscape tile is for"
    );
    assert_eq!(
        crate::ui::widgets::still_key(&without),
        "/ep/still",
        "…and never the show's shared art, which is the same picture on every episode"
    );

    // a MOVIE carries no still: its `thumb` already IS its own artwork
    let film = parse_item(
        &crate::plex::Metadata {
            kind: "movie".into(),
            rating_key: "4".into(),
            title: "Snatch".into(),
            thumb: "/film/poster".into(),
            ..Default::default()
        },
        sid(0),
    );
    assert!(film.still.is_empty());
}

/// **A ratingKey alone does not name an item once a second server exists.** Both servers
/// number from 1, so the merged catalog below holds two different films called `"1"` — and the
/// bare-key scan this replaced returned the FIRST of them to every caller, which is a play of
/// the wrong film from the item menu and the wrong backdrop on the detail page.
#[test]
fn a_catalog_row_is_found_by_its_server_and_key_never_by_the_key_alone() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let (a, b) = (sid(0), sid(1));
    let mk = |s: ServerId, rk: &str, title: &str| PmsMovie {
        sid: s,
        rk: rk.to_string(),
        title: title.to_string(),
        ..Default::default()
    };
    // ours first, so a bare-key scan would always answer with it
    let cat = vec![
        mk(a, "1", "ours"),
        mk(a, "2", "ours too"),
        mk(b, "1", "the friend's"),
    ];
    let hubs = vec![HubRow {
        title: "Continue Watching".into(),
        hub_id: "home.continue".into(),
        key: String::new(),
        source: String::new(),
        start: 0,
        len: 3,
    }];
    commit(&mut o.state, (cat, hubs, Vec::new()));

    assert_eq!(index_of_rk(&o.state, a, "1"), 0);
    assert_eq!(index_of_rk(&o.state, b, "1"), 2, "the SHARE's item 1, not ours");
    assert_eq!(
        movie(&o.state, index_of_rk(&o.state, b, "1") as usize).map(|m| m.title.as_str()),
        Some("the friend's")
    );
    assert_eq!(index_of_rk(&o.state, a, "2"), 1);
    assert_eq!(
        index_of_rk(&o.state, b, "2"),
        -1,
        "a key our server has and the share does not is a MISS"
    );
    assert_eq!(
        index_of_rk(&o.state, ServerId::UNSET, "1"),
        -1,
        "and an unscoped lookup answers for neither"
    );
    reset(&mut o.state, &o.adapter);
}

/// `reset` is the profile-switch wipe: the previous user's shelves must not survive it, and
/// the state machine must come back as a fresh boot's (Loading, no source, no backoff owed).
#[test]
fn reset_wipes_the_catalog_and_re_arms_the_fetch() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(4)))]);
    reset(&mut o.state, &o.adapter);
    assert_eq!(hub_count(&o.state), 0);
    assert_eq!(catalog(&o.state).len(), 0);
    assert_eq!(hero_pool_len(&o.state), 0);
    assert_eq!(hub_state(&o.state), HubState::Loading);
    assert!(o.state.srcs.is_empty());
}
