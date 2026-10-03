//! Per-application consent resources. The owner supplies both sides of every logical transition;
//! this adapter only performs persistence/publication side effects (telemetry's own, in
//! `telemetry::transition`) or records fixture effects.

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
            Resources::Live => crate::telemetry::transition::commit(previous, next),
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
            Resources::Live => crate::telemetry::transition::forget(prior),
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
        let _serial = plx_base::testlock::serial();
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

        let _serial = plx_base::testlock::serial();
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

    /// Copilot review on PR #105, finding 6. `transition::commit`/`transition::forget` call
    /// `telemetry::persistence::record`/`forget` — a genuine storage-helper round trip on the
    /// television — and must not run that call on the caller's own thread, since the caller here
    /// is the frame loop's message dispatch (`app::bridge::AppRig::deliver`). `release/v0.6`'s
    /// equivalent (`telemetry::mod.rs::record_with_receipt`/`forget_with_receipt`) submitted the
    /// same work to `plx_base::storage_worker` for exactly this reason; this test pins the live
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

        let _serial = plx_base::testlock::serial();
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
        plx_base::storage_worker::drain_for_test();
        assert_ne!(
            crate::telemetry::persistence::last_call_thread(),
            Some(caller_thread),
            "transition::commit must persist off the frame thread, not inline"
        );

        adapter.forget(&next);
        plx_base::storage_worker::drain_for_test();
        assert_ne!(
            crate::telemetry::persistence::last_call_thread(),
            Some(caller_thread),
            "transition::forget must persist off the frame thread, not inline"
        );
    }

    /// **Sign-out and Delete all local data erase a queued one-off report**, which a withdrawal
    /// (`transition::commit` → `spool::purge_withdrawn`) deliberately keeps.
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

        let _serial = plx_base::testlock::serial();
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

    /// **Sign-in events held while consent was unanswered** are replayed by `transition::commit` once a
    /// decision is published, and dropped unreplayed by `transition::forget`, so a departing account's
    /// attempt can never reach the next account's decision.
    #[test]
    fn commit_live_replays_held_signin_events_and_forget_live_drops_them() {
        use crate::diag::schema::DiagEvent;
        struct Redirect(std::path::PathBuf);
        impl Drop for Redirect {
            fn drop(&mut self) {
                crate::telemetry::redirect_for_test(None);
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let _serial = plx_base::testlock::serial();
        let saved = consent::current();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-consent-adapter-held-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _redirect = Redirect(dir.clone());
        crate::telemetry::redirect_for_test(Some(dir.join("telemetry.json")));

        crate::diag::clear_deferred();
        consent::install(Consent::default());
        crate::diag::event(DiagEvent::SignInStarted);
        assert_eq!(crate::diag::deferred_len(), 1);

        let mut adapter = ConsentAdapter::live();
        let yes = Consent {
            asked_version: consent::POLICY_VERSION,
            usage: true,
            install_id: Some("i".repeat(32)),
            ..Consent::default()
        };
        adapter.commit(&Consent::default(), &yes);
        let after_commit = crate::diag::deferred_len();

        crate::diag::event(DiagEvent::SignInStarted); // consent is answered yes: sent, not held
        consent::install(Consent::default());
        crate::diag::event(DiagEvent::SignInCancelled);
        let held_before_forget = crate::diag::deferred_len();
        adapter.forget(&yes);
        let after_forget = crate::diag::deferred_len();

        crate::diag::clear_deferred();
        if let Some(c) = saved {
            consent::install(c);
        }
        assert_eq!(after_commit, 0, "transition::commit left the held event queued");
        assert_eq!(held_before_forget, 1, "the unanswered sign-in event was not held");
        assert_eq!(after_forget, 0, "transition::forget left the held event queued");
    }
}
