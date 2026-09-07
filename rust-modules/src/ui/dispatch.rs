//! The frame — ONE algorithm (spec §3.3), generic over the `Host` bundle and a `Rig` that owns
//! whatever the dispatcher does not: the stores and the other addressable machines, the mounter,
//! the views, the measure, the adapters, and the three privileged OS calls (`ls2_pump`,
//! `opaque_route`, `clear_opaque_region`) which `app/run.rs` implements and this module only
//! names as hooks.
//!
//! The ten steps over one frame, with the step budgets, the parked structural ops, the carry-over
//! that never drops, the single NAV COMMIT with its reserved post-commit budget, the present
//! decision and the prepare/draw pair — over the container tree (`ui::containers::Navigation`,
//! phase 3b: tabs → one shared stack → the shared modal stack, minting every `EntryId` and
//! `InstanceId`). A structural op is REQUESTED at commit and applies at its transition's commit
//! point — a cut now, a dip at its floor — and comes back as `Life` steps this module executes:
//! mount through the one `Mounter`, deliver the §3.4 sequence in the post-commit drain, retire
//! bodies. The activity table (§8.3) is read off the modal stack's host fold: a Frozen host
//! receives no `Tick`, a Cached or Replaced host does not prepare, a Replaced host does not draw.
#![allow(dead_code)] // phase 3b: the product runs one LegacyPage through this; screens from 5b

use std::collections::VecDeque;

use super::containers::modal::{HostRender, HostUpdate};
use super::containers::stack::Instance;
use super::containers::transition::{Immediate, Transition};
use super::containers::{Life, Navigation};
use super::focus::Outcome;
use super::frame::{Budget, RenderSet};
use super::geom::IndexElem;
use super::hit::PointerKind;
use super::input::{InputMachine, PressEvent};
use super::machine::{
    Addr, Cx, Delivery, Edge, Effects, EntryId, FocusKey, FocusRead, Fx, GroupId, Handled, Host,
    InputEvent, InputKind, InputOwner, InstanceId, Key, MachineId, Measure, NavOp, PressArm,
    PressFrom, PresentHandle, PressRead, RequestId, Stamped, StoreOrd, SystemInput, Tick, TimerId,
};
use super::present::Present;
use super::screen::{
    Activate, At, Dir, DrawFrame, EdgeRule, ElemKind, Enter, Focusable, FocusSource, FocusTarget,
    GroupSpec, HitSource, Mounter, Placed, ReturnState, Screen, ScreenEvent, Step, Stop,
};
use super::{Painter, Rect};

/// The strip's pills as element indices: `of_index(STRIP_BASE + i)` — a reserved band above any
/// page's own indices, so a pill and a tile never share a key.
pub const STRIP_BASE: u32 = 0xFFFF_0000;

/// The input owner's page COMPOSED with the container's strip (§6.2): the strip is a `Row`
/// group above the page's own, contributed only while the page allows it.
struct PageWithStrip<'a, H: Host> {
    page: &'a dyn Screen<H>,
    strip: Option<(GroupSpec, &'a [Rect])>,
    entry: EntryId,
}

impl<H: Host> Focusable<H> for PageWithStrip<'_, H>
where
    H::Elem: IndexElem,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if let Some((spec, _)) = &self.strip {
            out.push(*spec);
        }
        self.page.groups(cx, out);
    }
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        match key.index() {
            Some(i) if i >= STRIP_BASE => self.strip.as_ref().map(|(s, _)| s.id),
            _ => self.page.group_of(key, cx),
        }
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        match (k.elem.index(), &self.strip) {
            (Some(i), Some((_, rects))) if i >= STRIP_BASE => {
                let i = (i - STRIP_BASE) as usize;
                let mv = |j: usize| Step::Move(FocusKey {
                    entry: self.entry,
                    elem: H::Elem::of_index(STRIP_BASE + j as u32),
                });
                match dir {
                    Dir::Left if i > 0 => mv(i - 1),
                    Dir::Right if i + 1 < rects.len() => mv(i + 1),
                    _ => Step::Edge,
                }
            }
            _ => self.page.neighbour(k, dir, cx),
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        match (key.index(), &self.strip) {
            (Some(i), Some((_, rects))) if i >= STRIP_BASE => {
                let r = *rects.get((i - STRIP_BASE) as usize)?;
                Some(Placed {
                    rect: r,
                    rest_rect: r,
                    clip: Rect::FULL,
                    index: Some(i - STRIP_BASE),
                })
            }
            _ => self.page.place(key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        match (want.elem.index(), &self.strip) {
            (Some(i), Some((_, rects))) if i >= STRIP_BASE => {
                let j = ((i - STRIP_BASE) as usize).min(rects.len().saturating_sub(1));
                FocusKey {
                    entry: self.entry,
                    elem: H::Elem::of_index(STRIP_BASE + j as u32),
                }
            }
            _ => self.page.reconcile(want, cx),
        }
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        match &self.strip {
            Some((spec, rects)) if spec.id == g => {
                let cx_ = from.rect.cx();
                let mut best = (f32::MAX, 0usize);
                for (i, r) in rects.iter().enumerate() {
                    let d = (r.cx() - cx_).abs();
                    if d < best.0 {
                        best = (d, i);
                    }
                }
                FocusKey {
                    entry: self.entry,
                    elem: H::Elem::of_index(STRIP_BASE + best.1 as u32),
                }
            }
            _ => self.page.seat(g, from, cx),
        }
    }
}

/// Step invocations the pre-commit drain may spend per frame (§3.3 step 6).
pub const MAX_STEPS_PRE: u32 = 192;
/// Reserved for the post-commit drain (§3.3 step 7); never consumable by step 6.
pub const MAX_STEPS_POST: u32 = 64;
/// The coarse round counter, diagnostics only.
pub const MAX_ROUNDS: u32 = 4;
/// A debug build asserts when `carried` grows for this many consecutive frames.
pub const CARRY_GROWTH_FRAMES: u8 = 8;

/// The recorder's taps (spec §5.3): the dispatcher reports what it did, the recorder writes it.
/// Every method has a no-op default so an unarmed frame costs a vtable call per event and nothing
/// else. `NoTap` is the unarmed implementation.
pub trait Tap<H: Host> {
    fn tick(&mut self, _f: u64, _t: Tick) {}
    fn input(&mut self, _f: u64, _ev: &InputEvent<H::Elem>) {}
    fn result(&mut self, _f: u64, _addr: &Addr, _msg: &H::Msg) {}
    fn effect(&mut self, _f: u64, _s: &Stamped<H>) {}
    fn present(&mut self, _f: u64, _bit: bool, _why: Option<super::present::Provenance>) {}
    /// After the frame's drains, on a frame that had events: the logical-state hash.
    fn state(&mut self, _f: u64, _hash: u64) {}
    /// After the drains: the engine's resolved focus for the input owner, as
    /// `(entry, index, group)`.
    fn focus(&mut self, _f: u64, _focus: Option<(u32, u32, Option<u32>)>) {}
    fn frame_done(&mut self, _f: u64) {}
}

pub struct NoTap;
impl<H: Host> Tap<H> for NoTap {}

/// The adapter drain order (spec §3.3 step 3): one rank per PUMP in the legacy loop's order — a
/// store with two pumps has two ranks, so the list is a permutation of the loop and nothing is
/// named twice. Results are delivered by `(completion_frame, rank, arrival_index)`.
pub const ADAPTER_RANKS: [&str; 11] = [
    "auth",
    "pms",
    "browse",
    "search",
    "metadata/season",
    "person",
    "play",
    "metadata/detail",
    "viewstate",
    "alt_sources",
    "poster",
];

/// What the dispatcher borrows from the application for one frame. `split` hands out the
/// mounter, the views and the measure at once so a mount can read the stores it was built over.
pub trait Rig<H: Host> {
    fn split(&mut self) -> Split<'_, H>;
    /// Step a non-instance machine (a store, `Session`, `Player`, `Cache`) with the app's message.
    /// The rig builds the `Cx` itself, because a store's views are every OTHER store's.
    fn deliver(
        &mut self,
        to: MachineId,
        msg: &H::Msg,
        parts: &CxParts<H::Elem>,
        fx: &mut Effects<'_, H>,
    ) -> Handled;
    /// A timer for a non-instance machine.
    fn timer(&mut self, owner: MachineId, id: TimerId, parts: &CxParts<H::Elem>, fx: &mut Effects<'_, H>);
    /// Execute an application effect against the adapters; may emit more (an adapter result
    /// available at once, an `Emit`).
    fn app_fx(&mut self, from: MachineId, fx: H::Fx, parts: &CxParts<H::Elem>, out: &mut Effects<'_, H>);
    /// Write one log line (machines never log directly).
    fn log(&mut self, line: &str);
    /// The application's prepare work outside any screen (the `TexCache` upload step, §10).
    fn prepare(&mut self, b: &mut Budget, present: &mut Present);
    /// Privileged call 1 (§3.3 step 1).
    fn ls2_pump(&mut self);
    /// Privileged call 2 (§3.3 step 9): every frame, presented or not.
    fn opaque_route(&mut self, video_plane_bound: bool);
    /// Privileged call 3 (§3.3 step 10): at draw entry.
    fn clear_opaque_region(&mut self);
    /// The clock for `Budget` (one reading per take).
    fn now_us(&self) -> u64;
    /// BACK at the root of the root stack (§3.4): the application decides — the platform's Home
    /// on the television (`root_back_tests`); nothing, by default, in a fixture.
    fn back_at_root(&mut self) {}
}

pub struct Split<'a, H: Host> {
    pub mounter: &'a mut dyn Mounter<H>,
    pub views: H::Views<'a>,
    pub measure: &'a dyn Measure,
}

/// The pieces of a `Cx` that are the dispatcher's to know; the rig adds the views.
#[derive(Clone, Copy)]
pub struct CxParts<K> {
    pub tick: Tick,
    pub press: PressRead,
    pub focus: FocusRead<K>,
    pub owner: InputOwner,
}

impl<K: Copy> CxParts<K> {
    pub fn cx<'a, H: Host<Elem = K>>(&self, views: H::Views<'a>, measure: &'a dyn Measure) -> Cx<'a, H> {
        Cx {
            views,
            tick: self.tick,
            measure,
            press: self.press,
            focus: self.focus,
            owner: self.owner,
        }
    }
}

/// What one frame reports back (the instruments' half, §8.4, minimal).
#[derive(Debug, Default, Clone)]
pub struct FrameReport {
    pub presented: bool,
    pub steps_pre: u32,
    pub steps_post: u32,
    pub carried: usize,
    pub queue_hwm: usize,
    pub mounted: Vec<InstanceId>,
    pub unmounted: Vec<InstanceId>,
    pub dropped_deliveries: u32,
    /// The logical-state hash, on an event frame.
    pub state_hash: Option<u64>,
    /// The host fold this frame (§8.3): what the page beneath the surfaces did.
    pub host_update: Option<HostUpdate>,
    pub host_render: Option<HostRender>,
    /// Which bodies received `Tick` this frame, in delivery order.
    pub ticked: Vec<InstanceId>,
    /// The render set the frame composed (§8.3), for the residency check.
    pub render_set: RenderSet,
    /// The PAGE reported motion this frame (a surface's foreground springs do not count): what
    /// a Cached host's snapshot refresh reads.
    pub underlay_moving: bool,
    /// A BACK reached the root of the root stack this frame.
    pub back_at_root: bool,
}

/// The dispatcher: the queue that is never dropped, the parked structural ops, the timers, the
/// present gate, the budget, the tree.
pub struct Dispatcher<H: Host> {
    queue: VecDeque<Stamped<H>>,
    parked: Vec<Stamped<H>>,
    /// Lifecycle steps queued from outside a step (suspend/resume/profile reset), applied at
    /// the next commit ahead of the tree's own.
    parked_life: Vec<Life<H>>,
    timers: Vec<(TimerId, u32, MachineId)>,
    pub present: Present,
    pub budget: Budget,
    pub nav: Navigation<H>,
    /// The Input machine (§2.2): the engine, the hit map, the press and its arm.
    pub input: InputMachine<H::Elem>,
    /// A replay in `--targets` mode: every page resolves as a legacy one (the engine and the
    /// map are bypassed; focus comes from the recording).
    focus_override: Option<FocusSource>,
    frame: u64,
    carried_streak: u8,
    last_carried: usize,
    dropped_deliveries: u32,
    render_breach_logged: bool,
    /// The input owner answered `Handled::No` to a BACK: resolve it over its stack at commit.
    pending_back: bool,
}

impl<H: Host> Default for Dispatcher<H>
where
    H::Elem: IndexElem,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Host> Dispatcher<H>
where
    H::Elem: IndexElem,
{
    /// A dispatcher whose shared stack CUTS (the fixture host's shape).
    pub fn new() -> Self {
        Self::with_transition(Box::new(Immediate))
    }

    /// A dispatcher whose shared stack rides `transition` (the product's `PageDip`).
    pub fn with_transition(transition: Box<dyn Transition>) -> Self {
        Self {
            queue: VecDeque::new(),
            parked: Vec::new(),
            parked_life: Vec::new(),
            timers: Vec::new(),
            present: Present::new(),
            budget: Budget::new(),
            nav: Navigation::new(transition),
            input: InputMachine::new(),
            focus_override: None,
            frame: 0,
            carried_streak: 0,
            last_carried: 0,
            dropped_deliveries: 0,
            render_breach_logged: false,
            pending_back: false,
        }
    }

    /// The hit map of the last PRESENTED frame (§7.6): what a click on an idle frame resolves against.
    pub fn last_stops(&self) -> &[Stop<H::Elem>] {
        self.input.hit.front()
    }

    /// The input owner this frame: the television keyboard when it is up, else the tree's.
    fn owner(&self) -> InputOwner {
        if self.input.keyboard {
            InputOwner::System(SystemInput::Keyboard)
        } else {
            self.nav.input_owner().unwrap_or(InputOwner::Entry(EntryId(0)))
        }
    }

    /// The entry whose page answers focus and hits: the tree's owner (the keyboard consumes
    /// text, the page beneath it still owns the groups).
    fn owner_entry(&self) -> Option<EntryId> {
        match self.nav.input_owner() {
            Some(InputOwner::Entry(e)) => Some(e),
            _ => None,
        }
    }

    /// Does the owner's page resolve through the engine and the map (§7.6 `FocusSource`)?
    fn engine_page(&self) -> bool {
        if self.focus_override == Some(FocusSource::Legacy) {
            return false;
        }
        self.owner_entry()
            .and_then(|e| self.nav.entry(e))
            .and_then(|e| e.inst.as_ref())
            .map_or(false, |i| i.screen.focus_source() == FocusSource::Engine)
    }

    fn hit_page(&self) -> bool {
        if self.focus_override == Some(FocusSource::Legacy) {
            return false;
        }
        self.owner_entry()
            .and_then(|e| self.nav.entry(e))
            .and_then(|e| e.inst.as_ref())
            .map_or(false, |i| i.screen.hit_source() == HitSource::Engine)
    }

    /// A replay's mode (§5.5): `Some(Legacy)` bypasses the engine and the map for every page.
    pub fn set_focus_source_override(&mut self, o: Option<FocusSource>) {
        self.focus_override = o;
    }

    /// `Event::Store{id, gen}` (§3.4): a notice to every live instance, then the engine's
    /// `reconcile` before draw.
    pub fn store_changed(&mut self, ord: StoreOrd, gen: u32) {
        let bodies: Vec<InstanceId> = self.nav.bodies().filter_map(|e| e.inst.as_ref()).map(|i| i.id).collect();
        for id in bodies {
            self.queue.push_back(Stamped {
                from: MachineId::Nav,
                fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::StoreChanged(ord, gen))),
            });
        }
    }

    /// Queue an effect from OUTSIDE a step (the application's loop handing a store command to the
    /// dispatcher path): it drains in this frame's step 6 like any machine's emission.
    pub fn emit(&mut self, from: MachineId, fx: Fx<H>) {
        self.queue.push_back(Stamped { from, fx });
    }

    /// Queue a structural op from OUTSIDE a step (boot's `Root`, a lifecycle `Suspend`): it is
    /// parked like any other and applies at this frame's NAV COMMIT.
    pub fn request(&mut self, from: MachineId, op: NavOp<H::Arg>) {
        self.parked.push(Stamped {
            from,
            fx: Fx::Nav(op),
        });
    }

    /// `Lifecycle(0x103/0x104)`: `Suspend` down the tree at the next commit.
    pub fn suspend(&mut self) {
        let life = self.nav.suspend();
        self.parked_life.extend(life);
    }

    /// `Lifecycle(0x105/0x106)`.
    pub fn resume(&mut self) {
        let life = self.nav.resume();
        self.parked_life.extend(life);
    }

    /// `NavEvent::ResetForProfile`: every entry dropped at the next commit.
    pub fn reset_for_profile(&mut self) {
        let life = self.nav.reset_for_profile();
        self.parked_life.extend(life);
        self.input = InputMachine::new();
    }

    /// The top page's instance id, if mounted.
    pub fn top_page(&self) -> Option<InstanceId> {
        self.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| i.id)
    }

    /// The top page's screen, for a test to read.
    pub fn top_screen(&self) -> Option<&dyn super::screen::Screen<H>> {
        self.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| &*i.screen)
    }

    /// The engine's current focus for the input owner (§7.3 step 5).
    pub fn focus(&self) -> Option<FocusKey<H::Elem>> {
        self.owner_entry()
            .and_then(|e| self.input.engine.current(InputOwner::Entry(e)))
    }

    /// Feed the owner's focus from outside a step: a test's premise (no group is remembered).
    pub fn set_focus(&mut self, k: Option<FocusKey<H::Elem>>) {
        self.set_focus_in(k, None);
    }

    /// A replay's recorded resolution: the key AND the group it was seated in, so the
    /// remembered cursor is restored as the live run set it.
    pub fn set_focus_in(&mut self, k: Option<FocusKey<H::Elem>>, group: Option<GroupId>) {
        let Some(e) = self.owner_entry() else {
            return;
        };
        let owner = InputOwner::Entry(e);
        match k {
            None => self.input.engine.clear(owner),
            Some(k) => {
                self.input.engine.set(owner, k, group, super::screen::By::Restore);
            }
        }
    }

    /// The owner's focus as a recording spells it: `(entry, index, group)`.
    pub fn focus_record(&self) -> Option<(u32, u32, Option<u32>)> {
        let e = self.owner_entry()?;
        let owner = InputOwner::Entry(e);
        let k = self.input.engine.current(owner)?;
        Some((
            k.entry.0,
            k.elem.index().unwrap_or(u32::MAX),
            self.input.engine.current_group(owner).map(|g| g.0),
        ))
    }

    fn ret(&self) -> ReturnState<H::Elem> {
        ReturnState {
            focus: self.focus(),
            scroll: 0.0,
        }
    }

    fn parts(&self, tick: Tick) -> CxParts<H::Elem> {
        let owner = self.owner();
        CxParts {
            tick,
            press: PressRead {
                scale: self.input.press.scale(),
                is_long: self.input.press.was_long(),
            },
            focus: FocusRead { current: self.focus() },
            owner,
        }
    }

    /// The owner's page composed with the strip (§6.2), for one engine query.
    fn owner_view(nav: &Navigation<H>, entry: EntryId) -> Option<PageWithStrip<'_, H>> {
        let e = nav.entry(entry)?;
        let inst = e.inst.as_ref()?;
        let page: &dyn Screen<H> = &*inst.screen;
        let is_top_page = nav.top_page().map(|t| t.id) == Some(entry);
        let strip = (is_top_page && page.strip_reachable() && !nav.tabs.pill_rects.is_empty())
            .then(|| (nav.tabs.strip_group(), nav.tabs.pill_rects.as_slice()));
        Some(PageWithStrip { page, strip, entry })
    }

    /// ONE frame (§3.3): the ten steps in order.
    pub fn frame(
        &mut self,
        rig: &mut dyn Rig<H>,
        tick: Tick,
        inputs: Vec<InputEvent<H::Elem>>,
        results: Vec<(Addr, H::Msg)>,
        tap: &mut dyn Tap<H>,
    ) -> FrameReport {
        self.frame += 1;
        let f = self.frame;
        let mut report = FrameReport::default();
        let queued_before = self.queue.len();
        tap.tick(f, tick);
        let event_frame = !inputs.is_empty()
            || !results.is_empty()
            || !self.parked.is_empty()
            || !self.parked_life.is_empty();

        // 1. the first privileged call
        rig.ls2_pump();

        // 2. ingest: external events, delivered to the input owner, AHEAD of everything queued
        //    (external_events_are_drained_before_effect_results).
        let mut head: Vec<Stamped<H>> = Vec::new();
        for mut ev in inputs {
            tap.input(f, &ev);
            // the Input machine's own half first (§3.4): keyboard ownership, the press edges,
            // dpad mode, the pointer resolved against the hit map
            let pointer = match ev.kind {
                InputKind::Pointer { x, y, .. } => Some((PointerKind::Move, x, y)),
                InputKind::Click { x, y, .. } => Some((PointerKind::Click, x, y)),
                InputKind::Drag { x, y, .. } => Some((PointerKind::Drag, x, y)),
                _ => None,
            };
            if let Some((kind, x, y)) = pointer {
                {
                    if self.hit_page() {
                        let owner_e = self.owner_entry();
                        let focused = self.focus();
                        let res = self.input.hit.resolve(kind, x, y, focused);
                        match &mut ev.kind {
                            InputKind::Pointer { hit, .. } | InputKind::Click { hit, .. } | InputKind::Drag { hit, .. } => {
                                *hit = res.hit.map(|k| k.elem);
                            }
                            _ => {}
                        }
                        // a pointer press whose hit leaves its arm is cancelled (§7.4)
                        if let Some(arm) = self.input.arm {
                            if arm.from == PressFrom::Pointer && res.hit != Some(arm.key) {
                                self.input.cancel_press();
                            }
                        }
                        if let (Some(e), Some(k)) = (owner_e, res.focus) {
                            let owner = InputOwner::Entry(e);
                            if let Outcome::Moved { from, to, by } =
                                self.input.engine.set(owner, k, None, super::screen::By::Pointer)
                            {
                                if let Some(inst) = self.nav.instance_of(e) {
                                    head.push(Stamped {
                                        from: MachineId::Input,
                                        fx: Fx::Deliver(
                                            MachineId::Instance(inst),
                                            Delivery::Screen(ScreenEvent::FocusMoved { from, to, by }),
                                        ),
                                    });
                                }
                            }
                        }
                        if let (Some(e), Some((k, act))) = (owner_e, res.activate) {
                            if let Some(inst) = self.nav.instance_of(e) {
                                match act {
                                    Activate::Press => {
                                        self.input.arm(
                                            PressArm {
                                                key: k,
                                                from: PressFrom::Pointer,
                                                holdable: true,
                                            },
                                            MachineId::Instance(inst),
                                            tick.ms,
                                        );
                                    }
                                    Activate::Immediate | Activate::Direct => head.push(Stamped {
                                        from: MachineId::Input,
                                        fx: Fx::Deliver(
                                            MachineId::Instance(inst),
                                            Delivery::Screen(ScreenEvent::Activate(k.elem)),
                                        ),
                                    }),
                                }
                            }
                        }
                        if res.miss {
                            if let Some((id, super::containers::modal::OnMiss::Dismiss)) = self.nav.modals.on_miss() {
                                self.parked.push(Stamped {
                                    from: MachineId::Input,
                                    fx: Fx::Nav(NavOp::Dismiss(id)),
                                });
                            }
                        }
                    }
                }
            }
            match ev.kind {
                InputKind::SystemKeyboard(up) => self.input.keyboard = up,
                InputKind::Key { key, edge, .. } => {
                    if matches!(key, Key::Up | Key::Down | Key::Left | Key::Right) && edge == Edge::Down {
                        self.input.hit.note_dpad();
                    }
                    if key == Key::Ok {
                        match edge {
                            Edge::Up => self.input.release(tick.ms),
                            Edge::Repeat => self.input.note_alive(tick.ms),
                            Edge::Down => {}
                        }
                    }
                }
                _ => {}
            }
            if let Some(eid) = self.owner_entry() {
                if let Some(inst) = self.nav.instance_of(eid) {
                    head.push(Stamped {
                        from: MachineId::Input,
                        fx: Fx::Deliver(
                            MachineId::Instance(inst),
                            Delivery::Screen(ScreenEvent::Input(ev)),
                        ),
                    });
                }
            }
        }
        // 3. adapter results, in the caller's composite-key order (the recorder's order)
        for (addr, msg) in results {
            if !self.nav.is_deliverable(&addr) {
                self.dropped_deliveries += 1;
                report.dropped_deliveries += 1;
                continue;
            }
            tap.result(f, &addr, &msg);
            let delivery = match addr.to {
                MachineId::Instance(_) => Delivery::Screen(ScreenEvent::Async(addr.req, msg)),
                _ => Delivery::Machine(msg),
            };
            head.push(Stamped {
                from: MachineId::Nav,
                fx: Fx::Deliver(addr.to, delivery),
            });
        }
        // 4. one Tick: containers before pages. The transition and every surface's motion step
        //    first (a Closing surface unconditionally); then every ACTIVE body per §8.3 — a page
        //    under a Frozen fold receives nothing.
        {
            let Dispatcher { nav, present, .. } = self;
            let mut ph = PresentHandle::of(present);
            nav.tabs.stack.tick(tick, &mut ph);
            present.set_scope(super::present::Scope::Surface);
            let mut ph = PresentHandle::of(present);
            nav.modals.tick(tick, &mut ph);
            present.set_scope(super::present::Scope::Page);
        }
        let (host_update, host_render) = self.nav.modals.host_policy();
        report.host_update = Some(host_update);
        report.host_render = Some(host_render);
        let page_bodies: Vec<InstanceId> = self
            .nav
            .tabs
            .stack
            .entries
            .iter()
            .filter_map(|e| e.inst.as_ref())
            .map(|i| i.id)
            .collect();
        let surface_bodies: Vec<InstanceId> = self
            .nav
            .modals
            .surfaces
            .iter()
            .filter_map(|s| s.entry.inst.as_ref())
            .map(|i| i.id)
            .collect();
        // the press machine (§7.4): hold and commit are delivered from the Tick
        for pe in self.input.tick(tick.ms, tick.dt()) {
            let (ev, owner) = match pe {
                PressEvent::Hold(id, owner) => (ScreenEvent::PressHold(id), owner),
                PressEvent::Commit(id, owner) => (ScreenEvent::PressCommit(id), owner),
            };
            head.push(Stamped {
                from: MachineId::Input,
                fx: Fx::Deliver(owner, Delivery::Screen(ev)),
            });
        }
        let page_active = host_update == HostUpdate::Live;
        for id in page_bodies.iter().copied().filter(|_| page_active).chain(surface_bodies) {
            report.ticked.push(id);
            head.push(Stamped {
                from: MachineId::Nav,
                fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::Tick(tick))),
            });
        }
        // 5. expired timers, deadline then id order
        let mut due: Vec<(TimerId, u32, MachineId)> = Vec::new();
        self.timers.retain(|t| {
            if tick.ms.wrapping_sub(t.1) < 0x8000_0000 {
                due.push(*t);
                false
            } else {
                true
            }
        });
        due.sort_by_key(|t| (t.1, t.0));
        for (id, _, owner) in due {
            head.push(Stamped {
                from: MachineId::Nav,
                fx: Fx::Deliver(owner, Delivery::Screen(ScreenEvent::Timer(id))),
            });
        }
        // Carried work from the previous frame runs AHEAD of this frame's new work (§3.3 step 7);
        // this frame's ingest goes behind it in the same FIFO.
        for s in head {
            self.queue.push_back(s);
        }

        // 6. the pre-commit drain
        let parts = self.parts(tick);
        report.steps_pre = self.drain(rig, &parts, MAX_STEPS_PRE, &mut report, tap);
        report.queue_hwm = report.queue_hwm.max(queued_before);

        // 7. NAV COMMIT — one per frame — then the post-commit drain on its own budget
        let owner_before = self.owner();
        self.commit(rig, &parts, &mut report);
        let parts = self.parts(tick); // the owner may have changed at commit
        if parts.owner != owner_before {
            // an owner change cancels the press (§7.4)
            self.input.cancel_press();
        }
        report.steps_post = self.drain(rig, &parts, MAX_STEPS_POST, &mut report, tap);
        // §7.3 step 6: after every landing and before draw, the owner's reconcile
        self.reconcile(rig, &parts, &mut report);
        tap.focus(f, self.focus_record());
        let timers_fired = report.steps_pre > 0 && event_frame;
        if event_frame || timers_fired {
            let h = self.state_hash();
            tap.state(f, h);
            report.state_hash = Some(h);
        }

        // carry accounting: nothing is ever dropped from this queue
        report.carried = self.queue.len();
        if report.carried > self.last_carried && report.carried > 0 {
            self.carried_streak = self.carried_streak.saturating_add(1);
        } else {
            self.carried_streak = 0;
        }
        self.last_carried = report.carried;
        debug_assert!(
            self.carried_streak < CARRY_GROWTH_FRAMES,
            "the effect queue grew for {CARRY_GROWTH_FRAMES} consecutive frames"
        );
        if self.carried_streak >= CARRY_GROWTH_FRAMES {
            rig.log(&format!(
                "dispatch: carried={} growing for {} frames",
                report.carried, self.carried_streak
            ));
        }

        // 8. the present decision, once (its WHY is read before the take clears it)
        let why = self.present.why();
        report.underlay_moving = self.present.page_moving();
        let will_present = self.present.take(tick.ms) || self.budget.has_queued_work();
        tap.present(f, will_present, why);
        // `prepare_does_not_change_the_logical_state_hash` (§5.4): a prepare pass touches render
        // resources only — graded on every presenting frame of a debug build
        #[cfg(debug_assertions)]
        let hash_before_prepare = self.state_hash();

        // 9. prepare (only if presenting), then opaque_route on EVERY frame. The activity table:
        //    the top page prepares unless its host fold is Cached or Replaced; every surface does.
        let (_, host_render) = self.nav.modals.host_policy();
        if will_present {
            self.budget.begin_frame(rig.now_us());
            let parts = self.parts(tick);
            {
                let Dispatcher { nav, budget, .. } = self;
                let Split { views, measure, .. } = rig.split();
                let cx = parts.cx::<H>(views, measure);
                if host_render == HostRender::Live {
                    if let Some(inst) = nav.tabs.stack.top_mut().and_then(|e| e.inst.as_mut()) {
                        inst.screen.prepare(budget, &cx);
                    }
                }
                for s in &mut nav.modals.surfaces {
                    if let Some(inst) = s.entry.inst.as_mut() {
                        inst.screen.prepare(budget, &cx);
                    }
                }
            }
            let Dispatcher {
                budget, present, ..
            } = self;
            rig.prepare(budget, present);
        }
        rig.opaque_route(self.present.video_plane());
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.state_hash(),
            hash_before_prepare,
            "a prepare pass changed the logical state"
        );

        // 10. draw, then the tail
        if will_present {
            rig.clear_opaque_region();
            let parts = self.parts(tick);
            let Dispatcher { nav, .. } = self;
            let Split { views, measure, .. } = rig.split();
            let cx = parts.cx::<H>(views, measure);
            let mut stops = Vec::new();
            let mut set = RenderSet::default();
            // the page pass: the top page (and, under a push, the level beneath it), unless the
            // host fold REPLACED it
            if host_render != HostRender::Replaced {
                let draws_below = nav.tabs.stack.transition.draws_below();
                let n = nav.tabs.stack.entries.len();
                let from = if draws_below { n.saturating_sub(2) } else { n.saturating_sub(1) };
                for e in nav.tabs.stack.entries[from..].iter_mut() {
                    if let Some(inst) = e.inst.as_mut() {
                        let mut f = DrawFrame::new(&cx, Painter::root());
                        f.page_alpha = nav.tabs.stack.transition.page_alpha();
                        inst.screen.draw(&mut f);
                        stops.extend(f.into_stops());
                        set.pages += 1;
                        set.bytes += inst.screen.render_bytes();
                    }
                }
            }
            if host_render == HostRender::Cached {
                set.frame_cache_bytes = super::frame::FRAME_CACHE_BYTES;
            }
            // the surfaces, bottom to top; a later stop is above an earlier one
            for s in &mut nav.modals.surfaces {
                if let Some(inst) = s.entry.inst.as_mut() {
                    let mut f = DrawFrame::new(&cx, Painter::root());
                    f.page_alpha = s.motion.appear;
                    inst.screen.draw(&mut f);
                    stops.extend(f.into_stops());
                    set.surfaces.push((s.entry.id, 1));
                    set.bytes += inst.screen.render_bytes();
                }
            }
            // the hit map swaps only on a presented frame (§7.6); a legacy page registers nothing
            let hit_page = self.hit_page();
            self.input.hit.fill(if hit_page { stops } else { Vec::new() });
            self.input.hit.swap();
            if let Err(breach) = set.check() {
                debug_assert!(false, "render set breach: {breach}");
                if !self.render_breach_logged {
                    self.render_breach_logged = true;
                    rig.log(&format!("dispatch: render set breach: {breach}"));
                }
            }
            report.render_set = set;
            if let Some(fault) = self.present.take_fault() {
                rig.log(&format!("dispatch: fault {fault:?}"));
            }
        }
        report.presented = will_present;
        tap.frame_done(f);
        report
    }

    /// The logical-state hash (spec §5.4): every live instance's `LogicalState`, the tree's
    /// shape and surface phases, the focus, the present gate's video-plane bit and the queue
    /// depth, in a fixed order. Incremental hashing (dirty flags per machine) is the optimisation
    /// the spec names; this is the definition it must equal.
    pub fn state_hash(&self) -> u64 {
        let mut c = super::machine::Canon::new();
        self.nav.write(&mut c);
        self.input.write_with(&mut c, &|k, c| {
            c.u32(k.index().unwrap_or(u32::MAX));
        });
        c.bool(self.present.video_plane());
        c.u32(self.queue.len() as u32);
        c.finish()
    }

    /// Pop-and-execute until the queue is empty or `max_steps` step invocations are spent;
    /// structural ops are parked for NAV COMMIT; what is left is CARRIED, never dropped.
    fn drain(
        &mut self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        max_steps: u32,
        report: &mut FrameReport,
        tap: &mut dyn Tap<H>,
    ) -> u32 {
        let mut steps = 0;
        while steps < max_steps {
            let Some(item) = self.queue.pop_front() else {
                break;
            };
            report.queue_hwm = report.queue_hwm.max(self.queue.len() + 1);
            tap.effect(self.frame, &item);
            match item.fx {
                Fx::Nav(_) | Fx::Mount(_) | Fx::Unmount(_) => self.parked.push(item),
                Fx::Deliver(to, delivery) => {
                    steps += 1;
                    let mut out: Vec<Stamped<H>> = Vec::new();
                    self.execute_deliver(rig, parts, to, delivery, &mut out, report);
                    self.absorb(out);
                }
                Fx::Timer { id, after_ms } => {
                    self.timers
                        .push((id, parts.tick.ms.wrapping_add(after_ms), item.from));
                }
                Fx::CancelTimer(id) => self.timers.retain(|t| t.0 != id),
                Fx::Press(arm) => {
                    steps += 1;
                    self.input.arm(arm, item.from, parts.tick.ms);
                }
                Fx::Log(line) => rig.log(&line.0),
                Fx::App(app_fx) => {
                    steps += 1;
                    let mut out: Vec<Stamped<H>> = Vec::new();
                    {
                        let Dispatcher { present, .. } = self;
                        let mut fx = Effects::new(&mut out, item.from, present);
                        rig.app_fx(item.from, app_fx, parts, &mut fx);
                    }
                    self.absorb(out);
                }
            }
        }
        steps
    }

    /// A step's emissions: the structural three are PARKED at once (so a key that opens a page
    /// mounts it this frame even when the drain's budget was spent before the FIFO reached its
    /// op — `a_key_that_opens_a_page_mounts_in_the_same_frame`); everything else joins the tail.
    fn absorb(&mut self, out: Vec<Stamped<H>>) {
        for s in out {
            match s.fx {
                Fx::Nav(_) | Fx::Mount(_) | Fx::Unmount(_) => self.parked.push(s),
                _ => self.queue.push_back(s),
            }
        }
    }

    fn execute_deliver(
        &mut self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        to: MachineId,
        delivery: Delivery<H>,
        out: &mut Vec<Stamped<H>>,
        report: &mut FrameReport,
    ) {
        match (to, delivery) {
            (MachineId::Instance(id), Delivery::Screen(ev)) => {
                let back = matches!(
                    ev,
                    ScreenEvent::Input(InputEvent {
                        kind: super::machine::InputKind::Key {
                            key: super::machine::Key::Back,
                            edge: super::machine::Edge::Down,
                            ..
                        },
                        ..
                    })
                );
                let Dispatcher { nav, present, .. } = self;
                // a surface's springs report under their own scope (§4.4 MotionScope)
                let is_surface = nav
                    .modals
                    .surfaces
                    .iter()
                    .any(|s| s.entry.inst.as_ref().map_or(false, |i| i.id == id));
                present.set_scope(if is_surface {
                    super::present::Scope::Surface
                } else {
                    super::present::Scope::Page
                });
                let Some(inst) = nav.instance_mut(id) else {
                    present.set_scope(super::present::Scope::Page);
                    report.dropped_deliveries += 1;
                    return;
                };
                let Split { views, measure, .. } = rig.split();
                let cx = parts.cx::<H>(views, measure);
                let mut fx = Effects::new(out, to, present);
                let handled = inst.screen.step(&ev, &cx, &mut fx);
                drop(fx);
                present.set_scope(super::present::Scope::Page);
                // §7.3 step 1: the owner had first refusal; an unhandled BACK is the container's
                let is_owner = matches!(parts.owner, InputOwner::Entry(e) if nav.instance_of(e) == Some(id));
                if back && handled == Handled::No && is_owner {
                    self.pending_back = true;
                }
                // the engine's half: after the owner's refusal, and after an Enter / a hold
                self.after_step(rig, parts, id, &ev, handled, out);
            }
            (MachineId::Instance(_), Delivery::Machine(_)) => {
                report.dropped_deliveries += 1;
            }
            (other, Delivery::Machine(msg)) => {
                let Dispatcher { present, .. } = self;
                let mut fx = Effects::new(out, other, present);
                let _ = rig.deliver(other, &msg, parts, &mut fx);
            }
            (_, Delivery::Screen(_)) => {
                report.dropped_deliveries += 1;
            }
        }
    }

    /// The engine's half of a delivery (§7.3): a direction the owner declined goes to the
    /// engine; an OK it declined arms a press by the element's kind (or activates a bare
    /// element on the down edge); an `Enter` seats focus; a handled `PressHold` cancels the press.
    fn after_step(
        &mut self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        id: InstanceId,
        ev: &ScreenEvent<H>,
        handled: Handled,
        out: &mut Vec<Stamped<H>>,
    ) {
        let Some(entry) = self.nav.entry_of_instance(id) else {
            return;
        };
        let is_owner = self.owner_entry() == Some(entry);
        let engine = is_owner && self.engine_page() && !self.input.keyboard;
        let owner = InputOwner::Entry(entry);
        match ev {
            ScreenEvent::PressHold(_) if handled == Handled::Yes => {
                self.input.cancel_press();
            }
            ScreenEvent::Enter(e) if is_owner && self.engine_page() => {
                let (target, restored) = match e {
                    Enter::Fresh { focus } => (*focus, None),
                    Enter::Restored => (
                        FocusTarget::ContainerGroup(GroupId(0)),
                        self.nav.entry(entry).and_then(|e| e.ret.focus),
                    ),
                };
                let Dispatcher { nav, input, .. } = self;
                let Some(view) = Self::owner_view(nav, entry) else {
                    return;
                };
                let Split { views, measure, .. } = rig.split();
                let cx = parts.cx::<H>(views, measure);
                if let Outcome::Moved { from, to, by } = input.engine.enter(owner, &view, target, restored, &cx) {
                    out.push(Stamped {
                        from: MachineId::Input,
                        fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::FocusMoved { from, to, by })),
                    });
                }
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key, edge, at_edge, .. },
                ..
            }) if handled == Handled::No && engine && *edge != Edge::Up => {
                let dir = match key {
                    Key::Up => Some(Dir::Up),
                    Key::Down => Some(Dir::Down),
                    Key::Left => Some(Dir::Left),
                    Key::Right => Some(Dir::Right),
                    _ => None,
                };
                if let Some(dir) = dir {
                    if *at_edge {
                        return; // re-delivered under EdgeRule::Screen and still unhandled: dropped
                    }
                    let mut links = Vec::new();
                    let outcome = {
                        let Dispatcher { nav, input, .. } = self;
                        let Some(view) = Self::owner_view(nav, entry) else {
                            return;
                        };
                        view.page.links(&mut links);
                        let Split { views, measure, .. } = rig.split();
                        let cx = parts.cx::<H>(views, measure);
                        input.engine.move_dir(owner, &view, &links, dir, &cx)
                    };
                    if self.input.engine.take_fell_back() {
                        rig.log("focus: no current focus — seated by the first group's policy");
                    }
                    match outcome {
                        Outcome::Moved { from, to, by } => out.push(Stamped {
                            from: MachineId::Input,
                            fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::FocusMoved { from, to, by })),
                        }),
                        Outcome::Edge(EdgeRule::Screen) => {
                            if let ScreenEvent::Input(iev) = ev {
                                let mut again = *iev;
                                if let InputKind::Key { at_edge, .. } = &mut again.kind {
                                    *at_edge = true;
                                }
                                out.push(Stamped {
                                    from: MachineId::Input,
                                    fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::Input(again))),
                                });
                            }
                        }
                        Outcome::Edge(EdgeRule::Nav(super::machine::NavOpKind::Back)) => self.pending_back = true,
                        Outcome::Edge(EdgeRule::Nav(super::machine::NavOpKind::Dismiss)) => {
                            self.parked.push(Stamped {
                                from: MachineId::Input,
                                fx: Fx::Nav(NavOp::Dismiss(entry)),
                            });
                        }
                        Outcome::Edge(_) | Outcome::Nothing => {}
                    }
                } else if *key == Key::Ok && *edge == Edge::Down {
                    let kind = {
                        let Dispatcher { nav, input, .. } = self;
                        let Some(view) = Self::owner_view(nav, entry) else {
                            return;
                        };
                        let Split { views, measure, .. } = rig.split();
                        let cx = parts.cx::<H>(views, measure);
                        input.engine.kind_of(owner, &view, &cx)
                    };
                    match kind {
                        Some((k, ElemKind::Bare)) => out.push(Stamped {
                            from: MachineId::Input,
                            fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::Activate(k.elem))),
                        }),
                        Some((k, ek)) => out.push(Stamped {
                            from: MachineId::Instance(id),
                            fx: Fx::Press(PressArm {
                                key: k,
                                from: PressFrom::Key,
                                holdable: ek == ElemKind::Card,
                            }),
                        }),
                        None => {}
                    }
                }
            }
            _ => {}
        }
    }

    /// §7.3 step 6: the owner's pure `reconcile` on the current key; a different answer is a
    /// `Reconcile` move delivered before draw.
    fn reconcile(&mut self, rig: &mut dyn Rig<H>, parts: &CxParts<H::Elem>, report: &mut FrameReport) {
        if !self.engine_page() {
            return;
        }
        let Some(entry) = self.owner_entry() else {
            return;
        };
        let owner = InputOwner::Entry(entry);
        let outcome = {
            let Dispatcher { nav, input, .. } = self;
            let Some(view) = Self::owner_view(nav, entry) else {
                return;
            };
            let Split { views, measure, .. } = rig.split();
            let cx = parts.cx::<H>(views, measure);
            input.engine.reconcile(owner, &view, &cx)
        };
        if let Outcome::Moved { from, to, by } = outcome {
            if let Some(id) = self.nav.instance_of(entry) {
                let mut out = Vec::new();
                self.execute_deliver(rig, parts, MachineId::Instance(id), Delivery::Screen(ScreenEvent::FocusMoved { from, to, by }), &mut out, report);
                self.absorb(out);
            }
        }
    }

    /// NAV COMMIT (§3.3 step 7, §3.4): the parked ops are REQUESTED on the tree (a cut applies
    /// now, a transition at its floor), the tree's due ops apply, and every resulting lifecycle
    /// step is executed here — mounts through the `Mounter`, events queued for the post-commit
    /// drain. A structural op emitted DURING that drain is parked for the NEXT frame (one commit
    /// per frame).
    fn commit(&mut self, rig: &mut dyn Rig<H>, parts: &CxParts<H::Elem>, report: &mut FrameReport) {
        let parked = std::mem::take(&mut self.parked);
        let mut post: Vec<Stamped<H>> = Vec::new();
        let mut life: Vec<Life<H>> = std::mem::take(&mut self.parked_life);
        if std::mem::take(&mut self.pending_back) {
            let ret = self.ret();
            let (answer, steps) = self.nav.back(ret);
            life.extend(steps);
            if answer == super::containers::BackAnswer::AtRoot {
                report.back_at_root = true;
                rig.back_at_root();
            }
        }
        for item in parked {
            match item.fx {
                Fx::Nav(op) => {
                    let ret = self.ret();
                    life.extend(self.nav.request(op, ret));
                }
                Fx::Mount(eid) => life.push(Life::Mount(eid)),
                Fx::Unmount(eid) => life.push(Life::Unmount(eid)),
                _ => unreachable!("only structural ops are parked"),
            }
        }
        life.extend(self.nav.commit());
        for step in life {
            match step {
                Life::Mount(eid) => self.mount(rig, parts, eid, &mut post, report),
                Life::Ev(eid, ev) => self.push_lifecycle(eid, ev, &mut post),
                Life::Unmount(eid) | Life::Evict(eid) => self.unmount(eid, &mut post, report),
            }
        }
        // ahead of the carried queue: the mount that a key asked for happens THIS frame
        for s in post.into_iter().rev() {
            self.queue.push_front(s);
        }
    }

    fn push_lifecycle(&mut self, eid: EntryId, ev: ScreenEvent<H>, post: &mut Vec<Stamped<H>>) {
        if let Some(id) = self.nav.instance_of(eid) {
            post.push(Stamped {
                from: MachineId::Nav,
                fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ev)),
            });
        }
    }

    /// Mount: mint the `InstanceId`, call the one `mount` match, deliver `Mount` first.
    fn mount(
        &mut self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        eid: EntryId,
        post: &mut Vec<Stamped<H>>,
        report: &mut FrameReport,
    ) {
        if self.nav.entry(eid).map_or(true, |e| e.inst.is_some()) {
            return;
        }
        let id = self.nav.ids.instance();
        let mut out: Vec<Stamped<H>> = Vec::new();
        let screen = {
            let Dispatcher { nav, present, .. } = self;
            let entry = nav.entry(eid).expect("checked above");
            let Split {
                mounter,
                views,
                measure,
            } = rig.split();
            // the body being mounted is the owner its `mount` reads (`cx.owner`)
            let mut p = *parts;
            p.owner = InputOwner::Entry(eid);
            let cx = p.cx::<H>(views, measure);
            let mut fx = Effects::new(&mut out, MachineId::Instance(id), present);
            mounter.mount(id, &entry.arg, &entry.ret, &cx, &mut fx)
        };
        if let Some(entry) = self.nav.entry_mut(eid) {
            entry.inst = Some(Instance {
                id,
                screen,
                inflight: Vec::new(),
            });
            entry.evicted = false;
        }
        post.push(Stamped {
            from: MachineId::Nav,
            fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::Mount)),
        });
        post.extend(out);
        report.mounted.push(id);
    }

    /// Register a request as in flight for an instance (the app's registry calls this when it
    /// mints an `Addr` for a screen's request).
    pub fn track_inflight(&mut self, inst: InstanceId, req: RequestId) {
        if let Some(i) = self.nav.instance_mut(inst) {
            i.inflight.push(req);
        }
    }

    fn unmount(&mut self, eid: EntryId, post: &mut Vec<Stamped<H>>, report: &mut FrameReport) {
        let Some(inst) = self.nav.entry_mut(eid).and_then(|e| e.inst.take()) else {
            return;
        };
        // retire inflight: the live index no longer answers for it (§6.1 eviction rule)
        report.unmounted.push(inst.id);
        // the engine forgets an entry only when it leaves for GOOD; an evicted body keeps its
        // cursor for the remount (§6.1)
        let evicted = self.nav.entry(eid).map_or(false, |e| e.evicted);
        if !evicted {
            self.input.engine.forget(eid);
        }
        if self.input.arm.map_or(false, |a| a.owner == MachineId::Instance(inst.id)) {
            self.input.cancel_press();
        }
        // the body gets its Unmount as the last thing it hears; it is stepped from `post` while it
        // is still reachable, so keep it on the entry until then
        let id = inst.id;
        if let Some(entry) = self.nav.entry_mut(eid) {
            entry.inst = Some(Instance {
                id,
                screen: inst.screen,
                inflight: Vec::new(),
            });
        }
        post.push(Stamped {
            from: MachineId::Nav,
            fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::Unmount)),
        });
    }

    /// Called at the frame tail: drop the bodies whose `Unmount` was delivered.
    pub fn prune(&mut self, unmounted: &[InstanceId]) {
        self.nav.prune(unmounted);
    }

    pub fn frame_index(&self) -> u64 {
        self.frame
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn dropped_deliveries(&self) -> u32 {
        self.dropped_deliveries
    }

    pub fn viewport() -> Rect {
        Rect::FULL
    }
}
