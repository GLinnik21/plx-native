//! The UDS control socket — the typed API's transport. A UNIX domain socket
//! (`SOCK_STREAM`, JSON-lines) at `plxnative-api.sock` in the runtime directory, beside
//! the dev FIFO (`plxnative-remote`) and its state dump (`state.json`).
//!
//! The FIFO is a one-way byte pipe: a token in, a log line as the only answer, and the
//! state file as the only read-out. A typed control API needs a CONVERSATION: every
//! request carries a client-chosen request id, and exactly one response carries it back —
//! a structured outcome (what actually happened) or a structured error (a
//! machine-readable code and the human-readable reason).
//!
//! The response is produced when the OUTCOME is known, not when the request is queued.
//! The synchronous verbs answer their commit on the spot — the same commit path the
//! app's own actions use (`no_engine` with the state instead of a bare "no", the
//! committed track's `sid`, a refused commit as a structured error). The asynchronous
//! ones (play, seek, profile, shot) park in `api`'s AWAITING table and answer on the
//! frame their outcome lands: `play_started` with the title, `seek_landed` with the
//! position, `profile_seated` with the seated profile's title, `shot_captured` with the
//! numbered file — or the structured failure that explains why.
//!
//! The protocol is one JSON object per line (UTF-8, `\n`-terminated):
//!
//! ```text
//! client → {"id": 1, "req": {"kind": "state"}}
//! app    → {"id": 1, "ok": true, "outcome": {"kind": "state", "snapshot": {"state": "playing", "pos_ms": 354000, "title": "…", …}}}
//! client → {"id": 3, "req": {"kind": "play", "rk": "1$abc"}}
//! app    → {"id": 3, "ok": true, "outcome": {"kind": "play_started", "rk": "1$abc", "title": "…"}}   (on the first playing frame)
//! app    → {"id": 4, "ok": false, "error": {"code": "no_engine", "state": "idle"}}
//! app    → {"id": null, "ok": false, "error": {"code": "malformed", "reason": "..."}}
//! ```
//!
//! The `req` vocabulary is the typed form of the FIFO's `api:*` verbs — the same actions
//! and the same argument shapes (play, seek, audio, sub, quality, profile, pause,
//! resume, stop), plus the two reads (`state`, `shot`). Execution happens on the frame
//! thread through the same single input path as the FIFO
//! ([`crate::api::IngestToken`]); the response is produced there — where the outcome is
//! known — and written back by this module's server thread over a bounded reply queue.
//!
//! One client at a time: an accepted connection is served until it closes, and a second
//! connection waits in the listen backlog. The control surface is a single agent
//! session; parallel clients are a protocol change (replies would need a connection id),
//! not a bug fix.
//!
//! State is read from the owner: `state` answers with the snapshot the frame thread
//! builds every frame — the same value `state.json` holds — never a second copy.
//!
//! Like the FIFO door, the socket exists only where the dev triggers exist
//! (`devtrig::ENABLED`): a release build creates no socket file.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;

use plx_base::eventlog::log;

/// A request on the typed protocol: the client's id + the verb.
#[derive(Debug)]
pub struct Request {
    /// The client's own correlation id; echoed unchanged on the response.
    pub id: u64,
    pub verb: Verb,
}

/// The typed request vocabulary — the FIFO's `api:*` verbs with their argument shapes.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Verb {
    /// `api:play:<rk>` — the headless play ritual (resolve from PMS, seat the engine).
    Play { rk: String },
    /// `api:seek:<ms>` — an absolute engine seek (milliseconds).
    Seek { ms: u64 },
    /// `api:audio:<sid>` — the track menu's audio commit for the stream with this id.
    Audio { sid: i64 },
    /// `api:sub:<sid>` — the track menu's subtitle commit for the stream with this id.
    Sub { sid: i64 },
    /// `api:quality:<rung>` — the `…` menu's quality commit for this rung.
    Quality { rung: String },
    /// `api:profile:<index>` — the session machine's profile commit.
    Profile { index: usize },
    /// `api:pause` — the PAUSE key.
    Pause,
    /// `api:resume` — the PLAY key.
    Resume,
    /// `api:stop` — the STOP key's leave-playback ritual.
    Stop,
    /// Read: the frame thread's snapshot (the value `state.json` holds).
    State,
    /// Read: a capture of the next frame (a numbered PNG beside the state file).
    Shot,
}

/// A structured outcome: what actually happened, in the action's own vocabulary — never
/// an acknowledgement. The synchronous verbs (audio, sub, quality, pause, resume, stop)
/// answer their COMMIT: the media layer accepts or refuses on the spot. The asynchronous
/// ones (play, seek, profile, shot) are parked in `api`'s AWAITING table when the
/// request's frame commits them, and answered on the frame their OUTCOME lands:
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// play: the play actually started — the first frame reached `playing`; `title` is
    /// what is playing now.
    PlayStarted { rk: String, title: String },
    /// play: the play never started, with the reason from the layer that refused it —
    /// the staging worker (no PMS selected, no such item, no playable part), the routing
    /// question, the engine's verdict, a supersession (a stop or a newer play), or the
    /// 120 s deadline.
    PlayFailed { rk: String, reason: String },
    /// seek: the engine landed; `pos_ms` is the position it settled at (within 3 s of
    /// the target — the engine's landing, not an echo of the request).
    SeekLanded { ms: u64, pos_ms: i64 },
    /// seek: the seek never landed — the session left playback, or no landing in 10 s.
    SeekFailed { ms: u64, reason: String },
    /// audio/sub: the track menu's commit was performed for the stream with this id
    /// (`track` is `audio` or `sub`; it is not named `kind` — that is the tag).
    TrackCommitted { track: &'static str, sid: i64 },
    /// quality: the `…` menu's commit was performed for this rung.
    QualityCommitted { rung: String },
    /// profile: the switch LANDED — the session's auth flow reached `ready` after the
    /// pick; `title` is the seated profile's.
    ProfileSeated { index: usize, title: String },
    /// profile: the switch failed — the flow's own error line, or the flow left the
    /// switch without this pick (a sign-out, the 60 s deadline), naming the phase.
    ProfileFailed { index: usize, reason: String },
    Paused,
    Resumed,
    Stopped,
    /// state: the frame thread's snapshot — the same value `state.json` holds.
    State { snapshot: serde_json::Value },
    /// shot: the capture landed; `path` is the numbered `shot-<n>.png` in the runtime
    /// directory, written on the frame swap that followed the request.
    ShotCaptured { path: String },
    /// shot: the capture failed (empty viewport, encode/write error) — or no capture in
    /// 10 s.
    ShotFailed { reason: String },
}

/// A structured error: a machine-readable code (the stable vocabulary a client may
/// branch on) plus the reason (human-readable; its wording may change).
#[derive(Debug, Serialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ApiError {
    /// The request line was not a request (bad JSON, an unknown `kind`, a missing or
    /// mistyped argument). Carries no id — there was none to echo.
    Malformed { reason: String },
    /// The verb needs a seated engine; the session is in the named state instead.
    NoEngine { state: &'static str },
    /// No audio/subtitle track with that id on the playing item.
    NoTrack { sid: i64 },
    /// The quality rung is not one the routing vocabulary knows.
    UnknownRung { rung: String },
    /// The profile pick needs the roster screen; the session's auth flow is in the named
    /// phase instead (a pick outside the roster phase is one the session machine would
    /// silently drop — the typed surface says so).
    NotOnRoster { phase: String },
    /// The roster has no profile at that index.
    NoProfile { index: usize, count: usize },
    /// The read has no answer yet (the frame thread has not published a frame).
    NoSnapshot,
    /// The frame loop's command queue was full when the request was posted.
    QueueFull,
    /// The play's PMS-resolution worker was refused (the app is refusing work).
    WorkerRefused,
    /// Internal (serialization or an invariant).
    Internal { reason: String },
}

/// A response frame the frame thread has produced but the server thread has not yet
/// written.
pub struct Reply {
    pub ok: bool,
    pub outcome: Option<Outcome>,
    pub error: Option<ApiError>,
}

impl Reply {
    pub fn ok(outcome: Outcome) -> Self {
        Reply { ok: true, outcome: Some(outcome), error: None }
    }
    pub fn err(error: ApiError) -> Self {
        Reply { ok: false, outcome: None, error: Some(error) }
    }
}

/// The frame thread's half of the reply channel, set by [`start`]. A reply before
/// `start` (impossible in practice — there is no request without the socket) is
/// dropped.
static SENDER: Mutex<Option<mpsc::Sender<(Option<u64>, Reply)>>> = Mutex::new(None);

/// The frame thread's half: hand one response to the socket's server thread. The queue
/// is bounded on the SERVER side (a client that stops draining drops the OLDEST pending
/// reply there) — this send itself never blocks on the socket.
pub fn reply(id: u64, r: Reply) {
    if let Some(tx) = SENDER.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send((Some(id), r));
    }
}

/// Bind the socket and spawn its server thread. Called from the boot next to
/// `api::init()`. Like the FIFO door, the socket exists only where the dev triggers
/// exist; a stale socket file from a crashed app is replaced (a dead file is no server —
/// a connect to it would fail, so replacing it is always the right move).
pub fn start() {
    if !plx_base::devtrig::ENABLED {
        return;
    }
    let path = plx_base::paths::in_runtime_dir("plxnative-api.sock");
    let _ = std::fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(&path) else {
        log(&format!("api sock: bind {} failed — no typed control surface", path.display()));
        return;
    };
    let (tx, rx) = mpsc::channel::<(Option<u64>, Reply)>();
    *SENDER.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
    let name = path.display().to_string();
    let worker = plx_base::task::spawn("api-sock", move || {
        log(&format!("api sock: serving on {name}"));
        loop {
            match listener.accept() {
                Ok((conn, _)) => serve(conn, &rx),
                Err(e) => {
                    log(&format!("api sock: accept failed: {e}"));
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    });
    if worker.is_none() {
        log("api sock: the server thread was refused — no typed control surface");
    }
}

/// Serve one connection: read request lines, write responses. Runs until the client
/// closes, or a write to it times out (a client that cannot drain its buffer in a second
/// is gone; the next connect gets a fresh channel).
fn serve(mut conn: UnixStream, rx: &mpsc::Receiver<(Option<u64>, Reply)>) {
    let _ = conn.set_read_timeout(Some(Duration::from_millis(100)));
    let _ = conn.set_write_timeout(Some(Duration::from_secs(1)));
    let mut line = String::new();
    let mut pending: VecDeque<(Option<u64>, Reply)> = VecDeque::new();
    loop {
        // The responses the frame thread has produced since the last pass. This is also
        // where the reply queue is bounded: a client that stops draining drops the
        // oldest reply — the frame thread's time is never spent on a dead socket.
        while let Ok(r) = rx.try_recv() {
            if pending.len() >= 256 {
                let _ = pending.pop_front();
                log("api sock: the reply queue overflowed — dropped the oldest reply");
            }
            pending.push_back(r);
        }
        if !pending.is_empty() {
            let mut out = String::new();
            for (id, r) in pending.drain(..) {
                out.push_str(&wire(id, &r));
                out.push('\n');
            }
            if conn.write_all(out.as_bytes()).is_err() {
                return;
            }
        }
        let mut buf = [0u8; 4096];
        match conn.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => {
                line.push_str(&String::from_utf8_lossy(&buf[..n]));
                while let Some(p) = line.find('\n') {
                    let full = line[..p].trim().to_string();
                    line = line[p + 1..].to_string();
                    if !full.is_empty() {
                        handle_line(&full, &mut pending);
                    }
                }
                if line.len() > 64 * 1024 {
                    // A request without a newline is not a request.
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return,
        }
    }
}

/// One line of the wire: a request object (`{"id": 42, "req": ...}`) or
/// nothing. Returns the client's id and its verb when the line is a request
/// object; a malformed line's reply is the protocol's only on-the-spot answer,
/// and it names no id — there was none to echo.
fn parse(line: &str) -> Option<(u64, Verb)> {
    #[derive(Deserialize)]
    struct Wire {
        id: u64,
        req: Verb,
    }
    serde_json::from_str::<Wire>(line).ok().map(|w| (w.id, w.req))
}

/// Post a well-formed request to the frame thread's input path (the response
/// comes via [`reply`], later) or answer a protocol-level error immediately —
/// a malformed line has no id to correlate with, and the frame thread should
/// never parse it.
fn handle_line(line: &str, pending: &mut VecDeque<(Option<u64>, Reply)>) {
    let (id, verb) = match parse(line) {
        Some(v) => v,
        None => {
            pending.push_back((
                None,
                Reply::err(ApiError::Malformed {
                    reason: "the line is not a request object".to_string(),
                }),
            ));
            return;
        }
    };
    if !crate::api::push_req(Request { id, verb }) {
        pending.push_back((Some(id), Reply::err(ApiError::QueueFull)));
    }
    // No on-the-spot answer to a well-formed request: it sits on the frame
    // thread's queue until the action's outcome lands.
}

/// One response line.
fn wire(id: Option<u64>, r: &Reply) -> String {
    #[derive(Serialize)]
    struct F<'a> {
        id: Option<u64>,
        ok: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        outcome: Option<&'a Outcome>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<&'a ApiError>,
    }
    let f = F {
        id,
        ok: r.ok,
        outcome: r.outcome.as_ref(),
        error: r.error.as_ref(),
    };
    serde_json::to_string(&f).unwrap_or_else(|_| {
        r#"{"id":null,"ok":false,"error":{"code":"internal","reason":"response serialization failed"}}"#
            .to_string()
    })
}

#[cfg(test)]
mod tests {
    //! The wire, pinned: every verb, every outcome, every error serializes to
    //! the exact JSON a client may match on, and a full conversation round-trips
    //! over a real socket — the reply channel included.
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    fn verb(v: &Verb) -> &'static str {
        match v {
            Verb::Play { .. } => "play",
            Verb::Seek { .. } => "seek",
            Verb::Audio { .. } => "audio",
            Verb::Sub { .. } => "sub",
            Verb::Quality { .. } => "quality",
            Verb::Profile { .. } => "profile",
            Verb::Pause => "pause",
            Verb::Resume => "resume",
            Verb::Stop => "stop",
            Verb::State => "state",
            Verb::Shot => "shot",
        }
    }

    #[test]
    fn parse_the_whole_verb_vocabulary() {
        let cases: [(&str, u64, Verb); 11] = [
            (r#"{"id": 1, "req": {"kind": "play", "rk": "1$abc"}}"#, 1, Verb::Play { rk: "1$abc".into() }),
            (r#"{"id": 2, "req": {"kind": "seek", "ms": 120000}}"#, 2, Verb::Seek { ms: 120_000 }),
            (r#"{"id": 3, "req": {"kind": "audio", "sid": 2}}"#, 3, Verb::Audio { sid: 2 }),
            (r#"{"id": 4, "req": {"kind": "sub", "sid": 1}}"#, 4, Verb::Sub { sid: 1 }),
            (r#"{"id": 5, "req": {"kind": "quality", "rung": "720p_4_mbps"}}"#, 5, Verb::Quality { rung: "720p_4_mbps".into() }),
            (r#"{"id": 6, "req": {"kind": "profile", "index": 1}}"#, 6, Verb::Profile { index: 1 }),
            (r#"{"id": 7, "req": {"kind": "pause"}}"#, 7, Verb::Pause),
            (r#"{"id": 8, "req": {"kind": "resume"}}"#, 8, Verb::Resume),
            (r#"{"id": 9, "req": {"kind": "stop"}}"#, 9, Verb::Stop),
            (r#"{"id": 10, "req": {"kind": "state"}}"#, 10, Verb::State),
            (r#"{"id": 11, "req": {"kind": "shot"}}"#, 11, Verb::Shot),
        ];
        for (line, want_id, want) in cases {
            let (id, got) = parse(line).unwrap_or_else(|| panic!("parse: {line}"));
            assert_eq!(id, want_id, "the id for {line}");
            assert_eq!(verb(&got), verb(&want), "the verb for {line}");
        }
        // The door: not a request object, and an unknown kind are not requests.
        for bad in [
            r#"not json at all"#,
            r#"{"id": 1}"#,
            r#"{"id": 1, "req": {"kind": "explode"}}"#,
            r#"{"id": 1, "req": {"kind": "seek"}}"#,
            r#"{"id": 1, "req": {"kind": "play"}}"#,
            "{}",
        ] {
            assert!(parse(bad).is_none(), "the door: {bad}");
        }
    }

    #[test]
    fn the_outcome_vocabulary_is_wire_pinned() {
        let o = |o: Outcome| {
            let id: Option<u64> = Some(7);
            let r = Reply { ok: true, outcome: Some(o), error: None };
            wire(id, &r)
        };
        // The state snapshot is the app's own snapshot JSON, passed through
        // (a Value map serializes with sorted keys).
        assert_eq!(o(Outcome::State { snapshot: serde_json::json!({"state": "idle", "pos_ms": 0, "up_ms": 0}) }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"state","snapshot":{"pos_ms":0,"state":"idle","up_ms":0}}}"#);
        assert_eq!(o(Outcome::PlayStarted { rk: "1$abc".into(), title: "Akira".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"play_started","rk":"1$abc","title":"Akira"}}"#);
        assert_eq!(o(Outcome::PlayFailed { rk: "1$abc".into(), reason: "no PMS server selected yet (sign-in pending)".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"play_failed","rk":"1$abc","reason":"no PMS server selected yet (sign-in pending)"}}"#);
        assert_eq!(o(Outcome::SeekLanded { ms: 120_000, pos_ms: 120_345 }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"seek_landed","ms":120000,"pos_ms":120345}}"#);
        assert_eq!(o(Outcome::SeekFailed { ms: 120_000, reason: "the session left playback".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"seek_failed","ms":120000,"reason":"the session left playback"}}"#);
        assert_eq!(o(Outcome::TrackCommitted { track: "audio", sid: 2 }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"track_committed","track":"audio","sid":2}}"#);
        assert_eq!(o(Outcome::QualityCommitted { rung: "720p_4_mbps".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"quality_committed","rung":"720p_4_mbps"}}"#);
        assert_eq!(o(Outcome::ProfileSeated { index: 1, title: "Kids".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"profile_seated","index":1,"title":"Kids"}}"#);
        assert_eq!(o(Outcome::ProfileFailed { index: 3, reason: "the flow left".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"profile_failed","index":3,"reason":"the flow left"}}"#);
        assert_eq!(o(Outcome::Paused), r#"{"id":7,"ok":true,"outcome":{"kind":"paused"}}"#);
        assert_eq!(o(Outcome::Resumed), r#"{"id":7,"ok":true,"outcome":{"kind":"resumed"}}"#);
        assert_eq!(o(Outcome::Stopped), r#"{"id":7,"ok":true,"outcome":{"kind":"stopped"}}"#);
        assert_eq!(o(Outcome::ShotCaptured { path: "shot-7.png".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"shot_captured","path":"shot-7.png"}}"#);
        assert_eq!(o(Outcome::ShotFailed { reason: "the viewport was empty".into() }),
            r#"{"id":7,"ok":true,"outcome":{"kind":"shot_failed","reason":"the viewport was empty"}}"#);
    }

    #[test]
    fn the_error_vocabulary_is_wire_pinned() {
        let e = |e: ApiError| {
            let id: Option<u64> = Some(3);
            let r = Reply { ok: false, outcome: None, error: Some(e) };
            wire(id, &r)
        };
        assert_eq!(e(ApiError::Malformed { reason: "the line is not a request object".into() }),
            r#"{"id":3,"ok":false,"error":{"code":"malformed","reason":"the line is not a request object"}}"#);
        assert_eq!(e(ApiError::NoEngine { state: "idle" }),
            r#"{"id":3,"ok":false,"error":{"code":"no_engine","state":"idle"}}"#);
        assert_eq!(e(ApiError::NoTrack { sid: 2 }),
            r#"{"id":3,"ok":false,"error":{"code":"no_track","sid":2}}"#);
        assert_eq!(e(ApiError::UnknownRung { rung: "4k".into() }),
            r#"{"id":3,"ok":false,"error":{"code":"unknown_rung","rung":"4k"}}"#);
        assert_eq!(e(ApiError::NotOnRoster { phase: "auth".into() }),
            r#"{"id":3,"ok":false,"error":{"code":"not_on_roster","phase":"auth"}}"#);
        assert_eq!(e(ApiError::NoProfile { index: 9, count: 2 }),
            r#"{"id":3,"ok":false,"error":{"code":"no_profile","index":9,"count":2}}"#);
        assert_eq!(e(ApiError::NoSnapshot),
            r#"{"id":3,"ok":false,"error":{"code":"no_snapshot"}}"#);
        assert_eq!(e(ApiError::QueueFull), r#"{"id":3,"ok":false,"error":{"code":"queue_full"}}"#);
        assert_eq!(e(ApiError::WorkerRefused), r#"{"id":3,"ok":false,"error":{"code":"worker_refused"}}"#);
        assert_eq!(e(ApiError::Internal { reason: "the frame thread dropped it".into() }),
            r#"{"id":3,"ok":false,"error":{"code":"internal","reason":"the frame thread dropped it"}}"#);
        // The id-less door: a malformed line's reply carries no id.
        assert_eq!(
            {
                let r = Reply { ok: false, outcome: None, error: Some(ApiError::Malformed { reason: "x".into() }) };
                wire(None, &r)
            },
            r#"{"id":null,"ok":false,"error":{"code":"malformed","reason":"x"}}"#
        );
    }

    fn start_serve() -> (String, mpsc::Sender<(Option<u64>, Reply)>) {
        // The suite's TMPDIR hygiene check fails a binary that leaves anything behind,
        // so the scratch dir comes from the base's per-process helper (removed at exit).
        let dir = plx_base::testscratch::process_dir("api-sock");
        let sock = dir.join("t.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let path = sock.to_string_lossy().into_owned();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (conn, _) = listener.accept().unwrap();
            serve(conn, &rx);
        });
        (path, tx)
    }

    fn read_line(conn: &UnixStream) -> String {
        let mut reader = BufReader::new(conn);
        let mut s = String::new();
        reader.read_line(&mut s).unwrap();
        s
    }

    #[test]
    fn a_conversation_round_trips_over_the_socket() {
        let (path, tx) = start_serve();
        let mut conn = UnixStream::connect(&path).unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        conn.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
        use std::io::Read;

        // 1. The door: a malformed line answers on the spot, with no id.
        conn.write_all(b"garbage\n").unwrap();
        let line = read_line(&conn);
        assert!(line.contains("\"code\":\"malformed\""), "the door: {line}");
        assert!(line.contains("\"id\":null"), "the door names no id: {line}");

        // 2. A well-formed request gets NO on-the-spot answer: it sits on the
        //    frame thread's queue until the outcome lands.
        conn.write_all(b"{\"id\":7,\"req\":{\"kind\":\"state\"}}\n").unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let mut probe = [0u8; 1];
        let blocked = conn.read(&mut probe).is_err();
        assert!(blocked, "no on-the-spot answer to a queued request");

        // 3. The reply channel: the frame thread's outcome rides back with the
        //    request's id, and the conversation continues on the same
        //    connection.
        tx.send((
            Some(7),
            Reply {
                ok: true,
                outcome: Some(Outcome::State {
                    snapshot: serde_json::json!({
                        "state": "playing",
                        "pos_ms": 354_000,
                        "title": "Akira",
                        "up_ms": 90_000
                    }),
                }),
                error: None,
            },
        ))
        .unwrap();
        let line = read_line(&conn);
        let v: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["id"], 7, "the reply carries the request's id");
        assert_eq!(v["ok"], true);
        assert_eq!(v["outcome"]["kind"], "state");
        assert_eq!(v["outcome"]["snapshot"]["pos_ms"], 354_000);
    }
}
