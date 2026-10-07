//! The Tier 2 card-conformance harness for the Collection page (`cards_conformance_tests.rs`). A
//! child module of `collection` so it reads the private geometry the screen's own tests read.
use super::*;
use plx_machine::machine::{FocusKey, FocusRead, Host, InputOwner, PressRead};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;

struct CollectionHost;
impl Host for CollectionHost {
    type Arg = super::super::family::SettingsPage;
    type Fx = AppFx;
    type Msg = super::super::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = plx_data::collection::CollectionView<'a>;
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}
impl CollectionLike for CollectionHost {
    fn collection<'a>(cx: &Cx<'a, Self>) -> plx_data::collection::CollectionView<'a> { cx.views }
}

const ENTRY: EntryId = EntryId(9);

fn item(rk: &str) -> PmsMovie { PmsMovie { rk: rk.into(), title: rk.into(), ..Default::default() } }
fn set() -> CollectionRef {
    CollectionRef { sid: plx_plex::plex::ServerId::UNSET, rk: "50001".into(), sec: 1, tag: 7, name: "Set".into() }
}

pub(crate) struct Harness {
    store: plx_data::stores::collection::CollectionStore,
    screen: CollectionScreen,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
    n: usize,
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new(n)) }

impl Harness {
    fn new(n: usize) -> Self {
        let mut store = plx_data::stores::collection::CollectionStore::default();
        store.run(CollectionCmd::Open { target: CollectionTarget { id: set(), want: PAGE_SIZE } });
        store.install_for_test((0..n).map(|i| item(&format!("m{i}"))).collect(), CollectionStatus::Ready);
        let mut screen = CollectionScreen::new(ENTRY, set());
        screen.sync(store.view().current().unwrap(), &FixtureMeasure);
        Self { store, screen, focus: None, press: 1.0, ms: 0, n }
    }

    fn cx(&self) -> Cx<'_, CollectionHost> {
        Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY) }
    }

    /// Step one event; true when the step reported page motion.
    fn step(&mut self, ev: ScreenEvent<CollectionHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(9)), &mut present);
            Machine::<CollectionHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn index(&self, elem: u32) -> Option<usize> {
        self.screen.item_index(self.store.view().current()?, elem)
    }
    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }
    fn is_focused(&self, elem: u32) -> bool { self.focus.map(|k| k.elem) == Some(elem) }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        let c = self.store.view().current().unwrap();
        (0..c.items.len()).filter_map(|i| self.screen.elem_at(c, i)).collect()
    }
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
        match Focusable::<CollectionHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<CollectionHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        Some(self.screen.card_rect(self.index(elem)?, self.is_focused(elem), press))
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        Some(self.screen.tile_scale(self.index(elem)?, self.is_focused(elem), 1.0))
    }
    fn focus_scale(&self) -> f32 { plx_ui::poster_grid::STYLE.focus_scale }
    fn canon(&self) -> u64 {
        let mut c = Canon::new();
        LogicalState::write(&self.screen, &mut c);
        c.finish()
    }
    fn identity(&self, elem: u32) -> String {
        let c = self.store.view().current().unwrap();
        self.index(elem).map(|i| c.items[i].rk.clone()).unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> { Some(self.screen.scroll.pos) }
    fn columns(&self) -> Option<usize> { Some(plx_ui::poster_grid::COLS) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let focused = self.focus.map(|k| k.elem);
        let at = focused.and_then(|e| self.index(e)).ok_or("nothing focused")?;
        self.store.edit_for_test(|c| match l {
            Landing::Reorder => c.items.swap(0, 2),
            Landing::InsertAbove => c.items.insert(0, item("landed")),
            Landing::RemoveFocused => { c.items.remove(at); }
        });
        self.step(ScreenEvent::StoreChanged(plx_data::stores::StoreId::Collection.ord(), 1));
        let want = self.focus.unwrap();
        let now = Focusable::<CollectionHost>::reconcile(&self.screen, want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let mem = Screen::<CollectionHost>::memory_at(&self.screen, self.focus);
        let mut fresh = Harness::new(self.n);
        fresh.step(ScreenEvent::RestoreMemory(mem));
        fresh.screen.sync(fresh.store.view().current().unwrap(), &FixtureMeasure);
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<CollectionHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
