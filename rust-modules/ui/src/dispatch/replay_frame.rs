//! Explicit direct-driver replay capability. Product frames use their resource-owning Rig instead.
use super::*;

/// Borrow a direct driver's recorder, never an arbitrary passive caller, as replay authority.
/// Yielded commands remain pending: the direct driver must resume them through this same rig.
/// A second owned capability is rejected rather than selected, replaced or recorded twice.
pub struct ReplayFrame<'a, H: Host> {
    rig: &'a mut dyn Rig<H>,
    replay: &'a mut dyn ReplayControl<H>,
}
impl<'a, H: Host> ReplayFrame<'a, H> {
    pub fn new(rig: &'a mut dyn Rig<H>, replay: &'a mut dyn ReplayControl<H>) -> Self {
        assert!(matches!(rig.dispatch_tap(), FrameObservers::Caller), "frame already has an owned observer/authority");
        Self { rig, replay }
    }
}
impl<H: Host> Rig<H> for ReplayFrame<'_, H> {
    fn dispatch_tap(&mut self) -> FrameObservers<'_, H> { FrameObservers::Replay(self.replay) }
    fn split(&mut self) -> Split<'_, H> { self.rig.split() }
    fn deliver(&mut self, to: MachineId, msg: &H::Msg, parts: &CxParts<H::Elem>, fx: &mut Effects<'_, H>) -> Handled {
        self.rig.deliver(to, msg, parts, fx)
    }
    fn deliver_from(&mut self, from: MachineId, to: MachineId, msg: &H::Msg, parts: &CxParts<H::Elem>, fx: &mut Effects<'_, H>) -> Handled {
        self.rig.deliver_from(from, to, msg, parts, fx)
    }
    fn timer(&mut self, owner: MachineId, id: TimerId, parts: &CxParts<H::Elem>, fx: &mut Effects<'_, H>) {
        self.rig.timer(owner, id, parts, fx);
    }
    fn app_fx(&mut self, from: MachineId, fx: plx_machine::machine::InlineFx<H>, parts: &CxParts<H::Elem>, out: &mut Effects<'_, H>) {
        self.rig.app_fx(from, fx, parts, out);
    }
    fn app_return(&mut self, from: MachineId, ret: ReturnState<H::Elem, H::Memory>) { self.rig.app_return(from, ret); }
    fn app_command_done(&mut self, id: AppCommandId, from: MachineId, outcome: AppCommandOutcome) {
        self.rig.app_command_done(id, from, outcome);
    }
    fn system_keyboard(&mut self, up: bool) { self.rig.system_keyboard(up); }
    fn adopt_system_keyboard(&mut self) { self.rig.adopt_system_keyboard(); }
    fn navigation_presentation(&self, nav: &Navigation<H>) -> super::super::screen::NavPresentation { self.rig.navigation_presentation(nav) }
    fn draw_chrome(&mut self, arg: &H::Arg, parts: &CxParts<H::Elem>, nav: super::super::screen::NavPresentation,
        glass: Option<&mut super::super::frame::glass::GlassPlan>, lifted: Option<H::Elem>) {
        self.rig.draw_chrome(arg, parts, nav, glass, lifted);
    }
    fn bar_member_rect(&self, elem: H::Elem, nav: super::super::screen::NavPresentation) -> Option<super::super::Rect> {
        self.rig.bar_member_rect(elem, nav)
    }
    fn lift_bar_member(&mut self, elem: H::Elem, nav: super::super::screen::NavPresentation,
        material: Option<plx_gfx::gfx::GlassFace>, ground: &super::super::underlay::UnderlayField) {
        self.rig.lift_bar_member(elem, nav, material, ground);
    }
    fn page_updates(&self) -> bool { self.rig.page_updates() }
    fn surface_scope(&mut self) -> Option<super::super::popover::host::Live> { self.rig.surface_scope() }
    fn log(&mut self, line: &str) { self.rig.log(line); }
    fn prepare(&mut self, budget: &mut Budget, present: &mut Present) { self.rig.prepare(budget, present); }
    fn ls2_pump(&mut self) { self.rig.ls2_pump(); }
    fn opaque_route(&mut self, bound: bool) { self.rig.opaque_route(bound); }
    fn clear_opaque_region(&mut self) { self.rig.clear_opaque_region(); }
    fn now_us(&self) -> u64 { self.rig.now_us() }
    fn back_at_root(&mut self) { self.rig.back_at_root(); }
}
