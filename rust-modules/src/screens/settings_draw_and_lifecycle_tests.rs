//! Scrim/entrance alpha composition, nested draw across a push/pop, and the mount-time
//! session-write regression.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// Spec §14 phase 8: `Family::Settings`'s scrim/entrance composition reads
/// `DrawFrame::nav_page_alpha` rather than the `ui::nav` statics — these two pin the
/// arithmetic itself (the wiring at the two call sites is a straight field read, checked by
/// the compiler and by every existing draw test in this module staying green). A route dip
/// in flight (a non-1.0 `nav_page_alpha`) must dim the scrim and the entrance exactly as
/// much as the surface's own appear does — before this field existed, both call sites read
/// the live global instead of whatever a host test's `DrawFrame` carried, so a test built on
/// the OLD shape could not have told a wired composition from an ignored parameter; a
/// process-wide static is either at rest (1.0, indistinguishable from the identity) or being
/// driven by a second test racing this one (`testlock::serial()`'s whole reason for existing
/// — see `docs/../test-suite-global-pollution.md`), never a controlled non-1.0 value a test
/// can set.
#[test]
fn settings_scrim_and_entrance_alpha_compose_local_and_nav_page_alpha() {
    assert_eq!(settings_scrim_alpha(1.0, 1.0), SCRIM_A);
    assert_eq!(settings_entrance_alpha(1.0, 1.0), 1.0);
    // the surface is fully open (local 1.0) but the route beneath it is mid-dip (0.5): both
    // the scrim and the entrance must read the dip, not just the surface's own appear.
    assert_eq!(settings_scrim_alpha(1.0, 0.5), SCRIM_A * 0.5);
    assert_eq!(settings_entrance_alpha(1.0, 0.5), 0.5);
    // the surface is itself still appearing (local 0.5) over a route at rest (1.0).
    assert_eq!(settings_scrim_alpha(0.5, 1.0), SCRIM_A * 0.5);
    assert_eq!(settings_entrance_alpha(0.5, 1.0), 0.5);
    // both in flight at once multiply, never clamp or pick a max.
    assert_eq!(settings_scrim_alpha(0.5, 0.4), SCRIM_A * 0.2);
    assert_eq!(settings_entrance_alpha(0.5, 0.4), 0.2);
}

#[test]
fn nested_draw_preserves_navigation_at_rest_and_through_push_and_pop() {
    use crate::ui::containers::stack::Entry;
    use crate::ui::screen::NavPresentation;
    use std::{cell::RefCell, rc::Rc};

    let navigation = NavPresentation {
        page_alpha: 0.21, chrome_alpha: 0.37, view_tab: Some(2), blur_amount: 0.63,
    };
    let cases: &[(&str, f32, f32, bool, &[u32])] = &[
        ("rest after pop", 0.0, 0.0, false, &[2]),
        ("rest after push", 1.0, 1.0, false, &[2]),
        ("mid-push", 0.5, 1.0, false, &[1, 2]),
        ("mid-pop", 0.5, 0.0, true, &[2, 3]),
    ];
    let mut actual = Vec::new();
    let mut expected = Vec::new();
    for &(label, pos, target, popping, order) in cases {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let instance = |id| Instance {
            id: InstanceId(id),
            screen: Box::new(DrawProbe { id, seen: Rc::clone(&seen) }) as Box<dyn Screen<InnerHost>>,
            inflight: Vec::new(),
        };
        // Install inert bodies directly: no real page construction, auth/session reads or
        // lifecycle side effects. Both resting cases retain a page underneath the top.
        let mut inner = NavStack::new(Box::new(Immediate));
        for id in [1, 2] {
            inner.entries.push(Entry {
                id: EntryId(id), arg: SettingsPage::Root, ret: ReturnState::default(),
                inst: Some(instance(id)), evicted: false,
            });
        }
        let mut surface = RouteSurface {
            entry: EntryId(0), id: InstanceId(0), kind: Family::Settings,
            inner, ids: Minter::default(),
            push: Push { pos, vel: 0.0, target, leaving: popping.then(|| instance(3)) },
            ground: RouteGround::new(), ground_ready: false, remembered: Vec::new(),
        };
        let outer = cx(None);
        let c = inner_cx(&outer);
        let mut f = DrawFrame::with_navigation(&c, Painter::root(), navigation);
        let before = LogicalState::hash(&surface);
        surface.draw_pages(&mut f, Painter::root());
        assert_eq!(LogicalState::hash(&surface), before, "{label}: draw changed logical state");
        actual.push((label, seen.borrow().clone()));
        expected.push((label, order.iter().map(|&id| (id, navigation)).collect::<Vec<_>>()));
    }
    assert_eq!(actual, expected, "every nested draw path must inherit the outer snapshot");
}

/// **OPENING SETTINGS MUST NOT WRITE THE SESSION FILE.** Device-measured, 2026-09-09:
/// `fps:modal-ramp` (open and dismiss the Settings modal every 1500 ms) read a `worstframe`
/// of 188-210 ms against a 75 ms ceiling, with `FRAMEDROP` putting 150-181 ms of it in
/// `navcommit=` — the dispatcher's POST-COMMIT DRAIN, which is where this surface's mount and
/// its root page's `ScreenEvent::Enter` run.
///
/// [`RootPage::rebuild`] asks [`signed_in`] whether this television has an account, once at
/// construction and again on `Enter`, so TWICE per open. That question used to go through
/// [`crate::plex::session::load`] — the read-modify-WRITE door, whose own doc says a read that
/// can turn into a save "is not [an acceptable trade] on a path a keypress can reach", and
/// "do not add a per-frame reader of this file". On this television the key manager is
/// unusable ("session protection: no usable key manager; using the 0600 file fallback"), so
/// every `load` takes the plaintext branch and re-persists: `write_atomic`, i.e. a temp file,
/// `sync_all`, a rename and a second `sync_all` on the directory. Two flash writes with four
/// fsyncs, synchronously, on the frame that opens the modal. Instrumented on the set the same
/// day: 23 `load`s in one 18 s run, `saves=1 plaintext=1` on every one, 6-15 ms each in that
/// session and ~75 ms each in the sessions that failed — which is also why the symptom is
/// BIMODAL, and why it bisected to a range containing no functional change at all.
///
/// The assertion is the file's INODE, not its mtime: `write_atomic` renames a fresh temp file
/// into place, so a write always moves it, whatever a filesystem's timestamp resolution.
/// Watched red against `session::load()` — the inode changed on the mount.
#[test]
fn opening_settings_never_writes_the_session_file() {
    use std::os::unix::fs::MetadataExt;
    let _g = crate::testlock::serial();
    let sess = scratch_session("surface-no-session-write");
    let file = sess.path();
    let before = std::fs::metadata(&file).expect("the scratch session exists");
    let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root);
    step(&mut s, ScreenEvent::Mount, None);
    let after = std::fs::metadata(&file).expect("the scratch session still exists");
    assert_eq!(
        before.ino(),
        after.ino(),
        "opening Settings rewrote the session file: a flash write with two fsyncs on the \
         frame the modal mounts (fps:modal-ramp, 150 ms of navcommit)"
    );
    assert_eq!(before.len(), after.len(), "and nothing about its contents moved either");
}
