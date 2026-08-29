//! `net` — a small blocking HTTPS client over **whatever libcurl the machine already has**, bound
//! at runtime by the candidate list below (nothing is linked, and `stub/` — which this line named
//! for months after it was deleted — is gone). The plain-HTTP socket in [`crate::stream`] can
//! resolve names now, but it still cannot do TLS; this fills that gap. Every call is
//! **blocking**, so callers must run it off the SDL main loop (the account and PMS workers).
//!
//! **It is no longer the plex.tv transport alone**, and that line stood here while it was becoming
//! untrue. A PMS reached over the public internet is an `https://…plex.direct` origin — a NAME,
//! because that is what the certificate is issued for — so the whole PMS control plane comes
//! through here too whenever the origin is TLS. `crate::http` is the door that decides which of
//! the two transports a request takes; this module is only ever the https half of it. Direct
//! callers are `plex::account`, auth's public headerless QR-image fetch, and — since the telemetry
//! work — [`crate::telemetry::sender`], which is the first traffic in this app's history to a host
//! that is neither Plex nor the user's own server. It goes through [`post_ca`]: CA-verified,
//! unpinned, bounded sink. **This list has stood here while becoming untrue before**, which is why
//! it is worth checking rather than trusting; that is twice.
//!
//! Only the curl *easy* API is used here; [`crate::curlio`] binds the multi API separately for the
//! media plane. This module owns their shared process init, including the mutex callbacks required
//! when the TV's libcurl uses OpenSSL 1.0. The option/info integer constants are curl's stable
//! public ABI values (kept here so we do not need the header). TLS peer+host verification is ON,
//! and `NOSIGNAL` is set because we call from threads. Response bodies never carry into a log here.
#![allow(non_camel_case_types)]
use std::cell::UnsafeCell;
use std::ffi::CString;
use std::mem::MaybeUninit;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_void};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

type CURL = c_void;
type curl_slist = c_void;

/// The stable head of `curl_version_info_data`. Passing `CURLVERSION_FIRST` promises to inspect
/// only these original fields; libcurl extends the struct at the tail for later ages, so this
/// prefix has the same offsets on the television's 7.53.1 and current macOS.
#[repr(C)]
pub(crate) struct CurlVersionInfo {
    age: c_int,
    version: *const c_char,
    version_num: c_uint,
    host: *const c_char,
    features: c_int,
}

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
    fn curl_version_info(age: c_int) -> *const CurlVersionInfo;
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
const CURLOPT_LOW_SPEED_LIMIT: c_int = 19;
const CURLOPT_LOW_SPEED_TIME: c_int = 20;
const CURLOPT_FOLLOWLOCATION: c_int = 52;
const CURLOPT_MAXREDIRS: c_int = 68;
const CURLOPT_SSL_VERIFYPEER: c_int = 64;
const CURLOPT_SSL_VERIFYHOST: c_int = 81;
const CURLOPT_NOSIGNAL: c_int = 99;
const CURLOPT_CONNECTTIMEOUT: c_int = 78;
const CURLOPT_TIMEOUT: c_int = 13;
/// The numeric protocol allow-list options are the compatibility surface for this project's curl
/// floor. Their `_STR` replacements arrived in 7.85; the television has 7.53.1. Both numeric
/// options have existed since 7.19.4.
const CURLOPT_PROTOCOLS: c_int = 181;
const CURLOPT_REDIR_PROTOCOLS: c_int = 182;
const CURLPROTO_HTTP: c_long = 1 << 0;
const CURLPROTO_HTTPS: c_long = 1 << 1;
const PUBLIC_MAX_REDIRECTS: c_long = 5;
/// `CURLOPT_CUSTOMREQUEST` (OBJECTPOINT + 36). The METHOD TOKEN, and nothing else — it does not
/// change what curl sends or expects, it only overrides the verb written on the request line. That
/// is exactly what a body-less `PUT` needs: `CURLOPT_UPLOAD` would make curl wait to read a body
/// it is never given, while this sends a plain GET-shaped request that says `PUT` — the same bytes
/// `crate::http`'s plaintext arm puts on the wire for `plex::Client::put`.
///
/// It is an option ID, **not a new symbol**: `curl_easy_setopt` is already bound, so nothing about
/// the `dynlib!` table (and therefore nothing about which firmwares this binary starts on) moves
/// for this. Present since libcurl 7.1; the television's is 7.53.1.
const CURLOPT_CUSTOMREQUEST: c_int = 10036;
/// `CURLOPT_PINNEDPUBLICKEY` (OBJECTPOINT + 230) — `"sha256//<base64 of the SPKI's SHA-256>"`.
///
/// libcurl 7.39+, against the dev television's 7.53.1, so it reaches every firmware this app
/// claims. It is checked **independently of** `CURLOPT_SSL_VERIFYPEER`, which is what makes the
/// one caller possible: the lab receiver ([`crate::lab`]) is a self-signed certificate generated
/// per session on a developer's Mac, so there is no CA to verify against and the pin is the whole
/// of the endpoint's identity — a narrower trust root than the television's CA store, not a wider
/// one. No private key is in this binary; a pin is a hash of a public key.
const CURLOPT_PINNEDPUBLICKEY: c_int = 10230;

/// `CURLOPT_CAINFO` (OBJECTPOINT + 65) — a path to a PEM bundle to verify the peer against,
/// INSTEAD of whatever trust store this firmware shipped with in 2019.
///
/// **It is not pinning and must not be read as pinning**, which is the confusion
/// [`CURLOPT_PINNEDPUBLICKEY`]'s own doc exists to prevent from the other side: this selects which
/// roots are trusted, and any certificate chaining to one of them still validates. What it buys is
/// independence from a store nobody can update on a television, on a path whose far end is not a
/// Plex service and whose CA may rotate.
const CURLOPT_CAINFO: c_int = 10065;
// curl.h info ids (CURLINFO_LONG = 0x200000).
const CURLINFO_RESPONSE_CODE: c_int = 0x20_0002;
const CURL_GLOBAL_ALL: c_long = 3;
const CURLVERSION_FIRST: c_int = 0;
const CURL_VERSION_ASYNCHDNS: c_int = 1 << 7;

/// Is libcurl resolved? False on a device with no libcurl this app can bind, which means no
/// plex.tv account calls or HTTPS PMS control — but a running app, and a log line saying why.
static CURL_OK: AtomicBool = AtomicBool::new(false);

/// Whether two threads may enter distinct curl handles concurrently. The media multi transport
/// checks this separately from [`CURL_OK`]: an old OpenSSL whose mutex API cannot be installed may
/// still serve serialized HTTPS control, but must not be driven beside another curl request.
static CURL_THREADED_TLS_OK: AtomicBool = AtomicBool::new(false);
/// Only used on the abnormal old-OpenSSL/no-callback fallback. Normal devices never take it.
static CURL_FALLBACK_SERIAL: Mutex<()> = Mutex::new(());

// OpenSSL before 1.1 delegates its process-global locks to the application. These symbols remain
// optional instead of joining curl's all-or-nothing table: modern and non-OpenSSL backends do not
// export them. The lock array is process-lifetime storage because the callback is process-global
// and neither libcurl nor its dependency is closed.
struct LegacyMutex(UnsafeCell<MaybeUninit<libc::pthread_mutex_t>>);

impl LegacyMutex {
    fn uninit() -> Self {
        Self(UnsafeCell::new(MaybeUninit::uninit()))
    }

    fn as_mut_ptr(&self) -> *mut libc::pthread_mutex_t {
        unsafe { (*self.0.get()).as_mut_ptr() }
    }
}

// Access is exclusively through pthread's synchronization functions after boot-time init.
unsafe impl Sync for LegacyMutex {}

static LEGACY_CRYPTO_LOCKS: OnceLock<Box<[LegacyMutex]>> = OnceLock::new();
static LEGACY_CRYPTO_RESULT: OnceLock<LegacyCrypto> = OnceLock::new();

type LegacyLockCallback = unsafe extern "C" fn(c_int, c_int, *const c_char, c_int);
type CryptoNumLocks = unsafe extern "C" fn() -> c_int;
type CryptoGetLockingCallback = unsafe extern "C" fn() -> Option<LegacyLockCallback>;
type CryptoSetLockingCallback = unsafe extern "C" fn(Option<LegacyLockCallback>);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyCrypto {
    NotNeeded,
    Existing,
    Installed,
    Missing,
}

unsafe fn apply_legacy_crypto_lock(lock: *mut libc::pthread_mutex_t, mode: c_int) {
    if lock.is_null() {
        return;
    }
    if mode & 1 != 0 {
        libc::pthread_mutex_lock(lock);
    } else {
        libc::pthread_mutex_unlock(lock);
    }
}

/// OpenSSL 1.0's `locking_function`. READ and WRITE both map to an exclusive pthread mutex, which
/// is exactly the contract in the legacy API's own example and is sufficient for correctness.
unsafe extern "C" fn legacy_crypto_lock(mode: c_int, n: c_int, _file: *const c_char, _line: c_int) {
    if n < 0 {
        return;
    }
    let Some(lock) = LEGACY_CRYPTO_LOCKS.get().and_then(|locks| locks.get(n as usize)) else {
        return;
    };
    apply_legacy_crypto_lock(lock.as_mut_ptr(), mode);
}

fn needs_legacy_crypto_locks(version: &str) -> bool {
    version
        .split_ascii_whitespace()
        .any(|part| part.starts_with("OpenSSL/0.") || part.starts_with("OpenSSL/1.0."))
}

fn needs_legacy_thread_id(version: &str) -> bool {
    version.split_ascii_whitespace().any(|part| part.starts_with("OpenSSL/0."))
}

fn threaded_tls_policy(version: &str, locks: LegacyCrypto) -> bool {
    if needs_legacy_thread_id(version) {
        // 0.9.x defaults to getpid() on Unix, which is not a thread identity. We do not take
        // ownership of another component's process-global ID callback, so this backend stays on
        // the serialized control-plane fallback and cannot run concurrent media.
        false
    } else {
        !needs_legacy_crypto_locks(version)
            || matches!(locks, LegacyCrypto::Existing | LegacyCrypto::Installed)
    }
}

/// Install OpenSSL 1.0's process locks, or preserve a callback somebody loaded before us.
///
/// Reopening the selected libcurl SONAME returns its existing loader object, and symbol lookup on
/// that handle follows its dependency closure to the libcrypto it actually uses. This deliberately
/// costs one permanent reference instead of guessing a moving `libcrypto.so.*` SONAME or asking a
/// process-global scope that might contain two crypto majors. OpenSSL 1.1+ removes these entry
/// points; absence is therefore only fatal when curl's version string names a legacy backend.
///
/// No ID callback is installed: on the target's glibc, OpenSSL 1.0's documented default uses the
/// address of thread-local `errno`, which is already a unique thread identity. Overwriting an ID
/// callback owned by another component would be strictly less safe.
fn setup_legacy_crypto_locks(soname: &'static str) -> LegacyCrypto {
    *LEGACY_CRYPTO_RESULT.get_or_init(|| {
        let Some((scope, _)) = crate::dynlib::Handle::open(&[soname]) else {
            return LegacyCrypto::Missing;
        };
        let (Some(num), Some(get), Some(set)) = (
            scope.sym("CRYPTO_num_locks").filter(|p| !p.is_null()),
            scope.sym("CRYPTO_get_locking_callback").filter(|p| !p.is_null()),
            scope.sym("CRYPTO_set_locking_callback").filter(|p| !p.is_null()),
        ) else {
            return LegacyCrypto::Missing;
        };
        let num: CryptoNumLocks = unsafe { std::mem::transmute(num) };
        let get: CryptoGetLockingCallback = unsafe { std::mem::transmute(get) };
        let set: CryptoSetLockingCallback = unsafe { std::mem::transmute(set) };
        if unsafe { get() }.is_some() {
            return LegacyCrypto::Existing;
        }
        let count = unsafe { num() };
        if count <= 0 || count > 1024 {
            return LegacyCrypto::Missing;
        }

        // Allocate final storage first: no pthread mutex moves after pthread_mutex_init writes it.
        let locks: Box<[LegacyMutex]> = std::iter::repeat_with(LegacyMutex::uninit)
            .take(count as usize)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let mut initialised = 0usize;
        for lock in &locks {
            let rc = unsafe { libc::pthread_mutex_init(lock.as_mut_ptr(), ptr::null()) };
            if rc != 0 {
                for old in &locks[..initialised] {
                    unsafe { libc::pthread_mutex_destroy(old.as_mut_ptr()) };
                }
                return LegacyCrypto::Missing;
            }
            initialised += 1;
        }

        // Preserve a callback that appeared while storage was being prepared. Boot normally has
        // no competing initializer, but coexistence costs nothing to check here.
        if unsafe { get() }.is_some() {
            for lock in &locks {
                unsafe { libc::pthread_mutex_destroy(lock.as_mut_ptr()) };
            }
            return LegacyCrypto::Existing;
        }
        if let Err(locks) = LEGACY_CRYPTO_LOCKS.set(locks) {
            for lock in &locks {
                unsafe { libc::pthread_mutex_destroy(lock.as_mut_ptr()) };
            }
            return LegacyCrypto::Missing;
        }

        // Publish storage before the process-global callback: another curl thread may enter as
        // soon as the setter returns.
        unsafe { set(Some(legacy_crypto_lock)) };
        if unsafe { get() }.is_some() {
            LegacyCrypto::Installed
        } else {
            LegacyCrypto::Missing
        }
    })
}

fn available() -> bool {
    CURL_OK.load(Ordering::Acquire)
}

pub(crate) fn threaded_tls_ready() -> bool {
    CURL_THREADED_TLS_OK.load(Ordering::Acquire)
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
            let legacy = needs_legacy_crypto_locks(&v);
            let locks = if legacy { setup_legacy_crypto_locks(soname) } else { LegacyCrypto::NotNeeded };
            let threaded = threaded_tls_policy(&v, locks);
            CURL_THREADED_TLS_OK.store(threaded, Ordering::Release);
            // `curl_version()`'s prose happened to name c-ares on the development television,
            // but the feature bit is the API. With NOSIGNAL, a synchronous resolver can outlive
            // CONNECTTIMEOUT; log the runtime fact for every firmware instead of promoting one
            // set's string into a fleet-wide guarantee.
            let vi = unsafe { curl_version_info(CURLVERSION_FIRST) };
            let async_dns = if vi.is_null() {
                "unknown"
            } else if unsafe { (*vi).features } & CURL_VERSION_ASYNCHDNS != 0 {
                "yes"
            } else {
                "no"
            };
            crate::log(&format!(
                "net: bound libcurl -> {soname} ({v}; AsynchDNS={async_dns}); \
                 threaded-tls={threaded} legacy-locks={locks:?}"
            ));
            if legacy && !threaded {
                crate::log(
                    "net: legacy OpenSSL concurrency unavailable — serialized HTTPS control \
                     remains available; concurrent HTTPS media is disabled",
                );
            }
            CURL_OK.store(true, Ordering::Release);
            true
        }
        crate::dynlib::Loaded::NoLibrary => {
            crate::log(
                "net: no libcurl on this device (tried .so.4, .so.5 and .4.dylib) — \
                 account calls and HTTPS PMS control unavailable",
            );
            false
        }
        crate::dynlib::Loaded::Incomplete(soname, n) => {
            crate::log(&format!(
                "net: {soname} is missing {n} symbol(s) — account calls and HTTPS PMS control unavailable"
            ));
            false
        }
    }
}

struct BodySink {
    body: Vec<u8>,
    max: Option<usize>,
    overflowed: bool,
}

impl BodySink {
    fn new(max: Option<usize>) -> BodySink {
        BodySink { body: Vec::new(), max, overflowed: false }
    }

    /// Append one curl callback chunk without ever allocating past the caller's ceiling.
    fn push(&mut self, bytes: &[u8]) -> bool {
        if self.max.is_some_and(|max| bytes.len() > max.saturating_sub(self.body.len())) {
            self.overflowed = true;
            return false;
        }
        self.body.extend_from_slice(bytes);
        true
    }
}

/// libcurl `CURLOPT_WRITEFUNCTION`: append received bytes to the caller's bounded sink.
extern "C" fn write_cb(ptr: *mut c_char, size: usize, nmemb: usize, userdata: *mut c_void) -> usize {
    let n = size.saturating_mul(nmemb);
    if userdata.is_null() || ptr.is_null() {
        return 0;
    }
    let sink = unsafe { &mut *(userdata as *mut BodySink) };
    if sink.max.is_some_and(|max| n > max.saturating_sub(sink.body.len())) {
        sink.overflowed = true;
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, n) };
    if sink.push(slice) { n } else { 0 }
}

struct Easy(*mut CURL);

impl Drop for Easy {
    fn drop(&mut self) {
        unsafe { curl_easy_cleanup(self.0) };
    }
}

struct HeaderList(*mut curl_slist);

impl Drop for HeaderList {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { curl_slist_free_all(self.0) };
        }
    }
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

/// **How long one call may take.** The values are a PER-CALL argument rather than constants
/// because the right policy depends entirely on what is being fetched.
///
/// `total_s` is `CURLOPT_TIMEOUT`, which bounds the WHOLE transfer — connect, TLS, request,
/// response body, all of it. 25 s is right for an API call, whose answer is a few kilobytes of
/// JSON, and is **fatal for anything that streams**: a long transfer is aborted mid-body at 25 s
/// however healthy the connection is. The control plane once hard-coded that number for every
/// curl body; T4's separate curl-multi media transport does not use this easy-client policy.
///
/// `connect_s` is `CURLOPT_CONNECTTIMEOUT` and bounds only the handshake, so it is the one that
/// normally decides how long a *dead* address costs. The development television has c-ares, but
/// that is not assumed fleet-wide: [`global_init`] queries and logs `CURL_VERSION_ASYNCHDNS`.
/// With `NOSIGNAL` and a synchronous resolver, a name lookup may outlive this value.
///
/// `low_speed_bps` + `low_speed_s` are curl's rolling low-speed guard. They bound a connection
/// that succeeds and then stops making useful progress without imposing a deadline on a healthy
/// large body. Zero disables the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeouts {
    pub connect_s: c_long,
    pub total_s: c_long,
    pub low_speed_bps: c_long,
    pub low_speed_s: c_long,
}

/// The deadlines for an **API call** — a request whose answer is small and whose caller is a user
/// waiting behind a spinner. The values every call in this app used before they were a parameter,
/// unchanged, so nothing about plex.tv sign-in moves.
pub const API: Timeouts = Timeouts { connect_s: 8, total_s: 25, low_speed_bps: 0, low_speed_s: 0 };

/// The deadlines for a PMS body whose size is content-dependent (library JSON, artwork, sidecar
/// subtitles). A connect normally costs at most 8 s and fewer than one byte per second for 30 s
/// ends a stalled transfer, but a healthy transfer has no wall-clock guillotine:
/// `CURLOPT_TIMEOUT=0` is libcurl's documented disabled value.
pub const BULK: Timeouts =
    Timeouts { connect_s: 8, total_s: 0, low_speed_bps: 1, low_speed_s: 30 };

/// Protocol floor for a public redirect. A TLS request may remain TLS only; a plaintext request
/// may stay plaintext or upgrade. Pure so the no-downgrade rule is host-testable.
fn allowed_redirect_protocols(url: &[u8]) -> c_long {
    if url.get(..8).is_some_and(|scheme| scheme.eq_ignore_ascii_case(b"https://")) {
        CURLPROTO_HTTPS
    } else {
        CURLPROTO_HTTP | CURLPROTO_HTTPS
    }
}

/// Blocking HTTPS request. `headers` are full `"Name: value"` lines. `body` = `Some` for a request
/// that carries one (empty slice → a POST with no body), `None` for one that does not. `verb` is
/// the method token; see [`CURLOPT_CUSTOMREQUEST`] for how a body-less non-GET is sent.
///
/// Returns `None` on a transport error (offline, TLS failure, timeout) — callers treat that as
/// "not reachable". A request that COMPLETED comes back as `Some`, whatever status it carries, so
/// a `401` is a value here and never a `None`: that distinction is the whole of
/// `plex::probe::Outcome`, and folding it is what sends a user to look at a router for a token
/// problem.
///
/// **The easy handle is per call, deliberately.** It is initialised and cleaned up here, so there
/// is no cross-call state to remember to clear — no leftover `CUSTOMREQUEST` turning the next GET
/// into a PUT, no stale header list, no connection reuse whose keep-alive outlives the token that
/// authorised it. A reusable handle (or a share/multi) would buy connection reuse and cost a
/// design: this app makes tens of control-plane requests per session, not thousands.
pub(crate) fn request(
    url: &str,
    headers: &[String],
    verb: &str,
    body: Option<&[u8]>,
    t: Timeouts,
    follow_redirects: bool,
    max_body: Option<usize>,
) -> Option<Resp> {
    request_tls(url, headers, verb, body, t, follow_redirects, max_body, Tls::Ca)
}

/// **How the peer is verified.** Three modes, and they are an enum rather than an
/// `Option<&str>` for one reason: the pinned one turns CA verification OFF, so "pinned" and
/// "CA-verified" are opposites that a bare string parameter let a caller hold at the same time.
/// Making them variants means the compiler enforces what a comment used to ask for.
pub(crate) enum Tls<'a> {
    /// CA-verified against **the television's own trust store**. The default, and what every
    /// plex.tv and PMS call has always used — `request` is exactly this.
    Ca,
    /// CA-verified against **a PEM bundle we ship**, by absolute path. Same verification, different
    /// roots: it exists so a third-party endpoint's CA rotation is not at the mercy of a store
    /// baked into a 2019 firmware.
    CaBundle(&'a str),
    /// **Pinned**, and CA verification deliberately off — see [`CURLOPT_PINNEDPUBLICKEY`]. Only the
    /// lab receiver, which is a self-signed certificate on a developer's Mac.
    #[cfg_attr(not(feature = "lab-diagnostics"), allow(dead_code))]
    Pinned(&'a str),
}

/// The same three, owning their `CString`s so the pointers handed to curl outlive `perform`.
enum TlsCfg {
    Ca,
    CaBundle(CString),
    #[cfg_attr(not(feature = "lab-diagnostics"), allow(dead_code))]
    Pinned(CString),
}

/// [`request`], plus an explicit [`Tls`] mode. One extra parameter rather than a second
/// transport: everything about a pinned request — the header list, the verb shapes, the bounded
/// sink, the `CURLcode` naming, the fallback serialisation — is identical, and a copy of this
/// function would be a second place for all of it to drift.
///
/// One extra parameter rather than a second transport: everything else about a request — the header
/// list, the verb shapes, the bounded sink, the `CURLcode` naming, the fallback serialisation — is
/// identical, and a copy of this function would be a second place for all of it to drift.
#[allow(clippy::too_many_arguments)]
pub(crate) fn request_tls(
    url: &str,
    headers: &[String],
    verb: &str,
    body: Option<&[u8]>,
    t: Timeouts,
    follow_redirects: bool,
    max_body: Option<usize>,
    tls: Tls<'_>,
) -> Option<Resp> {
    // Every fallible CString is built BEFORE the easy handle exists. The RAII guards below still
    // make later early returns safe, but this ordering also means malformed caller input never
    // enters curl with a half-configured request.
    let verb_c = CString::new(verb).ok()?;
    let url_c = CString::new(url).ok()?;
    let ua = CString::new(crate::plex::identity::user_agent()).ok()?;
    let tls_c = match tls {
        Tls::Ca => TlsCfg::Ca,
        Tls::CaBundle(p) => TlsCfg::CaBundle(CString::new(p).ok()?),
        Tls::Pinned(p) => TlsCfg::Pinned(CString::new(p).ok()?),
    };
    let hdr_owned: Vec<CString> = headers
        .iter()
        .map(|line| CString::new(line.as_str()))
        .collect::<Result<_, _>>()
        .ok()?;
    // The guard `CURL_OK` exists for. Without it, a device with no libcurl this app can bind
    // reaches `curl_easy_init`'s wrapper and takes `dynlib::missing_symbol`, which panics — an
    // account lookup failing should return None and let the caller fall back, not kill a thread.
    if !available() {
        return None;
    }
    // A legacy OpenSSL whose callback API is unexpectedly hidden can still support HTTPS control,
    // but only one easy request at a time. The normal installed/existing-callback path never takes
    // this mutex, and curlio remains disabled in the degraded state.
    let _fallback_serial = if threaded_tls_ready() {
        None
    } else {
        Some(CURL_FALLBACK_SERIAL.lock().unwrap_or_else(|e| e.into_inner()))
    };
    unsafe {
        let h = curl_easy_init();
        if h.is_null() {
            return None;
        }
        let easy = Easy(h);
        curl_easy_setopt_ptr(easy.0, CURLOPT_URL, url_c.as_ptr() as *const c_void);
        curl_easy_setopt_ptr(easy.0, CURLOPT_WRITEFUNCTION, write_cb as *const c_void);
        let mut sink = BodySink::new(max_body);
        curl_easy_setopt_ptr(easy.0, CURLOPT_WRITEDATA, (&mut sink as *mut BodySink) as *mut c_void);
        // No curl call in this module may escape HTTP(S). The public QR fetch is the only one that
        // follows redirects; it is capped, and an HTTPS start may never downgrade to plaintext.
        curl_easy_setopt_long(easy.0, CURLOPT_PROTOCOLS, CURLPROTO_HTTP | CURLPROTO_HTTPS);
        curl_easy_setopt_long(easy.0, CURLOPT_FOLLOWLOCATION, follow_redirects as c_long);
        if follow_redirects {
            curl_easy_setopt_long(easy.0, CURLOPT_MAXREDIRS, PUBLIC_MAX_REDIRECTS);
            curl_easy_setopt_long(easy.0, CURLOPT_REDIR_PROTOCOLS, allowed_redirect_protocols(url.as_bytes()));
        }
        curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYPEER, 1 as c_long);
        curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYHOST, 2 as c_long);
        // A pinned request replaces CA verification with key pinning — see
        // [`CURLOPT_PINNEDPUBLICKEY`]. Written AFTER the two defaults above so the ordinary path is
        // still one unconditional pair of lines that cannot be reached with the wrong value.
        //
        // **THE RETURN CODE IS CHECKED HERE AND NOWHERE ELSE IN THIS FUNCTION, AND THAT ASYMMETRY
        // IS THE POINT.** Every other `setopt` above fails safe: a libcurl that refuses
        // `CURLOPT_TIMEOUT` gives a request without a deadline, which is worse but not unsafe. This
        // one fails OPEN. If the option is rejected — an older libcurl, or a TLS backend whose
        // pinning support post-dates it, which is neither a symbol nor a library and so is
        // invisible to `tools/fwcompat.py` — then the two lines under it would still run and the
        // request would go out with **no pinning and no CA verification at all**, accepting any
        // certificate anyone cared to present. So an unsupported option is a REFUSAL, before
        // verification is touched: a lab upload that does not happen costs a log, and one sent to
        // whoever answered costs the log's contents.
        match &tls_c {
            // The two lines above already ARE this mode. Named rather than left implicit, because
            // "CA-verified and unpinned" being the DEFAULT is the fact a plan written against this
            // module got wrong — it called for a new request mode to obtain what `request` had
            // been doing since it was written.
            TlsCfg::Ca => {}
            // A bundle we ship. The return code is checked for the same reason the pin's is, but
            // the failure it guards is milder and worth stating so nobody "simplifies" the pinned
            // check to match: a REJECTED `CURLOPT_CAINFO` leaves the device's own store in force,
            // which still verifies, whereas a rejected pin would leave nothing verifying at all.
            // Refusing here is a deliberate over-reaction — if we could not select the roots we
            // meant to, the honest report is that the send did not happen.
            TlsCfg::CaBundle(p) => {
                let rc = curl_easy_setopt_ptr(easy.0, CURLOPT_CAINFO, p.as_ptr() as *const c_void);
                if rc != 0 {
                    crate::log(&format!("net: this libcurl refuses CURLOPT_CAINFO (rc={rc}) — refusing to send against an unknown trust store"));
                    return None;
                }
            }
            TlsCfg::Pinned(p) => {
                let rc = curl_easy_setopt_ptr(easy.0, CURLOPT_PINNEDPUBLICKEY, p.as_ptr() as *const c_void);
                if rc != 0 {
                    crate::log(&format!("net: this libcurl refuses CURLOPT_PINNEDPUBLICKEY (rc={rc}) — refusing to send unpinned"));
                    return None;
                }
                curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYPEER, 0 as c_long);
                curl_easy_setopt_long(easy.0, CURLOPT_SSL_VERIFYHOST, 0 as c_long);
            }
        }
        curl_easy_setopt_long(easy.0, CURLOPT_NOSIGNAL, 1 as c_long);
        curl_easy_setopt_long(easy.0, CURLOPT_CONNECTTIMEOUT, t.connect_s);
        curl_easy_setopt_long(easy.0, CURLOPT_TIMEOUT, t.total_s);
        curl_easy_setopt_long(easy.0, CURLOPT_LOW_SPEED_LIMIT, t.low_speed_bps);
        curl_easy_setopt_long(easy.0, CURLOPT_LOW_SPEED_TIME, t.low_speed_s);
        curl_easy_setopt_ptr(easy.0, CURLOPT_USERAGENT, ua.as_ptr() as *const c_void);

        // request headers — keep the CStrings alive until after perform.
        let mut slist = HeaderList(ptr::null_mut());
        for c in &hdr_owned {
            let next = curl_slist_append(slist.0, c.as_ptr());
            if next.is_null() {
                return None;
            }
            slist.0 = next;
        }
        if !slist.0.is_null() {
            curl_easy_setopt_ptr(easy.0, CURLOPT_HTTPHEADER, slist.0 as *const c_void);
        }
        // The VERB. Three shapes, and the split is what keeps each one on the wire curl already
        // knows how to send:
        //   * `GET` with no body is curl's default — setting nothing is setting it right.
        //   * anything WITH a body rides `CURLOPT_POST`, so curl writes the `Content-Length` and
        //     the body itself; a non-`POST` verb on top of that only renames the request line.
        //   * a body-LESS non-GET (the `PUT` `select_streams` sends) is a GET-shaped request with
        //     the verb overridden — see [`CURLOPT_CUSTOMREQUEST`] for why not `CURLOPT_UPLOAD`.
        if let Some(body) = body {
            curl_easy_setopt_long(easy.0, CURLOPT_POST, 1 as c_long);
            curl_easy_setopt_long(easy.0, CURLOPT_POSTFIELDSIZE, body.len() as c_long);
            // curl references (doesn't copy) the buffer during perform; `body` outlives the call.
            curl_easy_setopt_ptr(easy.0, CURLOPT_POSTFIELDS, body.as_ptr() as *const c_void);
            if verb != "POST" {
                curl_easy_setopt_ptr(easy.0, CURLOPT_CUSTOMREQUEST, verb_c.as_ptr() as *const c_void);
            }
        } else if verb != "GET" {
            curl_easy_setopt_ptr(easy.0, CURLOPT_CUSTOMREQUEST, verb_c.as_ptr() as *const c_void);
        }

        let rc = curl_easy_perform(easy.0);
        let mut code: c_long = 0;
        curl_easy_getinfo_long(easy.0, CURLINFO_RESPONSE_CODE, &mut code as *mut c_long);

        if sink.overflowed {
            crate::log(&format!("net: response exceeded {} byte body limit", max_body.unwrap_or(0)));
            return None;
        }
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
                90 => "certificate pin did not match (stale lab session?)",
                _ => "transport error",
            };
            crate::log(&format!("net: curl rc={rc} — {why}"));
            return None;
        }
        Some(Resp { status: code as u16, body: sink.body })
    }
}

/// Blocking HTTPS GET on the [`API`] deadlines — the plex.tv account calls.
pub fn https_get(url: &str, headers: &[String]) -> Option<Resp> {
    request(url, headers, "GET", None, API, false, None)
}
/// Blocking HTTPS POST (`body` may be empty) on the [`API`] deadlines.
pub fn https_post(url: &str, headers: &[String], body: &[u8]) -> Option<Resp> {
    request(url, headers, "POST", Some(body), API, false, None)
}

/// Blocking **pinned** HTTPS POST — used by Lab Diagnostics uploads and Lab Control's long poll.
/// Their receiver is a self-signed certificate on a developer's Mac. Redirects are OFF (a pinned
/// endpoint that redirects is not the endpoint) and the response body is capped: the receiver
/// answers one small JSON object, and this is the one transport in the app whose far end is not a
/// Plex service.
#[cfg(feature = "lab-diagnostics")]
pub(crate) fn post_pinned(
    url: &str,
    headers: &[String],
    body: &[u8],
    pin: &str,
    t: Timeouts,
) -> Option<Resp> {
    request_tls(url, headers, "POST", Some(body), t, false, Some(4096), Tls::Pinned(pin))
}

/// Blocking **CA-verified, unpinned** HTTPS POST to a third-party endpoint — the telemetry sinks.
///
/// # Why this exists, given that [`https_post`] is already CA-verified and unpinned
///
/// The plan this was built to called for a new request mode on the grounds that [`post_pinned`]
/// sets `SSL_VERIFYPEER=0`, so "Sentry and PostHog need the opposite". They do — and `request` has
/// been that opposite since it was written: `VERIFYPEER=1`/`VERIFYHOST=2` unconditionally, dropped
/// only inside the pinning branch. The premise was wrong, and the mode it asked for already
/// existed. Recording that rather than quietly building it, because "add a mode that is already
/// the default" is the kind of finding that otherwise gets rediscovered.
///
/// What a telemetry sender genuinely needs beyond [`https_post`] is three other things:
///
/// * **a bounded response sink.** `https_post` passes `max_body: None`. plex.tv is a service this
///   app is built around; a telemetry endpoint is not, and an unbounded sink on a 1.68 GB
///   television is a memory risk for a reply we only read a status code from;
/// * **its own deadlines.** [`API`] is tuned for a call a person is waiting on. A background flush
///   holding a worker for 25 s to report a crash that already happened has the priority backwards;
/// * **our own roots, when we ship them.** The device's trust store was frozen in 2019 and cannot
///   be updated; a third party's CA rotation should not be able to end reporting on every
///   television at once. When `roots.pem` sits beside the binary this uses it, and says which it
///   used — otherwise "which trust store verified that" is unanswerable after the fact.
///
/// **Never pinned, deliberately.** Pinning a third party's SPKI means going dark at their next key
/// rotation, on televisions nobody can update. That is the opposite trade from the lab receiver's.
#[allow(dead_code)] // no sender yet — see `telemetry::sentry`
pub(crate) fn post_ca(url: &str, headers: &[String], body: &[u8], t: Timeouts) -> Option<Resp> {
    let bundle = shipped_ca_bundle(crate::paths::app_dir());
    // Once per process, not per send. Which trust store verified a telemetry endpoint is a fact
    // that is unanswerable after the event and free to state before it — but it does not change
    // between sends, and a line per upload would drown the log it is written into.
    static SAID: std::sync::Once = std::sync::Once::new();
    SAID.call_once(|| match &bundle {
        Some(p) => crate::log(&format!("net: telemetry TLS verifies against the shipped bundle ({p})")),
        None => crate::log("net: telemetry TLS verifies against the DEVICE trust store (no roots.pem beside the binary)"),
    });
    let tls = match bundle.as_deref() {
        Some(p) => Tls::CaBundle(p),
        None => Tls::Ca,
    };
    request_tls(url, headers, "POST", Some(body), t, false, Some(TELEMETRY_MAX_REPLY), tls)
}

/// The shipped PEM bundle beside the binary, if there is one.
///
/// Split out from [`post_ca`] because the interesting behaviour is the FALLBACK, and the fallback
/// is silent by nature: no bundle means the device's own 2019 trust store verifies instead, which
/// works right up until it does not. A host test can watch this choose, and cannot watch a socket.
///
/// `to_str` rather than a lossy conversion — a path that is not UTF-8 cannot become a `CString`
/// curl would open, and answering `None` sends us down the working path rather than into a
/// guaranteed error 77.
fn shipped_ca_bundle(dir: &std::path::Path) -> Option<String> {
    let p = dir.join("roots.pem");
    p.is_file().then(|| p.to_str().map(str::to_owned)).flatten()
}

/// How much of a telemetry endpoint's reply is worth keeping. Both vendors answer a small JSON
/// object and the only field either sender acts on is the status code; the body is kept at all so a
/// rejection can be logged with the server's own explanation, which is the difference between
/// debugging a 400 and guessing at one.
const TELEMETRY_MAX_REPLY: usize = 4096;

/// Redirect-following HTTPS GET for a PUBLIC, credential-free resource. The QR image fetch is the
/// only caller: at most five HTTP(S)-only redirects are followed, and an HTTPS request may never
/// downgrade. Account/PMS requests keep redirects off because replayed headers/URLs carry tokens.
pub fn https_get_public(url: &str) -> Option<Resp> {
    request(url, &[], "GET", None, API, true, None)
}

#[cfg(test)]
mod request_tests {
    use super::*;

    #[test]
    fn a_bounded_sink_refuses_before_it_allocates_past_the_limit() {
        let mut sink = BodySink::new(Some(4));
        assert!(sink.push(b"abc"));
        assert!(!sink.push(b"de"));
        assert_eq!(sink.body, b"abc", "the overflowing callback chunk is never appended");
        assert!(sink.overflowed);
    }

    #[test]
    fn bulk_reads_keep_the_connect_deadline_and_drop_the_transfer_deadline() {
        assert_eq!(
            API,
            Timeouts { connect_s: 8, total_s: 25, low_speed_bps: 0, low_speed_s: 0 }
        );
        assert_eq!(
            BULK,
            Timeouts { connect_s: 8, total_s: 0, low_speed_bps: 1, low_speed_s: 30 }
        );
    }

    #[test]
    fn public_redirects_are_http_only_and_never_downgrade_tls() {
        assert_eq!(allowed_redirect_protocols(b"https://example.invalid/qr"), CURLPROTO_HTTPS);
        assert_eq!(allowed_redirect_protocols(b"HTTPS://example.invalid/qr"), CURLPROTO_HTTPS);
        assert_eq!(
            allowed_redirect_protocols(b"http://example.invalid/qr"),
            CURLPROTO_HTTP | CURLPROTO_HTTPS,
            "plaintext may upgrade, but the inverse is forbidden"
        );
        assert_eq!(PUBLIC_MAX_REDIRECTS, 5);
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::*;

    #[test]
    fn only_pre_1_1_openssl_requires_application_locks() {
        assert!(needs_legacy_crypto_locks("libcurl/7.53.1 OpenSSL/1.0.2p zlib/1.2.11"));
        assert!(needs_legacy_crypto_locks("libcurl/7.20 OpenSSL/0.9.8"));
        assert!(!needs_legacy_crypto_locks("libcurl/8.7 OpenSSL/1.1.1w"));
        assert!(!needs_legacy_crypto_locks("libcurl/8.7 OpenSSL/3.2.1"));
        assert!(!needs_legacy_crypto_locks("libcurl/8.7 SecureTransport"));
    }

    #[test]
    fn old_openssl_is_concurrent_only_with_an_existing_or_installed_callback() {
        let old = "libcurl/7.53.1 OpenSSL/1.0.2p";
        assert!(!threaded_tls_policy(old, LegacyCrypto::Missing));
        assert!(threaded_tls_policy(old, LegacyCrypto::Existing));
        assert!(threaded_tls_policy(old, LegacyCrypto::Installed));
        assert!(!threaded_tls_policy(
            "libcurl/7.20 OpenSSL/0.9.8",
            LegacyCrypto::Installed
        ));
        assert!(threaded_tls_policy("libcurl/8.7 SecureTransport", LegacyCrypto::NotNeeded));
    }

    #[test]
    fn legacy_lock_helper_obeys_the_lock_bit() {
        let mut lock: libc::pthread_mutex_t = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::pthread_mutex_init(&mut lock, ptr::null()) }, 0);
        unsafe { apply_legacy_crypto_lock(&mut lock, 1) };
        assert_ne!(unsafe { libc::pthread_mutex_trylock(&mut lock) }, 0, "CRYPTO_LOCK must hold it");
        unsafe { apply_legacy_crypto_lock(&mut lock, 2) };
        assert_eq!(unsafe { libc::pthread_mutex_trylock(&mut lock) }, 0, "unlock mode must release it");
        unsafe {
            libc::pthread_mutex_unlock(&mut lock);
            libc::pthread_mutex_destroy(&mut lock);
        }
    }
}

#[cfg(test)]
mod tls_mode_tests {
    use super::*;

    /// **The fallback is the interesting half, and it is silent.** With no `roots.pem` beside the
    /// binary a telemetry POST verifies against the television's own 2019 trust store — which works
    /// until a third party rotates to a root that firmware never shipped, and then stops working on
    /// every set at once with nothing to read. Pinned here so the day the bundle starts shipping,
    /// the change in behaviour is a test diff rather than a discovery.
    #[test]
    fn a_missing_bundle_falls_back_to_the_device_trust_store() {
        let dir = std::env::temp_dir().join("plx-net-ca-absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert_eq!(shipped_ca_bundle(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// …and a bundle that IS there is selected, by absolute path. FFmpeg's `$ORIGIN` lesson applies
    /// to curl too: nothing beside this binary is on any search path, so the only workable form is
    /// the one `app_dir()` resolves at runtime.
    #[test]
    fn a_shipped_bundle_is_selected_by_absolute_path() {
        let dir = std::env::temp_dir().join("plx-net-ca-present");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("roots.pem"), b"-----BEGIN CERTIFICATE-----\n").expect("write");
        let got = shipped_ca_bundle(&dir).expect("the bundle beside the binary is found");
        assert!(got.ends_with("roots.pem"));
        assert!(std::path::Path::new(&got).is_absolute(), "curl is given an absolute path");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A DIRECTORY named `roots.pem` is not a bundle. `exists()` would accept it and hand curl a
    /// path it fails to read as error 77 — a send that dies at perform rather than falling back to
    /// the store that would have worked.
    #[test]
    fn a_directory_named_like_the_bundle_is_not_one() {
        let dir = std::env::temp_dir().join("plx-net-ca-dir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("roots.pem")).expect("temp dirs");
        assert_eq!(shipped_ca_bundle(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
