#![deny(clippy::wildcard_enum_match_arm)]
//! The URI-mode probe machine (issue #1064, the first test): what to say to `com.webos.media`, in
//! what order, and what each answer means, as a tested `State + Event -> Effects` machine in the
//! shape of `plx_media::player::lifecycle::ForegroundLifecycle` (not linked: that crate is above this one).
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
//! separate ownership reducer retains the registration/window until confirmed release. The named→anon
//! retry is the product driver's ladder (`register_ladder`, the one decision both call): it waits in
//! `AwaitingClose`, then arms the new register deadline after `Closed`.
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
/// The longest firmware release or build the `armed` line prints, in characters (a longer one is
/// not a version and prints as `-`).
const FIRMWARE_WORD_CHARS: usize = 32;
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

/// Everything the trigger file asks for: the [`Variant`] (empty or one word) and, as `key=N` words
/// in any order, the native adaptive-streaming rates in bit/s which the `load` payload carries
/// (`bpsstart`, `bpsmax`, `bpsmin`; each at most once, from 1 to [`MAX_ABR_BPS`]). `load
/// bpsmax=1500000` and `bpsmax=1500000` (the default variant) both arm it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub variant: Variant,
    pub bps: AbrBps,
}

impl Trigger {
    /// The arming a trigger file's text names; `None` for text that names none (an unknown word,
    /// a second word, an unknown or repeated key, a rate that is not a number from 1 to the
    /// bound), so the driver can say so instead of quietly arming something else.
    pub fn from_trigger(content: &str) -> Option<Trigger> {
        let mut bps = AbrBps::default();
        let mut word = None;
        for token in content.split_whitespace() {
            let Some((key, rate)) = token.split_once('=') else {
                if word.replace(token).is_some() {
                    return None;
                }
                continue;
            };
            let slot = match key.to_ascii_lowercase().as_str() {
                "bpsstart" => &mut bps.start,
                "bpsmax" => &mut bps.maximum,
                "bpsmin" => &mut bps.minimum,
                _ => return None,
            };
            if slot.replace(AbrBps::parse_rate(rate)?).is_some() {
                return None;
            }
        }
        Some(Trigger { variant: Variant::from_trigger(word.unwrap_or(""))?, bps })
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

/// A firmware release or build as the `armed` line prints it: the platform's own text when it is
/// plain version text (ASCII letters, digits and `.` `-` `_` `+`, at most [`FIRMWARE_WORD_CHARS`]),
/// `-` when it is empty or anything else. Nothing else the platform's file held reaches a log a
/// tester posts.
fn firmware_word(text: &str) -> &str {
    let plain = !text.is_empty()
        && text.len() <= FIRMWARE_WORD_CHARS
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'));
    if plain { text } else { "-" }
}

/// What kind of play the route chose (logged, nothing of the URL).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Direct,
    Hls,
}

/// The firmware the probe runs on, as the platform reported it at boot (`tv::device::Info`): the
/// release (`5.4.2`) and the exact build (`5.4.2-47`), each empty when unknown. The IO side reads
/// them and hands them in; the machine reads no global and only prints them on the `armed` line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Firmware {
    pub release: String,
    pub build: String,
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
    /// The set's firmware, for the `armed` line (a tester's log says which release it is).
    pub firmware: Firmware,
    /// Run the transport script after `playing` ([`Variant::Drive`]).
    pub transport: bool,
    /// The native adaptive-streaming rates the `load` payload carries (usually none).
    pub bps: AbrBps,
}

// ---------------------------------------------------------------------------------------------
// Payloads. The only place a URL is spelled into text.

pub use super::uri_protocol::{build_load, parse_event, scrub_text, AbrBps, MediaId, Message, Method, UriCall, MAX_ABR_BPS};
use super::uri_protocol::{Ev, Seen};
// What each event reads as is the shared fn's; `currentTime` is logged when it first moves and
// then at most every `CLOCK_LOG_EVERY_MS`.
use super::event_text::{event_text, unparsed_text, CLOCK_LOG_EVERY_MS};
#[cfg(test)]
use super::uri_protocol::SCRUBBED_TEXT_CHARS;

// ---------------------------------------------------------------------------------------------
// Events in

// The ACB steps are the shared bind sequence's (`acb_bind`); this machine only reports them.
pub use super::acb_bind::{AcbRequest, Op as AcbOp};
use super::acb_bind::{AcbBind, Event as AcbEvent, Failure as AcbFailure, Fx as AcbFx};
use super::register_ladder::{self, Outcome, Step};

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
    /// The shared ACB sequence ([`AcbBind`]) owns its own steps, paces and deadlines.
    Acb,
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
    fn name(&self) -> &'static str {
        match self {
            Phase::Idle | Phase::Registering { .. } | Phase::AwaitingClose => "register",
            Phase::Acb => "acb",
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

/// How often the service reports `currentTime`: the gap between each report and the one before it,
/// counted by value, so the median is exact in space bounded by the distinct gaps and not by the
/// length of the session. Time is the driver's frame clock: a gap is a whole number of frames.
#[derive(Debug, Default)]
struct ReportClock {
    last_ms: Option<u64>,
    gaps: std::collections::BTreeMap<u64, u64>,
}

impl ReportClock {


    /// The median gap; for an even count the smaller of the two middle gaps, so the answer is a
    /// gap that happened. `None` before the second report.
    fn median_ms(&self) -> Option<u64> {
        let total: u64 = self.gaps.values().sum();
        let rank = total.checked_sub(1)? / 2;
        let mut below = 0;
        self.gaps.iter().find_map(|(gap, count)| {
            below += count;
            (below > rank).then_some(*gap)
        })
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
    /// A `seekDone` arrived after the script's Seek call was sent (one from before it is not that
    /// call's).
    seek_done_ev: bool,
    seek_seen: Option<i64>,
    eos_ev: bool,
    pause_pos: Option<i64>,
    resume_pos: Option<i64>,
    /// The Resume call has been sent, and the first position the service reported after it.
    resume_sent: bool,
    resume_first: Option<i64>,
    pause_samples: PauseSamples,
    seek_target: Option<i64>,
    /// The first `audioInfo` that named a track: when (ms since the start) and which.
    audio_track: Option<(u64, i64)>,
    /// The script's text `selectTrack` has been sent, and a text `trackSelected` came after it.
    text_select_sent: bool,
    text_ev: bool,
    /// The gaps between `currentTime` reports.
    clock: ReportClock,
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
            resume_sent: false,
            resume_first: None,
            pause_samples: PauseSamples::default(),
            seek_target: None,
            audio_track: None,
            text_select_sent: false,
            text_ev: false,
            clock: ReportClock::default(),
            track_pending: None,
        }
    }
}

/// The probe. State is private; events enter through [`step`](UriProbe::step).
pub struct UriProbe {
    config: Config,
    phase: Phase,
    /// The identity this session registers as; refusal events advance it along the ladder.
    shape: Shape,
    /// The identities refused so far: the input of `register_ladder::decide`.
    refused: Vec<Outcome>,
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
    acb: AcbBind,
    foreground_sent: bool,
    requests: std::collections::BTreeMap<(Method, Option<&'static str>), plx_platform::tv::uri::RequestId>,
}

impl std::fmt::Debug for UriProbe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UriProbe {{ phase: {:?}, shape: {:?} }}", &self.phase, self.shape)
    }
}

impl UriProbe {
    pub fn new(mut config: Config) -> UriProbe {
        if config.variant == Variant::NoWindow {
            config.window_id = None;
        }
        let acb = AcbBind::new(config.variant == Variant::AcbDass);
        UriProbe {
            config,
            phase: Phase::Idle,
            shape: register_ladder::first(),
            refused: Vec::new(),
            started_ms: 0,
            media_id: None,
            subscribed: false,
            settled: None,
            last_time: None,
            time_logged_ms: None,
            time_unparsed_ms: None,
            placed: None,
            facts: Facts::default(),
            acb,
            foreground_sent: false,
            requests: std::collections::BTreeMap::new(),
        }
    }

    /// The only writer of probe state. Continuations borrow only this input, never the model.
    fn transition(&mut self, ev: UriEvent, now_ms: u64, fx: &mut Vec<UriEffect>) {
        if self.phase == Phase::Done { return; }
        enum Work<'a> {
            Start,
            Tick,
            Reply { method: Method, ok: bool, message: &'a Message },
            Service { method: Method, ok: bool, message: &'a Message },
            ServiceAfterVideo { method: Method, ok: bool, message: &'a Message },
            Events { message: &'a Message, index: usize },
            Seen { seen: &'a Seen, message: &'a Message },
            CurrentTime { seen: &'a Seen, message: &'a Message },
            Loaded { message: &'a Message },
            Acb { event: AcbEvent, now_ms: u64 },
            Drive { step: usize },
            NextShape { end: End },
            Stop { end: End },
            Finished,
        }
        let mut work = match &ev {
            UriEvent::Start => Work::Start,
            UriEvent::Back => Work::Stop { end: End::Back },
            UriEvent::Closed if matches!(self.phase, Phase::AwaitingClose) => {
                self.phase = Phase::Registering { deadline: now_ms.saturating_add(REGISTER_TIMEOUT_MS) };
                fx.push(UriEffect::Open(self.shape));
                Work::Finished
            }
            UriEvent::Closed | UriEvent::AcbReleaseTail { .. } => Work::Finished,
            UriEvent::AcbAnswered { request, invoked, completed_ms } => Work::Acb {
                event: AcbEvent::Answered { request: *request, invoked: *invoked, completed_ms: *completed_ms },
                now_ms: *completed_ms,
            },
            UriEvent::Dispatched { method, kind, request } => {
                self.requests.insert((*method, *kind), *request);
                Work::Finished
            }
            UriEvent::Tick => Work::Tick,
            UriEvent::Reply { method, ok, message } => Work::Reply { method: *method, ok: *ok, message },
        };
        // Video handling finishes before the service reply resumes; each seen item finishes before
        // the next one. These continuations never nest, so no heap queue or model copy is needed.
        let mut continuation = None;
        'reduce: loop {
            match work {
                Work::Finished => {
                    if let Some(next) = continuation.take() { work = next; continue 'reduce; }
                    break;
                }
                Work::Service { method, ok, message } => {
                    if self.requests.get(&(method, message.reply_track)).is_some_and(|request| message.reply_request != Some(*request)) { work = Work::Finished; continue 'reduce; }
                    if message.identity_conflict || (self.media_id.is_some() && message.media_id.is_some() && self.media_id != message.media_id) { work = Work::Finished; continue 'reduce; }
                    if let Some(video) = message.video_info {
                        debug_assert!(continuation.is_none());
                        continuation = Some(Work::ServiceAfterVideo { method, ok, message });
                        work = Work::Acb { event: AcbEvent::Video(video), now_ms }; continue 'reduce;
                    }
                    work = Work::ServiceAfterVideo { method, ok, message }; continue 'reduce;
                }
                Work::Events { message, index } => {
                    if self.phase == Phase::Done { work = Work::Finished; continue 'reduce; }
                    let Some(seen) = message.seen.get(index) else {
                        if message.seen.is_empty() {
                            if let Some(name) = &message.unknown { fx.push(self.line(now_ms, &unparsed_text(name))); }
                        }
                        work = Work::Finished; continue 'reduce;
                    };
                    let normalized = message.normalized.contains(&seen.ev);
                    if normalized || seen.ev == Ev::CurrentTime {
                        debug_assert!(continuation.is_none());
                        continuation = Some(Work::Events { message, index: index + 1 });
                        work = if normalized {
                            Work::Seen { seen, message }
                        } else { Work::CurrentTime { seen, message } }; continue 'reduce;
                    }
                    fx.push(self.line(now_ms, &event_text(message, seen, false)));
                    work = Work::Events { message, index: index + 1 }; continue 'reduce;
                }
                Work::Start => {
                    if self.phase != Phase::Idle {
                        work = Work::Finished; continue 'reduce;
                    }
                    self.started_ms = now_ms;
                    let c = &self.config;
                    let window = if c.window_id.is_some() { "exported" } else { "none" };
                    let kind = match c.kind {
                        Kind::Direct => "direct",
                        Kind::Hls => "hls",
                    };
                    let armed = format!(
                        "armed variant={} scheme={} kind={kind} window={window} app={} fw={} build={}{}",
                        c.variant.label(),
                        c.scheme.label(),
                        c.app_id,
                        firmware_word(&c.firmware.release),
                        firmware_word(&c.firmware.build),
                        c.bps.label(),
                    );
                    fx.push(self.line(now_ms, &armed));
                    if self.config.variant.acb() { work = Work::Acb { event: AcbEvent::Start, now_ms }; continue 'reduce; }
                    else {
                        self.phase = Phase::Registering { deadline: now_ms + REGISTER_TIMEOUT_MS };
                        fx.push(UriEffect::Open(self.shape));
                    }
                }
                Work::Tick => {
                    match self.phase {
                        Phase::Acb => { work = Work::Acb { event: AcbEvent::Tick, now_ms }; continue 'reduce; },
                        Phase::Registering { deadline } if now_ms >= deadline => { work = Work::Stop { end: End::Timeout("register") }; continue 'reduce; },
                        Phase::Loading { deadline } if now_ms >= deadline => { work = Work::Stop { end: End::Timeout("load") }; continue 'reduce; },
                        Phase::SettingWindow { deadline } if now_ms >= deadline => {
                            self.facts.window_call = WindowCall::Silent;
                            fx.push(self.line(now_ms, "call setWindowId no reply, playing anyway"));
                            if let Some(media_id) = self.media_id.clone() {
                                self.phase = Phase::Starting { deadline: now_ms + PLAY_TIMEOUT_MS };
                                fx.push(UriEffect::Call(UriCall::Play { media_id }));
                            }
                        }
                        Phase::Starting { deadline } if now_ms >= deadline => { work = Work::Stop { end: End::Timeout("play") }; continue 'reduce; },
                        Phase::Playing { frozen_at } if self.facts.time_moved.is_some() && now_ms >= frozen_at => {
                            { work = Work::Stop { end: End::Timeout("currentTime") }; continue 'reduce; }
                        }
                        Phase::Driving { step, deadline } if now_ms >= deadline => { work = Work::Drive { step }; continue 'reduce; },
                        Phase::Idle | Phase::Registering { .. } | Phase::AwaitingClose | Phase::Loading { .. } | Phase::SettingWindow { .. } | Phase::Starting { .. } | Phase::Playing { .. } | Phase::Driving { .. } | Phase::Done => {}
                    }
                }
                Work::Drive { step } => {
                    let Some(media_id) = self.media_id.clone() else {
                        work = Work::Stop { end: End::Script }; continue 'reduce;
                    };
                    let position = |p: Option<i64>| p.map_or_else(|| "-".to_owned(), |v| v.to_string());
                    match step {
                        0 => {
                            self.facts.pause_samples = PauseSamples { started_ms: Some(now_ms), ended_ms: None, first: None, last: None, range: None, reports: 0 };
                            self.facts.pause_pos = self.last_time;
                            fx.push(self.line(now_ms, &format!("drive pause pos={}", position(self.facts.pause_pos))));
                            fx.push(UriEffect::Call(UriCall::Pause { media_id }));
                        }
                        1 => {
                            self.facts.pause_samples.ended_ms = Some(now_ms);
                            self.facts.resume_pos = self.last_time;
                            self.facts.resume_sent = true;
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
                            self.facts.text_select_sent = true;
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
                            work = Work::Stop { end: End::Script }; continue 'reduce;
                        }
                    }
                    self.phase = Phase::Driving { step: step + 1, deadline: now_ms + DRIVE_STEP_MS };
                }
                Work::Reply { method, ok, message } => {
                    if method == Method::Dropped {
                        let count = message.dropped.unwrap_or(0);
                        fx.push(self.line(now_ms, &format!("transport dropped {count} replies")));
                        work = Work::Stop { end: End::Refused("overflow") }; continue 'reduce;
                    }
                    match self.phase {
                        Phase::Registering { .. } if method == Method::Register => {
                            let shape = register_ladder::label(self.shape);
                            if ok {
                                fx.push(self.line(now_ms, &format!("register shape={shape} ok")));
                                self.phase = Phase::Loading { deadline: now_ms + LOAD_TIMEOUT_MS };
                                let window_id = if self.config.variant.acb() { Some(super::acb_bind::DUMMY_WINDOW_ID.to_owned()) }
                                else if self.config.variant.binds_at_load() { self.config.window_id.clone() } else { None };
                                fx.push(UriEffect::Call(UriCall::Load { window_id, bps: self.config.bps }));
                            } else {
                                fx.push(self.line(now_ms, &format!("register shape={shape} refused({})", message.why())));
                                work = Work::NextShape { end: End::Refused("register") }; continue 'reduce;
                            }
                        }
                        Phase::Loading { .. } | Phase::SettingWindow { .. } | Phase::Starting { .. } | Phase::Playing { .. } | Phase::Driving { .. } | Phase::Acb => {
                            { work = Work::Service { method, ok, message }; continue 'reduce; }
                        }
                        Phase::Idle | Phase::Registering { .. } | Phase::AwaitingClose | Phase::Done => {}
                    }
                }
                Work::NextShape { end } => {
                    self.refused.push(Outcome::Refused);
                    match register_ladder::decide(&self.refused) {
                        Step::Register(shape) => {
                            self.shape = shape;
                            self.media_id = None;
                            self.subscribed = false;
                            fx.push(UriEffect::Close);
                            self.phase = Phase::AwaitingClose;
                        }
                        Step::Settled(_) | Step::Failed(_) => { work = Work::Stop { end: end }; continue 'reduce; },
                    }
                }
                Work::ServiceAfterVideo { method, ok, message } => {

                    let denied = !ok || message.return_value == Some(false);
                    let text = message.why();
                    if let Some(kind) = message.confirmed_track {
                        match kind { "text" => self.facts.text = CallResult::Ok, "audio" => self.facts.audio = CallResult::Ok, _ => {} }
                    }
                    match method {
                        Method::Load if denied && self.media_id.is_none() && matches!(self.phase, Phase::Loading { .. }) => {
                            fx.push(self.line(now_ms, &format!("call load err({text})")));
                            work = Work::NextShape { end: End::Refused("load") }; continue 'reduce;
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
                                work = Work::Finished; continue 'reduce;
                            }
                            work = Work::Stop { end: End::Refused("play") }; continue 'reduce;
                        }
                        Method::NotifyForeground => {
                            fx.push(self.line(now_ms, &format!("call notifyForeground {}", Self::said(ok, denied, &text))));
                            if denied { work = Work::Stop { end: End::Refused("notifyForeground") }; continue 'reduce; }
                            work = Work::Finished; continue 'reduce;
                        }
                        Method::Pause => {
                            self.facts.pause = Self::outcome(ok, denied);
                            fx.push(self.line(now_ms, &format!("call pause {}", Self::said(ok, denied, &text))));
                            work = Work::Finished; continue 'reduce;
                        }
                        Method::Seek => {
                            self.facts.seek = Self::outcome(ok, denied);
                            fx.push(self.line(now_ms, &format!("call seek {}", Self::said(ok, denied, &text))));
                            work = Work::Finished; continue 'reduce;
                        }
                        Method::SelectTrack => {
                            let kind = if let Some(kind) = message.reply_track { kind } else { self.facts.track_pending.take().unwrap_or("track") };
                            let outcome = Self::outcome(ok, denied);
                            match kind {
                                "text" => if self.facts.text != CallResult::Ok || outcome == CallResult::Refused { self.facts.text = outcome },
                                "audio" => if self.facts.audio != CallResult::Ok || outcome == CallResult::Refused { self.facts.audio = outcome },
                                _ => {},
                            }
                            fx.push(self.line(now_ms, &format!("call selectTrack {kind} {}", Self::said(ok, denied, &text))));
                            work = Work::Finished; continue 'reduce;
                        }
                        Method::SetSubtitleEnable => {
                            // No event confirms this call: its own reply is the whole answer.
                            self.facts.subs = if denied { CallResult::Refused } else if message.return_value == Some(true) { CallResult::Ok } else { CallResult::Unconfirmed };
                            fx.push(self.line(now_ms, &format!("call setSubtitleEnable {}", Self::said(ok, denied, &text))));
                            work = Work::Finished; continue 'reduce;
                        }
                        Method::SetWindowId => {
                            if !denied && message.return_value != Some(true) { work = Work::Finished; continue 'reduce; }
                            if denied {
                                self.facts.window_call = WindowCall::Refused;
                                fx.push(self.line(now_ms, &format!("call setWindowId err({text})")));
                            } else {
                                self.facts.window_call = WindowCall::Answered;
                                fx.push(self.line(now_ms, "call setWindowId ok"));
                            }
                            if matches!(self.phase, Phase::SettingWindow { .. }) {
                                if let Some(media_id) = self.media_id.clone() {
                                    self.phase = Phase::Starting { deadline: now_ms + PLAY_TIMEOUT_MS };
                                    fx.push(UriEffect::Call(UriCall::Play { media_id }));
                                }
                            }
                            work = Work::Finished; continue 'reduce;
                        }
                        Method::Load | Method::Subscribe | Method::Play if !denied && message.return_value == Some(true) && !message.has_events() => {
                            if method == Method::Play && matches!(self.phase, Phase::Driving { .. }) {
                                self.facts.resume = CallResult::Unconfirmed;
                            }
                            fx.push(self.line(now_ms, &format!("call {} ok", match method {
                                    Method::Load => "load",
                                    Method::Subscribe => "subscribe",
                                    Method::Register | Method::Play | Method::NotifyForeground | Method::Pause
                                    | Method::Seek | Method::SelectTrack | Method::SetSubtitleEnable
                                    | Method::SetWindowId | Method::Unload | Method::Dropped | Method::Other => "play",
                                })));
                        }
                        Method::Register | Method::Load | Method::Subscribe | Method::Play
                        | Method::Unload | Method::Dropped | Method::Other => {}
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
                    work = Work::Events { message, index: 0 }; continue 'reduce;
                }
                Work::Seen { seen, message } => {
                    if seen.ev != Ev::CurrentTime {
                        fx.push(self.line(now_ms, &event_text(message, seen, false)));
                    }
                    match seen.ev {
                        Ev::Error => { work = Work::Stop { end: End::Error(message.error_code) }; continue 'reduce; },
                        Ev::LoadCompleted => {
                            if matches!(self.phase, Phase::Loading { .. }) {
                                work = Work::Loaded { message }; continue 'reduce;
                            }
                        }
                        Ev::Playing => {
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
                            let samples = &self.facts.pause_samples;
                            self.facts.paused_ev |= samples.started_ms.is_some() && samples.ended_ms.is_none();
                            if self.facts.pause == CallResult::Unconfirmed {
                                self.facts.pause = CallResult::Ok;
                            }
                        }
                        Ev::SeekDone => {
                            // The subscription's request ID is not the seek command's identity, so only the
                            // order says whose it is: one from before the script's Seek call is not its.
                            self.facts.seek_done_ev |= self.facts.seek_target.is_some();
                        }
                        Ev::TrackSelected => {
                            if self.facts.text_select_sent && message.selected_track.is_some_and(|(kind, _)| kind == "text") {
                                self.facts.text_ev = true;
                            }
                            // A generic subscription event cannot name a text or audio command.
                            match message.confirmed_track {
                                Some("text") if self.facts.text == CallResult::Unconfirmed => self.facts.text = CallResult::Ok,
                                Some("audio") if self.facts.audio == CallResult::Unconfirmed => self.facts.audio = CallResult::Ok,
                                _ => {},
                            }
                        }
                        Ev::EndOfStream => {
                            self.facts.eos_ev = true;
                            if matches!(self.phase, Phase::Starting { .. } | Phase::Playing { .. } | Phase::Driving { .. } | Phase::Acb) {
                                work = Work::Stop { end: End::Eos }; continue 'reduce;
                            }
                        }
                        Ev::CurrentTime => { work = Work::CurrentTime { seen, message }; continue 'reduce; },
                        Ev::VideoInfo => {
                            self.facts.video_seen.get_or_insert(now_ms.saturating_sub(self.started_ms));
                            if let (Some((width, height)), true) = (message.dims, self.config.window_id.is_some()) {
                                if self.placed != Some((width, height)) {
                                    self.placed = Some((width, height));
                                    fx.push(UriEffect::PlaceWindow { width, height });
                                }
                            }
                        }
                        Ev::AudioInfo => {
                            let at = now_ms.saturating_sub(self.started_ms);
                            self.facts.audio_seen.get_or_insert(at);
                            if let Some(track) = message.audio_track {
                                self.facts.audio_track.get_or_insert((at, track));
                            }
                        }
                        Ev::BufferRange | Ev::BufferingStart | Ev::BufferingEnd | Ev::SourceInfo | Ev::StreamingInfo | Ev::CustomizedStreamingInfo | Ev::UnloadCompleted => {}
                    }
                }
                Work::Loaded { message } => {
                    if self.media_id.is_none() {
                        self.media_id = message.media_id.clone();
                    }
                    if self.media_id.is_none() {
                        fx.push(self.line(now_ms, "loadCompleted carried no mediaId"));
                        work = Work::Stop { end: End::Refused("mediaId") }; continue 'reduce;
                    }
                    self.facts.loaded_ms = Some(now_ms.saturating_sub(self.started_ms));
                    if self.settled.is_none() {
                        self.settled = Some(Settle::Started);
                        fx.push(UriEffect::Settle(Settle::Started));
                    }
                    if self.config.variant.acb() {
                        // Start service transport independently: some firmware emits videoInfo only after Play.
                        if let Some(media_id) = self.media_id.clone() {
                            self.phase = Phase::Starting { deadline: now_ms + PLAY_TIMEOUT_MS };
                            fx.push(UriEffect::Call(UriCall::Play { media_id }));
                        }
                        let media_id = self.media_id.clone().expect("loaded identity");
                        work = Work::Acb { event: AcbEvent::Loaded { media_id }, now_ms }; continue 'reduce;
                    }
                    match (self.config.variant, self.config.window_id.clone(), self.media_id.clone()) {
                        (Variant::Late, Some(window_id), Some(media_id)) => {
                            self.facts.window_call = WindowCall::Pending;
                            self.phase = Phase::SettingWindow { deadline: now_ms + LATE_ID_TIMEOUT_MS };
                            fx.push(UriEffect::Call(UriCall::SetWindowId { media_id, window_id }));
                        }
                        _ => if let Some(media_id) = self.media_id.clone() {
                            self.phase = Phase::Starting { deadline: now_ms + PLAY_TIMEOUT_MS };
                            fx.push(UriEffect::Call(UriCall::Play { media_id }));
                        },
                    }
                }
                Work::Acb { event, now_ms } => {
                    let mut out = Vec::new();
                    self.acb.step(event, now_ms, &mut out);
                    for effect in out {
                        match effect {
                            AcbFx::Run {request, step, op, ..} => {
                                self.phase = Phase::Acb;
                                fx.push(self.line(now_ms, &format!("acb request={} step={step:?}", request.number())));
                                fx.push(UriEffect::Acb {request, op});
                            }
                            AcbFx::WaitingVideo => {
                                self.phase = Phase::Acb;
                                fx.push(self.line(now_ms, "acb waiting for real videoInfo dimensions/rate"));
                            }
                            AcbFx::Answered {step, invoked} => {
                                fx.push(self.line(now_ms, &format!("acb answer step={step:?} invoked={invoked} (not plane proof)")));
                            }
                            AcbFx::GatePassed => {
                                self.phase = Phase::Registering {deadline: now_ms.saturating_add(REGISTER_TIMEOUT_MS)};
                                fx.push(UriEffect::Open(self.shape));
                            }
                            AcbFx::Bound => {
                                self.phase = if self.facts.playing_ms.is_some() { Phase::Playing {frozen_at: now_ms.saturating_add(FROZEN_TIME_MS)} }
                                else { Phase::Starting {deadline: now_ms.saturating_add(PLAY_TIMEOUT_MS)} };
                            }
                            AcbFx::Failed(failure) => {
                                let end = match failure {
                                    AcbFailure::Refused => End::Refused("acb"),
                                    AcbFailure::Timeout => End::Timeout("acb"),
                                    AcbFailure::VideoTimeout => End::Timeout("acb-video"),
                                };
                                if self.phase != Phase::Done {
                                    self.facts.failed_in = Some(self.phase.name());
                                    self.facts.ended = Some(end);
                                    if self.settled.is_none() {
                                        self.settled = Some(Settle::StartFailed);
                                        fx.push(UriEffect::Settle(Settle::StartFailed));
                                    }
                                    if let Some(media_id) = self.media_id.take() {
                                        fx.push(self.line(now_ms, "call unload requested"));
                                        fx.push(UriEffect::Call(UriCall::Unload { media_id }));
                                    }
                                    fx.push(UriEffect::Close);
                                    self.phase = Phase::Done;
                                    fx.push(self.line(now_ms, &self.summary()));
                                    fx.push(self.line(now_ms, "tester: report picture yes/no and sound yes/no"));}
                            }
                        }
                    }
                }
                Work::CurrentTime { seen, message } => {
                    let Some(value) = message.current_time else {
                        // Reported, but in neither known shape: say so once, with no payload. It is no report
                        // of a position, so it moves nothing and arms no timeout.
                        if self.time_unparsed_ms.is_none() {
                            self.time_unparsed_ms = Some(now_ms);
                            fx.push(self.line(now_ms, &event_text(message, seen, false)));
                        }
                        work = Work::Finished; continue 'reduce;
                    };
                    // Every valid report is fresh evidence, including an unchanged position.
                    if self.facts.pause_samples.ended_ms.is_none() && self.facts.pause_samples.started_ms.is_some_and(|at| now_ms > at) {
                        self.facts.pause_samples.first.get_or_insert((now_ms, value));
                        self.facts.pause_samples.last = Some((now_ms, value));
                        self.facts.pause_samples.range = Some(match self.facts.pause_samples.range {
                                Some((low, high)) => (low.min(value), high.max(value)),
                                None => (value, value),
                            });
                        self.facts.pause_samples.reports = self.facts.pause_samples.reports.saturating_add(1);
                    }
                    if let Some(last) = self.facts.clock.last_ms {
                        *self.facts.clock.gaps.entry(now_ms.saturating_sub(last)).or_insert(0) += 1;
                    }
                    self.facts.clock.last_ms = Some(now_ms);
                    if self.facts.resume_sent && self.facts.seek_target.is_none() && self.facts.resume_first.is_none() {
                        self.facts.resume_first = Some(value);
                    }
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
                        let due = self.time_logged_ms.is_none_or(|at| now_ms.saturating_sub(at) >= CLOCK_LOG_EVERY_MS);
                        if first || due {
                            self.time_logged_ms = Some(now_ms);
                            fx.push(self.line(now_ms, &event_text(message, seen, false)));
                        }
                    } else if self.facts.time_moved.is_none() && self.time_logged_ms.is_none() {
                        self.time_logged_ms = Some(now_ms);
                        fx.push(self.line(now_ms, &event_text(message, seen, true)));
                    }
                }
                Work::Stop { end } => {
                    if self.phase == Phase::Done {
                        work = Work::Finished; continue 'reduce;
                    }
                    self.facts.failed_in = Some(self.phase.name());
                    self.facts.ended = Some(end);
                    if self.settled.is_none() {
                        self.settled = Some(Settle::StartFailed);
                        fx.push(UriEffect::Settle(Settle::StartFailed));
                    }
                    if let Some(media_id) = self.media_id.take() {
                        fx.push(self.line(now_ms, "call unload requested"));
                        fx.push(UriEffect::Call(UriCall::Unload { media_id }));
                    }
                    fx.push(UriEffect::Close);
                    self.phase = Phase::Done;
                    fx.push(self.line(now_ms, &self.summary()));
                    fx.push(self.line(now_ms, "tester: report picture yes/no and sound yes/no"));
                }
            }
            work = Work::Finished;
        }
    }

    /// The session is over (everything it will say has been said).
    pub fn is_done(&self) -> bool {
        self.phase == Phase::Done
    }

    /// From `playing` until the end: the service has said the video plays, not plane proof.
    pub fn plane_open(&self) -> bool {
        matches!(self.phase, Phase::Playing { .. } | Phase::Driving { .. })
    }

    /// Apply one event, preserving effect order. Done is inert.
    pub fn step(&mut self, ev: UriEvent, now_ms: u64, fx: &mut Vec<UriEffect>) {
        self.transition(ev, now_ms, fx);
    }

    fn line(&self, now_ms: u64, body: &str) -> UriEffect {
        UriEffect::Log(format!("uriprobe: t={} {body}", now_ms.saturating_sub(self.started_ms)))
    }

    /// Accepted calls remain provisional until their attributed evidence arrives.
    fn outcome(ok: bool, denied: bool) -> CallResult {
        if ok && !denied { CallResult::Unconfirmed } else { CallResult::Refused }
    }

    /// The tail of a call log line.
    fn said(ok: bool, denied: bool, text: &str) -> String {
        if ok && !denied { "ok".to_owned() } else { format!("err({text})") }
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
        // `audioInfo` named a track by `loadCompleted` (the same message counts): the service had
        // already selected one at load.
        let audio_at_load = match (self.facts.audio_track, self.facts.loaded_ms) {
            (Some((at, track)), Some(loaded)) if at <= loaded => track.to_string(),
            _ => "none".to_owned(),
        };
        let summary = format!(
            "summary load={load} playing={} t_load={} t_playing={} currentTime_advanced={} videoInfo={} audioInfo={} window={} \
             setWindowId={window_call} variant={} scheme={} shape={} end={end} audio_at_load={audio_at_load} clock_period_ms={}",
            if self.facts.playing_ms.is_some() { "yes" } else { "no" },
            ms(self.facts.loaded_ms),
            ms(self.facts.playing_ms),
            if self.facts.time_moved.is_some() { "yes" } else { "no" },
            seen(self.facts.video_seen),
            seen(self.facts.audio_seen),
            if self.config.window_id.is_some() { "exported" } else { "none" },
            self.config.variant.label(),
            self.config.scheme.label(),
            register_ladder::label(self.shape),
            ms(self.facts.clock.median_ms()),
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
        // Some firmware stops sending positions while paused. A pause event during the dwell
        // and a resume near the original position corroborate the freeze when no samples arrive.
        let samples = &self.facts.pause_samples;
        let event_freeze = self.facts.paused_ev && samples.reports == 0
            && self.facts.pause_pos.zip(self.facts.resume_first)
                .is_some_and(|(before, after)| before.abs_diff(after) <= 1000);
        let (paused, pause_basis) = if self.facts.pause == CallResult::Refused {
            ("refused", "reported_positions")
        } else if event_freeze {
            ("froze", "paused_event_and_resume")
        } else if samples.started_ms.is_some() {
            (samples.grade(), "reported_positions")
        } else {
            (call(self.facts.pause), "reported_positions")
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
             pause_basis={pause_basis} pause_samples={} pause_span_ms={} pause_end_fresh={} paused_event={} seekdone={} resume_from={} text_ev={}",
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
            if self.facts.seek_done_ev { "yes" } else { "no" },
            self.facts.resume_first.map_or_else(|| "-".to_owned(), |position| position.to_string()),
            if self.facts.text_ev { "yes" } else { "no" },
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
            firmware: Firmware { release: "5.4.2".to_owned(), build: "5.4.2-12".to_owned() },
            transport: variant.drives(),
            bps: AbrBps::default(),
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
        call(UriCall::Load { window_id: window.map(str::to_owned), bps: AbrBps::default() })
    }
    fn media(id: &str) -> MediaId {
        id.into()
    }

    // ------------------------------------------------------------------ happy paths

    #[test]
    fn at_load_happy_path_in_order() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        let fx = rig.start();
        assert_eq!(logs(&fx), ["uriprobe: t=0 armed variant=at-load scheme=https kind=direct window=exported app=com.test.app fw=5.4.2 build=5.4.2-12"]);
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
             window=exported setWindowId=n/a variant=at-load scheme=https shape=named end=back audio_at_load=none clock_period_ms=-"
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
        assert_eq!(logs(&fx), ["uriprobe: t=0 armed variant=at-load scheme=http kind=hls window=none app=com.test.app fw=5.4.2 build=5.4.2-12"]);
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

    // ------------------------------------------------------------------ adaptive-streaming facts

    #[test]
    fn buffer_range_logs_the_buffer_health_fields() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","bufferRange":{"beginTime":0,"endTime":58,"remainingTime":12,"percent":40}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=bufferRange beginTime=0 endTime=58 remainingTime=12 percent=40"]);
        // a firmware which sends only the old pair still logs it
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","bufferRange":{"beginTime":3,"endTime":9}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=bufferRange beginTime=3 endTime=9"]);
    }

    #[test]
    fn streaming_info_logs_the_two_bitrates_as_numbers_and_no_other_field() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let payload = format!(r#"{{"mediaId":"_m1","streamingInfo":{{"instantBitrate":8000000,"totalBitrate":2000000.5,"host":"192.168.1.5","uri":"{URL}"}}}}"#);
        let fx = rig.msg("subscribe", &payload);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=streamingInfo instantBitrate=8000000 totalBitrate=2000000.5"]);
        assert_clean("streamingInfo", &logs(&fx).join("\n"));
    }

    #[test]
    fn streaming_info_never_logs_a_string_even_under_a_listed_key() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        // A host under a key the probe reads, a word `scalar` would print for a codec, a boolean and
        // a `{num, den}` pair: none is a bit rate, so none is shown.
        let payload = format!(
            r#"{{"mediaId":"_m1","streamingInfo":{{"instantBitrate":"{URL}","totalBitrate":"hevc"}}}}"#
        );
        let fx = rig.msg("subscribe", &payload);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=streamingInfo"]);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","streamingInfo":{"instantBitrate":"192.168.1.5","totalBitrate":{"num":1,"den":2}}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=streamingInfo"]);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","streamingInfo":{"instantBitrate":true,"totalBitrate":4000000}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=streamingInfo totalBitrate=4000000"]);
        // the same through the decoder, whose diagnostic projection is what the machine prints
        let message = parse_event(&payload);
        let seen = message.seen.iter().find(|s| s.ev == Ev::StreamingInfo).unwrap();
        assert_eq!(seen.detail, "");
        assert_clean("Message", &format!("{message:?}"));
    }

    #[test]
    fn customized_streaming_info_logs_the_ladder_size_and_position() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let payload = format!(r#"{{"mediaId":"_m1","customizedStreamingInfo":{{"numOfRates":4,"currentIndexOfRate":1,"uri":"{URL}"}}}}"#);
        let fx = rig.msg("subscribe", &payload);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=customizedStreamingInfo numOfRates=4 currentIndexOfRate=1"]);
        assert_clean("customizedStreamingInfo", &logs(&fx).join("\n"));
        let message = parse_event(&payload);
        assert!(message.normalized.contains(&Ev::CustomizedStreamingInfo), "an object is a supported event");
        // strings are not counts
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","customizedStreamingInfo":{"numOfRates":"4","currentIndexOfRate":"hevc"}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=customizedStreamingInfo"]);
    }

    #[test]
    fn video_info_adds_the_bitrates_only_when_present() {
        let mut rig = Rig::new(Variant::AtLoad, None);
        rig.loading();
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"codec":"H264","currentBitrate":2000000,"contentBitrate":8000000}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=videoInfo width=1280 height=720 codec=H264 currentBitrate=2000000 contentBitrate=8000000"]);
        let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"codec":"H264"}}"#);
        assert_eq!(logs(&fx), ["uriprobe: t=0 ev=videoInfo width=1280 height=720 codec=H264"]);
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
        let with = build_load(tricky, "com.test.app", Some(WINDOW), AbrBps::default());
        let value: Value = serde_json::from_str(&with).expect("valid JSON");
        assert_eq!(value["uri"], tricky);
        assert_eq!(value["type"], "media");
        assert_eq!(value["payload"]["option"]["appId"], "com.test.app");
        assert_eq!(value["payload"]["option"]["windowId"], WINDOW);
        let without: Value = serde_json::from_str(&build_load(tricky, "com.test.app", None, AbrBps::default())).unwrap();
        assert!(without["payload"]["option"].get("windowId").is_none());
        // no rates asked for: the option is exactly what it was before they existed
        assert!(!with.contains("adaptiveStreaming"));
        assert_eq!(without["payload"]["option"], serde_json::json!({ "appId": "com.test.app" }));
    }

    #[test]
    fn build_load_nests_the_asked_rates_under_adaptive_streaming_bps() {
        let all = AbrBps { start: Some(720_000), maximum: Some(2_000_000), minimum: Some(320_000) };
        let value: Value = serde_json::from_str(&build_load("http://h/x", "com.test.app", Some(WINDOW), all)).unwrap();
        assert_eq!(
            value["payload"]["option"],
            serde_json::json!({
                "appId": "com.test.app",
                "windowId": WINDOW,
                "adaptiveStreaming": { "bps": { "start": 720_000, "maximum": 2_000_000, "minimum": 320_000 } },
            })
        );
        // only the rates given appear
        let cap = AbrBps { maximum: Some(720_000), ..AbrBps::default() };
        let value: Value = serde_json::from_str(&build_load("http://h/x", "com.test.app", None, cap)).unwrap();
        assert_eq!(value["payload"]["option"]["adaptiveStreaming"], serde_json::json!({ "bps": { "maximum": 720_000 } }));
        assert!(value["payload"]["option"]["adaptiveStreaming"]["bps"].get("start").is_none());
        // rates are JSON integers, not strings
        assert!(value["payload"]["option"]["adaptiveStreaming"]["bps"]["maximum"].is_u64());
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
        let load = UriCall::Load { window_id: None, bps: AbrBps::default() };
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
    fn the_trigger_text_adds_the_rates_to_any_variant() {
        let bps = |start, maximum, minimum| AbrBps { start, maximum, minimum };
        for (text, want) in [
            ("load", Some((Variant::AtLoad, AbrBps::default()))),
            ("", Some((Variant::AtLoad, AbrBps::default()))),
            ("load bpsmax=720000\n", Some((Variant::AtLoad, bps(None, Some(720_000), None)))),
            ("bpsmax=720000", Some((Variant::AtLoad, bps(None, Some(720_000), None)))),
            ("bpsmax=720000 late", Some((Variant::Late, bps(None, Some(720_000), None)))),
            ("drive bpsstart=320000 bpsmax=2000000  bpsmin=320000", Some((Variant::Drive, bps(Some(320_000), Some(2_000_000), Some(320_000))))),
            (" LOAD BpsMax=1000000000 ", Some((Variant::AtLoad, bps(None, Some(MAX_ABR_BPS), None)))),
            ("load bpsmin=1", Some((Variant::AtLoad, bps(None, None, Some(1))))),
        ] {
            let want = want.map(|(variant, bps)| Trigger { variant, bps });
            assert_eq!(Trigger::from_trigger(text), want, "{text:?}");
        }
    }

    #[test]
    fn a_trigger_with_junk_arms_nothing() {
        for text in [
            "load bpsmax=",
            "load bpsmax=abc",
            "load bpsmax=0",
            "load bpsmax=-5",
            "load bpsmax=+5",
            "load bpsmax=1.5",
            "load bpsmax=720k",
            "load bpsmax=1000000001",
            "load bpsmax=4294967296",
            "load bpsmax=99999999999999999999",
            "load bpsmax=720000 bpsmax=720000",
            "load bpsmaximum=720000",
            "load bps=720000",
            "load nope=1",
            "load late",
            "late drive",
            "bpsmax=720000 lates",
            "http://x?a=1",
        ] {
            assert_eq!(Trigger::from_trigger(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_armed_line_names_the_rates_only_when_given_and_the_load_carries_them() {
        let bps = AbrBps { start: Some(320_000), maximum: Some(720_000), minimum: None };
        let mut rig = Rig { probe: UriProbe::new(Config { bps, ..config(Variant::AtLoad, Some(WINDOW)) }), now: 1_000 };
        let fx = rig.start();
        assert_eq!(logs(&fx), ["uriprobe: t=0 armed variant=at-load scheme=https kind=direct window=exported app=com.test.app fw=5.4.2 build=5.4.2-12 bpsstart=320000 bpsmax=720000"]);
        let fx = rig.msg("register", "");
        assert!(fx.contains(&call(UriCall::Load { window_id: Some(WINDOW.to_owned()), bps })), "{fx:?}");
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
        assert!(summary.contains("pause=froze pause_delta=- resume=sent"), "{summary}");
        assert!(summary.contains("pause_basis=paused_event_and_resume"), "{summary}");
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
        fx.iter().find_map(|fx| match fx {UriEffect::Acb {request,..} => Some(*request), UriEffect::Open(_) | UriEffect::Call(_) | UriEffect::Close | UriEffect::Settle(_) | UriEffect::PlaceWindow { .. } | UriEffect::Log(_) => None}).expect("ACB effect")
    }
    fn acb_loaded(variant: Variant, early_video: bool) -> (Rig, Vec<UriEffect>) {
        let mut rig=Rig::new(variant,None);
        let fx=rig.start(); let request=acb_request(&fx);
        let fx=rig.feed(UriEvent::AcbAnswered {request,invoked:true,completed_ms:rig.now});
        assert!(fx.contains(&UriEffect::Open(Shape::Named)));
        let fx=rig.msg("register","");
        assert!(fx.contains(&load(Some("window_id_dummy"))));
        let fx=rig.msg("load",r#"{"returnValue":true,"mediaId":"_m1"}"#);
        let calls:Vec<_>=fx.iter().filter_map(|fx| match fx {UriEffect::Call(call)=>Some(call.method()),UriEffect::Open(_) | UriEffect::Acb { .. } | UriEffect::Close | UriEffect::Settle(_) | UriEffect::PlaceWindow { .. } | UriEffect::Log(_) => None}).collect();
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
                let op=fx.iter().find_map(|fx| match fx {UriEffect::Acb{op,..}=>Some(op.clone()),UriEffect::Open(_) | UriEffect::Call(_) | UriEffect::Close | UriEffect::Settle(_) | UriEffect::PlaceWindow { .. } | UriEffect::Log(_) => None}).unwrap();
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
        assert!(rig.probe.acb.waiting_for_video());
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
            assert!(rig.probe.acb.waiting_for_video(),"foreign video advanced binding: {fx:?}");
            assert!(!fx.iter().any(|fx|matches!(fx,UriEffect::Acb{..})));
        }
    }

    /// The whole observable effect stream of the ACB arms, every log line and request number
    /// included: the contract the shared ACB bind sequence (`acb_bind`) was lifted out of this
    /// machine under. A change that moves a line, a number or a delay fails here.
    fn acb_transcript() -> String {
        let mut out = String::new();
        let mut note = |rig: &Rig, what: &str, fx: &[UriEffect]| {
            for effect in fx { out.push_str(&format!("{:>6} {what}: {effect:?}\n", rig.now)); }
        };
        let answer = |rig: &mut Rig, fx: &[UriEffect], invoked: bool| {
            rig.feed(UriEvent::AcbAnswered {request: acb_request(fx), invoked, completed_ms: rig.now})
        };
        for (variant, early_video) in [(Variant::AcbControl, true), (Variant::AcbDass, false)] {
            let mut rig = Rig::new(variant, None);
            let fx = rig.start(); note(&rig, "start", &fx);
            let fx2 = answer(&mut rig, &fx, true); note(&rig, "check", &fx2);
            let fx = rig.msg("register", ""); note(&rig, "register", &fx);
            let fx = rig.msg("load", r#"{"returnValue":true,"mediaId":"_m1"}"#); note(&rig, "load", &fx);
            if early_video {
                let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"frameRate":24,"bitRate":4139352}}"#);
                note(&rig, "video", &fx);
            }
            let mut fx = rig.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#); note(&rig, "loaded", &fx);
            if !early_video {
                let waiting = rig.tick(500); note(&rig, "waiting", &waiting);
                let playing = rig.msg("subscribe", r#"{"mediaId":"_m1","playing":true}"#); note(&rig, "playing", &playing);
                fx = rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"frameRate":24}}"#); note(&rig, "video", &fx);
            }
            for step in 0..5 {
                let fx2 = answer(&mut rig, &fx, true); note(&rig, "answer", &fx2);
                if step < 4 {
                    let wait = if step == 3 { 300 } else { 200 };
                    let early = rig.tick(wait - 1); note(&rig, "early", &early);
                    fx = rig.tick(1); note(&rig, "tick", &fx);
                }
            }
            let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","currentTime":100}"#); note(&rig, "time", &fx);
            let fx = rig.feed(UriEvent::Back); note(&rig, "back", &fx);
        }
        // Refused, late and timed-out steps, the video wait, and the stops that cancel it.
        for ending in ["refused", "late", "step-timeout", "video-timeout", "eos"] {
            let mut rig = Rig::new(Variant::AcbDass, None);
            let fx = rig.start(); answer(&mut rig, &fx, true);
            rig.msg("register", ""); rig.msg("load", r#"{"returnValue":true,"mediaId":"_m1"}"#);
            let has_video = matches!(ending, "refused" | "late" | "step-timeout");
            if has_video { rig.msg("subscribe", r#"{"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"frameRate":24}}"#); }
            let fx = rig.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
            let fx = match ending {
                "refused" => answer(&mut rig, &fx, false),
                "late" => rig.feed(UriEvent::AcbAnswered {request: acb_request(&fx), invoked: true, completed_ms: rig.now + LATE_ID_TIMEOUT_MS}),
                "step-timeout" => rig.tick(LATE_ID_TIMEOUT_MS),
                "video-timeout" => rig.tick(LOAD_TIMEOUT_MS),
                _ => rig.msg("subscribe", r#"{"mediaId":"_m1","endOfStream":true}"#),
            };
            note(&rig, ending, &fx);
        }
        out
    }
    #[test]
    fn uri_acb_effect_stream_is_the_recorded_contract() {
        assert_eq!(acb_transcript(), include_str!("uriprobe_acb_transcript.txt"));
    }

    fn drive_pause_rig() -> Rig { drive_pause_rig_at(1000) }
    /// A drive session whose Pause has been sent and answered, the position at the call `position`.
    fn drive_pause_rig_at(position: i64) -> Rig {
        let mut rig=Rig::new(Variant::Drive,Some(WINDOW));
        rig.playing();
        rig.msg("subscribe",&format!(r#"{{"mediaId":"_m1","currentTime":{position}}}"#));
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
    fn uri_drive_pause_event_and_resume_position_confirm_freeze_without_samples() {
        for (resumed, expected) in [(2641, "froze"), (4939, "unconfirmed"), (333, "unconfirmed")] {
            let mut rig = drive_pause_rig_at(2439);
            rig.msg("subscribe", r#"{"mediaId":"_m1","paused":true}"#);
            rig.tick(DRIVE_STEP_MS);
            drive_report_after(&mut rig, 200, resumed);
            let summary = summary_after_back(&mut rig);
            assert!(summary.contains(&format!("pause={expected} ")), "{summary}");
        }
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

    // ------------------------------------------------------------------ the fields a tester's run reads (D2)

    /// A drive session from Pause (the position `position` at the call) to the end of the Resume
    /// step: the dwell's reports are `position` too, then the resume call is sent.
    fn drive_resumed_rig(position: i64) -> Rig {
        let mut rig = drive_pause_rig_at(position);
        drive_report_after(&mut rig, 250, position);
        drive_report_after(&mut rig, 2_000, position);
        let fx = rig.tick(250);
        assert!(has_log(&fx, &format!("drive resume pos={position}")), "{fx:?}");
        assert!(fx.contains(&call(UriCall::Play { media_id: media("_m1") })));
        rig
    }
    /// The same session one step on: the seek 20 s past `position + 200` is sent and answered, and
    /// the first position after it is `landed`.
    fn drive_seeked_rig(position: i64, landed: i64) -> Rig {
        let mut rig = drive_resumed_rig(position);
        drive_report_after(&mut rig, 100, position + 200);
        let fx = rig.tick(DRIVE_STEP_MS);
        assert!(has_log(&fx, &format!("drive seek target={}", position + 200 + DRIVE_SEEK_MS)), "{fx:?}");
        rig.msg("seek", r#"{"returnValue":true}"#);
        drive_report_after(&mut rig, 100, landed);
        rig
    }
    fn summary_after_back(rig: &mut Rig) -> String {
        let fx = rig.feed(UriEvent::Back);
        summary_of(&fx).to_owned()
    }

    #[test]
    fn the_armed_line_names_the_firmware_the_driver_read() {
        let mut rig = Rig::new(Variant::Drive, Some(WINDOW));
        let fx = rig.start();
        assert!(logs(&fx)[0].ends_with("app=com.test.app fw=5.4.2 build=5.4.2-12"), "{:?}", logs(&fx));
    }

    #[test]
    fn an_unknown_or_unusual_firmware_prints_a_dash_and_never_the_text() {
        let long = "1".repeat(FIRMWARE_WORD_CHARS + 1);
        for (release, build, want) in [
            ("", "", "fw=- build=-"),
            ("6.5.3", "", "fw=6.5.3 build=-"),
            ("11.1.0", "11.1.0-3701", "fw=11.1.0 build=11.1.0-3701"),
            ("5.4.2 token=abc", "http://192.168.1.5/x?X-Plex-Token=SECRET", "fw=- build=-"),
            ("5.4.2\n", "5.4.2/47", "fw=- build=-"),
            (long.as_str(), "ok_1+2", "fw=- build=ok_1+2"),
        ] {
            let mut c = config(Variant::AtLoad, Some(WINDOW));
            c.firmware = Firmware { release: release.to_owned(), build: build.to_owned() };
            let mut rig = Rig { probe: UriProbe::new(c), now: 1_000 };
            let fx = rig.start();
            assert!(logs(&fx)[0].ends_with(&format!("app=com.test.app {want}")), "{release:?} {build:?}: {:?}", logs(&fx));
        }
    }

    #[test]
    fn a_resume_that_restarts_the_clock_reads_where_it_restarted() {
        let mut rig = drive_resumed_rig(5_000);
        assert_eq!(rig.probe.facts.pause_pos, Some(5_000));
        // The dwell's reports (5000, before the resume call) are not the first position after it.
        drive_report_after(&mut rig, 120, 333);
        drive_report_after(&mut rig, 250, 583);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" resume=sent "), "{summary}");
        assert!(summary.contains(" resume_from=333 "), "{summary}");
    }

    #[test]
    fn a_resume_that_carries_on_reads_a_position_at_or_after_the_paused_one() {
        let mut rig = drive_resumed_rig(5_000);
        drive_report_after(&mut rig, 120, 5_120);
        drive_report_after(&mut rig, 250, 5_370);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" resume_from=5120 "), "{summary}");
    }

    #[test]
    fn no_position_after_the_resume_reads_a_dash() {
        let mut rig = drive_resumed_rig(5_000);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" resume_from=- "), "{summary}");
        // A session that never got as far as the resume has none either.
        let mut rig = drive_pause_rig();
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" resume_from=- "), "{summary}");
    }

    #[test]
    fn a_seek_that_lands_without_a_seekdone_reads_seekdone_no() {
        let mut rig = drive_seeked_rig(5_000, 25_300);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" seek=hit "), "{summary}");
        assert!(summary.contains(" seekdone=no "), "{summary}");
    }

    #[test]
    fn a_seekdone_after_the_seek_reads_seekdone_yes() {
        let mut rig = drive_seeked_rig(5_000, 25_300);
        rig.msg("subscribe", r#"{"mediaId":"_m1","seekDone":true}"#);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" seek=hit "), "{summary}");
        assert!(summary.contains(" seekdone=yes "), "{summary}");
    }

    #[test]
    fn a_seekdone_from_before_the_seek_call_is_not_the_seeks() {
        let mut rig = drive_resumed_rig(5_000);
        rig.msg("subscribe", r#"{"mediaId":"_m1","seekDone":true}"#);
        drive_report_after(&mut rig, 100, 5_200);
        rig.tick(DRIVE_STEP_MS);
        rig.msg("seek", r#"{"returnValue":true}"#);
        drive_report_after(&mut rig, 100, 25_300);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" seek=hit "), "{summary}");
        assert!(summary.contains(" seekdone=no "), "{summary}");
    }

    #[test]
    fn an_audioinfo_naming_a_track_before_loadcompleted_reads_that_track() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        rig.msg("subscribe", r#"{"mediaId":"_m1","audioInfo":{"codec":"AAC","channels":2,"track":1}}"#);
        rig.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" audioInfo=seen "), "{summary}");
        assert!(summary.contains(" audio_at_load=1 "), "{summary}");
    }

    #[test]
    fn an_audioinfo_in_the_loadcompleted_message_counts_as_at_load() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        rig.msg("subscribe", r#"{"mediaId":"_m1","audioInfo":{"track":0},"loadCompleted":true}"#);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" audio_at_load=0 "), "{summary}");
    }

    #[test]
    fn an_audioinfo_after_loadcompleted_or_naming_no_track_reads_none() {
        let mut late = Rig::new(Variant::AtLoad, Some(WINDOW));
        late.loaded();
        late.after(200, UriEvent::Tick);
        late.msg("subscribe", r#"{"mediaId":"_m1","audioInfo":{"track":0}}"#);
        let summary = summary_after_back(&mut late);
        assert!(summary.contains(" audioInfo=seen ") && summary.contains(" audio_at_load=none "), "{summary}");

        let mut untracked = Rig::new(Variant::AtLoad, Some(WINDOW));
        untracked.loading();
        untracked.msg("subscribe", r#"{"mediaId":"_m1","audioInfo":{"codec":"AAC","channels":2}}"#);
        untracked.msg("subscribe", r#"{"mediaId":"_m1","loadCompleted":true}"#);
        let summary = summary_after_back(&mut untracked);
        assert!(summary.contains(" audioInfo=seen ") && summary.contains(" audio_at_load=none "), "{summary}");

        let mut silent = Rig::new(Variant::AtLoad, Some(WINDOW));
        silent.loaded();
        let summary = summary_after_back(&mut silent);
        assert!(summary.contains(" audioInfo=none ") && summary.contains(" audio_at_load=none "), "{summary}");
    }

    /// The drive session at the text call: pause, resume, seek (answered, landed) behind it, the
    /// `selectTrack text` sent and answered.
    fn drive_text_selected_rig() -> Rig {
        let mut rig = drive_seeked_rig(5_000, 25_300);
        let fx = rig.tick(DRIVE_STEP_MS);
        assert!(has_log(&fx, "drive selectTrack type=text index=0"), "{fx:?}");
        rig.msg("selectTrack", r#"{"returnValue":true}"#);
        rig
    }

    #[test]
    fn a_typed_text_trackselected_after_the_text_call_reads_text_ev_yes() {
        let mut rig = drive_text_selected_rig();
        rig.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":{"type":"text","index":0}}"#);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" text_ev=yes"), "{summary}");
    }

    #[test]
    fn only_a_typed_text_trackselected_after_the_text_call_counts() {
        // Before the call: the firmware chose a subtitle at load.
        let mut early = Rig::new(Variant::Drive, Some(WINDOW));
        early.playing();
        early.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":{"type":"text","index":0}}"#);
        let mut all = early.tick(DRIVE_FIRST_MS);
        for _ in 0..3 { all.extend(early.tick(DRIVE_STEP_MS)); }
        assert!(has_log(&all, "drive selectTrack type=text index=0"), "{all:?}");
        assert!(summary_after_back(&mut early).contains(" text_ev=no"));

        // An audio selection, an untyped event and a selection of another kind are not a text one.
        let mut rig = drive_text_selected_rig();
        rig.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":{"type":"audio","index":1}}"#);
        rig.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":true}"#);
        rig.msg("subscribe", r#"{"mediaId":"_m1","trackSelected":{"type":"video","index":0}}"#);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" text_ev=no"), "{summary}");
        assert!(summary.contains(" text=sent "), "the call's own answer is graded separately: {summary}");
    }

    fn clock_rig(gaps_ms: &[u64]) -> Rig {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.playing();
        for (n, gap) in std::iter::once(&0).chain(gaps_ms).enumerate() {
            rig.now += gap;
            rig.msg("subscribe", &format!(r#"{{"mediaId":"_m1","currentTime":{}}}"#, 1_000 + 7 * n));
        }
        rig
    }

    #[test]
    fn the_clock_period_is_the_median_gap_between_position_reports() {
        let mut rig = clock_rig(&[250, 250, 300, 1_000, 200]);
        let summary = summary_after_back(&mut rig);
        assert!(summary.contains(" clock_period_ms=250"), "{summary}");
        // Not the mean (400), not the first gap, not the last one.
        let mut rig = clock_rig(&[1_000, 1_000, 1_000, 40, 40]);
        assert!(summary_after_back(&mut rig).contains(" clock_period_ms=1000"));
    }

    #[test]
    fn an_even_number_of_gaps_reads_the_lower_middle_one() {
        let mut rig = clock_rig(&[500, 250]);
        assert!(summary_after_back(&mut rig).contains(" clock_period_ms=250"));
        let mut rig = clock_rig(&[250, 500, 500, 250]);
        assert!(summary_after_back(&mut rig).contains(" clock_period_ms=250"));
    }

    #[test]
    fn fewer_than_two_position_reports_read_a_dash() {
        let mut none = Rig::new(Variant::AtLoad, Some(WINDOW));
        none.playing();
        assert!(summary_after_back(&mut none).contains(" clock_period_ms=-"));
        let mut one = clock_rig(&[]);
        assert!(summary_after_back(&mut one).contains(" clock_period_ms=-"));
    }

    #[test]
    fn a_drive_summary_carries_every_field_the_gate_reads_in_one_line() {
        let mut rig = drive_text_selected_rig();
        rig.msg("subscribe", r#"{"mediaId":"_m1","seekDone":true}"#);
        let summary = summary_after_back(&mut rig);
        for key in ["pause=", "resume=", "resume_from=", "seek=", "seekdone=", "audio=", "audio_at_load=", "text_ev=", "paused_event=", "clock_period_ms=", "end="] {
            assert_eq!(summary.matches(&format!(" {key}")).count(), 1, "{key} must appear exactly once in {summary}");
        }
        assert!(!summary.contains('\n'));
    }

    #[test]
    fn grouped_events_finish_each_continuation_and_stop_before_the_remaining_items() {
        let mut rig = Rig::new(Variant::AtLoad, Some(WINDOW));
        rig.loading();
        let events = [Ev::LoadCompleted, Ev::Playing, Ev::CurrentTime, Ev::Error, Ev::VideoInfo];
        let message = Message {
            media_id: Some(media("_m1")),
            seen: events.iter().map(|ev| Seen { ev: *ev, detail: String::new() }).collect(),
            normalized: events.to_vec(),
            current_time: Some(100),
            error_code: Some(9),
            dims: Some((1280, 720)),
            ..Message::default()
        };
        let fx = rig.feed(UriEvent::Reply { method: Method::Subscribe, ok: true, message });
        let position = |predicate: fn(&UriEffect) -> bool| fx.iter().position(predicate).expect("ordered effect");
        let settled = position(|effect| matches!(effect, UriEffect::Settle(Settle::Started)));
        let play = position(|effect| matches!(effect, UriEffect::Call(UriCall::Play { .. })));
        let unload = position(|effect| matches!(effect, UriEffect::Call(UriCall::Unload { .. })));
        let close = position(|effect| matches!(effect, UriEffect::Close));
        let summary = position(|effect| matches!(effect, UriEffect::Log(line) if line.contains("summary ")));
        assert!(settled < play && play < unload && unload < close && close < summary);
        assert_eq!(settles(&fx, Settle::Started), 1);
        assert_eq!(settles(&fx, Settle::StartFailed), 0);
        assert!(rig.probe.is_done());
        assert!(!rig.probe.plane_open());
        assert_eq!(rig.probe.facts.playing_ms, Some(0));
        assert_eq!(rig.probe.last_time, Some(100));
        assert_eq!(rig.probe.facts.video_seen, None, "events after Error are inert");
        assert!(!fx.iter().any(|effect| matches!(effect, UriEffect::PlaceWindow { .. })));
        for event in [UriEvent::Start, UriEvent::Tick, UriEvent::Back, UriEvent::Closed] {
            assert!(rig.feed(event).is_empty(), "Done emits no repeated close or summary");
        }
    }

    #[test]
    fn video_continuation_precedes_foreground_reply_and_preserves_its_early_return() {
        let (mut rig, _) = acb_loaded(Variant::AcbControl, false);
        let message = parse_event(r#"{"returnValue":true,"mediaId":"_m1","videoInfo":{"width":1280,"height":720,"frameRate":24},"errorCode":9}"#);
        let fx = rig.feed(UriEvent::Reply { method: Method::NotifyForeground, ok: true, message });
        let acb = fx.iter().position(|effect| matches!(effect, UriEffect::Acb { .. })).expect("video starts binding");
        let answer = fx.iter().position(|effect| matches!(effect, UriEffect::Log(line) if line.contains("call notifyForeground ok"))).expect("foreground reply");
        assert!(acb < answer, "the video substep completes before reply handling resumes");
        assert!(!rig.probe.is_done(), "NotifyForeground does not consume service events");
        assert_eq!(rig.probe.facts.video_seen, None);
        assert_eq!(closes(&fx), 0);
        assert!(!logs(&fx).iter().any(|line| line.contains("ev=")));
    }

}
