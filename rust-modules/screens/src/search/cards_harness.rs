//! The Tier 2 card-conformance harness for Search (`cards_conformance_tests.rs`): the Movies
//! result shelf of a real query. A child module of `search` so it reads the private row model
//! and `frame` the way the screen's own tests do.
use super::*;
use plx_data::search::view::SearchView;
use plx_data::search::Shelf;
use plx_machine::machine::{FocusRead, Host, PressRead, Tick};
use plx_machine::present::Present;
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::By;

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl plx_ui::screen::ScreenArg for Arg {
    fn chrome(&self) -> plx_machine::machine::Chrome { plx_machine::machine::Chrome::None }
    fn id(&self) -> plx_machine::machine::ScreenId { plx_machine::machine::ScreenId(1) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, _: &Self) -> bool { true }
}

struct SearchHost;
impl Host for SearchHost {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = crate::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = SearchView<'a>;
    type Init = Arg;
    type Memory = PageMemory;
}
impl SearchLike for SearchHost {
    fn search<'a>(cx: &Cx<'a, Self>) -> SearchView<'a> { cx.views }
}

const ENTRY: EntryId = EntryId(64);
const INSTANCE: InstanceId = InstanceId(65);
const QUERY: &str = "conformance";

fn movie(rk: &str) -> Item {
    Item::Media(plx_data::pms::PmsMovie { rk: rk.into(), ..Default::default() })
}
fn movies(rks: &[String]) -> Vec<Shelf> {
    vec![Shelf { kind: Kind::Movie, items: rks.iter().map(|rk| movie(rk)).collect() }]
}

pub(crate) struct Harness {
    store: plx_data::stores::search::SearchStore,
    snap: plx_data::stores::search::SearchSnapshot,
    screen: SearchScreen,
    rks: Vec<String>,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new((0..n).map(|i| format!("m{i}")).collect())) }

impl Harness {
    fn new(rks: Vec<String>) -> Self {
        let mut store = plx_data::stores::search::SearchStore::default();
        store.run(SearchCmd::SetQuery(QUERY.into()));
        store.publish_shelves_for_test(movies(&rks));
        let snap = store.snapshot();
        let mut h = Self { store, snap, screen: SearchScreen::new(ENTRY, INSTANCE), rks, focus: None, press: 1.0, ms: 0 };
        h.step(ScreenEvent::Mount);
        h
    }

    fn cx(&self) -> Cx<'_, SearchHost> {
        Cx { views: self.snap.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn step(&mut self, ev: ScreenEvent<SearchHost>) -> bool {
        let mut present = Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx: Cx<'_, SearchHost> = Cx { views: self.snap.view(), tick: Tick { ms: self.ms, dt_us: 16_667 },
                measure: &FixtureMeasure, press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
            Machine::<SearchHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }

    fn at(&self, elem: u32) -> Option<(usize, usize)> {
        self.screen.rows.iter().enumerate().find_map(|(r, row)| row.elems.iter().position(|e| *e == elem).map(|c| (r, c)))
    }

    fn republish(&mut self) {
        self.store.publish_shelves_for_test(movies(&self.rks));
        self.snap = self.store.snapshot();
        self.step(ScreenEvent::StoreChanged(StoreId::Search.ord(), 1));
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> { self.screen.rows.first().map(|r| r.elems.clone()).unwrap_or_default() }
    fn focused(&self) -> Option<u32> { self.focus.map(|k| k.elem) }
    fn focus(&mut self, elem: u32, by: By) {
        let from = self.focus;
        self.focus = Some(self.key(elem));
        self.step(ScreenEvent::FocusMoved { from, to: self.key(elem), by });
    }
    fn tick(&mut self, frames: u32) -> bool {
        let mut moved = false;
        for _ in 0..frames {
            self.ms += 16;
            moved |= self.step(ScreenEvent::Tick(Tick { ms: self.ms, dt_us: 16_667 }));
        }
        moved
    }
    fn cover(&mut self) { self.step(ScreenEvent::Cover); }
    fn uncover(&mut self) { self.step(ScreenEvent::Uncover); }
    fn set_press(&mut self, scale: f32) { self.press = scale; }
    fn neighbour(&self, elem: u32, dir: Dir) -> Nb {
        match Focusable::<SearchHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<SearchHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    /// The rect the page registers as the stop of `elem` (the shelf's draw registers the rect it
    /// paints), at the live press.
    fn drawn_rect(&self, elem: u32, _press: f32) -> Option<Rect> {
        let (row, _) = self.at(elem)?;
        let cx = self.cx();
        let mut frame = DrawFrame::new(&cx, plx_ui::Painter::root());
        let src = self.screen.cards(self.snap.view(), row)?;
        let root = frame.painter;
        self.screen.rows[row].shelf.record_stops(&mut frame, root, &src, self.screen.frame(row, At::Drawn));
        frame.stops().iter().find(|s| s.key.elem == elem).map(|s| s.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        let (row, _) = self.at(elem)?;
        let src = self.screen.cards(self.snap.view(), row)?;
        self.screen.rows[row].shelf.scale_of(&self.cx(), &src, &elem)
    }
    fn focus_scale(&self) -> f32 { layout::style(Kind::Movie).focus_scale }
    fn canon(&self) -> u64 {
        let mut c = Canon::new();
        LogicalState::write(&self.screen, &mut c);
        c.finish()
    }
    fn identity(&self, elem: u32) -> String {
        let Some((row, col)) = self.at(elem) else { return String::new() };
        match self.snap.view().shelves().get(row).and_then(|s| s.items.get(col)) {
            Some(Item::Media(m)) => m.rk.clone(),
            _ => String::new(),
        }
    }
    fn scroll(&self) -> Option<f32> { self.screen.rows.first().map(|r| r.shelf.scroll()) }
    fn scroll_max(&self) -> Option<f32> { self.screen.rows.first().map(|r| r.shelf.style().max_scroll(self.cards().len())) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let (_, col) = self.focus.and_then(|k| self.at(k.elem)).ok_or("nothing focused")?;
        match l {
            Landing::Reorder => self.rks.swap(0, 2),
            Landing::InsertAbove => self.rks.insert(0, "landed".into()),
            Landing::RemoveFocused => { self.rks.remove(col); }
        }
        self.republish();
        let want = self.focus.unwrap();
        let now = Focusable::<SearchHost>::reconcile(&self.screen, want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let mem = Screen::<SearchHost>::memory_at(&self.screen, self.focus);
        let mut fresh = Harness::new(self.rks.clone());
        fresh.step(ScreenEvent::RestoreMemory(mem));
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<SearchHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
