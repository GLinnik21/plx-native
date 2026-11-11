//! The physical demux worker's session over the Feed's own state: the HLS/Original pause-lane
//! cursors, the native clock sampled at its original physical boundary, the automatic-transition
//! actuator, the callback observer and the effect sink. Construction captures no globals; every
//! sample and every publication is addressed to the `Load` epoch it was captured for, so a retired
//! worker can neither read nor write its successor's state ([`FEED_SHARED::with_active_demux_epoch`]).
//!
//! The captions are the common player's (`DemuxSubtitle`: the cue and ASS stores live in `plx_media`),
//! so [`FeedDemuxSession`] is generic over that one half and the composition supplies it. Two
//! values also belong to the composition, not to this crate, and arrive as fn pointers on
//! [`DemuxMediaHooks`] (a media-built const, not a global): the telemetry exit class a demux
//! fault is latched as (play-feed has no telemetry dependency) and the one common write an effect
//! makes, the container track names in `SHARED.track_names`.

use crate::shared::{
    HlsAutomaticTransition, HlsClockFenceError, UserPauseCursor, FEED_SHARED, FEED_TX,
};
use plx_playback_contract::adaptive::{
    AutomaticTransition, BufferFact, FenceHandle, FenceSettlement, LeaseHandle, CandidateFenceError,
};
use plx_playback_contract::demux::{
    AddressedSampler, DemuxAdaptive, DemuxCallbackEffect, DemuxClock, DemuxEffect, DemuxFault,
    DemuxMedia, DemuxObserver, DemuxSession, DemuxSubtitle, HlsBufferClocks, PauseClock,
    PauseEvent, PauseLane,
};
use plx_playback_contract::readout::{ABR_MODE_HLS, ABR_MODE_ORIGINAL};
use plx_playback_contract::subtitle::CaptionRetention;
use plx_playback_contract::tracks::TrackNames;
use plx_playback_contract::AttemptId;
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// The composition's half of the effect sink.
#[derive(Clone, Copy)]
pub struct DemuxMediaHooks {
    /// The byte the demuxer's exit latch stores for a fault (`DemuxExitClass::latch`).
    pub exit_latch: fn(DemuxFault) -> u8,
    /// Publish container-reported track names into the common store and hand back what it held.
    /// Runs under the native retirement barrier (so a retired open cannot publish); the caller drops
    /// the returned names after the barrier and the store's own lock are released.
    pub replace_track_names: fn(TrackNames) -> TrackNames,
}

/// The session a feed attempt's demux worker is handed: the clock and the adaptive actuator
/// ([`FeedDemuxClock`]), the effect sink ([`FeedDemuxMedia`]) and the observer are this crate's; the
/// caption half `S` is the composition's.
pub struct FeedDemuxSession<S> {
    observer: Arc<FeedDemuxObserver>,
    clock: FeedDemuxClock,
    media: FeedDemuxMedia,
    subtitle: S,
}

impl<S: DemuxSubtitle + Send + 'static> FeedDemuxSession<S> {
    /// Safe to capture on the main thread: pause cursors and the native clock are sampled only
    /// after the worker explicitly begins a lane. `subtitle` builds the caption half from the
    /// observer it will address its stores with.
    pub fn capture(
        attempt: AttemptId,
        native_epoch: u32,
        hooks: DemuxMediaHooks,
        subtitle: impl FnOnce(Arc<dyn DemuxObserver>) -> S,
    ) -> Box<dyn DemuxSession> {
        let observer = Arc::new(FeedDemuxObserver { attempt, native_epoch });
        Box::new(Self {
            clock: FeedDemuxClock { observer: observer.clone(), hls_pause: None, original_pause: None },
            media: FeedDemuxMedia { native_epoch, hooks },
            subtitle: subtitle(observer.clone() as Arc<dyn DemuxObserver>),
            observer,
        })
    }
}

impl<S: DemuxSubtitle + Send + 'static> DemuxSession for FeedDemuxSession<S> {
    fn observer(&self) -> Arc<dyn DemuxObserver> { self.observer.clone() }
    fn clock(&self) -> &dyn DemuxClock { &self.clock }
    fn clock_mut(&mut self) -> &mut dyn DemuxClock { &mut self.clock }
    fn adaptive_ref(&self) -> &dyn DemuxAdaptive { &self.clock }
    fn adaptive(&mut self) -> &mut dyn DemuxAdaptive { &mut self.clock }
    fn media(&mut self) -> &mut dyn DemuxMedia { &mut self.media }
    fn subtitle(&mut self) -> &mut dyn DemuxSubtitle { &mut self.subtitle }
}

pub struct FeedDemuxObserver {
    attempt: AttemptId,
    native_epoch: u32,
}

impl FeedDemuxObserver {
    /// The `Load` epoch every sample and publication of this observer is addressed to.
    pub fn native_epoch(&self) -> u32 { self.native_epoch }
}

impl DemuxObserver for FeedDemuxObserver {
    fn attempt(&self) -> AttemptId { self.attempt }
    fn with_active(&self, f: &mut dyn FnMut()) -> bool {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, f).is_some()
    }
    fn playpos_ns(&self) -> Option<i64> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || FEED_SHARED.playpos_ns.load(Ordering::Relaxed))
    }
    fn caption_retention(&self) -> Option<CaptionRetention> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || {
            let seeking = FEED_SHARED.seeking.load(Ordering::Acquire);
            let target = FEED_SHARED.seek_display_ns.load(Ordering::Relaxed);
            CaptionRetention {
                playhead_ns: Some(FEED_SHARED.playpos_ns.load(Ordering::Relaxed)),
                seek_target_ns: (seeking && target >= 0).then_some(target),
            }
        })
    }
    fn hold_epoch(&self) -> Option<u64> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || FEED_SHARED.hls_internal_hold_epoch.load(Ordering::Acquire))
    }
    fn rebuffering(&self) -> Option<bool> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || FEED_SHARED.hls_rebuffering.load(Ordering::Acquire))
    }
    fn audio_expected(&self) -> Option<bool> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.hls_audio_expected.load(Ordering::Acquire))
    }
    fn duration_ns(&self) -> Option<i64> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.duration_ns.load(Ordering::Relaxed))
    }
    fn declared_video_code(&self) -> Option<u8> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.dg_load_v.load(Ordering::Relaxed))
    }
    fn declared_audio_code(&self) -> Option<u8> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.dg_load_a.load(Ordering::Relaxed))
    }
    fn paused(&self) -> Option<bool> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || FEED_TX.paused.load(Ordering::Acquire))
    }
    fn pause_clock(&self) -> Option<PauseClock> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || {
            let (at, elapsed) = FEED_TX.pause_clock_sample();
            PauseClock { at, elapsed }
        })
    }
    fn hls_buffer_ms(&self) -> Option<Option<i64>> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, hls_buffered_ms)
    }
    fn hls_buffer_clocks(&self) -> Option<HlsBufferClocks> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || HlsBufferClocks {
            playhead_ns: FEED_SHARED.playpos_ns.load(Ordering::Relaxed),
            video_tail_ns: FEED_SHARED.hls_video_tail_ns.load(Ordering::Acquire),
            audio_tail_ns: FEED_SHARED.hls_audio_tail_ns.load(Ordering::Acquire),
            display_base_ns: FEED_SHARED.disp_base.load(Ordering::Relaxed),
        })
    }
    fn remaining_playback_ms(&self) -> Option<i64> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, remaining_playback_ms)
    }
    fn user_sequence(&self) -> Option<u64> {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || FEED_SHARED.hls_user_sequence())
    }
    fn publish(&self, effect: DemuxCallbackEffect) -> bool {
        FEED_SHARED.with_active_demux_epoch(self.native_epoch, || match effect {
            DemuxCallbackEffect::NetworkBytes(bytes) => {
                FEED_SHARED.dg_net_rx.fetch_add(bytes, Ordering::Relaxed);
            }
            DemuxCallbackEffect::HttpStatus(status) =>
                FEED_SHARED.dg_http_status.store(status, Ordering::Relaxed),
            DemuxCallbackEffect::FileSize(size) =>
                FEED_SHARED.file_size.store(size, Ordering::Release),
            DemuxCallbackEffect::DurationNs(ns) =>
                FEED_SHARED.duration_ns.store(ns, Ordering::Relaxed),
            DemuxCallbackEffect::TransportFailure { rc, age_ms, reopen_failed } => {
                FEED_SHARED.dg_transport_rc.store(rc, Ordering::Release);
                FEED_SHARED.dg_transport_age_ms.store(age_ms, Ordering::Release);
                FEED_SHARED.dg_transport_reopen_failed.store(reopen_failed, Ordering::Release);
                FEED_SHARED.demux_io_failed.store(true, Ordering::Release);
            }
            DemuxCallbackEffect::IoFailed =>
                FEED_SHARED.demux_io_failed.store(true, Ordering::Release),
            DemuxCallbackEffect::RequestTerminalHold => {
                FEED_SHARED.hls_rebuffer_request_tail_ns.store(
                    FEED_SHARED.hls_video_tail_ns.load(Ordering::Acquire), Ordering::Release,
                );
                FEED_SHARED.hls_rebuffer_requested.store(true, Ordering::Release);
            }
        }).is_some()
    }
}

/// The clock and the adaptive actuator of one demux attempt.
pub struct FeedDemuxClock {
    observer: Arc<FeedDemuxObserver>,
    hls_pause: Option<UserPauseCursor>,
    original_pause: Option<UserPauseCursor>,
}

impl FeedDemuxClock {
    fn cursor(&mut self, lane: PauseLane) -> &mut UserPauseCursor {
        match lane {
            PauseLane::Hls => self.hls_pause.as_mut()
                .expect("HLS pause lane begins after its cursor opens"),
            PauseLane::Original => self.original_pause.as_mut()
                .expect("Original pause lane begins before its source opens"),
        }
    }
}

impl DemuxClock for FeedDemuxClock {
    fn playpos_ns(&self) -> i64 { FEED_SHARED.playpos_ns.load(Ordering::Relaxed) }
    fn display_base_ns(&self) -> i64 { FEED_SHARED.disp_base.load(Ordering::Relaxed) }
    fn duration_ns(&self) -> i64 { FEED_SHARED.duration_ns.load(Ordering::Relaxed) }
    fn video_tail_ns(&self) -> i64 { FEED_SHARED.hls_video_tail_ns.load(Ordering::Acquire) }
    fn audio_tail_ns(&self) -> i64 { FEED_SHARED.hls_audio_tail_ns.load(Ordering::Acquire) }
    fn hold_epoch(&self) -> u64 { FEED_SHARED.hls_internal_hold_epoch.load(Ordering::Acquire) }
    fn rebuffering(&self) -> bool { FEED_SHARED.hls_rebuffering.load(Ordering::Acquire) }
    fn rebuffer_requested(&self) -> bool { FEED_SHARED.hls_rebuffer_requested.load(Ordering::Acquire) }
    fn paused(&self) -> bool { FEED_TX.paused.load(Ordering::Acquire) }
    fn pause_clock(&self) -> PauseClock {
        let (at, elapsed) = FEED_TX.pause_clock_sample();
        PauseClock { at, elapsed }
    }
    fn begin_pause_lane(&mut self, lane: PauseLane) {
        let cursor = UserPauseCursor::new(FEED_TX.pause_state_sample());
        match lane {
            PauseLane::Hls => self.hls_pause = Some(cursor),
            PauseLane::Original => self.original_pause = Some(cursor),
        }
    }
    fn pause_event(&mut self, lane: PauseLane) -> PauseEvent {
        let snapshot = FEED_TX.pause_state_sample();
        let completed_ms = self.cursor(lane).consume_completed(snapshot)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
        PauseEvent { paused: snapshot.paused, sequence: snapshot.sequence, completed_ms }
    }
    fn seeking(&self) -> bool { FEED_SHARED.seeking.load(Ordering::Relaxed) }
    fn take_seek_ns(&self) -> Option<i64> {
        FEED_SHARED.with_active_demux_epoch(self.observer.native_epoch, || {
            let ns = FEED_SHARED.seek_to_ns.swap(-1, Ordering::Acquire);
            (ns >= 0).then_some(ns)
        }).flatten()
    }
    fn user_sequence(&self) -> u64 { FEED_SHARED.hls_user_sequence() }
    fn desired_audio_idx(&self) -> i32 { FEED_SHARED.desired_audio_idx.load(Ordering::Relaxed) }
    fn applied_rate_permille(&self) -> i32 {
        FEED_SHARED.applied_rate_permille.load(Ordering::Relaxed)
    }
    fn side_subtitle_owner(&self) -> bool {
        FEED_SHARED.side_subs_owner.load(Ordering::Acquire)
    }
    fn playpos_sampler(&self) -> AddressedSampler<i64> {
        let observer = self.observer.clone();
        Arc::new(move || observer.playpos_ns().map(|ns| ns.max(0)))
    }
    fn hls_buffer_sampler(&self) -> AddressedSampler<Option<i64>> {
        let observer = self.observer.clone();
        Arc::new(move || observer.hls_buffer_ms())
    }
    fn remaining_playback_sampler(&self) -> AddressedSampler<i64> {
        let observer = self.observer.clone();
        Arc::new(move || observer.remaining_playback_ms())
    }
    fn user_sequence_sampler(&self) -> AddressedSampler<u64> {
        let observer = self.observer.clone();
        Arc::new(move || observer.user_sequence())
    }
}

fn hls_buffered_ms() -> Option<i64> {
    let playhead_ns = FEED_SHARED.playpos_ns.load(Ordering::Relaxed).max(0);
    let video = FEED_SHARED.hls_video_tail_ns.load(Ordering::Acquire);
    let audio = FEED_SHARED.hls_audio_tail_ns.load(Ordering::Acquire);
    let display_base = FEED_SHARED.disp_base.load(Ordering::Relaxed).max(0);
    BufferFact {
        playback_ms: playhead_ns / 1_000_000,
        video_tail_ms: video.max(0).saturating_add(display_base) / 1_000_000,
        audio_tail_ms: (audio >= 0).then_some(
            audio.max(0).saturating_add(display_base) / 1_000_000,
        ),
        audio_expected: audio >= 0,
    }.buffered_ms()
}

fn remaining_playback_ms() -> i64 {
    let duration = FEED_SHARED.duration_ns.load(Ordering::Relaxed);
    if duration <= 0 { return 0; }
    (duration - FEED_SHARED.playpos_ns.load(Ordering::Relaxed).max(0)).max(0) / 1_000_000
}

struct AutomaticLease { native_epoch: u32, token: u64 }
impl Drop for AutomaticLease {
    fn drop(&mut self) {
        let _ = FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.finish_hls_automatic_transition(self.token));
    }
}

struct TrialReserve { native_epoch: u32, token: u64 }
impl Drop for TrialReserve {
    fn drop(&mut self) {
        let _ = FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.finish_hls_trial(self.token));
    }
}

struct CandidateFence { native_epoch: u32, generation: u64 }
impl FenceSettlement for CandidateFence {
    fn settle(self: Box<Self>) {
        let settled = FEED_SHARED.with_active_demux_epoch(self.native_epoch, ||
            FEED_SHARED.settle_hls_candidate_transition(self.generation));
        if let Some(settled) = settled {
            debug_assert!(settled, "candidate generation changed inside its sole writer");
        }
    }
}

impl DemuxAdaptive for FeedDemuxClock {
    fn begin(&mut self, kind: AutomaticTransition, user_sequence: u64)
        -> Option<LeaseHandle> {
        let kind = match kind {
            AutomaticTransition::Original => HlsAutomaticTransition::Original,
            AutomaticTransition::QualityUp => HlsAutomaticTransition::QualityUp,
            AutomaticTransition::QualityDown => HlsAutomaticTransition::QualityDown,
        };
        let token = FEED_SHARED.with_active_demux_epoch(self.observer.native_epoch, ||
            FEED_SHARED.begin_hls_automatic_transition_at(kind, user_sequence)).flatten()?;
        Some(LeaseHandle::new(token, AutomaticLease {
            native_epoch: self.observer.native_epoch, token,
        }))
    }

    fn authorize(&mut self, token: u64) -> bool {
        FEED_SHARED.with_active_demux_epoch(self.observer.native_epoch, ||
            FEED_SHARED.authorize_hls_automatic_transition_commit(token)).unwrap_or(false)
    }

    fn arm_trial(&mut self, runway_ms: i64, user_sequence: u64)
        -> Option<LeaseHandle> {
        let token = FEED_SHARED.with_active_demux_epoch(self.observer.native_epoch, ||
            FEED_SHARED.arm_hls_trial_at(runway_ms, user_sequence)).flatten()?;
        Some(LeaseHandle::new(token, TrialReserve {
            native_epoch: self.observer.native_epoch, token,
        }))
    }

    fn arm_candidate(&mut self, automatic_token: u64)
        -> Result<FenceHandle, CandidateFenceError> {
        let generation = match FEED_SHARED.with_active_demux_epoch(self.observer.native_epoch, ||
            FEED_SHARED.begin_hls_candidate_transition(Some(automatic_token))) {
            None => return Err(CandidateFenceError::Aborted),
            Some(result) => match result {
            Ok(generation) => generation,
            Err(HlsClockFenceError::Stopping) => return Err(CandidateFenceError::Aborted),
            Err(HlsClockFenceError::Overlap) => return Err(CandidateFenceError::Overlap),
            Err(HlsClockFenceError::Exhausted) => return Err(CandidateFenceError::Exhausted),
            Err(HlsClockFenceError::Superseded) => return Err(CandidateFenceError::Superseded),
            },
        };
        Ok(FenceHandle::new(CandidateFence {
            native_epoch: self.observer.native_epoch, generation,
        }))
    }

    fn transition_permitted(&self, kind: AutomaticTransition) -> bool {
        FEED_SHARED.with_active_demux_epoch(self.observer.native_epoch, ||
            FEED_SHARED.hls_automatic_transition_permitted(match kind {
            AutomaticTransition::Original => HlsAutomaticTransition::Original,
            AutomaticTransition::QualityUp => HlsAutomaticTransition::QualityUp,
            AutomaticTransition::QualityDown => HlsAutomaticTransition::QualityDown,
        })).unwrap_or(false)
    }
}

/// The effect sink of one demux attempt: every publication is one typed write set, whole or
/// discarded.
pub struct FeedDemuxMedia {
    native_epoch: u32,
    hooks: DemuxMediaHooks,
}

impl DemuxMedia for FeedDemuxMedia {
    fn publish(&mut self, effect: DemuxEffect) {
        // Session reset holds this same native gate while replacing the mirrors.
        // One typed publication therefore belongs wholly to this open or is discarded.
        let mut previous_names = None;
        let _ = FEED_SHARED.with_active_demux_epoch(self.native_epoch, || match effect {
            DemuxEffect::AbrHlsStart { kbps } => {
                FEED_SHARED.dg_abr_mode.store(ABR_MODE_HLS, Ordering::Relaxed);
                FEED_SHARED.dg_abr_kbps.store(kbps, Ordering::Relaxed);
                FEED_SHARED.dg_abr_declared_kbps.store(-1, Ordering::Relaxed);
            }
            DemuxEffect::AbrHlsDeclared { kbps } =>
                FEED_SHARED.dg_abr_declared_kbps.store(kbps, Ordering::Relaxed),
            DemuxEffect::AbrCommitted { kbps, declared_kbps } => {
                FEED_SHARED.dg_abr_kbps.store(kbps, Ordering::Relaxed);
                FEED_SHARED.dg_abr_declared_kbps.store(declared_kbps, Ordering::Relaxed);
            }
            DemuxEffect::AbrOriginalStart { kbps } => {
                FEED_SHARED.dg_abr_mode.store(ABR_MODE_ORIGINAL, Ordering::Relaxed);
                FEED_SHARED.dg_abr_kbps.store(kbps, Ordering::Relaxed);
            }
            DemuxEffect::NoteFault(fault) => FEED_SHARED.note_demux_exit_latch((self.hooks.exit_latch)(fault)),
            DemuxEffect::FailFault(fault) => FEED_SHARED.fail_demux_latch((self.hooks.exit_latch)(fault)),
            DemuxEffect::IoFailed => FEED_SHARED.demux_io_failed.store(true, Ordering::Release),
            DemuxEffect::Failed => FEED_SHARED.demux_failed.store(true, Ordering::Release),
            DemuxEffect::NoVideo => FEED_SHARED.demux_no_video.store(true, Ordering::Release),
            DemuxEffect::ProbeActual { video, audio } => {
                if let Some(v) = video {
                    if v != 0 { FEED_SHARED.probe_actual_v.store(v, Ordering::Release); }
                }
                if let Some(a) = audio {
                    if a != 0 { FEED_SHARED.probe_actual_a.store(a, Ordering::Release); }
                }
            }
            DemuxEffect::HttpStatus(status) => FEED_SHARED.dg_http_status.store(status, Ordering::Relaxed),
            DemuxEffect::TrackNames { audio, subs } => {
                previous_names = Some((self.hooks.replace_track_names)(TrackNames { audio, subs }));
            }
            DemuxEffect::SideAnchor(anchor) => {
                *FEED_SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = anchor;
            }
            DemuxEffect::ContainerHdr(hdr) => {
                *FEED_SHARED.container_hdr.lock().unwrap() = hdr;
            }
            DemuxEffect::FirstAudioPts(pts) =>
                FEED_SHARED.first_audio_pts.store(pts, Ordering::Release),
            DemuxEffect::FileSize(size) => FEED_SHARED.file_size.store(size, Ordering::Release),
            DemuxEffect::TransportRc(rc) => FEED_SHARED.dg_transport_rc.store(rc, Ordering::Release),
            DemuxEffect::NetworkBytes(bytes) => { FEED_SHARED.dg_net_rx.fetch_add(bytes, Ordering::Relaxed); }
            DemuxEffect::DurationNs(ns) => FEED_SHARED.duration_ns.store(ns, Ordering::Relaxed),
            DemuxEffect::VideoRaster { width, height } => FEED_SHARED.publish_video_raster(width, height),
            DemuxEffect::VideoTailNs(ns) => FEED_SHARED.hls_video_tail_ns.store(ns, Ordering::Release),
            DemuxEffect::AudioTailNs(ns) => FEED_SHARED.hls_audio_tail_ns.store(ns, Ordering::Release),
            DemuxEffect::AudioExpected(expected) => FEED_SHARED.hls_audio_expected.store(expected, Ordering::Release),
            DemuxEffect::PrimeRunwayMax(runway_ms) => { FEED_SHARED.hls_prime_runway_ms.fetch_max(runway_ms, Ordering::AcqRel); }
            DemuxEffect::PrimeRunway(runway_ms) => FEED_SHARED.hls_prime_runway_ms.store(runway_ms, Ordering::Release),
            DemuxEffect::RecoverySegment { total_us, duration } => FEED_SHARED.observe_hls_recovery(total_us, duration),
        });
        // Old container strings may own large allocations. Release them after
        // both the track slot and native retirement barrier have been unlocked.
        drop(previous_names);
    }

    fn record(&self, line: &str) { plx_base::eventlog::log(line); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::subtitle::{DecodeKind, Manifest, RawCue, TrackInfo};

    /// The caption half is the composition's; these tests address the Feed's own state, which has
    /// none.
    struct NoCaptions;
    impl DemuxSubtitle for NoCaptions {
        fn plan(&mut self, _: &[TrackInfo]) -> Vec<DecodeKind> { Vec::new() }
        fn max_header_bytes(&self) -> usize { 0 }
        fn admit_font(&mut self, _: usize) -> bool { false }
        fn begin_sources(&mut self, _: &str, _: Manifest) -> Option<u64> { None }
        fn record_tracks(&self) {}
        fn selected_track(&self) -> i32 { -1 }
        fn bitmap_budget(&self) -> usize { 0 }
        fn emit(&mut self, _: u64, _: RawCue) {}
        fn seek(&mut self, _: u64) {}
    }

    fn no_latch(_: DemuxFault) -> u8 { 0 }
    fn keep_names(names: TrackNames) -> TrackNames { names }

    fn capture(attempt: AttemptId, native_epoch: u32) -> Box<dyn DemuxSession> {
        FeedDemuxSession::capture(
            attempt, native_epoch,
            DemuxMediaHooks { exit_latch: no_latch, replace_track_names: keep_names },
            |_| NoCaptions,
        )
    }

    struct ResetSession;
    impl Drop for ResetSession {
        fn drop(&mut self) { FEED_SHARED.reset_session(); }
    }

    #[test]
    fn user_pause_superseding_candidate_is_not_a_source_failure() {
        let _serial = plx_base::testlock::serial();
        let _reset = ResetSession;
        FEED_SHARED.reset_session();
        let epoch = FEED_SHARED.begin_native_session().expect("native epoch");
        let mut session = capture(AttemptId::new(8, 1), epoch);
        let generation = FEED_SHARED.hls_candidate_generation.load(Ordering::Acquire);
        let lease = session.adaptive().arm_trial(
            1_000, FEED_SHARED.hls_user_sequence(),
        ).expect("production upshift trial");
        assert_eq!(FEED_SHARED.hls_trial_reserve_ms.load(Ordering::Acquire), 1_000);
        let crate::shared::HlsUserPause::Issue(pause) =
            FEED_SHARED.prepare_hls_user_pause().expect("pause available") else {
                panic!("running playback must issue pause");
            };
        FEED_SHARED.complete_hls_user_pause(pause, true);
        let result = session.adaptive().arm_candidate(lease.token());
        assert!(result.is_err(), "superseded candidate must not publish");
        assert!(
            matches!(result, Err(CandidateFenceError::Superseded)),
            "viewer pause cancelled private candidate media, not the active source",
        );
        drop(lease);
        assert_eq!(FEED_SHARED.hls_trial_reserve_ms.load(Ordering::Acquire), -1,
            "cancelled trial must release its reserve");
        assert_eq!(FEED_SHARED.hls_candidate_generation.load(Ordering::Acquire), generation,
            "private candidate must not begin publication");
        assert!(FEED_SHARED.clock_user_held_for_test(),
            "candidate cleanup must preserve the viewer's pause");
    }

    #[test]
    fn retired_demux_cannot_consume_successors_seek() {
        let _serial = plx_base::testlock::serial();
        let _reset = ResetSession;
        FEED_SHARED.reset_session();
        let first_epoch = FEED_SHARED.begin_native_session().expect("first native epoch");
        let first = capture(AttemptId::new(5, 1), first_epoch);
        FEED_SHARED.reset_session();
        let next_epoch = FEED_SHARED.begin_native_session().expect("successor native epoch");
        let next = capture(AttemptId::new(5, 2), next_epoch);
        FEED_SHARED.seek_to_ns.store(222_000_000, Ordering::Release);
        assert_eq!(first.clock().take_seek_ns(), None, "retired worker consumed next seek");
        assert_eq!(next.clock().take_seek_ns(), Some(222_000_000));
    }

    #[test]
    fn retired_demux_cannot_claim_successors_adaptive_clock() {
        let _serial = plx_base::testlock::serial();
        let _reset = ResetSession;
        FEED_SHARED.reset_session();
        let first_epoch = FEED_SHARED.begin_native_session().expect("first native epoch");
        let mut first = capture(AttemptId::new(6, 1), first_epoch);
        let old = first.adaptive().begin(
            AutomaticTransition::QualityDown, FEED_SHARED.hls_user_sequence(),
        ).expect("first actuator lease");
        FEED_SHARED.reset_session();
        let next_epoch = FEED_SHARED.begin_native_session().expect("successor native epoch");
        let mut next = capture(AttemptId::new(6, 2), next_epoch);
        let current = next.adaptive().begin(
            AutomaticTransition::QualityDown, FEED_SHARED.hls_user_sequence(),
        ).expect("successor actuator lease");
        drop(old);
        assert!(next.adaptive().authorize(current.token()), "retired Drop cleared successor lease");
        drop(current);
        let sequence = FEED_SHARED.hls_user_sequence();
        assert!(first.adaptive().begin(AutomaticTransition::QualityDown, sequence).is_none());
        assert!(first.adaptive().arm_trial(1_000, sequence).is_none());
        let current = next.adaptive().begin(AutomaticTransition::QualityDown, sequence)
            .expect("successor still owns clock");
        assert!(matches!(first.adaptive().arm_candidate(current.token()), Err(CandidateFenceError::Aborted)));
        drop(current);
    }

    #[test]
    fn retained_clock_samplers_do_not_read_successor() {
        let _serial = plx_base::testlock::serial();
        let _reset = ResetSession;
        FEED_SHARED.reset_session();
        let first_epoch = FEED_SHARED.begin_native_session().expect("first native epoch");
        let first = capture(AttemptId::new(7, 1), first_epoch);
        let observer = first.observer();
        FEED_SHARED.playpos_ns.store(11_000_000, Ordering::Release);
        FEED_SHARED.duration_ns.store(100_000_000, Ordering::Release);
        let position = first.clock().playpos_sampler();
        let buffer = first.clock().hls_buffer_sampler();
        let remaining = first.clock().remaining_playback_sampler();
        let sequence = first.clock().user_sequence_sampler();
        assert_eq!(position(), Some(11_000_000));
        assert_eq!(remaining(), Some(89));
        assert!(buffer().is_some());
        assert!(sequence().is_some());

        FEED_SHARED.reset_session();
        let next_epoch = FEED_SHARED.begin_native_session().expect("successor native epoch");
        let next = capture(AttemptId::new(7, 2), next_epoch);
        FEED_SHARED.playpos_ns.store(22_000_000, Ordering::Release);
        FEED_SHARED.duration_ns.store(200_000_000, Ordering::Release);
        assert_eq!(position(), None);
        assert_eq!(buffer(), None);
        assert_eq!(remaining(), None);
        assert_eq!(sequence(), None);
        assert_eq!(observer.rebuffering(), None);
        assert_eq!(observer.audio_expected(), None);
        assert_eq!(observer.duration_ns(), None);
        assert_eq!(observer.hls_buffer_clocks(), None);
        assert_eq!(next.clock().playpos_sampler()(), Some(22_000_000));
        assert_eq!(next.clock().remaining_playback_sampler()(), Some(178));
    }

    #[test]
    fn retired_open_cannot_publish_into_next_sessions_diagnostics() {
        let _serial = plx_base::testlock::serial();
        let previous = FEED_SHARED.dg_http_status.load(Ordering::Relaxed);
        let epoch = FEED_SHARED.begin_native_session().expect("fixture owns native session");
        struct Restore { epoch: u32, successor: Option<u32>, status: i32 }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED.retire_native_session(self.epoch);
                if let Some(epoch) = self.successor {
                    FEED_SHARED.retire_native_session(epoch);
                }
                FEED_SHARED.dg_http_status.store(self.status, Ordering::Relaxed);
            }
        }
        let mut restore = Restore { epoch, successor: None, status: previous };
        let mut session = capture(AttemptId::new(4, 7), epoch);
        session.media().publish(DemuxEffect::HttpStatus(218));
        assert_eq!(FEED_SHARED.dg_http_status.load(Ordering::Relaxed), 218);

        assert!(FEED_SHARED.retire_native_session(epoch));
        let successor = FEED_SHARED.begin_native_session().expect("next native session begins");
        restore.successor = Some(successor);
        FEED_SHARED.dg_http_status.store(300, Ordering::Relaxed);
        session.media().publish(DemuxEffect::HttpStatus(499));
        assert_eq!(FEED_SHARED.dg_http_status.load(Ordering::Relaxed), 300);
        assert!(!session.observer().publish(DemuxCallbackEffect::HttpStatus(498)));
        assert_eq!(FEED_SHARED.dg_http_status.load(Ordering::Relaxed), 300);
    }

    /// The demux half of the Auto-quality facts and the prime runway each land in their mirror as
    /// the typed publication says, an HLS start before the declaration before a commit, and the
    /// runway's two writers differ: the round's `PrimeRunway` STORES (a newer, smaller value
    /// replaces) where the demux's `PrimeRunwayMax` only raises. A retired open writes neither.
    #[test]
    fn the_abr_and_prime_runway_publications_land_in_their_mirrors_for_the_current_open_only() {
        let _serial = plx_base::testlock::serial();
        let _reset = ResetSession;
        FEED_SHARED.reset_session();
        let first_epoch = FEED_SHARED.begin_native_session().expect("first native epoch");
        let mut first = capture(AttemptId::new(8, 1), first_epoch);
        let read = || (
            FEED_SHARED.dg_abr_mode.load(Ordering::Relaxed),
            FEED_SHARED.dg_abr_kbps.load(Ordering::Relaxed),
            FEED_SHARED.dg_abr_declared_kbps.load(Ordering::Relaxed),
        );
        assert_eq!(read(), (0, 0, -1), "no controller yet");
        first.media().publish(DemuxEffect::AbrHlsStart { kbps: 3_000 });
        assert_eq!(read(), (ABR_MODE_HLS, 3_000, -1), "the start declares nothing");
        first.media().publish(DemuxEffect::AbrHlsDeclared { kbps: 2_900 });
        assert_eq!(read(), (ABR_MODE_HLS, 3_000, 2_900));
        first.media().publish(DemuxEffect::AbrCommitted { kbps: 2_000, declared_kbps: 1_950 });
        assert_eq!(read(), (ABR_MODE_HLS, 2_000, 1_950), "a commit moves the request and the declaration together");
        first.media().publish(DemuxEffect::AbrOriginalStart { kbps: 8_000 });
        assert_eq!(read().0, ABR_MODE_ORIGINAL);
        assert_eq!(read().1, 8_000);

        first.media().publish(DemuxEffect::PrimeRunway(5_000));
        assert_eq!(FEED_SHARED.hls_prime_runway_ms.load(Ordering::Acquire), 5_000);
        first.media().publish(DemuxEffect::PrimeRunway(1_000));
        assert_eq!(FEED_SHARED.hls_prime_runway_ms.load(Ordering::Acquire), 1_000, "the round's value replaces");
        first.media().publish(DemuxEffect::PrimeRunwayMax(3_000));
        first.media().publish(DemuxEffect::PrimeRunwayMax(2_000));
        assert_eq!(FEED_SHARED.hls_prime_runway_ms.load(Ordering::Acquire), 3_000, "the demux's only raises");

        FEED_SHARED.reset_session();
        let next_epoch = FEED_SHARED.begin_native_session().expect("successor native epoch");
        let mut next = capture(AttemptId::new(8, 2), next_epoch);
        first.media().publish(DemuxEffect::AbrCommitted { kbps: 1, declared_kbps: 1 });
        first.media().publish(DemuxEffect::PrimeRunway(77));
        assert_eq!(read(), (0, 0, -1), "a retired open cannot write its successor's picture");
        assert_ne!(FEED_SHARED.hls_prime_runway_ms.load(Ordering::Acquire), 77);
        next.media().publish(DemuxEffect::PrimeRunway(77));
        assert_eq!(FEED_SHARED.hls_prime_runway_ms.load(Ordering::Acquire), 77);
    }
}
