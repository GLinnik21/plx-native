//! The Tier 2 card-conformance harness for Home (`cards_conformance_tests.rs`): the first grid
//! shelf of the real `HomeScreen`, seeded through the same `plx_data::pms` test hooks Home's own
//! tests use (`seed_for_test` mints the rating keys "1".."n"). A child module of `home` so it reads
//! the private geometry and motion helpers the screen's own tests read.
use super::*;
use plx_machine::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::By;

struct HomeHost;
impl Host for HomeHost {
    type Arg = super::super::family::SettingsPage;
    type Fx = AppFx;
    type Msg = super::super::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = HubsView<'a>;
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}
impl HomeLike for HomeHost {
    fn hubs<'a>(cx: &Cx<'a, Self>) -> HubsView<'a> { cx.views }
}

const ENTRY: EntryId = EntryId(7);
const INSTANCE: InstanceId = InstanceId(9);

pub(crate) struct Harness {
    n: usize,
    state: plx_data::pms::PmsState,
    adapter: std::sync::Arc<plx_data::pms::PmsAdapter>,
    snap: plx_data::pms::HubsSnapshot,
    /// Behind a cell because the real `Screen::draw` the stops are read from takes `&mut`.
    screen: std::cell::RefCell<HomeScreen>,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new(n)) }

impl Harness {
    fn new(n: usize) -> Self {
        let mut state = plx_data::pms::PmsState::default();
        let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
        plx_data::pms::seed_for_test(&mut state, &adapter, n, plx_data::pms::HubState::Ready);
        let snap = plx_data::pms::hubs_snapshot(&state);
        let mut h = Self { n, state, adapter, snap, screen: std::cell::RefCell::new(HomeScreen::new(ENTRY, INSTANCE)), focus: None, press: 1.0, ms: 0 };
        let snap = plx_data::pms::hubs_snapshot(&h.state);
        {
            let cx = Cx::<HomeHost> { views: snap.view(), tick: Tick::default(), measure: &FixtureMeasure,
                press: PressRead::default(), focus: FocusRead::default(), owner: InputOwner::Entry(ENTRY) };
            h.screen.get_mut().sync_catalog(&cx);
        }
        h.screen.get_mut().layout_grid();
        h.prime_on_grid();
        h
    }

    /// Home opens on its hero, and a card arrival there first runs the hero-to-grid dive
    /// (`snap_target`), during which `update_grid` pops nothing (`grid_live`). The pop rule is
    /// about a card arrival on a page already showing the grid, so open it the way a restored
    /// page does (`RestoreMemory`, then a `Restore` arrival jumps the dive), then let go: the
    /// page is left on the grid with nothing focused and every card at rest.
    fn prime_on_grid(&mut self) {
        let Some(&first) = self.screen.borrow().rows.first().and_then(|r| r.elems.first()) else { return };
        let mem = Screen::<HomeHost>::memory_at(&*self.screen.borrow(), None);
        self.step(ScreenEvent::RestoreMemory(mem));
        self.focus(first, By::Restore);
        self.tick(30);
        self.focus = None;
        self.tick(240);
    }

    fn cx(&self) -> Cx<'_, HomeHost> {
        Cx { views: self.snap.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn step(&mut self, ev: ScreenEvent<HomeHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = Cx::<HomeHost> { views: self.snap.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(INSTANCE), &mut present);
            Machine::<HomeHost>::step(self.screen.get_mut(), &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }

    fn at(&self, elem: u32) -> Option<(usize, usize)> {
        self.screen.borrow().rows.iter().enumerate().find_map(|(r, row)| row.elems.iter().position(|e| *e == elem).map(|c| (r, c)))
    }

    /// The store changed under the page: re-publish and deliver the event the host sends.
    fn republish(&mut self) {
        self.snap = plx_data::pms::hubs_snapshot(&self.state);
        self.step(ScreenEvent::StoreChanged(StoreId::Hubs.ord(), 1));
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> { self.screen.borrow().rows.first().map(|r| r.elems.clone()).unwrap_or_default() }
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
        match Focusable::<HomeHost>::neighbour(&*self.screen.borrow(), self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<HomeHost>::place(&*self.screen.borrow(), &elem, &self.cx(), at)
    }
    /// What the real `Screen::draw` registers for the card: the stop's rect, read back from the
    /// frame, with the press the harness was given on the focused card.
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let mut cx = self.cx();
        cx.press = PressRead { scale: press, ..Default::default() };
        let mut f = DrawFrame::new(&cx, plx_ui::Painter::recording());
        plx_ui::screen::record_stops_while_recording(|| {
            plx_gfx::gfx::without_frame_clear(|| Screen::<HomeHost>::draw(&mut *self.screen.borrow_mut(), &mut f));
        });
        f.into_stops().into_iter().find(|stop| stop.key.elem == elem).map(|stop| stop.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        let (row, _) = self.at(elem)?;
        let screen = self.screen.borrow();
        screen.grid.shelves.get(row)?.scale_of(&self.cx(), &screen.cards(self.snap.view(), row), &elem)
    }
    fn focus_scale(&self) -> f32 { RowStyle::HOME.focus_scale }
    fn canon(&self) -> u64 { LogicalState::hash(&*self.screen.borrow()) }
    fn identity(&self, elem: u32) -> String {
        let Some((row, col)) = self.at(elem) else { return String::new() };
        self.screen.borrow().item_at(self.snap.view(), row, col).map(|m| m.rk.clone()).unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> { self.screen.borrow().rows.first().map(|_| self.screen.borrow().grid.scroll_x(0)) }
    fn scroll_max(&self) -> Option<f32> { self.screen.borrow().grid.shelves.first().map(|s| s.style().max_scroll(self.cards().len())) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let want = self.focus.ok_or("nothing focused")?;
        let (_, col) = self.at(want.elem).ok_or("the focused card is not on a shelf")?;
        match l {
            // A landing the row can follow: the focused card moves one place along a row that
            // scrolls further than a place (reversing the row would send it past the scroll range,
            // where the shelf's clamp rightly lets the tile move on screen).
            Landing::Reorder => plx_data::pms::rotate_test_shelves_right(&mut self.state),
            // No hook inserts a card; a longer catalog rotated puts the new card (the last minted
            // rk) first and every old card one place later.
            Landing::InsertAbove => {
                plx_data::pms::seed_for_test(&mut self.state, &self.adapter, self.n + 1, plx_data::pms::HubState::Ready);
                plx_data::pms::rotate_test_shelves_right(&mut self.state);
            }
            Landing::RemoveFocused => {
                let rk = self.screen.borrow().item_at(self.snap.view(), 0, col).map(|m| m.rk.clone()).ok_or("no item behind the focus")?;
                plx_data::pms::remove_test_item(&mut self.state, &rk);
            }
        }
        self.republish();
        let now = Focusable::<HomeHost>::reconcile(&*self.screen.borrow(), want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let mem = Screen::<HomeHost>::memory_at(&*self.screen.borrow(), self.focus);
        let mut fresh = Harness::new(self.n);
        fresh.step(ScreenEvent::RestoreMemory(mem));
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<HomeHost>::reconcile(&*fresh.screen.borrow(), want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
