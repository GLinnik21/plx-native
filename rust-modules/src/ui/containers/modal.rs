//! `ModalStack` (restructure spec §6.2): a modal surface IS an `Entry` — same `EntryId`, same
//! `Instance`, same `Screen` contract, mounted by the same `Mounter`. The container is the ONE
//! owner of a surface's PHASE (`Hidden | Opening | Open | Closing`); `Popover`'s `open`/`closing`/
//! `dismiss`/`visible` flags are what it replaces, one module at a time (§14). `PopoverMotion` is
//! the appear spring the panel rides; the scrim maths, the `Opener` lift and the painter stay
//! with the legacy `Popover` until each popover's phase.
//!
//! - `input_owner()` = the topmost Opening|Open surface.
//! - `prune()` is the ONLY place Closing clears, so Closing surfaces step unconditionally (the
//!   fade must finish whatever the host is doing) — `the_closing_phase_is_stepped_even_when_the_host_is_frozen`.
//! - `on_miss(style)` is consulted only while the surface is `Open`: a click beside a Compact,
//!   Sheet or PlayerPanel dismisses it; an Alert ignores the miss; an Opaque surface swallows it.
//! - `host_policy()` folds bottom-to-top into the `(HostUpdate, HostRender)` pair the frame reads.

use super::super::machine::{EntryId, Host, InputOwner, Leave, PresentHandle, Tick};
use super::super::motion;
use super::super::machine::GroupId;
use super::super::screen::{Enter, FocusTarget, ReturnState, ScreenEvent};
use super::stack::{Entry, Instance};
use super::{Life, Minter};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Hidden,
    Opening,
    Open,
    Closing,
}

/// A surface's SHAPE, which decides what its host does beneath it and what a miss means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// A compact popover on a live host (the item menu).
    Compact,
    /// A sheet that freezes and caches its host (the account menu).
    Sheet,
    /// A decision alert: a miss beside it is ignored.
    Alert,
    /// A full-screen surface with an opaque ground (Settings, first-run consent). `snapshot`:
    /// the host is cached while the ground is not yet drawn (Settings) rather than left live
    /// (first-run consent, whose host drew nothing worth a snapshot).
    Opaque { snapshot: bool },
    /// A panel over the player's video plane; `survives_failure` keeps it up over the read-out.
    PlayerPanel { survives_failure: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HostUpdate {
    Live,
    Frozen,
}

/// Ordered: a fold takes the most severe.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum HostRender {
    Live,
    Cached,
    Replaced,
}

/// What a miss beside a surface does (§6.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OnMiss {
    Dismiss,
    Nothing,
    Swallow,
}

/// The appear spring (the `Popover` choreography's first half, pure).
#[derive(Clone, Copy, Debug)]
pub struct PopoverMotion {
    pub appear: f32,
    vel: f32,
    target: f32,
}

/// The appear spring's stiffness (`popover.rs`'s number).
pub const APPEAR_K: f32 = 300.0;

impl PopoverMotion {
    pub const fn at(v: f32) -> Self {
        Self {
            appear: v,
            vel: 0.0,
            target: v,
        }
    }
    pub fn to(&mut self, target: f32) {
        self.target = target;
    }
    pub fn settled(&self) -> bool {
        (self.appear - self.target).abs() < 0.002 && self.vel.abs() < 0.02
    }
    pub fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) {
        motion::spring(&mut self.appear, &mut self.vel, self.target, APPEAR_K, t, present);
        if self.settled() {
            self.appear = self.target;
            self.vel = 0.0;
        }
    }
}

pub struct Surface<H: Host> {
    pub entry: Entry<H>,
    pub phase: Phase,
    pub style: Style,
    pub motion: PopoverMotion,
    /// An `Opaque` surface's ground has drawn: the host is `Replaced` from here.
    pub ground_ready: bool,
}

/// The `(update, render)` a single surface asks of its host (§6.2's table).
pub fn surface_policy(style: Style, phase: Phase, ground_ready: bool) -> (HostUpdate, HostRender) {
    use HostRender as R;
    use HostUpdate as U;
    match (style, phase) {
        (_, Phase::Hidden) => (U::Live, R::Live),
        (Style::Compact, _) => (U::Live, R::Cached),
        (Style::Sheet | Style::Alert, Phase::Opening | Phase::Open) => (U::Frozen, R::Cached),
        (Style::Sheet | Style::Alert, Phase::Closing) => (U::Live, R::Cached),
        (Style::Opaque { snapshot: true }, Phase::Opening) => (U::Frozen, R::Cached),
        (Style::Opaque { snapshot: true }, Phase::Open) => {
            (U::Frozen, if ground_ready { R::Replaced } else { R::Cached })
        }
        (Style::Opaque { snapshot: true }, Phase::Closing) => (U::Live, R::Cached),
        (Style::Opaque { snapshot: false }, Phase::Opening | Phase::Open) => {
            (U::Frozen, if ground_ready { R::Replaced } else { R::Live })
        }
        (Style::Opaque { snapshot: false }, Phase::Closing) => (U::Live, R::Live),
        (Style::PlayerPanel { .. }, _) => (U::Live, R::Live),
    }
}

/// What a miss beside a surface does, by style — consulted only while `Open`.
pub fn on_miss(style: Style) -> OnMiss {
    match style {
        Style::Compact | Style::Sheet | Style::PlayerPanel { .. } => OnMiss::Dismiss,
        Style::Alert => OnMiss::Nothing,
        Style::Opaque { .. } => OnMiss::Swallow,
    }
}

pub struct ModalStack<H: Host> {
    pub surfaces: Vec<Surface<H>>,
    /// Surfaces whose `Unmount` is owed (removed by `prune`).
    pub retired: Vec<Entry<H>>,
}

impl<H: Host> Default for ModalStack<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Host> ModalStack<H> {
    pub fn new() -> Self {
        Self {
            surfaces: Vec::new(),
            retired: Vec::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.surfaces.is_empty()
    }

    /// Present a surface (§3.4): mint the entry, `Mount` + `Enter(Fresh)`; the caller (`Navigation`)
    /// adds the host's `Cover` in the same drain.
    pub fn present(&mut self, ids: &mut Minter, arg: H::Arg, style: Style) -> (EntryId, Vec<Life<H>>) {
        let id = ids.entry();
        self.surfaces.push(Surface {
            entry: Entry {
                id,
                arg,
                ret: ReturnState::default(),
                inst: None,
                evicted: false,
            },
            phase: Phase::Opening,
            style,
            motion: PopoverMotion::at(0.0),
            ground_ready: false,
        });
        self.surfaces.last_mut().unwrap().motion.to(1.0);
        let out = vec![
            Life::Mount(id),
            Life::Ev(
                id,
                ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::ContainerGroup(GroupId(0)),
                }),
            ),
        ];
        (id, out)
    }

    /// Dismiss: the phase goes to `Closing` NOW (the input owner changes on this frame); the body
    /// leaves when `prune` clears it. Returns whether the id named an open surface.
    pub fn dismiss(&mut self, id: EntryId) -> bool {
        let Some(s) = self.surfaces.iter_mut().find(|s| s.entry.id == id) else {
            return false;
        };
        if s.phase == Phase::Closing {
            return false;
        }
        s.phase = Phase::Closing;
        s.motion.to(0.0);
        true
    }

    /// The topmost Opening|Open surface owns input.
    pub fn input_owner(&self) -> Option<InputOwner> {
        self.surfaces
            .iter()
            .rev()
            .find(|s| matches!(s.phase, Phase::Opening | Phase::Open))
            .map(|s| InputOwner::Entry(s.entry.id))
    }

    pub fn top(&self) -> Option<&Surface<H>> {
        self.surfaces.last()
    }

    pub fn surface(&self, id: EntryId) -> Option<&Surface<H>> {
        self.surfaces.iter().find(|s| s.entry.id == id)
    }

    pub fn surface_mut(&mut self, id: EntryId) -> Option<&mut Surface<H>> {
        self.surfaces.iter_mut().find(|s| s.entry.id == id)
    }

    pub fn entry(&self, id: EntryId) -> Option<&Entry<H>> {
        self.surfaces
            .iter()
            .map(|s| &s.entry)
            .chain(self.retired.iter())
            .find(|e| e.id == id)
    }

    pub fn entry_mut(&mut self, id: EntryId) -> Option<&mut Entry<H>> {
        self.surfaces
            .iter_mut()
            .map(|s| &mut s.entry)
            .chain(self.retired.iter_mut())
            .find(|e| e.id == id)
    }

    /// One frame: EVERY surface's motion steps — Closing ones unconditionally, whatever the host
    /// fold says — and an Opening surface whose spring settled becomes Open.
    pub fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) {
        for s in &mut self.surfaces {
            if s.phase == Phase::Hidden {
                continue;
            }
            s.motion.tick(t, present);
            if s.phase == Phase::Opening && s.motion.settled() {
                s.phase = Phase::Open;
            }
        }
    }

    /// The ONLY place Closing clears: a Closing surface whose fade settled leaves for good.
    pub fn prune(&mut self) -> Vec<Life<H>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.surfaces.len() {
            let s = &self.surfaces[i];
            if s.phase == Phase::Closing && s.motion.settled() {
                let s = self.surfaces.remove(i);
                out.push(Life::Ev(s.entry.id, ScreenEvent::WillLeave(Leave::ForGood)));
                out.push(Life::Unmount(s.entry.id));
                self.retired.push(s.entry);
            } else {
                i += 1;
            }
        }
        out
    }

    /// Drop retired entries whose `Unmount` was delivered.
    pub fn drop_unmounted(&mut self, unmounted: &[super::super::machine::InstanceId]) {
        self.retired
            .retain(|e| !e.inst.as_ref().map_or(true, |i| unmounted.contains(&i.id)));
    }

    /// The fold (§6.2): bottom-to-top, update Frozen if any surface freezes, render the most
    /// severe any surface asks.
    pub fn host_policy(&self) -> (HostUpdate, HostRender) {
        self.surfaces.iter().fold((HostUpdate::Live, HostRender::Live), |(u, r), s| {
            let (su, sr) = surface_policy(s.style, s.phase, s.ground_ready);
            (
                if su == HostUpdate::Frozen || u == HostUpdate::Frozen {
                    HostUpdate::Frozen
                } else {
                    HostUpdate::Live
                },
                r.max(sr),
            )
        })
    }

    /// A miss (a click beside every stop) against the top surface: consulted only while `Open`.
    pub fn on_miss(&self) -> Option<(EntryId, OnMiss)> {
        let s = self.surfaces.iter().rev().find(|s| s.phase != Phase::Hidden)?;
        if s.phase != Phase::Open {
            return Some((s.entry.id, OnMiss::Swallow));
        }
        Some((s.entry.id, on_miss(s.style)))
    }

    pub fn instance_mut(&mut self, id: super::super::machine::InstanceId) -> Option<&mut Instance<H>> {
        self.surfaces
            .iter_mut()
            .map(|s| &mut s.entry)
            .chain(self.retired.iter_mut())
            .filter_map(|e| e.inst.as_mut())
            .find(|i| i.id == id)
    }
}
