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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
    crate::storage_worker::drain_for_test();
    assert_eq!(
        name(&s),
        word::SETTINGS,
        "OK on the switch must not push a page"
    );
    assert_eq!(s.inner.depth(), 1);
    assert!(
        crate::plex::session::peek().auto_sign_in(),
        "the queued switch is durable after the worker completes"
    );
    step(&mut s, ScreenEvent::Activate(row.elem), Some(row));
    crate::storage_worker::drain_for_test();
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
///
/// **Asserting the emitted REQUEST used to be the whole test, and that was never enough.** Every
/// page in this family shares the surface's outer `EntryId` and `GroupId(0)`, so
/// `FocusTarget::ContainerGroup(GroupId(0))` and the fixed `FocusTarget::FirstInGroup(GroupId(0))`
/// below are requests for the exact same group — the difference is invisible at this level and
/// only shows up one step later, inside `FocusEngine::enter`, where `ContainerGroup` resolves
/// through the group's `Seat::Remembered` policy and reads the OUTGOING page's row back for the
/// page being entered (`ui/focus.rs`'s `seat_in`). A version of this test that stopped at the
/// emitted effect would have kept passing on the old, buggy `ContainerGroup` request forever,
/// because the request LOOKED right; it just fed a policy that made it wrong two steps later. So
/// this asserts the new target by name, not merely "some fresh focus target came out".
#[test]
fn a_push_seats_the_new_page_fresh_rather_than_from_the_remembered_list() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-push-seat");
    let mut s = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        matches!(seat, Some(FocusTarget::FirstInGroup(GroupId(0)))),
        "a push seats the destination's own group 0 FIRST-in-group, ignoring any remembered \
         cursor for it (not `ContainerGroup`, whose `Seat::Remembered` would read the outgoing \
         page's row back): {seat:?}"
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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
        crate::pms::HubsSnapshot::empty_for_test().view(),
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

#[test]
fn session_refresh_rebuilds_root_without_navigation() {
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-session-refresh");
    let saved = crate::plex::session::peek();
    crate::plex::session::install_transient_for_test(true);
    let mut root = RootPage::new(EntryId(0), cx(None).views);
    assert!(!root.rows.iter().any(|a| matches!(a, Action::AutoSignIn)));
    crate::plex::session::save(&saved.with_auto_sign_in(true));
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    root.step(&ScreenEvent::Tick(Tick::default()), &cx(None), &mut fx);
    assert!(root.state.auto_sign_in, "a landed session must rebuild cached toggle values");
    assert!(root.rows.iter().any(|a| matches!(a, Action::AutoSignIn)),
        "a landed roster must restore the multi-user row");
}

#[test]
fn session_refresh_keeps_optimistic_setting_through_transient_completion() {
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-pending-refresh");
    let saved = crate::plex::session::peek();
    let mut root = RootPage::new(EntryId(0), cx(None).views);
    let ticket = crate::storage_worker::submit_retained(|| false);
    crate::storage_worker::drain_for_test();
    root.pending_auto = Some((true, ticket));
    root.rebuild(0, cx(None).views);
    crate::plex::session::install_transient_for_test(false);
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    root.step(&ScreenEvent::Tick(Tick::default()), &cx(None), &mut fx);
    assert!(root.state.auto_sign_in, "transient completion must not erase the optimistic value");
    assert!(root.rows.iter().any(|a| matches!(a, Action::AutoSignIn)),
        "transient storage must not remove the pending toggle");
    crate::plex::session::save(&saved);
    root.step(&ScreenEvent::Tick(Tick::default()), &cx(None), &mut fx);
    assert!(!root.state.auto_sign_in, "settled authority resolves the refused write");
    assert!(root.pending_auto.is_none());
}

/// **Unencrypted connections, in Settings** (PLX-NATIVE-10): a server the person allowed shows a
/// switch, its detail stating what it means and that a grant carries it now; turning the switch
/// off withdraws the grant AT ONCE — before the preferences write lands — and records the
/// revocation, so the next discovery keeps the server tokenless.
#[test]
fn the_unencrypted_connection_switch_shows_the_grant_and_revokes_it_at_once() {
    use crate::plex::session::PlaintextChoice;
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-plaintext-switch");
    crate::plex::reset_servers_for_test();
    crate::plex::grant::reset_for_test();
    let account = crate::plex::grant::account_key(&crate::plex::session::peek().account_token);
    assert!(!account.is_empty(), "the fixture session is signed in");
    let mut saved = crate::plex::session::peek().with_plaintext_choice(&account, "lan-machine", PlaintextChoice::Allowed);
    saved.sources.push(crate::plex::session::SourceRef {
        machine_id: "lan-machine".into(),
        name: "Basement".into(),
        ..Default::default()
    });
    crate::plex::session::save(&saved);
    let lan = crate::plex::Origin::http("192.168.0.10", 32400);
    crate::plex::grant::mint(crate::plex::grant::scope(), "lan-machine", &lan,
        &crate::plex::grant::eligible_evidence_for_test()).unwrap();

    let mut root = RootPage::new(EntryId(0), cx(None).views);
    let row = root.rows.iter().position(|a| matches!(a, Action::Plaintext(0)))
        .expect("an allowed server has its switch");
    let drawn = root.table.sections.iter().flat_map(|s| &s.rows).nth(row).unwrap();
    assert_eq!(drawn.label, "Basement");
    assert_eq!(drawn.toggle, Some(true));
    assert_eq!(drawn.detail, "Allowed on this network. Connected without encryption.");
    assert_eq!(root.state.plaintext, vec![true]);

    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    root.activate(row as i32, cx(None).views, &mut fx);
    assert_eq!(crate::plex::grant::granted_origin("lan-machine"), None, "revoked before the write lands");
    assert!(!crate::plex::grant::allowed_under(crate::plex::CredentialPolicy::HttpsOnly, &lan));
    assert_eq!(root.state.plaintext, vec![false], "the switch shows the answer optimistically");
    let drawn = root.table.sections.iter().flat_map(|s| &s.rows).nth(row).unwrap();
    assert_eq!(drawn.toggle, Some(false));
    assert_eq!(drawn.detail, "Not allowed. Only encrypted connections.");
    crate::storage_worker::drain_for_test();
    assert_eq!(crate::plex::session::peek().plaintext_choice(&account, "lan-machine"), PlaintextChoice::Revoked);
    crate::plex::grant::reset_for_test();
    crate::plex::reset_servers_for_test();
}

/// Nobody was ever asked, so there is nothing to turn off: no section at all.
#[test]
fn no_unencrypted_connection_section_without_an_answer() {
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-plaintext-none");
    let root = RootPage::new(EntryId(0), cx(None).views);
    assert!(!root.rows.iter().any(|a| matches!(a, Action::Plaintext(_))));
    assert!(root.state.plaintext.is_empty());
}

/// **Turning a switch ON asks first, and a never-asked server is listed** (PLX-NATIVE-10, code
/// review 3 and design review D8). A server discovery offers "Connect without encryption?" for
/// and nobody has answered shows its switch OFF; turning it on records nothing — it opens the
/// SAME question the read-outs ask (`screens::plaintext_question`), seated on *Not now* — and only
/// its *Connect* sends the answer (Allowed, re-finding the server), shown on at once.
#[test]
fn turning_an_unencrypted_connection_on_asks_the_shared_question_first() {
    use crate::plex::session::PlaintextChoice;
    let _g = crate::testlock::serial();
    let _sess = multi_user_session("root-plaintext-ask");
    crate::plex::reset_servers_for_test();
    crate::plex::grant::reset_for_test();
    crate::plex::grant::offered(crate::plex::grant::scope(), crate::plex::grant::PlaintextVerdict {
        machine_id: "lan-machine".into(), name: "Basement".into(), shared_by: String::new(),
        eligibility: crate::plex::probe::PlaintextEligibility::Eligible, choice: PlaintextChoice::Undecided,
    });
    let mut root = RootPage::new(EntryId(0), cx(None).views);
    let row = root.rows.iter().position(|a| matches!(a, Action::Plaintext(0)))
        .expect("an offered, never-asked server has its switch");
    let drawn = root.table.sections.iter().flat_map(|s| &s.rows).nth(row).unwrap();
    assert_eq!(drawn.toggle, Some(false));
    assert_eq!(drawn.detail, "Not allowed. Only encrypted connections.");

    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    root.activate(row as i32, cx(None).views, &mut fx);
    assert!(root.alert.is_open(), "ON asks before anything is recorded");
    assert_eq!(root.state.plaintext, vec![false]);
    assert!(crate::plex::grant::choices(&[], &crate::plex::session::peek().account_token).is_empty(),
        "nothing is recorded yet");
    let mut groups = Vec::new();
    Focusable::<InnerHost>::groups(&root, &cx(None), &mut groups);
    assert_eq!(groups.iter().map(|g| g.id).collect::<Vec<_>>(), [ALERT_GROUP], "the question traps focus");
    let from = Placed { rect: Rect::FULL, rest_rect: Rect::FULL, clip: Rect::FULL, index: None };
    assert_eq!(Focusable::<InnerHost>::seat(&root, ALERT_GROUP, from, &cx(None)).elem, super::super::registry::ALERT,
        "seated on Not now");

    let mut out = Vec::new();
    let mut present = Present::new();
    let mut fx = Effects::new(&mut out, MachineId::Session, &mut present);
    let connect = FocusKey { entry: EntryId(0), elem: super::super::registry::ALERT + 1 };
    root.step(&ScreenEvent::PressCommit(crate::ui::machine::PressId(1)), &cx(Some(connect)), &mut fx);
    let answers: Vec<_> = out.iter().filter_map(|st| match &st.fx {
        Fx::App(super::super::registry::AppFx::Session(crate::auth::SessionCmd::AnswerPlaintext { machine_id, choice, .. }))
            if machine_id == "lan-machine" => Some(*choice),
        _ => None,
    }).collect();
    assert_eq!(answers, [PlaintextChoice::Allowed]);
    assert!(!root.alert.is_open());
    assert_eq!(root.state.plaintext, vec![true], "the switch shows the answer optimistically");
    crate::plex::grant::reset_for_test();
    crate::plex::reset_servers_for_test();
}
