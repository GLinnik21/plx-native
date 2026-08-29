//! **Diagnostics plumbing shared by every channel that reports something off this device.**
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
//! There is ONE scrubber. If a future channel needs different redaction, it takes a different exit
//! from this module rather than a second implementation of it.

pub(crate) mod scrub;

// UNGATED for the same reason `scrub` is, and it is the same lesson: the guarantee this module
// provides is its TESTS — that no event can carry a runtime string, and that `PRIVACY.md` lists
// every event — and tests behind a feature the default gate does not build are tests that never
// run. `scrub`'s 31 assertions sat unexecuted for as long as they existed.
pub(crate) mod schema;

/// **Report one event.** The single door, so a call site carries no `#[cfg]` and cannot know
/// whether anything is listening — which is what `lab/mod.rs` does and what keeps the feature
/// attributes off ~25 scattered sites (the hazard `.claude/hooks/release-config-check.py` exists
/// for: a hand-written `cfg` pair where a spliced-in function swallows its neighbour's attribute).
///
/// Today nothing listens: there is no consent apparatus, no queue and no endpoint, so this is a
/// sink in every build. It exists NOW because the call sites are the part that has to be right —
/// each one is a decision about what may be observed — and because a schema with no producers is
/// an allowlist nobody has checked against reality.
pub(crate) fn event(e: schema::DiagEvent) {
    // **The gate, and it is here rather than at the call sites on purpose**: one place to be right,
    // and no site can forget it. Reads a published snapshot — never the disk, never a lock — which
    // is the shape `diag::scrub`'s identity list had to be rebuilt into after wiring it to
    // `session::peek()` put five file reads on every log line and deadlocked the `auth` tests.
    //
    // Every event declared today is a USAGE event. When error events arrive they ask the other
    // switch, and the mapping becomes a `match` on the variant.
    if !crate::telemetry::consent::allows_usage() {
        return;
    }
    // An identifier only exists after an opt-in, which `allows_usage` implies — but READ it rather
    // than assume it. "Implies" is how an invariant becomes a panic, and the failure here would be
    // a report with a fabricated id, which is the one outcome the whole design refuses.
    let Some(id) = crate::telemetry::consent::current().and_then(|c| c.install_id) else {
        return;
    };
    let Some(body) = crate::telemetry::sender::posthog_body(e, &id) else {
        return; // this build carries no PostHog key — see `telemetry::sender`
    };
    let Some(event_id) = crate::telemetry::sentry::new_event_id() else {
        return; // no randomness source, so nothing could acknowledge this record
    };
    // Queued, never sent from here: this is the frame loop, and a send opens a socket. The worker
    // in `telemetry::flush_soon` drains it.
    //
    // Deliberately not logged. `crate::log` writes the event log, and an event stream duplicated
    // into the primary debugging surface would double its volume to say nothing new — every one of
    // these is derived from a line already there.
    crate::telemetry::enqueue(crate::telemetry::queue::Record {
        category: crate::telemetry::queue::Category::Usage,
        dest: crate::telemetry::queue::Dest::PostHog,
        event_id,
        body,
    });
}

// Gated to their present consumer. Phase G/H widen these to
// `any(feature = "lab-diagnostics", feature = "telemetry")` when the Sentry and PostHog clients
// become second callers — deliberately not widened ahead of a caller, because `warnings = "deny"`
// turns "compiled but unused" into a build error, and that is the check doing the work here.
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod ring;
#[cfg(feature = "lab-diagnostics")]
pub(crate) mod zlib;
