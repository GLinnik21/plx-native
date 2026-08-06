//! Bind a TV library at RUNTIME instead of at link time, by SONAME candidate list.
//!
//! # Why this exists
//!
//! Everything the app links names its library in `DT_NEEDED`, and the dynamic loader treats a
//! missing entry as fatal: the process dies at `exec()`, before `main`, before the event log is
//! open. That is exactly the failure a webOS 5 television gives today — `libAcbAPI.so.1` was
//! deleted at 5.0 and every FFmpeg SONAME moved a major — and it presents to the user as "the
//! app does nothing", with no log line to read and nothing to report.
//!
//! `DT_NEEDED` also cannot express "either of these". One binary cannot depend on
//! `libavformat.so.57` *and* `libavformat.so.58`; naming both means both must be present, which
//! is true on no firmware ever shipped. So a single binary that runs on both eras has to resolve
//! those libraries itself. That is all this module does.
//!
//! # What it is not
//!
//! It is not a general dynamic-loading framework and it is not for libraries that exist
//! everywhere. `libSDL2`, `libGLESv2`, `libglib-2.0`, `libluna-service2`, `libwayland-client`,
//! `libplayerAPIs` and `libpf-1.0` are present with the same SONAME on every release from 4.4.2
//! through 11.2.0 (`tools/fwcompat.py --inventory` will show you), so they stay linked normally
//! and keep real link-time symbol checking. Moving a library here TRADES that checking away for
//! version tolerance, and is only worth it where the version actually varies.
//!
//! # The contract
//!
//! [`dynlib!`] takes a block shaped exactly like the `extern "C"` block it replaces and emits an
//! `unsafe fn` of the same name and signature for each entry, so call sites do not change. The
//! generated `load()` is all-or-nothing over the REQUIRED symbols: it resolves every one or it
//! reports which are missing and leaves the table empty. Callers must gate on that result —
//! `ff::boot()` does, and `ff::demux()` refuses when it failed.
//!
//! A trailing `optional { … }` section holds symbols the library may legitimately drop between
//! majors while staying perfectly usable. Their absence neither fails the load nor logs an error,
//! and the wrapper does nothing when called. They must return `()`: a symbol whose absence needs a
//! fallback VALUE is not optional, it is a branch the caller has to make and see. There is exactly
//! one today — `av_register_all`, a no-op since FFmpeg 4.0 and deleted in 5.0 — and getting that
//! wrong made the app refuse to demux on webOS 10.2.0 and 11.2.0 for a symbol it does not need.
//!
//! Calling a wrapper whose symbol never resolved is a bug in that gating, not a recoverable
//! condition, so it logs the symbol name and panics rather than jumping through a null pointer.
//! The alternative — returning a per-signature sentinel — would let a missing symbol travel as a
//! plausible-looking value into the media pipeline, which is the harder failure to diagnose of
//! the two, on a device where diagnosis means reading a log over ssh.
#![allow(dead_code)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicPtr, Ordering};

extern "C" {
    fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
}

/// `RTLD_NOW | RTLD_GLOBAL`. NOW because a lazily-bound miss would surface as a jump into the
/// resolver at an arbitrary later moment — the whole point here is to learn at load time. GLOBAL
/// because these libraries satisfy each other's imports (libavformat needs libavcodec needs
/// libavutil) and because the ACB path wants its symbols visible to anything loaded after it.
const RTLD_NOW_GLOBAL: c_int = 0x00002 | 0x00100;

/// An opened library. Not `Drop` — nothing here is ever closed, deliberately: the media libraries
/// stay mapped for the life of the process, and `dlclose` on a library with live pipeline threads
/// inside it is a way to unmap code that is executing.
#[derive(Clone, Copy)]
pub struct Handle(*mut c_void);
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    /// Try each SONAME in order, returning the first that opens **and which one it was**.
    ///
    /// Order is by preference, not by age: put the SONAME you expect on the firmware you most
    /// care about first. It matters only when a device carries both, which does happen —
    /// releases 5.3.1 and 6.4.0 keep a `libcurl.so.5` compat alias beside the real
    /// `libcurl.so.4`, so a list that asked for `.so.4` first would still be answered on a set
    /// where `.so.5` also resolves.
    ///
    /// The name comes back with the handle rather than being recovered by a second pass, because
    /// a second pass means calling `dlopen` again — which bumps the library's reference count and
    /// costs a real open on every candidate that was skipped.
    pub fn open<'a>(candidates: &[&'a str]) -> Option<(Handle, &'a str)> {
        Self::open_in(None, candidates)
    }

    /// As [`open`], but resolving names inside `dir` when one is given.
    ///
    /// `dir` is how the BUNDLED FFmpeg is found: those libraries live beside the binary, which is
    /// on no library search path, so a bare SONAME would either fail or — worse, on webOS 11,
    /// which ships FFmpeg 6 itself — silently open the television's copy instead of ours. An
    /// absolute path makes "which library did we get" structural rather than a matter of search
    /// order. (The `-plx` build suffix is the second half of that guarantee.)
    pub fn open_in<'a>(dir: Option<&str>, candidates: &[&'a str]) -> Option<(Handle, &'a str)> {
        for name in candidates {
            let path = match dir {
                Some(d) => format!("{d}/{name}"),
                None => (*name).to_string(),
            };
            let Ok(c) = CString::new(path) else { continue };
            let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW_GLOBAL) };
            if !h.is_null() {
                return Some((Handle(h), name));
            }
        }
        None
    }

    /// A handle to the process's own global symbol scope (`RTLD_DEFAULT`), for asking "did the
    /// library that actually loaded bring this entry point" about something already linked.
    ///
    /// This is how the webOS-version splits are detected without linking either side: SDL's
    /// exported-window family exists only from webOS 5.0, `SDL_webOSGetPanelResolution` only from
    /// 4.4.2, and naming either at link time would make the binary demand a symbol that older
    /// televisions do not have.
    pub fn self_handle() -> Handle {
        Handle(std::ptr::null_mut()) // RTLD_DEFAULT
    }

    pub fn sym(&self, name: &str) -> Option<*mut c_void> {
        let c = CString::new(name).ok()?;
        let p = unsafe {
            dlerror(); // clear any stale error so a NULL-valued symbol is distinguishable
            dlsym(self.0, c.as_ptr())
        };
        if p.is_null() && !unsafe { dlerror() }.is_null() {
            None
        } else {
            Some(p)
        }
    }
}

/// What a `load()` did. `Ok` means every symbol resolved and the table is live.
pub enum Loaded {
    Ok(&'static str),
    /// None of the candidate SONAMEs opened. Carries the list that was tried.
    NoLibrary,
    /// The library opened but is missing symbols. Carries the SONAME and how many are absent;
    /// the names themselves go to the event log, because a caller only ever needs the count.
    Incomplete(&'static str, usize),
}

impl Loaded {
    pub fn ok(&self) -> bool {
        matches!(self, Loaded::Ok(_))
    }
}

/// The panic path for a wrapper whose symbol never resolved. Cold, and it names the symbol,
/// because the whole value of failing here is that the log says which one.
#[cold]
#[inline(never)]
pub fn missing_symbol(lib: &str, sym: &str) -> ! {
    crate::log(&format!(
        "dynlib: FATAL — {sym} was called but never resolved from {lib}; the load gate was skipped"
    ));
    panic!("dynlib: {lib}:{sym} unresolved");
}

/// Resolve `names` from the first candidate SONAME that opens, into `cells`.
///
/// Split out of the macro so the logic exists once rather than once per library: a macro that
/// expands a loop body five times is five copies to keep in step.
/// `optional` entries are resolved and published if present, and their absence is neither logged
/// as an error nor counted against the load. That is for symbols the library legitimately drops
/// between majors while remaining perfectly usable — `av_register_all` is the whole population:
/// deprecated in FFmpeg 4.0, a no-op ever since, deleted in 5.0. Treating it as required made the
/// app refuse to demux on webOS 10.2.0 and 11.2.0 for a symbol it does not need, while reporting
/// the wrong reason.
///
/// An optional wrapper must return `()`. A symbol whose absence needs a fallback VALUE is not
/// optional — it is a branch the caller has to make and see.
pub fn load_into(
    dir: Option<&str>,
    candidates: &'static [&'static str],
    required: &[(&'static str, &AtomicPtr<c_void>)],
    optional: &[(&'static str, &AtomicPtr<c_void>)],
) -> Loaded {
    let Some((h, soname)) = Handle::open_in(dir, candidates) else {
        return Loaded::NoLibrary;
    };
    let mut missing = 0usize;
    let mut resolved = Vec::with_capacity(required.len());
    for (name, _) in required {
        match h.sym(name) {
            Some(p) if !p.is_null() => resolved.push(p),
            _ => {
                missing += 1;
                crate::log(&format!("dynlib: {soname} has no symbol {name}"));
                resolved.push(null_mut());
            }
        }
    }
    if missing > 0 {
        return Loaded::Incomplete(soname, missing);
    }
    // Publish only once every REQUIRED symbol is known good, so a partial table is never
    // observable. Optional cells stay null when absent, and their wrappers return without calling.
    for ((_, cell), p) in required.iter().zip(resolved) {
        cell.store(p, Ordering::Release);
    }
    for (name, cell) in optional {
        if let Some(p) = h.sym(name) {
            cell.store(p, Ordering::Release);
        }
    }
    Loaded::Ok(soname)
}

/// Declare a runtime-bound library. The body is shaped like the `extern "C"` block it replaces.
///
/// ```ignore
/// dynlib! {
///     /// doc comment lands on the generated module
///     avformat: ["libavformat.so.58", "libavformat.so.57"] {
///         fn av_read_frame(s: *mut AVFormatContext, pkt: *mut AVPacket) -> c_int;
///     }
/// }
/// ```
/// emits `mod avformat { pub fn load() -> Loaded; }` plus a module-level
/// `unsafe fn av_read_frame(...) -> c_int` that dispatches through the resolved pointer.
#[macro_export]
macro_rules! dynlib {
    (
        $(#[$meta:meta])*
        $modname:ident : [ $($cand:literal),+ $(,)? ] {
            $(
                $(#[$fmeta:meta])*
                fn $fname:ident ( $($arg:ident : $argty:ty),* $(,)? ) $(-> $ret:ty)? ;
            )*
        }
        $(
            // Symbols the library may legitimately not have on some majors. Must return `()`;
            // see `load_into`. Absent -> the wrapper does nothing, and the load still succeeds.
            optional {
                $(
                    $(#[$ometa:meta])*
                    fn $oname:ident ( $($oarg:ident : $oargty:ty),* $(,)? ) ;
                )*
            }
        )?
    ) => {
        $(#[$meta])*
        #[allow(non_upper_case_globals)]
        pub(crate) mod $modname {
            use std::os::raw::c_void;
            use std::sync::atomic::AtomicPtr;

            pub(crate) const CANDIDATES: &[&str] = &[$($cand),+];
            $( pub(crate) static $fname: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut()); )*
            $($( pub(crate) static $oname: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut()); )*)?

            /// Open the first candidate SONAME that exists and resolve every required symbol.
            /// `dir` scopes the search to one directory — see `Handle::open_in`.
            pub(crate) fn load(dir: Option<&str>) -> $crate::dynlib::Loaded {
                $crate::dynlib::load_into(
                    dir,
                    CANDIDATES,
                    &[$((stringify!($fname), &$fname)),*],
                    &[$($((stringify!($oname), &$oname)),*)?],
                )
            }
        }

        $(
            $(#[$fmeta])*
            #[inline]
            // The C symbol name IS the contract — `sws_getContext` cannot be spelled any other
            // way. As an `extern` block these were exempt from the style lint; as generated Rust
            // functions they are not.
            #[allow(non_snake_case)]
            pub(crate) unsafe fn $fname ( $($arg : $argty),* ) $(-> $ret)? {
                let p = $modname::$fname.load(std::sync::atomic::Ordering::Acquire);
                if p.is_null() {
                    $crate::dynlib::missing_symbol($modname::CANDIDATES[0], stringify!($fname));
                }
                let f: extern "C" fn($($argty),*) $(-> $ret)? = std::mem::transmute(p);
                f($($arg),*)
            }
        )*

        $($(
            $(#[$ometa])*
            #[inline]
            #[allow(non_snake_case)]
            pub(crate) unsafe fn $oname ( $($oarg : $oargty),* ) {
                let p = $modname::$oname.load(std::sync::atomic::Ordering::Acquire);
                if !p.is_null() {
                    let f: extern "C" fn($($oargty),*) = std::mem::transmute(p);
                    f($($oarg),*);
                }
            }
        )*)?
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's own C library, so these tests need no fixture and run wherever `cargo test`
    /// does. They grade the LOADER, not FFmpeg — the dev machine has neither libavformat nor a
    /// television, which is exactly why the loader is the part worth testing here.
    #[cfg(target_os = "macos")]
    static HOST_LIBC: &[&str] = &["libSystem.B.dylib"];
    #[cfg(not(target_os = "macos"))]
    static HOST_LIBC: &[&str] = &["libc.so.6"];

    /// A SONAME that exists on no platform must report `NoLibrary` rather than opening something.
    /// Cheap, but it is the branch every non-TV host takes, including this test runner.
    #[test]
    fn absent_library_is_reported_not_opened() {
        assert!(Handle::open(&["libplxnative-does-not-exist.so.99"]).is_none());
    }

    /// `load_into` on an absent library must report `NoLibrary` and leave every cell null, so a
    /// caller that ignores the verdict crashes at the wrapper rather than calling a stale pointer.
    #[test]
    fn a_failed_load_publishes_nothing() {
        static A: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        static B: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        static CAND: &[&str] = &["libplxnative-nope.so.99"];
        let v = load_into(None, CAND, &[("x", &A), ("y", &B)], &[]);
        assert!(matches!(v, Loaded::NoLibrary));
        assert!(A.load(Ordering::Acquire).is_null() && B.load(Ordering::Acquire).is_null());
    }

    /// The all-or-nothing rule: one missing symbol out of two must publish NEITHER. A partial
    /// table is the state where `ff::boot` says "not usable" while a wrapper happily dispatches.
    #[test]
    fn a_partial_load_publishes_nothing() {
        static A: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        static B: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        let v = load_into(None, HOST_LIBC, &[("malloc", &A), ("plxnative_no_such_symbol", &B)], &[]);
        assert!(matches!(v, Loaded::Incomplete(_, 1)), "expected one missing symbol");
        assert!(A.load(Ordering::Acquire).is_null(), "malloc must not be published alone");
    }

    /// An ABSENT OPTIONAL symbol must not fail the load, and must leave its cell null so the
    /// generated wrapper no-ops. This is the webOS 10.2.0/11.2.0 case exactly: libavformat 59 and
    /// 60 carry 55 of the 56 functions ff.rs binds, and the one they dropped — av_register_all —
    /// has been a no-op since FFmpeg 4.0. Counting it made the app refuse to demux on the two
    /// newest firmware families, and report the wrong reason.
    #[test]
    fn an_absent_optional_symbol_does_not_fail_the_load() {
        static A: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        static B: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        let v = load_into(None, HOST_LIBC, &[("malloc", &A)], &[("plxnative_no_such_symbol", &B)]);
        assert!(v.ok(), "an absent OPTIONAL symbol must not fail the load");
        assert!(!A.load(Ordering::Acquire).is_null(), "the required symbol must still publish");
        assert!(B.load(Ordering::Acquire).is_null(), "the absent optional must stay null");
    }

    /// A PRESENT optional symbol publishes like any other.
    #[test]
    fn a_present_optional_symbol_is_published() {
        static A: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        static B: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        let v = load_into(None, HOST_LIBC, &[("malloc", &A)], &[("free", &B)]);
        assert!(v.ok());
        assert!(!B.load(Ordering::Acquire).is_null());
    }

    /// The happy path, graded against the host libc so it runs everywhere `cargo test` does.
    #[test]
    fn a_complete_load_publishes_every_cell() {
        static A: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        static B: AtomicPtr<c_void> = AtomicPtr::new(null_mut());
        let v = load_into(None, HOST_LIBC, &[("malloc", &A), ("free", &B)], &[]);
        assert!(v.ok(), "libc should open and carry malloc/free");
        assert!(!A.load(Ordering::Acquire).is_null() && !B.load(Ordering::Acquire).is_null());
    }

    /// The candidate list is ordered, and the FIRST that opens wins. Graded against the host's
    /// own C library so the test needs no fixture: a bogus name ahead of a real one must fall
    /// through to the real one, which is precisely the 57-vs-58 behaviour the TV path depends on.
    #[test]
    fn first_openable_candidate_wins() {
        assert!(Handle::open(&["libplxnative-nope.so.1", HOST_LIBC[0]]).is_some());
    }
}
