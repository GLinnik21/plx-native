//! The command API: an UNCONDITIONAL local HTTP surface for an agent (an LLM over SSH, a
//! test harness, an operator's script) to drive and read the app.
//!
//! Everything else in this crate's input story is gated — the remote FIFO is a dev/test
//! channel, the lab command long-poll is a `lab-diagnostics` feature, `capture` needs a
//! devtrigger file. This one is on in the **debug flavour only**: a debug television
//! nobody can reach from a keyboard is also a television nobody can debug, and the debug
//! install (`com.beb.plxnative.debug`) is a developer machine in the first place. In the
//! stable build the module compiles in but `init` never starts the listener, so nothing
//! binds. While it runs it binds `INADDR_ANY` with no credential, the same exposure
//! `capture` accepts when armed: the threat is the LAN, and the surface only exposes
//! playback state plus the commands a remote control could send.
//!
//! Architecture, mirroring `capture` (one listener, no threads per connection beyond what
//! `task::spawn` allows, state in statics):
//!
//! - an API thread runs a tiny HTTP/1.1 server on `capture`'s port + 10
//!   (`default_port` below). It parses one request per connection and answers JSON.
//! - WRITE endpoints never touch the frame. They stage work:
//!   - a `/play` body's `rk` is fetched from PMS ON THE API THREAD (`servers::client().
//!     metadata(rk)` → `pms::parse_item`), parked in `PENDING_PLAYS`, and the token
//!     `api:play:<rk>` is posted to `PENDING`;
//!   - every other verb posts an `api:` token (`api:pause`, `api:resume`, `api:stop`,
//!     `api:seek:<ms>`, `api:audio:<sid>`, `api:sub:<sid>`).
//! - the frame loop drains `PENDING` once per frame inside `run::ingest` (the same single
//!   input path as the FIFO and the lab channel) and hands each token to
//!   `run::ingress_token`, where the `api:` prefix is peeled and [`handle`] performs it
//!   with full `&mut App`. Tokens without the prefix pass straight through the existing
//!   dispatcher — the API can speak the whole FIFO vocabulary.
//! - READ endpoints answer from `SNAPSHOT`, a string [`report`](run::report) republishes
//!   every frame from the live `PlaybackSession` + metadata store.
//!
//! Wire (all JSON):
//! - `GET /health`  → `{app, flavour, port, up_ms}`
//! - `GET /state`   → the last frame's snapshot, or `{"error":"no frame yet"}`
//! - `POST /play`   body `{"rk":"1234"}` → 202 `{"queued":"api:play:1234"}`; the rk is
//!   resolved from PMS on the API thread, so a 404 here means PMS does not hold it (or the
//!   session has no token yet — retry).
//! - `POST /command` body `{"tok":"pause"}` → 202; any FIFO token (keys, `txt:`, `pat:`, …).
//! - `POST /pause` / `/resume` / `/stop` → the frame-thread verbs `api:pause` /
//!   `api:resume` / `api:stop` (the transport key's effect and the exit ritual, performed
//!   directly — see the verb list below).
//! - `POST /seek`   body `{"pos_ms":12345}` → `api:seek:12345`.
//! - `POST /audio`  body `{"sid":2}` → `api:audio:2` (a PMS `Stream.id` from `/state`).
//! - `POST /sub`    body `{"sid":3}` → `api:sub:3`.
//! - `POST /quality` body `{"q":"720p_4_mbps"}` → `api:quality:720p_4_mbps` — a rung of the
//!   playback-quality ladder (the persisted quality spellings, see the verb list). Applied
//!   LIVE to the stream on screen: the routing question is re-asked and the stream reloads
//!   only if the answer changed.
//! - `POST /profile` body `{"index":0}` (optional `"pin":"1234"`) → `api:profile:0:-` — commit
//!   a pick of the sign-in flow's "who's watching" roster (the same command the roster's rows
//!   land in). The debug build also answers the roster itself, once per launch, with the first
//!   user (this household's admin) — see `publish`.
//! - `GET /auth`    → the sign-in phase, the offered roster and the flow's error line (the
//!   same object `/state` embeds under `auth`).
//! - `GET /llm`     → the operator guide (Markdown): the whole API's shapes, the
//!   queued-write contract and the token vocabulary in one document an LLM can curl
//!   and immediately operate — the API's own onboarding.
//! - `GET|POST /shot` — the one binary answer in this API (a PNG, so it bypasses the
//!   JSON-only `reply`): a screenshot of the app's own UI plane — what the app draws (the
//!   home rows, the detail page, the player HUD) — read back from the GL surface by the
//!   frame thread before the swap. It is NOT the video overlay: the overlay lives on a
//!   hardware plane the read-back cannot see (the same limit `capture` documents; the shot
//!   discards the framebuffer's alpha for the same reason — a compositor instruction, not
//!   opacity). 200 `image/png`, or 504 when the frame loop presented nothing within the
//!   deadline.
//!
//! The `api:` verbs, performed on the frame thread by [`handle`] (all of them, by
//! construction, go through the media layer's own commit functions — the same ones the
//! track menu's picks land in — so nothing here re-implements a playback decision):
//! - `api:play:<rk>`  — the headless play ritual: `route::request_play_movie` (+ the
//!   detail request) then `playback::start_playback`, exactly what the dev live-play
//!   scenario does.
//! - `api:seek:<ms>`  — `player::request_seek(ms * 1_000_000)`.
//! - `api:audio:<sid>` — the `playing()` item's audio `Stream` whose `id == sid`, through
//!   `route::commit_audio_selection` with the panel's `audio_ordinal` mapping.
//! - `api:sub:<sid>`  — the subtitle `Stream` whose `id == sid`, through
//!   `route::commit_subtitle_selection` (`sub_idx` = its row, client-renderable).
//! - `api:quality:<rung>` — the `…` menu's own quality commit: `apply_more_action` with
//!   `SetQuality` — the routing question re-asked for the stream on screen, which reloads only
//!   when the answer changed (the live switch while the engine runs); a terminal engine gets
//!   the persist-then-fresh-retry path instead, the failure screen's Retry at the new rung.
//!   The rung is a persisted-quality spelling: `auto`, `original`, `1080p_20_mbps`,
//!   `1080p_8_mbps`, `720p_4_mbps`, `720p_2_mbps`, `480p_720_kbps`.
//! - `api:profile:<index>[:<pin>|-]` — `SelectProfile` on the session machine: the same
//!   commit the "who's watching" roster's rows land in. The outcome (Switching → Ready, or the
//!   flow's error line) is readable in `GET /auth`.
//! - `api:pause` / `api:resume` — the transport key's effect performed directly on the frame
//!   thread: `lifecycle::set_transport_paused` under the PAUSE/PLAY key's `transport_target`,
//!   plus the HUD linger a key press would extend — the same commit the player page's own
//!   PAUSE/PLAY arms land in. They go direct because the bare-key spelling (`pause`/`play`
//!   FIFO tokens) rides SDL key synthesis, which this television build never delivers to the
//!   event loop (measured: the pre-existing devtrigger FIFO is inert the same way).
//! - `api:stop` — the one leave-playback ritual the STOP key performs, by the same direct path:
//!   `playback::exit_player` (supersede the play, close the overlays, stop the buffer feed,
//!   return to the origin page, arm the Continue-Watching refresh).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::json;

use plx_base::eventlog::log;

/// `capture`'s port band plus ten: 8920 stable / 8921 debug / 8922 nightly. `capture`
/// owns 8910..8912 (its `default_port`'s three-way rule); this offset keeps the two
/// surfaces one rule apart and out of each other's way.
pub(crate) fn default_port() -> u16 {
    crate::capture::default_port() + 10
}

/// Command tokens the API thread has posted, drained once per frame by `run::ingest`.
static PENDING: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());
/// Staged plays: the rk the client asked for → the `PmsMovie` fetched from PMS on the API
/// thread, consumed (removed) by `handle` on the frame thread. A play whose fetch failed is
/// never parked here, so a drained `api:play:<rk>` with no entry is a stale request.
static PENDING_PLAYS: Mutex<Option<HashMap<String, plx_data::pms::PmsMovie>>> = Mutex::new(None);
/// The last frame's snapshot, republished by `run::report` into a string the API thread
/// hands back verbatim. `None` until the first frame (the app is booting).
static SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);
/// The last frame's auth section (`GET /auth`; embedded in `/state` as `auth` too): the
/// sign-in flow's phase, the offered "who's watching" roster (title, PIN flag, admin flag)
/// and the flow's error line, republished alongside `SNAPSHOT`.
static AUTH_SNAPSHOT: Mutex<Option<String>> = Mutex::new(None);
/// The debug-flavour roster default: the "who's watching" screen is answered with the FIRST
/// user (this household's admin) exactly once per app launch. `true` after the pick has been
/// posted — or stays `false` for the whole launch when the flow never reached a roster.
static PROFILE_DEFAULTED: AtomicBool = AtomicBool::new(false);
static UP_SINCE: OnceLock<Instant> = OnceLock::new();

/// Boot init: spawn the listener. Debug flavour only — see the module doc for the why.
/// The gate lives HERE (not at the call site) so no caller can start it in another
/// flavour: the stable app id and the host build (no app id at all) fall out of
/// `flavour()` and get a logged refusal instead of a bound port.
pub(crate) fn init() {
    let Some(flavour) = plx_base::paths::flavour() else {
        log("api: no TV flavour (host build) — command API not started");
        return;
    };
    if flavour != "debug" {
        log(&format!("api: {flavour} flavour — command API is debug-only, not started"));
        return;
    }
    let port = default_port();
    let listener = plx_base::task::spawn("api-listen", move || api_listen(port));
    match listener {
        Some(_) => log(&format!("api: listening on :{port}")),
        None => log("api: DISABLED — a worker thread was refused"),
    }
}

/// One command token: the API's write door. `run::ingest` calls this once per frame and
/// feeds every token to `ingress_token` — the single input path.
pub(crate) fn take_pending() -> Vec<String> {
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_empty() {
        return Vec::new();
    }
    guard.drain(..).collect()
}

/// Post one token to the frame loop. `tok` is bounded (a FIFO token, never a body).
fn push(tok: String) {
    let mut guard = PENDING.lock().unwrap_or_else(|e| e.into_inner());
    if guard.len() >= 64 {
        log("api: command queue full, dropping");
        return;
    }
    guard.push_back(tok);
}

// ---------------------------------------------------------------------------
// HTTP listener (API thread)
// ---------------------------------------------------------------------------

/// The listener: accept loop, one request per connection (an agent talks one curl at a
/// time), `Connection: close` on every answer. A refused socket or a wedged client costs
/// the request, never the listener.
fn api_listen(port: u16) {
    let Ok(listener) = TcpListener::bind(("0.0.0.0", port)) else {
        log(&format!("api: bind :{port} failed"));
        return;
    };
    log(&format!("api: bound :{port}"));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        serve(stream, port);
    }
}

fn serve(stream: TcpStream, port: u16) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some((method, path, body)) = read_request(&stream) else {
        return;
    };
    // The one binary answer: a PNG, so it cannot ride the JSON-only `reply`.
    if path == "/shot" && (method == "GET" || method == "POST") {
        shot_reply(&stream);
        return;
    }
    // The LLM's onboarding: Markdown, so it does not ride the JSON-only `reply` either.
    if path == "/llm" && method == "GET" {
        reply_ct(&stream, 200, "text/markdown; charset=utf-8", llm_guide());
        return;
    }
    let (status, json) = route(&method, &path, &body, port);
    reply(&stream, status, &json);
}

/// `GET|POST /shot` — the one binary answer in this API (a PNG, so it cannot ride the
/// JSON-only `reply`): a screenshot of the app's own UI plane. Arms an on-demand capture
/// on the frame thread and waits for the read-back: 200 `image/png` on a presented frame,
/// 504 when the frame loop produced nothing within the deadline (wedged loop, or a screen
/// that never drew). Debug-only by construction — the whole API is debug-gated in `init`.
fn shot_reply(mut stream: &TcpStream) {
    use std::io::Write;
    let Some(png) = crate::shot::api_capture(Duration::from_secs(8)) else {
        log("api: shot: no frame presented within the deadline");
        let body =
            json!({"error": "no frame presented within 8s (frame loop wedged or screen never drew)"}).to_string();
        let head = format!(
            "HTTP/1.1 504 Gateway Timeout\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body.as_bytes());
        return;
    };
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        png.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&png);
    log(&format!("api: shot: served {} bytes", png.len()));
}

/// Read `METHOD SP PATH SP HTTP/1.1` + headers + body (up to 64 KiB — a command body is
/// a few tokens). Returns `None` on any framing violation; the connection is dropped.
fn read_request(mut stream: &TcpStream) -> Option<(String, String, String)> {
    use std::io::Read;
    let mut buf: Vec<u8> = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    // headers: read until CRLF CRLF, bounded
    let mut header_end: Option<usize> = None;
    while header_end.is_none() && buf.len() < 16 * 1024 {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = find_crlf_crlf(&buf) {
            header_end = Some(p);
        }
    }
    let header_end = header_end?;
    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.lines();
    let request_line = lines.next()?;
    let mut it = request_line.split_whitespace();
    let method = it.next()?.to_string();
    let path = it.next()?.to_string();
    let mut content_length = 0usize;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else { continue };
        if k.trim().eq_ignore_ascii_case("content-length") {
            content_length = v.trim().parse().ok()?;
        }
    }
    if content_length > 64 * 1024 {
        return None;
    }
    let mut body = String::from_utf8_lossy(&buf[header_end + 4..]).into_owned();
    while body.len() < content_length {
        let n = stream.read(&mut chunk).ok()?;
        if n == 0 {
            return None;
        }
        body.push_str(&String::from_utf8_lossy(&chunk[..n]));
    }
    body.truncate(content_length);
    Some((method, path, body))
}

fn find_crlf_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn reply(stream: &TcpStream, status: u16, body: &str) {
    reply_ct(stream, status, "application/json", body);
}

/// One answer with an explicit content type: the JSON routes ride [`reply`], `/llm`
/// carries Markdown.
fn reply_ct(mut stream: &TcpStream, status: u16, content_type: &str, body: &str) {
    use std::io::Write;
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

/// The operator guide any LLM can curl (`GET /llm`): the whole API's shapes, the
/// queued-write contract and the token vocabulary in one document, so an agent that
/// is handed only this television's IP can operate it without prior knowledge.
/// Static by design — every fact in it is a contract of this module (the shapes are
/// the arms of [`route`], the vocabulary is `events::dispatch_remote_token`), so
/// there is nothing to compute and nothing to drift.
fn llm_guide() -> &'static str {
    r#"# plx-native command API — operator guide

You are talking to a running plx-native (a Plex TV client) on this television.
Base URL: the one you used to fetch this guide (port 8920 stable / 8921 debug /
8922 nightly). All answers are JSON except `GET /shot` (PNG) and this document
(Markdown).

## The one rule

Writes are QUEUED, not applied. `202 {"queued":"..."}` means accepted; the effect
lands on the next presented frame (~50 ms). A 202 proves nothing — re-`GET /state`
(or `GET /shot`) until it says what you expected.

## Read

- `GET /health` → `{"app","flavour","port","up_ms"}` — liveness.
- `GET /state` → the last frame's snapshot: `state` (one of `idle`, `resolving`,
  `connecting`, `buffering`, `seeking`, `playing`, `error`), `playing`, `paused`,
  `pos_ms`, `dur_ms`, `kbps`, `title`, `rk`, `kind`, `url`, `transcode`, `verdict`,
  `audio[]` and `subs[]` (each with `sid`, `selected`, `title`), and `auth` (the
  sign-in phase plus `profiles[]` with `index`, `title`, `pin`). 404 until the
  first frame.
- `GET /auth` → the sign-in phase, the offered profile roster, the flow's error.
- `GET /llm` → this guide.

## Write (POST, small JSON body)

- `/play`    body `{"rk":"33258"}` — a PMS ratingKey (digits). 404: PMS has no such
  item, or the session has no token yet (retry).
- `/stop`, `/pause`, `/resume` — body `{}`. Player transport.
- `/seek`    body `{"pos_ms":120000}`.
- `/audio`   body `{"sid":190844}` — a track id from `/state`'s `audio[]`.
- `/sub`     body `{"sid":238396}` — a track id from `/state`'s `subs[]`.
- `/quality` body `{"q":"720p_4_mbps"}` — a rung: `auto`, `original`,
  `1080p_20_mbps`, `1080p_8_mbps`, `720p_4_mbps`, `720p_2_mbps`, `480p_720_kbps`.
  Applied live to the stream on screen.
- `/profile` body `{"index":0}` (optional `"pin":"1234"`) — a "who's watching" pick
  from `GET /auth`'s roster.
- `/command` body `{"tok":"down"}` — one verbatim remote token (vocabulary below).

## Screenshot

`GET /shot` (or POST) → 200 `image/png` (1920x1080, ~1.2 s) or 504 when nothing is
presented in time. It captures the app's UI plane only: rows, menus, the player
HUD. The VIDEO FRAME IS NOT in it — it plays on a hardware overlay plane the
read-back cannot see, so during playback the video area reads back black. Each
capture also writes `shot-N.png` under `/tmp/<app id>/` on the TV.

## Token vocabulary for /command

- navigation: `up` `down` `left` `right`
- keys: `ok` (=`enter`=`select`), `back` (=`esc`), `pageup`, `pagedown`, `chup`,
  `chdown`, `play`, `pause`, `stop`, `ff`, `rew`, `playpause`, `exit`, `backspace`,
  `clear`
- press-and-hold: `okdown` then `okup` (opens the item menu); the general form is
  `holddown:<name>`, `holdrep:<name>`, `holdup:<name>`
- pointer (1920x1080 logical pixels): `ck:x,y` click, `pm:x,y` move, `pd:x,y` down,
  `pu:x,y` up
- `wheel:<dy>` — the settings wheel; `k:<sym>,<wcode>` — a raw SDL key escape
  (both fields decimal, both required)
- `txt:star+wars` — keyboard text (`+` = space)
- `diag` — the lab diagnostics upload; `pat:<spec>` — the test-pattern ground
  (lab harnesses, rarely wanted)

## Finding content

This API does not browse the library. To find a ratingKey: pull the
`X-Plex-Token=...` parameter out of `/state`'s `url` (present while anything has
played) and query the Plex Media Server yourself; the items' `ratingKey` field is
the `rk` for `/play`.

## First actions

1. `GET /health` — alive?
2. `GET /state` — what is on screen?
3. act — `POST /play {"rk":"..."}` or a `/command` token
4. re-`GET /state` until the state says what you expected
5. `GET /shot` — see the screen
"#
}

/// Route one request. `path` is the raw request target (no query strings in this API).
fn route(method: &str, path: &str, body: &str, port: u16) -> (u16, String) {
    match (path, method) {
        ("/health", "GET") => {
            let up = UP_SINCE.get().map(|t| t.elapsed().as_millis() as u64).unwrap_or(0);
            (
                200,
                json!({
                    "app": "plx-native",
                    "flavour": plx_base::paths::flavour().unwrap_or("stable"),
                    "port": port,
                    "up_ms": up,
                })
                .to_string(),
            )
        }
        ("/state", "GET") => match SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(s) => (200, s),
            None => (404, json!({"error": "no frame yet"}).to_string()),
        },
        ("/play", "POST") => play(body),
        ("/command", "POST") => command(body),
        ("/pause", "POST") => passthrough("api:pause"),
        ("/resume", "POST") => passthrough("api:resume"),
        ("/stop", "POST") => passthrough("api:stop"),
        ("/seek", "POST") => seek(body),
        ("/audio", "POST") => track(body, "api:audio:"),
        ("/sub", "POST") => track(body, "api:sub:"),
        ("/quality", "POST") => quality(body),
        ("/profile", "POST") => profile(body),
        ("/auth", "GET") => match AUTH_SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            Some(s) => (200, s),
            None => (404, json!({"error": "no frame yet"}).to_string()),
        },
        _ => {
            if method == "GET" {
                (404, json!({"error": "no such route"}).to_string())
            } else {
                (405, json!({"error": "method not allowed"}).to_string())
            }
        }
    }
}

/// `POST /play {"rk":"1234"}` — resolve the item from PMS ON THIS THREAD (never the
/// frame thread), park it, post the token. The 404s distinguish "PMS does not hold it"
/// from "the session has no token yet" so an agent knows which to retry.
fn play(body: &str) -> (u16, String) {
    let Some(rk) = body_field(body, "rk") else {
        return (400, json!({"error": "missing \"rk\""}).to_string());
    };
    if !rk.bytes().all(|b| b.is_ascii_digit()) || rk.len() > 20 {
        return (400, json!({"error": "\"rk\" must be a PMS ratingKey"}).to_string());
    }
    let client = plx_plex::plex::client();
    let Some(meta) = client.metadata(&rk) else {
        return (
            404,
            json!({"error": "PMS has no such item (or the session has no token yet — retry)"}).to_string(),
        );
    };
    let sid = plx_media::route::surface_sid();
    let pmm = plx_data::pms::parse_item(&meta, sid);
    if pmm.part.is_empty() {
        return (404, json!({"error": "item has no playable part"}).to_string());
    }
    {
        let mut guard = PENDING_PLAYS.lock().unwrap_or_else(|e| e.into_inner());
        let map = guard.get_or_insert_with(HashMap::new);
        map.insert(rk.clone(), pmm);
    }
    let tok = format!("api:play:{rk}");
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

/// `POST /command {"tok":"pause"}` — verbatim passthrough into the token vocabulary:
/// key names (`up`, `ok`, `stop`, …), `k:<sym>,<wcode>`, `txt:`, `pat:`, `wheel:`.
fn command(body: &str) -> (u16, String) {
    let Some(tok) = body_field(body, "tok") else {
        return (400, json!({"error": "missing \"tok\""}).to_string());
    };
    if tok.len() > 256 || tok.is_empty() || tok.contains('\0') {
        return (400, json!({"error": "\"tok\" out of bounds"}).to_string());
    }
    let tok = tok.clone();
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

fn passthrough(tok: &str) -> (u16, String) {
    let tok = tok.to_string();
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

/// `POST /seek {"pos_ms":12345}`.
fn seek(body: &str) -> (u16, String) {
    let Some(ms) = body_num(body, "pos_ms") else {
        return (400, json!({"error": "missing \"pos_ms\""}).to_string());
    };
    if ms < 0 || ms > 24 * 3600 * 1000 {
        return (400, json!({"error": "\"pos_ms\" out of bounds"}).to_string());
    }
    let tok = format!("api:seek:{ms}");
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

/// `POST /audio {"sid":2}` / `POST /sub {"sid":3}` — a PMS `Stream.id` (the `sid`s in
/// `/state`'s track lists).
fn track(body: &str, prefix: &str) -> (u16, String) {
    let Some(sid) = body_num(body, "sid") else {
        return (400, json!({"error": "missing \"sid\""}).to_string());
    };
    let tok = format!("{prefix}{sid}");
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

/// `POST /quality {"q":"720p_4_mbps"}` — a rung of the playback-quality ladder, by its
/// persisted spelling (the `PlaybackQuality` wire names — the file-format contract, so the
/// API and the settings screen can never name the same rung differently). A rung that does
/// not parse is a 400: an agent's typo never reaches the frame thread.
fn quality(body: &str) -> (u16, String) {
    let Some(q) = body_field(body, "q") else {
        return (400, json!({"error": "missing \"q\""}).to_string());
    };
    if serde_json::from_value::<plx_media::route::Quality>(serde_json::Value::String(q.clone())).is_err() {
        return (
            400,
            json!({"error": "unknown rung (want auto | original | 1080p_20_mbps | 1080p_8_mbps | 720p_4_mbps | 720p_2_mbps | 480p_720_kbps)"}).to_string(),
        );
    }
    let tok = format!("api:quality:{q}");
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

/// `POST /profile {"index":0[, "pin":"1234"]}` — a pick of the "who's watching" roster,
/// as `/auth`'s `profiles` index it. The PIN is the digits the roster row asks for (its
/// `pin` flag is `true`); a profile that needs none refuses a supplied PIN the same way the
/// screen does.
fn profile(body: &str) -> (u16, String) {
    let Some(index) = body_num(body, "index") else {
        return (400, json!({"error": "missing \"index\""}).to_string());
    };
    if index < 0 || index > 63 {
        return (400, json!({"error": "\"index\" out of bounds (see /auth)"}).to_string());
    }
    let pin = body_field(body, "pin").unwrap_or_default();
    if !pin.is_empty() && (!pin.bytes().all(|b| b.is_ascii_digit()) || pin.len() > 8) {
        return (400, json!({"error": "\"pin\" must be digits (at most 8)"}).to_string());
    }
    let tok = format!("api:profile:{index}:{}", if pin.is_empty() { "-" } else { pin.as_str() });
    push(tok.clone());
    (202, json!({"queued": tok}).to_string())
}

// Minimal JSON field reads — the bodies are single-key literals the agent composes;
// `serde_json::Value` is the parser, nothing is deserialised into typed structs.
fn body_value(body: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    let v = serde_json::from_str::<serde_json::Value>(body).ok()?;
    Some(v.as_object()?.clone())
}

fn body_field(body: &str, key: &str) -> Option<String> {
    let v = body_value(body)?;
    v.get(key)?
        .as_str()?
        .trim()
        .to_string()
        .into()
}

fn body_num(body: &str, key: &str) -> Option<i64> {
    let v = body_value(body)?;
    v.get(key)?.as_i64()
}

// ---------------------------------------------------------------------------
// Frame-thread half
// ---------------------------------------------------------------------------

/// Perform an `api:` verb. Called from `run::ingress_token` with the prefix already
/// peeled. Returns whether the frame loop may consider the token dispatched (the
/// passthrough path returns what the dispatcher said; the `api:` verbs answer their
/// own acceptance here).
pub(crate) unsafe fn handle(app: &mut crate::app::App, fr: &mut crate::app::run::Frame, spec: &str) -> bool {
    let (kind, arg) = match spec.split_once(':') {
        Some(pair) => pair,
        None => (spec, ""),
    };
    match kind {
        "play" => handle_play(app, fr, arg),
        "pause" => handle_transport(app, fr, false),
        "resume" => handle_transport(app, fr, true),
        "stop" => handle_stop(app, fr),
        "seek" => match arg.parse::<i64>() {
            Ok(ms) if ms >= 0 => {
                plx_media::player::request_seek(ms * 1_000_000);
                true
            }
            _ => false,
        },
        "audio" => handle_track(app, fr, arg, true),
        "sub" => handle_track(app, fr, arg, false),
        "quality" => handle_quality(app, fr, arg),
        "profile" => handle_profile(app, fr, arg),
        _ => false,
    }
}

/// The headless play ritual — the same three calls the dev live-play scenario makes, in
/// the same order: request the item (which owns the resolve), ask for its detail (the
/// HUD's context), then seat the engine.
unsafe fn handle_play(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    rk: &str,
) -> bool {
    let Some(pmm) = PENDING_PLAYS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .and_then(|map| map.remove(rk))
    else {
        log(&format!("api: play rk {rk}: no staged item (stale request)"));
        return false;
    };
    let ctx = format!("api play rk {rk}");
    let requested = plx_media::route::request_play_movie(
        &mut app.player.session,
        app.bridge.metadata_mut(),
        &pmm,
        &ctx,
    );
    if requested {
        app.bridge.metadata_mut().run(
            plx_data::stores::metadata::MetadataCmd::RequestDetail {
                sid: pmm.sid,
                rk: pmm.rk.clone(),
            },
        );
        crate::app::playback::start_playback(
            &mut app.player.session,
            &mut app.adapters.player,
            0,
            crate::app::playback::Origin::Here,
            plx_screens::player::input::HUD_HEADLESS_MS,
            None,
            &mut app.pages,
            &mut app.bridge,
        );
    }
    requested
}

/// `api:pause` / `api:resume` — the transport key's effect, performed directly on the frame
/// thread (the bare-key spelling rides SDL synthesis, which this TV build does not deliver —
/// see the module doc). `play` is the key's `Transport` payload: `true` = the PLAY key
/// (resume), `false` = the PAUSE key. Seated-engine guard: `sink().pause` is a Starfish call
/// and a session with no engine (idle, resolving, connecting, errored) has nothing for it to
/// reach — those answer a log line and a false, never a guess at an engine.
unsafe fn handle_transport(
    app: &mut crate::app::App,
    fr: &mut crate::app::run::Frame,
    play: bool,
) -> bool {
    let verb = if play { "resume" } else { "pause" };
    let st = plx_media::player::state(&app.player.session);
    if !matches!(
        st,
        plx_media::player::PlaybackState::Buffering
            | plx_media::player::PlaybackState::Seeking
            | plx_media::player::PlaybackState::Playing
    ) {
        log(&format!("api: {verb}: no seated engine (state {st:?})"));
        return false;
    }
    let target = plx_media::player::lifecycle::transport_target(
        Some(play),
        plx_media::player::lifecycle::viewer_paused(),
    );
    let ok = plx_media::player::lifecycle::set_transport_paused(&mut app.adapters.player, target);
    if !ok {
        log(&format!("api: {verb}: transport commit refused"));
    }
    if let Some(player) = crate::app::bridge::player_mut(&mut app.pages) {
        player.hud.extend(fr.now, plx_screens::player::input::HUD_LINGER_MS);
        player.publish();
    }
    ok
}

/// `api:stop` — the one leave-playback ritual the STOP key performs, by the same direct path:
/// `playback::exit_player` (supersede the play, close the overlays, stop the buffer feed,
/// return to the origin page, arm the Continue-Watching refresh). An idle session has nothing
/// to leave.
unsafe fn handle_stop(app: &mut crate::app::App, _fr: &mut crate::app::run::Frame) -> bool {
    let st = plx_media::player::state(&app.player.session);
    if matches!(st, plx_media::player::PlaybackState::Idle) {
        log("api: stop: nothing to leave (state Idle)");
        return false;
    }
    crate::app::playback::exit_player(
        &mut app.player.session,
        &mut app.adapters.player,
        &mut app.refresh_hubs,
        &mut app.pages,
    );
    true
}

/// `api:audio:<sid>` / `api:sub:<sid>` — find the `playing()` item's track row by PMS
/// stream id and commit it through the media layer's own selection functions (the bare
/// `_selection` commits the ladder uses: no viewer memo is remembered, which is right
/// for an agent).
unsafe fn handle_track(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    arg: &str,
    audio: bool,
) -> bool {
    let Ok(sid) = arg.parse::<i64>() else {
        return false;
    };
    let views = app.bridge.views();
    let Some(item) = views.metadata.playing() else {
        return false;
    };
    if audio {
        let Some(i) = item.audio.iter().position(|s| s.id == sid) else {
            return false;
        };
        let ord = plx_data::metadata::audio_ordinal(&item.audio, i);
        plx_media::route::commit_audio_selection(
            &mut app.player.session,
            plx_media::route::CarriedAudio::from_stream(&item.audio[i], ord),
        )
    } else {
        let Some(i) = item.subs.iter().position(|s| s.id == sid) else {
            return false;
        };
        let s = &item.subs[i];
        // A picked track is client-renderable by definition (an embedded demux ordinal
        // or a sidecar the client draws); `Off` is the only non-renderable pick and it
        // has no sid to arrive here with.
        plx_media::route::commit_subtitle_selection(&mut app.player.session, i as i32, s.id, true);
        true
    }
}

/// `api:quality:<rung>` — the \u2026 menu\u2019s own quality commit, performed through the menu\u2019s own
/// function: `apply_more_action` re-asks the routing question for the stream on screen and
/// reloads only if the answer changed (the live switch while the engine runs); a terminal
/// engine gets the persist-then-fresh-retry path instead — the failure screen\u2019s Retry at the
/// new rung.
unsafe fn handle_quality(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    arg: &str,
) -> bool {
    let Ok(q) = serde_json::from_value::<plx_media::route::Quality>(serde_json::Value::String(arg.to_string())) else {
        log(&format!("api: quality: unknown rung {arg}"));
        return false;
    };
    crate::app::playback::apply_more_action(
        &mut app.player.session,
        &mut app.adapters.player,
        &mut app.bridge,
        plx_appkit::more_menu::Action::SetQuality(q),
    );
    log(&format!("api: quality: rung set to {arg}"));
    true
}

/// `api:profile:<index>[:<pin>|-]` — commit a roster pick through the session machine\u2019s own
/// command (the same commit the roster\u2019s rows land in). The pick moves the flow to Switching;
/// its outcome is visible in `GET /auth` (phase, error).
unsafe fn handle_profile(
    app: &mut crate::app::App,
    _fr: &mut crate::app::run::Frame,
    arg: &str,
) -> bool {
    let Some((idx, pin)) = arg.split_once(':') else {
        return false;
    };
    let Ok(index) = idx.parse::<usize>() else {
        return false;
    };
    let pin = (pin != "-").then(|| pin.to_string());
    log(&format!(
        "api: profile: select index {index} (pin {})",
        if pin.is_some() { "given" } else { "none" }
    ));
    crate::app::bridge::execute_session_command(
        &mut app.pages,
        plx_session::auth::SessionCmd::SelectProfile { index, pin },
    );
    true
}

/// Republish the snapshot, called from `run::report` once per frame. The string form is
/// what `GET /state` answers verbatim; building it here (frame thread) means the API
/// thread never borrows the app.
pub(crate) unsafe fn publish(app: &mut crate::app::App, fr: &mut crate::app::run::Frame) {
    let ps = &app.player.session;
    let st = plx_media::player::state(ps);
    let state_name = match st {
        plx_media::player::PlaybackState::Idle => "idle",
        plx_media::player::PlaybackState::Resolving => "resolving",
        plx_media::player::PlaybackState::Connecting => "connecting",
        plx_media::player::PlaybackState::Buffering => "buffering",
        plx_media::player::PlaybackState::Seeking => "seeking",
        plx_media::player::PlaybackState::Playing => "playing",
        plx_media::player::PlaybackState::Paused => "paused",
        plx_media::player::PlaybackState::Error => "error",
    };
    let views = app.bridge.views();
    // The debug-flavour roster default (the user\u2019s rule): the \u201cwho\u2019s watching\u201d screen is
    // answered with the FIRST user \u2014 this household\u2019s admin \u2014 exactly once per app launch, so a
    // debug install never sits on the picker. The flow\u2019s own phase change (Profiles \u2192
    // Switching) ends the condition; the flag keeps a refused pick from re-posting every frame.
    // An explicit `POST /profile` still reaches every other user.
    if plx_base::paths::flavour() == Some("debug") {
        let snap = views.auth.0;
        if snap.phase == plx_session::auth::Phase::Profiles
            && !snap.users.is_empty()
            && !PROFILE_DEFAULTED.swap(true, Ordering::Relaxed)
        {
            // The user's own name stays OUT of the event log: the scrub gate bans the `.title`
            // spelling in a log call (household identity is scrubbed at write time anyway).
            log(&format!(
                "api: roster: auto-selecting first user (user 1 of {})",
                snap.users.len()
            ));
            push("api:profile:0:-".to_string());
        }
    }
    // This frame\u2019s auth section: `GET /auth`\u2019s answer, and what `/state` embeds under `auth`.
    let auth = {
        let snap = views.auth.0;
        json!({
            "phase": format!("{:?}", snap.phase).to_lowercase(),
            "error": if snap.error.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::String(snap.error.to_string())
            },
            "profiles": snap.users.iter().enumerate().map(|(i, u)| {
                json!({
                    "index": i,
                    "title": u.title,
                    "pin": u.protected,
                    "admin": u.admin,
                })
            }).collect::<Vec<_>>(),
        })
    };
    let now = views.metadata.now_playing();
    let playing = views.metadata.playing();
    let tracks = |item: Option<&plx_data::metadata::PlayingItem>, audio: bool| -> Vec<serde_json::Value> {
        let Some(item) = item else {
            return Vec::new();
        };
        let list: &Vec<plx_data::metadata::Stream> = if audio { &item.audio } else { &item.subs };
        list.iter().enumerate().map(|(i, s)| {
            let mut o = json!({
                "sid": s.id,
                "ord": plx_data::metadata::audio_ordinal(&list, i),
                "codec": s.codec,
                "lang": s.lang,
                "title": s.title,
                "selected": s.selected,
            });
            if audio {
                o["ch"] = json!(s.channels);
                o["layout"] = json!(s.layout);
                o["kbits"] = json!(s.bitrate);
                o["default"] = json!(s.default);
            } else {
                o["external"] = json!(s.external);
                o["forced"] = json!(s.forced);
            }
            o
        }).collect()
    };
    let verdict = plx_media::route::play_verdict(ps).map(|s| s.to_string());
    let mut obj = json!({
        "t": fr.now,
        "state": state_name,
        "state_id": st as u8,
        "playing": plx_media::player::is_playing(ps),
        // The viewer's pause — `true` while the transport is paused for the viewer (a claim
        // hold's own pause is NOT the viewer's and stays out of this lens).
        "paused": plx_media::player::lifecycle::viewer_paused(),
        "pos_ms": plx_media::player::playpos_ns() / 1_000_000,
        "dur_ms": plx_media::player::duration_ns() / 1_000_000,
        "title": now.map(|n| n.title.clone()).unwrap_or_default(),
        "ep_title": now.map(|n| n.ep_title.clone()).unwrap_or_default(),
        "rk": playing.map(|p| p.rk.clone()).unwrap_or_default(),
        "kind": playing.map(|p| p.kind.clone()).unwrap_or_default(),
        "url": plx_media::route::url(ps),
        "transcode": plx_media::route::transcode_session(ps),
        "kbps": plx_media::route::transport_kbps(ps),
        "audio_sid": plx_media::route::cur_audio_sid(ps),
        "verdict": verdict,
        "audio": tracks(playing, true),
        "subs": tracks(playing, false),
    });
    obj["auth"] = auth.clone();
    let json = obj.to_string();
    *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(json);
    *AUTH_SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(auth.to_string());
    let _ = UP_SINCE.get_or_init(Instant::now);
}
