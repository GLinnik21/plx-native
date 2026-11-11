//! The packet feed worker's addressed session seam. The feed owns AVIO, FFmpeg
//! handles, access units and queue ordering. Its session port owns playback
//! clocks, accepted pause events, native transition leases and published state.

use crate::adaptive::{AutomaticTransition, FenceHandle, LeaseHandle, CandidateFenceError};
use crate::AttemptId;
use crate::subtitle::{Anchor, CaptionRetention, DecodeKind, Manifest, RawCue, TrackInfo};
use std::sync::Arc;
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

/// One ordered physical HLS clock read. The feed combines these published
/// tails with a staged segment only after the captured epoch has been checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HlsBufferClocks {
    pub playhead_ns: i64,
    pub video_tail_ns: i64,
    pub audio_tail_ns: i64,
    pub display_base_ns: i64,
}

/// The C AVIO callbacks retain this captured view while the worker retains
/// mutable cursor and lease ownership. A retired native epoch returns `None`
/// or refuses an effect, so a late callback cannot write the next session.
pub trait DemuxObserver: Send + Sync {
    fn attempt(&self) -> AttemptId;
    fn playpos_ns(&self) -> Option<i64>;
    /// One addressed caption retention sample; `None` means the demux epoch retired.
    fn caption_retention(&self) -> Option<CaptionRetention>;
    fn hold_epoch(&self) -> Option<u64>;
    fn rebuffering(&self) -> Option<bool>;
    fn audio_expected(&self) -> Option<bool>;
    fn duration_ns(&self) -> Option<i64>;
    fn declared_video_code(&self) -> Option<u8>;
    /// The `dg_load_a` code the Load declared for this open (0 = `needAudio:false`).
    fn declared_audio_code(&self) -> Option<u8>;
    fn paused(&self) -> Option<bool>;
    fn pause_clock(&self) -> Option<PauseClock>;
    fn hls_buffer_ms(&self) -> Option<Option<i64>>;
    fn hls_buffer_clocks(&self) -> Option<HlsBufferClocks>;
    fn remaining_playback_ms(&self) -> Option<i64>;
    fn user_sequence(&self) -> Option<u64>;
    fn publish(&self, effect: DemuxCallbackEffect) -> bool;
    /// Run `f` while this observer's native epoch is the live one, under the barrier that retires
    /// it, so what `f` reads or commits cannot land in the next session. `false` (and `f` not
    /// run) once the epoch has retired. `f` runs no native call and takes no lock the retirement
    /// waits on.
    fn with_active(&self, f: &mut dyn FnMut()) -> bool;
}

/// A late read bound to the worker's captured native epoch. `None` retires the read.
pub type AddressedSampler<T> = Arc<dyn Fn() -> Option<T> + Send + Sync>;

/// Only callback-safe atomics. Neither logs, queue operations nor native
/// calls run while the native epoch retirement barrier is held.
pub enum DemuxCallbackEffect {
    NetworkBytes(i64),
    HttpStatus(i32),
    FileSize(i64),
    DurationNs(i64),
    TransportFailure { rc: i32, age_ms: i64, reopen_failed: bool },
    IoFailed,
    RequestTerminalHold,
}

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
    fn side_subtitle_owner(&self) -> bool;
    fn playpos_sampler(&self) -> AddressedSampler<i64>;
    fn hls_buffer_sampler(&self) -> AddressedSampler<Option<i64>>;
    fn remaining_playback_sampler(&self) -> AddressedSampler<i64>;
    fn user_sequence_sampler(&self) -> AddressedSampler<u64>;
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
        -> Result<FenceHandle, CandidateFenceError>;
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
    AbrHlsStart { kbps: i64 },
    AbrHlsDeclared { kbps: i64 },
    /// The adaptive worker committed a new HLS rung: its target and the declaration of the cursor it
    /// promoted, in kbit/s. Published by the feed's own port at the promotion, so the demux facts
    /// keep one writer.
    AbrCommitted { kbps: i64, declared_kbps: i64 },
    AbrOriginalStart { kbps: i64 },
    NoteFault(DemuxFault),
    FailFault(DemuxFault),
    IoFailed,
    Failed,
    NoVideo,
    /// The codec the file ACTUALLY carries, in the `dg_load_v` / `dg_load_a` code spaces, published
    /// only when the demuxer refuses a Load whose bytes disagree with its declared codec. The
    /// pump's one-shot probe retry consumes it to reload with the corrected declaration; a healthy
    /// open publishes nothing.
    ProbeActual { video: Option<u8>, audio: Option<u8> },
    HttpStatus(i32),
    TrackNames { audio: Vec<String>, subs: Vec<String> },
    SideAnchor(Option<Anchor>),
    ContainerHdr(Option<crate::video::ContainerHdr>),
    FirstAudioPts(i64),
    FileSize(i64),
    TransportRc(i32),
    NetworkBytes(i64),
    DurationNs(i64),
    VideoRaster { width: i32, height: i32 },
    VideoTailNs(i64),
    AudioTailNs(i64),
    AudioExpected(bool),
    PrimeRunwayMax(i64),
    /// The adaptive controller's prime runway (ms), replacing the published one: a completed round or a
    /// committed trial. [`Self::PrimeRunwayMax`] only raises it.
    PrimeRunway(i64),
    RecoverySegment { total_us: u64, duration: Duration },
}

pub trait DemuxMedia {
    fn publish(&mut self, effect: DemuxEffect);
    fn record(&self, line: &str);
}

/// Subtitle presentation belongs to the player session. The feed supplies only
/// file-order inventory and owned packet observations; no FFmpeg handle, charset
/// state or render-store cue crosses this boundary.
pub trait DemuxSubtitle {
    fn plan(&mut self, tracks: &[TrackInfo]) -> Vec<DecodeKind>;
    fn max_header_bytes(&self) -> usize;
    fn admit_font(&mut self, bytes: usize) -> bool;
    fn begin_sources(&mut self, media_key: &str, manifest: Manifest) -> Option<u64>;
    fn record_tracks(&self);
    fn selected_track(&self) -> i32;
    fn bitmap_budget(&self) -> usize;
    fn emit(&mut self, generation: u64, cue: RawCue);
    fn seek(&mut self, generation: u64);
}

/// One move-only adapter follows the media open into its existing feed worker.
pub trait DemuxSession: Send {
    fn observer(&self) -> Arc<dyn DemuxObserver>;
    fn clock(&self) -> &dyn DemuxClock;
    fn clock_mut(&mut self) -> &mut dyn DemuxClock;
    fn adaptive_ref(&self) -> &dyn DemuxAdaptive;
    fn adaptive(&mut self) -> &mut dyn DemuxAdaptive;
    fn media(&mut self) -> &mut dyn DemuxMedia;
    fn subtitle(&mut self) -> &mut dyn DemuxSubtitle;
}
