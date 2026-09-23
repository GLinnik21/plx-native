//! Session-controller sanity, sign-out consent teardown, and the phase-6 worker
//! observation-boundary guards (login, profile-switch, and the full worker roster).

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

#[test]
fn session_controller_has_no_process_global_owner() {
    let source = include_str!("auth.rs");
    let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
    for declaration in ["static CTL:", "static QR_GENERATION:", "static ENDPOINT_ADMISSION:",
        "static DELETE_LEFTOVERS:", "static PROGRESS:", "static AUTH_EPOCH:", "static ACTIVATION_GATE:"] {
        assert!(!production.contains(declaration), "global decision/queue remains: {declaration}");
    }
}

/// **The next account to sign in must be asked afresh.** The maintainer's scenario (2026-09-04):
/// account A consents to both channels, signs out, account B signs in through the QR flow — and
/// B was never asked, while B's usage went out under A's consent and A's identifiers. Consent
/// belongs to the person who gave it, so signing out ends it: the decision returns to
/// *unanswered*, both identifiers are destroyed and the file is gone, exactly as a withdrawal
/// plus a fresh install would leave it. This resource test grades the live Session adapter's
/// CloseTelemetry effect. The Bridge erasure test separately proves that the owner emits it
/// before resource deletion; no network work is launched here.
#[test]
fn signing_out_leaves_no_consent_and_no_identifier_for_the_next_account() {
    use crate::telemetry::consent;
    /// Every crate-global redirect this test takes, handed back on drop — so a failed
    /// assertion cannot leave the next test writing into this one's directory.
    struct Redirects {
        dir: std::path::PathBuf,
        saved: Option<consent::Consent>,
    }
    impl Drop for Redirects {
        fn drop(&mut self) {
            crate::telemetry::spool::set_test_path(None);
            crate::telemetry::redirect_for_test(None);
            crate::plex::session::redirect_for_test(None);
            if let Some(c) = self.saved.take() {
                consent::install(c);
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    let _g = crate::testlock::serial();
    let dir =
        std::env::temp_dir().join(format!("plxnative-signout-consent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a writable temp dir");
    let _redirects = Redirects {
        dir: dir.clone(),
        saved: consent::current(),
    };
    crate::plex::session::redirect_for_test(Some(dir.join("auth.json")));
    let consent_file = dir.join("telemetry.json");
    crate::telemetry::redirect_for_test(Some(consent_file.clone()));
    crate::telemetry::spool::set_test_path(Some(dir.join("spool.jsonl")));

    // Account A answers yes to both, which mints both identifiers and persists the decision.
    crate::telemetry::record(consent::apply(
        &consent::Consent::default(),
        true,
        true,
        || Some("a".repeat(32)),
    ));
    assert!(consent::allows_usage() && consent::errors_id().is_some());
    assert!(
        crate::telemetry::persistence::load(std::slice::from_ref(&consent_file)).any(),
        "the decision was persisted for account A"
    );

    let mut bridge = crate::app::bridge::Bridge::for_consent_resource_test(
        consent::current().expect("account A decision is published"),
    );
    let mut dispatcher =
        crate::ui::dispatch::Dispatcher::<crate::app::bridge::AppHost>::new();
    dispatcher.emit(
        crate::ui::machine::MachineId::Session,
        crate::ui::machine::Fx::App(
            crate::screens::registry::AppFx::SessionEffect(
                owner::SessionFx::Coordinator(owner::CoordinatorAction::CloseTelemetry),
            ),
        ),
    );
    dispatcher.frame_with(
        &mut bridge,
        crate::ui::machine::Tick::default(),
        Vec::new(),
        Vec::new(),
        &mut crate::ui::dispatch::NoTap,
        false,
    );

    let after = consent::current().expect("a decision is always published");
    assert!(
        !after.answered(),
        "account B would never be asked: A's answer survived the sign-out"
    );
    assert!(
        after.install_id.is_none() && after.errors_id.is_none(),
        "an identifier survived the sign-out and would tag B's reports as A"
    );
    assert!(!consent::allows_usage() && !consent::allows_errors());
    assert!(consent::errors_id().is_none());
    assert!(
        consent::should_ask(&after, false),
        "the next authorized sign-in must put the question on screen again"
    );
    let reopened = crate::telemetry::persistence::load(std::slice::from_ref(&consent_file));
    assert!(
        !consent_file.exists() && !reopened.answered() && reopened.errors_id.is_none(),
        "the persisted decision outlived the sign-out and would resume A's at the next boot"
    );
}

// ---- phase 6: LoginProgress / apply_progress ----
//
// `login_thread` used to be a writer of `Ctl` with the same authority as `start_login`/
// `cancel`/`restart`, from a thread none of those three synchronize with except by convention.
// These three tests pin the replacement: a stale observation is refused, a cancel cannot be
// overtaken by a success that was already in flight when it happened, and the worker functions
// themselves no longer contain the write at all.

#[test]
fn a_profile_delta_preserves_unrelated_newer_session_preferences() {
    let mut current = signed_in_as("u-adult");
    current.recent_searches.push(session::RecentSearches {
        user: "u-adult".into(),
        terms: vec!["newer preference".into()],
        extensions: Default::default(),
    });
    let next = signed_in_as("u-kid");
    merge_profile_delta(
        &mut current,
        ProfileDelta {
            server: next.server,
            sources: next.sources,
            user: next.user,
            cache: None,
        },
    );
    assert_eq!(current.user.uuid, "u-kid");
    assert_eq!(current.recent_searches.len(), 1);
    assert_eq!(current.recent_searches[0].terms, ["newer preference"]);
}

#[test]
fn instance_profile_worker_completes_offline_policy_on_its_own_landing() {
    use crate::auth::owner::{SessionArrival, SessionOp, SessionWorkKey};
    use crate::app::adapters::session::SessionAdapter;
    use crate::ui::machine::RequestId;
    // No serial lock: both input credentials and both output transports are instance-local.
    let mut a = SessionAdapter::fixture();
    let mut b = SessionAdapter::fixture();
    for (adapter, epoch, uuid) in [(&mut a, 0x1_0000_0001, "u-kid"),
        (&mut b, 7, "not-cached")] {
        let stored = cached_session(None);
        let expected = SessionIdentity::of(&stored);
        let tile = UserTile { uuid: uuid.into(), title: "Synthetic profile".into(),
            ..Default::default() };
        adapter.launch(RequestId(1), SessionWorkKey { epoch, op: SessionOp::ProfileSwitch },
            true, |job| { job(); true }, move |output| {
                profile_switch_worker_with_output(epoch, expected, stored, tile, None,
                    false, &output, |_, _, _| SwitchOutcome::Unreachable);
            }).unwrap();
    }
    let a = a.take_results();
    let b = b.take_results();
    assert_eq!(a.len(), 1);
    assert_eq!(b.len(), 1);
    assert!(a[0].terminal && b[0].terminal);
    let SessionArrival::Data(a) = &a[0].outcome else { panic!("missing offline result") };
    let SessionArrival::Data(b) = &b[0].outcome else { panic!("missing failure result") };
    assert!(matches!(&**a, observation::Observation::ProfileSwitch(ProfileSwitchProgress {
        epoch: 0x1_0000_0001, outcome: ProfileSwitchOutcomeProgress::Ready { delta, .. }, ..
    }) if delta.user.uuid == "u-kid"));
    assert!(matches!(&**b, observation::Observation::ProfileSwitch(ProfileSwitchProgress {
        epoch: 7, outcome: ProfileSwitchOutcomeProgress::Failed { pin_denied: false, .. }, ..
    })));
}

/// S7 (plan §4): a grant whose only settled probe is `InsecureOnly` must fail with the SHARED
/// discovery copy, not "has no access to this server" — the grant is real, only the transport
/// is unusable in this build, and the ordinary wording sends the user to ask their friend for
/// access they already have.
#[test]
fn a_grant_verified_only_over_plaintext_reports_the_shared_insecure_only_copy() {
    use crate::auth::owner::{SessionArrival, SessionOp, SessionWorkKey};
    use crate::app::adapters::session::SessionAdapter;
    use crate::plex::account::SwitchedUser;
    use crate::ui::machine::RequestId;

    struct InsecureOnlyGrantIo;
    impl ProfileWorkIo for InsecureOnlyGrantIo {
        fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
            SwitchOutcome::Switched(SwitchedUser {
                id: 1, uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-token".into(),
            })
        }
        fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
            Ok(vec![Resource {
                name: "srv".into(), client_identifier: "srv".into(), provides: "server".into(),
                owned: true, access_token: "srv-token".into(), ..Default::default()
            }])
        }
        fn probe(&mut self, _: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
            // No winning source (nothing this build may put a credential on), but the probe
            // itself verified the server — over plaintext only.
            (None, settled_probe_for_test("srv", Outcome::InsecureOnly,
                Some(probe::Location::Local), Some("10.0.0.5".into())))
        }
        fn gap(&mut self) {}
    }

    let mut a = SessionAdapter::fixture();
    let stored = cached_session(None);
    let expected = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
    a.launch(RequestId(1), SessionWorkKey { epoch: 1, op: SessionOp::ProfileSwitch }, true,
        |job| { job(); true }, move |output| {
            profile_switch_worker_with_io(1, expected, stored, tile, None, false,
                &output, &mut InsecureOnlyGrantIo);
        }).unwrap();
    let results = a.take_results();
    assert_eq!(results.len(), 1);
    assert!(results[0].terminal);
    let SessionArrival::Data(data) = &results[0].outcome else { panic!("missing failure result") };
    assert!(
        matches!(&**data, observation::Observation::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Failed { error, pin_denied: false }, ..
        }) if error == DISCOVERY_INSECURE_ONLY_MESSAGE),
        "an InsecureOnly-only grant must report the shared discovery copy, not the generic \
         'has no access' wording"
    );
}

/// **The invariant phase 6 exists to establish, pinned by reading the source.** Nothing else
/// can see a regression here: a `with_ctl(|c| c.foo = …)` spliced back into `login_thread`
/// compiles cleanly, passes every OTHER test in this file (none of them spin up a real worker
/// thread against a real plex.tv — see the module doc), and only misbehaves on a device, under
/// contention nobody happened to be watching for. Modelled on `diag::scrub`'s
/// `no_log_call_site_interpolates_viewing_content`, which pins its own "the mechanism is that
/// nobody writes it" claim the same way.
///
/// **What this test actually checks, restated after a reviewer found the gap in the stronger
/// sentence this comment used to make ("no `with_ctl` call at all", full stop).** That claim
/// was true of the two DIRECT spellings this test greped for and false of a WRAPPED one: the
/// reviewer added `set_error_if_live(epoch, "…")` to `login_thread` — the exact pre-phase-6
/// call [`LoginProgress::Failed`]'s doc says is retired — and it reached `with_ctl` two hops
/// down (`set_error_if_live` → `set_error` → `with_ctl`) while this test kept reporting green,
/// because a two-hop wrapper call is neither `with_ctl(` nor `CTL.lock(` in the CALLER's own
/// text. Three checks run per function now, not two: the original direct-spelling pair, PLUS
/// [`calls_a_function_prefixed`] against `set_`, the naming family every `Ctl`-writing setter
/// in this file already belongs to — which catches `set_error_if_live(` (and `set_error(` on
/// its own) by the family, not by a name typed into this test.
///
/// **What is still NOT proven, and this says so rather than overclaiming again:** a wrapper
/// that reached `Ctl` under a name outside the `set_` convention would still slip past a
/// textual scan — closing that fully needs a real call-graph walk (or moving `Ctl` behind an
/// interface a worker's module cannot name at all), not a longer prefix list. This is a
/// materially stronger gate than the one it replaces, scoped to say exactly that.
///
/// This original QR-specific guard stays beside the broader R2A boundary below because it also
/// scans the helper chain called by login discovery. Profile, roster and endpoint workers are
/// covered by [`all_auth_worker_bodies_are_observation_only`].
#[test]
fn login_worker_functions_never_touch_ctl_directly() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    )
    .expect("auth.rs must be readable from its own test");
    for name in [
        "login_worker_with_output",
        "mint_pin",
        "finish_sign_in",
        "discover_and_store",
        "rediscovery_worker_with_output",
    ] {
        let body = extract_fn_body(&src, name);
        assert!(
            !body.contains("with_ctl("),
            "`{name}` calls `with_ctl` — a phase-6 worker must observe and push a \
             `LoginProgress` instead of touching `Ctl` directly (read OR write):\n{body}"
        );
        assert!(
            !body.contains("CTL.lock("),
            "`{name}` locks `CTL` directly, bypassing `with_ctl` but not the rule it exists \
             to enforce"
        );
        for wrapper in ["set_error(", "set_error_if_live("] {
            assert!(
                !body.contains(wrapper),
                "`{name}` calls `{wrapper}` — a `Ctl`-writing wrapper this test used to be \
                 blind to, because it only greped for `with_ctl(`/`CTL.lock(` directly and \
                 this reaches `with_ctl` two calls down. A worker must push a `LoginProgress` \
                 and let `apply_progress` make the write on the main thread instead."
            );
        }
        assert!(
            !calls_a_function_prefixed(body, "set_"),
            "`{name}` calls a `set_`-prefixed function — this file's naming convention for \
             every `Ctl`-writing setter it has (`set_error`, `set_error_if_live`, \
             `set_pin_denied_for_test`). A NEW setter sharing that prefix is refused here by \
             the family it belongs to; see the doc above this test for the mutation that made \
             the narrower direct-spelling check insufficient:\n{body}"
        );
    }
}

/// Textual worker boundary: the actual instance-worker entry points AND their shared policy
/// bodies. Scanning a thin forwarding wrapper alone cannot constrain its callee. This is
/// an explicit list, not automatic call-graph coverage; new worker helpers must be added.
/// Workers may perform network/probe/PBKDF2 work and publish immutable observations;
/// application mutation belongs to the main-thread owner/resource acceptance path.
#[test]
fn all_auth_worker_bodies_are_observation_only() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    )
    .expect("auth.rs must be readable from its own test");
    let forbidden = [
        "with_ctl(",
        "CTL.lock(",
        "with_live_epoch(",
        "session::load(",
        "session::peek(",
        "session::save(",
        "session::update(",
        "session::clear(",
        "session::set_current(",
        "activate_candidate(",
        "install_roster(",
        "publish_settled_probe(",
        "publish_settled_probes(",
        "crate::plex::register_origin(",
        "crate::plex::revoke_",
        "crate::plex::finish_profile_switch(",
        "crate::plex::publish_probe_result(",
        "crate::plex::describe_server(",
    ];
    for name in [
        "discover_and_store",
        "login_worker_with_output",
        "rediscovery_worker_with_output",
        "home_roster_worker_with_output",
        "server_roster_worker_with_output",
        "profile_switch_worker_with_output",
        "profile_switch_worker_with_io",
        "run_session_work",
        "endpoint_work_fact",
        "endpoint_worker_with_io",
        "probe_endpoint_work",
        "offline_switch_outcome",
        "candidate_activation",
        "probe_profile_resource_live",
    ] {
        let body = extract_fn_body(&src, name);
        for call in forbidden {
            assert!(
                !body.contains(call),
                "auth worker `{name}` crosses the observation boundary through `{call}`:\n{body}"
            );
        }
    }
}

/// #132: the roster worker's grading. A refused identity (a managed profile's 401) and an answer
/// with nobody in it are plex.tv's VERDICT — `Some(vec![])` — which the owner reads out as
/// "switching isn't available"; no answer at all, a 5xx or an unreadable body is `None`, read out
/// as a connection problem. Before this, all of these were one `None`.
#[test]
fn roster_grading_tells_a_refused_identity_from_no_answer() {
    use crate::net::{RequestError, RequestFailure};
    let user = HomeUser { uuid: "synthetic-user".into(), title: "Synthetic".into(), ..Default::default() };
    assert_eq!(grade_roster(Ok(vec![user])).map(|users| users.len()), Some(1));
    assert_eq!(grade_roster(Ok(Vec::new())).map(|users| users.len()), Some(0));
    assert_eq!(grade_roster(Err(Ok(401))).map(|users| users.len()), Some(0));
    assert_eq!(grade_roster(Err(Ok(403))).map(|users| users.len()), Some(0));
    assert!(grade_roster(Err(Ok(503))).is_none());
    assert!(grade_roster(Err(Ok(200))).is_none(), "an unreadable 200 is no verdict");
    assert!(grade_roster(Err(Err(RequestFailure { cause: RequestError::TimedOut, status: None,
        body_limit: None, curl_rc: Some(28) }))).is_none());
}

/// #132: a switched profile whose token plex.tv then REFUSES is not a connection fault. The
/// refusal says so; a request that got no answer keeps the connection wording.
#[test]
fn a_refused_profile_resources_request_does_not_blame_the_connection() {
    use crate::auth::owner::{SessionArrival, SessionOp, SessionWorkKey};
    use crate::app::adapters::session::SessionAdapter;
    use crate::net::{RequestError, RequestFailure};
    use crate::plex::account::{CallEvidence, SwitchedUser};
    use crate::ui::machine::RequestId;

    struct FailingResourcesIo(Option<CallEvidence>);
    impl ProfileWorkIo for FailingResourcesIo {
        fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
            SwitchOutcome::Switched(SwitchedUser {
                id: 1, uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-token".into(),
            })
        }
        fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, CallEvidence> {
            Err(self.0.take().expect("one resources request"))
        }
        fn probe(&mut self, _: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
            unreachable!("no resources, nothing to probe")
        }
        fn gap(&mut self) {}
    }

    let run = |evidence: CallEvidence| -> String {
        let mut a = SessionAdapter::fixture();
        let stored = cached_session(None);
        let expected = SessionIdentity::of(&stored);
        let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
        let mut io = FailingResourcesIo(Some(evidence));
        a.launch(RequestId(1), SessionWorkKey { epoch: 1, op: SessionOp::ProfileSwitch }, true,
            |job| { job(); true }, move |output| {
                profile_switch_worker_with_io(1, expected, stored, tile, None, false,
                    &output, &mut io);
            }).unwrap();
        let results = a.take_results();
        let SessionArrival::Data(data) = &results[0].outcome else { panic!("missing failure result") };
        match &**data {
            observation::Observation::ProfileSwitch(ProfileSwitchProgress {
                outcome: ProfileSwitchOutcomeProgress::Failed { error, pin_denied: false }, ..
            }) => error.clone(),
            _ => panic!("expected a banner failure"),
        }
    };

    let refused = run(Ok(401));
    assert!(!refused.contains("connection"), "a 401 is an answer: {refused}");
    assert!(refused.contains("Kid"), "the refusal names the profile: {refused}");
    let silent = run(Err(RequestFailure { cause: RequestError::TimedOut, status: None,
        body_limit: None, curl_rc: Some(28) }));
    assert!(silent.contains("check the connection"), "no answer keeps the connection copy: {silent}");
}
