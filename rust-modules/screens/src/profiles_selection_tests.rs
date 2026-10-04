//! Avatar selection, protection canon, and the roster spinner while a PIN is submitting.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// **The frozen-animator regression class, closed for the roster spinner (phase 12 D4).**
/// `spin_ms` used to be a raw `+= dt` accumulator with a separate, easy-to-forget
/// `fx.note(Motion)` a few lines below it. Now it is `motion::Phase`, which reports from
/// inside its own `advance`. A submitting PIN pad forces `has_spinner` true regardless of the
/// retained roster (`n` does not enter that branch of the `||`), so this drives it through the
/// real `Machine::step` path with an explicit immutable publication.
#[test]
fn the_roster_spinner_phase_reports_motion_on_every_tick_while_submitting() {
    let mut s = bare(Pad {
        open: true,
        submitting: true,
        ..Pad::new()
    });
    let c = cx(None);
    let mut present = Present::new();
    let _ = present.take(0);
    let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
    for ms in [16, 32, 48] {
        let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
        let ev = ScreenEvent::Tick(Tick { ms, dt_us: 16_667 });
        Machine::<SessionHost>::step(&mut s, &ev, &c, &mut fx);
        assert!(
            present.take(ms),
            "a submitting PIN pad's spinner must present every frame (ms={ms})"
        );
    }
}

#[test]
fn another_avatar_replaces_an_unprotected_choice_before_and_after_acceptance() {
    for accepted in [false, true] {
        for protected in [false, true] {
            let initial = snapshot_at(10, Phase::Profiles, vec![user("A", false), user("B", protected)]);
            let switching = snapshot_at(11, Phase::Switching, vec![user("A", false), user("B", protected)]);
            let mut s = ProfilesScreen::new(EntryId(3), initial.read());
            let first = commit_avatar(&mut s, 0, &initial);
            assert!(first.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Session(
                auth::SessionCmd::SelectProfileWithReply { index: 0, reply, .. })) if reply.correlation == 1)));
            if accepted {
                accept_selection(&mut s, 1, 11, &initial);
                step_ev_with(&mut s, &ScreenEvent::Tick(Tick { ms: 16, dt_us: 16_667 }), None, &switching, InstanceId(17));
            }
            let second = commit_avatar(&mut s, 1, &switching);
            if protected {
                assert!(s.pad.open, "protected B must open while A is pending; accepted={accepted}");
                assert!(s.pending_selection.is_none());
                let before = s.state.hash();
                accept_selection(&mut s, 1, 11, &switching);
                assert_eq!(s.state.hash(), before, "A's late ACK cannot affect B's fresh pad");
            } else {
                assert!(second.iter().any(|e| matches!(&e.fx, Fx::App(AppFx::Session(
                    auth::SessionCmd::SelectProfileWithReply { index: 1, reply, .. }))
                    if reply.correlation == 2 && reply.instance == 17)), "B must replace A; accepted={accepted}");
                accept_selection(&mut s, 1, 11, &switching);
                assert_eq!(s.pending_selection.map(PendingSelection::correlation), Some(2));
                let ready = snapshot_at(12, Phase::Ready, vec![user("A", false), user("B", false)]);
                step_ev_with(&mut s, &ScreenEvent::Tick(Tick { ms: 32, dt_us: 16_667 }), None, &ready, InstanceId(17));
                accept_selection(&mut s, 2, 12, &ready);
                assert!(s.pending_selection.is_none(), "B can complete on fast Ready without a Switching sample");
            }
        }
    }
}

#[test]
fn failed_replacement_allocation_preserves_the_original_choice() {
    for accepted in [false, true] {
        let read = snapshot_at(10, Phase::Profiles, vec![user("A", false), user("B", false)]);
        let mut s = ProfilesScreen::new(EntryId(3), read.read());
        commit_avatar(&mut s, 0, &read);
        if accepted { accept_selection(&mut s, 1, 11, &read); }
        let original = s.pending_selection;
        s.next_correlation = Some(u32::MAX);
        let effects = commit_avatar(&mut s, 1, &read);
        assert_eq!(s.pending_selection, original);
        assert!(!s.pad.submitting);
        assert!(effects.iter().all(|e| !matches!(e.fx, Fx::App(AppFx::Session(_)))));
    }
}

#[test]
fn equal_count_and_epoch_rosters_have_distinct_canon_for_protection_decisions() {
    let open = snapshot_at(20, Phase::Profiles, vec![user("U", false)]);
    let protected = snapshot_at(20, Phase::Profiles, vec![user("U", true)]);
    let mut a = ProfilesScreen::new(EntryId(3), open.read());
    let mut b = ProfilesScreen::new(EntryId(3), protected.read());
    let (a_hash, b_hash) = (a.state.hash(), b.state.hash());
    let a_fx = commit_avatar(&mut a, 0, &protected);
    let b_fx = commit_avatar(&mut b, 0, &protected);
    assert!(a_fx.iter().any(|e| matches!(e.fx, Fx::App(AppFx::Session(auth::SessionCmd::SelectProfileWithReply { .. })))));
    assert!(b.pad.open);
    assert!(b_fx.iter().all(|e| !matches!(e.fx, Fx::App(AppFx::Session(auth::SessionCmd::SelectProfileWithReply { .. })))));
    assert_ne!(a_hash, b_hash, "different retained protection decisions must not collide before identical PressCommit input");
}

#[test]
fn retained_protection_canon_preserves_order_and_ignores_allocation_identity() {
    let first = snapshot_at(20, Phase::Profiles, vec![user("A", false), user("B", true)]);
    let same = snapshot_at(20, Phase::Profiles, vec![user("A", false), user("B", true)]);
    let swapped = snapshot_at(20, Phase::Profiles, vec![user("A", true), user("B", false)]);
    let a = ProfilesScreen::new(EntryId(3), first.read());
    let b = ProfilesScreen::new(EntryId(3), same.read());
    let c = ProfilesScreen::new(EntryId(3), swapped.read());
    assert_eq!(a.state.hash(), b.state.hash());
    assert_ne!(a.state.hash(), c.state.hash(), "a count of protected tiles loses their order");
}

#[test]
fn a_submitting_pin_pad_still_blocks_avatar_commits() {
    let read = snapshot_at(30, Phase::Profiles, vec![user("A", true), user("B", false)]);
    let mut s = ProfilesScreen::new(EntryId(3), read.read());
    submit_locked(&mut s, 0, InstanceId(17));
    let pending = s.pending_selection;
    let effects = commit_avatar(&mut s, 1, &read);
    assert_eq!(s.pending_selection, pending);
    assert!(s.pad.submitting);
    assert!(effects.iter().all(|e| !matches!(e.fx, Fx::App(AppFx::Session(_)))));
}

#[test]
fn constructor_and_ticks_retain_coherent_rosters_per_instance_before_first_tick() {
    let first_snapshot = snapshot(Phase::Profiles, vec![user("First", false)]);
    let second_snapshot = snapshot(Phase::Switching, vec![user("Second", true)]);
    let first = ProfilesScreen::new(EntryId(1), first_snapshot.read());
    let mut second = ProfilesScreen::new(EntryId(2), second_snapshot.read());

    assert_eq!(first.users[0].title, "First");
    assert_eq!(
        first.state.roster_n, 1,
        "the first paint is seeded synchronously"
    );
    assert_eq!(second.users[0].title, "Second");
    assert_eq!(second.phase, Phase::Switching);

    let replacement = snapshot(
        Phase::Profiles,
        vec![user("Third", false), user("Fourth", false)],
    );
    step_ev_with(
        &mut second,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &replacement,
        InstanceId(2),
    );
    assert_eq!(second.users.len(), 2);
    assert_eq!(second.users[0].title, "Third");
    assert_eq!(
        first.users[0].title, "First",
        "another instance retains its own Arc roster"
    );
}

