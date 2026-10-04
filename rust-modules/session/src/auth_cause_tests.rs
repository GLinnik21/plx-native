//! **Why a server could not be used, and what each failure says to the person.** A television whose
//! trust store lacks a server's root showed "<profile> has no access to this server" for every
//! profile: `no_source_access` was the catch-all for anything that did not verify, so an untrusted
//! certificate, a dead server and a 401 all sent the owner to check account permissions.
//!
//! These drive the real race, verdict and worker bodies against scripted answers: the failure's
//! libcurl evidence goes in at the dial, the read-out text and the events-log line come out.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;
use plx_net::net::{RequestError, RequestFailure};
use plx_platform::i18n::msg;

/// What one scripted address does when dialled.
#[derive(Clone, Copy)]
enum Answer {
    /// libcurl 60 with this X509 verify result.
    Tls(i64),
    /// libcurl 28.
    Timeout,
    /// libcurl 7.
    Refused,
    /// HTTP 401.
    Unauthorized,
    /// 200, naming this machine.
    Verified(&'static str),
}

fn reply(answer: Answer) -> ProbeReply {
    let failed = |cause, rc, verify| ProbeReply::Failed(Some(RequestFailure {
        cause, status: None, body_limit: None, curl_rc: Some(rc), verify }));
    match answer {
        Answer::Tls(v) => failed(RequestError::Transport, 60, Some(v as _)),
        Answer::Timeout => failed(RequestError::TimedOut, 28, None),
        Answer::Refused => failed(RequestError::Transport, 7, None),
        Answer::Unauthorized => ProbeReply::Answered { status: 401, body: Vec::new(), peer_pin: None },
        Answer::Verified(mid) => ProbeReply::Answered { status: 200, body: identity_json(mid), peer_pin: None },
    }
}

/// A dial that answers by the host it is asked for: the first rule whose key the host contains.
fn dial_by_host(rules: Vec<(&'static str, Answer)>) -> ProbeDial {
    Arc::new(move |origin, _, _| {
        let host = origin.host().to_owned();
        let (_, answer) = rules.iter().find(|(key, _)| host.contains(key))
            .unwrap_or_else(|| panic!("no scripted answer for {host}"));
        reply(*answer)
    })
}

fn race(rules: Vec<(&'static str, Answer)>) -> Reach {
    probe_server_racing(&race_plan(), dial_by_host(rules), &threaded_spawn, test_policy(), &mut |_, _, _| {})
}

// ---- the race: one server, several routes -------------------------------------------------------

#[test]
fn a_chain_the_trust_store_cannot_verify_settles_as_tls_untrusted_not_as_silence() {
    for verify in [2, 18, 19, 20, 21] {
        let reach = race(vec![("192-0-2-10", Answer::Tls(verify)), ("203-0-113-9", Answer::Tls(verify))]);
        assert!(matches!(reach, Reach::TlsUntrusted(v) if i64::from(v) == verify), "verify {verify}");
    }
}

#[test]
fn the_clock_cases_and_every_other_failure_stay_unreachable() {
    for (name, answer) in [("not yet valid", Answer::Tls(9)), ("expired", Answer::Tls(10)),
        ("no verify result", Answer::Tls(0)), ("timeout", Answer::Timeout), ("refused", Answer::Refused)] {
        let reach = race(vec![("192-0-2-10", answer), ("203-0-113-9", answer)]);
        assert!(matches!(reach, Reach::No), "{name}");
    }
}

#[test]
fn an_untrusted_route_beside_a_silent_one_still_names_the_certificate() {
    let reach = race(vec![("192-0-2-10", Answer::Timeout), ("203-0-113-9", Answer::Tls(20))]);
    assert!(matches!(reach, Reach::TlsUntrusted(20)));
}

/// Precedence within one server: `At` > `InsecureOnly` > `Refused` > `TlsUntrusted` > `No`.
#[test]
fn a_verified_route_or_a_401_outranks_an_untrusted_one() {
    assert!(matches!(race(vec![("192-0-2-10", Answer::Verified("race-machine")), ("203-0-113-9", Answer::Tls(20))]),
        Reach::At(..)));
    assert!(matches!(race(vec![("192-0-2-10", Answer::Unauthorized), ("203-0-113-9", Answer::Tls(20))]),
        Reach::Refused));
}

#[test]
fn the_verdict_carries_the_cause_beside_an_unchanged_outcome() {
    for (reach, outcome, cause) in [
        (Reach::TlsUntrusted(20), Outcome::Unreachable, Cause::TlsUntrusted { verify: 20 }),
        (Reach::No, Outcome::Unreachable, Cause::Unreachable),
        (Reach::Refused, Outcome::Unauthorized, Cause::Unauthorized),
    ] {
        let v = probe_verdict(&reach);
        assert_eq!((v.outcome, v.cause), (outcome, Some(cause)));
    }
}

// ---- the closed value other code reads ----------------------------------------------------------

#[test]
fn a_settled_probe_names_its_cause_and_an_old_one_reads_as_its_outcome() {
    let plan = race_plan();
    let probe = settled_probe(&plan, Outcome::Unreachable, None, None)
        .with_cause(Some(Cause::TlsUntrusted { verify: 20 }));
    assert_eq!((probe.machine_id(), probe.cause()), ("race-machine", Some(Cause::TlsUntrusted { verify: 20 })));
    // A probe recorded before the cause existed (no field at all) means what its outcome says.
    let old: SettledProbe = serde_json::from_str(
        r#"{"machine_id":"m","outcome":3,"tier":null}"#).expect("an old probe still loads");
    assert_eq!(old.cause(), Some(Cause::Unreachable));
    let old: SettledProbe = serde_json::from_str(r#"{"machine_id":"m","outcome":2,"tier":null}"#).unwrap();
    assert_eq!(old.cause(), Some(Cause::Unauthorized));
    let verified: SettledProbe = serde_json::from_str(r#"{"machine_id":"m","outcome":0,"tier":null}"#).unwrap();
    assert_eq!(verified.cause(), None);
}

#[test]
fn a_settled_probe_round_trips_its_cause_and_omits_it_when_there_is_none() {
    let plan = race_plan();
    let with = settled_probe(&plan, Outcome::Unreachable, None, None).with_cause(Some(Cause::TlsUntrusted { verify: 21 }));
    let json = serde_json::to_string(&with).unwrap();
    assert!(json.contains(r#""cause":{"tls_untrusted":{"verify":21}}"#), "{json}");
    assert_eq!(serde_json::from_str::<SettledProbe>(&json).unwrap().cause(), Some(Cause::TlsUntrusted { verify: 21 }));
    let plain = serde_json::to_string(&settled_probe(&plan, Outcome::Reachable, None, None)).unwrap();
    assert!(!plain.contains("cause"), "a verdict without a cause serializes exactly as it always did: {plain}");
}

#[test]
fn failure_causes_count_by_cause_and_keep_the_verify_codes_per_machine() {
    let mut causes = FailureCauses::default();
    assert!(causes.is_empty() && causes.worst().is_none());
    causes.push("a", Cause::Unreachable);
    assert_eq!(causes.worst(), Some(Cause::Unreachable));
    causes.push("b", Cause::Unauthorized);
    assert_eq!(causes.worst(), Some(Cause::Unauthorized));
    causes.push("c", Cause::TlsUntrusted { verify: 20 });
    causes.push("d", Cause::TlsUntrusted { verify: 18 });
    causes.push("a", Cause::Unreachable); // a re-probe of one machine replaces, never double-counts
    assert_eq!(causes.worst(), Some(Cause::TlsUntrusted { verify: 20 }));
    assert_eq!((causes.unreachable(), causes.tls_untrusted(), causes.unauthorized()), (1, 2, 1));
    assert_eq!(causes.untrusted().collect::<Vec<_>>(), [("c", 20), ("d", 18)]);
    assert_eq!(causes.log_line(),
        "auth: no server verified \u{2014} unreachable=1 tls_untrusted=2 unauthorized=1 verify=[20,18]");
}

// ---- sign-in discovery --------------------------------------------------------------------------

fn none(refused: bool, insecure: bool, causes: FailureCauses) -> Resolved {
    Resolved::None { refused, insecure, evidence: None, causes }
}

fn untrusted_causes() -> FailureCauses {
    let mut causes = FailureCauses::default();
    causes.push("m1", Cause::TlsUntrusted { verify: 20 });
    causes.push("m2", Cause::Unreachable);
    causes
}

/// Plan §4's precedence, with the new verdict between `Refused` and `No`:
/// `At` > `InsecureOnly` > `Refused` > `TlsUntrusted` > `No`.
#[test]
fn discovery_orders_an_untrusted_certificate_between_a_refusal_and_silence() {
    let login = DiscoveryTrigger::Login;
    assert!(matches!(resolved_without_roster(none(false, false, untrusted_causes()), login),
        Err(Discovery::TlsUntrusted { .. })));
    assert!(matches!(resolved_without_roster(none(true, false, untrusted_causes()), login),
        Err(Discovery::Refused)), "a 401 outranks an untrusted certificate");
    assert!(matches!(resolved_without_roster(none(false, true, untrusted_causes()), login),
        Err(Discovery::InsecureOnly(_))), "a verified plaintext answer outranks it");
    let mut silent = FailureCauses::default();
    silent.push("m1", Cause::Unreachable);
    assert!(matches!(resolved_without_roster(none(false, false, silent), login),
        Err(Discovery::ServersUnreachable { trigger: DiscoveryTrigger::Login })));
}

#[test]
fn the_untrusted_discovery_says_the_new_sentence_and_reports_as_the_silent_class() {
    let trigger = DiscoveryTrigger::Rediscover;
    let Err(verdict) = resolved_without_roster(none(false, false, untrusted_causes()), trigger) else {
        panic!("no roster")
    };
    let (message, incident) = discovery_failure(&verdict).expect("a failure");
    assert_eq!(message, msg::browse_auth_tls_untrusted());
    assert_ne!(message, msg::browse_auth_servers_unreachable(), "this is not 'none of them answered'");
    // The telemetry schema is the privacy-reviewed closed one: no new class, no verify code. The
    // verdict reports as exactly what an unreachable-servers discovery reports as.
    let (_, silent) = discovery_failure(&Discovery::ServersUnreachable { trigger }).unwrap();
    assert_eq!(format!("{incident:?}"), format!("{silent:?}"));
    assert!(matches!(verdict, Discovery::TlsUntrusted { trigger: DiscoveryTrigger::Rediscover }));
}

#[test]
fn a_roster_probe_tallies_one_cause_per_server_that_did_not_verify() {
    let resources = vec![server("m-tls", "Tls", 1), server("m-dead", "Dead", 2), server("m-ok", "Ok", 3)];
    let rules = vec![("192-0-2-1.", Answer::Tls(20)), ("192-0-2-2.", Answer::Timeout),
        ("192-0-2-3.", Answer::Verified("m-ok"))];
    let dial = dial_by_host(rules);
    let mut probe_one = |plan: &ProbePlan| probe_server_racing(plan, Arc::clone(&dial), &threaded_spawn,
        test_policy(), &mut |_, _, _| {});
    let Resolved::Reached(found) = resolve_roster_using(&resources, &[], CredentialPolicy::HttpsOnly,
        &mut probe_one, &mut || {}, &mut |_, _| {}) else { panic!("one server verified") };
    assert_eq!(found.len(), 1);
    let rules = vec![("192-0-2-1.", Answer::Tls(20)), ("192-0-2-2.", Answer::Timeout),
        ("192-0-2-3.", Answer::Tls(21))];
    let dial = dial_by_host(rules);
    let mut probe_one = |plan: &ProbePlan| probe_server_racing(plan, Arc::clone(&dial), &threaded_spawn,
        test_policy(), &mut |_, _, _| {});
    // Each server's race publishes its own verdict as it settles — the machine id and the cause —
    // which is where whatever asks the person about a certificate reads it from.
    let mut settled = Vec::new();
    let Resolved::None { causes, refused, insecure, .. } = resolve_roster_using(&resources, &[],
        CredentialPolicy::HttpsOnly, &mut probe_one, &mut || {}, &mut |plan, v| settled.push(settled_probe_of(plan, v)))
        else { panic!("nothing verified") };
    assert_eq!(settled.iter().map(|p| (p.machine_id(), p.cause())).collect::<Vec<_>>(), [
        ("m-tls", Some(Cause::TlsUntrusted { verify: 20 })),
        ("m-dead", Some(Cause::Unreachable)),
        ("m-ok", Some(Cause::TlsUntrusted { verify: 21 })),
    ]);
    assert!(!refused && !insecure);
    assert_eq!(causes.log_line(),
        "auth: no server verified \u{2014} unreachable=1 tls_untrusted=2 unauthorized=0 verify=[20,21]");
}

// ---- the profile switch -------------------------------------------------------------------------

fn server(mid: &str, name: &str, octet: u8) -> Resource {
    resource(&format!(
        r#"{{"name":"{name}","clientIdentifier":"{mid}","provides":"server","owned":true,
            "sourceTitle":null,"publicAddressMatches":true,"httpsRequired":true,
            "accessToken":"tok-{mid}","connections":[
              {{"protocol":"https","address":"192.0.2.{octet}","port":32400,
                "uri":"https://192-0-2-{octet}.h.plex.direct:32400","local":true,"relay":false,"IPv6":false}}]}}"#))
}

/// The profile worker's I/O with the network replaced by a script: every probe is the production
/// race, plaintext settlement and verdict over [`dial_by_host`]'s answers.
struct ScriptedIo { servers: Vec<(String, u8)>, dial: ProbeDial }

impl ScriptedIo {
    fn new(servers: &[(&str, u8, Answer)]) -> Self {
        let rules = servers.iter().map(|(_, octet, answer)| {
            let key: &'static str = Box::leak(format!("192-0-2-{octet}.").into_boxed_str());
            (key, *answer)
        }).collect();
        Self {
            servers: servers.iter().map(|(mid, octet, _)| ((*mid).to_owned(), *octet)).collect(),
            dial: dial_by_host(rules),
        }
    }
}

impl ProfileWorkIo for ScriptedIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(plx_plex::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(), ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, CallEvidence> { Ok(self.servers.iter().map(|(mid, octet)| server(mid, mid, *octet)).collect()) }
    fn probe(&mut self, resource: &Resource, household: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        probe_profile_resource_with(resource, household, &[], &PlaintextAsk::undecided(),
            Arc::clone(&self.dial), &threaded_spawn, test_policy())
    }
    fn gap(&mut self) {}
}

#[derive(Default)]
struct Failures(std::cell::RefCell<Vec<String>>);
impl owner::ObservationSink for Failures {
    fn live(&self) -> bool { true }
    fn progress(&self, _: AuthProgress) -> bool { true }
    fn terminal(&self, value: AuthProgress) -> bool {
        if let AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Failed { error, .. }, .. }) = value {
            self.0.borrow_mut().push(error);
        }
        true
    }
}

/// The read-out a profile named `title` gets when every server answers as `servers` say.
fn switch_message(title: &str, servers: &[(&str, u8, Answer)]) -> String {
    let stored = cached_session(None);
    let identity = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: title.into(), ..Default::default() };
    let sink = Failures::default();
    profile_switch_worker_with_io(1, identity, stored, tile, None, false, &sink, &mut ScriptedIo::new(servers));
    let failures = sink.0.into_inner();
    assert_eq!(failures.len(), 1, "exactly one failure read-out: {failures:?}");
    failures.into_iter().next().unwrap()
}

#[test]
fn every_server_untrusted_tells_the_owner_about_the_certificate_not_account_access() {
    let message = switch_message("Kid", &[("s1", 1, Answer::Tls(20)), ("s2", 2, Answer::Tls(20))]);
    assert_eq!(message, msg::browse_auth_tls_untrusted());
    assert_ne!(message, msg::browse_auth_no_source_access("Kid"));
}

#[test]
fn every_server_silent_reuses_the_servers_unreachable_copy() {
    for answer in [Answer::Timeout, Answer::Refused, Answer::Tls(10)] {
        assert_eq!(switch_message("Kid", &[("s1", 1, answer), ("s2", 2, answer)]),
            msg::browse_auth_servers_unreachable());
    }
}

#[test]
fn a_401_is_the_only_thing_that_says_the_profile_has_no_access() {
    assert_eq!(switch_message("Kid", &[("s1", 1, Answer::Unauthorized)]), msg::browse_auth_no_source_access("Kid"));
    assert_eq!(switch_message("Kid", &[("s1", 1, Answer::Unauthorized), ("s2", 2, Answer::Timeout)]),
        msg::browse_auth_no_source_access("Kid"), "a refusal outranks silence");
}

#[test]
fn one_untrusted_server_among_silent_or_refusing_ones_names_the_certificate() {
    assert_eq!(switch_message("Kid", &[("s1", 1, Answer::Timeout), ("s2", 2, Answer::Tls(20))]),
        msg::browse_auth_tls_untrusted());
    assert_eq!(switch_message("Kid", &[("s1", 1, Answer::Unauthorized), ("s2", 2, Answer::Tls(18))]),
        msg::browse_auth_tls_untrusted(), "the actionable cause leads");
}

// ---- the events log -----------------------------------------------------------------------------

/// The lines the events log gained while `run` ran.
fn log_lines_during(run: impl FnOnce()) -> Vec<String> {
    let _serial = plx_base::testlock::serial();
    let log = plx_base::eventlog::events_log();
    let before = std::fs::metadata(&log).map_or(0, |m| m.len() as usize);
    run();
    let all = std::fs::read(&log).unwrap_or_default();
    String::from_utf8_lossy(&all[before.min(all.len())..]).lines().map(str::to_owned).collect()
}

#[test]
fn a_failed_switch_logs_one_closed_line_of_counts_and_verify_codes_and_no_names() {
    let lines = log_lines_during(|| {
        switch_message("Zebra Profile Name", &[("zz-machine-1", 1, Answer::Tls(19)), ("zz-machine-2", 2, Answer::Tls(21)),
            ("zz-machine-3", 3, Answer::Timeout)]);
    });
    let line = "auth: no server verified \u{2014} unreachable=1 tls_untrusted=2 unauthorized=0 verify=[19,21]";
    assert_eq!(lines.iter().filter(|l| l.contains(line)).count(), 1, "one line per failed switch: {lines:#?}");
    for l in lines.iter().filter(|l| l.contains("no server verified") && l.contains("verify=[19,21]")) {
        for private in ["Zebra", "zz-machine", "192.0.2", "plex.direct", "tok-"] {
            assert!(!l.contains(private), "{private} leaked into {l}");
        }
    }
}

#[test]
fn a_failed_discovery_logs_the_same_closed_line() {
    let lines = log_lines_during(|| {
        let mut causes = FailureCauses::default();
        causes.push("zz-disc-1", Cause::TlsUntrusted { verify: 2 });
        causes.push("zz-disc-2", Cause::TlsUntrusted { verify: 18 });
        let verdict = resolved_without_roster(none(false, false, causes), DiscoveryTrigger::Login);
        assert!(matches!(verdict, Err(Discovery::TlsUntrusted { .. })));
    });
    let line = "auth: no server verified \u{2014} unreachable=0 tls_untrusted=2 unauthorized=0 verify=[2,18]";
    assert_eq!(lines.iter().filter(|l| l.contains(line)).count(), 1, "{lines:#?}");
    assert!(lines.iter().all(|l| !l.contains("zz-disc")), "no machine id in the log");
}

// ---- the cached origin must not hide the cause --------------------------------------------------

#[test]
fn a_cached_origin_reprobe_keeps_the_cause_a_fresh_probe_would_have_given() {
    let plan = probe::plan(&server("ours", "Ours", 1), CredentialPolicy::HttpsOnly);
    let cached = source("ours", true, "tok");
    let probe = |answer| cached_probe_result(&server("ours", "Ours", 1), &cached, &[], &plan, reply(answer)).1;
    assert_eq!(probe(Answer::Tls(20)).cause(), Some(Cause::TlsUntrusted { verify: 20 }));
    assert_eq!(probe(Answer::Tls(10)).cause(), Some(Cause::Unreachable));
    assert_eq!(probe(Answer::Timeout).cause(), Some(Cause::Unreachable));
    assert_eq!(probe(Answer::Unauthorized).cause(), Some(Cause::Unauthorized));
    assert_eq!(probe(Answer::Verified("ours")).cause(), None);
}

/// The cached re-probe REPLACES the fresh verdict when the fresh race found nothing; a re-probe that
/// is merely silent must not turn "this TV does not trust the certificate" back into "unreachable".
#[test]
fn the_more_useful_failure_survives_when_a_cached_reprobe_replaces_a_fresh_one() {
    let plan = race_plan();
    let failed = |cause| settled_probe(&plan, Outcome::Unreachable, None, None).with_cause(Some(cause));
    let tls = failed(Cause::TlsUntrusted { verify: 20 });
    assert_eq!(failed(Cause::Unreachable).after_fresh(&tls).cause(), Some(Cause::TlsUntrusted { verify: 20 }));
    assert_eq!(tls.clone().after_fresh(&failed(Cause::Unreachable)).cause(), Some(Cause::TlsUntrusted { verify: 20 }));
    let verified = settled_probe(&plan, Outcome::Reachable, None, None);
    assert_eq!(verified.after_fresh(&tls).cause(), None, "a verified cached origin has no failure to explain");
}
