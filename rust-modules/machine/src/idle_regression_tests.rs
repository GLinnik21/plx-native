//! Original idle regression cases, exercised through an owned gate and explicit reports.
use crate::motion::report::*;
use crate::present::{Present, PresentEvent, Provenance, Scope, KEEPALIVE_MS};

struct Runtime {
    gate: Present,
    selected_damage: bool,
    _serial: plx_base::testlock::Serial,
}
impl Runtime {
    fn fresh() -> Self {
        let serial = plx_base::testlock::serial();
        reset_for_test();
        let mut gate = Present::global();
        gate.take(0);
        gate.acknowledge(0);
        gate.take_presents();
        Self { gate, selected_damage: false, _serial: serial }
    }
    fn presented(&mut self, now: u32) {
        self.gate.take(now);
        self.gate.acknowledge(now);
    }
    fn swap(&mut self, now: u32) -> bool {
        collect_into(&mut self.gate, now);
        self.selected_damage = self.gate.page_damaged();
        let demand = self.gate.take(now);
        if demand { self.gate.acknowledge(now); }
        demand
    }
}
#[test]
fn the_present_gate_answers_true_only_while_the_plane_bit_is_set() {
    let mut r = Runtime::fresh();
    assert!(!r.gate.video_plane());
    r.gate.note(PresentEvent::Damage(Provenance::Input));
    assert!(r.swap(0));
    assert!(!r.swap(16));
    r.gate.note(PresentEvent::VideoPlane(true));
    for now in [32, 48, 64, 80] { assert!(r.swap(now)); }
    assert!(!r.gate.page_damaged());
    r.gate.note(PresentEvent::VideoPlane(true));
    assert!(!r.gate.page_damaged(), "an unchanged plane edge reports no damage");
    r.gate.note(PresentEvent::VideoPlane(false));
    assert!(r.swap(96), "the owned edge paints the unbind frame");
    assert!(!r.swap(112));
}
#[test]
fn page_damage_is_what_no_popover_scope_raised() {
    let mut r = Runtime::fresh();
    assert!(!r.gate.page_damaged());
    r.gate.note_in_scope(Scope::Surface, PresentEvent::Damage(Provenance::Input));
    r.gate.note_in_scope(Scope::Surface, PresentEvent::Damage(Provenance::Input));
    assert!(!r.gate.page_damaged());
    r.gate.note_in_scope(Scope::Surface, PresentEvent::Damage(Provenance::Input));
    r.gate.note_in_scope(Scope::Surface, PresentEvent::Damage(Provenance::Input));
    r.gate.note_in_scope(Scope::Page, PresentEvent::Damage(Provenance::Input));
    assert!(r.gate.page_damaged());
    r.presented(0);
    assert!(!r.gate.page_damaged());
    frame_begin(1.0 / 60.0);
    r.swap(10_000);
    frame_begin(1.0 / 60.0);
    assert!(!r.swap(10_016));
    r.gate.note(PresentEvent::Damage(Provenance::Input));
    assert!(r.gate.page_damaged());
}
#[test]
fn a_standing_clock_can_judge_rest_with_another_dt() {
    let _r = Runtime::fresh();
    frame_begin(0.0);
    assert!(settled(1.0, 1.0, 100.0));
    let before = now_ms();
    frame_begin_judging_rest_with(0.0, 1.0 / 60.0);
    assert!(!settled(1.0, 1.0, 100.0));
    assert_eq!(now_ms(), before);
}
#[test]
fn settled_screen_stops_presenting() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(1.0, 1.0, 0.0);
    assert!(!r.swap(10_016));
}
#[test]
fn motion_forces_a_present() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(0.5, 1.0, 0.0);
    assert!(r.swap(10_016));
    frame_begin(1.0 / 60.0);
    note_spring(1.0, 1.0, 0.4);
    assert!(r.swap(10_032));
}
#[test]
fn sub_epsilon_residue_is_at_rest() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(1.0 + 0.001 / 4.0, 1.0, 0.001 / 4.0);
    assert!(!r.swap(10_016));
}
#[test]
fn a_scroll_that_has_visually_arrived_is_at_rest() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(280.0 - 0.05, 280.0, 5.0);
    assert!(!r.swap(10_016), "subpixel travel must not keep presenting");
}
#[test]
fn a_scroll_still_visibly_moving_presents() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(279.0, 280.0, 0.0);
    assert!(r.swap(10_016));
    frame_begin(1.0 / 60.0);
    note_spring(280.0, 280.0, 60.0);
    assert!(r.swap(10_032));
}
#[test]
fn scoped_motion_keeps_host_and_foreground_motion_distinct() {
    let mut r = Runtime::fresh();
    frame_begin(1.0 / 60.0);
    note_spring(0.0, 1.0, 0.0);
    let (_, host_moving) = scoped_motion(|| note_spring(1.0, 1.0, 0.0));
    assert!(!host_moving);
    assert!(r.swap(10_000));
    frame_begin(1.0 / 60.0);
    let (_, host_moving) = scoped_motion(|| note_spring(0.0, 1.0, 0.0));
    assert!(host_moving);
    assert!(r.swap(10_016));
}
#[test]
fn the_threshold_never_exceeds_a_quarter_pixel() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(1500.0 - 0.3, 1500.0, 0.0);
    assert!(r.swap(10_016));
}
#[test]
fn a_jump_reports_only_when_it_changes_something() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_jump(false);
    assert!(!r.swap(10_016));
    note_jump(true);
    assert!(r.swap(10_032));
}
#[test]
fn a_report_raised_during_a_draw_survives_to_the_next_frame() {
    let mut r = Runtime::fresh();
    frame_begin(1.0 / 60.0);
    collect_into(&mut r.gate, 10_000);
    assert!(r.gate.take(10_000));
    r.gate.note(PresentEvent::Damage(Provenance::Input));
    r.gate.acknowledge(10_000);
    frame_begin(1.0 / 60.0);
    assert!(r.swap(10_016));
}
#[test]
fn invalidate_buys_exactly_one_frame() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    r.gate.note(PresentEvent::Damage(Provenance::Input));
    assert!(r.swap(10_016));
    assert!(r.selected_damage);
    frame_begin(1.0 / 60.0);
    assert!(!r.swap(10_032));
}
#[test]
fn wake_buys_a_frame_without_reporting_content_damage() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    r.gate.note(PresentEvent::Wake);
    assert!(r.swap(10_016));
    assert!(!r.selected_damage);
    assert!(!r.gate.page_damaged());
}
#[test]
fn keepalive_bounds_staleness() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    assert!(!r.swap(10_000 + KEEPALIVE_MS - 1));
    assert!(r.swap(10_000 + KEEPALIVE_MS));
    assert!(!r.selected_damage);
}
#[test]
fn tick_wrap_is_arithmetically_sound() {
    let mut r = Runtime::fresh();
    r.presented(u32::MAX - 10);
    assert!(!r.swap(5));
    assert!(r.swap((u32::MAX - 10).wrapping_add(KEEPALIVE_MS)));
    r.presented(5);
    assert!(r.swap(u32::MAX - 10));
}
#[test]
fn kill_switch_always_presents() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    r.gate.set_enabled(false);
    assert!(r.swap(10_016));
}
#[test]
fn a_whole_settled_tree_asks_for_nothing() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    for i in 0..439 {
        let value = 1.0 + i as f32 * 0.001;
        note_spring(value, value, 0.0);
    }
    assert!(!r.swap(10_016));
}
#[test]
fn one_mover_in_a_settled_tree_is_enough() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    for _ in 0..438 { note_spring(1.0, 1.0, 0.0); }
    note_spring(1.0, 1.5, 0.0);
    assert!(r.swap(10_016));
}
#[test]
fn motion_does_not_survive_the_frame_that_had_it() {
    let mut r = Runtime::fresh();
    note_spring(0.0, 1.0, 0.0);
    assert!(r.swap(10_000));
    frame_begin(1.0 / 60.0);
    assert!(r.swap(10_016), "one settle frame");
    frame_begin(1.0 / 60.0);
    assert!(!r.swap(10_032));
}
#[test]
fn a_longer_frame_covers_more_travel() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    frame_begin(1.0 / 60.0);
    note_spring(300.0, 300.0, 12.0);
    assert!(!r.swap(10_016));
    frame_begin(0.05);
    note_spring(300.0, 300.0, 12.0);
    assert!(r.swap(10_032));
}
#[test]
fn the_kill_switch_leaves_the_dirty_flag_alone() {
    let mut r = Runtime::fresh();
    r.presented(10_000);
    r.gate.set_enabled(false);
    r.gate.note(PresentEvent::Damage(Provenance::Input));
    collect_into(&mut r.gate, 10_016);
    assert!(r.gate.take(10_016));
    assert!(r.gate.page_damaged());
    collect_into(&mut r.gate, 10_032);
    assert!(r.gate.take(10_032));
    assert!(r.gate.page_damaged(), "noidle changes rate, never consumes pending content damage");
    r.gate.set_enabled(true);
    assert!(r.gate.peek(10_032), "skipped swaps cannot consume the flag");
    r.gate.acknowledge(10_032);
    assert!(!r.swap(10_048));
}
#[test]
fn presents_drain_once() {
    let mut r = Runtime::fresh();
    r.presented(1);
    r.presented(2);
    assert_eq!(r.gate.take_presents(), 2);
    assert_eq!(r.gate.take_presents(), 0);
}
