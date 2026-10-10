//! The Continue Watching row through the Home store: servers' lanes merged into one row that pages
//! past its preview, a server that fails, and the edits the deck takes locally.

use super::*;
use super::test_support::*;

type Container = plx_plex::plex::MediaContainer;

/// One server's Continue Watching listing, newest first: `(rating key, last viewed)`.
struct Listing {
    items: Vec<(String, i64)>,
}

impl Listing {
    fn new(slot: u16, viewed: impl Iterator<Item = i64>) -> Listing {
        Listing { items: viewed.enumerate().map(|(i, v)| (format!("s{slot}-{i}"), v)).collect() }
    }

    fn page(&self, start: usize, size: usize) -> Option<Container> {
        let metadata: Vec<_> = self.items.iter().skip(start).take(size).map(|(key, viewed)| serde_json::json!({
            "ratingKey": key, "type": "movie", "title": "Movie", "thumb": "/poster", "lastViewedAt": viewed}))
            .collect();
        serde_json::from_value(serde_json::json!({"offset": start, "totalSize": self.items.len(),
            "Metadata": metadata})).ok()
    }

    /// The source a refresh would build from this listing: the preview, then the lookahead page.
    fn source(&self, slot: u16) -> Src {
        let preview = self.page(0, 12).unwrap();
        let hub = serde_json::json!({"hubIdentifier": "continueWatching", "more": self.items.len() > 12,
            "Metadata": preview.metadata.iter().map(|m| serde_json::json!({
                "ratingKey": m.rating_key, "type": "movie", "title": "Movie", "thumb": "/poster",
                "lastViewedAt": m.last_viewed_at})).collect::<Vec<_>>(), "totalSize": self.items.len()});
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
        &mut |request| { held.push(request); true });
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
        let landing = request.complete(build);
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
    assert!(edit_item(&mut owner.state, sid0, &gone, LocalEdit::LeftTheDeck));
    assert!(!rks(&owner.state, 0).contains(&gone));
    // The server drops it too, as `removeFromContinueWatching` does.
    let a = Listing { items: a.items.iter().filter(|(key, _)| *key != gone).cloned().collect() };
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
            &mut |request| { held.push(request); true });
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
        let _ = super::land(&mut owner.state, &owner.adapter, &request.complete(build));
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
    assert!(edit_item(&mut owner.state, sid0, &gone, LocalEdit::LeftTheDeck));
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
    assert!(edit_item(&mut owner.state, sid0, &marked, LocalEdit::Watched(true)));
    land_all(&mut owner, answers);
    let lane = &owner.state.srcs[0].last.as_ref().unwrap().lane;
    assert!(lane.rows.iter().any(|row| row.m.rk == marked), "the landed lane holds the card");
    assert!(lane.rows.iter().filter(|row| row.m.rk == marked).all(|row| row.m.watched), "the landing undid the mark");
    assert!(owner.state.srcs[0].edits.is_empty(), "the record of edits ends with the read it covered");
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

/// A move whose reads go out through the real worker path: a spawned fetch, the HTTP client, the
/// landing the worker posts. Waits for every worker, then lands what they left.
fn move_deck_over_the_wire(owner: &mut Owner, before: bool) {
    let adapter = Arc::clone(&owner.adapter);
    let _ = request_deck_page(&mut owner.state, &adapter, before, &BrowseScope::standalone(),
        &mut |request| spawn_fetch(&adapter, request));
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
