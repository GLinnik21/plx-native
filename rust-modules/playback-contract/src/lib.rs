//! Playback-level commands and evidence shared by backends. The source locator, native request
//! identities, firmware callbacks, and resource ownership remain with each backend driver.
//! `BackendState` is pure; a caller executes its effects through one `DriverSet` and feeds every
//! answer back as an addressed event. A queued command is not proof that playback changed.
#![deny(clippy::debug_assert_with_mut_call)]

mod state;
mod time;
pub use time::MediaTimeMs;
pub mod adaptive;
pub mod subtitle;
pub mod readout;
pub mod tracks;
pub mod uri;
pub mod playurl;
pub use uri::UriDriver;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(any(test, feature = "test-support"))]
pub mod feed_fake;
pub mod video;
pub use state::{BackendState, Event, Fx, Intent, Phase, Snapshot, TrackFact};

/// The selected physical executor. An attempt and every result carry this identity separately;
/// an old backend cannot publish into a successor even if a malformed event reused its attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackendKind { Uri, Feed }

/// The physical backends one composition supplies. Route policy reads it as a runtime fact (a
/// backend absent from the set is never offered, planned for or waited on) instead of knowing which
/// drivers were compiled in; the application's composition is what supplies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BackendSet { pub uri: bool, pub feed: bool }
impl BackendSet {
    pub const NONE: Self = Self { uri: false, feed: false };
    pub const URI: Self = Self { uri: true, feed: false };
    pub const FEED: Self = Self { uri: false, feed: true };
    pub const BOTH: Self = Self { uri: true, feed: true };

    pub const fn contains(self, backend: BackendKind) -> bool {
        match backend { BackendKind::Uri => self.uri, BackendKind::Feed => self.feed }
    }

    /// The set a composition's installed drivers form: a backend is in it when its driver reports
    /// capabilities. This is the only way a production boot derives its set, so route policy offers
    /// what is physically installed and never what Cargo features happened to be on.
    pub fn from_capabilities<D: DriverSet + ?Sized>(drivers: &D) -> Self {
        Self {
            uri: drivers.capabilities(BackendKind::Uri).is_some(),
            feed: drivers.capabilities(BackendKind::Feed).is_some(),
        }
    }
}
pub mod demux;
pub mod feed;

/// One logical start, supplied by the existing route owner. Its pair matches a route
/// transaction and that transaction's physical start attempt; the contract does not mint a
/// second playback identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AttemptId { owner: u64, generation: u64 }
impl AttemptId {
    pub const fn new(owner: u64, generation: u64) -> Self { Self { owner, generation } }
    pub const fn owner(self) -> u64 { self.owner }
    pub const fn generation(self) -> u64 { self.generation }
}

/// One dispatched operation. This is distinct from a backend's bus or firmware request ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(u64);
impl CommandId { pub const fn get(self) -> u64 { self.0 } }

/// Milliseconds in the content timeline, after a backend has normalized any native clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContentTime(u64);
impl ContentTime {
    pub const fn from_millis(value: u64) -> Self { Self(value) }
    pub const fn as_millis(self) -> u64 { self.0 }
}

/// Normal playback speed in the per-mille units shared by route policy and feed timing.
pub const RATE_NORMAL: i32 = 1000;

/// A driver-owned locator. It is deliberately a number: the reducer never holds or prints a URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceKey(u64);
impl SourceKey { pub const fn new(value: u64) -> Self { Self(value) } }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind { ProgressiveUri, HlsUri, PacketFeed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind { Audio, Subtitle }

/// Backend-neutral track identity. The driver maps it to its native index or stream ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TrackSelection { pub kind: TrackKind, pub id: u64 }

/// An explicit request. `Unspecified` leaves the current/default pick alone; `Off` asks the
/// driver to disable it, and `Selected` names a positive backend-neutral track identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackChoice { Unspecified, Off, Selected(u64) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity { Exact, Compatible }

/// Route requirements, carried without a PMS plan or any firmware-specific descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    pub source: SourceKind,
    pub video: Fidelity,
    pub audio: Fidelity,
    pub audio_track: TrackChoice,
    pub subtitle_track: TrackChoice,
}

/// Implemented callable operations. Codec declaration acceptance belongs to a driver/native
/// open, so these flags never veto an exact or Force route based on a codec table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub progressive_uri: bool,
    pub hls_uri: bool,
    pub packet_feed: bool,
    pub pause: bool,
    pub seek: bool,
    pub audio_track: bool,
    pub subtitle_track: bool,
    pub audio_off: bool,
    pub subtitle_off: bool,
}
impl Capabilities {
    pub fn supports(self, req: Requirements) -> bool {
        let source = match req.source {
            SourceKind::ProgressiveUri => self.progressive_uri,
            SourceKind::HlsUri => self.hls_uri,
            SourceKind::PacketFeed => self.packet_feed,
        };
        source && self.supports_track(TrackKind::Audio, req.audio_track)
            && self.supports_track(TrackKind::Subtitle, req.subtitle_track)
    }

    pub fn supports_track(self, kind: TrackKind, choice: TrackChoice) -> bool {
        match (kind, choice) {
            (_, TrackChoice::Unspecified) => true,
            (TrackKind::Audio, TrackChoice::Off) => self.audio_off,
            (TrackKind::Subtitle, TrackChoice::Off) => self.subtitle_off,
            (TrackKind::Audio, TrackChoice::Selected(_)) => self.audio_track,
            (TrackKind::Subtitle, TrackChoice::Selected(_)) => self.subtitle_track,
        }
    }
}

/// The common controller's initial transport goal. Start only prepares the driver; the
/// controller later issues addressed Seek and Play/Pause commands in the chosen order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialPlayback { pub position: Option<ContentTime>, pub paused: bool }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason { Stop, Replace, Suspend, Exit }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Start { source: SourceKey, requirements: Requirements },
    Play,
    Pause,
    Seek(ContentTime),
    SelectTrack { kind: TrackKind, choice: TrackChoice },
    Stop(StopReason),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure { Busy, Stale, Unavailable, Unsupported, Refused, TimedOut, Backend, ReleaseUncertain }

/// Dispatch only says whether work was queued. Native acceptance is a later observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchResult { Queued, Refused(Failure) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playback { ReadyAwaitPlay, Playing, Paused, Ended }

/// Provenance of an observation. A service state report cannot stand in for a presented frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvidenceSource { BackendReport, PresentationCallback }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Observed<T> { pub value: T, pub from: EvidenceSource }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    Playback(Observed<Playback>),
    Position(Observed<ContentTime>),
    /// The first picture of this attempt is on the panel, as far as the process can know. Its
    /// grade is **necessary-condition evidence (the plane bound / the sink counted displayed
    /// frames), not photon proof**: a driver emits it once, only when the conditions below hold,
    /// and otherwise emits nothing, so startup, rebuffer and handoff-gap metrics stay Unknown
    /// rather than guessed. A position clock, a service state report or a command acknowledgment
    /// is never this. Per backend:
    ///
    /// - **Feed, webOS 4 (ACB plane):** the plane bind is complete and one position tick has
    ///   followed it (`plane_shows_the_picture`), and the sink's displayed-frame counter
    ///   (callback type 47) has reported a count above zero since that bind.
    /// - **Feed, webOS 5+ (no bind):** the first displayed-frame counter callback (type 49) with
    ///   a count above zero. Never the type-0 `FRAMEREADY` tick, which is a 5 Hz position report.
    /// - **URI, ACB binding (webOS 4):** all three, in this order of observation: the service has
    ///   said `playing`; the plane owner has seen the firmware complete BOTH the full-screen
    ///   `setDisplayWindow` and the `setState(PLAYING)` of the bind with event 1, each matched on
    ///   its own task id; and the service's `currentTime` then advances (a sample above the one
    ///   before it, so a resumed start's first report is not movement). A completion of another
    ///   task, or one that is not event 1, never counts. The exported window and an unbound
    ///   attempt never present: nothing there says the plane was told to show the stream.
    /// - A firmware that never delivers the counter (Feed) or the completions (URI) never
    ///   presents; startup stays Unknown.
    Presented,
    TrackSelected(Observed<TrackSelection>),
    TrackOff(Observed<TrackKind>),
    /// A running attempt's media supply ran dry (`true`) and recovered (`false`). Startup
    /// buffering is not a rebuffer: a driver that cannot tell the two apart emits nothing, and
    /// the absence stays unknown rather than becoming `false`.
    Rebuffering(Observed<bool>),
    Failed(Failure),
}

/// A backend's resource-owner verdict. Uncertain close fences reuse until explicit release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseStatus { Closing, Released, Quarantined }

/// A clean exit requires every concrete driver to have released its native resources. A queued
/// Stop, returned worker, or missing callback is not a release verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitDisposition { Clean, PreserveNativeOwnership }

/// Only a caller can issue an intent. A driver may publish addressed observations and release
/// verdicts, but cannot request a new playback through its poll stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverEvent {
    Observation { attempt: AttemptId, observation: Observation },
    /// The exact queued transport verb reached the driver. This is command evidence only;
    /// it says nothing about the playback clock or a presented picture.
    ControlAccepted { attempt: AttemptId, command: CommandId, operation: Operation,
        native_acknowledged: bool },
    /// A queued command later reached the driver boundary and was refused. Track and seek
    /// refusals remain correlated to their exact request, including during startup.
    ControlRefused { attempt: AttemptId, command: CommandId, operation: Operation,
        failure: Failure },
    /// Completion of the exact queued track command, after native confirmation. A plain
    /// TrackSelected observation remains useful evidence but cannot settle startup policy.
    TrackSettled { attempt: AttemptId, command: CommandId, kind: TrackKind, choice: TrackChoice },
    /// Emitted only after the driver correlates completion to this exact Seek request.
    SeekLanded { attempt: AttemptId, command: CommandId, position: Option<Observed<ContentTime>> },
    Release { attempt: AttemptId, status: ReleaseStatus },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendEvent { pub backend: BackendKind, pub event: DriverEvent }
impl From<BackendEvent> for Event {
    fn from(value: BackendEvent) -> Self {
        let backend = value.backend;
        match value.event {
            DriverEvent::Observation { attempt, observation } => Event::Observation { backend, attempt, observation },
            DriverEvent::ControlAccepted { attempt, command, operation, native_acknowledged } =>
                Event::ControlAccepted { backend, attempt, command, operation,
                    native_acknowledged },
            DriverEvent::ControlRefused { attempt, command, operation, failure } =>
                Event::ControlRefused { backend, attempt, command, operation, failure },
            DriverEvent::TrackSettled { attempt, command, kind, choice } =>
                Event::TrackSettled { backend, attempt, command, kind, choice },
            DriverEvent::SeekLanded { attempt, command, position } => Event::SeekLanded { backend, attempt, command, position },
            DriverEvent::Release { attempt, status } => Event::Release { backend, attempt, status },
        }
    }
}

/// One injected set of optional persistent physical drivers. `Context` is borrowed for each IO
/// call, so a caller may pass its own main-thread token without a bound or downcast here.
pub trait DriverSet {
    type Context;
    fn capabilities(&self, backend: BackendKind) -> Option<Capabilities>;
    fn dispatch(&mut self, backend: BackendKind, ctx: &Self::Context, attempt: AttemptId,
        command: CommandId, operation: Operation) -> DispatchResult;
    fn poll(&mut self, ctx: &Self::Context) -> Vec<BackendEvent>;
    fn discard_staged(&mut self, backend: BackendKind, attempt: AttemptId, key: SourceKey);
    fn exit_disposition(&self) -> ExitDisposition;
}

#[cfg(test)]
mod backend_set_tests {
    use super::*;

    const CAPS: Capabilities = Capabilities {
        progressive_uri: true, hls_uri: false, packet_feed: true, pause: true, seek: true,
        audio_track: false, subtitle_track: false, audio_off: false, subtitle_off: false,
    };

    /// A driver set whose only fact is which backends answer `capabilities`.
    struct Installed { uri: bool, feed: bool }
    impl DriverSet for Installed {
        type Context = ();
        fn capabilities(&self, backend: BackendKind) -> Option<Capabilities> {
            match backend { BackendKind::Uri => self.uri, BackendKind::Feed => self.feed }.then_some(CAPS)
        }
        fn dispatch(&mut self, _: BackendKind, _: &(), _: AttemptId, _: CommandId, _: Operation)
            -> DispatchResult { DispatchResult::Refused(Failure::Unavailable) }
        fn poll(&mut self, _: &()) -> Vec<BackendEvent> { Vec::new() }
        fn discard_staged(&mut self, _: BackendKind, _: AttemptId, _: SourceKey) {}
        fn exit_disposition(&self) -> ExitDisposition { ExitDisposition::Clean }
    }

    #[test]
    fn the_set_is_exactly_the_backends_whose_driver_reports_capabilities() {
        for (uri, feed, want) in [
            (false, false, BackendSet::NONE),
            (true, false, BackendSet::URI),
            (false, true, BackendSet::FEED),
            (true, true, BackendSet::BOTH),
        ] {
            assert_eq!(BackendSet::from_capabilities(&Installed { uri, feed }), want,
                "uri={uri} feed={feed}");
        }
    }

    #[test]
    fn a_trait_object_forms_the_same_set() {
        let drivers: &dyn DriverSet<Context = ()> = &Installed { uri: true, feed: false };
        assert_eq!(BackendSet::from_capabilities(drivers), BackendSet::URI);
    }
}
