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
//! Consent is the ACCOUNT's ([`account_key`]): an answer is honoured only while the plex.tv
//! account that gave it is the one signed in, so a different account signing in on the same
//! television is asked again.
//! It names:
//!
//! * the server's `machineIdentifier`;
//! * the exact plaintext origin — scheme, NUMERIC private host and port — that verified; a name is
//!   never granted, because a resolver could answer it with anything;
//! * the **identity generation** it was minted under ([`identity_changed`]: sign-out, a sign-in
//!   starting; [`roster_replaced`]: a profile switch's COMMIT, which keeps only the grants for the
//!   exact origins the new profile's roster installs);
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
//! [`revoke`], [`identity_changed`], [`network_changed`] and [`roster_replaced`] change the table
//! at once, and the authority is asked when a request STARTS: every request that starts after
//! the change — queued, retried, a cached token-bearing URL, a media reopen — is refused at the
//! transport, and the registry re-grades every published client
//! ([`super::servers::regrade_credentials`]), blanking the token of any whose origin may no
//! longer carry one. A request already on the wire is not interrupted: it completes (or fails)
//! with the credential it started with, and nothing after it gets a new one.
//!
//! A grant also lives only as long as a FRESH verdict keeps reaching its server: discovery
//! withdraws it when the latest probe of that server did not reach the granted origin
//! (`auth::publish_settled_probe`), and retires it when HTTPS verifies
//! (`auth::retire_grant_on_https`). Nothing here watches the network itself — the platform
//! offers this app no network-change signal it subscribes to — so the foreground return is the
//! one continuity break treated as a new network.
//!
//! ## Offers
//!
//! The table also holds what may be ASKED: a fresh eligible insecure-only verdict under the
//! current generations ([`offered`]). The consent surfaces — the sign-in read-out, Home's and the
//! Library's failure read-out, Settings' *Unencrypted connections* — read [`offers`]; an offer is
//! never a grant, and dies with the generations exactly as a grant does.
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Mutex;

use super::origin::{CredentialPolicy, Origin, Scheme};
use super::probe::{AddressScope, InsecureEvidence, PlaintextEligibility};
use super::session::{PlaintextChoice, PlaintextConsent};

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
static ANSWERS: Mutex<Vec<Answer>> = Mutex::new(Vec::new());
/// The fresh eligible verdicts that may be asked about; see the module doc's *Offers*.
static OFFERS: Mutex<Vec<(GrantScope, PlaintextVerdict)>> = Mutex::new(Vec::new());
/// Bumped whenever anything a consent surface or the upgrade retry reads moves — the grant table,
/// the generations, the offers, this launch's answers — so a reader polling it every frame takes
/// no lock until something did.
static REVISION: AtomicU64 = AtomicU64::new(1);

/// One answer given this launch: the [`account_key`] it was given under, the server, the choice.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Answer {
    account: String,
    machine_id: String,
    choice: PlaintextChoice,
}

fn moved() {
    REVISION.fetch_add(1, Ordering::AcqRel);
}

/// The current revision of everything this module publishes; see [`REVISION`].
pub(crate) fn revision() -> u64 {
    REVISION.load(Ordering::Acquire)
}

/// **The account half of a consent's key** — a fingerprint of the plex.tv account token: the
/// first 64 bits of SHA-256 over a domain tag and the token, hex. One-way, so the public
/// preferences file that stores it says nothing usable about the credential; stable for as long
/// as that sign-in lasts, so every profile of the account shares the answer. Empty for an empty
/// token, and an empty key matches nothing — an answer that cannot be bound to an account is not
/// honoured. A fresh sign-in (a new token) is therefore asked again: the closed direction.
pub(crate) fn account_key(account_token: &str) -> String {
    if account_token.is_empty() {
        return String::new();
    }
    let mut input = b"plx-consent:".to_vec();
    input.extend_from_slice(account_token.as_bytes());
    crate::sha256::sha256(&input)[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// **One insecure-only server, as a read-out needs it** — which server, whose it is, whether the
/// same-network rule lets the person be asked ([`InsecureEvidence::plaintext_eligibility`]), and
/// what they already answered. The name and the credit are for the SCREEN only: the report carries
/// the closed eligibility and consent codes, never these strings (`PRIVACY.md`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PlaintextVerdict {
    pub machine_id: String,
    pub name: String,
    /// Whom to name for a SHARED server (`auth::credit_of`); empty for the household's own.
    pub shared_by: String,
    pub eligibility: PlaintextEligibility,
    pub choice: PlaintextChoice,
}

impl PlaintextVerdict {
    /// The person may be asked "Connect without encryption?" about this server.
    pub(crate) fn offers(&self) -> bool {
        self.eligibility == PlaintextEligibility::Eligible
    }

    /// Of two insecure-only servers, the one the read-out should speak about: the first that can
    /// be offered, else the first at all. A household's own non-eligible server does not hide a
    /// share the person could connect to, and the order stays plex.tv's (ours first).
    pub(crate) fn prefer<E>(held: Option<(E, PlaintextVerdict)>, next: (E, PlaintextVerdict))
        -> Option<(E, PlaintextVerdict)> {
        match held {
            Some(held) if held.1.offers() || !next.1.offers() => Some(held),
            _ => Some(next),
        }
    }
}

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
    withdraw_offer(machine_id);
    moved();
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
/// "Connected without encryption", and what the upgrade retry is trying to leave.
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

/// The signed-in identity changed (sign-out, a sign-in starting): every grant and every offer is
/// dead from this instant. A profile switch is [`roster_replaced`], at its commit.
pub(crate) fn identity_changed() {
    IDENTITY.fetch_add(1, Ordering::AcqRel);
    ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    clear_offers();
    if retain(|_| false) {
        crate::log("security: plaintext credentials withdrawn — identity changed");
    }
    moved();
}

/// The television may be on a different network (the app returned to the foreground): every grant
/// and offer is dead from this instant, and the next discovery re-proves the network before
/// minting again.
pub(crate) fn network_changed() {
    NETWORK.fetch_add(1, Ordering::AcqRel);
    clear_offers();
    if retain(|_| false) {
        crate::log("security: plaintext credentials withdrawn — network continuity unknown");
    }
    moved();
}

/// **A profile switch COMMITTED** (`RegistryPlan::Install { replace: true }` — the same commit
/// that blanks every published token, `plex::servers::revoke_for_profile_switch`). The identity
/// moves here, not when the switch starts: a switch that is refused or abandoned changed nobody.
/// A grant survives only when it was live until now and names exactly a `(machine, origin)` the
/// new roster `installed` — which the switch's own fresh probe minted under the consent of the
/// same account ([`account_key`]; this launch's answers are kept for that reason). Every other
/// grant and every offer dies.
pub(crate) fn roster_replaced(installed: &[(String, Origin)]) {
    let before = scope();
    IDENTITY.fetch_add(1, Ordering::AcqRel);
    let now = scope();
    clear_offers();
    let mut grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    let n = grants.len();
    grants.retain_mut(|g| {
        let keep = g.scope == before
            && installed.iter().any(|(machine, origin)| *machine == g.machine_id && *origin == g.origin);
        if keep {
            g.scope = now;
        }
        keep
    });
    let removed = grants.len() != n;
    COUNT.store(grants.len(), Ordering::Release);
    drop(grants);
    if removed {
        super::servers::regrade_credentials();
        crate::log("security: plaintext credentials withdrawn — profile changed");
    }
    moved();
}

/// **Offer `verdict`** — a fresh insecure-only verdict discovery settled under `scope`. An eligible
/// one replaces the server's previous offer; anything else (ineligible now, or captured under
/// generations that have since moved) withdraws it. See the module doc's *Offers*.
pub(crate) fn offered(scope: GrantScope, verdict: PlaintextVerdict) {
    let mut offers = OFFERS.lock().unwrap_or_else(|e| e.into_inner());
    offers.retain(|(_, v)| v.machine_id != verdict.machine_id);
    if verdict.offers() && scope == self::scope() {
        offers.push((scope, verdict));
    }
    drop(offers);
    moved();
}

/// The server is no longer insecure-only (it was reached, refused, or went silent): nothing to ask.
pub(crate) fn withdraw_offer(machine_id: &str) {
    let mut offers = OFFERS.lock().unwrap_or_else(|e| e.into_inner());
    let n = offers.len();
    offers.retain(|(_, v)| v.machine_id != machine_id);
    let changed = offers.len() != n;
    drop(offers);
    if changed {
        moved();
    }
}

fn clear_offers() {
    OFFERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// The live offers, in the order discovery settled them.
pub(crate) fn offers() -> Vec<PlaintextVerdict> {
    let now = scope();
    OFFERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|(scope, _)| *scope == now)
        .map(|(_, v)| v.clone())
        .collect()
}

/// `machine_id`'s live offer, if it has one.
pub(crate) fn offer(machine_id: &str) -> Option<PlaintextVerdict> {
    offers().into_iter().find(|v| v.machine_id == machine_id)
}

/// **Record the person's answer for `machine_id`**, given under `account` ([`account_key`]), for
/// the rest of this launch (the caller persists it too, through `plex::session`). Anything but
/// `Allowed` withdraws the server's grant at once — Settings' switch and the question's *Not now*
/// are both revocation. An answer with no account is not recorded: it could not be honoured.
pub(crate) fn answer(account: &str, machine_id: &str, choice: PlaintextChoice) {
    if !account.is_empty() {
        let mut answers = ANSWERS.lock().unwrap_or_else(|e| e.into_inner());
        answers.retain(|a| a.account != account || a.machine_id != machine_id);
        answers.push(Answer { account: account.to_owned(), machine_id: machine_id.to_owned(), choice });
    }
    {
        let mut offers = OFFERS.lock().unwrap_or_else(|e| e.into_inner());
        for (_, v) in offers.iter_mut().filter(|(_, v)| v.machine_id == machine_id) {
            v.choice = choice;
        }
    }
    if !choice.allows() {
        revoke(machine_id);
    }
    moved();
}

/// **Record the person's answer everywhere it lives**: [`answer`] for this launch (withdrawing the
/// grant at once unless it allows), and the session file for the next, stamped with `account` —
/// the key the owner computed from the account that answered, never re-read from whatever the
/// file holds when the write lands. Every consent surface comes through here, by way of
/// `auth::SessionCmd::AnswerPlaintext` and the session adapter.
pub(crate) fn record(
    account: &str,
    machine_id: &str,
    choice: PlaintextChoice,
) -> Result<(), crate::storage_worker::SubmitError> {
    answer(account, machine_id, choice);
    if account.is_empty() {
        return Ok(());
    }
    let (account, machine) = (account.to_owned(), machine_id.to_owned());
    super::session::queue_update_ticket(move |current| {
        (current.plaintext_choice(&account, &machine) != choice)
            .then(|| current.with_plaintext_choice(&account, &machine, choice))
    })
    .map(|_| ())
}

/// **What the person has answered, as the account `account_token` belongs to** — the persisted
/// answers in `persisted` that carry its [`account_key`], overlaid by this launch's. The one
/// reading of consent: [`PlaintextAsk::capture`] and Settings' list both use it.
pub(crate) fn choices(persisted: &[PlaintextConsent], account_token: &str) -> Vec<(String, PlaintextChoice)> {
    let account = account_key(account_token);
    if account.is_empty() {
        return Vec::new();
    }
    let mut choices: Vec<(String, PlaintextChoice)> = persisted
        .iter()
        .filter(|c| c.account == account)
        .map(|c| (c.machine_id.clone(), c.choice))
        .collect();
    for a in ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).iter().filter(|a| a.account == account) {
        choices.retain(|(m, _)| *m != a.machine_id);
        choices.push((a.machine_id.clone(), a.choice));
    }
    choices
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
    /// The live capture for the work of the account `account_token` belongs to ([`choices`]):
    /// the persisted answers that account gave, overlaid by this launch's. A worker with no account
    /// token (a fresh sign-in) captures none.
    pub(crate) fn capture(account_token: &str) -> Self {
        let session = super::session::peek();
        Self { scope: scope(), choices: choices(&session.plaintext_consent, account_token) }
    }

    /// The generations this capture may mint and offer under.
    pub(crate) fn scope(&self) -> GrantScope {
        self.scope
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

/// **The HTTPS upgrade retry.** While a server is on a grant, its endpoint is re-discovered:
/// first on `crate::pms`'s hub-retry backoff (2 s doubling to 30 s), then still doubling to a
/// ten-minute ceiling ([`retry_secs`]) — a server that stays on plaintext all evening costs a few
/// re-discoveries an hour. Discovery races every HTTPS candidate first, so the attempt that finds
/// one verifying registers the HTTPS origin, and the registry commit then retires the grant
/// (`auth::execute_session_registry`). A server that left the grant table leaves the clock with
/// it, and starts again from the first step if it returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UpgradeRetry {
    /// `(machine, due at frame ms, attempts made)`.
    watching: Vec<(String, u32, u32)>,
    /// The [`revision`] the table was last read at.
    seen: u64,
    /// The earliest due time in `watching`, if any — so a frame with nothing due and nothing moved
    /// reads two atomics and returns, with no lock and no allocation.
    next: Option<u32>,
}

impl UpgradeRetry {
    /// The live step: the servers on a grant now, and the slots whose retry is due at `now` (frame
    /// milliseconds, wrapping). Called every frame; see [`UpgradeRetry::poll`].
    pub(crate) fn due(&mut self, now: u32) -> crate::stores::EndpointRefreshSet {
        let mut out = crate::stores::EndpointRefreshSet::default();
        for machine in self.poll(now, revision(), granted_machines) {
            if let Some(sid) = super::servers::id_of_machine(&machine) {
                let _ = out.insert(crate::stores::EndpointRefresh { sid });
            }
        }
        out
    }

    /// Read the grant table (`granted`) only when it moved since the last read (`revision`) or a
    /// retry is due; otherwise answer nothing without touching it.
    fn poll(&mut self, now: u32, revision: u64, granted: impl FnOnce() -> Vec<String>) -> Vec<String> {
        let waiting = self.next.is_none_or(|at| now.wrapping_sub(at) >= 0x8000_0000);
        if revision == self.seen && waiting {
            return Vec::new();
        }
        self.seen = revision;
        let due = self.step(&granted(), now);
        self.next = self.watching.iter().map(|(_, at, _)| *at).min_by_key(|at| at.wrapping_sub(now));
        due
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

/// How many attempts follow the hub retry's own backoff before [`retry_secs`] keeps doubling.
const HUB_STEPS: u32 = 5;
/// The upgrade retry's ceiling, in seconds.
const UPGRADE_CEILING_S: f32 = 600.0;

/// The delay before upgrade attempt `attempt`: the hub retry's backoff for the first [`HUB_STEPS`],
/// then doubling from its ceiling to [`UPGRADE_CEILING_S`].
fn retry_secs(attempt: u32) -> f32 {
    let hub = crate::pms::backoff_secs(attempt.min(HUB_STEPS));
    let doublings = attempt.saturating_sub(HUB_STEPS).min(16);
    (hub * (1u32 << doublings) as f32).min(UPGRADE_CEILING_S)
}

/// `now` plus [`retry_secs`] for `attempt`, in frame milliseconds.
fn after(now: u32, attempt: u32) -> u32 {
    now.wrapping_add((retry_secs(attempt) * 1000.0) as u32)
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
        moved();
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
    drop(grants);
    ANSWERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    clear_offers();
    moved();
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

    /// The upgrade retry follows the grant table: the hub retry's backoff for the first attempts
    /// (2 s doubling to 30 s), then still doubling, to a ten-minute ceiling — a server that stays on
    /// plaintext for an evening costs a handful of re-discoveries an hour, not one every 30 s. A
    /// server that leaves the table leaves the clock, and one that returns starts again from the
    /// first step.
    #[test]
    fn the_upgrade_retry_backs_off_to_a_ten_minute_ceiling_and_follows_the_grant_table() {
        let mut clock = UpgradeRetry::default();
        let m = vec!["m".to_owned()];
        assert!(clock.step(&m, 0).is_empty(), "armed, not due");
        assert!(clock.step(&m, 1_999).is_empty());
        assert_eq!(clock.step(&m, 2_000), m, "the first step is due");
        assert!(clock.step(&m, 5_999).is_empty(), "then the doubled step");
        assert_eq!(clock.step(&m, 6_000), m);
        let mut at = 6_000u32;
        let mut gaps = Vec::new();
        for _ in 0..10 {
            let mut gap = 1_000u32;
            while clock.step(&m, at + gap).is_empty() {
                gap += 1_000;
            }
            at += gap;
            gaps.push(gap / 1_000);
        }
        assert_eq!(gaps, [8, 16, 30, 60, 120, 240, 480, 600, 600, 600]);
        assert!(clock.step(&[], at + 60_000).is_empty(), "left the table");
        assert!(clock.watching.is_empty());
        let back = at + 60_000;
        assert!(clock.step(&m, back).is_empty());
        assert_eq!(clock.step(&m, back.wrapping_add(2_000)), m, "back at the first step");
    }

    /// An idle frame costs two atomic loads: the table is read only when it moved or a retry is
    /// due, never on the frames in between.
    #[test]
    fn an_idle_upgrade_retry_reads_the_table_only_when_it_moved_or_a_retry_is_due() {
        let mut clock = UpgradeRetry::default();
        let reads = std::cell::Cell::new(0);
        let m = || { reads.set(reads.get() + 1); vec!["m".to_owned()] };
        assert!(clock.poll(0, 1, m).is_empty(), "the first read arms the clock");
        assert_eq!(reads.get(), 1);
        for now in (16..2_000).step_by(16) {
            assert!(clock.poll(now, 1, m).is_empty());
        }
        assert_eq!(reads.get(), 1, "nothing moved and nothing was due");
        assert_eq!(clock.poll(2_000, 1, m), vec!["m".to_owned()]);
        assert_eq!(reads.get(), 2, "a due retry reads the table");
        assert!(clock.poll(2_016, 2, m).is_empty());
        assert_eq!(reads.get(), 3, "a moved table is read at once");
        let mut idle = UpgradeRetry::default();
        let none = || { reads.set(reads.get() + 1); Vec::new() };
        let _ = idle.poll(0, 1, none);
        for now in (16..600_000).step_by(1_000) {
            let _ = idle.poll(now, 1, none);
        }
        assert_eq!(reads.get(), 4, "a household with no grant reads nothing after the first frame");
    }

    /// **Consent is the account's.** An answer recorded under one account's key is honoured for
    /// that account — on any of its profiles — and for no other: a different plex.tv account on the
    /// same television (or no account at all) captures nothing, and is asked again.
    #[test]
    fn consent_is_honoured_only_for_the_account_that_gave_it() {
        let _g = crate::testlock::serial();
        reset_for_test();
        let owner = account_key("owner-token");
        assert_eq!(owner.len(), 16);
        assert_ne!(owner, account_key("other-token"));
        assert_eq!(account_key(""), "");
        let persisted = vec![PlaintextConsent {
            machine_id: "m".into(),
            account: owner.clone(),
            choice: PlaintextChoice::Allowed,
            extensions: Default::default(),
        }];
        assert_eq!(choices(&persisted, "owner-token"), vec![("m".to_owned(), PlaintextChoice::Allowed)]);
        assert!(choices(&persisted, "other-token").is_empty());
        assert!(choices(&persisted, "").is_empty());
        answer(&account_key("other-token"), "m", PlaintextChoice::Declined);
        assert_eq!(choices(&persisted, "owner-token"), vec![("m".to_owned(), PlaintextChoice::Allowed)],
            "another account's answer does not overlay this one's");
        assert_eq!(choices(&persisted, "other-token"), vec![("m".to_owned(), PlaintextChoice::Declined)]);
        answer("", "n", PlaintextChoice::Allowed);
        assert!(choices(&[], "").is_empty(), "an answer with no account is never honoured");
        reset_for_test();
    }

    /// An offer is a fresh ELIGIBLE verdict under the current generations; the answer re-words it,
    /// a reach or a mint withdraws it, and the generations moving kill it with the grants.
    #[test]
    fn an_offer_follows_the_fresh_verdict_the_answer_and_the_generations() {
        let _g = crate::testlock::serial();
        crate::plex::reset_servers_for_test();
        reset_for_test();
        let verdict = |eligibility| PlaintextVerdict {
            machine_id: "m".into(), name: "nas".into(), shared_by: String::new(),
            eligibility, choice: PlaintextChoice::Undecided,
        };
        let rev = revision();
        offered(scope(), verdict(PlaintextEligibility::Eligible));
        assert!(revision() > rev, "an offer moves the revision");
        assert_eq!(offer("m").map(|v| v.choice), Some(PlaintextChoice::Undecided));
        answer(&account_key("t"), "m", PlaintextChoice::Declined);
        assert_eq!(offer("m").map(|v| v.choice), Some(PlaintextChoice::Declined));
        offered(scope(), verdict(PlaintextEligibility::NotLocal));
        assert!(offers().is_empty(), "an ineligible verdict withdraws it");
        let stale = scope();
        network_changed();
        offered(stale, verdict(PlaintextEligibility::Eligible));
        assert!(offers().is_empty(), "a verdict from before the network moved is not offered");
        offered(scope(), verdict(PlaintextEligibility::Eligible));
        mint(scope(), "m", &lan(), &eligible()).expect("eligible");
        assert!(offers().is_empty(), "a mint answers the offer");
        offered(scope(), verdict(PlaintextEligibility::Eligible));
        identity_changed();
        assert!(offers().is_empty(), "another identity was offered nothing");
        reset_for_test();
    }
}
