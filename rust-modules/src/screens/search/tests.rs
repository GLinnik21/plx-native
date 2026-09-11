//! The owned Search screen's own contracts: focus over the drawn document, the geometry a
//! pointer resolves against, and the motion each of this instance's springs owes the frame gate.
//!
//! These are the legacy `ui/search/mod.rs` behaviours carried onto the owned screen. What lives
//! here is what a `Focusable`/`Machine` step can answer with a retained publication and a real
//! `FocusEngine`; the input-ORDERING half (the system keyboard's native latch, the shared strip,
//! the dispatcher's press machine) is the Bridge tier's, in `app/search_owned_tests.rs`.
use super::*;
use crate::screens::registry::AppMsg;
use crate::search::view::SearchView;
use crate::search::{Item, Shelf};
use crate::ui::fixture::FixtureMeasure;
use crate::ui::focus::{FocusEngine, Outcome};
use crate::ui::hit::{HitMap, PointerKind};
use crate::ui::screen::By;
use crate::ui::machine::{FocusRead, Host, InputEvent, PressRead, Source, Stamped, Tick};
use crate::ui::present::Present;
use crate::ui::screen::{Activate, Hover, ScreenArg, Stop};

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> crate::ui::machine::Chrome { crate::ui::machine::Chrome::None }
    fn id(&self) -> crate::ui::machine::ScreenId { crate::ui::machine::ScreenId(1) }
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
    Item::Media(crate::pms::PmsMovie { rk: rk.into(), title: format!("Synthetic {rk}"), ..Default::default() })
}

/// A shelf of `n` synthetic items, addressed so two shelves never share a result identity.
fn shelf(kind: Kind, tag: &str, n: usize) -> Shelf {
    Shelf { kind, items: (0..n).map(|i| movie(&format!("{tag}-{i}"))).collect() }
}

struct Fixture {
    search: crate::stores::search::SearchSnapshot,
    measure: FixtureMeasure,
}

impl Fixture {
    /// A store with no query, no shelves and no remembered terms — a fresh boot.
    fn new() -> Self {
        crate::stores::search::apply(SearchCmd::Reset);
        Self { search: crate::stores::search::snapshot(), measure: FixtureMeasure }
    }
    fn query(&mut self, q: &str) -> &mut Self {
        crate::stores::search::apply(SearchCmd::SetQuery(q.into()));
        self.capture()
    }
    fn shelves(&mut self, shelves: Vec<Shelf>) -> &mut Self {
        crate::search::publish_shelves_for_test(shelves);
        self.capture()
    }
    fn capture(&mut self) -> &mut Self {
        self.search = crate::stores::search::snapshot();
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
fn watching(session: &crate::plex::session::TempSession, who: &str, terms: &[&str]) {
    let terms: Vec<String> = terms.iter().map(|t| (*t).to_owned()).collect();
    let who_owned = who.to_owned();
    crate::plex::session::update(|s| {
        let mut next = s.clone();
        next.set_recents_for(&who_owned, terms.clone());
        Some(next)
    });
    session.watching(who);
}

fn remembering(tag: &str, terms: &[&str]) -> crate::plex::session::TempSession {
    let session = crate::plex::session::TempSession::new(tag);
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
    let _serial = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("owned-search-handoff");

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
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `a_query_below_the_stores_own_threshold_is_not_a_query`: one character never reaches a
/// server, so the remembered terms must stay up rather than flicker out and back on a backspace.
#[test]
fn a_query_below_the_stores_own_threshold_keeps_the_remembered_terms() {
    let _serial = crate::testlock::serial();
    let session = remembering("owned-search-threshold", &["gromit", "wallace"]);
    let mut fixture = Fixture::new();
    fixture.query("w");
    let screen = fixture.screen();
    assert_eq!(crate::search::state(), crate::search::State::Idle,
        "one character never reaches the server — `search::MIN_QUERY` is 2");
    assert_eq!(screen.recents.len(), 2, "so the remembered terms stay on screen");
    assert!(screen.rows.is_empty());

    fixture.query("wa");
    let screen = fixture.screen();
    assert!(screen.recents.is_empty(), "now it IS a search, and an empty answer says so");
    assert!(screen.rows.is_empty());
    drop(session);
    crate::stores::search::apply(SearchCmd::Reset);
}

// ---- the shelves: visual column, ragged rows, and a set that empties -------------------------

/// Legacy `a_vertical_step_between_shelves_keeps_the_visual_column`: two shelves at different
/// horizontal scrolls put the same index in wildly different places, and carrying the INDEX
/// across is what made a ▼ read as the page lurching sideways. The owned screen answers with
/// `Seat::Projected` and `card_row::column_near_x`, on the drawn rect.
#[test]
fn a_vertical_step_between_shelves_keeps_the_visual_column() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("column").shelves(vec![shelf(Kind::Movie, "a", 20), shelf(Kind::Show, "b", 20)]);
    let mut screen = fixture.screen();
    let style = layout::style(Kind::Movie);
    screen.rows[0].motion.restore_scroll(4.0 * (style.w + style.gap), 20, &style);
    let mut engine = FocusEngine::new();
    let sixth = screen.key(screen.rows[0].elems[6]);
    engine.set(OWNER, sixth, Some(screen.rows[0].group), By::Restore);

    let down = step_dir(&mut screen, &fixture, &mut engine, Dir::Down).unwrap();
    assert_eq!(screen.rows[1].elems.iter().position(|e| *e == down.elem), Some(2),
        "▼ lands in the column that is drawn under the cursor, not on the same index");
    let up = step_dir(&mut screen, &fixture, &mut engine, Dir::Up).unwrap();
    assert_eq!(up, sixth, "…and ▲ returns to the tile it came from rather than drifting");
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `a_shelf_that_shrinks_under_the_cursor_re_seats_it`: a column carried onto a SHORTER
/// shelf clamps, the ends hold, and the cursor is where the user last actually stood.
#[test]
fn a_shelf_that_shrinks_under_the_cursor_re_seats_it() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    // Movies 8, Shows 2, Collections 5 — the shape a real search returns, on one poster lattice
    // so these assertions stay about the CLAMP rather than about an episode still's own width.
    fixture.query("ragged").shelves(vec![shelf(Kind::Movie, "m", 8), shelf(Kind::Show, "s", 2),
        shelf(Kind::Collection, "c", 5)]);
    let mut screen = fixture.screen();
    let mut engine = FocusEngine::new();
    let index = |screen: &SearchScreen, key: FocusKey<u32>| screen.index(key.elem)
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
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `focus_is_clamped_when_the_shelves_shrink_under_it` and
/// `moving_inside_an_emptied_result_set_falls_back_to_the_field`: every keystroke wipes the store,
/// so focus addressing a tile that is not there must land on the field — and the SLOT the user was
/// standing in survives, so a shelf that lands again seats where they were.
#[test]
fn focus_is_clamped_when_the_shelves_shrink_under_it() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("clamp").shelves(vec![shelf(Kind::Movie, "c", 12)]);
    let mut screen = fixture.screen();
    let deep = screen.key(screen.rows[0].elems[11]);

    // The same query, with the shelf shrunk to four items: the cursor re-seats onto the last one.
    fixture.shelves(vec![shelf(Kind::Movie, "c", 4)]);
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::StoreChanged(StoreId::Search.ord(), 0));
    let seated = <SearchScreen as Focusable<HostFixture>>::reconcile(&screen, deep, &fixture.cx(Some(deep)));
    assert_eq!(screen.index(seated.elem).map(|(g, i)| (g == screen.rows[0].group, i)), Some((true, 3)),
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
        assert!(engine.current(OWNER).is_some_and(|key| screen.index(key.elem).is_some()),
            "{dir:?} inside an emptied result set must land on something drawn");
    }
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `an_empty_result_set_is_not_a_card`: `app.rs` never arms a tvOS press on a screen with
/// no items. The owned answer is `ElemKind` — the field is `Bare` and there is no card group at
/// all until a shelf lands.
#[test]
fn an_empty_result_set_is_not_a_card() {
    let _serial = crate::testlock::serial();
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
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `the_recents_cursor_stops_on_the_clear_control`: ▼ walks the terms that are DRAWN and
/// then stops on Clear — never onto a row that was never drawn, and never past the control.
#[test]
fn the_recents_cursor_stops_on_the_clear_control() {
    let _serial = crate::testlock::serial();
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
    crate::stores::search::apply(SearchCmd::Reset);
}

// ---- the document's geometry, as the pointer sees it ------------------------------------------

/// Legacy `results.rs`'s `a_tile_scrolled_under_the_chrome_is_not_a_pointer_target`, on the owned
/// screen's `Focusable::place`: §7.6's `rect ∩ clip` is what a pointer resolves against, so the
/// visible part of a tile under the top chrome is still pressable and a tile fully behind it is
/// not a target at all. The clip is the page's floor, not the tile's own rect.
#[test]
fn a_tile_scrolled_under_the_chrome_is_not_a_pointer_target() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("chrome").shelves(vec![shelf(Kind::Movie, "t", 4)]);
    let mut screen = fixture.screen();
    let elem = screen.rows[0].elems[0];
    let floor = crate::ui::widgets::TOP_BAR_BOTTOM;
    let rest = screen.row_rect(0, 0, At::Drawn);

    // Fully on screen: hit anywhere inside it.
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), rest.cy()), Some(elem));

    // Straddling the floor: the visible half answers, the covered half does not.
    let straddle = rest.y + rest.h - floor - 20.0;
    screen.scroll.jump(straddle);
    let part = <SearchScreen as Focusable<HostFixture>>::place(&screen, &elem, &fixture.cx(None), At::Drawn).unwrap();
    let visible = part.rect.intersect(part.clip);
    assert!((visible.h - 20.0).abs() < 0.001, "only the part below the track is taken: {visible:?}");
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor + 10.0), Some(elem));
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor - 10.0), None,
        "above the track is chrome the standing strip owns");

    // Fully underneath: no target at all, including on the floor line itself.
    screen.scroll.jump(rest.y + rest.h - floor + 0.5);
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor), None);
    assert_eq!(hit(&screen, &fixture, &[elem], rest.cx(), floor + 1.0), None);
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `the_fields_hit_rect_rides_the_scroll_and_stops_at_the_track`: the query field is a
/// document element, so its hit rect scrolls with it — at rest it is `layout::FIELD` itself, half
/// under the track it is pressable on the half you can see, and fully under it is not a target.
#[test]
fn the_fields_hit_rect_rides_the_scroll_and_stops_at_the_track() {
    let _serial = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut screen = fixture.screen();
    let floor = crate::ui::widgets::TOP_BAR_BOTTOM;
    let at_rest = stop_of(&screen, &fixture, None, FIELD).unwrap();
    assert_eq!((at_rest.rect.y, at_rest.rect.h), (layout::FIELD.y, layout::FIELD.h),
        "an unscrolled screen must cost nothing: this is FIELD itself");
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), layout::FIELD.cy()), Some(FIELD));

    screen.scroll.jump(layout::FIELD.y - floor + 20.0);
    let part = stop_of(&screen, &fixture, None, FIELD).unwrap();
    let visible = part.rect.intersect(part.clip);
    assert_eq!(visible.y, floor, "floored at the track, not dropped");
    assert!((visible.h - (layout::FIELD.h - 20.0)).abs() < 0.001,
        "only the part under the track is taken: y={} h={}", visible.y, visible.h);
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), floor + 10.0), Some(FIELD));

    screen.scroll.jump(layout::FIELD.y + layout::FIELD.h - floor + 0.5);
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), floor), None,
        "once the whole box is behind the track it is not a target at all");
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `results.rs`'s `revealing_the_second_shelf_carries_the_query_field_under_the_track`:
/// the whole document travels, head included. Revealing shelf 0 scrolls nothing; revealing shelf 1
/// takes the field entirely under the tab track, where it is no longer a pointer target.
#[test]
fn revealing_the_second_shelf_carries_the_query_field_under_the_track() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("reveal").shelves(vec![shelf(Kind::Movie, "r0", 6), shelf(Kind::Show, "r1", 6)]);
    let mut screen = fixture.screen();
    let mut engine = seated(&screen, &fixture);

    step_dir(&mut screen, &fixture, &mut engine, Dir::Down);
    assert_eq!(screen.scroll_target, 0.0, "the first shelf is already on screen");
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), layout::FIELD.cy()), Some(FIELD));

    step_dir(&mut screen, &fixture, &mut engine, Dir::Down);
    assert!(screen.scroll_target > 0.0, "the second shelf is below the fold and must be revealed");
    for i in 0..120 { frame(&mut screen, &fixture, &engine, i); }
    let floor = crate::ui::widgets::TOP_BAR_BOTTOM;
    assert!(screen.scroll.pos > layout::FIELD.y + layout::FIELD.h - floor,
        "the field must end up wholly under the track (scroll {})", screen.scroll.pos);
    assert!(stop_of(&screen, &fixture, None, FIELD)
        .is_some_and(|stop| stop.rect.intersect(stop.clip).h <= 0.0));
    assert_eq!(hit(&screen, &fixture, &[FIELD], layout::FIELD.cx(), floor), None,
        "a query box carried into the chrome is not something a click can raise the keyboard on");
    crate::stores::search::apply(SearchCmd::Reset);
}

// ---- motion: every spring this instance owns owes the gate both halves ------------------------

/// Legacy `the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest` — `ui/CLAUDE.md`'s
/// two halves, because the failures are opposite and each is invisible to the other's gate. An
/// over-reporting animator costs the whole idle saving while every fps floor still passes.
///
/// Graded on the DISPATCHER's gate (`Present`), which is what an owned page's `Effects` reach;
/// `ui::idle` is the loop's gate and the bridge ORs the two.
#[test]
fn the_scroll_spring_reports_while_it_runs_and_goes_quiet_at_rest() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("scroll").shelves(vec![shelf(Kind::Movie, "s", 4)]);
    let mut screen = fixture.screen();
    let engine = seated(&screen, &fixture);
    assert!(!frame(&mut screen, &fixture, &engine, 100),
        "a Search screen with nothing moving must stop repainting");
    assert!(!frame(&mut screen, &fixture, &engine, 101), "…and stays quiet frame after frame");

    screen.scroll.jump(400.0);
    assert!(frame(&mut screen, &fixture, &engine, 102), "a scrolling shelf must keep the panel awake");
    let mut frames = 0;
    while frame(&mut screen, &fixture, &engine, 103 + frames) && frames < 600 { frames += 1; }
    assert!(frames < 600, "the scroll must arrive, not ring forever");
    assert!(screen.scroll.pos.abs() < 0.25, "with focus off the shelves the flow rests at zero");
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `the_fields_focus_fade_runs_when_focus_leaves_it_and_settles`: the field's two faces
/// cross-fade, so the fade owes the gate the same two halves. It mounts SEATED — a focused field
/// gliding up from idle would report motion for the first frames of a screen that is not moving.
#[test]
fn the_fields_focus_fade_runs_when_focus_leaves_it_and_settles() {
    let _serial = crate::testlock::serial();
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
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `the_caret_blinks_and_reports_only_on_the_flip`: the blink is a CLOCK, so it must ask
/// for a frame when the bar changes state and for nothing in between — a blink that reported every
/// frame would cost the whole idle saving to animate one 5px bar.
#[test]
fn the_caret_blinks_and_reports_only_on_the_flip() {
    let _serial = crate::testlock::serial();
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
    crate::stores::search::apply(SearchCmd::Reset);
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
    let _serial = crate::testlock::serial();
    let session = remembering("owned-search-mount", &["gromit"]);
    let mut fixture = Fixture::new();

    // A profile that has never searched: the field opens empty, nothing was ever asked.
    let screen = fixture.screen();
    let mut probe = String::new();
    <SearchScreen as Screen<HostFixture>>::state(&screen).probe(&mut probe);
    assert_eq!(crate::search::state(), crate::search::State::Idle);
    assert!(probe.contains("editing=false") && probe.contains("caret=0") && probe.contains("rows=0"));
    assert_eq!(screen.scroll_target, 0.0);
    assert_eq!(screen.hot.pos, 1.0, "the field mounts focused and SEATED, or it reports motion on arrival");

    // A search in progress, with a cursor deep in its results: mounting again over it keeps the
    // query, its generation and its shelves, and re-seats the posture.
    fixture.query("wallace").shelves(vec![shelf(Kind::Movie, "mount", 8)]);
    let generation = crate::search::query_gen();
    let mut screen = fixture.screen();
    screen.scroll.jump(400.0);
    screen.scroll_target = 400.0;
    let (_, out, _) = deliver(&mut screen, &fixture, None, ScreenEvent::Mount);
    assert_eq!(crate::search::query(), "wallace", "a mount must not wipe the term still on screen");
    assert_eq!(crate::search::query_gen(), generation, "…nor supersede the answer under it");
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
    assert!(screen.recents.is_empty() && screen.scroll_target == 0.0, "every cursor parks");
    drop(session);
    crate::stores::search::apply(SearchCmd::Reset);
}

// ---- the television's own keyboard: the four keys it delegates --------------------------------

/// Legacy `the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle` and
/// `backspace_only_bites_while_the_panel_is_up_and_takes_a_whole_character`. The panel sends four
/// keys and three of them once did nothing, which is how it shipped with visibly dead buttons.
/// Cyrillic throughout: two bytes a letter, so a byte step would split a codepoint.
#[test]
fn the_panels_edit_keys_move_the_caret_clear_the_field_and_type_in_the_middle() {
    let _serial = crate::testlock::serial();
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
        key_event(Key::Other, crate::ui::consts::SDLK_BACKSPACE));
    assert_eq!(handled, Handled::Yes, "editing, the screen consumes it");
    assert_eq!(screen.draft.query(), "суббота", "a whole codepoint, not one byte of a two-byte char");

    // Clear all is the whole field, wherever the caret was standing.
    deliver(&mut screen, &fixture, field, key_event(Key::Right, 0));
    let (handled, _, _) = deliver(&mut screen, &fixture, field,
        key_event(Key::Other, crate::ui::consts::SDLK_CLEAR));
    assert_eq!(handled, Handled::Yes, "the panel's Clear all is the screen's key");
    assert_eq!((screen.draft.query(), screen.draft.caret()), ("", 0));

    // An empty field still consumes Backspace — it is the field's — and edits nothing.
    let (handled, out, _) = deliver(&mut screen, &fixture, field,
        key_event(Key::Other, crate::ui::consts::SDLK_BACKSPACE));
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
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down`: the
/// result set stays still under a user who is still typing, and ▼ off the field lands on shelf 0
/// without the page jumping under it.
#[test]
fn the_shelf_flow_is_frozen_unless_the_shelves_hold_focus_with_the_keyboard_down() {
    let _serial = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.query("frozen").shelves(vec![shelf(Kind::Movie, "f0", 4), shelf(Kind::Show, "f1", 4),
        shelf(Kind::Episode, "f2", 4)]);
    let mut screen = fixture.screen();
    let deep = screen.key(screen.rows[2].elems[0]);
    let field = screen.key(FIELD);

    // The one case that scrolls at all: the shelves hold focus with the keyboard down.
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::FocusMoved {
        from: Some(field), to: deep, by: By::Dir });
    assert!(screen.scroll_target > 0.0);

    // The keyboard goes up: the flow parks at zero under a user who is still typing, and stays
    // there for as long as the panel is up.
    deliver(&mut screen, &fixture, Some(field), ScreenEvent::Activate(FIELD));
    assert!(screen.editing);
    assert_eq!(screen.scroll_target, 0.0, "the result set stays still while the panel is up");
    deliver(&mut screen, &fixture, Some(field), ScreenEvent::FocusMoved {
        from: Some(deep), to: field, by: By::Dir });
    assert_eq!(screen.scroll_target, 0.0);

    // A step off the field DROPS the panel first — which is what makes "the flow moves only with
    // the keyboard down" true by construction rather than by a second frozen flag.
    deliver(&mut screen, &fixture, Some(deep), ScreenEvent::FocusMoved {
        from: Some(field), to: deep, by: By::Dir });
    assert!(!screen.editing, "leaving the field takes the television's keyboard with it");
    assert!(screen.scroll_target > 0.0);

    // …and whenever the shelves do not hold focus, the flow is back at zero.
    for elem in [FIELD, screen.rows[0].elems[0]] {
        let key = screen.key(elem);
        deliver(&mut screen, &fixture, Some(key), ScreenEvent::FocusMoved {
            from: Some(deep), to: key, by: By::Dir });
        assert_eq!(screen.scroll_target, 0.0, "elem {elem} is not below the fold");
    }
    crate::stores::search::apply(SearchCmd::Reset);
}

// ---- the borrowed-source annotation -----------------------------------------------------------

/// Three sources on one shelf: the household's own, and two shares with different handles.
fn shared_shelf(fixture: &mut Fixture) -> [crate::plex::ServerId; 3] {
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    let own = crate::plex::register_for_test("own-machine", "127.0.0.1", 1, "own", "annotation");
    let a = crate::plex::register_for_test("share-a", "127.0.0.1", 2, "a", "annotation");
    let b = crate::plex::register_for_test("share-b", "127.0.0.1", 3, "b", "annotation");
    crate::plex::describe_server(own, "own-machine", "", true);
    crate::plex::describe_server(a, "share-a", "friend", false);
    crate::plex::describe_server(b, "share-b", "other", false);
    let item = |sid| Item::Media(crate::pms::PmsMovie { sid, rk: format!("annotated-{sid:?}"),
        title: "Synthetic result".into(), ..Default::default() });
    fixture.query("annotated").shelves(vec![
        Shelf { kind: Kind::Movie, items: vec![item(own), item(a), item(b)] },
        Shelf { kind: Kind::Show, items: vec![item(own)] }]);
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
    let _serial = crate::testlock::serial();
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
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(SearchCmd::Reset);
}

/// Legacy `results.rs`'s `a_settled_annotation_goes_quiet_and_a_moving_one_does_not` — the other
/// half of the same spring, on the dispatcher's gate: adopting a source invalidates once, the
/// rising spring asks every frame while it travels, and a settled annotation asks for nothing.
#[test]
fn a_settled_annotation_goes_quiet_and_a_moving_one_does_not() {
    let _serial = crate::testlock::serial();
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
    crate::plex::reset_servers_for_test();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::stores::search::apply(SearchCmd::Reset);
}
