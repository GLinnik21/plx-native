//! **The Sentry envelope, hand-rolled** — the pure half: parse a DSN, frame an envelope, budget it.
//!
//! No SDK, and that is a decision with reasons rather than a preference: `sentry-rust` pulls tokio
//! and reqwest into a crate that has neither, `sentry-native` does not list armv7, `minidump-writer`
//! grades `arm × unknown-linux-gnu` "untested, needs more work" and wants ptrace from a second
//! process, and Crashpad has no armv7 at all. Against that, the envelope format is a documented,
//! versioned, newline-delimited wire format that this file implements in about a hundred lines and
//! that `make check` can grade end to end without a network.
//!
//! **Everything here is pure.** A DSN goes in as a string, bytes come out. There is no socket, no
//! endpoint and no credential in this file — which is what lets the whole format be tested against
//! a synthetic DSN on the host, months before anyone has an account, and what will keep those tests
//! meaningful afterwards.
//!
//! # The failure modes this exists to make impossible
//!
//! Every one of these silently produces a 400 from a server that tells you nothing useful, which is
//! why they are asserted rather than trusted:
//!
//! * **the item header's `length` must be the payload's BYTE length**, not its character count and
//!   not off by the trailing newline. A wrong length makes the receiver read the next item's
//!   header as payload;
//! * **`event_id` is 32 lowercase hex characters with NO dashes**. A dashed UUID is rejected;
//! * **a retry reuses the original `event_id`**, or a flaky link manufactures duplicate issues out
//!   of one crash;
//! * **the compressed item limit is 200 KiB**, not the 1 MiB decompressed figure that gets quoted.
//!   Budgeting against the wrong one means the payload is refused exactly when a crash was
//!   interesting enough to carry a lot of context.
//!
//! # What is NOT here
//!
//! The send. That needs a CA-verified unpinned POST (`net::post_ca`, which does not exist yet) and
//! a DSN, and it is deliberately the only part that does — so this file's tests never need either.
//!
//! # Why every item carries `#[allow(dead_code)]`
//!
//! The same reason `consent`'s four do, and they go the same way: **the only non-test caller of any
//! of this is the sender**, and the sender is the next commit. Gating the module on a feature
//! instead would make the warnings vanish and take the tests with them, which is exactly how
//! `diag::scrub`'s 31 assertions sat unexecuted for as long as they existed. The attributes are the
//! honest version — they say "no caller yet" out loud, and `make check` still grades the format.

/// Where a DSN says to send, and who it says we are. Every field is derived from the DSN string;
/// none of it is secret — the public key is a write-only ingest credential that any binary sending
/// anything has to carry, which is why it is publishable by design and why this type is ordinary.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) struct Dsn {
    /// `https://o0.ingest.de.sentry.io` — scheme and host, no path
    pub origin: String,
    /// the public key, which becomes `sentry_key` in the auth header
    pub public_key: String,
    /// the numeric project id, which becomes the path segment
    pub project_id: String,
}

impl Dsn {
    /// The envelope endpoint this DSN addresses.
    #[allow(dead_code)] // no sender yet — see the module doc
    pub(crate) fn envelope_url(&self) -> String {
        format!("{}/api/{}/envelope/", self.origin, self.project_id)
    }

    /// The `X-Sentry-Auth` header value. `sentry_version=7` is the protocol this file implements;
    /// `sentry_client` is ours and is what a Sentry-side filter would key on if this ever needed
    /// one.
    #[allow(dead_code)] // no sender yet — see the module doc
    pub(crate) fn auth_header(&self) -> String {
        format!(
            "X-Sentry-Auth: Sentry sentry_version=7, sentry_client=plxnative/{}, sentry_key={}",
            env!("CARGO_PKG_VERSION"),
            self.public_key
        )
    }
}

/// Parse `https://<key>@<host>/<project_id>`.
///
/// **Fails closed on anything it does not fully understand**, returning `None` rather than a
/// partially-filled `Dsn`. A misparsed DSN does not fail loudly at runtime — it posts to a URL that
/// answers 404, or worse to the right URL with the wrong key, and the symptom is silence from a
/// system whose entire job is to break silence. There is nothing to lose by refusing: no DSN means
/// no telemetry, which is the safe direction.
///
/// Hand-rolled rather than a URL crate, for this crate's usual reason: the input is one shape, the
/// parse cannot fail in an interesting way, and the alternative is a dependency in a binary that
/// ships to televisions.
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) fn parse_dsn(dsn: &str) -> Option<Dsn> {
    let dsn = dsn.trim();
    let (scheme, rest) = dsn.split_once("://")?;
    // https only. A DSN is a credential in a query-free URL, but the payload is not: an event
    // carries a stack trace, and http would put it on the wire in clear.
    if scheme != "https" {
        return None;
    }
    let (public_key, rest) = rest.split_once('@')?;
    if public_key.is_empty() || !public_key.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let (host, project_id) = rest.rsplit_once('/')?;
    if host.is_empty() || host.contains('/') {
        return None;
    }
    // The project id is the last path segment and is numeric. Checked because the most likely
    // malformed DSN is one with a trailing slash, which would otherwise parse to an empty id and
    // post to `/api//envelope/`.
    if project_id.is_empty() || !project_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(Dsn {
        origin: format!("{scheme}://{host}"),
        public_key: public_key.to_string(),
        project_id: project_id.to_string(),
    })
}

/// Is this DSN pointed at Sentry's EU region?
///
/// Not enforced here — a caller may have good reason to use another — but SURFACED, because the
/// whole data-protection position this app publishes (`PRIVACY.md`, and the "location of data
/// stored" field of LG's Data Safety declaration) rests on it, and Sentry fixes an organisation's
/// region at creation: it cannot be moved later, only replaced. A build that quietly shipped a
/// `.us.` DSN would make a published document false, and nothing else would notice.
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) fn is_eu_region(d: &Dsn) -> bool {
    d.origin.contains(".de.sentry.io")
}

/// A 32-character lowercase hex id, which is what Sentry's `event_id` must be — **no dashes**.
///
/// Minted when a record is QUEUED, not when it is sent, and stored with it: a retry must reuse it
/// or one crash over a flaky link becomes several issues. That is a property of the caller, and the
/// reason this returns a value rather than writing one anywhere.
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) fn new_event_id() -> Option<String> {
    super::mint_install_id() // same 16 random bytes as hex; a different draw each call
}

/// **`image_addr` for this binary is the lowest `PT_LOAD` vaddr — `0x10000` — and NOT zero.**
///
/// Device-adjacent measurement, 2026-08-29, against the real Sentry EU project with a real armv7
/// DIF uploaded. This is the number that decides whether a crash report is a source line or a bare
/// address, and getting it wrong fails SILENTLY in the worst way: the image still resolves, the
/// event carries no processing error, and the frame simply comes back
/// `symbolicatorStatus: "missing_symbol"` — indistinguishable from "we never uploaded symbols".
///
/// Both were tried, same address, same DIF, minutes apart:
///
/// | `image_addr` | result |
/// |---|---|
/// | `0x0` | `missing_symbol`, no error reported |
/// | `0x10000` | `symbolicated` -> `plx_crash_install`, `crashtrace.c:290` |
///
/// The reason is that Symbolicator computes `rva = instruction_addr - image_addr` and an ELF's
/// symbol addresses are relative to its own load base, which for this non-PIE executable is the
/// first `PT_LOAD`'s vaddr rather than 0. The plan this was built to said "since the binary is
/// ET_EXEC, frames are absolute `instruction_addr` — no `addr_mode`", which is correct about the
/// FRAME and silent about the image, and the silence is the whole trap: absolute frames plus a zero
/// image base looks obviously right and yields nothing.
///
/// Read from the ELF rather than hardcoded would be better still, and is not possible here — the
/// running process cannot read its own program headers without parsing `/proc/self/exe`. It is a
/// constant because it is a property of the link, and `ci/check-elf.sh` is where a change to it
/// would be caught.
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) const IMAGE_ADDR: &str = "0x10000";

/// The compressed-item ceiling. **200 KiB, and it is the COMPRESSED figure** — the 1 MiB number
/// that gets quoted is the decompressed one, and budgeting against it means an interesting crash
/// is the one that gets refused.
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) const MAX_COMPRESSED: usize = 200 * 1024;

/// Frame one item into an envelope: an envelope header line, an item header line, then the payload.
///
/// Newline-delimited, and the item header's `length` is the payload's byte length — the field this
/// function exists to get right, since a wrong one makes the receiver parse the next line as
/// payload and reject the whole envelope with a message about neither.
#[allow(dead_code)] // no sender yet — see the module doc
pub(crate) fn envelope(event_id: &str, item_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 160);
    out.extend_from_slice(format!("{{\"event_id\":\"{event_id}\"}}\n").as_bytes());
    out.extend_from_slice(
        format!("{{\"type\":\"{item_type}\",\"length\":{}}}\n", payload.len()).as_bytes(),
    );
    out.extend_from_slice(payload);
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "https://abc123def456@o4507.ingest.de.sentry.io/1234567";

    #[test]
    fn a_well_formed_dsn_parses_into_its_three_parts() {
        let d = parse_dsn(GOOD).expect("parses");
        assert_eq!(d.origin, "https://o4507.ingest.de.sentry.io");
        assert_eq!(d.public_key, "abc123def456");
        assert_eq!(d.project_id, "1234567");
        assert_eq!(d.envelope_url(), "https://o4507.ingest.de.sentry.io/api/1234567/envelope/");
        assert!(d.auth_header().starts_with("X-Sentry-Auth: Sentry sentry_version=7,"));
        assert!(d.auth_header().contains("sentry_key=abc123def456"));
    }

    /// **Fails closed on every malformation**, because the alternative is posting somewhere that
    /// answers 404 and calling it "no crashes". Each of these is a real way a DSN gets mangled by
    /// being copied out of a settings page.
    #[test]
    fn a_malformed_dsn_is_refused_rather_than_half_understood() {
        for bad in [
            "",
            "not a dsn",
            "https://o4507.ingest.de.sentry.io/1234567",          // no key
            "https://abc123@o4507.ingest.de.sentry.io/",          // trailing slash, empty id
            "https://abc123@o4507.ingest.de.sentry.io",           // no path at all
            "https://abc123@/1234567",                            // no host
            "https://abc123@o4507.ingest.de.sentry.io/notanumber", // id is not numeric
            "https://@o4507.ingest.de.sentry.io/1234567",         // empty key
        ] {
            assert!(parse_dsn(bad).is_none(), "accepted a malformed DSN: {bad:?}");
        }
    }

    /// **http is refused.** A DSN's key is write-only, but the PAYLOAD is a stack trace and a set of
    /// tags, and cleartext would put that on the wire for anyone on the path.
    #[test]
    fn a_cleartext_dsn_is_refused() {
        assert!(parse_dsn("http://abc123@o4507.ingest.de.sentry.io/1234567").is_none());
    }

    /// The region is SURFACED, because a `.us.` DSN would make a published privacy document false
    /// and nothing else in the system would notice.
    #[test]
    fn the_region_is_visible_from_the_dsn() {
        assert!(is_eu_region(&parse_dsn(GOOD).unwrap()));
        let us = parse_dsn("https://abc123@o4507.ingest.us.sentry.io/1234567").unwrap();
        assert!(!is_eu_region(&us), "a US DSN must not read as EU");
    }

    /// **THE FRAMING.** The item header's `length` is the payload's BYTE length — not its character
    /// count, and not including the newline the framing adds after it. A wrong value here makes the
    /// receiver read the following bytes as a header and reject the envelope with a message about
    /// something else entirely.
    #[test]
    fn the_item_header_states_the_payloads_byte_length() {
        // Deliberately multi-byte: a `chars().count()` implementation passes an ASCII-only test and
        // fails on the first non-ASCII string in a message or a filename.
        let text = r#"{"m":"café — naïve"}"#;
        assert_ne!(
            text.len(),
            text.chars().count(),
            "the fixture must be multi-byte, or this test cannot tell bytes from characters"
        );
        let payload = text.as_bytes();
        let env = envelope("0123456789abcdef0123456789abcdef", "event", payload);
        let text = String::from_utf8(env).expect("utf-8");
        let mut lines = text.split('\n');
        assert_eq!(lines.next().unwrap(), r#"{"event_id":"0123456789abcdef0123456789abcdef"}"#);
        assert_eq!(lines.next().unwrap(), format!(r#"{{"type":"event","length":{}}}"#, payload.len()));
        assert_eq!(lines.next().unwrap().as_bytes(), payload);
        assert_eq!(lines.next().unwrap(), "", "the envelope ends with a newline");
    }

    /// An empty payload still frames correctly — `length: 0` is legal and is what an item with no
    /// body looks like.
    #[test]
    fn an_empty_payload_frames_as_length_zero() {
        let env = envelope("0123456789abcdef0123456789abcdef", "event", b"");
        let text = String::from_utf8(env).unwrap();
        assert!(text.contains(r#""length":0"#), "{text}");
    }

    /// `event_id` is 32 lowercase hex characters and carries NO dashes. A dashed UUID — the obvious
    /// thing to reach for — is rejected by the ingest endpoint.
    #[test]
    fn an_event_id_is_undashed_lowercase_hex() {
        let Some(id) = new_event_id() else { return }; // no /dev/urandom on this host
        assert_eq!(id.len(), 32);
        assert!(!id.contains('-'), "an event_id must not be a dashed UUID: {id}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    /// **The DSN this project actually configured parses, and is in the EU region.**
    ///
    /// Reads the gitignored `pkg/telemetry.local.json`, and **skips when it is absent** — which is
    /// every checkout but the maintainer's, and every CI runner. That is not a weakness: the value
    /// it grades is one a person pastes out of a settings page, so the failure it exists to catch
    /// (a mangled copy, or an org created in the wrong region) can only happen where the file is.
    ///
    /// **It never prints the DSN**, on any path — a test failure message goes into a terminal, a
    /// transcript and sometimes an issue. The assertions are shaped to say what is wrong without
    /// quoting what was read, which is the same rule `diag::scrub`'s tests follow.
    #[test]
    fn the_configured_dsn_parses_and_is_eu() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("rust-modules has a parent")
            .join("pkg/telemetry.local.json");
        let Ok(text) = std::fs::read_to_string(&path) else { return }; // not configured here
        let Some(dsn) = text
            .lines()
            .find_map(|l| l.trim().strip_prefix(r#""sentry_dsn": ""#))
            .and_then(|l| l.split('"').next())
        else {
            return; // no key in the file yet
        };
        if dsn.is_empty() || dsn.contains('<') {
            return; // still the example placeholder
        }
        let parsed = parse_dsn(dsn);
        assert!(
            parsed.is_some(),
            "pkg/telemetry.local.json holds a sentry_dsn this parser refuses \
             (not printed here on purpose) — check it was copied whole"
        );
        assert!(
            is_eu_region(&parsed.unwrap()),
            "the configured DSN is NOT in Sentry's EU region. PRIVACY.md and the LG Data Safety \
             declaration both claim EU storage, and Sentry fixes an organisation's region at \
             creation — it cannot be moved, only replaced with a new org"
        );
    }

    /// **The measured `image_addr`.** Pinned as a number because the failure it prevents is silent:
    /// with `0x0` the same address, the same DIF and the same project return `missing_symbol` with
    /// no error attached, which reads exactly like never having uploaded symbols at all. Verified
    /// end to end against the live EU project on 2026-08-29 — `0x10000` resolved
    /// `0x88ef8` to `plx_crash_install` at `crashtrace.c:290`, matching `addr2line` locally.
    #[test]
    fn the_image_base_is_the_load_vaddr_not_zero() {
        assert_eq!(IMAGE_ADDR, "0x10000");
        assert_ne!(IMAGE_ADDR, "0x0", "a zero image base silently yields missing_symbol");
    }

    /// The budget is the COMPRESSED ceiling, and it is 200 KiB rather than the 1 MiB decompressed
    /// figure that gets quoted. Pinned as a number because getting it wrong is invisible until a
    /// large payload is silently refused.
    #[test]
    fn the_budget_is_the_compressed_ceiling() {
        assert_eq!(MAX_COMPRESSED, 204_800);
    }
}
