//! Handled storage errors for Sentry — issue #76: a keymanager3 session that seals but never opens
//! again (`plex::session`'s `LOCKED_RECOVERABLE`/refused-marker shape, `keymanager.rs`'s round-trip
//! proof) has never sent a single telemetry event of any kind, on any set of that generation, and
//! nothing this app sends anywhere describes storage at all. This is the SAME shape as
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
//! twin — [`report_error`] is the only door, gated on `consent::allows_errors()` exactly like every
//! other standing handled-error report.

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
}

impl SessionStorageClass {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::None => "none",
            Self::Plaintext => "plaintext",
            Self::Secure => "secure",
            Self::SecureLocked => "secure_locked",
            Self::SecureRefused => "secure_refused",
        }
    }
}

/// Which step of a seal/open attempt a storage failure was reported from. Every variant is wired
/// live: the first five come from `keymanager::stage_for_method`'s `log_refusal`/`log_missing_field`
/// call sites, `RoundtripMismatch` from `keymanager::round_trips`'s own proof, `EnvelopeUnparseable`/
/// `EnvelopeLocked` from `plex::session::locked` (a read landing `LOCKED_RECOVERABLE`/
/// `LOCKED_UNRECOVERABLE`), and `NoReply`/`Unreachable` from `keymanager::note_unanswered` (a call
/// that timed out, or a registration that never succeeded).
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
}

impl StorageStage {
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
        }
    }
}

/// Everything one handled storage-error report carries. Every field is a closed enum, a bare
/// service error number with no identity of its own, or a bool.
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
}

/// Pure body builder — same shape as `signin::event_body`, minus the consent-kind tag this module
/// has no need of (there is only the standing form; see the module doc).
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    ctx: StorageErrorContext,
) -> Vec<u8> {
    let stage_code = ctx.stage.code();
    let class_code = ctx.class.code();
    let mut storage_ctx = serde_json::json!({
        "type": "storage",
        "stage": stage_code,
        "class": class_code,
        "refused_marker": ctx.refused_marker,
    });
    if let Some(code) = ctx.service_error_code {
        storage_ctx["service_error_code"] = Value::from(code);
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
    super::sentry::attach_user(&mut body, errors_id);
    super::sentry::attach_hardware_context(&mut body);
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Queue one handled event and ask the existing background sender to flush it. Gated exactly like
/// `signin::report_error` and `playback::report_error` — the same standing consent question and the
/// same Errors/Sentry lane, since this is a new report category riding the existing mechanism
/// rather than a new one.
///
/// Returns whether the report was actually queued.
///
/// Called from `plex::session::report_storage_error` (`#[cfg(not(test))]` — a real seal/open
/// failure, wired since T2 of issue #76). That makes this function itself unreachable in a TEST
/// build specifically: `session.rs`'s `#[cfg(test)]` twin routes to `tests::capture_report` instead
/// so a test can assert on what was reported with no Sentry endpoint compiled in, and dead-code
/// analysis runs per test build.
#[allow(dead_code)]
pub(crate) fn report_error(ctx: StorageErrorContext) -> bool {
    if !super::consent::allows_errors() || !super::sender::has_sentry() {
        return false;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — handled storage error was not queued");
        return false;
    };
    let body = event_body(
        &event_id,
        super::sentry::build_id(),
        super::consent::errors_id().as_deref(),
        ctx,
    );
    let record = super::queue::Record {
        category: super::queue::Category::Errors,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    match super::spool::append_if(&record, super::consent::allows_errors) {
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
    event_body(
        "<random per-error event id>",
        "<running ELF build id>",
        Some(super::native::PREVIEW_USER_ID),
        StorageErrorContext {
            stage: StorageStage::EnvelopeLocked,
            service_error_code: Some(-10001),
            class: SessionStorageClass::SecureRefused,
            refused_marker: true,
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
        }
    }

    #[test]
    fn every_stage_and_class_code_is_distinct_and_snake_case() {
        let stages = [
            StorageStage::GenerateKey,
            StorageStage::BeginEncrypt,
            StorageStage::FinishEncrypt,
            StorageStage::BeginDecrypt,
            StorageStage::FinishDecrypt,
            StorageStage::RoundtripMismatch,
            StorageStage::EnvelopeUnparseable,
            StorageStage::EnvelopeLocked,
            StorageStage::NoReply,
            StorageStage::Unreachable,
        ];
        let mut codes: Vec<&str> = stages.iter().map(|s| s.code()).collect();
        for c in &codes {
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        }
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "duplicate stage code");

        let classes = [
            SessionStorageClass::None,
            SessionStorageClass::Plaintext,
            SessionStorageClass::Secure,
            SessionStorageClass::SecureLocked,
            SessionStorageClass::SecureRefused,
        ];
        let mut ccodes: Vec<&str> = classes.iter().map(|c| c.code()).collect();
        for c in &ccodes {
            assert!(c.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'));
        }
        ccodes.sort_unstable();
        let n = ccodes.len();
        ccodes.dedup();
        assert_eq!(ccodes.len(), n, "duplicate class code");
    }

    /// Same PLX-NATIVE-F gap as playback/signin: a handled storage error must carry the crash
    /// path's `hardware`/`webos` contexts, not just its own `storage` context.
    #[test]
    fn handled_storage_error_carries_the_same_hardware_context_as_a_crash() {
        let v: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
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
                "transaction",
                "user",
            ]
        );
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
            ["class", "refused_marker", "stage", "type"]
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

    #[test]
    fn service_error_code_is_present_only_when_supplied() {
        let without: Value = serde_json::from_slice(&event_body(
            &"a".repeat(32),
            "0123456789abcdef",
            Some(&"e".repeat(32)),
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
    }

    /// No key material, ciphertext or plaintext is a value this module can even represent — the
    /// declaration region carries no owned string field, same shape as
    /// `diag::schema::tests::no_variant_can_carry_a_runtime_string`, so this is a source grep rather
    /// than a runtime assertion.
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
            assert!(
                !code.contains("String") && !code.contains("Cow<"),
                "line {} of storage.rs's declaration region introduces an owned string: {line}",
                i + 1
            );
        }
    }

    /// `report_error` needs standing error-report consent AND a build that carries a Sentry
    /// endpoint — same gate as `signin::report_error`/`playback::report_error`. In a dev checkout
    /// with no `pkg/telemetry.local.json` there is no endpoint, so this must refuse and touch
    /// nothing.
    #[test]
    fn report_error_needs_consent_and_an_endpoint() {
        if super::super::sender::has_sentry() {
            return;
        }
        assert!(!report_error(context()));
    }

    /// PRIVACY.md names the same closed vocabulary this module actually emits.
    #[test]
    fn privacy_names_the_closed_storage_vocabulary() {
        let privacy = include_str!("../../../PRIVACY.md");
        for value in [
            "generate_key",
            "begin_encrypt",
            "finish_encrypt",
            "begin_decrypt",
            "finish_decrypt",
            "roundtrip_mismatch",
            "envelope_unparseable",
            "envelope_locked",
            "no_reply",
            "unreachable",
            "none",
            "plaintext",
            "secure",
            "secure_locked",
            "secure_refused",
        ] {
            assert!(privacy.contains(value), "PRIVACY.md omitted {value}");
        }
    }
}
