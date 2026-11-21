//! How the application ends, and what it hands back to whatever started it.
//!
//! [`run_application`](super::run_application) RETURNS an [`Exit`]; it never ends the process.
//! An early-boot refusal returns its code, and a played session returns after `run::shutdown` and
//! the telemetry guard drop. Ending the process is the EXECUTABLE entry's: `port::plex_main` (the C
//! shim enters through `plex_run`, which only calls it) calls
//! `plx_base::process::exit_now(exit.code)` right after, in the order the app always ended in, so a
//! television behaves as before; the simulator's `main` calls `plx_platform_desktop::run`, which returns the
//! code, and ends the process the same way. A process that HOSTS the application never calls `exit_now` or
//! `exit`: libc's `exit` runs OpenSSL's cleanup under the detached curl workers (#1115), and a host
//! would be killed under its own runtime. No exit a hosted process can reach remains in the tree:
//! the graphics boot returns a [`GfxFault`](plx_gfx::gfx::GfxFault) ([`Exit::from`]) where it used
//! to call `exit_now(1)`, and `lifecycle_tests` below scans the production source for any
//! `exit_now(` outside the entries. `plex_main` is typed `-> !`, a const beside it proves it, and
//! the scan holds that the const is still there.
//!
//! # What is leaked, and why
//!
//! [`entry`] wraps the mounted value in `ManuallyDrop`, so returning drops nothing: the detached
//! workers (notification feed, metadata, watchlist, auth probes) are still running, and
//! `SessionAdapter::drop`, GL object drops (`QrCode::drop` deletes a texture) and the feed
//! `Engine` must never run under them (#1115). It is the same on both entry kinds. `App`'s
//! `drop_probe` (test builds) is the sentinel that proves it: dropping the `App` drops the probe,
//! whatever its other fields are.
//!
//! # What the release says
//!
//! [`Release`] is the one decision the teardown makes about the display: `Complete` is the arm of
//! [`release_of`] that ends with `SDL_Quit()` (`NativeExit::Clean`), `Retained` is the arm that logs
//! "SDL_Quit skipped" because a native worker is still inside firmware
//! (`NativeExit::WorkerInFirmware`) and, for a refusal, "nothing was torn down". The executable
//! ignores it; a hosted entry maps `Retained` to `clean = 0`. `Complete` means `Surface::quit` ran;
//! for the SDL shell that is `SDL_Quit`, which releases the window and the GL context.
//!
//! # The host doorway
//!
//! [`HostGate`], [`Completion`] and [`Stopped`] are the hosted entry's half of the contract (the
//! Tizen host's callbacks, `plx_host_stop` and its deadline watchdog). They are plain values, not
//! statics, and decide nothing by themselves that a runner has to wait for: [`HostGate::report`]
//! answers "not yet" while a gated callback is running and the runner asks again. The C ABI that
//! drives them is the hosted platform's; `lifecycle_tests` drives them through a fake host.
//!
//! ```text
//! host callback ──enter──▶ gate ──(stopping? refused)──▶ runs ──drop──▶ inflight - 1
//! loop thread returns ──report──▶ clean = Complete AND inflight == 0 ──deliver──▶ on_stopped (once)
//! deadline thread ──expire──▶ clean = 0 ──deliver──▶ on_stopped (once; the loser is suppressed)
//! ```

use plx_gfx::gfx::GfxFault;
use plx_media::player::NativeExit;
use std::mem::ManuallyDrop;
use std::os::raw::c_int;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Whether the application gave back every native resource it held before it returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Release {
    /// The teardown ran to its end and released the window and the GL context.
    Complete,
    /// Something is still held: a native worker is inside firmware, or the boot refused before any
    /// teardown ran. Nothing may be released under it, and a host treats process exit as the
    /// teardown.
    Retained,
}

/// What `run_application` returns to its entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    /// The process exit status: `1` after a failed recording or a refused boot, else `0`.
    pub code: c_int,
    pub release: Release,
}

impl Exit {
    /// An early-boot refusal. Nothing was torn down, so nothing is released.
    pub const fn refused(code: c_int) -> Self {
        Self { code, release: Release::Retained }
    }

    /// A played session's end: `failed` is a recording that did not land or a controlled run that
    /// refused, and `release` is what [`crate::app::run::shutdown`] returned.
    pub(crate) fn played(failed: bool, release: Release) -> Self {
        Self { code: c_int::from(failed), release }
    }
}

/// A required shader the driver refused ended the process with status 1 (`exit_now(1)`); it ends the
/// boot with the same status and no teardown, which is a refusal like any other.
impl From<GfxFault> for Exit {
    fn from(_: GfxFault) -> Self {
        Self::refused(1)
    }
}

/// What [`crate::app::run::shutdown`] releases for a worker-exit verdict. The ONE `match` that
/// decides it, exhaustive with no `_` arm so a third [`NativeExit`] fails to compile here and in
/// [`crate::app::run::quits_sdl`], which is derived from it: the hosted report cannot say
/// `Complete` where the executable skipped `SDL_Quit`.
pub(crate) fn release_of(native_exit: NativeExit) -> Release {
    match native_exit {
        NativeExit::Clean => Release::Complete,
        // A native worker is still inside firmware: the display must not be torn down under it.
        NativeExit::WorkerInFirmware => Release::Retained,
    }
}

/// Construct, run, return, and leak: the whole of `run_application`'s body, with the two phases
/// passed in so a host test drives the same path with a refusal, a stand-in or a real `App`.
///
/// A refusal returns its [`Exit`]. A mounted value is handed to `run` and then forgotten
/// (`ManuallyDrop`), never dropped: see the module note. `run` ends the session and says how.
pub(crate) fn entry<T>(
    enter: impl FnOnce() -> Result<T, Exit>,
    run: impl FnOnce(&mut T) -> Exit,
) -> Exit {
    match enter() {
        Ok(mounted) => run(&mut ManuallyDrop::new(mounted)),
        Err(exit) => exit,
    }
}

/// The sentinel on `App` that shows the leak: a counter its `Drop` increments, shared with the test
/// that holds the other end. It is a FIELD, not a static, so dropping the `App` drops it whatever
/// the `App`'s other fields are (and no `statics` line is owed).
#[cfg(test)]
#[derive(Default)]
pub(crate) struct DropProbe(std::sync::Arc<AtomicUsize>);

#[cfg(test)]
impl DropProbe {
    /// The counter, to be read after the `App` has gone wherever the test sent it. Only the
    /// hostsim lifecycle tests build an `App` to read it from.
    #[cfg_attr(not(all(feature = "hostsim", feature = "play-feed")), allow(dead_code))]
    pub(crate) fn counter(&self) -> std::sync::Arc<AtomicUsize> {
        std::sync::Arc::clone(&self.0)
    }
}

#[cfg(test)]
impl Drop for DropProbe {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// One host-bound callback's admission. Every callback into the host (window, player request, log
/// sink) and every event from it (a player completion) enters through ONE of these, and is refused
/// once the stop was requested.
///
/// `stopping` alone could only choose which sender reports; it cannot drain a callback that is
/// already running. `inflight` is that drain: [`HostGate::report`] answers `clean` only once it is
/// zero.
// The first caller is the hosted entry (the Tizen platform); `lifecycle_tests` grade it until then.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct HostGate {
    stopping: AtomicBool,
    inflight: AtomicUsize,
}

/// A callback that was admitted by [`HostGate::enter`] and has not returned. Dropping it is the
/// return.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct InFlight<'a>(&'a HostGate);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg_attr(not(test), allow(dead_code))]
impl HostGate {
    pub(crate) const fn new() -> Self {
        Self { stopping: AtomicBool::new(false), inflight: AtomicUsize::new(0) }
    }

    /// Admit one callback, or refuse it (`None`) because the stop was requested: the caller drops a
    /// late player completion and makes no further call into the host.
    ///
    /// The count goes up BEFORE `stopping` is read and comes back down on a refusal. The other
    /// order lets a callback read "not stopping", lose the CPU while [`HostGate::stop`] and
    /// [`HostGate::report`] run, and then start with the report already sent as clean. All four
    /// operations are `SeqCst`, so either the callback sees the stop or `report` sees the callback.
    pub(crate) fn enter(&self) -> Option<InFlight<'_>> {
        self.inflight.fetch_add(1, Ordering::SeqCst);
        if self.stopping.load(Ordering::SeqCst) {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(InFlight(self))
    }

    /// `plx_host_stop`: refuse every later callback. Answers whether THIS call is the one that
    /// stopped (a second `stop` is harmless and answers `false`).
    pub(crate) fn stop(&self) -> bool {
        !self.stopping.swap(true, Ordering::SeqCst)
    }

    pub(crate) fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Callbacks admitted and not yet returned. A refused [`HostGate::enter`] shows here for an
    /// instant, which is why [`HostGate::report`] is asked again rather than trusted once.
    pub(crate) fn in_flight(&self) -> usize {
        self.inflight.load(Ordering::SeqCst)
    }

    /// The loop thread's report once `run_application` returned `exit`: it stops the gate, and
    /// answers `None` ("a callback is still running, ask again") until none is. `clean` is
    /// `Complete` AND drained: a `Retained` loop is never clean, whatever the callbacks did.
    pub(crate) fn report(&self, exit: &Exit) -> Option<Stopped> {
        self.stop();
        (self.in_flight() == 0)
            .then_some(Stopped { code: exit.code, clean: exit.release == Release::Complete })
    }

    /// The deadline thread's report, when the loop has not finished in time (a stalled swap): never
    /// clean, because a loop that has not returned has released nothing.
    pub(crate) fn expire(&self, code: c_int) -> Stopped {
        self.stop();
        Stopped { code, clean: false }
    }
}

/// What `on_stopped(code, clean)` carries. With `clean` the host may release its callbacks and
/// surfaces; without it, it releases nothing and treats process exit as the teardown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Stopped {
    pub(crate) code: c_int,
    pub(crate) clean: bool,
}

/// `on_stopped`'s exactly-once guard. The loop thread's report and the deadline thread's race for
/// it; the first delivers, and the loser's report is suppressed (`false`).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct Completion {
    delivered: AtomicBool,
}

#[cfg_attr(not(test), allow(dead_code))]
impl Completion {
    pub(crate) const fn new() -> Self {
        Self { delivered: AtomicBool::new(false) }
    }

    /// Deliver `stopped` through `on_stopped`, unless a report was delivered already. The watchdog
    /// is disarmed first: the loop thread is about to end (its id with it), and after `on_stopped`
    /// the host may tear the process down around it.
    pub(crate) fn deliver(&self, stopped: Stopped, on_stopped: impl FnOnce(Stopped)) -> bool {
        if self.delivered.swap(true, Ordering::SeqCst) {
            return false;
        }
        plx_base::task::watchdog::disarm();
        on_stopped(stopped);
        true
    }
}

/// The host-doorway contract, through a fake host: the second implementation of the table the
/// hosted platform will fill, with threads standing in for its.
#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::sync::{mpsc, Mutex};

    /// A host: the gate and the once-guard its table shares, and what `on_stopped` received. It
    /// holds the process-global test lock for its whole life: `Completion::deliver` disarms the
    /// process watchdog, which a test that arms it and reads it back must not race.
    struct FakeHost {
        _serial: plx_base::testlock::Serial,
        gate: HostGate,
        completion: Completion,
        /// Each delivery, with whether the watchdog was still armed when the callback ran.
        received: Mutex<Vec<(Stopped, bool)>>,
    }

    impl FakeHost {
        fn new() -> Self {
            Self {
                _serial: plx_base::testlock::serial(),
                gate: HostGate::new(),
                completion: Completion::new(),
                received: Mutex::new(Vec::new()),
            }
        }

        fn on_stopped(&self, stopped: Stopped) {
            self.received.lock().unwrap().push((stopped, plx_base::task::watchdog::is_armed()));
        }

        /// The loop thread's tail once `run_application` returned: ask until the gate drains.
        fn loop_returned(&self, exit: &Exit) -> bool {
            loop {
                if let Some(stopped) = self.gate.report(exit) {
                    return self.completion.deliver(stopped, |stopped| self.on_stopped(stopped));
                }
                std::thread::yield_now();
            }
        }

        /// The watchdog thread `plx_host_stop` starts, once its deadline passed.
        fn deadline(&self, code: c_int) -> bool {
            let stopped = self.gate.expire(code);
            self.completion.deliver(stopped, |stopped| self.on_stopped(stopped))
        }

        fn received(&self) -> Vec<Stopped> {
            self.received.lock().unwrap().iter().map(|(stopped, _)| *stopped).collect()
        }
    }

    fn quit() -> Exit {
        Exit::played(false, Release::Complete)
    }

    #[test]
    fn a_boot_refusal_returns_its_code_and_the_host_is_told_once() {
        let exit = entry::<()>(|| Err(Exit::refused(7)), |_| unreachable!("nothing was mounted"));
        assert_eq!(exit, Exit { code: 7, release: Release::Retained });
        let host = FakeHost::new();
        assert!(host.loop_returned(&exit));
        assert_eq!(host.received(), [Stopped { code: 7, clean: false }],
            "a refusal tore nothing down, so the host is told not to release anything");
    }

    #[test]
    fn a_shader_the_driver_refused_reaches_the_host_as_status_one_and_the_host_stays_alive() {
        for fault in [GfxFault::Compile, GfxFault::Link] {
            let exit = entry::<()>(|| Err(Exit::from(fault)), |_| unreachable!("nothing was mounted"));
            assert_eq!(exit, Exit { code: 1, release: Release::Retained }, "{fault}");
            let host = FakeHost::new();
            assert!(host.loop_returned(&exit));
            assert_eq!(host.received(), [Stopped { code: 1, clean: false }], "{fault}");
        }
    }

    #[test]
    fn a_quit_during_playback_returns_after_the_teardown_and_reports_clean() {
        let ended = Mutex::new(false);
        let exit = entry(|| Ok(()), |_| {
            *ended.lock().unwrap() = true;
            quit()
        });
        assert!(*ended.lock().unwrap(), "the session ran before the entry returned");
        assert_eq!(exit, Exit { code: 0, release: Release::Complete });
        let host = FakeHost::new();
        assert!(host.loop_returned(&exit));
        assert_eq!(host.received(), [Stopped { code: 0, clean: true }]);
    }

    #[test]
    fn a_failed_recording_is_status_one_and_still_a_complete_release() {
        assert_eq!(Exit::played(true, Release::Complete), Exit { code: 1, release: Release::Complete });
    }

    #[test]
    fn a_stop_with_a_player_request_outstanding_drops_its_late_completion() {
        let host = FakeHost::new();
        // The loop asked the host to play; the host took the request and has not answered.
        drop(host.gate.enter().expect("a request before the stop is admitted"));
        assert!(host.gate.stop(), "plx_host_stop");
        assert!(host.loop_returned(&quit()));
        // The completion arrives after the stop: dropped, and nothing is delivered for it.
        assert!(host.gate.enter().is_none(), "a late player completion is refused");
        assert_eq!(host.gate.in_flight(), 0, "a refusal leaves nothing counted");
        assert_eq!(host.received(), [Stopped { code: 0, clean: true }]);
    }

    #[test]
    fn a_returning_loop_with_a_retained_worker_is_never_clean() {
        let host = FakeHost::new();
        assert!(host.loop_returned(&Exit::played(false, Release::Retained)));
        assert_eq!(host.received(), [Stopped { code: 0, clean: false }],
            "a worker is still in firmware: the host releases nothing");
    }

    #[test]
    fn a_callback_in_flight_at_stop_holds_the_clean_report_until_it_returns() {
        let gate = HostGate::new();
        let running = gate.enter().expect("admitted");
        assert_eq!(gate.in_flight(), 1);
        assert_eq!(gate.report(&quit()), None, "a flag alone cannot drain a callback already running");
        assert!(gate.stopping(), "and the report itself refuses every later one");
        assert!(gate.enter().is_none());
        drop(running);
        assert_eq!(gate.report(&quit()), Some(Stopped { code: 0, clean: true }));
    }

    #[test]
    fn the_loop_thread_waits_for_a_running_callback_and_reports_exactly_once() {
        let host = FakeHost::new();
        let running = host.gate.enter().expect("admitted");
        let (done, finished) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| done.send(host.loop_returned(&quit())).unwrap());
            while !host.gate.stopping() {
                std::thread::yield_now();
            }
            assert!(host.received().is_empty(), "the callback has not returned: nothing is reported");
            drop(running);
            assert!(finished.recv().unwrap(), "the loop thread delivered");
        });
        assert_eq!(host.received(), [Stopped { code: 0, clean: true }]);
    }

    /// Two senders, one flag: whichever reaches it first delivers, and the loser is suppressed.
    #[test]
    fn a_swap_that_stalls_and_wakes_after_the_deadline_reports_once_and_unclean() {
        let host = FakeHost::new();
        assert!(host.deadline(1), "the deadline thread reports while the loop is still stuck");
        assert!(!host.loop_returned(&quit()), "the loop wakes, returns Complete, and is suppressed");
        assert_eq!(host.received(), [Stopped { code: 1, clean: false }]);
    }

    #[test]
    fn a_loop_that_returns_in_time_suppresses_the_deadline() {
        let host = FakeHost::new();
        assert!(host.loop_returned(&quit()));
        assert!(!host.deadline(1));
        assert_eq!(host.received(), [Stopped { code: 0, clean: true }]);
    }

    #[test]
    fn two_senders_racing_deliver_exactly_once() {
        for _ in 0..200 {
            let host = FakeHost::new();
            let delivered = std::thread::scope(|scope| {
                let loop_thread = scope.spawn(|| host.loop_returned(&quit()));
                let deadline = scope.spawn(|| host.deadline(1));
                [loop_thread.join().unwrap(), deadline.join().unwrap()]
            });
            assert_eq!(delivered.iter().filter(|delivered| **delivered).count(), 1);
            assert_eq!(host.received().len(), 1);
        }
    }

    #[test]
    fn pause_then_stop_leaves_the_watchdog_disarmed_when_on_stopped_runs() {
        let host = FakeHost::new();
        plx_base::task::watchdog::arm();
        // OnPause: nothing may signal the loop thread while the host is parked.
        plx_base::task::watchdog::disarm();
        assert!(!plx_base::task::watchdog::is_armed());
        // OnResume re-captures the loop thread.
        plx_base::task::watchdog::arm();
        assert!(plx_base::task::watchdog::is_armed());
        // Stop: the report disarms before the callback runs.
        assert!(host.loop_returned(&quit()));
        let received = host.received.lock().unwrap();
        assert_eq!(received.len(), 1);
        assert!(!received[0].1, "the watchdog was disarmed before on_stopped");
    }

    #[test]
    fn the_gate_refuses_after_stop_and_counts_what_it_admitted() {
        let gate = HostGate::new();
        assert!(!gate.stopping());
        let (first, second) = (gate.enter().unwrap(), gate.enter().unwrap());
        assert_eq!(gate.in_flight(), 2);
        drop(first);
        assert_eq!(gate.in_flight(), 1);
        assert!(gate.stop());
        assert!(!gate.stop(), "only the first stop is the one that stopped");
        assert!(gate.enter().is_none());
        drop(second);
        assert_eq!(gate.in_flight(), 0);
    }

    #[test]
    fn release_follows_the_worker_exit_verdict_in_both_directions() {
        assert_eq!(release_of(NativeExit::Clean), Release::Complete);
        assert_eq!(release_of(NativeExit::WorkerInFirmware), Release::Retained);
    }

    /// **No exit of the app process returns into libc's `exit`, and none a hosted process could
    /// reach is left.** `exit` runs libcrypto's `OPENSSL_cleanup`, which frees and NULLs its global
    /// locks while the detached curl workers still run (#1115), so every exit goes through
    /// `plx_base::process::exit_now`; and that is the executable entries' to call, because a host
    /// is killed under its own runtime by one. `run_application` and the graphics boot RETURN
    /// (`Exit`, `GfxFault`), so the production `exit_now(` sites are the entries that own the
    /// process end (the webOS crate's `port::plex_main`, the simulator's `main` in the desktop crate), the frame dump (`devtriggers`, off in
    /// a release build) and the helper itself. A new exit anywhere else fails here.
    #[test]
    fn lifecycle_no_production_exit_is_reachable_from_a_hosted_process() {
        // The app process links every crate under rust-modules (the two platform crates among them, whose
        // `port.rs` and the desktop crate's `bin/sim.rs` are the entries) except `storage` (the separate
        // `plxstorage` helper binary, no OpenSSL workers) and os's `storage_service/` (that
        // helper's body). `base/src/process.rs` is the helper itself.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        collect_rs(&root.join("src"), &mut files);
        for entry in std::fs::read_dir(root).expect("rust-modules") {
            let dir = entry.expect("entry").path();
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if matches!(name.as_str(), "storage" | "target" | "tests")
                || name.starts_with("target-")
                || !dir.join("src").is_dir()
            {
                continue;
            }
            collect_rs(&dir.join("src"), &mut files);
        }
        assert!(files.len() > 100, "the scan must see the whole tree");
        let owns_process_end = ["platform-webos/src/port.rs", "platform-desktop/src/bin/sim.rs", "src/dev/framedump.rs"].map(|p| root.join(p));
        for entry in &owns_process_end {
            assert!(entry.is_file(), "{}: an entry the scan allows is gone; update the list", entry.display());
        }
        let (libc_exit, std_exit, exit_now) =
            (concat!("process::", "exit("), concat!("libc::", "exit("), concat!("exit_", "now("));
        let mut hits = Vec::new();
        for f in files {
            let path = f.to_string_lossy().replace('\\', "/");
            if path.ends_with("base/src/process.rs")
                || path.contains("/storage_service/")
                || path.contains("/tests/")
                || path.ends_with("_tests.rs")
            {
                continue;
            }
            let text = std::fs::read_to_string(&f).expect("read");
            let prod = production_prefix(&text);
            if path.ends_with("src/app/mod.rs") {
                assert!(prod.contains("fn run_application"), "the scan must still reach run_application");
            }
            if f == root.join("platform-webos/src/port.rs") {
                assert_process_entry(&text, exit_now);
            }
            for (n, line) in prod.lines().enumerate() {
                let code = line.split("//").next().unwrap_or("");
                if code.contains(libc_exit)
                    || code.contains(std_exit)
                    || (code.contains(exit_now) && !owns_process_end.contains(&f))
                {
                    hits.push(format!("{path}:{}", n + 1));
                }
            }
        }
        assert!(
            hits.is_empty(),
            "exit through plx_base::process::exit_now from an entry that owns the process end, \
             never libc exit (#1115) and never from library code a host runs: {hits:?}"
        );
    }

    /// `port.rs`'s code with its `//` comments dropped, so a commented-out call, const or definition
    /// cannot satisfy or trip a scan of it. (A block comment would defeat that, and is refused where
    /// this is used.)
    fn without_line_comments(text: &str) -> String {
        text.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("\n")
    }

    /// **The process entry in `port.rs`: `plex_run` is exactly one call, and `plex_main` ends the
    /// process.** The process entry ends the process itself and is typed `-> !`: `plex_run` returns
    /// into `__libc_start_main`'s libc `exit` if it ever returns (#1115). The const is the
    /// compile-time proof for `plex_main` only; `plex_run`, the C entry, is `-> c_int`, so an early
    /// `return 2;` above its call to `plex_main` would compile (`!` coerces). This holds the const
    /// still there, `plex_main` still the one that exits, and `plex_run` exactly the one call. All of
    /// it is read off comment-stripped lines, so a commented-out const or call cannot satisfy it (and
    /// no block comment may exist to defeat that).
    ///
    /// Both entries must also be DEFINED exactly once, anywhere in the file. This scan and
    /// `ci/check-deps.sh`'s `fnlen` budgets read only the FIRST `fn plex_run(` and the first
    /// `fn plex_main(`, so a second, cfg-gated definition would be the one a build runs while both
    /// guards graded the other.
    fn assert_process_entry(text: &str, exit_now: &str) {
        assert!(!text.contains("/*"), "port.rs has a block comment; strip those as well");
        let every = without_line_comments(text);
        for entry in ["fn plex_main(", "fn plex_run("] {
            assert_eq!(
                every.matches(entry).count(),
                1,
                "port.rs must define `{entry}` exactly once: the guards read only the first, so a \
                 second (cfg-gated) one would run unguarded (#1115)"
            );
        }
        let code = without_line_comments(production_prefix(text));
        let main = code.split_once("fn plex_main(").expect("port.rs must keep plex_main").1;
        let main = main.split_once("\n}\n").expect("the end of plex_main").0;
        assert!(main.contains(exit_now), "plex_main must end the process through exit_now");
        assert!(
            code.contains("const _: fn(*const c_char, c_int) -> ! = plex_main;"),
            "the const proving plex_main never returns is gone"
        );
        let run = code.split_once("fn plex_run(").expect("port.rs must keep plex_run").1;
        let run = run.split_once('{').expect("the start of plex_run's body").1;
        let run = run.split_once('}').expect("the end of plex_run's body").0;
        assert_eq!(
            run.split_whitespace().collect::<Vec<_>>().join(" "),
            "plex_main(pms_host, pms_port)",
            "plex_run must be exactly the call to plex_main: no `return`, no other statement, \
             or it can return into libc `exit` (#1115)"
        );
    }

    const ENTRY: &str = "\
#[no_mangle]
pub extern \"C\" fn plex_run(pms_host: *const c_char, pms_port: c_int) -> c_int {
    plex_main(pms_host, pms_port)
}
fn plex_main(pms_host: *const c_char, pms_port: c_int) -> ! {
    plx_base::process::exit_now(host::run(&PLATFORM, host, port).code)
}
const _: fn(*const c_char, c_int) -> ! = plex_main;
";

    /// The scan's own control: the entry shape it accepts, so the refusals below fail on what they
    /// plant and not on a premise the fixture lacks.
    #[test]
    fn the_process_entry_scan_accepts_the_one_call_entry() {
        assert_process_entry(ENTRY, concat!("exit_", "now("));
    }

    /// A second `plex_run` behind a cfg that is never true compiles everywhere and is invisible to a
    /// reader of the first, so the scan counts definitions instead of trusting its first match. The
    /// plant follows a test module too: the production prefix must not be where the count stops.
    #[test]
    #[should_panic(expected = "must define `fn plex_run(` exactly once")]
    fn a_second_cfg_gated_plex_run_is_refused() {
        let planted = format!(
            "{ENTRY}#[cfg(test)]\nmod tests {{\n}}\n#[cfg(any())]\nfn plex_run(h: *const c_char, p: c_int) -> c_int {{\n    return 2;\n}}\n"
        );
        assert_process_entry(&planted, concat!("exit_", "now("));
    }

    #[test]
    #[should_panic(expected = "must define `fn plex_main(` exactly once")]
    fn a_second_cfg_gated_plex_main_is_refused() {
        let planted = format!("{ENTRY}#[cfg(any())]\nfn plex_main(h: *const c_char, p: c_int) -> ! {{\n    loop {{}}\n}}\n");
        assert_process_entry(&planted, concat!("exit_", "now("));
    }

    /// The text above the file's first `#[cfg(test)]` MODULE. A `#[cfg(test)]` import or field on the
    /// way (`app/mod.rs` has both, above `run_application`) must not hide the production code under it.
    fn production_prefix(text: &str) -> &str {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let mut offset = 0;
        for pair in lines.windows(2) {
            let attr = pair[0].trim();
            let test_only = attr == "#[cfg(test)]" || attr.starts_with("#[cfg(all(test");
            if test_only && pair[1].contains("mod ") && pair[1].trim_end().ends_with('{') {
                return &text[..offset];
            }
            offset += pair[0].len();
        }
        text
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read_dir").flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_rs(&p, out);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push(p);
            }
        }
    }

    /// The `App` itself: the rig builds a real one, which only a hostsim build with the Feed has.
    #[cfg(all(feature = "hostsim", feature = "play-feed"))]
    mod app_leak {
        use super::*;
        use crate::app::run::lifecycle_regression_tests::app;

        #[test]
        fn the_returning_exit_path_leaks_the_app_so_nothing_under_it_drops() {
            let _serial = plx_base::testlock::serial();
            let app = app();
            let dropped = app.drop_probe.counter();
            let exit = entry(|| Ok(app), |_| quit());
            assert_eq!(exit, quit());
            assert_eq!(dropped.load(Ordering::SeqCst), 0, "the probe is a field: no field of the App dropped");
        }

        #[test]
        fn control_an_app_dropped_the_ordinary_way_trips_the_probe() {
            let _serial = plx_base::testlock::serial();
            let app = app();
            let dropped = app.drop_probe.counter();
            drop(app);
            assert_eq!(dropped.load(Ordering::SeqCst), 1, "the probe detects a drop");
        }
    }
}
