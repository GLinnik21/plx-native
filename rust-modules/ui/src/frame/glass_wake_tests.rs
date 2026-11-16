use super::glass::GlassPlan;
use plx_machine::present::Present;

#[test]
fn armed_dial_schedules_only_its_owner_after_admission() {
    let _serial = plx_base::testlock::serial();
    let mut owner = Present::new();
    let mut other = Present::new();
    let mut worker = Present::global();
    for p in [&mut other, &mut worker] { p.take(0); p.acknowledge(0); }
    assert!(owner.take(0));
    let mut plan = GlassPlan::new();
    plan.configure_navblur("1");
    plan.prepare_dial(1, &mut owner);
    owner.acknowledge(0);
    assert!(owner.peek(1), "post-admission dial demand must survive its current swap");
    assert!(!other.peek(1));
    assert!(!worker.peek(1), "the benchmark cannot use the global worker door");
    assert!(!owner.page_damaged());
    assert!(!owner.page_moving());
    assert!(owner.why().is_none());
    let _ = GlassPlan::new(); // restore the instrumentation-only published dial defaults
}

#[test]
fn an_unarmed_dial_requests_no_frame() {
    let _serial = plx_base::testlock::serial();
    let mut owner = Present::new();
    owner.take(0); owner.acknowledge(0);
    GlassPlan::new().prepare_dial(1, &mut owner);
    assert!(!owner.peek(1));
}
