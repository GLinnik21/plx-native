//! The frame — ONE algorithm (spec §3.3), generic over the `Host` bundle and a `Rig` that owns
//! whatever the dispatcher does not: the stores and the other addressable machines, the mounter,
//! the views, the measure, the adapters, and the three privileged OS calls (`ls2_pump`,
//! `opaque_route`, `clear_opaque_region`) which `app/run.rs` implements and this module only
//! names as hooks.
//!
//! Phase 2-i: the ten steps over one frame, with the step budgets, the parked structural ops,
//! the carry-over that never drops, the single NAV COMMIT with its reserved post-commit budget,
//! the present decision and the prepare/draw pair — over a minimal `NavTree` (one stack of
//! entries; tabs and modals are phase 3b's containers). What is deliberately NOT here: the focus
//! engine and the hit map (3b), timers' owner registry beyond the id (2), the recorder taps (2).
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

use std::collections::VecDeque;

use super::frame::Budget;
use super::machine::{
    Addr, Cx, Delivery, Effects, EntryId, FocusRead, FocusKey, Fx, Handled, Host, InputEvent,
    InputOwner, InstanceId, MachineId, Measure, NavOp, PressRead, RequestId, Stamped, Tick,
    TimerId,
};
use super::present::Present;
use super::screen::{
    pop_sequence, push_sequence, DrawFrame, Enter, FocusTarget, Mounter, ReturnState, Screen,
    ScreenEvent, Stop,
};
use super::{Painter, Rect};

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

/// A mounted body (§5.1).
pub struct Instance<H: Host> {
    pub id: InstanceId,
    pub screen: Box<dyn Screen<H>>,
    pub inflight: Vec<RequestId>,
}

/// An entry in the one stack (§6.2 `NavStack`, minimal).
pub struct Entry<H: Host> {
    pub id: EntryId,
    pub arg: H::Arg,
    pub ret: ReturnState<H::Elem>,
    pub inst: Option<Instance<H>>,
}

/// The container tree, phase 2-i shape: one stack. `Navigation` owns every entry and is the sole
/// owner of the live/inflight index behind `is_deliverable`.
pub struct NavTree<H: Host> {
    pub stack: Vec<Entry<H>>,
    next_entry: u32,
    next_inst: u32,
}

impl<H: Host> Default for NavTree<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Host> NavTree<H> {
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            next_entry: 0,
            next_inst: 0,
        }
    }

    fn mint_entry(&mut self, arg: H::Arg) -> EntryId {
        self.next_entry += 1;
        let id = EntryId(self.next_entry);
        self.stack.push(Entry {
            id,
            arg,
            ret: ReturnState::default(),
            inst: None,
        });
        id
    }

    fn mint_instance(&mut self) -> InstanceId {
        self.next_inst += 1;
        InstanceId(self.next_inst)
    }

    pub fn top(&self) -> Option<&Entry<H>> {
        self.stack.last()
    }

    pub fn entry_mut(&mut self, id: EntryId) -> Option<&mut Entry<H>> {
        self.stack.iter_mut().find(|e| e.id == id)
    }

    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut Instance<H>> {
        self.stack
            .iter_mut()
            .filter_map(|e| e.inst.as_mut())
            .find(|i| i.id == id)
    }

    /// The live index: is this address deliverable (§5.2).
    pub fn is_deliverable(&self, addr: &Addr) -> bool {
        match addr.to {
            MachineId::Instance(id) => self
                .stack
                .iter()
                .filter_map(|e| e.inst.as_ref())
                .any(|i| i.id == id && i.inflight.contains(&addr.req)),
            _ => true,
        }
    }

    pub fn input_owner(&self) -> Option<InputOwner> {
        self.top().map(|e| InputOwner::Entry(e.id))
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
}

/// The dispatcher: the queue that is never dropped, the parked structural ops, the timers, the
/// present gate, the budget, the tree.
pub struct Dispatcher<H: Host> {
    queue: VecDeque<Stamped<H>>,
    parked: Vec<Stamped<H>>,
    timers: Vec<(TimerId, u32, MachineId)>,
    pub present: Present,
    pub budget: Budget,
    pub nav: NavTree<H>,
    focus: Option<FocusKey<H::Elem>>,
    last_stops: Vec<Stop<H::Elem>>,
    frame: u64,
    carried_streak: u8,
    last_carried: usize,
    dropped_deliveries: u32,
}

impl<H: Host> Default for Dispatcher<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Host> Dispatcher<H> {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            parked: Vec::new(),
            timers: Vec::new(),
            present: Present::new(),
            budget: Budget::new(),
            nav: NavTree::new(),
            focus: None,
            last_stops: Vec::new(),
            frame: 0,
            carried_streak: 0,
            last_carried: 0,
            dropped_deliveries: 0,
        }
    }

    /// The hit map of the last PRESENTED frame (§7.6): what a click on an idle frame resolves against.
    pub fn last_stops(&self) -> &[Stop<H::Elem>] {
        &self.last_stops
    }

    /// Queue a structural op from OUTSIDE a step (boot's `Root`, a lifecycle `Suspend`): it is
    /// parked like any other and applies at this frame's NAV COMMIT.
    pub fn request(&mut self, from: MachineId, op: NavOp<H::Arg>) {
        self.parked.push(Stamped {
            from,
            fx: Fx::Nav(op),
        });
    }

    fn parts(&self, tick: Tick) -> CxParts<H::Elem> {
        CxParts {
            tick,
            press: PressRead::default(),
            focus: FocusRead { current: self.focus },
            owner: self
                .nav
                .input_owner()
                .unwrap_or(InputOwner::Entry(EntryId(0))),
        }
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
        let event_frame = !inputs.is_empty() || !results.is_empty() || !self.parked.is_empty();

        // 1. the first privileged call
        rig.ls2_pump();

        // 2. ingest: external events, delivered to the input owner, AHEAD of everything queued
        //    (external_events_are_drained_before_effect_results).
        let owner = self.parts(tick).owner;
        let mut head: Vec<Stamped<H>> = Vec::new();
        for ev in inputs {
            tap.input(f, &ev);
            if let InputOwner::Entry(eid) = owner {
                if let Some(inst) = self.nav.entry_mut(eid).and_then(|e| e.inst.as_ref()) {
                    head.push(Stamped {
                        from: MachineId::Input,
                        fx: Fx::Deliver(
                            MachineId::Instance(inst.id),
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
        // 4. one Tick to every live instance (containers before pages: one stack here)
        for e in &self.nav.stack {
            if let Some(inst) = &e.inst {
                head.push(Stamped {
                    from: MachineId::Nav,
                    fx: Fx::Deliver(
                        MachineId::Instance(inst.id),
                        Delivery::Screen(ScreenEvent::Tick(tick)),
                    ),
                });
            }
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
        self.commit(rig, &parts, &mut report);
        report.steps_post = self.drain(rig, &parts, MAX_STEPS_POST, &mut report, tap);
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
        let will_present = self.present.take(tick.ms) || self.budget.has_queued_work();
        tap.present(f, will_present, why);

        // 9. prepare (only if presenting), then opaque_route on EVERY frame
        if will_present {
            self.budget.begin_frame(rig.now_us());
            let parts = self.parts(tick);
            {
                let Dispatcher { nav, budget, .. } = self;
                let Split { views, measure, .. } = rig.split();
                let cx = parts.cx::<H>(views, measure);
                if let Some(inst) = nav.stack.last_mut().and_then(|e| e.inst.as_mut()) {
                    inst.screen.prepare(budget, &cx);
                }
            }
            let Dispatcher {
                budget, present, ..
            } = self;
            rig.prepare(budget, present);
        }
        rig.opaque_route(self.present.video_plane());

        // 10. draw, then the tail
        if will_present {
            rig.clear_opaque_region();
            let parts = self.parts(tick);
            let Dispatcher { nav, .. } = self;
            let Split { views, measure, .. } = rig.split();
            let cx = parts.cx::<H>(views, measure);
            let mut stops = Vec::new();
            if let Some(inst) = nav.stack.last_mut().and_then(|e| e.inst.as_mut()) {
                let mut f = DrawFrame::new(&cx, Painter::root());
                inst.screen.draw(&mut f);
                stops = f.into_stops();
            }
            // the hit map swaps only on a presented frame (§7.6)
            self.last_stops = stops;
            if let Some(fault) = self.present.take_fault() {
                rig.log(&format!("dispatch: fault {fault:?}"));
            }
        }
        report.presented = will_present;
        tap.frame_done(f);
        report
    }

    /// The logical-state hash (spec §5.4): every live instance's `LogicalState`, the focus, the
    /// stack's entry ids and the present gate's video-plane bit, in a fixed order. Incremental
    /// hashing (dirty flags per machine) is the optimisation the spec names; this is the
    /// definition it must equal.
    pub fn state_hash(&self) -> u64 {
        let mut c = super::machine::Canon::new();
        c.seq(self.nav.stack.len());
        for e in &self.nav.stack {
            c.u32(e.id.0);
            c.option(e.inst.as_ref(), |c, i| {
                c.u32(i.id.0);
                c.u64(i.screen.state().hash());
            });
        }
        c.option(self.focus, |c, fk| {
            c.u32(fk.entry.0);
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
                Fx::Press(_) => {
                    // the press machine is phase 2's Input machine; the arm is recorded as a step
                    steps += 1;
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
                let Dispatcher { nav, present, .. } = self;
                let Some(inst) = nav.instance_mut(id) else {
                    report.dropped_deliveries += 1;
                    return;
                };
                let Split { views, measure, .. } = rig.split();
                let cx = parts.cx::<H>(views, measure);
                let mut fx = Effects::new(out, to, present);
                let _ = inst.screen.step(&ev, &cx, &mut fx);
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

    /// NAV COMMIT (§3.3 step 7, §3.4): apply the parked ops, mount/unmount, and queue the
    /// lifecycle events for the post-commit drain. A structural op emitted DURING that drain is
    /// parked for the NEXT frame (one commit per frame).
    fn commit(&mut self, rig: &mut dyn Rig<H>, parts: &CxParts<H::Elem>, report: &mut FrameReport) {
        let parked = std::mem::take(&mut self.parked);
        let mut post: Vec<Stamped<H>> = Vec::new();
        for item in parked {
            match item.fx {
                Fx::Nav(op) => self.apply_nav(rig, parts, op, &mut post, report),
                Fx::Mount(eid) => self.mount(rig, parts, eid, &mut post, report),
                Fx::Unmount(eid) => self.unmount(eid, &mut post, report),
                _ => unreachable!("only structural ops are parked"),
            }
        }
        // ahead of the carried queue: the mount that a key asked for happens THIS frame
        for s in post.into_iter().rev() {
            self.queue.push_front(s);
        }
    }

    fn apply_nav(
        &mut self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        op: NavOp<H::Arg>,
        post: &mut Vec<Stamped<H>>,
        report: &mut FrameReport,
    ) {
        let is_replace = matches!(op, NavOp::Replace(_));
        match op {
            NavOp::Push(arg) | NavOp::Present(arg) => {
                let old = self.nav.top().map(|e| e.id);
                let new = self.nav.mint_entry(arg);
                self.mount(rig, parts, new, post, report);
                let Some(inst) = self.nav.entry_mut(new).and_then(|e| e.inst.as_ref()) else {
                    return;
                };
                let inst_id = inst.id;
                let seq = push_sequence::<H>(old, new, FocusTarget::ContainerGroup(super::machine::GroupId(0)));
                for (eid, ev) in seq {
                    if matches!(ev, ScreenEvent::Mount) {
                        continue; // delivered by `mount` itself, first
                    }
                    let hint = (eid == new).then_some(inst_id);
                    self.push_lifecycle(eid, ev, post, hint);
                }
            }
            NavOp::Root(arg) | NavOp::SelectTab(arg) | NavOp::Replace(arg) => {
                let op_is_replace = is_replace;
                // Root/SelectTab: every entry leaves for good; Replace: only the top does and
                // the entry beneath stays covered (§3.4).
                let leaving: Vec<EntryId> = match op_is_replace {
                    true => self.nav.top().map(|e| e.id).into_iter().collect(),
                    false => self.nav.stack.iter().rev().map(|e| e.id).collect(),
                };
                for eid in leaving {
                    self.push_lifecycle(eid, ScreenEvent::WillLeave(super::machine::Leave::ForGood), post, None);
                    self.unmount(eid, post, report);
                    self.nav.stack.retain(|e| e.id != eid);
                }
                let new = self.nav.mint_entry(arg);
                self.mount(rig, parts, new, post, report);
                if let Some(inst) = self.nav.entry_mut(new).and_then(|e| e.inst.as_ref()) {
                    let inst_id = inst.id;
                    self.push_lifecycle(
                        new,
                        ScreenEvent::Enter(Enter::Fresh {
                            focus: FocusTarget::ContainerGroup(super::machine::GroupId(0)),
                        }),
                        post,
                        Some(inst_id),
                    );
                }
            }
            NavOp::Pop => {
                let Some(top) = self.nav.top().map(|e| e.id) else {
                    return;
                };
                let under = self.nav.stack.iter().rev().nth(1).map(|e| e.id);
                for (eid, ev) in pop_sequence::<H>(top, under) {
                    if matches!(ev, ScreenEvent::Unmount) {
                        self.unmount(eid, post, report);
                        self.nav.stack.retain(|e| e.id != eid);
                    } else {
                        self.push_lifecycle(eid, ev, post, None);
                    }
                }
            }
            NavOp::PopTo(target) | NavOp::Dismiss(target) => {
                let above: Vec<EntryId> = self
                    .nav
                    .stack
                    .iter()
                    .rev()
                    .take_while(|e| e.id != target)
                    .map(|e| e.id)
                    .collect();
                if above.len() == self.nav.stack.len() {
                    return; // the target is not on the stack: nothing to do
                }
                for eid in above {
                    self.push_lifecycle(eid, ScreenEvent::WillLeave(super::machine::Leave::ForGood), post, None);
                    self.unmount(eid, post, report);
                    self.nav.stack.retain(|e| e.id != eid);
                }
                self.push_lifecycle(target, ScreenEvent::Uncover, post, None);
                self.push_lifecycle(target, ScreenEvent::Enter(Enter::Restored), post, None);
            }
            NavOp::Cancel => {}
        }
    }

    fn push_lifecycle(
        &mut self,
        eid: EntryId,
        ev: ScreenEvent<H>,
        post: &mut Vec<Stamped<H>>,
        inst_hint: Option<InstanceId>,
    ) {
        let inst = inst_hint.or_else(|| {
            self.nav
                .stack
                .iter()
                .find(|e| e.id == eid)
                .and_then(|e| e.inst.as_ref())
                .map(|i| i.id)
        });
        if let Some(id) = inst {
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
        let Some(idx) = self.nav.stack.iter().position(|e| e.id == eid) else {
            return;
        };
        if self.nav.stack[idx].inst.is_some() {
            return;
        }
        let id = self.nav.mint_instance();
        let mut out: Vec<Stamped<H>> = Vec::new();
        let screen = {
            let Dispatcher { nav, present, .. } = self;
            let entry = &nav.stack[idx];
            let Split {
                mounter,
                views,
                measure,
            } = rig.split();
            let cx = parts.cx::<H>(views, measure);
            let mut fx = Effects::new(&mut out, MachineId::Instance(id), present);
            mounter.mount(id, &entry.arg, &entry.ret, &cx, &mut fx)
        };
        self.nav.stack[idx].inst = Some(Instance {
            id,
            screen,
            inflight: Vec::new(),
        });
        // a request emitted from `mount` is inflight for this instance
        for s in &out {
            if let Fx::App(_) = s.fx {
                // the app maps its own request ids; the spike registers a synthetic one so the
                // address is deliverable (the registry's job in phase 2)
            }
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
        let Some(entry) = self.nav.entry_mut(eid) else {
            return;
        };
        let Some(inst) = entry.inst.take() else {
            return;
        };
        // retire inflight: the live index no longer answers for it (§6.1 eviction rule)
        report.unmounted.push(inst.id);
        // the body gets its Unmount as the last thing it hears; it is stepped from `post` while it
        // is still reachable, so keep it on the entry until then
        entry.inst = Some(Instance {
            id: inst.id,
            screen: inst.screen,
            inflight: Vec::new(),
        });
        post.push(Stamped {
            from: MachineId::Nav,
            fx: Fx::Deliver(MachineId::Instance(inst.id), Delivery::Screen(ScreenEvent::Unmount)),
        });
    }

    /// Called at the frame tail by phase 2's tree: drop the bodies whose `Unmount` was delivered.
    pub fn prune(&mut self, unmounted: &[InstanceId]) {
        for e in &mut self.nav.stack {
            if let Some(i) = &e.inst {
                if unmounted.contains(&i.id) {
                    e.inst = None;
                }
            }
        }
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
