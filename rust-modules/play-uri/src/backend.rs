//! Persistent URI executor. The caller owns route identity, source authorization and backend
//! admission. The native session and every bus request stay behind `Lifecycle`.
use crate::session_io;
use crate::uri_lifecycle::{Event as LifeEvent, Fx as LifeFx, Lifecycle, Phase};
use crate::uri_protocol::{Ev, Message, Method, UriCall};
use plx_base::task::MainThread;
use plx_platform::tv::sink::{DisplaySink, VP_EXPORTED};
use plx_platform::tv::uri::{Shape, UriMedia};
use plx_playback_contract::{AttemptId, BackendDriver, Capabilities, ContentTime, DispatchResult,
    DriverEvent, EvidenceSource, Failure, Observation, Observed, Operation, Playback,
    ReleaseStatus, SourceKey, SourceKind, TrackKind, TrackSelection};
use std::collections::VecDeque;
use std::os::raw::c_int;

/// A monotonic clock supplied by the runner. The ownership reducer only receives its value.
pub trait MonotonicClock { fn now_ms(&self) -> u64; }

/// An explicitly qualified display path. ACB cannot be selected here: its general native
/// descriptor and asynchronous release have not been qualified outside the diagnostic probe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binding { Exported { canvas: (c_int, c_int) }, Unbound }

/// No firmware capability is inferred from the display mode or transport response.
pub struct Config {
    pub transport: &'static dyn UriMedia,
    pub display: &'static dyn DisplaySink,
    pub clock: &'static dyn MonotonicClock,
    pub app_id: String,
    pub binding: Binding,
    pub capabilities: Capabilities,
    pub startup_timeout_ms: u64,
    pub release_allowance_ms: u64,
    pub source_ttl_ms: u64,
}

struct StagedSource { attempt: AttemptId, key: SourceKey, url: String, deadline_ms: u64 }

/// The active URL and the staged URL are deliberately excluded from `Debug` and all notices.
/// The caller must keep this object until `Release::Released`, or retain it in quarantine.
pub struct UriBackend {
    config: Config,
    lifecycle: Lifecycle,
    staged: Option<StagedSource>,
    active: Option<AttemptId>,
    last_started: Option<AttemptId>,
    url: Option<String>,
    window: bool,
    window_id: Option<String>,
    start_deadline_ms: Option<u64>,
    loaded: bool,
    play_sent: bool,
    subscribed: bool,
    pending: VecDeque<DriverEvent>,
    close_reported: bool,
    quarantine_reported: bool,
}

impl UriBackend {
    pub fn new(config: Config) -> Self {
        assert!(!config.capabilities.packet_feed, "URI transport cannot accept packet feeds");
        Self { config, lifecycle: Lifecycle::new(), staged: None, active: None,
            last_started: None, url: None, window: false, window_id: None,
            start_deadline_ms: None, loaded: false, play_sent: false, subscribed: false,
            pending: VecDeque::new(), close_reported: false, quarantine_reported: false }
    }

    /// Give exactly one authorized, owned URL to the next matching Start. Superseding a staged
    /// source destroys its old locator; a failed, cancelled or expired Start cannot reuse it.
    pub fn stage_source(&mut self, attempt: AttemptId, key: SourceKey, url: String) -> Result<(), Failure> {
        if url.is_empty() || self.last_started.is_some_and(|last| attempt <= last) {
            return Err(Failure::Stale);
        }
        let deadline_ms = self.config.clock.now_ms().saturating_add(self.config.source_ttl_ms);
        self.staged = Some(StagedSource { attempt, key, url, deadline_ms });
        Ok(())
    }

    /// Cancel an unconsumed locator without changing a live native attempt.
    pub fn cancel_staged(&mut self, attempt: AttemptId) {
        if self.staged.as_ref().is_some_and(|source| source.attempt == attempt) {
            self.staged = None;
        }
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
        if self.active.is_some() || !self.lifecycle.released() { return self.refuse_before_open(attempt, Failure::Busy); }
        if self.last_started.is_some_and(|last| attempt <= last) { return self.refuse_before_open(attempt, Failure::Stale); }
        if requirements.source == SourceKind::PacketFeed { return self.refuse_before_open(attempt, Failure::Unsupported); }
        if !self.config.capabilities.supports(requirements) { return self.refuse_before_open(attempt, Failure::Unsupported); }
        let Some(staged) = self.staged.take() else { return self.refuse_before_open(attempt, Failure::Refused); };
        if staged.attempt != attempt || staged.key != source || self.config.clock.now_ms() >= staged.deadline_ms {
            return self.refuse_before_open(attempt, Failure::Stale);
        }
        if matches!(self.config.binding, Binding::Exported { .. }) && self.config.display.window_mode() != VP_EXPORTED {
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
            Binding::Unbound => None,
        };
        self.last_started = Some(attempt);
        self.active = Some(attempt);
        self.url = Some(staged.url);
        self.window_id = window_id;
        self.start_deadline_ms = Some(self.config.clock.now_ms().saturating_add(self.config.startup_timeout_ms));
        self.loaded = false;
        self.play_sent = false;
        self.subscribed = false;
        self.close_reported = false;
        self.quarantine_reported = false;
        self.lifecycle = Lifecycle::new();
        self.feed(LifeEvent::Open(Shape::Named), mt, self.config.clock.now_ms());
        DispatchResult::Queued
    }

    fn begin_close(&mut self, mt: &MainThread, now_ms: u64) {
        self.staged = None;
        self.url = None;
        self.start_deadline_ms = None;
        if self.active.is_none() || matches!(self.lifecycle.phase(), Phase::Closing | Phase::Quarantined) {
            return;
        }
        if let Some(media_id) = self.lifecycle.media_id().cloned() {
            self.feed(LifeEvent::Call(UriCall::Unload { media_id }), mt, now_ms);
        }
        self.feed(LifeEvent::Close { now_ms, allowance_ms: self.config.release_allowance_ms }, mt, now_ms);
    }

    fn maybe_subscribe_and_play(&mut self, mt: &MainThread, now_ms: u64) {
        if self.lifecycle.phase() != Phase::Open { return; }
        let Some(media_id) = self.lifecycle.media_id().cloned() else { return; };
        if !self.subscribed {
            self.subscribed = true;
            self.feed(LifeEvent::Call(UriCall::Subscribe { media_id: media_id.clone() }), mt, now_ms);
        }
        if self.loaded && !self.play_sent {
            self.play_sent = true;
            self.feed(LifeEvent::Call(UriCall::Play { media_id }), mt, now_ms);
        }
    }

    fn delivered(&mut self, method: Method, ok: bool, message: Message,
        mt: &MainThread, now_ms: u64) {
        let denied = !ok || message.accepted() == Some(false);
        if denied {
            self.observe(Observation::Failed(Failure::Refused));
            if matches!(method, Method::Register | Method::Load | Method::Subscribe)
                || (method == Method::Play && self.start_deadline_ms.is_some()) {
                self.begin_close(mt, now_ms);
            }
            return;
        }
        if method == Method::Register {
            self.feed(LifeEvent::Call(UriCall::Load { window_id: self.window_id.clone() }), mt, now_ms);
            return;
        }
        if method == Method::Load {
            // The URL is needed only to build the one Load payload. Never retain it for controls.
            self.url = None;
        }
        let mut ended = false;
        for event in &message.normalized {
            let playback = match event {
                Ev::LoadCompleted => {
                    self.loaded = true;
                    Some(Playback::Ready)
                }
                Ev::Playing => {
                    self.start_deadline_ms = None;
                    Some(Playback::Playing)
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
        if message.confirmed_track.is_some() {
            if let Some((kind, index)) = message.selected_track {
                let kind = match kind { "audio" => Some(TrackKind::Audio), "text" => Some(TrackKind::Subtitle), _ => None };
                if let (Some(kind), Ok(id)) = (kind, u64::try_from(index)) {
                    self.observe(Observation::TrackSelected(Observed {
                        value: TrackSelection { kind, id }, from: EvidenceSource::BackendReport,
                    }));
                }
            }
        }
        if ended { self.begin_close(mt, now_ms); }
        else { self.maybe_subscribe_and_play(mt, now_ms); }
    }

    fn feed(&mut self, first: LifeEvent, mt: &MainThread, now_ms: u64) {
        let mut queue = VecDeque::from([first]);
        while let Some(event) = queue.pop_front() {
            let mut effects = Vec::new();
            self.lifecycle.step(event, &mut effects);
            for effect in effects {
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
                    }
                    LifeFx::Fault => {
                        self.observe(Observation::Failed(Failure::ReleaseUncertain));
                    }
                    LifeFx::UnloadAcb { session } => {
                        unsafe { self.config.display.plane_unload(mt) };
                        queue.push_back(LifeEvent::AcbUnloadDispatched { session });
                    }
                    LifeFx::AcbQuarantined | LifeFx::Open { .. } | LifeFx::Call { .. } | LifeFx::Close { .. } => {}
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

impl BackendDriver for UriBackend {
    type Context = MainThread;
    fn capabilities(&self) -> Capabilities { self.config.capabilities }

    fn dispatch(&mut self, mt: &MainThread, attempt: AttemptId,
        _command: plx_playback_contract::CommandId, operation: Operation) -> DispatchResult {
        if let Operation::Start { source, requirements } = operation {
            return self.start(mt, attempt, source, requirements);
        }
        if self.active != Some(attempt) { return DispatchResult::Refused(Failure::Stale); }
        let now_ms = self.config.clock.now_ms();
        if matches!(operation, Operation::Stop) {
            if self.lifecycle.phase() == Phase::Quarantined {
                return DispatchResult::Refused(Failure::ReleaseUncertain);
            }
            self.begin_close(mt, now_ms);
            return DispatchResult::Queued;
        }
        if self.lifecycle.phase() != Phase::Open { return DispatchResult::Refused(Failure::Busy); }
        let Some(media_id) = self.lifecycle.media_id().cloned() else { return DispatchResult::Refused(Failure::Busy); };
        let call = match operation {
            Operation::Play if self.config.capabilities.pause => UriCall::Play { media_id },
            Operation::Pause if self.config.capabilities.pause => UriCall::Pause { media_id },
            Operation::Seek(time) if self.config.capabilities.seek => {
                let Ok(position) = i64::try_from(time.as_millis()) else { return DispatchResult::Refused(Failure::Unsupported); };
                UriCall::Seek { media_id, position }
            }
            Operation::SelectTrack(track) => {
                let enabled = match track.kind {
                    TrackKind::Audio => self.config.capabilities.audio_track,
                    TrackKind::Subtitle => self.config.capabilities.subtitle_track,
                };
                if !enabled { return DispatchResult::Refused(Failure::Unsupported); }
                let Ok(index) = i64::try_from(track.id) else { return DispatchResult::Refused(Failure::Unsupported); };
                UriCall::SelectTrack { media_id, kind: match track.kind { TrackKind::Audio => "audio", TrackKind::Subtitle => "text" }, index }
            }
            _ => return DispatchResult::Refused(Failure::Unsupported),
        };
        self.feed(LifeEvent::Call(call), mt, now_ms);
        DispatchResult::Queued
    }

    fn poll(&mut self, mt: &MainThread) -> Vec<DriverEvent> {
        let now_ms = self.config.clock.now_ms();
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
        if self.start_deadline_ms.is_some_and(|deadline| now_ms >= deadline)
            && matches!(self.lifecycle.phase(), Phase::Opening | Phase::Open) {
            self.observe(Observation::Failed(Failure::TimedOut));
            self.begin_close(mt, now_ms);
        }
        self.pending.drain(..).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_platform::tv::sink::NoSink;
    use plx_platform::tv::uri::{CloseStatus, RequestId, SessionId, UriPoll, UriReply};
    use plx_playback_contract::{BackendState, Event as ContractEvent, Fidelity,
        Fx as ContractFx, Intent, Requirements, SourceKind};
    use std::sync::{atomic::{AtomicU64, Ordering}, Mutex};

    const CAPS: Capabilities = Capabilities { progressive_uri: true, hls_uri: false,
        packet_feed: false, exact_video: true, exact_audio: true, pause: true,
        seek: true, audio_track: true, subtitle_track: true };
    const REQ: Requirements = Requirements { source: SourceKind::ProgressiveUri,
        video: Fidelity::Exact, audio: Fidelity::Exact, audio_track: None,
        subtitle_track: None };
    const KEY: SourceKey = SourceKey::new(17);
    const A: AttemptId = AttemptId::new(7, 1);
    const B: AttemptId = AttemptId::new(7, 2);
    const URL: &str = "https://private.invalid/file?X-Plex-Token=SECRET";

    struct Clock(AtomicU64);
    impl MonotonicClock for Clock { fn now_ms(&self) -> u64 { self.0.load(Ordering::Relaxed) } }
    struct BusState { session: SessionId, serial: u64,
        calls: Vec<(&'static str, RequestId, String)>, replies: VecDeque<UriReply>,
        close: CloseStatus, hold: bool, refuse_open: bool }
    struct Bus(Mutex<BusState>);
    impl Bus {
        fn new() -> Self { Self(Mutex::new(BusState { session: SessionId::new(0),
            serial: 0, calls: Vec::new(), replies: VecDeque::new(),
            close: CloseStatus::Released, hold: false, refuse_open: false })) }
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
    struct Rig { driver: UriBackend, clock: &'static Clock, bus: &'static Bus,
        mt: MainThread, command: plx_playback_contract::CommandId }
    impl Rig {
        fn new() -> Self {
            let clock = Box::leak(Box::new(Clock(AtomicU64::new(0))));
            let bus = Box::leak(Box::new(Bus::new()));
            let display = Box::leak(Box::new(NoSink));
            let driver = UriBackend::new(Config { transport: bus, display, clock,
                app_id: "app.test".into(), binding: Binding::Unbound, capabilities: CAPS,
                startup_timeout_ms: 500, release_allowance_ms: 100, source_ttl_ms: 50 });
            let mut state = BackendState::new(CAPS);
            let mut fx = Vec::new();
            state.step(ContractEvent::Intent(Intent::Start {
                attempt: A, source: KEY, requirements: REQ }), &mut fx);
            let [ContractFx::Dispatch { command, .. }] = fx.as_slice() else { panic!("dispatch") };
            Self { driver, clock, bus, mt: unsafe { MainThread::assume() }, command: *command }
        }
        fn start(&mut self, attempt: AttemptId) -> DispatchResult {
            self.driver.stage_source(attempt, KEY, URL.into()).unwrap();
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
                observation: Observation::Playback(Observed { value: Playback::Ready,
                    from: EvidenceSource::BackendReport }) }]);
            assert_eq!(self.bus.calls("subscribe"), 1);
            assert_eq!(self.bus.calls("play"), 1);
        }
    }

    #[test]
    fn native_clock_and_ack_never_prove_a_picture_and_controls_remain_addressed() {
        let mut rig = Rig::new(); rig.opened();
        assert!(rig.driver.url.is_none(), "load consumed the private locator");
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
            Operation::SelectTrack(track)), DispatchResult::Queued);
        rig.bus.say("subscribe", r#"{"mediaId":"m1","trackSelected":{"type":"audio","index":3}}"#);
        assert!(rig.driver.poll(&rig.mt).is_empty());
        rig.bus.say("selectTrack", r#"{"returnValue":true,"mediaId":"m1"}"#);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Observation { attempt: A,
            observation: Observation::TrackSelected(Observed { value: track,
                from: EvidenceSource::BackendReport }) }]);
    }

    #[test]
    fn quarantine_blocks_successor_until_release_and_expired_source_is_not_reused() {
        let mut rig = Rig::new(); rig.opened();
        rig.bus.0.lock().unwrap().hold = true;
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command, Operation::Stop), DispatchResult::Queued);
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Closing }]);
        rig.driver.stage_source(B, KEY, URL.into()).unwrap();
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Busy));
        rig.clock.0.store(101, Ordering::Relaxed);
        assert!(rig.driver.poll(&rig.mt).contains(&DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Quarantined }));
        rig.bus.0.lock().unwrap().close = CloseStatus::Released;
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }]);
        assert_eq!(rig.driver.dispatch(&rig.mt, B, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Stale));
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
        assert_eq!(rig.start(B), DispatchResult::Queued);
        rig.bus.0.lock().unwrap().replies.push_back(UriReply { session: old_session,
            request: RequestId::new(0), method: "register", payload: String::new(), ok: true });
        assert!(rig.driver.poll(&rig.mt).is_empty());
        assert_eq!(rig.bus.calls("load"), 1, "the previous native generation cannot load the new source");
    }

    #[test]
    fn cancelled_or_expired_staged_locator_cannot_start_an_attempt() {
        let mut rig = Rig::new();
        rig.driver.stage_source(A, KEY, URL.into()).unwrap();
        rig.driver.cancel_staged(A);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Refused));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }], "a pre-open refusal has a proven empty owner");
        rig.driver.stage_source(A, KEY, URL.into()).unwrap();
        rig.clock.0.store(50, Ordering::Relaxed);
        assert_eq!(rig.driver.dispatch(&rig.mt, A, rig.command,
            Operation::Start { source: KEY, requirements: REQ }), DispatchResult::Refused(Failure::Stale));
        assert_eq!(rig.driver.poll(&rig.mt), [DriverEvent::Release { attempt: A,
            status: ReleaseStatus::Released }]);
        assert!(rig.driver.staged.is_none());
    }
}
