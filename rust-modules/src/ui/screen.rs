//! The screen contract (spec §6.1), the container query protocol focus and hit resolution run
//! over (§7.1), and the `Composed`/`Part` pair that lets a screen be ASSEMBLED from components
//! rather than hand-drawn (§7.1).
//!
//! A composed screen gets its `Focusable` through the `composed_*` free functions and the
//! `focusable_via_composed!` macro rather than a blanket impl: coherence forbids a blanket over
//! `Composed` once the widgets implement `Focusable` themselves (`ui::geom`, phase 3a).
//! `DrawFrame::stop` folds the painter's scale/clip cascade into the registered stop and
//! `DrawFrame::clip` is the RAII scissor scope (phase 3a); the nav alphas land with the
//! containers (3b).
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
    /// Frozen request-time memory, delivered before restored Enter for live and remounted bodies.
    RestoreMemory(H::Memory),
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
            ScreenEvent::RestoreMemory(_) => "restore_memory",
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
    /// Whether remounting an evicted child surface can read this page's identity-matched data.
    fn covered_surfaces_ready(&self) -> bool { true }
    /// May the container's strip be reached from this page right now (§6.2)? Home answers
    /// `false` while snapped to the grid.
    fn strip_reachable(&self) -> bool {
        true
    }
    /// The page's declared `Link`s (§7.3 step 3) — the strip→hero door among them.
    fn links(&self, _out: &mut Vec<Link>) {}
    fn focus_source(&self) -> FocusSource {
        FocusSource::Engine
    }
    fn hit_source(&self) -> HitSource {
        HitSource::Engine
    }
    /// The bytes of render this screen holds (its backing textures), for the `RenderSet` check.
    fn render_bytes(&self) -> usize {
        0
    }
    /// An `Opaque` surface's ground has drawn at full strength: the fold may REPLACE the host
    /// from here (§6.2 `Surface::ground_ready`). The dispatcher copies it onto the surface after
    /// every draw; a page never answers.
    fn ground_ready(&self) -> bool {
        false
    }
    /// This screen's own [`ReturnState::memory`] payload, RIGHT NOW — asked by the dispatcher's
    /// `ret()` (spec §6.1 tier 2) at the moment a navigation OFF this screen is raised, exactly as
    /// `focus` is read off the engine at the same instant. **Pure, like every other query here**:
    /// answering is a snapshot, never a mutation, and a screen that has nothing worth remembering
    /// (most of them) takes the default and never overrides this. `stores/metadata.rs`'s module
    /// doc is the worked example (Detail's `Spot`) and the contract every phase-7 screen that
    /// needs to remember something builds against, rather than re-deciding where its own memory
    /// lives.
    fn memory(&self) -> H::Memory {
        H::Memory::default()
    }
    /// Capture entry memory using the engine's current focus without storing a second cursor.
    fn memory_at(&self, _focus: Option<FocusKey<H::Elem>>) -> H::Memory {
        self.memory()
    }
    /// Typed application inspection during migration; the library never names a screen type.
    fn as_any(&self) -> Option<&dyn std::any::Any> {
        None
    }
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }
}

/// The application's screen argument (§6.1).
pub trait ScreenArg: Clone + LogicalState + 'static {
    fn chrome(&self) -> Chrome;
    fn id(&self) -> ScreenId;
    fn title(&self) -> Option<&str>;
    fn same_instance(&self, other: &Self) -> bool;
}

/// Return state (§6.1 tier 2): on the `Entry`, captured at request time — what an evicted entry
/// remounts from.
///
/// `M` is the opaque per-screen payload (`Host::Memory`), defaulted to `()` so every existing
/// spelling — `ReturnState<H::Elem>`, `ReturnState::default()` — keeps meaning exactly what it did
/// before this field existed: a Host with nothing to remember (`InnerHost`/`FixtureHost`,
/// and any container that only ever names `ReturnState<K>` with one type
/// argument) pays nothing and changes nothing. A screen that DOES need to remember something asks
/// for it through [`Screen::memory_at`] and fixes its Host's `Memory` associated type to a concrete
/// payload — see that method's doc and `stores/metadata.rs`'s module doc (`Spot`, the worked case).
///
/// **Not `Copy`.** It was, while its only fields were a key and a float; `M` is application data
/// (a `Spot` is not `Copy`) and every real use here is a single owned value passed once — `Clone`
/// costs nothing to keep for a caller that genuinely needs a second copy, and dropping the derive
/// only removes a bound this type never relied on.
#[derive(Clone, Debug)]
pub struct ReturnState<K, M = ()> {
    pub focus: Option<FocusKey<K>>,
    /// Engine-owned cursors for every group in this entry, including inactive groups.
    pub remembered: Vec<(GroupId, K)>,
    pub scroll: f32,
    /// The screen's own [`Screen::memory`] snapshot, taken the moment a navigation off it was
    /// raised — tier 2's answer to "where was this page standing" for whatever a screen's `focus`
    /// and `scroll` alone cannot say (spec §6.1; e.g. Detail's season/column/sub-row `Spot`).
    pub memory: M,
}

pub const RETURN_STATE_SHAPE: &str = "ReturnState{focus:Option<(EntryId,H::Elem)>,remembered:[(GroupId,H::Elem)],scroll:f32,memory:H::Memory}";

impl<K, M: Default> Default for ReturnState<K, M> {
    fn default() -> Self {
        Self {
            focus: None,
            remembered: Vec::new(),
            scroll: 0.0,
            memory: M::default(),
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
        ret: &ReturnState<H::Elem, H::Memory>,
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

/// What a group's elements ARE for the press (§7.4): a `Card` arms a holdable press, a `Control`
/// a non-holdable one, `Bare` delivers `Activate` on the DOWN edge and never arms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ElemKind {
    Card,
    Control,
    Bare,
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
    pub elem: ElemKind,
}

/// A declared non-standard transition (§7): at `from`'s `dir` edge, focus goes to `to` — and
/// wins over the group's `EdgeRule` for that direction only.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Link {
    pub from: GroupId,
    pub dir: Dir,
    pub to: GroupId,
}

/// Who answers a direction for this screen (§7.6): the engine, or the legacy ladders — for a
/// `LegacyPage` the engine and the hit map are INERT.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusSource {
    Engine,
    Legacy,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HitSource {
    Engine,
    Legacy,
}

#[derive(Clone, Copy, Debug)]
pub enum Step<K> {
    Move(FocusKey<K>),
    Edge,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    /// The element's INDEX in its group when it is one — the `from` tie-break of
    /// `card_row::column_near_x` (an exact tie keeps the index you had; any direction-only rule
    /// ratchets), carried on the placement so `seat` can read it without a second argument.
    pub index: Option<u32>,
}

/// EVERY method takes `&self`: the engine never mutates a screen (§7.3 step 5 says who does).
pub trait Focusable<H: Host> {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>);
    /// Which group an element belongs to — what the engine consults an `EdgeRule` on.
    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId>;
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

/// A `Composed` screen's focus protocol is its parts' concatenated in `layout` order. Not a
/// blanket `impl<T: Composed> Focusable for T` — coherence forbids any other `Focusable` impl
/// beside one (the widgets' own, `ui::geom`) — so a composed screen writes
/// `focusable_via_composed!(Type)`, which delegates to these five.
pub fn composed_groups<H: Host, T: Composed<H> + ?Sized>(s: &T, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
    for (id, _) in s.layout(cx) {
        s.part(id).groups(cx, out);
    }
}

pub fn composed_group_of<H: Host, T: Composed<H> + ?Sized>(s: &T, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
    s.layout(cx)
        .into_iter()
        .find_map(|(id, _)| s.part(id).group_of(key, cx))
}

pub fn composed_neighbour<H: Host, T: Composed<H> + ?Sized>(
    s: &T,
    key: FocusKey<H::Elem>,
    dir: Dir,
    cx: &Cx<'_, H>,
) -> Step<H::Elem> {
    for (id, _) in s.layout(cx) {
        let part = s.part(id);
        if part.place(&key.elem, cx, At::SpringTarget).is_some() {
            return part.neighbour(key, dir, cx);
        }
    }
    Step::Edge
}

pub fn composed_place<H: Host, T: Composed<H> + ?Sized>(s: &T, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
    s.layout(cx)
        .into_iter()
        .find_map(|(id, _)| s.part(id).place(key, cx, at))
}

pub fn composed_reconcile<H: Host, T: Composed<H> + ?Sized>(
    s: &T,
    want: FocusKey<H::Elem>,
    cx: &Cx<'_, H>,
) -> FocusKey<H::Elem> {
    // the element may no longer PLACE (a shelf that shrank under it): every part is asked, and
    // the first answer that places is the reconciliation
    for (id, _) in s.layout(cx) {
        let part = s.part(id);
        let r = part.reconcile(want, cx);
        if part.place(&r.elem, cx, At::SpringTarget).is_some() {
            return r;
        }
    }
    want
}

pub fn composed_seat<H: Host, T: Composed<H> + ?Sized>(s: &T, g: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
    let mut specs = Vec::new();
    for (id, _) in s.layout(cx) {
        specs.clear();
        let part = s.part(id);
        part.groups(cx, &mut specs);
        if specs.iter().any(|sp| sp.id == g) {
            return part.seat(g, from, cx);
        }
    }
    // No part owns the group: the engine's last resort is the first part's first seat.
    let first = s
        .layout(cx)
        .first()
        .map(|(id, _)| *id)
        .expect("a Composed screen has at least one part");
    s.part(first).seat(g, from, cx)
}

/// `impl Focusable<H> for $t` by delegation to its `Composed` parts (see `composed_groups`).
#[macro_export]
macro_rules! focusable_via_composed {
    ($t:ty, $h:ty) => {
        impl $crate::ui::screen::Focusable<$h> for $t {
            fn groups(&self, cx: &$crate::ui::machine::Cx<'_, $h>, out: &mut Vec<$crate::ui::screen::GroupSpec>) {
                $crate::ui::screen::composed_groups(self, cx, out)
            }
            fn group_of(
                &self,
                key: &<$h as $crate::ui::machine::Host>::Elem,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> Option<$crate::ui::machine::GroupId> {
                $crate::ui::screen::composed_group_of(self, key, cx)
            }
            fn neighbour(
                &self,
                key: $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem>,
                dir: $crate::ui::screen::Dir,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> $crate::ui::screen::Step<<$h as $crate::ui::machine::Host>::Elem> {
                $crate::ui::screen::composed_neighbour(self, key, dir, cx)
            }
            fn place(
                &self,
                key: &<$h as $crate::ui::machine::Host>::Elem,
                cx: &$crate::ui::machine::Cx<'_, $h>,
                at: $crate::ui::screen::At,
            ) -> Option<$crate::ui::screen::Placed> {
                $crate::ui::screen::composed_place(self, key, cx, at)
            }
            fn reconcile(
                &self,
                want: $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem>,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem> {
                $crate::ui::screen::composed_reconcile(self, want, cx)
            }
            fn seat(
                &self,
                g: $crate::ui::machine::GroupId,
                from: $crate::ui::screen::Placed,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem> {
                $crate::ui::screen::composed_seat(self, g, from, cx)
            }
        }
    };
}

/// `impl Focusable<H> for $t` by delegation to a VIEW the screen builds for the frame — a
/// `TableScreen`/`DocumentScreen` over its own state (`fn $view(&self) -> impl Focusable<$h>`).
/// The screen keeps the widgets; the composition is rebuilt per query, exactly as the draw
/// rebuilds it (phase 5b's family pages).
#[macro_export]
macro_rules! focusable_via_view {
    ($t:ty, $h:ty, $view:ident) => {
        impl $crate::ui::screen::Focusable<$h> for $t {
            fn groups(&self, cx: &$crate::ui::machine::Cx<'_, $h>, out: &mut Vec<$crate::ui::screen::GroupSpec>) {
                $crate::ui::screen::Focusable::<$h>::groups(&self.$view(), cx, out)
            }
            fn group_of(
                &self,
                key: &<$h as $crate::ui::machine::Host>::Elem,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> Option<$crate::ui::machine::GroupId> {
                $crate::ui::screen::Focusable::<$h>::group_of(&self.$view(), key, cx)
            }
            fn neighbour(
                &self,
                key: $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem>,
                dir: $crate::ui::screen::Dir,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> $crate::ui::screen::Step<<$h as $crate::ui::machine::Host>::Elem> {
                $crate::ui::screen::Focusable::<$h>::neighbour(&self.$view(), key, dir, cx)
            }
            fn place(
                &self,
                key: &<$h as $crate::ui::machine::Host>::Elem,
                cx: &$crate::ui::machine::Cx<'_, $h>,
                at: $crate::ui::screen::At,
            ) -> Option<$crate::ui::screen::Placed> {
                $crate::ui::screen::Focusable::<$h>::place(&self.$view(), key, cx, at)
            }
            fn reconcile(
                &self,
                want: $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem>,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem> {
                $crate::ui::screen::Focusable::<$h>::reconcile(&self.$view(), want, cx)
            }
            fn seat(
                &self,
                g: $crate::ui::machine::GroupId,
                from: $crate::ui::screen::Placed,
                cx: &$crate::ui::machine::Cx<'_, $h>,
            ) -> $crate::ui::machine::FocusKey<<$h as $crate::ui::machine::Host>::Elem> {
                $crate::ui::screen::Focusable::<$h>::seat(&self.$view(), g, from, cx)
            }
        }
    };
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
/// the stop sink. Phase 3a: `stop` folds the painter's cascade (translate, pop, clip) into the
/// registered rect, and `clip` is the RAII scissor scope. The sink is TYPED and lives here rather
/// than type-erased on the painter (the spec's `sink: Option<&HitSink>`): a `Part` draws through
/// this frame, so there is still exactly one writer and the painter stays `Copy` and
/// lifetime-free. The nav alphas arrive with the containers (3b).
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
    /// `s.rect`/`s.rest_rect` are in the painter's OWN space; the cascade's translate and pop are
    /// folded in here (`Painter::to_screen`) and the stop is clipped to the cascade's clip
    /// intersected with `s.clip` (also in painter space).
    pub fn stop(&mut self, p: Painter, mut s: Stop<H::Elem>) {
        let (rect, _, cascade_clip) = p.to_screen(s.rect);
        let (_, rest, _) = p.to_screen(s.rest_rect);
        let own = Rect::new(s.clip.x + p.dx(), s.clip.y + p.dy(), s.clip.w, s.clip.h);
        s.rect = rect;
        s.rest_rect = rest;
        s.clip = cascade_clip.intersect(own);
        self.stops.push(s);
    }

    /// Open a GL scissor for `r` (painter space) for the rest of the returned scope: the
    /// cascade's clip narrows to it and the scissor is restored when the scope drops — the RAII
    /// replacement for the bare `Painter::clip`/`clip_clear` pair (spec §7.6). Draw the clipped
    /// content through the painter the scope hands back.
    pub fn clip(&mut self, p: Painter, r: Rect) -> ClipScope {
        let inner = p.clipped(r);
        ClipScope::open(inner.clip_rect())
    }

    pub fn stops(&self) -> &[Stop<H::Elem>] {
        &self.stops
    }

    pub fn into_stops(self) -> Vec<Stop<H::Elem>> {
        self.stops
    }
}

/// A live GL scissor, restored on drop to what was set before it (nesting-safe). Pure
/// bookkeeping on the host test binary (no GL is linked): what is graded is the stack.
pub struct ClipScope {
    prev: Option<Rect>,
}

thread_local! {
    /// The scissor currently set by a `ClipScope`, if any — what a nested scope restores to.
    static CLIP_STACK: std::cell::Cell<Option<Rect>> = const { std::cell::Cell::new(None) };
}

impl ClipScope {
    fn open(screen: Rect) -> Self {
        let prev = CLIP_STACK.with(|c| c.replace(Some(screen)));
        apply_scissor(Some(screen));
        Self { prev }
    }

    /// The scissor in force (the innermost open scope), for tests and instruments.
    pub fn current() -> Option<Rect> {
        CLIP_STACK.with(|c| c.get())
    }
}

impl Drop for ClipScope {
    fn drop(&mut self) {
        CLIP_STACK.with(|c| c.set(self.prev));
        apply_scissor(self.prev);
    }
}

#[cfg(not(test))]
fn apply_scissor(r: Option<Rect>) {
    match r {
        Some(r) => crate::gfx::clip_set(r.x, r.y, r.w, r.h),
        None => crate::gfx::clip_clear(),
    }
}

#[cfg(test)]
fn apply_scissor(_r: Option<Rect>) {}

impl<'a, H: Host> Deref for DrawFrame<'a, H> {
    type Target = Cx<'a, H>;
    fn deref(&self) -> &Self::Target {
        self.cx
    }
}

#[cfg(test)]
mod draw_frame_tests {
    use super::*;
    use crate::ui::fixture::FixtureHost;
    use crate::ui::machine::{Cx, EntryId, FocusRead, InputOwner, PressRead, Tick};

    fn cx<'a>(measure: &'a dyn crate::ui::machine::Measure, store: &'a crate::ui::fixture::FixtureView) -> Cx<'a, FixtureHost> {
        Cx {
            views: crate::ui::fixture::FixtureViews { store },
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: InputOwner::Entry(EntryId(0)),
        }
    }

    fn stop(r: Rect, clip: Rect) -> Stop<u32> {
        Stop {
            key: FocusKey {
                entry: EntryId(0),
                elem: 1,
            },
            rect: r,
            rest_rect: r,
            clip,
            hover: Hover::Focus,
            activate: Activate::Press,
        }
    }

    /// Spec §7.6: a stop is recorded in SCREEN space — the cascade's translate and pop folded
    /// in, and clipped to the cascade's clip. A tile drawn through `scaled(1.1)` registers its
    /// popped rect; its rest rect is the settled one.
    #[test]
    fn a_stop_is_recorded_in_screen_space_with_the_pop_folded_in() {
        let m = crate::ui::fixture::FixtureMeasure;
        let store = crate::ui::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        let p = f.painter.translate(100.0, 50.0).scaled(1.1);
        f.stop(p, stop(Rect::new(0.0, 0.0, 100.0, 100.0), Rect::FULL));
        let s = &f.stops()[0];
        let want = Rect::new(100.0, 50.0, 100.0, 100.0).scaled(1.1);
        assert_eq!((s.rect.x, s.rect.y, s.rect.w, s.rect.h), (want.x, want.y, want.w, want.h));
        assert_eq!((s.rest_rect.x, s.rest_rect.y, s.rest_rect.w, s.rest_rect.h), (100.0, 50.0, 100.0, 100.0));
    }

    /// A stop under a clipped painter is clipped to the cascade's clip ∩ its own.
    #[test]
    fn a_stop_under_a_clip_carries_the_visible_part_only() {
        let m = crate::ui::fixture::FixtureMeasure;
        let store = crate::ui::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        let p = f.painter.translate(10.0, 0.0).clipped(Rect::new(0.0, 0.0, 50.0, 400.0));
        f.stop(p, stop(Rect::new(0.0, 0.0, 100.0, 100.0), Rect::new(0.0, 0.0, 200.0, 40.0)));
        let s = &f.stops()[0];
        // cascade clip: x 10..60; own clip: x 10..210, y 0..40 → x 10..60, y 0..40
        assert_eq!((s.clip.x, s.clip.y, s.clip.w, s.clip.h), (10.0, 0.0, 50.0, 40.0));
    }

    /// The RAII scissor: nested scopes narrow and restore in order, and nothing is left set.
    #[test]
    fn the_clip_scope_nests_and_restores() {
        let _g = crate::testlock::serial();
        let m = crate::ui::fixture::FixtureMeasure;
        let store = crate::ui::fixture::FixtureView::default();
        let cx = cx(&m, &store);
        let mut f = DrawFrame::new(&cx, Painter::root());
        assert!(ClipScope::current().is_none());
        {
            let p = f.painter.translate(0.0, 100.0);
            let _outer = f.clip(p, Rect::new(0.0, 0.0, 800.0, 300.0));
            let outer = ClipScope::current().unwrap();
            assert_eq!((outer.x, outer.y, outer.w, outer.h), (0.0, 100.0, 800.0, 300.0));
            {
                let inner_p = p.clipped(Rect::new(0.0, 0.0, 800.0, 300.0));
                let _inner = f.clip(inner_p, Rect::new(100.0, 50.0, 2000.0, 2000.0));
                let inner = ClipScope::current().unwrap();
                assert_eq!((inner.x, inner.y, inner.w, inner.h), (100.0, 150.0, 700.0, 250.0));
            }
            let back = ClipScope::current().unwrap();
            assert_eq!((back.x, back.y), (0.0, 100.0));
        }
        assert!(ClipScope::current().is_none());
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
