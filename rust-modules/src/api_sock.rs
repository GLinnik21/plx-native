//! The UDS control socket — the typed API's transport. A UNIX domain socket
//! (`SOCK_STREAM`, JSON-lines) at `plxnative-api.sock` in the runtime directory, beside
//! the dev FIFO (`plxnative-remote`) and its state dump (`state.json`).
//!
//! The FIFO is a one-way byte pipe: a token in, a log line as the only answer, and the
//! state file as the only read-out. A typed control API needs a CONVERSATION: every
//! request carries a client-chosen request id, and exactly one response carries it back —
//! a structured outcome (what actually happened: staged, committed, paused) or a
//! structured error (a machine-readable code and the human-readable reason).
//!
//! The protocol is one JSON object per line (UTF-8, `\n`-terminated):
//!
//! ```text
//! client → {"id": 3, "req": {"kind": "play", "rk": "1$abc"}}
//! app    → {"id": 3, "ok": true, "outcome": {"kind": "play_staged", "rk": "1$abc"}}
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

/// A structured outcome: what actually happened, in the action's own vocabulary. The
/// commit-2 semantics are the COMMIT itself — the play answers `play_staged` (the
/// request was accepted, the item resolves, the ritual performs on a later frame), the
/// seek answers `seek_committed` (the engine's seek request was made); the completion
/// outcomes (the first frame for a play, the landed position for a seek, the captured
/// file for a shot) ride in with the request→outcome correlation rework.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// play: the request was accepted and the item is resolving from PMS.
    PlayStaged { rk: String },
    /// seek: the engine's seek request was committed.
    SeekCommitted { ms: u64 },
    /// audio/sub: the track menu's commit was performed for the stream with this id
    /// (`track` is `audio` or `sub`; it is not named `kind` — that is the tag).
    TrackCommitted { track: &'static str, sid: i64 },
    /// quality: the `…` menu's commit was performed for this rung.
    QualityCommitted { rung: String },
    /// profile: the session machine's profile commit was performed.
    ProfileCommitted { index: usize },
    Paused,
    Resumed,
    Stopped,
    /// state: the frame thread's snapshot — the same value `state.json` holds.
    State { snapshot: serde_json::Value },
    /// shot: the read-back is armed; the numbered file lands on the next swap.
    ShotRequested,
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

/// One request line: parse it and either post the typed request to the frame thread's
/// input path (the response comes via [`reply`], later) or answer a protocol-level error
/// immediately (a malformed line has no id to correlate with, and the frame thread
/// should never parse it).
fn handle_line(line: &str, pending: &mut VecDeque<(Option<u64>, Reply)>) {
    #[derive(Deserialize)]
    struct Wire {
        id: u64,
        req: Verb,
    }
    let Ok(w) = serde_json::from_str::<Wire>(line) else {
        pending.push_back((
            None,
            Reply::err(ApiError::Malformed {
                reason: "the line is not a request object".to_string(),
            }),
        ));
        return;
    };
    if !crate::api::push_req(Request { id: w.id, verb: w.req }) {
        pending.push_back((Some(w.id), Reply::err(ApiError::QueueFull)));
    }
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
