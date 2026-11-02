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
//! ([`parse_event`]) and constrained, scrubbed diagnostic words, never from a raw payload: a service event can
//! echo the URI (`sourceInfo`, `streamingInfo`, an error), and the log is posted by a tester.
//!
//! **Phases.** `Registering` (ladder: the named client, then the anonymous one, once), `Loading`
//! (`load` sent; the reply carries `mediaId`, which is subscribed; waits for `loadCompleted`),
//! `SettingWindow` ([`Variant::Late`] only: `setWindowId` after the load, its reply logged),
//! `Starting` (`play` sent; waits for `playing`), `Playing` (watches `currentTime`), then `Done`.
//! Back and every failure go to logical `Done` through one `stop`: `unload` once a `mediaId` is
//! known, `close` requested, the attempt settled exactly once and the summary written once. The
//! separate ownership reducer retains the registration/window until confirmed release. Named→anon
//! retry waits in `AwaitingClose`, then arms the new register deadline after `Closed`.
//!
//! The ACB arms require an existing boot-created playerType0 handle and a known SDR720p24
//! fixture unless a validated native sourceInfo envelope arrives. They start service Play at
//! loadCompleted independently of the staged 200/200/200/300ms bind. Bus release triggers one
//! main-thread UNLOADED, then plane ownership stays quarantined until process exit: each arm
//! requires a fresh process. No callback or return value is promoted to proof of a picture.
//!
//! The probe cannot see the video plane. The summary reports what the service said; the tester's
//! eyes are the verdict, and no `video=shown` token is guessed.
use plx_platform::tv::uri::{Shape, UriReply};
#[cfg(test)]
use serde_json::Value;

/// How long each wait lasts, in milliseconds. A wait that runs out ends the session with
/// `timeout:<phase>`, except the window call, whose silence is logged and played past.
const REGISTER_TIMEOUT_MS: u64 = 10_000;
const LOAD_TIMEOUT_MS: u64 = 20_000;
const LATE_ID_TIMEOUT_MS: u64 = 5_000;
const PLAY_TIMEOUT_MS: u64 = 20_000;
/// `currentTime` must change at least this often once it has been reported at all.
const FROZEN_TIME_MS: u64 = 10_000;
/// The transport script (`Variant::Drive`): how long after `playing` the first step runs, and how
/// long each following step waits.
const DRIVE_FIRST_MS: u64 = 3_000;
const DRIVE_STEP_MS: u64 = 2_500;
/// How far past the position at seek time the script jumps.
const DRIVE_SEEK_MS: i64 = 20_000;
/// How close to the requested position a following `currentTime` counts as landing on it.
const DRIVE_SEEK_TOLERANCE_MS: i64 = 5_000;
/// `currentTime` is logged when it first moves and then at most this often.
const TIME_LOG_EVERY_MS: u64 = 1_000;
/// The longest an error text is kept after [`scrub_text`], in characters.

// ---------------------------------------------------------------------------------------------
// What the tester chooses and what the route says

/// What the trigger file asks for: where the window id goes (empty or `load`, `late`, `none`), and
/// `drive`, which binds like `load` and then drives the transport, plus two explicit controlled
/// ACB experiment arms. These share one developer build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    /// `windowId` in the `load` payload.
    AtLoad,
    /// `load` without it, then `setWindowId` after `loadCompleted`, then `play`.
    Late,
    /// No window id at all.
    NoWindow,
    /// `windowId` in the `load` payload, then pause, resume, seek, a text and an audio track, and
    /// subtitle enable, each logged with the position around it.
    Drive,
    /// Controlled known-SDR720p24 experiment: foreground + staged ACB, without connectDass.
    AcbControl,
    /// Identical controlled experiment, adding only fixed-zero connectDass in its timed slot.
    AcbDass,
}

impl Variant {
    /// The variant a trigger file's text names; `None` for text that names none, so the driver
    /// can say so instead of quietly running another variant.
    pub fn from_trigger(content: &str) -> Option<Variant> {
        match content.trim().to_ascii_lowercase().as_str() {
            "" | "load" => Some(Variant::AtLoad),
            "late" => Some(Variant::Late),
            "none" => Some(Variant::NoWindow),
            "drive" => Some(Variant::Drive),
            "acb-control" => Some(Variant::AcbControl),
            "acb-dass" => Some(Variant::AcbDass),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Variant::AtLoad => "at-load",
            Variant::Late => "late",
            Variant::NoWindow => "none",
            Variant::Drive => "drive",
            Variant::AcbControl => "acb-control",
            Variant::AcbDass => "acb-dass",
        }
    }

    /// Does the `load` payload carry the window id?
    fn binds_at_load(self) -> bool {
        matches!(self, Variant::AtLoad | Variant::Drive)
    }

    /// Is this one of the controlled, fresh-process ACB experiment arms?
    pub fn acb(self) -> bool { matches!(self, Self::AcbControl | Self::AcbDass) }

    /// Does the session run the transport script once the service says `playing`?
    pub fn drives(self) -> bool {
        self == Variant::Drive
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
    /// Run the transport script after `playing` ([`Variant::Drive`]).
    pub transport: bool,
}

// ---------------------------------------------------------------------------------------------
// Payloads. The only place a URL is spelled into text.

pub use super::uri_protocol::{build_load, parse_event, scrub_text, MediaId, Message, Method, UriCall};
use super::uri_protocol::VideoFacts;
use super::uri_protocol::{Ev, Seen};
#[cfg(test)]
use super::uri_protocol::SCRUBBED_TEXT_CHARS;

// ---------------------------------------------------------------------------------------------
// Events in

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AcbRequest(u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcbStep { Check, MediaId, Loaded, Connect, VideoData, Display }
#[derive(Clone, Debug, PartialEq)]
pub enum AcbOp {
    Check { dass: bool },
    MediaId { media_id: MediaId },
    Loaded,
    Connect { enabled: bool },
    VideoData { media_id: MediaId, video: VideoFacts },
    Display,
}

/// What the driver feeds [`UriProbe::step`].
#[derive(Clone, Debug, PartialEq)]
pub enum UriEvent {
    /// The probe is armed: begin registering.
    Start,
    /// Back (or any stop the driver wants): end the session now.
    Back,
    /// The previous registration actually released; the retry can now register.
    Closed,
    AcbReleaseTail { session: plx_platform::tv::uri::SessionId },
    AcbAnswered { request: AcbRequest, invoked: bool, completed_ms: u64 },
    /// Correlation assigned by the ownership reducer before this call can answer.
    Dispatched { method: Method, kind: Option<&'static str>, request: plx_platform::tv::uri::RequestId },
    /// Time has passed: the deadlines are checked against `now_ms`.
    Tick,
    /// A message from the transport, or the transport's refusal to send one.
    Reply { method: Method, ok: bool, message: Message },
}

impl UriEvent {
    /// The diagnostic entrypoint: drop the raw payload after normalized decoding. Production
    /// replies pass through the ownership reducer first, which validates session and request.
    pub fn from_reply(reply: &UriReply) -> UriEvent {
        let method = Method::of(reply.method);
        // `dropped` comes first: the transport's queue sends it with `ok: false` and a count, not a
        // refusal's text (`ReplyQueue::drain`).
        let message = if method == Method::Dropped {
            Message { dropped: reply.payload.trim().parse().ok(), ..Message::default() }
        } else if !reply.ok {
            Message::transport_failure(&reply.payload)
        } else if method == Method::Register {
            Message::default()
        } else {
            parse_event(&reply.payload)
        };
        let mut message = message;
        message.reply_request = Some(reply.request);
        UriEvent::Reply { method, ok: reply.ok, message }
    }

    /// The event for a transport call that returned `Err` at once (`open` or `call` refused).
    pub fn from_failure(method: &'static str, text: &str) -> UriEvent {
        UriEvent::Reply {
            method: Method::of(method),
            ok: false,
            message: Message::transport_failure(text),
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
#[derive(Clone, Debug, PartialEq)]
pub enum UriEffect {
    /// Register on the bus as `Shape`; the outcome returns as the `register` reply.
    Open(Shape),
    Acb { request: AcbRequest, op: AcbOp },
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
    AwaitingClose,
    AcbBinding { request: AcbRequest, step: AcbStep, deadline: u64 },
    AcbWaitingVideo { deadline: u64 },
    AcbWait { next: AcbStep, deadline: u64 },
    Loading { deadline: u64 },
    SettingWindow { deadline: u64 },
    Starting { deadline: u64 },
    Playing { frozen_at: u64 },
    /// The transport script ([`Variant::Drive`]): `step` runs at `deadline`.
    Driving { step: usize, deadline: u64 },
    Done,
}

impl Phase {
    /// The name a failure in this phase is reported under.
    fn name(self) -> &'static str {
        match self {
            Phase::Idle | Phase::Registering { .. } | Phase::AwaitingClose => "register",
            Phase::AcbBinding {..} | Phase::AcbWaitingVideo {..} | Phase::AcbWait {..} => "acb",
            Phase::Loading { .. } => "load",
            Phase::SettingWindow { .. } => "setWindowId",
            Phase::Starting { .. } => "play",
            Phase::Playing { .. } | Phase::Driving { .. } | Phase::Done => "currentTime",
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
    /// The transport script ran to its last step.
    Script,
}

/// What became of one transport call (`pause`, `seek`, `selectTrack`, `setSubtitleEnable`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CallResult {
    #[default]
    NotSent,
    /// The diagnostic has its required evidence: an attributed event, or the ACK-only subtitle switch.
    Ok,
    /// The service said ok, but the event that proves it did not follow.
    Unconfirmed,
    Refused,
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

/// Reported-position evidence collected strictly after Pause dispatch and before Resume.
/// This is a diagnostic sampling criterion, never proof of hardware pause behavior. A stationary
/// grade requires exact equality, observations spanning at least half the actual dwell, and a
/// final report observed in the Resume frame. No firmware jitter or reporting cadence is assumed.
#[derive(Debug, Default)]
struct PauseSamples {
    started_ms: Option<u64>,
    ended_ms: Option<u64>,
    first: Option<(u64, i64)>,
    last: Option<(u64, i64)>,
    range: Option<(i64, i64)>,
    reports: u64,
}

impl PauseSamples {
    fn begin(&mut self, now_ms: u64) {
        *self = Self { started_ms: Some(now_ms), ..Self::default() };
    }

    fn observe(&mut self, now_ms: u64, position: i64) {
        if self.ended_ms.is_some() || !self.started_ms.is_some_and(|at| now_ms > at) { return; }
        self.first.get_or_insert((now_ms, position));
        self.last = Some((now_ms, position));
        self.range = Some(match self.range {
            Some((low, high)) => (low.min(position), high.max(position)),
            None => (position, position),
        });
        self.reports = self.reports.saturating_add(1);
    }

    fn end_fresh(&self) -> bool {
        self.last.zip(self.ended_ms).is_some_and(|((at, _), end)| at == end)
    }

    fn span_ms(&self) -> Option<u64> {
        self.first.zip(self.last).map(|((from, _), (to, _))| to.saturating_sub(from))
    }

    fn grade(&self) -> &'static str {
        if self.range.is_some_and(|(low, high)| low != high) { return "moved"; }
        let covered = self.started_ms.zip(self.ended_ms).zip(self.span_ms()).is_some_and(|((start, end), span)| {
            let dwell = end.saturating_sub(start);
            dwell > 0 && span > 0 && span >= dwell.div_ceil(2)
        });
        if self.end_fresh() && covered { "froze" } else { "unconfirmed" }
    }

    fn delta(&self) -> String {
        self.first.zip(self.last).map_or_else(|| "-".to_owned(), |((_, from), (_, to))| (to - from).to_string())
    }
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
    /// The transport script: what each call came to, and what the events said.
    pause: CallResult,
    resume: CallResult,
    seek: CallResult,
    text: CallResult,
    audio: CallResult,
    subs: CallResult,
    paused_ev: bool,
    seek_done_ev: bool,
    seek_seen: Option<i64>,
    eos_ev: bool,
    pause_pos: Option<i64>,
    resume_pos: Option<i64>,
    pause_samples: PauseSamples,
    seek_target: Option<i64>,
    /// The diagnostic machine's fallback when no addressed dispatch context was supplied.
    track_pending: Option<&'static str>,
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
            pause: CallResult::NotSent,
            resume: CallResult::NotSent,
            seek: CallResult::NotSent,
            text: CallResult::NotSent,
            audio: CallResult::NotSent,
            subs: CallResult::NotSent,
            paused_ev: false,
            seek_done_ev: false,
            seek_seen: None,
            eos_ev: false,
            pause_pos: None,
            resume_pos: None,
            pause_samples: PauseSamples::default(),
            seek_target: None,
            track_pending: None,
        }
    }
}

/// The probe. State is private; the one writer is [`step`](UriProbe::step).
pub struct UriProbe {
    config: Config,
    phase: Phase,
    shape: Shape,
    started_ms: u64,
    media_id: Option<MediaId>,
    subscribed: bool,
    settled: Option<Settle>,
    last_time: Option<i64>,
    time_logged_ms: Option<u64>,
    /// When a `currentTime` the probe could not read was first logged; it is logged once.
    time_unparsed_ms: Option<u64>,
    placed: Option<(u32, u32)>,
    facts: Facts,
    acb_video: Option<VideoFacts>,
    acb_sequence: u64,
    foreground_sent: bool,
    requests: std::collections::BTreeMap<(Method, Option<&'static str>), plx_platform::tv::uri::RequestId>,
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
            time_unparsed_ms: None,
            placed: None,
            facts: Facts::default(),
            acb_video: None,
            acb_sequence: 0,
            foreground_sent: false,
            requests: std::collections::BTreeMap::new(),
        }
    }

    /// The session is over (everything it will say has been said).
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// From `playing` until the end: the service has said the video plays. A claim about the
    /// service, not an observation of the plane.
    pub fn plane_open(&self) -> bool {
        matches!(self.phase, Phase::Playing { .. } | Phase::Driving { .. })
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
            UriEvent::Closed if self.phase == Phase::AwaitingClose => {
                self.phase = Phase::Registering { deadline: now_ms.saturating_add(REGISTER_TIMEOUT_MS) };
                fx.push(UriEffect::Open(Shape::Anon));
            }
            UriEvent::Closed => {},
            UriEvent::AcbReleaseTail {..} => {},
            UriEvent::AcbAnswered {request, invoked, completed_ms} => self.acb_answered(request, invoked, completed_ms, fx),
            UriEvent::Dispatched { method, kind, request } => { self.requests.insert((method, kind), request); },
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
        if self.config.variant.acb() { self.acb_dispatch(AcbStep::Check, now_ms, fx); }
        else {
            self.phase = Phase::Registering { deadline: now_ms + REGISTER_TIMEOUT_MS };
            fx.push(UriEffect::Open(self.shape));
        }
    }

    /// An accepted call starts provisional. An explicit paused observation can confirm pause;
    /// track confirmation requires identified correlation. Generic seekDone and trackSelected
    /// notifications remain observations, not proof of a particular command.
    fn outcome(ok: bool, denied: bool) -> CallResult {
        if ok && !denied { CallResult::Unconfirmed } else { CallResult::Refused }
    }

    /// The tail of a call's log line: `ok`, or `err(<why>)`.
    fn said(ok: bool, denied: bool, text: &str) -> String {
        if ok && !denied { "ok".to_owned() } else { format!("err({text})") }
    }

    fn shape_label(shape: Shape) -> &'static str {
        match shape {
            Shape::Named => "named",
            Shape::Anon => "anon",
        }
    }

    fn tick(&mut self, now_ms: u64, fx: &mut Vec<UriEffect>) {
        match self.phase {
            Phase::AcbBinding {deadline, ..} if now_ms >= deadline => self.stop(now_ms, End::Timeout("acb"), fx),
            Phase::AcbWait {next, deadline} if now_ms >= deadline => self.acb_dispatch(next, now_ms, fx),
            Phase::AcbWaitingVideo {deadline} if now_ms >= deadline => self.stop(now_ms, End::Timeout("acb-video"), fx),
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
            Phase::Driving { step, deadline } if now_ms >= deadline => self.drive_step(step, now_ms, fx),
            _ => {}
        }
    }

    /// One step of the transport script. Each step sends its call, logs what it asked for (with the
    /// position it can see), and schedules the next; the last one ends the session, so a tester
    /// without a picture still learns whether the pipeline accepted the calls.
    fn drive_step(&mut self, step: usize, now_ms: u64, fx: &mut Vec<UriEffect>) {
        let Some(media_id) = self.media_id.clone() else {
            self.stop(now_ms, End::Script, fx);
            return;
        };
        let position = |p: Option<i64>| p.map_or_else(|| "-".to_owned(), |v| v.to_string());
        match step {
            0 => {
                self.facts.pause_samples.begin(now_ms);
                self.facts.pause_pos = self.last_time;
                fx.push(self.line(now_ms, &format!("drive pause pos={}", position(self.facts.pause_pos))));
                fx.push(UriEffect::Call(UriCall::Pause { media_id }));
            }
            1 => {
                self.facts.pause_samples.ended_ms = Some(now_ms);
                self.facts.resume_pos = self.last_time;
                self.facts.resume = CallResult::Unconfirmed;
                fx.push(self.line(now_ms, &format!("drive resume pos={}", position(self.facts.resume_pos))));
                fx.push(UriEffect::Call(UriCall::Play { media_id }));
            }
            2 => {
                let target = self.last_time.unwrap_or(0).max(0).saturating_add(DRIVE_SEEK_MS);
                self.facts.seek_target = Some(target);
                fx.push(self.line(now_ms, &format!("drive seek target={target}")));
                fx.push(UriEffect::Call(UriCall::Seek { media_id, position: target }));
            }
            3 => {
                self.facts.track_pending = Some("text");
                fx.push(self.line(now_ms, "drive selectTrack type=text index=0"));
                fx.push(UriEffect::Call(UriCall::SelectTrack { media_id, kind: "text", index: 0 }));
            }
            4 => {
                self.facts.track_pending = Some("audio");
                fx.push(self.line(now_ms, "drive selectTrack type=audio index=1"));
                fx.push(UriEffect::Call(UriCall::SelectTrack { media_id, kind: "audio", index: 1 }));
            }
            5 => {
                fx.push(self.line(now_ms, "drive subtitles enable"));
                fx.push(UriEffect::Call(UriCall::SetSubtitleEnable { media_id, enable: true }));
            }
            _ => {
                self.stop(now_ms, End::Script, fx);
                return;
            }
        }
        self.phase = Phase::Driving { step: step + 1, deadline: now_ms + DRIVE_STEP_MS };
    }

    fn reply(&mut self, method: Method, ok: bool, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if method == Method::Dropped {
            let count = message.dropped.unwrap_or(0);
            fx.push(self.line(now_ms, &format!("transport dropped {count} replies")));
            self.stop(now_ms, End::Refused("overflow"), fx);
            return;
        }
        match self.phase {
            Phase::Registering { .. } if method == Method::Register => {
                let shape = Self::shape_label(self.shape);
                if ok {
                    fx.push(self.line(now_ms, &format!("register shape={shape} ok")));
                    self.phase = Phase::Loading { deadline: now_ms + LOAD_TIMEOUT_MS };
                    let window_id = if self.config.variant.acb() { Some("window_id_dummy".to_owned()) }
                        else if self.config.variant.binds_at_load() { self.config.window_id.clone() } else { None };
                    fx.push(UriEffect::Call(UriCall::Load { window_id }));
                } else {
                    fx.push(self.line(now_ms, &format!("register shape={shape} refused({})", message.why())));
                    self.next_shape_or_stop(now_ms, End::Refused("register"), fx);
                }
            }
            Phase::Loading { .. } | Phase::SettingWindow { .. } | Phase::Starting { .. } | Phase::Playing { .. } | Phase::Driving { .. } | Phase::AcbBinding {..} | Phase::AcbWait {..} | Phase::AcbWaitingVideo {..} => {
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
            self.phase = Phase::AwaitingClose;
        } else {
            self.stop(now_ms, end, fx);
        }
    }

    fn service_message(&mut self, method: Method, ok: bool, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if self.requests.get(&(method, message.reply_track)).is_some_and(|request| message.reply_request != Some(*request)) { return; }
        if message.identity_conflict || (self.media_id.is_some() && message.media_id.is_some() && self.media_id != message.media_id) { return; }
        if let Some(video) = message.video_info {
            self.acb_video = Some(video);
            if matches!(self.phase, Phase::AcbWaitingVideo {..}) { self.acb_dispatch(AcbStep::MediaId, now_ms, fx); }
        }
        let denied = !ok || message.return_value == Some(false);
        let text = message.why();
        if let Some(kind) = message.confirmed_track {
            match kind { "text" => self.facts.text = CallResult::Ok, "audio" => self.facts.audio = CallResult::Ok, _ => {} }
        }
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
                if matches!(self.phase, Phase::Driving { .. }) {
                    // A refused resume is a finding, not the end of the session: the script still
                    // has the seek and the track calls to ask about.
                    self.facts.resume = CallResult::Refused;
                    return;
                }
                self.stop(now_ms, End::Refused("play"), fx);
                return;
            }
            Method::NotifyForeground => {
                fx.push(self.line(now_ms, &format!("call notifyForeground {}", Self::said(ok, denied, &text))));
                if denied { self.stop(now_ms, End::Refused("notifyForeground"), fx); }
                return;
            }
            Method::Pause => {
                self.facts.pause = Self::outcome(ok, denied);
                fx.push(self.line(now_ms, &format!("call pause {}", Self::said(ok, denied, &text))));
                return;
            }
            Method::Seek => {
                self.facts.seek = Self::outcome(ok, denied);
                fx.push(self.line(now_ms, &format!("call seek {}", Self::said(ok, denied, &text))));
                return;
            }
            Method::SelectTrack => {
                let kind = message.reply_track.or_else(|| self.facts.track_pending.take()).unwrap_or("track");
                let outcome = Self::outcome(ok, denied);
                match kind {
                    "text" => if self.facts.text != CallResult::Ok || outcome == CallResult::Refused { self.facts.text = outcome },
                    "audio" => if self.facts.audio != CallResult::Ok || outcome == CallResult::Refused { self.facts.audio = outcome },
                    _ => {},
                }
                fx.push(self.line(now_ms, &format!("call selectTrack {kind} {}", Self::said(ok, denied, &text))));
                return;
            }
            Method::SetSubtitleEnable => {
                // No event confirms this call: its own reply is the whole answer.
                self.facts.subs = if denied { CallResult::Refused } else if message.return_value == Some(true) { CallResult::Ok } else { CallResult::Unconfirmed };
                fx.push(self.line(now_ms, &format!("call setSubtitleEnable {}", Self::said(ok, denied, &text))));
                return;
            }
            Method::SetWindowId => {
                if !denied && message.return_value != Some(true) { return; }
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
                if method == Method::Play && matches!(self.phase, Phase::Driving { .. }) {
                    self.facts.resume = CallResult::Unconfirmed;
                }
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
            if self.config.variant.acb() && !self.foreground_sent {
                self.foreground_sent = true;
                fx.push(UriEffect::Call(UriCall::NotifyForeground {media_id: media_id.clone()}));
            }
            fx.push(UriEffect::Call(UriCall::Subscribe { media_id }));
        }
        self.events(message, now_ms, fx);
    }

    fn events(&mut self, message: &Message, now_ms: u64, fx: &mut Vec<UriEffect>) {
        for seen in &message.seen {
            if self.phase == Phase::Done {
                return;
            }
            if message.normalized.contains(&seen.ev) { self.event(seen, message, now_ms, fx); }
            else if seen.ev == Ev::CurrentTime { self.current_time(None, now_ms, fx); }
            else { fx.push(self.event_line(now_ms, seen.ev.name(), "(unparsed)")); }
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
                    self.phase = if self.config.transport {
                        Phase::Driving { step: 0, deadline: now_ms + DRIVE_FIRST_MS }
                    } else {
                        Phase::Playing { frozen_at: now_ms + FROZEN_TIME_MS }
                    };
                }
            }
            Ev::Paused => {
                fx.push(self.line(now_ms, "ev=paused"));
                self.facts.paused_ev = true;
                if self.facts.pause == CallResult::Unconfirmed {
                    self.facts.pause = CallResult::Ok;
                }
            }
            Ev::SeekDone => {
                fx.push(self.line(now_ms, "ev=seekDone"));
                self.facts.seek_done_ev = true;
                // The subscription's request ID is not the seek command's identity.
            }
            Ev::TrackSelected => {
                fx.push(self.line(now_ms, "ev=trackSelected"));
                // A generic subscription event cannot name a text or audio command.
                match message.confirmed_track {
                    Some("text") if self.facts.text == CallResult::Unconfirmed => self.facts.text = CallResult::Ok,
                    Some("audio") if self.facts.audio == CallResult::Unconfirmed => self.facts.audio = CallResult::Ok,
                    _ => {},
                }
            }
            Ev::EndOfStream => {
                fx.push(self.line(now_ms, "ev=endOfStream"));
                self.facts.eos_ev = true;
                if matches!(self.phase, Phase::Starting { .. } | Phase::Playing { .. } | Phase::Driving { .. } | Phase::AcbBinding {..} | Phase::AcbWait {..} | Phase::AcbWaitingVideo {..}) {
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
        if self.config.variant.acb() {
            // Start service transport independently: some firmware emits videoInfo only after Play.
            self.play(now_ms, fx);
            if self.acb_video.is_some() { self.acb_dispatch(AcbStep::MediaId, now_ms, fx); }
            else { self.phase = Phase::AcbWaitingVideo {deadline: now_ms.saturating_add(LOAD_TIMEOUT_MS)}; fx.push(self.line(now_ms, "acb waiting for real videoInfo dimensions/rate")); }
            return;
        }
        match (self.config.variant, self.config.window_id.clone(), self.media_id.clone()) {
            (Variant::Late, Some(window_id), Some(media_id)) => {
                self.facts.window_call = WindowCall::Pending;
                self.phase = Phase::SettingWindow { deadline: now_ms + LATE_ID_TIMEOUT_MS };
                fx.push(UriEffect::Call(UriCall::SetWindowId { media_id, window_id }));
            }
            _ => self.play(now_ms, fx),
        }
    }

    fn acb_dispatch(&mut self, step: AcbStep, now_ms: u64, fx: &mut Vec<UriEffect>) {
        let op = match step {
            AcbStep::Check => AcbOp::Check { dass: self.config.variant == Variant::AcbDass },
            AcbStep::MediaId => AcbOp::MediaId {media_id: self.media_id.clone().expect("loaded identity")},
            AcbStep::Loaded => AcbOp::Loaded,
            AcbStep::Connect => AcbOp::Connect {enabled: self.config.variant == Variant::AcbDass},
            AcbStep::VideoData => AcbOp::VideoData {media_id: self.media_id.clone().expect("loaded identity"), video: self.acb_video.expect("observed video facts")},
            AcbStep::Display => AcbOp::Display,
        };
        self.acb_sequence = self.acb_sequence.checked_add(1).expect("bounded ACB script");
        let request = AcbRequest(self.acb_sequence);
        self.phase = Phase::AcbBinding {request, step, deadline: now_ms.saturating_add(LATE_ID_TIMEOUT_MS)};
        fx.push(self.line(now_ms, &format!("acb request={} step={step:?}", self.acb_sequence)));
        fx.push(UriEffect::Acb {request, op});
    }

    fn acb_answered(&mut self, request: AcbRequest, invoked: bool, now_ms: u64, fx: &mut Vec<UriEffect>) {
        let Phase::AcbBinding {request: expected, step, deadline} = self.phase else { return };
        if expected != request { return; }
        if now_ms >= deadline { self.stop(now_ms, End::Timeout("acb"), fx); return; }
        fx.push(self.line(now_ms, &format!("acb answer step={step:?} invoked={invoked} (not plane proof)")));
        if !invoked { self.stop(now_ms, End::Refused("acb"), fx); return; }
        let next = match step {
            AcbStep::Check => {
                self.phase = Phase::Registering {deadline: now_ms.saturating_add(REGISTER_TIMEOUT_MS)};
                fx.push(UriEffect::Open(self.shape)); return;
            }
            AcbStep::MediaId => AcbStep::Loaded,
            AcbStep::Loaded => AcbStep::Connect,
            AcbStep::Connect => AcbStep::VideoData,
            AcbStep::VideoData => AcbStep::Display,
            AcbStep::Display => {
                self.phase = if self.facts.playing_ms.is_some() { Phase::Playing {frozen_at: now_ms.saturating_add(FROZEN_TIME_MS)} }
                    else { Phase::Starting {deadline: now_ms.saturating_add(PLAY_TIMEOUT_MS)} };
                return;
            }
        };
        self.phase = Phase::AcbWait {next, deadline: now_ms.saturating_add(if step == AcbStep::VideoData {300} else {200})};
    }

    fn play(&mut self, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if let Some(media_id) = self.media_id.clone() {
            self.phase = Phase::Starting { deadline: now_ms + PLAY_TIMEOUT_MS };
            fx.push(UriEffect::Call(UriCall::Play { media_id }));
        }
    }

    fn current_time(&mut self, value: Option<i64>, now_ms: u64, fx: &mut Vec<UriEffect>) {
        let Some(value) = value else {
            // Reported, but in neither known shape: say so once, with no payload. It is no report
            // of a position, so it moves nothing and arms no timeout.
            if self.time_unparsed_ms.is_none() {
                self.time_unparsed_ms = Some(now_ms);
                fx.push(self.line(now_ms, "ev=currentTime (unparsed)"));
            }
            return;
        };
        // Every valid report is fresh evidence, including an unchanged position.
        self.facts.pause_samples.observe(now_ms, value);
        let moved = self.last_time.is_some_and(|last| last != value);
        self.last_time = Some(value);
        if self.facts.seek_target.is_some() && self.facts.seek_seen.is_none() {
            self.facts.seek_seen = Some(value);
        }
        if moved {
            if matches!(self.phase, Phase::Playing { .. }) || (!self.config.transport && matches!(self.phase, Phase::Starting { .. })) {
                // Drive waits for the explicit playing event before scheduling its one script.
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
            fx.push(self.line(now_ms, "call unload requested"));
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
            Some(End::Script) => "script".to_owned(),
            Some(End::Timeout(phase)) => format!("timeout:{phase}"),
            Some(End::Refused(phase)) => format!("refused:{phase}"),
        };
        let summary = format!(
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
        );
        if !self.config.transport {
            return summary;
        }
        let call = |what: CallResult| match what {
            CallResult::NotSent => "none",
            CallResult::Ok => "ok",
            CallResult::Unconfirmed => "sent",
            CallResult::Refused => "refused",
        };
        let delta = |from: Option<i64>, to: Option<i64>| match (from, to) {
            (Some(a), Some(b)) => (b - a).to_string(),
            _ => "-".to_owned(),
        };
        // Pause labels grade reported positions only. Cached dispatch positions and an ACK or
        // paused notification alone never supply fresh end-of-dwell sampling evidence.
        let paused = if self.facts.pause == CallResult::Refused {
            "refused"
        } else if self.facts.pause_samples.started_ms.is_some() {
            self.facts.pause_samples.grade()
        } else {
            call(self.facts.pause)
        };
        let seek_landed = if self.facts.seek == CallResult::Refused {
            "refused"
        } else {
            match (self.facts.seek_target, self.facts.seek_seen) {
                (Some(target), Some(seen)) if (seen - target).abs() <= DRIVE_SEEK_TOLERANCE_MS => "hit",
                (Some(_), Some(_)) => "miss",
                _ => call(self.facts.seek),
            }
        };
        format!(
            "{summary} pause={paused} pause_delta={} resume={} seek={seek_landed} seek_delta={} text={} audio={} subs={} eos={} \
             pause_basis=reported_positions pause_samples={} pause_span_ms={} pause_end_fresh={} paused_event={}",
            self.facts.pause_samples.delta(),
            call(self.facts.resume),
            delta(self.facts.seek_target, self.facts.seek_seen),
            call(self.facts.text),
            call(self.facts.audio),
            call(self.facts.subs),
            if self.facts.eos_ev { "yes" } else { "no" },
            self.facts.pause_samples.reports,
            self.facts.pause_samples.span_ms().map_or_else(|| "-".to_owned(), |span| span.to_string()),
            if self.facts.pause_samples.end_fresh() { "yes" } else { "no" },
            if self.facts.paused_ev { "yes" } else { "no" },
        )
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::uri::ReplyQueue;

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
            transport: variant.drives(),
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
            self.feed(UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method, payload: payload.to_owned(), ok: true }))
        }
        fn deny(&mut self, method: &'static str, text: &str) -> Vec<UriEffect> {
            self.feed(UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method, payload: text.to_owned(), ok: false }))
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
    fn media(id: &str) -> MediaId {
        id.into()
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

        let fx = rig.after(60, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "load", payload: r#"{"returnValue":true,"mediaId":"_m1"}"#.into(), ok: true }));
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

        let fx = rig.after(300, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: r#"{"mediaId":"_m1","loadCompleted":true}"#.into(), ok: true }));
        assert_eq!(
            fx,
            [
                UriEffect::Log("uriprobe: t=400 ev=loadCompleted".into()),
                UriEffect::Settle(Settle::Started),
                call(UriCall::Play { media_id: media("_m1") }),
            ]
        );
        assert!(!rig.probe.plane_open());

        let fx = rig.after(100, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: r#"{"mediaId":"_m1","playing":true}"#.into(), ok: true }));
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
        let time = |rig: &mut Rig, ms: u64, value: i64| rig.after(ms, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: format!(r#"{{"currentTime":{value}}}"#), ok: true }));
        assert!(has_log(&time(&mut rig, 10, 0), "(first report)"));
        assert!(time(&mut rig, 10, 0).is_empty(), "the same value again says nothing");
        assert!(has_log(&time(&mut rig, 10, 40), "ev=currentTime value=40"), "first move");
        assert!(time(&mut rig, 400, 440).is_empty(), "inside the second");
        assert!(has_log(&time(&mut rig, 600, 1040), "ev=currentTime value=1040"), "a second on");
        assert!(summary_of(&rig.feed(UriEvent::Back)).contains("currentTime_advanced=yes"));
    }

    /// umediaserver documents the event as `{"currentTime":{"currentTime":n,"mediaId":s}}`; its
    /// client reads a bare number. The probe reads either.
    #[test]
    fn a_nested_current_time_is_read_like_the_bare_number() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        let time = |rig: &mut Rig, ms: u64, value: i64| {
            let payload = format!(r#"{{"currentTime":{{"currentTime":{value},"mediaId":"_m1"}}}}"#);
            rig.after(ms, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload, ok: true }))
        };
        assert!(has_log(&time(&mut rig, 10, 40), "ev=currentTime value=40 (first report)"));
        let fx = time(&mut rig, 600, 1040);
        assert!(has_log(&fx, "ev=currentTime value=1040") && !has_log(&fx, "(unparsed)"));
        assert!(summary_of(&rig.feed(UriEvent::Back)).contains("currentTime_advanced=yes"));
    }

    /// A `currentTime` the probe cannot read in either shape says so once, so a log can tell
    /// "never reported" from "reported in a shape we do not know"; it moves nothing and arms no
    /// timeout.
    #[test]
    fn a_current_time_in_no_known_shape_logs_one_line_and_arms_nothing() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        let odd = |rig: &mut Rig, ms: u64, payload: &str| rig.after(ms, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: payload.to_owned(), ok: true }));
        let fx = odd(&mut rig, 10, r#"{"currentTime":{"mediaId":"_m1"}}"#);
        assert_eq!(logs(&fx).len(), 1);
        assert!(has_log(&fx, "ev=currentTime (unparsed)"));
        assert!(!has_log(&fx, "_m1"), "no payload in the line");
        assert!(odd(&mut rig, 10, r#"{"currentTime":"soon"}"#).is_empty(), "a second one logs nothing");
        assert!(odd(&mut rig, 10, r#"{"currentTime":{"currentTime":{"currentTime":5}}}"#).is_empty());
        assert!(rig.tick(10 * FROZEN_TIME_MS).is_empty(), "it does not arm the frozen timeout");
        assert!(!rig.probe.is_done());
        // a readable report afterwards is still announced and still counts
        assert!(has_log(&odd(&mut rig, 10, r#"{"currentTime":0}"#), "(first report)"));
        assert!(has_log(&odd(&mut rig, 10, r#"{"currentTime":50}"#), "ev=currentTime value=50"));
        assert!(summary_of(&rig.feed(UriEvent::Back)).contains("currentTime_advanced=yes"));
    }

    // ------------------------------------------------------------------ the register ladder

    #[test]
    fn a_refused_named_registration_falls_to_the_anonymous_one_once() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        let fx = rig.deny("register", "code -1027: Invalid permissions for com.webos.media.client.plxprobe");
        assert_eq!(fx.len(), 2);
        assert!(has_log(&fx, "register shape=named refused(code <text> Invalid permissions for com.webos.media.client.plxprobe)"), "{fx:?}");
        assert_eq!(fx[1], UriEffect::Close);
        assert_eq!(rig.feed(UriEvent::Closed), [UriEffect::Open(Shape::Anon)]);
        assert_eq!(settles(&fx, Settle::StartFailed), 0, "the ladder is not an end");

        let fx = rig.msg("register", "");
        assert!(has_log(&fx, "register shape=anon ok") && fx[1] == load(Some(WINDOW)));

        // a second refusal anywhere ends it
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        rig.deny("register", "code -1: no");
        rig.feed(UriEvent::Closed);
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
        assert_eq!(fx.last(), Some(&UriEffect::Close));
        assert_eq!(rig.feed(UriEvent::Closed), [UriEffect::Open(Shape::Anon)]);
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
        assert_eq!(fx.last(), Some(&UriEffect::Close));
        assert_eq!(rig.feed(UriEvent::Closed), [UriEffect::Open(Shape::Anon)]);
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
        let time = |rig: &mut Rig, ms: u64, value: i64| rig.after(ms, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: format!(r#"{{"currentTime":{value}}}"#), ok: true }));
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

    #[test]
    fn a_frozen_nested_current_time_ends_the_session_like_a_bare_one() {
        let time = |rig: &mut Rig, ms: u64, value: i64| {
            let payload = format!(r#"{{"currentTime":{{"currentTime":{value},"mediaId":"_m1"}}}}"#);
            rig.after(ms, UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload, ok: true }))
        };
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        time(&mut rig, 10, 0);
        time(&mut rig, 10, 50);
        assert!(rig.tick(FROZEN_TIME_MS - 1).is_empty());
        let fx = rig.tick(1);
        assert!(summary_of(&fx).contains("currentTime_advanced=yes") && summary_of(&fx).contains("end=timeout:currentTime"));
        assert_eq!((settles(&fx, Settle::StartFailed), unloads(&fx), closes(&fx)), (0, 1, 1));
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
                UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: r#"{"error":{"errorCode":-1}}"#.into(), ok: true }),
                UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: r#"{"loadCompleted":true,"mediaId":"_m1"}"#.into(), ok: true }),
                UriEvent::from_reply(&UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "unload", payload: r#"{"returnValue":true}"#.into(), ok: true }),
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
        // The `dropped` reply exactly as the transport's own queue makes it (`ok: false`), by
        // overflowing a real one: three pushes into a queue of one lose two.
        let queue = ReplyQueue::new(plx_platform::tv::uri::SessionId::new(1), 1);
        for _ in 0..3 {
            queue.push(plx_platform::tv::uri::RequestId::new(1), "subscribe", r#"{"loadCompleted":true,"mediaId":"_m1"}"#.to_owned(), true);
        }
        let replies = queue.drain();
        let marker = replies.last().expect("the drain ends with the dropped reply");
        assert_eq!((marker.method, marker.ok), ("dropped", false), "the shape the queue sends");
        let fx: Vec<UriEffect> = replies.iter().flat_map(|reply| rig.feed(UriEvent::from_reply(reply))).collect();
        assert_eq!(logs(&fx)[0], "uriprobe: t=0 transport dropped 2 replies");
        assert!(rig.probe.is_done(), "loss invalidates the diagnostic stream");
        assert_eq!(closes(&fx), 1);
    }

    #[test]
    fn an_unknown_event_is_named_and_nothing_else() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let fx = rig.msg("subscribe", &format!(r#"{{"mediaId":"_m1","fooBar":{{"uri":"{URL}"}}}}"#));
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=unknown (unparsed)"]);
        let fx = rig.msg("subscribe", &format!(r#"{{"mediaId":"_m1","{URL}":1}}"#));
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=unknown (unparsed)"]);
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
    fn parse_event_reads_current_time_bare_or_nested_and_nothing_else_of_the_object() {
        let time = |payload: &str| parse_event(payload).current_time;
        assert_eq!(time(r#"{"currentTime":40}"#), Some(40));
        assert_eq!(time(r#"{"currentTime":40.9}"#), Some(40));
        assert_eq!(time(r#"{"currentTime":{"currentTime":40,"mediaId":"_m1"}}"#), Some(40));
        assert_eq!(time(r#"{"currentTime":{"currentTime":40.9}}"#), Some(40));
        for unreadable in [r#"{"currentTime":{"mediaId":"_m1"}}"#, r#"{"currentTime":{"currentTime":{"currentTime":5}}}"#, r#"{"currentTime":"40"}"#] {
            let message = parse_event(unreadable);
            assert_eq!(message.current_time, None, "{unreadable}");
            assert!(message.seen.iter().any(|seen| seen.ev == Ev::CurrentTime), "still an event: {unreadable}");
        }
        // The nested identity belongs to this event and survives opaque, without being logged.
        let message = parse_event(r#"{"currentTime":{"currentTime":40,"mediaId":"_m9"}}"#);
        assert_eq!(message.media_id.as_deref(), Some("_m9"));
        assert_clean("Message", &format!("{message:?}"));
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
        all.extend(rig.feed(UriEvent::Closed));
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
        let reply = UriReply { session: plx_platform::tv::uri::SessionId::new(1), request: plx_platform::tv::uri::RequestId::new(1), method: "subscribe", payload: hostile_event(), ok: true };
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
        for (text, want) in [
            ("", Some(Variant::AtLoad)),
            ("\n", Some(Variant::AtLoad)),
            ("load\n", Some(Variant::AtLoad)),
            (" Late ", Some(Variant::Late)),
            ("none", Some(Variant::NoWindow)),
            ("drive", Some(Variant::Drive)),
            (" DRIVE ", Some(Variant::Drive)),
            ("lates", None),
            ("http://x", None),
        ] {
            assert_eq!(Variant::from_trigger(text), want, "{text:?}");
        }
    }

    #[test]
    fn drive_binds_like_load_then_pauses_resumes_seeks_and_selects_tracks() {
        let mut rig = Rig::new(Variant::Drive, Some(WINDOW));
        rig.start();
        let fx = rig.msg("register", "");
        assert_eq!(fx[1], load(Some(WINDOW)), "drive binds at load, like the at-load variant");

        rig.loaded();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","playing":true}"#);
        assert!(!rig.probe.is_done(), "the session ended on playing: {fx:?}");
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","currentTime":{"currentTime":1000}}"#);
        assert!(has_log(&fx, "currentTime"), "the position did not arrive: {fx:?}");

        // Pause: the position at the call, and the call itself.
        let fx = rig.tick(3_000);
        assert!(has_log(&fx, "drive pause pos=1000"), "{fx:?}");
        assert!(fx.contains(&call(UriCall::Pause { media_id: media("_m1") })));
        let fx = rig.msg("pause", r#"{"returnValue":true}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=3000 call pause ok"]);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","paused":true}"#);
        assert!(has_log(&fx, "ev=paused"), "{fx:?}");

        // Resume: the same position, a second play, and the position moving again.
        let fx = rig.tick(2_500);
        assert!(has_log(&fx, "drive resume pos=1000"), "{fx:?}");
        assert!(fx.contains(&call(UriCall::Play { media_id: media("_m1") })));
        let fx = rig.msg("play", r#"{"returnValue":true}"#);
        assert!(has_log(&fx, "call play ok"), "{fx:?}");
        rig.msg("subscribe", r#"{"mediaId":"_m1","currentTime":{"currentTime":1500}}"#);

        // Seek: 20 s past where it is, then where the first position after it landed.
        let fx = rig.tick(2_500);
        assert!(has_log(&fx, "drive seek target=21500"), "{fx:?}");
        assert!(fx.contains(&call(UriCall::Seek { media_id: media("_m1"), position: 21_500 })));
        rig.msg("seek", r#"{"returnValue":true}"#);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","seekDone":true}"#);
        assert!(has_log(&fx, "ev=seekDone"), "{fx:?}");
        rig.msg("subscribe", r#"{"mediaId":"_m1","currentTime":{"currentTime":21550}}"#);

        // A text track, then an audio track, then the subtitle switch.
        let fx = rig.tick(2_500);
        assert!(has_log(&fx, "drive selectTrack type=text index=0"), "{fx:?}");
        assert!(fx.contains(&call(UriCall::SelectTrack { media_id: media("_m1"), kind: "text", index: 0 })));
        rig.msg("selectTrack", r#"{"returnValue":true}"#);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":true}"#);
        assert!(has_log(&fx, "ev=trackSelected"), "{fx:?}");

        let fx = rig.tick(2_500);
        assert!(has_log(&fx, "drive selectTrack type=audio index=1"), "{fx:?}");
        rig.msg("selectTrack", r#"{"returnValue":true}"#);
        rig.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":true}"#);

        let fx = rig.tick(2_500);
        assert!(has_log(&fx, "drive subtitles enable"), "{fx:?}");
        rig.msg("setSubtitleEnable", r#"{"returnValue":true}"#);

        // The last step ends the session, and the summary carries what each call came to.
        let fx = rig.tick(2_500);
        let summary = summary_of(&fx);
        assert!(summary.contains("variant=drive"), "{summary}");
        assert!(summary.contains("end=script"), "{summary}");
        // The paused event is independent evidence; this script supplied no clock reports
        // during the dwell, so the reported-position grade remains unconfirmed.
        assert!(summary.contains("pause=unconfirmed pause_delta=- resume=sent"), "{summary}");
        assert!(summary.contains("seek=hit seek_delta=50"), "{summary}");
        assert!(summary.contains("text=sent audio=sent subs=ok eos=no"), "{summary}");
        assert_eq!(unloads(&fx), 1);
        assert!(rig.probe.is_done());
    }

    #[test]
    fn a_refused_pause_is_reported_and_the_script_carries_on() {
        let mut rig = Rig::new(Variant::Drive, Some(WINDOW));
        rig.playing();
        rig.msg("subscribe", r#"{"mediaId":"_m1","currentTime":{"currentTime":2000}}"#);
        rig.tick(3_000);
        let fx = rig.msg("pause", r#"{"returnValue":false,"errorCode":-1,"errorText":"paused is not supported"}"#);
        assert!(has_log(&fx, "call pause err("), "{fx:?}");
        let fx = rig.tick(2_500);
        assert!(has_log(&fx, "drive resume pos=2000"), "the script continues: {fx:?}");
        let fx = rig.feed(UriEvent::Back);
        let summary = summary_of(&fx);
        assert!(summary.contains("pause=refused"), "{summary}");
        assert!(summary.contains("end=back"), "{summary}");
    }

    #[test]
    fn a_drive_session_that_reaches_the_end_of_the_stream_reports_it() {
        let mut rig = Rig::new(Variant::Drive, Some(WINDOW));
        rig.playing();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","endOfStream":true}"#);
        assert!(has_log(&fx, "ev=endOfStream"), "{fx:?}");
        assert!(summary_of(&fx).contains("eos=yes"), "{}", summary_of(&fx));
        assert!(summary_of(&fx).contains("end=eos"), "{}", summary_of(&fx));
    }

    #[test]
    fn the_scheme_is_read_from_the_front_of_the_url_only() {
        assert_eq!(Scheme::of(URL), Scheme::Https);
        assert_eq!(Scheme::of("HTTPS://h/x"), Scheme::Https);
        assert_eq!(Scheme::of("http://h/https://x"), Scheme::Http);
        assert_eq!(Scheme::of(""), Scheme::Http);
        assert_eq!(Scheme::of("héllo"), Scheme::Http, "a multi-byte head is not a panic");
    }
    #[test]
    fn uri_regression_unrelated_strings_cannot_end_or_start_playback() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        let fx = rig.msg("subscribe", r#"{"description":"endOfStream","playing":false,"endOfStream":null}"#);
        assert!(!rig.probe.is_done(), "unrelated string ended the probe: {fx:?}");
        let message = parse_event(r#"{"description":"playing","playing":false}"#);
        assert!(!message.seen.iter().any(|seen| seen.ev == Ev::Playing));
    }

    #[test]
    fn uri_regression_generic_trackselected_cannot_confirm_both_tracks() {
        let mut rig = Rig::new(Variant::Drive, Some(WINDOW));
        rig.playing();
        rig.tick(3_000); rig.tick(2_500); rig.tick(2_500); rig.tick(2_500);
        rig.msg("selectTrack", r#"{"returnValue":true}"#);
        rig.tick(2_500);
        rig.msg("selectTrack", r#"{"returnValue":true}"#);
        rig.msg("subscribe", r#"{"trackSelected":true}"#);
        assert_eq!(rig.probe.facts.text, CallResult::Unconfirmed);
        assert_eq!(rig.probe.facts.audio, CallResult::Unconfirmed);
    }

    #[test]
    fn uri_regression_named_retry_waits_for_release_evidence() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.start();
        let fx = rig.deny("register", "denied");
        assert!(fx.contains(&UriEffect::Close));
        assert!(!fx.contains(&UriEffect::Open(Shape::Anon)), "retry raced unregister: {fx:?}");
    }

    #[test]
    fn uri_retry_deadline_begins_after_close_ack() {
        let mut rig=Rig::new(Variant::AtLoad,Some(WINDOW)); rig.start();
        rig.deny("register","denied");
        assert!(rig.tick(100_000).is_empty()); assert!(!rig.probe.is_done());
        assert_eq!(rig.feed(UriEvent::Closed),[UriEffect::Open(Shape::Anon)]);
        assert!(rig.tick(REGISTER_TIMEOUT_MS-1).is_empty());
        let fx=rig.tick(1); assert!(rig.probe.is_done()); assert!(summary_of(&fx).contains("timeout:register"));
    }

    #[test]
    fn uri_acb_experiment_variants_are_explicit_opt_in() {
        for arm in ["acb-control", "acb-dass"] {
            assert!(Variant::from_trigger(arm).is_some(), "missing controlled arm {arm}");
        }
        assert_eq!(Variant::from_trigger(""), Some(Variant::AtLoad));
        assert!(Variant::from_trigger("acb").is_none());
    }

    fn acb_request(fx: &[UriEffect]) -> AcbRequest {
        fx.iter().find_map(|fx| match fx {UriEffect::Acb {request,..} => Some(*request), _ => None}).expect("ACB effect")
    }
    fn acb_loaded(variant: Variant, early_video: bool) -> (Rig, Vec<UriEffect>) {
        let mut rig=Rig::new(variant,None);
        let fx=rig.start(); let request=acb_request(&fx);
        let fx=rig.feed(UriEvent::AcbAnswered {request,invoked:true,completed_ms:rig.now});
        assert!(fx.contains(&UriEffect::Open(Shape::Named)));
        let fx=rig.msg("register","");
        assert!(fx.contains(&load(Some("window_id_dummy"))));
        let fx=rig.msg("load",r#"{"returnValue":true,"mediaId":"_m1"}"#);
        let calls:Vec<_>=fx.iter().filter_map(|fx| match fx {UriEffect::Call(call)=>Some(call.method()),_=>None}).collect();
        assert_eq!(calls,["notifyForeground","subscribe"]);
        if early_video { rig.msg("subscribe",r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"frameRate":24}}"#); }
        let fx=rig.msg("subscribe",r#"{"mediaId":"_m1","loadCompleted":true}"#);
        assert_eq!(fx.iter().filter(|fx| matches!(fx,UriEffect::Call(UriCall::Play{..}))).count(),1);
        (rig,fx)
    }
    #[test]
    fn uri_acb_pair_has_same_real_delays_and_only_connect_invocation_differs() {
        let run=|variant| {
            let (mut rig,mut fx)=acb_loaded(variant,true);
            let mut trace=Vec::new();
            for step in 0..5 {
                let request=acb_request(&fx);
                let op=fx.iter().find_map(|fx| match fx {UriEffect::Acb{op,..}=>Some(op.clone()),_=>None}).unwrap();
                trace.push((rig.now,op));
                fx=rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms:rig.now});
                if step<4 {
                    let delay=if step==3 {300} else {200};
                    assert!(rig.tick(delay-1).is_empty()); fx=rig.tick(1);
                }
            }
            assert!(!fx.iter().any(|fx| matches!(fx,UriEffect::Call(UriCall::Play{..}))),"no second service Play");
            trace
        };
        let control=run(Variant::AcbControl); let dass=run(Variant::AcbDass);
        assert_eq!(control.iter().map(|(t,_)|*t-control[0].0).collect::<Vec<_>>(),[0,200,400,600,900]);
        for (i,(a,b)) in control.iter().zip(&dass).enumerate() {
            if i==2 { assert!(matches!(a.1,AcbOp::Connect{enabled:false})); assert!(matches!(b.1,AcbOp::Connect{enabled:true})); }
            else { assert_eq!(a,b); }
        }
    }
    #[test]
    fn uri_acb_video_after_play_and_early_playing_do_not_erase_binding_waits() {
        let (mut rig,fx)=acb_loaded(Variant::AcbDass,false);
        assert!(!fx.iter().any(|fx| matches!(fx,UriEffect::Acb{..})));
        rig.msg("subscribe",r#"{"mediaId":"_m1","playing":true}"#);
        rig.msg("subscribe",r#"{"mediaId":"_m1","currentTime":100}"#);
        assert!(matches!(rig.probe.phase,Phase::AcbWaitingVideo{..}));
        let mut fx=rig.msg("subscribe",r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"frameRate":24}}"#);
        for step in 0..5 {
            let request=acb_request(&fx);
            fx=rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms:rig.now});
            if step<4 { fx=rig.tick(if step==3 {300} else {200}); }
        }
        assert!(matches!(rig.probe.phase,Phase::Playing{..}),"retains first playing, needs no second event");
    }
    #[test]
    fn uri_acb_late_frame_preserves_delay_and_stop_cancels_every_stage() {
        for at in 0..5 {
            let (mut rig,mut fx)=acb_loaded(Variant::AcbDass,true);
            for step in 0..at {
                let request=acb_request(&fx); rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms:rig.now});
                fx=rig.tick(if step==3 {300} else {200});
            }
            let request=acb_request(&fx);
            let stop=rig.feed(UriEvent::Back); assert_eq!(closes(&stop),1);
            assert!(rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms:rig.now}).is_empty());
            assert!(rig.tick(100_000).is_empty());
            assert!(rig.msg("subscribe",r#"{"videoInfo":{"width":1280,"height":720,"frameRate":24}}"#).is_empty());
            assert!(rig.feed(UriEvent::Back).is_empty());
        }
        let (mut rig,fx)=acb_loaded(Variant::AcbDass,true);
        rig.feed(UriEvent::AcbAnswered{request:acb_request(&fx),invoked:true,completed_ms:rig.now});
        let fx=rig.tick(2_000); let request=acb_request(&fx);
        assert_eq!(fx.iter().filter(|fx| matches!(fx,UriEffect::Acb{..})).count(),1);
        rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms:rig.now});
        assert!(rig.tick(199).is_empty()); assert!(rig.tick(1).iter().any(|fx| matches!(fx,UriEffect::Acb{op:AcbOp::Connect{..},..})));
    }
    #[test]
    fn uri_acb_missing_video_eos_error_and_timeout_stop_without_binding() {
        for end in ["eos","error","timeout","back"] {
            let (mut rig,_)=acb_loaded(Variant::AcbControl,false);
            let fx=match end {
                "eos"=>rig.msg("subscribe",r#"{"mediaId":"_m1","endOfStream":true}"#),
                "error"=>rig.msg("subscribe",r#"{"mediaId":"_m1","errorCode":104}"#),
                "timeout"=>rig.tick(LOAD_TIMEOUT_MS),
                _=>rig.feed(UriEvent::Back),
            };
            assert!(rig.probe.is_done(),"{end}"); assert_eq!(closes(&fx),1);
            assert!(!fx.iter().any(|fx| matches!(fx,UriEffect::Acb{..})));
        }
    }

    #[test]
    fn uri_acb_delays_anchor_to_call_completion_and_late_answers_stop() {
        let (mut rig,fx)=acb_loaded(Variant::AcbDass,true);
        let request=acb_request(&fx); let completed_ms=rig.now+50;
        rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms});
        assert!(rig.tick(249).is_empty());
        assert!(rig.tick(1).iter().any(|fx|matches!(fx,UriEffect::Acb{op:AcbOp::Loaded,..})));
        let (mut rig,fx)=acb_loaded(Variant::AcbDass,true);
        let request=acb_request(&fx); let completed_ms=rig.now+LATE_ID_TIMEOUT_MS;
        let fx=rig.feed(UriEvent::AcbAnswered{request,invoked:true,completed_ms});
        assert!(rig.probe.is_done()); assert_eq!(closes(&fx),1);
        assert!(!fx.iter().any(|fx|matches!(fx,UriEffect::Acb{..})));
    }

    #[test]
    fn uri_acb_review_wrong_nested_video_cannot_advance_descriptor_wait() {
        for payload in [
            r#"{"videoInfo":{"mediaId":"other","width":1280,"height":720,"frameRate":24}}"#,
            r#"{"mediaId":"_m1","videoInfo":{"mediaId":"other","width":1280,"height":720,"frameRate":24}}"#,
            r#"{"mediaId":"_m1","videoInfo":{"mediaId":42,"width":1280,"height":720,"frameRate":24}}"#,
        ] {
            let (mut rig,_)=acb_loaded(Variant::AcbDass,false);
            let fx=rig.msg("subscribe",payload);
            assert!(matches!(rig.probe.phase,Phase::AcbWaitingVideo{..}),"foreign video advanced binding: {fx:?}");
            assert!(!fx.iter().any(|fx|matches!(fx,UriEffect::Acb{..})));
        }
    }

    fn drive_pause_rig() -> Rig {
        let mut rig=Rig::new(Variant::Drive,Some(WINDOW));
        rig.playing();
        rig.msg("subscribe",r#"{"mediaId":"_m1","currentTime":1000}"#);
        let fx=rig.tick(DRIVE_FIRST_MS);
        assert!(fx.iter().any(|fx|matches!(fx,UriEffect::Call(UriCall::Pause{..}))));
        rig.msg("pause",r#"{"returnValue":true}"#);
        rig
    }
    #[test]
    fn uri_drive_review_ignored_pause_reports_movement_instead_of_freeze() {
        let mut rig=drive_pause_rig();
        rig.after(250,UriEvent::from_reply(&UriReply{session:plx_platform::tv::uri::SessionId::new(1),request:plx_platform::tv::uri::RequestId::new(1),method:"subscribe",payload:r#"{"mediaId":"_m1","currentTime":1250}"#.into(),ok:true}));
        rig.after(2_250,UriEvent::from_reply(&UriReply{session:plx_platform::tv::uri::SessionId::new(1),request:plx_platform::tv::uri::RequestId::new(1),method:"subscribe",payload:r#"{"mediaId":"_m1","currentTime":3500}"#.into(),ok:true}));
        rig.tick(0);
        let fx=rig.feed(UriEvent::Back);
        assert!(summary_of(&fx).contains("pause=moved"),"ignored Pause graded as freeze: {}",summary_of(&fx));
    }
    #[test]
    fn uri_drive_review_no_pause_reports_are_unconfirmed() {
        let mut rig=drive_pause_rig();
        rig.msg("subscribe",r#"{"mediaId":"_m1","paused":true}"#);
        rig.tick(DRIVE_STEP_MS);
        let fx=rig.feed(UriEvent::Back);
        assert!(summary_of(&fx).contains("pause=unconfirmed"),"cached position graded as freeze: {}",summary_of(&fx));
    }
    #[test]
    fn uri_drive_review_early_reports_then_silence_do_not_confirm_pause() {
        let mut rig=drive_pause_rig();
        for _ in 0..2 {
            rig.now+=100;
            rig.msg("subscribe",r#"{"mediaId":"_m1","currentTime":1000}"#);
        }
        rig.tick(DRIVE_STEP_MS-200);
        let fx=rig.feed(UriEvent::Back);
        assert!(summary_of(&fx).contains("pause=unconfirmed"),"missing end observation reused: {}",summary_of(&fx));
    }
    #[test]
    fn uri_drive_review_times_before_playing_start_transport_script_exactly_once() {
        let mut rig=Rig::new(Variant::Drive,Some(WINDOW)); rig.loaded();
        rig.msg("subscribe",r#"{"mediaId":"_m1","currentTime":1000}"#);
        rig.now+=100;
        rig.msg("subscribe",r#"{"mediaId":"_m1","currentTime":1100}"#);
        rig.msg("subscribe",r#"{"mediaId":"_m1","playing":true}"#);
        let fx=rig.tick(DRIVE_FIRST_MS);
        assert_eq!(fx.iter().filter(|fx|matches!(fx,UriEffect::Call(UriCall::Pause{..}))).count(),1,"times bypassed Drive startup: {fx:?}");
        let mut all=fx;
        for _ in 0..6 {
            all.extend(rig.msg("subscribe",r#"{"mediaId":"_m1","playing":true}"#));
            all.extend(rig.tick(DRIVE_STEP_MS));
        }
        assert!(rig.probe.is_done(),"script must end despite repeated playing events");
        assert_eq!(all.iter().filter(|fx|matches!(fx,UriEffect::Call(UriCall::Pause{..}))).count(),1);
        assert!(summary_of(&all).contains("end=script"));
    }

    fn drive_report_after(rig: &mut Rig, after_ms: u64, position: i64) {
        rig.now+=after_ms;
        rig.msg("subscribe",&format!(r#"{{"mediaId":"_m1","currentTime":{position}}}"#));
    }
    #[test]
    fn uri_drive_pause_equal_fresh_reports_with_full_coverage_grade_reported_freeze() {
        let mut rig=drive_pause_rig();
        drive_report_after(&mut rig,250,1000);
        drive_report_after(&mut rig,2_250,1000);
        rig.tick(0);
        let fx=rig.feed(UriEvent::Back); let summary=summary_of(&fx);
        assert!(summary.contains("pause=froze pause_delta=0"),"{summary}");
        assert!(summary.contains("pause_basis=reported_positions pause_samples=2 pause_span_ms=2250 pause_end_fresh=yes"),"{summary}");
    }
    #[test]
    fn uri_drive_pause_insufficient_coverage_or_single_frame_is_unconfirmed() {
        for (first_after,second_after) in [(2_400,100),(2_500,0)] {
            let mut rig=drive_pause_rig();
            drive_report_after(&mut rig,first_after,1000);
            drive_report_after(&mut rig,second_after,1000);
            rig.tick(0);
            let fx=rig.feed(UriEvent::Back); let summary=summary_of(&fx);
            assert!(summary.contains("pause=unconfirmed"),"{summary}");
            assert!(summary.contains("pause_end_fresh=yes"),"end freshness alone is insufficient: {summary}");
        }
        let mut rig=drive_pause_rig(); drive_report_after(&mut rig,DRIVE_STEP_MS,1000); rig.tick(0);
        let fx=rig.feed(UriEvent::Back); assert!(summary_of(&fx).contains("pause=unconfirmed"));
    }
    #[test]
    fn uri_drive_pause_equality_criterion_has_no_guessed_jitter_allowance() {
        let mut rig=drive_pause_rig();
        drive_report_after(&mut rig,250,1000);
        drive_report_after(&mut rig,2_250,1001);
        rig.tick(0);
        let fx=rig.feed(UriEvent::Back);
        assert!(summary_of(&fx).contains("pause=moved pause_delta=1"),"{}",summary_of(&fx));
    }
    #[test]
    fn uri_drive_position_reports_without_playing_do_not_start_script_or_hide_play_timeout() {
        let mut rig=Rig::new(Variant::Drive,Some(WINDOW)); rig.loaded();
        drive_report_after(&mut rig,0,1000);
        drive_report_after(&mut rig,5_000,6000);
        let fx=rig.tick(DRIVE_FIRST_MS);
        assert!(!fx.iter().any(|fx|matches!(fx,UriEffect::Call(UriCall::Pause{..}))));
        let fx=rig.tick(PLAY_TIMEOUT_MS);
        assert!(rig.probe.is_done()); assert!(summary_of(&fx).contains("end=timeout:play"));
    }

}
