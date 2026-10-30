//! Optimistic watch-state flips, the corrected-credit re-ask on a mounted page, the playing-
//! item cache, and the season-watched rule.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The OPTIMISTIC half of a view-state write (`crate::viewstate`): the page must show the press
/// on the frame it happens, because the write that justifies it is now on a worker and the
/// item's server may be a share that takes seconds to answer — or never answers at all.
///
/// Three things have to move together, and the season count is the one that is easy to forget:
/// the tab's tick is derived from `viewedLeafCount` ([`Season::watched`]), so a tick left saying
/// the opposite of the episode row under it is the same "one item, two answers on one screen"
/// this page refuses everywhere else.
/// **A mounted detail page follows a corrected credit** — the sixth surface, and the one where
/// a stale copy showed the longest, because nothing invalidates a page that is already open.
///
/// `Detail::source` was a `String` captured at FETCH time. Two ways that went wrong and neither
/// had a repair: a roster refresh re-grades the credit under the mounted page, and a detail
/// fetch dispatched before the correction lands after it carrying the old answer. `sid` is the
/// server this item came from, so the credit is simply re-asked; this test is the "under a
/// mounted page" half, and it fails against a stored field on the first assertion.
#[test]
fn a_mounted_detail_page_follows_a_corrected_credit() {
    let _serial = plx_base::testlock::serial();
    plx_plex::plex::reset_servers_for_test();
    let house = plx_plex::plex::register_for_test("md-house", "127.0.0.1", 1, "t", "cid");

    // what a build without the rule published: the household's own server wearing the account
    // holder's handle
    plx_plex::plex::describe_server(house, "Mac mini", "admin", plx_plex::plex::GrantEvidence::outside());
    set_current_for_test(test_state(), Some(Detail {
        sid: house,
        rk: "42".into(),
        ..Default::default()
    }));
    assert_eq!(current(test_state()).unwrap().source(), "admin");

    // the roster refresh re-grades it, with nothing touching the mounted page
    plx_plex::plex::describe_server(house, "Mac mini", "", plx_plex::plex::GrantEvidence::outside());
    assert_eq!(
        current(test_state()).unwrap().source(),
        "",
        "the page re-asks the registry rather than carrying a copy taken at fetch time"
    );

    // and a share is still credited, so this is not a blanket clear
    let friend = plx_plex::plex::register_for_test("md-friend", "127.0.0.1", 2, "t", "cid");
    plx_plex::plex::describe_server(friend, "nas-home", "friend", plx_plex::plex::GrantEvidence::outside());
    set_current_for_test(test_state(), Some(Detail {
        sid: friend,
        rk: "318".into(),
        ..Default::default()
    }));
    assert_eq!(current(test_state()).unwrap().source(), "friend");

    set_current_for_test(test_state(), None);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn an_optimistic_watch_flip_reaches_the_item_its_episodes_and_the_season_tabs_count() {
    let _serial = plx_base::testlock::serial();

    // the loaded item itself — the hero's own toggle
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "42".into(),
        resume_ms: 900_000,
        ..Default::default()
    }));
    assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "42"), true));
    assert!(current(test_state()).unwrap().watched);
    assert_eq!(
        current(test_state()).unwrap().resume_ms,
        0,
        "a watched item stops offering to resume"
    );

    // …and the SHARE's 42 is a different film, so neither its press nor ours reaches the other
    assert!(
        !set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_B, "42"), false),
        "another server's key names nothing here"
    );
    assert!(
        current(test_state()).unwrap().watched,
        "and leaves this page exactly as it was"
    );

    // an EPISODE of the loaded show — the filmstrip's context menu
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "show".into(),
        is_show: true,
        cur_season: 1,
        seasons: vec![
            Season {
                rk: "sk1".into(),
                index: 1,
                title: "S1".into(),
                leaf_count: 3,
                viewed_leaf_count: 3,
            },
            Season {
                rk: "sk2".into(),
                index: 2,
                title: "S2".into(),
                leaf_count: 3,
                viewed_leaf_count: 1,
            },
        ],
        episodes: vec![
            Episode {
                rk: "e1".into(),
                watched: true,
                ..Default::default()
            },
            Episode {
                rk: "e2".into(),
                resume_ms: 60_000,
                ..Default::default()
            },
        ]
        .into(),
        ..Default::default()
    }));

    assert!(
        set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "e2"), true),
        "an episode of the loaded season"
    );
    let d = current(test_state()).unwrap();
    let e2 = d.episodes.get(1).expect("the season has two episodes");
    assert!(e2.watched);
    assert_eq!(
        e2.resume_ms, 0,
        "…and its still stops drawing a resume bar"
    );
    assert!(!d.watched, "marking one episode does not finish the show");
    assert_eq!(
        d.seasons[1].viewed_leaf_count, 2,
        "the BROWSED season's count moves with it"
    );
    assert_eq!(
        d.seasons[0].viewed_leaf_count, 3,
        "and no other season's does"
    );

    // idempotent: pressing watched on an already-watched episode must not double-count the
    // season, which would make a part-watched season read as finished
    assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "e2"), true));
    assert_eq!(
        current(test_state()).unwrap().seasons[1].viewed_leaf_count,
        2,
        "the count follows the FLIP"
    );

    // …and the reverse, clamped at zero rather than going negative
    for _ in 0..5 {
        assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "e2"), false));
        assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "e1"), false));
    }
    assert_eq!(
        current(test_state()).unwrap().seasons[1].viewed_leaf_count,
        0,
        "never a negative remainder"
    );

    assert!(
        !set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "not-here"), true),
        "an rk on neither the item nor its row"
    );
    clear(test_state(), test_adapter());
}

/// The THIRD store this page holds, and the one the two arms above cannot reach: a **Related
/// tile**, which is a different item entirely.
///
/// Since 2026-08-21 that shelf has a context menu, so the detail page can mark an item that is
/// neither the loaded one nor a leaf of it. Without this pass the press wrote correctly to the
/// server and the tile under the user's thumb kept its old tick and its old resume bar until a
/// refetch — which reads as the row having done nothing, the exact failure the optimistic edit
/// exists to prevent.
///
/// Three properties, and each is a way the walk can be written wrong:
/// * it must run BEFORE (and outside) the loaded-item / episode arms, both of which return
///   early — chained under either one, a Related hit on a page whose own rk did not match would
///   never be reached;
/// * it must match on the ROW's `sid`, not the page's. Both servers number their ratingKeys
///   from 1, so a bare-key walk would flip a tile because a *share's* item happened to share its
///   number (`docs/shared-servers.md` §2);
/// * and the verdict must survive the arms below it, or the function reports "nothing here was
///   about that item" having just edited a tile.
#[test]
fn an_optimistic_watch_flip_reaches_the_related_shelf_the_menu_was_opened_on() {
    let _serial = plx_base::testlock::serial();
    let rel = |sid, rk: &str| Related {
        sid,
        rk: rk.into(),
        dur_ns: 7_020_000 * 1_000_000,
        resume_ms: 3_510_000,
        unwatched: true,
        ..Default::default()
    };
    // a SHOW page, so the loaded item and its episodes are both populated and both must be left
    // exactly as they were by a press on a tile that is neither
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "show".into(),
        is_show: true,
        episodes: vec![Episode {
            rk: "e1".into(),
            ..Default::default()
        }]
        .into(),
        related: vec![rel(SRV_A, "r0"), rel(SRV_A, "r1")],
        ..Default::default()
    }));

    // …and the tile is reached even though the page's own rk did not match and the rk is on no
    // episode — the two arms that both return early
    assert!(
        set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "r1"), true),
        "the Related tile is a hit, not a miss"
    );
    let d = current(test_state()).unwrap();
    assert!(
        d.related[1].watched && !d.related[1].unwatched,
        "the tick the menu just promised"
    );
    assert_eq!(
        d.related[1].resume_ms, 0,
        "…and the bar it was wearing, or the tile shows both"
    );
    assert!(d.related[0].resume_frac().is_some(), "no other tile moved");
    assert!(!d.watched, "the page's own item is not what was pressed");
    assert!(!d.episodes.get(0).expect("the show has one episode").watched, "…nor is any episode of it");

    // the way back, from the second row a part-watched tile offers
    assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "r1"), false));
    let d = current(test_state()).unwrap();
    assert!(d.related[1].unwatched && !d.related[1].watched);

    // A SHARE's `r0` is a different film that happens to carry the same number. The row's own
    // `sid` is what keeps the press off it — a bare-key walk would flip the tile here.
    assert!(
        !set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_B, "r0"), true),
        "another server's key names nothing on this shelf"
    );
    assert!(
        current(test_state()).unwrap().related[0].resume_frac().is_some(),
        "…and the tile is untouched"
    );

    clear(test_state(), test_adapter());
}

/// The collection shelf's members are tiles with a context menu too, and the page's own film is
/// one of them — a flip reaches the member tile, and the loaded item moves with it.
#[test]
fn an_optimistic_watch_flip_reaches_the_collection_shelf() {
    let _serial = plx_base::testlock::serial();
    let member = |rk: &str| Related { sid: SRV_A, rk: rk.into(), unwatched: true, ..Default::default() };
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "m1".into(),
        collection: Some(CollectionShelf {
            title: "A Pair".into(),
            section: 1,
            tag: 5,
            members: vec![member("m1"), member("m2")],
            count: 2,
        }),
        ..Default::default()
    }));
    assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "m2"), true));
    let d = current(test_state()).unwrap();
    let members = &d.collection.as_ref().unwrap().members;
    assert!(members[1].watched && !members[1].unwatched, "the pressed member ticks");
    assert!(members[0].unwatched && !d.watched, "nothing else moved");
    assert!(set_watched_local(test_state(), &crate::viewstate::ViewAt::leaf(SRV_A, "m1"), true));
    let d = current(test_state()).unwrap();
    assert!(d.watched && d.collection.as_ref().unwrap().members[0].watched,
        "the page's own film and its own tile agree");
    clear(test_state(), test_adapter());
}

/// `cached_playing` is the fast path that SKIPS the PMS fetch, so a false hit is the worst of
/// the five collisions: the whole `PlayingItem` — the `Stream.id`s that get PUT to a server, the
/// frame size the direct-play gate reasons about, the fps, the chapters, the markers — would be
/// the loaded page's item rather than the one about to play, with nothing on screen to say so.
#[test]
fn the_playing_item_cache_hits_only_for_the_same_item_on_the_same_server() {
    let _serial = plx_base::testlock::serial();
    let audio = vec![Stream {
        id: 7,
        ..Default::default()
    }];
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "42".into(),
        audio: audio.clone(),
        width: 3840,
        height: 2160,
        ..Default::default()
    }));

    let hit = cached_playing(test_state(), SRV_A, "42").expect("the loaded page IS this item");
    assert_eq!(
        (hit.sid, hit.rk.as_str()),
        (SRV_A, "42"),
        "the store records where it came from"
    );
    assert_eq!(hit.audio.first().map(|s| s.id), Some(7));

    assert!(
        cached_playing(test_state(), SRV_B, "42").is_none(),
        "the SHARE's 42 is a different film"
    );
    assert!(cached_playing(test_state(), SRV_A, "43").is_none());
    assert!(
        cached_playing(test_state(), plx_plex::plex::ServerId::UNSET, "42").is_none(),
        "unscoped names neither"
    );

    // …and the pre-existing rule is untouched: a page with no streams is not a usable cache
    // entry, whatever its identity says (it would hand playback an empty track list).
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "42".into(),
        ..Default::default()
    }));
    assert!(
        cached_playing(test_state(), SRV_A, "42").is_none(),
        "no streams loaded yet — go and fetch"
    );
    clear(test_state(), test_adapter());
}

/// `PlayingItem.kind` is the played item's own Plex type, read from the same wire `type` the
/// detail page reads. The three kinds the player meets (episode, movie, track) must each survive
/// the conversion from the server's item, and an absent item has no type.
#[test]
fn the_playing_item_carries_its_plex_type() {
    for wire in ["episode", "movie", "track"] {
        let it: plx_plex::plex::Metadata =
            serde_json::from_value(serde_json::json!({ "type": wire })).unwrap();
        let item = playing_item_from(SRV_A, "42", Some(&it));
        assert_eq!(item.kind, wire, "the played item's own type");
    }
    assert_eq!(
        playing_item_from(SRV_A, "42", None).kind,
        "",
        "an item the server did not return has no type"
    );
}

fn two_versions() -> plx_plex::plex::Metadata {
    serde_json::from_value(serde_json::json!({ "type": "episode", "Media": [
        { "id": 3086, "bitrate": 2372, "width": 1920, "height": 1080, "videoResolution": "1080",
          "videoCodec": "h264", "audioCodec": "aac", "audioChannels": 2, "container": "mp4",
          "Part": [{ "key": "/p/3134", "size": 1000, "Stream": [{ "id": 1, "streamType": 2, "codec": "aac" }] }] },
        { "id": 3082, "bitrate": 14663, "width": 3840, "height": 2160, "videoResolution": "4k",
          "videoCodec": "hevc", "audioCodec": "eac3", "audioChannels": 6,
          "Part": [{ "key": "/p/3130", "container": "mkv", "size": 6000, "Stream": [
            { "id": 2, "streamType": 1, "colorTrc": "smpte2084" },
            { "id": 3, "streamType": 2, "codec": "eac3" }] }] }
    ] })).unwrap()
}

/// A picked version's track store, frame size and part all come from THAT `Media[]` entry, so the
/// route can never pair version 0's part with version 1's streams; a version the item lost falls
/// back to the default and says so by naming no played version.
#[test]
fn a_picked_version_is_the_whole_playing_item() {
    let it = two_versions();
    let (item, played) = playing_version_from(SRV_A, "42", Some(&it), Some(plx_plex::plex::MediaId(3082)));
    let played = played.expect("the version is listed");
    assert_eq!((played.index, played.part.as_str(), played.vcodec.as_str()), (1, "/p/3130", "hevc"));
    assert_eq!((item.width, item.audio[0].id, item.hdr), (3840, 3, true));
    let (item, played) = playing_version_from(SRV_A, "42", Some(&it), Some(plx_plex::plex::MediaId(9)));
    assert!(played.is_none());
    assert_eq!((item.width, item.audio[0].id), (1920, 1));
    let (item, played) = playing_version_from(SRV_A, "42", Some(&it), None);
    assert!(played.is_none(), "the default needs no override");
    assert_eq!(item.width, 1920);
}

/// The picker's facts come off the listing row, one per version in the server's order; a
/// single-version item offers none.
#[test]
fn versions_are_listed_only_when_there_is_a_choice() {
    let v = crate::pms::versions_of(&two_versions());
    assert_eq!(v.iter().map(|v| v.id.0).collect::<Vec<_>>(), [3086, 3082]);
    assert_eq!((v[1].kbps, v[1].resolution.as_str(), v[1].channels, v[1].size, v[1].hdr), (14663, "4k", 6, 6000, true));
    assert_eq!((v[0].container.as_str(), v[1].container.as_str()), ("mp4", "mkv"), "the part's container wins");
    let one: plx_plex::plex::Metadata =
        serde_json::from_value(serde_json::json!({ "Media": [{ "id": 1, "Part": [{ "key": "/p/1" }] }] })).unwrap();
    assert!(crate::pms::versions_of(&one).is_empty());
}

/// `PlayingItem.hdr` is the played leaf's video HDR flag (PQ or HLG transfer), the fact the
/// player's HDR overlay dim reads; an SDR leaf and an absent item are not HDR.
#[test]
fn the_playing_item_carries_the_video_hdr_flag() {
    let leaf = |trc: &str| -> plx_plex::plex::Metadata {
        serde_json::from_value(serde_json::json!({ "Media": [{ "Part": [{ "Stream": [
            { "streamType": 1, "colorTrc": trc }
        ] }] }] })).unwrap()
    };
    assert!(playing_item_from(SRV_A, "42", Some(&leaf("smpte2084"))).hdr, "HDR10");
    assert!(playing_item_from(SRV_A, "42", Some(&leaf("arib-std-b67"))).hdr, "HLG");
    assert!(!playing_item_from(SRV_A, "42", Some(&leaf("bt709"))).hdr, "SDR");
    assert!(!playing_item_from(SRV_A, "42", None).hdr, "no item, no HDR");
}

/// The cache hit must hand back the same `kind` the page loaded, so a play from a detail page
/// that skips the PMS fetch still knows what it is playing.
#[test]
fn the_playing_item_cache_round_trips_the_kind() {
    let _serial = plx_base::testlock::serial();
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "42".into(),
        kind: "episode".into(),
        audio: vec![Stream { id: 7, ..Default::default() }],
        ..Default::default()
    }));
    let hit = cached_playing(test_state(), SRV_A, "42").expect("the loaded page IS this item");
    assert_eq!(hit.kind, "episode", "the cache carries the page's own type");
    clear(test_state(), test_adapter());
}

/// **Regression (2026-10-06):** the loaded page's copy of an item describes the server as the page
/// read it. Once the item has PLAYED, the server's streams may have moved under it (a subtitle
/// downloaded mid-playback is installed and selected; a track picked in the HUD changes the part's
/// selection), so the next start must not plan from that copy: it picked the embedded track the page
/// remembered and PUT it, which made PMS discard the downloaded subtitle. A page that LANDS after the
/// playback is a fresh read and is a cache again.
#[test]
fn the_playing_item_cache_is_not_trusted_once_the_item_has_played() {
    let _serial = plx_base::testlock::serial();
    let page = |subs: Vec<Stream>| Detail {
        sid: SRV_A,
        rk: "42".into(),
        audio: vec![Stream { id: 7, ..Default::default() }],
        subs,
        ..Default::default()
    };
    let embedded = Stream { id: 5, selected: true, ..Default::default() };
    set_current_for_test(test_state(), Some(page(vec![embedded.clone()])));
    assert!(cached_playing(test_state(), SRV_A, "42").is_some(), "precondition: a page nobody played from is a cache");

    install_playing(test_state(), cached_playing(test_state(), SRV_A, "42"));
    assert!(
        cached_playing(test_state(), SRV_A, "42").is_none(),
        "the item played since this page was read: its stream list may be stale — fetch it"
    );

    // another item's playback does not spoil this page
    set_current_for_test(test_state(), Some(Detail { rk: "43".into(), ..page(vec![embedded.clone()]) }));
    assert!(cached_playing(test_state(), SRV_A, "43").is_some());

    // a fresh landing of the played item is a fresh read
    set_current_for_test(test_state(), Some(page(vec![embedded.clone()])));
    install_playing(test_state(), cached_playing(test_state(), SRV_A, "42"));
    assert!(install_landed_detail(test_state(), test_adapter(), Some(page(vec![embedded]))));
    assert!(cached_playing(test_state(), SRV_A, "42").is_some(), "read after the playback: trusted again");
    clear(test_state(), test_adapter());
}

/// The season-scope watched rule. Pure (no crate global, so no `testlock` here) and worth its
/// own test because two very different call sites depend on it — the season tab draws a tick
/// off it, and "Mark Season Watched" will decide which way to scrobble off it. The counts are
/// the ones a live `/library/metadata/{show}/children` returned: `idx=1 leaves=10 viewed=10`
/// and `idx=2 leaves=10 viewed=1`.
#[test]
fn a_season_is_watched_only_when_the_server_counted_episodes_and_all_of_them_are_seen() {
    let season = |leaf: i64, viewed: i64| Season {
        rk: String::new(),
        index: 0,
        title: String::new(),
        leaf_count: leaf,
        viewed_leaf_count: viewed,
    };
    assert!(season(10, 10).watched(), "every episode seen");
    assert!(!season(10, 1).watched(), "one episode in is not watched");
    assert!(!season(10, 0).watched(), "never started");
    // A season the server sent no counts for is 0 >= 0 — the `leaf_count > 0` half of the rule
    // is the only thing keeping "we don't know" from reporting as "fully watched".
    assert!(!season(0, 0).watched(), "no counts is not a watched season");
    // viewedLeafCount can lead leafCount right after a scrobble of a season being re-indexed;
    // more-watched-than-exists is still watched, never a negative remainder.
    assert!(season(10, 11).watched(), "an over-count is still watched");
}

#[test]
fn a_show_or_season_scoped_change_marks_the_loaded_shows_episodes_and_season_counts() {
    use crate::viewstate::{Scope, ViewAt};
    let _serial = plx_base::testlock::serial();
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "show".into(),
        is_show: true,
        kind: "show".into(),
        seasons: vec![
            Season { rk: "sk1".into(), index: 1, title: "S1".into(), leaf_count: 2, viewed_leaf_count: 0 },
            Season { rk: "sk2".into(), index: 2, title: "S2".into(), leaf_count: 2, viewed_leaf_count: 0 },
        ],
        episodes: vec![
            Episode { rk: "e1".into(), season: 1, resume_ms: 5, ..Default::default() },
            Episode { rk: "e2".into(), season: 1, ..Default::default() },
        ].into(),
        ..Default::default()
    }));
    let season = ViewAt {
        sid: SRV_A,
        rk: "sk1".into(),
        scope: Scope::Season { show_rk: "show".into(), index: 1 },
    };
    assert!(set_watched_local(test_state(), &season, true));
    let d = current(test_state()).unwrap();
    assert!(d.episodes.get(0).unwrap().watched && d.episodes.get(1).unwrap().watched);
    assert_eq!(d.episodes.get(0).unwrap().resume_ms, 0);
    assert_eq!((d.seasons[0].viewed_leaf_count, d.seasons[1].viewed_leaf_count), (2, 0),
        "only the named season's count moves");
    assert!(!d.watched, "a season does not finish the show");

    let show = ViewAt { sid: SRV_A, rk: "show".into(), scope: Scope::Show };
    assert!(set_watched_local(test_state(), &show, false));
    let d = current(test_state()).unwrap();
    assert!(d.episodes.get(0).is_some_and(|e| !e.watched) && d.episodes.get(1).is_some_and(|e| !e.watched));
    assert_eq!((d.seasons[0].viewed_leaf_count, d.seasons[1].viewed_leaf_count), (0, 0));
    clear(test_state(), test_adapter());
}

#[test]
fn unmarking_one_season_of_a_watched_show_clears_the_shows_own_watched_flag() {
    use crate::viewstate::{Scope, ViewAt};
    let _serial = plx_base::testlock::serial();
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "show".into(),
        is_show: true,
        kind: "show".into(),
        watched: true,
        seasons: vec![
            Season { rk: "sk1".into(), index: 1, title: "S1".into(), leaf_count: 2, viewed_leaf_count: 2 },
            Season { rk: "sk2".into(), index: 2, title: "S2".into(), leaf_count: 2, viewed_leaf_count: 2 },
        ],
        episodes: vec![
            Episode { rk: "e1".into(), season: 1, watched: true, ..Default::default() },
        ].into(),
        ..Default::default()
    }));
    let season = ViewAt {
        sid: SRV_A,
        rk: "sk1".into(),
        scope: Scope::Season { show_rk: "show".into(), index: 1 },
    };
    assert!(set_watched_local(test_state(), &season, false));
    let d = current(test_state()).unwrap();
    assert!(!d.watched, "a show with an unwatched season is not watched");
    assert_eq!((d.seasons[0].viewed_leaf_count, d.seasons[1].viewed_leaf_count), (0, 2));
    clear(test_state(), test_adapter());
}

/// The detail read was requested, the user pressed Mark Watched on a related tile while it was out,
/// and the read installs the page as the server described it before the press. The ledger makes
/// the press again, through the same edit the press used. The stamp is taken by the REAL request
/// (`MetadataCmd::RequestDetail`), not assigned.
#[test]
fn a_detail_read_in_flight_across_a_watched_edit_installs_with_it_made_again() {
    use crate::stores::metadata::MetadataCmd;
    let _serial = plx_base::testlock::serial();
    let mut state = MetadataState::default();
    let adapter = test_adapter();
    let sid = plx_plex::plex::ServerId::UNSET;
    let tile = || related_tile(sid, "rel");
    let fresh = || Detail { sid, rk: "42".into(), related: vec![tile()], ..Default::default() };
    let tile_state = |state: &MetadataState| state.current.as_ref().map(|d| (d.related[0].watched, d.related[0].resume_ms));
    let at = crate::viewstate::ViewAt::leaf(sid, "rel");

    // an older press, before the request: the server has it, so the read must not undo the server
    let older = state.ledger.record(&crate::viewstate::ViewAt::leaf(sid, "older"), crate::viewstate::ViewEdit::Watched(true));
    state.ledger.landed(older);
    assert!(run(&mut state, adapter, MetadataCmd::RequestDetail { sid, rk: "42".into() })); // the read goes out…
    let pressed = state.ledger.record(&at, crate::viewstate::ViewEdit::Watched(true)); // …then the press
    assert!(install_landed_detail(&mut state, adapter, Some(fresh())));
    assert_eq!(tile_state(&state), Some((true, 0)));

    // a read requested after the press's write LANDED: the server has it, its answer stands
    state.ledger.landed(pressed);
    assert!(run(&mut state, adapter, MetadataCmd::RequestDetail { sid, rk: "42".into() }));
    assert!(install_landed_detail(&mut state, adapter, Some(fresh())));
    assert_eq!(tile_state(&state), Some((false, 4)), "the server's answer stands");
}

/// One episode's flip steps the browsed season's count by one, only on a real flip and clamped to
/// the season's leaf count (the optimistic layer: the tab and the hero read it before any answer).
/// The Detail re-read the read-back asks for then OVERWRITES it with the server's number, which may
/// differ from a +-1 step (a leaf watched elsewhere).
#[test]
fn a_single_episode_flip_steps_the_season_count_and_the_re_read_overwrites_it_with_the_servers() {
    use crate::viewstate::ViewAt;
    let _serial = plx_base::testlock::serial();
    let show = |viewed| Detail {
        sid: SRV_A,
        rk: "show".into(),
        is_show: true,
        kind: "show".into(),
        seasons: vec![Season { rk: "sk1".into(), index: 1, title: "S1".into(), leaf_count: 3, viewed_leaf_count: viewed }],
        episodes: vec![Episode { rk: "e1".into(), season: 1, ..Default::default() }].into(),
        ..Default::default()
    };
    set_current_for_test(test_state(), Some(show(0)));
    let count = || current(test_state()).unwrap().seasons[0].viewed_leaf_count;
    let ep = ViewAt::leaf(SRV_A, "e1");
    set_watched_local(test_state(), &ep, true);
    assert_eq!(count(), 1, "the flip steps the browsed season");
    set_watched_local(test_state(), &ep, true);
    assert_eq!(count(), 1, "a press that flips nothing steps nothing");
    set_watched_local(test_state(), &ep, false);
    assert_eq!(count(), 0);
    set_watched_local(test_state(), &ep, false);
    assert_eq!(count(), 0, "clamped at zero");

    // the server says two of three are watched (one of them elsewhere), after the optimistic step
    set_watched_local(test_state(), &ep, true);
    assert_eq!(count(), 1);
    assert!(install_landed_detail(test_state(), test_adapter(), Some(show(2))));
    assert_eq!(count(), 2, "the re-read overwrites the optimistic count with the server's number");
    clear(test_state(), test_adapter());
}

fn related_tile(sid: plx_plex::plex::ServerId, rk: &str) -> crate::pms::PmsMovie {
    crate::pms::PmsMovie { sid, rk: rk.into(), unwatched: true, resume_ms: 4, ..Default::default() }
}

/// The server's word (`Facts`, `Offset`) reaches the other items' tiles on a Detail page, the
/// Related shelf and the collection shelf's members, in place and only where it differs. The loaded
/// item, its episodes and its seasons are not patched here (they re-read themselves until the Detail
/// fold takes Facts).
#[test]
fn the_servers_facts_and_offset_patch_related_and_collection_tiles_only_where_they_differ() {
    use crate::stores::metadata::MetadataCmd;
    use crate::viewstate::{ViewAt, ViewEdit};
    let _serial = plx_base::testlock::serial();
    let tile = |rk: &str| Related { sid: SRV_A, rk: rk.into(), resume_ms: 3_510_000, unwatched: true, ..Default::default() };
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "show".into(),
        resume_ms: 11,
        related: vec![tile("r0"), tile("r1")],
        collection: Some(CollectionShelf { members: vec![tile("m0")], ..Default::default() }),
        ..Default::default()
    }));
    let adapter = std::sync::Arc::new(MetadataAdapter::default());
    let run = |rk: &str, sid, edit| super::run(test_state(), &adapter, MetadataCmd::ApplyView { at: ViewAt::leaf(sid, rk), edit });
    let facts = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0, sort: None };
    assert!(run("r1", SRV_A, facts));
    assert!(!run("r1", SRV_A, facts), "facts that agree with the tile change nothing");
    assert!(!run("r0", SRV_B, facts), "another server's r0 names nothing here");
    assert!(run("m0", SRV_A, ViewEdit::Offset { ms: 42 }));
    assert!(!run("m0", SRV_A, ViewEdit::Offset { ms: 42 }));
    let d = current(test_state()).unwrap();
    assert_eq!((d.related[1].watched, d.related[1].unwatched, d.related[1].resume_ms), (true, false, 0));
    assert_eq!(d.related[0].resume_ms, 3_510_000, "no other tile moved");
    let m = &d.collection.as_ref().unwrap().members[0];
    assert_eq!((m.watched, m.unwatched, m.resume_ms), (false, true, 42), "an offset leaves the watched state alone");
    assert_eq!(d.resume_ms, 11, "the loaded item is the fold's, not this patch's");
}

/// A stop's `Offset` reaches the LOADED item and the loaded show's episode it names, matched on the
/// exact (server, key): a resume point must not wait for a read the server may never answer
/// differently (a refused stop). A repeat of the same position changes nothing, so it raises no
/// notice, and another server's same key names nothing here.
#[test]
fn an_offset_moves_the_loaded_item_and_the_episode_it_names_and_only_when_it_differs() {
    use crate::stores::metadata::MetadataCmd;
    use crate::viewstate::{ViewAt, ViewEdit};
    let _serial = plx_base::testlock::serial();
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "show".into(),
        kind: "show".into(),
        is_show: true,
        resume_ms: 11,
        episodes: vec![
            Episode { rk: "e1".into(), season: 1, resume_ms: 5, ..Default::default() },
            Episode { rk: "e2".into(), season: 1, resume_ms: 6, ..Default::default() },
        ].into(),
        ..Default::default()
    }));
    let apply = |sid, rk: &str, ms| super::run(test_state(), test_adapter(),
        MetadataCmd::ApplyView { at: ViewAt::leaf(sid, rk), edit: ViewEdit::Offset { ms } });
    assert!(apply(SRV_A, "e1", 42_000), "an episode of the loaded show");
    assert!(!apply(SRV_A, "e1", 42_000), "the same position again changes nothing");
    assert!(apply(SRV_A, "show", 7_000), "the loaded item itself");
    assert!(!apply(SRV_A, "show", 7_000));
    assert!(!apply(SRV_B, "e2", 1), "another server's e2 names nothing here");
    let d = current(test_state()).unwrap();
    assert_eq!(d.resume_ms, 7_000);
    assert_eq!((d.episodes.get(0).unwrap().resume_ms, d.episodes.get(1).unwrap().resume_ms), (42_000, 6), "no other episode moved");
    clear(test_state(), test_adapter());
}
