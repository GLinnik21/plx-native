// A section's Recommended rows page like Home's: a row asks for its next window at its trailing
// edge, and the page tells the store which rows it holds cards for, only when that changes.
use super::*;

const GENRE: (&str, &str, &str) = ("movie.genre", "/hubs/sections/1/genre/5", "Genre");

fn fixture_with(rows: &[(String, String, String)], per_row: usize) -> Fixture {
    let mut fixture = Fixture::new();
    let stores = plx_data::stores::Stores::default();
    stores.browse.borrow_mut().seed_two_source_table_for_test();
    stores.capture_browse(&mut fixture.directory);
    stores.browse_run(BrowseCmd::SetCur(0));
    {
        let mut browse = stores.browse.borrow_mut();
        browse.seed_items_for_test(120);
        let named: Vec<(&str, &str, &str)> = rows.iter().map(|(a, b, c)| (a.as_str(), b.as_str(), c.as_str())).collect();
        browse.seed_named_shelves_for_test(0, &named, per_row);
        browse.seed_paging_for_test(0, 500);
    }
    let publication = stores.capture_browse(&mut fixture.directory);
    fixture.listing = publication.listing;
    fixture.hubs = publication.section_hubs;
    fixture.stores = Some(stores);
    fixture
}

fn genre_and_plain() -> Fixture {
    fixture_with(&[(GENRE.0.into(), GENRE.1.into(), GENRE.2.into()), ("movie.plain".into(), String::new(), "Plain".into())], 12)
}

fn focus_shelf_card(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, shelf: usize, col: usize) {
    let key = page.key(page.shelves[shelf].elems[col]);
    engine.set(OWNER, key, Some(page.shelves[shelf].group), By::Dir);
    deliver(page, engine, fixture, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir });
}

fn works(page: &mut LibraryScreen, engine: &FocusEngine<u32>, fixture: &Fixture, ms: u32) -> Vec<LibraryWork> {
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&ScreenEvent::Tick(Tick { ms, dt_us: 16_667 }), &fixture.cx(engine.current(OWNER)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    out.into_iter().filter_map(|e| match e.fx {
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed { work, .. }))) => Some(work),
        _ => None,
    }).collect()
}

fn pages(works: &[LibraryWork]) -> Vec<(String, bool)> {
    works.iter().filter_map(|w| match w {
        LibraryWork::HubPage { id, before, .. } => Some((id.clone(), *before)),
        _ => None,
    }).collect()
}

fn holds(works: &[LibraryWork]) -> Vec<(usize, usize)> {
    works.iter().filter_map(|w| match w { LibraryWork::HubHold { lo, hi } => Some((*lo, *hi)), _ => None }).collect()
}

#[test]
fn a_shelf_asks_for_its_next_window_at_its_trailing_edge_once() {
    let _guard = plx_base::testlock::serial();
    let fixture = genre_and_plain();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 3);
    let early: Vec<_> = (0..4).flat_map(|i| pages(&works(&mut page, &engine, &fixture, 9000 + i * 17))).collect();
    assert!(early.is_empty(), "the middle of the row asks for nothing: {early:?}");
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 9);
    let asked: Vec<_> = (0..6).flat_map(|i| pages(&works(&mut page, &engine, &fixture, 9100 + i * 17))).collect();
    assert_eq!(asked, vec![("movie.genre".to_string(), false)], "one ask per window at the trailing edge");
}

#[test]
fn a_row_with_no_pageable_key_never_asks() {
    let _guard = plx_base::testlock::serial();
    let fixture = genre_and_plain();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    focus_shelf_card(&mut page, &mut engine, &fixture, 1, 11);
    let asked: Vec<_> = (0..6).flat_map(|i| pages(&works(&mut page, &engine, &fixture, 9000 + i * 17))).collect();
    assert!(asked.is_empty(), "{asked:?}");
}

#[test]
fn the_held_range_is_sent_when_it_changes_and_not_otherwise() {
    let _guard = plx_base::testlock::serial();
    let rows: Vec<_> = (0..30).map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let fixture = fixture_with(&rows, 12);
    let mut page = fixture.screen();
    page.initial = false;
    let engine = FocusEngine::new();
    let sent: Vec<_> = (0..5).flat_map(|i| holds(&works(&mut page, &engine, &fixture, 9000 + i * 17))).collect();
    assert_eq!(sent.len(), 1, "an unchanged range is not sent again: {sent:?}");
    assert_eq!(sent[0].0, 0);
    let y = page.layout.shelf_reveal(&page.run, 20);
    page.scroll.jump(y);
    page.scroll_target = y;
    let moved: Vec<_> = (0..3).flat_map(|i| holds(&works(&mut page, &engine, &fixture, 9200 + i * 17))).collect();
    assert_eq!(moved.len(), 1, "a changed range is sent once: {moved:?}");
    assert!(moved[0].0 > 10 && moved[0].0 <= 20 && moved[0].1 >= 20, "{moved:?}");
}

#[test]
fn a_row_the_store_gave_its_cards_up_is_laid_out_as_placeholders_and_keeps_its_length() {
    let _guard = plx_base::testlock::serial();
    let rows: Vec<_> = (0..30).map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let mut fixture = fixture_with(&rows, 12);
    let page = fixture.screen();
    assert_eq!(page.shelves.len(), 30, "every row is there");
    let shelves = fixture.hubs.view().shelves();
    assert!(!shelves[0].released() && shelves[20].released(), "only the first sixteen rows hold cards");
    assert_eq!(page.shelves[20].elems.len(), 12, "a released row keeps its card count as slots");
    let id = fixture.listing.view().id().unwrap();
    let stores = fixture.stores.take().unwrap();
    stores.browse_run(BrowseCmd::Addressed {
        target: plx_data::stores::browse::SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section },
        work: LibraryWork::HubHold { lo: 20, hi: 25 },
    });
    let publication = stores.capture_browse(&mut fixture.directory);
    fixture.listing = publication.listing;
    fixture.hubs = publication.section_hubs;
    fixture.stores = Some(stores);
    let page = fixture.screen();
    assert!(fixture.hubs.view().shelves()[0].released(), "a row left behind gave its cards up");
    assert_eq!(page.shelves[0].elems.len(), 12, "and still lays out its twelve slots");
    let src = draw::HubSrc { elems: &page.shelves[0].elems, shelf: fixture.hubs.view().shelves().first(), paging: false };
    assert!(!<draw::HubSrc as plx_ui::cards::CardSource<HostFixture>>::loaded(&src, 0), "a slot draws nothing");
}

/// The shelf and column a key sits at.
fn shelf_cell(page: &LibraryScreen, elem: u32) -> Option<(usize, usize)> {
    page.shelves.iter().enumerate().find_map(|(row, shelf)| shelf.elems.iter().position(|e| *e == elem).map(|col| (row, col)))
}

#[test]
fn holding_down_onto_a_row_whose_cards_have_not_landed_keeps_row_and_column_when_they_land() {
    let _guard = plx_base::testlock::serial();
    let rows: Vec<_> = (0..30).map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let mut fixture = fixture_with(&rows, 12);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let col = 5;
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, col);
    // Down faster than the reads land: the key walks onto rows the ring has given up.
    let target = 18; // past the sixteen rows the ring holds
    for _ in 0..target { direction(&mut page, &mut engine, &fixture, Dir::Down); }
    let slot = engine.current(OWNER).unwrap();
    assert_eq!(shelf_cell(&page, slot.elem), Some((target, col)), "the key reached the row and kept its column");
    assert!(fixture.hubs.view().shelves()[target].released(), "its cards have not landed");
    let src = draw::HubSrc { elems: &page.shelves[target].elems, shelf: fixture.hubs.view().shelves().get(target), paging: false };
    assert!(!<draw::HubSrc as plx_ui::cards::CardSource<HostFixture>>::loaded(&src, col), "the card is a placeholder");
    // OK on a placeholder opens nothing
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&ScreenEvent::Activate(slot.elem), &fixture.cx(Some(slot)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.is_empty(), "OK on a placeholder is inert");
    // the ring moves onto the row and its cards land
    let id = fixture.listing.view().id().unwrap();
    let stores = fixture.stores.take().unwrap();
    stores.browse_run(BrowseCmd::Addressed {
        target: plx_data::stores::browse::SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section },
        work: LibraryWork::HubHold { lo: target - 3, hi: target + 3 },
    });
    stores.browse.borrow_mut().land_held_rows_for_test(0);
    let publication = stores.capture_browse(&mut fixture.directory);
    fixture.listing = publication.listing;
    fixture.hubs = publication.section_hubs;
    fixture.stores = Some(stores);
    assert!(!fixture.hubs.view().shelves()[target].released(), "the cards landed");
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    let outcome = engine.reconcile(OWNER, &page, &fixture.cx(engine.current(OWNER)));
    if let Outcome::Moved { from, to, by } = outcome {
        deliver(&mut page, &mut engine, &fixture, ScreenEvent::FocusMoved { from, to, by });
    }
    let landed = engine.current(OWNER).unwrap();
    assert_ne!(landed.elem, slot.elem, "the slot key became the card's");
    assert_eq!(shelf_cell(&page, landed.elem), Some((target, col)), "the same row and column");
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&ScreenEvent::Activate(landed.elem), &fixture.cx(Some(landed)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(!out.is_empty(), "and OK on the landed card does something, so the inert press above proves something");
}
