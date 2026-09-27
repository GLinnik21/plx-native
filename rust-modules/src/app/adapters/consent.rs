//! Per-application consent resources. The owner supplies both sides of every logical transition;
//! this adapter only performs persistence/publication side effects or records fixture effects.

use crate::telemetry::consent::Consent;

enum Resources {
    Live,
    #[cfg(test)]
    Fixture(FixtureResources),
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct FixtureResources {
    pub transitions: Vec<(Consent, Consent)>,
    pub forgotten: Vec<Consent>,
}

pub(crate) struct ConsentAdapter {
    resources: Resources,
}

impl ConsentAdapter {
    pub(crate) fn live() -> Self {
        Self {
            resources: Resources::Live,
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture() -> Self {
        Self {
            resources: Resources::Fixture(FixtureResources::default()),
        }
    }

    /// Commit an already-decided transition. In particular, the live resource path receives the
    /// owner's previous value instead of consulting the process-global publication as a second
    /// logical authority.
    pub(crate) fn commit(&mut self, previous: &Consent, next: &Consent) {
        match &mut self.resources {
            Resources::Live => commit_live(previous, next),
            #[cfg(test)]
            Resources::Fixture(resources) => {
                resources.transitions.push((previous.clone(), next.clone()));
            }
        }
    }

    /// End the prior account's tenure over telemetry. The prior state is explicit for the owner
    /// boundary and for fixture evidence; live erasure remains prospective.
    pub(crate) fn forget(&mut self, prior: &Consent) {
        match &mut self.resources {
            Resources::Live => forget_live(prior),
            #[cfg(test)]
            Resources::Fixture(resources) => resources.forgotten.push(prior.clone()),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture_resources(&self) -> &FixtureResources {
        match &self.resources {
            Resources::Fixture(resources) => resources,
            Resources::Live => panic!("live ConsentAdapter has no fixture resources"),
        }
    }
}

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
/// `crate::storage_worker` for exactly that reason, same as `release/v0.6`'s
/// `record_with_receipt`; only the in-memory publish above and the spool/native side effects run
/// inline. A failed or refused write is logged and still honoured for this session; enabling
/// detection remains a function of the owner's explicit transition.
fn commit_live(previous: &Consent, next: &Consent) {
    let enabling_errors = newly_enables_errors(previous, next);
    if enabling_errors {
        crate::telemetry::crashreport::discard_pending_before_opt_in();
    }
    crate::telemetry::consent::install(next.clone());
    if !next.errors {
        crate::player::report::clear_error_trace();
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
    let submitted = crate::storage_worker::submit(move || {
        let outcome = crate::telemetry::persistence::record(&next);
        if outcome.write != crate::telemetry::persistence::PersistResult::Durable {
            crate::log(&format!("telemetry: the decision is not durably persisted: {outcome:?}"));
        }
    });
    if submitted.is_err() {
        crate::log("telemetry: the decision could not be queued for persistence");
    }
    // Tests want the write's effect (and `persistence::last_call_thread()`) settled before the
    // next assertion; production has no such deadline and never drains.
    #[cfg(test)]
    crate::storage_worker::drain_for_test();
}

/// Publish the prospective default before touching disk, then erase every queued record and stop
/// native capture. The crash mark deliberately remains untouched by this path. The canonical
/// clear, like `commit_live`'s write, is queued off the frame thread rather than run inline.
///
/// This is sign-out and Delete all local data, not a withdrawal, so the spool is ERASED rather
/// than purged per category: a one-off report the departing account pressed Send for goes with it
/// (`spool::purge_all_local`), and `delivery::forget` first retires every in-flight one-off send and
/// the delivery states that would have shown a report's receipt.
fn forget_live(_prior: &Consent) {
    let next = Consent::default();
    crate::telemetry::consent::install(next.clone());
    crate::player::report::clear_error_trace();
    crate::telemetry::delivery::forget();
    crate::telemetry::spool::purge_all_local();
    crate::telemetry::native::sync_change(&next);
    persist_forget_off_thread();
}

fn persist_forget_off_thread() {
    let submitted = crate::storage_worker::submit(|| {
        let outcome = crate::telemetry::persistence::forget();
        if !matches!(
            outcome.write,
            crate::telemetry::persistence::PersistResult::Durable
                | crate::telemetry::persistence::PersistResult::Delegated
        ) {
            crate::log(&format!("telemetry: sign-out could not durably clear the decision: {outcome:?}"));
        }
    });
    if submitted.is_err() {
        crate::log("telemetry: sign-out could not be queued for persistence");
    }
    #[cfg(test)]
    crate::storage_worker::drain_for_test();
}

#[cfg(test)]
mod tests {
    use super::ConsentAdapter;
    use crate::telemetry::consent::{self, Consent};

    fn decision(id: &str) -> Consent {
        Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            usage: false,
            install_id: None,
            errors_id: Some(id.into()),
            ..Default::default()
        }
    }

    #[test]
    fn fixture_records_committed_transitions_in_order() {
        let first = decision("first");
        let second = decision("second");
        let third = decision("third");
        let mut adapter = ConsentAdapter::fixture();

        adapter.commit(&first, &second);
        adapter.commit(&second, &third);

        assert_eq!(
            adapter.fixture_resources().transitions,
            vec![(first, second.clone()), (second, third)]
        );
        assert!(adapter.fixture_resources().forgotten.is_empty());
    }

    #[test]
    fn fixture_records_forgotten_prior_state() {
        let prior = decision("prior");
        let mut adapter = ConsentAdapter::fixture();

        adapter.forget(&prior);

        assert_eq!(adapter.fixture_resources().forgotten, vec![prior]);
        assert!(adapter.fixture_resources().transitions.is_empty());
    }

    #[test]
    fn fixture_neither_reads_nor_publishes_the_global_snapshot() {
        let _serial = crate::testlock::serial();
        let published_before = consent::current();
        let revision_before = consent::revision();
        let previous = decision("owned");
        let next = Consent::default();
        let mut adapter = ConsentAdapter::fixture();

        adapter.commit(&previous, &next);
        adapter.forget(&next);

        assert_eq!(consent::current(), published_before);
        assert_eq!(consent::revision(), revision_before);
    }

    #[test]
    fn fixture_does_not_write_the_consent_file() {
        struct Redirect(std::path::PathBuf);
        impl Drop for Redirect {
            fn drop(&mut self) {
                crate::telemetry::redirect_for_test(None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let _serial = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-consent-adapter-fixture-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _redirect = Redirect(dir.clone());
        let file = dir.join("telemetry.json");
        crate::telemetry::redirect_for_test(Some(file.clone()));
        let previous = decision("owned");
        let next = Consent::default();
        let mut adapter = ConsentAdapter::fixture();

        adapter.commit(&previous, &next);
        adapter.forget(&next);

        assert!(!file.exists());
    }

    /// Copilot review on PR #105, finding 6. `commit_live`/`forget_live` call
    /// `telemetry::persistence::record`/`forget` — a genuine storage-helper round trip on the
    /// television — and must not run that call on the caller's own thread, since the caller here
    /// is the frame loop's message dispatch (`app::bridge::AppRig::deliver`). `release/v0.6`'s
    /// equivalent (`telemetry::mod.rs::record_with_receipt`/`forget_with_receipt`) submitted the
    /// same work to `crate::storage_worker` for exactly this reason; this test pins the live
    /// adapter to the same off-thread contract.
    #[test]
    fn commit_live_persists_off_the_calling_thread() {
        struct Redirect(std::path::PathBuf);
        impl Drop for Redirect {
            fn drop(&mut self) {
                crate::telemetry::redirect_for_test(None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let _serial = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-consent-adapter-live-thread-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _redirect = Redirect(dir.clone());
        let file = dir.join("telemetry.json");
        crate::telemetry::redirect_for_test(Some(file));

        let caller_thread = std::thread::current().id();
        let previous = decision("owned");
        let next = Consent::default();
        let mut adapter = ConsentAdapter::live();

        adapter.commit(&previous, &next);
        crate::storage_worker::drain_for_test();
        assert_ne!(
            crate::telemetry::persistence::last_call_thread(),
            Some(caller_thread),
            "commit_live must persist off the frame thread, not inline"
        );

        adapter.forget(&next);
        crate::storage_worker::drain_for_test();
        assert_ne!(
            crate::telemetry::persistence::last_call_thread(),
            Some(caller_thread),
            "forget_live must persist off the frame thread, not inline"
        );
    }

    /// **Sign-out and Delete all local data erase a queued one-off report**, which a withdrawal
    /// (`commit_live` → `spool::purge_withdrawn`) deliberately keeps.
    #[test]
    fn forget_live_erases_a_queued_one_off_report_that_a_withdrawal_keeps() {
        use crate::telemetry::queue::{Category, Dest, Record};
        use crate::telemetry::spool;
        struct Redirect(std::path::PathBuf);
        impl Drop for Redirect {
            fn drop(&mut self) {
                spool::set_test_path(None);
                crate::telemetry::redirect_for_test(None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let _serial = crate::testlock::serial();
        let saved = consent::current();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-consent-adapter-oneoff-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _redirect = Redirect(dir.clone());
        crate::telemetry::redirect_for_test(Some(dir.join("telemetry.json")));
        spool::set_test_path(Some(dir.join("spool.bin")));
        let one_off = Record {
            category: Category::OneOff,
            dest: Dest::Sentry,
            event_id: "one-off".into(),
            body: b"{}".to_vec(),
        };
        assert!(spool::append(&one_off));

        let mut adapter = ConsentAdapter::live();
        let previous = decision("owned");
        let withdrawn = Consent {
            asked_version: consent::POLICY_VERSION,
            ..Consent::default()
        };
        adapter.commit(&previous, &withdrawn);
        let kept: Vec<String> = spool::read().into_iter().map(|r| r.event_id).collect();
        adapter.forget(&withdrawn);
        let erased = spool::read().is_empty();
        if let Some(c) = saved {
            consent::install(c);
        }

        assert_eq!(kept, vec!["one-off".to_string()], "a withdrawal purged the one-off report");
        assert!(erased, "sign-out left the one-off report queued");
    }

    #[test]
    fn enabling_detection_uses_the_explicit_previous_decision() {
        let stale_yes = Consent {
            asked_version: consent::POLICY_VERSION.saturating_sub(1),
            errors: true,
            ..Consent::default()
        };
        let current_yes = decision("current");
        let current_no = Consent {
            asked_version: consent::POLICY_VERSION,
            ..Consent::default()
        };

        assert!(super::newly_enables_errors(
            &Consent::default(),
            &current_yes
        ));
        assert!(super::newly_enables_errors(&stale_yes, &current_yes));
        assert!(!super::newly_enables_errors(&current_yes, &current_yes));
        assert!(!super::newly_enables_errors(&current_yes, &current_no));
    }
}
