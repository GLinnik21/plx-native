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
    let dial: ProbeDial = Arc::new(|origin, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _| {
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
    let dial: ProbeDial = Arc::new(|_, _| (200, identity_json("race-machine")));
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
    let dial: ProbeDial = Arc::new(|_, _| panic!("a refused job must never run"));
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
    });
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_by_dial = Arc::clone(&seen);
    let dial: ProbeDial = Arc::new(move |origin, _| {
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
    });
    let dial: ProbeDial = Arc::new(|origin, _| {
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
    });
    let dial: ProbeDial = Arc::new(|origin, _| {
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
    let dial: ProbeDial = Arc::new(|_, _| (200, identity_json("race-machine")));
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
    let dial: ProbeDial = Arc::new(|origin, _| {
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
    let dial: ProbeDial = Arc::new(|origin, _| {
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
        &mut |plan| {
            order.push(plan.machine_id.clone());
            Reach::No
        },
        &mut || gaps += 1,
        &mut |_, _, _| {},
    );
    assert!(matches!(resolved, Resolved::None { refused: false }));
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
    };
    let origin = winner.origin().expect("fixture origin");
    let mut observed = Vec::new();
    let resolved = resolve_roster_using(
        &resources,
        &[],
        &mut |plan| match plan.machine_id.as_str() {
            "yes" => Reach::At(winner.clone(), origin.clone()),
            "denied" => Reach::Refused,
            _ => Reach::No,
        },
        &mut || {},
        &mut |plan, outcome, tier| observed.push((plan.machine_id.clone(), outcome, tier)),
    );

    assert!(matches!(resolved, Resolved::Reached(ref roster) if roster.len() == 1));
    assert_eq!(
        observed,
        vec![
            (
                "yes".into(),
                Outcome::Reachable,
                Some(probe::Location::Remote)
            ),
            ("denied".into(), Outcome::Unauthorized, None),
            ("off".into(), Outcome::Unreachable, None),
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
        },
        SettledProbe {
            machine_id: "denied".into(),
            outcome: Outcome::Unauthorized,
            tier: None,
        },
        SettledProbe {
            machine_id: "off".into(),
            outcome: Outcome::Unreachable,
            tier: None,
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
    let plan = probe::plan(&a_share());
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

    let plan = probe::plan(&a_share());
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
    let plan = probe::plan(&a_share());
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
    let mut plan = probe::plan(&a_share());
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
    let plan = probe::plan(&res);
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
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], &|o| d.dial(o))
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
        roster.iter().all(|s| s.usable()),
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
        resolve_roster(&a_two_server_account(), &[], &|o| d.dial(o))
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
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], &|o| d.dial(o))
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
        resolve_roster(&players, &[], &|_| (0, Vec::new())),
        Resolved::NoServers
    ));

    // servers that simply do not answer
    let silent = Dialled::new(vec![]);
    assert!(matches!(
        resolve_roster(&a_two_server_account(), &[], &|o| silent.dial(o)),
        Resolved::None { refused: false }
    ));

    // …and one that answers 401: something in front of it refuses unauthenticated requests,
    // which is not a network fault and must not be worded as one
    let refused = Dialled::new(vec![
        ("192.168.0.10", 401, Vec::new()),
        ("203.0.113.9", 401, Vec::new()),
    ]);
    assert!(matches!(
        resolve_roster(&a_two_server_account(), &[], &|o| refused.dial(o)),
        Resolved::None { refused: true }
    ));

    // a share that answers while OUR server is off still signs in — a friend's library beats
    // "no server found" — and it becomes the primary because it is the only thing there is
    let one = Dialled::new(vec![("203.0.113.9", 200, identity_json("bbbb2222"))]);
    let Resolved::Reached(roster) =
        resolve_roster(&a_two_server_account(), &[], &|o| one.dial(o))
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

