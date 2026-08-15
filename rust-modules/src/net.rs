//! `net` — a small blocking HTTPS client over **whatever libcurl the machine already has**, bound
//! at runtime by the candidate list below (nothing is linked, and `stub/` — which this line named
//! for months after it was deleted — is gone). The plain-HTTP numeric-IP socket in
//! [`crate::stream`] can't reach `plex.tv` (no DNS, no TLS); this fills that gap for the account /
//! login calls only — the local PMS keeps using the faster `stream.rs` path. Every call is
//! **blocking**, so callers must run it off the SDL main loop (the login poll + discovery threads).
//!
//! Only the curl *easy* API is used; the option/info integer constants are the stable public ABI
//! values from `curl.h` (kept here so we don't need the header). TLS peer+host verification is ON
//! (the device ships a CA bundle at `/etc/ssl/certs/ca-certificates.crt`, curl's default; macOS
//! uses its own trust store), and
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
// `libcurl.so.5`, and from 7.4.0 on only `libcurl.so.4` exists. Naming either in DT_NEEDED
// excludes half the fleet — and the exclusion is not "curl calls fail", it is the dynamic loader
// refusing to start the process. (5.3.1 and 6.4.0 carry BOTH names, a compat alias LG kept over
// the transition, which is why `.so.5` reached further than the file listing suggests.)
//
// The three setopt/getinfo wrappers share two C symbols via the macro's `= "name"` override. That
// is how the variadic API is bound: each call site passes exactly one trailing argument whose type
// is fixed by the option id, so one concrete non-variadic signature per call shape is both
// sufficient and what the compiler was already generating. This was hand-written once on the
// belief that a macro could not express it — the blocker was never the variadics, only the
// one-symbol-per-wrapper assumption.
//
// **The third name is macOS's, and it is what makes the desktop build able to SIGN IN.** The host
// simulator and the `PlxNative.app` bundle run this same module, and on a Mac the two ELF SONAMEs
// simply do not open — which is why signing in "did not work off-device" and was written up as an
// unfixable property of the simulator. It was one missing candidate: macOS ships libcurl in the
// dyld shared cache and `dlopen("libcurl.4.dylib")` resolves it with no install, no Homebrew and
// nothing to bundle (verified 2026-08-16: the handle opens and `curl_easy_setopt` resolves).
// It is LAST deliberately — a television never reaches it, so this costs the device nothing but
// one extra failed `dlopen` in the already-fatal no-curl case, and the candidate list stays
// ordered by "what the fleet actually answers to" first.
crate::dynlib! {
    curl: ["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"] {
    fn curl_global_init(flags: c_long) -> c_int;
    fn curl_version() -> *const c_char;
    fn curl_easy_init() -> *mut CURL;
    fn curl_easy_perform(handle: *mut CURL) -> c_int;
    fn curl_easy_cleanup(handle: *mut CURL);
    fn curl_slist_append(list: *mut curl_slist, s: *const c_char) -> *mut curl_slist;
    fn curl_slist_free_all(list: *mut curl_slist);
    // The three VARIADIC ones, and the `...` marks exactly what `curl.h` marks: the handle and the
    // option id are the only named parameters, and the value arrives through `va_arg`. Spelling
    // that value's type after the ellipsis is what lets one C symbol be bound as three wrappers;
    // moving it BEFORE the ellipsis would compile, run on the television, and hand libcurl a
    // garbage pointer on Apple ARM64 (see `dynlib!`'s doc — it is a stack-vs-register convention
    // difference, and it took sign-in down inside `strlen`).
    fn curl_easy_setopt_ptr = "curl_easy_setopt"(handle: *mut CURL, option: c_int, ..., v: *const c_void) -> c_int;
    fn curl_easy_setopt_long = "curl_easy_setopt"(handle: *mut CURL, option: c_int, ..., v: c_long) -> c_int;
    fn curl_easy_getinfo_long = "curl_easy_getinfo"(handle: *mut CURL, info: c_int, ..., out: *mut c_long) -> c_int;
}}

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

fn available() -> bool {
    CURL_OK.load(std::sync::atomic::Ordering::Acquire)
}

/// One-time process init (call on the main thread at boot before any request; curl's implicit
/// init isn't thread-safe). Idempotent on the curl side. Returns false if libcurl could not be
/// bound at all, in which case nothing else in this module may be called.
pub fn global_init() -> bool {
    match curl::load(None) {
        crate::dynlib::Loaded::Ok(soname) => {
            unsafe { curl_global_init(CURL_GLOBAL_ALL) };
            // The version string carries the TLS backend and its version, which is the fact worth
            // having in a bug report from hardware nobody here owns.
            let v = unsafe { curl_version() };
            let v = if v.is_null() {
                String::new()
            } else {
                unsafe { std::ffi::CStr::from_ptr(v) }.to_string_lossy().into_owned()
            };
            crate::log(&format!("net: bound libcurl -> {soname} ({v})"));
            CURL_OK.store(true, std::sync::atomic::Ordering::Release);
            true
        }
        crate::dynlib::Loaded::NoLibrary => {
            crate::log("net: no libcurl on this device (tried .so.4, .so.5 and .4.dylib) — plex.tv sign-in unavailable");
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
    // The guard `CURL_OK` exists for. Without it, a device with no libcurl this app can bind
    // reaches `curl_easy_init`'s wrapper and takes `dynlib::missing_symbol`, which panics — an
    // account lookup failing should return None and let the caller fall back, not kill a thread.
    if !available() {
        return None;
    }
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
            // NAMED, not just counted. Everything here rides the TELEVISION's curl and therefore
            // its OpenSSL and its CA store — the library webosbrew's caniuse data singles out as
            // the one that varies most across firmwares. Collapsing every failure to None made a
            // stale CA bundle on a set nobody here owns indistinguishable from being offline: the
            // QR sign-in simply never completes. These four are the ones that mean something
            // different from "the network is down".
            let why = match rc {
                60 => "peer certificate could not be verified (CA store too old?)",
                35 => "TLS handshake failed (protocol too new for this firmware?)",
                77 => "CA bundle could not be read",
                6 => "could not resolve host",
                28 => "timed out",
                _ => "transport error",
            };
            crate::log(&format!("net: curl rc={rc} — {why}"));
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
