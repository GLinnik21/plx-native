//! Adapters as a TRAIT (restructure spec §2.2, §3.1): the application's `Fx::App(H::Fx)` effects
//! leave the machines through exactly one door, `Adapters::execute`, which holds the OS/FFI
//! resources (sockets, disk, the engine slot, the poster workers, the measurer) and never any
//! logical state. The dispatcher reaches it through `Rig::app_fx`; an application's rig forwards
//! to its adapter set, and a test's rig forwards to a [`StubAdapters`] that answers by script.
//!
//! Phase 2 declares the door and the stub; `app/adapters/{net,disk,sys,poster}.rs` arrive with
//! the stores that emit to them (phases 3a and 4). `FixtureRig` is the first implementor.

use super::dispatch::CxParts;
use super::machine::{Effects, Host, MachineId};

/// The one door out of the machine world.
pub trait Adapters<H: Host> {
    /// Execute one application effect emitted by `from`. A result that must come back to a
    /// machine goes through the landing (a later frame's step 3) or, for a synchronous answer,
    /// as an effect pushed onto `out` (delivered in this drain).
    fn execute(&mut self, from: MachineId, fx: H::Fx, parts: &CxParts<H::Elem>, out: &mut Effects<'_, H>);
}

/// A scripted adapter set for tests: every effect is logged by the caller-supplied `describe`,
/// and an optional `answer` closure produces the synchronous reply. Replay's stub adapter
/// (spec §5.5, phase 4) is this with the recording as its script.
#[cfg(test)]
pub struct StubAdapters<H: Host> {
    pub executed: Vec<(MachineId, String)>,
    describe: fn(&H::Fx) -> String,
    #[allow(clippy::type_complexity)]
    answer: Option<Box<dyn FnMut(MachineId, H::Fx, &CxParts<H::Elem>, &mut Effects<'_, H>)>>,
}

#[cfg(test)]
impl<H: Host> StubAdapters<H> {
    pub fn new(describe: fn(&H::Fx) -> String) -> Self {
        Self {
            executed: Vec::new(),
            describe,
            answer: None,
        }
    }

    pub fn answering(
        mut self,
        f: impl FnMut(MachineId, H::Fx, &CxParts<H::Elem>, &mut Effects<'_, H>) + 'static,
    ) -> Self {
        self.answer = Some(Box::new(f));
        self
    }
}

#[cfg(test)]
impl<H: Host> Adapters<H> for StubAdapters<H> {
    fn execute(&mut self, from: MachineId, fx: H::Fx, parts: &CxParts<H::Elem>, out: &mut Effects<'_, H>) {
        self.executed.push((from, (self.describe)(&fx)));
        if let Some(a) = self.answer.as_mut() {
            a(from, fx, parts, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::{FixtureFx, FixtureHost, FixtureMsg};
    use crate::ui::machine::{Delivery, Fx, StoreOrd};

    /// The stub logs what it was asked and answers by script; the answer lands in the same drain
    /// (an effect on `out`), which is the synchronous half of an adapter's contract.
    #[test]
    fn a_stub_adapter_logs_the_effect_and_answers_by_script() {
        let mut stub: StubAdapters<FixtureHost> = StubAdapters::new(|fx| match fx {
            FixtureFx::StoreAdd(v) => format!("store-add:{v}"),
            FixtureFx::Net(_) => "net".to_string(),
            FixtureFx::Poster(k) => format!("poster:{}", k.0),
        })
        .answering(|_from, fx, _parts, out| {
            if let FixtureFx::StoreAdd(v) = fx {
                out.push(Fx::Deliver(
                    MachineId::Store(StoreOrd(0)),
                    Delivery::Machine(FixtureMsg::Store(StoreOrd(0), v + 1)),
                ));
            }
        });
        let parts = CxParts {
            tick: crate::ui::machine::Tick::default(),
            press: Default::default(),
            focus: Default::default(),
            owner: crate::ui::machine::InputOwner::Entry(crate::ui::machine::EntryId(0)),
        };
        let mut present = crate::ui::present::Present::new();
        let mut buf = Vec::new();
        let mut out = Effects::new(&mut buf, MachineId::Nav, &mut present);
        stub.execute(MachineId::Nav, FixtureFx::StoreAdd(6), &parts, &mut out);
        assert_eq!(out.emitted(), 1, "the script answered with one effect");
        assert_eq!(stub.executed, vec![(MachineId::Nav, "store-add:6".to_string())]);
    }
}
