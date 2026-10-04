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
// The classification vocabulary is telemetry's wire schema (`telemetry::classes`); this module
// classifies a live playback INTO it. Re-exported here so the player and the route layer keep
// naming the closed domains where they report from.
pub(crate) use crate::telemetry::classes::{
    AudioCodecClass, BufferClass, DecisionCodeClass, DeliveryClass, DeliveryReason, HttpClass,
    LoadElapsedClass, OriginalProbePhase, PipelineClass, PlaybackErrorContext, QualityClass,
    RasterClass, RateClass, RefusalContext, TraceAge, TraceDirection, TraceEvent, TraceOutcome,
    TraceStep, VideoCodecClass,
};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU8, Ordering::Relaxed};

/// The current attempt's opaque id — random, per attempt, never stored. See `DiagEvent`'s playback
/// block: it joins one attempt's lifecycle events and cannot link two playbacks, let alone two sets.
static ATTEMPT: AtomicI64 = AtomicI64::new(0);
/// The attempt's server slot and connection facts, packed into one word so `requested` publishes
/// all three with a SINGLE store and `emit` reads them with a single load — three separate atomics
/// (as this used to be: a `u16` server plus two `u8` link/ip fields) let a concurrent `emit` (the
/// engine's own worker thread; `requested` runs on the main thread) observe a torn combination —
/// e.g. the NEW attempt's server slot paired with the OLD attempt's link/ip, or vice versa,
/// whenever the two threads interleave between the three stores/loads.
///
/// Bit layout (LSB first), all little-endian within the `u32`:
/// - bits 0..16:  server slot (`ServerId::raw()`, a `u16`; `ServerId::UNSET.raw()` when idle)
/// - bits 16..24: link code (`plex::client::encode_link`'s `u8`: 0 = unknown)
/// - bits 24..32: ip code (`plex::client::encode_ip`'s `u8`: 0 = unknown)
///
/// #95 step 8, item 4: the connection half is SNAPSHOTTED once in [`requested`] rather than read
/// live off the server slot's client at every `emit`. A live read would let a mid-attempt re-point
/// (a fresh `Client` published over the same slot) relabel `started`/`ended` events that reported
/// `local` a moment ago as `unknown`, or worse, as whatever the NEW server's connection happens to
/// be — neither is the connection THIS attempt actually used.
static ATTEMPT_CONNECTION: AtomicU32 = AtomicU32::new(pack_connection(plx_plex::plex::ServerId::UNSET.raw(), 0, 0));

const fn pack_connection(server: u16, link: u8, ip: u8) -> u32 {
    (server as u32) | ((link as u32) << 16) | ((ip as u32) << 24)
}

fn unpack_connection(word: u32) -> (u16, u8, u8) {
    (word as u16, (word >> 16) as u8, (word >> 24) as u8)
}

// Link/IP encode-decode is the one pair `plx_plex::plex::client` owns (`encode_link`/`decode_link`,
// `encode_ip`/`decode_ip`) — this module used to keep a second private copy of both tables, which
// is exactly the drift the shared pair exists to rule out.
use plx_plex::plex::{decode_ip, decode_link, encode_ip, encode_link};
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

/// The wire class of the quality the viewer picked. `QualityClass` is telemetry's closed domain and
/// cannot name `route::Quality`, so the mapping lives with the producer.
fn selected_quality_class(q: crate::route::Quality) -> QualityClass {
    use crate::route::Quality as Q;
    match q {
        Q::Auto => QualityClass::Auto,
        Q::Original => QualityClass::Original,
        Q::P1080High => QualityClass::M20,
        Q::P1080 => QualityClass::M8,
        Q::P720 => QualityClass::M4,
        Q::P720Low => QualityClass::M2,
        Q::P480 => QualityClass::K720,
    }
}

/// The wire class of an ABR controller rung, for the same reason as [`selected_quality_class`].
pub(crate) fn rung_quality_class(rung: crate::abr::Rung) -> QualityClass {
    QualityClass::from_kbps(i64::from(rung.kbps()))
}

/// The delivery of the route this playback INSTALLED. A refused or unresolved plan installs none
/// (`tsession` stays empty and `cur_contract` describes an attempt that never ran), so reading the
/// route would label it `Direct` — the tag Sentry PLX-NATIVE-14 showed on every `decision_refused`
/// event. No route is `Unknown`.
fn delivery_class(ps: &crate::route::PlaybackSession) -> DeliveryClass {
    if crate::route::play_refused(ps) || crate::route::play_resolution_failed(ps) {
        DeliveryClass::Unknown
    } else if crate::route::is_segmented_hls(ps) {
        DeliveryClass::Hls
    } else if crate::route::is_transcoding(ps) && crate::route::is_remux(ps) {
        DeliveryClass::Remux
    } else if crate::route::is_transcoding(ps) {
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

/// Forget the in-memory error trace immediately when error reporting is withdrawn. Telemetry
/// reaches this through the hook [`install_trace_eraser`] registers
/// (`telemetry::playback::clear_error_trace`); the app's own call sites name it directly.
pub(crate) fn clear_error_trace() {
    super::SHARED
        .playback_trace
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// **Hand telemetry this trace's eraser.** Withdrawal and sign-out (`telemetry::transition`) and
/// the boot load (`telemetry::activate_initial`) erase the trace through
/// `telemetry::playback::clear_error_trace`, which cannot name the player and does nothing until
/// this runs. `app::enter_application` runs it
/// on every boot, before telemetry loads and before anything can play. Not from [`requested`]: a
/// detail-page preview never passes it, yet a failed preview still seals a `Failed` step here
/// ([`tick`] -> [`finish_trace`], whatever the generation), so an eraser installed by the first
/// full attempt would leave that trace beyond a withdrawal's reach.
pub(crate) fn install_trace_eraser() {
    crate::telemetry::playback::install_error_trace_clear(clear_error_trace);
}

pub(crate) fn note_seek_for(generation: u32) {
    push_trace_for(generation, TraceEvent::SeekRequested);
}

pub(crate) fn note_quality_selected_for(generation: u32, q: crate::route::Quality) {
    push_trace_for(
        generation,
        TraceEvent::QualitySelected {
            selected: selected_quality_class(q),
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
            requested: rung_quality_class(rung),
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

/// Record the native Load-returned gate opening, or issue #74 D.1.4's budget firing first — see
/// [`LoadElapsedClass`]. Callable off the main thread (the load thread reports the ordinary gate
/// transition), like every other `_for` breadcrumb here.
pub(crate) fn note_load_gate_for(generation: u32, elapsed: LoadElapsedClass) {
    push_trace_for(generation, TraceEvent::LoadGateOpened { elapsed });
}

/// The quality the installed route asked the server for. Original-quality routes ask for the
/// original, a progressive transcode asks for the viewer's selection, and HLS is the controller's
/// current rung. No route (`Unknown`) asked for nothing recorded, so it is not inferred.
fn requested_quality(delivery: DeliveryClass, selected: QualityClass) -> QualityClass {
    match delivery {
        DeliveryClass::Unknown => QualityClass::Unknown,
        DeliveryClass::Hls => QualityClass::from_kbps(super::SHARED.dg_abr_kbps.load(Relaxed)),
        DeliveryClass::Direct | DeliveryClass::Remux => QualityClass::Original,
        DeliveryClass::Transcode => selected,
    }
}

fn error_context(ps: &crate::route::PlaybackSession) -> PlaybackErrorContext {
    let delivery = delivery_class(ps);
    let selected = selected_quality_class(crate::route::quality());
    let requested = requested_quality(delivery, selected);
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
        refusal: refusal_context(ps),
    }
}

/// The refusal block for a refusal the SERVER made at `/decision`, else `None`.
///
/// **`attempted` is read from the contract, and that is honest.** `build_stream` stores the route it
/// is about to ask for in `plan.contract` BEFORE it asks `/decision`, and `apply_plan` installs
/// the contract of a refusing plan like any other (only the URL and the encoder session are
/// withheld) — so the contract records which route was ATTEMPTED even though none was installed.
/// The refusal arm is reached only from the remux / HLS / progressive-transcode branch, so those
/// are the only three answers; it is ordered as [`delivery_class`] orders them. `delivery` itself
/// stays `unknown`: reporting the attempt as the delivery would say a route existed.
fn refusal_context(ps: &crate::route::PlaybackSession) -> Option<RefusalContext> {
    let codes = crate::route::server_refusal_codes(ps)?;
    let attempted = if crate::route::is_segmented_hls(ps) {
        DeliveryClass::Hls
    } else if crate::route::is_remux(ps) {
        DeliveryClass::Remux
    } else {
        DeliveryClass::Transcode
    };
    Some(RefusalContext {
        general: DecisionCodeClass::from_code(codes.general),
        transcode: DecisionCodeClass::from_code(codes.transcode),
        attempted,
        source_video: VideoCodecClass::from_name(&crate::route::source_vcodec(ps)),
        source_audio: AudioCodecClass::from_name(&crate::route::source_acodec(ps)),
    })
}

fn presented_event(ps: &crate::route::PlaybackSession) -> TraceEvent {
    let delivery = delivery_class(ps);
    let requested = requested_quality(delivery, selected_quality_class(crate::route::quality()));
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
pub(crate) fn requested(ps: &crate::route::PlaybackSession, server: plx_plex::plex::ServerId) -> u32 {
    resolve_replaced_attempt(ps);
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
    // Snapshot the server slot AND the connection together, in one store — see
    // `ATTEMPT_CONNECTION`'s doc for why `emit` must never re-read the live client, and why this
    // must not be three separate stores.
    let (link, ip) = plx_plex::plex::client_for(server)
        .map(|c| (c.link(), c.ip_version()))
        .unwrap_or((None, None));
    ATTEMPT_CONNECTION.store(pack_connection(server.raw(), encode_link(link), encode_ip(ip)), Relaxed);
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
            selected_quality_class(crate::route::quality()),
        );
    } else {
        trace.arm(generation);
    }
    drop(trace);
    emit(DiagEvent::PlaybackRequested { playback_id: id });
    generation
}

/// Test-only window onto the frozen snapshot `requested` took, so a test can assert it survives a
/// mid-attempt re-point without driving the whole consent/spool pipeline `emit` feeds.
#[cfg(test)]
pub(crate) fn attempt_connection_snapshot_for_test(
) -> (Option<plx_plex::plex::probe::Location>, Option<plx_plex::plex::IpVersion>) {
    let (_, link, ip) = unpack_connection(ATTEMPT_CONNECTION.load(Relaxed));
    (decode_link(link), decode_ip(ip))
}

fn emit(event: DiagEvent) {
    // One load, not three — see `ATTEMPT_CONNECTION`'s doc for why a concurrent `requested` must
    // never be observable as a torn mix of the old server slot and the new connection or back.
    let (server, link, ip) = unpack_connection(ATTEMPT_CONNECTION.load(Relaxed));
    let sid = plx_plex::plex::ServerId::from_raw(server);
    let link = decode_link(link);
    let ip = decode_ip(ip);
    crate::diag::event_for_connection(event, sid, link, ip);
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

fn resolve_replaced_attempt(ps: &crate::route::PlaybackSession) {
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
                mode: mode(ps),
            });
        }
        Replacement::Abandoned => {
            report_quality(id);
            emit(DiagEvent::PlaybackAbandoned {
                playback_id: id,
                mode: mode(ps),
            });
        }
    }
}

/// The process is leaving an unresolved attempt. Unlike a newer Play, this was not a replacement
/// choice, so classify it as abandonment; if playback had started, close its quality summary too.
pub(crate) fn abandon_pending(ps: &crate::route::PlaybackSession) {
    let id = ATTEMPT.load(Relaxed);
    if id == 0 || SAW_FAIL.load(Relaxed) || SAW_END.swap(true, Relaxed) {
        return;
    }
    report_quality(id);
    emit(DiagEvent::PlaybackAbandoned {
        playback_id: id,
        mode: mode(ps),
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
pub(crate) fn tick(ps: &crate::route::PlaybackSession) {
    let now = super::state(ps);
    let prev = super::shared::PlaybackState::from_u8(LAST.swap(now as u8, Relaxed));
    // A claim's presentation hold answers Buffering on purpose (`state()`), so its
    // Playing -> Buffering edge is the viewer's own pick working, not a stall.
    if !super::claim_hold::active() {
        note_rebuffer(prev, now, SAW_START.load(Relaxed));
    }
    if prev != now && now == super::shared::PlaybackState::Playing {
        // Unlike the usage funnel's once-per-attempt `Started`, every return to actual presented
        // video is useful causal evidence after a seek or delivery reload.
        push_trace(presented_event(ps));
    }
    match transition(prev, now, SAW_START.load(Relaxed), SAW_FAIL.load(Relaxed)) {
        Some(What::Started) => {
            SAW_START.store(true, Relaxed);
            emit(DiagEvent::PlaybackStarted {
                playback_id: ATTEMPT.load(Relaxed),
                mode: mode(ps),
                raster: raster_class(super::SHARED.video_raster().1),
                fps: fps_rung(crate::route::stream_fps(ps)),
                video: video_codec_class(&crate::route::stream_vcodec(ps)),
                audio: audio_codec_class(&crate::route::stream_acodec(ps)),
                startup: startup_class(now_ms() - REQUESTED_MS.load(Relaxed)),
            });
        }
        Some(What::Failed) => {
            SAW_FAIL.store(true, Relaxed);
            report_quality(ATTEMPT.load(Relaxed));
            let shape = super::error_now(ps);
            let kind = shape.kind.class();
            let trace = finish_trace(TraceEvent::Failed { kind });
            crate::telemetry::playback::report_error(kind, error_context(ps), &trace);
            emit(DiagEvent::PlaybackFailed {
                playback_id: ATTEMPT.load(Relaxed),
                mode: mode(ps),
                kind: shape.kind.code(),
            });
        }
        None => {}
    }
}

/// **A real teardown.** Called from the one place playback actually ends — never from a seek, a
/// rung change or a suspend, each of which destroys an engine and keeps the playback.
pub(crate) fn ended(ps: &crate::route::PlaybackSession, position_ns: i64, duration_ns: i64) {
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
            mode: mode(ps),
        });
        clear_error_trace();
        return;
    }
    report_quality(id);
    emit(DiagEvent::PlaybackEnded {
        playback_id: id,
        mode: mode(ps),
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

/// `direct` / `transcode` of the route this playback INSTALLED, `unknown` when none was.
///
/// A refused or unresolved plan installs no route (`tsession` stays empty), and "no encoder
/// session" is how a direct play is recognised — so this read `direct` for every refusal, the same
/// false claim [`delivery_class`] made until Sentry PLX-NATIVE-14. The usage funnel's `mode`
/// domain gained `unknown` for it; `diag::schema::MODE` and `PRIVACY.md` carry the value.
fn mode(ps: &crate::route::PlaybackSession) -> &'static str {
    if crate::route::play_refused(ps) || crate::route::play_resolution_failed(ps) {
        "unknown"
    } else if crate::route::is_transcoding(ps) {
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

pub(crate) fn video_codec_class(name: &str) -> &'static str {
    VideoCodecClass::from_name(name).code()
}

pub(crate) fn audio_codec_class(name: &str) -> &'static str {
    AudioCodecClass::from_name(name).code()
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

    /// PR #104 review: `ATTEMPT_CONNECTION` packs the server slot, link code and ip code into one
    /// `u32` so `requested`/`emit` publish/observe them with a single store/load. Every field must
    /// round-trip through the pack/unpack pair independently of the others, at both ends of each
    /// field's range (a bit landing in the wrong lane would show up here as one field corrupting
    /// its neighbour).
    #[test]
    fn attempt_connection_packing_round_trips_every_field_independently() {
        for server in [0u16, 1, u16::MAX - 1, u16::MAX] {
            for link in [0u8, 1, 2, 3] {
                for ip in [0u8, 1, 2] {
                    let packed = pack_connection(server, link, ip);
                    assert_eq!(
                        unpack_connection(packed),
                        (server, link, ip),
                        "server={server} link={link} ip={ip} did not round-trip"
                    );
                }
            }
        }
        // The three lanes must not bleed into each other: changing one field's bits must never
        // change what another field decodes to.
        let base = pack_connection(0x1234, 1, 2);
        let bumped_server = pack_connection(0x5678, 1, 2);
        let (_, link, ip) = unpack_connection(bumped_server);
        assert_eq!((link, ip), (1, 2), "changing the server lane must not disturb link/ip");
        let (server, _, _) = unpack_connection(base);
        assert_eq!(server, 0x1234);
    }

    /// #95 step 8, item 4: `requested` snapshots the attempt's `(link, ip)` once; a re-point that
    /// lands mid-attempt (a fresh `Client` published over the same slot, e.g. the roster refresh
    /// path re-racing a working relay to LAN) must not relabel events already in flight for THIS
    /// attempt. A live `client_for(server)` read at emit time would have.
    #[test]
    fn requested_snapshots_the_connection_and_a_mid_attempt_repoint_does_not_change_it() {
        let _g = plx_base::testlock::serial();
        plx_plex::plex::reset_servers_for_test();
        let o = plx_plex::plex::Origin::http("10.0.0.9", 32400);
        let connection = plx_plex::plex::ConnectionFacts::new(
            Some(plx_plex::plex::probe::Location::Local),
            Some(plx_plex::plex::IpVersion::V4),
        );
        let sid = plx_plex::plex::register_pinned_with_client_id(
            "m1", &o, "tok", None, "cid", connection,
        );
        let ps = crate::route::PlaybackSession::default();
        requested(&ps, sid);
        assert_eq!(
            attempt_connection_snapshot_for_test(),
            (Some(plx_plex::plex::probe::Location::Local), Some(plx_plex::plex::IpVersion::V4)),
            "snapshotted at requested time"
        );
        // Mid-attempt re-point: a DIFFERENT origin for the same machine id, e.g. a roster refresh
        // finally reaching the LAN candidate — publishes a fresh `Client` with its own tier/ip.
        let o2 = plx_plex::plex::Origin::http("10.0.0.20", 32400);
        let repoint = plx_plex::plex::ConnectionFacts::new(
            Some(plx_plex::plex::probe::Location::Relay),
            Some(plx_plex::plex::IpVersion::V6),
        );
        let repointed = plx_plex::plex::register_pinned_with_client_id(
            "m1", &o2, "tok", None, "cid", repoint,
        );
        assert_eq!(repointed, sid, "re-pointed in place — same slot id");
        assert_eq!(
            plx_plex::plex::client_for(sid).unwrap().link(),
            Some(plx_plex::plex::probe::Location::Relay),
            "the live client really did change"
        );
        assert_eq!(
            attempt_connection_snapshot_for_test(),
            (Some(plx_plex::plex::probe::Location::Local), Some(plx_plex::plex::IpVersion::V4)),
            "the attempt's snapshot is untouched by the re-point"
        );
        plx_plex::plex::reset_servers_for_test();
    }

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

    /// Finding: `starts_rebuffer` read every `Playing -> Buffering` edge as a stall, and a claim's
    /// presentation hold (`claim_hold`) makes `state()` answer Buffering on purpose — so each held
    /// track pick, enhancement toggle or quality change counted as a rebuffer in the quality
    /// telemetry a viewer never experienced as one.
    #[test]
    fn a_claim_hold_is_not_a_rebuffer() {
        let _g = plx_base::testlock::serial();
        let ps = crate::route::PlaybackSession::IDLE;
        REBUFFER_COUNT.store(0, Relaxed);
        SAW_START.store(true, Relaxed);
        LAST.store(S::Playing as u8, Relaxed);
        // `state()` ranks a seek in flight above everything here; another test's leftover flag
        // would answer Seeking for both edges below.
        crate::player::SHARED.seeking.store(false, Relaxed);
        crate::player::claim_hold::hold_for_test(7, true);
        crate::route::force_applying_for_test(7);
        assert_eq!(crate::player::state(&ps), S::Buffering, "the hold draws the spinner");

        tick(&ps);
        let held = REBUFFER_COUNT.load(Relaxed);

        // ...whereas the same edge with no hold IS one.
        crate::player::claim_hold::clear();
        crate::route::reset_player_control_for_test(&ps);
        LAST.store(S::Playing as u8, Relaxed);
        crate::player::SHARED.pb_state.store(S::Buffering as u8, Relaxed);
        tick(&ps);
        let genuine = REBUFFER_COUNT.load(Relaxed);

        crate::player::SHARED.pb_state.store(S::Idle as u8, Relaxed);
        LAST.store(S::Idle as u8, Relaxed);
        SAW_START.store(false, Relaxed);
        REBUFFER_COUNT.store(0, Relaxed);
        REBUFFER_AT_MS.store(0, Relaxed);
        assert_eq!(held, 0, "a claim hold was counted as a rebuffer");
        assert_eq!(genuine, 1, "a genuine stall still counts");
    }

    /// PLX-NATIVE-14: a plan the server (or Force Direct Play) REFUSED never installs a route — no
    /// URL, no encoder session — so reading the delivery off the installed route made every
    /// `decision_refused` report say `original_direct` / requested `original`, whatever the attempt
    /// actually asked for. A refusal has no delivery and no requested quality; the report must say so.
    #[test]
    fn a_refused_plan_reports_no_delivery_and_no_requested_quality() {
        let _g = plx_base::testlock::serial();
        for verdict in [
            crate::route::PlayVerdict::Server("Cannot convert this item.".into(), Default::default()),
            crate::route::PlayVerdict::Server(String::new(), Default::default()),
            crate::route::PlayVerdict::DirectPlayDisabled,
            crate::route::PlayVerdict::Forced(crate::route::ForcedFailure::Video),
        ] {
            let mut ps = crate::route::PlaybackSession::default();
            crate::route::refuse_for_test(&mut ps, verdict.clone());
            let ctx = error_context(&ps);
            assert_eq!(ctx.delivery.code(), "unknown", "{verdict:?}: delivery was inferred");
            assert_eq!(ctx.requested, QualityClass::Unknown, "{verdict:?}: requested was inferred");
            // The selected quality is the viewer's own setting, which is recorded and still true.
            assert_eq!(ctx.selected, selected_quality_class(crate::route::quality()));
        }
    }

    /// Sentry PLX-NATIVE-14, the half `delivery: unknown` could not give: WHICH refusal. A server
    /// refusal reports the verdict numbers it was made on, the route the plan ATTEMPTED (recorded
    /// in the contract even though no route was installed), and the source file's codecs — while
    /// `delivery` itself stays `unknown`, because nothing was installed.
    #[test]
    fn a_server_refusal_reports_its_codes_the_attempted_route_and_the_source_codecs() {
        let _g = plx_base::testlock::serial();
        use crate::route::DecisionCodes as C;
        // (remux, hls) -> the attempt. Remux is checked after HLS, as `delivery_class` does.
        for (remux, hls, attempted) in [
            (true, false, "original_remux"),
            (false, true, "hls"),
            (false, false, "progressive_transcode"),
        ] {
            let mut ps = crate::route::PlaybackSession::default();
            crate::route::refuse_by_server_for_test(
                &mut ps,
                "Cannot convert this item.",
                C { general: Some(2000), transcode: Some(4007) },
                remux,
                hls,
                "vp9",
                "eac3",
            );
            let ctx = error_context(&ps);
            assert_eq!(ctx.delivery.code(), "unknown", "no route was installed");
            let r = ctx.refusal.expect("a server refusal carries its refusal block");
            assert_eq!(r.general.code(), "2000");
            assert_eq!(r.transcode.code(), "4007");
            assert_eq!(r.attempted.code(), attempted, "remux={remux} hls={hls}");
            assert_eq!(r.source_video.code(), "vp9");
            assert_eq!(r.source_audio.code(), "eac3");
        }
        // A body that carried only one of the two numbers keeps the other `absent`, not 0.
        let mut ps = crate::route::PlaybackSession::default();
        crate::route::refuse_by_server_for_test(
            &mut ps,
            "",
            C { general: Some(2000), transcode: None },
            false,
            false,
            "",
            "",
        );
        let r = error_context(&ps).refusal.expect("refusal block");
        assert_eq!((r.transcode.code(), r.source_video.code(), r.source_audio.code()), ("absent", "unknown", "unknown"));
    }

    /// The server's sentence is free text — it can carry a file name, a path, a library or server
    /// name — and must never leave the television. Build the real payload for a refused plan whose
    /// sentence, source codec names and server codes are all hostile, and demand the serialized
    /// event holds none of them while still holding the closed fields.
    #[test]
    fn no_free_text_from_the_server_decision_reaches_the_serialized_event() {
        let _g = plx_base::testlock::serial();
        use crate::route::DecisionCodes as C;
        let sentence = "Cannot convert /media/Private Films/Secret Title (2020)/secret.mkv on SERVER-NAME-9 \
                        token=abc123 http://192.168.1.50:32400/library";
        let mut ps = crate::route::PlaybackSession::default();
        crate::route::refuse_by_server_for_test(
            &mut ps,
            sentence,
            C { general: Some(2000), transcode: Some(4999) },
            false,
            false,
            "x-secret-video-tag",
            "x-secret-audio-tag",
        );
        let ctx = error_context(&ps);
        let body = crate::telemetry::playback::event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            crate::telemetry::classes::FailureClass::DecisionRefused,
            ctx,
            &[],
        );
        let text = String::from_utf8(body).expect("utf-8 event");
        for leak in [
            "Secret", "secret", "/media", "Private", "SERVER-NAME", "token", "abc123", "192.168",
            "32400", "Cannot convert", "x-secret",
        ] {
            assert!(!text.contains(leak), "the payload carries {leak:?}: {text}");
        }
        let v: serde_json::Value = serde_json::from_str(&text).expect("event JSON");
        let refusal = &v["contexts"]["refusal"];
        assert_eq!(refusal["general_code"], "2000");
        assert_eq!(refusal["transcode_code"], "other_4xxx", "an unlisted number is bucketed, not echoed");
        assert_eq!(refusal["source_video"], "other");
        assert_eq!(refusal["source_audio"], "other");
        assert_eq!(v["tags"]["playback.attempted_delivery"], "progressive_transcode");
    }

    /// The block is the SERVER's refusal and nothing else: the app's own policy refusals have no
    /// server codes and no attempted transcode, and a playback with a route is not a refusal.
    #[test]
    fn only_a_server_refusal_carries_the_refusal_block() {
        let _g = plx_base::testlock::serial();
        for verdict in [
            crate::route::PlayVerdict::DirectPlayDisabled,
            crate::route::PlayVerdict::Forced(crate::route::ForcedFailure::Video),
        ] {
            let mut ps = crate::route::PlaybackSession::default();
            crate::route::refuse_for_test(&mut ps, verdict.clone());
            assert!(error_context(&ps).refusal.is_none(), "{verdict:?}");
        }
        let mut hls = crate::route::PlaybackSession::default();
        crate::route::install_transcode_for_test(&mut hls, false, true);
        assert!(error_context(&hls).refusal.is_none());
    }

    /// The numbers are bucketed by their DOCUMENTED class (OpenAPI `generalDecisionCode`), and an
    /// absent number is never read as 0.
    #[test]
    fn decision_codes_fall_into_a_closed_domain() {
        for (code, want) in [
            (None, "absent"),
            (Some(2000), "2000"),
            (Some(2003), "2003"),
            (Some(4007), "4007"),
            (Some(1001), "other_1xxx"),
            (Some(2001), "other_2xxx"),
            (Some(3000), "other_3xxx"),
            (Some(4020), "other_4xxx"),
            (Some(0), "other"),
            (Some(5000), "other"),
            (Some(-1), "other"),
        ] {
            assert_eq!(DecisionCodeClass::from_code(code).code(), want, "{code:?}");
        }
        // The listed domain is the whole enum, with no code repeated.
        let mut codes: Vec<_> = DecisionCodeClass::ALL.iter().map(|c| c.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), DecisionCodeClass::ALL.len());
    }

    /// Source codecs reach the report through the closed tables only: a name the table does not
    /// know is `other` and is never echoed, which is what keeps a file's own tag out of Sentry.
    #[test]
    fn source_codecs_are_closed_classes_and_unknown_names_are_never_echoed() {
        assert_eq!(VideoCodecClass::from_name("HEVC").code(), "hevc");
        assert_eq!(VideoCodecClass::from_name("vc1").code(), "other");
        assert_eq!(VideoCodecClass::from_name("").code(), "unknown");
        assert_eq!(AudioCodecClass::from_name("dca").code(), "dts");
        assert_eq!(AudioCodecClass::from_name("x-private-tag").code(), "other");
        // The usage funnel's string helpers are the same table.
        assert_eq!(video_codec_class("h265"), VideoCodecClass::Hevc.code());
        assert_eq!(audio_codec_class("ec-3"), AudioCodecClass::Eac3.code());
    }

    /// `playback.failed`'s `mode` read `direct` for a refused plan, for the reason `delivery` did:
    /// it was derived from the absence of an encoder session, and a refusal has none.
    #[test]
    fn a_refused_plan_reports_mode_unknown_and_a_real_route_keeps_its_mode() {
        let _g = plx_base::testlock::serial();
        let mut refused = crate::route::PlaybackSession::default();
        crate::route::refuse_by_server_for_test(&mut refused, "", Default::default(), true, false, "", "");
        assert_eq!(mode(&refused), "unknown");
        let mut policy = crate::route::PlaybackSession::default();
        crate::route::refuse_for_test(&mut policy, crate::route::PlayVerdict::DirectPlayDisabled);
        assert_eq!(mode(&policy), "unknown");
        assert_eq!(mode(&crate::route::PlaybackSession::default()), "direct");
        let mut transcode = crate::route::PlaybackSession::default();
        crate::route::install_transcode_for_test(&mut transcode, false, false);
        assert_eq!(mode(&transcode), "transcode");
    }

    /// The other half: a failure on a playback that DID install a route keeps the real tags.
    #[test]
    fn an_installed_route_keeps_its_real_delivery_and_requested_quality() {
        let _g = plx_base::testlock::serial();
        let direct = crate::route::PlaybackSession::default();
        let ctx = error_context(&direct);
        assert_eq!((ctx.delivery.code(), ctx.requested), ("original_direct", QualityClass::Original));

        let mut remux = crate::route::PlaybackSession::default();
        crate::route::install_transcode_for_test(&mut remux, true, false);
        let ctx = error_context(&remux);
        assert_eq!((ctx.delivery.code(), ctx.requested), ("original_remux", QualityClass::Original));

        let mut transcode = crate::route::PlaybackSession::default();
        crate::route::install_transcode_for_test(&mut transcode, false, false);
        let ctx = error_context(&transcode);
        assert_eq!(ctx.delivery.code(), "progressive_transcode");
        assert_eq!(ctx.requested, ctx.selected);

        let mut hls = crate::route::PlaybackSession::default();
        crate::route::install_transcode_for_test(&mut hls, false, true);
        assert_eq!(error_context(&hls).delivery.code(), "hls");
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
                kind: crate::telemetry::classes::FailureClass::OriginalRollback,
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
                kind: crate::telemetry::classes::FailureClass::OriginalRollback,
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

    /// **A preview's failure is erased by withdrawal too.** A detail-page preview never passes
    /// [`requested`] (`trace_generation` stays 0), yet `tick` still seals its `Failed` step into
    /// the shared trace whatever the generation. Telemetry erases that trace on withdrawal and
    /// sign-out through `telemetry::playback::clear_error_trace`, a no-op while unset, so the
    /// eraser cannot be installed by the first full attempt: a viewer whose only playback was a
    /// failed preview would keep that trace past a withdrawal, and a later failure would send it.
    /// [`install_trace_eraser`] is what `app::enter_application` runs at boot.
    #[test]
    fn a_failed_preview_trace_is_erased_through_telemetrys_hook() {
        use crate::telemetry::consent::{self, Consent};
        let _g = plx_base::testlock::serial();
        let previous = consent::current();
        let mut enabled = Consent::default();
        enabled.errors = true;
        enabled.asked_version = consent::POLICY_VERSION;
        consent::install(enabled);
        clear_error_trace();

        let sealed = finish_trace(TraceEvent::Failed {
            kind: crate::telemetry::classes::FailureClass::OriginalRollback,
        });
        install_trace_eraser();
        crate::telemetry::playback::clear_error_trace();
        let left = super::super::SHARED
            .playback_trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .step_count_for_test();
        clear_error_trace();
        consent::install(previous.unwrap_or_default());

        assert_eq!(sealed.len(), 1, "a preview's failure is traced with no attempt armed");
        assert_eq!(left, 0, "withdrawal left a preview's failure trace in memory");
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
                selected_quality_class(quality),
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
            assert_eq!(rung_quality_class(rung), want, "rung {rung:?}");
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
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offences: Vec<String> = Vec::new();
        let mut files = 0usize;
        // The application crate and the `ui` layer (`../ui/src`), which was `src/ui` before it
        // became its own crate: moving a directory must not move it out of this scan.
        for src in [manifest.join("src"), manifest.join("ui/src")] {
            walk(&src, &mut |path: &std::path::Path, text: &str| {
                files += 1;
                for (n, line) in text.lines().enumerate() {
                    if line.contains(needle) {
                        offences.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            });
        }
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
