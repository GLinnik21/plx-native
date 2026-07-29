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

use std::io;
use std::marker::PhantomData;
use std::thread::{Builder, JoinHandle};

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
/// * **the `ENGINE` slot** — `player::engine::engine()`. `static mut`, handed out as
///   `&'static mut`, with worker threads holding raw pointers into the boxes it owns. Two live
///   `&mut` to it is instant UB, and nothing else prevents that.
///
/// The one deliberate hole: `assume` is callable, so `unsafe { MainThread::assume() }` inside a
/// worker would defeat this. That is the ceiling of the pattern, not an oversight — what it buys
/// is that the mistake has to be *written*, in an `unsafe` block, instead of happening by
/// forgetting a convention documented in three other files.
pub(crate) struct MainThread(PhantomData<*const ()>);

impl MainThread {
    /// Mint the token. `plex_run` calls this once, at the top, and nothing else should.
    ///
    /// # Safety
    /// The caller asserts this is the SDL main thread. It is not a memory-safety obligation in
    /// itself — it is the premise every `&MainThread` downstream is trusted on, including the
    /// aliasing of `ENGINE`, so a false one reintroduces exactly the races this prevents.
    pub(crate) unsafe fn assume() -> Self {
        MainThread(PhantomData)
    }
}

/// Stack for the short network workers — one HTTP round trip and a decode. The platform default
/// (2 MB) is pure reserved address space on a 32-bit target, and several of these run at once.
/// Deliberately NOT used for the demux/decode/encode threads: libavformat wants a real stack.
const SMALL_STACK: usize = 256 * 1024;

fn refused(what: &str, e: &io::Error) {
    crate::log(&format!("task: spawn '{what}' REFUSED ({e}) — this work is dropped"));
}

fn spawn_with(what: &str, stack: Option<usize>, f: impl FnOnce() + Send + 'static)
    -> Option<JoinHandle<()>> {
    let mut b = Builder::new();
    if let Some(n) = stack {
        b = b.stack_size(n);
    }
    match b.spawn(f) {
        Ok(h) => Some(h),
        Err(e) => {
            refused(what, &e);
            None
        }
    }
}

/// Spawn `f` and keep the handle. `None` means the OS refused the thread and **the worker does
/// not exist** — undo whatever was armed for it. `what` names the work in the failure log.
pub(crate) fn spawn(what: &str, f: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
    spawn_with(what, None, f)
}

/// [`spawn`] on a [`SMALL_STACK`], for the fire-and-forget network workers. `false` = not spawned.
pub(crate) fn spawn_small(what: &str, f: impl FnOnce() + Send + 'static) -> bool {
    spawn_with(what, Some(SMALL_STACK), f).is_some()
}

/// [`spawn_small`], but handing back the handle so the caller can [`join`] it later. For work that
/// is fire-and-forget *during* a session yet must still be allowed to finish before the process
/// exits — the end-of-playback scrobble is the only such case, and losing it would lose the
/// server-side resume point.
pub(crate) fn spawn_small_keeping(what: &str, f: impl FnOnce() + Send + 'static) -> Option<JoinHandle<()>> {
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
pub(crate) fn join(what: &str, h: JoinHandle<()>) {
    let t0 = std::time::Instant::now();
    let outcome = h.join();
    let ms = t0.elapsed().as_millis() as u64;
    if outcome.is_err() {
        // Previously swallowed by `let _ = t.join()`. A worker that died holding its socket or an
        // armed in-flight flag is the first thing worth knowing at teardown.
        crate::log(&format!("task: worker '{what}' PANICKED (joined after {ms}ms)"));
    }
    if ms >= STALL_MS {
        crate::log(&format!("THREADJOIN {what} {ms}ms STALL"));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    /// The whole point of the module: a refused spawn is a return value, not a panic. Forced with
    /// an unsatisfiable stack size, since exhausting the real thread limit is not something a
    /// host test can arrange politely.
    #[test]
    fn a_refused_spawn_reports_instead_of_panicking() {
        let h = super::spawn_with("unsatisfiable", Some(usize::MAX), || unreachable!());
        assert!(h.is_none(), "a spawn that cannot succeed must report, not hand back a handle");
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
        assert!(!Probe::<super::MainThread>::SEND, "MainThread must not be Send");
        assert!(!Probe::<&super::MainThread>::SEND, "nor may a borrow of it — that is the form it is passed in");
    }

    #[test]
    fn a_spawned_worker_actually_runs() {
        let (tx, rx) = mpsc::channel();
        assert!(super::spawn_small("probe", move || tx.send(7).unwrap()));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(7));
    }
}
