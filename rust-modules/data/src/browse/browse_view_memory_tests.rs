//! A library's view survives a restart: its sort (GitHub #278: *"sort order should persist between
//! restarts. I have to change it from the default (title) every time I fire it up."*), and — #441,
//! the same complaint about everything else the Library toolbar changes — its Unwatched switch,
//! genre and listing type.
//!
//! Graded end to end against a loopback PMS: the choice goes through the Library's own commit
//! (`QueryEdit::*`), reaches the session file, and a FRESH store — a cold session cache read back
//! from disk, a new `BrowseState`, a section whose menu nobody has seen yet this run — must open
//! that library the way it was left, asking the server only what it must to be sure the saved
//! sort and genre still exist.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_plex::plex::session::{LibraryView, LibraryViews};

/// A view that is only a sort, the shape a pre-#441 record had.
fn sort_view(machine: &str, key: i64, sort: &str, desc: bool) -> LibraryView {
    LibraryView {
        machine_id: machine.into(), key, sort: sort.into(), desc, ..Default::default()
    }
}

/// What one scripted PMS exchange saw: the request line it answered.
#[cfg(feature = "devtriggers")]
struct Pms {
    requests: std::sync::mpsc::Receiver<String>,
    sid: ServerId,
}

/// A loopback PMS answering `bodies` in order, one connection each. The listener gives up after
/// a few seconds so a red run fails its assertions instead of hanging the suite on `accept`.
#[cfg(feature = "devtriggers")]
fn loopback_pms(machine: &str, bodies: Vec<&'static str>) -> Pms {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, requests) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        for body in bodies {
            let mut socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            };
            socket.set_nonblocking(false).unwrap();
            let mut request = String::new();
            BufReader::new(&socket).read_line(&mut request).unwrap();
            let _ = tx.send(request);
            let _ = write!(socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    let sid = plx_plex::plex::register_for_test(machine, "127.0.0.1", port, "", "fixture");
    Pms { requests, sid }
}

/// A browse store holding one movie library on `pms`, as discovery would leave it.
#[cfg(feature = "devtriggers")]
fn movies_on(pms: &Pms, machine: &str) -> TestBrowse {
    library_on(pms, machine, "Movies", SecKind::Movie)
}

/// [`movies_on`] for a library of any kind.
#[cfg(feature = "devtriggers")]
fn library_on(pms: &Pms, machine: &str, title: &str, kind: SecKind) -> TestBrowse {
    assert!(plx_plex::plex::set_current(pms.sid));
    let mut source = a_source(machine, "", true);
    source.sid = pms.sid;
    // As a finished discovery leaves it: the roster sync must see this client as the one the
    // table was built against, or it re-runs discovery and that request eats a scripted answer.
    let client = plx_plex::plex::client_for(pms.sid).unwrap();
    source.client_addr = client as *const _ as usize;
    source.token_gen = client.token_gen();
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![source]);
    browse.append_sections(0, vec![(1, title.into(), kind)]);
    browse
}

/// Pump until the section's first page has landed (or a few seconds pass).
#[cfg(feature = "devtriggers")]
fn pump_until_landed(browse: &mut TestBrowse) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    browse.state.want(0, PAGE, None);
    while browse.state.cur_state().is_some_and(|state| state.total < 0)
        && std::time::Instant::now() < deadline {
        let _ = browse.pump();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// The menu a movie section advertises: Title first, and no Plays (PMS never advertises it —
/// the client adds it, `with_plays_sort`).
#[cfg(feature = "devtriggers")]
const MENU_PAGE: &str = r#"{"MediaContainer":{"totalSize":2,"Metadata":[{"ratingKey":"a-title"},{"ratingKey":"b-title"}],"Meta":{"Type":[{"active":true,"Sort":[{"key":"titleSort","title":"Title"},{"key":"addedAt","descKey":"addedAt:desc","title":"Date Added","defaultDirection":"desc"}]}]}}}"#;
#[cfg(feature = "devtriggers")]
const PLAYS_PAGE: &str = r#"{"MediaContainer":{"totalSize":2,"Metadata":[{"ratingKey":"most-played"},{"ratingKey":"least-played"}]}}"#;

/// THE bug (#278): sort by Plays, restart, and the library is back in title order.
#[cfg(feature = "devtriggers")]
#[test]
fn a_chosen_sort_survives_a_restart() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("sort-memory");
    session.watching("u-sorter");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;

    // ---- the first run: the viewer sorts Movies by Plays, most-played first -----------------
    let pms = loopback_pms("sort-machine", vec![MENU_PAGE, PLAYS_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);
    assert_eq!(browse.state.cur_state().unwrap().sorts[0].key, "titleSort");
    let target = crate::stores::browse::SectionAddress {
        epoch: browse.state.table_epoch(), sid: pms.sid, section: 1,
    };
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target,
        crate::stores::browse::LibraryWork::Commit {
            select: false, choice: false,
            query: Some(crate::stores::browse::QueryEdit::Sort { key: "viewCount".into(), desc: true }),
        }));
    pump_until_landed(&mut browse);
    let first_run: Vec<String> = (0..2).map(|_| pms.requests.recv().unwrap()).collect();
    assert!(first_run[1].contains("sort=viewCount%3Adesc"), "{}", first_run[1]);
    plx_base::storage_worker::drain_for_test();
    drop(browse);

    // ---- the restart: a cold session cache read from disk, and a brand-new store -------------
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::session::redirect_for_test(Some(session.path()));
    let _ = plx_plex::plex::session::peek();
    plx_base::storage_worker::drain_for_test();
    session.watching("u-sorter");
    let pms = loopback_pms("sort-machine", vec![MENU_PAGE, PLAYS_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);

    let snapshot = browse.state.listing_snapshot();
    let view = snapshot.view();
    assert_eq!(view.sorts()[view.sort_index()].key, "viewCount",
        "the library must reopen in the order the viewer chose, not the default title order");
    assert!(view.sort_desc(), "…and in the direction they chose");
    assert_eq!(view.item(0).map(|item| item.rk.as_str()), Some("most-played"),
        "the grid itself is in that order — the unsorted discovery page is never published");
    let requests: Vec<String> = (0..2).map(|_| pms.requests.recv().unwrap()).collect();
    assert!(requests[0].contains("includeMeta=1") && !requests[0].contains("sort="),
        "the remembered key is never sent before the menu proves it: {}", requests[0]);
    assert!(requests[1].contains("sort=viewCount%3Adesc"), "{}", requests[1]);
}

/// A remembered key the server no longer offers falls back SILENTLY to the default order — one
/// request, the unsorted page published, the menu on its first entry — and is never sent.
#[cfg(feature = "devtriggers")]
#[test]
fn a_remembered_sort_the_menu_no_longer_offers_falls_back_to_the_default() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("sort-memory-gone");
    session.watching("u-sorter");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-sorter", sort_view("sort-machine", 1, "lastViewedAt", true));
        Some(next)
    });
    let pms = loopback_pms("sort-machine", vec![MENU_PAGE, PLAYS_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);

    let snapshot = browse.state.listing_snapshot();
    let view = snapshot.view();
    assert_eq!((view.sort_index(), view.sort_desc()), (0, false));
    assert_eq!(view.item(0).map(|item| item.rk.as_str()), Some("a-title"));
    let first = pms.requests.recv().unwrap();
    assert!(!first.contains("lastViewedAt"), "{first}");
    assert!(pms.requests.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
        "no second request for a key the menu did not offer");
}

/// The view is the PROFILE's: another person on the same television opens the same library in
/// the default order, and a profile switch (a store reset) drops the previous person's memory.
#[test]
fn a_remembered_sort_belongs_to_the_profile_that_chose_it() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("sort-memory-profile");
    session.watching("u-sorter");
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-sorter", LibraryView {
            unwatched: true, genre: "28".into(), ..sort_view("mac-mini", 1, "viewCount", true)
        });
        Some(next)
    });
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    let restore = browse.state.restore_for(0).unwrap();
    assert_eq!(restore.sort, Some(("viewCount".to_string(), true)));
    assert_eq!(restore.genre.as_deref(), Some("28"));
    assert!(browse.state.cur_state().unwrap().unwatched, "Unwatched is applied as the section is made");

    browse.reset();
    assert!(browse.state.view_memory.get("mac-mini", 1).is_none(),
        "a reset forgets the previous profile's views before anything is seeded again");
    session.watching("u-other");
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    assert!(browse.state.restore_for(0).is_none(), "another profile opens it in the default order");
    assert!(!browse.state.cur_state().unwrap().unwatched, "…and with every filter off");
}

/// Choosing the default order back FORGETS the entry rather than recording it.
///
/// A Collections listing's sort is recorded too since #441 — as that listing's own
/// (`listing: "collections"` + `listing_sort`), never as the library's: the library's own sort
/// stays what it was, so it comes back when the viewer returns to the own listing. (Until #441 a
/// non-primary view's sort was deliberately never remembered; that is the intended change.)
#[test]
fn choosing_the_default_order_forgets_the_entry() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("sort-memory-default");
    session.watching("u-sorter");
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    land_page_with_sorts(&mut browse, vec![SortEntry {
        key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false,
    }]);
    let commit = |browse: &mut TestBrowse, key: &str, desc: bool| {
        let target = crate::stores::browse::SectionAddress {
            epoch: browse.state.table_epoch(), sid, section: 1,
        };
        assert!(browse.state.addressed_with_adapter(&browse.adapter, target,
            crate::stores::browse::LibraryWork::Commit {
                select: false, choice: false,
                query: Some(crate::stores::browse::QueryEdit::Sort { key: key.into(), desc }),
            }));
        plx_base::storage_worker::drain_for_test();
    };
    let held = |browse: &TestBrowse| {
        let machine = browse.state.sources()[0].machine_id.clone();
        plx_plex::plex::session::peek().views_for("u-sorter")
            .and_then(|views| views.get(&machine, 1).cloned())
    };
    commit(&mut browse, "viewCount", true);
    assert_eq!(held(&browse).and_then(|view| view.primary_sort().map(|(sort, desc)| (sort.to_string(), desc))),
        Some(("viewCount".to_string(), true)));
    // `set_sort_by_key` resets the query, so the menu is re-landed as a real page would.
    land_page_with_sorts(&mut browse, vec![]);
    commit(&mut browse, "titleSort", false);
    assert_eq!(held(&browse), None, "the default order needs no record");
    assert!(plx_plex::plex::session::peek().library_views.is_empty(),
        "and a profile with nothing remembered leaves no empty record behind");
    // A Collections view's sort is that view's menu, not the library's: recorded as the
    // LISTING's sort, with the library's own sort left alone.
    commit(&mut browse, "viewCount", true);
    land_page_with_sorts(&mut browse, vec![]);
    assert!(browse.state.set_library_type(LibraryType::Collections));
    land_page_with_sorts(&mut browse, vec![SortEntry {
        key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false,
    }]);
    commit(&mut browse, "titleSort", true);
    let view = held(&browse).expect("a Collections sort is remembered");
    assert_eq!(view.listing, "collections");
    assert_eq!(view.listing_sort(), Some(("titleSort", true)));
    assert_eq!(view.primary_sort(), Some(("viewCount", true)), "the library's own sort is untouched");
}

/// The per-profile list is bounded: the most recently changed libraries are kept.
#[test]
fn the_remembered_views_are_capped_most_recent_first() {
    let mut views = LibraryViews::default();
    for key in 0..(LibraryViews::CAP as i64 + 5) {
        views.set(sort_view("m", key, "addedAt", true));
    }
    assert_eq!(views.libs.len(), LibraryViews::CAP);
    assert!(views.get("m", 0).is_none(), "the oldest entries are evicted");
    assert!(views.get("m", LibraryViews::CAP as i64 + 4).is_some());
    // changing an old library refreshes it rather than duplicating it
    views.set(sort_view("m", 5, "titleSort", true));
    assert_eq!(views.libs.len(), LibraryViews::CAP);
    assert_eq!(views.libs.last().map(|lib| lib.key), Some(5));
    // nameless libraries are never recorded
    views.set(sort_view("", 1, "addedAt", true));
    assert!(views.get("", 1).is_none());
    // an oversized key or genre is dropped from the entry, and an entry that is left saying
    // nothing is not stored at all (and forgets the library's earlier one)
    views.set(sort_view("m", 99, &"k".repeat(200), true));
    assert!(views.get("m", 99).is_none());
    views.set(LibraryView { genre: "g".repeat(200), ..sort_view("m", 98, "addedAt", false) });
    assert_eq!(views.get("m", 98).map(|view| view.genre.as_str()), Some(""));
    assert_eq!(views.get("m", 98).map(|view| view.sort.as_str()), Some("addedAt"));
    views.set(LibraryView { listing: "nonsense".into(), ..sort_view("m", 97, "", false) });
    assert!(views.get("m", 97).is_none(), "an unknown listing word is not a view");
    views.set(sort_view("m", 5, "", false));
    assert!(views.get("m", 5).is_none(), "the default view forgets the entry");
}

/// Reopen the session file cold, as a restart does, and put the viewer's profile back.
#[cfg(feature = "devtriggers")]
fn restart(session: &TempPins, user: &str) {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::session::redirect_for_test(Some(session.path()));
    let _ = plx_plex::plex::session::peek();
    plx_base::storage_worker::drain_for_test();
    session.watching(user);
}

/// A saved genre the library no longer has (a tag merged or removed on the server) is NEVER sent:
/// the genre list is read first, the page is asked for without it, and the library opens on
/// everything — no error, and no second page request.
#[cfg(feature = "devtriggers")]
#[test]
fn a_remembered_genre_the_library_no_longer_has_falls_back_to_all() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-genre-gone");
    session.watching("u-viewer");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-viewer", LibraryView {
            machine_id: "sort-machine".into(), key: 1, genre: "99".into(), ..Default::default()
        });
        Some(next)
    });
    let pms = loopback_pms("sort-machine", vec![GENRE_DIR, MENU_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);

    let snapshot = browse.state.listing_snapshot();
    let view = snapshot.view();
    assert!(view.genre().is_none(), "the vanished genre is not applied");
    assert_eq!(view.item(0).map(|item| item.rk.as_str()), Some("a-title"));
    assert!(!view.genres().is_empty(), "the list the check fetched is the Filter menu's");
    let requests: Vec<String> = (0..2).map(|_| pms.requests.recv().unwrap()).collect();
    assert!(requests[0].contains("/library/sections/1/genre"), "{}", requests[0]);
    assert!(requests[1].contains("includeMeta=1") && !requests[1].contains("genre="), "{}", requests[1]);
    assert!(pms.requests.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
        "exactly two requests");
}

/// A genre list that cannot be read must not stop the library from opening: the saved genre is
/// simply not applied (nothing the server has not vouched for is sent), and the genre list stays
/// unmarked so the Filter menu fetches it itself later.
#[cfg(feature = "devtriggers")]
#[test]
fn a_failed_genre_check_opens_the_library_unfiltered() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-genre-failed");
    session.watching("u-viewer");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-viewer", LibraryView {
            machine_id: "sort-machine".into(), key: 1, genre: "28".into(), ..Default::default()
        });
        Some(next)
    });
    let pms = loopback_pms("sort-machine", vec!["this is not json", MENU_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);

    let snapshot = browse.state.listing_snapshot();
    let view = snapshot.view();
    assert!(view.genre().is_none());
    assert_eq!(view.item(0).map(|item| item.rk.as_str()), Some("a-title"),
        "a remembered preference is never the reason a library cannot open");
    assert!(!browse.state.cur_state().unwrap().genres_done,
        "the Filter menu still fetches the list itself");
    let requests: Vec<String> = (0..2).map(|_| pms.requests.recv().unwrap()).collect();
    assert!(!requests[1].contains("genre="), "{}", requests[1]);
}

/// The record `user` holds for (`machine`, library 1) once the queued session writes have landed.
#[cfg(feature = "devtriggers")]
fn stored_view(user: &str, machine: &str) -> Option<LibraryView> {
    plx_base::storage_worker::drain_for_test();
    plx_plex::plex::session::peek().views_for(user).and_then(|views| views.get(machine, 1).cloned())
}

/// Toggle Unwatched on in library 1 of `pms`, through the Library's own commit.
#[cfg(feature = "devtriggers")]
fn switch_unwatched_on(browse: &mut TestBrowse, pms: &Pms) {
    let target = crate::stores::browse::SectionAddress {
        epoch: browse.state.table_epoch(), sid: pms.sid, section: 1,
    };
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target,
        crate::stores::browse::LibraryWork::Commit {
            select: false, choice: false,
            query: Some(crate::stores::browse::QueryEdit::Unwatched(true)),
        }));
}

/// A saved sort whose restore FAILED transiently (the menu came back, the sorted re-ask did not)
/// was never resolved: the library opened in the default order, but the record is still the
/// viewer's. An unrelated edit afterwards must not read that default order as a choice.
#[cfg(feature = "devtriggers")]
#[test]
fn a_sort_whose_restore_failed_survives_an_unrelated_edit() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-sort-unresolved");
    session.watching("u-sorter");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-sorter", sort_view("sort-machine", 1, "viewCount", true));
        Some(next)
    });
    let pms = loopback_pms("sort-machine", vec![MENU_PAGE, "this is not json", PLAYS_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);
    let state = browse.state.cur_state().unwrap();
    assert!(!state.sorts.is_empty() && state.sort_idx == 0,
        "the failed re-ask leaves the discovery page in the default order");

    switch_unwatched_on(&mut browse, &pms);
    let held = stored_view("u-sorter", "sort-machine").expect("the record is still there");
    assert!(held.unwatched);
    assert_eq!(held.primary_sort(), Some(("viewCount", true)),
        "a restore that never happened is not the viewer choosing the default order");
}

/// The same for a saved genre whose existence check could not be read: the library opened
/// unfiltered, and the record keeps the genre through an Unwatched edit.
#[cfg(feature = "devtriggers")]
#[test]
fn a_genre_whose_check_failed_survives_an_unrelated_edit() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-genre-unresolved");
    session.watching("u-viewer");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-viewer", LibraryView {
            machine_id: "sort-machine".into(), key: 1, genre: "28".into(), ..Default::default()
        });
        Some(next)
    });
    let pms = loopback_pms("sort-machine", vec!["this is not json", MENU_PAGE, MENU_PAGE]);
    let mut browse = movies_on(&pms, "sort-machine");
    pump_until_landed(&mut browse);
    assert!(browse.state.cur_state().unwrap().genre.is_none(), "opened unfiltered");

    switch_unwatched_on(&mut browse, &pms);
    let held = stored_view("u-viewer", "sort-machine").expect("the record is still there");
    assert!(held.unwatched);
    assert_eq!(held.genre, "28", "a genre check that never answered is not the viewer clearing it");
}

/// The menu a show library's EPISODES listing advertises: Show order first, then Date Added.
#[cfg(feature = "devtriggers")]
const EPISODE_MENU_PAGE: &str = r#"{"MediaContainer":{"totalSize":2,"Metadata":[{"ratingKey":"ep-by-title"},{"ratingKey":"ep-2"}],"Meta":{"Type":[{"active":true,"Sort":[{"key":"show.titleSort,season.index,episode.index","title":"Show"},{"key":"addedAt","descKey":"addedAt:desc","title":"Date Added","defaultDirection":"desc"}]}]}}}"#;
#[cfg(feature = "devtriggers")]
const SORTED_PAGE: &str = r#"{"MediaContainer":{"totalSize":2,"Metadata":[{"ratingKey":"newest-episode"},{"ratingKey":"older-episode"}]}}"#;

/// The listing type is remembered with its OWN sort: Episodes, newest first, reopens as such —
/// `type=4` on the very first request, the remembered sort on the re-ask the menu allows, and no
/// separate "confirm the menu's first entry" re-ask before it.
#[cfg(feature = "devtriggers")]
#[test]
fn a_remembered_listing_type_and_its_sort_survive_a_restart() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-listing");
    session.watching("u-viewer");
    plx_plex::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_view_for("u-viewer", LibraryView {
            machine_id: "sort-machine".into(), key: 1, listing: "episodes".into(),
            listing_sort: "addedAt".into(), listing_desc: true, ..Default::default()
        });
        Some(next)
    });
    let pms = loopback_pms("sort-machine", vec![EPISODE_MENU_PAGE, SORTED_PAGE]);
    let mut browse = library_on(&pms, "sort-machine", "TV Shows", SecKind::Show);
    pump_until_landed(&mut browse);

    let snapshot = browse.state.listing_snapshot();
    let view = snapshot.view();
    assert_eq!(view.library_type(), LibraryType::Episodes);
    assert_eq!(view.sorts()[view.sort_index()].key, "addedAt");
    assert!(view.sort_desc());
    assert_eq!(view.item(0).map(|item| item.rk.as_str()), Some("newest-episode"));
    let requests: Vec<String> = (0..2).map(|_| pms.requests.recv().unwrap()).collect();
    assert!(requests[0].contains("type=4") && requests[0].contains("includeMeta=1")
        && !requests[0].contains("sort="), "{}", requests[0]);
    assert!(requests[1].contains("type=4") && requests[1].contains("sort=addedAt%3Adesc"),
        "{}", requests[1]);
    assert!(pms.requests.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
        "the remembered sort IS the confirming re-ask: no third request");
}

/// A saved listing the section does not offer (an old record, or another kind's) opens the
/// library's own listing; one it does offer is applied as the section is created, with Unwatched.
#[test]
fn a_saved_listing_type_the_section_does_not_offer_falls_back_to_the_main_listing() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-listing-offer");
    session.watching("u-viewer");
    plx_plex::plex::session::update(|current| {
        let mut next = current.clone();
        for key in [1, 2] {
            next.set_view_for("u-viewer", LibraryView {
                machine_id: "mac-mini".into(), key,
                listing: if key == 1 { "episodes" } else { "seasons" }.into(),
                unwatched: true, ..Default::default()
            });
        }
        Some(next)
    });
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(0, vec![
        (1, "Movies".into(), SecKind::Movie),
        (2, "TV Shows".into(), SecKind::Show),
    ]);
    let movies = browse.state.cur_state().unwrap().library_type;
    assert_eq!(movies, LibraryType::Primary, "a movie library has no Episodes listing");
    assert_eq!(browse.state.states[1].library_type, LibraryType::Seasons);
    assert!(browse.state.states.iter().all(|state| state.unwatched), "Unwatched needs no server");
}

/// The section's `/genre` value list, as PMS answers it.
#[cfg(feature = "devtriggers")]
const GENRE_DIR: &str = r#"{"MediaContainer":{"Directory":[{"key":"12","title":"Adventure"},{"key":"28","title":"Action"}]}}"#;
/// The first page of a filtered listing: the discovery request's own answer, menu included.
#[cfg(feature = "devtriggers")]
const FILTERED_PAGE: &str = r#"{"MediaContainer":{"totalSize":1,"Metadata":[{"ratingKey":"filtered-hit"}],"Meta":{"Type":[{"active":true,"Sort":[{"key":"titleSort","title":"Title"},{"key":"addedAt","descKey":"addedAt:desc","title":"Date Added","defaultDirection":"desc"}]}]}}}"#;

/// THE bug (#441): switch Unwatched on and pick a genre, restart, and the library is back to
/// showing everything. The same record that keeps the sort keeps them; the saved genre is checked
/// against the library's genre list before it is ever sent.
#[cfg(feature = "devtriggers")]
#[test]
fn unwatched_and_genre_survive_a_restart() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("view-memory-filters");
    session.watching("u-viewer");

    // ---- the first run: the viewer turns Unwatched on and picks Action ----------------------
    let (_cleanup, mut browse, sid, _) = registered_page_source();
    land_page_with_sorts(&mut browse, vec![SortEntry {
        key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false,
    }]);
    seed_query_choices_for_owner_test(&mut browse.state, vec![SortEntry {
        key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false,
    }], vec![GenreEntry { id: "28".into(), title: "Action".into() }]);
    let target = crate::stores::browse::SectionAddress {
        epoch: browse.state.table_epoch(), sid, section: 1,
    };
    for edit in [
        crate::stores::browse::QueryEdit::Unwatched(true),
        crate::stores::browse::QueryEdit::Genre(Some("28".into())),
    ] {
        assert!(browse.state.addressed_with_adapter(&browse.adapter, target,
            crate::stores::browse::LibraryWork::Commit {
                select: false, choice: false, query: Some(edit),
            }));
    }
    plx_base::storage_worker::drain_for_test();
    drop(browse);

    // ---- the restart: a cold session cache read from disk, and a brand-new store -------------
    restart(&session, "u-viewer");
    // (`registered_page_source`'s server is `browse-life`; the record is keyed by that machine.)
    let pms = loopback_pms("browse-life", vec![GENRE_DIR, FILTERED_PAGE]);
    let mut browse = movies_on(&pms, "browse-life");
    pump_until_landed(&mut browse);

    let snapshot = browse.state.listing_snapshot();
    let view = snapshot.view();
    assert!(view.unwatched(), "Unwatched must still be on");
    assert_eq!(view.genre().map(|genre| genre.id.as_str()), Some("28"));
    assert_eq!(view.item(0).map(|item| item.rk.as_str()), Some("filtered-hit"),
        "the grid is the filtered listing — the unfiltered page is never published");
    assert!(!view.genres().is_empty(), "the genre list the check fetched feeds the Filter menu");
    let requests: Vec<String> = (0..2).map(|_| pms.requests.recv().unwrap()).collect();
    assert!(requests[0].contains("/library/sections/1/genre"),
        "the saved genre is checked against the library's genres first: {}", requests[0]);
    assert!(requests[1].contains("includeMeta=1") && requests[1].contains("unwatched=1")
        && requests[1].contains("genre=28") && !requests[1].contains("sort="), "{}", requests[1]);
    assert!(pms.requests.recv_timeout(std::time::Duration::from_millis(200)).is_err(),
        "no third request");
}
