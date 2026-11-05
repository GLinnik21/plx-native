//! How the app process ends: [`exit_now`], the one `-> !` every production exit goes through.
//!
//! libc's `exit` (which `std::process::exit` calls, as does returning from `main`) runs the
//! `atexit` handlers, among them OpenSSL's `OPENSSL_cleanup`. That frees and NULLs libcrypto's
//! global locks while the detached curl workers (notification feed, metadata, watchlist, auth
//! probes) are still running; their next `CRYPTO_THREAD_*_lock` is `pthread_rwlock_*lock(NULL)`, a
//! SIGSEGV at 0x1c on a clean exit (#1115). It also runs the firmware libraries' C++ finalizers
//! under a worker still inside them. `_exit` ends every thread at once and runs none of it.
//!
//! In the TV executable libc's `exit` itself is interposed (`src/exit.c`), so an `exit` made by a
//! firmware or system library on a thread of its own ends the process through
//! [`plx_exit_intercepted`] and [`exit_now`] as well.

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

/// **The one function `src/exit.c` calls: an `exit()` made by a shared library ends the process
/// like every other exit** (Sentry PLX-NATIVE-1P).
///
/// The executable defines libc's `exit` itself and the link exports it
/// (`-Wl,--export-dynamic-symbol=exit`), so the `exit` that every other shared object calls binds
/// to that definition. It lands here with the caller's real `status`; one event-log line names the
/// calling thread, and [`exit_now`] ends the process: no `atexit` handler and no finalizer runs.
///
/// What the event shows (0.9.0, webOS TV Lite 4.10, board k5lp, firmware `p.649.grampians.k5lp.1`):
/// the breadcrumbs run `will_background {playing:true}`, `wayland_released` (+9 ms),
/// `native_unload` / `acb_plane_unload` / `native_destroyed` (about 1.2 s later), and the crash comes
/// about 160 ms after that. The crashed thread is SDL's own `SDLTimer` thread (the app only calls
/// `SDL_Init(SDL_INIT_VIDEO)` and adds no SDL timer), created late, next to the telemetry worker; its
/// stack is `__run_exit_handlers` and then `<unknown>`, an unsymbolized exit handler that took
/// SIGSEGV. The main thread was in `run::present_and_swap`, in `__nanosleep`. Which handler crashed,
/// and why the platform's SDL called `exit`, is not determined.
///
/// **Why an interposer and not an `atexit` handler.** A handler registered at boot runs after
/// everything registered later (such as a finalizer a library installs while the playback unload
/// that follows the background edge runs); an `atexit` handler is also not told the status, and
/// runs after `exit` has run the thread-local destructors (`__call_tls_dtors`). Interposing `exit`
/// is independent of when any finalizer registered, keeps the status, and acts before any of it.
///
/// **Not covered:** glibc's own callers of `exit` (`error(3)` and `err(3)` with a nonzero status,
/// `__libc_start_main` after `main` returns) reach libc's hidden alias, not the PLT, so they are not
/// interposed. The app's own exits all go through [`exit_now`] already.
///
/// This can run on a thread this app did not create, after that thread's thread-local destructors
/// ran, or before `main`, so it must not panic and must not use `std::thread::current()` or any
/// `thread_local`; `caller_thread` reads the thread's identity from the kernel for that reason.
#[no_mangle]
pub extern "C" fn plx_exit_intercepted(status: c_int) -> ! {
    crate::eventlog::log(&format!(
        "exit: libc exit({status}) on {}; ending with _exit, no exit handlers run",
        caller_thread()
    ));
    exit_now(status)
}

/// `tid=<n> name=<comm>` of the calling thread, best effort, with no filesystem and no thread-local.
fn caller_thread() -> String {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `gettid` takes no arguments and cannot fail.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        // `PR_GET_NAME` writes at most 16 bytes (the 15-byte comm and its NUL); the buffer is zeroed,
        // so a failed call leaves an empty name. The same pair `src/crashtrace.c` uses.
        let mut buf = [0u8; 16];
        // SAFETY: `buf` is a valid, writable 16-byte buffer for the whole call.
        unsafe {
            libc::prctl(
                libc::PR_GET_NAME,
                buf.as_mut_ptr() as libc::c_ulong,
                0,
                0,
                0,
            )
        };
        let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        format!("tid={tid} name={}", String::from_utf8_lossy(&buf[..len]))
    }
    #[cfg(not(target_os = "linux"))]
    {
        "tid=? name=?".to_string()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::caller_thread;

    #[test]
    fn caller_thread_names_the_calling_thread_and_its_own_tid() {
        let seen = std::thread::Builder::new()
            .name("exit-probe".into())
            .spawn(caller_thread)
            .expect("spawn")
            .join()
            .expect("join");
        assert!(seen.contains("name=exit-probe"), "{seen}");
        let tid: u32 = seen
            .strip_prefix("tid=")
            .and_then(|rest| rest.split(' ').next())
            .and_then(|tid| tid.parse().ok())
            .unwrap_or_else(|| panic!("no tid in {seen:?}"));
        assert_ne!(
            tid,
            std::process::id(),
            "a spawned thread has a tid of its own: {seen}"
        );
    }
}
