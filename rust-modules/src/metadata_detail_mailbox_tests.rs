//! The detail mailbox: landing, alt-source resolution, the Related shelf, and spawn/settle
//! races on the detail fetch pipeline.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use std::sync::atomic::Ordering;

/// **The cross-source projection, on the real measured shape.** A `/library/all?guid=…` answer
/// is the OTHER server's own row: its own `ratingKey`, its own library, and — measured against
/// this household's two servers on 2026-08-14 — its own localized title for the same film.
/// Everything a row needs must come off that answer, because nothing about the page we are on
/// describes the copy over there.
///
/// Pure: no statics, no socket, so no serial lock. It grades `resolve_alt_sources`'s projection
/// by feeding the container directly, which is the half that decides whether the panel offers
/// the right film.
#[test]
fn a_guid_answer_projects_the_other_servers_own_key_library_and_class() {
    let body = r#"{"MediaContainer":{"size":1,"Metadata":[{
        "ratingKey":"5274","type":"movie","title":"another title entirely",
        "guid":"plex://movie/6856893830a4aaafd5c4291d","librarySectionTitle":"Film Club",
        "duration":7020000,"Media":[{"videoResolution":"1080","width":1920,"height":1080}]}]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("parses")
        .media_container;

    let m = mc.metadata.first().expect("one row");
    assert_eq!(
        m.guid, "plex://movie/6856893830a4aaafd5c4291d",
        "the portable identity is read"
    );
    assert_eq!(m.rating_key, "5274", "…and the key is THEIRS, not ours");
    assert_eq!(
        m.library_section_title, "Film Club",
        "the library names the row, not the machine"
    );
    assert_eq!(m.duration, 7_020_000);
    assert_eq!(
        m.media.first().map(|x| x.video_resolution.as_str()),
        Some("1080")
    );
}

/// **The Related shelf's rows carry the watch state that was on the wire all along.**
///
/// The reported bug — a long press on a Related tile doing nothing — was explained by "a Related
/// row has no `(ratingKey, watched)` pair to build menu rows from", and that was true of the
/// old three-field struct while being false of the response. This is the test that keeps the two
/// from drifting apart again: it feeds `/related`'s REAL shape and asserts the fields the shelf's
/// tick, its resume bar and its context menu are each built from.
///
/// Three details are deliberately in the fixture rather than idealised away:
/// * `viewOffset`/`duration` arrive as JSON **strings**, which PMS really does (see
///   `plex/CLAUDE.md` — a non-lenient adapter fails the WHOLE container, not one field);
/// * `viewCount` is **absent** on an unwatched row rather than `0`;
/// * the show is **part-watched** (`viewedLeafCount < leafCount`), the state that is neither
///   watched nor unwatched and the one a `viewCount > 0` shortcut gets wrong.
#[test]
fn related_rows_carry_the_watch_state_the_wire_already_had() {
    let body = r#"{"MediaContainer":{"Hub":[{"title":"Similar Movies","Metadata":[
        {"ratingKey":"11","type":"movie","title":"finished","duration":"7020000","viewCount":2,
         "Media":[{"Part":[{"key":"/library/parts/11/file.mkv"}]}]},
        {"ratingKey":"12","type":"movie","title":"halfway","duration":"7020000","viewOffset":"3510000"},
        {"ratingKey":"13","type":"movie","title":"never started","duration":7020000},
        {"ratingKey":"14","type":"show","title":"three in","leafCount":10,"viewedLeafCount":3}
    ]}]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("parses")
        .media_container;
    let rows = related_rows(&mc, SRV_B);
    assert_eq!(rows.len(), 4, "every hub row with a key becomes a tile");

    // …and every row is stamped with the server it was FETCHED from. A related item is a key on
    // the page's own server, and both servers number from 1, so this is the field that keeps the
    // art request, the menu's SID and the scrobble off the wrong machine.
    assert!(
        rows.iter().all(|m| m.sid == SRV_B),
        "the row's server is the one that answered"
    );

    // finished: the tick, and no bar (`resume_frac` is None with no viewOffset)
    assert!(rows[0].watched && !rows[0].unwatched);
    assert_eq!(rows[0].resume_frac(), None);
    assert_eq!(
        rows[0].part, "/library/parts/11/file.mkv",
        "Play from Start needs the part id"
    );

    // halfway: the bar, at the fraction the wire's STRING-encoded numbers give
    assert!(
        !rows[1].watched && rows[1].unwatched,
        "a resume point is not a view count"
    );
    assert_eq!(
        rows[1].resume_frac(),
        Some(0.5),
        "the amber bar's fraction, off duration + viewOffset"
    );

    // never started: neither mark — and `viewCount` was absent, not zero
    assert!(!rows[2].watched && rows[2].unwatched);
    assert_eq!(rows[2].resume_frac(), None);

    // the part-watched SHOW: NEITHER flag, which is the state the menu turns into both verbs
    assert_eq!(
        rows[3].kind, 1,
        "the item KIND decides the menu's leaf/container rule"
    );
    assert!(!rows[3].watched, "3 of 10 leaves is not done");
    assert!(!rows[3].unwatched, "…and it is not untouched either");
}

/// The two bounds on the shelf, which are one function's job and were easy to lose in the move
/// to the shared row mapping.
///
/// **De-duplication is across the whole response, not per hub** — PMS's related hubs overlap
/// heavily, so the same film is routinely in two of them and a flattened strip would draw it
/// twice side by side. **The cap counts kept rows**, so a response padded with duplicates cannot
/// spend the budget on tiles that were never added.
#[test]
fn related_rows_dedupe_across_hubs_and_cap_the_shelf() {
    let hub = |keys: &[i32]| {
        let rows: Vec<String> = keys
            .iter()
            .map(|k| format!(r#"{{"ratingKey":"{k}","type":"movie","title":"t{k}"}}"#))
            .collect();
        format!(r#"{{"Metadata":[{}]}}"#, rows.join(","))
    };
    // the same three keys in two hubs, plus one the second hub alone has
    let body = format!(
        r#"{{"MediaContainer":{{"Hub":[{},{}]}}}}"#,
        hub(&[1, 2, 3]),
        hub(&[2, 3, 4])
    );
    let mc = serde_json::from_str::<crate::plex::Envelope>(&body)
        .expect("parses")
        .media_container;
    let rows = related_rows(&mc, SRV_A);
    let keys: Vec<&str> = rows.iter().map(|m| m.rk.as_str()).collect();
    assert_eq!(
        keys,
        ["1", "2", "3", "4"],
        "one tile per title, in first-seen order"
    );

    // a row PMS sent no key for is not a tile — it addresses nothing
    let body =
        r#"{"MediaContainer":{"Hub":[{"Metadata":[{"type":"movie","title":"keyless"}]}]}}"#;
    let mc = serde_json::from_str::<crate::plex::Envelope>(body)
        .expect("parses")
        .media_container;
    assert!(related_rows(&mc, SRV_A).is_empty(), "no ratingKey, no tile");

    // the cap, counted in KEPT rows: 30 distinct keys, each repeated twice
    let many: Vec<i32> = (0..30).collect();
    let body = format!(
        r#"{{"MediaContainer":{{"Hub":[{},{}]}}}}"#,
        hub(&many),
        hub(&many)
    );
    let mc = serde_json::from_str::<crate::plex::Envelope>(&body)
        .expect("parses")
        .media_container;
    let rows = related_rows(&mc, SRV_A);
    assert_eq!(rows.len(), RELATED_MAX, "the shelf is capped");
    assert_eq!(
        rows.last().map(|m| m.rk.as_str()),
        Some("19"),
        "…at the 20th DISTINCT title"
    );
}

/// A server that answers "I do not have it" contributes no row — and is not confused with one
/// that did not answer. Both yield nothing here; only the client keeps them apart (see
/// `find_by_guid`), which is what lets a later revision say "not reachable" in the panel.
#[test]
fn a_server_without_the_film_contributes_no_row() {
    let mc = serde_json::from_str::<crate::plex::Envelope>(r#"{"MediaContainer":{"size":0}}"#)
        .expect("parses")
        .media_container;
    assert!(
        mc.metadata.is_empty(),
        "size=0 is an answer, and it is an empty one"
    );
}

/// **The cross-source resolve carries its SERVER through the mailbox, and the pump hands both
/// halves to the panel.** The generation guard beside it cannot stand in for this: `test_adapter().alt_gen`
/// only moves when a DETAIL lands, so a page opened while a resolve is out — the whole reason
/// this is asynchronous, since one dead share costs a `connect(2)` timeout — is a page whose
/// own detail is still in flight, and the landing sails through the generation test. The rk
/// then matched too, because both servers number their items from 1: the panel listed the
/// other machine's copies and OK on one opened a different film.
///
/// Drives the real `pump_alt_sources` (the mailbox is filled directly, as the detail test does
/// for its failure case — there is no `land_alt` for a test to reach) and grades the GATE the
/// Detail page asks, which is the thing a user would see appear or not appear.
///
/// **The store is ADDRESSED since restructure phase 10, so this reads as a refusal on the way
/// OUT rather than on the way in** — the landing is filed under the pair it was resolved for,
/// and the page that is mounted asks about its own. The assertion is unchanged and is the one
/// that matters: our copies are not news about the share's film 4.
#[test]
fn an_alt_sources_landing_for_another_servers_copy_with_the_same_key_is_refused() {
    let _serial = crate::testlock::serial();
    alt_clear(test_state());
    // two copies on two sources — enough for the gate, which counts distinct SOURCES
    let copies = || {
        vec![
            AltCopy {
                sid: SRV_A,
                rk: "4".into(),
                ..Default::default()
            },
            AltCopy {
                sid: SRV_B,
                rk: "318".into(),
                ..Default::default()
            },
        ]
    };
    let land = |gen: u32, sid: crate::plex::ServerId, rk: &str| {
        test_adapter().alt_roster_gen.store(crate::plex::server_roster_gen(), Ordering::SeqCst);
        *test_adapter().alt_slot.lock().unwrap() = Some(AltResult {
            gen,
            roster_gen: crate::plex::server_roster_gen(),
            sid,
            rk: rk.to_string(),
            list: copies(),
        });
    };

    // our film 4 is the mounted page and its resolve is out…
    let gen = test_adapter().alt_gen.fetch_add(1, Ordering::SeqCst) + 1;
    // …and while it is out the user lands on the SHARE's film 4
    land(gen, SRV_A, "4");
    pump_alt_sources(test_state(), test_adapter());
    assert!(
        !alt_available(test_state(), SRV_B, "4"),
        "our copies are not news about the share's film"
    );

    // the control: the very same landing DOES reach the page that asked for it
    assert!(alt_available(test_state(), SRV_A, "4"), "the awaited landing installs");

    // …and a SUPERSEDED landing is dropped one layer earlier, by the generation
    alt_clear(test_state());
    let stale = test_adapter().alt_gen.fetch_add(1, Ordering::SeqCst) + 1;
    test_adapter().alt_gen.fetch_add(1, Ordering::SeqCst);
    land(stale, SRV_A, "4");
    pump_alt_sources(test_state(), test_adapter());
    assert!(
        !alt_available(test_state(), SRV_A, "4"),
        "a landing from a superseded resolve is dropped"
    );

    alt_clear(test_state());
}

#[test]
fn an_alt_source_from_a_revoked_slot_is_pruned_and_its_inflight_result_is_discarded() {
    let _serial = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let a = crate::plex::register_for_test("alt-a", "127.0.0.1", 1, "a", "cid");
    let b = crate::plex::register_for_test("alt-b", "127.0.0.1", 2, "b", "cid");
    let copies = vec![
        AltCopy {
            sid: a,
            rk: "4".into(),
            ..Default::default()
        },
        AltCopy {
            sid: b,
            rk: "9".into(),
            ..Default::default()
        },
    ];
    alt_clear(test_state());
    alt_install(test_state(), a, "4", copies.clone());
    assert!(alt_available(test_state(), a, "4"));

    let old_roster = crate::plex::server_roster_gen();
    test_adapter().alt_roster_gen.store(old_roster, Ordering::SeqCst);
    let gen = test_adapter().alt_gen.fetch_add(1, Ordering::SeqCst) + 1;
    *test_adapter().alt_slot.lock().unwrap() = Some(AltResult {
        gen,
        roster_gen: old_roster,
        sid: a,
        rk: "4".into(),
        list: copies,
    });
    crate::plex::revoke_for_profile_switch();
    crate::plex::register_for_test("alt-c", "127.0.0.1", 3, "c", "cid");

    pump_alt_sources(test_state(), test_adapter());
    assert!(
        !alt_available(test_state(), a, "4"),
        "the removed source neither stays cached nor re-lands"
    );

    alt_clear(test_state());
    crate::plex::reset_servers_for_test();
}

/// The whole detail mailbox in one serial test — the statics are global, so splitting this
/// into parallel #[test]s would have them racing each other rather than the code.
#[test]
fn a_detail_landing_only_installs_while_it_is_still_the_one_being_awaited() {
    let _serial = crate::testlock::serial();
    // Other serialized tests may leave a request pending; serialization is not a reset.
    // Reproduce that predecessor deterministically rather than depend on suite ordering.
    let previous = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "previous-test-request");
    clear(test_state(), test_adapter());
    // This synthetic worker is now finished; cancellation alone cannot release it.
    land_detail(test_adapter(), crate::plex::ServerId::UNSET, "previous-test-request", previous, None);
    // idle: nothing requested, nothing loading, nothing to pump
    assert!(!detail_loading(test_adapter()), "the isolated fixture is not loading anything");
    assert!(!pump_detail(test_state(), test_adapter()), "an empty mailbox pumps nothing");

    // a request is in flight until its landing is pumped
    let gen = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "movie-1");
    assert!(
        detail_loading(test_adapter()),
        "a bumped generation with DONE behind it reads as in flight"
    );
    landing(gen, "movie-1");
    assert!(pump_detail(test_state(), test_adapter()), "the awaited landing installs");
    assert_eq!(cur_rk().as_deref(), Some("movie-1"));
    assert!(!detail_loading(test_adapter()), "pumping the landing settles the spinner");

    // SUPERSEDED: a second request means the first one's landing is stale and must be dropped
    let old = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "stale-show");
    let new = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "fresh-show");
    landing(old, "stale-show");
    assert!(
        !pump_detail(test_state(), test_adapter()),
        "a landing from a superseded generation is discarded"
    );
    assert_eq!(
        cur_rk().as_deref(),
        Some("movie-1"),
        "and it must not touch CURRENT"
    );
    assert!(detail_loading(test_adapter()), "the NEWER request is still in flight");

    // MONOTONE mailbox: with the newer result already sitting unconsumed, the OLDER fetch
    // finally returns — it must not overwrite it. (This is the case that wedged the season
    // mailbox before its guard existed: losing the newest result stalled the spinner on.)
    landing(new, "fresh-show");
    landing(old, "stale-show");
    assert!(pump_detail(test_state(), test_adapter()), "the late older landing is refused by its generation");
    assert_eq!(cur_rk().as_deref(), Some("fresh-show"));

    // a FAILED fetch (None) settles the spinner but keeps the previously loaded item
    let g = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "fresh-show");
    land_detail(test_adapter(), crate::plex::ServerId::UNSET, "fresh-show", g, None);
    assert!(!pump_detail(test_state(), test_adapter()), "a failed fetch reports no fresh item");
    assert_eq!(
        cur_rk().as_deref(),
        Some("fresh-show"),
        "and leaves the page as it was"
    );
    assert!(!detail_loading(test_adapter()), "but it does settle the spinner");

    // CLOSING THE PAGE supersedes: a load requested on the way in must not repopulate
    // CURRENT behind whatever screen is mounted now.
    let inflight = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "arrived-after-close");
    clear(test_state(), test_adapter());
    assert!(!detail_loading(test_adapter()), "clear(test_state(), test_adapter()) settles the in-flight fetch");
    landing(inflight, "arrived-after-close");
    assert!(!pump_detail(test_state(), test_adapter()), "a landing after close is dropped");
    assert_eq!(cur_rk(), None, "the page stays closed");
}

/// Spec §5.2 / `docs/stores-as-machines.md` §2.5: the landing is keyed on `(server, rk)`.
/// The page awaits server A's item 7; server B's item 7 landing under the same generation is
/// skipped and counted, and A's own installs. Until phase 4 the mailbox carried no server at
/// all, which is the gap the spec's evidence line names.
#[test]
fn a_detail_landing_for_another_servers_item_of_the_same_key_is_skipped() {
    let _serial = crate::testlock::serial();
    clear(test_state(), test_adapter());
    let a = crate::plex::ServerId::from_raw(0);
    let b = crate::plex::ServerId::from_raw(1);
    let gen = begin_detail_for_test(test_adapter(), a, "7");
    let addr = detail_addr(gen);
    assert!(detail_loading(test_adapter()));
    let before = test_adapter().detail_landing.dropped_for(addr.to);
    land_detail(test_adapter(), 
        b,
        "7",
        gen,
        Some(Detail {
            rk: "7".into(),
            title: "theirs".into(),
            ..Default::default()
        }),
    );
    assert!(!pump_detail(test_state(), test_adapter()), "the other server's copy is not the awaited item");
    assert_eq!(cur_rk(), None);
    assert!(detail_loading(test_adapter()), "…and the page is still waiting for its own");
    assert_eq!(test_adapter().detail_landing.dropped_for(addr.to), before + 1, "counted");
    assert_eq!(test_adapter().detail_landing.inflight(addr.to), 0, "wrong key is a discarded terminal");
    let gen = begin_detail_for_test(test_adapter(), a, "7");
    land_detail(test_adapter(), 
        a,
        "7",
        gen,
        Some(Detail {
            rk: "7".into(),
            title: "ours".into(),
            ..Default::default()
        }),
    );
    assert!(pump_detail(test_state(), test_adapter()));
    assert_eq!(current(test_state()).map(|d| d.title.clone()).as_deref(), Some("ours"));
    assert!(!detail_loading(test_adapter()));
    clear(test_state(), test_adapter());
}

/// Spec §15.1 `a_refused_spawn_lands_a_refusal_event`, on the real store: a request whose
/// worker the OS refused is answered by a `Refused` record, and the pump settles the spinner
/// off it — exactly one event for the request, nothing latched.
#[test]
fn a_refused_detail_spawn_settles_the_spinner_through_the_landing() {
    let _serial = crate::testlock::serial();
    clear(test_state(), test_adapter());
    let a = crate::plex::ServerId::from_raw(0);
    request_detail_with_spawn(test_adapter(), a, "9", |_| false);
    let addr = detail_addr(test_adapter().detail_gen.load(Ordering::SeqCst));
    assert!(detail_loading(test_adapter()));
    assert_eq!(test_adapter().detail_landing.inflight(addr.to), 1, "OS refusal reserves its queued terminal");
    assert!(!pump_detail(test_state(), test_adapter()), "no item arrived");
    assert!(!detail_loading(test_adapter()), "but the refusal settled the wait");
    assert_eq!(test_adapter().detail_landing.inflight(addr.to), 0);
    clear(test_state(), test_adapter());
}

#[test]
fn rapid_detail_supersedes_bound_spawns_and_settle_capacity_refusal() {
    let _serial = crate::testlock::serial();
    clear(test_state(), test_adapter());
    let sid = crate::plex::ServerId::UNSET;
    let mut workers = Vec::new();
    for _ in 0..4 {
        request_detail_with_spawn(test_adapter(), sid, "old", |gen| { workers.push(gen); true });
        assert!(detail_loading(test_adapter()));
    }
    assert_eq!(workers.len(), 4);
    request_detail_with_spawn(test_adapter(), sid, "latest-refused", |_| panic!("refused admission must not spawn"));
    assert!(!detail_loading(test_adapter()), "capacity refusal settles synchronously");
    assert_eq!(detail_request_status(test_adapter(), sid, "latest-refused"), Some(false));
    assert_eq!(test_adapter().detail_landing.inflight(detail_addr(0).to), 4);
    assert!(test_adapter().detail_landing.is_empty(), "no queued capacity refusal");
    for gen in workers {
        landing(gen, "old");
        assert!(!pump_detail(test_state(), test_adapter()));
        assert!(current(test_state()).is_none(), "old completion must not install");
        assert!(!detail_loading(test_adapter()));
    }
    assert_eq!(test_adapter().detail_landing.inflight(detail_addr(0).to), 0);
    let mut next = None;
    request_detail_with_spawn(test_adapter(), sid, "fresh", |gen| { next = Some(gen); true });
    assert!(detail_loading(test_adapter()));
    landing(next.unwrap(), "fresh");
    assert!(pump_detail(test_state(), test_adapter()));
    assert_eq!(cur_rk().as_deref(), Some("fresh"));
    assert!(!detail_loading(test_adapter()));
    clear(test_state(), test_adapter());
}

#[test]
fn a_panicking_detail_fetch_acknowledges_and_settles_its_request() {
    let _serial = crate::testlock::serial();
    clear(test_state(), test_adapter());
    let sid = crate::plex::ServerId::UNSET;
    request_detail_with_spawn(test_adapter(), sid, "panic", |gen| {
        finish_detail_fetch(test_adapter(), sid, "panic", gen, || panic!("synthetic fetch panic"));
        true
    });
    assert!(detail_loading(test_adapter()), "terminal waits for pump");
    assert!(!pump_detail(test_state(), test_adapter()));
    assert!(!detail_loading(test_adapter()));
    assert_eq!(test_adapter().detail_landing.inflight(detail_addr(0).to), 0);
    clear(test_state(), test_adapter());
}

#[test]
fn controlled_cancelled_detail_ack_is_recorded_and_recovers_capacity() {
    let _serial = crate::testlock::serial();
    let mut value = serde_json::to_value(
        crate::app::bootstrap::Initial::synthetic_home(1, 32498, None).unwrap()).unwrap();
    value["content"] = serde_json::json!({"detail":"1001", "detailsec":1,
        "detailok":true, "filmography":true, "personcredits":9, "nowan":true});
    for trigger in ["detail", "detailsec", "detailok", "filmography", "personcredits", "nowan"] {
        value["triggers"].as_array_mut().unwrap()
            .push(serde_json::json!(format!("plxnative-{trigger}")));
    }
    let initial = crate::app::bootstrap::Initial::from_value(value).unwrap();
    crate::app::bootstrap::stores::init(&initial, false);
    crate::ui::landgate::arm_recording();
    test_adapter().detail_gen.store(0, Ordering::SeqCst);
    test_adapter().detail_done.store(0, Ordering::SeqCst);
    clear(test_state(), test_adapter());

    let mut recorded = Vec::new();
    for n in 0..6 {
        crate::app::bootstrap::stores::begin(Default::default(), Default::default());
        let gen = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, &format!("old-{n}"));
        clear(test_state(), test_adapter());
        land_detail(test_adapter(), crate::plex::ServerId::UNSET, &format!("old-{n}"), gen, None);
        assert!(!pump_detail(test_state(), test_adapter()));
        let results = crate::app::bootstrap::stores::take_results();
        assert_eq!(results.len(), 1,
            "the cancelled completion remains a recorded capacity-retiring observation");
        recorded.push(results[0].clone());
        assert_eq!(crate::ui::landgate::take_frame_lands(),
            vec![(crate::stores::StoreId::Metadata.ord(), 1)],
            "a filtered ACK retains its original observed landing frame");
        assert_eq!(crate::app::bootstrap::stores::finish().1, None);
        assert_eq!(test_adapter().detail_landing.inflight(detail_addr(gen).to), 0);
    }

    crate::app::bootstrap::stores::begin(Default::default(), Default::default());
    let wrong = begin_detail_for_test(test_adapter(), crate::plex::ServerId::from_raw(0), "same-key");
    land_detail(test_adapter(), crate::plex::ServerId::from_raw(1), "same-key", wrong, None);
    assert!(!pump_detail(test_state(), test_adapter()), "a wrong-server answer stays filtered");
    let wrong_result = crate::app::bootstrap::stores::take_results().pop().unwrap();
    assert_eq!(test_adapter().detail_landing.inflight(detail_addr(wrong).to), 0);
    assert_eq!(crate::ui::landgate::take_frame_lands(),
        vec![(crate::stores::StoreId::Metadata.ord(), 1)]);
    assert_eq!(crate::app::bootstrap::stores::finish().1, None);

    crate::app::bootstrap::stores::begin(Default::default(), Default::default());
    let fresh = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "fresh-after-cancel");
    land_detail(test_adapter(), crate::plex::ServerId::UNSET, "fresh-after-cancel", fresh,
        Some(Detail { sid:crate::plex::ServerId::UNSET, rk:"fresh-after-cancel".into(),
            ..Default::default() }));
    assert!(pump_detail(test_state(), test_adapter()), "more than the four-slot cap can run after cancelled ACKs retire");
    let fresh_result = crate::app::bootstrap::stores::take_results().pop().unwrap();
    assert_eq!(crate::ui::landgate::take_frame_lands(),
        vec![(crate::stores::StoreId::Metadata.ord(), 1)]);
    assert_eq!(crate::app::bootstrap::stores::finish().1, None);
    clear(test_state(), test_adapter());
    crate::ui::landgate::disarm();

    crate::app::bootstrap::stores::init(&initial, true);
    test_adapter().detail_gen.store(0, Ordering::SeqCst);
    test_adapter().detail_done.store(0, Ordering::SeqCst);
    clear(test_state(), test_adapter());
    for (n, result) in recorded.into_iter().enumerate() {
        crate::app::bootstrap::stores::begin(Default::default(), [result.clone()].into());
        let gen = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, &format!("old-{n}"));
        clear(test_state(), test_adapter());
        assert!(!pump_detail(test_state(), test_adapter()));
        assert_eq!(crate::app::bootstrap::stores::take_results(), vec![result],
            "replay grades the ACK it actually applied");
        assert_eq!(crate::app::bootstrap::stores::finish().1, None);
        assert_eq!(test_adapter().detail_landing.inflight(detail_addr(gen).to), 0,
            "the replayed cancelled ACK retires its reservation");
    }
    crate::app::bootstrap::stores::begin(Default::default(), [wrong_result.clone()].into());
    let wrong = begin_detail_for_test(test_adapter(), crate::plex::ServerId::from_raw(0), "same-key");
    assert!(!pump_detail(test_state(), test_adapter()), "replay preserves the wrong-server filter");
    assert_eq!(crate::app::bootstrap::stores::take_results(), vec![wrong_result]);
    assert_eq!(test_adapter().detail_landing.inflight(detail_addr(wrong).to), 0);
    assert_eq!(crate::app::bootstrap::stores::finish().1, None);
    crate::app::bootstrap::stores::begin(Default::default(), [fresh_result].into());
    let fresh = begin_detail_for_test(test_adapter(), crate::plex::ServerId::UNSET, "fresh-after-cancel");
    assert!(pump_detail(test_state(), test_adapter()), "replay also admits beyond the recovered four-slot cap");
    assert_eq!(test_adapter().detail_landing.inflight(detail_addr(fresh).to), 0);
    assert_eq!(crate::app::bootstrap::stores::finish().1, None);
    clear(test_state(), test_adapter());
    crate::app::bootstrap::stores::reset_for_test();
}
