//! **The event log** — `/tmp/plxnative-events.log` on the television, the app's one support channel.
//!
//! [`log`] is the ONE sink every module writes through; [`redact_tokens`] and [`scrub`] are the
//! guards every line passes on the way, and [`ring`] is the lab's tap. It lives in the base layer
//! (docs/module-layers.md) and names nothing outside itself but `paths`, so any module may log
//! without naming the application, and the log closes no module cycle. It
//! used to be `log` in `lib.rs`, and reaching the lab ring through `lab` from there put every
//! caller above the whole app in the module graph.

/// The redaction pass every line takes before the file write ([`scrub::scrub_local`]), and the
/// stricter one a Cloud Lab upload takes ([`scrub::scrub`]). **Ungated**: its assertions must run in
/// the default `make check`. See the module's doc for why there are two exits and why only the
/// remote one may drop a line.
pub mod scrub;
/// The bounded in-memory record ring, tapped by [`log`] one call below the redaction. Behind the
/// lab feature: a build without it has nothing to put in a ring.
#[cfg(feature = "lab-diagnostics")]
pub mod ring;

/// Strip any PMS/plex.tv token from a line bound for the event log.
///
/// **This is a backstop, not the policy.** The policy is that no call site formats a URL into a log
/// line at all — but that policy was violated for months by one `-> {url}` in `route::retranscode`,
/// reached by an ordinary audio-track switch, and the app's whole support channel is "send us
/// `/tmp/plxnative-events.log`". So the class is closed HERE, where every line passes, rather than
/// at the call sites, where the next one is one `format!` away from re-opening it.
///
/// Matches the parameter name rather than the value: the token is a short unstructured alphanumeric
/// with no distinguishing shape, so it cannot be recognised on its own — but it only ever reaches a
/// string as `X-Plex-Token=…`, appended by the single choke point in `plex::client`. The value runs
/// to the next `&` or whitespace, i.e. the end of that query parameter.
///
/// Cheap by construction: the `find` is a no-op scan for the overwhelming majority of lines, and
/// the log is written a few times a second at most, never per frame.
pub fn redact_tokens(m: &str) -> std::borrow::Cow<'_, str> {
    const KEY: &str = "X-Plex-Token=";
    if !m.contains(KEY) {
        return std::borrow::Cow::Borrowed(m);
    }
    let mut out = String::with_capacity(m.len());
    let mut rest = m;
    while let Some(at) = rest.find(KEY) {
        out.push_str(&rest[..at + KEY.len()]);
        out.push_str("<redacted>");
        let after = &rest[at + KEY.len()..];
        // the value ends at the next query separator or any whitespace — whichever comes first
        let end = after
            .find(|c: char| c == '&' || c.is_whitespace())
            .unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// The event log's path. One definition, because three things open this file: `log` below,
/// the simulator binary (which truncates it at startup), and `src/main.c` on the television — and
/// the last of those cannot see this module, which is what [`crate::paths::ENV_STEERABLE`] guarantees.
pub fn events_log() -> std::path::PathBuf {
    crate::paths::in_runtime_dir(crate::paths::runtime_file::EVENTS)
}

fn open_private_log_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe log sink",
        ));
    }
    if meta.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

/// Append one complete record with one [`std::io::Write::write`] call. The event log has several
/// independently opened `O_APPEND` descriptors; formatting the line and newline separately lets
/// another thread append between them, gluing two otherwise valid records together.
fn write_log_line(writer: &mut impl std::io::Write, line: &str) -> std::io::Result<()> {
    let mut record = Vec::with_capacity(line.len() + 1);
    record.extend_from_slice(line.as_bytes());
    record.push(b'\n');
    match writer.write(&record)? {
        written if written == record.len() => Ok(()),
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::WriteZero,
            "partial event-log record",
        )),
    }
}

/// Append one line to the on-device event log (`/tmp/plxnative-events.log`) — the primary debugging
/// surface (`make run` fetches it). The ONE shared sink; modules bring it in as `use crate::eventlog::log;`.
///
/// Every line goes through [`redact_tokens`] first — see its doc for why the guard lives here.
pub fn log(m: &str) {
    // Through the instance root, not a literal: several host simulators run at once, and one
    // shared event log would interleave their lines into something no run can be graded from.
    // On the television the root is `/tmp`, so this is byte-for-byte the path it always was —
    // `make run`, `tests/run.py` and every skill recipe still read the same file.
    let p = events_log();
    // The FULL local pass, not just the token backstop: identities, hostnames and bare
    // addresses are rewritten before anything reaches the disk. `scrub_local` never DROPS a line —
    // see its doc for why the network exit may and this one may not.
    let line = crate::eventlog::scrub::scrub_local(m);
    // The lab ring taps the log HERE, one call below the redaction, so it is by construction a
    // strict subset of the file every other tool reads and inherits the credential backstop above.
    // Compiled out without the `lab-diagnostics` feature — see `lab`.
    #[cfg(feature = "lab-diagnostics")]
    crate::eventlog::ring::record(&line);
    if let Ok(mut f) = open_private_log_append(&p) {
        let _ = write_log_line(&mut f, &line);
    }
}

/// The log's credential backstop. These run on the pure function, so they need no filesystem.
#[cfg(test)]
mod redact_tests {
    use super::redact_tokens;

    /// The exact line that shipped: a transcode URL with the token appended last.
    #[test]
    fn a_token_at_the_end_of_a_url_does_not_survive() {
        let line = "retranscode rk=42 -> http://10.0.0.2:32400/video/:/transcode/universal/start.mkv?protocol=http&X-Plex-Token=aBcD1234xyzQ";
        let out = redact_tokens(line);
        assert!(!out.contains("aBcD1234xyzQ"), "token survived: {out}");
        assert!(out.contains("X-Plex-Token=<redacted>"));
        assert!(
            out.contains("start.mkv"),
            "the diagnostic half must survive"
        );
    }

    /// A token in the MIDDLE keeps the parameters after it — the redaction ends at `&`, so a line
    /// is not silently truncated from the token onward (which would hide the very fields that make
    /// the line worth logging).
    #[test]
    fn a_token_mid_url_ends_at_the_ampersand() {
        let out = redact_tokens("GET /x?X-Plex-Token=SECRET&audio=3&sub=1 ok");
        assert!(!out.contains("SECRET"));
        assert!(out.contains("audio=3") && out.contains("sub=1") && out.ends_with(" ok"));
    }

    /// More than one occurrence on one line (two URLs logged together).
    #[test]
    fn every_occurrence_is_scrubbed_not_just_the_first() {
        let out = redact_tokens("a=?X-Plex-Token=AAA b=?X-Plex-Token=BBB");
        assert!(!out.contains("AAA") && !out.contains("BBB"), "{out}");
        assert_eq!(out.matches("<redacted>").count(), 2);
    }

    /// A token at the very end of the string (no trailing separator) must not panic or be missed.
    #[test]
    fn a_token_at_end_of_line_is_scrubbed() {
        let out = redact_tokens("tail X-Plex-Token=ZZZ");
        assert_eq!(out, "tail X-Plex-Token=<redacted>");
    }

    /// The common case is untouched and allocation-free.
    #[test]
    fn an_ordinary_line_is_borrowed_unchanged() {
        let line = "feed v#12 reply=Ok";
        assert!(matches!(redact_tokens(line), std::borrow::Cow::Borrowed(_)));
        assert_eq!(redact_tokens(line), line);
    }

    /// Multi-byte content must not panic the slicing (the app logs remote tokens and item titles).
    #[test]
    fn multibyte_text_around_a_token_does_not_panic() {
        let out = redact_tokens("séance ☃ ?X-Plex-Token=Q1 — après");
        assert!(!out.contains("Q1"));
        assert!(out.contains("séance") && out.contains("après"));
    }
}

#[cfg(test)]
mod private_log_tests {
    use super::{open_private_log_append, write_log_line};
    use std::io::Write;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn a_symlink_cannot_redirect_the_rust_log_sink() {
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plx-rust-log-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        let victim = dir.join("victim");
        let sink = dir.join("sink");
        let _ = std::fs::remove_file(&sink);
        std::fs::write(&victim, b"unchanged").unwrap();
        symlink(&victim, &sink).unwrap();
        assert!(open_private_log_append(&sink).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        let _ = std::fs::remove_file(&sink);

        std::fs::write(&sink, b"").unwrap();
        std::fs::set_permissions(&sink, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut file = open_private_log_append(&sink).unwrap();
        file.write_all(b"safe").unwrap();
        assert_eq!(
            std::fs::metadata(&sink).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let _ = std::fs::remove_file(sink);
        let _ = std::fs::remove_file(victim);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn each_log_line_is_one_newline_terminated_write() {
        #[derive(Default)]
        struct Sink {
            calls: usize,
            bytes: Vec<u8>,
        }
        impl std::io::Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut sink = Sink::default();
        write_log_line(&mut sink, "install: id=com.beb.plxnative.debug").unwrap();
        assert_eq!(sink.calls, 1);
        assert_eq!(sink.bytes, b"install: id=com.beb.plxnative.debug\n");
    }
}
