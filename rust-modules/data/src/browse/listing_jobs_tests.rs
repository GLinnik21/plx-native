//! The guard that stays in the body of a listing read: a read that panics lands as the failure its
//! kind already had, and the landing outside the guard is the store's own. And the settle of a read
//! the runner refused: the next step finds the claim back and the refusal's record, consumes it, and
//! asks again with the registry's key as of THAT step.

use super::*;
use crate::browse::test_support::*;
use crate::browse::SecFetch;
use crate::stores::browse::{LibraryWork, SectionAddress};
use crate::stores::StoreJob;
use plx_plex::plex::{ClientKey, ClientKeys};

/// The page job the steps have left, taken off the outbox.
#[track_caller]
fn the_page_job(browse: &mut TestBrowse) -> (plx_machine::job::Job<BrowsePageReq>, BrowsePagePort) {
    // the pump's discovery step may have asked for its own read too: it is not this test's subject
    browse.refuse_discovery_jobs();
    let mut jobs = std::mem::take(&mut browse.jobs);
    assert_eq!(jobs.len(), 1, "exactly one job is on the outbox: {jobs:?}");
    match jobs.remove(0) {
        StoreJob::BrowsePage(job, port) => (job, port),
        other => panic!("a page job, not {other:?}"),
    }
}

/// A page body that panics answers a FAILED page (`total < 0`, never an empty one): the landing
/// marks the listing failed and arms the retry countdown, exactly as a read that failed does, and
/// the claim the step raised is lowered by the take.
#[test]
fn a_panicking_page_body_lands_as_a_failed_page_and_arms_the_retry() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = loopback_page_source();
    let _ = browse.pump();
    let (job, port) = the_page_job(&mut browse);
    assert!(browse.adapter.page.busy(), "premise: the step raised the page claim");

    finish_page_with(&port, job.addr, client, job.req, |_, _, _, _, _| panic!("the body panicked"));

    assert!(browse.adapter.page.has_mail(), "the answer was posted, outside the guard");
    let outcome = browse.pump();
    assert!(outcome.changed, "the failed page was landed");
    assert_eq!(browse.state.states()[0].fetch, SecFetch::Failed, "a panic is a failed fetch");
    assert!(browse.state.states()[0].total < 0, "…and the listing keeps no total it did not get");
    assert_eq!(browse.state.retry_cd, 120, "…and the retry countdown is armed");
    assert!(!browse.adapter.page.busy(), "the take lowered the claim");
    assert!(browse.jobs.is_empty(), "no new page is asked for while the countdown runs: {:?}", browse.jobs);
}

/// A directory body that panics answers an empty list, as a read that found nothing does: the
/// landing marks the list fetched (the menu is not left waiting) and the claim goes down.
#[test]
fn a_panicking_directory_body_lands_as_an_empty_list() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    browse.refill();
    let target = crate::stores::browse::SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target,
        crate::stores::browse::LibraryWork::Genres, &browse.keys, &mut browse.jobs));
    let (job, port) = match browse.jobs.remove(0) {
        StoreJob::BrowseDirectory(job, port) => (job, port),
        other => panic!("a directory job, not {other:?}"),
    };
    assert!(browse.adapter.genres.busy(), "premise: the step raised the genre claim");

    finish_directory_with(&port, job.addr, client, job.req, || panic!("the body panicked"));

    assert!(browse.adapter.genres.has_mail(), "the answer was posted, outside the guard");
    let _ = browse.pump();
    let state = &browse.state.states()[0];
    assert!(state.genres_done, "an empty list is a landed list");
    assert!(state.genres.is_empty());
    assert!(!browse.adapter.genres.busy(), "the take lowered the claim");
}

/// The directory job the steps have left, taken off the outbox.
#[track_caller]
pub(super) fn the_directory_job(browse: &mut TestBrowse) -> (plx_machine::job::Job<BrowseDirectoryReq>, BrowseDirectoryPort) {
    let mut jobs = std::mem::take(&mut browse.jobs);
    assert_eq!(jobs.len(), 1, "exactly one job is on the outbox: {jobs:?}");
    match jobs.remove(0) {
        StoreJob::BrowseDirectory(job, port) => (job, port),
        other => panic!("a directory job, not {other:?}"),
    }
}

/// The menu's tick for `work` (`Genres` or `Letters`) on the registry as of now.
pub(super) fn tick_directory(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId, work: LibraryWork) {
    browse.refill();
    let target = SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target, work, &browse.keys, &mut browse.jobs));
}

/// The tick on a registry that holds no client, as the step the refused job's settle runs in.
pub(super) fn tick_directory_without_keys(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId, work: LibraryWork)
    -> Vec<StoreJob> {
    let target = SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    let mut jobs = Vec::new();
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target, work, &ClientKeys::default(), &mut jobs));
    jobs
}

/// Is the refusal of directory request `id` still on the adapter, unconsumed? (Consumes it if so.)
pub(super) fn directory_refusal_pending(browse: &TestBrowse, which: Directory, id: u32) -> bool {
    match which {
        Directory::Genres => browse.adapter.genres.take_refused(id),
        Directory::Letters => browse.adapter.letters.take_refused(id),
        Directory::Bands => browse.adapter.bands.take_refused(id),
    }
}

/// A page job whose key moved by the time the runner launched it is refused. Nothing is recorded:
/// the store's next step settles it (the claim is down, the refusal's record consumed) and asks
/// nothing while the registry holds no client for the server. The step after, with the client back
/// under a new key, asks again with THAT key.
#[test]
fn a_refused_page_job_is_settled_by_the_next_step_and_the_one_after_asks_again_with_the_new_key() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    let first = ClientKey::of(client);
    browse.maybe_spawn();
    let (job, port) = the_page_job(&mut browse);
    assert_eq!(job.req.server(), (sid, first), "premise: the first read asked with the first key");
    assert!(browse.adapter.page.busy(), "premise: the step raised the page claim");

    port.refused(job.addr); // the runner found the registry's client moved at the launch
    assert!(!browse.adapter.page.busy(), "the refusal gave the claim back in the call that refused");
    assert!(browse.state.states()[0].fetch == SecFetch::Loading,
        "…and nothing is settled until the store's next step");

    // The next step finds the registry holding no client for the server: it settles the refusal and
    // has nothing to ask with.
    let mut jobs = Vec::new();
    browse.state.maybe_spawn_owned(&browse.adapter, &ClientKeys::default(), &mut jobs);
    assert!(jobs.is_empty(), "no client, no read: {jobs:?}");
    assert!(!browse.adapter.page.busy(), "no claim is left up without a job in flight");
    assert!(!browse.adapter.page.take_refused(job.addr.req.0), "the refusal is consumed");

    // The client is back, under a key that is not the first.
    client.set_token("new");
    assert_ne!(ClientKey::of(client), first, "premise: the key moved");
    browse.maybe_spawn();
    let (job, _port) = the_page_job(&mut browse);
    assert_eq!(job.req.server(), (sid, ClientKey::of(client)), "the read asks with the current key");
    assert!(browse.adapter.page.busy(), "and holds the claim");
}

/// A refused page job leaves the listing exactly as a page that never started leaves it: the next
/// step asks for the SAME first missing page, not the one after it.
#[test]
fn a_refused_page_is_asked_for_again_at_the_same_offset() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    browse.maybe_spawn();
    let (job, port) = the_page_job(&mut browse);
    let start = job.req.start;
    port.refused(job.addr);

    browse.maybe_spawn();
    let (again, _port) = the_page_job(&mut browse);
    assert_eq!(again.req.start, start, "the same page is asked for again");
    assert_eq!(again.req.server().0, sid);
    assert_ne!(again.addr, job.addr, "…under a fresh request id");
}

/// A directory job whose key moved by the time the runner launched it is refused, for the genre
/// list and the letter counts alike: the next tick settles it (the claim is down, the list still
/// owed, the record consumed) and asks nothing while the registry holds no client. The tick after,
/// with the client back under a new key, asks again with THAT key.
#[test]
fn a_refused_directory_job_is_settled_by_the_next_step_and_the_one_after_asks_again_with_the_new_key() {
    for (which, work) in [(Directory::Genres, LibraryWork::Genres), (Directory::Letters, LibraryWork::Letters { today: None })] {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, sid, client) = loopback_page_source();
        let first = ClientKey::of(client);
        tick_directory(&mut browse, sid, work.clone());
        let (job, port) = the_directory_job(&mut browse);
        assert_eq!(job.req.server(), (sid, first), "premise: the first read asked with the first key");
        assert!(which.busy(&browse.adapter), "premise: the step raised the claim");

        port.refused(job.addr); // the runner found the registry's client moved at the launch
        assert!(!which.busy(&browse.adapter), "the refusal gave the claim back in the call that refused");

        // The next tick finds no client: it settles the refusal and has nothing to ask with.
        let jobs = tick_directory_without_keys(&mut browse, sid, work.clone());
        assert!(jobs.is_empty(), "no client, no read: {jobs:?}");
        assert!(!which.busy(&browse.adapter), "no claim is left up without a job in flight");
        assert!(!directory_refusal_pending(&browse, which, job.addr.req.0), "the refusal is consumed");
        let state = &browse.state.states()[0];
        assert!(!state.genres_done && state.letters_owed(), "the list is still owed, as after a read that never started");

        client.set_token("new");
        assert_ne!(ClientKey::of(client), first, "premise: the key moved");
        tick_directory(&mut browse, sid, work);
        let (job, _port) = the_directory_job(&mut browse);
        assert_eq!(job.req.server(), (sid, ClientKey::of(client)), "the read asks with the current key");
        assert!(which.busy(&browse.adapter), "and holds the claim");
    }
}

/// The two directory reads claim apart: a genre read out does not hold the letters' tick, and the
/// refusal of one settles only that one.
#[test]
fn a_refused_genre_read_leaves_the_letters_read_in_flight() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    tick_directory(&mut browse, sid, LibraryWork::Genres);
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    let jobs = std::mem::take(&mut browse.jobs);
    assert_eq!(jobs.len(), 2, "one read each: {jobs:?}");
    let StoreJob::BrowseDirectory(genres, port) = &jobs[0] else { panic!("genres first") };
    port.refused(genres.addr);

    assert!(!Directory::Genres.busy(&browse.adapter), "the refused read gave its claim back");
    assert!(Directory::Letters.busy(&browse.adapter), "the other read is still out");
}

// ---- the first-letter list is the grid's own (#869) --------------------------------------------
//
// The A-Z rail jumps by prefix sums of the letter counts, so the counts must describe the very
// listing the grid shows: the same terms, and never a list asked under other ones.

use crate::browse::{GenreEntry, LibraryType, SecItems, SecKind, SortEntry};
use crate::pms::PmsMovie;
use crate::viewstate::{ViewAt, ViewEdit};
use plx_plex::plex::{LibrarySection, MediaContainer};

const WATCHED_FACTS: ViewEdit = ViewEdit::Facts { watched: true, unwatched: false, resume_ms: 0, sort: None };

/// A `firstCharacter` answer of `counts`.
fn letter_container(counts: &[(&str, i64)]) -> MediaContainer {
    let mut container = MediaContainer::default();
    container.directory = counts.iter().map(|(label, size)| LibrarySection {
        key: label.to_lowercase(), title: (*label).into(), size: *size, ..Default::default()
    }).collect();
    container
}

/// Answer the one directory job on the outbox with `counts` through the production completion, then
/// run a step so the answer lands. The page and hub reads that step leaves are not these tests'.
#[track_caller]
fn answer_letters(browse: &mut TestBrowse, client: &'static plx_plex::plex::Client, counts: &[(&str, i64)]) {
    let (job, port) = the_directory_job(browse);
    let container = letter_container(counts);
    finish_directory_with(&port, job.addr, client, job.req, move || Some(container));
    let _ = browse.pump();
    browse.jobs.clear();
}

/// The letters of the current listing as the Library page reads them.
fn shown_letters(browse: &TestBrowse) -> Vec<(String, i64)> {
    browse.state.listing_snapshot().view().letters().to_vec()
}

fn rail_shown(browse: &TestBrowse) -> bool {
    browse.state.listing_snapshot().view().rail_available()
}

/// The menu's tick for the letters, answered with `counts`: the list the section asks for first.
#[track_caller]
fn land_letters(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId,
    client: &'static plx_plex::plex::Client, counts: &[(&str, i64)]) {
    tick_directory(browse, sid, LibraryWork::Letters { today: None });
    answer_letters(browse, client, counts);
}

const LETTERS_BODY: &str = r#"{"MediaContainer":{"Directory":[{"key":"A","title":"A","size":3},{"key":"B","title":"B","size":4}]}}"#;

/// A server on a loopback port that answers every request with letters and sends each request line
/// back. A step also dials the server for other things (the section list), so a test reads the
/// `firstCharacter` lines out of the stream ([`letters_requests`]). The thread ends with the process.
fn letters_server() -> (i32, std::sync::mpsc::Receiver<String>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        while let Ok((mut socket, _)) = listener.accept() {
            let mut line = String::new();
            if BufReader::new(&socket).read_line(&mut line).is_err() {
                continue;
            }
            let _ = tx.send(line);
            let _ = write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{LETTERS_BODY}",
                LETTERS_BODY.len());
        }
    });
    (port, rx)
}

/// The next `count` `firstCharacter` request lines the server saw, other requests skipped.
#[track_caller]
fn letters_requests(requests: &std::sync::mpsc::Receiver<String>, count: usize) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut lines = Vec::new();
    while lines.len() < count {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match requests.recv_timeout(left) {
            Ok(line) if line.contains("/firstCharacter") => lines.push(line),
            Ok(_) => {}
            Err(_) => panic!("{count} letters requests expected, saw {lines:?}"),
        }
    }
    lines
}

/// `(path, terms)` of a recorded request line, the terms without the `X-Plex-` identity.
fn path_and_terms(line: &str) -> (String, String) {
    let target = line.split_whitespace().nth(1).unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let terms: Vec<&str> = query.split('&').filter(|t| !t.is_empty() && !t.starts_with("X-Plex-")).collect();
    (path.to_owned(), terms.join("&"))
}

fn a_genre_menu(browse: &mut TestBrowse) {
    browse.state.state_mut(0).unwrap().genres = Arc::new(vec![GenreEntry { id: "7".into(), title: "Drama".into() }]);
}

/// The letters request carries the SAME terms the grid page sends (`SecState::query_filters`),
/// whatever combination the viewer chose and on whichever listing type, and is asked again when
/// they change after a list has landed.
#[test]
fn the_letters_request_carries_the_terms_the_grid_sends() {
    let scenarios: [(SecKind, LibraryType, bool, Option<&str>, &str); 7] = [
        (SecKind::Movie, LibraryType::Primary, true, None, "unwatched=1"),
        (SecKind::Movie, LibraryType::Primary, false, Some("7"), "genre=7"),
        (SecKind::Movie, LibraryType::Primary, true, Some("7"), "unwatched=1&genre=7"),
        (SecKind::Show, LibraryType::Primary, true, None, "type=2&unwatchedLeaves=1"),
        (SecKind::Show, LibraryType::Primary, true, Some("7"), "type=2&unwatchedLeaves=1&genre=7"),
        (SecKind::Show, LibraryType::Seasons, true, None, "type=3&unwatchedLeaves=1"),
        (SecKind::Show, LibraryType::Episodes, true, None, "type=4&unwatched=1"),
    ];
    for (kind, library_type, unwatched, genre, terms) in scenarios {
        let _g = plx_base::testlock::serial();
        let (port, requests) = letters_server();
        let (_cleanup, mut browse, sid, client) = loopback_source_at(port, kind);
        // The list every section asks for first (answered in place; only the edit's read dials).
        land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
        if library_type != LibraryType::Primary {
            assert!(browse.state.set_library_type(library_type));
        }
        a_genre_menu(&mut browse);
        if unwatched {
            assert!(browse.state.set_unwatched(true));
        }
        if let Some(id) = genre {
            assert!(browse.state.set_genre_by_id(Some(id)));
        }

        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        assert_eq!(browse.jobs.len(), 1, "{kind:?} {library_type:?}: the edit owes the letters again");
        assert_eq!(browse.run_listing_jobs(), 1);

        let lines = letters_requests(&requests, 1);
        let (path, asked) = path_and_terms(&lines[0]);
        assert_eq!(path, "/library/sections/1/firstCharacter");
        assert_eq!(asked, terms, "{kind:?} {library_type:?}");
    }
}

/// An answer asked under other terms than the grid now has is never shown: the viewer turned
/// Unwatched on while the first list was out. The counts of the unfiltered listing would put every
/// jump on the wrong title.
#[test]
fn an_answer_asked_under_older_terms_is_never_shown() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    let (job, port) = the_directory_job(&mut browse);
    assert!(browse.state.set_unwatched(true), "the viewer narrows the listing while the read is out");

    let container = letter_container(&[("A", 3), ("B", 4)]);
    finish_directory_with(&port, job.addr, client, job.req, move || Some(container));
    let _ = browse.pump();
    browse.jobs.clear();
    assert!(shown_letters(&browse).is_empty(), "counts of the unfiltered listing over a filtered grid: {:?}",
        shown_letters(&browse));
    assert!(!rail_shown(&browse));

    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    answer_letters(&mut browse, client, &[("A", 1), ("B", 2)]);
    assert_eq!(shown_letters(&browse), [("A".to_owned(), 1), ("B".to_owned(), 2)]);
    assert!(rail_shown(&browse), "the rail shows under Unwatched once the list is the grid's own");
}

/// The saved genre is applied by the first page's landing, after the letters were asked: the
/// unfiltered list must not stand for the filtered grid.
#[test]
fn a_restored_genre_owes_the_letters_again() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert!(rail_shown(&browse), "premise: the unfiltered list is the grid's own");

    browse.adapter.page.post(PageResult {
        client, token_gen: client.token_gen(), gen: browse.state.query_gen(), sec: 0, start: 0,
        items: Vec::new(), total: 0, sorts: None, restored: None, genres: None,
        genre: Some(GenreEntry { id: "7".into(), title: "Drama".into() }), resolved: Default::default(),
    }, |_| true);
    browse.adapter.page.claim(0);
    let _ = browse.pump();
    browse.jobs.clear();
    assert_eq!(browse.state.listing_snapshot().view().genre().map(|genre| genre.id.as_str()), Some("7"),
        "premise: the page landing applied the saved genre");

    assert!(shown_letters(&browse).is_empty(), "the unfiltered counts must not describe the genre's grid");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "the letters are owed again");
}

/// A read-back that changed what the Unwatched listing holds, and a scan that changed what a
/// section holds, move every row after the change by one: the counts are asked again.
#[test]
fn a_membership_change_owes_the_letters_again() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    assert!(browse.state.set_unwatched(true));
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    {
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 1;
        state.items = SecItems::from_vec(vec![Some(PmsMovie { sid, rk: "7".into(), unwatched: true, ..Default::default() })]);
    }
    assert_eq!(shown_letters(&browse).len(), 2, "premise: the list is the grid's own");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty(), "premise: nothing is owed");

    assert!(browse.state.apply_view_local(&ViewAt::leaf(sid, "7"), WATCHED_FACTS));
    assert!(browse.state.requery_stale(), "the listing is asked again");
    assert!(shown_letters(&browse).is_empty(), "the old counts include the title that left");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    answer_letters(&mut browse, client, &[("A", 2), ("B", 4)]);
    assert_eq!(browse.state.listing_snapshot().view().letter_start(1), 2);
}

/// The same for a read that was already out when the watch happened: it was asked before the
/// change, so its answer is not the list of the grid that follows.
#[test]
fn an_answer_asked_before_a_membership_change_is_not_the_current_list() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    assert!(browse.state.set_unwatched(true));
    {
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 1;
        state.items = SecItems::from_vec(vec![Some(PmsMovie { sid, rk: "7".into(), unwatched: true, ..Default::default() })]);
    }
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    let (job, port) = the_directory_job(&mut browse);
    assert!(browse.state.apply_view_local(&ViewAt::leaf(sid, "7"), WATCHED_FACTS));
    assert!(browse.state.requery_stale());

    let container = letter_container(&[("A", 3), ("B", 4)]);
    finish_directory_with(&port, job.addr, client, job.req, move || Some(container));
    let _ = browse.pump();
    browse.jobs.clear();
    assert!(shown_letters(&browse).is_empty(), "asked before the change: {:?}", shown_letters(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "and asked again");
}

/// A library scan that changed a section the viewer is not looking at: its counts are asked again
/// when it comes back.
#[test]
fn a_scan_owes_the_letters_of_a_section_that_is_not_showing() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    browse.append_sections(0, vec![(2, "Other".into(), SecKind::Movie)]);
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    browse.state.set_cur(1);
    assert!(browse.state.listings_changed(sid, false));
    browse.state.set_cur(0);
    assert!(shown_letters(&browse).is_empty(), "what the section lists moved while it was away");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1);
}

/// Over-invalidation guard (green before and after): a sort edit changes neither the terms nor the
/// membership, so the list stays and nothing is asked.
#[test]
fn a_sort_change_does_not_owe_the_letters() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    browse.state.state_mut(0).unwrap().sorts = Arc::new(vec![
        SortEntry { key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false },
        SortEntry { key: "addedAt".into(), desc_key: String::new(), title: "Date Added".into(), default_desc: true },
    ]);
    for (key, desc) in [("addedAt", true), ("titleSort", true), ("titleSort", false)] {
        assert!(browse.state.set_sort_by_key(key, desc));
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        assert!(browse.jobs.is_empty(), "{key} desc={desc}: nothing is owed: {:?}", browse.jobs);
        assert_eq!(shown_letters(&browse).len(), 2, "{key} desc={desc}: the list stays");
    }
    assert!(rail_shown(&browse));
}

/// The rail follows the sort and the list, not the filters: shown under Unwatched and a genre, hidden
/// for another sort, for a descending title sort and for a single letter.
#[test]
fn the_rail_is_hidden_by_the_sort_and_the_list_and_not_by_the_filters() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    a_genre_menu(&mut browse);
    browse.state.state_mut(0).unwrap().sorts = Arc::new(vec![
        SortEntry { key: "titleSort".into(), desc_key: String::new(), title: "Title".into(), default_desc: false },
        SortEntry { key: "addedAt".into(), desc_key: String::new(), title: "Date Added".into(), default_desc: true },
    ]);
    assert!(browse.state.set_unwatched(true));
    assert!(browse.state.set_genre_by_id(Some("7")));
    land_letters(&mut browse, sid, client, &[("A", 2), ("B", 1)]);
    assert!(rail_shown(&browse), "Unwatched and a genre on, ordered by title");
    assert!(browse.state.set_sort_by_key("addedAt", true));
    assert!(!rail_shown(&browse), "another sort");
    assert!(browse.state.set_sort_by_key("titleSort", true));
    assert!(!rail_shown(&browse), "descending by title");
    assert!(browse.state.set_sort_by_key("titleSort", false));
    assert!(rail_shown(&browse));

    assert!(browse.state.set_unwatched(false));
    assert!(browse.state.set_genre_by_id(None));
    land_letters(&mut browse, sid, client, &[("A", 3)]);
    assert!(!rail_shown(&browse), "a single letter has nothing to jump to");
}

/// The Title entries a PMS 1.43.5 declares for seasons and episodes, as measured (`docs/pms-api.md`,
/// "Count probes for seasons and episodes"): a COMPOSITE key, and a `descKey` that reverses the show alone.
const SEASONS_TITLE: (&str, &str) = ("show.titleSort,index", "show.titleSort:desc,index");
const EPISODES_TITLE: (&str, &str) = (
    "show.titleSort,season.index:nullsLast,episode.index:nullsLast,episode.originallyAvailableAt:nullsLast,episode.titleSort,episode.id",
    "show.titleSort:desc,season.index:nullsLast,episode.index:nullsLast,episode.originallyAvailableAt:nullsLast,episode.titleSort,episode.id",
);

fn declared_sort(key: &str, desc_key: &str, default_desc: bool) -> SortEntry {
    SortEntry { key: key.into(), desc_key: desc_key.into(), title: key.into(), default_desc }
}

/// The `sort=` value the page read of the listing as it stands would send.
#[track_caller]
fn page_sort(browse: &mut TestBrowse) -> String {
    browse.maybe_spawn();
    let (job, port) = the_page_job(browse);
    let sort = job.req.sort.clone();
    // never started: hands the claim back, so the next read of the test can be asked
    plx_machine::job::Port::refused(&port, job.addr);
    sort
}

/// **The letters rail of a TV library's Seasons and Episodes views** ("no ruler"): their Title sort is the
/// server's declared COMPOSITE, not `titleSort`, and the letters sum in that order only, so the rail is
/// offered for that entry ascending, and never for its `descKey` order, a value sort or `random`.
#[test]
fn seasons_and_episodes_offer_the_letters_for_their_declared_title_sort_ascending_only() {
    for (listing, (title, title_desc), others) in declared_menus() {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, sid, client) = loopback_source_at(9, SecKind::Show);
        assert!(browse.state.set_library_type(listing));
        browse.state.state_mut(0).unwrap().sorts = Arc::new(menu_of(title, title_desc, others));
        land_letters(&mut browse, sid, client, &[("C", 3), ("M", 4)]);

        assert!(rail_shown(&browse), "{listing:?}: ordered by its declared Title entry, ascending");
        assert!(browse.state.set_sort_by_key(title, true));
        assert!(!rail_shown(&browse), "{listing:?}: the rail needs the order the counts sum in");
        assert!(browse.state.set_sort_by_key(title, false));
        assert!(rail_shown(&browse), "{listing:?}: ascending again");
        for (key, _) in others {
            assert!(browse.state.set_sort_by_key(key, false));
            assert!(!rail_shown(&browse), "{listing:?}: {key} has no letters (bands, or none)");
        }
    }
}

/// The Title entries and the value sorts a PMS declares for each, as `(listing, (key, descKey), others)`.
fn declared_menus() -> [(LibraryType, (&'static str, &'static str), &'static [(&'static str, &'static str)]); 2] {
    [
        (LibraryType::Seasons, SEASONS_TITLE, &[("random", "random:desc")]),
        (LibraryType::Episodes, EPISODES_TITLE, &[("originallyAvailableAt", "originallyAvailableAt:desc"),
            ("addedAt", "addedAt:desc"), ("random", "random:desc")]),
    ]
}

fn menu_of(title: &str, title_desc: &str, others: &[(&str, &str)]) -> Vec<SortEntry> {
    let mut menu = vec![declared_sort(title, title_desc, false)];
    menu.extend(others.iter().map(|(key, desc_key)| declared_sort(key, desc_key, true)));
    menu
}

/// The page read of a seasons or episodes listing sends the declared Title key ascending and the declared
/// `descKey` descending, never the literal `titleSort` (whose order the letters' prefix sums do not land
/// on) and never `:desc` appended to the composite: the grid is in the order the rail's counts sum in.
#[test]
fn the_page_sends_the_declared_title_keys_for_seasons_and_episodes() {
    for (listing, (title, title_desc), others) in declared_menus() {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, _sid, _client) = loopback_source_at(9, SecKind::Show);
        assert!(browse.state.set_library_type(listing));
        browse.state.state_mut(0).unwrap().sorts = Arc::new(menu_of(title, title_desc, others));
        assert_eq!(page_sort(&mut browse), title, "{listing:?}: the declared key as it stands");
        assert!(browse.state.set_sort_by_key(title, true));
        assert_eq!(page_sort(&mut browse), title_desc, "{listing:?}: the declared descKey");
        assert!(browse.state.set_sort_by_key(title, false));
        assert_eq!(page_sort(&mut browse), title, "{listing:?}: ascending again");
    }
}

/// Before the menu has landed no sort is sent (the server's default order is the declared Title order,
/// measured), so a listing of seasons or episodes offers its letters then too, as the section's own
/// listing always did.
#[test]
fn a_listing_without_its_menu_yet_is_in_the_servers_default_order() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_source_at(9, SecKind::Show);
    assert!(browse.state.set_library_type(LibraryType::Episodes));
    assert_eq!(page_sort(&mut browse), "", "the page sends no sort");
    land_letters(&mut browse, sid, client, &[("C", 3), ("M", 4)]);
    assert!(browse.state.cur_state().unwrap().sorts.is_empty(), "premise: no menu yet");
    assert!(rail_shown(&browse));
}

/// **One predicate, per listing.** The section's own listing and the collections list `titleSort` and
/// nothing else is its Title order; a seasons or episodes listing's is the composite whose first term is
/// exactly `show.titleSort`, and NOT the literal `titleSort`, a `title` or the episode's own
/// `episode.titleSort,...` (the letters count by the SHOW's title and land on none of those orders). A key
/// of another fact never is, on any listing.
#[test]
fn the_title_order_of_a_listing_is_its_own_entry() {
    use LibraryType::{Collections, Episodes, Primary, Seasons};
    let entry = |key: &str| declared_sort(key, "", false);
    for (listing, key, want) in [
        (Primary, "titleSort", true), (Collections, "titleSort", true),
        (Primary, SEASONS_TITLE.0, false), (Primary, EPISODES_TITLE.0, false),
        (Seasons, SEASONS_TITLE.0, true), (Episodes, EPISODES_TITLE.0, true),
        (Seasons, "titleSort", false), (Episodes, "titleSort", false),
        (Seasons, "title", false), (Episodes, "title", false),
        (Seasons, "episode.titleSort,episode.index", false), (Episodes, "episode.titleSort,episode.index", false),
        (Seasons, "show.titleSort:desc,index", false),
        (Seasons, "random", false), (Episodes, "random", false),
        (Seasons, "addedAt", false), (Episodes, "addedAt", false), (Episodes, "originallyAvailableAt", false),
        (Episodes, "rating", false), (Episodes, "lastViewedAt", false),
        (Primary, "addedAt", false), (Primary, "rating", false), (Collections, "addedAt", false),
    ] {
        assert_eq!(entry(key).is_title_order(listing), want, "{listing:?} {key}");
    }
}

/// Navigation round trip through the Collections page, the one place the section's membership can
/// change without the section hearing of it: Unwatched is on, a title is watched while Collections
/// is listed (the player's read-back finds no loaded row of this listing to flag), and on the way
/// back the Unwatched counts from before the watch must not sit over the refetched grid.
#[test]
fn a_watch_while_collections_is_listed_does_not_leave_old_counts_over_the_new_grid() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    assert!(browse.state.set_unwatched(true));
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert_eq!(shown_letters(&browse).len(), 2, "premise: the Unwatched counts are current");

    assert!(browse.state.set_library_type(LibraryType::Collections));
    // What the player's read-back does after a collection member was played: nothing of this
    // listing is loaded, so no row is covered and the section is not flagged.
    browse.state.apply_view_local(&ViewAt::leaf(sid, "7"), WATCHED_FACTS);
    assert!(browse.state.set_library_type(LibraryType::Primary));

    assert!(shown_letters(&browse).is_empty(),
        "the counts from before the watch over the refetched grid: {:?}", shown_letters(&browse));
    assert!(!rail_shown(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    answer_letters(&mut browse, client, &[("A", 2), ("B", 4)]);
    assert_eq!(browse.state.listing_snapshot().view().letter_start(1), 2, "the post-watch counts");
}

/// Even with no watch at all, a listing handed back by the Collections page counts again: the
/// loan holds the view's sort, genre and menus, never the counts.
#[test]
fn returning_from_collections_asks_for_the_letters_again() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert!(browse.state.set_library_type(LibraryType::Collections));
    assert!(browse.state.set_library_type(LibraryType::Primary));
    assert!(shown_letters(&browse).is_empty());
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "one small request, like any fresh listing");
}

/// GUARD (green before and after): a read that SUCCEEDS with no rows (a filter matching nothing)
/// is a landed list. The rail stays hidden and nothing is asked again.
#[test]
fn an_empty_answer_hides_the_rail_without_a_retry_loop() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    assert!(browse.state.set_unwatched(true));
    land_letters(&mut browse, sid, client, &[]);
    assert!(shown_letters(&browse).is_empty());
    assert!(!rail_shown(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty(), "an empty answer is an answer: {:?}", browse.jobs);
    assert!(!Directory::Letters.busy(&browse.adapter), "and no claim is left up");
}

/// The request carries what the answer will be filed under: the letters read, the grid's terms and
/// the key it was asked under; the genre read, the listing's type alone and no key.
#[test]
fn the_request_carries_the_terms_and_the_key_it_is_asked_under() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_source_at(9, SecKind::Show);
    assert!(browse.state.set_library_type(LibraryType::Seasons));
    assert!(browse.state.set_unwatched(true));

    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    let (job, _port) = the_directory_job(&mut browse);
    assert_eq!(job.req.filters, terms(&[("type", "3"), ("unwatchedLeaves", "1")]));
    assert_eq!(job.req.letters, Some(browse.state.states()[0].letters_key()));
    assert_eq!(job.req.letters.as_ref().map(|key| (key.unwatched, key.genre.as_deref())), Some((true, None)));

    tick_directory(&mut browse, sid, LibraryWork::Genres);
    let (job, _port) = the_directory_job(&mut browse);
    assert_eq!(job.req.filters, terms(&[("type", "3")]), "the genre list is scoped by the type alone");
    assert_eq!(job.req.letters, None);
}

fn terms(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
}

/// A read that FAILS (an error, or a body that panicked) leaves the list as it was, hidden because it
/// no longer describes the grid, holds the same key back for the back-off and asks again after it. A
/// different key is never held back, and an answer ends the wait.
#[test]
fn a_failed_letters_read_keeps_the_rail_owed_and_is_retried_after_the_backoff() {
    use crate::browse::letters::LETTERS_RETRY_STEPS;
    for panics in [false, true] {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, sid, client) = loopback_page_source();
        land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
        assert!(browse.state.set_unwatched(true));

        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        let (job, port) = the_directory_job(&mut browse);
        if panics {
            finish_directory_with(&port, job.addr, client, job.req, || panic!("the body panicked"));
        } else {
            finish_directory_with(&port, job.addr, client, job.req, || None);
        }
        let _ = browse.pump();
        browse.jobs.clear();

        let state = &browse.state.states()[0];
        assert_eq!(*state.letters, vec![("A".to_owned(), 3), ("B".to_owned(), 4)], "panics={panics}: untouched");
        assert!(!state.letters_current() && state.letters_waiting(), "panics={panics}");
        assert!(shown_letters(&browse).is_empty(), "never shown over a grid it does not describe");
        assert!(browse.state.pump_needs_work(&browse.adapter), "the countdown must not stall on an idle loop");

        // Held back for the length of the back-off, one step at a time.
        for step in 1..LETTERS_RETRY_STEPS {
            tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
            assert!(browse.jobs.is_empty(), "panics={panics} step {step}: {:?}", browse.jobs);
            let _ = browse.pump();
            browse.jobs.clear();
        }
        let _ = browse.pump();
        browse.jobs.clear();
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        assert_eq!(browse.jobs.len(), 1, "panics={panics}: asked again once the back-off is spent");
        answer_letters(&mut browse, client, &[("A", 1)]);
        assert!(browse.state.states()[0].letters_current());
        assert!(!browse.state.states()[0].letters_waiting());

        // A fresh failure, then an edit: the key changed, so nothing waits.
        assert!(browse.state.set_unwatched(false));
        a_genre_menu(&mut browse);
        assert!(browse.state.set_genre_by_id(Some("7")));
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        let (job, port) = the_directory_job(&mut browse);
        finish_directory_with(&port, job.addr, client, job.req, || None);
        let _ = browse.pump();
        browse.jobs.clear();
        assert!(browse.state.states()[0].letters_waiting());
        assert!(browse.state.set_unwatched(true));
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        assert_eq!(browse.jobs.len(), 1, "panics={panics}: a different key is asked at once");
    }
}

/// Turning a filter on and off again must not cost the rail: the unfiltered list is kept beside the
/// filtered one (`SecState::letters_kept`), and is the grid's own again the moment the filter is
/// off, with no read. Issue #869 review: one slot replaced it and re-asked on every return.
#[test]
fn turning_a_filter_off_brings_the_unfiltered_rail_back_without_a_read() {
    for genre in [true, false] {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, sid, client) = loopback_page_source();
        a_genre_menu(&mut browse);
        let plain: Vec<(String, i64)> = vec![("A".into(), 3), ("B".into(), 4)];
        land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);

        if genre {
            assert!(browse.state.set_genre_by_id(Some("7")));
        } else {
            assert!(browse.state.set_unwatched(true));
        }
        assert!(shown_letters(&browse).is_empty(), "genre={genre}: the plain list does not describe the filtered grid");
        land_letters(&mut browse, sid, client, &[("A", 1)]);
        assert_eq!(shown_letters(&browse), [("A".to_owned(), 1)], "genre={genre}");

        if genre {
            assert!(browse.state.set_genre_by_id(None));
        } else {
            assert!(browse.state.set_unwatched(false));
        }
        assert_eq!(shown_letters(&browse), plain, "genre={genre}: back on the unfiltered grid, at once");
        assert!(rail_shown(&browse), "genre={genre}");
        tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
        assert!(browse.jobs.is_empty(), "genre={genre}: nothing is owed: {:?}", browse.jobs);

        // And the filtered list is still the one beside it: no read to switch the filter on again
        // for a genre (Unwatched is the next test).
        if genre {
            assert!(browse.state.set_genre_by_id(Some("7")));
            assert_eq!(shown_letters(&browse), [("A".to_owned(), 1)]);
        }
    }
}

/// A list counted under Unwatched is never the one kept: whether a title belongs to that listing
/// changes with every watch, so it is the one least likely to be worth showing again (the one
/// that stays in the landed slot is the next test's).
#[test]
fn an_unwatched_list_is_not_kept_across_the_switch() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    assert!(browse.state.set_unwatched(true));
    land_letters(&mut browse, sid, client, &[("A", 2), ("B", 4)]);
    assert!(browse.state.set_unwatched(false));
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert_eq!(shown_letters(&browse), [("A".to_owned(), 3), ("B".to_owned(), 4)]);

    assert!(browse.state.set_unwatched(true));
    assert!(shown_letters(&browse).is_empty(), "the Unwatched counts of an earlier visit: {:?}", shown_letters(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "asked afresh");
}

/// The order the first fix missed (#869 review, finding 1): unfiltered, then Unwatched on and off, so
/// the Unwatched list stays in the landed slot behind the unfiltered one, which is shown again with
/// no read. A title watched NOW, with the switch off, moves no term and no membership generation,
/// yet every Unwatched count after it is one late: the stale list must not come back when Unwatched
/// is turned on again. The unfiltered counts do not depend on viewing and are left alone.
#[test]
fn a_watch_made_while_unwatched_is_off_stales_the_unwatched_list_behind_the_kept_one() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    let plain: Vec<(String, i64)> = vec![("A".into(), 3), ("B".into(), 4)];
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert!(browse.state.set_unwatched(true));
    land_letters(&mut browse, sid, client, &[("A", 2), ("B", 4)]);
    assert!(browse.state.set_unwatched(false));
    assert_eq!(shown_letters(&browse), plain, "premise: the kept unfiltered list is shown again");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty(), "premise: nothing is owed");

    // A title of this server is finished (a read-back, its card in no loaded page). Unwatched is off:
    // the grid on screen is unaffected, and so are the unfiltered counts.
    browse.state.apply_view_local(&ViewAt::leaf(sid, "7"), WATCHED_FACTS);
    assert_eq!(shown_letters(&browse), plain, "the unfiltered counts do not move with a watch");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert!(browse.jobs.is_empty(), "and nothing is asked while Unwatched is off: {:?}", browse.jobs);

    assert!(browse.state.set_unwatched(true));
    assert!(shown_letters(&browse).is_empty(), "the Unwatched counts of before the watch: {:?}", shown_letters(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "asked afresh");
    answer_letters(&mut browse, client, &[("A", 1), ("B", 4)]);
    assert_eq!(shown_letters(&browse), [("A".to_owned(), 1), ("B".to_owned(), 4)]);
}

/// Finding 2: with Unwatched applied, finishing a title whose card is in no loaded page covers no
/// row, so nothing flags the grid, yet the Unwatched counts no longer include it. They are asked
/// again, whether the list had landed or was still out when the read-back came; a title of another
/// server is none of this section's business, and neither is a press (the read-back after it is).
#[test]
fn a_watch_of_a_title_in_no_loaded_page_owes_the_unwatched_letters() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    assert!(browse.state.set_unwatched(true));
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    {
        let state = browse.state.state_mut(0).unwrap();
        state.fetch = SecFetch::Ready;
        state.total = 7;
        state.items = SecItems::from_vec(vec![Some(PmsMovie { sid, rk: "1".into(), unwatched: true, ..Default::default() })]);
    }

    let elsewhere = plx_plex::plex::ServerId::from_raw(sid.raw().wrapping_add(7));
    assert!(!browse.state.apply_view_local(&ViewAt::leaf(elsewhere, "99"), WATCHED_FACTS), "another server's title");
    assert!(!browse.state.apply_view_local(&ViewAt::leaf(sid, "99"), ViewEdit::Watched(true)),
        "a press is not the server's word: the read-back that follows it is");
    assert_eq!(shown_letters(&browse).len(), 2, "the list stands");

    assert!(browse.state.apply_view_local(&ViewAt::leaf(sid, "99"), WATCHED_FACTS), "the rail lost its list: a change to publish");
    assert!(shown_letters(&browse).is_empty(), "the old counts include the title: {:?}", shown_letters(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "owed again");
    let (job, port) = the_directory_job(&mut browse);

    // The same read-back while that read is out: its answer was asked before it.
    assert!(!browse.state.apply_view_local(&ViewAt::leaf(sid, "98"), WATCHED_FACTS), "nothing shown, nothing lost");
    let container = letter_container(&[("A", 3), ("B", 3)]);
    finish_directory_with(&port, job.addr, client, job.req, move || Some(container));
    let _ = browse.pump();
    browse.jobs.clear();
    assert!(shown_letters(&browse).is_empty(), "asked before the read-back: {:?}", shown_letters(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    answer_letters(&mut browse, client, &[("A", 3), ("B", 2)]);
    assert_eq!(shown_letters(&browse), [("A".to_owned(), 3), ("B".to_owned(), 2)]);
}

/// What moves the key without a term (a scan, a membership change) forgets the kept list as it
/// forgets the shown one, and a library type change forgets both.
#[test]
fn a_membership_change_and_a_type_change_forget_the_kept_list() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    a_genre_menu(&mut browse);
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert!(browse.state.set_genre_by_id(Some("7")));
    land_letters(&mut browse, sid, client, &[("A", 1)]);

    browse.state.state_mut(0).unwrap().membership_changed();
    assert!(browse.state.set_genre_by_id(None));
    assert!(shown_letters(&browse).is_empty(), "the kept counts are of before the scan: {:?}", shown_letters(&browse));
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "asked afresh");

    browse.state.state_mut(0).unwrap().forget_letters();
    assert!(browse.state.states()[0].letters_kept.is_none());
}

/// The cost, stated and bounded: two lists are held, the last two keys asked. A third filter
/// evicts the oldest, so the way back to the unfiltered grid is read once more.
#[test]
fn only_the_last_two_lists_are_held() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    browse.state.state_mut(0).unwrap().genres = Arc::new(vec![
        GenreEntry { id: "7".into(), title: "Drama".into() },
        GenreEntry { id: "8".into(), title: "Comedy".into() },
    ]);
    land_letters(&mut browse, sid, client, &[("A", 3), ("B", 4)]);
    assert!(browse.state.set_genre_by_id(Some("7")));
    land_letters(&mut browse, sid, client, &[("A", 1)]);
    assert!(browse.state.set_genre_by_id(Some("8")));
    land_letters(&mut browse, sid, client, &[("B", 2)]);

    assert!(browse.state.set_genre_by_id(Some("7")));
    assert_eq!(shown_letters(&browse), [("A".to_owned(), 1)], "the previous one is held");
    assert!(browse.state.set_genre_by_id(None));
    assert!(shown_letters(&browse).is_empty(), "the oldest was evicted");
    tick_directory(&mut browse, sid, LibraryWork::Letters { today: None });
    assert_eq!(browse.jobs.len(), 1, "and is asked once more");
}
