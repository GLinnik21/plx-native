//! Shared fixtures and helpers for the `route::decision` test modules split out below.

use super::*;

/// Duplicated from `plan::tests` (a 3-line Dolby Vision Profile 8 fixture the shared-
/// fixture split left on both sides of the module boundary; see that module's `p8` for
/// the sibling copy and its provenance comment).
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module (see the
// comment on the first one).
#[cfg(feature = "devtriggers")]
pub(super) fn p8() -> crate::metadata::Dovi {
    crate::metadata::Dovi {
        present: true,
        profile: 8,
        bl_compat: 1,
        el_present: false,
        ..crate::metadata::Dovi::NONE
    }
}

/// Same provenance as `plan::tests::p5`: single-layer IPT-PQ with no HDR10 fallback.
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module (see the
// comment on the first one).
#[cfg(feature = "devtriggers")]
pub(super) fn p5() -> crate::metadata::Dovi {
    crate::metadata::Dovi {
        present: true,
        profile: 5,
        bl_compat: 0,
        el_present: false,
        ..crate::metadata::Dovi::NONE
    }
}

/// Most route tests install a projection without constructing a native Engine. Make that
/// synthetic boundary explicit in the fixture layer so production `apply_plan` and tests of
/// the start reducer both retain the real `Prepared -> Starting -> result` semantics.
pub(super) fn apply_plan(ps: &mut PlaybackSession, plan: Plan, rk: &str) {
    let start = super::apply_plan(ps, &mut crate::stores::metadata::MetadataStore::default(), plan, rk);
    settle_plan_start_in_unit_test(ps, start);
}

pub(super) fn settle_pending_native_start(ps: &mut PlaybackSession, result: RouteStartResult) -> RouteStartAttempt {
    let transaction = pending_route_start().expect("prepared native start transaction");
    let attempt = claim_route_start_attempt(transaction).expect("physical Load attempt");
    assert!(settle_route_start(ps, attempt, result));
    attempt
}

pub(super) fn rollback_seconds(ps: &mut PlaybackSession) -> Option<i64> {
    rollback_original_recovery(ps).map(|rollback| rollback.offset_ns / 1_000_000_000)
}

pub(super) fn test_original_candidate(subtitle_ordinal: Option<i32>) -> AutoOriginalCandidate {
    AutoOriginalCandidate {
        url: "https://example.invalid/source.mkv".into(),
        probe_part: "https://example.invalid/source.mkv".into(),
        direct: true,
        vcodec: "hevc".into(),
        acodec: "eac3".into(),
        fps: 23.976,
        dovi: crate::metadata::Dovi::NONE,
        immersive: true,
        audio_sid: 42,
        audio_ordinal: Some(1),
        subtitle_ordinal,
    }
}

// ---- the playing item's SERVER: captured once, carried by value ---------------------------
// A ratingKey, a Part id, a Stream id, a playQueueID and a resume point are all keys on ONE
// server. Every PMS call in this file used to find its server by asking which one was current
// at the instant of the call, so an item borrowed from a shared source was resolved, queued,
// PUT, stopped and — every ten seconds — reported to whichever server the user had since
// wandered off to. None of that is observable from inside the app, which is why these exist.

/// Take the crate-wide serialization lock, empty the server registry AND idle the session, so
/// each test below starts from "nothing installed" and, more importantly, LEAVES the table that
/// way. A test that registers a loopback server and walks off owes the next one an empty table:
/// its ports close when it returns, so what it leaves behind is a `client_opt()` that answers
/// `Some` with a client nothing will ever answer.
///
/// The session goes with it because it holds a `ServerId` INTO that table: a leftover `cur_sid`
/// names a slot the next test is about to re-fill with a different server, and `machine_id` is
/// a cache keyed on exactly that id — which `the_machine_id_cache_is_scoped_to_the_server_that_taught_it`
/// then reads. `reset_session` is the whole-session write, and this is what it is for.
pub(super) fn fresh_registry(ps: &mut PlaybackSession) -> crate::testlock::Serial {
    let g = crate::testlock::serial();
    // These are process-global route transactions, not Session fields. A host test has no
    // Engine pump to spend them, so leaving either behind makes a later loopback server see a
    // stop for an encoder from a completely different case.
    let _ = take_pending_original();
    // Establish the idle projection before resetting the reducer: its applied snapshot must
    // describe this test's empty route, not the previous test's final encoder.  Quality is
    // part of the same baseline; cases which need Auto opt in after this boundary and then
    // land a route explicitly.
    reset_session(ps);
    restore_quality(Quality::Original);
    crate::player::reset_route_requests_for_test(ps);
    crate::plex::reset_servers_for_test();
    crate::player::clear_original_failure();
    g
}

/// A `ServerId` naming a slot nothing is registered in — so `client_for` answers `None` and
/// `build_stream` takes its no-client exit without opening a socket.
pub(super) fn unregistered_sid() -> ServerId {
    let id = ServerId::from_raw((crate::plex::MAX_SERVERS - 1) as u16);
    assert!(
        crate::plex::client_for(id).is_none(),
        "the test needs an EMPTY slot"
    );
    id
}

pub(super) const MDE_DIRECTPLAY: &[u8] =
    br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"directplay"}]}]}]}}"#;

pub(super) const MDE_TRANSCODE: &[u8] =
    br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode"}]}]}]}}"#;

/// Part.decision=transcode, video copied, audio transcoded — the measured TrueHD-only shape.
pub(super) const MDE_TRANSCODE_COPY: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"copy"},{"streamType":2,"decision":"transcode"}]}]}]}]}}"#;

/// Part.decision=transcode AND the video lane itself is transcode (bit depth, …).
pub(super) const MDE_TRANSCODE_VIDEO: &[u8] = br#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"decision":"transcode"}]}]}]}]}}"#;

pub(super) const EMPTY_MC: &[u8] = br#"{"MediaContainer":{}}"#;

pub(super) fn drain_http(socket: &mut std::net::TcpStream) -> String {
    use std::io::{BufRead, BufReader, Read};
    let mut reader = BufReader::new(socket.try_clone().expect("clone"));
    let mut first = String::new();
    reader.read_line(&mut first).expect("request line");
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("header");
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0; content_length];
        let _ = reader.read_exact(&mut body);
    }
    first
}

/// Exact `key=value` on a request-line query, so `subtitleStreamID=0` cannot match `88001`.
pub(super) fn query_param<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let query = line.split_once('?')?.1;
    let query = query.split_whitespace().next().unwrap_or(query);
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

pub(super) fn write_json(socket: &mut std::net::TcpStream, body: &[u8]) {
    use std::io::Write;
    write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len(),
    )
    .expect("headers");
    socket.write_all(body).expect("body");
}

/// Loopback PMS that answers PlayQueue / PUT / `/decision` long enough for `build_stream`.
pub(super) fn plan_pms(
    n: usize,
    mde_body: &'static [u8],
) -> (
    i32,
    std::sync::mpsc::Receiver<Vec<String>>,
    std::thread::JoinHandle<()>,
) {
    plan_pms_inner(n, mde_body, None)
}

/// Same as [`plan_pms`], plus a bounded `start.mkv` body so Remote Auto can probe a remux.
/// A Part GET is 503 — that is PMS 1.43 after a transcode MDE, and the test grades that we
/// never ask.
pub(super) fn plan_pms_with_start_mkv(
    n: usize,
    mde_body: &'static [u8],
    start_bytes: usize,
) -> (
    i32,
    std::sync::mpsc::Receiver<Vec<String>>,
    std::thread::JoinHandle<()>,
) {
    plan_pms_inner(n, mde_body, Some(start_bytes))
}

pub(super) fn plan_pms_inner(
    n: usize,
    mde_body: &'static [u8],
    start_bytes: Option<usize>,
) -> (
    i32,
    std::sync::mpsc::Receiver<Vec<String>>,
    std::thread::JoinHandle<()>,
) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut requests = Vec::new();
        while requests.len() < n && std::time::Instant::now() < deadline {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    // The listener is nonblocking; on macOS the accepted fd inherits that,
                    // and a parallel suite can accept before the request line is buffered.
                    socket
                        .set_nonblocking(false)
                        .expect("blocking accepted socket");
                    let first = drain_http(&mut socket);
                    if start_bytes.is_some() && first.contains("/library/parts/") {
                        use std::io::Write;
                        write!(
                            socket,
                            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("503");
                    } else if let Some(bytes) =
                        start_bytes.filter(|_| first.contains("start.mkv"))
                    {
                        use std::io::Write;
                        if bytes == 0 {
                            write!(
                                socket,
                                "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .expect("empty start.mkv");
                        } else {
                            write!(
                                socket,
                                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n",
                                bytes.saturating_sub(1),
                                bytes.saturating_mul(2),
                            )
                            .expect("start.mkv headers");
                            socket
                                .write_all(&vec![0x55; bytes])
                                .expect("start.mkv body");
                        }
                    } else if first.contains("/decision?")
                        && first.contains("hasMDE=1")
                        && first.contains("directPlay=1")
                        && query_param(&first, "subtitles") != Some("none")
                    {
                        // PMS 1.43.4: hasMDE+directPlay with a selected subtitle and
                        // subtitles=auto (the default when omitted) is HTTP 400.
                        use std::io::Write;
                        write!(
                            socket,
                            "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .expect("400");
                    } else {
                        let body = if first.contains("/decision?") {
                            mde_body
                        } else {
                            EMPTY_MC
                        };
                        write_json(&mut socket, body);
                    }
                    requests.push(first);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(4));
                }
                Err(error) => panic!("accept plan request: {error}"),
            }
        }
        tx.send(requests).expect("publish plan requests");
    });
    (port, rx, handle)
}

/// Selection is PMS state, not a promise made by a GET's query parameters.
pub(super) fn selection_probe_pms(
    refuse: bool,
    burn: i64,
) -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<(String, (i64, i64))>>) {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    listener.set_nonblocking(true).unwrap();
    let (done, stop) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut selection = (1, 9);
        let mut requests = Vec::new();
        loop {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    socket.set_nonblocking(false).unwrap();
                    let line = drain_http(&mut socket);
                    if line.starts_with("PUT /library/parts/") {
                        selection = (
                            query_param(&line, "audioStreamID").unwrap().parse().unwrap(),
                            query_param(&line, "subtitleStreamID").unwrap().parse().unwrap(),
                        );
                    }
                    let ready = selection == (2, burn);
                    if line.contains("start.mkv") {
                        // A wrong part selection cannot produce the intended remux sample.
                        let bytes = if ready && !refuse {
                            remote_probe_plan(320).unwrap().target_bytes
                        } else { 0 };
                        if bytes > 0 {
                            write!(socket, "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n", bytes - 1, bytes * 2).unwrap();
                        } else {
                            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                        }
                        socket.write_all(&vec![0x55; bytes]).unwrap();
                    } else if line.contains("/decision?") {
                        let body: &[u8] = if !line.contains("hasMDE=1") && (refuse || !ready) {
                            br#"{"MediaContainer":{"generalDecisionCode":2000,"transcodeDecisionCode":2000,"transcodeDecisionText":"synthetic refusal"}}"#
                        } else {
                            MDE_TRANSCODE_COPY
                        };
                        write_json(&mut socket, body);
                    } else {
                        write_json(&mut socket, EMPTY_MC);
                    }
                    requests.push((line, selection));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() { break; }
                    std::thread::yield_now();
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        }
        requests
    });
    (port, done, handle)
}

pub(super) fn fourk_item(
    sid: ServerId,
    audio: Vec<crate::metadata::Stream>,
) -> crate::metadata::PlayingItem {
    fourk_item_with_subs(sid, audio, Vec::new())
}

pub(super) fn fourk_item_with_subs(
    sid: ServerId,
    audio: Vec<crate::metadata::Stream>,
    subs: Vec<crate::metadata::Stream>,
) -> crate::metadata::PlayingItem {
    crate::metadata::PlayingItem {
        sid,
        rk: "rk-4k".into(),
        audio,
        subs,
        video_fps: 23.976,
        width: 3840,
        height: 2160,
        bitrate: 48_000,
        dovi: crate::metadata::Dovi::NONE,
        markers: Vec::new(),
        chapters: Vec::new(),
    }
}

pub(super) fn eac3_track() -> crate::metadata::Stream {
    crate::metadata::Stream {
        id: 36014,
        index: 1,
        lang_code: "eng".into(),
        codec: "eac3".into(),
        channels: 6,
        default: true,
        selected: true,
        profile: "dolby digital plus + dolby atmos".into(),
        ..Default::default()
    }
}

pub(super) fn selected_sub(id: i64, codec: &str) -> crate::metadata::Stream {
    crate::metadata::Stream {
        id,
        index: 0,
        lang_code: "eng".into(),
        codec: codec.into(),
        selected: true,
        ..Default::default()
    }
}

/// The pipeline tier's entry into HLS, both ways round. Differential against the old seam,
/// which had ONE way in: declare an Original source rate no link could carry and let the
/// starvation horizon fire on a reserve that was visibly filling. That entry stopped working
/// when the horizon started requiring an observed drain, and it should have — the reserve was
/// growing, so nothing was starving. This is the honest replacement.
/// The raster every pre-2026-08-28 `auto_network` case had hardcoded into the function.
pub(super) const HD: (u16, u16) = (1_920, 1_080);

/// A one-shot loopback PMS: accepts ONE connection, hands its request line back down the
/// channel, and answers 200 so the client's read terminates. Real sockets, like `stream.rs`'s
/// own tests — which server a POST actually reached is the only thing the timeline routing can
/// be graded on without a television.
pub(super) fn stub_pms() -> (
    i32,
    std::sync::mpsc::Receiver<String>,
    std::thread::JoinHandle<()>,
) {
    use std::io::{BufRead, BufReader, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().unwrap().port() as i32;
    let (tx, rx) = std::sync::mpsc::channel();
    let h = std::thread::spawn(move || {
        if let Some(Ok(s)) = l.incoming().next() {
            let mut line = String::new();
            let _ = BufReader::new(&s).read_line(&mut line);
            let mut s = s;
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            let _ = tx.send(line);
        }
    });
    (port, rx, h)
}

pub(super) fn ordered_stub_pms(
    label: &'static str,
    tx: std::sync::mpsc::Sender<(&'static str, String)>,
) -> (i32, std::thread::JoinHandle<()>) {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().unwrap().port() as i32;
    let handle = std::thread::spawn(move || {
        if let Some(Ok(socket)) = listener.incoming().next() {
            let mut line = String::new();
            let _ = BufReader::new(&socket).read_line(&mut line);
            let _ = tx.send((label, line));
            let mut socket = socket;
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
        }
    });
    (port, handle)
}
