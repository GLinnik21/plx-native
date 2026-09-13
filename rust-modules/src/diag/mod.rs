//! **Typed usage events plus the log/lab diagnostics plumbing.**
//!
//! Three pieces, lifted out of `lab/` on 2026-08-29 when a second consumer appeared. They were
//! written for the Cloud Lab bridge, they were correct, and none of them was lab-shaped:
//!
//! * [`scrub`] — the redaction pass. **Ungated**, because `crate::log` calls
//!   [`scrub::scrub_local`] on every line in every build. See that module's doc for why there are
//!   two exits and why only the remote one may drop a line.
//! * [`ring`] — the bounded in-memory record ring, tapped one call below `redact_tokens`.
//! * [`zlib`] — `dlopen`'d `compress2` plus a gzip envelope, in its own one-symbol table.
//!
//! **`ring` and `zlib` stay behind a feature, `scrub` does not.** A build with neither
//! `lab-diagnostics` nor `telemetry` has nothing to put in a ring and nothing to compress, but it
//! still writes a log file — and the whole point of moving `scrub` here was that its assertions
//! run in the default `make check`, which `lab/`'s cfg had been quietly excluding them from.
//!
//! Logs and Cloud Lab diagnostics have ONE scrubber and take different exits from it. Native Sentry
//! envelopes are deliberately different data: `telemetry::native` applies a fixed JSON field
//! allowlist and path sanitizer before they enter the common consent-gated telemetry spool.

pub(crate) mod scrub;

// UNGATED for the same reason `scrub` is, and it is the same lesson: the guarantee this module
// provides is its TESTS — that action fields cannot carry runtime strings, that bounded context
// fields stay within their allowlisted schema, and that `PRIVACY.md` lists every usage event — and
// tests behind a feature the default gate does not build are tests that never run. `scrub`'s 31
// assertions sat unexecuted for as long as they existed.
pub(crate) mod schema;

/// **Report one event.** The single door, so a call site carries no `#[cfg]` and cannot know
/// whether anything is listening — which is what `lab/mod.rs` does and what keeps the feature
/// attributes off ~25 scattered sites (the hazard `.claude/hooks/release-config-check.py` exists
/// for: a hand-written `cfg` pair where a spliced-in function swallows its neighbour's attribute).
///
/// **It fails closed and it QUEUES; it never sends.** Three gates, in order: consent for this
/// event's category, an install identifier that actually exists, and a build that carries an
/// endpoint at all. Any of them missing is a silent return, which is the only safe direction —
/// and the reason the boot log states which destinations this build can reach, since "consented
/// but not wired" and "sent" look identical from every other line. One bounded exception sits in
/// front of the consent gate: an UNANSWERED decision defers the sign-in family instead of dropping
/// it (`defer`, cap [`DEFERRED_CAP`], in memory, session-only) so a fast sign-in before the first
/// question is answered is not silently lost; an ANSWERED "no" still drops, exactly as before.
///
/// Sending happens on `telemetry::flush_soon`'s worker. This is the frame loop: a send opens a
/// socket and can block for the sender's whole timeout.
///
/// It was a sink in every build when it was written, deliberately — the call sites are the part
/// that has to be right, each one being a decision about what may be observed, and a schema with
/// no producers is an allowlist nobody has checked against reality.
pub(crate) fn event(e: schema::DiagEvent) {
    event_for(e, None);
}

/// Report an action against the exact Plex server it used. This is the only honest source for
/// connection class in a multi-server account; generic events deliberately pass no server.
pub(crate) fn event_for_server(e: schema::DiagEvent, server: crate::plex::ServerId) {
    event_for(e, Some(server));
}

fn event_for(e: schema::DiagEvent, server: Option<crate::plex::ServerId>) {
    event_for_impl(e, server, None);
}

/// The stamp a deferred event was born with — captured once, at [`defer`] time, so a later
/// [`replay_deferred`] reports it as having happened when it actually did rather than when the
/// consent question finally got answered. `session_id` is `&'static str` because [`session_id`]
/// itself never changes within one process; captured anyway, alongside the timestamp, so the
/// replayed event's stamp is entirely self-contained rather than re-derived from process state
/// that happens not to have moved.
#[derive(Clone, Copy)]
struct Stamp {
    occurred_at_ms: u64,
    /// `None` when [`session_id`] could not be read at defer time (a transient `/dev/urandom`
    /// failure — it never un-latches once it has answered `Some`, so this is the one shape a
    /// replay can see that the direct path cannot). Kept as the fail-closed sentinel rather than
    /// coerced to `""`, so a replay drops the event exactly as the direct path already does
    /// instead of emitting an out-of-domain empty session id.
    session_id: Option<&'static str>,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn event_for_impl(
    e: schema::DiagEvent,
    server: Option<crate::plex::ServerId>,
    stamp: Option<Stamp>,
) {
    // **The gate, and it is here rather than at the call sites on purpose**: one place to be right,
    // and no site can forget it. Reads a published snapshot — never the disk, never a lock — which
    // is the shape `diag::scrub`'s identity list had to be rebuilt into after wiring it to
    // `session::peek()` put five file reads on every log line and deadlocked the `auth` tests.
    //
    // Every event declared today is a USAGE event. When error events arrive they ask the other
    // switch, and the mapping becomes a `match` on the variant.
    if !crate::telemetry::consent::allows_usage() {
        // **Issue #75: a sign-in that fails never reaches the consent question at all** —
        // `app.rs::maybe_ask_consent` asks it only once an authorized account exists — so this gate
        // drops the whole sign-in funnel for every attempt that never gets that far. When the ONLY
        // reason this event was dropped is that consent is UNANSWERED (as against answered "no",
        // which is a real decision and must stay dropped), hold a small, bounded, SIGN-IN-ONLY set
        // of events so a later "yes" can still see the funnel that led to it. See `defer`.
        //
        // A replayed event (`stamp.is_some()`) that STILL finds consent unanswered is not
        // re-deferred — `replay_deferred` empties the queue unconditionally, and re-holding it here
        // would fight that.
        let unresolved = crate::telemetry::consent::current().is_none_or(|c| !c.ever_answered())
            || crate::telemetry::consent::usage_enable_pending();
        if unresolved && stamp.is_none() && is_deferrable(&e) {
            defer(e);
        }
        return;
    }
    // Resolved here — right after the only gate that can turn this into a defer — rather than
    // further down beside the network-shaped checks below, so a replay's stamp is fixed before
    // anything build-config-dependent (`has_posthog`) can short-circuit the function; see the
    // `stamp_replayed_through_the_impl_is_what_gets_used` test, which observes this regardless of
    // whether the checkout carries a PostHog key at all.
    let (occurred_at_ms, session_id) = match stamp {
        // The same fail-closed drop the direct path takes below — a replayed event whose stamp
        // never got a real session id (a transient `/dev/urandom` failure at defer time) must not
        // emit one with an out-of-domain empty string in its place.
        Some(Stamp {
            occurred_at_ms,
            session_id: Some(session_id),
        }) => (occurred_at_ms, session_id),
        Some(Stamp {
            session_id: None, ..
        }) => return,
        None => {
            let Some(session_id) = session_id() else {
                return;
            };
            (now_ms(), session_id)
        }
    };
    #[cfg(test)]
    tests::record_stamp_used(occurred_at_ms, session_id);
    // An identifier only exists after an opt-in, which `allows_usage` implies — but READ it rather
    // than assume it. "Implies" is how an invariant becomes a panic, and the failure here would be
    // a report with a fabricated id, which is the one outcome the whole design refuses.
    let Some(_) = crate::telemetry::consent::install_id() else {
        return;
    };
    if !crate::telemetry::sender::has_posthog() {
        return;
    }
    let Some(event_id) = random_hex_id() else {
        return; // no randomness source, so nothing could acknowledge this record
    };
    let envelope = match server {
        Some(server) => {
            schema::UsageEnvelope::capture_for_server(e, occurred_at_ms, session_id, server)
        }
        None => schema::UsageEnvelope::capture(e, occurred_at_ms, session_id),
    };
    let Some(body) = envelope.encode() else {
        return;
    };
    // Queued, never sent from here: this is the frame loop, and a send opens a socket. The worker
    // in `telemetry::flush_soon` drains it.
    //
    // **Appended only if usage consent still holds while the spool is exclusively owned.** The
    // gate at the top of this function is a snapshot read; a withdrawal or a sign-out between it
    // and this line publishes its decision first and then takes this same lock to purge, so the
    // record is either refused here or appended and purged — never left behind to be framed under
    // the NEXT sign-in's identifier at send time (`sender::wire_body` attaches the identifier
    // current at the send, not at the capture). The handled playback error already went this way.
    //
    // Deliberately not logged. `crate::log` writes the event log, and an event stream duplicated
    // into the primary debugging surface would double its volume to say nothing new — every one of
    // these is derived from a line already there.
    let record = crate::telemetry::queue::Record {
        category: crate::telemetry::queue::Category::Usage,
        dest: crate::telemetry::queue::Dest::PostHog,
        event_id,
        body,
    };
    let _ = crate::telemetry::spool::append_if(&record, crate::telemetry::consent::allows_usage);
}

/// Only the sign-in family may be deferred — issue #75's whole point is that this funnel is the
/// one `app.rs::maybe_ask_consent` can never precede, because it asks only once an account is
/// authorized. Every other event (a route, a playback action) has an ordinary population that was
/// already asked at the normal point in its flow, and holding one of THOSE for a later consent
/// answer would misdate it — a route entered minutes before the question was answered is not the
/// same signal as a route entered after.
fn is_deferrable(e: &schema::DiagEvent) -> bool {
    matches!(
        e,
        schema::DiagEvent::SignInStarted
            | schema::DiagEvent::SignInCompleted
            | schema::DiagEvent::SignInFailed { .. }
            | schema::DiagEvent::SignInCancelled
    )
}

/// How many sign-in events one unanswered flow may hold before the oldest is dropped. A generous
/// bound for what one sign-in actually emits (started, at most a few failures across
/// `MAX_PIN_GENERATIONS` retries, then completed or cancelled) — bounded because this is memory a
/// stranger's television carries for as long as the consent question sits unanswered, which could
/// be indefinitely.
const DEFERRED_CAP: usize = 8;

/// One held sign-in event: the event itself, plus the stamp it was born with — see [`Stamp`].
struct Deferred {
    event: schema::DiagEvent,
    stamp: Stamp,
}

/// The sign-in events waiting on an unanswered consent decision. Session-only: never persisted, so
/// a crash or relaunch before the question is answered loses them, same as any other in-memory
/// queue here.
static DEFERRED: std::sync::Mutex<std::collections::VecDeque<Deferred>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());

fn defer(e: schema::DiagEvent) {
    // Captured HERE, not at replay: the event happened now, and a replay that recomputed "now"
    // would report it as having happened whenever the consent question finally got answered —
    // which, for a stalled sign-in, could be minutes or a relaunch away.
    let stamp = Stamp {
        occurred_at_ms: now_ms(),
        session_id: session_id(),
    };
    let mut q = DEFERRED.lock().unwrap_or_else(|e| e.into_inner());
    if q.len() >= DEFERRED_CAP {
        q.pop_front();
    }
    q.push_back(Deferred { event: e, stamp });
}

/// Drain and replay every deferred sign-in event through the normal gated path. Called from
/// `telemetry::record_with_receipt` after the new decision becomes effective — a "yes" lets these through
/// exactly as if consent had already been answered when they first happened; a "no" hits the same
/// gate every other event does and is dropped, which is why this drains UNCONDITIONALLY rather
/// than checking the answer itself: emptying the queue either way is what keeps a refused answer
/// from leaking a stale queue into whichever account signs in next.
///
/// Each event is replayed with the [`Stamp`] it was captured with at `defer` time — not a freshly
/// computed "now" — so the funnel a person can see afterwards is dated when the sign-in actually
/// happened, not when they finally answered the consent question.
pub(crate) fn replay_deferred() {
    let events: Vec<Deferred> = {
        let mut q = DEFERRED.lock().unwrap_or_else(|e| e.into_inner());
        q.drain(..).collect()
    };
    for d in events {
        event_for_impl(d.event, None, Some(d.stamp));
    }
}

/// Drop every deferred sign-in event with no replay. Called on sign-out/delete-local-data so a
/// queued event from the departing account's attempt can never cross into the next account's
/// consent decision.
pub(crate) fn clear_deferred() {
    DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// A process-local session identity. Random and never persisted separately: queued events carry the
/// value they were born with, so a later boot cannot merge an offline session into its own.
fn session_id() -> Option<&'static str> {
    static SESSION: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SESSION.get_or_init(random_uuid_v4).as_deref()
}

fn random_bytes() -> Option<[u8; 16]> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// Generic durable-record identity; no analytics code depends on a crash vendor for IDs.
pub(crate) fn random_hex_id() -> Option<String> {
    Some(random_bytes()?.iter().map(|b| format!("{b:02x}")).collect())
}

fn random_uuid_v4() -> Option<String> {
    let mut b = random_bytes()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    ))
}

// Gated to their present consumer, which is still only the lab bridge. The telemetry client turned
// out not to want either — it frames its own bodies and posts them uncompressed — so this stayed a
// one-consumer gate rather than widening, and there is no `feature = "telemetry"` to widen to (see
// `Cargo.toml`: that feature existed, gated nothing, and is gone). Deliberately not widened ahead
// of a caller either way, because `warnings = "deny"` turns "compiled but unused" into a build
// error, and that is the check doing the work here.
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod ring;
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod zlib;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::consent::{self, Consent};

    fn deferred_len() -> usize {
        DEFERRED.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// What `event_for_impl` last resolved as `(occurred_at_ms, session_id)`, recorded right where
    /// that value is fixed — before anything build-config-dependent (`has_posthog`) can
    /// short-circuit the function — so this observation works whether or not this checkout carries
    /// a PostHog key.
    static LAST_STAMP_USED: std::sync::Mutex<Option<(u64, &'static str)>> =
        std::sync::Mutex::new(None);

    pub(super) fn record_stamp_used(occurred_at_ms: u64, session_id: &'static str) {
        *LAST_STAMP_USED.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((occurred_at_ms, session_id));
    }

    fn take_last_stamp_used() -> Option<(u64, &'static str)> {
        LAST_STAMP_USED.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    /// Runs `body` under `testlock::serial()` with the published consent snapshot saved and
    /// restored around it, and the deferral queue cleared before and after — same shape as
    /// `telemetry::consent`'s own snapshot-install tests, since this module reads that same
    /// `CURRENT` global.
    fn with_consent_snapshot(body: impl FnOnce()) {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        clear_deferred();
        body();
        clear_deferred();
        if let Some(c) = saved {
            consent::install(c);
        }
    }

    /// **Issue #75: a `SignInStarted` that `diag::event` cannot send because consent has never
    /// been answered is held, not dropped**, so a later "yes" can still see it.
    #[test]
    fn sign_in_started_defers_while_consent_is_unanswered() {
        with_consent_snapshot(|| {
            consent::install(Consent::default()); // unanswered
            event(schema::DiagEvent::SignInStarted);
            assert_eq!(deferred_len(), 1);
        });
    }

    #[test]
    fn sign_in_started_defers_while_a_requested_usage_enable_is_pending() {
        with_consent_snapshot(|| {
            consent::install(Consent {
                asked_version: consent::POLICY_VERSION,
                ..Default::default()
            });
            consent::request_for_test(Consent {
                asked_version: consent::POLICY_VERSION,
                usage: true,
                usage_scope: consent::USAGE_SCOPE,
                install_id: Some("u".repeat(32)),
                ..Default::default()
            });
            event(schema::DiagEvent::SignInStarted);
            assert_eq!(deferred_len(), 1);
        });
    }

    /// Only the sign-in family is held back — every other event drops exactly as it always did
    /// once consent is unanswered, since holding a route or a playback action for a later consent
    /// answer would misdate it against when it actually happened.
    #[test]
    fn a_non_signin_event_is_never_deferred() {
        with_consent_snapshot(|| {
            consent::install(Consent::default()); // unanswered
            event(schema::DiagEvent::RouteEntered { screen: "home" });
            assert_eq!(deferred_len(), 0);
        });
    }

    /// An unanswered flow cannot grow the queue without bound — bounded because this is memory a
    /// stranger's television carries for as long as the question sits unanswered.
    #[test]
    fn the_deferred_queue_is_capped() {
        with_consent_snapshot(|| {
            consent::install(Consent::default()); // unanswered
            for _ in 0..(DEFERRED_CAP + 5) {
                event(schema::DiagEvent::SignInStarted);
            }
            assert_eq!(deferred_len(), DEFERRED_CAP);
        });
    }

    /// A held event is replayed through the normal gated path once a decision is published —
    /// **and it drains the queue whichever way that decision goes.** Here it is a real "no", so
    /// each replayed event hits the ordinary consent gate and is dropped rather than re-deferred
    /// (consent is now ANSWERED, so `event_for`'s unanswered check is false) — the queue still
    /// ends up empty, exactly as the consent persistence path promises for a refused answer.
    #[test]
    fn replay_deferred_empties_the_queue() {
        with_consent_snapshot(|| {
            consent::install(Consent::default()); // unanswered
            event(schema::DiagEvent::SignInStarted);
            assert_eq!(deferred_len(), 1);
            consent::install(Consent {
                asked_version: consent::POLICY_VERSION,
                usage: false,
                ..Default::default()
            }); // answered "no"
            replay_deferred();
            assert_eq!(deferred_len(), 0);
        });
    }

    /// **A replayed event keeps the stamp it was born with, not the moment it was replayed.**
    /// Deferred at consent-unanswered time; the test sleeps past that instant, then answers
    /// consent "yes" (so the replay reaches past the unanswered gate and resolves a stamp at all)
    /// and replays. The stamp `event_for_impl` resolves for the replay must equal the ORIGINAL
    /// capture, not a fresh `now_ms()` taken after the sleep.
    #[test]
    fn stamp_replayed_through_the_impl_is_what_gets_used() {
        with_consent_snapshot(|| {
            consent::install(Consent::default()); // unanswered
            event(schema::DiagEvent::SignInStarted);
            assert_eq!(deferred_len(), 1);
            let original_stamp = {
                let q = DEFERRED.lock().unwrap_or_else(|e| e.into_inner());
                q.front().expect("one deferred event").stamp
            };
            std::thread::sleep(std::time::Duration::from_millis(5));
            assert!(
                now_ms() > original_stamp.occurred_at_ms,
                "the sleep must actually move the clock, or this test proves nothing"
            );
            consent::install(Consent {
                asked_version: consent::POLICY_VERSION,
                usage: true,
                errors: true,
                install_id: Some("i".repeat(32)),
                errors_id: Some("e".repeat(32)),
                ..Default::default()
            }); // answered "yes" — replay now resolves a stamp instead of re-deferring
            take_last_stamp_used(); // clear anything a previous test left behind
            replay_deferred();
            let used = take_last_stamp_used().expect("event_for_impl resolved a stamp");
            assert_eq!(
                used.0, original_stamp.occurred_at_ms,
                "the replayed event must keep its ORIGINAL occurred_at_ms"
            );
            assert_eq!(Some(used.1), original_stamp.session_id);
        });
    }

    /// **A replayed event whose stamp never got a real session id is dropped, not sent with an
    /// empty one.** `Stamp::session_id` is `Option<&'static str>` (fail-closed) rather than a
    /// bare `&str` coerced from a `/dev/urandom` failure at defer time — the direct path already
    /// drops on `session_id() == None`, and a replay must take the same drop rather than emitting
    /// a `UsageEnvelope` with `session_id: ""`, an out-of-domain value PRIVACY.md documents as a
    /// per-launch random id.
    #[test]
    fn a_replayed_stamp_with_no_session_id_is_dropped_not_emitted_empty() {
        with_consent_snapshot(|| {
            consent::install(Consent {
                asked_version: consent::POLICY_VERSION,
                usage: true,
                errors: true,
                install_id: Some("i".repeat(32)),
                errors_id: Some("e".repeat(32)),
                ..Default::default()
            });
            take_last_stamp_used(); // clear anything a previous test left behind
            event_for_impl(
                schema::DiagEvent::SignInStarted,
                None,
                Some(Stamp {
                    occurred_at_ms: 1,
                    session_id: None,
                }),
            );
            assert!(
                take_last_stamp_used().is_none(),
                "a stamp with no session id must never reach the point that resolves one"
            );
        });
    }

    /// Sign-out (`auth::forget_account`) must never let a queued event from the departing
    /// account's attempt cross into the next account's consent decision.
    #[test]
    fn clear_deferred_drops_whatever_was_held() {
        with_consent_snapshot(|| {
            consent::install(Consent::default()); // unanswered
            event(schema::DiagEvent::SignInStarted);
            assert_eq!(deferred_len(), 1);
            clear_deferred();
            assert_eq!(deferred_len(), 0);
        });
    }

    #[test]
    fn durable_record_and_session_ids_have_their_required_shapes() {
        let Some(record) = random_hex_id() else {
            return;
        };
        assert_eq!(record.len(), 32);
        assert!(record
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        let Some(session) = random_uuid_v4() else {
            return;
        };
        assert_eq!(session.len(), 36);
        assert_eq!(&session[14..15], "4", "UUID version");
        assert!(
            matches!(&session[19..20], "8" | "9" | "a" | "b"),
            "UUID variant"
        );
    }
}
