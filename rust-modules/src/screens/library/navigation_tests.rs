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
    _stores: crate::stores::Stores,
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    hubs: crate::stores::browse::HubsSnapshot,
    sections: Vec<crate::stores::browse::SectionView>,
    epoch: u32,
}
impl Fixture {
    fn new(libraries: usize, items: usize, shelves: usize) -> Self {
        let stores = crate::stores::Stores::default();
        stores.browse.borrow_mut().seed_two_source_table_for_test();
        stores.browse_run(BrowseCmd::SetCur(0));
        let titles: Vec<_> = (0..shelves)
            .map(|i| format!("Synthetic shelf {i}"))
            .collect();
        let titles: Vec<_> = titles.iter().map(String::as_str).collect();
        if shelves > 0 {
            stores.browse.borrow_mut().seed_shelves_for_test(0, &titles, 4);
        }
        let epoch = stores.browse.borrow().table_epoch_for_test();
        let sid = crate::plex::ServerId::UNSET;
        let sections = (0..libraries)
            .map(|i| crate::stores::browse::SectionView {
                // `SectionView` no longer carries an ownership bit at all (issue #100/#165 — see
                // `screens/library/tests.rs`'s `a_single_favourite_library_draws_no_selector`), so
                // there is nothing left here to vary with the library count.
                sid: Some(sid),
                key: i as i64 + 1,
                kind: SecKind::Movie,
                row: crate::stores::browse::SrcRow {
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
        self.directory = crate::stores::browse::DirectorySnapshot::fixture(
            self.epoch,
            current,
            self.sections.clone(),
        );
        self.listing = crate::stores::browse::ListingSnapshot::fixture(
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
        crate::stores::browse::SectionView {
            sid: Some(sid),
            key: 1,
            kind: SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section: 0,
                title: "Movies".into(),
                pinned: true,
                ..Default::default()
            },
        },
        crate::stores::browse::SectionView {
            sid: Some(sid),
            key: 2,
            kind: SecKind::Show,
            row: crate::stores::browse::SrcRow {
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
        let mut expected = Vec::new();
        if libraries > 1 {
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

/// How the page was reached, as far as the focus engine can tell: a pointer click on the strip's
/// tab, a keyboard OK on it, or a boot/trigger open with nothing focused at all.
#[derive(Clone, Copy, Debug)]
enum Opened {
    Pointer,
    Keyboard,
    Unfocused,
}

/// Open a Library of `kind` whose A–Z grid is already prepared and whose own shelves have NOT
/// landed yet, and run it to its first seat.
fn opened_before_its_shelves(kind: SecKind, opened: Opened) -> (Fixture, LibraryScreen, FocusEngine<u32>) {
    let mut fixture = Fixture::new(1, 24, 0);
    for section in &mut fixture.sections { section.kind = kind; }
    fixture.publish(0, 24);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), kind);
    let mut engine = FocusEngine::new();
    let tab = FocusKey { entry: ENTRY, elem: crate::ui::dispatch::STRIP_BASE + 1 };
    match opened {
        Opened::Pointer => { engine.set(OWNER, tab, Some(STRIP), By::Pointer); }
        Opened::Keyboard => { engine.set(OWNER, tab, Some(STRIP), By::Dir); }
        Opened::Unfocused => {}
    }
    fixture.step(&mut page, &mut engine, ScreenEvent::Mount);
    for i in 0..3 {
        fixture.step(&mut page, &mut engine, ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_000 }));
    }
    (fixture, page, engine)
}

/// The shelves arrive: published by the store, observed by the page, a few frames run.
fn land_shelves(fixture: &mut Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>) {
    fixture._stores.browse.borrow_mut().seed_shelves_for_test(0, &["Synthetic shelf 0", "Synthetic shelf 1"], 4);
    fixture.hubs = fixture._stores.browse.borrow_mut().hubs_snapshot();
    fixture.step(page, engine, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    for i in 3..6 {
        fixture.step(page, engine, ScreenEvent::Tick(Tick { ms: i * 16, dt_us: 16_000 }));
    }
    assert_eq!(page.shelves.len(), 2, "the landing must reach the page's document");
}

/// **Field report, TV debug build of main a788b27a:** the Movies tab was clicked, the screen
/// cold-opened with its grid already prepared, `libhubs: section 0 landed 7 shelves` arrived
/// AFTER that, and the first DOWN put focus in the "All" block instead of on a shelf.
///
/// The page seats its own default focus on the document's first block the first frame it has
/// one. With the grid prepared and the shelves still in flight that block is the grid's heading
/// row, so focus sat on SORT; the shelves then committed above it (the page is at its head, so
/// `SecHubs::commit_staged` rightly lets them in) and pushed the seated control 2 × a shelf pitch
/// down, off the panel. Nothing re-seated, so the first DOWN walked on from the heading into the
/// grid. A seat nobody chose must follow the document's head until somebody moves — the same seat
/// the page takes when the shelves happen to land first. Held for both library kinds and every
/// entry path, because none of them is involved in the fault.
#[test]
fn a_shelf_landing_after_the_head_seat_moves_that_seat_onto_the_first_shelf() {
    let _guard = crate::testlock::serial();
    for kind in [SecKind::Movie, SecKind::Show] {
        for opened in [Opened::Pointer, Opened::Keyboard, Opened::Unfocused] {
            let (mut fixture, mut page, mut engine) = opened_before_its_shelves(kind, opened);
            assert_eq!(
                engine.current(OWNER).and_then(|key| page.group_of(&key.elem, &fixture.cx(&engine))),
                Some(TOOLBAR_GROUP),
                "{kind:?}/{opened:?}: with no shelves yet, the head of the document is the grid's heading"
            );
            land_shelves(&mut fixture, &mut page, &mut engine);
            assert_eq!(
                engine.current(OWNER),
                Some(page.key(page.shelves[0].elems[0])),
                "{kind:?}/{opened:?}: the landing must carry the page's own seat onto the first shelf's first card"
            );
            fixture.direction(&mut page, &mut engine, Dir::Down);
            assert_eq!(
                engine.current(OWNER),
                Some(page.key(page.shelves[1].elems[0])),
                "{kind:?}/{opened:?}: DOWN walks the shelves, never jumping to the grid's heading or the grid"
            );
        }
    }
}

/// …and only a seat nobody chose. Once the user has moved, a landing is not allowed to take
/// focus away from where they put it.
#[test]
fn a_shelf_landing_never_moves_a_seat_the_user_chose() {
    let _guard = crate::testlock::serial();
    let (mut fixture, mut page, mut engine) = opened_before_its_shelves(SecKind::Movie, Opened::Pointer);
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)));
    land_shelves(&mut fixture, &mut page, &mut engine);
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)), "the user's own move outranks the page's seat");
}

/// Pressing OK on the page's own seat is a choice as surely as moving is, and it moves nothing
/// (no `FocusMoved` reports it): opening Sort from the head seat and having the shelves land while
/// its menu is up must bring the reader back to Sort, not to a shelf.
#[test]
fn a_shelf_landing_never_moves_a_seat_the_user_activated() {
    let _guard = crate::testlock::serial();
    let (mut fixture, mut page, mut engine) = opened_before_its_shelves(SecKind::Movie, Opened::Keyboard);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)));
    fixture.step(&mut page, &mut engine, ScreenEvent::Activate(SORT));
    land_shelves(&mut fixture, &mut page, &mut engine);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)), "an activated seat is the user's");
}

/// …and the press is claimed where it STARTS, not where it commits: OK-down on the seat arms a
/// delayed press whose activation the dispatcher only delivers if focus is still on that key, so a
/// landing observed between the two would move focus and silently drop the press.
#[test]
fn a_shelf_landing_never_moves_a_seat_the_user_began_to_press() {
    let _guard = crate::testlock::serial();
    let press = |key, edge| ScreenEvent::Input(crate::ui::machine::InputEvent {
        at: Tick::default(),
        source: crate::ui::machine::Source::Sdl,
        kind: InputKind::Key { key, sym: 0, wcode: 0, edge, at_edge: false },
    });
    let (mut fixture, mut page, mut engine) = opened_before_its_shelves(SecKind::Movie, Opened::Keyboard);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)));
    fixture.step(&mut page, &mut engine, press(Key::Ok, Edge::Down));
    land_shelves(&mut fixture, &mut page, &mut engine);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)), "a press begun on the seat keeps it");
}

/// A pointer resting on the seat is the reader's too (a Magic Remote hovers without clicking), and
/// hovering the already-focused control moves nothing, so no `FocusMoved` would release it.
#[test]
fn a_shelf_landing_never_moves_a_seat_the_pointer_is_on() {
    let _guard = crate::testlock::serial();
    let (mut fixture, mut page, mut engine) = opened_before_its_shelves(SecKind::Movie, Opened::Pointer);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)));
    fixture.step(&mut page, &mut engine, ScreenEvent::Input(crate::ui::machine::InputEvent {
        at: Tick::default(),
        source: crate::ui::machine::Source::Sdl,
        kind: InputKind::Pointer { x: 0.0, y: 0.0, hit: Some(SORT) },
    }));
    land_shelves(&mut fixture, &mut page, &mut engine);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)), "a hovered seat is the user's");
}

/// **A restored seat is the reader's, even at the head.** Movies → Shows → Movies after leaving
/// focus on FILTER: returning to Movies restores its saved viewport (scroll 0) and the engine's
/// remembered FILTER, and the page's re-entry seat lands there. That seat is a restore, not the
/// page's own choice, so a shelf landing that follows must leave it where the reader left it.
#[test]
fn a_shelf_landing_never_moves_a_restored_seat_at_the_head() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new(2, 24, 0);
    fixture.sections[1].kind = SecKind::Show;
    fixture.publish(0, 24);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
    let mut engine = FocusEngine::new();
    let mut ms = 0;
    let mut frames = |fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, n: u32| {
        for _ in 0..n {
            ms += 16;
            fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_000 }));
        }
    };
    fixture.step(&mut page, &mut engine, ScreenEvent::Mount);
    frames(&fixture, &mut page, &mut engine, 3);
    assert!(page.libraries.is_empty(), "one library per kind draws no selector");
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)));
    fixture.direction(&mut page, &mut engine, Dir::Right);
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)));
    for (kind, section) in [(SecKind::Show, 1), (SecKind::Movie, 0)] {
        fixture.step(&mut page, &mut engine, ScreenEvent::App(AppMsg::Library(LibraryCmd::Enter(kind))));
        frames(&fixture, &mut page, &mut engine, 2);
        fixture.publish(section, 24);
        fixture.step(&mut page, &mut engine, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
        frames(&fixture, &mut page, &mut engine, 60);
        assert_eq!(page.kind, kind);
    }
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)), "the return restores the reader's seat");
    land_shelves(&mut fixture, &mut page, &mut engine);
    assert_eq!(engine.current(OWNER), Some(page.key(FILTER)), "a restored seat is the reader's");
}

/// **The engine's memory of a seat the PAGE placed is not a choice the reader made.** Every seat
/// is remembered by the engine, and TOOLBAR_GROUP is one group across sections — so Movies' own
/// automatic Sort seat, still remembered, read as a restore when Shows opened with its grid
/// before its shelves, and the Shows shelves then landed above Sort: the field report again, one
/// section later.
#[test]
fn a_page_placed_seat_remembered_by_the_engine_is_not_a_restore() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new(2, 24, 0);
    fixture.sections[1].kind = SecKind::Show;
    fixture.publish(0, 24);
    let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
    let mut engine = FocusEngine::new();
    let mut ms = 0;
    let mut frames = |fixture: &Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, n: u32| {
        for _ in 0..n {
            ms += 16;
            fixture.step(page, engine, ScreenEvent::Tick(Tick { ms, dt_us: 16_000 }));
        }
    };
    let land = |fixture: &mut Fixture, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, section: usize| {
        fixture._stores.browse_run(BrowseCmd::SetCur(section));
        fixture._stores.browse.borrow_mut().seed_shelves_for_test(section, &["Synthetic shelf 0", "Synthetic shelf 1"], 4);
        fixture.hubs = fixture._stores.browse.borrow_mut().hubs_snapshot();
        fixture.step(page, engine, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    };
    fixture.step(&mut page, &mut engine, ScreenEvent::Mount);
    frames(&fixture, &mut page, &mut engine, 3);
    assert_eq!(engine.current(OWNER), Some(page.key(SORT)), "Movies: grid first, so the page seats Sort");
    land(&mut fixture, &mut page, &mut engine, 0);
    frames(&fixture, &mut page, &mut engine, 3);
    assert_eq!(engine.current(OWNER), Some(page.key(page.shelves[0].elems[0])), "Movies: the seat follows the shelves");
    fixture._stores.browse_run(BrowseCmd::SetCur(1));
    fixture.hubs = fixture._stores.browse.borrow_mut().hubs_snapshot();
    fixture.step(&mut page, &mut engine, ScreenEvent::App(AppMsg::Library(LibraryCmd::Enter(SecKind::Show))));
    frames(&fixture, &mut page, &mut engine, 2);
    fixture.publish(1, 24);
    fixture.step(&mut page, &mut engine, ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1));
    frames(&fixture, &mut page, &mut engine, 60);
    assert_eq!(page.kind, SecKind::Show);
    assert!(page.shelves.is_empty(), "Shows: grid first, shelves still in flight");
    land(&mut fixture, &mut page, &mut engine, 1);
    frames(&fixture, &mut page, &mut engine, 3);
    assert_eq!(page.shelves.len(), 2, "Shows: the landing must reach the document");
    fixture.direction(&mut page, &mut engine, Dir::Down);
    let at = engine.current(OWNER).and_then(|key| page.group_of(&key.elem, &fixture.cx(&engine)));
    assert!(page.shelves.iter().any(|shelf| Some(shelf.group) == at),
        "Shows: DOWN must reach a shelf, not the grid's heading or the grid (focus in {at:?})");
}
