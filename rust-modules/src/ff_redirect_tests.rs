//! Media opens that meet an HTTP redirect.
//!
//! Field report (debug build of a788b27a): a Plex-hosted trailer's Part URL answered
//! `302`, the socket open reported `ff: http_open FAILED status=302`, and the demuxer saw no
//! access units. PMS redirects those parts to a presigned CDN URL on ANOTHER origin, usually
//! https, sometimes via a relative `Location`. These grade every plaintext media open the demux
//! thread makes — the progressive first open, the AVIO seek reopen, and the HLS source open —
//! against loopback servers that answer the way PMS did.

use super::*;

/// A loopback server that answers each connection with ONE scripted response and closes it,
/// recording the raw bytes each request arrived with (a request head, or — for a client that
/// opened TLS on it — the start of a ClientHello). Sequential by design: every client here is.
struct Scripted {
    port: u16,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    stop: std::sync::Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Scripted {
    fn start(reply: impl Fn(&str, u16) -> Vec<u8> + Send + 'static) -> Scripted {
        use std::io::{Read, Write};
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).expect("nonblocking listener");
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (seen_t, stop_t) = (seen.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stop_t.load(Ordering::Acquire) {
                let mut s = match srv.accept() {
                    Ok((s, _)) => s,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        continue;
                    }
                    Err(_) => return,
                };
                // macOS hands the accepted socket the listener's O_NONBLOCK.
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(std::time::Duration::from_millis(300)));
                let mut raw = Vec::new();
                let mut chunk = [0u8; 2048];
                while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => raw.extend_from_slice(&chunk[..n]),
                    }
                }
                let complete = raw.windows(4).any(|w| w == b"\r\n\r\n");
                let head = String::from_utf8_lossy(&raw).into_owned();
                seen_t.lock().unwrap().push(raw);
                if complete {
                    let _ = s.write_all(&reply(&head, port));
                    let _ = s.flush();
                }
            }
        });
        Scripted {
            port,
            seen,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<Vec<u8>> {
        self.seen.lock().unwrap().clone()
    }

    fn heads(&self) -> Vec<String> {
        self.requests()
            .iter()
            .map(|r| String::from_utf8_lossy(r).into_owned())
            .collect()
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn redirect(status: u16, location: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status} Moved\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
    .into_bytes()
}

fn ok_body(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn not_found() -> Vec<u8> {
    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
}

fn request_line(head: &str) -> &str {
    head.split("\r\n").next().unwrap_or("")
}

fn hls_resource(port: u16) -> crate::hls::Resource {
    crate::hls::Resource {
        origin: crate::plex::Origin::http("127.0.0.1", port as i32),
        path: "/unused".into(),
    }
}

fn hls_open_plain(
    port: u16,
    request_path: &str,
) -> (Result<(String, i64), String>, Box<HttpStream>) {
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut net = HlsNet {
        hs: &mut *hs,
        curl: None,
    };
    let opened = hls_open_source(
        &hls_resource(port),
        request_path,
        &mut *aq,
        &mut net,
        None,
        &mut crate::checkpoint::NoCheckpoint,
    );
    let outcome = match opened {
        Ok((Src::Socket { path, .. }, size, _)) => {
            Ok((path.to_string_lossy().into_owned(), size))
        }
        Ok((Src::Curl(_), size, _)) => Ok(("<curl>".into(), size)),
        Ok((Src::Idle, _, _)) => Err("idle".into()),
        Err(e) => Err(format!("{e:?}")),
    };
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    (outcome, hs)
}

/// A progressive open through the production helper, reduced to where it landed.
fn progressive_open(port: u16, path: &str) -> Result<(String, i64), String> {
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let origin = crate::plex::Origin::http("127.0.0.1", port as i32);
    let opened = open_plain_progressive(&mut *hs, &origin, path, &mut *aq);
    let outcome = match opened {
        Ok((Src::Socket { path, .. }, size)) => Ok((path.to_string_lossy().into_owned(), size)),
        Ok((Src::Curl(_), size)) => Ok(("<curl>".into(), size)),
        Ok((Src::Idle, _)) => Err("idle".into()),
        Err(e) => Err(format!("{e:?}")),
    };
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    outcome
}

#[test]
fn an_hls_open_follows_an_absolute_same_origin_302_and_keeps_the_token() {
    let _serial = crate::testlock::serial();
    let pms = Scripted::start(|head, port| {
        if request_line(head).starts_with("GET /start") {
            redirect(302, &format!("http://127.0.0.1:{port}/final/seg.ts"))
        } else if request_line(head).starts_with("GET /final/seg.ts") {
            ok_body("ABCD")
        } else {
            not_found()
        }
    });
    let (outcome, _hs) = hls_open_plain(pms.port, "/start?X-Plex-Token=tok");
    assert_eq!(
        outcome,
        Ok(("/final/seg.ts?X-Plex-Token=tok".to_string(), 4)),
        "the open must land on the Location, and a later reopen must use it"
    );
    let heads = pms.heads();
    assert_eq!(heads.len(), 2, "one redirect, one final GET: {heads:?}");
    assert!(
        request_line(&heads[1]).starts_with("GET /final/seg.ts?X-Plex-Token=tok "),
        "a same-origin hop keeps the credential: {}",
        request_line(&heads[1])
    );
}

#[test]
fn a_seek_reopen_follows_a_relative_302_and_keeps_its_range() {
    let _serial = crate::testlock::serial();
    let pms = Scripted::start(|head, _port| {
        let line = request_line(head);
        if line.starts_with("GET /dir/a.mp4 ") {
            redirect(307, "b.mp4")
        } else if line.starts_with("GET /dir/b.mp4 ") {
            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 2-3/4\r\nContent-Length: 2\r\nConnection: close\r\n\r\nCD".to_vec()
        } else {
            not_found()
        }
    });
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut state = AvioState {
        src: Src::Socket {
            hs: &mut *hs,
            host: CString::new("127.0.0.1").unwrap(),
            port: pms.port as c_int,
            path: CString::new("/dir/a.mp4").unwrap(),
        },
        aq: &mut *aq,
        off: 0,
        size: 4,
        io_failed: false,
        body_active_us: 0,
        body_bytes: 0,
        first_byte_at: None,
        reserve_deadline: ReserveDeadlineState::new(None, false),
        transport_watchdog: None,
        acquisition: None,
        bounce: Vec::new(),
        bounce_pos: 0,
    };
    let at = seek_cb(&mut state as *mut AvioState as *mut c_void, 2, SEEK_SET);
    let kept_path = match &state.src {
        Src::Socket { path, .. } => path.to_string_lossy().into_owned(),
        _ => "<not a socket>".into(),
    };
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    assert_eq!(
        at, 2,
        "the seek must follow the 307 and land at the requested byte"
    );
    let heads = pms.heads();
    assert_eq!(heads.len(), 2, "{heads:?}");
    assert!(
        heads[1].contains("Range: bytes=2-"),
        "the Range is not a credential and must survive the hop: {}",
        heads[1]
    );
    assert_eq!(
        kept_path, "/dir/b.mp4",
        "later seeks start from the effective URL"
    );
}

#[test]
fn a_cross_origin_hop_does_not_forward_the_token() {
    let _serial = crate::testlock::serial();
    let cdn = Scripted::start(|_head, _port| ok_body("ABCD"));
    let cdn_port = cdn.port;
    let pms = Scripted::start(move |_head, _port| {
        redirect(
            302,
            &format!("http://127.0.0.1:{cdn_port}/cdn/v.ts?sig=abc"),
        )
    });
    let outcome = progressive_open(pms.port, "/start?X-Plex-Token=tok");
    assert_eq!(outcome, Ok(("/cdn/v.ts?sig=abc".to_string(), 4)));
    let heads = cdn.heads();
    assert_eq!(heads.len(), 1, "{heads:?}");
    assert!(
        request_line(&heads[0]).starts_with("GET /cdn/v.ts?sig=abc "),
        "the presigned Location is requested verbatim: {}",
        request_line(&heads[0])
    );
    assert!(
        !heads[0].contains("tok"),
        "the PMS token must not reach another origin: {}",
        heads[0]
    );
}

#[test]
fn a_redirect_loop_is_bounded_and_fails() {
    let _serial = crate::testlock::serial();
    let pms = Scripted::start(|_head, _port| redirect(302, "/loop"));
    let (outcome, _hs) = hls_open_plain(pms.port, "/start");
    assert!(outcome.is_err(), "a loop must fail: {outcome:?}");
    assert_eq!(
        pms.requests().len(),
        1 + crate::stream::redirect::MAX_HOPS as usize,
        "the first request plus exactly MAX_HOPS followed hops"
    );
}

#[test]
fn a_progressive_open_follows_a_302() {
    let _serial = crate::testlock::serial();
    let pms = Scripted::start(|head, _port| {
        if request_line(head).starts_with("GET /library/parts/1/file.mp4") {
            redirect(302, "/services/iva/assets/1/video.mp4")
        } else {
            ok_body("ABCDEFGH")
        }
    });
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let origin = crate::plex::Origin::http("127.0.0.1", pms.port as i32);
    let opened = open_plain_progressive(
        &mut *hs,
        &origin,
        "/library/parts/1/file.mp4?X-Plex-Token=tok",
        &mut *aq,
    );
    let outcome = match opened {
        Ok((Src::Socket { path, .. }, size)) => Some((path.to_string_lossy().into_owned(), size)),
        _ => None,
    };
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    assert_eq!(
        outcome,
        Some((
            "/services/iva/assets/1/video.mp4?X-Plex-Token=tok".to_string(),
            8
        ))
    );
}

/// An https hop cannot be spoken by the socket client; it must be handed to libcurl. The
/// loopback "CDN" speaks no TLS, so the open fails — what grades the routing is that the bytes
/// that reached it are a TLS ClientHello (record type 0x16), not a plaintext `GET`.
#[test]
fn an_https_hop_from_the_socket_path_is_routed_to_curl() {
    let Some(_serial) = test_support::curl_gate() else {
        return;
    };
    let cdn = Scripted::start(|_head, _port| Vec::new());
    let cdn_port = cdn.port;
    let pms = Scripted::start(move |_head, _port| {
        redirect(
            302,
            &format!("https://127.0.0.1:{cdn_port}/cdn/seg.ts?sig=abc"),
        )
    });
    let outcome = progressive_open(pms.port, "/start?X-Plex-Token=tok");
    assert!(
        outcome.is_err(),
        "a plaintext server cannot finish TLS: {outcome:?}"
    );
    let seen = cdn.requests();
    assert_eq!(seen.len(), 1, "exactly one dial to the https target");
    assert_eq!(
        seen[0].first().copied(),
        Some(0x16),
        "the https hop must open TLS (curl), not send plaintext: {:?}",
        String::from_utf8_lossy(&seen[0])
    );
}

/// Review finding: a redirected master playlist was parsed against the URL it was REQUESTED
/// from, so `/old/master.m3u8` -> `/new/master.m3u8` with a relative child `variant.m3u8` asked
/// for `/old/variant.m3u8`. Children resolve against where the playlist actually came from.
#[test]
fn a_redirected_master_playlist_resolves_its_children_against_the_redirect_target() {
    let _serial = crate::testlock::serial();
    let pms = Scripted::start(|head, _port| {
        if request_line(head).starts_with("GET /old/master.m3u8") {
            redirect(302, "/new/master.m3u8")
        } else if request_line(head).starts_with("GET /new/master.m3u8") {
            ok_body("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvariant.m3u8\n")
        } else {
            not_found()
        }
    });
    let mut hs = crate::stream::http_stream_boxed();
    let mut aq = crate::aq::aq_new(1 << 20);
    let mut net = HlsNet {
        hs: &mut *hs,
        curl: None,
    };
    let origin = crate::plex::Origin::http("127.0.0.1", pms.port as i32);
    let cursor = hls_cursor_open(
        &origin,
        "/old/master.m3u8?X-Plex-Token=tok",
        &mut *aq,
        &mut net,
        false,
        None,
    );
    let media = cursor
        .as_ref()
        .map(|c| c.media.path.clone())
        .map_err(|e| format!("{e:?}"));
    crate::stream::http_close(&mut *hs);
    crate::aq::aq_destroy(&mut *aq);
    assert_eq!(media, Ok("/new/variant.m3u8".to_string()));
}

/// The HLS contract (`hls.rs`): every request stays on the PMS origin, so the media worker can
/// never be steered into fetching from elsewhere. A redirect off that origin is refused before
/// anything is dialled there.
#[test]
fn an_hls_redirect_off_the_pms_origin_is_refused_before_it_is_dialled() {
    let _serial = crate::testlock::serial();
    let cdn = Scripted::start(|_head, _port| ok_body("ABCD"));
    let cdn_port = cdn.port;
    let pms = Scripted::start(move |_head, _port| {
        redirect(302, &format!("http://127.0.0.1:{cdn_port}/cdn/v.ts?sig=abc"))
    });
    let (outcome, _hs) = hls_open_plain(pms.port, "/start?X-Plex-Token=tok");
    assert!(outcome.is_err(), "{outcome:?}");
    assert!(cdn.requests().is_empty(), "nothing may reach another origin");
}
