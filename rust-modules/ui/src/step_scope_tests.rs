use super::*;
use crate::fixture::{booted, tick, FixtureHost};
use crate::frame::Budget;
use crate::screen::RenderStrategy;
use plx_machine::machine::{LogLine, LogicalState, Machine};
use std::borrow::Cow;

struct ProbeScreen { probe: fn() }
impl Focusable<FixtureHost> for ProbeScreen {
    fn groups(&self, _: &Cx<'_, FixtureHost>, _: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _: &u32, _: &Cx<'_, FixtureHost>) -> Option<GroupId> { None }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, FixtureHost>) -> Step<u32> { Step::Edge }
    fn place(&self, _: &u32, _: &Cx<'_, FixtureHost>, _: At) -> Option<Placed> { None }
    fn reconcile(&self, key: FocusKey<u32>, _: &Cx<'_, FixtureHost>) -> FocusKey<u32> { key }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
        FocusKey { entry: EntryId(0), elem: 0 }
    }
}
impl Machine<FixtureHost> for ProbeScreen {
    type Ev = ScreenEvent<FixtureHost>;
    fn step(&mut self, _: &Self::Ev, _: &Cx<'_, FixtureHost>, fx: &mut Effects<'_, FixtureHost>) -> Handled {
        (self.probe)();
        fx.push(Fx::Log(LogLine("probe emission".into())));
        Handled::Yes
    }
}
impl Screen<FixtureHost> for ProbeScreen {
    fn name(&self) -> &'static str { "scope-probe" }
    fn state(&self) -> &dyn LogicalState { &() }
    fn crumb(&self, _: &Cx<'_, FixtureHost>) -> Option<Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, FixtureHost>) {}
    fn draw(&mut self, _: &mut DrawFrame<'_, '_, FixtureHost>) {}
    fn render(&self) -> RenderStrategy { RenderStrategy::VideoPlane }
}
struct RunnerTap(usize);
impl Tap<FixtureHost> for RunnerTap {
    fn effect(&mut self, _: u64, _: &Stamped<FixtureHost>) {
        plx_base::step::assert_io_allowed("effect drain");
        plx_base::task::spawn("scope-runner", || {}).unwrap().join().unwrap();
        self.0 += 1;
    }
}
#[test]
fn dispatch_marks_the_actual_screen_step_but_not_its_same_frame_effect_drain() {
    let (mut dispatcher, mut rig, id) = booted();
    fn inside_step() {
        assert!(std::panic::catch_unwind(|| plx_base::step::assert_io_allowed("screen probe")).is_err());
    }
    dispatcher.nav.instance_mut(id).unwrap().screen = Box::new(ProbeScreen { probe: inside_step });
    let mut runner = RunnerTap(0);
    dispatcher.frame(&mut rig, tick(16), vec![], vec![], &mut runner);
    assert!(runner.0 > 0, "the emitted effect ran in this frame's drain");
}
#[test]
fn dispatch_refuses_worker_and_clock_leaks_and_unwinding_restores_the_scope() {
    fn worker() { let _ = plx_base::task::spawn("forbidden", || {}); }
    fn clock() { plx_base::clock::vclock_ms(); }
    for probe in [worker as fn(), clock as fn()] {
        let (mut dispatcher, mut rig, id) = booted();
        dispatcher.nav.instance_mut(id).unwrap().screen = Box::new(ProbeScreen { probe });
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            dispatcher.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
        })).expect_err("screen IO must be refused");
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("IO during machine step"), "{message}");
        plx_base::step::assert_io_allowed("after dispatcher unwind");
    }
}
