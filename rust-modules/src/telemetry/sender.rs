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
//! [`enabled`] is `false` at COMPILE time — there is no endpoint in the binary to reach, so a fork,
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

/// The Sentry DSN, if this build was configured with one. See the module doc.
const SENTRY_DSN: Option<&str> = non_empty(option_env!("PLX_SENTRY_DSN"));
/// The PostHog project key, likewise.
const POSTHOG_KEY: Option<&str> = non_empty(option_env!("PLX_POSTHOG_KEY"));
/// PostHog's EU ingest host. A constant rather than configuration: the region is a claim
/// `PRIVACY.md` and the LG Data Safety declaration both make, so it should not be movable by an
/// environment variable nobody reads.
const POSTHOG_HOST: &str = "https://eu.i.posthog.com";

/// Deadlines for a background flush. Deliberately shorter than [`net::API`](crate::net::API), which
/// is tuned for a call somebody is waiting on: a worker holding a thread for 25 s to report a crash
/// that already happened has the priority backwards.
const TIMEOUTS: crate::net::Timeouts =
    crate::net::Timeouts { connect_s: 6, total_s: 12, low_speed_bps: 0, low_speed_s: 0 };

/// Could this build send anything at all? False at compile time in any checkout without the
/// configuration file — see the module doc.
pub(crate) fn configured() -> bool {
    SENTRY_DSN.is_some() || POSTHOG_KEY.is_some()
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
    match r.category {
        Category::Errors => c.errors,
        Category::Usage => c.usage,
    }
}

/// The URL and headers a record is sent to, or `None` if this build has no configuration for its
/// destination.
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
        Dest::PostHog => {
            POSTHOG_KEY?;
            Some((
                format!("{POSTHOG_HOST}/i/v0/e/"),
                vec!["Content-Type: application/json".into()],
            ))
        }
    }
}

/// Build a PostHog body for one event, if this build is configured for it.
///
/// Lives here rather than in [`posthog`] because the KEY does — that module is a pure serialiser
/// and should keep having no idea what this installation's credentials are.
pub(crate) fn posthog_body(
    e: crate::diag::schema::DiagEvent,
    distinct_id: &str,
) -> Option<Vec<u8>> {
    // `None` for the timestamp: PostHog stamps on arrival, which on a television whose clock runs
    // ~3h off is strictly better than what we could tell it. See `posthog::single`.
    Some(posthog::single(POSTHOG_KEY?, distinct_id, e, None))
}

/// Attempt one record. Returns the verdict and, when a server asked for one, the hold in seconds.
///
/// Never called from the main loop or from signal context — see [`super::flush_soon`].
pub(crate) fn send_one(r: &Record) -> (Verdict, Option<u64>) {
    let Some((url, headers)) = route(r) else {
        // No configuration for this destination in this build. Hopeless rather than Keep: nothing
        // about a later attempt will differ, and keeping would spool forever.
        return (Verdict::Hopeless, None);
    };
    match crate::net::post_ca(&url, &headers, &r.body, TIMEOUTS) {
        Some(resp) => {
            let v = classify(resp.status);
            // The response body is bounded and kept precisely so a rejection can be logged with the
            // server's own explanation — the difference between debugging a 400 and guessing at one.
            if v != Verdict::Done {
                crate::log(&format!("telemetry: {:?} -> {} ({v:?})", r.dest, resp.status));
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

    fn rec(cat: Category, dest: Dest) -> Record {
        Record { category: cat, dest, event_id: "e".into(), body: b"{}".to_vec() }
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
            assert_eq!(classify(s), Verdict::Keep, "{s} is the server's problem, not the payload's");
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
        assert_eq!(retry_after_secs("Content-Type: x\r\nRETRY-AFTER: 30\r\nX: y"), Some(30));
    }

    /// Sentry's own header carries the wait as a leading integer before its category list.
    #[test]
    fn sentrys_rate_limit_header_is_read_the_same_way() {
        assert_eq!(retry_after_secs("X-Sentry-Rate-Limits: 60:transaction:key"), Some(60));
        assert_eq!(retry_after_secs("x-sentry-rate-limits: 2700"), Some(2700));
    }

    /// **An HTTP-date `Retry-After` is DECLINED rather than guessed at.** It is the other legal
    /// form, and computing a delay from it needs a trustworthy wall clock — which this television
    /// does not have: its clock runs about three hours off, so the answer would be wrong by hours
    /// in whichever direction the skew happens to run. Falling back to the default hold is strictly
    /// better than a confidently wrong number.
    #[test]
    fn an_http_date_retry_after_falls_back_rather_than_guessing() {
        assert_eq!(retry_after_secs("Retry-After: Wed, 21 Oct 2026 07:28:00 GMT"), None);
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
        let errors_only =
            consent::Consent { asked_version: 1, errors: true, usage: false, install_id: None };
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
            assert!(posthog_body(crate::diag::schema::DiagEvent::AppLaunch, "id").is_none());
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
        assert_eq!(non_empty(Some("https://k@o1.ingest.de.sentry.io/2")),
                   Some("https://k@o1.ingest.de.sentry.io/2"));
    }

    /// A configured DSN produces the envelope endpoint and the auth header the protocol wants —
    /// skipped where the build has none, like the DSN test in `sentry`.
    #[test]
    fn a_configured_sentry_build_routes_to_the_envelope_endpoint() {
        let Some(_) = SENTRY_DSN else { return };
        let (url, headers) = route(&rec(Category::Errors, Dest::Sentry)).expect("routes");
        assert!(url.ends_with("/envelope/"), "{url}");
        assert!(headers.iter().any(|h| h.starts_with("Content-Type: application/x-sentry-envelope")));
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
