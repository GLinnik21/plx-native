//! **Playback lifecycle events joined by one attempt.**
//!
//! `requested -> started -> failed|ended|abandoned`, or
//! `requested -> failed|cancelled|abandoned`. The interesting number is the gap between requested
//! and a terminal outcome: `started / requested` is the startup success rate without leaving a
//! silent unresolved bucket for "it just sat there".
//!
//! The error-consent channel also keeps a separate sparse, typed trace in memory across reloads of
//! this same attempt. It queues exactly one handled Sentry event only if the derived state reaches
//! `Error`; ordinary low quality, buffering and rejected ABR candidates remain breadcrumbs at most,
//! never errors of their own.
//!
//! # These are TRANSITIONS, not log lines
//!
//! The first design put `playback.started` on the engine's `load:` line. That line is the wrong
//! seam and the name would have been a lie: it is emitted BEFORE the source is opened and before
//! anything plays, so a television that never produced a frame would report a start. It is
//! `requested` now, and `started` fires on the first transition into `Playing` — the same value the
//! HUD renders, so the event says what the viewer saw.
//!
//! # Observed at the DERIVED state, not at `pump::set_state`
//!
//! `set_state` looks like the choke point and is not: [`super::state`] derives two of its answers
//! outside `pb_state` entirely — `Resolving` while a plan is in flight, and `Error` for a
//! `/decision` refusal, which happens before an engine exists and so before the pump has ever run.
//! Hooking the setter would have silently missed the earliest and most certain failure there is.
//! So this observes the value the HUD reads, once a frame.
//!
//! # Once each, and only for a REAL end
//!
//! `Playing` is re-entered after every seek and every reload, and `Error` can be republished on
//! consecutive frames; the latch is what makes each event mean "this attempt", not "this frame".
//! And `ended` fires on a genuine teardown only — a seek, an ABR rung change and an app-switch
//! suspend all end an ENGINE without ending a playback, and counting those as endings would make
//! the completion rate a measure of how often people scrub.

use crate::diag::schema::DiagEvent;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU16, AtomicU32, AtomicU8, Ordering::Relaxed};

/// The current attempt's opaque id — random, per attempt, never stored. See `DiagEvent`'s playback
/// block: it joins one attempt's lifecycle events and cannot link two playbacks, let alone two sets.
static ATTEMPT: AtomicI64 = AtomicI64::new(0);
/// Registry slot of the server this attempt addresses. Captured when the plan commits so a later
/// server switch cannot relabel the attempt's analytics.
static ATTEMPT_SERVER: AtomicU16 = AtomicU16::new(crate::plex::ServerId::UNSET.raw());
/// Process-local trace generation. Unlike `ATTEMPT`, this is never sent; it only prevents an
/// outgoing demux worker from writing its late transitions into the next Play's reset trace.
static NEXT_TRACE_GENERATION: AtomicU32 = AtomicU32::new(0);
/// The last state this module reported a transition FROM.
static LAST: AtomicU8 = AtomicU8::new(0);
/// One `started`/`failed`/`ended` per attempt.
static SAW_START: AtomicBool = AtomicBool::new(false);
static SAW_FAIL: AtomicBool = AtomicBool::new(false);
static SAW_END: AtomicBool = AtomicBool::new(false);
/// At most one bounded rebuffer summary, emitted when a started attempt reaches an observable
/// terminal or replacement path. The app deliberately does not report dropped frames: LG's
/// position callback is a fixed 5 Hz clock and looks identical on smooth and visibly stuttering
/// playback, so treating it as frame cadence would fabricate data.
static SAW_QUALITY: AtomicBool = AtomicBool::new(false);
static REBUFFER_COUNT: AtomicU8 = AtomicU8::new(0);
static REBUFFER_AT_MS: AtomicI64 = AtomicI64::new(0);
static REBUFFER_TOTAL_MS: AtomicI64 = AtomicI64::new(0);
/// When this attempt was requested, in `SDL_GetTicks` milliseconds — the same monotonic clock every
/// other timestamp in this app uses, because pmlog's wall clock on this television runs ~3h off.
static REQUESTED_MS: AtomicI64 = AtomicI64::new(0);

/// At most this many sparse state changes survive until a terminal playback error. Segment fetches
/// are deliberately absent: one entry per segment would both drown the causal transitions and turn
/// a long film into a larger report than a short one.
pub(crate) const ERROR_TRACE_MAX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceAge {
    Under1s,
    S1To3,
    S3To10,
    S10To30,
    S30To120,
    Over2m,
}

impl TraceAge {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Under1s => "<1s",
            Self::S1To3 => "1-3s",
            Self::S3To10 => "3-10s",
            Self::S10To30 => "10-30s",
            Self::S30To120 => "30-120s",
            Self::Over2m => "2m+",
        }
    }

    fn from_ms(ms: i64) -> Self {
        match ms.max(0) {
            0..=999 => Self::Under1s,
            1_000..=2_999 => Self::S1To3,
            3_000..=9_999 => Self::S3To10,
            10_000..=29_999 => Self::S10To30,
            30_000..=119_999 => Self::S30To120,
            _ => Self::Over2m,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryClass {
    Direct,
    Remux,
    Hls,
    Transcode,
}

impl DeliveryClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Direct => "original_direct",
            Self::Remux => "original_remux",
            Self::Hls => "hls",
            Self::Transcode => "progressive_transcode",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QualityClass {
    Unknown,
    Auto,
    Original,
    K320,
    K720,
    M2,
    M4,
    M6,
    M8,
    M10,
    M12,
    M14,
    M16,
    M18,
    M20,
    M22,
}

impl QualityClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Auto => "auto",
            Self::Original => "original",
            Self::K320 => "320k",
            Self::K720 => "720k",
            Self::M2 => "2m",
            Self::M4 => "4m",
            Self::M6 => "6m",
            Self::M8 => "8m",
            Self::M10 => "10m",
            Self::M12 => "12m",
            Self::M14 => "14m",
            Self::M16 => "16m",
            Self::M18 => "18m",
            Self::M20 => "20m",
            Self::M22 => "22m",
        }
    }

    fn selected(q: crate::route::Quality) -> Self {
        use crate::route::Quality as Q;
        match q {
            Q::Auto => Self::Auto,
            Q::Original => Self::Original,
            Q::P1080High => Self::M20,
            Q::P1080 => Self::M8,
            Q::P720 => Self::M4,
            Q::P720Low => Self::M2,
            Q::P480 => Self::K720,
        }
    }

    pub(crate) fn from_rung(rung: crate::abr::Rung) -> Self {
        Self::from_kbps(i64::from(rung.kbps()))
    }

    pub(crate) fn from_kbps(kbps: i64) -> Self {
        match kbps {
            320 => Self::K320,
            720 => Self::K720,
            2_000 => Self::M2,
            4_000 => Self::M4,
            6_000 => Self::M6,
            8_000 => Self::M8,
            10_000 => Self::M10,
            12_000 => Self::M12,
            14_000 => Self::M14,
            16_000 => Self::M16,
            18_000 => Self::M18,
            20_000 => Self::M20,
            22_000 => Self::M22,
            _ => Self::Unknown,
        }
    }
}

/// Privacy-preserving buckets for rates PMS actually declared or emitted. These are observations,
/// not controller rungs: keeping the type separate prevents a 5.5 Mbit/s server response from being
/// mislabeled as the 22 Mbit/s actuator that requested it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RateClass {
    Unknown,
    Under1m,
    M1To3,
    M3To6,
    M6To12,
    M12To20,
    Over20m,
}

impl RateClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Under1m => "<1m",
            Self::M1To3 => "1-3m",
            Self::M3To6 => "3-6m",
            Self::M6To12 => "6-12m",
            Self::M12To20 => "12-20m",
            Self::Over20m => "20m+",
        }
    }

    fn from_kbps(kbps: i64) -> Self {
        match kbps {
            k if k <= 0 => Self::Unknown,
            1..=999 => Self::Under1m,
            1_000..=2_999 => Self::M1To3,
            3_000..=5_999 => Self::M3To6,
            6_000..=11_999 => Self::M6To12,
            12_000..=19_999 => Self::M12To20,
            _ => Self::Over20m,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RasterClass {
    Unknown,
    Sd,
    Hd,
    Fhd,
    Uhd,
}

impl RasterClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Sd => "sd",
            Self::Hd => "hd",
            Self::Fhd => "fhd",
            Self::Uhd => "uhd",
        }
    }

    fn from_height(height: i32) -> Self {
        match height {
            h if h <= 0 => Self::Unknown,
            h if h <= 576 => Self::Sd,
            h if h <= 720 => Self::Hd,
            h if h <= 1080 => Self::Fhd,
            _ => Self::Uhd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceDirection {
    Up,
    Down,
    Refresh,
}

impl TraceDirection {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeliveryReason {
    LinkFallback,
    OriginalRecovery,
    OriginalOpenRollback,
}

impl DeliveryReason {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::LinkFallback => "link_fallback",
            Self::OriginalRecovery => "original_recovery",
            Self::OriginalOpenRollback => "original_open_rollback",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OriginalProbePhase {
    // Retained stable wire vocabulary for events produced by builds before 2026-08-31. Current
    // runtime emits SampleSource only; removing/reusing these strings would rewrite dashboards.
    RetireHls,
    SampleSource,
    CloseSource,
    RestoreHls,
    OpenHls,
    CommitHls,
}

impl OriginalProbePhase {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::RetireHls => "retire_hls",
            Self::SampleSource => "sample_source",
            Self::CloseSource => "close_source",
            Self::RestoreHls => "restore_hls",
            Self::OpenHls => "open_hls",
            Self::CommitHls => "commit_hls",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceOutcome {
    Started,
    Succeeded,
    NoBody,
    Deadline,
    Transport,
    /// The app observed failure but the available signal does not distinguish a moved local
    /// session, a missing client or another control-plane circumstance.
    Inconclusive,
    ServerState,
    Refused,
}

impl TraceOutcome {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Succeeded => "succeeded",
            Self::NoBody => "no_body",
            Self::Deadline => "deadline",
            Self::Transport => "transport",
            Self::Inconclusive => "inconclusive",
            Self::ServerState => "server_state",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TraceEvent {
    Requested {
        selected: QualityClass,
    },
    Presented {
        delivery: DeliveryClass,
        requested: QualityClass,
        declared_rate: RateClass,
        raster: RasterClass,
    },
    SeekRequested,
    QualitySelected {
        selected: QualityClass,
    },
    DeliveryRequested {
        delivery: DeliveryClass,
        requested: QualityClass,
        reason: DeliveryReason,
    },
    HlsCommitted {
        direction: TraceDirection,
        requested: QualityClass,
    },
    OriginalProbe {
        phase: OriginalProbePhase,
        outcome: TraceOutcome,
    },
    Failed {
        kind: super::FailureKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TraceStep {
    pub(crate) age: TraceAge,
    pub(crate) event: TraceEvent,
}

/// The current play attempt's in-memory, privacy-bounded history. It lives in [`super::SHARED`]
/// because both the demux worker (ABR/probe) and main thread (request/seek/error) append to it.
/// Engine reloads do not clear it; a new play attempt does.
pub(crate) struct PlaybackTrace {
    generation: u32,
    started_ms: i64,
    sealed: bool,
    steps: Vec<TraceStep>,
}

impl PlaybackTrace {
    pub(crate) const fn new() -> Self {
        Self {
            generation: 0,
            started_ms: -1,
            sealed: false,
            steps: Vec::new(),
        }
    }

    fn reset(&mut self, generation: u32, at_ms: i64, selected: QualityClass) {
        self.generation = generation;
        self.started_ms = at_ms;
        self.sealed = false;
        self.steps.clear();
        self.push(at_ms, TraceEvent::Requested { selected });
    }

    /// Install an attempt boundary without collecting a breadcrumb. This keeps a later opt-in
    /// scoped to the current Play while preserving the rule that nothing is collected before it.
    fn arm(&mut self, generation: u32) {
        self.generation = generation;
        self.started_ms = -1;
        self.sealed = false;
        self.steps.clear();
    }

    fn push_for(&mut self, generation: u32, at_ms: i64, event: TraceEvent) -> bool {
        if generation == 0 || self.generation != generation || self.sealed {
            return false;
        }
        self.push(at_ms, event);
        true
    }

    fn push(&mut self, at_ms: i64, event: TraceEvent) {
        if self.steps.len() == ERROR_TRACE_MAX {
            // Keep the attempt boundary when it exists and retire the oldest interior transition.
            let drop_at = usize::from(matches!(
                self.steps.first().map(|s| s.event),
                Some(TraceEvent::Requested { .. })
            ));
            self.steps.remove(drop_at.min(self.steps.len() - 1));
        }
        if self.started_ms < 0 {
            self.started_ms = at_ms;
        }
        self.steps.push(TraceStep {
            age: TraceAge::from_ms(at_ms.saturating_sub(self.started_ms)),
            event,
        });
    }

    fn clear(&mut self) {
        self.generation = 0;
        self.started_ms = -1;
        self.sealed = false;
        self.steps.clear();
    }

    fn snapshot(&self) -> Vec<TraceStep> {
        self.steps.clone()
    }

    fn finish(&mut self, at_ms: i64, event: TraceEvent) -> Vec<TraceStep> {
        if !self.sealed {
            self.push(at_ms, event);
            self.sealed = true;
        }
        self.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn seed_for_reset_test(&mut self) {
        self.reset(42, 0, QualityClass::Auto);
    }

    #[cfg(test)]
    pub(crate) fn step_count_for_test(&self) -> usize {
        self.steps.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PipelineClass {
    Loading,
    Playing,
    Bound,
    Streaming,
}

impl PipelineClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Playing => "playing",
            Self::Bound => "bound",
            Self::Streaming => "streaming",
        }
    }

    fn from_stage(stage: u8) -> Self {
        match stage {
            1 => Self::Playing,
            2 => Self::Bound,
            3 => Self::Streaming,
            _ => Self::Loading,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HttpClass {
    None,
    Success,
    ClientError,
    ServerError,
    Other,
}

impl HttpClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Success => "2xx",
            Self::ClientError => "4xx",
            Self::ServerError => "5xx",
            Self::Other => "other",
        }
    }

    fn from_status(status: i32) -> Self {
        match status {
            0 => Self::None,
            200..=299 => Self::Success,
            400..=499 => Self::ClientError,
            500..=599 => Self::ServerError,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BufferClass {
    Unknown,
    Empty,
    Under3s,
    S3To10,
    S10To30,
    Over30s,
}

impl BufferClass {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Empty => "empty",
            Self::Under3s => "<3s",
            Self::S3To10 => "3-10s",
            Self::S10To30 => "10-30s",
            Self::Over30s => "30s+",
        }
    }

    fn from_ms(ms: i64) -> Self {
        match ms {
            m if m < 0 => Self::Unknown,
            0 => Self::Empty,
            1..=2_999 => Self::Under3s,
            3_000..=9_999 => Self::S3To10,
            10_000..=29_999 => Self::S10To30,
            _ => Self::Over30s,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlaybackErrorContext {
    pub(crate) delivery: DeliveryClass,
    pub(crate) selected: QualityClass,
    pub(crate) requested: QualityClass,
    pub(crate) declared_rate: RateClass,
    pub(crate) media_rate: RateClass,
    pub(crate) raster: RasterClass,
    pub(crate) pipeline: PipelineClass,
    pub(crate) http: HttpClass,
    pub(crate) buffer: BufferClass,
    pub(crate) started: bool,
}

fn delivery_class() -> DeliveryClass {
    if crate::route::is_segmented_hls() {
        DeliveryClass::Hls
    } else if crate::route::is_transcoding() && crate::route::is_remux() {
        DeliveryClass::Remux
    } else if crate::route::is_transcoding() {
        DeliveryClass::Transcode
    } else {
        DeliveryClass::Direct
    }
}

fn push_trace_for(generation: u32, event: TraceEvent) {
    let mut trace = super::SHARED
        .playback_trace
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    // Check consent while holding the same trace lock used by withdrawal's clear. Either this
    // append finishes before the clear (and is erased) or it observes the new decision and does
    // nothing; no breadcrumb can appear after withdrawal and survive it.
    if !crate::telemetry::consent::allows_errors() {
        return;
    }
    trace.push_for(generation, now_ms(), event);
}

fn push_trace(event: TraceEvent) {
    let mut trace = super::SHARED
        .playback_trace
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !crate::telemetry::consent::allows_errors() || trace.generation == 0 {
        return;
    }
    let generation = trace.generation;
    trace.push_for(generation, now_ms(), event);
}

/// Append the terminal step, seal the attempt against late worker writes, and take the report
/// snapshot under one lock. A split append/snapshot lets an ABR worker put an event *after*
/// `playback failed`, producing a causal sequence that never happened.
fn finish_trace(event: TraceEvent) -> Vec<TraceStep> {
    let mut trace = super::SHARED
        .playback_trace
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if !crate::telemetry::consent::allows_errors() {
        trace.sealed = true;
        return Vec::new();
    }
    trace.finish(now_ms(), event)
}

/// Forget the in-memory error trace immediately when error reporting is withdrawn.
pub(crate) fn clear_error_trace() {
    super::SHARED
        .playback_trace
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

pub(crate) fn note_seek_for(generation: u32) {
    push_trace_for(generation, TraceEvent::SeekRequested);
}

pub(crate) fn note_quality_selected_for(generation: u32, q: crate::route::Quality) {
    push_trace_for(
        generation,
        TraceEvent::QualitySelected {
            selected: QualityClass::selected(q),
        },
    );
}

pub(crate) fn note_delivery_requested_for(
    generation: u32,
    delivery: DeliveryClass,
    requested: QualityClass,
    reason: DeliveryReason,
) {
    push_trace_for(
        generation,
        TraceEvent::DeliveryRequested {
            delivery,
            requested,
            reason,
        },
    );
}

pub(crate) fn note_hls_committed_for(
    generation: u32,
    direction: crate::abr::Direction,
    rung: crate::abr::Rung,
    refresh: bool,
) {
    let direction = if refresh {
        TraceDirection::Refresh
    } else {
        match direction {
            crate::abr::Direction::Up => TraceDirection::Up,
            crate::abr::Direction::Down => TraceDirection::Down,
        }
    };
    push_trace_for(
        generation,
        TraceEvent::HlsCommitted {
            direction,
            requested: QualityClass::from_rung(rung),
        },
    );
}

pub(crate) fn note_original_probe_for(
    generation: u32,
    phase: OriginalProbePhase,
    outcome: TraceOutcome,
) {
    push_trace_for(generation, TraceEvent::OriginalProbe { phase, outcome });
}

fn error_context() -> PlaybackErrorContext {
    let delivery = delivery_class();
    let selected = QualityClass::selected(crate::route::quality());
    let requested = if delivery == DeliveryClass::Hls {
        QualityClass::from_kbps(super::SHARED.dg_abr_kbps.load(Relaxed))
    } else if matches!(delivery, DeliveryClass::Direct | DeliveryClass::Remux) {
        QualityClass::Original
    } else {
        selected
    };
    PlaybackErrorContext {
        delivery,
        selected,
        requested,
        declared_rate: RateClass::from_kbps(super::SHARED.dg_abr_declared_kbps.load(Relaxed)),
        media_rate: RateClass::from_kbps(super::SHARED.dg_abr_media_kbps.load(Relaxed)),
        raster: RasterClass::from_height(super::SHARED.video_raster().1),
        pipeline: PipelineClass::from_stage(super::SHARED.dg_stage.load(Relaxed)),
        http: HttpClass::from_status(super::SHARED.dg_http_status.load(Relaxed)),
        buffer: BufferClass::from_ms(super::SHARED.dg_abr_buffer_ms.load(Relaxed)),
        started: SAW_START.load(Relaxed),
    }
}

fn presented_event() -> TraceEvent {
    let delivery = delivery_class();
    let requested = if delivery == DeliveryClass::Hls {
        QualityClass::from_kbps(super::SHARED.dg_abr_kbps.load(Relaxed))
    } else if matches!(delivery, DeliveryClass::Direct | DeliveryClass::Remux) {
        QualityClass::Original
    } else {
        QualityClass::selected(crate::route::quality())
    };
    TraceEvent::Presented {
        delivery,
        requested,
        declared_rate: RateClass::from_kbps(super::SHARED.dg_abr_declared_kbps.load(Relaxed)),
        raster: RasterClass::from_height(super::SHARED.video_raster().1),
    }
}

/// **A new attempt.** Called where the app commits to a plan, before anything opens a socket.
///
/// Mints the id and clears every latch, so a second Play on the same item is a second attempt with
/// its own funnel rather than a silent no-op against the first one's latches.
pub(crate) fn requested(server: crate::plex::ServerId) -> u32 {
    resolve_replaced_attempt();
    let id = new_attempt_id();
    let at = now_ms();
    // **`fetch_update`'s own read-modify-write loop, written out.** That method was deprecated on
    // nightly in favour of `try_update`, and `make lint` denies warnings, so CI goes red on a
    // toolchain this repo deliberately does not pin — while renaming would stop every checkout on
    // an older nightly compiling at all. This is exactly the loop the method runs internally, it
    // predates both spellings, and the closure here never declined an update anyway.
    let previous = {
        let mut current = NEXT_TRACE_GENERATION.load(Relaxed);
        loop {
            let next = if current == u32::MAX { 1 } else { current + 1 };
            match NEXT_TRACE_GENERATION.compare_exchange_weak(current, next, Relaxed, Relaxed) {
                Ok(previous) => break previous,
                Err(actual) => current = actual,
            }
        }
    };
    let generation = if previous == u32::MAX {
        1
    } else {
        previous + 1
    };
    ATTEMPT.store(id, Relaxed);
    ATTEMPT_SERVER.store(server.raw(), Relaxed);
    SAW_START.store(false, Relaxed);
    SAW_FAIL.store(false, Relaxed);
    SAW_END.store(false, Relaxed);
    SAW_QUALITY.store(false, Relaxed);
    REBUFFER_COUNT.store(0, Relaxed);
    REBUFFER_AT_MS.store(0, Relaxed);
    REBUFFER_TOTAL_MS.store(0, Relaxed);
    REQUESTED_MS.store(at, Relaxed);
    LAST.store(super::shared::PlaybackState::Resolving as u8, Relaxed);
    let mut trace = super::SHARED
        .playback_trace
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if crate::telemetry::consent::allows_errors() {
        trace.reset(
            generation,
            at,
            QualityClass::selected(crate::route::quality()),
        );
    } else {
        trace.arm(generation);
    }
    drop(trace);
    emit(DiagEvent::PlaybackRequested { playback_id: id });
    generation
}

fn emit(event: DiagEvent) {
    let sid = crate::plex::ServerId::from_raw(ATTEMPT_SERVER.load(Relaxed));
    crate::diag::event_for_server(event, sid);
}

/// Resolve an attempt before a newer Play overwrites its join key. Before first frame this is an
/// explicit cancellation; after first frame it is an abandoned viewing session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Replacement {
    None,
    Cancelled,
    Abandoned,
}

fn replacement(saw_start: bool, saw_fail: bool, saw_end: bool) -> Replacement {
    if saw_fail || saw_end {
        Replacement::None
    } else if saw_start {
        Replacement::Abandoned
    } else {
        Replacement::Cancelled
    }
}

fn resolve_replaced_attempt() {
    let id = ATTEMPT.swap(0, Relaxed);
    if id == 0 {
        return;
    }
    match replacement(
        SAW_START.load(Relaxed),
        SAW_FAIL.load(Relaxed),
        SAW_END.load(Relaxed),
    ) {
        Replacement::None => {}
        Replacement::Cancelled => {
            emit(DiagEvent::PlaybackCancelled {
                playback_id: id,
                mode: mode(),
            });
        }
        Replacement::Abandoned => {
            report_quality(id);
            emit(DiagEvent::PlaybackAbandoned {
                playback_id: id,
                mode: mode(),
            });
        }
    }
}

/// The process is leaving an unresolved attempt. Unlike a newer Play, this was not a replacement
/// choice, so classify it as abandonment; if playback had started, close its quality summary too.
pub(crate) fn abandon_pending() {
    let id = ATTEMPT.load(Relaxed);
    if id == 0 || SAW_FAIL.load(Relaxed) || SAW_END.swap(true, Relaxed) {
        return;
    }
    report_quality(id);
    emit(DiagEvent::PlaybackAbandoned {
        playback_id: id,
        mode: mode(),
    });
}

/// What a frame's state change is worth reporting, if anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum What {
    Started,
    Failed,
}

/// **The rule, pure — because this is the part that decides whether a number is right or double.**
///
/// Everything a dashboard says about playback rests on each of these firing exactly once per
/// attempt, and the two ways to get it wrong are invisible from the dashboard itself: a `started`
/// that re-fires makes the success rate exceed 100% quietly, and one that fires on a rebuffer makes
/// heavy scrubbers look like heavy watchers. Neither is observable without a test, so the decision
/// is separated from the globals and graded on the host.
fn transition(
    prev: super::shared::PlaybackState,
    now: super::shared::PlaybackState,
    saw_start: bool,
    saw_fail: bool,
) -> Option<What> {
    use super::shared::PlaybackState as S;
    if prev == now {
        return None;
    }
    match now {
        // Re-entered after every seek, every ABR rung change and every rebuffer — the latch is what
        // makes this event mean "this attempt", not "this frame".
        S::Playing if !saw_start => Some(What::Started),
        // Republished on consecutive frames while the read-out is up, and reachable twice if a
        // transient state passes through between. Same latch, same reason.
        S::Error if !saw_fail => Some(What::Failed),
        _ => None,
    }
}

/// **Observe the state the HUD is rendering.** Called once a frame from the main loop.
pub(crate) fn tick() {
    let now = super::state();
    let prev = super::shared::PlaybackState::from_u8(LAST.swap(now as u8, Relaxed));
    note_rebuffer(prev, now, SAW_START.load(Relaxed));
    if prev != now && now == super::shared::PlaybackState::Playing {
        // Unlike the usage funnel's once-per-attempt `Started`, every return to actual presented
        // video is useful causal evidence after a seek or delivery reload.
        push_trace(presented_event());
    }
    match transition(prev, now, SAW_START.load(Relaxed), SAW_FAIL.load(Relaxed)) {
        Some(What::Started) => {
            SAW_START.store(true, Relaxed);
            emit(DiagEvent::PlaybackStarted {
                playback_id: ATTEMPT.load(Relaxed),
                mode: mode(),
                raster: raster_class(super::SHARED.video_raster().1),
                fps: fps_rung(crate::route::stream_fps()),
                video: video_codec_class(&crate::route::stream_vcodec()),
                audio: audio_codec_class(&crate::route::stream_acodec()),
                startup: startup_class(now_ms() - REQUESTED_MS.load(Relaxed)),
            });
        }
        Some(What::Failed) => {
            SAW_FAIL.store(true, Relaxed);
            report_quality(ATTEMPT.load(Relaxed));
            let shape = super::error_now();
            let trace = finish_trace(TraceEvent::Failed { kind: shape.kind });
            crate::telemetry::playback::report_error(shape.kind, error_context(), &trace);
            emit(DiagEvent::PlaybackFailed {
                playback_id: ATTEMPT.load(Relaxed),
                mode: mode(),
                kind: shape.kind.code(),
            });
        }
        None => {}
    }
}

/// **A real teardown.** Called from the one place playback actually ends — never from a seek, a
/// rung change or a suspend, each of which destroys an engine and keeps the playback.
pub(crate) fn ended(position_ns: i64, duration_ns: i64) {
    if SAW_END.swap(true, Relaxed) || SAW_FAIL.load(Relaxed) {
        clear_error_trace();
        return; // already terminal
    }
    let id = ATTEMPT.load(Relaxed);
    if id == 0 {
        clear_error_trace();
        return;
    }
    if !SAW_START.load(Relaxed) {
        emit(DiagEvent::PlaybackAbandoned {
            playback_id: id,
            mode: mode(),
        });
        clear_error_trace();
        return;
    }
    report_quality(id);
    emit(DiagEvent::PlaybackEnded {
        playback_id: id,
        mode: mode(),
        watched: watched_class(position_ns, duration_ns),
    });
    clear_error_trace();
}

fn note_rebuffer(
    prev: super::shared::PlaybackState,
    now: super::shared::PlaybackState,
    saw_start: bool,
) {
    use super::shared::PlaybackState as S;
    if starts_rebuffer(prev, now, saw_start) {
        // A saturating increment, as a loop; see `requested` for why not `fetch_update`.
        let mut current = REBUFFER_COUNT.load(Relaxed);
        while let Err(actual) =
            REBUFFER_COUNT.compare_exchange_weak(current, current.saturating_add(1), Relaxed, Relaxed)
        {
            current = actual;
        }
        REBUFFER_AT_MS.store(now_ms().max(1), Relaxed);
    } else if now != S::Buffering {
        finish_rebuffer_window();
    }
}

fn starts_rebuffer(
    prev: super::shared::PlaybackState,
    now: super::shared::PlaybackState,
    saw_start: bool,
) -> bool {
    use super::shared::PlaybackState as S;
    saw_start && prev == S::Playing && now == S::Buffering
}

fn finish_rebuffer_window() {
    let at = REBUFFER_AT_MS.swap(0, Relaxed);
    if at > 0 {
        REBUFFER_TOTAL_MS.fetch_add((now_ms() - at).max(0), Relaxed);
    }
}

fn report_quality(playback_id: i64) {
    if playback_id == 0 || !SAW_START.load(Relaxed) || SAW_QUALITY.swap(true, Relaxed) {
        return;
    }
    finish_rebuffer_window();
    emit(DiagEvent::PlaybackQuality {
        playback_id,
        rebuffers: rebuffer_count_class(REBUFFER_COUNT.load(Relaxed)),
        buffering: rebuffer_time_class(REBUFFER_TOTAL_MS.load(Relaxed)),
    });
}

fn rebuffer_count_class(n: u8) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=3 => "2-3",
        _ => "4+",
    }
}

fn rebuffer_time_class(ms: i64) -> &'static str {
    match ms {
        ..=0 => "none",
        1..=1_999 => "<2s",
        2_000..=9_999 => "2-10s",
        _ => "10s+",
    }
}

fn mode() -> &'static str {
    if crate::route::is_transcoding() {
        "transcode"
    } else {
        "direct"
    }
}

/// Milliseconds since this process started, monotonic.
///
/// **`std::time::Instant`, not `SDL_GetTicks`**, and the reason is the host suite rather than
/// taste: `cargo test --lib` links no SDL, so a `SDL_GetTicks` here does not skip a test — it stops
/// the whole suite LINKING, which is the boundary `ui/CLAUDE.md` records for `TTF_SizeUTF8`. Only
/// the DIFFERENCE of two readings is ever used, so any monotonic origin will do.
///
/// Never a wall clock either way: pmlog's on this television runs about three hours off, which is
/// why `docs/agent-reference.md` says to correlate a crash by monotonic time and not by time of day.
fn now_ms() -> i64 {
    static ORIGIN: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    ORIGIN
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as i64
}

/// A random attempt id. `/dev/urandom` like every other random value in this crate — never a clock
/// or a counter, both of which would say something about the television across attempts.
fn new_attempt_id() -> i64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        return 0; // no randomness: the funnel loses its join and nothing else
    }
    (i64::from_le_bytes(b) & i64::MAX) as i64
}

/// The video codec, from a CLOSED table.
///
/// `route::stream_vcodec` hands back a `String` off the wire, and `diag::schema` has no arm that
/// could carry one — deliberately, that being the property that makes "no runtime string reaches
/// the wire" a fact about the type. So the mapping is here: a name the table does not know becomes
/// `other`, which is a real answer (it means the server sent something this app did not expect) and
/// cannot become a leak.
pub(crate) fn video_codec_class(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "h264" | "avc" | "avc1" => "h264",
        "hevc" | "h265" | "hvc1" => "hevc",
        "av1" => "av1",
        "vp9" => "vp9",
        "mpeg2video" | "mpeg2" => "mpeg2",
        "" => "unknown",
        _ => "other",
    }
}

/// The audio codec, from a closed table, for [`video_codec_class`]'s reason.
pub(crate) fn audio_codec_class(name: &str) -> &'static str {
    match name.to_ascii_lowercase().as_str() {
        "aac" => "aac",
        "ac3" => "ac3",
        "eac3" | "ac3 plus" | "ec-3" => "eac3",
        "truehd" => "truehd",
        "dts" | "dca" => "dts",
        "flac" => "flac",
        "mp3" => "mp3",
        "opus" => "opus",
        "" => "unknown",
        _ => "other",
    }
}

// ---- the buckets, which are the privacy decision --------------------------------------------
//
// Exact duration + exact raster + exact frame rate + codec identifies a specific file in a specific
// library. As classes they answer every question this channel exists to answer — does 4K HEVC fail
// more than 1080p h264, does startup get worse on big files — and identify nothing. All pure, so
// every boundary is graded on the host.

/// The four rungs the whole project already reasons in — the pipeline tier's resolution matrix, the
/// PMS decision's own classes, LG's checklist. Named rather than measured for the reason above.
pub(crate) fn raster_class(height: i32) -> &'static str {
    RasterClass::from_height(height).code()
}

/// A fixed rung, so 23.976 and 24.000 are one bucket rather than two — the distinction is a
/// fingerprint of a particular encode and answers nothing. Anything off the ladder is `other`
/// rather than the nearest rung: a genuinely odd rate is a fact worth being able to see.
pub(crate) fn fps_rung(fps: f64) -> &'static str {
    const RUNGS: [(f64, &str); 6] = [
        (24.0, "24"),
        (25.0, "25"),
        (30.0, "30"),
        (50.0, "50"),
        (60.0, "60"),
        (100.0, "100"),
    ];
    if !(fps > 0.0) {
        return "unknown";
    }
    // 1.5% either side, which separates every rung above and still catches both spellings of each
    // (23.976/24, 29.97/30, 59.94/60 — the 1001-denominator forms this project's own fixtures use).
    RUNGS
        .iter()
        .find(|(r, _)| (fps - r).abs() / r <= 0.015)
        .map(|(_, n)| *n)
        .unwrap_or("other")
}

/// How long the viewer waited for a picture. The boundaries are where the EXPERIENCE changes, not
/// round numbers: under a second reads as instant, three seconds is where a person starts to wonder,
/// ten is where they press something.
pub(crate) fn startup_class(ms: i64) -> &'static str {
    match ms {
        m if m < 0 => "unknown",
        m if m < 1_000 => "<1s",
        m if m < 3_000 => "1-3s",
        m if m < 10_000 => "3-10s",
        _ => "10s+",
    }
}

/// How much of it was watched, as the four answers anyone asks of a completion rate. Not a
/// percentage: a percentage plus a duration bucket is a duration, which is the fingerprint the
/// buckets exist to avoid.
pub(crate) fn watched_class(position_ns: i64, duration_ns: i64) -> &'static str {
    if duration_ns <= 0 || position_ns < 0 {
        return "unknown";
    }
    match (position_ns as f64) / (duration_ns as f64) {
        f if f < 0.05 => "abandoned",
        f if f < 0.5 => "some",
        f if f < 0.9 => "most",
        _ => "finished",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_an_attempt_always_gives_the_old_one_a_terminal_outcome() {
        assert_eq!(replacement(false, false, false), Replacement::Cancelled);
        assert_eq!(replacement(true, false, false), Replacement::Abandoned);
        assert_eq!(replacement(false, true, false), Replacement::None);
        assert_eq!(replacement(true, false, true), Replacement::None);
    }

    #[test]
    fn quality_is_bounded_and_seek_priming_is_not_a_rebuffer() {
        assert_eq!(rebuffer_count_class(0), "0");
        assert_eq!(rebuffer_count_class(1), "1");
        assert_eq!(rebuffer_count_class(3), "2-3");
        assert_eq!(rebuffer_count_class(u8::MAX), "4+");
        assert_eq!(rebuffer_time_class(0), "none");
        assert_eq!(rebuffer_time_class(1_999), "<2s");
        assert_eq!(rebuffer_time_class(2_000), "2-10s");
        assert_eq!(rebuffer_time_class(10_000), "10s+");
        assert!(starts_rebuffer(S::Playing, S::Buffering, true));
        assert!(!starts_rebuffer(S::Seeking, S::Buffering, true));
        assert!(!starts_rebuffer(S::Playing, S::Buffering, false));
    }

    use super::super::shared::PlaybackState as S;

    /// Drive a sequence of states through the rule the way a frame loop would, latches and all, and
    /// report what it emitted.
    fn drive(states: &[S]) -> Vec<What> {
        let (mut saw_start, mut saw_fail) = (false, false);
        let mut prev = S::Idle;
        let mut out = Vec::new();
        for &s in states {
            if let Some(w) = transition(prev, s, saw_start, saw_fail) {
                match w {
                    What::Started => saw_start = true,
                    What::Failed => saw_fail = true,
                }
                out.push(w);
            }
            prev = s;
        }
        out
    }

    /// The ordinary success: one `started`, whatever the pre-roll did on the way there.
    #[test]
    fn a_normal_start_reports_once() {
        assert_eq!(
            drive(&[S::Resolving, S::Connecting, S::Buffering, S::Playing]),
            [What::Started]
        );
    }

    /// **A seek is not a second start.** `Playing` is re-entered after every seek, every ABR rung
    /// change and every rebuffer; counting those would push the success rate over 100% and make a
    /// heavy scrubber look like several viewers — both silently, since neither is visible from the
    /// dashboard the number appears on.
    #[test]
    fn seeking_and_rebuffering_do_not_start_a_second_playback() {
        let scrubbed = drive(&[
            S::Playing,
            S::Seeking,
            S::Playing,
            S::Seeking,
            S::Playing,
            S::Buffering, // a rebuffer on a bad link
            S::Playing,
        ]);
        assert_eq!(
            scrubbed,
            [What::Started],
            "a seek or a rebuffer reported a second start"
        );
    }

    /// A failure republished on consecutive frames — which is what the read-out being up looks
    /// like — is one failure.
    #[test]
    fn a_failure_held_on_screen_reports_once() {
        assert_eq!(
            drive(&[S::Resolving, S::Error, S::Error, S::Error]),
            [What::Failed]
        );
        // …and it stays one even if a transient state passes through and comes back.
        assert_eq!(drive(&[S::Error, S::Buffering, S::Error]), [What::Failed]);
    }

    /// A playback that started and then failed reports both, in that order: they are different
    /// questions ("did it ever play" and "did it break"), and a stream that dies mid-film answers
    /// yes to each.
    #[test]
    fn a_playback_that_starts_and_then_dies_reports_both() {
        assert_eq!(
            drive(&[S::Playing, S::Error]),
            [What::Started, What::Failed]
        );
    }

    /// The pre-flight refusal — `/decision` said no, so no engine ever existed and the pump never
    /// ran. It is the earliest and most certain failure there is, and it is why this observes the
    /// DERIVED state rather than `pump::set_state`.
    #[test]
    fn a_refusal_before_any_engine_still_reports_a_failure() {
        assert_eq!(drive(&[S::Resolving, S::Error]), [What::Failed]);
    }

    /// A long, eventful playback stays one bounded Sentry event. Preserve the attempt boundary and
    /// the newest transitions; retiring the terminal end to keep an old middle step would erase
    /// the cause this trace exists to carry.
    #[test]
    fn handled_error_trace_is_bounded_and_keeps_both_ends() {
        let mut trace = PlaybackTrace::new();
        trace.reset(7, 0, QualityClass::Auto);
        for at in 1..=ERROR_TRACE_MAX + 8 {
            trace.push(at as i64 * 1_000, TraceEvent::SeekRequested);
        }
        let snapshot = trace.finish(
            (ERROR_TRACE_MAX as i64 + 9) * 1_000,
            TraceEvent::Failed {
                kind: crate::player::FailureKind::OriginalRollback,
            },
        );
        assert_eq!(trace.steps.len(), ERROR_TRACE_MAX);
        assert!(matches!(
            trace.steps.first().map(|s| s.event),
            Some(TraceEvent::Requested { .. })
        ));
        assert!(matches!(
            snapshot.last().map(|s| s.event),
            Some(TraceEvent::Failed {
                kind: crate::player::FailureKind::OriginalRollback,
            })
        ));
        assert!(
            !trace.push_for(7, 99_000, TraceEvent::SeekRequested),
            "nothing may be appended after the terminal event",
        );
        trace.clear();
        assert!(
            trace.steps.is_empty(),
            "withdrawing consent must forget the trace in memory"
        );
    }

    #[test]
    fn a_late_worker_cannot_write_into_the_next_attempt() {
        let mut trace = PlaybackTrace::new();
        trace.reset(11, 0, QualityClass::Auto);
        trace.reset(12, 1_000, QualityClass::Original);
        assert!(
            !trace.push_for(11, 2_000, TraceEvent::SeekRequested),
            "the outgoing worker's generation is stale"
        );
        assert!(trace.push_for(12, 2_000, TraceEvent::SeekRequested));
        assert_eq!(
            trace.steps.len(),
            2,
            "new request plus its own transition only"
        );
    }

    #[test]
    fn privacy_buckets_are_pinned_at_every_boundary() {
        use crate::abr::Rung;
        use crate::route::Quality;

        for (quality, want) in [
            (Quality::Auto, QualityClass::Auto),
            (Quality::Original, QualityClass::Original),
            (Quality::P1080High, QualityClass::M20),
            (Quality::P1080, QualityClass::M8),
            (Quality::P720, QualityClass::M4),
            (Quality::P720Low, QualityClass::M2),
            (Quality::P480, QualityClass::K720),
        ] {
            assert_eq!(
                QualityClass::selected(quality),
                want,
                "selected {quality:?}"
            );
        }
        for (rung, want) in [
            (Rung::P240, QualityClass::K320),
            (Rung::P480, QualityClass::K720),
            (Rung::P720Low, QualityClass::M2),
            (Rung::P720, QualityClass::M4),
            (Rung::P1080M6, QualityClass::M6),
            (Rung::P1080, QualityClass::M8),
            (Rung::P1080M10, QualityClass::M10),
            (Rung::P1080M12, QualityClass::M12),
            (Rung::P1080M14, QualityClass::M14),
            (Rung::P1080M16, QualityClass::M16),
            (Rung::P1080M18, QualityClass::M18),
            (Rung::P1080High, QualityClass::M20),
            (Rung::Uhd, QualityClass::M22),
        ] {
            assert_eq!(QualityClass::from_rung(rung), want, "rung {rung:?}");
            assert_eq!(QualityClass::from_kbps(i64::from(rung.kbps())), want);
        }
        assert_eq!(QualityClass::from_kbps(5_500), QualityClass::Unknown);

        assert_eq!(TraceAge::from_ms(-1), TraceAge::Under1s);
        for (ms, want) in [
            (999, TraceAge::Under1s),
            (1_000, TraceAge::S1To3),
            (2_999, TraceAge::S1To3),
            (3_000, TraceAge::S3To10),
            (9_999, TraceAge::S3To10),
            (10_000, TraceAge::S10To30),
            (29_999, TraceAge::S10To30),
            (30_000, TraceAge::S30To120),
            (119_999, TraceAge::S30To120),
            (120_000, TraceAge::Over2m),
        ] {
            assert_eq!(TraceAge::from_ms(ms), want, "age {ms}");
        }
        for (kbps, want) in [
            (0, RateClass::Unknown),
            (1, RateClass::Under1m),
            (999, RateClass::Under1m),
            (1_000, RateClass::M1To3),
            (2_999, RateClass::M1To3),
            (3_000, RateClass::M3To6),
            (5_999, RateClass::M3To6),
            (6_000, RateClass::M6To12),
            (11_999, RateClass::M6To12),
            (12_000, RateClass::M12To20),
            (19_999, RateClass::M12To20),
            (20_000, RateClass::Over20m),
        ] {
            assert_eq!(RateClass::from_kbps(kbps), want, "rate {kbps}");
        }
        for (height, want) in [
            (0, RasterClass::Unknown),
            (1, RasterClass::Sd),
            (576, RasterClass::Sd),
            (577, RasterClass::Hd),
            (720, RasterClass::Hd),
            (721, RasterClass::Fhd),
            (1_080, RasterClass::Fhd),
            (1_081, RasterClass::Uhd),
        ] {
            assert_eq!(RasterClass::from_height(height), want, "height {height}");
        }
        for (ms, want) in [
            (-1, BufferClass::Unknown),
            (0, BufferClass::Empty),
            (1, BufferClass::Under3s),
            (2_999, BufferClass::Under3s),
            (3_000, BufferClass::S3To10),
            (9_999, BufferClass::S3To10),
            (10_000, BufferClass::S10To30),
            (29_999, BufferClass::S10To30),
            (30_000, BufferClass::Over30s),
        ] {
            assert_eq!(BufferClass::from_ms(ms), want, "buffer {ms}");
        }
        for (stage, want) in [
            (0, PipelineClass::Loading),
            (1, PipelineClass::Playing),
            (2, PipelineClass::Bound),
            (3, PipelineClass::Streaming),
            (4, PipelineClass::Loading),
        ] {
            assert_eq!(PipelineClass::from_stage(stage), want, "stage {stage}");
        }
        for (status, want) in [
            (0, HttpClass::None),
            (199, HttpClass::Other),
            (200, HttpClass::Success),
            (299, HttpClass::Success),
            (300, HttpClass::Other),
            (399, HttpClass::Other),
            (400, HttpClass::ClientError),
            (499, HttpClass::ClientError),
            (500, HttpClass::ServerError),
            (599, HttpClass::ServerError),
            (600, HttpClass::Other),
        ] {
            assert_eq!(HttpClass::from_status(status), want, "HTTP {status}");
        }
    }

    /// **A codec name the table does not know becomes `other`, never itself.** The wire vocabulary
    /// is closed by construction — `diag::schema` has no arm that can carry a runtime string — and
    /// this is the mapping that keeps it that way for a field whose source IS one.
    #[test]
    fn an_unknown_codec_name_cannot_travel_as_itself() {
        assert_eq!(video_codec_class("h264"), "h264");
        assert_eq!(
            video_codec_class("HEVC"),
            "hevc",
            "the server's casing varies"
        );
        assert_eq!(
            audio_codec_class("AC3 PLUS"),
            "eac3",
            "the Load payload's own spelling"
        );
        for odd in ["cinepak", "../../etc/passwd", "Dune.mkv", "h264 (Main)"] {
            assert_eq!(video_codec_class(odd), "other", "{odd} travelled as itself");
        }
        assert_eq!(video_codec_class(""), "unknown");
        assert_eq!(audio_codec_class(""), "unknown");
    }

    /// The rungs, and both spellings of each. `pipe_h264_1080p5994` is the project's own fixture
    /// that reaches `fps_rational`'s 1001-denominator branch and measures `60000/1001`; a bucket
    /// that put it beside 60 in one build and beside `other` in the next would make a year of
    /// comparisons meaningless.
    #[test]
    fn both_spellings_of_a_frame_rate_land_on_one_rung() {
        for (fps, want) in [
            (24.0, "24"),
            (24000.0 / 1001.0, "24"),
            (25.0, "25"),
            (30.0, "30"),
            (30000.0 / 1001.0, "30"),
            (50.0, "50"),
            (60.0, "60"),
            (60000.0 / 1001.0, "60"),
        ] {
            assert_eq!(fps_rung(fps), want, "{fps}");
        }
        // Off the ladder stays off it — a genuinely odd rate is worth being able to see.
        assert_eq!(fps_rung(48.0), "other");
        assert_eq!(fps_rung(23.0), "other");
        assert_eq!(fps_rung(0.0), "unknown");
        assert_eq!(fps_rung(f64::NAN), "unknown");
    }

    /// The raster classes are the ones the rest of the project already reasons in, and the
    /// boundaries are inclusive at the top of each: 1080 is FHD, 1081 is not.
    #[test]
    fn the_raster_classes_are_the_projects_own_rungs() {
        for (h, want) in [
            (480, "sd"),
            (576, "sd"),
            (720, "hd"),
            (1080, "fhd"),
            (1081, "uhd"),
            (2160, "uhd"),
        ] {
            assert_eq!(raster_class(h), want, "{h}");
        }
        assert_eq!(raster_class(0), "unknown");
        assert_eq!(raster_class(-1), "unknown");
    }

    /// **No bucket ever reports an exact value**, which is the whole reason they exist: raster plus
    /// frame rate plus duration plus codec identifies a specific file in a specific library.
    #[test]
    fn a_bucket_never_carries_the_number_it_was_built_from() {
        for h in [479, 481, 719, 1079, 2160, 4320] {
            assert!(
                !raster_class(h).contains(char::is_numeric),
                "{h} leaked its height"
            );
        }
        assert!(!watched_class(3_600_000_000_000, 7_200_000_000_000).contains(char::is_numeric));
    }

    /// The completion classes, including the two ends that are the point of the measure.
    #[test]
    fn the_watched_classes_separate_a_bounce_from_a_finish() {
        let hour = 3_600_000_000_000i64;
        assert_eq!(watched_class(0, hour), "abandoned");
        assert_eq!(watched_class(hour / 100, hour), "abandoned");
        assert_eq!(watched_class(hour / 4, hour), "some");
        assert_eq!(watched_class(hour * 3 / 4, hour), "most");
        assert_eq!(watched_class(hour, hour), "finished");
        // A live stream, or metadata that never arrived — not a completion of anything.
        assert_eq!(watched_class(hour, 0), "unknown");
        assert_eq!(watched_class(-1, hour), "unknown");
    }

    /// The startup boundaries are where the EXPERIENCE changes rather than round numbers, and a
    /// negative interval — a clock that went backwards, or an `ended` with no `requested` — is
    /// `unknown` rather than the fastest bucket, which would silently improve the metric.
    #[test]
    fn a_backwards_clock_is_unknown_and_not_the_fastest_bucket() {
        assert_eq!(startup_class(0), "<1s");
        assert_eq!(startup_class(999), "<1s");
        assert_eq!(startup_class(1_000), "1-3s");
        assert_eq!(startup_class(9_999), "3-10s");
        assert_eq!(startup_class(10_000), "10s+");
        assert_eq!(startup_class(-1), "unknown");
    }

    /// **No `fetch_update` may come back, and only a source grep can say so here.**
    ///
    /// Nightly deprecated that method in favour of `try_update`; `make lint` denies warnings, so
    /// three call sites turned into `error: use of deprecated method` and CI went red on
    /// `67b61515`, `446f64c7` and `69489be7` alike. The repo pins no nightly, so whether a given
    /// checkout SEES the deprecation is a property of when its toolchain was installed — this Mac
    /// was on a June nightly and compiled all three cleanly, which is exactly why the breakage
    /// first appeared on a runner. A reintroduction is therefore invisible to `make check` on the
    /// machine that writes it, and visible only after a push. This grep is that missing signal.
    ///
    /// The needle is assembled at run time so this test does not match its own source.
    #[test]
    fn no_atomic_read_modify_write_uses_the_deprecated_fetch_update() {
        let needle = concat!(".", "fetch_update", "(");
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offences: Vec<String> = Vec::new();
        let mut files = 0usize;
        walk(&src, &mut |path: &std::path::Path, text: &str| {
            files += 1;
            for (n, line) in text.lines().enumerate() {
                if line.contains(needle) {
                    offences.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        });
        assert!(
            files > 50,
            "the walk found only {files} source files — it is not reading the tree"
        );
        assert!(
            offences.is_empty(),
            "`fetch_update` is deprecated on current nightlies and `make lint` denies warnings; \
             write the compare-exchange loop instead (see `requested`):\n{}",
            offences.join("\n")
        );
    }

    fn walk(dir: &std::path::Path, f: &mut impl FnMut(&std::path::Path, &str)) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, f);
            } else if p.extension().is_some_and(|x| x == "rs") {
                if let Ok(t) = std::fs::read_to_string(&p) {
                    f(&p, &t);
                }
            }
        }
    }
}
