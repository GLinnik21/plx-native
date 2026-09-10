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
pub(crate) fn event_body(
    event_id: &str,
    dist: &str,
    errors_id: Option<&str>,
    consent: ConsentKind,
    ctx: SignInErrorContext,
) -> Vec<u8> {
    let kind_code = ctx.kind.code();
    let link_code = ctx.link.code();
    let consent_code = consent.code();
    let storage_code = ctx.storage.code();
    let mut signin_ctx = serde_json::json!({
        "type": "signin",
        "kind": kind_code,
        "link": link_code,
        "unanswered": ctx.unanswered.code(),
        "failing_for": ctx.failing_for.code(),
        "code_generation": ctx.code_generation,
        "consent": consent_code,
        "storage": storage_code,
    });
    if let Some(status) = ctx.http_status {
        signin_ctx["http_status"] = Value::from(status);
    }
    if let Some(rc) = ctx.curl_rc {
        signin_ctx["curl_rc"] = Value::from(rc);
    }
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-handled", "version": env!("PLX_VERSION")},
        "logger": "signin",
        "transaction": "signin",
        "culprit": format!("signin::{kind_code}"),
        "fingerprint": ["signin-error", kind_code, link_code],
        "exception": {"values": [{
            "type": "SignInError",
            "value": kind_code,
            "mechanism": {"type": "signin", "handled": true},
        }]},
        "tags": {
            "signin.kind": kind_code,
            "signin.link": link_code,
            "signin.consent": consent_code,
            "signin.storage": storage_code,
        },
        "contexts": {"signin": signin_ctx},
    });
    if !dist.is_empty() {
        body["dist"] = Value::String(dist.to_string());
    }
    super::sentry::attach_user(&mut body, errors_id);
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
    if !super::consent::allows_errors() || !super::sender::has_sentry() {
        return false;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — handled sign-in error was not queued");
        return false;
    };
    let body = event_body(
        &event_id,
        super::sentry::build_id(),
        super::consent::errors_id().as_deref(),
        ConsentKind::Standing,
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
/// is not a consent decision either way: nothing is recorded, no identifier is minted, and no
/// later consent change ever withdraws it — see `queue::Category::OneOff`'s doc. It requires only
/// that this build carries a Sentry endpoint at all; the standing "have error reports been turned
/// on" question does not apply to a report the person is looking at and pressing Send for.
///
/// Returns whether the report was actually queued.
pub(crate) fn send_once(ctx: SignInErrorContext) -> bool {
    if !super::sender::has_sentry() {
        return false;
    }
    let Some(event_id) = crate::diag::random_hex_id() else {
        crate::log("telemetry: no /dev/urandom — one-off sign-in report was not queued");
        return false;
    };
    // No errors_id: a one-off report carries no identifier of any kind, standing or otherwise.
    let body = event_body(
        &event_id,
        super::sentry::build_id(),
        None,
        ConsentKind::OneOff,
        ctx,
    );
    let record = super::queue::Record {
        category: super::queue::Category::OneOff,
        dest: super::queue::Dest::Sentry,
        event_id,
        body,
    };
    match super::spool::append_if(&record, || true) {
        Some(true) => {
            super::flush_soon();
            true
        }
        Some(false) => {
            crate::log("telemetry: one-off sign-in report did not fit the durable spool");
            false
        }
        None => false,
    }
}

/// Representative handled-error payload built through the real serialiser, for the consent
/// screen's preview. No consent-time identifier is minted.
///
/// Shows the STANDING form — the automatic report a person who already turned error reports on
/// gets at a failed sign-in, since that is the one the crash/errors consent question is actually
/// asking about. The one-off form ([`send_once`]) is not gated on this question at all — it is
/// its own press, described in prose rather than by a second sample.
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
        },
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
        ))
        .expect("event JSON");
        let ctx = &transport["contexts"]["signin"];
        assert_eq!(ctx["curl_rc"], 6);
        assert!(ctx.get("http_status").is_none());
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
        ))
        .expect("event JSON");
        assert_eq!(one_off["contexts"]["signin"]["consent"], "one_off");
        assert_eq!(one_off["tags"]["signin.consent"], "one_off");
        // No `user` object at all for a one-off report — no identifier of any kind.
        assert!(one_off.get("user").is_none());
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
