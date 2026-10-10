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
    plx_plex::plex::reset_servers_for_test();
    let mut srcs = Vec::new();
    for (i, listing) in listings.iter().enumerate() {
        let slot = plx_plex::plex::register_for_test(&format!("deck-{i}"), "127.0.0.1", 9 + i as i32, "synthetic", "fixture").raw();
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
