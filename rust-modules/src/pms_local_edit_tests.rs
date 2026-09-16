//! Local optimistic edits: watched/unwatched flips and Continue Watching deck removal.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// Every appearance of the item flips, deck and shelves alike — a home screen that marked one
/// of them and not the other would be two answers about one film on one screen. And the resume
/// point goes with the flag: `poster_mark` reads progress AHEAD of watched, so a row keeping its
/// old `viewOffset` wears its old bar and no tick, which reads as the press having done nothing.
#[test]
fn marking_an_item_watched_flips_every_row_that_names_it_and_retires_its_resume_bar() {
    let _g = crate::testlock::serial();
    reset();
    let mut b = SourceBuild {
        cw: vec![CwItem {
            last_viewed_at: 9,
            m: started(0, "7"),
        }],
        shelves: vec![shelf(
            0,
            "Recently Added",
            "home.movies.recent",
            &["7", "8"],
        )],
    };

    assert!(apply_edit(&mut b, sid(0), "7", LocalEdit::Watched(true)));

    assert!(b.cw[0].m.watched && !b.cw[0].m.unwatched, "the deck card");
    assert_eq!(b.cw[0].m.resume_ms, 0, "…and its bar retires with the flag");
    assert!(
        b.shelves[0].items[0].watched,
        "the same film on another shelf"
    );
    assert!(!b.shelves[0].items[1].watched, "and nothing else on it");

    // …and the reverse toggle is the exact inverse, on a container as much as on a leaf
    assert!(apply_edit(&mut b, sid(0), "7", LocalEdit::Watched(false)));
    assert!(!b.cw[0].m.watched && b.cw[0].m.unwatched);
    reset();
}

/// An item on another server that happens to share the ratingKey is a DIFFERENT item — the rule
/// `plex::same_item` exists for, applied to the one edit that writes to a row rather than
/// reading one. A bare-key match here would tick a friend's film because you finished yours.
#[test]
fn an_edit_never_reaches_the_same_rating_key_on_another_server() {
    let _g = crate::testlock::serial();
    reset();
    let mut b = SourceBuild {
        cw: Vec::new(),
        shelves: vec![shelf(1, "Theirs", "h", &["7"])],
    };
    assert!(
        !apply_edit(&mut b, sid(0), "7", LocalEdit::Watched(true)),
        "nothing matched"
    );
    assert!(!b.shelves[0].items[0].watched);
    reset();
}

/// Remove from Continue Watching is a HIDE and nothing else: the card leaves the deck, keeps its
/// resume point, and its appearances on every OTHER shelf are untouched — which is exactly what
/// the server does (`plex::Client::remove_from_continue_watching`). Marking it watched instead
/// would throw the position away, which is the mistake that endpoint exists to avoid.
#[test]
fn a_deck_removal_leaves_the_deck_only_and_keeps_the_resume_point() {
    let _g = crate::testlock::serial();
    reset();
    let mut b = SourceBuild {
        cw: vec![
            CwItem {
                last_viewed_at: 9,
                m: started(0, "7"),
            },
            CwItem {
                last_viewed_at: 8,
                m: started(0, "8"),
            },
        ],
        shelves: vec![Shelf {
            title: "Recently Added".into(),
            hub_id: "home.movies.recent".into(),
            key: String::new(),
            items: vec![started(0, "7")],
        }],
    };

    assert!(apply_edit(&mut b, sid(0), "7", LocalEdit::LeftTheDeck));

    assert_eq!(b.cw.len(), 1, "only the one asked for leaves");
    assert_eq!(b.cw[0].m.rk, "8");
    assert_eq!(b.shelves[0].items[0].rk, "7", "its other shelf keeps it");
    assert_eq!(
        b.shelves[0].items[0].resume_ms,
        30 * 60_000,
        "…with the position it had"
    );
    assert!(
        !b.shelves[0].items[0].watched,
        "…and its watch state untouched: this is a HIDE"
    );
    reset();
}

/// The reason the edit goes through the projection and the pure `merge` rather than splicing
/// `CATALOG`: a `HubRow` is a `start`/`len` WINDOW into one flat vec and the hero pool holds
/// indices into the same, so a row removed by hand means fixing every window behind it. Here the
/// deck loses a card and the shelf behind it must still draw exactly its own items.
#[test]
fn a_removed_deck_card_leaves_the_shelves_behind_it_correctly_addressed() {
    let _g = crate::testlock::serial();
    reset();
    let build = SourceBuild {
        cw: vec![
            CwItem {
                last_viewed_at: 9,
                m: started(0, "7"),
            },
            CwItem {
                last_viewed_at: 8,
                m: started(0, "8"),
            },
        ],
        shelves: vec![shelf(
            0,
            "Recently Added",
            "home.movies.recent",
            &["a", "b"],
        )],
    };
    seed(vec![src(0, "", HubState::Ready, Some(build))]);
    assert_eq!(rks(0), vec!["7", "8"], "the deck as it stands");
    assert_eq!(rks(1), vec!["a", "b"]);

    assert!(edit_item(sid(0), "7", LocalEdit::LeftTheDeck));

    assert_eq!(rks(0), vec!["8"], "the card is gone from the deck");
    assert_eq!(
        rks(1),
        vec!["a", "b"],
        "and the shelf behind it still names its own items"
    );
    assert_eq!(hub_count(), 2, "no shelf appeared or vanished");
    reset();
}

/// An item on no shelf at all — a Library-grid or Related page press — must not re-commit Home
/// for nothing: the return value is what tells `viewstate` whether anything on screen moved, and
/// a commit here would free the catalog strings out from under a live `hub_title` borrow for no
/// reason at all.
#[test]
fn an_item_on_no_shelf_reports_no_edit_and_recommits_nothing() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![src(0, "", HubState::Ready, Some(build_test(2)))]);
    assert!(!edit_item(sid(0), "not-on-home", LocalEdit::Watched(true)));
    assert_eq!(hub_len(0), 2, "Home is exactly as it was");
    reset();
}

#[test]
fn scoped_optimistic_edit_cannot_restore_an_unpinned_sibling_library() {
    let _guard = crate::testlock::serial();
    reset();
    let sid = sid(0);
    let directory = two_library_directory(sid, true);
    seed_two_library_home_for_test(sid, directory.view());
    assert_eq!(rks(0), ["alpha"]);

    let outcome = run_with_directory(
        crate::stores::hubs::HubsCmd::EditItem {
            sid,
            rk: "alpha".into(),
            edit: LocalEdit::Watched(true),
        },
        directory.view(),
    );

    assert!(outcome.changed);
    assert_eq!(rks(0), ["alpha"],
        "EditItem must re-merge through the retained one-pinned directory");
    assert!(hub_item(0, 0).unwrap().watched);
    reset();
}
