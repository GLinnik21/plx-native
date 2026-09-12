use super::*;
use crate::telemetry::consent::{self, Consent};
use crate::ui::dispatch::NoTap;

struct PublishedSnapshot(Option<Consent>);

impl Drop for PublishedSnapshot {
    fn drop(&mut self) {
        consent::install(self.0.take().unwrap_or_default());
    }
}

fn decision(id: &str) -> Consent {
    Consent {
        asked_version: consent::POLICY_VERSION,
        errors: true,
        usage: false,
        errors_id: Some(id.into()),
        install_id: None,
    }
}

fn drive(rig: &mut Bridge, fx: AppFx) {
    let mut d = Dispatcher::<AppHost>::new();
    d.emit(MachineId::Nav, Fx::App(fx));
    d.frame_with(
        rig,
        Tick::default(),
        Vec::new(),
        Vec::new(),
        &mut NoTap,
        false,
    );
}

#[test]
fn consent_decides_from_its_owned_state_and_not_the_published_snapshot() {
    let _serial = crate::testlock::serial();
    let saved = PublishedSnapshot(consent::current());
    let published = decision("published-id");
    consent::install(published.clone());
    let owned = decision("owned-id");
    let mut rig = Bridge::for_consent_test(owned.clone());

    drive(
        &mut rig,
        AppFx::Consent(ConsentCmd::Record {
            errors: true,
            usage: false,
        }),
    );

    assert_eq!(rig.consent.current(), &owned);
    assert_eq!(
        rig.consent_adapter.fixture_resources().transitions,
        vec![(owned.clone(), owned)]
    );
    assert_eq!(
        consent::current(),
        Some(published),
        "the fixture resource boundary must not make a process-global value the machine's owner"
    );
    drop(saved);
}

#[test]
fn consent_effect_is_an_addressed_delivery_before_the_owner_steps() {
    let initial = decision("owned-id");
    let mut rig = Bridge::for_consent_test(initial.clone());
    let mut out = Vec::new();
    let mut present = Present::new();
    let mut effects = Effects::new(&mut out, MachineId::Nav, &mut present);

    rig.app_effect(
        MachineId::Instance(InstanceId(7)),
        AppFx::Consent(ConsentCmd::Record {
            errors: false,
            usage: false,
        }),
        &mut effects,
    );

    assert_eq!(rig.consent.current(), &initial, "AppFx routing must not step the owner inline");
    assert!(matches!(
        out.as_slice(),
        [crate::ui::machine::Stamped {
            fx: Fx::Deliver(
                MachineId::Consent,
                Delivery::Machine(AppMsg::Consent(ConsentCmd::Record {
                    errors: false,
                    usage: false
                }))
            ),
            ..
        }]
    ));
}

#[test]
fn session_close_telemetry_is_executed_by_the_consent_owner() {
    let initial = decision("owned-id");
    let mut rig = Bridge::for_consent_test(initial.clone());

    drive(
        &mut rig,
        AppFx::SessionEffect(crate::auth::owner::SessionFx::Coordinator(
            crate::auth::owner::CoordinatorAction::CloseTelemetry,
        )),
    );

    assert_eq!(rig.consent.current(), &Consent::default());
    assert_eq!(
        rig.consent_adapter.fixture_resources().forgotten,
        vec![initial]
    );
    assert!(rig
        .session_adapter
        .fixture_resources()
        .coordinator_events
        .iter()
        .any(|action| matches!(
            action,
            crate::auth::owner::CoordinatorAction::CloseTelemetry
        )));
}

#[test]
fn signing_out_through_consent_owner_leaves_nothing_for_the_next_account() {
    struct Redirects {
        dir: std::path::PathBuf,
        saved: Option<Consent>,
    }
    impl Drop for Redirects {
        fn drop(&mut self) {
            crate::telemetry::spool::set_test_path(None);
            crate::telemetry::redirect_for_test(None);
            if let Some(saved) = self.saved.take() {
                consent::install(saved);
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    let _serial = crate::testlock::serial();
    let dir = std::env::temp_dir().join(format!(
        "plxnative-consent-owner-signout-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _redirects = Redirects {
        dir: dir.clone(),
        saved: consent::current(),
    };
    let consent_file = dir.join("telemetry.json");
    crate::telemetry::redirect_for_test(Some(consent_file.clone()));
    crate::telemetry::spool::set_test_path(Some(dir.join("spool.jsonl")));

    let enabled = Consent {
        asked_version: consent::POLICY_VERSION,
        errors: true,
        usage: true,
        install_id: Some("a".repeat(32)),
        errors_id: Some("b".repeat(32)),
    };
    crate::telemetry::record(enabled.clone());
    assert!(consent_file.exists());
    let mut rig = Bridge::for_consent_resource_test(enabled);

    drive(
        &mut rig,
        AppFx::SessionEffect(crate::auth::owner::SessionFx::Coordinator(
            crate::auth::owner::CoordinatorAction::CloseTelemetry,
        )),
    );

    assert_eq!(rig.consent.current(), &Consent::default());
    let published = consent::current().expect("the prospective refusal is published");
    assert!(!published.answered() && !published.any());
    assert!(published.install_id.is_none() && published.errors_id.is_none());
    assert!(!consent::allows_usage() && !consent::allows_errors());
    assert!(!consent_file.exists());
}

#[test]
fn consent_changes_its_own_canonical_subhash() {
    let initial = decision("owned-id");
    let mut rig = Bridge::for_consent_test(initial);
    let before = rig.consent_subhash();

    drive(
        &mut rig,
        AppFx::Consent(ConsentCmd::Record {
            errors: false,
            usage: false,
        }),
    );

    assert_ne!(rig.consent_subhash(), before);
}
