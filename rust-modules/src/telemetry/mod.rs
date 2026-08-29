//! **Telemetry: the decision, and one day the senders.**
//!
//! Today this is [`consent`] and its storage, plus the wire FORMATS in [`sentry`] and [`posthog`] — and nothing that can
//! send: there is no queue, no socket and no endpoint, and `diag::event` is a sink. The module
//! grows in that order on purpose. Consent is the part that has to be right before anything can be
//! sent and is answerable entirely on the host; the envelope is the part whose failures are silent
//! 400s from a server that explains nothing, so it is worth pinning to tests while there is still
//! no network to hide behind. Building both first means the sender arrives into a shape that
//! already refuses to send and already frames correctly when it does.
//!
//! **Ungated**, like `diag::scrub` and `diag::schema`, and for the reason both of those record:
//! the guarantees here are the tests — that no identifier exists before an opt-in, that withdrawal
//! destroys it, that the event path fails closed — and a test behind a feature the default gate
//! does not build is a test that never runs. What will be gated is the sending.
pub(crate) mod consent;
pub(crate) mod posthog;
pub(crate) mod queue;
pub(crate) mod sender;
pub(crate) mod sentry;

use consent::Consent;

/// Load the stored decision and publish it for the event path.
///
/// Called once at boot, before anything can report. A missing or unparsable file is the DEFAULT
/// decision — everything off, unanswered — which is the only safe reading: a file we cannot
/// understand is not consent.
pub(crate) fn boot() {
    let c = load();
    // Logged because the alternative is a silent behavioural difference between two televisions.
    // No identifier in the line: it is the one field here worth not putting in a log that gets
    // pasted into issue threads, and its PRESENCE is the only fact worth stating anyway.
    crate::log(&format!(
        "telemetry: answered={} errors={} usage={} id={}",
        c.answered(),
        c.errors,
        c.usage,
        if c.install_id.is_some() { "yes" } else { "none" }
    ));
    consent::install(c);
}

/// The first candidate that exists and parses. Same search-order shape as the session file, and
/// for the same reason: which of the two `/media` directories is writable depends on the jail
/// profile, so the answer cannot be a literal.
fn load() -> Consent {
    crate::paths::telemetry_candidates()
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .find_map(|b| serde_json::from_slice::<Consent>(&b).ok())
        .unwrap_or_default()
}

/// Record a decision: write it, then publish it. **Write first** — a decision that took effect but
/// did not persist would silently re-ask on the next boot while having already acted on itself.
///
/// A total write failure is logged and still applied to this session. The alternative is refusing
/// to honour something a person just chose because a disk is full, which is worse in both
/// directions: it ignores a "no", and it ignores a "yes".
#[allow(dead_code)] // its caller is the consent screen, which lands with the UI
pub(crate) fn record(c: Consent) {
    let Ok(json) = serde_json::to_vec_pretty(&c) else { return };
    let stored = crate::paths::telemetry_candidates()
        .iter()
        .any(|p| crate::plex::session::write_atomic(p, &json));
    if !stored {
        crate::log("telemetry: could not persist the decision to ANY candidate path");
    }
    consent::install(c);
}

// ---- the spool, and the one worker that drains it ---------------------------------------------

/// Guards against two flushes at once. A spool is a read-modify-write of one file, so two workers
/// racing would have the second write back a list that does not know what the first acknowledged —
/// re-sending records that were accepted, which is the duplicate-issue failure `event_id` reuse
/// exists to prevent, arriving by a different door.
static FLUSHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Read the spool from the first candidate that has one.
fn load_spool() -> Vec<queue::Record> {
    crate::paths::telemetry_spool_candidates()
        .iter()
        .find_map(|p| std::fs::read(p).ok())
        .map(|b| {
            let d = queue::decode_all(&b);
            if d.dropped_bytes > 0 {
                // Expected after a power cut, and worth one line either way: a non-zero count after
                // a CLEAN shutdown means something worse than a torn write.
                crate::log(&format!(
                    "telemetry: spool recovered {} records, {} bytes discarded",
                    d.records.len(),
                    d.dropped_bytes
                ));
            }
            d.records
        })
        .unwrap_or_default()
}

/// Write the spool back, trimmed. Atomic, like the decision file — a torn spool is survivable by
/// construction, but there is no reason to manufacture one on every flush.
fn save_spool(records: &[queue::Record]) {
    let (kept, dropped) = queue::trim(records.to_vec());
    if dropped > 0 {
        // Never silent. A queue that discards without saying so is a queue whose numbers are wrong
        // in a direction nobody can see.
        crate::log(&format!("telemetry: spool over cap, dropped {dropped} oldest records"));
    }
    let bytes: Vec<u8> = kept.iter().filter_map(queue::encode).flatten().collect();
    let stored = crate::paths::telemetry_spool_candidates()
        .iter()
        .any(|p| crate::plex::session::write_atomic(p, &bytes));
    if !stored {
        crate::log("telemetry: could not persist the spool to ANY candidate path");
    }
}

/// Queue one record for later. Cheap, main-thread-safe, and it does NOT send.
pub(crate) fn enqueue(r: queue::Record) {
    let mut all = load_spool();
    all.push(r);
    save_spool(&all);
}

/// Drain the spool on a worker thread.
///
/// **Never the main loop and never signal context.** A flush opens a socket and can block for
/// [`sender`]'s whole timeout; on the main thread that is a visibly frozen interface, and from a
/// signal handler it is neither async-signal-safe nor able to finish.
///
/// Returns immediately. A refused spawn is a return value rather than a panic — `task::spawn_small`
/// exists because `thread::spawn` panics on EAGAIN and killed this app once.
pub(crate) fn flush_soon() {
    use std::sync::atomic::Ordering;
    if !sender::configured() {
        return; // nothing in this build to send to — see `sender`'s module doc
    }
    // A decision must EXIST — nothing loaded means nothing consented.
    let Some(c) = consent::current() else { return };
    // …but deliberately no `if !c.any() { return }`. A withdrawal is exactly when the spool most
    // needs draining: `flush_now` retires a record whose category is now off without sending it,
    // so the purge rides the same path instead of needing one of its own. Returning early here
    // would leave records on disk that nobody has consented to, which is the opposite of what a
    // withdrawal is for. With both switches off the flush loads the spool, retires everything and
    // writes back an empty file, sending nothing.
    let _ = c.any();
    if FLUSHING.swap(true, Ordering::AcqRel) {
        return; // one at a time — see FLUSHING
    }
    let ok = crate::task::spawn_small("telemetry", move || {
        flush_now(&c);
        FLUSHING.store(false, Ordering::Release);
    });
    if !ok {
        FLUSHING.store(false, Ordering::Release);
    }
}

/// The flush itself, on the worker.
fn flush_now(c: &consent::Consent) {
    let all = load_spool();
    if all.is_empty() {
        return;
    }
    // Records that leave the spool, whether because a server took them or because nobody consents
    // to them any more. One list, because `queue::ack` asks one question — is this record still
    // ours to keep — and the two reasons for "no" need no distinction downstream.
    let mut retired: Vec<String> = Vec::new();
    for r in &all {
        // Per record, against its own category — a spool written before a withdrawal can still hold
        // records of a category that is now off.
        if !sender::allowed(r, c) {
            retired.push(r.event_id.clone()); // never sent: consent for it no longer exists
            continue;
        }
        match sender::send_one(r) {
            (sender::Verdict::Done, _) => retired.push(r.event_id.clone()),
            (sender::Verdict::Hopeless, _) => retired.push(r.event_id.clone()),
            // Held. Stop the whole flush rather than working down the list: whatever stopped this
            // record — rate limit, dead link, a server having a bad day — applies to the next one
            // too, and hammering an endpoint that just said stop is how a client earns a longer
            // ban. The hold is honoured by simply not trying again until the next flush.
            (sender::Verdict::Keep, hold) => {
                let s = hold.unwrap_or(sender::DEFAULT_HOLD_S);
                crate::log(&format!("telemetry: holding {} records, ~{s}s", all.len() - retired.len()));
                break;
            }
        }
    }
    if !retired.is_empty() {
        save_spool(&queue::ack(all, &retired));
    }
}

/// 16 bytes of `/dev/urandom` as lowercase hex — the ONLY way an `install_id` is ever produced.
///
/// Reads the device directly rather than taking a dependency: this crate has no RNG, and the one
/// property that matters is that the value is not derived from anything about this television or
/// this account. A read failure yields `None`, and [`consent::apply`]'s caller must then treat the
/// opt-in as not yet complete rather than inventing a fallback — a "random" identifier built from a
/// clock or a MAC is exactly the identifier this design refuses.
#[allow(dead_code)] // its caller is the consent screen, which lands with the UI
pub(crate) fn mint_install_id() -> Option<String> {
    let mut buf = [0u8; 16];
    use std::io::Read;
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The identifier is 32 hex characters and two mints differ. Not a randomness test — it is a
    /// test that the SOURCE is the device and not a constant, which is the failure that would make
    /// every install share one id and nobody notice.
    #[test]
    fn a_minted_identifier_is_random_hex() {
        let Some(a) = mint_install_id() else { return }; // no /dev/urandom: nothing to assert
        assert_eq!(a.len(), 32, "16 bytes as hex");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let b = mint_install_id().expect("second read");
        assert_ne!(a, b, "two mints produced the same identifier");
    }

    /// An unreadable or corrupt file is the DEFAULT decision, never a partial one — a file we
    /// cannot understand is not consent.
    #[test]
    fn an_unparsable_file_is_not_consent() {
        let c: Consent = serde_json::from_slice(b"{ not json").unwrap_or_default();
        assert!(!c.any() && !c.answered());
    }

    /// A file written by a FUTURE build, carrying fields this one does not know, still parses —
    /// and a file missing fields still parses. Both matter on a device that can be downgraded by a
    /// reinstall while the file survives it.
    #[test]
    fn the_stored_shape_tolerates_version_skew() {
        let older: Consent = serde_json::from_slice(br#"{"asked_version":1,"usage":true}"#)
            .expect("a file with fewer fields still parses");
        assert!(older.usage && !older.errors && older.install_id.is_none());
        let newer: Consent =
            serde_json::from_slice(br#"{"asked_version":1,"usage":true,"a_field_from_later":7}"#)
                .expect("a file with more fields still parses");
        assert!(newer.usage);
    }
}
