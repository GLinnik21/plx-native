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

/// Write first, then apply prospective cleanup, publish, purge withdrawn records and synchronize
/// the native backend. This preserves the established live ordering while making enabling
/// detection a function of the owner's explicit transition.
fn commit_live(previous: &Consent, next: &Consent) {
    let enabling_errors = newly_enables_errors(previous, next);
    let Ok(json) = serde_json::to_vec_pretty(next) else {
        return;
    };
    let stored = crate::telemetry::resource_candidates()
        .iter()
        .any(|path| crate::plex::session::write_atomic(path, &json));
    if !stored {
        crate::log("telemetry: could not persist the decision to ANY candidate path");
    }
    if enabling_errors {
        crate::telemetry::crashreport::discard_pending_before_opt_in();
    }
    crate::telemetry::consent::install(next.clone());
    if !next.errors {
        crate::player::report::clear_error_trace();
    }
    crate::telemetry::spool::purge_withdrawn(next);
    crate::telemetry::native::sync_change(next);
}

/// Publish the prospective default before touching disk, then purge withdrawn records and stop
/// native capture. The crash mark deliberately remains untouched by this path.
fn forget_live(_prior: &Consent) {
    let next = Consent::default();
    crate::telemetry::consent::install(next.clone());
    crate::player::report::clear_error_trace();
    for path in crate::telemetry::resource_candidates() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                let overwritten = serde_json::to_vec_pretty(&next)
                    .map(|json| crate::plex::session::write_atomic(&path, &json))
                    .unwrap_or(false);
                crate::log(&format!(
                    "telemetry: sign-out could not unlink the decision ({error}); overwritten={overwritten}"
                ));
            }
        }
    }
    crate::telemetry::spool::purge_withdrawn(&next);
    crate::telemetry::native::sync_change(&next);
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
