//! Player-owned state and native-clock actuator for the physical demux worker.
//! Construction captures no globals; the worker samples each clock at its
//! original physical boundary through the lower contract.

use super::{HlsAutomaticTransition, HlsClockFenceError, UserPauseCursor, SHARED, TX};
use plx_playback_contract::adaptive::{AutomaticTransition, BufferFact, FenceHandle, FenceSettlement, LeaseHandle, PhysicalFailure};
use plx_playback_contract::demux::{DemuxAdaptive, DemuxCallbackEffect, DemuxClock, DemuxEffect, DemuxFault, DemuxMedia, DemuxObserver, DemuxSession, PauseClock, PauseEvent, PauseLane};
use plx_playback_contract::AttemptId;
use plx_telemetry::telemetry::classes::DemuxExitClass;
use std::sync::Arc;
use std::sync::atomic::Ordering;

pub struct PlayerDemuxSession {
    observer: Arc<PlayerDemuxObserver>,
    hls_pause: Option<UserPauseCursor>,
    original_pause: Option<UserPauseCursor>,
}

impl PlayerDemuxSession {
    /// Safe to capture on the main thread: pause cursors and the native clock
    /// are sampled only after the worker explicitly begins a lane.
    pub fn capture(attempt: AttemptId, native_epoch: u32) -> Box<dyn DemuxSession> {
        Box::new(Self {
            observer: Arc::new(PlayerDemuxObserver { attempt, native_epoch }),
            hls_pause: None, original_pause: None,
        })
    }

    fn cursor(&mut self, lane: PauseLane) -> &mut UserPauseCursor {
        match lane {
            PauseLane::Hls => self.hls_pause.as_mut()
                .expect("HLS pause lane begins after its cursor opens"),
            PauseLane::Original => self.original_pause.as_mut()
                .expect("Original pause lane begins before its source opens"),
        }
    }
}

impl DemuxSession for PlayerDemuxSession {
    fn observer(&self) -> Arc<dyn DemuxObserver> { self.observer.clone() }
    fn clock(&self) -> &dyn DemuxClock { self }
    fn clock_mut(&mut self) -> &mut dyn DemuxClock { self }
    fn adaptive_ref(&self) -> &dyn DemuxAdaptive { self }
    fn adaptive(&mut self) -> &mut dyn DemuxAdaptive { self }
    fn media(&mut self) -> &mut dyn DemuxMedia { self }
}

struct PlayerDemuxObserver {
    attempt: AttemptId,
    native_epoch: u32,
}

impl DemuxObserver for PlayerDemuxObserver {
    fn attempt(&self) -> AttemptId { self.attempt }
    fn playpos_ns(&self) -> Option<i64> {
        SHARED.with_active_demux_epoch(self.native_epoch, || SHARED.playpos_ns.load(Ordering::Relaxed))
    }
    fn hold_epoch(&self) -> Option<u64> {
        SHARED.with_active_demux_epoch(self.native_epoch, || SHARED.hls_internal_hold_epoch.load(Ordering::Acquire))
    }
    fn rebuffering(&self) -> Option<bool> {
        SHARED.with_active_demux_epoch(self.native_epoch, || SHARED.hls_rebuffering.load(Ordering::Acquire))
    }
    fn paused(&self) -> Option<bool> {
        SHARED.with_active_demux_epoch(self.native_epoch, || TX.paused.load(Ordering::Acquire))
    }
    fn pause_clock(&self) -> Option<PauseClock> {
        SHARED.with_active_demux_epoch(self.native_epoch, || {
            let (at, elapsed) = TX.pause_clock_sample();
            PauseClock { at, elapsed }
        })
    }
    fn publish(&self, effect: DemuxCallbackEffect) -> bool {
        SHARED.with_active_demux_epoch(self.native_epoch, || match effect {
            DemuxCallbackEffect::NetworkBytes(bytes) => {
                SHARED.dg_net_rx.fetch_add(bytes, Ordering::Relaxed);
            }
            DemuxCallbackEffect::HttpStatus(status) =>
                SHARED.dg_http_status.store(status, Ordering::Relaxed),
            DemuxCallbackEffect::FileSize(size) =>
                SHARED.file_size.store(size, Ordering::Release),
            DemuxCallbackEffect::TransportFailure { rc, age_ms, reopen_failed } => {
                SHARED.dg_transport_rc.store(rc, Ordering::Release);
                SHARED.dg_transport_age_ms.store(age_ms, Ordering::Release);
                SHARED.dg_transport_reopen_failed.store(reopen_failed, Ordering::Release);
                SHARED.demux_io_failed.store(true, Ordering::Release);
            }
            DemuxCallbackEffect::IoFailed =>
                SHARED.demux_io_failed.store(true, Ordering::Release),
            DemuxCallbackEffect::RequestTerminalHold => {
                SHARED.hls_rebuffer_request_tail_ns.store(
                    SHARED.hls_video_tail_ns.load(Ordering::Acquire), Ordering::Release,
                );
                SHARED.hls_rebuffer_requested.store(true, Ordering::Release);
            }
        }).is_some()
    }
}

impl DemuxClock for PlayerDemuxSession {
    fn playpos_ns(&self) -> i64 { SHARED.playpos_ns.load(Ordering::Relaxed) }
    fn display_base_ns(&self) -> i64 { SHARED.disp_base.load(Ordering::Relaxed) }
    fn duration_ns(&self) -> i64 { SHARED.duration_ns.load(Ordering::Relaxed) }
    fn video_tail_ns(&self) -> i64 { SHARED.hls_video_tail_ns.load(Ordering::Acquire) }
    fn audio_tail_ns(&self) -> i64 { SHARED.hls_audio_tail_ns.load(Ordering::Acquire) }
    fn hold_epoch(&self) -> u64 { SHARED.hls_internal_hold_epoch.load(Ordering::Acquire) }
    fn rebuffering(&self) -> bool { SHARED.hls_rebuffering.load(Ordering::Acquire) }
    fn rebuffer_requested(&self) -> bool { SHARED.hls_rebuffer_requested.load(Ordering::Acquire) }
    fn paused(&self) -> bool { TX.paused.load(Ordering::Acquire) }
    fn pause_clock(&self) -> PauseClock {
        let (at, elapsed) = TX.pause_clock_sample();
        PauseClock { at, elapsed }
    }
    fn begin_pause_lane(&mut self, lane: PauseLane) {
        let cursor = UserPauseCursor::new(TX.pause_state_sample());
        match lane {
            PauseLane::Hls => self.hls_pause = Some(cursor),
            PauseLane::Original => self.original_pause = Some(cursor),
        }
    }
    fn pause_event(&mut self, lane: PauseLane) -> PauseEvent {
        let snapshot = TX.pause_state_sample();
        let completed_ms = self.cursor(lane).consume_completed(snapshot)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX));
        PauseEvent { paused: snapshot.paused, sequence: snapshot.sequence, completed_ms }
    }
    fn seeking(&self) -> bool { SHARED.seeking.load(Ordering::Relaxed) }
    fn take_seek_ns(&self) -> Option<i64> {
        let ns = SHARED.seek_to_ns.swap(-1, Ordering::Acquire);
        (ns >= 0).then_some(ns)
    }
    fn user_sequence(&self) -> u64 { SHARED.hls_user_sequence() }
    fn desired_audio_idx(&self) -> i32 { SHARED.desired_audio_idx.load(Ordering::Relaxed) }
    fn applied_rate_permille(&self) -> i32 {
        SHARED.applied_rate_permille.load(Ordering::Relaxed)
    }
    fn side_subtitle_owner(&self) -> bool {
        SHARED.side_subs_owner.load(Ordering::Acquire)
    }
    fn playpos_sampler(&self) -> fn() -> i64 {
        || SHARED.playpos_ns.load(Ordering::Relaxed).max(0)
    }
    fn hls_buffer_sampler(&self) -> fn() -> Option<i64> { hls_buffered_ms }
    fn remaining_playback_sampler(&self) -> fn() -> i64 { remaining_playback_ms }
    fn user_sequence_sampler(&self) -> fn() -> u64 { || SHARED.hls_user_sequence() }
}

fn hls_buffered_ms() -> Option<i64> {
    let playhead_ns = SHARED.playpos_ns.load(Ordering::Relaxed).max(0);
    let video = SHARED.hls_video_tail_ns.load(Ordering::Acquire);
    let audio = SHARED.hls_audio_tail_ns.load(Ordering::Acquire);
    let display_base = SHARED.disp_base.load(Ordering::Relaxed).max(0);
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
    let duration = SHARED.duration_ns.load(Ordering::Relaxed);
    if duration <= 0 { return 0; }
    (duration - SHARED.playpos_ns.load(Ordering::Relaxed).max(0)).max(0) / 1_000_000
}

struct AutomaticLease(u64);
impl Drop for AutomaticLease {
    fn drop(&mut self) { let _ = SHARED.finish_hls_automatic_transition(self.0); }
}

struct TrialReserve(u64);
impl Drop for TrialReserve {
    fn drop(&mut self) { let _ = SHARED.finish_hls_trial(self.0); }
}

struct CandidateFence(u64);
impl FenceSettlement for CandidateFence {
    fn settle(self: Box<Self>) {
        let settled = SHARED.settle_hls_candidate_transition(self.0);
        debug_assert!(settled, "candidate generation changed inside its sole writer");
    }
}

impl DemuxAdaptive for PlayerDemuxSession {
    fn begin(&mut self, kind: AutomaticTransition, user_sequence: u64)
        -> Option<LeaseHandle> {
        let kind = match kind {
            AutomaticTransition::Original => HlsAutomaticTransition::Original,
            AutomaticTransition::QualityUp => HlsAutomaticTransition::QualityUp,
            AutomaticTransition::QualityDown => HlsAutomaticTransition::QualityDown,
        };
        let token = SHARED.begin_hls_automatic_transition_at(kind, user_sequence)?;
        Some(LeaseHandle::new(token, AutomaticLease(token)))
    }

    fn authorize(&mut self, token: u64) -> bool {
        SHARED.authorize_hls_automatic_transition_commit(token)
    }

    fn arm_trial(&mut self, runway_ms: i64, user_sequence: u64)
        -> Option<LeaseHandle> {
        let token = SHARED.arm_hls_trial_at(runway_ms, user_sequence)?;
        Some(LeaseHandle::new(token, TrialReserve(token)))
    }

    fn arm_candidate(&mut self, automatic_token: u64)
        -> Result<FenceHandle, PhysicalFailure> {
        let generation = match SHARED.begin_hls_candidate_transition(Some(automatic_token)) {
            Ok(generation) => generation,
            Err(HlsClockFenceError::Stopping) => return Err(PhysicalFailure::Aborted),
            Err(HlsClockFenceError::Overlap) => return Err(PhysicalFailure::Failed(
                "overlapping HLS candidate publication",
            )),
            Err(HlsClockFenceError::Exhausted) => return Err(PhysicalFailure::Failed(
                "HLS candidate generation exhausted",
            )),
            Err(HlsClockFenceError::Superseded) => return Err(PhysicalFailure::Failed(
                "HLS candidate automatic lease was superseded",
            )),
        };
        Ok(FenceHandle::new(CandidateFence(generation)))
    }

    fn transition_permitted(&self, kind: AutomaticTransition) -> bool {
        SHARED.hls_automatic_transition_permitted(match kind {
            AutomaticTransition::Original => HlsAutomaticTransition::Original,
            AutomaticTransition::QualityUp => HlsAutomaticTransition::QualityUp,
            AutomaticTransition::QualityDown => HlsAutomaticTransition::QualityDown,
        })
    }
}

impl DemuxMedia for PlayerDemuxSession {
    fn publish(&mut self, effect: DemuxEffect) {
        match effect {
            DemuxEffect::AbrHlsStart { kbps } => {
                SHARED.dg_abr_mode.store(super::ABR_MODE_HLS, Ordering::Relaxed);
                SHARED.dg_abr_kbps.store(kbps, Ordering::Relaxed);
                SHARED.dg_abr_declared_kbps.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_media_kbps.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_net_kbps.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_buffer_ms.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_ratio_pm.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_action.store(super::ABR_ACTION_STEADY, Ordering::Relaxed);
                SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
                SHARED.dg_abr_unsafe_deficit_ms.store(0, Ordering::Relaxed);
            }
            DemuxEffect::AbrHlsDeclared { kbps } =>
                SHARED.dg_abr_declared_kbps.store(kbps, Ordering::Relaxed),
            DemuxEffect::AbrOriginalStart { kbps } => {
                SHARED.dg_abr_mode.store(super::ABR_MODE_ORIGINAL, Ordering::Relaxed);
                SHARED.dg_abr_kbps.store(kbps, Ordering::Relaxed);
                SHARED.dg_abr_net_kbps.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_buffer_ms.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_ratio_pm.store(-1, Ordering::Relaxed);
                SHARED.dg_abr_action.store(super::ABR_ACTION_STEADY, Ordering::Relaxed);
                SHARED.dg_abr_target_kbps.store(0, Ordering::Relaxed);
                SHARED.dg_abr_unsafe_deficit_ms.store(0, Ordering::Relaxed);
            }
            DemuxEffect::NoteFault(fault) => SHARED.note_demux_exit(demux_exit_class(fault)),
            DemuxEffect::FailFault(fault) => SHARED.fail_demux(demux_exit_class(fault)),
            DemuxEffect::IoFailed => SHARED.demux_io_failed.store(true, Ordering::Release),
            DemuxEffect::Failed => SHARED.demux_failed.store(true, Ordering::Release),
            DemuxEffect::NoVideo => SHARED.demux_no_video.store(true, Ordering::Release),
            DemuxEffect::HttpStatus(status) => SHARED.dg_http_status.store(status, Ordering::Relaxed),
            DemuxEffect::TrackNames { audio, subs } => {
                *SHARED.track_names.lock().unwrap() = super::TrackNames { audio, subs };
            }
            DemuxEffect::SideAnchor(anchor) => {
                *SHARED.side_anchor.lock().unwrap_or_else(|e| e.into_inner()) = anchor;
            }
            DemuxEffect::FirstAudioPts(pts) =>
                SHARED.first_audio_pts.store(pts, Ordering::Release),
            DemuxEffect::FileSize(size) => SHARED.file_size.store(size, Ordering::Release),
            DemuxEffect::TransportRc(rc) => SHARED.dg_transport_rc.store(rc, Ordering::Release),
            DemuxEffect::NetworkBytes(bytes) => { SHARED.dg_net_rx.fetch_add(bytes, Ordering::Relaxed); }
            DemuxEffect::DurationNs(ns) => SHARED.duration_ns.store(ns, Ordering::Relaxed),
            DemuxEffect::VideoRaster { width, height } => SHARED.publish_video_raster(width, height),
            DemuxEffect::VideoTailNs(ns) => SHARED.hls_video_tail_ns.store(ns, Ordering::Release),
            DemuxEffect::AudioTailNs(ns) => SHARED.hls_audio_tail_ns.store(ns, Ordering::Release),
            DemuxEffect::AudioExpected(expected) => SHARED.hls_audio_expected.store(expected, Ordering::Release),
            DemuxEffect::PrimeRunwayMax(runway_ms) => { SHARED.hls_prime_runway_ms.fetch_max(runway_ms, Ordering::AcqRel); }
            DemuxEffect::RecoverySegment { total_us, duration } => SHARED.observe_hls_recovery(total_us, duration),
        }
    }

    fn record(&self, line: &str) { super::log(line); }
}

fn demux_exit_class(fault: DemuxFault) -> DemuxExitClass {
    match fault {
        DemuxFault::HttpsReserveFailed => DemuxExitClass::HttpsReserveFailed,
        DemuxFault::HttpsOpenFailed => DemuxExitClass::HttpsOpenFailed,
        DemuxFault::HttpOpenFailed => DemuxExitClass::HttpOpenFailed,
        DemuxFault::OpenInputFailed => DemuxExitClass::OpenInputFailed,
        DemuxFault::FindStreamInfoFailed => DemuxExitClass::FindStreamInfoFailed,
        DemuxFault::NoVideoStream => DemuxExitClass::NoVideoStream,
        DemuxFault::NoFraming => DemuxExitClass::NoFraming,
        DemuxFault::LoadMismatch => DemuxExitClass::LoadMismatch,
        DemuxFault::NoAudioAu => DemuxExitClass::NoAudioAu,
        DemuxFault::NoUsableVideo => DemuxExitClass::NoUsableVideo,
        DemuxFault::DtsCoreMissing => DemuxExitClass::DtsCoreMissing,
        DemuxFault::HlsFailed => DemuxExitClass::HlsFailed,
        DemuxFault::HlsNotReady => DemuxExitClass::HlsNotReady,
        DemuxFault::DemuxPanicked => DemuxExitClass::DemuxPanicked,
        DemuxFault::NoAccessUnits => DemuxExitClass::NoAccessUnits,
        DemuxFault::Other => DemuxExitClass::Other,
    }
}
