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

/// The mode of the event log and its two siblings (`plxnative-crash.log`, `plxnative-stderr.log`,
/// which `src/main.c`'s `open_fd_log` opens at the same mode): owner read/write, the app's own
/// group (gid 5000 on the television) read, nothing for anyone else.
///
/// **Group read, not 0600, so a Developer Mode user can fetch it.** A television that is not
/// rooted gives its owner one tool, webOS Dev Manager, and it reads files as an unprivileged user
/// that is in the app's group but is not the app's uid; at 0600 the logs were unreadable to the one
/// person who needs them. `plxnative-diag.log` has always shipped 0640 for the same reason.
///
/// **Why that is acceptable where 0644 in the shared, mode-1777 `/tmp` was not** (the 2026-08-29
/// closed entry in `docs/distribution.md`): "other" still gets nothing, and the content is no
/// longer what it was — [`log`] passes every line through [`scrub::scrub_local`] before the write.
/// What stays open is that another native app in gid 5000 on the same television can read these
/// files; `docs/distribution.md` §6.9 states that residual risk and the audit behind it.
const LOG_MODE: u32 = 0o640;

/// Open the event log for append at [`LOG_MODE`], refusing anything but a regular file this process
/// owns (`O_NOFOLLOW` rejects a symlink planted in the shared `/tmp`). The mode is set with
/// `fchmod` rather than trusted to `open(2)`, so it does not depend on the umask and an append
/// target that survived from a 0600 release is corrected on its first write.
fn open_log_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(LOG_MODE)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "unsafe log sink",
        ));
    }
    if meta.permissions().mode() & 0o777 != LOG_MODE {
        file.set_permissions(std::fs::Permissions::from_mode(LOG_MODE))?;
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
    if let Ok(mut f) = open_log_append(&p) {
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
mod log_sink_tests {
    use super::{open_log_append, write_log_line};
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
        assert!(open_log_append(&sink).is_err());
        assert_eq!(std::fs::read(&victim).unwrap(), b"unchanged");
        let _ = std::fs::remove_file(&sink);

        std::fs::write(&sink, b"").unwrap();
        std::fs::set_permissions(&sink, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut file = open_log_append(&sink).unwrap();
        file.write_all(b"safe").unwrap();
        assert_eq!(
            std::fs::metadata(&sink).unwrap().permissions().mode() & 0o777,
            0o640,
            "an append target left at 0644 is corrected to 0640"
        );

        let _ = std::fs::remove_file(sink);
        let _ = std::fs::remove_file(victim);
        let _ = std::fs::remove_dir(dir);
    }

    /// A umask is PROCESS-wide, so the body runs in a CHILD copy of the test binary (selected by
    /// `CHILD_ENV`) whose umask is its own — the technique, and the reason, of
    /// `storage::diagnostics`' `publication_is_0640_even_under_a_restrictive_umask`: setting a
    /// restrictive umask here would silently change the mode of every file another test creates
    /// during the window.
    ///
    /// The contract is the one `plxnative-diag.log` already ships: **0640** whatever the creating
    /// process's umask says — group read, so a Dev Mode user (whose only tool is webOS Dev Manager,
    /// reading as an unprivileged user in the app's group) can download the file, and nothing at
    /// all for anyone outside that group. Three starting states, because each reaches the mode by
    /// a different route: a fresh create (the umask masks `open(2)`'s mode, so only the explicit
    /// chmod can give 0640), a target left at 0600 by the previous release, and one left at 0666.
    #[test]
    fn the_event_log_is_0640_even_under_a_restrictive_umask() {
        use std::os::unix::fs::MetadataExt;
        const CHILD_ENV: &str = "PLX_EVENTLOG_UMASK_CHILD";
        if std::env::var_os(CHILD_ENV).is_none() {
            // libtest names a test without its crate: `eventlog::log_sink_tests::…`.
            let name = format!(
                "{}::the_event_log_is_0640_even_under_a_restrictive_umask",
                module_path!().split_once("::").unwrap().1
            );
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--test-threads=1"])
                .env(CHILD_ENV, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "the child run failed or ran nothing:\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let dir = std::env::temp_dir().join(format!("plx-rust-log-umask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        // Only ever reached in the child process: this changes ITS umask, never the suite's.
        for restrictive in [0o077, 0o777] {
            unsafe {
                libc::umask(restrictive);
            }
            let fresh = dir.join(format!("fresh-{restrictive:o}"));
            drop(open_log_append(&fresh).unwrap());
            let meta = std::fs::metadata(&fresh).unwrap();
            assert_eq!(meta.mode() & 0o7777, 0o640, "fresh create under umask {restrictive:o}");
            // No gid assertion, on purpose: the group is the kernel's choice (the process's egid on
            // Linux, where /tmp is not setgid, so 5000 on the television; the DIRECTORY's group on
            // a BSD/macOS host) and nothing here chowns. The mode is the contract.
        }
        for (name, previous) in [("from-0600", 0o600), ("from-0666", 0o666)] {
            let path = dir.join(name);
            std::fs::write(&path, b"kept").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(previous)).unwrap();
            let mut file = open_log_append(&path).unwrap();
            file.write_all(b" and appended").unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().mode() & 0o7777,
                0o640,
                "pre-existing {name} target"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                b"kept and appended",
                "the existing contents are appended to, never truncated"
            );
        }
        // Group read, never world: no `other` bit on any outcome above.
        for entry in std::fs::read_dir(&dir).unwrap() {
            let mode = entry.unwrap().metadata().unwrap().mode();
            assert_eq!(mode & 0o007, 0, "no other-class permission bit, got {mode:o}");
        }
        // The refusals are unchanged: only a regular file is ever a sink.
        assert!(
            open_log_append(std::path::Path::new("/dev/null")).is_err(),
            "a device node is not a log sink"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
