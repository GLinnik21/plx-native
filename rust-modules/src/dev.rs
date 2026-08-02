//! The `/tmp` developer-trigger surface, behind one door.
//!
//! This app is driven headlessly by ~44 files under `/tmp/plxnative-*`: which screen to boot to,
//! which item to play, which URL to stream, whether to auto-press OK, which PMS token to use.
//! That is how `tests/run.py` and every capture scene work, and it is not going away.
//!
//! It must not exist in a public build. `/tmp` is the SHARED system `/tmp` in the production jail
//! too (mode 1777, both jail profiles), so on an ordinary user's TV every one of those files is a
//! behaviour switch any co-resident process can throw. Two are outright takeovers:
//! `plxnative-token` beats the signed-in session (`app.rs`'s boot gate), and `plxnative-url`
//! replaces the stream the player feeds.
//!
//! So every read goes through here, and here is `#[cfg]`-gated on the `devtriggers` feature. In a
//! `--no-default-features` build [`flag`] is `false` and [`read`] is `None` at COMPILE time, the
//! branches behind them fold away, and the binary opens nothing under `/tmp` but its own logs.
//!
//! Two rules for anything added later:
//!
//! 1. **Never open a `/tmp` path directly.** The grep that audits this (`/tmp/plxnative-` outside
//!    this module and the four log sinks) is the only thing keeping the property true.
//! 2. **A gate is not always a path.** `any_trigger_present` scans the whole directory and names
//!    no file at all — it was the one surface a literal-replacement sweep would have missed, and
//!    it silently changes which screen the app boots to. Structural surfaces that take no name
//!    (the capture listener's `INADDR_ANY` socket, the remote FIFO's `mkfifo`) are gated at their
//!    call sites in `app.rs` for the same reason.
//!
//! The four LOG sinks are deliberately NOT here and stay in every build: they are creates, not
//! reads, they are how on-device crash triage works at all, and writing them is not a way for
//! another process to steer this one.

/// Files that are pure diagnostics rather than automation — see [`any_trigger_present`].
///
/// Every log this app writes belongs here, not just its trigger. `plxnative-anim` was listed and
/// `plxnative-anim.log` was not, while arming the overlay creates exactly that file and nothing
/// ever removes it (`make run` clears only the event log; `tests/run.py` spares every `*.log` by
/// design) — so a single historical anim session skipped the who's-watching picker on every later
/// boot, interactive ones included.
// `test` as well as the feature: `any_trigger_present` is the only caller and it is cfg'd out of a
// release build, but the test below asserts this list's contents and runs with default features.
#[cfg(any(feature = "devtriggers", test))]
const DIAG: [&str; 9] = [
    "plxnative-events.log",
    "plxnative-stderr.log",
    "plxnative-crash.log",
    "plxnative-anim.log",
    "plxnative-profile",
    "plxnative-anim",
    "plxnative-remote",
    "plxnative-capture",
    "plxnative-noidle",
];

/// Is the trigger `name` (bare, without the `plxnative-` prefix) present?
#[cfg(feature = "devtriggers")]
pub(crate) fn flag(name: &str) -> bool {
    std::path::Path::new(&path(name)).exists()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn flag(_name: &str) -> bool {
    false
}

/// The trigger's CONTENT, trimmed. `Some("")` for a trigger armed as an empty file — several
/// distinguish empty (take the default) from a value (`autoseek`, `library`, `marker`), so an
/// empty file must not read the same as an absent one.
#[cfg(feature = "devtriggers")]
pub(crate) fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(path(name)).ok().map(|s| s.trim().to_string())
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read(_name: &str) -> Option<String> {
    None
}

/// A raw dev payload by ABSOLUTE path — only `/tmp/sample.h264` and `/tmp/sample.h265`, which
/// predate the `plxnative-` prefix and feed the player a local Annex-B sample instead of a stream.
#[cfg(feature = "devtriggers")]
pub(crate) fn read_bytes_at(abs: &str) -> Option<Vec<u8>> {
    std::fs::read(abs).ok()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read_bytes_at(_abs: &str) -> Option<Vec<u8>> {
    None
}

/// Is ANY non-diagnostic trigger armed? Used to skip the boot who's-watching picker, so that a
/// headless run lands on a deterministic Home.
///
/// This is the surface with no path literal: it `read_dir`s the shared `/tmp` and matches by
/// prefix, so in a release build it would still have run — and still have changed the boot
/// screen from a squatted file — after every named read had been compiled out.
#[cfg(feature = "devtriggers")]
pub(crate) fn any_trigger_present() -> bool {
    std::fs::read_dir("/tmp")
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("plxnative-") && !DIAG.contains(&n.as_str())
            })
        })
        .unwrap_or(false)
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn any_trigger_present() -> bool {
    false
}

/// `true` when this build reads `/tmp` at all — for the one boot log line that says so, and for
/// call sites gating a whole subsystem (the capture listener, the remote FIFO) rather than a read.
pub(crate) const ENABLED: bool = cfg!(feature = "devtriggers");

#[cfg(feature = "devtriggers")]
fn path(name: &str) -> String {
    format!("/tmp/plxnative-{name}")
}

#[cfg(test)]
mod tests {
    /// The DIAG list must name every log the app writes, or that log permanently suppresses the
    /// boot picker. This asserts the property against the paths the code actually opens rather
    /// than against a copy of the list, so adding a fifth log sink without listing it fails here.
    #[test]
    fn diag_names_every_log_this_app_writes() {
        for log in ["plxnative-events.log", "plxnative-stderr.log", "plxnative-crash.log", "plxnative-anim.log"] {
            assert!(super::DIAG.contains(&log), "{log} is written by this app but absent from DIAG — it would suppress the boot picker forever");
        }
    }

    /// An empty trigger file and an absent one mean different things to several call sites
    /// (`autoseek` empty = one seek to 140s; `navosc` empty = Home <-> the first library section).
    #[test]
    fn empty_trigger_is_some_not_none() {
        if !super::ENABLED {
            return; // a release build reads nothing; nothing to distinguish
        }
        // `/tmp` by literal, NOT `env::temp_dir()`: on the dev Mac that is `$TMPDIR`, a per-user
        // `/var/folders/…/T/` path, while `path()` always builds `/tmp/plxnative-…` — so the write
        // and the read never met and this failed on the host for a reason having nothing to do
        // with the property under test.
        let p = std::path::Path::new("/tmp/plxnative-devtest-empty");
        std::fs::write(p, "").unwrap();
        let got = super::read("devtest-empty");
        let _ = std::fs::remove_file(p);
        assert_eq!(got.as_deref(), Some(""), "an empty trigger must not read as absent");
    }
}
