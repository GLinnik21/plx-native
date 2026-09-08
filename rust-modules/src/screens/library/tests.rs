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
    fn new() -> Self {
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        let sid = crate::plex::ServerId::from_raw(0);
        let listing = crate::browse::view::ListingSnapshot::fixture(sid, (0..36).map(|i|
            Some(crate::pms::PmsMovie { sid, rk: format!("{}", i + 1), title: format!("s{i:04x}"), ..Default::default() })).collect(),
            vec![("A".into(), 18), ("Z".into(), 18)]);
        Self { listing, directory: Default::default(), hubs: crate::stores::browse::hubs_snapshot(), measure: FixtureMeasure }
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
