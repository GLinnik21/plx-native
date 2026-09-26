//! **May a credential go to this origin — the ONE answer, and the plaintext grants behind it.**
//!
//! Every layer that puts a Plex token on a request asks [`credential_allowed`] (or, in a pure
//! function that receives the build's policy as a parameter, [`allowed_under`]): the registry
//! when it admits an origin, endpoint admission, the control-plane transport (`crate::http`), the
//! media plane (`crate::curlio`, `crate::stream_redirect`'s plaintext hops, the player's stream
//! start). Nothing else re-derives the rule, and nothing asks `Origin::is_tls` for a credential
//! decision on its own.
//!
//! ## The rule
//!
//! A TLS origin may always carry a credential. A plaintext origin may when the BUILD allows it
//! ([`CredentialPolicy::AllowPlaintext`], developer builds only — [`CredentialPolicy::build`] is
//! still the only `cfg!` for this), or when a live [`PlaintextGrant`] names that exact origin. A
//! store build therefore sends a token over plaintext to exactly the origins the person consented
//! to, on the network and under the identity they consented on — and to nothing else.
//!
//! ## What a grant is bound to
//!
//! A grant is minted only by discovery ([`mint`]), from a FRESH verdict — plex.tv's resources and
//! a tokenless `/identity` answer — that [`InsecureEvidence::plaintext_eligibility`] calls
//! eligible, and only when the person's recorded consent for that (account, server) allows it.
//! It names:
//!
//! * the server's `machineIdentifier`;
//! * the exact plaintext origin — scheme, NUMERIC private host and port — that verified; a name is
//!   never granted, because a resolver could answer it with anything;
//! * the **identity generation** it was minted under ([`identity_changed`]: sign-out, sign-in, a
//!   profile switch starting);
//! * the **network generation** it was minted under ([`network_changed`]: the app returning to the
//!   foreground, where nothing proves the television is still on the network it consented on).
//!
//! A grant whose generations are not the current ones is dead: it admits nothing, and the table
//! drops it. Nothing about a grant is persisted — a session file that names a plaintext origin
//! reactivates nothing by itself (`plex::session::SourceRef`); the next discovery re-proves
//! eligibility and mints a fresh one, or the origin stays tokenless.
//!
//! ## Revocation
//!
//! [`revoke`], [`identity_changed`] and [`network_changed`] take effect at once: the table no
//! longer admits the origin, so every request from then on — queued, retried, a cached
//! token-bearing URL, a media reopen — is refused at the transport; and the registry re-grades
//! every published client ([`super::servers::regrade_credentials`]), blanking the token of any
//! whose origin may no longer carry one.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use super::origin::{CredentialPolicy, Origin, Scheme};
use super::probe::{AddressScope, InsecureEvidence, PlaintextEligibility};
use super::session::PlaintextChoice;

/// The generations a grant is bound to — captured by a discovery worker when it starts, so a grant
/// it mints after the identity or the network moved is dead on arrival.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GrantScope {
    identity: u64,
    network: u64,
}

/// One consented plaintext origin. Never persisted; see the module doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaintextGrant {
    scope: GrantScope,
    machine_id: String,
    origin: Origin,
}

static GRANTS: Mutex<Vec<PlaintextGrant>> = Mutex::new(Vec::new());
/// How many grants the table holds — the lock-free fast path: a household that never consented
/// (every household but the few this exists for) answers every request with one relaxed load.
static COUNT: AtomicUsize = AtomicUsize::new(0);
static IDENTITY: AtomicU64 = AtomicU64::new(1);
static NETWORK: AtomicU64 = AtomicU64::new(1);
/// Answers given THIS launch, under the current identity, overlaying the persisted ones
/// (`Session::plaintext_consent`) — so a *Connect* is honoured by the retry it triggers even
/// before (or without) the session write landing. Cleared with the identity; never a grant.
static ANSWERS: Mutex<Vec<(String, PlaintextChoice)>> = Mutex::new(Vec::new());

/// The generations a grant minted NOW would be bound to.
pub(crate) fn scope() -> GrantScope {
    GrantScope {
        identity: IDENTITY.load(Ordering::Acquire),
        network: NETWORK.load(Ordering::Acquire),
    }
}

/// **May a credential go to `origin` — the live answer**, under the build's own policy.
pub fn credential_allowed(origin: &Origin) -> bool {
    allowed_under(CredentialPolicy::build(), origin)
}

/// [`credential_allowed`] with the policy supplied — for the pure functions that receive the
/// build's policy as a parameter, and their tests. The grant half is always the live table.
pub(crate) fn allowed_under(policy: CredentialPolicy, origin: &Origin) -> bool {
    policy.may_carry_credential(origin) || (!origin.is_tls() && granted(origin))
}

/// Does `origin`'s credential rest on a grant — `policy` alone refuses it and a live grant admits
/// it? The registry records this per slot so a revocation re-grades exactly the slots a grant was
/// carrying, whatever the build's own policy.
pub(crate) fn rests_on_grant(policy: CredentialPolicy, origin: &Origin) -> bool {
    !policy.may_carry_credential(origin) && granted_now(origin)
}

/// Is `origin` admitted by a live grant right now (the grant half of [`allowed_under`] alone)?
pub(crate) fn granted_now(origin: &Origin) -> bool {
    !origin.is_tls() && granted(origin)
}

fn granted(origin: &Origin) -> bool {
    if COUNT.load(Ordering::Acquire) == 0 {
        return false;
    }
    let grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    admits(&grants, scope(), origin)
}

/// Pure: does any grant in `grants` admit `origin` at `now`? Only a grant minted under the current
/// generations counts, and only for its exact origin — scheme, host and port.
fn admits(grants: &[PlaintextGrant], now: GrantScope, origin: &Origin) -> bool {
    grants.iter().any(|g| g.scope == now && g.origin == *origin)
}

/// Why [`mint`] refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MintRefusal {
    /// The verdict is not eligible, with the rule's own reason.
    Ineligible(PlaintextEligibility),
    /// The origin is not plaintext — TLS needs no grant, and must never be recorded as one.
    NotPlaintext,
    /// The origin's host is not the numeric private literal the verdict classified.
    NotPrivateLiteral,
    /// The identity or the network moved since the minting worker started.
    Stale,
    /// The person has not allowed plaintext for this server (undecided, declined or revoked).
    NotConsented,
}

/// Pure: may a grant for `origin` be minted from `evidence` at `scope`, when the current
/// generations are `now`? Every condition of the eligibility rule, plus the ones only the origin
/// itself can answer: it is plaintext, and its host is a numeric private literal of the scope the
/// verdict recorded.
fn mint_check(
    origin: &Origin,
    evidence: &InsecureEvidence,
    scope: GrantScope,
    now: GrantScope,
) -> Result<(), MintRefusal> {
    if origin.scheme() != Scheme::Http {
        return Err(MintRefusal::NotPlaintext);
    }
    let (host_scope, _) = AddressScope::of(origin.host());
    if !host_scope.is_private_network() || host_scope != evidence.plaintext_scope {
        return Err(MintRefusal::NotPrivateLiteral);
    }
    match evidence.plaintext_eligibility() {
        PlaintextEligibility::Eligible => {}
        reason => return Err(MintRefusal::Ineligible(reason)),
    }
    if scope != now {
        return Err(MintRefusal::Stale);
    }
    Ok(())
}

/// **Mint a grant** for `machine_id` at `origin`, from a fresh eligible verdict, at the `scope`
/// the minting worker captured when it started. The caller has already established the person's
/// consent; this function establishes everything else. A server holds at most one grant — a new
/// one replaces its predecessor, so a server that moved on the LAN does not keep its old address
/// credentialed.
pub(crate) fn mint(
    scope: GrantScope,
    machine_id: &str,
    origin: &Origin,
    evidence: &InsecureEvidence,
) -> Result<(), MintRefusal> {
    let mut grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    mint_check(origin, evidence, scope, self::scope())?;
    let fresh = PlaintextGrant { scope, machine_id: machine_id.to_owned(), origin: origin.clone() };
    let already = grants.contains(&fresh);
    let before = grants.len();
    grants.retain(|g| g.machine_id != machine_id && g.scope == scope);
    let replaced = grants.len() != before && !already;
    grants.push(fresh);
    COUNT.store(grants.len(), Ordering::Release);
    drop(grants);
    if replaced {
        // The server's previous origin (it moved on the LAN) may be published with a token.
        super::servers::regrade_credentials();
    }
    if !already {
        // The authority only — never the machine id (a household fingerprint) nor the token.
        crate::log(&format!(
            "security: consented plaintext credentials for one server at {}",
            origin.log_form()
        ));
    }
    Ok(())
}

/// The plaintext origin `machine_id` is credentialed at right now, if any — what Settings shows as
/// "Not encrypted", and what the upgrade retry is trying to leave.
pub(crate) fn granted_origin(machine_id: &str) -> Option<Origin> {
    if COUNT.load(Ordering::Acquire) == 0 {
        return None;
    }
    let now = scope();
    let grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    grants.iter().find(|g| g.scope == now && g.machine_id == machine_id).map(|g| g.origin.clone())
}

/// The machines holding a live grant — the servers the upgrade retry is watching.
pub(crate) fn granted_machines() -> Vec<String> {
    if COUNT.load(Ordering::Acquire) == 0 {
        return Vec::new();
    }
    let now = scope();
    let grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    grants.iter().filter(|g| g.scope == now).map(|g| g.machine_id.clone()).collect()
}

/// **Revoke `machine_id`'s grant now** — consent withdrawn in Settings, or the server verified
/// over HTTPS and needs plaintext no more. Returns whether there was one.
pub(crate) fn revoke(machine_id: &str) -> bool {
    let removed = retain(|g| g.machine_id != machine_id);
    if removed {
        crate::log("security: plaintext credentials withdrawn for one server");
    }
    removed
}

/// The signed-in identity changed (sign-out, a sign-in or a profile switch starting): every grant
/// is dead from this instant.
pub(crate) fn identity_changed() {
    IDENTITY.fetch_add(1, Ordering::AcqRel);
    ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    if retain(|_| false) {
        crate::log("security: plaintext credentials withdrawn — identity changed");
    }
}

/// The television may be on a different network (the app returned to the foreground): every grant
/// is dead from this instant, and the next discovery re-proves the network before minting again.
pub(crate) fn network_changed() {
    NETWORK.fetch_add(1, Ordering::AcqRel);
    if retain(|_| false) {
        crate::log("security: plaintext credentials withdrawn — network continuity unknown");
    }
}

/// **Record the person's answer for `machine_id`** for the rest of this launch (the caller persists
/// it too, through `plex::session`). Anything but `Allowed` withdraws the server's grant at once —
/// Settings' switch and the question's *Not now* are both revocation.
pub(crate) fn answer(machine_id: &str, choice: PlaintextChoice) {
    {
        let mut answers = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
        answers.retain(|(m, _)| m != machine_id);
        answers.push((machine_id.to_owned(), choice));
    }
    if !choice.allows() {
        revoke(machine_id);
    }
}

/// **Record the person's answer everywhere it lives**: [`answer`] for this launch (withdrawing the
/// grant at once unless it allows), and the session file for the next — the ticket is the
/// preferences write, for a screen that shows the choice optimistically until it lands. The login
/// read-out's question and Settings' switch both come through here.
pub(crate) fn record(
    machine_id: &str,
    choice: PlaintextChoice,
) -> Result<crate::storage_worker::TypedTicket<bool>, crate::storage_worker::SubmitError> {
    answer(machine_id, choice);
    let machine = machine_id.to_owned();
    super::session::queue_update_ticket(move |current| {
        (current.plaintext_choice(&machine) != choice)
            .then(|| current.with_plaintext_choice(&machine, choice))
    })
}

/// **What one discovery run may do with an eligible plaintext answer** — the generations and the
/// person's answers, captured at the worker's spawn site (`crate::plex`'s "capture at the spawn
/// site" rule): a worker never reads the session file mid-probe, and a grant it mints after the
/// identity or the network moved is refused as stale.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PlaintextAsk {
    scope: GrantScope,
    choices: Vec<(String, PlaintextChoice)>,
}

impl PlaintextAsk {
    /// The live capture: the persisted answers, overlaid by this launch's.
    pub(crate) fn capture() -> Self {
        let session = super::session::peek();
        let mut choices: Vec<(String, PlaintextChoice)> = session
            .plaintext_consent
            .iter()
            .map(|c| (c.machine_id.clone(), c.choice))
            .collect();
        for (machine, choice) in ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).iter() {
            choices.retain(|(m, _)| m != machine);
            choices.push((machine.clone(), *choice));
        }
        Self { scope: scope(), choices }
    }

    /// Nobody has answered anything — the capture for a test or a path that never asks.
    #[cfg(test)]
    pub(crate) fn undecided() -> Self {
        Self { scope: scope(), choices: Vec::new() }
    }

    /// This capture with one answer set — the tests' consent.
    #[cfg(test)]
    pub(crate) fn with(mut self, machine_id: &str, choice: PlaintextChoice) -> Self {
        self.choices.retain(|(m, _)| m != machine_id);
        self.choices.push((machine_id.to_owned(), choice));
        self
    }

    pub(crate) fn choice(&self, machine_id: &str) -> PlaintextChoice {
        self.choices
            .iter()
            .find(|(m, _)| m == machine_id)
            .map_or(PlaintextChoice::Undecided, |(_, c)| *c)
    }

    /// Mint `machine_id`'s grant at `origin` when — and only when — the person allowed it and
    /// [`mint`] finds the fresh verdict eligible under this capture's generations.
    pub(crate) fn settle(
        &self,
        machine_id: &str,
        origin: &Origin,
        evidence: &InsecureEvidence,
    ) -> Result<(), MintRefusal> {
        if !self.choice(machine_id).allows() {
            return Err(MintRefusal::NotConsented);
        }
        mint(self.scope, machine_id, origin, evidence)
    }
}

/// **The HTTPS upgrade retry.** While a server is on a grant, its endpoint is re-discovered on
/// `crate::pms`'s hub-retry backoff (2 s doubling to a 30 s ceiling): discovery races every HTTPS
/// candidate first, so the attempt that finds one verifying registers the HTTPS origin, and the
/// registry commit then retires the grant (`auth::execute_session_registry`). A server that left
/// the grant table leaves the clock with it, and starts again from the first step if it returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpgradeRetry {
    /// `(machine, due at frame ms, attempts made)`.
    watching: Vec<(String, u32, u32)>,
}

impl UpgradeRetry {
    /// The live step: the servers on a grant now, and the slots whose retry is due at `now` (frame
    /// milliseconds, wrapping).
    pub(crate) fn due(&mut self, now: u32) -> crate::stores::EndpointRefreshSet {
        let mut out = crate::stores::EndpointRefreshSet::default();
        if COUNT.load(Ordering::Acquire) == 0 && self.watching.is_empty() {
            return out;
        }
        for machine in self.step(&granted_machines(), now) {
            if let Some(sid) = super::servers::id_of_machine(&machine) {
                let _ = out.insert(crate::stores::EndpointRefresh { sid });
            }
        }
        out
    }

    /// Pure: follow `granted` and return the machines due at `now`, re-arming each on the next
    /// backoff step.
    fn step(&mut self, granted: &[String], now: u32) -> Vec<String> {
        self.watching.retain(|(m, _, _)| granted.contains(m));
        for machine in granted {
            if !self.watching.iter().any(|(m, _, _)| m == machine) {
                self.watching.push((machine.clone(), after(now, 1), 1));
            }
        }
        let mut due = Vec::new();
        for (machine, at, attempts) in &mut self.watching {
            if now.wrapping_sub(*at) < 0x8000_0000 {
                *attempts = attempts.saturating_add(1);
                *at = after(now, *attempts);
                due.push(machine.clone());
            }
        }
        due
    }
}

/// `now` plus the hub retry's backoff for `attempt`, in frame milliseconds.
fn after(now: u32, attempt: u32) -> u32 {
    now.wrapping_add((crate::pms::backoff_secs(attempt) * 1000.0) as u32)
}

/// Keep only the grants `keep` accepts; when any went, re-grade the registry so no published
/// client keeps a token its origin may no longer carry. Returns whether any went.
fn retain(keep: impl Fn(&PlaintextGrant) -> bool) -> bool {
    let mut grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    let before = grants.len();
    grants.retain(|g| keep(g));
    let removed = grants.len() != before;
    COUNT.store(grants.len(), Ordering::Release);
    drop(grants);
    if removed {
        super::servers::regrade_credentials();
    }
    removed
}

/// Empty the table (the generations keep counting). Under [`crate::testlock::serial`].
#[cfg(test)]
pub(crate) fn reset_for_test() {
    crate::testlock::assert_held("the plaintext grant table (reset)");
    let mut grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    grants.clear();
    COUNT.store(0, Ordering::Release);
    ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// An eligible verdict for a private IPv4 LAN address — the grant fixtures' evidence.
#[cfg(test)]
pub(crate) fn eligible_evidence_for_test() -> InsecureEvidence {
    use crate::plex::probe::{AddressFamily, HttpsRoutes, RouteOutcome};
    InsecureEvidence {
        https: HttpsRoutes {
            lan_plex_direct: RouteOutcome::Tls,
            public_plex_direct: RouteOutcome::Timeout,
            custom_https: RouteOutcome::Absent,
            relay: RouteOutcome::Absent,
        },
        plaintext_local: true,
        public_address_matches: true,
        owned: true,
        https_required: false,
        plaintext_scope: AddressScope::Private,
        plaintext_family: AddressFamily::V4,
        identity_verified: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eligible() -> InsecureEvidence {
        eligible_evidence_for_test()
    }

    fn lan() -> Origin {
        Origin::http("192.168.0.10", 32400)
    }

    /// The pure admission rule: only the exact origin, only under the current generations.
    #[test]
    fn a_grant_admits_only_its_exact_origin_under_the_current_generations() {
        let now = GrantScope { identity: 3, network: 5 };
        let grants = [PlaintextGrant { scope: now, machine_id: "m".into(), origin: lan() }];
        assert!(admits(&grants, now, &lan()));
        assert!(!admits(&grants, now, &Origin::http("192.168.0.10", 32401)), "another port");
        assert!(!admits(&grants, now, &Origin::http("192.168.0.11", 32400)), "another host");
        assert!(
            !admits(&grants, now, &Origin::new(Scheme::Https, "192.168.0.10", 32400)),
            "another scheme"
        );
        assert!(!admits(&grants, GrantScope { identity: 4, network: 5 }, &lan()), "identity moved");
        assert!(!admits(&grants, GrantScope { identity: 3, network: 6 }, &lan()), "network moved");
        assert!(!admits(&[], now, &lan()));
    }

    /// Minting re-checks everything the origin itself can answer, then the eligibility rule, then
    /// the generations — each refusal on its own.
    #[test]
    fn mint_refuses_each_condition_on_its_own() {
        let now = GrantScope { identity: 1, network: 1 };
        assert_eq!(mint_check(&lan(), &eligible(), now, now), Ok(()));
        let https = Origin::new(Scheme::Https, "192.168.0.10", 32400);
        assert_eq!(mint_check(&https, &eligible(), now, now), Err(MintRefusal::NotPlaintext));
        for host in ["nas.example.test", "203.0.113.9", "127.0.0.1"] {
            assert_eq!(
                mint_check(&Origin::http(host, 32400), &eligible(), now, now),
                Err(MintRefusal::NotPrivateLiteral),
                "{host}"
            );
        }
        // A v6 ULA host is private, but not the scope this verdict recorded.
        assert_eq!(
            mint_check(&Origin::http("fd12::1", 32400), &eligible(), now, now),
            Err(MintRefusal::NotPrivateLiteral)
        );
        let mut remote = eligible();
        remote.plaintext_local = false;
        assert_eq!(
            mint_check(&lan(), &remote, now, now),
            Err(MintRefusal::Ineligible(PlaintextEligibility::NotLocal))
        );
        let later = GrantScope { identity: 1, network: 2 };
        assert_eq!(mint_check(&lan(), &eligible(), now, later), Err(MintRefusal::Stale));
    }

    /// The live authority end to end: a store policy refuses plaintext until a grant is minted,
    /// admits exactly that origin while it lives, and refuses it again the instant it is revoked —
    /// or the identity or the network moves.
    #[test]
    fn the_authority_follows_the_grant_table_live() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        reset_for_test();
        let store = CredentialPolicy::HttpsOnly;
        let tls = Origin::parse("https://192-168-0-10.hash.plex.direct:32400").unwrap();
        assert!(allowed_under(store, &tls));
        assert!(!allowed_under(store, &lan()));
        assert!(allowed_under(CredentialPolicy::AllowPlaintext, &lan()));

        mint(scope(), "m", &lan(), &eligible()).expect("eligible");
        assert!(allowed_under(store, &lan()));
        assert!(!allowed_under(store, &Origin::http("192.168.0.11", 32400)));
        assert_eq!(granted_origin("m"), Some(lan()));
        assert!(revoke("m"));
        assert!(!allowed_under(store, &lan()));
        assert_eq!(granted_origin("m"), None);

        mint(scope(), "m", &lan(), &eligible()).expect("eligible");
        network_changed();
        assert!(!allowed_under(store, &lan()), "a new network proves nothing");
        mint(scope(), "m", &lan(), &eligible()).expect("eligible");
        let stale = scope();
        identity_changed();
        assert!(!allowed_under(store, &lan()), "another identity consented to nothing");
        assert_eq!(mint(stale, "m", &lan(), &eligible()), Err(MintRefusal::Stale));
        reset_for_test();
    }

    /// One server, one plaintext origin: a server that moved on the LAN does not keep its old
    /// address credentialed.
    #[test]
    fn a_new_grant_replaces_the_servers_previous_origin() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        reset_for_test();
        mint(scope(), "m", &lan(), &eligible()).expect("eligible");
        let moved = Origin::http("192.168.0.20", 32400);
        mint(scope(), "m", &moved, &eligible()).expect("eligible");
        assert!(!allowed_under(CredentialPolicy::HttpsOnly, &lan()));
        assert!(allowed_under(CredentialPolicy::HttpsOnly, &moved));
        assert_eq!(granted_machines(), vec!["m".to_owned()]);
        reset_for_test();
    }

    /// The upgrade retry follows the grant table on the hub retry's backoff: first attempt after
    /// the first step, then doubling to the ceiling; a server that leaves the table leaves the
    /// clock, and one that returns starts again from the first step.
    #[test]
    fn the_upgrade_retry_backs_off_and_follows_the_grant_table() {
        let mut clock = UpgradeRetry::default();
        let m = vec!["m".to_owned()];
        assert!(clock.step(&m, 0).is_empty(), "armed, not due");
        assert!(clock.step(&m, 1_999).is_empty());
        assert_eq!(clock.step(&m, 2_000), m, "the first step is due");
        assert!(clock.step(&m, 5_999).is_empty(), "then the doubled step");
        assert_eq!(clock.step(&m, 6_000), m);
        let mut at = 6_000u32;
        for _ in 0..10 {
            at += 30_000;
            assert_eq!(clock.step(&m, at), m, "held at the ceiling");
        }
        assert!(clock.step(&[], at + 60_000).is_empty(), "left the table");
        assert!(clock.watching.is_empty());
        let back = at + 60_000;
        assert!(clock.step(&m, back).is_empty());
        assert_eq!(clock.step(&m, back.wrapping_add(2_000)), m, "back at the first step");
    }
}
