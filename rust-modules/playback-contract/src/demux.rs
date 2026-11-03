//! The packet feed worker's addressed session seam. The feed owns AVIO, FFmpeg
//! handles, access units and queue ordering. Its session port owns playback
//! clocks, accepted pause events, native transition leases and published state.

use crate::adaptive::{AutomaticTransition, FenceHandle, LeaseHandle, PhysicalFailure};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseLane { Hls, Original }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PauseEvent {
    pub paused: bool,
    pub sequence: u64,
    pub completed_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
pub struct PauseClock { pub at: Instant, pub elapsed: Duration }

/// Each read is made at the call site. A cached all-fields snapshot would
/// change the playhead and pause linearization around blocking AVIO and AQ.
pub trait DemuxClock {
    fn playpos_ns(&self) -> i64;
    fn display_base_ns(&self) -> i64;
    fn duration_ns(&self) -> i64;
    fn video_tail_ns(&self) -> i64;
    fn audio_tail_ns(&self) -> i64;
    fn hold_epoch(&self) -> u64;
    fn rebuffering(&self) -> bool;
    fn rebuffer_requested(&self) -> bool;
    fn paused(&self) -> bool;
    fn pause_clock(&self) -> PauseClock;
    fn begin_pause_lane(&mut self, lane: PauseLane);
    fn pause_event(&mut self, lane: PauseLane) -> PauseEvent;
    fn seeking(&self) -> bool;
    fn take_seek_ns(&self) -> Option<i64>;
    fn user_sequence(&self) -> u64;
    fn desired_audio_idx(&self) -> i32;
    fn applied_rate_permille(&self) -> i32;
    fn playpos_sampler(&self) -> fn() -> i64;
    fn hls_buffer_sampler(&self) -> fn() -> Option<i64>;
    fn remaining_playback_sampler(&self) -> fn() -> i64;
    fn user_sequence_sampler(&self) -> fn() -> u64;
}

/// A move-only physical lease is returned with its exact Drop action. The
/// player adapter owns the native clock mutex and generation tokens; no feed
/// pointer or global token registry enters the contract.
pub trait DemuxAdaptive {
    fn begin(&mut self, kind: AutomaticTransition, user_sequence: u64)
        -> Option<LeaseHandle>;
    fn authorize(&mut self, token: u64) -> bool;
    fn arm_trial(&mut self, runway_ms: i64, user_sequence: u64)
        -> Option<LeaseHandle>;
    fn arm_candidate(&mut self, automatic_token: u64)
        -> Result<FenceHandle, PhysicalFailure>;
    fn transition_permitted(&self, kind: AutomaticTransition) -> bool;
}

/// Physical reasons a source ended. The player adapter maps this vocabulary
/// to the report latch; the feed never imports report schema or Shared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemuxFault {
    HttpsReserveFailed, HttpsOpenFailed, HttpOpenFailed,
    OpenInputFailed, FindStreamInfoFailed, NoVideoStream,
    NoFraming, LoadMismatch, NoAudioAu, NoUsableVideo,
    DtsCoreMissing, HlsFailed, HlsNotReady, DemuxPanicked,
    NoAccessUnits, Other,
}

/// The effect alphabet is expanded by physical capability, not by adding one
/// field-writing method per native gauge. The adapter preserves each variant's
/// original atomic ordering and failure-latch rule.
pub enum DemuxEffect {
    NoteFault(DemuxFault),
    FailFault(DemuxFault),
    IoFailed,
    Failed,
    NoVideo,
    HttpStatus(i32),
    TrackNames { audio: Vec<String>, subs: Vec<String> },
    FileSize(i64),
    TransportRc(i32),
    NetworkBytes(i64),
    DurationNs(i64),
    VideoRaster { width: i32, height: i32 },
    VideoTailNs(i64),
    AudioTailNs(i64),
    AudioExpected(bool),
    PrimeRunwayMax(i64),
    RecoverySegment { total_us: u64, duration: Duration },
}

pub trait DemuxMedia {
    fn publish(&mut self, effect: DemuxEffect);
    fn record(&self, line: &str);
}

/// One move-only adapter follows the media open into its existing feed worker.
pub trait DemuxSession: Send {
    fn clock(&self) -> &dyn DemuxClock;
    fn clock_mut(&mut self) -> &mut dyn DemuxClock;
    fn adaptive_ref(&self) -> &dyn DemuxAdaptive;
    fn adaptive(&mut self) -> &mut dyn DemuxAdaptive;
    fn media(&mut self) -> &mut dyn DemuxMedia;
}
