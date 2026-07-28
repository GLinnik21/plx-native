//! Spawning background work — all of it.
//!
//! `std::thread::spawn` **panics** when the OS refuses the thread — it unwraps `Builder::spawn`,
//! whose `Err` is `pthread_create`'s EAGAIN (glibc returns it when `allocate_stack` cannot `mmap`
//! the stack, or `clone(2)` hits `RLIMIT_NPROC`/`threads-max`). Almost every worker here is
//! started from the SDL loop, so that panic unwinds out of `plex_run` through the C shim and
//! takes the app down. It would also fail at the worst possible moment: the caller has just armed
//! an in-flight flag, so had the app survived, the screen would sit on a spinner that can never
//! resolve.
//!
//! **Measured on the target (2026-07-28), because the first version of this note guessed:** the
//! app runs 31 threads at playback peak (13 at Home) against an `RLIMIT_NPROC` of 3746 for its
//! uid and a system `threads-max` of 7492; `VmSize` peaks at 363 MB of a ~3 GB 32-bit user space;
//! `vm.overcommit_memory` is 0, so a 2 MB stack `mmap` effectively cannot fail with ~430 MB
//! available. **EAGAIN is not a live risk on this hardware** — roughly 120x headroom on threads.
//! This module is not buying us a fix for something that happens; it is refusing to let an
//! impossible-but-fatal branch exist, at the cost of a return value. `Max open files` (1024 soft)
//! is the only limit anywhere near reach, and it is not this one.
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
use std::thread::{Builder, JoinHandle};

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

    #[test]
    fn a_spawned_worker_actually_runs() {
        let (tx, rx) = mpsc::channel();
        assert!(super::spawn_small("probe", move || tx.send(7).unwrap()));
        assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(5)), Ok(7));
    }
}
