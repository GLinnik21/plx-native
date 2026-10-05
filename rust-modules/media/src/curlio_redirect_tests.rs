//! Redirects, media plane: what `curlio::CurlSource` does when the server answers an open, a reopen
//! or a seek with a `3xx`. It follows the hops itself, one request at a time (`next_hop`). These
//! tests grade what that promises whoever relies on it, and held before it did: the hop cap, the
//! no-downgrade rule, the `Range` that rides every hop, a relative `Location`, which statuses are
//! followed, what a `3xx` body and a missing `Location` do, and what of the original URL a hop
//! receives. They also grade what only a hop-by-hop follower can promise: a hop is a request of its
//! own, to its own host, under its own TLS decision (`net::keypin`), whichever mode the host that
//! sent it was in.
//!
//! The doubles are `net::spawn_scripted`'s: each records every request it reads, so a test can say
//! what each hop received. Hosts are loopback ones and the synthetic `*.plex.direct` names of
//! `curlio_roots_tests.rs`, which reach loopback through a resolve pin; a hop to a name no pin
//! covers would need DNS, and no test here may. Every test holds `testlock::serial()`: the CA
//! override and the roots bundle are process-global.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use plx_net::net::origin::ResolvePin;
use plx_net::net::{curl_ready, keypin, mint_ca_issued_cert, mint_cert, resolve, spawn_scripted, ymd_from_now, Observed, Reply, TestCaGuard, TestCert};

use crate::curlio::{CurlSource, OpenErr};

const PLEX_DIRECT: &str = "127-0-0-1.0123456789abcdef0123456789abcdef.plex.direct";
const SECOND_PLEX_DIRECT: &str = "127-0-0-1.fedcba9876543210fedcba9876543210.plex.direct";
const TOKEN: &str = "test-token-0123456789";

fn media_body() -> Vec<u8> {
    (0..5000u32).map(|i| (i % 253) as u8).collect()
}

/// A leaf for `names` from a fresh CA; `pem` of the result is that CA.
fn leaf(names: &[&str]) -> Arc<TestCert> {
    Arc::new(mint_ca_issued_cert(names, ymd_from_now(-1), ymd_from_now(30)))
}

fn pin_to_loopback(host: &str, port: u16) {
    resolve::add(&ResolvePin::for_test(host, i32::from(port), std::net::IpAddr::from([127, 0, 0, 1])));
}

/// Clears what a test published about `host:port` in `net::keypin`'s tables when it ends.
fn watch(host: &str, port: u16) -> keypin::Scoped {
    keypin::Scoped::watch(&keypin::key_of(host, i32::from(port)))
}

fn url_of(host: &str, port: u16, path: &str) -> String {
    format!("https://{host}:{port}{path}")
}

/// Every request `served` has read, verbatim, in arrival order.
fn requests(served: &Observed) -> Vec<String> {
    served
        .requests
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .map(|r| String::from_utf8_lossy(r).into_owned())
        .collect()
}

/// The request line of each request: `GET /path?query HTTP/1.1`.
fn request_lines(served: &Observed) -> Vec<String> {
    requests(served).iter().map(|r| r.lines().next().unwrap_or_default().to_owned()).collect()
}

/// A header's value in one recorded request, the name matched case-insensitively.
fn header(request: &str, name: &str) -> Option<String> {
    request.lines().find_map(|l| {
        let (n, v) = l.split_once(':')?;
        n.eq_ignore_ascii_case(name).then(|| v.trim().to_owned())
    })
}

fn read_to_end(src: &mut CurlSource) -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = src.read(&mut buf);
        assert!(n >= 0, "read failed ({n}) after {} bytes", out.len());
        if n == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..n as usize]);
    }
}

/// A plaintext listener that counts the connections it accepts and answers none of them.
struct Plain {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

impl Plain {
    fn spawn() -> Plain {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind plaintext listener");
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stream.is_ok() {
                    count.fetch_add(1, Ordering::AcqRel);
                }
            }
        });
        Plain { port, accepted }
    }

    /// How many connections this listener has accepted besides the one this call makes to prove
    /// it is accepting at all: a bare count of zero would also be what a dead listener reports.
    fn connections_but_a_probe(&self) -> usize {
        let _probe = std::net::TcpStream::connect(("127.0.0.1", self.port)).expect("the plaintext listener is up");
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while self.accepted.load(Ordering::Acquire) == 0 && std::time::Instant::now() < give_up {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Connections queue in the order they arrive, so one made before the probe is counted by
        // the time the probe is; this is the time for the thread to count both.
        std::thread::sleep(std::time::Duration::from_millis(100));
        self.accepted.load(Ordering::Acquire).saturating_sub(1)
    }
}

#[test]
fn a_redirect_from_https_to_plain_http_is_refused_and_the_plain_target_is_never_dialled() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-downgrade");
    let plain = Plain::spawn();
    let served = spawn_scripted(cert, [("/start", Reply::redirect(302, &format!("http://127.0.0.1:{}/video.mkv?X-Plex-Token={TOKEN}", plain.port)))]);
    let _watch = watch("127.0.0.1", served.port);

    let err = CurlSource::open(&url_of("127.0.0.1", served.port, "/start"), 0)
        .err()
        .expect("a TLS media request may not be redirected into plaintext");
    // CURLE_UNSUPPORTED_PROTOCOL: a hop from an https URL may only be https (`next_hop`).
    assert_eq!(err, OpenErr::Transport(1));
    assert_eq!(request_lines(&served), ["GET /start HTTP/1.1"], "the redirecting server was asked once");
    assert_eq!(plain.connections_but_a_probe(), 0, "the plaintext target was never connected to");
}

/// `/h0` redirects to `/h1` and so on, `hops` times, and `/h<hops>` is the media.
fn chain(hops: usize) -> Vec<(String, Reply)> {
    let mut routes: Vec<(String, Reply)> = (0..hops).map(|i| (format!("/h{i}"), Reply::redirect(302, &format!("/h{}", i + 1)))).collect();
    routes.push((format!("/h{hops}"), Reply::ok(media_body())));
    routes
}

/// A plaintext HTTP/1.1 server that keeps its connections alive, answers a request by its path with
/// the response head and body `routes` holds for it (the body a moment AFTER the head, as a server
/// that is still producing it sends it), and counts the connections it accepts: the double in which a
/// connection libcurl kept after a redirect can be seen being handed to the next hop.
/// (`spawn_scripted` closes every connection, which is what makes its accept counts one per request.)
struct KeepAlive {
    port: u16,
    accepted: Arc<AtomicUsize>,
}

impl KeepAlive {
    fn spawn(routes: Vec<(&'static str, Vec<u8>, Vec<u8>)>) -> KeepAlive {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind keep-alive listener");
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&accepted);
        let routes = Arc::new(routes);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                count.fetch_add(1, Ordering::AcqRel);
                let routes = Arc::clone(&routes);
                std::thread::spawn(move || {
                    let mut seen = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        while let Some(end) = seen.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&seen[..end]).into_owned();
                            seen.drain(..end + 4);
                            let path = head.split_whitespace().nth(1).unwrap_or_default().split('?').next().unwrap_or_default().to_owned();
                            let (reply_head, reply_body) = routes
                                .iter()
                                .find(|(p, _, _)| *p == path)
                                .map(|(_, h, b)| (h.clone(), b.clone()))
                                .unwrap_or_else(|| (b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(), Vec::new()));
                            let sent = stream.write_all(&reply_head).and_then(|_| stream.flush()).map(|_| std::thread::sleep(std::time::Duration::from_millis(50)));
                            if sent.and_then(|_| stream.write_all(&reply_body)).is_err() {
                                return;
                            }
                        }
                        match stream.read(&mut buf) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => seen.extend_from_slice(&buf[..n]),
                        }
                    }
                });
            }
        });
        KeepAlive { port, accepted }
    }
}

/// **A redirect's connection is not wasted.** The hop is left only after its body has been read to its
/// end, so libcurl keeps the connection, and the next request to the same server finds it in the
/// source's connection cache: one connection for the redirect and the media, where a hop left
/// mid-body would cost a second.
#[test]
fn the_connection_a_redirect_arrived_on_carries_the_next_hop_when_its_body_was_read() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let note = b"moved, look over there. ".repeat(25);
    let redirect = format!("HTTP/1.1 302 Found\r\nLocation: /video.mkv\r\nContent-Length: {}\r\n\r\n", note.len()).into_bytes();
    let media = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", media_body().len()).into_bytes();
    let served = KeepAlive::spawn(vec![("/start", redirect, note), ("/video.mkv", media, media_body())]);

    let mut src = CurlSource::open(&format!("http://127.0.0.1:{}/start", served.port), 0).expect("the redirect is followed");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());
    assert_eq!(served.accepted.load(Ordering::Acquire), 1, "the hop reused the redirect's connection");
}

#[test]
fn a_chain_of_five_redirects_ends_in_the_media() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-five");
    let served = spawn_scripted(cert, chain(5));
    let _watch = watch("127.0.0.1", served.port);

    let mut src = CurlSource::open(&url_of("127.0.0.1", served.port, "/h0"), 0).expect("five redirects are within the cap");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());
    assert_eq!(request_lines(&served).len(), 6, "the open and its five hops: {:?}", request_lines(&served));
}

#[test]
fn a_chain_of_six_redirects_fails_as_too_many_after_exactly_six_requests() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-six");
    let served = spawn_scripted(cert, chain(6));
    let _watch = watch("127.0.0.1", served.port);

    let err = CurlSource::open(&url_of("127.0.0.1", served.port, "/h0"), 0)
        .err()
        .expect("the sixth redirect is over the cap");
    // CURLE_TOO_MANY_REDIRECTS: five redirects are followed (`MAX_HOPS`).
    assert_eq!(err, OpenErr::Transport(47));
    assert_eq!(request_lines(&served).len(), 6, "the sixth answer is read, and not followed: {:?}", request_lines(&served));
}

#[test]
fn an_open_at_an_offset_and_a_seek_send_the_same_range_on_both_hops_of_a_redirect() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-range");
    let target = spawn_scripted(Arc::clone(&cert), [("/video.mkv", Reply::ok(media_body()))]);
    let redirecting = spawn_scripted(cert, [("/start", Reply::redirect(302, &url_of("127.0.0.1", target.port, "/video.mkv")))]);
    let (_w1, _w2) = (watch("127.0.0.1", target.port), watch("127.0.0.1", redirecting.port));

    let mut src = CurlSource::open(&url_of("127.0.0.1", redirecting.port, "/start"), 1000).expect("the redirect is followed at an offset");
    assert_eq!(src.status(), 206);
    assert_eq!(src.size(), 5000, "the total comes from the Content-Range the last hop answered");
    let mut head = [0u8; 64];
    assert_eq!(src.read(&mut head), 64);
    assert_eq!(head[..], media_body()[1000..1064]);
    assert!(src.seek(3000), "a seek goes through the redirect again");
    assert_eq!(read_to_end(&mut src), media_body()[3000..]);

    for (hop, served) in [("the redirecting server", &redirecting), ("the target", &target)] {
        let ranges: Vec<Option<String>> = requests(served).iter().map(|r| header(r, "range")).collect();
        assert_eq!(ranges, [Some("bytes=1000-".to_owned()), Some("bytes=3000-".to_owned())], "{hop}");
    }
}

#[test]
fn a_relative_location_resolves_against_the_hop_that_sent_it() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-relative");
    // From `/a/b/next`, `../video.mkv` is `/a/video.mkv`; from the URL that was opened (`/a/start`)
    // it would be `/video.mkv`, which holds a decoy.
    let served = spawn_scripted(
        cert,
        [
            ("/a/start", Reply::redirect(302, "b/next")),
            ("/a/b/next", Reply::redirect(302, "../video.mkv?sig=abc")),
            ("/a/video.mkv", Reply::ok(media_body())),
            ("/video.mkv", Reply::ok(b"the wrong hop's base".to_vec())),
        ],
    );
    let _watch = watch("127.0.0.1", served.port);

    let mut src = CurlSource::open(&url_of("127.0.0.1", served.port, "/a/start"), 0).expect("a relative Location is followed");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());
    assert_eq!(
        request_lines(&served),
        ["GET /a/start HTTP/1.1", "GET /a/b/next HTTP/1.1", "GET /a/video.mkv?sig=abc HTTP/1.1"],
    );
}

#[test]
fn a_303_a_307_and_a_308_are_each_followed_with_a_get() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-statuses");
    for status in [303, 307, 308] {
        let served = spawn_scripted(
            Arc::clone(&cert),
            [("/start", Reply::redirect(status, "/video.mkv")), ("/video.mkv", Reply::ok(media_body()))],
        );
        let _watch = watch("127.0.0.1", served.port);
        let mut src = CurlSource::open(&url_of("127.0.0.1", served.port, "/start"), 0)
            .unwrap_or_else(|e| panic!("{status} is followed: {e:?}"));
        assert_eq!(src.status(), 200, "{status}");
        assert_eq!(read_to_end(&mut src), media_body(), "{status}");
        assert_eq!(request_lines(&served), ["GET /start HTTP/1.1", "GET /video.mkv HTTP/1.1"], "{status}");
    }
}

/// What a `302` with no `Location` is: there is nowhere to follow it to, so the `302` is the final
/// response, and the open refuses it as the status it is.
#[test]
fn a_302_with_no_location_fails_the_open_as_status_302() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-no-location");
    let served = spawn_scripted(cert, [("/start", Reply::status(302))]);
    let _watch = watch("127.0.0.1", served.port);

    let err = CurlSource::open(&url_of("127.0.0.1", served.port, "/start"), 0)
        .err()
        .expect("a redirect to nowhere is not a stream");
    assert_eq!(err, OpenErr::Status(302));
    assert_eq!(request_lines(&served), ["GET /start HTTP/1.1"], "nothing was followed");
}

/// The hop is graded like any response: a redirect into a path the server does not have is a `404`
/// open failure, not a transport one.
#[test]
fn a_redirect_to_a_path_the_server_does_not_have_fails_the_open_as_status_404() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-missing");
    let served = spawn_scripted(cert, [("/start", Reply::redirect(301, "/gone.mkv"))]);
    let _watch = watch("127.0.0.1", served.port);

    let err = CurlSource::open(&url_of("127.0.0.1", served.port, "/start"), 0)
        .err()
        .expect("the hop's 404 is the answer");
    assert_eq!(err, OpenErr::Status(404));
    assert_eq!(request_lines(&served), ["GET /start HTTP/1.1", "GET /gone.mkv HTTP/1.1"]);
}

#[test]
fn the_body_of_a_redirect_is_not_the_body_the_open_returns() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-body");
    let served = spawn_scripted(
        cert,
        [
            ("/start", Reply::redirect(302, "/video.mkv").with_body(b"moved, look over there. ".repeat(40))),
            ("/video.mkv", Reply::ok(media_body())),
        ],
    );
    let _watch = watch("127.0.0.1", served.port);

    let mut src = CurlSource::open(&url_of("127.0.0.1", served.port, "/start"), 0).expect("the redirect is followed");
    assert_eq!(src.status(), 200);
    assert_eq!(src.size(), 5000, "the size is the 200's Content-Length, not the redirect's");
    assert_eq!(read_to_end(&mut src), media_body());
}

#[test]
fn the_token_in_the_original_url_never_reaches_a_cross_host_redirect_target_but_the_user_agent_does() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    // One CA for both hosts: the household's `*.plex.direct` name and the CDN's address.
    let cert = leaf(&[PLEX_DIRECT, "127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "redirect-token");
    let cdn = spawn_scripted(Arc::clone(&cert), [("/cdn/video.mkv", Reply::ok(media_body()))]);
    let household = spawn_scripted(cert, [("/start", Reply::redirect(302, &url_of("127.0.0.1", cdn.port, "/cdn/video.mkv?sig=abc")))]);
    pin_to_loopback(PLEX_DIRECT, household.port);
    let (_w1, _w2) = (watch("127.0.0.1", cdn.port), watch(PLEX_DIRECT, household.port));

    let mut src = CurlSource::open(&url_of(PLEX_DIRECT, household.port, &format!("/start?X-Plex-Token={TOKEN}")), 0)
        .expect("the household's redirect to the CDN is followed");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());

    let asked = requests(&household);
    assert_eq!(asked.len(), 1);
    assert!(asked[0].lines().next().unwrap_or_default().contains(&format!("X-Plex-Token={TOKEN}")), "the original request carried the token: {asked:?}");
    let arrived = requests(&cdn);
    assert_eq!(arrived.len(), 1);
    assert_eq!(arrived[0].lines().next().unwrap_or_default(), "GET /cdn/video.mkv?sig=abc HTTP/1.1", "the hop is the Location, and only the Location");
    assert!(!arrived[0].to_ascii_lowercase().contains("x-plex-token") && !arrived[0].contains(TOKEN), "no trace of the token on the hop: {:?}", arrived[0]);
    let agent = plx_plex::plex::identity::user_agent();
    assert!(!agent.is_empty());
    assert_eq!(header(&asked[0], "user-agent").as_deref(), Some(agent.as_str()));
    assert_eq!(header(&arrived[0], "user-agent").as_deref(), Some(agent.as_str()), "the User-Agent follows the request to the next host");
}

#[test]
fn a_roots_mode_host_redirected_to_a_relative_path_on_itself_reads_the_body_through_the_bundle() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&[PLEX_DIRECT]);
    let served = spawn_scripted(
        Arc::clone(&cert),
        [("/start", Reply::redirect(302, "/video.mkv")), ("/video.mkv", Reply::ok(media_body()))],
    );
    // The device store holds nothing relevant; only the bundle can verify the host, on either hop.
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "redirect-roots-same-host");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "redirect-roots-same-host");
    pin_to_loopback(PLEX_DIRECT, served.port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(served.port));
    let _watch = keypin::Scoped::watch(&key);
    keypin::roots_established(&key, Some(20));

    let mut src = CurlSource::open(&url_of(PLEX_DIRECT, served.port, "/start"), 0).expect("the bundle verifies both hops");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());
    assert_eq!(request_lines(&served), ["GET /start HTTP/1.1", "GET /video.mkv HTTP/1.1"]);
    assert_eq!(served.accepted(), 2, "a latched host starts in roots mode: no strict handshake first, one connection per hop");
    assert!(keypin::is_roots_latched(&key), "still latched");
}

/// Two `*.plex.direct` names, neither verifiable by the device store, one CA (the bundle's) for both.
/// Neither is latched, so the open goes strict, then roots, on the first, and is redirected to the
/// second, which is a request of its own: it goes strict, then roots, under ITS key, from a ladder that
/// remembers nothing the first hop's did (the bundle is offered once per hop, not once per open). Both
/// end latched in roots mode. Each name reaches loopback through its own resolve pin, so the test
/// needs no DNS.
#[test]
fn a_roots_mode_open_redirected_to_a_second_plex_direct_host_ends_in_the_media() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&[PLEX_DIRECT, SECOND_PLEX_DIRECT]);
    let second = spawn_scripted(Arc::clone(&cert), [("/video.mkv", Reply::ok(media_body()))]);
    let first = spawn_scripted(Arc::clone(&cert), [("/start", Reply::redirect(302, &url_of(SECOND_PLEX_DIRECT, second.port, "/video.mkv")))]);
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "redirect-roots-two-hosts");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "redirect-roots-two-hosts");
    pin_to_loopback(PLEX_DIRECT, first.port);
    pin_to_loopback(SECOND_PLEX_DIRECT, second.port);
    let first_key = keypin::key_of(PLEX_DIRECT, i32::from(first.port));
    let _w1 = keypin::Scoped::watch(&first_key);
    let _w2 = watch(SECOND_PLEX_DIRECT, second.port);
    let second_key = keypin::key_of(SECOND_PLEX_DIRECT, i32::from(second.port));

    let mut src = CurlSource::open(&url_of(PLEX_DIRECT, first.port, "/start"), 0).expect("both hops are verified by the bundle");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());
    assert_eq!(request_lines(&first), ["GET /start HTTP/1.1"]);
    assert_eq!(request_lines(&second), ["GET /video.mkv HTTP/1.1"]);
    assert!(keypin::is_roots_latched(&first_key), "the host the open began on is latched in roots mode");
    assert!(keypin::is_roots_latched(&second_key), "so is the host it was redirected to: it went through the ladder under its own key");
}

/// The host a hop returns to is the host the open began on, unlatched at the start: the first request
/// runs the ladder (strict fails, the bundle answers) and latches the host, and the second begins from
/// that latch, in roots mode, with no doomed strict handshake of its own: three connections, not four.
#[test]
fn a_redirect_back_to_the_same_plex_direct_host_starts_its_second_hop_from_the_latch_the_first_set() {
    let _serial = plx_base::testlock::serial();
    if !curl_ready() { return; }
    let cert = leaf(&[PLEX_DIRECT]);
    let served = spawn_scripted(
        Arc::clone(&cert),
        [("/start", Reply::redirect(302, "/video.mkv")), ("/video.mkv", Reply::ok(media_body()))],
    );
    let _device = TestCaGuard::install(&mint_cert(&["unrelated.invalid"]).pem, "redirect-roots-same-host-cold");
    let _roots = keypin::test_roots::Guard::install(&cert.pem, "redirect-roots-same-host-cold");
    pin_to_loopback(PLEX_DIRECT, served.port);
    let key = keypin::key_of(PLEX_DIRECT, i32::from(served.port));
    let _watch = keypin::Scoped::watch(&key);
    assert!(!keypin::is_roots_latched(&key), "the open starts unlatched");

    let mut src = CurlSource::open(&url_of(PLEX_DIRECT, served.port, "/start"), 0).expect("both hops are verified by the bundle");
    assert_eq!(src.status(), 200);
    assert_eq!(read_to_end(&mut src), media_body());
    assert_eq!(request_lines(&served), ["GET /start HTTP/1.1", "GET /video.mkv HTTP/1.1"]);
    assert_eq!(served.accepted(), 3, "the failed strict handshake and the bundle one for the first hop, one more for the second");
    assert!(keypin::is_roots_latched(&key));
}
