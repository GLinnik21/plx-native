//! Hero pool ordering and rotation across borrowed and owned pages.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The rule: one page moves to the front, nothing is dropped, and nothing else is reordered.
#[test]
fn a_borrowed_page_never_opens_the_door_while_we_have_one_of_our_own() {
    let mut p = pool_of(&["friend", "friend", "", "", "friend"]);
    own_items_first(&mut p);
    assert_eq!(sources_of(&p), ["", "friend", "friend", "", "friend"]);
    assert_eq!(
        p.iter().map(|s| s.idx).collect::<Vec<_>>(),
        [2, 0, 1, 3, 4],
        "the pages themselves survive"
    );
}

#[test]
fn a_pool_that_already_opens_on_one_of_ours_is_left_exactly_alone() {
    for start in [
        vec!["", "friend", "", "friend"],
        vec!["", "", ""],
        vec!["", "friend"],
    ] {
        let mut p = pool_of(&start);
        own_items_first(&mut p);
        assert_eq!(
            sources_of(&p),
            start,
            "an ordering that already holds must not be re-derived"
        );
    }
}

/// Filtering instead of ordering would leave this account with NO hero at all.
#[test]
fn a_borrowed_only_account_still_gets_a_hero_and_it_holds_the_first_rotation() {
    let mut p = pool_of(&["friend", "friend2"]);
    own_items_first(&mut p);
    assert_eq!(
        sources_of(&p),
        ["friend", "friend2"],
        "nothing of ours to promote, so nothing moves"
    );
}

#[test]
fn the_ordering_holds_at_the_empty_and_single_page_ends() {
    let mut empty: Vec<HeroSlot> = Vec::new();
    own_items_first(&mut empty);
    assert!(empty.is_empty());
    for one in [vec![""], vec!["friend"]] {
        let mut p = pool_of(&one);
        own_items_first(&mut p);
        assert_eq!(sources_of(&p), one);
    }
}

/// The hero pool, on a MERGED deck. Two things it can only get right by knowing which source
/// each ROW came from: our own item opens the rotation, and two servers' identical ratingKeys
/// are two different films. The deck's own `source` is empty by design, so a slot that read its
/// handle from its shelf would attribute every borrowed film to nobody — and `own_items_first`
/// would think one of ours had already opened the door.
#[test]
fn the_hero_pool_opens_on_our_own_item_and_never_dedups_across_servers() {
    let _g = crate::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
        src(
            0,
            "",
            HubState::Ready,
            Some(built(0, &[(100, "1")], vec![])),
        ),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(1, &[(900, "1")], vec![])),
        ),
    ]);
    assert_eq!(
        rks(&o.state, 0),
        ["1", "1"],
        "the deck orders them by recency: theirs first"
    );
    assert_eq!(hero_pool_len(&o.state), 2, "one ratingKey, two servers, two films");
    assert_eq!(hero_pool_source(&o.state, 0), "", "the door opens on our own library");
    assert_eq!(
        hero_pool_source(&o.state, 1),
        "friend",
        "…and the borrowed film rotates in behind it, attributed"
    );
    assert!(
        std::ptr::eq(hero_pool_item(&o.state, 0).unwrap(), movie(&o.state, 1).unwrap()),
        "catalog row 1 is ours (row 0 is theirs, watched later)"
    );
    reset(&mut o.state, &o.adapter);
}
