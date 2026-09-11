//! Handled storage errors for Sentry — issue #76: a keymanager3 session that seals but never opens
//! again (`plex::session`'s `LOCKED_RECOVERABLE`/refused-marker shape, `keymanager.rs`'s round-trip
//! proof) needs a bounded storage-specific diagnostic alongside the existing telemetry surfaces.
//! This does not claim that every firmware generation was previously silent: receiver evidence
//! exists for some newer-firmware PostHog runs, while Sentry receipt remains separately unverified
//! here. This is the SAME shape as
//! `telemetry::signin` — a closed handled-error report spooled through the existing Errors/Sentry
//! lane and gated on the SAME standing consent question, so it needs no consent bump of its own for
//! the reporting mechanism, only for the fields it adds (see `consent::POLICY_VERSION`).
//!
//! Every value accepted here is a closed enum or a bare number with no identity of its own — there
//! is no key material, no ciphertext, no plaintext, no file path, and no LS2 payload beyond the
//! numeric `errorCode` a service reply already carries (the same reasoning `signin.rs` relies on for
//! its HTTP status / curl rc). [`SessionStorageClass`] is the one vocabulary this module and
//! `diag::schema::UsageContext`'s `session_storage` field share — a value here means the same thing
//! on a crash report as it does riding along a usage event.
//!
//! **Unlike `signin.rs` this module has only the standing form.** A storage failure has no screen a
//! person is looking at to offer a one-off "Send report" press from, so there is no `send_once`
//! twin — [`report_error_with_candidate_reads`] is the production door, gated on
//! `consent::allows_errors_at(6)`. This whole
//! report — not just one field on it — is Errors scope 6, unlike the playback report (scope 4) and
//! the sign-in report (scope 5), which are each gated at their own, lower scope; the three standing
//! handled-error reports deliberately no longer share one gate.
//!
//! **A report found before that question is answered is HELD, not dropped — issue #76's second
//! half.** A locked read (`plex::session::read_locked`) can land on the very FIRST launch after an
//! update, before the reporting question has been put to anyone at all, or before this report's
//! own scope-6 extension on an already-accepted channel has been ruled on — and the whole point of
//! this report is to describe exactly that shape of failure. [`report_error_with_candidate_reads`] tries
//! [`send_now`] first and, only when that refuses because the question is still open
//! ([`should_defer`]), holds the context in the small in-memory [`DEFERRED`] queue (cap
//! [`DEFERRED_CAP`], session-only — never persisted, so a crash or relaunch before the question is
//! answered loses it) rather than the old behaviour of dropping it outright. [`replay_deferred`],
//! called from `telemetry::record_with_receipt` right after a new decision publishes (same call site as
//! `diag::replay_deferred`, issue #75's twin for the sign-in funnel), sends every held report
//! through [`send_now`] on a "yes" and drops the whole queue with nothing sent on a "no" — either
//! way the queue empties, so a refused answer cannot leak into the next decision. Each held report
//! keeps the timestamp it was ORIGINALLY found at ([`event_body`]'s `occurred_at_ms` argument),
//! not the time the question finally got answered. **A report found before the question is
//! answered is discarded if you answer No, if you SIGN OUT, or if the app closes before you answer
//! at all** — it is never written to disk, so it does not survive past the launch that found it,
//! and [`forget`] is what keeps it from surviving past the ACCOUNT that found it either (review
//! finding, 2026-09-11: it used to, and the next account's "yes" then sent it under that account's
//! Crash report ID).

use serde_json::Value;

/// How this install's session file is protected right now, or was the last time anything asked.
/// Mirrors `plex::session`'s own states (`NOT_LOCKED`/`LOCKED_RECOVERABLE` plus the persisted
/// refused-marker and the no-key-manager plaintext fallback) into one closed wire vocabulary, so a
/// dashboard can be built on this enum's codes without reading `session.rs` at all.
///
/// **Wired live**, not a placeholder: `plex::session::storage_class()` is the one producer, read by
/// `diag::schema::UsageContext::for_server` (every usage event), `auth.rs`'s `signin_error_context`
/// (the sign-in report) and `plex::session`'s own `locked`/`save_locked` (this module's own
/// `StorageError` report) — so a crash report, a sign-in report and a usage event all agree on the
/// live verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionStorageClass {
    /// This process has read or saved nothing yet — distinct from [`None`](Self::None) so a
    /// caller reached before the first `load`/`peek`/`save` can say "unknown, ask again" rather
    /// than "known and empty". `plex::session::storage_class`'s own doc has the ordering.
    Unknown,
    /// No session file exists yet — nothing has been protected because there is nothing to protect.
    None,
    /// The 0600 plaintext fallback: no usable key manager was found. A downgrade already recorded
    /// against this install reports the more specific [`SecureLocked`](Self::SecureLocked) or
    /// [`SecureRefused`](Self::SecureRefused) instead — those outrank this one in
    /// `plex::session::storage_class`, even once the file on disk is plaintext.
    Plaintext,
    /// A keymanager3 envelope that sealed and, so far, has round-tripped open.
    Secure,
    /// An UNRECOVERABLE locked read — a secure-shaped file this build does not recognize (a wrong
    /// format/version, or bytes that decrypted but did not parse as a session). Never reachable for
    /// a recoverable lock, which always writes the marker first and so always reports
    /// [`SecureRefused`](Self::SecureRefused) instead — see `plex::session::storage_class`'s doc.
    SecureLocked,
    /// A PRIOR (or this) launch already recorded the persisted refused marker — this install keeps
    /// the 0600 file until sign-out or erase, on every future launch, regardless of whether a fresh
    /// round trip would look clean.
    SecureRefused,
    /// **A sealed sign-in is on this install and THIS LAUNCH could not read it, because the key
    /// service never answered at all** — no reply within its budget, a registration that never
    /// reached the bus, or the hub answering for a service that is not there. Deliberately NOT
    /// [`SecureRefused`](Self::SecureRefused): nothing has been refused and nothing has been
    /// downgraded — the envelope is untouched on disk and the very next launch (or the sign-in
    /// screen's own *Try again*) may open it. It is what a temporary key-service failure looks
    /// like from the outside, and separating the two is the whole point: a dashboard that cannot
    /// tell them apart reads a stalled service as a permanent downgrade, which is the mistake the
    /// app itself used to make. `plex::session`'s bounded escalation is what finally turns a
    /// service that never answers into a real `SecureRefused`.
    SecureUnavailable,
}

impl SessionStorageClass {
    /// Every variant, in no particular order — the exhaustiveness source for the tests below.
    /// `_assert_all_variants_covered` is a compile-time check that a new variant cannot be added
    /// here without also being added to this list (review finding, 2026-09-10: the two hand-
    /// written test arrays had already drifted from the live enum once).
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::Unknown,
        Self::None,
        Self::Plaintext,
        Self::Secure,
        Self::SecureLocked,
        Self::SecureRefused,
        Self::SecureUnavailable,
    ];

    #[cfg(test)]
    #[allow(dead_code)]
    fn _assert_all_variants_covered(v: Self) {
        match v {
            Self::Unknown | Self::None | Self::Plaintext | Self::Secure | Self::SecureLocked
            | Self::SecureRefused | Self::SecureUnavailable => {}
        }
    }

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::None => "none",
            Self::Plaintext => "plaintext",
            Self::Secure => "secure",
            Self::SecureLocked => "secure_locked",
            Self::SecureRefused => "secure_refused",
            Self::SecureUnavailable => "secure_unavailable",
        }
    }
}

/// Which step of a seal/open attempt a storage failure was reported from. Every variant is wired
/// live: the first five come from `keymanager::stage_for_method`'s `log_refusal`/`log_missing_field`
/// call sites, `RoundtripMismatch` from `keymanager::round_trips`'s own proof, `EnvelopeUnparseable`/
/// `EnvelopeLocked` from `plex::session::locked` (a read landing `LOCKED_RECOVERABLE`/
/// `LOCKED_UNRECOVERABLE`), `NoReply`/`Unreachable` from `keymanager::note_unanswered` (a call
/// that timed out, or a registration that never succeeded), and `IdentityUnavailable` from
/// `keymanager::note_client_error` (the envelope's own identity could not be registered as).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StorageStage {
    /// The key manager could not mint or fetch the device key at all.
    GenerateKey,
    /// A call to start an encrypt operation was refused.
    BeginEncrypt,
    /// An encrypt operation was accepted but did not finish (or its reply had no usable payload).
    FinishEncrypt,
    /// A call to start a decrypt operation was refused.
    BeginDecrypt,
    /// A decrypt operation was accepted but did not finish (or its reply had no usable payload).
    FinishDecrypt,
    /// `keymanager::seal`'s own round-trip proof — encrypt succeeded but decrypting the exact
    /// envelope it just produced gave back something other than the original plaintext.
    RoundtripMismatch,
    /// A recognized-shape secure envelope decrypted to bytes this build could not parse as a
    /// session — `plex::session`'s `LOCKED_UNRECOVERABLE`.
    EnvelopeUnparseable,
    /// A recognized-shape secure envelope this process could not open, and the open reached no
    /// stage of its own to name — `plex::session`'s `LOCKED_RECOVERABLE` when `keymanager` had
    /// nothing more specific (it usually does: a locked read reports the failing call's stage).
    EnvelopeLocked,
    /// The key service was registered with but did not answer a call within its budget
    /// (`keymanager::platform::BUDGET`) — the stalled-service shape behind issues #75/#76's
    /// "slow, then try again" symptom. No error code exists for it by construction.
    NoReply,
    /// The key service could not be registered with at all (an LS2 setup failure inside the
    /// jail), so no call was ever made. No error code exists for it either.
    Unreachable,
    /// No new restart-authoritative session could be persisted: writes failed, or
    /// a readable older candidate survived cleanup and would shadow the fallback.
    /// This is not a key-service verdict. Fresh-save diagnostics separately carry
    /// each candidate's OS errno or policy rejection (including `shadowed_candidate`).
    WriteFailed,
    /// **The 2026-09-10 trust-widening fix's own stage.** A candidate session file was found with
    /// any group/other WRITE bit set (any of `0o022`) — not merely readable-widened, which is a
    /// disclosure problem the mode repair alone already closes, but WRITABLE, which means another
    /// uid on the shared `/media/developer` namespace could have rewritten its bytes. The content is
    /// therefore not trusted at all: it is never parsed, the file is moved off the name the next
    /// launch reads rather than repaired in place, and this install falls back to no session. (It
    /// is QUARANTINED as `<name>.untrusted` rather than deleted since 2026-09-10 — see
    /// `plex::session::quarantine_untrusted` — which changes nothing this stage reports.) No error
    /// code exists for it either — no key service was ever asked.
    UntrustedMode,
    /// **The envelope names an LS2 identity this launch could not register as** — issue #76's
    /// identity fix, `keymanager::ClientError::IdentityUnavailable`. A sealed envelope records
    /// WHICH identity owns its key (`keymanager::Sealed::identity`) and is only ever opened
    /// through that same one; when the hub will not grant it on this launch, nothing has been
    /// learned about the key or the service — only about a name — so this is TRANSIENT and is the
    /// one open failure that must never arm the cross-launch refused marker. No error code: no
    /// call was made.
    IdentityUnavailable,
    /// **Issue #76's third half (report lane): a freshly-signed-in session could not be kept the
    /// way `plex::session::save` decided to keep it.** Distinct from every stage above, which are
    /// all about REOPENING an existing envelope — this one is about the SAVE that follows a
    /// successful sign-in, before there was ever anything to open. Carried through
    /// [`report_sign_in_not_persisted`] rather than the general [`report_error_with_candidate_reads`] entry point, with
    /// its own `persist_outcome`/`preserve_reason`/`candidate_reads` fields on
    /// [`StorageErrorContext`] rather than the stage-keyed fields above, since the save lane's own
    /// vocabulary (`persisted_plaintext`/`persisted_sealed`/`preserved_existing_secure`/
    /// `blocked_unknown_envelope`/`write_failed`/`serialization_failed`) answers a different
    /// question than any `StorageStage` above it does. No error code exists for it either — a
    /// `write_atomic` refusal carries none, same as [`WriteFailed`](Self::WriteFailed).
    ///
    /// Constructed only by [`report_sign_in_not_persisted`].
    SignInNotPersisted,
}

impl StorageStage {
    /// Every variant — see [`SessionStorageClass::ALL`]'s doc for why this exists and what keeps
    /// it complete.
    #[cfg(test)]
    pub(crate) const ALL: &'static [Self] = &[
        Self::GenerateKey,
        Self::BeginEncrypt,
        Self::FinishEncrypt,
        Self::BeginDecrypt,
        Self::FinishDecrypt,
        Self::RoundtripMismatch,
        Self::EnvelopeUnparseable,
        Self::EnvelopeLocked,
        Self::NoReply,
        Self::Unreachable,
        Self::WriteFailed,
        Self::UntrustedMode,
        Self::IdentityUnavailable,
        Self::SignInNotPersisted,
    ];

    #[cfg(test)]
    #[allow(dead_code)]
    fn _assert_all_variants_covered(v: Self) {
        match v {
            Self::GenerateKey
            | Self::BeginEncrypt
            | Self::FinishEncrypt
            | Self::BeginDecrypt
            | Self::FinishDecrypt
            | Self::RoundtripMismatch
            | Self::EnvelopeUnparseable
            | Self::EnvelopeLocked
            | Self::NoReply
            | Self::Unreachable
            | Self::WriteFailed
            | Self::UntrustedMode
            | Self::IdentityUnavailable
            | Self::SignInNotPersisted => {}
        }
    }

    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::GenerateKey => "generate_key",
            Self::BeginEncrypt => "begin_encrypt",
            Self::FinishEncrypt => "finish_encrypt",
            Self::BeginDecrypt => "begin_decrypt",
            Self::FinishDecrypt => "finish_decrypt",
            Self::RoundtripMismatch => "roundtrip_mismatch",
            Self::EnvelopeUnparseable => "envelope_unparseable",
            Self::EnvelopeLocked => "envelope_locked",
            Self::NoReply => "no_reply",
            Self::Unreachable => "unreachable",
            Self::WriteFailed => "write_failed",
            Self::UntrustedMode => "untrusted_mode",
            Self::IdentityUnavailable => "identity_unavailable",
            Self::SignInNotPersisted => "sign_in_not_persisted",
        }
    }
}

/// Everything one handled storage-error report carries. Every field is a closed enum, a bare
/// service error number with no identity of its own, or a bool.
///
/// **Stays `Copy`, deliberately** — `plex::session`'s many call sites (`queue_report`,
/// `report_once`'s `report_storage_error(ctx)` followed by `ctx.stage`) rely on that today, and
/// that module is out of this lane's file scope. The issue #76 report-lane fields
/// (`persist_outcome`/`preserve_reason`/`candidate_reads`) therefore live on the separate
/// [`StorageExtra`], carried ALONGSIDE a context rather than added to it — see that type's doc.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StorageErrorContext {
    pub stage: StorageStage,
    /// The numeric `errorCode` the LS2 service reply carried, when [`stage`](Self::stage) was
    /// reached from an actual service call — `None` for a purely local failure (a mismatch, an
    /// unparseable envelope) that never reached a service reply at all.
    pub service_error_code: Option<i64>,
    pub class: SessionStorageClass,
    /// Whether this install already carries the persisted cross-launch refused marker at the
    /// moment this report was built — the same fact `plex::session::has_refused_marker` answers,
    /// carried here rather than re-derived from `class` so the report is self-contained.
    pub refused_marker: bool,
    /// Issue #76's identity decider: what `generateKey` told the SEAL that produced the envelope
    /// or probe this report is about — `Existed` (`-10002`) if an earlier registration already
    /// owned the key, `Created` if this call minted a new one. `None` when no seal's outcome is
    /// known for this report (a report built from a locked READ that itself never called
    /// `generateKey`, on a launch whose own seal has not run yet). See `keymanager::KeyOutcome`'s
    /// doc for why a cross-launch caller must read a PERSISTED value here rather than the live
    /// process-global.
    pub key_outcome: Option<crate::keymanager::KeyOutcome>,
    /// Issue #76's "owner_hint": whether the LS2 registration behind the seal this report is about
    /// identified itself with an application id — `crate::keymanager::registered_with_app_id()`,
    /// a PROBED fact since 2026-09-10 where it was a closed `false` before. Carried on every
    /// report (never optional) so a set that grants the app-id registration and a set that
    /// refuses it are told apart by the field rather than by which reports happen to carry one.
    /// `false` also covers a process that never registered at all, deliberately: the question is
    /// whether this report's key COULD have had a stable owner, and it could not.
    ///
    /// It means the **application-service** registration specifically, and keeps meaning exactly
    /// that — [`registered_with_name`](Self::registered_with_name) is the other shape.
    pub registered_with_app_id: bool,
    /// The sibling probed fact, added the same day the plain NAMED registration was: whether that
    /// registration took the app id as a bare bus name (`LSRegister(app_id)`) instead.
    ///
    /// **Two bools rather than one widened one, because they are two different keys of LG's
    /// ownership rule** (application id > sender service name) and a reporter's firmware may well
    /// grant one and refuse the other — which is precisely the thing issue #76 needs told apart.
    /// Both are derived from `keymanager`'s single one-way identity latch, so they can never both
    /// be true; `both_owner_hints_cannot_be_true_at_once` pins that.
    pub registered_with_name: bool,
    /// **Which identity sealed the thing this report is ABOUT** — the envelope, or the
    /// cross-launch probe — as recorded inside it (`keymanager::Sealed::identity`), and `None`
    /// (reported as `none`) where the report is about nothing sealed at all: a write that never
    /// landed, a candidate removed unread because another uid could have written it.
    ///
    /// **It is a different question from the two bools above and that is the point** (review
    /// finding, 2026-09-10). Those describe the registration THIS LAUNCH latched; this one
    /// describes the owner the key already has. Issue #76's whole hypothesis is that the two
    /// disagree — an envelope sealed by an anonymous launch, reopened by one that now registers
    /// under a stable identity, or the reverse — and before this field a report could not say so:
    /// it published the launch's own registration twice over and the envelope's not at all, which
    /// is the one fact the report exists to settle.
    pub sealed_identity: Option<crate::keymanager::Identity>,
}

/// **Issue #76's report lane (2026-09-11).** Carried ALONGSIDE a [`StorageErrorContext`] rather
/// than added to it — that struct stays `Copy` for `plex::session`'s sake (see its own doc), and
/// `candidate_reads` is a runtime-built `String`, which cannot be. Every field defaults to `None`,
/// which is what every report built before this lane existed still sends: nothing here changes an
/// existing report's shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct StorageExtra {
    /// **What the SAVE that followed a fresh sign-in actually did with it.** Set only by
    /// [`report_sign_in_not_persisted`] — every other report leaves this `None`, since every other
    /// stage describes a seal/open attempt against an EXISTING envelope rather than a save
    /// decision. One of the save lane's own closed words: `persisted_plaintext`,
    /// `persisted_sealed`, `preserved_existing_secure`, `blocked_unknown_envelope`,
    /// `write_failed`, `serialization_failed` — never renamed here, since a dashboard already keys
    /// on them.
    pub persist_outcome: Option<&'static str>,
    /// **Only meaningful alongside `persist_outcome: Some("preserved_existing_secure")`** — WHY the
    /// save left the existing envelope alone rather than overwriting it: `not_proven` (this
    /// install's key service has not yet passed the cross-launch open-what-it-sealed check),
    /// `refused_marker_no_fresh_sign_in` (this install already carries the persisted refused
    /// marker and the save was not itself a fresh sign-in), or `seal_failed` (a seal was attempted
    /// and did not succeed, so the prior envelope was kept rather than lost). `None` for every
    /// other `persist_outcome`.
    pub preserve_reason: Option<&'static str>,
    /// **The read lane's compact, closed-vocabulary summary of which candidate session paths were
    /// read and how it went** — shaped `developer:open_failed:13,internal:missing,app_dir:plaintext`
    /// (candidate category `developer`/`internal`/`app_dir`/`runtime`/`other`, a colon, a rejection
    /// code `missing`/`open_failed`/`not_regular`/`wrong_owner`/`metadata_failed`/`too_large`/
    /// `read_failed`/`untrusted_mode`/`unparsable` — or, for a candidate that DID read, the storage class word
    /// it produced — optionally a colon and the errno). **Never a path**: every token in it is one
    /// of the closed words above, or a small bounded number. Populated on the general
    /// The explicit [`report_error_with_candidate_reads`] path carries it for the load that owns
    /// the report; ordinary reports leave this field absent.
    pub candidate_reads: Option<String>,
}

/// `occurred_at_ms` is a Unix-epoch millisecond stamp, carried as Sentry's own `timestamp` field
/// (seconds, may be fractional). It exists so a report that waited in [`DEFERRED`] for an
/// unanswered consent question is sent dated to when the failure actually happened, not to when
/// [`replay_deferred`] finally got to it — the same reasoning `diag::mod::Stamp` captures a
/// timestamp for the sign-in funnel it defers.
///
/// Thin wrapper over [`event_body_with_extra`] with an empty [`StorageExtra`] — every call site
/// this module had before the issue #76 report lane, unchanged.
#[cfg(test)]
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    occurred_at_ms: u64,
    ctx: StorageErrorContext,
) -> Vec<u8> {
    event_body_with_extra(
        event_id,
        dist,
        errors_id,
        occurred_at_ms,
        ctx,
        &StorageExtra::default(),
    )
}

/// Pure body builder — same shape as `signin::event_body`, minus the consent-kind tag this module
/// has no need of (there is only the standing form; see the module doc), plus (2026-09-11) the
/// issue #76 report-lane fields carried on `extra` rather than on `ctx` — see [`StorageExtra`]'s
/// doc for why the two are separate.
pub(crate) fn event_body_with_extra(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    occurred_at_ms: u64,
    ctx: StorageErrorContext,
    extra: &StorageExtra,
) -> Vec<u8> {
    let stage_code = ctx.stage.code();
    let class_code = ctx.class.code();
    let mut storage_ctx = serde_json::json!({
        "type": "storage",
        "stage": stage_code,
        "class": class_code,
        "refused_marker": ctx.refused_marker,
        "registered_with_app_id": ctx.registered_with_app_id,
        "registered_with_name": ctx.registered_with_name,
        // `none` is deliberately a WORD rather than a null: a dashboard grouping on this field
        // must be able to see "no sealed thing" as its own bucket instead of a hole, and
        // `every_identity_code_is_distinct_and_snake_case` keeps the enum from claiming it.
        "sealed_identity": ctx.sealed_identity.map_or("none", |i| i.code()),
    });
    if let Some(code) = ctx.service_error_code {
        storage_ctx["service_error_code"] = Value::from(code);
    }
    if let Some(outcome) = ctx.key_outcome {
        storage_ctx["key_outcome"] = Value::from(outcome.code());
    }
    if let Some(word) = extra.persist_outcome {
        storage_ctx["persist_outcome"] = Value::from(word);
    }
    if let Some(word) = extra.preserve_reason {
        storage_ctx["preserve_reason"] = Value::from(word);
    }
    if let Some(reads) = &extra.candidate_reads {
        storage_ctx["candidate_reads"] = Value::from(reads.as_str());
    }
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": env!("PLX_VERSION")},
        "logger": "storage",
        "transaction": "storage",
        "culprit": format!("storage::{stage_code}"),
        "fingerprint": ["storage-error", stage_code],
        "exception": {"values": [{
            "type": "StorageError",
            "value": stage_code,
            "mechanism": {"type": "storage", "handled": true},
        }]},
        "tags": {
            "storage.stage": stage_code,
            "storage.class": class_code,
        },
        "contexts": {"storage": storage_ctx},
    });
    if !dist.is_empty() {
        body["dist"] = Value::String(dist.to_string());
    }
    // Omitted rather than sent as `0.0` (1970-01-01) when the wall clock was at or behind the
    // epoch — the envelope's own `sent_at` (`sentry::envelope`) still dates the report to POST
    // time either way; see `signin::event_body`'s twin for the same reasoning (review finding,
    // 2026-09-10).
    if occurred_at_ms > 0 {
        body["timestamp"] = Value::from(occurred_at_ms as f64 / 1000.0);
    }
    super::sentry::attach_user(&mut body, errors_id);
    super::sentry::attach_hardware_context(&mut body);
    serde_json::to_vec(&body).unwrap_or_default()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// How many storage-error reports this process holds in memory while the Errors channel's own
/// consent question — or this report's own scope (6) as a pending extension on an
/// already-accepted channel — sits unanswered, before the oldest is dropped to make room. Small
/// and bounded for the same reason `diag`'s sign-in defer is: this is memory a stranger's
/// television carries for as long as the question goes unanswered, which could be indefinitely,
/// and one launch realistically produces at most a couple of these (a locked read, in principle
/// also a later seal failure — see `plex::session::queue_report`'s doc).
const DEFERRED_CAP: usize = 4;

/// One storage-error report held because consent had not yet settled the question — see
/// [`report_error_with_candidate_reads`]'s doc. `occurred_at_ms` is captured once, at defer time, so a later
/// [`replay_deferred`] reports the failure as having happened when it actually did, not when the
/// consent question finally got answered — the same reasoning `diag::mod::Stamp` exists for.
struct Deferred {
    ctx: StorageErrorContext,
    occurred_at_ms: u64,
    /// Issue #76's report-lane fields carried alongside `ctx` (see [`StorageExtra`]'s own doc) —
    /// held across the same wait `ctx` is, so a replayed report still carries whatever
    /// `candidate_reads`/`persist_outcome`/`preserve_reason` it was found with.
    extra: StorageExtra,
}

/// Session-only, like `diag::DEFERRED`: never persisted, so a crash or relaunch before the
/// question is answered loses whatever was held here — the same trade every other in-memory queue
/// in this corner of the app makes.
static DEFERRED: std::sync::Mutex<Vec<Deferred>> = std::sync::Mutex::new(Vec::new());

/// Should a report [`send_now`] could not send RIGHT NOW be HELD rather than dropped? Only the two
/// shapes a later "yes" can still resolve: the Errors channel's own consent question has never
/// been engaged at all ([`super::consent::Consent::ever_answered`] is false), or it has been
/// engaged and the channel IS on, but this report's own scope (6) is a pending extension nobody
/// has ruled on yet ([`super::consent::pending_extensions`]). A stored "no" (the channel is off
/// and the question has been answered) and an already-declined extension both read `false` here —
/// those are real decisions, and [`report_error_with_candidate_reads`] drops exactly as it always has for them.
///
/// Deliberately independent of `sender::has_sentry` — a build with no Sentry endpoint can never
/// send a held report either way, so holding one there is a few bytes wasted rather than a
/// correctness question, and keeping this function answer the consent question ALONE is what
/// makes it (and therefore the gating this module exists to prove) testable without one.
fn should_defer() -> bool {
    if super::consent::errors_scope_enable_pending(6) {
        return true;
    }
    let Some(c) = super::consent::current() else {
        return true; // nothing loaded at all reads the same as unanswered
    };
    if !c.ever_answered() {
        return true;
    }
    super::consent::pending_extensions(&c).contains(&super::consent::Category::Errors)
}

fn defer(ctx: StorageErrorContext, extra: StorageExtra) {
    let mut q = DEFERRED.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= DEFERRED_CAP {
        q.remove(0); // oldest first — keep the newest CAP
    }
    q.push(Deferred {
        ctx,
        occurred_at_ms: now_ms(),
        extra,
    });
}

/// Send `ctx` now, dated `occurred_at_ms` — the actual queue-and-flush [`report_error_with_candidate_reads`] always
/// did, factored out so both the direct path and [`replay_deferred`] share it and stay gated
/// identically: Errors scope 6 ([`super::consent::allows_errors_at`]'s doc) and a build that
/// carries a Sentry endpoint at all.
fn send_now(ctx: StorageErrorContext, occurred_at_ms: u64, extra: StorageExtra) -> bool {
    if !super::consent::allows_errors_at(6) {
        return false;
    }
    // Recorded once the consent gate itself has passed — this is what lets a test built with no
    // Sentry endpoint compiled in (every dev checkout; see `sender`'s module doc) still prove the
    // GATING decision this module exists for, the same way `plex::session`'s own
    // `report_storage_error` test double stands in for a real send. `extra.clone()`: `StorageExtra`
    // (unlike `ctx`) is not `Copy` — `candidate_reads` is a runtime `String` — so this test-only
    // recording call must not consume the value the real send below still needs.
    #[cfg(test)]
    tests::record_send_attempt(ctx, occurred_at_ms, extra.clone());
    if !super::sender::has_sentry() {
        return false;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — handled storage error was not queued");
        return false;
    };
    let body = event_body_with_extra(
        &event_id,
        super::sentry::build_id(),
        super::consent::errors_id().as_deref(),
        occurred_at_ms,
        ctx,
        &extra,
    );
    let record = super::queue::Record {
        category: super::queue::Category::Errors,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    match super::spool::append_if(&record, || super::consent::allows_errors_at(6)) {
        Some(true) => {
            super::flush_soon();
            true
        }
        Some(false) => {
            crate::log("telemetry: handled storage error did not fit the durable spool");
            false
        }
        None => false, // consent changed while the event was being shaped
    }
}

/// What became of one [`report_error_with_candidate_reads`] call — session.rs's `report_once` (issue #76 Stage B2)
/// needs this three-way split, not the bare bool the function used to return: a stage may only be
/// marked reported-and-done once it was actually SENT or safely HELD for a later "yes" to replay
/// ([`replay_deferred`]) — a stage that was outright DROPPED (the Errors channel's own consent
/// question already answered "No") must stay retriable-in-principle rather than being conflated
/// with one that left real evidence somewhere, even though both end this one call the same way:
/// nothing left in flight, nothing more for THIS call to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReportOutcome {
    /// Actually left this television, over the wire, this call.
    Sent,
    /// Held in [`DEFERRED`] because the consent question is still open — [`replay_deferred`] will
    /// try it again the moment a decision publishes.
    Deferred,
    /// Neither sent nor held: the consent question has already been answered "No" (or this build
    /// carries no Sentry endpoint at all), so nothing more will ever come of this exact call.
    Dropped,
}

/// Queue one handled event and ask the existing background sender to flush it — or, if the Errors
/// channel's own consent question is still open, HOLD it in memory instead of dropping it (see
/// [`should_defer`]/[`DEFERRED`]) so a later "yes" this same launch can still see it
/// ([`replay_deferred`]). Gated exactly like `signin::report_error` and `playback::report_error` —
/// the same standing consent question and the same Errors/Sentry lane, since this is a new report
/// category riding the existing mechanism rather than a new one.
///
/// Returns which of [`ReportOutcome`]'s three shapes this call ended in — never `Sent` for one
/// that was only held, and never `Deferred` for one a real "No" already settled (see
/// [`should_defer`]).
///
/// Called from `plex::session::report_storage_error` in every build (test or not — Stage B2
/// widened this from the `#[cfg(not(test))]`-only caller it used to be exactly so a test could
/// exercise the real consent-gated defer/drop split with no Sentry endpoint compiled in; the old
/// doc here said this function was unreachable from a test build, which stopped being true the
/// same move).
#[cfg(test)]
fn report_error(ctx: StorageErrorContext) -> ReportOutcome {
    report_error_with_candidate_reads(ctx, None)
}

/// Shared body of [`report_error_with_candidate_reads`] and [`report_sign_in_not_persisted`] — the same consent-gated,
/// deferral-and-replay path, parameterised over the [`StorageExtra`] each carries.
fn report_error_with_extra(ctx: StorageErrorContext, extra: StorageExtra) -> ReportOutcome {
    let outcome = if send_now(ctx, now_ms(), extra.clone()) {
        ReportOutcome::Sent
    } else if should_defer() {
        defer(ctx, extra);
        ReportOutcome::Deferred
    } else {
        ReportOutcome::Dropped
    };
    // Every value here is a closed enum code — see the module doc's "no key material, no
    // identity" guarantee — so this line is scrub-safe by construction, unlike a report body that
    // has to be built and gated before it can be inspected. It is what let the 2026-09-10 TV
    // telemetry proof (scenario 1) confirm a report's stage/outcome directly instead of inferring
    // it from an empty spool.
    crate::log(&format!(
        "storage report: stage={} class={} outcome={}",
        ctx.stage.code(),
        ctx.class.code(),
        match outcome {
            ReportOutcome::Sent => "sent",
            ReportOutcome::Deferred => "deferred",
            ReportOutcome::Dropped => "dropped",
        }
    ));
    outcome
}

/// **Issue #76's third half**: a fresh sign-in's session file could not be kept the way
/// `plex::session::save` decided to keep it, reported the same way every other storage failure is
/// — through [`report_error_with_candidate_reads`]'s existing consent-gated, deferral-and-replay path. Takes plain wire
/// words rather than the save lane's own enum so this module compiles independently of the lane
/// that produces them (`plex::session.rs`, out of this lane's file scope) — see [`StorageExtra`]'s
/// own doc for the vocabulary.
///
/// `class`/`registered_with_app_id`/`registered_with_name` are read live
/// (`plex::session::storage_class`/`crate::keymanager`'s own probed facts) so this report agrees
/// with a crash report or a usage event built the same moment; `refused_marker` is left `false`
/// here rather than guessed, since `plex::session::has_refused_marker` is private to that module —
/// a caller that knows better may still set it by building a `StorageErrorContext` directly and
/// calling [`report_error_with_candidate_reads`].
///
/// **Deduped once per process per (`persist_outcome`, `preserve_reason`) pair** — review finding,
/// 2026-09-11. This used to go straight to [`report_error_with_extra`] with no dedup at all, alone
/// among this app's storage reports: every other one is routed through
/// `plex::session::report_once`, whose "exactly once per process per stage" rule exists so that a
/// television answering the same refusal call after call does not fill the spool with the same
/// report. Here the repeat was structural rather than incidental — `auth::take_ready` commits a
/// sign-in on every profile switch and on `resume_stored`, so an install steadily returning
/// `PreservedExistingSecure(NotProven)` raised one on every switch for the life of the process.
///
/// Keyed on the PAIR rather than on the stage, because the stage is the same word for every one of
/// them and the pair is what actually distinguishes two different things having gone wrong. Same
/// three-way split as `report_once`: `Sent`/`Deferred` are "this shape is spoken for"
/// ([`REPORTED_SIGN_IN`]), `Dropped` is remembered apart ([`DROPPED_SIGN_IN`]) so a real "Yes"
/// later in the same process gives the shape its one attempt back ([`retry_dropped_sign_in`]).
/// [`ReportOutcome::Dropped`] is also what a suppressed duplicate answers: nothing left this
/// television and nothing is waiting to.
///
/// Called from `auth::commit_sign_in_persist` — i.e. from `take_ready` only, not from the
/// discovery thread's earlier save.
pub(crate) fn report_sign_in_not_persisted(
    persist_outcome: &'static str,
    preserve_reason: Option<&'static str>,
    candidate_reads: String,
) -> ReportOutcome {
    let shape = (persist_outcome, preserve_reason);
    if REPORTED_SIGN_IN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(&shape)
        || DROPPED_SIGN_IN
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&shape)
    {
        return ReportOutcome::Dropped;
    }
    let ctx = StorageErrorContext {
        stage: StorageStage::SignInNotPersisted,
        service_error_code: None,
        class: crate::plex::session::storage_class(),
        refused_marker: false,
        key_outcome: None,
        registered_with_app_id: crate::keymanager::registered_with_app_id(),
        registered_with_name: crate::keymanager::registered_with_name(),
        sealed_identity: None,
    };
    let outcome = report_error_with_extra(
        ctx,
        StorageExtra {
            persist_outcome: Some(persist_outcome),
            preserve_reason,
            candidate_reads: Some(candidate_reads),
        },
    );
    match outcome {
        ReportOutcome::Sent | ReportOutcome::Deferred => {
            REPORTED_SIGN_IN.lock().unwrap_or_else(|e| e.into_inner()).push(shape);
        }
        ReportOutcome::Dropped => {
            DROPPED_SIGN_IN.lock().unwrap_or_else(|e| e.into_inner()).push(shape);
        }
    }
    outcome
}

/// Which (`persist_outcome`, `preserve_reason`) shapes this PROCESS has already reported as
/// [`StorageStage::SignInNotPersisted`] — the twin of `plex::session`'s `REPORTED_STAGES`, keyed on
/// the pair because the stage word is the same for all of them. A shape lands here only once its
/// attempt was actually SENT or safely DEFERRED for a later replay.
static REPORTED_SIGN_IN: std::sync::Mutex<Vec<(&'static str, Option<&'static str>)>> =
    std::sync::Mutex::new(Vec::new());

/// …and the twin of `DROPPED_STAGES`, for the same reason it is kept apart: a shape whose attempt
/// was dropped outright (a stored "No", or a build with no Sentry endpoint) must not be retried on
/// every later profile switch, but nothing was sent and nothing is waiting to be, so it is not the
/// same fact as "reported". [`retry_dropped_sign_in`] is what gives it its one attempt back.
static DROPPED_SIGN_IN: std::sync::Mutex<Vec<(&'static str, Option<&'static str>)>> =
    std::sync::Mutex::new(Vec::new());

/// **End the signed-out account's tenure over this module's in-memory state** — review finding,
/// 2026-09-11. Called from `telemetry::forget`, beside `signin::forget_storage_outcome` and the
/// twin `diag::clear_deferred` that has been on that path from the start.
///
/// Two things go, and both belong to the account that is leaving:
///
/// * [`DEFERRED`] — a report HELD because the consent question was still open. Without this it
///   survived the sign-out and `replay_deferred` sent it the moment the NEXT account answered Yes,
///   stamped with that account's freshly minted Crash report ID. `PRIVACY.md` promises a sign-out
///   "destroys everything queued at once", and this module's own doc enumerates exactly two exits
///   for a held report — a No, or the app closing. A sign-out was neither, and the held report
///   crossed an account boundary it may never cross. The queue is DROPPED, not replayed: the
///   decision that could have authorised it is gone with the account.
/// * both sign-in dedup sets, so the next account's own first unpersisted sign-in is reported
///   rather than suppressed by a shape the previous account's install already raised.
pub(crate) fn forget() {
    DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    REPORTED_SIGN_IN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    DROPPED_SIGN_IN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// **A real "Yes" gives a dropped sign-in report its one attempt back** — the exact twin of
/// `plex::session::retry_dropped_stages`, called from the same persistence operation and for
/// the same reason: a "No" must not burn a report for the rest of the process after the SAME
/// process later turns Errors on in Settings.
pub(crate) fn retry_dropped_sign_in() {
    DROPPED_SIGN_IN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

/// Report a storage failure with the candidate summary owned by its load. Session's cold-load
/// path uses this explicit form when draining its detached report batch; unlike the removed
/// global hand-off, it cannot be claimed by a probe report or a concurrent save.
pub(crate) fn report_error_with_candidate_reads(
    ctx: StorageErrorContext,
    candidate_reads: Option<String>,
) -> ReportOutcome {
    report_error_with_extra(
        ctx,
        StorageExtra {
            candidate_reads,
            ..StorageExtra::default()
        },
    )
}

/// Drain and replay every storage-error report held because consent had not yet settled the
/// question. Called after a new decision becomes effective — same shape and
/// same reason as `diag::replay_deferred`: a "yes" lets the held reports through [`send_now`]
/// exactly as if the question had already been answered when the failure was first found; a "no"
/// (or a still-pending extension — see [`should_defer`]) hits the same gate [`send_now`] always
/// enforces and is dropped with no send. Either way the queue empties: a refused answer must not
/// leak a stale queue into whichever decision comes next, same reasoning as `diag`'s twin.
pub(crate) fn replay_deferred() {
    let held: Vec<Deferred> =
        std::mem::take(&mut *DEFERRED.lock().unwrap_or_else(|e| e.into_inner()));
    if held.is_empty() {
        return;
    }
    let n = held.len();
    // `send_now`'s gate (`consent::allows_errors_at(6)`) is the SAME question for every held
    // report, so one decision answers all of them the same way here — there is no shape where
    // some are sent and others dropped in one replay.
    let mut any_sent = false;
    for d in held {
        if send_now(d.ctx, d.occurred_at_ms, d.extra) {
            any_sent = true;
        }
    }
    crate::log(&format!(
        "storage report: replayed {n} deferred ({})",
        if any_sent { "sent" } else { "dropped" }
    ));
}

/// Representative handled-error payload built through the real serialiser, for the consent screen's
/// preview. No consent-time identifier is minted.
///
/// The shape a real report actually takes for a recoverable lock: `write_refused_marker` runs
/// before `class`/`refused_marker` are ever read (`plex::session::locked`'s doc), so `class` is
/// always [`SecureRefused`](SessionStorageClass::SecureRefused) here, never the narrower
/// `SecureLocked` — and `service_error_code` is a real sample rather than `None`, since the field
/// the notices promise ("the numeric error code the key service replied with") must actually show
/// up in the one payload a person can inspect before consenting to send it.
pub(crate) fn preview_event() -> Vec<u8> {
    event_body_with_extra(
        "<random per-error event id>",
        "<running ELF build id>",
        Some(super::native::PREVIEW_USER_ID),
        0,
        StorageErrorContext {
            stage: StorageStage::EnvelopeLocked,
            service_error_code: Some(-10001),
            class: SessionStorageClass::SecureRefused,
            refused_marker: true,
            key_outcome: Some(crate::keymanager::KeyOutcome::Existed),
            registered_with_app_id: crate::keymanager::registered_with_app_id(),
            registered_with_name: crate::keymanager::registered_with_name(),
            // The preview is a REPRESENTATIVE report, and the representative case is an install
            // whose envelope was sealed by the anonymous registration every build before
            // 2026-09-10 used — so the field the notices promise ("which identity sealed it")
            // shows a real word rather than the absence one.
            sealed_identity: Some(crate::keymanager::Identity::Anonymous),
        },
        &StorageExtra {
            persist_outcome: Some("preserved_existing_secure"),
            preserve_reason: Some("not_proven"),
            candidate_reads: Some(
                "developer:open_failed:13,internal:missing,app_dir:plaintext".into(),
            ),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> StorageErrorContext {
        StorageErrorContext {
            stage: StorageStage::EnvelopeLocked,
            service_error_code: None,
            class: SessionStorageClass::SecureLocked,
            refused_marker: false,
            key_outcome: None,
            registered_with_app_id: false,
            registered_with_name: false,
            sealed_identity: None,
        }
    }

    #[test]
    fn every_stage_and_class_code_is_distinct_and_snake_case() {
        // Driven off `ALL` (compile-time exhaustive, see `_assert_all_variants_covered`) rather
        // than a hand-written literal — the literal had already drifted from the live enum twice
        // (review finding, 2026-09-10: `IdentityUnavailable`/`SecureUnavailable` were both absent).
        let mut codes: Vec<&str> = StorageStage::ALL.iter().map(|s| s.code()).collect();
        for c in &codes {
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        }
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "duplicate stage code");

        let mut ccodes: Vec<&str> = SessionStorageClass::ALL.iter().map(|c| c.code()).collect();
        for c in &ccodes {
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        }
        ccodes.sort_unstable();
        let n = ccodes.len();
        ccodes.dedup();
        assert_eq!(ccodes.len(), n, "duplicate class code");
    }

    /// **The report says which identity sealed the ENVELOPE it is about**, in the same closed
    /// vocabulary the envelope, the probe file and the proven marker all record — plus `none` for
    /// a report that is not about a sealed thing at all (a write that never happened, a candidate
    /// removed unread).
    #[test]
    fn the_sealed_identity_is_reported_in_its_own_closed_vocabulary() {
        for (identity, code) in [
            (Some(crate::keymanager::Identity::AppId), "app_id"),
            (Some(crate::keymanager::Identity::Named), "named"),
            (Some(crate::keymanager::Identity::Anonymous), "anonymous"),
            (None, "none"),
        ] {
            let ctx = StorageErrorContext {
                sealed_identity: identity,
                ..context()
            };
            let v: Value = serde_json::from_slice(&event_body(
                &"a".repeat(32),
                "0123456789abcdef",
                Some(&"e".repeat(32)),
                1_725_000_000_000,
                ctx,
            ))
            .expect("event JSON");
            assert_eq!(v["contexts"]["storage"]["sealed_identity"], code);
        }
    }

    /// **`sealed_identity` and the two owner-hint bools answer DIFFERENT questions**, which is the
    /// whole reason the field was added: the bools describe the registration THIS LAUNCH latched,
    /// while `sealed_identity` describes the identity recorded in the envelope or probe the report
    /// is about. A launch that can register as an application service but is reporting an envelope
    /// sealed anonymously is exactly issue #76's failure, and one report has to be able to say so.
    #[test]
    fn the_sealed_identity_is_not_this_launchs_registration() {
        let ctx = StorageErrorContext {
            registered_with_app_id: true,
            registered_with_name: false,
            sealed_identity: Some(crate::keymanager::Identity::Anonymous),
            ..context()
        };
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            ctx,
        ))
        .expect("event JSON");
        assert_eq!(v["contexts"]["storage"]["registered_with_app_id"], true);
        assert_eq!(v["contexts"]["storage"]["sealed_identity"], "anonymous");
    }

    /// `Identity`'s closed vocabulary, driven off `ALL` for the reason
    /// [`every_stage_and_class_code_is_distinct_and_snake_case`] is — a hand-written literal here
    /// had already drifted once, when `named` was added.
    #[test]
    fn every_identity_code_is_distinct_and_snake_case() {
        let mut codes: Vec<&str> = crate::keymanager::Identity::ALL
            .iter()
            .map(|i| i.code())
            .collect();
        for c in &codes {
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'), "{c}");
        }
        assert!(
            !codes.contains(&"none"),
            "`none` is the report's word for the ABSENCE of an identity and must stay unclaimed"
        );
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "duplicate identity code");
    }

    /// Issue #76's identity decider: `KeyOutcome`'s own closed vocabulary — same shape as
    /// [`every_stage_and_class_code_is_distinct_and_snake_case`], for the enum this module reads
    /// from `keymanager` rather than owns.
    #[test]
    fn every_key_outcome_code_is_distinct_and_snake_case() {
        let outcomes = [
            crate::keymanager::KeyOutcome::Existed,
            crate::keymanager::KeyOutcome::Created,
        ];
        let mut codes: Vec<&str> = outcomes.iter().map(|o| o.code()).collect();
        for c in &codes {
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        }
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "duplicate key-outcome code");
    }

    /// `key_outcome` is present in `contexts.storage` only when the report actually carries one —
    /// same "optional and absent, never null" contract [`service_error_code_is_present_only_when_supplied`]
    /// pins.
    #[test]
    fn key_outcome_is_present_only_when_supplied() {
        let without: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            context(),
        ))
        .expect("event JSON");
        assert!(without["contexts"]["storage"].get("key_outcome").is_none());

        let with: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            StorageErrorContext {
                key_outcome: Some(crate::keymanager::KeyOutcome::Created),
                ..context()
            },
        ))
        .expect("event JSON");
        assert_eq!(with["contexts"]["storage"]["key_outcome"], "created");
    }

    /// Same PLX-NATIVE-F gap as playback/signin: a handled storage error must carry the crash
    /// path's `hardware`/`webos` contexts, not just its own `storage` context.
    #[test]
    fn handled_storage_error_carries_the_same_hardware_context_as_a_crash() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            context(),
        ))
        .expect("event JSON");
        assert!(v["contexts"]["hardware"].is_object(), "no hardware: {v}");
        assert!(v["contexts"]["hardware"]["rtkmem"].is_string());
        assert!(v["contexts"]["hardware"]["install"].is_string());
        assert!(v["contexts"]["hardware"]["soc"].is_string());
        assert_eq!(v["contexts"]["webos"]["type"], "webos");
        assert!(v["contexts"]["storage"].is_object());
    }

    #[test]
    fn event_body_top_level_and_context_keys_are_exact() {
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
            1_725_000_000_000,
            context(),
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
        assert_eq!(v["timestamp"], 1_725_000_000.0);
        assert_eq!(keys(&v["contexts"]), ["hardware", "storage", "webos"]);
        assert_eq!(
            keys(&v["contexts"]["webos"]),
            ["api", "codename", "name", "release", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["hardware"]),
            ["install", "model", "revision", "rtkmem", "soc", "type"]
        );
        assert_eq!(
            keys(&v["contexts"]["storage"]),
            [
                "class",
                "refused_marker",
                "registered_with_app_id",
                "registered_with_name",
                "sealed_identity",
                "stage",
                "type"
            ]
        );
        assert_eq!(keys(&v["tags"]), ["storage.class", "storage.stage"]);
        assert_eq!(v["exception"]["values"][0]["type"], "StorageError");
        assert_eq!(
            v["fingerprint"],
            serde_json::json!(["storage-error", "envelope_locked"])
        );
        assert_eq!(v["contexts"]["storage"]["class"], "secure_locked");
        assert_eq!(v["contexts"]["storage"]["refused_marker"], false);
    }

    /// Twin of `signin`'s: a zero `occurred_at_ms` must omit `timestamp` entirely rather than
    /// sending a fabricated 1970 stamp (review finding, 2026-09-10).
    #[test]
    fn a_zero_occurred_at_produces_no_timestamp_key() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            0,
            context(),
        ))
        .expect("event JSON");
        assert!(v.get("timestamp").is_none(), "expected no timestamp key, got {v}");
    }

    /// **The two owner hints are separate fields, and a report has to tell them apart.** They are
    /// two different keys of LG's key-manager ownership rule (application id > sender service
    /// name), and issue #76's open question is precisely which of them a reporter's firmware
    /// grants — so a set that refuses the application-service registration and grants the plain
    /// bus name must be readable as exactly that off one report.
    #[test]
    fn the_report_tells_the_two_owner_hints_apart() {
        for (app_id, name) in [(false, false), (true, false), (false, true)] {
            let ctx = StorageErrorContext {
                registered_with_app_id: app_id,
                registered_with_name: name,
                ..context()
            };
            let v: Value = serde_json::from_slice(&event_body(
                &"a".repeat(32),
                "0123456789abcdef",
                Some(&"e".repeat(32)),
                1_725_000_000_000,
                ctx,
            ))
            .expect("event JSON");
            assert_eq!(v["contexts"]["storage"]["registered_with_app_id"], app_id);
            assert_eq!(v["contexts"]["storage"]["registered_with_name"], name);
        }
    }

    #[test]
    fn service_error_code_is_present_only_when_supplied() {
        let without: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            context(),
        ))
        .expect("event JSON");
        assert!(without["contexts"]["storage"]
            .get("service_error_code")
            .is_none());

        let with: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            StorageErrorContext {
                service_error_code: Some(-1027),
                ..context()
            },
        ))
        .expect("event JSON");
        assert_eq!(with["contexts"]["storage"]["service_error_code"], -1027);
    }

    /// No 32-hex identifier appears anywhere in the preview beyond the fixed placeholder — same
    /// style as `signin::tests::preview_contains_no_real_identifier`.
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
    fn preview_event_parses_and_is_a_storage_error() {
        let v: Value = serde_json::from_slice(&preview_event()).expect("preview JSON");
        assert_eq!(v["exception"]["values"][0]["type"], "StorageError");
        let storage = &v["contexts"]["storage"];
        assert_eq!(storage["persist_outcome"], "preserved_existing_secure");
        assert_eq!(storage["preserve_reason"], "not_proven");
        assert_eq!(
            storage["candidate_reads"],
            "developer:open_failed:13,internal:missing,app_dir:plaintext"
        );
    }

    /// No key material, ciphertext or plaintext is a value this module can even represent — the
    /// declaration region carries no owned string field, same shape as
    /// `diag::schema::tests::no_variant_can_carry_a_runtime_string`, so this is a source grep rather
    /// than a runtime assertion.
    ///
    /// **`candidate_reads` is the one deliberate exception (issue #76 report lane, 2026-09-11)** —
    /// a bounded, closed-vocabulary summary of which candidate session paths were read and how
    /// (`developer:open_failed:13,internal:missing,app_dir:plaintext`), built only from the read
    /// lane's own category/rejection codes and small bounded numbers, never a path or free text.
    /// The rule stays for every other field, which is what a NEW owned string here still fails on.
    #[test]
    fn no_field_can_carry_a_runtime_string() {
        let src = include_str!("storage.rs");
        let from = src
            .find("pub(crate) enum SessionStorageClass")
            .expect("the class enum");
        let to = src
            .find("pub(crate) fn event_body")
            .expect("the body builder boundary");
        let decls = &src[from..to];
        for (i, line) in decls.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("candidate_reads") {
                continue;
            }
            assert!(
                !code.contains("String") && !code.contains("Cow<"),
                "line {} of storage.rs's declaration region introduces an owned string: {line}",
                i + 1
            );
        }
    }

    /// `report_error` needs standing error-report consent AND a build that carries a Sentry
    /// endpoint — same gate as `signin::report_error`/`playback::report_error`. In a dev checkout
    /// with no `pkg/telemetry.local.json` there is no endpoint, so this must refuse to SEND. It may
    /// still hold the report in memory (see the issue #76 defer tests below), which is fine and
    /// deliberately not asserted against here — this test only pins the "never sent" half.
    #[test]
    fn report_error_needs_consent_and_an_endpoint() {
        // Mutates the crate-global `DEFERRED` queue (via `report_error` -> `defer`) without a
        // guard was a cross-process/cross-test flake in the same family as the log-path one below
        // (review finding, 2026-09-10) — every neighbouring test in this module takes the lock.
        let _g = crate::testlock::serial();
        reset();
        if super::super::sender::has_sentry() {
            return;
        }
        assert_ne!(report_error(context()), ReportOutcome::Sent);
    }

    // ---- issue #76 storage telemetry, take 2: a report found before the reporting question is
    // answered is HELD (deferred), not dropped, and is replayed once the question is answered.
    // These mirror `diag::mod`'s sign-in defer tests in shape: they drive `should_defer`/`defer`/
    // `replay_deferred` directly against synthetic consent states, and they observe what
    // `send_now` was actually asked to send through `record_send_attempt` — a test-only hook, the
    // same trick `plex::session`'s own `report_storage_error` test double uses, and for the same
    // reason: no dev checkout carries a compiled Sentry endpoint, so the real network send can
    // never be the thing under test here (`report_error_needs_consent_and_an_endpoint`, above,
    // already pins that). What CAN be tested end to end without one is the gating decision this
    // module exists to get right: held vs dropped vs sent-now.

    thread_local! {
        static SEND_ATTEMPTS: std::cell::RefCell<Vec<(StorageErrorContext, u64, StorageExtra)>> =
            std::cell::RefCell::new(Vec::new());
    }

    pub(super) fn record_send_attempt(
        ctx: StorageErrorContext,
        occurred_at_ms: u64,
        extra: StorageExtra,
    ) {
        SEND_ATTEMPTS.with(|s| s.borrow_mut().push((ctx, occurred_at_ms, extra)));
    }

    fn send_attempts() -> Vec<(StorageErrorContext, u64, StorageExtra)> {
        SEND_ATTEMPTS.with(|s| s.borrow().clone())
    }

    fn clear_send_attempts() {
        SEND_ATTEMPTS.with(|s| s.borrow_mut().clear());
    }

    fn deferred_len() -> usize {
        DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn deferred_stages() -> Vec<StorageStage> {
        DEFERRED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|d| d.ctx.stage)
            .collect()
    }

    fn clear_deferred() {
        DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    fn unanswered() -> super::super::consent::Consent {
        super::super::consent::Consent::default()
    }

    fn errors_on_at_scope_6() -> super::super::consent::Consent {
        super::super::consent::Consent {
            asked_version: super::super::consent::POLICY_VERSION,
            errors: true,
            errors_scope: 6,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        }
    }

    fn errors_on_pending_scope_6_extension() -> super::super::consent::Consent {
        super::super::consent::Consent {
            asked_version: super::super::consent::POLICY_VERSION,
            errors: true,
            errors_scope: 4,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        }
    }

    fn stored_no() -> super::super::consent::Consent {
        super::super::consent::Consent {
            asked_version: super::super::consent::POLICY_VERSION,
            errors: false,
            ..Default::default()
        }
    }

    /// Fresh state for one of these tests: no consent published, nothing held, nothing sent.
    /// Callers hold `crate::testlock::serial()` for the whole test — these are crate globals.
    fn reset() {
        clear_deferred();
        clear_send_attempts();
        clear_sign_in_dedup();
    }

    /// The (save outcome, preserve reason) dedup sets are process-global and deliberately have no
    /// production clear short of sign-out, so every test that raises a `SignInNotPersisted` report
    /// has to start from an empty one or inherit the previous test's suppression.
    fn clear_sign_in_dedup() {
        super::REPORTED_SIGN_IN.lock().unwrap_or_else(|e| e.into_inner()).clear();
        super::DROPPED_SIGN_IN.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// (a) An unanswered install's locked-read context is HELD rather than dropped, and a later
    /// "yes" at scope 6 sends exactly the one context that was held — same stage, same service
    /// error code — dated to when it was ORIGINALLY found, not to when the answer arrived.
    #[test]
    fn an_unanswered_install_defers_and_a_later_yes_sends_the_original_report() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(unanswered());

        let ctx = StorageErrorContext {
            stage: StorageStage::EnvelopeLocked,
            service_error_code: Some(-10001),
            class: SessionStorageClass::SecureRefused,
            refused_marker: true,
            key_outcome: Some(crate::keymanager::KeyOutcome::Existed),
            registered_with_app_id: false,
            registered_with_name: false,
            sealed_identity: None,
        };
        assert_ne!(report_error(ctx), ReportOutcome::Sent, "must not claim to have sent it");
        assert_eq!(deferred_len(), 1, "the report must be HELD, not dropped");
        assert!(send_attempts().is_empty(), "nothing may be attempted yet");

        super::super::consent::install(errors_on_at_scope_6());
        replay_deferred();

        assert_eq!(deferred_len(), 0, "the queue empties on replay");
        let sent = send_attempts();
        assert_eq!(sent.len(), 1, "exactly one report must be replayed");
        assert_eq!(sent[0].0.stage, StorageStage::EnvelopeLocked);
        assert_eq!(sent[0].0.service_error_code, Some(-10001));
        assert_eq!(sent[0].0.class, SessionStorageClass::SecureRefused);
        assert!(sent[0].0.refused_marker);

        reset();
        super::super::consent::install(unanswered());
    }

    #[test]
    fn requested_scope_enable_defers_until_it_is_effective() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(stored_no());
        super::super::consent::request_for_test(errors_on_at_scope_6());
        assert_ne!(report_error(context()), ReportOutcome::Sent);
        assert_eq!(deferred_len(), 1, "pending enable was confused with a stored No");
        assert!(send_attempts().is_empty());
        reset();
        super::super::consent::install(unanswered());
    }

    /// (b) The same held report, but the eventual answer is "no": the queue empties with NOTHING
    /// sent.
    #[test]
    fn a_later_no_empties_the_deferred_queue_with_nothing_sent() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(unanswered());

        assert_ne!(report_error(context()), ReportOutcome::Sent);
        assert_eq!(deferred_len(), 1);

        super::super::consent::install(stored_no());
        replay_deferred();

        assert_eq!(deferred_len(), 0);
        assert!(send_attempts().is_empty(), "a No must send nothing");

        reset();
        super::super::consent::install(unanswered());
    }

    /// (c) An install already at Errors scope 6 attempts the send immediately — nothing is ever
    /// held.
    #[test]
    fn errors_on_at_scope_6_sends_immediately_with_no_deferral() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(errors_on_at_scope_6());

        report_error(context());

        assert_eq!(deferred_len(), 0, "nothing may be held when the gate already passes");
        assert_eq!(
            send_attempts().len(),
            1,
            "the gate having passed means a send was actually attempted"
        );

        reset();
        super::super::consent::install(unanswered());
    }

    /// (d) A stored "no" never queues at all — a real decision stays dropped, exactly as before
    /// issue #76's defer.
    #[test]
    fn a_stored_no_never_queues() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(stored_no());

        assert_ne!(report_error(context()), ReportOutcome::Sent);

        assert_eq!(deferred_len(), 0, "a real No must not be held");
        assert!(send_attempts().is_empty());

        reset();
        super::super::consent::install(unanswered());
    }

    /// (d.2) An already-declined scope-6 extension is the other real "no" shape — the channel
    /// stays on at its old scope, but this report's own scope was explicitly ruled out, so it must
    /// not be held either.
    #[test]
    fn a_declined_scope_extension_never_queues() {
        let _g = crate::testlock::serial();
        reset();
        let mut declined = errors_on_pending_scope_6_extension();
        declined.errors_declined_scope = 6;
        super::super::consent::install(declined);

        assert_ne!(report_error(context()), ReportOutcome::Sent);
        assert_eq!(deferred_len(), 0);
        assert!(send_attempts().is_empty());

        reset();
        super::super::consent::install(unanswered());
    }

    /// (c.2) The "pending extension" shape itself — errors already on, but scope 6 is a genuinely
    /// open question (not yet declined) — is held exactly like a fully unanswered install.
    #[test]
    fn a_pending_scope_extension_defers_like_an_unanswered_install() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(errors_on_pending_scope_6_extension());

        assert_ne!(report_error(context()), ReportOutcome::Sent);
        assert_eq!(deferred_len(), 1, "a pending extension gets the same second chance");

        reset();
        super::super::consent::install(unanswered());
    }

    /// (e) The cap holds: a fifth held report drops the OLDEST, keeping the newest
    /// [`DEFERRED_CAP`].
    #[test]
    fn the_deferred_cap_drops_the_oldest() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(unanswered());

        let stages = [
            StorageStage::GenerateKey,
            StorageStage::BeginEncrypt,
            StorageStage::FinishEncrypt,
            StorageStage::BeginDecrypt,
            StorageStage::FinishDecrypt,
        ];
        assert_eq!(stages.len(), DEFERRED_CAP + 1, "exercise exactly one past the cap");
        for stage in stages {
            report_error(StorageErrorContext { stage, ..context() });
        }

        assert_eq!(deferred_len(), DEFERRED_CAP, "the cap must hold");
        assert_eq!(
            deferred_stages(),
            &stages[1..],
            "the oldest (GenerateKey) must be the one dropped; the rest keep order"
        );

        reset();
        super::super::consent::install(unanswered());
    }

    /// **The TV telemetry proof (2026-09-10, scenario 1) had to infer the stage/outcome of a
    /// storage report from an empty spool, because nothing logs it.** `report_error` must log one
    /// line per call naming the stage, class and outcome — scrub-safe (every value here is a
    /// fixed closed-enum code, never free text).
    #[test]
    fn report_error_logs_stage_class_and_outcome() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(stored_no());

        let logged = crate::with_test_log(|_p| {
            let outcome = report_error(StorageErrorContext {
                stage: StorageStage::WriteFailed,
                ..context()
            });
            assert_eq!(outcome, ReportOutcome::Dropped);
            std::fs::read_to_string(_p).unwrap_or_default()
        });
        assert!(
            logged.contains("storage report: stage=write_failed class=secure_locked outcome=dropped"),
            "missing the stage/class/outcome line: {logged}"
        );

        reset();
        super::super::consent::install(unanswered());
    }

    /// **The same proof's scenario 2 had to infer a deferral from an empty spool plus no flush
    /// line.** `replay_deferred` must log the count and result when it actually replays something.
    #[test]
    fn replay_deferred_logs_the_count_and_result() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(unanswered());

        assert_ne!(report_error(context()), ReportOutcome::Sent);
        assert_eq!(deferred_len(), 1);

        let logged = crate::with_test_log(|_p| {
            super::super::consent::install(stored_no());
            replay_deferred();
            std::fs::read_to_string(_p).unwrap_or_default()
        });
        assert!(
            logged.contains("storage report: replayed 1 deferred (dropped)"),
            "missing the replay summary line: {logged}"
        );

        reset();
        super::super::consent::install(unanswered());
    }

    /// **Every field this report puts on the wire, as a list somebody had to type** (review
    /// finding, 2026-09-11). Adding a field to [`StorageErrorContext`] is a consent decision with
    /// two possible answers (`telemetry::consent`'s [`POLICY_VERSION`](super::super::consent) rule),
    /// and neither of them is "nothing happens" — but nothing in the compiler could tell the two
    /// apart, so a field could reach a stranger's Sentry project having been graded by no one. The
    /// list below is the record of that grading: a new key fails here, naming both routes, and the
    /// person who added it picks one deliberately.
    ///
    /// Both directions are pinned. A key that appears and is not listed is the case above; a
    /// listed key that stops appearing means the notice now describes a field this report no
    /// longer sends, which is the same document going stale from the other end.
    #[test]
    fn the_storage_report_fields_are_a_documented_list() {
        // Every optional field populated, so the whole surface is present in one body.
        let ctx = StorageErrorContext {
            stage: StorageStage::BeginDecrypt,
            service_error_code: Some(-20030),
            class: SessionStorageClass::Secure,
            refused_marker: true,
            key_outcome: Some(crate::keymanager::KeyOutcome::Existed),
            registered_with_app_id: false,
            registered_with_name: true,
            sealed_identity: Some(crate::keymanager::Identity::Named),
        };
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            ctx,
        ))
        .expect("event JSON");
        let mut keys: Vec<&str> = v["contexts"]["storage"]
            .as_object()
            .expect("the storage context is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        // Graded, and each one's route recorded: every field here rides the Errors channel's
        // scope 6 ("how your sign-in is stored"). `stage`/`class`/`refused_marker`/
        // `service_error_code`/`key_outcome` are what scope 6 was ASKED for;
        // `registered_with_app_id`/`registered_with_name`/`sealed_identity` came later, inside
        // that same purpose, and bumped `NOTICE_REVISION` instead (2, 3). `type` is the body's own
        // discriminator, not a collected fact.
        let mut documented = [
            "class",
            "key_outcome",
            "refused_marker",
            "registered_with_app_id",
            "registered_with_name",
            "sealed_identity",
            "service_error_code",
            "stage",
            "type",
        ];
        documented.sort_unstable();
        assert_eq!(
            keys,
            documented,
            "the storage report's fields changed. This is a consent decision with two answers and \
             no third: if the new field widens WHAT is collected past \"how your sign-in is \
             stored\", bump `consent::ERRORS_SCOPE` and add its `SCOPE_CHANGES` row (that re-asks \
             the people who have crash reports on); if it is a new fact INSIDE that same purpose, \
             bump `consent::NOTICE_REVISION` and describe it in `PRIVACY.md` and `ui::legal` (that \
             re-asks nobody). Then list it here."
        );
    }

    /// **The issue #76 report-lane fields are a documented list of their own** (`StorageExtra`,
    /// carried alongside a context rather than on it — see that type's doc for why). Same routing
    /// decision as [`the_storage_report_fields_are_a_documented_list`]: all three answer "how your
    /// sign-in is stored" — the exact purpose Errors scope 6 already covers — so they bumped
    /// `consent::NOTICE_REVISION` (4) rather than `consent::ERRORS_SCOPE`, and are described in
    /// `PRIVACY.md`/`ui::legal` instead of re-asking anyone.
    #[test]
    fn the_storage_extra_fields_are_present_only_when_supplied() {
        let without: Value = serde_json::from_slice(&event_body_with_extra(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            context(),
            &StorageExtra::default(),
        ))
        .expect("event JSON");
        for key in ["persist_outcome", "preserve_reason", "candidate_reads"] {
            assert!(
                without["contexts"]["storage"].get(key).is_none(),
                "an empty StorageExtra must omit {key}, not send a placeholder"
            );
        }

        let with: Value = serde_json::from_slice(&event_body_with_extra(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
            1_725_000_000_000,
            context(),
            &StorageExtra {
                persist_outcome: Some("preserved_existing_secure"),
                preserve_reason: Some("not_proven"),
                candidate_reads: Some("developer:open_failed:13,internal:missing".to_string()),
            },
        ))
        .expect("event JSON");
        assert_eq!(
            with["contexts"]["storage"]["persist_outcome"],
            "preserved_existing_secure"
        );
        assert_eq!(with["contexts"]["storage"]["preserve_reason"], "not_proven");
        assert_eq!(
            with["contexts"]["storage"]["candidate_reads"],
            "developer:open_failed:13,internal:missing"
        );
    }

    /// [`report_sign_in_not_persisted`] builds the `SignInNotPersisted` stage and folds its three
    /// wire words into a [`StorageExtra`] that actually reaches [`send_now`] — driven through the
    /// same defer/replay machinery as every other report, since it is `report_error_with_extra`
    /// underneath.
    #[test]
    fn report_sign_in_not_persisted_carries_its_three_wire_words() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(errors_on_at_scope_6());

        // Not asserting `ReportOutcome::Sent`: a dev checkout carries no compiled Sentry endpoint
        // (see `report_error_needs_consent_and_an_endpoint`'s own doc), so the gate this test cares
        // about — the attempt is made at all, with the right stage and extra — is what
        // `send_attempts()` (recorded before the endpoint check) can prove either way.
        let outcome = report_sign_in_not_persisted(
            "blocked_unknown_envelope",
            None,
            "developer:open_failed:13".to_string(),
        );
        assert_ne!(outcome, ReportOutcome::Deferred, "scope 6 is already accepted");

        let sent = send_attempts();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0.stage, StorageStage::SignInNotPersisted);
        assert_eq!(
            sent[0].2.persist_outcome,
            Some("blocked_unknown_envelope")
        );
        assert_eq!(sent[0].2.preserve_reason, None);
        assert_eq!(
            sent[0].2.candidate_reads.as_deref(),
            Some("developer:open_failed:13")
        );

        reset();
        super::super::consent::install(unanswered());
    }

    /// The explicit candidate-summary API carries the load-owned value on every detached report;
    /// ordinary reports carry no candidate summary.
    #[test]
    fn explicit_candidate_reads_reaches_each_owned_report() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(errors_on_at_scope_6());

        let reads = Some("internal:missing".to_string());
        report_error_with_candidate_reads(context(), reads.clone());
        let first = send_attempts();
        assert_eq!(first.len(), 1);
        assert_eq!(
            first[0].2.candidate_reads.as_deref(),
            Some("internal:missing")
        );

        report_error_with_candidate_reads(context(), reads);
        let second = send_attempts();
        assert_eq!(second.len(), 2, "the second call must still be attempted");
        assert_eq!(
            second[1].2.candidate_reads.as_deref(),
            Some("internal:missing")
        );

        // A report without an owned load summary remains intentionally empty.
        report_error(context());
        let third = send_attempts();
        assert_eq!(third[2].2.candidate_reads, None);

        reset();
        super::super::consent::install(unanswered());
    }

    #[test]
    fn sign_out_discards_deferred_storage_report_before_next_account_answers_yes() {
        let _g = crate::testlock::serial();
        reset();
        super::super::consent::install(unanswered());
        assert_ne!(report_error(context()), ReportOutcome::Sent);
        assert_eq!(deferred_len(), 1);

        // This is the production sign-out boundary; it must end the departing account's
        // in-memory telemetry tenure, not merely purge the durable spool.
        let _ = super::super::forget_with_receipt().wait_blocking();
        assert_eq!(deferred_len(), 0);

        super::super::consent::install(errors_on_at_scope_6());
        replay_deferred();
        assert!(send_attempts().is_empty());

        reset();
        super::super::consent::install(unanswered());
    }

    /// PRIVACY.md names the same closed vocabulary this module actually emits.
    ///
    /// **Driven off the live enums** — `StorageStage::ALL`, `SessionStorageClass::ALL` and
    /// `keymanager::Identity::ALL`, each compile-time exhaustive through its own
    /// `_assert_all_variants_covered` — for the reason
    /// [`every_stage_and_class_code_is_distinct_and_snake_case`] is: the hand-written literal this
    /// replaced had drifted from the enum twice already, and it carried no identity code at all,
    /// so `sealed_identity`'s three words were pinned by nothing (review finding, 2026-09-11).
    /// `KeyOutcome`'s two words stay a literal — it has no `ALL` to drive off, and a two-variant
    /// enum whose codes appear nowhere else is the one case a literal cannot silently under-report.
    #[test]
    fn privacy_names_the_closed_storage_vocabulary() {
        let privacy = include_str!("../../../PRIVACY.md");
        let vocabulary = StorageStage::ALL
            .iter()
            .map(|s| s.code())
            .chain(SessionStorageClass::ALL.iter().map(|c| c.code()))
            .chain(crate::keymanager::Identity::ALL.iter().map(|i| i.code()))
            .chain(["existed", "created"])
            // The save lane's own wire words (`plex::session.rs`, a different lane's file): not
            // driven off an owned enum here, since this module accepts them as opaque
            // `&'static str`s (see `StorageExtra`'s doc) rather than defining that vocabulary
            // itself — a literal list is the practical choice for words this module does not own.
            .chain([
                "persisted_plaintext",
                "persisted_sealed",
                "preserved_existing_secure",
                "blocked_unknown_envelope",
                "serialization_failed",
                "not_proven",
                "refused_marker_no_fresh_sign_in",
                "seal_failed",
            ])
            // The READ lane's own two closed vocabularies, which ride here as `candidate_reads`
            // (`plex::session::CandidateCategory`/`ReadRejection`). Driven off those enums rather
            // than off a literal list, so a variant added there — `Unparsable` was, on 2026-09-11,
            // and it is a word the notice must name before it can be sent — fails this test until
            // the policy names it.
            .chain(
                crate::plex::session::CandidateCategory::ALL
                    .iter()
                    .map(|c| c.wire()),
            )
            .chain(
                crate::plex::session::ReadRejection::ALL
                    .iter()
                    .map(|r| r.wire()),
            );
        for value in vocabulary {
            assert!(privacy.contains(value), "PRIVACY.md omitted {value}");
        }
        assert!(
            privacy.contains("`none`"),
            "…and the word this report sends for the ABSENCE of a sealed identity, which belongs \
             to no enum and so cannot be reached from one"
        );
    }

    // ---- Issue #76's report lane: the integrator's own call sites ------------------------------
    //
    // Both tests below drive REAL production paths (`auth::take_ready`, `plex::session::load`)
    // rather than calling this module's entry points directly, because the defect they guard is a
    // missing CALL: every function under test here compiled, passed its own lane's unit tests and
    // reported nothing at all, on every launch, because nobody called it.

    /// A temp session file of this test module's own, in one of two shapes.
    ///
    /// `unwritable` points the only candidate at a path whose parent DOES NOT EXIST, so
    /// `write_atomic` cannot even create its temp sibling: every candidate refuses and `save`
    /// answers `WriteFailed` — the one outcome that really does cost the user their sign-in at the
    /// next launch. `writable` is the ordinary healthy install, which is not a convenience but the
    /// only shape that can show the `candidate_reads` LEAK: a load that queues no report of its own
    /// leaves the hand-off slot unclaimed.
    struct TempSession {
        dir: std::path::PathBuf,
    }

    impl TempSession {
        fn unwritable(tag: &str) -> TempSession {
            TempSession::at(tag, |dir| dir.join("gone").join("auth.json"))
        }

        fn writable(tag: &str) -> TempSession {
            TempSession::at(tag, |dir| dir.join("auth.json"))
        }

        fn at(tag: &str, file: impl Fn(&std::path::Path) -> std::path::PathBuf) -> TempSession {
            let dir = std::env::temp_dir()
                .join(format!("plxnative-storage-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("a writable temp dir");
            crate::plex::session::redirect_for_test(Some(file(&dir)));
            // `report_once`'s REPORTED/DROPPED sets are PROCESS globals: a stage some earlier test
            // in this binary already reported is one the load below would silently skip.
            crate::plex::session::reset_report_state_for_test();
            reset();
            // Errors at scope 6, so `send_now` gets past its own consent gate and records the
            // attempt — the same arrangement every gating test in this module makes.
            super::super::consent::install(errors_on_at_scope_6());
            TempSession { dir }
        }
    }

    impl Drop for TempSession {
        fn drop(&mut self) {
            crate::plex::session::redirect_for_test(None);
            crate::plex::session::reset_report_state_for_test();
            crate::auth::reset_ctl_for_test();
            super::super::consent::install(unanswered());
            reset();
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn sign_in_session() -> crate::plex::session::Session {
        crate::plex::session::Session {
            client_id: "cid-for-test".to_string(),
            account_token: "tok-for-test".to_string(),
            ..Default::default()
        }
    }

    /// **A sign-in `take_ready` could not persist is REPORTED, carrying all three save/read
    /// facts.** Issue #76 field report case 6: the run carries on signed in (the in-memory `Ctl`
    /// holds real credentials) and the next launch asks again, so this report is the only way
    /// anyone ever learns it happened.
    #[test]
    fn take_ready_reports_a_sign_in_the_save_could_not_persist() {
        let _g = crate::testlock::serial();
        let _t = TempSession::unwritable("take-ready-report");
        // A cold load first, so `candidate_reads_wire()` describes a real read of this test's own
        // candidate rather than whatever the previous test left behind — the launch's read summary
        // is exactly what the report is supposed to carry beside the save's verdict.
        let _ = crate::plex::session::load();
        clear_send_attempts();
        clear_sign_in_dedup();

        crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::Write);

        crate::auth::arm_ready_for_test(sign_in_session());
        assert!(
            crate::auth::take_ready().is_none(),
            "save admission never waits inline"
        );
        crate::storage_worker::drain_for_test();
        assert!(
            crate::auth::take_ready().is_some(),
            "the run still gets its credentials — a failed save must not sign it out"
        );

        let attempts = send_attempts();
        let (ctx, _, extra) = attempts
            .iter()
            .find(|(c, _, _)| c.stage == StorageStage::SignInNotPersisted)
            .unwrap_or_else(|| {
                panic!("no sign_in_not_persisted report was attempted; got {attempts:?}")
            });
        assert_eq!(extra.persist_outcome, Some("write_failed"));
        assert_eq!(
            extra.preserve_reason, None,
            "a write that failed preserved nothing, so there is no reason word to give"
        );
        assert_eq!(
            extra.candidate_reads.as_deref(),
            Some("other:missing"),
            "the launch's own read summary, in the read lane's closed vocabulary"
        );
        assert_eq!(
            ctx.service_error_code, None,
            "no key service was asked anything — the file system said no on its own"
        );
    }

    /// A cold load can queue more than one storage report: the probe is checked before the
    /// session candidate, and an untrusted candidate can queue its own report afterward. Both
    /// reports must carry the same load-owned candidate summary; a later unrelated report must
    /// not inherit it.
    #[test]
    fn a_cold_load_attaches_candidates_to_probe_and_session_reports() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let t = TempSession::writable("multi-report-load");
        let auth = t.dir.join("auth.json");
        std::fs::write(&auth, b"not trusted session bytes").unwrap();
        std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o666)).unwrap();

        // ProbeFile uses `#[serde(flatten)]` for the sealed envelope fields.
        let probe = serde_json::json!({
            "backend": "keymanager3",
            "key": "plxnative.session.v1",
            "iv": "AAAAAAAAAAAAAAAAAAAAAA==",
            "data": "c2VjcmV0",
            "identity": "anonymous",
            "attempts": 2
        });
        std::fs::write(
            t.dir.join("secure-probe.json"),
            serde_json::to_vec_pretty(&probe).unwrap(),
        )
        .unwrap();
        std::fs::set_permissions(
            t.dir.join("secure-probe.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();

        crate::plex::session::reset_report_state_for_test();
        clear_send_attempts();
        crate::keymanager::arm_for_test(vec![("begin", Err(()))]);
        let _ = crate::plex::session::load();
        crate::keymanager::disarm_for_test();

        let sent = send_attempts();
        assert!(
            sent.iter().any(|(ctx, _, _)| {
                ctx.stage == StorageStage::NoReply
                    || ctx.stage == StorageStage::Unreachable
                    || ctx.stage == StorageStage::EnvelopeLocked
            }),
            "the probe report was not attempted: {:?}",
            sent.iter().map(|(ctx, _, _)| ctx.stage).collect::<Vec<_>>()
        );
        assert!(
            sent.iter().any(|(ctx, _, _)| ctx.stage == StorageStage::UntrustedMode),
            "the session-read report was not attempted: {:?}",
            sent.iter().map(|(ctx, _, _)| ctx.stage).collect::<Vec<_>>()
        );
        let load_reports: Vec<_> = sent
            .iter()
            .filter(|(ctx, _, _)| {
                matches!(
                    ctx.stage,
                    StorageStage::NoReply
                        | StorageStage::Unreachable
                        | StorageStage::EnvelopeLocked
                        | StorageStage::UntrustedMode
                )
            })
            .collect();
        assert!(load_reports.len() >= 2);
        for (_, _, extra) in &load_reports {
            assert_eq!(
                extra.candidate_reads.as_deref(),
                Some("other:untrusted_mode")
            );
        }

        report_error_with_candidate_reads(context(), None);
        let after = send_attempts();
        assert_eq!(after.last().unwrap().2.candidate_reads, None);
    }

    /// **One sign-in that cannot be persisted is ONE report, however many times a save asks.**
    ///
    /// Review finding, 2026-09-11. `note_sign_in_persist` reported from both of the sign-in flow's
    /// saves, so one attempt raised two identical reports — and `take_ready` also runs on every
    /// PROFILE SWITCH and on `resume_stored`, so an install whose save steadily returns the same
    /// non-persisting outcome spooled another one on every switch for the life of the process.
    /// Every other storage report in this app is deduped (`plex::session::report_once`); this one
    /// bypassed it entirely.
    ///
    /// Driven through the real `take_ready` twice — which is exactly what a profile switch does —
    /// against an install whose only candidate cannot be written.
    #[test]
    fn a_repeated_unpersistable_sign_in_is_reported_once_per_process() {
        let _g = crate::testlock::serial();
        let _t = TempSession::unwritable("take-ready-report-once");
        let _ = crate::plex::session::load();
        clear_send_attempts();
        clear_sign_in_dedup();

        for _ in 0..3 {
            crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::Write);
            crate::auth::arm_ready_for_test(sign_in_session());
            assert!(crate::auth::take_ready().is_none());
            crate::storage_worker::drain_for_test();
            assert!(crate::auth::take_ready().is_some());
        }

        let n = send_attempts()
            .iter()
            .filter(|(c, _, _)| c.stage == StorageStage::SignInNotPersisted)
            .count();
        assert_eq!(
            n, 1,
            "three commits of the same non-persisting outcome must raise one report, got {:?}",
            send_attempts()
        );
    }

    /// **…and the DISCOVERY-thread save is not one of the times it asks.** Both of the sign-in
    /// flow's saves used to report, which is where the doubling came from. The discovery save still
    /// records the outcome for the one-off body (`signin::note_storage_outcome`) and still logs its
    /// own line — it simply is not the user-visible commit of a sign-in, and `take_ready` is.
    #[test]
    fn the_discovery_threads_save_notes_the_outcome_without_reporting_it() {
        let _g = crate::testlock::serial();
        let _t = TempSession::unwritable("discovery-save-no-report");
        crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::Write);
        let _ = crate::plex::session::load();
        clear_send_attempts();
        clear_sign_in_dedup();

        crate::auth::note_discovery_save_for_test(
            crate::plex::session::PersistOutcome::WriteFailed,
        );

        assert!(
            send_attempts()
                .iter()
                .all(|(c, _, _)| c.stage != StorageStage::SignInNotPersisted),
            "the discovery save must raise no report of its own; got {:?}",
            send_attempts()
        );
        assert_eq!(
            super::super::signin::storage_outcome_for_test().persist_outcome,
            Some("write_failed"),
            "…while still recording what the save did, for the one-off body"
        );
    }

    /// **The `candidate_reads` hand-off reaches the load's OWN report.** `plex::session::locked`
    /// and its siblings build a `StorageErrorContext` literal with no field for this, which is why
    /// the value is handed over out of band rather than threaded through every one of them — and
    /// why nothing but an end-to-end load can prove it arrives.
    #[test]
    fn a_loads_candidate_summary_reaches_its_own_report() {
        let _g = crate::testlock::serial();
        let _t = TempSession::unwritable("candidate-reads-handoff");
        clear_send_attempts();
        clear_sign_in_dedup();

        // This load reads one missing candidate, then mints a client id and cannot write it — one
        // `WriteFailed` report raised from inside the load, which is the report the summary
        // belongs to.
        crate::storage::inject_next_commit_failure_for_test(crate::storage::CommitStage::Write);
        let _ = crate::plex::session::load();
        let attempts = send_attempts();
        let (_, _, extra) = attempts
            .iter()
            .find(|(c, _, _)| c.stage == StorageStage::WriteFailed)
            .unwrap_or_else(|| panic!("the load raised no write-failure report; got {attempts:?}"));
        assert_eq!(extra.candidate_reads.as_deref(), Some("other:missing"));
    }

    /// **…and never onto a report that is about something else.** The discriminating case is the
    /// HEALTHY load — the one that queues no report at all, so nothing claims the hand-off — which
    /// is exactly the boot every working install performs. Without `load`'s own clear, the single
    /// slot would still be holding this launch's read hours later, and the next unrelated failure
    /// (a seal failure inside some `update()`) would publish it as though it described that
    /// moment's read.
    #[test]
    fn a_healthy_loads_candidate_summary_never_leaks_onto_a_later_unrelated_report() {
        let _g = crate::testlock::serial();
        let _t = TempSession::writable("candidate-reads-no-leak");
        clear_send_attempts();
        clear_sign_in_dedup();

        let _ = crate::plex::session::load();
        assert!(
            send_attempts().is_empty(),
            "the premise: a healthy load reports nothing, so nothing claims the hand-off — got {:?}",
            send_attempts()
        );

        report_error(context());
        let later = send_attempts();
        assert_eq!(later.len(), 1, "{later:?}");
        assert_eq!(
            later[0].2.candidate_reads, None,
            "a report about something else must not publish this launch's read summary"
        );
    }
}
