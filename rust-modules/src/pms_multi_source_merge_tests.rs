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
        key: String::new(), source: "Alice".into(), total: 0, start: 0, len: 1 };
    let items = vec![row(0, "a"), row(1, "b"), row(0, "c")];
    let want = |slot| Some(HubIdentity::Identifier { sid: sid(slot), id, key: "" });
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
fn home_keeps_recently_added_rows_for_two_same_type_libraries() {
    // `librarySectionTitle` is present on every real PMS item (`docs/pms-api.md` §2) — set here
    // so `localized_hub_title`'s "Recently Added in {library}" override reproduces PMS's own
    // per-library disambiguation exactly, rather than only being exercised on the fallback path.
    let body = r#"{"MediaContainer":{"Hub":[
        {"type":"show","hubIdentifier":"home.television.recent",
         "title":"Recently Added in TV","key":"/hubs/home/recentlyAdded?type=2&sectionID=1",
         "Metadata":[{"ratingKey":"101","librarySectionID":"1","librarySectionTitle":"TV",
                      "type":"show","title":"TV Show","thumb":"/tv.jpg","art":"/tv-art.jpg"}]},
        {"type":"show","hubIdentifier":"home.television.recent",
         "title":"Recently Added in TV HDR","key":"/hubs/home/recentlyAdded?type=2&sectionID=2",
         "Metadata":[{"ratingKey":"202","librarySectionID":"2","librarySectionTitle":"TV HDR",
                      "type":"show","title":"HDR Show","thumb":"/hdr.jpg","art":"/hdr-art.jpg"}]}
    ]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("the two-library PMS response parses")
        .media_container;
    let build = project(&mc, &crate::plex::MediaContainer::default(), sid(0));
    let (items, hubs, _) = merge(&[src(0, "", HubState::Ready, Some(build))]);

    assert_eq!(hubs.len(), 2, "both same-type library shelves reach Home");
    assert_eq!(
        hubs.iter().map(|hub| hub.title.as_str()).collect::<Vec<_>>(),
        ["Recently Added in TV", "Recently Added in TV HDR"]
    );
    assert_eq!(
        hubs.iter().map(|hub| items[hub.start].sec).collect::<Vec<_>>(),
        [1, 2],
        "each shelf still points at its own library section"
    );
    assert_ne!(
        stable_hub_identity(&hubs[0], &items),
        stable_hub_identity(&hubs[1], &items),
        "Home must not fold two section shelves that share a hubIdentifier"
    );
}

/// Reviewer correction to issue #12's fix: a `home.<type>.recent` hub PMS mints because a
/// household owns exactly ONE library of that type reads more naturally by type ("Recently Added
/// Movies") than by library name ("Recently Added in Movies") — the per-library form stays
/// reserved for the case directly above, where PMS is disambiguating between two same-type
/// libraries by minting the identifier twice. A household with one movie library and one TV
/// library gets one `home.movies.recent` hub and one `home.television.recent` hub, each the
/// ONLY hub under its own identifier, so both take the per-type wording.
#[test]
fn one_movie_and_one_tv_library_get_the_natural_per_type_recently_added_titles() {
    let body = r#"{"MediaContainer":{"Hub":[
        {"type":"movie","hubIdentifier":"home.movies.recent",
         "title":"Recently Added Movies","key":"/hubs/home/recentlyAdded?type=1",
         "Metadata":[{"ratingKey":"1","librarySectionID":"1","librarySectionTitle":"Movies",
                      "type":"movie","title":"A Film","thumb":"/m.jpg","art":"/m-art.jpg"}]},
        {"type":"show","hubIdentifier":"home.television.recent",
         "title":"Recently Added TV","key":"/hubs/home/recentlyAdded?type=2",
         "Metadata":[{"ratingKey":"101","librarySectionID":"2","librarySectionTitle":"TV",
                      "type":"show","title":"TV Show","thumb":"/tv.jpg","art":"/tv-art.jpg"}]}
    ]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("the one-movie-one-tv PMS response parses")
        .media_container;
    let build = project(&mc, &crate::plex::MediaContainer::default(), sid(0));

    assert_eq!(build.shelves.len(), 2);
    assert_eq!(
        build.shelves.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(),
        ["Recently Added Movies", "Recently Added TV"],
        "each hubIdentifier is the household's ONLY hub of that type, so both read by type, \
         not by library name"
    );
}

/// Issue #12: a Belarusian UI showed some Home hub titles in Belarusian and others in whatever
/// PMS's own per-string coverage for `be` happened to answer. `localized_hub_title` closes that
/// for every STANDARD hub by overriding `Hub.title` client-side — unconditionally, the same way
/// Continue Watching's title never came from PMS at all — while a hub this catalog does not
/// recognize keeps drawing PMS's own text verbatim (checked below).
#[test]
fn a_recently_added_library_hub_renders_the_be_catalog_string_under_a_be_ui() {
    let _thread_locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::Be);
    let body = r#"{"MediaContainer":{"Hub":[
        {"type":"movie","hubIdentifier":"movie.recentlyadded.1",
         "title":"Recently Added in Movies","key":"/library/sections/1/all?sort=addedAt:desc",
         "Metadata":[{"ratingKey":"1","librarySectionID":"1","librarySectionTitle":"Movies",
                      "type":"movie","title":"A Film","thumb":"/t.jpg","art":"/a.jpg"}]}
    ]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("a PMS body with a per-library Recently Added hub parses")
        .media_container;
    let build = project(&mc, &crate::plex::MediaContainer::default(), sid(0));

    assert_eq!(build.shelves.len(), 1);
    assert_eq!(
        build.shelves[0].title, "Нядаўна дададзена ў «Movies»",
        "a be UI renders THIS client's be string for a known hubIdentifier, not PMS's own \
         (possibly untranslated) title"
    );
}

/// The `be` half of the per-type correction above: a household's only movie library still gets
/// the per-type "Recently Added Movies" wording (`browse.home.hub.recently_added_movies`), not
/// the per-library `{library}`-interpolated form, under a `be` UI.
#[test]
fn a_lone_movie_library_renders_the_be_per_type_catalog_string_under_a_be_ui() {
    let _thread_locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::Be);
    let body = r#"{"MediaContainer":{"Hub":[
        {"type":"movie","hubIdentifier":"home.movies.recent",
         "title":"Recently Added Movies","key":"/hubs/home/recentlyAdded?type=1",
         "Metadata":[{"ratingKey":"1","librarySectionID":"1","librarySectionTitle":"Movies",
                      "type":"movie","title":"A Film","thumb":"/m.jpg","art":"/m-art.jpg"}]}
    ]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("a PMS body with a lone whole-server Recently Added hub parses")
        .media_container;
    let build = project(&mc, &crate::plex::MediaContainer::default(), sid(0));

    assert_eq!(build.shelves.len(), 1);
    assert_eq!(
        build.shelves[0].title, "Нядаўна дададзеныя фільмы",
        "a be UI renders THIS client's be per-type string, not the per-library form"
    );
}

/// The other half of the same rule: a hubIdentifier this catalog does not enumerate — a custom
/// collection shelf, a promoted rail PMS mints under an id this app has never seen — keeps
/// drawing PMS's own `title` verbatim, in any language, exactly as it did before this change.
#[test]
fn an_unrecognized_hub_identifier_keeps_the_pms_title_verbatim() {
    let _thread_locale = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::Be);
    let body = r#"{"MediaContainer":{"Hub":[
        {"type":"movie","hubIdentifier":"custom.collection.987",
         "title":"Прайдзiсветы i Незнаёмцы","key":"/library/collections/987/children",
         "Metadata":[{"ratingKey":"5","librarySectionID":"1","librarySectionTitle":"Movies",
                      "type":"movie","title":"A Film","thumb":"/t.jpg","art":"/a.jpg"}]}
    ]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("a PMS body with an unrecognized (custom-collection-shaped) hub parses")
        .media_container;
    let build = project(&mc, &crate::plex::MediaContainer::default(), sid(0));

    assert_eq!(build.shelves.len(), 1);
    assert_eq!(
        build.shelves[0].title, "Прайдзiсветы i Незнаёмцы",
        "an unknown hubIdentifier has no client-side catalog entry to substitute"
    );
}

#[test]
fn a_mixed_section_hub_keeps_its_identity_when_the_leading_library_changes() {
    let body = |first_section: i64, first_key: &str, second_section: i64, second_key: &str| {
        format!(r#"{{"MediaContainer":{{"Hub":[{{
            "type":"show","hubIdentifier":"home.television.recent",
            "title":"Recently Added TV","key":"/hubs/home/recentlyAdded?type=2",
            "Metadata":[
                {{"ratingKey":"{first_key}","librarySectionID":"{first_section}",
                  "type":"show","title":"First","thumb":"/first.jpg","art":"/first-art.jpg"}},
                {{"ratingKey":"{second_key}","librarySectionID":"{second_section}",
                  "type":"show","title":"Second","thumb":"/second.jpg","art":"/second-art.jpg"}}
            ]
        }}]}}}}"#)
    };
    let projection = |wire: String| {
        let mc = serde_json::from_str::<crate::plex::Envelope>(&wire)
            .expect("the mixed-library PMS response parses")
            .media_container;
        let build = project(&mc, &crate::plex::MediaContainer::default(), sid(0));
        let (items, hubs, _) = merge(&[src(0, "", HubState::Ready, Some(build))]);
        let identity = match stable_hub_identity(&hubs[0], &items) {
            Some(HubIdentity::Identifier { sid, id, key }) => {
                (sid, id.to_owned(), key.to_owned())
            }
            other => panic!("expected an identified provider hub, got {other:?}"),
        };
        (items[hubs[0].start].sec, identity)
    };

    let first = projection(body(1, "101", 2, "202"));
    let second = projection(body(2, "202", 1, "101"));
    assert_eq!((first.0, second.0), (1, 2), "the fixture changes the leading library");

    assert_eq!(
        first.1,
        second.1,
        "content order cannot change the provider-published hub identity"
    );
}

#[test]
fn merged_home_deck_identity_survives_a_different_leading_server() {
    let mut hub = HubRow { title: "Continue Watching".into(), hub_id: "home.continue".into(),
        key: String::new(), source: String::new(), total: 0, start: 0, len: 1 };
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
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
        src(0, "", HubState::Loading, None),
        src(1, "friend", HubState::Loading, None),
    ]);

    land(
        &o.state, &o.adapter,
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
    land(&o.state, &o.adapter, 1, None);
    pump(&mut o.state, &o.adapter, 0.0);

    assert_eq!(
        hub_state(&o.state),
        HubState::Ready,
        "one server answering is an answered Home"
    );
    assert_eq!(
        hub_count(&o.state),
        1,
        "the dead share contributes NOTHING — no heading, no empty shelf"
    );
    assert_eq!(hub_len(&o.state, 0), 2);
    assert_eq!(
        hub_source(&o.state, 0),
        "",
        "and the shelf that did arrive is our own, so it is unannotated"
    );
    let s = &o.state.srcs;
    assert_eq!(s[0].state, HubState::Ready);
    assert_eq!(s[1].state, HubState::Failed);
    assert!(s[1].retry_s > 0.0, "the share retries on its own ladder…");
    assert_eq!(s[0].retry_s, 0.0, "…and the working server owes nothing");
    reset(&mut o.state, &o.adapter);
}

/// …and the same rule once BOTH sources are populated, which is the state a user is actually
/// sitting in front of when a friend's server goes to sleep. The failing source keeps the
/// shelves it last answered with, the working source is not touched at all, and neither the
/// catalog nor the deck loses a row.
#[test]
fn a_failing_source_leaves_a_populated_home_completely_intact() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
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
    let before: Vec<(String, String, usize)> = (0..hub_count(&o.state))
        .map(|i| (hub_title(&o.state, i).to_string(), hub_source(&o.state, i).to_string(), hub_len(&o.state, i)))
        .collect();
    assert_eq!(before.len(), 3, "deck + one shelf each");
    let rows = catalog(&o.state).len();

    land(&o.state, &o.adapter, 1, None); // the share stops answering
    pump(&mut o.state, &o.adapter, 0.0);

    let after: Vec<(String, String, usize)> = (0..hub_count(&o.state))
        .map(|i| (hub_title(&o.state, i).to_string(), hub_source(&o.state, i).to_string(), hub_len(&o.state, i)))
        .collect();
    assert_eq!(after, before, "not one shelf, heading or row moved");
    assert_eq!(catalog(&o.state).len(), rows);
    assert_eq!(
        rks(&o.state, 0),
        ["own-cw", "their-cw"],
        "and the merged deck kept both servers' items"
    );
    assert_eq!(
        hub_state(&o.state),
        HubState::Ready,
        "Home is not failed while a source is answering"
    );
    assert_eq!(
        o.state.srcs[0].retry_s,
        0.0,
        "the working source owes no backoff"
    );
    reset(&mut o.state, &o.adapter);
}

/// A PARTIAL landing repaints. A settled Home stops presenting entirely (`plx_machine::idle`), so a
/// source arriving seconds after the owned server did — which is the normal shape of this
/// feature, not an edge case — would otherwise draw its shelves invisibly until the next
/// keypress. The failure half matters just as much: a retry that fails changes the status
/// caption under an empty Home.
#[test]
fn a_source_landing_repaints_a_settled_home() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    plx_machine::idle::set_enabled(true);
    seed(&mut o.state, vec![
        src(0, "", HubState::Ready, Some(build_test(1))),
        src(1, "friend", HubState::Loading, None),
    ]);

    for (what, build) in [
        ("a share arriving", Some(build_test(2))),
        ("a share failing", None),
    ] {
        plx_machine::idle::should_present(0); // takes-and-clears whatever was already pending
        assert!(
            !plx_machine::idle::should_present(0),
            "the panel is settled with nothing happening"
        );
        land(&o.state, &o.adapter, 1, build);
        pump(&mut o.state, &o.adapter, 0.0);
        assert!(
            plx_machine::idle::should_present(0),
            "{what} must invalidate the frame"
        );
    }
    reset(&mut o.state, &o.adapter);
}

/// The total-failure read-out is reserved for a total failure. Any source answering makes Home
/// answered; a mix of failed and still-loading is still loading.
#[test]
fn only_every_source_failing_reads_as_a_failed_home() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
        src(0, "", HubState::Failed, None),
        src(1, "friend", HubState::Loading, None),
    ]);
    assert_eq!(
        hub_state(&o.state),
        HubState::Loading,
        "one source still trying is not a dead Home"
    );

    seed(&mut o.state, vec![
        src(0, "", HubState::Failed, None),
        src(1, "friend", HubState::Ready, None),
    ]);
    assert_eq!(
        hub_state(&o.state),
        HubState::Ready,
        "a share being down says nothing about our own"
    );

    seed(&mut o.state, vec![
        src(0, "", HubState::Failed, None),
        src(1, "friend", HubState::Failed, None),
    ]);
    assert_eq!(
        hub_state(&o.state),
        HubState::Failed,
        "everything down IS the whole-screen case"
    );
    reset(&mut o.state, &o.adapter);
}

/// Continue Watching is ONE shelf across every source, ordered by when the owner last watched —
/// so a borrowed item legitimately holds first position, and the heading therefore cannot claim
/// an owner. It carries no annotation at all. This is the official client's own shape: the
/// owner's screenshots show a friend's films sitting BETWEEN their own, in one row.
#[test]
fn continue_watching_merges_across_sources_by_last_viewed() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
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

    assert_eq!(hub_count(&o.state), 1, "one deck, not one per server");
    assert!(hub_is_continue(&o.state, 0));
    assert_eq!(
        hub_source(&o.state, 0),
        "",
        "a shelf drawn from two servers cannot be named by one of them"
    );
    // The timestamps INTERLEAVE on purpose: a per-source concatenation, which is what the
    // obvious implementation of "merge" does, would give own-old, own-oldest, their-new,
    // their-mid and pass any test written with one server's deck in front of the other's.
    assert_eq!(rks(&o.state, 0), ["their-new", "own-old", "their-mid", "own-oldest"]);
    reset(&mut o.state, &o.adapter);
}

/// Every OTHER shelf keeps its source: the owner's handle for a borrowed server, empty for our
/// own. And the groups stay contiguous in roster order — adjacency is the grouping device, so a
/// source's shelves may never be interleaved with another's.
#[test]
fn every_other_shelf_carries_its_source_and_the_groups_stay_contiguous() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
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

    let by_row: Vec<(&str, &str)> = (0..hub_count(&o.state))
        .map(|i| (hub_title(&o.state, i), hub_source(&o.state, i)))
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
    reset(&mut o.state, &o.adapter);
}

/// A source that has never answered contributes nothing — no heading, no empty shelf, no
/// spinner row (which would also hold the panel presenting for a source that isn't coming).
/// One that HAS answered and has since failed keeps what it last had: a transient failure must
/// not reflow the shelves under the focus ring.
#[test]
fn a_source_that_never_answered_draws_nothing_at_all() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
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
    let by_row: Vec<(&str, &str)> = (0..hub_count(&o.state))
        .map(|i| (hub_title(&o.state, i), hub_source(&o.state, i)))
        .collect();
    assert_eq!(by_row, [("Films", ""), ("Docs", "friend2")]);
    reset(&mut o.state, &o.adapter);
}

/// A source that leaves the roster takes its shelves with it at the next read — the one thing
/// that DOES remove a live source's rows, because "gone" (un-pinned, or a revoked share) is a
/// fact about the grant rather than about a fetch that happened to fail.
#[test]
fn a_source_that_leaves_the_roster_stops_contributing() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    seed(&mut o.state, vec![
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
    assert_eq!(hub_count(&o.state), 2);

    // the registry a host test has is empty, so the roster this recomputes to is empty too
    forget_roster(&mut o.state);
    sync_roster(&mut o.state);

    assert_eq!(hub_count(&o.state), 0, "both un-rostered sources' shelves are gone");
    assert!(o.state.srcs.is_empty());
    reset(&mut o.state, &o.adapter);
}

#[test]
fn an_equal_size_roster_replacement_has_a_different_cache_key_and_source_table() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    crate::plex::reset_servers_for_test();
    reset(&mut o.state, &o.adapter);
    let a = crate::plex::register_for_test("pms-a", "127.0.0.1", 1, "a", "cid");
    let b = crate::plex::register_for_test("pms-b", "127.0.0.1", 2, "b", "cid");
    sync_roster(&mut o.state);
    let before = roster_key();
    assert_eq!(
        o.state.srcs.iter().map(|s| s.sid).collect::<Vec<_>>(),
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
    sync_roster(&mut o.state);
    assert_eq!(
        o.state.srcs.iter().map(|s| s.sid).collect::<Vec<_>>(),
        [a, c]
    );

    reset(&mut o.state, &o.adapter);
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
    let _guard = plx_base::testlock::serial();
    let mut o = Owner::default();
    crate::plex::reset_servers_for_test();
    reset(&mut o.state, &o.adapter);
    let sid = crate::plex::register_for_test(
        "equal-generation-home", "127.0.0.1", 9, "synthetic", "fixture");
    let alpha = two_library_directory(sid, true);
    let beta = two_library_directory(sid, false);
    let alpha_scope = BrowseScope::retained(alpha.view());
    let beta_scope = BrowseScope::retained(beta.view());
    assert_eq!(alpha_scope.sections_gen, beta_scope.sections_gen,
        "the regression requires equal owner-local generations");
    seed_two_library_home_for_test(&mut o.state, sid, alpha.view());
    assert_eq!(rks(&o.state, 0), ["alpha"]);

    sync_roster_with_scope(&mut o.state, &beta_scope);

    assert_eq!(rks(&o.state, 0), ["beta"],
        "the PMS cache must not alias an independent equal-generation owner");
    reset(&mut o.state, &o.adapter);
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

/// Issue #395: a household with 17 Home rows saw 16. The shelf count has no ceiling of its own;
/// only the card budget bounds Home.
#[test]
fn the_reporters_seventeen_rows_all_reach_home() {
    let cw: Vec<(i64, String)> = (0..12).map(|i| (100 - i, format!("cw{i}"))).collect();
    let cw: Vec<(i64, &str)> = cw.iter().map(|(t, r)| (*t, r.as_str())).collect();
    let shelves: Vec<Shelf> = (0..16)
        .map(|s| {
            let keys: Vec<String> = (0..12).map(|i| format!("s{s}-{i}")).collect();
            shelf(0, &format!("Shelf {s}"), &format!("x.{s}"),
                &keys.iter().map(|r| r.as_str()).collect::<Vec<_>>())
        })
        .collect();
    let (_, hubs, _) = merge(&[src(0, "", HubState::Ready, Some(built(0, &cw, shelves)))]);
    assert_eq!(hubs.len(), 17, "the deck and all sixteen shelves");
    assert!(hubs.iter().all(|h| h.len == 12), "each row whole");
}

/// A shelf is published whole or not at all: when the card budget runs out mid-shelf the shelf and
/// everything after it are left off, never a truncated tail row.
#[test]
fn no_shelf_is_published_partially() {
    let cw: Vec<(i64, String)> = (0..24).map(|i| (100 - i, format!("cw{i}"))).collect();
    let cw: Vec<(i64, &str)> = cw.iter().map(|(t, r)| (*t, r.as_str())).collect();
    let shelves: Vec<Shelf> = (0..25)
        .map(|s| {
            let keys: Vec<String> = (0..12).map(|i| format!("s{s}-{i}")).collect();
            shelf(0, &format!("Shelf {s}"), &format!("x.{s}"),
                &keys.iter().map(|r| r.as_str()).collect::<Vec<_>>())
        })
        .collect();
    let (items, hubs, _) = merge(&[src(0, "", HubState::Ready, Some(built(0, &cw, shelves)))]);
    assert_eq!(hubs[0].len, 24);
    assert!(hubs[1..].iter().all(|h| h.len == 12), "no shelf is cut short");
    assert_eq!(hubs.len(), 20, "24 + 19 x 12 = 252 cards; the 20th shelf would be 264");
    assert_eq!(items.len(), 252);
}

/// The budget is split, not raced for. Whoever is asked first used to spend it — and `/hubs`
/// promotes several rows per library, so one four-library server can spend the whole card
/// budget and the share behind it drew nothing.
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

    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let many = |slot: u16, tag: &str| {
        (0..300)
            .map(|i| shelf(slot, &format!("{tag}{i}"), "x", &["r"]))
            .collect::<Vec<_>>()
    };
    seed(&mut o.state, vec![
        src(0, "", HubState::Ready, Some(built(0, &[], many(0, "a")))),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(1, &[], many(1, "b"))),
        ),
    ]);
    // one-card shelves, so the card budget is what binds: no shelf cap, 128 cards (= shelves) each
    assert_eq!(
        hub_count(&o.state),
        PMS_MAX_MOVIES,
        "the card cap is the only ceiling"
    );
    let theirs = (0..hub_count(&o.state))
        .filter(|&i| hub_source(&o.state, i) == "friend")
        .count();
    assert_eq!(
        theirs,
        PMS_MAX_MOVIES / 2,
        "and the share gets its half rather than the leftovers"
    );
    reset(&mut o.state, &o.adapter);
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
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    crate::plex::reset_servers_for_test();
    reset(&mut o.state, &o.adapter);
    let s = crate::plex::register_for_test("pms-credit", "127.0.0.1", 1, "t", "cid");
    assert_eq!(s, sid(0), "a fresh registry hands out slot 0");

    // what a build without the rule published: the household's own server, wearing the account
    // holder's handle, with shelves already merged from it
    crate::plex::describe_server(s, "Mac mini", "admin", crate::plex::GrantEvidence::outside());
    seed(&mut o.state, vec![src(
        0,
        "admin",
        HubState::Ready,
        Some(built(0, &[], vec![shelf(0, "Recently Added", "x", &["r1"])])),
    )]);
    assert_eq!(hub_source(&o.state, 0), "admin");

    // the roster refresh re-grades it — and there is deliberately NO landing after this
    crate::plex::describe_server(s, "Mac mini", "", crate::plex::GrantEvidence::outside());
    sync_roster(&mut o.state);

    assert_eq!(
        hub_source(&o.state, 0),
        "",
        "the shelf follows the registry off a credit without waiting for a fetch"
    );
    assert_eq!(hero_pool_source(&o.state, 0), "");
    assert_eq!(
        o.state.srcs[0].handle,
        "",
        "and the source itself is re-read, not only the rows"
    );

    reset(&mut o.state, &o.adapter);
    crate::plex::reset_servers_for_test();
}

/// The same split over catalog ROWS, which is the cap the shelves' items come out of.
#[test]
fn the_row_budget_is_shared_too() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
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
    seed(&mut o.state, vec![
        src(0, "", HubState::Ready, Some(built(0, &[], fat(0, "a")))),
        src(
            1,
            "friend",
            HubState::Ready,
            Some(built(1, &[], fat(1, "b"))),
        ),
    ]);
    // each source's share is 128 cards, and a shelf is whole or absent: 5 shelves of 24 = 120
    let per_source = |who: &str| -> usize {
        (0..hub_count(&o.state))
            .filter(|&i| hub_source(&o.state, i) == who)
            .map(|i| hub_len(&o.state, i))
            .sum()
    };
    let (ours, theirs) = (per_source(""), per_source("friend"));
    assert_eq!(ours, theirs, "equal shares, not the leftovers");
    assert_eq!(theirs, (PMS_MAX_MOVIES / 2 / MAX_SHELF_ITEMS) * MAX_SHELF_ITEMS);
    assert_eq!(theirs, 120);
    assert!(catalog(&o.state).len() <= PMS_MAX_MOVIES, "the total cap still holds");
    assert_eq!(catalog(&o.state).len(), ours + theirs);
    reset(&mut o.state, &o.adapter);
}

/// The merged deck is capped at what the grid can ADDRESS. Three sources' Continue Watching is
/// up to 36 cards, and past `MAX_SHELF_ITEMS` the home grid's focus ring and its OK dispatch clamp
/// differently — the ring stops at the last addressable card while the press opens whatever
/// column the raw index names. Unreachable with one server, which is why the cap lives here now.
#[test]
fn the_merged_deck_is_capped_at_what_the_grid_can_address() {
    let _g = plx_base::testlock::serial();
    let mut o = Owner::default();
    reset(&mut o.state, &o.adapter);
    let deck = |slot: u16, tag: &str| {
        let v: Vec<(i64, String)> = (0..12).map(|i| (i, format!("{tag}{i}"))).collect();
        built(
            slot,
            &v.iter().map(|(t, r)| (*t, r.as_str())).collect::<Vec<_>>(),
            vec![],
        )
    };
    seed(&mut o.state, vec![
        src(0, "", HubState::Ready, Some(deck(0, "a"))),
        src(1, "friend", HubState::Ready, Some(deck(1, "b"))),
        src(2, "friend2", HubState::Ready, Some(deck(2, "c"))),
    ]);
    assert_eq!(hub_count(&o.state), 1);
    assert_eq!(hub_len(&o.state, 0), MAX_SHELF_ITEMS, "36 cards merged, 24 drawable");
    reset(&mut o.state, &o.adapter);
}
