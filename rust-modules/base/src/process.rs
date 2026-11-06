//! How the app process ends: [`exit_now`], the one `-> !` every production exit of the app's own
//! code goes through. (A library's `exit()` ends the process through `src/exit.c` and
//! `plx_exit_record`, below.)
//!
//! libc's `exit` (which `std::process::exit` calls, as does returning from `main`) runs the
//! `atexit` handlers, among them OpenSSL's `OPENSSL_cleanup`. That frees and NULLs libcrypto's
//! global locks while the detached curl workers (notification feed, metadata, watchlist, auth
//! probes) are still running; their next `CRYPTO_THREAD_*_lock` is `pthread_rwlock_*lock(NULL)`, a
//! SIGSEGV at 0x1c on a clean exit (#1115). It also runs the firmware libraries' C++ finalizers
//! under a worker still inside them. `_exit` ends every thread at once and runs none of it.
//!
//! In the TV executable libc's `exit` itself is interposed (`src/exit.c`), so an `exit` made by a
//! firmware or system library on a thread of its own ends the process the same way: with no
//! handler (the `exit_group` syscall). That path is `plx_exit_record` in `src/crashtrace.c`, not [`exit_now`], because
//! `exit` can be reached from a signal handler and everything it does must be async-signal-safe,
//! and [`exit_now`] takes Rust's stdout and stderr locks and calls `fflush(NULL)`, which are not.

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
