//! Root-page mounting, Automatically Sign In toggling, BACK/focus restore, and the surface's
//! own logical-state hash across its inner stack.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::ui::machine::{Edge, InputEvent, InputKind, Source};
use crate::ui::present::Present;
use crate::ui::screen::By;

/// Mounting the surface at its `Root` page runs the inner stack's own lifecycle (§3.4) and
/// names the root — the heartbeat's `overlay=` before anything has been pressed.
#[test]
fn mounting_the_surface_names_its_root_page() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-mount");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    assert_eq!(name(&s), word::SETTINGS);
    assert_eq!(s.inner.depth(), 1);
    assert_eq!(s.kind, Family::Settings);
}

#[test]
fn signed_out_root_does_not_offer_automatically_sign_in() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("root-signed-out-auto");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    // Privacy / Legal / About — row 2 is About, not a switch.
    let about = FocusKey {
        entry: EntryId(0),
        elem: 2,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: about,
            by: By::Dir,
        },
        Some(about),
    );
    step(&mut s, ScreenEvent::Activate(about.elem), Some(about));
    assert_eq!(
        s.inner.depth(),
        2,
        "signed out, row 2 is About — a document push"
    );
    assert_eq!(name(&s), word::LEGAL);
    assert!(!crate::plex::session::peek().auto_sign_in());
}

#[test]
fn a_multi_user_root_toggles_automatically_sign_in_in_place() {
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-auto-toggle");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    let row = FocusKey {
        entry: EntryId(0),
        elem: AUTO_SIGN_IN_ROW,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: row,
            by: By::Dir,
        },
        Some(row),
    );
    step(&mut s, ScreenEvent::Activate(row.elem), Some(row));
    assert_eq!(
        name(&s),
        word::SETTINGS,
        "OK on the switch must not push a page"
    );
    assert_eq!(s.inner.depth(), 1);
    assert!(
        crate::plex::session::peek().auto_sign_in(),
        "OK commits the switch immediately"
    );
    step(&mut s, ScreenEvent::Activate(row.elem), Some(row));
    assert!(!crate::plex::session::peek().auto_sign_in());
    assert_eq!(s.inner.depth(), 1);
}

#[test]
fn right_on_automatically_sign_in_does_not_push() {
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-auto-right");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    let row = FocusKey {
        entry: EntryId(0),
        elem: AUTO_SIGN_IN_ROW,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: row,
            by: By::Dir,
        },
        Some(row),
    );
    let right: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Right,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: true,
        },
    });
    let _ = step(&mut s, right, Some(row));
    assert_eq!(s.inner.depth(), 1, "RIGHT on a switch is not rule 8");
    assert!(!crate::plex::session::peek().auto_sign_in());
    assert_eq!(name(&s), word::SETTINGS);
}

/// **BACK at the surface's own root does not touch the inner stack, and it is not swallowed
/// either** — `Handled::No` is exactly what tells the CONTAINER (the outer `ModalStack`) it
/// may dismiss the surface. `bridge.rs`'s own test asserts the CONSEQUENCE of this one level
/// up (the surface's phase goes to `Closing`); this is the return value that consequence is
/// built on.
#[test]
fn back_at_the_surface_s_own_root_is_not_handled() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-back-root");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    });
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
    let handled = <RouteSurface as Machine<InnerHost>>::step(&mut s, &back, &cx(None), &mut fx);
    assert_eq!(
        handled,
        Handled::No,
        "the surface's own stack has nothing to pop at depth 1"
    );
    assert_eq!(
        s.inner.depth(),
        1,
        "…and nothing about the stack moved while deciding that"
    );
}

/// **The remembered-focus round trip (spec §7.3 step 4).** Signed out, the root's rows are
/// Privacy & data / Legal notices / About PlxNative (`bridge.rs`'s own comment on the same
/// fixture: "Favourites is absent signed out"), so row 1 is Legal notices. The engine seats
/// focus there, OK pushes the index, and a BACK must hand focus back to THAT row — not row 0
/// — which is the one thing `bridge.rs`'s word-only assertions cannot see from outside `app/`.
#[test]
fn a_pop_from_legal_restores_focus_to_the_row_that_opened_it() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-pop-focus");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);

    let legal_row = FocusKey {
        entry: EntryId(0),
        elem: 1,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: legal_row,
            by: By::Dir,
        },
        Some(legal_row),
    );
    step(
        &mut s,
        ScreenEvent::Activate(legal_row.elem),
        Some(legal_row),
    );
    assert_eq!(
        name(&s),
        word::LEGAL,
        "OK on Legal notices pushed the index"
    );
    assert_eq!(s.inner.depth(), 2);

    let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    });
    let out = step(&mut s, back, None);
    assert_eq!(
        name(&s),
        word::SETTINGS,
        "BACK popped the inner stack, not the surface"
    );
    assert_eq!(s.inner.depth(), 1);

    let reseat = out.iter().find_map(|st| match &st.fx {
        Fx::Deliver(
            _,
            Delivery::Screen(ScreenEvent::Enter(Enter::Fresh {
                focus: FocusTarget::Elem(k),
            })),
        ) => Some(*k),
        _ => None,
    });
    assert_eq!(
        reseat,
        Some(legal_row),
        "BACK from Legal must ask the engine to re-seat the row that opened it, not the first row"
    );
}

/// The complementary case: a PUSH always asks for a fresh seat on the new page's own first
/// group, never the remembered list — a remembered entry belongs to the page being LEFT, and
/// reusing it for the page being ENTERED would seat the Legal index on whatever numeric row
/// happened to be focused on the root.
#[test]
fn a_push_seats_the_new_page_fresh_rather_than_from_the_remembered_list() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-push-seat");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    let root_row = FocusKey {
        entry: EntryId(0),
        elem: 1,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: root_row,
            by: By::Dir,
        },
        Some(root_row),
    );
    let out = step(&mut s, ScreenEvent::Activate(root_row.elem), Some(root_row));
    let seat = out.iter().find_map(|st| match &st.fx {
        Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) => {
            Some(*focus)
        }
        _ => None,
    });
    assert!(
        matches!(seat, Some(FocusTarget::ContainerGroup(GroupId(0)))),
        "a push seats the destination's own group 0, not a remembered element: {seat:?}"
    );
}

/// **`remembered` must not grow for the whole life of a Settings session.** Every push and
/// every pop records one entry; without the retire-time cleanup in `request`, opening and
/// closing Legal a few times would leave stale rows behind for entries the container has
/// already dropped for good, because a popped page's `EntryId` is never minted again.
#[test]
fn remembered_does_not_grow_across_repeated_visits_to_the_same_page() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-remembered");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    let legal_row = FocusKey {
        entry: EntryId(0),
        elem: 1,
    };
    for _ in 0..5 {
        step(
            &mut s,
            ScreenEvent::FocusMoved {
                from: None,
                to: legal_row,
                by: By::Dir,
            },
            Some(legal_row),
        );
        step(
            &mut s,
            ScreenEvent::Activate(legal_row.elem),
            Some(legal_row),
        );
        assert_eq!(name(&s), word::LEGAL);
        let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
            at: Tick::default(),
            source: Source::Sdl,
            kind: InputKind::Key {
                key: Key::Back,
                sym: 0,
                wcode: 0,
                edge: Edge::Down,
                at_edge: false,
            },
        });
        step(&mut s, back, None);
        assert_eq!(name(&s), word::SETTINGS);
    }
    assert!(
        s.remembered.len() <= 1,
        "five open/close cycles through the same page left {} remembered entries, want at most the live root",
        s.remembered.len()
    );
}

/// **A pop that has finished leaves the surface AT REST at depth two — which is the state the
/// draw used to get wrong.** Settings root → Legal notices → a legal document → BACK: the
/// reverse push runs 1 → 0, and once it lands the ONE page on screen is the top, the Legal
/// index. `draw` decided that case as "there is no page below me", which is only the same
/// question at depth 1 — so with the Settings root still under the index it took the PARENT
/// branch instead and drew the root at full strength while the index the user had just
/// returned to was never drawn at all, hit map included.
///
/// The assertion is on `at_rest()` because `draw` cannot be reached from a host test: it
/// paints, and painting measures text through SDL2_ttf, which this build does not link. This
/// is the predicate the branch is now keyed on, and the one that used to have no equivalent.
#[test]
fn a_settled_pop_leaves_the_surface_at_rest_at_depth_two() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-at-rest");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    assert!(
        s.at_rest(),
        "a freshly mounted surface has no push in flight"
    );

    // root → Legal notices → About-style document: two pushes, so the pop below lands on a
    // stack that still has something UNDER its top
    let legal_row = FocusKey {
        entry: EntryId(0),
        elem: 1,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: legal_row,
            by: By::Dir,
        },
        Some(legal_row),
    );
    step(
        &mut s,
        ScreenEvent::Activate(legal_row.elem),
        Some(legal_row),
    );
    assert!(
        !s.at_rest(),
        "the push is in flight the frame it is requested"
    );
    settle(&mut s);
    let doc_row = FocusKey {
        entry: EntryId(0),
        elem: 0,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: doc_row,
            by: By::Dir,
        },
        Some(doc_row),
    );
    step(&mut s, ScreenEvent::Activate(doc_row.elem), Some(doc_row));
    settle(&mut s);
    assert_eq!(
        s.inner.depth(),
        3,
        "root → Legal index → one Legal document"
    );

    let back: ScreenEvent<InnerHost> = ScreenEvent::Input(InputEvent {
        at: Tick::default(),
        source: Source::Sdl,
        kind: InputKind::Key {
            key: Key::Back,
            sym: 0,
            wcode: 0,
            edge: Edge::Down,
            at_edge: false,
        },
    });
    step(&mut s, back, None);
    assert_eq!(s.inner.depth(), 2, "BACK popped the document");
    assert!(!s.at_rest(), "…and the reverse push is carrying it out");
    settle(&mut s);
    assert!(
        s.at_rest(),
        "with the spring settled and nothing leaving, the Legal index is the only page on \
         screen — even though `below()` still answers the Settings root"
    );
    assert!(
        s.push.leaving.is_none(),
        "the outgoing body is released when the spring lands"
    );
}

/// **The surface's logical state covers its inner stack — the whole reason phase 5b re-pinned
/// `state_fp` and invalidated every committed replay fixture.** `Dispatcher::state_hash` folds
/// in exactly `screen.state().hash()` per surface, so with the ceremony alone in there (which
/// is what `SurfaceState` wrote until 2026-09-07) every press anywhere inside Settings,
/// Privacy, Legal and first-run Favourites hashed identically and a replay could never report
/// `DIVERGED`.
///
/// **What this test grades is the STACK half and nothing else, and the distinction is the one
/// the old wording lost.** It opens a page and watches the hash move, which proves the fold
/// reaches the bodies at all. It says nothing whatever about whether a given body's own
/// `state()` writes enough to tell two of ITS frames apart — the sentence here used to add
/// that the finer half "rides in through each body's own `state().hash()`", which is a
/// mechanism dressed as a guarantee, and a verifier refuted it by scrolling a Legal document
/// with the hash standing still. That half is each page's test to write, in each page's own
/// file; the `LogicalState` impl above carries the census of who currently does.
#[test]
fn the_logical_state_follows_the_inner_stack() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-state-hash");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    );
    step(&mut s, ScreenEvent::Mount, None);
    let at_root = <RouteSurface as Screen<InnerHost>>::state(&s).hash();

    let legal_row = FocusKey {
        entry: EntryId(0),
        elem: 1,
    };
    step(
        &mut s,
        ScreenEvent::FocusMoved {
            from: None,
            to: legal_row,
            by: By::Dir,
        },
        Some(legal_row),
    );
    step(
        &mut s,
        ScreenEvent::Activate(legal_row.elem),
        Some(legal_row),
    );
    let at_legal = <RouteSurface as Screen<InnerHost>>::state(&s).hash();
    assert_ne!(
        at_root, at_legal,
        "pushing the Legal index must move the surface's logical state, or a replay grades \
         every press inside the family as identical"
    );

    let mut probe = String::new();
    <RouteSurface as Screen<InnerHost>>::state(&s).probe(&mut probe);
    assert!(
        probe.starts_with("settings/root:") && probe.contains("/legal:"),
        "the probe names the path the surface is standing on, got {probe:?}"
    );
}
