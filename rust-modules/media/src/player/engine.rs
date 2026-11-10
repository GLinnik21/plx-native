//! player::engine — the common route and telemetry bridge for the physical Feed
//! executor in `plx_play_feed::native_executor`. The lower Engine owns its stream
//! and AU queues, hands their raw pointers to workers, and outlives the workers.
//! No worker names an Engine field through the common player's slot.
//!
//! That confinement is **enforced, not just asserted**: the slot is `App.adapters.player`, a
//! [`crate::player::adapter::PlayerAdapter`] that consumes a [`MainThread`] at construction, so a
//! `&mut PlayerAdapter` borrow is what confines it now (see `player::adapter`). Which is also the
//! rule for the rest of this file — a function here takes the token **iff** it reaches the slot
//! or the ACB/Starfish seam, so the parameter carries information. `FeedCommand::ArmOpen` (the Feed's
//! `FEED_SHARED.arm_open`) runs on the main thread too and deliberately takes none: it only
//! publishes to the Feed's own state.
#[cfg(test)]
use plx_play_feed::shared::FEED_SHARED;
#[cfg(all(test, feature = "hostsim"))]
use super::route_start::{exit_native, exit_native_with, finish_release_for_test, poll_release,
    reap_abandoned_load, release_server_stopped_engine, NativeExit};
#[cfg(test)]
use super::feed_open::PayloadFacts;
#[cfg(test)]
use super::feed_open::install_synthetic_playurl;
#[cfg(test)]
use plx_play_feed::{
    renamed_static_payload, static_envelope, with_opus_info, SinkEnvelope, PAYLOAD_H265, PAYLOAD_V,
};
#[cfg(test)]
use plx_play_feed::{app_pcm_for_rate, audio_payload_codec, fed_fps, parse_sinkmax, sink_envelope};
#[cfg(test)]
use plx_play_feed::pcm_lead::PcmLead;
#[cfg(test)]
use plx_play_feed::pcm_lead::pcm_lead_plan;
#[cfg(test)]
use plx_play_feed::pcm_lead::{pcm_output_lead, PCM_LEAD_BY_OUTPUT, PCM_SINK_LEAD_NS};
#[cfg(test)]
use plx_play_feed::{
    fed_fps_rational, opus_head, pcm_rate_khz, ENVELOPE_FHD60, ENVELOPE_UHD60, PAYLOAD_AV,
};
pub(crate) use plx_platform::devcaps::LoadVideo;
#[cfg(test)]
pub(crate) use plx_play_feed::load_video_refused;
#[cfg(all(test, feature = "hostsim"))]
use plx_playback_contract::feed::Stage;
#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::seek::{inplace_layout_known, old_abi_seek_tested, select_seek_mode};
#[cfg(all(test, feature = "hostsim"))]
use plx_playback_contract::feed::SeekMode;
#[cfg(test)]
use super::log;
#[cfg(all(test, feature = "hostsim"))]
use plx_base::task::MainThread;
#[cfg(test)]
use plx_platform::devcaps::AudioStack;
use plx_telemetry::telemetry::window::Stage as WindowStage;
#[cfg(all(test, feature = "hostsim"))]
use std::os::raw::{c_char, c_int};
use std::sync::atomic::Ordering;

/// The AU queues' BASE byte caps — the denominators the read-out shows `dg_aq_*` against, so a viewer
/// can see backpressure (a video lane pinned at its cap is the demuxer outrunning the decoder;
/// both lanes empty while Stage is Playing is the opposite).
///
/// **Base, not the cap in force.** For the first seconds of a file whose audio starts so late that
/// the video lane fills to this cap before the first audio packet, the demuxer lifts the video
/// lane's cap by `ff::PRE_AUDIO_VIDEO_HEADROOM` (24 MiB) and then walks it back down as the
/// consumer drains, so the read-out can show a video lane ABOVE this number then. That is the
/// headroom doing its job, not an accounting error. An ordinary file never sees it: the grant is
/// taken only when a video push would park at this cap with no audio yet. The reachable-reserve
/// model (`abr::plant::b_max_est_ms`) wants exactly this steady-state number: the headroom is a
/// transient of the start of one kind of file, gone before the buffer-driven controller acts.
/// The audio lane's base cap is soft in the other direction: a file that stores more audio than
/// this ahead of its video (issue #600) runs the lane up to `ff::AUDIO_AHEAD_HEADROOM` (16 MiB)
/// beyond it for as long as the video lane holds less than `MAX_FEED_AHEAD_NS` of media.
pub const fn aq_caps() -> (i64, i64) {
    plx_playback_contract::feed::aq_caps()
}

/// **The feed-ahead throttle's two leads, milliseconds** — video, then audio.
///
/// The other half of the reachable-reserve geometry. `B_max = lead + queue_bytes/rate` and
/// `aq_caps` above exposes only the `queue_bytes`; `abr/sim.rs` — the closed-loop plant the ABR
/// controller is graded against — carried these two BY VALUE with a comment, so half of `B_max`
/// could move here and nothing anywhere would fail.
///
/// That is not hypothetical. The same shape had already happened one level up: `sim.rs`'s
/// operating-point table was hand-transcribed, the fixture pack was rebuilt under it, and the
/// plant modelled a television that no longer existed at two of its three points for a month
/// without a single test going red. This closes the identical hole in the geometry.
pub const fn feed_leads_ms() -> (i64, i64) {
    (
        MAX_FEED_AHEAD_NS / 1_000_000,
        (MAX_FEED_AHEAD_NS + AUDIO_SLACK_NS) / 1_000_000,
    )
}

// Per-lane queue byte caps (two-lane feed). Audio is kept small (the TV is RAM-tight and audio
// frames are tiny) yet large enough to cushion the single demux thread briefly blocking on a full
// video lane.
//
// **The video cap is 10 MiB, and it stopped matching the Load payload's `srcBufferLevelVideo`
// (8 MiB) on purpose.** Those are two different buffers: this one sits between our demux thread
// and our pump, and that one is what Starfish will hold. They were equal because 8 MiB was copied
// from the payload, not because anything requires it — feeding past Starfish's own level returns
// `'B'` and the pump retries, which is the backpressure path every playback already uses.
//
// **What the 2 MiB buys, and why it is not a guess.** The reserve the adaptive controller can ever
// see is `B_max = feed_lead + queue_bytes*8 / R_elementary` — a queue term that falls as `1/R`
// while the upshift guard it has to clear is `Omega(D)`. R2 of the plan of record showed those
// crossing at about 15.7 Mbit/s of media at 8 MiB, i.e. **the top of the ladder is unreachable for
// any guard of that shape**, and no control law fixes a plant that cannot hold the reserve its own
// guard demands. `tools/abr-plant-sweep.py` prices this at +555 ms of climbable ceiling WITH the
// graded-reject feed and **+17 ms without it** — inside a measured 167 ms noise floor — so the two
// levers are one decision and neither is worth landing alone.
// `docs/measurements/p0-plant-sizing.md` is the record; `requiredMemory` is 160 MB, so +2 MiB RSS
// on a 32-bit set is inside the budget already declared.
#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::feed_state::{AQ_VIDEO_BYTES, AQ_AUDIO_BYTES};

#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::feed_state::{Engine, FeedPace, Source};
#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::feed_state::engines_dropped_for_test;

// The `ENGINE` slot is `App.adapters.player` since phase 9 — a field, reached as
// `pa: &mut PlayerAdapter` from the loop. `player::adapter` carries the confinement argument the
// four `static mut` accessors used to carry here, and says what the owned slot proves that the
// `&MainThread` argument could only assert.

/// The `on_release_step` hook: the release's breadcrumbs belong to the common telemetry owner.
pub fn release_step(step: plx_playback_contract::feed::ReleaseStep) {
    use plx_playback_contract::feed::ReleaseStep;
    window_step(match step {
        ReleaseStep::NativeUnload => WindowStage::NativeUnload,
        ReleaseStep::AcbPlaneUnload => WindowStage::AcbPlaneUnload,
        ReleaseStep::NativeQuarantined => WindowStage::NativeQuarantined,
        ReleaseStep::NativeDestroyed => WindowStage::NativeDestroyed,
    });
}

/// The `on_exported_window_destroy` hook.
pub fn exported_window_destroyed() {
    window_step(WindowStage::ExportedWindowDestroy);
}

#[cfg(all(test, feature = "hostsim"))]
fn release_native_object(mt: &MainThread, epoch: u32) {
    let _ = release_native_object_timed(mt, epoch, &mut TeardownSpans::new());
}

#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::native_executor::TeardownSpans;

#[cfg(all(test, feature = "hostsim"))]
fn release_native_object_timed(mt: &MainThread, epoch: u32, sp: &mut TeardownSpans)
    -> plx_playback_contract::ReleaseStatus {
    plx_play_feed::native_executor::release_native_object_timed(
        mt, epoch, sp, &FEED_SHARED, &crate::player::test_physical::TEST_HOOKS.release)
}

/// Release breadcrumbs belong to the common telemetry owner, not the physical executor.
fn window_step(stage: WindowStage) {
    plx_telemetry::telemetry::window::record_in_cycle(
        plx_telemetry::telemetry::window::Observation::step(stage, None),
    );
}

/// **The frame rate a playback speed may feed this stream at**: [`super::SPEED_MAX_FPS`], or the
/// measured decoder row's rate when that is lower. A speed feeds every frame `rate` times closer
/// together and the Load declares that fed rate, which [`plx_play_feed::sink_envelope`] then clamps to the row; a
/// speed whose fed rate passes the row is one the Load cannot declare, so the menu does not offer
/// it. Only a table that was actually READ bounds anything (`measured`,
/// the same condition [`sink_envelope_now`] clamps under), a stream whose codec the Load cannot
/// declare has no row, and a row with no rate (0) is no bound. The `+ 0.5` is the 60.5 ceiling's own
/// tolerance, so a 29.97 stream is not turned away from a 30 row. Pure; the wrapper feeds it the
/// route.
pub(crate) fn speed_fps_ceiling(
    video: Option<LoadVideo>,
    caps: &plx_platform::devcaps::Caps,
    measured: bool,
) -> f64 {
    let row_fps = match video {
        Some(video) if measured => video.decoder_row(caps).2,
        _ => 0,
    };
    if row_fps == 0 {
        return super::SPEED_MAX_FPS;
    }
    (f64::from(row_fps) + 0.5).min(super::SPEED_MAX_FPS)
}

#[cfg(test)]
use plx_play_feed::native_executor::{audio_declaration, load_audio};

// Route-format adapters for the neutral payload constructor.
#[cfg(test)]
fn with_pcm_info(p: &str, audio: &str, pcm: Option<crate::route::PcmFormat>) -> String {
    plx_play_feed::with_pcm_info(p, audio, pcm.map(Into::into))
}

/// The pipeline reads the true dimensions from the SPS (Phase 0 HEVC probe), so mw/mh are only
/// the sink envelope.
#[cfg(test)]
fn build_av_payload(ps: &crate::route::PlaybackSession, video: &str, audio: &str, env: SinkEnvelope) -> String {
    build_av_payload_at(ps, video, audio, env, FEED_SHARED.applied_rate_permille.load(std::sync::atomic::Ordering::Relaxed))
}

/// [`build_av_payload`] at an explicit session speed (`rate`, permille), so the host tests can pin
/// it without touching the process-wide `applied_rate_permille`.
#[cfg(test)]
fn build_av_payload_at(
    ps: &crate::route::PlaybackSession, video: &str, audio: &str, env: SinkEnvelope, rate: i32,
) -> String {
    let facts = PayloadFacts::capture(ps).to_feed();
    build_av_payload_from_facts(&facts, video, audio, env, rate, super::feed_open::payload_options(&facts))
}

#[cfg(test)]
use plx_play_feed::native_executor::build_av_payload_from_facts;

/// The `contents.immersive` node — **the Dolby Atmos half of the same envelope**, and the reason
/// the television's Atmos read-out never appeared while its Dolby Vision one did.
///
/// PURE, so the splice is host-testable, and spliced at the same `provider` anchor and in the same
/// shape as [`with_dolby_hdr_info`], which is its sibling in every sense: one key inside
/// `externalStreamingInfo.contents`, one string, and the whole difference between a stream the
/// pipeline treats as ordinary E-AC3 and one it treats as immersive.
///
/// **The key and the value are both read off the television's own binaries** (2026-08-21), which
/// matters because neither is guessable and a wrong one is silent:
/// - `libpf-1.0.so.1.0.0` holds the literal key path `option.externalStreamingInfo.contents.immersive`,
///   logs it as `PF_EXT_IMMERSIVE : %s`, and carries it onto the audio caps it builds —
///   `audio mediaInfo … channels[%d] language[%s] … immersive[%s] role[%s]`. It is a `%s` all the
///   way down: libpf validates nothing and passes the string through.
/// - The VALUE therefore has to come from whoever fills it, and **the bare literal `ATMOS` exists in
///   exactly one library on this device: `libcbe.so`** — Chromium's media backend, i.e. the path
///   LG's own web apps (Plex's included) take. In its string pool that literal sits immediately
///   after `immersive` and in the same run as `externalStreamingInfo`, `esInfo`, `seperatedPTS`,
///   `provider`, `DolbyHdrInfo`, `encryptionType`, `profileId` and `contents` — the payload we
///   build, key for key. So `"ATMOS"` is not our invention; it is the value the working client
///   sends, recovered from the binary that sends it.
///
/// `libplayerAPIs` injects `platformSupportDolbyATMOS` itself from its configd cache, exactly as it
/// does for Dolby Vision, so this node states a fact about the STREAM and never about the set.
#[cfg(test)]
fn with_immersive(p: &str, audio: &str, atmos: bool) -> String {
    let (payload, notice) = plx_play_feed::with_immersive(p, audio, atmos);
    if let Some(line) = notice { log(&line); }
    payload
}

/// The `contents.DolbyHdrInfo` node, spliced into the Load payload only for a route whose frozen
/// decision declared Dolby Vision after affirmative platform capability.
///
/// PURE, so the splice is host-testable; the decision arrives from route state and is the SAME
/// frozen value `route::build_stream` gated direct play on
/// ([`plx_data::metadata::Dovi::presentation`]).
///
/// **Why this one node is the fix, from the television's own binaries** (decompiled 2026-08-21,
/// webOS 4.10.2 `libpf`): `CustomPipeline::parseOptionStringSpi` builds the literal key
/// `option.externalStreamingInfo.contents.DolbyHdrInfo`, asks `Options::checkKeyExistance` for it,
/// and on a hit sets `hasDolbyHdrInfo` — unconditionally, before a single sub-field is read and
/// with no platform gate. `getVideoCaps` then ends by adding `dolby-vision=TRUE` (plus
/// `dolby-vision-profile` when `profileId != -1`) to the caps it was already building. Without the
/// node, appsrc gets plain `video/x-h265` and nothing downstream can engage Dolby Vision. The
/// pipeline has parsed this all along; we simply never sent it.
///
/// Three things that look like they should change and do not:
/// - **the codec string stays `"H265"`.** `getVideoCaps` maps H265 to `video/x-h265` and that
///   branch falls THROUGH into the Dolby Vision tail; there is no DVHE/DVH1 entry in its codec
///   table (those literals belong to AdaptivePipeline's RFC-6381 parser, a different pipeline).
///   LG's own Chromium client also reports `codec.video = "H265"` for a Dolby Vision stream. The
///   codec guard below is therefore a consistency check, not a translation. It also admits
///   `"AV1"` (Profile 10) with the codec string unchanged likewise; that the AV1 branch reaches
///   the same tail is measured on a webOS 6 set, not decompiled
///   ([`plx_data::metadata::Dovi::presentation`]).
/// - **`profileId` must be a JSON integer** (`getInt`). Quoting it would leave the `-1` sentinel.
/// - **the library's injected platform metadata is not a safety gate.** libplayerAPIs injects
///   `platformSupportDolbyVision` / `supportDolbyTVATMOS` at the root, but libpf enables DV from
///   our node's presence regardless. We do not duplicate those root fields; `webos::caps` reads
///   the same public configd key independently and route policy requires an exact `true` before
///   this function can receive `Declare`.
///
/// The anchor is `"provider":"plxnative"` — the last key of `contents` and, by the test below,
/// present exactly once in `PAYLOAD_AV`. A `replace` that finds nothing is a silent no-node, which
/// is why the miss is logged rather than assumed away.
#[cfg(test)]
fn with_dolby_hdr_info(p: &str, video: &str, dv: plx_data::metadata::DvPresentation) -> String {
    let (payload, notice) = plx_play_feed::with_dolby_hdr_info(
        p, video, dv.declared().map(|n| plx_playback_contract::feed::DolbyHdrInfo {
            profile_id: n.profile_id, track_type: n.track_type, encryption_type: n.encryption_type,
        }), dv.declared().is_some() && plx_data::metadata::dv_node_suppressed(),
    );
    if let Some(line) = notice { log(&line); }
    payload
}

#[cfg(test)]
use plx_play_feed::native_executor::{app_id_key, with_app_id};

#[cfg(all(test, feature = "hostsim"))]
fn with_window_id(mt: &MainThread, p: &str) -> String {
    plx_play_feed::native_executor::with_window_id(mt, p, &crate::player::test_physical::TEST_HOOKS.prepare)
}

/// The `on_exported_window_create` hook.
pub fn exported_window_created() {
    window_step(WindowStage::ExportedWindowCreate);
}

pub use super::route_start::{reload_at, reload_transcode, reload_transcode_tracked,
    stop_bufferfeed, suspend_bufferfeed, switch_audio_native, ReloadOutcome};
#[cfg(all(test, feature = "hostsim"))]
pub(super) use super::route_start::{RELOAD_AT_LOG, RELOAD_PREROLL_LOG};

pub use super::route_start::{start_bufferfeed, start_bufferfeed_tracked, BufferfeedStartOutcome};

#[cfg(test)]
use plx_play_feed::native_executor::stream_open_log_line;

/// Set where the pump ends an Engine for good under the failure read-out; spent by
/// [`super::route_start::release_failed_engine`] or by any teardown.
static FAILED_ENGINE_RELEASE_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The pump just failed the Engine for good ([`crate::route::fail_current_engine`] and the Error
/// state): release it next frame. Armed, not done here, because the pump still holds the Engine.
pub(super) fn arm_failed_engine_release() {
    FAILED_ENGINE_RELEASE_ARMED.store(true, Ordering::Release);
}

pub(super) fn failed_engine_release_armed() -> bool {
    FAILED_ENGINE_RELEASE_ARMED.load(Ordering::Acquire)
}

pub(super) fn clear_failed_engine_release_arm() {
    FAILED_ENGINE_RELEASE_ARMED.store(false, Ordering::Release);
}

pub(crate) use plx_playback_contract::feed::MAX_FEED_AHEAD_NS;
#[cfg(test)]
pub(crate) use plx_play_feed::feed_state::{AUDIO_LEAD_IN_NS, PRIME_VIDEO_MAX_NS};
pub use plx_playback_contract::feed::PRES_NONE;
pub(crate) use plx_playback_contract::feed::AUDIO_SLACK_NS;
#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::native_executor::{
    AUDIO_STALE_AHEAD_NS, FEED_LANE_SLICE_US, FEED_LOW_WATER_NS, PRIME_AUDIO_NS, PRIME_NS,
};

/// VIDEO lane feeder (aq_video is video-only). Owns the seek rebase + in-place-seek handshake + prime→Play, all of
/// which key off the first post-seek VIDEO keyframe. A BufferFull/over-budget breaks THIS lane
/// only — the audio lane (feed_audio_lane) keeps flowing so the audioSync master clock advances.
#[cfg(test)]
fn audio_sink_pts(fp: i64, pcm_lead_ns: i64) -> i64 {
    plx_play_feed::native_executor::audio_sink_pts(fp, pcm_lead_ns, FEED_SHARED.applied_rate_permille.load(std::sync::atomic::Ordering::Relaxed))
}

#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::native_executor::{settle_pcm_lead, hls_candidate_snapshot_stable};

#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::native_executor::arm_live_clock_prime;


#[cfg(all(test, feature = "hostsim"))]
pub(super) fn try_prime(mt: &MainThread, eng: &mut Engine) {
    plx_play_feed::native_executor::try_prime(mt, eng, &plx_play_feed::shared::FEED_TX, &crate::player::test_physical::TEST_HOOKS.feed);
}

#[cfg(all(test, feature = "hostsim"))]
pub(super) fn feed_both_lanes(mt: &MainThread, eng: &mut Engine) {
    plx_play_feed::native_executor::feed_both_lanes(mt, eng, &plx_play_feed::shared::FEED_TX, &crate::player::test_physical::TEST_HOOKS.feed);
}

#[cfg(test)]
use plx_play_feed::native_executor::audio_lead_in;

#[cfg(all(test, feature = "hostsim"))]
mod native_lifecycle_host_seam_tests {
    use super::*;

    struct FreshProcess;

    impl Drop for FreshProcess {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            crate::player::reset_session_for_test();
        }
    }

    /// S12a: the window breadcrumbs `release_native_object` leaves, as a host can see them. The
    /// capture sits behind the consent check, so the test grants the crash-report answer first and
    /// puts the previous one back.
    struct Consented(Option<plx_telemetry::telemetry::consent::Consent>);

    impl Consented {
        fn grant() -> Self {
            use plx_telemetry::telemetry::consent::{self, Consent};
            let previous = consent::current();
            let mut granted = Consent::default();
            granted.errors = true;
            granted.asked_version = consent::POLICY_VERSION;
            consent::install(granted);
            // Another test's lifecycle edges may have left a cycle open on this process-wide flag.
            plx_telemetry::telemetry::window::close_cycle_for_test();
            plx_telemetry::telemetry::window::take_recorded_for_test();
            Self(previous)
        }

        /// Consent granted with a background cycle open, as the teardown and create steps are
        /// recorded only inside one (`window::record_in_cycle`). The lifecycle edge's own crumb is
        /// drained, so a test sees only the steps it runs.
        fn grant_in_cycle() -> Self {
            let granted = Self::grant();
            plx_telemetry::telemetry::window::lifecycle(0x103, true);
            plx_telemetry::telemetry::window::take_recorded_for_test();
            granted
        }
    }

    impl Drop for Consented {
        fn drop(&mut self) {
            plx_telemetry::telemetry::consent::install(self.0.take().unwrap_or_default());
            plx_telemetry::telemetry::window::close_cycle_for_test();
            plx_telemetry::telemetry::window::take_recorded_for_test();
        }
    }

    fn loaded_epoch(mt: &MainThread) -> u32 {
        let epoch = FEED_SHARED.begin_native_session().expect("native epoch");
        assert_eq!(unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert_eq!(unsafe { crate::player::test_physical::sink().ready(mt) }, 1);
        epoch
    }

    #[test]
    fn a_proven_release_leaves_unload_destroyed_and_window_destroy_in_order() {
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        assert_eq!(release_native_object_timed(&mt, epoch, &mut TeardownSpans::new()),
            plx_playback_contract::ReleaseStatus::Released);
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            ["native_unload", "native_destroyed", "exported_window_destroy"],
        );
        drop(consent);
    }

    #[test]
    fn an_unproven_release_records_quarantine_not_destroyed() {
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        // No callback ever crossed the gate: the evidence is incomplete and the object is kept.
        plx_play_feed::ffi_host::force_callback_intercepts_for_test(0);
        assert_eq!(release_native_object_timed(&mt, epoch, &mut TeardownSpans::new()),
            plx_playback_contract::ReleaseStatus::Quarantined);
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            ["native_unload", "native_quarantined", "exported_window_destroy"],
        );
        drop(consent);
    }

    #[test]
    fn a_seam_rejected_destroy_is_recorded_as_quarantine() {
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        // Rust's three proofs hold, but the C seam's own evidence does not: `destroy` answers 0,
        // and the crumb must say so rather than "destroyed".
        plx_play_feed::ffi_host::force_destroy_refusal_for_test(true);
        assert_eq!(release_native_object_timed(&mt, epoch, &mut TeardownSpans::new()),
            plx_playback_contract::ReleaseStatus::Quarantined);
        let recorded = plx_telemetry::telemetry::window::take_recorded_for_test();
        assert!(recorded.contains(&"native_quarantined"), "{recorded:?}");
        assert!(!recorded.contains(&"native_destroyed"), "{recorded:?}");
        drop(consent);
    }

    #[test]
    fn the_acb_plane_unload_is_a_step_only_when_acb_is_the_plane() {
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = loaded_epoch(&mt);
        crate::player::plane().reset_for_test();
        crate::player::plane().boot(plx_platform::tv::plane::PlaneFacts { handle: true, player_type: 10 });
        release_native_object(&mt, epoch);
        crate::player::plane().reset_for_test();
        let recorded = plx_telemetry::telemetry::window::take_recorded_for_test();
        let at = |label| recorded.iter().position(|l| *l == label);
        assert!(
            at("native_unload") < at("acb_plane_unload") && at("acb_plane_unload") < at("native_destroyed"),
            "{recorded:?}",
        );
        drop(consent);
    }

    #[test]
    fn without_a_dispatchable_object_or_an_exported_window_nothing_is_recorded() {
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(false);
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        let epoch = FEED_SHARED.begin_native_session().expect("native epoch");
        release_native_object(&mt, epoch);
        assert!(
            plx_telemetry::telemetry::window::take_recorded_for_test().is_empty(),
            "the seam's destroy_window is a no-op outside VP_EXPORTED, so it is not a step",
        );
        drop(consent);
    }

    #[test]
    fn creating_the_exported_window_is_a_step_only_in_that_mode() {
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant_in_cycle();
        let mt = unsafe { MainThread::assume() };
        plx_play_feed::ffi_host::force_clocksink_for_test(false);
        let _ = with_window_id(&mt, "{}");
        assert!(plx_telemetry::telemetry::window::take_recorded_for_test().is_empty());
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _ = with_window_id(&mt, "{}");
        assert_eq!(
            plx_telemetry::telemetry::window::take_recorded_for_test(),
            ["exported_window_create"],
        );
        drop(consent);
    }

    /// One engine's native life through the real create and release paths: what a trailer's start
    /// and stop, a seek or quality reload's teardown and an abandoned Load's release all run.
    fn engine_comes_and_goes(mt: &MainThread) {
        let _ = with_window_id(mt, "{}");
        let epoch = loaded_epoch(mt);
        release_native_object(mt, epoch);
    }

    /// S12a: **the 16-slot ring belongs to the lifecycle cycle.** The webOS 5 exported-window path
    /// costs a trailer four steps, so browsing four trailers used to push `will_foreground`,
    /// `wm_*` and `first_frame` out of the ring before the next background cycle. Teardown and
    /// window steps are crumbs of a lifecycle cycle only: this drives the REAL create and release
    /// paths through the longest cycle (webOS 5, playing, foreground reload) with trailer churn
    /// before, between and after, and checks the cycle's own steps are all the ring ever holds.
    #[test]
    fn trailer_starts_and_reloads_add_no_window_crumbs_and_a_whole_cycle_fits_the_ring() {
        use plx_telemetry::telemetry::window::{self, Observation, Stage};
        let _serial = plx_base::testlock::serial();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        crate::player::reset_session_for_test();
        let _fresh_process = FreshProcess;
        let consent = Consented::grant();
        let mt = unsafe { MainThread::assume() };
        let step = |stage| window::record(Observation::step(stage, Some(true)));
        let mut ring = Vec::new();

        for _ in 0..6 {
            engine_comes_and_goes(&mt);
        }
        assert_eq!(
            window::take_recorded_for_test(),
            Vec::<&str>::new(),
            "browsing trailers and reloading engines is not a lifecycle cycle",
        );

        // Background, with playback: the engine is released inside the cycle.
        window::lifecycle(0x103, true);
        step(Stage::WaylandReleased);
        window::lifecycle(0x104, true);
        let epoch = loaded_epoch(&mt);
        release_native_object(&mt, epoch);
        step(Stage::BackgroundDone);
        ring.extend(window::take_recorded_for_test());

        // Foreground: the window is grabbed, then the restoring Load creates its exported window.
        window::lifecycle(0x105, true);
        window::lifecycle(0x106, true);
        step(Stage::WmQuery);
        step(Stage::WmReady);
        let _ = with_window_id(&mt, "{}");
        step(Stage::ForegroundDone);
        ring.extend(window::take_recorded_for_test());

        // The app is up again: trailers come and go before and after the first resumed frame.
        engine_comes_and_goes(&mt);
        step(Stage::FirstFrame);
        step(Stage::FirstSwapComplete);
        engine_comes_and_goes(&mt);
        ring.extend(window::take_recorded_for_test());

        assert_eq!(
            ring,
            [
                "will_background", "wayland_released", "did_background", "native_unload",
                "native_destroyed", "exported_window_destroy", "background_done",
                "will_foreground", "did_foreground", "wm_query", "wm_ready",
                "exported_window_create", "foreground_done", "first_frame", "first_swap_complete",
            ],
        );
        assert!(ring.len() <= window::LIMIT, "{} steps would evict the start of the cycle", ring.len());
        drop(consent);
    }
}

#[cfg(test)]
mod payload_tests {
    use super::{build_av_payload, with_dolby_hdr_info, with_immersive, SinkEnvelope, PAYLOAD_AV, PAYLOAD_H265, PAYLOAD_V};
    use plx_data::metadata::Dovi;

    fn p5() -> Dovi {
        Dovi {
            present: true,
            profile: 5,
            bl_compat: 0,
            el_present: false,
            ..Dovi::NONE
        }
    }

    fn p8() -> Dovi {
        Dovi {
            present: true,
            profile: 8,
            bl_compat: 1,
            el_present: false,
            ..Dovi::NONE
        }
    }

    /// The full audio/video declaration matrix, as the Load payload actually composes it.
    ///
    /// The 7.1 "kill" class: the LG pipeline is told the audio codec by a single string
    /// (`"AC3"`/`"AC3 PLUS"`/`"AAC"`/`"DTS"`) with NO channel-count field, and `audioSync:true`
    /// makes the audio ES the master clock. A declaration that is (a) the wrong string for the
    /// codec, (b) carries a stale `immersive:ATMOS` node for a non-JOC track, or (c) is built for
    /// a channel layout the sink can't configure, leaves the audio ES unconfigured — and with
    /// audioSync the video sink slaves to the dead audio clock and stalls. That is the "playback
    /// dies on this title" signature, and it is content- and codec-specific, which is why it
    /// reproduced on one film and not the eight hero titles.
    ///
    /// Each cell asserts the composed payload is internally consistent for its (video, audio,
    /// immersive) triple: the codec string is the one LG's Load vocabulary accepts, the immersive
    /// node is present IFF (audio == AC3 PLUS && immersive), the envelope is well-formed, and no
    /// placeholder survived. A cell that fails here is a declaration the television would
    /// misconfigure — the exact defect the 7.1 track exposes.
    #[test]
    fn audio_video_declaration_matrix_is_well_formed() {
        // (source codec name as the route records it, the LG Load string, the expected immersive gate)
        let audio: &[(&str, &str)] = &[
            ("ac3", "AC3"),
            ("eac3", "AC3 PLUS"),
            ("aac", "AAC"),
            ("dts", "DTS"),
        ];
        let video: &[(&str, &str, plx_platform::devcaps::dv::DvCapability)] = &[
            ("h264", "H264", plx_platform::devcaps::dv::DvCapability::Unsupported),
            ("hevc", "H265", plx_platform::devcaps::dv::DvCapability::Unsupported),
        ];
        // The 1080p60 direct-play envelope — the shape a 5.1/7.1 Bluray direct play carries.
        let env = SinkEnvelope { w: 1920, h: 1080, fps: 60 };

        for &(vc, vdecl, cap) in video {
            for &(ac, adecl) in audio {
                for &immersive in &[false, true] {
                    let mut ps = crate::route::PlaybackSession::IDLE;
                    assert!(
                        crate::route::set_stream_declaration_for_test(
                            &mut ps, vc, ac, 23.976, Dovi::NONE, immersive, cap
                        ),
                        "declaration for ({vc},{ac},imm={immersive})"
                    );
                    let payload = build_av_payload(&ps, vdecl, adecl, env);

                    // 1. The codec strings LG's Load vocabulary accepts — nothing else.
                    assert!(payload.contains(&format!(r#""video":"{vdecl}""#)), "video decl ({vc})");
                    assert!(payload.contains(&format!(r#""audio":"{adecl}""#)), "audio decl ({ac})");

                    // (The @APPID@/windowId placeholders are spliced at the single call-site AFTER
                    // build_av_payload — with_window_id/with_app_id — so they are correctly still
                    // present here; the appId splice is exercised by the start-bufferfeed path, not
                    // by the payload composer.)

                    // 3. The immersive node is present IFF (AC3 PLUS && immersive).  A stale
                    //    `immersive:ATMOS` on an AAC/AC3/DTS track, or a missing one on a true
                    //    E-AC3 JOC track, is exactly the misconfiguration that wedges the audio ES.
                    let want_imm = adecl == "AC3 PLUS" && immersive;
                    let has_imm = payload.contains(r#""immersive":"ATMOS""#);
                    assert_eq!(
                        has_imm, want_imm,
                        "immersive node mismatch for audio={ac} immersive={immersive}: {payload}"
                    );

                    // 4. The envelope is the direct-play dims, not the static default.
                    assert!(payload.contains(r#""maxWidth":1920"#), "env.w ({vc},{ac})");
                    assert!(payload.contains(r#""maxHeight":1080"#), "env.h ({vc},{ac})");
                    assert!(payload.contains(r#""maxFrameRate":60"#), "env.fps ({vc},{ac})");

                    // 5. audioSync stays the master clock on every AV payload (the design).
                    assert!(payload.contains(r#""audioSync":true"#), "audioSync ({vc},{ac})");
                }
            }
        }
    }

    #[test]
    fn dv_payload_matches_frozen_route_decision() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        let payload = build_av_payload(
            &ps,
            "H265",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(payload.contains("DolbyHdrInfo"), "{payload}");
        assert!(payload.contains(r#""profileId":8"#), "{payload}");

        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Unsupported,
        ));
        let payload = build_av_payload(
            &ps,
            "H265",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    /// A declared Profile 8.1 direct play whose declaration the route withdrew (#1166) sends the Load
    /// with no `DolbyHdrInfo` node, so no Dolby Vision splitter is built and the base layer plays.
    #[test]
    fn dv_withdraw_payload_has_no_dolby_hdr_info() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        let env = SinkEnvelope { w: 3840, h: 2160, fps: 60 };
        assert!(build_av_payload(&ps, "H265", "AC3 PLUS", env).contains("DolbyHdrInfo"));
        assert!(crate::route::withdraw_dv_presentation(&mut ps, "test"));
        let payload = build_av_payload(&ps, "H265", "AC3 PLUS", env);
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    /// An AV1 Profile 10.1 direct play (HDR10-compatible base) on a set with confirmed Dolby Vision
    /// support carries the node like an HEVC Profile 8 does, with the codec string left at `"AV1"`.
    /// VP9 never does: no Dolby Vision profile rides on it.
    #[test]
    fn an_av1_profile_10_direct_play_declares_dolby_vision() {
        let p10 = Dovi {
            present: true,
            profile: 10,
            bl_compat: 1,
            el_present: false,
            ..Dovi::NONE
        };
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "av1",
            "eac3",
            23.976,
            p10,
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        assert_eq!(crate::route::stream_dovi(&ps).profile, 10);
        let payload = build_av_payload(
            &ps,
            "AV1",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(payload.contains(r#""video":"AV1""#), "{payload}");
        assert!(
            payload.contains(r#""DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":10}"#),
            "{payload}"
        );

        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "vp9",
            "eac3",
            23.976,
            p10,
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        assert!(crate::route::stream_dv_presentation(&ps).declared().is_none());
        let payload = build_av_payload(
            &ps,
            "VP9",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    #[test]
    fn late_capability_result_does_not_change_installed_decision() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "hevc",
            "eac3",
            23.976,
            p8(),
            false,
            plx_platform::devcaps::dv::DvCapability::Unknown,
        ));
        let fresh = p8().presentation(
            true,
            plx_platform::devcaps::dv::DvCapability::Supported,
            true,
        );
        assert!(fresh.declared().is_some(), "a subsequent decision sees Supported");
        assert_eq!(
            crate::route::stream_dv_presentation(&ps),
            plx_data::metadata::DvPresentation::NotDv,
        );
        let payload = build_av_payload(
            &ps,
            "H265",
            "AC3 PLUS",
            SinkEnvelope { w: 3840, h: 2160, fps: 60 },
        );
        assert!(!payload.contains("DolbyHdrInfo"), "{payload}");
    }

    #[test]
    fn refused_synthetic_p5_start_cannot_be_retried_past_validation() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let play = plx_playback_contract::playurl::PlayUrl {
            url: "http://192.0.2.1/refused-p5.mkv".into(),
            vcodec: "hevc".into(),
            acodec: "eac3".into(),
            fps: 23.976,
            dovi: plx_playback_contract::playurl::PlayDovi {
                profile: 5,
                bl_compat: 0,
                el_present: false,
            },
            atmos: false,
            auto_source_kbps: 0,
            auto_hls_base: String::new(),
            auto_start_hls: false,
            source_raster: None,
        };

        for retry in 0..2 {
            assert_eq!(
                super::install_synthetic_playurl(&mut ps, &play),
                Err(crate::route::RouteStartResult::StartFailed),
                "retry {retry} must stop before Load",
            );
            assert!(
                !crate::route::has_url(&ps),
                "retry {retry} left the rejected URL installed",
            );
        }
    }

    /// **Every Load payload must carry the `appId` placeholder, exactly once**, because that key
    /// is what `with_window_id` splices the webOS 5+ `option.windowId` onto — without it the
    /// decoded video has nowhere to bind on every set from 5.0 up — and because a second one would
    /// splice a second `windowId`.
    ///
    /// This replaces a panel row that was proposed and rejected: the "not spliced — no anchor" arm
    /// is UNREACHABLE at runtime (the placeholder is in all three constants and `build_av_payload`
    /// never touches it), so a row reporting it would print the same constant on a working and a
    /// broken television. The real risk is a payload edited a year from now that drops it — a
    /// build-time risk, caught at build time. The placeholder is a strictly better witness than the
    /// literal id it replaced: an id can be spelled correctly by accident, `@APPID@` cannot.
    #[test]
    fn every_load_payload_carries_the_app_id_placeholder_exactly_once() {
        for (name, p) in [
            ("PAYLOAD_V", PAYLOAD_V),
            ("PAYLOAD_AV", PAYLOAD_AV),
            ("PAYLOAD_H265", PAYLOAD_H265),
        ] {
            assert_eq!(
                p.matches(r#""appId":"@APPID@""#).count(),
                1,
                "{name} must carry the appId placeholder exactly once — webOS 5+ video binds on it"
            );
        }
    }

    /// **The shipped app's payload must be byte-identical to what every release so far sent**, and
    /// the splice anchor must be the composed key rather than a second spelling of it.
    ///
    /// This is the whole safety argument for making the id dynamic. The webOS 5+ `windowId` path
    /// cannot be exercised on this project's 4.5 dev set (`vp_mode()` returns `VP_ACB` there and
    /// `with_window_id` returns early), so its failure — a black video plane with working audio,
    /// and no error line — would not be seen until somebody else's television. Pinning the
    /// composed bytes here is the only gate available for it.
    #[test]
    fn the_shipped_app_composes_the_payload_it_always_did() {
        let want = r#""appId":"com.beb.plxnative""#;
        for (name, p) in [
            ("PAYLOAD_V", PAYLOAD_V),
            ("PAYLOAD_AV", PAYLOAD_AV),
            ("PAYLOAD_H265", PAYLOAD_H265),
        ] {
            let composed = p.replace("@APPID@", plx_base::paths::STABLE_APP_ID);
            assert!(
                composed.contains(want),
                "{name} no longer composes the shipped key"
            );
            // key ORDER too: `appId` stays the first key of `option`, where it has always been.
            assert!(
                composed.contains(&format!(r#""option":{{{want},"#)),
                "{name} moved the appId key"
            );
        }
        // And the anchor the splice looks for is the composed key, not a restatement of it.
        assert_eq!(
            super::app_id_key(),
            format!(r#""appId":"{}""#, plx_base::paths::app_id())
        );
        assert!(super::with_app_id(PAYLOAD_V).contains(&super::app_id_key()));
        assert!(!super::with_app_id(PAYLOAD_V).contains("@APPID@"));
    }

    /// The Dolby Vision node's anchor, with the same reasoning as the one above: `provider` is the
    /// LAST key of `contents`, which is where `DolbyHdrInfo` has to land — the pipeline reads it at
    /// `option.externalStreamingInfo.contents.DolbyHdrInfo` and nowhere else. Exactly once, or a
    /// `replace` would splice two nodes into one payload.
    #[test]
    fn the_av_payload_carries_exactly_one_dolby_hdr_info_anchor() {
        assert_eq!(PAYLOAD_AV.matches(r#""provider":"plxnative""#).count(), 1);
        // and it really is the last key of `contents` — the next character after it closes the
        // object, so appending a key there stays INSIDE `contents`
        assert!(PAYLOAD_AV.contains(r#""provider":"plxnative"},"streamQualityInfo""#));
    }

    /// **What we actually send for a Profile 5 direct play.** The three fields at the path the
    /// television's own parser reads, `profileId` as a bare JSON integer (`getInt` — a quoted "5"
    /// would leave the pipeline's -1 sentinel), and the whole node inside `contents`.
    #[test]
    fn a_declared_profile_5_splices_the_node_into_contents() {
        // what `build_av_payload` hands it: the AV template with the codec already set to H265,
        // which is what a native HEVC direct play — the only kind that can be Dolby Vision — sends
        let base = PAYLOAD_AV.replace(r#""video":"H264""#, r#""video":"H265""#);
        let out = with_dolby_hdr_info(&base, "H265", p5().presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true));
        assert!(
            out.contains(r#""provider":"plxnative","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}}"#),
            "{out}"
        );
        // the trailing `}` above is `contents` closing: the node is the last key INSIDE it, not a
        // sibling of `contents` in `externalStreamingInfo`
        assert!(out.contains(r#""DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}},"streamQualityInfo""#));
        assert!(
            !out.contains(r#""profileId":"5""#),
            "getInt wants an integer, not a string"
        );
        // the codec string stays `H265` — `getVideoCaps` maps it to `video/x-h265` and falls
        // THROUGH into its Dolby Vision tail; there is no DVHE/DVH1 entry in that table, and
        // inventing one would describe a stream the pipeline has no decoder row for
        assert!(out.contains(r#""video":"H265""#), "{out}");
        assert!(!out.contains("DVHE") && !out.contains("dvh1"), "{out}");
        // and NOTHING else moved: take the node back out and the payload is what came in
        const NODE: &str =
            r#","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}"#;
        assert_eq!(out.replace(NODE, ""), base);
    }

    /// The three ways the node is NOT sent, each of which must leave the payload byte-identical:
    /// a file with no Dolby Vision, a Dolby Vision file we refuse (a dual layer without a
    /// cross-compatible base layer — declaring a layer we cannot feed is worse than refusing it),
    /// and the disarmed trigger, which is what a `RELEASE=1` build compiles in and what every boot
    /// without `/tmp/plxnative-dv` does today.
    #[test]
    fn nothing_is_spliced_unless_the_stream_is_declared() {
        let unconvertible_p7 = Dovi {
            present: true,
            profile: 7,
            bl_compat: 0,
            el_present: true,
            ..Dovi::NONE
        };
        for dv in [
            Dovi::NONE.presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true),
            unconvertible_p7.presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true),
            p5().presentation(false, plx_platform::devcaps::dv::DvCapability::Supported, true),
        ] {
            assert_eq!(with_dolby_hdr_info(PAYLOAD_AV, "H265", dv), PAYLOAD_AV);
        }
    }

    /// A Profile 7 with a cross-compatible base layer is fed as the 8.1 `ff::packet_to_annexb`
    /// makes of it, and the node says so: `profileId` 8, one track — never 7, never `"dual"`.
    #[test]
    fn a_converted_profile_7_declares_profile_8_single() {
        let p7 = Dovi {
            present: true,
            profile: 7,
            bl_compat: 6,
            el_present: true,
            ..Dovi::NONE
        };
        let out = with_dolby_hdr_info(
            PAYLOAD_AV,
            "H265",
            p7.presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true),
        );
        const NODE: &str =
            r#","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":8}"#;
        assert!(out.contains(NODE), "{out}");
        assert_eq!(out.replace(NODE, ""), PAYLOAD_AV);
    }

    /// **What we actually send for a Dolby Atmos track**, at the key path `libpf` reads
    /// (`option.externalStreamingInfo.contents.immersive`) and with the value `libcbe` — the
    /// television's own working client — puts there. Same anchor and same shape as the Dolby
    /// Vision node, which is the point: they are one envelope with two statements in it.
    #[test]
    fn an_atmos_track_splices_immersive_into_contents() {
        let out = with_immersive(PAYLOAD_AV, "AC3 PLUS", true);
        assert!(
            out.contains(r#""provider":"plxnative","immersive":"ATMOS"}"#),
            "{out}"
        );
        // the trailing `}` is `contents` closing — the node is INSIDE it, not a sibling of
        // `contents` in `externalStreamingInfo`, which is where libpf would never look
        assert!(
            out.contains(r#""immersive":"ATMOS"},"streamQualityInfo""#),
            "{out}"
        );
        // and nothing else moved
        assert_eq!(out.replace(r#","immersive":"ATMOS""#, ""), PAYLOAD_AV);
    }

    /// The other side of it, and the one that matters for the whole library: a track with no
    /// Atmos leaves the payload byte-identical. Every ordinary AAC/AC3 film takes this path, so a
    /// splice that fired unconditionally would tell the television that all of them are immersive.
    #[test]
    fn a_plain_track_splices_nothing() {
        assert_eq!(with_immersive(PAYLOAD_AV, "AC3 PLUS", false), PAYLOAD_AV);
    }

    /// A source Atmos flag that accidentally survives a full transcode must still be rejected at
    /// the final payload seam.  HLS carries AAC and ordinary AC3 carries no E-AC3 JOC extension;
    /// telling the system player either is immersive is a false stream declaration.
    #[test]
    fn an_atmos_declaration_never_rides_a_non_eac3_payload() {
        for audio in ["AAC", "AC3"] {
            assert_eq!(
                with_immersive(PAYLOAD_AV, audio, true),
                PAYLOAD_AV,
                "{audio}"
            );
        }
    }

    /// **Both nodes at once**, which is the real case — the Profile 5 test item is Dolby Vision
    /// AND Dolby Atmos, and it is what the television shows two read-outs for. They are spliced by
    /// two independent functions at ONE anchor, so this is the test that says the second does not
    /// land inside the first: `immersive` must be a sibling KEY of `DolbyHdrInfo` inside
    /// `contents`, never a fourth field of the `DolbyHdrInfo` object.
    #[test]
    fn dolby_vision_and_atmos_are_siblings_inside_contents() {
        let base = PAYLOAD_AV.replace(r#""video":"H264""#, r#""video":"H265""#);
        let out = with_immersive(
            &with_dolby_hdr_info(&base, "H265", p5().presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true)),
            "AC3 PLUS",
            true,
        );
        assert!(
            out.contains(
                r#""provider":"plxnative","immersive":"ATMOS","DolbyHdrInfo":{"trackType":"single","encryptionType":"clear","profileId":5}}"#
            ),
            "{out}"
        );
        // the DolbyHdrInfo object still has exactly its three fields — `immersive` did not get
        // swept inside it by an anchor both functions matched
        assert!(!out.contains(r#""profileId":5,"immersive""#), "{out}");
        assert_eq!(out.matches(r#""immersive":"ATMOS""#).count(), 1);
        assert_eq!(out.matches("DolbyHdrInfo").count(), 1);
    }

    /// The consistency guard: a Dolby Vision declaration only ever rides an HEVC elementary
    /// stream, so a payload built for H264 must not carry one. Unreachable by construction — the
    /// route records the DV record on the direct-play branch, whose codec is the file's own hevc —
    /// which is exactly why it is asserted rather than trusted: the `sourceInfo` envelope is
    /// parsed before anything decodes, and a malformed one wedges the sink instead of failing.
    #[test]
    fn a_declaration_never_rides_a_non_hevc_payload() {
        assert_eq!(
            with_dolby_hdr_info(PAYLOAD_AV, "H264", p5().presentation(true, plx_platform::devcaps::dv::DvCapability::Supported, true)),
            PAYLOAD_AV
        );
    }
}

/// A cropped diagnostic line cannot report what it never wrote down. `op_audio_enhancement_burn`
/// in `tests/run.py` proves a Burn actually happened by grepping the app's own
/// `stream: <origin> path=...` line for `subtitleStreamID=` and `subtitles=burn` — the only
/// on-device evidence of the query PMS actually received. Device runs of
/// `audio_enhancement_burns_embedded_subtitle` (rk 1804, 2026-09) show the grader failing even
/// though [`crate::route::decision`]'s own tests prove the Burn route is chosen correctly: the
/// line was cut to 80 characters, which lands inside `mediaIndex=`/`partIndex=`/`protocol=...`
/// for a realistic transcode URL, well before either marker.
#[cfg(test)]
mod stream_open_log_line_tests {
    use super::stream_open_log_line;

    /// A transcode URL shaped like the one PMS returns for this project's Burn route: video
    /// re-encode params, then `audioStreamID`, then `boostDialog`/`normalizeLoudness`, then
    /// `subtitleStreamID`/`subtitles=burn` last — matching `transcoder::transcode_query`'s actual
    /// build order — followed by the `X-Plex-Token` `Client::with_token` always appends last.
    fn burn_path() -> String {
        "/video/:/transcode/universal/start.mkv?path=%2Flibrary%2Fmetadata%2F1804&mediaIndex=0&\
         partIndex=0&protocol=http&directPlay=0&directStream=0&videoResolution=3840x2160&\
         maxVideoBitrate=40000&videoCodec=hevc&audioCodec=eac3&audioStreamID=10976&\
         boostDialog=0&normalizeLoudness=1&subtitleStreamID=10980&subtitleSize=100&\
         subtitles=burn"
            .to_string()
    }

    #[test]
    fn logged_line_carries_the_burn_evidence_the_grader_looks_for() {
        let path = burn_path();
        let line = stream_open_log_line("plex.direct:32400", &path);
        assert!(
            line.contains("subtitleStreamID=10980"),
            "log line dropped subtitleStreamID before the grader could see it: {line}"
        );
        assert!(
            line.contains("subtitles=burn"),
            "log line dropped subtitles=burn before the grader could see it: {line}"
        );
    }

    /// The un-truncated path still passes through `plx_base::eventlog::log`'s `scrub_local` pass, which is
    /// the ONLY place a token may be redacted — so leaving the full path in is safe precisely
    /// because `Client::with_token` always appends `X-Plex-Token=` last and `scrub_local` already
    /// redacts it by value. This is the end-to-end check that removing the truncation did not
    /// also remove the credential backstop.
    #[test]
    fn full_path_still_has_its_token_redacted_by_the_existing_scrub_pass() {
        let path = format!("{}&X-Plex-Token=aBcD1234xyzQ", burn_path());
        let line = stream_open_log_line("plex.direct:32400", &path);
        let scrubbed = plx_base::eventlog::scrub::scrub_local(&line);
        assert!(
            !scrubbed.contains("aBcD1234xyzQ"),
            "token leaked into the event log: {scrubbed}"
        );
        assert!(
            scrubbed.contains("subtitleStreamID=10980") && scrubbed.contains("subtitles=burn"),
            "scrub pass ate the burn evidence along with the token: {scrubbed}"
        );
    }
}

/// **The 94-second freeze of 2026-08-29, reduced to the mechanism that caused it.**
///
/// A viewer on a remote 4K Dolby Vision stream was downshifted into HLS, and the picture then did
/// not start for 94 seconds. The event log shows the reason plainly once you know where to look:
/// `primed: v=1999ms a=1962ms -> Play` was eventually printed with EXACTLY the fed extents of the
/// segment that had landed 94 seconds earlier — so the condition Play waits on had been satisfied
/// the whole time and was simply never EVALUATED.
///
/// The evaluation lived in one place only: inside [`feed_stream`]'s per-AU loop, after each
/// `sf_feed` — accepted or not, which was a second bug in the same lines. A pump tick runs the video lane and then the audio lane
/// ([`super::pump::pump`]), so when a whole HLS segment lands at once the video lane drains all of
/// it with `abuf` still at zero — `vbuf` tops out below [`PRIME_VIDEO_MAX_NS`] and the
/// `abuf >= PRIME_AUDIO_NS` arm cannot be true yet. The audio lane then fills, but it deliberately
/// carries no prime check. On every later tick `aq_pop` returns null and the loop breaks BEFORE
/// the check is reached. Play is now waiting for a video AU that only the next segment can supply.
///
/// That is why direct play is unaffected and HLS is not: direct play trickles AUs in over many
/// ticks, so audio keeps pace and the check passes on an ordinary tick. A segment arriving whole
/// is the shape that breaks it — and since [`PRIME_VIDEO_MAX_NS`] is 2500 ms, a 2-second segment
/// can never take the video-only escape, which makes this structural for every 2 s-segment reload
/// rather than a rare race.
///
/// **This test drives the real [`feed_stream`] and [`feed_audio_lane`] against the real host feed
/// seam** — it is not a restatement of the predicate, which is correct and would pass on the
/// broken build. What it grades is WHERE the predicate is evaluated.
#[cfg(all(test, feature = "hostsim"))]
mod prime_livelock_tests {
    use plx_play_feed::shared::{FEED_SHARED, FEED_TX};
    use super::*;

    /// One 2-second HLS segment, as the device delivered it: 48 video AUs at 24 fps and 93 AAC
    /// frames. These are the counts the failing run's `hls: segment=458 … v=48 a=93` line records.
    const VIDEO_AUS: i64 = 48;
    const AUDIO_AUS: i64 = 93;
    const V_STEP_NS: i64 = 41_666_666; // 24 fps
    const A_STEP_NS: i64 = 21_333_333; // 1024 samples at 48 kHz

    /// An engine in the state a fresh `Load` leaves behind: streaming, nothing fed yet, and
    /// `prime_play` armed so the next sufficient buffer starts the clock.
    pub(super) fn engine_after_reload() -> Engine {
        FEED_SHARED.ensure_initial_clock_hold_for_test();
        Engine {
            shared: &FEED_SHARED,
            attempt: crate::route::RouteStartAttempt::fixture().contract_id(),
            native_epoch: 0,
            stage: Stage::Streaming,
            video_info_sent: true,
            bound_at_frames: None,
            placed_src: (0, 0),
            eos_pushed: false,
            rebase_pending: false,
            rebase_drops: 0,
            seek_armed_at: 0,
            seek_retries: 0,
            segment_landing: None,
                flushed: false,
            presentation_rearm_pending: false,
            max_fed_video_pts: 0,
            max_fed_audio_pts: 0,
            audio_start_fp: None,
            pcm_sink_lead_ns: 0,
            pcm_lead_pending: false,
            feed_pace: FeedPace::default(),
            seek_base_pts: 0,
            prime_play: true,
            startup_controlled: false,
            startup_user_held: false,
            startup_play_commanded: false,
            startup_first_play_done: false,
            startup_play_refused: false,
            startup_immediate_play: false,
            aq_video: Some(plx_play_feed::aq::aq_new(AQ_VIDEO_BYTES)),
            aq_audio: Some(plx_play_feed::aq::aq_new(AQ_AUDIO_BYTES)),
            hs: plx_net::stream::http_stream_boxed(),
            pending_video: None,
            pending_audio: None,
            payload: std::ffi::CString::new("").unwrap(),
            source: Source::Stream,
            stream_th: None,
            load_th: None,
            side_reader: Box::new(plx_play_feed::side_reader::SideReaderOpen::new(
                Default::default(), super::super::subside::JOB)),
            preview_abandon: false,
            era_watch: plx_play_feed::era_callbacks::Watch::new(),
            dv_start: plx_play_feed::dv_start::Guard::inert(),
        }
    }

    /// Push one whole segment into both lanes before any tick runs — which is what a segmented
    /// demux does, and the entire difference from direct play.
    fn push_one_segment(eng: &mut Engine) {
        push_segment_at(eng, 0);
    }

    fn push_segment_at(eng: &mut Engine, base_ns: i64) {
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..VIDEO_AUS {
            plx_play_feed::aq::aq_push(
                qv,
                au.as_ptr(),
                au.len() as c_int,
                base_ns + i * V_STEP_NS,
                c_int::from(i == 0),
                1,
            );
        }
        let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..AUDIO_AUS {
            plx_play_feed::aq::aq_push(
                qa,
                au.as_ptr(),
                au.len() as c_int,
                base_ns + i * A_STEP_NS,
                1,
                2,
            );
        }
    }

    /// One pump tick — the real one. [`super::pump::pump`] calls exactly this.
    ///
    /// **This comment used to claim the test therefore "cannot drift from the shipped ordering",
    /// and that was false**: the test calls the wrapper, so a `pump` reverted to two separate lane
    /// calls would have restored the bug with this test still green. What actually enforces it is
    /// that [`feed_stream`] and [`feed_audio_lane`] are private — the revert no longer compiles.
    /// The test pins the BEHAVIOUR; the visibility pins the dispatch.
    fn tick(mt: &plx_base::task::MainThread, eng: &mut Engine) {
        feed_both_lanes(mt, eng);
    }

    #[test]
    fn a_segment_that_lands_whole_still_starts_the_picture() {
        let _serial = plx_base::testlock::serial();
        // Arm the host clock sink so `sf_feed` accepts AUs. See `ffi_host::FORCE_ENABLED` for why
        // this is an explicit override and not the `plxnative-clocksink` trigger file. RESTORED on
        // the way out, panic or not: it is a process-global, and leaving it armed would silently
        // change what every later test's `sf_feed` returns.
        struct DisarmSink;
        impl Drop for DisarmSink {
            fn drop(&mut self) {
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _disarm = DisarmSink;

        let mt = unsafe { plx_base::task::MainThread::assume() };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);

        // Tick 1: the segment is there in full. Tick 2: the next segment has not arrived, so both
        // queues are empty — the state the device sat in for 94 seconds.
        tick(&mt, &mut eng);
        tick(&mt, &mut eng);

        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;

        // **Precondition, asserted rather than assumed.** If the host feed seam refused the AUs
        // then nothing was fed, and a `prime_play` that is still armed would mean "the sink was
        // not armed", not "the bug". Distinguishing those is the whole lesson of this project's
        // silent-instrument traps, so it gets its own message.
        assert!(
            vbuf > 0 && abuf > 0,
            "PRECONDITION FAILED, not the defect: the host feed seam accepted nothing \
             (v={vbuf}ns a={abuf}ns). The clock sink is not armed, so this test graded nothing."
        );
        assert!(
            vbuf >= PRIME_NS,
            "video lane should hold a whole segment, got {vbuf}ns"
        );
        assert!(
            abuf >= PRIME_AUDIO_NS,
            "audio lane should hold a whole segment, got {abuf}ns"
        );

        assert!(
            !eng.prime_play,
            "the picture never started: both lanes hold enough to play \
             (v={}ms >= {}ms, a={}ms >= {}ms) but Play was never called. \
             The prime check is only reachable from inside feed_stream's per-AU loop, and by the \
             time the audio lane has caught up the video queue is empty, so the check is never \
             evaluated again. This is the 94-second freeze.",
            vbuf / 1_000_000,
            PRIME_NS / 1_000_000,
            abuf / 1_000_000,
            PRIME_AUDIO_NS / 1_000_000,
        );
    }

    /// Arm the host clock sink and a per-read feed-clock cost; restored on drop (both are
    /// process-globals).
    struct SlowFeed;
    impl SlowFeed {
        fn arm(step_us: u64) -> Self {
            plx_play_feed::ffi_host::force_clocksink_for_test(true);
            crate::player::test_physical::FEED_TEST_CLOCK_STEP_US.with(|s| s.set(step_us));
            SlowFeed
        }
    }
    impl Drop for SlowFeed {
        fn drop(&mut self) {
            crate::player::test_physical::FEED_TEST_CLOCK_STEP_US.with(|s| s.set(0));
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
        }
    }

    /// Ticks a freshly reloaded engine takes to start the clock on one whole segment.
    fn ticks_to_prime(step_us: u64) -> (u32, Engine) {
        let _slow = SlowFeed::arm(step_us);
        let mt = unsafe { plx_base::task::MainThread::assume() };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        let mut ticks = 0;
        while eng.prime_play && ticks < 100 {
            tick(&mt, &mut eng);
            ticks += 1;
        }
        (ticks, eng)
    }

    /// **A slow `Feed()` does not stretch the prime.** With each feed-clock read costing 4 ms
    /// (past [`FEED_LANE_SLICE_US`]) a sliced lane feeds one AU a tick, and the prime's 17 video
    /// and ~15 audio AUs took that many ticks to first frame. A lane short of its prime depth is
    /// exempt, so the prime takes the same ticks as a greedy feed, and still on BOTH lanes.
    #[test]
    fn a_slow_feed_does_not_stretch_the_prime() {
        let _serial = plx_base::testlock::serial();
        let (greedy, _) = ticks_to_prime(0);
        let (slow, eng) = ticks_to_prime(4_000);
        assert!(!eng.prime_play, "the prime never completed in {slow} ticks");
        assert!(
            slow <= greedy + 1,
            "a 4 ms Feed() took {slow} ticks to prime; greedy feeding takes {greedy}"
        );
        assert!(
            eng.max_fed_video_pts - eng.seek_base_pts >= PRIME_NS
                && eng.max_fed_audio_pts - eng.seek_base_pts >= PRIME_AUDIO_NS,
            "Play started without both lanes primed"
        );
    }

    /// **Once a lane holds its prime depth, the rest of the backlog is fed a slice per lane per
    /// tick, not all in one frame.** On the TV every `Feed()` blocks the frame thread, and the
    /// first tick of a playback fed 62 video and 120 audio AUs at once (an 89 ms frame). Each
    /// feed-clock read costs 1 ms here, so a lane above low-water may feed only up to
    /// [`FEED_LANE_SLICE_US`] worth per tick: the first tick primes (exempt, to the prime depth)
    /// and the clock starts on both lanes, then the leftover backlog drains a slice a tick.
    #[test]
    fn a_post_prime_backlog_is_fed_a_slice_per_lane_per_tick() {
        let _serial = plx_base::testlock::serial();
        let _slow = SlowFeed::arm(1_000);
        let mt = unsafe { plx_base::task::MainThread::assume() };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        let per_lane_max = (FEED_LANE_SLICE_US / 1_000) as i64 + 1;

        tick(&mt, &mut eng);
        assert!(!eng.prime_play, "the first tick primes: both lanes reach their depth");
        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
        assert!(
            vbuf >= PRIME_NS && abuf >= PRIME_AUDIO_NS,
            "Play started without both lanes primed (v={}ms a={}ms)",
            vbuf / 1_000_000,
            abuf / 1_000_000,
        );
        let v_after_prime = (vbuf + V_STEP_NS - 1) / V_STEP_NS;
        assert!(
            v_after_prime <= PRIME_NS / V_STEP_NS + per_lane_max,
            "the video lane fed {v_after_prime} AUs: the slice must apply past the prime depth"
        );

        // The presented position sits at the base, so the lead (>= the prime depth) is healthy.
        FEED_SHARED.pres_fed.store(0, Ordering::Relaxed);
        let (v0, a0) = (eng.max_fed_video_pts, eng.max_fed_audio_pts);
        tick(&mt, &mut eng);
        let v_fed = (eng.max_fed_video_pts - v0) / V_STEP_NS;
        let a_fed = (eng.max_fed_audio_pts - a0) / A_STEP_NS;
        assert!(
            eng.max_fed_video_pts > v0 && eng.max_fed_audio_pts > a0,
            "both lanes must keep draining the backlog"
        );
        assert!(
            v_fed <= per_lane_max && a_fed <= per_lane_max,
            "one tick fed {v_fed} video and {a_fed} audio AUs; the slice allows {per_lane_max} per lane",
        );
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
    }

    /// **A playing lane below low-water is fed past its slice**: at 4 ms a `Feed()` the slice
    /// alone would feed one AU a tick, which at a 50 Hz present rate cannot keep 60 fps content's
    /// lead. Below [`FEED_LOW_WATER_NS`] the lane is topped up in one tick; above it, the slice.
    #[test]
    fn a_playing_lane_below_low_water_is_fed_past_its_slice() {
        let _serial = plx_base::testlock::serial();
        let _slow = SlowFeed::arm(4_000);
        let mt = unsafe { plx_base::task::MainThread::assume() };
        const PRES: i64 = 10_000_000_000;
        FEED_SHARED.pres_fed.store(PRES, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mut eng = engine_after_reload();
        eng.prime_play = false;
        eng.seek_base_pts = 0;
        let lead0 = 100_000_000;
        eng.max_fed_video_pts = PRES + lead0;
        eng.max_fed_audio_pts = PRES + lead0;
        // This fixture holds the presented position fixed. Keep FeedPace's existing anchor
        // ahead of the real clock so host preemption cannot extrapolate that position while
        // the simulated feed clock is testing the slice. FeedPace's run-on has its own tests.
        assert_eq!(eng.feed_pace.reference(PRES, u32::MAX), PRES);
        push_segment_at(&mut eng, PRES + lead0 + 1);

        tick(&mt, &mut eng);
        let v_fed = (eng.max_fed_video_pts - (PRES + lead0)) / V_STEP_NS;
        let want = (FEED_LOW_WATER_NS - lead0 + V_STEP_NS - 1) / V_STEP_NS;
        assert_eq!(v_fed, want, "the video lane is topped up to low-water in one tick, not one AU");
        assert!(eng.max_fed_video_pts - PRES >= FEED_LOW_WATER_NS);

        // Above low-water the slice is back in force: one AU a tick at 4 ms a Feed().
        let v0 = eng.max_fed_video_pts;
        tick(&mt, &mut eng);
        assert_eq!((eng.max_fed_video_pts - v0) / V_STEP_NS, 1, "a healthy lane is sliced");
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
    }

    /// Starting with less playable media than the next observed acquisition costs recreates the
    /// freeze/catch-up loop: the clock empties the segment before its replacement can be credited.
    /// The demuxer's runway is a measured duration, so it must raise the decoder-only prime floor.
    #[test]
    fn an_hls_clock_waits_for_the_observed_acquisition_runway() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let old = FEED_SHARED.hls_prime_runway_ms.swap(3_000, Ordering::Relaxed);
        let _restore = Restore { runway: old };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);

        let vbuf = eng.max_fed_video_pts - eng.seek_base_pts;
        let abuf = eng.max_fed_audio_pts - eng.seek_base_pts;
        assert!(
            vbuf > PRIME_NS && abuf > PRIME_AUDIO_NS,
            "the decoder prime is satisfied"
        );
        assert!(
            vbuf < 3_000_000_000 && abuf < 3_000_000_000,
            "the measured runway is not"
        );
        assert!(
            eng.prime_play,
            "Play started on decoder latency alone even though the next acquisition costs 3000ms",
        );
    }

    #[test]
    fn automatic_rebuffer_cannot_resume_without_fresh_media() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            rebuffering: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                FEED_SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                FEED_SHARED.finish_hls_recovery();
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let restore = Restore {
            runway: FEED_SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            rebuffering: FEED_SHARED.hls_rebuffering.swap(false, Ordering::Relaxed),
        };
        let _restore = restore;
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "ordinary initial prime should have started"
        );

        eng.prime_play = true;
        FEED_SHARED.begin_hls_recovery();
        FEED_SHARED.hls_rebuffering.store(true, Ordering::Relaxed);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "the old high-water is not newly buffered media"
        );

        FEED_SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));
        push_segment_at(&mut eng, 2_000_000_000);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "a fresh segment covering the measured runway may resume"
        );
        assert!(!FEED_SHARED.hls_rebuffering.load(Ordering::Relaxed));
    }

    /// A candidate transaction owns the discretionary reserve its deadline is currently spending.
    /// Starting an already-held native clock in the middle would spend the same balance at the
    /// playhead too. The transaction must reach commit/reject before already-buffered media may
    /// release an initial or internal prime; a running clock is never stopped by this marker.
    #[test]
    fn an_inflight_hls_trial_cannot_release_the_prime_hold() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            trial_reserve: i64,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                FEED_SHARED
                    .hls_trial_reserve_ms
                    .store(self.trial_reserve, Ordering::Relaxed);
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }

        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _restore = Restore {
            runway: FEED_SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            trial_reserve: FEED_SHARED.hls_trial_reserve_ms.swap(500, Ordering::Relaxed),
        };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "Play started while the candidate still owned its measured reserve",
        );

        FEED_SHARED.hls_trial_reserve_ms.store(-1, Ordering::Relaxed);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "the completed transaction did not release an otherwise-ready prime",
        );
    }

    #[test]
    fn a_complete_candidate_cycle_invalidates_an_even_prime_snapshot() {
        assert!(hls_candidate_snapshot_stable(40, 40));
        assert!(
            !hls_candidate_snapshot_stable(40, 42),
            "an even->odd->next-even cycle is not the same stable snapshot",
        );
        assert!(!hls_candidate_snapshot_stable(41, 41));
    }

    /// A recovery downshift has no discretionary up-trial reserve. Its first queued candidate
    /// AUs can nevertheless make the shared tails satisfy the OLD rung's recovery certificate
    /// before controller and route ownership commit. The structural publication fence must hold
    /// Play across that window, and a successful transition must begin a fresh actuator epoch
    /// rather than releasing from the mixed snapshot.
    #[test]
    fn a_downshift_candidate_cannot_release_an_old_recovery_epoch_mid_commit() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            trial_reserve: i64,
            settled_generation: u64,
            rebuffering: bool,
            video_tail: i64,
            audio_tail: i64,
            audio_expected: bool,
            pts_shift: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                FEED_SHARED
                    .hls_trial_reserve_ms
                    .store(self.trial_reserve, Ordering::Relaxed);
                FEED_SHARED
                    .hls_candidate_generation
                    .store(self.settled_generation, Ordering::Release);
                FEED_SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                FEED_SHARED
                    .hls_video_tail_ns
                    .store(self.video_tail, Ordering::Relaxed);
                FEED_SHARED
                    .hls_audio_tail_ns
                    .store(self.audio_tail, Ordering::Relaxed);
                FEED_SHARED
                    .hls_audio_expected
                    .store(self.audio_expected, Ordering::Relaxed);
                FEED_SHARED.pts_shift.store(self.pts_shift, Ordering::Relaxed);
                FEED_TX.commit_paused(self.paused);
                FEED_SHARED.finish_hls_recovery();
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }

        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let old_paused = FEED_TX.paused.load(Ordering::Acquire);
        FEED_TX.commit_paused(false);
        let stable_generation = FEED_SHARED.hls_candidate_generation.load(Ordering::Acquire);
        assert_eq!(stable_generation & 1, 0, "test starts outside a transition");
        let active_generation = stable_generation.wrapping_add(1);
        let settled_generation = active_generation.wrapping_add(1);
        FEED_SHARED
            .hls_candidate_generation
            .compare_exchange(
                stable_generation,
                active_generation,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .expect("test arms one candidate generation");
        let _restore = Restore {
            runway: FEED_SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            trial_reserve: FEED_SHARED.hls_trial_reserve_ms.swap(-1, Ordering::Relaxed),
            settled_generation,
            rebuffering: FEED_SHARED.hls_rebuffering.swap(true, Ordering::Relaxed),
            video_tail: FEED_SHARED
                .hls_video_tail_ns
                .swap(2_000_000_000, Ordering::Relaxed),
            audio_tail: FEED_SHARED
                .hls_audio_tail_ns
                .swap(2_000_000_000, Ordering::Relaxed),
            audio_expected: FEED_SHARED.hls_audio_expected.swap(true, Ordering::Relaxed),
            pts_shift: FEED_SHARED.pts_shift.swap(0, Ordering::Relaxed),
            paused: old_paused,
        };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        FEED_SHARED.begin_hls_recovery();
        FEED_SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "candidate tails released Play before their ownership transition committed",
        );

        // The successful commit publishes a fresh new-rung epoch before the release store.
        FEED_SHARED.begin_hls_recovery();
        FEED_SHARED
            .hls_candidate_generation
            .store(settled_generation, Ordering::Release);
        tick(&mt, &mut eng);
        assert!(
            eng.prime_play,
            "clearing the fence reused the old actuator's completed recovery sample",
        );

        FEED_SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));
        FEED_SHARED
            .hls_video_tail_ns
            .store(4_000_000_000, Ordering::Release);
        FEED_SHARED
            .hls_audio_tail_ns
            .store(4_000_000_000, Ordering::Release);
        push_segment_at(&mut eng, 2_000_000_000);
        tick(&mt, &mut eng);
        assert!(
            !eng.prime_play,
            "one complete active-rung acquisition should now release the fresh exact epoch",
        );
    }

    /// Paused Starfish source buffers are finite. Media already demuxed into our AU queues is
    /// nonetheless guaranteed playable, so a pause-local recovery certificate may span both
    /// layers once the native decoder itself has its ordinary prime. Otherwise a full native
    /// buffer plus a full application queue can never release the hold.
    #[test]
    fn internal_rebuffer_certificate_counts_the_whole_playable_pipeline() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            rebuffering: bool,
            video_tail: i64,
            audio_tail: i64,
            audio_expected: bool,
            pts_shift: i64,
            seeking: bool,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                FEED_SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                FEED_SHARED
                    .hls_video_tail_ns
                    .store(self.video_tail, Ordering::Relaxed);
                FEED_SHARED
                    .hls_audio_tail_ns
                    .store(self.audio_tail, Ordering::Relaxed);
                FEED_SHARED
                    .hls_audio_expected
                    .store(self.audio_expected, Ordering::Relaxed);
                FEED_SHARED.pts_shift.store(self.pts_shift, Ordering::Relaxed);
                FEED_SHARED.seeking.store(self.seeking, Ordering::Relaxed);
                FEED_TX.paused.store(self.paused, Ordering::Relaxed);
                FEED_SHARED.finish_hls_recovery();
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _restore = Restore {
            runway: FEED_SHARED.hls_prime_runway_ms.swap(3_000, Ordering::Relaxed),
            rebuffering: FEED_SHARED.hls_rebuffering.swap(true, Ordering::Relaxed),
            video_tail: FEED_SHARED
                .hls_video_tail_ns
                .swap(3_500_000_000, Ordering::Relaxed),
            audio_tail: FEED_SHARED
                .hls_audio_tail_ns
                .swap(3_500_000_000, Ordering::Relaxed),
            audio_expected: FEED_SHARED.hls_audio_expected.swap(true, Ordering::Relaxed),
            pts_shift: FEED_SHARED.pts_shift.swap(0, Ordering::Relaxed),
            seeking: FEED_SHARED.seeking.swap(false, Ordering::Relaxed),
            paused: FEED_TX.paused.swap(false, Ordering::Relaxed),
        };

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        eng.max_fed_video_pts = PRIME_NS;
        eng.max_fed_audio_pts = PRIME_AUDIO_NS;
        FEED_SHARED.begin_hls_recovery();
        FEED_SHARED.observe_hls_recovery(3_000_000, std::time::Duration::from_secs(3));
        try_prime(&mt, &mut eng);

        assert!(
            !eng.prime_play,
            "three seconds already demuxed behind a decoder-prime must release a 3s runway hold",
        );
        assert!(!FEED_SHARED.hls_rebuffering.load(Ordering::Relaxed));
    }

    /// Reproduces the device sequence: Pause, let the demux queues fill, then Resume. The user
    /// intent may reopen feeding, but it must not run the native/ACB clock until Starfish has
    /// accepted a balanced decoder prime. Historical stress runway is not a runtime Resume floor.
    #[test]
    fn pause_to_fill_resume_primes_both_lanes_before_native_play() {
        assert_viewer_resume_primes(3_000, 3_500_000_000);
    }

    /// Issue #1216: 14.754 s of historical runway, but the paused native pipeline and
    /// validated queues can hold only 10.8 s. Neither tail grows until Play.
    #[test]
    fn a_resume_prime_whose_runway_exceeds_the_queue_capacity_still_plays() {
        assert_viewer_resume_primes(14_754, 10_800_000_000);
    }

    fn assert_viewer_resume_primes(runway_ms: i64, playable_ns: i64) {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            video_tail: i64,
            audio_tail: i64,
            audio_expected: bool,
            pts_shift: i64,
            presented: i64,
            paused: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                FEED_SHARED
                    .hls_video_tail_ns
                    .store(self.video_tail, Ordering::Relaxed);
                FEED_SHARED
                    .hls_audio_tail_ns
                    .store(self.audio_tail, Ordering::Relaxed);
                FEED_SHARED
                    .hls_audio_expected
                    .store(self.audio_expected, Ordering::Relaxed);
                FEED_SHARED.pts_shift.store(self.pts_shift, Ordering::Relaxed);
                FEED_SHARED.pres_fed.store(self.presented, Ordering::Relaxed);
                FEED_TX.commit_paused(self.paused);
                FEED_SHARED.reset_hls_clock_for_test();
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }

        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        FEED_SHARED.reset_hls_clock_for_test();
        let base = 10_000_000_000;
        let _restore = Restore {
            runway: FEED_SHARED.hls_prime_runway_ms.swap(runway_ms, Ordering::Relaxed),
            video_tail: FEED_SHARED
                .hls_video_tail_ns
                .swap(base + playable_ns, Ordering::Relaxed),
            audio_tail: FEED_SHARED
                .hls_audio_tail_ns
                .swap(base + playable_ns, Ordering::Relaxed),
            audio_expected: FEED_SHARED.hls_audio_expected.swap(true, Ordering::Relaxed),
            pts_shift: FEED_SHARED.pts_shift.swap(0, Ordering::Relaxed),
            presented: FEED_SHARED.pres_fed.swap(base, Ordering::Relaxed),
            paused: FEED_TX.paused.swap(true, Ordering::Relaxed),
        };
        let pause = FEED_SHARED.prepare_hls_user_pause().expect("reserve user Pause");
        let pause = match pause {
            plx_playback_contract::feed::HlsUserPause::Issue(token) => token,
            other => panic!("unexpected Pause reservation: {other:?}"),
        };
        assert_eq!(
            FEED_SHARED.complete_hls_user_pause(pause, true),
            plx_playback_contract::feed::HlsPauseCompletion::Accepted
        );
        assert_eq!(
            FEED_SHARED.prepare_hls_user_resume(true),
            Some(plx_playback_contract::feed::HlsUserResume::Prime)
        );

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        eng.prime_play = false;
        arm_live_clock_prime(&mut eng);
        assert_eq!(eng.seek_base_pts, base);
        push_segment_at(&mut eng, base);
        let before = plx_play_feed::ffi_host::play_calls_for_test();
        try_prime(&mt, &mut eng);
        assert_eq!(
            plx_play_feed::ffi_host::play_calls_for_test(),
            before,
            "Resume intent itself issued Play before feeding reopened",
        );

        FEED_TX.commit_paused(false);
        // Balanced decoder prime is still mandatory even when historical runway is ignored.
        eng.max_fed_video_pts = base + PRIME_NS;
        eng.max_fed_audio_pts = base;
        try_prime(&mt, &mut eng);
        assert_eq!(plx_play_feed::ffi_host::play_calls_for_test(), before);
        for _ in 0..600 {
            tick(&mt, &mut eng);
        }

        let accepted_video = eng.max_fed_video_pts - base;
        let accepted_audio = eng.max_fed_audio_pts - base;
        assert!(accepted_video >= PRIME_NS && accepted_audio >= PRIME_AUDIO_NS);
        assert!(
            accepted_video < runway_ms * 1_000_000 && accepted_audio < runway_ms * 1_000_000,
            "the fixture must leave part of the measured runway in the application queues",
        );
        assert_eq!(plx_play_feed::ffi_host::play_calls_for_test(), before + 1);
        assert!(
            !eng.prime_play,
            "viewer Resume stayed fenced after a balanced decoder prime",
        );
    }

    /// A `Play` request is an actuator command, not confirmation that the media clock moved. If
    /// Starfish refuses it, clearing the internal hold would publish smooth playback while the
    /// clock is still stopped and leave no path that retries the command.
    #[test]
    fn a_refused_play_keeps_the_internal_rebuffer_hold_armed() {
        let _serial = plx_base::testlock::serial();
        struct Restore {
            runway: i64,
            rebuffering: bool,
        }
        impl Drop for Restore {
            fn drop(&mut self) {
                FEED_SHARED
                    .hls_prime_runway_ms
                    .store(self.runway, Ordering::Relaxed);
                FEED_SHARED
                    .hls_rebuffering
                    .store(self.rebuffering, Ordering::Relaxed);
                FEED_SHARED.finish_hls_recovery();
                plx_play_feed::ffi_host::force_play_result_for_test(None);
                plx_play_feed::ffi_host::force_clocksink_for_test(false);
            }
        }

        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        plx_play_feed::ffi_host::force_play_result_for_test(Some(0));
        let _restore = Restore {
            runway: FEED_SHARED.hls_prime_runway_ms.swap(500, Ordering::Relaxed),
            rebuffering: FEED_SHARED.hls_rebuffering.swap(true, Ordering::Relaxed),
        };
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);

        let mt = unsafe { plx_base::task::MainThread::assume() };
        let mut eng = engine_after_reload();
        FEED_SHARED.begin_hls_recovery();
        FEED_SHARED.observe_hls_recovery(500_000, std::time::Duration::from_secs(2));
        push_one_segment(&mut eng);
        tick(&mt, &mut eng);

        assert!(
            eng.prime_play,
            "a refused Play must remain pending for the next pump tick"
        );
        assert!(
            FEED_SHARED.hls_rebuffering.load(Ordering::Relaxed),
            "the UI/runtime hold was cleared without a successful clock transition",
        );
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod lifecycle_clock_tests {
    use super::*;

    #[test]
    fn stop_without_an_engine_cannot_poison_the_next_initial_hold() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        assert!(
            !pa.is_live(),
            "test requires an empty native-session slot"
        );
        FEED_SHARED.reset_hls_clock_for_test();

        stop_bufferfeed(&mut ps, &mut pa);

        assert!(
            FEED_SHARED.arm_initial_clock_hold(false, false),
            "a no-op stop left ClockState in Stopping"
        );
        FEED_SHARED.cancel_clock_session_start();
    }
}

/// **LG App Self Checklist #46 — replay after completion.** A `plxnative-playurl` stream that
/// reaches EOS is started AGAIN, and that second start does not travel the UI's asynchronous
/// request path: `app.rs` re-arms `auto_tried`, the fixture entry calls `start_playback`, and
/// `start_bufferfeed_tracked` asks the reducer for a start owner directly. So the phase a
/// completed stop leaves behind is the whole contract between the two halves.
/// Gated on `hostsim` for the same reason [`lifecycle_clock_tests`] is: `stop_bufferfeed` reaches
/// the video sink, and only the simulator's (`ffi_host::HostSink`) keeps the state this asserts;
/// the default build's `NoSink` answers every verb with nothing.
#[cfg(all(test, feature = "hostsim"))]
mod replay_after_stop_tests {
    use super::*;

    /// Device-observed 2026-09-02 (`pipe_replay_after_eos`): `stop_bufferfeed: torn down` was
    /// followed by `replay: starting the finished stream again (0 left)` and then
    /// `start_bufferfeed: route reducer refused a start owner` — one Load and one HTTP fetch
    /// where the case requires two. The stop parks the reducer in `Stopping`, which is the fence
    /// for the *duration* of the synchronous main-thread teardown; nothing moved it out once that
    /// teardown returned, and `begin_route_start` mints a transaction only from a phase which owns
    /// no live Engine to preserve.
    #[test]
    fn a_completed_stop_grants_the_next_start_a_route_owner() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::route::reset_player_control_for_test(&ps);
        assert!(
            !pa.is_live(),
            "test requires an empty native-session slot"
        );

        stop_bufferfeed(&mut ps, &mut pa);

        let start = crate::route::begin_route_start()
            .expect("a completed stop must grant the replay a start owner");
        assert!(
            crate::route::prepare_route_start(start),
            "the replay owns the transaction it was just granted"
        );
        let attempt = crate::route::claim_route_start_attempt(start)
            .expect("a granted transaction must mint one physical Load attempt");
        assert!(crate::route::settle_route_start(&mut ps, 
            attempt,
            crate::route::RouteStartResult::Started
        ));

        crate::route::reset_player_control_for_test(&ps);
    }
}

/// Test-only: an Engine parked at `Stage::Streaming` on the host sink, for the pump-level rigs
/// that live outside this module (`pump::flight_tests`).
#[cfg(all(test, feature = "hostsim"))]
pub(super) fn streaming_engine_for_test(native_epoch: u32) -> Engine {
    let mut eng = prime_livelock_tests::engine_after_reload();
    eng.native_epoch = native_epoch;
    eng
}

/// **BACK's frame does not pay for `Unload`** (`PendingRelease`). On the dev set
/// `StarfishMediaAPIs::Unload` held the frame that handled BACK for 113-150 ms on every return
/// (`teardown spans: … sf_unload:135.7`). The host sink is given a slow Unload of its own
/// (`delay_unload_for_test`) and the contract is graded where the device's cannot be: the stop
/// returns long before the Unload does, nothing the stopped object says reaches the state the next
/// session owns, no native session can begin while the object is still being released, and a start
/// issued meanwhile is queued behind the release instead of racing it — and the release still ends in a
/// D1 with complete evidence, not a quarantine.
#[cfg(all(test, feature = "hostsim"))]
mod off_frame_release_tests {
    use super::*;

    const UNLOAD_MS: u64 = 400;

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    fn ready_now() -> c_int {
        unsafe { crate::player::test_physical::sink().ready(&MainThread::assume()) }
    }

    /// A live Engine over a ready host object whose Load has returned: what BACK tears down.
    fn playing_engine(pa: &mut crate::player::adapter::PlayerAdapter) -> u32 {
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        assert_eq!(unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some());
        let mut eng = streaming_engine_for_test(epoch);
        eng.load_th = None;
        pa.install(eng);
        assert_ne!(
            ready_now(),
            0,
            "PRECONDITION FAILED: the object must be dispatchable, or the stop has nothing to Unload"
        );
        epoch
    }

    fn arm(ps: &crate::route::PlaybackSession) -> crate::player::adapter::PlayerAdapter {
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        crate::player::adapter::PlayerAdapter::new_probed(unsafe { MainThread::assume() })
    }

    #[test]
    fn server_stop_releases_a_loaded_engine_off_the_frame() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        crate::route::note_engine_failure(&mut ps, super::super::RuntimeFailure::ServerStopped, 0);
        plx_play_feed::ffi_host::delay_unload_for_test(UNLOAD_MS);

        release_server_stopped_engine(&mut ps, &mut pa);

        assert!(!pa.is_live());
        assert!(pa.has_pending_release(), "the Unload must run off the frame thread");
        finish_release_for_test(&mut pa);
        assert_eq!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ServerStopped);
    }

    #[test]
    fn a_stop_returns_before_the_unload_and_the_next_start_is_deferred_until_its_tail_ran() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let epoch = playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(UNLOAD_MS);

        let started = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        let stop = started.elapsed();
        assert!(
            stop < std::time::Duration::from_millis(UNLOAD_MS / 4),
            "the stop held the frame for {stop:?} on a {UNLOAD_MS} ms Unload: BACK pays for it again"
        );
        assert!(!pa.is_live(), "the stop must still complete from the route's point of view");
        assert!(pa.has_pending_release(), "the object must be parked for its main-thread tail");

        // A callback the stopped object delivers now (a late error, say) is dropped: the session
        // state was already reset for whatever plays next.
        let before = FEED_SHARED.dg_cb_count.load(Ordering::Relaxed);
        plx_play_feed::sf_event::sf_on_event(epoch, 5, 0, std::ptr::null());
        assert_eq!(
            FEED_SHARED.dg_cb_count.load(Ordering::Relaxed),
            before,
            "a callback from the object being released reached the next session's state"
        );
        assert!(
            FEED_SHARED.begin_native_session().is_none(),
            "a native session began while the previous object was still inside Unload"
        );
        assert!(
            !poll_release(&mut ps, &mut pa, std::time::Instant::now()),
            "the per-frame poll must not wait for a running Unload"
        );

        // A start now (no URL, so it would end NoRoute) is deferred: it neither waits for the
        // Unload nor runs over the object, and the frame poll starts it once the tail has run.
        let before_start = std::time::Instant::now();
        let outcome = start_bufferfeed_tracked(&mut ps, &mut pa);
        assert_eq!(outcome, BufferfeedStartOutcome::Deferred);
        assert!(
            before_start.elapsed() < std::time::Duration::from_millis(UNLOAD_MS / 4),
            "the start waited for the Unload"
        );
        assert!(pa.has_pending_release(), "the Unload is still running");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !poll_release(&mut ps, &mut pa, std::time::Instant::now()) {
            assert!(std::time::Instant::now() < deadline, "the returned Unload was never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(UNLOAD_MS),
            "the tail ran before the Unload returned"
        );
        assert!(!pa.has_pending_release(), "the tail must leave no release pending");
        assert_eq!(ready_now(), 0, "the released object must be destroyed before a new one may load");
        assert!(
            !plx_play_feed::ffi_host::lifecycle_blocked_for_test(),
            "the off-frame release must end in a D1 with complete evidence, not a quarantine"
        );
        let next = FEED_SHARED.begin_native_session().expect("native sessions resume after the release");
        assert!(FEED_SHARED.retire_native_session(next));
    }

    #[test]
    fn the_frame_poll_finishes_a_returned_unload_and_exit_waits_for_a_running_one() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;

        // The ordinary return: the Unload comes back while Home draws; a later frame's poll runs
        // the tail.
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !poll_release(&mut ps, &mut pa, std::time::Instant::now()) {
            assert!(std::time::Instant::now() < deadline, "the returned Unload was never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pa.has_pending_release());
        assert_eq!(ready_now(), 0);
        assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test(), "the poll's tail must D1");

        // The exit (`app::run::shutdown`): a release still running is waited for, not abandoned.
        crate::route::reset_player_control_for_test(&ps);
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(UNLOAD_MS);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        finish_release_for_test(&mut pa);
        assert!(!pa.has_pending_release(), "finish_release must not return with the Unload running");
        assert_eq!(ready_now(), 0);
        assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test());
    }

    #[test]
    fn the_releases_own_unloadcompleted_does_not_count_into_the_next_sessions_diagnostics() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        finish_release_for_test(&mut pa);
        assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test(), "the release must still D1");
        assert_eq!(
            FEED_SHARED.dg_cb_count.load(Ordering::Relaxed),
            0,
            "the stopped object's UNLOADCOMPLETED (after reset_session) counted into the next session's callback census"
        );
        assert_eq!(FEED_SHARED.dg_cb_err_at.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn taking_the_token_or_splitting_never_waits_for_a_running_unload() {
        for use_split in [false, true] {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let _serial = plx_base::testlock::serial();
            let mut pa = arm(&ps);
            let _cleanup = Cleanup;
            playing_engine(&mut pa);
            plx_play_feed::ffi_host::delay_unload_for_test(UNLOAD_MS);
            let started = std::time::Instant::now();
            stop_bufferfeed(&mut ps, &mut pa);
            assert!(pa.has_pending_release());
            let before = std::time::Instant::now();
            if use_split {
                let _ = pa.split();
            } else {
                let _ = pa.mt();
            }
            assert!(
                before.elapsed() < std::time::Duration::from_millis(UNLOAD_MS / 4),
                "{} waited for the running Unload (PLX-NATIVE-1Q)",
                if use_split { "split()" } else { "mt()" }
            );
            // …and a verb through the adapter's sink (the composition's fence) does not reach the
            // object the worker is inside.
            assert_eq!(
                unsafe { pa.display_sink().pause(pa.mt()) },
                0,
                "a main-thread verb reached the object while it was Releasing"
            );
            assert!(pa.has_pending_release(), "nothing may finish the release but its own poll");
            finish_release_for_test(&mut pa);
            assert!(
                started.elapsed() >= std::time::Duration::from_millis(UNLOAD_MS),
                "the release finished before the Unload returned"
            );
            assert_eq!(ready_now(), 0);
            assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test());
        }
    }

    #[test]
    fn a_refused_unload_thread_unloads_on_the_main_thread_and_still_d1s() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(150);
        pa.probe().resources(|resources| resources.set_unload_spawner(Some(|_| None)));
        let started = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(150),
            "with no thread the Unload must have run synchronously inside the stop"
        );
        assert!(!pa.has_pending_release(), "no worker exists, so nothing may be parked");
        assert_eq!(ready_now(), 0, "the object must be destroyed");
        assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test(), "the fallback must end in a D1");
    }

    /// Lets the held host `Unload` go when a test unwinds, so a failed assertion cannot leave
    /// the next test's release hanging.
    struct ReleaseUnloadOnDrop;
    impl Drop for ReleaseUnloadOnDrop {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::release_unload_for_test();
        }
    }

    /// PLX-NATIVE-1Q: a start over an `Unload` that never returns froze the UI thread in the
    /// join. It must come back at once, deferred, and fail typed at the deadline.
    #[test]
    fn a_start_over_an_unload_that_never_returns_does_not_block_and_fails_typed_at_the_deadline() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::hold_unload_for_test();
        let t_stop = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        // A helper lets the hold go after 3 s, so the unfixed code ends instead of hanging.
        let helper = std::thread::Builder::new()
            .name("test-unload-release".into())
            .spawn(|| {
                std::thread::sleep(std::time::Duration::from_secs(3));
                plx_play_feed::ffi_host::release_unload_for_test();
            })
            .expect("spawn helper");

        let started = std::time::Instant::now();
        let outcome = start_bufferfeed_tracked(&mut ps, &mut pa);
        let took = started.elapsed();
        assert!(
            took < std::time::Duration::from_millis(50),
            "the start blocked the UI thread for {took:?} over an Unload that had not returned"
        );
        assert_eq!(outcome, BufferfeedStartOutcome::Deferred);
        assert!(!pa.is_live());
        assert!(FEED_SHARED.begin_native_session().is_none());
        assert!(pa.has_pending_release());
        assert_eq!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Resolving,
            "a deferred start must read as Connecting, not as an empty player"
        );

        // The deadline, on the machine's injected clock: the queued start fails typed.
        let t0 = t_stop;
        assert!(!poll_release(&mut ps, &mut pa, t0 + crate::player::release::RELEASE_DEADLINE - std::time::Duration::from_millis(1)));
        assert_ne!(
            crate::player::error_now(&ps).kind,
            crate::player::FailureKind::ReleaseTimeout,
            "the read-out appeared before the deadline"
        );
        assert!(!poll_release(&mut ps, &mut pa, t0 + crate::player::release::RELEASE_DEADLINE + std::time::Duration::from_secs(5)));
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Error);
        let shape = crate::player::error_now(&ps);
        assert_eq!(shape.kind, crate::player::FailureKind::ReleaseTimeout);
        assert_eq!(shape.kind.code(), "release_timeout");
        let actions = crate::player::failure_actions(
            shape.kind,
            crate::player::FailureContext { forced: false, can_retry: true, repair_idle: true },
        );
        assert!(actions.contains(&crate::player::FailureAction::TryAgain), "{actions:?}");

        // A second start is refused with no new object; the leaked object was not D1'd.
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Failed);
        assert!(!pa.is_live());
        assert_eq!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ReleaseTimeout);
        assert_ne!(
            unsafe { crate::player::test_physical::raw_sink().ready(&MainThread::assume()) },
            0,
            "the overdue object was destroyed under a worker that may still be inside it"
        );
        assert!(pa.has_pending_release(), "the overdue release must stay parked");
        assert!(pa.release_overdue());

        // The Unload finally returns: the tail runs, nothing quarantines, the next start goes.
        helper.join().unwrap(); // released by the helper after 3 s
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !poll_release(&mut ps, &mut pa, std::time::Instant::now()) {
            assert!(std::time::Instant::now() < give_up, "the late Unload was never finished");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pa.has_pending_release());
        assert_eq!(ready_now(), 0);
        assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test(), "the late tail must D1");
        assert_ne!(
            start_bufferfeed_tracked(&mut ps, &mut pa),
            BufferfeedStartOutcome::Deferred,
            "the slot must be free again"
        );
        assert_ne!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ReleaseTimeout);
    }

    /// A stop over a deferred start ends that playback: the queued start must not run after the tail.
    #[test]
    fn a_stop_over_a_deferred_start_cancels_it() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(!ps.readout().start_queued());
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        assert!(ps.readout().start_queued(), "the machine's queue reaches the session's readout");
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(!pa.release_has_queued_start());
        assert!(!ps.readout().start_queued(), "and leaves it with the stop that cancels it");
        assert_ne!(crate::player::state(&ps), crate::player::PlaybackState::Resolving);
        // The frame poll (may_start = true) is what would launch a surviving queued start.
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pa.release_busy() {
            assert!(std::time::Instant::now() < give_up, "the Unload never returned");
            poll_release(&mut ps, &mut pa, std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!pa.is_live(), "the cancelled start ran after the tail");
        assert_eq!(ready_now(), 0);
    }

    /// A landing replaces the whole session (`apply_plan` rebuilds it). The queued start lives in the
    /// release machine, not in the session, so a stop after that landing still cancels it.
    #[test]
    fn a_stop_after_a_landing_replaced_the_session_still_cancels_the_queued_start() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(50);
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Resolving, "derived from the machine");
        // A landing's rebuild keeps the readout (`decision.rs`, `readout: s.readout`); a session built
        // afresh, as here, learns the queue from the machine at the machine's next step (every step
        // states it), so no copy goes stale and no separate sync call exists.
        ps = crate::route::PlaybackSession::IDLE;
        assert!(!ps.readout().start_queued(), "a session built afresh holds no queue");
        assert!(pa.release_has_queued_start(), "the queue is the machine's, and the landing did not touch it");
        pa.release_step(&mut ps, &crate::player::release::ReleaseEvent::Tick { now: std::time::Instant::now() },
            &mut Vec::new());
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Resolving, "the machine's next step states its queue");
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(!pa.release_has_queued_start());
        assert!(!ps.readout().start_queued(), "and the stop's step states that it is gone");
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pa.release_busy() {
            assert!(std::time::Instant::now() < give_up, "the Unload never returned");
            poll_release(&mut ps, &mut pa, std::time::Instant::now());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(!pa.is_live(), "the queued start survived a stop and ran after the tail");
    }

    /// The foreground restore over a held `Unload` neither blocks nor loses the start: it parks, and
    /// lands once the release goes Idle, or fails typed at the deadline.
    #[test]
    fn a_foreground_start_over_a_held_unload_parks_without_blocking_then_lands() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        let t = std::time::Instant::now();
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        assert!(ps.readout().start_queued());
        pa.watch_queued_start();
        assert_eq!(pa.take_queued_landing(), crate::player::adapter::QueuedLanding::Waiting);
        poll_release(&mut ps, &mut pa, std::time::Instant::now());
        assert!(t.elapsed() < std::time::Duration::from_millis(50), "the restore blocked the frame");
        assert_eq!(pa.take_queued_landing(), crate::player::adapter::QueuedLanding::Waiting);
        plx_play_feed::ffi_host::release_unload_for_test();
        let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let landing = loop {
            poll_release(&mut ps, &mut pa, std::time::Instant::now());
            match pa.take_queued_landing() {
                crate::player::adapter::QueuedLanding::Waiting => {
                    assert!(std::time::Instant::now() < give_up, "the deferred start never ran");
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                other => break other,
            }
        };
        assert!(
            !matches!(landing, crate::player::adapter::QueuedLanding::Waiting | crate::player::adapter::QueuedLanding::Stale),
            "{landing:?}"
        );
        assert!(!ps.readout().start_queued(), "the tail that launched it emptied the queue");
    }

    /// A preview whose start is queued behind the release is not admitted while queued, and a
    /// deadline that refuses it (or a run that fails) settles the admission instead of leaving it counted.
    /// (Admission on a SUCCESSFUL launch needs a route fixture the rig does not have.)
    #[test]
    fn a_deferred_preview_start_is_settled_when_it_runs_not_when_queued() {
        use crate::player::preview::{phase_for_test, set_phase_for_test, reset_for_test, Phase};
        for refuse in [false, true] {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let _serial = plx_base::testlock::serial();
            let mut pa = arm(&ps);
            let _cleanup = Cleanup;
            let _release = ReleaseUnloadOnDrop;
            reset_for_test();
            playing_engine(&mut pa);
            plx_play_feed::ffi_host::hold_unload_for_test();
            stop_bufferfeed(&mut ps, &mut pa);
            set_phase_for_test(Phase::Fetching);
            assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
            pa.admit_preview_at_launch(true);
            assert_eq!(phase_for_test(), Phase::Fetching, "queued is not admitted");
            if refuse {
                let later = std::time::Instant::now() + crate::player::release::RELEASE_DEADLINE + std::time::Duration::from_secs(1);
                poll_release(&mut ps, &mut pa, later);
                assert_eq!(phase_for_test(), Phase::Idle, "a refused start gives the slot back");
            } else {
                plx_play_feed::ffi_host::release_unload_for_test();
                let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
                while phase_for_test() == Phase::Fetching {
                    assert!(std::time::Instant::now() < give_up, "the queued start never launched");
                    poll_release(&mut ps, &mut pa, std::time::Instant::now());
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                // The fixture session has no URL, so the start runs and ends NoRoute: the admission is
                // settled when the start RUNS (here refused, the slot given back), not when queued.
                assert_eq!(phase_for_test(), Phase::Idle, "settled at the run, not before");
            }
            reset_for_test();
        }
    }

    /// …and past the deadline the parked restore fails typed (`release_timeout`), still without blocking.
    #[test]
    fn a_foreground_start_over_an_overdue_unload_fails_typed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(start_bufferfeed_tracked(&mut ps, &mut pa), BufferfeedStartOutcome::Deferred);
        pa.watch_queued_start();
        let later = std::time::Instant::now() + crate::player::release::RELEASE_DEADLINE + std::time::Duration::from_secs(1);
        poll_release(&mut ps, &mut pa, later);
        assert_eq!(pa.take_queued_landing(), crate::player::adapter::QueuedLanding::Failed);
        assert_eq!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ReleaseTimeout);
    }

    /// The exit with an `Unload` that never returns is bounded by the deadline and says a worker
    /// is still in firmware, so the display is not torn down under it.
    #[test]
    fn exit_with_an_unload_that_never_returns_is_bounded_and_reports_a_worker_in_firmware() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        // An injected clock that jumps a second per look: no real sleep, the deadline in ~11 reads.
        let t0 = std::time::Instant::now();
        let mut ticks = 0u64;
        let exit = exit_native_with(
            &mut ps,
            &mut pa,
            || {
                ticks += 1;
                t0 + std::time::Duration::from_secs(ticks)
            },
            || {},
        );
        assert_eq!(exit, NativeExit::WorkerInFirmware);
        assert!(ticks >= 10, "the exit gave up before the deadline ({ticks} looks)");
        // The object is still in firmware: the adapter must not own it (dropping the adapter would
        // free the Engine under the live Unload), and its drop must never run.
        assert!(!pa.has_pending_release(), "the overdue object must leave the adapter, leaked");
        let dropped = engines_dropped_for_test();
        drop(pa);
        assert_eq!(engines_dropped_for_test(), dropped, "dropping the adapter freed the Engine");
        plx_play_feed::ffi_host::release_unload_for_test();
    }

    /// The flake behind `exit_after_a_load_timeout_never_quits_the_display_under_a_hung_load`: a
    /// test that ends with its `sf-unload` worker still held leaves a DETACHED thread whose
    /// `sf_unload` later reads the process-global `ACTIVE_EPOCH` (by then the NEXT test's session)
    /// and delivers UNLOADCOMPLETED into it. The reset every test opens and closes with must wait
    /// the worker out, so none outlives its test.
    #[test]
    fn the_lifecycle_reset_waits_out_a_released_unload_worker() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let _release = ReleaseUnloadOnDrop;
        playing_engine(&mut pa);
        plx_play_feed::ffi_host::hold_unload_for_test();
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(
            plx_play_feed::ffi_host::unload_workers_for_test(),
            1,
            "PRECONDITION FAILED: the stop must have a worker inside the held Unload"
        );
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        assert_eq!(
            plx_play_feed::ffi_host::unload_workers_for_test(),
            0,
            "the reset returned with an sf-unload worker still alive: it could fire into the next test"
        );
    }

    #[test]
    fn an_unloadcompleted_from_the_wrong_epoch_during_the_release_is_not_evidence() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        let mut pa = arm(&ps);
        let _cleanup = Cleanup;
        let epoch = playing_engine(&mut pa);
        plx_play_feed::ffi_host::delay_unload_for_test(UNLOAD_MS);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_pending_release());
        let before = FEED_SHARED.dg_cb_count.load(Ordering::Relaxed);
        plx_play_feed::sf_event::sf_on_event(epoch + 1, 23, 0, std::ptr::null());
        plx_play_feed::sf_event::sf_on_event(0, 23, 0, std::ptr::null());
        assert_eq!(FEED_SHARED.dg_cb_count.load(Ordering::Relaxed), before, "a wrong-epoch event was processed");
        assert!(
            !FEED_SHARED.native_unload_completed(epoch),
            "a wrong-epoch UNLOADCOMPLETED counted as the release's D1 evidence"
        );
        finish_release_for_test(&mut pa);
        assert!(!plx_play_feed::ffi_host::lifecycle_blocked_for_test());
    }
}

/// issue #74 D.1 regression tests. Real v0.6.0 dispatches every Starfish/ACB verb through
/// `sf_ready_object()` alone (`src/starfish.c`), which went live at `SMP_SET_READY(1)` BEFORE the
/// real, blocking `sf_load` call (`g_load_with_context`) returned — nothing waited for that
/// return. So the whole duration of a Load was a window in which `sf_is_load_completed`/
/// `sf_play`/`sf_feed`/etc. could dispatch into an object whose LoadCommon ctor path was still
/// running on the load thread: investigation.md's run A (a null-arg SIGSEGV on the priming path)
/// and run B (a Play-vs-LoadCommon mutex deadlock ending in an OMX_VideoComp abort).
#[cfg(all(test, feature = "hostsim"))]
mod load_in_flight_tests {
    use super::*;

    /// The Engine itself needs no cleanup since phase 9 — it lives in the test's own
    /// `PlayerAdapter`, which drops with the test. What is still process-wide is `FEED_SHARED`, the
    /// native lifecycle test seams, and the route reducer's projection.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            plx_play_feed::ffi_host::set_load_in_flight_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// An engine parked in `Stage::Loading`, native session installed, not yet primed. Mirrors
    /// [`prime_livelock_tests::engine_after_reload`] but for the state a fresh `Load` is in
    /// BEFORE it returns, rather than after.
    ///
    /// `rebase_pending: true` stands in for a segmented-HLS route fixture: `route::is_segmented_hls`
    /// is decided by `route.rs`, which another package in this run owns. `rebase_pending` drives
    /// the exact same `prime_before_play` branch at the pump's loadCompleted arm
    /// (`pump.rs::prime_before_play`) that segmented HLS does — the branch investigation.md §A.2
    /// (run A) actually took — so the code path under test is identical.
    fn engine_loading(epoch: u32) -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.native_epoch = epoch;
        eng.stage = Stage::Loading;
        eng.rebase_pending = true;
        eng.prime_play = false;
        eng.max_fed_video_pts = 0;
        eng.max_fed_audio_pts = 0;
        eng
    }

    /// Waits (bounded — this is a HOST test, never the television) for the host seam's
    /// `LOAD_IN_FLIGHT` window to actually open. This project's silent-instrument rule
    /// (AGENTS.md): a seam that never entered the in-flight window would score zero dispatches
    /// and prove nothing, so the precondition is its own assertion with its own message, kept
    /// separate from the real assertion below.
    fn wait_for_load_in_flight() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !plx_play_feed::ffi_host::load_in_flight_for_test() {
            assert!(
                std::time::Instant::now() < deadline,
                "PRECONDITION FAILED: sf_load never entered its in-flight window (loadCompleted \
                 emitted, not yet returned). A run that never opens this window would score zero \
                 seam dispatches and prove nothing about the gate — see AGENTS.md's \
                 silent-instrument rule."
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// The primary test: real concurrency, a real second thread running `native_load::load_thread`,
    /// a real held `sf_load`.
    #[test]
    fn nothing_reaches_the_seam_while_load_is_still_in_flight() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        // Push a whole segment's worth onto both lanes now, the way a demuxer already running
        // ahead of Load would (investigation.md §A.4 names exactly this as what differed between
        // the dev set's clean runs and the reporter's segmented-HLS one) — enough to clear both
        // `PRIME_NS` (video) and `PRIME_AUDIO_NS` (audio), so the positive half below can reach a
        // real `sf_play` once the gate opens, not just an engine that primed and stalled.
        {
            const V_STEP_NS: i64 = 41_666_666; // 24 fps
            const A_STEP_NS: i64 = 21_333_333; // 1024 samples at 48 kHz
            const VIDEO_AUS: i64 = 30; // ~1.25s > PRIME_NS (700ms)
            const AUDIO_AUS: i64 = 20; // ~427ms > PRIME_AUDIO_NS (300ms)
            let mut eng = pa.engine().expect("engine installed above");
            let au = [0u8; 32];
            let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
            for i in 0..VIDEO_AUS {
                plx_play_feed::aq::aq_push(
                    qv,
                    au.as_ptr(),
                    au.len() as c_int,
                    i * V_STEP_NS,
                    c_int::from(i == 0),
                    1,
                );
            }
            let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
            for i in 0..AUDIO_AUS {
                plx_play_feed::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, i * A_STEP_NS, 1, 2);
            }
        }

        plx_play_feed::ffi_host::hold_load_for_test();
        let payload = std::ffi::CString::new("{}").unwrap();
        let payload_ptr = plx_play_feed::SendPtr(payload.as_ptr() as *mut c_char);
        let load_thread = std::thread::Builder::new()
            .name("test-load-thread".into())
            .spawn(move || crate::player::test_physical::load_thread(payload_ptr, epoch, None, PcmLead::Off))
            .expect("spawn load_thread");

        wait_for_load_in_flight();

        // The primary assertion: several real pump ticks, none of which may reach the seam.
        for _ in 0..5 {
            crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        }
        assert_eq!(
            plx_play_feed::ffi_host::in_flight_calls_for_test(), 0,
            "the pump reached the Starfish/ACB seam ({:?}) while StarfishMediaAPIs::Load was \
             still executing on the load thread. In issue #74 that window is where the app died: \
             a SIGSEGV in libc with a null first argument on the priming path, and a \
             Play-vs-LoadCommon deadlock ending in an OMX_VideoComp abort on the immediate-Play \
             path.",
            plx_play_feed::ffi_host::in_flight_verb_for_test()
        );
        assert!(
            !FEED_SHARED.native_load_returned(epoch),
            "sanity: the Rust-side gate must still read closed while sf_load has not returned"
        );
        assert!(
            pa.engine().is_some_and(|e| e.stage == Stage::Loading),
            "the pump must not have advanced past Loading while Load was still in flight"
        );

        // Release the held Load and let it actually return.
        plx_play_feed::ffi_host::release_load_for_test();
        load_thread.join().expect("load_thread joins");

        // The positive half: the gate isn't just permanently closed. A few more ticks must now
        // advance past Loading and reach Play — proving the fix doesn't just refuse forever.
        let mut advanced = false;
        for _ in 0..20 {
            crate::player::pump::pump(&mut ps, &mut pa, 1_000);
            if pa.engine().is_some_and(|e| e.stage != Stage::Loading) {
                advanced = true;
                break;
            }
        }
        assert!(
            FEED_SHARED.native_load_returned(epoch),
            "the Rust-side gate must open once native_load::load_thread has actually returned"
        );
        assert!(
            advanced,
            "the gate opening must let the pump leave Stage::Loading"
        );
        assert!(
            plx_play_feed::ffi_host::play_calls_for_test() > 0,
            "once Load has genuinely returned, Play must eventually be reached — the gate must \
             not simply stay closed forever"
        );
    }

    /// `load-budget-skipped-when-the-gate-can-never-open` (a fix-wave finding, not a P1a
    /// acceptance line): the D.1.4 budget below shares its precondition with the gate it exists
    /// to bound — both `native_load_elapsed` and `native_load_returned` require the epoch to
    /// still own `NativeSessionPhase::Active`. A synchronous UnloadCompleted callback for the
    /// live epoch (firmware type=23) flips the phase to `Unloaded` without `load_call` ever
    /// having reached `Returned`, which — before this fix — left `pump.rs`'s loadCompleted arm
    /// deferring forever with no deadline: exactly the permanent Connecting spinner D.1.4 exists
    /// to make impossible, on the one path where the gate can never open again.
    #[test]
    fn losing_the_active_phase_while_still_deferred_fires_the_budget_instead_of_hanging() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        // `sf_ready(mt)` must already read nonzero (SMP_READY true) BEFORE the loadCompleted arm
        // is even reached — pump.rs's own `sf_ready == 0 => Connecting` early return sits ahead
        // of it — so this needs the same real held-Load rig as
        // `nothing_reaches_the_seam_while_load_is_still_in_flight`, not a bare phase flip.
        plx_play_feed::ffi_host::hold_load_for_test();
        let payload = std::ffi::CString::new("{}").unwrap();
        let payload_ptr = plx_play_feed::SendPtr(payload.as_ptr() as *mut c_char);
        let load_thread = std::thread::Builder::new()
            .name("test-load-thread-2".into())
            .spawn(move || crate::player::test_physical::load_thread(payload_ptr, epoch, None, PcmLead::Off))
            .expect("spawn load_thread");
        wait_for_load_in_flight();

        assert!(
            !FEED_SHARED.native_load_returned(epoch),
            "sanity: the gate starts closed"
        );

        // Simulate the firmware's synchronous UnloadCompleted callback for this epoch while the
        // pump is still deferring loadCompleted (Load is genuinely still executing on the load
        // thread) — the phase leaves Active for Unloaded without `load_call` ever reaching
        // `Returned`.
        FEED_SHARED.with_native_session(
            epoch,
            plx_play_feed::shared::NativeEventClass::UnloadCompleted,
            23,
            || {},
        );
        assert!(
            FEED_SHARED.native_load_elapsed(epoch).is_none(),
            "PRECONDITION FAILED: the epoch must have actually left Active, or this test proves \
             nothing about the hazard (AGENTS.md's silent-instrument rule)"
        );

        assert!(!FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire));
        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert!(
            FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "losing Active while loadCompleted was still deferred must fire the D.1.4 budget \
             immediately rather than silently disabling it — the permanent Connecting spinner \
             this exists to rule out"
        );

        // Unblock and join the held load thread so the process doesn't leak it into later tests.
        plx_play_feed::ffi_host::release_load_for_test();
        load_thread.join().expect("load_thread joins");
    }

    /// `load-budget-expiry-path-itself-has-no-test` (a fix-wave finding): of this arm's three
    /// match arms, `losing_the_active_phase_while_still_deferred_fires_the_budget_instead_of_hanging`
    /// above pins the `None` arm, but `Some(elapsed) if elapsed >= NATIVE_LOAD_BUDGET` — the arm
    /// the D.1.4 acceptance criterion is actually about — was reachable only by waiting a real 20s,
    /// so nothing drove it: inverting the comparison to `elapsed < NATIVE_LOAD_BUDGET` left the
    /// host suite green. `Shared::test_backdate_native_load_issued` (test-only) rewinds the
    /// recorded `load_issued_at` past the budget instead, so the same real held-Load rig as
    /// `nothing_reaches_the_seam_while_load_is_still_in_flight` can drive the actual comparison
    /// without a sleep.
    #[test]
    fn native_load_budget_expiry_fires_load_failed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        plx_play_feed::ffi_host::hold_load_for_test();
        let payload = std::ffi::CString::new("{}").unwrap();
        let payload_ptr = plx_play_feed::SendPtr(payload.as_ptr() as *mut c_char);
        let load_thread = std::thread::Builder::new()
            .name("test-load-thread-budget".into())
            .spawn(move || crate::player::test_physical::load_thread(payload_ptr, epoch, None, PcmLead::Off))
            .expect("spawn load_thread");
        wait_for_load_in_flight();

        assert!(
            FEED_SHARED.test_backdate_native_load_issued(epoch, plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the epoch must still own the Active phase to backdate its \
             load_issued_at, or this test proves nothing about the arm under test"
        );
        assert!(
            FEED_SHARED
                .native_load_elapsed(epoch)
                .is_some_and(|e| e >= plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the backdate must actually push elapsed past the budget — a \
             silent-instrument trap (AGENTS.md) if it doesn't"
        );
        assert!(
            !FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "sanity: load_failed starts clear"
        );

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);

        assert!(
            FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "elapsed >= NATIVE_LOAD_BUDGET must publish load_failed — an inverted or otherwise \
             broken comparison here would leave a Load that never returns spinning forever \
             instead of reaching the failure read-out"
        );

        // Unblock and join the held load thread so the process doesn't leak it into later tests.
        plx_play_feed::ffi_host::release_load_for_test();
        load_thread.join().expect("load_thread joins");
    }

    /// The budget verdict is the Feed's, read through the constructor gate: a constructor still in
    /// flight (the sink not ready, its Load live) reports `Awaiting { timeout: None }` while the
    /// Feed's clock, which starts at `begin_native_session`, is inside `NATIVE_LOAD_BUDGET`, and
    /// `Awaiting { timeout: Some(Elapsed(..)) }` (with the elapsed time, past the budget) once it
    /// has passed it. `native_load_budget_expiry_fires_load_failed` above pins the other site (the
    /// sink ready, the Load tick's verdict); this pins the gate's own, and that the clock decides it.
    #[test]
    fn the_constructor_gate_reports_the_load_timeout_once_the_feeds_clock_passes_the_budget() {
        use plx_playback_contract::feed::{CtorGate, LoadTimeout};
        let budget = plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET;
        let ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        assert_eq!(pa.feed_ctor_gate(), CtorGate::Awaiting { timeout: None },
            "a Load that has only just been issued is inside its budget");
        // The Feed's clock a second short of the budget: still inside it.
        let second = std::time::Duration::from_secs(1);
        assert!(FEED_SHARED.test_backdate_native_load_issued(epoch, budget - second),
            "PRECONDITION FAILED: the epoch must still own the Active phase to backdate its clock");
        assert!(FEED_SHARED.native_load_elapsed(epoch).is_some_and(|e| e < budget),
            "PRECONDITION FAILED: the backdated clock must still read inside the budget");
        assert_eq!(pa.feed_ctor_gate(), CtorGate::Awaiting { timeout: None },
            "a second short of the budget reports no timeout");
        assert!(!FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "the gate publishes no failure inside the budget");
        // Two seconds on: past it.
        assert!(FEED_SHARED.test_backdate_native_load_issued(epoch, second * 2));
        assert!(FEED_SHARED.native_load_elapsed(epoch).is_some_and(|e| e >= budget),
            "PRECONDITION FAILED: the backdated clock must now read past the budget");
        let verdict = pa.feed_ctor_gate();
        assert!(matches!(verdict, CtorGate::Awaiting { timeout: Some(LoadTimeout::Elapsed(e)) } if e >= budget),
            "past the budget the verdict is the elapsed time: {verdict:?}");
        assert!(FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "the verdict is published with the failure latch");
    }

    /// `load-returned-log-is-conditional-on-loadcompleted-and-unbounded-after`: the OTHER half of
    /// D.1.4's budget. `native_load_budget_expiry_fires_load_failed` above pins "Load never
    /// returns"; this pins "Load returns, but `loadCompleted` never arrives" — a wait that was
    /// completely unbounded before this fix, since `pump.rs`'s D.1.4 arm only runs while
    /// `!native_load_returned`, and stops being reachable the instant Load returns.
    ///
    /// Drives the gate by hand (like `the_gate_is_epoch_scoped` below): no real `sf_load` call is
    /// made at all — the host stub's `sf_load` unconditionally emits the `loadCompleted` callback
    /// before it can return, so it cannot represent "Load returned but loadCompleted never
    /// arrived". `force_object_ready_for_test` gives `sf_ready()` its `true` without that call,
    /// leaving `LOADED` (and so `sf_is_load_completed()`) at its default `false` — the "quiet
    /// pipeline" this fix is about.
    #[test]
    fn native_load_returned_loadcompleted_never_arrives_fires_load_failed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        plx_play_feed::ffi_host::force_object_ready_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        assert!(
            FEED_SHARED.mark_native_load_returned(epoch, None).is_some(),
            "PRECONDITION FAILED: Load must actually be marked Returned, or this test proves \
             nothing about the second-half budget"
        );
        assert!(
            FEED_SHARED.native_load_returned(epoch),
            "PRECONDITION FAILED: the gate must read Returned"
        );
        assert!(
            !FEED_SHARED.load_completed.load(std::sync::atomic::Ordering::Relaxed),
            "PRECONDITION FAILED: loadCompleted must genuinely never have arrived"
        );
        assert_ne!(
            unsafe { crate::player::test_physical::sink().ready(pa.mt()) },
            0,
            "PRECONDITION FAILED: sf_ready() must read true (force_object_ready_for_test), or \
             pump() bails out at its own sf_ready wait before ever reaching the arm under test"
        );
        assert_eq!(
            unsafe { crate::player::test_physical::sink().is_load_completed(pa.mt()) },
            0,
            "PRECONDITION FAILED: sf_is_load_completed() must read false — if it reads true, the \
             OTHER arm (loadCompleted arrived) fires instead and this test proves nothing about \
             the arm under test"
        );

        assert!(
            FEED_SHARED.test_backdate_native_load_returned(
                epoch,
                plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET
            ),
            "PRECONDITION FAILED: the epoch must still own the Active phase to backdate when it \
             returned, or this test proves nothing about the arm under test"
        );
        assert!(
            FEED_SHARED
                .native_load_returned_elapsed(epoch)
                .is_some_and(|e| e >= plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the backdate must actually push the returned-elapsed past the \
             budget — a silent-instrument trap (AGENTS.md) if it doesn't"
        );
        assert!(
            !FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "sanity: load_failed starts clear"
        );

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);

        assert!(
            FEED_SHARED.load_failed.load(std::sync::atomic::Ordering::Acquire),
            "loadCompleted never arriving within NATIVE_LOAD_BUDGET of Load RETURNING must \
             publish load_failed — an unbounded wait here is exactly the permanent-Connecting- \
             spinner shape D.1.4 exists to rule out, on the half that was still open"
        );
    }

    /// A narrow SIMULATION of the epoch-scoping property, not a historical reproduction of the
    /// crash — it drives both sides of the gate by hand (no real second thread, no real held
    /// `sf_load`) to prove a mark for a DIFFERENT (superseded) epoch can never open the current
    /// one's gate. See `nothing_reaches_the_seam_while_load_is_still_in_flight` for the
    /// real-concurrency test that this one deliberately does not attempt to replace.
    #[test]
    fn the_gate_is_epoch_scoped() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        pa.install(engine_loading(epoch));

        // Get the host seam's OBJECT_READY (so `sf_ready(mt)` reads true and the pump reaches
        // the loadCompleted arm at all) without arming HOLD_LOAD — this call returns at once.
        let payload = std::ffi::CString::new("{}").unwrap();
        assert_eq!(unsafe { crate::player::test_physical::sink().load(payload.as_ptr(), epoch) }, 1);
        // Hand-set the in-flight window and publish loadCompleted via the existing sf_on_event
        // path — simulating "Load has not returned" without a real second thread.
        plx_play_feed::ffi_host::set_load_in_flight_for_test(true);
        plx_play_feed::sf_event::sf_on_event(epoch, 2, 0, c"{\"loadCompleted\":true}".as_ptr());

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert_eq!(
            plx_play_feed::ffi_host::in_flight_calls_for_test(), 0,
            "loadCompleted alone, with the Rust-side gate not yet marked Returned for this \
             epoch, must not let any seam verb dispatch ({:?})",
            plx_play_feed::ffi_host::in_flight_verb_for_test()
        );
        assert!(
            pa.engine().is_some_and(|e| e.stage == Stage::Loading),
            "the gate must still be closed"
        );

        // Mark a DIFFERENT epoch as returned. Since it never owns the Active phase, this must be
        // a complete no-op for the session actually running.
        let other_epoch = epoch.wrapping_add(1).max(1);
        assert_ne!(other_epoch, epoch, "the test must pick a genuinely different epoch");
        assert!(
            FEED_SHARED.mark_native_load_returned(other_epoch, None).is_none(),
            "marking an epoch that does not own the Active phase must be a no-op"
        );
        assert!(
            !FEED_SHARED.native_load_returned(epoch),
            "a mark for a DIFFERENT epoch must never open THIS epoch's gate — this is the \
             epoch-scoping property that stops a late thread from a superseded session opening \
             the gate for a newer one"
        );

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert_eq!(
            plx_play_feed::ffi_host::in_flight_calls_for_test(), 0,
            "the pump must still defer after a differently-epoched mark ({:?})",
            plx_play_feed::ffi_host::in_flight_verb_for_test()
        );
        assert!(
            pa.engine().is_some_and(|e| e.stage == Stage::Loading),
            "marking an epoch other than the pump's own must never open ITS gate"
        );

        plx_play_feed::ffi_host::set_load_in_flight_for_test(false);
    }

    #[test]
    fn native_source_correction_ctor_wait_has_an_issued_budget() {
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&crate::route::PlaybackSession::IDLE);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        let mut ps = crate::route::PlaybackSession::IDLE;
        let epoch = FEED_SHARED.begin_native_session().unwrap();
        pa.install(engine_loading(epoch));
        assert_eq!(unsafe { crate::player::test_physical::sink().ready(pa.mt()) }, 0);
        crate::player::pump::pump(&mut ps, &mut pa, 1000);
        assert!(!FEED_SHARED.load_failed.load(Ordering::Acquire));
        assert!(FEED_SHARED.test_backdate_native_load_issued(epoch, plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET));
        crate::player::pump::pump(&mut ps, &mut pa, 1001);
        assert!(FEED_SHARED.load_failed.load(Ordering::Acquire), "constructor readiness must not bypass the issued budget");
    }
    #[test]
    fn native_source_correction_stale_load_refusal_cannot_fail_new_epoch() {
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let mt = unsafe { plx_base::task::MainThread::assume() };
        unsafe { crate::player::test_physical::sink().quarantine(&mt); }
        let old = FEED_SHARED.begin_native_session().unwrap();
        assert!(FEED_SHARED.retire_native_session(old));
        let current = FEED_SHARED.begin_native_session().unwrap();
        crate::player::test_physical::load_thread(plx_play_feed::SendPtr(c"{}".as_ptr() as *mut c_char), old, None, PcmLead::Off);
        assert!(!FEED_SHARED.load_failed.load(Ordering::Acquire), "a stale worker must not poison the newer epoch");
        assert!(!FEED_SHARED.native_load_returned(current));
        crate::player::test_physical::load_thread(plx_play_feed::SendPtr(c"{}".as_ptr() as *mut c_char), current, None, PcmLead::Off);
        assert!(FEED_SHARED.load_failed.load(Ordering::Acquire), "a current synchronous refusal must still fail");
    }

    #[test]
    fn native_ready_demux_failure_keeps_precedence_over_the_load_wait() {
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&crate::route::PlaybackSession::IDLE);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        plx_play_feed::ffi_host::force_object_ready_for_test(true);
        let _cleanup = Cleanup;
        let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        let mut ps = crate::route::PlaybackSession::IDLE;
        // The readout the pump derives the state into is addressed by the attempt the view began for.
        ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), crate::player::attempt_view::Backend::Feed);
        let epoch = FEED_SHARED.begin_native_session().unwrap();
        pa.install(engine_loading(epoch));
        crate::player::pump::pump(&mut ps, &mut pa, 1000);
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Connecting);
        FEED_SHARED.demux_failed.store(true, Ordering::Release);
        crate::player::pump::pump(&mut ps, &mut pa, 1001);
        assert_eq!(crate::player::state(&ps), crate::player::PlaybackState::Error);
    }

    #[test]
    fn server_stop_parks_a_native_load_that_has_not_returned() {
        use std::time::Duration;

        let mut ps = crate::route::PlaybackSession::IDLE;
        let _flight = crate::route::flight_rig::FlightRig::start(&mut ps, Duration::ZERO);
        crate::player::reset_session_for_test();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        let mut eng = engine_loading(epoch);
        plx_play_feed::ffi_host::hold_load_for_test();
        let payload_ptr = plx_play_feed::SendPtr(eng.payload.as_ptr() as *mut c_char);
        eng.load_th = Some(plx_base::task::spawn("test-server-stopped-load", move || {
            crate::player::test_physical::load_thread(payload_ptr, epoch, None, PcmLead::Off)
        }).expect("spawn the held Load"));
        pa.install(eng);
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let (result, returned) = std::sync::mpsc::channel();
        let watchdog = plx_base::task::spawn("test-server-stop-watchdog", move || {
            let returned_in_time = wait.recv_timeout(Duration::from_secs(5)).is_ok();
            plx_play_feed::ffi_host::release_load_for_test();
            let _ = result.send(returned_in_time);
        }).expect("spawn the held Load watchdog");
        wait_for_load_in_flight();

        crate::player::note_server_stop(crate::route::playback_session_id(&ps), "rig-1", "admin stop", 12_000);
        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        let parked = !pa.is_live() && pa.has_abandoned_load();
        let _ = done.send(());
        plx_base::task::join("test-server-stop-watchdog", watchdog);
        let returned = returned.recv().unwrap();
        if pa.is_live() {
            stop_bufferfeed(&mut ps, &mut pa);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while pa.has_abandoned_load() && !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the released Load was not reaped");
            std::thread::yield_now();
        }
        assert!(returned, "server stop blocked on the held native Load");
        assert!(parked, "the unfinished Load should be parked with its payload");
        assert_eq!(crate::player::error_now(&ps).kind, crate::player::FailureKind::ServerStopped);
    }

    /// PR #105 review finding: the D.1.4 budget turns a `Load` that never returns into a
    /// failure read-out, but BACK from that read-out ran `teardown`, which JOINED the media
    /// thread on the SDL main thread before anything else — so a genuinely hung `sf_load` froze
    /// the whole app the moment the user tried to leave the screen that told them about it.
    ///
    /// The held host `sf_load` is the hang. A watchdog releases it after `WATCHDOG` so a broken
    /// teardown FAILS this test (elapsed ≈ WATCHDOG) instead of hanging the suite.
    #[test]
    fn teardown_after_a_load_timeout_does_not_block_on_the_hung_load() {
        const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(3);
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;

        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        let mut eng = engine_loading(epoch);
        plx_play_feed::ffi_host::hold_load_for_test();
        let payload_ptr = plx_play_feed::SendPtr(eng.payload.as_ptr() as *mut c_char);
        eng.load_th = Some(
            std::thread::Builder::new()
                .name("test-load-thread-hung".into())
                .spawn(move || crate::player::test_physical::load_thread(payload_ptr, epoch, None, PcmLead::Off))
                .expect("spawn load_thread"),
        );
        pa.install(eng);
        wait_for_load_in_flight();

        assert!(
            FEED_SHARED.test_backdate_native_load_issued(epoch, plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the epoch must still own Active to backdate it"
        );
        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert!(
            FEED_SHARED.load_timed_out.load(Ordering::Acquire),
            "PRECONDITION FAILED: the budget must have fired, or this test is not about a timed-out Load"
        );

        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watchdog = {
            let (done, fired) = (done.clone(), fired.clone());
            std::thread::Builder::new().name("test-load-watchdog".into()).spawn(move || {
                let deadline = std::time::Instant::now() + WATCHDOG;
                while std::time::Instant::now() < deadline {
                    if done.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                fired.store(true, Ordering::Release);
                plx_play_feed::ffi_host::release_load_for_test();
            }).expect("spawn watchdog")
        };

        let started = std::time::Instant::now();
        stop_bufferfeed(&mut ps, &mut pa);
        let elapsed = started.elapsed();
        done.store(true, Ordering::Release);
        watchdog.join().unwrap();

        assert!(
            !fired.load(Ordering::Acquire) && elapsed < std::time::Duration::from_secs(1),
            "teardown blocked the main thread for {elapsed:?} on a Load that never returned \
             (watchdog had to release it: {}) — BACK from the timeout read-out freezes the app",
            fired.load(Ordering::Acquire)
        );
        assert_eq!(
            plx_play_feed::ffi_host::in_flight_calls_for_test(),
            0,
            "teardown reached the Starfish seam ({:?}) while Load was still in flight — the #74 race",
            plx_play_feed::ffi_host::in_flight_verb_for_test()
        );
        assert!(!pa.is_live(), "the stop must still complete from the route's point of view");
        assert!(pa.has_abandoned_load(), "the in-flight Load must be parked, not dropped");
        assert_ne!(
            unsafe { crate::player::test_physical::sink().ready(pa.mt()) },
            0,
            "the object must be neither unloaded, destroyed nor quarantined while Load is in flight"
        );
        assert!(
            FEED_SHARED.begin_native_session().is_none(),
            "no new native session may begin while the seam still owns the abandoned object"
        );

        // While Load is still in flight the per-frame release is a no-op and a start is refused.
        for _ in 0..3 {
            assert!(!reap_abandoned_load(&mut pa));
        }
        assert!(pa.has_abandoned_load());
        let start = crate::route::begin_route_start().expect("a completed stop grants a start owner");
        // A resolved, qualified source: the offer check passes and an unguarded start would go on to
        // build a second native object. Without one the start answers `NoRoute` (or, were the offer
        // not qualified, `StartFailed` for that reason), and this guard would never be reached.
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/hls/start.m3u8");
        assert!(ps.qualify_resolved_feed_offer(crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        assert_eq!(
            crate::player::route_start::start_bufferfeed_inner(&mut ps, &mut pa, start).err(),
            Some(crate::route::RouteStartResult::StartFailed),
            "a later playback must not be handed the object an abandoned Load still owns"
        );
        let _ = crate::route::abort_route_start(start, crate::route::RouteStartResult::StartFailed);
        assert_eq!(plx_play_feed::ffi_host::in_flight_calls_for_test(), 0, "{:?}", plx_play_feed::ffi_host::in_flight_verb_for_test());

        // Now the Load returns. The next reap releases the object with full evidence (D1, not a
        // quarantine), and native sessions are available again.
        plx_play_feed::ffi_host::release_load_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the returned Load was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(!pa.has_abandoned_load());
        assert_eq!(unsafe { crate::player::test_physical::sink().ready(pa.mt()) }, 0, "the released object must be gone");
        assert!(
            !plx_play_feed::ffi_host::lifecycle_blocked_for_test(),
            "the release must be a D1 with complete evidence, not a process-long quarantine"
        );
        let next = FEED_SHARED.begin_native_session().expect("native sessions resume after the release");
        assert!(FEED_SHARED.retire_native_session(next));
    }

    /// Releases the held host `sf_load` when a test unwinds, so a failed assertion cannot leave
    /// the next test's Load hanging.
    struct ReleaseLoadOnDrop;
    impl Drop for ReleaseLoadOnDrop {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::release_load_for_test();
        }
    }

    /// An Engine whose `sf_load` is held past the budget: the pump's budget arm has fired
    /// (`load_failed` + `load_timed_out`), the Load thread is still inside `sf_load`, and the
    /// Engine is installed in `pa`. The state the Auto Original's recovery starts from.
    fn install_engine_with_timed_out_load(
        ps: &mut crate::route::PlaybackSession,
        pa: &mut crate::player::adapter::PlayerAdapter,
    ) {
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        // What a real start does (`open_begin`): the attempt's readout exists before the
        // pump samples into it and before the teardown records its terminal facts.
        ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), crate::player::attempt_view::Backend::Feed);
        let mut eng = engine_loading(epoch);
        plx_play_feed::ffi_host::hold_load_for_test();
        let payload_ptr = plx_play_feed::SendPtr(eng.payload.as_ptr() as *mut c_char);
        eng.load_th = Some(
            std::thread::Builder::new()
                .name("test-load-thread-hung".into())
                .spawn(move || crate::player::test_physical::load_thread(payload_ptr, epoch, None, PcmLead::Off))
                .expect("spawn load_thread"),
        );
        pa.install(eng);
        wait_for_load_in_flight();
        assert!(
            FEED_SHARED.test_backdate_native_load_issued(epoch, plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET),
            "PRECONDITION FAILED: the epoch must still own Active to backdate it"
        );
        crate::player::pump::pump(ps, pa, 1_000);
        assert!(
            FEED_SHARED.load_timed_out.load(Ordering::Acquire),
            "PRECONDITION FAILED: the budget must have fired, or this test is not about a timed-out Load"
        );
    }

    /// The exit after a playback error (`load_timeout`): `app::run::shutdown` then quits SDL and
    /// destroys the Wayland display. A Load still inside firmware on the `media` thread must make
    /// the exit say so, so the display is left to the OS rather than torn down under it.
    #[test]
    fn exit_after_a_load_timeout_never_quits_the_display_under_a_hung_load() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        install_engine_with_timed_out_load(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        let exit = exit_native(&mut ps, &mut pa);
        assert!(
            exit == NativeExit::WorkerInFirmware || !plx_play_feed::ffi_host::load_in_flight_for_test(),
            "the exit's native half returned with a Load still inside firmware: SDL_Quit would run under it"
        );
        assert_eq!(exit, NativeExit::WorkerInFirmware);
        // The payload the `media` thread reads is leaked, not owned by the adapter that is dropped next.
        assert!(!pa.has_abandoned_load(), "the parked Load must leave the adapter, leaked");
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// A live session's Stats-for-nerds rows: an Engine installed, the mailboxes holding a
    /// session's values, the pump's sample ingested, and `diag()` showing them.
    fn show_live_rows(ps: &mut crate::route::PlaybackSession,
        pa: &mut crate::player::adapter::PlayerAdapter) {
        crate::route::set_url(ps, "http://127.0.0.1:1/hls/start.m3u8");
        assert!(ps.qualify_resolved_feed_offer(crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        install_engine_with_timed_out_load(ps, pa);
        let live = crate::player::diag_sample_tests::scripted();
        crate::player::diag_sample_tests::store_mailboxes(&live);
        pa.publish_feed_attempt_view(ps);
        assert_eq!(crate::player::diag_sample_tests::rows(&crate::player::test_physical::diag(ps)), live,
            "PRECONDITION FAILED: the live session's rows must be on the read-out before the teardown");
    }

    /// **The line printed right after a real stop** reads the idle rows: the teardown resets the
    /// mailboxes and then the readout (`SessionReset`), in one call, so the sample the read-out holds
    /// is the idle set the mailboxes were reset to (`place_rv` is `i32::MIN`, not 0).
    #[test]
    fn the_rows_after_a_real_stop_are_the_idle_rows() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        show_live_rows(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!(crate::player::diag_sample_tests::rows(&d), plx_playback_contract::feed::FeedDiag::IDLE,
            "after a stop the rows are the reset mailboxes' values");
        assert_eq!(d.place_rv, i32::MIN);
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// **The line printed right after a real reload** reads the idle rows too: a reload resets the
    /// session's mailboxes and its readout alike, and the new Engine has not been sampled yet.
    #[test]
    fn the_rows_after_a_real_reload_are_the_idle_rows() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        show_live_rows(&mut ps, &mut pa);
        let outcome = reload_transcode(&mut ps, &mut pa, 0);
        let _ = crate::player::pump::settle_reload(outcome, "test reload", &mut ps);
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!(crate::player::diag_sample_tests::rows(&d), plx_playback_contract::feed::FeedDiag::IDLE,
            "after a reload the rows are the reset mailboxes' values");
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// The Original failure pair is the playback's: the stop that ends the playback clears it, the
    /// reload that restores HLS keeps it. A live session whose Original was refused, as the rows
    /// show it, then each teardown.
    fn show_original_failure(ps: &mut crate::route::PlaybackSession,
        pa: &mut crate::player::adapter::PlayerAdapter) {
        show_live_rows(ps, pa);
        crate::player::note_original_failure(ps, crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503)),
            "PRECONDITION FAILED: the failure is the readout's before the teardown");
    }

    /// **A real stop clears the Original failure**: a new playback must not carry another item's PMS
    /// response into its diagnostics. Its own event, stepped after the session's reset.
    #[test]
    fn a_real_stop_clears_the_original_failure() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        show_original_failure(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(ps.readout().original_failure(), None);
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!((d.abr_failure_kind, d.abr_failure_status), (0, 0));
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// **A rollback reload keeps the Original failure**: restoring HLS tears the Engine down and
    /// rebuilds it, and the cause of the refusal must still be there to be photographed.
    #[test]
    fn a_rollback_reload_keeps_the_original_failure() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        show_original_failure(&mut ps, &mut pa);
        let outcome = reload_transcode(&mut ps, &mut pa, 0);
        let _ = crate::player::pump::settle_reload(outcome, "test rollback reload", &mut ps);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503)));
        let d = crate::player::test_physical::diag(&ps);
        assert_eq!((d.abr_failure_kind, d.abr_failure_status), (crate::player::ABR_FAILURE_ORIGINAL_HTTP, 503));
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// The channel half: an edit the worker sent that the pump has not drained when the Engine is
    /// torn down is taken in by the teardown's last drain (the worker is joined by then), so a
    /// reload keeps a failure noted a frame ago and a real stop still ends with it cleared. The new
    /// open's channel that follows a reload starts with an idle shipment, and applying that cannot
    /// erase the failure; the new open's own clear does.
    #[test]
    fn a_workers_note_survives_the_reload_that_follows_it_and_the_new_opens_idle_shipment() {
        use plx_playback_contract::adaptive::{AdaptiveEffects, FailureEdit};
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        show_live_rows(&mut ps, &mut pa);
        let attempt = crate::route::RouteStartAttempt::fixture();
        let (tx, inbox) = crate::adaptive::readout_channel();
        pa.bind_abr_inbox(attempt, inbox);
        let mut worker = crate::adaptive::PlayerAdaptiveEffects::new(tx);
        worker.failure(FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_DEADLINE, status: 0 });
        assert_eq!(ps.readout().original_failure(), None, "PRECONDITION FAILED: nobody has drained the edit");

        let outcome = reload_transcode(&mut ps, &mut pa, 0);
        let _ = crate::player::pump::settle_reload(outcome, "test reload", &mut ps);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_DEADLINE, 0)),
            "the teardown took the worker's last report in before it closed the channel");

        // The reload's new open: a new channel and a worker that has said nothing yet.
        ps.begin_attempt_view(attempt, crate::player::attempt_view::Backend::Feed);
        let (tx, inbox) = crate::adaptive::readout_channel();
        pa.bind_abr_inbox(attempt, inbox);
        let mut renewed = crate::adaptive::PlayerAdaptiveEffects::new(tx);
        renewed.readout(plx_playback_contract::adaptive::AbrReadout::IDLE);
        pa.drain_abr(&mut ps);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_DEADLINE, 0)),
            "the new open's idle shipment must not erase the failure the reload kept");
        renewed.failure(FailureEdit::Cleared);
        pa.drain_abr(&mut ps);
        assert_eq!(ps.readout().original_failure(), None, "the new open's own clear does");

        // Two edits between drains end as the later one, in either order: every edit replaces the
        // whole pair, so the drain's coalescing loses nothing.
        renewed.failure(FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_HTTP, status: 503 });
        renewed.failure(FailureEdit::Cleared);
        pa.drain_abr(&mut ps);
        assert_eq!(ps.readout().original_failure(), None, "a note and the clear after it, one drain");
        renewed.failure(FailureEdit::Cleared);
        renewed.failure(FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_OPEN, status: 0 });
        pa.drain_abr(&mut ps);
        assert_eq!(ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0)),
            "a clear and the note after it, one drain");
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// A note the worker sent just before a REAL stop is taken in by the teardown's drain and then
    /// cleared with the playback: the stop's own event comes after the drain, so it is the last word.
    #[test]
    fn a_workers_note_before_a_real_stop_is_taken_in_and_then_cleared() {
        use plx_playback_contract::adaptive::{AdaptiveEffects, FailureEdit};
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        show_live_rows(&mut ps, &mut pa);
        let (tx, inbox) = crate::adaptive::readout_channel();
        pa.bind_abr_inbox(crate::route::RouteStartAttempt::fixture(), inbox);
        let mut worker = crate::adaptive::PlayerAdaptiveEffects::new(tx);
        worker.failure(FailureEdit::Noted { kind: crate::player::ABR_FAILURE_ORIGINAL_TRANSPORT, status: 0 });
        stop_bufferfeed(&mut ps, &mut pa);
        assert_eq!(ps.readout().original_failure(), None);
        plx_play_feed::ffi_host::release_load_for_test();
    }

    /// PLX-NATIVE-15: the Auto Original's Load passes its budget, the HLS recovery reloads while
    /// the Load is still inside `sf_load`, the reload's teardown parks that Load (clearing
    /// `load_timed_out` with the rest of the session) and the start is refused. The failure was
    /// published with no cause, so the report said `unspecified`; the cause is the timeout.
    #[test]
    fn a_recovery_reload_refused_for_an_abandoned_load_still_reports_the_timeout() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/hls/start.m3u8");
        assert!(ps.qualify_resolved_feed_offer(crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        install_engine_with_timed_out_load(&mut ps, &mut pa);

        // `land_recovery_flight`'s own two calls: the reload (whose teardown parks the
        // still-running Load) and the settle that publishes the outcome.
        let outcome = reload_transcode(&mut ps, &mut pa, 0);
        let started = crate::player::pump::settle_reload(outcome, "test HLS recovery", &mut ps);
        assert!(!started, "the recovery start must be refused while the Load is outstanding");
        assert!(pa.has_abandoned_load(), "PRECONDITION FAILED: the Load must still be parked");

        // One frame later: with no Engine live the pump's no-engine arm sets Idle. The refusal is
        // the session's own fact (`native_start_refused`, the slot a fresh start refused for the
        // same Load sets), so the read-out outlives that frame, as the fresh-start sibling's does:
        // Error with Try Again until a start clears it, not an Error that lasts one frame.
        crate::player::pump::pump(&mut ps, &mut pa, 1_000);
        assert_eq!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "the refused reload's Error must outlive the next pump frame, not fall back to Idle"
        );
        assert_eq!(
            crate::player::error_now(&ps).kind,
            crate::player::FailureKind::LoadTimeout,
            "the refusal must keep the timeout as its cause, not report `unspecified`"
        );
        assert_eq!(ps.native_start_refused, Some(crate::route::StartRefusal::LoadTimeout));
        assert!(
            crate::player::test_physical::diag(&ps).load_failed,
            "the diagnostics Load row (and the lab snapshot) must read the refused reload as refused"
        );
    }

    /// The fresh-start half of the same hole: a Retry or Back+Play while the parked Load is still
    /// inside `sf_load` is refused with no Engine to publish anything, so the player sat in Idle
    /// and drew nothing: no read-out, no report, no Try Again. The refusal stays (a second native
    /// object must not be started), but the viewer is told why.
    #[test]
    fn a_fresh_start_refused_for_an_abandoned_load_shows_an_error_with_try_again() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        install_engine_with_timed_out_load(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_abandoned_load(), "PRECONDITION FAILED: the Load must be parked");

        // `finish_landed_play`'s entry. A refused start answers false and creates no Engine.
        assert!(!crate::player::start_bufferfeed(&mut ps, &mut pa));
        assert!(!pa.is_live(), "the start stays refused: no second native object");
        assert!(pa.has_abandoned_load());

        assert_eq!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "a refused fresh start left the player Idle: a black screen with no read-out"
        );
        let shape = crate::player::error_now(&ps);
        assert_eq!(shape.kind, crate::player::FailureKind::LoadTimeout);
        let actions = crate::player::failure_actions(
            shape.kind,
            crate::player::FailureContext { forced: false, can_retry: true, repair_idle: true },
        );
        assert!(
            actions.contains(&crate::player::FailureAction::TryAgain),
            "the read-out must offer Try Again: {actions:?}"
        );
        assert!(
            crate::player::test_physical::diag(&ps).load_failed,
            "the diagnostics Load row (and the lab snapshot) must read the refused start as refused"
        );

        // The Load returns and is reaped: the next start is no longer refused for it, and the
        // refusal's read-out does not outlive the refusal.
        plx_play_feed::ffi_host::release_load_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the returned Load was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = crate::player::start_bufferfeed(&mut ps, &mut pa);
        assert_ne!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "a start that is no longer refused must not keep the refusal's read-out"
        );
    }

    /// The latch's other edge: a start refused for the parked Load sets `native_start_refused`,
    /// and a RELOAD that then starts (the foreground restore's rollback landing after the Load was
    /// reaped: `land_engineless_rollback` -> `reload_transcode_tracked`) goes through
    /// `start_bufferfeed_for`, not `start_bufferfeed_tracked`, which is where the flag used to be
    /// cleared. The flag outlived the refusal and, once the flight ended, `state()` answered Error
    /// over playing video and `report::tick` sent a bogus `load_timeout`.
    #[test]
    fn a_reload_that_starts_after_a_refused_fresh_start_does_not_keep_the_refusals_error() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _release = ReleaseLoadOnDrop;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        install_engine_with_timed_out_load(&mut ps, &mut pa);
        stop_bufferfeed(&mut ps, &mut pa);
        assert!(pa.has_abandoned_load(), "PRECONDITION FAILED: the Load must be parked");
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/hls/start.m3u8");
        assert!(ps.qualify_resolved_feed_offer(crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        crate::route::set_stream_acodec(&mut ps, "aac");

        // The resumed foreground's `PlayerForegroundActuator::start_load`.
        assert_eq!(
            start_bufferfeed_tracked(&mut ps, &mut pa),
            BufferfeedStartOutcome::Failed,
            "PRECONDITION FAILED: the fresh start must be refused for the parked Load"
        );
        assert!(ps.native_start_refused.is_some(), "PRECONDITION FAILED: the refusal must be recorded");

        // The parked Load returns and is reaped; the rollback's reload then starts.
        plx_play_feed::ffi_host::release_load_for_test();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !reap_abandoned_load(&mut pa) {
            assert!(std::time::Instant::now() < deadline, "the returned Load was never released");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let reload = reload_transcode_tracked(&mut ps, &mut pa, 0);
        assert!(
            matches!(reload, BufferfeedStartOutcome::Launched(_)),
            "PRECONDITION FAILED: the reload must start once the Load is reaped ({reload:?})"
        );
        assert!(pa.is_live(), "the reload must have built an Engine");

        assert_ne!(
            crate::player::state(&ps),
            crate::player::PlaybackState::Error,
            "a reload that started kept the refused fresh start's Error over live video \
             (and report::tick would send a bogus load_timeout)"
        );
        stop_bufferfeed(&mut ps, &mut pa);
    }


}

#[cfg(test)]
mod pcm_info_tests {
    use super::*;
    use crate::route::PcmFormat;

    const BASE: &str = r#"{"contents":{"codec":{"audio":"PCM"},"provider":"plxnative"}}"#;

    #[test]
    fn a_pcm_load_states_the_shape_the_decoder_produces() {
        let f = PcmFormat { sample_rate: 48_000, bits: 16, channels: 2, source_rate: 48_000 };
        let p = with_pcm_info(BASE, "PCM", Some(f));
        assert!(p.contains(r#""pcmInfo":{"format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":48,"bitsPerSample":16}"#), "{p}");
        let wide = with_pcm_info(BASE, "PCM", Some(PcmFormat { sample_rate: 48_000, bits: 32, channels: 1, source_rate: 96_000 }));
        assert!(wide.contains(r#""format":"S32LE","channelMode":"mono""#) && wide.contains(r#""bitsPerSample":32"#), "{wide}");
        // the payload stays parseable JSON
        serde_json::from_str::<serde_json::Value>(&p).unwrap();
    }

    /// **A hi-res track the app resamples is declared as what the decoder emits**, never as the file's own rate:
    /// the Load is built from the format `route::pcm_format_of` returns, which names the PCM input's table
    /// rate in S32 (a 16-bit source included), so the payload carries 48 or 44.1 kHz and 32 bits and none of 96, 192 or 88.2.
    #[test]
    fn a_resampled_hi_res_track_declares_the_tables_rate_and_a_32_bit_sample() {
        let track = |rate: i64, bits: i64| crate::route::CarriedAudio {
            sid: 7, ordinal: 4, codec: "flac".into(), channels: 2,
            can_normalize_loudness: false, immersive: false, sample_rate: rate, bit_depth: bits,
        };
        for (rate, bits, khz, never) in [(96_000, 24, "48", ["96", "192", "88.2"]), (192_000, 24, "48", ["96", "192", "88.2"]), (88_200, 16, "44.1", ["96", "192", "88.2"])] {
            let f = crate::route::pcm_format_of(Some(&track(rate, bits)), "flac").unwrap();
            let p = with_pcm_info(BASE, "PCM", Some(f));
            let v: serde_json::Value = serde_json::from_str(&p).unwrap();
            let info = &v["contents"]["pcmInfo"];
            assert_eq!(info["format"], "S32LE", "{rate}/{bits}: {p}");
            assert_eq!(info["bitsPerSample"], 32, "{rate}/{bits}: {p}");
            assert_eq!(info["sampleRate"].as_f64(), khz.parse::<f64>().ok(), "{rate}/{bits}: {p}");
            for n in never {
                assert!(!p.contains(&format!(r#""sampleRate":{n}"#)), "{rate}/{bits}: the file's own rate {n} must not be declared: {p}");
            }
        }
    }

    /// The diagnostics line names the track's own rate only where the app resamples it.
    #[test]
    fn the_decode_log_line_names_the_source_rate_only_when_it_is_resampled() {
        let line = |codec: &str, format: &PcmFormat| {
            plx_play_feed::pcm_decode_line(codec, &(*format).into())
        };
        let exact = PcmFormat { sample_rate: 48_000, bits: 16, channels: 2, source_rate: 48_000 };
        assert_eq!(line("flac", &exact), "pcm: flac decoded in-app -> 48000Hz 16-bit 2ch");
        let hires = PcmFormat { sample_rate: 48_000, bits: 32, channels: 2, source_rate: 96_000 };
        assert_eq!(line("flac", &hires), "pcm: flac decoded in-app -> 48000Hz 32-bit 2ch (resampled from 96000Hz)");
        assert_eq!(line("dca", &PcmFormat::STEREO_48K), "pcm: dca decoded in-app -> 48000Hz 16-bit 2ch");
    }

    /// Every rate the PCM path admits is declared in kilohertz, the unit libpf's table is in. In
    /// hertz each of them played at the 44.1 kHz fallback on the B8, with no error anywhere.
    #[test]
    fn the_sample_rate_is_declared_in_kilohertz() {
        let want = ["8", "12", "16", "22.05", "24", "32", "44.1", "48"];
        for (hz, khz) in plx_platform::devcaps::PCM_SAMPLE_RATES.into_iter().zip(want) {
            assert_eq!(pcm_rate_khz(hz), khz);
            let p = with_pcm_info(BASE, "PCM", Some(PcmFormat { sample_rate: hz, bits: 16, channels: 2, source_rate: hz }));
            let v: serde_json::Value = serde_json::from_str(&p).unwrap();
            let declared = v["contents"]["pcmInfo"]["sampleRate"].as_f64().unwrap();
            assert!((declared * 1000.0 - f64::from(hz)).abs() < 1e-6, "{hz}: {p}");
        }
        assert_eq!(pcm_rate_khz(11_025), "11.025");
    }

    #[test]
    fn a_speed_session_leaves_the_pcm_to_the_speed_decoder() {
        let f = || Some(Ok(PcmFormat { sample_rate: 44_100, bits: 16, channels: 2, source_rate: 44_100 }));
        assert_eq!(app_pcm_for_rate(crate::player::RATE_NORMAL, f()), f(), "1x: the FLAC decoder's shape");
        assert!(app_pcm_for_rate(1500, f()).is_none(), "1.5x: the speed decoder declares its own");
        // a FLAC track whose facts are missing only fails at 1x, where the FLAC decoder needs them
        let missing = || Some(Err::<crate::route::PcmFormat, _>("unknown".to_string()));
        assert!(app_pcm_for_rate(crate::player::RATE_NORMAL, missing()).is_some_and(|r| r.is_err()));
        assert!(app_pcm_for_rate(750, missing()).is_none());
    }

    /// **DTS the attempt decodes in the app** is declared as PCM — stereo 48 kHz S16, one `pcmInfo` — and
    /// where it does not is `DTS` exactly as before. The shape is the session's own
    /// (`route::stream_pcm`), so a native DTS Load can never carry a PCM node nor the reverse.
    #[test]
    fn app_decoded_dts_is_declared_as_pcm_and_native_dts_as_dts() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::set_stream_codecs(&mut ps, "h264", "dts");
        assert_eq!(audio_declaration("dts", crate::route::stream_pcm(&ps).and_then(Result::ok).is_some()), Some(("DTS", 4)));
        let off = build_av_payload_at(&ps, "H264", "DTS", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert!(off.contains(r#""audio":"DTS""#) && !off.contains("pcmInfo"), "{off}");

        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
        assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K));
        assert_eq!(audio_declaration("dts", shape.is_some()), Some(("PCM", 5)));
        let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert!(on.contains(r#""audio":"PCM""#), "{on}");
        assert_eq!(on.matches("pcmInfo").count(), 1, "{on}");
        assert!(on.contains(r#""format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":48,"bitsPerSample":16"#), "{on}");
        // at a speed the speed decoder declares PCM itself, from the same codec: still one node
        let fast = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, 1500);
        assert_eq!(fast.matches("pcmInfo").count(), 1, "{fast}");
        serde_json::from_str::<serde_json::Value>(&on).unwrap();
    }

    /// **Force Direct Play on a set with no DTS row declares a STEREO DTS track as stereo 48 kHz PCM**: the
    /// app decodes it (the table's lossy policy admits DTS, the set has no native path, and a 2.0 source
    /// is no downmix, `Caps::app_may_mix_down`), so the Load carries one `pcmInfo` and `PCM` instead of a
    /// `DTS` the pipeline would hang on. Under Auto the same value still declares `DTS`. The carried track
    /// supplies the channel count the rule reads; a 5.1 one is refused before any Load
    /// (`forced_resolve_refuses_a_surround_dts_track_instead_of_mixing_it_down`).
    #[test]
    fn a_forced_dts_with_no_row_is_declared_as_stereo_48k_pcm() {
        let caps = std::sync::Arc::new(plx_platform::devcaps::Caps {
            app_lossy: plx_platform::devcaps::AppLossy::Measured,
            ..plx_platform::devcaps::Caps::assumed()
        });
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::begin_attempt_for_test(&mut ps, caps);
        crate::route::set_stream_codecs(&mut ps, "h264", "dts");
        crate::route::set_carried_audio_for_test(
            &mut ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "dts".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 48_000, bit_depth: 0,
            },
        );
        assert_eq!(crate::route::stream_pcm(&ps).and_then(Result::ok), None, "Auto: the native decoder's");
        crate::route::set_direct_play_mode_for_test(&mut ps, crate::route::DirectPlayMode::Forced);
        let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
        assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K));
        assert_eq!(audio_declaration("dts", shape.is_some()), Some(("PCM", 5)));
        let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert!(on.contains(r#""audio":"PCM""#) && !on.contains(r#""audio":"DTS""#), "{on}");
        assert_eq!(on.matches("pcmInfo").count(), 1, "{on}");
        serde_json::from_str::<serde_json::Value>(&on).unwrap();
    }

    /// MP3 and Vorbis in force (S4-4) are declared exactly as DTS is: `PCM`, stereo 48 kHz S16.
    #[test]
    fn app_decoded_mp3_and_vorbis_are_declared_as_pcm_and_native_ones_are_refused() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        for codec in ["mp3", "vorbis"] {
            crate::route::set_stream_codecs(&mut ps, "h264", codec);
            let off = crate::route::stream_pcm(&ps).and_then(Result::ok);
            assert_eq!(off, None, "{codec}: gate off");
            assert_eq!(audio_declaration(codec, off.is_some()).map(|d| d.0).filter(|n| *n == "PCM"), None, "{codec}");
        }
        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        for codec in ["mp3", "vorbis"] {
            crate::route::set_stream_codecs(&mut ps, "h264", codec);
            let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
            assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K), "{codec}");
            assert_eq!(audio_declaration(codec, shape.is_some()), Some(("PCM", 5)), "{codec}");
            let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
            assert_eq!(on.matches("pcmInfo").count(), 1, "{codec}: {on}");
            // declared PCM, so planned the stack's lead like a FLAC Load (`load_audio` is the one value)
            let planned =
                load_audio(codec, crate::player::RATE_NORMAL, shape.is_some(), None, AudioStack::Liblxa).unwrap();
            assert_eq!((planned.name, planned.lead), ("PCM", PcmLead::FromOutput(AudioStack::Liblxa)), "{codec}");
        }
    }

    /// TrueHD in force (S4-5) is declared as `PCM`, stereo 48 kHz S16, like the other lossy ones.
    #[test]
    fn app_decoded_truehd_is_declared_as_pcm() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::set_stream_codecs(&mut ps, "h264", "truehd");
        assert_eq!(crate::route::stream_pcm(&ps).and_then(Result::ok), None, "not in force");
        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        crate::route::set_stream_codecs(&mut ps, "h264", "truehd");
        let shape = crate::route::stream_pcm(&ps).and_then(Result::ok);
        assert_eq!(shape, Some(crate::route::PcmFormat::STEREO_48K));
        assert_eq!(audio_declaration("truehd", shape.is_some()), Some(("PCM", 5)));
        let on = build_av_payload_at(&ps, "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert_eq!(on.matches("pcmInfo").count(), 1, "{on}");
        let planned = load_audio("truehd", crate::player::RATE_NORMAL, shape.is_some(), None, AudioStack::Liblxa).unwrap();
        assert_eq!((planned.name, planned.lead), ("PCM", PcmLead::FromOutput(AudioStack::Liblxa)), "planned the stack's lead");
    }

    /// **An app-decoded lossy track at a playback speed still loads.** At a speed the speed decoder
    /// declares PCM itself and the app-decoded shape is dropped ([`app_pcm_for_rate`]), so `load_audio`
    /// is asked for a codec the pipeline has no payload name for (`mp3`, `vorbis`, `truehd`): the Load must still
    /// declare PCM and take the stack's lead, as a speed session on any other codec does, not be
    /// refused as an "unsupported audio codec". The `pcm` argument is derived exactly as
    /// `start_bufferfeed_inner` derives it, so this goes through the Load's own path (the speed
    /// assertion of `app_decoded_dts_is_declared_as_pcm_and_native_dts_as_dts` hands `"PCM"` straight
    /// to the payload builder and cannot see this).
    #[test]
    fn an_app_decoded_lossy_track_loads_as_pcm_at_a_speed() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        crate::route::begin_attempt_for_test(
            &mut ps, std::sync::Arc::new(plx_platform::devcaps::Caps::assumed().with_app_lossy()),
        );
        for codec in ["mp3", "vorbis", "truehd"] {
            crate::route::set_stream_codecs(&mut ps, "h264", codec);
            for rate in [500, 1500, 2000] {
                let pcm = app_pcm_for_rate(rate, crate::route::stream_pcm(&ps)).and_then(Result::ok);
                assert_eq!(pcm, None, "{codec} at {rate}: the speed decoder declares the shape");
                let planned = load_audio(codec, rate, pcm.is_some(), None, AudioStack::Liblxa)
                    .map(|a| (a.name, a.diagnostic, a.lead));
                assert_eq!(
                    planned,
                    Some(("PCM", 5, PcmLead::FromOutput(AudioStack::Liblxa))),
                    "{codec} at {rate}: a speed session declares PCM whatever the codec is called"
                );
            }
        }
    }

    #[test]
    fn only_a_pcm_load_carries_pcm_info() {
        let f = PcmFormat { sample_rate: 48_000, bits: 16, channels: 2, source_rate: 48_000 };
        assert_eq!(with_pcm_info(BASE, "AC3", Some(f)), BASE);
        assert_eq!(with_pcm_info(BASE, "PCM", None), BASE);
    }
}

/// **The webOS 10.3.1 refusal, replayed.** `docs/webos10-lab-report.md` §3.2 records the exact
/// callback order the pipeline produced when it refused the 4K60 H.264 envelope: `13/1`, `14/0`,
/// the `type=5` sink echo, `15/0`, `8/0`, then `18/601 Resource Allocation Error` — with `Load()`
/// itself having returned `ok=1`. §3.5 records what the app did about it: nothing. `load_failed`
/// stayed false, the state machine sat in Connecting, and no failure read-out was ever reached.
///
/// This test is that artifact, host-side, watched RED before the fix (2026-09-03): the red is the
/// lab set's measured sequence, NOT a reproduction of issue #63, whose set nobody here has seen a
/// log from. Under `hostsim` only because it borrows `prime_livelock_tests`' engine constructor.
/// The envelope rule, cell by cell. Every value here is either a measured declaration
/// (`ENVELOPE_UHD60`, `ENVELOPE_FHD60`) or today's behaviour; the test exists so the narrow rule
/// stays narrow — a "smarter" envelope that changes HEVC or the unknown-raster case fails here.
#[cfg(test)]
mod sink_envelope_tests {
    use super::*;

    fn caps(h264: (u32, u32, u32), hevc: (u32, u32, u32)) -> plx_platform::devcaps::Caps {
        let mut c = plx_platform::devcaps::Caps::assumed();
        c.h264_row = h264;
        c.hevc_row = hevc;
        c
    }
    /// The synthesized header is byte-for-byte the CodecPrivate of a real 5.1 Opus MKV track
    /// (`ffprobe -show_entries stream=extradata`, libopus surround layout), and stereo is the
    /// 19-byte family-0 form; a width SMP cannot be told about is refused, not guessed.
    #[test]
    fn opus_head_matches_a_real_surround_track_and_refuses_unknown_widths() {
        let real_51: [u8; 27] = [
            0x4f, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, 0x01, 0x06, 0x38, 0x01, 0x80, 0xbb,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x04, 0x02, 0x00, 0x04, 0x01, 0x02, 0x03, 0x05,
        ];
        assert_eq!(opus_head(6).unwrap(), real_51);
        let stereo = opus_head(2).unwrap();
        assert_eq!(stereo.len(), 19);
        assert_eq!(&stereo[8..10], &[1, 2]);
        assert_eq!(stereo[18], 0, "mapping family 0");
        for channels in 1..=8 {
            let h = opus_head(channels).unwrap();
            assert_eq!(h.len(), if channels <= 2 { 19 } else { 21 + channels as usize });
        }
        for bad in [0, -1, 9, 255] {
            assert_eq!(opus_head(bad), None);
        }
    }

    /// An OPUS Load declares the codec and carries exactly one `opusInfo` inside `contents`, and
    /// the result is still one valid JSON document.
    #[test]
    fn an_opus_load_carries_one_opus_info_node() {
        assert_eq!(audio_payload_codec("opus"), Some(("OPUS", 6)));
        let ps = crate::route::PlaybackSession::IDLE;
        let p = with_opus_info(&build_av_payload(&ps, "AV1", "OPUS", ENVELOPE_FHD60), 6).unwrap();
        assert_eq!(p.matches(r#""opusInfo""#).count(), 1);
        let v: serde_json::Value = serde_json::from_str(&p).expect("valid JSON");
        let contents = &v["args"][0]["option"]["externalStreamingInfo"]["contents"];
        assert_eq!(contents["codec"]["audio"], "OPUS");
        assert_eq!(contents["opusInfo"]["channels"], 6);
        assert_eq!(contents["opusInfo"]["streamHeader"], plx_base::b64::encode(&opus_head(6).unwrap()));
        assert_eq!(with_opus_info(&p, 0), None);
    }

    /// The speed menu offers only what the Load can declare: on a set whose measured H.264 row is
    /// 4K@30, a 24p stream at 1.5x is fed at 36 fps and the envelope clamps the declaration to 30,
    /// so 1.5x and above are not offered. Without a measured table, for a codec the Load cannot
    /// declare, or for a row with no rate, the flat [`SPEED_MAX_FPS`] ceiling stands.
    #[test]
    fn the_speed_offer_stops_at_the_measured_rows_frame_rate() {
        let row30 = plx_platform::devcaps::Caps {
            h264_row: (3840, 2160, 30),
            ..plx_platform::devcaps::Caps::assumed()
        };
        let offered = |fps: f64, video: Option<LoadVideo>, caps: &plx_platform::devcaps::Caps, measured: bool| {
            crate::player::speeds_within(fps, speed_fps_ceiling(video, caps, measured))
        };
        assert_eq!(
            offered(24.0, Some(LoadVideo::H264), &row30, true),
            vec![500, 750, 1000, 1250],
            "24p at 1.5x is fed at 36 fps, past a 30 fps row"
        );
        assert_eq!(
            offered(29.97, Some(LoadVideo::H264), &row30, true),
            vec![500, 750, 1000],
            "29.97p is within the 30 row at 1x only"
        );
        assert_eq!(
            offered(24.0, Some(LoadVideo::H265), &row30, true),
            crate::player::PLAY_RATES.to_vec(),
            "the H.264 row bounds H.264 only"
        );
        for (what, video, caps, measured) in [
            ("an unmeasured table", Some(LoadVideo::H264), &row30, false),
            ("a codec the Load cannot declare", None, &row30, true),
            (
                "a row with no rate",
                Some(LoadVideo::H264),
                &plx_platform::devcaps::Caps { h264_row: (3840, 2160, 0), ..plx_platform::devcaps::Caps::assumed() },
                true,
            ),
        ] {
            assert_eq!(speed_fps_ceiling(video, caps, measured), crate::player::SPEED_MAX_FPS, "{what}");
        }
        let row120 = plx_platform::devcaps::Caps {
            h264_row: (3840, 2160, 120),
            ..plx_platform::devcaps::Caps::assumed()
        };
        assert_eq!(
            speed_fps_ceiling(Some(LoadVideo::H264), &row120, true),
            crate::player::SPEED_MAX_FPS,
            "a faster row never raises the ceiling"
        );
    }
    #[test]
    fn audio_payload_names_dts_and_refuses_unsupported_formats() {
        assert_eq!(audio_payload_codec("dts"), Some(("DTS", 4)));
        assert_eq!(audio_payload_codec("eac3"), Some(("AC3 PLUS", 2)));
        // PMS's own spelling of DTS, and the other spellings the shared canonicaliser folds.
        assert_eq!(audio_payload_codec("dca"), Some(("DTS", 4)));
        assert_eq!(audio_payload_codec("DCA"), Some(("DTS", 4)));
        assert_eq!(audio_payload_codec("ec-3"), Some(("AC3 PLUS", 2)));
        assert_eq!(audio_payload_codec("flac"), Some(("PCM", 5)));
        assert_eq!(audio_payload_codec("FLAC"), Some(("PCM", 5)));
        // ALAC and every LPCM flavour the bundled FFmpeg decodes are fed as the same PCM
        for codec in ["alac", "ALAC", "pcm", "lpcm", "pcm_s16le", "pcm_s24le", "pcm_s32be"] {
            assert_eq!(audio_payload_codec(codec), Some(("PCM", 5)), "{codec}");
        }
        for codec in ["truehd", "", "unknown", "pcm_f32le", "pcm_bluray", "pcm_u8"] {
            assert_eq!(audio_payload_codec(codec), None);
        }
        let ps = crate::route::PlaybackSession::IDLE;
        let payload = build_av_payload(&ps, "H264", "DTS", ENVELOPE_FHD60);
        assert!(payload.contains(r#""audio":"DTS""#));
    }

    const DEV_SET: ((u32, u32, u32), (u32, u32, u32)) = ((4096, 2304, 60), (4096, 2176, 60));
    const FHD_SET: ((u32, u32, u32), (u32, u32, u32)) = ((1920, 1088, 60), (1920, 1088, 60));

    #[test]
    fn h264_declares_fhd_only_when_the_widest_raster_fits_fhd() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for (raster, want) in [
            ((720, 480), ENVELOPE_FHD60),
            ((1280, 720), ENVELOPE_FHD60),
            ((1920, 1080), ENVELOPE_FHD60),
            ((1920, 1088), ENVELOPE_FHD60), // coded height of a 1080p stream
            ((1918, 802), ENVELOPE_FHD60),  // scope film
            ((3840, 2160), ENVELOPE_UHD60),
            ((4096, 2176), ENVELOPE_UHD60),
            ((0, 0), ENVELOPE_UHD60),  // nobody said: today's value
            ((1920, 0), ENVELOPE_UHD60),
        ] {
            assert_eq!(sink_envelope(LoadVideo::H264, raster, 0.0, &c, true), want, "h264 {raster:?}");
            assert_eq!(
                sink_envelope(LoadVideo::H264, raster, 0.0, &c, false),
                want,
                "h264 {raster:?} unmeasured"
            );
        }
    }

    #[test]
    fn hevc_keeps_the_verified_4k60_on_a_4k_set_whatever_the_source() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for raster in [(720, 480), (1920, 1080), (3840, 2160), (0, 0)] {
            assert_eq!(sink_envelope(LoadVideo::H265, raster, 0.0, &c, true), ENVELOPE_UHD60, "{raster:?}");
            assert_eq!(sink_envelope(LoadVideo::H265, raster, 0.0, &c, false), ENVELOPE_UHD60, "{raster:?}");
        }
    }

    #[test]
    fn a_measured_full_hd_table_clamps_both_codecs_and_the_assumed_table_clamps_nothing() {
        let c = caps(FHD_SET.0, FHD_SET.1);
        assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), 0.0, &c, true), ENVELOPE_FHD60);
        assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), 0.0, &c, true), ENVELOPE_FHD60);
        assert_eq!(sink_envelope(LoadVideo::H265, (0, 0), 0.0, &c, true), ENVELOPE_FHD60);
        // the same rows, not measured: never a clamp
        assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), 0.0, &c, false), ENVELOPE_UHD60);
        assert_eq!(sink_envelope(LoadVideo::H265, (0, 0), 0.0, &plx_platform::devcaps::Caps::assumed(), true), ENVELOPE_UHD60);
        // a table stating a lower frame rate clamps it; a higher one never raises it
        let slow = caps((3840, 2160, 30), (3840, 2160, 120));
        assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), 0.0, &slow, true).fps, 30);
        assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), 0.0, &slow, true).fps, 60);
    }

    /// The device measurement behind the rate rule: 4K H.264 declared at 60 is re-announced at
    /// 30 by the pipeline; at its own class it is not. HEVC and an unknown rate keep 60.
    #[test]
    fn h264_declares_the_streams_rate_class_and_hevc_keeps_60() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for (fps, want) in [(24.0, 24), (23.976, 24), (25.0, 25), (29.97, 30), (30.0, 30), (50.0, 50), (59.94, 60), (60.0, 60), (120.0, 60), (0.0, 60)] {
            assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), fps, &c, true).fps, want, "h264 {fps}");
            assert_eq!(sink_envelope(LoadVideo::H264, (1920, 1080), fps, &c, true).fps, want, "h264 fhd {fps}");
            assert_eq!(sink_envelope(LoadVideo::H265, (3840, 2160), fps, &c, true).fps, 60, "hevc {fps}");
        }
        // the table still clamps a class from above
        let slow = caps((3840, 2160, 25), (3840, 2160, 60));
        assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), 30.0, &slow, true).fps, 25);
    }

    /// Issue #665: an MKV's 1 ms timebase stores a 30p frame as 33 ms, so PMS reports 30.303 and
    /// the strict class rounded it up to 50, which a 4K@30 H.264 decoder refuses. A rate within
    /// the timebase rounding error above a class snaps to it; a genuinely higher one still rounds up.
    #[test]
    fn a_millisecond_timebase_rate_snaps_to_its_class() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        for (fps, want) in [
            (30.303, 30), (24.39, 24), (23.81, 24), (25.0, 25), (50.0, 50), (58.82, 60), (59.94, 60),
            (31.0, 50), (26.0, 30),
        ] {
            assert_eq!(sink_envelope(LoadVideo::H264, (3840, 2160), fps, &c, false).fps, want, "h264 {fps}");
        }
    }

    /// Issue #665: the re-encode of a 4K source at 480p declares FHD60 for H.264.
    #[test]
    fn a_480p_ceiling_on_a_4k_source_declares_fhd60_for_h264() {
        let c = caps(DEV_SET.0, DEV_SET.1);
        assert_eq!(sink_envelope(LoadVideo::H264, (854, 480), 0.0, &c, false), ENVELOPE_FHD60);
    }

    #[test]
    fn the_payload_carries_all_three_envelope_numbers() {
        let ps = crate::route::PlaybackSession::IDLE;
        let p = build_av_payload(&ps, "H264", "AAC", ENVELOPE_FHD60);
        assert!(p.contains(r#""maxWidth":1920"#), "{p}");
        assert!(p.contains(r#""maxHeight":1080"#), "{p}");
        assert!(p.contains(r#""maxFrameRate":60"#), "{p}");
        let p = build_av_payload(&ps, "H265", "AC3 PLUS", ENVELOPE_UHD60);
        assert!(p.contains(r#""maxWidth":3840"#) && p.contains(r#""maxHeight":2160"#), "{p}");
        assert!(!p.contains(r#""maxFrameRate":30"#), "the template's 30 must be replaced: {p}");
    }

    /// A playback-speed Load declares PCM with its format, the sample rate in KILOHERTZ (libpf
    /// matches `48`; `48000` silently became 44.1 on the 3.9.3 set). Compressed audio carries no
    /// `pcmInfo` at all, so every playback without a speed is byte-for-byte what it was.
    #[test]
    fn a_pcm_load_declares_its_format_and_compressed_audio_does_not() {
        let ps = crate::route::PlaybackSession::IDLE;
        let p = build_av_payload(&ps, "H264", "PCM", ENVELOPE_FHD60);
        assert!(p.contains(r#""audio":"PCM""#), "{p}");
        assert!(
            p.contains(r#""pcmInfo":{"bitsPerSample":16,"sampleRate":48,"channelMode":"stereo"},"esInfo":"#),
            "{p}"
        );
        assert_eq!(p.matches("pcmInfo").count(), 1);
        for audio in ["AAC", "AC3", "AC3 PLUS", "DTS"] {
            assert!(!build_av_payload(&ps, "H264", audio, ENVELOPE_FHD60).contains("pcmInfo"), "{audio}");
        }
    }

    /// A real session playing 44.1 kHz / 16-bit stereo FLAC, as the route carries it.
    fn flac_session() -> crate::route::PlaybackSession {
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(crate::route::set_stream_declaration_for_test(
            &mut ps,
            "h264",
            "flac",
            0.0,
            plx_data::metadata::Dovi::NONE,
            false,
            plx_platform::devcaps::dv::DvCapability::Supported,
        ));
        crate::route::set_carried_audio_for_test(
            &mut ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "flac".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 44_100, bit_depth: 16,
            },
        );
        ps
    }

    /// The Load carries EXACTLY ONE `pcmInfo`. At 1x a FLAC session declares `audio:"PCM"` too
    /// (`audio_payload_codec`), and the speed decoder's fixed kilohertz node used to be inserted
    /// as well, so the Load held two nodes that disagreed on the rate's unit and the bit depth.
    #[test]
    fn a_flac_load_at_1x_carries_only_its_own_pcm_info() {
        let p = build_av_payload_at(&flac_session(), "H264", "PCM", ENVELOPE_FHD60, crate::player::RATE_NORMAL);
        assert_eq!(p.matches("pcmInfo").count(), 1, "{p}");
        assert!(
            p.contains(r#""pcmInfo":{"format":"S16LE","channelMode":"stereo","layout":"interleaved","sampleRate":44.1,"bitsPerSample":16}"#),
            "{p}"
        );
        assert!(!p.contains(r#""sampleRate":48,"#), "the speed decoder's node must be absent: {p}");
        serde_json::from_str::<serde_json::Value>(&p).unwrap();
    }

    /// At a speed the speed decoder owns the PCM, FLAC included: main's fixed node, and only it.
    #[test]
    fn a_flac_load_at_a_speed_carries_the_speed_decoders_pcm_info() {
        for rate in [500, 1500, 2000] {
            let p = build_av_payload_at(&flac_session(), "H264", "PCM", ENVELOPE_FHD60, rate);
            assert_eq!(p.matches("pcmInfo").count(), 1, "{rate}: {p}");
            assert!(
                p.contains(r#""pcmInfo":{"bitsPerSample":16,"sampleRate":48,"channelMode":"stereo"},"esInfo":"#),
                "{rate}: {p}"
            );
            assert!(!p.contains("S16LE"), "{rate}: {p}");
        }
    }

    /// Compressed audio carries no `pcmInfo`, whatever the session's own track is.
    #[test]
    fn compressed_audio_carries_no_pcm_info_even_on_a_flac_session() {
        for rate in [crate::player::RATE_NORMAL, 1500] {
            for audio in ["AAC", "AC3", "AC3 PLUS", "DTS"] {
                let p = build_av_payload_at(&flac_session(), "H264", audio, ENVELOPE_FHD60, rate);
                assert!(!p.contains("pcmInfo"), "{rate} {audio}: {p}");
            }
        }
    }

    /// At a speed the Load declares the rate actually fed, exactly: 23.976 at 1.5x is 36000/1001,
    /// and at 1x every declaration is what it was.
    #[test]
    fn a_speed_declares_the_fed_frame_rate() {
        assert_eq!(super::fed_fps_rational(23.976, 1000), Some((24000, 1001)));
        assert_eq!(super::fed_fps_rational(23.976, 1500), Some((36000, 1001)));
        assert_eq!(super::fed_fps_rational(25.0, 2000), Some((50, 1)));
        assert_eq!(super::fed_fps_rational(29.97, 500), Some((15000, 1001)));
        assert_eq!(super::fed_fps_rational(0.0, 1500), None);
        assert!((super::fed_fps(30.0, 2000) - 60.0).abs() < 1e-9);
    }

    /// The static payloads' declared envelopes, read out of the strings that are actually sent.
    #[test]
    fn the_static_envelopes_are_the_static_payloads_own_numbers() {
        for (video, payload) in [(LoadVideo::H264, PAYLOAD_V), (LoadVideo::H265, PAYLOAD_H265)] {
            let e = static_envelope(video);
            assert!(payload.contains(&format!(r#""maxWidth":{}"#, e.w)), "{video:?}");
            assert!(payload.contains(&format!(r#""maxHeight":{}"#, e.h)), "{video:?}");
            assert!(payload.contains(&format!(r#""maxFrameRate":{}"#, e.fps)), "{video:?}");
        }
    }

    #[test]
    fn the_sinkmax_override_parses_exactly_wxh_at_f() {
        assert_eq!(
            parse_sinkmax("1920x1080@30\n"),
            Some(SinkEnvelope { w: 1920, h: 1080, fps: 30 })
        );
        assert_eq!(parse_sinkmax("1920x1080"), None);
        assert_eq!(parse_sinkmax("0x1080@60"), None);
        assert_eq!(parse_sinkmax("abc"), None);
    }

    #[test]
    fn load_video_maps_exactly_today_plus_av1_and_vp9() {
        for (vcodec, want) in [
            ("hevc", LoadVideo::H265),
            ("h264", LoadVideo::H264),
            ("av1", LoadVideo::Av1),
            ("vp9", LoadVideo::Vp9),
            ("", LoadVideo::H264),
            ("HEVC", LoadVideo::H265),
            ("H264", LoadVideo::H264),
            ("h265", LoadVideo::H265),
        ] {
            assert_eq!(LoadVideo::of_stream(vcodec), Some(want), "{vcodec:?}");
        }
        // an unknown codec is refused, never declared as H.264
        for vcodec in ["mpeg2video", "vc1", "vp8", "theora", "mpeg4", "avc", "x"] {
            assert_eq!(LoadVideo::of_stream(vcodec), None, "{vcodec:?}");
        }
        let all = [LoadVideo::H264, LoadVideo::H265, LoadVideo::Av1, LoadVideo::Vp9];
        assert_eq!(all.map(LoadVideo::payload_name), ["H264", "H265", "AV1", "VP9"]);
        assert_eq!(all.map(LoadVideo::diag_code), [1, 2, 3, 4]);
    }

    #[test]
    fn vp9_envelope_follows_hevc_and_its_own_row() {
        let mut c = caps(DEV_SET.0, DEV_SET.1);
        c.vp9 = true;
        c.vp9_row = (3840, 2160, 30);
        assert_eq!(sink_envelope(LoadVideo::Vp9, (1920, 1080), 23.976, &c, false), ENVELOPE_UHD60);
        assert_eq!(
            sink_envelope(LoadVideo::Vp9, (1920, 1080), 23.976, &c, true),
            SinkEnvelope { w: 3840, h: 2160, fps: 30 }
        );
        c.vp9_row = (0, 0, 0);
        assert_eq!(sink_envelope(LoadVideo::Vp9, (1920, 1080), 23.976, &c, true), ENVELOPE_UHD60);
    }

    #[test]
    fn av1_envelope_follows_hevc_and_its_own_row() {
        let mut c = caps(DEV_SET.0, DEV_SET.1);
        c.av1 = true;
        c.av1_row = (3840, 2160, 30);
        assert_eq!(sink_envelope(LoadVideo::Av1, (1920, 1080), 23.976, &c, false), ENVELOPE_UHD60);
        assert_eq!(
            sink_envelope(LoadVideo::Av1, (1920, 1080), 23.976, &c, true),
            SinkEnvelope { w: 3840, h: 2160, fps: 30 }
        );
        c.av1_row = (0, 0, 0);
        assert_eq!(sink_envelope(LoadVideo::Av1, (1920, 1080), 23.976, &c, true), ENVELOPE_UHD60);
    }

    #[test]
    fn static_envelope_av1_and_vp9_are_uhd60() {
        assert_eq!(static_envelope(LoadVideo::Av1), ENVELOPE_UHD60);
        assert_eq!(static_envelope(LoadVideo::Vp9), ENVELOPE_UHD60);
        assert_eq!(static_envelope(LoadVideo::H265), ENVELOPE_UHD60);
        assert_eq!(static_envelope(LoadVideo::H264), SinkEnvelope { w: 1920, h: 1080, fps: 30 });
    }

    #[test]
    fn the_renamed_static_payloads_declare_their_codec() {
        assert_eq!(PAYLOAD_H265.matches(r#""video":"H265""#).count(), 1);
        for (video, name) in [(LoadVideo::Av1, "AV1"), (LoadVideo::Vp9, "VP9")] {
            let p = renamed_static_payload(video);
            assert!(p.contains(&format!(r#""video":"{name}""#)), "{p}");
            assert!(!p.contains("H265"), "{p}");
            assert!(p.contains(r#""maxWidth":3840"#) && p.contains(r#""maxHeight":2160"#), "{p}");
        }
    }

    #[test]
    fn load_video_refused_only_for_av1_and_vp9_without_their_rows() {
        let assumed = plx_platform::devcaps::Caps::assumed();
        assert!(load_video_refused(LoadVideo::Av1, &assumed));
        assert!(load_video_refused(LoadVideo::Vp9, &assumed));
        assert!(!load_video_refused(LoadVideo::H264, &assumed));
        assert!(!load_video_refused(LoadVideo::H265, &assumed));
        let with_row = plx_platform::devcaps::Caps {
            av1: true,
            vp9: true,
            ..plx_platform::devcaps::Caps::assumed()
        };
        for video in [LoadVideo::H264, LoadVideo::H265, LoadVideo::Av1, LoadVideo::Vp9] {
            assert!(!load_video_refused(video, &with_row), "{video:?}");
        }
        // one row never admits the other codec
        let av1_only = plx_platform::devcaps::Caps { av1: true, ..plx_platform::devcaps::Caps::assumed() };
        assert!(load_video_refused(LoadVideo::Vp9, &av1_only));
    }

    #[test]
    fn load_v_name_is_the_name_the_payload_declares() {
        use crate::player::load_v_name;
        assert_eq!(load_v_name(1), "H264");
        assert_eq!(load_v_name(2), "H265");
        assert_eq!(load_v_name(3), "AV1");
        assert_eq!(load_v_name(4), "VP9");
        assert_eq!(load_v_name(0), "—");
        for video in [LoadVideo::H264, LoadVideo::H265, LoadVideo::Av1, LoadVideo::Vp9] {
            assert_eq!(load_v_name(video.diag_code()), video.payload_name());
        }
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod load_refusal_tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// The strings the lab log carries at each callback, sanitised exactly as the report has them.
    const ECHO_SINK: &std::ffi::CStr =
        c"0 video/x-h264 (null) (null) 3840 2160 (null) 60.000000 0 0 0";
    const REFUSAL: &std::ffi::CStr = c"Resource Allocation Error";

    /// The Engine itself needs no cleanup since phase 9 — it lives in the test's own
    /// `PlayerAdapter`, which drops with the test. What is still process-wide is `FEED_SHARED` and the
    /// route reducer's projection.
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::player::reset_session_for_test();
            // A `Drop` takes no arguments and this test's own session is being dropped beside it;
            // the reducer only needs SOME idle projection to be put back.
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    #[test]
    fn an_asynchronous_load_refusal_is_a_verdict_not_a_black_screen() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        let _cleanup = Cleanup;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe {
            plx_base::task::MainThread::assume()
        });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        // The attempt's readout, as `open_begin` begins it: the pump samples the refusal into it.
        ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), crate::player::attempt_view::Backend::Feed);
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.native_epoch = epoch;
        pa.install(eng);

        // `Load()` returned ok=1 — nothing here says otherwise — and then the pipeline talked.
        plx_play_feed::sf_event::sf_on_event(epoch, 13, 1, c"".as_ptr());
        plx_play_feed::sf_event::sf_on_event(epoch, 14, 0, c"1".as_ptr());
        plx_play_feed::sf_event::sf_on_event(epoch, 5, 0, ECHO_SINK.as_ptr());
        plx_play_feed::sf_event::sf_on_event(epoch, 15, 0, c"1".as_ptr());
        plx_play_feed::sf_event::sf_on_event(epoch, 8, 0, c"audio/mpeg".as_ptr());
        plx_play_feed::sf_event::sf_on_event(epoch, 18, 601, REFUSAL.as_ptr());

        crate::player::pump::pump(&mut ps, &mut pa, 1_000);

        assert!(
            FEED_SHARED.load_failed.load(Ordering::Acquire),
            "type=18 num=601 before any picture must publish load_failed; the lab set sat on a \
             black screen for 70 s because it did not"
        );
        assert_eq!(
            super::super::state(&ps),
            crate::player::PlaybackState::Error,
            "the pump must turn a refused Load into the Error state the read-out renders from"
        );
        assert_eq!(
            super::super::error_now(&ps).kind,
            super::super::FailureKind::TvPipeline,
            "and the verdict must name the television's pipeline, not a generic stop"
        );
    }
}

#[cfg(test)]
mod audio_lead_in_tests {
    use super::{audio_lead_in, AUDIO_LEAD_IN_NS};

    #[test]
    fn the_opening_au_and_the_window_after_it_are_lead_in_and_nothing_else_is() {
        let start = 11_008_000_000;
        assert!(audio_lead_in(start, true, None), "the marked first AU opens the timeline");
        assert!(!audio_lead_in(start, false, None), "an unmarked AU with no start is not");
        assert!(audio_lead_in(start + AUDIO_LEAD_IN_NS - 1, false, Some(start)));
        assert!(!audio_lead_in(start + AUDIO_LEAD_IN_NS, false, Some(start)), "the window ends");
        assert!(!audio_lead_in(start - 1, false, Some(start)), "nothing before the start");
    }
}

/// **A film whose audio starts 11 s after its video must still start.** The device log of the
/// failing WEB-DL (E-AC-3 with an MKV `Delay` of 11.008 s): `SMP Play`, `feed v#1..v#4`, the
/// demuxer's `first audio at 11008ms`, then no `feed a#` line at all and `timeline playing t=0s`
/// for good. Starfish's clock waits for audio, so `pres_fed` stays 0 and the video lane stops
/// [`MAX_FEED_AHEAD_NS`] in; the audio lane dropped every AU as stale because each was more than
/// [`AUDIO_STALE_AHEAD_NS`] past that video. These tests drive the real lanes through
/// [`feed_both_lanes`] against the host feed seam, in the state a progressive start leaves:
/// Play already issued, nothing presented (`pres_fed` 0, not [`PRES_NONE`]).
#[cfg(all(test, feature = "hostsim"))]
mod late_audio_start_tests {
    use super::*;

    const V_STEP_NS: i64 = 41_708_333; // 23.976 fps
    const A_STEP_NS: i64 = 32_000_000; // E-AC-3: 1536 samples at 48 kHz
    const AUDIO_DELAY_NS: i64 = 11_008_000_000;

    /// Process-wide state these tests set, restored on the way out, panic or not.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
            FEED_SHARED.pres_fed.store(0, Ordering::Relaxed);
        }
    }

    /// An engine past `SMP Play` at offset 0 holding the film's first 12 s: video from 0, audio
    /// from [`AUDIO_DELAY_NS`]. `first_audio` is what the demuxer published as its first audio.
    fn started_with_late_audio(first_audio: i64) -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.prime_play = false;
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..(12_000_000_000 / V_STEP_NS) {
            plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, i * V_STEP_NS, c_int::from(i == 0), 1);
        }
        let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..200 {
            plx_play_feed::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, AUDIO_DELAY_NS + i * A_STEP_NS, 1, 2);
        }
        FEED_SHARED.first_audio_pts.store(first_audio, Ordering::Relaxed);
        eng
    }

    fn run_ticks(eng: &mut Engine) {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        for _ in 0..8 {
            feed_both_lanes(&mt, eng);
        }
    }

    fn arm() -> Restore {
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        FEED_SHARED.pres_fed.store(0, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        Restore
    }

    #[test]
    fn a_track_whose_audio_starts_late_reaches_the_sink() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        let mut eng = started_with_late_audio(AUDIO_DELAY_NS);
        run_ticks(&mut eng);

        // Precondition: the video lane fed and then stopped at the throttle, as on the device.
        assert!(
            eng.max_fed_video_pts > 0 && eng.max_fed_video_pts < AUDIO_DELAY_NS - AUDIO_STALE_AHEAD_NS,
            "PRECONDITION FAILED, not the defect: video fed to {}ns — the host sink is not armed \
             or the throttle did not hold the video lane short of the audio",
            eng.max_fed_video_pts
        );
        assert_eq!(eng.audio_start_fp, Some(AUDIO_DELAY_NS), "the first audio AU was fed");
        assert!(
            eng.max_fed_audio_pts >= AUDIO_DELAY_NS + AUDIO_LEAD_IN_NS - A_STEP_NS,
            "the lead-in was fed ahead of the clock (audio to {}ns)",
            eng.max_fed_audio_pts
        );
        // Past the lead-in the throttle holds the audio — held, not dropped as stale.
        let next = eng.pending_audio.as_ref().map(|n| unsafe { plx_play_feed::aq::au_fields(n.0) }.2);
        assert_eq!(next, Some(eng.max_fed_audio_pts + A_STEP_NS), "the next AU waits for the clock");
    }

    #[test]
    fn audio_far_ahead_that_does_not_open_the_timeline_is_still_dropped_as_stale() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        // The demuxer has not marked these AUs: stale pre-seek audio looks exactly like this.
        let mut eng = started_with_late_audio(i64::MIN);
        run_ticks(&mut eng);

        assert!(eng.max_fed_video_pts > 0, "PRECONDITION FAILED: the host sink fed no video");
        assert_eq!(eng.max_fed_audio_pts, 0, "stale audio must not reach the sink");
        assert_eq!(eng.audio_start_fp, None);
    }
}

/// **Issue #568: on webOS 5+ (`VP_EXPORTED`) every direct-play seek was a full fresh Starfish
/// `Load` (black screen, the DV/HDR badge re-popping).** `scrub: tap commit 905s` was followed by
/// `reload_at: fresh Load at 905s`, because the Kodi in-place path reaches LG-private objects and
/// was switched off there. The seek now goes through LG's public `StarfishMediaAPIs::Seek`.
///
/// These drive the real mode selection, the pump's reload-or-in-place decision and the real
/// [`feed_stream`] re-anchor against the host sink with the clock sink armed, which reports
/// `VP_EXPORTED` exactly as a webOS 5+ set does.
#[cfg(all(test, feature = "hostsim"))]
mod native_seek_tests {
    use plx_play_feed::shared::{FEED_SHARED, FEED_TX};
    use super::*;
    use plx_play_feed::ffi_host;

    const V_STEP_NS: i64 = 41_708_333;
    const TARGET_NS: i64 = 905_000_000_000;

    /// Process-wide state these tests set, restored on the way out, panic or not.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ffi_host::force_clocksink_for_test(false);
            ffi_host::reset_seek_probe_for_test(None);
            plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
            FEED_SHARED.pts_shift.store(0, Ordering::Relaxed);
            FEED_SHARED.seek_target_ns.store(-1, Ordering::Relaxed);
            FEED_SHARED.pres_fed.store(0, Ordering::Relaxed);
            FEED_TX.seek_to_ns.store(-1, Ordering::Relaxed);
        }
    }

    fn arm(force_seek: Option<c_int>) -> Restore {
        ffi_host::force_clocksink_for_test(true);
        ffi_host::reset_seek_probe_for_test(force_seek);
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_TX.seek_to_ns.store(-1, Ordering::Relaxed);
        Restore
    }

    /// An engine that has applied a direct-play seek and is waiting for the keyframe: flushed,
    /// rebase pending, the video queue holding a keyframe at `TARGET_NS` and the AUs after it.
    fn engine_seeking() -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.flushed = true;
        eng.rebase_pending = true;
        FEED_SHARED.seek_target_ns.store(TARGET_NS, Ordering::Relaxed);
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..60 {
            plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, TARGET_NS + i * V_STEP_NS, c_int::from(i == 0), 1);
        }
        eng
    }

    fn tick(eng: &mut Engine) {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        feed_both_lanes(&mt, eng);
    }

    /// The mode a set on release `major` (old-ABI Feed when `cow`) gets from a window of kind
    /// `window_mode`, composed from the same facts `select_seek_mode_for_load` composes.
    fn mode_for(major: u32, cow: bool, window_mode: c_int) -> SeekMode {
        select_seek_mode(inplace_layout_known(major, cow), old_abi_seek_tested(major), window_mode, None)
    }

    #[test]
    fn a_webos5_session_seeks_natively_and_never_reloads() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(None);
        let sink_mode = crate::player::test_physical::sink().window_mode();
        assert_eq!(sink_mode, crate::player::VP_EXPORTED, "PRECONDITION: the armed host sink is the webOS 5+ one");
        let mode = mode_for(5, false, sink_mode);
        assert_eq!(mode, SeekMode::NativeSeek, "VP_EXPORTED selects LG's public Seek");
        assert!(
            !crate::player::pump::seek_reloads(mode),
            "a mid-play direct-play seek on VP_EXPORTED must NOT be a fresh Load (#568)"
        );
    }

    /// The table of modes per firmware is `plx_play_feed::seek`'s test; what stays here is the
    /// pump's reading of them: only `Reload` is a fresh `Load`.
    #[test]
    fn only_the_reload_mode_reloads_the_pipeline_on_a_seek() {
        assert!(crate::player::pump::seek_reloads(SeekMode::Reload));
        assert!(!crate::player::pump::seek_reloads(SeekMode::Segment));
        assert!(!crate::player::pump::seek_reloads(SeekMode::NativeSeek));
    }

    /// A speed is applied only to a freshly loaded pipeline, so a seek that carries a speed
    /// change, and every seek while the speed is not 1x, reloads in EVERY mode; at 1x with no
    /// change pending each mode keeps its own answer.
    #[test]
    fn a_speed_makes_every_direct_play_seek_a_reload() {
        use crate::player::pump::seek_must_reload;
        let normal = crate::player::RATE_NORMAL;
        for mode in [SeekMode::Reload, SeekMode::Segment, SeekMode::NativeSeek] {
            assert!(seek_must_reload(mode, 1500, false), "{mode:?} at 1.5x");
            assert!(seek_must_reload(mode, 500, false), "{mode:?} at 0.5x");
            assert!(seek_must_reload(mode, normal, true), "{mode:?}: the change back to 1x reloads too");
            assert_eq!(seek_must_reload(mode, normal, false), crate::player::pump::seek_reloads(mode), "{mode:?} at 1x");
        }
    }

    #[test]
    fn the_landed_keyframe_is_the_native_seek_and_nothing_flushes() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::NativeSeek);
        let mut eng = engine_seeking();
        tick(&mut eng);

        assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "Seek(<keyframe ms>), once");
        assert_eq!(ffi_host::flush_calls_for_test(), 0, "Seek flushes itself; a flush after it re-anchors at 0");
        assert!(!eng.flushed && !eng.rebase_pending, "the seek is applied");
        assert_eq!(FEED_SHARED.pts_shift.load(Ordering::Relaxed), 0, "content time, no rebase");
        assert!(eng.max_fed_video_pts >= TARGET_NS, "the keyframe and the AUs after it were fed on the Seek's timeline");
        assert_eq!(plx_play_feed::seek::seek_mode(), SeekMode::NativeSeek);
    }

    #[test]
    fn a_pre_zero_keyframe_seeks_to_zero_not_negative() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::NativeSeek);
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.flushed = true;
        eng.rebase_pending = true;
        FEED_SHARED.seek_target_ns.store(0, Ordering::Relaxed);
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, -80_000_000, 1, 1);
        tick(&mut eng);
        assert_eq!(ffi_host::seek_log_for_test(), vec![0], "Seek refuses a negative millis");
    }

    #[test]
    fn a_refused_seek_reloads_this_one_and_every_later_one() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm(Some(0));
        plx_play_feed::seek::set_seek_mode(SeekMode::NativeSeek);
        let mut eng = engine_seeking();
        tick(&mut eng);

        assert_eq!(plx_play_feed::seek::seek_mode(), SeekMode::Reload, "native seek is off for the rest of the session");
        assert!(crate::player::pump::seek_reloads(plx_play_feed::seek::seek_mode()));
        assert_eq!(
            FEED_TX.seek_to_ns.load(Ordering::Acquire),
            TARGET_NS,
            "this seek is re-posted at the user's target for the pump's reload"
        );
        assert_eq!(eng.max_fed_video_pts, 0, "nothing is fed into a pipeline that refused the seek");
        assert!(eng.pending_video.is_none());
        assert_eq!(ffi_host::flush_calls_for_test(), 0);
    }
}

/// **App-decoded (PCM) audio must reach the sink later than the picture it belongs with** (#1086,
/// #1088): the set renders a PCM Load through a mixer channel that lacks the lip sync its own
/// decoder gets (400 ms, less the active output's own delay: [`PCM_SINK_LEAD_NS`]). The correction
/// is the audio lane's sink pts and nothing else.
#[cfg(test)]
mod pcm_sink_lead_pure_tests {
    use super::*;

    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            FEED_SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        }
    }

    #[test]
    fn no_lead_is_the_pipeline_timeline_and_a_lead_is_real_time_after_the_speed_division() {
        let _serial = plx_base::testlock::serial();
        let _restore = Restore;
        FEED_SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(7_000_000_000, 0), plx_play_feed::shared::to_sink_ns(7_000_000_000));
        assert_eq!(audio_sink_pts(7_000_000_000, PCM_SINK_LEAD_NS), 7_000_000_000 + PCM_SINK_LEAD_NS);

        FEED_SHARED.applied_rate_permille.store(1500, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(3_000_000_000, 0), plx_play_feed::shared::to_sink_ns(3_000_000_000));
        assert_eq!(
            audio_sink_pts(3_000_000_000, 400_000_000),
            2_000_000_000 + 400_000_000,
            "film time is divided by the speed, the set's 400 ms is not"
        );
        FEED_SHARED.applied_rate_permille.store(500, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(3_000_000_000, 400_000_000), 6_000_000_000 + 400_000_000);
    }

    #[test]
    fn a_sink_pts_never_goes_below_zero_or_overflows() {
        let _serial = plx_base::testlock::serial();
        let _restore = Restore;
        FEED_SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        assert_eq!(audio_sink_pts(0, 0), 0);
        assert_eq!(audio_sink_pts(-500_000_000, 400_000_000), 0, "a unit before the origin is not sent negative");
        assert_eq!(audio_sink_pts(100_000_000, -400_000_000), 0, "a negative dev override clamps too");
        assert_eq!(audio_sink_pts(i64::MAX - 1, PCM_SINK_LEAD_NS), i64::MAX);
    }

    /// The stacks with no lead at all: 2016/2017's `adecsink`, Realtek and MStar (`Other`), and a set
    /// nobody probed.
    const NO_LEAD_STACKS: [AudioStack; 3] = [AudioStack::AdecSink, AudioStack::Other, AudioStack::Unknown];
    /// The stacks whose lead follows the sound output (so it is read on the Load thread).
    const OUTPUT_STACKS: [AudioStack; 2] = [AudioStack::Liblxa, AudioStack::AudioSinkNew];
    const ALL_STACKS: [AudioStack; 6] = [
        AudioStack::Liblxa,
        AudioStack::LgAudioSink,
        AudioStack::AudioSinkNew,
        AudioStack::AdecSink,
        AudioStack::Other,
        AudioStack::Unknown,
    ];

    #[test]
    fn only_a_pcm_load_gets_a_lead_and_the_dev_override_moves_only_that() {
        let liblxa = AudioStack::Liblxa;
        assert_eq!(PCM_SINK_LEAD_NS, 400_000_000);
        assert_eq!(pcm_lead_plan("PCM", None, liblxa), PcmLead::FromOutput(liblxa));
        assert_eq!(pcm_lead_plan("PCM", Some("0"), liblxa), PcmLead::Override(0, liblxa));
        assert_eq!(pcm_lead_plan("PCM", Some(" 200\n"), liblxa), PcmLead::Override(200_000_000, liblxa));
        assert_eq!(pcm_lead_plan("PCM", Some("-50"), liblxa), PcmLead::Override(-50_000_000, liblxa), "an experiment may go the other way");
        assert_eq!(pcm_lead_plan("PCM", Some("loud"), liblxa), PcmLead::FromOutput(liblxa), "garbage is no override: the output decides");
        assert_eq!(pcm_lead_plan("PCM", Some(""), liblxa), PcmLead::FromOutput(liblxa));
        let new2018 = AudioStack::AudioSinkNew;
        assert_eq!(pcm_lead_plan("PCM", None, new2018), PcmLead::FromOutput(new2018), "2018 follows the output and carries its own stack");
        assert_eq!(pcm_lead_plan("PCM", Some("150"), new2018), PcmLead::Override(150_000_000, new2018));
        assert_eq!(pcm_lead_plan("PCM", Some("loud"), new2018), PcmLead::FromOutput(new2018));
        let adec = AudioStack::AdecSink;
        assert_eq!(pcm_lead_plan("PCM", None, adec), PcmLead::NoLead(adec), "2016/2017: no lead");
        assert_eq!(pcm_lead_plan("PCM", Some("300"), adec), PcmLead::Override(300_000_000, adec), "pcmavoffset is how it gets measured");
        let sink = AudioStack::LgAudioSink;
        assert_eq!(pcm_lead_plan("PCM", None, sink), PcmLead::Fixed(sink), "the output does not change the lead there: no read");
        assert_eq!(pcm_lead_plan("PCM", Some("0"), sink), PcmLead::Override(0, sink), "the override wins on every stack");
        assert_eq!(pcm_lead_plan("PCM", Some("loud"), sink), PcmLead::Fixed(sink));
        for stack in ALL_STACKS {
            for compressed in ["DTS", "AC3", "AC3 PLUS", "AAC", "OPUS", "-"] {
                assert_eq!(pcm_lead_plan(compressed, None, stack), PcmLead::Off, "{compressed} on {stack:?}");
                assert_eq!(pcm_lead_plan(compressed, Some("400"), stack), PcmLead::Off, "{compressed} on {stack:?}: the override is PCM-only");
            }
        }
    }

    /// The table, one cell per stack x output: the filmed outputs of the `liblxa` stack, one nobody
    /// filmed, no answer; 2018's the same figures, none of them measured; every output of the LG audio
    /// sink (one lead); every output of a stack with no lead.
    #[test]
    fn the_lead_follows_the_stack_and_then_the_sound_output() {
        let liblxa = AudioStack::Liblxa;
        assert_eq!(pcm_output_lead(liblxa, Some("tv_speaker")), (400_000_000, true), "filmed: -0.03 to +0.08 s");
        assert_eq!(pcm_output_lead(liblxa, Some("bt_soundbar")), (250_000_000, true), "filmed: +0.017 s at 250 ms, +0.13 to +0.19 s at 400");
        for unmeasured in ["external_optical", "external_arc", "headphone", "lg_sound_sync_optical", "other"] {
            assert_eq!(pcm_output_lead(liblxa, Some(unmeasured)), (PCM_SINK_LEAD_NS, false), "liblxa + {unmeasured}");
        }
        assert_eq!(pcm_output_lead(liblxa, None), (PCM_SINK_LEAD_NS, false), "no answer keeps the liblxa default");

        let y2018 = AudioStack::AudioSinkNew;
        assert_eq!(pcm_output_lead(y2018, Some("tv_speaker")), (400_000_000, false), "2018: 475 less the +75 bias, read from firmware");
        assert_eq!(pcm_output_lead(y2018, Some("bt_soundbar")), (250_000_000, false), "2018: the decoder's -150 for a wireless output, decompile only");
        for unmeasured in ["external_optical", "external_arc", "headphone", "lg_sound_sync_optical", "other"] {
            assert_eq!(pcm_output_lead(y2018, Some(unmeasured)), (PCM_SINK_LEAD_NS, false), "2018 + {unmeasured}");
        }
        assert_eq!(pcm_output_lead(y2018, None), (PCM_SINK_LEAD_NS, false), "2018 + no answer");

        for output in [Some("tv_speaker"), Some("bt_soundbar"), Some("external_arc"), None] {
            assert_eq!(
                pcm_output_lead(AudioStack::LgAudioSink, output),
                (PCM_SINK_LEAD_NS, false),
                "lgaudiosink + {output:?}: 400 ms on every output, Bluetooth included, never measured"
            );
        }
        for stack in NO_LEAD_STACKS {
            for output in [Some("tv_speaker"), Some("bt_soundbar"), Some("external_arc"), None] {
                assert_eq!(pcm_output_lead(stack, output), (0, false), "{stack:?} + {output:?}: the behaviour before #1087");
            }
        }
        assert_eq!(PCM_LEAD_BY_OUTPUT.iter().filter(|(name, _)| *name == "tv_speaker").count(), 1);
    }

    /// Resolution is where the output is read, and the only place: the read is a closure, so what
    /// asks for it is countable.
    #[test]
    fn the_output_is_read_only_for_a_load_that_needs_it() {
        let reads = std::cell::Cell::new(0);
        let read = |answer: Option<&str>| {
            let reads = &reads;
            let answer = answer.map(str::to_string);
            move || {
                reads.set(reads.get() + 1);
                answer
            }
        };
        let off = PcmLead::Off.resolve(read(Some("bt_soundbar")));
        assert_eq!((off.ns, off.output), (0, None));
        assert_eq!(reads.get(), 0, "a compressed Load never asks");

        for stack in ALL_STACKS {
            let overridden = PcmLead::Override(330_000_000, stack).resolve(read(Some("bt_soundbar")));
            assert_eq!((overridden.ns, overridden.output), (330_000_000, None), "{stack:?}: the override wins on any stack");
        }
        assert_eq!(reads.get(), 0, "the dev override wins over the table without asking");

        for stack in NO_LEAD_STACKS {
            let unmeasured = PcmLead::NoLead(stack).resolve(read(Some("bt_soundbar")));
            assert_eq!((unmeasured.ns, unmeasured.output), (0, None), "{stack:?}");
        }
        assert_eq!(reads.get(), 0, "a stack with no lead does not pay for the bus read");

        let sink = PcmLead::Fixed(AudioStack::LgAudioSink).resolve(|| panic!("the LG audio sink's lead does not depend on the output"));
        assert_eq!((sink.ns, sink.output), (PCM_SINK_LEAD_NS, None));

        for stack in OUTPUT_STACKS {
            let before = reads.get();
            let bt = PcmLead::FromOutput(stack).resolve(read(Some("bt_soundbar")));
            assert_eq!((bt.ns, bt.output.as_deref()), (250_000_000, Some("bt_soundbar")), "{stack:?}");
            assert_eq!(reads.get(), before + 1, "a PCM Load on {stack:?} asks once");
            let speaker = PcmLead::FromOutput(stack).resolve(read(Some("tv_speaker")));
            assert_eq!(speaker.ns, 400_000_000, "{stack:?}");
            let optical = PcmLead::FromOutput(stack).resolve(read(Some("external_optical")));
            assert_eq!((optical.ns, optical.output.as_deref()), (PCM_SINK_LEAD_NS, Some("external_optical")), "{stack:?}");
            let none = PcmLead::FromOutput(stack).resolve(read(None));
            assert_eq!((none.ns, none.output), (PCM_SINK_LEAD_NS, None), "{stack:?}");
            assert_eq!(reads.get(), before + 4, "{stack:?}");
        }
    }

    /// The same, from the declaration: the plan a PCM Load gets on each stack, resolved with a read
    /// that counts, asks only on a stack whose lead follows the output (`liblxa`, 2018); the LG audio
    /// sink settles 400 ms without asking, and the stacks with no lead never ask.
    #[test]
    fn a_pcm_load_asks_for_the_output_only_on_a_stack_whose_lead_follows_it() {
        for stack in ALL_STACKS {
            let reads = std::cell::Cell::new(0);
            let lead = load_audio("flac", crate::player::RATE_NORMAL, false, None, stack).unwrap().lead;
            let resolved = lead.resolve(|| {
                reads.set(reads.get() + 1);
                Some("bt_soundbar".to_string())
            });
            if OUTPUT_STACKS.contains(&stack) {
                assert_eq!((reads.get(), resolved.ns), (1, 250_000_000), "{stack:?}");
            } else if stack == AudioStack::LgAudioSink {
                assert_eq!((reads.get(), resolved.ns), (0, PCM_SINK_LEAD_NS), "{stack:?}");
            } else {
                assert_eq!((reads.get(), resolved.ns), (0, 0), "{stack:?}");
            }
        }
    }

    #[test]
    fn a_resolved_lead_says_what_it_decided_and_whether_it_was_measured() {
        let line = |plan: PcmLead, answer: Option<&str>| {
            plan.resolve(|| answer.map(str::to_string)).log_line()
        };
        let said = |text: &str| Some(text.to_string());
        let liblxa = AudioStack::Liblxa;
        assert_eq!(line(PcmLead::FromOutput(liblxa), Some("tv_speaker")), said("pcm: audio sink pts +400ms (stack liblxa, output tv_speaker, default 400ms)"));
        assert_eq!(line(PcmLead::FromOutput(liblxa), Some("bt_soundbar")), said("pcm: audio sink pts +250ms (stack liblxa, output bt_soundbar, default 400ms)"));
        assert_eq!(line(PcmLead::FromOutput(liblxa), Some("external_arc")), said("pcm: audio sink pts +400ms (stack liblxa, output external_arc, default 400ms, not measured)"));
        assert_eq!(line(PcmLead::FromOutput(liblxa), None), said("pcm: audio sink pts +400ms (stack liblxa, output unknown, default 400ms, not measured)"));
        let y2018 = PcmLead::FromOutput(AudioStack::AudioSinkNew);
        assert_eq!(line(y2018, Some("tv_speaker")), said("pcm: audio sink pts +400ms (stack audiosink_new, output tv_speaker, default 400ms, not measured)"));
        assert_eq!(line(y2018, Some("bt_soundbar")), said("pcm: audio sink pts +250ms (stack audiosink_new, output bt_soundbar, default 400ms, not measured)"));
        assert_eq!(line(y2018, None), said("pcm: audio sink pts +400ms (stack audiosink_new, output unknown, default 400ms, not measured)"));
        assert_eq!(line(PcmLead::NoLead(AudioStack::AdecSink), None), said("pcm: audio sink pts +0ms (stack adecsink, not measured on this chip)"));
        assert_eq!(line(PcmLead::Override(330_000_000, AudioStack::AudioSinkNew), None), said("pcm: audio sink pts +330ms (pcmavoffset override, stack audiosink_new)"));
        assert_eq!(
            line(PcmLead::Fixed(AudioStack::LgAudioSink), None),
            said("pcm: audio sink pts +400ms (stack lgaudiosink, every output, not measured)")
        );
        assert_eq!(line(PcmLead::NoLead(AudioStack::Other), None), said("pcm: audio sink pts +0ms (stack other, not measured on this chip)"));
        assert_eq!(line(PcmLead::NoLead(AudioStack::Unknown), None), said("pcm: audio sink pts +0ms (stack unknown, not measured on this chip)"));
        assert_eq!(line(PcmLead::Override(330_000_000, AudioStack::Other), None), said("pcm: audio sink pts +330ms (pcmavoffset override, stack other)"));
        assert_eq!(line(PcmLead::Override(330_000_000, AudioStack::Liblxa), None), said("pcm: audio sink pts +330ms (pcmavoffset override, stack liblxa)"));
        assert_eq!(line(PcmLead::Off, None), None, "a compressed Load says nothing");
        assert_eq!(
            PcmLead::Override(330_000_000, AudioStack::Other).start_log_line(),
            said("pcm: audio sink pts +330ms (pcmavoffset override, stack other)")
        );
        assert_eq!(
            PcmLead::NoLead(AudioStack::Other).start_log_line(),
            said("pcm: audio sink pts +0ms (stack other, not measured on this chip)"),
            "the frame thread settles it, so it says so now"
        );
        assert_eq!(
            PcmLead::Fixed(AudioStack::LgAudioSink).start_log_line(),
            said("pcm: audio sink pts +400ms (stack lgaudiosink, every output, not measured)"),
            "settled on the frame thread, so it says so now, once"
        );
        assert_eq!(
            PcmLead::NoLead(AudioStack::AdecSink).start_log_line(),
            said("pcm: audio sink pts +0ms (stack adecsink, not measured on this chip)")
        );
        for stack in OUTPUT_STACKS {
            assert_eq!(PcmLead::FromOutput(stack).start_log_line(), None, "{stack:?}: the Load thread logs it, with the output it read");
        }
        assert_eq!(PcmLead::Off.start_log_line(), None);
    }

    /// The Load's declared audio and the plan for its lead come out of ONE value ([`load_audio`], the
    /// pair `start_bufferfeed_inner` puts in the payload and on the Engine), speed override included:
    /// a 1.5x session on an AC3 track declares PCM and takes the lead, a 1x AC3 session does not, a
    /// 1x FLAC session does. Only the shape of the plan follows the stack: a lead that follows the
    /// output, the LG audio sink's one lead, or no lead at all.
    #[test]
    fn the_engines_lead_follows_the_declared_audio_the_speed_override_and_the_stack() {
        use crate::player::RATE_NORMAL;
        let plan = |codec: &str, rate: i32| load_audio(codec, rate, false, None, AudioStack::Liblxa).map(|a| (a.name, a.lead));
        let from_output = PcmLead::FromOutput(AudioStack::Liblxa);
        assert_eq!(plan("flac", RATE_NORMAL), Some(("PCM", from_output)));
        assert_eq!(plan("alac", RATE_NORMAL), Some(("PCM", from_output)));
        assert_eq!(plan("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)));
        assert_eq!(plan("dca", RATE_NORMAL), Some(("DTS", PcmLead::Off)), "the set's own DTS decoder keeps its own compensation");
        assert_eq!(plan("eac3", RATE_NORMAL), Some(("AC3 PLUS", PcmLead::Off)));
        assert_eq!(plan("ac3", 1500), Some(("PCM", from_output)), "a speed session declares PCM, so it takes the lead");
        assert_eq!(plan("dca", 500), Some(("PCM", from_output)));
        assert_eq!(plan("flac", 1500), Some(("PCM", from_output)));
        assert_eq!(plan("not-a-codec", RATE_NORMAL), None, "an undeclarable codec still refuses the Load");
        // At a speed the codec's name is not declared at all, so it cannot refuse: the guard is upstream,
        // at every Load (`player::session_rate_for` -> `speed_offer` -> `ff::speed_decoder_available`).
        assert_eq!(plan("not-a-codec", 1500), Some(("PCM", from_output)), "a speed session declares PCM whatever the codec is called");
        let sink = AudioStack::LgAudioSink;
        let on_sink = |codec: &str, rate: i32| load_audio(codec, rate, false, None, sink).map(|a| (a.name, a.lead));
        assert_eq!(on_sink("flac", RATE_NORMAL), Some(("PCM", PcmLead::Fixed(sink))));
        assert_eq!(on_sink("ac3", 1500), Some(("PCM", PcmLead::Fixed(sink))), "a speed session declares PCM on this stack too");
        assert_eq!(on_sink("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)), "a compressed Load keeps the set's own decoder");
        let y2018 = AudioStack::AudioSinkNew;
        let on_2018 = |codec: &str, rate: i32| load_audio(codec, rate, false, None, y2018).map(|a| (a.name, a.lead));
        assert_eq!(on_2018("flac", RATE_NORMAL), Some(("PCM", PcmLead::FromOutput(y2018))));
        assert_eq!(on_2018("ac3", 1500), Some(("PCM", PcmLead::FromOutput(y2018))));
        assert_eq!(on_2018("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)));
        for stack in NO_LEAD_STACKS {
            let on = |codec: &str, rate: i32| load_audio(codec, rate, false, None, stack).map(|a| (a.name, a.lead));
            assert_eq!(on("flac", RATE_NORMAL), Some(("PCM", PcmLead::NoLead(stack))), "{stack:?}");
            assert_eq!(on("ac3", 1500), Some(("PCM", PcmLead::NoLead(stack))), "{stack:?}");
            assert_eq!(on("ac3", RATE_NORMAL), Some(("AC3", PcmLead::Off)), "{stack:?}");
        }
    }

    /// **App-decoded DTS** (S4, the lossy gate on) declares `"audio":"PCM"` and so takes the lead of the
    /// set's stack exactly like FLAC; the same track left to the set's own decoder (`a="DTS"`) gets none.
    /// `load_audio` is given the shape the session declared (`route::stream_pcm`), so the name the
    /// payload carries and the lead plan cannot disagree.
    #[test]
    fn app_decoded_dts_takes_the_stacks_lead_and_native_dts_none() {
        use crate::player::RATE_NORMAL;
        for stack in ALL_STACKS {
            let app = load_audio("dts", RATE_NORMAL, true, None, stack).unwrap();
            let like_flac = load_audio("flac", RATE_NORMAL, true, None, stack).unwrap();
            assert_eq!((app.name, app.diagnostic), ("PCM", 5), "{stack:?}");
            assert_eq!(app.lead, like_flac.lead, "{stack:?}: the same plan as a FLAC Load");
            assert_eq!(app.lead, pcm_lead_plan("PCM", None, stack), "{stack:?}");
            let native = load_audio("dts", RATE_NORMAL, false, None, stack).unwrap();
            assert_eq!((native.name, native.lead), ("DTS", PcmLead::Off), "{stack:?}: a compressed Load gets none");
        }
        assert_eq!(
            load_audio("dts", RATE_NORMAL, true, None, AudioStack::Liblxa).unwrap().lead,
            PcmLead::FromOutput(AudioStack::Liblxa)
        );
    }

    /// The per-Load override reaches the Engine's lead through the same value, for a PCM Load only,
    /// and on every stack (it is how another chip gets measured).
    #[test]
    fn the_dev_override_reaches_the_engines_lead_through_the_declaration() {
        use crate::player::RATE_NORMAL;
        for stack in ALL_STACKS {
            assert_eq!(load_audio("flac", RATE_NORMAL, false, Some("330"), stack).unwrap().lead, PcmLead::Override(330_000_000, stack));
            assert_eq!(load_audio("ac3", 1500, false, Some("0"), stack).unwrap().lead, PcmLead::Override(0, stack));
            assert_eq!(load_audio("ac3", RATE_NORMAL, false, Some("330"), stack).unwrap().lead, PcmLead::Off);
        }
        let flac = load_audio("flac", RATE_NORMAL, false, None, AudioStack::Liblxa).unwrap();
        assert_eq!(flac.diagnostic, 5, "the diagnostics code is the same pair's");
    }

    /// What the frame thread can settle itself and what has to wait for the Load thread's read.
    #[test]
    fn only_a_lead_that_follows_the_output_waits_for_the_load_thread() {
        assert_eq!(PcmLead::Off.at_engine_start(), (0, false));
        assert_eq!(PcmLead::Override(330_000_000, AudioStack::Other).at_engine_start(), (330_000_000, false));
        assert_eq!(PcmLead::NoLead(AudioStack::Other).at_engine_start(), (0, false), "nothing to wait for: no lead");
        assert_eq!(
            PcmLead::Fixed(AudioStack::LgAudioSink).at_engine_start(),
            (PCM_SINK_LEAD_NS, false),
            "the lead is there from the start and nothing is pending"
        );
        for stack in OUTPUT_STACKS {
            assert_eq!(PcmLead::FromOutput(stack).at_engine_start(), (0, true), "{stack:?}: the audio waits for the Load thread's read");
        }
        for stack in NO_LEAD_STACKS {
            assert_eq!(PcmLead::NoLead(stack).at_engine_start(), (0, false), "{stack:?}");
        }
    }
}

/// The line from the set's audio stack to the Engine's lead, through the production start itself:
/// `start_bufferfeed_inner` reads `devcaps::audio_stack()` and hands it to `load_audio`. A start that
/// stopped passing it on (or passed a fixed one) leaves every other test green, because they all
/// build the plan or the Engine by hand.
#[cfg(all(test, feature = "hostsim"))]
mod pcm_stack_wiring_tests {
    use super::*;
    use plx_platform::devcaps::{audio_stack_on_this_thread_for_test, AudioStack};

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// Start a FLAC Load (declares `"audio":"PCM"`) on a set whose stack is `stack`, let its Load
    /// thread return, take what the Engine took from it, and give back `(pending before the take,
    /// lead after it)`. `unload_ms` is how long the host `Unload` of this start's stop takes, and
    /// `linger_ms` how long the Load is left returned before the stop (a late thread of an earlier
    /// start has that long to reach it).
    fn lead_of_a_flac_start_on(stack: AudioStack, unload_ms: u64, linger_ms: u64) -> (bool, i64) {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _stack = audio_stack_on_this_thread_for_test(stack);
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        crate::route::set_url(&mut ps, "http://127.0.0.1:1/track.flac");
        assert!(ps.qualify_resolved_feed_offer(crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        crate::route::set_stream_acodec(&mut ps, "flac");
        crate::route::set_carried_audio_for_test(
            &mut ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "flac".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 48_000, bit_depth: 16,
            },
        );

        let started = start_bufferfeed_tracked(&mut ps, &mut pa);
        assert!(
            matches!(started, BufferfeedStartOutcome::Launched(_)),
            "PRECONDITION FAILED: the FLAC start must launch ({started:?})"
        );
        let mut eng = pa.engine().expect("a launched start installs its Engine");
        assert!(
            eng.payload.to_string_lossy().contains("PCM"),
            "PRECONDITION FAILED: a FLAC Load must declare PCM: {:?}",
            eng.payload
        );
        let pending = eng.pcm_lead_pending;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !FEED_SHARED.native_load_returned(eng.native_epoch) {
            assert!(std::time::Instant::now() < deadline, "the Load never returned");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(settle_pcm_lead(&mut eng), "a returned Load has settled its lead");
        let lead = eng.pcm_sink_lead_ns;
        std::thread::sleep(std::time::Duration::from_millis(linger_ms));
        assert!(
            FEED_SHARED.native_load_returned(eng.native_epoch),
            "a Load that returned stays returned until its stop: another session's Unload reached this one"
        );
        drop(eng);
        plx_play_feed::ffi_host::delay_unload_for_test(unload_ms);
        stop_bufferfeed(&mut ps, &mut pa);
        finish_release_for_test(&mut pa);
        (pending, lead)
    }

    /// The route a launched FLAC start needs: a source URL, its qualified offer and the carried audio.
    fn flac_route(ps: &mut crate::route::PlaybackSession) {
        crate::route::set_url(ps, "http://127.0.0.1:1/track.flac");
        assert!(ps.qualify_resolved_feed_offer(crate::route::backend_policy::OfferId::FEED_ORIGINAL,
            crate::route::CandidatePurpose::NewCandidate));
        crate::route::set_stream_acodec(ps, "flac");
        crate::route::set_carried_audio_for_test(
            ps,
            crate::route::CarriedAudio {
                sid: 7, ordinal: 1, codec: "flac".into(), channels: 2,
                can_normalize_loudness: false, immersive: false, sample_rate: 48_000, bit_depth: 16,
            },
        );
    }

    /// A transcode restart's `Rebase` (`reload_transcode`) is visible to the very next position read: the
    /// start that follows samples the attempt it mints, so the readout carries the offset as the
    /// position and the display base with no pump between, and the side reader a start captures reads
    /// the commanded base while `raw` (idle after the teardown) says 0.
    #[test]
    fn a_transcode_restarts_rebase_is_visible_to_the_next_position_read_in_the_same_frame() {
        const OFFSET_NS: i64 = 600_000_000_000;
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _stack = audio_stack_on_this_thread_for_test(AudioStack::Liblxa);
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        flac_route(&mut ps);
        assert_eq!(ps.readout().raw().playpos_ns, 0, "no attempt has sampled the feed yet");

        let outcome = reload_transcode(&mut ps, &mut pa, OFFSET_NS);
        assert_eq!(outcome, ReloadOutcome::Started, "PRECONDITION FAILED: the restart must launch");
        assert_eq!(ps.readout().raw().playpos_ns, OFFSET_NS, "the position read after the restart is the offset");
        assert_eq!(ps.readout().raw().disp_base, OFFSET_NS, "and the display base is the offset");
        assert_eq!(crate::player::subside::capture_offset_ns(&ps), OFFSET_NS);

        plx_play_feed::ffi_host::delay_unload_for_test(0);
        stop_bufferfeed(&mut ps, &mut pa);
        finish_release_for_test(&mut pa);
        assert_eq!(ps.readout().open_disp_base_ns(), 0, "a stop retires the commanded base with the session");
    }

    /// The session's audio choice is the readout's, and reaches the feed's demuxer copy at the start
    /// the demuxer spawns in: a writer with no adapter in hand (the route's landing) is heard by the
    /// readout alone, and a reload keeps the choice on both sides.
    #[test]
    fn a_start_hands_the_feed_the_sessions_audio_choice_and_a_reload_keeps_it() {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let _stack = audio_stack_on_this_thread_for_test(AudioStack::Liblxa);
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        flac_route(&mut ps);
        FEED_SHARED.desired_audio_idx.store(-1, Ordering::Relaxed);

        crate::player::set_audio_track(&mut ps, 4);
        assert_eq!(ps.readout().audio_choice(), 4, "the route's writer stepped the session's readout");
        assert_eq!(crate::player::feed_audio_idx_for_test(), -1, "and the feed's copy waits for the start");

        let started = start_bufferfeed_tracked(&mut ps, &mut pa);
        assert!(matches!(started, BufferfeedStartOutcome::Launched(_)), "PRECONDITION FAILED: the start must launch ({started:?})");
        assert_eq!(crate::player::feed_audio_idx_for_test(), 4, "the start sent the choice before the demuxer spawned");

        // A reload tears the session down and starts again: the choice is on both sides throughout.
        plx_play_feed::ffi_host::delay_unload_for_test(0);
        suspend_bufferfeed(&mut ps, &mut pa);
        finish_release_for_test(&mut pa);
        assert_eq!(ps.readout().audio_choice(), 4, "a session reset keeps the pick");
        assert_eq!(crate::player::feed_audio_idx_for_test(), 4, "and so does the feed's own reset");
    }

    /// `liblxa` and 2018's stack yield a lead for a PCM Load once the Load thread has read the output
    /// (the hostsim bus has no answer, so the 400 ms default), pending until then; the LG audio sink
    /// has its 400 ms from the start with nothing pending; 2016/2017's `adecsink`, any other stack and
    /// one nobody probed yield none and have nothing to wait for.
    #[test]
    fn a_pcm_start_gets_the_lead_its_audio_stack_has_and_none_where_it_has_none() {
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Liblxa, 0, 0), (true, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::AudioSinkNew, 0, 0), (true, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::LgAudioSink, 0, 0), (false, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::AdecSink, 0, 0), (false, 0));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Other, 0, 0), (false, 0));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Unknown, 0, 0), (false, 0));
    }

    /// A stop hands its object to an `sf-unload` worker, and the host `Unload` names the object by
    /// the process-global active epoch. The worker of one start that runs late (a loaded CI runner)
    /// therefore unloads whichever session is active when it gets there: the next start's. This
    /// makes the worker late on purpose: the next start's Load returns, and must still be returned
    /// after the earlier worker's `Unload` has had its time. (The start helper used to drop its
    /// adapter, and with it the handle of the parked release, without `finish_release`.)
    #[test]
    fn an_earlier_starts_late_unload_never_reaches_the_next_start() {
        const UNLOAD_MS: u64 = 150;
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Liblxa, UNLOAD_MS, 0), (true, PCM_SINK_LEAD_NS));
        assert_eq!(lead_of_a_flac_start_on(AudioStack::Other, 0, 2 * UNLOAD_MS), (false, 0));
    }
}

#[cfg(all(test, feature = "hostsim"))]
mod pcm_sink_lead_tests {
    use super::*;
    use plx_play_feed::ffi_host;

    const V_STEP_NS: i64 = 41_708_333; // 23.976 fps
    const A_STEP_NS: i64 = 32_000_000;
    const VIDEO_AUS: i64 = 30;
    const AUDIO_AUS: i64 = 40;
    /// Where the in-place seek and the reload land, in content time.
    const T_NS: i64 = 905_000_000_000;

    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ffi_host::force_clocksink_for_test(false);
            ffi_host::reset_seek_probe_for_test(None);
            plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
            FEED_SHARED.pts_shift.store(0, Ordering::Relaxed);
            FEED_SHARED.seek_target_ns.store(-1, Ordering::Relaxed);
            FEED_SHARED.pres_fed.store(0, Ordering::Relaxed);
            FEED_SHARED.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
            FEED_SHARED.applied_rate_permille.store(crate::player::RATE_NORMAL, Ordering::Relaxed);
        }
    }

    fn arm() -> Restore {
        ffi_host::force_clocksink_for_test(true);
        ffi_host::reset_seek_probe_for_test(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
        FEED_SHARED.pres_fed.store(PRES_NONE, Ordering::Relaxed);
        FEED_SHARED.seek_to_ns.store(-1, Ordering::Relaxed);
        FEED_SHARED.pts_shift.store(0, Ordering::Relaxed);
        FEED_SHARED.first_audio_pts.store(i64::MIN, Ordering::Relaxed);
        Restore
    }

    /// A started engine (`lead` as the Load chose it) holding a keyframe at `at_ns` and the AUs
    /// after it, and audio from the same instant.
    fn engine_at(at_ns: i64, lead: i64) -> Engine {
        let mut eng = super::prime_livelock_tests::engine_after_reload();
        eng.prime_play = false;
        eng.pcm_sink_lead_ns = lead;
        let au = [0u8; 64];
        let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..VIDEO_AUS {
            plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, at_ns + i * V_STEP_NS, c_int::from(i == 0), 1);
        }
        let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
        for i in 0..AUDIO_AUS {
            plx_play_feed::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, at_ns + i * A_STEP_NS, 1, 2);
        }
        eng
    }

    fn tick(eng: &mut Engine) {
        let mt = unsafe { plx_base::task::MainThread::assume() };
        feed_both_lanes(&mt, eng);
    }

    /// What the sink saw, with the preconditions asserted so a refused feed cannot pass for a pass.
    fn first_feeds() -> (i64, i64, Vec<i64>) {
        let v = ffi_host::feed_log_for_test(1);
        let a = ffi_host::feed_log_for_test(2);
        assert!(
            !v.is_empty() && !a.is_empty(),
            "PRECONDITION FAILED, not the defect: the host sink saw v={} a={} feeds, so it is not armed",
            v.len(),
            a.len()
        );
        (v[0], a[0], a)
    }

    /// Audio minus video at the first Feed of each, within the 1 ns the video lane's nudge moves it.
    fn assert_lead(eng: &Engine, lead: i64, what: &str) {
        let (v0, a0, audio) = first_feeds();
        let gap = a0 - v0;
        assert!((gap - lead).abs() <= 1, "{what}: audio is {gap} ns after the picture, want {lead} ns");
        assert!(
            audio.windows(2).all(|w| w[1] - w[0] > 0),
            "{what}: the lead is one constant, so audio sink pts still ascend: {audio:?}"
        );
        // Bookkeeping stays in content time: the sink saw the lead, the high-water mark did not.
        assert_eq!(
            eng.max_fed_audio_pts,
            audio.last().unwrap() - lead,
            "{what}: max_fed_audio_pts is content time, without the lead"
        );
    }

    fn pcm_first_load(lead: i64) -> Engine {
        let mut eng = engine_at(0, lead);
        tick(&mut eng);
        eng
    }

    #[test]
    fn a_pcm_load_plays_its_first_audio_a_lead_after_the_picture_on_a_first_load() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        let eng = pcm_first_load(PCM_SINK_LEAD_NS);
        assert_lead(&eng, PCM_SINK_LEAD_NS, "first Load");
    }

    #[test]
    fn a_pcm_load_keeps_the_lead_after_a_reload_rebase() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        let mut eng = engine_at(T_NS, PCM_SINK_LEAD_NS);
        eng.rebase_pending = true;
        tick(&mut eng);
        assert_eq!(FEED_SHARED.pts_shift.load(Ordering::Relaxed), -T_NS, "PRECONDITION: the reload rebased the keyframe to 0");
        assert_lead(&eng, PCM_SINK_LEAD_NS, "reload rebase");
    }

    #[test]
    fn a_pcm_load_keeps_the_lead_after_an_in_place_segment_seek() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
        let mut eng = engine_at(T_NS, PCM_SINK_LEAD_NS);
        eng.flushed = true;
        eng.rebase_pending = true;
        FEED_SHARED.seek_target_ns.store(T_NS, Ordering::Relaxed);
        tick(&mut eng);
        assert!(!eng.rebase_pending, "PRECONDITION: the seek's keyframe was accepted");
        assert_eq!(FEED_SHARED.pts_shift.load(Ordering::Relaxed), 0, "PRECONDITION: content time, no rebase");
        assert_lead(&eng, PCM_SINK_LEAD_NS, "in-place Segment seek");
    }

    #[test]
    fn a_compressed_load_gets_no_lead_anywhere() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        assert_lead(&pcm_first_load(0), 0, "first Load");
        ffi_host::reset_seek_probe_for_test(None);

        let mut eng = engine_at(T_NS, 0);
        eng.rebase_pending = true;
        tick(&mut eng);
        assert_lead(&eng, 0, "reload rebase");
        ffi_host::reset_seek_probe_for_test(None);

        let mut eng = engine_at(T_NS, 0);
        eng.flushed = true;
        eng.rebase_pending = true;
        FEED_SHARED.seek_target_ns.store(T_NS, Ordering::Relaxed);
        tick(&mut eng);
        assert_lead(&eng, 0, "in-place Segment seek");
    }

    /// A lead that follows the sound output is read on the Load thread (the frame thread may not
    /// block on the bus) and handed over WITH the Load's return. Until then the Engine feeds the
    /// picture and holds the audio back: stamping it with a guessed lead and moving it afterwards
    /// would jump the audio timeline inside one Load.
    #[test]
    fn audio_waits_for_the_output_lead_then_takes_exactly_that() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        crate::player::reset_session_for_test();
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        let mut eng = engine_at(0, 0);
        eng.native_epoch = epoch;
        eng.pcm_lead_pending = true;
        tick(&mut eng);
        assert!(!ffi_host::feed_log_for_test(1).is_empty(), "PRECONDITION: the picture is fed");
        assert!(ffi_host::feed_log_for_test(2).is_empty(), "no audio before the Load thread's lead is published");
        assert!(eng.pcm_lead_pending);

        assert!(FEED_SHARED.mark_native_load_returned(epoch, Some(250_000_000)).is_some());
        tick(&mut eng);
        assert!(!eng.pcm_lead_pending, "taken once, at the first feed after the Load returned");
        assert_eq!(eng.pcm_sink_lead_ns, 250_000_000);
        assert_lead(&eng, 250_000_000, "a Bluetooth Load");
        crate::player::reset_session_for_test();
    }

    #[test]
    fn the_load_thread_publishes_the_lead_it_resolved_with_the_loads_return() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        crate::player::reset_session_for_test();
        ffi_host::reset_native_lifecycle_for_test();
        let load = |lead: PcmLead| {
            let epoch = FEED_SHARED.begin_native_session().expect("native session");
            assert_eq!(FEED_SHARED.native_load_pcm_lead(epoch), None, "nothing before the Load returns");
            crate::player::test_physical::load_thread(plx_play_feed::SendPtr(c"{}".as_ptr() as *mut c_char), epoch, None, lead);
            assert!(FEED_SHARED.native_load_returned(epoch), "PRECONDITION: the Load returned");
            let published = FEED_SHARED.native_load_pcm_lead(epoch);
            crate::player::reset_session_for_test();
            ffi_host::reset_native_lifecycle_for_test();
            published
        };
        // A host has no platform to ask, so "no answer" is what a PCM Load resolves to here.
        assert_eq!(load(PcmLead::FromOutput(AudioStack::Liblxa)), Some(PCM_SINK_LEAD_NS));
        assert_eq!(load(PcmLead::FromOutput(AudioStack::AudioSinkNew)), Some(PCM_SINK_LEAD_NS));
        assert_eq!(load(PcmLead::Off), None, "a Load that needs no read publishes none");
        assert_eq!(load(PcmLead::Override(330_000_000, AudioStack::Liblxa)), None, "an override is settled before the thread starts");
        assert_eq!(load(PcmLead::NoLead(AudioStack::Other)), None, "a stack with no lead is settled before the thread starts");
        assert_eq!(load(PcmLead::NoLead(AudioStack::AdecSink)), None, "2016/2017: no lead, no read");
        assert_eq!(load(PcmLead::Fixed(AudioStack::LgAudioSink)), None, "a lead that ignores the output is settled before the thread starts");
    }

    /// At 1.5x the picture's pts and the audio's both run on the divided timeline, and the lead on
    /// top of it is still the output's, not scaled.
    #[test]
    fn the_lead_is_not_scaled_at_a_speed() {
        let _serial = plx_base::testlock::serial();
        let _restore = arm();
        FEED_SHARED.applied_rate_permille.store(1500, Ordering::Relaxed);
        let mut eng = engine_at(T_NS, PCM_SINK_LEAD_NS);
        eng.flushed = true;
        eng.rebase_pending = true;
        FEED_SHARED.seek_target_ns.store(T_NS, Ordering::Relaxed);
        tick(&mut eng);
        let (v0, a0, audio) = first_feeds();
        assert_eq!(a0, plx_play_feed::shared::to_sink_ns(T_NS) + PCM_SINK_LEAD_NS);
        assert!((a0 - v0 - PCM_SINK_LEAD_NS).abs() <= 1, "audio is {} ns after the picture", a0 - v0);
        let last_content = plx_play_feed::shared::from_sink_ns(audio.last().unwrap() - PCM_SINK_LEAD_NS);
        assert!(
            (eng.max_fed_audio_pts - last_content).abs() <= 2,
            "max_fed_audio_pts {} is content time, the sink's last audio gives {last_content}",
            eng.max_fed_audio_pts
        );
    }
}

/// A start whose source no route plan resolved: the dev triggers (`plxnative-playurl`, `-url`, the
/// local samples) name the stream themselves inside `feed_open::resolve_source`, so no `apply_plan`
/// ever qualified a Feed recipe for it, while `start_bufferfeed_inner` refuses a Start without one
/// ("no qualified feed offer for this resolved route"). These tests drive the production start the
/// way the dev arm does (`route::clear_url`, then the start) and read what it answered. Every other
/// start test in this file qualifies its recipe BY HAND (`qualify_resolved_feed_offer`) before it
/// starts, which is why none of them could see the hole.
#[cfg(all(test, feature = "hostsim"))]
mod explicit_source_start_tests {
    use super::*;
    use crate::route::backend_policy::OfferId;

    /// The pipeline tier's own playurl content (`tests/run.py`), verbatim but for the host.
    const PLAYURL: &str = r#"{"vcodec":"h264","acodec":"aac","fps":24.0,"url":"http://127.0.0.1:1/__abr/ladder/master.m3u8"}"#;

    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// One dev start of the armed `playurl`, as `dev::scenarios::autoplay_arm` makes it.
    fn start_playurl(ps: &mut crate::route::PlaybackSession,
        pa: &mut crate::player::adapter::PlayerAdapter) -> BufferfeedStartOutcome {
        crate::route::clear_url(ps);
        start_bufferfeed_tracked(ps, pa)
    }

    fn stop(ps: &mut crate::route::PlaybackSession, pa: &mut crate::player::adapter::PlayerAdapter) {
        stop_bufferfeed(ps, pa);
        crate::player::route_start::finish_release_for_test(pa);
    }

    /// Run `body` over a fresh session and adapter with the `playurl` trigger armed in a private root.
    fn with_playurl_armed(body: impl FnOnce(&mut crate::route::PlaybackSession,
        &mut crate::player::adapter::PlayerAdapter)) {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let _serial = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        crate::route::reset_player_control_for_test(&ps);
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let _cleanup = Cleanup;
        let mut pa = crate::player::adapter::PlayerAdapter::new_probed(unsafe { plx_base::task::MainThread::assume() });
        plx_base::devtrig::with_private_triggers(|| {
            std::fs::write(plx_base::devtrig::path("playurl"), PLAYURL).unwrap();
            body(&mut ps, &mut pa);
        });
    }

    /// The observed failure: a boot with no session (or a Home with one) arms `playurl`, the arm
    /// clears the route's URL and starts, and the start was refused before the trigger was read.
    #[test]
    fn a_playurl_start_no_route_plan_resolved_still_launches() {
        with_playurl_armed(|ps, pa| {
            let started = start_playurl(ps, pa);
            assert!(matches!(started, BufferfeedStartOutcome::Launched(_)),
                "the playurl fixture's start was refused ({started:?})");
            assert!(pa.is_live(), "a launched start builds the Engine");
            assert_eq!(ps.planned_feed_offer(), Some(OfferId::FEED_ORIGINAL),
                "an explicit source hands its bytes over as they are: the original recipe");
            stop(ps, pa);
        });
    }

    /// **`is_live` is the adapter's bound attempt, so a start that returns before it binds must
    /// leave no Engine installed.** `start_bufferfeed_prepared` returns early on any `Rejected` or
    /// `CommandRefused` for its attempt. A `Rejected` Start never reaches the driver; a refused
    /// dispatch (the Feed driver's `dispatch`: no staged open, a busy slot, a failed launch) returns
    /// before `launch_native_open` installs, which is its last step; and one `send_at(Start)`
    /// dispatches no follow-on command (the startup plan waits at `Ready` for the Load's own
    /// observation), so no refusal of a later command can reach that exit with an Engine standing.
    /// This drives the refused dispatch through the adapter, as the start does, with nothing
    /// staged for it.
    #[test]
    fn a_start_the_driver_refuses_leaves_no_engine_installed() {
        with_playurl_armed(|ps, pa| {
            let attempt = crate::route::RouteStartAttempt::fixture().contract_id();
            let source = pa.next_source_key();
            let at = pa.backend_tick();
            let result = pa.backend_send_at(ps, plx_playback_contract::Intent::Start {
                backend: plx_playback_contract::BackendKind::Feed,
                attempt,
                source,
                requirements: plx_playback_contract::Requirements {
                    source: plx_playback_contract::SourceKind::PacketFeed,
                    video: plx_playback_contract::Fidelity::Exact,
                    audio: plx_playback_contract::Fidelity::Exact,
                    audio_track: plx_playback_contract::TrackChoice::Unspecified,
                    subtitle_track: plx_playback_contract::TrackChoice::Unspecified,
                },
                initial: plx_playback_contract::InitialPlayback { position: None, paused: false },
            }, crate::player::attempt_metrics::Origin::Fresh, at);
            assert!(result.playback.iter().any(|fx| matches!(fx,
                plx_playback_contract::Fx::Rejected { attempt: id, .. }
                | plx_playback_contract::Fx::CommandRefused { attempt: id, .. } if *id == attempt)),
                "PRECONDITION FAILED: the Start must be refused: {:?}", result.playback);
            assert!(result.controls.is_empty(), "a refused Start dispatches nothing after it");
            assert!(pa.engine().is_none(), "a refused Start installs no Engine");
            assert!(!pa.is_live(), "and binds no attempt");
        });
    }

    /// LG App Self Checklist #46: the finished stream starts again (`maybe_replay_after_eos`: the
    /// arm re-fires and clears the URL). The first play spent the catalog's original recipe; the
    /// second is a new request and begins its own.
    #[test]
    fn a_playurl_replay_after_the_first_play_launches_again() {
        with_playurl_armed(|ps, pa| {
            let first = start_playurl(ps, pa);
            assert!(matches!(first, BufferfeedStartOutcome::Launched(_)), "first start {first:?}");
            stop(ps, pa);
            let second = start_playurl(ps, pa);
            assert!(matches!(second, BufferfeedStartOutcome::Launched(_)),
                "the replay was refused ({second:?}): the first play's spent catalog outlived it");
            stop(ps, pa);
        });
    }

    /// `pipe_replay_after_eos` on the set: the replay arrives while the finished play's off-frame
    /// `Unload` is still running, so the start is deferred into the release machine and must run
    /// when the release completes. The in-between tests above wait for the release before they
    /// start again, which is why none of them could see a deferred start that is lost.
    #[test]
    fn a_playurl_replay_deferred_behind_the_unload_launches_when_the_release_completes() {
        with_playurl_armed(|ps, pa| {
            let first = start_playurl(ps, pa);
            assert!(matches!(first, BufferfeedStartOutcome::Launched(_)), "first start {first:?}");
            plx_play_feed::ffi_host::delay_unload_for_test(50);
            stop_bufferfeed(ps, pa);
            assert!(pa.has_pending_release(), "the stop must leave its Unload running");
            let second = start_playurl(ps, pa);
            assert_eq!(second, BufferfeedStartOutcome::Deferred, "the replay over a running Unload");
            pa.watch_queued_start();
            let give_up = std::time::Instant::now() + std::time::Duration::from_secs(5);
            let landing = loop {
                poll_release(ps, pa, std::time::Instant::now());
                match pa.take_queued_landing() {
                    crate::player::adapter::QueuedLanding::Waiting => {
                        assert!(std::time::Instant::now() < give_up, "the deferred replay never ran");
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    other => break other,
                }
            };
            assert!(matches!(landing, crate::player::adapter::QueuedLanding::Launched(_)),
                "the release completed but the deferred replay did not launch ({landing:?})");
            assert!(pa.is_live(), "a launched replay builds the Engine");
            stop(ps, pa);
        });
    }

    /// The cause of `pipe_replay_after_eos` on the set. A play that ends is stopped with its
    /// end-of-stream still standing in the attempt view (`teardown` publishes the physical facts, `ended`
    /// among them, before `crate::player::reset_session_for_test()` clears them). The replay's page mounts while its
    /// start is deferred behind the old `Unload`, with no new attempt yet to replace those facts, and
    /// the loop's end-of-stream rule (`app::run::playback_tick`: player page up and `ended` true)
    /// exits the player again: that stop cancels the deferred start, so no second Load ever comes.
    #[test]
    fn a_stopped_play_leaves_no_end_of_stream_for_the_deferred_replay_to_find() {
        with_playurl_armed(|ps, pa| {
            let first = start_playurl(ps, pa);
            assert!(matches!(first, BufferfeedStartOutcome::Launched(_)), "first start {first:?}");
            // The pump's end of stream: the readout the app's end-of-film rule polls.
            ps.step_readout(crate::player::readout::ReadoutEvent::StreamEnded);
            assert_eq!(crate::player::ended(ps), Some(true), "PRECONDITION FAILED: the film has ended");
            plx_play_feed::ffi_host::delay_unload_for_test(50);
            stop_bufferfeed(ps, pa);
            assert!(pa.has_pending_release(), "the stop must leave its Unload running");
            let second = start_playurl(ps, pa);
            assert_eq!(second, BufferfeedStartOutcome::Deferred, "the replay over a running Unload");
            assert_ne!(crate::player::ended(ps), Some(true),
                "the finished play's end of stream outlived its stop: the replay's page would exit it again");
            stop(ps, pa);
        });
    }

    /// A seek that escalates (or any reload) comes back through the same door with the route's URL
    /// already installed: it reopens the recipe the start admitted, and must not qualify a new
    /// candidate (that would clear the admission it reopens from).
    #[test]
    fn a_reload_of_a_playurl_source_reopens_the_admitted_recipe() {
        with_playurl_armed(|ps, pa| {
            let first = start_playurl(ps, pa);
            assert!(matches!(first, BufferfeedStartOutcome::Launched(_)), "first start {first:?}");
            assert!(ps.backend_planned_feed_allowed(), "the admitted recipe stays reopenable");
            let epoch = pa.engine().map(|eng| eng.native_epoch);
            assert_eq!(reload_at(ps, pa, 5_000_000_000), ReloadOutcome::Started);
            assert_eq!(ps.planned_feed_offer(), Some(OfferId::FEED_ORIGINAL));
            let reloaded = pa.engine().map(|eng| eng.native_epoch);
            assert!(reloaded.is_some() && reloaded != epoch, "the reload built a new Engine ({epoch:?} -> {reloaded:?})");
            stop(ps, pa);
        });
    }
}
