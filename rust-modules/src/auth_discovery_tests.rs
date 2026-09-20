//! Server discovery and probe-racing tests: retry policy, candidate racing, relay fallback,
//! identity verification, address dialability, and the resolve-roster end-to-end scenarios.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|_, _, _| (200, identity_json("race-machine")));
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
    let dial: ProbeDial = Arc::new(|_, _, _| panic!("a refused job must never run"));
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
    let dial: ProbeDial = Arc::new(move |origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|_, _, _| (200, identity_json("race-machine")));
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
    let dial: ProbeDial = Arc::new(move |origin, _, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
        matches!(reach, Reach::InsecureOnly(_)),
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
    let dial: ProbeDial = Arc::new(|origin, _, _| {
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
        Arc::new(|origin, _, _| {
            if origin.host().starts_with("192-") {
                (200, identity_json("race-machine"))
            } else {
                (0, Vec::new())
            }
        }),
    );
    assert_case(
        &probe::plan(&issue_95_account(true), CredentialPolicy::HttpsOnly),
        Arc::new(issue_95_dial),
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
    let dial: ProbeDial = Arc::new(move |origin, pin, budget| {
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
    let dial: ProbeDial = Arc::new(issue_95_dial);
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
    let dial: ProbeDial = Arc::new(issue_95_dial);
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
        resolved_without_roster(Resolved::NoServers),
        Err(Discovery::NoServers)
    ));
    assert!(matches!(
        resolved_without_roster(Resolved::None { refused: true, insecure: false }),
        Err(Discovery::Refused)
    ));
    assert!(matches!(
        resolved_without_roster(Resolved::None { refused: false, insecure: false }),
        Err(Discovery::Silent(None))
    ));
    assert!(
        matches!(
            resolved_without_roster(Resolved::None { refused: true, insecure: true }),
            Err(Discovery::InsecureOnly)
        ),
        "a verified plaintext answer outranks a parallel/proxy 401"
    );
    assert!(matches!(
        resolved_without_roster(Resolved::None { refused: false, insecure: true }),
        Err(Discovery::InsecureOnly)
    ));
    assert!(matches!(resolved_without_roster(Resolved::Reached(vec![])), Ok(v) if v.is_empty()));
}

/// Sign-in and rediscovery must say the SAME sentence for the SAME verdict — the whole point
/// of a shared const (plan §4) is that the two paths cannot drift apart on this copy the way
/// the three other Discovery failures never had a name collision to drift on. `discover_and_store`
/// itself needs a live plex.tv edge no host test can reach, so this pins the SHARED CONST's
/// content directly; the two call sites (`login_worker_with_output`, `rediscovery_worker_with_output`)
/// are both spelled `output_failed(output, epoch, DISCOVERY_INSECURE_ONLY_MESSAGE)` — greppable,
/// and unable to drift apart without a compile error renaming one identifier but not the other.
#[test]
fn insecure_only_copy_is_one_shared_const_naming_the_fixable_cause() {
    assert!(!DISCOVERY_INSECURE_ONLY_MESSAGE.is_empty());
    assert!(DISCOVERY_INSECURE_ONLY_MESSAGE.contains("securely"));
    assert!(DISCOVERY_INSECURE_ONLY_MESSAGE.contains("HTTPS"));
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
    let dial: ProbeDial = Arc::new(move |origin, pin, budget| {
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
    let dial: ProbeDial = Arc::new(issue_95_dial_pinned);
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

/// RAII guard for the process-global CA override (`net::test_ca_bundle`): writes `pem` to a
/// scratch file, installs it as curl's trusted CAINFO for the duration, and always clears the
/// override (and deletes the file) on drop — including on panic/unwind — so a failing
/// assertion in one of the tests below can never leak a trusted CA into another test running
/// after it under the same `testlock::serial()` guard.
struct TestCaGuard(std::path::PathBuf);

impl TestCaGuard {
    fn install(pem: &str, tag: &str) -> TestCaGuard {
        let path = std::env::temp_dir().join(format!(
            "plxnative-test-ca-{tag}-{}-{:?}.pem",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::write(&path, pem).expect("write scratch CA bundle");
        let path_str = path.to_string_lossy().into_owned();
        crate::net::test_ca_bundle::set(Some(&path_str));
        TestCaGuard(path)
    }
}

impl Drop for TestCaGuard {
    fn drop(&mut self) {
        crate::net::test_ca_bundle::set(None);
        let _ = std::fs::remove_file(&self.0);
    }
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
/// (`get_identity` → `crate::http::request_probe` → `crate::net::request_result`) rather
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
        Resolved::NoServers => panic!("expected Resolved::None{{insecure:true}}, got NoServers"),
    }
    assert!(matches!(
        resolved_without_roster(resolved),
        Err(Discovery::InsecureOnly)
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
    let dial: ProbeDial = Arc::new(move |_origin, pin, _budget| {
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
    assert_eq!(
        primary_index(&roster),
        0,
        "ours is the primary and becomes `current`"
    );

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
        Resolved::NoServers
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
    assert_eq!(primary_index(&roster), 0);
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
    let (_, silent) = discovery_failure(&Discovery::Silent(None)).expect("a failure");
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
                discovery_failure(&Discovery::Silent(Some(last))).expect("a failure");
            assert_eq!(incident.kind, IncidentKind::Authorization, "{status}");
            assert_eq!(incident.http_status, Some(status));
            assert!(!message.contains("connection"), "{status}: {message:?} blames the network");
        }
    }
    // Real silence, and other answers, stay what they were.
    for last in [None, Some(Ok(503)), Some(Ok(429))] {
        let (message, incident) = discovery_failure(&Discovery::Silent(last)).expect("a failure");
        assert_eq!(incident.kind, IncidentKind::Discovery(DiscoveryClass::Silent), "{last:?}");
        assert!(message.contains("connection"));
    }
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
