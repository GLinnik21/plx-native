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
            let changed = self.appear != self.target || self.vel != 0.0;
            self.appear = self.target;
            self.vel = 0.0;
            // The generic spring's visual epsilon can stop requesting presents before this
            // exact endpoint. The surface must paint the snap, including its last closing frame.
            if changed {
                present.note(super::super::present::PresentEvent::Motion);
            }
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

/// **Does this STYLE hold a snapshot of its host?** — the one answer `popover`'s process-wide
/// `HOST_USERS` counter is driven from (`app/bridge.rs`'s `sync_host`).
///
/// DERIVED from [`surface_policy`] rather than restating it as a second `matches!` over `Style`,
/// and that is the whole point of the function existing. The counter is what arms
/// `popover::host::page_pass`'s freeze, so a style the table calls `HostRender::Cached` and this
/// answer calls `false` produces a surface whose host is redrawn in full on every frame it is up
/// — with nothing failing, because the two statements live three modules apart and nothing
/// compares them.
///
/// **That is exactly what happened to `Style::Compact` (restructure phase 8).** The bridge listed
/// `Sheet | Opaque { snapshot: true } | Alert` by hand and left Compact out — the one production
/// Compact surface being the Library's Sort/Filter menu, whose legacy `Popover` had been
/// `caching_host()` since 2026-09-03, and whose `Style::Compact` doc names `item_menu` (also
/// `caching_host()`) as its exemplar. Measured on the television, `fps:library-switch`: sustained
/// 58 ms frames with the whole library page — ambient wash, shelves, poster grid and all their
/// text — re-rendered under an open menu, `loop=` 43-45 against a floor of 45.
///
/// The phase is deliberately NOT a parameter. `HOST_USERS` is an ownership count held from the
/// surface's first live frame to its release; a per-frame answer would flap as
/// `Opaque { snapshot: true }` crosses into `Replaced` and the bridge would owe a release it
/// never took. `Phase::Opening` with no ground drawn is the phase at which every style states
/// its host policy in full.
pub fn style_caches_host(style: Style) -> bool {
    surface_policy(style, Phase::Opening, false).1 != HostRender::Live
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

    /// **The INSTANT twin of [`dismiss`](Self::dismiss): `Closing` with the motion JUMPED to 0**,
    /// so the next `prune` — the same frame's, since a spring at its target is settled by
    /// definition — retires the surface with nothing ever composited of it again.
    ///
    /// `dismiss` runs the appear spring back down, which is right for a person closing a panel
    /// over a screen that stays. It is wrong for the one case that has no screen left to fade
    /// over: Privacy & data → **Delete all local data**, confirmed. That sweep signs the account
    /// out, so the page the surface was presented on is replaced by the sign-in screen in the
    /// same frame — and a dismissal fade over it composites a stale snapshot of a page that no
    /// longer exists across the incoming one. The legacy code reached for `ui::settings::hide()`
    /// here for exactly this reason ("the screen under it is going — no fade to run over"), and
    /// that is the behaviour this restores.
    ///
    /// Unlike `dismiss` it does NOT refuse a surface already `Closing`: the caller's whole claim
    /// is that there is no longer a host to fade over, which is as true of a fade already running
    /// as of one about to start, so an in-flight dismissal is cut short rather than left to
    /// finish. The RETURN value still means what `dismiss`'s does — "this call is the one that
    /// took the surface out of the running" — so a caller that emits the host's `Uncover` off it
    /// does not emit a second one for a surface whose `dismiss` already did.
    ///
    /// It is the STACK's method, not `Navigation`'s, and so — unlike
    /// `Navigation::request(NavOp::Dismiss)` — it emits no `Uncover` on the host. That is not an
    /// omission to be tidied up in general: the only caller is the one whose host is being
    /// replaced in the same frame, and telling a page it has been uncovered immediately before
    /// unmounting it is a lie the page might act on. A future caller that hides a surface over a
    /// host that STAYS wants the `Navigation` path with the uncover bookkeeping, not this one.
    pub fn hide(&mut self, id: EntryId) -> bool {
        let Some(s) = self.surfaces.iter_mut().find(|s| s.entry.id == id) else {
            return false;
        };
        let was_running = s.phase != Phase::Closing;
        s.phase = Phase::Closing;
        // The jump, not `motion.to(0.0)`: `at` sets appear, velocity AND target together, which
        // is what makes `settled()` true immediately. Leaving the velocity behind would keep the
        // spring unsettled for a frame or two and put the surface back on screen after the host
        // had gone — the exact artefact this method exists to prevent.
        s.motion = PopoverMotion::at(0.0);
        was_running
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
        for surface in &mut self.surfaces {
            if surface.entry.evicted && surface.entry.inst.as_ref().is_some_and(|i| unmounted.contains(&i.id)) {
                surface.entry.inst = None;
            }
        }
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

/// `dismiss` vs [`ModalStack::hide`] — the fade and the CUT, which are the same phase change and
/// two entirely different frames on the panel.
#[cfg(test)]
mod hide_tests {
    use super::*;
    use crate::ui::fixture::{tick, FixtureArg, FixtureHost};
    use crate::ui::present::Present;

    /// A surface presented and then stepped until its appear spring has settled: `Open`, with the
    /// motion at 1. Everything below starts here, because a JUST-presented surface is at 0 and
    /// `dismiss` would look instant for the wrong reason.
    fn opened() -> (ModalStack<FixtureHost>, EntryId) {
        let mut ms: ModalStack<FixtureHost> = ModalStack::new();
        let mut ids = Minter::default();
        let (id, _) = ms.present(&mut ids, FixtureArg::Modal, Style::Sheet);
        let mut present = Present::new();
        for i in 0..200u32 {
            let mut ph = PresentHandle::of(&mut present);
            ms.tick(tick(16 + i * 16), &mut ph);
            if ms.surface(id).unwrap().phase == Phase::Open {
                break;
            }
        }
        assert_eq!(ms.surface(id).unwrap().phase, Phase::Open, "the appear spring settled");
        (ms, id)
    }

    fn step(ms: &mut ModalStack<FixtureHost>, from: u32, n: u32) {
        let mut present = Present::new();
        for i in 0..n {
            let mut ph = PresentHandle::of(&mut present);
            ms.tick(tick(from + i * 16), &mut ph);
        }
    }

    /// `hide` retires on the same frame — no spring runs at all — while `dismiss` over the same
    /// surface is still on screen many frames later.
    #[test]
    fn hide_retires_without_a_spring_while_dismiss_still_runs_one() {
        // the cut
        let (mut ms, id) = opened();
        assert!(ms.hide(id), "the surface was up, so this call took it out of the running");
        let s = ms.surface(id).expect("still present until prune");
        assert_eq!(s.phase, Phase::Closing);
        assert_eq!(s.motion.appear, 0.0, "JUMPED, not sprung");
        assert!(s.motion.settled());
        let life = ms.prune();
        assert_eq!(life.len(), 2, "WillLeave then Unmount, on the very first prune");
        assert!(ms.is_empty(), "nothing left to composite over the incoming screen");

        // the fade, for contrast: the same surface, the same first prune, and it is STILL up
        let (mut ms, id) = opened();
        assert!(ms.dismiss(id));
        assert_eq!(ms.surface(id).unwrap().phase, Phase::Closing);
        assert!(ms.surface(id).unwrap().motion.appear > 0.5, "the fade has barely begun");
        assert!(ms.prune().is_empty(), "and prune leaves it alone");
        step(&mut ms, 4096, 4);
        assert!(
            ms.prune().is_empty(),
            "four frames in, a dismissal is still fading and prune still leaves it alone"
        );
        assert!(!ms.is_empty(), "…so it is still on screen");
    }

    /// A dismissal already in flight is CUT SHORT rather than refused — the caller's claim is that
    /// there is no host left to fade over, which a fade already running does not change. The
    /// return value still reports that this call was not the one that closed it, so a caller
    /// emitting the host's `Uncover` off it does not emit a second one.
    #[test]
    fn hide_cuts_a_dismissal_already_in_flight_short() {
        let (mut ms, id) = opened();
        assert!(ms.dismiss(id));
        step(&mut ms, 4096, 3);
        assert!(ms.surface(id).unwrap().motion.appear > 0.0, "mid-fade");
        assert!(!ms.hide(id), "it was already closing: not this call's doing");
        assert_eq!(ms.surface(id).unwrap().motion.appear, 0.0, "…but it is gone NOW");
        assert_eq!(ms.prune().len(), 2);
        assert!(ms.is_empty());
    }

    /// An id that names no surface is a no-op, exactly as `dismiss`'s is.
    #[test]
    fn hide_of_an_unknown_id_changes_nothing() {
        let (mut ms, id) = opened();
        assert!(!ms.hide(EntryId(id.0 + 99)));
        assert_eq!(ms.surface(id).unwrap().phase, Phase::Open);
        assert!(ms.prune().is_empty());
    }
}
