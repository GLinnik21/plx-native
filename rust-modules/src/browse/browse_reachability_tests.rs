//! Source reachability/probe-state plumbing, discovery-spawn backoff, and the
//! optimistic watched-edit fanout.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **Reachability is a fact about NOW, and a page fetch is the only evidence that keeps
/// arriving.** `sections_done` latches on success, so the discovery worker never asks that
/// server anything again — without this a source that went offline an hour into the session
/// could never stop reading as reachable, and its group would never dim. It moves in both
/// directions, because a server that came back must stop being dimmed too.
#[test]
fn a_page_fetch_is_what_keeps_reachability_honest_after_discovery() {
    let _g = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    browse.append_sections(1, vec![(1, "Film Club".into(), SecKind::Movie)]);
    assert!(
        browse.state.sources()[1].sections_done,
        "discovery is done — it will never re-ask by itself"
    );

    browse.state.source_mut(1).unwrap().set_reachable(false); // a page for THEIR library did not come back
    assert!(!browse.state.sources()[1].reachable(), "their group dims");
    assert!(
        browse.state.sources()[0].reachable(),
        "…and ours is untouched — it answered"
    );

    browse.state.source_mut(1).unwrap().set_reachable(true); // …and it comes back
    assert!(browse.state.sources()[1].reachable());
    assert_eq!(
        browse.state.sources()[1].retry_cd,
        0,
        "a server that answered is worth re-asking at once"
    );

    browse.state.source_mut(1).unwrap().state = SourceState::Unauthorized;
    browse.state.source_mut(1).unwrap().set_reachable(true);
    assert_eq!(
        browse.state.sources()[1].state,
        SourceState::Reachable,
        "a successful page clears a known 401"
    );
}
/// **The widening contract, pinned.** [`SourceState`] replaced a `bool`, and the whole claim of
/// that commit is that it changed nothing — so the mapping is a test rather than a paragraph.
///
/// The asymmetry is the part worth pinning: **`NotProbed` reads as reachable**. That looks
/// wrong until you recall what it replaced — a source was seeded `reachable: true` at
/// registration precisely so its group would not open dimmed before anything had been dialled.
/// A future reader who "fixes" this to `false` will dim every group for the frames between
/// registration and the first probe, which is a visible flicker on every boot.
#[test]
fn not_probed_reads_as_reachable_and_only_a_failed_dial_dims_a_group() {
    let _g = crate::testlock::serial();
    let mut s = a_source("nas-home", "friend", true);

    s.state = SourceState::NotProbed;
    assert!(
        s.reachable(),
        "nobody has dialled it — the group must not open dimmed"
    );
    assert_eq!(s.tier, None, "and nothing has told us which tier won");

    s.set_reachable(true);
    assert_eq!(s.state, SourceState::Reachable);
    assert!(s.reachable());

    s.set_reachable(false);
    assert_eq!(s.state, SourceState::Unreachable);
    assert!(!s.reachable(), "the ONLY state that dims a group");

    // Answered-but-refused is a token problem, not a network failure. The legacy reachability
    // question therefore remains true, while `ui::source_list` matches Unauthorized directly
    // and dims/disables the group because there is nothing browsable behind that credential.
    s.state = SourceState::Unauthorized;
    s.set_reachable(false);
    assert_eq!(
        s.state,
        SourceState::Unauthorized,
        "a status-folded request cannot erase a known 401"
    );
    assert!(
        s.reachable(),
        "it answered; this old bool projection is only the network question"
    );
}
/// Issue #95 plan §4/S9: `InsecureOnly` is a FIFTH state, and unlike `Unauthorized` it reads
/// `reachable() == false` — there is nothing behind it in this build, not a credential problem
/// on a server that would otherwise work.
#[test]
fn insecure_only_reads_unreachable_and_a_status_fold_cannot_erase_it() {
    let _g = crate::testlock::serial();
    assert_eq!(
        source_state(Some(crate::plex::probe::Outcome::InsecureOnly)),
        SourceState::InsecureOnly
    );
    let mut s = a_source("nas-home", "friend", true);
    s.state = SourceState::InsecureOnly;
    assert!(
        !s.reachable(),
        "verified alive but nothing this build may put a credential on — not the same as NotProbed"
    );
    s.set_reachable(false);
    assert_eq!(
        s.state,
        SourceState::InsecureOnly,
        "a status-folded request cannot erase the more specific InsecureOnly verdict"
    );
    s.set_reachable(true);
    assert_eq!(
        s.state,
        SourceState::Reachable,
        "only a real success clears it"
    );
}

#[test]
fn registry_probe_state_and_tier_seed_and_update_the_browse_source() {
    let _g = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::plex::reset_servers_for_test();
        }
    }
    crate::plex::reset_servers_for_test();
    let _cleanup = Cleanup;
    let mut browse = TestBrowse::default();

    let sid = crate::plex::register_for_test("mach-A", "10.0.0.1", 32400, "tok", "cid");
    crate::plex::client_for(sid)
        .unwrap()
        .set_link(crate::plex::probe::Location::Remote);
    browse.sync_roster();
    assert_eq!(
        browse.state.sources()[0].state,
        SourceState::NotProbed,
        "a restored tier is not a current probe answer"
    );
    assert_eq!(
        browse.state.sources()[0].tier,
        Some(crate::plex::probe::Location::Remote)
    );

    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unauthorized);
    browse.sync_roster();
    assert_eq!(browse.state.sources()[0].state, SourceState::Unauthorized);
    assert_eq!(
        browse.state.sources()[0].tier,
        Some(crate::plex::probe::Location::Remote),
        "cached route metadata is retained"
    );

    crate::plex::client_for(sid)
        .unwrap()
        .set_link(crate::plex::probe::Location::Relay);
    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Reachable);
    browse.sync_roster();
    assert_eq!(browse.state.sources()[0].state, SourceState::Reachable);
    assert_eq!(browse.state.sources()[0].tier, Some(crate::plex::probe::Location::Relay));

    crate::plex::publish_probe_result(sid, crate::plex::probe::Outcome::Unreachable);
    browse.sync_roster();
    assert_eq!(browse.state.sources()[0].state, SourceState::Unreachable);
    assert_eq!(
        browse.state.sources()[0].tier,
        Some(crate::plex::probe::Location::Relay),
        "offline does not erase the last route"
    );
}
/// The same mapping on the projection the renderer actually sees, because `SrcGroup` carries
/// its own copy of the question and two copies are how a widening drifts.
#[test]
fn the_source_group_projection_answers_reachability_the_same_way() {
    let _g = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    // Set the SOURCE directly. `mark_source_reachable` takes a *section* index and resolves the
    // source through it (`sections()[i].src`), so with no sections seeded it early-returns —
    // which cost this test one failing run before the name gave it away.
    browse.state.source_mut(1).unwrap().set_reachable(false);
    let g = browse.state.source_groups();
    assert_eq!(g[0].state, SourceState::Reachable);
    assert!(g[0].reachable());
    assert_eq!(g[1].state, SourceState::Unreachable);
    assert!(!g[1].reachable(), "the dim the Sources list draws");
    assert_eq!(g[1].tier, None);
}
/// A REFUSED discovery spawn must back off, not retry at 60 Hz.
///
/// `maybe_discover` runs once a frame, so releasing the single flight alone re-picks the same
/// source on the very next frame — and `task::spawn` logs every refusal, so the app would write
/// ~60 `task: spawn 'sources' REFUSED` lines a second into the one file on-device triage reads,
/// exactly when the machine is under enough thread pressure to be worth reading about.
///
/// The other half is what it must NOT do: nothing was asked of the server, so the source stays
/// reachable and its discovery stays un-done. A refusal is ours, not theirs.
///
/// Drives `discovery_spawn_refused` rather than `maybe_discover`, because there is no way to
/// make the OS refuse a thread on demand — and then runs a real second of frames through
/// `maybe_discover` to show the backoff actually holds the picker off. That call is safe here
/// precisely BECAUSE the backoff is armed: every source is un-ready, so it decrements the
/// counters and returns without dialling anything.
#[test]
fn a_refused_discovery_spawn_backs_off_instead_of_flooding_the_log() {
    let _g = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![
        a_source("mac-mini", "", true),
        a_source("nas-home", "friend", true),
    ]);
    for i in 0..2 {
        let s = browse.state.source_mut(i).unwrap();
        s.sections_done = false; // both still want discovery
        s.counts_done = true;
    }
    browse.adapter.src_fetching.store(true, Ordering::SeqCst); // as `maybe_discover` armed it before spawning

    browse.state.discovery_spawn_refused_owned(&browse.adapter, 1);

    assert!(
        !browse.adapter.src_fetching.load(Ordering::SeqCst),
        "the single flight must be released"
    );
    assert_eq!(
        browse.state.sources()[1].retry_cd,
        SRC_RETRY_CD,
        "…and the next attempt is ~10s out, not 1 frame"
    );
    assert!(
        browse.state.sources()[1].reachable(),
        "a refused THREAD says nothing about their server"
    );
    assert!(
        !browse.state.sources()[1].sections_done,
        "…and it is still a source waiting to be discovered"
    );
    assert_eq!(browse.state.sources()[0].retry_cd, 0, "the other source is untouched");

    // one second of frames: the picker must not come back to it
    browse.state.source_mut(0).unwrap().retry_cd = SRC_RETRY_CD; // so nothing in this table is dialable
    for _ in 0..60 {
        browse.state.maybe_discover_owned(&browse.adapter, &mut execute_discovery);
    }
    assert!(
        !browse.adapter.src_fetching.load(Ordering::SeqCst),
        "no attempt was made in a whole second"
    );
    assert!(
        browse.state.sources()[1].retry_cd > 0,
        "…and the backoff still has most of its cooldown left"
    );
}
/// An EMPTY count landing is a failure, not an answer: the worker pushes one entry per request
/// that succeeded. Latching `counts_done` on it would leave those rows reading their type word
/// instead of their size for the rest of the session, with nothing able to fix it —
/// `maybe_discover` skips a done source, and this is the bug class the module has now hit twice
/// (the single-flight flags were the first).
#[test]
fn an_empty_count_landing_does_not_latch_the_probe_off() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, _, client) = registered_source();
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    if let Some(s) = browse.state.source_mut(0) {
        s.counts_done = false;
    }
    let epoch = browse.state.table_epoch();

    // nothing came back
    *browse.adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((
        epoch,
        0,
        SrcLanding {
            client,
            token_gen: client.token_gen(),
            name: String::new(),
            what: SrcWhat::Counts(Vec::new()),
        },
    ));
    let _ = browse.state.land_discovery_owned(&browse.adapter);
    assert!(
        !browse.state.sources()[0].counts_done,
        "an empty answer must leave the probe armed"
    );
    assert_eq!(browse.state.sections()[0].count, -1);

    // …and the real one does land, and does latch
    *browse.adapter.src_result.lock().unwrap_or_else(|e| e.into_inner()) = Some((
        epoch,
        0,
        SrcLanding {
            client,
            token_gen: client.token_gen(),
            name: String::new(),
            what: SrcWhat::Counts(vec![(1, 185)]),
        },
    ));
    let _ = browse.state.land_discovery_owned(&browse.adapter);
    assert!(browse.state.sources()[0].counts_done);
    assert_eq!(
        browse.state.sections()[0].count,
        185,
        "the row can say \"185 films\" now"
    );
}
/// With one source providing the canonical Movie and Show libraries, the two permanent type
/// destinations resolve directly to those rows — and the Source chip is absent, not empty.
#[test]
fn one_source_resolves_both_permanent_type_destinations() {
    let _g = crate::testlock::serial();
    // The strip reads the favourite set, and the favourite set is resolved against the
    // RECORDED per-profile answer — so this test needs a session of its own, or it
    // grades whatever the host machine happens to have on disk.
    let _t = TempPins::new("strip-one-source");
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(
        0,
        vec![
            (1, "Movies".into(), SecKind::Movie),
            (2, "TV Shows".into(), SecKind::Show),
        ],
    );
    assert_eq!(browse.tab_count(), browse.section_count());
    for i in 0..browse.section_count() {
        assert_eq!(browse.state.tab_section(i), Some(i));
        assert_eq!(browse.tab_of_section(i), Some(i));
        assert_eq!(browse.tab_title(i), browse.section_title(i));
    }
    assert_eq!(
        browse.state.sources().len(),
        1,
        "…and the Source chip's own condition is false"
    );
}
/// A failure landing from a SUPERSEDED query must not blame the current one: the user has
/// already changed sort/filter/section, a fresh fetch is on its way, and marking the new query
/// Failed would show a failure read-out over a listing that is still perfectly healthy.
#[test]
fn a_stale_failure_landing_does_not_blame_the_current_query() {
    let _g = crate::testlock::serial();
    let (_cleanup, mut browse, _, client) = registered_page_source();
    let stale = browse.state.query_gen();
    browse.state.bump_gen(); // the query moved on under the in-flight fetch
    let r = PageResult {
        client,
        token_gen: client.token_gen(),
        gen: stale,
        sec: 0,
        start: 0,
        items: Vec::new(),
        total: -1,
        sorts: None,
    };
    *browse.adapter.page_result.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    let _outcome = browse.pump();
    assert_eq!(
        browse.fetch_state(),
        SecFetch::Loading,
        "the current query has not answered yet — it has not failed"
    );
}
/// The optimistic edit behind the browse grid's context menu — three properties in one, because
/// they are one press: the mark flips on the frame it is pressed, it flips in EVERY section's
/// store rather than only the one on screen, and a row on another SERVER with the same key is
/// untouched.
///
/// Without this the write went out correctly and nothing on screen changed until a refetch, so
/// the row read as having done nothing — the exact gap `pms::edit_item`'s doc records ("the item
/// is on no shelf (a Library-grid or Related item): nothing to redraw").
#[test]
fn a_watched_edit_reaches_every_section_and_only_the_right_server() {
    let _g = crate::testlock::serial();
    let mut browse = TestBrowse::default();
    seed_one_section(&mut browse);
    let sid = crate::plex::ServerId::UNSET;
    let other = crate::plex::ServerId::from_raw(1);
    let row = |sid, rk: &str, resume: i64| {
        let mut m = PmsMovie::default();
        m.sid = sid;
        m.rk = rk.to_string();
        m.unwatched = true;
        m.resume_ms = resume;
        Some(m)
    };
    {
        let st = browse.state.states_mut();
        *st = vec![SecState::default(), SecState::default()];
        // the section on screen, the one browsed a minute ago (which keeps its items), and a
        // FRIEND's row carrying the same key — both servers number their items from 1
        st[0].items = SecItems::from_vec(vec![row(sid, "7", 90_000), row(other, "7", 0)]);
        st[1].items = SecItems::from_vec(vec![row(sid, "7", 0), row(sid, "9", 0)]);
    }

    assert!(
        browse.state.set_watched_local(sid, "7", true),
        "the item is in the store, so the edit lands"
    );
    let watched = |browse: &TestBrowse, sec: usize, i: usize| {
        let st = browse.state.states();
        let m = st[sec].items.get(i).unwrap();
        (m.watched, m.unwatched, m.resume_ms)
    };
    assert_eq!(
        watched(&browse, 0, 0),
        (true, false, 0),
        "…tick on, and the resume bar retires with it"
    );
    assert_eq!(
        watched(&browse, 1, 0),
        (true, false, 0),
        "…in a section that is not the one being browsed"
    );
    assert_eq!(
        watched(&browse, 0, 1),
        (false, true, 0),
        "…and never on the friend's item with the same key"
    );
    assert_eq!(
        watched(&browse, 1, 1),
        (false, true, 0),
        "…nor on an item that was not asked about"
    );

    assert!(
        !browse.state.set_watched_local(sid, "404", true),
        "an item in no section reports a miss"
    );
}
