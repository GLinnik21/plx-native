//! An unregistered screen speaks the existing product effects, not a fixture Host or route enum.
use super::*;
use plx_machine::machine::{Canon, Cx, Effects, FocusKey, Fx, GroupId, Handled, LogicalState, Machine, NavOp};
use plx_ui::frame::Budget;
use plx_ui::screen::{At, Dir, DrawFrame, Focusable, GroupSpec, Placed, Screen, ScreenEvent, Step};

#[derive(Default)]
struct Extension { fired: bool }
impl LogicalState for Extension {
    fn write(&self, c: &mut Canon) { c.bool(self.fired); }
    fn probe(&self, out: &mut String) { out.push_str(if self.fired { "fired" } else { "ready" }); }
}
impl Machine<AppHost> for Extension {
    type Ev = ScreenEvent<AppHost>;
    fn step(&mut self, event: &Self::Ev, _: &Cx<'_, AppHost>, fx: &mut Effects<'_, AppHost>) -> Handled {
        if matches!(event, ScreenEvent::Tick(_)) && !self.fired {
            self.fired = true;
            fx.push(Fx::App(AppFx::Store(plx_data::stores::StoreId::Metadata,
                plx_data::stores::StoreCmd::Metadata(plx_data::stores::metadata::MetadataCmd::Clear))));
            fx.push(Fx::Nav(NavOp::Push(AppArg::Search)));
            return Handled::Yes;
        }
        Handled::No
    }
}
impl Focusable<AppHost> for Extension {
    fn groups(&self, _: &Cx<'_, AppHost>, _: &mut Vec<GroupSpec>) {}
    fn group_of(&self, _: &u32, _: &Cx<'_, AppHost>) -> Option<GroupId> { None }
    fn neighbour(&self, _: FocusKey<u32>, _: Dir, _: &Cx<'_, AppHost>) -> Step<u32> { Step::Edge }
    fn place(&self, _: &u32, _: &Cx<'_, AppHost>, _: At) -> Option<Placed> { None }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, AppHost>) -> FocusKey<u32> { want }
    fn seat(&self, _: GroupId, _: Placed, _: &Cx<'_, AppHost>) -> FocusKey<u32> { unreachable!() }
}
impl Screen<AppHost> for Extension {
    fn name(&self) -> &'static str { "unregistered-product-extension" }
    fn state(&self) -> &dyn LogicalState { self }
    fn crumb(&self, _: &Cx<'_, AppHost>) -> Option<std::borrow::Cow<'_, str>> { None }
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, AppHost>) {}
    fn draw(&mut self, _: &mut DrawFrame<'_, '_, AppHost>) {}
    fn render(&self) -> plx_ui::screen::RenderStrategy { plx_ui::screen::RenderStrategy::Page }
    fn memory_at(&self, _: Option<FocusKey<u32>>) -> plx_screens::registry::PageMemory { Default::default() }
}

struct ExtensionMounter(plx_screens::registry::AppMounter);
impl plx_ui::screen::Mounter<AppHost> for ExtensionMounter {
    fn mount(&mut self, id: plx_machine::machine::InstanceId, arg: &AppArg,
        ret: &plx_ui::screen::ReturnState<u32, plx_screens::registry::PageMemory>,
        cx: &Cx<'_, AppHost>, fx: &mut Effects<'_, AppHost>) -> Box<dyn Screen<AppHost>> {
        if matches!(arg, AppArg::Home) { Box::<Extension>::default() }
        else { self.0.mount(id, arg, ret, cx, fx) }
    }
}
struct ExtensionRig<'loan, 'owners> {
    bridge: &'loan mut Bridge<'owners>,
    mounter: ExtensionMounter,
}
impl plx_ui::dispatch::Rig<AppHost> for ExtensionRig<'_, '_> {
    fn split(&mut self) -> plx_ui::dispatch::Split<'_, AppHost> {
        let split = plx_ui::dispatch::Rig::split(self.bridge);
        plx_ui::dispatch::Split { mounter: &mut self.mounter, views: split.views, measure: split.measure }
    }
    fn deliver(&mut self, to: MachineId, msg: &plx_screens::registry::AppMsg,
        cx: &plx_ui::dispatch::CxParts<u32>, fx: &mut Effects<'_, AppHost>) -> Handled {
        plx_ui::dispatch::Rig::deliver(self.bridge, to, msg, cx, fx)
    }
    fn timer(&mut self, owner: MachineId, id: plx_machine::machine::TimerId,
        cx: &plx_ui::dispatch::CxParts<u32>, fx: &mut Effects<'_, AppHost>) {
        plx_ui::dispatch::Rig::timer(self.bridge, owner, id, cx, fx);
    }
    fn app_fx(&mut self, from: MachineId, effect: plx_screens::registry::InlineAppFx,
        cx: &plx_ui::dispatch::CxParts<u32>, fx: &mut Effects<'_, AppHost>) {
        plx_ui::dispatch::Rig::app_fx(self.bridge, from, effect, cx, fx);
    }
    fn log(&mut self, line: &str) { plx_ui::dispatch::Rig::log(self.bridge, line); }
    fn prepare(&mut self, budget: &mut Budget, present: &mut plx_machine::present::Present) {
        plx_ui::dispatch::Rig::prepare(self.bridge, budget, present);
    }
    fn ls2_pump(&mut self) { plx_ui::dispatch::Rig::ls2_pump(self.bridge); }
    fn opaque_route(&mut self, bound: bool) { plx_ui::dispatch::Rig::opaque_route(self.bridge, bound); }
    fn clear_opaque_region(&mut self) { plx_ui::dispatch::Rig::clear_opaque_region(self.bridge); }
    fn now_us(&self) -> u64 { plx_ui::dispatch::Rig::now_us(self.bridge) }
}

#[test]
fn an_unregistered_product_screen_uses_generic_navigation_and_actual_store_effects() {
    let _serial = plx_base::testlock::serial();
    let mut owner = crate::app::runtime_owners::Harness::new(|| 0);
    let mut bridge = owner.bridge();
    let mut dispatcher = Dispatcher::<AppHost>::new();
    nav_root(&mut dispatcher, AppArg::Home);
    let mut rig = ExtensionRig { bridge: &mut bridge, mounter: ExtensionMounter(Default::default()) };
    dispatcher.frame(&mut rig, super::test_support::tick(0), vec![], vec![], &mut plx_ui::dispatch::NoTap);
    let id = dispatcher.nav.top_page().unwrap().id;
    assert_eq!(dispatcher.nav.top_page().unwrap().inst.as_ref().unwrap().screen.name(), "unregistered-product-extension");
    plx_data::metadata::set_current_for_test(rig.bridge.metadata_mut().state_mut(),
        Some(plx_data::metadata::Detail { rk: "extension-owned".into(), ..Default::default() }));
    assert!(rig.bridge.metadata_view().current().is_some());
    dispatcher.frame(&mut rig, super::test_support::tick(1), vec![], vec![], &mut plx_ui::dispatch::NoTap);
    assert!(rig.bridge.metadata_view().current().is_none(), "the actual Metadata child consumed the generic Store command");
    assert!(matches!(dispatcher.top_arg(), Some(AppArg::Search)), "generic NavOp mounted through the existing product mounter");
    assert_ne!(dispatcher.nav.top_page().unwrap().id, id);
    assert_eq!(dispatcher.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.name(), "unregistered-product-extension");
    assert!(dispatcher.pending_app_command().is_none());
    assert_eq!(dispatcher.queued(), 0);
    rig.bridge.assert_no_store_jobs_left_for_test();
}
