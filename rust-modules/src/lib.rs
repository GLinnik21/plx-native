//! PlxNative — an unofficial native Plex client for LG webOS.
//! Copyright © 2026 Gleb Linnik. Licensed under the MIT Licence; see LICENSE at the repository
//! root, and THIRD-PARTY-NOTICES.md for the components this links or redistributes.
//! Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.
//!
//! plxnative-modules — the Rust app core, built as a staticlib and linked into the C
//! boot shim. The crate's C surface is tiny: C calls `plex_run` (app.rs) and forwards
//! the two starfish callbacks (`sf_on_event`/`acb_on_event`, player/mod.rs); everything
//! else is Rust-internal (the per-module `repr(C)` shapes are migration legacy, not ABI).
mod app; // plex_run — the Rust app core / event loop (the entry inverted from main.c)
mod aq;
mod cbuf; // fixed NUL-terminated C-string buffer read/write (shared by pms/route/posters)
mod auth; // plex.tv login/boot flow controller (PIN/QR → discovery → who's-watching → install)
mod browse; // Library browse: per-section paged catalog (sparse store + off-thread page fetches)
mod capture; // dev live UI capture stream: own-GLES-frame grab → MPEG1/TS or JPEG → TCP (UI plane only)
mod dev; // the /tmp/plxnative-* trigger surface, behind one `devtriggers` feature — read it before adding a trigger
mod devcaps; // what this SoC decodes — the TV's own codec table, read once at boot (the capability profile + direct-play gate derive from it)
#[macro_use]
mod dynlib; // dlopen-by-SONAME-candidate: the libraries whose major moves between webOS releases
mod ff; // FFmpeg (libavformat/libavcodec/libavutil) demuxer — the TV's own FFmpeg 3.3 via the stub-.so link
mod focusprobe; // dev: one diffable line naming everything app.rs's key ladder can move, logged when it changes
mod gfx;
mod img;
mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
mod person; // person/actor page data layer: the header handed in by the cast row + /library/people/{id}/media
mod player; // buffer-feed video engine (was playback.c) — step 5
mod plex; // typed Plex API layer (rust-modules/src/plex/) — one method per PMS operation (the live READ layer; playback ops still in route.rs)
mod paths; // where the app's own files live — /proc/self/exe, not a hardcoded install prefix
mod pms;
mod posters;
mod remote; // dev/testing remote-control channel: a FIFO the loop drains into synthetic SDL keys
mod route; // play_movie route selection (direct-play vs transcode) — step 3
mod search; // Search data layer: /hubs/search fanned out across every source, merged into typed shelves
mod stream;
mod surface; // what we are actually drawing into — drawable vs the 1920x1080 logical canvas
#[cfg(feature = "hostsim")]
mod shot; // simulator screenshots: read the frame back and write a PNG (see the module doc)
mod svg; // runtime SVG rasterizer FFI (src/svg.c / nanosvg) — vector icon assets
mod system;
mod task; // the one spawn: a refused thread is a return value, not a panic that kills the app
mod viewstate; // watched / unwatched / remove-from-deck: the PMS view-state WRITES, off the SDL thread

#[cfg(test)]
pub(crate) mod testlock {
    //! One lock for every test that touches a process-global.
    //!
    //! The app's async seams are process-wide by construction — `static mut CURRENT`, route's play
    //! mailbox, the player's SHARED block — so tests in DIFFERENT modules contend on the same
    //! state and `cargo test` threads them. A per-module mutex cannot see that: the season and
    //! detail mailboxes are two test functions in one file, but the season generation also moves
    //! under `pump_detail` (which calls `supersede_season`).
    //!
    //! Hold the guard for the whole test. Poison is stepped over so a failing test reports ITS
    //! assertion instead of dragging every later one down with a poison panic.
    static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn serial() -> std::sync::MutexGuard<'static, ()> {
        GLOBALS.lock().unwrap_or_else(|e| e.into_inner())
    }
}
mod text;
mod textinput; // the TV's own on-screen keyboard, via plain SDL_StartTextInput (see the module doc)
mod webos; // which webOS this set is — nyx's os_info.json, read once at boot (release + codename)
mod ui; // retui — retained UI framework; ui/home.rs now owns the home-screen C ABI

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
fn redact_tokens(m: &str) -> std::borrow::Cow<'_, str> {
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
        let end = after.find(|c: char| c == '&' || c.is_whitespace()).unwrap_or(after.len());
        rest = &after[end..];
    }
    out.push_str(rest);
    std::borrow::Cow::Owned(out)
}

/// Append one line to the on-device event log (`/tmp/plxnative-events.log`) — the primary debugging
/// surface (`make run` fetches it). The ONE shared sink; modules bring it in as `use crate::log;`.
///
/// Every line goes through [`redact_tokens`] first — see its doc for why the guard lives here.
/// The event log's path. One definition, because three things open this file: `log` below,
/// the simulator binary (which truncates it at startup), and `src/main.c` on the television — and
/// the last of those cannot see this module, which is what [`paths::ENV_STEERABLE`] guarantees.
fn events_log() -> std::path::PathBuf {
    paths::in_runtime_dir("plxnative-events.log")
}

pub(crate) fn log(m: &str) {
    use std::io::Write;
    // Through the instance root, not a literal: several host simulators run at once, and one
    // shared event log would interleave their lines into something no run can be graded from.
    // On the television the root is `/tmp`, so this is byte-for-byte the path it always was —
    // `make run`, `tests/run.py` and every skill recipe still read the same file.
    let p = events_log();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{}", redact_tokens(m));
    }
}

/// The instance root, for the simulator binary.
///
/// `src/bin/sim.rs` is a separate crate and cannot see `pub(crate)` items, but it must create the
/// directory and truncate the event log inside it before the app starts. Exposing the resolver
/// keeps ONE definition of where that is — a second `env::var` read in the binary would be a
/// second answer waiting to drift from this one.
#[cfg(feature = "hostsim")]
pub fn sim_runtime_dir() -> std::path::PathBuf {
    paths::runtime_dir().to_path_buf()
}

/// The event log's path, built by the ONE expression [`log`] uses.
///
/// `src/bin/sim.rs` truncates this file at startup. Spelling the name a second time over there
/// would mean a rename could leave the binary truncating a file the app never appends to — the
/// simulator's log would silently start non-empty, which is exactly the state `tests/run.py` dates
/// its first line from.
#[cfg(feature = "hostsim")]
pub fn sim_events_log() -> std::path::PathBuf {
    events_log()
}

/// Re-exported so the simulator binary calls the SAME entry the C shim calls, by name, with the
/// compiler checking the signature. It previously re-declared `plex_run` in its own `extern "C"`
/// block, which meant the one binary whose whole premise is "cannot drift from the shipped boot
/// path" was the one place a signature change would become a silent ABI mismatch instead of a
/// compile error.
#[cfg(feature = "hostsim")]
pub use app::plex_run;

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
        assert!(out.contains("start.mkv"), "the diagnostic half must survive");
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
