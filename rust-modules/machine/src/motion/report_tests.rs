use super::report::*;

#[test]
fn surface_and_decorative_motion_do_not_move_page_layout() {
    frame_begin(1.0 / 60.0);
    let scope = MotionScope::open();
    note_motion();
    assert!(scope.close());
    assert!(present_moving());
    assert!(!page_moving());
    assert!(!page_layout_moving());
    frame_begin(1.0 / 60.0);
    decorative(note_motion);
    assert!(page_moving());
    assert!(!page_layout_moving());
    note_motion();
    assert!(page_layout_moving());
}

#[test]
fn dropped_and_nested_scopes_restore_attribution() {
    frame_begin(1.0 / 60.0);
    {
        let _outer = MotionScope::open();
        let _inner = MotionScope::open();
        note_motion();
    }
    assert!(!page_moving());
    note_motion();
    assert!(page_moving());
}

#[test]
fn velocity_is_judged_by_visible_frame_travel() {
    frame_begin(1.0 / 60.0);
    assert!(settled(1000.0, 1000.0, 1.0));
    assert!(!settled(1000.0, 1000.0, 100.0));
    assert!(!settled(1000.0, 1000.3, 0.0));
    frame_begin_judging_rest_with(0.0, 1.0 / 60.0);
    assert!(!settled(1000.0, 1000.0, 100.0));
}

#[test]
fn scoped_damage_preserves_concurrent_page_damage() {
    let _guard = plx_base::testlock::serial();
    use crate::present::{Present, PresentEvent, Provenance, Scope};
    reset_for_test();
    let mut present = Present::new();
    present.take(0);
    present.acknowledge(0);
    present.note_in_scope(Scope::Surface, PresentEvent::Damage(Provenance::Input));
    assert!(!present.page_damaged());
    present.note_in_scope(Scope::Surface, PresentEvent::Damage(Provenance::Input));
    present.note_in_scope(Scope::Page, PresentEvent::Damage(Provenance::Lifecycle));
    assert!(present.page_damaged());
    present.take(1);
    present.acknowledge(1);
    assert!(!present.page_damaged());
}

#[test]
fn frozen_draw_reports_are_ignored_but_worker_landings_survive() {
    let _guard = plx_base::testlock::serial();
    reset_for_test();
    use crate::present::{Present, PresentEvent, Provenance};
    let mut present = Present::global();
    present.take(0);
    present.acknowledge(0);
    set_page_frozen(true);
    note_motion();
    assert_eq!(take_local_motion(), 0);
    std::thread::spawn(crate::present::wake_from_worker).join().unwrap();
    assert!(present.changed(), "worker completion wakes scheduling despite a frozen decoration");
    assert!(!present.page_damaged(), "workers cannot assert UI provenance");
    present.note(PresentEvent::Damage(Provenance::Lifecycle));
    assert!(present.page_damaged(), "the runner's typed landing is not suppressed by draw freeze");
    set_page_frozen(false);
}

#[test]
fn captured_draw_motion_survives_actual_swap_acknowledgement() {
    use crate::present::Present;
    reset_for_test();
    let mut present = Present::new();
    collect_into(&mut present, 0);
    assert!(present.take(0));
    note_motion();
    collect_into(&mut present, 0); // visible draw's reports land before swap acknowledgement
    assert!(present.changed_after_admission());
    present.acknowledge(0);
    frame_begin(1.0 / 60.0);
    assert!(present.changed(), "frame reset cannot erase already-owned draw motion");
    assert!(present.page_moving());
    assert!(!present.page_damaged(), "animation never fabricates content provenance");
    assert!(present.take(16));
}

#[test]
fn draw_damage_survives_owner_admission_and_swap() {
    use crate::present::Present;
    let _guard = plx_base::testlock::serial();
    reset_for_test();
    let mut present = Present::new();
    collect_into(&mut present, 0);
    assert!(present.take(0));
    present.note(crate::present::PresentEvent::Damage(crate::present::Provenance::Input));
    present.acknowledge(0);
    collect_into(&mut present, 1);
    assert!(present.take(1));
    assert!(present.page_damaged());
    assert!(present.page_damaged(), "frame readout is stable across consumers");
    present.acknowledge(1);
    collect_into(&mut present, 2);
    assert!(!present.peek(2));
    assert!(!present.page_damaged());
}
