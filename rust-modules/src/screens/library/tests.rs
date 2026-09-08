use super::*;
use crate::ui::fixture::FixtureMeasure;
use crate::ui::focus::{FocusEngine, Outcome};
use crate::ui::machine::{Host, InputOwner, FocusRead, PressRead, Tick};
use crate::ui::screen::ScreenArg;

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
#[derive(Clone, Copy)]
struct Views<'a> {
    listing: crate::stores::browse::ListingView<'a>,
    directory: crate::stores::browse::DirectoryView<'a>,
    hubs: crate::stores::browse::HubsView<'a>,
}
impl Host for HostFixture {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = Arg;
    type Memory = PageMemory;
}
impl LibraryLike for HostFixture {
    fn listing<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::HubsView<'a> { cx.views.hubs }
}
const ENTRY: EntryId = EntryId(81);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);

#[test]
fn large_listing_publication_work_is_bounded_by_initial_slots_then_changed_page() {
    let _guard = crate::testlock::serial();
    const TOTAL: usize = 10_000;
    const PAGE: usize = 60;
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(0);
    let movies = |start: usize| (start..start + PAGE).map(|i| crate::pms::PmsMovie {
        sid, rk: format!("large-{i}"), title: format!("Large {i}"), ..Default::default()
    }).collect::<Vec<_>>();
    fixture.listing = crate::browse::view::ListingSnapshot::fixture(
        sid, movies(0).into_iter().map(Some).collect(), Vec::new()).with_total(TOTAL);

    let mut page = fixture.screen();
    let (initial_slots, initial_known) = page.pair.detail.publication_ops();
    let initial_registry = page.keys.register_probes();
    page.pair.detail.reset_publication_ops();
    page.keys.reset_register_probes();

    fixture.listing = fixture.listing.clone().with_page(PAGE, movies(PAGE));
    page.sync(&fixture.cx(None));
    let (next_slots, next_known) = page.pair.detail.publication_ops();
    let next_registry = page.keys.register_probes();
    eprintln!("publication-ops initial slots={initial_slots} known={initial_known} registry={initial_registry}; next slots={next_slots} known={next_known} registry={next_registry}");

    assert!(initial_slots == TOTAL && initial_known <= TOTAL && initial_registry <= TOTAL + 128
        && next_slots == PAGE && next_known <= PAGE && next_registry <= PAGE + 128,
        "publication work must be linear initially and page-bounded later: initial slots={initial_slots} known={initial_known} registry={initial_registry}; next slots={next_slots} known={next_known} registry={next_registry}");
}

#[test]
fn derived_grid_indexes_survive_reorder_truncation_clear_and_restore() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let original = fixture.listing.clone();
    let mut page = fixture.screen();
    let stable = page.pair.detail.elem_at(5).unwrap();
    let removed = page.pair.detail.elem_at(30).unwrap();

    let mut reordered = (0..36).map(|i| original.view().item(i).unwrap().clone()).collect::<Vec<_>>();
    reordered.swap(5, 17);
    fixture.listing = fixture.listing.clone().with_page(0, reordered);
    page.sync(&fixture.cx(None));
    assert_eq!(page.pair.detail.elem_at(17), Some(stable));
    assert_eq!(page.pair.detail.index_of(stable), Some(17), "a same-query reorder follows the stable item key");

    fixture.listing = fixture.listing.clone().with_total(10);
    page.sync(&fixture.cx(None));
    assert_eq!(page.pair.detail.index_of(stable), None);
    assert_eq!(page.pair.detail.index_of(removed), None);
    assert_eq!(page.pair.detail.fallback_for(stable), page.pair.detail.elem_at(9));
    assert_eq!(page.pair.detail.fallback_for(removed), page.pair.detail.elem_at(9),
        "a truncated item falls back through its last published slot");

    let memory = page.page_memory();
    fixture.listing = crate::browse::view::ListingSnapshot::absent();
    page.sync(&fixture.cx(None));
    assert!(page.pair.detail.elems.is_empty());
    assert_eq!(page.pair.detail.index_of(stable), None);

    fixture.listing = original.with_total(10);
    let mut restored = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    restored.restore(&memory);
    restored.sync(&fixture.cx(None));
    assert_eq!(restored.pair.detail.elem_at(5), Some(stable),
        "restore rebuilds lookup indexes and reuses the stable item key");
    assert_eq!(restored.pair.detail.index_of(stable), Some(5));
    assert_eq!(restored.pair.detail.fallback_for(removed), restored.pair.detail.elem_at(9));
}

#[test]
fn down_from_a_missing_final_row_column_clamps_to_the_last_item() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(0);
    fixture.listing = crate::browse::view::ListingSnapshot::fixture(sid,
        (0..8).map(|i| Some(crate::pms::PmsMovie { sid, rk: format!("{i}"), ..Default::default() })).collect(),
        Vec::new());
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let key = page.key(page.pair.detail.elems[5]);
    engine.set(OWNER, key, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current(OWNER), Some(page.key(page.pair.detail.elems[7])));
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current(OWNER), Some(page.key(page.pair.detail.elems[7])), "the last row remains an edge");
}

#[test]
fn rail_eligibility_and_last_producer_hold_over_a_long_shelf() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-rail-layer");
    session.watching("u-library-rail-layer");
    let mut fixture = Fixture::shelves(&["movie.inprogress.1", "movie.recentlyadded.1"], 12);
    let view = fixture.listing.view();
    let id = view.id().unwrap();
    let items = (0..view.total() as usize).map(|i| view.item(i).cloned()).collect();
    fixture.listing = crate::browse::view::ListingSnapshot::fixture(id.sid, items,
        (0..9).map(|i| (format!("{i}"), 14)).collect()).with_section(id.epoch, id.section);
    let mut page = fixture.screen();
    assert!(page.layout.row_y(0, page.scroll.pos) > SCR_H, "the fixture grid starts below the viewport");
    let shelf = page.key(*page.shelves[0].elems.last().unwrap());
    let mut engine = FocusEngine::new();
    engine.set(OWNER, shelf, Some(page.shelves[0].group), By::Restore);
    let mut groups = Vec::new();
    page.groups(&fixture.cx(Some(shelf)), &mut groups);
    assert!(!groups.iter().any(|group| group.id == page.pair.groups_config().master));
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    assert_eq!(engine.current(OWNER), Some(shelf), "the shelf edge cannot enter an ineligible rail");

    let grid = page.key(page.pair.detail.elems[0]);
    engine.set(OWNER, grid, Some(page.pair.groups_config().detail), By::Restore);
    let cx = fixture.cx(Some(grid));
    page.pair.master.advance(&cx, Some(0), true, 0.05);
    let rail = page.key(page.pair.master.elems[0]);
    let placed = page.place(&rail.elem, &cx, At::Drawn).unwrap();
    assert!(placed.clip.h > 0.0);
    let mut draw = DrawFrame::new(&cx, crate::ui::Painter::root());
    page.record_stops(&mut draw);
    let stops = draw.into_stops();
    assert!(stops.iter().any(|stop| region_of_elem(stop.key.elem) == Some(KeyRegion::Shelf)
        && stop.rect.intersect(stop.clip).contains(placed.rect.cx(), placed.rect.cy())), "the producer-order assertion needs actual overlap");
    let mut hit = crate::ui::hit::HitMap::new();
    hit.fill(stops); hit.swap();
    assert_eq!(hit.top_at(placed.rect.cx(), placed.rect.cy()).map(|stop| stop.key), Some(rail),
        "the eligible rail must paint and register after the document's wide shelf");
}

#[test]
fn owned_rail_keeps_the_fixed_legacy_origin_and_short_window() {
    let _guard = crate::testlock::serial();
    for n in [9, 30] {
        let mut fixture = Fixture::new();
        let sid = crate::plex::ServerId::from_raw(0);
        fixture.listing = crate::browse::view::ListingSnapshot::fixture(sid,
            (0..36).map(|i| Some(crate::pms::PmsMovie { sid, rk: format!("{i}"), ..Default::default() })).collect(),
            (0..n).map(|i| (format!("{i}"), 1)).collect());
        let page = fixture.screen();
        let mut groups = Vec::new();
        page.pair.master.groups(&fixture.cx(Some(page.key(page.pair.detail.elems[0]))), &mut groups);
        let rail = groups.first().expect("the fixture has a real rail").extent;
        assert_eq!(rail.y, 240.0, "the rail is not attached to the scrolling first grid row");
        if n == 9 { assert_eq!(rail.h, 9.0 * layout::RAIL_PITCH); }
        else { assert!(rail.h < n as f32 * layout::RAIL_PITCH, "long alphabets scroll at the same pitch"); }
    }
}

#[test]
fn retry_stop_matches_the_shared_measured_status_action_with_and_without_reason() {
    let _guard = crate::testlock::serial();
    for owner in ["", "friend"] {
        let mut fixture = Fixture::new();
        fixture.listing = crate::stores::browse::listing_snapshot();
        fixture.directory = crate::browse::view::DirectorySnapshot::fixture_source(4, crate::plex::ServerId::from_raw(7),
            crate::browse::SrcGroup { name: "Cinema server".into(), handle: owner.into(),
                state: crate::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
        let page = fixture.screen();
        let cx = fixture.cx(Some(page.key(RETRY)));
        let (caption, reason) = page.status_text(&cx);
        assert_eq!(reason.is_some(), !owner.is_empty());
        let mut overlay = crate::ui::widgets::StatusOverlay::new(page.status_frame(), &caption,
            crate::ui::widgets::StatusKind::Failed).action(c"Try again");
        if let Some(reason) = &reason { overlay = overlay.reason(reason); }
        let expected = overlay.action_frame_measured(cx.measure).unwrap();
        for at in [At::Drawn, At::SpringTarget] {
            let actual = page.place(&RETRY, &cx, at).unwrap().rect;
            assert_eq!([actual.x, actual.y, actual.w, actual.h], [expected.x, expected.y, expected.w, expected.h],
                "reason={owner:?}, at={at:?}");
        }
    }
}

#[test]
fn a_fully_discovered_missing_kind_finishes_its_fade_and_has_no_foreign_grid() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        crate::browse::view::SectionView { borrowed: false, sid: Some(crate::plex::ServerId::from_raw(0)), key: 1,
            kind: SecKind::Movie, row: crate::browse::SrcRow { section: 0, title: "Cinema".into(),
                pinned: true, current: true, ..Default::default() } }]);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Show);
    page.page_fade.mount();
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    for i in 0..40 {
        page.step(&ScreenEvent::Tick(Tick { ms: i * 20, dt_us: 20_000 }), &fixture.cx(None),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    }
    assert_eq!(page.readout, Readout::Empty);
    assert_eq!(page.page_fade.alpha(), 1.0, "a terminal absent type must not hold the page at black");
    assert_eq!(page.kind, SecKind::Show, "the requested type is not silently replaced by Movies");
    assert_eq!(page.status_text(&fixture.cx(None)).0.to_str().unwrap(), "Nothing here matches");
    assert!(!out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::Store(StoreId::Browse,
        StoreCmd::Browse(BrowseCmd::Addressed { work: LibraryWork::Want { .. }, .. }))))),
        "waiting for Shows cannot page through the retained Movies listing");
    let mut groups = Vec::new();
    page.groups(&fixture.cx(None), &mut groups);
    assert!(!groups.iter().any(|g| g.id == page.pair.groups_config().detail || g.id == TOOLBAR_GROUP));
    assert!(page.focused_item(Some(page.key(page.keys.keys().first().map_or(0, |k| k.elem))), &fixture.cx(None)).is_none());
}

#[test]
fn owned_card_stops_clip_pointer_hits_and_hold_the_engine_item() {
    use crate::ui::hit::PointerKind;
    use crate::ui::input::{InputMachine, PressEvent};
    use crate::ui::machine::{PressArm, PressFrom};
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    page.scroll.jump(400.0);
    page.scroll_target = 400.0;
    let first = page.key(page.pair.detail.elems[0]);
    let chosen = page.key(page.pair.detail.elems[1]);
    page.relayout(Some(first));
    let cx = fixture.cx(Some(first));
    let mut draw = DrawFrame::new(&cx, crate::ui::Painter::root());
    page.record_stops(&mut draw);
    let stops = draw.into_stops();
    let stop = *stops.iter().find(|stop| stop.key == chosen).unwrap();
    let mut input = InputMachine::new();
    input.engine.set(OWNER, first, Some(page.pair.groups_config().detail), By::Restore);
    input.hit.fill(stops);
    input.hit.swap();
    assert!(stop.rect.y < 0.0, "the fixture contains a genuinely clipped card");
    assert!(input.hit.resolve(Some(ENTRY), PointerKind::Click, stop.rect.cx(), stop.rect.y + 1.0, Some(first)).miss);
    let y = stop.clip.y + 10.0;
    assert!(stop.rect.contains(stop.rect.cx(), y));
    let resolved = input.hit.resolve(Some(ENTRY), PointerKind::Click, stop.rect.cx(), y, Some(first));
    assert_eq!(resolved.focus, Some(chosen));
    assert_eq!(resolved.activate, Some((chosen, crate::ui::screen::Activate::Press)));
    input.engine.set(OWNER, chosen, page.group_of(&chosen.elem, &cx), By::Pointer);
    input.arm(PressArm { key: chosen, from: PressFrom::Pointer, holdable: true }, MachineId::Instance(InstanceId(19)), 0);
    let held = input.tick(crate::ui::press::LONG_MS + 1, 0.016);
    assert!(matches!(held.as_slice(), [PressEvent::Hold(_, _, key)] if *key == chosen));
    let PressEvent::Hold(id, _, _) = held[0] else { unreachable!() };
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.step(&ScreenEvent::PressHold(id), &fixture.cx(input.engine.current(OWNER)),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Library(LibraryReq::ItemMenu { sid, rk, from_deck: false }))
            if *sid == crate::plex::ServerId::from_raw(0) && rk == "2")));
    assert!(input.hit.resolve(Some(EntryId(99)), PointerKind::Click, stop.rect.cx(), y, Some(chosen)).miss,
        "a menu owner cannot click through to a retained Library stop");
}

#[test]
fn actual_sort_menu_traps_engine_navigation_in_its_own_entry() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let page = fixture.screen();
    let entry = EntryId(99);
    let owner = InputOwner::Entry(entry);
    let arg = crate::screens::registry::LibraryMenuArg { host: InstanceId(19),
        target: page.address(&fixture.cx(None)).unwrap(), kind: crate::screens::registry::LibraryMenuKind::Sort,
        anchor: [0; 4] };
    let mut menu = menu::LibraryMenu::new(entry, arg);
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    menu.step(&ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(GroupId(0)) }), &fixture.cx(None),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(20)), &mut present));
    let mut engine = FocusEngine::new();
    engine.enter(owner, &menu, FocusTarget::ContainerGroup(GroupId(0)), None, &fixture.cx(None));
    let first = engine.current(owner).expect("actual menu has a sort row");
    for dir in [Dir::Up, Dir::Left, Dir::Right, Dir::Down] {
        engine.move_dir(owner, &menu, &[], dir, &fixture.cx(Some(first)));
        assert_eq!(engine.current(owner).unwrap().entry, entry);
    }
    assert_eq!(engine.current(OWNER), None, "menu navigation never writes Library focus");
}

#[test]
fn a_compact_menu_keeps_the_host_store_pump_and_deferred_commit_live() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.wanted_kind = None;
    let target = page.address(&fixture.cx(None)).unwrap();
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present);
    page.step(&ScreenEvent::Cover, &fixture.cx(None), &mut fx);
    page.step(&ScreenEvent::App(AppMsg::LibraryEdit { target,
        edit: crate::stores::browse::QueryEdit::Unwatched(true) }), &fixture.cx(None), &mut fx);
    page.step(&ScreenEvent::Tick(Tick { ms: 80, dt_us: 80_000 }), &fixture.cx(None), &mut fx);
    drop(fx);
    assert!(out.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            work: LibraryWork::Commit { query: Some(crate::stores::browse::QueryEdit::Unwatched(true)), .. }, .. }))))));
    assert!(out.iter().any(|effect| matches!(&effect.fx, Fx::App(AppFx::StoreWork(StoreWork::Browse)))),
        "the store must fetch the just-committed query while the compact menu remains open");
}

#[test]
fn published_library_projection_and_layout_enter_canonical_state() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let hash = |page: &LibraryScreen| { let mut c = Canon::new(); page.write(&mut c); c.finish() };
    let mut page = fixture.screen();
    let initial = hash(&page);
    page.pair.detail.elems.swap(0, 1);
    assert_ne!(initial, hash(&page), "published item order determines the next navigation step");
    let prior = hash(&page);
    page.target_layout.rows += 1;
    assert_ne!(prior, hash(&page), "target document geometry determines reveal and placement");
    let prior = hash(&page);
    page.libraries.push((MORE, usize::MAX));
    assert_ne!(prior, hash(&page), "bounded favorite window determines available controls");
    let prior = hash(&page);
    page.readout = Readout::Failed;
    assert_ne!(prior, hash(&page), "failure control availability is logical state");
}

#[test]
fn library_capsule_and_control_motion_enter_canonical_state() {
    let hash = |page: &LibraryScreen| {
        let mut c = Canon::new();
        page.write(&mut c);
        c.finish()
    };
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
    let initial = hash(&page);
    page.library_capsules.update(0, 0, |_| Some((120.0, 180.0)),
        crate::ui::widgets::SelMark::Travels, 0.016);
    let capsule = hash(&page);
    assert_ne!(initial, capsule, "held capsule geometry affects subsequent frames");
    page.library_pop.step(Some(0), 0.016);
    assert_ne!(capsule, hash(&page), "control focus and spring velocity affect subsequent frames");
}

#[test]
fn pending_semantic_commits_change_the_library_state_hash() {
    fn hash(page: &LibraryScreen) -> u64 {
        let mut canon = Canon::new();
        page.write(&mut canon);
        canon.finish()
    }
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
    let empty = hash(&page);
    let section = SectionTarget { epoch: 1, index: 0,
        identity: LibrarySectionIdentity { sid: crate::plex::ServerId::from_raw(0), key: 1 }, kind: SecKind::Movie };
    page.pending.request_section(section.clone());
    let with_section = hash(&page);
    assert_ne!(empty, with_section, "a next-frame section commit must enter canonical state");
    page.pending.request_section(SectionTarget { epoch: 2, ..section.clone() });
    assert_ne!(with_section, hash(&page), "epoch refusal changes the next commit");
    page.pending.request_section(SectionTarget { identity: LibrarySectionIdentity {
        sid: crate::plex::ServerId::from_raw(1), key: 1 }, ..section });
    assert_ne!(with_section, hash(&page), "same section key on another server is a different commit");
    let target = GridTarget { epoch: 1, sid: crate::plex::ServerId::from_raw(0), section: 1, query: 1 };
    let actions = [GridAction::Unwatched { desired: true }, GridAction::Unwatched { desired: false },
        GridAction::Sort { key: "titleSort".into(), desc: false }, GridAction::Sort { key: "titleSort".into(), desc: true },
        GridAction::Sort { key: "addedAt".into(), desc: true }, GridAction::Genre { id: None },
        GridAction::Genre { id: Some("7".into()) }];
    let mut hashes = vec![empty, with_section];
    for action in actions {
        page.pending.request_grid(target.clone(), action);
        let next = hash(&page);
        assert!(!hashes.contains(&next), "distinct semantic actions must have distinct encodings");
        hashes.push(next);
    }
}

struct Fixture {
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    hubs: crate::stores::browse::HubsSnapshot,
    measure: FixtureMeasure,
}

#[test]
fn discovery_failure_retry_targets_the_source_without_a_section() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(7);
    fixture.listing = crate::stores::browse::listing_snapshot();
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture_source(4, sid,
        crate::browse::SrcGroup { name: "Cinema server".into(), handle: "friend".into(),
            state: crate::browse::SourceState::Unreachable, tier: None }, SecFetch::Failed);
    let mut page = fixture.screen();
    assert!(fixture.listing.view().id().is_none());
    assert_eq!(page.readout, Readout::Failed);
    assert_eq!(page.status_text(&fixture.cx(None)).0.to_str().unwrap(), "Can't reach Cinema server");
    let mut out = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.activate(RETRY, false, &fixture.cx(Some(page.key(RETRY))),
        &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(out.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::RetrySource { epoch: 4, sid: target }))) if *target == sid)),
        "Retry must issue work even when discovery never produced a section address");
}

#[test]
fn failed_and_empty_readouts_offer_only_their_real_owned_controls() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let original = fixture.listing.clone();
    for (fetch, total, expected, grid, retry) in [
        (SecFetch::Failed, 36, Readout::Grid, true, false),
        (SecFetch::Ready, 0, Readout::Empty, false, false),
        (SecFetch::Failed, -1, Readout::Failed, false, true),
        (SecFetch::Loading, -1, Readout::Loading, false, false),
    ] {
        fixture.listing = original.clone().with_fetch(fetch, total);
        let page = fixture.screen();
        let cx = fixture.cx(None);
        assert_eq!(page.readout, expected);
        let mut groups = Vec::new();
        page.groups(&cx, &mut groups);
        assert_eq!(groups.iter().any(|g| g.id == page.pair.groups_config().detail), grid);
        assert_eq!(page.place(&SORT, &cx, At::SpringTarget).is_some(), grid);
        assert_eq!(page.place(&FILTER, &cx, At::SpringTarget).is_some(), grid);
        assert_eq!(page.place(&RETRY, &cx, At::SpringTarget).is_some(), retry);
        if !grid {
            assert!(!groups.iter().any(|g| g.id == page.pair.groups_config().master), "stale letters are not a live rail");
        }
        if retry {
            let mut engine = FocusEngine::new();
            engine.enter(OWNER, &page, FocusTarget::ContainerGroup(STATUS_GROUP), None, &cx);
            assert_eq!(engine.current(OWNER), Some(page.key(RETRY)));
        }
    }
}

#[test]
fn foreign_section_replacement_upgrades_a_grid_fade_once() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    page.grid_fade.reload();
    fixture.listing = fixture.listing.clone().with_section(2, 1);
    page.sync(&fixture.cx(None));
    assert!(page.page_fade.is_swapping(), "foreign replacement remounts the entire document");
    assert_eq!(page.page_fade.alpha(), 0.0);
    assert!(!page.grid_fade.is_swapping());
    page.page_fade.tick(0.04, true);
    let alpha = page.page_fade.alpha();
    page.sync(&fixture.cx(None));
    assert_eq!(page.page_fade.alpha(), alpha, "the same publication must not remount twice");
}

#[test]
fn toolbar_stops_use_the_shared_value_chip_measurement() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let page = fixture.screen();
    let cx = fixture.cx(None);
    let sort = page.place(&SORT, &cx, At::Drawn).unwrap().rect;
    let filter = page.place(&FILTER, &cx, At::Drawn).unwrap().rect;
    assert_eq!(sort.w, crate::ui::value_chip::ValueChip::width(cx.measure, c"Sort", c" · Title", None));
    assert_eq!(filter.w, crate::ui::value_chip::ValueChip::width(cx.measure, c"Filter", c" · All", None));
    assert_eq!(filter.x, sort.x + sort.w + 16.0);
}

#[test]
fn section_grid_memories_do_not_overwrite_one_another() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let first = fixture.listing.clone();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let group_a = page.pair.groups_config().detail;
    let card_a = page.key(page.pair.detail.elem_at(17).unwrap());
    engine.set(OWNER, card_a, Some(group_a), By::Restore);
    fixture.listing = first.clone().with_section(1, 2);
    page.sync(&fixture.cx(engine.current(OWNER)));
    let group_b = page.pair.groups_config().detail;
    assert_ne!(group_a, group_b, "each section needs its own engine memory slot");
    let card_b = page.key(page.pair.detail.elem_at(23).unwrap());
    engine.set(OWNER, card_b, Some(group_b), By::Restore);
    fixture.listing = first;
    page.sync(&fixture.cx(engine.current(OWNER)));
    assert_eq!(page.pair.groups_config().detail, group_a);
    assert!(engine.remembered_for(ENTRY).contains(&(group_a, card_a.elem)));
    assert!(engine.remembered_for(ENTRY).contains(&(group_b, card_b.elem)));
    engine.enter(OWNER, &page, FocusTarget::ContainerGroup(group_a), None, &fixture.cx(engine.current(OWNER)));
    assert_eq!(engine.current(OWNER), Some(card_a), "Remembered seating restores the exact section card");
    // Enter the rail through a toolbar: projection must consult only this section's grid memory.
    engine.set(OWNER, page.key(FILTER), Some(TOOLBAR_GROUP), By::Dir);
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    assert_eq!(page.pair.master.start_for_elem(engine.current(OWNER).unwrap().elem), Some(0));
}

#[test]
fn section_viewport_bookmarks_survive_switch_and_evicted_body() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let first = fixture.listing.clone();
    let mut page = fixture.screen();
    let y = page.layout.row_reveal(3);
    page.scroll.jump(y);
    page.scroll_target = y;
    fixture.listing = first.clone().with_section(1, 2);
    page.sync(&fixture.cx(None));
    assert_eq!(page.scroll.pos, 0.0, "a new section starts at its own head");
    let PageMemory::Library(memory) = <LibraryScreen as Screen<HostFixture>>::memory(&page) else { panic!() };
    let mut evicted = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    evicted.restore(&memory);
    evicted.sync(&fixture.cx(None));
    fixture.listing = first;
    for page in [&mut page, &mut evicted] {
        page.sync(&fixture.cx(None));
        assert_eq!(page.scroll.pos, y, "viewport belongs to the section, including after body eviction");
        assert_eq!(page.scroll_target, y);
    }
}
impl Fixture {
    fn shelves(titles: &[&str], count: usize) -> Self {
        let mut fixture = Self::new();
        crate::browse::seed_two_source_table_for_test();
        fixture.directory.capture();
        crate::stores::browse::apply(BrowseCmd::SetCur(0));
        crate::browse::seed_items_for_test(120);
        crate::browse::section_hubs::seed_shelves_for_test(0, titles, count);
        fixture.directory.capture();
        fixture.listing = crate::stores::browse::listing_snapshot();
        fixture.hubs = crate::stores::browse::hubs_snapshot();
        let listing = fixture.listing.view().id().unwrap();
        let hubs = fixture.hubs.view().id().unwrap();
        assert_eq!((listing.epoch, listing.sid, listing.section), (hubs.epoch, hubs.sid, hubs.section));
        assert_eq!(fixture.hubs.view().shelves().len(), titles.len());
        fixture
    }

    fn new() -> Self {
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        let sid = crate::plex::ServerId::from_raw(0);
        let listing = crate::browse::view::ListingSnapshot::fixture(sid, (0..36).map(|i|
            Some(crate::pms::PmsMovie { sid, rk: format!("{}", i + 1), title: format!("s{i:04x}"), ..Default::default() })).collect(),
            vec![("A".into(), 18), ("Z".into(), 18)]);
        let directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, vec![
            crate::browse::view::SectionView { borrowed: false, sid: Some(sid), key: 1, kind: SecKind::Movie,
                row: crate::browse::SrcRow { section: 0, title: "Cinema".into(), pinned: true, current: true, ..Default::default() } }]);
        Self { listing, directory, hubs: crate::stores::browse::hubs_snapshot(), measure: FixtureMeasure }
    }
    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, HostFixture> {
        Cx { views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick::default(), measure: &self.measure, focus: FocusRead { current: focus },
            press: PressRead::default(), owner: OWNER }
    }
    fn screen(&self) -> LibraryScreen {
        let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
        page.sync(&self.cx(None));
        page
    }
}
fn deliver(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, event: ScreenEvent<HostFixture>) -> usize {
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.step(&event, &fixture.cx(engine.current(OWNER)),
        &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    let mut remembers = 0;
    for effect in output {
        if let Fx::Remember { group, elem } = effect.fx {
            remembers += 1;
            engine.remember_projected(ENTRY, group, elem);
        }
    }
    remembers
}
fn direction(page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, fixture: &Fixture, dir: Dir) -> usize {
    let mut links = Vec::new();
    <LibraryScreen as Screen<HostFixture>>::links(page, &mut links);
    let outcome = engine.move_dir(OWNER, page, &links, dir, &fixture.cx(engine.current(OWNER)));
    if let Outcome::Moved { from, to, by } = outcome {
        deliver(page, engine, fixture, ScreenEvent::FocusMoved { from, to, by })
    } else { 0 }
}

#[test]
fn shelf_horizontal_viewport_and_engine_item_survive_body_eviction() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-shelf-return");
    session.watching("u-library-shelf-return");
    let fixture = Fixture::shelves(&["movie.inprogress.1"], 12);
    let mut page = fixture.screen();
    page.shelves[0].motion.restore_scroll(700.0, 12, &RowStyle::HOME);
    let key = page.key(page.shelves[0].elems[3]);
    let before = page.place(&key.elem, &fixture.cx(Some(key)), At::Drawn).unwrap().rest_rect;
    let mut engine = FocusEngine::new();
    engine.set(OWNER, key, Some(page.shelves[0].group), By::Restore);
    let memory = page.page_memory();
    let mut restored = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
    restored.restore(&memory);
    restored.sync(&fixture.cx(Some(key)));
    engine.enter(OWNER, &restored, FocusTarget::ContainerGroup(restored.shelves[0].group), Some(key), &fixture.cx(Some(key)));
    assert_eq!(engine.current(OWNER), Some(key));
    assert_eq!(restored.shelves[0].motion.scroll_x(), 700.0);
    let after = restored.place(&key.elem, &fixture.cx(Some(key)), At::Drawn).unwrap().rest_rect;
    assert_eq!([before.x, before.y, before.w, before.h], [after.x, after.y, after.w, after.h]);
    assert_eq!(restored.focused_item(engine.current(OWNER), &fixture.cx(Some(key))).unwrap().rk, "movie.inprogress.1-3");
    crate::stores::browse::apply(BrowseCmd::Reset);
}

#[test]
fn shelf_return_follows_the_film_then_its_last_published_slot() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-shelf-removal");
    session.watching("u-library-shelf-removal");
    for evict in [false, true] {
        for remove_focused in [false, true] {
            let mut fixture = Fixture::shelves(&["movie.inprogress.1"], 6);
            let mut page = fixture.screen();
            let key = page.key(page.shelves[0].elems[3]);
            let memory = page.page_memory();
            let item = fixture.hubs.view().shelves()[0].items[if remove_focused { 3 } else { 0 }].clone();
            assert!(crate::stores::browse::apply(BrowseCmd::LeftTheDeck { sid: item.sid, rk: item.rk }));
            fixture.hubs = crate::stores::browse::hubs_snapshot();
            if evict {
                page = LibraryScreen::new(ENTRY, InstanceId(20), SecKind::Movie);
                page.restore(&memory);
            }
            page.sync(&fixture.cx(Some(key)));
            let mut engine = FocusEngine::new();
            engine.set(OWNER, key, Some(page.shelves[0].group), By::Restore);
            engine.enter(OWNER, &page, FocusTarget::ContainerGroup(page.shelves[0].group), Some(key), &fixture.cx(Some(key)));
            let focus = engine.current(OWNER).unwrap();
            let item = page.focused_item(Some(focus), &fixture.cx(Some(focus))).unwrap();
            assert_eq!(item.rk, if remove_focused { "movie.inprogress.1-4" } else { "movie.inprogress.1-3" });
            assert_eq!(page.shelves[0].elems.iter().position(|elem| *elem == focus.elem), Some(if remove_focused { 3 } else { 2 }));
            if !remove_focused { assert_eq!(focus, key); }
        }
    }
    crate::stores::browse::apply(BrowseCmd::Reset);
}

#[test]
fn projected_entry_preserves_the_last_item_within_a_letter_then_live_move_jumps() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let exact = page.key(page.pair.detail.elem_at(17).unwrap());
    engine.set(OWNER, exact, Some(page.pair.groups_config().detail), By::Restore);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Right), 0);
    assert_eq!(engine.current_group(OWNER), Some(page.pair.groups_config().master));
    assert_eq!(page.pair.master.start_for_elem(engine.current(OWNER).unwrap().elem), Some(0));
    assert_eq!(engine.remembered_for(ENTRY).iter().find(|(g, _)| *g == page.pair.groups_config().detail).unwrap().1, exact.elem);
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(engine.current(OWNER), Some(exact));
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Down), 1);
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((3, 0)));
}

#[test]
fn toolbar_rail_entry_uses_engine_grid_memory_and_returns_to_toolbar() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let exact = page.key(page.pair.detail.elem_at(23).unwrap());
    engine.set(OWNER, exact, Some(page.pair.groups_config().detail), By::Restore);
    engine.set(OWNER, page.key(FILTER), Some(TOOLBAR_GROUP), By::Dir);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Right), 0);
    assert_eq!(page.pair.master.start_for_elem(engine.current(OWNER).unwrap().elem), Some(18));
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)));
    assert_eq!(engine.remembered_for(ENTRY).iter().find(|(g, _)| *g == page.pair.groups_config().detail).unwrap().1, exact.elem);
}

#[test]
fn removed_grid_key_keeps_its_typed_master_detail_reconciliation_path() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    let original = page.key(page.pair.detail.elem_at(17).unwrap());
    let sid = crate::plex::ServerId::from_raw(0);
    fixture.listing = crate::browse::view::ListingSnapshot::fixture(sid,
        (0..35).map(|i| Some(crate::pms::PmsMovie { sid, rk: format!("{}", i + 100), ..Default::default() })).collect(),
        vec![("A".into(), 18), ("Z".into(), 17)]);
    page.sync(&fixture.cx(Some(original)));
    assert_eq!(<LibraryScreen as Focusable<HostFixture>>::group_of(&page, &original.elem, &fixture.cx(Some(original))), None);
    let recovered = <LibraryScreen as Focusable<HostFixture>>::reconcile(&page, original, &fixture.cx(Some(original)));
    assert_eq!(page.grid_position(Some(recovered)), Some((2, 5)));
}

#[test]
fn direct_rail_activation_jumps_even_when_the_letter_was_already_selected() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let exact = page.key(page.pair.detail.elem_at(17).unwrap());
    engine.set(OWNER, exact, Some(page.pair.groups_config().detail), By::Restore);
    direction(&mut page, &mut engine, &fixture, Dir::Right);
    let letter = engine.current(OWNER).unwrap().elem;
    assert_eq!(deliver(&mut page, &mut engine, &fixture, ScreenEvent::Activate(letter)), 1);
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(page.grid_position(engine.current(OWNER)), Some((0, 0)));
}

#[test]
fn sort_chosen_during_section_fade_commits_to_the_incoming_library() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(0);
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, (0..2).map(|i|
        crate::browse::view::SectionView { borrowed: false, sid: Some(sid), key: i as i64 + 1, kind: SecKind::Movie,
            row: crate::browse::SrcRow { section: i, title: format!("s{i:04x}"), pinned: true, current: i == 0, ..Default::default() } }).collect());
    let mut page = fixture.screen();
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    let mut fx = Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present);
    page.activate(page.libraries[1].0, false, &fixture.cx(None), &mut fx);
    page.activate(SORT, false, &fixture.cx(None), &mut fx);
    drop(fx);
    let incoming = SectionAddress { epoch: 1, sid, section: 2 };
    let target = output.iter().find_map(|effect| match &effect.fx {
        Fx::App(AppFx::Library(LibraryReq::Menu { target, .. })) => Some(*target), _ => None,
    }).unwrap();
    assert_eq!(target, incoming, "menu intent follows the requested page during its outgoing fade");
    output.clear();
    let mut fx = Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present);
    page.step(&ScreenEvent::App(AppMsg::LibraryEdit { target,
        edit: crate::stores::browse::QueryEdit::Sort { key: "titleSort".into(), desc: true } }), &fixture.cx(None), &mut fx);
    page.step(&ScreenEvent::WillLeave(crate::ui::machine::Leave::Deeper), &fixture.cx(None), &mut fx);
    drop(fx);
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(StoreId::Browse, StoreCmd::Browse(BrowseCmd::Addressed {
            target, work: LibraryWork::Commit { select: true, query: Some(crate::stores::browse::QueryEdit::Sort { key, desc: true }), .. }
        }))) if *target == incoming && key == "titleSort")), "leaving flushes selection and semantic sort in one addressed store command");
}

#[test]
fn singleton_borrowed_library_opens_sources_and_uses_value_chip_geometry() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, vec![
        crate::browse::view::SectionView { borrowed: true, sid: Some(crate::plex::ServerId::from_raw(0)), key: 1,
            kind: SecKind::Movie, row: crate::browse::SrcRow { section: 0, title: "Cinema".into(),
                pinned: true, current: true, ..Default::default() } }]);
    let mut page = fixture.screen();
    assert_eq!(page.libraries.len(), 1);
    let elem = page.libraries[0].0;
    let cx = fixture.cx(Some(page.key(elem)));
    let rect = page.place(&elem, &cx, At::Drawn).unwrap().rest_rect;
    assert_eq!(rect.w, crate::ui::value_chip::ValueChip::width(cx.measure, c"Library", c" · Cinema", None));
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.activate(elem, false, &cx, &mut Effects::new(&mut output, MachineId::Instance(InstanceId(19)), &mut present));
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Library(LibraryReq::Menu { kind: crate::screens::registry::LibraryMenuKind::Sources, anchor, .. }))
            if *anchor == [rect.x.to_bits(), rect.y.to_bits(), rect.w.to_bits(), rect.h.to_bits()])));
}

#[test]
fn favorite_library_row_uses_shared_strip_geometry_and_incoming_type() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let sid = crate::plex::ServerId::from_raw(0);
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, (0..4).map(|i|
        crate::browse::view::SectionView { borrowed: false, sid: Some(sid), key: i as i64 + 1,
            kind: if i < 2 { SecKind::Movie } else { SecKind::Show },
            row: crate::browse::SrcRow { section: i, title: format!("Library {i}"), pinned: true,
                current: i == 0, ..Default::default() } }).collect());
    let mut page = fixture.screen();
    let cx = fixture.cx(None);
    let lays = crate::ui::widgets::strip_layout_measured(["Library 0".into(), "Library 1".into()].into_iter(),
        MARGIN_X + crate::ui::widgets::STRIP_PAD, crate::ui::theme::size::BODY,
        crate::ui::widgets::STRIP_GAP_WIDE, cx.measure);
    for (i, lay) in lays.iter().enumerate() {
        let expected = crate::ui::widgets::strip_pill_rect(lay, CONTENT_TOP, crate::ui::widgets::StatusOverlay::CTRL_H);
        let actual = page.library_rect(i, &cx);
        assert_eq!([actual.x, actual.y, actual.w, actual.h], [expected.x, expected.y, expected.w, expected.h]);
    }
    page.pending.request_section(SectionTarget { epoch: 1, index: 3,
        identity: LibrarySectionIdentity { sid, key: 4 }, kind: SecKind::Show });
    page.sync(&cx);
    assert_eq!(page.libraries.iter().map(|(_, section)| *section).collect::<Vec<_>>(), vec![2, 3],
        "the favorite row must not keep movie libraries beneath the incoming Shows type");
}

#[test]
fn rapid_shelf_moves_use_settled_geometry_and_walk_each_document_row() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-shelf-geometry");
    session.watching("u-library-shelf-geometry");
    let mut fixture = Fixture::new();
    crate::browse::seed_two_source_table_for_test();
    fixture.directory.capture(); // Resolve this isolated profile's pins before choosing the subject.
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);
    crate::browse::section_hubs::seed_shelves_for_test(0, &["s0", "s1", "s2"], 12);
    fixture.directory.capture();
    fixture.listing = crate::stores::browse::listing_snapshot();
    fixture.hubs = crate::stores::browse::hubs_snapshot();
    let listing_id = fixture.listing.view().id().unwrap();
    let hubs_id = fixture.hubs.view().id().unwrap();
    assert_eq!((listing_id.epoch, listing_id.sid, listing_id.section), (hubs_id.epoch, hubs_id.sid, hubs_id.section));
    assert_eq!(fixture.hubs.view().shelves().len(), 3);
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let first = page.key(page.shelves[0].elems[3]);
    engine.set(OWNER, first, Some(page.shelves[0].group), By::Restore);
    let resting = page.place(&first.elem, &fixture.cx(Some(first)), At::Drawn).unwrap();
    let mut pressed_cx = fixture.cx(Some(first));
    pressed_cx.press = PressRead { scale: 0.85, is_long: true };
    let pressed = page.place(&first.elem, &pressed_cx, At::Drawn).unwrap();
    assert!(pressed.rect.w < resting.rect.w);
    let rect = |r: Rect| [r.x, r.y, r.w, r.h];
    assert_eq!(rect(pressed.rest_rect), rect(resting.rest_rect), "hold/menu opener stays on the unpressed card rectangle");
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current_group(OWNER), Some(page.shelves[1].group), "the grid cannot stand geometrically above the shelves that precede it");
    let key = engine.current(OWNER).unwrap();
    let drawn = page.place(&key.elem, &fixture.cx(Some(key)), At::Drawn).unwrap();
    let settled = page.place(&key.elem, &fixture.cx(Some(key)), At::SpringTarget).unwrap();
    assert_ne!(drawn.rect.y, settled.rect.y, "the second key must resolve against the destination document and scroll");
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current_group(OWNER), Some(page.shelves[2].group));
    assert_eq!(page.shelves[2].elems.iter().position(|elem| *elem == engine.current(OWNER).unwrap().elem), Some(3));
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
}

#[test]
fn shelf_publication_request_distinguishes_page_fade_from_grid_fade_and_head_focus() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut page = fixture.screen();
    page.initial = false;
    let grid = page.key(page.pair.detail.elem_at(0).unwrap());
    let request = |page: &mut LibraryScreen| {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        page.step(&ScreenEvent::Tick(Tick::default()), &fixture.cx(Some(grid)),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present));
        out.into_iter().find_map(|effect| match effect.fx {
            Fx::App(AppFx::Library(LibraryReq::PublishShelves { hidden_page, at_head, .. })) => Some((hidden_page, at_head)), _ => None,
        }).unwrap()
    };
    assert_eq!(request(&mut page), (false, true), "grid focus at a settled document head does not starve the first shelf publication");
    page.scroll.jump(700.0);
    page.scroll_target = 700.0;
    page.grid_fade.reload();
    assert_eq!(request(&mut page), (false, false), "a fading grid leaves the shelves visible");
    page.page_fade.reload();
    assert_eq!(request(&mut page), (false, false), "the outgoing page remains visible until the fade floor");
    page.page_fade.mount();
    assert_eq!(request(&mut page), (true, false), "only the full-page fade permits publication away from the head");
}
