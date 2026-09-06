//! The screen contract (spec §6.1), the container query protocol focus and hit resolution run
//! over (§7.1), and the `Composed`/`Part` pair that lets a screen be ASSEMBLED from components
//! rather than hand-drawn (§7.1 blanket impls).
//!
//! Phase 2-i: the traits, the blanket `Focusable` for anything `Composed`, and the draw
//! skeleton helpers. `DrawFrame` is the spike's narrow shape (painter root, page alpha, the stop
//! sink); the RAII clip stack and the nav alphas land with the painter work in phase 3a.
#![allow(dead_code)] // phase 2-i: no consumer until phase 2 (spec §13)

use std::borrow::Cow;
use std::ops::Deref;

use super::frame::Budget;
use super::machine::{
    Chrome, Cx, Effects, EntryId, FocusKey, GroupId, Host, InputEvent, InstanceId, Leave,
    LogicalState, Machine, PartId, PressId, PressRead, RequestId, ScreenId, StoreOrd, Tick,
    TimerId,
};
use super::{Painter, Rect};

/// Everything a mounted screen can be told (§6.1).
pub enum ScreenEvent<H: Host> {
    Mount,
    Enter(Enter<H::Elem>),
    Cover,
    Uncover,
    WillLeave(Leave),
    Unmount,
    Suspend,
    Resume,
    Input(InputEvent<H::Elem>),
    PressHold(PressId),
    PressCommit(PressId),
    Activate(H::Elem),
    Tick(Tick),
    Timer(TimerId),
    Async(RequestId, H::Msg),
    StoreChanged(StoreOrd, u32),
    FocusMoved {
        from: Option<FocusKey<H::Elem>>,
        to: FocusKey<H::Elem>,
        by: By,
    },
    App(H::Msg),
}

impl<H: Host> ScreenEvent<H> {
    /// The event's name for a log line or a recording (`life` records, §5.3).
    pub fn name(&self) -> &'static str {
        match self {
            ScreenEvent::Mount => "mount",
            ScreenEvent::Enter(_) => "enter",
            ScreenEvent::Cover => "cover",
            ScreenEvent::Uncover => "uncover",
            ScreenEvent::WillLeave(_) => "will_leave",
            ScreenEvent::Unmount => "unmount",
            ScreenEvent::Suspend => "suspend",
            ScreenEvent::Resume => "resume",
            ScreenEvent::Input(_) => "input",
            ScreenEvent::PressHold(_) => "press_hold",
            ScreenEvent::PressCommit(_) => "press_commit",
            ScreenEvent::Activate(_) => "activate",
            ScreenEvent::Tick(_) => "tick",
            ScreenEvent::Timer(_) => "timer",
            ScreenEvent::Async(..) => "async",
            ScreenEvent::StoreChanged(..) => "store_changed",
            ScreenEvent::FocusMoved { .. } => "focus_moved",
            ScreenEvent::App(_) => "app",
        }
    }
}

/// Lifecycle entry (§6.1) — distinct from `Seat`, the focus-entry policy.
#[derive(Clone, Copy, Debug)]
pub enum Enter<K> {
    Fresh { focus: FocusTarget<K> },
    Restored,
}

/// "Mount with focus on the strip" is expressible.
#[derive(Clone, Copy, Debug)]
pub enum FocusTarget<K> {
    Elem(FocusKey<K>),
    ContainerGroup(GroupId),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum By {
    Dir,
    Pointer,
    Restore,
    Reconcile,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderStrategy {
    Page,
    VideoPlane,
}

/// The screen (§6.1). `step` is the only entrance that mutates logical state after construction.
pub trait Screen<H: Host>: Machine<H, Ev = ScreenEvent<H>> + Focusable<H> {
    /// The heartbeat word — byte-identical to today's route word (§15.3).
    fn name(&self) -> &'static str;
    fn state(&self) -> &dyn LogicalState;
    fn crumb(&self, cx: &Cx<'_, H>) -> Option<Cow<'_, str>>;
    /// RENDER resources only.
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>);
    fn draw(&mut self, f: &mut DrawFrame<'_, H>);
    fn render(&self) -> RenderStrategy;
}

/// The application's screen argument (§6.1).
pub trait ScreenArg: Clone + 'static {
    fn chrome(&self) -> Chrome;
    fn id(&self) -> ScreenId;
    fn title(&self) -> Option<&str>;
    fn same_instance(&self, other: &Self) -> bool;
}

/// Return state (§6.1 tier 2): on the `Entry`, captured at request time — what an evicted entry
/// remounts from.
#[derive(Clone, Copy, Debug)]
pub struct ReturnState<K> {
    pub focus: Option<FocusKey<K>>,
    pub scroll: f32,
}

impl<K> Default for ReturnState<K> {
    fn default() -> Self {
        Self {
            focus: None,
            scroll: 0.0,
        }
    }
}

/// The ONE `mount` match (`screens/registry.rs`). Receives the id first so a request it emits is
/// addressable.
pub trait Mounter<H: Host> {
    fn mount(
        &mut self,
        id: InstanceId,
        arg: &H::Arg,
        ret: &ReturnState<H::Elem>,
        cx: &Cx<'_, H>,
        fx: &mut Effects<'_, H>,
    ) -> Box<dyn Screen<H>>;
}

// ---------------------------------------------------------------------------------------------
// §7.1 — the container query protocol
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug)]
pub enum GroupKind {
    Row { wrap: bool },
    Column,
    Grid { cols: usize, holes: &'static [(usize, usize)] },
    Free,
    Document,
}

/// The focus-ENTRY policy (§7.3 step 4), named cases, never one heuristic.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Seat {
    Nearest,
    Remembered,
    RememberedNear { rows: u8 },
    First,
    Projected,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EdgeRule {
    Geometric,
    Stop,
    Screen,
    Nav(super::machine::NavOpKind),
}

/// Bit 0 = horizontal, bit 1 = vertical: the axes along which a geometric search may LAND here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AxisMask(pub u8);

impl AxisMask {
    pub const BOTH: AxisMask = AxisMask(0b11);
    pub const HORIZONTAL: AxisMask = AxisMask(0b01);
    pub const VERTICAL: AxisMask = AxisMask(0b10);
}

#[derive(Clone, Copy, Debug)]
pub struct GroupSpec {
    pub id: GroupId,
    pub kind: GroupKind,
    pub seat: Seat,
    pub reachable: AxisMask,
    /// `[up, down, left, right]`.
    pub edge: [EdgeRule; 4],
    pub extent: Rect,
    pub len: usize,
}

#[derive(Clone, Copy, Debug)]
pub enum Step<K> {
    Move(FocusKey<K>),
    Edge,
}

#[derive(Clone, Copy, Debug)]
pub enum At {
    SpringTarget,
    Drawn,
}

/// What was painted / where it rests / what clipped it (§7.6).
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    pub rect: Rect,
    pub rest_rect: Rect,
    pub clip: Rect,
}

/// EVERY method takes `&self`: the engine never mutates a screen (§7.3 step 5 says who does).
pub trait Focusable<H: Host> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>);
    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem>;
    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed>;
    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem>;
    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem>;
}

/// A component that can be focused AND drawn — what `&dyn Focusable` cannot (§7.1).
pub trait Part<H: Host>: Focusable<H> {
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, H>);
    fn draw(&mut self, f: &mut DrawFrame<'_, H>, rect: Rect);
}

/// A screen assembled from parts. `part_mut` is the only `&mut` access and it is to a RENDER
/// half; logical state stays behind `step`.
pub trait Composed<H: Host> {
    fn layout(&self, cx: &Cx<'_, H>) -> Vec<(PartId, Rect)>;
    fn part(&self, id: PartId) -> &dyn Part<H>;
    fn part_mut(&mut self, id: PartId) -> &mut dyn Part<H>;
}

/// The blanket: a `Composed` screen's focus protocol is its parts' concatenated in `layout` order.
impl<H: Host, T: Composed<H>> Focusable<H> for T {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        for (id, _) in self.layout(cx) {
            self.part(id).groups(cx, out);
        }
    }

    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        for (id, _) in self.layout(cx) {
            let part = self.part(id);
            if part.place(&key.elem, cx, At::SpringTarget).is_some() {
                return part.neighbour(key, dir, cx);
            }
        }
        Step::Edge
    }

    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        self.layout(cx)
            .into_iter()
            .find_map(|(id, _)| self.part(id).place(key, cx, at))
    }

    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        for (id, _) in self.layout(cx) {
            let part = self.part(id);
            if part.place(&want.elem, cx, At::SpringTarget).is_some() {
                return part.reconcile(want, cx);
            }
        }
        want
    }

    fn seat(&self, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let mut specs = Vec::new();
        for (id, _) in self.layout(cx) {
            specs.clear();
            let part = self.part(id);
            part.groups(cx, &mut specs);
            if specs.iter().any(|s| s.id == g) {
                return part.seat(g, from, cx);
            }
        }
        // No part owns the group: the engine's last resort is the first part's first seat.
        let first = self
            .layout(cx)
            .first()
            .map(|(id, _)| *id)
            .expect("a Composed screen has at least one part");
        self.part(first).seat(g, from, cx)
    }
}

/// The draw skeleton for a `Composed` screen: iterate `layout`, prepare / draw each part.
pub fn composed_prepare<H: Host, T: Composed<H>>(s: &mut T, b: &mut Budget, cx: &Cx<'_, H>) {
    for (id, _) in s.layout(cx) {
        s.part_mut(id).prepare(b, cx);
    }
}

pub fn composed_draw<H: Host, T: Composed<H>>(s: &mut T, f: &mut DrawFrame<'_, H>) {
    let cx: &Cx<'_, H> = f;
    let layout = s.layout(cx);
    for (id, rect) in layout {
        s.part_mut(id).draw(f, rect);
    }
}

/// A stop the draw registers (§7.5, §7.6).
#[derive(Clone, Copy, Debug)]
pub struct Stop<K> {
    pub key: FocusKey<K>,
    pub rect: Rect,
    pub rest_rect: Rect,
    pub clip: Rect,
    pub hover: Hover,
    pub activate: Activate,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hover {
    Focus,
    Ignore,
    OnlyIfFocused,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Activate {
    Press,
    Immediate,
    Direct,
}

/// What `draw` receives (§6.1): `Deref<Target = Cx>` plus the painter root, the page alpha and
/// the stop sink. Phase 3a adds the RAII clip stack and the nav alphas.
pub struct DrawFrame<'a, H: Host> {
    pub cx: &'a Cx<'a, H>,
    pub painter: Painter,
    pub page_alpha: f32,
    pub press: PressRead,
    stops: Vec<Stop<H::Elem>>,
}

impl<'a, H: Host> DrawFrame<'a, H> {
    pub fn new(cx: &'a Cx<'a, H>, painter: Painter) -> Self {
        Self {
            cx,
            painter,
            page_alpha: 1.0,
            press: cx.press,
            stops: Vec::new(),
        }
    }

    /// The hit map's only writer (§7.6): records the stop in screen space with insertion index.
    pub fn stop(&mut self, s: Stop<H::Elem>) {
        self.stops.push(s);
    }

    pub fn stops(&self) -> &[Stop<H::Elem>] {
        &self.stops
    }

    pub fn into_stops(self) -> Vec<Stop<H::Elem>> {
        self.stops
    }
}

impl<'a, H: Host> Deref for DrawFrame<'a, H> {
    type Target = Cx<'a, H>;
    fn deref(&self) -> &Self::Target {
        self.cx
    }
}

/// Lifecycle-sequence helper (§3.4): the events a Push delivers, in order, as `(entry, event)`.
/// Kept as data so the dispatcher's tables are testable without a container.
pub fn push_sequence<H: Host>(
    old_top: Option<EntryId>,
    new: EntryId,
    focus: FocusTarget<H::Elem>,
) -> Vec<(EntryId, ScreenEvent<H>)> {
    let mut v = Vec::new();
    if let Some(old) = old_top {
        v.push((old, ScreenEvent::WillLeave(Leave::Deeper)));
    }
    v.push((new, ScreenEvent::Mount));
    v.push((new, ScreenEvent::Enter(Enter::Fresh { focus })));
    if let Some(old) = old_top {
        v.push((old, ScreenEvent::Cover));
    }
    v
}

/// The events a Pop delivers, in order (§3.4).
pub fn pop_sequence<H: Host>(top: EntryId, under: Option<EntryId>) -> Vec<(EntryId, ScreenEvent<H>)> {
    let mut v = vec![
        (top, ScreenEvent::WillLeave(Leave::ForGood)),
        (top, ScreenEvent::Unmount),
    ];
    if let Some(u) = under {
        v.push((u, ScreenEvent::Uncover));
        v.push((u, ScreenEvent::Enter(Enter::Restored)));
    }
    v
}
