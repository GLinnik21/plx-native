//! `FixtureHost` — the application bundle the library is compiled and tested against with no
//! Plex type in scope (spec §3.1, §13 phase 2-i): one `FixtureScreen` that is `Composed` of one
//! `Part` (a row of N stops), one `FixtureStore` behind a `StoreOrd`, a `FixtureMeasure`, a stub
//! `Uploader` under a `TexCache<PosterKey>`, and the `Rig` that hands the dispatcher all of it.
//!
//! The smoke test at the bottom drives the dispatcher through boot, a key that opens a page, a
//! store landing and a poster result — one frame each — and is the spike's proof that the
//! contracts COMPOSE. Every §15.1 phase-2 / 3b test is present below as an `#[ignore]`d name so
//! the phase that makes it real only removes the attribute.
#![cfg(test)]

use std::borrow::Cow;
use std::ffi::CStr;

use super::dispatch::{CxParts, Dispatcher, NoTap, Rig, Split};
use super::frame::Budget;
use super::machine::{
    Addr, Canon, Chrome, Cx, Delivery, Effects, Fx, GroupId, Handled, Host, InputEvent, InputKind,
    Key, LogLine, LogicalState, Machine, MachineId, Measure, NavOp, PartId, PosterKey, RequestId,
    ScreenId, StoreOrd, Tick, TimerId,
};
use super::present::{Present, Provenance};
use super::screen::{
    composed_draw, composed_prepare, Activate, At, AxisMask, Composed, Dir, DrawFrame, EdgeRule,
    Focusable, GroupKind, GroupSpec, Hover, Mounter, Part, Placed, RenderStrategy, ReturnState,
    Screen, ScreenArg, ScreenEvent, Seat, Step, Stop,
};
use super::tex::{Decoded, PosterReady, Tex, TexCache, Uploader};
use super::Rect;

use crate::ui::machine::FocusKey;

// ---------------------------------------------------------------------------------------------
// the bundle
// ---------------------------------------------------------------------------------------------

pub struct FixtureHost;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FixtureArg {
    Home,
    Page(u32),
}

impl ScreenArg for FixtureArg {
    fn chrome(&self) -> Chrome {
        match self {
            FixtureArg::Home => Chrome::TabBar,
            FixtureArg::Page(_) => Chrome::None,
        }
    }
    fn id(&self) -> ScreenId {
        match self {
            FixtureArg::Home => ScreenId(1),
            FixtureArg::Page(_) => ScreenId(2),
        }
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, other: &Self) -> bool {
        self == other
    }
}

/// The app's effects: a store command, a network request, a poster request.
pub enum FixtureFx {
    StoreAdd(u32),
    Net(Addr),
    Poster(PosterKey),
}

/// The app's messages.
pub enum FixtureMsg {
    Store(StoreOrd, u32),
    Http { status: u16, blob: Vec<u8> },
    Cache(PosterReady<PosterKey>),
}

/// The read side: the store's PUBLISHED view.
#[derive(Clone, Copy)]
pub struct FixtureViews<'a> {
    pub store: &'a FixtureView,
}

#[derive(Default)]
pub struct FixtureView {
    pub items: Vec<u32>,
    pub gen: u32,
}

#[derive(Default)]
pub struct FixtureInit {
    pub seed: u32,
}

impl LogicalState for FixtureInit {
    fn write(&self, w: &mut Canon) {
        w.u32(self.seed);
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("seed={}", self.seed));
    }
}

impl Host for FixtureHost {
    type Arg = FixtureArg;
    type Fx = FixtureFx;
    type Msg = FixtureMsg;
    type Elem = u32;
    type Views<'a> = FixtureViews<'a>;
    type Init = FixtureInit;
}

// ---------------------------------------------------------------------------------------------
// the store — a machine behind StoreOrd(0); its view is published separately from its state
// ---------------------------------------------------------------------------------------------

#[derive(Default)]
pub struct FixtureStore {
    state: Vec<u32>,
    pub view: FixtureView,
}

impl FixtureStore {
    fn add(&mut self, v: u32) {
        self.state.push(v);
        self.view.items = self.state.clone();
        self.view.gen += 1;
    }
}

// ---------------------------------------------------------------------------------------------
// the part — a row of N stops — and the screen composed of it
// ---------------------------------------------------------------------------------------------

pub struct FixtureRow {
    pub len: usize,
    pub group: GroupId,
    pub entry: super::machine::EntryId,
    pub prepared: u32,
    pub drawn: u32,
}

impl Focusable<FixtureHost> for FixtureRow {
    fn groups(&self, _cx: &Cx<'_, FixtureHost>, out: &mut Vec<GroupSpec>) {
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: Rect::new(0.0, 0.0, 1920.0, 200.0),
            len: self.len,
        });
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, FixtureHost>) -> Step<u32> {
        let i = key.elem as usize;
        match dir {
            Dir::Left if i > 0 => Step::Move(FocusKey {
                entry: key.entry,
                elem: key.elem - 1,
            }),
            Dir::Right if i + 1 < self.len => Step::Move(FocusKey {
                entry: key.entry,
                elem: key.elem + 1,
            }),
            _ => Step::Edge,
        }
    }
    fn place(&self, key: &u32, _cx: &Cx<'_, FixtureHost>, _at: At) -> Option<Placed> {
        if (*key as usize) < self.len {
            let r = Rect::new(*key as f32 * 200.0, 0.0, 180.0, 180.0);
            Some(Placed {
                rect: r,
                rest_rect: r,
                clip: Rect::FULL,
            })
        } else {
            None
        }
    }
    fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
        FocusKey {
            entry: want.entry,
            elem: want.elem.min(self.len.saturating_sub(1) as u32),
        }
    }
    fn seat(&self, _g: GroupId, from: Placed, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
        let col = ((from.rect.x + from.rect.w * 0.5) / 200.0).floor().max(0.0) as u32;
        FocusKey {
            entry: self.entry,
            elem: col.min(self.len.saturating_sub(1) as u32),
        }
    }
}

impl Part<FixtureHost> for FixtureRow {
    fn prepare(&mut self, _b: &mut Budget, _cx: &Cx<'_, FixtureHost>) {
        self.prepared += 1;
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, FixtureHost>, rect: Rect) {
        self.drawn += 1;
        for i in 0..self.len as u32 {
            let r = Rect::new(rect.x + i as f32 * 200.0, rect.y, 180.0, 180.0);
            f.stop(Stop {
                key: FocusKey {
                    entry: self.entry,
                    elem: i,
                },
                rect: r,
                rest_rect: r,
                clip: rect,
                hover: Hover::Focus,
                activate: Activate::Press,
            });
        }
    }
}

/// The screen's logical state: what it heard, hashed.
#[derive(Default)]
pub struct FixtureState {
    pub events: Vec<&'static str>,
    pub keys: u32,
    pub items_seen: u32,
    /// The HTTP statuses that landed, in arrival order.
    pub statuses: Vec<u16>,
}

impl FixtureState {
    /// The shape census (spec §5.4): field names and types, in order. A new field is a new shape.
    pub const SHAPE: &'static str = "FixtureState{events:[str],keys:u32,items_seen:u32,statuses:[u16]}";
}

/// The bundle's state fingerprint: every `LogicalState` shape it carries, in a fixed order.
pub fn fixture_state_fp() -> u64 {
    super::rec::state_fp(&[FixtureState::SHAPE, "FixtureInit{seed:u32}"])
}

impl LogicalState for FixtureState {
    fn write(&self, w: &mut Canon) {
        w.seq(self.events.len());
        for e in &self.events {
            w.str(e);
        }
        w.u32(self.keys).u32(self.items_seen);
        w.seq(self.statuses.len());
        for st in &self.statuses {
            w.u32(*st as u32);
        }
    }
    fn probe(&self, out: &mut String) {
        out.push_str(&format!("events={:?} keys={} statuses={:?}", self.events, self.keys, self.statuses));
    }
}

pub struct FixtureScreen {
    pub arg: FixtureArg,
    pub state: FixtureState,
    row: FixtureRow,
}

impl Composed<FixtureHost> for FixtureScreen {
    fn layout(&self, _cx: &Cx<'_, FixtureHost>) -> Vec<(PartId, Rect)> {
        vec![(PartId(0), Rect::new(0.0, 100.0, 1920.0, 200.0))]
    }
    fn part(&self, _id: PartId) -> &dyn Part<FixtureHost> {
        &self.row
    }
    fn part_mut(&mut self, _id: PartId) -> &mut dyn Part<FixtureHost> {
        &mut self.row
    }
}

impl Machine<FixtureHost> for FixtureScreen {
    type Ev = ScreenEvent<FixtureHost>;
    fn step(
        &mut self,
        ev: &Self::Ev,
        cx: &Cx<'_, FixtureHost>,
        fx: &mut Effects<'_, FixtureHost>,
    ) -> Handled {
        self.state.events.push(ev.name());
        match ev {
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Ok, .. },
                ..
            }) => {
                // OK on Home opens a page: the structural op that must mount THIS frame
                self.state.keys += 1;
                let next = match self.arg {
                    FixtureArg::Home => self.state.keys,
                    FixtureArg::Page(n) => n + 1,
                };
                fx.push(Fx::Nav(NavOp::Push(FixtureArg::Page(next))));
                fx.invalidate(Provenance::Input);
                Handled::Yes
            }
            ScreenEvent::Input(InputEvent {
                kind: InputKind::Key { key: Key::Back, .. },
                ..
            }) => {
                fx.push(Fx::Nav(NavOp::Pop));
                Handled::Yes
            }
            ScreenEvent::Input(_) => Handled::No,
            ScreenEvent::Mount => {
                // a request emitted from mount is addressable: the id arrived first
                fx.push(Fx::App(FixtureFx::Net(Addr {
                    to: fx.from(),
                    req: RequestId(1),
                })));
                fx.push(Fx::Log(LogLine(format!("fixture: mounted {:?}", self.arg))));
                Handled::Yes
            }
            ScreenEvent::StoreChanged(_, _) => {
                self.state.items_seen = cx.views.store.items.len() as u32;
                self.row.len = self.state.items_seen as usize;
                fx.invalidate(Provenance::Landing(fx.from()));
                Handled::Yes
            }
            ScreenEvent::Async(_, FixtureMsg::Http { status, .. }) => {
                self.state.statuses.push(*status);
                if *status == 200 {
                    fx.push(Fx::App(FixtureFx::StoreAdd(7)));
                }
                Handled::Yes
            }
            ScreenEvent::Enter(_) if self.arg == FixtureArg::Page(2) => {
                // a structural op emitted from Enter: parked for the NEXT frame's commit (§3.3)
                fx.push(Fx::Nav(NavOp::Push(FixtureArg::Page(3))));
                Handled::Yes
            }
            _ => Handled::No,
        }
    }
}

impl Screen<FixtureHost> for FixtureScreen {
    fn name(&self) -> &'static str {
        match self.arg {
            FixtureArg::Home => "home",
            FixtureArg::Page(_) => "detail",
        }
    }
    fn state(&self) -> &dyn LogicalState {
        &self.state
    }
    fn crumb(&self, _cx: &Cx<'_, FixtureHost>) -> Option<Cow<'_, str>> {
        None
    }
    fn prepare(&mut self, b: &mut Budget, cx: &Cx<'_, FixtureHost>) {
        composed_prepare(self, b, cx);
    }
    fn draw(&mut self, f: &mut DrawFrame<'_, FixtureHost>) {
        composed_draw(self, f);
    }
    fn render(&self) -> RenderStrategy {
        RenderStrategy::Page
    }
}

// ---------------------------------------------------------------------------------------------
// the rig: mounter, store, measure, cache, adapters, the privileged hooks
// ---------------------------------------------------------------------------------------------

pub struct FixtureMeasure;

impl Measure for FixtureMeasure {
    fn width(&self, s: &CStr, sz: i32, _bold: bool) -> f32 {
        s.to_bytes().len() as f32 * sz as f32 * 0.5
    }
    fn cap_h(&self, sz: i32) -> f32 {
        sz as f32 * 0.7
    }
    fn line_h(&self, sz: i32) -> f32 {
        sz as f32 * 1.2
    }
}

pub struct StubUploader {
    next: u32,
}

impl Uploader for StubUploader {
    fn upload(&mut self, d: &Decoded) -> Tex {
        self.next += 1;
        Tex {
            id: self.next,
            w: d.w,
            h: d.h,
        }
    }
    fn warm(&mut self, _t: Tex) {}
    fn free(&mut self, _t: Tex) {}
}

struct FixtureMounter {
    mounted: u32,
}

impl Mounter<FixtureHost> for FixtureMounter {
    fn mount(
        &mut self,
        _id: super::machine::InstanceId,
        arg: &FixtureArg,
        _ret: &ReturnState<u32>,
        cx: &Cx<'_, FixtureHost>,
        _fx: &mut Effects<'_, FixtureHost>,
    ) -> Box<dyn Screen<FixtureHost>> {
        self.mounted += 1;
        let entry = match cx.owner {
            super::machine::InputOwner::Entry(e) => e,
            _ => super::machine::EntryId(0),
        };
        Box::new(FixtureScreen {
            arg: arg.clone(),
            state: FixtureState::default(),
            row: FixtureRow {
                len: cx.views.store.items.len(),
                group: GroupId(1),
                entry,
                prepared: 0,
                drawn: 0,
            },
        })
    }
}

pub struct FixtureRig {
    mounter: FixtureMounter,
    store: FixtureStore,
    measure: FixtureMeasure,
    pub cache: TexCache<PosterKey>,
    uploader: StubUploader,
    pub log: Vec<String>,
    pub net_requests: Vec<Addr>,
    pub ls2_pumps: u32,
    pub opaque_route_calls: Vec<bool>,
    pub clears: u32,
    us: u64,
}

impl FixtureRig {
    pub fn new() -> Self {
        Self {
            mounter: FixtureMounter { mounted: 0 },
            store: FixtureStore::default(),
            measure: FixtureMeasure,
            cache: TexCache::new(8),
            uploader: StubUploader { next: 0 },
            log: Vec::new(),
            net_requests: Vec::new(),
            ls2_pumps: 0,
            opaque_route_calls: Vec::new(),
            clears: 0,
            us: 0,
        }
    }
}

impl Rig<FixtureHost> for FixtureRig {
    fn split(&mut self) -> Split<'_, FixtureHost> {
        Split {
            mounter: &mut self.mounter,
            views: FixtureViews {
                store: &self.store.view,
            },
            measure: &self.measure,
        }
    }

    fn deliver(
        &mut self,
        to: MachineId,
        msg: &FixtureMsg,
        _parts: &CxParts<u32>,
        fx: &mut Effects<'_, FixtureHost>,
    ) -> Handled {
        match (to, msg) {
            (MachineId::Store(StoreOrd(0)), FixtureMsg::Store(_, v)) => {
                self.store.add(*v);
                // the notice, not the payload (§3.4): every live instance hears it
                fx.push(Fx::App(FixtureFx::StoreAdd(u32::MAX))); // sentinel: "broadcast gen"
                Handled::Yes
            }
            (MachineId::Cache, FixtureMsg::Cache(_)) => Handled::No, // handled in app_fx path below
            _ => Handled::No,
        }
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
        from: MachineId,
        fx: FixtureFx,
        parts: &CxParts<u32>,
        out: &mut Effects<'_, FixtureHost>,
    ) {
        // the rig forwards to its adapter set — the one door (`ui::adapters`)
        super::adapters::Adapters::execute(self, from, fx, parts, out);
    }

    fn log(&mut self, line: &str) {
        self.log.push(line.to_string());
    }

    fn prepare(&mut self, b: &mut Budget, present: &mut Present) {
        let mut ph = super::machine::PresentHandle(present);
        let us = self.us;
        self.cache.prepare(b, &mut self.uploader, &mut ph, || us);
    }

    fn ls2_pump(&mut self) {
        self.ls2_pumps += 1;
    }

    fn opaque_route(&mut self, bound: bool) {
        self.opaque_route_calls.push(bound);
    }

    fn clear_opaque_region(&mut self) {
        self.clears += 1;
    }

    fn now_us(&self) -> u64 {
        self.us
    }
}

/// The fixture's adapters ARE the rig: it holds the resources a real adapter set would (the
/// request log, the texture cache) and answers each effect synchronously.
impl super::adapters::Adapters<FixtureHost> for FixtureRig {
    fn execute(
        &mut self,
        _from: MachineId,
        fx: FixtureFx,
        _parts: &CxParts<u32>,
        out: &mut Effects<'_, FixtureHost>,
    ) {
        match fx {
            FixtureFx::StoreAdd(u32::MAX) => {
                // the store's generation moved: notify — the rig has no instance list, so the
                // test delivers `StoreChanged` through `Dispatcher::frame`'s results (the
                // dispatcher's broadcast is phase 2's `Event::Store{gen}` table row)
            }
            FixtureFx::StoreAdd(v) => {
                out.push(Fx::Deliver(
                    MachineId::Store(StoreOrd(0)),
                    Delivery::Machine(FixtureMsg::Store(StoreOrd(0), v)),
                ));
            }
            FixtureFx::Net(addr) => self.net_requests.push(addr),
            FixtureFx::Poster(key) => self.cache.accept(PosterReady {
                key,
                result: Ok(Decoded {
                    w: 4,
                    h: 4,
                    rgba: vec![0; 64].into_boxed_slice(),
                }),
            }),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the smoke test — the spike's proof that the contracts compose
// ---------------------------------------------------------------------------------------------

fn tick(ms: u32) -> Tick {
    Tick { ms, dt_us: 16_000 }
}

fn key(k: Key, at: Tick) -> InputEvent<u32> {
    InputEvent {
        at,
        source: super::machine::Source::Script,
        kind: InputKind::Key {
            key: k,
            sym: 0,
            wcode: 0,
            edge: super::machine::Edge::Down,
            at_edge: false,
        },
    }
}

#[test]
fn the_spike_composes_boot_a_key_a_landing_and_a_poster_over_four_frames() {
    let _g = crate::testlock::serial();
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    let mut rig = FixtureRig::new();

    // frame 1: boot — Root(Home) parked, committed, mounted, entered; presented (first frame)
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    let r1 = d.frame(&mut rig, tick(0), vec![], vec![], &mut NoTap);
    assert!(r1.presented);
    assert_eq!(r1.mounted.len(), 1, "Home mounted at NAV COMMIT");
    assert_eq!(rig.ls2_pumps, 1);
    assert_eq!(rig.opaque_route_calls, vec![false], "opaque_route runs every frame");
    assert_eq!(rig.clears, 1, "clear_opaque_region at draw entry");
    assert_eq!(rig.net_requests.len(), 1, "a request emitted from Mount is addressable");
    assert!(rig.log.iter().any(|l| l.contains("mounted Home")));
    let home = d.nav.top().and_then(|e| e.inst.as_ref()).unwrap().id;
    d.track_inflight(home, RequestId(1));

    // frame 2: an HTTP result lands on Home; nothing moved on screen, so no present unless damaged
    let addr = Addr {
        to: MachineId::Instance(home),
        req: RequestId(1),
    };
    let r2 = d.frame(
        &mut rig,
        tick(16),
        vec![],
        vec![(
            addr,
            FixtureMsg::Http {
                status: 200,
                blob: vec![],
            },
        )], &mut NoTap);
    assert_eq!(r2.dropped_deliveries, 0);
    assert!(!r2.presented, "an Async that damaged nothing does not present");
    assert_eq!(rig.store.view.items, vec![7], "Home asked the store to add; the store stepped in the same drain");

    // frame 3: the store notice reaches Home (phase 2's broadcast, spelled by the test) and OK
    // opens a page — the page mounts THIS frame (a_key_that_opens_a_page_mounts_in_the_same_frame)
    let notice = (
        Addr {
            to: MachineId::Instance(home),
            req: RequestId(0),
        },
        FixtureMsg::Store(StoreOrd(0), 1),
    );
    d.track_inflight(home, RequestId(0));
    let r3 = d.frame(&mut rig, tick(32), vec![key(Key::Ok, tick(32))], vec![notice], &mut NoTap);
    assert_eq!(r3.mounted.len(), 1, "the page mounted in the same frame as the key");
    assert!(r3.presented, "the key invalidated");
    assert_eq!(d.nav.stack.len(), 2);
    assert_eq!(d.nav.top().unwrap().arg, FixtureArg::Page(1));
    assert!(r3.steps_post > 0, "the post-commit drain ran on its own budget");
    let page = d.nav.top().and_then(|e| e.inst.as_ref()).unwrap();
    assert_eq!(page.screen.name(), "detail");
    assert_eq!(d.last_stops().len(), 1, "the page drew one stop: the store has one item");

    // frame 4: a poster arrives as an app effect and is accepted, then uploaded in PREPARE
    // (a_poster_result_is_accepted_in_the_drain_and_uploaded_in_prepare — the spike's half)
    d.request(MachineId::Nav, NavOp::Cancel); // a withdrawn transition: nothing mounted
    // the app's poster adapter delivers to `MachineId::Cache`; the rig's `accept` is that path
    rig.cache.accept(PosterReady {
        key: PosterKey(9),
        result: Ok(Decoded {
            w: 4,
            h: 4,
            rgba: vec![0; 64].into_boxed_slice(),
        }),
    });
    d.budget.note_queued(rig.cache.has_pending());
    let r4 = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert!(r4.presented, "queued prepare work forces a present");
    assert!(rig.cache.resolve(PosterKey(9)).is_some(), "uploaded in prepare");
    assert!(!rig.cache.has_pending());
    d.budget.note_queued(false);

    // frame 5: BACK pops the page: WillLeave → Unmount → Uncover → Enter(Restored) on Home
    let r5 = d.frame(&mut rig, tick(64), vec![key(Key::Back, tick(64))], vec![], &mut NoTap);
    assert_eq!(r5.unmounted.len(), 1);
    d.prune(&r5.unmounted);
    assert_eq!(d.nav.stack.len(), 1);
    let home_inst = d.nav.top().and_then(|e| e.inst.as_ref()).unwrap();
    let mut probe = String::new();
    home_inst.screen.state().probe(&mut probe);
    assert!(probe.contains("\"uncover\", \"enter\""), "{probe}");
    assert_ne!(home_inst.screen.state().hash(), 0);
}

// ---------------------------------------------------------------------------------------------
// the recorder tap and the replay codec for this bundle
// ---------------------------------------------------------------------------------------------

use super::dispatch::Tap;
use super::rec::{Header, MemSink, Recording, Writer};
use super::replay::{run_targets, Codec};
use serde_json::{json, Value};

pub struct RecTap {
    pub w: Writer,
}

fn key_name(k: Key) -> &'static str {
    match k {
        Key::Up => "up",
        Key::Down => "down",
        Key::Left => "left",
        Key::Right => "right",
        Key::Ok => "ok",
        Key::Back => "back",
        Key::Other => "other",
    }
}

impl Tap<FixtureHost> for RecTap {
    fn tick(&mut self, f: u64, t: Tick) {
        self.w.tick(f, t);
    }
    fn input(&mut self, f: u64, ev: &InputEvent<u32>) {
        if let InputKind::Key { key, .. } = ev.kind {
            self.w.input(f, json!({"kind": "key", "key": key_name(key), "ms": ev.at.ms}));
        }
    }
    fn result(&mut self, f: u64, addr: &Addr, msg: &FixtureMsg) {
        let (to, payload) = match (addr.to, msg) {
            (MachineId::Instance(i), FixtureMsg::Http { status, .. }) => {
                (format!("inst:{}", i.0), json!({"kind": "http", "status": status}))
            }
            (MachineId::Instance(i), FixtureMsg::Store(o, v)) => {
                (format!("inst:{}", i.0), json!({"kind": "store", "ord": o.0, "v": v}))
            }
            _ => return,
        };
        self.w.result(f, &to, addr.req.0, payload);
    }
    fn effect(&mut self, f: u64, s: &super::machine::Stamped<FixtureHost>) {
        let e = match &s.fx {
            Fx::Nav(_) => "nav",
            Fx::Mount(_) => "mount",
            Fx::Unmount(_) => "unmount",
            Fx::Deliver(..) => "deliver",
            Fx::Timer { .. } => "timer",
            Fx::CancelTimer(_) => "cancel_timer",
            Fx::Press(_) => "press",
            Fx::Log(_) => "log",
            Fx::App(_) => "app",
        };
        self.w.effect(f, &format!("{:?}", s.from), e, None);
    }
    fn present(&mut self, f: u64, bit: bool, why: Option<Provenance>) {
        let why = why.map(|p| format!("{p:?}"));
        self.w.present(f, bit, why.as_deref());
    }
    fn state(&mut self, f: u64, hash: u64) {
        self.w.state(f, hash);
    }
    fn frame_done(&mut self, _f: u64) {
        self.w.flush_frame().expect("the memory sink never fails");
    }
}

pub struct FixtureCodec;

impl Codec<FixtureHost> for FixtureCodec {
    fn decode_input(&self, v: &Value) -> Option<InputEvent<u32>> {
        let k = match v["key"].as_str()? {
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "ok" => Key::Ok,
            "back" => Key::Back,
            _ => Key::Other,
        };
        Some(key(k, tick(v["ms"].as_u64()? as u32)))
    }
    fn decode_result(&self, v: &Value) -> Option<(Addr, FixtureMsg)> {
        let to = v["to"].as_str()?;
        let inst = to.strip_prefix("inst:")?.parse::<u32>().ok()?;
        let addr = Addr {
            to: MachineId::Instance(super::machine::InstanceId(inst)),
            req: RequestId(v["req"].as_u64()? as u32),
        };
        let p = &v["payload"];
        let msg = match p["kind"].as_str()? {
            "http" => FixtureMsg::Http {
                status: p["status"].as_u64()? as u16,
                blob: vec![],
            },
            "store" => FixtureMsg::Store(StoreOrd(p["ord"].as_u64()? as u32), p["v"].as_u64()? as u32),
            _ => return None,
        };
        Some((addr, msg))
    }
}

/// The scenario every recorder test drives: boot, an HTTP landing, a store notice + OK, BACK.
fn drive(d: &mut Dispatcher<FixtureHost>, rig: &mut FixtureRig, tap: &mut dyn Tap<FixtureHost>) {
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    d.frame(rig, tick(0), vec![], vec![], tap);
    let home = d.nav.top().and_then(|e| e.inst.as_ref()).unwrap().id;
    d.track_inflight(home, RequestId(1));
    d.track_inflight(home, RequestId(0));
    let addr = |req: u32| Addr {
        to: MachineId::Instance(home),
        req: RequestId(req),
    };
    d.frame(
        rig,
        tick(16),
        vec![],
        vec![(addr(1), FixtureMsg::Http { status: 200, blob: vec![] })],
        tap,
    );
    d.frame(
        rig,
        tick(32),
        vec![key(Key::Ok, tick(32))],
        vec![(addr(0), FixtureMsg::Store(StoreOrd(0), 1))],
        tap,
    );
    d.frame(rig, tick(48), vec![], vec![], tap);
    let r = d.frame(rig, tick(64), vec![key(Key::Back, tick(64))], vec![], tap);
    d.prune(&r.unmounted);
}

fn record() -> Recording {
    let sink = MemSink::default();
    let segs = sink.segments.clone();
    let header = Header::new(fixture_state_fp(), &FixtureInit { seed: 1 });
    let w = Writer::open(Box::new(sink), &header, 0).unwrap();
    let mut tap = RecTap { w };
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    let mut rig = FixtureRig::new();
    drive(&mut d, &mut rig, &mut tap);
    tap.w.finish();
    let manifest = serde_json::to_string(&json!({
        "schema": super::rec::SCHEMA, "state_fp": fixture_state_fp(),
        "init": {"probe": "seed=1", "hash": FixtureInit { seed: 1 }.hash()}
    }))
    .unwrap();
    let s = segs.borrow();
    let refs: Vec<&[u8]> = s.iter().map(|v| v.as_slice()).collect();
    Recording::parse(&manifest, &refs, fixture_state_fp()).unwrap()
}

/// Replay `rec` the way the product driver will: the replay re-registers the inflight requests
/// the scenario minted (the app's registry does that from `Fx::App` in phase 2's registry).
fn replay(rec: &Recording, codec: &dyn Codec<FixtureHost>) -> super::replay::Report {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    // the first frame mounts Home; the replay driver runs it, then the test registers the two
    // requests the recorded scenario tracked before feeding the rest
    let first = &rec.frames[..1];
    let rest = &rec.frames[1..];
    let head = Recording {
        header: rec.header.clone(),
        frames: first.to_vec(),
        metrics: Default::default(),
        stopped_at: None,
    };
    let r0 = run_targets(&head, codec, &mut d, &mut rig, &|| None);
    let home = d.nav.top().and_then(|e| e.inst.as_ref()).unwrap().id;
    d.track_inflight(home, RequestId(1));
    d.track_inflight(home, RequestId(0));
    let tail = Recording {
        header: rec.header.clone(),
        frames: rest.to_vec(),
        metrics: Default::default(),
        stopped_at: None,
    };
    let mut r = run_targets(&tail, codec, &mut d, &mut rig, &|| None);
    r.frames += r0.frames;
    r.graded += r0.graded;
    r.divergences.splice(0..0, r0.divergences);
    r.present_diffs.splice(0..0, r0.present_diffs);
    r
}

#[test]
fn a_sim_recording_replays_to_the_same_state_hash_stream() {
    // "sim" in the spec's sense: the recording is taken by the tap, not typed by hand; the product
    // fixture recorded on the simulator is the same path with the product codec.
    let rec = record();
    assert!(rec.state_stream().len() >= 4, "every event frame carries an st record");
    let report = replay(&rec, &FixtureCodec);
    assert!(report.is_clean(), "{:?}", report.safe_lines());
    assert_eq!(report.graded as usize, rec.state_stream().len());

    // A codec that loses the HTTP landing is a different application: pointwise divergences from
    // the frame it first mattered, and replay CONTINUES.
    struct Lossy;
    impl Codec<FixtureHost> for Lossy {
        fn decode_input(&self, v: &Value) -> Option<InputEvent<u32>> {
            FixtureCodec.decode_input(v)
        }
        fn decode_result(&self, v: &Value) -> Option<(Addr, FixtureMsg)> {
            let r = FixtureCodec.decode_result(v)?;
            matches!(r.1, FixtureMsg::Store(..)).then_some(r)
        }
    }
    let report = replay(&rec, &Lossy);
    assert!(!report.is_clean());
    assert_eq!(report.divergences[0].frame, 2, "the landing frame is the first to diverge");
    assert!(report.frames == rec.frames.len() as u64, "replay continued past the divergence");
    assert!(report.safe_lines()[0].starts_with("diverge f=2 expected=0x"));
}

#[test]
fn the_present_bit_is_recorded_and_replayed() {
    let rec = record();
    let bits: Vec<Option<bool>> = rec.frames.iter().map(|f| f.present).collect();
    assert_eq!(bits[0], Some(true), "boot presents");
    assert_eq!(bits[1], Some(false), "a landing that damaged nothing does not");
    assert_eq!(bits[2], Some(true), "the key did");
    assert_eq!(rec.frames[2].present_why.as_deref(), Some("Input"));
    let report = replay(&rec, &FixtureCodec);
    assert!(report.present_diffs.is_empty());
}

#[test]
fn the_fixture_bundles_state_shape_is_pinned() {
    // Re-pin only with a named reason: a shape change invalidates every fixture of this bundle.
    assert_eq!(fixture_state_fp(), 0x7fa4_0b1a_c484_3abf);
}

fn booted() -> (Dispatcher<FixtureHost>, FixtureRig, super::machine::InstanceId) {
    let mut d: Dispatcher<FixtureHost> = Dispatcher::new();
    let mut rig = FixtureRig::new();
    d.request(MachineId::Nav, NavOp::Root(FixtureArg::Home));
    d.frame(&mut rig, tick(0), vec![], vec![], &mut NoTap);
    let home = d.nav.top().and_then(|e| e.inst.as_ref()).unwrap().id;
    (d, rig, home)
}

fn events_of(d: &Dispatcher<FixtureHost>, idx: usize) -> String {
    let mut s = String::new();
    d.nav.stack[idx].inst.as_ref().unwrap().screen.state().probe(&mut s);
    s
}

#[test]
fn external_events_are_drained_before_effect_results() {
    let (mut d, mut rig, home) = booted();
    d.track_inflight(home, RequestId(1));
    let addr = Addr { to: MachineId::Instance(home), req: RequestId(1) };
    // a direction key Home does not handle, so no page opens and the order stays on one screen
    d.frame(&mut rig, tick(16), vec![key(Key::Down, tick(16))], vec![(addr, FixtureMsg::Http { status: 404, blob: vec![] })], &mut NoTap);
    let ev = events_of(&d, 0);
    let i = ev.find("\"input\"").unwrap();
    let a = ev.find("\"async\"").unwrap();
    let t = ev.rfind("\"tick\"").unwrap();
    assert!(i < a && a < t, "{ev}");
}

#[test]
fn the_tick_is_delivered_after_every_result_and_before_nav_commit() {
    let (mut d, mut rig, home) = booted();
    d.track_inflight(home, RequestId(1));
    let addr = Addr { to: MachineId::Instance(home), req: RequestId(1) };
    let r = d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![(addr, FixtureMsg::Http { status: 404, blob: vec![] })], &mut NoTap);
    assert_eq!(r.mounted.len(), 1);
    let home_ev = events_of(&d, 0);
    let t = home_ev.rfind("\"tick\"").unwrap();
    let wl = home_ev.find("\"will_leave\"").unwrap();
    assert!(t < wl, "the tick precedes the commit's lifecycle: {home_ev}");
    let page_ev = events_of(&d, 1);
    assert!(page_ev.contains("[\"mount\", \"enter\"]"), "{page_ev}");
}

#[test]
fn deliver_executes_in_the_drain_and_mount_at_commit() {
    let (mut d, mut rig, _home) = booted();
    let r = d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert!(r.steps_pre >= 1, "the input's Deliver ran in the pre-commit drain");
    assert_eq!(r.mounted.len(), 1, "the mount happened at commit");
    assert!(r.steps_post >= 2, "Mount and Enter were delivered post-commit");
}

#[test]
fn a_key_that_opens_a_page_mounts_in_the_same_frame() {
    let (mut d, mut rig, _home) = booted();
    let r = d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    assert_eq!(r.mounted.len(), 1);
    assert_eq!(d.nav.stack.len(), 2);
}

#[test]
fn a_nav_emitted_from_enter_commits_next_frame() {
    let (mut d, mut rig, _home) = booted();
    // two OKs: Page(1), then Page(2) — whose Enter pushes Page(3)
    d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], vec![], &mut NoTap);
    let r2 = d.frame(&mut rig, tick(32), vec![key(Key::Ok, tick(32))], vec![], &mut NoTap);
    assert_eq!(r2.mounted.len(), 1, "one commit per frame: Page(3) waits");
    assert_eq!(d.nav.top().unwrap().arg, FixtureArg::Page(2));
    let r3 = d.frame(&mut rig, tick(48), vec![], vec![], &mut NoTap);
    assert_eq!(r3.mounted.len(), 1);
    assert_eq!(d.nav.top().unwrap().arg, FixtureArg::Page(3));
}

#[test]
fn carried_effects_survive_to_the_next_frame() {
    let (mut d, mut rig, home) = booted();
    let n = super::dispatch::MAX_STEPS_PRE + super::dispatch::MAX_STEPS_POST + 40;
    for i in 0..n {
        d.track_inflight(home, RequestId(100 + i));
    }
    let results: Vec<(Addr, FixtureMsg)> = (0..n)
        .map(|i| (Addr { to: MachineId::Instance(home), req: RequestId(100 + i) }, FixtureMsg::Http { status: 404, blob: vec![] }))
        .collect();
    let r1 = d.frame(&mut rig, tick(16), vec![], results, &mut NoTap);
    assert!(r1.carried > 0, "more work than the budget: carried, never dropped");
    let r2 = d.frame(&mut rig, tick(32), vec![], vec![], &mut NoTap);
    assert_eq!(r2.carried, 0);
    let ev = events_of(&d, 0);
    assert_eq!(ev.matches("\"async\"").count() as u32, n, "every result was delivered");
    assert_eq!(r1.dropped_deliveries + r2.dropped_deliveries, 0);
}

#[test]
fn the_post_commit_drain_has_its_own_reserved_budget() {
    let (mut d, mut rig, home) = booted();
    let n = super::dispatch::MAX_STEPS_PRE + super::dispatch::MAX_STEPS_POST + 10;
    for i in 0..n {
        d.track_inflight(home, RequestId(100 + i));
    }
    let results: Vec<(Addr, FixtureMsg)> = (0..n)
        .map(|i| (Addr { to: MachineId::Instance(home), req: RequestId(100 + i) }, FixtureMsg::Http { status: 404, blob: vec![] }))
        .collect();
    // the key is at the head of the queue: its Nav parks before the budget is spent on results
    let r = d.frame(&mut rig, tick(16), vec![key(Key::Ok, tick(16))], results, &mut NoTap);
    assert_eq!(r.steps_pre, super::dispatch::MAX_STEPS_PRE, "the pre-commit budget was spent");
    assert_eq!(r.mounted.len(), 1, "…and the page still mounted this frame");
    assert!(r.steps_post >= 2, "on the reserved post-commit budget");
    assert!(r.carried > 0);
}

#[test]
fn two_results_in_one_frame_replay_in_arrival_order() {
    let (mut d, mut rig, home) = booted();
    d.track_inflight(home, RequestId(1));
    d.track_inflight(home, RequestId(2));
    let addr = |r: u32| Addr { to: MachineId::Instance(home), req: RequestId(r) };
    d.frame(&mut rig, tick(16), vec![], vec![(addr(1), FixtureMsg::Http { status: 404, blob: vec![] }), (addr(2), FixtureMsg::Http { status: 200, blob: vec![] })], &mut NoTap);
    let ev = events_of(&d, 0);
    assert!(ev.contains("statuses=[404, 200]"), "{ev}");
}

#[test]
fn the_adapter_drain_order_is_the_documented_one() {
    let ranks = super::dispatch::ADAPTER_RANKS;
    let documented = ["auth", "pms", "browse", "search", "metadata/season", "person", "play", "metadata/detail", "viewstate", "alt_sources", "poster"];
    assert_eq!(ranks, documented);
    let mut set = std::collections::HashSet::new();
    assert!(ranks.iter().all(|r| set.insert(*r)), "no adapter is named twice");
}

// ---------------------------------------------------------------------------------------------
// §15.1 — the tests written first, present as names; each phase removes its `#[ignore]`
// ---------------------------------------------------------------------------------------------

macro_rules! pending {
    ($phase:literal: $($name:ident),+ $(,)?) => {
        $(
            #[test]
            #[ignore = concat!("spec §15.1: lands in phase ", $phase)]
            fn $name() {
                unreachable!("an ignored placeholder; phase {} makes it real", $phase);
            }
        )+
    };
}

mod phase_2 {
    // `a_worker_wakes_the_present_gate_through_the_one_door` is real in `ui/present.rs`;
    // `a_rebaseline_without_a_divergence_record_is_refused` is real in `tests/test_harness.py`
    // (the rebaseline is `tools/plxnative-rec`'s, so its test is the tool's). The one left is
    // the half that needs stores as machines: dev flags as recorded `Sys` results (phase 4's
    // adapter path); phase 2 records the armed trigger NAMES and refuses a replay whose set
    // differs (`app::recorder::triggers_differ`).
    pending!("2":
        dev_flags_reach_machines_only_as_recorded_sys_results,
    );
}

mod phase_3a {
    pending!("3a":
        a_poster_result_is_accepted_in_the_drain_and_uploaded_in_prepare,
        the_source_pass_registers_no_stops_and_mutates_no_render_cache,
    );
}

mod phase_3b {
    pending!("3b":
        the_render_set_is_checked_over_the_whole_frame,
        slot_to_item_promotion_is_an_explicit_reconcile,
        resolve_mode_reports_every_mismatch_and_continues_from_the_recording,
        prepare_does_not_change_the_logical_state_hash,
        a_modal_foreground_spring_does_not_invalidate_the_host_snapshot,
        a_new_screen_is_unit_tested_with_no_sdl,
        // §7.7
        every_stop_times_every_direction_on_the_fixture_trees,
        down_from_a_row_lands_nearest_by_the_column_near_x_contract,
        bouncing_between_two_rows_is_stable,
        remembered_entry_reveals_an_off_screen_element,
        remembered_near_falls_to_nearest_beyond_its_rows,
        a_seated_door_projects_through_the_container,
        a_grid_hole_is_skipped_along_the_direction_of_travel,
        a_document_scrolls_inside_and_leaves_at_its_ends,
        a_control_that_handles_a_direction_keeps_it_from_the_engine,
        a_link_replaces_the_geometric_answer_for_its_direction_only,
        an_unreachable_axis_group_is_never_a_destination,
        a_modal_scopes_focus_to_its_own_groups,
        the_strip_is_the_containers_group_and_the_page_gates_it,
        reconcile_runs_after_a_landing_and_before_draw,
        the_same_film_keeps_its_key_after_a_shelf_reorder,
        two_fast_presses_resolve_against_the_spring_target,
        a_click_on_a_clipped_stop_hits_only_its_visible_part,
        a_scaled_focused_tile_registers_its_popped_rect,
        a_pointer_press_is_cancelled_when_the_hit_leaves_its_arm,
        a_press_commit_fires_from_tick_with_no_key_up,
        a_bare_element_activates_on_the_down_edge,
        the_system_keyboard_is_an_input_owner,
        a_legacy_page_never_consults_the_map_or_the_engine,
        a_click_on_an_idle_frame_resolves_against_the_last_presented_map,
        // §6 containers
        the_closing_phase_is_stepped_even_when_the_host_is_frozen,
        a_fixture_screen_mounts_with_no_app_change,
        settings_back_walks_its_own_stack_not_the_apps,
        back_off_a_detail_restores_the_spot_captured_at_the_press,
        person_detail_person_is_three_entries,
        library_remembers_its_section_scroll_and_cursor_across_a_detail_push,
        search_keeps_its_query_and_shelves_across_a_result_push,
        switching_profile_drops_every_tab_instance,
        an_evicted_entry_keeps_its_focus_identity_on_remount,
    );
}

mod phase_4 {
    pending!("4":
        the_landing_cap_drops_the_newest_and_hashes_the_count,
        a_full_landing_replies_dropped_and_retires_inflight,
        a_same_rating_key_on_a_different_server_is_skipped,
        a_refused_spawn_lands_a_refusal_event,
    );
}
