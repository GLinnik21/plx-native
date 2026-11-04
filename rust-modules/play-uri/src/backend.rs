//! Persistent URI executor. The caller owns route identity, source authorization and backend
//! admission. The native session and every bus request stay behind `Lifecycle`.
use crate::acb_bind::{AcbBind, Event as AcbEvent, Failure as AcbFailure, Fx as AcbFx};
use crate::acb_drive::{self, Descriptor, Executed, Verdict};
use crate::session_io;
use crate::uri_lifecycle::{Event as LifeEvent, Fx as LifeFx, Lifecycle, Phase};
use crate::uri_protocol::{AbrBps, AcbSource, Ev, Message, Method, UriCall};
use plx_base::task::MainThread;
use plx_platform::tv::plane::Refusal;
use plx_platform::tv::sink::{DisplaySink, VP_ACB, VP_EXPORTED};
use plx_platform::tv::uri::{Shape, UriMedia};
use plx_playback_contract::{AttemptId, BackendEvent, BackendKind, Capabilities, CommandId,
    ContentTime, DispatchResult, DriverEvent, DriverSet, EvidenceSource, ExitDisposition,
    Failure, Observation, Observed, Operation, Playback, ReleaseStatus, SourceKey, SourceKind,
    TrackChoice, TrackKind, TrackSelection};
use std::collections::{BTreeMap, VecDeque};

/// A monotonic clock supplied by the runner. The ownership reducer only receives its value.
pub trait MonotonicClock { fn now_ms(&self) -> u64; }

/// The display path the URI executor is composed with (`Binding` is the contract's value). `Acb`
/// is the webOS 4.x plane: the paced bind sequence of `acb_bind` through the one plane owner,
/// released by `UNLOADED` and its completion fence. The executor refuses to start on it unless the
/// display is in ACB mode and the gate (`acb_bind::gate`) passes, and it forwards only a descriptor
/// the service itself reported. Present, and not routed: no qualifier produces an offer for it yet.
pub use plx_playback_contract::uri::Binding;

/// The display path composition chooses for the executor: the ACB plane when the display is in ACB
/// mode and the same gate the executor re-checks at every Start holds (a handle the library
/// confirmed initialized, player type 0; `connectDass` is a developer arm and is not asked for);
/// otherwise nothing: the exported window is not selected here because no source is certified for
/// it either. `initialized` is the sink's `plane_initialized`, read by the caller at the IO boundary.
pub fn product_binding(display_mode: i32, plane: plx_platform::tv::plane::PlaneFacts, initialized: bool) -> Binding {
    let gate = crate::acb_bind::gate(false, display_mode, plane.handle, plane.player_type, false, initialized);
    if gate.ready { Binding::Acb } else { Binding::Unbound }
}

/// No firmware capability is inferred from the display mode or transport response.
pub struct Config {
    pub transport: &'static dyn UriMedia,
    pub display: &'static dyn DisplaySink,
    /// The process's plane owner: grants the one ACB binding and releases it.
    pub plane: &'static plx_platform::tv::plane::PlaneOwner,
    pub clock: Box<dyn MonotonicClock>,
    pub app_id: String,
    pub binding: Binding,
    pub capabilities: Capabilities,
    pub startup_timeout_ms: u64,
    pub release_allowance_ms: u64,
    pub source_ttl_ms: u64,
}

pub use plx_playback_contract::uri::{NativeTrackIndex, UriSource};
use plx_playback_contract::uri::native_index;

struct StagedSource { attempt: AttemptId, key: SourceKey, source: UriSource, deadline_ms: u64 }

#[derive(Clone, Copy)]
enum ControlKind { Transport, Track(TrackSelection), Seek { ack: bool, done: bool } }
#[derive(Clone, Copy)]
struct PendingControl { command: CommandId, operation: Operation, kind: ControlKind,
    deadline_ms: u64 }

/// The active URL and the staged URL are deliberately excluded from `Debug` and all notices.
/// The caller must keep this object until `Release::Released`, or retain it in quarantine.
pub struct UriBackend {
    config: Config,
    lifecycle: Lifecycle,
    staged: Option<StagedSource>,
    active: Option<AttemptId>,
    last_started: Option<AttemptId>,
    url: Option<String>,
    tracks: Vec<NativeTrackIndex>,
    window: bool,
    window_id: Option<String>,
    start_deadline_ms: Option<u64>,
    loaded: bool,
    play_sent: bool,
    played_once: bool,
    subscribed: bool,
    dispatching: Option<PendingControl>,
    controls: BTreeMap<plx_platform::tv::uri::RequestId, PendingControl>,
    pending: VecDeque<DriverEvent>,
    close_reported: bool,
    quarantine_reported: bool,
    acb: AcbBind,
    /// The envelope the service reported for the loaded media: the only descriptor an ACB bind sends.
    acb_source: Option<AcbSource>,
    foreground_sent: bool,
    /// Why the plane owner refused the bind, when it did; the failure reported is typed from it.
    acb_refusal: Option<Refusal>,
    /// The transport state the service last accepted, and whether the plane was told it is paused.
    transport_paused: bool,
    plane_paused: bool,
}

impl UriBackend {
    pub fn new(config: Config) -> Self {
        assert!(!config.capabilities.packet_feed, "URI transport cannot accept packet feeds");
        assert!(!config.capabilities.audio_off && !config.capabilities.subtitle_off,
            "URI transport has no confirmed track-disable operation");
        Self { config, lifecycle: Lifecycle::new(), staged: None, active: None,
            last_started: None, url: None, tracks: Vec::new(),
            window: false, window_id: None,
            start_deadline_ms: None, loaded: false, play_sent: false, played_once: false,
            subscribed: false,
            dispatching: None, controls: BTreeMap::new(),
            pending: VecDeque::new(), close_reported: false, quarantine_reported: false,
            acb: AcbBind::new(false), acb_source: None, foreground_sent: false, acb_refusal: None,
            transport_paused: false, plane_paused: false }
    }

    /// Give exactly one authorized, owned URL to the next matching Start. Superseding a staged
    /// source destroys its old locator; a failed, cancelled or expired Start cannot reuse it.
    pub fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, source: UriSource) -> Result<(), Failure> {
        if !source.has_url() || self.last_started.is_some_and(|last| attempt <= last) {
            return Err(Failure::Stale);
        }
        if !source.valid_track_map() {
            return Err(Failure::Unsupported);
        }
        let deadline_ms = self.config.clock.now_ms().saturating_add(self.config.source_ttl_ms);
        self.staged = Some(StagedSource { attempt, key, source, deadline_ms });
        Ok(())
    }

    /// Cancel an unconsumed locator without changing a live native attempt.
    pub fn cancel_staged(&mut self, attempt: AttemptId) {
        if self.staged.as_ref().is_some_and(|source| source.attempt == attempt) {
            self.staged = None;
        }
    }

    /// Native ownership remains live through close and quarantine. Only the real release effect
    /// makes application exit clean; a deadline never does.
    pub fn exit_disposition(&self) -> ExitDisposition {
        if self.active.is_some() || !self.lifecycle.released() {
            ExitDisposition::PreserveNativeOwnership
        } else {
            ExitDisposition::Clean
        }
    }

    /// The plane remains owned after service Playing until native release: the exported window
    /// until it is destroyed, an ACB binding from the plane owner's grant until the `UNLOADED`
    /// fence. This is an ownership fact, not proof that a picture reached the panel.
    pub fn plane_owned(&self) -> bool {
        (self.window && self.played_once)
            || (self.config.binding == Binding::Acb && self.active.is_some() && self.lifecycle.plane_open())
    }

    /// Whether the active source has an exact native index for this selected source track.
    /// This is source evidence, independent of a general SelectTrack capability flag; a PMS
    /// single-track output cannot switch to a different source track in place.
    pub fn maps_track(&self, selection: TrackSelection) -> bool {
        let enabled = match selection.kind {
            TrackKind::Audio => self.config.capabilities.audio_track,
            TrackKind::Subtitle => self.config.capabilities.subtitle_track,
        };
        self.active.is_some() && enabled && native_index(&self.tracks, selection).is_some()
    }

    fn observe(&mut self, observation: Observation) {
        if let Some(attempt) = self.active {
            self.pending.push_back(DriverEvent::Observation { attempt, observation });
        }
    }

    fn release_notice(&mut self, status: ReleaseStatus) {
        if let Some(attempt) = self.active {
            self.pending.push_back(DriverEvent::Release { attempt, status });
        }
    }

    fn refuse_before_open(&mut self, attempt: AttemptId, reason: Failure) -> DispatchResult {
        // The common reducer requires a resource-owner verdict even if admission failed before
        // registration. Do not claim release while another attempt or native slot is occupied.
        if self.active.is_none() && self.lifecycle.released() {
            self.pending.push_back(DriverEvent::Release { attempt, status: ReleaseStatus::Released });
        }
        DispatchResult::Refused(reason)
    }

    fn start(&mut self, mt: &MainThread, attempt: AttemptId, source: SourceKey,
        requirements: plx_playback_contract::Requirements) -> DispatchResult {
        // A Start consumes its matching authorization even if capability or binding admission
        // fails. A wrong attempt cannot consume another attempt's staged URL.
        let staged = if self.staged.as_ref().is_some_and(|staged| staged.attempt == attempt) {
            self.staged.take()
        } else { None };
        if self.active.is_some() || !self.lifecycle.released() { return self.refuse_before_open(attempt, Failure::Busy); }
        if self.last_started.is_some_and(|last| attempt <= last) { return self.refuse_before_open(attempt, Failure::Stale); }
        if requirements.source == SourceKind::PacketFeed { return self.refuse_before_open(attempt, Failure::Unsupported); }
        if !self.config.capabilities.supports(requirements) { return self.refuse_before_open(attempt, Failure::Unsupported); }
        let Some(staged) = staged else { return self.refuse_before_open(attempt, Failure::Refused); };
        if staged.attempt != attempt || staged.key != source || self.config.clock.now_ms() >= staged.deadline_ms {
            return self.refuse_before_open(attempt, Failure::Stale);
        }
        if !staged.source.can_supply(requirements) {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        if matches!(self.config.binding, Binding::Exported { .. }) && self.config.display.window_mode() != VP_EXPORTED {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        if self.config.binding == Binding::Acb
            && (self.config.display.window_mode() != VP_ACB || !self.acb_gate(mt)) {
            return self.refuse_before_open(attempt, Failure::Unsupported);
        }
        let window_id = match self.config.binding {
            Binding::Exported { canvas } => {
                let Some(id) = session_io::create_exported_window(self.config.display, mt) else {
                    return self.refuse_before_open(attempt, Failure::Backend);
                };
                self.window = true;
                session_io::place_exported_window(self.config.display, mt, canvas, canvas);
                Some(id)
            }
            // 4.x ignores the window option; the service wants the key present (measured sequence).
            Binding::Acb => Some(crate::acb_bind::DUMMY_WINDOW_ID.to_owned()),
            Binding::Unbound => None,
        };
        self.last_started = Some(attempt);
        self.active = Some(attempt);
        let (url, tracks) = staged.source.into_parts();
        self.url = Some(url);
        self.tracks = tracks;
        self.window_id = window_id;
        self.start_deadline_ms = Some(self.config.clock.now_ms().saturating_add(self.config.startup_timeout_ms));
        self.loaded = false;
        self.play_sent = false;
        self.played_once = false;
        self.subscribed = false;
        self.dispatching = None;
        self.controls.clear();
        self.close_reported = false;
        self.quarantine_reported = false;
        self.acb_source = None;
        self.foreground_sent = false;
        self.acb_refusal = None;
        self.transport_paused = false;
        self.plane_paused = false;
        self.lifecycle = Lifecycle::new();
        self.feed(LifeEvent::Open(Shape::Named), mt, self.config.clock.now_ms());
        DispatchResult::Queued
    }

    fn begin_close(&mut self, mt: &MainThread, now_ms: u64) {
        // No ACB verb may run after UNLOADED has been asked for: a stray display step would send
        // PLAYING over a release in flight.
        self.acb.step(AcbEvent::Cancel, now_ms, &mut Vec::new());
        self.acb_source = None;
        self.url = None;
        self.start_deadline_ms = None;
        self.dispatching = None;
        self.controls.clear();
        if self.active.is_none() || matches!(self.lifecycle.phase(), Phase::Closing | Phase::Quarantined) {
            return;
        }
        if let Some(media_id) = self.lifecycle.media_id().cloned() {
            self.feed(LifeEvent::Call(UriCall::Unload { media_id }), mt, now_ms);
        }
        self.feed(LifeEvent::Close { now_ms, allowance_ms: self.config.release_allowance_ms }, mt, now_ms);
    }

    fn maybe_subscribe(&mut self, mt: &MainThread, now_ms: u64) {
        if self.lifecycle.phase() != Phase::Open { return; }
        let Some(media_id) = self.lifecycle.media_id().cloned() else { return; };
        if !self.subscribed {
            self.subscribed = true;
            // The measured ACB sequence announces the media as the foreground connection before
            // it subscribes; a service that refuses it ends the start (see `delivered`).
            if self.config.binding == Binding::Acb && !self.foreground_sent {
                self.foreground_sent = true;
                self.feed(LifeEvent::Call(UriCall::NotifyForeground { media_id: media_id.clone() }), mt, now_ms);
            }
            self.feed(LifeEvent::Call(UriCall::Subscribe { media_id }), mt, now_ms);
        }
    }

    /// A delivered method reply is useful only when its native request still names this exact
    /// command. Callback play state and clock reports remain separate observations.
    fn control_message(&mut self, method: Method, denied: bool, message: &Message,
        mt: &MainThread) -> bool {
        if let Some(request) = message.reply_request {
            if let Some(pending) = self.controls.get(&request).copied() {
                let matches_method = matches!((method, pending.kind),
                    (Method::Play, ControlKind::Transport) if pending.operation == Operation::Play)
                    || matches!((method, pending.kind),
                        (Method::Pause, ControlKind::Transport) if pending.operation == Operation::Pause)
                    || matches!((method, pending.kind),
                        (Method::Seek, ControlKind::Seek { .. })
                            | (Method::SelectTrack, ControlKind::Track(_)));
                if matches_method && denied {
                    self.controls.remove(&request);
                    self.retire_control(request);
                    if let Some(attempt) = self.active {
                        self.pending.push_back(DriverEvent::ControlRefused { attempt,
                            command: pending.command, operation: pending.operation,
                            failure: Failure::Refused });
                    }
                    return true;
                }
                if matches_method && matches!(pending.kind, ControlKind::Transport)
                    && message.accepted() == Some(true) {
                    self.controls.remove(&request);
                    self.retire_control(request);
                    if let Some(attempt) = self.active {
                        self.pending.push_back(DriverEvent::ControlAccepted { attempt,
                            command: pending.command, operation: pending.operation,
                            native_acknowledged: true });
                    }
                    self.mirror_transport(pending.operation, mt);
                }
            }
            if method == Method::Seek && !denied {
                let landed = if let Some(pending) = self.controls.get_mut(&request) {
                    if let ControlKind::Seek { ack, done } = &mut pending.kind {
                        if message.accepted() == Some(true) { *ack = true; }
                        if message.normalized.contains(&Ev::SeekDone) { *done = true; }
                        *ack && *done
                    } else { false }
                } else { false };
                if landed {
                    let pending = self.controls.remove(&request).expect("landed seek is pending");
                    self.retire_control(request);
                    if let Some(attempt) = self.active {
                        let position = message.current_time.and_then(|ms| u64::try_from(ms).ok())
                            .map(|ms| Observed { value: ContentTime::from_millis(ms),
                                from: EvidenceSource::BackendReport });
                        self.pending.push_back(DriverEvent::SeekLanded {
                            attempt, command: pending.command, position,
                        });
                    }
                }
            }
        }
        if message.confirmed_track.is_some() {
            if let Some((kind, index)) = message.selected_track {
                let kind = match kind { "audio" => Some(TrackKind::Audio),
                    "text" => Some(TrackKind::Subtitle), _ => None };
                if let Some(kind) = kind {
                    let matches: Vec<_> = self.controls.iter().filter_map(|(request, pending)| {
                        let ControlKind::Track(selected) = pending.kind else { return None };
                        (selected.kind == kind && native_index(&self.tracks, selected) == Some(index))
                            .then_some(*request)
                    }).collect();
                    if let [request] = matches.as_slice() {
                        let pending = self.controls.remove(request).expect("confirmed track is pending");
                        self.retire_control(*request);
                        let ControlKind::Track(selected) = pending.kind else { unreachable!() };
                        self.observe(Observation::TrackSelected(Observed {
                            value: selected, from: EvidenceSource::BackendReport,
                        }));
                        if let Some(attempt) = self.active {
                            self.pending.push_back(DriverEvent::TrackSettled { attempt,
                                command: pending.command, kind: selected.kind,
                                choice: TrackChoice::Selected(selected.id) });
                        }
                    }
                }
            }
        }
        false
    }

    fn retire_control(&mut self, request: plx_platform::tv::uri::RequestId) {
        // A confirmed request cannot correlate a later selection of the same native index.
        // Keep the lifecycle's request set aligned with the interactive command ledger.
        self.lifecycle.step(LifeEvent::Retire { request }, &mut Vec::new());
    }

    fn delivered(&mut self, method: Method, ok: bool, message: Message,
        mt: &MainThread, now_ms: u64) {
        let denied = !ok || message.accepted() == Some(false);
        let refused_control = self.control_message(method, denied, &message, mt);
        if denied {
            if matches!(method, Method::Play | Method::Pause | Method::Seek | Method::SelectTrack) {
                if refused_control && method == Method::Play { self.play_sent = false; }
                return;
            }
            self.observe(Observation::Failed(Failure::Refused));
            if matches!(method, Method::Register | Method::Load | Method::Subscribe)
                || (method == Method::NotifyForeground && self.config.binding == Binding::Acb) {
                self.begin_close(mt, now_ms);
            }
            return;
        }
        if method == Method::Register {
            self.feed(LifeEvent::Call(UriCall::Load { window_id: self.window_id.clone(), bps: AbrBps::default() }), mt, now_ms);
            return;
        }
        if method == Method::Load {
            // The URL is needed only to build the one Load payload. Never retain it for controls.
            self.url = None;
        }
        // The envelope can arrive in the same message as `loadCompleted`: take it first, so the
        // sequence that `loadCompleted` starts already has the facts it waits for.
        if self.config.binding == Binding::Acb {
            if let Some(source) = message.acb_source.as_ref() {
                self.acb_source = Some(source.clone());
                self.acb_step(AcbEvent::Video(source.video), mt, now_ms);
            }
        }
        let mut ended = false;
        for event in &message.normalized {
            let playback = match event {
                Ev::LoadCompleted => {
                    self.loaded = true;
                    self.start_deadline_ms = None;
                    if self.config.binding == Binding::Acb {
                        if let Some(media_id) = self.lifecycle.media_id().cloned() {
                            self.acb_step(AcbEvent::Loaded { media_id }, mt, now_ms);
                        }
                    }
                    Some(Playback::ReadyAwaitPlay)
                }
                Ev::Playing => {
                    if !self.play_sent || self.controls.values().any(|pending| matches!(pending.kind, ControlKind::Track(_))) {
                        self.observe(Observation::Failed(Failure::Backend));
                        ended = true;
                        None
                    } else {
                        self.played_once = true;
                        Some(Playback::Playing)
                    }
                }
                Ev::Paused => Some(Playback::Paused),
                Ev::EndOfStream => { ended = true; Some(Playback::Ended) },
                Ev::Error => {
                    self.observe(Observation::Failed(Failure::Backend));
                    ended = true;
                    None
                }
                Ev::CurrentTime => {
                    if let Some(ms) = message.current_time.and_then(|ms| u64::try_from(ms).ok()) {
                        self.observe(Observation::Position(Observed {
                            value: ContentTime::from_millis(ms), from: EvidenceSource::BackendReport,
                        }));
                    }
                    None
                }
                Ev::TrackSelected => None,
                _ => None,
            };
            if let Some(value) = playback {
                self.observe(Observation::Playback(Observed { value, from: EvidenceSource::BackendReport }));
            }
        }
        if ended { self.begin_close(mt, now_ms); }
        else { self.maybe_subscribe(mt, now_ms); }
    }

    /// The ACB gate, run through the shared sequence's own first step before anything is opened.
    fn acb_gate(&mut self, mt: &MainThread) -> bool {
        self.acb = AcbBind::new(false);
        self.acb_refusal = None;
        let now_ms = self.config.clock.now_ms();
        self.acb_step(AcbEvent::Start, mt, now_ms);
        self.acb.passed_gate()
    }

    /// Step the shared ACB sequence and run what it asks, each answer fed straight back.
    fn acb_step(&mut self, first: AcbEvent, mt: &MainThread, now_ms: u64) {
        let mut queue = VecDeque::from([first]);
        while let Some(event) = queue.pop_front() {
            let mut out = Vec::new();
            self.acb.step(event, now_ms, &mut out);
            for effect in out {
                match effect {
                    AcbFx::Run { request, step, op, retry } => {
                        if !retry {
                            plx_base::eventlog::log(&format!("uri-acb: request={} step={step:?}", request.number()));
                        }
                        let executed = self.acb_run(&op, mt, now_ms);
                        let completed_ms = self.config.clock.now_ms();
                        queue.push_back(match executed.verdict {
                            Verdict::Invoked(invoked) => AcbEvent::Answered { request, invoked, completed_ms },
                            Verdict::Deferred => AcbEvent::Deferred { request },
                            Verdict::Refused(why) => {
                                plx_base::eventlog::log(&format!("uri-acb: bind not granted (Refused({why:?}))"));
                                self.acb_refusal = Some(why);
                                AcbEvent::Answered { request, invoked: false, completed_ms }
                            }
                        });
                    }
                    AcbFx::WaitingVideo => plx_base::eventlog::log(
                        "uri-acb: waiting for the service's source envelope (real video facts)"),
                    AcbFx::Answered { step, invoked } => plx_base::eventlog::log(
                        &format!("uri-acb: answer step={step:?} invoked={invoked} (not plane proof)")),
                    AcbFx::GatePassed => {}
                    AcbFx::Bound => {
                        plx_base::eventlog::log("uri-acb: bound (a dispatch result, not a picture)");
                        if self.transport_paused { self.mirror_pause(mt); }
                    }
                    AcbFx::Failed(failure) => {
                        // Before a session exists (the gate) the caller refuses the start itself.
                        if self.active.is_some() && self.lifecycle.session().is_some() {
                            let failure = match failure {
                                AcbFailure::Refused => match self.acb_refusal {
                                    Some(Refusal::NoPlane) => Failure::Unavailable,
                                    Some(Refusal::Bound) => Failure::Busy,
                                    Some(Refusal::Quarantined(_)) => Failure::ReleaseUncertain,
                                    None => Failure::Backend,
                                },
                                AcbFailure::Timeout | AcbFailure::VideoTimeout => Failure::TimedOut,
                            };
                            plx_base::eventlog::log(&format!("uri-acb: the bind failed ({failure:?}); closing"));
                            self.observe(Observation::Failed(failure));
                            self.begin_close(mt, now_ms);
                        }
                    }
                }
            }
        }
    }

    /// Perform one step of the sequence. The descriptor is only ever the service's own envelope
    /// for exactly these facts; nothing is built here.
    fn acb_run(&mut self, op: &crate::acb_bind::Op, mt: &MainThread, now_ms: u64) -> Executed {
        let source = self.acb_source.as_ref();
        let mut descriptor = |media_id: &crate::uri_protocol::MediaId, video: crate::uri_protocol::VideoFacts| {
            source.filter(|source| &source.media_id == media_id && source.video == video)
                .map(|source| Descriptor { text: source.text().to_owned(), label: "observed-envelope" })
                .ok_or("the service reported no source envelope for these video facts")
        };
        let executed = acb_drive::execute(op, mt, self.config.display, self.config.plane, &mut descriptor,
            &mut |_, line| plx_base::eventlog::log(&format!("uri-acb: {line}")));
        if let (true, Some(session)) = (executed.touched, self.lifecycle.session()) {
            self.feed(LifeEvent::AcbTouched { session }, mt, now_ms);
        }
        executed
    }

    /// Mirror an accepted transport pause or resume to the plane, as Feed does (Kodi parity: the
    /// pipeline's Pause/Play alone leave the ACB play state stale). Only once the bind is complete.
    fn mirror_transport(&mut self, operation: Operation, mt: &MainThread) {
        match operation {
            Operation::Pause => {
                self.transport_paused = true;
                self.mirror_pause(mt);
            }
            Operation::Play => {
                self.transport_paused = false;
                if self.plane_paused && self.acb.bound() {
                    self.plane_paused = false;
                    // SAFETY: main thread, witnessed by `mt`; the bind completed.
                    unsafe { self.config.display.plane_resume(mt) };
                }
            }
            _ => {}
        }
    }

    fn mirror_pause(&mut self, mt: &MainThread) {
        if self.config.binding == Binding::Acb && self.acb.bound() && !self.plane_paused {
            self.plane_paused = true;
            // SAFETY: main thread, witnessed by `mt`; the bind completed.
            unsafe { self.config.display.plane_pause(mt) };
        }
    }

    fn feed(&mut self, first: LifeEvent, mt: &MainThread, now_ms: u64) {
        let mut queue = VecDeque::from([first]);
        while let Some(event) = queue.pop_front() {
            let mut effects = Vec::new();
            self.lifecycle.step(event, &mut effects);
            for effect in effects {
                if let LifeFx::Call { request, call, .. } = &effect {
                    if let Some(pending) = self.dispatching {
                        let matches = matches!((call, pending.kind),
                            (UriCall::Play { .. }, ControlKind::Transport)
                                | (UriCall::Pause { .. }, ControlKind::Transport)
                                | (UriCall::Seek { .. }, ControlKind::Seek { .. })
                                | (UriCall::SelectTrack { .. }, ControlKind::Track(_)));
                        if matches { self.controls.insert(*request, pending); }
                    }
                }
                if let Some(answer) = session_io::transport_answer(
                    &effect, self.config.transport, self.url.as_deref().unwrap_or(""), &self.config.app_id,
                ) {
                    queue.push_back(answer);
                    continue;
                }
                match effect {
                    LifeFx::Deliver { method, ok, message } => self.delivered(method, ok, message, mt, now_ms),
                    LifeFx::Released => {
                        self.release_notice(ReleaseStatus::Released);
                        if self.window { session_io::destroy_exported_window(self.config.display, mt); }
                        self.window = false;
                        self.window_id = None;
                        self.active = None;
                        self.url = None;
                        self.acb_source = None;
                        self.tracks.clear();
                        self.dispatching = None;
                        self.controls.clear();
                        self.loaded = false;
                        self.play_sent = false;
                        self.played_once = false;
                        self.subscribed = false;
                    }
                    LifeFx::Fault => {
                        self.observe(Observation::Failed(Failure::ReleaseUncertain));
                    }
                    LifeFx::UnloadAcb { session } => {
                        // However the close began (a Stop, the service ending, an unsolicited bus
                        // release), no ACB verb may follow UNLOADED: a stray display step would
                        // send PLAYING over a release in flight.
                        self.acb.step(AcbEvent::Cancel, now_ms, &mut Vec::new());
                        self.acb_source = None;
                        let plane = self.config.plane.unload(self.config.display, mt);
                        queue.push_back(LifeEvent::AcbUnloadDispatched { session, plane });
                    }
                    LifeFx::AcbReleased | LifeFx::AcbQuarantined(_) | LifeFx::Open { .. } | LifeFx::Call { .. } | LifeFx::Close { .. } => {}
                }
            }
            if self.active.is_some() {
                if self.lifecycle.phase() == Phase::Closing && !self.close_reported {
                    self.close_reported = true;
                    self.release_notice(ReleaseStatus::Closing);
                }
                if self.lifecycle.phase() == Phase::Quarantined && !self.quarantine_reported {
                    self.quarantine_reported = true;
                    self.release_notice(ReleaseStatus::Quarantined);
                }
            }
        }
    }
}

impl UriBackend {
    fn dispatch(&mut self, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if let Operation::Start { source, requirements } = operation {
            return self.start(mt, attempt, source, requirements);
        }
        if self.active != Some(attempt) { return DispatchResult::Refused(Failure::Stale); }
        let now_ms = self.config.clock.now_ms();
        if matches!(operation, Operation::Stop(_)) {
            if self.lifecycle.phase() == Phase::Quarantined {
                return DispatchResult::Refused(Failure::ReleaseUncertain);
            }
            self.begin_close(mt, now_ms);
            return DispatchResult::Queued;
        }
        if self.lifecycle.phase() != Phase::Open || !self.loaded {
            return DispatchResult::Refused(Failure::Busy);
        }
        let Some(media_id) = self.lifecycle.media_id().cloned() else { return DispatchResult::Refused(Failure::Busy); };
        let (call, control) = match operation {
            Operation::Play if self.config.capabilities.pause => {
                if self.controls.values().any(|pending| matches!(pending.kind, ControlKind::Track(_))) {
                    return DispatchResult::Refused(Failure::Busy);
                }
                self.play_sent = true;
                (UriCall::Play { media_id }, Some(ControlKind::Transport))
            }
            Operation::Pause if self.config.capabilities.pause =>
                (UriCall::Pause { media_id }, Some(ControlKind::Transport)),
            Operation::Seek(time) if self.config.capabilities.seek => {
                let Ok(position) = i64::try_from(time.as_millis()) else { return DispatchResult::Refused(Failure::Unsupported); };
                (UriCall::Seek { media_id, position }, Some(ControlKind::Seek { ack: false, done: false }))
            }
            Operation::SelectTrack { kind, choice } => {
                let enabled = match kind {
                    TrackKind::Audio => self.config.capabilities.audio_track,
                    TrackKind::Subtitle => self.config.capabilities.subtitle_track,
                };
                if !enabled { return DispatchResult::Refused(Failure::Unsupported); }
                let TrackChoice::Selected(id) = choice else {
                    return DispatchResult::Refused(Failure::Unsupported);
                };
                let track = TrackSelection { kind, id };
                if self.controls.values().any(|pending| matches!(pending.kind,
                    ControlKind::Track(previous) if previous.kind == kind)) {
                    return DispatchResult::Refused(Failure::Busy);
                }
                let Some(index) = native_index(&self.tracks, track) else {
                    return DispatchResult::Refused(Failure::Unsupported);
                };
                (UriCall::SelectTrack { media_id, kind: match kind { TrackKind::Audio => "audio", TrackKind::Subtitle => "text" }, index }, Some(ControlKind::Track(track)))
            }
            _ => return DispatchResult::Refused(Failure::Unsupported),
        };
        self.dispatching = control.map(|kind| PendingControl { command, operation, kind,
            deadline_ms: now_ms.saturating_add(self.config.startup_timeout_ms) });
        self.feed(LifeEvent::Call(call), mt, now_ms);
        self.dispatching = None;
        DispatchResult::Queued
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<DriverEvent> {
        let now_ms = self.config.clock.now_ms();
        if self.staged.as_ref().is_some_and(|source| now_ms >= source.deadline_ms) {
            self.staged = None;
        }
        if let Some(session) = self.lifecycle.poll_session() {
            match self.config.transport.poll(session) {
                Ok(poll) => {
                    for reply in poll.replies { self.feed(LifeEvent::Reply(reply), mt, now_ms); }
                    self.feed(LifeEvent::Status { session, status: poll.close }, mt, now_ms);
                }
                Err(_) => self.feed(LifeEvent::PollFailed { session }, mt, now_ms),
            }
        }
        self.feed(LifeEvent::Tick { now_ms }, mt, now_ms);
        if let Some(session) = self.lifecycle.session().filter(|session| self.lifecycle.acb_fence_due(*session)) {
            // UNLOADED is in flight: drain the firmware's completions into the plane owner, and
            // let the lifecycle release (or quarantine) on what it says.
            let plane = self.config.plane.pump(self.config.display, mt);
            self.feed(LifeEvent::AcbPlane { session, plane }, mt, now_ms);
        }
        self.acb_step(AcbEvent::Tick, mt, now_ms);
        if self.start_deadline_ms.is_some_and(|deadline| now_ms >= deadline)
            && matches!(self.lifecycle.phase(), Phase::Opening | Phase::Open) {
            self.observe(Observation::Failed(Failure::TimedOut));
            self.begin_close(mt, now_ms);
        }
        if self.lifecycle.phase() == Phase::Open {
            let expired: Vec<_> = self.controls.iter().filter_map(|(request, pending)|
                (now_ms >= pending.deadline_ms).then_some(*request)).collect();
            for request in expired {
                let Some(pending) = self.controls.remove(&request) else { continue };
                self.retire_control(request);
                if let Some(attempt) = self.active {
                    self.pending.push_back(DriverEvent::ControlRefused { attempt,
                        command: pending.command, operation: pending.operation,
                        failure: Failure::TimedOut });
                }
                if pending.operation == Operation::Play { self.play_sent = false; }
            }
        }
        self.pending.drain(..).collect()
    }
}

/// A URI-only set composes with the common runner directly. A larger application may wrap it
/// alongside Feed; neither backend can claim the other's attempt or staged locator.
impl DriverSet for UriBackend {
    type Context = MainThread;

    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
        (backend == BackendKind::Uri).then_some(self.config.capabilities)
    }

    fn dispatch(&mut self, backend: BackendKind, mt: &MainThread, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult {
        if backend != BackendKind::Uri { return DispatchResult::Refused(Failure::Unavailable); }
        UriBackend::dispatch(self, mt, attempt, command, operation)
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<BackendEvent> {
        UriBackend::poll(self, mt).into_iter().map(|event| BackendEvent {
            backend: BackendKind::Uri, event,
        }).collect()
    }

    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey) {
        if backend == BackendKind::Uri
            && self.staged.as_ref().is_some_and(|staged| staged.attempt == attempt && staged.key == key) {
            self.staged = None;
        }
    }

    fn exit_disposition(&self) -> ExitDisposition { UriBackend::exit_disposition(self) }
}

#[cfg(test)]
mod tests {
    #[test]
    fn composition_selects_the_acb_plane_only_when_the_gate_holds() {
        use plx_platform::tv::plane::PlaneFacts;
        use plx_platform::tv::sink::{VP_ACB, VP_EXPORTED, VP_NONE};
        let facts = PlaneFacts { handle: true, player_type: 0 };
        assert_eq!(product_binding(VP_ACB, facts, true), Binding::Acb);
        assert_eq!(product_binding(VP_ACB, facts, false), Binding::Unbound, "a created handle is not an initialized one");
        assert_eq!(product_binding(VP_ACB, PlaneFacts { handle: true, player_type: 10 }, true), Binding::Unbound);
        assert_eq!(product_binding(VP_ACB, PlaneFacts::ABSENT, true), Binding::Unbound);
        assert_eq!(product_binding(VP_EXPORTED, facts, true), Binding::Unbound);
        assert_eq!(product_binding(VP_NONE, facts, true), Binding::Unbound);
    }

    use super::*;
    use plx_platform::tv::sink::{NoSink, PlaneUnload, ScriptedPlaneSink};
    use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, UriPoll, UriReply};
    use plx_playback_contract::{BackendState, Event as ContractEvent, Fidelity,
        Fx as ContractFx, InitialPlayback, Intent, Requirements, SourceKind, StopReason};
    use std::sync::{atomic::{AtomicU64, Ordering}, Mutex};

    const CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false,
        packet_feed: false, pause: true,
        seek: true, audio_track: true, subtitle_track: true, audio_off: false, subtitle_off: false };
    const REQ: Requirements = Requirements { source: SourceKind::ProgressiveUri,
        video: Fidelity::Exact, audio: Fidelity::Exact, audio_track: TrackChoice::Unspecified,
        subtitle_track: TrackChoice::Unspecified };
    const INITIAL: InitialPlayback = InitialPlayback { position: None, paused: false };
    const KEY: SourceKey = SourceKey::new(17);
    const A: AttemptId = AttemptId::new(7, 1);
    const B: AttemptId = AttemptId::new(7, 2);
    const URL: &str = "https://private.invalid/file?X-Plex-Token=SECRET";

    struct Clock(std::sync::Arc<AtomicU64>);
    impl MonotonicClock for Clock { fn now_ms(&self) -> u64 { self.0.load(Ordering::Relaxed) } }
    struct BusState { session: SessionId, serial: u64,
        calls: Vec<(&'static str, RequestId, String)>, replies: VecDeque<UriReply>,
        close: CloseStatus, hold: bool, refuse_open: bool, fail_poll: bool }
    struct Bus(Mutex<BusState>);
    impl Bus {
        fn new() -> Self { Self(Mutex::new(BusState { session: SessionId::new(0),
            serial: 0, calls: Vec::new(), replies: VecDeque::new(),
            close: CloseStatus::Released, hold: false, refuse_open: false, fail_poll: false })) }
        fn say(&self, method: &'static str, payload: &str) {
            let mut state = self.0.lock().unwrap();
            let request = if method == "register" { RequestId::new(0) } else {
                state.calls.iter().rev().find(|(name, _, _)| *name == method)
                    .map(|(_, request, _)| *request).expect("call was sent") };
            let session = state.session;
            state.replies.push_back(UriReply { session, request, method,
                payload: payload.into(), ok: true });
        }
        fn calls(&self, method: &str) -> usize {
            self.0.lock().unwrap().calls.iter().filter(|(name, _, _)| *name == method).count()
        }
    }
    impl UriMedia for Bus {
        fn open(&self, _shape: Shape) -> Result<SessionId, String> {
            let mut state = self.0.lock().unwrap();
            if state.refuse_open { return Err("refused".into()); }
            state.serial += 1;
            state.session = SessionId::new(state.serial);
            state.close = CloseStatus::Open;
            Ok(state.session)
        }
        fn call(&self, session: SessionId, request: RequestId, method: &'static str,
            payload: &str) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(session, state.session);
            state.calls.push((method, request, payload.into()));
            Ok(())
        }
        fn poll(&self, session: SessionId) -> Result<UriPoll, String> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(session, state.session);
            if state.fail_poll { return Err("poll failed".into()); }
            let close = state.close;
            Ok(UriPoll { replies: state.replies.drain(..).collect(), close })
        }
        fn close(&self, session: SessionId) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            assert_eq!(session, state.session);
            state.close = if state.hold { CloseStatus::Closing } else { CloseStatus::Released };
            Ok(())
        }
    }
    struct Rig { driver: UriBackend, clock: std::sync::Arc<AtomicU64>, bus: &'static Bus,
        mt: MainThread, command: plx_playback_contract::CommandId }
    impl Rig {
        fn new() -> Self {
            Self::with(Binding::Unbound, Box::leak(Box::new(NoSink)),
                Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new())))
        }
        /// An ACB-bound backend over a scripted sink and its own plane owner (booted as the
        /// measured sequence needs: a handle, player type 0).
        fn acb() -> (Self, &'static ScriptedPlaneSink, &'static plx_platform::tv::plane::PlaneOwner) {
            let sink: &'static ScriptedPlaneSink =
                Box::leak(Box::new(ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 })));
            let plane: &'static plx_platform::tv::plane::PlaneOwner =
                Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
            plane.boot(plx_platform::tv::plane::PlaneFacts { handle: true, player_type: 0 });
            (Self::with(Binding::Acb, sink, plane), sink, plane)
        }
        fn with(binding: Binding, display: &'static dyn DisplaySink,
            plane: &'static plx_platform::tv::plane::PlaneOwner) -> Self {
            let clock = std::sync::Arc::new(AtomicU64::new(0));
            let bus = Box::leak(Box::new(Bus::new()));
            let driver = UriBackend::new(Config { transport: bus, display, plane,
                clock: Box::new(Clock(clock.clone())),
                app_id: "app.test".into(), binding, capabilities: CAPS,
                startup_timeout_ms: 500, release_allowance_ms: 100, source_ttl_ms: 50 });
            let mut state = BackendState::new();
            let mut fx = Vec::new();
            state.step(ContractEvent::Intent(Intent::Start {
                backend: BackendKind::Uri, attempt: A, source: KEY, requirements: REQ,
                initial: INITIAL }), Some(CAPS), &mut fx);
            let [ContractFx::Dispatch { command, .. }] = fx.as_slice() else { panic!("dispatch") };
            Self { driver, clock, bus, mt: unsafe { MainThread::assume() }, command: *command }
        }
        fn start(&mut self, attempt: AttemptId) -> DispatchResult {
            self.driver.stage_source(attempt, KEY, UriSource::new(URL.into(), vec![
                NativeTrackIndex { selection: TrackSelection { kind: TrackKind::Audio, id: 3 }, index: 3 },
            ])).unwrap();
            self.driver.dispatch(&self.mt, attempt, self.command,
                Operation::Start { source: KEY, requirements: REQ })
        }
        fn opened(&mut self) {
            assert_eq!(self.start(A), DispatchResult::Queued);
            self.bus.say("register", "");
            assert!(self.driver.poll(&self.mt).is_empty());
            assert_eq!(self.bus.calls("load"), 1);
            self.bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
            assert_eq!(self.driver.poll(&self.mt), [DriverEvent::Observation { attempt: A,
                observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                    from: EvidenceSource::BackendReport }) }]);
            assert_eq!(self.bus.calls("subscribe"), 1);
            assert_eq!(self.bus.calls("play"), 0);
        }
    }

    #[test]
    fn native_clock_and_ack_never_prove_a_picture_and_controls_remain_addressed() {
        let mut rig = Rig::new(); rig.opened();
        assert!(rig.driver.url.is_none(), "load consumed the private locator");
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true,"currentTime":123}"#);
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::Playing,
                from: EvidenceSource::BackendReport }) }));
        assert!(!notices.iter().any(|event| matches!(event, DriverEvent::Observation {
            observation: Observation::Presented, .. })));
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command, Operation::Pause), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::Paused,
                from: EvidenceSource::BackendReport }) }));
    }

    #[test]
    fn track_event_before_ack_requires_native_correlation() {
        let mut rig = Rig::new(); rig.opened();
        let track = TrackSelection { kind: TrackKind::Audio, id: 3 };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: track.kind, choice: TrackChoice::Selected(track.id) }), DispatchResult::Queued);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::TrackSelected(Observed { value: track,
                from: EvidenceSource::BackendReport }) },
            DriverEvent::TrackSettled { attempt: A, command: rig.command,
                kind: TrackKind::Audio, choice: TrackChoice::Selected(track.id) }]);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: track.kind, choice: TrackChoice::Selected(track.id) }),
            DispatchResult::Queued);
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::TrackSelected(Observed { value: track,
                from: EvidenceSource::BackendReport }) },
            DriverEvent::TrackSettled { attempt: A, command: rig.command,
                kind: TrackKind::Audio, choice: TrackChoice::Selected(track.id) }],
            "a retired native request cannot make a repeated selection ambiguous");
    }

    #[test]
    fn quarantine_blocks_successor_until_release_and_expired_source_is_not_reused() {
        let mut rig = Rig::new(); rig.opened();
        rig.bus.0.lock().unwrap().hold = true;
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)), DispatchResult::Queued);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }]);
        rig.driver.stage_source(B, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Busy));
        rig.clock.store(101, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Quarantined }));
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }]);
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Refused),
            "a refused Start consumed B's authorization while A was closing");
    }

    #[test]
    fn immediate_registration_refusal_still_reports_confirmed_release() {
        let mut rig = Rig::new();
        rig.bus.0.lock().unwrap().refuse_open = true;
        assert_eq!(rig.start(A), DispatchResult::Queued);
        assert_eq!(rig.driver.poll(&rig.mt), [
            DriverEvent::Observation { attempt: A, observation: Observation::Failed(Failure::Refused) },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Released },
        ]);
    }

    #[test]
    fn terminal_event_and_release_in_one_poll_keep_their_order_and_fence_old_bus_replies() {
        let mut rig = Rig::new(); rig.opened();
        let old_session = rig.bus.0.lock().unwrap().session;
        rig.bus.say("subscribe", r#"{"mediaId":"m1","endOfStream":true}"#);
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        assert_eq!(rig.driver.poll(&rig.mt), [
            DriverEvent::Observation { attempt: A, observation: Observation::Playback(Observed {
                value: Playback::Ended, from: EvidenceSource::BackendReport }) },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Closing },
            DriverEvent::Release { attempt: A, status: ReleaseStatus::Released },
        ]);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }],
            "even a duplicate direct Start gets a proven empty-owner verdict");
        assert_eq!(rig.start(B), DispatchResult::Queued);
        assert_ne!(rig.bus.0.lock().unwrap().session, old_session,
            "the transport assigns a new native session across lifecycle resets");
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session: old_session,
            request: RequestId::new(0), method: "register", payload: String::new(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.calls("load"), 1, "the previous native generation cannot load the new source");
    }

    #[test]
    fn cancelled_or_expired_staged_locator_cannot_start_an_attempt() {
        let mut rig = Rig::new();
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.driver.cancel_staged(A);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Refused));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }], "a pre-open refusal has a proven empty owner");
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.clock.store(50, Ordering::Relaxed);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }]);
        assert!(rig.driver.staged.is_none());
        rig.driver.stage_source(B, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.clock.store(100, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert!(rig.driver.staged.is_none(), "poll drops an unused authorization at its deadline");
    }

    #[test]
    fn capability_and_binding_refusals_both_destroy_the_matching_url() {
        let mut rig = Rig::new();
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        let hls = Requirements { source: SourceKind::HlsUri, ..REQ };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: hls }), DispatchResult::Refused(Failure::Unsupported));
        assert!(rig.driver.staged.is_none());
        rig.driver.poll(&rig.mt);
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        rig.driver.config.binding = Binding::Exported { canvas: (1920, 1080) };
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Unsupported));
        assert!(rig.driver.staged.is_none());
        assert_eq!(rig.bus.0.lock().unwrap().serial, 0);
    }

    #[test]
    fn ready_never_plays_or_selects_and_explicit_track_confirmation_gates_play() {
        let mut rig = Rig::new();
        let audio = TrackSelection { kind: TrackKind::Audio, id: 42 };
        let subtitle = TrackSelection { kind: TrackKind::Subtitle, id: 77 };
        let required = Requirements {
            audio_track: TrackChoice::Selected(audio.id),
            subtitle_track: TrackChoice::Selected(subtitle.id), ..REQ
        };
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), vec![
            NativeTrackIndex { selection: audio, index: 3 },
            NativeTrackIndex { selection: subtitle, index: 5 },
        ])).unwrap();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: required }), DispatchResult::Queued);
        rig.bus.say("register", "");
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("load", r#"{"returnValue":true,"mediaId":"m1","loadCompleted":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::Playback(Observed { value: Playback::ReadyAwaitPlay,
                from: EvidenceSource::BackendReport }) }]);
        assert_eq!(rig.bus.calls("selectTrack"), 0);
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause),
            DispatchResult::Queued, "a paused initial intent stays in common ownership");
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(audio.id) }),
            DispatchResult::Queued);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Subtitle, choice: TrackChoice::Selected(subtitle.id) }),
            DispatchResult::Queued);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play),
            DispatchResult::Refused(Failure::Busy));
        let (session, audio_request, subtitle_request) = {
            let state = rig.bus.0.lock().unwrap();
            let mut requests = state.calls.iter().filter(|(method, _, _)| *method == "selectTrack");
            (state.session, requests.next().unwrap().1, requests.next().unwrap().1)
        };
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: RequestId::new(999), method: "selectTrack",
            payload: r#"{"returnValue":true,"mediaId":"m1"}"#.into(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        for (request, kind, index, selected) in [
            (audio_request, "audio", 3, audio), (subtitle_request, "text", 5, subtitle)
        ] {
            rig.bus.0.lock().unwrap().replies.push_back(UriReply { session, request,
                method: "selectTrack", payload: r#"{"returnValue":true,"mediaId":"m1"}"#.into(),
                ok: true });
            assert!(rig.driver.poll(&rig.mt).is_empty(), "ACK cannot confirm selection");
            assert_eq!(rig.bus.calls("play"), 0);
            rig.bus.say("subscribe", &format!(
                r#"{{"mediaId":"m1","trackSelected":{{"type":"{kind}","index":{index}}}}}"#));
            assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
                observation: Observation::TrackSelected(Observed { value: selected,
                    from: EvidenceSource::BackendReport }) },
                DriverEvent::TrackSettled { attempt: A, command: rig.command,
                    kind: selected.kind, choice: TrackChoice::Selected(selected.id) }]);
        }
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play),
            DispatchResult::Queued);
        assert_eq!(rig.bus.calls("play"), 1);
    }

    #[test]
    fn missing_original_mapping_refuses_before_open_and_reducer_returns_to_vacant() {
        let mut rig = Rig::new();
        let required = Requirements { audio_track: TrackChoice::Selected(42), ..REQ };
        rig.driver.stage_source(A, KEY, UriSource::new(URL.into(), Vec::new())).unwrap();
        let mut state = BackendState::new();
        let mut effects = Vec::new();
        state.step(ContractEvent::Intent(Intent::Start {
            backend: BackendKind::Uri, attempt: A, source: KEY, requirements: required,
            initial: INITIAL,
        }), Some(CAPS), &mut effects);
        let [ContractFx::Dispatch { command, operation, .. }] = effects.as_slice() else { panic!("dispatch") };
        let (command, operation) = (*command, *operation);
        let result = rig.driver.dispatch(&rig.mt, A, command, operation);
        assert_eq!(result, DispatchResult::Refused(Failure::Unsupported));
        assert!(rig.driver.staged.is_none(), "failed admission destroys the authorized URL");
        effects.clear();
        state.step(ContractEvent::CommandAnswer { backend: BackendKind::Uri,
            attempt: A, command, result }, None, &mut effects);
        assert_eq!(state.snapshot().phase, plx_playback_contract::Phase::Releasing);
        for notice in rig.driver.poll(&rig.mt) {
            effects.clear();
            state.step(BackendEvent { backend: BackendKind::Uri, event: notice }.into(), None, &mut effects);
        }
        assert_eq!(state.snapshot().phase, plx_playback_contract::Phase::Vacant);
        assert_eq!(rig.bus.0.lock().unwrap().serial, 0, "no native registration was attempted");
    }

    #[test]
    fn refused_track_reports_exact_command_and_waits_for_owner_stop() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(3) }),
            DispatchResult::Queued);
        let reply = {
            let state = rig.bus.0.lock().unwrap();
            let (_, request, _) = state.calls.iter().rev().find(|(method, _, _)| *method == "selectTrack").unwrap();
            UriReply { session: state.session, request: *request,
                method: "selectTrack", payload: "refused".into(), ok: false }
        };
        rig.bus.0.lock().unwrap().replies.push_back(reply);
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::ControlRefused { attempt: A,
            command: rig.command,
            operation: Operation::SelectTrack { kind: TrackKind::Audio,
                choice: TrackChoice::Selected(3) }, failure: Failure::Refused }));
        assert!(!notices.iter().any(|event| matches!(event, DriverEvent::Release { .. })),
            "the common startup owner decides if this was mandatory or interactive");
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Stop(StopReason::Replace)), DispatchResult::Queued);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }));
    }

    #[test]
    fn later_pause_refusal_is_correlated_and_does_not_end_playback() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1","playing":true}"#);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::ControlAccepted { attempt: A,
            command: rig.command, operation: Operation::Play, native_acknowledged: true }));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        let (session, old_request) = {
            let state = rig.bus.0.lock().unwrap();
            (state.session, state.calls.iter().rev().find(|(method, _, _)| *method == "pause").unwrap().1)
        };
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: old_request, method: "pause", payload: "refused".into(), ok: false });
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlRefused { attempt: A,
            command: rig.command, operation: Operation::Pause, failure: Failure::Refused }]);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);

        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: old_request, method: "pause", payload: "refused".into(), ok: false });
        assert!(rig.driver.poll(&rig.mt).is_empty(), "retired native request cannot refuse a retry");
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlAccepted { attempt: A,
            command: rig.command, operation: Operation::Pause, native_acknowledged: true },
            DriverEvent::Observation { attempt: A, observation: Observation::Playback(Observed {
                value: Playback::Paused, from: EvidenceSource::BackendReport }) }]);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":false,"mediaId":"m1"}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::ControlRefused { attempt: A,
            command: rig.command, operation: Operation::Play, failure: Failure::Refused }]);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership,
            "a later Play refusal leaves the live attempt available for another viewer press");
    }

    #[test]
    fn seek_landing_requires_exact_request_and_ack_and_off_is_unsupported() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Subtitle, choice: TrackChoice::Off }),
            DispatchResult::Refused(Failure::Unsupported));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Seek(ContentTime::from_millis(5000))), DispatchResult::Queued);
        let (session, request) = {
            let state = rig.bus.0.lock().unwrap();
            (state.session, state.calls.iter().rev().find(|(method, _, _)| *method == "seek").unwrap().1)
        };
        rig.bus.say("subscribe", r#"{"mediaId":"m1","seekDone":true,"currentTime":5000}"#);
        assert!(!rig.driver.poll(&rig.mt).iter().any(|event| matches!(event, DriverEvent::SeekLanded { .. })));
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session,
            request: RequestId::new(999), method: "seek",
            payload: r#"{"returnValue":true,"mediaId":"m1","seekDone":true}"#.into(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session, request,
            method: "seek", payload: r#"{"returnValue":true,"mediaId":"m1","seekDone":true,"currentTime":5000}"#.into(),
            ok: true });
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::SeekLanded { attempt: A,
            command: rig.command, position: Some(Observed {
                value: ContentTime::from_millis(5000), from: EvidenceSource::BackendReport }) },
            DriverEvent::Observation { attempt: A, observation: Observation::Position(Observed {
                value: ContentTime::from_millis(5000), from: EvidenceSource::BackendReport }) }],
            "the same native reply both lands the addressed seek and reports sparse position");
        assert_eq!(rig.bus.calls("play"), 0, "seeking never chooses playback policy");
    }

    #[test]
    fn acknowledged_track_without_confirmation_times_out_and_waits_for_owner_stop() {
        let mut rig = Rig::new(); rig.opened();
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::SelectTrack { kind: TrackKind::Audio, choice: TrackChoice::Selected(3) }),
            DispatchResult::Queued);
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.clock.store(500, Ordering::Relaxed);
        let notices = rig.driver.poll(&rig.mt);
        assert!(notices.contains(&DriverEvent::ControlRefused { attempt: A,
            command: rig.command,
            operation: Operation::SelectTrack { kind: TrackKind::Audio,
                choice: TrackChoice::Selected(3) }, failure: Failure::TimedOut }));
        assert!(!notices.iter().any(|event| matches!(event, DriverEvent::Release { .. })),
            "the common startup owner decides if this was mandatory or interactive");
        assert_eq!(rig.bus.calls("play"), 0);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Stop(StopReason::Replace)), DispatchResult::Queued);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }));
    }

    #[test]
    fn startup_deadline_stops_at_ready_and_exit_waits_for_real_release() {
        let mut rig = Rig::new();
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
        rig.opened();
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        rig.clock.store(501, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).is_empty(), "common owner may stay paused at ready");
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Stop(StopReason::Exit)), DispatchResult::Queued);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
    }

    // ---- the ACB binding ----

    const ENVELOPE: &str = r#"{"mediaId":"m1","sourceInfo":{"context":"m1","video":{"width":1280,"height":720,"frameRate":24}},"loadCompleted":true}"#;

    impl Rig {
        fn at(&self, ms: u64) { self.clock.store(ms, Ordering::Relaxed); }
        /// Started, registered, loaded, subscribed: the service has not said `loadCompleted` yet.
        fn acb_loading(&mut self) {
            assert_eq!(self.start(A), DispatchResult::Queued);
            self.bus.say("register", "");
            self.driver.poll(&self.mt);
            self.bus.say("load", r#"{"returnValue":true,"mediaId":"m1"}"#);
            self.driver.poll(&self.mt);
        }
        fn bus_methods(&self) -> Vec<&'static str> {
            self.bus.0.lock().unwrap().calls.iter().map(|(name, _, _)| *name).collect()
        }
        /// Run the paced sequence to its end; `loadCompleted` (with the envelope) is at t=0.
        fn acb_bound(&mut self) {
            self.acb_loading();
            self.bus.say("subscribe", ENVELOPE);
            self.driver.poll(&self.mt);
            for t in [200, 400, 600, 900] { self.at(t); self.driver.poll(&self.mt); }
        }
    }

    #[test]
    fn acb_start_refuses_unless_every_gate_fact_holds() {
        let boot = |handle, player_type| -> &'static plx_platform::tv::plane::PlaneOwner {
            let plane = Box::leak(Box::new(plx_platform::tv::plane::PlaneOwner::new()));
            plane.boot(plx_platform::tv::plane::PlaneFacts { handle, player_type });
            plane
        };
        let scripted = || -> &'static ScriptedPlaneSink {
            Box::leak(Box::new(ScriptedPlaneSink::new(PlaneUnload { rv: 1, task: 5 })))
        };
        let unconfirmed = scripted();
        unconfirmed.initialized.store(false, Ordering::Relaxed);
        let cases: [(&str, &'static ScriptedPlaneSink, &'static dyn DisplaySink, &'static plx_platform::tv::plane::PlaneOwner); 4] = {
            let (a, b, c) = (scripted(), scripted(), scripted());
            [("a created handle is not an initialized one", unconfirmed, unconfirmed, boot(true, 0)),
             ("the measured sequence initialized player type 0", a, a, boot(true, 10)),
             ("no handle", b, b, boot(false, 0)),
             ("not the ACB display", c, Box::leak(Box::new(NoSink)), boot(true, 0))]
        };
        for (case, sink, display, plane) in cases {
            let mut rig = Rig::with(Binding::Acb, display, plane);
            assert_eq!(rig.start(A), DispatchResult::Refused(Failure::Unsupported), "{case}");
            assert!(rig.bus_methods().is_empty(), "{case}: nothing was opened");
            assert!(sink.calls().is_empty(), "{case}: no verb ran");
            // The refusal leaves the backend startable.
            assert!(rig.driver.exit_disposition() == ExitDisposition::Clean, "{case}");
        }
    }

    #[test]
    fn acb_load_carries_the_dummy_window_and_the_foreground_notice_precedes_subscribe() {
        let (mut rig, _, _) = Rig::acb();
        rig.acb_loading();
        assert_eq!(rig.bus_methods(), ["load", "notifyForeground", "subscribe"]);
        let state = rig.bus.0.lock().unwrap();
        assert!(state.calls[0].2.contains(r#""windowId":"window_id_dummy""#));
        assert!(state.calls[1].2.contains(r#""connectionId":"m1""#));
    }

    #[test]
    fn acb_a_refused_foreground_notice_ends_the_start() {
        let (mut rig, _, _) = Rig::acb();
        rig.acb_loading();
        {
            let mut state = rig.bus.0.lock().unwrap();
            let request = state.calls.iter().rev().find(|(name, _, _)| *name == "notifyForeground").unwrap().1;
            let session = state.session;
            state.replies.push_back(UriReply { session, request, method: "notifyForeground",
                payload: r#"{"returnValue":false}"#.into(), ok: true });
        }
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Failed(Failure::Refused) }));
        assert!(rig.bus_methods().contains(&"unload"), "the session is closing");
    }

    #[test]
    fn acb_binds_in_the_measured_order_and_pace_after_load_completed() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        assert!(sink.calls().is_empty(), "nothing binds before the service has loaded the media");
        rig.bus.say("subscribe", ENVELOPE);
        let events = rig.driver.poll(&rig.mt);
        assert!(events.iter().any(|event| matches!(event, DriverEvent::Observation { observation:
            Observation::Playback(Observed { value: Playback::ReadyAwaitPlay, .. }), .. })));
        assert_eq!(sink.calls(), ["plane_media_id m1"]);
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Bound);
        assert!(rig.driver.plane_owned(), "from the owner's grant on, the plane is this attempt's");
        for (t, expect) in [(199, 1), (200, 2), (399, 2), (400, 2), (599, 2), (600, 3), (899, 3), (900, 4)] {
            rig.at(t);
            rig.driver.poll(&rig.mt);
            assert_eq!(sink.calls().len(), expect, "t={t}: {:?}", sink.calls());
        }
        let calls = sink.calls();
        let envelope_len = serde_json::from_str::<serde_json::Value>(ENVELOPE).unwrap()["sourceInfo"].to_string().len();
        assert_eq!(calls, ["plane_media_id m1", "plane_loaded", &format!("plane_send_video_data len={envelope_len}"), "plane_finish"]);
        assert_eq!(rig.bus.calls("play"), 0, "the service's play is the caller's, never the sequence's");
    }

    #[test]
    fn acb_without_the_service_envelope_binds_nothing_and_times_out() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", r#"{"mediaId":"m1","videoInfo":{"width":1280,"height":720,"frameRate":24},"loadCompleted":true}"#);
        rig.driver.poll(&rig.mt);
        rig.at(19_999);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.at(20_000);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Failed(Failure::TimedOut) }));
        assert!(sink.calls().is_empty(), "a descriptor is never made up from bare video facts");
        assert!(!rig.driver.plane_owned());
    }

    #[test]
    fn acb_a_refused_bind_is_a_typed_failure_that_unloads_nothing() {
        let (mut rig, sink, plane) = Rig::acb();
        // Another backend already holds the plane.
        assert_eq!(plane.bind(sink, &rig.mt, plx_platform::tv::plane::BindPolicy::Strict, "other"),
            plx_platform::tv::plane::BindVerdict::Granted);
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Observation { attempt: A,
            observation: Observation::Failed(Failure::Busy) }));
        assert!(!sink.calls().iter().any(|call| call.starts_with("plane_media_id")), "{:?}", sink.calls());
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.unload_count(), 0, "the other binding is not ours to unload");
    }

    #[test]
    fn acb_a_bind_during_a_release_is_retried_on_a_later_tick() {
        let (mut rig, sink, plane) = Rig::acb();
        plane.bind(sink, &rig.mt, plx_platform::tv::plane::BindPolicy::Strict, "previous");
        plane.unload(sink, &rig.mt);
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        assert!(!sink.calls().iter().any(|call| call.starts_with("plane_media_id")));
        sink.mailbox.push(5, 1);
        rig.at(100);
        rig.driver.poll(&rig.mt);
        assert!(sink.calls().contains(&"plane_media_id m1".to_owned()), "{:?}", sink.calls());
    }

    #[test]
    fn acb_close_cancels_the_sequence_unloads_once_and_releases_on_the_completion() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop)), DispatchResult::Queued);
        // The bus releases; UNLOADED is asked for; the sequence's later steps never run.
        rig.at(1_000);
        let events = rig.driver.poll(&rig.mt);
        assert!(!events.iter().any(|event| matches!(event, DriverEvent::Release { status: ReleaseStatus::Released, .. })));
        assert_eq!(sink.unload_count(), 1);
        assert_eq!(sink.calls(), ["plane_media_id m1", "plane_unload"], "no step runs after UNLOADED");
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        // No completion yet: the backend keeps pumping and does not release.
        rig.at(1_100);
        assert!(!rig.driver.poll(&rig.mt).iter().any(|event| matches!(event, DriverEvent::Release { .. })));
        // The firmware's callback for the UNLOADED task (and another task's, which is no fence).
        sink.mailbox.push(4, 1);
        rig.driver.poll(&rig.mt);
        assert_ne!(plane.phase(), plx_platform::tv::plane::Phase::Released);
        sink.mailbox.push(5, 1);
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Released }), "{events:?}");
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Released);
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::Clean);
        assert!(!rig.driver.plane_owned());
    }

    #[test]
    fn acb_a_poll_failure_then_a_bus_release_runs_no_step_after_unloaded() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls(), ["plane_media_id m1"]);
        // A failed poll quarantines the session without any Stop, so `begin_close` never ran and
        // the paced sequence is still alive when the bus reports the release.
        rig.bus.0.lock().unwrap().fail_poll = true;
        rig.at(10);
        rig.driver.poll(&rig.mt);
        {
            let mut bus = rig.bus.0.lock().unwrap();
            bus.fail_poll = false;
            bus.close = CloseStatus::Released;
        }
        rig.at(20);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.unload_count(), 1);
        for t in [200, 400, 600, 900, 1_500] { rig.at(t); rig.driver.poll(&rig.mt); }
        assert_eq!(sink.calls(), ["plane_media_id m1", "plane_unload"],
            "no display step may follow UNLOADED, however the close began");
    }

    #[test]
    fn acb_close_without_a_completion_quarantines_at_the_owners_deadline() {
        let (mut rig, sink, plane) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop(StopReason::Stop));
        rig.driver.poll(&rig.mt);
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Releasing);
        // The owner measures its deadline on its own monotonic clock.
        std::thread::sleep(std::time::Duration::from_millis(plx_platform::tv::plane::RELEASE_FENCE_MS + 100));
        let events = rig.driver.poll(&rig.mt);
        assert!(events.contains(&DriverEvent::Release { attempt: A, status: ReleaseStatus::Quarantined }), "{events:?}");
        assert_eq!(plane.phase(), plx_platform::tv::plane::Phase::Quarantined(plx_platform::tv::plane::Why::Deadline));
        assert_eq!(rig.driver.exit_disposition(), ExitDisposition::PreserveNativeOwnership);
        assert_eq!(sink.unload_count(), 1);
    }

    #[test]
    fn acb_pause_and_resume_mirror_to_the_plane_once_bound() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_bound();
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_finish"));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_pause"));
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1"}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_resume"));
    }

    #[test]
    fn acb_the_first_play_is_not_mirrored_and_a_pause_before_the_bind_applies_when_it_completes() {
        let (mut rig, sink, _) = Rig::acb();
        rig.acb_loading();
        rig.bus.say("subscribe", ENVELOPE);
        rig.driver.poll(&rig.mt);
        // The service's first Play and an early Pause both land while the sequence is running.
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Play), DispatchResult::Queued);
        rig.bus.say("play", r#"{"returnValue":true,"mediaId":"m1"}"#);
        rig.driver.poll(&rig.mt);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Pause), DispatchResult::Queued);
        rig.bus.say("pause", r#"{"returnValue":true,"mediaId":"m1","paused":true}"#);
        rig.driver.poll(&rig.mt);
        assert!(!sink.calls().iter().any(|call| call == "plane_pause" || call == "plane_resume"), "{:?}", sink.calls());
        for t in [200, 400, 600, 900] { rig.at(t); rig.driver.poll(&rig.mt); }
        assert_eq!(sink.calls().last().map(String::as_str), Some("plane_pause"), "{:?}", sink.calls());
    }
}
