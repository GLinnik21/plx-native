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

/// Ask for the next (or previous) tail window and wait for it to land.
fn slide(before: bool) {
    let was = (detail().related_tail.offset, detail().related_tail.end);
    assert!(want_related(test_state(), test_adapter(), before), "a request starts");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while (detail().related_tail.offset, detail().related_tail.end) == was && std::time::Instant::now() < until {
        pump_related_pages(test_state(), test_adapter());
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_ne!((detail().related_tail.offset, detail().related_tail.end), was, "the window never moved");
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
