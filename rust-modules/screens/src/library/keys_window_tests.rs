// The Library grid projects keys for the wanted windows only. The tests below hold the contract
// the windowing must keep: the key table is bounded by the window however long the listing is,
// and the cell under focus never moves, whatever happens to the keys around it.
use super::*;
use super::grid_motion_tests::{frame, numbered, settle};
use plx_data::stores::browse::ListingSnapshot;

fn movie(i: usize) -> plx_data::pms::PmsMovie {
    plx_data::pms::PmsMovie { sid: plx_plex::plex::ServerId::from_raw(0), rk: format!("{i}"), title: format!("s{i:05}"),
        ..Default::default() }
}

/// A listing of `total` slots of which only `loaded` hold an item.
fn partial(total: usize, loaded: std::ops::Range<usize>) -> ListingSnapshot {
    ListingSnapshot::fixture(plx_plex::plex::ServerId::from_raw(0), loaded.map(|i| Some(movie(i))).collect(),
        vec![("A".into(), total as i64)]).with_total(total)
}

fn grid_keys(page: &LibraryScreen) -> usize {
    page.keys.keys().iter().filter(|key| matches!(key.identity,
        LibraryIdentity::Grid { .. } | LibraryIdentity::GridSlot { .. })).count()
}

fn scroll_to_row(page: &mut LibraryScreen, fixture: &Fixture, row: usize) {
    let y = page.target_layout.row_reveal(row);
    page.scroll.jump(y);
    page.scroll_target = y;
    page.sync(&fixture.cx(None));
}

fn seat(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, elem: u32) {
    let key = page.key(elem);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    deliver(page, engine, fixture, ScreenEvent::FocusMoved { from: None, to: key, by: By::Restore });
    settle(page, engine, fixture, 0);
}

/// The element a command asks the engine to remember.
fn remembered_by(page: &mut LibraryScreen, fixture: &Fixture, cmd: LibraryCmd) -> Option<u32> {
    let mut out = Vec::new();
    let mut present = plx_machine::present::Present::new();
    page.command(cmd, &fixture.cx(None), &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    out.iter().find_map(|e| match e.fx { Fx::Remember { elem, .. } => Some(elem), _ => None })
}

/// What the dispatcher does after a frame's events: the engine reconciles its focus with the page.
fn reconcile(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture) {
    let outcome = engine.reconcile(OWNER, page, &fixture.cx(engine.current(OWNER)));
    if let Outcome::Moved { from, to, by } = outcome {
        deliver(page, engine, fixture, ScreenEvent::FocusMoved { from, to, by });
    }
}

/// The cell the focused element occupies, with the engine focus folded in.
fn focused_rect(page: &LibraryScreen, engine: &FocusEngine<u32>, fixture: &Fixture) -> plx_ui::Rect {
    let at = page.pair.detail.index_of(engine.current(OWNER).unwrap().elem).unwrap();
    page.pair.detail.rect_at(&fixture.cx(engine.current(OWNER)), at)
}

#[test]
fn a_twenty_thousand_slot_section_keeps_its_grid_keys_within_the_window() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = partial(20_000, 0..60);
    let mut page = fixture.screen();
    let bound = |page: &LibraryScreen| 2 * page.pair.detail.projected() + 256;
    let mut peak = 0;
    for row in (0..5_000).step_by(7) {
        scroll_to_row(&mut page, &fixture, row);
        assert!(grid_keys(&page) <= bound(&page), "row {row}: {} keys over the bound {}", grid_keys(&page), bound(&page));
        peak = peak.max(grid_keys(&page));
    }
    assert!(peak > page.pair.detail.projected(), "the walk did mint more keys than one window holds");
    for query in 2..12 {
        fixture.listing = partial(20_000, 0..60).with_query(query);
        scroll_to_row(&mut page, &fixture, 100 * query as usize);
        assert!(grid_keys(&page) <= bound(&page), "sort change {query}: {} keys over the bound {}", grid_keys(&page), bound(&page));
    }
}

#[test]
fn the_keys_registered_per_frame_do_not_grow_with_the_listing() {
    let _guard = plx_base::testlock::serial();
    let probes = |total: usize| {
        let mut fixture = Fixture::new();
        fixture.listing = partial(total, 0..60);
        let mut page = fixture.screen();
        scroll_to_row(&mut page, &fixture, 20);
        page.keys.reset_register_probes();
        scroll_to_row(&mut page, &fixture, 21);
        page.keys.register_probes()
    };
    let (small, large) = (probes(2_000), probes(200_000));
    assert!(small > 0, "a scroll step does project the row it brings in");
    assert_eq!(small, large, "a frame's work is the window's, not the listing's");
}

#[test]
fn every_cell_the_grid_can_paint_holds_a_key() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = partial(5_000, 0..60).with_library_type(plx_data::browse::LibraryType::Episodes);
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    // The scroll spring runs on its own, several rows per frame at first: in every frame the
    // cells the grid paints (its own window, after the frame's scroll step) must be keyed.
    for target_row in [40, 400, 1_100, 10] {
        page.scroll_target = page.target_layout.row_reveal(target_row);
        for n in 0..90 {
            frame(&mut page, &mut engine, &fixture, 100 * target_row as u32 + n * 16);
            let window = page.pair.detail.window();
            assert!(window.clone().all(|i| page.pair.detail.elem_at(i).is_some()),
                "row {target_row}, frame {n}: a painted cell has no key in {window:?}");
        }
    }
}

#[test]
fn a_focus_on_a_hole_keeps_its_cell_when_the_hole_lands() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = partial(400, 0..60);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let index = 101;
    let elem = deep(&mut page, &fixture, index);
    seat(&mut page, &mut engine, &fixture, elem);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((index / COLS, index % COLS)));
    let before = focused_rect(&page, &engine, &fixture);
    // The page holding the slot lands: the hole's key becomes the item's.
    fixture.listing = partial(400, 0..60).with_page(96, (96..112).map(movie).collect());
    deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    reconcile(&mut page, &mut engine, &fixture);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((index / COLS, index % COLS)),
        "the same slot index and column in the frame the item lands");
    assert_ne!(engine.current(OWNER).unwrap().elem, elem, "the landed item is a new key");
    let landed = focused_rect(&page, &engine, &fixture);
    assert!((before.cx() - landed.cx()).abs() < 0.5 && (before.cy() - landed.cy()).abs() < 0.5,
        "the cell under focus did not move: {before:?} -> {landed:?}");
    frame(&mut page, &mut engine, &fixture, 5000);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((index / COLS, index % COLS)));
    assert!((focused_rect(&page, &engine, &fixture).cy() - before.cy()).abs() < 0.5);
}

#[test]
fn a_focus_survives_the_eviction_and_refetch_of_its_neighbours() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = numbered(0..120);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let index = 45;
    let elem = deep(&mut page, &fixture, index);
    seat(&mut page, &mut engine, &fixture, elem);
    let before = focused_rect(&page, &engine, &fixture);
    // Every item but the focused slot's page is evicted (a hole each), then they come back.
    for (round, listing) in [partial(120, 44..48), numbered(0..120)].into_iter().enumerate() {
        fixture.listing = listing;
        deliver(&mut page, &mut engine, &fixture, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
        reconcile(&mut page, &mut engine, &fixture);
        frame(&mut page, &mut engine, &fixture, 6000 + round as u32 * 16);
        assert_eq!(page.grid_position(engine.current(OWNER)), Some((index / COLS, index % COLS)), "round {round}");
        let now = focused_rect(&page, &engine, &fixture);
        assert!((before.cx() - now.cx()).abs() < 0.5 && (before.cy() - now.cy()).abs() < 0.5,
            "round {round}: the cell under focus moved: {before:?} -> {now:?}");
    }
}

#[test]
fn a_jump_to_a_letter_in_a_never_loaded_region_seats_the_target_index() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = ListingSnapshot::fixture(plx_plex::plex::ServerId::from_raw(0), (0..60).map(|i| Some(movie(i))).collect(),
        vec![("A".into(), 10_000), ("M".into(), 9_000), ("Z".into(), 1_000)]).with_total(20_000);
    let mut page = fixture.screen();
    page.initial = false;
    assert!(page.pair.detail.elem_at(19_000).is_none(), "the target region holds no key before the jump");
    let remembered = remembered_by(&mut page, &fixture, LibraryCmd::SwitchStep(12)).expect("the jump seats");
    assert_eq!(page.pair.detail.index_of(remembered), Some(19_000));
}

#[test]
fn a_rail_of_more_than_sixty_four_letters_reaches_its_last_letter() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let letters: Vec<(String, i64)> = (0..120).map(|i| (format!("L{i}"), 10)).collect();
    fixture.listing = ListingSnapshot::fixture(plx_plex::plex::ServerId::from_raw(0), (0..60).map(|i| Some(movie(i))).collect(), letters)
        .with_total(1_200);
    let mut page = fixture.screen();
    page.initial = false;
    assert_eq!(page.pair.master.elems.len(), 120, "no letter is dropped");
    let remembered = remembered_by(&mut page, &fixture, LibraryCmd::SwitchStep(12)).expect("the jump seats");
    assert_eq!(page.pair.detail.index_of(remembered), Some(1_190));
}

#[test]
fn the_screen_sends_the_focused_slot_in_its_want() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = partial(400, 0..60);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let wants = |page: &mut LibraryScreen, engine: &FocusEngine<u32>, fixture: &Fixture| {
        let mut out = Vec::new();
        let mut present = plx_machine::present::Present::new();
        page.step(&ScreenEvent::Tick(Tick { ms: 9000, dt_us: 16_667 }), &fixture.cx(engine.current(OWNER)),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
        out.into_iter().filter_map(|e| match e.fx {
            Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Want { focus, .. }, .. }))) => Some(focus),
            _ => None,
        }).collect::<Vec<_>>()
    };
    assert_eq!(wants(&mut page, &engine, &fixture), vec![None], "no focus on the grid, no slot");
    let elem = deep(&mut page, &fixture, 33);
    seat(&mut page, &mut engine, &fixture, elem);
    assert_eq!(wants(&mut page, &engine, &fixture), vec![Some(33)]);
}

#[test]
fn viewports_of_a_stale_epoch_are_gone_after_the_epoch_changes() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    let first = fixture.listing.clone();
    let mut page = fixture.screen();
    let y = page.layout.row_reveal(3);
    page.scroll.jump(y);
    page.scroll_target = y;
    fixture.listing = first.clone().with_section(1, 2);
    page.sync(&fixture.cx(None));
    assert!(!page.viewports.is_empty(), "leaving a section saves its viewport");
    assert!(page.viewports.iter().all(|view| Some(view.epoch) == page.epoch));
    fixture.listing = first.with_section(2, 1);
    page.sync(&fixture.cx(None));
    assert_eq!(page.epoch, Some(2));
    assert!(page.viewports.iter().all(|view| view.epoch == 2), "an older epoch's viewports are dropped: {}", page.viewports.len());
}

#[test]
fn shelf_keys_stay_bounded_over_rotating_refetches() {
    let _guard = plx_base::testlock::serial();
    let session = plx_plex::plex::session::TempSession::new("library-shelf-rotation");
    session.watching("u-library-shelf-rotation");
    let mut fixture = Fixture::shelves(&["movie.inprogress.1"], 12);
    let mut page = fixture.screen();
    let shelf_keys = |page: &LibraryScreen| page.keys.keys().iter().filter(|key| matches!(key.identity,
        LibraryIdentity::Shelf { .. } | LibraryIdentity::ShelfSlot { .. })).count();
    let stores = fixture.stores.take().unwrap();
    let mut peak = 0;
    for round in 0..50 {
        // A refetch that returns different items under the same shelf.
        let rks: Vec<String> = (0..12).map(|i| format!("rot-{round}-{i}")).collect();
        stores.browse.borrow_mut().seed_first_shelf_items_for_test(0, &rks);
        let publication = stores.capture_browse(&mut fixture.directory);
        fixture.hubs = publication.section_hubs;
        fixture.listing = publication.listing;
        page.sync(&fixture.cx(None));
        peak = peak.max(shelf_keys(&page));
    }
    assert!(peak <= 2 * (12 + 12) + 256, "shelf keys grew to {peak} over 50 refetches");
}

#[test]
fn a_focus_far_from_the_scroll_keeps_its_key_through_pruning() {
    let _guard = plx_base::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = partial(20_000, 0..60);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let index = 101;
    let elem = deep(&mut page, &fixture, index);
    seat(&mut page, &mut engine, &fixture, elem);
    // The scroll walks far away while the engine keeps its focus on the slot: the table is pruned
    // around the focus, never of it.
    for row in (200..4_000).step_by(37) {
        let y = page.target_layout.row_reveal(row);
        page.scroll.jump(y);
        page.scroll_target = y;
        page.sync(&fixture.cx(engine.current(OWNER)));
        assert!(page.keys.key(elem).is_some(), "row {row}: the focused key was pruned");
    }
    assert!(grid_keys(&page) <= 2 * page.pair.detail.projected() + 256);
    assert_eq!(page.pair.detail.index_of(elem), Some(index));
}
