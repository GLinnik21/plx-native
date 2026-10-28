//! Owned engine ports for requested kinds, source-row seating and document navigation.
use super::*;
use plx_ui::fixture::{FixtureArg, FixtureMeasure};
use plx_ui::focus::{FocusEngine, Outcome};
use plx_machine::machine::{Host, InputOwner, Tick};

struct TestHost;

#[derive(Clone, Copy)]
struct Views<'a> {
    listing: plx_data::stores::browse::ListingView<'a>,
    directory: plx_data::stores::browse::DirectoryView<'a>,
    hubs: plx_data::stores::browse::HubsView<'a>,
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
    fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> {
        cx.views.listing
    }
    fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> {
        cx.views.directory
    }
    fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> {
        cx.views.hubs
    }
    /// The app's probe, in the one respect the header cares about: it answers for ANY section of
    /// the directory the probe has learned (`collections_probe::count`, which the app warms for
    /// every section when the directory lands), and only once a test has said the probe landed
    /// ([`land_collections`]). A section the test has not landed is the probe that has not answered
    /// yet: `None`, whichever library the row names.
    fn collections_count(cx: &Cx<'_, Self>, section: usize) -> Option<Result<u32, crate::registry::Forbidden>> {
        let key = u32::try_from(cx.views.directory.sections().get(section)?.key).ok()?;
        COLLECTION_ANSWERS.with(|answers| answers.borrow().iter().find(|(seen, _)| *seen == key).map(|(_, answer)| *answer))
    }
}
thread_local! {
    /// What the fixture's collections probe has learned, by section key.
    static COLLECTION_ANSWERS: std::cell::RefCell<Vec<(u32, Result<u32, crate::registry::Forbidden>)>> = const { std::cell::RefCell::new(Vec::new()) };
}
/// The fixture's probe for section `key` returns: `Ok(0)` / `Err` hide the control, `Ok(n)` counts.
fn land_collections(key: u32, answer: Result<u32, crate::registry::Forbidden>) {
    COLLECTION_ANSWERS.with(|answers| {
        let mut answers = answers.borrow_mut();
        answers.retain(|(seen, _)| *seen != key);
        answers.push((key, answer));
    });
}
const ENTRY: EntryId = EntryId(81);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);
struct Fixture {
    _stores: plx_data::stores::Stores,
    listing: plx_data::stores::browse::ListingSnapshot,
    directory: plx_data::stores::browse::DirectorySnapshot,
    hubs: plx_data::stores::browse::HubsSnapshot,
    sections: Vec<plx_data::stores::browse::SectionView>,
    epoch: u32,
}
impl Fixture {
    fn new(libraries: usize, items: usize, shelves: usize) -> Self {
        let stores = plx_data::stores::Stores::default();
        stores.browse.borrow_mut().seed_two_source_table_for_test();
        stores.browse_run_for_test(BrowseCmd::SetCur(0));
        let titles: Vec<_> = (0..shelves)
            .map(|i| format!("Synthetic shelf {i}"))
            .collect();
        let titles: Vec<_> = titles.iter().map(String::as_str).collect();
        // Zero shelves is the first-paint window having closed with no answer (the set is
        // settled and empty) — a section reveal waits for its shelves to settle.
        stores.browse.borrow_mut().seed_shelves_for_test(0, &titles, 4);
        let epoch = stores.browse.borrow().table_epoch_for_test();
        let sid = plx_plex::plex::ServerId::UNSET;
        let sections = (0..libraries)
            .map(|i| plx_data::stores::browse::SectionView {
                // `SectionView` no longer carries an ownership bit at all (issue #100/#165 — see
                // `screens/library/tests.rs`'s `a_single_favourite_library_draws_no_selector`), so
                // there is nothing left here to vary with the library count.
                sid: Some(sid),
                key: i as i64 + 1,
                kind: SecKind::Movie,
                row: plx_data::stores::browse::SrcRow {
                    section: i,
                    title: format!("Library {i}"),
                    pinned: true,
                    current: i == 0,
                    ..Default::default()
                },
            })
            .collect();
        let listing = stores.browse.borrow_mut().listing_snapshot();
        let hubs = stores.browse.borrow_mut().hubs_snapshot();
        let mut fixture = Self {
            listing,
            directory: Default::default(),
            hubs,
            _stores: stores,
            sections,
            epoch,
        };
        fixture.publish(0, items);
        fixture
    }
    fn publish(&mut self, current: usize, items: usize) {
        let sid = plx_plex::plex::ServerId::UNSET;
        let kind = if self
            .sections
            .get(current)
            .is_some_and(|section| section.kind == SecKind::Show)
        {
            1
        } else {
            0
        };
        self.directory = plx_data::stores::browse::DirectorySnapshot::fixture(
            self.epoch,
            current,
            self.sections.clone(),
        );
        self.listing = plx_data::stores::browse::ListingSnapshot::fixture(
            sid,
            (0..items)
                .map(|i| {
                    Some(plx_data::pms::PmsMovie {
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
        let mut present = plx_machine::present::Present::new();
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
#[test]
fn shows_requested_before_discovery_stays_loading_and_never_fetches_the_foreign_movie_listing() {
    let _guard = plx_base::testlock::serial();
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
        assert!(page.pair.detail.total() == 0);
        assert!(!effects.iter().any(|effect| matches!(
            effect,
            AppFx::Store(_, StoreCmd::Browse(BrowseCmd::Addressed { .. }))
        )));
    }
    let sid = plx_plex::plex::ServerId::UNSET;
    fixture.sections = vec![
        plx_data::stores::browse::SectionView {
            sid: Some(sid),
            key: 1,
            kind: SecKind::Movie,
            row: plx_data::stores::browse::SrcRow {
                section: 0,
                title: "Movies".into(),
                pinned: true,
                ..Default::default()
            },
        },
        plx_data::stores::browse::SectionView {
            sid: Some(sid),
            key: 2,
            kind: SecKind::Show,
            row: plx_data::stores::browse::SrcRow {
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
    assert!(page.pair.detail.total() == 0);
    fixture.publish(1, 12);
    fixture.step(
        &mut page,
        &mut engine,
        ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
    );
    assert_eq!(page.kind, SecKind::Show);
    assert_eq!(page.wanted_kind, None);
    assert_eq!(page.readout, Readout::Grid);
    assert_eq!(page.pair.detail.total(), 12);
    assert!((0..12).all(|i| fixture.listing.view().item(i).unwrap().kind == 1));
}

#[test]
fn an_external_sources_selection_reseats_the_library_row_once_not_on_metadata_refresh() {
    let _guard = plx_base::testlock::serial();
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
            kind: crate::registry::LibraryMenuKind::Sources,
            ..
        })
    )));
    let target = SectionAddress {
        epoch: fixture.epoch,
        sid: plx_plex::plex::ServerId::UNSET,
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
fn source_controls_and_document_groups_match_bare_full_and_failed_content() {
    let _guard = plx_base::testlock::serial();
    for (libraries, items, shelves, failed) in [
        (0, 0, 0, false),
        (2, 0, 2, false),
        (2, 12, 2, false),
        // A SINGLE favourite (issue #100/#165) never contributes a selector group any more —
        // `SectionView` carries no ownership bit to make it ambiguous, see
        // `screens/library/tests.rs`'s `a_single_favourite_library_draws_no_selector`. This tuple
        // keeps the 1-library case in the census below, now asserting its ABSENCE rather than its
        // presence.
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
        // One header row (names, "+N" and the controls) always leads the document.
        let mut expected = vec![LIBRARY_GROUP];
        let head = true;
        if items > 0 {
            expected.push(page.pair.groups_config().detail);
        }
        if failed {
            expected.push(STATUS_GROUP);
        }
        // A loading listing declares its (empty) grid group so a fresh enter into it seats nothing
        // (`PageRegions::declare_pending`): focus-inert, so it is not a control of the document.
        assert_eq!(
            groups.iter().filter(|group| group.len > 0).map(|group| group.id).collect::<Vec<_>>(),
            expected
        );
        for control in [SORTFILTER] {
            assert_eq!(
                page.place(&control, &fixture.cx(&engine), At::SpringTarget)
                    .is_some(),
                head
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
            let heading = page.toolbar_group();
            fixture.step(
                &mut page,
                &mut engine,
                ScreenEvent::Enter(Enter::Fresh {
                    focus: FocusTarget::ContainerGroup(heading),
                }),
            );
            let key = engine.current(OWNER).unwrap();
            let elems = page.header_elems();
            assert!(elems.contains(&key.elem), "the header row is one stop sequence");
            assert_eq!(
                groups.iter().find(|group| group.id == LIBRARY_GROUP).unwrap().len,
                elems.len()
            );
            for _ in 0..elems.len() {
                fixture.direction(&mut page, &mut engine, Dir::Left);
            }
            assert_eq!(engine.current(OWNER).unwrap().elem, elems[0], "Left stops at the first name");
            for want in elems.iter().skip(1) {
                fixture.direction(&mut page, &mut engine, Dir::Right);
                assert_eq!(engine.current(OWNER).unwrap().elem, *want, "Right walks names, +N, then the controls");
            }
            fixture.direction(&mut page, &mut engine, Dir::Right);
            assert_eq!(engine.current(OWNER).unwrap().elem, SORTFILTER, "Right stops at Sort & Filter");
        }
    }
}

/// DOWN from a heading chip lands on the cell the last UP left from (`PageRegions`'
/// `CONTENT_HEAD_SEAT`: Up then Down is a round trip, whatever stop of the row the walk ended on);
/// only a grid with no memory takes the tile under the chip (the projection that dragged the page
/// deep into All from its heading when the remembered cell was rows away is the memory's cell now,
/// and it is the one the reader just left).
#[test]
fn down_from_a_heading_chip_lands_on_the_remembered_tile_else_the_one_under_it() {
    let _guard = plx_base::testlock::serial();
    let fixture = Fixture::new(1, 24, 0);
    let mut page = fixture.screen();
    page.initial = false;
    let mut engine = FocusEngine::new();
    let grid = page.pair.groups_config().detail;
    let deep = page.pair.detail.elem_at(15).expect("a 24-item grid");
    let seat = page.key(SORTFILTER);
    fixture.step(&mut page, &mut engine, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(seat) }));
    let chip = page.place(&SORTFILTER, &fixture.cx(&engine), At::SpringTarget).unwrap().rect;
    let under = (0..page.layout.cols()).map(|col| page.pair.detail.elem_at(col).unwrap())
        .min_by(|a, b| {
            let d = |elem: &u32| (page.place(elem, &fixture.cx(&engine), At::SpringTarget).unwrap().rect.cx() - chip.cx()).abs();
            d(a).total_cmp(&d(b))
        }).unwrap();
    assert_ne!(under, page.pair.detail.elem_at(0).unwrap(), "the fixture's SORTFILTER chip stands over a later column");
    assert_ne!(under, deep, "the remembered cell is not the one under the chip");
    // No memory: the projection under the chip.
    fixture.direction(&mut page, &mut engine, Dir::Down);
    assert_eq!(engine.current(OWNER), Some(page.key(under)),
        "with nothing remembered, DOWN from SORTFILTER lands in the grid's first row under the chip");
    // Memory: the cell Up left from, whatever the chip stands over.
    let mut engine = FocusEngine::new();
    engine.remember_projected(ENTRY, grid, deep);
    fixture.step(&mut page, &mut engine, ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(seat) }));
    fixture.direction(&mut page, &mut engine, Dir::Down);
    assert_eq!(engine.current(OWNER), Some(page.key(deep)),
        "DOWN from SORTFILTER returns to the cell the last UP left from");
}

#[cfg(test)]
mod header_tests { include!("header_tests.rs"); }
