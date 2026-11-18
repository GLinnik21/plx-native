//! Real dispatcher draw, final-swap admission and ordinary budgeted delivery.
use super::*;
use crate::fixture::{FixtureArg, FixtureHost, FixtureMsg, FixtureRig, FixtureScreen};

#[derive(Default)]
struct Audit { seen: Vec<(u64, u64, EntryId, InstanceId)> }
impl Tap<FixtureHost> for Audit {
    fn presentation(&mut self, f: u64, paint: u64, entry: EntryId, instance: InstanceId, _: &FixtureMsg) {
        self.seen.push((f, paint, entry, instance));
    }
}
struct Runtime {
    d: Dispatcher<FixtureHost>, rig: FixtureRig, audit: Audit,
    admit: bool, swap: bool, draws: u32, report: FrameReport,
}
impl Runtime {
    fn new() -> Self {
        let mut d = Dispatcher::new();
        let mut rig = FixtureRig::new();
        d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(975)));
        d.frame_with(&mut rig, Tick::default(), vec![], vec![], &mut NoTap, false);
        Self { d, rig, audit: Audit::default(), admit:true, swap:true, draws:0, report:FrameReport::default() }
    }
    fn step_only(&mut self) {
        let tick = Tick { ms:self.d.frame as u32 * 16, dt_us:16_000 };
        self.d.frame_with(&mut self.rig, tick, vec![], vec![], &mut self.audit, false);
    }
    fn apps(&self) -> usize {
        self.d.nav.tabs.stack.top().unwrap().inst.as_ref().unwrap().screen
            .as_any().unwrap().downcast_ref::<FixtureScreen>().unwrap().state.events.iter().filter(|e| **e == "presented").count()
    }
    fn block_head(&mut self) {
        self.d.pending_app = Some(PendingAppCommand { id:AppCommandId(99), from:MachineId::Nav,
            command:None, remaining:0, parts:self.d.parts(Tick::default()), retired:false });
    }
    fn release_with_no_post_budget(&mut self) {
        let mut report = FrameReport::default();
        assert_eq!(self.d.resume_app_command(&mut self.rig, AppCommandId(99),
            AppCommandOutcome::Completed, vec![], &mut report, &mut self.audit), Ok(0));
    }
}
impl FrameRuntime<FixtureHost> for Runtime {
    fn dispatcher(&mut self) -> &mut Dispatcher<FixtureHost> { &mut self.d }
    fn pump(&mut self) { self.d.present.note(plx_machine::present::PresentEvent::Wake); }
    fn ingest(&mut self) {}
    fn results(&mut self) -> bool { true }
    fn step(&mut self) -> bool {
        self.report = self.d.frame_with(&mut self.rig,
            Tick { ms:self.d.frame as u32 * 16, dt_us:16_000 }, vec![], vec![], &mut self.audit, false);
        true
    }
    fn publish(&mut self) { self.d.finish_product_turn(&mut self.rig, &mut self.report, &mut self.audit); }
    fn admit(&mut self, demand: bool) -> bool { self.admit && demand }
    fn prepare(&mut self) { self.d.prepare(&mut self.rig); }
    fn draw_and_swap(&mut self) -> bool {
        let before = self.d.state_hash();
        self.d.draw(&mut self.rig, true);
        assert_eq!(self.d.state_hash(), before, "draw is not an effect execution lane");
        self.draws += 1;
        self.swap
    }
    fn skipped(&mut self) {}
    fn end(&mut self) {}
}

#[test]
fn actual_visible_paint_requires_positive_final_swap_then_delivers_once_before_tick() {
    let _serial = plx_base::testlock::serial();
    for (admit, swap) in [(true,true), (false,true), (true,false)] {
        let mut r = Runtime::new(); r.admit = admit; r.swap = swap;
        Dispatcher::product_frame(&mut r);
        assert_eq!(r.draws, u32::from(admit));
        assert_eq!(r.apps(), 0, "paint/swap does not mutate logical state");
        assert!(r.d.presentation_draw.is_none(), "failed and successful swaps both consume the candidate");
        r.step_only();
        assert_eq!(r.apps(), usize::from(admit && swap));
        assert_eq!(r.audit.seen.len(), usize::from(admit && swap));
        if admit && swap {
            let events = &r.d.nav.tabs.stack.top().unwrap().inst.as_ref().unwrap().screen
                .as_any().unwrap().downcast_ref::<FixtureScreen>().unwrap().state.events;
            let app = events.iter().position(|e| *e == "presented").unwrap();
            assert_eq!(events[app + 1], "tick", "external observation precedes this turn's Tick");
        }
        r.step_only();
        assert_eq!(r.apps(), usize::from(admit && swap), "a receipt is one-shot without another swap");
    }
}

#[test]
fn independent_owners_and_replaced_instances_cannot_inherit_paint_evidence() {
    let _serial = plx_base::testlock::serial();
    let mut a = Runtime::new(); let mut b = Runtime::new();
    Dispatcher::product_frame(&mut a);
    b.step_only(); assert_eq!(b.apps(), 0);
    a.d.nav.tabs.stack.top_mut().unwrap().inst.as_mut().unwrap().id = InstanceId(999);
    a.step_only(); assert_eq!(a.apps(), 0, "same entry, different actual instance is late");
    assert!(a.audit.seen.is_empty());
}

#[test]
fn speculative_recording_and_source_draws_cannot_mint_receipts() {
    let _serial = plx_base::testlock::serial();
    let mut r = Runtime::new();
    let parts = r.d.parts(Tick::default());
    let split = r.rig.split();
    let cx = parts.cx::<FixtureHost>(split.views, split.measure);
    let recorded = DrawFrame::new(&cx, Painter::recording());
    recorded.report_presentation(FixtureMsg::Store(StoreOrd(0), 1));
    assert!(recorded.take_presentation().is_none());
    let visible = DrawFrame::new(&cx, Painter::root());
    crate::rec::speculative(|| visible.report_presentation(FixtureMsg::Store(StoreOrd(0), 1)));
    assert!(visible.take_presentation().is_none());
    let sources = std::rc::Rc::new(std::cell::RefCell::new(crate::frame::backdrop::Sources::default()));
    sources.borrow_mut().begin(vec![]);
    let _source = crate::frame::backdrop::discover(sources);
    visible.report_presentation(FixtureMsg::Store(StoreOrd(0), 1));
    assert!(visible.take_presentation().is_none());
}

#[test]
fn replay_requires_explicit_observation_mode_exact_address_frame_and_no_duplicate() {
    let mut r = Runtime::new();
    let entry = r.d.owner_entry().unwrap();
    let instance = r.d.nav.instance_of(entry).unwrap();
    let frame = r.d.frame;
    let message = || FixtureMsg::Store(StoreOrd(0), 1);
    assert!(!r.d.replay_presentation(frame, entry, instance, message()));
    r.d.presentation_replay(true);
    assert!(!r.d.replay_presentation(frame + 1, entry, instance, message()));
    assert!(!r.d.replay_presentation(frame, entry, InstanceId(instance.0 + 1), message()));
    assert!(r.d.replay_presentation(frame, entry, instance, message()));
    assert!(!r.d.replay_presentation(frame, entry, instance, message()));
    r.step_only(); assert_eq!(r.apps(), 1);
    assert!(!r.d.replay_presentation(frame, entry, instance, message()));
}

#[test]
fn blocked_fifo_and_zero_remaining_post_carry_keep_the_only_real_presentation() {
    let _serial = plx_base::testlock::serial();
    let mut r = Runtime::new();
    Dispatcher::product_frame(&mut r);
    r.block_head(); r.step_only();
    assert_eq!(r.apps(), 0); assert_eq!(r.audit.seen.len(), 1);
    assert!(r.d.queue.iter().any(|s| matches!(s.fx, Fx::Deliver(_, Delivery::Screen(ScreenEvent::Presented(_))))));
    r.release_with_no_post_budget();
    assert_eq!(r.apps(), 0, "no bypass of the original exhausted POST budget");
    r.step_only();
    assert_eq!(r.apps(), 1, "same owner/offer evidence survives a legitimate FIFO delay");
    r.step_only(); assert_eq!(r.apps(), 1);
}

#[test]
fn carried_receipt_revalidates_actual_instance_and_newer_visible_generation() {
    let _serial = plx_base::testlock::serial();
    for replace in [false,true] {
        let mut r = Runtime::new(); Dispatcher::product_frame(&mut r);
        r.block_head(); r.step_only(); r.release_with_no_post_budget();
        if replace {
            r.d.nav.tabs.stack.top_mut().unwrap().inst.as_mut().unwrap().id = InstanceId(999);
        } else {
            r.d.draw(&mut r.rig, true); r.d.finish_presentation(true);
        }
        r.step_only();
        assert_eq!(r.apps(), usize::from(!replace), "only the newest still-addressed paint may land");
    }
}

#[test]
fn repeated_real_paints_bound_pending_receipts_without_bypassing_the_blocked_fifo() {
    let _serial = plx_base::testlock::serial();
    let mut r = Runtime::new(); Dispatcher::product_frame(&mut r);
    r.block_head(); r.step_only();
    for _ in 0..3 {
        r.d.draw(&mut r.rig, true); r.d.finish_presentation(true); r.step_only();
        assert_eq!(r.d.queue.iter().filter(|s| matches!(s.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Presented(_))))).count(), 1);
        assert_eq!(r.apps(), 0);
    }
    r.release_with_no_post_budget(); r.step_only();
    assert_eq!(r.apps(), 1, "only the newest displayed opportunity lands after the real head settles");
}

#[test]
fn numerically_identical_foreign_receipt_cannot_forge_this_runtime_authority() {
    let _serial = plx_base::testlock::serial();
    let mut r = Runtime::new(); Dispatcher::product_frame(&mut r); r.step_only();
    let (frame, entry, instance) = r.d.presentation_latest.unwrap();
    let foreign = plx_machine::machine::PresentationAuthority::default().receipt(
        frame, entry, instance, FixtureMsg::Store(StoreOrd(0), 1));
    r.d.emit(MachineId::Present, Fx::Deliver(MachineId::Instance(instance), Delivery::Screen(ScreenEvent::Presented(foreign))));
    r.step_only(); assert_eq!(r.apps(), 1, "foreign seal is rejected even with the same frame and numeric IDs");
}
