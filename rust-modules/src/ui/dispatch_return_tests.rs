// Housekeeping must not copy page memory, or consume a queued navigation bookmark.
use super::*;
use crate::app::bridge::AppHost;
use crate::screens::registry::{AppFx, LibraryReq};
use crate::stores::browse::{BrowseCmd, LibraryWork, SectionAddress};
use crate::stores::{StoreCmd, StoreId, StoreWork};
use crate::ui::fixture::{FixtureArg, FixtureMeasure};
use crate::ui::machine::{Canon, LogicalState, Machine};
use std::cell::Cell;
use std::rc::Rc;

struct ProbeHost;
impl Host for ProbeHost {
    type Arg = FixtureArg;
    type Fx = AppFx;
    type Msg = ();
    type Elem = u32;
    type Views<'a> = ();
    type Init = ();
    type Memory = Memory;
    fn app_fx_needs_return(fx: &AppFx) -> bool { AppHost::app_fx_needs_return(fx) }
}

#[derive(Clone, Default, Debug)]
struct Memory(u32);
impl LogicalState for Memory {
    fn write(&self, c: &mut Canon) { c.u32(self.0); }
    fn probe(&self, out: &mut String) { out.push_str(&self.0.to_string()); }
}

#[derive(Clone, Default)]
struct Probe {
    captures: Rc<Cell<u32>>,
    value: Rc<Cell<u32>>,
}
impl Machine<ProbeHost> for Probe {
    type Ev = ScreenEvent<ProbeHost>;
    fn step(&mut self, _: &Self::Ev, _: &Cx<'_, ProbeHost>, _: &mut Effects<'_, ProbeHost>) -> Handled {
        Handled::No
    }
}
impl Focusable<ProbeHost> for Probe {
    fn groups(&self, _: &Cx<'_, ProbeHost>, _: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _: &u32, _: &Cx<'_, ProbeHost>) -> Option<GroupId> { None }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, ProbeHost>) -> Step<u32> { Step::Edge }
    fn place(&self, _: &u32, _: &Cx<'_, ProbeHost>, _: At) -> Option<Placed> { None }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, ProbeHost>) -> FocusKey<u32> { want }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, ProbeHost>) -> FocusKey<u32> { unreachable!() }
}
impl Screen<ProbeHost> for Probe {
    fn name(&self) -> &'static str { "return-probe" }
    fn state(&self) -> &dyn LogicalState { &() }
    fn crumb(&self, _: &Cx<'_, ProbeHost>) -> Option<std::borrow::Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, ProbeHost>) {}
    fn draw(&mut self, _: &mut DrawFrame<'_, '_, ProbeHost>) {}
    fn render(&self) -> crate::ui::screen::RenderStrategy { crate::ui::screen::RenderStrategy::Page }
    fn memory_at(&self, _: Option<FocusKey<u32>>) -> Memory {
        self.captures.set(self.captures.get() + 1);
        Memory(self.value.get())
    }
}
impl Mounter<ProbeHost> for Probe {
    fn mount(&mut self, _: InstanceId, _: &FixtureArg, _: &ReturnState<u32, Memory>,
        _: &Cx<'_, ProbeHost>, _: &mut Effects<'_, ProbeHost>) -> Box<dyn Screen<ProbeHost>> {
        Box::new(self.clone())
    }
}
#[derive(Default)]
struct ProbeRig {
    probe: Probe,
    current_return: Memory,
    executed: Vec<(bool, u32)>,
}
impl Rig<ProbeHost> for ProbeRig {
    fn split(&mut self) -> Split<'_, ProbeHost> {
        Split { mounter: &mut self.probe, views: (), measure: &FixtureMeasure }
    }
    fn deliver(&mut self, _: MachineId, _: &(), _: &CxParts<u32>, _: &mut Effects<'_, ProbeHost>) -> Handled { Handled::No }
    fn timer(&mut self, _: MachineId, _: TimerId, _: &CxParts<u32>, _: &mut Effects<'_, ProbeHost>) {}
    fn app_return(&mut self, _: MachineId, ret: ReturnState<u32, Memory>) { self.current_return = ret.memory; }
    fn app_fx(&mut self, _: MachineId, fx: AppFx, _: &CxParts<u32>, _: &mut Effects<'_, ProbeHost>) {
        self.executed.push((matches!(fx, AppFx::Library(LibraryReq::Account)), self.current_return.0));
    }
    fn log(&mut self, _: &str) {}
    fn prepare(&mut self, _: &mut Budget, _: &mut Present) {}
    fn ls2_pump(&mut self) {}
    fn opaque_route(&mut self, _: bool) {}
    fn clear_opaque_region(&mut self) {}
    fn now_us(&self) -> u64 { 0 }
}
fn booted() -> (Dispatcher<ProbeHost>, ProbeRig) {
    let mut d = Dispatcher::new();
    let mut rig = ProbeRig::default();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Page(1)));
    d.frame(&mut rig, Tick::default(), vec![], vec![], &mut NoTap);
    rig.probe.captures.set(0);
    (d, rig)
}
fn housekeeping() -> Vec<AppFx> {
    let target = SectionAddress { epoch: 0, sid: crate::plex::ServerId::from_raw(0), section: 1 };
    vec![
        AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            target, work: LibraryWork::Want { lo: 0, hi: 24 },
        })),
        AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed { target, work: LibraryWork::Letters })),
        AppFx::Library(LibraryReq::PublishShelves { target, hidden_page: false, at_head: false }),
        AppFx::StoreWork(StoreWork::Browse),
    ]
}
fn enqueue(d: &mut Dispatcher<ProbeHost>, fx: AppFx, absorb: bool) {
    if absorb { d.absorb(vec![Stamped { from: MachineId::Nav, fx: Fx::App(fx) }]); }
    else { d.emit(MachineId::Nav, Fx::App(fx)); }
}
fn drain(d: &mut Dispatcher<ProbeHost>, rig: &mut ProbeRig, max: u32) {
    d.drain(rig, &d.parts(Tick::default()), max, &mut FrameReport::default(), &mut NoTap);
}

#[test]
fn housekeeping_never_captures_page_memory_at_emission_or_execution() {
    let _guard = crate::testlock::serial();
    for absorb in [false, true] {
        let (mut d, mut rig) = booted();
        rig.probe.value.set(525);
        for fx in housekeeping() { enqueue(&mut d, fx, absorb); }
        drain(&mut d, &mut rig, 100);
        assert_eq!(rig.probe.captures.get(), 0, "absorb={absorb}: housekeeping copied the page bookmark");
        assert_eq!(rig.executed, vec![(false, 0); 4]);
    }
}

#[test]
fn interleaved_housekeeping_keeps_navigation_bookmarks_at_emission_time_across_carry() {
    let _guard = crate::testlock::serial();
    for absorb in [false, true] {
        let (mut d, mut rig) = booted();
        rig.probe.value.set(11);
        enqueue(&mut d, housekeeping().remove(0), absorb);
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), absorb);
        rig.probe.value.set(22);
        enqueue(&mut d, housekeeping().remove(2), absorb);
        enqueue(&mut d, AppFx::Library(LibraryReq::Account), absorb);
        enqueue(&mut d, housekeeping().remove(3), absorb);
        rig.probe.value.set(99);
        for _ in 0..5 { drain(&mut d, &mut rig, 1); }
        assert_eq!(rig.executed, [(false, 0), (true, 11), (false, 0), (true, 22), (false, 0)],
            "absorb={absorb}: housekeeping stole a later bookmark or reused a prior one");
        assert_eq!(rig.probe.captures.get(), 2, "only the two navigation requests need bookmarks");
        assert!(d.app_returns.is_empty());
    }
}
