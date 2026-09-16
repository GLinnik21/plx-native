//! Multi-server Home merge: shelf/source projection, roster and library-pin scoping,
//! per-source failure isolation, and shared budgets.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use super::test_support::land;

#[test]
fn home_group_identity_uses_provider_and_server_not_title_or_position() {
    let id = "home.movies.recent";
    let mut hub = HubRow { title: "Recent movies".into(), hub_id: id.into(),
        key: String::new(), source: "Alice".into(), start: 0, len: 1 };
    let items = vec![row(0, "a"), row(1, "b"), row(0, "c")];
    let want = |slot| Some(HubIdentity::Identifier { sid: sid(slot), id });
    assert_eq!(stable_hub_identity(&hub, &items), want(0));
    hub.title = "Localized title".into();
    hub.source = "Renamed owner".into();
    hub.start = 2;
    assert_eq!(stable_hub_identity(&hub, &items), want(0));
    hub.start = 1;
    assert_eq!(stable_hub_identity(&hub, &items), want(1));
    hub.hub_id.clear();
    assert_eq!(stable_hub_identity(&hub, &items), None);
}

#[test]
fn merged_home_deck_identity_survives_a_different_leading_server() {
    let mut hub = HubRow { title: "Continue Watching".into(), hub_id: "home.continue".into(),
        key: String::new(), source: String::new(), start: 0, len: 1 };
    let items = vec![row(0, "a"), row(1, "b")];
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::ContinueWatching));
    hub.start = 1;
    assert_eq!(stable_hub_identity(&hub, &items), Some(HubIdentity::ContinueWatching));
}

/// The STAMPING contract, and the linchpin under every `(sid, rk)` test in the crate: if
/// `project` did not stamp the server it was asked of onto every row, two servers' item `1`
/// would be one item to the whole app.
#[test]
fn every_row_a_source_projects_is_stamped_with_the_server_it_was_asked_of() {
    let body = |rk: &str, title: &str| {
        format!(
            r#"{{"MediaContainer":{{"Hub":[{{"type":"movie","hubIdentifier":"home.movies.recent",
               "title":"Recently Added","Metadata":[{{"ratingKey":"{rk}","type":"movie","title":"{title}",
               "thumb":"/t.jpg","art":"/a.jpg"}}]}}]}}}}"#
        )
    };
    let parse = |s: String| {
        serde_json::from_str::<crate::plex::Envelope>(&s)
            .expect("a PMS body parses")
            .media_container
    };
    let empty = crate::plex::MediaContainer::default();
    let ours = sid(3);

    let b = project(&parse(body("1", "Ours")), &empty, ours);
    assert_eq!(
        b.shelves.len(),
        1,
        "the shelf survived the title/poster filter"
    );
    assert_eq!(b.shelves[0].items.len(), 1);
    assert_eq!(
        (b.shelves[0].items[0].sid, b.shelves[0].items[0].rk.as_str()),
        (ours, "1"),
        "the row names the server it came from"
    );

    // the same wire body parsed for ANOTHER server must not produce rows that compare equal to
    // the first server's — this is the whole reason the field exists, and with a MERGED Home
    // the two now sit in one catalog rather than in two runs of the app
    let theirs = sid(4);
    let b2 = project(&parse(body("1", "Theirs")), &empty, theirs);
    let (m1, m2) = (&b.shelves[0].items[0], &b2.shelves[0].items[0]);
    assert!(
        !crate::plex::same_item((m1.sid, &m1.rk), (m2.sid, &m2.rk)),
        "one ratingKey from two servers must never alias"
    );

    // …and merged, both survive into the catalog and into the hero pool
    let (cat, hubs, pool) = merge(&[
        src(3, "", HubState::Ready, Some(b)),
        src(4, "friend", HubState::Ready, Some(b2)),
    ]);
    assert_eq!(cat.len(), 2);
    assert_eq!(
        hubs.len(),
        2,
        "one shelf each, neither folded into the other"
    );
    assert_eq!(pool.len(), 2, "…and so does the hero pool, by construction");
}

/// The failure this unit exists for. Home used to be one `?` chain over one server, so a single
/// dead share aborted the whole build: nothing committed, and on a cold boot the user got a
/// whole-screen "Can't reach your Plex server" about their own working library. A source's
/// verdict is now its own — the one that answered commits, the one that failed contributes
/// nothing and backs off alone.
#[test]
fn one_failing_source_still_commits_the_other() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(0, "", HubState::Loading, None),
        src(1, "friend", HubState::Loading, None),
    ]);

    land(
        0,
        Some(built(
            0,
            &[],
            vec![shelf(
                0,
                "Recently Added",
                "home.movies.recent",
                &["a1", "a2"],
            )],
        )),
    );
    land(1, None);
    pump(0.0);

    assert_eq!(
        hub_state(),
        HubState::Ready,
        "one server answering is an answered Home"
    );
    assert_eq!(
        hub_count(),
        1,
        "the dead share contributes NOTHING — no heading, no empty shelf"
    );
    assert_eq!(hub_len(0), 2);
    assert_eq!(
        hub_source(0),
        "",
        "and the shelf that did arrive is our own, so it is unannotated"
    );
    let s = lock_srcs();
    assert_eq!(s[0].state, HubState::Ready);
    assert_eq!(s[1].state, HubState::Failed);
    assert!(s[1].retry_s > 0.0, "the share retries on its own ladder…");
    assert_eq!(s[0].retry_s, 0.0, "…and the working server owes nothing");
    drop(s);
    reset();
}

/// …and the same rule once BOTH sources are populated, which is the state a user is actually
/// sitting in front of when a friend's server goes to sleep. The failing source keeps the
/// shelves it last answered with, the working source is not touched at all, and neither the
/// catalog nor the deck loses a row.
#[test]
fn a_failing_source_leaves_a_populated_home_completely_intact() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(
            0,
            "",
            HubState::Ready,
            Some(built(
                0,
                &[(9, "own-cw")],
                vec![shelf(0, "Films", "a.recent", &["a1", "a2"])],
            )),
        ),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(
                1,
                &[(5, "their-cw")],
                vec![shelf(1, "Film Club", "b.recent", &["b1"])],
            )),
        ),
    ]);
    let before: Vec<(&str, &str, usize)> = (0..hub_count())
        .map(|i| (hub_title(i), hub_source(i), hub_len(i)))
        .collect();
    assert_eq!(before.len(), 3, "deck + one shelf each");
    let rows = catalog().len();

    land(1, None); // the share stops answering
    pump(0.0);

    let after: Vec<(&str, &str, usize)> = (0..hub_count())
        .map(|i| (hub_title(i), hub_source(i), hub_len(i)))
        .collect();
    assert_eq!(after, before, "not one shelf, heading or row moved");
    assert_eq!(catalog().len(), rows);
    assert_eq!(
        rks(0),
        ["own-cw", "their-cw"],
        "and the merged deck kept both servers' items"
    );
    assert_eq!(
        hub_state(),
        HubState::Ready,
        "Home is not failed while a source is answering"
    );
    assert_eq!(
        lock_srcs()[0].retry_s,
        0.0,
        "the working source owes no backoff"
    );
    reset();
}

/// A PARTIAL landing repaints. A settled Home stops presenting entirely (`ui::idle`), so a
/// source arriving seconds after the owned server did — which is the normal shape of this
/// feature, not an edge case — would otherwise draw its shelves invisibly until the next
/// keypress. The failure half matters just as much: a retry that fails changes the status
/// caption under an empty Home.
#[test]
fn a_source_landing_repaints_a_settled_home() {
    let _g = crate::testlock::serial();
    reset();
    crate::ui::idle::set_enabled(true);
    seed(vec![
        src(0, "", HubState::Ready, Some(build_test(1))),
        src(1, "friend", HubState::Loading, None),
    ]);

    for (what, build) in [
        ("a share arriving", Some(build_test(2))),
        ("a share failing", None),
    ] {
        crate::ui::idle::should_present(0); // takes-and-clears whatever was already pending
        assert!(
            !crate::ui::idle::should_present(0),
            "the panel is settled with nothing happening"
        );
        land(1, build);
        pump(0.0);
        assert!(
            crate::ui::idle::should_present(0),
            "{what} must invalidate the frame"
        );
    }
    reset();
}

/// The total-failure read-out is reserved for a total failure. Any source answering makes Home
/// answered; a mix of failed and still-loading is still loading.
#[test]
fn only_every_source_failing_reads_as_a_failed_home() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(0, "", HubState::Failed, None),
        src(1, "friend", HubState::Loading, None),
    ]);
    assert_eq!(
        hub_state(),
        HubState::Loading,
        "one source still trying is not a dead Home"
    );

    seed(vec![
        src(0, "", HubState::Failed, None),
        src(1, "friend", HubState::Ready, None),
    ]);
    assert_eq!(
        hub_state(),
        HubState::Ready,
        "a share being down says nothing about our own"
    );

    seed(vec![
        src(0, "", HubState::Failed, None),
        src(1, "friend", HubState::Failed, None),
    ]);
    assert_eq!(
        hub_state(),
        HubState::Failed,
        "everything down IS the whole-screen case"
    );
    reset();
}

/// Continue Watching is ONE shelf across every source, ordered by when the owner last watched —
/// so a borrowed item legitimately holds first position, and the heading therefore cannot claim
/// an owner. It carries no annotation at all. This is the official client's own shape: the
/// owner's screenshots show a friend's films sitting BETWEEN their own, in one row.
#[test]
fn continue_watching_merges_across_sources_by_last_viewed() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(
            0,
            "",
            HubState::Ready,
            Some(built(0, &[(300, "own-old"), (100, "own-oldest")], vec![])),
        ),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(1, &[(900, "their-new"), (200, "their-mid")], vec![])),
        ),
    ]);

    assert_eq!(hub_count(), 1, "one deck, not one per server");
    assert!(hub_is_continue(0));
    assert_eq!(
        hub_source(0),
        "",
        "a shelf drawn from two servers cannot be named by one of them"
    );
    // The timestamps INTERLEAVE on purpose: a per-source concatenation, which is what the
    // obvious implementation of "merge" does, would give own-old, own-oldest, their-new,
    // their-mid and pass any test written with one server's deck in front of the other's.
    assert_eq!(rks(0), ["their-new", "own-old", "their-mid", "own-oldest"]);
    reset();
}

/// Every OTHER shelf keeps its source: the owner's handle for a borrowed server, empty for our
/// own. And the groups stay contiguous in roster order — adjacency is the grouping device, so a
/// source's shelves may never be interleaved with another's.
#[test]
fn every_other_shelf_carries_its_source_and_the_groups_stay_contiguous() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(
            0,
            "",
            HubState::Ready,
            Some(built(
                0,
                &[(1, "cw")],
                vec![shelf(0, "Films", "a.recent", &["a"])],
            )),
        ),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(
                1,
                &[],
                vec![
                    shelf(1, "Film Club", "b.recent", &["b"]),
                    shelf(1, "Club TV", "b.tv", &["b2"]),
                ],
            )),
        ),
        src(
            2,
            "friend2",
            HubState::Ready,
            Some(built(2, &[], vec![shelf(2, "Docs", "c.recent", &["c"])])),
        ),
    ]);

    let by_row: Vec<(&str, &str)> = (0..hub_count())
        .map(|i| (hub_title(i), hub_source(i)))
        .collect();
    assert_eq!(
        by_row,
        [
            ("Continue Watching", ""),
            ("Films", ""),
            ("Film Club", "friend"),
            ("Club TV", "friend"),
            ("Docs", "friend2")
        ],
        "deck first, then our own, then each share whole"
    );
    reset();
}

/// A source that has never answered contributes nothing — no heading, no empty shelf, no
/// spinner row (which would also hold the panel presenting for a source that isn't coming).
/// One that HAS answered and has since failed keeps what it last had: a transient failure must
/// not reflow the shelves under the focus ring.
#[test]
fn a_source_that_never_answered_draws_nothing_at_all() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(
            0,
            "",
            HubState::Ready,
            Some(built(0, &[], vec![shelf(0, "Films", "a.recent", &["a"])])),
        ),
        src(1, "friend", HubState::Failed, None),
        src(
            2,
            "friend2",
            HubState::Failed,
            Some(built(2, &[], vec![shelf(2, "Docs", "c.recent", &["c"])])),
        ),
    ]);
    let by_row: Vec<(&str, &str)> = (0..hub_count())
        .map(|i| (hub_title(i), hub_source(i)))
        .collect();
    assert_eq!(by_row, [("Films", ""), ("Docs", "friend2")]);
    reset();
}

/// A source that leaves the roster takes its shelves with it at the next read — the one thing
/// that DOES remove a live source's rows, because "gone" (un-pinned, or a revoked share) is a
/// fact about the grant rather than about a fetch that happened to fail.
#[test]
fn a_source_that_leaves_the_roster_stops_contributing() {
    let _g = crate::testlock::serial();
    reset();
    seed(vec![
        src(
            0,
            "",
            HubState::Ready,
            Some(built(0, &[], vec![shelf(0, "Films", "a.recent", &["a"])])),
        ),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(
                1,
                &[],
                vec![shelf(1, "Film Club", "b.recent", &["b"])],
            )),
        ),
    ]);
    assert_eq!(hub_count(), 2);

    // the registry a host test has is empty, so the roster this recomputes to is empty too
    forget_roster();
    sync_roster();

    assert_eq!(hub_count(), 0, "both un-rostered sources' shelves are gone");
    assert!(lock_srcs().is_empty());
    reset();
}

#[test]
fn an_equal_size_roster_replacement_has_a_different_cache_key_and_source_table() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    reset();
    let a = crate::plex::register_for_test("pms-a", "127.0.0.1", 1, "a", "cid");
    let b = crate::plex::register_for_test("pms-b", "127.0.0.1", 2, "b", "cid");
    sync_roster();
    let before = roster_key();
    assert_eq!(
        lock_srcs().iter().map(|s| s.sid).collect::<Vec<_>>(),
        [a, b]
    );

    crate::plex::revoke_for_profile_switch();
    let c = crate::plex::register_for_test("pms-c", "127.0.0.1", 3, "c", "cid");
    assert_eq!(
        crate::plex::server_count(),
        2,
        "the replacement deliberately preserves count"
    );
    assert_ne!(
        roster_key(),
        before,
        "the exact registry generation, not count, keys Home"
    );
    sync_roster();
    assert_eq!(
        lock_srcs().iter().map(|s| s.sid).collect::<Vec<_>>(),
        [a, c]
    );

    reset();
    crate::plex::reset_servers_for_test();
}

/// **The pin's grain is a LIBRARY, and `/hubs` is a whole-SERVER request.** So the server-level
/// gate cannot be the only one: unpinning one library of a two-library server left every one of
/// its items on Home, which is what the owner hit ("I disabled local server lib from home but
/// it persisted"). Each row carries its own `librarySectionID`, and that is the join.
///
/// Unknown passes in both directions — a row whose server sent no id, and a library the section
/// table has not enumerated — because hiding what cannot be classified empties Home on the
/// frame it boots.
#[test]
fn an_unpinned_library_keeps_its_items_off_home_even_when_its_server_feeds_it() {
    let (a, b) = (sid(0), sid(1));
    // server A has two libraries: 1 pinned, 2 NOT. Server B has one, pinned.
    let pins = vec![(a, 1, true), (a, 2, false), (b, 1, true)];
    let row = |s: ServerId, sec: i64| PmsMovie {
        sid: s,
        sec,
        ..Default::default()
    };

    assert!(
        item_pinned(&pins, &row(a, 1)),
        "a pinned library of a server that feeds Home"
    );
    assert!(
        !item_pinned(&pins, &row(a, 2)),
        "…and its UNPINNED sibling, on the same server"
    );
    assert!(item_pinned(&pins, &row(b, 1)));

    // the same section KEY on another server is a different library — both servers number from 1
    let pins2 = vec![(a, 1, false), (b, 1, true)];
    assert!(!item_pinned(&pins2, &row(a, 1)));
    assert!(
        item_pinned(&pins2, &row(b, 1)),
        "keys collide across servers; the pair does not"
    );

    // unknown passes, both ways
    assert!(
        item_pinned(&pins, &row(a, 0)),
        "the server sent no librarySectionID"
    );
    assert!(
        item_pinned(&pins, &row(a, 9)),
        "a library the section table has not enumerated"
    );
    assert!(item_pinned(&[], &row(a, 1)), "nothing discovered yet");
}

#[test]
fn equal_generation_browse_owners_rebuild_the_pms_home_projection() {
    let _guard = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    reset();
    let sid = crate::plex::register_for_test(
        "equal-generation-home", "127.0.0.1", 9, "synthetic", "fixture");
    let alpha = two_library_directory(sid, true);
    let beta = two_library_directory(sid, false);
    let alpha_scope = BrowseScope::retained(alpha.view());
    let beta_scope = BrowseScope::retained(beta.view());
    assert_eq!(alpha_scope.sections_gen, beta_scope.sections_gen,
        "the regression requires equal owner-local generations");
    seed_two_library_home_for_test(sid, alpha.view());
    assert_eq!(rks(0), ["alpha"]);

    sync_roster_with_scope(&beta_scope);

    assert_eq!(rks(0), ["beta"],
        "the PMS cache must not alias an independent equal-generation owner");
    reset();
    crate::plex::reset_servers_for_test();
}

/// **The pin store is the seam, and "no pinned library" only means something for a server whose
/// libraries have been ENUMERATED.** `/library/sections` and `/hubs` land independently on
/// workers, so Home can briefly know a rostered source before its libraries.
///
/// Reading that state as "not pinned" is what produced the owner's report that a share
/// "appeared on the home screen only after I watched the library": the pin said On the whole
/// time, and Home was asking a question the section table could not yet answer.
#[test]
fn a_server_whose_libraries_are_unknown_is_undecided_not_unpinned() {
    let (a, b) = (sid(0), sid(1));
    // nothing discovered anywhere: every granted server feeds Home
    assert!(feeds_home(a, &[], &[]));
    assert!(feeds_home(b, &[], &[]));

    // **the regression**: `a` is discovered and pinned, `b` is in the roster and has not been
    // enumerated. `b` is UNDECIDED and must still feed Home.
    assert!(
        feeds_home(b, &[a], &[a]),
        "a share nobody has enumerated is not a share turned off"
    );

    // …and once `b`'s libraries ARE known, the pin is a real answer in both directions
    assert!(
        !feeds_home(b, &[a], &[a, b]),
        "enumerated, granted, browsable — and not pinned"
    );
    assert!(feeds_home(b, &[a, b], &[a, b]), "pinned");
    assert!(feeds_home(a, &[a], &[a, b]));
}

/// The budget is split, not raced for. Whoever is asked first used to spend it — and `/hubs`
/// promotes several rows per library, so one four-library server already overruns
/// [`MAX_SHELVES`] and the share behind it drew nothing.
#[test]
fn the_budget_is_shared_so_neither_source_starves_the_other() {
    assert_eq!(
        allot(10, &[4, 4]),
        [4, 4],
        "a budget nobody exhausts is not rationed"
    );
    assert_eq!(
        allot(10, &[99, 99]),
        [5, 5],
        "two greedy sources split it evenly"
    );
    assert_eq!(
        allot(10, &[2, 99]),
        [2, 8],
        "what one does not want is passed on, not wasted"
    );
    assert_eq!(
        allot(10, &[99, 0, 99]),
        [5, 0, 5],
        "…and RE-DIVIDED, not given to whoever is first"
    );
    assert_eq!(
        allot(1, &[9, 9, 9]),
        [1, 0, 0],
        "a budget below one each reaches as many as it can"
    );
    assert_eq!(allot(10, &[]), Vec::<usize>::new());

    let _g = crate::testlock::serial();
    reset();
    let many = |slot: u16, tag: &str| {
        (0..30)
            .map(|i| shelf(slot, &format!("{tag}{i}"), "x", &["r"]))
            .collect::<Vec<_>>()
    };
    seed(vec![
        src(0, "", HubState::Ready, Some(built(0, &[], many(0, "a")))),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(1, &[], many(1, "b"))),
        ),
    ]);
    assert_eq!(hub_count(), MAX_SHELVES, "the total cap still holds");
    let theirs = (0..hub_count())
        .filter(|&i| hub_source(i) == "friend")
        .count();
    assert_eq!(
        theirs,
        MAX_SHELVES / 2,
        "and the share gets its half rather than the leftovers"
    );
    reset();
}

/// **A corrected credit re-stamps the shelves Home has ALREADY built**, with no fetch landing.
///
/// This is the last hop of the "Shared by …" fix (`plex::servers::owner_credit`,
/// `docs/shared-servers.md` §13) and it needed two things that were both missing. The rows and
/// the hero pool carry `Src::handle` as a COPY taken at merge time, and `sync_roster` re-merged
/// only when a source had been dropped — so a re-graded credit sat in `Src::handle` and changed
/// nothing on screen until the next successful hub fetch, which for an offline source never
/// comes: "keep the last good shelves" would have preserved the wrong attribution for good.
/// And `roster_key` — the fingerprint this whole rebuild is skipped on — could not see a
/// `describe` at all, so `sync_roster` early-returned before reaching any of it.
#[test]
fn a_corrected_credit_restamps_the_shelves_home_already_built() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    reset();
    let s = crate::plex::register_for_test("pms-credit", "127.0.0.1", 1, "t", "cid");
    assert_eq!(s, sid(0), "a fresh registry hands out slot 0");

    // what a build without the rule published: the household's own server, wearing the account
    // holder's handle, with shelves already merged from it
    crate::plex::describe_server(s, "Mac mini", "admin", false);
    seed(vec![src(
        0,
        "admin",
        HubState::Ready,
        Some(built(0, &[], vec![shelf(0, "Recently Added", "x", &["r1"])])),
    )]);
    assert_eq!(hub_source(0), "admin");

    // the roster refresh re-grades it — and there is deliberately NO landing after this
    crate::plex::describe_server(s, "Mac mini", "", false);
    sync_roster();

    assert_eq!(
        hub_source(0),
        "",
        "the shelf follows the registry off a credit without waiting for a fetch"
    );
    assert_eq!(hero_pool_source(0), "");
    assert_eq!(
        lock_srcs()[0].handle,
        "",
        "and the source itself is re-read, not only the rows"
    );

    reset();
    crate::plex::reset_servers_for_test();
}

/// The same split over catalog ROWS, which is the cap the shelves' items come out of.
#[test]
fn the_row_budget_is_shared_too() {
    let _g = crate::testlock::serial();
    reset();
    // enough shelves, each already at the per-shelf ceiling, that the ROW cap is what binds
    let fat = |slot: u16, tag: &str| {
        (0..20)
            .map(|s| {
                let keys: Vec<String> = (0..MAX_SHELF_ITEMS)
                    .map(|i| format!("{tag}-{s}-{i}"))
                    .collect();
                shelf(
                    slot,
                    &format!("{tag}{s}"),
                    "x.recent",
                    &keys.iter().map(|r| r.as_str()).collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    };
    seed(vec![
        src(0, "", HubState::Ready, Some(built(0, &[], fat(0, "a")))),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(1, &[], fat(1, "b"))),
        ),
    ]);
    assert_eq!(catalog().len(), PMS_MAX_MOVIES, "the total cap still holds");
    let theirs: usize = (0..hub_count())
        .filter(|&i| hub_source(i) == "friend")
        .map(hub_len)
        .sum();
    assert_eq!(
        theirs,
        PMS_MAX_MOVIES / 2,
        "and the share gets half the rows, not the leftovers"
    );
    reset();
}

/// The merged deck is capped at what the grid can ADDRESS. Three sources' Continue Watching is
/// up to 36 cards, and past `MAX_SHELF_ITEMS` the home grid's focus ring and its OK dispatch clamp
/// differently — the ring stops at the last addressable card while the press opens whatever
/// column the raw index names. Unreachable with one server, which is why the cap lives here now.
#[test]
fn the_merged_deck_is_capped_at_what_the_grid_can_address() {
    let _g = crate::testlock::serial();
    reset();
    let deck = |slot: u16, tag: &str| {
        let v: Vec<(i64, String)> = (0..12).map(|i| (i, format!("{tag}{i}"))).collect();
        built(
            slot,
            &v.iter().map(|(t, r)| (*t, r.as_str())).collect::<Vec<_>>(),
            vec![],
        )
    };
    seed(vec![
        src(0, "", HubState::Ready, Some(deck(0, "a"))),
        src(1, "friend", HubState::Ready, Some(deck(1, "b"))),
        src(2, "friend2", HubState::Ready, Some(deck(2, "c"))),
    ]);
    assert_eq!(hub_count(), 1);
    assert_eq!(hub_len(0), MAX_SHELF_ITEMS, "36 cards merged, 24 drawable");
    reset();
}
