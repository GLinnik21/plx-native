//! The `Input` machine's HOME (restructure spec §2.2): the press machine, the focus engine, the
//! hit map, the press ARM (who armed what, from a key or the pointer, holdable or not), and the
//! television-keyboard owner flag. [`Input`] is the product's field today — the ONE owner of
//! the press, reached by the legacy key ladders as `&mut Press` (spec §14, the press facade).
//! [`InputMachine`] is the same machine generic over the element key, as the dispatcher owns
//! it (phase 3b); the two merge when the loop swaps onto the dispatcher.
//!
//! The press protocol (§7.4): `Fx::Press(PressArm)` arms; `Input` steps the press on `Tick`,
//! `Edge::Repeat` and `Up`, resolves dropped key-ups (`LOST_MS`, `MAX_HOLD_MS`), cancels on
//! navigation, owner change or a pointer press whose hit leaves its arm; at `LONG_MS` it
//! delivers `PressHold(id)` — `Handled::Yes` cancels the press (the item menu opened),
//! `Handled::No` latches it non-committing — and on release `PressCommit(id)` to the arming
//! owner only.
use super::press::Press;

pub(crate) struct Input {
    pub(crate) press: Press,
}

impl Input {
    pub(crate) const fn new() -> Self {
        Self {
            press: Press::new(),
        }
    }
}

use std::hash::Hash;

use super::focus::FocusEngine;
use super::hit::HitMap;
use super::machine::{Canon, FocusKey, LogicalState, MachineId, PressArm, PressFrom, PressId};

/// One armed press (§7.4).
#[derive(Clone, Copy, Debug)]
pub struct Arm<K> {
    pub id: PressId,
    pub key: FocusKey<K>,
    pub from: PressFrom,
    pub holdable: bool,
    /// The machine that armed it — the only one that hears `PressHold`/`PressCommit`.
    pub owner: MachineId,
    /// `PressHold` was delivered once.
    pub held_delivered: bool,
}

/// What the press machine asks the dispatcher to deliver after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressEvent {
    Hold(PressId, MachineId),
    Commit(PressId, MachineId),
}

pub struct InputMachine<K> {
    pub engine: FocusEngine<K>,
    pub hit: HitMap<K>,
    pub press: Press,
    pub arm: Option<Arm<K>>,
    next_press: u32,
    /// The television's keyboard is up: `InputOwner::System(Keyboard)`.
    pub keyboard: bool,
}

impl<K: Copy + Eq + Hash> Default for InputMachine<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash> InputMachine<K> {
    pub fn new() -> Self {
        Self {
            engine: FocusEngine::new(),
            hit: HitMap::new(),
            press: Press::new(),
            arm: None,
            next_press: 0,
            keyboard: false,
        }
    }

    /// Arm a press for `owner` at `now`: a new id; a holdable card or a non-holdable control.
    pub fn arm(&mut self, a: PressArm<K>, owner: MachineId, now: u32) -> PressId {
        self.next_press += 1;
        let id = PressId(self.next_press);
        if a.holdable {
            self.press.begin(now);
        } else {
            self.press.begin_ctl(now);
        }
        self.arm = Some(Arm {
            id,
            key: a.key,
            from: a.from,
            holdable: a.holdable,
            owner,
            held_delivered: false,
        });
        id
    }

    /// Abandon the press: navigation, an owner change, a pointer that left the arm.
    pub fn cancel_press(&mut self) {
        if self.arm.take().is_some() {
            self.press.cancel();
        }
    }

    /// The physical release.
    pub fn release(&mut self, now: u32) {
        self.press.release(now);
    }

    /// An auto-repeat beat: the dropped-key-up net's liveness.
    pub fn note_alive(&mut self, now: u32) {
        self.press.note_alive(now);
    }

    /// One frame: the press spring and the hold/commit decisions.
    pub fn tick(&mut self, now: u32, dt: f32) -> Vec<PressEvent> {
        let mut out = Vec::new();
        self.press.tick(now, dt);
        let Some(arm) = self.arm.as_mut() else {
            return out;
        };
        if arm.holdable && !arm.held_delivered && self.press.is_long(now) {
            arm.held_delivered = true;
            out.push(PressEvent::Hold(arm.id, arm.owner));
        }
        if self.press.take_commit(now) {
            out.push(PressEvent::Commit(arm.id, arm.owner));
            self.arm = None;
        } else if !self.press.is_active() {
            // sprung back with nothing to deliver (a cancelled or latched hold)
            self.arm = None;
        }
        out
    }

    /// The Input machine's logical state (§2.2): the press and the engine; the hit map is a
    /// render-side resource and the arm is transient.
    pub fn write_with(&self, c: &mut Canon, elem: &dyn Fn(&K, &mut Canon)) {
        self.press.write(c);
        self.engine.write_with(c, elem);
        c.bool(self.keyboard);
        c.option(self.arm.as_ref(), |c, a| {
            c.u32(a.id.0).bool(a.holdable);
        });
    }
}
