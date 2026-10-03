// The All grid's focus motion sampled over frames, in the delivery order the dispatcher
// guarantees (`ui::fixture::a_key_moves_focus_and_announces_it_before_the_frames_tick`): the
// key's `FocusMoved`, then the frame's `Tick`.
use super::*;

fn frame(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, ms: u32) {
    deliver(page, engine, fixture, ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }));
}

fn settle(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, from: u32) {
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
    let rest = page.pair.detail.rect_at(13, false, 1.0).w;
    let full = page.pair.detail.rect_at(12, true, 1.0).w;
    assert!(full > rest + 1.0, "the settled focused tile is lifted");

    direction(&mut page, &mut engine, &fixture, Dir::Right);
    frame(&mut page, &mut engine, &fixture, 2000);
    let leaving = page.pair.detail.rect_at(12, false, 1.0).w;
    assert!(strictly_between(leaving, rest, full), "one frame after RIGHT the old tile is mid-return: {leaving}");
    settle(&mut page, &mut engine, &fixture, 2016);
    assert_eq!(page.pair.detail.rect_at(12, false, 1.0).w, rest);

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
/// lands some frames later. Across that wait the chrome around the dark grid — shelves, heading,
/// TYPE chip — must not move, and the incoming cards must first draw where they settle.
#[test]
fn a_type_switch_never_draws_the_empty_interim_layout() {
    use crate::browse::LibraryType;
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 12);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    settle(&mut page, &mut engine, &fixture, 0);
    let chip = page.key(TYPE);
    engine.set(OWNER, chip, Some(page.toolbar), By::Restore);
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from: None, to: chip, by: By::Dir });
    settle(&mut page, &mut engine, &fixture, 2000);
    let head = |page: &LibraryScreen| page.layout.grid_block_top() - page.scroll.pos;
    let at_rest = head(&page);
    assert!(page.scroll.pos > 0.0, "the heading sits below shelves, so the page is scrolled to it");

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
            { let r = page.pair.detail.rect_at(0, false, 1.0); first.push([r.x, r.y, r.w, r.h]); }
        }
    }
    assert!(!first.is_empty() && first.iter().all(|r| *r == first[first.len() - 1]),
        "the incoming cards first draw where they settle: {first:?}");
}

/// Which half of an incoming section's document answers first.
#[derive(Clone, Copy, Debug)]
enum Arrival { GridFirst, HubsFirst, Together }

/// Owner report item 9, the LIBRARY half: entering another section (Movies → TV Shows). The
/// section's listing and its shelves are two independent fetches (`browse::section_hubs`' module
/// doc), and the shelves set the grid's absolute offset — so whichever order they answer in, the
/// incoming grid must first draw where it settles, never above it and then jump.
fn a_section_switch_draws_card_zero_where_it_settles(order: Arrival) -> LibraryScreen {
    a_section_switch_settles(order, 120, None)
}

/// `restore` is a bookmarked scroll (`ListingView::cursor`) the incoming section reopens at.
fn a_section_switch_settles(order: Arrival, items: usize, restore: Option<f32>) -> LibraryScreen {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 12);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    settle(&mut page, &mut engine, &fixture, 0);

    let stores = fixture.stores.take().unwrap();
    stores.browse_run(BrowseCmd::SetCur(1));
    let publish = |fixture: &mut Fixture, grid: bool, hubs: bool| {
        {
            let mut browse = stores.browse.borrow_mut();
            if grid { browse.seed_items_for_test(items); }
            if hubs { browse.seed_shelves_for_test(1, &["show.inprogress.2", "show.recentlyadded.2"], 12); }
        }
        let publication = stores.capture_browse(&mut fixture.directory);
        fixture.listing = publication.listing;
        if !grid { fixture.listing = fixture.listing.clone().with_fetch(SecFetch::Loading, -1).with_total(0); }
        if let Some(scroll) = restore {
            fixture.listing = fixture.listing.clone().with_cursor(crate::stores::browse::Cursor {
                at: crate::stores::browse::CursorAt::SlotIndex(0), scroll });
        }
        fixture.hubs = publication.section_hubs;
    };
    let (first, second) = match order {
        Arrival::GridFirst => ((true, false), (true, true)),
        Arrival::HubsFirst => ((false, true), (true, true)),
        Arrival::Together => ((true, true), (true, true)),
    };
    publish(&mut fixture, first.0, first.1);
    assert!(fixture.hubs.view().id().is_some());
    let mut ms = 4000;
    let mut drawn = Vec::new();
    for i in 0..120 {
        if i == 10 { publish(&mut fixture, second.0, second.1); }
        page.sync(&fixture.cx(engine.current(OWNER)));
        frame(&mut page, &mut engine, &fixture, ms);
        ms += 16;
        if page.page_fade.alpha() > 0.0 && page.grid_fade.alpha() > 0.0 && !page.pair.detail.elems.is_empty() {
            let r = page.pair.detail.rect_at(0, false, 1.0);
            drawn.push((i, [r.x, r.y, r.w, r.h]));
        }
    }
    assert!(!page.shelves.is_empty(), "the incoming section's shelves committed");
    let settled = drawn.last().expect("the incoming grid was drawn").1;
    let wrong: Vec<_> = drawn.iter().filter(|(_, r)| *r != settled).collect();
    assert!(wrong.is_empty(), "{order:?}: card 0 drew away from its settled rect {settled:?} on {wrong:?}");
    page
}

#[test]
fn a_section_switch_whose_grid_answers_before_its_shelves_draws_the_grid_where_it_settles() {
    a_section_switch_draws_card_zero_where_it_settles(Arrival::GridFirst);
}

#[test]
fn a_section_switch_whose_shelves_answer_before_its_grid_draws_the_grid_where_it_settles() {
    a_section_switch_draws_card_zero_where_it_settles(Arrival::HubsFirst);
}

#[test]
fn a_section_switch_answered_all_at_once_draws_the_grid_where_it_settles() {
    a_section_switch_draws_card_zero_where_it_settles(Arrival::Together);
}

/// A bookmarked scroll belongs to the document WITH its shelves: applied (and clamped) against
/// the shelfless interim it lands short, and the grid then draws somewhere the reader never left.
#[test]
fn a_section_switch_restores_its_bookmark_against_the_settled_document() {
    let saved = 900.0;
    for order in [Arrival::GridFirst, Arrival::HubsFirst, Arrival::Together] {
        let page = a_section_switch_settles(order, 12, Some(saved));
        assert_eq!(page.scroll.pos, saved.min(page.layout.max_scroll()), "{order:?}: the bookmark reopened where it was saved");
        assert!(page.layout.max_scroll() >= saved, "the fixture's settled document can hold the bookmark");
    }
}
