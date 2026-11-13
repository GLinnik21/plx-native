use super::present::*;

fn swap(p: &mut Present, now: u32) -> bool {
    let demand = p.take(now);
    if demand { p.acknowledge(now); }
    demand
}
#[test]
fn video_plane_edges_have_one_recipient_and_unbind_paints_once() {
    let mut p = Present::new();
    swap(&mut p, 0);
    p.note(PresentEvent::VideoPlane(true));
    assert!(swap(&mut p, 16));
    assert!(swap(&mut p, 32));
    p.note(PresentEvent::VideoPlane(false));
    assert!(swap(&mut p, 48));
    assert!(!p.peek(64));
}
#[test]
fn noidle_is_rate_only_and_damage_raised_during_draw_survives() {
    let mut p = Present::new();
    swap(&mut p, 0);
    p.set_enabled(false);
    assert!(p.take(1));
    p.note(PresentEvent::Damage(Provenance::Input));
    p.acknowledge(1);
    p.set_enabled(true);
    assert!(swap(&mut p, 2));
    assert_eq!(p.take_presents(), 3);
    assert!(!p.peek(3));
}
#[test]
fn one_resting_frame_follows_motion_without_moving_the_page_snapshot() {
    let mut p = Present::new();
    swap(&mut p, 0);
    p.report_motion(true, false);
    assert!(!p.page_moving());
    assert!(swap(&mut p, 16));
    p.report_motion(false, false);
    assert!(swap(&mut p, 32));
    assert!(!p.page_moving());
    p.report_motion(false, false);
    assert!(!p.peek(48));
}
#[test]
fn actual_present_keepalive_handles_tick_wrap() {
    let mut p = Present::new();
    swap(&mut p, u32::MAX - 100);
    assert!(!p.peek(100));
    assert!(p.peek(KEEPALIVE_MS));
    assert_eq!(p.take_presents(), 1);
    assert!(p.take(KEEPALIVE_MS));
    assert_eq!(p.take_presents(), 0, "admission is not a swap");
}
#[test]
fn newer_surface_motion_does_not_keep_acknowledged_page_motion_alive() {
    let mut p = Present::new();
    swap(&mut p, 0);
    p.note(PresentEvent::Motion);
    assert!(p.page_moving());
    p.take(1);
    p.set_scope(Scope::Surface);
    p.note(PresentEvent::Motion);
    p.acknowledge(1);
    assert!(p.changed(), "new surface motion remains pending");
    assert!(!p.page_moving(), "only the page generation was acknowledged");
}
#[test]
fn skipped_admission_preserves_first_provenance_until_actual_ack() {
    let mut p = Present::new();
    swap(&mut p, 0);
    p.note(PresentEvent::Damage(Provenance::Input));
    p.take(1);
    p.note(PresentEvent::Damage(Provenance::Nav));
    assert_eq!(p.why(), Some(Provenance::Input));
    // This admission never swapped: admitting again must not drop its older cause.
    p.take(2);
    assert_eq!(p.why(), Some(Provenance::Input));
    p.note(PresentEvent::Damage(Provenance::Lifecycle));
    p.acknowledge(2);
    assert_eq!(p.why(), Some(Provenance::Lifecycle));
    assert!(p.peek(3));
}
