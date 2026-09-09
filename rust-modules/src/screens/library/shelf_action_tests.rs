//! Activation ports: a deck promises playback; discovery, even an episode, does not.
use super::*;
use crate::ui::fixture::{FixtureArg, FixtureMeasure};
use crate::ui::focus::FocusEngine;
use crate::ui::machine::{Host, InputOwner, PressId, PressRead, Tick};

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

#[test]
fn shelf_activate_and_hold_keep_the_deck_promise_and_engine_item_identity() {
    let _guard = crate::testlock::serial();
    let session = crate::plex::session::TempSession::new("library-shelf-actions");
    session.watching("u-library-shelf-actions");
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(BrowseCmd::Reset);
        }
    }
    let _cleanup = Cleanup;
    crate::stores::browse::apply(BrowseCmd::Reset);
    crate::browse::seed_two_source_table_for_test();
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(12);
    crate::browse::section_hubs::seed_shelves_for_test(
        0,
        &["movie.inprogress.1", "tv.recentlyreleased.1"],
        3,
    );
    crate::browse::section_hubs::seed_landscape_for_test(0, "Synthetic show");
    directory.capture();
    let listing = crate::stores::browse::listing_snapshot();
    let hubs = crate::stores::browse::hubs_snapshot();
    let listing_id = listing.view().id().unwrap();
    let hubs_id = hubs.view().id().unwrap();
    assert_eq!(
        (listing_id.epoch, listing_id.sid, listing_id.section),
        (hubs_id.epoch, hubs_id.sid, hubs_id.section)
    );
    assert_eq!(hubs.view().shelves().len(), 2);
    let entry = EntryId(82);
    let owner = InputOwner::Entry(entry);
    let mut engine = FocusEngine::new();
    let cx = |engine: &FocusEngine<u32>| Cx::<TestHost> {
        views: Views {
            listing: listing.view(),
            directory: directory.view(),
            hubs: hubs.view(),
        },
        tick: Tick::default(),
        measure: &FixtureMeasure,
        focus: engine.read(owner),
        press: PressRead {
            scale: 0.85,
            is_long: true,
        },
        owner,
    };
    let mut page = LibraryScreen::new(entry, InstanceId(20), SecKind::Movie);
    page.sync(&cx(&engine));
    assert_eq!(page.shelves.len(), 2);
    for row in 0..2 {
        let item = &hubs.view().shelves()[row].items[1];
        assert_eq!(
            item.kind, 3,
            "discovery episode is the important non-play control"
        );
        assert!(hubs.view().shelves()[row].landscape);
        let from_deck = row == 0;
        assert_eq!(hubs.view().shelves()[row].is_continue, from_deck);
        let key = page.key(page.shelves[row].elems[1]);
        engine.set(owner, key, Some(page.shelves[row].group), By::Restore);
        for held in [false, true] {
            let mut out = Vec::new();
            let mut present = crate::ui::present::Present::new();
            let event = if held {
                ScreenEvent::PressHold(PressId(7))
            } else {
                ScreenEvent::Activate(key.elem)
            };
            let handled = page.step(
                &event,
                &cx(&engine),
                &mut Effects::new(&mut out, MachineId::Instance(InstanceId(20)), &mut present),
            );
            assert_eq!(handled, Handled::Yes);
            let requests: Vec<_> = out
                .into_iter()
                .filter_map(|effect| match effect.fx {
                    Fx::App(AppFx::Library(req)) => Some(req),
                    _ => None,
                })
                .collect();
            let expected = if held {
                LibraryReq::ItemMenu {
                    sid: item.sid,
                    rk: item.rk.clone(),
                    from_deck,
                }
            } else if from_deck {
                LibraryReq::Play {
                    sid: item.sid,
                    rk: item.rk.clone(),
                    resume_ns: 0,
                }
            } else {
                LibraryReq::Detail {
                    sid: item.sid,
                    rk: item.rk.clone(),
                }
            };
            assert_eq!(requests, vec![expected]);
            assert_eq!(engine.current(owner), Some(key));
        }
    }
}
