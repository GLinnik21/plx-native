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
    let _serial = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let house = crate::plex::register_for_test("md-house", "127.0.0.1", 1, "t", "cid");

    // what a build without the rule published: the household's own server wearing the account
    // holder's handle
    crate::plex::describe_server(house, "Mac mini", "admin", false);
    set_current_for_test(test_state(), Some(Detail {
        sid: house,
        rk: "42".into(),
        ..Default::default()
    }));
    assert_eq!(current(test_state()).unwrap().source(), "admin");

    // the roster refresh re-grades it, with nothing touching the mounted page
    crate::plex::describe_server(house, "Mac mini", "", false);
    assert_eq!(
        current(test_state()).unwrap().source(),
        "",
        "the page re-asks the registry rather than carrying a copy taken at fetch time"
    );

    // and a share is still credited, so this is not a blanket clear
    let friend = crate::plex::register_for_test("md-friend", "127.0.0.1", 2, "t", "cid");
    crate::plex::describe_server(friend, "nas-home", "friend", false);
    set_current_for_test(test_state(), Some(Detail {
        sid: friend,
        rk: "318".into(),
        ..Default::default()
    }));
    assert_eq!(current(test_state()).unwrap().source(), "friend");

    set_current_for_test(test_state(), None);
    crate::plex::reset_servers_for_test();
}

#[test]
fn an_optimistic_watch_flip_reaches_the_item_its_episodes_and_the_season_tabs_count() {
    let _serial = crate::testlock::serial();

    // the loaded item itself — the hero's own toggle
    set_current_for_test(test_state(), Some(Detail {
        sid: SRV_A,
        rk: "42".into(),
        resume_ms: 900_000,
        ..Default::default()
    }));
    assert!(set_watched_local(test_state(), SRV_A, "42", true));
    assert!(current(test_state()).unwrap().watched);
    assert_eq!(
        current(test_state()).unwrap().resume_ms,
        0,
        "a watched item stops offering to resume"
    );

    // …and the SHARE's 42 is a different film, so neither its press nor ours reaches the other
    assert!(
        !set_watched_local(test_state(), SRV_B, "42", false),
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
        ],
        ..Default::default()
    }));

    assert!(
        set_watched_local(test_state(), SRV_A, "e2", true),
        "an episode of the loaded season"
    );
    let d = current(test_state()).unwrap();
    assert!(d.episodes[1].watched);
    assert_eq!(
        d.episodes[1].resume_ms, 0,
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
    assert!(set_watched_local(test_state(), SRV_A, "e2", true));
    assert_eq!(
        current(test_state()).unwrap().seasons[1].viewed_leaf_count,
        2,
        "the count follows the FLIP"
    );

    // …and the reverse, clamped at zero rather than going negative
    for _ in 0..5 {
        assert!(set_watched_local(test_state(), SRV_A, "e2", false));
        assert!(set_watched_local(test_state(), SRV_A, "e1", false));
    }
    assert_eq!(
        current(test_state()).unwrap().seasons[1].viewed_leaf_count,
        0,
        "never a negative remainder"
    );

    assert!(
        !set_watched_local(test_state(), SRV_A, "not-here", true),
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
    let _serial = crate::testlock::serial();
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
        }],
        related: vec![rel(SRV_A, "r0"), rel(SRV_A, "r1")],
        ..Default::default()
    }));

    // …and the tile is reached even though the page's own rk did not match and the rk is on no
    // episode — the two arms that both return early
    assert!(
        set_watched_local(test_state(), SRV_A, "r1", true),
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
    assert!(!d.episodes[0].watched, "…nor is any episode of it");

    // the way back, from the second row a part-watched tile offers
    assert!(set_watched_local(test_state(), SRV_A, "r1", false));
    let d = current(test_state()).unwrap();
    assert!(d.related[1].unwatched && !d.related[1].watched);

    // A SHARE's `r0` is a different film that happens to carry the same number. The row's own
    // `sid` is what keeps the press off it — a bare-key walk would flip the tile here.
    assert!(
        !set_watched_local(test_state(), SRV_B, "r0", true),
        "another server's key names nothing on this shelf"
    );
    assert!(
        current(test_state()).unwrap().related[0].resume_frac().is_some(),
        "…and the tile is untouched"
    );

    clear(test_state(), test_adapter());
}

/// `cached_playing` is the fast path that SKIPS the PMS fetch, so a false hit is the worst of
/// the five collisions: the whole `PlayingItem` — the `Stream.id`s that get PUT to a server, the
/// frame size the direct-play gate reasons about, the fps, the chapters, the markers — would be
/// the loaded page's item rather than the one about to play, with nothing on screen to say so.
#[test]
fn the_playing_item_cache_hits_only_for_the_same_item_on_the_same_server() {
    let _serial = crate::testlock::serial();
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
        cached_playing(test_state(), crate::plex::ServerId::UNSET, "42").is_none(),
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
