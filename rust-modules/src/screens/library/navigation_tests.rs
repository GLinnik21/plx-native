//! Owned engine ports for requested kinds, source-row seating and document navigation.
use super::*;
use crate::ui::fixture::{FixtureArg, FixtureMeasure};
use crate::ui::focus::{FocusEngine, Outcome};
use crate::ui::machine::{Host, InputOwner, Tick};

struct TestHost;
#[derive(Clone, Copy)]
struct Views<'a> {
    listing: crate::stores::browse::ListingView<'a>,
    directory: crate::stores::browse::DirectoryView<'a>,
    hubs: crate::stores::browse::HubsView<'a>,
}
impl Host for TestHost {
    type Arg = FixtureArg;
    type Fx = AppFx;
    type Msg = AppMsg;
    type Elem = u32;
    type Views<'a> = Views<'a>;
    type Init = FixtureArg;
    type Memory = PageMemory;
}
impl LibraryLike for TestHost {
    fn listing<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::ListingView<'a> {
        cx.views.listing
    }
    fn directory<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::DirectoryView<'a> {
        cx.views.directory
    }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> crate::stores::browse::HubsView<'a> {
        cx.views.hubs
    }
}
const ENTRY: EntryId = EntryId(81);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);
struct Fixture {
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    hubs: crate::stores::browse::HubsSnapshot,
    sections: Vec<crate::browse::view::SectionView>,
    epoch: u32,
}
impl Fixture {
    fn new(libraries: usize, items: usize, shelves: usize) -> Self {
        crate::stores::browse::apply(BrowseCmd::Reset);
        crate::browse::seed_two_source_table_for_test();
        crate::stores::browse::apply(BrowseCmd::SetCur(0));
        let titles: Vec<_> = (0..shelves)
            .map(|i| format!("Synthetic shelf {i}"))
            .collect();
        let titles: Vec<_> = titles.iter().map(String::as_str).collect();
        if shelves > 0 {
            crate::browse::section_hubs::seed_shelves_for_test(0, &titles, 4);
        }
        let epoch = crate::browse::table_epoch();
        let sid = crate::plex::ServerId::UNSET;
        let sections = (0..libraries)
            .map(|i| crate::browse::view::SectionView {
                borrowed: libraries == 1,
                sid: Some(sid),
                key: i as i64 + 1,
                kind: SecKind::Movie,
                row: crate::browse::SrcRow {
                    section: i,
                    title: format!("Library {i}"),
                    pinned: true,
                    current: i == 0,
                    ..Default::default()
                },
            })
            .collect();
        let mut fixture = Self {
            listing: crate::stores::browse::listing_snapshot(),
            directory: Default::default(),
            hubs: crate::stores::browse::hubs_snapshot(),
            sections,
            epoch,
        };
        fixture.publish(0, items);
        fixture
    }
    fn publish(&mut self, current: usize, items: usize) {
        let sid = crate::plex::ServerId::UNSET;
        let kind = if self
            .sections
            .get(current)
            .is_some_and(|section| section.kind == SecKind::Show)
        {
            1
        } else {
            0
        };
        self.directory = crate::browse::view::DirectorySnapshot::fixture(
            self.epoch,
            current,
            self.sections.clone(),
        );
        self.listing = crate::browse::view::ListingSnapshot::fixture(
            sid,
            (0..items)
                .map(|i| {
                    Some(crate::pms::PmsMovie {
                        sid,
                        kind,
                        rk: format!("{}", i + 1),
                        ..Default::default()
                    })
                })
                .collect(),
            vec![
                ("A".into(), items as i64 / 2),
                ("Z".into(), items as i64 - items as i64 / 2),
            ],
        )
        .with_section(self.epoch, current as i64 + 1);
    }
    fn cx(&self, engine: &FocusEngine<u32>) -> Cx<'_, TestHost> {
        Cx {
            views: Views {
                listing: self.listing.view(),
                directory: self.directory.view(),
                hubs: self.hubs.view(),
            },
            tick: Tick::default(),
            measure: &FixtureMeasure,
            focus: engine.read(OWNER),
            press: Default::default(),
            owner: OWNER,
        }
    }
    fn screen(&self) -> LibraryScreen {
        let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
        page.sync(&self.cx(&FocusEngine::new()));
        page
    }
    fn step(
        &self,
        page: &mut LibraryScreen,
        engine: &mut FocusEngine<u32>,
        event: ScreenEvent<TestHost>,
    ) -> Vec<AppFx> {
        let mut queue = std::collections::VecDeque::from([event]);
        let mut apps = Vec::new();
        let mut present = crate::ui::present::Present::new();
        while let Some(event) = queue.pop_front() {
            let mut out = Vec::new();
            page.step(
                &event,
                &self.cx(engine),
                &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present),
            );
            if let ScreenEvent::Enter(Enter::Fresh { focus }) = event {
                if let Outcome::Moved { from, to, by } =
                    engine.enter(OWNER, page, focus, None, &self.cx(engine))
                {
                    queue.push_back(ScreenEvent::FocusMoved { from, to, by });
                }
            }
            for effect in out {
                match effect.fx {
                    Fx::Remember { group, elem } => engine.remember_projected(ENTRY, group, elem),
                    Fx::Deliver(_, Delivery::Screen(event)) => queue.push_back(event),
                    Fx::App(app) => apps.push(app),
                    _ => {}
                }
            }
        }
        apps
    }
    fn direction(&self, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, dir: Dir) {
        let mut links = Vec::new();
        <LibraryScreen as Screen<TestHost>>::links(page, &mut links);
        if let Outcome::Moved { from, to, by } =
            engine.move_dir(OWNER, page, &links, dir, &self.cx(engine))
        {
            self.step(page, engine, ScreenEvent::FocusMoved { from, to, by });
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::stores::browse::apply(BrowseCmd::Reset);
    }
}

#[test]
fn shows_requested_before_discovery_stays_loading_and_never_fetches_the_foreign_movie_listing() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new(0, 12, 0);
    fixture.directory = Default::default();
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Show);
    let mut engine = FocusEngine::new();
    for i in 0..30 {
        let effects = fixture.step(
            &mut page,
            &mut engine,
            ScreenEvent::Tick(Tick {
                ms: i * 16,
                dt_us: 16_000,
            }),
        );
        assert_eq!(page.kind, SecKind::Show);
        assert_eq!(page.wanted_kind, Some(SecKind::Show));
        assert_eq!(page.readout, Readout::Loading);
        assert!(page.pair.detail.elems.is_empty());
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            AppFx::Store(_, StoreCmd::Browse(BrowseCmd::Addressed { .. }))
        )));
    }
    let sid = crate::plex::ServerId::UNSET;
    fixture.sections = vec![
        crate::browse::view::SectionView {
            borrowed: false,
            sid: Some(sid),
            key: 1,
            kind: SecKind::Movie,
            row: crate::browse::SrcRow {
                section: 0,
                title: "Movies".into(),
                pinned: true,
                ..Default::default()
            },
        },
        crate::browse::view::SectionView {
            borrowed: false,
            sid: Some(sid),
            key: 2,
            kind: SecKind::Show,
            row: crate::browse::SrcRow {
                section: 1,
                title: "Shows".into(),
                pinned: true,
                ..Default::default()
            },
        },
    ];
    fixture.publish(0, 12);
    let effects = fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick::default()));
    assert!(effects.iter().any(|effect| matches!(effect,
        AppFx::Store(_, StoreCmd::Browse(BrowseCmd::Addressed { target,
            work: LibraryWork::Commit { select: true, .. } })) if target.section == 2)));
    assert!(!effects.iter().any(|effect| matches!(effect,
        AppFx::Store(_, StoreCmd::Browse(BrowseCmd::Addressed { target, .. })) if target.section == 1)));
    assert!(page.pair.detail.elems.is_empty());
    fixture.publish(1, 12);
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
    );
    assert_eq!(page.kind, SecKind::Show);
    assert_eq!(page.wanted_kind, None);
    assert_eq!(page.readout, Readout::Grid);
    assert_eq!(page.pair.detail.elems.len(), 12);
    assert!((0..12).all(|i| fixture.listing.view().item(i).unwrap().kind == 1));
}

#[test]
fn an_external_sources_selection_reseats_the_library_row_once_not_on_metadata_refresh() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new(12, 12, 0);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    assert!(
        page.libraries.iter().any(|(elem, _)| *elem == MORE),
        "the fixture must have actual overflow"
    );
    engine.set(OWNER, page.key(MORE), Some(LIBRARY_GROUP), By::Restore);
    let effects = fixture.step(&mut page, &mut engine, ScreenEvent::Activate(MORE));
    assert!(effects.iter().any(|effect| matches!(
        effect,
        AppFx::Library(LibraryReq::Menu {
            kind: crate::screens::registry::LibraryMenuKind::Sources,
            ..
        })
    )));
    let target = SectionAddress {
        epoch: fixture.epoch,
        sid: crate::plex::ServerId::UNSET,
        section: 3,
    };
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::App(AppMsg::LibrarySelect(target)),
    );
    let effects = fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::Tick(Tick {
            ms: 80,
            dt_us: 80_000,
        }),
    );
    assert!(effects.iter().any(|effect| matches!(effect, AppFx::Store(_, StoreCmd::Browse(
        BrowseCmd::Addressed { target: actual, work: LibraryWork::Commit { select: true, .. } })) if *actual == target)));
    fixture.publish(2, 12);
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
    );
    let selected = page
        .libraries
        .iter()
        .position(|(_, index)| *index == 2)
        .unwrap();
    assert!(
        selected > 0,
        "a first-slot fallback must not accidentally satisfy this test"
    );
    let wanted = page.key(page.libraries[selected].0);
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::Tick(Tick {
            ms: 96,
            dt_us: 16_000,
        }),
    );
    assert_eq!(
        engine.current(OWNER),
        Some(wanted),
        "the row must seat the library selected in Sources"
    );
    engine.set(OWNER, page.key(MORE), Some(LIBRARY_GROUP), By::Dir);
    fixture.publish(2, 24);
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::StoreChanged(StoreId::Browse.ord(), 2),
    );
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::Tick(Tick {
            ms: 112,
            dt_us: 16_000,
        }),
    );
    assert_eq!(
        engine.current(OWNER),
        Some(page.key(MORE)),
        "same-selection refresh must not leash the cursor"
    );
}

#[test]
fn source_controls_and_document_groups_match_bare_shelves_full_and_failed_content() {
    let _guard = crate::testlock::serial();
    for (libraries, items, shelves, failed) in [
        (0, 0, 0, false),
        (2, 0, 2, false),
        (2, 12, 2, false),
        (1, 12, 0, false),
        (2, 0, 2, true),
    ] {
        let mut fixture = Fixture::new(libraries, items, shelves);
        if libraries == 0 {
            fixture.directory = Default::default();
        }
        if failed {
            fixture.listing = fixture.listing.clone().with_fetch(SecFetch::Failed, -1);
        }
        let mut page = fixture.screen();
        page.initial = false;
        let mut engine = FocusEngine::new();
        let mut groups = Vec::new();
        page.groups(&fixture.cx(&engine), &mut groups);
        let mut expected = Vec::new();
        if libraries > 0 {
            expected.push(LIBRARY_GROUP);
        }
        expected.extend(page.shelves.iter().map(|shelf| shelf.group));
        assert_eq!(page.shelves.len(), shelves);
        if items > 0 {
            expected.push(TOOLBAR_GROUP);
            expected.push(page.pair.groups_config().detail);
        }
        if failed {
            expected.push(STATUS_GROUP);
        }
        assert_eq!(
            groups.iter().map(|group| group.id).collect::<Vec<_>>(),
            expected
        );
        for control in [SORT, FILTER] {
            assert_eq!(
                page.place(&control, &fixture.cx(&engine), At::SpringTarget)
                    .is_some(),
                items > 0
            );
        }
        if expected.is_empty() {
            continue;
        }
        fixture.step(
            &mut page,
            &mut engine,
            ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(expected[0]),
            }),
        );
        for wanted in expected.iter().skip(1) {
            fixture.direction(&mut page, &mut engine, Dir::Down);
            assert_eq!(
                page.group_of(&engine.current(OWNER).unwrap().elem, &fixture.cx(&engine)),
                Some(*wanted)
            );
        }
        if items == 0 {
            let at_end = engine.current(OWNER);
            fixture.direction(&mut page, &mut engine, Dir::Down);
            assert_eq!(
                engine.current(OWNER),
                at_end,
                "shelves/status are the real foot when no grid exists"
            );
        }
        // The foot boundary must not strand Retry: Up still returns through
        // the preceding document blocks, just as ordinary grid/toolbar Up does.
        for wanted in expected[..expected.len() - 1].iter().rev() {
            fixture.direction(&mut page, &mut engine, Dir::Up);
            assert_eq!(
                page.group_of(&engine.current(OWNER).unwrap().elem, &fixture.cx(&engine)),
                Some(*wanted),
                "Up must retrace the document, including Retry's preceding shelf"
            );
        }
        if items > 0 {
            fixture.step(
                &mut page,
                &mut engine,
                ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::ContainerGroup(TOOLBAR_GROUP),
                }),
            );
            let key = engine.current(OWNER).unwrap();
            assert!(
                [SORT, FILTER].contains(&key.elem),
                "Source is never a toolbar stop"
            );
            assert_eq!(
                groups
                    .iter()
                    .find(|group| group.id == TOOLBAR_GROUP)
                    .unwrap()
                    .len,
                2
            );
            fixture.direction(&mut page, &mut engine, Dir::Left);
            assert_eq!(engine.current(OWNER).unwrap().elem, SORT);
            fixture.direction(&mut page, &mut engine, Dir::Left);
            assert_eq!(engine.current(OWNER).unwrap().elem, SORT);
            fixture.direction(&mut page, &mut engine, Dir::Right);
            assert_eq!(engine.current(OWNER).unwrap().elem, FILTER);
            fixture.direction(&mut page, &mut engine, Dir::Left);
            assert_eq!(engine.current(OWNER).unwrap().elem, SORT);
        }
    }
}
