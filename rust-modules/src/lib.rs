//! PlxNative — an unofficial native Plex client for LG webOS.
//! Copyright © 2026 Gleb Linnik. Licensed under GPL-3.0-or-later; see LICENSE at the repository
//! root, and THIRD-PARTY-NOTICES.md for the components this links or redistributes.
//! Not affiliated with, endorsed by, or sponsored by Plex GmbH or LG Electronics.
//!
//! plxnative-modules — the Rust app core, built as a staticlib and linked into the C
//! boot shim. The crate's C surface is tiny: C calls `plex_run` (app.rs), writes the fallback
//! image marker through `plx_crash_write_image_marker`, re-enters the native-crash spool through
//! `plx_sentry_spool_external`, and forwards the two Starfish callbacks (`sf_on_event`/
//! `acb_on_event`, player/mod.rs). Everything else is Rust-internal (the per-module `repr(C)`
//! shapes are migration legacy, not ABI).
mod abr; // client-managed fixed-session HLS controller: estimate, propose, prime, then commit
mod app; // plex_run — the Rust app core / event loop (the entry inverted from main.c)
mod aq;
mod auth; // plex.tv login/boot flow controller (PIN/QR → discovery → who's-watching → install)
mod browse; // Library browse: per-section paged catalog (sparse store + off-thread page fetches)
mod capture; // dev live UI capture stream: own-GLES-frame grab → MPEG1/TS or JPEG → TCP (UI plane only)
mod cbuf; // fixed NUL-terminated C-string buffer read/write (shared by pms/route/posters)
mod coldstart; // retires old last-page bookmarks; authenticated cold boots now stay on Home
mod curlio; // the HTTPS media plane: a remote file pulled by byte range over libcurl-multi (stream.rs is the plaintext-socket twin)
mod dev; // the /tmp/plxnative-* trigger surface, behind one `devtriggers` feature — read it before adding a trigger
mod devcaps; // what this SoC decodes — the TV's own codec table, read once at boot (the capability profile + direct-play gate derive from it)
#[macro_use]
mod diag; // typed usage schema plus log/lab scrub, ring and zlib; native crashes have a separate allowlist
mod dynlib; // dlopen-by-SONAME-candidate: the libraries whose major moves between webOS releases
mod egl; // boot-time EGL capability probe (extensions, swap behaviour, buffer age) — diagnostic only
mod ff; // THE demuxer — the FFmpeg 9.0 this app BUNDLES and pins (majors 63/63/61), dlopen'd by absolute path beside the binary, never the television's
mod focusprobe; // dev: one diffable line naming everything app.rs's key ladder can move, logged when it changes
mod fontcov; // which codepoints a font file can draw, read from its cmap — text.rs's fallback chain, and the host gate that stops tofu shipping
mod gfx;
#[cfg(feature = "devtriggers")]
mod gpu_timer; // async EXT_disjoint_timer_query timing; no glFinish on the timing path
mod hls; // strict parser/auth/timeline for the measured one-variant PMS HLS shape
mod http; // the ONE door out of the control plane: dispatch a Plex REST request on its origin's scheme (stream.rs for http, net.rs/libcurl for https)
#[cfg(feature = "devtriggers")]
mod hwcnt; // direct userspace Mali r12p0 vinstr reader for the phase profiler
mod img;
mod imgcache; // a small on-disk image cache — today for profile avatars only, so the picker has faces offline
mod keymanager; // public LS2 key stores: keymanager3, legacy Palm service, or unavailable
mod lab; // Cloud Lab bridge: pinned diagnostic uploads + optional outbound command long-poll
mod metadata; // item detail data layer (detail page): full metadata + seasons/episodes + cast + related
mod net; // HTTPS client over the TV's libcurl (plex.tv account/login calls — stream.rs can't do TLS/DNS)
mod paths; // where the app's own files live — /proc/self/exe, not a hardcoded install prefix
mod person; // person/actor page data layer: the header handed in by the cast row + /library/people/{id}/media
mod player; // buffer-feed video engine (was playback.c) — step 5
mod plex; // typed Plex API layer (rust-modules/src/plex/) — one method per PMS operation (the live READ layer; playback ops still in route.rs)
mod pms;
// Pure RELEASE_LINE-parsing helpers, `include!`d verbatim by build.rs so `cargo test --lib`
// actually runs their unit tests (see the module for why). Nothing in the app itself calls
// them at runtime — the version rule they implement is applied once, at compile time, by
// build.rs — so they exist in THIS crate only for the test build; `#[cfg(test)]` here, not on
// the functions themselves, because build.rs's own separate compilation is never built with
// `--test` and needs them unconditionally.
#[cfg(test)]
mod release_line;
mod remote; // dev/testing remote-control channel: a FIFO the loop drains into synthetic SDL keys
mod screens; // the application's OWNED screens (restructure phase 5b): the Settings family on the dispatcher
mod route; // play_movie route selection (direct-play vs transcode) — step 3
mod search; // Search data layer: /hubs/search fanned out across every source, merged into typed shelves
mod sha256; // SHA-256 / HMAC / PBKDF2, hand-written: the offline PIN verifier's hash (no crypto dependency)
#[cfg(feature = "hostsim")]
mod shot; // simulator screenshots: read the frame back and write a PNG (see the module doc)
mod stores; // stores as machines (restructure phase 4): one command vocabulary + one step per data store
mod stream;
mod surface; // what we are actually drawing into — drawable vs the 1920x1080 logical canvas
mod svg; // runtime SVG rasterizer FFI (src/svg.c / nanosvg) — vector icon assets
mod system;
mod task; // the one spawn: a refused thread is a return value, not a panic that kills the app
mod telemetry; // the opt-in crash + usage channels: consent, the spool, the worker, the two wire formats
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
    //!
    //! **The lock also RECORDS who holds it, and that half is what makes the rule enforceable.**
    //! A mutex nobody is obliged to take is a convention, and a convention that is broken in one
    //! test out of two thousand does not fail — it hands some *other* module's test a wiped store
    //! at a rate low enough to read as flakiness. That is exactly how
    //! `app::chrome`'s `four_libraries_on_two_servers_publish_two_type_destinations` failed: its
    //! own `browse::reset()` + `seed_two_source_table_for_test()` are adjacent statements, so the
    //! table could only have been emptied by another thread, and the only thread that can run
    //! beside a lock holder is one that never took the lock.
    //!
    //! So [`serial`] publishes the holding thread in [`OWNER`], and [`assert_held`] — called from
    //! every crate-global mutator a test can reach — turns "somebody wrote this without the lock"
    //! from an intermittent failure in a bystander into a deterministic panic in the culprit,
    //! naming the culprit. Add the call to any new global store; do not add a retry anywhere.
    //!
    //! **This SUPERSEDES the static allowlist approach spec §14/§15.2 used earlier in the
    //! restructure (`ci/allow/statics-migration.txt` and friends) for the stores specifically.**
    //! That DoD criterion scopes a bare `static mut` OUT of `screens`/`ui` engine code — it asks
    //! "does a SCREEN still own process-wide state a second instance would corrupt", and an
    //! allowlisted exception there is a debt with a name and a phase number. A store's data module
    //! (`browse`, `pms`, `metadata`, `search`, `person`, `viewstate`) is a different question
    //! entirely: it keeps process-wide statics **by design** — `docs/stores-as-machines.md` §1 is
    //! explicit that this is "the same shape" on purpose, one Home catalog and one Library table
    //! for the whole process, not a per-screen instance — so an allowlist entry for a store's
    //! `static` would be a permanent fixture wearing a temporary label. What a store genuinely
    //! owes is not "stop being global" but "a test that mutates you holds the one lock every other
    //! test mutating you also holds", which is exactly what [`assert_held`] enforces at every
    //! mutator a test can reach, rather than at the `static` declaration site. Put differently: the
    //! allowlist answers "is this global allowed to exist", the assertion answers "was this write to
    //! it safe" — a store answers yes to the first question unconditionally, so only the second one
    //! applies to it.
    //!
    //! **Phase 12 / D5 closed the coverage this claim depends on** (2026-09-10): every store's own
    //! `apply`/`run` funnel now asserts (`stores::browse::run`, `stores::hubs::run`,
    //! `stores::metadata::run`, `stores::person::run`, `stores::search::run`,
    //! `stores::viewstate::run`), as does every `_for_test` seed/installer reachable from a test —
    //! `browse/mod.rs` (`seed_sources_for_test`, `set_pinned_for_test`, `land_pin_for_test`,
    //! `seed_pins_for_test`, `seed_two_source_table_for_test`, `seed_registered_table_for_test`,
    //! `seed_items_for_test`, `seed_letter_counts_for_test`, `seed_query_choices_for_test`,
    //! `append_section_for_test`, plus `reset`/`append_sections` themselves), `metadata.rs`
    //! (`install_for_test`, `set_current_for_test`, `begin_detail_for_test`,
    //! `land_detail_for_test`), `search.rs` (`publish_shelves_for_test`,
    //! `debounce_elapsed_for_test`), `person.rs` (`install_credits_for_test`, `install_for_test`,
    //! `install_source_for_test`) and `pms.rs`'s own eleven sites — and so does every entry point of
    //! the server registry a test can reach: `plex::servers::register_with_client_id` (and the
    //! `register_lazy` seam it and its sibling test constructors share), `revoke_all`,
    //! `set_current` and `reset_for_test`. A three-run `make check` plus a ten-run
    //! `--test-threads 16` stress at this coverage level turned up no test that had been writing a
    //! store without the lock — which says the existing call sites were already disciplined, not
    //! that the assertion was unnecessary: it is what keeps that true as the suite grows, instead of
    //! relying on every future PR remembering the convention on its own.
    use std::sync::atomic::{AtomicU64, Ordering};

    static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());
    /// The [`ticket`] of the thread currently inside a [`serial`] guard, or [`NOBODY`].
    static OWNER: AtomicU64 = AtomicU64::new(NOBODY);
    const NOBODY: u64 = 0;

    /// This thread's identity as a plain integer. `ThreadId` has no stable numeric projection on
    /// the pinned toolchain, and the value only has to be unique and comparable, so it is minted
    /// from a counter on first use and kept for the thread's life.
    fn ticket() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        thread_local! {
            static MINE: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
        }
        MINE.with(|mine| *mine)
    }

    /// The guard [`serial`] hands back. Its own `Drop` runs BEFORE its field's, so the owner is
    /// cleared while the mutex is still held — no window in which a second thread has the lock and
    /// this one still claims it.
    pub(crate) struct Serial(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Drop for Serial {
        fn drop(&mut self) {
            OWNER.store(NOBODY, Ordering::SeqCst);
        }
    }

    pub(crate) fn serial() -> Serial {
        let guard = GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        OWNER.store(ticket(), Ordering::SeqCst);
        Serial(guard)
    }

    thread_local! {
        /// Set by [`adopt_current_thread`]; see its doc.
        static ADOPTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    /// Treat the CALLING thread as if it too held the [`serial`] guard — for a test that
    /// deliberately spawns a WORKER thread to race a real *production* lock (not this one)
    /// against the thread that took `serial()`, e.g. proving that `plex::servers`' own `WRITE`
    /// mutex serializes a repoint against an in-flight commit. That worker still touches the same
    /// crate globals `serial()` exists to protect, so [`assert_held`] must not wave it through for
    /// free — but it is not a bystander test either, so the ordinary per-thread [`held`] check
    /// (correctly) refuses it.
    ///
    /// Call this as the FIRST statement inside the spawned closure, and only while the spawning
    /// thread's [`Serial`] guard is still alive for the rest of the adopted thread's work — nothing
    /// here clears the flag early or checks that the spawner is still holding it, so an adopted
    /// thread that outlives the guard's `Drop` (e.g. a leaked worker still running after the test
    /// function has returned) would silently keep reporting `held() == true` with nobody actually
    /// holding [`GLOBALS`], which is a quieter failure than the one this whole module exists to
    /// catch. A thread that calls this NEVER needs to call [`serial`] itself — doing so would
    /// deadlock against the spawning thread's still-held [`GLOBALS`] lock.
    pub(crate) fn adopt_current_thread() {
        ADOPTED.with(|a| a.set(true));
    }

    /// Does THIS thread hold the lock (or was it explicitly [`adopt`](adopt_current_thread)ed by
    /// one that does)? Not "is it held" — an UNADOPTED foreign holder is the failure.
    pub(crate) fn held() -> bool {
        OWNER.load(Ordering::SeqCst) == ticket() || ADOPTED.with(|a| a.get())
    }

    /// Refuse a write to a crate global from a thread that does not hold the lock.
    ///
    /// `what` names the store, because the panic is read by whoever wrote the offending test and
    /// the useful half is "which global" — the thread name libtest prints already says which test.
    #[track_caller]
    pub(crate) fn assert_held(what: &str) {
        assert!(
            held(),
            "{what} was written without crate::testlock::serial(). It is a process global: \
             without the lock this write lands in the middle of some other module's test and \
             fails THAT one, intermittently. Take the guard for the whole test body (see \
             lib.rs::testlock)."
        );
    }

    #[cfg(test)]
    mod tests {
        /// **Ownership is per THREAD, which is the whole discriminating property.**
        ///
        /// "Is the mutex locked" cannot answer this question: while a test holds the guard the
        /// mutex is locked for everybody, so a bystander thread asking that would be told yes and
        /// would go on to write the global it had no right to. The lock records WHO, and a second
        /// thread — the shape every one of these bugs has taken — is told no.
        #[test]
        fn a_second_thread_is_never_mistaken_for_the_holder() {
            assert!(!super::held(), "a thread that never took the guard holds nothing");
            let guard = super::serial();
            assert!(super::held());
            // …and while THIS thread holds it, another one still does not.
            assert!(
                !std::thread::spawn(super::held).join().expect("probe thread"),
                "a foreign thread must not inherit this one's claim"
            );
            drop(guard);
            assert!(!super::held(), "the claim ends with the guard, not after it");
        }
    }
}
mod text;
mod textinput; // the TV's own on-screen keyboard, via plain SDL_StartTextInput (see the module doc)
mod ui;
mod webos; // which webOS this set is — nyx's os_info.json, read once at boot (release + codename)

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
pub(crate) fn redact_tokens(m: &str) -> std::borrow::Cow<'_, str> {
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

pub(crate) fn log(m: &str) {
    use std::io::Write;
    // Through the instance root, not a literal: several host simulators run at once, and one
    // shared event log would interleave their lines into something no run can be graded from.
    // On the television the root is `/tmp`, so this is byte-for-byte the path it always was —
    // `make run`, `tests/run.py` and every skill recipe still read the same file.
    let p = events_log();
    // The FULL local pass, not just the token backstop: identities, hostnames and bare
    // addresses are rewritten before anything reaches the disk. `scrub_local` never DROPS a line —
    // see its doc for why the network exit may and this one may not.
    let line = diag::scrub::scrub_local(m);
    // The lab ring taps the log HERE, one call below the redaction, so it is by construction a
    // strict subset of the file every other tool reads and inherits the credential backstop above.
    // A compile-time no-op without the `lab-diagnostics` feature — see `crate::lab`.
    lab::record(&line);
    if let Ok(mut f) = open_private_log_append(&p) {
        let _ = writeln!(f, "{line}");
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
#[cfg(feature = "hostsim")]
pub use app::synthetic_home_initial;

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
    use super::open_private_log_append;
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
}
