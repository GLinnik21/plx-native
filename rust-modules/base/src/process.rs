//! How the app process ends: [`exit_now`], the one `-> !` every production exit goes through.
//!
//! libc's `exit` (which `std::process::exit` calls, as does returning from `main`) runs the
//! `atexit` handlers, among them OpenSSL's `OPENSSL_cleanup`. That frees and NULLs libcrypto's
//! global locks while the detached curl workers (notification feed, metadata, watchlist, auth
//! probes) are still running; their next `CRYPTO_THREAD_*_lock` is `pthread_rwlock_*lock(NULL)`, a
//! SIGSEGV at 0x1c on a clean exit (#1115). It also runs the firmware libraries' C++ finalizers
//! under a worker still inside them. `_exit` ends every thread at once and runs none of it.

use crate::task::watchdog::CrashPhase;
use std::os::raw::c_int;

/// End the process with `code` without running `atexit` handlers or shared-library destructors.
///
/// What `exit` would have flushed is flushed by hand first: Rust's stdout and stderr, and every
/// open C output stream (`fflush(NULL)`). The event log is appended unbuffered per line, so it
/// needs nothing; persistent stores must have saved before this is called.
///
/// The crash tracer's phase becomes [`CrashPhase::Exiting`] first, so every way out of the process
/// (a played session, an early-boot refusal, the simulators) labels a fault in these last
/// flushes the same way: the teardown is over and the process is ending.
pub fn exit_now(code: c_int) -> ! {
    crate::task::watchdog::set_crash_phase(CrashPhase::Exiting);
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `fflush(NULL)` flushes every open C output stream and takes no pointer of ours;
    // `_exit` is async-signal-safe and touches no Rust or C++ state, so no destructor or `atexit`
    // handler runs under a live worker.
    unsafe {
        libc::fflush(std::ptr::null_mut());
        libc::_exit(code)
    }
}

/// **Pre-empt libc `exit` from a thread this app does not own** (PLX-NATIVE-1P).
///
/// Every exit the app makes goes through [`exit_now`], but a firmware or system library thread can
/// still call libc `exit` on its own: measured on a webOS 4 Lite set (k5lp, `p.649`), a session
/// backgrounded mid-playback was ended that way while the main thread was still inside SDL, and
/// `__run_exit_handlers` ran a library finalizer that tore down the Wayland client state SDL was
/// still using — SIGSEGV in `libwayland-client`. Same class as #1115, reached from outside.
///
/// `atexit` handlers run last-registered-first, so a handler registered AFTER the libraries that
/// install finalizers runs before all of them (and before `_dl_fini`, which ld.so registers before
/// `main`), and ends the process with `_exit` so none of them runs. A library loaded later registers
/// after this one, so callers re-arm at the points where that matters (boot after the GL context and
/// the video plane, and each background edge); a duplicate registration is harmless because the
/// handler never returns. The `exit` status is not visible to an `atexit` handler, so the process
/// ends with 0 — the caller of `exit` was ending a session the system had already decided to close.
///
/// Only the app process may arm this: a host test binary reports failure through `exit(101)`, which
/// this would turn into 0. Call sites are gated off under `test` and `hostsim`.
pub fn arm_exit_guard() {
    // SAFETY: `atexit` stores a plain `extern "C"` function pointer; it takes no data of ours.
    let rc = unsafe { libc::atexit(intercept_exit) };
    if rc != 0 {
        crate::eventlog::log("exit guard: atexit registration failed; a foreign exit() runs library finalizers");
    }
}

/// The handler [`arm_exit_guard`] registers: say who called `exit`, then [`exit_now`].
extern "C" fn intercept_exit() {
    crate::eventlog::log(&format!("exit guard: libc exit() intercepted on {}; ending with _exit", caller_thread()));
    exit_now(0)
}

/// `tid=<n> name=<comm>` of the thread running the exit handlers, best effort.
fn caller_thread() -> String {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `gettid` takes no arguments and cannot fail.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        let name = std::fs::read_to_string("/proc/thread-self/comm").unwrap_or_default();
        format!("tid={tid} name={}", name.trim())
    }
    #[cfg(not(target_os = "linux"))]
    {
        format!("thread={:?}", std::thread::current().name())
    }
}

#[cfg(all(test, unix))]
mod tests {
    /// Run `body` in a forked child and return its exit status (or `None` on a signal).
    fn in_child(body: impl FnOnce()) -> Option<i32> {
        // SAFETY: the child only registers handlers and calls `exit`/`_exit`; it never returns
        // into the test harness.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0, "fork");
            if pid == 0 {
                body();
                libc::_exit(77);
            }
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            libc::WIFEXITED(status).then(|| libc::WEXITSTATUS(status))
        }
    }

    extern "C" fn sentinel() {
        // Would only run if the guard let the remaining exit handlers through.
        unsafe { libc::_exit(42) }
    }

    #[test]
    fn a_foreign_exit_skips_every_handler_registered_before_the_guard() {
        let status = in_child(|| unsafe {
            libc::atexit(sentinel); // a "library finalizer", registered first
            super::arm_exit_guard();
            libc::exit(3);
        });
        assert_eq!(status, Some(0), "the guard ends the process with _exit(0) before the sentinel");
    }

    #[test]
    fn without_the_guard_the_sentinel_runs() {
        let status = in_child(|| unsafe {
            libc::atexit(sentinel);
            libc::exit(3);
        });
        assert_eq!(status, Some(42));
    }
}
