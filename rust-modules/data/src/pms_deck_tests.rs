//! The Continue Watching row through the Home store: servers' lanes merged into one row that pages
//! past its preview, a server that fails, and the edits the deck takes locally.

use super::*;
use super::test_support::*;

type Container = plx_plex::plex::MediaContainer;

/// One server's Continue Watching listing, newest first: `(rating key, last viewed)`.
#[derive(Default)]
struct Listing {
    items: Vec<(String, i64)>,
    /// Rows that are episodes: rating key -> (show id byte, carries Media). Every other row is a movie.
    shows: std::collections::HashMap<String, (u8, bool)>,
}

impl Listing {
    fn new(slot: u16, viewed: impl Iterator<Item = i64>) -> Listing {
        Listing { items: viewed.enumerate().map(|(i, v)| (format!("s{slot}-{i}"), v)).collect(), ..Listing::default() }
    }

    /// Makes row `at` an episode of show `show` (a Plex agent guid every server shares).
    fn episode(mut self, at: usize, show: u8, media: bool) -> Listing {
        let key = self.items[at].0.clone();
        self.shows.insert(key, (show, media));
        self
    }

    /// One row as the server sends it.
    fn row_json(&self, key: &str, viewed: i64) -> serde_json::Value {
        match self.shows.get(key) {
            None => serde_json::json!({"ratingKey": key, "type": "movie", "title": "Movie", "thumb": "/poster",
                "lastViewedAt": viewed}),
            Some(&(show, media)) => {
                let mut row = serde_json::json!({"ratingKey": key, "type": "episode", "title": "Episode",
                    "thumb": "/still", "grandparentThumb": "/poster", "lastViewedAt": viewed,
                    "grandparentRatingKey": format!("show{show}"), "grandparentTitle": format!("Show {show}"),
                    "grandparentGuid": format!("plex://show/{}", format!("{show:02x}").repeat(12)),
                    "parentIndex": 1, "index": 1});
                if media {
                    row["Media"] = serde_json::json!([{"videoCodec": "h264", "Part": [{"key": format!("/library/parts/{key}/file.mkv")}]}]);
                }
                row
            }
        }
    }

    fn page(&self, start: usize, size: usize) -> Option<Container> {
        let metadata: Vec<_> = self.items.iter().skip(start).take(size).map(|(key, viewed)| self.row_json(key, *viewed))
            .collect();
        serde_json::from_value(serde_json::json!({"offset": start, "totalSize": self.items.len(),
            "Metadata": metadata})).ok()
    }

    /// The source a refresh would build from this listing: the preview, then the lookahead page.
    fn source(&self, slot: u16) -> Src {
        let hub = serde_json::json!({"hubIdentifier": "continueWatching", "more": self.items.len() > 12,
            "Metadata": self.items.iter().take(12).map(|(key, viewed)| self.row_json(key, *viewed)).collect::<Vec<_>>(),
            "totalSize": self.items.len()});
        let cw: Container = serde_json::from_value(serde_json::json!({"Hub": [hub]})).unwrap();
        let mut build = project(&Container::default(), &cw, sid(slot));
        with_lookahead(&mut build, sid(slot), |start, size| self.page(start, size));
        src(slot, "", HubState::Ready, Some(build))
    }
}

fn deck_owner(listings: &[&Listing]) -> Owner {
    deck_owner_at(listings, &(0..listings.len() as i32).map(|i| 9 + i).collect::<Vec<_>>())
}

/// [`deck_owner`] with each server registered at the loopback port given for it.
fn deck_owner_at(listings: &[&Listing], ports: &[i32]) -> Owner {
    plx_plex::plex::reset_servers_for_test();
    let mut srcs = Vec::new();
    for (i, listing) in listings.iter().enumerate() {
        let slot = plx_plex::plex::register_for_test(&format!("deck-{i}"), "127.0.0.1", ports[i], "synthetic", "fixture").raw();
        srcs.push(listing.source(slot));
    }
    settle_deck(&mut srcs, false);
    let mut owner = Owner::default();
    seed(&mut owner.state, srcs);
    owner
}

fn expected(listings: &[&Listing]) -> Vec<String> {
    let mut all: Vec<(i64, usize, usize, String)> = Vec::new();
    for (i, listing) in listings.iter().enumerate() {
        for (at, (key, viewed)) in listing.items.iter().enumerate() { all.push((-viewed, i, at, key.clone())); }
    }
    all.sort();
    all.into_iter().map(|item| item.3).collect()
}

/// Asks for a move and answers each request the way its worker would. `fail` is the listing index
/// whose server does not answer.
fn move_deck(owner: &mut Owner, listings: &[&Listing], before: bool, fail: Option<usize>) {
    let mut held = Vec::new();
    let _ = request_deck_page(&mut owner.state, &owner.adapter, before, &BrowseScope::standalone(),
        &mut |request| { held.push(request); });
    for request in held {
        let at = owner.state.srcs.iter().position(|s| s.sid == request.sid).unwrap();
        let listing = listings[at];
        let build = if fail == Some(at) { None } else {
            let (mut lane, hidden) = request.lane.clone().unwrap();
            let read = if before {
                deck::read_behind(request.sid, &mut lane, deck::PAGE, &hidden, |s, n| listing.page(s, n), |_| None)
            } else {
                deck::read_ahead(request.sid, &mut lane, deck::PAGE, &hidden, |s, n| listing.page(s, n))
            };
            read.ok().map(|()| SourceBuild { lane, ..SourceBuild::default() })
        };
        let landing = request.complete_for_test(build);
        let landing = record::decode(record::encode(&landing), |_| plx_plex::plex::client_for(landing.sid)).unwrap();
        let _ = super::land(&mut owner.state, &owner.adapter, &landing);
    }
}

#[test]
fn the_row_reaches_every_in_progress_item_of_every_server_and_comes_back() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..40).map(|i| 9000 - i * 20));
    let b = Listing::new(1, (0..30).map(|i| 8990 - i * 20));
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    let want = expected(&listings);
    assert_eq!(rks(&owner.state, 0), want[..24].to_vec());
    let mut shown = rks(&owner.state, 0);
    for _ in 0..20 {
        move_deck(&mut owner, &listings, false, None);
        let window = rks(&owner.state, 0);
        assert!(window.len() <= MAX_SHELF_ITEMS);
        for key in window { if !shown.contains(&key) { shown.push(key); } }
    }
    assert_eq!(shown, want, "every item, in the order of a full sort, once");
    assert!(!hubs(&owner.state)[0].more, "nothing after the last card");
    for _ in 0..20 { move_deck(&mut owner, &listings, true, None); }
    assert_eq!(rks(&owner.state, 0), want[..24].to_vec());
    assert_eq!(hubs(&owner.state)[0].offset, 0);
    plx_plex::plex::reset_servers_for_test();
}

/// The deck's page ask rides `HubsCmd::Page` like any Home row's, so it is refused the same way when
/// it names a window the deck has since moved, and honoured from the window that stands.
#[test]
fn a_deck_ask_computed_from_a_window_the_deck_has_since_moved_is_refused() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..60).map(|i| 9000 - i * 20));
    let b = Listing::new(1, (0..60).map(|i| 8990 - i * 20));
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    let cmd = |seen: u32| Some(crate::stores::hubs::HubsCmd::Page { sid: sid(0), id: DECK_ID.into(), key: String::new(),
        before: false, seen });
    let seen = hubs_snapshot(&owner.state).view().generation;
    move_deck(&mut owner, &listings, false, None);
    let moved = rks(&owner.state, 0);
    let now = hubs_snapshot(&owner.state).view().generation;
    assert_ne!(seen, now, "the landing published a new window");
    let mut launched = 0;
    let _ = controlled_work(&mut owner.state, &owner.adapter, cmd(seen), 0.0, &mut |_| { launched += 1; true });
    assert_eq!(launched, 0, "the ask names a window the deck no longer holds");
    assert!(owner.state.deck_ask.is_none());
    assert_eq!(rks(&owner.state, 0), moved);
    let _ = controlled_work(&mut owner.state, &owner.adapter, cmd(now), 0.0, &mut |_| { launched += 1; true });
    assert!(launched > 0, "from the window that stands it is honoured");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_server_that_does_not_answer_does_not_stop_the_others() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..60).map(|i| 9000 - i * 20));
    let b = Listing::new(1, (0..60).map(|i| 8990 - i * 20));
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    move_deck(&mut owner, &listings, false, None);
    let held = rks(&owner.state, 0);
    move_deck(&mut owner, &listings, false, Some(1));
    assert!(owner.state.deck_ask.is_none(), "the move ran without waiting for the server in retry");
    let window = rks(&owner.state, 0);
    assert_ne!(window, held, "the deck moved");
    assert!(window.iter().any(|key| key.starts_with("s0-") && !held.contains(key)), "the healthy server's cards came in");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_card_removed_from_the_deck_leaves_the_window_and_the_next_page_repeats_nothing() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..60).map(|i| 9000 - i * 20));
    let listings = [&a];
    let mut owner = deck_owner(&listings);
    move_deck(&mut owner, &listings, false, None);
    let gone = rks(&owner.state, 0)[3].clone();
    let sid0 = owner.state.srcs[0].sid;
    assert!(edit_item(&mut owner.state, &ViewAt::leaf(sid0, &gone), ViewEdit::LeftDeck));
    assert!(!rks(&owner.state, 0).contains(&gone));
    // The server drops it too, as `removeFromContinueWatching` does.
    let a = Listing { items: a.items.iter().filter(|(key, _)| *key != gone).cloned().collect(), ..Listing::default() };
    let listings = [&a];
    let mut shown = rks(&owner.state, 0);
    for _ in 0..10 {
        let held = rks(&owner.state, 0);
        move_deck(&mut owner, &listings, false, None);
        for key in rks(&owner.state, 0) {
            if !held.contains(&key) { assert!(!shown.contains(&key), "{key} came back"); shown.push(key); }
        }
    }
    assert!(shown.iter().all(|key| *key != gone));
    assert_eq!(shown.len(), 59 - 12, "everything past the first page, once");
    plx_plex::plex::reset_servers_for_test();
}

/// Moves forward until a move has to read, and returns that read's requests with their worker
/// answers, not yet landed. The moves before it are answered and landed in full.
fn read_in_flight(owner: &mut Owner, listings: &[&Listing]) -> Vec<(HubRequest, Option<SourceBuild>)> {
    let mut held = Vec::new();
    for _ in 0..10 {
        let _ = request_deck_page(&mut owner.state, &owner.adapter, false, &BrowseScope::standalone(),
            &mut |request| { held.push(request); });
        if !held.is_empty() { break; }
    }
    assert!(!held.is_empty(), "no move needed a read");
    held.into_iter().map(|request| {
        let at = owner.state.srcs.iter().position(|s| s.sid == request.sid).unwrap();
        let (mut lane, hidden) = request.lane.clone().unwrap();
        let listing = listings[at];
        deck::read_ahead(request.sid, &mut lane, deck::PAGE, &hidden, |s, n| listing.page(s, n)).unwrap();
        (request, Some(SourceBuild { lane, ..SourceBuild::default() }))
    }).collect()
}

fn land_all(owner: &mut Owner, answers: Vec<(HubRequest, Option<SourceBuild>)>) {
    for (request, build) in answers {
        let _ = super::land(&mut owner.state, &owner.adapter, &request.complete_for_test(build));
    }
}

#[test]
fn a_card_removed_while_a_deck_page_is_in_flight_does_not_come_back_when_it_lands() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..60).map(|i| 9000 - i * 20));
    let listings = [&a];
    let mut owner = deck_owner(&listings);
    let answers = read_in_flight(&mut owner, &listings);
    let gone = rks(&owner.state, 0).pop().unwrap();
    let sid0 = owner.state.srcs[0].sid;
    owner.state.ledger.record(&ViewAt::leaf(sid0, &gone), ViewEdit::LeftDeck);
    assert!(edit_item(&mut owner.state, &ViewAt::leaf(sid0, &gone), ViewEdit::LeftDeck));
    assert!(answers[0].1.as_ref().unwrap().lane.rows.iter().any(|row| row.m.rk == gone), "the read holds the card");
    land_all(&mut owner, answers);
    let lane = &owner.state.srcs[0].last.as_ref().unwrap().lane;
    assert!(lane.rows.iter().all(|row| row.m.rk != gone), "the landing put the removed card back in the lane");
    assert!(!rks(&owner.state, 0).contains(&gone), "the removed card is back in the row");
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_card_marked_watched_while_a_deck_page_is_in_flight_stays_watched_when_it_lands() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..60).map(|i| 9000 - i * 20));
    let listings = [&a];
    let mut owner = deck_owner(&listings);
    let answers = read_in_flight(&mut owner, &listings);
    let marked = rks(&owner.state, 0).pop().unwrap();
    let sid0 = owner.state.srcs[0].sid;
    owner.state.ledger.record(&ViewAt::leaf(sid0, &marked), ViewEdit::Watched(true));
    assert!(edit_item(&mut owner.state, &ViewAt::leaf(sid0, &marked), ViewEdit::Watched(true)));
    land_all(&mut owner, answers);
    let lane = &owner.state.srcs[0].last.as_ref().unwrap().lane;
    assert!(lane.rows.iter().any(|row| row.m.rk == marked), "the landed lane holds the card");
    assert!(lane.rows.iter().filter(|row| row.m.rk == marked).all(|row| row.m.watched), "the landing undid the mark");
    plx_plex::plex::reset_servers_for_test();
}

/// A plain-HTTP server on a loopback port that answers `/hubs/continueWatching/items` from a
/// listing, one page per request, and refuses everything else. Stops when dropped.
struct Loopback {
    port: i32,
    /// Pages served.
    hits: Arc<std::sync::atomic::AtomicUsize>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Loopback {
    fn serve(listing: &Listing) -> Option<Loopback> {
        use std::io::{Read, Write};
        use std::sync::atomic::Ordering;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").ok()?;
        listener.set_nonblocking(true).ok()?;
        let port = i32::from(listener.local_addr().ok()?.port());
        let items = listing.items.clone();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let quit = Arc::clone(&stop);
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let served = Arc::clone(&hits);
        let thread = std::thread::spawn(move || {
            while !quit.load(Ordering::Acquire) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                };
                let items = items.clone();
                let served = Arc::clone(&served);
                std::thread::spawn(move || {
                    let _ = stream.set_nonblocking(false);
                    let mut head = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk) { Ok(0) | Err(_) => return, Ok(n) => head.extend_from_slice(&chunk[..n]) }
                    }
                    let line = String::from_utf8_lossy(&head);
                    let target = line.split_whitespace().nth(1).unwrap_or("");
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    let param = |name: &str| query.split('&').find_map(|part| part.strip_prefix(name)?.strip_prefix('='))
                        .and_then(|value| value.parse::<usize>().ok());
                    let reply = if path == "/hubs/continueWatching/items" {
                        served.fetch_add(1, Ordering::AcqRel);
                        let (start, size) = (param("X-Plex-Container-Start").unwrap_or(0), param("X-Plex-Container-Size").unwrap_or(items.len()));
                        let metadata: Vec<_> = items.iter().skip(start).take(size).map(|(key, viewed)| serde_json::json!({
                            "ratingKey": key, "type": "movie", "title": "Movie", "thumb": "/poster", "lastViewedAt": viewed})).collect();
                        let body = serde_json::json!({"MediaContainer": {"offset": start, "size": metadata.len(),
                            "totalSize": items.len(), "Metadata": metadata}}).to_string();
                        format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
                    } else {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned()
                    };
                    let _ = stream.write_all(reply.as_bytes());
                });
            }
        });
        Some(Loopback { port, hits, stop, thread: Some(thread) })
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

/// A move whose reads go out through the real worker path: a launched job, the HTTP client, the
/// landing the worker posts. Waits for every worker, then lands what they left.
fn move_deck_over_the_wire(owner: &mut Owner, before: bool) {
    let adapter = Arc::clone(&owner.adapter);
    let _ = request_deck_page(&mut owner.state, &adapter, before, &BrowseScope::standalone(),
        &mut |request| {
            let client = request.client_for_test();
            let job = Job { addr: hubs_addr(request.seq), req: request };
            let started = launch_hubs_job(job, HubsPort::of(&adapter), client, |work| { std::thread::spawn(work); true });
            assert!(started, "a worker for the read");
        });
    let started = std::time::Instant::now();
    let mut landings = Vec::new();
    while owed_count_for_test(&adapter) > 0 {
        assert!(started.elapsed() < std::time::Duration::from_secs(20), "a worker never answered");
        landings.extend(take_landings(&adapter));
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    for landing in landings { let _ = super::land(&mut owner.state, &adapter, &landing); }
}

#[test]
fn a_two_server_deck_reads_through_the_workers_to_its_end_and_back() {
    let _guard = plx_base::testlock::serial();
    if !plx_net::net::global_init() { return; }
    let a = Listing::new(0, (0..40).map(|i| 9000 - i * 20));
    let b = Listing::new(1, (0..30).map(|i| 8990 - i * 20));
    let (Some(first), Some(second)) = (Loopback::serve(&a), Loopback::serve(&b)) else { return };
    let listings = [&a, &b];
    let mut owner = deck_owner_at(&listings, &[first.port, second.port]);
    let want = expected(&listings);
    assert_eq!(rks(&owner.state, 0), want[..24].to_vec());
    let mut shown = rks(&owner.state, 0);
    for _ in 0..20 {
        move_deck_over_the_wire(&mut owner, false);
        let window = rks(&owner.state, 0);
        assert!(window.len() <= MAX_SHELF_ITEMS);
        for key in window { if !shown.contains(&key) { shown.push(key); } }
    }
    assert_eq!(shown, want, "every item of both servers, in the order of a full sort, once");
    assert!(!hubs(&owner.state)[0].more, "nothing after the last card");
    for _ in 0..20 { move_deck_over_the_wire(&mut owner, true); }
    assert_eq!(rks(&owner.state, 0), want[..24].to_vec(), "back at the head");
    for server in [&first, &second] {
        assert!(server.hits.load(std::sync::atomic::Ordering::Acquire) > 0, "the pages came over the wire");
    }
    assert_eq!(hubs(&owner.state)[0].offset, 0);
    plx_plex::plex::reset_servers_for_test();
}

/// The shelf takes an ask as answered when the store publishes, and Home resets the deck row's
/// asks on every Hubs notice, so the deck needs no landing count of its own (`HubRef::epoch` stays
/// 0 for it): every landing of a deck read, and a read that failed and let the deck move without
/// its server, commits a publication.
#[test]
fn every_landing_of_a_deck_read_publishes_so_the_row_needs_no_epoch_of_its_own() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..40).map(|i| 9000 - i * 20));
    let b = Listing::new(1, (0..30).map(|i| 8990 - i * 20));
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    let mut moves = 0;
    while hubs(&owner.state)[0].more && moves < 30 {
        let (gen, fail) = (owner.state.catalog_gen, (moves % 4 == 3).then_some(1));
        move_deck(&mut owner, &listings, false, fail);
        assert_ne!(owner.state.catalog_gen, gen, "move {moves} (failed server: {fail:?}) published");
        assert_eq!(hubs(&owner.state)[0].epoch, 0, "and the deck row counts nothing itself");
        moves += 1;
    }
    assert!(moves >= 4, "the walk took {moves} moves");
    plx_plex::plex::reset_servers_for_test();
}


// ---- one tile per show across servers (#892) -------------------------------------------------------

/// Every rating key the deck has shown while it walks to its end, in the order first seen.
fn walk_forward(owner: &mut Owner, listings: &[&Listing]) -> Vec<String> {
    let mut shown = rks(&owner.state, 0);
    for _ in 0..40 {
        move_deck(owner, listings, false, None);
        for key in rks(&owner.state, 0) { if !shown.contains(&key) { shown.push(key); } }
        if !hubs(&owner.state)[0].more { break; }
    }
    shown
}

/// A copy the roster-first server lists deeper than the window is still the winner: the later
/// server's copy at the head must stay hidden until the winner is paged in, never shown twice.
#[test]
fn a_copy_read_pages_later_stays_hidden() {
    let _guard = plx_base::testlock::serial();
    // A's show sits 21 rows down its deck, past the window but inside the lookahead its lane holds; B lists the same show first of all.
    let a = Listing::new(0, (0..40).map(|i| 9000 - i * 20)).episode(21, 4, true);
    let b = Listing::new(1, (0..6).map(|i| 9500 - i * 20)).episode(0, 4, true);
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    assert!(!rks(&owner.state, 0).contains(&"s1-0".to_string()), "B's copy hides behind A's, which A lists later");
    let shown = walk_forward(&mut owner, &listings);
    assert!(!shown.contains(&"s1-0".to_string()), "B's copy never shows: {shown:?}");
    assert_eq!(shown.iter().filter(|key| *key == "s0-21").count(), 1, "A's copy shows once, where it sorts");
    plx_plex::plex::reset_servers_for_test();
}

/// The roster-first copy the window has already left behind keeps hiding the later server's.
#[test]
fn an_unshown_roster_first_copy_hides_a_later_servers_copy() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..50).map(|i| 9000 - i * 20)).episode(0, 4, true);
    let b = Listing::new(1, (0..50).map(|i| 8990 - i * 20)).episode(30, 4, true);
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    assert!(rks(&owner.state, 0).contains(&"s0-0".to_string()), "premise: A's copy is in the first window");
    let shown = walk_forward(&mut owner, &listings);
    assert!(!rks(&owner.state, 0).contains(&"s0-0".to_string()), "premise: the window has left A's copy behind");
    assert!(!shown.contains(&"s1-30".to_string()), "B's copy of the same show never shows: {shown:?}");
    plx_plex::plex::reset_servers_for_test();
}

/// A winner that left its server (removed from the deck) hides nothing; the other copy shows.
#[test]
fn a_winner_that_left_its_server_no_longer_hides_the_other_copy() {
    let _guard = plx_base::testlock::serial();
    let a = Listing::new(0, (0..50).map(|i| 9000 - i * 20)).episode(2, 4, true);
    let b = Listing::new(1, (0..50).map(|i| 8990 - i * 20)).episode(30, 4, true);
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    let sid0 = owner.state.srcs[0].sid;
    assert!(edit_item(&mut owner.state, &ViewAt::leaf(sid0, "s0-2"), ViewEdit::LeftDeck));
    let a = Listing { items: a.items.iter().filter(|(key, _)| key != "s0-2").cloned().collect(), shows: a.shows.clone() };
    let listings = [&a, &b];
    let shown = walk_forward(&mut owner, &listings);
    assert!(shown.contains(&"s1-30".to_string()), "with A's copy gone B's copy is the tile: {shown:?}");
    plx_plex::plex::reset_servers_for_test();
}

/// Twenty-four shows on both servers: the row pages to its end with every show once, however short
/// a window the hidden copies leave, and it keeps asking on while a window of copies is all hidden.
#[test]
fn a_window_of_hidden_copies_asks_on() {
    let _guard = plx_base::testlock::serial();
    let (mut a, mut b) = (Listing::new(0, (0..30).map(|i| 9000 - i * 20)), Listing::new(1, (0..30).map(|i| 8990 - i * 20)));
    for i in 0..30 { a = a.episode(i, i as u8, true); b = b.episode(i, i as u8, true); }
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    assert!(hubs(&owner.state)[0].more, "premise: 30 shows are more than one window");
    let shown = walk_forward(&mut owner, &listings);
    let own: Vec<_> = shown.iter().filter(|key| key.starts_with("s0-")).collect();
    assert_eq!(own.len(), 30, "every show once, from the roster-first server: {shown:?}");
    assert!(shown.iter().all(|key| key.starts_with("s0-")), "and never the other server's copy: {shown:?}");
    plx_plex::plex::reset_servers_for_test();
}

/// A mirrored household whose second server is read long ago: every copy on it sorts after the
/// first server's. A forward ask can land on a window of nothing but hidden copies, and the row
/// must not vanish while it has more to show: it asks on until a card is visible, and at the end
/// of the listing it stays where the last visible card was.
#[test]
fn a_window_of_only_hidden_copies_keeps_the_row() {
    let _guard = plx_base::testlock::serial();
    let (mut a, mut b) = (Listing::new(0, (0..40).map(|i| 9000 - i * 20)), Listing::new(1, (0..40).map(|i| 5000 - i * 20)));
    for i in 0..40 { a = a.episode(i, i as u8, true); b = b.episode(i, i as u8, true); }
    let listings = [&a, &b];
    let mut owner = deck_owner(&listings);
    let mut shown = rks(&owner.state, 0);
    for step in 0..12 {
        move_deck(&mut owner, &listings, false, None);
        assert!(hub_is_continue(&owner.state, 0), "step {step}: the row is still there ({} cards shown)", shown.len());
        assert!(!rks(&owner.state, 0).is_empty(), "step {step}: and it holds a card");
        for key in rks(&owner.state, 0) { if !shown.contains(&key) { shown.push(key); } }
    }
    assert_eq!(shown.iter().filter(|key| key.starts_with("s0-")).count(), 40, "every show once: {shown:?}");
    assert!(shown.iter().all(|key| key.starts_with("s0-")), "never the other server's copy: {shown:?}");
    plx_plex::plex::reset_servers_for_test();
}
