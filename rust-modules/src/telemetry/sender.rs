//! **The sender** — the one place in this app that talks to somebody who is not Plex.
//!
//! Ties the three pure halves together: [`queue`](super::queue) holds records, [`sentry`] and
//! [`posthog`] frame them, `net::post_ca` puts them on the wire. Everything interesting about it is
//! about *not* sending — when there is no consent, no configuration, no network, or a server that
//! has asked us to stop.
//!
//! # Configuration is compile-time and OPTIONAL, which is what makes "cannot send" checkable
//!
//! The DSN and the PostHog project key arrive through `option_env!`, set by the Makefile out of the
//! gitignored `pkg/telemetry.local.json`. A checkout without that file compiles to `None` and
//! [`configured`] is `false` at COMPILE time — there is no endpoint in the binary to reach, so a fork,
//! a CI runner and anyone building from source get an app that provably cannot report, without
//! having to trust a runtime flag.
//!
//! Both values are **write-only ingest credentials and publishable by design** — every client that
//! sends anything must carry one. The Sentry *auth token*, which can read and delete the project's
//! data, is a different thing entirely and is never compiled in: it lives on the dev Mac and is used
//! by `sentry-cli` to upload debug files.
//!
//! # The three ways this refuses, in order
//!
//! 1. **No consent.** Checked per record against its own category, not once for the batch — the two
//!    switches are independent, and a spool written before a withdrawal can still contain records
//!    of a category that is now off.
//! 2. **No configuration.** See above.
//! 3. **A server that said stop.** A `429` holds the whole spool rather than dropping records —
//!    being rate limited is not being rejected, and treating it as rejection loses exactly the
//!    reports that arrive in a burst, which is what a bad release looks like.
//!    **The INTERVAL a server asks for is not yet honoured**: `net::Resp` carries no headers, so
//!    [`retry_after_secs`] is written and tested but cannot be fed, and a hold is a flat
//!    [`DEFAULT_HOLD_S`]. Said here rather than left to be discovered, because "honours
//!    Retry-After" is the kind of claim that reads as true from the presence of a parser.
//!
//! # What is pure here, and why that is most of it
//!
//! [`classify`] and [`retry_after_secs`] decide everything: whether a record is done, should be
//! kept for later, or is hopeless. They take a status and headers and return a verdict, so every
//! rule below is a host test rather than a claim — which matters because the alternative is
//! discovering the rule from a dashboard that stayed empty.

use super::queue::{Category, Dest, Record};
use super::{consent, posthog, sentry};

/// **An EMPTY environment variable is not a configuration.**
///
/// `option_env!` answers `Some("")` for a variable that is set but blank, and the Makefile exports
/// both of these unconditionally — reading them out of a JSON file that a checkout may not have, in
/// which case the value is the empty string. Without this the unconfigured case would report itself
/// as configured, `route` would build a URL from nothing, and every record would spool and fail
/// forever against an endpoint that does not exist. `const fn` so the whole thing still collapses
/// at compile time, which is what makes "this build cannot send" a property of the artifact.
const fn non_empty(v: Option<&'static str>) -> Option<&'static str> {
    match v {
        Some(s) if !s.is_empty() => Some(s),
        _ => None,
    }
}

// ---- the credential PAIRS, and the environment they decide ------------------------------------

/// The production pair. Supplied only by the release workflow, out of GitHub repository variables —
/// deliberately never read from the working copy, so a developer's machine cannot hold them.
const SENTRY_DSN_PROD: Option<&str> = non_empty(option_env!("PLX_SENTRY_DSN"));
const POSTHOG_KEY_PROD: Option<&str> = non_empty(option_env!("PLX_POSTHOG_KEY"));

/// The development pair, from `pkg/telemetry.local.json` via `make telemetry-local`.
const SENTRY_DSN_DEV: Option<&str> = non_empty(option_env!("PLX_SENTRY_DSN_DEV"));
const POSTHOG_KEY_DEV: Option<&str> = non_empty(option_env!("PLX_POSTHOG_KEY_DEV"));

const HAS_PROD: bool = SENTRY_DSN_PROD.is_some() || POSTHOG_KEY_PROD.is_some();
const HAS_DEV: bool = SENTRY_DSN_DEV.is_some() || POSTHOG_KEY_DEV.is_some();

/// **The two configurations that are refused at COMPILE time.**
///
/// A `const` block, so these are build errors rather than something to discover from a dashboard.
/// Both describe a binary whose label would not match its behaviour, which is the one failure this
/// whole arrangement exists to prevent.
const _: () = {
    if HAS_PROD && HAS_DEV {
        panic!(
            "telemetry: both the production and the development credentials were supplied. \
             The environment is derived from WHICH pair a build was given, so a binary holding \
             both has no answer. The production pair belongs only to the release workflow."
        );
    }
    if HAS_PROD && cfg!(feature = "devtriggers") {
        panic!(
            "telemetry: production credentials in a build that still has the dev-trigger surface. \
             A binary that reports as production must be the one users get — pass RELEASE=1, or \
             use the development credentials (`make telemetry-local`)."
        );
    }
};

/// **Which side of the world this build is on, derived from its credentials rather than its
/// features.**
///
/// The first design read this off `cfg!(feature = "devtriggers")`, and an ordinary command breaks
/// that: `make RELEASE=1 FLAVOR=debug deploy` drops the feature, so the build would call itself
/// `production` while its key — still from the local file — sent everything to the dev project.
/// Label and destination diverging silently is the worst outcome for a field whose only job is to
/// say which side data is on. Deriving both from the same value makes disagreement impossible.
///
/// An unconfigured build reads `development`. Nothing can be sent from one, so the value is
/// unobservable; `development` is simply the honest reading of "not the shipped configuration".
pub(crate) const ENVIRONMENT: &str = if HAS_PROD {
    "production"
} else {
    "development"
};

/// The DSN this build actually uses — the production one, or the development one, never both.
const SENTRY_DSN: Option<&str> = if HAS_PROD {
    SENTRY_DSN_PROD
} else {
    SENTRY_DSN_DEV
};
/// The PostHog key this build actually uses.
const POSTHOG_KEY: Option<&str> = if HAS_PROD {
    POSTHOG_KEY_PROD
} else {
    POSTHOG_KEY_DEV
};
/// PostHog's EU ingest host. A constant rather than configuration: the region is a claim
/// `PRIVACY.md` and the LG Data Safety declaration both make, so it should not be movable by an
/// environment variable nobody reads.
const POSTHOG_HOST: &str = "https://eu.i.posthog.com";

/// Deadlines for a background flush. Deliberately shorter than [`net::API`](crate::net::API), which
/// is tuned for a call somebody is waiting on: a worker holding a thread for 25 s to report a crash
/// that already happened has the priority backwards.
const TIMEOUTS: crate::net::Timeouts = crate::net::Timeouts {
    connect_s: 6,
    total_s: 12,
    total_ms: 0,
    low_speed_bps: 0,
    low_speed_s: 0,
};

/// Could this build send anything at all? False at compile time in any checkout without the
/// configuration file — see the module doc.
pub(crate) fn configured() -> bool {
    has_sentry() || has_posthog()
}

/// Per DESTINATION, because one configured and the other not is the ordinary case here — the two
/// are separate services with separate credentials, and this project has already spent a stretch
/// with a Sentry key and no PostHog one. [`configured`] answers "can this build send at all", which
/// is the wrong question for a log line: it is `true` while half the app is silently discarding
/// every record it produces.
pub(crate) fn has_sentry() -> bool {
    SENTRY_DSN.is_some()
}

/// The ingest-only DSN for the native capture backend's event header.
///
/// Sentry Native has no transport in this build; the value is still required in the envelope it
/// writes for an external reporter. The ordinary sender discards that header and frames the
/// sanitised body with this same compile-time destination later.
pub(crate) fn sentry_dsn() -> Option<&'static str> {
    SENTRY_DSN
}

pub(crate) fn has_posthog() -> bool {
    POSTHOG_KEY.is_some()
}

/// What to do with a record after an attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Accepted. Drop it.
    Done,
    /// Not accepted, but might be next time — keep it and try later.
    Keep,
    /// The server will never accept this. Drop it, because retrying forever fills a spool with one
    /// poisoned record and starves everything behind it.
    Hopeless,
}

/// Decide a record's fate from an HTTP status.
///
/// The three-way split is the point. A two-way "did it work" collapses two opposite mistakes into
/// one: dropping a record the server was merely too busy for, and retrying a malformed one until it
/// crowds out every other. Both are silent.
pub(crate) fn classify(status: u16) -> Verdict {
    match status {
        200..=299 => Verdict::Done,
        // Rate limited or asked to back off. The record is fine; the moment is not.
        429 => Verdict::Keep,
        // 408 request timeout and 5xx are the server's problem, not the payload's.
        408 => Verdict::Keep,
        500..=599 => Verdict::Keep,
        // 4xx otherwise means this body will never be accepted: a malformed envelope, a revoked
        // key, a project that no longer exists. Keeping it would block the spool forever.
        400..=499 => Verdict::Hopeless,
        // Anything else is a transport oddity rather than an answer — keep and see.
        _ => Verdict::Keep,
    }
}

/// How long a server asked us to wait, in seconds, from `Retry-After` or `X-Sentry-Rate-Limits`.
///
/// **`Retry-After` has two legal forms** and only the delta-seconds one is handled: an HTTP-date
/// needs a parsed clock, and this television's wall clock runs about three hours off, so computing
/// a delay from it would produce a wait that is wrong by hours in whichever direction the skew
/// happens to run. Falling back to the default hold is strictly better than a confidently wrong
/// number. Sentry's own header is `<seconds>:<categories>:...`, so the leading integer is the wait
/// in both cases.
/// **NOT YET REACHABLE, and that is a gap rather than a style choice.** `net::Resp` carries a
/// status and a body and no headers at all — `request_tls` never installs a
/// `CURLOPT_HEADERFUNCTION` — so nothing can hand this a header block today, and a hold falls back
/// to [`DEFAULT_HOLD_S`]. The parser is written and tested because the rules in it (the two legal
/// `Retry-After` forms, Sentry's own header, and the clock-skew reason one of them is declined) are
/// worth settling once; wiring it needs response-header capture on the shared request path, which
/// every plex.tv call also uses and which is therefore its own change.
///
/// The practical consequence is stated plainly: this build honours **429 as a status** and holds
/// the spool for a fixed minute; it does not yet honour an interval a server asked for.
#[allow(dead_code)]
pub(crate) fn retry_after_secs(headers: &str) -> Option<u64> {
    for line in headers.lines() {
        let lower = line.to_ascii_lowercase();
        let key = if lower.starts_with("retry-after:") {
            "retry-after:"
        } else if lower.starts_with("x-sentry-rate-limits:") {
            "x-sentry-rate-limits:"
        } else {
            continue;
        };
        let v = line[key.len()..].trim();
        // The leading run of digits. Handles both `120` and Sentry's `60:transaction:key`, and
        // declines an HTTP-date, whose first token is a weekday.
        let digits: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
    }
    None
}

/// The hold applied when a server said back off but named no interval. A minute: long enough that a
/// burst does not hammer a rate-limited endpoint, short enough that an ordinary hiccup does not
/// strand a crash report until the next launch.
pub(crate) const DEFAULT_HOLD_S: u64 = 60;

/// May this record be sent right now, given the consent in force?
///
/// **Per record, against its own category.** A spool written before a withdrawal still holds
/// records of a category that is now off, and the whole point of storing the category with the
/// record is that this question has an answer.
pub(crate) fn allowed(r: &Record, c: &consent::Consent) -> bool {
    if !c.answered() {
        return false;
    }
    match r.category {
        Category::Errors => c.errors,
        Category::Usage => c.usage,
    }
}

/// The URL and headers a record is sent to, or `None` if this build has no configuration for its
/// destination.
/// **What actually goes on the wire, which is not always what is in the spool.**
///
/// The spool holds the EVENT, because that is the thing consent was given for and the thing
/// `queue::ack` identifies; the envelope is transport framing and belongs here, where the URL and
/// the headers are chosen.
///
/// **Sentry's endpoint accepted the unframed event with a 200 and an echoed id, and stored
/// nothing.** `route` has always addressed `/envelope/` with `Content-Type:
/// application/x-sentry-envelope`, and `sentry::envelope` was written, tested — and never called;
/// the raw event JSON went out instead. The receiver parses the first line as the ENVELOPE HEADER,
/// which a single-line event object happens to satisfy, finds no item lines after it, and returns
/// `200 {"id": <the header's event_id>}` for an envelope carrying nothing.
///
/// So the response is not evidence, and that is the part worth carrying away rather than the bug:
/// posting a body of nothing but `{"event_id":"…"}` — not an event by any reading — gets the same
/// 200 and the same echoed id. Measured against the live endpoint, which is how this was settled;
/// no amount of reading the status code could have.
fn wire_body(r: &Record) -> Vec<u8> {
    match r.dest {
        Dest::Sentry => sentry::envelope(&r.event_id, "event", &r.body),
        Dest::PostHog => {
            let Some(event) = crate::diag::schema::UsageEnvelope::decode(&r.body) else {
                // Retire both legacy vendor JSON and unknown future neutral records. Passing old
                // capture bodies through would make the exact-schema consent preview false after
                // an upgrade; guessing how to enrich them would invent occurrence/context facts.
                return Vec::new();
            };
            let Some(id) = consent::current().and_then(|c| c.install_id) else {
                return Vec::new();
            };
            let Some(key) = POSTHOG_KEY else {
                return Vec::new();
            };
            posthog::captured(key, &id, &event, ENVIRONMENT)
        }
    }
}

fn route(r: &Record) -> Option<(String, Vec<String>)> {
    match r.dest {
        Dest::Sentry => {
            let dsn = sentry::parse_dsn(SENTRY_DSN?)?;
            Some((
                dsn.envelope_url(),
                vec![
                    "Content-Type: application/x-sentry-envelope".into(),
                    dsn.auth_header(),
                ],
            ))
        }
        // **PostHog's 200 proves the SHAPE, not the destination.** Measured against the live
        // endpoint, 2026-08-30, three bodies:
        //
        // | body | answer |
        // |---|---|
        // | a well-formed event | `200 {"status":"Ok"}` |
        // | an event with no `event` name | `400 …missing event name attribute` |
        // | a well-formed event with a **bogus api_key** | `200 {"status":"Ok"}` |
        //
        // So this endpoint does validate — unlike Sentry's envelope endpoint, which accepted a
        // body of nothing but `{"event_id":"…"}` — and a 400 here is worth reading, because it
        // names the field. But a typo'd, rotated or revoked key reports perfect success forever
        // and stores nothing, and no log line on this side can ever say so. The only checks that
        // reach it are `release.yml` asserting the key is in the packaged bytes, and a human
        // looking at the project.
        Dest::PostHog => {
            POSTHOG_KEY?;
            Some((
                format!("{POSTHOG_HOST}/i/v0/e/"),
                vec!["Content-Type: application/json".into()],
            ))
        }
    }
}

/// Attempt one record. Returns the verdict and, when a server asked for one, the hold in seconds.
///
/// Never called from the main loop or from signal context — see [`super::flush_soon`].
pub(crate) fn send_one(r: &Record) -> (Verdict, Option<u64>) {
    let Some((url, headers)) = route(r) else {
        // No configuration for this destination in this build. Hopeless rather than Keep: nothing
        // about a later attempt will differ, and keeping would spool forever.
        //
        // **Logged, and it was not.** This is the one exit that discards a record without ever
        // opening a socket, and it looked from the outside exactly like a successful send: the
        // spool drained to zero, no line was written, and the event never arrived. Verifying the
        // crash channel end to end is what surfaced it — there was no way to tell "delivered" from
        // "silently dropped for want of a key", which is precisely the question a verification is
        // asking. One destination missing while the other is configured is the ordinary case here,
        // not an exotic one.
        crate::log(&format!(
            "telemetry: no endpoint for {:?} in this build — record discarded",
            r.dest
        ));
        return (Verdict::Hopeless, None);
    };
    let body = wire_body(r);
    if body.is_empty() {
        crate::log(&format!(
            "telemetry: obsolete or unsupported {:?} record discarded before send",
            r.dest
        ));
        return (Verdict::Hopeless, None);
    }
    match crate::net::post_ca(&url, &headers, &body, TIMEOUTS) {
        Some(resp) => {
            let v = classify(resp.status);
            // The response body is bounded and kept precisely so a rejection can be logged with the
            // server's own explanation — the difference between debugging a 400 and guessing at one.
            if v != Verdict::Done {
                // **With the server's own explanation.** The comment above promised this and the
                // line did not carry it, which is the difference between debugging a 400 and
                // guessing at one — Sentry and PostHog both answer a malformed envelope with a
                // sentence naming the field. Bounded hard and scrubbed like every other line
                // (`crate::log` runs `scrub_local` before the write), because it is third-party
                // text landing in the primary debugging surface.
                let why = String::from_utf8_lossy(&resp.body);
                let why: String = why.chars().filter(|c| !c.is_control()).take(160).collect();
                crate::log(&format!(
                    "telemetry: {:?} -> {} ({v:?}) {why}",
                    r.dest, resp.status
                ));
            }
            (v, None)
        }
        // No response at all: DNS, TLS, timeout, a television on a hotel network. Keep — this is
        // the case the spool exists for.
        None => (Verdict::Keep, Some(DEFAULT_HOLD_S)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **What is posted to an envelope endpoint must be an ENVELOPE.**
    ///
    /// This shipped wrong and the transport could not say so: `route` addressed `/envelope/` with
    /// the envelope content type while `send_one` posted the bare event, and Sentry answered
    /// `200 {"id": …}` because a single-line event object parses as an envelope HEADER — an
    /// envelope carrying zero items, accepted, stored nowhere. Measured against the live endpoint:
    /// a body of nothing but `{"event_id":"…"}` gets the same 200 and the same echoed id.
    ///
    /// Graded structurally, since no status code can grade it: the Sentry wire body is three parts
    /// — a header line, an item header whose `length` is the payload's byte length, and the payload
    /// itself — and the length is the field a hand-rolled framer gets wrong.
    #[test]
    fn a_sentry_record_goes_on_the_wire_as_a_framed_envelope() {
        let mut r = rec(Category::Errors, Dest::Sentry);
        r.event_id = "a".repeat(32);
        r.body = br#"{"event_id":"aaa","level":"fatal"}"#.to_vec();
        let wire = wire_body(&r);

        let text = String::from_utf8(wire.clone()).expect("utf-8");
        let mut lines = text.split('\n');
        let head = lines.next().expect("an envelope header line");
        assert!(
            head.contains(&r.event_id),
            "the header must carry the event id: {head}"
        );
        let item = lines.next().expect("an item header line");
        assert!(item.contains("\"type\":\"event\""), "item header: {item}");
        assert!(
            item.contains(&format!("\"length\":{}", r.body.len())),
            "the item length must be the payload's byte length, or the receiver parses the next \
             line as payload and rejects the whole envelope complaining about neither: {item}"
        );
        assert!(
            text.contains(std::str::from_utf8(&r.body).unwrap()),
            "the payload survives whole"
        );
        assert_ne!(
            wire, r.body,
            "the bare event was posted to an envelope endpoint"
        );
    }

    /// A legacy PostHog body is retired rather than sent outside the exact current schema shown on
    /// the consent screen. It cannot be enriched honestly: its occurrence/context facts are gone.
    #[test]
    fn a_legacy_posthog_record_is_not_sent_after_the_schema_upgrade() {
        let mut r = rec(Category::Usage, Dest::PostHog);
        r.body = br#"{"api_key":"phc_x","event":"app.launch"}"#.to_vec();
        assert!(wire_body(&r).is_empty());
    }

    #[test]
    fn an_unknown_neutral_usage_version_is_not_posted_as_vendor_json() {
        let mut r = rec(Category::Usage, Dest::PostHog);
        r.body = br#"{"version":99,"occurred_at_ms":1,"session_id":"s","name":"app.launch","fields":[]}"#.to_vec();
        assert!(
            wire_body(&r).is_empty(),
            "future internal storage escaped onto the wire"
        );
    }

    fn rec(cat: Category, dest: Dest) -> Record {
        Record {
            category: cat,
            dest,
            event_id: "e".into(),
            body: b"{}".to_vec(),
        }
    }

    /// The three-way split, at every boundary that matters. A two-way "did it work" collapses two
    /// opposite mistakes — dropping what the server was merely busy for, and retrying a malformed
    /// record until it crowds out the spool — and both are silent.
    #[test]
    fn a_status_maps_to_one_of_three_fates() {
        for s in [200, 201, 202, 204, 299] {
            assert_eq!(classify(s), Verdict::Done, "{s}");
        }
        for s in [408, 429, 500, 502, 503, 599] {
            assert_eq!(
                classify(s),
                Verdict::Keep,
                "{s} is the server's problem, not the payload's"
            );
        }
        for s in [400, 401, 403, 404, 413, 422, 499] {
            assert_eq!(classify(s), Verdict::Hopeless, "{s} will never be accepted");
        }
    }

    /// **429 is KEEP, not Hopeless**, even though it is a 4xx. Being rate limited is not being
    /// rejected, and treating it as rejection loses exactly the reports that arrive in a burst —
    /// which is what a bad release looks like.
    #[test]
    fn rate_limiting_holds_the_record_rather_than_dropping_it() {
        assert_eq!(classify(429), Verdict::Keep);
    }

    /// A `Retry-After` in delta-seconds is honoured, and header matching is case-insensitive
    /// because nothing guarantees a server's casing.
    #[test]
    fn a_delta_seconds_retry_after_is_read() {
        assert_eq!(retry_after_secs("Retry-After: 120"), Some(120));
        assert_eq!(retry_after_secs("retry-after: 5"), Some(5));
        assert_eq!(
            retry_after_secs("Content-Type: x\r\nRETRY-AFTER: 30\r\nX: y"),
            Some(30)
        );
    }

    /// Sentry's own header carries the wait as a leading integer before its category list.
    #[test]
    fn sentrys_rate_limit_header_is_read_the_same_way() {
        assert_eq!(
            retry_after_secs("X-Sentry-Rate-Limits: 60:transaction:key"),
            Some(60)
        );
        assert_eq!(retry_after_secs("x-sentry-rate-limits: 2700"), Some(2700));
    }

    /// **An HTTP-date `Retry-After` is DECLINED rather than guessed at.** It is the other legal
    /// form, and computing a delay from it needs a trustworthy wall clock — which this television
    /// does not have: its clock runs about three hours off, so the answer would be wrong by hours
    /// in whichever direction the skew happens to run. Falling back to the default hold is strictly
    /// better than a confidently wrong number.
    #[test]
    fn an_http_date_retry_after_falls_back_rather_than_guessing() {
        assert_eq!(
            retry_after_secs("Retry-After: Wed, 21 Oct 2026 07:28:00 GMT"),
            None
        );
    }

    /// No such header is `None`, and a header with no digits is too.
    #[test]
    fn absent_or_unparsable_headers_yield_nothing() {
        assert_eq!(retry_after_secs(""), None);
        assert_eq!(retry_after_secs("Content-Type: application/json"), None);
        assert_eq!(retry_after_secs("Retry-After:"), None);
        assert_eq!(retry_after_secs("Retry-After: soon"), None);
    }

    /// **Consent is checked per record against its own category.** A spool written before a
    /// withdrawal still holds records of a category that is now off, which is the entire reason the
    /// category is stored with the record.
    #[test]
    fn a_withdrawn_category_is_refused_even_from_an_old_spool() {
        let errors_only = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            usage: false,
            install_id: None,
            errors_id: Some("e".repeat(32)),
        };
        assert!(allowed(&rec(Category::Errors, Dest::Sentry), &errors_only));
        assert!(!allowed(&rec(Category::Usage, Dest::PostHog), &errors_only));

        let nothing = consent::Consent::default();
        assert!(!allowed(&rec(Category::Errors, Dest::Sentry), &nothing));
        assert!(!allowed(&rec(Category::Usage, Dest::PostHog), &nothing));
    }

    /// **An unconfigured build cannot send, and says so as a compile-time fact.** This assertion
    /// reads whichever way the checkout is configured, which is deliberate: the property is that
    /// `configured()` agrees with the constants, so a build with no `pkg/telemetry.local.json`
    /// carries no endpoint at all.
    #[test]
    fn an_unconfigured_build_has_no_endpoint_to_reach() {
        assert_eq!(configured(), SENTRY_DSN.is_some() || POSTHOG_KEY.is_some());
        if !configured() {
            assert!(route(&rec(Category::Errors, Dest::Sentry)).is_none());
            assert!(route(&rec(Category::Usage, Dest::PostHog)).is_none());
        }
    }

    /// **A blank variable is not a configuration.** The Makefile exports both unconditionally,
    /// reading them from a JSON file a checkout may not have — so the unconfigured case arrives
    /// here as `Some("")`, and without this it would report as configured and spool every record
    /// forever against a URL built from nothing.
    #[test]
    fn an_empty_environment_variable_is_not_a_configuration() {
        assert_eq!(non_empty(Some("")), None);
        assert_eq!(non_empty(None), None);
        assert_eq!(
            non_empty(Some("https://k@o1.ingest.de.sentry.io/2")),
            Some("https://k@o1.ingest.de.sentry.io/2")
        );
    }

    /// **The environment agrees with the credentials, whatever this checkout happens to have.**
    ///
    /// Written as a relation rather than an expected value, because the answer depends on how the
    /// build was configured — which is the whole design. What must never happen is the two
    /// disagreeing, and that is checkable in every configuration.
    #[test]
    fn the_environment_matches_the_credential_pair_that_was_supplied() {
        assert_eq!(
            ENVIRONMENT,
            if HAS_PROD {
                "production"
            } else {
                "development"
            }
        );
        // The active credentials come from the same side as the label.
        if HAS_PROD {
            assert_eq!(SENTRY_DSN, SENTRY_DSN_PROD);
            assert_eq!(POSTHOG_KEY, POSTHOG_KEY_PROD);
        } else {
            assert_eq!(SENTRY_DSN, SENTRY_DSN_DEV);
            assert_eq!(POSTHOG_KEY, POSTHOG_KEY_DEV);
        }
        // And the two sides are never both live — the `const` block above refuses to compile such
        // a build, so this asserts the invariant holds rather than re-deriving it.
        assert!(!(HAS_PROD && HAS_DEV));
    }

    /// A dev-featured build is never production. The counter-example that killed the first design
    /// was the other direction (`RELEASE=1 FLAVOR=debug` labelling itself production while sending
    /// to dev); this is the invariant that replaced it, and the `const` block makes the violating
    /// build fail to compile at all.
    #[test]
    fn a_build_with_dev_triggers_is_never_production() {
        if cfg!(feature = "devtriggers") {
            assert_eq!(ENVIRONMENT, "development");
        }
    }

    /// The environment reaches the wire, on the channel that carries product data.
    #[test]
    fn the_environment_is_a_property_on_every_event() {
        let body = posthog::single(
            "k",
            "id",
            crate::diag::schema::DiagEvent::AppLaunch,
            "development",
            None,
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["properties"]["environment"], "development");
    }

    /// A configured DSN produces the envelope endpoint and the auth header the protocol wants —
    /// skipped where the build has none, like the DSN test in `sentry`.
    #[test]
    fn a_configured_sentry_build_routes_to_the_envelope_endpoint() {
        let Some(_) = SENTRY_DSN else { return };
        let (url, headers) = route(&rec(Category::Errors, Dest::Sentry)).expect("routes");
        assert!(url.ends_with("/envelope/"), "{url}");
        assert!(headers
            .iter()
            .any(|h| h.starts_with("Content-Type: application/x-sentry-envelope")));
        assert!(headers.iter().any(|h| h.starts_with("X-Sentry-Auth:")));
    }

    /// PostHog goes to the single-event endpoint, **with its trailing slash** — the form the vendor
    /// documents, and the one whose `distinct_id` placement `posthog` is written against.
    #[test]
    fn a_configured_posthog_build_routes_to_the_single_event_endpoint() {
        let Some(_) = POSTHOG_KEY else { return };
        let (url, _) = route(&rec(Category::Usage, Dest::PostHog)).expect("routes");
        assert_eq!(url, "https://eu.i.posthog.com/i/v0/e/");
    }

    /// The region is a constant, not configuration. `PRIVACY.md` and the LG Data Safety declaration
    /// both claim EU storage, and a claim two published documents make should not be movable by an
    /// environment variable nobody reads.
    #[test]
    fn the_posthog_region_is_not_configurable() {
        assert!(POSTHOG_HOST.starts_with("https://eu."));
    }

    /// A background flush must not hold a worker as long as a call somebody is waiting on.
    #[test]
    fn a_background_flush_gives_up_sooner_than_an_interactive_call() {
        assert!(TIMEOUTS.total_s < crate::net::API.total_s);
        assert!(TIMEOUTS.connect_s <= crate::net::API.connect_s);
    }
}
