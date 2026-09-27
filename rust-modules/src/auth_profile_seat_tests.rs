//! Who's-watching profile seating: offline PIN activation from the cached roster, the
//! picker's resume/detach policy, and profile-switch UI banner behavior.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// The roster's uuid keys the record, whatever the `/switch` body says — an empty or
/// differing response uuid must not produce an entry the next pick cannot find.
#[test]
fn a_seated_profile_is_recorded_under_the_roster_uuid() {
    let u = crate::plex::account::SwitchedUser {
        uuid: String::new(),
        ..Default::default()
    };
    assert_eq!(seated_uuid(&u, &tile("u-kid", false)), "u-kid");
    let u = crate::plex::account::SwitchedUser {
        uuid: "u-other".into(),
        ..Default::default()
    };
    assert_eq!(seated_uuid(&u, &tile("u-kid", false)), "u-kid");
    assert_eq!(
        seated_uuid(&u, &tile("", false)),
        "u-other",
        "no roster uuid: the response's"
    );
}

/// The plaintext twin may answer first, but the store POLICY alone cannot make it live: under
/// `HttpsOnly` only an https origin can carry a credential, while a developer build keeps its lab
/// plaintext. A store build's one plaintext exception is a consented grant, and that is not this
/// policy's to give: `plex::grant::credential_allowed` (or `allowed_under`, where the policy is a
/// parameter) is the one answer, and it consults the grant table on top of this rule.
/// Superseded `activation_allowed_by_policy`, deleted with the race-semantics change.
#[test]
fn https_only_policy_alone_never_makes_a_plaintext_origin_live() {
    let plain = Origin::http("192.168.0.10", 32400);
    let tls = Origin::parse("https://192-168-0-10.abc.plex.direct:32400").unwrap();
    assert!(!CredentialPolicy::HttpsOnly.may_carry_credential(&plain));
    assert!(CredentialPolicy::HttpsOnly.may_carry_credential(&tls));
    assert!(
        CredentialPolicy::AllowPlaintext.may_carry_credential(&plain),
        "a developer build keeps its lab server"
    );
}

/// The outage that motivated the cache (2026-09-06): the active profile is the PIN-protected
/// admin, plex.tv is unreachable, and the PIN has to be checked by this television.
#[test]
fn a_protected_profile_is_seated_offline_on_its_pin_and_refused_on_any_other() {
    let stored = cached_session(Some("4821"));
    match offline_activation(&stored, &tile("u-admin", true), Some("4821")) {
        OfflineSwitch::Seat(next) => {
            assert_eq!(next.user.token, "admin-token");
            assert_eq!(
                next.client_id, "cid",
                "the account and its roster ride through"
            );
            assert_eq!(next.account_token, "acct");
        }
        _ => panic!("the right PIN seats the cached profile"),
    }
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), Some("0000")),
        OfflineSwitch::PinDenied
    ));
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), None),
        OfflineSwitch::PinDenied
    ));
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), Some("")),
        OfflineSwitch::PinDenied
    ));
}

/// A protected profile whose record predates the verifier cannot be checked, so it is not
/// seated — "no cache", never "no PIN".
#[test]
fn a_protected_profile_cached_without_a_verifier_is_not_seated() {
    let stored = cached_session(None);
    assert!(matches!(
        offline_activation(&stored, &tile("u-admin", true), Some("4821")),
        OfflineSwitch::NoCache
    ));
}

#[test]
fn an_unprotected_cached_profile_is_seated_on_the_pick_alone() {
    let stored = cached_session(Some("4821"));
    match offline_activation(&stored, &tile("u-kid", false), None) {
        OfflineSwitch::Seat(next) => {
            assert_eq!(next.user.uuid, "u-kid");
            assert_eq!(next.user.token, "kid-token");
            assert_eq!(next.server.token, "kid-token");
            assert_eq!(next.sources[0].token, "kid-token");
            assert_eq!(
                next.profiles.len(),
                2,
                "the cache itself is kept for the next pick"
            );
        }
        _ => panic!("an unprotected cached profile seats without a network"),
    }
}

#[test]
fn a_profile_this_television_never_seated_online_has_nothing_to_seat() {
    let stored = cached_session(Some("4821"));
    assert!(matches!(
        offline_activation(&stored, &tile("u-guest", false), None),
        OfflineSwitch::NoCache
    ));
    assert!(matches!(
        offline_activation(&Session::default(), &tile("u-admin", true), Some("4821")),
        OfflineSwitch::NoCache
    ));
}

/// The seating paths that never see a PIN still write the record for a PIN-free profile,
/// so a session stored before the cache existed becomes seatable offline on first use.
#[test]
fn seating_an_unprotected_active_profile_records_it_and_a_protected_one_is_left_to_the_switch()
{
    let mut s = cached_session(None);
    s.profiles.clear();
    s.home_users = vec![session::HomeUserRef {
        uuid: "u-admin".into(),
        protected: false,
        ..Default::default()
    }];
    remember_unprotected_active(&mut s);
    assert_eq!(s.profiles.len(), 1);
    assert_eq!(
        s.cached_profile("u-admin").unwrap().user.token,
        "admin-token"
    );

    let mut p = cached_session(None);
    p.profiles.clear();
    p.home_users = vec![session::HomeUserRef {
        uuid: "u-admin".into(),
        protected: true,
        ..Default::default()
    }];
    remember_unprotected_active(&mut p);
    assert!(
        p.profiles.is_empty(),
        "no PIN in hand, no verifier to write"
    );

    let mut none = Session::default();
    remember_unprotected_active(&mut none);
    assert!(
        none.profiles.is_empty(),
        "an account without Plex Home names no profile"
    );
}

#[test]
fn picker_policy_defaults_detachment_and_refusal_reasons_remain_exact() {
    for (picker, protected, allowed) in [
        (Picker::Boot, true, false), (Picker::Boot, false, true),
        (Picker::ChangeProfile, true, false), (Picker::ChangeProfile, false, false),
        (Picker::SignedIn, true, false), (Picker::SignedIn, false, false),
    ] { assert_eq!(may_resume(picker, protected), allowed); }
    assert_eq!(Picker::default(), Picker::Boot);
    assert!(detaches_active_profile(Picker::ChangeProfile));
    assert!(!detaches_active_profile(Picker::Boot));
    assert!(!detaches_active_profile(Picker::SignedIn));
    let adult = signed_in_as("u-adult");
    let kid = signed_in_as("u-kid");
    assert!(adult.active_profile_is_protected());
    assert!(!kid.active_profile_is_protected());
    assert_eq!(refusal_reason(Picker::ChangeProfile, &kid),
        "auth: BACK refused — the Change-profile picker is a root");
    assert_eq!(refusal_reason(Picker::Boot, &adult),
        "auth: BACK refused — the stored profile is PIN-protected");
    let unchosen = Session { user: UserRef::default(), ..adult };
    assert_eq!(refusal_reason(Picker::SignedIn, &unchosen),
        "auth: BACK refused — no profile has been chosen on this device yet");
}

/// **A wrong PIN must not follow the user back to the roster.** Reported as a *"strange 'Switch
/// Profile — Check the PIN' element"* appearing on Who's Watching after a rejected PIN.
///
/// It is `switch_thread`'s failure banner. The pad and the roster are two surfaces and only one
/// of them is asking about a PIN: `ui::profiles::draw` paints `auth::error()` under the avatar
/// row whenever the pad is closed, so the moment BACK dismissed the keypad the string the pad
/// had already answered with a red flash reappeared under the faces — blaming a PIN nobody was
/// being asked for any more, on the one screen where every profile is a candidate.
///
/// So a PIN-blaming failure leaves NO roster banner. Everything else keeps one, because the
/// roster is exactly where "no access to this server" or "check the connection" belongs — the
/// pad closes for those (`ui::profiles::update`), and a screen that swallowed the choice with
/// no read-out at all is the failure this banner was added for.
#[test]
fn a_rejected_pin_leaves_no_error_on_the_who_s_watching_roster() {
    let (banner, denied) = switch_failure(true);
    assert!(
        banner.is_empty(),
        "a PIN-blaming failure must leave the roster's error band EMPTY — got {banner:?}"
    );
    assert!(denied, "…and must still flash the pad's dots");

    let (banner, denied) = switch_failure(false);
    assert!(
        !banner.is_empty(),
        "a switch that failed for any other reason still owes the roster a read-out"
    );
    assert!(
        !denied,
        "…and must not flash the pad red, which reads as a typo to retry forever"
    );
}

/// A minimal, entirely local [`ProfileWorkIo`] for the ONLINE switch success path — no
/// network or thread sleep. `switch` always answers
/// with the one seated user the test configures; `resources` always answers with the one server
/// resource matching `stored.server.machine_id` ("ours" in [`cached_session`]); `probe` always
/// reports that server reachable, winning it a [`SourceRef`] built the same way the fixtures
/// build one.
struct OnlineSwitchIo {
    seated: crate::plex::account::SwitchedUser,
    resource_token: String,
}
impl ProfileWorkIo for OnlineSwitchIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            id: self.seated.id,
            uuid: self.seated.uuid.clone(),
            title: self.seated.title.clone(),
            auth_token: self.seated.auth_token.clone(),
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![Resource {
            name: "ours".into(),
            client_identifier: "ours".into(),
            provides: "server".into(),
            owned: true,
            access_token: self.resource_token.clone(),
            ..Default::default()
        }])
    }
    fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        let plan = probe::plan(resource, CredentialPolicy::build());
        let winner = source(&resource.client_identifier, resource.owned, &self.resource_token);
        let address = Some(winner.address.clone());
        (Some(winner), settled_probe(&plan, Outcome::Reachable, None, address))
    }
    fn admit(&mut self, _: &SourceRef, _: &str) -> crate::plex::EndpointAdmission {
        crate::plex::EndpointAdmission::Usable
    }
    fn gap(&mut self) {}
}

/// A captured-in-place [`owner::ObservationSink`] — no worker/adapter plumbing, since this
/// finding only needs the single [`AuthProgress`] the online switch emits on success.
#[derive(Default)]
struct CapturingSink(std::cell::RefCell<Vec<AuthProgress>>);
impl owner::ObservationSink for CapturingSink {
    fn live(&self) -> bool { true }
    fn progress(&self, value: AuthProgress) -> bool {
        self.0.borrow_mut().push(value);
        true
    }
    fn terminal(&self, value: AuthProgress) -> bool {
        self.0.borrow_mut().push(value);
        true
    }
}

struct FailedFreshProbeIo;
impl ProfileWorkIo for FailedFreshProbeIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![Resource { name: "ours".into(), client_identifier: "ours".into(),
            provides: "server".into(), owned: true, access_token: "fresh-kid-token".into(),
            ..Default::default() }])
    }
    fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        let plan = probe::plan(resource, CredentialPolicy::build());
        (None, settled_probe(&plan, Outcome::Unreachable, None, None))
    }
    fn admit(&mut self, _: &SourceRef, _: &str) -> crate::plex::EndpointAdmission {
        panic!("a failed identity probe has no endpoint to authenticate")
    }
    fn gap(&mut self) {}
}

#[test]
fn cached_dialable_address_with_a_failed_fresh_probe_never_reports_ready() {
    let stored = cached_session(None);
    let expected = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
    let sink = CapturingSink::default();
    profile_switch_worker_with_io(1, expected, stored, tile, None, false, &sink,
        &mut FailedFreshProbeIo);
    let events = sink.0.into_inner();
    assert!(events.iter().any(|event| matches!(event,
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Failed { .. }, .. }))));
    assert!(!events.iter().any(|event| matches!(event,
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Ready { .. }, .. }))));
}

struct ScriptedAdmissionIo {
    servers: Vec<(&'static str, &'static str)>,
    admissions: std::collections::VecDeque<crate::plex::EndpointAdmission>,
    checked: Vec<(String, String)>,
}
impl ProfileWorkIo for ScriptedAdmissionIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(self.servers.iter().enumerate().map(|(index, (machine_id, token))| Resource {
            name: format!("Server {}", index + 1), client_identifier: (*machine_id).into(),
            provides: "server".into(), owned: index == 0, access_token: (*token).into(),
            ..Default::default()
        }).collect())
    }
    fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        let plan = probe::plan(resource, CredentialPolicy::build());
        let mut winner = source(&resource.client_identifier, resource.owned, &resource.access_token);
        let index = self.servers.iter().position(|(machine_id, _)|
            *machine_id == resource.client_identifier).unwrap();
        winner.address = format!("10.0.0.{}", index + 10);
        winner.origin_url = format!("https://10-0-0-{}.example.plex.direct:32400", index + 10);
        let address = Some(winner.address.clone());
        (Some(winner), settled_probe(&plan, Outcome::Reachable,
            Some(probe::Location::Local), address))
    }
    fn admit(&mut self, source: &SourceRef, _: &str) -> crate::plex::EndpointAdmission {
        self.checked.push((source.machine_id.clone(), source.token.clone()));
        self.admissions.pop_front().expect("one admission result per reached source")
    }
    fn gap(&mut self) {}
}

fn run_scripted_admission(io: &mut ScriptedAdmissionIo) -> Vec<AuthProgress> {
    let stored = cached_session(None);
    let expected = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
    let sink = CapturingSink::default();
    profile_switch_worker_with_io(1, expected, stored, tile, None, false, &sink, io);
    sink.0.into_inner()
}

fn switch_failure_event(events: &[AuthProgress]) -> Option<&str> {
    events.iter().find_map(|event| match event {
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Failed { error, .. }, ..
        }) => Some(error.as_str()), _ => None,
    })
}

fn ready_primary(events: &[AuthProgress]) -> Option<&ServerRef> {
    events.iter().find_map(|event| match event {
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Ready { delta, .. }, ..
        }) => Some(&delta.server), _ => None,
    })
}

#[test]
fn identity_ok_but_authenticated_401_fails_with_server_refusal_wording_and_profile_token() {
    let mut io = ScriptedAdmissionIo { servers: vec![("ours", "fresh-kid-token")],
        admissions: [crate::plex::EndpointAdmission::Refused(401)].into(), checked: Vec::new() };
    let events = run_scripted_admission(&mut io);
    let error = switch_failure_event(&events).expect("a refused profile token fails the switch");
    assert!(error.contains("refused Kid"));
    assert_eq!(io.checked, vec![("ours".into(), "fresh-kid-token".into())]);
}

#[test]
fn authenticated_sections_timeout_fails_instead_of_reporting_ready() {
    let mut io = ScriptedAdmissionIo { servers: vec![("ours", "fresh-kid-token")],
        admissions: [crate::plex::EndpointAdmission::Timeout].into(), checked: Vec::new() };
    let events = run_scripted_admission(&mut io);
    assert_eq!(switch_failure_event(&events), Some("Couldn't switch profile — check the connection."));
    assert!(ready_primary(&events).is_none());
}

#[test]
fn malformed_authenticated_sections_body_fails_instead_of_reporting_ready() {
    for body in [b"not json".as_slice(), b"{}".as_slice(), b"{\"MediaContainer\":null}".as_slice(),
        br#"{"error":"unauthorized"}"#.as_slice()] {
        assert_eq!(crate::plex::endpoint_admission_from_reply(200, body),
            crate::plex::EndpointAdmission::Malformed, "body={}", String::from_utf8_lossy(body));
    }
    let malformed = crate::plex::EndpointAdmission::Malformed;
    let mut io = ScriptedAdmissionIo { servers: vec![("ours", "fresh-kid-token")],
        admissions: [malformed].into(), checked: Vec::new() };
    let events = run_scripted_admission(&mut io);
    assert_eq!(switch_failure_event(&events),
        Some("Couldn't switch profile — the server sent an invalid response."));
    assert!(ready_primary(&events).is_none());
}

#[test]
fn empty_valid_authenticated_sections_body_reports_ready() {
    let empty = crate::plex::endpoint_admission_from_reply(200,
        br#"{"MediaContainer":{"size":0,"Directory":[]}}"#);
    assert_eq!(empty, crate::plex::EndpointAdmission::Usable);
    let mut io = ScriptedAdmissionIo { servers: vec![("ours", "fresh-kid-token")],
        admissions: [empty].into(), checked: Vec::new() };
    let events = run_scripted_admission(&mut io);
    assert_eq!(ready_primary(&events).map(|server| server.machine_id.as_str()), Some("ours"));
    assert_eq!(io.checked, vec![("ours".into(), "fresh-kid-token".into())]);
}

struct MultiEndpointAdmissionIo {
    admissions: std::collections::VecDeque<crate::plex::EndpointAdmission>,
    checked: Vec<(String, String)>,
}
impl ProfileWorkIo for MultiEndpointAdmissionIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![Resource { name: "Server".into(), client_identifier: "ours".into(),
            provides: "server".into(), owned: true, access_token: "fresh-kid-token".into(),
            ..Default::default() }])
    }
    fn probe(&mut self, resource: &Resource, household: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        self.probe_after(resource, household, &[])
    }
    fn probe_after(&mut self, resource: &Resource, _: &[i64], rejected: &[String])
        -> (Option<SourceRef>, SettledProbe) {
        let plan = probe::plan(resource, CredentialPolicy::build());
        let octet = 10 + rejected.len();
        let mut winner = source(&resource.client_identifier, true, &resource.access_token);
        winner.address = format!("10.0.0.{octet}");
        winner.origin_url = format!("https://10-0-0-{octet}.example.plex.direct:32400");
        let address = Some(winner.address.clone());
        (Some(winner), settled_probe(&plan, Outcome::Reachable,
            Some(probe::Location::Local), address))
    }
    fn admit(&mut self, source: &SourceRef, _: &str) -> crate::plex::EndpointAdmission {
        self.checked.push((source.origin_url.clone(), source.token.clone()));
        self.admissions.pop_front().unwrap()
    }
    fn gap(&mut self) {}
}

#[test]
fn refused_first_endpoint_then_authenticated_second_reports_ready_on_second() {
    let mut io = MultiEndpointAdmissionIo { admissions: [
        crate::plex::EndpointAdmission::Refused(403), crate::plex::EndpointAdmission::Usable,
    ].into(), checked: Vec::new() };
    let stored = cached_session(None);
    let expected = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
    let sink = CapturingSink::default();
    profile_switch_worker_with_io(1, expected, stored, tile, None, false, &sink, &mut io);
    let events = sink.0.into_inner();
    let ready = ready_primary(&events).expect("the second authenticated endpoint seats the profile");
    assert_eq!(ready.origin_url, "https://10-0-0-11.example.plex.direct:32400");
    assert_eq!(io.checked, vec![
        ("https://10-0-0-10.example.plex.direct:32400".into(), "fresh-kid-token".into()),
        ("https://10-0-0-11.example.plex.direct:32400".into(), "fresh-kid-token".into()),
    ]);
}

struct ExpiredBudgetIo { probes: usize, admissions: usize }
impl ProfileWorkIo for ExpiredBudgetIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![Resource { name: "Server".into(), client_identifier: "ours".into(),
            provides: "server".into(), owned: true, access_token: "fresh-kid-token".into(),
            ..Default::default() }])
    }
    fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        self.probes += 1;
        let plan = probe::plan(resource, CredentialPolicy::build());
        let winner = source("ours", true, &resource.access_token);
        (Some(winner.clone()), settled_probe(&plan, Outcome::Reachable,
            Some(probe::Location::Local), Some(winner.address)))
    }
    fn admit_until(&mut self, _: &SourceRef, _: &str, _: Instant)
        -> crate::plex::EndpointAdmission {
        self.admissions += 1;
        crate::plex::EndpointAdmission::Usable
    }
    fn admission_budget(&self) -> Duration { Duration::ZERO }
    fn gap(&mut self) {}
}

#[test]
fn exhausted_switch_admission_budget_fails_with_timeout_evidence_before_another_attempt() {
    let stored = cached_session(None);
    let sink = CapturingSink::default();
    let mut io = ExpiredBudgetIo { probes: 0, admissions: 0 };
    profile_switch_worker_with_io(1, SessionIdentity::of(&stored), stored,
        UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() },
        None, false, &sink, &mut io);
    let events = sink.0.into_inner();
    assert_eq!(io.probes, 1, "identity probing precedes the admission budget");
    assert_eq!(io.admissions, 0, "an exhausted overall budget starts no authenticated request");
    assert_eq!(switch_failure_event(&events),
        Some("Couldn't switch profile — check the connection."));
}

struct StalledLosingProbeIo {
    probe_started: Option<Instant>,
    admission_deadline: Option<Instant>,
}
impl ProfileWorkIo for StalledLosingProbeIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![Resource { name: "A".into(), client_identifier: "ours".into(),
            provides: "server".into(), owned: true, access_token: "fresh-kid-token".into(),
            ..Default::default() }])
    }
    fn probe(&mut self, _: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        panic!("the deadline-aware probe seam must be used")
    }
    fn probe_after(&mut self, resource: &Resource, _: &[i64], _: &[String])
        -> (Option<SourceRef>, SettledProbe) {
        // Model `race_batch` returning the LAN winner only after its losing remote worker settles.
        self.probe_started = Some(Instant::now());
        std::thread::sleep(Duration::from_millis(5));
        let plan = probe::plan(resource, CredentialPolicy::build());
        let winner = source("ours", true, &resource.access_token);
        (Some(winner.clone()), settled_probe(&plan, Outcome::Reachable,
            Some(probe::Location::Local), Some(winner.address)))
    }
    fn admit_until(&mut self, _: &SourceRef, _: &str, deadline: Instant)
        -> crate::plex::EndpointAdmission {
        self.admission_deadline = Some(deadline);
        if self.probe_started.is_some_and(|started| deadline > started + Duration::from_millis(95)) {
            crate::plex::EndpointAdmission::Usable
        } else {
            crate::plex::EndpointAdmission::Timeout
        }
    }
    fn admission_budget(&self) -> Duration { Duration::from_millis(100) }
    fn gap(&mut self) {}
}

#[test]
fn stalled_losing_remote_probe_does_not_consume_the_healthy_lan_winner_admission_budget() {
    let stored = cached_session(None);
    let sink = CapturingSink::default();
    let mut io = StalledLosingProbeIo { probe_started: None, admission_deadline: None };
    profile_switch_worker_with_io(1, SessionIdentity::of(&stored), stored,
        UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() },
        None, false, &sink, &mut io);
    let events = sink.0.into_inner();
    assert!(ready_primary(&events).is_some(),
        "the reached LAN endpoint must receive a fresh authenticated-admission budget");
    assert!(io.admission_deadline > io.probe_started,
        "authenticated admission starts after the identity race has produced a winner");
}

struct LaterProbeBudgetIo {
    admissions: Vec<String>,
}
impl ProfileWorkIo for LaterProbeBudgetIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![
            Resource { name: "A".into(), client_identifier: "a".into(), provides: "server".into(),
                owned: true, access_token: "a-token".into(), ..Default::default() },
            Resource { name: "B".into(), client_identifier: "b".into(), provides: "server".into(),
                access_token: "b-token".into(), ..Default::default() },
        ])
    }
    fn probe(&mut self, _: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        panic!("the retry-aware probe seam must be used")
    }
    fn probe_after(&mut self, resource: &Resource, _: &[i64], _: &[String])
        -> (Option<SourceRef>, SettledProbe) {
        let plan = probe::plan(resource, CredentialPolicy::build());
        if resource.client_identifier == "b" {
            std::thread::sleep(Duration::from_millis(35));
            return (None, settled_probe(&plan, Outcome::Unreachable, None, None));
        }
        let winner = source("a", true, &resource.access_token);
        (Some(winner.clone()), settled_probe(&plan, Outcome::Reachable,
            Some(probe::Location::Remote), Some(winner.address)))
    }
    fn probe_cached(&mut self, resource: &Resource, cached: &SourceRef, _: &[i64], _: &[String])
        -> Option<(Option<SourceRef>, SettledProbe)> {
        if resource.client_identifier != "b" { return None; }
        std::thread::sleep(Duration::from_millis(35));
        let plan = probe::plan(resource, CredentialPolicy::build());
        let mut winner = cached.clone();
        winner.token = resource.access_token.clone();
        Some((Some(winner.clone()), settled_probe(&plan, Outcome::Reachable,
            Some(probe::Location::Relay), Some(winner.address))))
    }
    fn admit_until(&mut self, source: &SourceRef, _: &str, _: Instant)
        -> crate::plex::EndpointAdmission {
        self.admissions.push(source.machine_id.clone());
        if source.machine_id == "a" {
            std::thread::sleep(Duration::from_millis(45));
            crate::plex::EndpointAdmission::Timeout
        } else {
            crate::plex::EndpointAdmission::Usable
        }
    }
    fn admission_budget(&self) -> Duration { Duration::from_millis(100) }
    fn gap(&mut self) {}
}

#[test]
fn later_direct_and_cached_identity_probes_do_not_consume_the_admission_budget() {
    let mut stored = cached_session(None);
    stored.sources.push(source("b", false, "old-b-token"));
    let sink = CapturingSink::default();
    let mut io = LaterProbeBudgetIo { admissions: Vec::new() };
    profile_switch_worker_with_io(1, SessionIdentity::of(&stored), stored,
        UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() },
        None, false, &sink, &mut io);
    let events = sink.0.into_inner();
    assert_eq!(io.admissions, ["a", "b"],
        "only authenticated request time may consume the cross-server admission budget");
    assert_eq!(ready_primary(&events).map(|server| server.machine_id.as_str()), Some("b"));
}

struct DiesDuringSecondaryIo {
    live: std::rc::Rc<std::cell::Cell<bool>>,
    probes: Vec<String>,
}
impl ProfileWorkIo for DiesDuringSecondaryIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![
            Resource { name: "A".into(), client_identifier: "a".into(), provides: "server".into(),
                owned: true, access_token: "a-token".into(), ..Default::default() },
            Resource { name: "B".into(), client_identifier: "b".into(), provides: "server".into(),
                access_token: "b-token".into(), ..Default::default() },
        ])
    }
    fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        self.probes.push(resource.client_identifier.clone());
        if resource.client_identifier == "b" {
            self.live.set(false);
        }
        let plan = probe::plan(resource, CredentialPolicy::build());
        let winner = source(&resource.client_identifier, resource.owned, &resource.access_token);
        (Some(winner.clone()), settled_probe(&plan, Outcome::Reachable, None,
            Some(winner.address)))
    }
    fn admit(&mut self, source: &SourceRef, _: &str) -> crate::plex::EndpointAdmission {
        assert_eq!(source.machine_id, "a", "secondary servers are not authenticated for liveness");
        crate::plex::EndpointAdmission::Usable
    }
    fn gap(&mut self) {}
}

struct LivenessSink {
    live: std::rc::Rc<std::cell::Cell<bool>>,
    events: std::cell::RefCell<Vec<AuthProgress>>,
}
impl owner::ObservationSink for LivenessSink {
    fn live(&self) -> bool { self.live.get() }
    fn progress(&self, value: AuthProgress) -> bool {
        self.events.borrow_mut().push(value);
        self.live.get()
    }
    fn terminal(&self, value: AuthProgress) -> bool {
        self.events.borrow_mut().push(value);
        self.live.get()
    }
}

#[test]
fn dead_output_stops_after_the_post_ready_secondary_identity_probe() {
    let live = std::rc::Rc::new(std::cell::Cell::new(true));
    let sink = LivenessSink { live: live.clone(), events: Default::default() };
    let mut io = DiesDuringSecondaryIo { live, probes: Vec::new() };
    let stored = cached_session(None);
    profile_switch_worker_with_io(1, SessionIdentity::of(&stored), stored,
        UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() },
        None, false, &sink, &mut io);
    assert_eq!(io.probes, ["a", "b"], "no retry may start after output cancellation");
    assert!(sink.events.into_inner().iter().any(|event| matches!(event,
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Ready { .. }, .. }))));
}

struct UnavailableSecondaryIo;
impl ProfileWorkIo for UnavailableSecondaryIo {
    fn switch(&mut self, _: &AccountClient, _: &str, _: Option<&str>) -> SwitchOutcome {
        SwitchOutcome::Switched(crate::plex::account::SwitchedUser {
            uuid: "u-kid".into(), title: "Kid".into(), auth_token: "kid-account-token".into(),
            ..Default::default()
        })
    }
    fn resources(&mut self, _: &AccountClient) -> Result<Vec<Resource>, crate::plex::account::CallEvidence> {
        Ok(vec![
            Resource { name: "A".into(), client_identifier: "ours".into(), provides: "server".into(),
                owned: true, access_token: "fresh-a-token".into(), ..Default::default() },
            Resource { name: "B".into(), client_identifier: "b".into(), provides: "server".into(),
                access_token: "fresh-b-token".into(), ..Default::default() },
        ])
    }
    fn probe(&mut self, resource: &Resource, _: &[i64]) -> (Option<SourceRef>, SettledProbe) {
        let plan = probe::plan(resource, CredentialPolicy::build());
        if resource.client_identifier == "b" {
            (None, settled_probe(&plan, Outcome::Unreachable, None, None))
        } else {
            let winner = source("ours", true, &resource.access_token);
            (Some(winner.clone()), settled_probe(&plan, Outcome::Reachable, None,
                Some(winner.address)))
        }
    }
    fn gap(&mut self) {}
}

#[test]
fn unavailable_secondary_grant_stays_live_and_in_the_saved_profile() {
    let mut stored = cached_session(None);
    stored.sources.push(source("b", false, "old-b-token"));
    let sink = CapturingSink::default();
    profile_switch_worker_with_io(1, SessionIdentity::of(&stored), stored,
        UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() },
        None, false, &sink, &mut UnavailableSecondaryIo);
    let events = sink.0.into_inner();
    let delta = events.iter().find_map(|event| match event {
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Ready { delta, .. }, ..
        }) => Some(delta), _ => None,
    }).expect("A is freshly admitted");
    let cached = delta.cache.as_ref().expect("the profile is saved");
    assert_eq!(cached.sources.iter().find(|source| source.machine_id == "b").unwrap().token,
        "fresh-b-token");
    assert_eq!(delta.sources.iter().find(|source| source.machine_id == "b").unwrap().token,
        "fresh-b-token", "authenticated admission selects the primary, not secondary liveness");
    assert_eq!(delta.server.machine_id, "ours");
}

/// Copilot review on PR #105, finding 3: the online profile-switch success path built its
/// `ProfileCreds` with `extensions: Default::default()`, unconditionally discarding whatever
/// extensions a PRIOR seating of the same uuid had cached — `Session::remember_profile` then
/// replaces that uuid's whole cache entry with the impoverished one. `cached_session`'s `u-kid`
/// entry starts with empty extensions like every other test fixture; this test gives it a
/// real one first, switches to `u-kid` online, and asserts the delta's cache keeps it.
#[test]
fn online_profile_switch_preserves_the_uuids_existing_cached_extensions() {
    let mut stored = cached_session(None);
    let mut previous = stored
        .profiles
        .iter()
        .find(|p| p.uuid == "u-kid")
        .cloned()
        .expect("cached_session seeds a u-kid profile");
    previous.extensions = session::OpaqueExtensions(std::collections::BTreeMap::from([(
        "futureField".to_string(),
        serde_json::json!("kept from an earlier build"),
    )]));
    stored.remember_profile(previous.clone());

    let expected = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
    let mut io = OnlineSwitchIo {
        seated: crate::plex::account::SwitchedUser {
            id: 0,
            uuid: "u-kid".into(),
            title: "Kid".into(),
            auth_token: "fresh-kid-token".into(),
        },
        resource_token: "fresh-kid-token".into(),
    };
    let sink = CapturingSink::default();
    profile_switch_worker_with_io(1, expected, stored, tile, None, false, &sink, &mut io);

    let events = sink.0.into_inner();
    let ready = events.iter().find_map(|event| match event {
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Ready { delta, .. }, ..
        }) => Some(delta),
        _ => None,
    });
    let delta = ready.expect("expected a Ready online-switch outcome among the emitted events");
    let cache = delta.cache.as_ref().expect("an online switch always caches credentials");
    assert_eq!(
        cache.extensions.0.get("futureField"),
        previous.extensions.0.get("futureField"),
        "the online switch dropped the uuid's previously-cached extensions instead of \
         carrying them forward"
    );
}

/// The three credentials in a profile switch belong to three different authorities. The
/// account owner authorizes `/switch`, that response authorizes plex.tv as the managed user,
/// and `/resources` supplies the managed user's per-server PMS credential.
#[test]
fn online_profile_switch_keeps_managed_plex_tv_and_pms_tokens_separate() {
    let mut stored = cached_session(None);
    stored.account_token = "owner-account-token".into();
    let expected = SessionIdentity::of(&stored);
    let tile = UserTile { uuid: "u-kid".into(), title: "Kid".into(), ..Default::default() };
    let mut io = OnlineSwitchIo {
        seated: crate::plex::account::SwitchedUser {
            id: 27, uuid: "u-kid".into(), title: "Kid".into(),
            auth_token: "managed-plex-tv-token".into(),
        },
        resource_token: "managed-pms-token".into(),
    };
    let sink = CapturingSink::default();
    profile_switch_worker_with_io(1, expected, stored, tile, None, false, &sink, &mut io);

    let events = sink.0.into_inner();
    let delta = events.iter().find_map(|event| match event {
        AuthProgress::ProfileSwitch(ProfileSwitchProgress {
            outcome: ProfileSwitchOutcomeProgress::Ready { delta, .. }, ..
        }) => Some(delta),
        _ => None,
    }).expect("expected a Ready online-switch outcome");
    assert_eq!(delta.user.token, "managed-pms-token");
    assert_eq!(delta.user.plex_tv_token.as_deref(), Some("managed-plex-tv-token"));
    let cached = delta.cache.as_ref().expect("online switch caches both credentials");
    assert_eq!(cached.user.token, "managed-pms-token");
    assert_eq!(cached.user.plex_tv_token.as_deref(), Some("managed-plex-tv-token"));
}
