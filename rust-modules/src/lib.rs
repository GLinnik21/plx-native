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
mod checkpoint; // the transport-neutral "may I keep waiting?" seam every blocking media wait consults
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
mod spki; // a PEM certificate -> its CURLOPT_PINNEDPUBLICKEY string (sha256//<base64 of the SPKI hash>)
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
pub(crate) mod testnet {
    //! The one accept for a loopback test server whose listener is nonblocking.
    //!
    //! A fixture makes its listener nonblocking so the acceptor can poll a stop flag or a
    //! deadline. Darwin hands the accepted socket the listener's O_NONBLOCK; Linux does not. A
    //! reader that assumed blocking I/O then sees `WouldBlock` whenever a parallel suite accepts
    //! before the request line is buffered, and a writer can drop a body on a full send buffer —
    //! flakes that exist on the Mac only. Every such acceptor accepts through here, so the
    //! connection behaves as on Linux and read/write timeouts mean what they say.

    /// `listener.accept()`, with the accepted socket put back in blocking mode.
    pub(crate) fn accept(
        listener: &std::net::TcpListener,
    ) -> std::io::Result<(std::net::TcpStream, std::net::SocketAddr)> {
        let (socket, peer) = listener.accept()?;
        socket.set_nonblocking(false)?;
        Ok((socket, peer))
    }
}

#[cfg(test)]
pub(crate) mod testlock {
    //! One lock for every test that touches a process-global.
    //!
    //! Some remaining async seams are process-wide by construction — metadata's `static mut CURRENT`, route's play
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
    //! (`pms`, `metadata`, `search`) is a different question entirely: those remaining
    //! compatibility stores keep process-wide statics **by design** — `docs/stores-as-machines.md`
    //! §1 is explicit about that temporary shape. Browse, Person and ViewState have since moved to
    //! per-`Bridge` physical owners, and their owner gates reject a restored process global. For
    //! each remaining compatibility store, an allowlist entry would therefore be a permanent
    //! fixture wearing a temporary label. What such a store genuinely owes is not "stop being
    //! global" but "a test that mutates you holds the one lock every other
    //! test mutating you also holds", which is exactly what [`assert_held`] enforces at every
    //! mutator a test can reach, rather than at the `static` declaration site. Put differently: the
    //! allowlist answers "is this global allowed to exist", the assertion answers "was this write to
    //! it safe" — a store answers yes to the first question unconditionally, so only the second one
    //! applies to it.
    //!
    //! **Phase 12 / D5 closed the coverage this claim depends on** (2026-09-10): each remaining
    //! process-global store's `apply`/`run` funnel asserts (`stores::hubs::run`,
    //! `stores::metadata::run` and `stores::search::run`), as does every
    //! `_for_test` installer that touches shared state. Browse, Person and ViewState fixtures instead own
    //! explicit stores; Browse's helpers cover source, pin,
    //! table, item, letter and query seeds without selecting process Browse state. Helpers that
    //! also touch the shared session or server registry assert the same lock. The remaining global
    //! installers include `metadata.rs`
    //! (`install_for_test`, `set_current_for_test`, `begin_detail_for_test`,
    //! `land_detail_for_test`), `search.rs` (`publish_shelves_for_test`,
    //! `debounce_elapsed_for_test`) and `pms.rs`'s own eleven sites — and so does every entry point of
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
            crate::storage_worker::drain_for_test();
            OWNER.store(NOBODY, Ordering::SeqCst);
        }
    }

    /// Take the lock, or PANIC if this thread already holds it.
    ///
    /// The re-entrancy check is not a nicety. [`GLOBALS`] is a plain mutex, so a test that takes
    /// the guard and then calls a helper which takes it again — `screens::detail`'s `install()` is
    /// exactly such a helper, and two trailer-scrub tests did this on 2026-09-18 — does not fail.
    /// It *hangs*, holding the one lock the whole suite queues on, so every other serial test in
    /// the run wedges behind it at 0% CPU with no output and no failure to read. That cost an hour
    /// of wall clock and was indistinguishable from a slow build from the outside. A deadlock and
    /// a panic are the same bug; only one of them names itself.
    pub(crate) fn serial() -> Serial {
        assert!(
            !held(),
            "testlock::serial() taken twice on one thread — the second take would deadlock the \
             whole suite. Hold the guard a helper (e.g. detail's `install`) already returned \
             instead of taking a second one."
        );
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

        /// A second take on one thread must PANIC, not block.
        ///
        /// Graded on a spawned thread so the failure mode under test cannot take the suite's own
        /// lock down with it, and because that is the shape the bug has: a test takes the guard,
        /// then calls a helper that takes it again. Without this assertion the inner take blocks
        /// forever holding the lock every other serial test queues on — a silent, output-free
        /// hang. `join` returning an `Err` is the panic; a `join` that never returns would itself
        /// be the regression, which is why nothing here has a timeout to get wrong.
        #[test]
        fn taking_the_guard_twice_on_one_thread_panics_instead_of_hanging() {
            let attempt = std::thread::spawn(|| {
                let _guard = super::serial();
                let _second = super::serial(); // the deadlock this assertion replaces
            })
            .join();
            assert!(attempt.is_err(), "the re-entrant take must panic");
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
    paths::in_runtime_dir(paths::runtime_file::EVENTS)
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

/// Open a log sink for append at [`LOG_MODE`] — **the one open discipline every Rust-side sink
/// goes through** (the event log below and the panic hook's crash log in `app::boot`), the twin of
/// `src/main.c`'s `open_fd_log`. The path is in the shared, sticky `/tmp`, so the open is a
/// security boundary and refuses, with the file untouched:
///
/// * a symlink (`O_NOFOLLOW`);
/// * anything that is not a regular file, or is not owned by this process's uid;
/// * **a file with more than one link** (`st_nlink != 1`). A co-resident app in the same gid that
///   can link names in `/tmp` can `link(2)` one of OUR 0600 files — the `auth.json` session
///   fallback sits in the same runtime directory — onto a sink name when `fs.protected_hardlinks`
///   is off. The open then lands on an inode that passes every other check because it IS ours, and
///   the `fchmod` below would publish it to the group. Asked of the OPENED descriptor, so there is
///   no window between the check and the use.
///
/// An existing name is opened without `O_CREAT`; only a name that does not exist is created, with
/// `O_EXCL`, so the mode correction runs on an inode this process just made or on a single-link one
/// it already owned. The mode is set with `fchmod` rather than trusted to `open(2)`, so it does not
/// depend on the umask and an append target that survived from a 0600 release is corrected on its
/// first write.
pub(crate) fn open_log_append(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    let open = |create: bool| {
        let mut options = std::fs::OpenOptions::new();
        options.append(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
        if create {
            options.create_new(true).mode(LOG_MODE);
        }
        options.open(path)
    };
    let file = match open(false) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match open(true) {
            // Lost a race to another creator (the C shim, a second thread): check that file instead.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => open(false)?,
            other => other?,
        },
        other => other?,
    };
    let meta = file.metadata()?;
    if !meta.file_type().is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
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

/// Append one complete, newline-terminated record to the log sink at `path`: [`open_log_append`]'s
/// discipline, then [`write_log_line`]'s single `write`. The ONE way a Rust-side sink is written,
/// shared by [`log`] and the panic hook's crash log (`app::boot`), so no sink is opened with less
/// care than the event log. The caller passes the line already redacted
/// ([`diag::scrub::scrub_local`]); this adds no scrubbing of its own.
pub(crate) fn append_record(path: &std::path::Path, line: &str) -> std::io::Result<()> {
    write_log_line(&mut open_log_append(path)?, line)
}

pub(crate) fn log(m: &str) {
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
    let _ = append_record(&p, &line);
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

    /// **The hard-link confused deputy.** A co-resident app in the shared gid can `link(2)` one of
    /// OUR files onto a sink name in the shared, sticky `/tmp` (when `fs.protected_hardlinks` is
    /// off, which the television's kernel config does not promise). The open then lands on an inode
    /// that passes `O_NOFOLLOW`, `S_ISREG` and "owned by us" because it IS ours; widening its mode
    /// would publish a file that was 0600 on purpose (the `auth.json` session fallback lives in the
    /// same runtime directory). `st_nlink != 1` is the only thing that tells it from a real sink, and
    /// it has to be asked of the OPENED descriptor.
    #[test]
    fn a_sink_hard_linked_to_another_file_is_refused_and_that_file_is_untouched() {
        use std::os::unix::fs::MetadataExt;
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plx-rust-log-link-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let other = dir.join("auth.json");
        let sink = dir.join("sink");
        std::fs::write(&other, b"session").unwrap();
        std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&other, &sink).unwrap();

        assert!(
            open_log_append(&sink).is_err(),
            "a sink with a second name is somebody else's inode"
        );
        let meta = std::fs::metadata(&other).unwrap();
        assert_eq!(meta.mode() & 0o7777, 0o600, "the other file's mode is never widened");
        assert_eq!(std::fs::read(&other).unwrap(), b"session", "and never written to");
        assert_eq!(meta.nlink(), 2, "the refusal removes nothing either");

        // The same file with its second name gone is an ordinary single-link survivor again, so
        // the upgrade correction (a 0600 file left by an older release becomes 0640) still runs.
        std::fs::remove_file(&sink).unwrap();
        drop(open_log_append(&other).unwrap());
        assert_eq!(std::fs::metadata(&other).unwrap().mode() & 0o7777, 0o640);
        let _ = std::fs::remove_dir_all(&dir);
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
            // libtest names a test without its crate: `log_sink_tests::…`.
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

// Stage A foundation: owner adapters connect these APIs in the next integration stage.
#[allow(dead_code)]
mod storage;
#[allow(dead_code)]
mod storage_worker;
