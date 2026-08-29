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
//! # Why every item carries `//!
//! The same reason `consent`'s four do, and they go the same way: **the only non-test caller of any
//! of this is the sender**, and the sender is the next commit. Gating the module on a feature
//! instead would make the warnings vanish and take the tests with them, which is exactly how
//! `diag::scrub`'s 31 assertions sat unexecuted for as long as they existed. The attributes are the
//! honest version — they say "no caller yet" out loud, and `make check` still grades the format.

/// Where a DSN says to send, and who it says we are. Every field is derived from the DSN string;
/// none of it is secret — the public key is a write-only ingest credential that any binary sending
/// anything has to carry, which is why it is publishable by design and why this type is ordinary.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub(crate) fn envelope_url(&self) -> String {
        format!("{}/api/{}/envelope/", self.origin, self.project_id)
    }

    /// The `X-Sentry-Auth` header value. `sentry_version=7` is the protocol this file implements;
    /// `sentry_client` is ours and is what a Sentry-side filter would key on if this ever needed
    /// one.
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
// Test-only, and that is the point of it: `the_configured_dsn_parses_and_is_eu` asserts this
// installation's DSN is an EU one, because `PRIVACY.md` and the LG Data Safety filing both claim
// EU storage and Sentry fixes an organisation's region at creation — it cannot be moved, only
// replaced. Nothing at runtime should ask: a build either has an EU DSN or should not ship.
#[allow(dead_code)]
pub(crate) fn is_eu_region(d: &Dsn) -> bool {
    d.origin.contains(".de.sentry.io")
}

/// A 32-character lowercase hex id, which is what Sentry's `event_id` must be — **no dashes**.
///
/// Minted when a record is QUEUED, not when it is sent, and stored with it: a retry must reuse it
/// or one crash over a flaky link becomes several issues. That is a property of the caller, and the
/// reason this returns a value rather than writing one anywhere.
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
/// **This is the FALLBACK, not the source of truth** — [`image_addr`] derives the number from the
/// running binary's own program headers. It stays written down because it is the measured value
/// above, so a build whose `/proc/self/exe` is unreadable still reports the number that was proven
/// to symbolicate rather than a zero that silently would not.
///
/// This doc used to say deriving it "is not possible here — the running process cannot read its own
/// program headers without parsing `/proc/self/exe`", which names the method and then treats it as
/// a reason not to. `paths::app_dir` has read that link since long before this module existed.
pub(crate) const IMAGE_ADDR: &str = "0x10000";

/// **The image base this binary actually links at**, as the `0x…` string a Sentry debug image wants.
///
/// Derived rather than declared: `Symbolicator` computes `rva = instruction_addr - image_addr`, so
/// this number decides whether a crash is a source line or nothing at all, and it is a property of
/// the LINK — a linker script change, a switch to PIE, a different toolchain default would each
/// move it with no other symptom. Resolved once per process; the read is ~64 KiB of the head of
/// `/proc/self/exe`, not the whole 7 MB binary.
///
/// Falls back to [`IMAGE_ADDR`] whenever the answer cannot be trusted — no `/proc` (the macOS
/// simulator), a Mach-O rather than an ELF, a big-endian object, or `ET_DYN`, where the vaddr in
/// the program header is an offset from a load bias this function cannot see. A wrong non-zero
/// base and a zero base fail identically and invisibly, so "I could not tell" must resolve to the
/// value that was measured to work, never to a guess.
pub(crate) fn image_addr() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        read_head("/proc/self/exe").as_deref().map(image_addr_of).unwrap_or_else(|| IMAGE_ADDR.to_string())
    })
}

/// The pure half of [`image_addr`], so a test grades the expression the app actually uses rather
/// than a re-derivation of it.
fn image_addr_of(buf: &[u8]) -> String {
    lowest_load_vaddr(buf).map(|v| format!("0x{v:x}")).unwrap_or_else(|| IMAGE_ADDR.to_string())
}

/// The head of a file — enough to hold an ELF header and every program header after it. Bounded
/// rather than a whole read: this runs at boot on a set with 1.68 GB, and the binary is 7 MB.
fn read_head(path: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path).ok()?.take(64 * 1024).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// **The GNU build id of the running binary, as lowercase hex** — the only thing that can pair a
/// crash report with the `plxnative.debug` a release cut and uploaded.
///
/// `-Wl,--build-id=sha1` is unconditional on every link (CLAUDE.md: it costs 20 bytes and `strip`
/// preserves it), and a debuginfo build and a plain one produce DIFFERENT ids from identical
/// sources — which is why `SYMBOLS` is in the `RUST_CFG` stamp, and why this has to be read from
/// the binary that is actually running rather than baked in at compile time by anything.
///
/// Empty when it cannot be read, and a caller must then send NO debug image: an image carrying a
/// wrong or absent id is what produces `missing_symbol` with no error, the failure mode
/// [`IMAGE_ADDR`]'s doc records from the other direction.
pub(crate) fn build_id() -> &'static str {
    static V: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    V.get_or_init(|| {
        read_head("/proc/self/exe").as_deref().and_then(gnu_build_id).unwrap_or_default()
    })
}

/// Sentry's `debug_id` for an ELF: the first 16 bytes of the build id, read as a **little-endian**
/// UUID.
///
/// The byte swap is the whole content of this function and it is not decoration — `symbolic`, the
/// library on Sentry's side, builds an ELF debug id with `Uuid::from_slice_le`, so the first three
/// UUID fields are byte-reversed relative to the build id's own order. Emitting the bytes in file
/// order instead produces a perfectly well-formed UUID that matches no uploaded object, and the
/// symptom is a frame that comes back unsymbolicated with no error attached — see [`IMAGE_ADDR`].
///
/// `None` for a build id shorter than 16 bytes, which no `sha1` id is.
pub(crate) fn debug_id(build_id_hex: &str) -> Option<String> {
    let b: Vec<u8> = (0..build_id_hex.len() / 2)
        .map(|i| u8::from_str_radix(&build_id_hex[i * 2..i * 2 + 2], 16).ok())
        .collect::<Option<Vec<u8>>>()?;
    if b.len() < 16 {
        return None;
    }
    Some(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{}",
        b[3], b[2], b[1], b[0],
        b[5], b[4],
        b[7], b[6],
        b[8], b[9],
        b[10..16].iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}

/// Walk an ELF's `PT_NOTE` segments for `NT_GNU_BUILD_ID`. Pure, for the reason
/// [`lowest_load_vaddr`] is.
///
/// Note records are `[namesz][descsz][type]` then the name and the descriptor, each padded to four
/// bytes — and the PADDING is the part that is easy to drop, because `"GNU\0"` is exactly four
/// bytes and so a parser that forgets to round up reads correctly on every ELF anyone would test
/// with and wrongly on the one that has a different note first.
pub(crate) fn gnu_build_id(buf: &[u8]) -> Option<String> {
    const PT_NOTE: u32 = 4;
    const NT_GNU_BUILD_ID: u32 = 3;
    if buf.len() < 64 || &buf[..4] != b"\x7fELF" || buf[5] != 1 {
        return None;
    }
    let is64 = buf[4] == 2;
    let u16at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(buf.get(o..o + 2)?.try_into().ok()?)) };
    let u32at = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(buf.get(o..o + 4)?.try_into().ok()?)) };
    let u64at = |o: usize| -> Option<u64> { Some(u64::from_le_bytes(buf.get(o..o + 8)?.try_into().ok()?)) };
    let (phoff, phentsize, phnum) = if is64 {
        (u64at(32)? as usize, u16at(54)? as usize, u16at(56)? as usize)
    } else {
        (u32at(28)? as usize, u16at(42)? as usize, u16at(44)? as usize)
    };
    for i in 0..phnum {
        let base = phoff.checked_add(i.checked_mul(phentsize)?)?;
        if u32at(base)? != PT_NOTE {
            continue;
        }
        let (off, size) = if is64 {
            (u64at(base + 8)? as usize, u64at(base + 32)? as usize)
        } else {
            (u32at(base + 4)? as usize, u32at(base + 16)? as usize)
        };
        let mut at = off;
        let end = off.checked_add(size)?;
        while at + 12 <= end && at + 12 <= buf.len() {
            let namesz = u32at(at)? as usize;
            let descsz = u32at(at + 4)? as usize;
            let kind = u32at(at + 8)?;
            let name_at = at + 12;
            let desc_at = name_at.checked_add(namesz.next_multiple_of(4))?;
            let desc_end = desc_at.checked_add(descsz)?;
            if kind == NT_GNU_BUILD_ID && buf.get(name_at..name_at + namesz) == Some(b"GNU\0") {
                let d = buf.get(desc_at..desc_end)?;
                return Some(d.iter().map(|b| format!("{b:02x}")).collect());
            }
            let next = desc_at.checked_add(descsz.next_multiple_of(4))?;
            if next <= at {
                break; // a zero-length note would otherwise spin here forever
            }
            at = next;
        }
    }
    None
}

/// The lowest `PT_LOAD` virtual address in an ELF image. Pure — the caller owns the read, which is
/// what makes every branch here host-testable against a hand-built header.
///
/// `None` for anything this cannot answer honestly: not an ELF, big-endian, `ET_DYN` (see
/// [`image_addr`]), a truncated header, or no `PT_LOAD` at all.
pub(crate) fn lowest_load_vaddr(buf: &[u8]) -> Option<u64> {
    const PT_LOAD: u32 = 1;
    const ET_EXEC: u16 = 2;
    if buf.len() < 64 || &buf[..4] != b"\x7fELF" {
        return None;
    }
    let is64 = match buf[4] {
        1 => false,
        2 => true,
        _ => return None,
    };
    if buf[5] != 1 {
        return None; // big-endian: every field below is read little-endian
    }
    let u16at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(buf.get(o..o + 2)?.try_into().ok()?)) };
    let u32at = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(buf.get(o..o + 4)?.try_into().ok()?)) };
    let u64at = |o: usize| -> Option<u64> { Some(u64::from_le_bytes(buf.get(o..o + 8)?.try_into().ok()?)) };

    if u16at(16)? != ET_EXEC {
        return None;
    }
    let (phoff, phentsize, phnum) = if is64 {
        (u64at(32)? as usize, u16at(54)? as usize, u16at(56)? as usize)
    } else {
        (u32at(28)? as usize, u16at(42)? as usize, u16at(44)? as usize)
    };
    // `p_vaddr` is the field after `p_offset` in both classes, at different widths and — because
    // ELF64 moves `p_flags` up to second — different offsets. Getting this pair wrong reads a file
    // offset as a load address, which on this binary happens to be a plausible small number.
    let vaddr_at = if is64 { 16 } else { 8 };
    let min_entry = if is64 { 24 } else { 12 };
    if phentsize < min_entry || phnum == 0 {
        return None;
    }
    (0..phnum)
        .filter_map(|i| {
            let base = phoff.checked_add(i.checked_mul(phentsize)?)?;
            if u32at(base)? != PT_LOAD {
                return None;
            }
            if is64 { u64at(base + vaddr_at) } else { u32at(base + vaddr_at).map(u64::from) }
        })
        .min()
}

/// The compressed-item ceiling. **200 KiB, and it is the COMPRESSED figure** — the 1 MiB number
/// that gets quoted is the decompressed one, and budgeting against it means an interesting crash
/// is the one that gets refused.
#[allow(dead_code)] // no compression on this path — `queue::MAX_RECORD` bounds an item long
// before this would, and nothing gzips a Sentry item today. Kept because the number is the
// COMPRESSED ceiling and the 1 MiB figure everyone quotes is the decompressed one, which is the
// mistake this constant exists to have already made once.
pub(crate) const MAX_COMPRESSED: usize = 200 * 1024;

/// Frame one item into an envelope: an envelope header line, an item header line, then the payload.
///
/// Newline-delimited, and the item header's `length` is the payload's byte length — the field this
/// function exists to get right, since a wrong one makes the receiver parse the next line as
/// payload and reject the whole envelope with a message about neither.
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

    // ---- the ELF the app reads about ITSELF ---------------------------------------------------

    /// A minimal little-endian ELF32 `ET_EXEC` image: header, `n` program headers, and whatever
    /// note bytes a caller wants appended. Hand-built rather than a checked-in fixture so every
    /// offset this parser reads is stated once, here, where a wrong one is visible.
    fn elf32(phdrs: &[[u32; 8]], notes: &[u8], note_off: u32) -> Vec<u8> {
        const EHDR: usize = 52;
        const PHENT: usize = 32;
        let mut v = vec![0u8; EHDR];
        v[..4].copy_from_slice(b"\x7fELF");
        v[4] = 1; // ELFCLASS32
        v[5] = 1; // ELFDATA2LSB
        v[6] = 1;
        v[16..18].copy_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        v[18..20].copy_from_slice(&40u16.to_le_bytes()); // EM_ARM
        v[28..32].copy_from_slice(&(EHDR as u32).to_le_bytes()); // e_phoff
        v[40..42].copy_from_slice(&(EHDR as u16).to_le_bytes()); // e_ehsize
        v[42..44].copy_from_slice(&(PHENT as u16).to_le_bytes()); // e_phentsize
        v[44..46].copy_from_slice(&(phdrs.len() as u16).to_le_bytes()); // e_phnum
        for ph in phdrs {
            for w in ph {
                v.extend_from_slice(&w.to_le_bytes());
            }
        }
        v.resize(note_off.max(v.len() as u32) as usize, 0);
        v.extend_from_slice(notes);
        v.resize(v.len().max(64), 0); // the parser refuses anything shorter than an ELF64 header
        v
    }

    /// One ELF note: `[namesz][descsz][type]`, then the name and the descriptor, **each padded to
    /// four bytes**. The padding is the part a hand-rolled walker forgets, and forgetting it reads
    /// correctly on every object anyone would test with — `"GNU\0"` is exactly four bytes — and
    /// wrongly on the one whose first note has a name that is not.
    fn note(name: &[u8], kind: u32, desc: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&(name.len() as u32).to_le_bytes());
        v.extend_from_slice(&(desc.len() as u32).to_le_bytes());
        v.extend_from_slice(&kind.to_le_bytes());
        v.extend_from_slice(name);
        v.resize(v.len().next_multiple_of(4), 0);
        v.extend_from_slice(desc);
        v.resize(v.len().next_multiple_of(4), 0);
        v
    }

    /// **The image base is the LOWEST `PT_LOAD`, not the first one written down.** A link that
    /// emits its segments out of order, or adds one below the text, would otherwise move the base
    /// and every frame with it — and the failure is `missing_symbol` with no error, per the table
    /// above.
    #[test]
    fn the_image_base_is_the_lowest_load_segment() {
        //          p_type      offset  vaddr     paddr  filesz  memsz  flags  align
        let data = [1u32, 0x8000, 0x30000, 0, 0x100, 0x100, 6, 0x1000];
        let text = [1u32, 0x0000, 0x10000, 0, 0x8000, 0x8000, 5, 0x1000];
        let phdr = [6u32, 0x34, 0x10034, 0, 0x40, 0x40, 4, 4];
        // Deliberately NOT in address order, and with a non-LOAD segment first.
        let b = elf32(&[phdr, data, text], &[], 0);
        assert_eq!(lowest_load_vaddr(&b), Some(0x10000));
        assert_eq!(image_addr_of(&b), "0x10000", "and it agrees with the measured constant");
    }

    /// Everything this cannot answer honestly answers `None`, and the caller then falls back to the
    /// measured constant. A wrong non-zero base and a zero base fail identically and invisibly, so
    /// "I could not tell" must never resolve to a guess.
    #[test]
    fn anything_unanswerable_is_refused_rather_than_guessed() {
        let text = [1u32, 0, 0x10000, 0, 0x8000, 0x8000, 5, 0x1000];
        let mut dynamic = elf32(&[text], &[], 0);
        dynamic[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN: vaddr is not the load address
        assert_eq!(lowest_load_vaddr(&dynamic), None);

        let mut be = elf32(&[text], &[], 0);
        be[5] = 2; // big-endian: every field below is read little-endian
        assert_eq!(lowest_load_vaddr(&be), None);

        assert_eq!(lowest_load_vaddr(b"not an elf at all, not remotely, no"), None);
        assert_eq!(lowest_load_vaddr(&elf32(&[], &[], 0)), None, "no PT_LOAD");
        // A header whose `e_phoff` points past the end of what was read. Not a corruption case so
        // much as the ordinary bound on `read_head`: this parser sees the first 64 KiB, and an
        // unreadable table must be "I do not know" rather than an entry read out of whatever bytes
        // happened to be at that offset.
        let mut far = elf32(&[text], &[], 0);
        far[28..32].copy_from_slice(&0x10_0000u32.to_le_bytes());
        assert_eq!(lowest_load_vaddr(&far), None);
    }

    /// **The build id is found past a note that precedes it**, which is the padding case above. A
    /// walker that advances by `namesz` instead of `align4(namesz)` finds the GNU note only when it
    /// happens to be first.
    #[test]
    fn the_build_id_note_is_found_behind_another_note() {
        let id: Vec<u8> = (0u8..20).collect();
        let mut notes = note(b"Linux\0", 0x100, &[1, 2, 3]); // namesz 6 -> two bytes of padding
        notes.extend_from_slice(&note(b"GNU\0", 3, &id));
        let off = 52 + 32 * 2; // after two program headers
        let seg = [4u32, off, 0, 0, notes.len() as u32, 0, 4, 4]; // PT_NOTE
        let text = [1u32, 0, 0x10000, 0, 0x8000, 0x8000, 5, 0x1000];
        let b = elf32(&[seg, text], &notes, off);
        assert_eq!(
            gnu_build_id(&b).as_deref(),
            Some("000102030405060708090a0b0c0d0e0f10111213")
        );
    }

    /// **Sentry's ELF `debug_id` byte-swaps the first three UUID fields.** `symbolic` builds it
    /// with `Uuid::from_slice_le`, so emitting the build id's bytes in file order produces a
    /// perfectly well-formed UUID that pairs with nothing — and the symptom is a frame that comes
    /// back unsymbolicated with no error attached.
    #[test]
    fn the_debug_id_reads_the_build_id_as_a_little_endian_uuid() {
        let id = "000102030405060708090a0b0c0d0e0f10111213"; // 20 bytes, sha1
        assert_eq!(debug_id(id).as_deref(), Some("03020100-0504-0706-0809-0a0b0c0d0e0f"));
        // The trailing four bytes of a sha1 id are not part of the UUID at all.
        assert_eq!(debug_id(id), debug_id(&id[..32]));
        assert_eq!(debug_id("0011"), None, "shorter than 16 bytes is not a debug id");
    }
}
