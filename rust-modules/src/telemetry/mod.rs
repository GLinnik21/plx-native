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
