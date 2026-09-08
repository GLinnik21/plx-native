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
impl Fixture {
    fn new() -> Self {
        crate::browse::reset();
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
    engine.set(OWNER, exact, Some(GRID_GROUP), By::Restore);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Right), 0);
    assert_eq!(engine.current_group(OWNER), Some(RAIL_GROUP));
    assert_eq!(page.pair.master.start_for_elem(engine.current(OWNER).unwrap().elem), Some(0));
    assert_eq!(engine.remembered_for(ENTRY).iter().find(|(g, _)| *g == GRID_GROUP).unwrap().1, exact.elem);
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
    engine.set(OWNER, exact, Some(GRID_GROUP), By::Restore);
    engine.set(OWNER, page.key(FILTER), Some(TOOLBAR_GROUP), By::Dir);
    assert_eq!(direction(&mut page, &mut engine, &fixture, Dir::Right), 0);
    assert_eq!(page.pair.master.start_for_elem(engine.current(OWNER).unwrap().elem), Some(18));
    direction(&mut page, &mut engine, &fixture, Dir::Left);
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)));
    assert_eq!(engine.remembered_for(ENTRY).iter().find(|(g, _)| *g == GRID_GROUP).unwrap().1, exact.elem);
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
    engine.set(OWNER, exact, Some(GRID_GROUP), By::Restore);
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
        crate::browse::view::SectionView { sid: Some(sid), key: i as i64 + 1, kind: SecKind::Movie,
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
fn rapid_shelf_moves_use_settled_geometry_and_walk_each_document_row() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-shelf-geometry");
    session.watching("u-library-shelf-geometry");
    let mut fixture = Fixture::new();
    crate::browse::seed_two_source_table_for_test();
    fixture.directory.capture(); // Resolve this isolated profile's pins before choosing the subject.
    crate::browse::set_cur(0);
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
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current_group(OWNER), Some(page.shelves[1].group), "the grid cannot stand geometrically above the shelves that precede it");
    let key = engine.current(OWNER).unwrap();
    let drawn = page.place(&key.elem, &fixture.cx(Some(key)), At::Drawn).unwrap();
    let settled = page.place(&key.elem, &fixture.cx(Some(key)), At::SpringTarget).unwrap();
    assert_ne!(drawn.rect.y, settled.rect.y, "the second key must resolve against the destination document and scroll");
    direction(&mut page, &mut engine, &fixture, Dir::Down);
    assert_eq!(engine.current_group(OWNER), Some(page.shelves[2].group));
    assert_eq!(page.shelves[2].elems.iter().position(|elem| *elem == engine.current(OWNER).unwrap().elem), Some(3));
    crate::browse::reset();
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
    assert_eq!(request(&mut page), (true, false), "only the full-page fade permits publication away from the head");
}
