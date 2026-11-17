use super::*;
use crate::fixture::{FixtureHost, FixtureMsg, FixtureRig};

#[derive(Default)]
struct Audit { calls: Vec<&'static str>, hashes: Vec<u64>, authority_calls: usize, trap: bool }
impl Tap<FixtureHost> for Audit {
    fn tick(&mut self, _: u64, _: Tick) { self.calls.push("tick"); }
    fn input(&mut self, _: u64, _: &InputEvent<u32>) { self.calls.push("input"); }
    fn result(&mut self, _: u64, _: &Addr, _: &FixtureMsg) { self.calls.push("result"); }
    fn presentation(&mut self, _: u64, _: u64, _: EntryId, _: InstanceId, _: &FixtureMsg) { self.calls.push("presentation"); }
    fn effect(&mut self, _: u64, _: &Stamped<FixtureHost>) { self.calls.push("effect"); }
    fn focus(&mut self, _: u64, _: Option<(u32, u32, Option<u32>)>) { self.calls.push("focus"); }
    fn state(&mut self, _: u64, hash: &mut dyn FnMut() -> u64) {
        self.calls.push("state"); self.hashes.extend([hash(), hash()]);
    }
    fn present(&mut self, _: u64, _: bool, _: Option<plx_machine::present::Provenance>) { self.calls.push("present"); }
    fn frame_done(&mut self, _: u64) { self.calls.push("done"); }
    fn command_done(&mut self, _: u64, _: AppCommandId, _: MachineId, _: AppCommandOutcome) { self.calls.push("command"); }
}
impl ReplayAuthority<FixtureHost> for Audit {
    fn resolution_mode(&self) -> Option<bool> { assert!(!self.trap, "passive caller obtained replay authority"); Some(false) }
    fn resolve_focus(&mut self, _: u64, _: u8, _: EntryId, actual: Option<FocusAnswer<u32>>) -> Option<FocusAnswer<u32>> {
        assert!(!self.trap, "passive caller answered a replay decision"); self.authority_calls += 1; actual
    }
}

#[test]
fn owned_and_caller_observe_every_callback_once_but_only_owner_answers() {
    let mut rig = FixtureRig::new();
    let mut owned = Audit::default();
    let mut caller = Audit { trap: true, ..Default::default() };
    let mut walks = 0;
    {
        let mut frame = ReplayFrame::new(&mut rig, &mut owned);
        let mut observers = frame_tap(&mut frame, &mut caller);
        observers.tick(1, Tick::default());
        observers.input(1, &InputEvent { at: Tick::default(), source: plx_machine::machine::Source::Replay,
            kind: InputKind::SystemKeyboard(false) });
        let msg = FixtureMsg::Store(plx_machine::machine::StoreOrd(0), 1);
        observers.result(1, &Addr { to: MachineId::Nav, req: plx_machine::machine::RequestId(1) }, &msg);
        observers.presentation(1, 0, EntryId(1), InstanceId(1), &msg);
        observers.effect(1, &Stamped { from: MachineId::Nav, fx: Fx::CancelTimer(TimerId(1)) });
        observers.focus(1, None);
        observers.state(1, &mut || { walks += 1; 42 });
        observers.present(1, false, None);
        observers.command_done(1, AppCommandId(1), MachineId::Nav, AppCommandOutcome::Completed);
        observers.frame_done(1);
        assert_eq!(frame_authority(&mut frame, |a| a.resolution_mode()), Some(false));
        assert_eq!(frame_authority(&mut frame, |a| a.resolve_focus(1, 0, EntryId(1), None)), None);
    }
    let expected = ["tick", "input", "result", "presentation", "effect", "focus", "state", "present", "command", "done"];
    assert_eq!(owned.calls, expected);
    assert_eq!(caller.calls, expected);
    assert_eq!(owned.hashes, [42, 42]);
    assert_eq!(caller.hashes, [42, 42]);
    assert_eq!(walks, 1, "two observers and repeated reads still walk logical state once");
    assert_eq!(owned.authority_calls, 1);
    assert_eq!(caller.authority_calls, 0);
}

#[test]
fn unarmed_observers_do_not_evaluate_the_hash() {
    let mut rig = FixtureRig::new();
    frame_tap(&mut rig, &mut NoTap).state(1, &mut || panic!("unarmed state walk"));
}

#[test]
#[should_panic(expected = "frame already has an owned observer/authority")]
fn a_second_explicit_replay_authority_is_rejected() {
    let mut rig = FixtureRig::new();
    let mut first = Audit::default();
    let mut second = Audit::default();
    let mut frame = ReplayFrame::new(&mut rig, &mut first);
    let _second = ReplayFrame::new(&mut frame, &mut second);
}
