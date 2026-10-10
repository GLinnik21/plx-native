//! A Related row longer than `/related`'s previews: the head is every preview, the tails are the
//! hubs' own listings read from where each preview stops, chained in response order through the
//! shared sliding window. Run against a loopback server that honours `X-Plex-Container-Start/Size`.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;

/// `/library/metadata/{hub}/similar` listings: hub `h` holds `totals[h]` items, item `i` is
/// `"{h}-{i}"` unless `rename` says otherwise.
struct Hubs {
    port: i32,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn param(request: &str, name: &str) -> Option<usize> {
    let query = request.split_once('?')?.1.split_whitespace().next()?;
    query.split('&').find_map(|kv| kv.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
}

fn serve(totals: Vec<(u32, usize)>, rename: fn(u32, usize) -> String) -> Hubs {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let stop = Arc::new(AtomicBool::new(false));
    let halt = stop.clone();
    let thread = std::thread::spawn(move || {
        while !halt.load(AtomicOrdering::SeqCst) {
            let Ok((mut conn, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            conn.set_nonblocking(false).unwrap();
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let hub: u32 = request.split("/library/metadata/").nth(1)
                .and_then(|rest| rest.split('/').next()).and_then(|id| id.parse().ok()).unwrap_or(0);
            let total = totals.iter().find(|(h, _)| *h == hub).map_or(0, |(_, t)| *t);
            let start = param(&request, "X-Plex-Container-Start").unwrap_or(0);
            let size = param(&request, "X-Plex-Container-Size").unwrap_or(usize::MAX);
            let to = start.saturating_add(size).min(total);
            let rows: Vec<String> = (start.min(total)..to)
                .map(|i| format!(r#"{{"ratingKey":"{}","type":"movie","title":"T{hub}-{i}","thumb":"/t/{hub}/{i}"}}"#, rename(hub, i)))
                .collect();
            let body = format!(
                r#"{{"MediaContainer":{{"size":{},"totalSize":{total},"offset":{start},"Metadata":[{}]}}}}"#,
                rows.len(), rows.join(","),
            );
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
        }
    });
    Hubs { port, stop, thread: Some(thread) }
}

impl Drop for Hubs {
    fn drop(&mut self) {
        self.stop.store(true, AtomicOrdering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        plx_plex::plex::reset_servers_for_test();
    }
}

fn plain(hub: u32, i: usize) -> String {
    format!("{hub}-{i}")
}

/// Item 20 of hub 1 is a title the head already shows.
fn head_title_at_20(hub: u32, i: usize) -> String {
    if hub == 1 && i == 20 { "1-0".into() } else { plain(hub, i) }
}

/// A `/related` response: one hub per `(id, preview length, listing total, more)`, previews being
/// the first items of the listing, plus any extra hub JSON.
fn related_response(hubs: &[(u32, usize, usize, bool)], extra: &str) -> plx_plex::plex::MediaContainer {
    let hubs: Vec<String> = hubs.iter().map(|&(id, preview, total, more)| {
        let items: Vec<String> = (0..preview)
            .map(|i| format!(r#"{{"ratingKey":"{id}-{i}","type":"movie","title":"T{id}-{i}","thumb":"/t/{id}/{i}"}}"#))
            .collect();
        format!(
            r#"{{"hubIdentifier":"movie.similar.{id}","title":"Hub {id}","type":"movie","key":"/library/metadata/{id}/similar","more":{more},"size":{preview},"totalSize":{total},"Metadata":[{}]}}"#,
            items.join(","),
        )
    }).chain((!extra.is_empty()).then(|| extra.to_string())).collect();
    serde_json::from_str(&format!(r#"{{"size":{},"Hub":[{}]}}"#, hubs.len(), hubs.join(","))).unwrap()
}

fn open(server: &Hubs, mc: &plx_plex::plex::MediaContainer) {
    assert!(plx_net::net::global_init() && plx_net::net::available() && plx_net::net::threaded_tls_ready());
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("related-tail", "127.0.0.1", server.port, "token", "related-tail-client");
    plx_plex::plex::client_for(sid).unwrap().set_link(plx_plex::plex::probe::Location::Local);
    let rows = related_rows(mc, sid, "page");
    test_state().current = Some(Detail {
        sid,
        rk: "page".into(),
        related: rows.related,
        related_tail: rows.tail,
        ..Default::default()
    });
}

fn detail() -> &'static Detail {
    test_state().current.as_ref().unwrap()
}

/// Ask for the next (or previous) tail window and wait for it to land. A read that only advances
/// the ledger of a listing that would not hold still lands too, with the window where it was.
fn slide(before: bool) {
    let state = || (detail().related_tail.offset, detail().related_tail.end, detail().related_tail.row.clone());
    let was = state();
    let seen = (detail().related_tail.offset, detail().related_tail.end);
    assert!(want_related(test_state(), test_adapter(), before, seen), "a request starts");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while state() == was && std::time::Instant::now() < until {
        pump_related_pages(test_state(), test_adapter());
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_ne!(state(), was, "the window never moved");
}

fn window() -> Vec<(usize, String)> {
    let d = detail();
    d.related_tail.positions.iter().copied().zip(d.related[d.related_tail.head..].iter().map(|m| m.rk.clone())).collect()
}

/// Two hubs with 300 and 100 more items each than their previews are walked to the end and back:
/// at most the head and 24 tail cards are held, every item is seen once, in response order, and the
/// walk back shows the same item at the same position.
#[test]
fn two_hubs_are_chained_walked_to_the_end_and_back_within_the_window() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 312), (2, 106)], plain);
    let mc = related_response(&[(1, 12, 312, true), (2, 6, 106, true)], "");
    open(&server, &mc);
    let head = detail().related.len();
    assert_eq!(head, 18, "the head is every preview");
    assert_eq!(detail().related_tail.head, head);
    assert!(detail().related_tail.more);

    let mut forward: Vec<(usize, String)> = Vec::new();
    while detail().related_tail.more {
        slide(false);
        assert!(detail().related.len() <= head + 24, "{} cards held", detail().related.len());
        assert_eq!(detail().related.len(), head + detail().related_tail.positions.len());
        for (position, rk) in window() {
            match forward.iter().find(|(p, _)| *p == position) {
                Some((_, seen)) => assert_eq!(*seen, rk, "position {position} changed"),
                None => forward.push((position, rk)),
            }
        }
    }
    let expected: Vec<String> = (12..312).map(|i| plain(1, i)).chain((6..106).map(|i| plain(2, i))).collect();
    let seen: Vec<String> = forward.iter().map(|(_, rk)| rk.clone()).collect();
    assert_eq!(seen, expected, "every tail item once, hub 1 then hub 2");

    // Back to the start: each window shows the item the walk forward put at that position.
    while detail().related_tail.offset > 0 {
        slide(true);
        assert!(detail().related.len() <= head + 24);
        for (position, rk) in window() {
            assert_eq!(forward.iter().find(|(p, _)| *p == position).map(|(_, r)| r), Some(&rk), "position {position}");
        }
    }
    assert_eq!(window().first().map(|(_, rk)| rk.clone()), Some(plain(1, 12)));
}

/// A tail item the head already shows is skipped, not shown twice.
#[test]
fn a_key_the_head_shows_is_not_shown_again() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 60)], head_title_at_20);
    let mc = related_response(&[(1, 12, 60, true)], "");
    open(&server, &mc);
    let mut tail: Vec<String> = Vec::new();
    while detail().related_tail.more {
        slide(false);
        for (_, rk) in window() {
            if !tail.contains(&rk) { tail.push(rk); }
        }
    }
    assert!(!tail.iter().any(|rk| rk == "1-0"), "the head's title is not in the tail");
    assert_eq!(tail.len(), 60 - 12 - 1);
}

/// A hub whose key the pager does not admit keeps its preview, gets no tail, and is counted.
#[test]
fn an_unadmitted_hub_keeps_its_preview_and_is_counted() {
    let items = r#"{"ratingKey":"x1","type":"movie","title":"X1","thumb":"/t/x"}"#;
    let extra = format!(
        r#"{{"hubIdentifier":"odd","title":"Odd","type":"movie","key":"/library/metadata/9/extras","more":true,"size":1,"totalSize":50,"Metadata":[{items}]}}"#
    );
    let mc = related_response(&[(1, 3, 40, true)], &extra);
    let rows = related_rows(&mc, plx_plex::plex::ServerId::UNSET, "page");
    assert_eq!(rows.related.len(), 4, "both previews stay");
    assert_eq!(rows.tail.unadmitted, 1);
    assert_eq!(rows.tail.hubs.len(), 1, "the admitted hub has its tail");
    assert_eq!(rows.tail.hubs[0].preview, 3);
}

/// Hubs that hold nothing past their preview make no tail, and the encoding is unchanged.
#[test]
fn a_row_with_no_tail_is_the_plain_encoding() {
    let mc = related_response(&[(1, 5, 5, false)], "");
    let rows = related_rows(&mc, plx_plex::plex::ServerId::UNSET, "page");
    assert!(rows.tail.hubs.is_empty() && !rows.tail.more);
    let d = Detail { related: rows.related, related_tail: rows.tail, ..Default::default() };
    let json = serde_json::to_value(&d).unwrap();
    assert!(json.get("related_tail").is_none());
}

/// A hub whose listing the test edits between reads: `hook(read, listing)` runs before the
/// `read`th listing page is answered (0-based), and a batch read by rating key answers from the
/// listing as it then stands, leaving out a key that is gone.
fn serve_live(listing: Vec<String>, hook: impl Fn(usize, &mut Vec<String>) + Send + 'static) -> Hubs {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    let stop = Arc::new(AtomicBool::new(false));
    let halt = stop.clone();
    let thread = std::thread::spawn(move || {
        let (mut listing, mut reads) = (listing, 0usize);
        let item = |key: &str| format!(r#"{{"ratingKey":"{key}","type":"movie","title":"T{key}","thumb":"/t/{key}"}}"#);
        while !halt.load(AtomicOrdering::SeqCst) {
            let Ok((mut conn, _)) = listener.accept() else {
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            };
            conn.set_nonblocking(false).unwrap();
            let mut buf = [0u8; 4096];
            let n = conn.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("");
            let rest = path.strip_prefix("/library/metadata/").unwrap_or("");
            let body = if rest.contains("/similar") {
                hook(reads, &mut listing);
                reads += 1;
                let total = listing.len();
                let start = param(&request, "X-Plex-Container-Start").unwrap_or(0);
                let size = param(&request, "X-Plex-Container-Size").unwrap_or(usize::MAX);
                let rows: Vec<String> = listing[start.min(total)..start.saturating_add(size).min(total)]
                    .iter().map(|key| item(key)).collect();
                format!(r#"{{"MediaContainer":{{"size":{},"totalSize":{total},"offset":{start},"Metadata":[{}]}}}}"#,
                    rows.len(), rows.join(","))
            } else {
                let keys = rest.split('?').next().unwrap_or("");
                let rows: Vec<String> = keys.split(',').filter(|key| listing.iter().any(|k| k == key))
                    .map(item).collect();
                format!(r#"{{"MediaContainer":{{"size":{},"Metadata":[{}]}}}}"#, rows.len(), rows.join(","))
            };
            let _ = write!(conn,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len());
        }
    });
    Hubs { port, stop, thread: Some(thread) }
}

/// Walks the tail forward to its end over a listing that changes under the walk. A card is never
/// shown again once it has left the window, and at most the head and 24 cards are held (the
/// ledger behind it is keys only: eight bytes a key for the numeric keys PMS issues, so the
/// listing's length times eight). Returns what was shown, in order.
fn walk_live_listing(initial: usize, hook: impl Fn(usize, &mut Vec<String>) + Send + 'static) -> Vec<String> {
    let listing: Vec<String> = (0..initial).map(|i| plain(1, i)).collect();
    let server = serve_live(listing, hook);
    let mc = related_response(&[(1, 12, initial, true)], "");
    open(&server, &mc);
    let head: Vec<String> = detail().related.iter().map(|m| m.rk.clone()).collect();
    let (mut shown, mut before): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    for _ in 0..200 {
        if !detail().related_tail.more { break; }
        slide(false);
        assert!(detail().related.len() <= head.len() + 24, "{} cards held", detail().related.len());
        let now: Vec<String> = window().into_iter().map(|(_, rk)| rk).collect();
        for rk in &now {
            assert!(!head.contains(rk), "{rk} is in the head and the tail");
            if !before.contains(rk) {
                assert!(!shown.contains(rk), "{rk} was shown again");
                shown.push(rk.clone());
            }
        }
        assert_eq!(now.iter().collect::<std::collections::HashSet<_>>().len(), now.len(), "a window repeats a card");
        before = now;
    }
    assert!(!detail().related_tail.more, "the walk never reached the end");
    shown
}

/// Forty titles arrive ahead of the window mid-walk: the edge is more than a page from where it was
/// read, so the window can no longer re-anchor on it. The walk still reaches every title. (They land
/// just past the head's twelve previews: the head is the page's own snapshot, never re-read.)
#[test]
fn an_item_inserted_before_the_window_mid_walk_is_reached_once() {
    let _serial = plx_base::testlock::serial();
    let shown = walk_live_listing(80, |read, listing| {
        if read == 3 { for i in 0..40 { listing.insert(12 + i % 5, format!("n-{i}")); } }
    });
    let mut expected: Vec<String> = (12..80).map(|i| plain(1, i)).chain((0..40).map(|i| format!("n-{i}"))).collect();
    let mut got = shown.clone();
    expected.sort();
    got.sort();
    assert_eq!(got, expected, "every item exactly once");
}

/// Thirty titles the walk has already shown leave the listing: what lies ahead is still reached.
#[test]
fn an_item_removed_mid_walk_does_not_stop_the_walk() {
    let _serial = plx_base::testlock::serial();
    let shown = walk_live_listing(90, |read, listing| {
        if read == 3 { listing.drain(14..44); }
    });
    for i in (44..90).chain(12..14) {
        assert!(shown.contains(&plain(1, i)), "{} was never reached", plain(1, i));
    }
}

/// The listing is rotated by seven on every read, so no read agrees with the one before it. Every
/// item is still shown exactly once and the walk ends.
#[test]
fn a_hub_that_reshuffles_on_every_read_is_walked_to_the_end() {
    let _serial = plx_base::testlock::serial();
    let shown = walk_live_listing(70, |_, listing| listing.rotate_left(7));
    let mut got = shown.clone();
    got.sort();
    let mut expected: Vec<String> = (12..70).map(|i| plain(1, i)).collect();
    expected.sort();
    assert_eq!(got, expected, "every tail item exactly once");
}

/// A frame captures its views before its landings are delivered, so the tick after a window's
/// landing can ask from the window it replaced. Honoured, it would slide the NEW window with focus
/// in the middle of it. The ask names the window it was read from and the store refuses one for a
/// window it has since slid; and when every landing meets one, the row still reaches its end.
#[test]
fn an_ask_computed_from_a_window_the_row_has_since_slid_is_refused() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 112)], plain);
    let mc = related_response(&[(1, 12, 112, true)], "");
    open(&server, &mc);
    let mut reached = std::collections::BTreeSet::new();
    while detail().related_tail.more {
        let stale = (detail().related_tail.offset, detail().related_tail.end);
        slide(false);
        reached.extend(window().into_iter().map(|(_, rk)| rk));
        let at = (detail().related_tail.offset, detail().related_tail.end);
        assert_ne!(stale, at);
        assert!(!want_related(test_state(), test_adapter(), false, stale), "the window it names is gone");
        assert!(!test_adapter().rel_pages.lock().unwrap().inflight, "and no read went out");
        assert_eq!((detail().related_tail.offset, detail().related_tail.end), at);
    }
    assert_eq!(reached.len(), 112 - 12, "every tail item was reached");
}

/// Waits for the Related read that is out to finish.
fn wait_for_related_read() {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while test_adapter().rel_pages.lock().unwrap().inflight && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(!test_adapter().rel_pages.lock().unwrap().inflight, "the read finished");
}

/// Focus left the edge a window was asked from while the read was out: the page withdraws the ask,
/// and the answer that arrives afterwards must not install. The read cannot be stopped, so the row
/// is asked again only once it has finished: no second read goes out beside it.
#[test]
fn a_withdrawn_read_never_installs_its_window() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 112)], plain);
    let mc = related_response(&[(1, 12, 112, true)], "");
    open(&server, &mc);
    let at = || (detail().related_tail.offset, detail().related_tail.end);
    let was = at();
    assert!(want_related(test_state(), test_adapter(), false, was), "a request starts");
    cancel_related(test_adapter());
    assert!(test_adapter().rel_pages.lock().unwrap().inflight, "the worker is still reading, so the latch is held");
    assert!(!want_related(test_state(), test_adapter(), true, was) || at() == was, "an ask for another direction starts nothing beside it");
    wait_for_related_read();
    assert!(!pump_related_pages(test_state(), test_adapter()), "nothing landed to install");
    assert_eq!(at(), was);
    slide(false);
    assert_ne!(at(), was, "and the next ask slides the window");
}

/// Rocking across the edge: ask, withdraw, ask the same window again while the first read is still
/// out. One read answers both, and its window installs.
#[test]
fn an_ask_for_the_window_a_withdrawn_read_is_reading_adopts_that_read() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 112)], plain);
    let mc = related_response(&[(1, 12, 112, true)], "");
    open(&server, &mc);
    let at = || (detail().related_tail.offset, detail().related_tail.end);
    let was = at();
    assert!(want_related(test_state(), test_adapter(), false, was), "a request starts");
    cancel_related(test_adapter());
    assert!(!want_related(test_state(), test_adapter(), false, was), "no second read starts");
    wait_for_related_read();
    assert!(pump_related_pages(test_state(), test_adapter()), "the adopted read's window installs");
    assert_ne!(at(), was);
}

/// A read has answered and its window waits for the next pump to install it. An edge asked in
/// between (a key repeat re-arms the shelf) still names the window on the page, so `seen` passes:
/// it must not read the same page a second time.
#[test]
fn an_edge_asked_while_a_landing_waits_to_install_reads_nothing_twice() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 112)], plain);
    let mc = related_response(&[(1, 12, 112, true)], "");
    open(&server, &mc);
    let at = || (detail().related_tail.offset, detail().related_tail.end);
    let was = at();
    assert!(want_related(test_state(), test_adapter(), false, was), "a request starts");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while test_adapter().rel_pages.lock().unwrap().landed.is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(at(), was, "landed, not yet installed");
    assert!(!want_related(test_state(), test_adapter(), false, was), "the answer to this edge is already here");
    assert!(pump_related_pages(test_state(), test_adapter()));
    assert_ne!(at(), was);
}

/// The page for the SAME item is read again (a return from the player, a watched toggle): the
/// fresh detail holds only the head page, but the user stands deep in the row. The window, its
/// positions and its ledger carry over, so the card under the focus is still in the row at the
/// place it was; a read that finds other hubs, or another item, keeps nothing of it.
#[test]
fn a_reread_of_the_same_item_keeps_the_window_the_row_had_slid_to() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 312)], plain);
    let mc = related_response(&[(1, 12, 312, true)], "");
    open(&server, &mc);
    for _ in 0..8 { slide(false); }
    let (before, tail_before) = (window(), detail().related_tail.clone());
    assert!(tail_before.offset >= 60, "the row is walked well past the head: {}", tail_before.offset);
    let sid = detail().sid;
    let land = |rk: &str, fresh: Detail| {
        let gen = begin_detail_for_test(test_adapter(), sid, rk);
        land_detail_for_test(test_state(), test_adapter(), sid, rk, gen, Some(fresh))
    };
    let fresh = |rk: &str| {
        let rows = related_rows(&mc, sid, "page");
        Detail { sid, rk: rk.into(), related: rows.related, related_tail: rows.tail, ..Default::default() }
    };
    assert!(land("page", fresh("page")));
    assert_eq!(window(), before, "the same cards stand at the same tail positions");
    let t = &detail().related_tail;
    assert_eq!((t.offset, t.end, t.total, t.more), (tail_before.offset, tail_before.end, tail_before.total, tail_before.more));
    assert_eq!(t.row, tail_before.row, "and the ledger");
    assert_eq!(detail().related.len(), t.head + before.len(), "no more than the head and one window");
    // the carried window still slides on from where it stands
    slide(false);
    assert!(detail().related_tail.offset > tail_before.offset);
    // hubs the window was not read from: nothing of it survives
    let mut moved = fresh("page");
    moved.related_tail.hubs[0].key = "/library/metadata/9/similar".into();
    assert!(land("page", moved));
    assert_eq!((detail().related_tail.offset, detail().related.len()), (0, detail().related_tail.head));
}

/// Another item's page starts at its own head, whatever window the last one stood at.
#[test]
fn a_read_of_another_item_drops_the_window() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 312)], plain);
    let mc = related_response(&[(1, 12, 312, true)], "");
    open(&server, &mc);
    for _ in 0..3 { slide(false); }
    assert!(detail().related_tail.offset > 0);
    let sid = detail().sid;
    let rows = related_rows(&mc, sid, "other");
    let next = Detail { sid, rk: "other".into(), related: rows.related, related_tail: rows.tail, ..Default::default() };
    let gen = begin_detail_for_test(test_adapter(), sid, "other");
    assert!(land_detail_for_test(test_state(), test_adapter(), sid, "other", gen, Some(next)));
    assert_eq!((detail().related_tail.offset, detail().related.len()), (0, detail().related_tail.head));
}

/// A page read afresh holds only the head, and its focus was last seen deep in the row: the store
/// opens the window at that tail position without walking to it, in place of the one it holds, and
/// the row can slide on from there in both directions.
#[test]
fn a_seek_opens_the_window_at_a_tail_position_and_the_row_slides_on_from_it() {
    let _serial = plx_base::testlock::serial();
    let server = serve(vec![(1, 312)], plain);
    let mc = related_response(&[(1, 12, 312, true)], "");
    open(&server, &mc);
    let seek = |at: usize, seen: (usize, usize)| ask_related(test_state(), test_adapter(), RelatedAsk::Seek(at), seen);
    let seen = (detail().related_tail.offset, detail().related_tail.end);
    // an ask from a window the store has replaced is refused, as an edge's is
    assert!(!seek(150, (seen.0 + 1, seen.1)));
    assert!(seek(150, seen), "a seek starts");
    assert!(!seek(150, seen), "and only one read is out");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while detail().related_tail.offset == 0 && std::time::Instant::now() < until {
        pump_related_pages(test_state(), test_adapter());
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let t = &detail().related_tail;
    assert_eq!((t.offset, t.end), (138, 162), "half a window before the position, one window long");
    assert_eq!(window().iter().find(|(position, _)| *position == 150).map(|(_, rk)| rk.as_str()), Some("1-162"),
        "tail position 150 is listing item 12 + 150");
    assert!(detail().related_tail.row.ledger.is_none(), "the window was opened on the listing alone");
    slide(false);
    assert!(detail().related_tail.offset > 138);
    slide(true);
    slide(true);
    assert!(detail().related_tail.offset < 138);
}

/// The place a Related card is named by in a restore survives the encoding.
#[test]
fn a_spot_names_a_tail_card_by_its_position_and_a_head_card_by_its_index() {
    let tail = Spot { section: Spot::RELATED, col: Spot::tail_col(150), ..Default::default() };
    assert_eq!(tail.tail_position(), Some(150));
    assert_eq!(Spot { section: Spot::RELATED, col: Spot::tail_col(0), ..Default::default() }.tail_position(), Some(0));
    assert_eq!(Spot { section: Spot::RELATED, col: 2, ..Default::default() }.tail_position(), None);
    // another section's negative column (the collection heading) is not a Related position
    assert_eq!(Spot { section: 7, col: -1, ..Default::default() }.tail_position(), None);
    assert_eq!(Spot { section: 7, col: Spot::tail_col(3), ..Default::default() }.tail_position(), None);
}
