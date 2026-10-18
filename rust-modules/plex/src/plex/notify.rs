//! The PMS notification feed (server-sent events): a long-lived read that tells this client what
//! other clients are doing on a server, so the screens can re-read what changed instead of waiting
//! for the next poll. `docs/pms-api.md` §9 is the measured contract; the bundled OpenAPI spec names
//! the path and the query parameter differently and is not the authority here.
//!
//! The feed carries `playing` (other clients' sessions) and `timeline` (library items created or
//! deleted); [`classify`] reads both. **The `timeline` frame shape is the one captured from a real
//! server** (`docs/pms-api.md` §9): `event: timeline`, `data: {"TimelineEntry":{...}}`, one OBJECT per
//! frame, no `NotificationContainer`. A delete has not been observed. The scrubbed frames it is
//! tested against live in `notify/fixtures.rs`.
//!
//! This file is the PROTOCOL half and nothing else: [`SseParser`] turns bytes into frames,
//! [`classify`] turns a frame into a [`Notification`], and [`Client::notifications`] runs one
//! connection over [`crate::http::stream`] until it ends. WHEN to connect, how long to back off and
//! what a reconnect owes the screens is the data layer's (`plx_data::stores::notify`).
//!
//! **A notification is a hint about a stranger's activity, never an event of ours.** `playing`
//! sessions describe whoever is playing on the server, this device included. The own-session filter
//! lives here ([`classify`] drops sessions carrying this client's identifier) so no consumer has to
//! remember it, and nothing in this module touches a timeline, a stop or a resume point.

use super::client::{pms_headers, Client, QueryBuilder};
use super::identity::{device_name, DEVICE};
use plx_net::net::StreamEnd;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

/// `Accept` for the feed. The server answers `text/event-stream` to it.
const ACCEPT_EVENT_STREAM: &str = "Accept: text/event-stream";

/// The event types the feed asks for. `timeline` is the library's creates and deletes; the comma
/// list (percent-encoded) was measured to deliver `timeline` frames (`docs/pms-api.md` §9).
const FILTERS: &str = "playing,timeline";

/// How long the feed may be silent before the read ends as [`NotifyEnd::Silent`]. The server pings
/// about every 10 s (`docs/pms-api.md` §9), so 25 s is two missed pings and a margin.
pub const IDLE: Duration = Duration::from_secs(25);

/// A line longer than this is dropped whole; a frame whose data grows past [`MAX_DATA`] is dropped.
/// Both bound a hostile or broken server, not a real one (a `playing` frame is a few hundred bytes).
const MAX_LINE: usize = 64 * 1024;
const MAX_DATA: usize = 256 * 1024;

/// One dispatched SSE frame: a comment line (the server's ping) or a message with its event name
/// and joined `data` lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SseFrame {
    Comment,
    Message { event: String, data: String },
}

/// A byte-fed SSE parser: CRLF, CR and LF line ends, multi-line `data`, `:` comments, dispatch on a
/// blank line. Bytes may be split anywhere, including inside a CRLF.
#[derive(Default)]
pub struct SseParser {
    line: Vec<u8>,
    /// The previous byte was a CR, so a following LF belongs to the same line end.
    after_cr: bool,
    /// The current line outgrew [`MAX_LINE`]; its remainder is skipped.
    line_overflow: bool,
    event: String,
    data: String,
    has_data: bool,
    data_overflow: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; `out` is called once per dispatched frame, in order.
    pub fn feed(&mut self, bytes: &[u8], out: &mut dyn FnMut(SseFrame)) {
        for &b in bytes {
            if std::mem::take(&mut self.after_cr) && b == b'\n' {
                continue;
            }
            match b {
                b'\r' | b'\n' => {
                    self.after_cr = b == b'\r';
                    self.end_line(out);
                }
                _ if self.line.len() >= MAX_LINE => self.line_overflow = true,
                _ => self.line.push(b),
            }
        }
    }

    fn end_line(&mut self, out: &mut dyn FnMut(SseFrame)) {
        let overflow = std::mem::take(&mut self.line_overflow);
        let raw = std::mem::take(&mut self.line);
        if overflow {
            // A line is missing from whatever frame this is: dispatching it would hand over a
            // frame with its middle gone. The next blank line discards the frame.
            self.data_overflow = true;
            return;
        }
        if raw.is_empty() {
            let event = std::mem::take(&mut self.event);
            let data = std::mem::take(&mut self.data);
            let has_data = std::mem::take(&mut self.has_data);
            if std::mem::take(&mut self.data_overflow) || !has_data {
                return;
            }
            let event = if event.is_empty() { "message".to_owned() } else { event };
            out(SseFrame::Message { event, data });
            return;
        }
        if raw[0] == b':' {
            out(SseFrame::Comment);
            return;
        }
        let (name, value) = match raw.iter().position(|&c| c == b':') {
            Some(i) => (&raw[..i], raw[i + 1..].strip_prefix(b" ").unwrap_or(&raw[i + 1..])),
            None => (&raw[..], &raw[raw.len()..]),
        };
        let value = String::from_utf8_lossy(value);
        match name {
            b"event" => self.event = value.into_owned(),
            b"data" => {
                if self.has_data {
                    self.data.push('\n');
                }
                self.has_data = true;
                if self.data.len() + value.len() > MAX_DATA {
                    self.data_overflow = true;
                } else {
                    self.data.push_str(&value);
                }
            }
            _ => {}
        }
    }
}

/// How a play session is going, as the server states it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Playing,
    Paused,
    Buffering,
    Stopped,
    Other,
}

/// One entry of a `playing` notification. Every field is optional on the wire; a missing one is
/// `None`, never a guess.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaySession {
    pub session_key: Option<String>,
    pub client_identifier: Option<String>,
    pub rating_key: Option<String>,
    pub key: Option<String>,
    pub view_offset_ms: Option<i64>,
    pub state: PlayState,
}

/// What kind of library item a `timeline` entry names. Only the two kinds the Watchlist matches
/// by guid are kept; every other type (episodes, music, photos) is dropped by [`classify`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryKind {
    Movie,
    Show,
}

/// A library item's lifecycle as a finished `timeline` entry reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LibraryState {
    /// The scanner finished the item: state 5 with no `metadataState` and no `mediaState`, the LAST
    /// frame of its sequence. State 5 also opens the sequence (with `metadataState: "processing"`),
    /// before the item has its metadata, so state alone is not "finished".
    Created,
    /// The item left the library (state 9). **UNVERIFIED: no delete has been captured**; 9 is the
    /// community-documented value and the frame's other fields are unknown.
    Deleted,
}

/// One finished create or delete of a movie or show. The wire names an `itemID` (the item's
/// `ratingKey` on that server), never a guid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryEntry {
    pub item_id: String,
    pub kind: LibraryKind,
    pub state: LibraryState,
    /// The library section's id, when the entry named one. Informational: nothing keys on it.
    pub section: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notification {
    /// Sessions other than ours (the own-session filter already ran). Never empty.
    Play(Vec<PlaySession>),
    /// Movies or shows the library finished creating or deleted. Never empty; scan progress,
    /// metadata updates and every other type are already dropped.
    Library(Vec<LibraryEntry>),
    /// The server's keep-alive: a comment line or a `ping` event.
    Ping,
    /// Anything else: an event type this client does not use, or a frame that is not JSON.
    Ignored,
}

/// Turn one frame into a [`Notification`], dropping every session whose client identifier is
/// `own_client_id`. A `playing` frame that held only our own sessions is `Ignored`, as is a
/// `timeline` frame with no finished movie or show create or delete in it.
pub fn classify(frame: &SseFrame, own_client_id: &str) -> Notification {
    let (event, data) = match frame {
        SseFrame::Comment => return Notification::Ping,
        SseFrame::Message { event, data } => (event, data),
    };
    if event == "ping" {
        return Notification::Ping;
    }
    let Ok(root) = serde_json::from_str::<serde_json::Value>(data) else {
        return Notification::Ignored;
    };
    // A captured `timeline` frame has no `NotificationContainer`: the event name is its type and the
    // data is `{"TimelineEntry":{...}}`. The community envelope (`type` inside the container) is
    // still read, for `playing` frames, whose real shape has not been captured here.
    let container = if root["NotificationContainer"].is_object() { &root["NotificationContainer"] } else { &root };
    if event == "timeline" {
        return library(container);
    }
    match container["type"].as_str() {
        Some("timeline") => return library(container),
        Some(kind) if kind != "playing" => return Notification::Ignored,
        _ => {}
    }
    let Some(entries) = container["PlaySessionStateNotification"].as_array() else {
        return Notification::Ignored;
    };
    let sessions: Vec<PlaySession> = entries
        .iter()
        .filter(|entry| entry.is_object())
        .map(play_session)
        .filter(|s| own_client_id.is_empty() || s.client_identifier.as_deref() != Some(own_client_id))
        .collect();
    if sessions.is_empty() { Notification::Ignored } else { Notification::Play(sessions) }
}

/// The `identifier` a library entry carries; another plugin's timeline (music, a channel) is not ours.
const LIBRARY_IDENTIFIER: &str = "com.plexapp.plugins.library";
/// `TimelineEntry.type` of a movie and of a show, and the `state` values of a processed item and of a
/// delete (UNVERIFIED, never captured). Every other state is scan progress. Captured states: 2, 3, 4
/// and 5; a movie runs 5 (`metadataState` processing), 4 (loading), 5 (no `metadataState`).
const TYPE_MOVIE: i64 = 1;
const TYPE_SHOW: i64 = 2;
const STATE_PROCESSED: i64 = 5;
const STATE_DELETED: i64 = 9;

/// The finished movie and show creates and deletes of a `timeline` payload. `TimelineEntry` is one
/// object on the wire (an array is read too, as the community docs show it). An entry missing its
/// `itemID`, `type` or `state` is dropped, never guessed at; numbers may arrive as strings.
fn library(payload: &serde_json::Value) -> Notification {
    let entry = &payload["TimelineEntry"];
    let kept: Vec<LibraryEntry> = match entry {
        serde_json::Value::Array(entries) => entries.iter().filter_map(library_entry).collect(),
        serde_json::Value::Object(_) => library_entry(entry).into_iter().collect(),
        _ => return Notification::Ignored,
    };
    if kept.is_empty() { Notification::Ignored } else { Notification::Library(kept) }
}

fn library_entry(entry: &serde_json::Value) -> Option<LibraryEntry> {
    if entry["identifier"].as_str().is_some_and(|id| id != LIBRARY_IDENTIFIER) {
        return None;
    }
    let item_id = text(&entry["itemID"]).filter(|id| !id.is_empty())?;
    let kind = match whole(&entry["type"])? {
        TYPE_MOVIE => LibraryKind::Movie,
        TYPE_SHOW => LibraryKind::Show,
        _ => return None,
    };
    let state = match whole(&entry["state"])? {
        STATE_PROCESSED if in_progress(entry) => return None,
        STATE_PROCESSED => LibraryState::Created,
        STATE_DELETED => LibraryState::Deleted,
        _ => return None,
    };
    Some(LibraryEntry { item_id, kind, state, section: text(&entry["sectionID"]) })
}

/// State 5 opens a sequence too, flagged by a `metadataState` ("processing") or, for analysis, a
/// `mediaState`; the finished frame carries neither.
fn in_progress(entry: &serde_json::Value) -> bool {
    ["metadataState", "mediaState"].iter().any(|k| text(&entry[*k]).is_some_and(|v| !v.is_empty()))
}

/// A whole number the server wrote as a number or as a numeric string.
fn whole(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn play_session(entry: &serde_json::Value) -> PlaySession {
    PlaySession {
        session_key: text(&entry["sessionKey"]),
        client_identifier: text(&entry["clientIdentifier"]),
        rating_key: text(&entry["ratingKey"]),
        key: text(&entry["key"]),
        view_offset_ms: match &entry["viewOffset"] {
            serde_json::Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
            serde_json::Value::String(s) => s.trim().parse().ok(),
            _ => None,
        },
        state: match entry["state"].as_str() {
            Some("playing") => PlayState::Playing,
            Some("paused") => PlayState::Paused,
            Some("buffering") => PlayState::Buffering,
            Some("stopped") => PlayState::Stopped,
            _ => PlayState::Other,
        },
    }
}

/// A string field, or a number the server wrote where a string was expected (ids are both).
fn text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Why one connection ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyEnd {
    /// The caller cancelled it.
    Cancelled,
    /// Nothing arrived for [`IDLE`].
    Silent,
    /// The server closed the stream.
    Closed,
    /// The server answered with this status instead of a stream, or the request was refused before
    /// a socket was opened (`status` 0: no token, data I/O off, the credential gate).
    Refused { status: u16 },
    /// The connection failed (DNS, connect, TLS, a broken read).
    Transport,
    /// This build or device cannot stream (no libcurl, or a libcurl without the progress hook).
    Unsupported,
}

impl NotifyEnd {
    fn of(end: StreamEnd) -> NotifyEnd {
        match end {
            StreamEnd::Closed => NotifyEnd::Closed,
            StreamEnd::Idle => NotifyEnd::Silent,
            // A consumer that stops reading is the caller's decision, as a cancel is.
            StreamEnd::Cancelled | StreamEnd::Consumer => NotifyEnd::Cancelled,
            StreamEnd::Status(status) => NotifyEnd::Refused { status },
            StreamEnd::Transport(_) => NotifyEnd::Transport,
            StreamEnd::Unsupported => NotifyEnd::Unsupported,
            StreamEnd::Refused => NotifyEnd::Refused { status: 0 },
        }
    }
}

impl Client {
    /// The identity headers the feed's request carries: who this device is, so the server groups
    /// the connection under the one device it already knows. `X-Plex-Provides` is deliberately
    /// absent: it would announce a controllable player, and this client is not one.
    pub(super) fn notify_identity_headers(&self) -> Vec<String> {
        vec![
            format!("X-Plex-Client-Identifier: {}", self.client_id),
            format!("X-Plex-Product: {}", self.product),
            format!("X-Plex-Version: {}", self.version),
            format!("X-Plex-Platform: {}", self.platform),
            format!("X-Plex-Platform-Version: {}", super::identity::platform_version()),
            format!("X-Plex-Device: {DEVICE}"),
            format!("X-Plex-Device-Name: {}", device_name()),
        ]
    }

    /// Does this client hold a token? A blank slot (a profile switch in flight, a sign-out) has
    /// nothing to authenticate a stream with. The token itself never leaves the client.
    pub fn has_token(&self) -> bool {
        self.token.read().is_ok_and(|t| !t.is_empty())
    }

    /// May this server's token ride its origin RIGHT NOW (TLS, a developer build, or a live
    /// consented grant)? The pure read of the gate `http::stream` applies, asked before a dial so a
    /// refused origin is not connected to at all (a foreground clears every plaintext grant until
    /// discovery mints it again).
    pub fn credential_transport_open(&self) -> bool {
        crate::plex::grant::credential_allowed(&self.origin)
    }

    /// One connection to the notification feed, run to its end on the calling worker.
    /// `on` receives each [`Notification`] as it arrives, pings included and own sessions already
    /// dropped. Refused up front, without a socket, when the token is blank or data I/O is off.
    #[track_caller]
    pub fn notifications(
        &self,
        _off: &plx_base::task::OffFrame,
        cancel: &AtomicBool,
        on: &mut dyn FnMut(Notification),
    ) -> NotifyEnd {
        if self.data_io_disabled() || !self.has_token() {
            return NotifyEnd::Refused { status: 0 };
        }
        let path = self.with_token(
            &QueryBuilder::new("/:/eventsource/notifications").str("filters", FILTERS).build(),
        );
        let mut owned = pms_headers(&[ACCEPT_EVENT_STREAM]);
        owned.extend(self.notify_identity_headers());
        let headers: Vec<&str> = owned.iter().map(String::as_str).collect();
        let mut parser = SseParser::new();
        let mut deliver = |bytes: &[u8]| {
            parser.feed(bytes, &mut |frame| match classify(&frame, &self.client_id) {
                Notification::Ignored => {}
                n => on(n),
            });
            true
        };
        NotifyEnd::of(crate::http::stream(
            &self.origin,
            &path,
            &headers,
            self.resolve_pin.as_ref(),
            IDLE,
            cancel,
            &mut deliver,
        ))
    }
}

/// Timeline frames the parser is tested against: the captured shape, scrubbed.
#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plex::origin::Origin;
    use crate::plex::servers::ServerId;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::fixtures;

    fn frames(chunks: &[&[u8]]) -> Vec<SseFrame> {
        let mut p = SseParser::new();
        let mut out = Vec::new();
        for c in chunks {
            p.feed(c, &mut |f| out.push(f));
        }
        out
    }

    fn msg(event: &str, data: &str) -> SseFrame {
        SseFrame::Message { event: event.to_owned(), data: data.to_owned() }
    }

    fn client(origin: Origin, token: &str) -> Client {
        Client::new(ServerId::UNSET, "machine", origin, token, "cid-own")
    }

    #[test]
    fn the_parser_joins_a_frame_split_across_chunks() {
        let got = frames(&[b"event: noti", b"fication\nda", b"ta: {\"a\":1}\n", b"\n"]);
        assert_eq!(got, vec![msg("notification", "{\"a\":1}")]);
    }

    #[test]
    fn the_parser_reads_crlf_and_a_crlf_split_between_chunks() {
        let got = frames(&[b"event: e\r\ndata: x\r", b"\n\r\ndata: y\r\n\r\n"]);
        assert_eq!(got, vec![msg("e", "x"), msg("message", "y")]);
    }

    #[test]
    fn a_comment_line_is_a_ping_frame_and_does_not_disturb_a_frame_in_progress() {
        let got = frames(&[b"data: a\n: ping\ndata: b\n\n: ping\n"]);
        assert_eq!(got, vec![SseFrame::Comment, msg("message", "a\nb"), SseFrame::Comment]);
    }

    #[test]
    fn multiple_data_lines_join_with_a_newline_and_one_leading_space_is_stripped() {
        let got = frames(&[b"data:  one\ndata:two\ndata\n\n"]);
        assert_eq!(got, vec![msg("message", " one\ntwo\n")]);
    }

    #[test]
    fn a_blank_line_with_nothing_pending_dispatches_nothing_and_unknown_fields_are_ignored() {
        let got = frames(&[b"\n\nid: 7\nretry: 100\n\n"]);
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn an_overlong_data_line_inside_a_frame_drops_the_whole_frame() {
        let mut big = b"data: first\ndata: ".to_vec();
        big.extend(std::iter::repeat(b'x').take(MAX_LINE + 10));
        big.extend_from_slice(b"\ndata: last\n\ndata: ok\n\n");
        let got = frames(&[&big]);
        assert_eq!(got, vec![msg("message", "ok")], "a frame with its middle missing must not dispatch");
    }

    #[test]
    fn an_overlong_data_line_alone_dispatches_nothing() {
        let mut big = b"data: ".to_vec();
        big.extend(std::iter::repeat(b'x').take(MAX_LINE + 10));
        big.extend_from_slice(b"\n\n");
        assert!(frames(&[&big]).is_empty());
    }

    #[test]
    fn an_overlong_line_is_dropped_whole_and_the_stream_resumes() {
        let mut big = b"data: ".to_vec();
        big.extend(std::iter::repeat(b'x').take(MAX_LINE + 10));
        big.extend_from_slice(b"\n\ndata: ok\n\n");
        let got = frames(&[&big]);
        assert_eq!(got, vec![msg("message", "ok")]);
    }

    const PLAYING: &str = r#"{"NotificationContainer":{"type":"playing","size":2,"PlaySessionStateNotification":[
        {"sessionKey":"12","clientIdentifier":"other-device","ratingKey":"345","key":"/library/metadata/345","viewOffset":61000,"state":"playing","extra":{"a":1}},
        {"sessionKey":"13","clientIdentifier":"cid-own","ratingKey":"346","key":"/library/metadata/346","viewOffset":5,"state":"paused"}]}}"#;

    #[test]
    fn a_playing_frame_becomes_sessions_with_our_own_dropped() {
        let n = classify(&msg("notification", PLAYING), "cid-own");
        let Notification::Play(sessions) = n else { panic!("{n:?}") };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_key.as_deref(), Some("12"));
        assert_eq!(sessions[0].client_identifier.as_deref(), Some("other-device"));
        assert_eq!(sessions[0].rating_key.as_deref(), Some("345"));
        assert_eq!(sessions[0].key.as_deref(), Some("/library/metadata/345"));
        assert_eq!(sessions[0].view_offset_ms, Some(61000));
        assert_eq!(sessions[0].state, PlayState::Playing);
    }

    #[test]
    fn a_frame_of_only_our_own_sessions_is_ignored() {
        let only_own = PLAYING.replace("other-device", "cid-own");
        assert_eq!(classify(&msg("notification", &only_own), "cid-own"), Notification::Ignored);
    }

    #[test]
    fn missing_fields_extra_fields_and_numeric_strings_are_tolerated() {
        let data = r#"{"NotificationContainer":{"type":"playing","PlaySessionStateNotification":[
            {"ratingKey":346,"viewOffset":"1500","unknown":[1,2]}, {}]}}"#;
        let Notification::Play(s) = classify(&msg("notification", data), "cid-own") else { panic!() };
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].rating_key.as_deref(), Some("346"));
        assert_eq!(s[0].view_offset_ms, Some(1500));
        assert_eq!(s[0].state, PlayState::Other);
        assert_eq!(s[1].session_key, None);
        assert_eq!(s[1].view_offset_ms, None);
    }

    #[test]
    fn the_stopped_state_is_mapped_and_unknown_states_are_other() {
        let mk = |state: &str| {
            let data = format!(
                r#"{{"NotificationContainer":{{"type":"playing","PlaySessionStateNotification":[{{"clientIdentifier":"x","state":"{state}"}}]}}}}"#
            );
            let Notification::Play(s) = classify(&msg("notification", &data), "own") else { panic!() };
            s[0].state
        };
        assert_eq!(mk("stopped"), PlayState::Stopped);
        assert_eq!(mk("buffering"), PlayState::Buffering);
        assert_eq!(mk("paused"), PlayState::Paused);
        assert_eq!(mk("playing"), PlayState::Playing);
        assert_eq!(mk("somethingnew"), PlayState::Other);
    }

    #[test]
    fn unknown_types_and_malformed_data_are_ignored_and_pings_are_pings() {
        let activity = r#"{"NotificationContainer":{"type":"activity","ActivityNotification":[{"event":"started"}]}}"#;
        assert_eq!(classify(&msg("notification", activity), "own"), Notification::Ignored);
        assert_eq!(classify(&msg("notification", "not json"), "own"), Notification::Ignored);
        assert_eq!(classify(&msg("notification", "{}"), "own"), Notification::Ignored);
        assert_eq!(classify(&SseFrame::Comment, "own"), Notification::Ping);
        assert_eq!(classify(&msg("ping", ""), "own"), Notification::Ping);
    }

    fn timeline(data: &str) -> Notification {
        classify(&msg("timeline", data), "own")
    }

    fn library_of(data: &str) -> Vec<LibraryEntry> {
        match timeline(data) {
            Notification::Library(entries) => entries,
            other => panic!("{other:?}"),
        }
    }

    fn entry(id: &str, kind: LibraryKind, state: LibraryState, section: Option<&str>) -> LibraryEntry {
        LibraryEntry { item_id: id.into(), kind, state, section: section.map(str::to_owned) }
    }

    /// The entries a whole sequence of captured frames yields, in order.
    fn kept(frames: &[&str]) -> Vec<LibraryEntry> {
        frames
            .iter()
            .filter_map(|f| match timeline(f) {
                Notification::Library(e) => Some(e),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn a_captured_movie_add_yields_one_created_entry_on_its_finished_frame() {
        // 5 + processing and 4 + loading are scan progress; only the bare 5 is the finished create.
        assert_eq!(timeline(fixtures::MOVIE_ADD[0]), Notification::Ignored);
        assert_eq!(timeline(fixtures::MOVIE_ADD[1]), Notification::Ignored);
        assert_eq!(
            library_of(fixtures::MOVIE_ADD[2]),
            [entry("1001", LibraryKind::Movie, LibraryState::Created, Some("1"))]
        );
        assert_eq!(kept(&fixtures::MOVIE_ADD).len(), 1);
    }

    #[test]
    fn a_captured_show_add_yields_one_created_entry() {
        assert_eq!(
            kept(&fixtures::SHOW_ADD),
            [entry("2001", LibraryKind::Show, LibraryState::Created, Some("2"))]
        );
    }

    #[test]
    fn seasons_episodes_and_music_tracks_are_dropped_whole() {
        assert_eq!(kept(&fixtures::SEASON_AND_EPISODES), [], "type 3 and 4 never reach the Watchlist");
        assert_eq!(kept(&fixtures::MUSIC_TRACK), [], "type 10, analyzing or not");
        assert_eq!(timeline(fixtures::OTHER_PLUGIN), Notification::Ignored);
    }

    #[test]
    fn a_metadata_state_marks_a_frame_unfinished_whatever_its_state() {
        let frame = |state: &str, extra: &str| {
            format!(
                r#"{{"TimelineEntry":{{"identifier":"com.plexapp.plugins.library","sectionID":"1","itemID":"9","type":1,"state":{state}{extra}}}}}"#
            )
        };
        assert!(matches!(timeline(&frame("5", "")), Notification::Library(_)));
        for extra in [r#","metadataState":"processing""#, r#","metadataState":"queued""#, r#","mediaState":"analyzing""#] {
            assert_eq!(timeline(&frame("5", extra)), Notification::Ignored, "{extra}");
        }
        for state in ["0", "1", "2", "3", "4", "6"] {
            assert_eq!(timeline(&frame(state, "")), Notification::Ignored, "state {state}");
        }
    }

    #[test]
    fn a_delete_is_read_at_the_unverified_state_9() {
        // No delete was ever captured; this pins the community-documented value, not a recording.
        assert_eq!(
            library_of(fixtures::MOVIE_DELETED),
            [entry("1002", LibraryKind::Movie, LibraryState::Deleted, Some("1"))]
        );
    }

    #[test]
    fn the_community_envelope_with_string_numbers_and_an_array_is_still_read() {
        let n = classify(&msg("notification", fixtures::COMMUNITY_ENVELOPE), "own");
        assert_eq!(
            n,
            Notification::Library(vec![
                entry("1003", LibraryKind::Movie, LibraryState::Created, Some("1")),
                entry("2003", LibraryKind::Show, LibraryState::Created, Some("2")),
            ])
        );
    }

    #[test]
    fn malformed_timeline_entries_are_dropped_without_a_panic() {
        for data in fixtures::MALFORMED {
            assert_eq!(timeline(data), Notification::Ignored, "{data}");
        }
        for data in [
            r#"{"NotificationContainer":{"type":"timeline"}}"#,
            r#"{"NotificationContainer":{"type":"timeline","TimelineEntry":{"itemID":"1"}}}"#,
            r#"{"TimelineEntry":{"itemID":"1"}}"#,
            "{}",
            "[]",
        ] {
            assert_eq!(timeline(data), Notification::Ignored, "{data}");
        }
    }

    #[test]
    fn a_timeline_frame_is_not_filtered_by_our_own_client_id() {
        // Library changes carry no session: the own-session filter has nothing to match.
        let n = classify(&msg("timeline", fixtures::MOVIE_ADD[2]), "cid-own");
        assert!(matches!(n, Notification::Library(e) if e.len() == 1));
    }

    #[test]
    fn identity_headers_carry_the_persisted_client_id_and_no_provides() {
        let c = client(Origin::http("192.0.2.1", 32400), "tok");
        let headers = c.notify_identity_headers();
        assert!(headers.iter().any(|h| h == "X-Plex-Client-Identifier: cid-own"), "{headers:?}");
        for name in ["Product", "Version", "Platform", "Platform-Version", "Device", "Device-Name"] {
            assert!(headers.iter().any(|h| h.starts_with(&format!("X-Plex-{name}: "))), "{name}: {headers:?}");
        }
        assert!(
            !headers.iter().any(|h| h.to_ascii_lowercase().contains("provides")),
            "the feed must not announce a controllable player: {headers:?}"
        );
        assert!(!headers.iter().any(|h| h.to_ascii_lowercase().contains("token")), "{headers:?}");
    }

    /// A listener that records whether anything connected: `notifications` must not even dial.
    fn watched() -> (TcpListener, Origin) {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        l.set_nonblocking(true).unwrap();
        let port = l.local_addr().unwrap().port();
        (l, Origin::http("127.0.0.1", i32::from(port)))
    }

    fn run(c: &Client) -> (NotifyEnd, Vec<Notification>) {
        let cancel = AtomicBool::new(false);
        let mut got = Vec::new();
        let end = c.notifications(&plx_base::task::OffFrame::for_test(), &cancel, &mut |n| got.push(n));
        (end, got)
    }

    #[test]
    fn an_empty_token_is_refused_and_no_request_is_made() {
        let _g = plx_base::testlock::serial();
        let (listener, origin) = watched();
        let c = client(origin, "");
        let (end, got) = run(&c);
        assert_eq!(end, NotifyEnd::Refused { status: 0 });
        assert!(got.is_empty());
        assert!(listener.accept().is_err(), "an empty token must never reach a socket");
    }

    #[test]
    fn disabled_data_io_is_refused_without_a_request_or_a_count() {
        let _g = plx_base::testlock::serial();
        let (listener, origin) = watched();
        let c = client(origin, "tok");
        assert!(!c.data_io_disabled());
        c.disable_data_io();
        assert!(c.data_io_disabled());
        let (end, _) = run(&c);
        assert_eq!(end, NotifyEnd::Refused { status: 0 });
        assert!(listener.accept().is_err());
        assert_eq!(c.denied_data_requests(), 0, "the pure read counts nothing");
        assert!(c.data_io_disabled(), "…and asking twice changes nothing");
        assert_eq!(c.denied_data_requests(), 0);
    }

    #[test]
    fn a_stream_is_delivered_in_order_with_our_headers_and_ends_closed() {
        let _g = plx_base::testlock::serial();
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().expect("accept");
            let mut req = Vec::new();
            let mut b = [0u8; 1024];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut b).expect("read");
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&b[..n]);
            }
            s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
            let frame = format!("event: notification\ndata: {}\n\n", PLAYING.replace('\n', " "));
            for chunk in [": ping\n\n", frame.as_str()] {
                s.write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes()).unwrap();
            }
            s.write_all(b"0\r\n\r\n").unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        let c = client(Origin::http("127.0.0.1", i32::from(port)), "secret-token");
        let (end, got) = run(&c);
        let request = server.join().unwrap();
        assert_eq!(end, NotifyEnd::Closed);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0], Notification::Ping);
        assert!(matches!(&got[1], Notification::Play(s) if s.len() == 1));
        assert!(request.starts_with("GET /:/eventsource/notifications?filters=playing%2Ctimeline&X-Plex-Token="), "{request}");
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains("accept: text/event-stream"), "{request}");
        assert!(lower.contains("x-plex-client-identifier: cid-own"), "{request}");
        assert!(!lower.contains("provides"), "{request}");
    }

    #[test]
    fn a_non_2xx_answer_is_refused_with_its_status() {
        let _g = plx_base::testlock::serial();
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = l.accept().expect("accept");
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            s.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
        });
        let c = client(Origin::http("127.0.0.1", i32::from(port)), "tok");
        let (end, got) = run(&c);
        let _ = server.join();
        assert!(got.is_empty());
        assert_eq!(end, NotifyEnd::Refused { status: 401 });
    }

    #[test]
    fn stream_ends_map_one_to_one() {
        use plx_net::net::StreamEnd as S;
        assert_eq!(NotifyEnd::of(S::Closed), NotifyEnd::Closed);
        assert_eq!(NotifyEnd::of(S::Idle), NotifyEnd::Silent);
        assert_eq!(NotifyEnd::of(S::Cancelled), NotifyEnd::Cancelled);
        assert_eq!(NotifyEnd::of(S::Consumer), NotifyEnd::Cancelled);
        assert_eq!(NotifyEnd::of(S::Status(403)), NotifyEnd::Refused { status: 403 });
        assert_eq!(NotifyEnd::of(S::Unsupported), NotifyEnd::Unsupported);
        assert_eq!(NotifyEnd::of(S::Refused), NotifyEnd::Refused { status: 0 });
    }
}
