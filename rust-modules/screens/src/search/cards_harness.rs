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
use plx_ui::screen::{By, Focusable, Step};

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
impl crate::registry::ScreenInputsLike for SearchHost {
    fn screen_inputs<'a>(_: &Cx<'a, Self>) -> &'a crate::registry::ScreenInputs { crate::registry::caption_fixture() }
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
    vec![Shelf { epoch: 0, window: Default::default(), kind: Kind::Movie, items: rks.iter().map(|rk| movie(rk)).collect() }]
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
            modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
            press: PressRead { scale: self.press, ..Default::default() },
            focus: FocusRead { current: self.focus, ..Default::default() }, owner: InputOwner::Entry(ENTRY) }
    }

    fn step(&mut self, ev: ScreenEvent<SearchHost>) -> bool {
        let mut present = Present::new();
        let mut out = Vec::new();
        let (_, moving) = plx_machine::motion::report::scoped_motion(|| {
            let cx: Cx<'_, SearchHost> = Cx { views: self.snap.view(), tick: Tick { ms: self.ms, dt_us: 16_667 },
                modal: Default::default(), page_image: Default::default(), hold_hint: Default::default(),
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

/// **The page's elements have ids**: the field, and the Movies result row of a real query, project, are
/// unique, and are found again (`plx_ui::a11y::conformance`). The ids are the stack's own; this page
/// adds no group of its own beyond the sections it hands the stack.
#[test]
fn the_page_answers_accessibility_ids() {
    assert!(!search_conforms().is_empty());
}

/// The Search page over a real query's Movies row, run through the id conformance (`AppArg::Search`;
/// the coverage table calls this, and so does the test above).
pub(crate) fn search_conforms() -> plx_ui::a11y::Namespaces {
    use plx_ui::a11y::conform;
    let mut namespaces = Vec::new();
    let _guard = plx_base::testlock::serial();
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let h = Harness::new((0..8).map(|i| format!("m{i}")).collect());
    namespaces.push(conform::<SearchHost, _>(&h.screen, &h.cx()));
    namespaces
}

/// **The sections are named for what they ARE, and a hit's id carries its ratingKey**: the field is
/// `field.0` and a result row is named by its kind (`movies.card.<i>`, `shows.card.<i>`), so a row
/// is the same name whichever rows stand before it, and a hit's subject is its ratingKey. A page
/// whose Movies row is gone keeps its Shows row at `shows.card.<i>`, which a position-named section
/// (`section.<i>`) would have renamed.
#[test]
fn the_sections_are_named_and_a_hit_names_its_rating_key() {
    let _guard = plx_base::testlock::serial();
    let _policy = crate::registry::captions::PolicyPin::default_setting();
    let mut h = Harness::new((0..2).map(|i| format!("m{i}")).collect());
    let ids = |h: &Harness| plx_ui::a11y::listing::<SearchHost, _>(&h.screen, &h.cx());
    let cards = |h: &Harness| -> Vec<(String, Option<String>)> {
        let all = ids(h);
        assert!(all.iter().all(|a| !a.path.to_string().starts_with("section.")), "a section left unnamed");
        assert!(all.iter().any(|a| a.path.to_string() == "field.0"), "the field is named");
        all.into_iter()
            .filter(|a| a.role == plx_ui::a11y::Role::Card)
            .map(|a| (a.path.to_string(), a.subject.map(|s| s.into_owned())))
            .collect()
    };
    let hit = |path: &str, rk: &str| (path.to_owned(), Some(rk.to_owned()));
    assert_eq!(cards(&h), [hit("movies.card.0", "m0"), hit("movies.card.1", "m1")]);

    h.store.publish_shelves_for_test(vec![Shelf {
        epoch: 0, window: Default::default(), kind: Kind::Show, items: vec![movie("s0"), movie("s1")],
    }]);
    h.snap = h.store.snapshot();
    h.step(ScreenEvent::StoreChanged(StoreId::Search.ord(), 1));
    assert_eq!(cards(&h), [hit("shows.card.0", "s0"), hit("shows.card.1", "s1")]);
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
        let src = self.screen.row_cards(self.snap.view(), row, crate::registry::captions::policy())?;
        let root = frame.painter;
        if let Some((at, shelf)) = self.screen.row_frame(&self.cx(), row) { shelf.record_stops(&mut frame, root, &src, at); }
        frame.stops().iter().find(|s| s.key.elem == elem).map(|s| s.rect)
    }
    fn scale(&self, elem: u32) -> Option<f32> {
        let (row, _) = self.at(elem)?;
        let src = self.screen.row_cards(self.snap.view(), row, crate::registry::captions::policy())?;
        self.screen.row_frame(&self.cx(), row)?.1.scale_of(&self.cx(), &src, &elem)
    }
    fn focus_scale(&self) -> f32 { layout::style(Kind::Movie).focus_scale }
    fn canon(&self) -> u64 { LogicalState::hash(&self.screen) }
    fn identity(&self, elem: u32) -> String {
        let Some((row, col)) = self.at(elem) else { return String::new() };
        match self.snap.view().shelves().get(row).and_then(|s| s.items.get(col)) {
            Some(Item::Media(m)) => m.rk.clone(),
            _ => String::new(),
        }
    }
    fn scroll(&self) -> Option<f32> { self.screen.row_frame(&self.cx(), 0).map(|(_, shelf)| shelf.scroll()) }
    fn scroll_max(&self) -> Option<f32> { self.screen.row_frame(&self.cx(), 0).map(|(_, shelf)| shelf.style().max_scroll(self.cards().len())) }
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
    fn shift(&mut self, by: usize) -> Result<(), &'static str> {
        for k in 0..by { self.rks.insert(0, format!("arrived{k}")); }
        self.republish();
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
