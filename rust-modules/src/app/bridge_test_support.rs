//! Shared driver helpers for bridge.rs's split test modules: frame/tick drivers, route
//! argument builders, and the directory-policy fixture used by the ViewState/chrome tests.

use super::*;

/// **Put `route` on top, then run one frame** — the TEST driver that replaced `sync_page`.
///
/// It is deliberately the same three-line decision `sync_page` made every frame in production
/// (reuse an entry that is already on the stack, otherwise root a peer or stack a page), kept
/// here so that ~240 existing assertions still read as "drive a frame on this screen". What
/// changed is WHO says it: production asks for the op it wants at the press, and no frame
/// derives a navigation from a second copy of the route.
pub(super) fn goto(d: &mut Dispatcher<AppHost>, want: AppArg) {
    if d.has_pending_navigation() { return; }
    super::show_page(d, want);
}

pub(super) fn frame(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick, inputs: Vec<InputEvent<u32>>) -> (&'static str, FrameReport) {
    goto(d, route);
    super::frame(d, rig, tick, inputs)
}

/// …and the same driver for the two frames that also want the effect tap or a supplied result
/// set. Only the `goto` is the test's: everything after it is production's own frame.
pub(super) fn frame_with_tap(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, tap: &mut dyn crate::ui::dispatch::Tap<AppHost>)
    -> (&'static str, FrameReport) {
    goto(d, route);
    super::frame_with_tap(d, rig, tick, inputs, tap)
}

pub(super) fn frame_with_results(d: &mut Dispatcher<AppHost>, rig: &mut Bridge, route: AppArg, tick: Tick,
    inputs: Vec<InputEvent<u32>>, take: impl FnOnce() -> AppResults,
    tap: &mut dyn crate::ui::dispatch::Tap<AppHost>) -> (&'static str, FrameReport) {
    goto(d, route);
    super::frame_with_results(d, rig, tick, inputs, take, tap)
}

/// A detail page's argument, for the tests that used to name `Route::Detail` and let a trail
/// supply the identity. The fold puts the identity ON the argument, which is what makes the
/// trail seeding this helper used to do unnecessary.
pub(super) fn detail_arg(rk: &str) -> AppArg {
    AppArg::Content(ContentArg::Detail { sid: crate::plex::ServerId::UNSET, rk: rk.into() })
}

/// …and a person page's.
pub(super) fn person_arg(key: &str) -> AppArg {
    AppArg::Content(ContentArg::Person {
        sid: crate::plex::ServerId::UNSET,
        key: key.into(),
        guid: format!("tag://{key}"),
        name: String::new(),
        thumb: String::new(),
    })
}


pub(super) fn tick(i: u32) -> Tick {
    Tick {
        ms: i * 16,
        dt_us: 16_000,
    }
}

pub(super) use super::super::words::every_route;

pub(super) fn notices(d: &Dispatcher<AppHost>) -> String {
    let mut s = String::new();
    if let Some(sc) = d.top_screen() {
        sc.state().probe(&mut s);
    }
    s
}

pub(super) fn directory_policy_fixture(
    own: crate::plex::ServerId,
    hidden: crate::plex::ServerId,
) -> crate::stores::browse::DirectorySnapshot {
    let section = |sid, key, section, title: &str, pinned| {
        crate::stores::browse::SectionView {
            borrowed: false,
            sid: Some(sid),
            key,
            kind: crate::stores::browse::SecKind::Movie,
            row: crate::stores::browse::SrcRow {
                section,
                title: title.into(),
                pinned,
                current: section == 0,
                ..Default::default()
            },
        }
    };
    crate::stores::browse::DirectorySnapshot::fixture(41, 0, vec![
        section(own, 1, 0, "Retained Movies", true),
        section(hidden, 2, 1, "Hidden Movies", false),
    ])
}

pub(super) struct DirectoryPolicyCleanup;

impl Drop for DirectoryPolicyCleanup {
    fn drop(&mut self) {
        // Search and Hubs are now owned per-Bridge (`rig`, dropped with the test's own stack
        // frame), so there is no process-wide store state left for this cleanup to reset.
        crate::plex::reset_servers_for_test();
    }
}
