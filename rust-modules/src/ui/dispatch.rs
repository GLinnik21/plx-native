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
#![allow(dead_code)] // since 5b screens run through most of this; suspend/resume/set_focus_source_override/viewport/frame_index still have no caller anywhere (grep-checked, not 3b's stale "screens from 5b")

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
    GroupSpec, HitSource, Mounter, Placed, ReturnState, Screen, ScreenArg, ScreenEvent, Step, Stop,
};
use super::{Painter, Rect};

/// The strip's control namespace: `of_index(STRIP_BASE + stable_id)`, never display position.
/// A removed pill therefore cannot renumber another destination or collide with a page's tile.
pub const STRIP_BASE: u32 = 0xFFFF_0000;

/// The input owner's page COMPOSED with the container's strip (§6.2): the strip is a `Row`
/// group above the page's own, contributed only while the page allows it.
struct PageWithStrip<'a, H: Host> {
    page: &'a dyn Screen<H>,
    strip: Option<(GroupSpec, &'a [super::containers::tabs::StripMember<H::Elem>])>,
    strip_fallback: Option<H::Elem>,
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
            Some(i) if i >= STRIP_BASE => self.strip.as_ref()
                .filter(|(_, members)| members.iter().any(|m| m.elem == *key)).map(|(s, _)| s.id),
            _ => self.page.group_of(key, cx),
        }
    }
    fn neighbour(&self, k: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        match (k.elem.index(), &self.strip) {
            (Some(i), Some((_, members))) if i >= STRIP_BASE => {
                let Some(i) = members.iter().position(|m| m.elem == k.elem) else { return Step::Edge };
                let mv = |j: usize| Step::Move(FocusKey {
                    entry: self.entry,
                    elem: members[j].elem,
                });
                match dir {
                    Dir::Left if i > 0 => mv(i - 1),
                    Dir::Right if i + 1 < members.len() => mv(i + 1),
                    _ => Step::Edge,
                }
            }
            _ => self.page.neighbour(k, dir, cx),
        }
    }
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        match (key.index(), &self.strip) {
            (Some(i), Some((_, members))) if i >= STRIP_BASE => {
                let index = members.iter().position(|m| m.elem == *key)?;
                let member = &members[index];
                Some(Placed {
                    rect: match at { At::Drawn => member.drawn, At::SpringTarget => member.target },
                    rest_rect: member.target,
                    clip: member.clip,
                    index: Some(index as u32),
                })
            }
            _ => self.page.place(key, cx, at),
        }
    }
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        match (want.elem.index(), &self.strip) {
            (Some(i), Some((_, members))) if i >= STRIP_BASE =>
                FocusKey { entry: self.entry,
                    elem: members.iter().find(|m| m.elem == want.elem)
                        .or_else(|| self.strip_fallback.and_then(|key| members.iter().find(|m| m.elem == key)))
                        .unwrap_or(&members[0]).elem },
            _ => self.page.reconcile(want, cx),
        }
    }
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        match &self.strip {
            Some((spec, members)) if spec.id == g => {
                let cx_ = from.rect.cx();
                let mut best = (f32::MAX, 0usize);
                for (i, member) in members.iter().enumerate() {
                    let d = (member.target.cx() - cx_).abs();
                    if d < best.0 {
                        best = (d, i);
                    }
                }
                FocusKey {
                    entry: self.entry,
                    elem: members[best.1].elem,
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
    /// Snapshot at effect execution, before another input can move the requesting page.
    fn app_return(&mut self, _from: MachineId, _ret: ReturnState<H::Elem, H::Memory>) {}
    /// Main-thread native operation, reached only after Input validates the requesting owner.
    fn system_keyboard(&mut self, _up: bool) {}
    /// A native text commit proves the panel is already active; do not start it again.
    fn adopt_system_keyboard(&mut self) {}
    fn page_alpha(&self) -> f32 { 1.0 }
    fn navigation_presentation(&self) -> super::screen::NavPresentation {
        super::screen::NavPresentation { page_alpha: self.page_alpha(), ..Default::default() }
    }
    /// Shared chrome draws after its page and before surfaces, using a captured vocabulary.
    fn draw_chrome(&mut self, _arg: &H::Arg, _parts: &CxParts<H::Elem>, _nav: super::screen::NavPresentation) {}
    /// The shared top bar's render values this frame — `(chip_expand, bar_glass_face)` — for a
    /// `Scrim::lift` bare `fn` that redraws a piece of that bar over a surface's dim (spec phase
    /// 12, PX-WIDGETS; today's one caller is the account menu's chip lift). Default `(0.0, None)`
    /// is correct for any rig with no such bar — a fixture, or a host test.
    fn scrim_chip_read(&self) -> (f32, Option<crate::gfx::GlassFace>) { (0.0, None) }
    /// Coexistence adapter for a legacy overlay which freezes an owned host (phase 10 retirement).
    fn page_updates(&self) -> bool { true }
    /// The application lifts its legacy host-cache cull before drawing an owned surface.
    fn surface_scope(&mut self) -> Option<super::popover::host::Live> { None }
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
#[derive(Clone)]
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
            focus: self.focus.clone(),
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
    /// Bodies mounted this frame, each with its `Screen::name()` — the cold-open clock's START
    /// (`diag::heartbeat::ColdOpens`). The name rides along because by the time anything
    /// downstream reads this the body is behind the tree, and asking the tree for it would be a
    /// second lookup that can fail (the body may already be gone).
    pub mounted: Vec<(InstanceId, &'static str)>,
    /// Bodies whose Unmount step completed and may now be pruned, not merely queued.
    pub unmounted: Vec<InstanceId>,
    /// Bodies that DREW this frame, in draw order — the cold-open clock's STOP.
    pub drawn: Vec<InstanceId>,
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
    parked: Vec<(Stamped<H>, ReturnState<H::Elem, H::Memory>)>,
    app_returns: VecDeque<(MachineId, ReturnState<H::Elem, H::Memory>)>,
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
    /// Deliveries dropped since the heartbeat last read them (`take_heartbeat_counters`).
    dropped_deliveries: u32,
    /// The cold-open instrument (spec §8.4). It lives here rather than beside the loop's other
    /// instruments because both of its events are the dispatcher's — a mount at nav commit and a
    /// draw in the page/surface pass — and neither is visible from outside one `FrameReport`.
    cold: crate::diag::heartbeat::ColdOpens,
    render_breach_logged: bool,
    /// The input owner answered `Handled::No` to a BACK: resolve it over its stack at commit.
    pending_back: bool,
    /// The tick of the last `frame_with`, for a `draw` the caller runs later in its own frame.
    last_tick: Tick,
    /// This frame's prepare pass ran (a `draw` after a non-presenting frame runs it itself).
    prepared: bool,
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
            app_returns: VecDeque::new(),
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
            cold: Default::default(),
            render_breach_logged: false,
            pending_back: false,
            last_tick: Tick::default(),
            prepared: false,
        }
    }

    /// Does the dispatcher OWN the input right now (phase 5b, the coexistence seam): the input
    /// owner is a surface, or a page whose focus comes from the engine — i.e. anything but a
    /// screen that still declares `FocusSource::Legacy` (the player, until phase 12). The legacy
    /// loop hands its keys and pointer to `frame_with` while this answers `true` and keeps them
    /// for its own ladders otherwise.
    pub fn owns_input(&self) -> bool {
        self.engine_page()
    }

    /// A surface is up (any phase but Hidden): the legacy loop draws the tree at its surface slot.
    pub fn surface_up(&self) -> bool {
        self.nav
            .modals
            .surfaces
            .iter()
            .any(|s| s.phase != super::containers::modal::Phase::Hidden)
    }

    /// The topmost surface's heartbeat word, if a surface is up.
    pub fn top_surface_name(&self) -> Option<&'static str> {
        self.nav
            .modals
            .surfaces
            .iter()
            .rev()
            .find(|s| s.phase != super::containers::modal::Phase::Hidden)
            .and_then(|s| s.entry.inst.as_ref())
            .map(|i| i.screen.name())
    }

    /// The host fold (§6.2, §8.3) as it stands now.
    pub fn host_policy(&self) -> (HostUpdate, HostRender) {
        self.nav.modals.host_policy()
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
        if matches!(fx, Fx::App(_)) {
            self.app_returns.push_back((from, self.return_state()));
        }
        self.queue.push_back(Stamped { from, fx });
    }

    /// Queue a structural op from OUTSIDE a step (boot's `Root`, a lifecycle `Suspend`): it is
    /// parked like any other and applies at this frame's NAV COMMIT.
    pub fn request(&mut self, from: MachineId, op: NavOp<H::Arg>) {
        self.request_with_return(from, op, self.return_state());
    }

    pub fn request_with_return(&mut self, from: MachineId, op: NavOp<H::Arg>, ret: ReturnState<H::Elem, H::Memory>) {
        self.parked.push((Stamped {
            from,
            fx: Fx::Nav(op),
        }, ret));
    }

    fn park(&mut self, item: Stamped<H>) {
        let ret = self.return_state();
        self.parked.push((item, ret));
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

    /// **The top page's ARGUMENT** — "which page is on top", asked of the container rather than of
    /// a mirror kept beside it (spec §15.2). It answers before the body is mounted, which
    /// [`Self::top_screen`] cannot: an entry minted this frame has an argument and no instance
    /// until the commit's `Mount` step has run.
    pub fn top_arg(&self) -> Option<&H::Arg> {
        self.nav.top_page().map(|e| &e.arg)
    }

    /// …and its `ScreenId`, the identity every argument of one screen shares.
    pub fn top_id(&self) -> Option<super::machine::ScreenId> {
        use super::screen::ScreenArg;
        self.top_arg().map(|a| a.id())
    }

    /// The top page's screen, for a test to read.
    pub fn top_screen(&self) -> Option<&dyn super::screen::Screen<H>> {
        self.nav.top_page().and_then(|e| e.inst.as_ref()).map(|i| &*i.screen)
    }

    /// **Is a PAGE navigation already parked for this frame's commit?**
    ///
    /// Asked by `app::bridge::sync_page`, whose whole job is to put the committed route's page on
    /// top: a page op parked by the loop itself (`Nav::Open`'s `Push`, `Nav::Back`'s `Pop`) is
    /// already that decision, and a second one would duplicate the page. A parked SURFACE op is
    /// not — `Navigation::moves_page` is the one classifier, so this and the commit that routes
    /// the op give the same answer. It used to match `Fx::Nav(_)` flatly, which made
    /// `exit_player`'s `NavOp::Dismiss` of an open player panel suppress the page sync for the
    /// very frame that carried the post-player route.
    pub fn has_pending_navigation(&self) -> bool {
        self.parked.iter().any(|(s, _)| match &s.fx {
            Fx::Nav(op) => self.nav.moves_page(op),
            _ => false,
        })
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

    /// The outgoing top screen's return state (spec §6.1 tier 2): focus off the engine, and
    /// [`Screen::memory`] off the screen itself — the same "ask the owner, never assume" rule
    /// [`Screen::state`]/`crumb` already follow. `top_screen()` is `None` only before the very
    /// first mount, when there is nothing to remember either.
    pub fn return_state(&self) -> ReturnState<H::Elem, H::Memory> {
        ReturnState {
            focus: self.focus(),
            remembered: self.owner_entry().map(|e| self.input.engine.remembered_for(e)).unwrap_or_default(),
            scroll: 0.0,
            memory: self.owner_entry().and_then(|e| self.nav.entry(e))
                .and_then(|e| e.inst.as_ref()).map(|i| i.screen.memory_at(self.focus())).unwrap_or_default(),
        }
    }

    fn ret(&self) -> ReturnState<H::Elem, H::Memory> {
        self.return_state()
    }

    fn parts(&self, tick: Tick) -> CxParts<H::Elem> {
        let owner = self.owner();
        CxParts {
            tick,
            press: PressRead {
                scale: self.input.press.scale(),
                is_long: self.input.press.was_long(),
            },
            focus: self.input.engine.read(owner),
            owner,
        }
    }

    /// The owner's page composed with the strip (§6.2), for one engine query.
    fn owner_view(nav: &Navigation<H>, entry: EntryId) -> Option<PageWithStrip<'_, H>> {
        let e = nav.entry(entry)?;
        let inst = e.inst.as_ref()?;
        let page: &dyn Screen<H> = &*inst.screen;
        let is_top_page = nav.top_page().map(|t| t.id) == Some(entry);
        let strip = (is_top_page && page.strip_reachable() && !nav.tabs.strip.is_empty())
            .then(|| (nav.tabs.strip_group(), nav.tabs.strip.as_slice()));
        Some(PageWithStrip { page, strip, strip_fallback: nav.tabs.strip_fallback, entry })
    }

    /// **Is this frame's picture a hardware VIDEO PLANE?** (spec §9, §16 risk 10.)
    ///
    /// TWO terms, and both are required. The top page must ANSWER
    /// [`RenderStrategy::VideoPlane`] — a declaration about what it draws, not about where it is —
    /// and the plane must actually be BOUND, which arrives as the gate's
    /// [`PresentEvent::VideoPlane`](super::present::PresentEvent::VideoPlane) input from the one
    /// machine that owns that bit. Either term alone is wrong: the player page is up for the whole
    /// pre-bind spinner and the whole post-unbind read-out, where our surface holds an ordinary
    /// picture, and a bound plane under some other page is not a thing this tree can produce.
    ///
    /// What it decides, in this file: nothing below the top page ticks or draws — `(Frozen,
    /// Replaced)` in §6.2's vocabulary, applied to everything UNDER the page rather than to the
    /// page itself — because there is nothing to see under a hole punched in the surface. The
    /// other half, the four framebuffer-sampling doors, is armed for the length of the draw and
    /// enforced in `gfx::video_plane_refuses`.
    pub fn video_plane_frame(&self) -> bool {
        self.present.video_plane()
            && self.top_screen().map(|s| s.render()) == Some(super::screen::RenderStrategy::VideoPlane)
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
        self.frame_with(rig, tick, inputs, results, tap, true)
    }

    /// Steps 1–9, and step 10 only if `draw`. The legacy loop (phase 5b onwards) runs the
    /// dispatcher's frame at its NAV COMMIT and draws the tree LATER, at the positional point
    /// its own frame reserves for the surfaces, on its own present gate — so it asks for the
    /// steps without the draw here and calls [`Self::draw`] there. The prepare pass still runs
    /// here when the dispatcher's gate presents; `draw` runs it itself when it did not.
    pub fn frame_with(
        &mut self,
        rig: &mut dyn Rig<H>,
        tick: Tick,
        inputs: Vec<InputEvent<H::Elem>>,
        results: Vec<(Addr, H::Msg)>,
        tap: &mut dyn Tap<H>,
        draw: bool,
    ) -> FrameReport {
        self.frame += 1;
        self.last_tick = tick;
        self.prepared = false;
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
                        let res = self.input.hit.resolve(owner_e, kind, x, y, focused);
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
                                        let kind = {
                                            let view = Self::owner_view(&self.nav, e);
                                            let Split { views, measure, .. } = rig.split();
                                            let cx = self.parts(tick).cx::<H>(views, measure);
                                            view.and_then(|view| self.input.engine.kind_of_key(k, &view, &cx))
                                        };
                                        if let Some(kind) = kind { self.input.arm(
                                            PressArm {
                                                key: k,
                                                from: PressFrom::Pointer,
                                                holdable: kind == ElemKind::Card,
                                            },
                                            MachineId::Instance(inst),
                                            tick.ms,
                                        ); }
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
                                self.park(Stamped {
                                    from: MachineId::Input,
                                    fx: Fx::Nav(NavOp::Dismiss(id)),
                                });
                            }
                        }
                    }
                }
            }
            match ev.kind {
                // With a page, this transition belongs to its ordered input delivery below,
                // not this pre-pass over the entire batch. No page means no queued delivery.
                InputKind::SystemKeyboard(up) if self.owner_entry().is_none() => {
                    self.input.keyboard = up;
                    self.input.keyboard_owner = None;
                    self.input.cancel_press();
                }
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
            .rev()
            .take(if self.nav.tabs.stack.transition.draws_below() && !self.video_plane_frame() {
                2
            } else {
                // §9: `(Frozen, …)` on everything below a bound video plane. A page that cannot be
                // seen must not be stepped either — its springs would run out their travel behind
                // the picture and land settled, so the cross-fade back out of the player would
                // begin already over.
                1
            })
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
            let (delivery, owner) = match pe {
                PressEvent::Hold(id, owner, key) => (Delivery::Press { id, key, held: true }, owner),
                PressEvent::Commit(id, owner, key) => (Delivery::Press { id, key, held: false }, owner),
            };
            head.push(Stamped {
                from: MachineId::Input,
                fx: Fx::Deliver(owner, delivery),
            });
        }
        let page_active = host_update == HostUpdate::Live && rig.page_updates();
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
        // …and the drops, for the heartbeat's `dropped=`. The field on `self` accumulates until
        // the heartbeat drains it (`take_heartbeat_counters`), and it is folded HERE, from the
        // report, rather than beside each `report.dropped_deliveries += 1`: there are eight of
        // those sites and only one of them ever incremented both, so `Dispatcher::dropped_deliveries`
        // was a count of undeliverable ADDRESSEES calling itself a count of dropped deliveries.
        self.dropped_deliveries = self
            .dropped_deliveries
            .saturating_add(report.dropped_deliveries);
        // a body unmounted without ever drawing owes no `coldopen` line (§8.4)
        for id in &report.unmounted {
            self.cold.unmounted(id.0);
        }
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
        if will_present {
            self.prepare_pass(rig, tick);
        }
        rig.opaque_route(self.present.video_plane());
        #[cfg(debug_assertions)]
        debug_assert_eq!(
            self.state_hash(),
            hash_before_prepare,
            "a prepare pass changed the logical state"
        );

        // 10. draw, then the tail
        if will_present && draw {
            self.draw_pass(rig, tick, &mut report);
        }
        report.presented = will_present;
        tap.frame_done(f);
        report
    }

    /// Step 9's prepare pass: the top page unless its host fold is Cached or Replaced, then
    /// every surface, then the rig's own render cache — inside the budget's frame.
    fn prepare_pass(&mut self, rig: &mut dyn Rig<H>, tick: Tick) {
        self.prepared = true;
        let (_, host_render) = self.nav.modals.host_policy();
        // The budget's frame opens at the START of the prepare window and nowhere else (§8.1):
        // the ceiling measures how long THIS window has run, so an origin taken any earlier
        // charges the window for phases that are not its own.
        //
        // **While the legacy loop still owns the product's frame there are two such windows in
        // one iteration**, sharing the one budget: this pass (the owned screens', which spends
        // nothing today) and the loop's own around the render cache's upload step, opened later
        // and therefore the origin the uploads are admitted against. Two openings cost the
        // instruments nothing — `take_frame_stats` accumulates until its reader drains it, and
        // `begin_frame` resets only the quotas and the solo latch — and they collapse to one
        // when the dispatcher owns the whole frame.
        self.budget.begin_frame(rig.now_us());
        // The cold-open line's `prepared=` term (§8.4): the budget's refusal count as this
        // frame's prepare window opens, differenced at the draw. Taken here rather than from
        // `take_frame_stats` because that drain belongs to the heartbeat, once a second.
        self.cold.note_prepare(self.budget.refused());
        let parts = self.parts(tick);
        {
            let Dispatcher { nav, input, budget, .. } = self;
            let Split { views, measure, .. } = rig.split();
            if host_render == HostRender::Live {
                if let Some(e) = nav.tabs.stack.top_mut() {
                    let mut page_cx = parts.cx::<H>(views, measure);
                    page_cx.owner = InputOwner::Entry(e.id);
                    page_cx.focus = input.engine.read(page_cx.owner);
                    if let Some(inst) = e.inst.as_mut() { inst.screen.prepare(budget, &page_cx); }
                }
            }
            for s in &mut nav.modals.surfaces {
                if let Some(inst) = s.entry.inst.as_mut() {
                    let mut surface_cx = parts.cx::<H>(views, measure);
                    surface_cx.owner = InputOwner::Entry(s.entry.id);
                    surface_cx.focus = input.engine.read(surface_cx.owner);
                    inst.screen.prepare(budget, &surface_cx);
                }
            }
        }
        let Dispatcher {
            budget, present, ..
        } = self;
        rig.prepare(budget, present);
    }

    /// **Resolve every visible surface's REFRESHING backdrop, before the host page draws.**
    ///
    /// A second, narrower prepare, called from the loop's glass-owner block rather than from the
    /// dispatcher's own frame, because the slot is load-bearing at both ends and neither boundary
    /// exists inside `prepare_pass`: `popover::host::begin_frame` above it latches the own-damage
    /// ledger this fold reads, and `gfx::blur_direct_region()` is sampled below it, so an
    /// invalidation raised here reaches this frame's own blur source instead of the next one's.
    ///
    /// The TOP PAGE and every visible surface are asked, in that order. What the container
    /// contributes is the half a screen cannot know: whether its own appear spring has SETTLED,
    /// which it owns (`Surface::motion`) — a page, having no such spring, is asked with `true`.
    /// The decision itself stays one function, `popover::glass_refresh`, in the screen that owns
    /// the glass policy.
    ///
    /// It replaces a call that NAMED one screen (`ui::person_bio::prepare_present`, routed on
    /// `Route::Person` for a while, then self-gated) — the shape §14 is about: a rule stated in one
    /// module and enforced by a route test three modules away. A screen with a cached ground, or
    /// none, inherits the no-op.
    pub fn prepare_present(
        &mut self,
        glass: &mut crate::ui::frame::glass::GlassPlan,
        underlay_changed: bool,
    ) {
        if let Some(inst) = self.nav.tabs.stack.top_mut().and_then(|e| e.inst.as_mut()) {
            inst.screen.prepare_present(glass, underlay_changed, true);
        }
        for s in &mut self.nav.modals.surfaces {
            if s.phase == crate::ui::containers::modal::Phase::Hidden {
                continue;
            }
            let Some(inst) = s.entry.inst.as_mut() else { continue };
            let settled = s.motion.settled();
            inst.screen.prepare_present(glass, underlay_changed, settled);
        }
    }

    /// Step 10 on a frame the CALLER presents (the legacy loop's own gate, phase 5b): the
    /// prepare pass first if this frame's steps did not run it, then the draw. The surfaces
    /// alone or the whole tree — `pages` says whether the page pass is drawn here too (the
    /// legacy loop draws its own pages and reserves this call for the dispatcher's surfaces
    /// and its OWNED pages).
    pub fn draw(&mut self, rig: &mut dyn Rig<H>, pages: bool) -> FrameReport {
        let tick = self.last_tick;
        if !self.prepared {
            self.prepare_pass(rig, tick);
        }
        let mut report = FrameReport::default();
        self.draw_with(rig, tick, &mut report, pages);
        report
    }

    fn draw_pass(&mut self, rig: &mut dyn Rig<H>, tick: Tick, report: &mut FrameReport) {
        self.draw_with(rig, tick, report, true);
    }

    fn draw_with(&mut self, rig: &mut dyn Rig<H>, tick: Tick, report: &mut FrameReport, pages: bool) {
        let strip_owner = self.owner_entry();
        let navigation = rig.navigation_presentation();
        let (_, host_render) = self.nav.modals.host_policy();
        // §9: armed for the LENGTH OF THE DRAW, and restored rather than cleared, exactly as the
        // page freeze is. Every framebuffer-sampling door is refused while it is up.
        let video_plane = self.video_plane_frame();
        let was_video_plane = crate::gfx::set_video_plane_frame(video_plane);
        rig.clear_opaque_region();
        let parts = self.parts(tick);
        let Dispatcher { nav, input, .. } = self;
        let mut stops = Vec::new();
        let mut set = RenderSet {
            // the shared poster/logo residency (ui/tex.rs) is the one pool rule (c) sums beside
            // the screens' own renders and the FrameCache
            extra_bytes: super::tex::resident_bytes(),
            ..Default::default()
        };
        // the page pass: the top page (and, under a push, the level beneath it), unless the
        // host fold REPLACED it
        if pages && host_render != HostRender::Replaced {
            // §9: `(…, Replaced)` on everything below a bound video plane — the level a push or
            // pop transition would otherwise draw beneath the top one. Drawing it would put a page
            // on the panel UNDER a hole, i.e. over the film.
            let draws_below = nav.tabs.stack.transition.draws_below() && !video_plane;
            let n = nav.tabs.stack.entries.len();
            let top_entry = nav.tabs.stack.top().map(|entry| entry.id);
            let from = if draws_below { n.saturating_sub(2) } else { n.saturating_sub(1) };
            for e in nav.tabs.stack.entries[from..].iter_mut() {
                if let Some(inst) = e.inst.as_mut() {
                    let Split { views, measure, .. } = rig.split();
                    let mut page_cx = parts.cx::<H>(views, measure);
                    page_cx.owner = InputOwner::Entry(e.id);
                    page_cx.focus = input.engine.read(page_cx.owner);
                    let mut f = DrawFrame::with_navigation(&page_cx, Painter::root(), navigation);
                    f.page_alpha *= nav.tabs.stack.transition.page_alpha();
                    inst.screen.draw(&mut f);
                    report.drawn.push(inst.id);
                    stops.extend(f.into_stops());
                    set.pages += 1;
                    set.bytes += inst.screen.render_report().bytes;
                    if top_entry == Some(e.id) && e.arg.chrome() == super::machine::Chrome::TabBar
                        && inst.screen.focus_source() == FocusSource::Engine {
                        let mut chrome_parts = parts.clone();
                        chrome_parts.owner = InputOwner::Entry(e.id);
                        chrome_parts.focus = input.engine.read(chrome_parts.owner);
                        drop(page_cx);
                        rig.draw_chrome(&e.arg, &chrome_parts, navigation);
                    }
                }
            }
            // The strip is the container's control, not a page-local pointer ladder. Publish
            // its stops above the page and below surfaces, from the same keyed geometry the
            // engine queries. A folded page disables hover seating, but its visible controls
            // retain direct clicks (the legacy Home bar allowed clicks from the grid too).
            if let Some(entry) = nav.tabs.stack.top() {
                if strip_owner == Some(entry.id) && entry.inst.as_ref().is_some_and(|i| i.screen.hit_source() == HitSource::Engine) {
                    let hover = if entry.inst.as_ref().is_some_and(|i| i.screen.strip_reachable()) {
                        super::screen::Hover::Focus
                    } else { super::screen::Hover::Ignore };
                    stops.extend(nav.tabs.strip.iter().map(|member| Stop {
                        key: FocusKey { entry: entry.id, elem: member.elem },
                        rect: member.drawn, rest_rect: member.target, clip: member.clip,
                        hover, activate: Activate::Direct,
                    }));
                }
            }
            // **The surfaces' modal dim, last thing in the page pass** (§6.2, §8.3). It is here
            // and not with each panel because the dim has to be on the framebuffer BEFORE the
            // host snapshot is taken — see `ModalStack::draw_scrims`, which owns the rule and the
            // one style it does not apply to. `surface_scope` is the freeze lift the paint needs:
            // a page served from its cached quad refuses every fill, and this is the frame's first
            // `popover::host::live()`, which is also what defines the snapshot as the UNDIMMED
            // page.
            {
                let chip_read = rig.scrim_chip_read();
                let _scope = rig.surface_scope();
                nav.modals.draw_scrims(navigation.page_alpha, chip_read);
            }
        }
        if host_render == HostRender::Cached {
            set.frame_cache_bytes = super::frame::FRAME_CACHE_BYTES;
        }
        // the surfaces, bottom to top; a later stop is above an earlier one
        for s in &mut nav.modals.surfaces {
            if let Some(inst) = s.entry.inst.as_mut() {
                let _surface_scope = rig.surface_scope();
                // §4.4: a spring stepped while a SURFACE draws is the panel's, and an
                // `idle::invalidate` it raises is the panel's damage — the same attribution the
                // step above gets. The page pass runs in NO scope, so the page's own motion and
                // damage stay the page's and `popover::host_refresh` can still see them.
                let _own = crate::ui::popover::own_motion();
                let Split { views, measure, .. } = rig.split();
                let mut surface_cx = parts.cx::<H>(views, measure);
                surface_cx.owner = InputOwner::Entry(s.entry.id);
                surface_cx.focus = input.engine.read(surface_cx.owner);
                let mut f = DrawFrame::with_navigation(&surface_cx, Painter::root(), navigation);
                f.page_alpha = s.motion.appear;
                inst.screen.draw(&mut f);
                report.drawn.push(inst.id);
                stops.extend(f.into_stops());
                // (b) is a count of THIS SURFACE's own backing renders, asked of the surface —
                // not a literal `1`, which is what made "more than one render per surface"
                // unspellable and the rule inert. A surface served from the shared FrameCache
                // owns none and reports 0.
                let render = inst.screen.render_report();
                set.surfaces.push((s.entry.id, render.textures));
                set.bytes += render.bytes;
                // an Opaque surface's ground has drawn: the fold REPLACES the host from here
                s.ground_ready = inst.screen.ground_ready();
            }
        }
        // the hit map swaps only on a presented frame (§7.6); a legacy page registers nothing
        let hit_page = self.hit_page();
        crate::gfx::set_video_plane_frame(was_video_plane);
        if !crate::gfx::blur_source_pass() {
            self.input.hit.fill(if hit_page { stops } else { Vec::new() });
            self.input.hit.swap();
        }
        if let Err(breach) = set.check() {
            // The policy itself is `frame::on_breach` — assert on the host, log once on a
            // television — so that both halves are reachable from a test.
            if let Some(line) = super::frame::on_breach(&breach, &mut self.render_breach_logged) {
                rig.log(&line);
            }
        }
        report.render_set = set;
        // §8.4: a mount's clock stops on the first frame the body both prepared and DREW. This
        // frame prepared (`prepare_pass` ran above, or in the `frame_with` that preceded this
        // `draw`), so every id in `drawn` that is still pending closes here.
        let refused_now = self.budget.refused();
        for id in &report.drawn {
            self.cold.drawn(id.0, tick.ms, refused_now);
        }
        if let Some(fault) = self.present.take_fault() {
            rig.log(&format!("dispatch: fault {fault:?}"));
        }
    }

    /// The logical-state hash (spec §5.4): every live instance's `LogicalState`, the tree's
    /// shape and surface phases, the focus, the present gate's video-plane bit, queue depth
    /// and queued press/input/keyboard-request payloads, in a fixed order. Incremental hashing is the optimisation
    /// the spec names; this is the definition it must equal.
    pub fn state_hash(&self) -> u64 {
        let mut c = super::machine::Canon::new();
        self.nav.write(&mut c);
        self.input.write_with(&mut c, &|k, c| {
            c.u32(k.index().unwrap_or(u32::MAX));
        });
        c.bool(self.present.video_plane());
        c.u32(self.queue.len() as u32);
        // Press envelopes outlive their retired arm. Include their identity and FIFO position;
        // equal queue depth alone must not equate two different pending activations.
        for queued in &self.queue {
            if let Fx::Deliver(to, Delivery::Press { id, key, held }) = &queued.fx {
                c.bool(true);
                queued.from.write_canon(&mut c);
                to.write_canon(&mut c);
                c.u32(id.0).u32(key.entry.0).u32(key.elem.index().unwrap_or(u32::MAX)).bool(*held);
            } else { c.bool(false); }
            if let Fx::Deliver(to, Delivery::Screen(ScreenEvent::Input(input))) = &queued.fx {
                c.bool(true);
                queued.from.write_canon(&mut c);
                to.write_canon(&mut c);
                input.write_with(&mut c, &|elem, c| { c.u32(elem.index().unwrap_or(u32::MAX)); });
            } else { c.bool(false); }
            if let Fx::Deliver(to, Delivery::Keyboard { up }) = &queued.fx {
                c.bool(true);
                queued.from.write_canon(&mut c);
                to.write_canon(&mut c);
                c.bool(*up);
            } else { c.bool(false); }
        }
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
                Fx::Nav(_) | Fx::Mount(_) | Fx::Unmount(_) => self.park(item),
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
                    if !self.input.keyboard { self.input.arm(arm, item.from, parts.tick.ms); }
                }
                Fx::Remember { group, elem } => {
                    steps += 1;
                    let entry = match item.from {
                        MachineId::Instance(id) => self.nav.entry_of_instance(id),
                        _ => None,
                    };
                    // A covered top page still processes store/query changes. It may update
                    // its own remembered seat without moving either scope's current focus.
                    // Buried/retired pages are excluded, including late effects after Unmount.
                    let valid = entry.filter(|entry| self.owner_entry() == Some(*entry)
                        || self.nav.top_page().is_some_and(|page| page.id == *entry)).filter(|entry| {
                        let Some(view) = Self::owner_view(&self.nav, *entry) else { return false };
                        let Split { views, measure, .. } = rig.split();
                        let mut cx = parts.cx::<H>(views, measure);
                        cx.owner = InputOwner::Entry(*entry);
                        cx.focus = self.input.engine.read(cx.owner);
                        view.group_of(&elem, &cx) == Some(group)
                            && view.place(&elem, &cx, At::SpringTarget).is_some()
                    });
                    if let Some(entry) = valid {
                        self.input.engine.remember_projected(entry, group, elem);
                    } else {
                        report.dropped_deliveries += 1;
                    }
                }
                Fx::Log(line) => rig.log(&line.0),
                Fx::App(app_fx) => {
                    steps += 1;
                    let captured = self.app_returns.iter().position(|(from, _)| *from == item.from)
                        .and_then(|i| self.app_returns.remove(i)).map(|(_, ret)| ret)
                        .unwrap_or_else(|| self.return_state());
                    rig.app_return(item.from, captured);
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
    /// op — `a_key_that_opens_a_page_mounts_in_the_same_frame`). Immediate input operations
    /// precede the next input: releases see their arms, and text sees its field/keyboard state.
    fn absorb(&mut self, out: Vec<Stamped<H>>) {
        let mut immediate_input = Vec::new();
        for s in out {
            match s.fx {
                Fx::Nav(_) | Fx::Mount(_) | Fx::Unmount(_) => self.park(s),
                // An OK-up already queued behind its OK-down must see this arm. Keeping it
                // at the FIFO tail loses same-frame releases (remote_synth_key emits both).
                Fx::Press(_) | Fx::Deliver(_, Delivery::Keyboard { .. }) => immediate_input.push(s),
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Activate(_))) if s.from == MachineId::Input => immediate_input.push(s),
                Fx::App(_) => {
                    self.app_returns.push_back((s.from, self.return_state()));
                    self.queue.push_back(s);
                }
                _ => self.queue.push_back(s),
            }
        }
        for arm in immediate_input.into_iter().rev() {
            self.queue.push_front(arm);
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
            (MachineId::Instance(instance), Delivery::Keyboard { up }) => {
                let active = self.owner_entry().and_then(|entry| self.nav.instance_of(entry)) == Some(instance);
                let accepted = if up { active } else {
                    self.input.keyboard_owner == Some(instance) || (self.input.keyboard_owner.is_none() && active)
                };
                if !accepted { report.dropped_deliveries += 1; return; }
                let owner = up.then_some(instance);
                if self.input.keyboard != up || self.input.keyboard_owner != owner {
                    self.input.keyboard = up;
                    self.input.keyboard_owner = owner;
                    self.cancel_keyboard_gestures(report);
                }
                rig.system_keyboard(up);
            }
            (MachineId::Instance(instance), Delivery::Press { id, key, held }) => {
                let valid_owner = !self.input.keyboard && self.owner_entry() == Some(key.entry)
                    && self.nav.instance_of(key.entry) == Some(instance)
                    && self.focus() == Some(key);
                let valid_key = valid_owner && {
                    let view = Self::owner_view(&self.nav, key.entry);
                    let Split { views, measure, .. } = rig.split();
                    let mut cx = parts.cx::<H>(views, measure);
                    cx.focus.current = self.focus();
                    cx.owner = InputOwner::Entry(key.entry);
                    view.is_some_and(|view| view.reconcile(key, &cx) == key
                        && view.place(&key.elem, &cx, At::Drawn).is_some())
                };
                if !valid_key {
                    report.dropped_deliveries += 1;
                    return;
                }
                let event = if held { ScreenEvent::PressHold(id) } else { ScreenEvent::PressCommit(id) };
                self.execute_deliver(rig, parts, to, Delivery::Screen(event), out, report);
            }
            (MachineId::Instance(id), Delivery::Screen(ev)) => {
                // An OS ownership edge takes effect at its position in the delivery stream.
                // It is global even if its original recipient retired while the event waited.
                if let ScreenEvent::Input(InputEvent {
                    kind: InputKind::SystemKeyboard(up), ..
                }) = &ev {
                    if *up { rig.adopt_system_keyboard(); }
                    else { rig.system_keyboard(false); }
                    let owner = up.then_some(id);
                    if self.input.keyboard != *up || self.input.keyboard_owner != owner {
                        self.input.keyboard = *up;
                        self.input.keyboard_owner = owner;
                        self.cancel_keyboard_gestures(report);
                    }
                }
                // A carried event can outlive its input owner. Reject it before the handler
                // can write stores or request playback; a later navigation-effect guard is
                // too late. Addressed commands, notices and lifecycle restoration still reach
                // covered entries (including the host beneath a restored modal).
                if matches!(ev, ScreenEvent::Input(_) | ScreenEvent::Activate(_) |
                    ScreenEvent::PressHold(_) | ScreenEvent::PressCommit(_))
                    && self.owner_entry().and_then(|entry| self.nav.instance_of(entry)) != Some(id)
                {
                    report.dropped_deliveries += 1;
                    return;
                }
                // Ingest releases an already-live gesture before the press Tick. A Down in
                // THIS input batch creates its arm later, while draining, so apply subsequent
                // physical edges in delivery order too. These operations are idempotent for
                // an arm ingest already saw, and never act on another instance's gesture.
                if self.input.arm.is_some_and(|arm| arm.owner == MachineId::Instance(id)) {
                    if let ScreenEvent::Input(InputEvent {
                        kind: InputKind::Key { key: Key::Ok, edge, .. }, ..
                    }) = &ev {
                        match edge {
                            Edge::Up => self.input.release(parts.tick.ms),
                            Edge::Repeat => self.input.note_alive(parts.tick.ms),
                            Edge::Down => {}
                        }
                    }
                }
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
                let mut addressed = parts.clone();
                if let Some(entry) = self.nav.entry_of_instance(id) {
                    // Focus still belongs to the underlying entry while the keyboard owns
                    // input. Covered lifecycle recipients retain their own entry context.
                    let entry_owner = InputOwner::Entry(entry);
                    addressed.owner = if self.owner_entry() == Some(entry) { self.owner() } else { entry_owner };
                    addressed.focus = self.input.engine.read(entry_owner);
                }
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
                // …and the `ui::idle` half of the same attribution, which is the one a screen's
                // OWN springs reach: an owned screen animates through `gfx::spring`, which reports
                // to `ui::idle` and not to this gate (the Library notes no `Motion` at all). Held
                // for the body — a guard, so the early return below cannot leak the scope.
                let _own = is_surface.then(crate::ui::popover::own_motion);
                let Some(inst) = nav.instance_mut(id) else {
                    present.set_scope(super::present::Scope::Page);
                    report.dropped_deliveries += 1;
                    return;
                };
                let Split { views, measure, .. } = rig.split();
                let cx = addressed.cx::<H>(views, measure);
                let mut fx = Effects::new(out, to, present);
                let handled = inst.screen.step(&ev, &cx, &mut fx);
                drop(fx);
                drop(cx);
                present.set_scope(super::present::Scope::Page);
                // §7.3 step 1: the owner had first refusal; an unhandled BACK is the container's.
                // This is the ONE place a BACK becomes `pending_back`, and it is reached by two
                // roads: the physical key, and an `EdgeRule::Nav(NavOpKind::Back)` that
                // `after_step` re-delivered as a synthetic one (see that arm). The detection is
                // deliberately blind to `at_edge` and to the wcode, so the two roads cannot
                // diverge.
                let is_owner = matches!(addressed.owner, InputOwner::Entry(e) if nav.instance_of(e) == Some(id));
                if back && handled == Handled::No && is_owner {
                    self.pending_back = true;
                }
                // the engine's half: after the owner's refusal, and after an Enter / a hold
                self.after_step(rig, &addressed, id, &ev, handled, out);
                // WillLeave and Unmount are queued after structural commit. Keep the engine's
                // read snapshot available until the retiring body's final step has consumed it.
                if matches!(ev, ScreenEvent::Unmount) {
                    report.unmounted.push(id);
                    if let Some(entry) = self.nav.entry_of_instance(id)
                        .filter(|entry| self.nav.entry(*entry).is_some_and(|page| !page.evicted)) {
                        self.input.engine.forget(entry);
                    }
                }
            }
            (MachineId::Instance(_), Delivery::Machine(_)) => {
                report.dropped_deliveries += 1;
            }
            (other, Delivery::Machine(msg)) => {
                let Dispatcher { present, .. } = self;
                let mut fx = Effects::new(out, other, present);
                let _ = rig.deliver(other, &msg, parts, &mut fx);
            }
            (_, Delivery::Screen(_) | Delivery::Press { .. } | Delivery::Keyboard { .. }) => {
                report.dropped_deliveries += 1;
            }
        }
    }

    fn cancel_keyboard_gestures(&mut self, report: &mut FrameReport) {
        self.input.cancel_press();
        // Tick may have queued a result behind this ownership edge. Returning to the same
        // page restores its input scope, not the revoked gesture.
        let before = self.queue.len();
        self.queue.retain(|s| !matches!(&s.fx, Fx::Deliver(_, Delivery::Press { .. })));
        report.dropped_deliveries += (before - self.queue.len()) as u32;
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
                if matches!(e, Enter::Restored) {
                    if let Some(saved) = self.nav.entry(entry) {
                        self.input.engine.restore_remembered(entry, &saved.ret.remembered);
                    }
                }
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
                                let mut again = iev.clone();
                                if let InputKind::Key { at_edge, .. } = &mut again.kind {
                                    *at_edge = true;
                                }
                                out.push(Stamped {
                                    from: MachineId::Input,
                                    fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::Input(again))),
                                });
                            }
                        }
                        // An edge-rule BACK is a BACK KEY, not a container op — so it takes the
                        // road a real BACK press takes and the OWNER gets first refusal. It is
                        // re-delivered as a synthetic `Key::Back` at `Edge::Down`, and only
                        // `execute_deliver`'s existing unhandled-BACK arm — the same one a
                        // physical BACK goes through — turns a refusal into `pending_back`.
                        //
                        // It used to set `pending_back` here, which skipped the owner entirely,
                        // and `Navigation::back` answers `Dismissed` for ANY surface owner
                        // whatever its own depth. So LEFT inside the Settings family dismissed
                        // the WHOLE surface where a BACK press walked its inner stack: LEFT in a
                        // legal document left Settings instead of returning to the index
                        // (`ui/legal.rs`'s
                        // `right_enters_a_document_and_left_walks_all_the_way_back_out` is the
                        // legacy behaviour it broke; that module and its test went out of the tree
                        // with phase 5b, and `screens/settings.rs`'s composed LEFT-road tests are
                        // what execute the same property now — do not go looking for the old name).
                        // Every other owner is unchanged, because a
                        // screen that does not answer a BACK still ends at `pending_back`.
                        //
                        // Three details are load-bearing:
                        //  * `Edge::Down`, never the incoming edge. A held LEFT arrives as
                        //    `Edge::Repeat`, and both a screen's BACK arm and the unhandled-BACK
                        //    detection match the DOWN edge only — forwarding a Repeat would be
                        //    delivered to nobody and silently swallow the press.
                        //  * `at_edge: true`, which is what this flag is for ("re-delivered to
                        //    the owner's step"). It cannot loop: `after_step`'s only `at_edge`
                        //    consumer is the direction branch below, and `Key::Back` has no
                        //    direction, so an unhandled synthetic BACK reaches `pending_back`
                        //    and stops there. Nothing re-enters the engine.
                        //  * `at`/`source` are the originating press's, so a recording replays
                        //    the same provenance; `sym`/`wcode` are 0 because no hardware key
                        //    produced this event and inventing a code would let a screen match
                        //    on one that never came off the remote.
                        Outcome::Edge(EdgeRule::Nav(super::machine::NavOpKind::Back)) => {
                            if let ScreenEvent::Input(iev) = ev {
                                out.push(Stamped {
                                    from: MachineId::Input,
                                    fx: Fx::Deliver(
                                        MachineId::Instance(id),
                                        Delivery::Screen(ScreenEvent::Input(InputEvent {
                                            at: iev.at,
                                            source: iev.source,
                                            kind: InputKind::Key {
                                                key: Key::Back,
                                                sym: 0,
                                                wcode: 0,
                                                edge: Edge::Down,
                                                at_edge: true,
                                            },
                                        })),
                                    ),
                                });
                            }
                        }
                        Outcome::Edge(EdgeRule::Nav(super::machine::NavOpKind::Dismiss)) => {
                            self.park(Stamped {
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
            if self.input.arm.is_some_and(|arm| arm.key.entry == entry && arm.key != to) {
                // A catalog reconciliation changes the cursor, not the identity of an ongoing
                // gesture. Never let release activate the fallback item that replaced its arm.
                self.input.cancel_press();
            }
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
        for (item, ret) in parked {
            match item.fx {
                Fx::Nav(op) => {
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
                Life::Unmount(eid) => self.unmount(eid, false, &mut post),
                Life::Evict(eid) => self.unmount(eid, true, &mut post),
            }
        }
        // ahead of the carried queue: the mount that a key asked for happens THIS frame
        for s in post.into_iter().rev() {
            self.queue.push_front(s);
        }
    }

    fn push_lifecycle(&mut self, eid: EntryId, ev: ScreenEvent<H>, post: &mut Vec<Stamped<H>>) {
        if let Some(id) = self.nav.instance_of(eid) {
            if matches!(ev, ScreenEvent::Enter(Enter::Restored)) {
                let memory = self.nav.entry(eid).expect("live entry").ret.memory.clone();
                post.push(Stamped {
                    from: MachineId::Nav,
                    fx: Fx::Deliver(MachineId::Instance(id), Delivery::Screen(ScreenEvent::RestoreMemory(memory))),
                });
            }
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
            let Dispatcher { nav, present, input, .. } = self;
            let entry = nav.entry(eid).expect("checked above");
            let Split {
                mounter,
                views,
                measure,
            } = rig.split();
            // the body being mounted is the owner its `mount` reads (`cx.owner`)
            let mut p = parts.clone();
            p.owner = InputOwner::Entry(eid);
            p.focus = input.engine.read(p.owner);
            let cx = p.cx::<H>(views, measure);
            let mut fx = Effects::new(&mut out, MachineId::Instance(id), present);
            mounter.mount(id, &entry.arg, &entry.ret, &cx, &mut fx)
        };
        // read before the body moves into the tree — the cold-open clock's start (§8.4)
        let name = screen.name();
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
        report.mounted.push((id, name));
        self.cold.mounted(id.0, name, parts.tick.ms);
    }

    /// Register a request as in flight for an instance (the app's registry calls this when it
    /// mints an `Addr` for a screen's request).
    pub fn track_inflight(&mut self, inst: InstanceId, req: RequestId) {
        if let Some(i) = self.nav.instance_mut(inst) {
            i.inflight.push(req);
        }
    }

    fn unmount(&mut self, eid: EntryId, evicted: bool, post: &mut Vec<Stamped<H>>) {
        let Some(inst) = self.nav.entry_mut(eid).and_then(|e| e.inst.take()) else {
            // No body remains to receive lifecycle events; a final retirement can forget now.
            if !evicted { self.input.engine.forget(eid); }
            return;
        };
        // Inflight is cleared below at structural commit; pruning waits for actual Unmount
        // delivery, which may be carried beyond this frame's post-commit budget.
        // Eviction retains engine state; final retirement forgets after Unmount is delivered.
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

    /// The heartbeat's `carried=`/`dropped=` (§8.4), once a second: the queue depth the LAST
    /// frame of the second carried forward, and every delivery dropped since the previous read.
    /// `carried` is a level and is not reset; `dropped` is a count and is.
    pub fn take_heartbeat_counters(&mut self) -> (usize, u32) {
        (self.last_carried, std::mem::take(&mut self.dropped_deliveries))
    }

    /// The `coldopen` lines this frame closed (§8.4), for the loop's report block to log.
    pub fn take_cold_open_lines(&mut self) -> Vec<String> {
        self.cold.take_lines()
    }

    pub fn viewport() -> Rect {
        Rect::FULL
    }
}

// ==============================================================================================
// §7.3 — an edge-rule BACK takes the BACK KEY's road
// ==============================================================================================

/// What `EdgeRule::Nav(NavOpKind::Back)` does when a direction runs off a group that declares it
/// — which is what LEFT resolves to on the Settings family's table and document groups, and on
/// anything else that follows its crumb leftwards.
///
/// It cannot be graded through `ui::fixture`'s screens: every group there declares
/// `EdgeRule::Geometric` on all four sides, so no key any fixture screen can be sent ever reaches
/// the arm under test. The bundle is still `FixtureHost` — the `Arg`, the views and the measure
/// are the fixture's — but this module brings its own `Mounter` (a `Rig` is the only way to
/// choose one) and one screen with a `Nav(Back)` rule on its LEFT edge, carrying an inner DEPTH
/// it walks down exactly as `screens::settings::RouteSurface` walks its own stack: it HANDLES a
/// BACK while it has depth and DECLINES one at its root.
/// The cold-open instrument's dispatcher half (spec §8.4): the mount that starts the clock and
/// the first prepared+drawn frame that stops it. The arithmetic and the line's shape are pinned in
/// `diag::heartbeat`; what is pinned HERE is that the two events are wired to the right places and
/// that the word on the line is the screen's own `Screen::name()`.
#[cfg(test)]
mod cold_open_tests {
    use super::*;
    use crate::ui::fixture::{tick, FixtureArg, FixtureHost, FixtureRig};
    use crate::ui::machine::MachineId;

    #[test]
    fn one_mount_produces_exactly_one_cold_open_line_naming_that_screen() {
        let _g = crate::testlock::serial();
        let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
        let mut rig = FixtureRig::new();

        // boot: Home is requested, mounts at nav commit and draws in the same frame
        d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
        let r = d.frame(&mut rig, tick(0), vec![], vec![], &mut NoTap);
        assert_eq!(r.mounted.len(), 1);
        assert_eq!(r.mounted[0].1, "home", "the report carries the screen's own name");
        assert!(r.drawn.contains(&r.mounted[0].0), "it drew in the frame it mounted in");
        assert_eq!(
            d.take_cold_open_lines(),
            vec!["coldopen screen=home ms=0 prepared=true".to_string()],
        );

        // …and never again for that instance, however many frames it goes on drawing for
        d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
        d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
        assert!(d.take_cold_open_lines().is_empty(), "one line per MOUNT, not per frame");

        // A second screen mounted later reports its own word — and its own CLOCK, which is the
        // half `ms=0` above cannot show. This mount lands on a frame the present gate refuses
        // (a structural op that damaged nothing), so the body has not drawn yet and owes no line;
        // the next frame that presents is the one it opens on, 16 ms later.
        d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(1)));
        let r2 = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
        assert_eq!(r2.mounted.len(), 1);
        assert!(!r2.presented && r2.drawn.is_empty(), "the mount frame drew nothing");
        assert!(d.take_cold_open_lines().is_empty(), "a mount that has not drawn owes no line");
        d.present.note(crate::ui::present::PresentEvent::Damage(
            crate::ui::present::Provenance::Input,
        ));
        let r3 = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
        assert!(r3.presented, "damage opened the gate");
        assert_eq!(
            d.take_cold_open_lines(),
            vec!["coldopen screen=detail ms=16 prepared=true".to_string()],
        );
    }
}

#[cfg(test)]
mod edge_back_tests {
    use std::borrow::Cow;

    use super::*;
    use crate::ui::containers::modal::{Phase, Style};
    use crate::ui::fixture::{key, tick, FixtureArg, FixtureFx, FixtureHost, FixtureMeasure, FixtureMsg, FixtureView, FixtureViews};
    use crate::ui::machine::{Canon, LogicalState, Machine, NavOpKind};
    use crate::ui::screen::{AxisMask, GroupKind, RenderStrategy, Seat};

    /// The one group the test screen contributes.
    const G: GroupId = GroupId(11);
    /// `FixtureArg::Page(DEPTH_BASE + n)` mounts a screen with an inner depth of `n`.
    const DEPTH_BASE: u32 = 900;

    #[derive(Default)]
    struct EdgeBackState {
        read_memory: Option<u32>,
        /// The screen's own inner stack depth; 1 is its root, where BACK is declined.
        depth: usize,
        /// Every `Key::Back` at `Edge::Down` this screen was stepped with…
        backs: u32,
        /// …and how many of those carried `at_edge: true`, i.e. came from the engine's edge rule
        /// rather than off the remote.
        at_edge_backs: u32,
    }

    impl LogicalState for EdgeBackState {
        fn write(&self, w: &mut Canon) {
            w.u32(self.depth as u32).u32(self.backs).u32(self.at_edge_backs);
            w.option(self.read_memory, |w, value| { w.u32(value); });
        }
        fn probe(&self, out: &mut String) {
            out.push_str(&format!(
                "depth={} backs={} at_edge_backs={}",
                self.depth, self.backs, self.at_edge_backs
            ));
            out.push_str(&format!(" read_memory={:?}", self.read_memory));
        }
    }

    struct EdgeBackScreen {
        entry: EntryId,
        state: EdgeBackState,
    }

    const EXTENT: Rect = Rect::new(600.0, 200.0, 720.0, 600.0);

    impl Focusable<FixtureHost> for EdgeBackScreen {
        fn groups(&self, _cx: &Cx<'_, FixtureHost>, out: &mut Vec<GroupSpec>) {
            out.push(GroupSpec {
                id: G,
                kind: GroupKind::Column,
                seat: Seat::First,
                reachable: AxisMask::BOTH,
                // `[up, down, left, right]` — LEFT follows the crumb, which is the rule under test.
                edge: [
                    EdgeRule::Geometric,
                    EdgeRule::Geometric,
                    EdgeRule::Nav(NavOpKind::Back),
                    EdgeRule::Geometric,
                ],
                extent: EXTENT,
                len: 1,
                elem: ElemKind::Control,
            });
        }
        fn group_of(&self, key: &u32, _cx: &Cx<'_, FixtureHost>) -> Option<GroupId> {
            (*key == 0).then_some(G)
        }
        fn neighbour(&self, _key: FocusKey<u32>, _dir: Dir, _cx: &Cx<'_, FixtureHost>) -> Step<u32> {
            Step::Edge // one element: every direction runs off the group at once
        }
        fn place(&self, key: &u32, _cx: &Cx<'_, FixtureHost>, _at: At) -> Option<Placed> {
            (*key == 0).then_some(Placed {
                rect: EXTENT,
                rest_rect: EXTENT,
                clip: Rect::FULL,
                index: Some(0),
            })
        }
        fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            want
        }
        fn seat(&self, _g: GroupId, _from: Placed, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            FocusKey {
                entry: self.entry,
                elem: 0,
            }
        }
    }

    impl Machine<FixtureHost> for EdgeBackScreen {
        type Ev = ScreenEvent<FixtureHost>;
        fn step(
            &mut self,
            ev: &Self::Ev,
            cx: &Cx<'_, FixtureHost>,
            fx: &mut Effects<'_, FixtureHost>,
        ) -> Handled {
            if matches!(ev, ScreenEvent::Unmount) {
                fx.push(Fx::App(FixtureFx::StoreAdd(self.entry.0)));
            }
            if matches!(ev, ScreenEvent::WillLeave(_)) {
                assert_eq!(cx.owner, InputOwner::Entry(self.entry));
                self.state.read_memory = cx.focus.remembered(GroupId(901));
            }
            match ev {
                ScreenEvent::Input(InputEvent {
                    kind: InputKind::Key {
                        key: Key::Back,
                        edge: Edge::Down,
                        at_edge,
                        ..
                    },
                    ..
                }) => {
                    self.state.backs += 1;
                    if *at_edge {
                        self.state.at_edge_backs += 1;
                    }
                    if self.state.depth > 1 {
                        self.state.depth -= 1;
                        Handled::Yes
                    } else {
                        Handled::No // its own root: the container decides what BACK means
                    }
                }
                _ => Handled::No,
            }
        }
    }

    impl Screen<FixtureHost> for EdgeBackScreen {
        fn name(&self) -> &'static str {
            "edgeback"
        }
        fn state(&self) -> &dyn LogicalState {
            &self.state
        }
        fn crumb(&self, _cx: &Cx<'_, FixtureHost>) -> Option<Cow<'_, str>> {
            None
        }
        fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, FixtureHost>) {}
        fn draw(&mut self, _f: &mut DrawFrame<'_, '_, FixtureHost>) {}
        fn render(&self) -> RenderStrategy {
            RenderStrategy::Page
        }
    }

    struct EdgeBackMounter;

    impl Mounter<FixtureHost> for EdgeBackMounter {
        fn mount(
            &mut self,
            _id: InstanceId,
            arg: &FixtureArg,
            _ret: &ReturnState<u32>,
            cx: &Cx<'_, FixtureHost>,
            _fx: &mut Effects<'_, FixtureHost>,
        ) -> Box<dyn Screen<FixtureHost>> {
            // `mount` is handed the mounting body's OWN entry as `cx.owner` (see `Dispatcher::mount`)
            let entry = match cx.owner {
                InputOwner::Entry(e) => e,
                _ => EntryId(0),
            };
            let depth = match arg {
                FixtureArg::Page(n) if *n >= DEPTH_BASE => (*n - DEPTH_BASE) as usize,
                _ => 1,
            };
            Box::new(EdgeBackScreen {
                entry,
                state: EdgeBackState {
                    depth,
                    ..EdgeBackState::default()
                },
            })
        }
    }

    struct EdgeBackRig {
        mounter: EdgeBackMounter,
        view: FixtureView,
        measure: FixtureMeasure,
        /// How many times the application was told BACK reached the root of the root stack.
        roots: u32,
        unmounts: Vec<EntryId>,
    }

    impl EdgeBackRig {
        fn new() -> Self {
            Self {
                mounter: EdgeBackMounter,
                view: FixtureView::default(),
                measure: FixtureMeasure,
                roots: 0,
                unmounts: Vec::new(),
            }
        }
    }

    impl Rig<FixtureHost> for EdgeBackRig {
        fn split(&mut self) -> Split<'_, FixtureHost> {
            Split {
                mounter: &mut self.mounter,
                views: FixtureViews { store: &self.view },
                measure: &self.measure,
            }
        }
        fn deliver(
            &mut self,
            _to: MachineId,
            _msg: &FixtureMsg,
            _parts: &CxParts<u32>,
            _fx: &mut Effects<'_, FixtureHost>,
        ) -> Handled {
            Handled::No
        }
        fn timer(
            &mut self,
            _owner: MachineId,
            _id: TimerId,
            _parts: &CxParts<u32>,
            _fx: &mut Effects<'_, FixtureHost>,
        ) {
        }
        fn app_fx(
            &mut self,
            _from: MachineId,
            fx: FixtureFx,
            _parts: &CxParts<u32>,
            _out: &mut Effects<'_, FixtureHost>,
        ) {
            if let FixtureFx::StoreAdd(entry) = fx { self.unmounts.push(EntryId(entry)); }
        }
        fn log(&mut self, _line: &str) {}
        fn prepare(&mut self, _b: &mut Budget, _present: &mut Present) {}
        fn ls2_pump(&mut self) {}
        fn opaque_route(&mut self, _bound: bool) {}
        fn clear_opaque_region(&mut self) {}
        fn now_us(&self) -> u64 {
            0
        }
        fn back_at_root(&mut self) {
            self.roots += 1;
        }
    }

    fn booted() -> (Dispatcher<FixtureHost>, EdgeBackRig) {
        let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
        let mut rig = EdgeBackRig::new();
        d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
        d.frame(&mut rig, tick(0), vec![], vec![], &mut NoTap);
        (d, rig)
    }

    #[test]
    fn receiving_entries_keep_their_own_focus_read_through_cover_and_retirement() {
        let _guard = crate::testlock::serial();
        let (mut d, mut rig) = booted();
        d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
        let home = d.nav.top_page().unwrap().id;
        d.input.engine.remember_projected(home, GroupId(901), 17);
        let modal = open(&mut d, &mut rig, 2, 16);
        d.input.engine.remember_projected(modal, GroupId(901), 29);
        let id = d.nav.instance_of(home).unwrap();
        d.emit(MachineId::Nav, Fx::Deliver(MachineId::Instance(id),
            Delivery::Screen(ScreenEvent::WillLeave(super::super::machine::Leave::Deeper))));
        d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
        assert!(probe(&d, home).contains("read_memory=Some(17)"), "covered page must not read modal memory");
        d.request(MachineId::Nav, NavOp::Replace(FixtureArg::Page(1)));
        d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
        assert!(probe(&d, home).contains("read_memory=Some(17)"), "WillLeave reads memory before retirement forgets it");
        assert!(d.input.engine.remembered_snapshot(home).is_empty(), "retired entries are forgotten after their last step");
    }

    #[test]
    fn carried_unmounts_finish_before_production_pruning_forgets_retired_bodies() {
        let _guard = crate::testlock::serial();
        let (mut d, mut rig) = booted();
        d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
        let mut entries = vec![d.nav.top_page().unwrap().id];
        for i in 0..33 { entries.push(open(&mut d, &mut rig, 2, 16 * (i + 1))); }
        let mut requests = Vec::new();
        for &entry in &entries {
            d.input.engine.remember_projected(entry, GroupId(901), entry.0);
            let instance = d.nav.instance_of(entry).unwrap();
            d.track_inflight(instance, RequestId(9));
            requests.push(Addr { to: MachineId::Instance(instance), req: RequestId(9) });
        }
        d.request(MachineId::Nav, NavOp::Replace(FixtureArg::Page(1)));
        let report = d.frame(&mut rig, tick(560), vec![], vec![], &mut NoTap);
        assert!(report.carried > 0, "the fixture must exceed the post-commit lifecycle budget");
        assert!(requests.iter().all(|addr| !d.nav.is_deliverable(addr)), "inflight retirement is immediate even for carried Unmount");
        d.prune(&report.unmounted); // The application's frame tail, not a test-only delayed prune.
        for i in 0..8 {
            let report = d.frame(&mut rig, tick(576 + i * 16), vec![], vec![], &mut NoTap);
            d.prune(&report.unmounted);
            if d.queued() == 0 { break; }
        }
        let mut observed = rig.unmounts.clone();
        observed.sort_by_key(|entry| entry.0);
        entries.sort_by_key(|entry| entry.0);
        assert_eq!(observed, entries, "each retired body must execute Unmount exactly once");
        for entry in entries {
            assert!(d.nav.entry(entry).is_none());
            assert_eq!(d.input.engine.current(InputOwner::Entry(entry)), None);
            assert!(d.input.engine.remembered_snapshot(entry).is_empty());
        }
    }

    #[test]
    fn covered_top_page_can_refresh_its_own_group_memory_but_retired_page_cannot() {
        let _guard = crate::testlock::serial();
        let (mut d, mut rig) = booted();
        d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
        let home = d.nav.top_page().unwrap().id;
        let instance = d.nav.instance_of(home).unwrap();
        let modal = open(&mut d, &mut rig, 2, 16);
        let owner = InputOwner::Entry(modal);
        let current = d.input.engine.current(owner);
        let modal_memory = d.input.engine.remembered_snapshot(modal);
        d.input.engine.remember_projected(home, G, 99); // A former item in a replaced listing.
        d.emit(MachineId::Instance(instance), Fx::Remember { group: G, elem: 0 });
        let report = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
        assert_eq!(d.input.engine.read(InputOwner::Entry(home)).remembered(G), Some(0));
        assert_eq!(report.dropped_deliveries, 0);
        assert_eq!(d.input.engine.current(owner), current, "memory refresh never moves the input owner's focus");
        assert_eq!(&*d.input.engine.remembered_snapshot(modal), &*modal_memory);

        d.emit(MachineId::Instance(instance), Fx::Remember { group: GroupId(9000), elem: 0 });
        let report = d.frame(&mut rig, tick(40), vec![], vec![], &mut NoTap);
        assert!(report.dropped_deliveries > 0, "covered memory writes still validate group membership");
        assert_eq!(d.input.engine.read(InputOwner::Entry(home)).remembered(GroupId(9000)), None);

        d.request(MachineId::Nav, NavOp::Replace(FixtureArg::Page(1)));
        d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
        assert!(d.nav.entry(home).is_some(), "retired body remains until the frame-tail prune");
        d.emit(MachineId::Instance(instance), Fx::Remember { group: G, elem: 0 });
        let report = d.frame(&mut rig, tick(64), vec![], vec![], &mut NoTap);
        assert!(report.dropped_deliveries > 0);
        assert!(d.input.engine.remembered_snapshot(home).is_empty(), "late effects cannot resurrect retired focus memory");
    }

    /// Present an `Opaque` surface (the Settings family's style) whose screen starts at `depth`.
    fn open(d: &mut Dispatcher<FixtureHost>, rig: &mut EdgeBackRig, depth: u32, ms: u32) -> EntryId {
        d.nav.next_style = Style::Opaque { snapshot: true };
        d.request(MachineId::Nav, NavOp::Present(FixtureArg::Page(DEPTH_BASE + depth)));
        d.frame(rig, tick(ms), vec![], vec![], &mut NoTap);
        d.nav.modals.top().expect("presented").entry.id
    }

    fn probe(d: &Dispatcher<FixtureHost>, id: EntryId) -> String {
        let mut s = String::new();
        d.nav.entry(id).unwrap().inst.as_ref().unwrap().screen.state().probe(&mut s);
        s
    }

    /// Seat focus on the owner's one element, as the test's PREMISE. Without it the first
    /// direction would be spent by `move_dir`'s no-focus fallback (it seats and returns `Moved`),
    /// which never reaches an edge rule at all — so the assertion would be about seating rather
    /// than about the arm under test.
    fn seat(d: &mut Dispatcher<FixtureHost>, entry: EntryId) {
        d.set_focus(Some(FocusKey { entry, elem: 0 }));
    }

    /// **The blocker, first half.** LEFT off a group whose left edge rule is `Nav(Back)` reaches
    /// the owner's `step` as a synthetic BACK, and a screen that HANDLES it walks its own inner
    /// stack. The surface stays up and keeps input — where the engine's old shortcut
    /// (`pending_back` straight from the edge rule) sent the press to `Navigation::back`, which
    /// answers `Dismissed` for any surface owner whatever its depth.
    #[test]
    fn an_edge_rule_back_the_owner_handles_walks_its_stack_and_does_not_dismiss_it() {
        let (mut d, mut rig) = booted();
        let id = open(&mut d, &mut rig, 3, 16);
        seat(&mut d, id);
        d.frame(&mut rig, tick(32), vec![key(Key::Left, tick(32))], vec![], &mut NoTap);
        let p = probe(&d, id);
        assert!(p.contains("backs=1"), "the owner was stepped with the BACK: {p}");
        assert!(p.contains("at_edge_backs=1"), "…and it carried at_edge, as a re-delivery must: {p}");
        assert!(p.contains("depth=2"), "…and it walked ONE level of its own stack: {p}");
        assert_ne!(
            d.nav.modals.top().unwrap().phase,
            Phase::Closing,
            "a handled BACK is not the container's"
        );
        assert_eq!(d.nav.input_owner(), Some(InputOwner::Entry(id)), "the surface still owns input");

        // and a second LEFT walks the next one, still without dismissing
        d.frame(&mut rig, tick(48), vec![key(Key::Left, tick(48))], vec![], &mut NoTap);
        assert!(probe(&d, id).contains("depth=1"), "{}", probe(&d, id));
        assert_ne!(d.nav.modals.top().unwrap().phase, Phase::Closing);
    }

    /// **The blocker, second half** — and the half that must NOT change. At its own root the
    /// screen declines the synthetic BACK, and the unhandled-BACK path the physical key already
    /// used dismisses the surface, on the same frame's NAV COMMIT.
    #[test]
    fn an_edge_rule_back_the_owner_declines_still_dismisses_the_surface() {
        let (mut d, mut rig) = booted();
        let id = open(&mut d, &mut rig, 1, 16);
        seat(&mut d, id);
        d.frame(&mut rig, tick(32), vec![key(Key::Left, tick(32))], vec![], &mut NoTap);
        let p = probe(&d, id);
        assert!(p.contains("backs=1") && p.contains("depth=1"), "declined at its root: {p}");
        assert_eq!(
            d.nav.modals.top().unwrap().phase,
            Phase::Closing,
            "the refusal became the container's BACK, in the same frame"
        );
        assert_ne!(d.nav.input_owner(), Some(InputOwner::Entry(id)), "input left with it");
    }

    /// The engine cannot loop the synthetic BACK back onto itself: `at_edge` is consumed only by
    /// `after_step`'s DIRECTION branch, and `Key::Back` has no direction, so exactly one BACK is
    /// delivered per LEFT however many frames run afterwards.
    #[test]
    fn the_synthetic_back_is_delivered_once_and_never_re_enters_the_engine() {
        let (mut d, mut rig) = booted();
        let id = open(&mut d, &mut rig, 3, 16);
        seat(&mut d, id);
        d.frame(&mut rig, tick(32), vec![key(Key::Left, tick(32))], vec![], &mut NoTap);
        for i in 0..5u32 {
            d.frame(&mut rig, tick(48 + i * 16), vec![], vec![], &mut NoTap);
        }
        let p = probe(&d, id);
        assert!(p.contains("backs=1"), "one LEFT, one BACK, and no carried storm: {p}");
        assert_eq!(d.queued(), 0, "nothing was left circulating");
    }

    /// Every OTHER owner is unchanged. A page-stack owner that declines the synthetic BACK pops
    /// its stack exactly as a physical BACK does…
    #[test]
    fn an_edge_rule_back_still_pops_a_page_stack() {
        let (mut d, mut rig) = booted();
        d.request(MachineId::Nav, NavOp::Push(FixtureArg::Page(DEPTH_BASE + 1)));
        d.frame(&mut rig, tick(16), vec![], vec![], &mut NoTap);
        assert_eq!(d.nav.tabs.stack.depth(), 2, "the page is up");
        let top = d.nav.top_page().unwrap().id;
        seat(&mut d, top);
        let r = d.frame(&mut rig, tick(32), vec![key(Key::Left, tick(32))], vec![], &mut NoTap);
        assert_eq!(d.nav.tabs.stack.depth(), 1, "LEFT popped it");
        assert!(!r.back_at_root, "a pop is not the platform's BACK");
    }

    /// …and at the root of the root stack it is still the APPLICATION's answer (the platform's
    /// Home), reported once and asked of the rig exactly once.
    #[test]
    fn an_edge_rule_back_at_the_root_is_still_the_applications() {
        let (mut d, mut rig) = booted();
        let home = d.nav.top_page().unwrap().id;
        seat(&mut d, home);
        let r = d.frame(&mut rig, tick(16), vec![key(Key::Left, tick(16))], vec![], &mut NoTap);
        assert!(r.back_at_root, "the root refused it");
        assert_eq!(rig.roots, 1, "…and the application heard it once");
        assert_eq!(d.nav.tabs.stack.depth(), 1, "the stack never moved");
    }
}
