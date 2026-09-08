use super::super::*;
use super::*;
use crate::ui::fixture::FixtureMeasure;
use crate::ui::focus::{FocusEngine, Outcome};
use crate::ui::machine::{FocusRead, Host, InputOwner, PressRead, Tick};
use crate::ui::screen::ScreenArg;
include!("query_tests.rs");

#[derive(Clone)]
struct Arg;
impl LogicalState for Arg {
    fn write(&self, _: &mut Canon) {}
    fn probe(&self, _: &mut String) {}
}
impl ScreenArg for Arg {
    fn chrome(&self) -> crate::ui::machine::Chrome {
        crate::ui::machine::Chrome::None
    }
    fn id(&self) -> crate::ui::machine::ScreenId {
        crate::ui::machine::ScreenId(1)
    }
    fn title(&self) -> Option<&str> {
        None
    }
    fn same_instance(&self, _: &Self) -> bool {
        true
    }
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
    listing: crate::browse::view::ListingSnapshot,
    directory: crate::browse::view::DirectorySnapshot,
    hubs: crate::browse::section_hubs::HubsSnapshot,
    measure: FixtureMeasure,
}
impl Fixture {
    fn new() -> Self {
        crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
        let sid = crate::plex::ServerId::from_raw(0);
        let listing = crate::browse::view::ListingSnapshot::fixture(
            sid,
            (0..36)
                .map(|i| {
                    Some(crate::pms::PmsMovie {
                        sid,
                        rk: format!("{}", i + 1),
                        title: format!("s{i:04x}"),
                        ..Default::default()
                    })
                })
                .collect(),
            vec![("A".into(), 18), ("Z".into(), 18)],
        );
        let directory = crate::browse::view::DirectorySnapshot::fixture(
            1,
            0,
            vec![crate::browse::view::SectionView {
                borrowed: false,
                sid: Some(sid),
                key: 1,
                kind: SecKind::Movie,
                row: crate::browse::SrcRow {
                    section: 0,
                    title: "Cinema".into(),
                    pinned: true,
                    current: true,
                    ..Default::default()
                },
            }],
        );
        Self {
            listing,
            directory,
            hubs: crate::stores::browse::hubs_snapshot(),
            measure: FixtureMeasure,
        }
    }
    fn cx(&self, focus: Option<FocusKey<u32>>) -> Cx<'_, HostFixture> {
        Cx {
            views: Views {
                listing: self.listing.view(),
                directory: self.directory.view(),
                hubs: self.hubs.view(),
            },
            tick: Tick::default(),
            measure: &self.measure,
            focus: FocusRead {
                current: focus,
                ..Default::default()
            },
            press: PressRead::default(),
            owner: OWNER,
        }
    }
    fn screen(&self) -> LibraryScreen {
        let mut page = LibraryScreen::new(ENTRY, InstanceId(19), SecKind::Movie);
        page.sync(&self.cx(None));
        page
    }
}

#[test]
fn fresh_bookmarks_follow_stable_items_then_slots_and_keep_the_returned_card_visible() {
    let _guard = crate::testlock::serial();
    for scenario in 0..4 {
        let mut fixture = Fixture::new();
        let original = fixture.screen();
        let slot = if scenario == 2 { 35 } else { 17 };
        let mut items: Vec<_> = (0..36)
            .map(|i| fixture.listing.view().item(i).cloned())
            .collect();
        let expected = match scenario {
            0 | 3 => {
                items.swap(0, 17);
                if scenario == 0 {
                    0
                } else {
                    17
                }
            }
            1 => {
                items.remove(17);
                17
            }
            _ => {
                items.pop();
                34
            }
        };
        fixture.listing = crate::browse::view::ListingSnapshot::fixture(
            crate::plex::ServerId::from_raw(0),
            items,
            vec![("A".into(), 18), ("Z".into(), 18)],
        )
        .with_cursor(crate::browse::Cursor {
            at: crate::browse::CursorAt::ItemKey {
                sid: crate::plex::ServerId::from_raw(if scenario == 3 { 1 } else { 0 }),
                rk: if scenario == 2 { "36" } else { "18" }.into(),
                slot,
            },
            scroll: original.target_layout.row_reveal(slot / COLS),
        });
        let mut page = fixture.screen();
        let mut engine = FocusEngine::new();
        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        assert!(page.seed_cursor(
            &fixture.cx(None),
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present
            )
        ));
        for effect in output {
            if let Fx::Remember { group, elem } = effect.fx {
                engine.remember_projected(ENTRY, group, elem);
            }
        }
        let outcome = engine.enter(
            OWNER,
            &page,
            FocusTarget::ContainerGroup(page.pair.groups_config().detail),
            None,
            &fixture.cx(None),
        );
        let Outcome::Moved { from, to, by } = outcome else {
            panic!("bookmark must seed a real engine item")
        };
        page.step(
            &ScreenEvent::FocusMoved { from, to, by },
            &fixture.cx(Some(to)),
            &mut Effects::new(
                &mut Vec::new(),
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert_eq!(
            engine.current(OWNER),
            Some(page.key(page.pair.detail.elems[expected])),
            "scenario {scenario}"
        );
        let placed = page
            .place(&to.elem, &fixture.cx(Some(to)), At::SpringTarget)
            .unwrap();
        assert!(
            placed.clip.contains(placed.rect.cx(), placed.rect.cy()),
            "scenario {scenario}: restored stable item must be visible: {placed:?}"
        );
    }
}

#[test]
fn leaving_with_a_foreign_frame_snapshot_cannot_bookmark_that_section() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let grid = page.key(page.pair.detail.elems[5]);
    engine.set(
        OWNER,
        grid,
        Some(page.pair.groups_config().detail),
        By::Restore,
    );
    let original = fixture.listing.clone();
    for (sid, epoch, section) in [(0, 2, 1), (0, 1, 2), (7, 1, 1)] {
        fixture.listing = if sid == 0 {
            original.clone().with_section(epoch, section)
        } else {
            crate::browse::view::ListingSnapshot::fixture(
                crate::plex::ServerId::from_raw(sid),
                vec![None; 36],
                Vec::new(),
            )
            .with_section(epoch, section)
        };
        let mut cx = fixture.cx(Some(grid));
        cx.focus = engine.read(OWNER);
        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        page.step(
            &ScreenEvent::WillLeave(crate::ui::machine::Leave::ForGood),
            &cx,
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert!(
            output.is_empty(),
            "no old screen index may be interpreted against foreign store facts"
        );
    }
}

#[test]
fn live_engine_memory_wins_over_a_stale_store_bookmark_and_saves_from_toolbar() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    fixture.listing = fixture.listing.clone().with_cursor(crate::browse::Cursor {
        at: crate::browse::CursorAt::SlotIndex(17),
        scroll: 900.0,
    });
    let mut page = fixture.screen();
    let mut engine = FocusEngine::new();
    let grid = page.key(page.pair.detail.elems[5]);
    engine.set(
        OWNER,
        grid,
        Some(page.pair.groups_config().detail),
        By::Restore,
    );
    engine.set(OWNER, page.key(SORT), Some(TOOLBAR_GROUP), By::Restore);
    let mut cx = fixture.cx(engine.current(OWNER));
    cx.focus = engine.read(OWNER);
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    assert!(page.seed_cursor(
        &cx,
        &mut Effects::new(
            &mut output,
            MachineId::Instance(InstanceId(19)),
            &mut present
        )
    ));
    assert!(
        output.is_empty(),
        "a stale store bookmark cannot overwrite engine group memory"
    );
    page.save_cursor(
        &cx,
        &mut Effects::new(
            &mut output,
            MachineId::Instance(InstanceId(19)),
            &mut present,
        ),
    );
    assert!(output.iter().any(|effect| matches!(&effect.fx,
        Fx::App(AppFx::Store(_, crate::stores::StoreCmd::Browse(BrowseCmd::Addressed {
            work: LibraryWork::SaveCursor { cursor: crate::browse::Cursor {
                at: crate::browse::CursorAt::ItemKey { rk, slot: 5, .. }, ..
            }, .. }, ..
        }))) if rk == "6")));
}

#[test]
fn a_late_listing_keeps_its_bookmark_seed_pending_until_the_card_is_placeable() {
    let _guard = crate::testlock::serial();
    for fetch in [SecFetch::Loading, SecFetch::Failed] {
        let mut fixture = Fixture::new();
        let saved = crate::browse::Cursor {
            at: crate::browse::CursorAt::ItemKey {
                sid: crate::plex::ServerId::from_raw(0),
                rk: "18".into(),
                slot: 17,
            },
            scroll: 900.0,
        };
        let loaded = fixture.listing.clone().with_cursor(saved.clone());
        fixture.listing = crate::browse::view::ListingSnapshot::fixture(
            crate::plex::ServerId::from_raw(0),
            Vec::new(),
            Vec::new(),
        )
        .with_cursor(saved)
        .with_fetch(fetch, -1);
        // Two favorite rows make the document's first block available before its grid arrives.
        let mut sections = fixture.directory.view().sections().to_vec();
        sections.push(crate::browse::view::SectionView {
            borrowed: true,
            sid: Some(crate::plex::ServerId::from_raw(1)),
            key: 2,
            kind: SecKind::Movie,
            row: crate::browse::SrcRow {
                section: 1,
                title: "Shared".into(),
                pinned: true,
                ..Default::default()
            },
        });
        fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, sections);
        let mut page = fixture.screen();
        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        page.step(
            &ScreenEvent::Tick(Tick::default()),
            &fixture.cx(None),
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert!(
            page.initial,
            "a visible source row must not consume an unplaceable grid bookmark"
        );
        fixture.listing = loaded;
        output.clear();
        page.step(
            &ScreenEvent::Tick(Tick::default()),
            &fixture.cx(None),
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert!(!page.initial);
        assert!(output.iter().any(|effect| matches!(effect.fx, Fx::Remember { elem, .. } if elem == page.pair.detail.elems[17])));
    }
}

#[test]
fn switch_diagnostic_requests_type_sort_filter_and_rail_actions() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut sections = fixture.directory.view().sections().to_vec();
    sections.push(crate::browse::view::SectionView {
        borrowed: false,
        sid: Some(crate::plex::ServerId::from_raw(0)),
        key: 2,
        kind: SecKind::Show,
        row: crate::browse::SrcRow {
            section: 1,
            title: "Series".into(),
            pinned: true,
            ..Default::default()
        },
    });
    fixture.directory = crate::browse::view::DirectorySnapshot::fixture(1, 0, sections);
    let mut page = fixture.screen();
    let cx = fixture.cx(None);
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    page.command(
        LibraryCmd::SwitchStep(0),
        &cx,
        &mut Effects::new(
            &mut output,
            MachineId::Instance(InstanceId(19)),
            &mut present,
        ),
    );
    assert_eq!(page.wanted_kind, Some(SecKind::Show));
    page.command(
        LibraryCmd::SwitchStep(1),
        &cx,
        &mut Effects::new(
            &mut output,
            MachineId::Instance(InstanceId(19)),
            &mut present,
        ),
    );
    for (step, kind) in [(2, LibraryMenuKind::Sort), (7, LibraryMenuKind::Filter)] {
        output.clear();
        page.command(
            LibraryCmd::SwitchStep(step),
            &cx,
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert!(output.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Library(LibraryReq::Menu {kind: actual, ..})) if *actual == kind)));
    }
    for (step, index) in [(12, 18), (13, 0)] {
        output.clear();
        page.command(
            LibraryCmd::SwitchStep(step),
            &cx,
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert!(output.iter().any(
            |e| matches!(&e.fx, Fx::Remember {elem,..} if *elem == page.pair.detail.elems[index])
        ));
    }
}

#[test]
fn rail_keyboard_ok_and_back_return_the_exact_engine_remembered_item() {
    let _guard = crate::testlock::serial();
    for key in [Key::Ok, Key::Back] {
        let fixture = Fixture::new();
        let mut page = fixture.screen();
        let mut engine = FocusEngine::new();
        let grid = page.key(page.pair.detail.elems[17]);
        engine.set(
            OWNER,
            grid,
            Some(page.pair.groups_config().detail),
            By::Restore,
        );
        let mut links = Vec::new();
        <LibraryScreen as Screen<HostFixture>>::links(&page, &mut links);
        let Outcome::Moved { from, to, by } =
            engine.move_dir(OWNER, &page, &links, Dir::Right, &fixture.cx(Some(grid)))
        else {
            panic!("enter rail")
        };
        let mut output = Vec::new();
        let mut present = crate::ui::present::Present::new();
        page.step(
            &ScreenEvent::FocusMoved { from, to, by },
            &fixture.cx(Some(to)),
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(19)),
                &mut present,
            ),
        );
        assert!(output.iter().all(|e| !matches!(e.fx, Fx::Remember { .. })));
        output.clear();
        let event = ScreenEvent::Input(crate::ui::machine::InputEvent {
            at: Tick::default(),
            source: crate::ui::machine::Source::Script,
            kind: InputKind::Key {
                key,
                sym: 0,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        assert_eq!(
            page.step(
                &event,
                &fixture.cx(Some(to)),
                &mut Effects::new(
                    &mut output,
                    MachineId::Instance(InstanceId(19)),
                    &mut present
                )
            ),
            Handled::Yes
        );
        let target = output
            .into_iter()
            .find_map(|e| match e.fx {
                Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) => {
                    Some(focus)
                }
                _ => None,
            })
            .expect("rail keys request an engine seat, not document Home");
        engine.enter(OWNER, &page, target, None, &fixture.cx(Some(to)));
        assert_eq!(engine.current(OWNER), Some(grid));
    }
}

#[test]
fn rapid_filter_activations_invert_the_pending_desired_value() {
    let _guard = crate::testlock::serial();
    let fixture = Fixture::new();
    let mut menu = LibraryMenu::new(
        EntryId(99),
        LibraryMenuArg {
            host: InstanceId(19),
            kind: LibraryMenuKind::Filter,
            target: SectionAddress {
                epoch: 1,
                sid: crate::plex::ServerId::from_raw(0),
                section: 1,
            },
            anchor: [0; 4],
        },
    );
    let cx = fixture.cx(None);
    menu.refresh(&cx);
    let elem = menu
        .rows
        .iter()
        .find(|row| matches!(row.action, Action::Edit(QueryEdit::Unwatched(_))))
        .unwrap()
        .key;
    let mut output = Vec::new();
    let mut present = crate::ui::present::Present::new();
    for _ in 0..2 {
        menu.step(
            &ScreenEvent::Activate(elem),
            &cx,
            &mut Effects::new(
                &mut output,
                MachineId::Instance(InstanceId(99)),
                &mut present,
            ),
        );
        menu.refresh(&cx);
    }
    let desired: Vec<_> = output
        .iter()
        .filter_map(|effect| match &effect.fx {
            Fx::Deliver(
                _,
                Delivery::Screen(ScreenEvent::App(AppMsg::LibraryEdit {
                    edit: QueryEdit::Unwatched(value),
                    ..
                })),
            ) => Some(*value),
            _ => None,
        })
        .collect();
    assert_eq!(desired, vec![true, false]);
    let mut page = fixture.screen();
    for effect in output {
        if let Fx::Deliver(_, Delivery::Screen(ev)) = effect.fx {
            page.step(
                &ev,
                &cx,
                &mut Effects::new(
                    &mut Vec::new(),
                    MachineId::Instance(InstanceId(19)),
                    &mut present,
                ),
            );
        }
    }
    let mut commits = Vec::new();
    page.step(
        &ScreenEvent::WillLeave(crate::ui::machine::Leave::Deeper),
        &cx,
        &mut Effects::new(
            &mut commits,
            MachineId::Instance(InstanceId(19)),
            &mut present,
        ),
    );
    assert!(
        commits.is_empty(),
        "returning to committed filter must not requery"
    );
}
