//! `net` — a small blocking HTTPS client over the TV's **libcurl** (linked via `stub/libcurl.so`,
//! SONAME `libcurl.so.5`; the real lib loads at runtime). The plain-HTTP numeric-IP socket in
//! [`crate::stream`] can't reach `plex.tv` (no DNS, no TLS); this fills that gap for the account /
//! login calls only — the local PMS keeps using the faster `stream.rs` path. Every call is
//! **blocking**, so callers must run it off the SDL main loop (the login poll + discovery threads).
//!
//! Only the curl *easy* API is used; the option/info integer constants are the stable public ABI
//! values from `curl.h` (kept here so we don't need the header). TLS peer+host verification is ON
//! (the device ships a CA bundle at `/etc/ssl/certs/ca-certificates.crt`, curl's default), and
//! `NOSIGNAL` is set because we call from threads. Response bodies never carry into a log here —
//! the account layer logs only status codes and non-sensitive fields.
#![allow(non_camel_case_types)]
use std::os::raw::{c_char, c_int, c_long, c_void};
use std::ffi::CString;
use std::ptr;

type CURL = c_void;
type curl_slist = c_void;

// ---- libcurl, bound at RUNTIME by SONAME candidate list ----
//
// The TV's libcurl SONAME is not stable across webOS: releases up to 6.4.0 answer to
// `libcurl.so.5`, and from 7.4.0 on only `libcurl.so.4` exists. Naming either one in DT_NEEDED
// therefore excludes half the fleet — and the exclusion is not "curl calls fail", it is the
// dynamic loader refusing to start the process at all. (5.3.1 and 6.4.0 carry BOTH names, a
// compat alias LG kept over the transition, which is why `.so.5` reached further than the file
// listing suggests.)
//
// Hand-written rather than expanded from `dynlib!` because two of these are VARIADIC —
// `curl_easy_setopt(handle, option, ...)` is the whole shape of curl's easy API — and a
// macro_rules pattern cannot carry `...` through to the function-pointer type it transmutes to.
// The loading itself is still `dynlib::load_into`, so the all-or-nothing publication rule and the
// per-symbol logging are shared, not reimplemented.
mod sys {
    use std::os::raw::c_void;
    use std::sync::atomic::AtomicPtr;

    pub(super) const CANDIDATES: &[&str] = &["libcurl.so.4", "libcurl.so.5"];
    macro_rules! cell {
        ($($n:ident),*) => { $( pub(super) static $n: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut()); )* };
    }
    cell!(GLOBAL_INIT, EASY_INIT, SETOPT, PERFORM, GETINFO, CLEANUP, SLIST_APPEND, SLIST_FREE_ALL);

    pub(super) fn load() -> crate::dynlib::Loaded {
        crate::dynlib::load_into(
            None, // the TV's own libcurl, wherever the loader finds it
            CANDIDATES,
            &[
                ("curl_global_init", &GLOBAL_INIT),
                ("curl_easy_init", &EASY_INIT),
                ("curl_easy_setopt", &SETOPT),
                ("curl_easy_perform", &PERFORM),
                ("curl_easy_getinfo", &GETINFO),
                ("curl_easy_cleanup", &CLEANUP),
                ("curl_slist_append", &SLIST_APPEND),
                ("curl_slist_free_all", &SLIST_FREE_ALL),
            ],
            &[], // every curl symbol here has existed since forever and on every SONAME
        )
    }
}

/// Resolve one cell or take the `missing_symbol` panic. Unreachable once [`global_init`] has
/// returned true, which every caller here is downstream of.
macro_rules! curlfn {
    ($cell:ident, $name:literal, $ty:ty) => {{
        let p = sys::$cell.load(std::sync::atomic::Ordering::Acquire);
        if p.is_null() {
            crate::dynlib::missing_symbol("libcurl", $name);
        }
        std::mem::transmute::<*mut c_void, $ty>(p)
    }};
}

unsafe fn curl_global_init(flags: c_long) -> c_int {
    curlfn!(GLOBAL_INIT, "curl_global_init", extern "C" fn(c_long) -> c_int)(flags)
}
unsafe fn curl_easy_init() -> *mut CURL {
    curlfn!(EASY_INIT, "curl_easy_init", extern "C" fn() -> *mut CURL)()
}
unsafe fn curl_easy_perform(handle: *mut CURL) -> c_int {
    curlfn!(PERFORM, "curl_easy_perform", extern "C" fn(*mut CURL) -> c_int)(handle)
}
unsafe fn curl_easy_cleanup(handle: *mut CURL) {
    curlfn!(CLEANUP, "curl_easy_cleanup", extern "C" fn(*mut CURL))(handle)
}
unsafe fn curl_slist_append(list: *mut curl_slist, s: *const c_char) -> *mut curl_slist {
    curlfn!(SLIST_APPEND, "curl_slist_append", extern "C" fn(*mut curl_slist, *const c_char) -> *mut curl_slist)(list, s)
}
unsafe fn curl_slist_free_all(list: *mut curl_slist) {
    curlfn!(SLIST_FREE_ALL, "curl_slist_free_all", extern "C" fn(*mut curl_slist))(list)
}
/// The two variadic ones. Each call site passes exactly one trailing argument and its type is
/// fixed by the option id, so they are resolved to a concrete non-variadic signature per use —
/// which is what the compiler was doing at the call sites anyway, and is why the option-id
/// constants below must keep matching the argument each one is given.
unsafe fn curl_easy_setopt_ptr(handle: *mut CURL, option: c_int, v: *const c_void) -> c_int {
    curlfn!(SETOPT, "curl_easy_setopt", extern "C" fn(*mut CURL, c_int, *const c_void) -> c_int)(handle, option, v)
}
unsafe fn curl_easy_setopt_long(handle: *mut CURL, option: c_int, v: c_long) -> c_int {
    curlfn!(SETOPT, "curl_easy_setopt", extern "C" fn(*mut CURL, c_int, c_long) -> c_int)(handle, option, v)
}
unsafe fn curl_easy_getinfo_long(handle: *mut CURL, info: c_int, out: *mut c_long) -> c_int {
    curlfn!(GETINFO, "curl_easy_getinfo", extern "C" fn(*mut CURL, c_int, *mut c_long) -> c_int)(handle, info, out)
}

// curl.h option ids (CURLOPTTYPE_LONG=0, OBJECTPOINT=10000, FUNCTIONPOINT=20000).
const CURLOPT_URL: c_int = 10002;
const CURLOPT_WRITEFUNCTION: c_int = 20011;
const CURLOPT_WRITEDATA: c_int = 10001;
const CURLOPT_HTTPHEADER: c_int = 10023;
const CURLOPT_POSTFIELDS: c_int = 10015;
const CURLOPT_POSTFIELDSIZE: c_int = 60;
const CURLOPT_POST: c_int = 47;
const CURLOPT_USERAGENT: c_int = 10018;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_SSL_VERIFYPEER: c_int = 64;
const CURLOPT_SSL_VERIFYHOST: c_int = 81;
const CURLOPT_NOSIGNAL: c_int = 99;
const CURLOPT_CONNECTTIMEOUT: c_int = 78;
const CURLOPT_TIMEOUT: c_int = 13;
// curl.h info ids (CURLINFO_LONG = 0x200000).
const CURLINFO_RESPONSE_CODE: c_int = 0x20_0002;
const CURL_GLOBAL_ALL: c_long = 3;

/// Is libcurl resolved? False on a device with no libcurl this app can bind, which means no
/// plex.tv sign-in and no account calls — but a running app, and a log line saying why.
static CURL_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn available() -> bool {
    CURL_OK.load(std::sync::atomic::Ordering::Acquire)
}

/// One-time process init (call on the main thread at boot before any request; curl's implicit
/// init isn't thread-safe). Idempotent on the curl side. Returns false if libcurl could not be
/// bound at all, in which case nothing else in this module may be called.
pub fn global_init() -> bool {
    match sys::load() {
        crate::dynlib::Loaded::Ok(soname) => {
            crate::log(&format!("net: bound libcurl -> {soname}"));
            unsafe { curl_global_init(CURL_GLOBAL_ALL) };
            CURL_OK.store(true, std::sync::atomic::Ordering::Release);
            true
        }
        crate::dynlib::Loaded::NoLibrary => {
            crate::log("net: no libcurl on this device (tried .so.4 and .so.5) — plex.tv sign-in unavailable");
            false
        }
        crate::dynlib::Loaded::Incomplete(soname, n) => {
            crate::log(&format!("net: {soname} is missing {n} symbol(s) — plex.tv sign-in unavailable"));
            false
        }
    }
}

/// libcurl `CURLOPT_WRITEFUNCTION`: append received bytes to the caller's `Vec<u8>`.
extern "C" fn write_cb(ptr: *mut c_char, size: usize, nmemb: usize, userdata: *mut c_void) -> usize {
    let n = size.saturating_mul(nmemb);
    if userdata.is_null() || ptr.is_null() {
        return 0;
    }
    let buf = unsafe { &mut *(userdata as *mut Vec<u8>) };
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, n) };
    buf.extend_from_slice(slice);
    n
}

/// An HTTP response: numeric status + raw body bytes.
pub struct Resp {
    pub status: u16,
    pub body: Vec<u8>,
}
impl Resp {
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Blocking HTTPS request. `headers` are full `"Name: value"` lines. `post_body` = `Some` for POST
/// (empty slice → POST with no body), `None` → GET. Returns `None` on a transport error (offline,
/// TLS failure, timeout) — callers treat that as "not reachable" and fall back to the local server.
fn perform(url: &str, headers: &[String], post_body: Option<&[u8]>) -> Option<Resp> {
    unsafe {
        let h = curl_easy_init();
        if h.is_null() {
            return None;
        }
        let url_c = CString::new(url).ok()?;
        curl_easy_setopt_ptr(h, CURLOPT_URL, url_c.as_ptr() as *const c_void);
        curl_easy_setopt_ptr(h, CURLOPT_WRITEFUNCTION, write_cb as *const c_void);
        let mut buf: Vec<u8> = Vec::new();
        curl_easy_setopt_ptr(h, CURLOPT_WRITEDATA, (&mut buf as *mut Vec<u8>) as *mut c_void);
        curl_easy_setopt_long(h, CURLOPT_FOLLOWLOCATION, 1 as c_long);
        curl_easy_setopt_long(h, CURLOPT_SSL_VERIFYPEER, 1 as c_long);
        curl_easy_setopt_long(h, CURLOPT_SSL_VERIFYHOST, 2 as c_long);
        curl_easy_setopt_long(h, CURLOPT_NOSIGNAL, 1 as c_long);
        curl_easy_setopt_long(h, CURLOPT_CONNECTTIMEOUT, 8 as c_long);
        curl_easy_setopt_long(h, CURLOPT_TIMEOUT, 25 as c_long);
        let ua = CString::new(crate::plex::identity::user_agent()).ok()?;
        curl_easy_setopt_ptr(h, CURLOPT_USERAGENT, ua.as_ptr() as *const c_void);

        // request headers — keep the CStrings alive until after perform.
        let mut hdr_owned: Vec<CString> = Vec::with_capacity(headers.len());
        let mut slist: *mut curl_slist = ptr::null_mut();
        for line in headers {
            if let Ok(c) = CString::new(line.as_str()) {
                slist = curl_slist_append(slist, c.as_ptr());
                hdr_owned.push(c);
            }
        }
        if !slist.is_null() {
            curl_easy_setopt_ptr(h, CURLOPT_HTTPHEADER, slist as *const c_void);
        }
        if let Some(body) = post_body {
            curl_easy_setopt_long(h, CURLOPT_POST, 1 as c_long);
            curl_easy_setopt_long(h, CURLOPT_POSTFIELDSIZE, body.len() as c_long);
            // curl references (doesn't copy) the buffer during perform; `body` outlives the call.
            curl_easy_setopt_ptr(h, CURLOPT_POSTFIELDS, body.as_ptr() as *const c_void);
        }

        let rc = curl_easy_perform(h);
        let mut code: c_long = 0;
        curl_easy_getinfo_long(h, CURLINFO_RESPONSE_CODE, &mut code as *mut c_long);

        if !slist.is_null() {
            curl_slist_free_all(slist);
        }
        curl_easy_cleanup(h);
        drop(hdr_owned);
        if rc != 0 {
            return None;
        }
        Some(Resp { status: code as u16, body: buf })
    }
}

/// Blocking HTTPS GET.
pub fn https_get(url: &str, headers: &[String]) -> Option<Resp> {
    perform(url, headers, None)
}
/// Blocking HTTPS POST (`body` may be empty).
pub fn https_post(url: &str, headers: &[String], body: &[u8]) -> Option<Resp> {
    perform(url, headers, Some(body))
}
