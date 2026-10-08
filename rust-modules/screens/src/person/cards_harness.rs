//! The Tier 2 card-conformance harness for the Person page (`cards_conformance_tests.rs`): the
//! credit shelves (movies, then shows). A child module of `person` so it reads the screen's
//! private geometry the way its own tests do.
use super::*;
use plx_machine::machine::{FocusKey, FocusRead, Host, InputOwner, PressRead, Tick};
use plx_ui::screen::{Placed, Step};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;

struct PersonHost;
impl Host for PersonHost {
    type Arg = super::super::family::SettingsPage;
    type Fx = AppFx;
    type Msg = super::super::registry::AppMsg;
    type Elem = u32;
    type Views<'a> = plx_data::person::PersonView<'a>;
    type Init = super::super::family::NoInit;
    type Memory = PageMemory;
}
impl PersonLike for PersonHost {
    fn person<'a>(cx: &Cx<'a, Self>) -> plx_data::person::PersonView<'a> { cx.views }
}

const ENTRY: EntryId = EntryId(0);
const SHOWS: usize = 3;

fn item(rk: &str) -> PmsMovie { PmsMovie { sid: ServerId::UNSET, rk: rk.into(), ..Default::default() } }

fn new_screen() -> PersonScreen {
    PersonScreen::new(ENTRY, ServerId::UNSET, "161".to_string(), "5d77682aeb5d26001f1de4b0".to_string(),
        "Idina Menzel".to_string(), String::new())
}

pub(crate) struct Harness {
    store: plx_data::stores::person::PersonStore,
    screen: PersonScreen,
    movies: Vec<String>,
    focus: Option<FocusKey<u32>>,
    press: f32,
    ms: u32,
}

pub(crate) fn mount(n: usize) -> Box<dyn CardHarness> { Box::new(Harness::new((0..n).map(|i| format!("m{i}")).collect())) }

impl Harness {
    fn new(movies: Vec<String>) -> Self {
        let mut store = plx_data::stores::person::PersonStore::default();
        store.run(PersonCmd::Open { sid: ServerId::UNSET, key: "161".into(),
            guid: "5d77682aeb5d26001f1de4b0".into(), name: "Idina Menzel".into(), thumb: String::new() });
        let mut h = Self { store, screen: new_screen(), movies, focus: None, press: 1.0, ms: 0 };
        h.install();
        h
    }

    fn install(&mut self) {
        self.store.install_for_test(self.movies.iter().map(|m| item(m)).collect(),
            (0..SHOWS).map(|i| item(&format!("s{i}"))).collect());
        // A landed credits and profile answer: with no store pump here, the header's facts wait
        // (`facts_pending`, a repaint loop by design until the profile is asked) would otherwise
        // never end.
        self.store.install_credits_for_test(&[("Actor", 9)]);
        self.store.install_profile_for_test();
        let cx: Cx<'_, PersonHost> = Cx { views: self.store.view(), tick: Tick::default(), measure: &FixtureMeasure,
            press: PressRead::default(), focus: FocusRead::default(), owner: InputOwner::Entry(ENTRY) };
        self.screen.page.refresh_store_cache(&cx);
        // the stack lays its sections out on the first event it sees after the content moved
        self.step(ScreenEvent::Cover);
    }

    fn cx(&self) -> Cx<'_, PersonHost> {
        Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn step(&mut self, ev: ScreenEvent<PersonHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::idle::scoped_motion(|| {
            let cx = Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                press: PressRead { scale: self.press, ..Default::default() },
                focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) };
            let mut fx = Effects::new(&mut out,
                plx_machine::machine::MachineId::Instance(plx_machine::machine::InstanceId(0)), &mut present);
            Machine::<PersonHost>::step(&mut self.screen, &ev, &cx, &mut fx);
        });
        moving || present.page_moving()
    }

    fn key(&self, elem: u32) -> FocusKey<u32> { FocusKey { entry: ENTRY, elem } }

    fn at(&self, elem: u32) -> Option<(usize, usize)> {
        match self.screen.page.locate(self.store.view().current()?, elem)? {
            Located::Shelf(kind, col) => Some((kind, col)),
            _ => None,
        }
    }
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        let p = self.store.view().current().unwrap();
        (0..NSHELF).flat_map(|kind| (0..p.shelf(kind).len()).map(move |col| (kind, col)))
            .map(|(kind, col)| self.screen.page.shelf_key(p, kind, col).elem).collect()
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
        match Focusable::<PersonHost>::neighbour(&self.screen, self.key(elem), dir, &self.cx()) {
            Step::Move(k) => Nb::To(k.elem),
            Step::Edge => Nb::Edge,
        }
    }
    fn place(&self, elem: u32, at: At) -> Option<Placed> {
        Focusable::<PersonHost>::place(&self.screen, &elem, &self.cx(), at)
    }
    /// The rect the page registers as the stop of `elem` (the shelf's draw registers the rect it
    /// paints), at the live press.
    fn drawn_rect(&self, elem: u32, _press: f32) -> Option<Rect> {
        self.at(elem)?;
        let cx = self.cx();
        let mut frame = DrawFrame::new(&cx, plx_ui::Painter::root());
        self.screen.stack.view(&self.screen.page).record_stops(&mut frame);
        frame.stops().iter().find(|s| s.key.elem == elem).map(|s| s.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        self.at(elem)?;
        self.screen.stack.view(&self.screen.page).scale_of(&self.cx(), &elem)
    }
    fn focus_scale(&self) -> f32 { SHELF_STYLE.focus_scale }
    fn canon(&self) -> u64 { LogicalState::hash(&self.screen) }
    fn identity(&self, elem: u32) -> String {
        let Some(p) = self.store.view().current() else { return String::new() };
        self.at(elem).and_then(|(kind, col)| p.shelf(kind).get(col)).map(|m| m.rk.clone()).unwrap_or_default()
    }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let focused = self.focus.map(|k| k.elem).and_then(|e| self.at(e)).ok_or("nothing focused")?;
        match l {
            Landing::Reorder => self.movies.swap(0, 2),
            Landing::InsertAbove => self.movies.insert(0, "landed".into()),
            Landing::RemoveFocused => { self.movies.remove(focused.1); }
        }
        self.install();
        self.step(ScreenEvent::StoreChanged(plx_data::stores::StoreId::Person.ord(), 1));
        let want = self.focus.unwrap();
        let now = Focusable::<PersonHost>::reconcile(&self.screen, want, &self.cx());
        if now != want { self.focus(now.elem, By::Reconcile); }
        Ok(())
    }
    fn memory_roundtrip(&mut self) -> Result<Box<dyn CardHarness>, &'static str> {
        let PageMemory::Person(mem) = Screen::<PersonHost>::memory_at(&self.screen, self.focus) else {
            return Err("memory_at did not return PageMemory::Person");
        };
        let mut fresh = Harness::new(self.movies.clone());
        fresh.screen = new_screen();
        fresh.screen.restore(&mem);
        fresh.step(ScreenEvent::RestoreMemory(PageMemory::Person(mem)));
        fresh.install();
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<PersonHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
