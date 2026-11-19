//! Runtime library binding: `dlopen` a short list of SONAME candidates, then resolve symbols by
//! name. No Tizen library is linked, so `libplx.so`'s DT_NEEDED stays libc/libdl/libpthread/libm/
//! libgcc_s whatever the firmware ships. (House style: `rust-modules/base/src/dynlib.rs`; this file
//! shares no code with it, because the crate is a standalone throwaway.)
//!
//! Every dlopen attempt and every symbol resolution is logged: on a television the log is the only
//! instrument there is.

use crate::log::plog;
use std::ffi::{c_char, c_void, CStr, CString};

pub type Ptr = *mut c_void;

/// An opened library. Never closed: the process owns it until exit.
#[derive(Clone, Copy)]
pub struct Lib {
    pub name: &'static str,
    handle: Ptr,
}

// SAFETY: a dlopen handle is a process-global token; the loader serializes its own state.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

fn dlerror_text() -> String {
    // SAFETY: dlerror returns null or a NUL-terminated string valid until the next dl* call on this
    // thread; it is copied immediately.
    unsafe {
        let p = libc::dlerror();
        if p.is_null() {
            "(no dlerror text)".to_string()
        } else {
            CStr::from_ptr(p as *const c_char).to_string_lossy().into_owned()
        }
    }
}

/// Try each candidate in order and return the first that opens, logging every attempt and which
/// name hit. `label` names the role ("ecore_wl2", "EGL"), not the file.
pub fn open_first(label: &str, candidates: &[&'static str]) -> Option<Lib> {
    for &name in candidates {
        let Ok(c) = CString::new(name) else { continue };
        // RTLD_NOW: a lazily-bound miss would surface later as a jump into the resolver at an
        // arbitrary moment; the point of this test is to learn at load time.
        // SAFETY: `c` is a valid NUL-terminated string.
        let h = unsafe { libc::dlopen(c.as_ptr(), libc::RTLD_NOW) };
        if h.is_null() {
            plog!("dlopen {label}: {name} -> FAIL: {}", dlerror_text());
        } else {
            plog!("dlopen {label}: {name} -> ok (hit)");
            return Some(Lib { name, handle: h });
        }
    }
    plog!("dlopen {label}: no candidate opened ({})", candidates.join(", "));
    None
}

/// The address of `sym` in the first library that has it, then in the process's global scope.
/// Returns the address and the library name that answered.
pub fn resolve(libs: &[&Lib], sym: &str) -> Option<(Ptr, &'static str)> {
    let c = CString::new(sym).ok()?;
    for lib in libs {
        // SAFETY: valid handle from dlopen, valid C string.
        let p = unsafe { libc::dlsym(lib.handle, c.as_ptr()) };
        if !p.is_null() {
            return Some((p, lib.name));
        }
    }
    // SAFETY: RTLD_DEFAULT is a valid pseudo-handle.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c.as_ptr()) };
    (!p.is_null()).then_some((p, "(process global scope)"))
}

/// Read an exported global `int` (`ECORE_EVENT_KEY_DOWN` and friends): dlsym returns a pointer TO
/// the variable, not its value. The value is only meaningful after the library's init has run.
pub fn read_int(libs: &[&Lib], sym: &str) -> Option<i32> {
    let (p, _) = resolve(libs, sym)?;
    // SAFETY: the symbol is documented as an exported `int`; the pointer is non-null and aligned.
    Some(unsafe { *(p as *const i32) })
}

/// Resolve one function pointer of type `T` (must be pointer-sized) and log the outcome.
pub fn bind<T: Copy>(libs: &[&Lib], sym: &str, required: bool) -> Option<T> {
    assert_eq!(std::mem::size_of::<T>(), std::mem::size_of::<Ptr>());
    match resolve(libs, sym) {
        Some((p, from)) => {
            plog!("sym {sym} <- {from}");
            // SAFETY: T is a pointer-sized `extern "C"` fn pointer type chosen by the caller to
            // match the C prototype; `p` is the symbol's address.
            Some(unsafe { std::mem::transmute_copy::<Ptr, T>(&p) })
        }
        None => {
            plog!(
                "sym {sym}: {}",
                if required { "MISSING (required)" } else { "missing (optional)" }
            );
            None
        }
    }
}

/// Declare a table of C function pointers plus its loader. `required` symbols must all resolve or
/// `load` returns the list of missing names; `optional` ones become `Option`s. The two lists keep a
/// version difference between firmwares from being fatal where it need not be.
macro_rules! symtable {
    (
        $(#[$meta:meta])*
        pub struct $name:ident {
            required { $( $rf:ident : $rt:ty = $rs:literal ),* $(,)? }
            optional { $( $of:ident : $ot:ty = $os:literal ),* $(,)? }
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy)]
        pub struct $name {
            $( pub $rf: $rt, )*
            $( pub $of: Option<$ot>, )*
        }

        impl $name {
            /// Resolve the table from `libs` (then the global scope). `Err` lists the missing
            /// required symbols, already logged by name.
            pub fn load(libs: &[&$crate::dl::Lib]) -> Result<Self, Vec<&'static str>> {
                let mut missing: Vec<&'static str> = Vec::new();
                $( let $rf: Option<$rt> = $crate::dl::bind(libs, $rs, true);
                   if $rf.is_none() { missing.push($rs); } )*
                $( let $of: Option<$ot> = $crate::dl::bind(libs, $os, false); )*
                if !missing.is_empty() {
                    return Err(missing);
                }
                Ok(Self {
                    $( $rf: $rf.unwrap(), )*
                    $( $of, )*
                })
            }
        }
    };
}
pub(crate) use symtable;
