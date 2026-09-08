//! Foreign table replacement must remount incoming content, not inherit the outgoing cursor.
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
const ENTRY: EntryId = EntryId(83);
const INSTANCE: InstanceId = InstanceId(21);
const OWNER: InputOwner = InputOwner::Entry(ENTRY);

struct Publication {
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    hubs: crate::stores::browse::HubsSnapshot,
}
impl Publication {
    fn replace(sids: [crate::plex::ServerId; 2], current: usize) -> Self {
        crate::stores::browse::apply(BrowseCmd::Reset);
        crate::browse::seed_registered_table_for_test(sids);
        let mut directory = crate::stores::browse::DirectorySnapshot::default();
        directory.capture(); // Resolve profile pins before choosing the intended section.
        crate::stores::browse::apply(BrowseCmd::ApplyPins(vec![(0, true), (2, true)]));
        crate::stores::browse::apply(BrowseCmd::SetCur(current));
        crate::browse::seed_items_for_test(120);
        directory.capture();
        let listing = crate::stores::browse::listing_snapshot();
        let hubs = crate::stores::browse::hubs_snapshot();
        let id = listing.view().id().unwrap();
        let hub_id = hubs.view().id().unwrap();
        assert_eq!(directory.view().current(), Some(current));
        assert_eq!(directory.view().epoch(), Some(id.epoch));
        assert_eq!(
            (id.epoch, id.sid, id.section),
            (hub_id.epoch, hub_id.sid, hub_id.section)
        );
        assert_eq!(directory.view().sections()[current].sid, Some(id.sid));
        assert_eq!(listing.view().total(), 120);
        Self {
            listing,
            directory,
            hubs,
        }
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
    // Route the screen's queued Enter/Remember effects through the actual engine. No test cursor.
    fn deliver(
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
                &mut Effects::new(&mut out, MachineId::Instance(INSTANCE), &mut present),
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
    fn frame(&self, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>, n: u32) -> Vec<AppFx> {
        let mut effects = self.deliver(
            page,
            engine,
            ScreenEvent::Tick(Tick {
                ms: n * 16,
                dt_us: 16_000,
            }),
        );
        if let Outcome::Moved { from, to, by } = engine.reconcile(OWNER, page, &self.cx(engine)) {
            effects.extend(self.deliver(page, engine, ScreenEvent::FocusMoved { from, to, by }));
        }
        effects
    }
    fn down(&self, page: &mut LibraryScreen, engine: &mut FocusEngine<u32>) {
        let mut links = Vec::new();
        <LibraryScreen as Screen<TestHost>>::links(page, &mut links);
        let outcome = engine.move_dir(OWNER, page, &links, Dir::Down, &self.cx(engine));
        let Outcome::Moved { from, to, by } = outcome else {
            panic!("fixture Down must move");
        };
        self.deliver(page, engine, ScreenEvent::FocusMoved { from, to, by });
    }
}

#[test]
fn foreign_table_replacement_during_grid_query_mounts_incoming_engine_focus_and_viewport_once() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-foreign-replacement");
    session.watching("u-library-foreign-replacement");
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    crate::stores::browse::apply(BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let own = crate::plex::register_for_test("foreign-own", "127.0.0.1", 9, "synthetic", "fixture");
    let shared =
        crate::plex::register_for_test("foreign-shared", "127.0.0.1", 10, "synthetic", "fixture");
    for incoming_index in [0, 2] {
        let outgoing = Publication::replace([own, shared], 0);
        let old_id = outgoing.listing.view().id().unwrap();
        let mut engine = FocusEngine::new();
        let mut page = LibraryScreen::new(ENTRY, INSTANCE, SecKind::Movie);
        outgoing.deliver(&mut page, &mut engine, ScreenEvent::Mount);
        let first_group = page.first_group();
        outgoing.deliver(
            &mut page,
            &mut engine,
            ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::ContainerGroup(first_group),
            }),
        );
        for n in 0..80 {
            outgoing.frame(&mut page, &mut engine, n);
        }
        let old_grid = page.pair.groups_config().detail;
        let deep = page.key(page.pair.detail.elem_at(9 * COLS + 3).unwrap());
        outgoing.deliver(
            &mut page,
            &mut engine,
            ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(deep),
            }),
        );
        for n in 80..160 {
            outgoing.frame(&mut page, &mut engine, n);
        }
        assert_eq!(page.grid_position(engine.current(OWNER)), Some((9, 3)));
        let old_scroll = page.scroll.pos;
        assert!(old_scroll > SCR_H);
        assert!((old_scroll - page.layout.row_reveal(9)).abs() < 0.01);
        assert_eq!(engine.read(OWNER).remembered(old_grid), Some(deep.elem));
        outgoing.deliver(
            &mut page,
            &mut engine,
            ScreenEvent::App(AppMsg::LibraryEdit {
                target: SectionAddress {
                    epoch: old_id.epoch,
                    sid: old_id.sid,
                    section: old_id.section,
                },
                edit: crate::stores::browse::QueryEdit::Unwatched(true),
            }),
        );
        assert!(page.grid_fade.is_swapping());
        assert!(!page.page_fade.is_swapping());
        assert!(page.pending.grid().is_some());

        let incoming = Publication::replace([own, shared], incoming_index);
        let id = incoming.listing.view().id().unwrap();
        assert_ne!(id.epoch, old_id.epoch);
        assert_eq!(id.sid, if incoming_index == 0 { own } else { shared });
        assert_eq!(
            outgoing.listing.view().id().unwrap(),
            old_id,
            "old frame stays retained, not replayed from the new globals"
        );
        assert_eq!(outgoing.listing.view().total(), 120);
        incoming.deliver(
            &mut page,
            &mut engine,
            ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
        );
        assert_eq!(page.epoch, Some(id.epoch));
        assert_eq!(
            page.section,
            Some(LibrarySectionIdentity {
                sid: id.sid,
                key: id.section
            })
        );
        let new_grid = page.pair.groups_config().detail;
        assert_ne!(
            new_grid, old_grid,
            "same section after reset still has a new epoch-owned group"
        );
        assert!(page.page_fade.is_swapping());
        assert_eq!(page.page_fade.alpha(), 0.0);
        assert!(!page.grid_fade.is_swapping());
        assert!(page.pending.grid().is_none());
        assert_eq!(page.scroll.pos, 0.0);
        assert_eq!(page.scroll_target, 0.0);
        let first_effects = incoming.frame(&mut page, &mut engine, 160);
        assert!(!first_effects.iter().any(|fx| matches!(
            fx,
            AppFx::Store(
                _,
                StoreCmd::Browse(BrowseCmd::Addressed {
                    work: LibraryWork::Commit { .. },
                    ..
                })
            )
        )));
        let first = engine.current(OWNER).unwrap();
        let selected = page
            .libraries
            .iter()
            .find(|(_, index)| *index == incoming_index)
            .unwrap()
            .0;
        assert_eq!(
            first,
            page.key(selected),
            "first incoming focus is its selected library, not the outgoing deep card"
        );
        assert_eq!(page.scroll.pos, 0.0);
        assert_eq!(
            engine.read(OWNER).remembered(old_grid),
            Some(deep.elem),
            "old history remains engine-owned, but cannot seat the incoming epoch"
        );
        assert_eq!(engine.read(OWNER).remembered(new_grid), None);

        // Mount's first ready tick changes Hold to In; the following tick advances alpha.
        incoming.frame(&mut page, &mut engine, 161);
        let alpha = page.page_fade.alpha();
        assert!(alpha > 0.0);
        incoming.deliver(
            &mut page,
            &mut engine,
            ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
        );
        assert_eq!(
            page.page_fade.alpha(),
            alpha,
            "same publication cannot remount at zero again"
        );
        assert!(!page.initial);
        incoming.frame(&mut page, &mut engine, 162);
        assert!(page.page_fade.alpha() > alpha);
        assert_eq!(engine.current(OWNER), Some(first));
        assert_eq!(page.scroll.pos, 0.0);
        for n in 163..240 {
            incoming.frame(&mut page, &mut engine, n);
        }
        incoming.down(&mut page, &mut engine); // selected library -> toolbar
        assert_eq!(engine.current_group(OWNER), Some(TOOLBAR_GROUP));
        incoming.down(&mut page, &mut engine); // toolbar -> incoming remembered grid (empty history)
        assert_eq!(page.grid_position(engine.current(OWNER)), Some((0, 0)));
        let key = engine.current(OWNER).unwrap();
        let item = page.focused_item(Some(key), &incoming.cx(&engine)).unwrap();
        let expected = incoming.listing.view().item(0).unwrap();
        assert_eq!(
            (item.sid, item.rk.as_str()),
            (expected.sid, expected.rk.as_str())
        );
        assert_eq!(item.sid, id.sid);
        assert_eq!(engine.read(OWNER).remembered(new_grid), Some(key.elem));
        for n in 240..320 {
            incoming.frame(&mut page, &mut engine, n);
        }
        assert!((page.scroll.pos - page.layout.row_reveal(0)).abs() < 0.01);
        assert!(page.scroll.pos < old_scroll);
        incoming.down(&mut page, &mut engine);
        for n in 320..400 {
            incoming.frame(&mut page, &mut engine, n);
        }
        let moved = engine.current(OWNER);
        let moved_scroll = page.scroll.pos;
        assert_eq!(page.grid_position(moved), Some((1, 0)));
        incoming.deliver(
            &mut page,
            &mut engine,
            ScreenEvent::StoreChanged(StoreId::Browse.ord(), 1),
        );
        incoming.frame(&mut page, &mut engine, 400);
        assert_eq!(
            engine.current(OWNER),
            moved,
            "quiet publication must not reseat a viewer who moved"
        );
        assert!((page.scroll.pos - moved_scroll).abs() < 0.01);
        assert!(!page.page_fade.is_swapping() && !page.grid_fade.is_swapping());
    }
}
