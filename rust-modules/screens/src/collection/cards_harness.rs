//! The Tier 2 card-conformance harness for the Collection page (`cards_conformance_tests.rs`). A
//! child module of `collection` so it reads the private geometry the screen's own tests read.
use super::*;
use plx_machine::machine::{FocusKey, FocusRead, Host, InputOwner, PressRead};
use plx_ui::cards::conformance::{CardHarness, Landing, Nb};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::screen::{Placed, Step};

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
impl crate::registry::ScreenInputsLike for CollectionHost {
    fn screen_inputs<'a>(_: &Cx<'a, Self>) -> &'a crate::registry::ScreenInputs { crate::registry::caption_fixture() }
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
        store.run(CollectionCmd::Open { target: CollectionTarget { id: set(), want: PAGE_SIZE } }, &plx_plex::plex::ClientKeys::default());
        store.install_for_test((0..n).map(|i| item(&format!("m{i}"))).collect(), CollectionStatus::Ready);
        let mut screen = CollectionScreen::new(ENTRY, set());
        screen.page.sync(store.view().current().unwrap(), &FixtureMeasure);
        let mut h = Self { store, screen, focus: None, press: 1.0, ms: 0, n };
        // the stack lays its sections out on the first event it sees
        h.step(ScreenEvent::Tick(Tick { ms: 0, dt_us: 16_667 }));
        h
    }

    fn cx(&self) -> Cx<'_, CollectionHost> {
        Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
            press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() },
            owner: InputOwner::Entry(ENTRY) }
    }

    /// Step one event; true when the step reported page motion.
    fn step(&mut self, ev: ScreenEvent<CollectionHost>) -> bool {
        let mut present = plx_machine::present::Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::motion::report::scoped_motion(|| {
            let cx = Cx { views: self.store.view(), tick: Tick { ms: self.ms, dt_us: 16_667 }, measure: &FixtureMeasure,
                modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
                press: PressRead { scale: self.press, owner: self.focus, ..Default::default() },
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
}

/// **The page's elements have ids**: the head, the member grid and the status read-outs it composes on
/// the stack all project, are unique, and are found again (`plx_ui::a11y::conformance`). The ids are the
/// stack's own; this page adds no group of its own beyond the sections it hands the stack.
#[test]
fn the_page_answers_accessibility_ids() {
    assert!(!collection_conforms().is_empty());
}

/// The Collection page over eight members, run through the id conformance (`ContentArg::Collection`;
/// the coverage table calls this, and so does the test above).
pub(crate) fn collection_conforms() -> plx_ui::a11y::Namespaces {
    use plx_ui::a11y::conform;
    let mut namespaces = Vec::new();
    let _guard = plx_base::testlock::serial();
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let h = Harness::new(8);
    namespaces.push(conform::<CollectionHost, _>(&h.screen, &h.cx()));
    namespaces
}

/// **The sections are named, and a member's id carries its ratingKey as the subject**: the member
/// grid is `items.card.<i>` (never `section.<position>.card.<i>`, which moves with the sections),
/// and each card's subject is its member's ratingKey (the id's index is only a place).
#[test]
fn the_sections_are_named_and_a_member_names_its_rating_key() {
    let _guard = plx_base::testlock::serial();
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let h = Harness::new(8);
    let all = plx_ui::a11y::listing::<CollectionHost, _>(&h.screen, &h.cx());
    assert!(all.iter().all(|a| !a.path.to_string().starts_with("section.")), "a section left unnamed");
    let ids: Vec<_> = all.into_iter().filter(|a| a.role == plx_ui::a11y::Role::Card).collect();
    let paths: Vec<String> = ids.iter().map(|a| a.path.to_string()).collect();
    assert_eq!(paths, (0..8).map(|i| format!("items.card.{i}")).collect::<Vec<_>>());
    for (i, a) in ids.iter().enumerate() {
        assert_eq!(a.subject.as_deref(), Some(format!("m{i}").as_str()), "{}", a.path);
    }
    let cx = h.cx();
    let card = plx_ui::a11y::A11yPath::parse_relative("items.card.5").unwrap();
    let cards = CardHarness::cards(&h);
    assert_eq!(Focusable::<CollectionHost>::a11y_find(&h.screen, &card, &cx), Some(cards[5]));
}

impl CardHarness for Harness {
    fn cards(&self) -> Vec<u32> {
        let c = self.store.view().current().unwrap();
        (0..c.shown()).filter_map(|i| self.screen.elem_at(c, i)).collect()
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
    /// The rect the grid registers as `elem`'s stop at `press` (`Grid::record_stops`, which `draw`
    /// ends with and which uses the very rect `draw` paints; painting itself needs the GL context
    /// a host test does not have).
    fn drawn_rect(&self, elem: u32, press: f32) -> Option<Rect> {
        let cx = Cx { press: PressRead { scale: press, owner: self.focus, ..Default::default() }, ..self.cx() };
        let painter = plx_ui::Painter::root();
        let mut f = plx_ui::screen::DrawFrame::new(&cx, painter);
        self.screen.stack.view(&self.screen.page).record_stops(&mut f);
        f.stops().iter().find(|stop| stop.key.elem == elem).map(|stop| stop.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        self.screen.stack.view(&self.screen.page).scale_of(&self.cx(), &elem)
    }
    fn focus_scale(&self) -> f32 { plx_ui::cards::GRID_STYLE.focus_scale }
    fn canon(&self) -> u64 { LogicalState::hash(&self.screen) }
    fn identity(&self, elem: u32) -> String {
        let c = self.store.view().current().unwrap();
        self.index(elem).map(|i| c.item(i).map(|m| m.rk.clone()).unwrap_or_default()).unwrap_or_default()
    }
    fn scroll(&self) -> Option<f32> { Some(self.screen.stack.scroll()) }
    fn columns(&self) -> Option<usize> { Some(plx_ui::cards::GRID_COLS) }
    fn landing(&mut self, l: Landing) -> Result<(), &'static str> {
        let focused = self.focus.map(|k| k.elem);
        let at = focused.and_then(|e| self.index(e)).ok_or("nothing focused")?;
        self.store.edit_for_test(|c| match l {
            Landing::Reorder => c.edit_items_for_test(|v| v.swap(0, 2)),
            Landing::InsertAbove => c.edit_items_for_test(|v| v.insert(0, item("landed"))),
            Landing::RemoveFocused => c.edit_items_for_test(|v| { v.remove(at); }),
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
        fresh.screen.page.sync(fresh.store.view().current().unwrap(), &FixtureMeasure);
        let want = self.focus.ok_or("nothing focused")?;
        let now = Focusable::<CollectionHost>::reconcile(&fresh.screen, want, &fresh.cx());
        fresh.focus(now.elem, By::Restore);
        Ok(Box::new(fresh))
    }
}
