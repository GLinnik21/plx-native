/// **The Library's Sort/Filter menu freezes the page under it, as its legacy `Popover` did.**
///
/// The reproduction of the `fps:library-switch` regression (restructure phase 8, device-measured
/// `loop=` 43-45 against a floor of 45, sustained 58 ms frames with `draw=57`). `sync_host` listed
/// the host-caching styles by hand and left `Style::Compact` out, so the one production Compact
/// surface — this menu — took no `HOST_USERS` share, `popover::host::page_pass` short-circuited on
/// `users() == 0`, and the whole library page (ambient wash, shelves, poster grid, every string on
/// it) was re-rendered on every frame the menu was open. Eight of the scene's fourteen steps hold
/// one open.
///
/// It grades the COUNTER rather than a frame time, because no host test draws a pixel: the counter
/// is the single bit that arms the freeze, and `modal::surface_policy` had said `HostRender::Cached`
/// for Compact all along.
#[test]
fn a_compact_library_menu_holds_a_frozen_host_and_gives_it_back_on_dismissal() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-host-freeze");
    session.watching("u-library-host-freeze");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("freeze-own", "127.0.0.1", 9, "synthetic", "fixture");
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, sid]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);

    let base = crate::ui::popover::host_users_for_test();
    let users = || crate::ui::popover::host_users_for_test() - base;

    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
    assert_eq!(users(), 0, "a bare library page freezes nothing");
    let page = d.nav.top_page().unwrap().id;
    let host = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    let listing = rig.listing.view().id().unwrap();
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(
            crate::screens::registry::LibraryMenuArg {
                host,
                kind: crate::screens::registry::LibraryMenuKind::Sort,
                anchor: [0; 4],
                target: crate::stores::browse::SectionAddress {
                    epoch: listing.epoch,
                    sid: listing.sid,
                    section: listing.section,
                },
            },
        )),
    );
    for i in 1..40 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let menu = d
        .nav
        .modals
        .surfaces
        .first()
        .expect("the menu is up")
        .entry
        .id;
    assert_ne!(menu, page, "the menu is its own entry");
    assert_eq!(
        users(),
        1,
        "an open Compact menu holds the one host snapshot its style's HostRender::Cached promises"
    );

    d.request(MachineId::Nav, NavOp::Dismiss(menu));
    for i in 40..120 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    assert!(
        d.nav.modals.surfaces.is_empty(),
        "the menu finished fading out"
    );
    assert_eq!(users(), 0, "and gave the page back to the live path");
}

/// **A page that MOVES under an open panel is the HOST moving** (spec §4.4, §15.1) — the sibling
/// of `a_modal_foreground_spring_does_not_invalidate_the_host_snapshot`, which grades the other
/// direction and passed for the wrong reason while this one failed.
///
/// The reproduction, measured against the broken build (14 consecutive frames):
/// `moving=true page_moving=false` — the Library's own scroll spring in flight while the Sort menu
/// fades out, with `idle::page_moving()` reading FALSE for every one of them.
/// `frame_with_results` wrapped the WHOLE dispatcher frame in `popover::own_motion` while any
/// surface was up, so every spring an owned page stepped inside that frame was attributed to the
/// panel. `surface_up()` counts a `Closing` surface, and a `Closing` surface has already handed
/// input back to the page — which is exactly the state `popover::host_refresh`'s `fading_only`
/// term exists for. So the one signal that re-takes the frozen snapshot for a page the user is
/// driving again was suppressed by construction, and the panel's fade showed a stale picture of a
/// scrolling page.
///
/// **It has to be the LIBRARY, and it has to be `idle`.** The dispatcher keeps a motion ledger of
/// its own (`Present::page_moving`, reported as `FrameReport::underlay_moving`), but a screen only
/// reaches it by calling `fx.note(PresentEvent::Motion)` — which `screens::library` never does. It
/// animates through `crate::ui::Spring`, i.e. `gfx::spring`, which reports to `ui::idle` and
/// nowhere else; `report.underlay_moving` is false on every frame below. `idle::page_moving()` is
/// the only witness there is.
#[test]
fn a_host_page_spring_under_an_open_panel_is_host_motion() {
    let _guard = crate::testlock::serial();
    struct Cleanup;
    impl Drop for Cleanup {
        fn drop(&mut self) {
            crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
            crate::plex::reset_servers_for_test();
        }
    }
    let _cleanup = Cleanup;
    let session = crate::plex::session::TempSession::new("library-host-motion");
    session.watching("u-library-host-motion");
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::Reset);
    crate::plex::reset_servers_for_test();
    let sid = crate::plex::register_for_test("motion-own", "127.0.0.1", 9, "synthetic", "fixture");
    crate::plex::set_current(sid);
    crate::browse::seed_registered_table_for_test([sid, sid]);
    let mut directory = crate::stores::browse::DirectorySnapshot::default();
    directory.capture();
    crate::stores::browse::apply(crate::stores::browse::BrowseCmd::SetCur(0));
    crate::browse::seed_items_for_test(120);

    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    frame(&mut d, &mut rig, AppArg::Library, tick(0), vec![]);
    d.nav.tabs.stack.transition = Box::new(crate::ui::containers::transition::Immediate);
    // park the grid focus deep, and let every boot spring settle
    Bridge::library_command(&mut d, crate::screens::registry::LibraryCmd::FocusGrid { row: 8, col: 4 });
    for i in 1..80 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let host = d.nav.top_page().unwrap().inst.as_ref().unwrap().id;
    let listing = rig.listing.view().id().unwrap();
    d.nav.next_style = crate::ui::containers::modal::Style::Compact;
    d.request(
        MachineId::Nav,
        NavOp::Present(AppArg::LibraryMenu(crate::screens::registry::LibraryMenuArg {
            host,
            kind: crate::screens::registry::LibraryMenuKind::Sort,
            anchor: [0; 4],
            target: crate::stores::browse::SectionAddress {
                epoch: listing.epoch,
                sid: listing.sid,
                section: listing.section,
            },
        })),
    );
    for i in 80..120u32 {
        frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
    }
    let menu = d.nav.modals.surfaces.first().expect("the menu is up").entry.id;

    // a settled panel over a settled page: nothing moves, and in particular the panel's own
    // appear spring (`ModalStack::tick`, now in a scope of its own) is not the page's motion
    crate::ui::idle::frame_begin(1.0 / 60.0);
    frame(&mut d, &mut rig, AppArg::Library, tick(120), vec![]);
    assert!(!crate::ui::idle::page_moving(), "a settled host does not move");

    // dismiss: input returns to the page while the panel is still visible, and the page is driven
    d.request(MachineId::Nav, NavOp::Dismiss(menu));
    frame(&mut d, &mut rig, AppArg::Library, tick(121), vec![]);
    Bridge::library_command(&mut d, crate::screens::registry::LibraryCmd::FocusGrid { row: 0, col: 0 });
    let mut moved = 0;
    for i in 122..136u32 {
        crate::ui::idle::frame_begin(1.0 / 60.0);
        let (_, report) = frame(&mut d, &mut rig, AppArg::Library, tick(i), vec![]);
        if !crate::ui::idle::present_moving() {
            continue;
        }
        moved += 1;
        assert!(
            d.surface_up(),
            "frame {i}: the reproduction needs the panel still up (fading) while the page moves"
        );
        assert!(
            crate::ui::idle::page_moving(),
            "frame {i}: the page's own scroll spring is in flight and nothing else is — \
             it is the HOST that is moving (report.underlay_moving={})",
            report.underlay_moving
        );
        assert!(
            crate::ui::popover::host_refresh(true, false, crate::ui::idle::page_moving()),
            "frame {i}: …so the fading panel's frozen snapshot is re-taken"
        );
    }
    assert!(moved >= 5, "the scroll spring travelled for {moved} frames — not a reproduction");
}

/// The other half, at the seam the bug was actually in: the bridge's counter answer is DERIVED
/// from the container's own policy table, so the two cannot state different things again.
#[test]
fn every_style_caches_its_host_exactly_when_the_policy_table_says_cached() {
    use crate::ui::containers::modal::{style_caches_host, surface_policy, HostRender, Phase, Style};
    for style in [
        Style::Compact,
        Style::Sheet,
        Style::Alert,
        Style::Opaque { snapshot: true },
        Style::Opaque { snapshot: false },
        Style::PlayerPanel {
            survives_failure: true,
        },
    ] {
        assert_eq!(
            style_caches_host(style),
            surface_policy(style, Phase::Opening, false).1 != HostRender::Live,
            "{style:?} states one host policy, not two"
        );
    }
    assert!(
        style_caches_host(Style::Compact),
        "Compact is HostRender::Cached — the Library menu and item_menu both freeze their page"
    );
    assert!(!style_caches_host(Style::PlayerPanel {
        survives_failure: true
    }),
        "GL cannot read the punch-through alpha over the video plane, so no player panel caches"
    );
}
