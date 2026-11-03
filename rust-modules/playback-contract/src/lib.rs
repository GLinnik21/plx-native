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
pub mod tracks;
pub use state::{BackendState, Event, Fx, Intent, Phase, Snapshot, TrackFact};

/// The selected physical executor. An attempt and every result carry this identity separately;
/// an old backend cannot publish into a successor even if a malformed event reused its attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BackendKind { Uri, Feed }

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
    Presented,
    TrackSelected(Observed<TrackSelection>),
    TrackOff(Observed<TrackKind>),
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
