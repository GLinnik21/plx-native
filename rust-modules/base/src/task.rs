//! The thread seam: where work leaves the main thread ([`spawn`]), and what cannot follow it
//! ([`MainThread`]).
//!
//! `std::thread::spawn` **panics** when the OS refuses the thread — it unwraps `Builder::spawn`,
//! whose `Err` is `pthread_create`'s EAGAIN (glibc returns it when `allocate_stack` cannot `mmap`
//! the stack, or `clone(2)` hits `RLIMIT_NPROC`/`threads-max`). Almost every worker here is
//! started from the SDL loop, so that panic unwinds out of `plex_run` through the C shim and
//! takes the app down. It would also fail at the worst possible moment: the caller has just armed
//! an in-flight flag, so had the app survived, the screen would sit on a spinner that can never
//! resolve.
//!
//! **Measured on the target, not estimated** (`tools/threadprobe.c`, 2026-07-28, run under the
//! app's own uid — the first version of this note guessed and was wrong about which limit binds):
//!
//! | stack | refused at | binding limit |
//! |---|---|---|
//! | 2 MB (the platform default) | **2043 threads** | `RLIMIT_AS` — the full AArch32 4 GB space |
//! | 256 KB (`spawn_small`) | **3745 threads** | `RLIMIT_NPROC`, which is 3746 |
//!
//! Both refusals are EAGAIN, exactly the error `spawn` unwraps. Against that, the app runs **31
//! threads at playback peak** (13 at Home) and peaks at 363 MB of `VmSize` — ~66x and ~11x
//! headroom. So this module is not fixing something that happens; it is refusing to let an
//! unreachable-but-unrecoverable branch exist, for the price of a return value.
//!
//! The table is also why [`SMALL_STACK`] is not a micro-optimisation: which limit you hit depends
//! on the stack size, and the crossover sits between these two values. A 2 MB stack spends address
//! space (the scarcer resource here at 1/2043 per thread); 256 KB spends a thread slot instead.
//!
//! Both entry points below report the refusal instead of panicking. Releasing the armed flag
//! stays with the caller — a latch is a property of the screen, not of the thread.
//!
//! **This is what step 3 of `docs/async-model-decision.md` became.** That step adopted a generic
//! `task::Job<T>` — generation guard, monotone one-slot mailbox, single-flight, cancel — to
//! replace the hand-rolled worker idiom. By the time its turn came, the idiom had already landed
//! by hand at five sites and been device-verified, so `Job` would have been a pure refactor of
//! working code; the re-evaluation declined it and is logged in the decision doc. What that
//! re-evaluation turned up instead is the divergence above: of the five copies of the idiom, only
//! two handled a refused spawn. The piece worth sharing was the spawn, not the mailbox.
//!
//! **The two thread-affinity tokens are mirror images.** [`MainThread`] is `!Send`: a function
//! that must stay on the frame thread takes one, and moving it onto a worker stops compiling.
//! [`OffFrame`] is the same rule in reverse: it is `Send`, it can only be minted *inside* a worker
//! ([`spawn_off_frame`], or `OffFrame::for_test` under test), and a function that must never run
//! on the frame thread takes `&OffFrame`. **A PMS half takes `&OffFrame`**: the network round trip
//! of a Plex call is the work that may not run under a frame scope, and holding the token is how a
//! signature says so. Every route-changing PMS half takes it (`route::flight`'s workers and the
//! `run_*` steps behind them); the two frame-thread sites left in `ci/allow/blocking.txt` (which
//! `ci/check-deps.sh`'s `blocking` gate only lets shrink) are the refused-worker fallbacks
//! `spawn_or_inline` and `route::decision::scrobble_stop`, not PMS halves.
//!
//! [`spawn_small_or_inline`] is the one place the "the OS refused the worker" fallback lives: it
//! tries a small-stack thread and, if the OS refuses, runs the same work inline under a labelled
//! [`allow_blocking`] exception and logs it. A server-side resource stop must still happen, so
//! dropping it (what [`spawn_small`] reports) is not an option there.
//!
//! **This module is also the allowlist boundary of `ci/check-deps.sh`'s `threads` gate**
//! (restructure spec §15.2, phase 12): every `std::thread::spawn` call outside this file, in
//! production code, is a violation — checked file by file across the whole crate, with a
//! `#[cfg(test)] mod` block excluded (a unit test's own mock TCP/HTTP peer stands in for a real
//! peer and is not a worker). Verified 2026-09-10: it is already empty, i.e. every real worker
//! this crate spawns — the demux/media threads, `aq`, `stream`, `imgcache`, `ff`, `http`, `auth`,
//! `curlio`/`route::decision`, the Plex client/transcoder, `browse`, `player`, `plx_machine::present`,
//! `plx_machine::landgate` — already calls [`spawn`]/[`spawn_small`], not `std::thread::spawn` directly.
//! `ci/allow/threads.txt` is that gate's allowlist and it, too, is empty for the same reason; a
//! new production `thread::spawn` outside this file fails CI rather than waiting for a review.

use std::io;
use std::marker::PhantomData;
use std::thread::{Builder, JoinHandle};

mod blocking;
pub mod watchdog;
#[cfg(feature = "threadcheck")]
pub mod runtime_check;
pub use blocking::{allow_blocking, AllowBlocking};
pub use blocking::{assert_may_block, BlockingGuard, BlockingLabel, FrameScope};

/// Proof that the holder runs on a worker thread, never on the frame thread — the [`MainThread`]
/// rule in reverse.
///
/// A ZST with a private field, so it cannot be written down outside this module. It is minted in
/// exactly two places: inside the thread [`spawn_off_frame`] starts (and handed to the closure by
/// reference), and `OffFrame::for_test` for host tests. Unlike [`MainThread`] it is `Send`: the
/// point is to be carried into a worker and passed down to the functions that do the blocking
/// work, whose signatures then say they are not for the frame thread. **A PMS half takes
/// `&OffFrame`.**
///
/// The ceiling is the same as [`MainThread`]'s: a worker can hand its `&OffFrame` to a function
/// it calls, but nothing can conjure one on the frame thread without being written in this file.
pub struct OffFrame(());

impl OffFrame {
    /// Mint the token on the current thread, which must be a worker.
    fn mint() -> Self {
        debug_assert!(!blocking::in_frame(), "an OffFrame token was minted on the frame thread");
        OffFrame(())
    }

    /// For host tests, which call worker-side functions directly on the test thread.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test() -> Self {
        OffFrame(())
    }
}

/// Proof that the holder runs on the SDL main thread.
///
/// A ZST whose only content is a `!Send + !Sync` marker, so neither it nor a `&` to it can be
/// captured by anything handed to [`spawn`]. That is the whole mechanism: functions that must
/// not leave the main thread take one, and moving such a call onto a thread stops compiling.
///
/// It gates the two things in `player/` that were main-thread-confined by comment only:
///
/// * **the ACB/Starfish seam** — `player::ffi`'s wrappers. The raw `extern "C"` block is private
///   to that module, so the token is the only way in. Bind order (`setMediaId` → `LOADED` →
///   `setMediaVideoData` → `setDisplayWindow` → `PLAYING`) is a sequence of calls with no
///   locking behind it; a second thread stepping into the middle of it corrupts the sink.
/// * **the native session slot** — `App.adapters.player`. Until phase 9 that was
///   `player::engine::ENGINE`, a `static mut` handed out as `&'static mut` with worker threads
///   holding raw pointers into the boxes it owns: two live `&mut` to it is instant UB, and the
///   token was the only thing standing between the code and one. It is a FIELD now, so the token
///   is CONSUMED into `player::adapter::PlayerAdapter` and `&mut PlayerAdapter` is the
///   proof instead — one the borrow checker keeps rather than one a caller can satisfy twice.
///
/// The one deliberate hole: `assume` is callable, so `unsafe { MainThread::assume() }` inside a
/// worker would defeat this. That is the ceiling of the pattern, not an oversight — what it buys
/// is that the mistake has to be *written*, in an `unsafe` block, instead of happening by
/// forgetting a convention documented in three other files.
pub struct MainThread(PhantomData<*const ()>);

impl MainThread {
    /// Mint the token. `plex_run` calls this once, at the top, and nothing else should.
    ///
    /// # Safety
    /// The caller asserts this is the SDL main thread. It is not a memory-safety obligation in
    /// itself — it is the premise every `&MainThread` downstream is trusted on, including the
    /// one the Player adapter holds, so a false one reintroduces exactly the races this prevents.
    pub unsafe fn assume() -> Self {
        MainThread(PhantomData)
    }
}

/// Stack for the short network workers — one HTTP round trip and a decode. The platform default
/// (2 MB) is pure reserved address space on a 32-bit target, and several of these run at once.
/// Deliberately NOT used for the demux/decode/encode threads: libavformat wants a real stack.
const SMALL_STACK: usize = 256 * 1024;

fn refused(what: &str, e: &io::Error) {
    crate::eventlog::log(&format!(
        "task: spawn '{what}' REFUSED ({e}) — this work is dropped"
    ));
}

fn try_spawn(what: &str, stack: Option<usize>, f: impl FnOnce() + Send + 'static) -> io::Result<JoinHandle<()>> {
    // `what` names the worker for the host tests' drain report; the shipping build has no use for it.
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = what;
    let mut b = Builder::new();
    if let Some(n) = stack {
        b = b.stack_size(n);
    }
    // Counted BEFORE the spawn and released by the closure's own drop, so a refused spawn (which
    // drops the closure unrun) and a panicking worker both give the count back.
    #[cfg(any(test, feature = "test-support"))]
    let counted = SmallWorker::enter(stack == Some(SMALL_STACK), what);
    b.spawn(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _counted = counted;
        f()
    })
}

/// How many small-stack network workers (`spawn_small*`) are alive. A host test that returns while
/// one is still running lets it write process globals (the player and route state, the season and
/// detail mailboxes, libcurl's init) under the NEXT test's lock; [`drain_workers_for_test`] is the
/// wait that ends that.
#[cfg(any(test, feature = "test-support"))]
static SMALL_WORKERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The labels of the live counted workers, so the drain's report names what it is waiting on.
#[cfg(any(test, feature = "test-support"))]
static SMALL_LABELS: std::sync::Mutex<Vec<(u64, String)>> = std::sync::Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-support"))]
struct SmallWorker(Option<u64>);

#[cfg(any(test, feature = "test-support"))]
impl SmallWorker {
    fn enter(counted: bool, what: &str) -> Self {
        // Not counted: the library-discovery worker (`browse::spawn_discovery`) dials whatever
        // server the test registered, which may be an address that answers only by its connect
        // timeout, and it posts into the per-owner adapter the test owns and drops with itself —
        // no process global — so waiting for it would charge every such test seconds for nothing.
        const UNCOUNTED: &[&str] = &["sources"];
        if !counted || UNCOUNTED.contains(&what) {
            return SmallWorker(None);
        }
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SMALL_LABELS.lock().unwrap_or_else(|e| e.into_inner()).push((id, what.to_owned()));
        SMALL_WORKERS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        SmallWorker(Some(id))
    }
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for SmallWorker {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            SMALL_LABELS.lock().unwrap_or_else(|e| e.into_inner()).retain(|(i, _)| *i != id);
            SMALL_WORKERS.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// Wait (bounded) until no small-stack worker is alive. `testlock::Serial`'s drop calls this, so
/// the lock a test holds is not released while a worker it started is still running. Production
/// never calls it. A worker still alive at the bound (parked on a peer the test never answers) is
/// reported on stderr and left, rather than hanging the suite.
#[cfg(any(test, feature = "test-support"))]
pub fn drain_workers_for_test() {
    use std::sync::atomic::Ordering::SeqCst;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while SMALL_WORKERS.load(SeqCst) > 0 {
        if std::time::Instant::now() >= deadline {
            let live: Vec<String> =
                SMALL_LABELS.lock().unwrap_or_else(|e| e.into_inner()).iter().map(|(_, w)| w.clone()).collect();
            eprintln!("task: {} small worker(s) still running after 5 s at test end: {live:?}", live.len());
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

fn spawn_with(
    what: &str,
    stack: Option<usize>,
    f: impl FnOnce() + Send + 'static,
) -> Option<JoinHandle<()>> {
    match try_spawn(what, stack, f) {
        Ok(h) => Some(h),
        Err(e) => {
            refused(what, &e);
            None
        }
    }
}

/// Spawn `f` and keep the handle. `None` means the OS refused the thread and **the worker does
/// not exist** — undo whatever was armed for it. `what` names the work in the failure log.
pub fn spawn(what: &str, f: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    spawn_with(what, None, f)
}

/// [`spawn`] on a [`SMALL_STACK`], for the fire-and-forget network workers. `false` = not spawned.
pub fn spawn_small(what: &str, f: impl FnOnce() + Send + 'static) -> bool {
    spawn_with(what, Some(SMALL_STACK), f).is_some()
}

/// [`spawn_small`] for work that must run off the frame thread and read the proof:
/// `f` receives the [`OffFrame`] token, minted inside the new thread. `false` = the OS refused the
/// thread and the work does not exist (undo whatever was armed for it), exactly as
/// [`spawn_small`].
pub fn spawn_off_frame(what: &str, f: impl FnOnce(&OffFrame) + Send + 'static) -> bool {
    spawn_small(what, move || f(&OffFrame::mint()))
}

/// [`spawn`] for a long-lived worker that reads the proof, and whose handle the caller keeps
/// (the demuxer, which parks on the network for a whole playback): `f` receives the [`OffFrame`]
/// token, minted inside the new thread.
pub fn spawn_off_frame_keeping(what: &str, f: impl FnOnce(&OffFrame) + Send + 'static) -> Option<JoinHandle<()>> {
    spawn(what, move || f(&OffFrame::mint()))
}

/// Run `f` on a small-stack worker; if the OS refuses the thread, run it HERE, inline, under
/// `allow_blocking(label)`, and log that it happened. For work that must not be dropped (a stop
/// that frees a server-side resource) and whose caller has no way to defer it. `f` runs exactly
/// once, on one side or the other. `label` should end "(worker thread refused)".
///
/// `#[track_caller]`: the inline exception reports the CALLER's `file:line`, not this function's.
/// Its `allow_blocking` is the one `ci/allow/blocking.txt` entry for every refused-thread arm.
#[track_caller]
pub fn spawn_small_or_inline(
    what: &'static str,
    label: &'static BlockingLabel,
    f: impl FnOnce() + Send + 'static,
) {
    spawn_or_inline(Some(SMALL_STACK), what, label, f);
}

#[track_caller]
fn spawn_or_inline(
    stack: Option<usize>,
    what: &'static str,
    label: &'static BlockingLabel,
    f: impl FnOnce() + Send + 'static,
) {
    // The closure is consumed by a failed `Builder::spawn`, so the inline arm needs its own door
    // to it: both arms take it out of one cell, and whichever gets there first is the only run.
    let cell = std::sync::Arc::new(std::sync::Mutex::new(Some(f)));
    let worker = std::sync::Arc::clone(&cell);
    let take = |cell: &std::sync::Mutex<Option<_>>| cell.lock().unwrap_or_else(|e| e.into_inner()).take();
    if let Err(e) = try_spawn(what, stack, move || {
        if let Some(f) = take(&worker) {
            f();
        }
    }) {
        crate::eventlog::log(&format!(
            "task: spawn '{what}' REFUSED ({e}) — running it inline under '{}'",
            label.text()
        ));
        let _block = allow_blocking(label);
        if let Some(f) = take(&cell) {
            f();
        }
    }
}

/// [`spawn_small`], but handing back the handle so the caller can [`join`] it later. For work that
/// is fire-and-forget *during* a session yet must still be allowed to finish before the process
/// exits — the end-of-playback scrobble is the only such case, and losing it would lose the
/// server-side resume point.
pub fn spawn_small_keeping(
    what: &str,
    f: impl FnOnce() + Send + 'static,
) -> Option<JoinHandle<()>> {
    spawn_with(what, Some(SMALL_STACK), f)
}

/// A join this long parked the SDL loop for ~15 frames — the shortest stall a person reads as a
/// freeze rather than a stutter.
const STALL_MS: u64 = 250;

/// Join a worker and report what THIS thread paid for it.
///
/// The counterpart to [`spawn`], and the reason it exists rather than a bare `let _ = h.join();`:
/// the frame loop has an FPS heartbeat, `/tmp/plxnative-framedrop` catches the frames that blow the
/// budget, and `ui::profile` splits a frame by draw phase — but none of them can see a worker, and
/// every teardown stall this engine has had was the main thread parked in one of these joins with
/// no number left behind. Unconditional: an `Instant` pair around a call whose whole purpose is to
/// block cannot perturb what it measures, and gating it would hide the teardown numbers in exactly
/// the harness runs where they show up.
///
/// The number is the JOINER's wait, not the worker's lifetime. Joining a worker that finished
/// 300 ms ago costs nothing, and two of `engine::teardown`'s three joins are normally of workers
/// that have already exited — reporting their lifetimes would flag every teardown as stalling and
/// make the numbers worthless for finding the one that does.
///
/// After this, a bare `.join()` anywhere outside this module is a stall nobody can see;
/// `grep -rn '\.join()' rust-modules/src` is the enforcement, because there is no other.
///
/// **Baseline measured on device** (2026-07-29, BACK out of a direct-play movie on a healthy LAN,
/// captured by temporarily setting `STALL_MS` to 0): `demux 0ms`, `media 0ms`, `timeline 0ms` —
/// all three joins are free in the ordinary case. That is the number the teardown findings have to
/// be read against: the joins are fault-conditional, not an everyday cost, and it independently
/// confirms the condvar fix that superseded `docs/async-model-review.md` §3b's "every teardown
/// pays 0-1000 ms". It also means a single `THREADJOIN` line in a log is signal, never noise.
pub fn join(what: &str, h: JoinHandle<()>) {
    let t0 = std::time::Instant::now();
    let outcome = h.join();
    let ms = t0.elapsed().as_millis() as u64;
    if outcome.is_err() {
        // Previously swallowed by `let _ = t.join()`. A worker that died holding its socket or an
        // armed in-flight flag is the first thing worth knowing at teardown.
        crate::eventlog::log(&format!(
            "task: worker '{what}' PANICKED (joined after {ms}ms)"
        ));
    }
    if ms >= STALL_MS {
        crate::eventlog::log(&format!("THREADJOIN {what} {ms}ms STALL"));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    /// The whole point of the module: a refused spawn is a return value, not a panic. Forced with
    /// an unsatisfiable stack size, since exhausting the real thread limit is not something a
    /// host test can arrange politely.
    ///
    /// **Not `usize::MAX`**, which is the obvious spelling of "unsatisfiable" and the one this
    /// test used until 2026-08-02. `std`'s unix `Thread::new` rounds the requested stack up to a
    /// page — `stack_size + page_size - 1` — and on `usize::MAX` that addition overflows, so a
    /// debug-assertions build panics *inside std* before `pthread_create` is ever reached. The
    /// test then fails claiming the very thing it exists to prove. It is toolchain-dependent
    /// (green on 1.96 stable, red on 1.98-nightly) and this crate is nightly-only for
    /// `-Z build-std`, so `make check` was the one that saw it.
    ///
    /// Half the address space is just as unsatisfiable — no allocator anywhere will find 8 EiB —
    /// while leaving room for the page rounding to land, and it stays correct on the 32-bit
    /// target, which a literal like `1 << 60` would not.
    #[test]
    fn a_refused_spawn_reports_instead_of_panicking() {
        let h = super::spawn_with("unsatisfiable", Some(usize::MAX / 2), || unreachable!());
        assert!(
            h.is_none(),
            "a spawn that cannot succeed must report, not hand back a handle"
        );
    }

    /// [`super::MainThread`] earns its keep entirely by NOT being `Send` — that absence is what
    /// makes a closure which captured one impossible to hand to `spawn`. An absent impl is
    /// invisible to ordinary code, so detect it: the inherent const applies only when `T: Send`,
    /// and resolution falls through to the blanket trait when it doesn't. The `i32` line is there
    /// so a probe that silently answered "never Send" would fail rather than pass everything.
    #[test]
    fn the_main_thread_token_cannot_cross_a_spawn() {
        struct Probe<T>(std::marker::PhantomData<T>);
        trait NotSend {
            const SEND: bool = false;
        }
        impl<T> NotSend for Probe<T> {}
        impl<T: Send> Probe<T> {
            const SEND: bool = true;
        }

        assert!(Probe::<i32>::SEND, "the probe must see a Send type as Send");
        assert!(
            !Probe::<super::MainThread>::SEND,
            "MainThread must not be Send"
        );
        assert!(
            !Probe::<&super::MainThread>::SEND,
            "nor may a borrow of it — that is the form it is passed in"
        );
    }

    /// The token is minted in the worker and handed to it by reference, so the closure must run on
    /// a thread that is not inside a frame scope, even when the spawner is.
    #[test]
    fn spawn_off_frame_runs_its_closure_off_the_frame() {
        let _frame = super::FrameScope::enter();
        assert!(super::blocking::in_frame(), "the spawner is on the frame");
        let (tx, rx) = mpsc::channel();
        assert!(super::spawn_off_frame("off-frame probe", move |_off| {
            tx.send(super::blocking::in_frame()).unwrap();
        }));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(false));
    }

    /// Unlike `MainThread`, the token exists to be moved INTO a worker closure.
    #[test]
    fn the_off_frame_token_can_cross_a_spawn() {
        fn assert_send<T: Send>() {}
        assert_send::<super::OffFrame>();
        assert_eq!(std::mem::size_of::<super::OffFrame>(), 0, "it is a proof, not data");
        let _ = super::OffFrame::for_test();
    }

    /// The OS-refused arm: forced with an unsatisfiable stack, the work still runs, here, under
    /// the declared exception (so a frame-thread guard inside it passes), exactly once.
    #[test]
    fn a_refused_spawn_runs_inline_under_its_exception() {
        let _frame = super::FrameScope::enter();
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = ran.clone();
        super::spawn_or_inline(
            Some(usize::MAX / 2),
            "unsatisfiable",
            const { &super::BlockingLabel::new("inline fixture (worker thread refused)") },
            move || {
                let _guard = super::assert_may_block(const { &super::BlockingLabel::new("inside the inline fallback") });
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        );
        assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
        // ...and the exception ended with the call.
        assert!(std::panic::catch_unwind(|| super::assert_may_block(const { &super::BlockingLabel::new("after") })).is_err());
    }

    /// The ordinary arm: the work runs on a worker, once, and never on the caller's thread.
    #[test]
    fn spawn_small_or_inline_runs_on_a_worker_when_it_can() {
        let (tx, rx) = mpsc::channel();
        let caller = std::thread::current().id();
        super::spawn_small_or_inline(
            "worker probe",
            const { &super::BlockingLabel::new("worker probe (worker thread refused)") },
            move || tx.send(std::thread::current().id()).unwrap(),
        );
        let ran_on = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_ne!(ran_on, caller);
        assert!(rx.recv_timeout(std::time::Duration::from_millis(50)).is_err(), "exactly once");
    }

    #[test]
    fn a_spawned_worker_actually_runs() {
        let (tx, rx) = mpsc::channel();
        assert!(super::spawn_small("probe", move || tx.send(7).unwrap()));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(7));
    }
}
