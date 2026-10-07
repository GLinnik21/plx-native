//! The Tier 2 card-conformance harness for Detail (`cards_conformance_tests.rs`): the RELATED
//! shelf of the real `DetailScreen`. Detail has four card shelves (related, collection, extras,
//! cast); Related is the representative one because the collection shelf is the same strip under
//! a linked heading (`related::draw_strip`, `collection::rect` delegates to `related::rect`),
//! while extras and cast are different card kinds with their own tile painters and are not
//! driven here. A child module of `detail` so it reads the private section geometry and motion
//! rows the screen's own tests read; the metadata store is the thread-local one `tests` uses.
use super::tests::{bare_held, test_store, TestHost};
use super::*;
use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;

const ENTRY: EntryId = EntryId(7);
const SECTION: i32 = 3;

pub(crate) struct Harness {
    rks: Vec<String>,
    screen: DetailScreen,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new((0..n).map(|i| format!("r{i}")).collect())) }

fn detail_of(rks: &[String]) -> Detail {
    let sid = ServerId::UNSET;
    Detail {
        sid,
        rk: "page".into(),
        related: rks.iter().map(|rk| plx_data::pms::PmsMovie { sid, rk: rk.clone(), ..Default::default() }).collect(),
        ..Default::default()
    }
}

impl Harness {
    fn new(rks: Vec<String>) -> Self {
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail_of(&rks)));
        let screen = bare_held(ServerId::UNSET, "page");
        let mut h = Self { rks, screen, focus: None, press: 1.0, ms: 0 };
        h.step(ScreenEvent::Mount);
        h
    }

    fn cx(&self) -> Cx<'_, TestHost> {
        static MEASURE: FixtureMeasure = FixtureMeasure;
        Cx { views: (), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &MEASURE,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn step(&mut self, ev: ScreenEvent<TestHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            static MEASURE: FixtureMeasure = FixtureMeasure;
            let cx = Cx::<TestHost> { views: (), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &MEASURE,
                press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out, plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(1)), &mut present);
            Machine::<TestHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }

    /// The index of `elem` on the Related shelf.
    fn at(&self, elem: u32) -> Option<usize> {
        (0..self.rks.len()).find(|&i| related::elem(i).and_then(|l| self.screen.engine_key(l)) == Some(elem))
    }

    fn top(&self, at: At) -> f32 {
        let meta = test_store().view();
        let d = self.screen.detail(meta).expect("the page detail is installed");
        let vertical = if at == At::Drawn { self.screen.scroll.pos } else { self.screen.scroll_target };
        self.screen.section_top_at(SECTION, d, &FixtureMeasure, at) - vertical
    }

    fn republish(&mut self) {
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail_of(&self.rks)));
        self.step(ScreenEvent::StoreChanged(StoreId::Metadata.ord(), 1));
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        (0..self.rks.len()).filter_map(|i| related::elem(i).and_then(|l| self.screen.engine_key(l))).collect()
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
        match Focusable::<TestHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<TestHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    /// What `related::draw_focused_in` paints: the tile at the live pop times the live press
    /// (`row.scale(index) * press`), under the section's drawn top.
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let i = self.at(elem)?;
        let focused = self.focus.map(|k| k.elem) == Some(elem);
        let row = &self.screen.related;
        let base = card_row::tile_rect(
            i,
            plx_ui::consts::MARGIN_X,
            RowStyle::HOME.w + RowStyle::HOME.gap,
            row.scroll_x(),
            self.top(At::Drawn) + related::LABEL_H,
            (RowStyle::HOME.w, RowStyle::HOME.h),
        );
        Some(base.scaled(row.scale(i) * if focused && press > 0.0 { press } else { 1.0 }))
    }
    fn scale(&self, elem: u32) -> Option<f32> { Some(self.screen.related.scale(self.at(elem)?)) }
    fn focus_scale(&self) -> f32 { RowStyle::HOME.focus_scale }
    fn canon(&self) -> u64 {
        let mut c = Canon::new();
        LogicalState::write(&self.screen, &mut c);
        c.finish()
    }
    fn identity(&self, elem: u32) -> String {
        self.at(elem).map(|i| self.rks[i].clone()).unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> { Some(self.screen.related.scroll_x()) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let want = self.focus.ok_or("nothing focused")?;
        let at = self.at(want.elem).ok_or("the focused card is not on the Related shelf")?;
        match l {
            Landing::Reorder => self.rks.swap(0, 2),
            Landing::InsertAbove => self.rks.insert(0, "landed".into()),
            Landing::RemoveFocused => { self.rks.remove(at); }
        }
        self.republish();
        let now = Focusable::<TestHost>::reconcile(&self.screen, want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let mem = Screen::<TestHost>::memory_at(&self.screen, self.focus);
        let mut fresh = Harness::new(self.rks.clone());
        fresh.step(ScreenEvent::RestoreMemory(mem));
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<TestHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
