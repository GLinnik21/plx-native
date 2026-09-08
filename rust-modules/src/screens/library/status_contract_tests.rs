use super::super::{readout, LibraryScreen, Readout, FILTER, LIBRARY_GROUP, RETRY, SORT, STATUS_GROUP, TOOLBAR_GROUP};
use crate::browse::{SecFetch, SecKind, SrcGroup, SourceState, SrcRow};
use crate::screens::registry::{LibraryLike, PageMemory};
use crate::ui::fixture::FixtureMeasure;
use crate::ui::consts::SCR_W;
use crate::ui::machine::{Canon, Cx, EntryId, FocusKey, FocusRead, Host, InputOwner, InstanceId, LogicalState, PressRead, ScreenId, Tick};
use crate::ui::screen::{At, Focusable, GroupSpec, ScreenArg};

#[derive(Clone)]
struct Arg;

impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}

impl ScreenArg for Arg {
    fn chrome(&self) -> crate::ui::machine::Chrome { crate::ui::machine::Chrome::None }
    fn id(&self) -> ScreenId { ScreenId(1) }
    fn title(&self) -> Option<&str> { None }
    fn same_instance(&self, _: &Self) -> bool { true }
}

struct StatusHost;

#[derive(Clone, Copy)]
struct Views<'a> {
    listing: crate::stores::browse::ListingView<'a>,
    directory: crate::stores::browse::DirectoryView<'a>,
    hubs: crate::stores::browse::HubsView<'a>,
}

impl Host for StatusHost {
    type Arg = Arg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = Arg;
    type Memory = PageMemory;
}

impl LibraryLike for StatusHost {
    fn listing<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::ListingView<'a> { cx.views.listing }
    fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a> { cx.views.directory }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::HubsView<'a> { cx.views.hubs }
}

const ENTRY: EntryId = EntryId(91);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);

struct Fixture {
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    hubs: crate::stores::browse::HubsSnapshot,
    measure: FixtureMeasure,
}

impl Fixture {
    fn normal() -> Self {
        let sid = crate::plex::ServerId::from_raw(0);
        let listing = crate::stores::browse::ListingSnapshot::fixture(
            sid,
            (0..36).map(|i| Some(crate::pms::PmsMovie {
                sid,
                rk: format!("status-{i}"),
                title: format!("Status {i}"),
                ..Default::default()
            })).collect(),
            vec![("A".into(), 18), ("Z".into(), 18)],
        );
        let directory = crate::stores::browse::DirectorySnapshot::fixture(1, 0, vec![
            Self::section(0, 1, "Cinema", true),
            Self::section(1, 2, "Television", false),
        ]);
        Self { listing, directory, hubs: crate::stores::browse::hubs_snapshot(), measure: FixtureMeasure }
    }

    fn section(section: usize, key: i64, title: &str, current: bool) -> crate::browse::view::SectionView {
        crate::browse::view::SectionView {
            borrowed: false,
            sid: Some(crate::plex::ServerId::from_raw(0)),
            key,
            kind: SecKind::Movie,
            row: SrcRow { section, title: title.into(), pinned: true, current, ..Default::default() },
        }
    }

    fn listing(fetch: SecFetch, total: i64) -> crate::stores::browse::ListingSnapshot {
        let sid = crate::plex::ServerId::from_raw(0);
        crate::stores::browse::ListingSnapshot::fixture(
            sid,
            (0..total.max(0) as usize).map(|i| Some(crate::pms::PmsMovie {
                sid,
                rk: format!("status-{i}"),
                title: format!("Status {i}"),
                ..Default::default()
            })).collect(),
            Vec::new(),
        ).with_fetch(fetch, total)
    }

    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, StatusHost> {
        Cx {
            views: Views { listing: self.listing.view(), directory: self.directory.view(), hubs: self.hubs.view() },
            tick: Tick::default(),
            measure: &self.measure,
            focus: FocusRead { current: focus, ..Default::default() },
            press: PressRead::default(),
            owner: OWNER,
        }
    }

    fn screen(&self) -> LibraryScreen {
        let mut page = LibraryScreen::new(ENTRY, InstanceId(29), SecKind::Movie);
        page.sync(&self.cx(None));
        page
    }
}

#[test]
fn owned_readout_helper_preserves_the_legacy_precedence_matrix() {
    for (table, sections, fetch, total, expected) in [
        (SecFetch::Ready, 2, SecFetch::Failed, 185, Readout::Grid),
        (SecFetch::Ready, 2, SecFetch::Failed, -1, Readout::Failed),
        (SecFetch::Ready, 2, SecFetch::Ready, 0, Readout::Empty),
        (SecFetch::Ready, 2, SecFetch::Ready, 1, Readout::Grid),
        (SecFetch::Ready, 2, SecFetch::Loading, -1, Readout::Loading),
        (SecFetch::Ready, 0, SecFetch::Loading, -1, Readout::Empty),
        (SecFetch::Loading, 0, SecFetch::Loading, -1, Readout::Loading),
        (SecFetch::Failed, 0, SecFetch::Loading, -1, Readout::Failed),
        (SecFetch::Failed, 0, SecFetch::Ready, -1, Readout::Failed),
        (SecFetch::Failed, 0, SecFetch::Failed, -1, Readout::Failed),
        (SecFetch::Failed, 3, SecFetch::Failed, 185, Readout::Grid),
        (SecFetch::Failed, 3, SecFetch::Failed, -1, Readout::Failed),
    ] {
        assert_eq!(readout(table, sections, fetch, total), expected,
            "table={table:?}, sections={sections}, fetch={fetch:?}, total={total}");
    }
}

#[test]
fn failed_status_occupies_content_and_keeps_only_navigation_and_retry() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::normal();
    fixture.listing = Fixture::listing(SecFetch::Failed, -1);
    let page = fixture.screen();
    let cx = fixture.cx(None);
    assert_eq!(page.readout, Readout::Failed);

    let mut groups = Vec::<GroupSpec>::new();
    page.groups(&cx, &mut groups);
    assert!(groups.iter().any(|group| group.id == LIBRARY_GROUP), "the library row remains the way out");
    assert!(groups.iter().any(|group| group.id == STATUS_GROUP), "failure publishes its Retry control");
    assert!(!groups.iter().any(|group| group.id == TOOLBAR_GROUP));
    assert!(!groups.iter().any(|group| group.id == page.pair.groups_config().detail));
    assert!(!groups.iter().any(|group| group.id == page.pair.groups_config().master));
    assert!(page.place(&SORT, &cx, At::SpringTarget).is_none());
    assert!(page.place(&FILTER, &cx, At::SpringTarget).is_none());

    let frame = page.status_frame();
    let retry = page.place(&RETRY, &cx, At::SpringTarget).expect("failed status publishes Retry placement").rect;
    assert!(frame.y >= crate::screens::library::layout::CONTENT_TOP);
    assert!(crate::ui::consts::inside_safe(frame));
    assert!(crate::ui::consts::inside_safe(retry));
    let status = groups.iter().find(|group| group.id == STATUS_GROUP).unwrap();
    assert_eq!([status.extent.x, status.extent.y, status.extent.w, status.extent.h],
        [retry.x, retry.y, retry.w, retry.h]);
    assert_eq!(frame.x, crate::ui::consts::MARGIN_X);
    assert_eq!(frame.w, SCR_W - 2.0 * crate::ui::consts::MARGIN_X);
}

#[test]
fn empty_loading_and_failed_discovery_publish_no_false_grid_controls() {
    let _guard = crate::testlock::serial();
    let sid = crate::plex::ServerId::from_raw(7);
    let scenarios = [
        ("reachable empty table", crate::stores::browse::DirectorySnapshot::fixture_source(
            1, sid, SrcGroup { name: "Cinema server".into(), handle: String::new(), state: SourceState::Reachable, tier: None }, SecFetch::Ready),
            Fixture::listing(SecFetch::Loading, -1), Readout::Empty, false),
        ("still discovering table", crate::stores::browse::DirectorySnapshot::default(),
            crate::stores::browse::ListingSnapshot::absent(), Readout::Loading, false),
        ("failed sections", crate::stores::browse::DirectorySnapshot::fixture_source(
            2, sid, SrcGroup { name: "Cinema server".into(), handle: "friend".into(), state: SourceState::Unreachable, tier: None }, SecFetch::Failed),
            crate::stores::browse::ListingSnapshot::absent(), Readout::Failed, true),
    ];

    for (name, directory, listing, expected, retry) in scenarios {
        let mut fixture = Fixture::normal();
        fixture.directory = directory;
        fixture.listing = listing;
        let page = fixture.screen();
        let cx = fixture.cx(None);
        assert_eq!(page.readout, expected, "{name}");
        let mut groups = Vec::new();
        page.groups(&cx, &mut groups);
        assert_eq!(groups.iter().any(|group| group.id == STATUS_GROUP), retry, "{name}");
        assert_eq!(page.place(&RETRY, &cx, At::SpringTarget).is_some(), retry, "{name}");
        assert!(page.place(&SORT, &cx, At::SpringTarget).is_none(), "{name}");
        assert!(page.place(&FILTER, &cx, At::SpringTarget).is_none(), "{name}");
        assert!(!groups.iter().any(|group| group.id == TOOLBAR_GROUP), "{name}");
        assert!(!groups.iter().any(|group| group.id == page.pair.groups_config().detail), "{name}");
        assert!(!groups.iter().any(|group| group.id == page.pair.groups_config().master), "{name}");
    }
}

#[test]
fn failed_page_or_source_with_resident_items_stays_a_grid_without_status_controls() {
    let _guard = crate::testlock::serial();
    assert_eq!(readout(SecFetch::Failed, 3, SecFetch::Failed, 185), Readout::Grid,
        "a failed source with resident items keeps the grid verdict");
    let mut fixture = Fixture::normal();
    fixture.listing = Fixture::listing(SecFetch::Failed, 185);
    let page = fixture.screen();
    let cx = fixture.cx(None);
    assert_eq!(page.readout, Readout::Grid, "a failed page with resident items keeps the grid");
    let mut groups = Vec::new();
    page.groups(&cx, &mut groups);
    assert!(groups.iter().any(|group| group.id == page.pair.groups_config().detail));
    assert!(!groups.iter().any(|group| group.id == STATUS_GROUP));
    assert!(page.place(&RETRY, &cx, At::SpringTarget).is_none());
}
