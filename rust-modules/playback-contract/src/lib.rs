//! Playback-level commands and evidence shared by backends. The source locator, native request
//! identities, firmware callbacks, and resource ownership remain with each backend driver.
//! `BackendState` is pure; a caller executes its effects through `BackendDriver` and feeds every
//! answer back as an addressed event. A queued command is not proof that playback changed.

mod state;
pub mod adaptive;
pub use state::{BackendState, Fx, Event, Intent, Phase, Snapshot};

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fidelity { Exact, Compatible }

/// Route requirements, carried without a PMS plan or any firmware-specific descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Requirements {
    pub source: SourceKind,
    pub video: Fidelity,
    pub audio: Fidelity,
    pub audio_track: Option<TrackSelection>,
    pub subtitle_track: Option<TrackSelection>,
}

/// Facts a driver can promise for admission. These are not proof that a particular stream works.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub progressive_uri: bool,
    pub hls_uri: bool,
    pub packet_feed: bool,
    pub exact_video: bool,
    pub exact_audio: bool,
    pub pause: bool,
    pub seek: bool,
    pub audio_track: bool,
    pub subtitle_track: bool,
}
impl Capabilities {
    pub fn supports(self, req: Requirements) -> bool {
        let source = match req.source {
            SourceKind::ProgressiveUri => self.progressive_uri,
            SourceKind::HlsUri => self.hls_uri,
            SourceKind::PacketFeed => self.packet_feed,
        };
        source
            && (req.video != Fidelity::Exact || self.exact_video)
            && (req.audio != Fidelity::Exact || self.exact_audio)
            && (req.audio_track.is_none() || self.audio_track)
            && (req.subtitle_track.is_none() || self.subtitle_track)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Start { source: SourceKey, requirements: Requirements },
    Play,
    Pause,
    Seek(ContentTime),
    SelectTrack(TrackSelection),
    Stop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure { Busy, Stale, Unsupported, Refused, TimedOut, Backend, ReleaseUncertain }

/// Dispatch only says whether work was queued. Native acceptance is a later observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchResult { Queued, Refused(Failure) }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Playback { Ready, Playing, Paused, Ended }

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
    Failed(Failure),
}

/// A backend's resource-owner verdict. Uncertain close fences reuse until explicit release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseStatus { Closing, Released, Quarantined }

/// Only a caller can issue an intent. A driver may publish addressed observations and release
/// verdicts, but cannot request a new playback through its poll stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriverEvent {
    Observation { attempt: AttemptId, observation: Observation },
    Release { attempt: AttemptId, status: ReleaseStatus },
}
impl From<DriverEvent> for Event {
    fn from(value: DriverEvent) -> Self {
        match value {
            DriverEvent::Observation { attempt, observation } => Event::Observation { attempt, observation },
            DriverEvent::Release { attempt, status } => Event::Release { attempt, status },
        }
    }
}

/// The backend-specific IO lives here; native correlation and close rules stay behind this trait.
pub trait BackendDriver {
    fn capabilities(&self) -> Capabilities;
    fn dispatch(&mut self, attempt: AttemptId, command: CommandId, operation: Operation) -> DispatchResult;
    fn poll(&mut self) -> Vec<DriverEvent>;
}
