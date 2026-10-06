//! Shared fixtures and helpers for the `route::decision` test modules split out below.

use super::*;

/// Duplicated from `plan::tests` (a 3-line Dolby Vision Profile 8 fixture the shared-
/// fixture split left on both sides of the module boundary; see that module's `p8` for
/// the sibling copy and its provenance comment).
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module (see the
// comment on the first one).
#[cfg(feature = "devtriggers")]
pub(super) fn p8() -> plx_data::metadata::Dovi {
    plx_data::metadata::Dovi {
        present: true,
        profile: 8,
        bl_compat: 1,
        el_present: false,
        ..plx_data::metadata::Dovi::NONE
    }
}

/// Same provenance as `plan::tests::p5`: single-layer IPT-PQ with no HDR10 fallback.
// Dev-only: used only by the `#[cfg(feature = "devtriggers")]` tests in this module (see the
// comment on the first one).
#[cfg(feature = "devtriggers")]
pub(super) fn p5() -> plx_data::metadata::Dovi {
    plx_data::metadata::Dovi {
        present: true,
        profile: 5,
        bl_compat: 0,
        el_present: false,
        ..plx_data::metadata::Dovi::NONE
    }
}

/// Most route tests install a projection without constructing a native Engine. Make that
/// synthetic boundary explicit in the fixture layer so production `apply_plan` and tests of
/// the start reducer both retain the real `Prepared -> Starting -> result` semantics.
pub(super) fn apply_plan(ps: &mut PlaybackSession, plan: Plan, rk: &str) {
    let start = super::apply_plan(ps, &mut plx_data::stores::metadata::MetadataStore::default(), plan, rk);
    settle_plan_start_in_unit_test(ps, start);
}

/// The claim half of [`take_ready_flight`], for the tests that drive claims: a seek's landing is a
/// different test's business and fails this one loudly.
pub(super) fn take_ready_retranscode_claim(
    ps: &mut PlaybackSession,
) -> Option<(ClaimedRouteAction, ClaimTail, i64, i64)> {
    match take_ready_flight(ps, false)? {
        ReadyFlight::Claim { action, tail, pending_seek, user_target } => Some((action, tail, pending_seek, user_target)),
        ReadyFlight::Seek { .. } => panic!("a seek landing where a claim's was expected"),
    }
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

// `test_original_candidate` lives in `route::decision` (beside `enhancement_test_session`, a seam
// the layers above call, which cannot name this `cfg(test)`-only module).
pub(super) use super::test_original_candidate;

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
pub(super) fn fresh_registry(ps: &mut PlaybackSession) -> plx_base::testlock::Serial {
    let g = plx_base::testlock::serial();
    // These are process-global route transactions, not Session fields. A host test has no
    // Engine pump to spend them, so leaving either behind makes a later loopback server see a
    // stop for an encoder from a completely different case.
    let _ = take_pending_original();
    // A worker landing (or a presentation hold) a previous test left behind — a test that panicked
    // mid-flight leaves both — belongs to nothing this test owns.
    drop(take_claim_landing());
    clear_injected_fault();
    crate::player::claim_hold::clear();
    // Establish the idle projection before resetting the reducer: its applied snapshot must
    // describe this test's empty route, not the previous test's final encoder.  Quality is
    // part of the same baseline; cases which need Auto opt in after this boundary and then
    // land a route explicitly.
    reset_session(ps);
    restore_quality(Quality::Original);
    restore_direct_play_mode(DirectPlayMode::Auto);
    // Issue #266: `ResolveEnv::snapshot` reads the enhancement preference from a global, so a
    // test that set it must not leak an enhanced resolve into the next one.
    crate::player::restore_audio_enhancements(plx_plex::plex::AudioEnhancements::NONE);
    crate::player::reset_route_requests_for_test(ps);
    plx_plex::plex::reset_servers_for_test();
    crate::player::clear_original_failure();
    g
}

/// A `ServerId` naming a slot nothing is registered in — so `client_for` answers `None` and
/// `build_stream` takes its no-client exit without opening a socket.
pub(super) fn unregistered_sid() -> ServerId {
    let id = ServerId::from_raw((plx_plex::plex::MAX_SERVERS - 1) as u16);
    assert!(
        plx_plex::plex::client_for(id).is_none(),
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

/// A bodyless status line, `Content-Length: 0`, `Connection: close` — the shape every synthetic
/// server in this file used for a plain refusal/ack before this helper existed.
pub(super) fn write_status(socket: &mut std::net::TcpStream, code: u16) {
    use std::io::Write;
    let reason = match code {
        200 => "OK",
        400 => "Bad Request",
        503 => "Service Unavailable",
        _ => "",
    };
    write!(
        socket,
        "HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    )
    .expect("status line");
}

/// A `206 Partial Content` answer covering `[0, n)` of a `2n`-byte resource, body filled with
/// `0x55` — the fixed "prove a Range GET works" shape every probe/admission fixture in this file
/// used before this helper existed.
pub(super) fn write_partial(socket: &mut std::net::TcpStream, n: usize) {
    use std::io::Write;
    write!(
        socket,
        "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {n}\r\nConnection: close\r\n\r\n",
        n.saturating_sub(1),
        n.saturating_mul(2),
    )
    .expect("partial headers");
    socket.write_all(&vec![0x55; n]).expect("partial body");
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
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let first = drain_http(&mut socket);
                    if start_bytes.is_some() && first.contains("/library/parts/") {
                        write_status(&mut socket, 503);
                    } else if let Some(bytes) =
                        start_bytes.filter(|_| first.contains("start.mkv"))
                    {
                        if bytes == 0 {
                            write_status(&mut socket, 200);
                        } else {
                            write_partial(&mut socket, bytes);
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
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    listener.set_nonblocking(true).unwrap();
    let (done, stop) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut selection = (1, 9);
        let mut requests = Vec::new();
        loop {
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
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
                            write_partial(&mut socket, bytes);
                        } else {
                            write_status(&mut socket, 200);
                        }
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
    audio: Vec<plx_data::metadata::Stream>,
) -> plx_data::metadata::PlayingItem {
    fourk_item_with_subs(sid, audio, Vec::new())
}

pub(super) fn fourk_item_with_subs(
    sid: ServerId,
    audio: Vec<plx_data::metadata::Stream>,
    subs: Vec<plx_data::metadata::Stream>,
) -> plx_data::metadata::PlayingItem {
    plx_data::metadata::PlayingItem {
        sid,
        rk: "rk-4k".into(),
        show_rk: String::new(),
        audio,
        subs,
        video_fps: 23.976,
        width: 3840,
        height: 2160,
        bitrate: 48_000,
        dovi: plx_data::metadata::Dovi::NONE,
        markers: Vec::new(),
        chapters: Vec::new(),
        blur: None,
    }
}

pub(super) fn eac3_track() -> plx_data::metadata::Stream {
    plx_data::metadata::Stream {
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

pub(super) fn selected_sub(id: i64, codec: &str) -> plx_data::metadata::Stream {
    plx_data::metadata::Stream {
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

/// How [`enhancement_pms`] answers a transcode `/decision` that carries `boostDialog=1` or
/// `normalizeLoudness=1` (issue #266). Shapes are the measured ones (`/tmp/plx266` M1/M2): an
/// honoured ask is a Part transcode with the video copied and the audio re-encoded.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum EnhMode {
    /// Audio `transcode`, declared as this output codec.
    Honor(&'static str),
    /// Audio `transcode` with no codec on either stream — `decision_codecs` finds nothing.
    HonorNoCodecs,
    /// `generalDecisionCode 2000` — the refusal wire `tests/mock_pms.py --refuse-enhancements` sends.
    Refuse,
    /// Audio `copy` despite the params — an old or non-conforming PMS.
    Ignore,
}

/// Loopback PMS for the audio-enhancement resolve: MDE answers `mde`, a transcode decision answers
/// per [`EnhMode`] when it carries a param and a plain remux (video+audio copy) when it does not,
/// and every media GET (Part or `start.mkv`) serves `media_bytes` so a Remote probe can complete.
/// Runs until the returned sender fires; the join handle yields every request line.
pub(super) fn enhancement_pms(
    mde: &'static [u8],
    mode: EnhMode,
    media_bytes: usize,
) -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<String>>) {
    enhancement_pms_parts(mde, mode, media_bytes, PartAnswer::Serve)
}

/// How [`enhancement_pms_parts`] answers a raw `/library/parts/` GET.
#[derive(Clone, Copy)]
pub(super) enum PartAnswer {
    /// The same bytes `start.mkv` gets.
    Serve,
    /// `503 Service Unavailable` — what the device got for the Original Part on the release of an
    /// enhanced remux (issue #266 PR 4 device run, PMS 1.43.4: "Denying access due to session
    /// lacking permission to direct play" is the server's own wording for this status).
    Refuse,
    /// Headers with a `Content-Length` the connection then never delivers: a transport failure
    /// (curl reports `CURLE_PARTIAL_FILE`) mid-body, not a status the server chose. This is the
    /// shape `ThroughputFailure::BodyRead` classifies, distinct from `Refuse`'s definite status.
    Reset,
    /// `503` for a Part GET that carries exactly this `X-Plex-Session-Identifier` (the id of a
    /// remux that is still live), `206`/`200` for any other session id or none. This is what a
    /// real PMS measured on 2026-10-06 (`docs/pms-api.md` M8).
    RefuseSession(&'static str),
}

/// [`enhancement_pms`] with the raw Part's answer chosen separately from `start.mkv`'s.
pub(super) fn enhancement_pms_parts(
    mde: &'static [u8],
    mode: EnhMode,
    media_bytes: usize,
    parts: PartAnswer,
) -> (i32, std::sync::mpsc::Sender<()>, std::thread::JoinHandle<Vec<String>>) {
    use std::io::Write;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port() as i32;
    listener.set_nonblocking(true).unwrap();
    let (done, stop) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut requests = Vec::new();
        loop {
            match plx_base::testnet::accept(&listener) {
                Ok((mut socket, _)) => {
                    let line = drain_http(&mut socket);
                    let enhanced = query_param(&line, "boostDialog") == Some("1")
                        || query_param(&line, "normalizeLoudness") == Some("1");
                    let live_session_part = line.starts_with("GET /library/parts/")
                        && matches!(parts, PartAnswer::RefuseSession(id)
                            if query_param(&line, "X-Plex-Session-Identifier") == Some(id));
                    if live_session_part
                        || (line.starts_with("GET /library/parts/") && matches!(parts, PartAnswer::Refuse))
                    {
                        write_status(&mut socket, 503);
                    } else if line.starts_with("GET /library/parts/") && matches!(parts, PartAnswer::Reset) {
                        // A valid 206 answer to the exact Range the admission asked for, then the
                        // connection dies before any of the promised body arrives: curl reports
                        // this as a transport failure (`CURLE_PARTIAL_FILE`) on an otherwise
                        // successful response, not as the server's own answer.
                        let len = media_bytes.max(1);
                        let _ = write!(
                            socket,
                            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-{}/{}\r\nContent-Length: {len}\r\n\r\n",
                            len - 1,
                            len * 2,
                        );
                        let _ = socket.flush();
                        // Give curl a chance to parse the headers (and this admission's own open
                        // to succeed) as a call separate from the one that meets the closed
                        // connection, so the failure lands in the body-read phase deterministically
                        // rather than racing the header parse itself.
                        std::thread::sleep(std::time::Duration::from_millis(30));
                        drop(socket);
                    } else if line.contains("start.mkv") || line.starts_with("GET /library/parts/") {
                        if media_bytes > 0 {
                            write_partial(&mut socket, media_bytes);
                        } else {
                            write_status(&mut socket, 200);
                        }
                    } else if line.contains("/decision?") && line.contains("hasMDE=1") {
                        write_json(&mut socket, mde);
                    } else if line.contains("/decision?") {
                        let body: String = match (enhanced, mode) {
                            (true, EnhMode::Refuse) => r#"{"MediaContainer":{"generalDecisionCode":2000,"transcodeDecisionCode":4020,"transcodeDecisionText":"synthetic enhancement refusal","Metadata":[]}}"#.into(),
                            (true, EnhMode::HonorNoCodecs) => String::from_utf8(MDE_TRANSCODE_COPY.to_vec()).unwrap(),
                            (true, EnhMode::Honor(codec)) => format!(r#"{{"MediaContainer":{{"Metadata":[{{"Media":[{{"Part":[{{"decision":"transcode","Stream":[{{"streamType":1,"codec":"hevc","decision":"copy"}},{{"streamType":2,"codec":"{codec}","decision":"transcode"}}]}}]}}]}}]}}}}"#),
                            (true, EnhMode::Ignore) | (false, _) => r#"{"MediaContainer":{"Metadata":[{"Media":[{"Part":[{"decision":"transcode","Stream":[{"streamType":1,"codec":"hevc","decision":"copy"},{"streamType":2,"codec":"ac3","decision":"copy"}]}]}]}]}}"#.into(),
                        };
                        write_json(&mut socket, body.as_bytes());
                    } else {
                        write_json(&mut socket, EMPTY_MC);
                    }
                    requests.push(line);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop.try_recv().is_ok() { break; }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(e) => panic!("fixture accept: {e}"),
            }
        }
        requests
    });
    (port, done, handle)
}
