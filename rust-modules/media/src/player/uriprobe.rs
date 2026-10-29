//! The URI-mode probe machine (issue #1064, the first test): what to say to `com.webos.media`, in
//! what order, and what each answer means, as a tested `State + Event -> Effects` machine in the
//! shape of [`super::lifecycle::ForegroundLifecycle`].
//!
//! webOS 5+ plays a URL inside its own media service when asked over the platform bus (`load` with
//! a `uri`, `subscribe`, `play`, `unload`); the probe asks whether the picture then shows in the
//! window this process exports, and logs what the service says. The machine reads no clock and no
//! file and performs no IO. Time arrives as `now_ms` on [`UriProbe::step`], the trigger file's
//! content as a [`Variant`], the URL's scheme as a [`Scheme`]; the work it asks for leaves as a
//! [`UriEffect`] the driver (`uriprobe_drive`) executes, and every answer returns as the next
//! [`UriEvent`].
//!
//! **The URL never enters the machine.** [`UriEffect::Call`] names a method and the ids a payload
//! needs; the driver, which holds the URL, builds the `load` payload with [`build_load`] and hands
//! it to the transport. Every line the machine writes is built from whitelisted scalars
//! ([`parse_event`]) and [`scrub_text`]ed hub text, never from a raw payload: a service event can
//! echo the URI (`sourceInfo`, `streamingInfo`, an error), and the log is posted by a tester.
//!
//! **Phases.** `Registering` (ladder: the named client, then the anonymous one, once), `Loading`
//! (`load` sent; the reply carries `mediaId`, which is subscribed; waits for `loadCompleted`),
//! `SettingWindow` ([`Variant::Late`] only: `setWindowId` after the load, its reply logged),
//! `Starting` (`play` sent; waits for `playing`), `Playing` (watches `currentTime`), then `Done`.
//! Back and every failure go to `Done` through one `stop`: `unload` once a `mediaId` is known,
//! `close` always, the route-start attempt settled exactly once, the summary written once, and
//! nothing after that.
//!
//! The probe cannot see the video plane. The summary reports what the service said; the tester's
//! eyes are the verdict, and no `video=shown` token is guessed.
use plx_platform::tv::uri::{Shape, UriReply};
use serde_json::{json, Map, Value};

/// How long each wait lasts, in milliseconds. A wait that runs out ends the session with
/// `timeout:<phase>`, except the window call, whose silence is logged and played past.
const REGISTER_TIMEOUT_MS: u64 = 10_000;
const LOAD_TIMEOUT_MS: u64 = 20_000;
const LATE_ID_TIMEOUT_MS: u64 = 5_000;
const PLAY_TIMEOUT_MS: u64 = 20_000;
/// `currentTime` must change at least this often once it has been reported at all.
const FROZEN_TIME_MS: u64 = 10_000;
/// `currentTime` is logged when it first moves and then at most this often.
const TIME_LOG_EVERY_MS: u64 = 1_000;
/// The longest an error text is kept after [`scrub_text`], in characters.
const SCRUBBED_TEXT_CHARS: usize = 120;

// ---------------------------------------------------------------------------------------------
// What the tester chooses and what the route says

/// Where the window id goes, chosen by the content of the trigger file (empty or `load`, `late`,
/// `none`). One build answers all three, so the tester needs fewer round trips.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `windowId` in the `load` payload.
    AtLoad,
    /// `load` without it, then `setWindowId` after `loadCompleted`, then `play`.
    Late,
    /// No window id at all.
    NoWindow,
}

impl Variant {
    /// The variant a trigger file's text names; `None` for text that names none, so the driver
    /// can say so instead of quietly running another variant.
    pub fn from_trigger(content: &str) -> Option<Variant> {
        match content.trim().to_ascii_lowercase().as_str() {
            "" | "load" => Some(Variant::AtLoad),
            "late" => Some(Variant::Late),
            "none" => Some(Variant::NoWindow),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Variant::AtLoad => "at-load",
            Variant::Late => "late",
            Variant::NoWindow => "none",
        }
    }
}

/// The URL's scheme. A refused HTTPS load may be a certificate problem in the service's pipeline
/// and not the window; the `armed` line carries this so the log can tell them apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    Http,
    Https,
}

impl Scheme {
    /// The scheme of `url`. Reads no more than the scheme and keeps none of the rest.
    pub fn of(url: &str) -> Scheme {
        if url.get(..8).is_some_and(|head| head.eq_ignore_ascii_case("https://")) {
            Scheme::Https
        } else {
            Scheme::Http
        }
    }

    fn label(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }
}

/// What kind of play the route chose (logged, nothing of the URL).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Direct,
    Hls,
}

/// Everything the machine is armed with. A value, not state: the driver builds it once.
#[derive(Clone, Debug)]
pub struct Config {
    pub variant: Variant,
    pub scheme: Scheme,
    pub kind: Kind,
    /// The exported window's id, when one was created (`_Window_Id_<n>`; not a secret). `None` on a
    /// television that exports none, and ignored by [`Variant::NoWindow`].
    pub window_id: Option<String>,
    /// The app id the `load` payload names (not a secret).
    pub app_id: String,
}

// ---------------------------------------------------------------------------------------------
// Payloads. The only place a URL is spelled into text.

/// The `load` payload: `{"uri":U,"type":"media","payload":{"option":{"appId":A[,"windowId":W]}}}`.
/// Built with the JSON writer, never by formatting the URL into a string.
pub fn build_load(url: &str, app_id: &str, window_id: Option<&str>) -> String {
    let mut option = Map::new();
    option.insert("appId".to_owned(), json!(app_id));
    if let Some(window) = window_id {
        option.insert("windowId".to_owned(), json!(window));
    }
    json!({ "uri": url, "type": "media", "payload": { "option": Value::Object(option) } }).to_string()
}

/// A call the machine asks the driver to send. Everything a payload needs except the URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UriCall {
    /// `window_id` is the id to put in the payload (`Variant::AtLoad` with a window), else `None`.
    Load { window_id: Option<String> },
    Subscribe { media_id: String },
    Play { media_id: String },
    SetWindowId { media_id: String, window_id: String },
    Unload { media_id: String },
}

impl UriCall {
    /// The method name under `luna://com.webos.media/`.
    pub fn method(&self) -> &'static str {
        match self {
            UriCall::Load { .. } => "load",
            UriCall::Subscribe { .. } => "subscribe",
            UriCall::Play { .. } => "play",
            UriCall::SetWindowId { .. } => "setWindowId",
            UriCall::Unload { .. } => "unload",
        }
    }

    /// The payload, for every call but `load` (which needs the URL: [`build_load`]).
    pub fn payload(&self) -> Option<String> {
        Some(match self {
            UriCall::Load { .. } => return None,
            UriCall::Subscribe { media_id } => json!({ "mediaId": media_id, "subscribe": true }),
            UriCall::Play { media_id } | UriCall::Unload { media_id } => json!({ "mediaId": media_id }),
            UriCall::SetWindowId { media_id, window_id } => json!({ "mediaId": media_id, "windowId": window_id }),
        }
        .to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// Events in: the whitelist

/// A service event the probe knows by name. Anything else is logged as `ev=<name> (unparsed)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ev {
    LoadCompleted,
    Playing,
    Paused,
    EndOfStream,
    CurrentTime,
    BufferRange,
    BufferingStart,
    BufferingEnd,
    SeekDone,
    TrackSelected,
    SourceInfo,
    StreamingInfo,
    VideoInfo,
    AudioInfo,
    UnloadCompleted,
    Error,
}

/// The key each event arrives under, and the text it logs as.
const EVENT_KEYS: [(&str, Ev); 16] = [
    ("loadCompleted", Ev::LoadCompleted),
    ("playing", Ev::Playing),
    ("paused", Ev::Paused),
    ("endOfStream", Ev::EndOfStream),
    ("currentTime", Ev::CurrentTime),
    ("bufferRange", Ev::BufferRange),
    ("bufferingStart", Ev::BufferingStart),
    ("bufferingEnd", Ev::BufferingEnd),
    ("seekDone", Ev::SeekDone),
    ("trackSelected", Ev::TrackSelected),
    ("sourceInfo", Ev::SourceInfo),
    ("streamingInfo", Ev::StreamingInfo),
    ("videoInfo", Ev::VideoInfo),
    ("audioInfo", Ev::AudioInfo),
    ("unloadCompleted", Ev::UnloadCompleted),
    ("error", Ev::Error),
];

impl Ev {
    fn name(self) -> &'static str {
        EVENT_KEYS.iter().find(|(_, ev)| *ev == self).map_or("?", |(key, _)| key)
    }

    /// The event a bare word names: the state words some builds send as a value
    /// (`"state":"playing"`), and the old client's `"loaded"`.
    fn of_word(word: &str) -> Option<Ev> {
        if word == "loaded" {
            return Some(Ev::LoadCompleted);
        }
        EVENT_KEYS.iter().find(|(key, _)| *key == word).map(|(_, ev)| *ev)
    }
}

/// One whitelisted event of a message with the whitelisted scalars it carried, already formatted
/// (`width=1920 height=1080 codec=H264`). Holds no raw text.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    ev: Ev,
    detail: String,
}

/// What a service message said, reduced to the scalars the probe may keep. Every string in it is a
/// token of `[A-Za-z0-9_-]` or went through [`scrub_text`]; there is no field for a payload, so
/// `Debug` on it cannot print a URL.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Message {
    return_value: Option<bool>,
    media_id: Option<String>,
    seen: Vec<Seen>,
    /// An identifier-shaped top-level key of a message that named no event the probe knows.
    unknown: Option<String>,
    error_code: Option<i64>,
    error_text: Option<String>,
    current_time: Option<i64>,
    dims: Option<(u32, u32)>,
    dropped: Option<u64>,
}

/// A name the log may print: ASCII letters, digits, `_` and `-` only (no `.`, `:` or `/`), and
/// short. Anything else (a URL, a path, a host) is refused outright, never filtered into a leaky
/// remnant.
fn token(text: &str) -> Option<String> {
    let clean = !text.is_empty() && text.len() <= 40 && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    clean.then(|| text.to_owned())
}

/// A scalar of a service message as log text, whitelisted by type: numbers, booleans, tokens, and
/// a `{num, den}` pair as `num/den`.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::String(s) => token(s),
        Value::Object(o) => {
            let part = |key| o.get(key).and_then(Value::as_i64);
            Some(format!("{}/{}", part("num")?, part("den")?))
        }
        _ => None,
    }
}

/// `key=value` pairs for the listed keys of `value` (an object), in the listed order.
fn details(value: &Value, keys: &[&str]) -> String {
    let Some(object) = value.as_object() else { return String::new() };
    let parts: Vec<String> = keys
        .iter()
        .filter_map(|key| object.get(*key).and_then(scalar).map(|shown| format!("{key}={shown}")))
        .collect();
    parts.join(" ")
}

/// The keys of a message that are not events: how a reply is told from an unknown event.
const PLAIN_KEYS: [&str; 6] = ["returnValue", "mediaId", "subscribed", "subscribe", "errorCode", "errorText"];

/// Reduce a service payload to what the probe may keep. A whitelist: a key it does not know
/// contributes at most its own identifier-shaped NAME; a value it does not know is never read.
pub fn parse_event(payload: &str) -> Message {
    let mut message = Message::default();
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(payload) else { return message };
    message.return_value = object.get("returnValue").and_then(Value::as_bool);
    message.media_id = object.get("mediaId").and_then(Value::as_str).and_then(token);
    // The error fields sit at the top level or inside an `error` object.
    let error_object = object.get("error").and_then(Value::as_object);
    let field = |key: &str| object.get(key).or_else(|| error_object.and_then(|inner| inner.get(key)));
    message.error_code = field("errorCode").and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    message.error_text = field("errorText")
        .and_then(Value::as_str)
        .or_else(|| object.get("error").and_then(Value::as_str))
        .map(scrub_text);
    message.current_time = object.get("currentTime").and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)));
    for (key, ev) in EVENT_KEYS {
        let Some(value) = object.get(key) else { continue };
        if matches!(value, Value::Null | Value::Bool(false)) {
            continue;
        }
        let detail = match ev {
            Ev::VideoInfo => details(value, &["width", "height", "codec", "frameRate", "hdr"]),
            Ev::AudioInfo => details(value, &["codec", "channels", "sampleRate"]),
            Ev::BufferRange => details(value, &["beginTime", "endTime"]),
            Ev::CurrentTime => scalar(value).map(|v| format!("value={v}")).unwrap_or_default(),
            _ => String::new(),
        };
        if ev == Ev::VideoInfo {
            let dim = |key| value.get(key).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
            message.dims = dim("width").zip(dim("height"));
        }
        message.seen.push(Seen { ev, detail });
    }
    for value in object.values() {
        if let Value::String(word) = value {
            if let Some(ev) = Ev::of_word(word) {
                if message.seen.iter().all(|seen| seen.ev != ev) {
                    message.seen.push(Seen { ev, detail: String::new() });
                }
            }
        }
    }
    // An error event: an `error` key (above), or error fields on a message that is not a refused call.
    if message.error_code.is_some_and(|code| code != 0) && message.return_value != Some(false) && message.seen.iter().all(|seen| seen.ev != Ev::Error) {
        message.seen.push(Seen { ev: Ev::Error, detail: String::new() });
    }
    if message.seen.is_empty() {
        message.unknown = object
            .keys()
            .filter(|key| !PLAIN_KEYS.contains(&key.as_str()))
            .find_map(|key| (key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && key.len() <= 32).then(|| key.clone()));
    }
    message
}

/// Hub or service text made safe for a log line a tester will post: every whitespace-delimited word
/// that could hold a URL, a path, an address, a host or a token is replaced by `<url>`, and the
/// result is cut to [`SCRUBBED_TEXT_CHARS`]. A word is replaced when it contains `/` (a path, or
/// `://`), `@`, a backslash, `token` (any case), `plex.direct`, a `host:port` colon followed by
/// digits, or an interior `.`
/// (a host name, a dotted quad, a file name) that is not part of one of this television's own
/// service names (`com.webos.*`, `com.palm.*`, `com.lge.*`, `com.beb.*`), which the hub names in
/// its refusals and which tell a tester why a call was denied.
pub fn scrub_text(text: &str) -> String {
    let words: Vec<String> = text.split_whitespace().map(scrub_word).collect();
    words.join(" ").chars().take(SCRUBBED_TEXT_CHARS).collect()
}

fn scrub_word(word: &str) -> String {
    let edge = |c: char| matches!(c, '.' | ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '<' | '>' | '!' | '?');
    let bare = word.trim_matches(edge);
    let lower = bare.to_ascii_lowercase();
    let service = ["com.webos.", "com.palm.", "com.lge.", "com.beb."].iter().any(|prefix| lower.starts_with(prefix))
        && bare.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let host_port = bare.rsplit_once(':').is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
    let tainted = word.contains(['/', '@', '\\'])
        || lower.contains("token")
        || lower.contains("plex.direct")
        || host_port
        || (bare.contains('.') && !service);
    if !tainted {
        return word.to_owned();
    }
    // Only the core is replaced: the brackets and stops around it stay, so a sentence still reads.
    let start = word.find(bare).unwrap_or(0);
    format!("{}<url>{}", &word[..start], &word[start + bare.len()..])
}

// ---------------------------------------------------------------------------------------------
// Events in

/// Which call a reply answers (the transport tags every message with the method that was sent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Register,
    Load,
    Subscribe,
    Play,
    SetWindowId,
    Unload,
    Dropped,
    Other,
}

impl Method {
    fn of(name: &str) -> Method {
        match name {
            "register" => Method::Register,
            "load" => Method::Load,
            "subscribe" => Method::Subscribe,
            "play" => Method::Play,
            "setWindowId" => Method::SetWindowId,
            "unload" => Method::Unload,
            "dropped" => Method::Dropped,
            _ => Method::Other,
        }
    }
}

/// What the driver feeds [`UriProbe::step`].
#[derive(Clone, Debug, PartialEq)]
pub enum UriEvent {
    /// The probe is armed: begin registering.
    Start,
    /// Back (or any stop the driver wants): end the session now.
    Back,
    /// Time has passed: the deadlines are checked against `now_ms`.
    Tick,
    /// A message from the transport, or the transport's refusal to send one.
    Reply { method: Method, ok: bool, message: Message },
}

impl UriEvent {
    /// The event for a transport message. The payload is parsed here and dropped: only the
    /// whitelisted [`Message`] goes on. A transport failure's text is the hub's, scrubbed.
    pub fn from_reply(reply: &UriReply) -> UriEvent {
        let method = Method::of(reply.method);
        let message = if !reply.ok {
            Message { error_text: Some(scrub_text(&reply.payload)), ..Message::default() }
        } else if method == Method::Dropped {
            Message { dropped: reply.payload.trim().parse().ok(), ..Message::default() }
        } else if method == Method::Register {
            Message::default()
        } else {
            parse_event(&reply.payload)
        };
        UriEvent::Reply { method, ok: reply.ok, message }
    }

    /// The event for a transport call that returned `Err` at once (`open` or `call` refused).
    pub fn from_failure(method: &'static str, text: &str) -> UriEvent {
        UriEvent::Reply {
            method: Method::of(method),
            ok: false,
            message: Message { error_text: Some(scrub_text(text)), ..Message::default() },
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Effects out

/// How the route-start attempt the probe stands in for ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    Started,
    StartFailed,
}

/// What the machine asks the driver to do, in order. Holds ids and log text, never the URL or a
/// payload, so a derived `Debug` is safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UriEffect {
    /// Register on the bus as `Shape`; the outcome returns as the `register` reply.
    Open(Shape),
    /// Send this call; the transport tags its answers with the method.
    Call(UriCall),
    /// Unregister (idempotent). The service unloads a pipeline whose load client leaves the bus.
    Close,
    /// Settle the attempt the probe stands in for (`route::settle_route_start`), exactly once.
    Settle(Settle),
    /// Re-place the exported window for the coded size the service reported.
    PlaceWindow { width: u32, height: u32 },
    /// One event-log line, complete (`uriprobe: t=… …`).
    Log(String),
}

// ---------------------------------------------------------------------------------------------
// The machine

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Registering { deadline: u64 },
    Loading { deadline: u64 },
    SettingWindow { deadline: u64 },
    Starting { deadline: u64 },
    Playing { frozen_at: u64 },
    Done,
}

impl Phase {
    /// The name a failure in this phase is reported under.
    fn name(self) -> &'static str {
        match self {
            Phase::Idle | Phase::Registering { .. } => "register",
            Phase::Loading { .. } => "load",
            Phase::SettingWindow { .. } => "setWindowId",
            Phase::Starting { .. } => "play",
            Phase::Playing { .. } | Phase::Done => "currentTime",
        }
    }
}

/// How a session ended: the `end=` of the summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum End {
    Back,
    Eos,
    Error(Option<i64>),
    Timeout(&'static str),
    Refused(&'static str),
}

/// What became of `setWindowId` (the `setWindowId=` of the summary).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WindowCall {
    NotSent,
    Pending,
    Answered,
    Refused,
    Silent,
}

/// What the summary reports: milliseconds since the start at which each thing was first seen.
#[derive(Debug)]
struct Facts {
    window_call: WindowCall,
    loaded_ms: Option<u64>,
    playing_ms: Option<u64>,
    time_moved: Option<u64>,
    video_seen: Option<u64>,
    audio_seen: Option<u64>,
    failed_in: Option<&'static str>,
    ended: Option<End>,
}

impl Default for Facts {
    fn default() -> Self {
        Facts {
            window_call: WindowCall::NotSent,
            loaded_ms: None,
            playing_ms: None,
            time_moved: None,
            video_seen: None,
            audio_seen: None,
            failed_in: None,
            ended: None,
        }
    }
}

/// The probe. State is private; the one writer is [`step`](UriProbe::step).
pub struct UriProbe {
    config: Config,
    phase: Phase,
    shape: Shape,
    started_ms: u64,
    media_id: Option<String>,
    subscribed: bool,
    settled: Option<Settle>,
    last_time: Option<i64>,
    time_logged_ms: Option<u64>,
    placed: Option<(u32, u32)>,
    facts: Facts,
}

impl std::fmt::Debug for UriProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UriProbe {{ phase: {:?}, shape: {:?} }}", self.phase, self.shape)
    }
}

impl UriProbe {
    pub fn new(mut config: Config) -> UriProbe {
        if config.variant == Variant::NoWindow {
            config.window_id = None;
        }
        UriProbe {
            config,
            phase: Phase::Idle,
            shape: Shape::Named,
            started_ms: 0,
            media_id: None,
            subscribed: false,
            settled: None,
            last_time: None,
            time_logged_ms: None,
            placed: None,
            facts: Facts::default(),
        }
    }

    /// The session is over (everything it will say has been said).
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// From `playing` until the end: the service has said the video plays. A claim about the
    /// service, not an observation of the plane.
    pub fn plane_open(&self) -> bool {
        matches!(self.phase, Phase::Playing { .. })
    }

    /// Apply one event at `now_ms`, pushing what the driver must execute onto `fx`, in order. Once
    /// the session is `Done` nothing moves and nothing is pushed.
    pub fn step(&mut self, ev: UriEvent, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if self.phase == Phase::Done {
            return;
        }
        match ev {
            UriEvent::Start => self.start(now_ms, fx),
            UriEvent::Back => self.stop(now_ms, End::Back, fx),
            UriEvent::Tick => self.tick(now_ms, fx),
            UriEvent::Reply { method, ok, message } => self.reply(method, ok, &message, now_ms, fx),
        }
    }

    fn line(&self, now_ms: u64, body: &str) -> UriEffect {
        UriEffect::Log(format!("uriprobe: t={} {body}", now_ms.saturating_sub(self.started_ms)))
    }

    fn event_line(&self, now_ms: u64, name: &str, detail: &str) -> UriEffect {
        self.line(now_ms, format!("ev={name} {detail}").trim_end())
    }

    fn start(&mut self, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if self.phase != Phase::Idle {
            return;
        }
        self.started_ms = now_ms;
        let c = &self.config;
        let window = if c.window_id.is_some() { "exported" } else { "none" };
        let kind = match c.kind {
            Kind::Direct => "direct",
            Kind::Hls => "hls",
        };
        let armed = format!("armed variant={} scheme={} kind={kind} window={window} app={}", c.variant.label(), c.scheme.label(), c.app_id);
        fx.push(self.line(now_ms, &armed));
        self.phase = Phase::Registering { deadline: now_ms + REGISTER_TIMEOUT_MS };
        fx.push(UriEffect::Open(self.shape));
    }

    fn shape_label(shape: Shape) -> &'static str {
        match shape {
            Shape::Named => "named",
            Shape::Anon => "anon",
        }
    }

    fn tick(&mut self, now_ms: u64, fx: &mut Vec<UriEffect>) {
        match self.phase {
            Phase::Registering { deadline } if now_ms >= deadline => self.stop(now_ms, End::Timeout("register"), fx),
            Phase::Loading { deadline } if now_ms >= deadline => self.stop(now_ms, End::Timeout("load"), fx),
            Phase::SettingWindow { deadline } if now_ms >= deadline => {
                self.facts.window_call = WindowCall::Silent;
                fx.push(self.line(now_ms, "call setWindowId no reply, playing anyway"));
                self.play(now_ms, fx);
            }
            Phase::Starting { deadline } if now_ms >= deadline => self.stop(now_ms, End::Timeout("play"), fx),
            Phase::Playing { frozen_at } if self.facts.time_moved.is_some() && now_ms >= frozen_at => {
                self.stop(now_ms, End::Timeout("currentTime"), fx)
            }
            _ => {}
        }
    }

    fn reply(&mut self, method: Method, ok: bool, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if method == Method::Dropped {
            let count = message.dropped.unwrap_or(0);
            fx.push(self.line(now_ms, &format!("transport dropped {count} replies")));
            return;
        }
        match self.phase {
            Phase::Registering { .. } if method == Method::Register => {
                let shape = Self::shape_label(self.shape);
                if ok {
                    fx.push(self.line(now_ms, &format!("register shape={shape} ok")));
                    self.phase = Phase::Loading { deadline: now_ms + LOAD_TIMEOUT_MS };
                    let window_id = if self.config.variant == Variant::AtLoad { self.config.window_id.clone() } else { None };
                    fx.push(UriEffect::Call(UriCall::Load { window_id }));
                } else {
                    fx.push(self.line(now_ms, &format!("register shape={shape} refused({})", message.why())));
                    self.next_shape_or_stop(now_ms, End::Refused("register"), fx);
                }
            }
            Phase::Loading { .. } | Phase::SettingWindow { .. } | Phase::Starting { .. } | Phase::Playing { .. } => {
                self.service_message(method, ok, message, now_ms, fx)
            }
            _ => {}
        }
    }

    /// A refused registration or a denied load: the named client gives way to the anonymous one,
    /// once; a refusal of the anonymous one ends the session.
    fn next_shape_or_stop(&mut self, now_ms: u64, end: End, fx: &mut Vec<UriEffect>) {
        if self.shape == Shape::Named {
            self.shape = Shape::Anon;
            self.media_id = None;
            self.subscribed = false;
            fx.push(UriEffect::Close);
            self.phase = Phase::Registering { deadline: now_ms + REGISTER_TIMEOUT_MS };
            fx.push(UriEffect::Open(Shape::Anon));
        } else {
            self.stop(now_ms, end, fx);
        }
    }

    fn service_message(&mut self, method: Method, ok: bool, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        let denied = !ok || message.return_value == Some(false);
        let text = message.why();
        match method {
            Method::Load if denied && self.media_id.is_none() && matches!(self.phase, Phase::Loading { .. }) => {
                fx.push(self.line(now_ms, &format!("call load err({text})")));
                self.next_shape_or_stop(now_ms, End::Refused("load"), fx);
                return;
            }
            Method::Subscribe | Method::Load if denied => {
                fx.push(self.line(now_ms, &format!("call {} err({text})", if method == Method::Load { "load" } else { "subscribe" })));
            }
            Method::Play if denied => {
                fx.push(self.line(now_ms, &format!("call play err({text})")));
                self.stop(now_ms, End::Refused("play"), fx);
                return;
            }
            Method::SetWindowId => {
                if denied {
                    self.facts.window_call = WindowCall::Refused;
                    fx.push(self.line(now_ms, &format!("call setWindowId err({text})")));
                } else {
                    self.facts.window_call = WindowCall::Answered;
                    fx.push(self.line(now_ms, "call setWindowId ok"));
                }
                if matches!(self.phase, Phase::SettingWindow { .. }) {
                    self.play(now_ms, fx);
                }
                return;
            }
            Method::Load | Method::Subscribe | Method::Play if !denied && message.return_value == Some(true) && !message.has_events() => {
                fx.push(self.line(now_ms, &format!("call {} ok", match method {
                    Method::Load => "load",
                    Method::Subscribe => "subscribe",
                    _ => "play",
                })));
            }
            _ => {}
        }
        if self.media_id.is_none() && !denied {
            self.media_id = message.media_id.clone();
        }
        if let (Some(media_id), false) = (self.media_id.clone(), self.subscribed) {
            self.subscribed = true;
            fx.push(UriEffect::Call(UriCall::Subscribe { media_id }));
        }
        self.events(message, now_ms, fx);
    }

    fn events(&mut self, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        for seen in &message.seen {
            if self.phase == Phase::Done {
                return;
            }
            self.event(seen, message, now_ms, fx);
        }
        if message.seen.is_empty() {
            if let Some(name) = &message.unknown {
                fx.push(self.line(now_ms, &format!("ev={name} (unparsed)")));
            }
        }
    }

    fn event(&mut self, seen: &Seen, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        match seen.ev {
            Ev::Error => {
                let code = message.error_code;
                let shown = code.map_or_else(|| "?".to_owned(), |c| c.to_string());
                fx.push(self.line(now_ms, &format!("ev=error code={shown} text=\"{}\"", message.error_text.clone().unwrap_or_default())));
                self.stop(now_ms, End::Error(code), fx);
            }
            Ev::LoadCompleted => {
                fx.push(self.line(now_ms, "ev=loadCompleted"));
                if matches!(self.phase, Phase::Loading { .. }) {
                    self.loaded(message, now_ms, fx);
                }
            }
            Ev::Playing => {
                fx.push(self.line(now_ms, "ev=playing"));
                if self.facts.playing_ms.is_none() {
                    self.facts.playing_ms = Some(now_ms.saturating_sub(self.started_ms));
                }
                if matches!(self.phase, Phase::Starting { .. }) {
                    self.phase = Phase::Playing { frozen_at: now_ms + FROZEN_TIME_MS };
                }
            }
            Ev::EndOfStream => {
                fx.push(self.line(now_ms, "ev=endOfStream"));
                if matches!(self.phase, Phase::Starting { .. } | Phase::Playing { .. }) {
                    self.stop(now_ms, End::Eos, fx);
                }
            }
            Ev::CurrentTime => self.current_time(message.current_time, now_ms, fx),
            Ev::VideoInfo => {
                self.facts.video_seen.get_or_insert(now_ms.saturating_sub(self.started_ms));
                fx.push(self.event_line(now_ms, "videoInfo", &seen.detail));
                if let (Some((width, height)), true) = (message.dims, self.config.window_id.is_some()) {
                    if self.placed != Some((width, height)) {
                        self.placed = Some((width, height));
                        fx.push(UriEffect::PlaceWindow { width, height });
                    }
                }
            }
            Ev::AudioInfo => {
                self.facts.audio_seen.get_or_insert(now_ms.saturating_sub(self.started_ms));
                fx.push(self.event_line(now_ms, "audioInfo", &seen.detail));
            }
            other => fx.push(self.event_line(now_ms, other.name(), &seen.detail)),
        }
    }

    /// `loadCompleted`: the attempt is settled as started, then `setWindowId` (late variant with a
    /// window) or `play`.
    fn loaded(&mut self, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if self.media_id.is_none() {
            self.media_id = message.media_id.clone();
        }
        if self.media_id.is_none() {
            fx.push(self.line(now_ms, "loadCompleted carried no mediaId"));
            self.stop(now_ms, End::Refused("mediaId"), fx);
            return;
        }
        self.facts.loaded_ms = Some(now_ms.saturating_sub(self.started_ms));
        self.settle(Settle::Started, fx);
        match (self.config.variant, self.config.window_id.clone(), self.media_id.clone()) {
            (Variant::Late, Some(window_id), Some(media_id)) => {
                self.facts.window_call = WindowCall::Pending;
                self.phase = Phase::SettingWindow { deadline: now_ms + LATE_ID_TIMEOUT_MS };
                fx.push(UriEffect::Call(UriCall::SetWindowId { media_id, window_id }));
            }
            _ => self.play(now_ms, fx),
        }
    }

    fn play(&mut self, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if let Some(media_id) = self.media_id.clone() {
            self.phase = Phase::Starting { deadline: now_ms + PLAY_TIMEOUT_MS };
            fx.push(UriEffect::Call(UriCall::Play { media_id }));
        }
    }

    fn current_time(&mut self, value: Option<i64>, now_ms: u64, fx: &mut Vec<UriEffect>) {
        let Some(value) = value else { return };
        let moved = self.last_time.is_some_and(|last| last != value);
        self.last_time = Some(value);
        if moved {
            if matches!(self.phase, Phase::Playing { .. } | Phase::Starting { .. }) {
                self.phase = Phase::Playing { frozen_at: now_ms + FROZEN_TIME_MS };
            }
            let first = self.facts.time_moved.is_none();
            self.facts.time_moved.get_or_insert(now_ms.saturating_sub(self.started_ms));
            let due = self.time_logged_ms.is_none_or(|at| now_ms.saturating_sub(at) >= TIME_LOG_EVERY_MS);
            if first || due {
                self.time_logged_ms = Some(now_ms);
                fx.push(self.line(now_ms, &format!("ev=currentTime value={value}")));
            }
        } else if self.facts.time_moved.is_none() && self.time_logged_ms.is_none() {
            self.time_logged_ms = Some(now_ms);
            fx.push(self.line(now_ms, &format!("ev=currentTime value={value} (first report)")));
        }
    }

    fn settle(&mut self, how: Settle, fx: &mut Vec<UriEffect>) {
        if self.settled.is_none() {
            self.settled = Some(how);
            fx.push(UriEffect::Settle(how));
        }
    }

    /// The one way out. Settles the attempt if it is still open, unloads once a `mediaId` is known,
    /// always closes, writes the summary, and ends the session.
    fn stop(&mut self, now_ms: u64, end: End, fx: &mut Vec<UriEffect>) {
        if self.phase == Phase::Done {
            return;
        }
        self.facts.failed_in = Some(self.phase.name());
        self.facts.ended = Some(end);
        self.settle(Settle::StartFailed, fx);
        if let Some(media_id) = self.media_id.take() {
            fx.push(self.line(now_ms, "call unload sent"));
            fx.push(UriEffect::Call(UriCall::Unload { media_id }));
        }
        fx.push(UriEffect::Close);
        self.phase = Phase::Done;
        fx.push(self.line(now_ms, &self.summary()));
        fx.push(self.line(now_ms, "tester: report picture yes/no and sound yes/no"));
    }

    /// The terminal line: what the service said, not what the screen showed.
    pub fn summary(&self) -> String {
        let ms = |at: Option<u64>| at.map_or_else(|| "-".to_owned(), |t| t.to_string());
        let seen = |at: Option<u64>| if at.is_some() { "seen" } else { "none" };
        let load = match (self.facts.loaded_ms, self.facts.failed_in) {
            (Some(_), _) => "ok".to_owned(),
            (None, phase) => format!("fail({})", phase.unwrap_or("register")),
        };
        let window_call = match self.facts.window_call {
            WindowCall::NotSent => "n/a",
            WindowCall::Pending | WindowCall::Silent => "noreply",
            WindowCall::Answered => "ok",
            WindowCall::Refused => "refused",
        };
        let end = match self.facts.ended {
            Some(End::Back) | None => "back".to_owned(),
            Some(End::Eos) => "eos".to_owned(),
            Some(End::Error(code)) => format!("error:{}", code.map_or_else(|| "?".to_owned(), |c| c.to_string())),
            Some(End::Timeout(phase)) => format!("timeout:{phase}"),
            Some(End::Refused(phase)) => format!("refused:{phase}"),
        };
        format!(
            "summary load={load} playing={} t_load={} t_playing={} currentTime_advanced={} videoInfo={} audioInfo={} window={} \
             setWindowId={window_call} variant={} scheme={} shape={} end={end}",
            if self.facts.playing_ms.is_some() { "yes" } else { "no" },
            ms(self.facts.loaded_ms),
            ms(self.facts.playing_ms),
            if self.facts.time_moved.is_some() { "yes" } else { "no" },
            seen(self.facts.video_seen),
            seen(self.facts.audio_seen),
            if self.config.window_id.is_some() { "exported" } else { "none" },
            self.config.variant.label(),
            self.config.scheme.label(),
            Self::shape_label(self.shape),
        )
    }
}

impl Message {
    fn has_events(&self) -> bool {
        !self.seen.is_empty()
    }

    /// Why a call was refused, for a log line: the service's code and scrubbed text when it gave
    /// them (`code -1: Denied`), else whatever the transport said.
    fn why(&self) -> String {
        match (self.error_code, &self.error_text) {
            (Some(code), Some(text)) if !text.is_empty() => format!("code {code}: {text}"),
            (Some(code), _) => format!("code {code}"),
            (None, Some(text)) => text.clone(),
            (None, None) => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: &str = "_Window_Id_7";
    /// A media URL as the planner hands it over: a `plex.direct` host, a port, a path and a token.
    const URL: &str = "https://192-168-1-5.0123456789abcdef.plex.direct:32400/library/parts/42/file.mkv?X-Plex-Token=SECRETTOKEN123&x=1";
    /// Every fragment of [`URL`] and its relatives that must never reach an output.
    const LEAKS: [&str; 10] =
        ["192.168", "192-168", "plex.direct", "SECRETTOKEN", "X-Plex-Token", "://", "32400", "/library", "file.mkv", "abc.plex"];

    fn config(variant: Variant, window: Option<&str>) -> Config {
        Config {
            variant,
            scheme: Scheme::Https,
            kind: Kind::Direct,
            window_id: window.map(str::to_owned),
            app_id: "com.test.app".to_owned(),
        }
    }

    /// A probe and the clock the driver would hold.
    struct Rig {
        probe: UriProbe,
        now: u64,
    }

    impl Rig {
        fn new(variant: Variant, window: Option<&str>) -> Rig {
            Rig { probe: UriProbe::new(config(variant, window)), now: 1_000 }
        }
        fn feed(&mut self, ev: UriEvent) -> Vec<UriEffect> {
            let mut fx = Vec::new();
            self.probe.step(ev, self.now, &mut fx);
            fx
        }
        fn after(&mut self, ms: u64, ev: UriEvent) -> Vec<UriEffect> {
            self.now += ms;
            self.feed(ev)
        }
        fn msg(&mut self, method: &'static str, payload: &str) -> Vec<UriEffect> {
            self.feed(UriEvent::from_reply(&UriReply { method, payload: payload.to_owned(), ok: true }))
        }
        fn deny(&mut self, method: &'static str, text: &str) -> Vec<UriEffect> {
            self.feed(UriEvent::from_reply(&UriReply { method, payload: text.to_owned(), ok: false }))
        }
        fn tick(&mut self, ms: u64) -> Vec<UriEffect> {
            self.after(ms, UriEvent::Tick)
        }
        fn start(&mut self) -> Vec<UriEffect> {
            self.feed(UriEvent::Start)
        }
        /// Started, registered, load sent and answered with `_m1` (so subscribed).
        fn loading(&mut self) {
            self.start();
            self.msg("register", "");
            self.msg("load", r#"{"returnValue":true,"mediaId":"_m1"}"#);
        }
        fn loaded(&mut self) -> Vec<UriEffect> {
            self.loading();
            self.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#)
        }
        fn playing(&mut self) {
            self.loaded();
            self.msg("subscribe", r#"{"mediaId":"_m1","playing":true}"#);
        }
    }

    fn count(fx: &[UriEffect], pred: impl Fn(&UriEffect) -> bool) -> usize {
        fx.iter().filter(|e| pred(e)).count()
    }
    fn unloads(fx: &[UriEffect]) -> usize {
        count(fx, |e| matches!(e, UriEffect::Call(UriCall::Unload { .. })))
    }
    fn closes(fx: &[UriEffect]) -> usize {
        count(fx, |e| matches!(e, UriEffect::Close))
    }
    fn settles(fx: &[UriEffect], how: Settle) -> usize {
        count(fx, |e| *e == UriEffect::Settle(how))
    }
    fn logs(fx: &[UriEffect]) -> Vec<&str> {
        fx.iter().filter_map(|e| if let UriEffect::Log(l) = e { Some(l.as_str()) } else { None }).collect()
    }
    fn has_log(fx: &[UriEffect], needle: &str) -> bool {
        logs(fx).iter().any(|l| l.contains(needle))
    }
    fn call(c: UriCall) -> UriEffect {
        UriEffect::Call(c)
    }
    fn summary_of(fx: &[UriEffect]) -> &str {
        logs(fx).into_iter().find(|l| l.contains(" summary ")).expect("a summary line")
    }
    fn load(window: Option<&str>) -> UriEffect {
        call(UriCall::Load { window_id: window.map(str::to_owned) })
    }
    fn media(id: &str) -> String {
        id.to_owned()
    }

    // ------------------------------------------------------------------ happy paths

    #[test]
    fn at_load_happy_path_in_order() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        let fx = rig.start();
        assert_eq!(logs(&fx), ["uriprobe: t=0 armed variant=at-load scheme=https kind=direct window=exported app=com.test.app"]);
        assert_eq!(fx[1], UriEffect::Open(Shape::Named));

        let fx = rig.after(40, UriEvent::Reply { method: Method::Register, ok: true, message: Message::default() });
        assert_eq!(fx, [UriEffect::Log("uriprobe: t=40 register shape=named ok".into()), load(Some(WINDOW))]);

        let fx = rig.after(60, UriEvent::from_reply(&UriReply { method: "load", payload: r#"{"returnValue":true,"mediaId":"_m1"}"#.into(), ok: true }));
        assert_eq!(fx, [UriEffect::Log("uriprobe: t=100 call load ok".into()), call(UriCall::Subscribe { media_id: media("_m1") })]);

        let fx = rig.msg("subscribe", r#"{"returnValue":true,"subscribed":true}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=100 call subscribe ok"]);

        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":1920,"height":1080,"codec":"H264","frameRate":{"num":24000,"den":1001}}}"#);
        assert_eq!(
            fx,
            [
                UriEffect::Log("uriprobe: t=100 ev=videoInfo width=1920 height=1080 codec=H264 frameRate=24000/1001".into()),
                UriEffect::PlaceWindow { width: 1920, height: 1080 },
            ]
        );

        let fx = rig.after(300, UriEvent::from_reply(&UriReply { method: "subscribe", payload: r#"{"mediaId":"_m1","loadCompleted":true}"#.into(), ok: true }));
        assert_eq!(
            fx,
            [
                UriEffect::Log("uriprobe: t=400 ev=loadCompleted".into()),
                UriEffect::Settle(Settle::Started),
                call(UriCall::Play { media_id: media("_m1") }),
            ]
        );
        assert!(!rig.probe.plane_open());

        let fx = rig.after(100, UriEvent::from_reply(&UriReply { method: "subscribe", payload: r#"{"mediaId":"_m1","playing":true}"#.into(), ok: true }));
        assert_eq!(logs(&fx), ["uriprobe: t=500 ev=playing"]);
        assert!(rig.probe.plane_open());

        let fx = rig.after(900, UriEvent::Back);
        assert_eq!(unloads(&fx), 1);
        assert_eq!(closes(&fx), 1);
        assert_eq!(settles(&fx, Settle::StartFailed), 0, "the attempt was settled as started; Back settles nothing");
        assert_eq!(
            summary_of(&fx),
            "uriprobe: t=1400 summary load=ok playing=yes t_load=400 t_playing=500 currentTime_advanced=no videoInfo=seen audioInfo=none \
             window=exported setWindowId=n/a variant=at-load scheme=https shape=named end=back"
        );
        assert!(logs(&fx).last().unwrap().ends_with("tester: report picture yes/no and sound yes/no"));
        assert!(rig.probe.is_done() && !rig.probe.plane_open());
    }

    #[test]
    fn late_variant_loads_without_the_window_then_sets_it_then_plays() {
        let mut rig = Rig::new(Variant::Late, Some(WINDOW));
        rig.start();
        let fx = rig.msg("register", "");
        assert_eq!(fx[1], load(None), "the late variant's load carries no window id");
        rig.msg("load", r#"{"returnValue":true,"mediaId":"_m1"}"#);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
        assert_eq!(
            fx,
            [
                UriEffect::Log("uriprobe: t=0 ev=loadCompleted".into()),
                UriEffect::Settle(Settle::Started),
                call(UriCall::SetWindowId { media_id: media("_m1"), window_id: WINDOW.into() }),
            ],
            "play waits for the setWindowId reply"
        );
        let fx = rig.msg("setWindowId", r#"{"returnValue":true}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 call setWindowId ok"]);
        assert_eq!(fx[1], call(UriCall::Play { media_id: media("_m1") }));
        let fx = rig.feed(UriEvent::Back);
        assert!(summary_of(&fx).contains("setWindowId=ok variant=late"), "{}", summary_of(&fx));
    }

    #[test]
    fn late_variant_logs_a_refused_window_call_and_still_plays() {
        let mut rig = Rig::new(Variant::Late, Some(WINDOW));
        rig.loaded();
        let fx = rig.msg("setWindowId", r#"{"returnValue":false,"errorCode":-1,"errorText":"windowId must be specified"}"#);
        assert!(has_log(&fx, "call setWindowId err(code"), "{fx:?}");
        assert_eq!(fx.last(), Some(&call(UriCall::Play { media_id: media("_m1") })));
        let fx = rig.feed(UriEvent::Back);
        assert!(summary_of(&fx).contains("setWindowId=refused"));
    }

    #[test]
    fn late_variant_on_a_set_with_no_window_plays_straight_after_the_load() {
        let mut rig = Rig::new(Variant::Late, None);
        let fx = rig.loaded();
        assert_eq!(fx.last(), Some(&call(UriCall::Play { media_id: media("_m1") })));
        assert!(!fx.iter().any(|e| matches!(e, UriEffect::Call(UriCall::SetWindowId { .. }))));
        assert!(summary_of(&rig.feed(UriEvent::Back)).contains("window=none setWindowId=n/a"));
    }

    #[test]
    fn no_window_variant_never_names_a_window_even_if_one_exists() {
        let mut rig = Rig::new(Variant::NoWindow, Some(WINDOW));
        let fx = rig.start();
        assert!(has_log(&fx, "variant=none") && has_log(&fx, "window=none"));
        assert_eq!(rig.msg("register", "")[1], load(None));
        rig.msg("load", r#"{"returnValue":true,"mediaId":"_m1"}"#);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":3840,"height":2160}}"#);
        assert!(!fx.iter().any(|e| matches!(e, UriEffect::PlaceWindow { .. })), "no window to place");
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
        assert_eq!(fx.last(), Some(&call(UriCall::Play { media_id: media("_m1") })));
    }

    #[test]
    fn the_armed_line_names_the_scheme_and_kind() {
        let mut probe = UriProbe::new(Config { scheme: Scheme::Http, kind: Kind::Hls, ..config(Variant::AtLoad, None) });
        let mut fx = Vec::new();
        probe.step(UriEvent::Start, 0, &mut fx);
        assert_eq!(logs(&fx), ["uriprobe: t=0 armed variant=at-load scheme=http kind=hls window=none app=com.test.app"]);
    }

    #[test]
    fn events_may_arrive_as_state_words_and_the_old_loaded_word() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","state":"loaded"}"#);
        assert_eq!(settles(&fx, Settle::Started), 1);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","state":"playing"}"#);
        assert!(rig.probe.plane_open() && has_log(&fx, "ev=playing"));
    }

    #[test]
    fn current_time_is_logged_when_it_first_moves_then_once_a_second() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        let time = |rig: &mut Rig, ms: u64, value: i64| rig.after(ms, UriEvent::from_reply(&UriReply { method: "subscribe", payload: format!(r#"{{"currentTime":{value}}}"#), ok: true }));
        assert!(has_log(&time(&mut rig, 10, 0), "(first report)"));
        assert!(time(&mut rig, 10, 0).is_empty(), "the same value again says nothing");
        assert!(has_log(&time(&mut rig, 10, 40), "ev=currentTime value=40"), "first move");
        assert!(time(&mut rig, 400, 440).is_empty(), "inside the second");
        assert!(has_log(&time(&mut rig, 600, 1040), "ev=currentTime value=1040"), "a second on");
        assert!(summary_of(&rig.feed(UriEvent::Back)).contains("currentTime_advanced=yes"));
    }

    // ------------------------------------------------------------------ the register ladder

    #[test]
    fn a_refused_named_registration_falls_to_the_anonymous_one_once() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        let fx = rig.deny("register", "code -1027: Invalid permissions for com.webos.media.client.plxprobe");
        assert_eq!(fx.len(), 3);
        assert!(has_log(&fx, "register shape=named refused(code -1027: Invalid permissions for com.webos.media.client.plxprobe)"), "{fx:?}");
        assert_eq!(fx[1], UriEffect::Close);
        assert_eq!(fx[2], UriEffect::Open(Shape::Anon));
        assert_eq!(settles(&fx, Settle::StartFailed), 0, "the ladder is not an end");

        let fx = rig.msg("register", "");
        assert!(has_log(&fx, "register shape=anon ok") && fx[1] == load(Some(WINDOW)));

        // a second refusal anywhere ends it
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        rig.deny("register", "code -1: no");
        let fx = rig.deny("register", "code -2: no");
        assert_eq!(settles(&fx, Settle::StartFailed), 1);
        assert_eq!((unloads(&fx), closes(&fx)), (0, 1));
        assert!(summary_of(&fx).contains("load=fail(register)") && summary_of(&fx).contains("shape=anon end=refused:register"), "{}", summary_of(&fx));
    }

    #[test]
    fn a_hub_denied_load_advances_the_ladder_once_then_ends() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        rig.msg("register", "");
        let fx = rig.msg("load", r#"{"returnValue":false,"errorCode":-1,"errorText":"Denied method call load for com.webos.media.client.plxprobe"}"#);
        assert!(has_log(&fx, "call load err(code"));
        assert_eq!(&fx[fx.len() - 2..], [UriEffect::Close, UriEffect::Open(Shape::Anon)]);
        assert_eq!(rig.msg("register", "")[1], load(Some(WINDOW)), "the anonymous client loads again");
        let fx = rig.msg("load", r#"{"returnValue":false,"errorCode":-1,"errorText":"Denied"}"#);
        assert_eq!(settles(&fx, Settle::StartFailed), 1);
        assert!(summary_of(&fx).contains("load=fail(load)") && summary_of(&fx).contains("end=refused:load"));
    }

    #[test]
    fn a_load_the_transport_could_not_send_is_a_denied_load() {
        let mut rig = Rig::new(Variant::AtLoad, None);
        rig.start();
        rig.msg("register", "");
        let fx = rig.feed(UriEvent::from_failure("load", "no open worker"));
        assert_eq!(fx.last(), Some(&UriEffect::Open(Shape::Anon)));
    }

    // ------------------------------------------------------------------ timeouts

    #[test]
    fn nothing_times_out_early() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        assert!(rig.tick(REGISTER_TIMEOUT_MS - 1).is_empty());
        rig.msg("register", "");
        assert!(rig.tick(LOAD_TIMEOUT_MS - 1).is_empty());
    }

    #[test]
    fn register_load_and_play_timeouts_end_the_session_once() {
        // register
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        let fx = rig.tick(REGISTER_TIMEOUT_MS);
        assert!(summary_of(&fx).contains("load=fail(register)") && summary_of(&fx).contains("end=timeout:register"));
        assert_eq!((settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (1, 0, 1));
        // load, with the id known
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let fx = rig.tick(LOAD_TIMEOUT_MS);
        assert!(summary_of(&fx).contains("load=fail(load)") && summary_of(&fx).contains("end=timeout:load"));
        assert_eq!((settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (1, 1, 1));
        // play: settled as started before, so the timeout settles nothing again
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loaded();
        assert!(rig.tick(PLAY_TIMEOUT_MS - 1).is_empty());
        let fx = rig.tick(1);
        assert!(summary_of(&fx).contains("load=ok playing=no") && summary_of(&fx).contains("end=timeout:play"));
        assert_eq!((settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (0, 1, 1));
    }

    #[test]
    fn a_silent_set_window_call_is_logged_and_played_past() {
        let mut rig = Rig::new(Variant::Late, Some(WINDOW));
        rig.loaded();
        assert!(rig.tick(LATE_ID_TIMEOUT_MS - 1).is_empty());
        let fx = rig.tick(1);
        assert!(has_log(&fx, "call setWindowId no reply"));
        assert_eq!(fx.last(), Some(&call(UriCall::Play { media_id: media("_m1") })));
        assert!(summary_of(&rig.feed(UriEvent::Back)).contains("setWindowId=noreply"));
    }

    #[test]
    fn a_frozen_current_time_ends_the_session_but_a_silent_one_does_not() {
        let time = |rig: &mut Rig, ms: u64, value: i64| rig.after(ms, UriEvent::from_reply(&UriReply { method: "subscribe", payload: format!(r#"{{"currentTime":{value}}}"#), ok: true }));
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        time(&mut rig, 10, 0);
        time(&mut rig, 10, 50);
        assert!(rig.tick(FROZEN_TIME_MS - 1).is_empty());
        let fx = rig.tick(1);
        assert!(summary_of(&fx).contains("currentTime_advanced=yes") && summary_of(&fx).contains("end=timeout:currentTime"));
        assert_eq!((settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (0, 1, 1));

        // never reported: nothing to call frozen; only Back ends it
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        assert!(rig.tick(10 * FROZEN_TIME_MS).is_empty());
        assert!(!rig.probe.is_done());
    }

    // ------------------------------------------------------------------ Back, errors, exactly once

    /// Back at every point of a session: one close, an unload once the id is known and not before,
    /// the attempt settled as failed only if it never started, and nothing after.
    #[test]
    fn back_in_every_phase_closes_once_unloads_once_known_and_settles_once() {
        type Drive = fn(&mut Rig);
        let phases: [(&str, Variant, Drive, usize, usize); 7] = [
            ("registering", Variant::AtLoad, |r| { r.start(); }, 0, 1),
            ("loading, id unknown", Variant::AtLoad, |r| { r.start(); r.msg("register", ""); }, 0, 1),
            ("loading, id known", Variant::AtLoad, |r| r.loading(), 1, 1),
            ("setting the window", Variant::Late, |r| { r.loaded(); }, 1, 0),
            ("starting", Variant::AtLoad, |r| { r.loaded(); }, 1, 0),
            ("playing", Variant::AtLoad, |r| r.playing(), 1, 0),
            ("late, playing", Variant::Late, |r| { r.loaded(); r.msg("setWindowId", "{}"); r.msg("subscribe", r#"{"playing":true}"#); }, 1, 0),
        ];
        for (name, variant, drive, want_unloads, want_failed) in phases {
            let mut rig = Rig::new(variant, Some(WINDOW));
            drive(&mut rig);
            let fx = rig.feed(UriEvent::Back);
            assert_eq!(unloads(&fx), want_unloads, "{name}: unloads");
            assert_eq!(closes(&fx), 1, "{name}: closes");
            assert_eq!(settles(&fx, Settle::StartFailed), want_failed, "{name}: settle as failed");
            assert_eq!(logs(&fx).iter().filter(|l| l.contains(" summary ")).count(), 1, "{name}: one summary");
            assert!(rig.probe.is_done(), "{name}");
            // after the end: nothing moves, nothing is said
            for ev in [
                UriEvent::Back,
                UriEvent::Tick,
                UriEvent::from_reply(&UriReply { method: "subscribe", payload: r#"{"error":{"errorCode":-1}}"#.into(), ok: true }),
                UriEvent::from_reply(&UriReply { method: "subscribe", payload: r#"{"loadCompleted":true,"mediaId":"_m1"}"#.into(), ok: true }),
                UriEvent::from_reply(&UriReply { method: "unload", payload: r#"{"returnValue":true}"#.into(), ok: true }),
                UriEvent::Start,
            ] {
                assert!(rig.after(100_000, ev).is_empty(), "{name}: effects after Done");
            }
        }
    }

    #[test]
    fn an_error_before_load_completed_fails_the_attempt_and_unloads() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","error":{"errorCode":-7,"errorText":"unsupported codec"}}"#);
        assert!(has_log(&fx, "ev=error code=-7 text=\"unsupported codec\""), "{fx:?}");
        assert_eq!((settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (1, 1, 1));
        assert!(summary_of(&fx).contains("load=fail(load)") && summary_of(&fx).contains("end=error:-7"));
        assert!(rig.msg("subscribe", r#"{"loadCompleted":true}"#).is_empty(), "no second settle, no play");
    }

    #[test]
    fn a_top_level_error_code_is_an_error_but_a_refused_call_and_a_zero_code_are_not() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        assert!(rig.msg("subscribe", r#"{"mediaId":"_m1","errorCode":0,"errorText":""}"#).is_empty());
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","errorCode":103,"errorText":"decode failed"}"#);
        assert!(summary_of(&fx).contains("end=error:103"));
        assert_eq!(settles(&fx, Settle::StartFailed), 0, "it had started");
    }

    #[test]
    fn end_of_stream_ends_with_eos_and_never_settles_the_attempt_again() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","endOfStream":true}"#);
        assert!(summary_of(&fx).contains("end=eos"));
        assert_eq!((settles(&fx, Settle::Started), settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (0, 0, 1, 1));
    }

    #[test]
    fn a_refused_play_ends_the_session() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loaded();
        let fx = rig.msg("play", r#"{"returnValue":false,"errorCode":-3,"errorText":"not loaded"}"#);
        assert!(summary_of(&fx).contains("end=refused:play"));
    }

    #[test]
    fn a_load_completed_without_any_media_id_ends_the_session() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        rig.msg("register", "");
        let fx = rig.msg("load", r#"{"returnValue":true,"loadCompleted":true}"#);
        assert!(summary_of(&fx).contains("end=refused:mediaId"));
        assert_eq!((settles(&fx, Settle::Started), settles(&fx, Settle::StartFailed), unloads(&fx)), (0, 1, 0));
    }

    #[test]
    fn stale_replies_do_nothing_and_a_dropped_count_is_logged() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        assert!(rig.msg("subscribe", r#"{"loadCompleted":true,"mediaId":"_m1"}"#).is_empty(), "no load was sent yet");
        let fx = rig.msg("dropped", "3");
        assert_eq!(logs(&fx), ["uriprobe: t=0 transport dropped 3 replies"]);
    }

    #[test]
    fn an_unknown_event_is_named_and_nothing_else() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let fx = rig.msg("subscribe", &format!(r#"{{"mediaId":"_m1","fooBar":{{"uri":"{URL}"}}}}"#));
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=fooBar (unparsed)"]);
        let fx = rig.msg("subscribe", &format!(r#"{{"mediaId":"_m1","{URL}":1}}"#));
        assert!(fx.is_empty(), "a key that is not an identifier is not even named: {fx:?}");
    }

    // ------------------------------------------------------------------ privacy

    fn assert_clean(what: &str, text: &str) {
        for leak in LEAKS {
            assert!(!text.contains(leak), "{what} leaked {leak:?}: {text}");
        }
    }

    /// A service event that echoes the URL in every place a real one might.
    fn hostile_event() -> String {
        format!(
            r#"{{"mediaId":"_m1","sourceInfo":{{"uri":"{URL}","container":"matroska"}},"streamingInfo":{{"host":"192.168.1.5"}},
            "videoInfo":{{"codec":"{URL}","width":1920,"height":1080,"frameRate":"192.168.1.5","hdr":"abc.plex.direct"}},
            "audioInfo":{{"codec":"aac","channels":2,"sampleRate":48000}},
            "error":{{"errorCode":-3,"errorText":"cannot open {URL} on 192.168.1.5:32400 via abc.plex.direct:32400 token SECRETTOKEN123 (host nas.local)."}}}}"#
        )
    }

    #[test]
    fn scrub_text_replaces_every_word_that_could_name_a_server() {
        let cases = [
            URL,
            "http://192.168.1.5:32400/x",
            "192.168.1.5",
            "192.168.1.5:32400",
            "host.plex.direct",
            "host.plex.direct:32400",
            "10-0-0-5.deadbeef.plex.direct",
            "X-Plex-Token=abc",
            "x-plex-token",
            "/library/metadata/5",
            "user@host",
            "nas:32400",
            "nas.local",
            "(192.168.1.5)",
            "uri=\"http://h\"",
        ];
        for case in cases {
            let out = scrub_text(&format!("failed at {case} now"));
            // the brackets and quotes around a core stay; the core does not
            assert_eq!(out.replace(['(', ')', '"'], ""), "failed at <url> now", "{case}");
        }
    }

    #[test]
    fn scrub_text_keeps_the_hubs_own_service_names_and_plain_words() {
        let out = scrub_text("Denied method call \"load\" for \"com.webos.media.client.plxprobe\" (code -1027).");
        assert!(out.contains("com.webos.media.client.plxprobe") && out.contains("Denied method call"), "{out}");
        assert!(!scrub_text("com.webos.x.plex.direct").contains("plex"));
    }

    #[test]
    fn scrub_text_is_cut_to_the_cap_and_flattens_whitespace() {
        let long = "word ".repeat(100);
        assert_eq!(scrub_text(&long).chars().count(), SCRUBBED_TEXT_CHARS);
        assert_eq!(scrub_text("a\n\tb   c"), "a b c");
        assert_eq!(scrub_text(""), "");
    }

    #[test]
    fn parse_event_keeps_scalars_and_nothing_that_can_name_a_server() {
        let message = parse_event(&hostile_event());
        assert_clean("Message", &format!("{message:?}"));
        assert_eq!(message.media_id.as_deref(), Some("_m1"));
        assert_eq!(message.error_code, Some(-3));
        let names: Vec<&str> = message.seen.iter().map(|s| s.ev.name()).collect();
        assert_eq!(names, ["sourceInfo", "streamingInfo", "videoInfo", "audioInfo", "error"]);
        // the audio fields were clean tokens and numbers, so they survive; the video ones were not
        let audio = message.seen.iter().find(|s| s.ev == Ev::AudioInfo).unwrap();
        assert_eq!(audio.detail, "codec=aac channels=2 sampleRate=48000");
        let video = message.seen.iter().find(|s| s.ev == Ev::VideoInfo).unwrap();
        assert_eq!(video.detail, "width=1920 height=1080", "a URL, an address and a host are refused, not trimmed");
        assert_eq!(message.dims, Some((1920, 1080)));
    }

    #[test]
    fn parse_event_of_text_that_is_not_json_is_empty() {
        assert_eq!(parse_event("not json http://x"), Message::default());
        assert_eq!(parse_event("[1,2]"), Message::default());
        assert_eq!(parse_event(""), Message::default());
    }

    #[test]
    fn a_whole_session_over_hostile_events_writes_no_server_into_any_output() {
        let hostile = hostile_event();
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        let mut all: Vec<UriEffect> = Vec::new();
        all.extend(rig.start());
        all.extend(rig.deny("register", &format!("code -1: Denied for {URL} at 192.168.1.5:32400")));
        all.extend(rig.msg("register", ""));
        all.extend(rig.msg("load", r#"{"returnValue":true,"mediaId":"_m1"}"#));
        all.extend(rig.msg("subscribe", &hostile));
        all.extend(rig.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#));
        all.extend(rig.feed(UriEvent::from_failure("play", &format!("could not send to {URL}"))));
        all.extend(rig.msg("subscribe", r#"{"playing":true}"#));
        all.extend(rig.msg("subscribe", &hostile));
        all.extend(rig.msg("subscribe", &format!(r#"{{"mediaId":"_m1","weird":"{URL}"}}"#)));
        all.extend(rig.feed(UriEvent::Back));
        assert!(rig.probe.is_done());
        assert_clean("the effects", &format!("{all:?}"));
        assert_clean("the summary", summary_of(&all));
        assert_clean("the machine", &format!("{:?}", rig.probe));
        // and the error text did arrive, scrubbed, so the test is not vacuous
        assert!(all.iter().any(|e| matches!(e, UriEffect::Log(l) if l.contains("cannot open <url> on <url> via <url> <url> <url> (host <url>)."))), "{all:?}");
    }

    #[test]
    fn events_built_from_a_reply_hold_no_payload() {
        let reply = UriReply { method: "subscribe", payload: hostile_event(), ok: true };
        assert_clean("UriEvent", &format!("{:?}", UriEvent::from_reply(&reply)));
        assert_clean("failure", &format!("{:?}", UriEvent::from_failure("load", &format!("refused {URL}"))));
    }

    // ------------------------------------------------------------------ payloads and parsers

    #[test]
    fn build_load_escapes_the_url_and_carries_the_window_only_when_given() {
        let tricky = r#"http://h:1/a b?q="x"&r=\y"#;
        let with = build_load(tricky, "com.test.app", Some(WINDOW));
        let value: Value = serde_json::from_str(&with).expect("valid JSON");
        assert_eq!(value["uri"], tricky);
        assert_eq!(value["type"], "media");
        assert_eq!(value["payload"]["option"]["appId"], "com.test.app");
        assert_eq!(value["payload"]["option"]["windowId"], WINDOW);
        let without: Value = serde_json::from_str(&build_load(tricky, "com.test.app", None)).unwrap();
        assert!(without["payload"]["option"].get("windowId").is_none());
    }

    #[test]
    fn the_other_calls_name_their_method_and_payload() {
        let calls = [
            (UriCall::Subscribe { media_id: "_m1".into() }, "subscribe", r#"{"mediaId":"_m1","subscribe":true}"#),
            (UriCall::Play { media_id: "_m1".into() }, "play", r#"{"mediaId":"_m1"}"#),
            (UriCall::Unload { media_id: "_m1".into() }, "unload", r#"{"mediaId":"_m1"}"#),
            (UriCall::SetWindowId { media_id: "_m1".into(), window_id: WINDOW.into() }, "setWindowId", r#"{"mediaId":"_m1","windowId":"_Window_Id_7"}"#),
        ];
        for (call, method, payload) in calls {
            let want: Value = serde_json::from_str(payload).unwrap();
            let got: Value = serde_json::from_str(&call.payload().unwrap()).unwrap();
            assert_eq!((call.method(), got), (method, want));
        }
        let load = UriCall::Load { window_id: None };
        assert_eq!((load.method(), load.payload()), ("load", None));
    }

    #[test]
    fn the_trigger_text_picks_the_variant() {
        for (text, want) in [("", Some(Variant::AtLoad)), ("\n", Some(Variant::AtLoad)), ("load\n", Some(Variant::AtLoad)), (" Late ", Some(Variant::Late)), ("none", Some(Variant::NoWindow)), ("lates", None), ("http://x", None)] {
            assert_eq!(Variant::from_trigger(text), want, "{text:?}");
        }
    }

    #[test]
    fn the_scheme_is_read_from_the_front_of_the_url_only() {
        assert_eq!(Scheme::of(URL), Scheme::Https);
        assert_eq!(Scheme::of("HTTPS://h/x"), Scheme::Https);
        assert_eq!(Scheme::of("http://h/https://x"), Scheme::Http);
        assert_eq!(Scheme::of(""), Scheme::Http);
        assert_eq!(Scheme::of("héllo"), Scheme::Http, "a multi-byte head is not a panic");
    }
}
