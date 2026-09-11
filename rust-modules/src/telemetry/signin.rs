//! Handled sign-in errors for Sentry — issue #75: telemetry is blind to a failed sign-in **by
//! construction**, because `app.rs::maybe_ask_consent` only asks the crash/analytics question
//! once an AUTHORIZED account exists, so `diag::event`'s consent gate drops `SignInStarted` and
//! `SignInCompleted` on every attempt that never reaches that question — including every one that
//! fails. A stranger whose television cannot sign in has therefore never sent a single
//! `signin.*` usage event, and PostHog has nothing to show for it. This is the SAME shape as
//! `telemetry::playback` (a closed handled-error report, spooled through the existing Errors/
//! Sentry lane and gated on the SAME consent question, so it needs no consent bump of its own for
//! the reporting mechanism — only for the fields it adds, see `consent::POLICY_VERSION`), built
//! because a report only reaches Sentry from a television that DID get as far as consenting to
//! error reports, which is a narrower population than "every failed sign-in" but is the only one
//! this app can ever ask.
//!
//! Every value accepted here is a closed enum built from `auth::LinkState` and `net::CallOutcome`
//! — there is no PIN, no code, no token, no account, no URL, no hostname (other than the literal
//! `plex.tv`, which never appears here since nothing here is free text). The HTTP status and the
//! raw curl return code are included, but only as the NUMBER libcurl itself reported — a status
//! code and an rc carry no identity, the same reasoning `net::describe_outcome` already relies on
//! for the on-screen sign-in diagnostic this module reuses classifications from.

use crate::net::CallOutcome;
use crate::telemetry::storage::SessionStorageClass;
use serde_json::Value;

/// Which stage of the sign-in flow the failure was reported from. Mirrors
/// `diag::schema::SignInFailure`'s closed codes exactly — a second enum rather than reusing that
/// one, because `SignInFailure::code` is private to `diag::schema` and this module needs its own
/// `code()` for the wire body below. `auth.rs::signin_error_context` maps `Ctl::phase` onto every
/// variant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SignInFailureKind {
    PinCreate,
    Authorization,
    Discovery,
    Other,
}

impl SignInFailureKind {
    fn code(self) -> &'static str {
        match self {
            Self::PinCreate => "pin_create",
            Self::Authorization => "authorization",
            Self::Discovery => "discovery",
            Self::Other => "other",
        }
    }
}

/// What the most recent plex.tv call actually did, coarsened from `net::CallOutcome` into the
/// class this report carries. An `Answered*` class always carries the exact status
/// ([`SignInErrorContext::http_status`]); `Dns`/`Tls`/`Timeout`/`TransportOther` always carry the
/// exact curl return code ([`SignInErrorContext::curl_rc`]) — `Timeout` reports libcurl's own 28
/// (`CURLE_OPERATION_TIMEDOUT`) even though `CallOutcome::TimedOut` itself carries no field, since
/// that is the one rc it is always standing in for. `Unknown` covers "no call has happened yet"
/// and "libcurl itself could not be loaded" alike — neither is a real transport failure with a
/// code worth naming.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LinkOutcomeClass {
    Answered2xx,
    Answered4xx,
    Answered5xx,
    AnsweredOther,
    Dns,
    Tls,
    Timeout,
    TransportOther,
    Unknown,
}

impl LinkOutcomeClass {
    fn code(self) -> &'static str {
        match self {
            Self::Answered2xx => "answered_2xx",
            Self::Answered4xx => "answered_4xx",
            Self::Answered5xx => "answered_5xx",
            Self::AnsweredOther => "answered_other",
            Self::Dns => "dns",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::TransportOther => "transport_other",
            Self::Unknown => "unknown",
        }
    }
}

/// How many consecutive plex.tv polls (or the pin-creation call that opens the flow) came back
/// with no usable answer, bucketed — never the raw count, which is unbounded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UnansweredBucket {
    Zero,
    One,
    TwoToFive,
    SixPlus,
}

impl UnansweredBucket {
    fn from_count(n: u32) -> Self {
        match n {
            0 => Self::Zero,
            1 => Self::One,
            2..=5 => Self::TwoToFive,
            _ => Self::SixPlus,
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::Zero => "zero",
            Self::One => "one",
            Self::TwoToFive => "two_to_five",
            Self::SixPlus => "six_plus",
        }
    }
}

/// How long the current run of misses has lasted, bucketed — never the raw duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailingForBucket {
    None,
    Under10s,
    Under60s,
    Under5m,
    FiveMinPlus,
}

impl FailingForBucket {
    fn from_duration(d: Option<std::time::Duration>) -> Self {
        let Some(d) = d else { return Self::None };
        let secs = d.as_secs();
        if secs < 10 {
            Self::Under10s
        } else if secs < 60 {
            Self::Under60s
        } else if secs < 300 {
            Self::Under5m
        } else {
            Self::FiveMinPlus
        }
    }
    fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Under10s => "under_10s",
            Self::Under60s => "under_60s",
            Self::Under5m => "under_5m",
            Self::FiveMinPlus => "five_min_plus",
        }
    }
}

/// Everything one handled sign-in error report carries. Every field is a closed enum, a bucket, or
/// — for `http_status`/`curl_rc` — a bare number with no identity of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SignInErrorContext {
    pub kind: SignInFailureKind,
    pub link: LinkOutcomeClass,
    /// The exact HTTP status — present only when `link` is one of the `Answered*` classes.
    pub http_status: Option<u16>,
    /// The exact `CURLcode` (28 stood in for `CallOutcome::TimedOut`) — present only when `link`
    /// is `Dns`, `Tls`, `Timeout` or `TransportOther`.
    pub curl_rc: Option<i32>,
    pub unanswered: UnansweredBucket,
    pub failing_for: FailingForBucket,
    /// Which automatic code the flow was on, 1..=4 — clamped, since this is a count of pins
    /// issued during one flow, not an open-ended value worth reporting exactly past that.
    pub code_generation: u8,
    /// issue #76: how this install's session file is protected at the moment the sign-in error was
    /// reported — the same closed vocabulary [`crate::diag::schema::UsageContext::session_storage`]
    /// rides. `auth.rs`'s `signin_error_context` reads the live value
    /// (`crate::plex::session::storage_class()`) and passes it in — collected by the CALLER before
    /// the `Ctl` lock is taken, since the read can do flash I/O and that lock is also taken by the
    /// render thread every frame.
    pub storage: SessionStorageClass,
    /// Unix-epoch milliseconds at the moment this context was BUILT (`context_from`), not at
    /// whichever later moment a report carrying it actually reaches the wire — the standing report
    /// can sit in the durable spool across a flush retry or a closed app, and the one-off report
    /// (`send_once`) is not sent until the person presses "Send report", which can be minutes after
    /// the failure it describes. Without this, Sentry stamps the event at RECEIPT time — exactly
    /// the gap the 2026-09-10 TV telemetry proof (scenario 4) found, where a held report's Sentry
    /// timestamp landed at the draining launch rather than the failing one.
    /// `storage::StorageErrorContext` carries the same fact as a separate `occurred_at_ms`
    /// parameter to its `event_body`, rather than a field on its context, because its context is
    /// `Copy` with no held-until-later reuse; this one IS reused later (`auth::Trouble::ctx`), so
    /// the timestamp has to travel with it.
    pub occurred_at_ms: u64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Coarsen one `net::CallOutcome` into the class this report carries, plus the fields that are
/// only meaningful for that class. PURE.
fn classify(outcome: Option<CallOutcome>) -> (LinkOutcomeClass, Option<u16>, Option<i32>) {
    match outcome {
        None => (LinkOutcomeClass::Unknown, None, None),
        Some(CallOutcome::Answered(status)) => {
            let class = match status {
                200..=299 => LinkOutcomeClass::Answered2xx,
                400..=499 => LinkOutcomeClass::Answered4xx,
                500..=599 => LinkOutcomeClass::Answered5xx,
                _ => LinkOutcomeClass::AnsweredOther,
            };
            (class, Some(status), None)
        }
        // libcurl's own CURLE_OPERATION_TIMEDOUT is 28; CallOutcome::TimedOut carries no field
        // because net.rs already treats it as its own case, but this report's curl_rc is exactly
        // the code that variant is standing in for.
        Some(CallOutcome::TimedOut) => (LinkOutcomeClass::Timeout, None, Some(28)),
        Some(CallOutcome::Transport(rc)) if rc < 0 => (LinkOutcomeClass::Unknown, None, None),
        Some(CallOutcome::Transport(6)) => (LinkOutcomeClass::Dns, None, Some(6)),
        Some(CallOutcome::Transport(rc)) if matches!(rc, 35 | 60 | 77 | 90) => {
            (LinkOutcomeClass::Tls, None, Some(rc))
        }
        Some(CallOutcome::Transport(rc)) => (LinkOutcomeClass::TransportOther, None, Some(rc)),
    }
}

/// PURE. Build the report's context from the flow's own state. Called from
/// `auth.rs::signin_error_context`, which supplies the live `LinkState`, the last plex.tv call, the
/// code generation under `Ctl`'s lock, and (issue #76) the session's current storage class — that
/// last one is a plain pass-through, since deriving the live verdict is `plex::session`'s business,
/// not this pure builder's.
pub(crate) fn context_from(
    kind: SignInFailureKind,
    link: &crate::auth::LinkState,
    last: Option<CallOutcome>,
    generation: u32,
    storage: SessionStorageClass,
) -> SignInErrorContext {
    let (class, http_status, curl_rc) = classify(last);
    SignInErrorContext {
        kind,
        link: class,
        http_status,
        curl_rc,
        storage,
        unanswered: UnansweredBucket::from_count(link.unanswered),
        failing_for: FailingForBucket::from_duration(link.failing_for),
        code_generation: generation.clamp(1, 4) as u8,
        occurred_at_ms: now_ms(),
    }
}

/// The `signin.consent` tag/context value — which of the two reporting paths produced one report,
/// so the two are separable in Sentry even though they share a schema. `report_error` (the
/// standing crash/error consent, already on) always stamps `Standing`; `send_once` (the one
/// explicit press on the sign-in screen, no standing consent involved) always stamps `OneOff`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsentKind {
    Standing,
    /// Stamped by [`send_once`], which `auth::send_trouble_once` calls from the sign-in screen's
    /// one-off report alert (`ui::login`'s "Send report" answer).
    OneOff,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OneOffSource {
    SignInTrouble,
    StorageDetails,
}

impl OneOffSource {
    fn code(self) -> &'static str {
        match self {
            Self::SignInTrouble => "sign_in_trouble",
            Self::StorageDetails => "storage_details",
        }
    }
}

impl ConsentKind {
    fn code(self) -> &'static str {
        match self {
            Self::Standing => "standing",
            Self::OneOff => "one_off",
        }
    }
}

/// Pure body builder — same shape as `playback::event_body`: `dist` and `errors_id` are passed in
/// so the consent preview can exercise this exact serialiser without reading `/proc/self/exe` or
/// minting an id before consent.
///
/// `storage_allowed` is the caller's read of `consent::allows_errors_at(6)` — `storage` is Errors
/// SCOPE 6 (`SCOPE_CHANGES`'s "Reports can now say how the sign-in is stored and why it was
/// refused"), one scope above the report's own gate (`report_error` is scope 5). A person whose
/// accepted scope is only 5 must not have this field on the wire yet, so it is OMITTED — not sent
/// as a placeholder — exactly as `posthog::envelope_props` omits `session_storage` when
/// `allows_usage_at(6)` is false. See `consent::allows_errors_at`.
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    consent: ConsentKind,
    ctx: SignInErrorContext,
    storage_allowed: bool,
) -> Vec<u8> {
    event_body_with_storage_outcome(
        event_id,
        dist,
        errors_id,
        consent,
        ctx,
        storage_allowed,
        &StorageOutcome::default(),
    )
}

/// Issue #76's third half: what a fresh sign-in's SAVE actually did with it — `None`/`None`/`None`
/// on every existing call site (`event_body`'s thin wrapper above). Carried as a separate parameter
/// rather than a field on [`SignInErrorContext`], which stays `Copy` for `auth.rs`'s sake
/// (`Trouble::ctx` is read out of a shared reference in `trouble_snapshot`/`send_trouble_once`,
/// which needs it to be — a runtime `String` cannot be `Copy` regardless) — same reasoning as
/// `telemetry::storage::StorageExtra`, this module's sibling for the same issue.
///
/// **Reaches only the ONE-OFF body.** The "Why" this exists for: when the storage layer itself
/// could not keep the standing consent decision, no STANDING report can ever leave — only the
/// one-off press can, so these fields are gated on `consent == ConsentKind::OneOff` inside
/// [`event_body_with_storage_outcome`] rather than on `storage_allowed`, which answers a different
/// (unrelated) scope question about the STANDING report's own `storage` field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StorageOutcome {
    pub persist_outcome: Option<&'static str>,
    pub preserve_reason: Option<&'static str>,
    pub candidate_reads: Option<String>,
    pub diagnostic: Option<crate::plex::session::ColdStorageDiagnostic>,
    pub fresh_readback: Option<crate::plex::session::FreshSaveReadback>,
    pub fresh_write_attempts: Vec<crate::plex::session::FreshWriteAttempt>,
    pub fresh_save_errors: Vec<crate::plex::session::FreshSaveError>,
}

/// Backing store for [`note_storage_outcome`]/[`storage_outcome`]/[`forget_storage_outcome`].
/// Module-level rather than a [`SignInErrorContext`] field for the reason [`StorageOutcome`]'s own
/// doc gives. **Not cleared on a flow reset or a new attempt** — a fresh sign-in's own save outcome
/// is what this exists to describe, and a stuck-or-failed attempt's one-off report is offered on
/// exactly the attempt whose save this was about, so there is no narrower moment to clear it at
/// short of the one this module is told about explicitly: sign-out (see [`forget_storage_outcome`]).
static STORAGE_OUTCOME: std::sync::Mutex<StorageOutcome> = std::sync::Mutex::new(StorageOutcome {
    persist_outcome: None,
    preserve_reason: None,
    candidate_reads: None,
    diagnostic: None,
    fresh_readback: None,
    fresh_write_attempts: Vec::new(),
    fresh_save_errors: Vec::new(),
});

/// Issue #76's third half: record what the save that followed a fresh sign-in actually did, so the
/// one-off report (`send_once`, the only shape that can leave the television when the standing
/// consent decision itself could not be stored — see the module doc's "Why") can say so. Called
/// from `auth::note_sign_in_persist`, right after `plex::session::save` decides an outcome — on
/// both of the sign-in flow's own saves (the discovery-thread roster save and `take_ready`'s).
pub(crate) fn note_storage_outcome(
    persist_outcome: Option<&'static str>,
    preserve_reason: Option<&'static str>,
    candidate_reads: Option<String>,
) {
    *STORAGE_OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) = StorageOutcome {
        persist_outcome,
        preserve_reason,
        candidate_reads,
        diagnostic: None,
        fresh_readback: None,
        fresh_write_attempts: Vec::new(),
        fresh_save_errors: Vec::new(),
    };
}

/// Test-only reader for the single slot above — the issue #76 report-lane integration tests live
/// in `telemetry::storage`'s own module (the send-attempt double is private to it) while the thing
/// under test is `auth`'s save lane, so neither side can reach this static directly.
#[cfg(test)]
pub(crate) fn storage_outcome_for_test() -> StorageOutcome {
    storage_outcome()
}

fn storage_outcome() -> StorageOutcome {
    let mut outcome = STORAGE_OUTCOME
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    // An early sign-in failure can happen before either save has run. In that case the one-off
    // report still needs the cold read's candidate summary, but must not pretend a save outcome
    // exists. `cold_session_facts` is the session layer's proof that this launch performed its
    // cold read; the session owner clears it at sign-out and before the next account's boot.
    if outcome.candidate_reads.is_none()
        && crate::plex::session::cold_session_facts().is_some()
    {
        outcome.candidate_reads = Some(crate::plex::session::candidate_reads_wire());
    }
    outcome.diagnostic = crate::plex::session::cold_storage_diagnostic();
    outcome.fresh_readback = crate::plex::session::last_fresh_save_readback();
    outcome.fresh_write_attempts = crate::plex::session::fresh_write_attempts();
    outcome.fresh_save_errors = crate::plex::session::fresh_save_errors();
    outcome
}

/// Cleared on sign-out, through the existing forget path (`telemetry::forget`, behind
/// `auth::forget_account`) — the same lifetime every other per-account telemetry fact in this
/// corner of the app has: consent belongs to the account that gave it, and so does the record of
/// what its own sign-in save did.
pub(crate) fn forget_storage_outcome() {
    *STORAGE_OUTCOME.lock().unwrap_or_else(|e| e.into_inner()) = StorageOutcome::default();
}

pub(crate) fn event_body_with_storage_outcome(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    consent: ConsentKind,
    ctx: SignInErrorContext,
    storage_allowed: bool,
    storage_outcome: &StorageOutcome,
) -> Vec<u8> {
    event_body_with_storage_outcome_and_source(
        event_id,
        dist,
        errors_id,
        consent,
        ctx,
        storage_allowed,
        storage_outcome,
        OneOffSource::SignInTrouble,
    )
}

fn event_body_with_storage_outcome_and_source(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    consent: ConsentKind,
    ctx: SignInErrorContext,
    storage_allowed: bool,
    storage_outcome: &StorageOutcome,
    source: OneOffSource,
) -> Vec<u8> {
    let kind_code = ctx.kind.code();
    let link_code = ctx.link.code();
    let consent_code = consent.code();
    let mut signin_ctx = serde_json::json!({
        "type": "signin",
        "kind": kind_code,
        "link": link_code,
        "unanswered": ctx.unanswered.code(),
        "failing_for": ctx.failing_for.code(),
        "code_generation": ctx.code_generation,
        "consent": consent_code,
    });
    if storage_allowed {
        signin_ctx["storage"] = Value::from(ctx.storage.code());
    }
    if let Some(status) = ctx.http_status {
        signin_ctx["http_status"] = Value::from(status);
    }
    if let Some(rc) = ctx.curl_rc {
        signin_ctx["curl_rc"] = Value::from(rc);
    }
    // The one-off report is the ONLY shape that can leave the television when the storage layer
    // itself could not keep the standing consent decision — see this module's doc's "Why" and
    // `StorageOutcome`'s own. A `Standing` body never carries these, whatever `storage_outcome`
    // holds, so a body built with them can never also say "standing".
    if matches!(consent, ConsentKind::OneOff) {
        signin_ctx["source"] = Value::from(source.code());
        if let Some(word) = storage_outcome.persist_outcome {
            signin_ctx["storage_persist_outcome"] = Value::from(word);
        }
        if let Some(word) = storage_outcome.preserve_reason {
            signin_ctx["storage_preserve_reason"] = Value::from(word);
        }
        if let Some(reads) = &storage_outcome.candidate_reads {
            signin_ctx["storage_candidate_reads"] = Value::from(reads.as_str());
        }
        if let Some(cold) = &storage_outcome.diagnostic {
            signin_ctx["storage_candidate_reads"] = Value::from(cold.candidate_reads.as_str());
            signin_ctx["storage_cold_candidate"] = cold
                .facts
                .winner
                .map_or(Value::Null, |winner| Value::from(winner.wire()));
            signin_ctx["storage_cold_account_credential"] =
                Value::from(cold.facts.account_token_present);
            signin_ctx["storage_cold_pms_credential"] = Value::from(cold.facts.pms_token_present);
            signin_ctx["storage_cold_server_dialable"] = Value::from(cold.facts.server_dialable);
            signin_ctx["storage_cold_can_go_local"] = Value::from(cold.facts.can_go_local);
            signin_ctx["storage_cold_errors"] = Value::Array(
                cold.errors
                    .iter()
                    .map(|error| {
                        let ctx = error.context;
                        let mut value = serde_json::json!({
                            "candidate": error.candidate.wire(),
                            "stage": ctx.stage.code(),
                            "class": ctx.class.code(),
                            "refused_marker": ctx.refused_marker,
                            "registered_with_app_id": ctx.registered_with_app_id,
                            "registered_with_name": ctx.registered_with_name,
                            "sealed_identity": ctx.sealed_identity.map_or("none", |i| i.code()),
                        });
                        if let Some(code) = ctx.service_error_code {
                            value["service_error_code"] = Value::from(code);
                        }
                        if let Some(outcome) = ctx.key_outcome {
                            value["key_outcome"] = Value::from(outcome.code());
                        }
                        value
                    })
                    .collect(),
            );
        }
        if let Some(readback) = storage_outcome.fresh_readback {
            signin_ctx["storage_fresh_save_candidate"] = Value::from(readback.winner.wire());
            signin_ctx["storage_fresh_save_readback"] = Value::from(readback.result.wire());
        }
        signin_ctx["storage_fresh_write_attempts"] = Value::Array(
            storage_outcome
                .fresh_write_attempts
                .iter()
                .map(|attempt| {
                    use crate::plex::session::{
                        AtomicWriteDurability, AtomicWriteFailure, FreshWriteResult,
                    };
                    let mut value = serde_json::json!({"candidate": attempt.candidate.wire()});
                    match attempt.result {
                        FreshWriteResult::Written { durability } => {
                            value["result"] = Value::from("written");
                            match durability {
                                AtomicWriteDurability::Durable => {
                                    value["durability"] = Value::from("durable");
                                }
                                AtomicWriteDurability::Warning { operation, errno } => {
                                    value["durability"] = Value::from("warning");
                                    value["operation"] = Value::from(operation.wire());
                                    value["errno"] = Value::from(errno);
                                }
                            }
                        }
                        FreshWriteResult::Failed { failure } => {
                            value["result"] = Value::from("failed");
                            match failure {
                                AtomicWriteFailure::Policy(policy) => {
                                    value["policy"] = Value::from(policy.wire());
                                }
                                AtomicWriteFailure::Os { operation, errno } => {
                                    value["operation"] = Value::from(operation.wire());
                                    value["errno"] = Value::from(errno);
                                }
                            }
                        }
                    }
                    value
                })
                .collect(),
        );
        signin_ctx["storage_fresh_save_errors"] = Value::Array(
            storage_outcome
                .fresh_save_errors
                .iter()
                .map(|error| {
                    let ctx = error.context;
                    let mut value = serde_json::json!({
                        "origin": "fresh_save",
                        "stage": ctx.stage.code(),
                        "class": ctx.class.code(),
                        "refused_marker": ctx.refused_marker,
                        "registered_with_app_id": ctx.registered_with_app_id,
                        "registered_with_name": ctx.registered_with_name,
                        "sealed_identity": ctx.sealed_identity.map_or("none", |i| i.code()),
                    });
                    if let Some(candidate) = error.candidate {
                        value["candidate"] = Value::from(candidate.wire());
                    }
                    if let Some(code) = ctx.service_error_code {
                        value["service_error_code"] = Value::from(code);
                    }
                    if let Some(outcome) = ctx.key_outcome {
                        value["key_outcome"] = Value::from(outcome.code());
                    }
                    value
                })
                .collect(),
        );
    }
    let (logger, transaction, culprit, fingerprint, exception_type, exception_value) = match source {
        OneOffSource::StorageDetails if matches!(consent, ConsentKind::OneOff) => (
            "storage",
            "storage",
            "storage::user_requested",
            serde_json::json!(["storage-diagnostic", "storage_details"]),
            "DiagnosticReport",
            "storage_details",
        ),
        _ => (
            "signin",
            "signin",
            "signin",
            serde_json::json!(["signin-error", kind_code, link_code]),
            "SignInError",
            kind_code,
        ),
    };
    let culprit = if culprit == "signin" {
        format!("signin::{kind_code}")
    } else {
        culprit.to_string()
    };
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": env!("PLX_VERSION")},
        "logger": logger,
        "transaction": transaction,
        "culprit": culprit,
        "fingerprint": fingerprint,
        "exception": {"values": [{
            "type": exception_type,
            "value": exception_value,
            "mechanism": {"type": "signin", "handled": true},
        }]},
        "tags": {
            "signin.kind": kind_code,
            "signin.link": link_code,
            "signin.consent": consent_code,
        },
        "contexts": {"signin": signin_ctx},
    });
    if matches!(consent, ConsentKind::OneOff) {
        body["tags"]["report.source"] = Value::from(source.code());
    }
    if storage_allowed {
        body["tags"]["signin.storage"] = Value::from(ctx.storage.code());
    }
    if !dist.is_empty() {
        body["dist"] = Value::String(dist.to_string());
    }
    // Omitted rather than sent as `0.0` (1970-01-01) when the wall clock was at or behind the
    // epoch when this was captured (`now_ms`'s own doc) — the envelope's `sent_at`
    // (`sentry::envelope`) still dates the report to POST time either way, but a fabricated
    // 1970 OCCURRENCE timestamp would be actively misleading rather than merely imprecise
    // (review finding, 2026-09-10).
    if ctx.occurred_at_ms > 0 {
        body["timestamp"] = Value::from(ctx.occurred_at_ms as f64 / 1000.0);
    }
    super::sentry::attach_user(&mut body, errors_id);
    super::sentry::attach_hardware_context(&mut body);
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Queue one handled event and ask the existing background sender to flush it. No network work is
/// performed on the render thread. Gated exactly like `playback::report_error` — the same consent
/// question and the same Errors/Sentry lane, since this is a new FIELD SET on an existing report
/// category rather than a new consent question. Called from `auth.rs::set_error` after the `Ctl`
/// lock that built `ctx` has been released.
///
/// Returns whether the report was actually queued, so a caller that also wants to tell the person
/// "a report was sent" can know it really was.
pub(crate) fn report_error(ctx: SignInErrorContext) -> bool {
    // Sign-in error reports are Errors scope 5 — a stored yes from an accepted scope below 5 does
    // not cover this field set yet, even while ordinary crash reports keep flowing.
    if !super::consent::allows_errors_at(5) || !super::sender::has_sentry() {
        return false;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — handled sign-in error was not queued");
        return false;
    };
    // The `storage` fact is a SEPARATE, later scope (Errors 6) than the report itself (5) — read
    // and bake it in here, same as the spool re-check below reads the report's own scope.
    let storage_allowed = super::consent::allows_errors_at(6);
    let body = event_body(
        &event_id,
        super::sentry::build_id(),
        super::consent::errors_id().as_deref(),
        ConsentKind::Standing,
        ctx,
        storage_allowed,
    );
    let record = super::queue::Record {
        category: super::queue::Category::Errors,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    match super::spool::append_if(&record, || super::consent::allows_errors_at(5)) {
        Some(true) => {
            super::flush_soon();
            true
        }
        Some(false) => {
            crate::log("telemetry: handled sign-in error did not fit the durable spool");
            false
        }
        None => false, // consent changed while the event was being shaped
    }
}

/// **A ONE-OFF report, consented by exactly one explicit press.**
///
/// This is the "Send report" answer on the sign-in screen's decision alert
/// (`auth::send_trouble_once`, called from `ui::login`), offered for any attempt that has not
/// already been reported automatically. That is USUALLY a failed or stuck sign-in with standing
/// crash-report consent off (when it is on, [`report_error`] already sent one and the alert is
/// never opened) — but a STUCK sign-in (`auth::note_waiting_trouble`) never takes the standing
/// path even when consent is already on, so this can also be reached with standing consent ON. It
/// is not a standing consent decision either way: no account-wide choice or persistent/user
/// identifier is recorded, though the accepted report gets an ephemeral event ID the reporter can
/// quote. No later standing-consent change withdraws it — see `queue::Category::OneOff`'s doc. It
/// requires only that this build carries a Sentry endpoint at all; the standing "have error reports been turned
/// on" question does not apply to a report the person is looking at and pressing Send for.
///
/// Returns whether the one-off lane accepted the report, either into the durable spool or its
/// bounded direct fallback.
#[cfg(test)]
pub(crate) fn send_once(ctx: SignInErrorContext) -> bool {
    send_one_off(ctx, OneOffSource::SignInTrouble).is_some()
}

/// Explicit user-requested storage report. Returns the ephemeral Sentry event id once the one-off
/// lane accepts it (durable spool or bounded direct fallback), so the reporter can quote a receipt
/// without acquiring a persistent identity. Delivery state is owned by `telemetry::oneoff`.
pub(crate) fn send_requested(
    ctx: SignInErrorContext,
    source: OneOffSource,
) -> Option<String> {
    send_one_off(ctx, source)
}

fn send_one_off(ctx: SignInErrorContext, source: OneOffSource) -> Option<String> {
    if !super::sender::has_sentry() {
        return None;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — one-off sign-in report was not queued");
        return None;
    };
    // No errors_id or other persistent/user identity: only this report's ephemeral event ID.
    // Gated on nothing (PRIVACY.md: "the explicit press, gated on nothing"), so `storage` always
    // rides — the person is looking at the offer to send this exact shape when they press it.
    let body = event_body_with_storage_outcome_and_source(
        &event_id,
        super::sentry::build_id(),
        None,
        ConsentKind::OneOff,
        ctx,
        true,
        &storage_outcome(),
        source,
    );
    let record = super::queue::Record {
        category: super::queue::Category::OneOff,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    let event_id = record.event_id.clone();
    super::oneoff::submit(record).then_some(event_id)
}

/// Representative handled-error payload built through the real serialiser, for the consent
/// screen's preview. No consent-time identifier is minted.
///
/// Shows the STANDING form — the automatic report a person who already turned error reports on
/// gets at a failed sign-in, since that is the one the crash/errors consent question is actually
/// asking about. The one-off form ([`send_once`]) is not gated on this question at all — it is
/// its own press, described in prose rather than by a second sample. Always shows `storage` — a
/// preview is what a FULLY ACCEPTED decision would send, and disclosing that field before anyone
/// accepts it is the whole point (`posthog::preview`'s doc says the same for `session_storage`).
pub(crate) fn preview_event() -> Vec<u8> {
    event_body(
        "<random per-error event id>",
        "<running ELF build id>",
        Some(super::native::PREVIEW_USER_ID),
        ConsentKind::Standing,
        SignInErrorContext {
            kind: SignInFailureKind::PinCreate,
            link: LinkOutcomeClass::Dns,
            http_status: None,
            curl_rc: Some(6),
            unanswered: UnansweredBucket::One,
            failing_for: FailingForBucket::Under10s,
            code_generation: 1,
            storage: SessionStorageClass::Secure,
            occurred_at_ms: 0,
        },
        true,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::LinkState;
    use std::time::Duration;

    fn link(unanswered: u32, failing_for: Option<Duration>) -> LinkState {
        LinkState {
            unanswered,
            failing_for,
            last_call: None,
        }
    }

    #[test]
    fn classify_maps_every_call_outcome_to_its_link_class() {
        assert_eq!(classify(None), (LinkOutcomeClass::Unknown, None, None));
        assert_eq!(
            classify(Some(CallOutcome::Answered(200))),
            (LinkOutcomeClass::Answered2xx, Some(200), None)
        );
        assert_eq!(
            classify(Some(CallOutcome::Answered(299))),
            (LinkOutcomeClass::Answered2xx, Some(299), None)
        );
        assert_eq!(
            classify(Some(CallOutcome::Answered(429))),
            (LinkOutcomeClass::Answered4xx, Some(429), None)
        );
        assert_eq!(
            classify(Some(CallOutcome::Answered(503))),
            (LinkOutcomeClass::Answered5xx, Some(503), None)
        );
        assert_eq!(
            classify(Some(CallOutcome::Answered(101))),
            (LinkOutcomeClass::AnsweredOther, Some(101), None)
        );
        assert_eq!(
            classify(Some(CallOutcome::TimedOut)),
            (LinkOutcomeClass::Timeout, None, Some(28))
        );
        assert_eq!(
            classify(Some(CallOutcome::Transport(6))),
            (LinkOutcomeClass::Dns, None, Some(6))
        );
        for rc in [35, 60, 77, 90] {
            assert_eq!(
                classify(Some(CallOutcome::Transport(rc))),
                (LinkOutcomeClass::Tls, None, Some(rc)),
                "rc {rc}"
            );
        }
        assert_eq!(
            classify(Some(CallOutcome::Transport(7))),
            (LinkOutcomeClass::TransportOther, None, Some(7))
        );
        assert_eq!(
            classify(Some(CallOutcome::Transport(-1))),
            (LinkOutcomeClass::Unknown, None, None),
            "libcurl unavailable is Unknown, not a real transport code"
        );
    }

    #[test]
    fn unanswered_bucket_edges() {
        assert_eq!(UnansweredBucket::from_count(0), UnansweredBucket::Zero);
        assert_eq!(UnansweredBucket::from_count(1), UnansweredBucket::One);
        assert_eq!(UnansweredBucket::from_count(2), UnansweredBucket::TwoToFive);
        assert_eq!(UnansweredBucket::from_count(5), UnansweredBucket::TwoToFive);
        assert_eq!(UnansweredBucket::from_count(6), UnansweredBucket::SixPlus);
    }

    #[test]
    fn failing_for_bucket_edges() {
        assert_eq!(FailingForBucket::from_duration(None), FailingForBucket::None);
        assert_eq!(
            FailingForBucket::from_duration(Some(Duration::from_secs(9))),
            FailingForBucket::Under10s
        );
        assert_eq!(
            FailingForBucket::from_duration(Some(Duration::from_secs(10))),
            FailingForBucket::Under60s
        );
        assert_eq!(
            FailingForBucket::from_duration(Some(Duration::from_secs(59))),
            FailingForBucket::Under60s
        );
        assert_eq!(
            FailingForBucket::from_duration(Some(Duration::from_secs(60))),
            FailingForBucket::Under5m
        );
        assert_eq!(
            FailingForBucket::from_duration(Some(Duration::from_secs(299))),
            FailingForBucket::Under5m
        );
        assert_eq!(
            FailingForBucket::from_duration(Some(Duration::from_secs(300))),
            FailingForBucket::FiveMinPlus
        );
    }

    #[test]
    fn context_from_buckets_the_live_flow_state() {
        let ctx = context_from(
            SignInFailureKind::Authorization,
            &link(3, Some(Duration::from_secs(45))),
            Some(CallOutcome::Answered(429)),
            2,
            SessionStorageClass::Secure,
        );
        assert_eq!(ctx.kind, SignInFailureKind::Authorization);
        assert_eq!(ctx.link, LinkOutcomeClass::Answered4xx);
        assert_eq!(ctx.http_status, Some(429));
        assert_eq!(ctx.curl_rc, None);
        assert_eq!(ctx.unanswered, UnansweredBucket::TwoToFive);
        assert_eq!(ctx.failing_for, FailingForBucket::Under60s);
        assert_eq!(ctx.code_generation, 2);
        assert_eq!(ctx.storage, SessionStorageClass::Secure);
    }

    #[test]
    fn context_from_clamps_code_generation() {
        let s = link(0, None);
        assert_eq!(
            context_from(SignInFailureKind::Other, &s, None, 0, SessionStorageClass::None)
                .code_generation,
            1
        );
        assert_eq!(
            context_from(SignInFailureKind::Other, &s, None, 9, SessionStorageClass::None)
                .code_generation,
            4
        );
    }

    fn context() -> SignInErrorContext {
        SignInErrorContext {
            kind: SignInFailureKind::PinCreate,
            link: LinkOutcomeClass::Dns,
            http_status: None,
            curl_rc: Some(6),
            unanswered: UnansweredBucket::One,
            failing_for: FailingForBucket::Under10s,
            code_generation: 1,
            storage: SessionStorageClass::SecureRefused,
            occurred_at_ms: 1_725_000_000_000,
        }
    }

    #[test]
    fn signin_context_carries_http_status_xor_curl_rc() {
        let answered: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            SignInErrorContext {
                link: LinkOutcomeClass::Answered4xx,
                http_status: Some(429),
                curl_rc: None,
                ..context()
            },
            true,
        ))
        .expect("event JSON");
        let ctx = &answered["contexts"]["signin"];
        assert_eq!(ctx["http_status"], 429);
        assert!(ctx.get("curl_rc").is_none());

        let transport: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            context(),
            true,
        ))
        .expect("event JSON");
        let ctx = &transport["contexts"]["signin"];
        assert_eq!(ctx["curl_rc"], 6);
        assert!(ctx.get("http_status").is_none());
    }

    /// **The 2026-09-10 TV telemetry proof (scenario 4)**: a sign-in report queued while the
    /// spool held onto it (a slow flush, a closed app) was stamped with SENTRY'S RECEIPT time
    /// rather than when the sign-in actually failed, because `event_body` wrote no `timestamp` at
    /// all. `storage::event_body` already carries `occurred_at_ms` for exactly this reason
    /// (`docs/measurements/telemetry-proof-tv-2026-09-10.md`'s deviation 3) — this pins the same
    /// field here.
    #[test]
    fn context_from_stamps_occurred_at_ms_and_event_body_carries_it() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let ctx = context_from(
            SignInFailureKind::PinCreate,
            &link(0, None),
            None,
            1,
            SessionStorageClass::None,
        );
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(
            ctx.occurred_at_ms >= before && ctx.occurred_at_ms <= after,
            "occurred_at_ms {} not in [{before}, {after}]",
            ctx.occurred_at_ms
        );

        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            ctx,
            true,
        ))
        .expect("event JSON");
        assert_eq!(v["timestamp"], ctx.occurred_at_ms as f64 / 1000.0);
    }

    /// Same PLX-NATIVE-F gap as playback: a handled sign-in error must carry the crash path's
    /// `hardware`/`webos` contexts, not just its own `signin` context.
    #[test]
    fn handled_signin_error_carries_the_same_hardware_context_as_a_crash() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            context(),
            true,
        ))
        .expect("event JSON");
        assert!(v["contexts"]["hardware"].is_object(), "no hardware: {v}");
        assert!(v["contexts"]["hardware"]["rtkmem"].is_string());
        assert!(v["contexts"]["hardware"]["install"].is_string());
        assert!(v["contexts"]["hardware"]["soc"].is_string());
        assert_eq!(v["contexts"]["webos"]["type"], "webos");
        assert!(v["contexts"]["signin"].is_object());
    }

    #[test]
    fn event_body_top_level_keys_are_exact() {
        fn keys(v: &Value) -> Vec<&str> {
            let mut out: Vec<_> = v
                .as_object()
                .expect("object")
                .keys()
                .map(String::as_str)
                .collect();
            out.sort_unstable();
            out
        }
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            context(),
            true,
        ))
        .expect("event JSON");
        assert_eq!(
            keys(&v),
            [
                "contexts",
                "culprit",
                "dist",
                "environment",
                "event_id",
                "exception",
                "fingerprint",
                "level",
                "logger",
                "platform",
                "release",
                "sdk",
                "tags",
                "timestamp",
                "transaction",
                "user",
            ]
        );
        assert_eq!(keys(&v["contexts"]), ["hardware", "signin", "webos"]);
        assert_eq!(
            keys(&v["contexts"]["webos"]),
            ["api", "codename", "name", "release", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["hardware"]),
            ["install", "model", "revision", "rtkmem", "soc", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["signin"]),
            [
                "code_generation",
                "consent",
                "curl_rc",
                "failing_for",
                "kind",
                "link",
                "storage",
                "type",
                "unanswered",
            ]
        );
        assert_eq!(
            keys(&v["tags"]),
            [
                "signin.consent",
                "signin.kind",
                "signin.link",
                "signin.storage",
            ]
        );
        assert_eq!(v["exception"]["values"][0]["type"], "SignInError");
        assert_eq!(v["fingerprint"], serde_json::json!(["signin-error", "pin_create", "dns"]));
        assert_eq!(v["contexts"]["signin"]["consent"], "standing");
        assert_eq!(v["tags"]["signin.consent"], "standing");
        assert_eq!(v["contexts"]["signin"]["storage"], "secure_refused");
        assert_eq!(v["tags"]["signin.storage"], "secure_refused");
    }

    /// A zero `occurred_at_ms` (the wall clock was at/behind the epoch when it was captured —
    /// `now_ms`'s own doc) must produce a body with NO `timestamp` key at all, never a fabricated
    /// 1970 stamp (review finding, 2026-09-10).
    #[test]
    fn a_zero_occurred_at_produces_no_timestamp_key() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            SignInErrorContext { occurred_at_ms: 0, ..context() },
            true,
        ))
        .expect("event JSON");
        assert!(v.get("timestamp").is_none(), "expected no timestamp key, got {v}");
    }

    /// **The two reporting paths are tagged distinctly, everywhere a Sentry query could split on
    /// it** — a body built with `ConsentKind::OneOff` never says "standing" anywhere.
    #[test]
    fn one_off_and_standing_bodies_carry_distinct_consent_tags() {
        let one_off: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            ConsentKind::OneOff,
            context(),
            true,
        ))
        .expect("event JSON");
        assert_eq!(one_off["contexts"]["signin"]["consent"], "one_off");
        assert_eq!(one_off["tags"]["signin.consent"], "one_off");
        // No `user` object for a one-off report — its event ID is not a persistent user identifier.
        assert!(one_off.get("user").is_none());
    }

    /// **Issue #76's third half: the storage-outcome fields ride the ONE-OFF body only, and only
    /// when the module has been told one.** A `Standing` body must never carry them even when a
    /// `StorageOutcome` is supplied — "a body built with them never says standing" is the property
    /// this pins, not merely "they can appear on a one-off body".
    #[test]
    fn storage_outcome_fields_appear_only_on_the_one_off_body_and_only_when_set() {
        let full = StorageOutcome {
            persist_outcome: Some("preserved_existing_secure"),
            preserve_reason: Some("not_proven"),
            candidate_reads: Some("developer:open_failed:13,internal:missing".to_string()),
            ..StorageOutcome::default()
        };

        // Present on OneOff when set.
        let one_off: Value = serde_json::from_slice(&event_body_with_storage_outcome(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            ConsentKind::OneOff,
            context(),
            true,
            &full,
        ))
        .expect("event JSON");
        assert_eq!(
            one_off["contexts"]["signin"]["storage_persist_outcome"],
            "preserved_existing_secure"
        );
        assert_eq!(
            one_off["contexts"]["signin"]["storage_preserve_reason"],
            "not_proven"
        );
        assert_eq!(
            one_off["contexts"]["signin"]["storage_candidate_reads"],
            "developer:open_failed:13,internal:missing"
        );

        // Absent on OneOff when the module has nothing to add.
        let one_off_empty: Value = serde_json::from_slice(&event_body_with_storage_outcome(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            ConsentKind::OneOff,
            context(),
            true,
            &StorageOutcome::default(),
        ))
        .expect("event JSON");
        for key in [
            "storage_persist_outcome",
            "storage_preserve_reason",
            "storage_candidate_reads",
        ] {
            assert!(
                one_off_empty["contexts"]["signin"].get(key).is_none(),
                "an empty StorageOutcome must omit {key}"
            );
        }

        // Never present on Standing, even when the module has something to say — a body carrying
        // these fields must never also say "standing".
        let standing: Value = serde_json::from_slice(&event_body_with_storage_outcome(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            context(),
            true,
            &full,
        ))
        .expect("event JSON");
        assert_eq!(standing["contexts"]["signin"]["consent"], "standing");
        for key in [
            "storage_persist_outcome",
            "storage_preserve_reason",
            "storage_candidate_reads",
        ] {
            assert!(
                standing["contexts"]["signin"].get(key).is_none(),
                "a Standing body must never carry {key}"
            );
        }
    }

    #[test]
    fn requested_storage_report_carries_owned_cold_evidence_without_changing_standing_reports() {
        use crate::plex::session::{
            CandidateCategory, ColdSessionFacts, ColdStorageDiagnostic, ColdStorageError,
            FreshSaveReadback, FreshSaveReadbackResult,
        };
        let full = StorageOutcome {
            candidate_reads: Some("internal:secure".to_string()),
            diagnostic: Some(ColdStorageDiagnostic {
                candidate_reads: "internal:secure".to_string(),
                facts: ColdSessionFacts {
                    winner: Some(CandidateCategory::Internal),
                    account_token_present: false,
                    pms_token_present: false,
                    server_dialable: false,
                    can_go_local: false,
                },
                errors: vec![ColdStorageError {
                    candidate: CandidateCategory::Internal,
                    context: crate::telemetry::storage::StorageErrorContext {
                        stage: crate::telemetry::storage::StorageStage::NoReply,
                        service_error_code: Some(-7),
                        class: crate::telemetry::storage::SessionStorageClass::SecureUnavailable,
                        refused_marker: false,
                        key_outcome: None,
                        registered_with_app_id: true,
                        registered_with_name: false,
                        sealed_identity: Some(crate::keymanager::Identity::AppId),
                    },
                }],
            }),
            fresh_readback: Some(FreshSaveReadback {
                winner: CandidateCategory::Internal,
                result: FreshSaveReadbackResult::Match,
            }),
            fresh_write_attempts: vec![crate::plex::session::FreshWriteAttempt {
                candidate: CandidateCategory::Developer,
                result: crate::plex::session::FreshWriteResult::Failed {
                    failure: crate::plex::session::AtomicWriteFailure::Os {
                        operation: crate::plex::session::AtomicWriteOperation::CreateTemp,
                        errno: 13,
                    },
                },
            }],
            fresh_save_errors: vec![crate::plex::session::FreshSaveError {
                candidate: None,
                context: crate::telemetry::storage::StorageErrorContext {
                    stage: crate::telemetry::storage::StorageStage::WriteFailed,
                    service_error_code: None,
                    class: crate::telemetry::storage::SessionStorageClass::Plaintext,
                    refused_marker: false,
                    key_outcome: None,
                    registered_with_app_id: false,
                    registered_with_name: true,
                    sealed_identity: None,
                },
            }],
            ..StorageOutcome::default()
        };
        let body: Value = serde_json::from_slice(&event_body_with_storage_outcome_and_source(
            &"d".repeat(32),
            "dist",
            None,
            ConsentKind::OneOff,
            context(),
            true,
            &full,
            OneOffSource::StorageDetails,
        ))
        .unwrap();
        let signin = &body["contexts"]["signin"];
        assert_eq!(body["exception"]["values"][0]["type"], "DiagnosticReport");
        assert_eq!(body["tags"]["report.source"], "storage_details");
        assert_eq!(signin["storage_cold_errors"][0]["candidate"], "internal");
        assert_eq!(signin["storage_cold_errors"][0]["stage"], "no_reply");
        assert_eq!(signin["storage_cold_errors"][0]["service_error_code"], -7);
        assert_eq!(signin["storage_cold_errors"][0]["class"], "secure_unavailable");
        assert_eq!(signin["storage_cold_errors"][0]["sealed_identity"], "app_id");
        assert_eq!(signin["storage_cold_account_credential"], false);
        assert_eq!(signin["storage_fresh_save_readback"], "match");
        assert_eq!(signin["storage_fresh_write_attempts"][0]["candidate"], "developer");
        assert_eq!(signin["storage_fresh_write_attempts"][0]["operation"], "create_temp");
        assert_eq!(signin["storage_fresh_write_attempts"][0]["errno"], 13);
        assert_eq!(signin["storage_fresh_save_errors"][0]["origin"], "fresh_save");
        assert_eq!(signin["storage_fresh_save_errors"][0]["stage"], "write_failed");
        assert_eq!(signin["storage_fresh_save_errors"][0]["class"], "plaintext");
        assert!(signin["storage_fresh_save_errors"][0].get("candidate").is_none());

        let standing: Value = serde_json::from_slice(&event_body_with_storage_outcome_and_source(
            &"e".repeat(32),
            "dist",
            None,
            ConsentKind::Standing,
            context(),
            true,
            &full,
            OneOffSource::SignInTrouble,
        ))
        .unwrap();
        assert!(standing["contexts"]["signin"].get("storage_cold_errors").is_none());
        assert!(standing["contexts"]["signin"]
            .get("storage_fresh_save_errors")
            .is_none());
        assert!(standing["tags"].get("report.source").is_none());
    }

    /// **Issue #76's third half, end to end: a sign-in `take_ready` could not persist reaches the
    /// ONE-OFF body.**
    ///
    /// This is the report that matters most on an affected set and the reason the whole lane
    /// exists: when the storage layer cannot keep a session file it very likely could not keep the
    /// standing consent decision either, so the one-off press is the only shape that can leave the
    /// television at all — and it had nothing to say about the save, because nobody called
    /// [`note_storage_outcome`]. Driven through the real `auth::take_ready` rather than through
    /// that setter, since the defect was the missing CALL, not the setter.
    #[test]
    fn a_sign_in_take_ready_could_not_persist_reaches_the_one_off_body() {
        let _g = crate::testlock::serial();
        // The only candidate sits under a directory that does not exist, so `write_atomic` cannot
        // create its temp sibling and every candidate refuses: `save` answers `WriteFailed`.
        let dir = std::env::temp_dir().join(format!("plxnative-signin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a writable temp dir");
        crate::plex::session::redirect_for_test(Some(dir.join("gone").join("auth.json")));
        forget_storage_outcome();

        crate::auth::arm_ready_for_test(crate::plex::session::Session {
            client_id: "cid-for-test".to_string(),
            account_token: "tok-for-test".to_string(),
            ..Default::default()
        });
        assert!(crate::auth::take_ready().is_some());

        let one_off: Value = serde_json::from_slice(&event_body_with_storage_outcome(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            ConsentKind::OneOff,
            context(),
            true,
            &storage_outcome(),
        ))
        .expect("event JSON");
        assert_eq!(
            one_off["contexts"]["signin"]["storage_persist_outcome"], "write_failed",
            "the one-off body must say what the sign-in's own save did"
        );
        assert!(
            one_off["contexts"]["signin"]
                .get("storage_preserve_reason")
                .is_none(),
            "a write that failed preserved nothing"
        );
        assert!(
            one_off["contexts"]["signin"]
                .get("storage_candidate_reads")
                .is_some(),
            "…and which candidate locations this launch had read"
        );

        forget_storage_outcome();
        crate::auth::reset_ctl_for_test();
        crate::plex::session::redirect_for_test(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`note_storage_outcome`] is what [`send_once`] actually reads — proven end to end through
    /// the durable spool the same way [`send_once_needs_no_standing_consent`] does.
    #[test]
    fn note_storage_outcome_is_read_by_send_once() {
        // `STORAGE_OUTCOME` is a crate global and this test WRITES it — including a
        // `forget_storage_outcome` that wipes it. Without the lock it raced the integration test
        // above (which drives a real `take_ready` and then reads the same global back) and made it
        // flake roughly one run in four. `testlock::serial`, not a local mutex: the global is also
        // reached from `auth` and from `telemetry::forget`.
        let _g = crate::testlock::serial();
        crate::plex::session::reset_report_state_for_test();
        note_storage_outcome(
            Some("write_failed"),
            None,
            Some("app_dir:plaintext".to_string()),
        );
        let got = storage_outcome();
        assert_eq!(got.persist_outcome, Some("write_failed"));
        assert_eq!(got.preserve_reason, None);
        assert_eq!(got.candidate_reads.as_deref(), Some("app_dir:plaintext"));
        forget_storage_outcome();
        let cleared = storage_outcome();
        assert_eq!(cleared, StorageOutcome::default());
    }

    /// A failure before QR completion has no save verdict, but the explicit one-off report can
    /// still identify which candidate locations the cold read examined. This must not fabricate a
    /// `persist_outcome` merely because the summary is available.
    #[test]
    fn an_early_one_off_signin_failure_carries_cold_candidates_without_save_fields() {
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!(
            "plxnative-early-signin-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("auth.json");
        crate::plex::session::redirect_for_test(Some(path));
        forget_storage_outcome();
        let _ = crate::plex::session::load();

        let one_off: Value = serde_json::from_slice(&event_body_with_storage_outcome(
            &"a".repeat(32),
            "0123456789abcdef",
            None,
            ConsentKind::OneOff,
            context(),
            true,
            &storage_outcome(),
        ))
        .unwrap();
        assert_eq!(
            one_off["contexts"]["signin"]["storage_candidate_reads"],
            "other:missing"
        );
        assert!(one_off["contexts"]["signin"]
            .get("storage_persist_outcome")
            .is_none());
        assert!(one_off["contexts"]["signin"]
            .get("storage_preserve_reason")
            .is_none());

        crate::plex::session::clear();
        crate::plex::session::redirect_for_test(None);
        assert_eq!(storage_outcome().candidate_reads, None);
        forget_storage_outcome();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The `storage` fact is Errors scope 6, one scope above the sign-in report itself (5): a
    /// person whose accepted scope is only 5 must not have it on the wire yet, and it is OMITTED
    /// — not sent as a placeholder — exactly like `posthog`'s `session_storage`.**
    #[test]
    fn storage_is_omitted_when_the_scope_is_not_accepted() {
        let allowed: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            context(),
            true,
        ))
        .expect("event JSON");
        assert_eq!(allowed["contexts"]["signin"]["storage"], "secure_refused");
        assert_eq!(allowed["tags"]["signin.storage"], "secure_refused");

        let refused: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            ConsentKind::Standing,
            context(),
            false,
        ))
        .expect("event JSON");
        assert!(
            refused["contexts"]["signin"].get("storage").is_none(),
            "an unaccepted errors scope must OMIT the key, not send a placeholder: {:?}",
            refused["contexts"]["signin"]
        );
        assert!(
            refused["tags"].get("signin.storage").is_none(),
            "an unaccepted errors scope must OMIT the tag, not send a placeholder: {:?}",
            refused["tags"]
        );
        // Every other field is unaffected — this is a per-field omission, not a truncated event.
        assert_eq!(refused["contexts"]["signin"]["kind"], "pin_create");
        assert_eq!(refused["contexts"]["signin"]["consent"], "standing");
        assert_eq!(refused["tags"]["signin.consent"], "standing");
    }

    /// **`send_once` needs no standing consent at all** — unlike `report_error`, it does not ask
    /// `consent::allows_errors()`. The only gate is whether this build carries a Sentry endpoint;
    /// with none (the common case in a dev checkout with no `pkg/telemetry.local.json`), it
    /// refuses and touches nothing — never spooling, never spawning the network flush worker,
    /// which is the boundary this test can prove without a real endpoint to send to.
    #[test]
    fn send_once_needs_no_standing_consent() {
        if super::super::sender::has_sentry() {
            // This checkout DOES carry a DSN: the full spool/flush path is covered instead by
            // `queue`'s and `sender`'s own OneOff tests, which exercise it without triggering an
            // actual network send.
            return;
        }
        assert!(!send_once(context()));
    }

    /// No 32-hex identifier (what a minted install/errors id looks like) appears anywhere in the
    /// preview beyond the fixed placeholder — same style as
    /// `ui::consent::the_preview_cannot_contain_a_real_identifier`.
    #[test]
    fn preview_contains_no_real_identifier() {
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        let text = v.to_string();
        let bytes: Vec<char> = text.chars().collect();
        let run = bytes.windows(32).any(|w| {
            w.iter()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        });
        assert!(!run, "the preview contains something shaped like a real id");
        let user = v["user"].as_object().expect("user object");
        assert_eq!(user.keys().collect::<Vec<_>>(), vec!["id"]);
        assert_eq!(v["user"]["id"], super::super::native::PREVIEW_USER_ID);
    }

    #[test]
    fn preview_event_parses_and_is_a_sign_in_error() {
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        assert_eq!(v["exception"]["values"][0]["type"], "SignInError");
    }

    /// PRIVACY.md names the same closed vocabulary this module actually emits — same shape as
    /// `playback::tests::consent_preview_and_privacy_name_the_closed_failure_domains`.
    #[test]
    fn privacy_names_the_closed_signin_vocabulary() {
        let privacy = include_str!("../../../PRIVACY.md");
        for value in [
            "pin_create",
            "authorization",
            "discovery",
            "answered_4xx",
            "dns",
            "tls",
            "timeout",
            "transport_other",
            "standing",
            "one_off",
        ] {
            assert!(privacy.contains(value), "PRIVACY.md omitted {value}");
        }
    }
}
