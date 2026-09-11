//! Local UI owner for the user-confirmed native sandbox repair.

use crate::player::FailureKind;
use crate::ui::decision_alert::{Choice, DecisionAlert, Tone};
#[cfg(any(feature = "devtriggers", test))]
use crate::webos::jail_repair::Failure;
use crate::webos::jail_repair::State;

const BODY: &str = "Use Homebrew Channel’s root access to update PlxNative’s sandbox with LG’s native profile. This requires a rooted TV. Close and reopen PlxNative afterward.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PrimaryAction {
    Quality,
    Repair,
    None,
}

/// The single routing rule shared by key and pointer failure activation.
pub(crate) fn primary_action(kind: FailureKind, repair: State) -> PrimaryAction {
    if kind != FailureKind::JailMissingRtkmem {
        PrimaryAction::Quality
    } else if repair == State::Idle {
        PrimaryAction::Repair
    } else {
        PrimaryAction::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlertIntent {
    Consume,
    Dismiss,
    Confirm,
}

fn alert_intent(open: bool, choice: Choice, ok: bool, back: bool) -> AlertIntent {
    if !open {
        AlertIntent::Consume
    } else if back || (ok && choice == Choice::Cancel) {
        AlertIntent::Dismiss
    } else if ok {
        AlertIntent::Confirm
    } else {
        AlertIntent::Consume
    }
}

pub(crate) struct Controller {
    alert: DecisionAlert,
    last_state: State,
    fixture: Option<State>,
}

impl Controller {
    pub(crate) fn new() -> Self {
        let mut alert = DecisionAlert::new();
        alert.set_tone(Tone::Neutral);
        let fixture = fixture();
        Self {
            alert,
            last_state: fixture.unwrap_or_else(crate::webos::jail_repair::snapshot),
            fixture,
        }
    }

    pub(crate) fn state(&self) -> State {
        self.fixture
            .unwrap_or_else(crate::webos::jail_repair::snapshot)
    }

    pub(crate) fn visible(&self) -> bool {
        self.alert.visible()
    }

    pub(crate) fn open(&mut self) {
        if self.state() == State::Idle && !self.alert.visible() {
            self.alert
                .open_with_body(c"Repair PlxNative’s sandbox?", BODY);
        }
    }

    pub(crate) fn update(&mut self, subject_current: bool, dt: f32) {
        if !subject_current {
            if self.alert.visible() {
                self.alert.close();
            }
            return;
        }
        let state = self.state();
        if state != self.last_state {
            self.last_state = state;
            crate::ui::idle::invalidate();
        }
        self.alert.update(dt);
    }

    /// Returns true whenever this alert owns the key, including its dismissal fade.
    pub(crate) fn key(
        &mut self,
        mt: &crate::task::MainThread,
        left: bool,
        right: bool,
        ok: bool,
        back: bool,
    ) -> bool {
        if !self.alert.visible() {
            return false;
        }
        match alert_intent(self.alert.is_open(), self.alert.choice(), ok, back) {
            AlertIntent::Dismiss => self.alert.dismiss(),
            AlertIntent::Confirm => {
                self.alert.dismiss();
                if self.fixture.is_none() {
                    let _ = crate::webos::jail_repair::request(mt);
                }
            }
            AlertIntent::Consume => {
                if self.alert.is_open() {
                    if left {
                        self.alert.move_focus(-1);
                    } else if right {
                        self.alert.move_focus(1);
                    }
                }
            }
        }
        true
    }

    /// Pointer activation is positional only after the alert has reached its final geometry.
    pub(crate) fn press_at(&mut self, mt: &crate::task::MainThread, x: f32, y: f32) -> bool {
        if !self.alert.is_open() || !self.alert.settled() {
            return self.alert.visible();
        }
        if !self.alert.press_at(x, y) {
            return true;
        }
        match self.alert.choice() {
            Choice::Cancel => self.alert.dismiss(),
            Choice::Destructive => {
                self.alert.dismiss();
                if self.fixture.is_none() {
                    let _ = crate::webos::jail_repair::request(mt);
                }
            }
        }
        true
    }

    pub(crate) fn draw(&mut self) {
        self.alert.draw_scrim();
        self.alert.draw(c"Cancel", c"Repair sandbox");
    }
}

#[cfg(feature = "devtriggers")]
fn fixture() -> Option<State> {
    crate::dev::read("jailrepair-view").and_then(|name| fixture_state(&name))
}

#[cfg(feature = "devtriggers")]
fn fixture_state(name: &str) -> Option<State> {
    match name {
        "running" => Some(State::Running),
        "repaired" => Some(State::Repaired),
        "unavailable" => Some(State::Failed(Failure::HbcUnavailable)),
        "not_root" => Some(State::Failed(Failure::NotRoot)),
        "timeout" => Some(State::Failed(Failure::Timeout)),
        _ => None,
    }
}

#[cfg(not(feature = "devtriggers"))]
fn fixture() -> Option<State> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_idle_jail_failure_routes_to_repair() {
        assert_eq!(
            primary_action(FailureKind::JailMissingRtkmem, State::Idle),
            PrimaryAction::Repair
        );
        for state in [
            State::Running,
            State::Repaired,
            State::Failed(Failure::Timeout),
        ] {
            assert_eq!(
                primary_action(FailureKind::JailMissingRtkmem, state),
                PrimaryAction::None
            );
        }
        assert_eq!(
            primary_action(FailureKind::TvPipeline, State::Idle),
            PrimaryAction::Quality
        );
        assert_eq!(
            primary_action(FailureKind::TvPipeline, State::Running),
            PrimaryAction::Quality
        );
    }

    #[test]
    fn cancel_choice_and_back_never_confirm() {
        assert_eq!(
            alert_intent(true, Choice::Cancel, true, false),
            AlertIntent::Dismiss
        );
        assert_eq!(
            alert_intent(true, Choice::Destructive, false, true),
            AlertIntent::Dismiss
        );
        assert_eq!(
            alert_intent(true, Choice::Destructive, true, false),
            AlertIntent::Confirm
        );
        assert_eq!(
            alert_intent(false, Choice::Destructive, true, false),
            AlertIntent::Consume
        );
    }

    #[test]
    #[cfg(feature = "devtriggers")]
    fn display_fixtures_are_closed_and_never_offer_a_repair_action() {
        for name in ["running", "repaired", "unavailable", "not_root", "timeout"] {
            let state = fixture_state(name).expect("documented fixture");
            assert_eq!(
                primary_action(FailureKind::JailMissingRtkmem, state),
                PrimaryAction::None
            );
        }
        assert_eq!(fixture_state("repair"), None);
        assert_eq!(fixture_state("anything"), None);
    }
}
