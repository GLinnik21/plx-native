//! Server discovery and probe-racing tests: retry policy, candidate racing, relay fallback,
//! identity verification, address dialability, and the resolve-roster end-to-end scenarios.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use crate::net::{ymd_from_now, TestCaGuard};

/// PLX-NATIVE-12: authorization has already succeeded, so a transient failure listing plex.tv
/// resources must be retried in place instead of becoming the terminal silent verdict.
#[test]
fn authorized_discovery_retries_a_dns_blip_before_settling() {
    use std::sync::Mutex;
    struct Live(Mutex<Vec<AuthProgress>>);
    impl owner::ObservationSink for Live {
        fn live(&self) -> bool { true }
        fn progress(&self, progress: AuthProgress) -> bool {
            self.0.lock().unwrap().push(progress);
            true
        }
        fn terminal(&self, _: AuthProgress) -> bool { true }
    }
    let dns = crate::net::RequestFailure {
        cause: crate::net::RequestError::Transport,
        status: None,
        body_limit: None,
        curl_rc: Some(6),
    };
    let account = AccountClient::new("client", Some("authorized-token"));
    let mut servers = Some(vec![resource(r#"{"name":"ours","clientIdentifier":"machine","provides":"server",
        "owned":true,"accessToken":"profile-token","connections":[]}"#)]);
    struct Clock(Duration);
    impl RetryClock for Clock {
        fn elapsed(&self) -> Duration { self.0 }
        fn wait(&mut self, duration: Duration) -> bool { self.0 += duration; true }
    }
    let mut clock = Clock(Duration::ZERO);
    let mut calls = 0;
    let output = Live(Mutex::new(Vec::new()));
    let outcome = discover_and_store_with_resources_and_clock(
        &account,
        "client",
        1,
        DiscoveryTrigger::Login,
        &PlaintextAsk::undecided(),
        &output,
        &mut clock,
        |_, _| {
            calls += 1;
            if calls == 1 { Err(Err(dns)) } else { Ok(servers.take().unwrap()) }
        },
    );
    let progress = output.0.lock().unwrap();
    assert!(matches!(progress.first(),
        Some(AuthProgress::Login(LoginProgress::DiscoveryTrouble { progress:
            DiscoveryRetryProgress { run: DiscoveryRetryRun::Resources, misses: 1, .. }, .. }))));
    assert!(matches!(progress.get(1),
        Some(AuthProgress::Login(LoginProgress::DiscoveryRetrySettled {
            run: DiscoveryRetryRun::Resources, .. }))),
        "a recovered resources retry is cleared before probing continues");
    assert!(
        !matches!(outcome, Discovery::PlexTvFailed(_)),
        "an authorized discovery must retry a transient plex.tv resources failure"
    );
    assert_eq!(calls, 2);
}

#[test]
fn login_home_users_retries_then_falls_back_to_an_empty_roster() {
    struct Live;
    impl owner::ObservationSink for Live {
        fn live(&self) -> bool { true }
        fn progress(&self, _: AuthProgress) -> bool { true }
        fn terminal(&self, _: AuthProgress) -> bool { true }
    }
    struct Clock(Duration);
    impl RetryClock for Clock {
        fn elapsed(&self) -> Duration { self.0 }
        fn wait(&mut self, duration: Duration) -> bool { self.0 += duration; true }
    }
    let account = AccountClient::new("client", Some("authorized-token"));
    let dns = Err(crate::net::RequestFailure { cause: crate::net::RequestError::Transport,
        status: None, body_limit: None, curl_rc: Some(6) });
    let mut calls = 0;
    let users = sign_in_home_users_with_clock(&account, 7, &Live,
        &mut Clock(Duration::ZERO), |_, _| {
            calls += 1;
            Err(dns)
        }).expect("the live login continues after roster fallback");
    assert_eq!(calls, INTERACTIVE_ACCOUNT.max_attempts as usize);
    assert!(users.is_empty(), "roster failure keeps the existing single-user fallback");
}

#[test]
fn login_home_users_publishes_the_first_miss_before_the_second_request_returns() {
    use std::sync::Mutex;
    struct Capture(Mutex<Vec<AuthProgress>>);
    impl owner::ObservationSink for Capture {
        fn live(&self) -> bool { true }
        fn progress(&self, progress: AuthProgress) -> bool {
            self.0.lock().unwrap().push(progress);
            true
        }
        fn terminal(&self, _: AuthProgress) -> bool { true }
    }
    struct Clock(Duration);
    impl RetryClock for Clock {
        fn elapsed(&self) -> Duration { self.0 }
        fn wait(&mut self, duration: Duration) -> bool { self.0 += duration; true }
    }
    let output = Capture(Mutex::new(Vec::new()));
    let account = AccountClient::new("client", Some("authorized-token"));
    let dns = Err(crate::net::RequestFailure { cause: crate::net::RequestError::Transport,
        status: None, body_limit: None, curl_rc: Some(6) });
    let mut calls = 0;
    let users = sign_in_home_users_with_clock(&account, 9, &output,
        &mut Clock(Duration::ZERO), |_, _| {
            calls += 1;
            if calls == 1 { return Err(dns) }
            assert!(matches!(output.0.lock().unwrap().as_slice(),
                [AuthProgress::Login(LoginProgress::DiscoveryTrouble { epoch: 9,
                    progress: DiscoveryRetryProgress {
                        run: DiscoveryRetryRun::HomeUsers, misses: 1, ..
                    } })]),
                "the screen must know about the first miss while attempt two is outstanding");
            Ok(Vec::new())
        }).unwrap();
    assert!(users.is_empty());
    assert!(matches!(output.0.lock().unwrap().last(),
        Some(AuthProgress::Login(LoginProgress::DiscoveryRetrySettled {
            epoch: 9, run: DiscoveryRetryRun::HomeUsers,
        }))));
}

#[test]
fn background_home_roster_refresh_retries_before_preserving_the_cache() {
    struct Clock(Duration);
    impl RetryClock for Clock {
        fn elapsed(&self) -> Duration { self.0 }
        fn wait(&mut self, duration: Duration) -> bool { self.0 += duration; true }
    }
    let account = AccountClient::new("client", Some("token"));
    let dns = Err(crate::net::RequestFailure { cause: crate::net::RequestError::Transport,
        status: None, body_limit: None, curl_rc: Some(6) });
    let mut calls = 0;
    let graded = home_roster_with_io_and_clock(&account, &mut Clock(Duration::ZERO), |_, _| {
        calls += 1;
        Err(dns)
    }).expect("the worker remains live");
    assert_eq!(calls, BACKGROUND_ACCOUNT.max_attempts as usize);
    assert!(graded.is_none(), "an unanswered refresh preserves the cached roster");
}

#[test]
fn retry_reuses_an_authorized_account_only_for_discovery_errors() {
    let mut old = owner::SessionInit::captured(Session {
        account_token: "persisted-but-not-authorized-now".into(),
        ..Session::default()
    });
    old.phase = Phase::Error;
    assert_eq!(
        retry_kind(old.phase, old.authorized_in_flow),
        RetryKind::Login
    );

    let mut current = old;
    current.authorized_in_flow = true;
    assert_eq!(
        retry_kind(current.phase, current.authorized_in_flow),
        RetryKind::Discovery
    );
    assert_eq!(retry_kind(Phase::Waiting, true), RetryKind::Login);
}

/// **A stalled DISCOVERY retries discovery, not the whole sign-in.** `ui::login` grows a
/// `Try again` once a working phase has run long enough to look wedged, and discovery is the
/// phase that reaches — it only runs after the pin has already yielded an account credential.
/// Routing that press through `RetryKind::Login` minted a fresh QR and made the user
/// authorize on their phone a second time for what is usually one unreachable server.
#[test]
fn a_stalled_discovery_retries_discovery_rather_than_minting_a_new_qr() {
    assert_eq!(retry_kind(Phase::Discovering, true), RetryKind::Discovery);
    assert_eq!(
        retry_kind(Phase::Discovering, false),
        RetryKind::Login,
        "…but discovery reached without an authorization in THIS flow has no token to reuse"
    );
    assert_eq!(
        retry_kind(Phase::Creating, true),
        RetryKind::Login,
        "and a stall before the pin exists can only start over"
    );
}

/// Completion order is responsiveness, never preference. A lower-scoring remote candidate
/// finishing last cannot replace the local winner that already activated.
#[test]
fn a_worse_candidate_finishing_last_never_downgrades_the_winner() {
    let plan = race_plan();
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host().starts_with("203-") {
            std::thread::sleep(Duration::from_millis(30));
        }
        (200, identity_json("race-machine"))
    });
    let mut activated = Vec::new();
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, origin| activated.push(origin.base()),
    );

    let Reach::At(candidate, _) = reach else {
        panic!("the local candidate must win")
    };
    assert_eq!(candidate.location, probe::Location::Local);
    assert_eq!(
        activated.len(),
        1,
        "the worse last result must not cause a re-point"
    );
    assert!(activated[0].contains("192-0-2-10"));
}

#[test]
fn a_better_candidate_finishing_last_causes_exactly_one_final_repoint() {
    let mut plan = race_plan();
    plan.candidates.swap(0, 1); // remote launches first; local remains the better score
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host().starts_with("192-") {
            std::thread::sleep(Duration::from_millis(30));
        }
        (200, identity_json("race-machine"))
    });
    let mut activated = Vec::new();
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, c, _| activated.push(c.location),
    );
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Local));
    assert_eq!(
        activated,
        [probe::Location::Remote, probe::Location::Local],
        "first usable, then one final best-score re-point"
    );
}

/// Pending means a worker really exists. Refusing one launch cannot leave the coordinator
/// awaiting a message that can never be sent.
#[test]
fn one_refused_spawn_still_settles_on_the_worker_that_exists() {
    let plan = race_plan();
    let dial: ProbeDial = status_dial(|_, _, _| (200, identity_json("race-machine")));
    let spawn = |index: usize, job: ProbeJob| {
        if index == 0 {
            false
        } else {
            std::thread::spawn(job);
            true
        }
    };
    let mut activated = Vec::new();
    let reach = probe_server_racing(&plan, dial, &spawn, test_policy(), &mut |_, c, _| {
        activated.push(c.location)
    });
    assert!(matches!(reach, Reach::At(..)));
    assert_eq!(activated, vec![probe::Location::Remote]);
}

#[test]
fn all_refused_spawns_terminate_as_failure() {
    let plan = race_plan();
    let dial: ProbeDial = status_dial(|_, _, _| panic!("a refused job must never run"));
    let mut activations = 0;
    let reach =
        probe_server_racing(&plan, dial, &|_, _| false, test_policy(), &mut |_, _, _| {
            activations += 1
        });
    assert!(matches!(reach, Reach::No));
    assert_eq!(activations, 0);
}

/// Relay is a second phase, not one more concurrent candidate. It is launched only after the
/// non-relay set has settled without a winner.
#[test]
fn relay_is_dialled_only_after_every_nonrelay_candidate_settles() {
    let mut plan = race_plan();
    plan.candidates.truncate(1);
    plan.candidates.push(Candidate {
        url: "https://relay.example.test:443".into(),
        scheme: Scheme::Https,
        location: probe::Location::Relay,
        address: "relay.example.test".into(),
        port: 443,
        ipv6: false,
        credential_eligible: true,
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_dial = Arc::clone(&seen);
    let dial: ProbeDial = status_dial(move |origin, _, _| {
        seen_by_dial.lock().unwrap().push(origin.host().to_string());
        if origin.host() == "relay.example.test" {
            (200, identity_json("race-machine"))
        } else {
            (0, Vec::new())
        }
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Relay));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        ["192-0-2-10.h.plex.direct", "relay.example.test"]
    );
}

#[test]
fn relay_only_server_gets_a_fresh_probe_budget_after_direct_timeouts_and_is_admitted() {
    let mut resource = resource(
        r#"{"name":"ours","clientIdentifier":"race-machine","provides":"server","owned":true,
            "accessToken":"profile-token","connections":[
              {"protocol":"https","address":"192.0.2.10","port":32400,
               "uri":"https://192-0-2-10.h.plex.direct:32400","local":true,"relay":false},
              {"protocol":"https","address":"relay.example.test","port":443,
               "uri":"https://relay.example.test:443","local":false,"relay":true}]}"#,
    );
    resource.public_address_matches = true;
    let policy = ProbeDeadlines {
        local: Duration::from_millis(10),
        remote: Duration::from_millis(50),
    };
    let relay_budgets = Arc::new(Mutex::new(Vec::new()));
    let relay_budgets_at_dial = Arc::clone(&relay_budgets);
    let dial: ProbeDial = status_dial(move |origin, _, budget| {
        if origin.host() == "relay.example.test" {
            relay_budgets_at_dial.lock().unwrap().push(budget);
            if budget == policy.remote {
                return (200, identity_json("race-machine"));
            }
            return (0, Vec::new());
        }
        std::thread::sleep(policy.local + Duration::from_millis(5));
        (0, Vec::new())
    });
    let plan = probe::plan(&resource, CredentialPolicy::HttpsOnly);
    let mut probe_one = move |_: &ProbePlan, _: &[String]| {
        probe_server_racing(&plan, Arc::clone(&dial), &threaded_spawn, policy,
            &mut |_, _, _| {})
    };
    let mut admissions = 0;
    let resolved = resolve_roster_using_admission(
        &[resource], &[], CredentialPolicy::HttpsOnly, &PlaintextAsk::undecided(), &mut probe_one,
        &mut |_| {
            admissions += 1;
            crate::plex::EndpointAdmission::Usable
        },
        &mut |_, _, _| {}, &mut || {}, &mut |_, _, _, _| {},
    );

    assert!(matches!(resolved.outcome, Resolved::Reached(ref found)
        if found.len() == 1 && found[0].tier == Some(probe::Location::Relay)));
    assert_eq!(resolved.admitted_machine_id.as_deref(), Some("race-machine"));
    assert_eq!(admissions, 1, "the relay winner must reach authenticated admission");
    assert_eq!(relay_budgets.lock().unwrap().as_slice(), [policy.remote],
        "the relay identity phase owns a fresh remote probe budget");
}

#[test]
fn a_reachable_relay_beats_a_direct_proxy_401() {
    let mut plan = race_plan();
    plan.candidates.truncate(1);
    plan.candidates.push(Candidate {
        url: "https://relay.example.test:443".into(),
        scheme: Scheme::Https,
        location: probe::Location::Relay,
        address: "relay.example.test".into(),
        port: 443,
        ipv6: false,
        credential_eligible: true,
    });
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host() == "relay.example.test" {
            (200, identity_json("race-machine"))
        } else {
            (401, Vec::new())
        }
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Relay));
}

#[test]
fn discovery_retries_the_same_server_via_relay_after_direct_admission_times_out() {
    let resource = resource(
        r#"{"name":"ours","clientIdentifier":"machine","provides":"server","owned":true,
            "accessToken":"profile-token","connections":[
              {"protocol":"https","address":"192.0.2.10","port":32400,
               "uri":"https://192-0-2-10.h.plex.direct:32400","local":true,"relay":false},
              {"protocol":"https","address":"relay.example.test","port":443,
               "uri":"https://relay.example.test:443","local":false,"relay":true}]}"#,
    );
    let plan = probe::plan(&resource, CredentialPolicy::HttpsOnly);
    let direct = plan.candidates.iter().find(|c| c.location != probe::Location::Relay).unwrap().clone();
    let relay = plan.candidates.iter().find(|c| c.location == probe::Location::Relay).unwrap().clone();
    let mut probes = 0;
    let mut probe_one = |_: &ProbePlan, rejected: &[String]| {
        probes += 1;
        if probes == 2 {
            assert_eq!(rejected, ["https://192-0-2-10.h.plex.direct:32400"]);
        }
        let candidate = if probes == 1 { direct.clone() } else { relay.clone() };
        Reach::At(candidate.clone(), candidate.origin().unwrap())
    };
    let mut admissions = vec![crate::plex::EndpointAdmission::Timeout,
        crate::plex::EndpointAdmission::Usable].into_iter();
    let resolved = resolve_roster_using_admission(
        &[resource], &[], CredentialPolicy::HttpsOnly, &PlaintextAsk::undecided(), &mut probe_one,
        &mut |_| admissions.next().unwrap(), &mut |_, _, _| {},
        &mut || {}, &mut |_, _, _, _| {},
    );
    assert_eq!(resolved.admitted_machine_id.as_deref(), Some("machine"));
    let Resolved::Reached(found) = resolved.outcome else { panic!("relay admission must retain the server") };
    assert_eq!(probes, 2);
    assert_eq!(found[0].origin_url, "https://relay.example.test:443");
}

#[test]
fn a_direct_401_remains_the_reason_when_relay_is_silent() {
    let mut plan = race_plan();
    plan.candidates.truncate(1);
    plan.candidates.push(Candidate {
        url: "https://relay.example.test:443".into(),
        scheme: Scheme::Https,
        location: probe::Location::Relay,
        address: "relay.example.test".into(),
        port: 443,
        ipv6: false,
        credential_eligible: true,
    });
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host() == "relay.example.test" {
            (0, Vec::new())
        } else {
            (401, Vec::new())
        }
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(matches!(reach, Reach::Refused));
}

/// A result's completion timestamp, not a delayed coordinator observation, decides whether it
/// met the deadline. The injected spawn holds the coordinator after the job has already sent.
#[test]
fn an_on_time_result_queued_before_the_deadline_survives_coordinator_delay() {
    let mut plan = race_plan();
    plan.candidates.truncate(1);
    let dial: ProbeDial = status_dial(|_, _, _| (200, identity_json("race-machine")));
    let spawn = |_: usize, job: ProbeJob| {
        job();
        std::thread::sleep(Duration::from_millis(20));
        true
    };
    let policy = ProbeDeadlines {
        local: Duration::from_millis(5),
        remote: Duration::from_millis(5),
    };
    let reach = probe_server_racing(&plan, dial, &spawn, policy, &mut |_, _, _| {});
    assert!(matches!(reach, Reach::At(..)));
}

#[test]
fn a_late_local_result_is_ignored_while_a_remote_deadline_remains_live() {
    let plan = race_plan();
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host().starts_with("192-") {
            std::thread::sleep(Duration::from_millis(25));
        } else {
            std::thread::sleep(Duration::from_millis(35));
        }
        (200, identity_json("race-machine"))
    });
    let policy = ProbeDeadlines {
        local: Duration::from_millis(5),
        remote: Duration::from_millis(100),
    };
    let mut activated = Vec::new();
    let reach = probe_server_racing(&plan, dial, &threaded_spawn, policy, &mut |_, c, _| {
        activated.push(c.location)
    });
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Remote));
    assert_eq!(activated, [probe::Location::Remote]);
}

/// A proxy-specific 401 can race a verified answer on another origin. Reachability wins when
/// identity was actually proved; 401 is the final reason only when no candidate reaches.
#[test]
fn a_verified_reachable_candidate_wins_over_a_parallel_401() {
    let plan = race_plan();
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host().starts_with("192-") {
            (401, Vec::new())
        } else {
            (200, identity_json("race-machine"))
        }
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Remote));
}

/// **Dev counterpart of the issue #95 fixtures: under `AllowPlaintext` the plaintext twin is
/// eligible, wins the race and is activated — exactly the behaviour every build had before
/// this credential-eligibility rule existed, and exactly what a developer build with no TLS
/// server of its own still needs.** Relay must never be dialled once it wins.
#[test]
fn under_allow_plaintext_the_lan_twin_wins_and_relay_is_never_dialled() {
    let mut plan = race_plan();
    plan.policy = CredentialPolicy::AllowPlaintext;
    plan.candidates[0] = Candidate {
        url: "http://192.0.2.10:32400".into(),
        scheme: Scheme::Http,
        location: probe::Location::Local,
        address: "192.0.2.10".into(),
        port: 32400,
        ipv6: false,
        credential_eligible: true,
    };
    plan.candidates.push(Candidate {
        url: "https://relay.example.test:443".into(),
        scheme: Scheme::Https,
        location: probe::Location::Relay,
        address: "relay.example.test".into(),
        port: 443,
        ipv6: false,
        credential_eligible: true,
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_dial = Arc::clone(&seen);
    let dial: ProbeDial = status_dial(move |origin, _, _| {
        seen_by_dial.lock().unwrap().push(origin.host().to_string());
        if origin.host() == "192.0.2.10" {
            (200, identity_json("race-machine"))
        } else {
            (0, Vec::new())
        }
    });
    let mut activated = Vec::new();
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, origin| activated.push(origin.clone()),
    );
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Local));
    assert!(
        !seen.lock().unwrap().iter().any(|s| s.contains("relay")),
        "relay must not be dialled once the eligible plaintext twin verifies: {:?}",
        seen.lock().unwrap()
    );
    assert_eq!(activated.len(), 1);
    assert!(!activated[0].is_tls(), "the LAN plaintext twin was activated");
}

/// **Precedence: `Reach::InsecureOnly` outranks `Reach::Refused`.** A verified identity —
/// even one this build cannot put a credential on — is stronger evidence than a 401 from a
/// different candidate; silence is weaker still.
#[test]
fn insecure_only_outranks_a_refusal() {
    let mut plan = race_plan();
    plan.candidates[0] = Candidate {
        url: "http://192.0.2.10:32400".into(),
        scheme: Scheme::Http,
        location: probe::Location::Local,
        address: "192.0.2.10".into(),
        port: 32400,
        ipv6: false,
        credential_eligible: false,
    };
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host() == "192.0.2.10" {
            (200, identity_json("race-machine")) // verified, but ineligible
        } else {
            (401, Vec::new())
        }
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(
        matches!(reach, Reach::InsecureOnly(..)),
        "a verified plaintext answer must outrank a parallel 401"
    );
}

/// **A late eligible answer, arriving after an early ineligible one, still becomes `first`
/// and is activated exactly once.** The ineligible answer never occupies the `first`/`best`
/// slots (`settle_probe_message`), so it cannot cause a spurious re-point when the eligible
/// candidate settles afterwards.
#[test]
fn a_late_eligible_answer_after_an_early_plaintext_one_becomes_first_and_activates_once() {
    let mut plan = race_plan();
    plan.candidates[0] = Candidate {
        url: "http://192.0.2.10:32400".into(),
        scheme: Scheme::Http,
        location: probe::Location::Local,
        address: "192.0.2.10".into(),
        port: 32400,
        ipv6: false,
        credential_eligible: false,
    };
    let dial: ProbeDial = status_dial(|origin, _, _| {
        if origin.host() == "192.0.2.10" {
            (200, identity_json("race-machine")) // instant, but ineligible
        } else {
            std::thread::sleep(Duration::from_millis(30));
            (200, identity_json("race-machine")) // eligible, and late
        }
    });
    let mut activated = Vec::new();
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, origin| activated.push(origin.clone()),
    );
    assert!(matches!(reach, Reach::At(ref c, _) if c.location == probe::Location::Remote));
    assert_eq!(
        activated.len(),
        1,
        "the late eligible answer becomes first, not a second re-point: {activated:?}"
    );
    assert!(activated[0].is_tls());
}

/// **Invariant: under `HttpsOnly`, every origin this coordinator ever activates, and every
/// `Reach::At` origin it returns, is TLS** — across several of this file's own racing
/// fixtures, including the issue #95 topology whose whole point is a plaintext winner that
/// must never surface as either.
#[test]
fn under_https_only_every_activation_and_every_reach_at_origin_is_tls() {
    let mut activated_total = 0;
    let mut assert_case = |plan: &ProbePlan, dial: ProbeDial| {
        let mut activated = Vec::new();
        let reach = probe_server_racing(
            plan,
            dial,
            &threaded_spawn,
            test_policy(),
            &mut |_, _, origin| activated.push(origin.clone()),
        );
        if let Reach::At(_, ref origin) = reach {
            assert!(
                origin.is_tls(),
                "Reach::At must be TLS under HttpsOnly: {}",
                origin.base()
            );
        }
        for o in &activated {
            assert!(
                o.is_tls(),
                "an activated origin must be TLS under HttpsOnly: {}",
                o.base()
            );
        }
        activated_total += activated.len();
    };

    assert_case(
        &race_plan(),
        status_dial(|origin, _, _| {
            if origin.host().starts_with("192-") {
                (200, identity_json("race-machine"))
            } else {
                (0, Vec::new())
            }
        }),
    );
    assert_case(
        &probe::plan(&issue_95_account(true), CredentialPolicy::HttpsOnly),
        status_dial(issue_95_dial),
    );
    assert!(
        activated_total > 0,
        "the invariant must have been exercised by at least one activation, not vacuously true"
    );
}

// ---- issue #95: a plaintext-only winner is reported reached and starves the relay ----
//
// Reporter topology (v0.6.6, `/api/v2/resources` for one OWNED, `httpsRequired:false` server):
// fourteen `local=1` Docker bridge gateways (172.17.0.1..172.30.0.1), each dead over both its
// advertised HTTPS uri and its synthesized plaintext twin; the REAL LAN connection, whose HTTPS
// `plex.direct` name this (internet-less) LAN cannot resolve but whose plaintext twin answers
// 200 with the right `machineIdentifier`; a remote custom HTTPS connection on port 443 whose
// HTTPS candidate is also dead and whose plaintext twin answers 400; and a relay connection
// that verifies over HTTPS. A store (non-`devtriggers`) build refuses to put a token on
// plaintext (`http::credential_transport_allowed`), so the LAN twin's 200 is a real answer this
// build can never use — and it must not be treated as reached, or relay never gets dialled.

/// Fourteen dead Docker bridge gateways, `local=1` in plex.tv's own (RFC1918) sense. Both
/// halves of each twin are wired dead in [`issue_95_dial`] — a real dial failure at every one
/// of them, not a fixture that never reaches these candidates at all.
fn issue_95_dead_gateways_json() -> String {
    (17..=30)
        .map(|i| {
            format!(
                r#",{{"protocol":"https","address":"172.{i}.0.1","port":32400,
                     "uri":"https://172-{i}-0-1.h.plex.direct:32400","local":true,"relay":false,"IPv6":false}}"#
            )
        })
        .collect()
}

/// The reporter's server, `owned:true` and `httpsRequired:false` — the shape that makes
/// `probe::candidates` synthesize a plaintext twin for every non-relay connection at all
/// (`probe.rs`'s rule 2). `with_relay` lets the no-relay variant reuse the same LAN/remote
/// shape without the one candidate that lets the race recover.
fn issue_95_account(with_relay: bool) -> Resource {
    let gateways = issue_95_dead_gateways_json();
    let relay = if with_relay {
        r#",{"protocol":"https","address":"relay.example.net","port":8443,
             "uri":"https://relay.example.net:8443","local":false,"relay":true,"IPv6":false}"#
    } else {
        ""
    };
    resource(&format!(
        r#"{{"name":"issue-95","clientIdentifier":"issue95mid","provides":"server","owned":true,
            "sourceTitle":null,"publicAddressMatches":true,"httpsRequired":false,
            "accessToken":"tok-95","connections":[
              {{"protocol":"https","address":"192.168.1.50","port":32400,
               "uri":"https://192-168-1-50.h.plex.direct:32400","local":true,"relay":false,"IPv6":false}}{gateways},
              {{"protocol":"https","address":"custom.example.net","port":443,
               "uri":"https://custom.example.net:443","local":false,"relay":false,"IPv6":false}}{relay}
            ]}}"#
    ))
}

/// What actually answers each candidate the fixture above generates. Keyed on `(host, is_tls)`
/// because the remote custom connection's plaintext twin shares its HOST with its HTTPS
/// candidate — only the scheme tells them apart — while the LAN connection's twin has a
/// DIFFERENT host from its `plex.direct` name, exactly as a real advertised uri does.
/// Ignores `_pin` on purpose: this fixture keeps testing the UNPINNED path (the plaintext
/// candidate answers by its literal address already, and no `.plex.direct` host appears here),
/// so a pin — present or not — must not change what it returns. `issue_95_dial_pinned` below is
/// the pinning-specific fixture.
fn issue_95_dial(
    origin: &Origin,
    _pin: Option<&crate::plex::ResolvePin>,
    _budget: Duration,
) -> (i32, Vec<u8>) {
    match (origin.host(), origin.is_tls()) {
        ("192.168.1.50", false) => (200, identity_json("issue95mid")),
        ("custom.example.net", false) => (400, Vec::new()),
        ("relay.example.net", true) => (200, identity_json("issue95mid")),
        _ => (0, Vec::new()), // every gateway twin, and both dead-HTTPS candidates
    }
}

/// **The direct race alone: a plaintext-only winner must not be `Reach::At`, and must not
/// starve the relay that verifies.**
///
/// This is now true by construction rather than by a second cfg-independent re-grading at the
/// end: `Candidate::credential_eligible` is computed once, at synthesis, from the
/// `CredentialPolicy` this plan was built with (here `HttpsOnly`, so a plaintext twin is
/// ineligible regardless of `devtriggers`), and `settle_probe_message` never lets an
/// ineligible winner become `result.first`/`result.best` — it becomes `result.insecure`
/// instead, which does not stop `probe_server_racing`'s `if batch.first.is_none() &&
/// !relay.is_empty()` from still running the relay batch. So this test drives the fixture
/// through the STORE policy directly and asserts the outcome; it needs no re-grading of what
/// `activate` was called with, because an ineligible candidate now never reaches `activate`
/// at all (see `settle_probe_message`).
#[test]
fn issue_95_a_plaintext_only_winner_must_not_be_reach_at_and_must_not_starve_the_relay() {
    let plan = probe::plan(&issue_95_account(true), CredentialPolicy::HttpsOnly);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_dial = Arc::clone(&seen);
    let dial: ProbeDial = status_dial(move |origin, pin, budget| {
        seen_by_dial.lock().unwrap().push(origin.log_form());
        issue_95_dial(origin, pin, budget)
    });
    let mut activated = Vec::new();
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, origin| activated.push(origin.clone()),
    );

    let Reach::At(_, ref origin) = reach else {
        panic!(
            "the relay verified this machine and must be reached: {:?}",
            seen.lock().unwrap()
        )
    };
    assert_eq!(
        origin.host(),
        "relay.example.net",
        "a plaintext-only winner must not be the reported origin — got {} (dialled: {:?})",
        origin.base(),
        seen.lock().unwrap()
    );
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("relay.example.net")),
        "relay was never dialled: {:?}",
        seen.lock().unwrap()
    );
    for origin in &activated {
        assert!(
            CredentialPolicy::HttpsOnly.may_carry_credential(origin),
            "activated a plaintext origin no store build can ever put a token on: {}",
            origin.base()
        );
    }
}

/// **The whole-roster path: a store build's roster must only ever record an origin it can put
/// a credential on.** Same topology, through [`resolve_roster_using`] rather than the racing
/// coordinator alone, because `SourceRef::origin_url` — not `Reach` — is what a boot actually
/// persists and re-dials from.
#[test]
fn issue_95_resolve_roster_only_ever_records_an_https_origin() {
    let resources = vec![issue_95_account(true)];
    let dial: ProbeDial = status_dial(issue_95_dial);
    let mut probe_one = |plan: &ProbePlan| {
        probe_server_racing(
            plan,
            Arc::clone(&dial),
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        )
    };
    let resolved =
        resolve_roster_using(
            &resources,
            &[],
            CredentialPolicy::HttpsOnly,
            &mut probe_one,
            &mut || {},
            &mut |_, _, _, _| {},
        );
    let Resolved::Reached(roster) = resolved else {
        panic!("the relay verifies this machine and must be recorded as reached");
    };
    assert_eq!(roster.len(), 1);
    assert!(
        roster[0].origin_url.starts_with("https://"),
        "a store build can never put a credential on the recorded origin otherwise: {}",
        roster[0].origin_url
    );
}

/// **Without a relay to fall back to, a plaintext-only answer must not be reported reached at
/// all** — the account has no address this build can use, and the whole point of `Reach`'s
/// three-way split (`probe.rs`'s module doc) is that "reachable but unusable" must not read as
/// success.
#[test]
fn issue_95_without_a_relay_a_plaintext_only_answer_is_not_reached() {
    let resources = vec![issue_95_account(false)];
    let dial: ProbeDial = status_dial(issue_95_dial);
    let mut probe_one = |plan: &ProbePlan| {
        probe_server_racing(
            plan,
            Arc::clone(&dial),
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        )
    };
    let resolved =
        resolve_roster_using(
            &resources,
            &[],
            CredentialPolicy::HttpsOnly,
            &mut probe_one,
            &mut || {},
            &mut |_, _, _, _| {},
        );
    assert!(
        !matches!(resolved, Resolved::Reached(_)),
        "a plaintext-only answer this build cannot use must not be reported reached"
    );
}

// ---- issue #95, step 5: InsecureOnly through the outcome consumers ----

/// `Resolved::None{insecure:true}` becomes `Discovery::InsecureOnly` REGARDLESS of `refused`
/// (plan §4's precedence: `InsecureOnly` beats `Refused`), and every other shape keeps its old
/// mapping. `resolved_without_roster` is the pure fold `discover_and_store` runs this through.
#[test]
fn resolved_none_insecure_outranks_refused_and_every_other_shape_is_unchanged() {
    assert!(matches!(
        resolved_without_roster(Resolved::NoServers { resources: 0 }, DiscoveryTrigger::Login),
        Err(Discovery::NoServers(_))
    ));
    assert!(matches!(
        resolved_without_roster(Resolved::None { refused: true, insecure: false, evidence: None }, DiscoveryTrigger::Login),
        Err(Discovery::Refused)
    ));
    assert!(matches!(
        resolved_without_roster(Resolved::None { refused: false, insecure: false, evidence: None }, DiscoveryTrigger::Login),
        Err(Discovery::ServersUnreachable { trigger: DiscoveryTrigger::Login })
    ));
    assert!(
        matches!(
            resolved_without_roster(Resolved::None { refused: true, insecure: true, evidence: None }, DiscoveryTrigger::Login),
            Err(Discovery::InsecureOnly(_))
        ),
        "a verified plaintext answer outranks a parallel/proxy 401"
    );
    assert!(matches!(
        resolved_without_roster(Resolved::None { refused: false, insecure: true, evidence: None }, DiscoveryTrigger::Login),
        Err(Discovery::InsecureOnly(_))
    ));
    assert!(matches!(resolved_without_roster(Resolved::Reached(vec![]), DiscoveryTrigger::Login), Ok(v) if v.is_empty()));
}

/// Sign-in and rediscovery must say the SAME sentence for the SAME verdict — the whole point
/// of a shared const (plan §4) is that the two paths cannot drift apart on this copy the way
/// the three other Discovery failures never had a name collision to drift on. `discover_and_store`
/// itself needs a live plex.tv edge no host test can reach, so this pins the SHARED CONST's
/// content directly; the two call sites (`login_worker_with_output`, `rediscovery_worker_with_output`)
/// are both spelled `output_failed(output, epoch, discovery_insecure_only_message())` — greppable,
/// and unable to drift apart without a compile error renaming one identifier but not the other.
#[test]
fn insecure_only_copy_is_one_shared_const_naming_the_fixable_cause() {
    assert!(!discovery_insecure_only_message().is_empty());
    assert!(discovery_insecure_only_message().contains("securely"));
    assert!(discovery_insecure_only_message().contains("HTTPS"));
}

// ---- issue #95, step 4: probe pinning ----
//
// A router with DNS-rebind protection answers every `*.plex.direct` name with NXDOMAIN, so the
// TLS LAN candidate above never even reached a socket — only its ineligible plaintext twin did,
// which is what left the account stuck on relay. `race_batch` now pins that candidate exactly
// as `apply_candidate_activation` pins the winner it persists (`ResolvePin::for_origin`, keyed
// on `Candidate::address`), so the fixture below answers the SAME topology as
// `issue_95_account`, except the LAN TLS candidate now verifies too — but only when it is
// dialled with the pin `192.168.1.50` decodes to, standing in for "no resolver reached this
// name, but the pinned address did."

/// Identical to [`issue_95_dial`] except the LAN `plex.direct` TLS candidate now answers, and
/// only when dialled with the pin its own label decodes to — standing in for the DNS-rebind
/// router, which would otherwise make this exact candidate NXDOMAIN.
fn issue_95_dial_pinned(
    origin: &Origin,
    pin: Option<&crate::plex::ResolvePin>,
    budget: Duration,
) -> (i32, Vec<u8>) {
    if origin.host() == "192-168-1-50.h.plex.direct" && origin.is_tls() {
        return if pin.is_some_and(|p| p.addr() == "192.168.1.50".parse::<std::net::IpAddr>().unwrap()) {
            (200, identity_json("issue95mid"))
        } else {
            (0, Vec::new()) // no resolver reaches this name on the reporter's LAN
        };
    }
    issue_95_dial(origin, pin, budget)
}

/// **A pinned LAN `plex.direct` candidate wins outright, and the relay is never dialled.**
/// This is the fix for #95 itself: with the pin, the TLS candidate a DNS-rebind-protected
/// router used to make unreachable now verifies first — `credential_eligible`, `local` tier —
/// so `probe_server_racing`'s `first.is_none() && !relay.is_empty()` gate never opens.
#[test]
fn a_pinned_lan_https_candidate_wins_and_the_relay_is_never_dialled() {
    let plan = probe::plan(&issue_95_account(true), CredentialPolicy::HttpsOnly);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_dial = Arc::clone(&seen);
    let dial: ProbeDial = status_dial(move |origin, pin, budget| {
        seen_by_dial.lock().unwrap().push(origin.log_form());
        issue_95_dial_pinned(origin, pin, budget)
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );

    let Reach::At(candidate, origin) = reach else {
        panic!(
            "the pinned LAN candidate verifies and must be reached: {:?}",
            seen.lock().unwrap()
        )
    };
    assert_eq!(origin.base(), "https://192-168-1-50.h.plex.direct:32400");
    assert_eq!(candidate.location, probe::Location::Local);
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.contains("relay.example.net")),
        "the pinned LAN candidate verified; relay must not have been dialled: {:?}",
        seen.lock().unwrap()
    );
}

/// **The same fixture through the whole-roster path**: what a boot actually persists is
/// `SourceRef::origin_url`, and a pinned winner must persist as the `plex.direct` NAME — never
/// the bare address the pin dialled it at — so a later boot or data call re-derives the same
/// pin from the same stored address (`session.rs` re-installs through `register_origin`/
/// `install`, both of which take a fresh `ResolvePin::for_origin` computed the identical way).
#[test]
fn a_pinned_winner_is_recorded_as_the_plex_direct_origin_not_the_dialled_address() {
    let resources = vec![issue_95_account(true)];
    let dial: ProbeDial = status_dial(issue_95_dial_pinned);
    let mut probe_one = |plan: &ProbePlan| {
        probe_server_racing(
            plan,
            Arc::clone(&dial),
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        )
    };
    let resolved =
        resolve_roster_using(
            &resources,
            &[],
            CredentialPolicy::HttpsOnly,
            &mut probe_one,
            &mut || {},
            &mut |_, _, _, _| {},
        );
    let Resolved::Reached(roster) = resolved else {
        panic!("the pinned LAN candidate verifies and must be recorded as reached");
    };
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].origin_url, "https://192-168-1-50.h.plex.direct:32400");
    assert_eq!(roster[0].address, "192.168.1.50");
    assert_eq!(roster[0].tier, Some(probe::Location::Local));
}

/// A `Resource` fixture for the issue #95 E2E race: one LAN `plex.direct` HTTPS candidate at
/// `lan_port`, a dead gateway at `dead_port` (same address, a port nothing answers — pinned
/// exactly like the real candidate, so it proves a pin alone is not enough to win), and,
/// when `relay_port` is `Some`, a relay candidate over a bare-IP HTTPS uri (no pin needed:
/// `ResolvePin::for_origin` only fires for a `plex.direct` name).
fn e2e95_resource(lan_port: u16, dead_port: u16, relay_port: Option<u16>) -> Resource {
    let relay = match relay_port {
        Some(p) => format!(
            r#",{{"protocol":"https","address":"127.0.0.1","port":{p},
                 "uri":"https://127.0.0.1:{p}","local":false,"relay":true,"IPv6":false}}"#
        ),
        None => String::new(),
    };
    resource(&format!(
        r#"{{"name":"e2e95","clientIdentifier":"e2e95mid","provides":"server","owned":true,
            "sourceTitle":null,"publicAddressMatches":true,"httpsRequired":false,
            "accessToken":"tok-e2e95","connections":[
              {{"protocol":"https","address":"127.0.0.1","port":{lan_port},
               "uri":"https://127-0-0-1.e2e95.plex.direct:{lan_port}","local":true,"relay":false,"IPv6":false}},
              {{"protocol":"https","address":"127.0.0.1","port":{dead_port},
               "uri":"https://127-0-0-1.e2e95dead.plex.direct:{dead_port}","local":true,"relay":false,"IPv6":false}}{relay}
            ]}}"#
    ))
}

/// **The real curl/TLS stack reaches the pinned HTTPS LAN candidate and never activates its
/// plaintext twin.** Issue #95's shape, driven through the PRODUCTION dial
/// (`get_identity` → `crate::http::request_probe_learning_key` → `crate::net::request_result_evidence`) rather
/// than a fake [`ProbeDial`] closure: a loopback double
/// ([`crate::net::spawn_dual_protocol`]) answers the SAME `/identity` body over
/// both a real TLS handshake (against a minted self-signed cert curl is told to trust via
/// `net::test_ca_bundle`, the same seam `request_tls_evidence` reads `CURLOPT_CAINFO` from)
/// and plaintext HTTP, on one port — exactly what `probe::candidates` assumes when it
/// synthesizes a plaintext twin at a connection's own address and port. A relay candidate (a
/// second loopback double, reached over a bare `127.0.0.1` uri covered by the same cert's IP
/// SAN) and a dead gateway round out the fixture. Under `CredentialPolicy::HttpsOnly` the
/// pinned LAN candidate must win outright, and no plaintext origin may ever be activated.
#[test]
fn e2e_real_curl_race_reaches_the_pinned_https_lan_candidate_over_a_real_tls_handshake() {
    let _serial = crate::testlock::serial();
    if !(crate::net::global_init() && crate::net::available()) {
        eprintln!("curl unavailable on this host; skipping");
        return;
    }
    // The verified probe remembers the server's key (issue #380): into a scratch session, never
    // the developer's own.
    let _session = crate::plex::session::TempSession::new("lan-race");
    let cert = std::sync::Arc::new(crate::net::mint_cert(&[
        "127-0-0-1.e2e95.plex.direct",
        "127.0.0.1",
    ]));
    let _ca = TestCaGuard::install(&cert.pem, "lan-race");

    let body = identity_json("e2e95mid");
    let lan_port = crate::net::spawn_dual_protocol(Arc::clone(&cert), body.clone());
    let relay_port = crate::net::spawn_dual_protocol(Arc::clone(&cert), body.clone());
    let dead = crate::net::dead_port();

    let resource = e2e95_resource(lan_port, dead, Some(relay_port));
    let plan = probe::plan(&resource, CredentialPolicy::HttpsOnly);

    let dial: ProbeDial = Arc::new(get_identity);
    let mut activated = Vec::new();
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, origin| activated.push(origin.clone()),
    );

    let Reach::At(candidate, origin) = reach else {
        panic!("the pinned LAN candidate answers a real identity over real TLS and must be reached");
    };
    assert_eq!(origin.host(), "127-0-0-1.e2e95.plex.direct");
    assert!(origin.is_tls());
    assert_eq!(candidate.location, probe::Location::Local);
    for activated_origin in &activated {
        assert!(
            CredentialPolicy::HttpsOnly.may_carry_credential(activated_origin),
            "a plaintext origin must never be activated under HttpsOnly: {}",
            activated_origin.base()
        );
    }
    // The same race, through `race_batch`'s real worker, taught the session the LAN leaf's key.
    assert_eq!(
        learned_pin("e2e95mid"),
        Some(crate::spki::pin_from_spki_der(&cert.spki_der)),
        "a verified, accepted LAN answer is remembered"
    );
}

/// **A valid certificate presented outside its validity window is logged as such, not as a stale
/// CA store.** GitHub discussion #351: a television cold-booted with no internet has a wrong clock,
/// so the server's genuine `*.plex.direct` leaf fails libcurl's validity check (rc=60), which must
/// not be reported as a CA bundle problem. The leaf here chains to a trusted CA and expired thirty days ago
/// — dates are relative to now, so the test does not rot — and the real TLS stack must fail it and
/// the event log must say why. The host's libcurl (LibreSSL, 8.x) is not the television's
/// (OpenSSL, 7.53.1): when it does not report the X509 verify result at all, the line must still
/// admit that rather than blame the CA store, and the test says which one it saw.
#[test]
fn an_expired_leaf_is_logged_as_expired_not_as_a_stale_ca_store() {
    let _serial = crate::testlock::serial();
    if !(crate::net::global_init() && crate::net::available()) {
        eprintln!("curl unavailable on this host; skipping");
        return;
    }
    let cert = std::sync::Arc::new(crate::net::mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-90), ymd_from_now(-30)));
    let _ca = TestCaGuard::install(&cert.pem, "expired-leaf");
    let port = crate::net::spawn_dual_protocol(std::sync::Arc::clone(&cert), identity_json("expired"));

    let log = crate::events_log();
    let before = std::fs::metadata(&log).map_or(0, |m| m.len());
    let out = crate::net::request_result_evidence(
        &format!("https://127.0.0.1:{port}/identity"),
        &[],
        "GET",
        None,
        crate::net::API,
        false,
        None,
        None,
        false,
    );
    let Err(failure) = out else { panic!("an expired leaf must not verify") };
    assert_eq!(failure.curl_rc, Some(60), "peer verification failure");

    let tail = std::fs::read(&log).map(|b| String::from_utf8_lossy(&b[before as usize..]).into_owned());
    let tail = tail.expect("event log readable");
    let line = tail
        .lines()
        .find(|l| l.contains("net: curl rc=60"))
        .unwrap_or_else(|| panic!("no rc=60 line in the log tail: {tail:?}"));
    assert!(!line.contains("CA store"), "an expired leaf is not a CA problem: {line}");
    if line.contains("has expired") {
        assert!(line.contains("clock"), "the clock is the actionable part: {line}");
    } else {
        eprintln!("host libcurl did not report X509_V_ERR_CERT_HAS_EXPIRED: {line}");
        assert!(line.contains("X509 verify result"), "an unexplained result prints its number: {line}");
    }
}

/// **The whole-roster path persists the pinned `plex.direct` origin, never the plaintext
/// twin, over the real dial.** Same fixture as the test above, through
/// `resolve_roster_using` — what a boot actually stores is `SourceRef::origin_url`.
#[test]
fn e2e_real_curl_resolve_roster_only_ever_records_the_pinned_https_origin() {
    let _serial = crate::testlock::serial();
    if !(crate::net::global_init() && crate::net::available()) {
        eprintln!("curl unavailable on this host; skipping");
        return;
    }
    let _session = crate::plex::session::TempSession::new("lan-roster");
    let cert = std::sync::Arc::new(crate::net::mint_cert(&[
        "127-0-0-1.e2e95.plex.direct",
        "127.0.0.1",
    ]));
    let _ca = TestCaGuard::install(&cert.pem, "lan-roster");

    let body = identity_json("e2e95mid");
    let lan_port = crate::net::spawn_dual_protocol(Arc::clone(&cert), body.clone());
    let dead = crate::net::dead_port();

    let resources = vec![e2e95_resource(lan_port, dead, None)];
    let dial: ProbeDial = Arc::new(get_identity);
    let mut probe_one = |plan: &ProbePlan| {
        probe_server_racing(
            plan,
            Arc::clone(&dial),
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        )
    };
    let resolved = resolve_roster_using(
        &resources,
        &[],
        CredentialPolicy::HttpsOnly,
        &mut probe_one,
        &mut || {},
        &mut |_, _, _, _| {},
    );
    let Resolved::Reached(roster) = resolved else {
        panic!("the pinned LAN candidate verifies over real TLS and must be reached");
    };
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].origin_url, format!("https://127-0-0-1.e2e95.plex.direct:{lan_port}"));
    assert_eq!(roster[0].address, "127.0.0.1");
    assert_eq!(roster[0].tier, Some(probe::Location::Local));
}

/// **A real failed TLS handshake plus a real plaintext answer must settle as `InsecureOnly`,
/// never `Reach::At` plaintext.** The advertised HTTPS uri points at a loopback double that
/// only ever speaks plaintext ([`crate::net::spawn_plain_only`]) — a real curl
/// TLS ClientHello against it fails the handshake — while the auto-synthesized plaintext twin
/// on the SAME port answers 200 with a correct identity body. No relay in this fixture, so
/// there is nothing else to win: the only verified answer is `!credential_eligible`, and under
/// `CredentialPolicy::HttpsOnly` that must never become the reachable origin.
#[test]
fn e2e_real_curl_tls_failure_with_a_verified_plaintext_answer_yields_insecure_only_not_reach_at() {
    let _serial = crate::testlock::serial();
    if !(crate::net::global_init() && crate::net::available()) {
        eprintln!("curl unavailable on this host; skipping");
        return;
    }
    // No cert/CA override needed: this candidate never completes a TLS handshake at all.
    // Must match `e2e95_resource`'s hardcoded `clientIdentifier` ("e2e95mid") — a mismatch
    // here makes verification fail as a wrong-machine answer instead of exercising the
    // insecure-plaintext path this test means to prove.
    let body = identity_json("e2e95mid");
    let plain_port = crate::net::spawn_plain_only(body.clone());
    let dead = crate::net::dead_port();

    let resource = e2e95_resource(plain_port, dead, None);
    let plan = probe::plan(&resource, CredentialPolicy::HttpsOnly);
    let dial: ProbeDial = Arc::new(get_identity);
    let reach = probe_server_racing(
        &plan,
        Arc::clone(&dial),
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(
        !matches!(reach, Reach::At(_, _)),
        "a plaintext-only verified answer must never become Reach::At"
    );

    let resources = vec![resource];
    let mut probe_one = |plan: &ProbePlan| {
        probe_server_racing(
            plan,
            Arc::clone(&dial),
            &threaded_spawn,
            test_policy(),
            &mut |_, _, _| {},
        )
    };
    let resolved = resolve_roster_using(
        &resources,
        &[],
        CredentialPolicy::HttpsOnly,
        &mut probe_one,
        &mut || {},
        &mut |_, _, _, _| {},
    );
    match &resolved {
        Resolved::None { insecure, .. } => {
            assert!(*insecure, "the plaintext twin verified this machine and must be recorded insecure");
        }
        Resolved::Reached(_) => panic!("expected Resolved::None{{insecure:true}}, got a Reached roster"),
        Resolved::NoServers { .. } => panic!("expected Resolved::None{{insecure:true}}, got NoServers"),
    }
    assert!(matches!(
        resolved_without_roster(resolved, DiscoveryTrigger::Login),
        Err(Discovery::InsecureOnly(Some(_)))
    ));
}

/// **A candidate whose dashed label does not encode the address stored beside it gets no
/// pin.** A stale plex.tv cache, a re-point, or any fixture where the two simply disagree must
/// not make `race_batch` guess — `ResolvePin::for_origin` already refuses this, and this test
/// is the boundary that would notice `race_batch` computing the pin from the wrong field.
#[test]
fn a_candidate_whose_label_does_not_encode_its_own_address_gets_no_pin() {
    let mismatched = Candidate {
        url: "https://192-0-2-10.h.plex.direct:32400".into(),
        scheme: Scheme::Https,
        location: probe::Location::Local,
        // What plex.tv advertised beside this connection does NOT decode from the label.
        address: "192.0.2.99".into(),
        port: 32400,
        ipv6: false,
        credential_eligible: true,
    };
    let plan = ProbePlan {
        machine_id: "mismatch-machine".into(),
        token: "tok".into(),
        owned: true,
        name: "mismatch-server".into(),
        source_title: None,
        candidates: vec![mismatched],
        policy: CredentialPolicy::HttpsOnly,
    };
    let seen_pin: Arc<Mutex<Option<Option<crate::plex::ResolvePin>>>> = Arc::new(Mutex::new(None));
    let seen_pin_by_dial = Arc::clone(&seen_pin);
    let dial: ProbeDial = status_dial(move |_origin, pin, _budget| {
        *seen_pin_by_dial.lock().unwrap() = Some(pin.cloned());
        (200, identity_json("mismatch-machine"))
    });
    let reach = probe_server_racing(
        &plan,
        dial,
        &threaded_spawn,
        test_policy(),
        &mut |_, _, _| {},
    );
    assert!(matches!(reach, Reach::At(_, _)));
    assert_eq!(
        seen_pin.lock().unwrap().clone(),
        Some(None),
        "a mismatched label must reach the dial with no pin at all"
    );
}

#[test]
fn servers_are_serial_owned_then_public_match_with_one_gap_between_each() {
    let resources = vec![
        resource(
            r#"{"name":"unmatched","clientIdentifier":"shared-u","provides":"server",
                     "owned":false,"publicAddressMatches":false}"#,
        ),
        resource(
            r#"{"name":"owned","clientIdentifier":"owned","provides":"server",
                     "owned":true,"publicAddressMatches":false}"#,
        ),
        resource(
            r#"{"name":"matched","clientIdentifier":"shared-m","provides":"server",
                     "owned":false,"publicAddressMatches":true}"#,
        ),
    ];
    let mut order = Vec::new();
    let mut gaps = 0;
    let resolved = resolve_roster_using(
        &resources,
        &[],
        CredentialPolicy::HttpsOnly,
        &mut |plan| {
            order.push(plan.machine_id.clone());
            Reach::No
        },
        &mut || gaps += 1,
        &mut |_, _, _, _| {},
    );
    assert!(matches!(resolved, Resolved::None { refused: false, .. }));
    assert_eq!(order, ["owned", "shared-m", "shared-u"]);
    assert_eq!(
        gaps, 2,
        "three serial servers have exactly two inter-server gaps"
    );
}

#[test]
fn every_server_settlement_publishes_its_specific_state_and_winning_tier() {
    let resources = vec![
        resource(r#"{"name":"yes","clientIdentifier":"yes","provides":"server","owned":true}"#),
        resource(
            r#"{"name":"denied","clientIdentifier":"denied","provides":"server","owned":false}"#,
        ),
        resource(
            r#"{"name":"off","clientIdentifier":"off","provides":"server","owned":false}"#,
        ),
    ];
    let winner = Candidate {
        url: "https://remote.example.test:32400".into(),
        scheme: Scheme::Https,
        location: probe::Location::Remote,
        address: "203.0.113.9".into(),
        port: 32400,
        ipv6: false,
        credential_eligible: true,
    };
    let origin = winner.origin().expect("fixture origin");
    let mut observed = Vec::new();
    let resolved = resolve_roster_using(
        &resources,
        &[],
        CredentialPolicy::HttpsOnly,
        &mut |plan| match plan.machine_id.as_str() {
            "yes" => Reach::At(winner.clone(), origin.clone()),
            "denied" => Reach::Refused,
            _ => Reach::No,
        },
        &mut || {},
        &mut |plan, outcome, tier, address| observed.push((plan.machine_id.clone(), outcome, tier, address)),
    );

    assert!(matches!(resolved, Resolved::Reached(ref roster) if roster.len() == 1));
    assert_eq!(
        observed,
        vec![
            (
                "yes".into(),
                Outcome::Reachable,
                Some(probe::Location::Remote),
                Some("203.0.113.9".into()),
            ),
            ("denied".into(), Outcome::Unauthorized, None, None),
            ("off".into(), Outcome::Unreachable, None, None),
        ]
    );
}

#[test]
fn a_changed_refresh_republishes_reached_unauthorized_and_offline_after_registry_replacement() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    let old = [
        crate::plex::register_for_test("yes", "10.0.0.1", 32400, "old", "cid"),
        crate::plex::register_for_test("denied", "10.0.0.2", 32400, "old", "cid"),
        crate::plex::register_for_test("off", "10.0.0.3", 32400, "old", "cid"),
    ];
    for id in old {
        crate::plex::publish_probe_result(id, Outcome::Reachable);
    }

    // The changed=true refresh path resets every old profile fact before installing the final
    // roster. These registrations stand in for install_roster without its network side effect.
    crate::plex::revoke_for_profile_switch();
    let installed = [
        crate::plex::register_for_test("yes", "10.0.0.1", 32400, "new", "cid"),
        crate::plex::register_for_test("denied", "10.0.0.2", 32400, "new", "cid"),
        crate::plex::register_for_test("off", "10.0.0.3", 32400, "new", "cid"),
    ];
    crate::plex::client_for(installed[0])
        .unwrap()
        .set_link(probe::Location::Remote);
    crate::plex::client_for(installed[1])
        .unwrap()
        .set_link(probe::Location::Local);
    crate::plex::client_for(installed[2])
        .unwrap()
        .set_link(probe::Location::Relay);
    crate::plex::finish_profile_switch(&installed);
    assert!(installed
        .iter()
        .all(|&id| crate::plex::server_probe_result(id).is_none()));

    publish_settled_probes(&[
        SettledProbe {
            machine_id: "yes".into(),
            outcome: Outcome::Reachable,
            tier: Some(probe::Location::Remote),
            address: Some("203.0.113.9".into()),
        },
        SettledProbe {
            machine_id: "denied".into(),
            outcome: Outcome::Unauthorized,
            tier: None,
            address: None,
        },
        SettledProbe {
            machine_id: "off".into(),
            outcome: Outcome::Unreachable,
            tier: None,
            address: None,
        },
    ]);

    assert_eq!(
        crate::plex::server_probe_result(installed[0]),
        Some(Outcome::Reachable)
    );
    assert_eq!(
        crate::plex::server_probe_result(installed[1]),
        Some(Outcome::Unauthorized)
    );
    assert_eq!(
        crate::plex::server_probe_result(installed[2]),
        Some(Outcome::Unreachable)
    );
    assert_eq!(
        crate::plex::client_for(installed[0]).unwrap().link(),
        Some(probe::Location::Remote)
    );
    assert_eq!(
        crate::plex::client_for(installed[1]).unwrap().link(),
        Some(probe::Location::Local)
    );
    assert_eq!(
        crate::plex::client_for(installed[2]).unwrap().link(),
        Some(probe::Location::Relay)
    );
    crate::plex::reset_servers_for_test();
}

/// **Identity is verified before a connection is accepted.** A candidate that answers is not
/// the server we asked for: rule 1 of `probe.rs` is a live account of how a stranger's box on
/// our own LAN answers a probe, and accepting it would register their machine under our
/// friend's name and browse it.
///
/// The wrong machine is discarded and the NEXT candidate is tried — a mismatch is a fact about
/// that address, not about the server.
#[test]
fn a_response_from_the_wrong_machine_is_rejected_and_the_next_address_is_tried() {
    let plan = probe::plan(&a_share(), CredentialPolicy::HttpsOnly);
    let d = Dialled::new(vec![
        ("198-51-100-7.h.plex.direct", 200, identity_json("zzzz9999")), // someone else entirely
        ("203-0-113-9.h.plex.direct", 200, identity_json("bbbb2222")), // the server we asked for
    ]);

    match probe_server(&plan, &|o| d.dial(o)) {
        Reach::At(c, o) => {
            // The DIAGNOSTIC half is still the address plex.tv sent…
            assert_eq!((c.address.as_str(), c.port), ("203.0.113.9", 31234));
            // …and the origin is the NAME the certificate is issued for, which is the whole
            // reason `Reach::At` carries both. A roster rebuilt from `address` would store an
            // https origin no certificate matches.
            assert_eq!(o.base(), "https://203-0-113-9.h.plex.direct:31234");
        }
        _ => panic!(
            "the second address answers as the right machine: {:?}",
            d.seen()
        ),
    }
    // Every https candidate is tried before any plaintext one. Rule 1 keeps the guarded TLS
    // URI from the owner's LAN, but never its plaintext twin.
    assert_eq!(
        d.seen(),
        vec![
            "https://172-20-4-7.h.plex.direct:32400",
            "https://media.example.internal:31234",
            "https://198-51-100-7.h.plex.direct:31234",
            "https://203-0-113-9.h.plex.direct:31234",
        ]
    );

    // …and the same body from the wrong machine is never enough on its own
    assert_eq!(
        classify(200, &identity_json("zzzz9999"), "bbbb2222"),
        Outcome::WrongServer
    );
    assert_eq!(
        classify(200, &identity_json("bbbb2222"), "bbbb2222"),
        Outcome::Reachable
    );
    // a 200 that says nothing we can check is not an acceptance either
    assert_eq!(
        classify(200, b"<html>router login</html>", "bbbb2222"),
        Outcome::WrongServer
    );
    // nor is a resource plex.tv sent without an identity to verify against
    assert_eq!(
        classify(200, &identity_json("bbbb2222"), ""),
        Outcome::WrongServer
    );
}

/// The legacy synchronous seam stops at 401. Production races every direct candidate and lets
/// relay follow a direct proxy 401; the coordinator tests above grade those semantics. This
/// fixture remains only to pin the older one-at-a-time acceptance harness.
#[test]
fn the_legacy_sequential_seam_stops_at_401_instead_of_calling_it_a_dead_address() {
    assert_eq!(classify(401, b"", "bbbb2222"), Outcome::Unauthorized);
    // and it is the ONLY status that means this: a refusal of the endpoint, a dead gateway and
    // no answer at all are all just "try the next address"
    for s in [403, 404, 500, 502, 0] {
        assert_eq!(
            classify(s, b"", "bbbb2222"),
            Outcome::Unreachable,
            "status {s}"
        );
    }

    let plan = probe::plan(&a_share(), CredentialPolicy::HttpsOnly);
    let d = Dialled::new(vec![
        ("198-51-100-7.h.plex.direct", 401, Vec::new()),
        ("203.0.113.9", 200, identity_json("bbbb2222")),
    ]);
    assert!(matches!(
        probe_server(&plan, &|o| d.dial(o)),
        Reach::Refused
    ));
    assert_eq!(
        d.seen(),
        vec![
            "https://172-20-4-7.h.plex.direct:32400",
            "https://media.example.internal:31234",
            "https://198-51-100-7.h.plex.direct:31234",
        ],
        "the 401 ends the SERVER: the address that would have answered is never even tried"
    );
}

/// **Every advertised address is dialable now, and the only thing that can still refuse one is
/// a port no socket could take.** This test asserted the opposite for four shapes — an https
/// origin, a hostname, a v6 literal, and by implication the whole `plex.direct` fleet — and
/// each of those was true of a transport that no longer exists: `crate::http` routes TLS
/// through libcurl, and `stream.rs` resolves names and dials either address family.
///
/// The `probe_server` leg is the one that matters more than the table: it proves that opening
/// the transport did not open the ACCEPTANCE. Candidates are dialled here until only the one
/// nothing, and only the one whose `machineIdentifier` matches is accepted.
#[test]
fn every_advertised_address_is_dialable_and_only_an_impossible_port_is_not() {
    // AllowPlaintext: this test is about DIALABILITY, not credential eligibility, and its
    // fixture answers over the plaintext twin (`203.0.113.9`, no scheme).
    let plan = probe::plan(&a_share(), CredentialPolicy::AllowPlaintext);
    assert_eq!(
        plan.candidates.len(),
        7,
        "guarded LAN TLS plus three remote uri/twin pairs"
    );
    assert!(
        plan.candidates.iter().all(dialable),
        "not one of them is refused any more: {plan:#?}",
        plan = plan.candidates
    );

    let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
    assert!(matches!(probe_server(&plan, &|o| d.dial(o)), Reach::At(..)));
    // The owner's `172.20.x.x` connection keeps only the advertised TLS URI. Identity and the
    // certificate can reject a stranger there; the unsafe plaintext twin is never emitted.
    let seen = d.seen();
    assert!(
        seen.iter().any(|s| s.contains("172-20-4-7")),
        "the guarded TLS URI survives: {seen:?}"
    );
    assert!(
        !seen.iter().any(|s| s == "10.9.9.7:32400"),
        "the plaintext twin is absent: {seen:?}"
    );

    // The rule itself, stated on the candidates. The fixture builds `url` the way
    // `probe::candidates` does — from the SAME address and port — because that consistency is
    // the property `dial_target` relies on: it reads the origin off the URL, which is also what
    // gets recorded, so a fixture whose url and port disagree would assert nothing real.
    let cand = |scheme: Scheme, host: &str, port: i64| Candidate {
        url: format!(
            "{}://{}:{port}",
            scheme.as_str(),
            if host.contains(':') {
                format!("[{host}]")
            } else {
                host.to_string()
            }
        ),
        scheme,
        location: probe::Location::Remote,
        address: host.into(),
        port,
        ipv6: host.contains(':'),
        credential_eligible: scheme == Scheme::Https,
    };
    let at = |host: &str| cand(Scheme::Http, host, 32400);
    assert!(dialable(&at("203.0.113.9")));
    assert!(
        dialable(&cand(Scheme::Https, "203-0-113-9.h.plex.direct", 31234)),
        "libcurl speaks TLS"
    );
    assert!(
        dialable(&at("media.example.internal")),
        "stream.rs resolves names now"
    );
    assert!(
        dialable(&at("2001:db8::1")),
        "…and dials either address family"
    );

    // …and the PORT is the one narrowing left. `4_294_999_696 as i32` is 32400, so without the
    // range check `probe::dial_port` applies — inside `Origin::parse` now, one layer down from
    // where it used to be — a nonsense answer from plex.tv would have been dialled at the most
    // ordinary port there is.
    assert!(!dialable(&cand(Scheme::Http, "203.0.113.9", 4_294_999_696)));
    assert!(!dialable(&cand(Scheme::Http, "203.0.113.9", 0)));
    assert!(!dialable(&cand(Scheme::Http, "203.0.113.9", 70_000)));

    // **The predicate hands back the ORIGIN, and it is the one `probe_server` dials and
    // `resolve_roster` records.** One value, so the address that answered and the address
    // written down cannot be two different things — and for an https candidate the two really
    // do differ, which is why this is a value rather than a bool.
    assert_eq!(
        dial_target(&at("203.0.113.9")),
        Some(crate::plex::Origin::http("203.0.113.9", 32400))
    );
    assert_eq!(
        dial_target(&cand(Scheme::Https, "203-0-113-9.h.plex.direct", 31234)).map(|o| o.base()),
        Some("https://203-0-113-9.h.plex.direct:31234".to_string())
    );
}

/// A candidate whose port cannot be dialled is SKIPPED, exactly as a hostname is — the next
/// address gets its turn, and the server is not written off for one broken connection.
///
/// The failure this prevents is silent in both directions: with a wrapping `as i32` the app
/// dials port 32400 at that address, and whatever answers there is accepted the moment its
/// `machineIdentifier` matches — which, on a server that really is at 32400, it does.
#[test]
fn an_undialable_port_costs_that_candidate_and_not_the_server() {
    // AllowPlaintext: the fixture below answers over the plaintext twin.
    let mut plan = probe::plan(&a_share(), CredentialPolicy::AllowPlaintext);
    let good = plan
        .candidates
        .iter()
        .find(|c| dialable(c))
        .cloned()
        .expect("the share has one dialable candidate");
    // ahead of it, the same server at another address, advertised on a port that wraps
    plan.candidates.insert(
        0,
        Candidate {
            address: "192.0.2.55".into(),
            port: 4_294_999_696,
            ..good.clone()
        },
    );

    let d = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
    assert!(
        matches!(probe_server(&plan, &|o| d.dial(o)), Reach::At(..)),
        "the good one still answers"
    );
    assert!(
        !d.seen().iter().any(|s| s.starts_with("192.0.2.55")),
        "the wrapping candidate was never dialled: {:?}",
        d.seen()
    );
}

/// **Only an address that ANSWERED is ever stored** — the guard that replaced
/// `choose_local_connection`, which took the first `local` match and persisted it sight unseen,
/// so one v6 address wrote an undialable server to disk and broke every later boot.
///
/// The guard was once "this transport can only dial a dotted quad" and is now structural
/// instead, which is strictly stronger: every advertised address is dialable, nothing but a
/// candidate that answered as the right machine becomes a `SourceRef`, and the origin recorded
/// is the very value that was dialled.
///
/// The scenario is **a LAN with no route to the internet**, which is the case ranking TLS first
/// costs something: every `plex.direct` name is probed and none resolves, and the plaintext
/// twin — the address that works there — is what answers. That is the whole trade, priced.
#[test]
fn only_an_address_that_answered_is_ever_chosen_and_stored() {
    // our own server, v6 first — and the second v6 lies about its flag, which is why the shape
    // of the address is what decides rather than `IPv6`
    let res = resource(
        r#"{"name":"Mac mini","clientIdentifier":"aaaa1111","provides":"server","owned":true,
            "publicAddressMatches":false,"httpsRequired":false,"accessToken":"tok-own",
            "connections":[
              {"protocol":"https","address":"2001:db8::1","port":32400,
               "uri":"https://2001-db8--1.h.plex.direct:32400","local":true,"relay":false,"IPv6":true},
              {"protocol":"https","address":"fd00::5","port":32400,"uri":"","local":true,"relay":false,"IPv6":false},
              {"protocol":"https","address":"192.168.0.10","port":32400,
               "uri":"https://192-168-0-10.h.plex.direct:32400","local":true,"relay":false,"IPv6":false}]}"#,
    );
    // AllowPlaintext: this is the isolated-LAN case, and the whole point of the fixture is
    // that only the plaintext twin ever answers.
    let plan = probe::plan(&res, CredentialPolicy::AllowPlaintext);
    let d = Dialled::new(vec![
        // No plex.direct name resolves on an isolated LAN, so only the plaintext twins are
        // reachable — and both v6 ones answer too, so nothing but the ORDER decides.
        ("2001:db8::1", 200, identity_json("aaaa1111")),
        ("fd00::5", 200, identity_json("aaaa1111")),
        ("192.168.0.10", 200, identity_json("aaaa1111")),
    ]);

    match probe_server(&plan, &|o| d.dial(o)) {
        Reach::At(c, o) => {
            assert_eq!(
                c.address, "192.168.0.10",
                "IPv4 leads the plaintext fallbacks"
            );
            assert_eq!(
                o.base(),
                "http://192.168.0.10:32400",
                "…and the origin recorded is what was dialled"
            );
        }
        _ => panic!("the LAN IPv4 answers: {:?}", d.seen()),
    }
    assert_eq!(
        d.seen(),
        vec![
            "https://192-168-0-10.h.plex.direct:32400",
            "https://2001-db8--1.h.plex.direct:32400",
            "192.168.0.10:32400",
        ],
        "TLS is tried first and costs two probes here; the twin is the fallback that answers"
    );
    // …and the v6 addresses are never reached, because a candidate that answers ends the walk
    assert!(
        !d.seen()
            .iter()
            .any(|s| s.contains("fd00") || s.contains("2001:db8")),
        "{:?}",
        d.seen()
    );
}

/// The one field that decides whether we trust a connection is scanned for, not deserialized:
/// PMS answers XML unless an explicit JSON Accept survives to it, and a probe is the request
/// most likely to meet a proxy that rewrites headers.
#[test]
fn the_machine_identifier_is_read_from_json_and_from_xml_alike() {
    assert_eq!(
        machine_id_in(&identity_json("abc123")).as_deref(),
        Some("abc123")
    );
    assert_eq!(
        machine_id_in(
            br#"<MediaContainer size="0" machineIdentifier="abc123" version="1.43.3"/>"#
        )
        .as_deref(),
        Some("abc123")
    );
    assert_eq!(
        machine_id_in(br#"{"MediaContainer":{"machineIdentifier" : "abc123"}}"#).as_deref(),
        Some("abc123")
    );
    // an empty value is no value — it must not read as "the next field"
    assert_eq!(machine_id_in(br#"{"machineIdentifier":"","size":0}"#), None);
    assert_eq!(machine_id_in(b"nothing here"), None);
    assert_eq!(machine_id_in(b""), None);
}

/// **What a real sign-in must produce.** The whole of discovery over the measured two-server
/// account, with only the socket faked: this is the assertion that stands in for a device run,
/// because everything downstream — Home, the library grid, playback — talks to whatever this
/// function decided.
///
/// Two servers, OURS FIRST (plex.tv listed the share first), each settled on the one address
/// that answers from this TV: our LAN IPv4, and the share's PUBLIC IPv4 rather than the owner's
/// 172.20 LAN. Each carries its own grant, and the non-server resource is not in the roster.
#[test]
fn a_sign_in_to_a_two_server_account_settles_on_one_address_each_ours_first() {
    let d = Dialled::new(vec![
        ("192.168.0.10", 200, identity_json("aaaa1111")),
        ("203.0.113.9", 200, identity_json("bbbb2222")),
    ]);
    // AllowPlaintext: the fixture answers over the plaintext twin.
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::AllowPlaintext, &|o| d.dial(o))
    else {
        panic!("both servers answer: {:?}", d.seen())
    };

    assert_eq!(roster.len(), 2, "a player resource is not a server");
    let own = &roster[0];
    assert!(own.owned && own.machine_id == "aaaa1111");
    assert_eq!(
        (own.address.as_str(), own.port),
        ("192.168.0.10", 32400),
        "the LAN v4, not the v6"
    );
    assert_eq!(own.token, "tok-own");
    assert!(
        own.shared_by.is_empty(),
        "an owned server has no owner to name"
    );

    let share = &roster[1];
    assert!(!share.owned && share.machine_id == "bbbb2222");
    assert_eq!(
        (share.address.as_str(), share.port),
        ("203.0.113.9", 31234),
        "the owner's 172.20 LAN is not ours to dial, and their hostname does not resolve"
    );
    assert_eq!(
        share.token, "tok-share",
        "a share is a separate authority: OUR token gets a 401"
    );
    assert_eq!(share.shared_by, "friend");
    assert!(
        roster.iter().all(|s| s.dialable()),
        "every entry is dialable, so every one registers"
    );

    // OURS is probed first, though plex.tv listed the share first — that ordering is what
    // decides which library Home is built from. Within each server, TLS leads and the plaintext
    // twin is the fallback that answers on this (internet-less) LAN, and the walk STOPS at the
    // first acceptance: the relay is never reached, and neither is the share's plain hostname.
    assert_eq!(
        d.seen(),
        vec![
            "https://192-168-0-10.h.plex.direct:32400",
            "https://2001-db8--1.h.plex.direct:32400",
            "192.168.0.10:32400",
            "https://172-20-4-7.h.plex.direct:32400",
            "https://media.example.internal:31234",
            "https://203-0-113-9.h.plex.direct:31234",
            "203.0.113.9:31234",
        ]
    );
    assert!(
        !d.seen().iter().any(|s| s.contains("plex-relay")),
        "a 2 Mbit/s tunnel is a last resort"
    );
}

/// **The case this whole unit exists for: an account signed in from OUTSIDE the servers' LAN.**
/// It is the shape an LG QA reviewer has — no PMS on their network, an account we supply — and
/// before the TLS control plane it produced an empty roster and "Couldn't reach any Plex
/// server", because every candidate that can work from there is an https `plex.direct` name and
/// not one of them was dialable.
///
/// Here nothing on either LAN answers. The share is reached at its public `plex.direct` name,
/// and OUR server — which this fixture advertises no public direct address for, the ordinary
/// shape when nobody has forwarded a port — is reached at its **relay**, the last candidate
/// there is. What must come out is a roster whose origins are the NAMES a certificate is issued
/// for, while `address`, the diagnostic half, still reads as whatever plex.tv sent.
#[test]
fn an_account_reached_only_over_the_public_internet_settles_on_its_https_origins() {
    let d = Dialled::new(vec![
        ("plex-relay.example.net", 200, identity_json("aaaa1111")),
        ("203-0-113-9.h.plex.direct", 200, identity_json("bbbb2222")),
    ]);
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::HttpsOnly, &|o| d.dial(o))
    else {
        panic!("both servers answer over TLS: {:?}", d.seen())
    };

    assert_eq!(roster.len(), 2);
    assert_eq!(
        roster[0].origin_url, "https://plex-relay.example.net:8443",
        "ours, over the relay"
    );
    assert_eq!(
        roster[1].origin_url, "https://203-0-113-9.h.plex.direct:31234",
        "the share, direct"
    );
    for s in &roster {
        let o = s.origin().expect("a reached entry is dialable");
        assert!(
            o.is_tls(),
            "the connection that answered was TLS, so the stored origin must be"
        );
        assert_eq!(o.base(), s.origin_url, "the stored string round-trips");
    }
    // The share's stored origin is the NAME and its `address` is the quad behind it. That
    // inequality is the whole reason an origin is parsed from a URL rather than rebuilt from an
    // address: rebuild it and the certificate stops matching.
    assert_eq!(roster[1].address, "203.0.113.9");
    assert_ne!(
        roster[1].origin().expect("dialable").host(),
        roster[1].address
    );

    // The relay is genuinely LAST: every LAN candidate of our own server was tried first, and
    // the share's walk stopped the moment its public name answered.
    let seen = d.seen();
    assert_eq!(
        seen.last().map(String::as_str),
        Some("https://203-0-113-9.h.plex.direct:31234")
    );
    assert!(
        seen.iter().position(|x| x.contains("plex-relay")).unwrap() == 4,
        "four LAN candidates of ours precede the relay: {seen:?}"
    );
}

/// **Each roster entry's ORIGIN comes from the candidate's URL, not from its address.**
///
/// A plaintext twin has the same host as `address`; an accepted TLS candidate deliberately
/// does not. plex.tv advertises the `plex.direct` NAME in `uri` while `address` stays the quad
/// behind it, so a roster rebuilt from `address` would store an origin no certificate matches.
#[test]
fn each_reached_entry_records_the_origin_its_url_named() {
    let d = Dialled::new(vec![
        ("192.168.0.10", 200, identity_json("aaaa1111")),
        ("203.0.113.9", 200, identity_json("bbbb2222")),
    ]);
    // AllowPlaintext: this test is specifically about the plaintext twins' recorded origin.
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::AllowPlaintext, &|o| d.dial(o))
    else {
        panic!("both servers answer")
    };

    assert_eq!(roster[0].origin_url, "http://192.168.0.10:32400");
    assert_eq!(roster[1].origin_url, "http://203.0.113.9:31234");
    // …and it is a parseable origin, so the registry gets one rather than the legacy fallback
    for s in &roster {
        let o = s.origin().expect("a reached entry is dialable");
        assert_eq!(o.base(), s.origin_url, "the stored string round-trips");
        assert!(
            !o.is_tls(),
            "these are the plaintext twins, and they answered"
        );
        // On a plaintext twin the URL's host IS the address, which is what makes this leg the
        // control for the https one above: there the two differ, and only the URL is right.
        assert_eq!((o.host(), o.port() as i64), (s.address.as_str(), s.port));
    }
}

/// The three ways discovery can come to nothing are three different things to say, and the one
/// that used to be said for all of them ("No local Plex server found on this network") was the
/// old policy talking rather than a description of what happened.
#[test]
fn the_three_empty_outcomes_are_distinguished() {
    let players = serde_json::from_str::<Vec<Resource>>(
        r#"[{"name":"iPad","clientIdentifier":"cccc3333","provides":"player","connections":[]}]"#,
    )
    .unwrap();
    assert!(matches!(
        resolve_roster(&players, &[], CredentialPolicy::HttpsOnly, &|_| (0, Vec::new())),
        Resolved::NoServers { .. }
    ));

    // servers that simply do not answer
    let silent = Dialled::new(vec![]);
    assert!(matches!(
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::HttpsOnly, &|o| silent.dial(o)),
        Resolved::None { refused: false, .. }
    ));

    // …and one that answers 401: something in front of it refuses unauthenticated requests,
    // which is not a network fault and must not be worded as one
    let refused = Dialled::new(vec![
        ("192.168.0.10", 401, Vec::new()),
        ("203.0.113.9", 401, Vec::new()),
    ]);
    assert!(matches!(
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::HttpsOnly, &|o| refused.dial(o)),
        Resolved::None { refused: true, .. }
    ));

    // a share that answers while OUR server is off still signs in — a friend's library beats
    // "no server found" — and it becomes the primary because it is the only thing there is
    let one = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
    // AllowPlaintext: the share answers only over its plaintext twin here.
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::AllowPlaintext, &|o| one.dial(o))
    else {
        panic!("the share answered")
    };
    assert_eq!(roster.len(), 1);
    assert!(
        !roster[0].owned,
        "the primary is a share here, and that is the point"
    );
}

// ---- the discovery failure's incident (review round 2026-09-19) ----

/// **The discovery-only retry reports the SAME failure the sign-in did.** On this branch the
/// rediscovery worker is reached only through *Try again* after an authorized discovery failed
/// (`retry_kind`), so the failure it ends on is that one again. It used to be minted as a kind of
/// its own, which the per-launch dedup key reads as a new question — the person who answered
/// Not now was asked again by their own retry. Both workers take the whole incident from
/// [`discovery_failure`], the one table, and name no kind themselves.
#[test]
fn the_discovery_retry_reports_the_failure_it_retried() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    )
    .expect("auth.rs must be readable from its own test");
    for name in ["rediscovery_worker_with_output", "login_worker_with_output"] {
        let body = extract_fn_body(&src, name);
        assert!(body.contains("discovery_failure(&discovery)"), "`{name}` no longer uses the shared table");
        assert!(
            !body.contains("IncidentKind::Rediscovery") && !body.contains("IncidentKind::Discovery"),
            "`{name}` names its own discovery incident kind instead of taking the shared table's:\n{body}"
        );
    }
    let (_, silent) = discovery_failure(&Discovery::ServersUnreachable {
        trigger: DiscoveryTrigger::Login,
    }).expect("a failure");
    assert_eq!(silent.kind, IncidentKind::Discovery(DiscoveryClass::Silent));
}

/// **A token plex.tv refused is an authorization failure, not a network one.** `/resources`
/// answering 401 or 403 means plex.tv heard us and said no — "check the connection" sends the
/// person to a router that is fine, and the report must not be grouped with real silence.
#[test]
fn a_refused_token_is_reported_as_authorization_not_as_silence() {
    for status in [401u16, 403] {
        let refused = [
            Ok(status),
            // A refusal whose body broke is still that refusal (`net::response_status`).
            Err(crate::net::RequestFailure {
                cause: crate::net::RequestError::Transport,
                status: Some(status),
                body_limit: None,
                curl_rc: Some(18),
            }),
        ];
        for last in refused {
            let (message, incident) =
                discovery_failure(&Discovery::PlexTvFailed(PlexTvFailure {
                    last, attempts: 1, elapsed: Duration::ZERO, trigger: DiscoveryTrigger::Login,
                })).expect("a failure");
            assert_eq!(incident.kind, IncidentKind::Authorization, "{status}");
            assert_eq!(incident.http_status, Some(status));
            assert!(!message.contains("connection"), "{status}: {message:?} blames the network");
        }
    }
    // Real silence, and other answers, stay what they were.
    for last in [Ok(503), Ok(429)] {
        let (message, incident) = discovery_failure(&Discovery::PlexTvFailed(PlexTvFailure {
            last, attempts: 3, elapsed: Duration::from_secs(6), trigger: DiscoveryTrigger::Login,
        })).expect("a failure");
        assert_eq!(incident.kind, IncidentKind::Discovery(DiscoveryClass::Silent), "{last:?}");
        assert!(message.contains("plex.tv"));
    }
}

#[test]
fn terminal_discovery_copy_names_the_target_cause_retry_and_action() {
    let failure = |rc, cause| Err(crate::net::RequestFailure { cause, status: None,
        body_limit: None, curl_rc: rc });
    let message = |last| discovery_failure(&Discovery::PlexTvFailed(PlexTvFailure {
        last, attempts: 3, elapsed: Duration::from_secs(6), trigger: DiscoveryTrigger::Login,
    })).unwrap().0.into_owned();
    assert_eq!(message(failure(Some(6), crate::net::RequestError::Transport)),
        "This TV couldn't find plex.tv, so your servers weren't checked. We tried 3 times. Check the TV's internet connection, then try again.");
    assert_eq!(message(failure(Some(28), crate::net::RequestError::TimedOut)),
        "This TV couldn't reach plex.tv, so your servers weren't checked. We tried 3 times. Check the TV's internet connection, then try again.");
    assert_eq!(message(failure(Some(60), crate::net::RequestError::Transport)),
        "This TV couldn't make a secure connection to plex.tv. Check the TV's date and time, then try again.");
    for evidence in [Ok(200), Ok(408), Ok(429), Ok(503)] {
        assert_eq!(message(evidence),
            "plex.tv is having trouble right now, so your servers weren't checked. Try again in a few minutes.");
    }
    let servers = discovery_failure(&Discovery::ServersUnreachable {
        trigger: DiscoveryTrigger::Rediscover,
    }).unwrap().0;
    assert_eq!(servers,
        "plex.tv listed your servers, but none of them answered. Make sure your Plex Media Server is on and online, then try again.");
}

#[test]
fn a_single_discovery_attempt_uses_grammatical_retry_copy() {
    let last = Err(crate::net::RequestFailure { cause: crate::net::RequestError::Transport,
        status: None, body_limit: None, curl_rc: Some(6) });
    let (message, _) = discovery_failure(&Discovery::PlexTvFailed(PlexTvFailure {
        last, attempts: 1, elapsed: Duration::ZERO, trigger: DiscoveryTrigger::Login,
    })).unwrap();
    assert!(message.contains("We tried once."), "{message}");
    assert!(!message.contains("1 times"), "{message}");
}

#[test]
fn background_resource_call_sites_clamp_each_request_to_the_runner_budget() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    ).unwrap();
    let roster = extract_fn_body(&src, "server_roster_worker_with_output");
    assert!(roster.contains("resources_with(account_timeouts(remaining))"), "{roster}");
    let endpoint = extract_fn_body(&src, "probe_endpoint_work");
    assert!(endpoint.contains("resources(&ac, remaining)"),
        "the endpoint runner must pass its remaining budget through the injected seam:\n{endpoint}");
    let production = extract_fn_body(&src, "run_session_work");
    assert!(production.contains("resources_with(account_timeouts(remaining))"),
        "the production endpoint seam must clamp the account request:\n{production}");
}

#[test]
fn dns_copy_never_claims_that_a_plex_server_was_contacted() {
    let last = Err(crate::net::RequestFailure { cause: crate::net::RequestError::Transport,
        status: None, body_limit: None, curl_rc: Some(6) });
    let (caption, _) = discovery_failure(&Discovery::PlexTvFailed(PlexTvFailure {
        last, attempts: 3, elapsed: Duration::from_secs(6), trigger: DiscoveryTrigger::Login,
    })).unwrap();
    assert!(caption.contains("plex.tv"));
    assert!(caption.contains("TV's internet connection"));
    assert!(!caption.contains("Plex server"), "{caption}");
}

/// **Discovery writes the grant evidence down beside the credit.** The sign-in ingest is one of
/// three that produce a `SourceRef` (`resolve_roster_using`, `source_from_reach`,
/// `refreshed_sources`), and a session whose roster carried only `owned` could not answer "is
/// this our household's server" on any later boot — the credit is an empty string for the
/// household's own server and for a share plex.tv never named alike.
///
/// The verdict here is the ordinary single-account one (we own ours, the share is outside), which
/// is the point: the evidence is plex.tv's, carried verbatim, and it is a later roster that
/// re-grades it rather than this ingest.
#[test]
fn a_sign_in_roster_carries_plex_tvs_household_evidence_verbatim() {
    let d = Dialled::new(vec![
        ("192.168.0.10", 200, identity_json("aaaa1111")),
        ("203.0.113.9", 200, identity_json("bbbb2222")),
    ]);
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], CredentialPolicy::AllowPlaintext, &|o| d.dial(o))
    else {
        panic!("both servers answer");
    };

    assert_eq!(
        roster.iter().map(|s| (s.machine_id.as_str(), s.owned, s.home, s.owner_id)).collect::<Vec<_>>(),
        [("aaaa1111", true, false, 0), ("bbbb2222", false, false, 987_654)],
        "`ownerId:null` is 0 and never matches a household member; the share names its owner",
    );
}

// ---- PLX-NATIVE-10: an insecure-only verdict carries the evidence that explains it ----

/// Race `resource` through the production coordinator with `dial`, then fold the one-server
/// roster exactly as sign-in does — the path whose `Discovery` the incident is built from.
fn insecure_verdict(resource: Resource, dial: ProbeDial) -> Discovery {
    let resources = vec![resource];
    let mut probe_one = |plan: &ProbePlan| {
        probe_server_racing(plan, Arc::clone(&dial), &threaded_spawn, test_policy(),
            &mut |_, _, _| {})
    };
    let resolved = resolve_roster_using(&resources, &[], CredentialPolicy::HttpsOnly,
        &mut probe_one, &mut || {}, &mut |_, _, _, _| {});
    resolved_without_roster(resolved, DiscoveryTrigger::Login)
        .err()
        .expect("nothing credential-eligible verified, so discovery must fail")
}

fn insecure_evidence_of(d: &Discovery) -> probe::InsecureEvidence {
    let Discovery::InsecureOnly(Some((evidence, _))) = d else {
        panic!("expected an insecure-only verdict with probe evidence");
    };
    let (_, incident) = discovery_failure(d).expect("an insecure-only verdict is a failure");
    assert_eq!(incident.insecure, Some(*evidence), "the incident carries the verdict's evidence");
    *evidence
}

fn timed_out() -> crate::net::RequestFailure {
    crate::net::RequestFailure {
        cause: crate::net::RequestError::TimedOut, status: None, body_limit: None, curl_rc: Some(28),
    }
}

fn transport_rc(rc: i32) -> crate::net::RequestFailure {
    crate::net::RequestFailure {
        cause: crate::net::RequestError::Transport, status: None, body_limit: None, curl_rc: Some(rc),
    }
}

/// Our own server with every HTTPS route plex.tv can advertise — a LAN `plex.direct` name, a
/// public `plex.direct` name, a custom access URL and a relay — plus the LAN plaintext twin the
/// fixtures let answer. `relay` false drops the relay connection entirely.
fn every_route_server(relay: bool) -> Resource {
    let relay = if relay {
        r#",{"protocol":"https","address":"198.51.100.4","port":8443,
             "uri":"https://198-51-100-4.relayhash.plex.direct:8443","local":false,"relay":true,"IPv6":false}"#
    } else {
        ""
    };
    resource(&format!(
        r#"{{"name":"ours","clientIdentifier":"routes01","provides":"server","owned":true,
            "sourceTitle":null,"publicAddressMatches":true,"httpsRequired":false,
            "accessToken":"tok-routes","connections":[
              {{"protocol":"https","address":"192.168.0.10","port":32400,
               "uri":"https://192-168-0-10.hash.plex.direct:32400","local":true,"relay":false,"IPv6":false}},
              {{"protocol":"https","address":"203.0.113.9","port":32400,
               "uri":"https://203-0-113-9.hash.plex.direct:32400","local":false,"relay":false,"IPv6":false}},
              {{"protocol":"https","address":"media.example.test","port":443,
               "uri":"https://media.example.test:443","local":false,"relay":false,"IPv6":false}}{relay}
            ]}}"#
    ))
}

/// **(a) A real failed TLS handshake is named as TLS, not folded into silence.** The same real
/// curl fixture as the `Reach::At`-refusal test above: the LAN `plex.direct` name points at a
/// plaintext-only double, so curl's ClientHello fails (a TLS `CURLcode`), while the plaintext
/// twin on that port verifies the machine. The evidence must say which route failed and how,
/// and carry the plaintext answer's closed facts — a loopback literal on a `local` connection.
#[test]
fn e2e_insecure_only_evidence_names_the_failed_tls_handshake() {
    let _serial = crate::testlock::serial();
    if !(crate::net::global_init() && crate::net::available()) {
        eprintln!("curl unavailable on this host; skipping");
        return;
    }
    let plain_port = crate::net::spawn_plain_only(identity_json("e2e95mid"));
    let dead = crate::net::dead_port();
    let d = insecure_verdict(e2e95_resource(plain_port, dead, None), Arc::new(get_identity));
    let e = insecure_evidence_of(&d);
    assert_eq!(e.https.lan_plex_direct, probe::RouteOutcome::Tls, "{e:?}");
    assert_eq!(e.https.public_plex_direct, probe::RouteOutcome::Absent);
    assert_eq!(e.https.custom_https, probe::RouteOutcome::Absent);
    assert_eq!(e.https.relay, probe::RouteOutcome::Absent);
    assert!(e.plaintext_local && e.owned && e.public_address_matches && !e.https_required, "{e:?}");
    assert_eq!(e.plaintext_scope, probe::AddressScope::Loopback);
    assert_eq!(e.plaintext_family, probe::AddressFamily::V4);
}

/// **(b) Every HTTPS route timing out is four timeouts, not one `unknown`.** The LAN plaintext
/// twin verifies; each HTTPS route — LAN name, public name, custom URL and the relay raced after
/// them — times out. The plaintext facts are the ones a same-network decision would need.
#[test]
fn insecure_only_evidence_records_a_timeout_on_every_https_route() {
    let dial: ProbeDial = Arc::new(|origin, _, _| {
        if origin.is_tls() {
            ProbeReply::Failed(Some(timed_out()))
        } else if origin.host() == "192.168.0.10" {
            ProbeReply::from((200, identity_json("routes01")))
        } else {
            ProbeReply::Failed(None)
        }
    });
    let e = insecure_evidence_of(&insecure_verdict(every_route_server(true), dial));
    assert_eq!(
        e.https,
        probe::HttpsRoutes {
            lan_plex_direct: probe::RouteOutcome::Timeout,
            public_plex_direct: probe::RouteOutcome::Timeout,
            custom_https: probe::RouteOutcome::Timeout,
            relay: probe::RouteOutcome::Timeout,
        }
    );
    assert!(e.plaintext_local && e.owned && e.public_address_matches && !e.https_required, "{e:?}");
    assert_eq!(e.plaintext_scope, probe::AddressScope::Private);
    assert_eq!(e.plaintext_family, probe::AddressFamily::V4);
}

/// **(c) A relay plex.tv never advertised is `absent`; one that failed says how.** Same server,
/// same failures on the direct routes (a TLS refusal on the LAN name, a refused connect on the
/// public one, DNS on the custom name); only the relay row differs.
#[test]
fn insecure_only_evidence_tells_an_absent_relay_from_a_failed_one() {
    let dial: ProbeDial = Arc::new(|origin, _, _| {
        let host = origin.host();
        if !origin.is_tls() {
            return if host == "192.168.0.10" {
                ProbeReply::from((200, identity_json("routes01")))
            } else {
                ProbeReply::Failed(None)
            };
        }
        ProbeReply::Failed(Some(if host.starts_with("192-") {
            transport_rc(35)
        } else if host.starts_with("203-") {
            transport_rc(7)
        } else {
            transport_rc(6)
        }))
    });
    let failed = insecure_evidence_of(&insecure_verdict(every_route_server(true), Arc::clone(&dial)));
    let absent = insecure_evidence_of(&insecure_verdict(every_route_server(false), dial));
    for e in [failed, absent] {
        assert_eq!(e.https.lan_plex_direct, probe::RouteOutcome::Tls, "{e:?}");
        assert_eq!(e.https.public_plex_direct, probe::RouteOutcome::Refused, "{e:?}");
        assert_eq!(e.https.custom_https, probe::RouteOutcome::Dns, "{e:?}");
    }
    assert_eq!(failed.https.relay, probe::RouteOutcome::Dns, "the relay was raced and failed");
    assert_eq!(absent.https.relay, probe::RouteOutcome::Absent, "plex.tv advertised no relay");
}

/// **(d) An account whose resources are all players says how many it had, and which flow asked.**
/// `NoServers` is a statement about the account, so its evidence is a bucketed count of what
/// `/resources` did return — never a name — and whether a fresh sign-in or a retry produced it.
#[test]
fn no_servers_evidence_counts_the_players_and_names_the_trigger() {
    let player = |n: usize| {
        format!(r#"{{"name":"p{n}","clientIdentifier":"cccc{n}","provides":"player","connections":[]}}"#)
    };
    let one = serde_json::from_str::<Vec<Resource>>(&format!("[{}]", player(1))).unwrap();
    let seven = serde_json::from_str::<Vec<Resource>>(&format!(
        "[{}]",
        (0..7).map(player).collect::<Vec<_>>().join(",")
    ))
    .unwrap();
    for (resources, trigger, bucket) in [
        (one, DiscoveryTrigger::Rediscover, crate::telemetry::incident::CountBucket::One),
        (seven, DiscoveryTrigger::Login, crate::telemetry::incident::CountBucket::SixPlus),
    ] {
        let resolved = resolve_roster(&resources, &[], CredentialPolicy::HttpsOnly, &|_| (0, Vec::new()));
        let d = resolved_without_roster(resolved, trigger).err().expect("no server is a failure");
        let Discovery::NoServers(evidence) = d else { panic!("expected NoServers") };
        assert_eq!(
            evidence,
            crate::telemetry::incident::NoServersEvidence { resources: bucket, trigger }
        );
        let (_, incident) = discovery_failure(&d).expect("no servers is a failure");
        assert_eq!(incident.no_servers, Some(evidence));
        assert_eq!(incident.insecure, None);
    }
}

// ---- the no-server read-out names the account (production wiring) ----

/// Sink that keeps the terminal observation, as the owner's FIFO would receive it.
struct TerminalCapture(std::sync::Mutex<Vec<AuthProgress>>);
impl owner::ObservationSink for TerminalCapture {
    fn live(&self) -> bool { true }
    fn progress(&self, _: AuthProgress) -> bool { true }
    fn terminal(&self, progress: AuthProgress) -> bool {
        self.0.lock().unwrap().push(progress);
        true
    }
}

/// `/resources` answering with accounts' devices, none of them a server.
fn players_only() -> Vec<Resource> {
    serde_json::from_str(
        r#"[{"name":"phone","clientIdentifier":"cccc1","provides":"player","connections":[]}]"#,
    ).unwrap()
}

/// What the screen is handed when the sign-in's last step ran with `user_call` as plex.tv's
/// `/api/v2/user`: the failure's `message` and `account`, the call count, and the incident.
fn no_servers_failure(user_call: impl FnOnce() -> Option<String>)
    -> (String, Option<String>, usize, IncidentContext) {
    let output = TerminalCapture(std::sync::Mutex::new(Vec::new()));
    let ac = AccountClient::new("client", Some("authorized-token"));
    let discovery = discover_and_store_with_resources(&ac, "client", 5, DiscoveryTrigger::Login,
        &PlaintextAsk::undecided(), &output, |_, _| Ok(players_only()));
    let (message, incident) = discovery_failure(&discovery).expect("no servers is a failure");
    let mut calls = 0;
    let account = no_servers_account_with(&discovery, &output, || { calls += 1; user_call() });
    output_failed_naming(&output, 5, &message, incident, plaintext_offer(&discovery), account);
    let terminal = output.0.lock().unwrap().pop().expect("the failure was published");
    let AuthProgress::Login(LoginProgress::Failed { message, account, incident, .. }) = terminal
    else { panic!("not a sign-in failure") };
    (message, account, calls, incident)
}

/// **Regression: a sign-in that ends in "no server yet" names the account.** `/resources` answered
/// with a list holding no server and `/api/v2/user` answered with a name: the failure that reaches
/// the owner carries that name beside the plain caption. Every way the user call can come up empty
/// — it failed or timed out (`None`), or named nobody — leaves exactly the nameless failure.
#[test]
fn a_no_servers_sign_in_hands_the_screen_the_account_name() {
    let (message, account, calls, _) = no_servers_failure(|| Some("alexandra".to_owned()));
    assert_eq!(message, crate::i18n::msg::browse_auth_no_servers(), "the caption stays the fallback");
    assert_eq!(account.as_deref(), Some("alexandra"), "the account is named on the failure");
    assert_eq!(calls, 1, "ONE user call");

    let (message, account, calls, _) = no_servers_failure(|| None);
    assert_eq!((message.as_str(), account, calls),
        (crate::i18n::msg::browse_auth_no_servers(), None, 1),
        "a failed, timed-out or nameless user call is today's failure, unchanged");
}

/// The user call is asked ONLY when discovery ended in no servers.
#[test]
fn the_account_name_is_fetched_only_for_a_no_servers_verdict() {
    let output = TerminalCapture(std::sync::Mutex::new(Vec::new()));
    let evidence = crate::telemetry::incident::NoServersEvidence {
        resources: crate::telemetry::incident::CountBucket::One, trigger: DiscoveryTrigger::Login };
    for (verdict, asks) in [
        (Discovery::NoServers(evidence), true),
        (Discovery::Refused, false),
        (Discovery::Cancelled, false),
        (Discovery::ServersUnreachable { trigger: DiscoveryTrigger::Login }, false),
        (Discovery::InsecureOnly(None), false),
    ] {
        let mut asked = false;
        let account = no_servers_account_with(&verdict, &output, || { asked = true; Some("n".into()) });
        assert_eq!(asked, asks, "only a no-servers verdict asks");
        assert_eq!(account.is_some(), asks);
    }
    // Structure: both workers ask only inside the failure branch, after discovery, and a
    // successful sign-in's tail never does.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/auth.rs"),
    ).expect("auth.rs must be readable from its own test");
    for name in ["rediscovery_worker_with_output", "login_worker_with_output"] {
        let body = extract_fn_body(&src, name);
        let discovered = body.find("discover_and_store(").expect("discovers");
        let failure = body.find("discovery_failure(&discovery)").expect("grades the failure");
        let asked = body.find("no_servers_account(&discovery").expect("asks for the account");
        assert!(discovered < failure && failure < asked, "`{name}` asks outside the failure branch");
        assert_eq!(body.matches("no_servers_account(").count(), 1);
    }
    assert!(!extract_fn_body(&src, "finish_sign_in").contains("display_name"));
}

// ---- PLX-NATIVE-10: consent-gated plaintext on the home network ----

/// What answers the no-relay reporter's topology when the plaintext candidate is named by any
/// host (the hostname variant below re-addresses it): every plaintext candidate but the remote
/// custom one answers `/identity` for the machine, every HTTPS route is dead. `relay` switches
/// the relay back on and lets it verify.
fn plx10_dial(origin: &Origin, _pin: Option<&crate::plex::ResolvePin>, _budget: Duration) -> (i32, Vec<u8>) {
    match (origin.host(), origin.is_tls()) {
        ("custom.example.net", false) => (400, Vec::new()),
        ("relay.example.net", true) => (200, identity_json("issue95mid")),
        (host, false) if !host.starts_with("172.") => (200, identity_json("issue95mid")),
        _ => (0, Vec::new()),
    }
}

/// One store-policy discovery of `resource` under `ask`, recording every authenticated admission
/// as `(origin_url, token)` — the only step of discovery that puts a credential on a request (the
/// identity race is tokenless by construction: `ProbeDial` is handed no token at all).
fn plx10_discover(resource: Resource, ask: &PlaintextAsk) -> (Resolution, Vec<(String, String)>) {
    let (resolved, admitted, _) =
        plx10_discover_with(resource, ask, plx10_dial, |_| crate::plex::EndpointAdmission::Usable);
    (resolved, admitted)
}

/// [`plx10_discover`] with the dial and the admission answer chosen by the test. The probe honours
/// the origins admission already rejected exactly as the live one does ([`ProbePlan::without`]),
/// so a re-probe after a refusal sees the plan production would. The third value is each server's
/// settled verdict, as the owner would publish it (`RegistryPlan::Probe`).
fn plx10_discover_with(
    resource: Resource,
    ask: &PlaintextAsk,
    dial: fn(&Origin, Option<&crate::plex::ResolvePin>, Duration) -> (i32, Vec<u8>),
    answer: impl Fn(&SourceRef) -> crate::plex::EndpointAdmission,
) -> (Resolution, Vec<(String, String)>, Vec<SettledProbe>) {
    let dial: ProbeDial = status_dial(dial);
    let mut probe_one = |plan: &ProbePlan, rejected: &[String]| {
        probe_server_racing(&plan.without(rejected), Arc::clone(&dial), &threaded_spawn,
            test_policy(), &mut |_, _, _| {})
    };
    let mut admitted = Vec::new();
    let mut settled = Vec::new();
    let resolved = resolve_roster_using_admission(
        &[resource], &[], CredentialPolicy::HttpsOnly, ask, &mut probe_one,
        &mut |source| {
            admitted.push((source.origin_url.clone(), source.token.clone()));
            answer(source)
        },
        &mut |_, _, _| {}, &mut || {},
        &mut |plan, outcome, tier, address| settled.push(settled_probe(plan, outcome, tier, address)),
    );
    (resolved, admitted, settled)
}

fn plx10_offer(resolved: &Resolution) -> Option<&PlaintextVerdict> {
    match &resolved.outcome {
        Resolved::None { evidence: Some((_, verdict)), .. } => Some(verdict),
        _ => None,
    }
}

/// **Before consent, no credential travels over plaintext** — HTTPS fails everywhere and there is
/// no relay; the server answers only at its LAN plaintext address. Discovery settles insecure-only
/// with an eligible, undecided verdict (the question the sign-in read-out asks), no admission ever
/// named an `http://` origin, and no grant exists.
#[test]
fn plx10_before_consent_an_eligible_lan_answer_is_offered_and_no_token_goes_over_plaintext() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    crate::plex::grant::reset_for_test();
    let (resolved, admitted) = plx10_discover(issue_95_account(false), &PlaintextAsk::undecided());
    assert!(admitted.iter().all(|(url, _)| url.starts_with("https://")), "{admitted:?}");
    let verdict = plx10_offer(&resolved).expect("an insecure-only verdict");
    assert_eq!(verdict.eligibility, probe::PlaintextEligibility::Eligible);
    assert_eq!(verdict.choice, PlaintextChoice::Undecided);
    assert!(verdict.offers());
    assert!(crate::plex::grant::granted_machines().is_empty());
    assert!(!crate::plex::grant::allowed_under(
        CredentialPolicy::HttpsOnly, &Origin::http("192.168.1.50", 32400)));

    // Declined is recorded, never offered as a question again, and still mints nothing.
    let declined = PlaintextAsk::undecided().with("issue95mid", PlaintextChoice::Declined);
    let (resolved, admitted) = plx10_discover(issue_95_account(false), &declined);
    assert!(admitted.is_empty(), "{admitted:?}");
    let verdict = plx10_offer(&resolved).expect("an insecure-only verdict");
    assert_eq!(verdict.choice, PlaintextChoice::Declined);
    assert!(crate::plex::grant::granted_machines().is_empty());
    crate::plex::reset_servers_for_test();
}

/// **After consent, the plaintext origin is activated and the token goes to it alone**: exactly one
/// admission, at `http://192.168.1.50:32400`, and the authority admits that exact origin — not the
/// same host on another port, not another host.
#[test]
fn plx10_after_consent_the_token_goes_only_to_the_exact_verified_origin() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    crate::plex::grant::reset_for_test();
    let allowed = PlaintextAsk::undecided().with("issue95mid", PlaintextChoice::Allowed);
    let (resolved, admitted) = plx10_discover(issue_95_account(false), &allowed);
    assert_eq!(admitted, vec![("http://192.168.1.50:32400".to_owned(), "tok-95".to_owned())]);
    let Resolved::Reached(found) = resolved.outcome else { panic!("consent must reach the server") };
    assert_eq!(found[0].origin_url, "http://192.168.1.50:32400");
    let store = CredentialPolicy::HttpsOnly;
    assert!(crate::plex::grant::allowed_under(store, &Origin::http("192.168.1.50", 32400)));
    assert!(!crate::plex::grant::allowed_under(store, &Origin::http("192.168.1.50", 32401)));
    assert!(!crate::plex::grant::allowed_under(store, &Origin::http("192.168.1.51", 32400)));
    assert!(!crate::plex::grant::allowed_under(store, &Origin::http("custom.example.net", 443)));
    crate::plex::grant::reset_for_test();
    crate::plex::reset_servers_for_test();
}

/// **Never offered, never granted — even with consent recorded**: a relay that verifies, a server
/// that requires secure connections, a network plex.tv does not call the server's, a plaintext
/// address that is a name, and a server plex.tv calls remote. Each keeps today's refusal (or the
/// relay), and no admission ever names an `http://` origin.
#[test]
fn plx10_consent_never_downgrades_an_ineligible_topology() {
    let _g = crate::testlock::serial();
    let allowed = PlaintextAsk::undecided().with("issue95mid", PlaintextChoice::Allowed);
    let variants: Vec<(&str, Resource)> = vec![
        ("relay verifies", issue_95_account(true)),
        ("httpsRequired", {
            let mut r = issue_95_account(false); r.https_required = true; r
        }),
        ("publicAddressMatches=false", {
            let mut r = issue_95_account(false); r.public_address_matches = false; r
        }),
        ("hostname address", {
            let mut r = issue_95_account(false);
            r.connections[0].address = "nas.home.arpa".into();
            r
        }),
        ("remote", {
            let mut r = issue_95_account(false);
            for c in &mut r.connections { c.local = false; }
            r
        }),
    ];
    for (label, resource) in variants {
        crate::plex::reset_servers_for_test();
        crate::plex::grant::reset_for_test();
        let (resolved, admitted) = plx10_discover(resource, &allowed);
        assert!(admitted.iter().all(|(url, _)| url.starts_with("https://")), "{label}: {admitted:?}");
        assert!(crate::plex::grant::granted_machines().is_empty(), "{label}: a grant was minted");
        assert!(plx10_offer(&resolved).is_none_or(|v| !v.offers()), "{label}: offered");
    }
    crate::plex::reset_servers_for_test();
}

/// **An HTTPS origin refused at ADMISSION is an HTTPS answer, not an absent route.** The server's
/// LAN `plex.direct` origin verifies `/identity` and then refuses the grant (401); admission drops
/// it and the probe runs again without it. That re-probe must not read the refused route as never
/// having existed and call the plaintext twin eligible — HTTPS reached the server; the token
/// problem is not the network's, and consent recorded earlier must not turn it into a plaintext
/// sign-in.
#[test]
fn plx10_an_https_origin_refused_at_admission_is_not_a_reason_to_go_plaintext() {
    let _g = crate::testlock::serial();
    crate::plex::reset_servers_for_test();
    crate::plex::grant::reset_for_test();
    fn dial(origin: &Origin, _pin: Option<&crate::plex::ResolvePin>, _budget: Duration) -> (i32, Vec<u8>) {
        match (origin.host(), origin.is_tls()) {
            ("192-168-1-50.h.plex.direct", true) => (200, identity_json("issue95mid")),
            ("192.168.1.50", false) => (200, identity_json("issue95mid")),
            _ => (0, Vec::new()),
        }
    }
    let allowed = PlaintextAsk::undecided().with("issue95mid", PlaintextChoice::Allowed);
    let (resolved, admitted, _) = plx10_discover_with(issue_95_account(false), &allowed, dial,
        |source| if source.origin_url.starts_with("https://") {
            crate::plex::EndpointAdmission::Refused(401)
        } else {
            crate::plex::EndpointAdmission::Usable
        });
    assert!(admitted.iter().all(|(url, _)| url.starts_with("https://")), "{admitted:?}");
    assert!(crate::plex::grant::granted_machines().is_empty(), "a grant was minted");
    assert!(plx10_offer(&resolved).is_none_or(|v| !v.offers()), "offered");
    crate::plex::reset_servers_for_test();
}

/// **A grant does not outlive the verdict that justified it.** A server connected under consent,
/// then re-discovered on a network where it is no longer eligible (plex.tv now says the client is
/// outside the server's NAT) or no longer answers at all: the published verdict for that machine
/// is not a reach at the granted origin, so the grant is withdrawn — the next request would
/// otherwise carry the token to an origin nothing re-proved.
#[test]
fn plx10_a_fresh_verdict_that_does_not_reach_the_granted_origin_revokes_the_grant() {
    let _g = crate::testlock::serial();
    let allowed = PlaintextAsk::undecided().with("issue95mid", PlaintextChoice::Allowed);
    let silent = |_: &Origin, _: Option<&crate::plex::ResolvePin>, _: Duration| (0, Vec::new());
    for (label, resource, dial) in [
        ("no longer eligible", {
            let mut r = issue_95_account(false); r.public_address_matches = false; r
        }, plx10_dial as fn(&Origin, Option<&crate::plex::ResolvePin>, Duration) -> (i32, Vec<u8>)),
        ("no longer answers", issue_95_account(false), silent),
    ] {
        crate::plex::reset_servers_for_test();
        crate::plex::grant::reset_for_test();
        let _ = plx10_discover(issue_95_account(false), &allowed);
        assert_eq!(crate::plex::grant::granted_machines(), vec!["issue95mid".to_owned()], "{label}");
        let (_, admitted, settled) = plx10_discover_with(resource, &allowed, dial,
            |_| crate::plex::EndpointAdmission::Usable);
        assert!(admitted.is_empty(), "{label}: {admitted:?}");
        publish_settled_probes(&settled);
        assert!(crate::plex::grant::granted_machines().is_empty(), "{label}: the grant survived");
    }
    crate::plex::grant::reset_for_test();
    crate::plex::reset_servers_for_test();
}

/// **Every insecure-only reason fits the read-out's two-line slot, and names its action.** The
/// failed read-out's reason is drawn at `StatusOverlay::REASON_W` and never grows past two lines
/// (`StatusOverlay::reason_view`); a longer one is cut, and the part cut is the tail — which is
/// where every one of these says what to do. The host has no LG font, so the check is twofold: the
/// fixture measurer's wrap at the real width and size, and a character budget
/// ([`READOUT_REASON_BUDGET`]) that holds with room for the real face's wider glyphs. The shared
/// forms are measured with a long owner name. The owner-approved
/// `discovery_insecure_only_message()` predates the budget and is kept byte-identical; it is held to
/// the measured wrap only.
#[test]
fn every_insecure_only_reason_fits_two_lines_and_names_its_action() {
    use crate::ui::text_view::TextView;
    use crate::ui::widgets::StatusOverlay;
    const READOUT_REASON_BUDGET: usize = 125;
    let fits = |text: &str| !TextView::new(text, crate::ui::theme::size::BODY, crate::ui::theme::TEXT_SECONDARY)
        .max_lines(2)
        .with_measure(&crate::ui::fixture::FixtureMeasure)
        .truncates(StatusOverlay::REASON_W);
    assert!(fits(discovery_insecure_only_message()));
    let mut seen = 0;
    for owner in ["", "a-longish-owner18"] {
        for eligibility in [probe::PlaintextEligibility::Eligible, probe::PlaintextEligibility::NotLocal,
            probe::PlaintextEligibility::NotPrivateAddress, probe::PlaintextEligibility::HttpsAnswered] {
            for choice in [PlaintextChoice::Undecided, PlaintextChoice::Allowed, PlaintextChoice::Declined,
                PlaintextChoice::Revoked] {
                for surface in [ReadoutSurface::SignIn, ReadoutSurface::SignedIn] {
                    let v = PlaintextVerdict {
                        machine_id: "m".into(), name: "nas".into(), shared_by: owner.into(),
                        eligibility, choice,
                    };
                    let copy = plaintext_copy(Some(&v), surface);
                    if copy == discovery_insecure_only_message() { continue; }
                    seen += 1;
                    assert!(copy.chars().count() <= READOUT_REASON_BUDGET, "{} chars: {copy}", copy.chars().count());
                    assert!(fits(&copy), "wraps past two lines: {copy}");
                    if v.offers() {
                        assert!(copy.contains("without encryption") && copy.contains("this network")
                            || copy.contains("Settings \u{2192} Unencrypted connections")
                            || copy.contains("Try again"), "{copy}");
                        let action = match (choice, surface) {
                            (PlaintextChoice::Undecided, _) => "Select Connect",
                            (PlaintextChoice::Allowed, _) | (_, ReadoutSurface::SignIn) => "Select Try again",
                            (_, ReadoutSurface::SignedIn) => "Settings \u{2192} Unencrypted connections",
                        };
                        assert!(copy.contains(action), "{choice:?}/{surface:?}: {copy}");
                    }
                }
            }
        }
    }
    assert!(seen > 20);
}

#[test]
fn localized_plaintext_copy_preserves_owner_names_and_the_complete_named_action() {
    use crate::i18n::{LocaleContext, Preference};
    use crate::ui::text_view::TextView;
    use crate::ui::widgets::StatusOverlay;
    use std::ffi::CStr;
    /// `FixtureMeasure`'s half-em advance per UNICODE SCALAR rather than per UTF-8 byte. Its byte
    /// count doubles every Cyrillic letter, so it would grade Belarusian against a font no device
    /// has; this grades every locale on exactly the advance the English copy is held to.
    struct ScalarMeasure;
    impl crate::ui::machine::Measure for ScalarMeasure {
        fn width(&self, text: &CStr, size: i32, _bold: bool) -> f32 {
            text.to_string_lossy().chars().count() as f32 * size as f32 * 0.5
        }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.7 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
    }
    for (preference, connect, retry, settings_path) in [
        (Preference::En, "Connect", "Try again", "Settings → Unencrypted connections"),
        (Preference::Es, "Conectar", "Reintentar", "Ajustes → Conexiones sin cifrar"),
        (Preference::Be, "Злучыцца", "Паспрабаваць зноў", "Налады → Злучэнні без шыфравання"),
    ] {
        let locale = LocaleContext::resolve(preference, None, None, None, None);
        for owner in ["", "a-longish-owner18"] {
            for choice in [PlaintextChoice::Undecided, PlaintextChoice::Allowed,
                PlaintextChoice::Declined, PlaintextChoice::Revoked] {
                for surface in [ReadoutSurface::SignIn, ReadoutSurface::SignedIn] {
                    let verdict = PlaintextVerdict { machine_id: "fixture".into(), name: "fixture".into(),
                        shared_by: owner.into(), eligibility: probe::PlaintextEligibility::Eligible, choice };
                    let text = plaintext_copy_in(Some(&verdict), surface, &locale);
                    let action = match (choice, surface) {
                        (PlaintextChoice::Undecided, _) => connect,
                        (PlaintextChoice::Allowed, _) | (_, ReadoutSurface::SignIn) => retry,
                        (_, ReadoutSurface::SignedIn) => settings_path,
                    };
                    assert!(text.contains(action), "{preference:?}/{choice:?}/{surface:?}: {text}");
                    if !owner.is_empty() {
                        assert_eq!(text.matches(owner).count(), 1, "the owner remains literal metadata");
                        if preference != Preference::En {
                            assert!(!text.contains("’s"), "English possessive grammar must not leak: {text}");
                        }
                    }
                    assert!(text.chars().count() <= 125, "reason budget: {preference:?}: {text}");
                    assert!(!TextView::new(&text, crate::ui::theme::size::BODY, crate::ui::theme::TEXT_SECONDARY)
                        .max_lines(2).with_measure(&ScalarMeasure)
                        .truncates(StatusOverlay::REASON_W), "complete action must fit: {text}");
                }
            }
        }
    }
}

/// **The "no server yet" reason keeps its two lines for every name.** Line 1 is always the
/// sentence naming the account and fits the reason column (it never wraps); a short name is left
/// alone; a long one is shortened with an ellipsis and the sentence keeps its final period; line 2
/// is always the plain no-server sentence; a blank name has no line 1 at all, so the caller says
/// `browse.auth.no_servers` instead. Graded in every shipped language with the device's advances.
#[test]
fn the_signed_in_reason_names_the_account_on_one_line_for_every_name_length() {
    use crate::fontcov::advances::{ShippedMeasure, HEADROOM};
    use crate::i18n::{language_on_this_thread_for_test, msg, Preference};
    use crate::ui::machine::Measure;
    use crate::ui::widgets::StatusOverlay;
    let column = StatusOverlay::REASON_W * HEADROOM;
    for language in [Preference::En, Preference::Es, Preference::Be] {
        let _guard = language_on_this_thread_for_test(language);
        let second = msg::browse_auth_no_servers();
        for name in ["alexandra", "alexandra.konstantinopolskaya",
            "alexandra.konstantinopolskaya.with.a.very.long.name.indeed",
            "ААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААААА"] {
            let reason = signed_in_reason(name, &ShippedMeasure).expect("a name gives a reason");
            let (first, rest) = reason.split_once('\n').unwrap_or_else(|| panic!("{language:?} {name}: no break"));
            assert_eq!(rest, second, "{language:?} {name}: line 2 is the plain sentence");
            assert!(!rest.contains('\n') && !first.contains('\n'), "{language:?} {name}");
            let width = ShippedMeasure.width_str(first, crate::ui::theme::size::BODY, false);
            assert!(width <= column, "{language:?} {name}: line 1 is {width}px of {column}px: {first:?}");
            assert!(first.ends_with('.'), "{language:?} {name}: the sentence keeps its period: {first:?}");
            if name == "alexandra" {
                assert_eq!(reason, msg::browse_auth_no_servers_signed_in_as(name), "a short name is untouched");
            }
            if first.contains('\u{2026}') {
                assert!(!first.ends_with("\u{2026}."), "{language:?} {name}: the ellipsis sits against the period: {first:?}");
                let at = first.find('\u{2026}').unwrap();
                assert!(at > 3 && first[at + 3..].chars().count() > 3, "{language:?} {name}: not cut in the middle: {first:?}");
                assert!(name.chars().count() > 20, "{language:?} {name}: shortened without need");
                assert!(!first.contains(name), "{language:?}: the full name survived");
            } else {
                assert!(first.contains(name), "{language:?} {name}: {first:?}");
            }
        }
        // the long name really was cut, in the language that says the most around it
        let cut = signed_in_reason("alexandra.konstantinopolskaya.with.a.very.long.name.indeed", &ShippedMeasure).unwrap();
        assert!(cut.lines().next().unwrap().contains('\u{2026}'), "{language:?}: {cut:?}");
        // Cut from the middle, both ends of the name kept, the period left alone.
        if language == Preference::En {
            assert_eq!(signed_in_reason("Maximilian.Wolfgang.Kowalczyk.MMWWMMWWMMWWMMWWAB", &ShippedMeasure).as_deref(),
                Some("Signed in as Maximilian.Wolfgang.\u{2026}.MMWWMMWWMMWWMMWWAB.\nThis Plex account has no server yet."),
                "a 48-character name is cut in the middle, not before the sentence's period");
        }
        assert_eq!(signed_in_reason("", &ShippedMeasure), None);
        assert_eq!(signed_in_reason("  \n\t ", &ShippedMeasure), None);
        // whitespace inside a name cannot break the first line
        let spaced = signed_in_reason("Alex\nandra  K", &ShippedMeasure).unwrap();
        assert_eq!(spaced.matches('\n').count(), 1, "{spaced:?}");
    }
}

/// **A column too narrow for even the bare sentence names nobody.** The reason is `None`, so the
/// caller shows `browse.auth.no_servers`, rather than the full unshortened name overflowing.
#[test]
fn the_signed_in_reason_is_none_when_the_sentence_leaves_the_name_no_room() {
    struct Wide;
    impl crate::ui::machine::Measure for Wide {
        fn width(&self, text: &std::ffi::CStr, _size: i32, _bold: bool) -> f32 { text.to_bytes().len() as f32 * 100.0 }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.7 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.2 }
    }
    assert_eq!(signed_in_reason("alexandra", &Wide), None);
}

/// The user call is not made for a sink that is no longer live (the sign-in was cancelled).
#[test]
fn a_dead_sink_never_asks_for_the_account_name() {
    struct Dead;
    impl owner::ObservationSink for Dead {
        fn live(&self) -> bool { false }
        fn progress(&self, _: AuthProgress) -> bool { false }
        fn terminal(&self, _: AuthProgress) -> bool { false }
    }
    let evidence = crate::telemetry::incident::NoServersEvidence {
        resources: crate::telemetry::incident::CountBucket::One, trigger: DiscoveryTrigger::Login };
    let mut calls = 0;
    let account = no_servers_account_with(&Discovery::NoServers(evidence), &Dead,
        || { calls += 1; Some("n".to_owned()) });
    assert_eq!((account, calls), (None, 0));
}

#[test]
fn localized_discovery_retries_use_the_whole_sentence_and_belarusian_count_rules() {
    use crate::i18n::{LocaleContext, Preference};
    let be = LocaleContext::resolve(Preference::Be, None, None, None, None);
    for (count, phrase) in [(1, "1 раз."), (2, "2 разы."), (5, "5 разоў."),
        (11, "11 разоў."), (21, "21 раз.")] {
        for text in [crate::i18n::msg::browse_auth_plex_dns_retry_in(&be, count),
            crate::i18n::msg::browse_auth_plex_connect_retry_in(&be, count)] {
            assert!(text.contains(phrase), "{text}");
            assert!(text.contains("plex.tv") && text.contains("Праверце злучэнне"), "{text}");
            assert!(!text.contains("We tried"), "an English sentence fragment must never survive");
        }
    }
}

/// `request_result_evidence` against one loopback TLS answer, the way the identity probe makes it.
fn identity_request(port: u16, scheme: &str, learn_pin: bool) -> Result<crate::net::Resp, crate::net::RequestFailure> {
    crate::net::request_result_evidence(
        &format!("{scheme}://127.0.0.1:{port}/identity"),
        &[],
        "GET",
        None,
        crate::net::API,
        false,
        None,
        None,
        learn_pin,
    )
}

fn curl_ready() -> bool {
    let ready = crate::net::global_init() && crate::net::available();
    if !ready {
        eprintln!("curl unavailable on this host; skipping");
    }
    ready
}

/// **Issue #380: a strictly verified TLS answer carries the pin of the served leaf, and only when
/// the request asked.** Both halves of the expectation are independent of the code under test:
/// the pin of the PEM the server serves, and the pin of the key pair the certificate was minted
/// from (`TestCert::spki_der`). The host's libcurl (LibreSSL, 8.x) is not the television's
/// (OpenSSL, 7.53.1); this test passing is the proof that the host reports `CERTINFO`, and a host
/// that did not would fail here rather than skip.
#[test]
fn a_verified_tls_answer_carries_the_leaf_pin_only_when_asked() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = std::sync::Arc::new(crate::net::mint_cert(&["127.0.0.1"]));
    let _ca = TestCaGuard::install(&cert.pem, "pin-learn");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&cert), identity_json("m"));

    let asked = identity_request(port, "https", true).expect("a trusted loopback leaf verifies");
    assert_eq!(asked.peer_pin, crate::spki::pin_from_pem(&cert.pem), "the served leaf's pin");
    assert_eq!(asked.peer_pin, Some(crate::spki::pin_from_spki_der(&cert.spki_der)), "the key pair's pin");

    let not_asked = identity_request(port, "https", false).expect("verifies");
    assert_eq!(not_asked.peer_pin, None, "an ordinary request pays for no chain and learns nothing");
}

/// The chain the host reports starts at the peer's OWN certificate: a leaf issued by a CA is
/// pinned by the leaf's key, not the issuer's. The server sends `[leaf, CA]` (the way a real
/// server does), so the order is what is being proved, not an accident of a one-element list.
#[test]
fn the_pin_is_the_leaf_of_a_ca_issued_chain_not_its_issuer() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = Arc::new(crate::net::mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-30), ymd_from_now(30)).serving_chain());
    let _ca = TestCaGuard::install(&cert.pem, "pin-ca-issued");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&cert), identity_json("m"));
    let resp = identity_request(port, "https", true).expect("a leaf chaining to the trusted CA verifies");
    assert_eq!(resp.peer_pin, Some(crate::spki::pin_from_spki_der(&cert.spki_der)));
    assert_ne!(resp.peer_pin, crate::spki::pin_from_pem(&cert.pem), "not the CA's key");
}

#[test]
fn plaintext_and_failed_verification_learn_no_pin() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = Arc::new(crate::net::mint_cert(&["127.0.0.1"]));
    let _ca = TestCaGuard::install(&cert.pem, "pin-none");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&cert), identity_json("m"));
    // Plaintext: there is no certificate to read, whatever was asked.
    let plain = identity_request(port, "http", true).expect("the twin answers in the clear");
    assert_eq!(plain.peer_pin, None);

    // Verification fails: the expired leaf of `an_expired_leaf_…`. No `Resp` exists to carry a pin,
    // and the failure is the date check the next layer will care about.
    let expired = Arc::new(crate::net::mint_ca_issued_cert(&["127.0.0.1"], ymd_from_now(-90), ymd_from_now(-30)));
    let _expired_ca = TestCaGuard::install(&expired.pem, "pin-expired");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&expired), identity_json("m"));
    let failure = identity_request(port, "https", true).err().expect("an expired leaf must not verify");
    assert_eq!(failure.curl_rc, Some(60));
}

/// Only the identity probe asks libcurl for the chain, and an ordinary control-plane request over
/// the same strictly verified connection comes back with no pin at all.
#[test]
fn only_the_learning_probe_reads_the_peer_key() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = Arc::new(crate::net::mint_cert(&["127.0.0.1"]));
    let _ca = TestCaGuard::install(&cert.pem, "pin-http");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&cert), identity_json("m"));
    let origin = Origin::parse(&format!("https://127.0.0.1:{port}")).unwrap();
    let get = crate::http::Method::Get;
    let hdr = [crate::http::ACCEPT_JSON];
    let ordinary = crate::http::request(&origin, IDENTITY, get, &hdr, None).expect("answers");
    assert_eq!(ordinary.peer_pin, None, "an ordinary request");
    let plain_probe = crate::http::request_probe(&origin, IDENTITY, get, &hdr, 4096, 5, None).expect("answers");
    assert_eq!(plain_probe.peer_pin, None, "a probe that did not ask");
    let learning = crate::http::request_probe_learning_key(&origin, IDENTITY, get, &hdr, 4096, 5, None)
        .expect("answers");
    assert_eq!(learning.peer_pin, Some(crate::spki::pin_from_spki_der(&cert.spki_der)));
}

/// What the session remembers for `machine_id`, once the queued write has landed.
fn learned_pin(machine_id: &str) -> Option<String> {
    crate::storage_worker::drain_for_test();
    crate::plex::session::peek().server_key_pin(machine_id).map(str::to_owned)
}

/// The hash label of the `plex.direct` names these tests dial; plex.tv's is 32 hex digits.
const LEARN_HASH: &str = "0123456789abcdef0123456789abcdef";

/// The name plex.tv would mint for a LAN server at `127.0.0.1`: the only kind of origin that has a
/// `ResolvePin`, and so the only kind the probe learns from.
fn learn_host() -> String {
    format!("127-0-0-1.{LEARN_HASH}.plex.direct")
}

/// One identity probe of a loopback server, graded the way the race grades it: `https` dials the
/// `plex.direct` name through its `ResolvePin` (as `race_batch` and `probe_cached` build one),
/// `http` the bare-address plaintext twin, which has none.
fn probe_and_learn(scheme: &str, port: u16, machine_id: &str, location: probe::Location) -> Outcome {
    let (origin, pin) = if scheme == "https" {
        let origin = Origin::parse(&format!("https://{}:{port}", learn_host())).unwrap();
        let pin = crate::plex::ResolvePin::for_origin(&origin, "127.0.0.1").expect("a dashed plex.direct name pins");
        (origin, Some(pin))
    } else {
        (Origin::parse(&format!("{scheme}://127.0.0.1:{port}")).unwrap(), None)
    };
    get_identity(&origin, pin.as_ref(), Duration::from_secs(5)).grade_learning(machine_id, location).0
}

/// **The pin is learned when, and only when, the answer is accepted for the machine asked for over
/// a strictly verified connection to a pinned `plex.direct` origin.** Every refusal below leaves
/// the session without an entry.
#[test]
fn an_accepted_identity_over_verified_tls_is_remembered_and_nothing_else_is() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let _session = crate::plex::session::TempSession::new("pin-learn-accept");
    let cert = Arc::new(crate::net::mint_cert(&[&learn_host(), "127.0.0.1"]));
    let _ca = TestCaGuard::install(&cert.pem, "pin-learn-accept");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&cert), identity_json("m-real"));
    let want = crate::spki::pin_from_spki_der(&cert.spki_der);

    // A different machine answering at the address: WrongServer, and no key is anyone's.
    assert_eq!(probe_and_learn("https", port, "m-other", probe::Location::Local), Outcome::WrongServer);
    assert_eq!(learned_pin("m-other"), None);
    assert_eq!(learned_pin("m-real"), None);
    // Relay ends at Plex's relay; skipping it is a conservative choice.
    assert_eq!(probe_and_learn("https", port, "m-real", probe::Location::Relay), Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), None, "a relay route is never learned from");
    // Plaintext twin: reachable, but there is no certificate.
    assert_eq!(probe_and_learn("http", port, "m-real", probe::Location::Local), Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), None);
    // Verified HTTPS to an origin WITHOUT a pin (a custom host, a bare address): reachable, but
    // not a name the offline fallback could ever apply to, so it teaches nothing.
    let unpinned = Origin::parse(&format!("https://127.0.0.1:{port}")).unwrap();
    let (outcome, _) = get_identity(&unpinned, None, Duration::from_secs(5)).grade_learning("m-real", probe::Location::Local);
    assert_eq!(outcome, Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), None, "no ResolvePin, no key");

    // The accepted answer.
    assert_eq!(probe_and_learn("https", port, "m-real", probe::Location::Local), Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), Some(want));
    assert_eq!(learned_pin("m-other"), None);
}

#[test]
fn an_identity_that_fails_verification_teaches_no_key() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let _session = crate::plex::session::TempSession::new("pin-learn-unverified");
    // The server's certificate is NOT in the trust store this request verifies against.
    let trusted = Arc::new(crate::net::mint_cert(&[&learn_host()]));
    let stranger = Arc::new(crate::net::mint_cert(&[&learn_host()]));
    let _ca = TestCaGuard::install(&trusted.pem, "pin-learn-unverified");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&stranger), identity_json("m-real"));
    assert_eq!(probe_and_learn("https", port, "m-real", probe::Location::Local), Outcome::Unreachable);
    assert_eq!(learned_pin("m-real"), None);
}

/// A new key replaces the old entry; the same key again writes nothing at all (the probe runs on
/// every boot and every re-discovery).
#[test]
fn a_changed_key_replaces_the_entry_and_the_same_key_costs_no_write() {
    use std::os::unix::fs::MetadataExt;
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let session = crate::plex::session::TempSession::new("pin-learn-replace");
    let file = session.path();
    let first = Arc::new(crate::net::mint_cert(&[&learn_host()]));
    let second = Arc::new(crate::net::mint_cert(&[&learn_host()]));
    // One trusted certificate at a time: both are minted with the same subject, so a bundle holding
    // the two would let the first shadow the second by name.
    let ca_a = TestCaGuard::install(&first.pem, "pin-learn-replace-a");
    let port_a = crate::net::spawn_dual_protocol(Arc::clone(&first), identity_json("m-real"));
    let port_b = crate::net::spawn_dual_protocol(Arc::clone(&second), identity_json("m-real"));

    assert_eq!(probe_and_learn("https", port_a, "m-real", probe::Location::Local), Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), Some(crate::spki::pin_from_spki_der(&first.spki_der)));

    let stamp = |f: &std::path::Path| (std::fs::read(f).unwrap(), std::fs::metadata(f).unwrap().ino());
    let before = stamp(&file);
    assert_eq!(probe_and_learn("https", port_a, "m-real", probe::Location::Local), Outcome::Reachable);
    crate::storage_worker::drain_for_test();
    assert_eq!(stamp(&file), before, "the same key again must not rewrite the session file");

    drop(ca_a);
    let _ca_b = TestCaGuard::install(&second.pem, "pin-learn-replace-b");
    assert_eq!(probe_and_learn("https", port_b, "m-real", probe::Location::Local), Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), Some(crate::spki::pin_from_spki_der(&second.spki_der)));
    let after = crate::plex::session::peek();
    assert_eq!(after.server_key_pins.len(), 1, "one entry per machine");
    assert_ne!(stamp(&file), before);
}

/// **One machine published at a `plex.direct` name AND at a custom host with its own certificate
/// (a reverse proxy) keeps the `plex.direct` leaf's key, however the two answers interleave.**
/// Both routes are dialled by the same race and both answer as the same machine over verified
/// TLS; only the pinned one is a name the offline fallback can apply to, so only it may write. The
/// custom route answers LAST here on purpose: it is the order in which an unconditional learner
/// leaves the proxy's key stored.
#[test]
fn a_custom_host_with_its_own_certificate_never_overwrites_the_plex_direct_key() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let _session = crate::plex::session::TempSession::new("pin-learn-flap");
    // Different subjects, so one bundle can trust both: a CA-issued leaf behind the plex.direct
    // name, a self-signed certificate behind the proxy.
    let direct = Arc::new(crate::net::mint_ca_issued_cert(&[&learn_host()], ymd_from_now(-30), ymd_from_now(30)).serving_chain());
    let proxy = Arc::new(crate::net::mint_cert(&["127.0.0.1"]));
    let _ca = TestCaGuard::install(&format!("{}{}", direct.pem, proxy.pem), "pin-learn-flap");
    let body = identity_json("flapmid");
    let direct_port = crate::net::spawn_dual_protocol(Arc::clone(&direct), body.clone());
    let proxy_port = crate::net::spawn_dual_protocol(Arc::clone(&proxy), body);
    let resource = resource(&format!(
        r#"{{"name":"flap","clientIdentifier":"flapmid","provides":"server","owned":true,
            "sourceTitle":null,"publicAddressMatches":true,"httpsRequired":false,
            "accessToken":"tok-flap","connections":[
              {{"protocol":"https","address":"127.0.0.1","port":{direct_port},
               "uri":"https://{host}:{direct_port}","local":true,"relay":false,"IPv6":false}},
              {{"protocol":"https","address":"127.0.0.1","port":{proxy_port},
               "uri":"https://127.0.0.1:{proxy_port}","local":true,"relay":false,"IPv6":false}}
            ]}}"#,
        host = learn_host()
    ));
    let plan = probe::plan(&resource, CredentialPolicy::HttpsOnly);

    let proxy_answered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let seen = Arc::clone(&proxy_answered);
    let dial: ProbeDial = Arc::new(move |origin, pin, budget| {
        let is_proxy = origin.port() == i32::from(proxy_port);
        if is_proxy {
            std::thread::sleep(Duration::from_millis(400));
        }
        let reply = get_identity(origin, pin, budget);
        if is_proxy {
            seen.store(true, std::sync::atomic::Ordering::Release);
        }
        reply
    });
    let reach = probe_server_racing(&plan, dial, &threaded_spawn, test_policy(), &mut |_, _, _| {});
    assert!(matches!(reach, Reach::At(..)), "both routes answer; one of them is reached");

    // Let the proxy's worker finish, learning or not, before reading what the session kept.
    let give_up = Instant::now() + Duration::from_secs(10);
    while !proxy_answered.load(std::sync::atomic::Ordering::Acquire) && Instant::now() < give_up {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(proxy_answered.load(std::sync::atomic::Ordering::Acquire), "the proxy route was dialled");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        learned_pin("flapmid"),
        Some(crate::spki::pin_from_spki_der(&direct.spki_der)),
        "the pin is the plex.direct leaf's; the custom host's answer wrote nothing"
    );
}

// ---------------------------------------------------------------------------------------------
// Issue #378: a strictly verified request that fails ONLY on the certificate's dates is retried
// recognising the server by its remembered key. Every certificate's dates are relative to NOW, and
// every test keys the process-wide key table by its own loopback server's ephemeral port
// (`net::keypin::Scoped`), so nothing leaks between the threads of the suite; each also holds
// `testlock::serial()` because the CA override they all use is process-global.
// ---------------------------------------------------------------------------------------------

fn expired_leaf(names: &[&str]) -> Arc<crate::net::TestCert> {
    Arc::new(crate::net::mint_ca_issued_cert(names, ymd_from_now(-90), ymd_from_now(-30)))
}

fn not_yet_valid_leaf(names: &[&str]) -> Arc<crate::net::TestCert> {
    Arc::new(crate::net::mint_ca_issued_cert(names, ymd_from_now(30), ymd_from_now(90)))
}

fn valid_leaf(names: &[&str]) -> Arc<crate::net::TestCert> {
    Arc::new(crate::net::mint_ca_issued_cert(names, ymd_from_now(-30), ymd_from_now(30)))
}

/// Remember `cert`'s key for the loopback server on `port`, until the returned guard drops.
fn remember(port: u16, cert: &crate::net::TestCert) -> crate::net::keypin::Scoped {
    crate::net::keypin::Scoped::new(key_of_port(port), &leaf_pin(cert))
}

fn key_of_port(port: u16) -> String {
    crate::net::keypin::key_of("127.0.0.1", i32::from(port))
}

fn leaf_pin(cert: &crate::net::TestCert) -> String {
    crate::spki::pin_from_spki_der(&cert.spki_der)
}

/// A loopback server serving `cert`, trusted through the test CA override.
fn serve_trusted(cert: &Arc<crate::net::TestCert>, tag: &str) -> (TestCaGuard, u16) {
    let ca = TestCaGuard::install(&cert.pem, tag);
    let port = crate::net::spawn_dual_protocol(Arc::clone(cert), identity_json("m"));
    (ca, port)
}

/// An expired leaf and the loopback server serving it, trusted through the test CA override.
fn expired_server(tag: &str) -> (Arc<crate::net::TestCert>, TestCaGuard, u16) {
    let cert = expired_leaf(&["127.0.0.1"]);
    let (ca, port) = serve_trusted(&cert, tag);
    (cert, ca, port)
}

#[test]
fn an_expired_leaf_is_served_when_its_remembered_key_is_known() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let (cert, _ca, port) = expired_server("clock-expired");
    let _key = remember(port, &cert);
    let resp = identity_request(port, "https", false).expect("the date alone must not refuse the server we know");
    assert_eq!(resp.status, 200);
    let facts = crate::net::keypin::fact_for(&key_of_port(port));
    assert!(
        matches!(facts.engaged, Some(Some(year)) if (2025..2200).contains(&year)),
        "key mode engaged, carrying the year the device believed: {facts:?}"
    );
    assert_eq!(facts.blocked, None, "a served request blocks nothing");
}

#[test]
fn a_not_yet_valid_leaf_is_served_when_its_remembered_key_is_known() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = not_yet_valid_leaf(&["127.0.0.1"]);
    let (_ca, port) = serve_trusted(&cert, "clock-not-yet");
    let _key = remember(port, &cert);
    let resp = identity_request(port, "https", false).expect("a leaf from the future, a clock in the past");
    assert_eq!(resp.status, 200);
}

#[test]
fn an_expired_leaf_whose_key_differs_from_the_remembered_one_is_refused_with_a_pin_mismatch() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let (_cert, _ca, port) = expired_server("clock-other-key");
    let someone_else = crate::net::mint_cert(&["127.0.0.1"]);
    let _key = remember(port, &someone_else);
    let failure = identity_request(port, "https", false).err().expect("a stranger's key is not the server's");
    assert_eq!(failure.curl_rc, Some(90));
    assert!(!crate::net::keypin::is_latched(&key_of_port(port)), "a refusal never latches");
    assert_eq!(
        crate::net::keypin::fact_for(&key_of_port(port)).blocked,
        Some(crate::net::keypin::Blocked::KeyChanged),
        "the control plane publishes the changed key",
    );
}

#[test]
fn an_expired_leaf_for_another_name_is_refused_even_with_its_key_remembered() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    // The leaf is issued for a name that is not the one dialled: the host check stays ON in key
    // mode, so remembering the key cannot make any certificate the server's for any name.
    let cert = expired_leaf(&["not-this-name.example"]);
    let (_ca, port) = serve_trusted(&cert, "clock-wrong-name");
    let _key = remember(port, &cert);
    let failure = identity_request(port, "https", false).err().expect("the name check still applies");
    assert!(matches!(failure.curl_rc, Some(51 | 60)), "{failure:?}");
    assert!(!crate::net::keypin::is_latched(&key_of_port(port)));
}

#[test]
fn an_expired_leaf_with_no_remembered_key_is_refused_as_before() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let (_cert, _ca, port) = expired_server("clock-no-key");
    let _watched = crate::net::keypin::Scoped::watch(&key_of_port(port));
    let failure = identity_request(port, "https", false).err().expect("nothing to recognise it by");
    assert_eq!(failure.curl_rc, Some(60));
    assert_eq!(
        crate::net::keypin::fact_for(&key_of_port(port)).blocked,
        Some(crate::net::keypin::Blocked::NoKey),
        "a date failure with no key to fall back on is published",
    );
}

#[test]
fn an_untrusted_issuer_is_refused_even_with_the_leafs_key_remembered() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    // Valid dates, but the CA this request trusts is not the one that issued the leaf: the date
    // fallback is for dates only.
    let served = valid_leaf(&["127.0.0.1"]);
    let trusted = crate::net::mint_cert(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&trusted.pem, "clock-untrusted");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&served), identity_json("m"));
    let _key = remember(port, &served);
    let failure = identity_request(port, "https", false).err().expect("an untrusted issuer is not a date problem");
    assert_eq!(failure.curl_rc, Some(60));
    assert!(!crate::net::keypin::is_latched(&key_of_port(port)));
    assert_eq!(crate::net::keypin::fact_for(&key_of_port(port)).blocked, None, "not a date failure, not a fact");
}

#[test]
fn a_valid_leaf_is_served_strictly_and_key_mode_is_never_engaged() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = valid_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-valid");
    let served = crate::net::spawn_observed(Arc::clone(&cert), identity_json("m"));
    let key = key_of_port(served.port);
    let _key = crate::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));
    let resp = identity_request(served.port, "https", true).expect("verifies");
    assert_eq!(resp.peer_pin, Some(leaf_pin(&cert)), "strict mode still learns");
    assert!(!crate::net::keypin::is_latched(&key));
    assert_eq!(served.accepted(), 1, "one handshake: no retry");
}

#[test]
fn a_key_mode_answer_carries_no_peer_pin_and_teaches_nothing() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let (cert, ca, port) = expired_server("clock-no-learn");
    let _key = remember(port, &cert);
    let resp = identity_request(port, "https", true).expect("served by its key");
    assert_eq!(resp.peer_pin, None, "that handshake was not strictly verified");
    drop(ca);

    // And through the real probe, which is the only thing that learns: the answer is accepted for
    // the machine and still no key reaches the session.
    let _session = crate::plex::session::TempSession::new("clock-no-learn");
    let named = expired_leaf(&[&learn_host()]);
    let _named_ca = TestCaGuard::install(&named.pem, "clock-no-learn-named");
    let port = crate::net::spawn_dual_protocol(Arc::clone(&named), identity_json("m-real"));
    let _named_key = crate::net::keypin::Scoped::new(
        crate::net::keypin::key_of(&learn_host(), i32::from(port)), &leaf_pin(&named));
    assert_eq!(probe_and_learn("https", port, "m-real", probe::Location::Local), Outcome::Reachable);
    assert_eq!(learned_pin("m-real"), None, "a key-mode handshake never reaches learn_server_key");
}

#[test]
fn after_a_fallback_succeeds_later_requests_make_one_handshake_until_the_pin_changes() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-latch");
    let served = crate::net::spawn_observed(Arc::clone(&cert), identity_json("m"));
    let key = key_of_port(served.port);
    let _key = crate::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));

    identity_request(served.port, "https", false).expect("first: strict fails, key mode serves");
    assert_eq!(served.accepted(), 2, "the failed strict handshake and the key-mode one");
    assert!(crate::net::keypin::is_latched(&key));

    identity_request(served.port, "https", false).expect("second: straight to key mode");
    assert_eq!(served.accepted(), 3, "exactly one handshake");

    // A pin change for the host clears the latch: the next request starts strict again.
    let other = crate::net::mint_cert(&["127.0.0.1"]);
    crate::net::keypin::set_for_test(&key, &leaf_pin(&other));
    assert!(!crate::net::keypin::is_latched(&key));
    let failure = identity_request(served.port, "https", false).err().expect("the new pin is not the server's");
    assert_eq!(failure.curl_rc, Some(90));
    assert_eq!(served.accepted(), 5, "strict first again, then key mode");
}

#[test]
fn a_pin_mismatch_in_key_mode_clears_the_latch_and_is_the_failure() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-latch-mismatch");
    let served = crate::net::spawn_observed(Arc::clone(&cert), identity_json("m"));
    let key = key_of_port(served.port);
    // The latch stands on a key the server does not present (it rotated while we were latched).
    let wrong = leaf_pin(&crate::net::mint_cert(&["127.0.0.1"]));
    let _key = crate::net::keypin::Scoped::new(key.clone(), &wrong);
    crate::net::keypin::key_established(&key, &wrong, Some(10));
    assert!(crate::net::keypin::is_latched(&key));
    let failure = identity_request(served.port, "https", false).err().expect("the key changed");
    assert_eq!(failure.curl_rc, Some(90));
    assert_eq!(served.accepted(), 1, "latched: no strict attempt first");
    assert!(!crate::net::keypin::is_latched(&key), "rc 90 ends key mode for the host");
    assert_eq!(
        crate::net::keypin::fact_for(&key).blocked,
        Some(crate::net::keypin::Blocked::KeyChanged),
        "the engaged fact stands and the change is published",
    );
    assert!(crate::net::keypin::fact_for(&key).engaged.is_some(), "…and the engaged fact survives the refusal");
}

#[test]
fn a_post_body_survives_the_retry_once_and_intact() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-post");
    let served = crate::net::spawn_observed(Arc::clone(&cert), identity_json("m"));
    let _key = remember(served.port, &cert);
    let payload: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
    let resp = crate::net::request_result_evidence(
        &format!("https://127.0.0.1:{}/hubs/search", served.port),
        &["Content-Type: application/octet-stream".to_owned()],
        "POST",
        Some(&payload),
        crate::net::API,
        false,
        None,
        None,
        false,
    )
    .expect("the retried POST is answered");
    assert_eq!(resp.status, 200);
    let seen = served.requests.lock().unwrap();
    assert_eq!(seen.len(), 1, "the strict handshake failed before anything was sent");
    let request = &seen[0];
    let head_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert!(request.starts_with(b"POST /hubs/search"), "{:?}", String::from_utf8_lossy(&request[..40]));
    assert_eq!(&request[head_end..], &payload[..], "the body arrived once and intact");
}

// ---------------------------------------------------------------------------------------------
// Issue #378, media plane: `curlio::CurlSource` opens, reopens and seeks the same way.
// ---------------------------------------------------------------------------------------------

fn media_body() -> Vec<u8> {
    (0..5000u32).map(|i| (i % 253) as u8).collect()
}

fn media_url(port: u16) -> String {
    format!("https://127.0.0.1:{port}/video.mkv")
}

fn read_n(src: &mut crate::curlio::CurlSource, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    let mut got = 0;
    while got < n {
        let r = src.read(&mut out[got..]);
        assert!(r > 0, "read returned {r} after {got} bytes");
        got += r as usize;
    }
    out
}

#[test]
fn a_media_open_on_an_expired_leaf_reads_bytes_when_its_remembered_key_is_known() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-open");
    let served = crate::net::spawn_observed(Arc::clone(&cert), media_body());
    let _key = remember(served.port, &cert);
    let mut src = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .expect("the date alone must not refuse the server we know");
    assert_eq!(src.status(), 200);
    assert_eq!(read_n(&mut src, 64), media_body()[..64]);
}

#[test]
fn a_media_open_whose_key_differs_from_the_remembered_one_fails_with_a_pin_mismatch() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-mismatch");
    let served = crate::net::spawn_observed(Arc::clone(&cert), media_body());
    let other = crate::net::mint_cert(&["127.0.0.1"]);
    let key = key_of_port(served.port);
    let _key = crate::net::keypin::Scoped::new(key.clone(), &leaf_pin(&other));
    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .err()
        .expect("a stranger's key is not the server's");
    assert_eq!(err, crate::curlio::OpenErr::Transport(90));
    assert!(!crate::net::keypin::is_latched(&key));
    assert_eq!(
        crate::net::keypin::fact_for(&key).blocked,
        Some(crate::net::keypin::Blocked::KeyChanged),
        "the media plane publishes the changed key too",
    );
}

#[test]
fn a_media_open_on_an_expired_leaf_with_no_remembered_key_is_refused_as_before() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-nokey");
    let served = crate::net::spawn_observed(Arc::clone(&cert), media_body());
    let _watched = crate::net::keypin::Scoped::watch(&key_of_port(served.port));
    let err = crate::curlio::CurlSource::open(&media_url(served.port), 0)
        .err()
        .expect("nothing to recognise it by");
    assert_eq!(err, crate::curlio::OpenErr::Transport(60));
    assert_eq!(
        crate::net::keypin::fact_for(&key_of_port(served.port)).blocked,
        Some(crate::net::keypin::Blocked::NoKey),
        "the media plane publishes a date failure with no key",
    );
}

#[test]
fn media_seeks_and_reopens_in_key_mode_each_do_their_own_handshake() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-latch");
    let served = crate::net::spawn_observed(Arc::clone(&cert), media_body());
    let key = key_of_port(served.port);
    let _key = crate::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));

    let mut src = crate::curlio::CurlSource::open(&media_url(served.port), 0).expect("first open");
    assert_eq!(served.accepted(), 2, "the failed strict handshake and the key-mode one");
    assert!(crate::net::keypin::is_latched(&key));
    assert_eq!(read_n(&mut src, 16), media_body()[..16]);

    assert!(src.seek(1000), "a seek in key mode");
    assert_eq!(served.accepted(), 3, "exactly one handshake");
    assert_eq!(read_n(&mut src, 16), media_body()[1000..1016]);

    src.reopen_until(&media_url(served.port), None, &mut crate::checkpoint::NoCheckpoint)
        .expect("a reopen in key mode");
    assert_eq!(served.accepted(), 4, "exactly one handshake");
    assert_eq!(read_n(&mut src, 16), media_body()[..16]);

    // The remembered key stops being the server's: the next seek fails closed with rc 90 and the
    // host is out of key mode.
    let other = crate::net::mint_cert(&["127.0.0.1"]);
    crate::net::keypin::set_for_test(&key, &leaf_pin(&other));
    assert!(!src.seek(2000), "the new pin is not the server's");
    assert!(!crate::net::keypin::is_latched(&key));
}

/// **A key-mode transfer never rides a kept-alive connection.** The source keeps its multi and
/// connection cache across attempts, and a handle that reused a cached connection would perform no
/// handshake, so there would be no certificate to confirm and the open would be refused as a pin
/// mismatch — which every HLS segment after the first and every seek after a read to EOF would hit.
/// The `Connection: close` double used above cannot see that, so this one keeps connections alive.
#[test]
fn media_key_mode_never_reuses_a_kept_alive_connection() {
    let _serial = crate::testlock::serial();
    if !curl_ready() { return; }
    let cert = expired_leaf(&["127.0.0.1"]);
    let _ca = TestCaGuard::install(&cert.pem, "clock-media-keepalive");
    let served = crate::net::spawn_observed_keepalive(Arc::clone(&cert), media_body());
    let key = key_of_port(served.port);
    let _key = crate::net::keypin::Scoped::new(key.clone(), &leaf_pin(&cert));

    let mut src = crate::curlio::CurlSource::open(&media_url(served.port), 0).expect("first open");
    assert_eq!(served.accepted(), 2, "the failed strict handshake and the key-mode one");
    assert!(crate::net::keypin::is_latched(&key));

    // Read to the end: the transfer completes cleanly, which is what leaves a connection in the
    // cache for a following handle to find.
    let mut all = Vec::new();
    let mut buf = [0u8; 700];
    loop {
        let n = src.read(&mut buf);
        assert!(n >= 0, "read failed with {n} after {} bytes", all.len());
        if n == 0 {
            break;
        }
        all.extend_from_slice(&buf[..n as usize]);
    }
    assert_eq!(all, media_body());

    assert!(src.seek(1000), "a seek after a read to EOF must succeed in key mode");
    assert_eq!(served.accepted(), 3, "its own handshake, not the cached connection");
    assert_eq!(read_n(&mut src, 16), media_body()[1000..1016]);

    src.reopen_until(&media_url(served.port), None, &mut crate::checkpoint::NoCheckpoint)
        .expect("a reopen in key mode");
    assert_eq!(served.accepted(), 4, "its own handshake, not the cached connection");
    assert_eq!(read_n(&mut src, 16), media_body()[..16]);

    assert!(
        crate::net::keypin::is_latched(&key),
        "no pin-mismatch outcome: the host is still served in key mode"
    );
    assert_eq!(crate::net::keypin::pin_for_test(&key).as_deref(), Some(leaf_pin(&cert).as_str()));
}
