//! Catalog publication lifecycle: commit generations, retained publication, reset, and
//! row lookup by server+key.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn collection_rows_have_their_own_kind_and_unknown_types_are_not_listable() {
    let row = |kind: &str| plx_plex::plex::Metadata {
        kind: kind.into(), rating_key: "42".into(), title: "Not a movie".into(),
        thumb: "/poster".into(), ..Default::default()
    };
    assert_eq!(parse_item(&row("collection"), sid(0)).kind, 4, "a collection is not a movie");
    for kind in ["movie", "show", "season", "episode"] {
        assert!(listable(kind), "{kind} remains listable");
    }
    for kind in ["collection", "playlist", "clip", "something-new"] {
        assert!(!listable(kind), "{kind} must not surface as a playable catalog row");
    }
}

/// A collection row keeps its member count (the Library caption's "N items"); no other kind reads
/// `childCount` into it, so a show's season count never masquerades as one.
#[test]
fn a_collection_row_carries_its_member_count() {
    let row = |kind: &str, child_count: i64| plx_plex::plex::Metadata {
        kind: kind.into(), rating_key: "50001".into(), title: "Trilogy".into(), child_count,
        ..Default::default()
    };
    assert_eq!(parse_item(&row("collection", 3), sid(0)).child_count, 3);
    assert_eq!(parse_item(&row("collection", -1), sid(0)).child_count, 0, "a nonsense count reads as none");
    assert_eq!(parse_item(&row("show", 5), sid(0)).child_count, 0);
}

/// A collection has no watch or resume state of its own, whatever counters the server sends with
/// it: `parse_item` is the one place that says so, and every reader (the poster mark, the progress
/// bar, the item menu) trusts the row.
#[test]
fn a_collection_row_has_no_watch_or_resume_state() {
    let row = plx_plex::plex::Metadata {
        kind: "collection".into(), rating_key: "50001".into(), title: "Trilogy".into(),
        view_count: 2, view_offset: 60_000, duration: 120_000, leaf_count: 3, viewed_leaf_count: 3,
        ..Default::default()
    };
    let m = parse_item(&row, sid(0));
    assert!(!m.watched && !m.unwatched, "no watched disc and no unwatched triangle");
    assert_eq!(m.resume_ms, 0, "no resume point");
    assert_eq!(m.resume_frac(), None, "no progress bar");
}

#[test]
#[should_panic(expected = "requires its server in the retained Browse directory")]
fn a_directory_scoped_hubs_fixture_refuses_an_empty_browse_publication() {
    let _guard = plx_base::testlock::serial();
    let mut o = Owner::default();
    let directory = crate::stores::browse::DirectorySnapshot::default();
    seed_for_directory_test(
        &mut o.state, &o.adapter, ServerId::from_raw(0), 1, HubState::Ready, directory.view());
}

#[test]
fn every_catalog_commit_advances_the_published_generation() {
    let _guard = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let before = o.state.catalog_gen;
    commit(&mut o.state, (vec![row(0, "new")], Vec::new(), Vec::new()));
    assert_ne!(o.state.catalog_gen, before, "an optimistic or roster commit must invalidate cached views too");
    reset(&mut o.state, &o.adapter);
}

#[test]
fn retained_home_publication_survives_commit_and_reset_without_copying_items() {
    let _guard = plx_base::testlock::serial();
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
    assert!(std::ptr::eq(&**view.hub(0).unwrap().items.first().unwrap(), item));
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
        key: "/library/collections/7/children".into(), source: String::new(), total: 0, offset: 0, more: false, start: 0, len: 1 };
    let key = "/library/collections/7/children";
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::Key { sid: sid(0), key }));
    hub.title = "Other locale".into();
    hub.start = 1;
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::Key { sid: sid(1), key }));
    hub.hub_id = key.into();
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::Identifier { sid: sid(1), id: key, key }));
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
        let it = plx_plex::plex::Metadata {
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

    // …and with no show poster the still is STILL the episode's own frame, not the backdrop
    let without = ep("");
    assert_eq!(without.thumb, "/ep/still", "nothing to substitute");
    assert_eq!(
        without.still, "/ep/still",
        "the episode's own frame, which the landscape tile is for"
    );
    // (what the tile then DRAWS from `still` — `ui::widgets::still_key` — is graded in
    // `screens/library/labels_tests.rs`, which may name the UI)

    // a MOVIE carries no still: its `thumb` already IS its own artwork
    let film = parse_item(
        &plx_plex::plex::Metadata {
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

#[test]
fn a_flat_season_listing_keeps_its_show_title_and_own_poster() {
    let season: plx_plex::plex::Metadata = serde_json::from_str(r#"{
        "type":"season", "ratingKey":"17", "title":"Season 2", "index":2,
        "parentRatingKey":"9", "parentTitle":"Example Show", "thumb":"/season/poster"
    }"#).unwrap();
    let item = parse_item(&season, sid(0));
    assert_eq!(item.show_title, "Example Show",
        "seasons from different shows need their show name in a flat listing");
    assert_eq!(item.title, "Season 2");
    assert_eq!(item.show_rk, "9");
    assert_eq!(item.season_index, 2);
    assert_eq!(item.thumb, "/season/poster");
    assert!(item.still.is_empty());
}

/// **A ratingKey alone does not name an item once a second server exists.** Both servers
/// number from 1, so the merged catalog below holds two different films called `"1"` — and the
/// bare-key scan this replaced returned the FIRST of them to every caller, which is a play of
/// the wrong film from the item menu and the wrong backdrop on the detail page.
#[test]
fn a_catalog_row_is_found_by_its_server_and_key_never_by_the_key_alone() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let (a, b) = (sid(0), sid(1));
    let mk = |s: ServerId, rk: &str, title: &str| Arc::new(PmsMovie {
        sid: s,
        rk: rk.to_string(),
        title: title.to_string(),
        ..Default::default()
    });
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
        total: 0, offset: 0, more: false,
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
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    seed(&mut o.state, vec![src(0, "", HubState::Ready, Some(build_test(4)))]);
    reset(&mut o.state, &o.adapter);
    assert_eq!(hub_count(&o.state), 0);
    assert_eq!(catalog(&o.state).len(), 0);
    assert_eq!(hero_pool_len(&o.state), 0);
    assert_eq!(hub_state(&o.state), HubState::Loading);
    assert!(o.state.srcs.is_empty());
}
