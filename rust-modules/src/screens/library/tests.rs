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
