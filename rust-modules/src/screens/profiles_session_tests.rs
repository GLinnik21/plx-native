//! Typed session commands and the ack/epoch/correlation bookkeeping around a pending selection.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// #132: an empty picker whose roster request has ENDED with a reason is a read-out, not the
/// loading spinner — the spinner had no failure state to end it and spun forever. An empty picker
/// with no reason is still loading; a reason over tiles is the ordinary one-line switch error.
#[test]
fn an_empty_roster_with_a_reason_is_a_readout_and_not_the_spinner() {
    let mut s = bare(Pad::new());
    let loading = snapshot(Phase::Profiles, Vec::new());
    s.resync(loading.read());
    assert!(!s.roster_readout(), "no reason yet: still loading");

    let mut failed = snapshot(Phase::Profiles, Vec::new());
    failed.error = Arc::from(auth::owner::ROSTER_REFUSED);
    s.resync(failed.read());
    assert!(s.roster_readout(), "a finished, empty roster reads out its reason");

    let (handled, fx) = step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
    assert_eq!(handled, Handled::Yes);
    assert!(fx.iter().any(|st| matches!(st.fx, Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { .. })))),
        "BACK asks the session to leave; the owner decides (see `roster_dead_end`)");

    let mut switch_failed = snapshot(Phase::Profiles, vec![user("Synthetic", false)]);
    switch_failed.error = Arc::from("Couldn't switch profile. Try again.");
    s.resync(switch_failed.read());
    assert!(!s.roster_readout(), "tiles on screen: the error is the one-line switch failure");
}

#[test]
fn selection_pin_and_signout_are_typed_session_commands() {
    let published = snapshot_at(
        10,
        Phase::Profiles,
        vec![user("Open", false), user("Locked", true)],
    );
    let mut open_screen = ProfilesScreen::new(EntryId(3), published.read());

    let (_, select_fx) = step_ev_with(
        &mut open_screen,
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
        Some(FocusKey {
            entry: EntryId(3),
            elem: 0,
        }),
        &published,
        InstanceId(17),
    );
    assert!(select_fx.iter().any(|st| matches!(
        &st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::SelectProfileWithReply {
            index: 0,
            pin: None,
            reply,
        })) if reply.instance == 17 && reply.correlation == 1
    )));
    assert!(matches!(
        open_screen.pending_selection,
        Some(PendingSelection::AwaitingAck {
            correlation: 1,
            pad: false
        })
    ));

    let mut signout_screen = ProfilesScreen::new(EntryId(4), published.read());
    let (_, signout_fx) = step_ev_with(
        &mut signout_screen,
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(2)),
        Some(FocusKey {
            entry: EntryId(4),
            elem: FOOTER,
        }),
        &published,
        InstanceId(17),
    );
    assert!(signout_fx
        .iter()
        .any(|st| matches!(st.fx, Fx::App(AppFx::Session(auth::SessionCmd::SignOut)))));

    let mut locked_screen = ProfilesScreen::new(EntryId(5), published.read());
    let mut pin_effects = Vec::<Stamped<SessionHost>>::new();
    let mut present = Present::new();
    {
        let mut fx = Effects::new(
            &mut pin_effects,
            MachineId::Instance(InstanceId(17)),
            &mut present,
        );
        locked_screen.select(1, &mut fx);
        assert!(locked_screen.pad.open);
        for digit in b"1234" {
            locked_screen.press(*digit, &mut fx);
        }
    }
    assert!(pin_effects.iter().any(|st| matches!(
        &st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::SelectProfileWithReply {
            index: 1,
            pin: Some(pin),
            reply,
        })) if pin == "1234" && reply.instance == 17 && reply.correlation == 1
    )));
    assert!(locked_screen.pad.submitting);
}

#[test]
fn a_fresh_pad_cannot_consume_the_previous_instances_pin_denial() {
    let mut stale = snapshot_at(40, Phase::Profiles, vec![user("Locked", true)]);
    stale.pin_denied = true;
    let mut screen = ProfilesScreen::new(EntryId(3), stale.read());
    assert!(
        !screen.pin_denied,
        "the carried dismissal may not delay fresh first paint"
    );
    let mut effects = Vec::<Stamped<SessionHost>>::new();
    let mut present = Present::new();
    {
        let mut fx = Effects::new(
            &mut effects,
            MachineId::Instance(InstanceId(17)),
            &mut present,
        );
        screen.select(0, &mut fx);
        for digit in b"1234" {
            screen.press(*digit, &mut fx);
        }
    }

    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &stale,
        InstanceId(17),
    );
    assert!(
        screen.pad.submitting,
        "the old denial is ignored while command acceptance is unknown"
    );

    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 41,
            },
        ),
        None,
        &stale,
        InstanceId(17),
    );
    assert!(
        screen.pad.submitting,
        "the retained read is older than accepted epoch 41"
    );

    let mut denied = snapshot_at(41, Phase::Profiles, vec![user("Locked", true)]);
    denied.pin_denied = true;
    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 32,
            dt_us: 16_667,
        }),
        None,
        &denied,
        InstanceId(17),
    );
    assert!(!screen.pad.submitting);
    assert!(
        screen.pad.error_s > 0.0,
        "a denial published after the clear belongs to this pad"
    );
}

#[test]
fn a_new_denial_is_not_lost_when_dismiss_and_switch_publish_between_ticks() {
    let mut old_denial = snapshot_at(50, Phase::Profiles, vec![user("Locked", true)]);
    old_denial.pin_denied = true;
    let mut screen = ProfilesScreen::new(EntryId(3), old_denial.read());
    let mut effects = Vec::<Stamped<SessionHost>>::new();
    let mut present = Present::new();
    {
        let mut fx = Effects::new(
            &mut effects,
            MachineId::Instance(InstanceId(17)),
            &mut present,
        );
        screen.select(0, &mut fx);
        for digit in b"1234" {
            screen.press(*digit, &mut fx);
        }
    }

    // Session may publish the constructor's Dismiss(false) and the new switch's denial(true)
    // in one drain before the next frame. The accepted epoch makes the two true values
    // distinguishable without requiring an intermediate false Tick.
    let mut new_denial = snapshot_at(51, Phase::Profiles, vec![user("Locked", true)]);
    new_denial.pin_denied = true;
    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &new_denial,
        InstanceId(17),
    );
    assert!(
        screen.pad.submitting,
        "the terminal E=51 read may arrive before its ACK"
    );
    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 51,
            },
        ),
        None,
        &new_denial,
        InstanceId(17),
    );

    assert!(
        !screen.pad.submitting,
        "the new request's denial must be consumed"
    );
    assert!(
        screen.pad.error_s > 0.0,
        "the new denial flashes the current pad"
    );
}

#[test]
fn a_carried_select_command_does_not_look_like_an_immediate_switch_failure() {
    let published = snapshot_at(60, Phase::Profiles, vec![user("Locked", true)]);
    let mut screen = ProfilesScreen::new(EntryId(3), published.read());
    let mut effects = Vec::<Stamped<SessionHost>>::new();
    let mut present = Present::new();
    {
        let mut fx = Effects::new(
            &mut effects,
            MachineId::Instance(InstanceId(17)),
            &mut present,
        );
        screen.select(0, &mut fx);
        for digit in b"1234" {
            screen.press(*digit, &mut fx);
        }
    }
    assert!(screen.pad.submitting);
    assert!(effects.iter().any(|st| matches!(
        &st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::SelectProfileWithReply {
            index: 0,
            pin: Some(pin),
            reply,
        })) if pin == "1234" && reply.instance == 17 && reply.correlation == 1
    )));

    // The command is still in the queue; this is the same publication used by the press.
    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &published,
        InstanceId(17),
    );

    assert!(
        screen.pad.open,
        "an unchanged pre-command publication is not a switch failure"
    );
    assert!(
        screen.pad.submitting,
        "the pad waits for an addressed acknowledgement/result"
    );

    let (_, refused) = step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: false,
                flow_epoch: 60,
            },
        ),
        None,
        &published,
        InstanceId(17),
    );
    assert!(!screen.pad.submitting);
    assert!(
        screen.pad.open,
        "rejection is finite but does not invent a switch failure"
    );
    assert!(refused.iter().all(|st| !matches!(
        st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError))
    )));
}

#[test]
fn a_fast_ready_read_seen_before_its_ack_is_consumed_at_that_epoch() {
    let old = snapshot_at(65, Phase::Profiles, vec![user("Open", false)]);
    let mut screen = ProfilesScreen::new(EntryId(3), old.read());
    let (_, effects) = step_ev_with(
        &mut screen,
        &ScreenEvent::PressCommit(crate::ui::machine::PressId(1)),
        Some(FocusKey {
            entry: EntryId(3),
            elem: 0,
        }),
        &old,
        InstanceId(17),
    );
    assert!(effects.iter().any(|st| matches!(
        &st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::SelectProfileWithReply {
            index: 0,
            pin: None,
            reply,
        })) if reply.correlation == 1
    )));

    let ready = snapshot_at(66, Phase::Ready, vec![user("Open", false)]);
    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &ready,
        InstanceId(17),
    );
    assert!(matches!(
        screen.pending_selection,
        Some(PendingSelection::AwaitingAck { .. })
    ));

    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 66,
            },
        ),
        None,
        &ready,
        InstanceId(17),
    );
    assert!(screen.pending_selection.is_none());
}

#[test]
fn older_reads_wait_and_a_newer_flow_drops_only_local_pending_state() {
    let old = snapshot_at(70, Phase::Profiles, vec![user("Locked", true)]);
    let mut screen = ProfilesScreen::new(EntryId(3), old.read());
    submit_locked(&mut screen, 0, InstanceId(17));
    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 71,
            },
        ),
        None,
        &old,
        InstanceId(17),
    );
    assert!(matches!(
        screen.pending_selection,
        Some(PendingSelection::Accepted { flow_epoch: 71, .. })
    ));

    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &old,
        InstanceId(17),
    );
    assert!(
        screen.pad.submitting,
        "a read older than accepted E=71 still waits"
    );

    let mut superseding = snapshot_at(72, Phase::Profiles, vec![user("Locked", true)]);
    superseding.pin_denied = true;
    let (_, effects) = step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 32,
            dt_us: 16_667,
        }),
        None,
        &superseding,
        InstanceId(17),
    );
    assert!(screen.pending_selection.is_none());
    assert!(!screen.pad.open, "the superseded local pad is retired");
    assert!(
        effects.iter().all(|st| !matches!(
            st.fx,
            Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError))
        )),
        "retiring E=71 must not clear the newer E=72 flow's verdict"
    );
}

#[test]
fn closing_and_reopening_the_pad_makes_old_and_foreign_acks_harmless() {
    let old = snapshot_at(80, Phase::Profiles, vec![user("Locked", true)]);
    let mut screen = ProfilesScreen::new(EntryId(3), old.read());
    submit_locked(&mut screen, 0, InstanceId(17));
    assert_eq!(
        screen.pending_selection.map(PendingSelection::correlation),
        Some(1)
    );

    step_ev_with(
        &mut screen,
        &key_down(Key::Back, 0, 0),
        None,
        &old,
        InstanceId(17),
    );
    assert!(screen.pending_selection.is_none());
    submit_locked(&mut screen, 0, InstanceId(17));
    assert_eq!(
        screen.pending_selection.map(PendingSelection::correlation),
        Some(2)
    );

    for event in [
        ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 81,
            },
        ),
        ScreenEvent::Async(
            crate::ui::machine::RequestId(99),
            AppMsg::SelectionReply {
                correlation: 2,
                accepted: true,
                flow_epoch: 82,
            },
        ),
    ] {
        step_ev_with(&mut screen, &event, None, &old, InstanceId(17));
        assert_eq!(
            screen.pending_selection.map(PendingSelection::correlation),
            Some(2),
            "neither the closed pad's stale ACK nor a foreign RequestId settles this pad"
        );
        assert!(screen.pad.submitting);
    }

    let mut fast_denial = snapshot_at(82, Phase::Profiles, vec![user("Locked", true)]);
    fast_denial.pin_denied = true;
    step_ev_with(
        &mut screen,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &fast_denial,
        InstanceId(17),
    );
    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(2),
            AppMsg::SelectionReply {
                correlation: 2,
                accepted: true,
                flow_epoch: 82,
            },
        ),
        None,
        &fast_denial,
        InstanceId(17),
    );
    assert!(screen.pad.error_s > 0.0);

    let before = screen.state.hash();
    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(2),
            AppMsg::SelectionReply {
                correlation: 2,
                accepted: true,
                flow_epoch: 82,
            },
        ),
        None,
        &fast_denial,
        InstanceId(17),
    );
    assert_eq!(screen.state.hash(), before, "a duplicate ACK is stale");
}

#[test]
fn checked_selection_correlation_exhaustion_emits_nothing_and_never_submits() {
    let published = snapshot_at(90, Phase::Profiles, vec![user("Locked", true)]);
    let mut screen = ProfilesScreen::new(EntryId(3), published.read());
    screen.next_correlation = Some(u32::MAX);
    let effects = submit_locked(&mut screen, 0, InstanceId(17));
    assert!(screen.pad.open);
    assert!(
        !screen.pad.submitting,
        "an unaddressable request must not strand a spinner"
    );
    assert!(screen.pending_selection.is_none());
    assert!(effects.iter().all(|st| !matches!(
        st.fx,
        Fx::App(AppFx::Session(
            auth::SessionCmd::SelectProfileWithReply { .. }
        ))
    )));
}

#[test]
fn canonical_state_distinguishes_awaiting_and_each_accepted_epoch() {
    let published = snapshot_at(100, Phase::Profiles, vec![user("Locked", true)]);
    let mut screen = ProfilesScreen::new(EntryId(3), published.read());
    let fresh = Screen::<SessionHost>::state(&screen).hash();
    submit_locked(&mut screen, 0, InstanceId(17));
    let awaiting = Screen::<SessionHost>::state(&screen).hash();
    assert_ne!(awaiting, fresh);

    step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 101,
            },
        ),
        None,
        &published,
        InstanceId(17),
    );
    let accepted_101 = Screen::<SessionHost>::state(&screen).hash();
    assert_ne!(accepted_101, awaiting);

    let mut other = ProfilesScreen::new(EntryId(4), published.read());
    submit_locked(&mut other, 0, InstanceId(18));
    step_ev_with(
        &mut other,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::SelectionReply {
                correlation: 1,
                accepted: true,
                flow_epoch: 102,
            },
        ),
        None,
        &published,
        InstanceId(18),
    );
    assert_ne!(Screen::<SessionHost>::state(&other).hash(), accepted_101);
}

#[test]
fn canon_distinguishes_the_cached_read_used_to_reduce_the_same_accepted_ack() {
    let old = snapshot_at(200, Phase::Profiles, vec![user("Locked", true)]);
    let mut carried = ProfilesScreen::new(EntryId(3), old.read());
    let mut observed = ProfilesScreen::new(EntryId(4), old.read());
    submit_locked(&mut carried, 0, InstanceId(17));
    submit_locked(&mut observed, 0, InstanceId(18));

    let switching = snapshot_at(201, Phase::Switching, vec![user("Locked", true)]);
    step_ev_with(
        &mut observed,
        &ScreenEvent::Tick(Tick {
            ms: 16,
            dt_us: 16_667,
        }),
        None,
        &switching,
        InstanceId(18),
    );

    for (screen, instance) in [
        (&mut carried, InstanceId(17)),
        (&mut observed, InstanceId(18)),
    ] {
        step_ev_with(
            screen,
            &ScreenEvent::Async(
                crate::ui::machine::RequestId(1),
                AppMsg::SelectionReply {
                    correlation: 1,
                    accepted: true,
                    flow_epoch: 201,
                },
            ),
            None,
            if instance == InstanceId(17) {
                &old
            } else {
                &switching
            },
            instance,
        );
        assert!(matches!(
            screen.pending_selection,
            Some(PendingSelection::Accepted {
                flow_epoch: 201,
                ..
            })
        ));
    }

    assert_eq!(
        carried.state.selection_correlation,
        observed.state.selection_correlation
    );
    assert_eq!(
        carried.state.selection_epoch,
        observed.state.selection_epoch
    );
    assert_ne!(
        Screen::<SessionHost>::state(&carried).hash(),
        Screen::<SessionHost>::state(&observed).hash(),
        "the same pending E=201 reduces differently with cached E=200/Profiles versus E=201/Switching"
    );
}

#[test]
fn root_back_uses_the_instance_address_and_checked_correlation_space() {
    let mut screen = bare(Pad::new());
    let (_, effects) = step_ev_with(
        &mut screen,
        &key_down(Key::Back, 0, 0),
        None,
        &EMPTY_SNAPSHOT,
        InstanceId(83),
    );
    assert!(effects.iter().any(|st| matches!(
        &st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { reply }))
            if reply.instance == 83 && reply.correlation == 1
    )));
    let (handled, _) = step_ev_with(
        &mut screen,
        &ScreenEvent::Async(
            crate::ui::machine::RequestId(1),
            AppMsg::BackReply {
                correlation: 1,
                resumed: false,
            },
        ),
        None,
        &EMPTY_SNAPSHOT,
        InstanceId(83),
    );
    assert_eq!(handled, Handled::Yes);

    screen.next_correlation = Some(u32::MAX);
    let (_, exhausted) = step_ev_with(
        &mut screen,
        &key_down(Key::Back, 0, 0),
        None,
        &EMPTY_SNAPSHOT,
        InstanceId(83),
    );
    assert!(exhausted
        .iter()
        .all(|st| !matches!(st.fx, Fx::App(AppFx::Session(_)))));
}

