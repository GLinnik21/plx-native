//! A library's sort survives a restart (GitHub #278): *"sort order should persist between
//! restarts. I have to change it from the default (title) every time I fire it up."*
//!
//! Graded end to end against a loopback PMS: the choice goes through the Library's own commit
//! (`QueryEdit::Sort`), reaches the session file, and a FRESH store — a cold session cache read
//! back from disk, a new `BrowseState`, a section whose menu nobody has seen yet this run — must
//! open that library in the remembered order.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

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
    let sid = crate::plex::register_for_test(machine, "127.0.0.1", port, "", "fixture");
    Pms { requests, sid }
}

/// A browse store holding one movie library on `pms`, as discovery would leave it.
#[cfg(feature = "devtriggers")]
fn movies_on(pms: &Pms, machine: &str) -> TestBrowse {
    assert!(crate::plex::set_current(pms.sid));
    let mut source = a_source(machine, "", true);
    source.sid = pms.sid;
    // As a finished discovery leaves it: the roster sync must see this client as the one the
    // table was built against, or it re-runs discovery and that request eats a scripted answer.
    let client = crate::plex::client_for(pms.sid).unwrap();
    source.client_addr = client as *const _ as usize;
    source.token_gen = client.token_gen();
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![source]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    browse
}

/// Pump until the section's first page has landed (or a few seconds pass).
#[cfg(feature = "devtriggers")]
fn pump_until_landed(browse: &mut TestBrowse) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    browse.state.want(0, PAGE);
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
    crate::plex::reset_servers_for_test();
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
    crate::plex::reset_servers_for_test();
    crate::plex::session::redirect_for_test(Some(session.path()));
    let _ = crate::plex::session::peek();
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
    crate::plex::reset_servers_for_test();
    let _cleanup = RegisteredCleanup;
    crate::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_sort_for("u-sorter", "sort-machine", 1, Some(("lastViewedAt", true)));
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

/// The sort is the PROFILE's: another person on the same television opens the same library in
/// the default order, and a profile switch (a store reset) drops the previous person's memory.
#[test]
fn a_remembered_sort_belongs_to_the_profile_that_chose_it() {
    let _g = plx_base::testlock::serial();
    let session = TempPins::new("sort-memory-profile");
    session.watching("u-sorter");
    crate::plex::session::update(|current| {
        let mut next = current.clone();
        next.set_sort_for("u-sorter", "mac-mini", 1, Some(("viewCount", true)));
        Some(next)
    });
    let mut browse = TestBrowse::default();
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    assert_eq!(browse.state.restore_for(0).map(|r| (r.sort, r.desc)),
        Some(("viewCount".to_string(), true)));

    browse.reset();
    assert!(browse.state.restore_for(0).is_none(), "a reset forgets the previous profile's sorts");
    session.watching("u-other");
    browse.seed_sources(vec![a_source("mac-mini", "", true)]);
    browse.append_sections(0, vec![(1, "Movies".into(), SecKind::Movie)]);
    assert!(browse.state.restore_for(0).is_none(), "another profile opens it in the default order");
}

/// Choosing the default order back FORGETS the entry rather than recording it, and a non-primary
/// view (Episodes of a show library) is never remembered.
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
        crate::plex::session::peek().sorts_for("u-sorter")
            .and_then(|sorts| sorts.get(&machine, 1).map(|(sort, desc)| (sort.to_string(), desc)))
    };
    commit(&mut browse, "viewCount", true);
    assert_eq!(held(&browse), Some(("viewCount".to_string(), true)));
    // `set_sort_by_key` resets the query, so the menu is re-landed as a real page would.
    land_page_with_sorts(&mut browse, vec![]);
    commit(&mut browse, "titleSort", false);
    assert_eq!(held(&browse), None, "the default order needs no record");
    assert!(crate::plex::session::peek().library_sorts.is_empty(),
        "and a profile with nothing remembered leaves no empty record behind");
    // A Collections view's sort is that view's menu, not the library's: never remembered.
    assert!(browse.state.set_library_type(LibraryType::Collections));
    land_page_with_sorts(&mut browse, vec![SortEntry {
        key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false,
    }]);
    commit(&mut browse, "titleSort", true);
    assert_eq!(held(&browse), None);
}

/// The per-profile list is bounded: the most recently sorted libraries are kept.
#[test]
fn the_remembered_sorts_are_capped_most_recent_first() {
    use crate::plex::session::LibrarySorts;
    let mut sorts = LibrarySorts::default();
    for key in 0..(LibrarySorts::CAP as i64 + 5) {
        sorts.set("m", key, Some(("addedAt", true)));
    }
    assert_eq!(sorts.libs.len(), LibrarySorts::CAP);
    assert!(sorts.get("m", 0).is_none(), "the oldest entries are evicted");
    assert!(sorts.get("m", LibrarySorts::CAP as i64 + 4).is_some());
    // re-sorting an old library refreshes it rather than duplicating it
    sorts.set("m", 5, Some(("titleSort", true)));
    assert_eq!(sorts.libs.len(), LibrarySorts::CAP);
    assert_eq!(sorts.libs.last().map(|lib| lib.key), Some(5));
    // nameless libraries and oversized keys are never recorded
    sorts.set("", 1, Some(("addedAt", true)));
    assert!(sorts.get("", 1).is_none());
    sorts.set("m", 99, Some((&"k".repeat(200), true)));
    assert!(sorts.get("m", 99).is_none());
}
