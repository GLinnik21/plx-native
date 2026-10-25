// The Recommended page's rows page like Home's: a row asks for its next window at its trailing
// edge, and the page tells the store which rows it holds cards for, only when that changes.
use super::*;
use super::recommended_tests::{deliver, direction, Fixture, HostFixture, OWNER};
use plx_machine::machine::Tick;
use plx_ui::focus::{FocusEngine, Outcome};

const GENRE: (&str, &str, &str) = ("movie.genre", "/hubs/sections/1/genre/5", "Genre");

fn genre_and_plain() -> Fixture {
    Fixture::named_shelves(&[(GENRE.0.into(), GENRE.1.into(), GENRE.2.into()), ("movie.plain".into(), String::new(), "Plain".into())], 12)
}

fn focus_shelf_card(page: &mut RecommendedScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, shelf: usize, col: usize) {
    let key = page.key(page.shelves[shelf].elems[col]);
    engine.set(OWNER, key, Some(page.shelves[shelf].group), By::Dir);
    deliver(page, engine, fixture, ScreenEvent::FocusMoved { from: None, to: key, by: By::Dir });
}

fn works(page: &mut RecommendedScreen, engine: &FocusEngine<u32>, fixture: &Fixture, ms: u32) -> Vec<LibraryWork> {
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

/// The revision each hub-page ask of `works` was computed from.
fn seen_by(works: &[LibraryWork]) -> Vec<u64> {
    works.iter().filter_map(|w| match w { LibraryWork::HubPage { seen, .. } => Some(*seen), _ => None }).collect()
}

/// **A kept row asks again on the first tick after a publication**, whatever row the publication
/// changed. A frame captures its views before its landings are delivered, so an ask can name a
/// window the store has since replaced, and the store refuses it; the publication it missed
/// rebuilds the changed rows and leaves this one as it stands, and the row's own latch must not
/// hold the refused ask until a rung of the retry ladder (a quarter of a second at the first,
/// climbing to half a minute) while the row's trailing cells paint as skeletons.
#[test]
fn a_kept_rows_refused_ask_is_asked_again_on_the_next_tick_after_a_publication() {
    let _guard = plx_base::testlock::serial();
    let other = ("movie.other".to_string(), "/hubs/sections/1/other/6".to_string(), "Other".to_string());
    let mut fixture = Fixture::named_shelves(&[(GENRE.0.into(), GENRE.1.into(), GENRE.2.into()), other], 12);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 9);
    let (kept, landing) = (page.shelves[0].elems.clone(), page.shelves[1].elems.clone());
    // The store takes a landing on the OTHER row that this frame's views do not carry yet ...
    let id = fixture.listing.view().id().unwrap();
    let stores = fixture.stores.as_ref().unwrap();
    assert!(stores.browse.borrow_mut().land_row_for_test(0, 1, 12), "premise: the other row took the landing");
    // ... so the ask the tick sends names a window the store has left
    let first = works(&mut page, &engine, &fixture, 9000);
    let seen = seen_by(&first);
    assert_eq!(seen.len(), 1, "premise: the trailing edge asks: {first:?}");
    let target = plx_data::stores::browse::SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section };
    for work in first { stores.browse_run_for_test(BrowseCmd::Addressed { target, work }); }
    fixture.republish();
    let now = fixture.hubs.view().revision().expect("a published section");
    assert_ne!(seen[0], now, "premise: the store refused the ask for the window it had left");
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    assert_eq!(page.shelves[0].elems, kept, "premise: the publication left the asking row as it stood");
    assert_ne!(page.shelves[1].elems, landing, "premise: and rebuilt the row it changed");
    // the next tick is well inside the ladder's first rung
    let again = works(&mut page, &engine, &fixture, 9017);
    assert_eq!(pages(&again), vec![("movie.genre".to_string(), false)], "the refused ask goes out again at once");
    assert_eq!(seen_by(&again), vec![now], "from the window that stands");
}

fn cancels(works: &[LibraryWork]) -> Vec<String> {
    works.iter().filter_map(|w| match w { LibraryWork::HubPageCancel { id, .. } => Some(id.clone()), _ => None }).collect()
}

/// A landing slides the window by half, so an ask is only safe while focus stays in the half the
/// window keeps: the first card out of the edge's zone withdraws it, and so does moving to another row.
#[test]
fn a_shelf_withdraws_its_ask_when_focus_leaves_the_edge_it_was_sent_from() {
    let _guard = plx_base::testlock::serial();
    let fixture = genre_and_plain();
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 9);
    let asked: Vec<_> = (0..3).flat_map(|i| pages(&works(&mut page, &engine, &fixture, 9000 + i * 17))).collect();
    assert_eq!(asked.len(), 1);
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 6);
    assert!(cancels(&works(&mut page, &engine, &fixture, 9100)).is_empty(), "still inside the zone");
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 5);
    assert_eq!(cancels(&works(&mut page, &engine, &fixture, 9117)), vec!["movie.genre".to_string()]);
    assert!(cancels(&works(&mut page, &engine, &fixture, 9134)).is_empty(), "once");
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 9);
    let again: Vec<_> = (0..3).flat_map(|i| pages(&works(&mut page, &engine, &fixture, 9200 + i * 17))).collect();
    assert_eq!(again.len(), 1, "the edge asks again on return");
    focus_shelf_card(&mut page, &mut engine, &fixture, 1, 3);
    assert_eq!(cancels(&works(&mut page, &engine, &fixture, 9300)), vec!["movie.genre".to_string()], "another row");
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
    let fixture = Fixture::named_shelves(&rows, 12);
    let mut page = fixture.screen();
    page.initial = false;
    let engine = FocusEngine::new();
    let sent: Vec<_> = (0..5).flat_map(|i| holds(&works(&mut page, &engine, &fixture, 9000 + i * 17))).collect();
    assert_eq!(sent.len(), 1, "an unchanged range is not sent again: {sent:?}");
    assert_eq!(sent[0].0, 0);
    let y = reveal(&page.run, 20);
    page.scroll.jump(y);
    page.scroll_target = y;
    let moved: Vec<_> = (0..3).flat_map(|i| holds(&works(&mut page, &engine, &fixture, 9200 + i * 17))).collect();
    assert_eq!(moved.len(), 1, "a changed range is sent once: {moved:?}");
    assert!(moved[0].0 > 10 && moved[0].0 <= 20 && moved[0].1 >= 20, "{moved:?}");
}

/// Send the store what the page asked of it this frame, let the rows it now holds land, publish the
/// result to the page and let the engine follow a focused card whose key changed with it.
fn settle_holds(
    page: &mut RecommendedScreen, engine: &mut FocusEngine<u32>, fixture: &mut Fixture, asked: &[(usize, usize)],
) {
    if asked.is_empty() { return; }
    let id = fixture.listing.view().id().unwrap();
    let stores = fixture.stores.take().unwrap();
    for &(lo, hi) in asked {
        stores.browse_run_for_test(BrowseCmd::Addressed {
            target: plx_data::stores::browse::SectionAddress { epoch: id.epoch, sid: id.sid, section: id.section },
            work: LibraryWork::HubHold { lo, hi },
        });
    }
    stores.browse.borrow_mut().land_held_rows_for_test(0);
    let publication = stores.capture_browse(&mut fixture.directory);
    fixture.listing = publication.listing;
    fixture.hubs = publication.section_hubs;
    fixture.stores = Some(stores);
    deliver(page, engine, fixture, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    if let Outcome::Moved { from, to, by } = engine.reconcile(OWNER, page, &fixture.cx(engine.current(OWNER))) {
        deliver(page, engine, fixture, ScreenEvent::FocusMoved { from, to, by });
    }
}

/// BACK on a long Recommended page seats focus on row 0 while the view glides up from row 20: the
/// focused row is folded into the rows in view, a span the store (which keeps sixteen rows) cannot
/// hold, so it keeps the middle of it and gives rows 0..4 up. The page must ask again as the glide
/// shortens the span, so that the top of the page it arrives on is held, not placeholders with no
/// read behind them.
#[test]
fn back_to_the_top_from_deep_rows_leaves_the_top_rows_held_not_placeholders() {
    let _guard = plx_base::testlock::serial();
    let rows: Vec<_> = (0..30).map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let mut fixture = Fixture::named_shelves(&rows, 12);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    // deep: the view and the focus on row 21, the store holding the rows around it
    focus_shelf_card(&mut page, &mut engine, &fixture, 21, 0);
    let y = reveal(&page.run, 21);
    page.scroll.jump(y);
    page.scroll_target = y;
    let mut ms = 9000;
    for _ in 0..6 {
        ms += 17;
        let asked = holds(&works(&mut page, &engine, &fixture, ms));
        settle_holds(&mut page, &mut engine, &mut fixture, &asked);
    }
    assert!(fixture.hubs.view().shelves()[0].released(), "premise: the top of the page was given up");
    assert!(!fixture.hubs.view().shelves()[21].released(), "premise: the rows in view are held");
    // BACK: focus seats on row 0 and the view glides up on the scroll spring
    focus_shelf_card(&mut page, &mut engine, &fixture, 0, 0);
    assert!(page.scroll_target < y / 2.0, "premise: the view is sent to the top ({} of {y})", page.scroll_target);
    let mut frames = 0;
    while page.scroll.pos > 0.5 && frames < 400 {
        frames += 1;
        ms += 17;
        let asked = holds(&works(&mut page, &engine, &fixture, ms));
        settle_holds(&mut page, &mut engine, &mut fixture, &asked);
    }
    assert!(page.scroll.pos <= 0.5 && frames > 4, "premise: the glide arrived over frames ({frames})");
    for _ in 0..3 {
        ms += 17;
        let asked = holds(&works(&mut page, &engine, &fixture, ms));
        settle_holds(&mut page, &mut engine, &mut fixture, &asked);
    }
    let shelves = fixture.hubs.view().shelves();
    for row in 0..=4 {
        assert!(!shelves[row].released(), "row {row} of the page the glide arrived on is held, not a placeholder");
    }
}

#[test]
fn a_row_the_store_gave_its_cards_up_is_laid_out_as_placeholders_and_keeps_its_length() {
    let _guard = plx_base::testlock::serial();
    let rows: Vec<_> = (0..30).map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let mut fixture = Fixture::named_shelves(&rows, 12);
    let page = fixture.screen();
    assert_eq!(page.shelves.len(), 30, "every row is there");
    let shelves = fixture.hubs.view().shelves();
    assert!(!shelves[0].released() && shelves[20].released(), "only the first sixteen rows hold cards");
    assert_eq!(page.shelves[20].elems.len(), 12, "a released row keeps its card count as slots");
    let id = fixture.listing.view().id().unwrap();
    let stores = fixture.stores.take().unwrap();
    stores.browse_run_for_test(BrowseCmd::Addressed {
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
    let src = HubSrc { elems: &page.shelves[0].elems, shelf: fixture.hubs.view().shelves().first(), paging: false };
    assert!(!<HubSrc as plx_ui::cards::CardSource<HostFixture>>::loaded(&src, 0), "a slot draws nothing");
}

/// The shelf and column a key sits at.
fn shelf_cell(page: &RecommendedScreen, elem: u32) -> Option<(usize, usize)> {
    page.shelves.iter().enumerate().find_map(|(row, shelf)| shelf.elems.iter().position(|e| *e == elem).map(|col| (row, col)))
}

#[test]
fn holding_down_onto_a_row_whose_cards_have_not_landed_keeps_row_and_column_when_they_land() {
    let _guard = plx_base::testlock::serial();
    let rows: Vec<_> = (0..30).map(|i| (format!("movie.row{i}"), format!("/hubs/sections/1/row{i}"), format!("Row {i}"))).collect();
    let mut fixture = Fixture::named_shelves(&rows, 12);
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
    let src = HubSrc { elems: &page.shelves[target].elems, shelf: fixture.hubs.view().shelves().get(target), paging: false };
    assert!(!<HubSrc as plx_ui::cards::CardSource<HostFixture>>::loaded(&src, col), "the card is a placeholder");
    // OK on a placeholder opens nothing
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.step(&ScreenEvent::Activate(slot.elem), &fixture.cx(Some(slot)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.is_empty(), "OK on a placeholder is inert");
    // the ring moves onto the row and its cards land
    let id = fixture.listing.view().id().unwrap();
    let stores = fixture.stores.take().unwrap();
    stores.browse_run_for_test(BrowseCmd::Addressed {
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

/// The focus read-out (`cdg`) names a card by its place in the hub's listing: a window that
/// starts at twelve puts its slot 11 at position 23.
#[test]
fn a_slid_hub_window_names_its_cards_by_listing_position() {
    let global = |src: &HubSrc, i| <HubSrc as plx_ui::cards::CardSource<HostFixture>>::global(src, i);
    let mut shelf = plx_data::browse::section_hubs::Shelf::default();
    shelf.offset = 12;
    shelf.positions = (12..24).collect();
    assert_eq!(global(&HubSrc { elems: &[], shelf: Some(&shelf), paging: false }, 11), 23);
    shelf.positions.clear();
    assert_eq!(global(&HubSrc { elems: &[], shelf: Some(&shelf), paging: false }, 11), 23, "offset plus slot");
    assert_eq!(global(&HubSrc { elems: &[], shelf: None, paging: false }, 11), 11);
}
