// The All grid's focus motion sampled over frames, in the delivery order the dispatcher
// guarantees (`ui::fixture::a_key_moves_focus_and_announces_it_before_the_frames_tick`): the
// key's `FocusMoved`, then the frame's `Tick`.
use super::*;

pub(super) fn frame(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, ms: u32) {
    deliver(page, engine, fixture, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
}

pub(super) fn settle(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, from: u32) {
    for i in 0..120 { frame(page, engine, fixture, from + i * 16); }
}

fn strictly_between(v: f32, a: f32, b: f32) -> bool { v > a.min(b) + 1.0e-3 && v < a.max(b) - 1.0e-3 }

/// Owner report items 4, 5 and 7: the tile losing focus returns to rest over frames, and the
/// rows open and close their caption band over frames — sideways, down, and out of the grid.
#[test]
fn an_all_grid_tile_and_its_rows_animate_back_when_focus_leaves() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let start = page.key(page.pair.detail.elem_at(12).unwrap());
    engine.set(OWNER, start, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: start, by: By::Restore });
    settle(&mut page, &mut engine, &fixture, 0);
    let rest = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 13).w;
    let full = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 12).w;
    assert!(full > rest + 1.0, "the settled focused tile is lifted");

    direction(&mut page, &mut engine, &fixture, Dir::Right);
    frame(&mut page, &mut engine, &fixture, 2000);
    let leaving = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 12).w;
    assert!(strictly_between(leaving, rest, full), "one frame after RIGHT the old tile is mid-return: {leaving}");
    settle(&mut page, &mut engine, &fixture, 2016);
    assert_eq!(page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 12).w, rest);

    let below = page.layout.row_y(4, page.scroll.pos);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    frame(&mut page, &mut engine, &fixture, 5000);
    assert!(strictly_between(page.layout.row_expansion(2), 0.0, 1.0), "row 2 closes over frames");
    assert!(strictly_between(page.layout.row_expansion(3), 0.0, 1.0), "row 3 opens over frames");
    settle(&mut page, &mut engine, &fixture, 5016);
    assert!(page.layout.row_y(4, page.scroll.pos) != below, "the settled layout moved row 4");

    for _ in 0..4 { direction(&mut page, &mut engine, &fixture, Dir::Up); }
    assert!(engine.current(OWNER).is_some_and(|key| page.pair.detail.index_of(key.elem).is_none()),
        "focus left the grid for its heading");
    frame(&mut page, &mut engine, &fixture, 9000);
    assert!(strictly_between(page.layout.row_expansion(3), 0.0, 1.0), "leaving the grid closes its band over frames");
}

/// Owner report items 8 and 9: switching TYPE (Shows / Seasons / Episodes) in an All grid
/// scrolled to its heading. The grid fades out, the store empties on the commit, and the answer
/// lands some frames later. Across that wait the chrome around the dark grid — heading,
/// TYPE chip — must not move, and the incoming cards must first draw where they settle.
#[test]
fn a_type_switch_never_draws_the_empty_interim_layout() {
    use plx_data::browse::LibraryType;
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    settle(&mut page, &mut engine, &fixture, 0);
    let chip = page.key(page.pair.detail.elem_at(0).unwrap());
    engine.set(OWNER, chip, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: chip, by: By::Dir });
    settle(&mut page, &mut engine, &fixture, 2000);
    let head = |page: &LibraryScreen| page.layout.grid_top() - page.scroll.pos;
    let at_rest = head(&page);

    let (mut out, mut present) = (Vec::new(), plx_machine::present::Present::new());
    page.command(LibraryCmd::SetType(LibraryType::Seasons), &fixture.cx(engine.current(OWNER)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    let old = fixture.listing.clone();
    let mut ms = 4000;
    for _ in 0..12 {
        let pending = page.pending.grid().is_some();
        frame(&mut page, &mut engine, &fixture, ms);
        ms += 16;
        if pending && page.pending.grid().is_none() {
            // `browse::requery`: the same section under a new query, emptied and loading
            fixture.listing = old.clone().with_query(99).with_library_type(LibraryType::Seasons)
                .with_fetch(SecFetch::Loading, -1).with_total(0);
        }
        assert!((head(&page) - at_rest).abs() < 0.5, "the heading moved while the grid waited on its answer: {} vs {at_rest}", head(&page));
    }
    assert!(page.pending.grid().is_none(), "the transaction committed");
    fixture.listing = old.with_query(99).with_library_type(LibraryType::Seasons)
        .with_fetch(SecFetch::Ready, 30).with_total(30);
    let mut first = Vec::new();
    for _ in 0..30 {
        frame(&mut page, &mut engine, &fixture, ms);
        ms += 16;
        assert!((head(&page) - at_rest).abs() < 0.5, "the heading moved when the answer landed: {} vs {at_rest}", head(&page));
        if page.grid_fade.alpha() > 0.0 {
            { let r = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 0); first.push([r.x, r.y, r.w, r.h]); }
        }
    }
    assert!(!first.is_empty() && first.iter().all(|r| *r == first[first.len() - 1]),
        "the incoming cards first draw where they settle: {first:?}");
}

/// Owner report item 9: entering another section (Movies → TV Shows). The section's listing
/// answers some frames after the switch (`Loading` first), and the incoming grid must first draw
/// where it settles, never above it and then jump. `restore` is a bookmarked scroll
/// (`ListingView::cursor`) the incoming section reopens at.
fn a_section_switch_settles(items: usize, restore: Option<f32>) -> LibraryScreen {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::shelves(&["movie.inprogress.1"], 12);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    settle(&mut page, &mut engine, &fixture, 0);

    let stores = fixture.stores.take().unwrap();
    stores.browse_run_for_test(BrowseCmd::SetCur(1));
    let publish = |fixture: &mut Fixture, grid: bool| {
        if grid { stores.browse.borrow_mut().seed_items_for_test(items); }
        let publication = stores.capture_browse(&mut fixture.directory);
        fixture.listing = publication.listing;
        if !grid { fixture.listing = fixture.listing.clone().with_fetch(SecFetch::Loading, -1).with_total(0); }
        if let Some(scroll) = restore {
            fixture.listing = fixture.listing.clone().with_cursor(plx_data::stores::browse::Cursor {
                at: plx_data::stores::browse::CursorAt::SlotIndex(0), scroll });
        }
    };
    publish(&mut fixture, false);
    let mut ms = 4000;
    let mut drawn = Vec::new();
    for i in 0..120 {
        if i == 10 { publish(&mut fixture, true); }
        page.sync(&fixture.cx(engine.current(OWNER)));
        frame(&mut page, &mut engine, &fixture, ms);
        ms += 16;
        if page.page_fade.alpha() > 0.0 && page.grid_fade.alpha() > 0.0 && page.pair.detail.total() > 0 {
            let r = page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), 0);
            drawn.push((i, [r.x, r.y, r.w, r.h]));
        }
    }
    let settled = drawn.last().expect("the incoming grid was drawn").1;
    let wrong: Vec<_> = drawn.iter().filter(|(_, r)| *r != settled).collect();
    assert!(wrong.is_empty(), "card 0 drew away from its settled rect {settled:?} on {wrong:?}");
    page
}

#[test]
fn a_section_switch_draws_the_grid_where_it_settles() {
    a_section_switch_settles(120, None);
}

/// A bookmarked scroll is applied against the settled document, not the empty interim.
#[test]
fn a_section_switch_restores_its_bookmark_against_the_settled_document() {
    let saved = 900.0;
    let page = a_section_switch_settles(120, Some(saved));
    assert_eq!(page.scroll.pos, saved.min(page.layout.max_scroll()), "the bookmark reopened where it was saved");
    assert!(page.layout.max_scroll() >= saved, "the fixture's settled document can hold the bookmark");
}

/// A movie listing of items `rks`, each identified by its number (an element keeps its identity
/// across a landing that moves it).
pub(super) fn numbered(rks: std::ops::Range<usize>) -> plx_data::stores::browse::ListingSnapshot {
    let sid = plx_plex::plex::ServerId::from_raw(0);
    let total = rks.len() as i64;
    plx_data::browse::view::ListingSnapshot::fixture(sid, rks.map(|i|
        Some(plx_data::pms::PmsMovie { sid, rk: format!("{i}"), title: format!("s{i:04x}"), ..Default::default() })).collect(),
        vec![("A".into(), total)])
}

/// A page on `before` with the item numbered `focus` seated and settled.
fn seated(before: std::ops::Range<usize>, focus: usize) -> (Fixture, LibraryScreen, FocusEngine<u32>, u32) {
    let mut fixture = Fixture::new();
    fixture.listing = numbered(before.clone());
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let elem = crate::library::tests::deep(&mut page, &fixture, focus - before.start);
    let key = page.key(elem);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore });
    settle(&mut page, &mut engine, &fixture, 0);
    (fixture, page, engine, elem)
}

/// The listing lands `after` (a requery-free refill: same section, same query) and one frame ticks.
fn land(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &mut Fixture, after: std::ops::Range<usize>, ms: u32) {
    fixture.listing = numbered(after);
    deliver(page, engine, fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    frame(page, engine, fixture, ms);
}

fn drawn_y(page: &LibraryScreen, engine: &FocusEngine<u32>, fixture: &Fixture, elem: u32) -> f32 {
    page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), page.pair.detail.index_of(elem).unwrap()).y
}

/// A content landing that moves the focused element to another row leaves its tile where it was
/// on screen, whichever way the row moved: a row inserted above pushes it down and a row removed
/// above pulls it up, and the caption band the new row opens does not skew the shift.
#[test]
fn a_landing_above_the_focused_tile_leaves_it_where_it_was_on_screen() {
    let _guard = plx_base::testlock::serial();
    for (before, after, what) in [(6..60, 0..60, "a row inserted above"), (0..60, 6..60, "a row removed above")] {
        let (mut fixture, mut page, mut engine, elem) = seated(before, 20);
        let y = drawn_y(&page, &engine, &fixture, elem);
        land(&mut page, &mut engine, &mut fixture, after, 4000);
        assert!((drawn_y(&page, &engine, &fixture, elem) - y).abs() < 0.5,
            "{what}: the tile moved on screen: {y} -> {}", drawn_y(&page, &engine, &fixture, elem));
        settle(&mut page, &mut engine, &fixture, 4016);
        assert!((drawn_y(&page, &engine, &fixture, elem) - y).abs() < 0.5, "{what}: and stayed put once settled");
    }
}

/// The shift never carries the page outside its document: a tile near the head of a barely scrolled
/// page pulled up by a removed row clamps at the head, and one at the foot of a scrolled page
/// pushed down by an inserted row while the tail goes away clamps at the foot.
#[test]
fn a_landing_shift_never_scrolls_the_page_outside_its_document() {
    let _guard = plx_base::testlock::serial();
    let within = |page: &LibraryScreen, what: &str| {
        let max = page.target_layout.max_scroll();
        assert!((0.0..=max).contains(&page.scroll.pos), "{what}: scroll {} outside 0..={max}", page.scroll.pos);
        assert!((0.0..=max).contains(&page.scroll_target), "{what}: target {} outside 0..={max}", page.scroll_target);
    };
    let (mut fixture, mut page, mut engine, _) = seated(0..60, 12);
    page.scroll.jump(50.0);
    page.scroll_target = 50.0;
    land(&mut page, &mut engine, &mut fixture, 6..60, 4000);
    within(&page, "head");

    let (mut fixture, mut page, mut engine, _) = seated(6..60, 50);
    let max = page.target_layout.max_scroll();
    page.scroll.jump(max);
    page.scroll_target = max;
    land(&mut page, &mut engine, &mut fixture, 0..52, 4000);
    within(&page, "foot");
}

/// A requery that empties and refills while the page is covered leaves the grid's pop on a cell
/// of the OLD content. Uncovering must read the refill as new content (the reset jump alone
/// places the page), not as the same element landing at another index and shifting the page
/// by rows on top of the jump.
#[test]
fn a_requery_refilled_under_a_cover_is_not_read_as_a_landing_on_uncover() {
    let _guard = plx_base::testlock::serial();
    let (mut fixture, mut page, mut engine, elem) = seated(30..90, 50);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::Cover);
    fixture.listing = numbered(0..0).with_query(99);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    fixture.listing = numbered(0..90).with_query(99);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1));
    assert!(page.pair.detail.index_of(elem) != Some(20), "the same element sits at a new index");
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::Uncover);
    let jumped = page.scroll.pos;
    frame(&mut page, &mut engine, &fixture, 6000);
    assert!((page.scroll.pos - jumped).abs() < 0.5, "uncovering moved the page by a landing's rows: {jumped} -> {}", page.scroll.pos);
}

/// What `deliver` drops: the effects one event produced.
fn effects_of(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, event: ScreenEvent<HostFixture>)
    -> Vec<plx_machine::machine::Stamped<HostFixture>> {
    let mut output = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&event, &fixture.cx(engine.current(OWNER)),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    for effect in &output {
        if let Fx::Remember { group, elem } = effect.fx { engine.remember_projected(ENTRY, group, elem); }
    }
    output
}

fn saved_cursors(effects: &[plx_machine::machine::Stamped<HostFixture>]) -> Vec<plx_data::browse::CursorAt> {
    effects.iter().filter_map(|effect| match &effect.fx {
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            work: LibraryWork::SaveCursor { cursor, .. }, .. }))) => Some(cursor.at.clone()),
        _ => None,
    }).collect()
}

/// A page that is stale and then buried by a PAGE push: the store re-fetches the same query in the
/// background (emptied and loading, then refilled), and the reader comes back to the same place.
/// `query_moves` is what the store used to do — present the re-fetch as a new query.
fn a_membership_refresh_under_a_page_push(query_moves: bool, refill_before_uncover: bool) {
    use plx_machine::machine::Leave;
    let (mut fixture, mut page, mut engine, elem) = seated(0..90, 50);
    let key = engine.current(OWNER).unwrap();
    let scroll = page.scroll.pos;
    let store = || ScreenEvent::StoreChanged(plx_data::stores::StoreId::Browse.ord(), 1);
    let query = fixture.listing.view().id().unwrap().query;
    let after = |l: plx_data::stores::browse::ListingSnapshot| if query_moves { l.with_query(query + 1) } else { l };

    fixture.listing = numbered(0..90).with_membership_stale();
    deliver(&mut page, &mut engine, &fixture, store());
    let mut effects = effects_of(&mut page, &mut engine, &fixture, ScreenEvent::WillLeave(Leave::Deeper));
    effects.extend(effects_of(&mut page, &mut engine, &fixture, ScreenEvent::Cover));
    assert!(effects.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Store(StoreId::Browse,
        StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Requery, .. }))))), "buried by a page: re-query");

    fixture.listing = after(numbered(0..0).with_fetch(SecFetch::Loading, -1));
    deliver(&mut page, &mut engine, &fixture, store());
    if refill_before_uncover {
        fixture.listing = after(numbered(0..90));
        deliver(&mut page, &mut engine, &fixture, store());
    }
    effects.extend(effects_of(&mut page, &mut engine, &fixture, ScreenEvent::Uncover));
    effects.extend(effects_of(&mut page, &mut engine, &fixture, ScreenEvent::Enter(Enter::Restored)));
    frame(&mut page, &mut engine, &fixture, 6000);
    if !refill_before_uncover {
        fixture.listing = after(numbered(0..90));
        deliver(&mut page, &mut engine, &fixture, store());
    }
    for ms in 0..30 { frame(&mut page, &mut engine, &fixture, 6016 + ms * 16); }

    assert_eq!(engine.current(OWNER), Some(key), "focus stays on the tile the reader left");
    assert!(page.pair.detail.index_of(elem) == Some(50), "the tile is still item 50");
    assert!((page.scroll.pos - scroll).abs() < 0.5, "the page moved: {scroll} -> {}", page.scroll.pos);
    effects.extend(effects_of(&mut page, &mut engine, &fixture, ScreenEvent::WillLeave(Leave::ForGood)));
    let saved = saved_cursors(&effects);
    assert!(!saved.iter().any(|at| matches!(at, plx_data::browse::CursorAt::SlotIndex(0))),
        "the bookmark was reset to the top: {saved:?}");
    assert!(matches!(saved.last(), Some(plx_data::browse::CursorAt::ItemKey { rk, .. }) if rk == "50"),
        "the bookmark names the reader's item: {saved:?}");
}

#[test]
fn a_membership_requery_under_a_page_keeps_focus_and_scroll_when_refilled_before_uncover() {
    let _guard = plx_base::testlock::serial();
    a_membership_refresh_under_a_page_push(false, true);
}

#[test]
fn a_membership_requery_under_a_page_keeps_focus_and_scroll_when_refilled_after_uncover() {
    let _guard = plx_base::testlock::serial();
    a_membership_refresh_under_a_page_push(false, false);
}

/// The Under posters setting reaches the grid: a policy that shows nothing opens no caption band, so
/// focusing a tile moves no row below it, while the default opens one and pushes them down.
#[test]
fn the_under_posters_policy_sizes_the_focused_rows_band() {
    use plx_plex::plex::session::{UnderPosters, UnderPostersDetails};
    let _guard = plx_base::testlock::serial();
    let below_focus = |setting: UnderPosters| {
        crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.set(Some(setting)));
        let fixture = Fixture::new();
        let mut page = fixture.screen();
        page.initial = false;
        let mut engine = FocusEngine::new();
        let rest = page.layout.row_y(4, 0.0);
        let start = page.key(page.pair.detail.elem_at(12).unwrap());
        engine.set(OWNER, start, Some(page.pair.groups_config().detail), By::Restore);
        deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: start, by: By::Restore });
        settle(&mut page, &mut engine, &fixture, 0);
        page.layout.row_y(4, 0.0) - rest
    };
    let nothing = below_focus(UnderPosters { title: false, details: UnderPostersDetails::None, sort_value: false });
    let one_line = below_focus(UnderPosters { title: false, details: UnderPostersDetails::Genre, sort_value: false });
    let two_lines = below_focus(UnderPosters { title: true, details: UnderPostersDetails::Genre, sort_value: false });
    crate::registry::captions::TEST_UNDER_POSTERS.with(|cell| cell.set(None));
    assert!(nothing.abs() < 0.01, "a policy showing nothing opens no band: {nothing}");
    assert!((one_line - 40.0).abs() < 0.01, "one line opens 80 - 40 px: {one_line}");
    assert!((two_lines - 72.0).abs() < 0.01, "two lines open 112 - 40 px: {two_lines}");
}

#[test]
fn value_mode_keeps_the_real_library_focus_and_restores_the_caption_geometry() {
    use plx_plex::plex::session::UnderPosters;
    let _guard = plx_base::testlock::serial();
    crate::registry::captions::TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters::DEFAULT)));
    let mut fixture = Fixture::new();
    fixture.listing = fixture.listing.clone().with_sort_key("year");
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let first = page.key(page.pair.detail.elem_at(0).unwrap());
    engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: first, by: By::Restore });
    settle(&mut page, &mut engine, &fixture, 0);
    assert!(page.layout.value_line(), "Sort value On and a sort with a value: the grid shows values");
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    settle(&mut page, &mut engine, &fixture, 2000);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((1, 1)));
    let focus = engine.current(OWNER);
    let pitch = page.layout.grid_pitch();
    assert_eq!(pitch - page.layout.card_h(), 60.0, "rows are 60 px apart while values show");
    // Sort value Off: the grid draws as it did before, 40 px rows and the Title/Details band.
    crate::registry::captions::TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters { sort_value: false, ..UnderPosters::DEFAULT })));
    frame(&mut page, &mut engine, &fixture, 5000);
    assert!(!page.layout.value_line());
    assert_eq!(page.layout.grid_pitch() - page.layout.card_h(), 40.0);
    assert_eq!(engine.current(OWNER), focus, "turning values off keeps the selected poster");
    // Back On, but sorted by Title: no value to show, so the same plain geometry.
    crate::registry::captions::TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters::DEFAULT)));
    fixture.listing = fixture.listing.clone().with_sort_key("titleSort");
    frame(&mut page, &mut engine, &fixture, 8000);
    assert!(!page.layout.value_line(), "Title and Random sorts show no value");
    assert_eq!(page.layout.grid_pitch() - page.layout.card_h(), 40.0);
    // And a sort a later server declares, which the table does not word, keeps the captions too.
    fixture.listing = fixture.listing.clone().with_sort_key("futureServerSort");
    frame(&mut page, &mut engine, &fixture, 11000);
    assert!(!page.layout.value_line());
    // The same switch DEEP in the grid: the rows' pitch changes (60 -> 40 px) under a scroll held in the
    // old pitch's coordinates, so the page must seat the focused row again the way a focus move does, or
    // the row it stands on is scrolled up under the header.
    crate::registry::captions::TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters::DEFAULT)));
    let mut deep = Fixture::new();
    deep.listing = numbered(0..300).with_sort_key("year");
    let mut page = deep.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let elem = crate::library::tests::deep(&mut page, &deep, 90);
    let row = 90 / page.layout.cols();
    let key = page.key(elem);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    deliver(&mut page, &mut engine, &deep, ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore });
    settle(&mut page, &mut engine, &deep, 0);
    assert!(page.layout.value_line() && row >= 10, "values show at a deep row ({row})");
    // Where a focus move seats a row: its poster top where the first row's is at rest.
    let seated = |page: &LibraryScreen| page.layout.row_y(row, page.scroll.pos) - page.layout.row_y(0, 0.0);
    assert!(seated(&page).abs() < 1.0, "with values the focused row is seated at the grid top: {}", seated(&page));
    crate::registry::captions::TEST_UNDER_POSTERS.with(|c| c.set(Some(UnderPosters { sort_value: false, ..UnderPosters::DEFAULT })));
    frame(&mut page, &mut engine, &deep, 5000);
    assert!(!page.layout.value_line());
    assert_eq!(engine.current(OWNER), Some(key), "turning values off keeps the selected poster");
    assert!(seated(&page).abs() < 1.0, "Off at a deep row: the focused row is seated at the grid top, not under the header: {}", seated(&page));
    settle(&mut page, &mut engine, &deep, 5016);
    assert!(seated(&page).abs() < 1.0, "and it stays seated once settled: {}", seated(&page));
    crate::registry::captions::TEST_UNDER_POSTERS.with(|c| c.set(None));
}
