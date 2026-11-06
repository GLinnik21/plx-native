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
use std::os::raw::{c_int, c_void};

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
/// to that definition. It lands here with the caller's real `status` and the return address of the
/// `exit` call; one event-log line names the calling thread and the caller of `exit` (the shared
/// object and the return address's offset in it, [`describe_caller`]), and [`exit_now`] ends the
/// process: no `atexit` handler and no finalizer runs. The caller is logged because the Sentry event
/// has no frame above `__run_exit_handlers`, so which library called `exit` is otherwise unknown.
///
/// The firmware's own SDL2 imports `exit@GLIBC_2.4` through an `R_ARM_JUMP_SLOT` on the inventoried
/// images (webOS 4.x, 6.x, 9.2, 11.2), so an `exit` SDL makes binds here; whether PLX-NATIVE-1P's
/// call came through a PLT at all is not determined, the event has no frame above
/// `__run_exit_handlers`.
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
/// **Not covered:** libc.so.6's own internal callers of `exit` (`error(3)` and `err(3)` with a
/// nonzero status, `__libc_start_main` after `main` returns) call libc's `exit` directly, not
/// through the PLT, so they still run the handlers. libpthread's last-thread `exit(0)` on glibc
/// <= 2.31 (before libpthread merged into libc) does go through its PLT and is interposed, which is
/// harmless: the app never ends its main thread with `pthread_exit`. Every exit of the app session
/// itself goes through [`exit_now`]; `src/main.c` returns from `main` only on the Sentry-spool
/// re-entry, which runs libc's own `exit` as before.
///
/// This can run on a thread this app did not create, after that thread's thread-local destructors
/// ran, or before `main`, so it must not panic and must not use `std::thread::current()` or any
/// `thread_local`; `caller_thread` reads the thread's identity from the kernel for that reason.
#[no_mangle]
pub extern "C" fn plx_exit_intercepted(status: c_int, caller: *const c_void) -> ! {
    crate::eventlog::log(&format!(
        "exit: libc exit({status}) on {}, {}; ending with _exit, no exit handlers run",
        caller_thread(),
        describe_caller(caller)
    ));
    exit_now(status)
}

/// `caller=<object>+0x<offset>` for the return address of an `exit` call, plus
/// ` (near <symbol>+0x<offset>)` when the loader knows a symbol at or below it. `<offset>` is the
/// raw return address minus the object's load base: the number to look up in that library
/// (`addr2line`, `objdump -d`) after backing up the 1-4 bytes of the call instruction. The firmware's
/// libraries are stripped, so the symbol is the nearest exported one, not necessarily the function
/// that made the call. An address in no loaded object reads `caller=0x<addr> (no shared object)`.
///
/// `dladdr` gets the byte BEFORE the return address: `exit` never returns, so a call to it can be the
/// last instruction of a function, and the return address is then one past that function's end, in
/// the next symbol.
fn describe_caller(caller: *const c_void) -> String {
    #[cfg(unix)]
    {
        use std::ffi::CStr;
        use std::fmt::Write;

        // A return address into Thumb code carries the Thumb bit in bit 0; no instruction address
        // is odd, so it is cleared before it is used as one.
        #[cfg(target_arch = "arm")]
        let caller = (caller as usize & !1) as *const c_void;
        // SAFETY: `Dl_info` is plain data (pointers and no invariants), valid when zeroed.
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        // SAFETY: `dladdr` only reads the address it is given, which is never dereferenced, and
        // writes into `info`.
        let known = !caller.is_null()
            && unsafe { libc::dladdr(caller.wrapping_byte_sub(1), &mut info) } != 0;
        if !known {
            return format!("caller={caller:p} (no shared object)");
        }
        let at = caller as usize;
        // SAFETY: on success `dladdr` sets each string pointer to a NUL-terminated string that
        // lives as long as the object stays loaded, or to null; null is handled.
        let name = |ptr: *const libc::c_char| {
            (!ptr.is_null()).then(|| {
                unsafe { CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .into_owned()
            })
        };
        let object = name(info.dli_fname).unwrap_or_else(|| "?".to_string());
        let mut out = format!(
            "caller={object}+{:#x}",
            at.wrapping_sub(info.dli_fbase as usize)
        );
        if let Some(symbol) = name(info.dli_sname) {
            let _ = write!(
                out,
                " (near {symbol}+{:#x})",
                at.wrapping_sub(info.dli_saddr as usize)
            );
        }
        out
    }
    #[cfg(not(unix))]
    {
        let _ = caller;
        "caller=?".to_string()
    }
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

#[cfg(all(test, unix))]
mod tests {
    use super::describe_caller;
    use std::os::raw::c_void;

    #[cfg(target_os = "linux")]
    #[test]
    fn caller_thread_names_the_calling_thread_and_its_own_tid() {
        let seen = std::thread::Builder::new()
            .name("exit-probe".into())
            .spawn(super::caller_thread)
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

    #[test]
    fn describe_caller_names_the_object_and_offset_of_an_address_in_libc() {
        let inside_libc = (libc::getpid as *const c_void).wrapping_byte_add(4);
        let seen = describe_caller(inside_libc).to_lowercase();
        assert!(seen.starts_with("caller="), "{seen}");
        // Linux resolves `getpid` into libc.so.6, macOS into libsystem_kernel.dylib.
        assert!(
            seen.contains("libc.so") || seen.contains("libsystem"),
            "{seen}"
        );
        assert!(seen.contains("+0x"), "{seen}");
        assert!(seen.contains("(near "), "{seen}");
    }

    #[test]
    fn describe_caller_without_an_address_says_no_shared_object() {
        let seen = describe_caller(std::ptr::null());
        assert!(seen.contains("no shared object"), "{seen}");
    }
}
