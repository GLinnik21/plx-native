//! The owned Search screen's own contracts: focus over the drawn document, the geometry a
//! pointer resolves against, and the motion each of this instance's springs owes the frame gate.
//!
//! These are the legacy `ui/search/mod.rs` behaviours carried onto the owned screen. What lives
//! here is what a `Focusable`/`Machine` step can answer with a retained publication and a real
//! `FocusEngine`; the input-ORDERING half (the system keyboard's native latch, the shared strip,
//! the dispatcher's press machine) is the Bridge tier's, in `app/search_owned_tests.rs`.
use super::*;
use crate::registry::AppMsg;
use plx_data::search::view::SearchView;
use plx_data::search::{Item, Shelf};
use plx_ui::fixture::FixtureMeasure;
use plx_ui::focus::{FocusEngine, Outcome};
use plx_ui::hit::{HitMap, PointerKind};
use plx_ui::screen::{By, Focusable, Step};
use plx_machine::machine::{FocusRead, Host, InputEvent, PressRead, Source, Stamped, Tick};
use plx_machine::present::Present;
use plx_ui::screen::{Activate, Hover, ScreenArg, Stop};

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> plx_machine::machine::Chrome { plx_machine::machine::Chrome::None }
    fn id(&self) -> plx_machine::machine::ScreenId { plx_machine::machine::ScreenId(1) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, _: &Self) -> bool { true }
}

struct HostFixture;
impl Host for HostFixture {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = SearchView<'a>;
    type Init = Arg;
    type Memory = PageMemory;
}
impl SearchLike for HostFixture {
    fn search<'a>(cx: &Cx<'a, Self>) -> SearchView<'a> { cx.views }
}

const ENTRY: EntryId = EntryId(64);
const INSTANCE: InstanceId = InstanceId(65);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);
const DT_US: u32 = 16_667;

fn tick(i: u32) -> Tick { Tick { ms: i * 16, dt_us: DT_US } }

fn movie(rk: &str) -> Item {
    Item::Media(plx_data::pms::PmsMovie { rk: rk.into(), title: format!("Synthetic {rk}"), ..Default::default() })
}

/// A shelf of `n` synthetic items, addressed so two shelves never share a result identity.
fn shelf(kind: Kind, tag: &str, n: usize) -> Shelf {
    Shelf { window: Default::default(), kind, items: (0..n).map(|i| movie(&format!("{tag}-{i}"))).collect() }
}

struct Fixture {
    store: plx_data::stores::search::SearchStore,
    search: plx_data::stores::search::SearchSnapshot,
    measure: FixtureMeasure,
}

impl Fixture {
    /// A store with no query, no shelves and no remembered terms — a fresh boot.
    fn new() -> Self {
        let store = plx_data::stores::search::SearchStore::default();
        let search = store.snapshot();
        Self { store, search, measure: FixtureMeasure }
    }
    fn query(&mut self, q: &str) -> &mut Self {
        self.store.run(SearchCmd::SetQuery(q.into()));
        self.capture()
    }
    fn shelves(&mut self, shelves: Vec<Shelf>) -> &mut Self {
        self.store.publish_shelves_for_test(shelves);
        self.capture()
    }
    fn capture(&mut self) -> &mut Self {
        self.search = self.store.snapshot();
        self
    }
    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, HostFixture> {
        Cx { views: self.search.view(), tick: Tick::default(), measure: &self.measure,
            focus: FocusRead { current: focus, ..Default::default() },
            press: PressRead::default(), owner: OWNER }
    }
    /// A mounted screen, its content fade already run out so a settled-motion assertion is about
    /// the springs rather than about the page arriving.
    fn screen(&self) -> SearchScreen {
        let mut screen = SearchScreen::new(ENTRY, INSTANCE);
        let field = Some(FocusKey { entry: ENTRY, elem: FIELD });
        deliver(&mut screen, self, field, ScreenEvent::Mount);
        // Settled WITH the field focused, because that is where `Mount` reseats focus: ticking a
        // mount with no focus at all cools the field's own fill and every later assertion would
        // then be about that fade arriving rather than about what the test is asking.
        for i in 0..40 { deliver(&mut screen, self, field, ScreenEvent::Tick(tick(i))); }
        screen
    }
}

fn deliver(screen: &mut SearchScreen, fixture: &Fixture, focus: Option<FocusKey<u32>>,
    event: ScreenEvent<HostFixture>) -> (Handled, Vec<Stamped<HostFixture>>, bool) {
    let cx = fixture.cx(focus);
    let mut out = Vec::new();
    let mut present = Present::new();
    present.take(0); // a fresh gate's own always-dirty first frame is not this event's damage
    let handled = {
        let mut fx = Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present);
        Machine::<HostFixture>::step(screen, &event, &cx, &mut fx)
    };
    (handled, out, present.take(1))
}

/// One frame: the tick every screen owes, graded on whether it asked the gate for a present.
fn frame(screen: &mut SearchScreen, fixture: &Fixture, engine: &FocusEngine<u32>, at: u32) -> bool {
    deliver(screen, fixture, engine.current(OWNER), ScreenEvent::Tick(tick(at))).2
}

/// A key the way the dispatcher hands one to the owning page.
fn key_event(key: Key, sym: u32) -> ScreenEvent<HostFixture> {
    ScreenEvent::Input(InputEvent { at: tick(0), source: Source::Script,
        kind: InputKind::Key { key, sym, wcode: 0, edge: Edge::Down, at_edge: false } })
}

fn text_event(edit: TextEdit) -> ScreenEvent<HostFixture> {
    ScreenEvent::Input(InputEvent { at: tick(0), source: Source::Script, kind: InputKind::Text(edit) })
}

fn links(screen: &SearchScreen) -> Vec<Link> {
    let mut out = Vec::new();
    <SearchScreen as Screen<HostFixture>>::links(screen, &mut out);
    out
}

/// A direction key, through the real focus engine, delivering `FocusMoved` exactly as the
/// dispatcher does. Answers where focus ended up.
fn step_dir(screen: &mut SearchScreen, fixture: &Fixture, engine: &mut FocusEngine<u32>, dir: Dir)
    -> Option<FocusKey<u32>> {
    let links = links(screen);
    let outcome = engine.move_dir(OWNER, screen, &links, dir, &fixture.cx(engine.current(OWNER)));
    if let Outcome::Moved { from, to, by } = outcome {
        deliver(screen, fixture, Some(to), ScreenEvent::FocusMoved { from, to, by });
    }
    engine.current(OWNER)
}

/// The engine seated where a mount seats it: on the field.
fn seated(screen: &SearchScreen, fixture: &Fixture) -> FocusEngine<u32> {
    let mut engine = FocusEngine::new();
    engine.enter(OWNER, screen, FocusTarget::Elem(screen.key(FIELD)), None, &fixture.cx(None));
    engine
}

/// The stop the renderer registers for `elem`, built from the same `Placed` `render::stop` reads.
fn stop_of(screen: &SearchScreen, fixture: &Fixture, focus: Option<FocusKey<u32>>, elem: u32)
    -> Option<Stop<u32>> {
    let cx = fixture.cx(focus);
    let placed = <SearchScreen as Focusable<HostFixture>>::place(screen, &elem, &cx, At::Drawn)?;
    Some(Stop { key: screen.key(elem), rect: placed.rect, rest_rect: placed.rest_rect,
        clip: placed.clip, hover: Hover::Focus, activate: Activate::Press })
}

/// What a pointer resolves to over the drawn document — the production map, filled with the
/// production stops.
fn hit(screen: &SearchScreen, fixture: &Fixture, elems: &[u32], x: f32, y: f32) -> Option<u32> {
    let mut map = HitMap::new();
    map.fill(elems.iter().filter_map(|elem| stop_of(screen, fixture, None, *elem)).collect());
    map.swap();
    map.resolve(Some(ENTRY), PointerKind::Click, x, y, None).hit.map(|key| key.elem)
}

/// A profile that has remembered `terms`. The write comes BEFORE the switch deliberately: the
/// recents store caches per profile GENERATION, so a file write behind an already-seated profile
/// is not read until something moves that generation.
fn watching(session: &plx_plex::plex::session::TempSession, who: &str, terms: &[&str]) {
    let terms: Vec<String> = terms.iter().map(|t| (*t).to_owned()).collect();
    let who_owned = who.to_owned();
    plx_plex::plex::session::update(|s| {
        let mut next = s.clone();
        next.set_recents_for(&who_owned, terms.clone());
        Some(next)
    });
    session.watching(who);
}

fn remembering(tag: &str, terms: &[&str]) -> plx_plex::plex::session::TempSession {
    let session = plx_plex::plex::session::TempSession::new(tag);
    watching(&session, tag, terms);
    session
}

// ---- the ▼ handoff: focus only enters a region that is drawn ---------------------------------

/// Legacy `the_handoff_answers_only_with_regions_that_are_drawn` and
/// `down_from_the_field_with_nothing_below_it_stays_put`, on the owned screen: there is no
/// `below_of` state machine any more — the same rule is that the screen publishes a GROUP only
/// for a region it draws, so ▼ has nothing to link to. The four cases are unchanged.
#[test]
fn down_from_the_field_reaches_only_a_region_that_is_drawn() {
    let _serial = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("owned-search-handoff");

    // (1) no query, no terms: nothing is under the field at all.
    watching(&session, "owned-search-handoff-fresh", &[]);
    let fixture = Fixture::new();
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    assert!(screen.recents.is_empty());
    assert_eq!(step_dir(&mut screen, &fixture, &mut engine, Dir::Down), Some(screen.key(FIELD)),
        "nothing is drawn below the field, so focus must not leave it");

    // (2) no query, terms remembered: the recents list.
    watching(&session, "owned-search-handoff-terms", &["gromit", "wallace"]);
    let mut fixture = Fixture::new();
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    assert_eq!(screen.recents.len(), 2);
    let recent = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    assert_eq!(Some(recent.elem), screen.recents.first().copied(), "▼ lands on the first term");

    // (3) a query whose answer is empty: the statement holds no focus, and it does NOT fall back
    // to the remembered terms either.
    fixture.query("wallace");
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    assert!(screen.recents.is_empty(), "a live query outranks the remembered terms");
    assert_eq!(step_dir(&mut screen, &fixture, &mut engine, Dir::Down), Some(screen.key(FIELD)),
        "an empty answer draws a statement, not a focusable region");

    // (4) a query with shelves: the results.
    fixture.shelves(vec![shelf(Kind::Movie, "handoff", 3)]);
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    let tile = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    assert_eq!(Some(tile.elem), screen.rows[0].elems.first().copied());
    drop(session);
}

/// Legacy `a_query_below_the_stores_own_threshold_is_not_a_query`: one character never reaches a
/// server, so the remembered terms must stay up rather than flicker out and back on a backspace.
#[test]
fn a_query_below_the_stores_own_threshold_keeps_the_remembered_terms() {
    let _serial = plx_base::testlock::serial();
    let session = remembering("owned-search-threshold", &["gromit", "wallace"]);
    let mut fixture = Fixture::new();
    fixture.query("w");
    let screen = fixture.screen();
    assert_eq!(fixture.store.state(), plx_data::search::State::Idle,
        "one character never reaches the server — `search::MIN_QUERY` is 2");
    assert_eq!(screen.recents.len(), 2, "so the remembered terms stay on screen");
    assert!(screen.rows.is_empty());

    fixture.query("wa");
    let screen = fixture.screen();
    assert!(screen.recents.is_empty(), "now it IS a search, and an empty answer says so");
    assert!(screen.rows.is_empty());
    drop(session);
}

// ---- the shelves: visual column, ragged rows, and a set that empties -------------------------

/// Legacy `a_vertical_step_between_shelves_keeps_the_visual_column`: two shelves at different
/// horizontal scrolls put the same index in wildly different places, and carrying the INDEX
/// across is what made a ▼ read as the page lurching sideways. The owned screen answers with
/// `Seat::Projected` and `card_row::column_near_x`, on the drawn rect.
#[test]
fn a_vertical_step_between_shelves_keeps_the_visual_column() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("column").shelves(vec![shelf(Kind::Movie, "a", 20), shelf(Kind::Show, "b", 20)]);
    let mut screen = fixture.screen();
    let style = layout::style(Kind::Movie);
    scroll_row(&mut screen, &fixture, 0, 4.0 * (style.w + style.gap));
    let mut engine = FocusEngine::new();
    let sixth = screen.key(screen.rows[0].elems[6]);
    engine.set(OWNER, sixth, Some(screen.rows[0].group), By::Restore);

    let down = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    assert_eq!(screen.rows[1].elems.iter().position(|e| *e == down.elem), Some(2),
        "▼ lands in the column that is drawn under the cursor, not on the same index");
    let up = step_dir(&mut screen, &fixture, &mut engine, Dir::Up).unwrap();
    assert_eq!(up, sixth, "…and ▲ returns to the tile it came from rather than drifting");
}

/// Legacy `a_shelf_that_shrinks_under_the_cursor_re_seats_it`: a column carried onto a SHORTER
/// shelf clamps, the ends hold, and the cursor is where the user last actually stood.
#[test]
fn a_shelf_that_shrinks_under_the_cursor_re_seats_it() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    // Movies 8, Shows 2, Collections 5 — the shape a real search returns, on one poster lattice
    // so these assertions stay about the CLAMP rather than about an episode still's own width.
    fixture.query("ragged").shelves(vec![shelf(Kind::Movie, "m", 8), shelf(Kind::Show, "s", 2),
        shelf(Kind::Collection, "c", 5)]);
    let mut screen = fixture.screen();
    let mut engine = FocusEngine::new();
    let index = |screen: &SearchScreen, key: FocusKey<u32>| index_of(&screen, key.elem)
        .and_then(|(group, i)| screen.rows.iter().position(|row| row.group == group).map(|row| (row, i)));
    engine.set(OWNER, screen.key(screen.rows[0].elems[6]), Some(screen.rows[0].group), By::Restore);

    let down = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    assert_eq!(index(&screen, down), Some((1, 1)), "▼ lands inside a two-item shelf, not off its end");
    let down = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    assert_eq!(index(&screen, down), Some((2, 1)),
        "…and keeps the column it can now afford, not the 6 it started with");

    // The ends hold: ◀ at column 0, ▶ at the last item, ▼ on the last shelf.
    for (row, col, dir) in [(0usize, 0usize, Dir::Left), (0, 7, Dir::Right), (2, 0, Dir::Down)] {
        engine.set(OWNER, screen.key(screen.rows[row].elems[col]), Some(screen.rows[row].group), By::Restore);
        let landed = step_dir(&mut screen, &fixture, &mut engine, dir).unwrap();
        assert_eq!(index(&screen, landed), Some((row, col)), "{dir:?} at the end of its row holds");
    }

    // ▲ off shelf 0 leaves the shelves entirely — the field.
    engine.set(OWNER, screen.key(screen.rows[0].elems[3]), Some(screen.rows[0].group), By::Restore);
    assert_eq!(step_dir(&mut screen, &fixture, &mut engine, Dir::Up), Some(screen.key(FIELD)));
}

/// Legacy `focus_is_clamped_when_the_shelves_shrink_under_it` and
/// `moving_inside_an_emptied_result_set_falls_back_to_the_field`: every keystroke wipes the store,
/// so focus addressing a tile that is not there must land on the field — and the SLOT the user was
/// standing in survives, so a shelf that lands again seats where they were.
#[test]
fn focus_is_clamped_when_the_shelves_shrink_under_it() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("clamp").shelves(vec![shelf(Kind::Movie, "c", 12)]);
    let mut screen = fixture.screen();
    let deep = screen.key(screen.rows[0].elems[11]);

    // The same query, with the shelf shrunk to four items: the cursor re-seats onto the last one.
    fixture.shelves(vec![shelf(Kind::Movie, "c", 4)]);
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::StoreChanged(StoreId::Search.ord(), 0));
    let seated = <SearchScreen as Focusable<HostFixture>>::reconcile(&screen, deep, &fixture.cx(Some(deep)));
    assert_eq!(index_of(&screen, seated.elem).map(|(g, i)| (g == screen.rows[0].group, i)), Some((true, 3)),
        "a cursor past the end of a shrunken shelf seats on its last item");

    // …and a set that empties entirely leaves focus on the field, never on a tile that is gone.
    fixture.query("replacement");
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::StoreChanged(StoreId::Search.ord(), 0));
    assert!(screen.rows.is_empty());
    let cx = fixture.cx(Some(deep));
    assert_eq!(<SearchScreen as Focusable<HostFixture>>::reconcile(&screen, deep, &cx), screen.key(FIELD));
    assert!(<SearchScreen as Focusable<HostFixture>>::place(&screen, &deep.elem, &cx, At::Drawn).is_none(),
        "…and the tile it named is no longer anywhere on the page");
    for dir in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
        let mut engine = FocusEngine::new();
        engine.set(OWNER, deep, None, By::Restore);
        // The frame's own reconcile pass first, exactly as the dispatcher runs it: that is what
        // moves a cursor off a region that has gone, and a direction key never sees the stale one.
        engine.reconcile(OWNER, &screen, &fixture.cx(Some(deep)));
        step_dir(&mut screen, &fixture, &mut engine, dir);
        assert!(engine.current(OWNER).is_some_and(|key| index_of(&screen, key.elem).is_some()),
            "{dir:?} inside an emptied result set must land on something drawn");
    }
}

/// Legacy `an_empty_result_set_is_not_a_card`: `app.rs` never arms a tvOS press on a screen with
/// no items. The owned answer is `ElemKind` — the field is `Bare` and there is no card group at
/// all until a shelf lands.
#[test]
fn an_empty_result_set_is_not_a_card() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("nothing");
    let screen = fixture.screen();
    let cx = fixture.cx(Some(screen.key(FIELD)));
    let mut groups = Vec::new();
    <SearchScreen as Focusable<HostFixture>>::groups(&screen, &cx, &mut groups);
    assert!(!groups.iter().any(|g| g.elem == ElemKind::Card), "an empty answer holds no card");
    assert_eq!(groups.iter().find(|g| g.id == FIELD_GROUP).map(|g| g.elem), Some(ElemKind::Bare),
        "the field is never a card: OK on it opens the keyboard, it does not commit a press");

    fixture.shelves(vec![shelf(Kind::Movie, "card", 2), shelf(Kind::Person, "p", 2)]);
    let screen = fixture.screen();
    let cx = fixture.cx(None);
    let mut groups = Vec::new();
    <SearchScreen as Focusable<HostFixture>>::groups(&screen, &cx, &mut groups);
    assert_eq!(groups.iter().find(|g| g.id == screen.rows[0].group).map(|g| g.elem), Some(ElemKind::Card));
    assert_eq!(groups.iter().find(|g| g.id == screen.rows[1].group).map(|g| g.elem), Some(ElemKind::Bare),
        "a person is not a press-and-hold card either");
}

/// Legacy `the_recents_cursor_stops_on_the_clear_control`: ▼ walks the terms that are DRAWN and
/// then stops on Clear — never onto a row that was never drawn, and never past the control.
#[test]
fn the_recents_cursor_stops_on_the_clear_control() {
    let _serial = plx_base::testlock::serial();
    let session = remembering("owned-search-clear", &["gromit", "wallace", "preston"]);
    let fixture = Fixture::new();
    let mut screen = fixture.screen();
    assert_eq!(screen.recents.len(), 3);
    let mut engine = seated(&screen, &fixture);
    let mut seen = Vec::new();
    for _ in 0..10 {
        seen.push(step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap().elem);
    }
    let expected: Vec<u32> = screen.recents.iter().copied().chain(std::iter::repeat(CLEAR)).take(10).collect();
    assert_eq!(seen, expected, "▼ walks every drawn term and then caps on Clear");

    // Clear is one row past the last term shown, and its own block is where the renderer draws it.
    let cx = fixture.cx(Some(screen.key(CLEAR)));
    let clear = <SearchScreen as Focusable<HostFixture>>::place(&screen, &CLEAR, &cx, At::Drawn).unwrap();
    let last = <SearchScreen as Focusable<HostFixture>>::place(&screen, &screen.recents[2], &cx, At::Drawn).unwrap();
    assert!(clear.rect.y >= last.rect.y + last.rect.h, "Clear sits below the last DRAWN term");
    assert_eq!(clear.rect.y, layout::clear(3, 0.0, &fixture.measure).y);
    assert!(step_dir(&mut screen, &fixture, &mut engine, Dir::Up).is_some_and(|k| k.elem == screen.recents[2]),
        "…and ▲ returns to the list rather than to the field");

    // Pressing it clears THIS profile's history through the store's own vocabulary, drops the rows
    // at once rather than waiting for the write, and hands focus back to the field — the control
    // it was standing on is the one thing that cannot survive its own press.
    let clear_key = screen.key(CLEAR);
    let (_, out, _) = deliver(&mut screen, &fixture, Some(clear_key), ScreenEvent::Activate(CLEAR));
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Search,
        StoreCmd::Search(SearchCmd::ClearRecents { .. }))))));
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::Deliver(_, Delivery::Screen(
        ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(key) }))) if key.elem == FIELD)));
    assert!(screen.recents.is_empty(), "the list goes now, not when the disk answers");
    assert!(<SearchScreen as Focusable<HostFixture>>::place(&screen, &CLEAR,
        &fixture.cx(None), At::Drawn).is_none(), "…and Clear is no longer a stop at all");
    drop(session);
}

// ---- the document's geometry, as the pointer sees it ------------------------------------------

/// Legacy `results.rs`'s `a_tile_scrolled_under_the_chrome_is_not_a_pointer_target`, on the owned
/// screen's `Focusable::place`: §7.6's `rect ∩ clip` is what a pointer resolves against, so the
/// visible part of a tile under the top chrome is still pressable and a tile fully behind it is
/// not a target at all. The clip is the page's floor, not the tile's own rect.
#[test]
fn a_tile_scrolled_under_the_chrome_is_not_a_pointer_target() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("chrome").shelves(vec![shelf(Kind::Movie, "t", 4)]);
    let mut screen = fixture.screen();
    let elem = screen.rows[0].elems[0];
    let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
    let rest = <SearchScreen as Focusable<HostFixture>>::place(&screen, &elem, &fixture.cx(None), At::Drawn).unwrap().rect;

    // Fully on screen: hit anywhere inside it.
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), rest.cy()), Some(elem));

    // Straddling the floor: the visible half answers, the covered half does not.
    let straddle = rest.y + rest.h - floor - 20.0;
    park(&mut screen, straddle);
    let part = <SearchScreen as Focusable<HostFixture>>::place(&screen, &elem, &fixture.cx(None), At::Drawn).unwrap();
    let visible = part.rect.intersect(part.clip);
    assert!((visible.h - 20.0).abs() < 0.001, "only the part below the track is taken: {visible:?}");
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor + 10.0), Some(elem));
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor - 10.0), None,
        "above the track is chrome the standing strip owns");

    // Fully underneath: no target at all, including on the floor line itself.
    park(&mut screen, rest.y + rest.h - floor + 0.5);
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor), None);
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor + 1.0), None);
}

/// Legacy `the_fields_hit_rect_rides_the_scroll_and_stops_at_the_track`: the query field is a
/// document element, so its hit rect scrolls with it — at rest it is `layout::FIELD` itself, half
/// under the track it is pressable on the half you can see, and fully under it is not a target.
#[test]
fn the_fields_hit_rect_rides_the_scroll_and_stops_at_the_track() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut screen = fixture.screen();
    let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
    let at_rest = stop_of(&screen, &fixture, None, FIELD).unwrap();
    assert_eq!((at_rest.rect.y, at_rest.rect.h), (layout::FIELD.y, layout::FIELD.h),
        "an unscrolled screen must cost nothing: this is FIELD itself");
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), layout::FIELD.cy()), Some(FIELD));

    park(&mut screen, layout::FIELD.y - floor + 20.0);
    let part = stop_of(&screen, &fixture, None, FIELD).unwrap();
    let visible = part.rect.intersect(part.clip);
    assert_eq!(visible.y, floor, "floored at the track, not dropped");
    assert!((visible.h - (layout::FIELD.h - 20.0)).abs() < 0.001,
        "only the part under the track is taken: y={} h={}", visible.y, visible.h);
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), floor + 10.0), Some(FIELD));

    park(&mut screen, layout::FIELD.y + layout::FIELD.h - floor + 0.5);
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), floor), None,
        "once the whole box is behind the track it is not a target at all");
}

/// Legacy `results.rs`'s `revealing_the_second_shelf_carries_the_query_field_under_the_track`:
/// the whole document travels, head included. Revealing shelf 0 scrolls nothing; revealing shelf 1
/// takes the field entirely under the tab track, where it is no longer a pointer target.
#[test]
fn revealing_the_second_shelf_carries_the_query_field_under_the_track() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("reveal").shelves(vec![shelf(Kind::Movie, "r0", 6), shelf(Kind::Show, "r1", 6)]);
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);

    step_dir(&mut screen, &fixture, &mut engine, Dir::Down);
    assert_eq!(screen.stack.target(), 0.0, "the first shelf is already on screen");
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), layout::FIELD.cy()), Some(FIELD));

    step_dir(&mut screen, &fixture, &mut engine, Dir::Down);
    assert!(screen.stack.target() > 0.0, "the second shelf is below the fold and must be revealed");
    for i in 0..120 { frame(&mut screen, &fixture, &engine, i); }
    let floor = plx_ui::widgets::TOP_BAR_BOTTOM;
    assert!(screen.stack.scroll() > layout::FIELD.y + layout::FIELD.h - floor,
        "the field must end up wholly under the track (scroll {})", screen.stack.scroll());
    assert!(stop_of(&screen, &fixture, None, FIELD)
        .is_some_and(|stop| stop.rect.intersect(stop.clip).h <= 0.0));
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), floor), None,
        "a query box carried into the chrome is not something a click can raise the keyboard on");
}

// ---- motion: every spring this instance owns owes the gate both halves ------------------------

/// Legacy `the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest` — `ui/CLAUDE.md`'s
/// two halves, because the failures are opposite and each is invisible to the other's gate. An
/// over-reporting animator costs the whole idle saving while every fps floor still passes.
///
/// Graded on the DISPATCHER's gate (`Present`), which is what an owned page's `Effects` reach;
/// `plx_machine::idle` is the loop's gate and the bridge ORs the two.
#[test]
fn the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("scroll").shelves(vec![shelf(Kind::Movie, "s", 4)]);
    let mut screen = fixture.screen();
    let engine = seated(&screen, &fixture);
    assert!(!frame(&mut screen, &fixture, &engine, 100),
        "a Search screen with nothing moving must stop repainting");
    assert!(!frame(&mut screen, &fixture, &engine, 101), "…and stays quiet frame after frame");

    park(&mut screen, 400.0);
    assert!(frame(&mut screen, &fixture, &engine, 102), "a scrolling shelf must keep the panel awake");
    let mut frames = 0;
    while frame(&mut screen, &fixture, &engine, 103 + frames) && frames < 600 { frames += 1; }
    assert!(frames < 600, "the scroll must arrive, not ring forever");
    assert!(screen.stack.scroll().abs() < 0.25, "with focus off the shelves the flow rests at zero");
}

/// Legacy `the_fields_focus_fade_runs_when_focus_leaves_it_and_settles`: the field's two faces
/// cross-fade, so the fade owes the gate the same two halves. It mounts SEATED — a focused field
/// gliding up from idle would report motion for the first frames of a screen that is not moving.
#[test]
fn the_fields_focus_fade_runs_when_focus_leaves_it_and_settles() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("fade").shelves(vec![shelf(Kind::Movie, "f", 4)]);
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    assert_eq!(screen.hot.pos, 1.0, "the screen mounts with the field focused and the fill seated");
    assert!(!frame(&mut screen, &fixture, &engine, 100));

    step_dir(&mut screen, &fixture, &mut engine, Dir::Down); // onto the shelf: the field cools
    let mut ran = 0;
    while frame(&mut screen, &fixture, &engine, 101 + ran) && ran < 200 { ran += 1; }
    assert!(ran > 4, "the fade must keep the panel awake while it travels (ran {ran} frames)");
    assert!(ran < 120, "…and settle rather than ring forever");
    assert!(screen.hot.pos < 0.02, "…arriving on the idle face (got {})", screen.hot.pos);

    step_dir(&mut screen, &fixture, &mut engine, Dir::Up); // …and back
    assert!(frame(&mut screen, &fixture, &engine, 400), "focus returning to the field animates too");
    for i in 0..200 { frame(&mut screen, &fixture, &engine, 401 + i); }
    assert!(screen.hot.pos > 0.98, "…and arrives on the focused face (got {})", screen.hot.pos);
}

/// Legacy `the_caret_blinks_and_reports_only_on_the_flip`: the blink is a CLOCK, so it must ask
/// for a frame when the bar changes state and for nothing in between — a blink that reported every
/// frame would cost the whole idle saving to animate one 5px bar.
#[test]
fn the_caret_blinks_and_reports_only_on_the_flip() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut screen = fixture.screen();
    let engine = seated(&screen, &fixture);
    screen.editing = true;
    screen.blink_us = 0;
    let mut flips = 0;
    let mut was = true;
    for f in 0..((2 * BLINK_US / DT_US) + 4) {
        let asked = frame(&mut screen, &fixture, &engine, 100 + f);
        let on = screen.blink_us < BLINK_US;
        if on != was {
            flips += 1;
            assert!(asked, "frame {f}: the bar changed state and did not ask to be drawn");
            was = on;
        } else {
            assert!(!asked, "frame {f}: a caret mid-phase asked for a repaint");
        }
    }
    assert_eq!(flips, 2, "one full cycle is exactly two flips");

    // …and with the panel down nothing animates at all.
    screen.editing = false;
    for f in 0..200 {
        assert!(!frame(&mut screen, &fixture, &engine, 400 + f),
            "frame {f}: the caret asked for a repaint with no keyboard up");
        assert_eq!(screen.blink_us, 0, "the phase parks ON so the next open does not start invisible");
    }
}

// ---- the mount: a return is not a reset -------------------------------------------------------

/// Legacy `the_pill_returns_to_the_search_that_was_there_and_a_seed_replaces_it`,
/// `resuming_a_profile_that_has_never_searched_is_a_clean_mount` and
/// `entering_seeds_the_field_and_parks_every_cursor`, as one contract on the owned screen: a mount
/// SEATS a posture and never touches the store. There is no `resume`/`enter` pair any more —
/// what the pill press used to reset, the store now owns, so a seeded entry is a `SearchCmd`
/// somebody else sends and a return is this instance mounting over whatever is published.
#[test]
fn a_mount_seats_the_field_and_parks_every_cursor_without_replacing_the_search() {
    let _serial = plx_base::testlock::serial();
    let session = remembering("owned-search-mount", &["gromit"]);
    let mut fixture = Fixture::new();

    // A profile that has never searched: the field opens empty, nothing was ever asked.
    let screen = fixture.screen();
    let mut probe = String::new();
    <SearchScreen as Screen<HostFixture>>::state(&screen).probe(&mut probe);
    assert_eq!(fixture.store.state(), plx_data::search::State::Idle);
    assert!(probe.contains("editing=false") && probe.contains("caret=0") && probe.contains("rows=0"));
    assert_eq!(screen.stack.target(), 0.0);
    assert_eq!(screen.hot.pos, 1.0, "the field mounts focused and SEATED, or it reports motion on arrival");

    // A search in progress, with a cursor deep in its results: mounting again over it keeps the
    // query, its generation and its shelves, and re-seats the posture.
    fixture.query("wallace").shelves(vec![shelf(Kind::Movie, "mount", 8)]);
    let generation = fixture.store.query_gen();
    let mut screen = fixture.screen();
    screen.stack.jump_to(400.0);
    let (_, out, _) = deliver(&mut screen, &fixture, None, ScreenEvent::Mount);
    assert_eq!(fixture.store.query(), "wallace", "a mount must not wipe the term still on screen");
    assert_eq!(fixture.store.query_gen(), generation, "…nor supersede the answer under it");
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::Deliver(_, Delivery::Screen(
        ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(key) }))) if key.elem == FIELD)),
        "a mount opens on the field, whatever the last visit left");
    assert!(!screen.editing, "…and does NOT raise the keyboard: an arrival has nothing to type");

    // A seeded entry — the boot trigger's, now a store command — puts the term in the field with
    // the caret at its live end, which is where somebody who had just typed it would be standing.
    fixture.query("gromit ");
    let screen = fixture.screen();
    let mut probe = String::new();
    <SearchScreen as Screen<HostFixture>>::state(&screen).probe(&mut probe);
    assert!(probe.contains("caret=7"), "the caret is the LIVE string's end: {probe}");
    assert!(screen.recents.is_empty() && screen.stack.target() == 0.0, "every cursor parks");
    drop(session);
}

// ---- the television's own keyboard: the four keys it delegates --------------------------------

/// Legacy `the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle` and
/// `backspace_only_bites_while_the_panel_is_up_and_takes_a_whole_character`. The panel sends four
/// keys and three of them once did nothing, which is how it shipped with visibly dead buttons.
/// Cyrillic throughout: two bytes a letter, so a byte step would split a codepoint.
#[test]
fn the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle() {
    let _serial = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut screen = fixture.screen();
    let field = Some(FocusKey { entry: ENTRY, elem: FIELD });

    // With the panel down the screen has no claim on the key, and must not edit anyway.
    let (handled, out, _) = deliver(&mut screen, &fixture, field, key_event(Key::Left, 0));
    assert_eq!(handled, Handled::No, "with the panel down the screen has no claim on the key");
    assert!(out.iter().all(|effect| !matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(SearchCmd::SetQueryScoped { .. }))))));
    // OK on the field arrives as `Activate` — the field is a `Bare` element, so the dispatcher
    // delivers the activation rather than arming a press.
    let (handled, _, _) = deliver(&mut screen, &fixture, field, ScreenEvent::Activate(FIELD));
    assert_eq!(handled, Handled::Yes);
    assert!(screen.editing, "OK on the field raises the panel");

    deliver(&mut screen, &fixture, field, text_event(TextEdit::Commit("суббота".into())));
    assert_eq!(screen.draft.query(), "суббота");
    assert_eq!(screen.draft.caret(), "суббота".len(), "typing leaves the caret after what was typed");

    // ◀ walks back one CHARACTER at a time and clamps at the front; ▶ steps one forward.
    for _ in 0..3 { deliver(&mut screen, &fixture, field, key_event(Key::Left, 0)); }
    assert_eq!(screen.draft.caret(), "суббота".len() - "ота".len(), "three letters back, not three bytes");
    for _ in 0..40 { deliver(&mut screen, &fixture, field, key_event(Key::Left, 0)); }
    assert_eq!(screen.draft.caret(), 0, "◀ clamps at the front");
    deliver(&mut screen, &fixture, field, key_event(Key::Right, 0));
    assert_eq!(screen.draft.caret(), "с".len(), "▶ steps one whole character forward");

    // A commit lands AT the caret, and Backspace takes the character before it.
    let (_, out, _) = deliver(&mut screen, &fixture, field, text_event(TextEdit::Commit("X".into())));
    assert_eq!(screen.draft.query(), "сXуббота");
    assert_eq!(screen.draft.caret(), "сX".len(), "…leaving the caret after the inserted text");
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Search,
        StoreCmd::Search(SearchCmd::SetQueryScoped { query, .. }))) if query == "сXуббота")),
        "every accepted edit is one scoped store command");
    let (handled, _, _) = deliver(&mut screen, &fixture, field,
        key_event(Key::Other, plx_ui::consts::SDLK_BACKSPACE));
    assert_eq!(handled, Handled::Yes, "editing, the screen consumes it");
    assert_eq!(screen.draft.query(), "суббота", "a whole codepoint, not one byte of a two-byte char");

    // Clear all is the whole field, wherever the caret was standing.
    deliver(&mut screen, &fixture, field, key_event(Key::Right, 0));
    let (handled, _, _) = deliver(&mut screen, &fixture, field,
        key_event(Key::Other, plx_ui::consts::SDLK_CLEAR));
    assert_eq!(handled, Handled::Yes, "the panel's Clear all is the screen's key");
    assert_eq!((screen.draft.query(), screen.draft.caret()), ("", 0));

    // An empty field still consumes Backspace — it is the field's — and edits nothing.
    let (handled, out, _) = deliver(&mut screen, &fixture, field,
        key_event(Key::Other, plx_ui::consts::SDLK_BACKSPACE));
    assert_eq!(handled, Handled::Yes);
    assert!(out.iter().all(|effect| !matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Search, StoreCmd::Search(SearchCmd::SetQueryScoped { .. }))))),
        "a no-op edit is not a query the store has to answer");

    // OK again is the COMMIT: the panel comes down and the term is filed (legacy
    // `ok_raises_the_panel_and_leaving_the_field_drops_it`, whose other half — a ▼ that moved
    // nothing must not dismiss — is the Bridge tier's `owned_search_opens_system_ownership_…`).
    deliver(&mut screen, &fixture, field, text_event(TextEdit::Commit("wallace".into())));
    let (handled, out, _) = deliver(&mut screen, &fixture, field, key_event(Key::Ok, 0));
    assert_eq!(handled, Handled::Yes);
    assert!(!screen.editing, "OK again commits and drops the panel");
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Search,
        StoreCmd::Search(SearchCmd::RememberRecent { term, .. }))) if term == "wallace")),
        "a committed search is what earns a place in the remembered terms");
}

/// Legacy `the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down`: the
/// result set stays still under a user who is still typing, and ▼ off the field lands on shelf 0
/// without the page jumping under it.
#[test]
fn the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("frozen").shelves(vec![shelf(Kind::Movie, "f0", 4), shelf(Kind::Show, "f1", 4),
        shelf(Kind::Episode, "f2", 4)]);
    let mut screen = fixture.screen();
    let deep = screen.key(screen.rows[2].elems[0]);
    let field = screen.key(FIELD);

    // The one case that scrolls at all: the shelves hold focus with the keyboard down.
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::FocusMoved {
        from: Some(field), to: deep, by: By::Dir });
    assert!(screen.stack.target() > 0.0);

    // The keyboard goes up: the flow parks at zero under a user who is still typing, and stays
    // there for as long as the panel is up.
    deliver(&mut screen, &fixture, Some(field), ScreenEvent::Activate(FIELD));
    assert!(screen.editing);
    assert_eq!(screen.stack.target(), 0.0, "the result set stays still while the panel is up");
    deliver(&mut screen, &fixture, Some(field), ScreenEvent::FocusMoved {
        from: Some(deep), to: field, by: By::Dir });
    assert_eq!(screen.stack.target(), 0.0);

    // A step off the field DROPS the panel first — which is what makes "the flow moves only with
    // the keyboard down" true by construction rather than by a second frozen flag.
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::FocusMoved {
        from: Some(field), to: deep, by: By::Dir });
    assert!(!screen.editing, "leaving the field takes the television's keyboard with it");
    assert!(screen.stack.target() > 0.0);

    // …and whenever the shelves do not hold focus, the flow is back at zero.
    for elem in [FIELD, screen.rows[0].elems[0]] {
        let key = screen.key(elem);
        deliver(&mut screen, &fixture, Some(key), ScreenEvent::FocusMoved {
            from: Some(deep), to: key, by: By::Dir });
        assert_eq!(screen.stack.target(), 0.0, "elem {elem} is not below the fold");
    }
}

/// The scroll that shows result row `k` of `kinds` focused, from scroll `cur`, worked from the
/// layout: blocks measured at their destination (the focused row's caption band open, the others
/// closed), the page's `CONTENT_TOP` as the margin.
fn search_reveal_want(kinds: &[Kind], k: usize, cur: f32) -> f32 {
    use plx_ui::cards::{reveal, under_band};
    use plx_ui::consts::{MARGIN_Y, SCR_H};
    let block = |n: usize, open: bool| layout::block_h(kinds[n], under_band(open as i32 as f32));
    let top = layout::CONTENT_TOP + (0..k).map(|n| block(n, false)).sum::<f32>();
    let end = layout::CONTENT_TOP + (0..kinds.len()).map(|n| block(n, n == k)).sum::<f32>();
    reveal(cur, top + block(k, true) - (SCR_H - MARGIN_Y), top - layout::CONTENT_TOP, (end - (SCR_H - MARGIN_Y)).max(0.0))
}

/// Up to a row already revealed does not move the page when measured from where the page is
/// headed (the target), which is not where it is (the live position) when keys outrun the glide.
#[test]
fn a_step_up_is_revealed_from_the_target_the_walk_down_left_not_from_the_lagging_position() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("walk").shelves(vec![shelf(Kind::Movie, "w0", 6), shelf(Kind::Show, "w1", 6),
        shelf(Kind::Episode, "w2", 6), shelf(Kind::Person, "w3", 6), shelf(Kind::Collection, "w4", 6)]);
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    let kinds = [Kind::Movie, Kind::Show, Kind::Episode, Kind::Person, Kind::Collection];
    let want = |k, cur| search_reveal_want(&kinds, k, cur);
    let mut target = 0.0;
    for k in 0..5 {
        step_dir(&mut screen, &fixture, &mut engine, Dir::Down);
        target = want(k, target);
        assert_eq!(screen.stack.target(), target, "row {k}, from the previous target");
    }
    assert_eq!(screen.stack.scroll(), 0.0, "setup: no frame has run, the page has not left home");
    step_dir(&mut screen, &fixture, &mut engine, Dir::Up);
    let from_target = want(3, target);
    assert_eq!(screen.stack.target(), from_target, "Up measures from the target");
    assert_ne!(from_target, want(3, screen.stack.scroll()), "setup: from the live position it would differ");
}

// ---- the borrowed-source annotation -----------------------------------------------------------

/// Three sources on one shelf: the household's own, and two shares with different handles.
fn shared_shelf(fixture: &mut Fixture) -> [plx_plex::plex::ServerId; 3] {
    plx_plex::plex::reset_servers_for_test();
    let own = plx_plex::plex::register_for_test("own-machine", "127.0.0.1", 1, "own", "annotation");
    let a = plx_plex::plex::register_for_test("share-a", "127.0.0.1", 2, "a", "annotation");
    let b = plx_plex::plex::register_for_test("share-b", "127.0.0.1", 3, "b", "annotation");
    plx_plex::plex::describe_server(own, "own-machine", "", plx_plex::plex::GrantEvidence::ours());
    plx_plex::plex::describe_server(a, "share-a", "friend", plx_plex::plex::GrantEvidence::outside());
    plx_plex::plex::describe_server(b, "share-b", "other", plx_plex::plex::GrantEvidence::outside());
    let item = |sid| Item::Media(plx_data::pms::PmsMovie { sid, rk: format!("annotated-{sid:?}"),
        title: "Synthetic result".into(), ..Default::default() });
    fixture.query("annotated").shelves(vec![
        Shelf { window: Default::default(), kind: Kind::Movie, items: vec![item(own), item(a), item(b)] },
        Shelf { window: Default::default(), kind: Kind::Show, items: vec![item(own)] }]);
    let handles: Vec<&str> = fixture.search.view().scope().sources().iter()
        .map(|source| source.handle.as_str()).collect();
    assert_eq!(handles, ["", "friend", "other"], "the fixture's own roster projection");
    [own, a, b]
}

/// Legacy `results.rs`'s `the_owner_annotation_swaps_its_words_only_while_it_is_invisible`: the
/// handle under a heading rises to near-full alpha, a NEW handle replaces the old one only at or
/// under [`OWNER_FLOOR`] — so a word never changes under the eye — and an own item fades the run
/// out and leaves it absent.
#[test]
fn the_owner_annotation_swaps_its_words_only_while_it_is_invisible() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let _sids = shared_shelf(&mut fixture);
    let mut screen = fixture.screen();
    let mut engine = FocusEngine::new();
    let seat = |screen: &SearchScreen, engine: &mut FocusEngine<u32>, row: usize, col: usize| {
        engine.set(OWNER, screen.key(screen.rows[row].elems[col]), Some(screen.rows[row].group), By::Dir);
    };

    // The own item is nobody's borrowed source: no run, ever.
    seat(&screen, &mut engine, 0, 0);
    for i in 0..60 { frame(&mut screen, &fixture, &engine, i); }
    assert_eq!(screen.owner, "");
    assert!(screen.owner_alpha.pos <= OWNER_FLOOR, "an own item carries no annotation");

    // A share: the handle is adopted and rises.
    seat(&screen, &mut engine, 0, 1);
    for i in 60..160 { frame(&mut screen, &fixture, &engine, i); }
    assert_eq!(screen.owner, "friend");
    assert_eq!(screen.owner_row, Some(0));
    assert!(screen.owner_alpha.pos > 0.9, "the source reaches near-full alpha: {}", screen.owner_alpha.pos);

    // A DIFFERENT share: the word may not change while the old one is still visible.
    seat(&screen, &mut engine, 0, 2);
    let mut swapped_at = None;
    for i in 160..320 {
        frame(&mut screen, &fixture, &engine, i);
        // The alpha the swap guard compared is this frame's, after its own spring step — which is
        // also the alpha the renderer draws the run at, so "invisible" means the same thing to
        // both halves.
        if screen.owner != "friend" && swapped_at.is_none() {
            swapped_at = Some(screen.owner_alpha.pos);
        }
        if swapped_at.is_none() {
            assert_eq!(screen.owner, "friend", "frame {i}: the word changed while it was still on screen");
        }
    }
    assert!(swapped_at.is_some_and(|alpha| alpha <= OWNER_FLOOR),
        "the swap must happen at or under the floor, not at {swapped_at:?}");
    assert_eq!(screen.owner, "other");
    assert!(screen.owner_alpha.pos > 0.9, "…and then rises again");

    // Back onto the own item: the run fades out and stays absent.
    seat(&screen, &mut engine, 0, 0);
    for i in 320..480 { frame(&mut screen, &fixture, &engine, i); }
    assert_eq!(screen.owner, "");
    assert!(screen.owner_alpha.pos <= OWNER_FLOOR);

    // And exactly one shelf ever holds the run.
    seat(&screen, &mut engine, 1, 0);
    for i in 480..560 { frame(&mut screen, &fixture, &engine, i); }
    assert_eq!(screen.owner_row, Some(1), "the annotation belongs to the row the cursor is in");
    assert_eq!(screen.owner, "", "…and that row's item is the household's own");
    plx_plex::plex::reset_servers_for_test();
}

/// Legacy `results.rs`'s `a_settled_annotation_goes_quiet_and_a_moving_one_does_not` — the other
/// half of the same spring, on the dispatcher's gate: adopting a source invalidates once, the
/// rising spring asks every frame while it travels, and a settled annotation asks for nothing.
#[test]
fn a_settled_annotation_goes_quiet_and_a_moving_one_does_not() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let _sids = shared_shelf(&mut fixture);
    let mut screen = fixture.screen();
    let mut engine = FocusEngine::new();
    // Seated on the OWN item and fully settled: every other spring on this screen is at rest, so
    // what the next frames report is the annotation and nothing else.
    engine.set(OWNER, screen.key(screen.rows[0].elems[0]), Some(screen.rows[0].group), By::Dir);
    for i in 0..200 { frame(&mut screen, &fixture, &engine, i); }
    for i in 200..210 {
        assert!(!frame(&mut screen, &fixture, &engine, i), "frame {i}: a settled screen asked for a repaint");
    }

    engine.set(OWNER, screen.key(screen.rows[0].elems[1]), Some(screen.rows[0].group), By::Dir);
    assert!(frame(&mut screen, &fixture, &engine, 210), "adopting a source invalidates once");
    let mut ran = 0;
    while frame(&mut screen, &fixture, &engine, 211 + ran) && ran < 300 { ran += 1; }
    assert!(ran > 4, "the rising annotation must keep the panel awake (ran {ran} frames)");
    assert!(ran < 300, "…and settle rather than ring forever");
    assert!(screen.owner_alpha.pos > 0.98, "…arriving lit (got {})", screen.owner_alpha.pos);
    for i in 0..10 {
        assert!(!frame(&mut screen, &fixture, &engine, 600 + i),
            "frame {i}: a settled annotation asked for a repaint");
    }
    plx_plex::plex::reset_servers_for_test();
}

/// **A collection hit routes to the collection page, by ratingKey first and by section + tag id
/// when it has no ratingKey.** `search::CollectionHit::route` builds `ContentArg::Collection`
/// (`stores::content_arg`, re-exported as `registry::ContentArg`); this test grades what it builds.
/// The store's own tests (`search_merge_ranking_tests`) grade the fields a hit is made of, from the
/// wire row.
#[test]
fn a_collection_hit_routes_by_rating_key_or_by_section_and_tag_id() {
    use plx_plex::plex::collections::CollectionRef;
    use crate::registry::ContentArg;
    use plx_data::search::{CollectionHit, TagHit};
    let sid = plx_plex::plex::ServerId::from_raw(3);

    // a full `type=collection` row (`includeCollections=1`): both ids ride along
    let row = plx_plex::plex::Metadata {
        kind: "collection".into(),
        rating_key: "50007".into(),
        title: "Aardman Shorts".into(),
        index: 7,
        child_count: 12,
        library_section_id: 1,
        thumb: "/library/collections/50007/composite/1700000000".into(),
        ..Default::default()
    };
    let hit = CollectionHit::from_row(&row, sid);
    assert_eq!(
        hit.route(),
        Some(ContentArg::Collection(CollectionRef {
            sid,
            rk: "50007".into(),
            sec: 1,
            tag: 7,
            name: "Aardman Shorts".into(),
        }))
    );

    // a tag-shaped row, from a server that ignored the flag: no ratingKey, so the section and the
    // tag id are the whole identity
    let tag = TagHit {
        sid,
        name: "Aardman Shorts".into(),
        id: "7".into(),
        sec: 1,
        count: 12,
        ..Default::default()
    };
    let hit = CollectionHit::from_tag(&tag);
    assert_eq!(
        hit.route(),
        Some(ContentArg::Collection(CollectionRef::by_tag(sid, 1, 7, "Aardman Shorts")))
    );
    // with no section or no tag id there is nothing to resolve, so there is no route
    let no_section = CollectionHit {
        item: plx_data::pms::PmsMovie { sec: 0, ..hit.item.clone() },
        ..hit.clone()
    };
    assert!(no_section.route().is_none(), "no section");
    assert!(CollectionHit { tag: 0, ..hit.clone() }.route().is_none(), "no tag id");
}

/// The count read-out and the heading, the other half: the two counts are different questions and
/// a Collections shelf answers both at once — three collections found ("3 results", the store's
/// `Kind::count_label`), one of which holds twelve films ("12 items", `ui::fmt::item_count`, the
/// UI's formatter shared with every collection tile and the collection page).
#[test]
fn a_collection_shelf_counts_results_and_its_tiles_count_items() {
    use plx_ui::fmt::item_count;
    assert_eq!(
        (plx_data::search::Kind::Collection.count_label(3), item_count(12)),
        ("3 results".to_owned(), "12 items".to_owned())
    );
    assert_eq!(item_count(1), "1 item");
    // Cardinal rules apply to the absolute value, including negative wire counts.
    assert_eq!((item_count(0), item_count(-1)), ("0 items".to_owned(), "-1 item".to_owned()));
}

/// Put the page's scroll at `y` with its target left where it was, as a glide in progress.
fn park(screen: &mut SearchScreen, y: f32) {
    let target = screen.stack.target();
    screen.stack.jump_to(y);
    screen.stack.scroll_to(target);
}

/// The group `elem` is in and its place there, read off the page's model.
fn index_of(screen: &SearchScreen, elem: u32) -> Option<(GroupId, usize)> {
    if elem == FIELD { return Some((FIELD_GROUP, 0)); }
    if elem == CLEAR && !screen.recents.is_empty() { return Some((CLEAR_GROUP, 0)); }
    if let Some(i) = screen.recents.iter().position(|key| *key == elem) { return Some((RECENTS_GROUP, i)); }
    screen.rows.iter().find_map(|row| row.elems.iter().position(|key| *key == elem).map(|i| (row.group, i)))
}

/// Result row `row`'s shelf in the page's stack.
fn shelf_of(screen: &SearchScreen, row: usize) -> &plx_ui::cards::Shelf {
    screen.stack.shelf(screen.section(row)).expect("the row has a shelf")
}

/// Put result row `row`'s tiles `x` px along their strip, as a Back to it would.
fn scroll_row(screen: &mut SearchScreen, fixture: &Fixture, row: usize, x: f32) {
    let sec = screen.section(row);
    screen.stack.restore(&StackMemory { scroll: screen.stack.scroll(), shelves: vec![(sec, x)] });
    deliver(screen, fixture, None, ScreenEvent::Resume);
}

// ---- result rows follow their kind, not their place in the list -------------------------------

/// The pop of `elem` on result row `row`, as the row's shelf would draw it now.
fn pop_of(screen: &SearchScreen, fixture: &Fixture, focus: Option<FocusKey<u32>>, row: usize, elem: u32) -> Option<f32> {
    let src = screen.row_cards(fixture.search.view(), row)?;
    shelf_of(screen, row).scale_of::<HostFixture, _>(&fixture.cx(focus), &src, &elem)
}

/// Sources answer one at a time, so a row can land above rows already on the page. Each row keeps
/// its own shelf (its scroll and its lifted card), keyed by its kind; a row inserted above must
/// not hand its neighbour's scroll or pop to itself.
#[test]
fn a_row_that_lands_above_keeps_the_neighbours_scroll_and_pop_with_the_neighbour() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("landing").shelves(vec![shelf(Kind::Show, "s", 20)]);
    let mut screen = fixture.screen();
    let pitch = layout::style(Kind::Show).w + layout::style(Kind::Show).gap;
    scroll_row(&mut screen, &fixture, 0, 3.0 * pitch);
    let focus = Some(screen.key(screen.rows[0].elems[5]));
    deliver(&mut screen, &fixture, focus, ScreenEvent::FocusMoved { from: None, to: focus.unwrap(), by: By::Restore });
    for i in 0..60 { deliver(&mut screen, &fixture, focus, ScreenEvent::Tick(tick(i))); }
    let scrolled = shelf_of(&screen, 0).scroll();
    assert!(scrolled > 0.0);
    let elem = focus.unwrap().elem;
    assert_eq!(pop_of(&screen, &fixture, focus, 0, elem), Some(layout::style(Kind::Show).focus_scale));

    fixture.shelves(vec![shelf(Kind::Movie, "m", 20), shelf(Kind::Show, "s", 20)]);
    deliver(&mut screen, &fixture, focus, ScreenEvent::StoreChanged(StoreId::Search.ord(), 1));
    deliver(&mut screen, &fixture, focus, ScreenEvent::Tick(tick(100)));
    assert_eq!(screen.rows.iter().map(|row| row.kind).collect::<Vec<_>>(), [Kind::Movie, Kind::Show]);
    let kinds = [Kind::Movie, Kind::Show];
    let followed = search_reveal_want(&kinds, 1, 0.0);
    assert!(followed > 0.0, "setup: the Show row now sits below the fold");
    assert_eq!(screen.stack.target(), followed, "the page follows the focused row down to where it landed");
    assert_eq!(shelf_of(&screen, 0).scroll(), 0.0, "the new row starts at its own beginning");
    assert_eq!(shelf_of(&screen, 1).scroll(), scrolled, "the Show row keeps the Show row's scroll");
    assert_eq!(pop_of(&screen, &fixture, focus, 1, elem), Some(layout::style(Kind::Show).focus_scale),
        "and its lifted card stays lifted");
    let first = screen.rows[0].elems[0];
    assert_eq!(pop_of(&screen, &fixture, focus, 0, first), Some(1.0), "the new row lifts nothing");
}

/// Collection's `dismissing_a_hold_menu_and_moving_leaves_no_stale_lift`, on a result row: the
/// opener a hold covers sits at full pop under the menu's focus (the opener redraw reads it), and
/// once the menu is dismissed and focus moves on, the old card lets go and the new one grows from
/// rest, so nothing stays lifted for a stale redraw to read.
#[test]
fn dismissing_a_hold_menu_and_moving_leaves_no_stale_lift() {
    let _serial = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("opener").shelves(vec![shelf(Kind::Movie, "m", 6)]);
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);
    let a = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    for i in 0..60 { frame(&mut screen, &fixture, &engine, i); }
    let full = layout::style(Kind::Movie).focus_scale;
    let at_full = |screen: &SearchScreen| pop_of(screen, &fixture, Some(a), 0, a.elem).is_some_and(|pop| (pop - full).abs() < 0.001);
    assert!(at_full(&screen), "the settled opener is at full pop");

    // The menu opens over the page: the page keeps its focus and its pop.
    deliver(&mut screen, &fixture, Some(a), ScreenEvent::Cover);
    for i in 0..30 { frame(&mut screen, &fixture, &engine, 100 + i); }
    assert!(at_full(&screen), "covered, the opener stays lifted");
    let held = <SearchScreen as Focusable<HostFixture>>::place(&screen, &a.elem, &fixture.cx(Some(a)), At::Drawn).unwrap();
    assert!((held.rect.w - held.rest_rect.w).abs() < 0.01, "the opener redraw paints the settled rect");
    deliver(&mut screen, &fixture, Some(a), ScreenEvent::Uncover);

    let b = step_dir(&mut screen, &fixture, &mut engine, Dir::Right).unwrap();
    assert_ne!(a, b);
    frame(&mut screen, &fixture, &engine, 200);
    let (old, new) = (pop_of(&screen, &fixture, Some(b), 0, a.elem).unwrap(), pop_of(&screen, &fixture, Some(b), 0, b.elem).unwrap());
    assert!(old < full - 0.001 && old >= 1.0, "the old opener is letting go, not held at full pop: {old}");
    assert!(new < full - 0.001, "the new card grows from rest: {new}");
}

// ---- characterization: the page's focus model, pinned before it moves under `Stack` -----------

/// What the hand-written page does today, as text: the focus groups and their policies, the
/// neighbour of every element on every edge, the seats, where each element is placed at rest, the
/// scroll after walking the shelves down and back, what a return from Detail restores and how a
/// replaced result set reconciles. The expectations are the behaviour of the page before it was a
/// `Stack` page; they change only when Search's behaviour is meant to.
mod characterization {
    use super::*;

    /// The page's elements in document order, by whatever the page keeps them in.
    fn content(screen: &SearchScreen) -> (Vec<u32>, Vec<Vec<u32>>) {
        (screen.recents.clone(), screen.rows.iter().map(|row| row.elems.clone()).collect())
    }

    fn f(v: f32) -> String { format!("{:.1}", if v == 0.0 { 0.0 } else { v }) }
    fn r(rect: Rect) -> String { format!("[{} {} {} {}]", f(rect.x), f(rect.y), f(rect.w), f(rect.h)) }

    fn groups(screen: &SearchScreen, fixture: &Fixture, focus: Option<FocusKey<u32>>) -> String {
        let mut out = Vec::new();
        <SearchScreen as Focusable<HostFixture>>::groups(screen, &fixture.cx(focus), &mut out);
        out.iter()
            .map(|g| format!("{:#x} {:?} seat={:?} edge={:?} len={} elem={:?} extent={}", g.id.0, g.kind, g.seat, g.edge, g.len, g.elem, r(g.extent)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn name(elem: u32, screen: &SearchScreen) -> String {
        let (recents, rows) = content(screen);
        if elem == FIELD { return "field".into(); }
        if elem == CLEAR { return "clear".into(); }
        if let Some(i) = recents.iter().position(|e| *e == elem) { return format!("recent{i}"); }
        for (row, elems) in rows.iter().enumerate() {
            if let Some(i) = elems.iter().position(|e| *e == elem) { return format!("r{row}c{i}"); }
        }
        format!("?{elem}")
    }

    fn neighbours(screen: &SearchScreen, fixture: &Fixture, elems: &[u32]) -> String {
        let mut lines = Vec::new();
        for &elem in elems {
            let key = screen.key(elem);
            let cx = fixture.cx(Some(key));
            let at = |dir| match <SearchScreen as Focusable<HostFixture>>::neighbour(screen, key, dir, &cx) {
                Step::Move(k) => name(k.elem, screen),
                Step::Edge => "-".into(),
                #[allow(unreachable_patterns)]
                _ => "other".into(),
            };
            lines.push(format!("{}: up={} down={} left={} right={}", name(elem, screen),
                at(Dir::Up), at(Dir::Down), at(Dir::Left), at(Dir::Right)));
        }
        lines.join("\n")
    }

    fn all_elems(screen: &SearchScreen) -> Vec<u32> {
        let (recents, rows) = content(screen);
        let mut v = vec![FIELD];
        v.extend(recents.iter().copied());
        if !recents.is_empty() { v.push(CLEAR); }
        v.extend(rows.into_iter().flatten());
        v
    }

    fn placed(screen: &SearchScreen, fixture: &Fixture, focus: Option<FocusKey<u32>>, elems: &[u32], at: At) -> String {
        elems.iter().map(|&e| {
            match <SearchScreen as Focusable<HostFixture>>::place(screen, &e, &fixture.cx(focus), at) {
                Some(p) => format!("{}: rect={} rest={} clip={} index={:?}", name(e, screen), r(p.rect), r(p.rest_rect), r(p.clip), p.index),
                None => format!("{}: none", name(e, screen)),
            }
        }).collect::<Vec<_>>().join("\n")
    }

    fn seats(screen: &SearchScreen, fixture: &Fixture, focus: Option<FocusKey<u32>>, gs: &[GroupId], from: Placed) -> String {
        gs.iter().map(|&g| {
            let k = <SearchScreen as Focusable<HostFixture>>::seat(screen, g, from, &fixture.cx(focus));
            format!("{:#x} -> {}", g.0, name(k.elem, screen))
        }).collect::<Vec<_>>().join("\n")
    }

    fn settle(screen: &mut SearchScreen, fixture: &Fixture, engine: &FocusEngine<u32>) {
        for i in 0..180 { frame(screen, fixture, engine, 1000 + i); }
    }

    fn recents_fixture() -> (plx_plex::plex::session::TempSession, Fixture) {
        (remembering("characterization-recents", &["gromit", "wallace", "preston", "feathers"]), Fixture::new())
    }

    fn shelves_fixture() -> Fixture {
        let mut fixture = Fixture::new();
        fixture.query("many").shelves(vec![shelf(Kind::Movie, "m", 9), shelf(Kind::Show, "s", 3),
            shelf(Kind::Episode, "e", 7), shelf(Kind::Person, "p", 12), shelf(Kind::Collection, "c", 4)]);
        fixture
    }

    #[test]
    fn the_recents_page_groups_neighbours_and_places() {
        let _serial = plx_base::testlock::serial();
        let (_session, fixture) = recents_fixture();
        let screen = fixture.screen();
        let field = Some(screen.key(FIELD));
        let elems = all_elems(&screen);
        let from = <SearchScreen as Focusable<HostFixture>>::place(&screen, &FIELD, &fixture.cx(field), At::Drawn).unwrap();
        let got = [groups(&screen, &fixture, field), neighbours(&screen, &fixture, &elems),
            placed(&screen, &fixture, field, &elems, At::Drawn), placed(&screen, &fixture, field, &elems, At::SpringTarget),
            seats(&screen, &fixture, field, &[FIELD_GROUP, RECENTS_GROUP, CLEAR_GROUP], from)].join("\n--\n");
        assert_eq!(got, include_str!("characterization/recents.txt").trim_end());
        let mut links = links(&screen).iter().map(|l| format!("{:#x} {:?} {:#x}", l.from.0, l.dir, l.to.0)).collect::<Vec<_>>();
        links.sort();
        assert_eq!(links.join("\n"), include_str!("characterization/recents_links.txt").trim_end());
        // a seat from a card-less place (anywhere but the field) enters at the first element
        let down = <SearchScreen as Focusable<HostFixture>>::seat(&screen, RECENTS_GROUP, from, &fixture.cx(Some(screen.key(CLEAR))));
        assert_eq!(name(down.elem, &screen), "recent0");
    }

    #[test]
    fn the_results_page_groups_neighbours_and_places() {
        let _serial = plx_base::testlock::serial();
        let fixture = shelves_fixture();
        let screen = fixture.screen();
        let field = Some(screen.key(FIELD));
        let elems = all_elems(&screen);
        let from = <SearchScreen as Focusable<HostFixture>>::place(&screen, &FIELD, &fixture.cx(field), At::Drawn).unwrap();
        let gids: Vec<GroupId> = [FIELD_GROUP].into_iter().chain((0..5).map(|i| GroupId(0x5345_4200 + i))).collect();
        let got = [groups(&screen, &fixture, field), neighbours(&screen, &fixture, &elems),
            placed(&screen, &fixture, field, &elems, At::Drawn), seats(&screen, &fixture, field, &gids, from)].join("\n--\n");
        assert_eq!(got, include_str!("characterization/results.txt").trim_end());
        let mut links = links(&screen).iter().map(|l| format!("{:#x} {:?} {:#x}", l.from.0, l.dir, l.to.0)).collect::<Vec<_>>();
        links.sort();
        assert_eq!(links.join("\n"), include_str!("characterization/results_links.txt").trim_end());
    }

    /// Walk ▼ through every shelf (settling each stop) and back ▲: the scroll at rest, where the
    /// focused tile is placed, and that the field and the first shelf come back where they were.
    #[test]
    fn walking_down_through_every_shelf_and_back_scrolls_and_places_as_it_did() {
        let _serial = plx_base::testlock::serial();
        let fixture = shelves_fixture();
        let mut screen = fixture.screen();
        let mut engine = seated(&screen, &fixture);
        let mut lines = Vec::new();
        let mut record = |screen: &mut SearchScreen, engine: &FocusEngine<u32>, what: &str| {
            settle(screen, &fixture, engine);
            let focus = engine.current(OWNER);
            let elem = focus.unwrap().elem;
            let p = <SearchScreen as Focusable<HostFixture>>::place(screen, &elem, &fixture.cx(focus), At::Drawn).unwrap();
            let field = <SearchScreen as Focusable<HostFixture>>::place(screen, &FIELD, &fixture.cx(focus), At::Drawn).unwrap();
            lines.push(format!("{what}: {} scroll={} rect={} rest={} field_y={}", name(elem, screen), f(screen.stack.scroll()), r(p.rect), r(p.rest_rect), f(field.rect.y)));
        };
        record(&mut screen, &engine, "start");
        for _ in 0..5 {
            step_dir(&mut screen, &fixture, &mut engine, Dir::Down);
            record(&mut screen, &engine, "down");
        }
        step_dir(&mut screen, &fixture, &mut engine, Dir::Right);
        record(&mut screen, &engine, "right");
        for _ in 0..5 {
            step_dir(&mut screen, &fixture, &mut engine, Dir::Up);
            record(&mut screen, &engine, "up");
        }
        assert_eq!(lines.join("\n"), include_str!("characterization/walk.txt").trim_end());
    }

    #[test]
    fn a_return_from_detail_restores_the_scroll_the_shelves_and_the_focus() {
        let _serial = plx_base::testlock::serial();
        let fixture = shelves_fixture();
        let mut screen = fixture.screen();
        let mut engine = seated(&screen, &fixture);
        for _ in 0..3 { step_dir(&mut screen, &fixture, &mut engine, Dir::Down); }
        let deep = engine.current(OWNER).unwrap();
        for _ in 0..3 { step_dir(&mut screen, &fixture, &mut engine, Dir::Right); }
        let focus = engine.current(OWNER).unwrap();
        settle(&mut screen, &fixture, &engine);
        let before = placed(&screen, &fixture, Some(focus), &[focus.elem], At::Drawn);
        let memory = Screen::<HostFixture>::memory_at(&screen, Some(focus));

        let mut fresh = SearchScreen::new(ENTRY, INSTANCE);
        deliver(&mut fresh, &fixture, None, ScreenEvent::RestoreMemory(memory));
        deliver(&mut fresh, &fixture, Some(focus), ScreenEvent::Mount);
        let reconciled = <SearchScreen as Focusable<HostFixture>>::reconcile(&fresh, focus, &fixture.cx(Some(focus)));
        assert_eq!(reconciled, focus, "the tile that was focused is the tile that comes back");
        let mut engine = FocusEngine::new();
        engine.set(OWNER, focus, None, By::Restore);
        for i in 0..4 { frame(&mut fresh, &fixture, &engine, i); }
        assert_eq!(name(deep.elem, &fresh), name(deep.elem, &screen));
        let got = format!("scroll={} target={}\n{}", f(fresh.stack.scroll()), f(fresh.stack.target()),
            placed(&fresh, &fixture, Some(focus), &[focus.elem], At::Drawn));
        assert_eq!(got.lines().last().unwrap(), before, "the restored page puts the tile where it stood");
        assert_eq!(got.lines().next().unwrap(), include_str!("characterization/restore.txt").trim_end());
    }

    #[test]
    fn replacing_the_results_under_focus_reconciles_to_the_same_slot_else_the_field() {
        let _serial = plx_base::testlock::serial();
        let mut fixture = shelves_fixture();
        let mut screen = fixture.screen();
        let mut out = Vec::new();
        for (elem_of, label) in [((1usize, 2usize), "r1c2"), ((3, 11), "r3c11"), ((0, 8), "r0c8")] {
            let want = screen.key(content(&screen).1[elem_of.0][elem_of.1]);
            // the same query with fewer items in every shelf, then with a shelf gone
            fixture.shelves(vec![shelf(Kind::Movie, "m", 5), shelf(Kind::Show, "s", 1),
                shelf(Kind::Episode, "e", 7), shelf(Kind::Person, "p", 4), shelf(Kind::Collection, "c", 4)]);
            deliver(&mut screen, &fixture, Some(want), ScreenEvent::StoreChanged(StoreId::Search.ord(), 0));
            let shrunk = <SearchScreen as Focusable<HostFixture>>::reconcile(&screen, want, &fixture.cx(Some(want)));
            out.push(format!("{label} shrunk -> {}", name(shrunk.elem, &screen)));
            fixture.shelves(vec![shelf(Kind::Show, "s", 3), shelf(Kind::Episode, "e", 7)]);
            deliver(&mut screen, &fixture, Some(want), ScreenEvent::StoreChanged(StoreId::Search.ord(), 0));
            let gone = <SearchScreen as Focusable<HostFixture>>::reconcile(&screen, want, &fixture.cx(Some(want)));
            out.push(format!("{label} dropped -> {}", name(gone.elem, &screen)));
            fixture.shelves(vec![shelf(Kind::Movie, "m", 9), shelf(Kind::Show, "s", 3),
                shelf(Kind::Episode, "e", 7), shelf(Kind::Person, "p", 12), shelf(Kind::Collection, "c", 4)]);
            deliver(&mut screen, &fixture, None, ScreenEvent::StoreChanged(StoreId::Search.ord(), 0));
        }
        assert_eq!(out.join("\n"), include_str!("characterization/reconcile.txt").trim_end());
    }
}

/// A collection hit opens its page on a hold, never an item menu, so it never gets the hint and
/// never spends the kind's one showing.
#[test]
fn the_hold_hint_does_not_stand_on_a_collection_hit() {
    let _serial = plx_base::testlock::serial();
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut fixture = Fixture::new();
    let hits = (0..5).map(|i| Item::Collection(plx_data::search::CollectionHit {
        item: plx_data::pms::PmsMovie { rk: format!("c-{i}"), title: format!("Collection {i}"),
            kind: plx_data::pms::KIND_COLLECTION, ..Default::default() },
        tag: i,
    })).collect();
    fixture.query("hint").shelves(vec![Shelf { window: Default::default(), kind: Kind::Collection, items: hits }]);
    let mut screen = fixture.screen();
    let card = Some(screen.key(screen.rows[0].elems[1]));
    deliver(&mut screen, &fixture, card, ScreenEvent::FocusMoved { from: None, to: card.unwrap(), by: By::Dir });
    for at in 100..100 + 6 * 60 {
        deliver(&mut screen, &fixture, card, ScreenEvent::Tick(tick(at)));
    }
    assert!(!screen.stack.hint_visible(), "a hold on a collection hit opens its page, not a menu");
    assert!(!plx_ui::hold_hint::shown_this_run(plx_ui::hold_hint::Kind::Search), "and nothing is spent");
}

/// The hold hint (`ui::hold_hint`) on a result card: it stands after the dwell on a settled card,
/// not on the field (not a card), not while a menu owns focus, and not a second time this run.
#[test]
fn the_hold_hint_stands_on_a_resting_result_card_once_per_run() {
    let _serial = plx_base::testlock::serial();
    plx_ui::hold_hint::reset_learned_for_test();
    plx_ui::hold_hint::reset_shown_for_test();
    let mut fixture = Fixture::new();
    fixture.query("hint").shelves(vec![shelf(Kind::Movie, "m", 8)]);
    let mut screen = fixture.screen();
    let card = Some(screen.key(screen.rows[0].elems[1]));
    let mut at = 100;
    let mut rest = |screen: &mut SearchScreen, focus: Option<FocusKey<u32>>, secs: f32| {
        for _ in 0..(secs * 60.0) as u32 {
            at += 1;
            deliver(screen, &fixture, focus, ScreenEvent::Tick(tick(at)));
        }
    };
    deliver(&mut screen, &fixture, card, ScreenEvent::FocusMoved { from: None, to: card.unwrap(), by: By::Dir });
    rest(&mut screen, card, 1.2);
    assert!(!screen.stack.hint_visible(), "not before the dwell");
    // negatives first: a stand spends the kind's latch and would make them vacuous
    rest(&mut screen, Some(FocusKey { entry: EntryId(900), elem: 0 }), 4.0);
    assert!(!screen.stack.hint_visible() && !plx_ui::hold_hint::shown_this_run(plx_ui::hold_hint::Kind::Search), "never while a menu owns focus");
    let field = Some(screen.key(FIELD));
    rest(&mut screen, field, 4.0);
    assert!(!screen.stack.hint_visible() && !plx_ui::hold_hint::shown_this_run(plx_ui::hold_hint::Kind::Search), "never on the field");
    rest(&mut screen, card, 3.6);
    assert!(screen.stack.hint_visible(), "after the dwell, on a settled card");
    rest(&mut screen, Some(FocusKey { entry: EntryId(900), elem: 0 }), 1.0);
    assert!(!screen.stack.hint_visible(), "a menu taking focus hides it");
    rest(&mut screen, card, 4.0);
    assert!(!screen.stack.hint_visible(), "and not a second time on Search this run");
}

/// A row whose window has hits past it asks for the next window once focus nears its trailing
/// edge, and when the window then slides the focused card is still the same element.
#[test]
fn a_row_with_hits_past_its_window_asks_for_the_next_one_and_keeps_the_focused_card_as_it_slides() {
    let _serial = plx_base::testlock::serial();
    let window = |start, before, after| plx_data::search::Window { start, before, after };
    let mut fixture = Fixture::new();
    let mut first = shelf(Kind::Movie, "m", 24);
    first.window = window(0, false, true);
    fixture.query("slide").shelves(vec![first]);
    let mut screen = fixture.screen();
    let pages = |out: &[Stamped<HostFixture>]| out.iter().filter(|e| matches!(&e.fx, Fx::App(AppFx::Store(
        StoreId::Search, StoreCmd::Search(SearchCmd::Page { kind: Kind::Movie, before: false, .. }))))).count();
    // mid-row: nothing is asked
    let mid = screen.key(screen.rows[0].elems[3]);
    deliver(&mut screen, &fixture, Some(mid), ScreenEvent::FocusMoved { from: None, to: mid, by: By::Dir });
    let asked: usize = (100..160).map(|at| pages(&deliver(&mut screen, &fixture, Some(mid), ScreenEvent::Tick(tick(at))).1)).sum();
    assert_eq!(asked, 0, "far from the trailing edge");
    // near the edge: asked, once
    let focus = screen.key(screen.rows[0].elems[22]);
    deliver(&mut screen, &fixture, Some(focus), ScreenEvent::FocusMoved { from: Some(mid), to: focus, by: By::Dir });
    let asked: usize = (200..300).map(|at| pages(&deliver(&mut screen, &fixture, Some(focus), ScreenEvent::Tick(tick(at))).1)).sum();
    assert_eq!(asked, 1, "the trailing edge asks once per window");
    // the store slides half a window on: the focused card is the same element, seated earlier
    let mut next = Shelf { window: window(12, true, true), kind: Kind::Movie,
        items: (12..36).map(|i| movie(&format!("m-{i}"))).collect() };
    next.window.after = false;
    fixture.shelves(vec![next]);
    deliver(&mut screen, &fixture, Some(focus), ScreenEvent::Tick(tick(400)));
    assert_eq!(screen.rows[0].elems.get(10).copied(), Some(focus.elem), "m-22 keeps its identity at its new place");
    assert_eq!(screen.rows[0].elems.len(), 24);
}
