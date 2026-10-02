//! **Onboarding incidents for Sentry: a failed sign-in, discovery, save, locked read, profile
//! switch or first content load, as one closed handled-error report.**
//!
//! Consent is asked after a successful sign-in, so a television whose onboarding FAILS has never
//! been asked anything — the population behind issues #75 and #76 is exactly the one the standing
//! error channel cannot see. An incident therefore has two ways out, and [`ConsentKind`] tags which
//! one produced a report:
//!
//! * **Standing** ([`report_standing`]): error reports are on at [`ONBOARDING_REPORT_SCOPE`] or
//!   above — the person opted in on a build that disclosed this report. Spooled on the Errors lane
//!   with the crash-report id, like `telemetry::playback`'s handled error; its event id is shown
//!   as the Report ID too.
//! * **One-off** ([`send_one_off`]): anybody else, and only after they press Send on the report
//!   they were shown. No identifier of any kind; the event id is the receipt they can quote.
//!
//! Which of the two applies is `consent::report_permission`'s answer, not this module's.
//!
//! **Every value accepted here is closed or bucketed.** An [`IncidentKind`], a [`LinkClass`] built
//! from the network layer's own evidence, bucketed counts and durations, a clamped code
//! generation, a persistence failure class, and — on the two discovery verdicts that need it —
//! per-route HTTPS outcome classes, closed facts about a verified plaintext answer
//! (`plex::probe::InsecureEvidence`) or a bucketed resource count. The only raw integers are the HTTP status, the
//! `CURLcode` and the storage service's own error code — numbers with no identity of their own.
//! There is no free-text slot at all: no error or caption string (a profile-switch message embeds
//! the profile's title), no PIN, code, token, account, profile or server name, URL, hostname or
//! address. `every_body_is_closed_and_carries_no_identity_or_content_slot` walks every body shape
//! to keep it that way.
//!
//! Ported and generalised from 0.6.6's `telemetry/signin.rs`, which reported sign-in failures only.
//! Its free-form storage outcomes are replaced by closed persistence/helper stages,
//! keymanager stages and a bounded array of candidate errno numbers (never candidate paths).

use super::consent::{self, Permission, ONBOARDING_REPORT_SCOPE};
use crate::net::{RequestError, RequestFailure};
use crate::plex::session::async_persistence::{CompletionOutcome, Failure};
use crate::storage::wire::KeymanagerStage;
use serde_json::Value;

/// Why server discovery ended with nothing to connect to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum DiscoveryClass {
    /// The account has no server at all.
    NoServers,
    /// A server answered and refused the credentials.
    Refused,
    /// No server answered.
    Silent,
    /// Only insecure connections were offered, and none was allowed.
    InsecureOnly,
}

/// Which first content load failed during onboarding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ContentSource {
    Home,
    Libraries,
}

/// A sign-in that failed inside the app itself rather than on any network: its own machinery
/// refused or lost the work. Closed, like every other class here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum InternalClass {
    /// The sign-in's work was not admitted by the adapter.
    AdmissionRefused,
    /// The sign-in worker could not be started (the thread spawn was refused).
    WorkerRefused,
    /// The sign-in worker went away without an answer.
    WorkerDropped,
    /// The finished sign-in could not be committed.
    CommitRefused,
    /// The installation's client identifier could not be read to start the sign-in.
    ClientIdUnavailable,
    /// An internal counter ran out.
    Exhausted,
}

/// Where onboarding failed. Closed, and each variant's [`Self::code`] is a stable wire value —
/// Sentry groups on it, so renaming one splits an issue in two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum IncidentKind {
    /// The sign-in code could not be created.
    PinCreate,
    /// plex.tv stopped answering while the code was on screen. Offered only behind *Details* —
    /// never by the alert, which would cover a code that may be mid-scan — and never sent
    /// automatically: a slow network is not a failure until the person decides it is one.
    LinkStalled,
    /// Every code the flow issued expired unused.
    PinExpired,
    /// plex.tv refused the account token the sign-in had just been given (a 401 or 403 from
    /// `/resources` during discovery).
    Authorization,
    Discovery(DiscoveryClass),
    /// The sign-in could not be saved.
    SaveFailed,
    /// The saved sign-in exists but could not be opened.
    StoredLocked,
    /// Switching to a profile failed. Never raised for a wrong PIN.
    ProfileSwitch,
    ContentLoad(ContentSource),
    /// The sign-in failed inside the app, not on the network — see [`InternalClass`].
    Internal(InternalClass),
}

impl IncidentKind {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::PinCreate => "pin_create",
            Self::LinkStalled => "link_stalled",
            Self::PinExpired => "pin_expired",
            Self::Authorization => "authorization",
            Self::Discovery(DiscoveryClass::NoServers) => "discovery_no_servers",
            Self::Discovery(DiscoveryClass::Refused) => "discovery_refused",
            Self::Discovery(DiscoveryClass::Silent) => "discovery_silent",
            Self::Discovery(DiscoveryClass::InsecureOnly) => "discovery_insecure_only",
            Self::SaveFailed => "save_failed",
            Self::StoredLocked => "stored_locked",
            Self::ProfileSwitch => "profile_switch",
            Self::ContentLoad(ContentSource::Home) => "content_load_home",
            Self::ContentLoad(ContentSource::Libraries) => "content_load_libraries",
            Self::Internal(InternalClass::AdmissionRefused) => "internal_admission_refused",
            Self::Internal(InternalClass::WorkerRefused) => "internal_worker_refused",
            Self::Internal(InternalClass::WorkerDropped) => "internal_worker_dropped",
            Self::Internal(InternalClass::CommitRefused) => "internal_commit_refused",
            Self::Internal(InternalClass::ClientIdUnavailable) => "internal_client_id_unavailable",
            Self::Internal(InternalClass::Exhausted) => "internal_exhausted",
        }
    }

    /// Does a failure of this kind mean plex.tv refused the account token the sign-in holds? Such
    /// a token cannot be retried: *Try again* after one must start a new QR sign-in, not a
    /// discovery pass that presents the refused token again (`auth::retry_kind`).
    pub(crate) fn refuses_the_account_token(self) -> bool {
        matches!(self, Self::Authorization)
    }

    /// May a Granted person's incident of this kind be sent WITHOUT asking? Everything except a
    /// stalled wait, which is only ever offered.
    pub(crate) fn standing_eligible(self) -> bool {
        !matches!(self, Self::LinkStalled)
    }

    /// May an incident of this kind put the report question on screen as an alert? Everything
    /// except a stalled wait: the QR code is still up during one, possibly mid-scan, so a stall
    /// is offered only behind *Details*, when the person asks for it.
    pub(crate) fn alert_eligible(self) -> bool {
        !matches!(self, Self::LinkStalled)
    }
}

// What the most recent network call observed, coarsened into the class this report carries
// (`LinkClass`), and the pure `classify` that coarsens it, are defined in `plex::probe`: the probe
// grades its own transport failures in the same vocabulary and `plex` sits beneath this layer.
// Re-exported, so every incident producer and reader keeps naming them here.
pub(crate) use crate::plex::probe::{classify, LinkClass};

/// How many consecutive calls came back with no usable answer, bucketed — never the raw count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum UnansweredBucket {
    Zero,
    One,
    TwoToFive,
    SixPlus,
}

impl UnansweredBucket {
    pub(crate) fn from_count(n: u32) -> Self {
        match n {
            0 => Self::Zero,
            1 => Self::One,
            2..=5 => Self::TwoToFive,
            _ => Self::SixPlus,
        }
    }
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::TwoToFive => "two_to_five",
            Self::SixPlus => "six_plus",
        }
    }
}

/// A count of things plex.tv returned, bucketed — never the raw count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum CountBucket {
    Zero,
    One,
    TwoToFive,
    SixPlus,
}

impl CountBucket {
    pub(crate) fn from_count(n: usize) -> Self {
        match n {
            0 => Self::Zero,
            1 => Self::One,
            2..=5 => Self::TwoToFive,
            _ => Self::SixPlus,
        }
    }
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::TwoToFive => "two_to_five",
            Self::SixPlus => "six_plus",
        }
    }
}

/// Which flow ran the discovery that failed: the one right after a fresh code was authorized, or
/// the discovery-only retry (*Try again*) that reuses that authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum DiscoveryTrigger {
    Login,
    Rediscover,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum DiscoveryTarget { PlexTv, Servers }
impl DiscoveryTarget {
    pub(crate) fn code(self) -> &'static str {
        match self { Self::PlexTv => "plex_tv", Self::Servers => "servers" }
    }
}

/// Evidence shared by every discovery verdict. `target` is absent for NoServers so moving its
/// trigger into this shared record leaves that established JSON byte-for-byte unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DiscoveryEvidence {
    pub trigger: DiscoveryTrigger,
    pub target: Option<DiscoveryTarget>,
}

impl DiscoveryTrigger {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Rediscover => "rediscover",
        }
    }
}

/// Why `/resources` named no server: how much it did return, and which flow asked. Every
/// resource counted is a non-server one by definition of the verdict, so one bucket says both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct NoServersEvidence {
    pub resources: CountBucket,
    pub trigger: DiscoveryTrigger,
}

/// How long the current run of misses has lasted, bucketed — never the raw duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum FailingForBucket {
    None,
    Under10s,
    Under60s,
    Under5m,
    FiveMinPlus,
}

impl FailingForBucket {
    pub(crate) fn from_duration(d: Option<std::time::Duration>) -> Self {
        let Some(d) = d else { return Self::None };
        match d.as_secs() {
            0..=9 => Self::Under10s,
            10..=59 => Self::Under60s,
            60..=299 => Self::Under5m,
            _ => Self::FiveMinPlus,
        }
    }
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Under10s => "under_10s",
            Self::Under60s => "under_60s",
            Self::Under5m => "under_5m",
            Self::FiveMinPlus => "five_min_plus",
        }
    }
}

/// Why saving (or re-reading) the sign-in failed, from the session persistence completion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PersistenceFailure {
    /// The storage worker refused the job.
    Admission,
    /// The write itself failed.
    WriteFailed,
    /// The store refused or failed the record.
    Storage,
    /// The commit may or may not have reached the disk.
    CommitUncertain,
    /// Sealing the credentials with the platform key service failed.
    Protection,
    /// Sealing may or may not have happened.
    ProtectionUncertain,
    /// The worker went away before answering.
    WorkerDropped,
}

impl PersistenceFailure {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Admission => "admission",
            Self::WriteFailed => "write_failed",
            Self::Storage => "storage",
            Self::CommitUncertain => "commit_uncertain",
            Self::Protection => "protection",
            Self::ProtectionUncertain => "protection_uncertain",
            Self::WorkerDropped => "worker_dropped",
        }
    }
}

pub(crate) fn keymanager_stage_code(stage: KeymanagerStage) -> &'static str {
    match stage {
        KeymanagerStage::Validate => "validate",
        KeymanagerStage::Generate => "generate",
        KeymanagerStage::Begin => "begin",
        KeymanagerStage::Finish => "finish",
        KeymanagerStage::Roundtrip => "roundtrip",
    }
}

/// **What became of a "Connect without encryption?" offer** — the closed consent outcome an
/// insecure-only report carries for an eligible server (`plaintext_consent`). Never the server,
/// its address or its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PlaintextConsentOutcome {
    /// Eligible, and the person had not answered — the question is on screen.
    Offered,
    /// Allowed, yet this discovery still could not connect (the grant was refused as stale, or
    /// the consented origin did not admit the credential).
    Accepted,
    /// *Not now* on the question.
    Declined,
    /// Turned off in Settings after it had been allowed.
    Revoked,
}

impl PlaintextConsentOutcome {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Offered => "offered",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Revoked => "revoked",
        }
    }
}

/// Everything one incident report carries. Every field is a closed enum, a bucket, a clamped
/// count or a bare number with no identity of its own — see the module doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct IncidentContext {
    pub kind: IncidentKind,
    pub link: LinkClass,
    /// The exact HTTP status — only with an `Answered*` link class.
    pub http_status: Option<u16>,
    /// The exact `CURLcode` — only with a `Dns`, `Tls`, `Timeout` or `TransportOther` link class.
    pub curl_rc: Option<i32>,
    pub unanswered: UnansweredBucket,
    pub failing_for: FailingForBucket,
    /// Which sign-in code the flow was on, 1..=4 — clamped, since it counts codes issued during
    /// one flow. `None` outside the code flow.
    pub code_generation: Option<u8>,
    pub persistence: Option<PersistenceFailure>,
    #[serde(default)]
    pub helper: Option<crate::storage::wire::failure::HelperFailure>,
    #[serde(default)]
    pub candidate_errnos: [Option<i32>; 8],
    /// The key-service stage a protection failure stopped at.
    pub keymanager_stage: Option<KeymanagerStage>,
    /// The key service's own numeric error code, when it gave one.
    pub service_error_code: Option<i32>,
    /// Why discovery settled as insecure-only — only on
    /// [`DiscoveryClass::InsecureOnly`]. Every field closed; see
    /// [`crate::plex::probe::InsecureEvidence`].
    #[serde(default)]
    pub insecure: Option<crate::plex::probe::InsecureEvidence>,
    /// What became of the "Connect without encryption?" offer for the server an insecure-only
    /// verdict speaks about — only when that server was eligible to be asked. A closed code; the
    /// server and its owner are never carried.
    #[serde(default)]
    pub plaintext_consent: Option<PlaintextConsentOutcome>,
    /// What `/resources` returned when it named no server — only on [`DiscoveryClass::NoServers`].
    #[serde(default)]
    pub no_servers: Option<NoServersEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<DiscoveryEvidence>,
    /// Exact request count for local presentation. Reports continue to emit only the closed
    /// `unanswered` bucket; old persisted contexts deserialize with this absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_attempts: Option<u32>,
    /// Unix-epoch milliseconds when the context was BUILT — not when a report carrying it reaches
    /// the wire, which for a one-off is whenever the person presses Send and for a standing report
    /// can be a later launch's flush. `0` when the wall clock was at or before the epoch, and for
    /// an [`IncidentKind::Internal`] context, which is built where no clock is read.
    pub occurred_at_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// **The [`LinkClass`] outcomes a plex.tv-facing incident always reads the same way**, shared by
/// [`IncidentKind::PinCreate`] (`mint_pin`, `auth.rs`) and the `Discovery(Silent)`/`PlexTv` arm of
/// [`IncidentContext::readout_glyph`] — the one place both used to spell out the same three arms
/// separately, which is exactly how `PinCreate` drifted (it never grew them at all, and kept a
/// fixed `WifiSlash` even once `self.link` recorded plex.tv actually answering). `Dns`/`Tls`/an
/// `Answered*` status all say something SPECIFIC happened reaching or on plex.tv, so both callers
/// agree here. `None` for `Timeout`/`TransportOther`/`Unknown` — the two callers diverge only on
/// that "nothing specific happened" fallback, each picking its own glyph, so this returns `None`
/// rather than guessing one. Exhaustive on [`LinkClass`], no wildcard arm.
fn plextv_link_glyph(link: LinkClass) -> Option<crate::ui::icons::Icon> {
    use crate::ui::icons::Icon;
    match link {
        LinkClass::Dns => Some(Icon::GlobeBadgeQuestion),
        LinkClass::Tls => Some(Icon::LockBadgeAlert),
        LinkClass::Answered2xx
        | LinkClass::Answered4xx
        | LinkClass::Answered5xx
        | LinkClass::AnsweredOther => Some(Icon::CloudBadgeAlert),
        LinkClass::Timeout | LinkClass::TransportOther | LinkClass::Unknown => None,
    }
}

impl IncidentContext {
    /// A context for `kind`, classifying the last network call and stamping the time. The link
    /// counters, code generation and persistence evidence start empty; the `with_*` builders set
    /// the ones a producer has.
    pub(crate) fn new(kind: IncidentKind, last: Option<Result<u16, RequestFailure>>) -> Self {
        let (link, http_status, curl_rc) = classify(last);
        Self {
            kind,
            link,
            http_status,
            curl_rc,
            unanswered: UnansweredBucket::Zero,
            failing_for: FailingForBucket::None,
            code_generation: None,
            persistence: None, helper: None, candidate_errnos: [None; 8],
            keymanager_stage: None,
            service_error_code: None,
            insecure: None,
            plaintext_consent: None,
            no_servers: None,
            discovery: None,
            discovery_attempts: None,
            occurred_at_ms: now_ms(),
        }
    }

    /// A failure inside the app ([`IncidentKind::Internal`]). Raised by the session owner, which
    /// reads no clock — it is pure and replayable — so it carries no occurrence time and the
    /// report's `timestamp` is left to Sentry's receipt time (see [`Self::occurred_at_ms`]).
    pub(crate) fn internal(class: InternalClass) -> Self {
        Self {
            kind: IncidentKind::Internal(class),
            link: LinkClass::Unknown,
            http_status: None,
            curl_rc: None,
            unanswered: UnansweredBucket::Zero,
            failing_for: FailingForBucket::None,
            code_generation: None,
            persistence: None, helper: None, candidate_errnos: [None; 8],
            keymanager_stage: None,
            service_error_code: None,
            insecure: None,
            plaintext_consent: None,
            no_servers: None,
            discovery: None,
            discovery_attempts: None,
            occurred_at_ms: 0,
        }
    }

    /// The sign-in wait's live counters: consecutive unanswered polls, how long they have lasted,
    /// and which code the flow is on.
    pub(crate) fn with_link_state(
        mut self,
        unanswered: u32,
        failing_for: Option<std::time::Duration>,
        code_generation: u32,
    ) -> Self {
        self.unanswered = UnansweredBucket::from_count(unanswered);
        self.failing_for = FailingForBucket::from_duration(failing_for);
        self.code_generation = Some(code_generation.clamp(1, 4) as u8);
        self
    }

    /// The evidence behind an insecure-only discovery verdict.
    pub(crate) fn with_insecure(mut self, evidence: crate::plex::probe::InsecureEvidence) -> Self {
        self.insecure = Some(evidence);
        self
    }

    /// The consent outcome for an insecure-only verdict's eligible server (see
    /// [`Self::plaintext_consent`]); `None` leaves it off the report.
    pub(crate) fn with_plaintext_consent(mut self, outcome: Option<PlaintextConsentOutcome>) -> Self {
        self.plaintext_consent = outcome;
        self
    }

    /// The evidence behind a no-servers discovery verdict.
    pub(crate) fn with_no_servers(mut self, evidence: NoServersEvidence) -> Self {
        self.no_servers = Some(evidence);
        self
    }

    pub(crate) fn with_discovery(mut self, evidence: DiscoveryEvidence) -> Self {
        self.discovery = Some(evidence);
        self
    }

    /// A completed bounded retry run. Reuses the established bucket fields and deliberately does
    /// not set `code_generation`, which belongs only to QR/PIN issuance.
    pub(crate) fn with_retry_run(mut self, attempts: u32, elapsed: std::time::Duration) -> Self {
        self.unanswered = UnansweredBucket::from_count(attempts);
        self.failing_for = FailingForBucket::from_duration(Some(elapsed));
        self.discovery_attempts = Some(attempts);
        self
    }

    /// The failure class of a session persistence completion. A completion that is not a failure
    /// (durable, or superseded by a later clear) leaves the context unchanged.
    // The save-failure producer (`IncidentKind::SaveFailed`) is the next stage's; its evidence
    // builder lands with the schema so the body and the notice describe one closed set.
    #[allow(dead_code)]
    pub(crate) fn with_persistence(mut self, outcome: &CompletionOutcome) -> Self {
        let (class, protection) = match outcome {
            CompletionOutcome::Durable(_)
            | CompletionOutcome::Superseded
            | CompletionOutcome::Failed(Failure::Superseded) => return self,
            CompletionOutcome::Uncertain { .. } => (PersistenceFailure::CommitUncertain, None),
            CompletionOutcome::ProtectionUncertain(p) => (PersistenceFailure::ProtectionUncertain, Some(p)),
            CompletionOutcome::Failed(Failure::Admission(_)) => (PersistenceFailure::Admission, None),
            CompletionOutcome::Failed(Failure::Persistence(_)) => (PersistenceFailure::WriteFailed, None),
            CompletionOutcome::Failed(Failure::Storage(_) | Failure::Helper(..)) => (PersistenceFailure::Storage, None),
            CompletionOutcome::Failed(Failure::Protection(p)) => (PersistenceFailure::Protection, Some(p)),
            CompletionOutcome::Failed(Failure::WorkerDropped) => (PersistenceFailure::WorkerDropped, None),
        };
        let (helper, errnos) = outcome.helper_evidence();
        if let Some(failure) = helper {
            self.helper = Some(failure);
            self.candidate_errnos = errnos;
        }
        self.persistence = Some(class);
        self.keymanager_stage = protection.map(|p| p.failure.stage);
        self.service_error_code = protection.and_then(|p| p.failure.service_code);
        self
    }

    /// **The one glyph a page-filling `Failed` read-out draws above its verdict, decided from the
    /// SAME evidence as the caption** (`auth::discovery_failure`'s table), so the two can never
    /// disagree — this is not a second, string-matched opinion about what went wrong.
    ///
    /// Exhaustive on [`IncidentKind`] and, where discovery needs it, on [`DiscoveryClass`] and
    /// [`LinkClass`] — no wildcard arm, so a new variant is a compile error here rather than a
    /// silent fallthrough. The owner-approved mapping (`docs` carries none of this; it lives only
    /// here and at the call sites that read it):
    ///
    /// * [`IncidentKind::PinExpired`] / [`IncidentKind::LinkStalled`] — the wait ran out —
    ///   `ClockBadgeAlert`.
    /// * [`IncidentKind::PinCreate`] — `mint_pin` (`auth.rs`) keeps its last plex.tv response in
    ///   `self.link` the same as discovery does, so this follows the SAME [`LinkClass`] table as
    ///   the `Discovery(Silent)`/`PlexTv` arm below (`plextv_link_glyph`) rather than a fixed
    ///   glyph: `Dns` → `GlobeBadgeQuestion`, `Tls` → `LockBadgeAlert`, an `Answered*` class
    ///   (plex.tv answered, just not usably) → `CloudBadgeAlert`. Only its FALLBACK differs —
    ///   `Timeout`/`TransportOther`/`Unknown`, or no link recorded at all (`mint_pin` never even
    ///   got to ask) — `WifiSlash`, the genuine "can't get out" case, rather than the Discovery
    ///   arm's `GlobeBadgeMinus`.
    /// * [`IncidentKind::Authorization`] — plex.tv refused the token — `PersonBadgeXmark`.
    /// * `Discovery(NoServers)` — the account has no server — `ServerBadgePlus`.
    /// * `Discovery(Refused)` — a server answered and refused — `ServerBadgeXmark`.
    /// * `Discovery(Silent)` targeting [`DiscoveryTarget::Servers`] — plex.tv named servers that
    ///   never answered — `ServerBadgeMinus`; targeting [`DiscoveryTarget::PlexTv`] (or absent) —
    ///   plex.tv itself did not answer, so the badge follows `plextv_link_glyph`, falling back to
    ///   `GlobeBadgeMinus` (not `PinCreate`'s `WifiSlash`: discovery already ran, so SOME network
    ///   reached somewhere, unlike `PinCreate`'s failure to even start).
    /// * `Discovery(InsecureOnly)` — not in the owner's table; the closest honest read is "the
    ///   server itself is the obstacle", the same shape as a Silent/Servers verdict, so this
    ///   shares `ServerBadgeMinus` rather than inventing new art.
    /// * [`IncidentKind::SaveFailed`] / [`IncidentKind::StoredLocked`] — the sign-in could not be
    ///   kept or read back — `KeyBadgeAlert`.
    /// * [`IncidentKind::ProfileSwitch`] — `PeopleBadgeAlert`.
    /// * [`IncidentKind::ContentLoad`] — first content load failing reads exactly like Home's or
    ///   the Library's own "can't reach" verdict — `ServerBadgeMinus`.
    /// * [`IncidentKind::Internal`] — not in the owner's table either; the app's own machinery
    ///   failed rather than any named server or link, so this takes `CloudBadgeAlert`, the same
    ///   "something didn't work" mark an answered-but-broken plex.tv call wears, as the least
    ///   specific honest choice among the twelve.
    pub(crate) fn readout_glyph(&self) -> crate::ui::icons::Icon {
        use crate::ui::icons::Icon;
        match self.kind {
            IncidentKind::PinExpired | IncidentKind::LinkStalled => Icon::ClockBadgeAlert,
            IncidentKind::PinCreate => plextv_link_glyph(self.link).unwrap_or(Icon::WifiSlash),
            IncidentKind::Authorization => Icon::PersonBadgeXmark,
            IncidentKind::Discovery(DiscoveryClass::NoServers) => Icon::ServerBadgePlus,
            IncidentKind::Discovery(DiscoveryClass::Refused) => Icon::ServerBadgeXmark,
            IncidentKind::Discovery(DiscoveryClass::InsecureOnly) => Icon::ServerBadgeMinus,
            IncidentKind::Discovery(DiscoveryClass::Silent) => {
                match self.discovery.and_then(|d| d.target) {
                    Some(DiscoveryTarget::Servers) => Icon::ServerBadgeMinus,
                    Some(DiscoveryTarget::PlexTv) | None =>
                        plextv_link_glyph(self.link).unwrap_or(Icon::GlobeBadgeMinus),
                }
            }
            IncidentKind::SaveFailed | IncidentKind::StoredLocked => Icon::KeyBadgeAlert,
            IncidentKind::ProfileSwitch => Icon::PeopleBadgeAlert,
            IncidentKind::ContentLoad(ContentSource::Home | ContentSource::Libraries) => {
                Icon::ServerBadgeMinus
            }
            IncidentKind::Internal(
                InternalClass::AdmissionRefused
                | InternalClass::WorkerRefused
                | InternalClass::WorkerDropped
                | InternalClass::CommitRefused
                | InternalClass::ClientIdUnavailable
                | InternalClass::Exhausted,
            ) => Icon::CloudBadgeAlert,
        }
    }
}

#[cfg(test)]
mod readout_glyph_tests {
    use super::*;
    use crate::ui::icons::Icon;

    fn ctx(kind: IncidentKind) -> IncidentContext {
        IncidentContext::new(kind, None)
    }

    fn with_link(mut c: IncidentContext, link: LinkClass) -> IncidentContext {
        c.link = link;
        c
    }

    fn with_target(mut c: IncidentContext, target: DiscoveryTarget) -> IncidentContext {
        c.discovery = Some(DiscoveryEvidence { trigger: DiscoveryTrigger::Login, target: Some(target) });
        c
    }

    /// One row per [`IncidentKind`] arm (plus the discovery target/link split), so a new cause
    /// added to the enum without a row here fails loudly rather than silently inheriting whatever
    /// arm happened to be last.
    #[test]
    fn every_incident_kind_maps_to_the_owner_approved_glyph() {
        assert_eq!(ctx(IncidentKind::PinExpired).readout_glyph(), Icon::ClockBadgeAlert);
        assert_eq!(ctx(IncidentKind::LinkStalled).readout_glyph(), Icon::ClockBadgeAlert);
        // `PinCreate` follows the SAME plex.tv `LinkClass` table as the Silent/PlexTv discovery
        // arm below — `mint_pin` (`auth.rs`) keeps its last response in `self.link` too, so an
        // HTTP error or a malformed 2xx from plex.tv must not read as "no connection" (`WifiSlash`)
        // — only its fallback (no specific link recorded) does.
        assert_eq!(with_link(ctx(IncidentKind::PinCreate), LinkClass::Dns).readout_glyph(), Icon::GlobeBadgeQuestion);
        assert_eq!(with_link(ctx(IncidentKind::PinCreate), LinkClass::Tls).readout_glyph(), Icon::LockBadgeAlert);
        for answered in [
            LinkClass::Answered2xx,
            LinkClass::Answered4xx,
            LinkClass::Answered5xx,
            LinkClass::AnsweredOther,
        ] {
            assert_eq!(with_link(ctx(IncidentKind::PinCreate), answered).readout_glyph(), Icon::CloudBadgeAlert,
                "PinCreate with link={answered:?} must not read as no-connection");
        }
        for other in [LinkClass::Timeout, LinkClass::TransportOther, LinkClass::Unknown] {
            assert_eq!(with_link(ctx(IncidentKind::PinCreate), other).readout_glyph(), Icon::WifiSlash);
        }
        // No link recorded at all (the context's own default) is the genuine "never got to ask" case.
        assert_eq!(ctx(IncidentKind::PinCreate).readout_glyph(), Icon::WifiSlash);
        assert_eq!(ctx(IncidentKind::Authorization).readout_glyph(), Icon::PersonBadgeXmark);
        assert_eq!(
            ctx(IncidentKind::Discovery(DiscoveryClass::NoServers)).readout_glyph(),
            Icon::ServerBadgePlus
        );
        assert_eq!(
            ctx(IncidentKind::Discovery(DiscoveryClass::Refused)).readout_glyph(),
            Icon::ServerBadgeXmark
        );
        assert_eq!(
            ctx(IncidentKind::Discovery(DiscoveryClass::InsecureOnly)).readout_glyph(),
            Icon::ServerBadgeMinus
        );
        assert_eq!(
            with_target(ctx(IncidentKind::Discovery(DiscoveryClass::Silent)), DiscoveryTarget::Servers)
                .readout_glyph(),
            Icon::ServerBadgeMinus
        );
        let plextv = with_target(ctx(IncidentKind::Discovery(DiscoveryClass::Silent)), DiscoveryTarget::PlexTv);
        assert_eq!(with_link(plextv, LinkClass::Dns).readout_glyph(), Icon::GlobeBadgeQuestion);
        assert_eq!(with_link(plextv, LinkClass::Tls).readout_glyph(), Icon::LockBadgeAlert);
        for answered in [
            LinkClass::Answered2xx,
            LinkClass::Answered4xx,
            LinkClass::Answered5xx,
            LinkClass::AnsweredOther,
        ] {
            assert_eq!(with_link(plextv, answered).readout_glyph(), Icon::CloudBadgeAlert);
        }
        for other in [LinkClass::Timeout, LinkClass::TransportOther, LinkClass::Unknown] {
            assert_eq!(with_link(plextv, other).readout_glyph(), Icon::GlobeBadgeMinus);
        }
        // No discovery target recorded at all (defensive: the field is `Option`) reads the same
        // as `PlexTv` — the fallback branch above, exercised here with the default `Unknown` link.
        assert_eq!(
            ctx(IncidentKind::Discovery(DiscoveryClass::Silent)).readout_glyph(),
            Icon::GlobeBadgeMinus
        );
        assert_eq!(ctx(IncidentKind::SaveFailed).readout_glyph(), Icon::KeyBadgeAlert);
        assert_eq!(ctx(IncidentKind::StoredLocked).readout_glyph(), Icon::KeyBadgeAlert);
        assert_eq!(ctx(IncidentKind::ProfileSwitch).readout_glyph(), Icon::PeopleBadgeAlert);
        assert_eq!(
            ctx(IncidentKind::ContentLoad(ContentSource::Home)).readout_glyph(),
            Icon::ServerBadgeMinus
        );
        assert_eq!(
            ctx(IncidentKind::ContentLoad(ContentSource::Libraries)).readout_glyph(),
            Icon::ServerBadgeMinus
        );
        for internal in [
            InternalClass::AdmissionRefused,
            InternalClass::WorkerRefused,
            InternalClass::WorkerDropped,
            InternalClass::CommitRefused,
            InternalClass::ClientIdUnavailable,
            InternalClass::Exhausted,
        ] {
            assert_eq!(ctx(IncidentKind::Internal(internal)).readout_glyph(), Icon::CloudBadgeAlert);
        }
    }
}

/// Which of the two ways out produced a report — see the module doc. Tagged on every body so the
/// two are separable in Sentry although they share a schema.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum ConsentKind {
    Standing,
    OneOff,
}

impl ConsentKind {
    fn code(self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::OneOff => "one_off",
        }
    }
}

/// Pure body builder, shaped like `playback::event_body`: `dist` and `errors_id` are passed in so
/// the consent preview exercises this exact serialiser without reading `/proc/self/exe` or minting
/// an id before consent. `errors_id` is attached as `user.id` through the one shared
/// [`super::sentry::attach_user`]; a one-off report passes `None` and has no `user` key at all.
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    ctx: IncidentContext,
    consent: ConsentKind,
) -> Value {
    let kind = ctx.kind.code();
    let link = ctx.link.code();
    let mut incident = serde_json::json!({
        "type": "incident",
        "kind": kind,
        "link": link,
        "unanswered": ctx.unanswered.code(),
        "failing_for": ctx.failing_for.code(),
        "consent": consent.code(),
    });
    if let Some(status) = ctx.http_status {
        incident["http_status"] = Value::from(status);
    }
    if let Some(rc) = ctx.curl_rc {
        incident["curl_rc"] = Value::from(rc);
    }
    if let Some(generation) = ctx.code_generation {
        incident["code_generation"] = Value::from(generation);
    }
    if let Some(class) = ctx.persistence {
        incident["persistence"] = Value::from(class.code());
    }
    if let Some(stage) = ctx.keymanager_stage {
        incident["keymanager_stage"] = Value::from(keymanager_stage_code(stage));
    }
    if let Some(code) = ctx.service_error_code {
        incident["service_error_code"] = Value::from(code);
    }
    if let Some(e) = ctx.insecure {
        incident["https_lan"] = Value::from(e.https.lan_plex_direct.code());
        incident["https_public"] = Value::from(e.https.public_plex_direct.code());
        incident["https_custom"] = Value::from(e.https.custom_https.code());
        incident["https_relay"] = Value::from(e.https.relay.code());
        incident["plaintext_local"] = Value::from(e.plaintext_local);
        incident["public_address_matches"] = Value::from(e.public_address_matches);
        incident["owned"] = Value::from(e.owned);
        incident["https_required"] = Value::from(e.https_required);
        incident["plaintext_scope"] = Value::from(e.plaintext_scope.code());
        incident["plaintext_family"] = Value::from(e.plaintext_family.code());
        incident["plaintext_eligibility"] = Value::from(e.plaintext_eligibility().code());
    }
    if let Some(outcome) = ctx.plaintext_consent {
        incident["plaintext_consent"] = Value::from(outcome.code());
    }
    if let Some(e) = ctx.no_servers {
        incident["resources"] = Value::from(e.resources.code());
        incident["discovery_trigger"] = Value::from(e.trigger.code());
    }
    if let Some(e) = ctx.discovery {
        incident["discovery_trigger"] = Value::from(e.trigger.code());
        if let Some(target) = e.target {
            incident["discovery_target"] = Value::from(target.code());
        }
    }
    if let Some(helper) = ctx.helper {
        incident["helper"] = serde_json::to_value(helper).expect("closed helper evidence");
        incident["candidate_errnos"] = serde_json::to_value(ctx.candidate_errnos).expect("errno array");
    }
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": env!("PLX_VERSION")},
        "logger": "onboarding",
        "transaction": "onboarding",
        "culprit": format!("onboarding::{kind}"),
        "fingerprint": ["onboarding", kind, link],
        "exception": {"values": [{
            "type": "OnboardingError",
            "value": kind,
            "mechanism": {"type": "onboarding", "handled": true},
        }]},
        "tags": {
            "incident.kind": kind,
            "incident.link": link,
            "incident.consent": consent.code(),
        },
        "contexts": {"incident": incident},
    });
    if !dist.is_empty() {
        body["dist"] = Value::String(dist.to_string());
    }
    // Omitted rather than sent as 1970: a fabricated occurrence time is worse than Sentry's own
    // receipt time, which is what an absent `timestamp` falls back to.
    if ctx.occurred_at_ms > 0 {
        body["timestamp"] = Value::from(ctx.occurred_at_ms as f64 / 1000.0);
    }
    super::sentry::attach_user(&mut body, errors_id);
    body
}

/// The storage-failure fields as one short, fixed-shape line — the screen's projection of the
/// SAME typed evidence [`event_body`] serialises into `contexts.incident.persistence` /
/// `.keymanager_stage` / `.service_error_code`. `screens::login::support_line` appends this to the
/// failure read-out's Details card, the one place a locked-out person can photograph it without a
/// shell.
///
/// Every one of the three is independently absent-or-present, so the line always names all three
/// slots and never grows or shrinks with what happened to be captured: `None` renders as the fixed
/// `unknown` class, exactly like every other closed value in this module, rather than being
/// omitted — an omission would itself be a signal, and a signal is exactly what an *unexpected*
/// input must never become able to send. `ctx: None` (no evidence was ever retained for this
/// incident — a declined report keeps nothing, see `auth::owner::incident`) renders identically to
/// evidence that carries none of the three: `unknown` all the way across either way.
pub(crate) fn storage_evidence_line(ctx: Option<&IncidentContext>) -> String {
    let persistence = ctx.and_then(|c| c.persistence).map_or("unknown", PersistenceFailure::code);
    let keymanager_stage =
        ctx.and_then(|c| c.keymanager_stage).map_or("unknown", keymanager_stage_code);
    let service_error_code = ctx
        .and_then(|c| c.service_error_code)
        .map_or_else(|| "unknown".to_string(), |code| code.to_string());
    format!("persistence:{persistence} keymgr:{keymanager_stage} svc:{service_error_code}")
}

fn granted() -> bool {
    consent::report_permission_now(ONBOARDING_REPORT_SCOPE) == Permission::Granted
}

/// Queue a STANDING report and ask the background sender to flush it; no network work on the
/// calling thread. Only for a person whose error reports are on at [`ONBOARDING_REPORT_SCOPE`],
/// and never for a stalled wait ([`IncidentKind::standing_eligible`]). The permission is read
/// again under the spool lock, exactly as `playback::report_error` re-reads its gate, so a
/// withdrawal racing this call either refuses the record or purges it.
///
/// Returns the queued report's event id — the Report ID the screen shows, as a one-off's is —
/// or `None` when nothing was queued. Queued is not delivered: `telemetry::delivery::state` says
/// what became of it, and the session adapter watches that for both lanes.
pub(crate) fn report_standing(ctx: IncidentContext) -> Option<String> {
    if !ctx.kind.standing_eligible() || !granted() || !super::sender::has_sentry() {
        return None;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::eventlog::log("telemetry: no /dev/urandom — onboarding incident was not queued");
        return None;
    };
    let body = event_body(
        &event_id,
        super::sentry::build_id(),
        consent::errors_id().as_deref(),
        ctx,
        ConsentKind::Standing,
    );
    let record = super::queue::Record {
        category: super::queue::Category::Errors,
        dest: super::queue::Dest::Sentry,
        event_id: event_id.clone(),
        body: serde_json::to_vec(&body).unwrap_or_default(),
    };
    let queued = queue_standing(&record, granted);
    if queued {
        super::flush_soon();
    }
    queued.then_some(event_id)
}

/// Append a standing report to the durable spool while `allowed` still holds, and watch its
/// Report ID from that moment (`super::delivery`) — the flush settles it from there.
fn queue_standing(record: &super::queue::Record, allowed: impl FnOnce() -> bool) -> bool {
    let tenure = super::delivery::tenure();
    match super::spool::append_watched_if(record, tenure, allowed) {
        Some(true) => true,
        Some(false) => {
            crate::eventlog::log("telemetry: onboarding incident did not fit the durable spool");
            false
        }
        None => false, // consent/tenure changed, or no delivery watch could be admitted
    }
}

/// **A ONE-OFF report, consented by exactly one explicit press.**
///
/// Call this ONLY from a person's tap on Send report — the incident alert or its Details. That
/// press is the whole of this report's consent: no standing decision is read or recorded, no
/// identifier is attached (no crash-report id, no analytics id), and a later consent change does
/// not withdraw it (`queue::Category::OneOff`). The only gate is that this build has a Sentry
/// endpoint at all. Sign-out and Delete all local data still erase it while it is queued.
///
/// Returns the report's event id once the one-off lane accepted it — durable spool or its bounded
/// direct fallback — as the receipt the person can quote; `telemetry::delivery::state`
/// says what became of it, and the session adapter watches that so the offer moves on to
/// delivered, saved for later, or — a report that did not get through after all — re-sendable
/// (`auth::owner::IncidentDelivery`).
pub(crate) fn send_one_off(ctx: IncidentContext) -> Option<String> {
    if !super::sender::has_sentry() {
        return None;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::eventlog::log("telemetry: no /dev/urandom — one-off onboarding report was not queued");
        return None;
    };
    let body = event_body(&event_id, super::sentry::build_id(), None, ctx, ConsentKind::OneOff);
    let record = super::queue::Record {
        category: super::queue::Category::OneOff,
        dest: super::queue::Dest::Sentry,
        event_id: event_id.clone(),
        body: serde_json::to_vec(&body).unwrap_or_default(),
    };
    super::oneoff::submit(record).then_some(event_id)
}

/// Representative incident payload built through the real serialiser, for the consent screen's
/// example. Shows the STANDING form — the report the crash-report question actually asks about —
/// with a per-report placeholder for the random and runtime values. No identifier is minted.
pub(crate) fn preview_event() -> Vec<u8> {
    let ctx = IncidentContext {
        occurred_at_ms: 0,
        ..IncidentContext::new(
            IncidentKind::PinCreate,
            Some(Err(RequestFailure {
                cause: RequestError::Transport,
                status: None,
                body_limit: None,
                curl_rc: Some(6),
            })),
        )
        .with_link_state(1, Some(std::time::Duration::from_secs(4)), 1)
    };
    serde_json::to_vec(&event_body(
        "<random per-error event id>",
        "<running ELF build id>",
        Some(super::native::PREVIEW_USER_ID),
        ctx,
        ConsentKind::Standing,
    ))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn failure(cause: RequestError, status: Option<u16>, curl_rc: Option<i32>) -> RequestFailure {
        RequestFailure { cause, status, body_limit: None, curl_rc }
    }

    #[test]
    fn helper_failure_report_has_closed_stage_and_candidate_errnos() {
        use crate::storage::wire::failure::{HelperFailure, Stage};
        let mut failure = HelperFailure::new(Stage::Connect, Some(libc::ECONNREFUSED));
        failure.activation = Some(crate::storage::wire::failure::Detail::new(Stage::ActivationRegister, Some(-13)));
        let mut errnos = [None; 8];
        errnos[0] = Some(libc::EACCES);
        let context = IncidentContext::new(IncidentKind::SaveFailed, None)
            .with_persistence(&CompletionOutcome::Failed(Failure::Helper(failure, errnos)));
        let body = event_body(&"a".repeat(32), "", None, context, ConsentKind::OneOff);
        assert_eq!(body["contexts"]["incident"]["helper"]["observed"]["stage"], "connect");
        assert_eq!(body["contexts"]["incident"]["helper"]["activation"]["stage"], "activation-register");
        assert_eq!(body["contexts"]["incident"]["helper"]["activation"]["code"], -13);
        assert_eq!(body["contexts"]["incident"]["candidate_errnos"][0], libc::EACCES);
        assert!(body.get("user").is_none());
        let text = body.to_string();
        for private in ["token", "hostname", "path", "nonce", "uid", "pid"] {
            assert!(!text.contains(private), "private slot: {private}");
        }
    }

    #[test]
    fn bucket_edges() {
        assert_eq!(UnansweredBucket::from_count(0), UnansweredBucket::Zero);
        assert_eq!(UnansweredBucket::from_count(1), UnansweredBucket::One);
        assert_eq!(UnansweredBucket::from_count(2), UnansweredBucket::TwoToFive);
        assert_eq!(UnansweredBucket::from_count(5), UnansweredBucket::TwoToFive);
        assert_eq!(UnansweredBucket::from_count(6), UnansweredBucket::SixPlus);
        let f = |s| FailingForBucket::from_duration(Some(Duration::from_secs(s)));
        assert_eq!(FailingForBucket::from_duration(None), FailingForBucket::None);
        assert_eq!(f(9), FailingForBucket::Under10s);
        assert_eq!(f(10), FailingForBucket::Under60s);
        assert_eq!(f(59), FailingForBucket::Under60s);
        assert_eq!(f(60), FailingForBucket::Under5m);
        assert_eq!(f(299), FailingForBucket::Under5m);
        assert_eq!(f(300), FailingForBucket::FiveMinPlus);
    }

    #[test]
    fn the_context_buckets_clamps_and_stamps() {
        let before = now_ms();
        let ctx = IncidentContext::new(IncidentKind::Authorization, Some(Ok(429)))
            .with_link_state(3, Some(Duration::from_secs(45)), 9);
        assert!(ctx.occurred_at_ms >= before && ctx.occurred_at_ms <= now_ms());
        assert_eq!((ctx.link, ctx.http_status, ctx.curl_rc), (LinkClass::Answered4xx, Some(429), None));
        assert_eq!(ctx.unanswered, UnansweredBucket::TwoToFive);
        assert_eq!(ctx.failing_for, FailingForBucket::Under60s);
        assert_eq!(ctx.code_generation, Some(4));
        let low = IncidentContext::new(IncidentKind::PinCreate, None).with_link_state(0, None, 0);
        assert_eq!(low.code_generation, Some(1));
    }

    fn every_kind() -> Vec<IncidentKind> {
        use ContentSource as C;
        use DiscoveryClass as D;
        vec![
            IncidentKind::PinCreate,
            IncidentKind::LinkStalled,
            IncidentKind::PinExpired,
            IncidentKind::Authorization,
            IncidentKind::Discovery(D::NoServers),
            IncidentKind::Discovery(D::Refused),
            IncidentKind::Discovery(D::Silent),
            IncidentKind::Discovery(D::InsecureOnly),
            IncidentKind::SaveFailed,
            IncidentKind::StoredLocked,
            IncidentKind::ProfileSwitch,
            IncidentKind::ContentLoad(C::Home),
            IncidentKind::ContentLoad(C::Libraries),
            IncidentKind::Internal(InternalClass::AdmissionRefused),
            IncidentKind::Internal(InternalClass::WorkerRefused),
            IncidentKind::Internal(InternalClass::WorkerDropped),
            IncidentKind::Internal(InternalClass::CommitRefused),
            IncidentKind::Internal(InternalClass::ClientIdUnavailable),
            IncidentKind::Internal(InternalClass::Exhausted),
        ]
    }

    /// Sentry groups on these codes: they must be distinct and wire-shaped.
    #[test]
    fn every_kind_has_a_distinct_stable_code_and_only_a_stall_is_offer_only() {
        let codes: Vec<_> = every_kind().into_iter().map(IncidentKind::code).collect();
        let mut unique = codes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), codes.len(), "duplicate code in {codes:?}");
        assert!(codes.iter().all(|c| c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')));
        let offer_only: Vec<_> = every_kind().into_iter().filter(|k| !k.standing_eligible()).collect();
        assert_eq!(offer_only, vec![IncidentKind::LinkStalled]);
        let details_only: Vec<_> = every_kind().into_iter().filter(|k| !k.alert_eligible()).collect();
        assert_eq!(details_only, vec![IncidentKind::LinkStalled]);
    }

    fn protection_failure(service_code: Option<i32>) -> crate::plex::session::persistence::ProtectionFailure {
        use crate::storage::wire::{
            AuthPreservation, ErrorCode, KeymanagerFailure, KeymanagerFailureCategory,
            KeymanagerOperation,
        };
        crate::plex::session::persistence::ProtectionFailure {
            failure: KeymanagerFailure {
                operation: KeymanagerOperation::Seal,
                stage: KeymanagerStage::Finish,
                code: ErrorCode::Unavailable,
                category: KeymanagerFailureCategory::ServiceRejected,
                service_code,
            },
            preservation: AuthPreservation::Unchanged,
            db8_commit_verified: false,
        }
    }

    #[test]
    fn a_persistence_completion_maps_to_its_closed_class() {
        use crate::plex::session::async_persistence::PersistOutcome;
        let ctx = || IncidentContext::new(IncidentKind::SaveFailed, None);
        let sealed = ctx().with_persistence(&CompletionOutcome::Failed(Failure::Protection(
            protection_failure(Some(-3961)),
        )));
        assert_eq!(sealed.persistence, Some(PersistenceFailure::Protection));
        assert_eq!(sealed.keymanager_stage, Some(KeymanagerStage::Finish));
        assert_eq!(sealed.service_error_code, Some(-3961));

        let uncertain = ctx().with_persistence(&CompletionOutcome::ProtectionUncertain(protection_failure(None)));
        assert_eq!(uncertain.persistence, Some(PersistenceFailure::ProtectionUncertain));
        assert_eq!(uncertain.service_error_code, None);

        let write = ctx().with_persistence(&CompletionOutcome::Failed(Failure::Persistence(
            PersistOutcome::WriteFailed,
        )));
        assert_eq!(write.persistence, Some(PersistenceFailure::WriteFailed));
        assert_eq!(write.keymanager_stage, None);
        assert_eq!(
            ctx().with_persistence(&CompletionOutcome::Uncertain {
                stage: crate::storage::CommitStage::Rename,
                errno: 5,
                helper: None,
            })
            .persistence,
            Some(PersistenceFailure::CommitUncertain)
        );
        assert_eq!(
            ctx().with_persistence(&CompletionOutcome::Failed(Failure::WorkerDropped)).persistence,
            Some(PersistenceFailure::WorkerDropped)
        );
        assert_eq!(
            ctx().with_persistence(&CompletionOutcome::Failed(Failure::Admission(
                crate::storage_worker::SubmitError::Full
            )))
            .persistence,
            Some(PersistenceFailure::Admission)
        );
        // Not failures: the context is untouched.
        assert_eq!(ctx().with_persistence(&CompletionOutcome::Superseded).persistence, None);
        assert_eq!(
            ctx().with_persistence(&CompletionOutcome::Failed(Failure::Superseded)).persistence,
            None
        );
    }

    fn keys(v: &Value) -> Vec<&str> {
        let mut out: Vec<_> = v.as_object().expect("object").keys().map(String::as_str).collect();
        out.sort_unstable();
        out
    }

    fn dns_context() -> IncidentContext {
        IncidentContext {
            occurred_at_ms: 1_725_000_000_000,
            ..IncidentContext::new(
                IncidentKind::PinCreate,
                Some(Err(failure(RequestError::Transport, None, Some(6)))),
            )
            .with_link_state(1, Some(Duration::from_secs(4)), 1)
        }
    }

    /// An insecure-only verdict's evidence, every route bucket distinct so a key cannot be read
    /// off the wrong field.
    fn insecure_context() -> IncidentContext {
        use crate::plex::probe::{AddressFamily, AddressScope, HttpsRoutes, InsecureEvidence, RouteOutcome};
        IncidentContext::new(IncidentKind::Discovery(DiscoveryClass::InsecureOnly), None)
            .with_insecure(InsecureEvidence {
                https: HttpsRoutes {
                    lan_plex_direct: RouteOutcome::Tls,
                    public_plex_direct: RouteOutcome::Timeout,
                    custom_https: RouteOutcome::Absent,
                    relay: RouteOutcome::Dns,
                },
                plaintext_local: true,
                public_address_matches: true,
                owned: true,
                https_required: false,
                plaintext_scope: AddressScope::Private,
                plaintext_family: AddressFamily::V4,
                identity_verified: true,
            })
    }

    fn no_servers_context() -> IncidentContext {
        IncidentContext::new(IncidentKind::Discovery(DiscoveryClass::NoServers), None)
            .with_no_servers(NoServersEvidence {
                resources: CountBucket::TwoToFive,
                trigger: DiscoveryTrigger::Rediscover,
            })
    }

    #[test]
    fn persisted_no_servers_contexts_from_e437f264_and_79ef8433_still_round_trip() {
        const E437F264: &str = r#"{"kind":{"Discovery":"NoServers"},"link":"Unknown","http_status":null,"curl_rc":null,"unanswered":"Zero","failing_for":"None","code_generation":null,"persistence":null,"helper":null,"candidate_errnos":[null,null,null,null,null,null,null,null],"keymanager_stage":null,"service_error_code":null,"insecure":null,"plaintext_consent":null,"no_servers":{"resources":"TwoToFive","trigger":"Rediscover"},"occurred_at_ms":1}"#;
        const N79EF8433: &str = r#"{"kind":{"Discovery":"NoServers"},"link":"Unknown","http_status":null,"curl_rc":null,"unanswered":"Zero","failing_for":"None","code_generation":null,"persistence":null,"helper":null,"candidate_errnos":[null,null,null,null,null,null,null,null],"keymanager_stage":null,"service_error_code":null,"insecure":null,"plaintext_consent":null,"no_servers":{"resources":"One","trigger":"Login"},"occurred_at_ms":2}"#;
        for fixture in [E437F264, N79EF8433] {
            let context: IncidentContext = serde_json::from_str(fixture)
                .expect("a context persisted by the parent shape must deserialize");
            assert_eq!(serde_json::to_string(&context).unwrap(), fixture,
                "new optional discovery fields must not alter legacy NoServers JSON");
            assert!(context.discovery.is_none());
            assert!(context.discovery_attempts.is_none());
        }
    }

    /// `PRIVACY.md` names every key a sign-in report's `incident` context can carry, so a key
    /// added to [`event_body`] without its line in the notice fails here, not in review.
    #[test]
    fn privacy_notice_names_every_sign_in_incident_key() {
        let notice = include_str!("../../../PRIVACY.md");
        // A class carries the status OR the CURLcode, never both; the notice names both.
        let mut ctx = IncidentContext::new(IncidentKind::Authorization, Some(Ok(503)))
            .with_link_state(2, Some(Duration::from_secs(9)), 3);
        ctx.curl_rc = Some(7);
        ctx.code_generation = Some(2);
        let v = event_body("a", "", None, ctx, ConsentKind::OneOff);
        for key in keys(&v["contexts"]["incident"]) {
            if key == "type" {
                continue;
            }
            assert!(notice.contains(&format!("`{key}`")), "PRIVACY.md does not name `{key}`");
        }
        // The discovery evidence ships only on its two kinds; walk both.
        for ctx in [
            insecure_context().with_plaintext_consent(Some(PlaintextConsentOutcome::Offered)),
            no_servers_context(),
        ] {
            let v = event_body("a", "", None, ctx, ConsentKind::OneOff);
            for key in keys(&v["contexts"]["incident"]) {
                assert!(key == "type" || notice.contains(&format!("`{key}`")), "PRIVACY.md does not name `{key}`");
            }
        }
        // The save-failure trio ships too, but `dns_context`-shaped fixtures above never set it —
        // a context that HAS gone through `with_persistence` is the one that would have caught
        // `persistence`/`keymanager_stage`/`service_error_code` missing from the notice.
        let save_ctx = IncidentContext::new(IncidentKind::SaveFailed, None)
            .with_persistence(&CompletionOutcome::Failed(Failure::Protection(protection_failure(Some(-3961)))));
        let save_v = event_body("a", "", None, save_ctx, ConsentKind::OneOff);
        for key in keys(&save_v["contexts"]["incident"]) {
            if key == "type" {
                continue;
            }
            assert!(notice.contains(&format!("`{key}`")), "PRIVACY.md does not name `{key}`");
        }
    }

    #[test]
    fn event_body_keys_are_exact() {
        let v = event_body(&"a".repeat(32), "0123456789abcdef", Some(&"e".repeat(32)), dns_context(), ConsentKind::Standing);
        assert_eq!(
            keys(&v),
            [
                "contexts", "culprit", "dist", "environment", "event_id", "exception",
                "fingerprint", "level", "logger", "platform", "release", "sdk", "tags",
                "timestamp", "transaction", "user",
            ]
        );
        assert_eq!(keys(&v["contexts"]), ["incident"]);
        assert_eq!(
            keys(&v["contexts"]["incident"]),
            ["code_generation", "consent", "curl_rc", "failing_for", "kind", "link", "type", "unanswered"]
        );
        assert_eq!(keys(&v["tags"]), ["incident.consent", "incident.kind", "incident.link"]);
        assert_eq!(v["exception"]["values"][0]["type"], "OnboardingError");
        assert_eq!(v["exception"]["values"][0]["mechanism"]["handled"], true);
        assert_eq!(v["fingerprint"], serde_json::json!(["onboarding", "pin_create", "dns"]));
        assert_eq!(v["tags"]["incident.consent"], "standing");
        assert_eq!(v["timestamp"], 1_725_000_000.0);

        // A one-off body has the same shape less the `user`, and says so everywhere.
        let one_off = event_body(&"a".repeat(32), "", None, dns_context(), ConsentKind::OneOff);
        assert!(one_off.get("user").is_none(), "a one-off report carried a user: {one_off}");
        assert!(one_off.get("dist").is_none());
        assert_eq!(one_off["contexts"]["incident"]["consent"], "one_off");
        assert_eq!(one_off["tags"]["incident.consent"], "one_off");

        // Persistence evidence adds exactly its three keys.
        let save = IncidentContext::new(IncidentKind::SaveFailed, None).with_persistence(
            &CompletionOutcome::Failed(Failure::Protection(protection_failure(Some(-3961)))),
        );
        let v = event_body("a", "", None, save, ConsentKind::OneOff);
        assert_eq!(
            keys(&v["contexts"]["incident"]),
            [
                "consent", "failing_for", "keymanager_stage", "kind", "link", "persistence",
                "service_error_code", "type", "unanswered",
            ]
        );
        assert_eq!(v["contexts"]["incident"]["keymanager_stage"], "finish");

        let discovery = IncidentContext::new(IncidentKind::Discovery(DiscoveryClass::Silent),
            Some(Err(RequestFailure { cause: RequestError::Transport, status: None,
                body_limit: None, curl_rc: Some(6) })))
            .with_discovery(DiscoveryEvidence { trigger: DiscoveryTrigger::Login,
                target: Some(DiscoveryTarget::PlexTv) })
            .with_retry_run(3, Duration::from_secs(6));
        let v = event_body("a", "", None, discovery, ConsentKind::OneOff);
        assert_eq!(keys(&v["contexts"]["incident"]), [
            "consent", "curl_rc", "discovery_target", "discovery_trigger", "failing_for",
            "kind", "link", "type", "unanswered",
        ]);
        assert_eq!(v["contexts"]["incident"]["discovery_target"], "plex_tv");
        assert_eq!(v["contexts"]["incident"]["discovery_trigger"], "login");
        assert_eq!(v["contexts"]["incident"]["unanswered"], "two_to_five");
        assert_eq!(v["contexts"]["incident"]["failing_for"], "under_10s");
        assert!(v["contexts"]["incident"].get("code_generation").is_none());

        // An insecure-only verdict adds exactly its route buckets and plaintext facts.
        let v = event_body("a", "", None, insecure_context(), ConsentKind::OneOff);
        assert_eq!(
            keys(&v["contexts"]["incident"]),
            [
                "consent", "failing_for", "https_custom", "https_lan", "https_public", "https_relay",
                "https_required", "kind", "link", "owned", "plaintext_eligibility", "plaintext_family",
                "plaintext_local", "plaintext_scope", "public_address_matches", "type", "unanswered",
            ]
        );
        // …and an offered consent question adds exactly its closed outcome.
        let offered = insecure_context().with_plaintext_consent(Some(PlaintextConsentOutcome::Declined));
        let with_consent = event_body("a", "", None, offered, ConsentKind::OneOff);
        let mut want = keys(&v["contexts"]["incident"]);
        want.push("plaintext_consent");
        want.sort_unstable();
        assert_eq!(keys(&with_consent["contexts"]["incident"]), want);
        assert_eq!(with_consent["contexts"]["incident"]["plaintext_consent"], "declined");
        let incident = &v["contexts"]["incident"];
        assert_eq!(
            [&incident["https_lan"], &incident["https_public"], &incident["https_custom"], &incident["https_relay"]],
            ["tls", "timeout", "absent", "dns"]
        );
        assert_eq!(incident["plaintext_scope"], "private");
        assert_eq!(incident["plaintext_family"], "v4");
        assert_eq!(incident["plaintext_local"], true);
        assert_eq!(incident["https_required"], false);

        // A no-servers verdict adds exactly its resource bucket and trigger.
        let v = event_body("a", "", None, no_servers_context(), ConsentKind::OneOff);
        assert_eq!(
            keys(&v["contexts"]["incident"]),
            ["consent", "discovery_trigger", "failing_for", "kind", "link", "resources", "type", "unanswered"]
        );
        assert_eq!(v["contexts"]["incident"]["resources"], "two_to_five");
        assert_eq!(v["contexts"]["incident"]["discovery_trigger"], "rediscover");
    }

    #[test]
    fn the_incident_context_carries_http_status_xor_curl_rc() {
        let answered = event_body("a", "", None, IncidentContext::new(IncidentKind::Authorization, Some(Ok(429))), ConsentKind::OneOff);
        assert_eq!(answered["contexts"]["incident"]["http_status"], 429);
        assert!(answered["contexts"]["incident"].get("curl_rc").is_none());
        let transport = event_body("a", "", None, dns_context(), ConsentKind::OneOff);
        assert_eq!(transport["contexts"]["incident"]["curl_rc"], 6);
        assert!(transport["contexts"]["incident"].get("http_status").is_none());
        let unknown = event_body("a", "", None, IncidentContext::new(IncidentKind::StoredLocked, None), ConsentKind::OneOff);
        assert!(unknown["contexts"]["incident"].get("http_status").is_none());
        assert!(unknown["contexts"]["incident"].get("curl_rc").is_none());
    }

    #[test]
    fn a_zero_occurred_at_produces_no_timestamp_key() {
        let ctx = IncidentContext { occurred_at_ms: 0, ..dns_context() };
        let v = event_body("a", "", None, ctx, ConsentKind::OneOff);
        assert!(v.get("timestamp").is_none(), "{v}");
    }

    /// **An absent value cannot widen the screen's line — it can only ever render as the fixed
    /// `unknown` class.** `storage_evidence_line` is what `screens::login::support_line` appends
    /// to the failure read-out's Details card, the one place a locked-out person without a shell
    /// can read (and photograph) this evidence; its shape must not change with what it is fed.
    ///
    /// No context at all (a Declined offer keeps none — `auth::owner::incident`'s doc) renders
    /// byte-for-byte identically to evidence that carries none of the three fields, and evidence
    /// that carries every field never produces anything longer than three short `key:value`
    /// tokens — never a raw string, a path or anything the value's own `Display` did not put
    /// there.
    #[test]
    fn storage_evidence_line_never_widens_on_an_absent_or_unexpected_input() {
        let unknown_all = "persistence:unknown keymgr:unknown svc:unknown";
        assert_eq!(storage_evidence_line(None), unknown_all);
        assert_eq!(
            storage_evidence_line(Some(&IncidentContext::new(IncidentKind::SaveFailed, None))),
            unknown_all,
            "evidence with none of the three fields set must read exactly like no evidence at all"
        );

        let full = IncidentContext::new(IncidentKind::SaveFailed, None)
            .with_persistence(&CompletionOutcome::Failed(Failure::Protection(protection_failure(Some(-3961)))));
        assert_eq!(
            storage_evidence_line(Some(&full)),
            "persistence:protection keymgr:finish svc:-3961"
        );

        // A service code at the extremes of `i32` is still ONLY its `Display`, never anything
        // that could grow the line's shape (no separators, no extra tokens, no text run in).
        for extreme in [i32::MIN, i32::MAX, 0] {
            let ctx = IncidentContext {
                persistence: Some(PersistenceFailure::Protection),
                keymanager_stage: Some(KeymanagerStage::Finish),
                service_error_code: Some(extreme),
                ..IncidentContext::new(IncidentKind::SaveFailed, None)
            };
            let line = storage_evidence_line(Some(&ctx));
            assert_eq!(line, format!("persistence:protection keymgr:finish svc:{extreme}"));
            assert!(
                line.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b' ' | b'_' | b'-')),
                "{line:?} contains a character no typed class or numeric code could produce"
            );
        }
    }

    /// **No identity or content slot, in any body shape.** Every key at every depth is walked
    /// against the names a title, a server, an account or a free-text error would arrive under,
    /// and every string in the incident context is a closed code — not a single one is free text.
    #[test]
    fn every_body_is_closed_and_carries_no_identity_or_content_slot() {
        fn walk<'a>(v: &'a Value, keys: &mut Vec<&'a str>) {
            match v {
                Value::Object(m) => m.iter().for_each(|(k, v)| {
                    keys.push(k);
                    walk(v, keys);
                }),
                Value::Array(a) => a.iter().for_each(|v| walk(v, keys)),
                _ => {}
            }
        }
        let mut bodies = vec![serde_json::from_slice::<Value>(&preview_event()).expect("preview JSON")];
        for kind in every_kind() {
            for consent in [ConsentKind::Standing, ConsentKind::OneOff] {
                let errors_id = (consent == ConsentKind::Standing).then(|| "e".repeat(32));
                let ctx = IncidentContext { kind, ..dns_context() }.with_persistence(
                    &CompletionOutcome::Failed(Failure::Protection(protection_failure(Some(-1)))),
                );
                bodies.push(event_body("a", "b", errors_id.as_deref(), ctx, consent));
            }
        }
        // The discovery evidence rides only on its own kinds, so walk it where it is carried.
        let silent = IncidentContext::new(IncidentKind::Discovery(DiscoveryClass::Silent),
            Some(Err(RequestFailure { cause: RequestError::Transport, status: None,
                body_limit: None, curl_rc: Some(6) })))
            .with_discovery(DiscoveryEvidence { trigger: DiscoveryTrigger::Login,
                target: Some(DiscoveryTarget::PlexTv) })
            .with_retry_run(3, Duration::from_secs(6));
        for ctx in [insecure_context(), no_servers_context(), silent] {
            bodies.push(event_body("a", "b", Some(&"e".repeat(32)), ctx, ConsentKind::Standing));
            bodies.push(event_body("a", "b", None, ctx, ConsentKind::OneOff));
        }
        for v in &bodies {
            let mut all = Vec::new();
            walk(v, &mut all);
            for forbidden in [
                "title", "profile", "server", "account", "pin", "code", "token", "url",
                "path", "host", "hostname", "address", "email", "username", "ip_address",
                "request", "error", "message", "caption", "detail", "text", "breadcrumbs",
            ] {
                assert!(!all.contains(&forbidden), "forbidden key {forbidden}: {all:?}");
            }
            for (key, value) in v["contexts"]["incident"].as_object().expect("incident") {
                if let Value::String(s) = value {
                    assert!(
                        s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
                        "{key} = {s:?} is not a closed code"
                    );
                }
            }
            match v.get("user") {
                None => assert_eq!(v["contexts"]["incident"]["consent"], "one_off"),
                Some(user) => {
                    assert_eq!(v["contexts"]["incident"]["consent"], "standing");
                    assert_eq!(keys(user), ["id"], "the one identity slot is user.id alone");
                }
            }
        }
    }

    /// **(d) A standing report is watched from the moment the spool takes it** — its Report ID
    /// reads Queued until the flush settles it, exactly as a one-off's does — and one the spool
    /// refused is not watched at all.
    #[test]
    fn a_queued_standing_report_is_watched_from_the_moment_the_spool_takes_it() {
        use super::super::delivery::{self, DeliveryState};
        let _g = crate::testlock::serial();
        delivery::forget();
        let dir = std::env::temp_dir().join(format!("plxnative-standing-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::super::spool::set_test_path(Some(dir.join("spool.bin")));
        let record = |id: &str| super::super::queue::Record {
            category: super::super::queue::Category::Errors,
            dest: super::super::queue::Dest::Sentry,
            event_id: id.into(),
            body: b"{}".to_vec(),
        };
        let queued = queue_standing(&record("standing-1"), || true);
        let refused = queue_standing(&record("standing-2"), || false);
        let states = (delivery::state("standing-1"), delivery::state("standing-2"));
        super::super::spool::set_test_path(None);
        let _ = std::fs::remove_dir_all(&dir);
        delivery::forget();
        assert!(queued && !refused);
        assert_eq!(states, (Some(DeliveryState::Queued), None));
    }

    #[test]
    fn completion_before_append_returns_is_not_lost() {
        use super::super::{delivery, spool};
        use delivery::DeliveryState;
        let _g = crate::testlock::serial();
        delivery::forget();
        let dir = std::env::temp_dir().join(format!("plxnative-standing-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        spool::set_test_path(Some(dir.join("spool.bin")));
        let record = super::super::queue::Record {
            category: super::super::queue::Category::Errors,
            dest: super::super::queue::Dest::Sentry,
            event_id: "standing-race".into(), body: b"body".to_vec(),
        };
        spool::on_append_for_test(|| {
            let records = spool::read();
            assert_eq!(records.len(), 1);
            let consent = consent::Consent {
                asked_version: consent::POLICY_VERSION, errors: true, ..Default::default()
            };
            let (retired, _) = super::super::process_records(
                &records, &consent, || true, |_| (super::super::sender::Verdict::Done, None),
            );
            spool::commit_retiring(&retired);
        });
        let queued = queue_standing(&record, || true);
        let state = delivery::state(&record.event_id);
        spool::set_test_path(None);
        delivery::forget();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(queued);
        assert_eq!(state, Some(DeliveryState::Delivered), "the ordinary flush settled before queue_standing resumed");
    }

    /// A stalled wait is never sent without asking, even for a person who granted the scope; and
    /// nobody below the scope gets a standing report at all. In a checkout with no Sentry DSN both
    /// calls refuse for that reason too, so the table half is graded where it is decided —
    /// `consent::tests::report_permission_follows_the_accepted_and_declined_scope`.
    #[test]
    fn a_standing_report_needs_the_scope_and_never_carries_a_stall() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        let dir = std::env::temp_dir().join(format!("plxnative-incident-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        super::super::spool::set_test_path(Some(dir.join("spool.bin")));

        let yes_at_4 = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            errors_id: Some("e".repeat(32)),
            errors_scope: 4,
            ..Default::default()
        };
        consent::install(yes_at_4);
        let refused_below_scope = report_standing(dns_context()).is_none();

        consent::install(consent::apply(&consent::Consent::default(), true, false, || Some("e".repeat(32))));
        let stall = IncidentContext { kind: IncidentKind::LinkStalled, ..dns_context() };
        let refused_stall = report_standing(stall).is_none();
        let spooled = super::super::spool::read().len();

        super::super::spool::set_test_path(None);
        let _ = std::fs::remove_dir_all(&dir);
        if let Some(c) = saved {
            consent::install(c);
        }
        assert!(refused_below_scope, "a Yes at scope 4 sent an onboarding report without asking");
        assert!(refused_stall, "a stalled wait was sent without asking");
        assert_eq!(spooled, 0);
    }
}
