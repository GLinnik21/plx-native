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

extern "C" {
    fn curl_global_init(flags: c_long) -> c_int;
    fn curl_easy_init() -> *mut CURL;
    fn curl_easy_setopt(handle: *mut CURL, option: c_int, ...) -> c_int;
    fn curl_easy_perform(handle: *mut CURL) -> c_int;
    fn curl_easy_getinfo(handle: *mut CURL, info: c_int, ...) -> c_int;
    fn curl_easy_cleanup(handle: *mut CURL);
    fn curl_slist_append(list: *mut curl_slist, s: *const c_char) -> *mut curl_slist;
    fn curl_slist_free_all(list: *mut curl_slist);
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

/// One-time process init (call on the main thread at boot before any request; curl's implicit
/// init isn't thread-safe). Idempotent on the curl side.
pub fn global_init() {
    unsafe {
        curl_global_init(CURL_GLOBAL_ALL);
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
        curl_easy_setopt(h, CURLOPT_URL, url_c.as_ptr());
        curl_easy_setopt(h, CURLOPT_WRITEFUNCTION, write_cb as *const c_void);
        let mut buf: Vec<u8> = Vec::new();
        curl_easy_setopt(h, CURLOPT_WRITEDATA, (&mut buf as *mut Vec<u8>) as *mut c_void);
        curl_easy_setopt(h, CURLOPT_FOLLOWLOCATION, 1 as c_long);
        curl_easy_setopt(h, CURLOPT_SSL_VERIFYPEER, 1 as c_long);
        curl_easy_setopt(h, CURLOPT_SSL_VERIFYHOST, 2 as c_long);
        curl_easy_setopt(h, CURLOPT_NOSIGNAL, 1 as c_long);
        curl_easy_setopt(h, CURLOPT_CONNECTTIMEOUT, 8 as c_long);
        curl_easy_setopt(h, CURLOPT_TIMEOUT, 25 as c_long);
        let ua = CString::new("PlexForWebOS/1.0 (LG webOS)").ok()?;
        curl_easy_setopt(h, CURLOPT_USERAGENT, ua.as_ptr());

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
            curl_easy_setopt(h, CURLOPT_HTTPHEADER, slist as *const c_void);
        }
        if let Some(body) = post_body {
            curl_easy_setopt(h, CURLOPT_POST, 1 as c_long);
            curl_easy_setopt(h, CURLOPT_POSTFIELDSIZE, body.len() as c_long);
            // curl references (doesn't copy) the buffer during perform; `body` outlives the call.
            curl_easy_setopt(h, CURLOPT_POSTFIELDS, body.as_ptr() as *const c_void);
        }

        let rc = curl_easy_perform(h);
        let mut code: c_long = 0;
        curl_easy_getinfo(h, CURLINFO_RESPONSE_CODE, &mut code as *mut c_long);

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
