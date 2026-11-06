//! The Dolby Vision landing guard driven through the real `pump()` (#1166): a declared landing the
//! firmware's splitter would skip (the row), or that no position update follows (the watch), is one
//! `reload_at` without the node. A sibling of `native_seek_pump_tests` and built on its `Rig`; every
//! case builds the guard through `engine::dv_start_for`, the only constructor production uses, and
//! plants the firmware row the way production reaches it: through the ATTEMPT's caps
//! (`begin_attempt_for_test`), never the per-thread firmware override, which applies no rows.
//! `reload_at` has no route in a unit rig, so each call is read off `engine::RELOAD_AT_LOG`.
//!
//! Case 11 of the plan (a declared direct play with surround native Opus: the DV watch fires before
//! `native_stall`, and a stall before the prime leaves only `native_stall`) is NOT written here yet:
//! it needs a native-audio rig, see the handoff note.

use super::native_seek_pump_tests::{preroll_at_reload, reloads, Rig, TARGET_NS};
use super::*;
use crate::player::dv_start::Mode;
use crate::player::ffi_host;
use plx_data::metadata::Dovi;
use plx_platform::devcaps::{apply_exclusions, dv::DvCapability, Caps, EXCLUSIONS};
use std::os::raw::c_int;

/// An HEVC CRA as an access unit's `key` (`aq::AuNode::key`).
const CRA: c_int = 21;
const IDR: c_int = crate::aq::AU_KEY;
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

/// Build the Engine's guard the way `start_bufferfeed` does, from the rig's route.
fn arm_guard(rig: &mut Rig, armed: Option<i64>) {
    let guard = super::super::engine::dv_start_for(&rig.ps, armed);
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_resume_landing_on_a_cra_reloads_once_at_the_armed_target() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), true);
    // A fresh Engine with a seek armed for its first open (a resume), not flushed.
    rig.complete_reload(TARGET_NS);
    let armed = SHARED.seek_to_ns.load(Relaxed);
    arm_guard(&mut rig, (armed >= 0).then_some(armed));
    rig.land_keyframe_with_key(TARGET_NS, 60, CRA);
    frames(&mut rig, 12);
    assert_eq!(reloads(), vec![TARGET_NS], "one reload, at the armed target");
    assert!(ffi_host::seek_log_for_test().is_empty());
    assert!(!is_declared(&rig));
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}

#[test]
fn a_position_update_before_4_s_keeps_dolby_vision() {
    let _serial = plx_base::testlock::serial();
    let mut rig = declared(p81(), false);
    seek_and_land(&mut rig, CRA);
    advance(&mut rig, 2_000);
    SHARED.frames.store(1, Relaxed);
    advance(&mut rig, 12_000);
    assert!(reloads().is_empty());
    assert!(is_declared(&rig));
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
        crate::player::request_seek(TAP_NS); // a tap during the flight
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
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
    // The guard `dv_start_for` built for this route (checked last: the behaviour above is the symptom).
    assert_eq!(rig.eng().dv_start.mode(), Mode::Act);
}
