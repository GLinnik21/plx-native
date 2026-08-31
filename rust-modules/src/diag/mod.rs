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
// provides is its TESTS — that no usage event can carry a runtime string, and that `PRIVACY.md`
// lists every usage event — and tests behind a feature the default gate does not build are tests that never
// run. `scrub`'s 31 assertions sat unexecuted for as long as they existed.
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
/// but not wired" and "sent" look identical from every other line.
///
/// Sending happens on `telemetry::flush_soon`'s worker. This is the frame loop: a send opens a
/// socket and can block for the sender's whole timeout.
///
/// It was a sink in every build when it was written, deliberately — the call sites are the part
/// that has to be right, each one being a decision about what may be observed, and a schema with
/// no producers is an allowlist nobody has checked against reality.
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
    let Some(_) = crate::telemetry::consent::current().and_then(|c| c.install_id) else {
        return;
    };
    if !crate::telemetry::sender::has_posthog() {
        return;
    }
    let Some(event_id) = random_hex_id() else {
        return; // no randomness source, so nothing could acknowledge this record
    };
    let Some(session_id) = session_id() else {
        return;
    };
    let occurred_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let Some(body) = schema::UsageEnvelope::capture(e, occurred_at_ms, session_id).encode() else {
        return;
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
fn random_hex_id() -> Option<String> {
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

    #[test]
    fn durable_record_and_session_ids_have_their_required_shapes() {
        let Some(record) = random_hex_id() else { return };
        assert_eq!(record.len(), 32);
        assert!(record.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));

        let Some(session) = random_uuid_v4() else { return };
        assert_eq!(session.len(), 36);
        assert_eq!(&session[14..15], "4", "UUID version");
        assert!(matches!(&session[19..20], "8" | "9" | "a" | "b"), "UUID variant");
    }
}
