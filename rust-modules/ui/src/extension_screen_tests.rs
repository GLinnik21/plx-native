//! A new screen composes existing navigation/store effects without extending any effect enum,
//! classifier, application runner or CI list. Its only registration is its own mounter.
use super::*;
use crate::dispatch::{Dispatcher, FrameReport, NoTap, Tap};

struct ExtensionScreen { inner: Box<dyn Screen<FixtureHost>> }
impl Machine<FixtureHost> for ExtensionScreen {
    type Ev = ScreenEvent<FixtureHost>;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, FixtureHost>, fx: &mut Effects<'_, FixtureHost>) -> Handled {
        if matches!(ev, ScreenEvent::Activate(1)) {
            fx.push(Fx::App(FixtureFx::StoreAdd(42)));
            fx.push(Fx::Nav(plx_machine::machine::NavOp::Push(FixtureArg::Page(2))));
            Handled::Yes
        } else { self.inner.step(ev, cx, fx) }
    }
}
impl Focusable<FixtureHost> for ExtensionScreen {
    fn groups(&self, cx: &Cx<'_, FixtureHost>, out: &mut Vec<GroupSpec>) { self.inner.groups(cx, out); }
    fn group_of(&self, key: &u32, cx: &Cx<'_, FixtureHost>) -> Option<GroupId> { self.inner.group_of(key, cx) }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, FixtureHost>) -> Step<u32> { self.inner.neighbour(key, dir, cx) }
    fn place(&self, key: &u32, cx: &Cx<'_, FixtureHost>, at: At) -> Option<Placed> { self.inner.place(key, cx, at) }
    fn reconcile(&self, key: FocusKey<u32>, cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> { self.inner.reconcile(key, cx) }
    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> { self.inner.seat(group, from, cx) }
}
impl Screen<FixtureHost> for ExtensionScreen {
    fn name(&self) -> &'static str { "extension" }
    fn state(&self) -> &dyn LogicalState { self.inner.state() }
    fn crumb(&self, cx: &Cx<'_, FixtureHost>) -> Option<std::borrow::Cow<'_, str>> { self.inner.crumb(cx) }
    fn prepare(&mut self, budget: &mut Budget, cx: &Cx<'_, FixtureHost>) { self.inner.prepare(budget, cx); }
    fn draw(&mut self, frame: &mut DrawFrame<'_, '_, FixtureHost>) { self.inner.draw(frame); }
    fn render(&self) -> crate::screen::RenderStrategy { self.inner.render() }
}
struct ExtensionMounter(FixtureMounter);
impl Mounter<FixtureHost> for ExtensionMounter {
    fn mount(&mut self, id: plx_machine::machine::InstanceId, arg: &FixtureArg, ret: &ReturnState<u32>,
        cx: &Cx<'_, FixtureHost>, fx: &mut Effects<'_, FixtureHost>) -> Box<dyn Screen<FixtureHost>> {
        let inner = self.0.mount(id, arg, ret, cx, fx);
        if *arg == FixtureArg::Page(1) { Box::new(ExtensionScreen { inner }) } else { inner }
    }
}
struct ExtensionRig { inner: FixtureRig, mounter: ExtensionMounter }
impl Rig<FixtureHost> for ExtensionRig {
    fn split(&mut self) -> Split<'_, FixtureHost> {
        let split = self.inner.split();
        Split { mounter: &mut self.mounter, views: split.views, measure: split.measure }
    }
    fn deliver(&mut self, to: MachineId, msg: &FixtureMsg, cx: &CxParts<u32>, fx: &mut Effects<'_, FixtureHost>) -> Handled {
        self.inner.deliver(to, msg, cx, fx)
    }
    fn timer(&mut self, owner: MachineId, id: plx_machine::machine::TimerId, cx: &CxParts<u32>, fx: &mut Effects<'_, FixtureHost>) {
        self.inner.timer(owner, id, cx, fx);
    }
    fn app_fx(&mut self, from: MachineId, fx: FixtureFx, cx: &CxParts<u32>, out: &mut Effects<'_, FixtureHost>) {
        self.inner.app_fx(from, fx, cx, out);
    }
    fn log(&mut self, line: &str) { Rig::log(&mut self.inner, line); }
    fn prepare(&mut self, budget: &mut Budget, present: &mut Present) { self.inner.prepare(budget, present); }
    fn ls2_pump(&mut self) { self.inner.ls2_pump(); }
    fn opaque_route(&mut self, bound: bool) { self.inner.opaque_route(bound); }
    fn clear_opaque_region(&mut self) { self.inner.clear_opaque_region(); }
    fn now_us(&self) -> u64 { self.inner.now_us() }
    fn run_yielded_app_command(&mut self, _: &mut Dispatcher<FixtureHost>, _: &mut FrameReport, _: &mut dyn Tap<FixtureHost>) -> Option<u32> {
        panic!("generic store/navigation effects must not ask a new screen for an application runner")
    }
}
#[test]
fn a_new_screen_uses_generic_store_and_navigation_without_central_effect_changes() {
    let mut rig = ExtensionRig { inner: FixtureRig::new(), mounter: ExtensionMounter(FixtureMounter { mounted: 0 }) };
    let mut d = Dispatcher::<FixtureHost>::new();
    d.request(MachineId::Nav, plx_machine::machine::NavOp::Root(FixtureArg::Page(1)));
    d.frame(&mut rig, Tick::default(), vec![], vec![], &mut NoTap);
    let sender = MachineId::Instance(d.top_page().unwrap());
    d.emit(sender, Fx::Deliver(sender, plx_machine::machine::Delivery::Screen(ScreenEvent::Activate(1))));
    d.frame(&mut rig, Tick::default(), vec![], vec![], &mut NoTap);
    assert_eq!(d.top_arg(), Some(&FixtureArg::Page(2)));
    assert!(rig.inner.store.view.items.contains(&42));
    assert!(d.pending_app_command().is_none());
    assert_eq!(d.queued(), 0);
}
