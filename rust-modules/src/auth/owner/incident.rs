//! **The onboarding incident offer: one small state machine per failure, owned by Session.**
//!
//! A sign-in failure used to be a caption and a *Try again*. It is now also an [`IncidentOffer`]:
//! the closed evidence of what failed (`telemetry::incident::IncidentContext`, never the caption
//! text) and where the question "may this be reported?" has got to. Screens only render it and
//! send [`Command`]s; every transition is here.
//!
//! ```text
//!   ★ failure ─► Pending ──ResolveIncident(permission)──┬─► Offered ──Not now──► NotNow
//!                                                      │      │ Send report     (alert kinds,
//!                                                      │      ▼                  NotDetermined)
//!                                                      │   Sending ──► Queued{receipt} | Failed
//!                                                      ├─► AutoSending ─► Queued{receipt} | Failed
//!                                                      │                                (Granted)
//!                                                      ├─► OnRequest       (LinkStalled, or a key
//!                                                      │                    seen this launch;
//!                                                      │                    not Declined)
//!                                                      └─► Dropped                    (Declined)
//!   Queued{receipt} ──held (not through yet, still spooled)──► Saved{receipt}
//!   Queued / Saved{receipt} ──a server accepted it──► Delivered{receipt}
//!   Queued / Saved{receipt} ──dropped (refused, discarded, fallback failed)──► Failed
//!   OnRequest / NotNow / Failed / Dropped / Pending ──Send report (Details)──► Sending
//! ```
//!
//! * **Permission is an INPUT, not a read.** This owner is pure and replayable, so the consent
//!   decision reaches it as [`Command::ResolveIncident`], sent by the screen that would present the
//!   offer, together with the `consent::revision` it was derived at. An Offered incident is
//!   re-resolved whenever that revision moves: a Yes at the onboarding scope turns it into a
//!   standing send, a No drops it.
//! * **Dedup is per LAUNCH, keyed on (flow, kind, link).** A resolved key is remembered in
//!   `SessionInit::incidents_seen`; `restart_login` mints a new epoch on every *Try again*, and a
//!   per-epoch key would ask the same question after every press. The failure already held keeps
//!   its offer and its answer. A seen key coming back after a DIFFERENT failure is raised again —
//!   the read-out must never show another failure's Details — but resolved quietly: OnRequest,
//!   behind Details only, never the alert and never a standing send.
//! * **A newer incident supersedes** whatever is held — never a second alert. A reply for the
//!   superseded one is fenced by its id.
//! * **Every sign-in ending names its incident** (`SessionMachine::fail_login`): a worker's
//!   failure carries its own evidence, an ending inside the app raises
//!   `IncidentKind::Internal`, and an ending the report does not cover retires what is held.
//! * **Declined keeps nothing.** A dropped offer forgets its context; *Details → Send report*
//!   builds one at press time from the key alone ([`IncidentReport::AtPress`]).
//! * **A stalled wait is offered through Details only** (`IncidentKind::alert_eligible`,
//!   `IncidentKind::standing_eligible`): the QR code is still on screen, possibly mid-scan, so no
//!   permission puts the alert over it, and none sends it automatically. Granted and undetermined
//!   both resolve it to OnRequest — the status line and *Details → Send report*, a one-off sent
//!   only on that press. Declined still drops it. OnRequest is re-resolved on a revision move, so
//!   a later No forgets its evidence just as it would an Offered one's.
//! * **Queued is not delivered.** Both lanes hand back the report's event id when they TAKE it;
//!   what became of it afterwards is observed by the session adapter (`telemetry::delivery`) and
//!   arrives as [`IncidentDelivery::Held`], [`IncidentDelivery::Delivered`] or
//!   [`IncidentDelivery::Undelivered`], each fenced by the id AND the receipt it is about. The
//!   screen only renders the state; it never samples telemetry.
//! * Sign-out and *Delete all local data* erase the offer and the seen keys with everything else.

use super::{Command, SessionFx, SessionMachine};
use crate::telemetry::consent::Permission;
use crate::telemetry::incident::{IncidentContext, IncidentKind, LinkClass};
use crate::ui::machine::Canon;
use serde::{Deserialize, Serialize};

/// Which onboarding flow an incident belongs to. Only sign-in raises one in this stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IncidentFlow {
    SignIn,
}

/// The per-launch dedup key: the same failure over the same kind of link is one question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IncidentKey {
    pub flow: IncidentFlow,
    pub kind: IncidentKind,
    pub link: LinkClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IncidentState {
    /// Raised, not yet resolved against the consent decision — nothing is shown for it yet.
    Pending,
    /// The alert is being offered, as resolved at this `consent::revision`.
    Offered { revision: u32 },
    /// Offered behind *Details* only — never by the alert, never sent without a press — as
    /// resolved at this `consent::revision`. The resolution for a kind that is not
    /// `IncidentKind::alert_eligible`.
    OnRequest { revision: u32 },
    /// Granted: the standing report is being queued.
    AutoSending,
    /// The person pressed Send report; the one-off is being queued.
    Sending,
    /// Queued for delivery. `receipt` is the report's event id, the Report ID the person can
    /// quote — a one-off's or a standing report's alike.
    Queued { receipt: String },
    /// A send was tried and did not get through for a reason that may pass; the report is still
    /// queued durably and a later flush tries again ([`IncidentDelivery::Held`]).
    Saved { receipt: String },
    /// A server accepted the report ([`IncidentDelivery::Delivered`]).
    Delivered { receipt: String },
    /// The report could not be queued, or a queued one was dropped after all — refused, discarded
    /// unsent, or a one-off's direct fallback failed ([`IncidentDelivery::Undelivered`]). Send
    /// report is accepted again.
    Failed,
    /// The person answered Not now. Details still offers Send report.
    NotNow,
    /// The decision is No: nothing kept, nothing offered. Details still offers Send report.
    Dropped,
}

/// One incident and where its report has got to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct IncidentOffer {
    /// Owner-allocated, never reused in a launch — fences a reply for a superseded offer.
    pub id: u32,
    pub key: IncidentKey,
    /// The closed evidence. `None` once Dropped: a No keeps nothing.
    pub context: Option<IncidentContext>,
    pub state: IncidentState,
}

impl IncidentOffer {
    /// Whether a person's Send report press is accepted in this state.
    pub(crate) fn sendable(&self) -> bool {
        matches!(
            self.state,
            IncidentState::Pending
                | IncidentState::Offered { .. }
                | IncidentState::OnRequest { .. }
                | IncidentState::NotNow
                | IncidentState::Failed
                | IncidentState::Dropped
        )
    }
}

/// Which lane a report leaves by: the standing Errors lane (Granted, no press) or a one-off.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IncidentLane {
    Standing,
    OneOff,
}

/// What a report is built from when the adapter executes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IncidentReport {
    /// The evidence captured when the failure happened.
    Retained(IncidentContext),
    /// Nothing was kept (the decision was No): the adapter builds a context now, from the key.
    AtPress(IncidentKey),
}

/// The adapter's answer for one [`SessionFx::Incident`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum IncidentDelivery {
    /// The standing report's event id once the spool took it; `None` when nothing was queued.
    Standing { receipt: Option<String> },
    /// The one-off's event id once its lane took it; `None` when nothing was.
    OneOff { receipt: Option<String> },
    /// A report the lane had taken (`receipt`) will never reach the server — refused, discarded
    /// unsent, or a one-off's direct fallback failed (`telemetry::delivery::DeliveryState::Failed`).
    /// Observed by the adapter after the fact, so it is fenced by the receipt as well as the id.
    Undelivered { receipt: String },
    /// The report (`receipt`) was tried and did not get through yet; it is still queued durably
    /// (`DeliveryState::Held`).
    Held { receipt: String },
    /// A server accepted the report (`receipt`) (`DeliveryState::Delivered`).
    Delivered { receipt: String },
}

/// How many distinct keys a launch remembers. The closed kind × link space is small; the bound
/// only stops a pathological loop from growing state without limit.
const SEEN_CAP: usize = 32;

/// Has the question about this offer been settled — answered, sent, or decided by the consent?
fn is_settled(offer: &IncidentOffer) -> bool {
    !matches!(
        offer.state,
        IncidentState::Pending | IncidentState::Offered { .. } | IncidentState::OnRequest { .. }
    )
}

pub(super) fn write_kind(w: &mut Canon, kind: IncidentKind) {
    w.str(kind.code());
}

pub(super) fn write_key(w: &mut Canon, key: &IncidentKey) {
    w.u8(match key.flow {
        IncidentFlow::SignIn => 0,
    });
    write_kind(w, key.kind);
    w.str(key.link.code());
}

pub(super) fn write_context(w: &mut Canon, c: &IncidentContext) {
    use crate::telemetry::incident::keymanager_stage_code;
    write_kind(w, c.kind);
    w.str(c.link.code());
    w.option(c.http_status, |w, s| {
        w.u32(u32::from(s));
    });
    w.option(c.curl_rc, |w, rc| {
        w.u32(rc as u32);
    });
    w.str(c.unanswered.code()).str(c.failing_for.code());
    w.option(c.code_generation, |w, g| {
        w.u8(g);
    });
    w.option(c.persistence, |w, p| {
        w.str(p.code());
    });
    w.option(c.keymanager_stage, |w, s| {
        w.str(keymanager_stage_code(s));
    });
    w.option(c.service_error_code, |w, e| {
        w.u32(e as u32);
    });
    w.u64(c.occurred_at_ms);
}

pub(super) fn write_offer(w: &mut Canon, offer: &IncidentOffer) {
    w.u32(offer.id);
    write_key(w, &offer.key);
    w.option(offer.context.as_ref(), |w, c| write_context(w, c));
    match &offer.state {
        IncidentState::Pending => {
            w.u8(0);
        }
        IncidentState::Offered { revision } => {
            w.u8(1).u32(*revision);
        }
        IncidentState::AutoSending => {
            w.u8(2);
        }
        IncidentState::Sending => {
            w.u8(3);
        }
        IncidentState::Queued { receipt } => {
            w.u8(4).str(receipt);
        }
        IncidentState::Failed => {
            w.u8(5);
        }
        IncidentState::NotNow => {
            w.u8(6);
        }
        IncidentState::Dropped => {
            w.u8(7);
        }
        IncidentState::OnRequest { revision } => {
            w.u8(8).u32(*revision);
        }
        IncidentState::Saved { receipt } => {
            w.u8(9).str(receipt);
        }
        IncidentState::Delivered { receipt } => {
            w.u8(10).str(receipt);
        }
    }
}

impl SessionMachine {
    /// A producer's failure, with its closed evidence. See the module doc for the rules: the
    /// failure already held keeps its offer and its answer; anything else supersedes what is
    /// held — a key this launch has already resolved included, which is then resolved quietly.
    pub(super) fn raise_incident(&mut self, flow: IncidentFlow, context: IncidentContext) {
        let key = IncidentKey { flow, kind: context.kind, link: context.link };
        if let Some(held) = self.state.incident.as_mut().filter(|held| held.key == key) {
            // The same failure again. Unresolved (a second stalled report in one wait): keep the
            // offer, take the fresher evidence. Resolved: keep the answer it already has.
            if !is_settled(held) {
                held.context = Some(context);
            }
            return;
        }
        self.state.next_incident = self.state.next_incident.wrapping_add(1).max(1);
        self.state.incident = Some(IncidentOffer {
            id: self.state.next_incident,
            key,
            context: Some(context),
            state: IncidentState::Pending,
        });
    }

    /// The offer's permission, derived by the presenting screen at `revision`.
    pub(super) fn resolve_incident(
        &mut self,
        id: u32,
        permission: Permission,
        revision: u32,
        emit: &mut impl FnMut(SessionFx),
    ) -> bool {
        let seen = &self.state.incidents_seen;
        let Some(held) = self.state.incident.as_mut().filter(|held| held.id == id) else { return false };
        match held.state {
            IncidentState::Pending => {}
            IncidentState::Offered { revision: at } | IncidentState::OnRequest { revision: at }
                if at != revision => {}
            _ => return false,
        }
        let key = held.key;
        // Offered behind Details only: a kind the alert never covers, a key this launch has
        // already asked about (the raise that brought it back is Pending over a seen key), and
        // an offer already resolved that way — a later decision may drop it, never escalate it.
        let quiet = !key.kind.alert_eligible()
            || matches!(held.state, IncidentState::OnRequest { .. })
            || (held.state == IncidentState::Pending && seen.contains(&key));
        match permission {
            Permission::Declined => {
                held.state = IncidentState::Dropped;
                held.context = None;
            }
            Permission::Granted | Permission::NotDetermined if quiet => {
                held.state = IncidentState::OnRequest { revision };
            }
            Permission::Granted if key.kind.standing_eligible() => {
                let Some(context) = held.context else { return false };
                held.state = IncidentState::AutoSending;
                emit(SessionFx::Incident { id, lane: IncidentLane::Standing, report: IncidentReport::Retained(context) });
            }
            Permission::Granted | Permission::NotDetermined => {
                held.state = IncidentState::Offered { revision };
            }
        }
        self.remember_seen(key);
        true
    }

    /// The person's Send report — from the alert, or from Details afterwards.
    pub(super) fn report_incident(&mut self, id: u32, emit: &mut impl FnMut(SessionFx)) -> bool {
        let Some(held) = self.state.incident.as_mut().filter(|held| held.id == id && held.sendable()) else {
            return false;
        };
        let report = match held.context {
            Some(context) => IncidentReport::Retained(context),
            None => IncidentReport::AtPress(held.key),
        };
        let key = held.key;
        held.state = IncidentState::Sending;
        emit(SessionFx::Incident { id, lane: IncidentLane::OneOff, report });
        self.remember_seen(key);
        true
    }

    /// The person's Not now. Only an offer on screen can be answered.
    pub(super) fn decline_incident(&mut self, id: u32) -> bool {
        let Some(held) = self.state.incident.as_mut().filter(|held| held.id == id) else { return false };
        if !matches!(held.state, IncidentState::Offered { .. }) {
            return false;
        }
        held.state = IncidentState::NotNow;
        true
    }

    /// The adapter's answer. Fenced by id: a reply for a superseded offer changes nothing. What
    /// became of a queued report is fenced by its receipt as well — a different receipt is a report
    /// this offer no longer shows.
    pub(super) fn incident_reported(&mut self, id: u32, delivery: &IncidentDelivery) -> bool {
        let Some(held) = self.state.incident.as_mut().filter(|held| held.id == id) else { return false };
        let shown = match &held.state {
            IncidentState::Queued { receipt } | IncidentState::Saved { receipt } => Some(receipt.as_str()),
            _ => None,
        };
        held.state = match (&held.state, delivery) {
            (IncidentState::AutoSending, IncidentDelivery::Standing { receipt: Some(receipt) })
            | (IncidentState::Sending, IncidentDelivery::OneOff { receipt: Some(receipt) }) => {
                IncidentState::Queued { receipt: receipt.clone() }
            }
            (IncidentState::AutoSending, IncidentDelivery::Standing { receipt: None })
            | (IncidentState::Sending, IncidentDelivery::OneOff { receipt: None }) => IncidentState::Failed,
            // Tried and not through yet, still queued durably: said once, from Queued.
            (IncidentState::Queued { .. }, IncidentDelivery::Held { receipt }) if shown == Some(receipt) => {
                IncidentState::Saved { receipt: receipt.clone() }
            }
            (_, IncidentDelivery::Delivered { receipt }) if shown == Some(receipt) => {
                IncidentState::Delivered { receipt: receipt.clone() }
            }
            // The report did not get through after all: Send report is accepted again.
            (_, IncidentDelivery::Undelivered { receipt }) if shown == Some(receipt) => IncidentState::Failed,
            _ => return false,
        };
        true
    }

    /// Sign-out and Delete all local data: every incident, and what this launch has seen.
    pub(super) fn forget_incidents(&mut self) {
        self.state.incident = None;
        self.state.incidents_seen.clear();
        self.state.link_trouble = false;
    }

    fn remember_seen(&mut self, key: IncidentKey) {
        if self.state.incidents_seen.contains(&key) {
            return;
        }
        if self.state.incidents_seen.len() >= SEEN_CAP {
            self.state.incidents_seen.remove(0);
        }
        self.state.incidents_seen.push(key);
    }

    pub(super) fn step_incident_command(
        &mut self,
        command: &Command,
        emit: &mut impl FnMut(SessionFx),
    ) -> bool {
        match command {
            Command::ResolveIncident { id, permission, revision } => {
                self.resolve_incident(*id, *permission, *revision, emit)
            }
            Command::ReportIncident { id } => self.report_incident(*id, emit),
            Command::DeclineIncident { id } => self.decline_incident(*id),
            _ => false,
        }
    }
}

#[cfg(test)]
#[path = "incident_tests.rs"]
mod tests;
