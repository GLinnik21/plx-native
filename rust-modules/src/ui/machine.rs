//! The layer-neutral CONTRACT of the UI restructure (spec §3.1, §3.2, §5.1, §5.4): the `Host`
//! bundle a generic library is compiled against, the newtypes, `Machine` + `Cx` + `Effects`, the
//! effect vocabulary `Fx`, input events, navigation ops, addressing, and the `LogicalState` /
//! `Canon` pair every hashed state implements.
//!
//! **Phase 2-i — the contract spike.** This module and its siblings (`screen`, `present`,
//! `landing`, `tex`, `frame`, `dispatch`) are a COMPILING host-only skeleton; nothing in the
//! product calls them until phase 2 replaces `app/run.rs`'s loop with `dispatch`. They exist so
//! the boundaries are proven to COMPOSE against the layer rule (§2.1: this module names no
//! application type, no widget, no engine, no screen — `stores/` will reach `ui::` through this
//! module alone) before any real code moves. Every type here is what the spec names; where the
//! spike narrows one (a `Timer` with no owner registry, a `TextEdit` with three variants) the doc
//! on it says so.
#![allow(dead_code)] // phase 2-i: the contract has no consumer until phase 2 (spec §13)

use std::ffi::CStr;
use std::hash::Hash;

use super::present::{Present, PresentEvent, Provenance};
use super::screen::{ScreenArg, ScreenEvent};

/// The application bundle a generic `ui/` is compiled against (§3.1). The library is tested with
/// `FixtureHost` and no Plex type in scope.
pub trait Host: 'static {
    /// The app's screen argument enum (what an entry is mounted from).
    type Arg: ScreenArg;
    /// App effects: Store / Net / Disk / Sys / Player / Poster / Emit — executed by `app/effects.rs`.
    type Fx: 'static;
    /// App payloads: async results, store notices, deliveries — parsed by the REQUESTER.
    type Msg: 'static;
    /// `ElemKey`: interned, never a String.
    type Elem: Copy + Eq + Hash + 'static;
    /// The read side: `&BrowseView`, `&MetadataView`, … as one `Copy` bundle of references.
    type Views<'a>: Copy;
    /// The application's initial conditions for a recording header (§5.3).
    type Init: LogicalState + 'static;
}

macro_rules! newtype {
    ($(#[$m:meta])* $name:ident) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
        pub struct $name(pub u32);
    };
}
newtype!(
    /// A screen KIND (two Detail entries share one).
    ScreenId
);
newtype!(
    /// One addressed request, unique per addressee.
    RequestId
);
newtype!(
    /// An entry in a container: minted at creation, stable across body eviction (§5.1).
    EntryId
);
newtype!(
    /// A mounted body: minted at MOUNT, never reused, the addressee of async work (§5.1).
    InstanceId
);
newtype!(
    /// A timer; the id encodes its owner (§3.4).
    TimerId
);
newtype!(
    /// One armed press.
    PressId
);
newtype!(
    /// A focus group inside a screen or container (§7.1).
    GroupId
);
newtype!(
    /// An opaque poster identity — the seam between the app's source and the render cache (§10).
    PosterKey
);
newtype!(
    /// A store, as the LIBRARY sees it: an ordinal the app maps to its `StoreId` (§5.1).
    StoreOrd
);
newtype!(
    /// A part of a `Composed` screen (§7.1).
    PartId
);

/// Why an entry is left (§3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Leave {
    Deeper,
    ForGood,
}

/// What shared chrome a `ScreenArg` wears (today's `Nav::wears_tab_bar` match).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Chrome {
    TabBar,
    None,
}

/// A `step`'s answer: did the machine consume the event.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Handled {
    Yes,
    No,
}

/// The frame time (§4.1): one per frame, the ONLY clock a machine sees.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Tick {
    pub ms: u32,
    pub dt_us: u32,
}

impl Tick {
    /// Seconds since the previous tick, as the animation timestep the integrators take.
    pub fn dt(self) -> f32 {
        self.dt_us as f32 / 1_000_000.0
    }
}

/// A source of ticks (§4.1): `SdlClock` on the device, `VirtualClock` under replay.
pub trait Clock {
    fn tick(&mut self) -> Tick;
}

/// The replay clock: hands out exactly the ticks it was given.
pub struct VirtualClock {
    ticks: std::collections::VecDeque<Tick>,
    last: Tick,
}

impl VirtualClock {
    pub fn new(ticks: impl IntoIterator<Item = Tick>) -> Self {
        Self {
            ticks: ticks.into_iter().collect(),
            last: Tick::default(),
        }
    }
}

impl Clock for VirtualClock {
    fn tick(&mut self) -> Tick {
        if let Some(t) = self.ticks.pop_front() {
            self.last = t;
        }
        self.last
    }
}

/// The one contract every machine implements: `State + Event → State + Effects`. `&mut self` and
/// an effect sink rather than a pure `(S, E) -> (S, Vec<E>)` because the stores are too large to
/// return by value; purity is enforced by `Effects` being the only exit (§3.1).
pub trait Machine<H: Host> {
    type Ev;
    fn step(&mut self, ev: &Self::Ev, cx: &Cx<'_, H>, fx: &mut Effects<'_, H>) -> Handled;
}

/// Synchronous text measurement as a CAPABILITY (§4.3): `TtfMeasure` on the device and the
/// simulator, `TableMeasure` under replay, `FixtureMeasure` in host tests. Never a free function.
pub trait Measure {
    fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32;
    fn cap_h(&self, sz: i32) -> f32;
    fn line_h(&self, sz: i32) -> f32;
}

/// What a machine may read about the press machine (§7.4): the renderer's two numbers.
#[derive(Clone, Copy, Default, Debug)]
pub struct PressRead {
    pub scale: f32,
    pub is_long: bool,
}

/// What a machine may read about focus (§7.3 step 5): the engine owns the state, screens read it.
#[derive(Clone, Copy, Debug)]
pub struct FocusRead<K> {
    pub current: Option<FocusKey<K>>,
}

impl<K> Default for FocusRead<K> {
    fn default() -> Self {
        Self { current: None }
    }
}

/// Focus identity (§7.2): the entry and the CONTROL (never its face). Strict equality — no
/// item-first/slot-second fallback; promotion is an explicit `reconcile` answer.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FocusKey<K> {
    pub entry: EntryId,
    pub elem: K,
}

/// The read-only context a step receives (§3.1).
pub struct Cx<'a, H: Host> {
    pub views: H::Views<'a>,
    pub tick: Tick,
    pub measure: &'a dyn Measure,
    pub press: PressRead,
    pub focus: FocusRead<H::Elem>,
    pub owner: InputOwner,
}

/// The dispatcher's reborrow of `App.present` for one step (§3.1, §4.4): `note` is its one method.
pub struct PresentHandle<'p>(pub(super) &'p mut Present);

impl<'p> PresentHandle<'p> {
    /// The application's reborrow of its `Present` for one step (the dispatcher's, or the
    /// legacy loop's around the render cache's prepare).
    pub fn of(p: &'p mut Present) -> Self {
        PresentHandle(p)
    }
}

impl PresentHandle<'_> {
    pub fn note(&mut self, ev: PresentEvent) {
        self.0.note(ev);
    }
}

/// One machine's emission in one step, stamped with who emitted it.
pub struct Stamped<H: Host> {
    pub from: MachineId,
    pub fx: Fx<H>,
}

/// A single machine emitting more than this in one `step` is a debug assertion (§3.3 step 6).
pub const MAX_EMIT_PER_STEP: u32 = 32;

/// The ONE effect sink (§3.1): a dispatcher-owned buffer the dispatcher stamps `from` on at push,
/// plus the present handle. `push` and `note` are the two exits; `invalidate` is sugar.
pub struct Effects<'p, H: Host> {
    buf: &'p mut Vec<Stamped<H>>,
    from: MachineId,
    present: PresentHandle<'p>,
    emitted: u32,
}

impl<'p, H: Host> Effects<'p, H> {
    pub fn new(buf: &'p mut Vec<Stamped<H>>, from: MachineId, present: &'p mut Present) -> Self {
        Self {
            buf,
            from,
            present: PresentHandle(present),
            emitted: 0,
        }
    }

    pub fn push(&mut self, fx: Fx<H>) {
        self.emitted += 1;
        debug_assert!(
            self.emitted <= MAX_EMIT_PER_STEP,
            "{:?} emitted more than MAX_EMIT_PER_STEP effects in one step",
            self.from
        );
        self.buf.push(Stamped { from: self.from, fx });
    }

    pub fn note(&mut self, ev: PresentEvent) {
        self.present.note(ev);
    }

    pub fn invalidate(&mut self, why: Provenance) {
        self.note(PresentEvent::Damage(why));
    }

    /// Who this sink stamps — a machine that emits on behalf of a child uses it for `Provenance`.
    pub fn from(&self) -> MachineId {
        self.from
    }

    /// How many effects this step has pushed (the dispatcher's per-step count).
    pub fn emitted(&self) -> u32 {
        self.emitted
    }
}

/// The library's effect vocabulary (§3.1). Damage is NOT a variant: `Effects::invalidate`.
pub enum Fx<H: Host> {
    /// Structural — executed only at NAV COMMIT (§3.3 step 7).
    Nav(NavOp<H::Arg>),
    Mount(EntryId),
    Unmount(EntryId),
    /// Executed in the drain: step the target, append its emissions to the tail.
    Deliver(MachineId, Delivery<H>),
    Timer {
        id: TimerId,
        after_ms: u32,
    },
    CancelTimer(TimerId),
    Press(PressArm<H::Elem>),
    Log(LogLine),
    /// Handed to the application's adapters.
    App(H::Fx),
}

/// What a `Deliver` carries: a screen event for an instance, or the app's own message.
pub enum Delivery<H: Host> {
    Screen(ScreenEvent<H>),
    Machine(H::Msg),
}

/// One event-log line a machine asks the dispatcher to write (machines never log directly).
pub struct LogLine(pub String);

/// A press arm (§7.4).
#[derive(Clone, Copy, Debug)]
pub struct PressArm<K> {
    pub key: FocusKey<K>,
    pub from: PressFrom,
    pub holdable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PressFrom {
    Key,
    Pointer,
}

/// Input (§3.2). The remote FIFO produces these directly; there is no SDL byte-array synthesis.
#[derive(Clone, Copy, Debug)]
pub struct InputEvent<K> {
    pub at: Tick,
    pub source: Source,
    pub kind: InputKind<K>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Sdl,
    RemoteFifo,
    Script,
    Replay,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Edge {
    Down,
    /// The hardware auto-repeat (`0x101`), carried so the press machine's dropped-key-up net and
    /// the HUD's continuous scrub replay.
    Repeat,
    Up,
}

/// The direction / activation vocabulary a screen matches on. The spike carries the four
/// directions and the two activation keys; `consts::Key`'s full alphabet joins in phase 2.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    Other,
}

#[derive(Clone, Copy, Debug)]
pub enum InputKind<K> {
    Key {
        key: Key,
        sym: u32,
        wcode: u32,
        edge: Edge,
        /// Set by the engine when it re-delivers a direction under `EdgeRule::Screen` (§7.1);
        /// false on first delivery. An unhandled `at_edge: true` key is DROPPED.
        at_edge: bool,
    },
    Pointer {
        x: f32,
        y: f32,
        hit: Option<K>,
    },
    Click {
        x: f32,
        y: f32,
        hit: Option<K>,
    },
    Drag {
        x: f32,
        y: f32,
        hit: Option<K>,
    },
    Wheel {
        dy: f32,
    },
    Text(TextEdit),
    PointerHidden,
    SystemKeyboard(bool),
}

/// What the television's keyboard sends (§7.3 step 7). The spike's three edits; the field's full
/// grammar (`docs/search.md`) joins with the Search screen in phase 8.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextEdit {
    Insert(char),
    Backspace,
    Clear,
}

/// Who receives input this frame (§3.2): a page OR a modal surface (both are entries), or a
/// system owner such as the television keyboard.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum InputOwner {
    Entry(EntryId),
    System(SystemInput),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SystemInput {
    Keyboard,
}

/// Navigation ops (§3.4). A transition's op applies at its floor, a cut's op now.
#[derive(Clone, Debug)]
pub enum NavOp<A> {
    Root(A),
    Push(A),
    Pop,
    PopTo(EntryId),
    Replace(A),
    SelectTab(A),
    Present(A),
    Dismiss(EntryId),
    Cancel,
}

/// The payload-free form an `EdgeRule` names (§3.4).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavOpKind {
    /// `Pop` on the input owner's own stack; `Dismiss` when that stack is a modal at depth 0.
    Back,
    Dismiss,
}

/// Every addressable machine (§5.1). `Store` carries the library's ordinal, never the app's id.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum MachineId {
    Session,
    Consent,
    Input,
    Present,
    Nav,
    Player,
    Store(StoreOrd),
    Instance(InstanceId),
    Cache,
}

/// Where an async result goes (§5.1).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Addr {
    pub to: MachineId,
    pub req: RequestId,
}

/// Canonical state (§5.4): a hand-written encoder — floats as bits, collections as explicitly
/// ordered sequences, enums as explicit discriminants — hashed to one `u64`. No JSON is hashed.
pub struct Canon {
    h: u64,
    len: u64,
}

impl Default for Canon {
    fn default() -> Self {
        Self::new()
    }
}

impl Canon {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    pub fn new() -> Self {
        Self {
            h: Self::OFFSET,
            len: 0,
        }
    }

    fn byte(&mut self, b: u8) {
        self.h ^= b as u64;
        self.h = self.h.wrapping_mul(Self::PRIME);
        self.len += 1;
    }

    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.tag(1);
        self.byte(v);
        self
    }

    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.tag(2);
        for b in v.to_le_bytes() {
            self.byte(b);
        }
        self
    }

    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.tag(3);
        for b in v.to_le_bytes() {
            self.byte(b);
        }
        self
    }

    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.tag(4);
        self.byte(v as u8);
        self
    }

    /// Floats are their BITS: NaN, +0/-0 and Inf are all distinct and all stable.
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.tag(5);
        for b in v.to_bits().to_le_bytes() {
            self.byte(b);
        }
        self
    }

    pub fn str(&mut self, v: &str) -> &mut Self {
        self.tag(6);
        self.u32(v.len() as u32);
        for b in v.bytes() {
            self.byte(b);
        }
        self
    }

    /// An enum's discriminant, written explicitly by the census `match`.
    pub fn discriminant(&mut self, d: u32) -> &mut Self {
        self.tag(7);
        self.u32(d)
    }

    /// The length prefix of an explicitly ORDERED sequence; a `HashMap` never reaches here.
    pub fn seq(&mut self, len: usize) -> &mut Self {
        self.tag(8);
        self.u32(len as u32)
    }

    /// `None` is distinct from any value, including a zero.
    pub fn option<T>(&mut self, v: Option<T>, f: impl FnOnce(&mut Self, T)) -> &mut Self {
        match v {
            None => {
                self.tag(9);
            }
            Some(t) => {
                self.tag(10);
                f(self, t);
            }
        }
        self
    }

    fn tag(&mut self, t: u8) {
        self.byte(t);
    }

    pub fn finish(&self) -> u64 {
        self.h ^ self.len.rotate_left(32)
    }
}

/// What every hashed, restored, recorded state implements (§5.4).
pub trait LogicalState {
    /// Write the canonical encoding.
    fn write(&self, w: &mut Canon);
    /// A human-readable probe for a divergence report (never hashed).
    fn probe(&self, out: &mut String);

    fn hash(&self) -> u64 {
        let mut c = Canon::new();
        self.write(&mut c);
        c.finish()
    }
}

#[cfg(test)]
mod canon_tests {
    use super::*;

    #[test]
    fn canon_distinguishes_nan_from_null_and_orders_collections() {
        let mut a = Canon::new();
        a.option(Some(f32::NAN), |c, v| {
            c.f32(v);
        });
        let mut b = Canon::new();
        b.option::<f32>(None, |c, v| {
            c.f32(v);
        });
        assert_ne!(a.finish(), b.finish(), "NaN and None must hash apart");

        let mut p = Canon::new();
        p.seq(2).u32(1).u32(2);
        let mut q = Canon::new();
        q.seq(2).u32(2).u32(1);
        assert_ne!(p.finish(), q.finish(), "a sequence is ORDERED");

        let mut z = Canon::new();
        z.f32(0.0);
        let mut nz = Canon::new();
        nz.f32(-0.0);
        assert_ne!(z.finish(), nz.finish(), "+0 and -0 are different bits");
    }

    struct NoHost;
    #[derive(Clone)]
    struct NoArg;
    impl ScreenArg for NoArg {
        fn chrome(&self) -> Chrome {
            Chrome::None
        }
        fn id(&self) -> ScreenId {
            ScreenId(0)
        }
        fn title(&self) -> Option<&str> {
            None
        }
        fn same_instance(&self, _other: &Self) -> bool {
            true
        }
    }
    struct NoInit;
    impl LogicalState for NoInit {
        fn write(&self, _w: &mut Canon) {}
        fn probe(&self, _out: &mut String) {}
    }
    impl Host for NoHost {
        type Arg = NoArg;
        type Fx = ();
        type Msg = ();
        type Elem = u32;
        type Views<'a> = ();
        type Init = NoInit;
    }

    #[test]
    fn the_effect_sink_counts_emissions_per_step() {
        // A sink over a plain buffer: no real host needed to prove the count and the stamp.
        let mut buf: Vec<Stamped<NoHost>> = Vec::new();
        let mut present = Present::new();
        let mut fx = Effects::new(&mut buf, MachineId::Nav, &mut present);
        fx.push(Fx::Log(LogLine("one".into())));
        fx.push(Fx::Nav(NavOp::Pop));
        assert_eq!(fx.emitted(), 2);
        drop(fx);
        assert_eq!(buf.len(), 2);
        assert!(buf.iter().all(|s| s.from == MachineId::Nav));
    }
}
