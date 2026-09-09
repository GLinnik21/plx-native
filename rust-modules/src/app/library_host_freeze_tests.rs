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
    frame(&mut d, &mut rig, Route::Library, tick(0), vec![]);
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
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
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
        frame(&mut d, &mut rig, Route::Library, tick(i), vec![]);
    }
    assert!(
        d.nav.modals.surfaces.is_empty(),
        "the menu finished fading out"
    );
    assert_eq!(users(), 0, "and gave the page back to the live path");
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
