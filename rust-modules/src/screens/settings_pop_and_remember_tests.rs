//! `forward`-driven `Fx::Remember`/`Fx::Nav(NavOp::Pop)` ownership: who may project a
//! remembered selection, and when a Pop is the surface's own dismissal rather than an inner
//! pop.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::ui::present::Present;
use crate::ui::screen::By;

#[test]
fn only_current_inner_instance_can_project_remembered_selection() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("inner-remember-owner");
    let mut s = RouteSurface::new(EntryId(0), InstanceId(0), Family::Settings, SettingsPage::Root,
        crate::pms::HubsSnapshot::empty_for_test().view());
    step(&mut s, ScreenEvent::Mount, None);
    let covered = s.inner.top().unwrap().inst.as_ref().unwrap().id;
    forwarded(&mut s, Fx::Nav(NavOp::Push(SettingsPage::Legal)));
    let active = s.inner.top().unwrap().inst.as_ref().unwrap().id;
    assert_ne!(covered, active);
    for (source, accepted) in [(covered, false), (active, true)] {
        let mut out = Vec::new();
        let mut present = Present::new();
        let mut sink = Effects::new(&mut out, MachineId::Instance(InstanceId(0)), &mut present);
        s.forward(
            vec![Stamped {
                from: MachineId::Instance(source),
                fx: Fx::Remember { group: GroupId(0), elem: 0 },
            }],
            &cx(None),
            &mut sink,
        );
        assert_eq!(out.iter().any(|s| matches!(s.fx, Fx::Remember { .. })), accepted);
    }
}

/// **A Pop that would empty the inner stack is the surface's dismissal, not a surface left up
/// with no page in it.** The configuration is the real one:
/// `/tmp/plxnative-settings=privacy` roots the surface AT Privacy & data, and that page's
/// Done (`consent::band_commit`'s Settings arm) emits a bare `Fx::Nav(NavOp::Pop)` — correct
/// when Privacy sits over the Settings root, and one entry too many here. `NavStack` has no
/// depth guard of its own, so before the fix this retired the only entry and left `top_mut()`
/// at `None` while `at_rest()` stayed true: scrim and ground still drawn, input still owned,
/// no page, no hit stops, and no key that could reach it. `onboard::leave`'s settings arm and
/// `/tmp/plxnative-settings=home` are the same pair.
#[test]
fn a_pop_that_would_empty_the_stack_dismisses_the_surface_instead() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-pop-empty");
    let mut s = RouteSurface::new(
        EntryId(7),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Privacy,
    
        crate::pms::HubsSnapshot::empty_for_test().view(),
    );
    step(&mut s, ScreenEvent::Mount, None);
    assert_eq!(
        s.inner.depth(),
        1,
        "rooted at Privacy, there is nothing under it"
    );

    let out = forwarded(&mut s, Fx::Nav(NavOp::Pop));
    assert!(
        out.iter()
            .any(|st| matches!(&st.fx, Fx::Nav(NavOp::Dismiss(e)) if *e == EntryId(7))),
        "the Pop must become this surface's own dismissal, emitted against its entry"
    );
    assert_eq!(
        s.inner.depth(),
        1,
        "…and the stack must NOT have been emptied on the way"
    );
    assert!(
        s.top().is_some(),
        "a surface with no top page draws its ground over nothing and cannot be left"
    );
}

/// The complementary half, and the one that must not regress: with a page UNDER it, the same
/// forwarded Pop is an ordinary inner pop and the surface stays up. Without this, "dismiss on
/// Pop" would be indistinguishable from "dismiss on every Pop", which would take Privacy's
/// Done straight out of Settings instead of back to its root.
#[test]
fn a_pop_with_a_page_under_it_pops_the_inner_stack_and_stays_up() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-pop-inner");
    let mut s = RouteSurface::new(
        EntryId(7),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    
        crate::pms::HubsSnapshot::empty_for_test().view(),
    );
    step(&mut s, ScreenEvent::Mount, None);
    let legal_row = FocusKey {
        entry: EntryId(7),
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
    assert_eq!(s.inner.depth(), 2, "Legal is up");

    let out = forwarded(&mut s, Fx::Nav(NavOp::Pop));
    assert!(
        !out.iter()
            .any(|st| matches!(&st.fx, Fx::Nav(NavOp::Dismiss(_)))),
        "a Pop with something under it is the INNER stack's, never the surface's"
    );
    assert_eq!(s.inner.depth(), 1);
    assert_eq!(
        name(&s),
        word::SETTINGS,
        "it landed back on the Settings root"
    );
}

/// **`remembered` is in the hash, and this is the arrangement that isolates it.** Two
/// surfaces are driven through byte-identical page states — same root selection (`FocusMoved`
/// writes `RootState::sel` from the event, not from `Cx`), same push, same entry and instance
/// ids from the same `Minter` sequence — and differ in ONE respect: the first pushes with the
/// engine reporting a current focus, so `request` records the seat, and the second pushes
/// with none, so it records nothing. Before the seats were folded in, those two surfaces
/// hashed identically and then behaved differently the moment BACK was pressed: the second
/// assertion is that divergence, arriving one frame later at the re-seat, with nothing in the
/// record able to say why.
#[test]
fn the_remembered_seats_are_part_of_the_hash() {
    let _g = crate::testlock::serial();
    let _sess = scratch_session("surface-seat-hash");
    let legal_row = FocusKey {
        entry: EntryId(0),
        elem: 1,
    };

    let mut seated = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    
        crate::pms::HubsSnapshot::empty_for_test().view(),
    );
    step(&mut seated, ScreenEvent::Mount, None);
    step(
        &mut seated,
        ScreenEvent::FocusMoved {
            from: None,
            to: legal_row,
            by: By::Dir,
        },
        Some(legal_row),
    );
    step(
        &mut seated,
        ScreenEvent::Activate(legal_row.elem),
        Some(legal_row),
    );

    let mut unseated = RouteSurface::new(
        EntryId(0),
        InstanceId(0),
        Family::Settings,
        SettingsPage::Root,
    
        crate::pms::HubsSnapshot::empty_for_test().view(),
    );
    step(&mut unseated, ScreenEvent::Mount, None);
    step(
        &mut unseated,
        ScreenEvent::FocusMoved {
            from: None,
            to: legal_row,
            by: By::Dir,
        },
        Some(legal_row),
    );
    step(&mut unseated, ScreenEvent::Activate(legal_row.elem), None);

    assert_eq!(
        seated.inner.depth(),
        unseated.inner.depth(),
        "the same stack, by construction"
    );
    assert!(
        !seated.remembered.is_empty(),
        "the seated push recorded where focus was"
    );
    assert!(
        unseated.remembered.is_empty(),
        "the unseated one had nothing to record"
    );
    assert_ne!(
        <RouteSurface as Screen<InnerHost>>::state(&seated).hash(),
        <RouteSurface as Screen<InnerHost>>::state(&unseated).hash(),
        "two surfaces that will seat focus differently on the next BACK must not hash alike"
    );

    // …and here is the behaviour that difference predicts, so the hash is grading something
    // a replay can actually see go wrong rather than an incidental field. `ScreenEvent` is
    // not `Clone` (it carries a host's own types), so the press is built once per surface.
    let a = step(&mut seated, back_key(), None);
    let b = step(&mut unseated, back_key(), None);
    let seat_of = |out: &[Stamped<InnerHost>]| {
        out.iter().find_map(|st| match &st.fx {
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus }))) => {
                Some(*focus)
            }
            _ => None,
        })
    };
    assert!(matches!(seat_of(&a), Some(FocusTarget::Elem(k)) if k == legal_row));
    // `unseated` never remembered a row for the entry it's popping back to, so `request` falls
    // to its default arm — `FirstInGroup`, not `ContainerGroup`, since `ContainerGroup` would
    // resolve through `Seat::Remembered` and could read back whatever the shared entry's group 0
    // last held (Legal's own row, still live in the engine at this point in the test), which is
    // exactly the cross-page leak this surface's own `remembered` bookkeeping exists to avoid.
    assert!(matches!(
        seat_of(&b),
        Some(FocusTarget::FirstInGroup(GroupId(0)))
    ));
}
