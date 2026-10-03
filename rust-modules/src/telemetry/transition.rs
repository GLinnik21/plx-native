//! **The live side effects of a consent transition.**
//!
//! The app's owner decides a transition (`previous` -> `next`, or the end of an account's tenure)
//! and hands both sides over; this module is what the decision does to the resources telemetry
//! itself keeps: the published snapshot, the held sign-in events, the in-memory playback trace, the
//! spool, the native crash backend and the persisted record. `app::adapters::consent::
//! ConsentAdapter` calls it for the live case and records fixture effects instead for the rest.

use super::consent::Consent;

fn effectively_allows_errors(consent: &Consent) -> bool {
    consent.answered() && consent.errors
}

fn newly_enables_errors(previous: &Consent, next: &Consent) -> bool {
    effectively_allows_errors(next) && !effectively_allows_errors(previous)
}

/// Apply prospective cleanup, publish, purge withdrawn records and synchronize the native backend,
/// then persist. The canonical write is a storage-helper round trip on the television, so a
/// withdrawal must not wait behind it with the old decision still published (0.6.6 published at
/// once and wrote on its storage worker) — **and neither may this function's own caller**, the
/// frame loop's message dispatch (`app::bridge::AppRig::deliver`). The write itself is queued onto
/// `plx_base::storage_worker` for exactly that reason, same as `release/v0.6`'s
/// `record_with_receipt`; only the in-memory publish above and the spool/native side effects run
/// inline. A failed or refused write is logged and still honoured for this session; enabling
/// detection remains a function of the owner's explicit transition.
pub(crate) fn commit(previous: &Consent, next: &Consent) {
    let enabling_errors = newly_enables_errors(previous, next);
    if enabling_errors {
        crate::telemetry::crashreport::discard_pending_before_opt_in();
    }
    crate::telemetry::consent::install(next.clone());
    // Sign-in events held while the question was unanswered (`diag::event`) go through the
    // ordinary gate now that the decision is published: a "yes" lets them through with their
    // original stamp, a "no" drops them, and either way the held queue is empty afterwards.
    crate::diag::replay_deferred();
    if !next.errors {
        super::playback::clear_error_trace();
    }
    crate::telemetry::spool::purge_withdrawn(next);
    crate::telemetry::native::sync_change(next);
    persist_record_off_thread(next.clone());
}

/// Submit the canonical write to the shared persistence worker rather than running it on the
/// caller's thread. Dropping the returned ticket only cancels the caller's interest in the
/// result — per `storage_worker`'s own contract ("Dropping a ticket cancels interest, not an
/// already accepted durable write") — the job still runs to completion.
fn persist_record_off_thread(next: Consent) {
    let submitted = plx_base::storage_worker::submit(move || {
        let outcome = crate::telemetry::persistence::record(&next);
        if outcome.write != crate::telemetry::persistence::PersistResult::Durable {
            plx_base::eventlog::log(&format!("telemetry: the decision is not durably persisted: {outcome:?}"));
        }
    });
    if submitted.is_err() {
        plx_base::eventlog::log("telemetry: the decision could not be queued for persistence");
    }
    // Tests want the write's effect (and `persistence::last_call_thread()`) settled before the
    // next assertion; production has no such deadline and never drains.
    #[cfg(test)]
    plx_base::storage_worker::drain_for_test();
}

/// Publish the prospective default before touching disk, then erase every queued record and stop
/// native capture. The crash mark deliberately remains untouched by this path. The canonical
/// clear, like [`commit`]'s write, is queued off the frame thread rather than run inline.
///
/// This is sign-out and Delete all local data, not a withdrawal, so the spool is ERASED rather
/// than purged per category: a one-off report the departing account pressed Send for goes with it
/// (`spool::purge_all_local`), and `delivery::forget` first retires every in-flight one-off send and
/// the delivery states that would have shown a report's receipt.
pub(crate) fn forget(_prior: &Consent) {
    let next = Consent::default();
    crate::telemetry::consent::install(next.clone());
    // A held sign-in event belongs to the account whose attempt caused it, never to whoever signs
    // in next: drop it unreplayed.
    crate::diag::clear_deferred();
    super::playback::clear_error_trace();
    crate::telemetry::delivery::forget();
    crate::telemetry::spool::purge_all_local();
    crate::telemetry::native::sync_change(&next);
    persist_forget_off_thread();
}

fn persist_forget_off_thread() {
    let submitted = plx_base::storage_worker::submit(|| {
        let outcome = crate::telemetry::persistence::forget();
        if !matches!(
            outcome.write,
            crate::telemetry::persistence::PersistResult::Durable
                | crate::telemetry::persistence::PersistResult::Delegated
        ) {
            plx_base::eventlog::log(&format!("telemetry: sign-out could not durably clear the decision: {outcome:?}"));
        }
    });
    if submitted.is_err() {
        plx_base::eventlog::log("telemetry: sign-out could not be queued for persistence");
    }
    #[cfg(test)]
    plx_base::storage_worker::drain_for_test();
}

#[cfg(test)]
mod tests {
    use super::newly_enables_errors;
    use crate::telemetry::consent::{self, Consent};

    #[test]
    fn enabling_detection_uses_the_explicit_previous_decision() {
        let stale_yes = Consent {
            asked_version: consent::POLICY_VERSION.saturating_sub(1),
            errors: true,
            ..Consent::default()
        };
        let current_yes = Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            usage: false,
            install_id: None,
            errors_id: Some("current".into()),
            ..Default::default()
        };
        let current_no = Consent {
            asked_version: consent::POLICY_VERSION,
            ..Consent::default()
        };

        assert!(newly_enables_errors(&Consent::default(), &current_yes));
        assert!(newly_enables_errors(&stale_yes, &current_yes));
        assert!(!newly_enables_errors(&current_yes, &current_yes));
        assert!(!newly_enables_errors(&current_yes, &current_no));
    }
}
