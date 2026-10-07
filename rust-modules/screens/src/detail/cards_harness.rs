//! The Tier 2 card-conformance harness for Detail (`cards_conformance_tests.rs`): one harness over
//! the real `DetailScreen`, mounted once per card shelf (Related, the member collection, Extras,
//! Cast). Each mount fills only its own shelf, so the page's other sections do not exist and the
//! shelf under test sits where the real flow puts it. A child module of `detail` so it reads the
//! private section geometry and the shelf rows the screen's own tests read; the metadata store is
//! the thread-local one `tests` uses.
use super::tests::{bare_held, test_store, TestHost};
use super::*;
use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::card_row::RowStyle;
use plx_ui::fixture::FixtureMeasure;
use super::cards::Which;

const ENTRY: EntryId = EntryId(7);
pub(crate) struct Harness {
    which: Which,
    rks: Vec<String>,
    screen: DetailScreen,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

fn mount_of(which: Which, n: usize) -> Box<dyn CardHarness> {
    Box::new(Harness::new(which, (0..n).map(|i| format!("r{i}")).collect()))
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { mount_of(Which::Related, n) }
pub(crate) fn mount_collection(n: usize) -> Box<dyn CardHarness> { mount_of(Which::Collection, n) }
pub(crate) fn mount_extras(n: usize) -> Box<dyn CardHarness> { mount_of(Which::Extras, n) }
pub(crate) fn mount_cast(n: usize) -> Box<dyn CardHarness> { mount_of(Which::Cast, n) }

/// The page's `Detail` with only shelf `which` filled, one card per rating key.
fn detail_of(which: Which, rks: &[String]) -> Detail {
    let sid = ServerId::UNSET;
    let movies = || -> Vec<plx_data::pms::PmsMovie> {
        rks.iter().map(|rk| plx_data::pms::PmsMovie { sid, rk: rk.clone(), ..Default::default() }).collect()
    };
    let mut d = Detail { sid, rk: "page".into(), ..Default::default() };
    match which {
        Which::Related => d.related = movies(),
        Which::Collection => {
            d.collection = Some(plx_data::metadata::CollectionShelf {
                title: String::new(),
                section: 1,
                tag: 1,
                members: movies(),
                count: rks.len(),
            });
        }
        Which::Extras => {
            d.extras = rks.iter().map(|rk| plx_data::metadata::Extra { rk: rk.clone(), ..Default::default() }).collect();
        }
        Which::Cast => {
            // a person's identity is its numeric id, so a reorder keeps each headshot's key
            d.cast = rks.iter().map(|rk| plx_data::metadata::Cast {
                tag: rk.clone(),
                id: rk.bytes().fold(1i64, |acc, b| (acc * 31 + i64::from(b)) % 1_000_003) + 1,
                role: String::new(),
                thumb: String::new(),
                tag_key: String::new(),
            }).collect();
        }
    }
    d
}

impl Harness {
    fn new(which: Which, rks: Vec<String>) -> Self {
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail_of(which, &rks)));
        let screen = bare_held(ServerId::UNSET, "page");
        let mut h = Self { which, rks, screen, focus: None, press: 1.0, ms: 0 };
        h.step(ScreenEvent::Mount);
        h
    }

    /// The page section the shelf sits in, and the shelf itself.
    fn section(&self) -> i32 {
        match self.which {
            Which::Related => 3,
            Which::Collection => 7,
            Which::Extras => 6,
            Which::Cast => 4,
        }
    }

    fn shelf(&self) -> &plx_ui::cards::Shelf {
        match self.which {
            Which::Related => &self.screen.related,
            Which::Collection => &self.screen.collection,
            Which::Extras => &self.screen.extras,
            Which::Cast => &self.screen.cast,
        }
    }

    fn style(&self) -> &'static RowStyle { self.which.style() }

    /// The pitch of one card on the strip.
    fn pitch(&self) -> f32 {
        match self.which {
            Which::Cast => cast::SLOT,
            _ => self.style().w + self.style().gap,
        }
    }

    fn local(&self, i: usize) -> Option<u32> {
        match self.which {
            Which::Related => related::elem(i),
            Which::Collection => collection::elem(i),
            Which::Extras => extras::elem(i),
            Which::Cast => cast::elem(i),
        }
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
        (0..self.rks.len()).find(|&i| self.local(i).and_then(|l| self.screen.engine_key(l)) == Some(elem))
    }

    fn top(&self, at: At) -> f32 {
        let meta = test_store().view();
        let d = self.screen.detail(meta).expect("the page detail is installed");
        let vertical = if at == At::Drawn { self.screen.scroll.pos } else { self.screen.scroll_target };
        self.screen.section_top_at(self.section(), d, &FixtureMeasure, at) - vertical
    }

    fn republish(&mut self) {
        plx_data::metadata::set_current_for_test(test_store().state_mut(), Some(detail_of(self.which, &self.rks)));
        self.step(ScreenEvent::StoreChanged(StoreId::Metadata.ord(), 1));
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        (0..self.rks.len()).filter_map(|i| self.local(i).and_then(|l| self.screen.engine_key(l))).collect()
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
    /// What the shelf paints: the tile at the live pop times the live press, under the section's
    /// drawn top.
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let i = self.at(elem)?;
        let focused = self.focus.map(|k| k.elem) == Some(elem);
        let style = self.style();
        let base = card_row::tile_rect(
            i,
            plx_ui::consts::MARGIN_X,
            self.pitch(),
            self.shelf().scroll(),
            self.top(At::Drawn) + related::LABEL_H,
            (style.w, style.h),
        );
        Some(base.scaled(self.scale(elem)? * if focused && press > 0.0 { press } else { 1.0 }))
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        let meta = test_store().view();
        let src = self.screen.cards(self.which, self.screen.detail(meta)?);
        self.shelf().scale_of(&self.cx(), &src, &elem)
    }
    fn focus_scale(&self) -> f32 { self.style().focus_scale }
    fn canon(&self) -> u64 {
        let mut c = Canon::new();
        LogicalState::write(&self.screen, &mut c);
        c.finish()
    }
    fn identity(&self, elem: u32) -> String {
        self.at(elem).map(|i| self.rks[i].clone()).unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> { Some(self.shelf().scroll()) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let want = self.focus.ok_or("nothing focused")?;
        let at = self.at(want.elem).ok_or("the focused card is not on the shelf")?;
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
        let mut fresh = Harness::new(self.which, self.rks.clone());
        fresh.step(ScreenEvent::RestoreMemory(mem));
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<TestHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
