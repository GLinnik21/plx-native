//! The Dolby Vision landing guard driven through the real `pump()` (#1166): a declared landing the
//! firmware's splitter would skip (the row), or that no position update follows (the watch), is one
//! `reload_at` without the node. A sibling of `native_seek_pump_tests` and built on its `Rig`; every
//! case builds the guard the way production does, from the route's plan (`feed_open::dv_landing_plan`)
//! through the Feed's constructor (`Guard::from_plan`), and plants the firmware row the way production
//! reaches it: through the ATTEMPT's caps (`begin_attempt_for_test`), never the per-thread firmware
//! override, which applies no rows. The pump reaches the guard only through `FeedLoad::dv_landing_due`
//! and `FeedCommand::DvStandDown`. `reload_at` has no route in a unit rig, so each call is read off
//! `engine::RELOAD_AT_LOG`.
//!
//! The last group is the guard beside the native surround-audio watchdog (`native_stall`): both read the
//! same silence (a pipeline that accepts nothing and shows no picture), and only the Dolby Vision guard
//! may act on it once the prime has released Play, so a declared play with native 5.1 Opus withdraws the
//! node and never writes the install-wide learned Opus refusal.

use super::native_seek_pump_tests::{preroll_at_reload, reloads, Rig, TARGET_NS, V_STEP_NS};
use super::*;
use plx_data::metadata::Dovi;
use plx_platform::devcaps::{apply_exclusions, dv::DvCapability, Caps, EXCLUSIONS};
use plx_play_feed::dv_start::{Guard, Mode};
use plx_play_feed::native_audio::{self, Kind};
use plx_play_feed::native_stall::{Watch, NATIVE_AUDIO_STALL_MS, WATCH};
use plx_play_feed::{aq, ffi_host};
use std::os::raw::c_int;
use std::sync::atomic::Ordering::Relaxed;

/// An HEVC CRA as an access unit's `key` (`aq::AuNode::key`).
const CRA: c_int = 21;
const IDR: c_int = aq::AU_KEY;
/// A tap taken while a flight is out, later than the seek that was armed.
const TAP_NS: i64 = 1_200_000_000_000;

fn p81() -> Dovi {
    Dovi { present: true, profile: 8, bl_compat: 1, el_present: false, ..Dovi::NONE }
}

fn p5() -> Dovi {
    Dovi { present: true, profile: 5, bl_compat: 0, el_present: false, ..Dovi::NONE }
}

/// The caps of the CX-family set the row is for (`jhericurl-jervisbay`), applied as the boot probe does.
fn row_caps() -> std::sync::Arc<Caps> {
    let mut c = Caps::assumed();
    apply_exclusions(&mut c, "jhericurl-jervisbay", false, EXCLUSIONS);
    std::sync::Arc::new(c)
}

/// Build the Engine's guard the way `start_bufferfeed` and the Feed's Engine constructor do: the route's
/// plan, then the Feed's guard from it, armed with the seek armed for the open.
fn arm_guard(rig: &mut Rig, armed: Option<i64>) {
    let guard = Guard::from_plan(super::super::feed_open::dv_landing_plan(&rig.ps), armed);
    rig.eng().dv_start = guard;
}

/// A declared direct play of `dovi`, on a set with the row when `row`.
fn declared(dovi: Dovi, row: bool) -> Rig {
    let mut rig = Rig::new(None);
    assert!(crate::route::set_stream_declaration_for_test(
        &mut rig.ps,
        "hevc",
        "eac3",
        23.976,
        dovi,
        false,
        DvCapability::Supported,
    ));
    if row {
        crate::route::begin_attempt_for_test(&mut rig.ps, row_caps());
    }
    arm_guard(&mut rig, None);
    rig
}

fn is_declared(rig: &Rig) -> bool {
    crate::route::stream_dv_presentation(&rig.ps).declared().is_some()
}

fn frames(rig: &mut Rig, n: usize) {
    for _ in 0..n {
        rig.frame();
    }
}

/// Advance the pump clock by `ms`, a frame per 250 ms.
fn advance(rig: &mut Rig, ms: u32) {
    for _ in 0..ms / 250 {
        rig.now += 250;
        rig.frame();
    }
}

/// An in-place seek to `TARGET_NS` whose landing keyframe carries `key`, then enough frames for the
/// pump to meet it, feed what follows and release the prime.
fn seek_and_land(rig: &mut Rig, key: c_int) {
    rig.arm(TARGET_NS);
    rig.land_keyframe_with_key(TARGET_NS, 90, key);
    frames(rig, 12);
}

/// The event log of what `f` did on this thread.
fn logged<R>(f: impl FnOnce() -> R) -> String {
    plx_base::eventlog::with_private_log(|| {
        f();
        std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
    })
}

#[test]
fn a_native_seek_landing_on_a_cra_under_the_cx_row_reloads_once_without_dolby_vision() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    seek_and_land(&mut rig, CRA);
    assert!(ffi_host::seek_log_for_test().is_empty(), "the keyframe the splitter would skip is not fed, so no native Seek");
    assert_eq!(reloads(), vec![TARGET_NS], "one reload at the seek's target");
    assert_eq!(crate::route::stream_dv_presentation(&rig.ps), plx_data::metadata::DvPresentation::NotDv);
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn the_same_landing_on_an_idr_seeks_in_place_as_before() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    seek_and_land(&mut rig, IDR);
    assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "one native Seek, at the keyframe");
    assert!(reloads().is_empty());
    assert!(is_declared(&rig), "an IDR is what the splitter takes: Dolby Vision stays");
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_resume_landing_on_a_cra_reloads_once_at_the_armed_target() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    // A fresh Engine with a seek armed for its first open (a resume), not flushed.
    rig.complete_reload(TARGET_NS);
    let armed = FEED_SHARED.seek_to_ns.load(Relaxed);
    arm_guard(&mut rig, (armed >= 0).then_some(armed));
    rig.land_keyframe_with_key(TARGET_NS, 60, CRA);
    frames(&mut rig, 12);
    assert_eq!(reloads(), vec![TARGET_NS], "one reload, at the armed target");
    assert!(ffi_host::seek_log_for_test().is_empty());
    assert!(!is_declared(&rig));
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn with_no_row_a_declared_landing_with_no_position_update_reloads_without_dolby_vision_after_4_s() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), false);
    seek_and_land(&mut rig, CRA);
    assert!(reloads().is_empty(), "no row, no prediction: the keyframe is fed");
    advance(&mut rig, 3_000);
    assert!(reloads().is_empty(), "inside the 4 s");
    advance(&mut rig, 7_000);
    assert_eq!(reloads(), vec![TARGET_NS], "one reload, after 10 s of frames");
    assert!(!is_declared(&rig));
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_position_update_before_4_s_keeps_dolby_vision() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), false);
    seek_and_land(&mut rig, CRA);
    advance(&mut rig, 2_000);
    FEED_SHARED.frames.store(1, Relaxed);
    advance(&mut rig, 12_000);
    assert!(reloads().is_empty());
    assert!(is_declared(&rig));
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_paused_viewer_never_trips_the_watch() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), false);
    seek_and_land(&mut rig, CRA);
    rig.viewer_pause();
    advance(&mut rig, 10_000);
    assert!(reloads().is_empty(), "a paused viewer is not a silent pipeline");
    assert!(is_declared(&rig));
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_profile_5_landing_on_a_cra_is_fed_as_before() {
    let _serial = plx_base::testlock::serial();
    // Nothing can take over for a Profile 5 (no base layer): reported only.
    let mut rig = declared(p5(), true);
    let log = logged(|| seek_and_land(&mut rig, CRA));
    assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "fed as before");
    assert!(reloads().is_empty());
    assert!(is_declared(&rig));
    assert_eq!(log.matches("dv: landing keyframe").count(), 1, "one report line: {log}");
    assert!(!log.contains("dv: withdrawn"), "{log}");
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::ReportOnly);
}

#[test]
fn a_declared_remux_landing_on_a_cra_is_reported_and_fed_as_before() {
    let _serial = plx_base::testlock::serial();
    // A remux that copies the declared video: its seek is a flight ending in a fresh Load of a new
    // start.mkv with no av_seek armed, so its landing is the first video keyframe of an Engine started
    // with no seek armed.
    let mut rig = Rig::new(None);
    crate::route::install_transcode_for_test(&mut rig.ps, true, false);
    assert!(crate::route::set_stream_declaration_for_test(
        &mut rig.ps,
        "hevc",
        "eac3",
        23.976,
        p81(),
        false,
        DvCapability::Supported,
    ));
    crate::route::begin_attempt_for_test(&mut rig.ps, row_caps());
    arm_guard(&mut rig, None);
    let log = logged(|| {
        rig.land_keyframe_with_key(0, 60, CRA);
        frames(&mut rig, 12);
    });
    assert_eq!(log.matches("dv: landing keyframe").count(), 1, "one report line: {log}");
    assert!(reloads().is_empty());
    assert!(!log.contains("dv: withdrawn"), "{log}");
    assert!(rig.eng().max_fed_video_pts > 0, "the access units were fed");
    assert!(is_declared(&rig));
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::ReportOnly);
}

#[test]
fn after_the_withdrawal_a_later_cra_seek_is_in_place() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    assert!(crate::route::withdraw_dv_presentation(&mut rig.ps, "test"));
    // The reload's fresh Engine is built from the withdrawn route: no node, so nothing to guard.
    arm_guard(&mut rig, None);
    assert_eq!(rig.eng().dv_start.mode(), Mode::Inert);
    seek_and_land(&mut rig, CRA);
    assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "in place, a native Seek");
    assert!(reloads().is_empty());
}

#[test]
fn a_withdrawal_deferred_by_a_flight_reloads_once_at_the_tap_taken_during_it() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    let log = logged(|| {
        rig.arm(TARGET_NS);
        // A flight is out when the row's landing arrives: the driver must wait for it.
        crate::route::force_applying_for_test(7);
        assert!(crate::route::flight_outstanding());
        rig.land_keyframe_with_key(TARGET_NS, 60, CRA);
        frames(&mut rig, 6);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, TAP_NS); // a tap during the flight
        frames(&mut rig, 4);
        assert!(reloads().is_empty(), "nothing reloads while the flight is out");
        rig.now += 5_000; // far past SEEK_STUCK_MS: the stuck watchdog must yield to the withdrawal
        frames(&mut rig, 4);
        assert!(reloads().is_empty(), "nor does the stuck watchdog reload with the node still declared");
        crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        frames(&mut rig, 6);
    });
    assert_eq!(reloads(), vec![TAP_NS], "exactly one reload, at the tap taken during the flight");
    assert!(!log.contains("seek: in-place stuck"), "{log}");
    assert!(!is_declared(&rig));
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_paused_seek_landing_reloads_with_the_preroll_kept() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    rig.viewer_pause();
    rig.arm(TARGET_NS);
    rig.land_keyframe_with_key(TARGET_NS, 60, CRA);
    frames(&mut rig, 12);
    assert_eq!(reloads(), vec![TARGET_NS]);
    assert_eq!(preroll_at_reload(), vec![true], "the paused seek's preroll is carried across the reload");
    // The guard `dv_landing_plan` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_route_that_refuses_the_withdrawal_stands_the_guard_down_and_the_landing_feeds() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    // The guard was armed from a route that declared a node, which the route has since lost: a race the
    // arming did not see. The pump's withdrawal is refused and answers `FeedCommand::DvStandDown`.
    assert!(crate::route::withdraw_dv_presentation(&mut rig.ps, "test"));
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act, "PRECONDITION: the guard still believes in its node");
    let log = logged(|| seek_and_land(&mut rig, CRA));
    assert!(log.contains("dv: the route no longer declares a Dolby Vision node it can withdraw"), "{log}");
    assert!(!log.contains("dv: withdrawn"), "{log}");
    assert_eq!(rig.eng().dv_start.mode(), Mode::Inert, "the guard stood down");
    assert!(!rig.eng().dv_start.landing_held(), "the held landing is released");
    assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "the landing fed: one native Seek");
    assert!(reloads().is_empty());
}

// -- beside the native surround-audio watchdog -------------------------------------------------------

/// One audio access unit's duration in these rigs.
const A_STEP_NS: i64 = 20_000_000;

/// The facts of a play whose 5.1 Opus track is fed natively and whose Load has returned: what
/// `native_stall` needs besides the two non-empty queues. They are process globals, so the guard takes
/// them over and gives them back (its `Drop` runs on a failing assertion too, and before the `Rig`'s).
struct NativeOpus51;

impl NativeOpus51 {
    fn on(rig: &mut Rig) -> NativeOpus51 {
        *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Watch::new();
        native_audio::publish_fed(Kind::Opus, 6);
        let epoch = rig.eng().native_epoch;
        assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some(), "the Load returned: the watchdog's precondition");
        NativeOpus51
    }
}

impl Drop for NativeOpus51 {
    fn drop(&mut self) {
        native_audio::reset();
        *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Watch::new();
        FEED_SHARED.native_audio_refusals.store(0, Release);
    }
}

/// `n` video access units from `ns` (the demux thread's non-key frames after a landing).
fn push_video(rig: &mut Rig, ns: i64, n: i64) {
    let au = [0u8; 64];
    let q = &mut **rig.eng().aq_video.as_mut().expect("the video queue") as *mut aq::AuQueue;
    for i in 0..n {
        aq::aq_push(q, au.as_ptr(), au.len() as c_int, ns + i * V_STEP_NS, 0, 1);
    }
}

/// `n` audio access units from `ns`.
fn push_audio(rig: &mut Rig, ns: i64, n: i64) {
    let au = [0u8; 64];
    let q = &mut **rig.eng().aq_audio.as_mut().expect("the audio queue") as *mut aq::AuQueue;
    for i in 0..n {
        aq::aq_push(q, au.as_ptr(), au.len() as c_int, ns + i * A_STEP_NS, 1, 0);
    }
}

/// The order D5 claims: the Dolby Vision watch speaks first and the audio watchdog has not. The case stops
/// stepping at the withdrawal's reload attempt: this rig has no url, so the reload fails synchronously and
/// the Engine (and its stall watch) survives, where production replaces it with a fresh session.
#[test]
fn a_dv_and_surround_opus_stall_withdraws_dolby_vision_first() {
    let _serial = plx_base::testlock::serial();
    // No row: the 4 s watch is the trigger, so the landing is fed and anchored before it runs.
    let mut rig = declared(p81(), false);
    let _opus = NativeOpus51::on(&mut rig);
    let log = logged(|| {
        rig.arm(TARGET_NS);
        // The presented position the feed-ahead throttle measures from: once the prime has released
        // Play, video stops 1.6 s ahead of it and audio 3.6 s, whatever the queues hold.
        FEED_SHARED.pres_fed.store(TARGET_NS, Relaxed);
        rig.land_keyframe_with_key(TARGET_NS, 70, CRA);
        push_audio(&mut rig, TARGET_NS, 30); // 600 ms: the prime wants 300 ms of audio beside the video
        frames(&mut rig, 12);
        assert!(!rig.eng().prime_play, "PRECONDITION: the prime released Play on its audio");
        assert!(reloads().is_empty(), "no row: the CRA is fed");
        // Both queues hold data far past the throttle, and the pipeline accepts no more audio.
        push_video(&mut rig, TARGET_NS + 3_000_000_000, 40);
        push_audio(&mut rig, TARGET_NS + 4_000_000_000, 40);
        advance(&mut rig, 3_000);
        assert!(reloads().is_empty(), "inside the 4 s");
        // Step to the first frame that attempts the reload, and no further: the withdrawal's frame
        // returns right after `reload_at`, so nothing the audio watchdog does later is in this log.
        let mut waited = 0;
        while reloads().is_empty() && waited < 3_000 {
            advance(&mut rig, 250);
            waited += 250;
        }
    });
    assert!(log.contains("native audio: watching opus with 6 channels"), "PRECONDITION: native_stall armed on this feed:\n{log}");
    assert_eq!(reloads(), vec![TARGET_NS], "the Dolby Vision watch fired: one reload at the landing\n{log}");
    assert!(log.contains("dv: withdrawn for this play: no position update"), "the withdrawal came from the watch:\n{log}");
    assert!(!is_declared(&rig));
    assert!(!log.contains("native audio: STALL"), "the audio watchdog spoke before the Dolby Vision watch:\n{log}");
    assert_eq!(FEED_SHARED.native_audio_refusals.load(Relaxed), 0, "no install-wide Opus refusal is raised for the Dolby Vision splitter's silence");
}

#[test]
fn a_surround_opus_stall_before_the_prime_leaves_only_native_stall() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), false);
    let _opus = NativeOpus51::on(&mut rig);
    // The Load returned, both queues hold data and nothing is accepted, and the prime never released
    // Play: the Engine is still `Loading` (no frame feeds), `prime_play` set, so the guard's clock cannot
    // run (it needs the prime released and 700 ms of video accepted) and the audio watchdog is the
    // only reader of the silence.
    rig.eng().stage = Stage::Loading;
    rig.eng().prime_play = true;
    push_video(&mut rig, TARGET_NS, 3);
    push_audio(&mut rig, TARGET_NS, 3);
    let log = logged(|| {
        rig.frame();
        rig.now += NATIVE_AUDIO_STALL_MS + 100;
        rig.frame();
    });
    assert!(log.contains("native audio: STALL, no picture"), "PRECONDITION: the audio watchdog fired:\n{log}");
    assert!(!log.contains("dv: withdrawn"), "{log}");
    assert!(reloads().is_empty(), "the guard did not reload");
    assert!(!rig.eng().dv_start.withdrawal_pending());
    assert!(is_declared(&rig), "the declaration is untouched");
}

#[test]
fn a_held_landing_is_not_a_native_stall() {
    let _serial = plx_base::testlock::serial();
    // The row's landing is held for the reload and neither lane feeds: audio accepted does not move
    // for far longer than the audio watchdog's window, with both queues full and no picture.
    let mut rig = declared(p81(), true);
    let _opus = NativeOpus51::on(&mut rig);
    let log = logged(|| {
        seek_and_land(&mut rig, CRA);
        assert!(rig.eng().dv_start.landing_held(), "PRECONDITION: the landing is held");
        push_audio(&mut rig, TARGET_NS, 40);
        advance(&mut rig, NATIVE_AUDIO_STALL_MS * 3);
    });
    assert_eq!(reloads(), vec![TARGET_NS], "the row's reload, once");
    assert!(!log.contains("native audio:"), "the audio watchdog neither armed nor fired on a held feed:\n{log}");
    assert_eq!(FEED_SHARED.native_audio_refusals.load(Relaxed), 0);
}
