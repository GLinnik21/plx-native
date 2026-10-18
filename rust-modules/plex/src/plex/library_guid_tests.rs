//! `Client::find_by_guids`: the request shape (pure) and the answer handling (loopback fixture).

use super::*;

fn guid(kind: &str, n: usize) -> String {
    format!("plex://{kind}/{n:024x}")
}

fn guids(kind: &str, n: usize) -> Vec<String> {
    (0..n).map(|i| guid(kind, i)).collect()
}

fn query_param<'a>(path: &'a str, key: &str) -> Option<&'a str> {
    path.split_once('?')?.1.split('&').find_map(|p| p.strip_prefix(key)?.strip_prefix('='))
}

#[test]
fn empty_input_makes_no_request() {
    assert!(guid_lookup_paths(GuidKind::Movie, &[]).is_empty());
    // Nothing but non-plex guids is also nothing to ask.
    let only_foreign = vec!["imdb://tt0083658".to_string(), "tmdb://78".to_string(), String::new()];
    assert!(guid_lookup_paths(GuidKind::Show, &only_foreign).is_empty());
}

#[test]
fn type_param_follows_the_kind() {
    let g = vec![guid("movie", 1)];
    let m = guid_lookup_paths(GuidKind::Movie, &g);
    let s = guid_lookup_paths(GuidKind::Show, &g);
    assert_eq!(query_param(&m[0].0, "type"), Some("1"));
    assert_eq!(query_param(&s[0].0, "type"), Some("2"));
    assert!(m[0].0.starts_with("/library/all?"));
}

#[test]
fn chunks_at_forty_guids_per_request() {
    for (n, expect) in [(1, vec![1]), (40, vec![40]), (41, vec![40, 1]), (80, vec![40, 40]), (95, vec![40, 40, 15])] {
        let chunks = guid_lookup_paths(GuidKind::Movie, &guids("movie", n));
        let sizes: Vec<usize> = chunks.iter().map(|(_, c)| c.len()).collect();
        assert_eq!(sizes, expect, "{n} guids");
        for (path, chunk) in &chunks {
            assert_eq!(query_param(path, "guid").unwrap().split(',').count(), chunk.len());
        }
    }
    assert_eq!(GUIDS_PER_REQUEST, 40);
}

#[test]
fn guids_are_comma_joined_and_percent_encoded() {
    let g = vec![guid("movie", 1), guid("movie", 2)];
    let (path, chunk) = &guid_lookup_paths(GuidKind::Movie, &g)[0];
    let value = query_param(path, "guid").unwrap();
    assert_eq!(value.split(',').count(), 2, "{path}");
    assert!(value.split(',').all(|v| v.starts_with("plex%3A%2F%2Fmovie%2F")), "{path}");
    assert_eq!(chunk, &g);
    // One guid param, not repeated (PMS keeps only the last of a repeated key).
    assert_eq!(path.matches("guid=").count(), 1);
}

#[test]
fn non_plex_and_duplicate_guids_are_dropped_in_order() {
    let g = vec![
        "imdb://tt0083658".to_string(),
        guid("movie", 2),
        "com.plexapp.agents.imdb://tt1?lang=en".to_string(),
        guid("movie", 1),
        guid("movie", 2),
        "plex://".to_string(),
    ];
    let chunks = guid_lookup_paths(GuidKind::Movie, &g);
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].1, vec![guid("movie", 2), guid("movie", 1)]);
}

#[test]
fn no_request_carries_a_token() {
    for (path, _) in guid_lookup_paths(GuidKind::Show, &guids("show", 3)) {
        assert!(!path.contains("X-Plex-Token"), "{path}");
    }
}

#[test]
fn unreachable_server_is_unanswered_not_absent() {
    let client = Client::new(
        crate::plex::ServerId::UNSET, "fixture",
        crate::plex::Origin::http("127.0.0.1", 9), "", "cid",
    );
    client.disable_data_io();
    assert_eq!(client.find_by_guids(GuidKind::Movie, &guids("movie", 2)), Err(GuidLookupError::Unanswered));
    assert_eq!(client.denied_data_requests(), 1);
    // Nothing to ask means nothing to fail.
    assert_eq!(client.find_by_guids(GuidKind::Movie, &[]), Ok(vec![]));
    assert_eq!(client.denied_data_requests(), 1, "empty input must not reach the transport");
}

#[cfg(feature = "devtriggers")]
mod loopback {
    use super::*;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    /// Serves `replies` in order, one per connection; returns the request lines it saw.
    fn serve(replies: Vec<(&'static str, String)>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(3);
            let mut replies = replies.into_iter();
            while Instant::now() < deadline {
                let Ok((mut socket, _)) = plx_base::testnet::accept(&listener) else {
                    std::thread::sleep(Duration::from_millis(5));
                    continue;
                };
                socket.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
                let mut buf = [0; 16384];
                let n = socket.read(&mut buf).unwrap();
                let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                seen.push(text.lines().next().unwrap_or_default().to_string());
                let Some((status, body)) = replies.next() else { break };
                let _ = write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                if replies.len() == 0 { break; }
            }
            seen
        });
        (port, handle)
    }

    fn client(port: u16) -> Client {
        Client::new(
            crate::plex::ServerId::UNSET, "fixture",
            crate::plex::Origin::http("127.0.0.1", port as i32), "", "cid",
        )
    }

    fn row(guid: &str, key: &str, section: i64) -> String {
        format!(r#"{{"guid":"{guid}","ratingKey":"{key}","librarySectionID":{section},"title":"t"}}"#)
    }

    #[test]
    fn held_guids_come_back_with_keys_and_absent_ones_are_missing() {
        let g = guids("movie", 3);
        let body = format!(
            r#"{{"MediaContainer":{{"size":2,"Metadata":[{},{},{}]}}}}"#,
            row(&g[0], "101", 4),
            row(&g[2], "103", 0),
            row("plex://movie/ffffffffffffffffffffffff", "999", 4), // never asked for
        );
        let (port, server) = serve(vec![("200 OK", body)]);
        let got = client(port).find_by_guids(GuidKind::Movie, &g).unwrap();
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].starts_with("GET /library/all?type=1&guid="), "{}", seen[0]);
        assert_eq!(got, vec![
            GuidMatch { guid: g[0].clone(), rating_key: "101".into(), library_section_id: Some(4), blur: None },
            GuidMatch { guid: g[2].clone(), rating_key: "103".into(), library_section_id: None, blur: None },
        ]);
    }

    #[test]
    fn forty_one_guids_make_two_requests_and_a_failed_chunk_is_unanswered() {
        let ok = r#"{"MediaContainer":{"size":0}}"#.to_string();
        let (port, server) = serve(vec![("200 OK", ok), ("500 Internal Server Error", "{}".to_string())]);
        let r = client(port).find_by_guids(GuidKind::Show, &guids("show", 41));
        let seen = server.join().unwrap();
        assert_eq!(r, Err(GuidLookupError::Unanswered));
        assert!(seen.len() >= 2, "{seen:?}");
        assert!(seen[0].contains("type=2"), "{}", seen[0]);
    }
}
