//! Ports of the legacy deferred-transaction assertions through the owned screen and store.
use super::*;
use crate::stores::browse::QueryEdit;
use crate::ui::fixture::{FixtureArg, FixtureMeasure};
use crate::ui::machine::{Host, InputOwner, Stamped, Tick};

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
struct Fixture {
    listing: crate::stores::browse::ListingSnapshot,
    directory: crate::stores::browse::DirectorySnapshot,
    hubs: crate::stores::browse::HubsSnapshot,
    sids: [crate::plex::ServerId; 2],
    _session: crate::plex::session::TempSession,
}
impl Fixture {
    fn new() -> Self {
        let session = crate::plex::session::TempSession::new("library-deferred-ports");
        session.watching("u-library-deferred-ports");
        crate::stores::browse::apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
        let own =
            crate::plex::register_for_test("deferred-own", "127.0.0.1", 9, "synthetic", "fixture");
        let shared = crate::plex::register_for_test(
            "deferred-shared",
            "127.0.0.1",
            10,
            "synthetic",
            "fixture",
        );
        let mut fixture = Self {
            listing: crate::stores::browse::listing_snapshot(),
            directory: Default::default(),
            hubs: crate::stores::browse::hubs_snapshot(),
            sids: [own, shared],
            _session: session,
        };
        fixture.rebuild();
        fixture
    }
    fn rebuild(&mut self) {
        crate::stores::browse::apply(BrowseCmd::Reset);
        crate::browse::seed_registered_table_for_test(self.sids);
        self.directory.capture();
        crate::stores::browse::apply(BrowseCmd::SetCur(0));
        crate::browse::seed_items_for_test(12);
        self.capture();
        assert_eq!(self.directory.view().current(), Some(0));
    }
    fn capture(&mut self) {
        self.directory.capture();
        self.listing = crate::stores::browse::listing_snapshot();
        self.hubs = crate::stores::browse::hubs_snapshot();
    }
    fn cx(&self) -> Cx<'_, TestHost> {
        Cx {
            views: Views {
                listing: self.listing.view(),
                directory: self.directory.view(),
                hubs: self.hubs.view(),
            },
            tick: Tick::default(),
            measure: &FixtureMeasure,
            focus: Default::default(),
            press: Default::default(),
            owner: InputOwner::Entry(EntryId(81)),
        }
    }
    fn screen(&self) -> LibraryScreen {
        let mut page = LibraryScreen::new(EntryId(81), InstanceId(19), SecKind::Movie);
        page.sync(&self.cx());
        page
    }
    fn address(&self, index: usize) -> SectionAddress {
        let view = self.directory.view();
        let section = &view.sections()[index];
        SectionAddress {
            epoch: view.epoch().unwrap(),
            sid: section.sid.unwrap(),
            section: section.key,
        }
    }
    fn step(
        &self,
        page: &mut LibraryScreen,
        event: ScreenEvent<TestHost>,
    ) -> Vec<Stamped<TestHost>> {
        let mut out = Vec::new();
        let mut present = crate::ui::present::Present::new();
        page.step(
            &event,
            &self.cx(),
            &mut Effects::new(&mut out, MachineId::Instance(InstanceId(19)), &mut present),
        );
        out
    }
    fn flush(&mut self, page: &mut LibraryScreen) -> Vec<bool> {
        let out = self.step(
            page,
            ScreenEvent::WillLeave(crate::ui::machine::Leave::Deeper),
        );
        let results = apply(out);
        self.capture();
        assert!(
            page.pending.section().is_none() && page.pending.grid().is_none(),
            "leaving drains both halves"
        );
        results
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        crate::stores::browse::apply(BrowseCmd::Reset);
        crate::plex::reset_servers_for_test();
    }
}
fn apply(out: Vec<Stamped<TestHost>>) -> Vec<bool> {
    // Pump the real synchronous StoreCmd drain; no asynchronous worker is needed to grade
    // selection/query acceptance. The retained fixture views refresh only after this drain.
    out.into_iter()
        .filter_map(|effect| match effect.fx {
            Fx::App(AppFx::Store(_, command)) => Some(crate::stores::apply(command)),
            _ => None,
        })
        .collect()
}
fn back() -> ScreenEvent<TestHost> {
    ScreenEvent::Input(crate::ui::machine::InputEvent {
        at: Tick::default(),
        source: crate::ui::machine::Source::Script,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    })
}

#[test]
fn newest_section_replaces_old_grid_work_and_commits_only_the_last_section() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    let original = page.toolbar_chip(FILTER, &fixture.cx()).value;
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibraryEdit {
            target: fixture.address(0),
            edit: QueryEdit::Unwatched(true),
        }),
    );
    assert_ne!(page.toolbar_chip(FILTER, &fixture.cx()).value, original);
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibrarySelect(fixture.address(2))),
    );
    assert!(
        page.pending.grid().is_none(),
        "the outgoing section's grid request must be discarded"
    );
    assert_eq!(page.toolbar_chip(FILTER, &fixture.cx()).value, original);
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibrarySelect(fixture.address(1))),
    );
    assert_eq!(
        page.view_section(&fixture.cx()),
        Some(1),
        "chrome acknowledges the newest press immediately"
    );
    assert!(page.page_fade.is_swapping());
    assert_eq!(fixture.flush(&mut page), vec![true]);
    assert_eq!(fixture.directory.view().current(), Some(1));
    assert!(
        !fixture.listing.view().unwatched(),
        "discarded grid work cannot alter the arriving query"
    );
}

#[test]
fn reselecting_the_incoming_section_preserves_its_own_grid_transaction() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    let incoming = fixture.address(2);
    fixture.step(&mut page, ScreenEvent::App(AppMsg::LibrarySelect(incoming)));
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibraryEdit {
            target: fixture.address(0),
            edit: QueryEdit::Unwatched(true),
        }),
    );
    fixture.step(&mut page, ScreenEvent::App(AppMsg::LibrarySelect(incoming)));
    assert!(
        page.pending.section().is_some() && page.pending.grid().is_some(),
        "replacement only discards work belonging to a different section"
    );
    assert_eq!(
        fixture.flush(&mut page),
        vec![true],
        "both halves commit in one addressed command"
    );
    assert_eq!(fixture.directory.view().current(), Some(2));
    assert!(fixture.listing.view().unwatched());
}

#[test]
fn a_target_that_moves_after_the_press_is_refused_at_the_store_drain() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    let old = fixture.address(0);
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibraryEdit {
            target: old,
            edit: QueryEdit::Unwatched(true),
        }),
    );
    crate::stores::browse::apply(BrowseCmd::SetCur(2));
    crate::browse::seed_items_for_test(12);
    fixture.capture();
    assert_eq!(fixture.directory.view().current(), Some(2));
    assert_eq!(
        old.epoch,
        fixture.address(2).epoch,
        "the target guard, not epoch refusal, must protect this case"
    );
    assert_ne!(old.sid, fixture.address(2).sid);
    let query = fixture.listing.view().id().unwrap().query;
    assert_eq!(fixture.flush(&mut page), vec![false]);
    assert_eq!(fixture.directory.view().current(), Some(2));
    assert_eq!(fixture.listing.view().id().unwrap().query, query);
    assert!(!fixture.listing.view().unwatched());
}

#[test]
fn queued_section_refuses_a_renumbered_table_after_a_positive_control() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibrarySelect(fixture.address(2))),
    );
    assert_eq!(fixture.flush(&mut page), vec![true]);
    assert_eq!(fixture.directory.view().current(), Some(2));
    fixture.rebuild();
    let mut page = fixture.screen();
    let old = fixture.address(2);
    fixture.step(&mut page, ScreenEvent::App(AppMsg::LibrarySelect(old)));
    assert!(page.pending.section().is_some());
    fixture.rebuild();
    assert_ne!(fixture.address(2).epoch, old.epoch);
    assert_eq!(fixture.flush(&mut page), vec![false]);
    assert_eq!(
        fixture.directory.view().current(),
        Some(0),
        "old indices cannot select a new profile's library"
    );
}

#[test]
fn foreign_grid_targets_neither_relabel_chips_nor_mutate_the_current_query() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    for index in [1, 2] {
        let mut page = fixture.screen();
        let before = page.toolbar_chip(FILTER, &fixture.cx()).value;
        let query = fixture.listing.view().id().unwrap().query;
        fixture.step(
            &mut page,
            ScreenEvent::App(AppMsg::LibraryEdit {
                target: fixture.address(index),
                edit: QueryEdit::Unwatched(true),
            }),
        );
        assert_eq!(page.toolbar_chip(FILTER, &fixture.cx()).value, before);
        assert_eq!(fixture.flush(&mut page), vec![false]);
        assert!(!fixture.listing.view().unwatched());
        assert_eq!(fixture.listing.view().id().unwrap().query, query);
        assert_eq!(fixture.directory.view().current(), Some(0));
    }
}

#[test]
fn queued_grid_epoch_is_refused_even_when_the_section_identity_still_matches() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibraryEdit {
            target: fixture.address(0),
            edit: QueryEdit::Unwatched(true),
        }),
    );
    assert_eq!(fixture.flush(&mut page), vec![true]);
    assert!(
        fixture.listing.view().unwatched(),
        "positive control must reach the real query"
    );
    fixture.rebuild();
    let mut page = fixture.screen();
    let old = fixture.address(0);
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibraryEdit {
            target: old,
            edit: QueryEdit::Unwatched(true),
        }),
    );
    fixture.rebuild();
    assert_eq!(
        (old.sid, old.section),
        (fixture.address(0).sid, fixture.address(0).section)
    );
    assert_ne!(old.epoch, fixture.address(0).epoch);
    assert_eq!(fixture.flush(&mut page), vec![false]);
    assert!(!fixture.listing.view().unwatched());
}

#[test]
fn back_before_commit_cancels_both_halves_and_their_outgoing_fades() {
    let _guard = crate::testlock::serial();
    let mut fixture = Fixture::new();
    let mut page = fixture.screen();
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibrarySelect(fixture.address(2))),
    );
    fixture.step(
        &mut page,
        ScreenEvent::App(AppMsg::LibraryEdit {
            target: fixture.address(0),
            edit: QueryEdit::Unwatched(true),
        }),
    );
    assert!(page.pending.section().is_some() && page.pending.grid().is_some());
    assert!(page.page_fade.is_swapping() && page.grid_fade.is_swapping());
    let out = fixture.step(&mut page, back());
    assert!(
        out.is_empty(),
        "BACK cancels here, rather than requesting Home"
    );
    assert!(page.pending.section().is_none() && page.pending.grid().is_none());
    assert!(
        !page.page_fade.cancel() && !page.grid_fade.cancel(),
        "the outgoing phases have already reversed"
    );
    for frame in 1..40 {
        let out = fixture.step(
            &mut page,
            ScreenEvent::Tick(Tick {
                ms: frame * 16,
                dt_us: 16_000,
            }),
        );
        assert!(
            !out.iter().any(|effect| matches!(
                &effect.fx,
                Fx::App(AppFx::Store(
                    _,
                    StoreCmd::Browse(BrowseCmd::Addressed {
                        work: LibraryWork::Commit { .. },
                        ..
                    })
                ))
            )),
            "the return animation must not commit cancelled work"
        );
    }
    assert!(!page.page_fade.is_swapping() && !page.grid_fade.is_swapping());
    assert!(fixture.flush(&mut page).is_empty());
    assert_eq!(fixture.directory.view().current(), Some(0));
    assert!(!fixture.listing.view().unwatched());
}
