//! PIN entry, the wrong-PIN flash, and the doors that close the pad.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ---------------------------------------------------------------------------------------
// the keypad grid's own hole-skip walk
// ---------------------------------------------------------------------------------------

#[test]
fn pin_edits_publish_logical_state_before_the_next_tick() {
    let mut s = bare(Pad::opened(2));
    s.state = s.snapshot_state(3);
    step_ev(&mut s, &key_down(Key::Other, b'7' as u32, 0), None);
    assert_eq!(s.state.pad_len, 1);
    assert_eq!(s.state.pad_target, 2);
    let typed = Screen::<SessionHost>::state(&s).hash();
    step_ev(&mut s, &ScreenEvent::Activate(pad_elem(3, 2)), None);
    assert_eq!(
        s.state.pad_len, 0,
        "delete must publish before returning early"
    );
    assert_ne!(Screen::<SessionHost>::state(&s).hash(), typed);
    assert_eq!(s.state.roster_n, 3);
}

#[test]
fn closing_pin_publishes_the_closed_state_before_the_next_tick() {
    let mut s = bare(Pad {
        entry: "12".into(),
        ..Pad::opened(2)
    });
    s.state = s.snapshot_state(3);
    step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
    assert!(!s.state.pad_open);
    assert_eq!(s.state.pad_len, 0);
    assert!(!s.state.pad_submitting);
    assert_eq!(s.state.roster_n, 3);
}

// ---------------------------------------------------------------------------------------
// pin_flash / digit_of — pure, ported verbatim from `ui/profiles.rs`
// ---------------------------------------------------------------------------------------

#[test]
fn a_pin_digit_is_read_from_either_field() {
    assert_eq!(digit_of(b'7' as u32, 0), Some(b'7'), "a dev keyboard's sym");
    assert_eq!(
        digit_of(0, 55),
        Some(b'7'),
        "the remote's wcode, same digit"
    );
    assert_eq!(digit_of(b'0' as u32, 0), Some(b'0'));
    assert_eq!(digit_of(999, 0), None, "not a digit in either field");
}

/// A 1.4s error window reads as several distinct red pulses, not one — the assertion a static
/// tint fails no matter how red it is. Ported from `ui/profiles.rs`'s own property test,
/// minus the `ui::idle` half (this screen reports through `Effects` instead — see the next
/// test for that half).
#[test]
fn the_wrong_pin_flash_blinks_at_least_four_times() {
    let dt = 1.0 / 60.0;
    let mut error_s = PIN_ERR_S;
    assert_eq!(pin_flash(error_s), Some(true), "opens LIT");
    let mut phases = vec![pin_flash(error_s)];
    let mut frames = 0;
    while error_s > 0.0 && frames < 600 {
        frames += 1;
        error_s = (error_s - dt).max(0.0);
        let now = pin_flash(error_s);
        if now != *phases.last().unwrap() {
            phases.push(now);
        }
    }
    assert_eq!(
        phases.last(),
        Some(&None),
        "the last report returns the dots to their resting ink"
    );
    let lit = phases.iter().filter(|p| **p == Some(true)).count();
    assert!(
        lit >= 4,
        "a 1.4s window must read as several pulses — got {lit} ({phases:?})"
    );
}

/// The flip reports to the dispatcher's `Present`, not to `ui::idle` — the new half of the
/// property above. Graded on a fresh `Present` (whose own `dirty` starts `true` — "the first
/// frame always draws" — so it is drained once before the assertion means anything).
#[test]
fn a_flash_flip_invalidates_the_present_gate() {
    let mut present = Present::new();
    present.take(0); // drain the fresh gate's own always-dirty first frame
    let mut pad = Pad {
        open: true,
        error_s: PIN_ERR_S,
        ..Pad::new()
    };
    let dt = 1.0 / 60.0;
    let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
    let mut saw_a_flip_invalidate = false;
    while pad.error_s > 0.0 {
        let was = pin_flash(pad.error_s);
        {
            let mut fx =
                Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
            ProfilesScreen::step_pin_flash(&mut pad, dt, &mut fx);
        }
        let now = pin_flash(pad.error_s);
        let asked = present.take(0);
        if now != was {
            assert!(
                asked,
                "the dot row changed colour and did not ask to be drawn"
            );
            saw_a_flip_invalidate = true;
        } else {
            assert!(!asked, "a dot row mid-phase asked for a repaint");
        }
    }
    assert!(
        saw_a_flip_invalidate,
        "the loop above never actually crossed a phase boundary"
    );
}

// ---------------------------------------------------------------------------------------
// the pad's own doors — BACK, a pointer miss, digit entry, submitting
// ---------------------------------------------------------------------------------------

/// BACK takes the keypad down and retires the PIN verdict with it — the one property
/// `ui/profiles.rs`'s own `every_door_out_of_the_keypad_goes_through_one_close` existed to
/// protect, ported onto the real `step`/`close_pad` rather than a hand-rolled `Pad::new()`.
///
/// **Pinned on `target: 2`, not 0** — with the pad opened on the FIRST avatar every past
/// version of this bug (`close_pad` re-seating through `Enter::Fresh { focus:
/// ContainerGroup(ROSTER_GROUP) }`, which resolves to the shelf's own `head_of` corner, tile
/// 0) would still pass by accident. A protected profile deep in the roster is the case that
/// actually distinguishes "closing returns to where the user was" from "closing always lands
/// on profile 0".
#[test]
fn back_closes_the_pad_and_clears_the_verdict() {
    let mut s = bare(Pad {
        open: true,
        target: 2,
        error_s: PIN_ERR_S,
        entry: "12".into(),
        ..Pad::new()
    });
    s.pin_denied = true;
    let (handled, effs) = step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
    assert_eq!(handled, Handled::Yes);
    assert!(!s.pad.open, "BACK takes the keypad down");
    assert_eq!(s.pad.error_s, 0.0, "…and the flash goes with it");
    assert!(
        !s.pin_denied,
        "…and the local verdict, which is what leaks onto the roster behind it"
    );
    assert!(effs.iter().any(|st| matches!(
        st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError))
    )));
    assert!(
        effs.iter().any(|st| matches!(
            &st.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                if k.elem == 2
        )),
        "closing returns focus to the PROTECTED profile the pad was opened on (index 2), \
         not wherever the roster's default seat lands"
    );
}

/// **The shared mechanism, tested directly.** All three doors out of the pad — BACK, a
/// pointer miss (both above/below), and `tick`'s own non-PIN-failure branch (`ProfilesScreen`'s
/// module doc, `close_pad`'s doc) — call this one function, so a fix or a regression in it
/// moves all three at once. The test exercises `close_pad` on its own terms: it is the guard
/// against the exact regression shape the bug had — reading
/// `self.pad.target` AFTER `self.pad = Pad::new()` has already zeroed it, which is invisible
/// to a caller and fails every door identically.
#[test]
fn close_pad_returns_focus_to_its_own_target_not_index_zero() {
    let mut present = Present::new();
    let mut buf: Vec<Stamped<SessionHost>> = Vec::new();
    let mut s = bare(Pad::opened(2));
    {
        let mut fx = Effects::new(&mut buf, MachineId::Instance(InstanceId(0)), &mut present);
        s.close_pad(&mut fx);
    }
    assert!(!s.pad.open);
    assert!(
        buf.iter().any(|st| matches!(
            &st.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                if k.elem == 2
        )),
        "close_pad must capture pad.target BEFORE replacing self.pad, or every caller reads back 0"
    );
}

/// With the pad CLOSED, every BACK is the typed Session root request.
#[test]
fn back_with_the_pad_closed_asks_the_loop_for_the_root_press() {
    let mut s = bare(Pad::new());
    let (handled, effs) = step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
    assert_eq!(handled, Handled::Yes);
    assert!(
        effs.iter().any(|st| matches!(
            &st.fx,
            Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { reply }))
                if reply.instance == 0 && reply.correlation == 1
        )),
        "the picker has no panel of its own to close first — BACK asks Session for the root press"
    );
}

/// The original transition-specific rule: Switching does not change root BACK, while an open
/// PIN pad still owns the first BACK locally. Both halves run on `ProfilesScreen::step` with
/// the same real Switching publication, so phase and pad state cannot be proved in isolation.
#[test]
fn switching_preserves_the_picker_root_back_and_pin_pad_split() {
    let switching = snapshot_at(23, Phase::Switching, vec![user("Locked", true)]);
    let mut root = ProfilesScreen::new(EntryId(9), switching.read());
    let (handled, effects) = step_ev_with(&mut root, &key_down(Key::Back, 0, 0), None,
        &switching, InstanceId(44));
    assert_eq!(handled, Handled::Yes);
    assert!(effects.iter().any(|stamped| matches!(
        &stamped.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { reply }))
            if reply.instance == 44 && reply.correlation == 1
    )), "a closed Switching picker must ask Session for root BACK");

    let mut pad = ProfilesScreen::new(EntryId(9), switching.read());
    pad.pad = Pad { open: true, target: 0, error_s: PIN_ERR_S, entry: "12".into(),
        ..Pad::new() };
    pad.pin_denied = true;
    let (handled, effects) = step_ev_with(&mut pad, &key_down(Key::Back, 0, 0), None,
        &switching, InstanceId(44));
    assert_eq!(handled, Handled::Yes);
    assert!(!pad.pad.open);
    assert!(!pad.pin_denied);
    assert!(effects.iter().all(|stamped| !matches!(
        stamped.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::BackAtRoot { .. }))
    )), "the open PIN pad spends BACK locally even during Switching");
    assert!(effects.iter().any(|stamped| matches!(
        stamped.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError))
    )));
}

/// A pointer click that resolves onto NONE of the pad's twelve stops (`hit: None`) is the
/// pointer's own door out — the module doc's account of why this has to be answered from the
/// raw `Click` event rather than a library `OnMiss` policy.
#[test]
fn a_pointer_click_outside_every_pad_stop_closes_it_too() {
    // `target: 2`, not 0 — see `back_closes_the_pad_and_clears_the_verdict`'s doc for why the
    // first avatar cannot distinguish a real fix from the old default-seat bug.
    let mut s = bare(Pad {
        open: true,
        target: 2,
        error_s: PIN_ERR_S,
        ..Pad::new()
    });
    s.pin_denied = true;
    let (handled, effs) = step_ev(&mut s, &click(None), None);
    assert_eq!(handled, Handled::Yes);
    assert!(!s.pad.open, "the pointer's miss takes the keypad down too");
    assert!(
        !s.pin_denied,
        "the pointer's door owes the SAME local cleanup BACK's does"
    );
    assert!(effs.iter().any(|st| matches!(
        st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError))
    )));
    assert!(
        effs.iter().any(|st| matches!(
            &st.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                if k.elem == 2
        )),
        "the pointer's door owes the SAME focus-return BACK's does — back to profile 2, not profile 0"
    );
    // …and a click that DID hit a cell must not also close the pad — that is the Activate
    // event's job, not this one's.
    let mut s2 = bare(Pad::opened(0));
    let (_, _) = step_ev(&mut s2, &click(Some(pad_elem(0, 0))), None);
    assert!(s2.pad.open, "a hit is not a miss");
}

/// A number key types straight into the entry without moving the visual cursor. This focused
/// edit test stops at three digits; typed four-digit command emission is covered above.
#[test]
fn a_digit_key_types_into_the_entry_and_backspace_removes_one() {
    let mut s = bare(Pad::opened(0));
    step_ev(&mut s, &key_down(Key::Other, b'1' as u32, 0), None);
    step_ev(&mut s, &key_down(Key::Other, b'2' as u32, 0), None);
    assert_eq!(s.pad.entry, "12");
    assert!(!s.pad.submitting, "only three digits so far");
    step_ev(
        &mut s,
        &key_down(Key::Other, 0, 55 /* '7' by wcode */),
        None,
    );
    assert_eq!(s.pad.entry, "127");
    // backspace via the delete cell's OWN digit path is not reachable through `digit_of` (it
    // is not in 48..=57) — `press` handles it directly, exercised below through `Activate`.
}

/// Backspace and typing-again both cancel a running wrong-PIN flash — `press`'s own first
/// line, ported from `ui/profiles.rs`.
#[test]
fn typing_again_cancels_a_running_wrong_pin_flash() {
    let mut s = bare(Pad {
        open: true,
        error_s: PIN_ERR_S,
        entry: "1".into(),
        ..Pad::new()
    });
    step_ev(&mut s, &key_down(Key::Other, b'2' as u32, 0), None);
    assert_eq!(s.pad.error_s, 0.0);
}

/// While a PIN is submitting, only BACK acts — every digit is swallowed with no effect on the
/// entry, matching `ui/profiles.rs::pad_key`'s own early return.
#[test]
fn only_back_acts_while_a_pin_is_submitting() {
    let mut s = bare(Pad {
        open: true,
        submitting: true,
        entry: "123".into(),
        ..Pad::new()
    });
    let (handled, _) = step_ev(&mut s, &key_down(Key::Other, b'4' as u32, 0), None);
    assert_eq!(
        handled,
        Handled::Yes,
        "swallowed, not ignored — nothing behind the pad may act on it"
    );
    assert_eq!(s.pad.entry, "123", "the digit changed nothing");
    assert!(s.pad.submitting);
}

/// The keypad's Bare `Activate` — OK on a focused cell, or its pointer twin — presses that
/// exact cell regardless of which key produced the event, exercised through the digit '5' at
/// (1, 1) and the backspace at (3, 2).
#[test]
fn activating_a_pad_cell_presses_that_cell() {
    let mut s = bare(Pad::opened(0));
    step_ev(&mut s, &ScreenEvent::Activate(pad_elem(1, 1)), None); // '5'
    assert_eq!(s.pad.entry, "5");
    step_ev(&mut s, &ScreenEvent::Activate(pad_elem(3, 2)), None); // delete
    assert_eq!(s.pad.entry, "");
    // the hole itself has no key behind it — activating it (which nothing on screen can ever
    // focus, since `pad_place`/`groups` never offer it) must still be a harmless no-op
    step_ev(&mut s, &ScreenEvent::Activate(pad_elem(3, 0)), None);
    assert_eq!(s.pad.entry, "");
}

/// The flash OPENS LIT and ENDS DARK, with several distinct pulses in between. The sibling
/// `the_wrong_pin_flash_blinks_at_least_four_times` walks the same window frame by frame; this
/// one adds the endpoint check `main`'s draft made explicit — the loop's own guard samples only
/// positive times, so the terminal `None` is asked for separately rather than assumed.
#[test]
fn pin_flash_opens_lit_and_blinks_out() {
    assert_eq!(pin_flash(0.0), None, "no flash running");
    assert_eq!(
        pin_flash(PIN_ERR_S),
        Some(true),
        "a rejected PIN opens red, not dim"
    );
    let mut phases = Vec::new();
    let mut t = PIN_ERR_S;
    let dt = 1.0 / 60.0;
    while t > 0.0 {
        let now = pin_flash(t);
        if phases.last() != Some(&now) {
            phases.push(now);
        }
        t = (t - dt).max(0.0);
    }
    // The loop's guard samples only positive times; record the exact endpoint explicitly so
    // the assertion covers the production function's terminal `None` state as well.
    let end = pin_flash(0.0);
    if phases.last() != Some(&end) {
        phases.push(end);
    }
    assert_eq!(
        phases.last(),
        Some(&None),
        "the flash ends dark, not stuck lit"
    );
    let lit = phases.iter().filter(|p| **p == Some(true)).count();
    assert!(
        lit >= 4,
        "a 1.4s window must read as several distinct pulses, got {phases:?}"
    );
}

/// A digit typed anywhere on the pad reaches [`ProfilesScreen::press`] through the SAME real
/// `step()` the dispatcher calls, whichever cell happens to hold focus — typing does not depend
/// on navigating to the key first.
///
/// `main`'s draft expressed "which cell holds focus" by seeding a `Pad::fr`/`fc` render mirror;
/// this file has no such mirror (the engine owns the focused cell), so the same condition is
/// stated where it now lives — `cx.focus.current`, parked on the pad's bottom-right cell.
#[test]
fn a_typed_digit_reaches_the_pad_regardless_of_which_cell_holds_focus() {
    let mut s = bare(Pad::opened(0));
    let parked = Some(FocusKey {
        entry: s.entry,
        elem: pad_elem(2, 2),
    });
    let (handled, _) = step_ev(&mut s, &key_down(Key::Other, b'5' as u32, 0), parked);
    assert_eq!(handled, Handled::Yes);
    assert_eq!(
        s.pad.entry, "5",
        "the digit typed regardless of the focused cell (2,2)"
    );
}

/// Every door out of the pad clears the PIN verdict — BACK, from `main`'s draft of
/// `every_door_out_of_the_keypad_goes_through_one_close`'s first door. Kept alongside the
/// sibling `back_closes_the_pad_and_clears_the_verdict`, which pins the strictly stronger
/// `target: 2` case: this one is the `target: 0` statement as `main` wrote it, and the two
/// together say that closing re-seats on the pad's OWN avatar whether or not that avatar
/// happens to be the roster's default seat.
#[test]
fn back_closes_the_pad_and_clears_the_pin_verdict() {
    let mut s = bare(Pad {
        open: true,
        target: 0,
        error_s: PIN_ERR_S,
        ..Pad::new()
    });
    s.pin_denied = true;
    let (handled, effs) = step_ev(&mut s, &key_down(Key::Back, 0, 0), None);
    assert_eq!(handled, Handled::Yes);
    assert!(!s.pad.open, "BACK takes the pad down");
    assert!(
        !s.pin_denied,
        "…and the local verdict, which is what would leak onto the roster behind it"
    );
    assert!(effs.iter().any(|st| matches!(
        st.fx,
        Fx::App(AppFx::Session(auth::SessionCmd::DismissPinError))
    )));
    assert!(
        effs.iter().any(|st| matches!(
            &st.fx,
            Fx::Deliver(_, Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::Elem(k) })))
                if k.elem == 0
        )),
        "closing re-seats the engine on the avatar whose pad this was, not on the group's default corner"
    );
}

