//! Pure URI protocol builders and normalized decoder. Only explicit supported envelopes drive state.
use serde_json::{json, Map, Value};
pub(crate) const SCRUBBED_TEXT_CHARS: usize = 120;

/// Opaque service identity. Its contents are never diagnostic text.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaId(String);
impl From<&str> for MediaId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}
impl From<String> for MediaId {
    fn from(value: String) -> Self {
        Self(value)
    }
}
impl std::ops::Deref for MediaId {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for MediaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MediaId(<opaque>)")
    }
}
impl serde::Serialize for MediaId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// The `load` payload: `{"uri":U,"type":"media","payload":{"option":{"appId":A[,"windowId":W]}}}`.
/// Built with the JSON writer, never by formatting the URL into a string.
pub fn build_load(url: &str, app_id: &str, window_id: Option<&str>) -> String {
    let mut option = Map::new();
    option.insert("appId".to_owned(), json!(app_id));
    if let Some(window) = window_id {
        option.insert("windowId".to_owned(), json!(window));
    }
    json!({ "uri": url, "type": "media", "payload": { "option": Value::Object(option) } })
        .to_string()
}

/// Only observed positive, finite video dimensions/rate are useful to an ACB experiment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoFacts { pub width: u32, pub height: u32, pub frame_rate: f64, pub bit_rate: Option<u64> }
impl VideoFacts {
    fn decode(value: &Value) -> Option<Self> {
        let dim = |key| value.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok()).filter(|n| *n > 0);
        let rate = value.get("frameRate")?;
        let frame_rate = rate.as_f64().or_else(|| Some(rate.get("num")?.as_f64()? / rate.get("den")?.as_f64()?))?;
        if !frame_rate.is_finite() || frame_rate <= 0.0 { return None; }
        Some(Self { width: dim("width")?, height: dim("height")?, frame_rate, bit_rate: value.get("bitRate").and_then(Value::as_u64).filter(|n| *n > 0) })
    }
}
/// A validated observed ACB envelope; driver-only content, excluded from all diagnostics.
#[derive(Clone, PartialEq)]
pub struct AcbSource { pub(crate) media_id: MediaId, pub(crate) video: VideoFacts, text: String }
impl std::fmt::Debug for AcbSource { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("AcbSource(<opaque>)") } }
impl AcbSource { pub fn text(&self) -> &str { &self.text } }

/// Pinned Stremio051818e descriptor, allowed ONLY for the explicitly controlled SDR720p24 fixture.
/// Arbitrary URI/HDR streams require an actual validated sourceInfo envelope instead.
pub fn build_acb_fixture_video(media_id: &MediaId, video: VideoFacts) -> Option<String> {
    if video.width != 1280 || video.height != 720 || video.frame_rate != 24.0 { return None; }
    Some(json!({"content":"movie","context":media_id,"video":{
        "adaptive":false,"bitRate":video.bit_rate.unwrap_or(8_000_000),
        "data3D":{"currentPattern":"2d","originalPattern":"2d","typeLR":"LR"},
        "frameRate":video.frame_rate,"hdrType":"none","height":video.height,"width":video.width,
        "mediaSei":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"maxContentLightLevel":0,"maxDisplayMasteringLuminance":0,"maxPicAverageLightLevel":0,"minDisplayMasteringLuminance":0,"whitePointX":0,"whitePointY":0},
        "mediaVui":{"colorPrimaries":2,"matrixCoeffs":2,"transferCharacteristics":2,"videoFullRangeFlag":false},
        "hfr":false,"path":"network","pixelAspectRatio":{"height":1,"width":1},"rotation":"0","scanType":"VIDEO_PROGRESSIVE","specificRendering":"none"
    }}).to_string())
}

/// A call the machine asks the driver to send. Everything a payload needs except the URL.
#[derive(Clone, PartialEq, Eq)]
pub enum UriCall {
    /// `window_id` is the id to put in the payload (`Variant::AtLoad`/`Drive` with a window), else
    /// `None`.
    Load {
        window_id: Option<String>,
    },
    Subscribe {
        media_id: MediaId,
    },
    Play {
        media_id: MediaId,
    },
    NotifyForeground { media_id: MediaId },
    /// Stop the picture where it is; `currentTime` stops moving.
    Pause {
        media_id: MediaId,
    },
    /// Jump to `position`, in milliseconds from the start.
    Seek {
        media_id: MediaId,
        position: i64,
    },
    /// Select track `index` of `kind` (`"text"`, `"audio"`); the reply and `trackSelected` say
    /// whether the pipeline took it.
    SelectTrack {
        media_id: MediaId,
        kind: &'static str,
        index: i64,
    },
    /// Turn the pipeline's own subtitle rendering on.
    SetSubtitleEnable {
        media_id: MediaId,
        enable: bool,
    },
    SetWindowId {
        media_id: MediaId,
        window_id: String,
    },
    Unload {
        media_id: MediaId,
    },
}

impl UriCall {
    /// The method name under `luna://com.webos.media/`.
    pub fn method(&self) -> &'static str {
        match self {
            UriCall::Load { .. } => "load",
            UriCall::Subscribe { .. } => "subscribe",
            UriCall::Play { .. } => "play",
            UriCall::NotifyForeground { .. } => "notifyForeground",
            UriCall::Pause { .. } => "pause",
            UriCall::Seek { .. } => "seek",
            UriCall::SelectTrack { .. } => "selectTrack",
            UriCall::SetSubtitleEnable { .. } => "setSubtitleEnable",
            UriCall::SetWindowId { .. } => "setWindowId",
            UriCall::Unload { .. } => "unload",
        }
    }

    /// The payload, for every call but `load` (which needs the URL: [`build_load`]).
    pub fn payload(&self) -> Option<String> {
        Some(
            match self {
                UriCall::Load { .. } => return None,
                UriCall::NotifyForeground { media_id } => json!({"connectionId":media_id}),
                UriCall::Subscribe { media_id } => {
                    json!({ "mediaId": media_id, "subscribe": true })
                }
                UriCall::Play { media_id }
                | UriCall::Unload { media_id }
                | UriCall::Pause { media_id } => json!({ "mediaId": media_id }),
                UriCall::Seek { media_id, position } => {
                    json!({ "mediaId": media_id, "position": position })
                }
                UriCall::SelectTrack {
                    media_id,
                    kind,
                    index,
                } => json!({ "mediaId": media_id, "type": kind, "index": index }),
                UriCall::SetSubtitleEnable { media_id, enable } => {
                    json!({ "mediaId": media_id, "enable": enable })
                }
                UriCall::SetWindowId {
                    media_id,
                    window_id,
                } => json!({ "mediaId": media_id, "windowId": window_id }),
            }
            .to_string(),
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Events in: the whitelist

/// A service event the probe knows by name. Anything else is logged as `ev=<name> (unparsed)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ev {
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
    pub(crate) fn name(self) -> &'static str {
        EVENT_KEYS
            .iter()
            .find(|(_, ev)| *ev == self)
            .map_or("?", |(key, _)| key)
    }

    /// The event a bare word names: the state words some builds send as a value
    /// (`"state":"playing"`), and the old client's `"loaded"`.
    fn of_word(word: &str) -> Option<Ev> {
        if word == "loaded" {
            return Some(Ev::LoadCompleted);
        }
        EVENT_KEYS
            .iter()
            .find(|(key, _)| *key == word)
            .map(|(_, ev)| *ev)
    }
}

/// One whitelisted event of a message with the whitelisted scalars it carried, already formatted
/// (`width=1920 height=1080 codec=H264`). Holds no raw text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Seen {
    pub(crate) ev: Ev,
    pub(crate) detail: String,
}

/// Opaque identity, normalized facts and a separate diagnostic projection. Debug excludes all
/// service text and identity contents. The private ACB source envelope retains service text for
/// the driver to forward; neither it nor the original message is diagnostic output.
#[derive(Clone, Default, PartialEq)]
pub struct Message {
    pub(crate) return_value: Option<bool>,
    pub(crate) media_id: Option<MediaId>,
    pub(crate) seen: Vec<Seen>,
    /// A fixed marker when no supported event was recognized; arbitrary service keys stay private.
    pub(crate) unknown: Option<String>,
    pub(crate) error_code: Option<i64>,
    pub(crate) error_text: Option<String>,
    pub(crate) current_time: Option<i64>,
    pub(crate) dims: Option<(u32, u32)>,
    pub(crate) dropped: Option<u64>,
    /// Supported explicit events only; `seen` is the diagnostic projection.
    pub(crate) normalized: Vec<Ev>,
    pub(crate) identity_conflict: bool,
    pub(crate) video_info: Option<VideoFacts>,
    pub(crate) acb_source: Option<AcbSource>,
    pub(crate) selected_track: Option<(&'static str, i64)>,
    pub(crate) reply_request: Option<plx_platform::tv::uri::RequestId>,
    pub(crate) reply_track: Option<&'static str>,
    pub(crate) confirmed_track: Option<&'static str>,
}

/// A typed diagnostic scalar: numbers, booleans, a finite codec/HDR vocabulary, and
/// a `{num, den}` pair as `num/den`.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::String(s) => match s.as_str() {
            "aac" | "ac3" | "eac3" | "dts" | "H264" | "H265" | "h264" | "hevc" | "AV1" | "VP9"
            | "sdr" | "hdr10" | "dolbyVision" => Some(s.clone()),
            _ => None,
        },
        Value::Object(o) => {
            let part = |key| o.get(key).and_then(Value::as_i64);
            Some(format!("{}/{}", part("num")?, part("den")?))
        }
        _ => None,
    }
}

/// `key=value` pairs for the listed keys of `value` (an object), in the listed order.
fn details(value: &Value, keys: &[&str]) -> String {
    let Some(object) = value.as_object() else {
        return String::new();
    };
    let parts: Vec<String> = keys
        .iter()
        .filter_map(|key| {
            object
                .get(*key)
                .and_then(scalar)
                .map(|shown| format!("{key}={shown}"))
        })
        .collect();
    parts.join(" ")
}

/// The position of a `currentTime` event, in either shape a firmware may send: a bare number
/// (`{"currentTime":123}`, what uMediaClient reads) or the nested object umediaserver documents
/// (`{"currentTime":{"currentTime":123,"mediaId":"_m1"}}`). Identity is decoded separately;
/// invalid, negative or overflowing clock values never become a position.
fn current_time_of(value: &Value) -> Option<i64> {
    let number = |v: &Value| {
        if let Some(n) = v.as_i64() {
            return (n >= 0).then_some(n);
        }
        let f = v.as_f64()?;
        (f.is_finite() && f >= 0.0 && f < 9_223_372_036_854_775_808.0).then(|| f.trunc() as i64)
    };
    number(value).or_else(|| {
        value
            .as_object()
            .and_then(|inner| inner.get("currentTime"))
            .and_then(number)
    })
}

/// The keys of a message that are not events: how a reply is told from an unknown event.
const PLAIN_KEYS: [&str; 6] = [
    "returnValue",
    "mediaId",
    "subscribed",
    "subscribe",
    "errorCode",
    "errorText",
];

/// Reduce a service payload to what the probe may keep. A whitelist: a key it does not know
/// contributes only a fixed unknown marker; a value it does not know is never read.
pub fn parse_event(payload: &str) -> Message {
    let mut message = Message::default();
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(payload) else {
        return message;
    };
    message.return_value = object.get("returnValue").and_then(Value::as_bool);
    message.media_id = object
        .get("mediaId")
        .and_then(Value::as_str)
        .map(MediaId::from);
    // The error fields sit at the top level or inside an `error` object.
    let error_object = object.get("error").and_then(Value::as_object);
    let field = |key: &str| {
        object
            .get(key)
            .or_else(|| error_object.and_then(|inner| inner.get(key)))
    };
    message.error_code = field("errorCode").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
    });
    message.error_text = field("errorText")
        .and_then(Value::as_str)
        .or_else(|| object.get("error").and_then(Value::as_str))
        .map(diagnostic_text);
    message.current_time = object.get("currentTime").and_then(current_time_of);
    message.video_info = object.get("videoInfo").and_then(VideoFacts::decode);
    if let Some(source) = object.get("sourceInfo").filter(|value| value.is_object()) {
        if let (Some(id), Some(video)) = (source.get("context").and_then(Value::as_str), source.get("video").and_then(VideoFacts::decode)) {
            message.acb_source = Some(AcbSource {media_id: id.into(), video, text: source.to_string()});
            if message.video_info.is_none() { message.video_info = Some(video); }
        }
    }
    // Known event envelopes can carry the same opaque identity. The ACB experiment reads only
    // sourceInfo context and numeric video facts above; streamingInfo remains diagnostic-only.
    if object.get("mediaId").is_some_and(|id| !id.is_string()) {
        message.identity_conflict = true;
    }
    for key in [
        "loadCompleted",
        "playing",
        "paused",
        "endOfStream",
        "currentTime",
        "videoInfo",
        "seekDone",
        "trackSelected",
        "unloadCompleted",
    ] {
        let Some(inner) = object.get(key).and_then(Value::as_object) else {
            continue;
        };
        if inner.get("mediaId").is_some_and(|id| !id.is_string()) {
            message.identity_conflict = true;
        }
        if let Some(id) = inner.get("mediaId").and_then(Value::as_str) {
            let id = MediaId::from(id);
            if message.media_id.as_ref().is_some_and(|outer| outer != &id) {
                message.identity_conflict = true;
            } else {
                message.media_id = Some(id);
            }
        }
    }
    // Only the already-validated native ACB envelope gives sourceInfo.context identity meaning.
    if let Some(source) = message.acb_source.as_ref() {
        if message.media_id.as_ref().is_some_and(|outer| outer != &source.media_id) { message.identity_conflict = true; }
        else { message.media_id = Some(source.media_id.clone()); }
    } else if let Some(source) = object.get("sourceInfo") {
        if source.get("video").and_then(VideoFacts::decode).is_some()
            && source.get("context").is_some_and(|id| !id.is_string()) { message.identity_conflict = true; }
    }
    let track = object.get("trackSelected").and_then(Value::as_object);
    message.selected_track = track.and_then(|track| {
        let kind = match track.get("type")?.as_str()? {
            "text" => "text",
            "audio" => "audio",
            _ => return None,
        };
        Some((kind, track.get("index")?.as_i64()?))
    });
    for (key, ev) in EVENT_KEYS {
        let Some(value) = object.get(key) else {
            continue;
        };
        if matches!(value, Value::Null | Value::Bool(false)) {
            continue;
        }
        let detail = match ev {
            Ev::VideoInfo => details(value, &["width", "height", "codec", "frameRate", "hdr"]),
            Ev::AudioInfo => details(value, &["codec", "channels", "sampleRate"]),
            Ev::BufferRange => details(value, &["beginTime", "endTime"]),
            Ev::CurrentTime => scalar(value)
                .map(|v| format!("value={v}"))
                .unwrap_or_default(),
            _ => String::new(),
        };
        if ev == Ev::VideoInfo {
            let dim = |key| {
                value
                    .get(key)
                    .and_then(Value::as_u64)
                    .and_then(|n| u32::try_from(n).ok())
                    .filter(|n| *n > 0)
            };
            message.dims = dim("width").zip(dim("height"));
        }
        let supported = match ev {
            Ev::CurrentTime => message.current_time.is_some(),
            Ev::VideoInfo
            | Ev::AudioInfo
            | Ev::SourceInfo
            | Ev::StreamingInfo
            | Ev::BufferRange => value.is_object(),
            _ => value == &Value::Bool(true) || value.is_object(),
        };
        if supported {
            message.normalized.push(ev);
        }
        message.seen.push(Seen { ev, detail });
    }
    // Explicit envelope names only. A description, URL or unrelated string is not state.
    for key in ["state", "event"] {
        if let Some(ev) = object
            .get(key)
            .and_then(Value::as_str)
            .and_then(Ev::of_word)
        {
            if !message.normalized.contains(&ev) {
                message.normalized.push(ev);
            }
            if message.seen.iter().all(|seen| seen.ev != ev) {
                message.seen.push(Seen {
                    ev,
                    detail: String::new(),
                });
            }
        }
    }
    // An error event: an `error` key (above), or error fields on a message that is not a refused call.
    if message.error_code.is_some_and(|code| code != 0) && message.return_value != Some(false) {
        if message.seen.iter().all(|seen| seen.ev != Ev::Error) {
            message.seen.push(Seen {
                ev: Ev::Error,
                detail: String::new(),
            });
        }
        if !message.normalized.contains(&Ev::Error) {
            message.normalized.push(Ev::Error);
        }
    }
    if message.seen.is_empty() {
        message.unknown = object
            .keys()
            .any(|key| !PLAIN_KEYS.contains(&key.as_str()))
            .then(|| "unknown".to_owned());
    }
    if message.identity_conflict || message.return_value == Some(false) {
        message.normalized.clear();
        message.current_time = None;
        message.selected_track = None;
        message.video_info = None;
        message.acb_source = None;
        // Refusals still belong to an opaque identity and must pass the owner's check.
        if message.identity_conflict {
            message.media_id = None;
        }
    }
    message
}

/// First-stage address redaction. `diagnostic_text` also constrains words before logging:
/// every whitespace-delimited word
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

/// Keep known diagnostic words after address scrubbing. Unknown service text can be a credential.
pub(crate) fn diagnostic_text(text: &str) -> String {
    let scrubbed = scrub_text(text);
    scrubbed
        .split_whitespace()
        .map(|word| {
            let bare = word.trim_matches(|c: char| {
                matches!(
                    c,
                    '.' | ','
                        | ';'
                        | ':'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '\"'
                        | '\''
                        | '<'
                        | '>'
                        | '!'
                        | '?'
                )
            });
            let lower = bare.to_ascii_lowercase();
            let safe = word.contains("<url>")
                || matches!(
                    lower.as_str(),
                    "setup"
                        | "stage=app-id"
                        | "stage=glib-context"
                        | "stage=register"
                        | "stage=attach"
                        | "stage=register-unregister"
                        | "stage=attach-unregister"
                        | "denied"
                        | "invalid"
                        | "permissions"
                        | "permission"
                        | "method"
                        | "call"
                        | "load"
                        | "play"
                        | "pause"
                        | "seek"
                        | "selecttrack"
                        | "setsubtitleenable"
                        | "setwindowid"
                        | "subscribe"
                        | "unload"
                        | "register"
                        | "registration"
                        | "for"
                        | "code"
                        | "no"
                        | "bus"
                        | "named"
                        | "anon"
                        | "refused"
                        | "cannot"
                        | "open"
                        | "on"
                        | "via"
                        | "host"
                        | "at"
                        | "could"
                        | "not"
                        | "send"
                        | "to"
                        | "worker"
                        | "already"
                        | "gone"
                        | "ended"
                        | "closing"
                        | "unsupported"
                        | "codec"
                        | "windowid"
                        | "must"
                        | "be"
                        | "specified"
                        | "service"
                        | "error"
                        | "failed"
                        | "the"
                        | "a"
                        | "an"
                        | "with"
                        | "is"
                        | "and"
                        | "com.webos.media"
                        | "com.webos.media.client.plxprobe"
                );
            if safe {
                word.to_owned()
            } else {
                "<text>".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(SCRUBBED_TEXT_CHARS)
        .collect()
}

fn scrub_word(word: &str) -> String {
    let edge = |c: char| {
        matches!(
            c,
            '.' | ','
                | ';'
                | ':'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '"'
                | '\''
                | '<'
                | '>'
                | '!'
                | '?'
        )
    };
    let bare = word.trim_matches(edge);
    let lower = bare.to_ascii_lowercase();
    let service = ["com.webos.", "com.palm.", "com.lge.", "com.beb."]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
        && bare
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    let host_port = bare
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()));
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

impl Message {
    pub(crate) fn has_events(&self) -> bool {
        !self.seen.is_empty()
    }

    /// Why a call was refused, for a log line: the service's code and scrubbed text when it gave
    /// them (`code -1: Denied`), else whatever the transport said.
    pub(crate) fn why(&self) -> String {
        match (self.error_code, &self.error_text) {
            (Some(code), Some(text)) if !text.is_empty() => format!("code {code}: {text}"),
            (Some(code), _) => format!("code {code}"),
            (None, Some(text)) => text.clone(),
            (None, None) => String::new(),
        }
    }
}

impl std::fmt::Debug for UriCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UriCall")
            .field("method", &self.method())
            .finish()
    }
}
impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Message")
            .field("return_value", &self.return_value)
            .field("media_id", &self.media_id)
            .field("events", &self.normalized)
            .field("current_time", &self.current_time)
            .field("identity_conflict", &self.identity_conflict)
            .finish()
    }
}
impl Message {
    pub(crate) fn transport_failure(text: &str) -> Self {
        Self {
            error_text: Some(diagnostic_text(text)),
            ..Self::default()
        }
    }
    pub fn accepted(&self) -> Option<bool> {
        self.return_value
    }
    pub fn media_id(&self) -> Option<&MediaId> {
        self.media_id.as_ref()
    }
    pub fn position_ms(&self) -> Option<i64> {
        self.current_time
    }
    pub fn events(&self) -> &[Ev] {
        &self.normalized
    }
    pub fn identity_conflict(&self) -> bool {
        self.identity_conflict
    }
}

/// Which call a reply answers (the transport tags every message with the method that was sent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Method {
    Register,
    Load,
    Subscribe,
    Play,
    NotifyForeground,
    Pause,
    Seek,
    SelectTrack,
    SetSubtitleEnable,
    SetWindowId,
    Unload,
    Dropped,
    Other,
}

impl Method {
    pub fn of(name: &str) -> Method {
        match name {
            "register" => Method::Register,
            "load" => Method::Load,
            "subscribe" => Method::Subscribe,
            "play" => Method::Play,
            "notifyForeground" => Method::NotifyForeground,
            "pause" => Method::Pause,
            "seek" => Method::Seek,
            "selectTrack" => Method::SelectTrack,
            "setSubtitleEnable" => Method::SetSubtitleEnable,
            "setWindowId" => Method::SetWindowId,
            "unload" => Method::Unload,
            "dropped" => Method::Dropped,
            _ => Method::Other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn uri_protocol_strict_events_ignore_false_null_and_unrelated_words() {
        for text in [
            r#"{"description":"playing"}"#,
            r#"{"description":"endOfStream","playing":false}"#,
            r#"{"playing":null}"#,
            r#"{"playing":"yes"}"#,
        ] {
            assert!(parse_event(text).events().is_empty(), "{text}");
        }
        assert_eq!(
            parse_event(r#"{"state":"playing"}"#).events(),
            &[Ev::Playing]
        );
        assert_eq!(parse_event(r#"{"playing":true}"#).events(), &[Ev::Playing]);
        assert_eq!(
            parse_event(r#"{"returnValue":true}"#).accepted(),
            Some(true)
        );
        assert!(parse_event(r#"{"returnValue":true}"#).events().is_empty());
        assert_eq!(
            parse_event(r#"{"error":"failure","errorCode":104}"#).events(),
            &[Ev::Error]
        );
        assert!(parse_event(r#"{"returnValue":false,"playing":true}"#)
            .events()
            .is_empty());
    }
    #[test]
    fn uri_protocol_clock_shapes_preserve_identity_and_reject_invalid_conversions() {
        assert_eq!(
            parse_event(r#"{"currentTime":40.9}"#).position_ms(),
            Some(40)
        );
        let nested =
            parse_event(r#"{"currentTime":{"currentTime":41,"mediaId":"opaque://id?secret"}}"#);
        assert_eq!(nested.position_ms(), Some(41));
        assert_eq!(
            parse_event(r#"{"loadCompleted":{"mediaId":"nested"}}"#)
                .media_id()
                .map(|id| &**id),
            Some("nested")
        );
        assert_eq!(
            nested.media_id().map(|id| &**id),
            Some("opaque://id?secret")
        );
        let conflict = parse_event(
            r#"{"mediaId":"a","currentTime":{"currentTime":41,"mediaId":"b"},"playing":true}"#,
        );
        assert!(conflict.identity_conflict());
        assert!(conflict.events().is_empty());
        assert_eq!(conflict.position_ms(), None);
        for value in [
            "-1",
            "-0.01",
            "9223372036854775808",
            "18446744073709551615",
            "1e100",
            "1e999",
            "null",
            r#""123""#,
        ] {
            let parsed = parse_event(&format!("{{\"currentTime\":{value}}}"));
            assert_eq!(parsed.position_ms(), None, "{value}");
            assert!(!parsed.events().contains(&Ev::CurrentTime), "{value}");
        }
    }
    #[test]
    fn uri_protocol_diagnostics_and_debug_never_copy_arbitrary_service_strings() {
        let message = parse_event(
            r#"{"mediaId":"SECRETID","SECRETKEY":"playing","videoInfo":{"codec":"SECRET_CODEC","width":1920},"error":{"errorCode":-1,"errorText":"SECRET_CREDENTIAL"}}"#,
        );
        let shown = format!("{message:?}");
        assert!(!shown.contains("SECRET"));
        assert_eq!(message.error_text.as_deref(), Some("<text>"));
        assert!(!format!("{:?}", message.seen).contains("SECRET"));
        assert_eq!(
            diagnostic_text("no bus 54321 987654321"),
            "no bus <text> <text>"
        );
        assert!(!format!(
            "{:?}",
            UriCall::Play {
                media_id: "SECRETID".into()
            }
        )
        .contains("SECRET"));
    }
    #[test]
    fn uri_acb_video_facts_are_strict_and_fixture_payload_has_pinned_shape() {
        for value in [r#"{"width":1280,"height":720}"#,r#"{"width":0,"height":720,"frameRate":24}"#,r#"{"width":1280,"height":720,"frameRate":-1}"#,r#"{"width":1280,"height":720,"frameRate":{"num":24,"den":0}}"#] {
            assert!(parse_event(&format!("{{\"videoInfo\":{value}}}")).video_info.is_none());
        }
        let facts=parse_event(r#"{"videoInfo":{"width":1280,"height":720,"frameRate":{"num":24000,"den":1001}}}"#).video_info.unwrap();
        assert_eq!(facts.frame_rate,24000.0/1001.0); assert!(build_acb_fixture_video(&"m".into(),facts).is_none());
        let facts=parse_event(r#"{"videoInfo":{"width":1280,"height":720,"frameRate":24}}"#).video_info.unwrap();
        let payload:Value=serde_json::from_str(&build_acb_fixture_video(&"m".into(),facts).unwrap()).unwrap();
        assert_eq!(payload["context"],"m"); assert_eq!(payload["video"]["hdrType"],"none"); assert_eq!(payload["video"]["mediaVui"]["transferCharacteristics"],2);
        assert_eq!(serde_json::from_str::<Value>(&UriCall::NotifyForeground{media_id:"m".into()}.payload().unwrap()).unwrap(),json!({"connectionId":"m"}));
    }

    #[test]
    fn uri_acb_observed_envelope_round_trips_privately_and_refusal_discards_it() {
        let input=r#"{"mediaId":"m","sourceInfo":{"context":"m","video":{"width":1280,"height":720,"frameRate":24},"uri":"http://SECRET/?token=SECRET"}}"#;
        let message=parse_event(input); let source=message.acb_source.as_ref().unwrap();
        assert!(source.text().contains("SECRET"));
        assert!(!format!("{message:?} {source:?}").contains("SECRET"));
        let refused=parse_event(&input.replace("{\"mediaId", "{\"returnValue\":false,\"mediaId"));
        assert!(refused.acb_source.is_none()); assert!(refused.video_info.is_none());
    }

    #[test]
    fn uri_acb_review_video_info_identity_is_validated_before_facts() {
        let nested=parse_event(r#"{"videoInfo":{"mediaId":"other","width":1280,"height":720,"frameRate":24}}"#);
        assert_eq!(nested.media_id.as_deref(),Some("other"),"nested videoInfo identity must survive for owner validation");
        for id in [r#""other""#, "42", "null"] {
            let payload=format!(r#"{{"mediaId":"owner","videoInfo":{{"mediaId":{id},"width":1280,"height":720,"frameRate":24}}}}"#);
            let message=parse_event(&payload);
            assert!(message.identity_conflict,"{payload}");
            assert!(message.video_info.is_none(),"invalid identity published binding facts: {message:?}");
        }
        for context in [r#""other""#, "42", "null"] {
            let payload=format!(r#"{{"mediaId":"owner","sourceInfo":{{"context":{context},"video":{{"width":1280,"height":720,"frameRate":24}}}}}}"#);
            let message=parse_event(&payload);
            assert!(message.identity_conflict,"{payload}");
            assert!(message.video_info.is_none() && message.acb_source.is_none());
        }

    }

}
