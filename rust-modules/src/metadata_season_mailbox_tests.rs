//! The season mailbox: landing races and cross-server guards on the season fetch pipeline.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The whole season mailbox in one serial test — same shape and same reason as the detail one
/// above: the statics are global, so splitting this into parallel `#[test]`s would have them
/// racing each other rather than the code.
///
/// The FIRST block is the audit finding: `fetch_episodes` returned an empty Vec for BOTH "this
/// season has no episodes" and "the `/children` GET failed", and `pump_season` installed it
/// either way — so one transient PMS failure blanked a populated episode row, with no spinner
/// and no error, onto a tab that could then never be asked again. The blocks after it cover the
/// supersede / monotone / wrong-item guards this change rewrites; on their own they would pass
/// before and after, which is why they live inside the failing test rather than beside it.
#[test]
fn a_season_landing_only_installs_while_it_is_still_the_one_being_awaited() {
    let _serial = crate::testlock::serial();

    // A FAILED /children GET. It must not be mistaken for a season with no episodes.
    install_show("show-1", 0, &["s1e1", "s1e2"]);
    let (gen, prev) = begin_switch(1);
    assert_eq!(
        selected_tab(),
        1,
        "the tab flips optimistically while the fetch is in flight"
    );
    assert!(
        season_loading(test_adapter()),
        "a bumped generation with DONE behind it reads as in flight"
    );
    land_season(test_adapter(), gen, SRV_A, "show-1".to_string(), 1, prev, None);
    assert!(!pump_season(test_state(), test_adapter()), "a failed fetch is not a new episode list");
    assert_eq!(
        listed_eps(),
        ["s1e1", "s1e2"],
        "the populated row survives the failure"
    );
    assert_eq!(
        selected_tab(),
        0,
        "the failed tab is released, so focusing it again refetches"
    );
    assert!(
        !season_loading(test_adapter()),
        "the episode row must still come out of its loading state"
    );

    // A season that GENUINELY has no episodes is a SUCCESS: the row clears. This is why the
    // discriminant is an Option and not an `is_empty()` check — a "keep the old list whenever
    // the new one is empty" fix passes the block above and leaves THIS one showing the
    // previous season's episodes under the new season's tab.
    let (gen, prev) = begin_switch(1);
    land_season(test_adapter(), gen, SRV_A, "show-1".to_string(), 1, prev, Some(Vec::new()));
    assert!(
        pump_season(test_state(), test_adapter()),
        "an empty season is a successful fetch — the row did change"
    );
    assert!(
        listed_eps().is_empty(),
        "and the previous season's episodes are gone"
    );
    assert_eq!(
        selected_tab(),
        1,
        "the tab stays on the season that answered"
    );

    // the ordinary success path
    let (gen, prev) = begin_switch(0);
    land_season(test_adapter(), 
        gen,
        SRV_A,
        "show-1".to_string(),
        0,
        prev,
        Some(vec![episode("s1e1")]),
    );
    assert!(pump_season(test_state(), test_adapter()));
    assert_eq!(listed_eps(), ["s1e1"]);
    assert_eq!(selected_tab(), 0);

    // SUPERSEDED: a blocking `load_season_now`, or a new item's `request_detail`, bumps the
    // generation — the fetch that was in flight for the old tab is dropped, not applied.
    let (old, prev) = begin_switch(1);
    supersede_season(test_adapter());
    land_season(test_adapter(), 
        old,
        SRV_A,
        "show-1".to_string(),
        1,
        prev,
        Some(vec![episode("s2e1")]),
    );
    assert!(
        !pump_season(test_state(), test_adapter()),
        "a landing from a superseded generation is discarded"
    );
    assert_eq!(
        listed_eps(),
        ["s1e1"],
        "and it must not touch the episode row"
    );

    // MONOTONE mailbox: with a newer result sitting unconsumed, an older fetch finally
    // returning must not overwrite it. Losing the newest season that way also lost its
    // SEASON_DONE catch-up, which wedged the loading spinner on.
    let (old, prev) = begin_switch(1);
    let (new, _) = begin_switch(1);
    land_season(test_adapter(), 
        new,
        SRV_A,
        "show-1".to_string(),
        1,
        prev,
        Some(vec![episode("fresh")]),
    );
    land_season(test_adapter(), 
        old,
        SRV_A,
        "show-1".to_string(),
        1,
        prev,
        Some(vec![episode("stale")]),
    );
    assert!(pump_season(test_state(), test_adapter()), "the newest season lands");
    assert_eq!(
        listed_eps(),
        ["fresh"],
        "the late older landing was refused"
    );

    // A LANDING FOR ANOTHER ITEM: the page can move (Related -> a new detail) while a season
    // fetch is in flight, and those episodes belong to nobody on screen. It must still settle
    // the spinner — nothing else is going to.
    let (gen, prev) = begin_switch(1);
    install_show("show-2", 0, &["other-e1"]);
    land_season(test_adapter(), 
        gen,
        SRV_A,
        "show-1".to_string(),
        1,
        prev,
        Some(vec![episode("s2e1")]),
    );
    assert!(
        !pump_season(test_state(), test_adapter()),
        "a landing for a different item reports no change"
    );
    assert_eq!(
        listed_eps(),
        ["other-e1"],
        "and leaves the item now on screen alone"
    );
    assert!(!season_loading(test_adapter()), "but it still settles the spinner");

    clear(test_state(), test_adapter());
}

/// The SAME landing, refused because the page moved to the OTHER SERVER's show with the same
/// ratingKey. Nothing else can see it: the hop bumps no generation that distinguishes them (a
/// `request_detail` for a different item does, but this is a page mounted from the trail or a
/// merged shelf, and the rk test — the only ownership test there was — passes.) So the share's
/// show would have been listing our show's episodes, silently.
#[test]
fn a_season_landing_for_another_servers_show_with_the_same_key_is_refused() {
    let _serial = crate::testlock::serial();

    // our server's show 42, one season switch in flight
    install_show_on(SRV_A, "42", 0, &["ours-e1"]);
    let (gen, prev) = begin_switch(1);
    // …and while it is out, the user lands on the SHARE's show 42
    install_show_on(SRV_B, "42", 0, &["theirs-e1"]);
    land_season(test_adapter(), 
        gen,
        SRV_A,
        "42".to_string(),
        1,
        prev,
        Some(vec![episode("ours-s2e1")]),
    );

    assert!(
        !pump_season(test_state(), test_adapter()),
        "our episodes are not news about the share's show"
    );
    assert_eq!(
        listed_eps(),
        ["theirs-e1"],
        "the page on screen keeps its own list"
    );
    assert!(
        !season_loading(test_adapter()),
        "…and the spinner still settles, as for any foreign landing"
    );

    // the control: the very same landing DOES install when the page is still ours
    install_show_on(SRV_A, "42", 0, &["ours-e1"]);
    let (gen, prev) = begin_switch(1);
    land_season(test_adapter(), 
        gen,
        SRV_A,
        "42".to_string(),
        1,
        prev,
        Some(vec![episode("ours-s2e1")]),
    );
    assert!(pump_season(test_state(), test_adapter()));
    assert_eq!(listed_eps(), ["ours-s2e1"]);

    clear(test_state(), test_adapter());
}
